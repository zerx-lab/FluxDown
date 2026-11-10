//! `BtGuard` 内部可变状态（全部由 `BtGuard::state` 互斥锁保护）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};

use super::bitset::PieceSet;
use super::cloud::ClientRule;
use super::ip::IpGroup;
use super::layers::Decided;
use super::model::{EvidenceItem, GuardAction, InfoHash, Rule, Verdict};
use crate::db::{
    BT_GUARD_EVENT_RING, BtGuardBatch, BtGuardEntryOp, BtGuardEventRow, BtGuardLedgerRow,
};

pub(super) type ConnKey = (InfoHash, SocketAddr);

/// 账本内存预算（每种子）与每条账本的固定开销估计（结构体 + 哈希表槽位，字节）。
const LEDGER_MEMORY_BUDGET: usize = 8 * 1024 * 1024;
const LEDGER_ENTRY_OVERHEAD: usize = 256;

/// 每种子账本组数上限：`min(4096, floor(8 MiB / (2 × ceil(num_pieces/8) + 固定开销)))`，至少 64。
/// 每条账本持有 delivered / last_claimed 两个按分片的位集，所以大种子上限随分片数收缩。
pub fn ledger_group_cap(num_pieces: u32) -> usize {
    let bitset = (num_pieces as usize).div_ceil(8);
    (LEDGER_MEMORY_BUDGET / (2 * bitset + LEDGER_ENTRY_OVERHEAD)).clamp(64, 4096)
}
/// 账本保留期。
pub(super) const LEDGER_RETENTION_SECS: i64 = 14 * 86_400;
/// strike 衰减期：这么久没有新 strike 即归零。
pub(super) const STRIKE_DECAY_SECS: i64 = 30 * 86_400;
/// 待上报证据队列上限。
pub(super) const EVIDENCE_CAP: usize = 1000;
/// lag：送达后的宽限期。
pub(super) const LAG_GRACE_SECS: i64 = 120;
/// lag：同一连接两次评估的最小间隔（热路径节流）。
pub(super) const LAG_CHECK_INTERVAL_SECS: i64 = 15;
/// lag：同组探测最小间隔。
pub(super) const PROBE_INTERVAL_SECS: i64 = 24 * 3600;
/// 同组同规则 observe 记录的去重窗口。
pub(super) const OBSERVE_GATE_SECS: i64 = 3600;
/// 单连接记录的已送达分片时间上限。
pub(super) const CONN_DELIVERED_CAP: usize = 65_536;
/// rewind：判定所需的最小丢失片数（小位图上 1~2 片的抖动不足以证明回退）。
pub(super) const REWIND_MIN_LOST_PIECES: usize = 4;
/// rewind：NAT 保护（同组有其它存活连接 / 空位图换客户端）对单个账本 24h 内最多跳过的次数。
pub(super) const REWIND_SKIP_QUOTA: u32 = 3;
pub(super) const REWIND_SKIP_WINDOW_SECS: i64 = 24 * 3600;

pub(super) struct Ledger {
    pub delivered_bytes: u64,
    pub dup_bytes: u64,
    /// 已完整送达的分片。
    pub delivered: PieceSet,
    /// 最近一次连接结束时对端位图。
    pub last_claimed: PieceSet,
    pub rerequests: u32,
    /// 0 = 从未探测。
    pub probed_at: i64,
    pub first_seen: i64,
    pub last_seen: i64,
    /// 探测中：断开前对端缺失的分片（仅内存；重启后丢失即放弃该次探测）。
    pub probe_pending: Option<Vec<u32>>,
    pub torrent_size: u64,
    /// 最近一次 lag 探测被「补齐」清除的时刻（0 = 无）。冷却期内 lag 只观察不处置。
    pub lag_cleared_at: i64,
    /// 上次因 excessive 封禁时的 `delivered_bytes` 基线；再犯只看超出基线的增量。
    pub excess_baseline: u64,
    /// 上次连接结束时对端 peer_id 前 8 字节（仅内存）。
    pub last_peer_prefix: Option<[u8; 8]>,
    /// NAT 保护跳过 rewind 的计数窗口起点与次数（仅内存）。
    pub rewind_skip_since: i64,
    pub rewind_skips: u32,
}

