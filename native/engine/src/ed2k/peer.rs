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
use crate::ed2k::status::{PeerInfo, PeerLive};
use crate::logger::log_info;
use crate::speed_limiter::SpeedLimiter;
use crate::transfer_activity::TransferTracker;

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

/// 对端在 FILESTATUS 位图里声明不持有目标块时的错误文案；调度层据此只对
/// （源, 块）退避，而不是把整个源拉入退避。
pub(crate) const PEER_LACKS_BLOCK: &str = "peer does not hold requested block";

/// 位图（LSB 优先）里是否持有第 `block` 块。空位图 = 持有整文件（`part_count==0`）。
fn peer_has_block(bitfield: &[u8], block: u64) -> bool {
    if bitfield.is_empty() {
        return true;
    }
    bitfield
        .get((block / 8) as usize)
        .is_some_and(|byte| byte & (1 << (block % 8)) != 0)
}

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
    let tracker = TransferTracker::new();
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
        &tracker,
    )
    .await
}

/// 在一条**已建立**的 TCP 连接上完成单块下载（握手 + 申请槽位 + 拉一个块）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn download_block_on_stream(
    stream: TcpStream,
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
    tracker: &TransferTracker,
) -> Result<[u8; 16], DownloadError> {
    let mut session = PeerSession::open(stream, file_hash, cancel, Arc::default())
        .await
        .map_err(|error| error.source)?;
    session
        .download_block(
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
            tracker,
        )
        .await
}

/// [`PeerSession::open`] 的失败。
pub(crate) struct SessionOpenError {
    /// 是否已向对端发出 `OP_STARTUPLOADREQ`。发出即计入对端的重询节流，
    /// 调度层据此决定该源多久之后才能再次申请槽位。
    pub asked: bool,
    pub source: DownloadError,
}

/// 与单个源的一条已握手、已取得上传槽的连接，可在其上连续下载多个块
/// （出站直连与 LowID callback 入站连接共用）。
///
/// eMule 对每次 `OP_STARTUPLOADREQ` 计数：同一文件 10 分钟内重复申请记一次
/// 坏请求，累计 4 次封 IP 2 小时。因此一条连接只申请一次槽位并持续复用，
/// 不得「每块一连接」。
pub(crate) struct PeerSession {
    stream: TcpStream,
    /// 对端 FILESTATUS 位图（空 = 持有整文件）。
    bitfield: Vec<u8>,
    /// 该源的实况（阶段 / 排队名次 / 对端身份），供详情页读取。
    live: Arc<PeerLive>,
}

impl PeerSession {
    /// 握手并申请上传槽位：`Hello → FileRequest → FileStatus → StartUpload → Accept`。
    ///
    /// # Errors
    ///
    /// 握手 / 协商失败、超时、取消。
    pub(crate) async fn open(
        mut stream: TcpStream,
        file_hash: &[u8; 16],
        cancel: &CancellationToken,
        live: Arc<PeerLive>,
    ) -> Result<Self, SessionOpenError> {
        let info = handshake(&mut stream, file_hash, cancel)
            .await
            .map_err(|source| SessionOpenError {
                asked: false,
                source,
            })?;
        live.set_info(info);
        let mut asked = false;
        match negotiate_upload_slot(&mut stream, file_hash, cancel, &mut asked, &live).await {
            Ok(bitfield) => {
                live.set_transferring();
                Ok(Self {
                    stream,
                    bitfield,
                    live,
                })
            }
            Err(source) => Err(SessionOpenError { asked, source }),
        }
    }

    /// 对端持有的块数（`parts_total` 为文件块总数；空位图 = 持有整文件）。
    pub(crate) fn held_parts(&self, parts_total: u64) -> u32 {
        let held = if self.bitfield.is_empty() {
            parts_total
        } else {
            (0..parts_total)
                .filter(|block| peer_has_block(&self.bitfield, *block))
                .count() as u64
        };
        u32::try_from(held).unwrap_or(u32::MAX)
    }

