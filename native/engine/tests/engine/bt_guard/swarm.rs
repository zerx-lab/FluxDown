//! 端到端夹具：真实 librqbit 做种会话（装着防吸血策略）+ 手写线协议的假对端。
//!
//! 假对端让每条消息及其发出的时刻完全受测试控制（反复请求已声明分片、故意丢弃位图……），
//! 做种端则是生产同款的 `librqbit::Session`，策略对象来自 `BtGuardHost::policy()`。

#![allow(dead_code)]

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fluxdown_engine::bt_guard::{BtGuardConfig, BtGuardHost, HostTimings, RuleMode};
use fluxdown_engine::db::Db;
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::proxy_config::ProxyConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerOptions, ManagedTorrent, MseMode,
    Session, SessionOptions, create_torrent,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

pub const PIECE_LEN: u32 = 32 * 1024;
pub const BLOCK: u32 = 16 * 1024;
pub const PIECES: u32 = 4;
pub const FILE_LEN: usize = (PIECE_LEN * PIECES) as usize;

/// 轮询直到条件成立。
pub async fn wait_for(what: &str, mut cond: impl FnMut() -> bool, within: Duration) {
    let deadline = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn unique_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fluxdown_guard_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// 记录全部引擎事件的接收端。
#[derive(Default)]
pub struct RecordingSink {
    events: Mutex<Vec<EngineEvent>>,
}

impl RecordingSink {
    pub fn count(&self, f: impl Fn(&EngineEvent) -> bool) -> usize {
        self.events
            .lock()
            .expect("sink lock")
            .iter()
            .filter(|e| f(e))
            .count()
    }

    pub fn changed(&self) -> usize {
        self.count(|e| matches!(e, EngineEvent::BtGuardChanged))
    }

    pub fn cloud_requests(&self) -> Vec<u64> {
        self.events
            .lock()
            .expect("sink lock")
            .iter()
            .filter_map(|e| match e {
                EngineEvent::BtGuardCloudRefreshWanted { have_sequence } => Some(*have_sequence),
                _ => None,
            })
            .collect()
    }

    pub fn evidence_counts(&self) -> Vec<u32> {
        self.events
            .lock()
            .expect("sink lock")
            .iter()
            .filter_map(|e| match e {
                EngineEvent::BtGuardEvidenceReady { count } => Some(*count),
                _ => None,
            })
            .collect()
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, event: EngineEvent) {
        self.events.lock().expect("sink lock").push(event);
    }
}

/// 毫秒级节拍：测试里等得起。
pub fn fast_timings() -> HostTimings {
    HostTimings {
        coalesce: Duration::from_millis(20),
        flush_retry: Duration::from_millis(200),
        readmit_interval: Duration::from_millis(50),
        cloud_interval: Duration::from_secs(3600),
        filter_interval: Duration::from_secs(3600),
    }
}

/// 回环地址默认被豁免；端到端必须关掉豁免才能让规则对 127.0.0.1 生效。
pub fn loopback_config(mode: RuleMode) -> BtGuardConfig {
    BtGuardConfig {
        skip_lan: false,
        rule_excessive: RuleMode::Off,
        rule_rerequest: mode,
        rule_rewind: RuleMode::Off,
        rule_lag: RuleMode::Off,
        rule_fingerprint: RuleMode::Off,
        cloud_mode: RuleMode::Off,
        ..BtGuardConfig::default()
    }
}

/// guard + 内存库 + 记录接收端 + 宿主。
pub struct HostFixture {
    pub host: BtGuardHost,
    pub sink: Arc<RecordingSink>,
    pub db: Db,
}

impl HostFixture {
    pub async fn new(config: BtGuardConfig) -> Self {
        Self::with(config, fast_timings()).await
    }

    pub async fn with(config: BtGuardConfig, timings: HostTimings) -> Self {
        let db = Db::connect("sqlite::memory:").await.expect("memory db");
        Self::with_db(config, timings, db).await
    }

    /// 使用调用方提供的库（例如可被外部连接破坏 / 修复的文件库）。
    pub async fn with_db(config: BtGuardConfig, timings: HostTimings, db: Db) -> Self {
        let sink = Arc::new(RecordingSink::default());
        let guard = Arc::new(fluxdown_engine::bt_guard::BtGuard::new(config));
        let host = BtGuardHost::with_timings(
            guard,
            db.clone(),
            sink.clone(),
            ProxyConfig::default(),
            timings,
        );
        Self { host, sink, db }
    }
}

/// 做种夹具：单文件种子 + 装了策略的监听会话。
pub struct Seeder {
    pub session: Arc<Session>,
    pub handle: Arc<ManagedTorrent>,
    pub addr: SocketAddr,
    pub info_hash: [u8; 20],
    pub closing: CancellationToken,
    pub torrent_bytes: bytes::Bytes,
    dir: PathBuf,
}

impl Drop for Seeder {
    fn drop(&mut self) {
        self.closing.cancel();
        if let Err(error) = std::fs::remove_dir_all(&self.dir) {
            eprintln!("seed dir removal failed: {error}");
        }
    }
}

fn patterned(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31 + i / 251) % 251).unwrap_or(0))
        .collect()
}

