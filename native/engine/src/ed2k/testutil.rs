//! ED2K mock TCP 对端（peer + server），集成测试专用，仅 `cfg(test)` 编译。
//!
//! - [`MockPeer`]：接受直连，执行 HELLO→(多块)HASHSET→REQUESTPARTS→SENDINGPART
//!   全流程，按 [`PeerFault`] 注入完整性攻击面（投毒 hashset / 越界 / 长度不符 /
//!   未请求数据 / 压缩帧）。
//! - [`MockServer`]：接受登录 + GETSOURCES，回 IDCHANGE + FOUNDSOURCES（HighID）。
//!
//! 二者均 `bind` 到 `127.0.0.1:0`（临时端口），循环 accept，`Drop` 时 `abort`。

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::downloader::DownloadError;
use crate::ed2k::hash;
use crate::ed2k::proto::{
    self, MAX_SERVER_FRAME, OP_ACCEPTUPLOADREQ, OP_COMPRESSEDPART, OP_FILEREQANSWER, OP_FILESTATUS,
    OP_FOUNDSOURCES, OP_GETSOURCES, OP_HASHSETANSWER, OP_HASHSETREQUEST, OP_HELLO, OP_HELLOANSWER,
    OP_IDCHANGE, OP_LOGINREQUEST, OP_REQUESTFILENAME, OP_REQUESTPARTS, OP_REQUESTPARTS_I64,
    OP_SENDINGPART, OP_SENDINGPART_I64, OP_SETREQFILEID, OP_STARTUPLOADREQ, PROTO_EMULE,
    RequestParts,
};
use crate::ed2k::server::PeerAddr;

// ---------------------------------------------------------------------------
// 测试数据辅助
// ---------------------------------------------------------------------------

/// 计算给定字节流的 eD2K root hash（含 phantom-tail 处理）。
#[must_use]
pub fn root_hash(data: &[u8], part_size: u64) -> [u8; 16] {
    let total = data.len() as u64;
    let pc = hash::part_count(total, part_size);
    let mut hashes = Vec::with_capacity(pc as usize);
    for i in 0..pc {
        let (s, e) = hash::part_span(i, total, part_size);
        hashes.push(hash::hash_part(&data[s as usize..e as usize]));
    }
    hash::compute_root(&hash::build_root_input(&hashes, total, part_size))
}

/// 构造 `ed2k://|file|<name>|<size>|<hex root>|/` 链接。
#[must_use]
pub fn ed2k_link(name: &str, data: &[u8], part_size: u64) -> String {
    let root = root_hash(data, part_size);
    format!(
        "ed2k://|file|{}|{}|{}|/",
        name,
        data.len(),
        hex::encode(root)
    )
}

// ---------------------------------------------------------------------------
// Mock peer
// ---------------------------------------------------------------------------

/// peer 注入的完整性攻击面（首个分片响应生效一次）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PeerFault {
    /// 正常行为。
    #[default]
    None,
    /// HASHSETANSWER 首块哈希置错 → root 自验失败。
    PoisonHashset,
    /// SENDINGPART 声明 `end_exclusive` 超文件尾。
    OutOfBounds,
    /// 数据实际长度 ≠ 声明区间长度。
    LengthMismatch,
    /// 发送客户端从未请求的（但在界内的）区间数据。
    Unrequested,
    /// 用 zlib 压缩帧（`OP_COMPRESSEDPART`）发送（正常路径变体）。
    Compressed,
    /// Hold the first body response so tests can observe a pending peer read.
    DelayBody,
}

/// 单文件 mock peer：按 leech 客户端期望的时序应答。
pub struct MockPeer {
    /// 监听地址（`127.0.0.1:<临时端口>`）。
    pub addr: SocketAddr,
    /// 收到的 `OP_STARTUPLOADREQ` 总数（真实 eMule 对重复申请计坏请求并封禁）。
    pub slot_requests: Arc<AtomicUsize>,
    handle: JoinHandle<()>,
}

impl Drop for MockPeer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl MockPeer {
    /// 起一个 mock peer，服务 `file_data` 全量内容。
    ///
    /// # Errors
    ///
    /// TCP 绑定失败时返回 [`io::Error`]。
    pub async fn spawn(file_data: Vec<u8>, part_size: u64, fault: PeerFault) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let data = Arc::new(file_data);
        let slot_requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&slot_requests);
        let handle = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let d = Arc::clone(&data);
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    if let Err(error) = serve_peer(stream, d, part_size, fault, counter).await {
                        tracing::debug!(%error, "ED2K mock peer connection ended");
                    }
                });
            }
        });
        Ok(Self {
            addr,
            slot_requests,
            handle,
        })
    }

    /// 本 peer 的 [`PeerAddr`]（供 [`crate::ed2k::peer::download_block_from_peer`]）。
    #[must_use]
    pub fn peer_addr(&self) -> PeerAddr {
        PeerAddr {
            ip: Ipv4Addr::LOCALHOST,
            port: self.addr.port(),
        }
    }
}

