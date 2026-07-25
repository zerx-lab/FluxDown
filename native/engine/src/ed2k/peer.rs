//! eD2K peer 会话 —— 从单个 peer 下载单个块（leech-only，仅出站直连）。
//!
//! [`download_block_from_peer`] 完成一次单块下载：握手 → 设置请求文件 →
//! 查对端是否持块 → （多块时）拉取并**自验** hashset → 分片请求循环 →
//! 增量落盘 → 整块 MD4。成功返回 `(PeerAddr, md4)`；任何失败返回
//! [`Ed2kBlockError`]（携带 peer 身份）。
//!
//! **完整性/防御**（每条对应 §4 测试）：hashset 投毒（收到即对 root 自验）、
//! SENDINGPART 越界、未请求数据、区间碎片洪泛、stall 超时、QueueRank 独立
//! 排队态、CompressedPart zlib 炸弹限长。违规一律 [`DownloadError::Ed2kIntegrity`]
//! → 调度层拉黑；纯网络失败 [`DownloadError::Ed2k`] → 退避。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::downloader::DownloadError;
use crate::ed2k::Ed2kBlockError;
use crate::ed2k::hash::{self, BLOCK_SIZE};
use crate::ed2k::proto::{
    self, Ed2kMessage, OP_ACCEPTUPLOADREQ, OP_FILEREQANSNOFIL, OP_FILEREQANSWER, OP_FILESTATUS,
    OP_HASHSETREQUEST, OP_HELLO, OP_QUEUERANK, OP_REQUESTFILENAME, OP_SETREQFILEID,
    OP_STARTUPLOADREQ, PartRange, RequestParts,
};
use crate::ed2k::server::PeerAddr;
use crate::logger::log_info;
use crate::speed_limiter::SpeedLimiter;

/// peer 单次 socket 读的 stall 超时。
const PEER_STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// 排队（QueueRank）总超时：超过则放弃该 peer（不拉黑，排队是常态）。
const QUEUE_TIMEOUT: Duration = Duration::from_secs(120);

/// 单块已收区间列表的碎片上限；超过视为碎片洪泛攻击 → 整块重启+断连。
const MAX_INTERVALS: usize = 16;

/// 单块最多同时在飞的 pending 请求区间数（协议单帧最多 3 组）。
const MAX_INFLIGHT: usize = 3;

/// peer 帧 payload 上限：一个分片数据帧 = 头部 + 至多 BLOCK_SIZE 数据。
const MAX_PEER_FRAME: u32 = (BLOCK_SIZE as u32) + 1024;

/// 从单个 peer 下载单个块。
///
/// - `hashset_cache`：多块文件共享，首个成功拉取并自验的任务填充；后续任务
///   直接复用，`OnceCell::get_or_try_init` 天然处理首次请求去重。
/// - `progress`：`block_index → 已落盘字节`，供编排层旁路任务周期上报。
///
/// # Errors
///
/// 任何失败返回 [`Ed2kBlockError`]：完整性违规 → `source` 为
/// [`DownloadError::Ed2kIntegrity`]；纯网络失败 → [`DownloadError::Ed2k`] /
/// [`DownloadError::Io`]。
#[allow(clippy::too_many_arguments)]
pub async fn download_block_from_peer(
    peer: PeerAddr,
    file_hash: &[u8; 16],
    block_index: u64,
    total_bytes: u64,
    part_size: u64,
    large_file: bool,
    dest: &Path,
    cancel: &CancellationToken,
    limiter: &SpeedLimiter,
    hashset_cache: Arc<OnceCell<Vec<[u8; 16]>>>,
    progress: Arc<StdMutex<HashMap<u64, i64>>>,
) -> Result<(PeerAddr, [u8; 16]), Ed2kBlockError> {
    let err = |e: DownloadError| Ed2kBlockError { peer, source: e };

    let result = download_block_inner(
        peer,
        file_hash,
        block_index,
        total_bytes,
        part_size,
        large_file,
        dest,
        cancel,
        limiter,
        &hashset_cache,
        &progress,
    )
    .await;
    result.map(|md4| (peer, md4)).map_err(err)
}