impl Seeder {
    /// 创建种子、起监听会话、等待自检完成，并把会话挂到宿主上。
    pub async fn start(host: &BtGuardHost) -> Self {
        Self::start_with(Some(host)).await
    }

    /// 不装策略、不挂宿主的普通做种端（给被测的下载端当对端）。
    pub async fn start_plain() -> Self {
        Self::start_with(None).await
    }

    async fn start_with(host: Option<&BtGuardHost>) -> Self {
        let dir = unique_dir("seed");
        std::fs::create_dir_all(&dir).expect("seed dir");
        let file = dir.join("data.bin");
        std::fs::write(&file, patterned(FILE_LEN)).expect("seed file");
        let torrent = create_torrent(
            &file,
            CreateTorrentOptions {
                piece_length: Some(PIECE_LEN),
                ..Default::default()
            },
            &librqbit::spawn_utils::BlockingSpawner::new(1),
        )
        .await
        .expect("create torrent");
        let bytes = torrent.as_bytes().expect("torrent bytes");
        let session = Session::new_with_opts(
            dir.clone(),
            SessionOptions {
                dht: None,
                persistence: None,
                listen: Some(ListenerOptions {
                    listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                    ipv4_only: true,
                    ..Default::default()
                }),
                ipv4_only: true,
                disable_local_service_discovery: true,
                mse_mode: MseMode::Disabled,
                peer_policy: host.map(BtGuardHost::policy),
                ..Default::default()
            },
        )
        .await
        .expect("seed session");
        let handle = session
            .add_torrent(
                AddTorrent::from_bytes(bytes.clone()),
                Some(AddTorrentOptions {
                    paused: false,
                    output_folder: Some(path_text(&dir)),
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await
            .expect("add torrent")
            .into_handle()
            .expect("torrent handle");
        timeout(Duration::from_secs(30), handle.wait_until_completed())
            .await
            .expect("seeder did not finish its check in time")
            .expect("seeder check failed");
        let addr = session.listen_addr().expect("listen addr");
        let closing = CancellationToken::new();
        if let Some(host) = host {
            host.attach(&session, closing.clone());
        }
        Self {
            session,
            handle,
            addr,
            info_hash: torrent.info_hash().0,
            torrent_bytes: bytes,
            closing,
            dir,
        }
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------------------------
// 线协议假对端
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wire {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(Vec<u8>),
    Request(u32, u32, u32),
    Piece { index: u32, begin: u32, len: usize },
    Cancel(u32, u32, u32),
    Extended,
    Other(u8),
}

pub enum Recv {
    Msg(Wire),
    Timeout,
    Closed,
}

pub struct FakePeer {
    stream: TcpStream,
    buf: Vec<u8>,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

pub fn peer_id(prefix: &[u8; 8]) -> [u8; 20] {
    let mut id = [b'x'; 20];
    id[..8].copy_from_slice(prefix);
    id
}

fn handshake_bytes(info_hash: &[u8; 20], peer_id: &[u8; 20]) -> [u8; 68] {
    let mut b = [0u8; 68];
    b[0] = 19;
    b[1..20].copy_from_slice(b"BitTorrent protocol");
    // 扩展协议位（BEP 10）。
    b[25] = 0x10;
    b[28..48].copy_from_slice(info_hash);
    b[48..68].copy_from_slice(peer_id);
    b
}

impl FakePeer {
    /// 连接并完成握手；对端关闭 / 超时返回错误。
    pub async fn connect(
        addr: SocketAddr,
        info_hash: &[u8; 20],
        peer_id: &[u8; 20],
    ) -> Result<Self, String> {
        let mut peer = Self::connect_raw(addr).await?;
        peer.send_raw(&handshake_bytes(info_hash, peer_id)).await?;
        let mut reply = [0u8; 68];
        timeout(Duration::from_secs(3), peer.stream.read_exact(&mut reply))
            .await
            .map_err(|_| "no handshake reply".to_owned())?
            .map_err(|e| format!("handshake reply: {e}"))?;
        if &reply[28..48] != info_hash.as_slice() {
            return Err("wrong info hash in the reply".to_owned());
        }
        Ok(peer)
    }

    pub async fn connect_raw(addr: SocketAddr) -> Result<Self, String> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        Ok(Self {
            stream,
            buf: Vec::new(),
        })
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.stream
            .write_all(bytes)
            .await
            .map_err(|e| format!("write: {e}"))
    }

    pub fn frame(id: u8, payload: &[u8]) -> Vec<u8> {
        let mut m = Vec::with_capacity(5 + payload.len());
        m.extend_from_slice(&u32::try_from(payload.len() + 1).unwrap_or(1).to_be_bytes());
        m.push(id);
        m.extend_from_slice(payload);
        m
    }

    async fn send(&mut self, id: u8, payload: &[u8]) -> Result<(), String> {
        self.send_raw(&Self::frame(id, payload)).await
    }

    fn triple(a: u32, b: u32, c: u32) -> Vec<u8> {
        [a, b, c].iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    pub async fn request(&mut self, piece: u32, begin: u32, len: u32) -> Result<(), String> {
        self.send(6, &Self::triple(piece, begin, len)).await
    }

    pub async fn have(&mut self, piece: u32) -> Result<(), String> {
        self.send(4, &piece.to_be_bytes()).await
    }

    pub async fn bitfield(&mut self, bits: &[u8]) -> Result<(), String> {
        self.send(5, bits).await
    }

    pub async fn interested(&mut self) -> Result<(), String> {
        self.send(2, &[]).await
    }

    pub async fn extended_handshake(&mut self, bencoded_dict: &[u8]) -> Result<(), String> {
        let mut payload = vec![0u8];
        payload.extend_from_slice(bencoded_dict);
        self.send(20, &payload).await
    }

    fn try_parse(&mut self) -> Option<Wire> {
        if self.buf.len() < 4 {
            return None;
        }
        let len = be32(&self.buf) as usize;
        if self.buf.len() < 4 + len {
            return None;
        }
        let frame: Vec<u8> = self.buf.drain(..4 + len).skip(4).collect();
        let Some((&id, p)) = frame.split_first() else {
            return Some(Wire::KeepAlive);
        };
        Some(match (id, p.len()) {
            (0, 0) => Wire::Choke,
            (1, 0) => Wire::Unchoke,
            (2, 0) => Wire::Interested,
            (3, 0) => Wire::NotInterested,
            (4, 4) => Wire::Have(be32(p)),
            (5, _) => Wire::Bitfield(p.to_vec()),
            (6, 12) => Wire::Request(be32(p), be32(&p[4..]), be32(&p[8..])),
            (7, n) if n >= 8 => Wire::Piece {
                index: be32(p),
                begin: be32(&p[4..]),
                len: n - 8,
            },
            (8, 12) => Wire::Cancel(be32(p), be32(&p[4..]), be32(&p[8..])),
            (20, _) => Wire::Extended,
            (id, _) => Wire::Other(id),
        })
    }

    pub async fn recv(&mut self, within: Duration) -> Recv {
        let deadline = Instant::now() + within;
        loop {
            if let Some(m) = self.try_parse() {
                return Recv::Msg(m);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let mut tmp = [0u8; 64 * 1024];
            match timeout(left, self.stream.read(&mut tmp)).await {
                Err(_) => return Recv::Timeout,
                Ok(Ok(0) | Err(_)) => return Recv::Closed,
                Ok(Ok(n)) => self.buf.extend_from_slice(&tmp[..n]),
            }
        }
    }

    /// 等 `n` 个 piece 消息；连接关闭 / 超时返回错误。
    pub async fn expect_pieces(&mut self, n: usize, within: Duration) -> Result<(), String> {
        let deadline = Instant::now() + within;
        let mut got = 0;
        while got < n {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.recv(left).await {
                Recv::Msg(Wire::Piece { .. }) => got += 1,
                Recv::Msg(_) => {}
                Recv::Timeout => return Err(format!("got {got} of {n} pieces in {within:?}")),
                Recv::Closed => return Err(format!("closed after {got} of {n} pieces")),
            }
        }
        Ok(())
    }

    /// 在 `within` 内等对端关闭连接；期间收到的 piece 数一并返回。
    pub async fn closed_within(&mut self, within: Duration) -> Option<usize> {
        let deadline = Instant::now() + within;
        let mut pieces = 0;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.recv(left).await {
                Recv::Msg(Wire::Piece { .. }) => pieces += 1,
                Recv::Msg(_) => {}
                Recv::Timeout => return None,
                Recv::Closed => return Some(pieces),
            }
        }
    }

    /// `within` 内连接保持打开（没有被关闭）。
    pub async fn stays_open(&mut self, within: Duration) -> bool {
        self.closed_within(within).await.is_none()
    }
}

/// 装了策略的下载端：没有监听，只靠 `initial_peers` 出站拨号。
pub struct Leecher {
    pub session: Arc<Session>,
    pub handle: Arc<ManagedTorrent>,
    pub closing: CancellationToken,
    dir: PathBuf,
}

impl Drop for Leecher {
    fn drop(&mut self) {
        self.closing.cancel();
        if let Err(error) = std::fs::remove_dir_all(&self.dir) {
            eprintln!("leech dir removal failed: {error}");
        }
    }
}

impl Leecher {
    pub async fn start(host: &BtGuardHost, seed: &Seeder) -> Self {
        let dir = unique_dir("leech");
        std::fs::create_dir_all(&dir).expect("leech dir");
        let session = Session::new_with_opts(
            dir.clone(),
            SessionOptions {
                dht: None,
                persistence: None,
                listen: None,
                connect: Some(librqbit::ConnectionOptions::default()),
                ipv4_only: true,
                disable_local_service_discovery: true,
                mse_mode: MseMode::Disabled,
                peer_policy: Some(host.policy()),
                ..Default::default()
            },
        )
        .await
        .expect("leech session");
        let closing = CancellationToken::new();
        host.attach(&session, closing.clone());
        let handle = session
            .add_torrent(
                AddTorrent::from_bytes(seed.torrent_bytes.clone()),
                Some(AddTorrentOptions {
                    paused: false,
                    initial_peers: Some(vec![seed.addr]),
                    ..Default::default()
                }),
            )
            .await
            .expect("add torrent")
            .into_handle()
            .expect("torrent handle");
        Self {
            session,
            handle,
            closing,
            dir,
        }
    }

    pub async fn completed(&self, within: Duration) -> bool {
        matches!(
            timeout(within, self.handle.wait_until_completed()).await,
            Ok(Ok(()))
        )
    }
}