impl Ledger {
    pub fn new(now: i64, torrent_size: u64) -> Self {
        Self {
            delivered_bytes: 0,
            dup_bytes: 0,
            delivered: PieceSet::default(),
            last_claimed: PieceSet::default(),
            rerequests: 0,
            probed_at: 0,
            first_seen: now,
            last_seen: now,
            probe_pending: None,
            torrent_size,
            lag_cleared_at: 0,
            excess_baseline: 0,
            last_peer_prefix: None,
            rewind_skip_since: 0,
            rewind_skips: 0,
        }
    }

    /// 消耗一次 NAT 保护跳过额度；成功返回剩余次数，额度用完返回 `None`。窗口 24h 滚动。
    pub fn try_skip_rewind(&mut self, now: i64) -> Option<u32> {
        if self.rewind_skips == 0 || now - self.rewind_skip_since >= REWIND_SKIP_WINDOW_SECS {
            self.rewind_skip_since = now;
            self.rewind_skips = 0;
        }
        if self.rewind_skips >= REWIND_SKIP_QUOTA {
            return None;
        }
        self.rewind_skips += 1;
        Some(REWIND_SKIP_QUOTA - self.rewind_skips)
    }
}

pub(super) struct StrikeRec {
    pub strikes: u32,
    pub last_strike_at: i64,
    /// 最近一次封禁的到期时刻（0 = 无）。
    pub ban_until: i64,
}

impl StrikeRec {
    /// 衰减起算点：最近一次 strike 与最近一次封禁到期时刻中较晚者。
    /// 这样封顶时长的封禁到期后立即再犯仍升档，只有「封禁结束后」整整 30 天无新 strike 才归零。
    pub fn decay_from(&self) -> i64 {
        self.last_strike_at.max(self.ban_until)
    }
}

#[derive(Clone, Copy)]
pub(super) struct CachedDecision {
    pub epoch: u64,
    pub decided: Decided,
    pub valid: bool,
}

#[derive(Default)]
pub(super) struct PieceProgress {
    pub bytes: u32,
    pub begins: HashSet<u32>,
}

pub(super) struct Conn {
    pub group: IpGroup,
    pub ip: IpAddr,
    pub seeding: bool,
    pub peer_id: Option<[u8; 20]>,
    pub client: Option<String>,
    /// 对端声明拥有的分片（bitfield + HAVE）。
    pub claimed: PieceSet,
    /// 对端请求过、且此前声明已拥有的不同分片。
    pub claimed_requests: HashSet<u32>,
    /// 本连接内进行中的分片：已收到的不同块起点与累计字节（同一块重发不重复累计）。
    pub progress: HashMap<u32, PieceProgress>,
    /// 本连接内完整送达的分片及时刻（推送序 = 时间序）。
    pub delivered_at: Vec<(u32, i64)>,
    pub last_lag_check: i64,
    pub verdict: Verdict,
    /// 已处理过的规则位（每连接每规则只处理一次）。
    pub fired: u8,
    /// 命中的规则与是否已执行。
    pub hits: Vec<(Rule, bool)>,
    pub decision: CachedDecision,
    /// 已应用的过滤层判决（避免每个事件重复计数）。
    pub layer_applied: Option<(i64, super::model::FilterAction)>,
}

impl Conn {
    pub fn new(group: IpGroup, ip: IpAddr) -> Self {
        Self {
            group,
            ip,
            seeding: false,
            peer_id: None,
            client: None,
            claimed: PieceSet::default(),
            claimed_requests: HashSet::new(),
            progress: HashMap::new(),
            delivered_at: Vec::new(),
            last_lag_check: i64::MIN / 2,
            verdict: Verdict::Allow,
            fired: 0,
            hits: Vec::new(),
            decision: CachedDecision {
                epoch: u64::MAX,
                decided: Decided {
                    decision: super::layers::Decision::None,
                    valid_until: i64::MIN,
                },
                valid: false,
            },
            layer_applied: None,
        }
    }

    pub fn raise(&mut self, verdict: Verdict) {
        if verdict > self.verdict {
            self.verdict = verdict;
        }
    }
}

#[derive(Default)]
pub(super) struct CloudState {
    pub sequence: i64,
    pub updated_at: i64,
    /// 文档过期时刻；0 = 从未应用。
    pub valid_until: i64,
    pub entries: usize,
    pub client_rules: Vec<ClientRule>,
    pub attribution: Vec<String>,
    pub last_error: Option<String>,
    pub rejected_reason: Option<String>,
}