/// 单块下载的实际实现，错误不带 peer（由外层 [`download_block_from_peer`] 包裹）。
///
/// 拆出内层以让外层用 `map_err` 统一回填 peer，避免每个 `?` 手写 peer。
#[allow(clippy::too_many_arguments)]
async fn download_block_inner(
    peer: PeerAddr,
    file_hash: &[u8; 16],
    block_index: u64,
    total_bytes: u64,
    part_size: u64,
    large_file: bool,
    dest: &Path,
    cancel: &CancellationToken,
    limiter: &SpeedLimiter,
    hashset_cache: &Arc<OnceCell<Vec<[u8; 16]>>>,
    progress: &Arc<StdMutex<HashMap<u64, i64>>>,
) -> Result<[u8; 16], DownloadError> {
    let stream = TcpStream::connect((peer.ip, peer.port))
        .await
        .map_err(DownloadError::Io)?;
    download_block_on_stream(
        stream,
        file_hash,
        block_index,
        total_bytes,
        part_size,
        large_file,
        dest,
        cancel,
        limiter,
        hashset_cache,
        progress,
    )
    .await
}

/// 在一条**已建立**的 TCP 连接上完成单块下载（出站直连或入站/callback 共用）。
///
/// 与 [`download_block_inner`] 的唯一区别是连接由调用方提供 —— 入站 peer
/// 与 LowID callback 连接在 [`crate::ed2k::client`] 里被接受后交由本函数拉块，
/// 复用握手→hashset→分片循环→整块 MD4 的全部逻辑。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn download_block_on_stream(
    mut stream: TcpStream,
    file_hash: &[u8; 16],
    block_index: u64,
    total_bytes: u64,
    part_size: u64,
    large_file: bool,
    dest: &Path,
    cancel: &CancellationToken,
    limiter: &SpeedLimiter,
    hashset_cache: &Arc<OnceCell<Vec<[u8; 16]>>>,
    progress: &Arc<StdMutex<HashMap<u64, i64>>>,
) -> Result<[u8; 16], DownloadError> {
    let (block_start, block_end) = hash::part_span(block_index, total_bytes, part_size);
    let block_len = block_end - block_start;
    let is_single = hash::part_count(total_bytes, part_size) == 1;

    // --- 握手 ---
    handshake(&mut stream, file_hash, cancel).await?;

    // --- 协商上传槽位：FileRequest→FileStatus→StartUpload→Accept ---
    // 关键修复：跳过此序列 peer 永不发数据（实网验证 3/5 源在补全后正常供数据）。
    negotiate_upload_slot(&mut stream, file_hash, cancel).await?;

    // --- 多块：确保 hashset 已获取并自验（槽位已开，peer 照答 HashSetRequest）---
    if !is_single {
        ensure_hashset(
            &mut stream,
            file_hash,
            total_bytes,
            part_size,
            hashset_cache,
            cancel,
        )
        .await?;
    }

    // --- 打开目标文件，seek 到块起点 ---
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(dest)
        .await
        .map_err(DownloadError::Io)?;

    // --- 分片请求循环 ---
    // received：已收且落盘的区间（相对块起点，半开），合并去重。
    let mut received: Vec<(u64, u64)> = Vec::new();
    // inflight：已请求未收全的区间（绝对偏移，半开）。
    let mut inflight: Vec<PartRange> = Vec::new();
    let mut next_req_start = block_start;
    let mut hasher_buf: Vec<u8> = vec![0u8; block_len as usize];

    loop {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        // 补满 inflight（≤3 组），每组 ≤ BLOCK_SIZE，止于 block_end。
        while inflight.len() < MAX_INFLIGHT && next_req_start < block_end {
            let req_end = (next_req_start + BLOCK_SIZE).min(block_end);
            inflight.push(PartRange {
                start: next_req_start,
                end_exclusive: req_end,
            });
            next_req_start = req_end;
        }
        if inflight.is_empty() && received_covers(&received, block_len) {
            break; // 块已收全。
        }
        if !inflight.is_empty() {
            send_request_parts(&mut stream, file_hash, &inflight, large_file).await?;
        }

        // 读一帧（stall 超时）。
        let (proto_byte, opcode, payload) = tokio::time::timeout(
            PEER_STALL_TIMEOUT,
            proto::read_frame(&mut stream, MAX_PEER_FRAME),
        )
        .await
        .map_err(|_| DownloadError::Ed2k("peer stalled".into()))??;

        match proto::dispatch(proto_byte, opcode, &payload, large_file)? {
            Ed2kMessage::SendingPart {
                start,
                end_exclusive,
                data,
                ..
            } => {
                accept_part(
                    &mut file,
                    block_start,
                    block_end,
                    total_bytes,
                    start,
                    end_exclusive,
                    &data,
                    &mut inflight,
                    &mut received,
                    &mut hasher_buf,
                    limiter,
                )
                .await?;
                if let Ok(mut p) = progress.lock() {
                    p.insert(block_index, received_total(&received) as i64);
                }
            }
            Ed2kMessage::CompressedPart { start, packed, .. } => {
                // 字段级 zlib 解压，上限 = 单块请求粒度 + slack（防炸弹）。
                let data = proto::decompress_bounded(&packed, BLOCK_SIZE as usize + 1024)?;
                let end_exclusive = start + data.len() as u64;
                accept_part(
                    &mut file,
                    block_start,
                    block_end,
                    total_bytes,
                    start,
                    end_exclusive,
                    &data,
                    &mut inflight,
                    &mut received,
                    &mut hasher_buf,
                    limiter,
                )
                .await?;
                if let Ok(mut p) = progress.lock() {
                    p.insert(block_index, received_total(&received) as i64);
                }
            }
            Ed2kMessage::QueueRank(rank) => {
                // 排队是常态，独立于 stall。等待 ACCEPT（这里以 QUEUE_TIMEOUT 兜底）。
                let remote = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_default();
                log_info!("[ed2k-peer] {} queued, rank={}", remote, rank);
                wait_for_accept(&mut stream).await?;
            }
            Ed2kMessage::OutOfPartReqs => {
                return Err(DownloadError::Ed2k("peer out of part requests".into()));
            }
            Ed2kMessage::NoFile { .. } => {
                return Err(DownloadError::Ed2k("peer does not have this file".into()));
            }
            Ed2kMessage::Unknown(_) | Ed2kMessage::ServerStatus => {}
            _ => {}
        }

        if received_covers(&received, block_len) {
            break;
        }
    }

    // --- 落盘 flush + 整块 MD4 ---
    file.flush().await.map_err(DownloadError::Io)?;
    let buf = std::mem::take(&mut hasher_buf);
    let md4 = tokio::task::spawn_blocking(move || hash::hash_part(&buf))
        .await
        .map_err(|e| DownloadError::Ed2k(format!("md4 join error: {e}")))?;
    Ok(md4)
}