async fn serve_peer(
    mut stream: TcpStream,
    data: Arc<Vec<u8>>,
    part_size: u64,
    fault: PeerFault,
    slot_requests: Arc<AtomicUsize>,
) -> Result<(), DownloadError> {
    let total = data.len() as u64;
    let mut fired = false;
    loop {
        let (proto_byte, opcode, payload) =
            match proto::read_frame(&mut stream, MAX_SERVER_FRAME).await {
                Ok(f) => f,
                Err(_) => return Ok(()), // 对端关闭。
            };
        match opcode {
            OP_HELLO => {
                let mut p = Vec::new();
                p.extend_from_slice(&[0u8; 16]); // user hash
                p.extend_from_slice(&0u32.to_le_bytes()); // id
                p.extend_from_slice(&0u16.to_le_bytes()); // port
                p.extend_from_slice(&0u32.to_le_bytes()); // tag count
                stream
                    .write_all(&proto::frame(OP_HELLOANSWER, &p))
                    .await
                    .map_err(DownloadError::Io)?;
            }
            OP_REQUESTFILENAME => {
                // 对端持有文件 → FileAnswer：hash(16) + nameLen(u16) + name。
                let mut p = Vec::new();
                p.extend_from_slice(&[0u8; 16]);
                p.extend_from_slice(&4u16.to_le_bytes());
                p.extend_from_slice(b"mock");
                stream
                    .write_all(&proto::frame(OP_FILEREQANSWER, &p))
                    .await
                    .map_err(DownloadError::Io)?;
            }
            OP_SETREQFILEID => {
                // FileStatus：hash(16) + bitCount(u16=0)（全量持有的简写，
                // leech 端只按 opcode 推进协商，不读位图）。
                let mut p = Vec::new();
                p.extend_from_slice(&[0u8; 16]);
                p.extend_from_slice(&0u16.to_le_bytes());
                stream
                    .write_all(&proto::frame(OP_FILESTATUS, &p))
                    .await
                    .map_err(DownloadError::Io)?;
            }
            OP_STARTUPLOADREQ => {
                slot_requests.fetch_add(1, Ordering::Relaxed);
                // 直接授予上传槽。
                stream
                    .write_all(&proto::frame(OP_ACCEPTUPLOADREQ, &[]))
                    .await
                    .map_err(DownloadError::Io)?;
            }
            OP_HASHSETREQUEST => {
                let pc = hash::part_count(total, part_size);
                let mut hashes = Vec::with_capacity(pc as usize);
                for i in 0..pc {
                    let (s, e) = hash::part_span(i, total, part_size);
                    hashes.push(hash::hash_part(&data[s as usize..e as usize]));
                }
                if fault == PeerFault::PoisonHashset && !hashes.is_empty() {
                    hashes[0] = [0xAB; 16];
                }
                let mut p = Vec::new();
                p.extend_from_slice(&[0u8; 16]); // file hash（peer 不校验此字段）
                p.extend_from_slice(&(pc as u16).to_le_bytes());
                for h in &hashes {
                    p.extend_from_slice(h);
                }
                stream
                    .write_all(&proto::frame(OP_HASHSETANSWER, &p))
                    .await
                    .map_err(DownloadError::Io)?;
            }
            // 与 eMule 一致：I64 请求只在扩展协议（0xC5）下识别，0xE3 下的同 opcode 静默忽略。
            OP_REQUESTPARTS | OP_REQUESTPARTS_I64
                if (opcode == OP_REQUESTPARTS_I64) == (proto_byte == PROTO_EMULE) =>
            {
                if fault == PeerFault::DelayBody && !fired {
                    fired = true;
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
                let large = opcode == OP_REQUESTPARTS_I64;
                let rp = RequestParts::decode(&payload, large)?;
                if fault == PeerFault::Unrequested && !fired {
                    fired = true;
                    // 界内但从未请求的区间：[3*BS, 4*BS)（要求块 ≥ 4*BLOCK_SIZE）。
                    let bs = hash::BLOCK_SIZE;
                    let s = 3 * bs;
                    let e = (4 * bs).min(total);
                    send_sending_part(&mut stream, s, e, &data[s as usize..e as usize], large)
                        .await?;
                    continue;
                }
                for r in rp.ranges.iter().flatten() {
                    let apply = !fired
                        && matches!(
                            fault,
                            PeerFault::OutOfBounds
                                | PeerFault::LengthMismatch
                                | PeerFault::Compressed
                        );
                    if apply {
                        fired = true;
                    }
                    let slice = &data[r.start as usize..r.end_exclusive as usize];
                    match (apply, fault) {
                        (true, PeerFault::OutOfBounds) => {
                            send_sending_part(&mut stream, r.start, total + 100, slice, large)
                                .await?;
                        }
                        (true, PeerFault::LengthMismatch) => {
                            let short = &slice[..slice.len().saturating_sub(1)];
                            send_sending_part(&mut stream, r.start, r.end_exclusive, short, large)
                                .await?;
                        }
                        (true, PeerFault::Compressed) => {
                            send_compressed_part(&mut stream, r.start, slice).await?;
                        }
                        _ => {
                            send_sending_part(&mut stream, r.start, r.end_exclusive, slice, large)
                                .await?;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

async fn send_sending_part(
    stream: &mut TcpStream,
    start: u64,
    end_exclusive: u64,
    data: &[u8],
    large: bool,
) -> Result<(), DownloadError> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0u8; 16]); // file hash（peer 端忽略）
    if large {
        p.extend_from_slice(&start.to_le_bytes());
        p.extend_from_slice(&end_exclusive.to_le_bytes());
        p.extend_from_slice(data);
        stream
            .write_all(&proto::frame_with_proto(
                PROTO_EMULE,
                OP_SENDINGPART_I64,
                &p,
            ))
            .await
            .map_err(DownloadError::Io)
    } else {
        p.extend_from_slice(&(start as u32).to_le_bytes());
        p.extend_from_slice(&(end_exclusive as u32).to_le_bytes());
        p.extend_from_slice(data);
        stream
            .write_all(&proto::frame(OP_SENDINGPART, &p))
            .await
            .map_err(DownloadError::Io)
    }
}

async fn send_compressed_part(
    stream: &mut TcpStream,
    start: u64,
    data: &[u8],
) -> Result<(), DownloadError> {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).map_err(DownloadError::Io)?;
    let packed = enc.finish().map_err(DownloadError::Io)?;

    let mut p = Vec::new();
    p.extend_from_slice(&[0u8; 16]); // file hash
    p.extend_from_slice(&(start as u32).to_le_bytes());
    p.extend_from_slice(&(packed.len() as u32).to_le_bytes());
    p.extend_from_slice(&packed);
    stream
        .write_all(&proto::frame_with_proto(PROTO_EMULE, OP_COMPRESSEDPART, &p))
        .await
        .map_err(DownloadError::Io)
}

// ---------------------------------------------------------------------------
// Mock server
// ---------------------------------------------------------------------------

/// mock eD2K 索引服务器：登录后对 GETSOURCES 回一组 HighID 源。
pub struct MockServer {
    /// 监听地址（`127.0.0.1:<临时端口>`）。
    pub addr: SocketAddr,
    handle: JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl MockServer {
    /// 起一个 mock server，GETSOURCES 回 `peers`。
    ///
    /// # Errors
    ///
    /// TCP 绑定失败时返回 [`io::Error`]。
    pub async fn spawn(peers: Vec<PeerAddr>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let peers = Arc::new(peers);
        let handle = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let p = Arc::clone(&peers);
                tokio::spawn(async move {
                    if let Err(error) = serve_server(stream, p).await {
                        tracing::debug!(%error, "ED2K mock server connection ended");
                    }
                });
            }
        });
        Ok(Self { addr, handle })
    }

    /// `find_sources` 服务器列表用的 `host:port` 串。
    #[must_use]
    pub fn server_string(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }
}

async fn serve_server(
    mut stream: TcpStream,
    peers: Arc<Vec<PeerAddr>>,
) -> Result<(), DownloadError> {
    loop {
        let (_proto, opcode, payload) = match proto::read_frame(&mut stream, MAX_SERVER_FRAME).await
        {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };
        match opcode {
            OP_LOGINREQUEST => {
                // IDCHANGE：给我方分配一个 client id（值不影响出站拉取）。
                let p = 0x0034_5678u32.to_le_bytes();
                stream
                    .write_all(&proto::frame(OP_IDCHANGE, &p))
                    .await
                    .map_err(DownloadError::Io)?;
            }
            OP_GETSOURCES => {
                let mut fh = [0u8; 16];
                if payload.len() >= 16 {
                    fh.copy_from_slice(&payload[..16]);
                }
                let mut p = Vec::new();
                p.extend_from_slice(&fh);
                p.push(peers.len() as u8);
                for peer in peers.iter() {
                    // HighID = IPv4 四字节小端打包为 u32。
                    let id = u32::from_le_bytes(peer.ip.octets());
                    p.extend_from_slice(&id.to_le_bytes());
                    p.extend_from_slice(&peer.port.to_le_bytes());
                }
                stream
                    .write_all(&proto::frame(OP_FOUNDSOURCES, &p))
                    .await
                    .map_err(DownloadError::Io)?;
            }
            _ => {}
        }
    }
}