    /// 在本连接上下载一个块。成功后连接仍可继续下载其它块；返回错误后
    /// （[`PEER_LACKS_BLOCK`] 除外）连接状态不再可信，应丢弃。
    ///
    /// # Errors
    ///
    /// 对端缺该块、完整性违规、网络失败、取消。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn download_block(
        &mut self,
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
        tracker: &TransferTracker,
    ) -> Result<[u8; 16], DownloadError> {
        if !peer_has_block(&self.bitfield, block_index) {
            return Err(DownloadError::Ed2k(PEER_LACKS_BLOCK.into()));
        }
        let stream = &mut self.stream;
        let (block_start, block_end) = hash::part_span(block_index, total_bytes, part_size);
        let block_len = block_end - block_start;

        // --- 多块：确保 hashset 已获取并自验（槽位已开，peer 照答 HashSetRequest）---
        if !hash::is_single_block(total_bytes, part_size) {
            ensure_hashset(
                stream,
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
        // 重新取得槽位后须重发未完成的请求。
        let mut resend = false;

        loop {
            if cancel.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }
            // 补满 inflight（≤3 组），每组 ≤ BLOCK_SIZE，止于 block_end。
            let requested = inflight.len();
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
            // 只在有新区间（或重新拿到槽位）时发请求；与 eMule 一致，请求帧同时带上
            // 仍在途的旧区间，对端按区间去重。
            if inflight.len() > requested || resend {
                send_request_parts(stream, file_hash, &inflight, large_file).await?;
                resend = false;
            }

            // 读一帧（stall 超时）。
            // Count only the peer response read; queue, negotiation and hashing are not transfers.
            let transfer = tracker.start(block_index as i32);
            let (proto_byte, opcode, payload) = tokio::time::timeout(
                PEER_STALL_TIMEOUT,
                proto::read_frame(stream, MAX_PEER_FRAME),
            )
            .await
            .map_err(|_| DownloadError::Ed2k("peer stalled".into()))??;
            drop(transfer);

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
                    self.live.set_queued(rank);
                    wait_for_accept(stream, &self.live).await?;
                    self.live.set_transferring();
                    resend = true;
                }
                Ed2kMessage::OutOfPartReqs => {
                    // 对端本次上传会话到上限：我方被放回其等待队列（不计入重询节流），
                    // 再次轮到时在同一连接上收到 ACCEPT。
                    self.live.set_queued(0);
                    wait_for_accept(stream, &self.live).await?;
                    self.live.set_transferring();
                    resend = true;
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
}

/// `CT_EMULE_MISCOPTIONS2` tag 名。
const CT_EMULE_MISCOPTIONS2: u8 = 0xFE;
/// MISCOPTIONS2 的 LargeFiles 位（bit4）。只声明 FluxDown 已实现的能力：
/// 置其它位（ExtMultiPacket、CryptLayer 等）会让对端改发未处理的包。
const MISCOPTIONS2_LARGE_FILES: u32 = 1 << 4;

/// 构造 `OP_HELLO` 载荷（含能力 tag + 尾部 server endpoint）。
///
/// eMule `OP_HELLO` 线格式：`hashlen(1=0x10) + user_hash(16) + client_id(4) +
/// port(2) + tagCount(4) + tags + serverIP(4) + serverPort(2)`。
/// 未声明 LargeFiles 位的客户端会被 eMule 系对端视为不支持 >4GB 文件，
/// 对 `OP_REQUESTFILENAME` 直接静默不回包。
fn build_hello_payload() -> Vec<u8> {
    let user_hash = crate::ed2k::identity::user_hash();
    // 能力 tag：名称 + eDonkey 版本 + eMule 版本（aMule 软件 id 3）+ MISCOPTIONS2。
    let name_tag = encode_peer_string_tag(0x01, "FluxDown");
    let ver_tag = encode_peer_u32_tag(0x11, 0x3C);
    let mule_ver: u32 = (3 << 24) | (1 << 7);
    let mule_tag = encode_peer_u32_tag(0xFB, mule_ver);
    let misc2_tag = encode_peer_u32_tag(CT_EMULE_MISCOPTIONS2, MISCOPTIONS2_LARGE_FILES);

    let mut payload = Vec::new();
    payload.push(0x10);
    payload.extend_from_slice(&user_hash);
    payload.extend_from_slice(&0u32.to_le_bytes()); // client_id
    payload.extend_from_slice(&0u16.to_le_bytes()); // listen port（leech=0）
    payload.extend_from_slice(&4u32.to_le_bytes()); // tagCount=4
    payload.extend_from_slice(&name_tag);
    payload.extend_from_slice(&ver_tag);
    payload.extend_from_slice(&mule_tag);
    payload.extend_from_slice(&misc2_tag);
    payload.extend_from_slice(&0u32.to_le_bytes()); // server IP
    payload.extend_from_slice(&0u16.to_le_bytes()); // server port
    payload
}

/// `OP_HELLOANSWER` 里的昵称 tag。
const CT_NAME: u8 = 0x01;
/// `OP_HELLOANSWER` 里的 eMule 兼容客户端版本 tag：
/// `compatible_client(8) | major(7) | minor(7) | update(3) | build(7)`。
const CT_EMULE_VERSION: u8 = 0xFB;
/// 握手应答里最多解析的 tag 数（真实客户端不超过十来个）。
const MAX_HELLO_TAGS: u32 = 64;
/// 昵称展示上限（字符）。
const MAX_NICKNAME_CHARS: usize = 64;

/// 握手：发合规 HELLO，收 HELLOANSWER 并取出对端身份。缺尾部 server endpoint 或缺
/// 能力 tag，现代 peer 会视为畸形/异常客户端直接断开（实网验证）。
async fn handshake(
    stream: &mut TcpStream,
    _file_hash: &[u8; 16],
    cancel: &CancellationToken,
) -> Result<PeerInfo, DownloadError> {
    let frame = proto::frame(OP_HELLO, &build_hello_payload());
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
    match proto::dispatch(proto_byte, opcode, &payload, false)? {
        Ed2kMessage::HelloAnswer { .. } => Ok(parse_hello_answer(&payload)),
        _ => Ok(PeerInfo::default()),
    }
}

/// 从 `OP_HELLOANSWER` 载荷（`user_hash(16) + client_id(4) + port(2) + tagCount(4) + tags…`）
/// 取出昵称与客户端软件。只用于展示：任何一处越界 / 未知 tag 类型即停，返回已取到的部分。
fn parse_hello_answer(payload: &[u8]) -> PeerInfo {
    let mut nickname = String::new();
    let mut emule_version = None;
    let count = payload
        .get(22..26)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map_or(0, u32::from_le_bytes);
    let mut offset = 26;
    for _ in 0..count.min(MAX_HELLO_TAGS) {
        let Some((name, value, next)) = read_hello_tag(payload, offset) else {
            break;
        };
        offset = next;
        match (name, value) {
            (CT_NAME, HelloTagValue::Str(bytes)) => {
                nickname = String::from_utf8_lossy(bytes)
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(MAX_NICKNAME_CHARS)
                    .collect::<String>()
                    .trim()
                    .to_owned();
            }
            (CT_EMULE_VERSION, HelloTagValue::U32(version)) => emule_version = Some(version),
            _ => {}
        }
    }
    PeerInfo {
        client: client_software(emule_version),
        nickname,
    }
}

enum HelloTagValue<'a> {
    Str(&'a [u8]),
    U32(u32),
    Other,
}

/// 读一个 eD2K tag，返回 `(单字节名, 值, 下一个 tag 的偏移)`；多字节名记 0。
/// 支持标准名（`type + nameLen(u16) + name`）与紧凑名（`type|0x80 + id`）。
fn read_hello_tag(bytes: &[u8], offset: usize) -> Option<(u8, HelloTagValue<'_>, usize)> {
    let u16_at = |at: usize| -> Option<usize> {
        let raw = <[u8; 2]>::try_from(bytes.get(at..at.checked_add(2)?)?).ok()?;
        Some(usize::from(u16::from_le_bytes(raw)))
    };
    let type_byte = *bytes.get(offset)?;
    let mut at = offset + 1;
    let name = if type_byte & 0x80 != 0 {
        let id = *bytes.get(at)?;
        at += 1;
        id
    } else {
        let len = u16_at(at)?;
        at += 2;
        let name = bytes.get(at..at.checked_add(len)?)?;
        at += len;
        match name {
            [id] => *id,
            _ => 0,
        }
    };
    let (value, len) = match type_byte & 0x7f {
        // hash16
        0x01 => (HelloTagValue::Other, 16),
        0x02 => {
            let len = u16_at(at)?;
            at += 2;
            (
                HelloTagValue::Str(bytes.get(at..at.checked_add(len)?)?),
                len,
            )
        }
        0x03 => {
            let raw = <[u8; 4]>::try_from(bytes.get(at..at.checked_add(4)?)?).ok()?;
            (HelloTagValue::U32(u32::from_le_bytes(raw)), 4)
        }
        // float32
        0x04 => (HelloTagValue::Other, 4),
        // bool / u8
        0x05 | 0x09 => (HelloTagValue::Other, 1),
        // blob：len(u32) + 数据
        0x07 => {
            let raw = <[u8; 4]>::try_from(bytes.get(at..at.checked_add(4)?)?).ok()?;
            at += 4;
            (
                HelloTagValue::Other,
                usize::try_from(u32::from_le_bytes(raw)).ok()?,
            )
        }
        0x08 => (HelloTagValue::Other, 2),
        0x0B => (HelloTagValue::Other, 8),
        // str1..str16：长度编码在类型里
        ty @ 0x11..=0x20 => {
            let len = usize::from(ty - 0x10);
            (
                HelloTagValue::Str(bytes.get(at..at.checked_add(len)?)?),
                len,
            )
        }
        _ => return None,
    };
    let next = at.checked_add(len)?;
    (next <= bytes.len()).then_some((name, value, next))
}

/// 由 `CT_EMULE_VERSION` 推出客户端软件名与版本；没有该 tag 的是原版 eDonkey 系客户端。
fn client_software(emule_version: Option<u32>) -> String {
    let Some(version) = emule_version else {
        return "eDonkey".to_owned();
    };
    let compatible = version >> 24;
    let major = (version >> 17) & 0x7f;
    let minor = (version >> 10) & 0x7f;
    let update = (version >> 7) & 0x07;
    let name = match compatible {
        // eMule 用字母表示修订号：0.50a。
        0 => {
            let letter = char::from(b'a' + u8::try_from(update).unwrap_or(0));
            return format!("eMule {major}.{minor}{letter}");
        }
        1 => "cDonkey",
        2 => "xMule",
        3 => "aMule",
        4 | 40 | 68 => "Shareaza",
        10 | 152 => "MLDonkey",
        20 => "lphant",
        other => return format!("eD2K client #{other} {major}.{minor}.{update}"),
    };
    format!("{name} {major}.{minor}.{update}")
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
/// → StartUpload → (AcceptUpload | QueueRank→等待)`。成功返回对端的块位图，
/// 此后即可请求分片。发出 `StartUpload` 时置 `asked`。
///
/// 实网验证：跳过此序列（旧实现直接 REQUESTPARTS）→ peer 永不发数据。
async fn negotiate_upload_slot(
    stream: &mut TcpStream,
    file_hash: &[u8; 16],
    cancel: &CancellationToken,
    asked: &mut bool,
    live: &PeerLive,
) -> Result<Vec<u8>, DownloadError> {
    // FileRequest（0x58）→ 期待 FileAnswer(0x59) / NoFile(0x48)。
    let req = proto::frame(OP_REQUESTFILENAME, file_hash);
    stream.write_all(&req).await.map_err(DownloadError::Io)?;

    let mut sent_status = false;
    let mut sent_start = false;
    let mut bitfield = Vec::new();
    for _ in 0..32 {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let (proto_byte, opcode, payload) = tokio::time::timeout(
            PEER_STALL_TIMEOUT,
            proto::read_frame(stream, MAX_PEER_FRAME),
        )
        .await
        .map_err(|_| DownloadError::Ed2k("upload negotiation stalled".into()))??;
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
                if let Ed2kMessage::FileStatus { bitfield: held, .. } =
                    proto::dispatch(proto_byte, opcode, &payload, false)?
                {
                    bitfield = held;
                }
                // 发 StartUpload（0x54），入对端上传队列。
                let su = proto::frame(OP_STARTUPLOADREQ, file_hash);
                stream.write_all(&su).await.map_err(DownloadError::Io)?;
                sent_start = true;
                *asked = true;
            }
            OP_FILESTATUS => {}
            OP_ACCEPTUPLOADREQ => {
                // 获得上传槽 → 可以请求分片。
                return Ok(bitfield);
            }
            OP_QUEUERANK | proto::OP_QUEUERANKING => {
                // 排队中：等待 ACCEPT（QUEUE_TIMEOUT 兜底）。
                let rank = match proto::dispatch(proto_byte, opcode, &payload, false) {
                    Ok(Ed2kMessage::QueueRank(rank)) => rank,
                    _ => 0,
                };
                live.set_queued(rank);
                wait_for_accept(stream, live).await?;
                return Ok(bitfield);
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
            // 投毒防御：用它校验任何块前先对 link root 自验；存储为 part_count 项。
            let Some(normalized) =
                hash::normalize_hashset(&part_hashes, file_hash, total_bytes, part_size)
            else {
                return Err(DownloadError::Ed2kIntegrity(
                    "hashset root mismatch (poison?)".into(),
                ));
            };
            // 幂等填充（多任务并发时首个赢，其余复用）。
            hashset_cache.get_or_init(|| async { normalized }).await;
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
    let frame = rp.to_frame();
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

/// 等待 ACCEPTUPLOAD（排队后），QUEUE_TIMEOUT 兜底；期间对端更新的名次写入 `live`。
async fn wait_for_accept(stream: &mut TcpStream, live: &PeerLive) -> Result<(), DownloadError> {
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
                    Ed2kMessage::QueueRank(rank) => live.set_queued(rank), // 仍在排队。
                    _ => return Ok(()), // ACCEPT 或数据帧 → 退出等待。
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
    fn hello_declares_large_file_capability_only() {
        let p = super::build_hello_payload();
        // 1 + 16 + 4 + 2 之后是 tagCount。
        assert_eq!(u32::from_le_bytes([p[23], p[24], p[25], p[26]]), 4);
        let tag = super::encode_peer_u32_tag(0xFE, 1 << 4);
        assert!(p.windows(tag.len()).any(|w| w == tag.as_slice()));
    }

    #[test]
    fn hello_answer_yields_nickname_and_client_software() {
        use super::{encode_peer_string_tag, encode_peer_u32_tag, parse_hello_answer};
        let header = |tag_count: u32| {
            let mut payload = vec![0xAA; 16]; // user hash
            payload.extend_from_slice(&1u32.to_le_bytes()); // client id
            payload.extend_from_slice(&4662u16.to_le_bytes()); // port
            payload.extend_from_slice(&tag_count.to_le_bytes());
            payload
        };

        // eMule 0.50a：标准名 tag，昵称带首尾空白与控制字符。
        let mut emule = header(3);
        emule.extend_from_slice(&encode_peer_string_tag(0x01, "  小明\u{7}  "));
        emule.extend_from_slice(&encode_peer_u32_tag(0x11, 0x3C));
        emule.extend_from_slice(&encode_peer_u32_tag(0xFB, 50 << 10));
        emule.extend_from_slice(&[0u8; 6]); // server ip + port
        let info = parse_hello_answer(&emule);
        assert_eq!(info.nickname, "小明");
        assert_eq!(info.client, "eMule 0.50a");

        // aMule 2.3.3：紧凑名 tag（type|0x80 + id）。
        let mut amule = header(1);
        amule.extend_from_slice(&[0x83, 0xFB]);
        amule.extend_from_slice(&((3u32 << 24) | (2 << 17) | (3 << 10) | (3 << 7)).to_le_bytes());
        assert_eq!(parse_hello_answer(&amule).client, "aMule 2.3.3");

        // 载荷截断在版本 tag 中间：保留已取到的昵称，客户端按无版本 tag 处理。
        let truncated = &emule[..emule.len() - 9];
        let info = parse_hello_answer(truncated);
        assert_eq!(info.nickname, "小明");
        assert_eq!(info.client, "eDonkey");

        // 声明的 tag 数远多于实际、未知 tag 类型、空载荷都不越界。
        let mut lying = header(u32::MAX);
        lying.extend_from_slice(&[0x7F, 0x01, 0x00, 0x01, 0xFF]);
        assert!(parse_hello_answer(&lying).nickname.is_empty());
        assert_eq!(parse_hello_answer(&[]).client, "eDonkey");
    }

    #[test]
    fn peer_has_block_bitmap_semantics() {
        use super::peer_has_block;
        assert!(peer_has_block(&[], 12345), "空位图 = 整文件");
        assert!(peer_has_block(&[0b0000_0101], 0));
        assert!(!peer_has_block(&[0b0000_0101], 1));
        assert!(peer_has_block(&[0b0000_0101], 2));
        assert!(peer_has_block(&[0, 0b0000_0010], 9));
        assert!(!peer_has_block(&[0xFF], 8), "位图长度不足 = 不持有");
    }

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