/// 握手：发合规 HELLO（含能力 tag + 尾部 server endpoint），收 HELLOANSWER。
///
/// eMule `OP_HELLO` 线格式：`hashlen(1=0x10) + user_hash(16) + client_id(4) +
/// port(2) + tagCount(4) + tags + serverIP(4) + serverPort(2)`。缺尾部 6 字节
/// server endpoint 或缺能力 tag，现代 peer 会视为畸形/异常客户端直接断开
/// （实网验证：补全后 peer 正常回 HelloAnswer 并进入文件协商）。
async fn handshake(
    stream: &mut TcpStream,
    _file_hash: &[u8; 16],
    cancel: &CancellationToken,
) -> Result<(), DownloadError> {
    let mut user_hash = [0u8; 16];
    user_hash[5] = 14;
    user_hash[14] = 111;
    // 能力 tag：名称 + eDonkey 版本 + eMule 版本（aMule 软件 id 3）。
    let name_tag = encode_peer_string_tag(0x01, "FluxDown");
    let ver_tag = encode_peer_u32_tag(0x11, 0x3C);
    let mule_ver: u32 = (3 << 24) | (1 << 7);
    let mule_tag = encode_peer_u32_tag(0xFB, mule_ver);

    let mut payload = Vec::new();
    payload.push(0x10);
    payload.extend_from_slice(&user_hash);
    payload.extend_from_slice(&0u32.to_le_bytes()); // client_id
    payload.extend_from_slice(&0u16.to_le_bytes()); // listen port（leech=0）
    payload.extend_from_slice(&3u32.to_le_bytes()); // tagCount=3
    payload.extend_from_slice(&name_tag);
    payload.extend_from_slice(&ver_tag);
    payload.extend_from_slice(&mule_tag);
    payload.extend_from_slice(&0u32.to_le_bytes()); // server IP
    payload.extend_from_slice(&0u16.to_le_bytes()); // server port
    let frame = proto::frame(OP_HELLO, &payload);
    stream.write_all(&frame).await.map_err(DownloadError::Io)?;

    // 读一帧确认（HELLOANSWER 或其它）；stall 超时。
    if cancel.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    let (proto_byte, opcode, payload) = tokio::time::timeout(
        PEER_STALL_TIMEOUT,
        proto::read_frame(stream, MAX_PEER_FRAME),
    )
    .await
    .map_err(|_| DownloadError::Ed2k("peer handshake stalled".into()))??;
    let _ = proto::dispatch(proto_byte, opcode, &payload, false)?;
    Ok(())
}