pub(super) struct FileStatus {
    pub ok: bool,
    pub entries: usize,
    pub updated_at: i64,
    pub error: Option<String>,
    /// 上次成功应用的正文 SHA-256（仅内存）。
    pub hash: Option<[u8; 32]>,
    /// 上次应用被条目上限截断的说明（`error` 在刷新成功后回落到它）。
    pub truncation: Option<String>,
    pub last: Option<super::guard::FilterApplySummary>,
}

#[derive(Default)]
pub(super) struct Pending {
    pub entry_ops: Vec<BtGuardEntryOp>,
    pub events: VecDeque<BtGuardEventRow>,
    pub dirty_ledgers: HashSet<(InfoHash, IpGroup)>,
    pub removed_ledgers: Vec<(InfoHash, IpGroup)>,
    pub dirty_strikes: HashSet<IpGroup>,
    /// 被 LRU 淘汰时仍脏的账本：行已生成，随下次 flush 落库（不丢进度）。
    pub evicted: Vec<BtGuardLedgerRow>,
    /// 云名单文档元数据 JSON，随下次 flush 落库。
    pub cloud_meta: Option<String>,
    /// 因 `Disconnected` 变脏的账本：普通 flush 只写这些（其余脏账本留到判定 / 处置 /
    /// 会话关闭时再写，避免任一连接断开都把所有持续上传的账本整行重写）。
    pub disconnected: HashSet<(InfoHash, IpGroup)>,
    /// 发生了判定 / 处置 / 解封：下一次 flush 写入全部脏账本。
    pub urgent: bool,
}

impl Pending {
    /// 普通 flush 有事可做（不含仅因上传累加而变脏的账本）。
    pub fn is_empty(&self) -> bool {
        self.entry_ops.is_empty()
            && self.events.is_empty()
            && self.removed_ledgers.is_empty()
            && self.dirty_strikes.is_empty()
            && self.evicted.is_empty()
            && self.cloud_meta.is_none()
            && self.disconnected.is_empty()
            && !self.urgent
    }

    /// 连上传累加留下的脏账本也算（`flush_all` 才会写它们）。
    pub fn is_clean(&self) -> bool {
        self.is_empty() && self.dirty_ledgers.is_empty()
    }
}

#[derive(Default)]
pub(super) struct State {
    pub conns: HashMap<ConnKey, Conn>,
    pub ledgers: HashMap<InfoHash, HashMap<IpGroup, Ledger>>,
    pub strikes: HashMap<IpGroup, StrikeRec>,
    pub evidence: VecDeque<EvidenceItem>,
    /// 已入队证据的去重键。
    pub reported: HashSet<(IpGroup, Rule, InfoHash)>,
    /// observe 记录去重：(组, 规则) → 最近记录时刻。
    pub observed_gate: HashMap<(IpGroup, Rule), i64>,
    pub cloud: CloudState,
    pub files: std::collections::BTreeMap<String, FileStatus>,
    pub pending: Pending,
    pub actions: Vec<GuardAction>,
    /// 期间发生了宿主需要处理的新事项；`StateGuard` 释放时据此唤醒宿主并清零。
    pub wake: bool,
}

