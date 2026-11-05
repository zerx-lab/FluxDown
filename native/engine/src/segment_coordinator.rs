//! IDM-style dynamic segment coordinator.
//!
//! Instead of spawning a fixed set of segment tasks and waiting for them all to
//! finish, the coordinator manages a pool of **workers** that are assigned
//! segments on demand.  When a worker finishes its segment, it asks the
//! coordinator for more work — which may be an existing pending segment or a
//! **newly created** segment obtained by splitting the largest in-progress
//! segment in half (the *in-half division rule*).
//!
//! This achieves two key IDM-style behaviours:
//! 1. **Connection reuse** — TCP/TLS connections stay alive across segments.
//! 2. **Dynamic segmentation** — slow segments are split at runtime so idle
//!    workers can help.
//!
//! ## Invariants
//!
//! After every mutation of the segment map, these invariants hold:
//! - The union of all `[start_byte, end_byte]` ranges covers `[0, total_bytes-1]`
//!   exactly, with no gaps and no overlaps.
//! - `next_index` is strictly greater than any existing segment index.
//! - Every segment's `downloaded_bytes <= end_byte - start_byte + 1`.
//!
//! ## Crash safety
//!
//! On resume, the segment map is rebuilt from DB rows.  A split that was
//! persisted to DB but whose worker never started is restored as `Pending`.
//! A split whose parent's `end_byte` was updated but the new child row wasn't
//! written yet results in a gap — the integrity check at the end of download
//! catches this and the task retries from scratch.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

mod file_layout;
mod multipath;
mod request_path;
mod splice_audit;

use request_path::RequestPath;

use crate::cdn::NodePool;
use crate::db::Db;
use crate::downloader::{
    DownloadError, ProgressUpdate, SegmentProgressInfo, is_disk_full, is_range_not_satisfiable,
    is_server_rejection,
};
use crate::events::{EngineEvent, EventSink};
use crate::logger::{log_info, log_warn};
use crate::model::SourceBytes;
use crate::output;
use crate::speed_limiter::SpeedLimiter;
use crate::temp_file_guard::TempFileGuard;
use crate::transfer_activity::{TaskRuntime, TaskSegment, TransferGuard, TransferTracker};

// ---------------------------------------------------------------------------
// 就地扩容（BUG-HTTP-HINT-UNDERSIZED）
// ---------------------------------------------------------------------------

/// 多段下载途中，服务器在 206 `Content-Range` 分母里自报的真实总大小大于当前
/// 规划时（[`DownloadError::TrueSizeLarger`]），coordinator【就地扩容】：延长
/// 预分配、追加尾段 Pending、更新共享 planned_total——已下数据零丢弃。本常量是
/// 单次下载内接受的最大扩容次数：防御【仍在渐进上传】的文件持续增长（或病态
/// 服务器无限膨胀分母）导致永不收敛。超限则任务以 TrueSizeLarger 显式失败
/// （status=4）——DB 段行与临时文件【保留】，用户重试时 resume 会重新 probe
/// 真实大小接着下，进度不丢。3 次足以覆盖"上传恰好在下载期间收尾"的常见情形。
const MAX_SIZE_EXPANSIONS: u32 = 3;

// ---------------------------------------------------------------------------
// 域名连接策略缓存（负面上限 + 正面起步提示）
// ---------------------------------------------------------------------------
// 负面面（cap）：当 coordinator 检测到某域名的服务器拒绝多连接（403/429、
// 惩罚性崩塌回滚），把【削减后的连接上限】记入进程级缓存（1 = 单连接）。
// 后续对同域名的下载任务以该值裁剪 worker 上限，避免重蹈覆辙；重复记录取
// 更低值（更保守的观察优先）。
// 正面面（hint）：任务【无任何拒绝信号】成功完成时，把实际维持过的连接
// 规模记入缓存；同域名后续任务以 min(hint, worker_cap) 作为 ramp 起步额度，
// 消除对已证明主机的重复爬坡（2→4→8→…，每次翻倍一个评估窗）。hint 只是
// 起点：ramp 评估/冻结/崩塌回滚照常生效，读取时被未过期 cap 钳制（负面
// 观察恒压制正面提示）。
// 两面各自持时间戳，独立走 24h TTL——服务器策略可能变化，过期后重新学习。
//
// 持久化：缓存经 config 表（key = `domain_conn_caps`）跨重启保留——
// Engine 启动时 [`load_domain_conn_caps`] 读回，记录点经
// [`record_domain_conn_cap_persist`] / [`record_domain_conn_hint_persist`]
// 异步落盘。时间戳用 Unix 秒（墙钟），使 TTL 判定跨进程生命周期依然成立。

/// TTL: 24 小时后允许重新尝试多连接。
const CONN_CAP_TTL: Duration = Duration::from_secs(24 * 3600);

/// 持久化缓存的 config 表 key。
const CONN_CAP_CONFIG_KEY: &str = "domain_conn_caps";

/// 域名连接策略缓存条目——两个正交观察面，各自持时间戳独立走 TTL：
/// - `cap`（负面）：拒绝多连接后学到的连接上限。合并取更低，硬约束。
/// - `hint`（正面）：无拒绝完成任务实际维持过的规模。合并取更高，仅作
///   ramp 起步额度——读取时被未过期 cap 钳制，不构成天花板。
///
/// 0 = 该观察面无记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ConnPolicyEntry {
    cap: i32,
    cap_ts: u64,
    hint: i32,
    hint_ts: u64,
}

/// 进程级的域名 → 连接策略（负面上限 + 正面起步提示）缓存。
static DOMAIN_CONN_CAPS: OnceLock<StdMutex<HashMap<String, ConnPolicyEntry>>> = OnceLock::new();

fn conn_cap_cache() -> &'static StdMutex<HashMap<String, ConnPolicyEntry>> {
    DOMAIN_CONN_CAPS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// 当前 Unix 秒（墙钟早于 epoch 的病态时钟回退为 0，仅影响 TTL 判定的保守性）。
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 时间戳是否仍在 TTL 内。
fn conn_cap_fresh(recorded_secs: u64, now_secs: u64) -> bool {
    now_secs.saturating_sub(recorded_secs) < CONN_CAP_TTL.as_secs()
}

/// 就地清除条目中已过期的观察面；返回条目是否两面皆空（可整体移除）。
fn prune_stale_faces(entry: &mut ConnPolicyEntry, now: u64) -> bool {
    if entry.cap > 0 && !conn_cap_fresh(entry.cap_ts, now) {
        entry.cap = 0;
        entry.cap_ts = 0;
    }
    if entry.hint > 0 && !conn_cap_fresh(entry.hint_ts, now) {
        entry.hint = 0;
        entry.hint_ts = 0;
    }
    entry.cap == 0 && entry.hint == 0
}

/// 提取 URL 的 host 部分（含端口），用于域名级缓存的 key。
pub(crate) fn extract_host(url: &str) -> Option<String> {
    url::Url::parse(url).ok().and_then(|u| {
        u.host_str().map(|h| {
            if let Some(port) = u.port() {
                format!("{}:{}", h, port)
            } else {
                h.to_string()
            }
        })
    })
}

/// 记录某域名学习到的连接上限（1 = 单连接）。与【未过期】的既有记录取更低值
/// 并刷新 TTL；已过期的旧值不参与合并（读取侧视其为不存在，写入侧同样以本次
/// 新鲜观察直接覆盖，两侧语义一致）。仅更新内存；持久化用
/// [`record_domain_conn_cap_persist`]。
pub(crate) fn record_domain_conn_cap(url: &str, cap: i32) {
    let cap = cap.max(1);
    if let Some(host) = extract_host(url)
        && let Ok(mut cache) = conn_cap_cache().lock()
    {
        let now = now_unix_secs();
        let entry = cache.entry(host.clone()).or_default();
        let effective = if entry.cap > 0 && conn_cap_fresh(entry.cap_ts, now) {
            entry.cap.min(cap)
        } else {
            cap
        };
        entry.cap = effective;
        entry.cap_ts = now;
        log_info!(
            "[conn-policy] 记录域名 {} 连接上限 {}，24h 内新任务按此上限调度",
            host,
            effective
        );
    }
}

/// 记录连接上限并异步落盘（记录点常规入口——拒绝事件本就低频，逐次落盘）。
pub(crate) fn record_domain_conn_cap_persist(url: &str, cap: i32, db: &Db) {
    record_domain_conn_cap(url, cap);
    persist_domain_conn_caps(db);
}

/// 记录单连接限制并异步落盘。
pub(crate) fn record_single_conn_domain_persist(url: &str, db: &Db) {
    record_domain_conn_cap_persist(url, 1, db);
}

/// 记录某域名的正面起步提示：一次【无拒绝】完成的任务实际维持过的连接
/// 规模。与【未过期】的既有 hint 取更高值（更大的已证明容量优先）并刷新
/// TTL；已过期的旧值以本次新鲜观察直接覆盖。仅更新内存；持久化用
/// [`record_domain_conn_hint_persist`]。
pub(crate) fn record_domain_conn_hint(url: &str, scale: i32) {
    let scale = scale.max(1);
    if let Some(host) = extract_host(url)
        && let Ok(mut cache) = conn_cap_cache().lock()
    {
        let now = now_unix_secs();
        let entry = cache.entry(host.clone()).or_default();
        let effective = if entry.hint > 0 && conn_cap_fresh(entry.hint_ts, now) {
            entry.hint.max(scale)
        } else {
            scale
        };
        entry.hint = effective;
        entry.hint_ts = now;
        log_info!(
            "[conn-policy] 记录域名 {} 起步提示 {}（无拒绝完成的实测规模），24h 内同域名任务按此起步",
            host,
            effective
        );
    }
}

/// 记录正面起步提示并异步落盘（每任务成功完成至多一次，低频）。
pub(crate) fn record_domain_conn_hint_persist(url: &str, scale: i32, db: &Db) {
    record_domain_conn_hint(url, scale);
    persist_domain_conn_caps(db);
}

/// 查询某域名学习到的连接上限（未记录或已过期 → None）。
pub(crate) fn domain_conn_cap(url: &str) -> Option<i32> {
    read_conn_policy(url).and_then(|e| (e.cap > 0).then_some(e.cap))
}

/// 查询某域名的 ramp 起步提示（正面学习）。未记录/已过期 → None；存在
/// 未过期负面 cap 时钳制到 cap（负面观察恒压制正面提示）。
pub(crate) fn domain_conn_hint(url: &str) -> Option<i32> {
    read_conn_policy(url)
        .and_then(|e| (e.hint > 0).then_some(if e.cap > 0 { e.hint.min(e.cap) } else { e.hint }))
}

/// 读取某域名的策略条目快照：顺带就地清除过期观察面，两面皆空时移除整条。
fn read_conn_policy(url: &str) -> Option<ConnPolicyEntry> {
    let host = extract_host(url)?;
    let mut cache = conn_cap_cache().lock().ok()?;
    let now = now_unix_secs();
    let entry = cache.get_mut(&host)?;
    if prune_stale_faces(entry, now) {
        cache.remove(&host);
        return None;
    }
    Some(*entry)
}

/// 检查某域名是否被限制为单连接（且未过期）。
pub(crate) fn is_single_conn_domain(url: &str) -> bool {
    domain_conn_cap(url) == Some(1)
}

/// 持久化格式版本。语义规则变化（字段含义、TTL 判定、合并策略）时递增——
/// 旧版本数据在加载时整体丢弃并重新学习，绝不跨版本猜测字段含义。
/// 与 aria2 `server-stat-timeout` 的思路一致：学习数据是可再生的性能缓存，
/// 失效的正确处置是丢弃重学，而非迁移。
const CONN_CAP_FORMAT_VERSION: &str = "v3";

/// 序列化缓存为 config 存储格式：首行版本标记，之后每行
/// `host<TAB>cap<TAB>cap_ts<TAB>hint<TAB>hint_ts`（0 = 该观察面无记录）。
/// host 不可能含 TAB/换行（URL host 语法排除空白），无需转义。
fn serialize_conn_caps(map: &HashMap<String, ConnPolicyEntry>) -> String {
    let mut lines: Vec<String> = map
        .iter()
        .map(|(host, e)| format!("{host}\t{}\t{}\t{}\t{}", e.cap, e.cap_ts, e.hint, e.hint_ts))
        .collect();
    lines.sort(); // 确定性输出，便于测试与 diff
    let mut out = String::from(CONN_CAP_FORMAT_VERSION);
    for line in lines {
        out.push('\n');
        out.push_str(&line);
    }
    out
}

/// 解析 config 存储格式。版本标记不匹配（含旧版 v1）→ 返回空表（整体
/// 重学）；版本内的畸形行静默跳过（防御手改/半写入）。
fn parse_conn_caps(raw: &str) -> HashMap<String, ConnPolicyEntry> {
    let mut map = HashMap::new();
    let mut lines = raw.lines();
    if lines.next().map(str::trim) != Some(CONN_CAP_FORMAT_VERSION) {
        return map;
    }
    for line in lines {
        let mut parts = line.split('\t');
        let (Some(host), Some(cap), Some(cap_ts), Some(hint), Some(hint_ts)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        let (Ok(cap), Ok(cap_ts), Ok(hint), Ok(hint_ts)) = (
            cap.parse::<i32>(),
            cap_ts.parse::<u64>(),
            hint.parse::<i32>(),
            hint_ts.parse::<u64>(),
        ) else {
            continue;
        };
        // 每面 0 = 无记录；负值或两面皆空为非法行
        if host.is_empty() || cap < 0 || hint < 0 || (cap == 0 && hint == 0) {
            continue;
        }
        map.insert(
            host.to_string(),
            ConnPolicyEntry {
                cap,
                cap_ts,
                hint,
                hint_ts,
            },
        );
    }
    map
}

/// 统计持久化域名连接策略文本中未过期的条目数（设置页「已学习的服务器
/// 策略」计数；宿主不得复制存储格式）。版本不匹配 / 空串 → 0。
#[must_use]
pub fn count_domain_conn_policies(raw: &str) -> usize {
    let now = now_unix_secs();
    parse_conn_caps(raw)
        .into_values()
        .filter(|entry| {
            let mut entry = *entry;
            !prune_stale_faces(&mut entry, now)
        })
        .count()
}

/// Engine 启动时从 config 表读回持久化的域名连接策略（过期观察面丢弃；
/// 与内存中已有条目按面合并：cap 取更低、hint 取更高，语义分别与
/// [`record_domain_conn_cap`] / [`record_domain_conn_hint`] 一致）。
pub(crate) async fn load_domain_conn_caps(db: &Db) {
    let raw = match db.get_config(CONN_CAP_CONFIG_KEY).await {
        Ok(Some(v)) => v,
        Ok(None) => return,
        Err(e) => {
            log_info!("[conn-policy] 读取持久化域名连接策略失败（忽略）: {}", e);
            return;
        }
    };
    let now = now_unix_secs();
    let loaded = parse_conn_caps(&raw);
    if let Ok(mut cache) = conn_cap_cache().lock() {
        let mut restored = 0usize;
        for (host, mut e) in loaded {
            if prune_stale_faces(&mut e, now) {
                continue;
            }
            let entry = cache.entry(host).or_default();
            // cap 面取更低（保守优先）；内存侧无记录/已过期时直接采纳。
            if e.cap > 0
                && (entry.cap == 0 || !conn_cap_fresh(entry.cap_ts, now) || e.cap < entry.cap)
            {
                entry.cap = e.cap;
                entry.cap_ts = e.cap_ts;
            }
            // hint 面取更高（已证明容量优先）；同上直接采纳规则。
            if e.hint > 0
                && (entry.hint == 0 || !conn_cap_fresh(entry.hint_ts, now) || e.hint > entry.hint)
            {
                entry.hint = e.hint;
                entry.hint_ts = e.hint_ts;
            }
            restored += 1;
        }
        if restored > 0 {
            log_info!("[conn-policy] 已恢复 {} 条持久化域名连接策略", restored);
        }
    }
}

/// 把当前缓存快照异步写回 config 表（fire-and-forget；过期条目顺带清理）。
fn persist_domain_conn_caps(db: &Db) {
    let snapshot = {
        let Ok(mut cache) = conn_cap_cache().lock() else {
            return;
        };
        let now = now_unix_secs();
        cache.retain(|_, e| !prune_stale_faces(e, now));
        serialize_conn_caps(&cache)
    };
    // 同步调用方（如 set_proxy_config）可能不在 runtime 内（纯单元测试），
    // 此时静默跳过——持久化是尽力而为的缓存，丢一次写不影响正确性。
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let db = db.clone();
    handle.spawn(async move {
        if let Err(e) = db.set_config(CONN_CAP_CONFIG_KEY, &snapshot).await {
            log_info!("[conn-policy] 持久化域名连接上限失败（忽略）: {}", e);
        }
    });
}

/// 清空域名连接上限缓存（内存 + 持久化）。
///
/// 网络环境变化时调用（代理配置切换）：连接上限是服务器**对某个客户端出口**
/// 的策略观察——换代理/出口 IP 后旧观察不再可信，既可能过严（新出口本可
/// 多连接）也可能过宽（新出口更受限），丢弃重学是唯一无偏的处置。
pub(crate) fn clear_domain_conn_caps(db: &Db) {
    if let Ok(mut cache) = conn_cap_cache().lock() {
        if cache.is_empty() {
            return;
        }
        log_info!(
            "[conn-policy] 网络环境变化，清空 {} 条域名连接上限观察",
            cache.len()
        );
        cache.clear();
    }
    persist_domain_conn_caps(db);
}

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// 最小拆分阈值的默认/上限值（高速连接 >10 MB/s 时使用）。
/// 低速连接通过 [`dynamic_min_split_bytes`] 自适应降低此阈值。
const MIN_SPLIT_BYTES: i64 = 2 * 1024 * 1024; // 2 MB

/// 尾部相对阈值分母：`remaining < total / 该值` 视为“接近末尾”。50 = 末尾 2%。
/// 与绝对阈值 `worker_cap × MIN_SPLIT_BYTES` 以 AND 组合构成 [`is_tail`] 判据。
/// 初值待真实大文件下载校验：偏早（过度冻结）调大，偏晚（仍见骤降）调小。
const TAIL_FRACTION_DENOM: i64 = 50;

/// 根据当前实时吞吐量动态计算最小拆分阈值。
///
/// - 低速（< 1 MB/s）：512 KB — 更积极拆分，空闲 worker 可快速参与慢段
/// - 中速（1–10 MB/s）：1 MB — 平衡 HTTP 请求开销与并行收益
/// - 高速（> 10 MB/s）：2 MB — TLS 1.3 握手占比 <1%，保持默认
fn dynamic_min_split_bytes(throughput_bps: f64) -> i64 {
    const BW_LOW: f64 = 1.0 * 1024.0 * 1024.0; //  1 MB/s
    const BW_HIGH: f64 = 10.0 * 1024.0 * 1024.0; // 10 MB/s
    if throughput_bps < BW_LOW {
        512 * 1024 // 512 KB
    } else if throughput_bps < BW_HIGH {
        1024 * 1024 // 1 MB
    } else {
        MIN_SPLIT_BYTES // 2 MB
    }
}

/// Maximum number of concurrently active/pending segments (= connections) per
/// task. 用户显式线程数的硬上限；与 `fluxdown_protocol::MAX_TASK_SEGMENTS` 一致。
/// Auto 模式不受此值放大：advisor 与 [`HINT_UNCAP_MAX`] 仍各自封顶 64。
pub(crate) const MAX_SEGMENTS: i32 = 512;

// ---------------------------------------------------------------------------
// 渐进启动 / 自适应连接调度
// ---------------------------------------------------------------------------
// `initial_segment_count` 的语义是【最大连接数上限】而非启动并发：coordinator
// 从 RAMP_INITIAL_WORKERS 条连接起步，每个评估窗口测一次总吞吐，健康（稳定
// 206、无 403/429、吞吐仍有增益）则倍增额度直至上限；扩容后吞吐无增益则冻结
// 在当前规模（找到服务器/链路的实际并发甜点）。分段规划不变——连接数 =
// 存活 worker 数，与预切分段数解耦（段只是 worker 的工作队列）。

/// 启动时的初始并发连接数（ramp-up 起点）。4 条与浏览器对同主机的常规并发
/// 同量级（Chrome 6 条/主机），不会单凭起步触发反多线程风控；从 2 起步要多
/// 一个 2s 评估窗才到 8 条，小文件 / 高 RTT 链路上大半时间都花在爬坡上
/// （实测 GitHub 9.6MB：GD3 起步 8 条 7.5s，FD 起步 2 条 11.3s）。
const RAMP_INITIAL_WORKERS: usize = 4;

/// ramp 评估窗口间隔（秒）：每窗口测一次总吞吐，决定扩容/冻结。
const RAMP_TICK_SECS: u64 = 2;

/// open-ended 首响应即时放队后的最短完整评估窗（毫秒）。
/// 短于此时长的 tick 只做槽位对账 + 按额度补队，跳过吞吐采样/评估/shrink/
/// 扩容，且不推进 `ramp_last_*` 窗口边界——保证下个整窗测速干净。
const RAMP_MIN_EVAL_WINDOW_MS: u128 = 500;

/// 软冻结冷却：Freeze 后等待此数量的完整 ramp tick（× [`RAMP_TICK_SECS`]）
/// 再发起 +1 试探。15 × 2s ≈ 30s，足以让瞬时噪声谷过去，又不会把一次
/// 误冻结钉死到任务结束。
const RAMP_SOFT_PROBE_COOLDOWN_TICKS: u32 = 15;

/// 单次 Soft 生命周期内最多 +1 试探次数。两次失败即升 Hard——持续无增益
/// 说明当前规模已是甜点，再探只会空烧连接建立开销。
const RAMP_SOFT_PROBE_MAX_ATTEMPTS: u32 = 2;

/// 整任务累计软试探次数上限（跨多次进 Soft）。6 次 × 平均冷却覆盖约数分钟
/// 量级的再探预算；超额后 Freeze 直接进 Hard，防止长任务反复软解冻抖动。
const RAMP_SOFT_PROBE_TASK_BUDGET: u32 = 6;

/// hint 解封天花板：在无负面域名 cap、且存在正面 hint 时，允许把 `worker_cap`
/// 抬到 `hint × 2`（下一档探索空间），但仍受此平台上限与 [`MAX_SEGMENTS`] 钳制。
/// 移动端连接/电量更敏感，上限减半；桌面端封顶 64（自动探索不进入高风险区间）。
/// 证据门槛：仅当调用方显式允许（用户 Auto 默认档）且无未过期负面 cap 时生效；
/// ramp 的 IMPROVE/Collapse/reject 反馈闭环全额保留，解封只放宽天花板不跳过评估。
#[cfg(any(target_os = "android", target_os = "ios"))]
const HINT_UNCAP_MAX: usize = 32;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const HINT_UNCAP_MAX: usize = 64;

/// Range 能力裁决（`range_verdict: AtomicU8` 的取值）。
/// hint 模式（probe 被跳过、Range 从未验证）从 UNKNOWN 起步：首连接发不带
/// Range 的 plain GET，从首响应（206 或 `Accept-Ranges: bytes`）学习；裁决前
/// 绝不发出任何带 Range 的请求——配额型端点（fnOS multiple-download）对
/// bounded Range 一律 400 且【作废 token】（实测），一次 Range 风暴 = 任务判死。
/// probe/resume 路径直接以 SUPPORTED 起步（行为与既往完全一致）。
const RANGE_VERDICT_UNKNOWN: u8 = 0;
const RANGE_VERDICT_SUPPORTED: u8 = 1;
const RANGE_VERDICT_UNSUPPORTED: u8 = 2;

/// 「全员被拒 + 瞬时 200 家族」归一为 RangeNotSupported（触发清盘单流回退）
/// 时允许丢弃的已下字节上限。低于此值：一次性下完的收益 > 重下代价；高于
/// 此值：保留 fail-loud + 数据，用户 resume 重新 probe 后仍可多段续传。
const RANGE_FAMILY_DISCARD_MAX: i64 = 8 * 1024 * 1024;

/// 扩容有效判据：扩容后窗口吞吐 >= 扩容前吞吐 × 此系数才继续扩容，
/// 否则冻结（新增连接没有带来净收益，继续加只会触发服务器风控）。
const RAMP_IMPROVE_FACTOR: f64 = 1.05;

/// 扩容崩塌判据：扩容后窗口吞吐 < 扩容前吞吐 × 此系数，说明服务器对新增
/// 连接施加了惩罚性限速（按连接均分带宽并压低单连接速率）。此时仅冻结是
/// 不够的——被惩罚的规模会一直跑到任务结束。处置：回滚到扩容前规模（多余
/// worker 完成当前段后自然退休，进行中的连接绝不取消），并把回滚后的规模
/// 记入域名连接上限缓存，同域名后续任务直接以该值起步。
/// 0.5 的余量足够宽：正常网络抖动极少让 2s 窗口吞吐腰斩，而惩罚型限速的
/// 跌幅通常是数量级级别（实测 59 MB/s -> 2 MB/s）。
const RAMP_COLLAPSE_FACTOR: f64 = 0.5;

/// 冻结/到顶后的持续劣化收缩：以任务内观察到的峰值窗口吞吐为参照，连续
/// 此数量的评估窗口吞吐 < 峰值 × [`RAMP_COLLAPSE_FACTOR`] 时，把连接额度
/// 乘性减半（下限 1）。覆盖「起步规模就已触发服务器惩罚」的场景——此时
/// 从未发生过扩容评估，崩塌回滚判据不可达。收缩只在额度被用满
/// （alive >= allowed）时计数，下载尾声 worker 自然退休导致的吞吐下降
/// 不会误触发。
const RAMP_SHRINK_STRIKES: u32 = 2;

/// 持续劣化收缩的验证窗口数：收缩后这么多个完整窗口内，窗口吞吐须有一次超过
/// 收缩时吞吐 × [`RAMP_IMPROVE_FACTOR`]，否则判定劣化来自链路噪声（代理抖动、
/// 共享带宽）而非服务器对连接数的惩罚——撤销收缩并在本任务内停用该机制。
const RAMP_SHRINK_EVAL_WINDOWS: u32 = 2;

/// 收缩验证结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShrinkEval {
    /// 证据不足，继续观察。
    Pending,
    /// 收缩后吞吐回升：确是连接数惩罚，保留收缩。
    Confirmed,
    /// 收缩没有带来回升：撤销收缩。
    Revert,
}

/// 收缩验证判据：`shrunk_bps` 为触发收缩那一窗的吞吐，`best_bps` 为收缩后各
/// 完整窗口的最高吞吐，`windows` 为已观察的完整窗口数。
fn shrink_eval_verdict(shrunk_bps: f64, best_bps: f64, windows: u32) -> ShrinkEval {
    if best_bps > shrunk_bps * RAMP_IMPROVE_FACTOR {
        ShrinkEval::Confirmed
    } else if windows >= RAMP_SHRINK_EVAL_WINDOWS {
        ShrinkEval::Revert
    } else {
        ShrinkEval::Pending
    }
}

/// 峰值吞吐参照的每窗口衰减系数：约 70 个窗口（2 分钟出头）半衰。
/// 让持续劣化收缩的参照系跟随近期真实速度，而非被启动瞬间的突发窗口
/// （页缓存命中、链路 burst）永久钉死；对 2 窗口（4s）内完成的惩罚检测
/// 影响 < 2%。
const RAMP_PEAK_DECAY: f64 = 0.99;

/// 扩容评估结论：扩容后的窗口吞吐相对扩容前的三档判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RampVerdict {
    /// 有净增益（> 扩容前 × [`RAMP_IMPROVE_FACTOR`]），可继续扩容。
    Improved,
    /// 无净增益，冻结在当前规模。
    Freeze,
    /// 崩塌（< 扩容前 × [`RAMP_COLLAPSE_FACTOR`]），回滚到扩容前规模。
    Collapse,
}

/// ramp 冻结状态：Active 允许正常倍增扩容；Soft 冷却后可 +1 试探；Hard 永久停扩。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FreezeState {
    /// 未冻结，可走正常倍增扩容路径。
    Active,
    /// 扩容无增益后的软冻结：冷却 `cooldown` 个完整 tick 后可发起有限次 +1 试探。
    Soft {
        ticks_waited: u32,
        cooldown: u32,
        attempts: u32,
    },
    /// 硬冻结：reject / conn_sensitive / Collapse / 试探预算耗尽——本任务不再扩容。
    Hard,
}

/// 三档判定本体。边界语义：吞吐前后同为 0（服务器停滞）落入 Freeze——
/// 绝不因"没有变差"而对停滞服务器继续打满并发；恰在崩塌线上（等于
/// 扩容前 × 崩塌系数）判 Freeze 而非 Collapse，回滚只留给明确的惩罚信号。
fn ramp_verdict(pre_grow_bps: f64, current_bps: f64) -> RampVerdict {
    if current_bps > pre_grow_bps * RAMP_IMPROVE_FACTOR {
        RampVerdict::Improved
    } else if current_bps < pre_grow_bps * RAMP_COLLAPSE_FACTOR {
        RampVerdict::Collapse
    } else {
        RampVerdict::Freeze
    }
}

/// 尾部判据：剩余量既不足以给每个目标连接一个整段（工作饥饿），又已接近文件
/// 末尾。两条件 **AND** 缺一不可：
/// - 仅绝对项会让小/中文件（total < `worker_cap` × [`MIN_SPLIT_BYTES`]）从第 0
///   字节起恒判尾部，使其真实惩罚检测整体失效；相对项排除此误判。
/// - 仅相对项在巨型文件上会远未工作饥饿就判尾部，过早冻结；绝对项收窄之。
///
/// 进入尾部后 coordinator 冻结连接额度、短路自适应收缩/学习（见 ramp tick），
/// 保持当前连接数持续下载剩余字节，消除下载尾段的速度骤降。
fn is_tail(remaining: i64, worker_cap: usize, total: i64) -> bool {
    remaining < worker_cap as i64 * MIN_SPLIT_BYTES && remaining < total / TAIL_FRACTION_DENOM
}

/// 峰值型持续劣化收缩判据：本窗口是否应计一次收缩 strike。
///
/// 仅在【非扩容评估期】（`!awaiting_eval`）、已冻结或额度到顶、额度 >1、存活数
/// 恰等于额度、且吞吐跌破峰值 × [`RAMP_COLLAPSE_FACTOR`] 时成立。尾部由调用点的
/// `!tail` 短路，不在此函数内判定。
fn should_shrink(
    throughput: f64,
    peak: f64,
    allowed: usize,
    cap: usize,
    alive: usize,
    frozen: bool,
    awaiting_eval: bool,
) -> bool {
    !awaiting_eval
        && (frozen || allowed >= cap)
        && allowed > 1
        && alive == allowed
        && peak > 0.0
        && throughput < peak * RAMP_COLLAPSE_FACTOR
}

/// 扩容门：未冻结、无连接敏感信号、额度已用满、未达上限，且【本窗口未发生收缩】。
/// `shrunk_this_tick` 根治同窗口 shrink→re-expand 震荡。
fn should_expand(
    frozen: bool,
    conn_sensitive: bool,
    alive: usize,
    allowed: usize,
    cap: usize,
    shrunk_this_tick: bool,
) -> bool {
    !frozen && !conn_sensitive && alive >= allowed && allowed < cap && !shrunk_this_tick
}

/// ramp 起步额度：无正面提示 → 保守的 [`RAMP_INITIAL_WORKERS`]；命中同域名
/// 正面提示（此前任务无拒绝维持过的规模）→ 直接以该规模起步（仍受
/// `worker_cap` 约束）。提示只抬起点，不改变 ramp 的评估/扩容/回滚语义。
fn initial_allowed_workers(worker_cap: usize, hint: Option<i32>) -> usize {
    let base = match hint {
        Some(h) if h as usize > RAMP_INITIAL_WORKERS => h as usize,
        _ => RAMP_INITIAL_WORKERS,
    };
    worker_cap.min(base).max(1)
}

/// hint 解封天花板：在允许解封、无未过期负面 cap、且存在正面 hint 时，
/// 把 `base_cap` 抬到 `hint × 2`（已证实规模的下一档探索空间），并钳在
/// [`HINT_UNCAP_MAX`] 与 [`MAX_SEGMENTS`] 内。
///
/// - `allow == false`（用户显式段数 / 非默认 auto_max）→ 原样返回 `base_cap`。
/// - 负面记录存在时绝不放宽（含 `cap == Some(1)` 的单连接域名）。
/// - ramp 的 IMPROVE / Collapse / reject 反馈闭环全额保留；本函数只抬天花板。
fn hint_uncap_ceiling(
    allow: bool,
    negative_cap: Option<i32>,
    hint: Option<i32>,
    base_cap: usize,
) -> usize {
    if !allow || negative_cap.is_some() {
        return base_cap;
    }
    let Some(h) = hint else {
        return base_cap;
    };
    if h <= 0 {
        return base_cap;
    }
    let target = (h as usize)
        .saturating_mul(2)
        .min(HINT_UNCAP_MAX)
        .min(MAX_SEGMENTS as usize);
    if target > base_cap { target } else { base_cap }
}

/// 任务内解封的线性度门槛：一次扩容让吞吐增幅 ≥ 连接增幅 × 该值，才认定瓶颈
/// 是单连接速率（与 Ghost Downloader 自动提速的 0.8 判据同口径）。
const RAMP_UNCAP_LINEARITY: f64 = 0.8;

/// 任务内解封：额度刚升到天花板 `cap`，而这次 `pre_workers → workers` 的扩容
/// 带来接近线性的增益——按连接限速的 CDN / 远程代理链路上，再多的连接仍在兑现
/// 带宽，把天花板翻倍继续爬升（同 hint 解封钳在 [`HINT_UNCAP_MAX`]）。带宽已饱和
/// 时增益远低于线性，天花板不动。剩余字节不足以喂饱翻倍后的连接（每条至少
/// 2 × [`MIN_SPLIT_BYTES`]）时不解封。允许条件与 [`hint_uncap_ceiling`] 相同。
#[allow(clippy::too_many_arguments)]
fn in_task_uncap_ceiling(
    allow: bool,
    negative_cap: Option<i32>,
    cap: usize,
    pre_workers: usize,
    workers: usize,
    pre_bps: f64,
    bps: f64,
    remaining: i64,
) -> usize {
    if !allow || negative_cap.is_some() || workers < cap || pre_workers == 0 {
        return cap;
    }
    if workers <= pre_workers || !(pre_bps > 0.0 && bps.is_finite()) {
        return cap;
    }
    let conn_gain = workers as f64 / pre_workers as f64 - 1.0;
    let rate_gain = bps / pre_bps - 1.0;
    if rate_gain < conn_gain * RAMP_UNCAP_LINEARITY {
        return cap;
    }
    let target = cap
        .saturating_mul(2)
        .min(HINT_UNCAP_MAX)
        .min(MAX_SEGMENTS as usize);
    let fed = target as i64 * 2 * MIN_SPLIT_BYTES;
    if target > cap && remaining >= fed {
        target
    } else {
        cap
    }
}

/// 软冻结 +1 试探是否就绪（完整 tick 守卫，不含 awaiting 消费）。
/// 尾部 / 串行 / 连接敏感 / 拒绝 / 额度未用满 / 已到顶 / 任务试探预算耗尽
/// 任一成立即否——结构上保证 tail 短路试探。
#[allow(clippy::too_many_arguments)]
fn soft_probe_ready(
    freeze: FreezeState,
    tail: bool,
    range_ok: bool,
    serial_mode: bool,
    conn_sensitive: bool,
    reject_strikes: u32,
    alive: usize,
    allowed: usize,
    worker_cap: usize,
    task_probes_used: u32,
) -> bool {
    let FreezeState::Soft {
        ticks_waited,
        cooldown,
        attempts,
    } = freeze
    else {
        return false;
    };
    ticks_waited >= cooldown
        && attempts < RAMP_SOFT_PROBE_MAX_ATTEMPTS
        && !tail
        && range_ok
        && !serial_mode
        && !conn_sensitive
        && reject_strikes == 0
        && alive >= allowed
        && allowed < worker_cap
        && task_probes_used < RAMP_SOFT_PROBE_TASK_BUDGET
}

/// 试探窗评估转移：返回 `(下一冻结态, 是否回滚到 pre_grow)`。
///
/// **红线**：本函数不含任何域名缓存写入分支——试探窗的 Freeze/Collapse
/// 均不记 `record_domain_conn_cap`；写缓存只存在于非试探 Collapse 臂。
fn soft_probe_eval_transition(
    verdict: RampVerdict,
    attempts: u32,
    cooldown: u32,
) -> (FreezeState, bool) {
    match verdict {
        RampVerdict::Improved => (FreezeState::Active, false),
        RampVerdict::Freeze => {
            let next_attempts = attempts.saturating_add(1);
            if next_attempts >= RAMP_SOFT_PROBE_MAX_ATTEMPTS {
                (FreezeState::Hard, true)
            } else {
                (
                    FreezeState::Soft {
                        ticks_waited: 0,
                        cooldown: cooldown.saturating_mul(2),
                        attempts: next_attempts,
                    },
                    true,
                )
            }
        }
        // 试探窗崩塌 ≠ 真 Collapse：回滚 + Hard，绝不写域名缓存。
        RampVerdict::Collapse => (FreezeState::Hard, true),
    }
}

/// 正常扩容路径收到 Freeze 裁决后的下一状态。
/// 任务软试探预算已耗尽时直接 Hard，否则进入初始 Soft 冷却。
fn freeze_verdict_next_state(task_probes_used: u32) -> FreezeState {
    if task_probes_used >= RAMP_SOFT_PROBE_TASK_BUDGET {
        FreezeState::Hard
    } else {
        FreezeState::Soft {
            ticks_waited: 0,
            cooldown: RAMP_SOFT_PROBE_COOLDOWN_TICKS,
            attempts: 0,
        }
    }
}
/// 持续劣化收缩后的冻结状态。Active 至少进入 Soft 冷却，阻止下一完整 tick
/// 立即倍增回原额度；已经处于 Soft/Hard 时说明冷却或试探期间仍在劣化，升级 Hard。
fn sustained_shrink_next_state(current: FreezeState, task_probes_used: u32) -> FreezeState {
    if matches!(current, FreezeState::Active) {
        freeze_verdict_next_state(task_probes_used)
    } else {
        FreezeState::Hard
    }
}

/// 任务尾正面学习规模：取「被接受过的规模」与「受益过的规模」之较小者，
/// 防止 Freeze 不回滚场景下跨任务棘轮抬升 hint。
fn beneficial_hint_scale(proven_scale: usize, beneficial_scale: usize) -> usize {
    proven_scale.min(beneficial_scale)
}

/// 尾部微拆分阈值：当正常拆分（`dynamic_min_split_bytes` 计算的阈值）失败时，
/// 用此极低阈值重试，避免下载尾部空闲 worker 干等最后一个慢段。
///
/// 64 KB 的段在 1 MB/s 连接上只需 64ms，TLS 1.3 握手开销（1 RTT ≈ 30ms）
/// 占比约 32%，仍有净收益。低于此值则 HTTP 请求开销反超下载本身。
///
/// 此设计是 fast-down 投机执行（Speculative Execution）的实用替代方案。
/// fast-down 用 AtomicU128 CAS 让多个 worker 竞争同一段的字节范围（零额外
/// HTTP 请求），但需要重构整个写入路径为 CAS-guarded（放弃 BufWriter、修改
/// 进度报告/DB 持久化）。尾部微拆分在 FluxDown 架构下以极小改动覆盖了 90%+
/// 的尾延迟场景：段 remaining ≥ 128KB 时拆成两半各 ≥64KB，两个 worker 各发
/// 独立 Range 请求并行完成。
///
/// **瀑布防护**：尾部微拆分仅在最大剩余分段 ≥ 2 × TAIL_MIN_SPLIT_BYTES（即
/// 128KB）时才激活，确保只救援真正的"落后分段"而非均等小分段。若所有分段
/// 剩余量均接近 TAIL_MIN_SPLIT_BYTES（例如下载最后 1% 时大量分段均为 ~66KB），
/// 继续拆分只会产生更多 HTTP 请求开销并导致 worker 集体退出——活跃 worker 数
/// 从 ~48 骤降至 ~16，引发 UI 速度指示器的"99% 速度下降"现象。
const TAIL_MIN_SPLIT_BYTES: i64 = 64 * 1024; // 64 KB

/// Proactive split 定时器间隔（秒）。
///
/// 定时预拆分最大 Active 段为 Pending，使下一个完成的 worker 无需在 Done
/// 处理的关键路径上计算拆分 + DB 持久化，直接从 Pending 队列取任务。
const PROACTIVE_SPLIT_INTERVAL_SECS: u64 = 2;

/// 默认 BufWriter 容量（低速/小段场景）。
const BUF_WRITER_CAPACITY_SMALL: usize = 256 * 1024; // 256 KB
/// 中等段（4-32 MB）使用 512 KB 缓冲区，减少系统调用频率。
const BUF_WRITER_CAPACITY_MEDIUM: usize = 512 * 1024; // 512 KB
/// 大段（>32 MB）使用 1 MB 缓冲区，充分利用高速连接。
const BUF_WRITER_CAPACITY_LARGE: usize = 1024 * 1024; // 1 MB

/// 根据段剩余字节数动态选择 BufWriter 容量。
/// 大段使用更大的缓冲区以减少 write 系统调用频率；
/// 小段使用较小缓冲区避免内存浪费。
fn buf_writer_capacity_for_segment(remaining_bytes: i64) -> usize {
    const THRESHOLD_LARGE: i64 = 32 * 1024 * 1024; // 32 MB
    const THRESHOLD_MEDIUM: i64 = 4 * 1024 * 1024; //  4 MB
    if remaining_bytes >= THRESHOLD_LARGE {
        BUF_WRITER_CAPACITY_LARGE
    } else if remaining_bytes >= THRESHOLD_MEDIUM {
        BUF_WRITER_CAPACITY_MEDIUM
    } else {
        BUF_WRITER_CAPACITY_SMALL
    }
}

/// Return type for `build_fresh_segments`: (in-memory map, DB tuples).
type FreshSegments = (BTreeMap<i32, LiveSegment>, Vec<(i32, i64, i64)>);

/// DB save interval — matches downloader.rs.
const DB_SAVE_INTERVAL_SECS: u64 = 3;

/// Progress report interval to the client UI.
const UI_REPORT_INTERVAL_MS: u128 = 200;

/// Retry constants for segment downloads.
///
/// 大文件下载（>1GB）最多 32 个分段并发，每个分段独立受 stall 检测。
/// 网络抖动时任何一个分段重试耗尽都会导致整个任务失败。
/// 5 次重试（含指数退避：2s/4s/8s/16s）给予充足的恢复窗口，
/// 总容忍时间从 ~36s 提升到 ~80s，大幅降低大文件下载因瞬时网络问题而中断的概率。
const MAX_RETRIES: u32 = 5;
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// 一次失败的尝试至少落盘这么多字节，才算「有实质进展」而重置段内重试预算。
const RETRY_PROGRESS_RESET_BYTES: i64 = 256 * 1024;

/// 段内瞬时失败总次数上限 = 重试预算 × 该倍数（进展重置预算时也不清零），
/// 防止涓流服务器让单段无限重连。
const RETRY_TOTAL_CEILING_FACTOR: u32 = 4;

/// 多 CDN 钉定节点租约的段内重试预算（收紧版 MAX_RETRIES）。
///
/// SYS 单节点时段内重试是唯一自愈手段，值得 5 次 × 指数退避（~80s）的耐心；
/// 钉定节点死亡时同样耐心意味着 ~30s×3 次才被踢——架空快速故障切换
///（#127 主诉求之一）。钉定租约上抛廉价：coordinator 立即回收段重派，
/// 下一次租借自然落到健康节点，故只留一次带短退避的就地重试（吸收单次
/// 网络抖动），失败即上抛交给节点池切换。
const PINNED_NODE_MAX_RETRIES: u32 = 2;

/// 限速器饱和判据：本窗口吞吐至少达到适用限速的该比例才算被限速器钳住。
/// 令牌耗尽的偶发等待（刚改限速、尾部微段）不足以说明样本受限速器支配。
const LIMITER_SATURATION_FLOOR: f64 = 0.5;

/// 本窗口是否 app-limited（BBR 语义）：限速已启用、窗口内限速器确实让连接
/// 等过令牌（`throttle_waits_delta > 0`），且任务吞吐已贴近限速值。直连比
/// 限速值还慢时令牌恒有余量、不会等待，连接受链路而非限速器约束，此时仍应
/// 采样并探索代理。
fn window_app_limited(limit_bps: u64, throttle_waits_delta: u64, throughput_bps: f64) -> bool {
    limit_bps > 0
        && throttle_waits_delta > 0
        && throughput_bps >= LIMITER_SATURATION_FLOOR * limit_bps as f64
}

/// SYS 租约是否具备「传输失败快速放手」资格：Auto 多路径下存在与 SYS 不同的
/// 存活备选路径、改道配额未耗尽（`sys_fallback_available`），且该段不是
/// 开放式首段 / plain GET 生命线（配额型端点只允许留在起飞路径、重连可能
/// 被拒，永远保持完整预算）。
fn sys_fast_release(sys_fallback_available: bool, open_ended: bool, no_range: bool) -> bool {
    sys_fallback_available && !open_ended && !no_range
}

/// 一次段内失败所适用的重试预算。
///
/// - 非 SYS 租约（钉定节点 / 备选路径）：恒 [`PINNED_NODE_MAX_RETRIES`]，失败
///   快速上抛交给节点池切换；
/// - SYS 租约默认 [`MAX_RETRIES`]（单路径时段内重试是唯一自愈手段）。具备
///   快放手资格（`fast_release`）且本次是传输层失败时收紧为
///   [`PINNED_NODE_MAX_RETRIES`]——节点池会改道接手，坐等 ~80s 退避只会拖死
///   已坏的路径。HTTP 类瞬时错误（503 等）换路修不好，保持原预算，不因
///   Auto 降低容错。
fn segment_retry_budget(
    attributable_lease: bool,
    fast_release: bool,
    transport_error: bool,
) -> u32 {
    if attributable_lease || (fast_release && transport_error) {
        PINNED_NODE_MAX_RETRIES
    } else {
        MAX_RETRIES
    }
}

/// 单段瞬时失败隔离次数上限：段内重试耗尽后、其它连接仍健康时，同一段最多
/// 回队列重派这么多次；再失败按原语义上抛（整任务失败交由自动重试）。
const SEGMENT_TRANSIENT_REQUEUE_MAX: u32 = 3;

/// 全员 429 后单连接复活的次数上限（每次退避 = 基数 × 第几次：5s / 10s / 15s）。
const ALL_REJECTED_REVIVE_MAX: u32 = 3;
const ALL_REJECTED_REVIVE_BASE_DELAY: Duration = Duration::from_secs(5);

/// 单个 chunk 的读取超时（stall detection）。如果超过此时间没有收到任何数据，
/// 视为连接停滞，返回错误触发 retry 机制（断开旧连接，用 Range 请求从断点续传）。
/// 5 秒足够容忍正常的 CDN 抖动，又能快速从真正卡死的连接中恢复。
/// 这解决了大文件下载到 98%+ 时 TCP 连接卡死、速度趋近 0 的问题。
const CHUNK_STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// 配额型端点（发生过开放式首段吸收合并 → 新连接必被拒，重连 = 判死任务）的
/// 停滞容忍。对这类唯一存活流，掐流的代价是无限大，宁可多等也不轻易断开。
/// 取值对齐 aria2 的读超时默认值（`--timeout`，OptionHandlerFactory.cc 定义
/// 60s；aria2 甚至默认禁用低速断开 `--lowest-speed-limit=0`）。普通服务器仍用
/// 上方 5s 快速回收死连接——重连廉价，且我们的尾段抢救依赖快速回收。
const CHUNK_STALL_TIMEOUT_HOSTILE: Duration = Duration::from_secs(60);

/// fdatasync 合并闸的最小间隔：全局每 2s 至多触发一次整盘 fdatasync。
///
/// 取值略小于 [`DB_SAVE_INTERVAL_SECS`]（3s），确保每个落库周期内仍有一次新鲜
/// fsync 覆盖，同时把 64 段各自每 3s 的冗余整盘 fsync 合并为全局约每 2–3s 一次。
const MIN_SYNC_GAP: Duration = Duration::from_secs(2);

/// 全局（单文件级）fdatasync 合并闸。
///
/// `fdatasync(fd)` 刷的是【整个文件 inode】的脏页，与调用它的具体 fd 无关。多段
/// 下载中每个 worker 各持一个指向同一文件的 fd，若各自周期性 fsync，则文件会被
/// 重复整盘刷写（N 段 → 每周期 N 次）。本闸把并发的 fsync 请求合并为全局每
/// [`MIN_SYNC_GAP`] 至多一次。
///
/// # 正确性契约
///
/// 闸记录最近一次【已完成】fdatasync 的**起始时刻** `S`。fdatasync 保证刷入所有
/// 在其【起始】前发起的写入，故凡在 `S` 之前完成的写入（其字节此刻已在 OS 页
/// 缓存）此刻均已持久化到磁盘。调用方须先 `file.flush()` 把自己的 BufWriter 落入
/// 页缓存、记下快照时刻 `snap_t`，**仅当** `S >= snap_t` 时才把该快照偏移写入
/// DB，从而维持 "DB 偏移 <= 已持久化字节" 不变式（BUG-COORD-FSYNC）。因判据用
/// fsync 的【起始】而非完成时刻，即便复用他段触发的 fsync，也绝不会信任一次早于
/// 自身 flush 的 fsync（Windows 共享文件缓存同理）。
///
/// 周期落库走 [`FileSyncGate::kick_background`]：fdatasync 在阻塞线程池里跑，
/// worker 不等它、继续读 socket，下个落库周期再按 `S` 提交已被覆盖的快照。
/// macOS 上 `sync_data` 是 `F_FULLFSYNC`（实测 256MB 脏页 242ms），旧做法在读
/// 循环里同步等待，每 3s 让所有连接停读一次。段完成用 [`FileSyncGate::sync_covering`]
/// 同步等待覆盖，二者共享同一份状态与合并判据。
#[derive(Clone)]
struct FileSyncGate {
    inner: Arc<StdMutex<GateState>>,
    notify: Arc<Notify>,
}

struct GateState {
    /// 是否有一次 fdatasync 正在进行（串行化并发请求，避免重复整盘刷）。
    syncing: bool,
    /// 最近一次【已完成】fdatasync 的起始时刻；None 表示尚无任何 fsync。
    last_completed_start: Option<Instant>,
    /// 后台 fdatasync 失败（磁盘 I/O 错误、空间耗尽）：不会自愈，所有 worker
    /// 在下个落库周期据此上抛。
    failure: Option<(std::io::ErrorKind, String)>,
}

impl FileSyncGate {
    fn new() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(GateState {
                syncing: false,
                last_completed_start: None,
                failure: None,
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    /// 最近一次已完成 fdatasync 的起始时刻。
    fn last_completed_start(&self) -> Option<Instant> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_completed_start
    }

    /// 后台 fdatasync 的失败（若有）。
    fn failure(&self) -> Option<std::io::Error> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .failure
            .as_ref()
            .map(|(kind, message)| std::io::Error::new(*kind, message.clone()))
    }

    /// 非阻塞合并式 fdatasync：无在途 fsync、且距上次已完成 fsync 起始已满
    /// [`MIN_SYNC_GAP`]（或从未 fsync）时，在阻塞线程池发起一次整盘 fdatasync；
    /// 否则什么都不做。立即返回，结果经 [`Self::last_completed_start`] /
    /// [`Self::failure`] 观察。`file` 是同一文件的独立句柄（fdatasync 作用于 inode）。
    fn kick_background(&self, file: &Arc<std::fs::File>) {
        {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if st.syncing
                || st
                    .last_completed_start
                    .is_some_and(|s| s.elapsed() < MIN_SYNC_GAP)
            {
                return;
            }
            st.syncing = true;
        }
        let gate = self.clone();
        let file = Arc::clone(file);
        tokio::task::spawn_blocking(move || {
            // 起始时刻记于 fdatasync 之前：它是「覆盖判据」的时刻锚点。
            let start = Instant::now();
            // panic 也必须清掉 `syncing`，否则 sync_covering 会永远等这次 fsync。
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| file.sync_data()))
                .unwrap_or_else(|_| Err(std::io::Error::other("background fdatasync panicked")));
            // 先释放复制句柄再通知：Windows 上它与写入句柄共享同一文件对象，最后一个
            // 句柄关闭时 NTFS 会把修改时间刷成当前时间。若晚于完成期 apply_server_mtime
            // 才关闭，会覆盖服务器文件时间；等待者被唤醒前释放即排除该时序。
            drop(file);
            {
                let mut st = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
                st.syncing = false;
                match res {
                    Ok(()) => st.last_completed_start = Some(start),
                    Err(error) => st.failure = Some((error.kind(), error.to_string())),
                }
            }
            gate.notify.notify_waiters();
        });
    }

    /// 覆盖式 fdatasync：返回的已完成 fdatasync 起始时刻 `S` **必须 ≥ `snap_t`**，
    /// 从而覆盖 `snap_t` 前完成的全部写入（其字节在调用前已落入 OS 页缓存）。
    ///
    /// # 正确性契约
    ///
    /// 调用方须先 `file.flush()` 把自己的 BufWriter 落入页缓存，再记下快照时刻
    /// `snap_t`，然后调用本方法。返回的 `S` 满足 `S >= snap_t`，故凡在 `snap_t`
    /// 之前完成的写入均已被该次（或之后的）fdatasync 持久化——调用方据此才可把
    /// 对应偏移写入 DB，维持 "DB 偏移 <= 已持久化字节" 不变式（BUG-COORD-FSYNC）。
    ///
    /// # 与 [`Self::kick_background`] 的差异
    ///
    /// 本方法**无视** [`MIN_SYNC_GAP`] 且同步等待：只要 `last_completed_start` 尚不
    /// 存在或 `< snap_t`，就必须亲自执行一次新的 fdatasync（或等待在途完成后重判）。
    /// 周期落库路径用 `kick_background` 在后台合并刷写；段完成等需要强一致覆盖的
    /// 路径用本方法。
    ///
    /// 并发完成者天然合并到同一次覆盖 fsync：若已有在途 sync，等待其结束后
    /// 重判；若其 `last_completed_start >= snap_t` 则直接复用，无需再刷。
    async fn sync_covering(
        &self,
        file: &tokio::fs::File,
        snap_t: Instant,
    ) -> std::io::Result<Instant> {
        loop {
            // 决策阶段：持锁判断，绝不跨 .await 持锁。
            let do_sync = {
                let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                match st.last_completed_start {
                    // 已有覆盖 snap_t 的完成 fsync → 直接复用。
                    Some(s) if s >= snap_t => return Ok(s),
                    // 尚未覆盖，但已有一次在进行 → 等待其完成后重判。
                    _ if st.syncing => false,
                    // 尚未覆盖且无在途 → 无视 MIN_SYNC_GAP，由本调用执行。
                    _ => {
                        st.syncing = true;
                        true
                    }
                }
            };

            if do_sync {
                // my_start 记于 fdatasync 之前：它是"覆盖判据"的时刻锚点。
                // 调用契约保证 snap_t 取于本方法之前，故 my_start >= snap_t。
                let my_start = Instant::now();
                let res = file.sync_data().await;
                {
                    let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    st.syncing = false;
                    if res.is_ok() {
                        st.last_completed_start = Some(my_start);
                    }
                }
                self.notify.notify_waiters();
                res?;
                return Ok(my_start);
            }

            // 等待在途 fsync 完成后重判。带 50ms 兜底以规避 notify TOCTOU
            // （与 sync_if_stale / speed_limiter 相同处理）。
            tokio::select! {
                biased;
                () = self.notify.notified() => {}
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }
}

/// 抽干 worker 侧 durable 进度表，对照 coordinator 权威 `segments` 过滤后供批量落库。
///
/// - 未知 `seg_index` 或已 `Completed` 的条目丢弃（完成写由 worker 覆盖 fsync 后立即落库，
///   不走批量通道；Completed 后 dual-write 无意义且可能用旧水位回退）。
/// - 值钳制到 `seg.size()`，防止拆分收窄后的越界水位。
fn take_durable_progress_rows(
    durable_progress: &StdMutex<HashMap<i32, i64>>,
    segments: &BTreeMap<i32, LiveSegment>,
) -> Vec<(i32, i64, i64)> {
    let pending = {
        let mut guard = durable_progress.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    };
    let mut rows = Vec::with_capacity(pending.len());
    for (idx, bytes) in pending {
        let Some(seg) = segments.get(&idx) else {
            continue;
        };
        if seg.state == SegState::Completed {
            continue;
        }
        // (segment_index, downloaded_bytes, start_byte)——start_byte 供批量 WHERE 守卫。
        rows.push((idx, bytes.min(seg.size()).max(0), seg.start_byte));
    }
    rows
}

// ---------------------------------------------------------------------------
// Segment state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegState {
    /// Segment exists but no worker is downloading it yet.
    Pending,
    /// A worker is actively downloading this segment.
    Active,
    /// Segment has been fully downloaded.
    Completed,
}

#[derive(Debug, Clone)]
struct LiveSegment {
    index: i32,
    start_byte: i64,
    end_byte: i64,
    /// Bytes downloaded within this segment (relative to start_byte).
    downloaded_bytes: i64,
    state: SegState,
    /// 持有连接最近一个稳态窗口的实测速率（B/s）；未知（无持有者/预热窗/
    /// 限速窗）为 `None`。拆分按它挑选预计完成最晚的段并均衡切分。
    rate_bps: Option<f64>,
}

impl LiveSegment {
    /// Total size of this segment in bytes.
    fn size(&self) -> i64 {
        (self.end_byte - self.start_byte + 1).max(0)
    }

    /// Remaining bytes to download in this segment.
    fn remaining(&self) -> i64 {
        (self.size() - self.downloaded_bytes).max(0)
    }

    /// Whether this segment has been fully downloaded.
    #[cfg(test)]
    fn is_complete(&self) -> bool {
        self.downloaded_bytes >= self.size()
    }
}

// ---------------------------------------------------------------------------
// Worker ↔ Coordinator messages
// ---------------------------------------------------------------------------

/// Sent by a worker to the coordinator when its segment finishes or fails.
enum WorkerEvent {
    /// A failed segment attempt is entering backoff for another request.
    Retrying {
        seg_index: i32,
        attempt: u32,
        error: String,
    },
    /// Segment completed successfully.
    Done {
        worker_id: usize,
        seg_index: i32,
        downloaded_bytes: i64,
    },
    /// Segment failed after all retries.
    Failed {
        #[allow(dead_code)]
        worker_id: usize,
        seg_index: i32,
        error: DownloadError,
    },
    /// 分段被 coordinator 完成时间抢占（[`NodePool::preempt`]）：worker 已在
    /// 取消分支刷盘并把进度写入共享 seg_states，本 worker 保活等待新派工。
    Preempted { worker_id: usize, seg_index: i32 },
    /// hint 模式首连接的 Range 能力裁决（每任务恰好一次，见
    /// [`RANGE_VERDICT_UNKNOWN`]）。`supports_range == false` 时 coordinator
    /// 必须立即把 Pending 段吸收进开放式首段（不能等 ramp tick——LAN 上首段
    /// 预算可能在 2s 内跑完，Done 后就没有活流可吸收了）。
    RangeVerdict { supports_range: bool },
    /// open-ended 首段首响应通过校验，机队可立即释放，不必等下一个 ramp tick。
    /// 仅在 `assignment.open_ended` 且本 worker 尚未发过时发送一次。
    OpenEndedEstablished,
}

/// Sent by the coordinator to a worker to assign work.
struct WorkerAssignment {
    seg_index: i32,
    seg_start: i64,
    actual_start: i64,
    seg_end: i64,
    /// 开放式首段：Range 头用 `bytes={actual_start}-`（不带终点），服务器把
    /// 响应流一直送到文件末尾，worker 按共享 seg_states 的 end_byte 预算截断。
    /// 仅从字节 0 起始的初始派工置 true——配额型端点（fnOS multiple-download
    /// 的 token 请求次数配额）拒绝其余连接时，coordinator 可把右侧 Pending 段
    /// 并入本段，复用这条已建立的流下完整个文件，无需任何新请求。
    open_ended: bool,
    /// hint 模式（Range 未经验证）的首连接：完全不带 Range/If-Range 头的
    /// plain GET。仅与 `open_ended` 同真（从字节 0 起的初始派工）；响应 200
    /// 走与开放式 200 相同的"直接采用 + 预算截断"路径。
    no_range: bool,
}

/// Result of `find_next_work`: an assignment plus optionally the index of the
/// parent segment that was shrunk by a split (for targeted DB persistence).
struct NextWork {
    assignment: WorkerAssignment,
    /// If this work came from an in-half split, this is the index of the
    /// segment that was shrunk.  `None` when reusing an existing Pending segment.
    split_parent: Option<i32>,
}

/// 子范围下载的任务级上报映射（轨对等「一任务多资源」场景）。
///
/// coordinator 内部【始终】以本资源相对坐标工作——段行、Range 头、扩容、
/// 完整性校验全部不变；仅在两个发射边界（`ProgressUpdate`、`SegmentSplit`）
/// 把坐标平移进任务空间，并按 `owns_task_total` 抑制任务级 `tasks.total_bytes`
/// 写入（子范围时由宿主统一校准）。HTTP 整任务路径用
/// [`ReportScope::whole_task`]，行为逐字节不变。
#[derive(Clone, Copy)]
pub struct ReportScope {
    /// 任务坐标偏移（= 前序资源已占字节）。ProgressUpdate.downloaded_bytes
    /// 与快照/拆分事件的字节区间都加上它。
    pub base: i64,
    /// ProgressUpdate.total_bytes 的覆盖值（>0 生效；0 = 报 planned_total）。
    pub total_override: i64,
    /// 是否允许写 tasks.total_bytes（整任务 true；子范围 false，防止单资源
    /// 长度覆盖任务级总量）。
    pub owns_task_total: bool,
    /// 本资源 `total_bytes` 为权威值（来自 206 Content-Range 分母等强证据）：
    /// resume 漂移吸附与 Content-Range 扩容检查采【零容差】。防止段行被
    /// 「另一轨/另一画质」的近长残留在 1%/1MiB 容差窗口内错认——那会静默
    /// 截断尾部或错拼两种版本的字节。整任务 probe 路径保持既有容差。
    pub strict_total: bool,
}

impl ReportScope {
    /// 整任务下载（HTTP 单资源路径）：零偏移、不覆盖总量、拥有任务级写入。
    #[must_use]
    pub fn whole_task() -> Self {
        Self {
            base: 0,
            total_override: 0,
            owns_task_total: true,
            strict_total: false,
        }
    }

    /// 把本资源相对的分段快照映射进任务坐标：区间 +base；base>0 时前插一个
    /// 覆盖 [0, base) 的 100% 前缀段（index=-1 哨兵；真实段索引不动，保持
    /// SegmentSplit 事件与快照的索引精确关联）。
    fn map_snapshot(&self, mut snapshot: Vec<SegmentProgressInfo>) -> Vec<SegmentProgressInfo> {
        if self.base <= 0 {
            return snapshot;
        }
        for s in &mut snapshot {
            s.start_byte += self.base;
            s.end_byte += self.base;
        }
        snapshot.insert(
            0,
            SegmentProgressInfo {
                index: -1,
                start_byte: 0,
                end_byte: self.base - 1,
                downloaded_bytes: self.base,
                active: None,
            },
        );
        snapshot
    }
}
fn sampled_http_runtime(
    task_id: &str,
    total_bytes: i64,
    parallelism_limit: u32,
    seg_states: &StdMutex<Vec<SegmentProgressInfo>>,
    scope: ReportScope,
    tracker: &TransferTracker,
) -> (Vec<SegmentProgressInfo>, TaskRuntime) {
    let sample_sequence = crate::transfer_activity::next_sample_sequence();
    let snapshot = seg_states.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut snapshot = scope.map_snapshot(snapshot);
    let (active_transfers, active_indices) = tracker.snapshot();
    for segment in &mut snapshot {
        if segment.index >= 0 {
            segment.active = Some(active_indices.contains(&segment.index));
        }
    }
    let runtime = TaskRuntime {
        task_id: task_id.to_owned(),
        sampled_at_ms: chrono::Utc::now().timestamp_millis(),
        sample_sequence,
        active_transfers: Some(active_transfers),
        connected_peers: None,
        parallelism_limit: Some(parallelism_limit),
        total_bytes,
        segments: snapshot
            .iter()
            .map(|s| TaskSegment {
                index: s.index,
                start_byte: s.start_byte,
                end_byte: s.end_byte,
                downloaded_bytes: s.downloaded_bytes,
                active: s.active,
            })
            .collect(),
        // 来源累计由持有节点池的调用方按需回填；这里不做归因。
        source_bytes: None,
    };
    (snapshot, runtime)
}

/// 多路径池下本次运行的加速来源累计 + 已落库基数（供 `TaskRuntime.source_bytes`）。
///
/// 非多节点池（单直连）返回 `None`：该路径不做来源归因。`peak` 保持本次运行
/// 累计单调不减——在途租约进度可能因段拆分 / 租约归还的瞬间口径差短暂回落，
/// 单调化后 UI 与落库增量都不会抖动或重复计数。整池一次加锁，仅随既有周期 tick
/// 调用，不进入逐块热路径。
fn sample_run_source_bytes(
    nodes: &NodePool,
    segments: &mut BTreeMap<i32, LiveSegment>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    base: SourceBytes,
    peak: &mut SourceBytes,
) -> Option<SourceBytes> {
    if !nodes.is_multi() {
        return None;
    }
    sync_downloaded_from_shared(segments, seg_states);
    let run = nodes.source_bytes(&multipath::in_flight(nodes, segments));
    *peak = peak.max(run);
    Some(base.saturating_add(*peak))
}

/// 把本次运行新增的加速来源字节落库（`run` 为单调累计，`flushed` 为已落库部分）。
/// 仅成功后推进 `flushed`，失败下一 tick 重试同一增量，绝不重复计数。
async fn flush_source_bytes(
    db: &Db,
    task_id: &str,
    spawn_gen: i64,
    run: SourceBytes,
    flushed: &mut SourceBytes,
) {
    let delta = run.saturating_sub(*flushed);
    if delta.is_zero() {
        return;
    }
    match db.add_task_source_bytes(task_id, spawn_gen, delta).await {
        Ok(()) => *flushed = flushed.max(run),
        Err(e) => log_info!(
            "[coordinator] task {} source bytes flush failed (non-fatal): {e:#}",
            task_id
        ),
    }
}

/// 本次 coordinator generation 中仍在读取 body 的已验证响应数。
///
/// `total_downloaded` 与 Completed 段都包含恢复前的历史进度，不能证明“当前这批
/// 连接里仍有请求成功”。域名连接上限只有在 403/429 与另一个已通过 Range、
/// validator、大小和编码校验的存活响应并存时才可学习。
#[derive(Default)]
struct GenerationEvidence {
    tracker: TransferTracker,
    /// 本次 generation 内是否有任一响应携带 ETag（强 validator，逐段比对过）。
    etag_seen: AtomicBool,
    /// 本次 generation 内是否有任一响应携带 Last-Modified。
    lm_seen: AtomicBool,
    /// 是否出现过「Last-Modified 变了、却因 Content-Range 总大小与起点一致而被当作
    /// CDN edge 时钟差异容忍」的情形——版本可能已变但 HTTP 层判不出。
    lm_drift_tolerated: AtomicBool,
}

impl GenerationEvidence {
    fn has_active_response(&self) -> bool {
        self.tracker.active() > 0
    }

    fn can_learn_conn_cap(&self, server_rejection: bool) -> bool {
        server_rejection && self.has_active_response()
    }

    fn response_started(&self, index: i32) -> TransferGuard {
        self.tracker.start(index)
    }

    /// 记录一个成功响应携带的版本标识（空串 = 缺失）。
    fn note_response_validators(&self, etag: &str, last_modified: &str) {
        if !etag.is_empty() {
            self.etag_seen.store(true, Ordering::Relaxed);
        }
        if !last_modified.is_empty() {
            self.lm_seen.store(true, Ordering::Relaxed);
        }
    }

    /// 记录一次被容忍的 Last-Modified 漂移。
    fn note_lm_drift_tolerated(&self) {
        self.lm_drift_tolerated.store(true, Ordering::Relaxed);
    }

    /// 完成前是否值得做一次内容采样审计（见 [`splice_audit`]）。仅在「确有嫌疑」时
    /// 为真：没有任何 ETag 逐段比对过，且 ①探测基线带 ETag 却被分段 206 全部剥离，
    /// ②基线只有 Last-Modified 却被剥离，或 ③出现过被容忍的 Last-Modified 漂移。
    /// 从未有过任何版本基线、响应也无 validator 的任务（hint 无 validator）不审计。
    fn content_audit_warranted(&self, baseline_etag: &str, baseline_last_modified: &str) -> bool {
        if self.etag_seen.load(Ordering::Relaxed) {
            return false;
        }
        !baseline_etag.is_empty()
            || (!baseline_last_modified.is_empty() && !self.lm_seen.load(Ordering::Relaxed))
            || self.lm_drift_tolerated.load(Ordering::Relaxed)
    }
}

/// spawn_worker 所需共享句柄的集合，支持事件循环中途（ramp-up 扩容）动态增开
/// worker——启动时与扩容时走完全相同的 spawn 路径，避免两处参数漂移。
///
/// 注意：持有一个 `event_tx` clone 意味着 worker 事件 channel 在整个事件循环
/// 期间不会因"所有 worker 退出"而关闭（`event_rx.recv()` 的 `None` 分支变为
/// 防御性代码）。所有退出路径均由显式条件覆盖：all_done / cancel / 致命错误
/// break / 串行模式的 any_alive 检查。
struct WorkerSpawnCtx {
    event_tx: mpsc::Sender<WorkerEvent>,
    task_id: String,
    url: String,
    dest: PathBuf,
    temp_file_guard: TempFileGuard,
    planned_total: Arc<AtomicI64>,
    size_is_estimate: bool,
    first_validators: Arc<StdMutex<Option<(String, String)>>>,
    nodes: Arc<NodePool>,
    worker_cancel: CancellationToken,
    conn_sensitive: Arc<AtomicBool>,
    reconnect_hostile: Arc<AtomicBool>,
    range_verdict: Arc<AtomicU8>,
    total_downloaded: Arc<AtomicI64>,
    generation_evidence: Arc<GenerationEvidence>,
    request_path: Arc<RequestPath>,
    seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    db: Db,
    speed_limiter: SpeedLimiter,
    spec: Arc<crate::downloader::DownloadSpec>,
    etag: String,
    last_modified: String,
    sync_gate: FileSyncGate,
    /// worker 周期 durable 进度（seg_index → 段内已 fsync 覆盖字节）；coordinator 批量落库。
    durable_progress: Arc<StdMutex<HashMap<i32, i64>>>,
    scope: ReportScope,
    spawn_gen: i64,
}

impl WorkerSpawnCtx {
    /// 新建一个 worker（分配 channel + spawn task），返回其分配通道与句柄。
    fn spawn(
        &self,
        worker_id: usize,
    ) -> (mpsc::Sender<WorkerAssignment>, tokio::task::JoinHandle<()>) {
        let (assign_tx, assign_rx) = mpsc::channel::<WorkerAssignment>(4);
        let handle = spawn_worker(
            worker_id,
            assign_rx,
            self.event_tx.clone(),
            self.task_id.clone(),
            self.url.clone(),
            self.dest.clone(),
            self.temp_file_guard.clone(),
            self.planned_total.clone(),
            self.size_is_estimate,
            self.first_validators.clone(),
            self.nodes.clone(),
            self.worker_cancel.clone(),
            self.conn_sensitive.clone(),
            self.reconnect_hostile.clone(),
            self.range_verdict.clone(),
            self.total_downloaded.clone(),
            self.generation_evidence.clone(),
            self.request_path.clone(),
            self.seg_states.clone(),
            self.db.clone(),
            self.speed_limiter.clone(),
            self.spec.clone(),
            self.etag.clone(),
            self.last_modified.clone(),
            self.sync_gate.clone(),
            self.durable_progress.clone(),
            self.scope,
            self.spawn_gen,
        );
        (assign_tx, handle)
    }
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

/// Run the dynamic segment coordinator.
/// 成功时返回本次下载的【最终有效总大小】：当途中发生就地扩容（服务器 206
/// `Content-Range` 分母自报真实大小 > 规划，见 [`DownloadError::TrueSizeLarger`]）
/// 或 resume 漂移吸附时，可能不同于入参 `total_bytes`——调用方
/// （`run_download_inner`）须以返回值校准完整性检查与完成信号。
///
/// `scope`：子范围下载的任务级上报映射（见 [`ReportScope`]）；整任务路径
/// 传 [`ReportScope::whole_task`]。
///
///
/// This replaces the old "spawn N tasks and join" logic in
/// `download_multi_segment`.  The function signature is intentionally close to
/// the original so it can be swapped in with minimal changes.
#[allow(clippy::too_many_arguments)]
pub async fn run_coordinated_download(
    task_id: &str,
    url: &str,
    dest: &Path,
    total_bytes: i64,
    // `total_bytes` 是否为【未经 probe 验证的估计值】（fresh hint 模式）。透传至
    // do_segment 决定 Content-Range 分母的扩容容差（估计值→零容差）。
    size_is_estimate: bool,
    initial_segment_count: i32,
    nodes: Arc<NodePool>,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    spec: &Arc<crate::downloader::DownloadSpec>,
    sink: &dyn EventSink,
    etag: &str,
    last_modified: &str,
    scope: ReportScope,
    spawn_gen: i64,
    // 是否允许 hint 解封抬高 worker_cap：仅 Auto 默认档（segment_count<=0 且
    // auto_max_connections 等于引擎默认值）为 true；用户显式段数或自定义上限
    // 时 false，尊重其设定的天花板。
    allow_hint_uncap: bool,
    // `ProxyMode::Auto` 任务的多路径上下文（None = 非 Auto/无候选/一次性
    // failover 链路——单路径，只做连接窗口采样与均衡拆分）。
    auto_proxy: Option<Arc<crate::auto_proxy::AutoProxyCtx>>,
    // 多网卡聚合输入（None = 功能关闭）。额外链路在后台规划，首个完整 ramp
    // 窗口挂入节点池，不推迟首连接。
    multi_nic: Option<Arc<crate::multi_nic::MultiNicInput>>,
) -> Result<i64, DownloadError> {
    // ----- 0. Defensive checks ------------------------------------------------
    if total_bytes <= 0 {
        return Err(DownloadError::Other(format!(
            "coordinator: invalid total_bytes={total_bytes} for task {task_id}"
        )));
    }
    if initial_segment_count < 1 {
        return Err(DownloadError::Other(format!(
            "coordinator: invalid initial_segment_count={initial_segment_count} for task {task_id}"
        )));
    }
    // Every coordinator caller shares this write boundary.  This is especially
    // important for DASH multi-segment downloads, which reach preallocation
    // before the single-stream downloader's directory preparation.
    output::ensure_parent(dest).await?;
    let temp_file_guard = TempFileGuard::acquire(dest, task_id).await?;

    // 段数钳制：保证每个新建分段至少覆盖 1 字节。build_fresh_segments 用
    // chunk = total_bytes / count；当 count > total_bytes 时 chunk=0，会生成大量
    // start>end 的空段，worker 据此发出非法 Range（如 bytes=0--1），分段永远无法
    // 被标记 Completed，整个下载**死循环**（实测：1 字节文件配 32 段必 hang）。
    // 生产路径通常在上游钳制，但此处作为 coordinator 自身的防御兜底（其防御检查
    // 此前只挡 total_bytes<=0 / count<1，漏了 count>total_bytes 这条）。
    let initial_segment_count = if (initial_segment_count as i64) > total_bytes {
        // total_bytes 在 count>total_bytes 时必然很小，可安全转 i32。
        total_bytes as i32
    } else {
        initial_segment_count
    };

    // 落盘布局：非稀疏文件系统（FAT32 / exFAT SD 卡等）上整文件预分配与高偏移
    // 起写都要同步写零，起步延迟随文件大小线性增长（#974）。须先于建段：布局
    // 决定新建段数与续传完整性判据。
    let file_layout = file_layout::detect(dest, total_bytes as u64).await?;
    if !file_layout.preallocates() {
        log_info!(
            "[coordinator] task {} 目标文件系统延伸需写零：不预分配，贴近 EOF 推进写入（窗口 {} 字节）",
            task_id,
            file_layout::FRONTIER_WINDOW_BYTES
        );
    }
    let rebuild_plan_count = file_layout.fresh_plan_count(initial_segment_count);

    // ----- 0.5 夺取段行布局属主权 --------------------------------------------
    // 必须先于 load_segments/建行：从这一刻起，旧 spawn 迟到的段进度写入
    // （update_segment_progress_bounded 的 epoch 存在性守卫）全类失效——
    // 不存在"重建后、夺权前"的空窗；持久化失败必须在启动 worker 前停止。
    db.set_segments_epoch(task_id, spawn_gen).await?;

    // ----- 1. Build initial segment map from DB or fresh calculation ---------
    let existing = db.load_segments(task_id).await?;
    let mut segments: BTreeMap<i32, LiveSegment> = BTreeMap::new();
    let mut next_index: i32;

    if existing.is_empty() {
        // Create fresh segments (uniform split).
        //
        // hint 模式（size_is_estimate，Range 未经 probe 验证）只规划【一个】
        // 覆盖全文件的段：首连接是不带 Range 的 plain GET，若首响应证实
        // Range 支持（RangeVerdict → SUPPORTED），ramp 补充循环会经
        // find_next_work 的动态拆分把这个大段逐步切给带 Range 的并发
        // worker（IDM 式先单流后升级）；若不支持，这个段本身就覆盖整个
        // 文件，plain GET 流一路下完——**不存在**"预切段在吸收合并落地前
        // 被旧预算截断"的竞速（预算从一开始就是全文件）。
        //
        // GrowOnWrite 布局同样只建一个段，原因不同：均匀分段的高偏移起点会立即
        // 触发大范围零填充，改由拆分逐个贴近前沿放出连接。
        let plan_count = if size_is_estimate {
            1
        } else {
            rebuild_plan_count
        };
        let (fresh, db_segs) = build_fresh_segments(plan_count, total_bytes);
        segments = fresh;
        db.insert_segments(task_id, &db_segs).await?;
        next_index = plan_count;
    } else {
        // Restore from DB (resume scenario).
        next_index = 0;
        for seg in &existing {
            let state = if seg.downloaded_bytes >= (seg.end_byte - seg.start_byte + 1) {
                SegState::Completed
            } else {
                SegState::Pending
            };
            segments.insert(
                seg.index,
                LiveSegment {
                    index: seg.index,
                    start_byte: seg.start_byte,
                    end_byte: seg.end_byte,
                    downloaded_bytes: seg.downloaded_bytes,
                    state,
                    rate_bps: None,
                },
            );
            if seg.index >= next_index {
                next_index = seg.index + 1;
            }
        }
    }

    // Verify the invariant: segment ranges must cover [0, total_bytes-1] exactly.
    //
    // When resuming, the freshly probed total_bytes may differ from the value
    // encoded in DB segment boundaries (e.g. CDN re-signing shifts Content-Length
    // by a few bytes, or the server file has genuinely changed size).
    //
    // Three distinct cases:
    //
    //  db_total == total_bytes
    //    → Exact match.  Trust DB segments as-is.
    //
    //  db_total < total_bytes  (server reports a *larger* file)
    //    → Two sub-cases distinguished by a tolerance threshold:
    //
    //      delta <= threshold  (CDN drift — Transfer-Encoding overhead,
    //                           dynamic header injection, signed-URL padding…)
    //        → The extra bytes the server "claims" are not real file content.
    //          Trust DB segments and correct tasks.total_bytes to db_total so
    //          the progress bar reaches exactly 100 % when segments complete.
    //
    //      delta > threshold  (file genuinely grew on the server)
    //        → The tail content is real and must be downloaded.  Rebuild
    //          segments from scratch using the new total_bytes so every byte
    //          is covered.  Without this, the tail would never be fetched,
    //          the file would be silently truncated, and the integrity check
    //          would still pass because it compares against the old db_total.
    //
    //  db_total > total_bytes  (server reports a *smaller* file)
    //    → Do NOT trust DB segments.  Requesting Range bytes beyond the server's
    //      actual EOF would return 416 Range Not Satisfiable.  Fall through with
    //      total_bytes so validate_coverage detects the mismatch and rebuilds
    //      segments to fit the new (smaller) file size.
    //
    // Threshold: same formula used in db::update_task_file_info_resume —
    //   1 % of db_total, capped at 1 MiB, floor 1 byte.
    // Keeping both thresholds in sync ensures the two layers never disagree
    // about whether a size change is "real".
    let mut effective_total_bytes = if !existing.is_empty() {
        // segments is non-empty here; max() will always return Some.
        let db_total = segments
            .values()
            .map(|s| s.end_byte + 1)
            .max()
            .unwrap_or(total_bytes); // unreachable, defensive only

        if db_total != total_bytes {
            log_info!(
                "[coordinator] task {} total_bytes probe={} vs db_segments={}",
                task_id,
                total_bytes,
                db_total
            );
        }

        if db_total == total_bytes {
            // Exact match — nothing to do.
            db_total
        } else if db_total < total_bytes {
            // Server reports a larger file than what the DB segments cover.
            // Decide whether this is CDN drift or a genuine file growth.
            let threshold: i64 = if scope.strict_total {
                // 权威 total（轨对 scope）：任何不等即换轨/换画质/真变化，
                // 一律重建，绝不吸附。
                0
            } else {
                (db_total / 100).clamp(1, 1_048_576)
            };
            let delta = total_bytes - db_total;

            if delta <= threshold {
                // CDN drift — the extra bytes are not real file content.
                // Trust existing segments and snap tasks.total_bytes back to
                // db_total so the UI reaches 100 % on segment completion.
                log_info!(
                    "[coordinator] task {} probe={} db={} delta={} <= threshold={}: \
                     CDN drift, trusting DB segments",
                    task_id,
                    total_bytes,
                    db_total,
                    delta,
                    threshold
                );
                if scope.owns_task_total {
                    db.update_task_total_bytes(task_id, db_total).await?;
                }
                db_total
            } else {
                // Genuine file growth — the tail bytes are real and must be
                // fetched.  Rebuild segments from scratch using the new size.
                // This discards all prior progress, but keeping the old
                // segments would silently truncate the file.
                log_info!(
                    "[coordinator] task {} probe={} db={} delta={} > threshold={}: \
                     file genuinely grew, rebuilding segments to avoid tail truncation",
                    task_id,
                    total_bytes,
                    db_total,
                    delta,
                    threshold
                );
                db.delete_segments(task_id).await?;
                let (fresh, db_segs) = build_fresh_segments(rebuild_plan_count, total_bytes);
                segments = fresh;
                db.insert_segments(task_id, &db_segs).await?;
                next_index = rebuild_plan_count;
                if scope.owns_task_total {
                    db.update_task_total_bytes(task_id, total_bytes).await?;
                }
                // Return early — segments are already valid, skip validate_coverage.
                // Re-run pre-allocation and workers with total_bytes.
                total_bytes
            }
        } else {
            // db_total > total_bytes: server file is smaller than DB segments cover.
            // Using db_total would issue Range requests past EOF → 416 errors.
            // Use total_bytes so validate_coverage below detects the mismatch
            // and resets segments to the current file size.
            log_info!(
                "[coordinator] task {} DB segments cover {} bytes but server reports only {}; \
                 resetting segments to avoid out-of-range requests",
                task_id,
                db_total,
                total_bytes
            );
            total_bytes
        }
    } else {
        total_bytes
    };

    if let Err(msg) = validate_coverage(&segments, effective_total_bytes) {
        log_info!(
            "[coordinator] task {} segment coverage invalid: {}. Resetting all segments.",
            task_id,
            msg
        );
        // Coverage is broken (e.g. partial split persisted before crash, or file
        // size changed so db_total > total_bytes above).
        // Safest recovery: wipe segments and start fresh.
        db.delete_segments(task_id).await?;
        let (fresh, db_segs) = build_fresh_segments(rebuild_plan_count, total_bytes);
        segments = fresh;
        db.insert_segments(task_id, &db_segs).await?;
        next_index = rebuild_plan_count;
        // update_task_total_bytes may have set tasks.total_bytes to db_total earlier
        // (db_total <= total_bytes path).  After a fresh reset the canonical size is
        // total_bytes (from probe), so re-sync.
        if scope.owns_task_total {
            db.update_task_total_bytes(task_id, total_bytes).await?;
        }
    }

    // Integrity check for resumed files: verify the file on disk is intact.
    //
    // 重要局限性（避免给维护者制造虚假安全感）：本检查只能基于**文件大小**判断，
    // 而多段下载会在写入任何数据**之前**就把临时文件预分配到 effective_total_bytes
    // （见下方第 2 步 fallocate/SetFileInformationByHandle）。因此正常续传时
    // file_len 恒等于 effective_total_bytes，它**无法**反映“实际已写入内容量”，
    // 也就**无法**检测以下内容空洞类损坏：
    //   - 崩溃发生在 seek+write 与 update_segment_progress 之间（DB 记账领先磁盘）；
    //   - 外部工具截断了内容但保留了 apparent size；
    //   - 稀疏空洞被读为 0。
    // 这类内容损坏只能由末尾的 seg_total 聚合检查（run_coordinated_download 第 8 步）
    // 或可选的 checksum 兜底，size 检查对此形同虚设。
    //
    // 本检查能可靠捕获的，仅是**文件比上次预分配后更短**的情形：文件被外部删除
    // （file_len==0）或被截断到 effective_total_bytes 以下。一旦续传时（db_downloaded>0）
    // 文件存在，则上一会话必然已执行过预分配（预分配先于任何 worker 写入），
    // 故 file_len 本应 >= effective_total_bytes；若更短即为外部损坏，重置是安全的，
    // 且因合规续传文件恒 >= effective_total_bytes 而不会误伤。
    //
    // GrowOnWrite 布局不预分配，合规文件只保证覆盖已落库的最高写入位置
    // （durable 水位只在 fsync 覆盖后推进），判据随之换成该高水位。
    {
        let db_downloaded: i64 = segments.values().map(|s| s.downloaded_bytes).sum();
        let file_len = match tokio::fs::metadata(dest).await {
            Ok(m) => m.len() as i64,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        let required_len = if file_layout.preallocates() {
            effective_total_bytes
        } else {
            written_high_water(&segments)
        };
        // 与 required_len 比较（而非更弱的 db_downloaded）：预分配后合规
        // 续传文件长度恒 >= effective_total_bytes，低于即为外部截断/删除。
        if db_downloaded > 0
            && (file_len == 0 || file_len < db_downloaded || file_len < required_len)
        {
            log_info!(
                "[coordinator] task {} file integrity mismatch: file_len={}, db_downloaded={}, expected>={}. Resetting.",
                task_id,
                file_len,
                db_downloaded,
                required_len
            );
            for seg in segments.values_mut() {
                seg.downloaded_bytes = 0;
                seg.state = SegState::Pending;
            }
            db.reset_segments_progress(task_id).await?;
        }
    }

    // 加速来源累计基数：必须在上面的进度复位之后读取（复位同时清零计数）。
    // `source_flushed` 是本次运行已落库的部分，`source_peak` 是本次运行的单调累计。
    let source_base = match db.load_task_source_bytes(task_id).await {
        Ok(v) => v,
        Err(e) => {
            log_info!(
                "[coordinator] task {} load source bytes failed (counters restart from 0): {e:#}",
                task_id
            );
            SourceBytes::default()
        }
    };
    let mut source_peak = SourceBytes::default();
    let mut source_flushed = SourceBytes::default();

    // ----- 2. Pre-allocate file to full size --------------------------------
    // 平台策略（fallocate / SetFileInformationByHandle / set_len 回退）见
    // preallocate_file_len——就地扩容（TrueSizeLarger）复用同一助手延长文件。
    // GrowOnWrite 布局跳过：文件随写入增长（见 file_layout 模块文档）。
    truncate_stale_tail(dest, effective_total_bytes as u64).await?;
    if file_layout.preallocates() {
        preallocate_file_len(dest, effective_total_bytes as u64, &temp_file_guard).await?;
    }
    let split_window = file_layout.split_window();

    // ----- 3. Shared state for progress reporting ---------------------------
    let total_downloaded = Arc::new(AtomicI64::new(
        segments.values().map(|s| s.downloaded_bytes).sum::<i64>(),
    ));
    // Segment restoration/coverage repair can move the real starting counter
    // in either direction relative to tasks.downloaded_bytes. Establish it
    // before spawning workers, through the same ordered progress channel.
    let baseline = total_downloaded.load(Ordering::Relaxed) + scope.base;
    if progress_tx
        .send(ProgressUpdate {
            task_id: task_id.to_owned(),
            downloaded_bytes: baseline,
            download_baseline: Some(baseline),
            total_bytes: if scope.total_override > 0 {
                scope.total_override
            } else {
                effective_total_bytes
            },
            status: 1,
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("segment progress receiver closed");
        return Err(DownloadError::Cancelled);
    }
    let generation_evidence = Arc::new(GenerationEvidence::default());
    let request_path = Arc::new(RequestPath::default());

    // 规划总大小的【共享可变】视图。worker 栈上的 i64 拷贝会在就地扩容后过期
    // （尾段 worker 拿旧值 → 对同一分母反复误报 TrueSizeLarger / 进度上报错误
    // 总量），故经 Arc<AtomicI64> 共享，仅由 coordinator 在扩容时单点更新。
    let planned_total = Arc::new(AtomicI64::new(effective_total_bytes));

    // hint 模式（无 probe 基线）的跨段版本一致性基线：第一个 206 响应携带的
    // (ETag, Last-Modified)。见 do_segment 内的 latch 守卫。
    let first_validators: Arc<StdMutex<Option<(String, String)>>> = Arc::new(StdMutex::new(None));

    // The shared segment-progress vector mirrors the `segments` map and is
    // updated by workers via std::sync::Mutex (cheap, no async).
    let seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>> =
        Arc::new(StdMutex::new(build_seg_state_vec(&segments)));

    // ----- 4. Event channel (workers → coordinator) -------------------------
    let (event_tx, mut event_rx) = mpsc::channel::<WorkerEvent>(64);

    // ----- 5. Worker pool ---------------------------------------------------
    // 渐进启动：`initial_segment_count` 是【最大连接数上限】，
    // 不是启动并发。启动只开 RAMP_INITIAL_WORKERS 条连接，后续由事件循环的
    // ramp 定时分支按吞吐反馈逐步扩容到 worker_cap。
    let pending_count = segments
        .values()
        .filter(|s| s.state == SegState::Pending)
        .count();

    // 本任务的连接数硬上限：规划段数 ∧ MAX_SEGMENTS ∧ 域名学习缓存；
    // 无负面 cap 且允许 hint 解封时，可再抬到 hint×2（见 hint_uncap_ceiling）。
    let start_hint: Option<usize> = domain_conn_hint(url).map(|h| h as usize);
    // 可变：Auto 默认档下若扩容到顶仍有接近线性的增益，任务内解封翻倍
    // （见 in_task_uncap_ceiling）。
    let mut worker_cap = {
        let mut cap = initial_segment_count.clamp(1, MAX_SEGMENTS) as usize;
        if let Some(domain_cap) = domain_conn_cap(url) {
            let dc = domain_cap.max(1) as usize;
            if dc < cap {
                log_info!(
                    "[adaptive] task {} 域名连接上限缓存命中：cap {} -> {}",
                    task_id,
                    cap,
                    dc
                );
                cap = dc;
            }
        }
        let before_uncap = cap;
        cap = hint_uncap_ceiling(
            allow_hint_uncap,
            domain_conn_cap(url),
            domain_conn_hint(url),
            cap,
        );
        if let Some(limit) = spec.connection_cap() {
            cap = cap.min(limit.max(1));
        }
        if cap > before_uncap {
            log_info!(
                "[adaptive] task {} hint 解封：cap {} -> {}（hint={:?}）",
                task_id,
                before_uncap,
                cap,
                domain_conn_hint(url)
            );
        }
        cap
    };
    // 高线程任务（超出自动探索上限）在默认软限制下几个任务就会撞 EMFILE。
    if worker_cap > HINT_UNCAP_MAX {
        crate::proc::raise_nofile_limit_once();
    }
    // 当前允许的活跃连接额度（ramp 控制变量，1..=worker_cap 内调整）。
    // 命中同域名正面起步提示时直接以已证明规模起步（消除重复爬坡）；
    // 首连接仍受下方 open-ended 单启动守卫约束，机队在首个 ramp tick 放出。
    let mut allowed_workers = initial_allowed_workers(worker_cap, domain_conn_hint(url));
    if allowed_workers > RAMP_INITIAL_WORKERS {
        log_info!(
            "[adaptive] task {} 正面起步提示命中：起步额度 {}（cap={}）",
            task_id,
            allowed_workers,
            worker_cap
        );
    }
    // 受益过的规模：起步额度作为下界；Improved 时抬到当时 allowed，
    // shrink/Collapse 回滚后 min 收敛。任务尾学习取 proven ∩ beneficial。
    let mut beneficial_scale: usize = allowed_workers;
    // 只为 Pending 段开 worker；resume 时多数段可能已完成。
    // 存在开放式首段候选（从字节 0 起的零进度 Pending 段，判据与下方
    // pending_assignments 的 open_ended 完全一致）时，初始只启动它一个：
    // 开放式首段是配额型端点（fnOS multiple-download 等）的潜在生命线，与
    // 其他 worker 并发启动会竞速消耗请求配额——竞速输掉 = 生命线永远 400 =
    // 任务死（实测随机复现）。其余 worker 由 ramp tick（RAMP_TICK_SECS=2s）
    // 自然补齐，正常服务器仅第二连接晚 2s，多段吞吐无感。
    let has_open_ended_candidate = segments
        .values()
        .any(|s| s.state == SegState::Pending && s.start_byte + s.downloaded_bytes == 0);
    let initial_workers = if has_open_ended_candidate {
        pending_count.min(allowed_workers).min(1)
    } else {
        pending_count.min(allowed_workers)
    };
    // worker 周期 durable 进度汇聚表：各段仅在 fsync 覆盖推进时写入，
    // coordinator 每 DB_SAVE_INTERVAL_SECS 抽干后一次事务批量落库。
    let durable_progress: Arc<StdMutex<HashMap<i32, i64>>> =
        Arc::new(StdMutex::new(HashMap::new()));

    // fdatasync 合并闸：多段共享，把各 worker 每 3s 的整盘 fsync 合并为全局约每
    // MIN_SYNC_GAP 一次（fdatasync 本就刷整个 inode，per-fd 重复毫无意义）。
    let sync_gate = FileSyncGate::new();

    let mut worker_assign_txs: Vec<Option<mpsc::Sender<WorkerAssignment>>> =
        Vec::with_capacity(initial_workers);
    let mut worker_handles: Vec<Option<tokio::task::JoinHandle<()>>> =
        Vec::with_capacity(initial_workers);

    // Collect pending assignments.
    let mut pending_assignments: Vec<WorkerAssignment> = segments
        .values()
        .filter(|s| s.state == SegState::Pending)
        .map(|s| WorkerAssignment {
            seg_index: s.index,
            seg_start: s.start_byte,
            actual_start: s.start_byte + s.downloaded_bytes,
            seg_end: s.end_byte,
            // 从字节 0 起始的段用开放式 Range（fresh 下载恒有且仅有一个；
            // resume 时仅当该段零进度）。其余段闭区间。
            open_ended: s.start_byte + s.downloaded_bytes == 0,
            // hint 模式（Range 未验证）的首连接完全不带 Range 头——配额型
            // 端点对 bounded Range 一律 400 且作废 token（fnOS 实测）。
            no_range: size_is_estimate && s.start_byte + s.downloaded_bytes == 0,
        })
        .collect();
    let mut assign_iter = pending_assignments.drain(..);

    // 协调器专属【子令牌】：所有 workers 监听它，而非任务主令牌 cancel_token。
    // 关键不变式——协调器遇致命错误（Path B）时只 cancel 这个子令牌来停掉 workers，
    // 【绝不】cancel 主令牌：run_download_inner 捕获 RangeNotSupported 后要用【存活的
    // 主令牌】回退 download_single 单流下载；若主令牌被 cancel，单流回退会瞬间命中
    // cancelled() 一个字节都下不了 → 任务永久卡死（历史致命 BUG）。用户主动取消时
    // cancel 主令牌，子令牌作为其 child 自动级联取消，workers 照常停止，语义不变。
    let worker_cancel = cancel_token.child_token();
    // Unwinding or dropping the coordinator must stop detached body readers as well.
    let _worker_cancel_on_drop = worker_cancel.clone().drop_guard();

    // 连接敏感 latch：workers 一旦观察到服务器对 Range 请求返回非 206（瞬时/持续 200），
    // 置位此标志。coordinator 据此停止【主动拆分】（见下方 proactive 定时分支）以降低
    // 连接 churn——每次拆分最终=一次新连接，激进预拆分会推高连接建立速率，从而诱发
    // alist 代理迅雷/光鸭云盘等连接受限后端的瞬时 200。按需的 reactive 拆分（空闲
    // worker 抢救尾段）仍保留，故不引入尾段停滞。仅在观察到敏感行为后生效——正常
    // 服务器永不置位，行为与优化前完全一致（零回归）。
    let conn_sensitive = Arc::new(AtomicBool::new(false));

    // 重连敌意 latch：吸收合并发生（配额型端点的强信号——新连接已被 400/403
    // 拒绝且 Pending 段已并入唯一存活流）时由 coordinator 置位。worker 据此把
    // 停滞容忍从 5s 提到 60s（见 CHUNK_STALL_TIMEOUT_HOSTILE）：此时掐流重连
    // 必然撞拒绝，等价于判死整个任务。
    let reconnect_hostile = Arc::new(AtomicBool::new(false));

    // Range 能力裁决（见 RANGE_VERDICT_* 常量）：hint 模式从 UNKNOWN 起步，
    // 首连接 plain GET 的响应决定裁决；probe/resume 路径恒 SUPPORTED。
    let range_verdict = Arc::new(AtomicU8::new(if size_is_estimate {
        RANGE_VERDICT_UNKNOWN
    } else {
        RANGE_VERDICT_SUPPORTED
    }));

    // ---- 多路径调度（ProxyMode::Auto 的备选路径在首个 worker 租借前挂入
    // 节点池，先验占优的路径起飞即被选中；非 Auto 任务只做窗口采样）----
    let mut multipath = multipath::Multipath::new(auto_proxy, multi_nic, url, &nodes, task_id);

    // worker 生成上下文：启动与 ramp 扩容共用同一 spawn 路径。
    let ctx = WorkerSpawnCtx {
        event_tx,
        task_id: task_id.to_string(),
        url: url.to_string(),
        dest: dest.to_path_buf(),
        temp_file_guard: temp_file_guard.clone(),
        planned_total: planned_total.clone(),
        size_is_estimate,
        first_validators: first_validators.clone(),
        nodes: nodes.clone(),
        worker_cancel: worker_cancel.clone(),
        conn_sensitive: conn_sensitive.clone(),
        reconnect_hostile: reconnect_hostile.clone(),
        total_downloaded: total_downloaded.clone(),
        generation_evidence: generation_evidence.clone(),
        request_path: request_path.clone(),
        range_verdict: range_verdict.clone(),
        seg_states: seg_states.clone(),
        db: db.clone(),
        speed_limiter: speed_limiter.clone(),
        spec: spec.clone(),
        etag: etag.to_string(),
        last_modified: last_modified.to_string(),
        sync_gate: sync_gate.clone(),
        durable_progress: durable_progress.clone(),
        scope,
        spawn_gen,
    };

    // 开放式首段跟踪：当前仍由某 worker 持有开放式响应流的段索引；
    // 该段 Done/Failed 时清除（流已不在）。
    let mut open_ended_streaming: Option<i32> = None;

    for worker_id in 0..initial_workers {
        let (assign_tx, handle) = ctx.spawn(worker_id);

        // Send initial assignment (if available).
        if let Some(assignment) = assign_iter.next() {
            let seg_idx = assignment.seg_index;
            let is_open_ended = assignment.open_ended;
            if let Some(seg) = segments.get_mut(&seg_idx) {
                seg.state = SegState::Active;
            }
            // This send cannot fail — channel just created with capacity 4.
            if let Err(error) = assign_tx.try_send(assignment) {
                tracing::debug!(worker_id, %error, "initial HTTP worker assignment failed");
                if let Some(seg) = segments.get_mut(&seg_idx) {
                    seg.state = SegState::Pending;
                }
            }
            if is_open_ended {
                open_ended_streaming = Some(seg_idx);
            }
        }

        worker_assign_txs.push(Some(assign_tx));
        worker_handles.push(Some(handle));
    }
    drop(assign_iter);
    drop(pending_assignments);

    // If all segments are already completed (rare but possible), exit early.
    if all_done(&segments) {
        for tx in &mut worker_assign_txs {
            *tx = None;
        }
        let mut join_error = None;
        for handle in &mut worker_handles {
            if let Some(h) = handle.take()
                && let Err(error) = h.await
            {
                if error.is_cancelled() {
                    tracing::debug!("HTTP worker cancelled");
                } else if join_error.is_none() {
                    join_error = Some(error);
                } else {
                    crate::logger::report_warning(
                        "http-coordinator",
                        "join another completed worker",
                        &error,
                    );
                }
            }
        }
        if let Some(error) = join_error {
            return Err(DownloadError::Other(format!(
                "HTTP worker join failed: {error}"
            )));
        }
        let (_, mut runtime) = sampled_http_runtime(
            task_id,
            if scope.total_override > 0 {
                scope.total_override
            } else {
                effective_total_bytes
            },
            worker_cap as u32,
            &seg_states,
            scope,
            &generation_evidence.tracker,
        );
        runtime.source_bytes = sample_run_source_bytes(
            &nodes,
            &mut segments,
            &seg_states,
            source_base,
            &mut source_peak,
        );
        sink.emit(EngineEvent::TaskRuntimeChanged(runtime));
        return Ok(effective_total_bytes);
    }

    // ----- 6. Coordinator event loop ----------------------------------------
    // serial_mode: 当检测到服务器拒绝多连接（403/429）时置为 true，
    // 此后 coordinator 同一时刻只分配一个分段给一个 worker，
    // 避免并发连接触发服务器的反多线程机制。
    let mut serial_mode = false;
    let mut final_error: Option<DownloadError> = None;
    // 本次下载已执行的就地扩容次数（TrueSizeLarger 分支，上限 MAX_SIZE_EXPANSIONS）。
    let mut expansions: u32 = 0;

    // 吞吐量跟踪：用于动态调整 MIN_SPLIT_BYTES
    let mut last_throughput_bytes = total_downloaded.load(Ordering::Relaxed);
    let mut last_throughput_time = Instant::now();
    let mut current_min_split = MIN_SPLIT_BYTES;
    // 与 current_min_split 同节拍刷新的单连接速率估计，供拆分代价模型在逐段
    // 速率样本出现前使用（见 SplitCost::conn_bps）。
    let mut conn_bps_hint: Option<f64> = None;

    // Proactive split timer: pre-create Pending segments so the next idle
    // worker can pick one up immediately without a split in the hot path.
    let mut proactive_interval =
        tokio::time::interval(Duration::from_secs(PROACTIVE_SPLIT_INTERVAL_SECS));
    proactive_interval.tick().await; // consume the immediate first tick

    // ---- 渐进/自适应连接调度状态 ----
    // freeze：Active 可倍增扩容；Soft 冷却后 +1 试探；Hard 永久停扩。
    let mut freeze = FreezeState::Active;
    // 分级降级计数：第 1 次拒绝 → 乘性减半（保留存活连接）；第 2 次 → 串行模式。
    let mut reject_strikes: u32 = 0;
    // 单段瞬时失败隔离计数（seg_index → 已隔离次数），见 SEGMENT_TRANSIENT_REQUEUE_MAX。
    let mut transient_requeues: HashMap<i32, u32> = HashMap::new();
    // 首次拒绝乘性降额后的额度：此后仍高于它的在途连接被拒视为兑现那次降额
    // （shedding），而非新的拒绝证据。未降额过 = usize::MAX（永不 shedding）。
    let mut reject_quota: usize = usize::MAX;
    // 任务内解封前的连接天花板（None = 未解封或已回退）。解封后的超额连接被拒
    // 是越界证据而非连接压力：回退到它，不计拒绝、不写域名缓存，本任务不再解封。
    let mut uncap_floor: Option<usize> = None;
    let mut uncap_disabled = false;
    // 全员 429 后的单连接复活：到点由 ramp tick 派一个 worker 续传 Pending 段。
    let mut revive_at: Option<Instant> = None;
    let mut revives_used: u32 = 0;
    // 上个 ramp tick 刚扩容，本 tick 用窗口吞吐评估扩容效果。
    let mut awaiting_ramp_eval = false;
    // 当前 awaiting 是否为软冻结 +1 试探窗（非倍增）。试探失败不写域名缓存。
    let mut probe_window = false;
    // 整任务累计软试探次数（受 RAMP_SOFT_PROBE_TASK_BUDGET 约束）。
    let mut soft_probe_task_used: u32 = 0;
    let mut pre_grow_throughput = 0.0_f64;
    // 扩容前的连接额度：评估判定崩塌/试探失败时回滚到此规模。
    let mut pre_grow_workers = allowed_workers;
    // 任务内观察到的峰值窗口吞吐（持续劣化收缩的参照基准）。
    let mut peak_throughput = 0.0_f64;
    // 持续劣化计数：连续 RAMP_SHRINK_STRIKES 个窗口跌破峰值 × RAMP_COLLAPSE_FACTOR
    // 时乘性收缩。吞吐恢复即清零。
    let mut shrink_strikes: u32 = 0;
    // 收缩验证：Some((收缩前额度, 收缩那一窗吞吐, 收缩后最高窗口吞吐, 已观察窗口数))。
    let mut shrink_eval: Option<(usize, f64, f64, u32)> = None;
    // 一次收缩被证伪（吞吐没有回升）后，本任务不再做持续劣化收缩。
    let mut shrink_disabled = false;
    // 正面学习：任务内【无拒绝窗口】观察到的最大存活连接数。任务无拒绝
    // 成功完成时作为同域名起步提示落盘（见函数末尾第 9 步）。
    let mut proven_scale: usize = 0;
    // ramp 独立采样窗口（与上方 min_split 采样解耦，互不污染窗口边界）。
    let mut ramp_last_bytes = total_downloaded.load(Ordering::Relaxed);
    let mut ramp_last_throttle_waits = speed_limiter.throttle_waits();
    let mut ramp_last_time = Instant::now();
    let mut ramp_interval = tokio::time::interval(Duration::from_secs(RAMP_TICK_SECS));
    ramp_interval.tick().await; // consume the immediate first tick

    // UI 进度快照：coordinator 单点每 UI_REPORT_INTERVAL_MS 汇总 seg_states，
    // 取代各 worker 内 200ms 分散上报（字段语义与原 worker 发送一致）。
    let mut ui_interval =
        tokio::time::interval(Duration::from_millis(UI_REPORT_INTERVAL_MS as u64));
    ui_interval.tick().await; // consume the immediate first tick

    // 段进度批量落库：抽干 durable_progress，一次事务写入（与 worker 周期对齐）。
    let mut db_flush_interval = tokio::time::interval(Duration::from_secs(DB_SAVE_INTERVAL_SECS));
    db_flush_interval.tick().await; // consume the immediate first tick

    'coordinator: loop {
        tokio::select! {
            biased;

            _ = cancel_token.cancelled() => {
                for tx in &mut worker_assign_txs {
                    *tx = None;
                }
                final_error = Some(DownloadError::Cancelled);
                break;
            }

            event = event_rx.recv() => {
                match event {
                    Some(WorkerEvent::Retrying { seg_index, attempt, error }) => {
                        if let Err(e) = crate::task_activity::record(
                            db, sink, task_id, "retry",
                            format!("HTTP 分段 {seg_index} 第 {attempt} 次重试：{error}"), Some(1),
                        ).await {
                            tracing::error!(task_id, error = %e, "failed to persist HTTP segment retry");
                        }
                    }
                    Some(WorkerEvent::Done { worker_id, seg_index, downloaded_bytes }) => {
                        if open_ended_streaming == Some(seg_index) {
                            open_ended_streaming = None;
                        }
                        // Mark segment completed in our authoritative map.
                        // 合并竞态兜底：开放式首段被 merge 扩容的瞬间，worker 可能
                        // 已在【旧边界】break 并上报 Done——downloaded < size。直接
                        // 标 Completed 会把未下载的尾部当成已完成（静默空洞）。此时
                        // 把余量拆成新 Pending 段、本段收缩为已下区间后标记完成。
                        let mut shortfall_child: Option<i32> = None;
                        if let Some(seg) = segments.get_mut(&seg_index) {
                            if downloaded_bytes > 0 && downloaded_bytes < seg.size() {
                                let rem_start = seg.start_byte + downloaded_bytes;
                                let rem_end = seg.end_byte;
                                seg.end_byte = rem_start - 1;
                                seg.downloaded_bytes = downloaded_bytes;
                                seg.state = SegState::Completed;
                                let child_idx = next_index;
                                next_index += 1;
                                segments.insert(child_idx, LiveSegment {
                                    index: child_idx,
                                    start_byte: rem_start,
                                    end_byte: rem_end,
                                    downloaded_bytes: 0,
                                    state: SegState::Pending,
                                    rate_bps: None,
                                });
                                shortfall_child = Some(child_idx);
                            } else {
                                // Cap downloaded_bytes to segment size: a worker may
                                // have written one chunk past the split boundary before
                                // seg_states reflected the shrunk end_byte.  Clamping
                                // here keeps the coordinator's total accurate.
                                seg.downloaded_bytes = downloaded_bytes.min(seg.size());
                                seg.state = SegState::Completed;
                            }
                        }
                        if let Some(child_idx) = shortfall_child {
                            log_info!(
                                "[coordinator] task {} seg {} Done 短量（{} < 段大小），余量拆回 Pending 段 #{}",
                                task_id,
                                seg_index,
                                downloaded_bytes,
                                child_idx
                            );
                            if let Err(error) = persist_segment_change(
                                db, task_id, &segments, child_idx, Some(seg_index),
                            ).await {
                                worker_cancel.cancel();
                                for tx in &mut worker_assign_txs { *tx = None; }
                                final_error = Some(error);
                                break 'coordinator;
                            }
                            send_split_event(
                                sink, task_id, seg_index, child_idx, &segments, false, scope,
                            );
                            rebuild_seg_states(&segments, &seg_states);
                        }

                        // Sync the coordinator's view of active segments'
                        // downloaded_bytes from the shared state (updated by
                        // workers in real-time) so that split-point calculations
                        // use current data, not stale initial values.
                        sync_downloaded_from_shared(&mut segments, &seg_states);

                        // Try to assign new work to this worker.
                        // 串行模式下：同一时刻只允许一个 worker 工作，
                        // 且不进行分段拆分（避免产生新的并发连接）。
                        // 动态计算最小拆分阈值：根据最近的实时吞吐量调整，
                        // 低速时更积极拆分，高速时保守避免 HTTP 请求开销。
                        {
                            let now = Instant::now();
                            let elapsed = now.duration_since(last_throughput_time);
                            if elapsed.as_millis() >= 500 {
                                let current_bytes = total_downloaded.load(Ordering::Relaxed);
                                let delta = (current_bytes - last_throughput_bytes).max(0) as f64;
                                let throughput = delta / elapsed.as_secs_f64();
                                current_min_split = dynamic_min_split_bytes(throughput);
                                let transferring = segments
                                    .values()
                                    .filter(|s| s.state == SegState::Active)
                                    .count();
                                conn_bps_hint = (throughput > 0.0 && transferring > 0)
                                    .then(|| throughput / transferring as f64);
                                last_throughput_bytes = current_bytes;
                                last_throughput_time = now;
                            }
                        }

                        let next_work = if serial_mode {
                            let other_active =
                                segments.values().any(|s| s.state == SegState::Active);
                            if other_active {
                                // 还有其他 worker 在下载 → 退休当前 worker，等它完成
                                None
                            } else {
                                // 无其他活跃连接 → 取一个 Pending 分段（不拆分）
                                find_next_pending_only(&mut segments)
                            }
                        } else if worker_assign_txs.iter().filter(|tx| tx.is_some()).count()
                            > allowed_workers
                        {
                            // 乘性降级后的【自然退休】：存活 worker 数超出当前额度，
                            // 本 worker 完成当前段后不再领新活——绝不 cancel 进行中
                            // 的连接，已下载进度零丢弃。
                            None
                        } else {
                            find_next_work(
                                &mut segments,
                                &mut next_index,
                                effective_total_bytes,
                                current_min_split,
                                split_cost(&request_path, conn_bps_hint, split_window),
                            )
                        };

                        if let Some(next) = next_work {
                            let new_seg_idx = next.assignment.seg_index;

                            // Persist new/updated segments to DB.
                            if let Err(error) = persist_segment_change(
                                db, task_id, &segments,
                                new_seg_idx, next.split_parent,
                            ).await {
                                worker_cancel.cancel();
                                for tx in &mut worker_assign_txs { *tx = None; }
                                final_error = Some(error);
                                break 'coordinator;
                            }

                            // Notify host about the split event (if this came from a split).
                            if let Some(parent_idx) = next.split_parent {
                                send_split_event(
                                    sink, task_id, parent_idx, new_seg_idx,
                                    &segments, false, scope,
                                );
                            }

                            // Update shared visualization state (segment count
                            // or ranges may have changed due to split).
                            rebuild_seg_states(&segments, &seg_states);

                            if let Some(Some(tx)) = worker_assign_txs.get(worker_id)
                                && tx.send(next.assignment).await.is_err() {
                                    // Worker died — reclaim segment.
                                    if let Some(seg) = segments.get_mut(&new_seg_idx) {
                                        seg.state = SegState::Pending;
                                    }
                                }
                        } else {
                            // No more work — retire this worker.
                            if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                                *slot = None;
                            }
                        }

                        // Check if all segments are done.
                        if all_done(&segments) {
                            for tx in &mut worker_assign_txs {
                                *tx = None;
                            }
                            break;
                        }
                    }

                    Some(WorkerEvent::Failed { worker_id, seg_index, error }) => {
                        // 段失败即其响应流已断；若正是开放式首段则解除跟踪
                        //（重派 assignment 均为闭区间，不再具备开放式语义）。
                        if open_ended_streaming == Some(seg_index) {
                            open_ended_streaming = None;
                        }
                        // --- 就地扩容：服务器自报真实大小 > 当前规划 ----------
                        // （BUG-HTTP-HINT-UNDERSIZED）hint 偏小/文件在下载中增长。
                        // 处理方式是【就地扩容】而非清空重下：延长预分配 → 追加
                        // 尾段 [旧 total, 真实 total) → 更新共享 planned_total →
                        // 给仍存活的 worker 重新派工。已下数据全部保留（区间对齐、
                        // 版本一致由 do_segment 守卫），代价与文件体积无关。
                        //
                        // 并发重复报告：两个 worker 的 206 可能同时携带同一个更大
                        // 分母，第二个 Failed 到达时扩容已完成（reported <=
                        // effective）——按 stale 处理：不烧配额、不进致命分支，
                        // 仅把失败段回 Pending 并重新派工（worker 重试时会从共享
                        // planned_total 读到新值，不再误报）。
                        if let DownloadError::TrueSizeLarger(reported) = &error {
                            let reported_total = *reported;
                            if reported_total > effective_total_bytes {
                                if expansions >= MAX_SIZE_EXPANSIONS {
                                    // 配额耗尽：文件仍在增长/病态分母膨胀。显式
                                    // 失败（fail-loud），但【不清数据】——DB 段行
                                    // 与临时文件保留，重试时 resume 重新 probe。
                                    log_info!(
                                        "[coordinator] task {} 就地扩容已达上限 {}（服务器仍自报更大 \
                                         {} > {}），文件持续增长/分母异常，任务显式失败（数据保留可续）",
                                        task_id,
                                        MAX_SIZE_EXPANSIONS,
                                        reported_total,
                                        effective_total_bytes
                                    );
                                    worker_cancel.cancel();
                                    if let Some(seg) = segments.get_mut(&seg_index) {
                                        seg.state = SegState::Pending;
                                    }
                                    for tx in &mut worker_assign_txs {
                                        *tx = None;
                                    }
                                    final_error =
                                        Some(DownloadError::TrueSizeLarger(reported_total));
                                    break;
                                }
                                // 物理扩容临时文件（逻辑 EOF + 尽量物理分配）。
                                // 失败（如 ENOSPC）是致命错误：停 workers 上报。
                                // GrowOnWrite 布局不扩容：尾段写入时文件自然增长。
                                if file_layout.preallocates()
                                    && let Err(e) = preallocate_file_len(
                                        dest,
                                        reported_total as u64,
                                        &temp_file_guard,
                                    )
                                    .await
                                {
                                    log_info!(
                                        "[coordinator] task {} 就地扩容预分配失败: {}",
                                        task_id,
                                        e
                                    );
                                    worker_cancel.cancel();
                                    if let Some(seg) = segments.get_mut(&seg_index) {
                                        seg.state = SegState::Pending;
                                    }
                                    for tx in &mut worker_assign_txs {
                                        *tx = None;
                                    }
                                    final_error = Some(e);
                                    break;
                                }
                                expansions += 1;
                                let old_total = effective_total_bytes;
                                let tail_idx = next_index;
                                next_index += 1;
                                segments.insert(
                                    tail_idx,
                                    LiveSegment {
                                        index: tail_idx,
                                        start_byte: old_total,
                                        end_byte: reported_total - 1,
                                        downloaded_bytes: 0,
                                        state: SegState::Pending,
                                        rate_bps: None,
                                    },
                                );
                                effective_total_bytes = reported_total;
                                planned_total.store(reported_total, Ordering::Relaxed);
                                if let Err(error) = db.upsert_segment(task_id, tail_idx, old_total, reported_total - 1, 0).await {
                                    worker_cancel.cancel();
                                    for tx in &mut worker_assign_txs { *tx = None; }
                                    final_error = Some(error.into());
                                    break 'coordinator;
                                }
                                if scope.owns_task_total
                                    && let Err(error) = db
                                        .update_task_total_bytes(task_id, reported_total)
                                        .await {
                                        worker_cancel.cancel();
                                        for tx in &mut worker_assign_txs { *tx = None; }
                                        final_error = Some(error.into());
                                        break;
                                    }
                                rebuild_seg_states(&segments, &seg_states);
                                log_info!(
                                    "[coordinator] task {} 就地扩容（第 {}/{} 次）：规划 {} -> 服务器自报 \
                                     {}，追加尾段 #{} [{}, {}]，已下数据零丢弃",
                                    task_id,
                                    expansions,
                                    MAX_SIZE_EXPANSIONS,
                                    old_total,
                                    reported_total,
                                    tail_idx,
                                    old_total,
                                    reported_total - 1
                                );
                            } else {
                                log_info!(
                                    "[coordinator] task {} seg {} 滞后的 TrueSizeLarger（{} <= 已扩容 \
                                     {}），按 stale 重派",
                                    task_id,
                                    seg_index,
                                    reported_total,
                                    effective_total_bytes
                                );
                            }
                            // 失败段回 Pending，并给仍存活的 worker 重新派工
                            // （worker 对 TrueSizeLarger 保活等待新 assignment）。
                            if let Some(seg) = segments.get_mut(&seg_index) {
                                seg.state = SegState::Pending;
                            }
                            let verdict_ok = range_verdict.load(Ordering::Relaxed)
                                == RANGE_VERDICT_SUPPORTED;
                            let next_work = if !verdict_ok {
                                // Range 裁决未确认支持（hint 首枪 plain GET 抛出
                                // TrueSizeLarger 的路径）：绝不生成带 Range 的重派工
                                // ——配额型端点会被 bounded Range 作废 token。只允许
                                // 「从 0 起的零进度段」保持 plain GET 语义重飞；其余
                                // 段放回 Pending（该裁决下 ramp/proactive 均被门控，
                                // 若因此无工可派，存活性兜底会 fail-loud 保数据）。
                                match find_next_pending_only(&mut segments) {
                                    Some(mut nw) if nw.assignment.actual_start == 0 => {
                                        nw.assignment.open_ended = true;
                                        nw.assignment.no_range = true;
                                        Some(nw)
                                    }
                                    Some(nw) => {
                                        if let Some(seg) =
                                            segments.get_mut(&nw.assignment.seg_index)
                                        {
                                            seg.state = SegState::Pending;
                                        }
                                        None
                                    }
                                    None => None,
                                }
                            } else if serial_mode {
                                let other_active = segments.values()
                                    .any(|s| s.state == SegState::Active);
                                if other_active {
                                    None
                                } else {
                                    find_next_pending_only(&mut segments)
                                }
                            } else {
                                find_next_work(
                                    &mut segments,
                                    &mut next_index,
                                    effective_total_bytes,
                                    current_min_split,
                                    split_cost(&request_path, conn_bps_hint, split_window),
                                )
                            };
                            if let Some(next) = next_work {
                                let new_seg_idx = next.assignment.seg_index;
                                let redispatch_open = next.assignment.open_ended;
                                if let Err(error) = persist_segment_change(
                                    db, task_id, &segments,
                                    new_seg_idx, next.split_parent,
                                ).await {
                                    worker_cancel.cancel();
                                    for tx in &mut worker_assign_txs { *tx = None; }
                                    final_error = Some(error);
                                    break 'coordinator;
                                }
                                if let Some(parent_idx) = next.split_parent {
                                    send_split_event(
                                        sink, task_id, parent_idx, new_seg_idx,
                                        &segments, false, scope,
                                    );
                                }
                                rebuild_seg_states(&segments, &seg_states);
                                if let Some(Some(tx)) = worker_assign_txs.get(worker_id) {
                                    if tx.send(next.assignment).await.is_err() {
                                        if let Some(seg) = segments.get_mut(&new_seg_idx) {
                                            seg.state = SegState::Pending;
                                        }
                                    } else if redispatch_open {
                                        // plain/开放式语义的重飞段重新成为可吸收的
                                        // 生命线（初始派工同款跟踪）。
                                        open_ended_streaming = Some(new_seg_idx);
                                    }
                                }
                            } else if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                                *slot = None;
                            }
                            continue;
                        }
                        // --- 多 CDN 节点可归因失败：回收段重派 ----------------
                        // worker 已在钉定租约上把节点可归因错误翻译为
                        // CdnNodeFailed（SYS 租约错误不经翻译，不会到达此臂），
                        // 且 NodePool 已完成降权/踢除——此处只需把段回 Pending
                        // 并重新派工：下一次租借自然避开劣质节点。节点池全灭时
                        // 租约退 SYS，其错误走原语义分支——本臂绝不可能把任务
                        // 拖入无限循环。派工逻辑与 TrueSizeLarger 重派完全同款
                        //（verdict / 串行模式门控一致）。
                        if let DownloadError::CdnNodeFailed(msg) = &error {
                            log_info!(
                                "[coordinator] task {} seg {} CDN 节点失败，回收重派: {}",
                                task_id,
                                seg_index,
                                msg
                            );
                            if let Some(seg) = segments.get_mut(&seg_index) {
                                seg.state = SegState::Pending;
                            }
                            let verdict_ok = range_verdict.load(Ordering::Relaxed)
                                == RANGE_VERDICT_SUPPORTED;
                            let next_work = if !verdict_ok {
                                match find_next_pending_only(&mut segments) {
                                    Some(mut nw) if nw.assignment.actual_start == 0 => {
                                        nw.assignment.open_ended = true;
                                        nw.assignment.no_range = true;
                                        Some(nw)
                                    }
                                    Some(nw) => {
                                        if let Some(seg) =
                                            segments.get_mut(&nw.assignment.seg_index)
                                        {
                                            seg.state = SegState::Pending;
                                        }
                                        None
                                    }
                                    None => None,
                                }
                            } else if serial_mode {
                                let other_active = segments.values()
                                    .any(|s| s.state == SegState::Active);
                                if other_active {
                                    None
                                } else {
                                    find_next_pending_only(&mut segments)
                                }
                            } else {
                                find_next_work(
                                    &mut segments,
                                    &mut next_index,
                                    effective_total_bytes,
                                    current_min_split,
                                    split_cost(&request_path, conn_bps_hint, split_window),
                                )
                            };
                            if let Some(next) = next_work {
                                let new_seg_idx = next.assignment.seg_index;
                                let redispatch_open = next.assignment.open_ended;
                                if let Err(error) = persist_segment_change(
                                    db, task_id, &segments,
                                    new_seg_idx, next.split_parent,
                                ).await {
                                    worker_cancel.cancel();
                                    for tx in &mut worker_assign_txs { *tx = None; }
                                    final_error = Some(error);
                                    break 'coordinator;
                                }
                                if let Some(parent_idx) = next.split_parent {
                                    send_split_event(
                                        sink, task_id, parent_idx, new_seg_idx,
                                        &segments, false, scope,
                                    );
                                }
                                rebuild_seg_states(&segments, &seg_states);
                                if let Some(Some(tx)) = worker_assign_txs.get(worker_id) {
                                    if tx.send(next.assignment).await.is_err() {
                                        if let Some(seg) = segments.get_mut(&new_seg_idx) {
                                            seg.state = SegState::Pending;
                                        }
                                    } else if redispatch_open {
                                        open_ended_streaming = Some(new_seg_idx);
                                    }
                                } else if let Some(seg) = segments.get_mut(&new_seg_idx) {
                                    // worker 槽位已退休（竞态防御）：段回 Pending
                                    // 由存活 worker 的 Done 流程拾取。
                                    seg.state = SegState::Pending;
                                }
                            } else if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                                // 无工可派（串行模式他段在跑等）：退休本 worker，
                                // Pending 段由存活 worker 拾取（与既有重派同款）。
                                *slot = None;
                            }
                            continue;
                        }

                        // --- 单段瞬时失败隔离 ------------------------------------
                        // 段内重试预算耗尽的瞬时错误（断流 / 停滞 / 连接重置 / 5xx，
                        // 判据与任务级自动重试同源）而其它已验证响应仍在传 body：
                        // 是这一条连接 / 这一跳链路坏了，不是服务器不可用。把段放回
                        // Pending、退休本 worker（下个 ramp tick 按额度补一个新连接），
                        // 其余连接照常下载。旧逻辑直接整任务失败，再由管理器自动重试
                        // 从 4 条连接重新爬坡——代理链路抖动时速度呈锯齿。每段最多
                        // 隔离 SEGMENT_TRANSIENT_REQUEUE_MAX 次，超出仍按原语义上抛。
                        if !serial_mode
                            && !reconnect_hostile.load(Ordering::Relaxed)
                            && range_verdict.load(Ordering::Relaxed) == RANGE_VERDICT_SUPPORTED
                            && !is_server_rejection(&error)
                            && !is_http_400(&error)
                            && generation_evidence.has_active_response()
                            // 本地 I/O / DB 失败走的是不做检查点的 `?` 出口：共享进度可能
                            // 领先于真正落入文件的字节（BufWriter 未刷即丢弃），按共享进度
                            // 重派会留下空洞。这类错误也不是链路问题，交原语义处置。
                            && !matches!(error, DownloadError::Io(_) | DownloadError::Db(_))
                            && crate::download_manager::is_retriable_error(
                                &crate::downloader::download_error_chain_text(&error),
                            )
                        {
                            let used = transient_requeues.entry(seg_index).or_insert(0);
                            if *used < SEGMENT_TRANSIENT_REQUEUE_MAX {
                                *used += 1;
                                log_info!(
                                    "[coordinator] task {} seg {} 瞬时失败隔离（第 {}/{} 次）：\
                                     其它连接仍在传输，段回队列由新连接接手：{}",
                                    task_id,
                                    seg_index,
                                    *used,
                                    SEGMENT_TRANSIENT_REQUEUE_MAX,
                                    error
                                );
                                sync_downloaded_from_shared(&mut segments, &seg_states);
                                if let Some(seg) = segments.get_mut(&seg_index) {
                                    seg.rate_bps = None;
                                    seg.state = if seg.remaining() == 0 {
                                        SegState::Completed
                                    } else {
                                        SegState::Pending
                                    };
                                }
                                if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                                    *slot = None;
                                }
                                if all_done(&segments) {
                                    for tx in &mut worker_assign_txs {
                                        *tx = None;
                                    }
                                    break;
                                }
                                continue;
                            }
                        }

                        // 失败处置分三类：
                        //   (1) 403/429/400 拒绝，且本 generation 还有其它请求在途；
                        //   (2) 任务已有可保留 Range 进度后偶发 200 全量响应；
                        //   (3) 当前 URL 全部拒绝、真·无 Range 或其它致命错误。
                        // (1)(2) 走串行降级：保数据、退休失败 worker、保活其它 worker；
                        // (3) 走 Path B：仅停 workers 并上报错误。
                        let is_range_err =
                            matches!(error, DownloadError::RangeNotSupported(_));
                        // 是否存在任何可保留进度；这里允许历史进度参与，只用于决定
                        // 重试/清盘，不用于学习跨任务域名连接上限。
                        let any_data = total_downloaded.load(Ordering::Relaxed) > 0;
                        // Active 只代表当前 generation 请求仍在途；Completed 是历史
                        // 状态，绝不能作为“当前还有其它连接”的证据。
                        let other_in_flight = has_other_in_flight(&segments, seg_index);

                        // Range 曾工作过却收到 RangeNotSupported，可按瞬时 200 处理以
                        // 保留进度；是否写域名 cap 仍由存活的已验证响应单独门控。
                        let transient_range = is_range_err && any_data;
                        let server_rejection = is_server_rejection(&error);
                        // 400 可参与当前任务降级，但语义宽泛，永不写域名连接缓存。
                        let conn_rejection = server_rejection || is_http_400(&error);
                        // ---- 全员 429 后单连接复活 ----
                        // 最后一条在途连接也被 429（按 IP 的请求速率 / 并发窗口），但已有
                        // 进度：限流窗口过去后单连接通常就能继续（USTC 实测全员 429 后
                        // 单连接 100 MB/s）。退避后由 ramp tick 派一个 worker 续传，而不是
                        // 立刻失败——那会让 manager 隔 5s 从头爬坡重来。复活次数有上限，
                        // 用完仍被拒才走下方原有的致命出口。
                        if is_http_429(&error)
                            && any_data
                            && !other_in_flight
                            && revives_used < ALL_REJECTED_REVIVE_MAX
                        {
                            revives_used += 1;
                            let delay = ALL_REJECTED_REVIVE_BASE_DELAY * revives_used;
                            serial_mode = true;
                            freeze = FreezeState::Hard;
                            awaiting_ramp_eval = false;
                            probe_window = false;
                            if let Some(seg) = segments.get_mut(&seg_index) {
                                seg.rate_bps = None;
                                seg.state = SegState::Pending;
                            }
                            if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                                *slot = None;
                            }
                            revive_at = Some(Instant::now() + delay);
                            log_info!(
                                "[coordinator] task {} seg {} 全部连接被 429，{:?} 后单连接复活（{}/{}）",
                                task_id,
                                seg_index,
                                delay,
                                revives_used,
                                ALL_REJECTED_REVIVE_MAX
                            );
                            continue;
                        }
                        if (conn_rejection && other_in_flight) || transient_range {
                            // ---- 分级自适应降级 ----
                            // 第 1 次拒绝且存活连接充足：乘性减半连接额度，冻结扩容，
                            // 保留所有存活连接（超额部分完成当前段后自然退休）；
                            // 再次拒绝（或本就只有 <=2 条连接）才降到串行模式。
                            let alive = worker_assign_txs
                                .iter()
                                .filter(|tx| tx.is_some())
                                .count();
                            // 降额后仍在途的超额连接（同一扩容批次一起被拒的请求）
                            // 被拒，只是在兑现上一次降额，不是「降额后仍被拒」的新
                            // 证据：只退休本 worker，不再升级为串行 / 单连接缓存。
                            // 实测 Hetzner 每 IP 限 5 连接：4→8 扩容的 4 条新连接
                            // 几乎同时 429，旧逻辑第 2 条就把任务打成串行并写入
                            // 24h 单连接缓存，速度从 4 连接跌到 1 连接。
                            // reject_quota 只在降额 / 解封回退后才小于 usize::MAX。
                            let shedding_excess =
                                !transient_range && !serial_mode && alive > reject_quota;
                            // 解封越过 advisor 天花板后的首个拒绝：越界的是本任务自己
                            // 加出来的连接（实测 USTC 16 条正常、16→32 后 429），回退到
                            // 解封前的天花板，不算拒绝、不学习域名上限。
                            let overshoot_floor = uncap_floor.filter(|&floor| {
                                !shedding_excess && !transient_range && !serial_mode && alive > floor
                            });
                            if !shedding_excess && overshoot_floor.is_none() {
                                reject_strikes += 1;
                            }
                            freeze = FreezeState::Hard;
                            awaiting_ramp_eval = false;
                            probe_window = false;
                            let reason = if transient_range {
                                "transient-200"
                            } else {
                                "server-rejection"
                            };
                            if let Some(floor) = overshoot_floor {
                                worker_cap = floor;
                                allowed_workers = floor;
                                reject_quota = floor;
                                uncap_floor = None;
                                uncap_disabled = true;
                                log_info!(
                                    "[adaptive] task {} seg {} 解封越界被拒：{} alive -> 回退到解封前上限 {}",
                                    task_id,
                                    seg_index,
                                    alive,
                                    floor
                                );
                            } else if shedding_excess {
                                log_info!(
                                    "[adaptive] task {} seg {} rejected while shedding excess \
                                     connections ({} alive > {} allowed); retiring worker only",
                                    task_id,
                                    seg_index,
                                    alive,
                                    allowed_workers
                                );
                            } else if !serial_mode && reject_strikes == 1 && alive > 2 {
                                allowed_workers = (alive / 2).max(1);
                                reject_quota = allowed_workers;
                                log_info!(
                                    "[adaptive] task {} ramp down: {} -> {}, reason={}, \
                                     preserve_active=true",
                                    task_id,
                                    alive,
                                    allowed_workers,
                                    reason
                                );
                                // 仅当拒绝发生时有另一个【已验证响应】仍在传 body，
                                // 才能证明是连接压力而非签名 URL 整体失效。
                                if generation_evidence.can_learn_conn_cap(server_rejection) {
                                    record_domain_conn_cap_persist(
                                        url,
                                        allowed_workers as i32,
                                        db,
                                    );
                                }
                            } else if !serial_mode {
                                // ---- 降级为串行模式 ----
                                log_info!(
                                    "[coordinator] task {} seg {} 降级为串行模式 (reason={})",
                                    task_id,
                                    seg_index,
                                    reason
                                );
                                serial_mode = true;
                                // 同上：只凭历史 Completed 段或其它尚未收到响应的
                                // Active 请求，不能把整个域名投毒成 24h 单连接。
                                if generation_evidence.can_learn_conn_cap(server_rejection) {
                                    record_single_conn_domain_persist(url, db);
                                }
                            }

                            // 将失败分段标记为 Pending，等待串行下载
                            if let Some(seg) = segments.get_mut(&seg_index) {
                                seg.state = SegState::Pending;
                            }

                            // ---- 开放式首段吸收（连接配额自救）----
                            // 服务器拒绝新连接，但开放式首段的响应流仍活着：把
                            // 右侧【字节连续且零进度】的 Pending 段全部并入该段，
                            // 复用这条已建立的流下完整个文件，无需任何新请求。
                            // 这是配额型端点（fnOS multiple-download）唯一能下完
                            // 的方式。见 absorb_pending_into_open_stream。
                            absorb_pending_into_open_stream(
                                db,
                                task_id,
                                &mut segments,
                                &seg_states,
                                open_ended_streaming,
                                &reconnect_hostile,
                            )
                            .await;

                            // 退休当前失败的 worker（关闭其分配通道）
                            if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                                *slot = None;
                            }

                            // 安全检查：如果所有 worker 都已退休且无活跃分段，
                            // 说明服务器甚至拒绝单连接 → 多段路线无法继续。
                            let any_alive = worker_assign_txs.iter().any(|tx| tx.is_some())
                                || segments.values().any(|s| s.state == SegState::Active);
                            if !any_alive && !all_done(&segments) {
                                // 归一为 RangeNotSupported 上抛（run_download_inner
                                // 捕获后清盘回退单流 plain GET）的两种情形：
                                //   • any_data==0：从未有任何段拿到过 206/写入数据，
                                //     磁盘无可保留进度。Range 敌对的配额型端点
                                //     （fnOS multiple-download 对带 Range 的请求回
                                //     400，仅裸 GET 可用）唯一能下完的方式。
                                //   • 触发错误本身是 RangeNotSupported（瞬时 200
                                //     家族且重试打满）**且已下进度可忽略**（<
                                //     RANGE_FAMILY_DISCARD_MAX）：服务器持续对
                                //     Range 回 200 全量——plain GET 必然可用，丢弃
                                //     少量已下数据换一次性下完。
                                // 其余 any_data>0 的失败保留致命错误（fail-loud +
                                // 数据保留）：403/429/400 是配额/并发问题，清盘重下
                                // 反丢进度；深进度大文件即便是 200 家族也绝不清盘
                                // ——resume 重新 probe 后仍有机会多段续传，无条件
                                // 清盘会把 90% 进度的 4GB 文件整个重下。
                                let downloaded = total_downloaded.load(Ordering::Relaxed);
                                let range_family =
                                    matches!(error, DownloadError::RangeNotSupported(_));
                                let discard_ok = downloaded < RANGE_FAMILY_DISCARD_MAX;
                                final_error = Some(if !any_data || (range_family && discard_ok) {
                                    DownloadError::RangeNotSupported(format!(
                                        "every connection rejected ({error})"
                                    ))
                                } else {
                                    DownloadError::Other(format!(
                                        "服务器拒绝所有下载连接（包括单连接），无法继续下载：{error}"
                                    ))
                                });
                                break;
                            }

                            // 不 break、不 cancel — 让已建立连接的 active workers
                            // 继续下载。当它们完成后，Done 事件会触发串行分配剩余
                            // Pending 分段。
                        } else {
                            // Path B：真·无 Range 或其它致命错误。
                            // 只 cancel【子令牌】停 workers，【绝不】cancel 主令牌——
                            // run_download_inner 捕获 RangeNotSupported 后要用【存活的
                            // 主令牌】回退 download_single 单流；cancel 主令牌会让回退
                            // 瞬间命中 cancelled() 一个字节都下不了 → 任务永久卡死
                            // （历史致命 BUG）。真·无 Range 时 do_segment 已按
                            // any_data==0 记录主机单连接缓存，此处无需再记。
                            worker_cancel.cancel();
                            if let Some(seg) = segments.get_mut(&seg_index) {
                                seg.state = SegState::Pending;
                            }
                            for tx in &mut worker_assign_txs {
                                *tx = None;
                            }
                            if final_error.is_none() {
                                // 连接级拒绝（400/403/429）且从未下到任何字节：与上方
                                // "全员被拒"出口同理，归一为 RangeNotSupported 触发
                                // 单流 plain GET 回退（16 连接同时被拒时，最后一个失败
                                // 事件看到的其余段全是 Pending，走的是本分支而非上方）。
                                final_error = Some(if conn_rejection && !any_data {
                                    DownloadError::RangeNotSupported(format!(
                                        "every connection rejected, no byte ever served: {error}"
                                    ))
                                } else {
                                    error
                                });
                            }
                            break;
                        }
                    }

                    Some(WorkerEvent::Preempted { worker_id, seg_index }) => {
                        // 完成时间抢占：worker 已在取消分支刷盘并把进度写入共享
                        // seg_states。剩余字节回 Pending，本 worker 立即领新活——
                        // 新租借按最新路径估计落到最优路径（通常正是剩余字节）。
                        sync_downloaded_from_shared(&mut segments, &seg_states);
                        if let Some(seg) = segments.get_mut(&seg_index) {
                            seg.rate_bps = None;
                            seg.state = if seg.remaining() == 0 {
                                SegState::Completed
                            } else {
                                SegState::Pending
                            };
                        }
                        if all_done(&segments) {
                            for tx in &mut worker_assign_txs {
                                *tx = None;
                            }
                            break;
                        }
                        let next_work = if serial_mode {
                            let other_active =
                                segments.values().any(|s| s.state == SegState::Active);
                            if other_active {
                                None
                            } else {
                                find_next_pending_only(&mut segments)
                            }
                        } else if reject_strikes > 0
                            && worker_assign_txs.iter().filter(|tx| tx.is_some()).count()
                                > allowed_workers
                        {
                            // 拒绝降级后的额度收缩期：本 worker 顺势退休。抢占本身
                            // 是交接不是扩容，其余情况（含探索 worker 暂超额度）
                            // 一律续派，避免余量等待下一个 Done。
                            None
                        } else {
                            find_next_work(
                                &mut segments,
                                &mut next_index,
                                effective_total_bytes,
                                current_min_split,
                                split_cost(&request_path, conn_bps_hint, split_window),
                            )
                        };
                        if let Some(next) = next_work {
                            let new_seg_idx = next.assignment.seg_index;
                            if let Err(error) = persist_segment_change(
                                db, task_id, &segments,
                                new_seg_idx, next.split_parent,
                            ).await {
                                worker_cancel.cancel();
                                for tx in &mut worker_assign_txs { *tx = None; }
                                final_error = Some(error);
                                break 'coordinator;
                            }
                            if let Some(parent_idx) = next.split_parent {
                                send_split_event(
                                    sink, task_id, parent_idx, new_seg_idx,
                                    &segments, false, scope,
                                );
                            }
                            rebuild_seg_states(&segments, &seg_states);
                            if let Some(Some(tx)) = worker_assign_txs.get(worker_id)
                                && tx.send(next.assignment).await.is_err()
                                && let Some(seg) = segments.get_mut(&new_seg_idx)
                            {
                                seg.state = SegState::Pending;
                            }
                        } else if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                            // 无工可派/额度收缩期：退休本 worker，Pending 余量由存活
                            // worker 拾取。
                            *slot = None;
                        }
                    }

                    Some(WorkerEvent::RangeVerdict { supports_range }) => {
                        if supports_range {
                            // 首响应证实 Range 支持（206 或 Accept-Ranges: bytes）：
                            // ramp 裁决门放行，后续 tick 照常扩容多段；持久化标记，
                            // 此后 resume 走正常 probe 路径（写失败仅降级：resume
                            // 会再走一次保守启动，多花一次裁决，无正确性影响）。
                            if let Err(error) = db.set_task_range_verified(task_id, true).await {
                                crate::logger::report_warning("http-coordinator", "persist Range capability", &error);
                            }
                            log_info!(
                                "[coordinator] task {} hint 首响应确认 Range 支持，放行多段扩容",
                                task_id
                            );
                        } else {
                            // 首响应无 Range 能力证据：本任务判定为 Range 敌对/
                            // 未知（配额型端点对 bounded Range 一律 400 且作废
                            // token）。hint 任务规划期只建了一个覆盖全文件的段，
                            // 这条 plain GET 流就是唯一生命线：冻结扩容、抑制
                            // 主动拆分、提高停滞容忍（掐流重连必被拒）。吸收
                            // 调用兜底 resume 等存在多段的边角（通常为 no-op）。
                            log_info!(
                                "[coordinator] task {} hint 首响应无 Range 支持证据，\
                                 保持首段 plain GET 单流下完",
                                task_id
                            );
                            freeze = FreezeState::Hard;
                            conn_sensitive.store(true, Ordering::Relaxed);
                            reconnect_hostile.store(true, Ordering::Relaxed);
                            absorb_pending_into_open_stream(
                                db,
                                task_id,
                                &mut segments,
                                &seg_states,
                                open_ended_streaming,
                                &reconnect_hostile,
                            )
                            .await;
                        }
                    }

                    Some(WorkerEvent::OpenEndedEstablished) => {
                        // open-ended 首响应已通过校验：立即触发一次 ramp pass
                        // 放出机队，不必干等下一个 2s tick。
                        ramp_interval.reset_immediately();
                    }

                    None => {
                        // All workers dropped their event_tx — we're done.
                        break;
                    }
                }
            }

            // --- Proactive split timer ------------------------------------
            // Periodically pre-split the largest active segment to create a
            // Pending segment.  The next worker to finish picks it up via
            // find_next_work → Strategy 1 (Pending), skipping the expensive
            // split + DB-persist that would otherwise block the Done handler.
            _ = proactive_interval.tick() => {
                // conn_sensitive：一旦观察到服务器对 Range 返回非 206（见 do_segment 置位处），
                // 停止【主动拆分】以降低连接 churn（reactive 拆分仍保留做尾段抢救）。
                // range_verdict：hint 任务在首响应裁决前/裁决为不支持时，绝不拆分——
                // 拆分会临时缩小首段共享预算，在「响应头已到、UNSUPPORTED 事件仍在
                // 途中」的窗口里可能让首段 plain GET 流被旧预算提前截断。
                if !serial_mode
                    && !conn_sensitive.load(Ordering::Relaxed)
                    && range_verdict.load(Ordering::Relaxed) == RANGE_VERDICT_SUPPORTED
                    && !all_done(&segments)
                {
                    sync_downloaded_from_shared(&mut segments, &seg_states);
                    // Try proactive split at the normal threshold first; if that fails
                    // (last segment has < current_min_split but >= TAIL_MIN_SPLIT_BYTES
                    // remaining), also try the tail micro threshold so the proactive
                    // timer covers the full range from current_min_split down to 64 KB.
                    let work = try_proactive_split(
                        &mut segments,
                        &mut next_index,
                        current_min_split,
                        split_cost(&request_path, conn_bps_hint, split_window),
                    )
                    .or_else(|| {
                        if current_min_split > TAIL_MIN_SPLIT_BYTES {
                            // Mirror the straggler guard from find_next_work Strategy 3:
                            // only pre-create a pending micro-segment when there is a
                            // genuine outlier (largest active segment ≥ 2 × TAIL_MIN_SPLIT_BYTES).
                            // Pre-splitting equally-small segments would prime idle workers
                            // to cascade-split the tail and retire en-masse at 99%.
                            let max_remaining = segments
                                .values()
                                .filter(|s| s.state == SegState::Active)
                                .map(|s| s.remaining())
                                .max()
                                .unwrap_or(0);
                            if max_remaining >= 2 * TAIL_MIN_SPLIT_BYTES {
                                try_proactive_split(
                                    &mut segments,
                                    &mut next_index,
                                    TAIL_MIN_SPLIT_BYTES,
                                    split_cost(&request_path, conn_bps_hint, split_window),
                                )
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    });
                    if let Some(next) = work {
                        let new_seg_idx = next.assignment.seg_index;
                        if let Err(error) = persist_segment_change(
                            db, task_id, &segments,
                            new_seg_idx, next.split_parent,
                        ).await {
                            worker_cancel.cancel();
                            for tx in &mut worker_assign_txs { *tx = None; }
                            final_error = Some(error);
                            break 'coordinator;
                        }
                        if let Some(parent_idx) = next.split_parent {
                            send_split_event(
                                sink, task_id, parent_idx, new_seg_idx,
                                &segments, true, scope,
                            );
                        }
                        rebuild_seg_states(&segments, &seg_states);
                    }
                }
            }

            // --- Adaptive ramp timer -------------------------------------
            // 渐进启动 + 自适应扩容：每个窗口测一次总吞吐；上次扩容有净收益且
            // 无拒绝/连接敏感信号时倍增连接额度，否则软/硬冻结；随后按额度
            // 补充 worker（优先领 Pending 段，否则拆分最大 Active 段）。
            _ = ramp_interval.tick() => {
                let now = Instant::now();
                let bytes = total_downloaded.load(Ordering::Relaxed);

                // 槽位对账：worker task 可能未经事件通道即消亡（panic 展开、
                // Done 派工时 send 失败但槽位仍为 Some 的历史缝隙）。以
                // JoinHandle::is_finished 为准清掉死槽位，否则 alive 永远
                // 高估，下方存活性兜底永不触发，任务会无错误地永久停滞。
                for (tx, h) in worker_assign_txs.iter_mut().zip(worker_handles.iter()) {
                    if tx.is_some() && h.as_ref().is_none_or(|h| h.is_finished()) {
                        *tx = None;
                    }
                }
                let mut alive = worker_assign_txs.iter().filter(|t| t.is_some()).count();

                // 全员 429 复活：退避到点、无存活 worker 时派一个 worker 领 Pending 段。
                if let Some(at) = revive_at
                    && now >= at
                {
                    revive_at = None;
                    sync_downloaded_from_shared(&mut segments, &seg_states);
                    if alive == 0
                        && let Some(next) = find_next_work(
                            &mut segments,
                            &mut next_index,
                            effective_total_bytes,
                            current_min_split,
                            split_cost(&request_path, conn_bps_hint, split_window),
                        )
                    {
                        let new_seg_idx = next.assignment.seg_index;
                        if let Err(error) = persist_segment_change(
                            db, task_id, &segments,
                            new_seg_idx, next.split_parent,
                        ).await {
                            worker_cancel.cancel();
                            for tx in &mut worker_assign_txs { *tx = None; }
                            final_error = Some(error);
                            break 'coordinator;
                        }
                        rebuild_seg_states(&segments, &seg_states);
                        let worker_id = worker_assign_txs.len();
                        let (assign_tx, handle) = ctx.spawn(worker_id);
                        if assign_tx.try_send(next.assignment).is_err() {
                            if let Some(seg) = segments.get_mut(&new_seg_idx) {
                                seg.state = SegState::Pending;
                            }
                        } else {
                            worker_assign_txs.push(Some(assign_tx));
                            worker_handles.push(Some(handle));
                            alive += 1;
                            log_info!(
                                "[coordinator] task {} 单连接复活：seg {}",
                                task_id,
                                new_seg_idx
                            );
                        }
                    }
                }

                // 短窗口守卫（open-ended 即时放队后常见）：只做对账 + 按当前
                // 额度补队，跳过吞吐采样/评估/shrink/扩容，且不推进 ramp_last_*
                // ——保证下个整窗测速干净。awaiting 顺延到下个整窗消费。
                let dt_ms = now.duration_since(ramp_last_time).as_millis();
                if dt_ms < RAMP_MIN_EVAL_WINDOW_MS {
                    let range_ok =
                        range_verdict.load(Ordering::Relaxed) == RANGE_VERDICT_SUPPORTED;
                    if !serial_mode && !all_done(&segments) && range_ok {
                        while alive < allowed_workers {
                            sync_downloaded_from_shared(&mut segments, &seg_states);
                            let Some(next) = find_next_work(
                                &mut segments,
                                &mut next_index,
                                effective_total_bytes,
                                current_min_split,
                                split_cost(&request_path, conn_bps_hint, split_window),
                            ) else {
                                break;
                            };
                            let new_seg_idx = next.assignment.seg_index;
                            if let Err(error) = persist_segment_change(
                                db, task_id, &segments,
                                new_seg_idx, next.split_parent,
                            ).await {
                                worker_cancel.cancel();
                                for tx in &mut worker_assign_txs { *tx = None; }
                                final_error = Some(error);
                                break 'coordinator;
                            }
                            if let Some(parent_idx) = next.split_parent {
                                send_split_event(
                                    sink, task_id, parent_idx, new_seg_idx,
                                    &segments, false, scope,
                                );
                            }
                            rebuild_seg_states(&segments, &seg_states);
                            let worker_id = worker_assign_txs.len();
                            let (assign_tx, handle) = ctx.spawn(worker_id);
                            if assign_tx.try_send(next.assignment).is_err() {
                                if let Some(seg) = segments.get_mut(&new_seg_idx) {
                                    seg.state = SegState::Pending;
                                }
                                break;
                            }
                            worker_assign_txs.push(Some(assign_tx));
                            worker_handles.push(Some(handle));
                            alive += 1;
                        }
                    }
                    if alive == 0 && revive_at.is_none() && !all_done(&segments) {
                        log_info!(
                            "[coordinator] task {} 所有 worker 已退出但任务未完成，退出事件循环",
                            task_id
                        );
                        break;
                    }
                    continue;
                }

                // 1. 测量本窗口总吞吐（独立采样窗口，与 min_split 采样解耦）。
                let elapsed = now.duration_since(ramp_last_time).as_secs_f64();
                let throughput = if elapsed > 0.0 {
                    (bytes - ramp_last_bytes).max(0) as f64 / elapsed
                } else {
                    0.0
                };
                ramp_last_bytes = bytes;
                // 与吞吐同窗：本窗口内限速器令牌耗尽的等待次数增量。
                let limiter_waits_now = speed_limiter.throttle_waits();
                let limiter_waits_delta =
                    limiter_waits_now.saturating_sub(ramp_last_throttle_waits);
                ramp_last_throttle_waits = limiter_waits_now;
                ramp_last_time = now;

                // Soft 冷却计时：仅完整 tick 累加（短窗口不进此路径）。
                if !awaiting_ramp_eval
                    && let FreezeState::Soft {
                        ref mut ticks_waited,
                        ..
                    } = freeze
                {
                    *ticks_waited = ticks_waited.saturating_add(1);
                }

                // ---- 多路径调度：窗口采样 / 冷路径探索 / 完成时间抢占 ----
                // 连接稳态速率喂路径估计与段 rate_bps（拆分挑选与均衡切分），
                // 慢于最优路径完成时间判据的在途连接在当前字节处被抢占交接。
                let range_ok_now =
                    range_verdict.load(Ordering::Relaxed) == RANGE_VERDICT_SUPPORTED;
                // app-limited 判定：只有限速器真正在节流的窗口，样本才反映
                // 限速器而非链路；限速值高于链路速率时连接并非 app-limited。
                let limiter_throttled = window_app_limited(
                    speed_limiter.limit(),
                    limiter_waits_delta,
                    throughput,
                );
                sync_downloaded_from_shared(&mut segments, &seg_states);
                // 多网卡聚合：后台规划完成后在本窗挂入额外链路（冷路径，随后
                // 由下方探索逻辑各放 1 条真实分段连接实测）。
                multipath.poll_links(&nodes, sink, task_id).await;
                let mp_report = multipath.on_tick(
                    &nodes,
                    &mut segments,
                    now,
                    &multipath::TickGuards {
                        sampling: !limiter_throttled,
                        may_reroute: range_ok_now
                            && !serial_mode
                            && !limiter_throttled
                            && !conn_sensitive.load(Ordering::Relaxed)
                            && !reconnect_hostile.load(Ordering::Relaxed),
                        remaining_total: effective_total_bytes.saturating_sub(bytes),
                        protected_seg: open_ended_streaming,
                    },
                    task_id,
                );
                multipath.publish_route(&nodes, &segments, db, sink, task_id).await;
                // 冷路径探索：额外放出 1 个 worker 承载探索连接（采样即真实
                // 分段下载），不必等既有连接完成大段才轮到新租借。探索片须
                // 足以跨过慢启动窗口（只从剩余 ≥ 2×EXPLORE_MIN_PIECE 的段拆），
                // 且至多超出额度 1 个 worker。
                if mp_report.explore_worker_wanted
                    && alive <= allowed_workers
                    && !all_done(&segments)
                    && let Some(next) = try_split_largest(
                        &mut segments,
                        &mut next_index,
                        2 * crate::path_scheduler::EXPLORE_MIN_PIECE,
                        split_cost(&request_path, conn_bps_hint, split_window),
                    )
                {
                    let new_seg_idx = next.assignment.seg_index;
                    if let Err(error) = persist_segment_change(
                        db, task_id, &segments,
                        new_seg_idx, next.split_parent,
                    ).await {
                        worker_cancel.cancel();
                        for tx in &mut worker_assign_txs { *tx = None; }
                        final_error = Some(error);
                        break 'coordinator;
                    }
                    if let Some(parent_idx) = next.split_parent {
                        send_split_event(
                            sink, task_id, parent_idx, new_seg_idx,
                            &segments, false, scope,
                        );
                    }
                    rebuild_seg_states(&segments, &seg_states);
                    let worker_id = worker_assign_txs.len();
                    let (assign_tx, handle) = ctx.spawn(worker_id);
                    if assign_tx.try_send(next.assignment).is_err() {
                        if let Some(seg) = segments.get_mut(&new_seg_idx) {
                            seg.state = SegState::Pending;
                        }
                    } else {
                        worker_assign_txs.push(Some(assign_tx));
                        worker_handles.push(Some(handle));
                        alive += 1;
                    }
                }
                let quiet_tick = multipath.take_quiet_tick();
                let migrating = mp_report.preempted > 0 || quiet_tick;

                // 正面学习采样：本窗口以 alive 条连接产生了真实吞吐且全程无
                // 拒绝/降级/连接敏感信号 → 该规模已被服务器实际接受。
                if throughput > 0.0
                    && reject_strikes == 0
                    && !serial_mode
                    && !conn_sensitive.load(Ordering::Relaxed)
                    && alive > proven_scale
                {
                    proven_scale = alive;
                }

                // Range 裁决门（hint 模式）：首响应未到（UNKNOWN）或已判不支持
                // （UNSUPPORTED，吸收合并已由 RangeVerdict 事件完成）时，跳过
                // 扩容与补充——裁决前绝不发出任何带 Range 的请求。
                let range_ok =
                    range_verdict.load(Ordering::Relaxed) == RANGE_VERDICT_SUPPORTED;
                if !serial_mode && !all_done(&segments) && range_ok {
                    // 尾部判据：进入尾部即冻结连接额度、短路
                    // 自适应 collapse/shrink/expand，绝不因尾部固有的吞吐下降收缩
                    // 连接或学习域名上限；补 worker 循环仍按当前额度死磕剩余字节。
                    let remaining = (effective_total_bytes - bytes).max(0);
                    let tail = is_tail(remaining, worker_cap, effective_total_bytes);
                    let mut shrunk_this_tick = false;
                    if tail || migrating {
                        // 丢弃待评估的扩容：清 awaiting_ramp_eval 使下方 collapse
                        // 评估短路（不回滚、不写域名缓存、不改 freeze）。路径交接
                        // 期同理——吞吐波动来自改道而非连接额度。
                        awaiting_ramp_eval = false;
                        probe_window = false;
                    }
                    // 2. 评估上一次扩容/试探的效果。
                    if awaiting_ramp_eval {
                        awaiting_ramp_eval = false;
                        let was_probe = probe_window;
                        probe_window = false;
                        let verdict = ramp_verdict(pre_grow_throughput, throughput);
                        if was_probe {
                            // 试探窗：Improved → 恢复 Active；Freeze/Collapse → 回滚
                            // 且**绝不写域名缓存**（试探崩塌 ≠ 真 Collapse）。
                            let attempts = match freeze {
                                FreezeState::Soft { attempts, .. } => attempts,
                                _ => 0,
                            };
                            let cooldown = match freeze {
                                FreezeState::Soft { cooldown, .. } => cooldown,
                                _ => RAMP_SOFT_PROBE_COOLDOWN_TICKS,
                            };
                            let (next, rollback) =
                                soft_probe_eval_transition(verdict, attempts, cooldown);
                            freeze = next;
                            if rollback {
                                let old = allowed_workers;
                                allowed_workers = pre_grow_workers.max(1);
                                beneficial_scale = beneficial_scale.min(allowed_workers);
                                log_info!(
                                    "[adaptive] task {} 软试探回滚：{} -> {}（verdict={:?}）",
                                    task_id,
                                    old,
                                    allowed_workers,
                                    verdict
                                );
                            } else {
                                // Improved：恢复正常倍增，受益规模同步到当前额度。
                                beneficial_scale = allowed_workers;
                                log_info!(
                                    "[adaptive] task {} 软试探成功：{} conns，恢复倍增扩容",
                                    task_id,
                                    allowed_workers
                                );
                            }
                        } else {
                            match verdict {
                                RampVerdict::Improved => {
                                    beneficial_scale = allowed_workers;
                                    let uncapped = in_task_uncap_ceiling(
                                        allow_hint_uncap
                                            && !uncap_disabled
                                            && reject_strikes == 0,
                                        domain_conn_cap(url),
                                        worker_cap,
                                        pre_grow_workers,
                                        allowed_workers,
                                        pre_grow_throughput,
                                        throughput,
                                        remaining,
                                    );
                                    let uncapped = spec
                                        .connection_cap()
                                        .map_or(uncapped, |l| uncapped.min(l.max(1)));
                                    if uncapped > worker_cap {
                                        log_info!(
                                            "[adaptive] task {} 任务内解封：{} -> {} conns \
                                             上限（{} -> {} 连接吞吐 {:.0} -> {:.0} B/s，\
                                             近线性增益）",
                                            task_id,
                                            worker_cap,
                                            uncapped,
                                            pre_grow_workers,
                                            allowed_workers,
                                            pre_grow_throughput,
                                            throughput
                                        );
                                        uncap_floor.get_or_insert(worker_cap);
                                        worker_cap = uncapped;
                                    }
                                }
                                RampVerdict::Collapse => {
                                    freeze = FreezeState::Hard;
                                    let old = allowed_workers;
                                    allowed_workers = pre_grow_workers.max(1);
                                    beneficial_scale = beneficial_scale.min(allowed_workers);
                                    // 多余 worker 完成当前段后经 Done 派工的额度检查
                                    // 自然退休；进行中的连接与已下数据零丢弃。
                                    // 备选路径承载过流量时连接规模混有代理连接，
                                    // 不学习为源站域名上限。
                                    if !multipath.alternates_used(&nodes) {
                                        record_domain_conn_cap_persist(
                                            url,
                                            allowed_workers as i32,
                                            db,
                                        );
                                    }
                                    log_info!(
                                        "[adaptive] task {} ramp collapse rollback: {} -> {} \
                                         conns, throughput {:.0} -> {:.0} B/s（回滚并记入域名上限）",
                                        task_id,
                                        old,
                                        allowed_workers,
                                        pre_grow_throughput,
                                        throughput
                                    );
                                }
                                RampVerdict::Freeze => {
                                    // 探索期：起步 hint 已被越过时回滚到扩容前规模
                                    // （多余 worker 自然退休），不写域名缓存。
                                    // fresh ramp（无 hint 或未越过 hint）停在扩容后规模。
                                    if start_hint.is_some_and(|h| pre_grow_workers >= h) {
                                        let old = allowed_workers;
                                        allowed_workers = pre_grow_workers.max(1);
                                        beneficial_scale =
                                            beneficial_scale.min(allowed_workers);
                                        log_info!(
                                            "[adaptive] task {} ramp freeze 回滚：{} -> {} \
                                             （pre_grow≥start_hint，不写域名缓存）",
                                            task_id,
                                            old,
                                            allowed_workers
                                        );
                                    }
                                    freeze = freeze_verdict_next_state(soft_probe_task_used);
                                    log_info!(
                                        "[adaptive] task {} ramp freeze at {} conns: throughput \
                                         {:.0} -> {:.0} B/s (no gain, state={:?})",
                                        task_id,
                                        allowed_workers,
                                        pre_grow_throughput,
                                        throughput,
                                        freeze
                                    );
                                }
                            }
                        }
                    }

                    // 2a'. 验证上一次持续劣化收缩：收缩后吞吐没有回升说明劣化
                    //      不是连接数惩罚（实测代理链路噪声让 Hetzner 任务 2 -> 1
                    //      连接后速度更低），撤销收缩并停用本任务的收缩机制。
                    if let Some((pre_workers, shrunk_bps, best_bps, windows)) = shrink_eval {
                        if tail || migrating {
                            shrink_eval = None;
                        } else if alive > allowed_workers {
                            // 收缩尚未生效：超额连接要做完手头的段才退休，这些窗口的
                            // 吞吐仍是旧规模的，不计入验证。
                        } else {
                            let best_bps = best_bps.max(throughput);
                            let windows = windows + 1;
                            match shrink_eval_verdict(shrunk_bps, best_bps, windows) {
                                ShrinkEval::Pending => {
                                    shrink_eval = Some((pre_workers, shrunk_bps, best_bps, windows));
                                }
                                ShrinkEval::Confirmed => shrink_eval = None,
                                ShrinkEval::Revert => {
                                    shrink_eval = None;
                                    shrink_disabled = true;
                                    let old = allowed_workers;
                                    allowed_workers = pre_workers.min(worker_cap).max(allowed_workers);
                                    shrunk_this_tick = true;
                                    log_info!(
                                        "[adaptive] task {} 撤销持续劣化收缩：{} -> {} conns \
                                         （收缩后最高 {:.0} B/s 未超过收缩时 {:.0} B/s，判为链路噪声）",
                                        task_id,
                                        old,
                                        allowed_workers,
                                        best_bps,
                                        shrunk_bps
                                    );
                                }
                            }
                        }
                    }

                    // 2b. 持续劣化收缩：以峰值窗口吞吐为参照。仅在非扩容评估期
                    //     （冻结后或额度已到顶——起步即被惩罚时扩容评估从未发生）
                    //     且存活数恰等于额度时计数：alive < allowed 是下载尾声
                    //     （worker 自然退休），alive > allowed 是回滚/收缩后的
                    //     过渡期（多余连接尚未退光、惩罚尚未解除），两者的低吞吐
                    //     都不是当前额度的责任。收缩不写域名缓存——单窗口证据
                    //     弱于扩容前后的直接对照，误学习会让同域名 24h 内全部降速。
                    //
                    //     峰值随时间衰减：启动瞬间的突发窗口（页缓存/链路 burst）
                    //     不该永久钉死参照系。每 tick 乘 RAMP_PEAK_DECAY，惩罚
                    //     检测只需 2 个窗口（4s），衰减对灵敏度无感。
                    let freeze_for_shrink = !matches!(freeze, FreezeState::Active);
                    peak_throughput *= RAMP_PEAK_DECAY;
                    if throughput > peak_throughput {
                        peak_throughput = throughput;
                        shrink_strikes = 0;
                    } else if !tail
                        && !migrating
                        && !shrink_disabled
                        && shrink_eval.is_none()
                        && should_shrink(
                            throughput,
                            peak_throughput,
                            allowed_workers,
                            worker_cap,
                            alive,
                            freeze_for_shrink,
                            awaiting_ramp_eval,
                        )
                    {
                        shrink_strikes += 1;
                        if shrink_strikes >= RAMP_SHRINK_STRIKES {
                            shrink_strikes = 0;
                            let old = allowed_workers;
                            allowed_workers = (allowed_workers / 2).max(1);
                            beneficial_scale = beneficial_scale.min(allowed_workers);
                            shrunk_this_tick = true;
                            shrink_eval = Some((old, throughput, 0.0, 0));
                            // 收缩后至少进入 Soft 冷却，避免下一完整 tick 立即倍增回
                            // 原额度；冷却/试探期间仍劣化则升级 Hard。
                            freeze = sustained_shrink_next_state(freeze, soft_probe_task_used);
                            log_info!(
                                "[adaptive] task {} sustained degradation shrink: {} -> {} \
                                 conns, throughput {:.0} B/s < peak {:.0} × {:.2}",
                                task_id,
                                old,
                                allowed_workers,
                                throughput,
                                peak_throughput,
                                RAMP_COLLAPSE_FACTOR
                            );
                        }
                    } else {
                        shrink_strikes = 0;
                    }

                    // 3.+4. 补充与扩容（顺序敏感）：先把存活数补齐到当前额度，
                    //    再用【补齐后】的存活数判定扩容/软试探，扩容成功后立刻
                    //    补充新增额度。扩容每 tick 至多一次。
                    let mut expanded_this_tick = false;
                    loop {
                        // 按额度补充 worker：与 Done 派工同一逻辑（Pending 优先，
                        // 否则拆分最大 Active 段），启动与扩容共用 ctx.spawn 路径。
                        while alive < allowed_workers {
                            sync_downloaded_from_shared(&mut segments, &seg_states);
                            let Some(next) = find_next_work(
                                &mut segments,
                                &mut next_index,
                                effective_total_bytes,
                                current_min_split,
                                split_cost(&request_path, conn_bps_hint, split_window),
                            ) else {
                                break;
                            };
                            let new_seg_idx = next.assignment.seg_index;
                            if let Err(error) = persist_segment_change(
                                db, task_id, &segments,
                                new_seg_idx, next.split_parent,
                            ).await {
                                worker_cancel.cancel();
                                for tx in &mut worker_assign_txs { *tx = None; }
                                final_error = Some(error);
                                break 'coordinator;
                            }
                            if let Some(parent_idx) = next.split_parent {
                                send_split_event(
                                    sink, task_id, parent_idx, new_seg_idx,
                                    &segments, false, scope,
                                );
                            }
                            rebuild_seg_states(&segments, &seg_states);
                            let worker_id = worker_assign_txs.len();
                            let (assign_tx, handle) = ctx.spawn(worker_id);
                            // channel 刚创建（容量 4），try_send 不可能失败；防御回退。
                            if assign_tx.try_send(next.assignment).is_err() {
                                if let Some(seg) = segments.get_mut(&new_seg_idx) {
                                    seg.state = SegState::Pending;
                                }
                                break;
                            }
                            worker_assign_txs.push(Some(assign_tx));
                            worker_handles.push(Some(handle));
                            alive += 1;
                        }

                        if expanded_this_tick || tail || shrunk_this_tick {
                            break;
                        }

                        // 软冻结 +1 试探（评估/shrink 之后、正常倍增之前）。
                        if soft_probe_ready(
                            freeze,
                            tail,
                            range_ok,
                            serial_mode,
                            conn_sensitive.load(Ordering::Relaxed),
                            reject_strikes,
                            alive,
                            allowed_workers,
                            worker_cap,
                            soft_probe_task_used,
                        ) {
                            pre_grow_throughput = throughput;
                            pre_grow_workers = allowed_workers;
                            awaiting_ramp_eval = true;
                            probe_window = true;
                            soft_probe_task_used =
                                soft_probe_task_used.saturating_add(1);
                            let old = allowed_workers;
                            allowed_workers = (allowed_workers + 1).min(worker_cap);
                            expanded_this_tick = true;
                            log_info!(
                                "[adaptive] task {} 软解冻试探：{} -> {}",
                                task_id,
                                old,
                                allowed_workers
                            );
                            continue;
                        }

                        // 正常倍增扩容：仅 Active 时 should_expand 的 frozen=false。
                        let expand_frozen = !matches!(freeze, FreezeState::Active);
                        if !should_expand(
                            expand_frozen,
                            conn_sensitive.load(Ordering::Relaxed),
                            alive,
                            allowed_workers,
                            worker_cap,
                            shrunk_this_tick,
                        ) {
                            break;
                        }
                        pre_grow_throughput = throughput;
                        pre_grow_workers = allowed_workers;
                        awaiting_ramp_eval = true;
                        probe_window = false;
                        let old = allowed_workers;
                        allowed_workers = (allowed_workers * 2).min(worker_cap);
                        expanded_this_tick = true;
                        log_info!(
                            "[adaptive] task {} ramp up: {} -> {} (cap={}), \
                             throughput={:.0} B/s, range_ok=true",
                            task_id,
                            old,
                            allowed_workers,
                            worker_cap,
                            throughput
                        );
                    }
                }

                // 5. 存活性兜底：ctx 持有 event_tx，worker 全部退出（如 panic）
                //    不再触发 channel 关闭。若无存活 worker、任务未完成且上面的
                //    补充循环也无法开工，退出事件循环——交由第 8 步完整性检查
                //    报错，与旧实现 channel-close 路径语义等价。
                if alive == 0 && revive_at.is_none() && !all_done(&segments) {
                    log_info!(
                        "[coordinator] task {} 所有 worker 已退出但任务未完成，退出事件循环",
                        task_id
                    );
                    break;
                }
            }

            // --- UI progress snapshot ------------------------------------
            // 单点汇总：字段与原 worker 内 200ms 上报完全一致（status=1、
            // downloaded_bytes = total_downloaded + scope.base、total_bytes 取
            // total_override 或 planned_total、segment_details 经 map_snapshot）。
            _ = ui_interval.tick() => {
                if progress_tx.is_closed() { continue; }
                let current_total = total_downloaded.load(Ordering::Relaxed);
                let report_total = if scope.total_override > 0 {
                    scope.total_override
                } else {
                    planned_total.load(Ordering::Relaxed)
                };
                let (snapshot, mut runtime) = sampled_http_runtime(
                    task_id, report_total, worker_cap as u32, &seg_states, scope,
                    &generation_evidence.tracker,
                );
                runtime.source_bytes = sample_run_source_bytes(
                    &nodes, &mut segments, &seg_states, source_base, &mut source_peak,
                );
                if progress_tx
                    .send(ProgressUpdate {
                        task_id: task_id.to_string(),
                        downloaded_bytes: current_total + scope.base,
                        total_bytes: report_total,
                        status: 1,
                        error_message: String::new(),
                        file_name: String::new(),
                        segment_details: Some(snapshot),
                        runtime: Some(runtime),
                        ..Default::default()
                    })
                    .await.is_err() {
                    tracing::debug!("HTTP progress receiver closed");
                }
            }

            // --- Durable segment progress batch flush --------------------
            // 抽干 worker 侧 durable 水位，过滤 Completed/未知段后一次事务批量写。
            // 完成写仍由 worker 覆盖 fsync 后立即落库，不经本通道。
            _ = db_flush_interval.tick() => {
                if sample_run_source_bytes(
                    &nodes, &mut segments, &seg_states, source_base, &mut source_peak,
                )
                .is_some()
                {
                    flush_source_bytes(db, task_id, spawn_gen, source_peak, &mut source_flushed)
                        .await;
                }
                let rows = take_durable_progress_rows(&durable_progress, &segments);
                if !rows.is_empty()
                    && let Err(e) = db
                        .update_segments_progress_batch(task_id, spawn_gen, &rows)
                        .await
                {
                    log_info!(
                        "[coordinator] task {} durable batch flush failed (non-fatal): {}",
                        task_id,
                        e
                    );
                }
            }

        }
    }

    // Drain events while joining: a stopped coordinator must not leave workers
    // blocked on a full event channel during cancellation or error cleanup.
    for handle in &mut worker_handles {
        if let Some(mut handle) = handle.take() {
            loop {
                tokio::select! {
                    joined = &mut handle => {
                        if let Err(error) = joined {
                            if error.is_cancelled() { tracing::debug!("HTTP worker cancelled"); }
                            else {
                                crate::logger::report_warning("http-coordinator", "join worker", &error);
                                if matches!(final_error, None | Some(DownloadError::Cancelled)) {
                                    final_error = Some(DownloadError::Other(format!("HTTP worker join failed: {error}")));
                                }
                            }
                        }
                        break;
                    }
                    Some(event) = event_rx.recv() => record_shutdown_worker_event(event, &mut final_error),
                }
            }
        }
    }
    while let Ok(event) = event_rx.try_recv() {
        record_shutdown_worker_event(event, &mut final_error);
    }
    // Preserve range geometry after the last body reader leaves.
    let (_, mut runtime) = sampled_http_runtime(
        task_id,
        if scope.total_override > 0 {
            scope.total_override
        } else {
            planned_total.load(Ordering::Relaxed)
        },
        worker_cap as u32,
        &seg_states,
        scope,
        &generation_evidence.tracker,
    );
    runtime.source_bytes = sample_run_source_bytes(
        &nodes,
        &mut segments,
        &seg_states,
        source_base,
        &mut source_peak,
    );
    sink.emit(EngineEvent::TaskRuntimeChanged(runtime));

    // 多路径：主导链路最终定论 + 各路径实测估计写入跨任务先验（成功与
    // 取消/失败同样有效——估计来自已发生的真实传输）。
    sync_downloaded_from_shared(&mut segments, &seg_states);
    multipath
        .publish_route(&nodes, &segments, db, sink, task_id)
        .await;
    multipath.record_priors(&nodes, db);

    // 抽干剩余 durable 水位（循环退出后 interval 不再 tick）。
    {
        let rows = take_durable_progress_rows(&durable_progress, &segments);
        if !rows.is_empty()
            && let Err(e) = db
                .update_segments_progress_batch(task_id, spawn_gen, &rows)
                .await
        {
            log_info!(
                "[coordinator] task {} final durable batch failed (non-fatal): {}",
                task_id,
                e
            );
        }
    }

    // 加速来源累计的最终增量（worker 已全部退出，在途租约已并入槽位回报）。
    flush_source_bytes(db, task_id, spawn_gen, source_peak, &mut source_flushed).await;

    if let Some(err) = final_error {
        return Err(err);
    }

    // ----- 8. Final verification --------------------------------------------
    // Sync one last time to get the most accurate downloaded_bytes.
    sync_downloaded_from_shared(&mut segments, &seg_states);

    let seg_total: i64 = segments.values().map(|s| s.downloaded_bytes).sum();
    if seg_total < effective_total_bytes {
        return Err(DownloadError::Other(format!(
            "coordinator: incomplete download, segments total={} expected={}",
            seg_total, effective_total_bytes
        )));
    }

    // Verify byte-range coverage as a final safety net.
    if let Err(msg) = validate_coverage(&segments, effective_total_bytes) {
        return Err(DownloadError::Other(format!(
            "coordinator: post-download coverage error: {}",
            msg
        )));
    }

    // 确有嫌疑的多段下载（探测基线带 validator 却被分段 206 剥离 / 出现被容忍的
    // Last-Modified 漂移，见 `content_audit_warranted`）：HTTP 层无 ETag 可逐段比对
    // （If-Range 按设计不发），文件中途被替换会静默拼成新旧混合的损坏文件。以内容
    // 采样补一道审计；仅在拿到确凿矛盾时判 VersionChanged（上层清盘后单流重下新
    // 版本），审计本身失败/无结论一律放行；每个任务至多一次审计触发的重下。
    // 配额型端点（发生过吸收合并、新连接必被拒）不再额外打扰：审计请求注定被拒且可能
    // 消耗 token。
    if segments.len() > 1
        && generation_evidence.content_audit_warranted(etag, last_modified)
        && splice_audit::redownload_allowed(task_id)
        && !reconnect_hostile.load(Ordering::Relaxed)
        && let Some(client) = nodes.sys_client()
        && let Some(offset) = splice_audit::find_version_mismatch(
            &client,
            url,
            spec.as_ref(),
            dest,
            effective_total_bytes,
            cancel_token,
        )
        .await
    {
        splice_audit::record_redownload(task_id);
        log_info!(
            "[coordinator] task {} 版本证据缺失，完成后内容采样在偏移 {} 处与服务器当前版本不符——文件在下载中被替换，回退重下",
            task_id,
            offset
        );
        return Err(DownloadError::VersionChanged(format!(
            "post-download content audit: bytes at offset {offset} differ from the server's \
             current version (no ETag was available to detect a mid-download replacement)"
        )));
    }

    // Flush the authoritative in-memory downloaded_bytes (already capped to
    // segment size) back to the DB in a single transaction.  This is the
    // canonical final state: any overshoot from the split race is corrected
    // here, ensuring run_download_inner's integrity check sees correct totals.
    let flush_updates: Vec<(i32, i64)> = segments
        .values()
        .map(|s| (s.index, s.downloaded_bytes))
        .collect();
    db.flush_segments_progress(task_id, flush_updates).await?;
    // The coordinator's last geometry must reach the reporter before the caller's
    // completion frame; a fast download may never have hit the periodic tick.
    let report_total = if scope.total_override > 0 {
        scope.total_override
    } else {
        planned_total.load(Ordering::Relaxed)
    };
    let (snapshot, mut runtime) = sampled_http_runtime(
        task_id,
        report_total,
        worker_cap as u32,
        &seg_states,
        scope,
        &generation_evidence.tracker,
    );
    runtime.source_bytes = sample_run_source_bytes(
        &nodes,
        &mut segments,
        &seg_states,
        source_base,
        &mut source_peak,
    );
    if progress_tx
        .send(ProgressUpdate {
            task_id: task_id.to_owned(),
            downloaded_bytes: seg_total + scope.base,
            total_bytes: report_total,
            status: 1,
            segment_details: Some(snapshot),
            runtime: Some(runtime),
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("HTTP progress receiver closed");
    }

    // ----- 9. 正面域名学习 --------------------------------------------------
    // 任务全程无拒绝/降级/连接敏感信号且以多连接规模真实运行过 → 把
    // proven ∩ beneficial（受益过的规模，而非仅被接受过的规模）记为同域名
    // 起步提示。Freeze 不回滚场景下 proven 可能棘轮抬升，beneficial 钳制之。
    // 出现过任何拒绝信号的路径绝不记录——那些观察已由负面 cap 面接管。
    let hint_scale = beneficial_hint_scale(proven_scale, beneficial_scale);
    if hint_scale > RAMP_INITIAL_WORKERS
        && reject_strikes == 0
        && !serial_mode
        && !conn_sensitive.load(Ordering::Relaxed)
        && !multipath.alternates_used(&nodes)
    {
        record_domain_conn_hint_persist(url, hint_scale as i32, db);
    }

    Ok(effective_total_bytes)
}

/// 启动时把比目标更长的旧临时文件截短。总大小缩小后分段按新总量重建，
/// [新总量, 旧总量) 留着上一版本的数据或预分配的零，成品会带陈旧尾部。
/// 仅用于 worker 启动之前：此时没有并发写；就地扩容只会变大，不走这里。
async fn truncate_stale_tail(dest: &Path, target_len: u64) -> Result<(), DownloadError> {
    let file = match OpenOptions::new().write(true).open(dest).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if file.metadata().await?.len() > target_len {
        file.set_len(target_len).await?;
    }
    Ok(())
}

/// 把 `dest` 预分配/扩容到至少 `target_len` 字节（逻辑 EOF + 尽量物理分配）。
/// 已 `>= target_len` 时为 no-op。两处使用：
///   1. 下载启动时的整文件预分配（coordinator 第 2 步）；
///   2. 就地扩容（[`DownloadError::TrueSizeLarger`]）时延长临时文件——此时其它
///      worker 可能正持有各自句柄在低偏移写入：Windows 默认共享模式允许并发
///      SetEndOfFile 扩展，Linux fallocate 同理，均不影响进行中的写。
///
/// 平台策略：
/// - Linux:   fallocate(2) 分配真实磁盘块（不写零，近乎瞬时），避免
///   set_len()/ftruncate 稀疏文件导致的碎片化和延迟 ENOSPC。
/// - Windows: 先给文件打 NTFS sparse 标记（[`crate::bt_sparse::mark_sparse`]）再
///   SetEndOfFile 设置逻辑大小：非 sparse 文件在高偏移首次写入时 NTFS 会按
///   valid-data-length 语义同步零填充 `[VDL, offset)`，多段 worker 各自从
///   `k·N/n` 起写会造成数量级的写放大与周期性掉速。代价是磁盘不足不再于预分配
///   阶段暴露，而是在写入时以 ENOSPC/`ERROR_DISK_FULL` 上报（致命、不重试）。
///   打标记失败（FAT32/exFAT/网络盘等）才退回 FileAllocationInfo 物理预分配。
/// - 其它:    回退 set_len()。
async fn preallocate_file_len(
    dest: &Path,
    target_len: u64,
    temp_file_guard: &TempFileGuard,
) -> Result<(), DownloadError> {
    let temp_file_guard = temp_file_guard.clone();
    #[cfg(target_os = "windows")]
    let dest_owned = dest.to_path_buf();
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dest)
        .await?;
    let current_len = file.metadata().await?.len();
    if current_len >= target_len {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let std_file = file.into_std().await;
        tokio::task::spawn_blocking(move || -> Result<(), DownloadError> {
            let _temp_file_guard = temp_file_guard;
            use std::os::unix::io::AsRawFd;
            let fd = std_file.as_raw_fd();
            // fallocate(fd, 0, 0, len): 预分配 [0, len) 范围的磁盘块，
            // 不写零，ext4/XFS/Btrfs 均支持，耗时 O(1)。
            // mode=0 同时将文件大小设为 max(当前大小, offset+len)。
            // fs2 0.4.3 的 posix_fallocate 会误读 errno，且 glibc 仿真
            // 不等价于 O_WRONLY/并发扩容下的内核 fallocate。
            let allocation_len = libc::off_t::try_from(target_len).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "allocation exceeds off_t")
            })?;
            // SAFETY: std_file 持有有效可写 fd；长度已检查为 off_t，调用不保留 fd。
            let ret = unsafe { libc::fallocate(fd, 0, 0, allocation_len) };
            if ret == 0 {
                return Ok(());
            }
            // fallocate 失败 — 检查是否为文件系统不支持
            let err = std::io::Error::last_os_error();
            let raw = err.raw_os_error().unwrap_or(0);
            if raw == libc::EOPNOTSUPP || raw == libc::ENOSYS {
                // tmpfs/NFS 等不支持 fallocate 的文件系统，回退到 ftruncate
                log_info!(
                    "[coordinator] fallocate 不支持 (errno={}), 回退到 ftruncate",
                    raw
                );
                std_file.set_len(target_len)?;
                Ok(())
            } else {
                // ENOSPC 等真实错误，直接上报（提前检测磁盘空间不足）
                Err(err.into())
            }
        })
        .await
        .map_err(|e| DownloadError::Other(format!("fallocate task panicked: {e}")))??;
    }
    #[cfg(target_os = "windows")]
    {
        let std_file = file.into_std().await;
        tokio::task::spawn_blocking(move || -> Result<(), DownloadError> {
            let _temp_file_guard = temp_file_guard;
            // Step 0: sparse 标记必须先于任何 set_len / 写入。失败只记日志，
            // 退回下方的物理预分配。
            let sparse = match crate::bt_sparse::mark_sparse(&dest_owned) {
                Ok(()) => true,
                Err(e) => {
                    log_info!("[coordinator] FSCTL_SET_SPARSE 失败，退回物理预分配: {}", e);
                    false
                }
            };
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::FILE_ALLOCATION_INFO;
            // fs2::allocate 额外查询 allocated_size/metadata 并合并分配与 EOF
            // 错误；不替换此处“分配失败仅记录、EOF 失败传播”的既有策略。
            let handle = std_file.as_raw_handle();
            // Step 1: sparse 不可用时尽量预留物理簇；失败记日志后继续设置 EOF，
            // 真正写入仍负责上报磁盘不足，不将后备分配错误混为 EOF 错误。
            if !sparse {
                let info = FILE_ALLOCATION_INFO {
                    AllocationSize: i64::try_from(target_len).map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "allocation exceeds LARGE_INTEGER",
                        )
                    })?,
                };
                // SAFETY: std_file 持有有效可写句柄；info 是对应信息类的官方
                // 结构，正确对齐且大小匹配；同步调用不保留指针或句柄。
                let ret = unsafe {
                    windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle(
                        handle,
                        windows_sys::Win32::Storage::FileSystem::FileAllocationInfo,
                        &info as *const _ as *const core::ffi::c_void,
                        std::mem::size_of::<FILE_ALLOCATION_INFO>() as u32,
                    )
                };
                if ret == 0 {
                    // FAT32/exFAT/网络驱动器等不支持时仅记录日志，不中断
                    log_info!(
                        "[coordinator] SetFileInformationByHandle(FileAllocationInfo) 失败: {}",
                        std::io::Error::last_os_error()
                    );
                }
            }
            // Step 2: 设置逻辑 EOF——后续 seek+write 依赖此值
            std_file.set_len(target_len)?;
            Ok(())
        })
        .await
        .map_err(|e| DownloadError::Other(format!("prealloc task panicked: {e}")))??;
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let std_file = file.into_std().await;
        tokio::task::spawn_blocking(move || {
            let _temp_file_guard = temp_file_guard;
            std_file.set_len(target_len)
        })
        .await
        .map_err(|e| DownloadError::Other(format!("prealloc task panicked: {e}")))??;
    }
    Ok(())
}

/// hint 模式（无 probe 基线）的跨段版本一致性检查。
///
/// probe 被跳过时（浏览器扩展 hint 保护一次性签名 URL），`expected_etag` /
/// `expected_last_modified` 均为空，do_segment 的常规版本守卫恒不生效——多段间
/// 没有任何版本一致性保障：文件在下载中途被【替换】会拼出新旧混合的损坏文件。
/// 本函数以【第一个 206 响应】携带的 (ETag, Last-Modified) 为基线（latch），后续
/// 所有段（含就地扩容追加的尾段）与之比较。
///
/// 比较策略与 probe 路径一致（缺失容忍）：仅当基线与响应【双方均非空】且不等时
/// 判为漂移，返回 `Err(基线)`；CDN 在 206 上剥离 validator（空串）永不比较。
fn check_cross_segment_validators(
    first_validators: &StdMutex<Option<(String, String)>>,
    resp_etag: &str,
    resp_lm: &str,
) -> Result<(), (String, String)> {
    let mut guard = match first_validators.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    match guard.as_ref() {
        None => {
            *guard = Some((resp_etag.to_string(), resp_lm.to_string()));
            Ok(())
        }
        Some((base_etag, base_lm)) => {
            let etag_mismatch =
                !base_etag.is_empty() && !resp_etag.is_empty() && base_etag != resp_etag;
            let lm_mismatch = !base_lm.is_empty() && !resp_lm.is_empty() && base_lm != resp_lm;
            if etag_mismatch || lm_mismatch {
                Err((base_etag.clone(), base_lm.clone()))
            } else {
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: build fresh uniform segments
// ---------------------------------------------------------------------------

/// Create `count` uniform segments spanning `[0, total_bytes-1]` and return
/// both the in-memory map and the DB tuples for batch insertion.
fn build_fresh_segments(count: i32, total_bytes: i64) -> FreshSegments {
    let count_i64 = count as i64;
    let chunk = total_bytes / count_i64;
    let mut segments = BTreeMap::new();
    let mut db_segs = Vec::with_capacity(count as usize);
    for i in 0..count {
        let start = i as i64 * chunk;
        let end = if i == count - 1 {
            total_bytes - 1
        } else {
            (i as i64 + 1) * chunk - 1
        };
        segments.insert(
            i,
            LiveSegment {
                index: i,
                start_byte: start,
                end_byte: end,
                downloaded_bytes: 0,
                state: SegState::Pending,
                rate_bps: None,
            },
        );
        db_segs.push((i, start, end));
    }
    (segments, db_segs)
}

// ---------------------------------------------------------------------------
// Segment coverage validation
// ---------------------------------------------------------------------------

/// 已记账写入的最高文件偏移（不含）：各段 `start + downloaded` 的最大值。
/// GrowOnWrite 布局下合规续传文件至少有这么长。
fn written_high_water(segments: &BTreeMap<i32, LiveSegment>) -> i64 {
    segments
        .values()
        .filter(|s| s.downloaded_bytes > 0)
        .map(|s| s.start_byte + s.downloaded_bytes)
        .max()
        .unwrap_or(0)
}

/// Verify that segment ranges cover `[0, total_bytes-1]` with no gaps/overlaps.
fn validate_coverage(
    segments: &BTreeMap<i32, LiveSegment>,
    total_bytes: i64,
) -> Result<(), String> {
    if segments.is_empty() {
        return Err("no segments".to_string());
    }

    // Sort by start_byte to check contiguity.
    let mut sorted: Vec<&LiveSegment> = segments.values().collect();
    sorted.sort_by_key(|s| s.start_byte);

    // First segment must start at 0.
    if sorted[0].start_byte != 0 {
        return Err(format!(
            "first segment starts at {} instead of 0",
            sorted[0].start_byte
        ));
    }

    // Last segment must end at total_bytes - 1.
    let last = sorted[sorted.len() - 1];
    if last.end_byte != total_bytes - 1 {
        return Err(format!(
            "last segment ends at {} instead of {}",
            last.end_byte,
            total_bytes - 1
        ));
    }

    // Check contiguity: each segment's start must be exactly previous end + 1.
    for window in sorted.windows(2) {
        let prev = window[0];
        let curr = window[1];
        let expected_start = prev.end_byte + 1;
        if curr.start_byte != expected_start {
            return Err(format!(
                "gap or overlap between segment {} (end={}) and segment {} (start={})",
                prev.index, prev.end_byte, curr.index, curr.start_byte
            ));
        }
    }

    // Verify total coverage equals total_bytes.
    let total_coverage: i64 = segments.values().map(|s| s.size()).sum();
    if total_coverage != total_bytes {
        return Err(format!(
            "total coverage {} != total_bytes {}",
            total_coverage, total_bytes
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Work assignment logic
// ---------------------------------------------------------------------------

/// Find the next piece of work for an idle worker.
///
/// Strategy (matching IDM behaviour):
/// 1. If there is a `Pending` segment, return it.
/// 2. Otherwise, try to split the largest `Active` segment in half.
/// 3. If nothing can be split, return `None` (worker should retire).
fn find_next_work(
    segments: &mut BTreeMap<i32, LiveSegment>,
    next_index: &mut i32,
    _total_bytes: i64,
    min_split: i64,
    cost: SplitCost,
) -> Option<NextWork> {
    // Strategy 1: existing Pending segment.
    if let Some(seg) = segments.values().find(|s| s.state == SegState::Pending) {
        let assignment = WorkerAssignment {
            seg_index: seg.index,
            seg_start: seg.start_byte,
            actual_start: seg.start_byte + seg.downloaded_bytes,
            seg_end: seg.end_byte,
            open_ended: false,
            no_range: false,
        };
        let idx = seg.index;
        if let Some(s) = segments.get_mut(&idx) {
            s.state = SegState::Active;
        }
        return Some(NextWork {
            assignment,
            split_parent: None,
        });
    }

    // Strategy 2: split the largest active segment at the dynamic threshold.
    if let Some(work) = try_split_largest(segments, next_index, min_split, cost) {
        return Some(work);
    }

    // Strategy 3: tail micro-split — when normal split fails (remaining bytes
    // below the dynamic threshold), retry with TAIL_MIN_SPLIT_BYTES (64 KB).
    //
    // This eliminates "tail stall": in a 16-segment download of a 1 GB file,
    // if the last segment has 1.5 MB remaining and MIN_SPLIT is 2 MB, 15 workers
    // idle while 1 slow worker finishes.  With tail micro-split, the 1.5 MB is
    // split into 750 KB + 750 KB, and an idle worker helps finish it 2× faster.
    //
    // Guard A: only activate when the normal threshold is above the tail threshold;
    // if dynamic_min_split already returned 512 KB (low speed), and 512 KB >
    // 64 KB, we retry at 64 KB.  If min_split is already <= 64 KB, there's
    // nothing smaller to try.
    //
    // Guard B: "straggler" check — only micro-split when the largest remaining
    // active segment is ≥ 2 × TAIL_MIN_SPLIT_BYTES (128 KB), indicating a
    // genuine imbalance worth rescuing.
    //
    // Without Guard B, when all remaining segments are equally small (all ~66 KB
    // at the tail of a 50 MB download with 48 workers), workers cascade-split
    // them into ~33 KB pieces.  Workers finishing those micro-segments find
    // nothing more to split and retire en-masse, dropping active worker count
    // from ~48 → ~16 and causing the visible "99% speed drop" in the UI.
    //
    // With Guard B, the cascade stops naturally: a worker finishing a 33 KB
    // segment finds no straggler (max_remaining ≈ 66 KB < 128 KB) and retires
    // gracefully instead of further subdividing already-tiny peers.
    if min_split > TAIL_MIN_SPLIT_BYTES {
        let max_remaining = segments
            .values()
            .filter(|s| s.state == SegState::Active)
            .map(|s| s.remaining())
            .max()
            .unwrap_or(0);
        if max_remaining >= 2 * TAIL_MIN_SPLIT_BYTES {
            try_split_largest(segments, next_index, TAIL_MIN_SPLIT_BYTES, cost)
        } else {
            None
        }
    } else {
        None
    }
}

/// 串行模式专用：只从 Pending 分段中分配工作，不进行拆分。
///
/// 与 [`find_next_work`] 不同，此函数绝不会拆分 Active 分段来创建新工作，
/// 确保在限制并发连接的服务器上不会发起额外的 HTTP 请求。
fn find_next_pending_only(segments: &mut BTreeMap<i32, LiveSegment>) -> Option<NextWork> {
    let seg = segments.values().find(|s| s.state == SegState::Pending)?;
    let assignment = WorkerAssignment {
        seg_index: seg.index,
        seg_start: seg.start_byte,
        actual_start: seg.start_byte + seg.downloaded_bytes,
        seg_end: seg.end_byte,
        open_ended: false,
        no_range: false,
    };
    let idx = seg.index;
    if let Some(s) = segments.get_mut(&idx) {
        s.state = SegState::Active;
    }
    Some(NextWork {
        assignment,
        split_parent: None,
    })
}

/// 拆分的代价模型输入。
#[derive(Clone, Copy, Default)]
struct SplitCost {
    /// 帮手从发出请求到收到响应头的耗时估计（秒，实测 EWMA；0 = 无样本）。
    setup_secs: f64,
    /// 逐段速率尚无样本（首个完整采样窗之前、短任务全程）时的单连接速率估计：
    /// 近期总吞吐 / 在传分段数。
    conn_bps: Option<f64>,
    /// 持有者保留量上限（[`file_layout::FileLayout::split_window`]）：随写增长的文件上，帮手
    /// 必须紧贴持有者起写，否则高偏移首写会零填充整个间隙。`None` = 不限。
    max_keep: Option<i64>,
}

fn split_cost(
    request_path: &RequestPath,
    conn_bps: Option<f64>,
    max_keep: Option<i64>,
) -> SplitCost {
    SplitCost {
        setup_secs: request_path.setup_secs(),
        conn_bps,
        max_keep,
    }
}

/// 拆分规划：挑选预计完成最晚的活动段（ECF：持有连接实测速率未知时按
/// 当前最快连接速率估计，全部未知时按 `cost.conn_bps`，再无则退化为剩余字节
/// 最多），按持有者与帮手（当前最快连接）的速率比例切分剩余区间使双方同时
/// 完成（DEMS 均衡完成；速率未知时为中点），并计入帮手的请求建立耗时：持有者
/// 在帮手收到首字节前就能下完时不拆（见
/// [`crate::path_scheduler::split_keep_with_setup`]），保留量再受
/// `cost.max_keep` 封顶。返回
/// `(段索引, 当前位置, 剩余字节, 拆分点)`；无可拆段或拆分点非法 → `None`。
fn plan_split(
    segments: &BTreeMap<i32, LiveSegment>,
    min_split: i64,
    cost: SplitCost,
) -> Option<(i32, i64, i64, i64)> {
    let helper_bps = segments
        .values()
        .filter(|s| s.state == SegState::Active)
        .filter_map(|s| s.rate_bps)
        .filter(|r| *r > 0.0)
        .max_by(f64::total_cmp)
        .or(cost.conn_bps.filter(|r| *r > 0.0));
    let expected_finish = |s: &LiveSegment| -> f64 {
        match helper_bps {
            Some(helper) => {
                crate::path_scheduler::completion_secs(s.remaining(), s.rate_bps.unwrap_or(helper))
            }
            None => s.remaining() as f64,
        }
    };
    let best = segments
        .values()
        .filter(|s| s.state == SegState::Active && s.remaining() >= min_split)
        .max_by(|a, b| {
            expected_finish(a)
                .total_cmp(&expected_finish(b))
                .then(a.remaining().cmp(&b.remaining()))
        })?;

    // The current download position in the best segment.
    let current_pos = best.start_byte + best.downloaded_bytes;
    let remaining = best.end_byte - current_pos + 1;
    if remaining < min_split {
        return None;
    }
    let keep = crate::path_scheduler::split_keep_with_setup(
        remaining,
        best.rate_bps.or(helper_bps),
        helper_bps,
        cost.setup_secs,
    )?;
    let keep = cost.max_keep.map_or(keep, |window| keep.min(window));
    let split_point = current_pos + keep;
    // Validate: split_point must be within (current_pos, end_byte].
    // This guarantees both halves are non-empty.
    if split_point <= current_pos || split_point > best.end_byte {
        return None;
    }
    Some((best.index, current_pos, remaining, split_point))
}

/// IDM-style division of the active segment expected to finish last (see
/// [`plan_split`]): the holder keeps a rate-proportional share of its
/// remaining range, the new segment takes the rest.
///
/// Returns a `NextWork` for the **new** segment (upper part), including the
/// index of the parent segment that was shrunk, or `None` if no segment is
/// large enough to split.
fn try_split_largest(
    segments: &mut BTreeMap<i32, LiveSegment>,
    next_index: &mut i32,
    min_split: i64,
    cost: SplitCost,
) -> Option<NextWork> {
    // Only count non-Completed segments — Completed slots do not contribute
    // to the concurrent-connection limit. This allows idle workers to keep
    // helping the last active segment even after many historical splits.
    let active_or_pending = segments
        .values()
        .filter(|s| s.state != SegState::Completed)
        .count();
    if active_or_pending >= MAX_SEGMENTS as usize {
        return None;
    }

    let (best_idx, current_pos, remaining, split_point) = plan_split(segments, min_split, cost)?;
    let old_end = segments.get(&best_idx)?.end_byte;

    // New segment covers [split_point, old_end].
    let new_index = *next_index;
    *next_index += 1;

    let new_seg = LiveSegment {
        index: new_index,
        start_byte: split_point,
        end_byte: old_end,
        downloaded_bytes: 0,
        state: SegState::Active,
        rate_bps: None,
    };

    // Shrink the original segment to [old_start, split_point - 1].
    // The worker currently downloading this segment sees the new end_byte
    // via the shared seg_states and truncates its writes accordingly.
    if let Some(orig) = segments.get_mut(&best_idx) {
        orig.end_byte = split_point - 1;
    }

    let assignment = WorkerAssignment {
        seg_index: new_index,
        seg_start: split_point,
        actual_start: split_point,
        seg_end: old_end,
        open_ended: false,
        no_range: false,
    };

    segments.insert(new_index, new_seg);

    log_info!(
        "[coordinator] split segment {} → new segment {} at byte {} (parent remaining: {}→{})",
        best_idx,
        new_index,
        split_point,
        remaining,
        split_point - current_pos
    );

    Some(NextWork {
        assignment,
        split_parent: Some(best_idx),
    })
}

/// Proactively split the active segment expected to finish last (see
/// [`plan_split`]) while other workers are still
/// running, creating a **Pending** (not Active) child so that an idle or newly-
/// freed worker can pick it up via `find_next_work`.
///
/// Called periodically by the coordinator's proactive-split timer (every
/// [`PROACTIVE_SPLIT_INTERVAL_SECS`] seconds) to pre-create work items.  This
/// moves the split computation + DB persistence off the critical Done-handler
/// path, reducing worker idle time between segments.
///
/// Returns `None` when:
/// - any `Pending` segment already exists (no need to create more), or
/// - no active segment is large enough to split (< `min_split` remaining), or
/// - the segment cap `MAX_SEGMENTS` would be exceeded.
fn try_proactive_split(
    segments: &mut BTreeMap<i32, LiveSegment>,
    next_index: &mut i32,
    min_split: i64,
    cost: SplitCost,
) -> Option<NextWork> {
    // Do nothing if there's already a pending segment waiting for a worker.
    if segments.values().any(|s| s.state == SegState::Pending) {
        return None;
    }

    // Only count non-Completed segments — Completed slots do not contribute
    // to the concurrent-connection limit. This allows idle workers to keep
    // helping the last active segment even after many historical splits.
    let active_or_pending = segments
        .values()
        .filter(|s| s.state != SegState::Completed)
        .count();
    if active_or_pending >= MAX_SEGMENTS as usize {
        return None;
    }

    let (best_idx, _current_pos, _remaining, split_point) = plan_split(segments, min_split, cost)?;
    let old_end = segments.get(&best_idx)?.end_byte;
    let new_index = *next_index;
    *next_index += 1;

    // New segment is Pending — a worker will pick it up when idle.
    let new_seg = LiveSegment {
        index: new_index,
        start_byte: split_point,
        end_byte: old_end,
        downloaded_bytes: 0,
        state: SegState::Pending,
        rate_bps: None,
    };

    if let Some(orig) = segments.get_mut(&best_idx) {
        orig.end_byte = split_point - 1;
    }

    let assignment = WorkerAssignment {
        seg_index: new_index,
        seg_start: split_point,
        actual_start: split_point,
        seg_end: old_end,
        open_ended: false,
        no_range: false,
    };

    segments.insert(new_index, new_seg);

    log_info!(
        "[coordinator] proactive split: segment {} → new pending segment {} at byte {}",
        best_idx,
        new_index,
        split_point
    );

    Some(NextWork {
        assignment,
        split_parent: Some(best_idx),
    })
}

// ---------------------------------------------------------------------------
// Helper: check completion
// ---------------------------------------------------------------------------

fn has_other_in_flight(segments: &BTreeMap<i32, LiveSegment>, failed_index: i32) -> bool {
    segments
        .values()
        .any(|s| s.index != failed_index && s.state == SegState::Active)
}

fn all_done(segments: &BTreeMap<i32, LiveSegment>) -> bool {
    segments.values().all(|s| s.state == SegState::Completed)
}

// ---------------------------------------------------------------------------
// Helpers: shared state synchronization
// ---------------------------------------------------------------------------

/// 共享 seg_states 中某段的位置。向量恒由 `BTreeMap` 按段索引升序重建
/// （[`build_seg_state_vec`] / [`rebuild_seg_states`]），worker 每个 chunk 查两次，
/// 二分查找把 64～512 段时的线性扫描降到对数；顺序不变式意外被破坏时退回线性扫描。
fn seg_state_pos(states: &[SegmentProgressInfo], seg_idx: i32) -> Option<usize> {
    states
        .binary_search_by_key(&seg_idx, |s| s.index)
        .ok()
        .or_else(|| states.iter().position(|s| s.index == seg_idx))
}

/// Build a fresh `Vec<SegmentProgressInfo>` from the segment map.
fn build_seg_state_vec(segments: &BTreeMap<i32, LiveSegment>) -> Vec<SegmentProgressInfo> {
    segments
        .values()
        .map(|s| SegmentProgressInfo {
            index: s.index,
            start_byte: s.start_byte,
            end_byte: s.end_byte,
            downloaded_bytes: s.downloaded_bytes,
            active: None,
        })
        .collect()
}

/// Overwrite the shared visualization state from the authoritative segment map.
///
/// `downloaded_bytes` is **exclusively owned by the worker** (see ownership
/// contract on [`update_seg_state`]); the coordinator only owns `start_byte` /
/// `end_byte` / `index`.  Naively replacing the whole vector from the
/// coordinator's in-memory map would clobber `downloaded_bytes` with a value
/// that may already be stale — between `sync_downloaded_from_shared` at the top
/// of the Done handler and this call there is an `.await` (persist), during
/// which other active workers keep advancing and writing larger
/// `downloaded_bytes` into `seg_states`.  Writing the smaller stale value back
/// would regress those segments' progress, briefly rewind the UI, and feed a
/// stale split point into `try_split_largest`.
///
/// To honour the single-writer contract we preserve each *existing* segment's
/// `downloaded_bytes` from the current shared state and only let
/// newly-introduced segments (split children, absent from the snapshot) take
/// their value from the map.
fn rebuild_seg_states(
    segments: &BTreeMap<i32, LiveSegment>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
) {
    let mut new_states = build_seg_state_vec(segments);
    if let Ok(mut states) = seg_states.lock() {
        // 快照现有各段 worker 维护的 downloaded_bytes（index → bytes）。
        let existing: HashMap<i32, i64> = states
            .iter()
            .map(|s| (s.index, s.downloaded_bytes))
            .collect();
        // 仅恢复快照中已存在的段；split 新生子段不在其中，保持来自 map 的值。
        for s in &mut new_states {
            if let Some(&dl) = existing.get(&s.index) {
                s.downloaded_bytes = dl;
            }
        }
        *states = new_states;
    }
}

/// 把开放式首段右侧【字节连续且零进度】的 Pending 段全部并入该段。
///
/// DB 先行（单事务）：失败则放弃合并（内存不动），避免内存/DB 段布局分叉。
/// 成功吸收 ≥1 段时置位 `reconnect_hostile`（这条流从此是唯一生命线，掐流
/// 重连必被拒）并重建共享 seg_states。返回吸收的段数。
///
/// 两个调用点：连接级拒绝降级（服务器拒绝新连接但首段流仍活着）与 hint 模式
/// `RangeVerdict { supports_range: false }`（首段 plain GET 流是唯一生命线）。
async fn absorb_pending_into_open_stream(
    db: &Db,
    task_id: &str,
    segments: &mut BTreeMap<i32, LiveSegment>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    open_ended_streaming: Option<i32>,
    reconnect_hostile: &AtomicBool,
) -> usize {
    let Some(open_idx) = open_ended_streaming else {
        return 0;
    };
    let Some(open_end) = segments
        .get(&open_idx)
        .filter(|s| s.state == SegState::Active)
        .map(|s| s.end_byte)
    else {
        return 0;
    };
    let mut new_end = open_end;
    let mut absorbed: Vec<i32> = Vec::new();
    while let Some((idx, end)) = segments
        .values()
        .find(|s| {
            s.state == SegState::Pending && s.downloaded_bytes == 0 && s.start_byte == new_end + 1
        })
        .map(|s| (s.index, s.end_byte))
    {
        new_end = end;
        absorbed.push(idx);
    }
    if absorbed.is_empty() {
        return 0;
    }
    match db
        .persist_merge(task_id, open_idx, new_end, &absorbed)
        .await
    {
        Ok(()) => {
            for idx in &absorbed {
                segments.remove(idx);
            }
            if let Some(seg) = segments.get_mut(&open_idx) {
                seg.end_byte = new_end;
            }
            rebuild_seg_states(segments, seg_states);
            // 吸收合并 = 配额型端点实锤：这条流从此是唯一生命线，重连必被拒。
            // 提高其停滞容忍，禁止轻易掐流。
            reconnect_hostile.store(true, Ordering::Relaxed);
            log_info!(
                "[coordinator] task {} 开放式首段 #{} 吸收 {} 个 Pending 段（新终点 {}），\
                 复用现有连接续流到文件尾",
                task_id,
                open_idx,
                absorbed.len(),
                new_end
            );
            absorbed.len()
        }
        Err(e) => {
            log_info!(
                "[coordinator] task {} persist_merge 失败：{}，放弃合并走常规降级",
                task_id,
                e
            );
            0
        }
    }
}

/// Sync the coordinator's `downloaded_bytes` for Active segments from the
/// shared state (which workers update in real-time).
///
/// Without this, `try_split_largest` would calculate split points based on
/// the initial `downloaded_bytes` at assignment time, potentially placing the
/// split within an already-downloaded region.
fn sync_downloaded_from_shared(
    segments: &mut BTreeMap<i32, LiveSegment>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
) {
    let snapshot = match seg_states.lock() {
        Ok(guard) => guard.clone(),
        Err(e) => e.into_inner().clone(),
    };
    for info in &snapshot {
        if let Some(seg) = segments.get_mut(&info.index)
            && seg.state == SegState::Active
        {
            seg.downloaded_bytes = info.downloaded_bytes;
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: persist segment changes to DB
// ---------------------------------------------------------------------------

/// Persist a segment change (new segment from split, or re-assigned pending)
/// and optionally the parent whose end_byte was shrunk.
///
/// When a `split_parent` is provided, both the child upsert and the parent
/// end_byte update are persisted in a **single** SQLite transaction via
/// `Db::persist_split`, preventing crash-induced overlaps.
///
/// Reassignments preserve the existing durable DB offset; only new splits write rows.
async fn persist_segment_change(
    db: &Db,
    task_id: &str,
    segments: &BTreeMap<i32, LiveSegment>,
    changed_index: i32,
    split_parent: Option<i32>,
) -> Result<(), DownloadError> {
    // Reassignments already have a DB row. Never upsert live (not yet synced)
    // progress from the visualization map as a durable resume checkpoint.
    let Some(parent_idx) = split_parent else {
        return Ok(());
    };
    let child = segments
        .get(&changed_index)
        .ok_or_else(|| DownloadError::Other(format!("missing split child {changed_index}")))?;
    let parent = segments
        .get(&parent_idx)
        .ok_or_else(|| DownloadError::Other(format!("missing split parent {parent_idx}")))?;
    db.persist_split(
        task_id,
        child.index,
        child.start_byte,
        child.end_byte,
        0,
        parent.index,
        parent.end_byte,
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: send split event to the host
// ---------------------------------------------------------------------------

/// Emit an `EngineEvent::SegmentSplit` so the host can animate the split.
/// 字节区间经 `scope` 平移进任务坐标（索引不动），与 ProgressUpdate 快照的
/// 映射保持一致。
fn send_split_event(
    sink: &dyn EventSink,
    task_id: &str,
    parent_idx: i32,
    child_idx: i32,
    segments: &BTreeMap<i32, LiveSegment>,
    is_proactive: bool,
    scope: ReportScope,
) {
    let Some(parent) = segments.get(&parent_idx) else {
        return;
    };
    let Some(child) = segments.get(&child_idx) else {
        return;
    };

    sink.emit(EngineEvent::SegmentSplit {
        task_id: task_id.to_string(),
        parent_index: parent_idx,
        parent_new_end: parent.end_byte + scope.base,
        child_index: child_idx,
        child_start: child.start_byte + scope.base,
        child_end: child.end_byte + scope.base,
        is_proactive,
        total_segments: segments.len() as i32,
    });

    log_info!(
        "[coordinator] split event sent: parent={} new_end={}, child={} [{}, {}], proactive={}, total={}",
        parent_idx,
        parent.end_byte,
        child_idx,
        child.start_byte,
        child.end_byte,
        is_proactive,
        segments.len()
    );
}

// ---------------------------------------------------------------------------
// Worker implementation
// ---------------------------------------------------------------------------

/// Spawn a worker task that loops: receive assignment → download segment → report.
///
/// The worker reuses its HTTP client (and thus TCP/TLS connections) across
/// multiple segment assignments — achieving IDM-style connection reuse.
#[allow(clippy::too_many_arguments)]
fn spawn_worker(
    worker_id: usize,
    mut assign_rx: mpsc::Receiver<WorkerAssignment>,
    event_tx: mpsc::Sender<WorkerEvent>,
    task_id: String,
    url: String,
    dest: PathBuf,
    temp_file_guard: TempFileGuard,
    planned_total: Arc<AtomicI64>,
    size_is_estimate: bool,
    first_validators: Arc<StdMutex<Option<(String, String)>>>,
    nodes: Arc<NodePool>,
    cancel_token: CancellationToken,
    conn_sensitive: Arc<AtomicBool>,
    reconnect_hostile: Arc<AtomicBool>,
    range_verdict: Arc<AtomicU8>,
    total_downloaded: Arc<AtomicI64>,
    generation_evidence: Arc<GenerationEvidence>,
    request_path: Arc<RequestPath>,
    seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    db: Db,
    speed_limiter: SpeedLimiter,
    spec: Arc<crate::downloader::DownloadSpec>,
    etag: String,
    last_modified: String,
    sync_gate: FileSyncGate,
    durable_progress: Arc<StdMutex<HashMap<i32, i64>>>,
    scope: ReportScope,
    spawn_gen: i64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _temp_file_guard = temp_file_guard;
        // Worker loop: keep accepting assignments until the channel closes.
        while let Some(assignment) = assign_rx.recv().await {
            if cancel_token.is_cancelled() {
                break;
            }

            // 每个 assignment 租借一个节点：段内重试沿用同一租约的 client
            //（keep-alive 连接不丢）；段失败重派后的下一次租借自然避开被
            // 降权/踢除的节点——段间节点切换零新增状态机。开放式首段 /
            // plain GET 是配额型端点的唯一生命线，只允许留在起飞路径。
            // 分段令牌是任务令牌的子令牌：coordinator 完成时间抢占只取消
            // 本段，任务级取消仍一并生效。
            let seg_cancel = cancel_token.child_token();
            let start_downloaded = assignment.actual_start - assignment.seg_start;
            let lease = nodes.lease_for(crate::cdn::node_pool::LeaseRequest {
                seg_index: assignment.seg_index,
                start_downloaded,
                bytes: assignment.seg_end - assignment.actual_start + 1,
                allow_alternates: !assignment.open_ended && !assignment.no_range,
                cancel: seg_cancel.clone(),
            });
            let seg_started = Instant::now();

            let result = do_segment_with_retry(
                &task_id,
                assignment.seg_index,
                &url,
                &dest,
                assignment.seg_start,
                assignment.actual_start,
                assignment.seg_end,
                assignment.open_ended,
                assignment.no_range,
                // SYS 租约在 Auto 多路径（有存活备选路径、非生命线段）下对传输层
                // 失败快速放手交给节点池改道；段内每次失败的预算按错误类别选取
                // （见 `segment_retry_budget`），钉定节点 / 备选路径恒收紧。
                sys_fast_release(
                    nodes.sys_fallback_available(),
                    assignment.open_ended,
                    assignment.no_range,
                ),
                !lease.is_attributable(),
                lease.client(),
                &seg_cancel,
                &conn_sensitive,
                &reconnect_hostile,
                &range_verdict,
                &event_tx,
                &total_downloaded,
                &generation_evidence,
                &request_path,
                &planned_total,
                size_is_estimate,
                &first_validators,
                &db,
                &seg_states,
                &speed_limiter,
                &spec,
                &etag,
                &last_modified,
                &sync_gate,
                &durable_progress,
                scope,
                spawn_gen,
            )
            .await;

            // 本租约实际传输字节：成功取返回的段总量，失败/被抢占取取消与
            // 停滞分支刷入共享 seg_states 的进度；减去租约起点即为本租约贡献。
            let final_downloaded = match &result {
                Ok(downloaded) => *downloaded,
                Err(_) => seg_states
                    .lock()
                    .ok()
                    .and_then(|states| {
                        states
                            .iter()
                            .find(|s| s.index == assignment.seg_index)
                            .map(|s| s.downloaded_bytes)
                    })
                    .unwrap_or(start_downloaded),
            };
            let transferred = (final_downloaded - start_downloaded).max(0) as u64;
            nodes.record_transfer(&lease, transferred);

            // 健康度回报：成功为未实测的节点提供初值（≥256KB 段），失败降权/踢除。
            // 取消不回报（非节点信号）。归还（并发额度）由 lease Drop 承担。
            match &result {
                Ok(_) => nodes.report(&lease, transferred, seg_started.elapsed(), Ok(())),
                Err(DownloadError::Cancelled) => {}
                Err(e) => nodes.report(&lease, 0, seg_started.elapsed(), Err(e)),
            }
            let lease_attributable = lease.is_attributable();
            let lease_desc = lease.describe();
            drop(lease);

            match result {
                Ok(downloaded) => {
                    if event_tx
                        .send(WorkerEvent::Done {
                            worker_id,
                            seg_index: assignment.seg_index,
                            downloaded_bytes: downloaded,
                        })
                        .await
                        .is_err()
                    {
                        tracing::debug!("HTTP coordinator event receiver closed");
                        break;
                    }
                }
                Err(DownloadError::Cancelled) if !cancel_token.is_cancelled() => {
                    // 仅本段令牌被取消 = coordinator 完成时间抢占：进度已在
                    // 取消分支刷盘并写入 seg_states；上报后保活等待新派工。
                    if event_tx
                        .send(WorkerEvent::Preempted {
                            worker_id,
                            seg_index: assignment.seg_index,
                        })
                        .await
                        .is_err()
                    {
                        tracing::debug!("HTTP coordinator event receiver closed");
                        break;
                    }
                }
                Err(DownloadError::Cancelled) => {
                    // Don't report — coordinator already knows via cancel_token.
                    break;
                }
                Err(e) => {
                    // TrueSizeLarger 是【可恢复的协调级事件】而非 worker 致命错误：
                    // coordinator 就地扩容后会立刻给本 worker 重新派工，故报告后
                    // 【保活】等待下一个 assignment（coordinator 退休本 worker 或
                    // 结束时关闭 channel，recv 返回 None 自然退出）。其余错误维持
                    // 原语义：报告后退出。
                    //
                    // 多路径池的非 SYS 租约（钉定节点 / 备选路径）上，节点可归因
                    // 错误（连接失败/超时/停滞/validator 不一致/HTTP 拒绝）翻译为
                    // CdnNodeFailed（同为可恢复：coordinator 回收段重派，绝不升级
                    // 为任务失败）。备选路径（代理）另把 Range 失效/错位归因到
                    // 路径本身——代理改写响应不能被误学成源站不支持 Range 并触发
                    // 清盘回退。SYS 租约的错误保持原样——语义与单路径完全一致。
                    // 钉定节点与备选路径同样把 Range 失效/错位归因到节点/路径本身：
                    // 一个坏边缘节点或改写响应的代理不能被学成源站不支持 Range，
                    // 更不能触发清盘回退。
                    let attributable = crate::cdn::is_node_attributable(&e)
                        || (lease_attributable
                            && matches!(
                                e,
                                DownloadError::RangeNotSupported(_)
                                    | DownloadError::RangeMisaligned(_)
                            ));
                    // Auto 多路径：SYS 路径的传输层失败（连接重置/超时/停滞）在
                    // 仍有其它路径存活时同样回收重派——直连被中途阻断时由代理
                    // 接手，而不是整个任务失败。HTTP 状态/校验类错误保持原语义
                    // （源站拒绝学习、清盘回退）。判据是类型化传输失败分类
                    // （Io/Db 永不归类），与 OS 本地化文本无关。
                    let sys_transport_fallback = !lease_attributable
                        && crate::net_failure::classify_download_error(&e).is_some()
                        && nodes.try_sys_transport_fallback();
                    let e = if (lease_attributable && attributable) || sys_transport_fallback {
                        log_info!(
                            "[worker {}] task {} seg {} 节点 {} 可归因失败，翻译为 CdnNodeFailed: {}",
                            worker_id,
                            task_id,
                            assignment.seg_index,
                            lease_desc,
                            e
                        );
                        DownloadError::CdnNodeFailed(format!("node {lease_desc}: {e}"))
                    } else {
                        e
                    };
                    let recoverable = matches!(
                        e,
                        DownloadError::TrueSizeLarger(_) | DownloadError::CdnNodeFailed(_)
                    );
                    if event_tx
                        .send(WorkerEvent::Failed {
                            worker_id,
                            seg_index: assignment.seg_index,
                            error: e,
                        })
                        .await
                        .is_err()
                    {
                        tracing::debug!("HTTP coordinator event receiver closed");
                        break;
                    }
                    if !recoverable {
                        break;
                    }
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Segment download with retry
// ---------------------------------------------------------------------------

/// 等待响应头的上限（只覆盖发出请求到收到响应头，不限制 body 速度）。
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
/// 吸收合并后的唯一生命线（重连必被拒）：响应头等待更宽容。
const RESPONSE_HEADER_TIMEOUT_HOSTILE: Duration = Duration::from_secs(90);

/// HTTP 429 Too Many Requests 判定：明确的限流信号，窗口过去后同一请求可成功。
fn is_http_429(e: &DownloadError) -> bool {
    matches!(
        e,
        DownloadError::Request(req_err)
            if req_err.status() == Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
    )
}

/// HTTP 400 Bad Request 判定。
///
/// 配额型下载端点（fnOS `multiple-download?token=`：一个 token 只允许固定次数
/// 成功 GET）对超额请求返回 400 而非 403/429。coordinator 仅在同 generation
/// 仍有其他请求在途时把 400 归入连接级拒绝走降级；且 400 绝不写入域名连接数
/// 缓存（语义宽泛，避免把坏 URL 学习成主机级降速）。
fn is_http_400(e: &DownloadError) -> bool {
    matches!(
        e,
        DownloadError::Request(req_err)
            if req_err.status() == Some(reqwest::StatusCode::BAD_REQUEST)
    )
}

/// Download a single segment with automatic retry on transient failures.
/// Returns the total `downloaded_bytes` for this segment on success.
#[allow(clippy::too_many_arguments)]
async fn do_segment_with_retry(
    task_id: &str,
    seg_idx: i32,
    url: &str,
    dest: &Path,
    seg_start: i64,
    mut actual_start: i64,
    mut seg_end: i64,
    open_ended: bool,
    no_range: bool,
    // SYS 租约是否在传输层失败时快速放手（Auto 多路径有存活备选路径、非
    // 生命线段）：为真时传输失败按 PINNED_NODE_MAX_RETRIES 上抛交给节点池
    // 改道；HTTP 类错误不受影响。非 SYS 租约恒按 PINNED_NODE_MAX_RETRIES。
    fast_release: bool,
    sys_lease: bool,
    client: &Client,
    cancel: &CancellationToken,
    conn_sensitive: &AtomicBool,
    reconnect_hostile: &AtomicBool,
    range_verdict: &AtomicU8,
    event_tx: &mpsc::Sender<WorkerEvent>,
    total_downloaded: &AtomicI64,
    generation_evidence: &GenerationEvidence,
    request_path: &RequestPath,
    planned_total: &AtomicI64,
    size_is_estimate: bool,
    first_validators: &StdMutex<Option<(String, String)>>,
    db: &Db,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    speed_limiter: &SpeedLimiter,
    spec: &crate::downloader::DownloadSpec,
    expected_etag: &str,
    expected_last_modified: &str,
    sync_gate: &FileSyncGate,
    durable_progress: &StdMutex<HashMap<i32, i64>>,
    scope: ReportScope,
    spawn_gen: i64,
) -> Result<i64, DownloadError> {
    let mut attempts = 0u32;
    // 本段累计瞬时失败次数（进展重置预算时也不清零），封顶见 RETRY_TOTAL_CEILING_FACTOR。
    let mut total_failures = 0u32;
    // open-ended 首响应 Established 事件 latch：整段重试生命周期内只发一次。
    let mut open_ended_established_sent = false;

    loop {
        match do_segment(
            task_id,
            seg_idx,
            url,
            dest,
            seg_start,
            actual_start,
            seg_end,
            open_ended,
            no_range,
            sys_lease,
            client,
            cancel,
            conn_sensitive,
            reconnect_hostile,
            &mut open_ended_established_sent,
            range_verdict,
            event_tx,
            total_downloaded,
            generation_evidence,
            request_path,
            planned_total,
            size_is_estimate,
            first_validators,
            db,
            seg_states,
            speed_limiter,
            spec,
            expected_etag,
            expected_last_modified,
            sync_gate,
            durable_progress,
            scope,
            spawn_gen,
        )
        .await
        {
            Ok(dl) => {
                // 插件 `maxRangeBytes`：一次请求只覆盖一个窗口。段未写满时在段内接着发
                // 下一个窗口（复用连接池），成功的窗口不消耗重试预算、不退避。
                if spec.current().range_window_cap().is_none() {
                    return Ok(dl);
                }
                // 段尾可能已被 coordinator 拆分收窄 / 吸收扩大，以共享状态为准。
                let mut end = seg_end;
                if let Ok(states) = seg_states.lock()
                    && let Some(pos) = seg_state_pos(&states, seg_idx)
                {
                    end = states[pos].end_byte;
                }
                let next = seg_start + dl;
                if next > end {
                    return Ok(dl);
                }
                if next <= actual_start {
                    return Err(DownloadError::Other(format!(
                        "segment {seg_idx} made no progress within a Range window \
                         (at {next}, segment end {end})"
                    )));
                }
                actual_start = next;
                seg_end = end;
                attempts = 0;
            }
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            // 文件版本变化：重试必然拿到同样的 200（validator 永不再匹配），且旧数据
            // 已作废。立即返回，让 coordinator 走 Path B → run_download_inner 清空临时
            // 文件 + 重下新版本；绝不当瞬时错误退避重试（那会空烧退避并误入串行降级
            // 死循环，最终以误导性的"服务器拒绝所有连接"报错）。
            Err(e @ DownloadError::VersionChanged(_)) => return Err(e),
            // Range 错位：服务器回 206 却发【从 0 的全量流】（Content-Range 起点不符，
            // 如 123 盘失效签名 URL）。系统性错误——重试必然拿到同样错位响应，且已写入
            // 的数据是错位垃圾。立即返回让 coordinator 走 Path B → run_download_inner
            // 清空临时文件 + 回退单流；绝不当瞬时错误退避重试。
            Err(e @ DownloadError::RangeMisaligned(_)) => return Err(e),
            // 服务器自报真实大小明显大于规划：系统性错误（重试必然拿到同样的
            // Content-Range 分母），需要 coordinator 就地扩容（延长预分配 + 追加
            // 尾段）后重新派工。
            // 立即返回，绝不当瞬时错误退避重试（BUG-HTTP-HINT-UNDERSIZED）。
            Err(e @ DownloadError::TrueSizeLarger(_)) => return Err(e),
            // RangeNotSupported 的两义性处理：
            //   • 没有任何可保留进度：立即返回，让 coordinator 快速回退单流；
            //   • 已有进度（含恢复前历史进度）：按瞬时 200 重试，避免一次响应
            //     波动清掉大文件断点。
            Err(e @ DownloadError::RangeNotSupported(_))
                if total_downloaded.load(Ordering::Relaxed) == 0 =>
            {
                return Err(e);
            }
            // 插件在凭据刷新阶段以 `onFailure: "fail"` 中止任务：重试不会改变插件的决定，
            // 立即作为任务级失败上抛。
            Err(e) if crate::downloader::is_plugin_abort(&e) => return Err(e),
            // 本地磁盘写满：腾出空间前不会自愈，退避重试只会空烧预算。立即作为
            // 致命错误上抛（任务级自动重试的瞬时错误白名单也不含它）。
            Err(e) if is_disk_full(&e) => {
                log_info!(
                    "[segment-retry] task {} seg {} 本地磁盘空间不足，跳过重试直接上报",
                    task_id,
                    seg_idx
                );
                return Err(e);
            }
            // BUG-HTTP-416-RETRY-EXHAUST：416 Range Not Satisfiable 是服务器对
            // 当前 Range 的明确拒绝，重试同一区间必然拿到同样的 416——不能像
            // 瞬时网络错误一样烧完 max_retries 次重试预算才失败。仅当该段自身
            // 从未确认过任何字节（actual_start == seg_start）才立即短路；已有
            // 进度的段保留在下方瞬时错误分支重试，一次孤立的 416 不该扔掉整段
            // 已下的字节。归一为 RangeNotSupported 复用其既有上抛路径
            // （coordinator 捕获后触发单流回退，清空临时文件从头重下）。
            Err(e) if is_range_not_satisfiable(&e) && actual_start == seg_start => {
                log_info!(
                    "[segment-retry] task {} seg {} 收到 416 Range Not Satisfiable 且该段无\
                     历史进度，跳过重试直接上报触发单流回退",
                    task_id,
                    seg_idx
                );
                return Err(DownloadError::RangeNotSupported(format!("416 ({e})")));
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(e);
                }
                // 403/429 是服务器明确拒绝多连接；400 在配额型端点同样意味着
                // "这条连接不会被服务"（见 is_http_400）——重试只会空烧退避，
                // 还会拖慢 coordinator 的开放式首段吸收时机（需要在其余 worker
                // 尽快报告失败后立即扩容其预算）。跳过重试直接上报分类处置。
                if is_server_rejection(&e) || is_http_400(&e) {
                    log_info!(
                        "[segment-retry] task {} seg {} 收到服务器拒绝，跳过重试直接上报",
                        task_id,
                        seg_idx
                    );
                    return Err(e);
                }
                // no_range 段（hint 首枪 plain GET）已写入部分字节后中途断流/停滞：
                // 若 Range 裁决为不支持，重连只能带 Range（plain GET 无法从偏移
                // 续传），而带 Range 会作废配额型 token（fnOS 实测）——不重试，
                // 直接上报（fail-loud + 数据保留），把"是否重下"留给用户决策，
                // 绝不替端点烧掉可能仍有效的 token。
                if no_range
                    && actual_start > seg_start
                    && range_verdict.load(Ordering::Relaxed) == RANGE_VERDICT_UNSUPPORTED
                {
                    log_info!(
                        "[segment-retry] task {} seg {} plain 流中途断开且服务器不支持 Range，\
                         跳过带 Range 的重连直接上报",
                        task_id,
                        seg_idx
                    );
                    return Err(e);
                }
                // Recover actual_start *and* seg_end from DB for partial progress.
                // seg_end may have been shrunk by a coordinator split since we started.
                let attempt_start = actual_start;
                let segs = db.load_segments(task_id).await?;
                if let Some(seg) = segs.iter().find(|s| s.index == seg_idx) {
                    seg_end = seg.end_byte;
                    actual_start = seg_start + seg.downloaded_bytes;
                    if actual_start > seg_end {
                        // Segment completed during previous attempt.
                        return Ok(seg.downloaded_bytes);
                    }
                }
                // 预算按「连续无实质进展的失败」计：本次尝试落盘超过
                // RETRY_PROGRESS_RESET_BYTES（慢链路上常见的下一段 → 停滞 → 重连）就
                // 重新计数，不让长时间下载因零星断流累计耗尽预算而整段失败；涓流式
                // 「每次只给一块就断」的服务器既重置不了预算，也受总次数封顶约束。
                total_failures += 1;
                if actual_start - attempt_start >= RETRY_PROGRESS_RESET_BYTES {
                    attempts = 0;
                }
                attempts += 1;
                // 预算按本次错误类别选取（见 `segment_retry_budget`）：快放手只对
                // 传输层失败生效，HTTP 类瞬时错误仍按原预算段内重试。
                let max_retries = segment_retry_budget(
                    !sys_lease,
                    fast_release,
                    crate::net_failure::classify_download_error(&e).is_some(),
                );
                if attempts >= max_retries
                    || total_failures >= max_retries.saturating_mul(RETRY_TOTAL_CEILING_FACTOR)
                {
                    return Err(e);
                }
                // 瞬时失败必须留痕：截断/停滞/网络错误此前静默进退避，日志里
                // 只见"最后一击"（如重连撞 400），无法还原首因（断流 vs 停滞）。
                log_info!(
                    "[segment-retry] task {} seg {} 瞬时失败（第 {}/{} 次重试前）：{}",
                    task_id,
                    seg_idx,
                    attempts,
                    max_retries,
                    e
                );
                let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                tokio::select! {
                    _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
                if event_tx
                    .send(WorkerEvent::Retrying {
                        seg_index: seg_idx,
                        attempt: attempts,
                        error: e.to_string(),
                    })
                    .await
                    .is_err()
                {
                    tracing::debug!("HTTP coordinator event receiver closed");
                    return Err(DownloadError::Cancelled);
                }
            }
        }
    }
}

/// Download a single segment.  Returns `downloaded_bytes` for the segment.
///
/// The worker detects dynamic segment shrinking (from coordinator splits) by
/// reading the shared `seg_states` **before** each write.  Writes are truncated
/// at the effective boundary to prevent cross-segment data corruption.
#[allow(clippy::too_many_arguments)]
async fn do_segment(
    task_id: &str,
    seg_idx: i32,
    url: &str,
    dest: &Path,
    seg_start: i64,
    actual_start: i64,
    seg_end: i64,
    open_ended: bool,
    // hint 模式（Range 未经验证）的首连接：不带 Range/If-Range 的 plain GET，
    // 仅在 actual_start == 0 时生效（重试推进起点后自然回到带 Range 路径）。
    no_range: bool,
    // 当前租约是否 SYS 槽位。钉定节点 / 备选路径上的非 206 与压缩响应是该节点
    // 自身的问题，不得学习成任务级 conn_sensitive 或 24h 域名单连接缓存。
    sys_lease: bool,
    client: &Client,
    cancel: &CancellationToken,
    conn_sensitive: &AtomicBool,
    reconnect_hostile: &AtomicBool,
    // open-ended 首响应 Established 事件 latch（由 with_retry 持有，重试不重发）。
    open_ended_established_sent: &mut bool,
    // Range 能力裁决（RANGE_VERDICT_*）：仍为 UNKNOWN 时由本函数依据首响应
    // 裁决（206 或 `Accept-Ranges: bytes` → SUPPORTED，否则 UNSUPPORTED），
    // 并经 `event_tx` 通知 coordinator（吸收/放行 ramp）。
    range_verdict: &AtomicU8,
    event_tx: &mpsc::Sender<WorkerEvent>,
    total_downloaded: &AtomicI64,
    generation_evidence: &GenerationEvidence,
    // 跨 worker 共享的重定向终点与请求建立延迟观测（见 request_path 模块）。
    request_path: &RequestPath,
    // 当前规划总大小的共享视图（coordinator 就地扩容时更新，见 planned_total 注释）。
    planned_total: &AtomicI64,
    // `planned_total` 是否为【未经 probe 验证的估计值】（fresh hint 模式）。true 时
    // Content-Range 分母才是权威真实大小，扩容检查采零容差；见下方 size-check 注释。
    size_is_estimate: bool,
    // hint 模式跨段版本一致性基线（首个 206 的 validator latch），见
    // check_cross_segment_validators。
    first_validators: &StdMutex<Option<(String, String)>>,
    db: &Db,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    speed_limiter: &SpeedLimiter,
    spec: &crate::downloader::DownloadSpec,
    expected_etag: &str,
    expected_last_modified: &str,
    sync_gate: &FileSyncGate,
    durable_progress: &StdMutex<HashMap<i32, i64>>,
    scope: ReportScope,
    spawn_gen: i64,
) -> Result<i64, DownloadError> {
    if actual_start > seg_end {
        // Already complete.
        return Ok(seg_end - seg_start + 1);
    }

    // 插件 `maxRangeBytes`：本次请求的 Range 跨度 ≤ cap。段内剩余区间由
    // `do_segment_with_retry` 逐窗口衔接；无 cap 时 `request_end == seg_end`，行为不变。
    let window_cap = spec.current().range_window_cap();
    let windowed = window_cap.is_some();
    let request_end = match window_cap {
        Some(cap) => seg_end.min(actual_start.saturating_add(cap - 1)),
        None => seg_end,
    };
    // 本请求允许写入的末字节：窗口模式下不得越过窗口末尾（含开放式首段的预算延伸）。
    let write_limit = move |effective_end: i64| {
        if windowed {
            effective_end.min(request_end)
        } else {
            effective_end
        }
    };
    // hint 模式首连接完全不带 Range；其余分段与 aria2 一致，只发送 Range。
    // 部分一次性签名端点会把 If-Range 视为签名外请求头并立即作废 URL，
    // 收到 403 后再重试纯 Range 已无法恢复。版本一致性由 206 响应的
    // ETag/Last-Modified 后验校验保证。有 cap 的源拒绝无 Range / 开放式 Range，
    // 首连接也必须带有界 Range。
    let plain_first = no_range && actual_start == 0 && !windowed;
    // 多段下载始终用 GET——上游 resolve_file_info 已确保 spec.is_get_like()。
    let range = if open_ended && !windowed {
        format!("bytes={actual_start}-")
    } else {
        format!("bytes={}-{}", actual_start, request_end)
    };
    // 等响应头与 cancel、头超时竞速：惩罚型/停滞服务器可能接受 TCP 连接后长时间
    // 不回响应头（client 只有 connect_timeout，响应头无超时），不竞速则删除/取消
    // 被卡住、尾段一条沉默连接拖住整个任务。超时只覆盖等头阶段，不限制 body 速度。
    let header_timeout = if reconnect_hostile.load(Ordering::Relaxed) {
        RESPONSE_HEADER_TIMEOUT_HOSTILE
    } else {
        RESPONSE_HEADER_TIMEOUT
    };
    // 重定向终点复用：只用于 SYS 租约的 Range 请求。钉定节点 / 备选路径按自身路由
    // 解析原始主机；hint 首连接 plain GET 是配额型端点的生命线，均保持原始 URL。
    let reuse_redirect = sys_lease && !plain_first;
    let mut direct = if reuse_redirect {
        request_path.redirect_target()
    } else {
        None
    };
    // hint 模式跳过了探测，反机器人质询只能在真实请求上首次暴露：换中性 UA 重发
    // 同一请求（`escape_bot_challenge` 保证每个请求最多重发一次，循环必然收敛）。
    let mut used = spec.current();
    let mut refreshed = false;
    let resp = loop {
        // 单 GET 探测保留的首响应（一次性 URL 的唯一一次请求）：首连接直接消费它，不再发请求。
        // 只有零进度的 plain 首连接会取；重试 / 其余分段照常发 Range 请求。
        if plain_first && let Some(held) = spec.take_held_response() {
            break held;
        }
        // 仅任务 URL 经凭据刷新改写；重定向终点 `direct` 保持不变。凭据锚点恒为（改写后的）
        // 任务 URL：直连终点与经跳转链抵达终点携带完全相同的 Cookie / Authorization。
        let routed = spec.route_url(url);
        let origin_target = routed.as_deref().unwrap_or(url);
        let target = direct.as_deref().unwrap_or(origin_target);
        let mut req = crate::downloader::build_request(client, target, reqwest::Method::GET, &used)
            .anchor(origin_target);
        if !plain_first {
            req = req.header("Range", &range);
        }
        let sent_at = Instant::now();
        let sent = tokio::select! {
            _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
            r = tokio::time::timeout(header_timeout, req.send()) => r,
        };
        // 直连终点失败（传输错误、头超时或非 206）：终点可能已过期、一次性或按请求
        // 分配（过期终点常回 200 全量 / HTML 错误页）。本任务内放弃复用并立即改发
        // 原始 URL——不消耗段重试预算，也不把终点的 403 / 200 误当成服务器拒绝多连接
        // 或 Range 失效。
        if direct.is_some() {
            let failure = match &sent {
                Ok(Ok(r)) if r.status() == reqwest::StatusCode::PARTIAL_CONTENT => None,
                Ok(Ok(r)) => Some(r.status().to_string()),
                Ok(Err(e)) => Some(e.to_string()),
                Err(_) => Some(format!(
                    "no response headers within {}s",
                    header_timeout.as_secs()
                )),
            };
            if let Some(failure) = failure {
                log_info!(
                    "[coordinator] task {} seg {} 重定向终点直连失败（{}），本任务改回原始 URL",
                    task_id,
                    seg_idx,
                    failure
                );
                request_path.abandon_redirect();
                direct = None;
                continue;
            }
        }
        let resp = match sent {
            Ok(r) => r?,
            Err(_) => {
                return Err(DownloadError::Other(format!(
                    "segment {seg_idx} stalled: no response headers received within {}s",
                    header_timeout.as_secs()
                )));
            }
        };
        request_path.record_setup(sent_at.elapsed());
        match spec.escape_bot_challenge(&used, &resp, db, task_id).await {
            Some(neutral) => {
                drop(resp);
                used = neutral;
            }
            None => {
                // 被拒（401 / 403 / 410）且有插件声明 refresh：换发凭据后重发同一请求，
                // 每个请求至多一次，不消耗段重试预算。
                if !refreshed
                    && !resp.status().is_success()
                    && let Some(fresh) = spec.refresh_after_rejection(&used, resp.status()).await?
                {
                    refreshed = true;
                    drop(resp);
                    used = fresh;
                    continue;
                }
                break resp;
            }
        }
    };
    let resp = resp.error_for_status()?;
    spec.confirm(&used);
    if reuse_redirect && direct.is_none() && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        request_path.learn_redirect(url, resp.url().as_str());
    }

    // --- Range 能力裁决（hint 模式，每任务恰好一次）------------------------
    // 依据首个成功响应：206（服务器履行了 Range）或 200 携带
    // `Accept-Ranges: bytes`（plain GET 下服务器自述支持）→ SUPPORTED；
    // 否则 UNSUPPORTED。CAS 保证只裁决一次；事件必须在进入写循环【之前】
    // 送出——UNSUPPORTED 时 coordinator 要赶在本段预算耗尽前完成吸收合并。
    if range_verdict.load(Ordering::Relaxed) == RANGE_VERDICT_UNKNOWN {
        let supports_range = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT
            || resp
                .headers()
                .get(reqwest::header::ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case("bytes"));
        let verdict = if supports_range {
            RANGE_VERDICT_SUPPORTED
        } else {
            RANGE_VERDICT_UNSUPPORTED
        };
        if range_verdict
            .compare_exchange(
                RANGE_VERDICT_UNKNOWN,
                verdict,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
            && event_tx
                .send(WorkerEvent::RangeVerdict { supports_range })
                .await
                .is_err()
        {
            tracing::debug!("HTTP coordinator event receiver closed");
            return Err(DownloadError::Cancelled);
        }
    }

    // --- Range support verification ----------------------------------------
    // We sent a `Range: bytes=X-Y` header; the server MUST respond with 206
    // Partial Content if it honours Range requests.  A 200 OK response means
    // the server ignored the Range header and is streaming the full file from
    // byte 0 — writing that body at `actual_start` would overwrite adjacent
    // segments and silently corrupt the assembled output file.
    //
    // Observed with FnOS NAS "multiple-download?token=..." endpoints: the
    // server accepts the Range header syntactically but always replies 200 +
    // full content, making multi-segment assembly impossible.
    //
    // Fix: record the host so future tasks automatically use single-stream
    // mode (24 h TTL via the existing single-conn cache); return an error so
    // the coordinator cancels all workers for the current attempt.  On retry
    // the cached policy kicks in and the download proceeds in single-stream.
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        // Range 返回 non-206 时先依据响应 validator 区分文件版本变化与真正不支持
        // Range；不能把一次文件替换误记成整个域名 24h 单连接。
        let resp_etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let resp_lm = resp
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let version_changed = !plain_first
            && ((!expected_etag.is_empty() && !resp_etag.is_empty() && resp_etag != expected_etag)
                || (!expected_last_modified.is_empty()
                    && !resp_lm.is_empty()
                    && resp_lm != expected_last_modified));
        // validator 变化：清空旧数据重下新版本；不污染域名连接能力缓存。
        if version_changed {
            return Err(DownloadError::VersionChanged(resp.status().to_string()));
        }

        // 开放式首段 + 从 0 起：200 全量流与我们请求的字节完全一致（服务器
        // 忽略/不支持 Range 时对 `bytes=0-` 返回整文件），直接采用——写循环按
        // 共享预算截断，行为与 206 无异。不置 conn_sensitive、不记录单连接
        // 缓存（这条流本身就是最优路径；其余 worker 的非 206 照常走信号路径）。
        let accepted_open_ended_200 = open_ended
            && actual_start == 0
            && resp.status() == reqwest::StatusCode::OK
            && request_end >= seg_end;
        if !accepted_open_ended_200 {
            // 连接敏感信号：服务器对 Range 请求返回了非 206（且非版本变化）——典型于 alist
            // 代理云盘在连接压力下偶发全量响应。置位后 coordinator 衰减【主动拆分】以降低连接
            // churn，减少后续瞬时 200 的发生（保留 reactive 拆分做尾段抢救）。一次性 latch，
            // 仅首次置位打日志。
            if sys_lease && !conn_sensitive.swap(true, Ordering::Relaxed) {
                log_info!(
                    "[coordinator] task {} 检测到连接敏感（服务器对 Range 请求返回 {}），\
                 停止主动拆分以降低连接 churn",
                    task_id,
                    resp.status()
                );
            }
            // 历史或本轮任一成功 Range 都证明该主机具备 Range 能力；偶发 200
            // 不能把整个域名标记成单连接。
            if sys_lease && total_downloaded.load(Ordering::Relaxed) == 0 {
                record_single_conn_domain_persist(url, db);
            }
            return Err(DownloadError::RangeNotSupported(resp.status().to_string()));
        }
        log_info!(
            "[coordinator] task {} seg {} 开放式首段收到 200 全量流（{}），\
             从 0 起字节等价，直接采用",
            task_id,
            seg_idx,
            if plain_first {
                "hint 首连接 plain GET"
            } else {
                "服务器忽略 Range"
            }
        );
    }

    // --- Content-Range 起点对齐校验（BUG-CDN-206-BYTE0-FULLSTREAM）----------
    // 收到 206 不代表服务器真的返回了我们请求的区间。劣质 CDN（如 123 盘免费下载
    // 节点）在签名 URL 失效/超配额时，对 `Range: bytes=X-Y` 回 206 却发【从 byte 0
    // 的全量流】（Content-Range 起点为 0，或整体缺失）。若不校验，seek(actual_start)
    // 后写入的是文件开头字节 → 字节数写满区间（骗过末尾仅校验字节数量的完整性
    // 检查），但内容整体错位 → 完整大小的损坏文件（无 checksum 时无法察觉）。
    // 这里断言 Content-Range 起点 == 本段请求起点，不符则返回 RangeMisaligned，由
    // coordinator 走 Path B 回退单流（单流全量请求不带 Range，服务器"忽略 Range 返
    // 全量"的行为反而正常，能下到正确文件）。不置 conn_sensitive——这不是连接压力
    // 而是链接失效，且随后 Path B 会取消所有 worker。
    let cr_start = crate::downloader::parse_content_range_start(resp.headers());
    if crate::downloader::is_range_response_misaligned(cr_start, actual_start) {
        log_info!(
            "[coordinator] task {} seg {} Content-Range 错位：请求起点 {} 但响应起点 {:?}\
             （服务器在 206 上返回错位/从-0 的全量流），回退单流",
            task_id,
            seg_idx,
            actual_start,
            cr_start
        );
        return Err(DownloadError::RangeMisaligned(format!(
            "segment {}: requested Range start {} but response Content-Range start is {:?}",
            seg_idx, actual_start, cr_start
        )));
    }

    // --- ETag / Last-Modified consistency check -----------------------------
    // Verify that this segment's response comes from the same file version as
    // the initial probe.  A mismatch means the server updated the file while
    // we're downloading — the resulting file would be a corrupt splice of two
    // different versions. An ETag mismatch is always fatal (strong validator);
    // a Last-Modified-only mismatch is tolerated when the response's
    // `Content-Range` total and start agree with the known total/offset —
    // that pattern is CDN edge clock/format drift on identical content, not
    // an actual file change (see [`crate::downloader::validator_mismatch_is_fatal`]).
    //
    // 版本一致性守卫【先于】下方的“真实大小 > 规划”扩容检查执行：文件在下载中途被
    // 替换应被判定为【版本变化】（fail-fast），而不是先触发一次整体扩容重下、再在
    // 陈旧 validator 上失败。（fresh hint 模式下 expected_etag 为空 → 守卫被跳过，此
    // 次序调整只影响 probe 验证过的下载。）
    //
    // Only check when the probe returned a non-empty value AND the segment
    // response also provides the header.  Many CDN edge servers strip these
    // headers on Range responses, so a missing header is not an error.
    let resp_etag_str = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let resp_lm_str = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    generation_evidence.note_response_validators(resp_etag_str, resp_lm_str);
    let etag_mismatch =
        !expected_etag.is_empty() && !resp_etag_str.is_empty() && resp_etag_str != expected_etag;
    let last_modified_mismatch = !expected_last_modified.is_empty()
        && !resp_lm_str.is_empty()
        && resp_lm_str != expected_last_modified;
    if etag_mismatch || last_modified_mismatch {
        let content_range_total = crate::downloader::parse_content_range_total(resp.headers());
        let known_total = planned_total.load(Ordering::Relaxed);
        if crate::downloader::validator_mismatch_is_fatal(
            etag_mismatch,
            last_modified_mismatch,
            content_range_total,
            known_total,
            cr_start,
            Some(actual_start),
        ) {
            // VersionChanged 而非 Other：与非 206 路径一致，do_segment_with_retry
            // 立即短路（不退避重试），并走 Path B 清盘重下新版本。
            return Err(DownloadError::VersionChanged(format!(
                "segment {seg_idx}: validator mismatch — probe etag=\"{expected_etag}\" \
                 lm=\"{expected_last_modified}\", segment etag=\"{resp_etag_str}\" \
                 lm=\"{resp_lm_str}\". The file may have changed on the server during download."
            )));
        }
        generation_evidence.note_lm_drift_tolerated();
        log_warn!(
            "[coordinator] task {} seg {} Last-Modified 跨 CDN edge 不同（probe=\"{}\" 本段=\"{}\"）\
             但 Content-Range 总大小（{:?}，已知 {}）与请求起点（{:?}，请求 {}）均一致，\
             判定为时钟/格式差异，继续该段下载",
            task_id,
            seg_idx,
            expected_last_modified,
            resp_lm_str,
            content_range_total,
            known_total,
            cr_start,
            actual_start
        );
    }

    // --- hint 模式（无 probe 基线）的跨段版本一致性 latch --------------------
    // probe 被跳过时 expected_etag/expected_last_modified 均为空，上方两个守卫恒
    // 不生效——多段之间没有任何版本一致性保障：文件在下载中途被【替换】会拼出
    // 新旧混合的静默损坏文件（含就地扩容追加的尾段与前缀不同版本的情形）。这里
    // 以第一个 206 响应携带的 validator 为基线，之后所有段与之比较；漂移 →
    // VersionChanged（Path B 清空回退单流，重下新版本）。比较策略与 probe 路径
    // 一致：双方非空才比较，CDN 剥离 validator 时永不误报。
    if expected_etag.is_empty() && expected_last_modified.is_empty() {
        let resp_etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let resp_lm = resp
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if let Err((base_etag, base_lm)) =
            check_cross_segment_validators(first_validators, resp_etag, resp_lm)
        {
            // 与 probe 路径同一判据：仅 Last-Modified 不同、而 Content-Range 总大小
            // 与起点均一致时是多 edge CDN 的时钟/格式差异，不是版本变化；ETag 双方
            // 非空且不同仍致命。
            let etag_mismatch =
                !base_etag.is_empty() && !resp_etag.is_empty() && base_etag != resp_etag;
            let lm_mismatch = !base_lm.is_empty() && !resp_lm.is_empty() && base_lm != resp_lm;
            let fatal = crate::downloader::validator_mismatch_is_fatal(
                etag_mismatch,
                lm_mismatch,
                crate::downloader::parse_content_range_total(resp.headers()),
                planned_total.load(Ordering::Relaxed),
                cr_start,
                Some(actual_start),
            );
            if !fatal {
                generation_evidence.note_lm_drift_tolerated();
                log_warn!(
                    "[coordinator] task {} seg {} 跨段 Last-Modified 不同（基线=\"{}\" 本段=\"{}\"）\
                     但 Content-Range 总大小与起点一致，判定为 CDN edge 时钟/格式差异，继续",
                    task_id,
                    seg_idx,
                    base_lm,
                    resp_lm
                );
            } else {
                log_info!(
                    "[coordinator] task {} seg {} 跨段 validator 漂移（基线 etag=\"{}\" lm=\"{}\"，\
                     本段 etag=\"{}\" lm=\"{}\"）——文件在下载中被替换，回退重下",
                    task_id,
                    seg_idx,
                    base_etag,
                    base_lm,
                    resp_etag,
                    resp_lm
                );
                return Err(DownloadError::VersionChanged(format!(
                    "segment {seg_idx}: validators drifted across segment responses \
                     (baseline etag=\"{base_etag}\" lm=\"{base_lm}\", \
                     got etag=\"{resp_etag}\" lm=\"{resp_lm}\")"
                )));
            }
        }

        // hint 模式无 probe 基线：`tasks.orig_etag`/`orig_last_modified` 仍是空串。
        // use_server_time 完成时靠 apply_server_mtime 读它落地文件 mtime,但多段路径
        // 没有单流那样"直接锁存实际响应"的出口(见 run_download_inner 的
        // single_result.latched_last_modified)——浏览器扩展 takeover 的大文件走多段,
        // 永远拿不到服务器时间(use_server_time 静默失效,BUG-HINT-MULTISEG-NO-MTIME)。
        // 这里把已通过跨段一致性校验的 validator 落库,供 run_download_inner 完成后
        // 回读;每段都会重复写入同一份值,幂等无害。
        if (!resp_etag.is_empty() || !resp_lm.is_empty())
            && let Err(e) = db.set_task_validator(task_id, resp_etag, resp_lm).await
        {
            log_info!(
                "[coordinator] task {} 持久化 hint 模式 validator 失败:{}(use_server_time 可能回退本地时间)",
                task_id,
                e
            );
        }
        // hint 模式没有 probe：文件起点的那条连接是唯一看到 Content-Disposition /
        // Content-Type 的实际 GET 响应，记为命名证据供完成期精修推断名（先写者胜）。
        if actual_start == 0 {
            crate::downloader::record_response_naming(db, task_id, &resp).await;
        }
    }

    // --- 服务器自报真实总大小 > 规划总大小 → 规划偏小，继续会静默截断 -------------
    // 合法 206 的 `Content-Range: bytes X-Y/<total>` 分母是服务器【自报的真实总大小】。
    // 当它大于当前规划的 planned_total 时，规划区间 [0, planned) 只覆盖了文件前缀——
    // 典型：浏览器扩展在 <video> Range 流式播放【渐进上传中】的视频时抓到【当时的部分
    // 大小】并作为 hint 传入，hint 模式跳过 probe 把偏小 hint 当权威总大小
    // （BUG-HTTP-HINT-UNDERSIZED）。返回 TrueSizeLarger 让 coordinator【就地扩容】
    // （延长预分配 + 追加尾段，已下数据零丢弃）后重新派工，下满整文件。
    //
    // planned 必须从共享 planned_total【实时读取】：扩容后 coordinator 已更新该值，
    // 若用 worker 启动时的栈上拷贝，尾段会对同一分母反复误报、永不收敛。
    //
    // 漂移容差取决于 planned 的【来源可信度】（size_is_estimate）：
    //   • size_is_estimate=true（fresh hint 模式）：planned 只是扩展抓到的、未经
    //     probe 验证的猜测值，而 Content-Range 分母才是服务器自报的【权威真实大小】。
    //     故【零容差】——只要 true_total > planned（精确）即触发扩容。任何非零容差
    //     都会重新打开静默截断窗口（例：hint 2_985_000 vs 真实 3_000_000，缺口 15_000
    //     < 1% 容差 29_850 → 尾部 15 KB 被静默丢弃）。
    //   • size_is_estimate=false（resume/probe 路径）：planned 已被 probe/DB 校准，
    //     与磁盘上的分段边界一致。沿用 resume 端一致的 CDN 漂移容差（1%，上限 1MB），
    //     仅在真实大小【明显】更大时才触发，避免与 resume 的“trust DB segments”小漂移
    //     决策相互打架；正常多段（规划值==真实大小，分母相等）永不触发，不影响吞吐。
    // 开放式首段的 200 全量流没有 Content-Range，但其 Content-Length 就是服务器
    // 自报的真实总大小，同样参与"规划偏小"检查（hint 偏小时照常触发就地扩容）。
    let reported_total =
        crate::downloader::parse_content_range_total(resp.headers()).or_else(|| {
            if resp.status() == reqwest::StatusCode::OK {
                resp.headers()
                    .get(reqwest::header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<i64>().ok())
                    .filter(|v| *v > 0)
            } else {
                None
            }
        });
    if let Some(true_total) = reported_total {
        let planned = planned_total.load(Ordering::Relaxed);
        let drift_tolerance = if size_is_estimate || scope.strict_total {
            0
        } else {
            (planned / 100).clamp(1, 1_048_576)
        };
        if true_total > planned + drift_tolerance {
            log_info!(
                "[coordinator] task {} seg {} 服务器自报真实大小 {} > 规划总大小 {}（漂移容差 \
                 {}，size_is_estimate={}），hint 偏小/文件增长——就地扩容下满整文件",
                task_id,
                seg_idx,
                true_total,
                planned,
                drift_tolerance,
                size_is_estimate
            );
            return Err(DownloadError::TrueSizeLarger(true_total));
        }
    }

    // Safety net: if a Range response carries Content-Encoding, the raw
    // compressed bytes cannot be spliced into the correct file offset — each
    // segment would need independent decompression but the decompressed size
    // is unpredictable, making precise byte-range assembly impossible.
    //
    // The probe phase now checks GET Range:0-0 specifically and disables
    // multi-segment when Range responses are compressed.  Reaching this point
    // with compression means the server changed behaviour between probe and
    // download (e.g. CDN edge node rotation).  This is extremely rare but we
    // must guard against it to prevent silent file corruption.
    if let Some(enc) = crate::downloader::detect_content_encoding(resp.headers()) {
        // Record the domain so that the retry (or any future task for this
        // host) automatically uses single-stream mode.
        if sys_lease {
            record_single_conn_domain_persist(url, db);
        }
        return Err(DownloadError::Other(format!(
            "segment {}: server returned Content-Encoding ({:?}) on a Range response. \
             Compressed byte ranges cannot be assembled into a valid file. \
             Please retry — the download will use single-stream mode.",
            seg_idx, enc
        )));
    }
    // 未知但存在的 Content-Encoding（如 compress）：detect 返回 None 会被当 identity
    // 原样拼接 → 损坏。同样回退单流（BUG-HTTP-UNKNOWN-ENCODING-RAW 的多段对应面）。
    if let Some(unknown) = crate::downloader::unsupported_content_encoding(resp.headers()) {
        if sys_lease {
            record_single_conn_domain_persist(url, db);
        }
        return Err(DownloadError::Other(format!(
            "segment {seg_idx}: server returned unsupported Content-Encoding '{unknown}' on a \
             Range response; cannot assemble byte ranges. Please retry in single-stream mode."
        )));
    }

    // 注：旧版本会在 segment 0 响应中提取 Content-Disposition 的"更好文件名"
    // 写入 DB 并通知客户端 UI，run_download_inner 末尾再据此 dedup + 重定向
    // dest_path。该机制已移除——新架构下文件名由 DownloadManager 在
    // do_start_task 同步段统一决策（probe 阶段读取 Content-Disposition），
    // 所有下载器内部不再变更文件名，避免与 manager 的 reserved_temp_paths
    // 协调断裂导致并发冲突（参见 PR #296 自我冲突回归 bug）。

    // open-ended 首响应已通过全部校验、即将读 body：通知 coordinator 立即
    // 放队（不必等下一个 ramp tick）。latch 保证重试路径不重发。
    // hint 模式 RangeVerdict 已在上方先发，保证 coordinator 先处理裁决。
    if open_ended && !*open_ended_established_sent {
        *open_ended_established_sent = true;
        if event_tx
            .send(WorkerEvent::OpenEndedEstablished)
            .await
            .is_err()
        {
            tracing::debug!("HTTP coordinator event receiver closed");
            return Err(DownloadError::Cancelled);
        }
    }

    // 到这里响应已通过状态、Range、validator、大小与编码校验；在 body 流存活
    // 期间登记为“当前 generation 正在被服务”。403/429 只有与这份同时成功证据
    // 并存，才有资格学习域名连接上限。
    let _active_response = generation_evidence.response_started(seg_idx);

    let mut stream = resp.bytes_stream();

    let file = OpenOptions::new().write(true).open(dest).await?;
    let seg_remaining = request_end - actual_start + 1;
    let buf_cap = buf_writer_capacity_for_segment(seg_remaining);
    let mut file = tokio::io::BufWriter::with_capacity(buf_cap, file);
    file.seek(std::io::SeekFrom::Start(actual_start as u64))
        .await?;
    // 后台 fdatasync 用的同文件独立句柄，首个落库周期才创建（短段不付这个 fd）。
    let mut sync_handle: Option<Arc<std::fs::File>> = None;

    let mut seg_downloaded = actual_start - seg_start;
    let mut last_db_save = Instant::now();
    // durable_offset：仅记录【已被 fdatasync 覆盖】的偏移，供周期落库使用，以维持
    // BUG-COORD-FSYNC 不变式（"DB 偏移 <= 已持久化字节"）。异常退出/段完成路径不
    // 经此水位，直接 fsync + 落 seg_downloaded（见循环内各 break 分支及循环后收尾）。
    // pending_snap 暂存"已写入页缓存但尚未被 fsync 覆盖"的最新快照，待后续 fsync
    // 覆盖后提交，使各段落库水位仅滞后约一个落库周期。
    let mut durable_offset = seg_downloaded;
    let mut pending_snap: Option<(i64, Instant)> = None;

    // The effective end byte, which may shrink if the coordinator splits us.
    //
    // This is a *read-only mirror* of `seg_states[seg_idx].end_byte`.  The
    // coordinator owns the canonical value (see ownership contract on
    // `update_seg_state`); we re-read it before each write to honour any
    // split that happened since our last chunk.
    let mut effective_end = seg_end;

    // 标记我们是否真正抵达了段边界（写满了 [actual_start, effective_end]）。
    // 只有在 `write_len == 0`（预算耗尽）或 `write_len < bytes.len()`（截断本块
    // 命中边界）这两条 break 路径上才置为 true。若循环因 `None`（流被服务器
    // 干净地提前关闭，常见于大文件尾部的 CDN 故障）退出而此标志仍为 false，
    // 说明收到的字节少于请求的区间——必须返回 Err 触发 do_segment_with_retry
    // 的指数退避重试，而非把截断段当作成功（否则会留下内容空洞，仅被末尾的
    // 聚合检查 seg_total < effective_total_bytes 捕获，迫使整任务重启而非廉价
    // 的单段重试）。
    let mut boundary_reached = false;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                file.flush().await?;
                // Resume trusts this offset: a failed sync must not advance its checkpoint.
                file.get_ref().sync_data().await?;
                update_seg_state(seg_states, seg_idx, seg_downloaded);
                db
                    .update_segment_progress_bounded(
                        task_id, seg_idx, seg_downloaded, seg_start, spawn_gen,
                    )
                    .await?;
                return Err(DownloadError::Cancelled);
            }
            result = tokio::time::timeout(
                // 停滞容忍双档：普通流 5s（重连廉价，快速回收死连接）；吸收
                // 合并后的唯一生命线 60s（重连必被拒，对齐 aria2 --timeout）。
                if reconnect_hostile.load(Ordering::Relaxed) {
                    CHUNK_STALL_TIMEOUT_HOSTILE
                } else {
                    CHUNK_STALL_TIMEOUT
                },
                stream.next(),
            ) => {
                // Unwrap the timeout layer first.  If no chunk arrived within
                // CHUNK_STALL_TIMEOUT the TCP connection is likely dead — flush
                // partial progress and bubble up an error so do_segment_with_retry
                // can resume from a fresh connection.
                let chunk = match result {
                    Ok(c) => c,
                    Err(_) => {
                        let checkpoint = async {
                            file.flush().await?;
                            file.get_ref().sync_data().await?;
                            update_seg_state(seg_states, seg_idx, seg_downloaded);
                            db
                                .update_segment_progress_bounded(
                                    task_id, seg_idx, seg_downloaded, seg_start, spawn_gen,
                                )
                                .await?;
                            Ok::<(), DownloadError>(())
                        }.await;
                        if let Err(cleanup_error) = checkpoint {
                            crate::logger::report_warning("http-coordinator", "persist segment after stall", &cleanup_error);
                            update_seg_state(seg_states, seg_idx, durable_offset);
                        }
                        let stall_secs = if reconnect_hostile.load(Ordering::Relaxed) {
                            CHUNK_STALL_TIMEOUT_HOSTILE.as_secs()
                        } else {
                            CHUNK_STALL_TIMEOUT.as_secs()
                        };
                        return Err(DownloadError::Other(format!(
                            "segment {seg_idx} stalled: no data received for {stall_secs}s"
                        )));
                    }
                };
                match chunk {
                    Some(Ok(bytes)) => {
                        // --- Boundary check BEFORE writing ---
                        // Read the possibly-shrunk end_byte from shared state.
                        if let Ok(states) = seg_states.lock()
                            && let Some(pos) = seg_state_pos(&states, seg_idx) {
                                effective_end = states[pos].end_byte;
                            }

                        // Calculate the write budget.
                        let current_pos = seg_start + seg_downloaded;
                        let budget = (write_limit(effective_end) - current_pos + 1).max(0) as usize;
                        let write_len = bytes.len().min(budget);

                        if write_len == 0 {
                            // Reached the (possibly shrunk) boundary — stop.
                            boundary_reached = true;
                            break;
                        }

                        let write_slice = &bytes[..write_len];

                        // --- Speed limiter: write in sub-chunks as tokens allow ---
                        // 令牌等待与取消竞速：低限速 + 多 worker 时一个 chunk 要等
                        // 数秒，暂停/删除不能被挡在 select 分支体外。
                        let mut offset = 0usize;
                        while offset < write_len {
                            let remaining = (write_len - offset) as u64;
                            let allowed = tokio::select! {
                                _ = cancel.cancelled() => {
                                    // 已写入的子块计入进度，与上方取消分支的落库一致。
                                    let written = offset as i64;
                                    seg_downloaded += written;
                                    total_downloaded.fetch_add(written, Ordering::Relaxed);
                                    file.flush().await?;
                                    file.get_ref().sync_data().await?;
                                    update_seg_state(seg_states, seg_idx, seg_downloaded);
                                    db
                                        .update_segment_progress_bounded(
                                            task_id, seg_idx, seg_downloaded, seg_start, spawn_gen,
                                        )
                                        .await?;
                                    return Err(DownloadError::Cancelled);
                                }
                                allowed = speed_limiter.consume(remaining) => allowed,
                            };
                            let end = offset + allowed as usize;
                            file.write_all(&write_slice[offset..end]).await?;
                            offset = end;
                        }

                        let len = write_len as i64;
                        seg_downloaded += len;
                        total_downloaded.fetch_add(len, Ordering::Relaxed);

                        // Only `downloaded_bytes` is written — `end_byte` is
                        // exclusively owned by the coordinator.
                        update_seg_state(seg_states, seg_idx, seg_downloaded);

                        // 边界截断：仅 flush 页缓存，DB 满段写统一走循环后完成路径
                        // （覆盖式 fdatasync → update_segment_progress_bounded），
                        // 避免「环内先写 DB、fsync 在 break 后」违反 BUG-COORD-FSYNC。
                        if write_len < bytes.len() {
                            boundary_reached = true;
                            file.flush().await?;
                            break;
                        }


                        // --- DB persistence (periodic) ---
                        // 落库前必须保证偏移已 fdatasync 落盘：DB 记录的偏移是 resume
                        // 的信任来源，若只 flush（用户态→页缓存）就落库，掉电后该区间
                        // 在盘上仍是预分配的 0，resume 据 DB 跳过 → 永久空洞且骗过完整性
                        // 检查（BUG-COORD-FSYNC）。
                        //
                        // 但 fdatasync 刷的是【整个文件 inode】的脏页，与 fd 无关——64 段
                        // 各自每 3s fsync 是重复整盘刷写。FileSyncGate 把它合并为全局每
                        // MIN_SYNC_GAP 至多一次、并放到阻塞线程池里做（本 worker 不等它，
                        // 继续读 socket），仅把【已被某次已完成 fsync 覆盖】的快照提交为
                        // durable_offset；未覆盖的最早快照留在 pending_snap，下个周期再判
                        // （保留最早而非最新：慢盘上 fsync 超过一个周期时水位仍必然推进）。
                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            if let Some(error) = sync_gate.failure() {
                                return Err(error.into());
                            }
                            file.flush().await?;
                            let snap = seg_downloaded;
                            let snap_t = Instant::now();
                            let prev_durable = durable_offset;
                            let covered = |t: Instant| {
                                sync_gate.last_completed_start().is_some_and(|s| s >= t)
                            };
                            match pending_snap {
                                Some((off, t)) if covered(t) => {
                                    durable_offset = off;
                                    pending_snap = Some((snap, snap_t));
                                }
                                Some(_) => {}
                                None => pending_snap = Some((snap, snap_t)),
                            }
                            let handle = match &sync_handle {
                                Some(handle) => Arc::clone(handle),
                                None => {
                                    let handle = Arc::new(
                                        file.get_ref().try_clone().await?.into_std().await,
                                    );
                                    sync_handle = Some(Arc::clone(&handle));
                                    handle
                                }
                            };
                            sync_gate.kick_background(&handle);
                            // 仅在 durable 水位推进时写入汇聚表；coordinator 批量落库。
                            if durable_offset != prev_durable {
                                durable_progress
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .insert(seg_idx, durable_offset);
                            }
                            last_db_save = Instant::now();
                        }
                    }
                    Some(Err(e)) => {
                        let checkpoint = async {
                            file.flush().await?;
                            file.get_ref().sync_data().await?;
                            update_seg_state(seg_states, seg_idx, seg_downloaded);
                            db
                                .update_segment_progress_bounded(
                                    task_id, seg_idx, seg_downloaded, seg_start, spawn_gen,
                                )
                                .await?;
                            Ok::<(), DownloadError>(())
                        }.await;
                        if let Err(cleanup_error) = checkpoint {
                            crate::logger::report_warning("http-coordinator", "persist segment after read failure", &cleanup_error);
                            update_seg_state(seg_states, seg_idx, durable_offset);
                        }
                        return Err(DownloadError::Request(e));
                    }
                    None => break,
                }
            }
        }
    }

    drop(_active_response);
    // 段完成态落库前必须有覆盖式 fdatasync（BUG-COORD-FSYNC）：
    // coordinator 把 Completed 段视为永久完成、resume 时绝不重取——若此处不
    // 持久化，崩溃/掉电后会留下 "DB 完成但磁盘为 0" 的空洞且通过完整性检查。
    // 合并闸把尾部多段同时完成的 fsync 风暴收敛为少数几次整盘刷。
    // 放在下方 fadvise(DONTNEED) 之前，使已落盘的干净页可被安全丢弃。
    file.flush().await?;
    let snap_t = Instant::now();
    sync_gate.sync_covering(file.get_ref(), snap_t).await?;

    // --- Truncation / short-read detection ---------------------------------
    // 若循环并非因抵达段边界而退出（boundary_reached == false），且未被取消，
    // 则唯一的退出途径是 `None`（流被干净地提前关闭）。此时若仍未写满目标
    // 区间，说明服务器在没有错误帧的情况下截断了响应体——这是大文件尾部
    // 常见的 CDN 故障模式，会被 206 状态码与无错误的流结束所掩盖。
    //
    // 重新读取共享状态里可能已被 split 收窄的 end_byte：若 split 恰好把边界
    // 收窄到我们停下的位置，本段实际已完成，不应误报截断。
    if !boundary_reached && !cancel.is_cancelled() {
        if let Ok(states) = seg_states.lock()
            && let Some(pos) = seg_state_pos(&states, seg_idx)
        {
            effective_end = states[pos].end_byte;
        }
        let next_pos = seg_start + seg_downloaded;
        if next_pos <= write_limit(effective_end) {
            // 已写到磁盘的部分进度已通过 update_seg_state / update_segment_progress
            // 在循环内持久化（见 DB_SAVE_INTERVAL_SECS 分支）。这里仅补一次落库，
            // 确保 do_segment_with_retry 以 seg_start + downloaded_bytes 续传本段，
            // 而非从头重下，也不污染 total_downloaded 计数。
            update_seg_state(seg_states, seg_idx, seg_downloaded);
            db.update_segment_progress_bounded(
                task_id,
                seg_idx,
                seg_downloaded,
                seg_start,
                spawn_gen,
            )
            .await?;
            return Err(DownloadError::Other(format!(
                "segment {} truncated: received {} bytes, expected {} (server closed stream early)",
                seg_idx,
                next_pos - actual_start,
                write_limit(effective_end) - actual_start + 1
            )));
        }
    }

    // Linux: posix_fadvise(FADV_DONTNEED) 通知内核释放已完成段的页缓存，
    // 防止大文件下载过程中页缓存无限增长占满内存。
    // 参考 aria2 的 readDataDropCache() 策略。
    // posix_fadvise 仅为内核提供提示，不阻塞，无需 spawn_blocking。
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let fd = file.get_ref().as_raw_fd();
        match (
            libc::off_t::try_from(seg_start),
            libc::off_t::try_from(seg_downloaded),
        ) {
            (Ok(offset), Ok(len)) => {
                // SAFETY: file 在调用期间持有有效 fd；偏移与长度均可用 off_t 表示。
                let error =
                    unsafe { libc::posix_fadvise(fd, offset, len, libc::POSIX_FADV_DONTNEED) };
                // posix_fadvise 直接返回错误码，不设置 errno；提示失败不撤销下载成功。
                if error != 0 {
                    log_info!(
                        "[coordinator] posix_fadvise(DONTNEED) failed: {}",
                        std::io::Error::from_raw_os_error(error)
                    );
                }
            }
            _ => log_info!("[coordinator] skip posix_fadvise: range exceeds off_t"),
        }
    }

    update_seg_state(seg_states, seg_idx, seg_downloaded);
    db.update_segment_progress_bounded(task_id, seg_idx, seg_downloaded, seg_start, spawn_gen)
        .await?;

    Ok(seg_downloaded)
}

/// During shutdown only local persistence failures can replace a normal cancellation.
fn record_shutdown_worker_event(event: WorkerEvent, final_error: &mut Option<DownloadError>) {
    if let WorkerEvent::Failed { error, .. } = event {
        if matches!(error, DownloadError::Io(_) | DownloadError::Db(_)) {
            if matches!(final_error, None | Some(DownloadError::Cancelled)) {
                *final_error = Some(error);
            } else {
                crate::logger::report_warning(
                    "http-coordinator",
                    "persist another segment during shutdown",
                    &error,
                );
            }
        } else {
            tracing::debug!(%error, "HTTP worker exited while coordinator was stopping");
        }
    }
}

/// Update a single segment's `downloaded_bytes` in the shared visualization state.
///
/// **Ownership contract for `seg_states[i]`** (Single-Writer Principle):
///
/// | Field             | Writer                         | Reader                    |
/// |-------------------|--------------------------------|---------------------------|
/// | `downloaded_bytes`| **worker** (this function)     | coordinator (`sync_downloaded_from_shared`) |
/// | `end_byte`        | **coordinator** (`rebuild_seg_states` after split) | worker (boundary check) |
/// | `start_byte`      | coordinator (immutable post-split) | worker / UI               |
/// | `index`           | coordinator (immutable post-split) | worker / UI               |
///
/// Historically this function also wrote `end_byte` using the worker's locally
/// cached `effective_end`, which races with `rebuild_seg_states`: if the
/// coordinator shrinks `end_byte` to trigger a split between the worker
/// reading `effective_end` and writing it back, the worker would clobber the
/// new boundary, miss the split, and continue downloading into the child
/// segment's range — defeating the entire dynamic-split mechanism.
fn update_seg_state(
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    seg_idx: i32,
    downloaded_bytes: i64,
) {
    if let Ok(mut states) = seg_states.lock()
        && let Some(pos) = seg_state_pos(&states, seg_idx)
    {
        states[pos].downloaded_bytes = downloaded_bytes;
        // Intentionally do NOT touch `end_byte` — it is owned by the
        // coordinator (see `rebuild_seg_states`).
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        FileSyncGate, FreezeState, HINT_UNCAP_MAX, LiveSegment, MAX_SEGMENTS, MIN_SPLIT_BYTES,
        MIN_SYNC_GAP, RAMP_SOFT_PROBE_COOLDOWN_TICKS, RAMP_SOFT_PROBE_MAX_ATTEMPTS,
        RAMP_SOFT_PROBE_TASK_BUDGET, RampVerdict, SegState, SplitCost, TAIL_MIN_SPLIT_BYTES,
        all_done, beneficial_hint_scale, build_seg_state_vec, check_cross_segment_validators,
        conn_cap_cache, domain_conn_hint, dynamic_min_split_bytes, extract_host,
        find_next_pending_only, find_next_work, freeze_verdict_next_state, has_other_in_flight,
        hint_uncap_ceiling, initial_allowed_workers, is_single_conn_domain, is_tail, ramp_verdict,
        rebuild_seg_states, record_domain_conn_cap, record_domain_conn_hint, should_expand,
        should_shrink, soft_probe_eval_transition, soft_probe_ready, sustained_shrink_next_state,
        try_proactive_split, try_split_largest, validate_coverage,
    };
    use crate::downloader::{
        DownloadError, SegmentProgressInfo, is_range_not_satisfiable, is_server_rejection,
    };
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn preallocate_file_len_preserves_data_and_never_shrinks_existing_file() {
        let dir = std::env::temp_dir().join(format!("fluxdown_prealloc_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("payload");
        std::fs::write(&dest, b"keep").unwrap();
        let guard = crate::temp_file_guard::TempFileGuard::acquire(&dest, "prealloc")
            .await
            .unwrap();
        super::preallocate_file_len(&dest, 4096, &guard)
            .await
            .unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 4096);
        super::preallocate_file_len(&dest, 16, &guard)
            .await
            .unwrap();
        let data = std::fs::read(&dest).unwrap();
        assert_eq!(data.len(), 4096);
        assert_eq!(&data[..4], b"keep");
        assert!(data[4..].iter().all(|byte| *byte == 0));
        drop(guard);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn coordinator_rejects_another_writer_before_truncating_stale_tail() {
        use crate::cdn::NodePool;
        use crate::downloader::RequestSpec;
        use crate::events::{EngineEvent, EventSink};
        use crate::speed_limiter::SpeedLimiter;
        use crate::temp_file_guard::TempFileGuard;
        use tokio_util::sync::CancellationToken;

        struct NoopSink;
        impl EventSink for NoopSink {
            fn emit(&self, _event: EngineEvent) {}
        }

        let dir =
            std::env::temp_dir().join(format!("fluxdown_coord_lock_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir).await.unwrap();
        db.insert_task(
            "second",
            "http://127.0.0.1:1/f",
            "f",
            "",
            1,
            0,
            "",
            "",
            "",
            0,
        )
        .await
        .unwrap();
        let dest = dir.join("f.fdownloading");
        std::fs::write(&dest, b"existing partial data").unwrap();
        let first = TempFileGuard::acquire(&dest, "first").await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(1024);
        let spec = RequestSpec {
            method: reqwest::Method::GET,
            cookies: String::new(),
            referrer: String::new(),
            extra_headers: std::collections::HashMap::new(),
            body: None,
            ..Default::default()
        };
        let result = super::run_coordinated_download(
            "second",
            "http://127.0.0.1:1/f",
            &dest,
            4,
            false,
            2,
            NodePool::single(reqwest::Client::new()),
            &db,
            &tx,
            &CancellationToken::new(),
            &SpeedLimiter::new(0),
            &Arc::new(crate::downloader::DownloadSpec::new(spec)),
            &NoopSink,
            "",
            "",
            super::ReportScope::whole_task(),
            1,
            false,
            None,
            None,
        )
        .await;
        assert!(matches!(
            result,
            Err(DownloadError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert_eq!(std::fs::read(&dest).unwrap(), b"existing partial data");
        assert!(db.load_segments("second").await.unwrap().is_empty());
        drop(first);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
    }

    #[tokio::test]
    async fn redispatch_keeps_durable_offset_and_split_starts_new_child_at_zero() {
        let db = crate::db::Db::connect("sqlite::memory:")
            .await
            .expect("open DB");
        db.insert_task(
            "layout",
            "http://localhost/f",
            "f",
            "",
            2,
            100,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");
        db.insert_segments("layout", &[(0, 0, 99)])
            .await
            .expect("insert segment");
        db.update_segment_progress("layout", 0, 10)
            .await
            .expect("save durable offset");
        let mut segments = BTreeMap::new();
        segments.insert(0, make_seg(0, 0, 99, 50, SegState::Pending));
        super::persist_segment_change(&db, "layout", &segments, 0, None)
            .await
            .expect("redispatch");
        assert_eq!(
            db.load_segments("layout").await.expect("load segment")[0].downloaded_bytes,
            10,
            "live progress must never become a durable offset on redispatch"
        );
        segments.get_mut(&0).expect("parent").end_byte = 74;
        segments.insert(1, make_seg(1, 75, 99, 0, SegState::Pending));
        super::persist_segment_change(&db, "layout", &segments, 1, Some(0))
            .await
            .expect("split");
        let rows = db.load_segments("layout").await.expect("load split");
        assert_eq!(
            rows.iter()
                .map(|r| (r.index, r.start_byte, r.end_byte, r.downloaded_bytes))
                .collect::<Vec<_>>(),
            vec![(0, 0, 74, 10), (1, 75, 99, 0)]
        );
    }

    #[tokio::test]
    async fn coordinated_cancel_joins_readers_and_preserves_durable_prefixes() {
        use crate::events::{EngineEvent, EventSink};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        struct Sink;
        impl EventSink for Sink {
            fn emit(&self, _: EngineEvent) {}
        }
        const TOTAL: i64 = 4 * 1024 * 1024;
        let db = crate::db::Db::connect("sqlite::memory:")
            .await
            .expect("open DB");
        db.insert_task(
            "cancel",
            "http://localhost/f",
            "f",
            "",
            4,
            TOTAL,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind HTTP server");
        let url = format!(
            "http://{}/f",
            listener.local_addr().expect("server address")
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let server_cancel = cancel.clone();
        let server = tokio::spawn(async move {
            let mut readers = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = tokio::select! {
                    _ = server_cancel.cancelled() => break,
                    accepted = listener.accept() => accepted.expect("accept HTTP request"),
                };
                let cancel = server_cancel.clone();
                readers.spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = socket.read(&mut buf).await.expect("read request");
                        if n == 0 { return; }
                        request.extend_from_slice(&buf[..n]);
                    }
                    let text = String::from_utf8(request).expect("ASCII request").to_ascii_lowercase();
                    let range = text.lines().find_map(|line| line.strip_prefix("range: bytes=")).expect("Range request");
                    let (start, end) = range.trim().split_once('-').expect("range bounds");
                    let start: i64 = start.parse().expect("range start");
                    let end: i64 = if end.is_empty() { TOTAL - 1 } else { end.parse().expect("range end") };
                    let header = format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{TOTAL}\r\nConnection: close\r\n\r\n", end - start + 1);
                    socket.write_all(header.as_bytes()).await.expect("send headers");
                    socket.write_all(&[0x5a; 16 * 1024]).await.expect("send prefix");
                    cancel.cancelled().await;
                });
            }
            while let Some(joined) = readers.join_next().await {
                joined.expect("server reader finishes");
            }
        });
        let dir =
            std::env::temp_dir().join(format!("fluxdown_coord_cancel_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create fixture directory");
        let dest = dir.join("f.fdownloading");
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let spec = Arc::new(crate::downloader::DownloadSpec::new(
            crate::downloader::RequestSpec {
                method: reqwest::Method::GET,
                cookies: String::new(),
                referrer: String::new(),
                extra_headers: std::collections::HashMap::new(),
                body: None,
                ..Default::default()
            },
        ));
        let pool = crate::cdn::NodePool::single(
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("build client"),
        );
        let limiter = crate::speed_limiter::SpeedLimiter::new(0);
        let download = super::run_coordinated_download(
            "cancel",
            &url,
            &dest,
            TOTAL,
            false,
            4,
            pool,
            &db,
            &tx,
            &cancel,
            &limiter,
            &spec,
            &Sink,
            "",
            "",
            super::ReportScope::whole_task(),
            1,
            false,
            None,
            None,
        );
        tokio::pin!(download);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    result = &mut download => panic!("partial response completed before cancellation: {result:?}"),
                    update = rx.recv() => if update.expect("progress channel alive").downloaded_bytes > 0 { break; },
                }
            }
        }).await.expect("observe downloaded prefix");
        cancel.cancel();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut download)
                .await
                .expect("coordinator joins cancelled readers"),
            Err(crate::downloader::DownloadError::Cancelled)
        ));
        server.await.expect("HTTP server stops");
        let rows = db
            .load_segments("cancel")
            .await
            .expect("load durable checkpoints");
        let disk = tokio::fs::read(&dest).await.expect("read partial file");
        assert!(
            rows.iter().any(|row| row.downloaded_bytes > 0),
            "observed bytes must remain resumable after clean cancellation"
        );
        for row in rows {
            let start = row.start_byte as usize;
            let end = start + row.downloaded_bytes as usize;
            assert!(
                disk[start..end].iter().all(|byte| *byte == 0x5a),
                "checkpoint cannot include preallocated zero bytes"
            );
        }
        assert_eq!(disk.len(), TOTAL as usize);
        tokio::fs::remove_dir_all(dir)
            .await
            .expect("remove fixture directory");
    }

    #[tokio::test]
    async fn connection_cap_bounds_concurrent_workers() {
        use crate::events::{EngineEvent, EventSink};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        struct Sink;
        impl EventSink for Sink {
            fn emit(&self, _: EngineEvent) {}
        }
        const TOTAL: i64 = 4 * 1024 * 1024;
        let db = crate::db::Db::connect("sqlite::memory:")
            .await
            .expect("open DB");
        db.insert_task(
            "cap",
            "http://localhost/f",
            "f",
            "",
            8,
            TOTAL,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind HTTP server");
        let url = format!(
            "http://{}/f",
            listener.local_addr().expect("server address")
        );
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let server = {
            let active = active.clone();
            let peak = peak.clone();
            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let active = active.clone();
                    let peak = peak.clone();
                    tokio::spawn(async move {
                        let mut request = Vec::new();
                        let mut buf = [0; 1024];
                        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                            let n = socket.read(&mut buf).await.unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            request.extend_from_slice(&buf[..n]);
                        }
                        let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                        let (start, end) = text
                            .lines()
                            .find_map(|l| l.strip_prefix("range: bytes="))
                            .and_then(|r| r.trim().split_once('-'))
                            .map(|(s, e)| {
                                (
                                    s.parse::<i64>().unwrap_or(0),
                                    e.parse::<i64>().unwrap_or(TOTAL - 1).min(TOTAL - 1),
                                )
                            })
                            .unwrap_or((0, TOTAL - 1));
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        // 持有连接一小段时间，让并发窗口可被观测。
                        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                        let header = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{TOTAL}\r\nConnection: close\r\n\r\n",
                            end - start + 1
                        );
                        let body = vec![0x5a; (end - start + 1) as usize];
                        if socket.write_all(header.as_bytes()).await.is_ok()
                            && let Err(error) = socket.write_all(&body).await
                        {
                            tracing::debug!(%error, "test client closed");
                        }
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            })
        };
        let dir = std::env::temp_dir().join(format!("fluxdown_coord_cap_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create fixture directory");
        let dest = dir.join("f.fdownloading");
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let spec = Arc::new(
            crate::downloader::DownloadSpec::new(crate::downloader::RequestSpec::empty_get())
                .with_connection_cap(2),
        );
        let pool = crate::cdn::NodePool::single(
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("build client"),
        );
        let limiter = crate::speed_limiter::SpeedLimiter::new(0);
        let cancel = tokio_util::sync::CancellationToken::new();
        let total = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_coordinated_download(
                "cap",
                &url,
                &dest,
                TOTAL,
                false,
                8,
                pool,
                &db,
                &tx,
                &cancel,
                &limiter,
                &spec,
                &Sink,
                "",
                "",
                super::ReportScope::whole_task(),
                1,
                false,
                None,
                None,
            ),
        )
        .await
        .expect("download finishes")
        .expect("download succeeds");
        assert_eq!(total, TOTAL);
        assert!(
            peak.load(Ordering::SeqCst) <= 2,
            "peak concurrent connections {} exceeded cap 2",
            peak.load(Ordering::SeqCst)
        );
        server.abort();
        drop(tx);
        drain.abort();
        tokio::fs::remove_dir_all(dir)
            .await
            .expect("remove fixture directory");
    }

    #[tokio::test]
    async fn coordinator_refreshes_rejected_segment_credentials() {
        use crate::downloader::test_support::{RefreshScript, params_with_interceptor};
        use crate::events::{EngineEvent, EventSink};
        use std::collections::HashSet;
        use std::sync::Mutex;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        struct Sink;
        impl EventSink for Sink {
            fn emit(&self, _: EngineEvent) {}
        }
        const TOTAL: i64 = 4 * 1024 * 1024;
        fn byte_at(i: i64) -> u8 {
            (i % 251) as u8
        }
        let db = crate::db::Db::connect("sqlite::memory:")
            .await
            .expect("open DB");
        db.insert_task(
            "tokens",
            "http://localhost/f",
            "f",
            "",
            4,
            TOTAL,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind HTTP server");
        let url = format!(
            "http://{}/f",
            listener.local_addr().expect("server address")
        );
        // 每个 X-Token 只接受一次请求；记录被接受的令牌序列。
        let spent = Arc::new(Mutex::new(HashSet::<String>::new()));
        let accepted = Arc::new(Mutex::new(Vec::<String>::new()));
        let server = {
            let spent = spent.clone();
            let accepted = accepted.clone();
            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let spent = spent.clone();
                    let accepted = accepted.clone();
                    tokio::spawn(async move {
                        let mut request = Vec::new();
                        let mut buf = [0; 1024];
                        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                            let n = socket.read(&mut buf).await.unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            request.extend_from_slice(&buf[..n]);
                        }
                        let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                        let token = text
                            .lines()
                            .find_map(|l| l.strip_prefix("x-token:"))
                            .map(|t| t.trim().to_string())
                            .unwrap_or_default();
                        let fresh = !token.is_empty()
                            && spent.lock().expect("spent lock").insert(token.clone());
                        if !fresh {
                            let denied = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                            if let Err(error) = socket.write_all(denied.as_bytes()).await {
                                tracing::debug!(%error, "test client closed");
                            }
                            return;
                        }
                        accepted.lock().expect("accepted lock").push(token);
                        let (start, end) = text
                            .lines()
                            .find_map(|l| l.strip_prefix("range: bytes="))
                            .and_then(|r| r.trim().split_once('-'))
                            .map(|(s, e)| {
                                (
                                    s.parse::<i64>().unwrap_or(0),
                                    e.parse::<i64>().unwrap_or(TOTAL - 1).min(TOTAL - 1),
                                )
                            })
                            .unwrap_or((0, TOTAL - 1));
                        let header = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{TOTAL}\r\nConnection: close\r\n\r\n",
                            end - start + 1
                        );
                        let body: Vec<u8> = (start..=end).map(byte_at).collect();
                        if socket.write_all(header.as_bytes()).await.is_ok()
                            && let Err(error) = socket.write_all(&body).await
                        {
                            tracing::debug!(%error, "test client closed");
                        }
                    });
                }
            })
        };
        let script = Arc::new(RefreshScript::new(std::time::Duration::ZERO, |n| {
            Some(format!(r#"{{"setHeaders":{{"X-Token":"tok{n}"}}}}"#))
        }));
        let params = params_with_interceptor(&url, script.clone())
            .await
            .expect("build params");
        let mut base = crate::downloader::RequestSpec::empty_get();
        base.extra_headers
            .insert("X-Token".to_string(), "tok0".to_string());
        // 单连接：段按序下载，每段的请求都撞上已用掉的令牌，必须经刷新换发后才能完成。
        let spec = Arc::new(
            crate::downloader::DownloadSpec::new(base)
                .with_connection_cap(1)
                .with_refresher(crate::intercept::Refresher::for_params(
                    &params,
                    crate::intercept::ProtocolKind::Http,
                )),
        );
        let dir =
            std::env::temp_dir().join(format!("fluxdown_coord_refresh_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create fixture directory");
        let dest = dir.join("f.fdownloading");
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let pool = crate::cdn::NodePool::single(
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("build client"),
        );
        let limiter = crate::speed_limiter::SpeedLimiter::new(0);
        let cancel = tokio_util::sync::CancellationToken::new();
        let total = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_coordinated_download(
                "tokens",
                &url,
                &dest,
                TOTAL,
                false,
                4,
                pool,
                &db,
                &tx,
                &cancel,
                &limiter,
                &spec,
                &Sink,
                "",
                "",
                super::ReportScope::whole_task(),
                1,
                false,
                None,
                None,
            ),
        )
        .await
        .expect("download finishes")
        .expect("download succeeds");
        assert_eq!(total, TOTAL);
        let written = std::fs::read(&dest).expect("read downloaded file");
        assert_eq!(written.len() as i64, TOTAL);
        assert!(
            written
                .iter()
                .enumerate()
                .all(|(i, b)| *b == byte_at(i as i64)),
            "downloaded bytes differ from the served file"
        );
        let accepted = accepted.lock().expect("accepted lock").clone();
        let distinct: HashSet<&String> = accepted.iter().collect();
        assert_eq!(
            distinct.len(),
            accepted.len(),
            "token accepted twice: {accepted:?}"
        );
        assert!(
            script.calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "rejected segments must have triggered a refresh"
        );
        server.abort();
        drop(tx);
        drain.abort();
        tokio::fs::remove_dir_all(dir)
            .await
            .expect("remove fixture directory");
    }

    fn make_seg(index: i32, start: i64, end: i64, downloaded: i64, state: SegState) -> LiveSegment {
        LiveSegment {
            index,
            start_byte: start,
            end_byte: end,
            downloaded_bytes: downloaded,
            state,
            rate_bps: None,
        }
    }

    // -----------------------------------------------------------------------
    // ramp_verdict（扩容评估三档判定）
    // -----------------------------------------------------------------------

    /// 扩容后吞吐显著提升 → 继续扩容。
    #[test]
    fn ramp_verdict_improved_on_clear_gain() {
        assert_eq!(
            ramp_verdict(10_000_000.0, 18_000_000.0),
            RampVerdict::Improved
        );
    }

    /// 增益不足（<= 前值 × 1.05）→ 冻结；含持平与轻微下降。
    #[test]
    fn ramp_verdict_freeze_on_marginal_change() {
        assert_eq!(
            ramp_verdict(10_000_000.0, 10_000_000.0),
            RampVerdict::Freeze
        );
        assert_eq!(
            ramp_verdict(10_000_000.0, 10_400_000.0),
            RampVerdict::Freeze
        );
        assert_eq!(ramp_verdict(10_000_000.0, 8_000_000.0), RampVerdict::Freeze);
    }

    /// 吞吐腰斩以下 → 崩塌回滚（惩罚型限速，实测 59 MB/s -> 2 MB/s 场景）。
    #[test]
    fn ramp_verdict_collapse_on_throughput_cliff() {
        assert_eq!(
            ramp_verdict(59_109_230.0, 2_095_233.0),
            RampVerdict::Collapse
        );
    }

    /// 恰在崩塌线上（等于 前值 × 0.5）判 Freeze——回滚只留给明确信号。
    #[test]
    fn ramp_verdict_boundary_is_freeze() {
        assert_eq!(ramp_verdict(10_000_000.0, 5_000_000.0), RampVerdict::Freeze);
    }

    /// 前后同为 0（服务器停滞）→ Freeze，绝不对停滞服务器继续扩容。
    #[test]
    fn ramp_verdict_zero_throughput_freezes() {
        assert_eq!(ramp_verdict(0.0, 0.0), RampVerdict::Freeze);
    }

    // -----------------------------------------------------------------------
    // is_tail / should_shrink / should_expand（尾部自适应门控）
    // -----------------------------------------------------------------------

    const MB: i64 = 1024 * 1024;

    /// 大文件尾部场景：5.6GB @cap16，剩余 15MB → 尾部（abs 15<32MB ∧ rel 15<112MB）。
    #[test]
    fn is_tail_true_at_large_file_tail() {
        assert!(is_tail(15 * MB, 16, 5600 * MB));
    }

    /// 双阈值 AND：小/中文件中段不得判尾部，否则其真实惩罚检测会被整程禁用。
    /// 50MB@cap64，剩余 40MB：abs(40<128MB)✓ 但 rel(40 < 50/50=1MB)✗ → 非尾部。
    #[test]
    fn is_tail_false_for_small_file_midway() {
        assert!(!is_tail(40 * MB, 64, 50 * MB));
    }

    /// 巨型文件远未工作饥饿不得判尾部：100GB@cap16，剩余 2GB，abs(2GB<32MB)✗。
    #[test]
    fn is_tail_false_for_huge_file_far_from_end() {
        assert!(!is_tail(2048 * MB, 16, 102_400 * MB));
    }

    /// cap==1 的 no-op 由下游 allowed>1 / allowed<cap 守卫保证，非 is_tail 恒真：
    /// is_tail(x,1,_) 仅在 remaining 同时 <2MB 且 <total/50 时为真。
    #[test]
    fn is_tail_cap_one_is_not_always_true() {
        assert!(is_tail(512 * 1024, 1, 50 * MB));
        assert!(!is_tail(3 * MB, 1, 50 * MB));
    }

    /// 绝对阈值用严格 <：恰等于 cap×MIN_SPLIT 判非尾部，减 1 才入尾部。
    #[test]
    fn is_tail_absolute_boundary_excludes_equal() {
        assert!(!is_tail(32 * MB, 16, 5600 * MB));
        assert!(is_tail(32 * MB - 1, 16, 5600 * MB));
    }

    /// 直连比限速值慢：令牌恒有余量，限速器不等待 → 窗口不算 app-limited，
    /// 即使限速已启用（旧逻辑任何限速都整体关闭多路径）。
    #[test]
    fn window_below_limit_without_throttle_waits_is_not_app_limited() {
        let limit = 10 * 1024 * 1024;
        assert!(!super::window_app_limited(limit, 0, 2.0 * 1024.0 * 1024.0));
        // 吞吐恰在限速值上但限速器从未让连接等待：链路自己跑满，不是限速器。
        assert!(!super::window_app_limited(limit, 0, limit as f64));
    }

    /// 限速器饱和窗口（有节流等待且吞吐贴近限速值）才被判为 app-limited。
    #[test]
    fn saturated_window_with_throttle_waits_is_app_limited() {
        let limit = 10 * 1024 * 1024;
        assert!(super::window_app_limited(limit, 1, limit as f64));
        assert!(super::window_app_limited(limit, 500, 0.95 * limit as f64));
    }

    /// 边界：未限速永不 app-limited；偶发等待但吞吐远低于限速（刚改限速、
    /// 令牌桶冷启动）不算；吞吐恰为饱和下限算。
    #[test]
    fn app_limited_boundaries() {
        assert!(!super::window_app_limited(0, 100, 1e9), "limit=0 即未限速");
        let limit = 1_000_000u64;
        let floor = super::LIMITER_SATURATION_FLOOR * limit as f64;
        assert!(super::window_app_limited(limit, 1, floor));
        assert!(!super::window_app_limited(limit, 1, floor - 1.0));
        assert!(!super::window_app_limited(limit, 1, 0.0));
    }

    /// 快放手资格：需存活备选 + 改道配额，且生命线段（open_ended / no_range）排除。
    #[test]
    fn sys_fast_release_needs_live_alternate_and_excludes_lifeline_segments() {
        use super::sys_fast_release;
        assert!(sys_fast_release(true, false, false));
        assert!(
            !sys_fast_release(false, false, false),
            "无存活备选 / 配额耗尽"
        );
        assert!(!sys_fast_release(true, true, false), "open_ended 生命线");
        assert!(!sys_fast_release(true, false, true), "no_range 生命线");
    }

    /// SYS 租约快放手只对传输层失败生效：HTTP 类瞬时错误（503 等）保持原预算；
    /// 无资格时传输失败同样保持原预算；非 SYS 租约恒收紧。
    #[test]
    fn sys_lease_fast_release_applies_to_transport_errors_only() {
        use super::{MAX_RETRIES, PINNED_NODE_MAX_RETRIES, segment_retry_budget};
        // 有资格 + 传输失败 → 快速放手。
        assert_eq!(
            segment_retry_budget(false, true, true),
            PINNED_NODE_MAX_RETRIES
        );
        // 有资格 + HTTP 类错误 → 原耐心（换路修不好服务器错误）。
        assert_eq!(segment_retry_budget(false, true, false), MAX_RETRIES);
        // 无资格：传输失败也保持原预算。
        assert_eq!(segment_retry_budget(false, false, true), MAX_RETRIES);
        assert_eq!(segment_retry_budget(false, false, false), MAX_RETRIES);
        // 非 SYS 租约恒收紧，不论错误类别。
        assert_eq!(
            segment_retry_budget(true, false, false),
            PINNED_NODE_MAX_RETRIES
        );
        assert_eq!(
            segment_retry_budget(true, false, true),
            PINNED_NODE_MAX_RETRIES
        );
    }

    /// 中段真实惩罚（59MB/s→2MB/s、已到 cap、alive==allowed）仍应收缩。
    #[test]
    fn should_shrink_true_on_midway_penalty() {
        assert!(should_shrink(
            2_000_000.0,
            59_000_000.0,
            16,
            16,
            16,
            true,
            false
        ));
    }

    /// 扩容评估期内不收缩（留给扩容前后的直接对照，避免误判）。
    #[test]
    fn should_shrink_false_while_awaiting_eval() {
        assert!(!should_shrink(
            2_000_000.0,
            59_000_000.0,
            16,
            16,
            16,
            true,
            true
        ));
    }

    /// 未冻结且未到顶 → 收缩前置条件不满足。
    #[test]
    fn should_shrink_false_when_not_frozen_and_below_cap() {
        assert!(!should_shrink(
            2_000_000.0,
            59_000_000.0,
            8,
            16,
            8,
            false,
            false
        ));
    }

    /// alive != allowed（尾声退休 / 收缩过渡期）不收缩。
    #[test]
    fn should_shrink_false_when_alive_ne_allowed() {
        assert!(!should_shrink(
            2_000_000.0,
            59_000_000.0,
            16,
            16,
            8,
            true,
            false
        ));
    }

    /// allowed==1 到达下限不再收缩。
    #[test]
    fn should_shrink_false_at_min_allowed() {
        assert!(!should_shrink(
            2_000_000.0,
            59_000_000.0,
            1,
            1,
            1,
            true,
            false
        ));
    }

    /// 收缩验证：收缩后吞吐回升（连接数惩罚解除）保留收缩；观察满窗口仍未回升
    /// （链路噪声）撤销；窗口不足继续观察。
    #[test]
    fn shrink_eval_keeps_only_shrinks_that_recover_throughput() {
        use super::{RAMP_SHRINK_EVAL_WINDOWS, ShrinkEval, shrink_eval_verdict};
        assert_eq!(
            shrink_eval_verdict(5_000_000.0, 9_000_000.0, 1),
            ShrinkEval::Confirmed
        );
        assert_eq!(
            shrink_eval_verdict(5_000_000.0, 3_000_000.0, 1),
            ShrinkEval::Pending
        );
        assert_eq!(
            shrink_eval_verdict(5_000_000.0, 5_100_000.0, RAMP_SHRINK_EVAL_WINDOWS),
            ShrinkEval::Revert
        );
    }

    /// 正常可扩：未冻结、无连接敏感、额度用满、未达上限、本窗口未收缩。
    #[test]
    fn should_expand_true_when_healthy() {
        assert!(should_expand(false, false, 8, 8, 16, false));
    }

    /// 本窗口发生过收缩 → 禁止同窗口 re-expand（根治震荡）。
    #[test]
    fn should_expand_false_after_shrink_same_tick() {
        assert!(!should_expand(false, false, 16, 8, 16, true));
    }

    /// 已冻结 / 连接敏感 / 已达上限 均不扩容。
    #[test]
    fn should_expand_false_on_guards() {
        assert!(!should_expand(true, false, 8, 8, 16, false));
        assert!(!should_expand(false, true, 8, 8, 16, false));
        assert!(!should_expand(false, false, 16, 16, 16, false));
    }

    // -----------------------------------------------------------------------
    // is_http_400（配额型端点连接级拒绝分类）
    // -----------------------------------------------------------------------

    /// 构造带指定 HTTP 状态码的 DownloadError::Request（与 downloader.rs 测试
    /// 的同名辅助一致：从合成响应调用 error_for_status 得到真实 reqwest::Error）。
    fn make_status_error(status: u16) -> DownloadError {
        let http_resp = ::reqwest::Response::from(
            ::http::Response::builder()
                .status(status)
                .body("")
                .unwrap_or_else(|_| {
                    panic!("failed to build http::Response with status {}", status)
                }),
        );
        let err = http_resp.error_for_status().unwrap_err();
        DownloadError::Request(err)
    }

    #[test]
    fn http_400_detected() {
        assert!(super::is_http_400(&make_status_error(400)));
    }

    #[test]
    fn http_400_is_not_server_rejection() {
        // 分类必须与 403/429 区分：400 走连接级拒绝降级（需同 generation
        // 其它在途请求佐证），但绝不写入域名连接数缓存。
        assert!(!is_server_rejection(&make_status_error(400)));
    }

    #[test]
    fn http_400_ignores_other_statuses() {
        assert!(!super::is_http_400(&make_status_error(403)));
        assert!(!super::is_http_400(&make_status_error(404)));
        assert!(!super::is_http_400(&make_status_error(500)));
    }

    #[test]
    fn http_400_ignores_non_request_errors() {
        assert!(!super::is_http_400(&DownloadError::Other(
            "400".to_string()
        )));
    }

    #[test]
    fn range_not_satisfiable_detects_416_in_coordinator_scope() {
        assert!(is_range_not_satisfiable(&make_status_error(416)));
        assert!(!is_range_not_satisfiable(&make_status_error(400)));
        assert!(!is_range_not_satisfiable(&DownloadError::Other(
            "416".to_string()
        )));
    }

    // -----------------------------------------------------------------------
    // check_cross_segment_validators（hint 模式跨段版本一致性 latch）
    // -----------------------------------------------------------------------

    #[test]
    fn cross_segment_validators_latch_first_then_accept_same() {
        let latch = StdMutex::new(None);
        // 第一个 206：建立基线。
        assert!(check_cross_segment_validators(&latch, "\"e1\"", "Mon, 01 Jan 2024").is_ok());
        // 后续段同版本：通过。
        assert!(check_cross_segment_validators(&latch, "\"e1\"", "Mon, 01 Jan 2024").is_ok());
    }

    #[test]
    fn cross_segment_validators_detect_etag_drift() {
        let latch = StdMutex::new(None);
        assert!(check_cross_segment_validators(&latch, "\"e1\"", "").is_ok());
        // 文件中途被替换：ETag 漂移必须报错（否则拼出新旧混合的损坏文件）。
        let err = check_cross_segment_validators(&latch, "\"e2\"", "");
        assert_eq!(err, Err(("\"e1\"".to_string(), String::new())));
    }

    #[test]
    fn cross_segment_validators_detect_last_modified_drift() {
        let latch = StdMutex::new(None);
        assert!(check_cross_segment_validators(&latch, "", "Mon, 01 Jan 2024").is_ok());
        let err = check_cross_segment_validators(&latch, "", "Tue, 02 Jan 2024");
        assert_eq!(err, Err((String::new(), "Mon, 01 Jan 2024".to_string())));
    }

    #[test]
    fn cross_segment_validators_tolerate_stripped_headers() {
        // CDN 在 206 上剥离 validator：基线为空串 → 永不比较（缺失容忍，
        // 与 probe 路径策略一致）。
        let latch = StdMutex::new(None);
        assert!(check_cross_segment_validators(&latch, "", "").is_ok());
        assert!(check_cross_segment_validators(&latch, "\"e9\"", "any").is_ok());
        // 反向：基线有值、响应剥离 → 同样容忍。
        let latch2 = StdMutex::new(None);
        assert!(check_cross_segment_validators(&latch2, "\"e1\"", "lm1").is_ok());
        assert!(check_cross_segment_validators(&latch2, "", "").is_ok());
    }

    // -----------------------------------------------------------------------
    // rebuild_seg_states (F044: single-writer contract for downloaded_bytes)
    // -----------------------------------------------------------------------

    // rebuild_seg_states 必须保留 worker 在 seg_states 里维护的 downloaded_bytes，
    // 而不是用 coordinator 内存映射里可能陈旧的值覆盖之。否则会回退活跃段进度、
    // 让 UI 短暂倒退，并向 try_split_largest 喂入过期的 split point。
    #[test]
    fn rebuild_preserves_worker_downloaded_bytes() {
        let mut segs = BTreeMap::new();
        // coordinator 映射里的 downloaded_bytes 是陈旧的低值（100）。
        segs.insert(0, make_seg(0, 0, 999, 100, SegState::Active));

        // 共享状态里 worker 已推进到更高的值（700）。
        let seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>> =
            Arc::new(StdMutex::new(vec![SegmentProgressInfo {
                index: 0,
                start_byte: 0,
                end_byte: 999,
                downloaded_bytes: 700,
                active: None,
            }]));

        rebuild_seg_states(&segs, &seg_states);

        let states = seg_states.lock().expect("lock not poisoned");
        let s0 = states.iter().find(|s| s.index == 0).expect("seg 0 exists");
        assert_eq!(
            s0.downloaded_bytes, 700,
            "rebuild 必须保留 worker 的较高进度，而非覆盖为 coordinator 的陈旧值"
        );
    }

    // 新生 split 子段（不在旧 seg_states 快照里）应保留来自映射的 downloaded_bytes，
    // 同时父段已存在的进度仍被保留。
    #[test]
    fn rebuild_keeps_new_split_child_and_preserves_parent() {
        let mut segs = BTreeMap::new();
        // 父段 0 被 split：end_byte 收窄到 499；映射里其 downloaded 为陈旧 100。
        segs.insert(0, make_seg(0, 0, 499, 100, SegState::Active));
        // split 出的新子段 1，downloaded_bytes 来自映射（0）。
        segs.insert(1, make_seg(1, 500, 999, 0, SegState::Pending));

        // 旧快照只有父段 0，且 worker 进度已到 300（高于映射）。
        let seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>> =
            Arc::new(StdMutex::new(vec![SegmentProgressInfo {
                index: 0,
                start_byte: 0,
                end_byte: 999,
                downloaded_bytes: 300,
                active: None,
            }]));

        rebuild_seg_states(&segs, &seg_states);

        let states = seg_states.lock().expect("lock not poisoned");
        let s0 = states.iter().find(|s| s.index == 0).expect("seg 0 exists");
        let s1 = states.iter().find(|s| s.index == 1).expect("seg 1 exists");
        // 父段保留 worker 进度（300），但 end_byte 取映射里收窄后的新值（499）。
        assert_eq!(s0.downloaded_bytes, 300, "父段进度应被保留");
        assert_eq!(s0.end_byte, 499, "父段 end_byte 应反映 split 收窄");
        // 新子段不在旧快照里，downloaded_bytes 取映射值（0）。
        assert_eq!(s1.downloaded_bytes, 0, "新 split 子段进度取映射值");
        assert_eq!(s1.start_byte, 500);
        assert_eq!(s1.end_byte, 999);
    }

    // build_seg_state_vec 自身仍按 map 原样构建（不涉及保留逻辑），作为基线对照。
    #[test]
    fn build_seg_state_vec_mirrors_map() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 499, 250, SegState::Active));
        segs.insert(1, make_seg(1, 500, 999, 0, SegState::Pending));
        let v = build_seg_state_vec(&segs);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].downloaded_bytes, 250);
        assert_eq!(v[1].downloaded_bytes, 0);
    }

    // -----------------------------------------------------------------------
    // validate_coverage
    // -----------------------------------------------------------------------

    #[test]
    fn coverage_valid_single_segment() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_ok());
    }

    #[test]
    fn coverage_valid_multi_segment() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 499, 500, SegState::Completed));
        segs.insert(1, make_seg(1, 500, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_ok());
    }

    #[test]
    fn coverage_gap_detected() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 499, 0, SegState::Active));
        // Gap: 500 is missing
        segs.insert(1, make_seg(1, 501, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    #[test]
    fn coverage_overlap_detected() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 500, 0, SegState::Active));
        // Overlap: both cover byte 500
        segs.insert(1, make_seg(1, 500, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    #[test]
    fn coverage_wrong_start() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 1, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    #[test]
    fn coverage_wrong_end() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 998, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    #[test]
    fn coverage_empty_segments() {
        let segs = BTreeMap::new();
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    // -----------------------------------------------------------------------
    // try_split_largest
    // -----------------------------------------------------------------------

    #[test]
    fn split_largest_basic() {
        let mut segs = BTreeMap::new();
        // Segment 0: 0..99MB, downloaded 10MB — remaining 90MB
        segs.insert(
            0,
            make_seg(0, 0, 100_000_000 - 1, 10_000_000, SegState::Active),
        );
        // Segment 1: 100MB..199MB, downloaded 50MB — remaining 50MB
        segs.insert(
            1,
            make_seg(
                1,
                100_000_000,
                200_000_000 - 1,
                50_000_000,
                SegState::Active,
            ),
        );

        let mut next_idx = 2;
        let result = try_split_largest(
            &mut segs,
            &mut next_idx,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(result.is_some(), "should split the largest segment");

        let next = result.expect("already checked");
        assert_eq!(
            next.assignment.seg_index, 2,
            "new segment index should be 2"
        );
        assert_eq!(next_idx, 3);
        assert_eq!(next.split_parent, Some(0), "parent should be segment 0");

        // Original segment 0 should have a smaller end_byte now.
        let orig = segs.get(&0).expect("segment 0 exists");
        assert!(
            orig.end_byte < 100_000_000 - 1,
            "segment 0 should be shrunk"
        );

        // New segment should cover the upper half.
        let new_seg = segs.get(&2).expect("segment 2 exists");
        assert_eq!(new_seg.end_byte, 100_000_000 - 1);
        assert_eq!(new_seg.start_byte, next.assignment.seg_start);

        // Coverage must remain valid.
        assert!(
            validate_coverage(&segs, 200_000_000).is_ok(),
            "coverage must be valid after split"
        );
    }

    #[test]
    fn split_no_split_when_too_small() {
        let mut segs = BTreeMap::new();
        // Segment with only 2MB remaining — below MIN_SPLIT_BYTES.
        segs.insert(0, make_seg(0, 0, 3_000_000, 1_000_001, SegState::Active));

        let mut next_idx = 1;
        let result = try_split_largest(
            &mut segs,
            &mut next_idx,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(result.is_none(), "should not split small segments");
    }

    #[test]
    fn split_respects_max_segments() {
        let mut segs = BTreeMap::new();
        for i in 0..MAX_SEGMENTS {
            segs.insert(
                i,
                make_seg(
                    i,
                    i as i64 * 10_000_000,
                    (i as i64 + 1) * 10_000_000 - 1,
                    0,
                    SegState::Active,
                ),
            );
        }
        let mut next_idx = MAX_SEGMENTS;
        let result = try_split_largest(
            &mut segs,
            &mut next_idx,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(result.is_none(), "should not exceed MAX_SEGMENTS");
    }

    /// After Fix 1: completed segments do not count toward MAX_SEGMENTS.
    /// MAX_SEGMENTS-1 Completed + 1 Active of 10 MB should allow a split because
    /// active_or_pending = 1 < MAX_SEGMENTS.
    #[test]
    fn split_allowed_when_completed_segments_free_slots() {
        let total_bytes: i64 = 10_000_000;
        let mut segs = BTreeMap::new();
        // MAX_SEGMENTS-1 completed segments (minimal placeholder ranges).
        for i in 0..(MAX_SEGMENTS - 1) {
            segs.insert(i, make_seg(i, i as i64, i as i64, 1, SegState::Completed));
        }
        // 1 active segment with 10 MB remaining (well above MIN_SPLIT_BYTES).
        segs.insert(
            MAX_SEGMENTS - 1,
            make_seg(MAX_SEGMENTS - 1, 0, total_bytes - 1, 0, SegState::Active),
        );
        let mut next_idx = MAX_SEGMENTS;

        // With old code: segments.len() == MAX_SEGMENTS → None (workers retired).
        // With fix: active_or_pending == 1 < MAX_SEGMENTS → should split successfully.
        let result = try_split_largest(
            &mut segs,
            &mut next_idx,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(
            result.is_some(),
            "completed segments must not prevent splits of the remaining active segment"
        );
        // next_idx must have been incremented, confirming a new segment was created.
        assert_eq!(
            next_idx,
            MAX_SEGMENTS + 1,
            "next_idx must advance after a successful split"
        );
        // The new segment must exist in the map.
        assert!(
            segs.contains_key(&MAX_SEGMENTS),
            "new segment must be inserted into the map"
        );
        // Note: validate_coverage is intentionally omitted here because the 63
        // Completed placeholder segments use non-contiguous byte ranges (they are
        // stand-ins for "historically finished" slots, not a valid byte layout).
        // The purpose of this test is solely to verify that the active_or_pending
        // count check allows the split; byte-range integrity is covered by the
        // split_consecutive_splits_maintain_coverage and split_largest_basic tests.
    }

    /// MAX_SEGMENTS still limits truly concurrent connections:
    /// when MAX_SEGMENTS Active/Pending segments exist, no further split is allowed.
    #[test]
    fn split_blocked_when_max_active_segments_reached() {
        let mut segs = BTreeMap::new();
        for i in 0..MAX_SEGMENTS {
            segs.insert(
                i,
                make_seg(
                    i,
                    i as i64 * 1000,
                    i as i64 * 1000 + 999,
                    0,
                    SegState::Active,
                ),
            );
        }
        let mut next_idx = MAX_SEGMENTS;

        // active_or_pending == MAX_SEGMENTS → must still return None.
        let result = try_split_largest(
            &mut segs,
            &mut next_idx,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(
            result.is_none(),
            "must not exceed MAX_SEGMENTS active connections"
        );
    }

    #[test]
    fn split_consecutive_splits_maintain_coverage() {
        let total_bytes: i64 = 200_000_000;
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, total_bytes - 1, 0, SegState::Active));

        let mut next_idx = 1;

        // Perform multiple consecutive splits.
        for _ in 0..5 {
            let result = try_split_largest(
                &mut segs,
                &mut next_idx,
                MIN_SPLIT_BYTES,
                SplitCost::default(),
            );
            assert!(result.is_some(), "should be able to split");
            assert!(
                validate_coverage(&segs, total_bytes).is_ok(),
                "coverage must remain valid after each split"
            );
        }

        // All segments should cover exactly [0, total_bytes-1].
        let total_coverage: i64 = segs.values().map(|s| s.size()).sum();
        assert_eq!(total_coverage, total_bytes);
    }

    #[test]
    fn split_with_progress_uses_correct_midpoint() {
        let total_bytes: i64 = 100_000_000;
        let mut segs = BTreeMap::new();
        // Segment at 70% progress — remaining 30MB.
        segs.insert(
            0,
            make_seg(0, 0, total_bytes - 1, 70_000_000, SegState::Active),
        );

        let mut next_idx = 1;
        let result = try_split_largest(
            &mut segs,
            &mut next_idx,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(result.is_some());

        let next = result.expect("checked");
        // Split should be at midpoint of remaining [70_000_000, 99_999_999].
        // remaining = 30_000_000, midpoint = 70_000_000 + 15_000_000 = 85_000_000
        assert_eq!(next.assignment.seg_start, 85_000_000);
        assert_eq!(next.assignment.seg_end, 99_999_999);

        let orig = segs.get(&0).expect("exists");
        assert_eq!(orig.end_byte, 84_999_999);

        assert!(validate_coverage(&segs, total_bytes).is_ok());
    }

    #[test]
    fn split_does_not_split_completed_segments() {
        let mut segs = BTreeMap::new();
        segs.insert(
            0,
            make_seg(0, 0, 9_999_999, 10_000_000, SegState::Completed),
        );
        segs.insert(1, make_seg(1, 10_000_000, 19_999_999, 0, SegState::Active));
        let mut next = 2;

        let result = try_split_largest(&mut segs, &mut next, MIN_SPLIT_BYTES, SplitCost::default());
        assert!(result.is_some());

        let next = result.expect("checked");
        // Should split segment 1 (Active), not segment 0 (Completed).
        assert_eq!(next.split_parent, Some(1));
    }

    // -----------------------------------------------------------------------
    // find_next_work
    // -----------------------------------------------------------------------

    #[test]
    fn find_work_prefers_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 50_000_000, 0, SegState::Active));
        segs.insert(
            1,
            make_seg(1, 50_000_001, 100_000_000, 0, SegState::Pending),
        );

        let mut next_idx = 2;
        let result = find_next_work(
            &mut segs,
            &mut next_idx,
            100_000_001,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(result.is_some());
        let next = result.expect("checked");
        assert_eq!(
            next.assignment.seg_index, 1,
            "should pick the pending segment first"
        );
        assert!(
            next.split_parent.is_none(),
            "pending reuse should not have split_parent"
        );
    }

    #[test]
    fn find_work_splits_when_no_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 9_999_999, 0, SegState::Active));
        let mut next_idx = 1;

        let result = find_next_work(
            &mut segs,
            &mut next_idx,
            10_000_000,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(result.is_some());
        let next = result.expect("checked");
        assert!(next.split_parent.is_some(), "should come from a split");
        assert_eq!(next_idx, 2, "next_index should have advanced");
    }

    #[test]
    fn find_work_returns_none_when_all_done() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));

        let mut next_idx = 1;
        let result = find_next_work(
            &mut segs,
            &mut next_idx,
            100,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(result.is_none(), "no work when all completed");
    }

    // -----------------------------------------------------------------------
    // Tail micro-split (Strategy 3 in find_next_work)
    // -----------------------------------------------------------------------

    /// When a segment's remaining bytes are between TAIL_MIN_SPLIT_BYTES*2 and
    /// MIN_SPLIT_BYTES, normal split fails but tail micro-split succeeds.
    #[test]
    fn tail_microsplit_splits_below_normal_threshold() {
        let mut segs = BTreeMap::new();
        // 500 KB remaining — too small for MIN_SPLIT_BYTES (2 MB) but
        // large enough for TAIL_MIN_SPLIT_BYTES (64 KB).
        let remaining = 500 * 1024; // 500 KB
        assert!(
            remaining < MIN_SPLIT_BYTES,
            "precondition: below normal threshold"
        );
        assert!(
            remaining >= TAIL_MIN_SPLIT_BYTES * 2,
            "precondition: above tail threshold"
        );
        segs.insert(0, make_seg(0, 0, remaining - 1, 0, SegState::Active));

        let mut next_idx = 1;

        // Normal split should fail:
        let normal = try_split_largest(
            &mut segs,
            &mut next_idx,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(normal.is_none(), "normal split should fail for 500 KB");

        // But find_next_work should succeed via tail micro-split (Strategy 3):
        let result = find_next_work(
            &mut segs,
            &mut next_idx,
            remaining,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(
            result.is_some(),
            "tail micro-split should succeed for 500 KB"
        );
        let next = result.expect("checked");
        assert!(next.split_parent.is_some(), "should come from a split");
        assert!(
            validate_coverage(&segs, remaining).is_ok(),
            "coverage must be valid after tail micro-split"
        );
    }

    /// Segments smaller than 2× TAIL_MIN_SPLIT_BYTES cannot be micro-split.
    #[test]
    fn tail_microsplit_respects_minimum() {
        let mut segs = BTreeMap::new();
        // 100 KB remaining — just above TAIL_MIN_SPLIT_BYTES (64 KB) but below 2×64 KB=128 KB.
        // Actually TAIL_MIN_SPLIT_BYTES is 64KB, and try_split_largest requires remaining >= threshold.
        // With 100KB remaining and threshold 64KB, the split point would be at 50KB from current_pos.
        // Each half would be 50KB, which is < 64KB... but the check is remaining >= threshold,
        // not each-half >= threshold. Let's use 60 KB which is < 64 KB.
        let remaining = 60 * 1024; // 60 KB < TAIL_MIN_SPLIT_BYTES
        segs.insert(0, make_seg(0, 0, remaining - 1, 0, SegState::Active));

        let mut next_idx = 1;
        let result = find_next_work(
            &mut segs,
            &mut next_idx,
            remaining,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(
            result.is_none(),
            "should not split segment smaller than TAIL_MIN_SPLIT_BYTES"
        );
    }

    /// Tail micro-split does not trigger when min_split is already at
    /// TAIL_MIN_SPLIT_BYTES (guard: min_split > TAIL_MIN_SPLIT_BYTES).
    #[test]
    fn tail_microsplit_no_infinite_retry() {
        let mut segs = BTreeMap::new();
        // 100 KB remaining, min_split already at TAIL_MIN_SPLIT_BYTES.
        let remaining = 100 * 1024;
        segs.insert(0, make_seg(0, 0, remaining - 1, 0, SegState::Active));

        let mut next_idx = 1;
        // When min_split == TAIL_MIN_SPLIT_BYTES, Strategy 3 should not retry.
        let result = find_next_work(
            &mut segs,
            &mut next_idx,
            remaining,
            TAIL_MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        // 100KB >= 64KB so try_split_largest(TAIL) succeeds, but we're testing
        // that when called with TAIL_MIN_SPLIT_BYTES directly, Strategy 2
        // handles it (not Strategy 3 infinite loop).
        // Strategy 2: try_split_largest(segs, next, 64KB) with 100KB remaining → succeeds.
        assert!(
            result.is_some(),
            "Strategy 2 itself should handle TAIL_MIN_SPLIT_BYTES"
        );
    }

    /// 逐段速率尚无样本时按单连接估计判断：持有者 0.25s 能下完的 128KB 尾巴，
    /// 帮手要 1.2s 才能收到首字节——不拆（否则多一个请求、空占一条连接）；
    /// 无延迟样本时仍按历史行为拆分。
    #[test]
    fn tail_split_waits_for_holder_when_helper_setup_dominates() {
        let remaining = 128 * 1024;
        let slow_setup = SplitCost {
            setup_secs: 1.2,
            conn_bps: Some(512.0 * 1024.0),
            max_keep: None,
        };
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, remaining - 1, 0, SegState::Active));
        let mut next_idx = 1;
        assert!(
            find_next_work(
                &mut segs,
                &mut next_idx,
                remaining,
                MIN_SPLIT_BYTES,
                slow_setup
            )
            .is_none()
        );

        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, remaining - 1, 0, SegState::Active));
        let mut next_idx = 1;
        assert!(
            find_next_work(
                &mut segs,
                &mut next_idx,
                remaining,
                MIN_SPLIT_BYTES,
                SplitCost::default()
            )
            .is_some()
        );
    }

    /// 随写增长的文件（exFAT SD 卡，#974）：每次拆分帮手都紧贴持有者一个窗口
    /// 起写，连续扩容后所有起写点都落在前沿附近，而不是跳到剩余区间中点
    /// （高偏移首写会零填充整个间隙）。不限窗口时仍按中点均衡拆分。
    #[test]
    fn windowed_split_keeps_helpers_adjacent_to_the_write_frontier() {
        let total: i64 = 1024 * 1024 * 1024;
        let window = super::file_layout::FRONTIER_WINDOW_BYTES;
        let cost = SplitCost {
            max_keep: Some(window),
            ..SplitCost::default()
        };
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, total - 1, 0, SegState::Active));
        let mut next_idx = 1;
        for step in 1..=4 {
            let next = find_next_work(&mut segs, &mut next_idx, total, MIN_SPLIT_BYTES, cost)
                .expect("tail segment must split");
            assert_eq!(next.assignment.actual_start, step * window, "split #{step}");
            assert_eq!(next.assignment.seg_end, total - 1);
        }
        validate_coverage(&segs, total).expect("windowed splits keep exact coverage");

        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, total - 1, 0, SegState::Active));
        let mut next_idx = 1;
        let next = find_next_work(
            &mut segs,
            &mut next_idx,
            total,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        )
        .expect("split");
        assert_eq!(next.assignment.actual_start, total / 2);
    }

    /// dynamic_min_split_bytes returns expected thresholds at boundary speeds.
    #[test]
    fn dynamic_min_split_at_boundaries() {
        // < 1 MB/s → 512 KB
        assert_eq!(dynamic_min_split_bytes(500.0 * 1024.0), 512 * 1024);
        // 1 MB/s – 10 MB/s → 1 MB
        assert_eq!(dynamic_min_split_bytes(5.0 * 1024.0 * 1024.0), 1024 * 1024);
        // > 10 MB/s → 2 MB (MIN_SPLIT_BYTES)
        assert_eq!(
            dynamic_min_split_bytes(50.0 * 1024.0 * 1024.0),
            MIN_SPLIT_BYTES
        );
    }

    /// Tail micro-split maintains full byte coverage after splitting.
    #[test]
    fn tail_microsplit_maintains_coverage() {
        let total: i64 = 10 * 1024 * 1024; // 10 MB
        let mut segs = BTreeMap::new();
        // Two segments: seg0 completed, seg1 active with 300 KB remaining.
        let seg1_start = total - 300 * 1024;
        segs.insert(
            0,
            make_seg(0, 0, seg1_start - 1, seg1_start, SegState::Completed),
        );
        segs.insert(1, make_seg(1, seg1_start, total - 1, 0, SegState::Active));
        assert!(validate_coverage(&segs, total).is_ok(), "precondition");

        let mut next_idx = 2;
        let result = find_next_work(
            &mut segs,
            &mut next_idx,
            total,
            MIN_SPLIT_BYTES,
            SplitCost::default(),
        );
        assert!(result.is_some(), "tail micro-split should work");
        assert!(
            validate_coverage(&segs, total).is_ok(),
            "coverage must remain valid after tail micro-split"
        );
        // Verify three segments now exist.
        assert_eq!(segs.len(), 3);
    }

    // -----------------------------------------------------------------------
    // try_proactive_split
    // -----------------------------------------------------------------------

    #[test]
    fn proactive_split_skips_when_pending_exists() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 49_999_999, 0, SegState::Active));
        segs.insert(1, make_seg(1, 50_000_000, 99_999_999, 0, SegState::Pending));

        let mut next_idx = 2;
        assert!(
            try_proactive_split(
                &mut segs,
                &mut next_idx,
                MIN_SPLIT_BYTES,
                SplitCost::default()
            )
            .is_none(),
            "should not proactively split when Pending segments exist"
        );
    }

    #[test]
    fn proactive_split_creates_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 19_999_999, 0, SegState::Active));
        let mut next = 1;

        let result =
            try_proactive_split(&mut segs, &mut next, MIN_SPLIT_BYTES, SplitCost::default());
        assert!(result.is_some(), "proactive split should succeed");

        // New segment should be Pending.
        let new_seg = segs.get(&1).expect("new segment exists");
        assert_eq!(new_seg.state, SegState::Pending);
        assert!(validate_coverage(&segs, 20_000_000).is_ok());
    }

    // -----------------------------------------------------------------------
    // all_done
    // -----------------------------------------------------------------------

    #[test]
    fn all_done_true_when_all_completed() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 100, SegState::Completed));
        assert!(all_done(&segs));
    }

    #[test]
    fn all_done_false_when_active() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 50, SegState::Active));
        assert!(!all_done(&segs));
    }

    #[test]
    fn all_done_false_when_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 0, SegState::Pending));
        assert!(!all_done(&segs));
    }

    #[test]
    fn completed_segments_do_not_prove_current_connection_success() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 0, SegState::Active));

        assert!(
            !has_other_in_flight(&segs, 1),
            "恢复前完成的段不能把当前 URL 的 403 误判为并发限制"
        );

        segs.insert(2, make_seg(2, 200, 299, 0, SegState::Active));
        assert!(
            has_other_in_flight(&segs, 1),
            "同 generation 的其它在途请求应允许当前任务自适应降级"
        );
    }

    // -----------------------------------------------------------------------
    // LiveSegment methods
    // -----------------------------------------------------------------------

    #[test]
    fn segment_size_and_remaining() {
        let seg = make_seg(0, 100, 199, 50, SegState::Active);
        assert_eq!(seg.size(), 100);
        assert_eq!(seg.remaining(), 50);
        assert!(!seg.is_complete());
    }

    #[test]
    fn segment_complete() {
        let seg = make_seg(0, 0, 99, 100, SegState::Completed);
        assert!(seg.is_complete());
        assert_eq!(seg.remaining(), 0);
    }

    // -----------------------------------------------------------------------
    // find_next_pending_only（串行模式专用）
    // -----------------------------------------------------------------------

    #[test]
    fn pending_only_returns_pending_segment() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 0, SegState::Pending));
        segs.insert(2, make_seg(2, 200, 299, 0, SegState::Pending));

        let result = find_next_pending_only(&mut segs);
        assert!(result.is_some());
        let next = result.unwrap();
        assert_eq!(next.assignment.seg_index, 1);
        assert!(next.split_parent.is_none(), "串行模式不应产生拆分");
        // 分段应被标记为 Active
        assert_eq!(segs.get(&1).unwrap().state, SegState::Active);
    }

    #[test]
    fn pending_only_returns_none_when_no_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 50, SegState::Active));

        let result = find_next_pending_only(&mut segs);
        assert!(result.is_none(), "没有 Pending 分段时应返回 None");
    }

    #[test]
    fn pending_only_never_splits() {
        // 即使有很大的 Active 分段，串行模式也不应拆分
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99_999_999, 1_000_000, SegState::Active));

        let result = find_next_pending_only(&mut segs);
        assert!(result.is_none(), "串行模式不应拆分 Active 分段");
        assert_eq!(segs.len(), 1, "分段数量不应增加");
    }

    #[test]
    fn pending_only_resumes_partial_progress() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 999, 500, SegState::Pending));

        let result = find_next_pending_only(&mut segs);
        assert!(result.is_some());
        let next = result.unwrap();
        assert_eq!(next.assignment.seg_start, 0);
        assert_eq!(next.assignment.actual_start, 500, "应从已下载位置续传");
        assert_eq!(next.assignment.seg_end, 999);
    }

    // -----------------------------------------------------------------------
    // is_server_rejection
    // -----------------------------------------------------------------------

    #[test]
    fn server_rejection_ignores_non_request_errors() {
        // Other、Cancelled、Io 等非 Request 类型的错误不应被判定为服务器拒绝
        assert!(!is_server_rejection(&DownloadError::Other(
            "some error".to_string()
        )));
        assert!(!is_server_rejection(&DownloadError::Cancelled));
        assert!(!is_server_rejection(&DownloadError::Other(
            "403 forbidden".to_string()
        )));
    }

    // -----------------------------------------------------------------------
    // 域名缓存（extract_host / record / is_single_conn）
    // -----------------------------------------------------------------------

    #[test]
    fn extract_host_basic() {
        assert_eq!(
            extract_host("https://example.com/file.zip"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn extract_host_with_port() {
        assert_eq!(
            extract_host("https://example.com:8443/file.zip"),
            Some("example.com:8443".to_string())
        );
    }

    #[test]
    fn extract_host_invalid_url() {
        assert_eq!(extract_host("not a url"), None);
    }

    #[test]
    fn single_conn_domain_record_and_check() {
        let url = "http://single-conn-test-record.example.com/file";
        let domain = "single-conn-test-record.example.com";

        // 预清理：确保本测试域名在全局缓存中不存在（防止并行/重试干扰）
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(domain);
        }

        assert!(!is_single_conn_domain(url), "记录前不应命中缓存");

        record_domain_conn_cap(url, 1);
        assert!(is_single_conn_domain(url), "记录后应命中缓存");

        // 同域名不同路径也应命中
        assert!(
            is_single_conn_domain("http://single-conn-test-record.example.com/other.zip"),
            "同域名不同路径应命中缓存"
        );

        // 不同域名不应命中
        assert!(
            !is_single_conn_domain("http://single-conn-test-record-other.example.com/file"),
            "不同域名不应命中缓存"
        );

        // 清理：从缓存中移除测试数据
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(domain);
        }
    }

    #[test]
    fn single_conn_domain_different_ports_are_separate() {
        let url_a = "http://single-conn-test-ports-a.example.com:8080/file";
        let url_b = "http://single-conn-test-ports-b.example.com:9090/file";
        let domain_a = "single-conn-test-ports-a.example.com:8080";
        let domain_b = "single-conn-test-ports-b.example.com:9090";

        // 预清理：确保两个测试域名在全局缓存中不存在
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(domain_a);
            cache.remove(domain_b);
        }

        record_domain_conn_cap(url_a, 1);
        assert!(is_single_conn_domain(url_a), "记录后 url_a 应命中缓存");
        // 不同域名（含不同端口）不应命中
        assert!(
            !is_single_conn_domain(url_b),
            "不同端口/域名应视为不同服务器"
        );

        // 清理
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(domain_a);
            cache.remove(domain_b);
        }
    }

    #[test]
    fn domain_conn_cap_records_and_keeps_minimum() {
        use super::{domain_conn_cap, record_domain_conn_cap};
        let url = "http://conn-cap-test-min.example.com/file";
        let domain = "conn-cap-test-min.example.com";

        // 预清理：确保本测试域名在全局缓存中不存在
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(domain);
        }

        assert_eq!(domain_conn_cap(url), None, "记录前不应命中缓存");

        // 记录上限 8：命中 cap 缓存但不是单连接
        record_domain_conn_cap(url, 8);
        assert_eq!(domain_conn_cap(url), Some(8));
        assert!(!is_single_conn_domain(url), "cap>1 不应视为单连接");

        // 更低的观察优先：先 8 后 4 → 4；再记 16 不得回涨
        record_domain_conn_cap(url, 4);
        assert_eq!(domain_conn_cap(url), Some(4));
        record_domain_conn_cap(url, 16);
        assert_eq!(
            domain_conn_cap(url),
            Some(4),
            "更高的 cap 不应覆盖更低的观察"
        );

        // cap 下限钳制到 1；cap==1 即单连接语义
        record_domain_conn_cap(url, 0);
        assert_eq!(domain_conn_cap(url), Some(1));
        assert!(is_single_conn_domain(url), "cap==1 即单连接");

        // 清理
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(domain);
        }
    }

    #[test]
    fn conn_caps_serialize_parse_roundtrip() {
        use super::{ConnPolicyEntry, parse_conn_caps, serialize_conn_caps};
        let mut map = std::collections::HashMap::new();
        map.insert(
            "a.example.com".to_string(),
            ConnPolicyEntry {
                cap: 4,
                cap_ts: 1000,
                hint: 0,
                hint_ts: 0,
            },
        );
        map.insert(
            "b.example.com:8443".to_string(),
            ConnPolicyEntry {
                cap: 1,
                cap_ts: 2000,
                hint: 12,
                hint_ts: 2100,
            },
        );

        let raw = serialize_conn_caps(&map);
        assert!(raw.starts_with("v3\n"), "首行必须是版本标记");
        assert_eq!(parse_conn_caps(&raw), map, "roundtrip 必须无损");
    }

    #[test]
    fn conn_caps_unknown_version_discarded_entirely() {
        use super::parse_conn_caps;
        // 旧版本（含产生过误降级记录的 v2）/未来版本/无版本头整体丢弃，
        // 绝不跨版本猜测字段含义。
        assert!(parse_conn_caps("v1\na.example.com\t4\t1000").is_empty());
        assert!(
            parse_conn_caps("v2\na.example.com\t1\t1000\t0\t0").is_empty(),
            "旧策略写出的单连接缓存必须失效并重新学习"
        );
        assert!(parse_conn_caps("v4\na.example.com\t4\t1000\t8\t1000").is_empty());
        assert!(parse_conn_caps("a.example.com\t4\t1000\t0\t0").is_empty());
        assert!(parse_conn_caps("").is_empty());
    }

    #[test]
    fn conn_caps_malformed_lines_skipped_within_version() {
        use super::{ConnPolicyEntry, parse_conn_caps};
        let raw = "v3\nok.example.com\t4\t1000\t8\t1500\n\
                   缺字段\t4\t1\nbad-cap.example.com\tx\t1\t0\t0\n\
                   neg.example.com\t-1\t1\t0\t0\nempty.example.com\t0\t0\t0\t0";
        let map = parse_conn_caps(raw);
        assert_eq!(map.len(), 1, "畸形/非法行应被跳过");
        assert_eq!(
            map.get("ok.example.com"),
            Some(&ConnPolicyEntry {
                cap: 4,
                cap_ts: 1000,
                hint: 8,
                hint_ts: 1500,
            })
        );
    }

    #[test]
    fn http_runtime_counts_only_body_readers_and_maps_physical_ranges() {
        use super::{ReportScope, sampled_http_runtime};
        use crate::downloader::SegmentProgressInfo;
        use crate::transfer_activity::TransferTracker;
        use std::sync::Mutex;

        let segments = Mutex::new(vec![
            SegmentProgressInfo {
                index: 0,
                start_byte: 0,
                end_byte: 499,
                downloaded_bytes: 300,
                active: None,
            },
            SegmentProgressInfo {
                index: 1,
                start_byte: 500,
                end_byte: 999,
                downloaded_bytes: 20,
                active: None,
            },
        ]);
        let scope = ReportScope {
            base: 200,
            total_override: 1200,
            owns_task_total: false,
            strict_total: true,
        };
        let tracker = TransferTracker::new();
        let reading = tracker.start(1);
        let (_, live) = sampled_http_runtime("track-pair", 1200, 8, &segments, scope, &tracker);
        assert_eq!(live.active_transfers, Some(1));
        assert_eq!(
            live.segments.iter().map(|s| s.active).collect::<Vec<_>>(),
            vec![None, Some(false), Some(true)]
        );
        assert_eq!(
            live.segments
                .iter()
                .map(|s| (s.start_byte, s.end_byte))
                .collect::<Vec<_>>(),
            vec![(0, 199), (200, 699), (700, 1199)]
        );
        drop(reading);
        let (_, idle) = sampled_http_runtime("track-pair", 1200, 8, &segments, scope, &tracker);
        assert_eq!(idle.active_transfers, Some(0));
        assert_eq!(
            idle.segments.iter().map(|s| s.active).collect::<Vec<_>>(),
            vec![None, Some(false), Some(false)]
        );
        assert_eq!(idle.segments[2].downloaded_bytes, 20);
    }

    #[test]
    fn conn_cap_learning_requires_concurrent_validated_response() {
        use super::GenerationEvidence;

        let evidence = GenerationEvidence::default();
        assert!(
            !evidence.can_learn_conn_cap(true),
            "仅收到 403/429、没有当前成功响应时不得学习域名连接上限"
        );

        {
            let _active = evidence.response_started(0);
            assert!(
                evidence.can_learn_conn_cap(true),
                "拒绝与另一个已验证响应同时存在时才构成连接压力证据"
            );
            assert!(
                !evidence.can_learn_conn_cap(false),
                "非 403/429 错误不得写入连接上限"
            );
        }

        assert!(
            !evidence.can_learn_conn_cap(true),
            "成功响应结束后不能用旧证据污染后续 URL"
        );
    }

    #[test]
    fn content_audit_only_when_version_evidence_is_suspiciously_missing() {
        use super::GenerationEvidence;

        // 无基线、响应也无 validator（hint 无 validator）：无嫌疑依据，不审计。
        assert!(!GenerationEvidence::default().content_audit_warranted("", ""));

        // 探测基线带 ETag，分段 206 全部剥离：审计。
        assert!(GenerationEvidence::default().content_audit_warranted("\"e1\"", ""));

        // 探测基线带 ETag 且有响应回显 ETag：已逐段强比对，不审计。
        let echoed = GenerationEvidence::default();
        echoed.note_response_validators("\"e1\"", "");
        assert!(!echoed.content_audit_warranted("\"e1\"", ""));

        // 基线仅有 Last-Modified：被剥离才审计，回显则不审计。
        assert!(GenerationEvidence::default().content_audit_warranted("", "lm"));
        let lm_echoed = GenerationEvidence::default();
        lm_echoed.note_response_validators("", "lm");
        assert!(!lm_echoed.content_audit_warranted("", "lm"));

        // 被容忍的 Last-Modified 漂移（无 ETag 佐证）：审计；有 ETag 佐证则不审计。
        let drift = GenerationEvidence::default();
        drift.note_response_validators("", "lm");
        drift.note_lm_drift_tolerated();
        assert!(drift.content_audit_warranted("", ""));
        let drift_with_etag = GenerationEvidence::default();
        drift_with_etag.note_response_validators("\"e1\"", "lm");
        drift_with_etag.note_lm_drift_tolerated();
        assert!(!drift_with_etag.content_audit_warranted("\"e1\"", "lm"));
    }

    #[test]
    fn count_domain_conn_policies_skips_stale_and_foreign_versions() {
        use super::{count_domain_conn_policies, now_unix_secs};
        let now = now_unix_secs();
        let stale = now.saturating_sub(48 * 3600);
        let raw = format!(
            "v3\nfresh.example\t1\t{now}\t0\t0\nold.example\t1\t{stale}\t0\t0\nhinted.example\t0\t0\t8\t{now}\nbroken\tx\n"
        );
        assert_eq!(count_domain_conn_policies(&raw), 2);
        assert_eq!(count_domain_conn_policies(""), 0);
        assert_eq!(
            count_domain_conn_policies(&format!("v1\nfresh.example\t1\t{now}")),
            0
        );
    }

    /// 来源累计的采样 / 落库：单直连池不归因；多路径池按在途 + 已回报累计，
    /// 峰值单调（在途瞬时回落不抖动）；落库只写增量，重复 flush 不重复计数，
    /// 被夺权的旧 spawn 写不进去。
    #[tokio::test]
    async fn source_bytes_sampling_is_monotonic_and_flushes_only_increments() {
        use super::{flush_source_bytes, sample_run_source_bytes};
        use crate::auto_proxy::{CandidateSource, RoutePath};
        use crate::cdn::NodePool;
        use crate::cdn::node_pool::LeaseRequest;
        use crate::model::SourceBytes;
        use tokio_util::sync::CancellationToken;

        let dir = std::env::temp_dir().join(format!("fluxdown_srcb_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db = crate::db::Db::open(&dir).await.expect("open db");
        db.insert_task(
            "t",
            "http://example.com/f",
            "f",
            "/tmp",
            1,
            0,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert task");
        db.set_segments_epoch("t", 7).await.expect("set epoch");

        let mut segments = BTreeMap::new();
        segments.insert(0, make_seg(0, 0, 9_999, 0, SegState::Active));
        let seg_states = Arc::new(StdMutex::new(build_seg_state_vec(&segments)));
        let set_progress = |bytes: i64| {
            seg_states.lock().unwrap()[0].downloaded_bytes = bytes;
        };
        let base = SourceBytes {
            cdn: 1,
            proxy: 10,
            nic: 100,
        };
        let mut peak = SourceBytes::default();

        // 单直连池：该路径不做来源归因。
        let single = NodePool::single(reqwest::Client::new());
        assert_eq!(
            sample_run_source_bytes(&single, &mut segments, &seg_states, base, &mut peak),
            None
        );

        // 多路径池：槽位 1 = 已有先验的代理路径，实测更快 → 租约落在代理上。
        let manual = RoutePath::Proxy(CandidateSource::ManualFields);
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(manual, reqwest::Client::new(), Some(1e6))],
        );
        pool.observe_window(&[(0, 1e3), (1, 1e6)]);
        let lease = pool.lease_for(LeaseRequest {
            seg_index: 0,
            start_downloaded: 0,
            bytes: i64::MAX,
            allow_alternates: true,
            cancel: CancellationToken::new(),
        });
        assert_eq!(lease.route(), manual);

        // 在途 800 B 全部走代理；返回值 = 基数 + 本次运行累计。
        set_progress(800);
        let live = sample_run_source_bytes(&pool, &mut segments, &seg_states, base, &mut peak);
        assert_eq!(
            live,
            Some(SourceBytes {
                cdn: 1,
                proxy: 810,
                nic: 100
            })
        );
        let mut flushed = SourceBytes::default();
        flush_source_bytes(&db, "t", 7, peak, &mut flushed).await;
        assert_eq!(flushed.proxy, 800);
        assert_eq!(
            db.load_task_source_bytes("t").await.expect("load").proxy,
            800
        );

        // 在途口径瞬时回落：峰值不回退，落库无新增。
        set_progress(300);
        sample_run_source_bytes(&pool, &mut segments, &seg_states, base, &mut peak);
        assert_eq!(peak.proxy, 800);
        flush_source_bytes(&db, "t", 7, peak, &mut flushed).await;
        assert_eq!(
            db.load_task_source_bytes("t").await.expect("load").proxy,
            800
        );

        // 租约结束回报 1200 B、在途清零后继续增长：只追加 400 B 增量。
        pool.record_transfer(&lease, 1_200);
        drop(lease);
        sample_run_source_bytes(&pool, &mut segments, &seg_states, base, &mut peak);
        assert_eq!(peak.proxy, 1_200);
        flush_source_bytes(&db, "t", 7, peak, &mut flushed).await;
        flush_source_bytes(&db, "t", 7, peak, &mut flushed).await;
        assert_eq!(
            db.load_task_source_bytes("t").await.expect("load"),
            SourceBytes {
                cdn: 0,
                proxy: 1_200,
                nic: 0
            },
            "repeated flush of the same cumulative value must not double count"
        );

        // 被新 spawn 夺权：旧 coordinator 的落库不生效。
        db.set_segments_epoch("t", 8).await.expect("set epoch");
        let mut stale_flushed = SourceBytes::default();
        flush_source_bytes(&db, "t", 7, peak, &mut stale_flushed).await;
        assert_eq!(
            db.load_task_source_bytes("t").await.expect("load").proxy,
            1_200
        );

        drop(db);
        if let Err(error) = std::fs::remove_dir_all(dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
    }

    #[tokio::test]
    async fn conn_caps_persist_and_reload_roundtrip() {
        use super::{
            CONN_CAP_CONFIG_KEY, ConnPolicyEntry, domain_conn_cap, load_domain_conn_caps,
            now_unix_secs, serialize_conn_caps,
        };
        let dir = std::env::temp_dir().join(format!("fluxdown_connp_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db = crate::db::Db::open(&dir).await.expect("open db");

        let fresh_host = "persist-fresh.example.com";
        let stale_host = "persist-stale.example.com";
        let mixed_host = "persist-mixed.example.com";
        // 预清理全局缓存
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(fresh_host);
            cache.remove(stale_host);
            cache.remove(mixed_host);
        }

        // 三种条目：全新鲜 / 全过期（25h 前）/ cap 过期但 hint 新鲜
        let now = now_unix_secs();
        let mut map = std::collections::HashMap::new();
        map.insert(
            fresh_host.to_string(),
            ConnPolicyEntry {
                cap: 4,
                cap_ts: now,
                hint: 3,
                hint_ts: now,
            },
        );
        map.insert(
            stale_host.to_string(),
            ConnPolicyEntry {
                cap: 1,
                cap_ts: now - 25 * 3600,
                hint: 8,
                hint_ts: now - 25 * 3600,
            },
        );
        map.insert(
            mixed_host.to_string(),
            ConnPolicyEntry {
                cap: 2,
                cap_ts: now - 25 * 3600,
                hint: 16,
                hint_ts: now,
            },
        );
        db.set_config(CONN_CAP_CONFIG_KEY, &serialize_conn_caps(&map))
            .await
            .expect("set_config");

        load_domain_conn_caps(&db).await;

        let fresh_url = format!("http://{fresh_host}/f");
        assert_eq!(
            domain_conn_cap(&fresh_url),
            Some(4),
            "新鲜 cap 应跨重启恢复"
        );
        assert_eq!(
            domain_conn_hint(&fresh_url),
            Some(3),
            "新鲜 hint 应跨重启恢复（3 < cap=4，钳制不生效）"
        );
        let stale_url = format!("http://{stale_host}/f");
        assert_eq!(
            domain_conn_cap(&stale_url),
            None,
            "全过期条目在加载时应被丢弃"
        );
        assert_eq!(domain_conn_hint(&stale_url), None);
        let mixed_url = format!("http://{mixed_host}/f");
        assert_eq!(domain_conn_cap(&mixed_url), None, "过期 cap 面应被独立丢弃");
        assert_eq!(
            domain_conn_hint(&mixed_url),
            Some(16),
            "hint 面新鲜时应独立于过期 cap 面存活"
        );

        // 清理
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(fresh_host);
            cache.remove(stale_host);
            cache.remove(mixed_host);
        }
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
    }

    #[tokio::test]
    async fn legacy_v2_single_connection_cap_is_ignored_on_engine_start() {
        use super::{CONN_CAP_CONFIG_KEY, domain_conn_cap, load_domain_conn_caps, now_unix_secs};

        let dir = std::env::temp_dir().join(format!("fluxdown_connv2_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db = crate::db::Db::open(&dir).await.expect("open db");
        let host = format!("legacy-v2-{}.example.com", uuid::Uuid::new_v4());
        let url = format!("https://{host}/signed?token=expired");
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(&host);
        }

        db.set_config(
            CONN_CAP_CONFIG_KEY,
            &format!("v2\n{host}\t1\t{}\t0\t0", now_unix_secs()),
        )
        .await
        .expect("seed legacy cap");

        load_domain_conn_caps(&db).await;
        assert_eq!(
            domain_conn_cap(&url),
            None,
            "旧实现由失效签名 URL 写出的 24h 单连接限制不得继续生效"
        );

        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(&host);
        }
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
    }

    #[test]
    fn domain_conn_hint_take_higher_and_clamped_by_cap() {
        use super::domain_conn_cap;
        let url = "http://conn-hint-test.example.com/file";
        let domain = "conn-hint-test.example.com";
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(domain);
        }

        assert_eq!(domain_conn_hint(url), None, "记录前不应命中");

        // 取更高合并：8 → 4 不回落 → 16 覆盖
        record_domain_conn_hint(url, 8);
        assert_eq!(domain_conn_hint(url), Some(8));
        record_domain_conn_hint(url, 4);
        assert_eq!(
            domain_conn_hint(url),
            Some(8),
            "更低的 hint 不应覆盖更高观察"
        );
        record_domain_conn_hint(url, 16);
        assert_eq!(domain_conn_hint(url), Some(16));

        // 负面 cap 恒压制正面 hint（读取侧钳制，双面记录均保留）
        record_domain_conn_cap(url, 6);
        assert_eq!(domain_conn_hint(url), Some(6), "hint 必须被未过期 cap 钳制");
        assert_eq!(domain_conn_cap(url), Some(6));

        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(domain);
        }
    }

    #[test]
    fn conn_policy_faces_expire_independently() {
        use super::{ConnPolicyEntry, domain_conn_cap, now_unix_secs};
        let url = "http://conn-face-ttl-test.example.com/file";
        let domain = "conn-face-ttl-test.example.com";
        let now = now_unix_secs();
        if let Ok(mut cache) = conn_cap_cache().lock() {
            // cap 已过期（25h 前）、hint 新鲜
            cache.insert(
                domain.to_string(),
                ConnPolicyEntry {
                    cap: 2,
                    cap_ts: now - 25 * 3600,
                    hint: 8,
                    hint_ts: now,
                },
            );
        }
        assert_eq!(domain_conn_cap(url), None, "过期 cap 面不可读");
        assert_eq!(
            domain_conn_hint(url),
            Some(8),
            "新鲜 hint 面不受过期 cap 面牵连（且无钳制）"
        );

        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(domain);
        }
    }

    #[test]
    fn initial_allowed_workers_hint_semantics() {
        use super::RAMP_INITIAL_WORKERS;
        // 无提示 → 保守起步
        assert_eq!(initial_allowed_workers(16, None), RAMP_INITIAL_WORKERS);
        // 提示直接抬起点，但绝不越过 worker_cap
        assert_eq!(initial_allowed_workers(16, Some(16)), 16);
        assert_eq!(initial_allowed_workers(8, Some(16)), 8);
        // 不高于默认起步的提示不生效（无信息量）
        assert_eq!(initial_allowed_workers(16, Some(1)), RAMP_INITIAL_WORKERS);
        assert_eq!(initial_allowed_workers(16, Some(2)), RAMP_INITIAL_WORKERS);
        // 单连接 cap 恒压制
        assert_eq!(initial_allowed_workers(1, Some(16)), 1);
    }

    // -----------------------------------------------------------------------
    // FileSyncGate（fdatasync 合并闸）
    // -----------------------------------------------------------------------

    /// 在系统临时目录创建一个内容非空、可读写的临时文件，返回其路径与已打开的
    /// `tokio::fs::File` 句柄。调用方负责在测试结束时
    /// 关闭文件句柄后显式处理临时文件清理错误。
    async fn open_sync_gate_test_file() -> (std::path::PathBuf, tokio::fs::File) {
        let path = std::env::temp_dir().join(format!("fdgate-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"fluxdown-file-sync-gate-test").expect("write temp file content");
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .await
            .expect("open temp file for FileSyncGate test");
        (path, file)
    }

    fn std_handle(path: &std::path::Path) -> Arc<std::fs::File> {
        Arc::new(
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .expect("open std handle for FileSyncGate test"),
        )
    }

    /// 等待一次与 `prev` 不同的后台 fsync 完成，返回其起始时刻（5s 超时即失败）。
    async fn wait_background_sync(gate: &FileSyncGate, prev: Option<Instant>) -> Instant {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(start) = gate.last_completed_start()
                && Some(start) != prev
            {
                return start;
            }
            assert!(Instant::now() < deadline, "后台 fsync 未在 5s 内完成");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    // 合并闸的核心契约：MIN_SYNC_GAP 窗口内的重复 kick 必须是 no-op，绝不重复触发
    // 整盘 fdatasync——这正是 BUG-COORD-FSYNC 想避免的「N 段各自整盘刷写」退化。
    #[tokio::test]
    async fn kick_within_gap_is_noop() {
        let (path, file) = open_sync_gate_test_file().await;
        let handle = std_handle(&path);
        let gate = FileSyncGate::new();

        gate.kick_background(&handle);
        let s1 = wait_background_sync(&gate, None).await;
        gate.kick_background(&handle);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            gate.last_completed_start(),
            Some(s1),
            "MIN_SYNC_GAP 内的第二次 kick 不得发起新的 fdatasync"
        );

        drop((file, handle));
        std::fs::remove_file(&path).expect("test cleanup succeeds");
    }

    // gap 之外必须真正触发新一轮 fsync：防「数据长期得不到刷新」回归。用真实
    // sleep：sync_data 落在真实阻塞线程池，与虚拟时钟交互不可靠。
    #[tokio::test]
    async fn kick_after_gap_syncs_again() {
        let (path, file) = open_sync_gate_test_file().await;
        let handle = std_handle(&path);
        let gate = FileSyncGate::new();

        gate.kick_background(&handle);
        let s1 = wait_background_sync(&gate, None).await;
        tokio::time::sleep(MIN_SYNC_GAP + Duration::from_millis(100)).await;
        gate.kick_background(&handle);
        let s2 = wait_background_sync(&gate, Some(s1)).await;
        assert!(
            s2 > s1,
            "超过 MIN_SYNC_GAP 后必须发起一次起始更晚的新 fdatasync"
        );

        drop((file, handle));
        std::fs::remove_file(&path).expect("test cleanup succeeds");
    }

    // 并发突发：16 个 worker 同时 kick 只跑一次 fsync；随后的覆盖式等待复用这次
    // 起始晚于其快照的后台 fsync，不再自刷、也不死锁（notify TOCTOU 回归）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_kicks_coalesce_and_cover_waiters() {
        let (path, file) = open_sync_gate_test_file().await;
        let handle = std_handle(&path);
        let gate = FileSyncGate::new();
        let snap_t = Instant::now();

        let mut kicks = Vec::new();
        for _ in 0..16 {
            let gate = gate.clone();
            let handle = Arc::clone(&handle);
            kicks.push(tokio::spawn(async move { gate.kick_background(&handle) }));
        }
        for kick in kicks {
            kick.await.expect("kick task must not panic");
        }
        let background = wait_background_sync(&gate, None).await;
        let covering = gate
            .sync_covering(&file, snap_t)
            .await
            .expect("sync_covering");
        assert_eq!(
            covering, background,
            "后台 fsync 起始晚于快照时，覆盖式等待必须直接复用它"
        );

        drop((file, handle));
        std::fs::remove_file(&path).expect("test cleanup succeeds");
    }

    // -----------------------------------------------------------------------
    // FileSyncGate::sync_covering（覆盖式 fdatasync）
    // -----------------------------------------------------------------------

    /// 返回值恒 ≥ snap_t：多次调用、穿插写入后仍成立。
    #[tokio::test]
    async fn sync_covering_returns_start_ge_snap_t() {
        use tokio::io::AsyncWriteExt;
        let (path, mut file) = open_sync_gate_test_file().await;
        let gate = FileSyncGate::new();

        for i in 0..3u8 {
            file.write_all(&[i]).await.expect("write");
            file.flush().await.expect("flush");
            let snap_t = std::time::Instant::now();
            let s = gate
                .sync_covering(&file, snap_t)
                .await
                .expect("sync_covering");
            assert!(
                s >= snap_t,
                "覆盖式 fsync 起始时刻必须 ≥ snap_t；s={s:?} snap_t={snap_t:?}"
            );
        }

        drop(file);
        std::fs::remove_file(&path).expect("test cleanup succeeds");
    }

    /// 新鲜覆盖复用：T0 sync 后以 snap_t < T0 调用返回同一 T0。
    #[tokio::test]
    async fn sync_covering_reuses_completed_start_when_fresh() {
        let (path, file) = open_sync_gate_test_file().await;
        let gate = FileSyncGate::new();

        let before = std::time::Instant::now();
        let t0 = gate
            .sync_covering(&file, before)
            .await
            .expect("first covering sync");

        // snap_t 取在 T0 之前 → 已有覆盖，必须复用 T0，不触发新 fsync。
        let s = gate
            .sync_covering(&file, before)
            .await
            .expect("reuse covering sync");
        assert_eq!(
            s, t0,
            "snap_t ≤ last_completed_start 时必须复用同一起始时刻"
        );

        drop(file);
        std::fs::remove_file(&path).expect("test cleanup succeeds");
    }

    /// 过期强制：snap_t > last S 时必产生新 S ≥ snap_t（不受 MIN_SYNC_GAP 阻挡）。
    #[tokio::test]
    async fn sync_covering_forces_new_sync_ignoring_min_gap() {
        let (path, file) = open_sync_gate_test_file().await;
        let gate = FileSyncGate::new();

        let before = std::time::Instant::now();
        let s1 = gate
            .sync_covering(&file, before)
            .await
            .expect("first covering sync");

        // 仍在 MIN_SYNC_GAP 内；sync_if_stale 会复用，但 covering 必须以更新的
        // snap_t 强制推进。
        let snap_t = std::time::Instant::now();
        assert!(snap_t > s1, "测试前提：snap_t 必须严格晚于 s1");
        let s2 = gate
            .sync_covering(&file, snap_t)
            .await
            .expect("forced covering sync");
        assert!(
            s2 >= snap_t && s2 > s1,
            "snap_t > last S 时必须无视 MIN_SYNC_GAP 产生新的覆盖 fsync；\
             s1={s1:?} s2={s2:?} snap_t={snap_t:?}"
        );

        drop(file);
        std::fs::remove_file(&path).expect("test cleanup succeeds");
    }

    // -----------------------------------------------------------------------
    // hint_uncap_ceiling（hint 解封天花板）
    // -----------------------------------------------------------------------

    #[test]
    fn hint_uncap_ceiling_table() {
        // allow=false → base
        assert_eq!(hint_uncap_ceiling(false, None, Some(16), 16), 16);
        // 负面 cap 存在 → base（含单连接）
        assert_eq!(hint_uncap_ceiling(true, Some(1), Some(16), 16), 16);
        assert_eq!(hint_uncap_ceiling(true, Some(8), Some(16), 16), 16);
        // hint None → base
        assert_eq!(hint_uncap_ceiling(true, None, None, 16), 16);
        // hint=16, base=16 → 32
        assert_eq!(hint_uncap_ceiling(true, None, Some(16), 16), 32);
        // hint=40, base=16 → ×2=80 被 HINT_UNCAP_MAX 钳
        assert_eq!(
            hint_uncap_ceiling(true, None, Some(40), 16),
            HINT_UNCAP_MAX.min(MAX_SEGMENTS as usize)
        );
        // hint=4, base=16 → ×2=8 < base → 保持 16
        assert_eq!(hint_uncap_ceiling(true, None, Some(4), 16), 16);
    }

    #[test]
    fn in_task_uncap_only_on_near_linear_gain_at_cap() {
        use super::in_task_uncap_ceiling;
        const MB: f64 = 1024.0 * 1024.0;
        let plenty = 1 << 40;
        // 8 → 16 连接吞吐 16 → 31 MB/s（单连接限速）：天花板 16 → 32。
        assert_eq!(
            in_task_uncap_ceiling(true, None, 16, 8, 16, 16.0 * MB, 31.0 * MB, plenty),
            32.min(HINT_UNCAP_MAX)
        );
        // 带宽已饱和：翻倍连接只多 20% → 不解封。
        assert_eq!(
            in_task_uncap_ceiling(true, None, 16, 8, 16, 100.0 * MB, 120.0 * MB, plenty),
            16
        );
        // 还没到天花板 / 用户显式上限 / 负面记录 / 剩余字节喂不饱：都不动。
        assert_eq!(
            in_task_uncap_ceiling(true, None, 16, 4, 8, 8.0 * MB, 16.0 * MB, plenty),
            16
        );
        assert_eq!(
            in_task_uncap_ceiling(false, None, 16, 8, 16, 16.0 * MB, 32.0 * MB, plenty),
            16
        );
        assert_eq!(
            in_task_uncap_ceiling(true, Some(16), 16, 8, 16, 16.0 * MB, 32.0 * MB, plenty),
            16
        );
        assert_eq!(
            in_task_uncap_ceiling(true, None, 16, 8, 16, 16.0 * MB, 32.0 * MB, 32 << 20),
            16
        );
        // 已在平台上限：不再上抬。
        assert_eq!(
            in_task_uncap_ceiling(
                true,
                None,
                HINT_UNCAP_MAX,
                HINT_UNCAP_MAX / 2,
                HINT_UNCAP_MAX,
                16.0 * MB,
                32.0 * MB,
                plenty
            ),
            HINT_UNCAP_MAX
        );
    }

    // -----------------------------------------------------------------------
    // 软冻结试探就绪 / 转移（结构上无域名缓存写入分支）
    // -----------------------------------------------------------------------

    #[test]
    fn soft_probe_ready_guards() {
        let soft = FreezeState::Soft {
            ticks_waited: RAMP_SOFT_PROBE_COOLDOWN_TICKS,
            cooldown: RAMP_SOFT_PROBE_COOLDOWN_TICKS,
            attempts: 0,
        };
        // 健康路径就绪
        assert!(soft_probe_ready(
            soft, false, true, false, false, 0, 8, 8, 16, 0
        ));
        // tail 短路
        assert!(!soft_probe_ready(
            soft, true, true, false, false, 0, 8, 8, 16, 0
        ));
        // Active 不试探
        assert!(!soft_probe_ready(
            FreezeState::Active,
            false,
            true,
            false,
            false,
            0,
            8,
            8,
            16,
            0
        ));
        // Hard 不试探
        assert!(!soft_probe_ready(
            FreezeState::Hard,
            false,
            true,
            false,
            false,
            0,
            8,
            8,
            16,
            0
        ));
        // 冷却未到
        let cooling = FreezeState::Soft {
            ticks_waited: 3,
            cooldown: RAMP_SOFT_PROBE_COOLDOWN_TICKS,
            attempts: 0,
        };
        assert!(!soft_probe_ready(
            cooling, false, true, false, false, 0, 8, 8, 16, 0
        ));
        // 任务预算耗尽
        assert!(!soft_probe_ready(
            soft,
            false,
            true,
            false,
            false,
            0,
            8,
            8,
            16,
            RAMP_SOFT_PROBE_TASK_BUDGET
        ));
        // attempts 已达上限
        let exhausted = FreezeState::Soft {
            ticks_waited: RAMP_SOFT_PROBE_COOLDOWN_TICKS,
            cooldown: RAMP_SOFT_PROBE_COOLDOWN_TICKS,
            attempts: RAMP_SOFT_PROBE_MAX_ATTEMPTS,
        };
        assert!(!soft_probe_ready(
            exhausted, false, true, false, false, 0, 8, 8, 16, 0
        ));
    }

    /// 噪声谷：一次 Freeze → 冷却后 +1 Improved → 恢复 Active。
    /// 转移函数不含写缓存分支——试探失败零域名缓存写入由结构保证。
    #[test]
    fn soft_probe_noise_valley_recovers() {
        let (s1, rb1) =
            soft_probe_eval_transition(RampVerdict::Freeze, 0, RAMP_SOFT_PROBE_COOLDOWN_TICKS);
        assert!(rb1);
        assert_eq!(
            s1,
            FreezeState::Soft {
                ticks_waited: 0,
                cooldown: RAMP_SOFT_PROBE_COOLDOWN_TICKS * 2,
                attempts: 1,
            }
        );
        let (s2, rb2) = soft_probe_eval_transition(
            RampVerdict::Improved,
            1,
            RAMP_SOFT_PROBE_COOLDOWN_TICKS * 2,
        );
        assert!(!rb2);
        assert_eq!(s2, FreezeState::Active);
    }

    /// 持续劣化：两次试探 Freeze → Hard。
    #[test]
    fn soft_probe_two_failures_go_hard() {
        let (s1, _) =
            soft_probe_eval_transition(RampVerdict::Freeze, 0, RAMP_SOFT_PROBE_COOLDOWN_TICKS);
        match s1 {
            FreezeState::Soft {
                attempts, cooldown, ..
            } => {
                let (s2, rb2) = soft_probe_eval_transition(RampVerdict::Freeze, attempts, cooldown);
                assert!(rb2);
                assert_eq!(s2, FreezeState::Hard);
            }
            other => panic!("expected Soft after first freeze, got {other:?}"),
        }
    }

    /// 试探窗 Collapse → Hard（不写域名缓存，转移函数无副作用）。
    #[test]
    fn soft_probe_collapse_goes_hard_no_cache() {
        let (s, rb) =
            soft_probe_eval_transition(RampVerdict::Collapse, 0, RAMP_SOFT_PROBE_COOLDOWN_TICKS);
        assert!(rb);
        assert_eq!(s, FreezeState::Hard);
    }

    /// 任务软试探预算耗尽后 Freeze verdict 直接 Hard。
    #[test]
    fn freeze_verdict_budget_exhausted_hard() {
        assert_eq!(
            freeze_verdict_next_state(RAMP_SOFT_PROBE_TASK_BUDGET),
            FreezeState::Hard
        );
        assert_eq!(
            freeze_verdict_next_state(0),
            FreezeState::Soft {
                ticks_waited: 0,
                cooldown: RAMP_SOFT_PROBE_COOLDOWN_TICKS,
                attempts: 0,
            }
        );
    }
    /// Active 在持续劣化后必须先冷却，不能下一 tick 立即 32 → 64 抖回去。
    #[test]
    fn sustained_shrink_cools_active_before_reexpand() {
        let next = sustained_shrink_next_state(FreezeState::Active, 0);
        assert_eq!(
            next,
            FreezeState::Soft {
                ticks_waited: 0,
                cooldown: RAMP_SOFT_PROBE_COOLDOWN_TICKS,
                attempts: 0,
            }
        );
        assert!(!should_expand(
            !matches!(next, FreezeState::Active),
            false,
            32,
            32,
            64,
            false
        ));
    }

    /// 冷却期间继续劣化或任务试探预算耗尽时永久停扩，避免连接风暴反复出现。
    #[test]
    fn sustained_shrink_escalates_unhealthy_or_exhausted_state() {
        let soft = FreezeState::Soft {
            ticks_waited: 0,
            cooldown: RAMP_SOFT_PROBE_COOLDOWN_TICKS,
            attempts: 0,
        };
        assert_eq!(sustained_shrink_next_state(soft, 0), FreezeState::Hard);
        assert_eq!(
            sustained_shrink_next_state(FreezeState::Active, RAMP_SOFT_PROBE_TASK_BUDGET),
            FreezeState::Hard
        );
    }

    // -----------------------------------------------------------------------
    // beneficial_hint_scale
    // -----------------------------------------------------------------------

    #[test]
    fn beneficial_hint_scale_min_convergence() {
        assert_eq!(beneficial_hint_scale(16, 8), 8);
        assert_eq!(beneficial_hint_scale(8, 16), 8);
        assert_eq!(beneficial_hint_scale(12, 12), 12);
        assert_eq!(beneficial_hint_scale(0, 8), 0);
    }
}