/// 编码 peer 用「特殊名」字符串 tag：`type(0x02) + nameLen(2=1) + name(1) + valLen(2) + val`。
fn encode_peer_string_tag(name: u8, value: &str) -> Vec<u8> {
    let mut out = vec![0x02u8];
    out.extend_from_slice(&1u16.to_le_bytes());
    out.push(name);
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    out
}

/// 编码 peer 用「特殊名」u32 tag：`type(0x03) + nameLen(2=1) + name(1) + val(4)`。
fn encode_peer_u32_tag(name: u8, value: u32) -> Vec<u8> {
    let mut out = vec![0x03u8];
    out.extend_from_slice(&1u16.to_le_bytes());
    out.push(name);
    out.extend_from_slice(&value.to_le_bytes());
    out
}

/// 协商上传槽位：`FileRequest → FileAnswer → FileStatusRequest → FileStatus
/// → StartUpload → (AcceptUpload | QueueRank→等待)`。成功返回即可请求分片。
///
/// 实网验证：跳过此序列（旧实现直接 REQUESTPARTS）→ peer 永不发数据。
async fn negotiate_upload_slot(
    stream: &mut TcpStream,
    file_hash: &[u8; 16],
    cancel: &CancellationToken,
) -> Result<(), DownloadError> {
    // FileRequest（0x58）→ 期待 FileAnswer(0x59) / NoFile(0x48)。
    let req = proto::frame(OP_REQUESTFILENAME, file_hash);
    stream.write_all(&req).await.map_err(DownloadError::Io)?;

    let mut sent_status = false;
    let mut sent_start = false;
    for _ in 0..32 {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let (proto_byte, opcode, _payload) = tokio::time::timeout(
            PEER_STALL_TIMEOUT,
            proto::read_frame(stream, MAX_PEER_FRAME),
        )
        .await
        .map_err(|_| DownloadError::Ed2k("upload negotiation stalled".into()))??;
        let _ = proto_byte;
        match opcode {
            OP_FILEREQANSWER if !sent_status => {
                // 对端持有文件 → 发 FileStatusRequest（SETREQFILEID 0x4F）。
                let fsr = proto::frame(OP_SETREQFILEID, file_hash);
                stream.write_all(&fsr).await.map_err(DownloadError::Io)?;
                sent_status = true;
            }
            OP_FILEREQANSWER => {}
            OP_FILEREQANSNOFIL => {
                return Err(DownloadError::Ed2k("peer does not have this file".into()));
            }
            OP_FILESTATUS if !sent_start => {
                // 拿到分片位图 → 发 StartUpload（0x54），入对端上传队列。
                let su = proto::frame(OP_STARTUPLOADREQ, file_hash);
                stream.write_all(&su).await.map_err(DownloadError::Io)?;
                sent_start = true;
            }
            OP_FILESTATUS => {}
            OP_ACCEPTUPLOADREQ => {
                // 获得上传槽 → 可以请求分片。
                return Ok(());
            }
            OP_QUEUERANK | proto::OP_QUEUERANKING => {
                // 排队中：等待 ACCEPT（QUEUE_TIMEOUT 兜底）。
                wait_for_accept(stream).await?;
                return Ok(());
            }
            proto::OP_OUTOFPARTREQS => {
                return Err(DownloadError::Ed2k("peer out of upload slots".into()));
            }
            _ => {
                // 其它帧（ExtHello 等）忽略，继续等协商帧。
            }
        }
    }
    Err(DownloadError::Ed2k(
        "no upload slot after negotiation".into(),
    ))
}