impl State {
    pub fn push_action(&mut self, action: GuardAction) {
        if !self.actions.contains(&action) {
            self.actions.push(action);
            self.wake = true;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_event(
        &mut self,
        at: i64,
        kind: &str,
        group: Option<IpGroup>,
        info_hash: Option<&InfoHash>,
        rule: &str,
        mode: &str,
        action: &str,
        detail: String,
    ) {
        self.wake = true;
        self.pending.events.push_back(BtGuardEventRow {
            id: 0,
            at,
            kind: kind.to_owned(),
            ip_group: group.map(|g| g.to_string()).unwrap_or_default(),
            info_hash: info_hash.map(hex::encode).unwrap_or_default(),
            rule: rule.to_owned(),
            mode: mode.to_owned(),
            action: action.to_owned(),
            detail,
        });
        while self.pending.events.len() > BT_GUARD_EVENT_RING as usize {
            self.pending.events.pop_front();
        }
    }

    /// 取得（必要时创建）账本并标记待落库；超出该种子上限（见 [`ledger_group_cap`]）时淘汰
    /// `last_seen` 最旧者：干净的随 flush 从库中删除，脏的先生成行保留待落库。
    pub fn ledger_mut(
        &mut self,
        hash: InfoHash,
        group: IpGroup,
        now: i64,
        size: u64,
        num_pieces: u32,
    ) -> &mut Ledger {
        let cap = ledger_group_cap(num_pieces);
        let map = self.ledgers.entry(hash).or_default();
        if !map.contains_key(&group) {
            while map.len() >= cap {
                let Some(victim) = map.iter().min_by_key(|(_, l)| l.last_seen).map(|(g, _)| *g)
                else {
                    break;
                };
                if let Some(old) = map.remove(&victim) {
                    if self.pending.dirty_ledgers.remove(&(hash, victim)) {
                        self.pending.evicted.push(ledger_row(&hash, &victim, &old));
                    } else {
                        self.pending.removed_ledgers.push((hash, victim));
                    }
                }
            }
            // 重建一个曾被淘汰的账本：之前排队的「删除该键」已过时，留着会把新账本一并删掉。
            self.pending
                .removed_ledgers
                .retain(|key| *key != (hash, group));
        }
        self.pending.dirty_ledgers.insert((hash, group));
        let ledger = map.entry(group).or_insert_with(|| Ledger::new(now, size));
        ledger.last_seen = now;
        if size > 0 {
            ledger.torrent_size = size;
        }
        ledger
    }

    pub fn ledger(&self, hash: &InfoHash, group: &IpGroup) -> Option<&Ledger> {
        self.ledgers.get(hash).and_then(|m| m.get(group))
    }

    /// 取走待落库内容并生成批次；返回批次与被取走的键（失败时用于还原）。
    pub fn restore(&mut self, batch: BtGuardBatch, keys: TakenKeys) {
        self.wake = true;
        let mut ops = batch.entry_ops;
        ops.append(&mut self.pending.entry_ops);
        self.pending.entry_ops = ops;
        for row in batch.events.into_iter().rev() {
            self.pending.events.push_front(row);
        }
        while self.pending.events.len() > BT_GUARD_EVENT_RING as usize {
            self.pending.events.pop_front();
        }
        self.pending.dirty_ledgers.extend(keys.dirty_ledgers);
        self.pending.dirty_strikes.extend(keys.dirty_strikes);
        // 还原时键已被重新建立的账本：改为重写它而不是删除。
        for key in keys.removed_ledgers {
            if self.ledger(&key.0, &key.1).is_some() {
                self.pending.dirty_ledgers.insert(key);
            } else {
                self.pending.removed_ledgers.push(key);
            }
        }
        self.pending.evicted.extend(keys.evicted);
        self.pending.disconnected.extend(keys.disconnected);
        self.pending.urgent |= keys.urgent;
        if self.pending.cloud_meta.is_none() {
            self.pending.cloud_meta = batch.cloud_meta;
        }
    }
}

#[derive(Default)]
pub(super) struct TakenKeys {
    pub dirty_ledgers: Vec<(InfoHash, IpGroup)>,
    pub dirty_strikes: Vec<IpGroup>,
    pub removed_ledgers: Vec<(InfoHash, IpGroup)>,
    pub evicted: Vec<BtGuardLedgerRow>,
    pub disconnected: Vec<(InfoHash, IpGroup)>,
    pub urgent: bool,
}

pub(super) fn ledger_row(hash: &InfoHash, group: &IpGroup, l: &Ledger) -> BtGuardLedgerRow {
    BtGuardLedgerRow {
        info_hash: hex::encode(hash),
        ip_group: group.to_string(),
        delivered_bytes: i64::try_from(l.delivered_bytes).unwrap_or(i64::MAX),
        dup_bytes: i64::try_from(l.dup_bytes).unwrap_or(i64::MAX),
        delivered_bits: l.delivered.as_bytes().to_vec(),
        claimed_bits: l.last_claimed.as_bytes().to_vec(),
        rerequests: i64::from(l.rerequests),
        probed_at: l.probed_at,
        first_seen: l.first_seen,
        last_seen: l.last_seen,
        lag_cleared_at: l.lag_cleared_at,
        excess_baseline: i64::try_from(l.excess_baseline).unwrap_or(i64::MAX),
    }
}
