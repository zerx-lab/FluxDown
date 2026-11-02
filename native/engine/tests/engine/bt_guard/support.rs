//! 测试公用：可控时钟、torrent 上下文、事件构造。

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use fluxdown_engine::bt_guard::{
    BtGuard, BtGuardConfig, CloudDocument, CloudEntry, GuardCtx, GuardEvent, InfoHash, PieceSet,
    RuleMode, Verdict,
};

pub const T0: i64 = 1_800_000_000;
pub const MIB: u64 = 1024 * 1024;
pub const HOUR: i64 = 3600;
pub const DAY: i64 = 86_400;
/// 默认测试种子：100 MiB，100 片，每片 1 MiB。
pub const TOTAL: u64 = 100 * MIB;
pub const PIECE_LEN: u32 = 1024 * 1024;
pub const NUM_PIECES: u32 = 100;

pub struct Clock(pub Arc<AtomicI64>);

impl Clock {
    pub fn set(&self, t: i64) {
        self.0.store(t, Ordering::SeqCst);
    }
}

pub fn guard_with(config: BtGuardConfig) -> (BtGuard, Clock) {
    let cell = Arc::new(AtomicI64::new(T0));
    let reader = Arc::clone(&cell);
    let guard = BtGuard::with_clock(config, Arc::new(move || reader.load(Ordering::SeqCst)));
    (guard, Clock(cell))
}

/// 全部规则与云名单 enforce。
pub fn enforce_config() -> BtGuardConfig {
    BtGuardConfig {
        rule_excessive: RuleMode::Enforce,
        rule_rerequest: RuleMode::Enforce,
        rule_rewind: RuleMode::Enforce,
        rule_lag: RuleMode::Enforce,
        rule_fingerprint: RuleMode::Enforce,
        cloud_mode: RuleMode::Enforce,
        ..BtGuardConfig::default()
    }
}

pub fn hash(n: u8) -> InfoHash {
    [n; 20]
}

pub fn sock(ip: &str) -> SocketAddr {
    SocketAddr::new(ip.parse::<IpAddr>().expect("ip"), 6881)
}

pub fn ctx_for(ip: &str, seeding: bool) -> GuardCtx {
    GuardCtx {
        info_hash: hash(1),
        addr: sock(ip),
        private: false,
        total_length: TOTAL,
        piece_length: PIECE_LEN,
        num_pieces: NUM_PIECES,
        we_are_seeding: seeding,
    }
}

pub fn bits(indices: impl IntoIterator<Item = usize>) -> PieceSet {
    PieceSet::from_indices(NUM_PIECES as usize, indices)
}

pub fn ev_upload(piece: u32, length: u32) -> GuardEvent<'static> {
    GuardEvent::Uploaded {
        piece,
        begin: 0,
        length,
    }
}

/// 指定块起点的上传（同一分片内不同块用不同 `begin`）。
pub fn ev_block(piece: u32, begin: u32, length: u32) -> GuardEvent<'static> {
    GuardEvent::Uploaded {
        piece,
        begin,
        length,
    }
}

pub fn ev_request(piece: u32, claims: bool) -> GuardEvent<'static> {
    GuardEvent::Request {
        piece,
        begin: 0,
        length: 16384,
        peer_claims_piece: claims,
    }
}

/// 以一整片（`piece_length` 字节）的单事件完成送达。
pub fn deliver_piece(g: &BtGuard, ctx: &GuardCtx, piece: u32, now: i64) -> Verdict {
    g.on_event_at(ctx, ev_upload(piece, PIECE_LEN), now)
}

pub fn disconnect(g: &BtGuard, ctx: &GuardCtx, bits: Option<&PieceSet>, now: i64) {
    g.on_event_at(ctx, GuardEvent::Disconnected { bits }, now);
}

pub fn cloud_doc(sequence: i64, expires_at: i64, entries: Vec<CloudEntry>) -> CloudDocument {
    CloudDocument {
        format: "fluxdown-bt-guard-list/1".to_owned(),
        sequence,
        generated_at: T0,
        expires_at,
        entries,
        ..CloudDocument::default()
    }
}

pub fn cloud_entry(cidr: &str, action: &str) -> CloudEntry {
    CloudEntry {
        cidr: cidr.to_owned(),
        category: "community".to_owned(),
        action: action.to_owned(),
        reason: "test".to_owned(),
        expires_at: None,
    }
}