/// 确保 `hashset_cache` 已填充：首个任务拉取 hashset 并对 root 自验（防投毒）。
async fn ensure_hashset(
    stream: &mut TcpStream,
    file_hash: &[u8; 16],
    total_bytes: u64,
    part_size: u64,
    hashset_cache: &Arc<OnceCell<Vec<[u8; 16]>>>,
    cancel: &CancellationToken,
) -> Result<(), DownloadError> {
    if hashset_cache.get().is_some() {
        return Ok(());
    }
    // SETREQFILEID 已在 negotiate_upload_slot 阶段发过；此处只发 HASHSETREQUEST。
    let hsreq = proto::frame(OP_HASHSETREQUEST, file_hash);
    stream.write_all(&hsreq).await.map_err(DownloadError::Io)?;

    // 收帧直到 HashSetAnswer。
    for _ in 0..16 {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let (proto_byte, opcode, payload) = tokio::time::timeout(
            PEER_STALL_TIMEOUT,
            proto::read_frame(stream, MAX_PEER_FRAME),
        )
        .await
        .map_err(|_| DownloadError::Ed2k("hashset request stalled".into()))??;
        if let Ed2kMessage::HashSetAnswer { part_hashes } =
            proto::dispatch(proto_byte, opcode, &payload, false)?
        {
            // 投毒防御：用它校验任何块前先对 link root 自验。
            if !hash::verify_hashset_root(&part_hashes, file_hash, total_bytes, part_size) {
                return Err(DownloadError::Ed2kIntegrity(
                    "hashset root mismatch (poison?)".into(),
                ));
            }
            // 幂等填充（多任务并发时首个赢，其余复用）。
            let _ = hashset_cache.set(part_hashes);
            return Ok(());
        }
    }
    Err(DownloadError::Ed2k("no hashset answer".into()))
}

/// 发送一批 pending 区间为 REQUESTPARTS（≤3 组）。
async fn send_request_parts(
    stream: &mut TcpStream,
    file_hash: &[u8; 16],
    inflight: &[PartRange],
    large_file: bool,
) -> Result<(), DownloadError> {
    let mut ranges: [Option<PartRange>; 3] = [None, None, None];
    for (i, r) in inflight.iter().take(3).enumerate() {
        ranges[i] = Some(*r);
    }
    let rp = RequestParts {
        file_hash: *file_hash,
        ranges,
        large_file,
    };
    let (opcode, payload) = rp.encode();
    let frame = proto::frame(opcode, &payload);
    stream.write_all(&frame).await.map_err(DownloadError::Io)
}

/// 接收一个分片：越界/未请求校验 → 限速 → seek+write 落盘 → 更新区间。
#[allow(clippy::too_many_arguments)]
async fn accept_part(
    file: &mut tokio::fs::File,
    block_start: u64,
    block_end: u64,
    total_bytes: u64,
    start: u64,
    end_exclusive: u64,
    data: &[u8],
    inflight: &mut Vec<PartRange>,
    received: &mut Vec<(u64, u64)>,
    hasher_buf: &mut [u8],
    limiter: &SpeedLimiter,
) -> Result<(), DownloadError> {
    // 越界校验（完整性违规）。
    if start >= end_exclusive
        || start < block_start
        || end_exclusive > block_end
        || end_exclusive > total_bytes
    {
        return Err(DownloadError::Ed2kIntegrity(format!(
            "sendingpart out of bounds: [{start},{end_exclusive}) not in [{block_start},{block_end})"
        )));
    }
    // 实际数据长度须与声明区间一致。
    if data.len() as u64 != end_exclusive - start {
        return Err(DownloadError::Ed2kIntegrity(format!(
            "sendingpart length mismatch: declared {} got {}",
            end_exclusive - start,
            data.len()
        )));
    }
    // 未请求数据校验：必须落在某个 inflight 区间内。
    let in_flight = inflight
        .iter()
        .any(|r| start >= r.start && end_exclusive <= r.end_exclusive);
    if !in_flight {
        return Err(DownloadError::Ed2kIntegrity(
            "unrequested data received".into(),
        ));
    }

    // 限速。
    let mut remaining = data.len() as u64;
    while remaining > 0 {
        let granted = limiter.consume(remaining).await;
        remaining = remaining.saturating_sub(granted.max(1));
        if granted == 0 {
            break;
        }
    }

    // seek + write 落盘（绝对偏移）。
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(DownloadError::Io)?;
    file.write_all(data).await.map_err(DownloadError::Io)?;

    // 写入块内缓冲（用于整块 MD4）。相对块起点。
    let rel = (start - block_start) as usize;
    hasher_buf
        .get_mut(rel..rel + data.len())
        .ok_or_else(|| DownloadError::Ed2kIntegrity("buffer overflow".into()))?
        .copy_from_slice(data);

    // 更新已收区间（相对块起点，半开），合并。
    let rel_start = start - block_start;
    let rel_end = end_exclusive - block_start;
    received.push((rel_start, rel_end));
    merge_intervals(received);
    if received.len() > MAX_INTERVALS {
        return Err(DownloadError::Ed2kIntegrity(
            "too many fragmented intervals (flood?)".into(),
        ));
    }

    // 从 inflight 移除已完全覆盖的请求区间。
    inflight.retain(|r| {
        !interval_covered(
            received,
            r.start - block_start,
            r.end_exclusive - block_start,
        )
    });
    Ok(())
}

