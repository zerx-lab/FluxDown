//! BT 防吸血（bt_guard）。
//!
//! 核心（`guard` / `events` / `layers` / `store` 等）是纯逻辑与存储，不依赖 librqbit；
//! 宿主侧胶水依赖 librqbit，集中在三个模块：
//! - [`policy`]：把 rqbit 的 `PeerPolicy` 回调一对一转换成 [`GuardCtx`] / [`GuardEvent`]
//!   喂给 [`BtGuard::on_event`]，用返回的 [`Verdict`] 执行；`admit` 在连接前调用；
//! - [`host`]：由 guard 的 waker 驱动的泵（执行 [`BtGuard::take_actions`]、事件驱动落库）
//!   与随 BT 会话存亡的节拍任务（释放到期封禁 / 云名单 / 订阅）；
//! - [`filter_fetch`]：过滤文件订阅的拉取。
//!
//! 设计要点：
//! - 热路径（`admit` / `on_event`）无 IO：只改内存，待落库 / 待处置内容入队。
//! - 账本键 `(info_hash, ip_group)`；IPv4 /32，IPv6 按前缀聚合，v4-mapped / NAT64 归一为 v4。
//! - 规则 mode：off / observe / enforce；observe 只记录「本应执行」的动作，返回值恒为 `Allow`。
//! - 过滤层 allow > manual > auto / cloud / file；到期惰性判断，整层重建后原子替换。
//! - 云名单验签不在 engine（agent 负责）；这里只做 sequence / 金丝雀 / 差异 / 过期 / 宽条目防护。

mod bitset;
mod cloud;
mod config;
mod events;
mod filter_fetch;
mod filter_file;
mod guard;
mod host;
mod ip;
mod layers;
mod model;
mod policy;
mod ranges;
mod state;
mod store;

pub use bitset::PieceSet;
pub use cloud::{
    BUILTIN_CANARIES, CLOUD_FORMAT, ClientField, ClientOp, ClientRule, CloudBuild, CloudClientRule,
    CloudDocument, CloudEntry, CloudReject, build_cloud_layer,
};
pub use config::{BtGuardConfig, DownloadingMaxAction, RuleMode};
pub use filter_file::{
    MAX_FILTER_ENTRIES_PER_SOURCE, MAX_FILTER_RANGES, ParsedFilter, parse_filter_file,
};
pub use guard::{
    BtGuard, CloudApplySummary, CloudStatus, EntryFilter, FilterApplySummary, FilterSourceStatus,
    GuardError, NewEntry, ban_duration_secs,
};
pub use ip::{IpGroup, is_lan, normalize_ip};
pub use layers::{DecideInput, Decided, Decision, LayerIndex, Tables, decide};
pub use model::{
    Entry, EvidenceItem, FilterAction, GuardAction, GuardCtx, GuardEvent, GuardStats, InfoHash,
    Layer, LayerCounts, PeerInsight, Rule, Strength, Verdict,
};
pub use ranges::{IpRange, RangeError, RangeIndex};
pub use state::ledger_group_cap;
pub use store::LoadSummary;

pub use filter_fetch::{MAX_BODY_BYTES as MAX_FILTER_BODY_BYTES, refresh_sources};
pub use host::{BtGuardHost, HostTimings};
pub use policy::{GuardPolicy, guard_ctx, peer_verdict};