/// 等待 ACCEPTUPLOAD（排队后），QUEUE_TIMEOUT 兜底。
async fn wait_for_accept(stream: &mut TcpStream) -> Result<(), DownloadError> {
    let deadline = tokio::time::Instant::now() + QUEUE_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(DownloadError::Ed2k("queue timeout".into()));
        }
        let frame = tokio::time::timeout(
            remaining.min(PEER_STALL_TIMEOUT),
            proto::read_frame(stream, MAX_PEER_FRAME),
        )
        .await;
        match frame {
            Ok(Ok((proto_byte, opcode, payload))) => {
                match proto::dispatch(proto_byte, opcode, &payload, false)? {
                    Ed2kMessage::QueueRank(_) => {} // 仍在排队。
                    _ => return Ok(()),             // ACCEPT 或数据帧 → 退出等待。
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(DownloadError::Ed2k("queue timeout".into())),
        }
    }
}

// ---------------------------------------------------------------------------
// 区间工具（相对块起点，半开 [start,end)）
// ---------------------------------------------------------------------------

/// 合并相邻/重叠区间（就地，按 start 排序后线性合并）。
fn merge_intervals(intervals: &mut Vec<(u64, u64)>) {
    if intervals.len() <= 1 {
        return;
    }
    intervals.sort_by_key(|r| r.0);
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(intervals.len());
    for &(s, e) in intervals.iter() {
        if let Some(last) = merged.last_mut()
            && s <= last.1
        {
            last.1 = last.1.max(e);
        } else {
            merged.push((s, e));
        }
    }
    *intervals = merged;
}

/// 已收区间是否完整覆盖 `[0, block_len)`。
fn received_covers(received: &[(u64, u64)], block_len: u64) -> bool {
    received.len() == 1 && received[0].0 == 0 && received[0].1 >= block_len
}

/// 已收字节总数（合并后区间长度之和）。
fn received_total(received: &[(u64, u64)]) -> u64 {
    received.iter().map(|(s, e)| e - s).sum()
}

/// 相对区间 `[s,e)` 是否被 `received` 完全覆盖。
fn interval_covered(received: &[(u64, u64)], s: u64, e: u64) -> bool {
    received.iter().any(|&(rs, re)| rs <= s && re >= e)
}

#[cfg(test)]
mod tests {
    use super::{interval_covered, merge_intervals, received_covers, received_total};

    #[test]
    fn merge_adjacent_and_overlap() {
        let mut v = vec![(0, 100), (100, 200), (150, 250)];
        merge_intervals(&mut v);
        assert_eq!(v, vec![(0, 250)]);
    }

    #[test]
    fn merge_thousand_contiguous_to_one() {
        let mut v: Vec<(u64, u64)> = (0..1000).map(|i| (i * 100, i * 100 + 100)).collect();
        merge_intervals(&mut v);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], (0, 100_000));
    }

    #[test]
    fn merge_leaves_gaps() {
        let mut v = vec![(0, 100), (200, 300)];
        merge_intervals(&mut v);
        assert_eq!(v, vec![(0, 100), (200, 300)]);
    }

    #[test]
    fn received_covers_full_block() {
        assert!(received_covers(&[(0, 100)], 100));
        assert!(!received_covers(&[(0, 50)], 100));
        assert!(!received_covers(&[(0, 50), (50, 100)], 100)); // 未合并即不算覆盖
    }

    #[test]
    fn received_total_sums() {
        assert_eq!(received_total(&[(0, 100), (200, 250)]), 150);
    }

    #[test]
    fn interval_covered_checks() {
        assert!(interval_covered(&[(0, 200)], 50, 150));
        assert!(!interval_covered(&[(0, 100)], 50, 150));
    }
}
