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
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::downloader::{DownloadError, ProgressUpdate, SegmentProgressInfo, is_server_rejection};
use crate::events::{EngineEvent, EventSink};
use crate::logger::log_info;
use crate::speed_limiter::SpeedLimiter;

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
// 域名连接上限策略缓存
// ---------------------------------------------------------------------------
// 当 coordinator 检测到某域名的服务器拒绝多连接（403/429），把【削减后的
// 连接上限】记入进程级缓存（1 = 单连接）。后续对同域名的下载任务以该值
// 裁剪 worker 上限，避免重蹈覆辙；重复记录取更低值（更保守的观察优先）。
// 缓存带 24h TTL——服务器策略可能变化，过期后重新尝试多连接。
//
// 持久化：缓存经 config 表（key = `domain_conn_caps`）跨重启保留——
// Engine 启动时 [`load_domain_conn_caps`] 读回，记录点经
// [`record_domain_conn_cap_persist`] 异步落盘。时间戳用 Unix 秒（墙钟），
// 使 TTL 判定跨进程生命周期依然成立。

/// TTL: 24 小时后允许重新尝试多连接。
const CONN_CAP_TTL: Duration = Duration::from_secs(24 * 3600);

/// 持久化缓存的 config 表 key。
const CONN_CAP_CONFIG_KEY: &str = "domain_conn_caps";

/// 进程级的域名 → (连接上限, 记录时间的 Unix 秒) 缓存。
static DOMAIN_CONN_CAPS: OnceLock<StdMutex<HashMap<String, (i32, u64)>>> = OnceLock::new();

fn conn_cap_cache() -> &'static StdMutex<HashMap<String, (i32, u64)>> {
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

/// 提取 URL 的 host 部分（含端口），用于域名级缓存的 key。
fn extract_host(url: &str) -> Option<String> {
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
        let effective = cache
            .get(&host)
            .filter(|(_, recorded)| conn_cap_fresh(*recorded, now))
            .map_or(cap, |(prev, _)| (*prev).min(cap));
        log_info!(
            "[conn-policy] 记录域名 {} 连接上限 {}，24h 内新任务按此上限调度",
            host,
            effective
        );
        cache.insert(host, (effective, now));
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

/// 查询某域名学习到的连接上限（未记录或已过期 → None）。
pub(crate) fn domain_conn_cap(url: &str) -> Option<i32> {
    if let Some(host) = extract_host(url)
        && let Ok(mut cache) = conn_cap_cache().lock()
        && let Some((cap, recorded)) = cache.get(&host)
    {
        if conn_cap_fresh(*recorded, now_unix_secs()) {
            return Some(*cap);
        }
        // 过期，移除
        cache.remove(&host);
    }
    None
}

/// 检查某域名是否被限制为单连接（且未过期）。
pub(crate) fn is_single_conn_domain(url: &str) -> bool {
    domain_conn_cap(url) == Some(1)
}

/// 持久化格式版本。语义规则变化（字段含义、TTL 判定、合并策略）时递增——
/// 旧版本数据在加载时整体丢弃并重新学习，绝不跨版本猜测字段含义。
/// 与 aria2 `server-stat-timeout` 的思路一致：学习数据是可再生的性能缓存，
/// 失效的正确处置是丢弃重学，而非迁移。
const CONN_CAP_FORMAT_VERSION: &str = "v1";

/// 序列化缓存为 config 存储格式：首行版本标记，之后每行
/// `host<TAB>cap<TAB>unix_secs`。host 不可能含 TAB/换行（URL host 语法
/// 排除空白），无需转义。
fn serialize_conn_caps(map: &HashMap<String, (i32, u64)>) -> String {
    let mut lines: Vec<String> = map
        .iter()
        .map(|(host, (cap, ts))| format!("{host}\t{cap}\t{ts}"))
        .collect();
    lines.sort(); // 确定性输出，便于测试与 diff
    let mut out = String::from(CONN_CAP_FORMAT_VERSION);
    for line in lines {
        out.push('\n');
        out.push_str(&line);
    }
    out
}

/// 解析 config 存储格式。版本标记不匹配 → 返回空表（整体重学）；
/// 版本内的畸形行静默跳过（防御手改/半写入）。
fn parse_conn_caps(raw: &str) -> HashMap<String, (i32, u64)> {
    let mut map = HashMap::new();
    let mut lines = raw.lines();
    if lines.next().map(str::trim) != Some(CONN_CAP_FORMAT_VERSION) {
        return map;
    }
    for line in lines {
        let mut parts = line.split('\t');
        let (Some(host), Some(cap), Some(ts)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(cap), Ok(ts)) = (cap.parse::<i32>(), ts.parse::<u64>()) else {
            continue;
        };
        if !host.is_empty() && cap >= 1 {
            map.insert(host.to_string(), (cap, ts));
        }
    }
    map
}

/// Engine 启动时从 config 表读回持久化的域名连接上限（过期条目丢弃；与
/// 内存中已有条目取更低值合并，语义与 [`record_domain_conn_cap`] 一致）。
pub(crate) async fn load_domain_conn_caps(db: &Db) {
    let raw = match db.get_config(CONN_CAP_CONFIG_KEY).await {
        Ok(Some(v)) => v,
        Ok(None) => return,
        Err(e) => {
            log_info!("[conn-policy] 读取持久化域名连接上限失败（忽略）: {}", e);
            return;
        }
    };
    let now = now_unix_secs();
    let loaded = parse_conn_caps(&raw);
    if let Ok(mut cache) = conn_cap_cache().lock() {
        let mut restored = 0usize;
        for (host, (cap, ts)) in loaded {
            if !conn_cap_fresh(ts, now) {
                continue;
            }
            let entry = cache.entry(host).or_insert((cap, ts));
            if cap < entry.0 {
                *entry = (cap, ts);
            }
            restored += 1;
        }
        if restored > 0 {
            log_info!("[conn-policy] 已恢复 {} 条持久化域名连接上限", restored);
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
        cache.retain(|_, (_, ts)| conn_cap_fresh(*ts, now));
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

/// Maximum total number of segments (including dynamically created ones).
const MAX_SEGMENTS: i32 = 64;

// ---------------------------------------------------------------------------
// 渐进启动 / 自适应连接调度
// ---------------------------------------------------------------------------
// `initial_segment_count` 的语义是【最大连接数上限】而非启动并发：coordinator
// 从 RAMP_INITIAL_WORKERS 条连接起步，每个评估窗口测一次总吞吐，健康（稳定
// 206、无 403/429、吞吐仍有增益）则倍增额度直至上限；扩容后吞吐无增益则冻结
// 在当前规模（找到服务器/链路的实际并发甜点）。分段规划不变——连接数 =
// 存活 worker 数，与预切分段数解耦（段只是 worker 的工作队列）。

/// 启动时的初始并发连接数（conservative ramp-up 起点）。
const RAMP_INITIAL_WORKERS: usize = 2;

/// ramp 评估窗口间隔（秒）：每窗口测一次总吞吐，决定扩容/冻结。
const RAMP_TICK_SECS: u64 = 2;

/// 扩容有效判据：扩容后窗口吞吐 >= 扩容前吞吐 × 此系数才继续扩容，
/// 否则冻结（新增连接没有带来净收益，继续加只会触发服务器风控）。
const RAMP_IMPROVE_FACTOR: f64 = 1.05;

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

/// Progress report interval to Dart UI.
const UI_REPORT_INTERVAL_MS: u128 = 200;

/// Retry constants for segment downloads.
///
/// 大文件下载（>1GB）最多 32 个分段并发，每个分段独立受 stall 检测。
/// 网络抖动时任何一个分段重试耗尽都会导致整个任务失败。
/// 5 次重试（含指数退避：2s/4s/8s/16s）给予充足的恢复窗口，
/// 总容忍时间从 ~36s 提升到 ~80s，大幅降低大文件下载因瞬时网络问题而中断的概率。
const MAX_RETRIES: u32 = 5;
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

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
/// [`FileSyncGate::sync_if_stale`] 返回一次【已完成】fdatasync 的**起始时刻** `S`。
/// fdatasync 保证刷入所有在其【起始】前发起的写入，故凡在 `S` 之前完成的写入
/// （其字节此刻已在 OS 页缓存）此刻均已持久化到磁盘。调用方须先 `file.flush()`
/// 把自己的 BufWriter 落入页缓存、记下快照时刻 `snap_t`，**仅当** `S >= snap_t`
/// 时才把该快照偏移写入 DB，从而维持 "DB 偏移 <= 已持久化字节" 不变式
/// （BUG-COORD-FSYNC）。因判据用 fsync 的【起始】而非完成时刻，即便复用他段触发
/// 的 fsync，也绝不会信任一次早于自身 flush 的 fsync（Windows 共享文件缓存同理）。
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
}

impl FileSyncGate {
    fn new() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(GateState {
                syncing: false,
                last_completed_start: None,
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    /// 合并式 fdatasync：若距上次【已完成】fsync 起始不足 [`MIN_SYNC_GAP`] 则跳过、
    /// 复用其结果；否则（且无并发 fsync 在跑时）由本调用执行一次整盘 fdatasync。
    /// 返回一次已完成 fsync 的**起始时刻**（见类型级正确性契约）。
    async fn sync_if_stale(&self, file: &tokio::fs::File) -> std::io::Result<Instant> {
        loop {
            // 决策阶段：持锁判断，绝不跨 .await 持锁。
            let do_sync = {
                let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                match st.last_completed_start {
                    // 距最近一次已完成 fsync 起始不足 MIN_SYNC_GAP → 复用，跳过本次。
                    Some(s) if s.elapsed() < MIN_SYNC_GAP => return Ok(s),
                    // 无新鲜 fsync，但已有一次在进行 → 等待其完成后重判。
                    _ if st.syncing => false,
                    // 无新鲜 fsync 且无在途 → 由本调用执行。
                    _ => {
                        st.syncing = true;
                        true
                    }
                }
            };

            if do_sync {
                // my_start 记于 fdatasync 之前：它是"覆盖判据"的时刻锚点。
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

            // 等待在途 fsync 完成后重判。带 50ms 兜底以规避 notify TOCTOU（与
            // speed_limiter 相同处理）：notify_waiters 只唤醒已注册者，若通知在
            // 注册前触发会丢失，超时确保下一轮必然重判，绝不永久阻塞。
            tokio::select! {
                biased;
                () = self.notify.notified() => {}
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }
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
}

/// Result of `find_next_work`: an assignment plus optionally the index of the
/// parent segment that was shrunk by a split (for targeted DB persistence).
struct NextWork {
    assignment: WorkerAssignment,
    /// If this work came from an in-half split, this is the index of the
    /// segment that was shrunk.  `None` when reusing an existing Pending segment.
    split_parent: Option<i32>,
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
    planned_total: Arc<AtomicI64>,
    size_is_estimate: bool,
    first_validators: Arc<StdMutex<Option<(String, String)>>>,
    client: Client,
    worker_cancel: CancellationToken,
    conn_sensitive: Arc<AtomicBool>,
    reconnect_hostile: Arc<AtomicBool>,
    total_downloaded: Arc<AtomicI64>,
    seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    db: Db,
    progress_tx: mpsc::Sender<ProgressUpdate>,
    speed_limiter: SpeedLimiter,
    spec: crate::downloader::RequestSpec,
    etag: String,
    last_modified: String,
    sync_gate: FileSyncGate,
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
            self.planned_total.clone(),
            self.size_is_estimate,
            self.first_validators.clone(),
            self.client.clone(),
            self.worker_cancel.clone(),
            self.conn_sensitive.clone(),
            self.reconnect_hostile.clone(),
            self.total_downloaded.clone(),
            self.seg_states.clone(),
            self.db.clone(),
            self.progress_tx.clone(),
            self.speed_limiter.clone(),
            self.spec.clone(),
            self.etag.clone(),
            self.last_modified.clone(),
            self.sync_gate.clone(),
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
    client: &Client,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    spec: &crate::downloader::RequestSpec,
    sink: &dyn EventSink,
    etag: &str,
    last_modified: &str,
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

    // ----- 1. Build initial segment map from DB or fresh calculation ---------
    let existing = db.load_segments(task_id).await?;
    let mut segments: BTreeMap<i32, LiveSegment> = BTreeMap::new();
    let mut next_index: i32;

    if existing.is_empty() {
        // Create fresh segments (uniform split).
        let (fresh, db_segs) = build_fresh_segments(initial_segment_count, total_bytes);
        segments = fresh;
        db.insert_segments(task_id, &db_segs).await?;
        next_index = initial_segment_count;
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
            let threshold: i64 = (db_total / 100).clamp(1, 1_048_576);
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
                let _ = db.update_task_total_bytes(task_id, db_total).await;
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
                let (fresh, db_segs) = build_fresh_segments(initial_segment_count, total_bytes);
                segments = fresh;
                db.insert_segments(task_id, &db_segs).await?;
                next_index = initial_segment_count;
                let _ = db.update_task_total_bytes(task_id, total_bytes).await;
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
        let (fresh, db_segs) = build_fresh_segments(initial_segment_count, total_bytes);
        segments = fresh;
        db.insert_segments(task_id, &db_segs).await?;
        next_index = initial_segment_count;
        // update_task_total_bytes may have set tasks.total_bytes to db_total earlier
        // (db_total <= total_bytes path).  After a fresh reset the canonical size is
        // total_bytes (from probe), so re-sync.
        let _ = db.update_task_total_bytes(task_id, total_bytes).await;
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
    {
        let db_downloaded: i64 = segments.values().map(|s| s.downloaded_bytes).sum();
        let file_len = match tokio::fs::metadata(dest).await {
            Ok(m) => m.len() as i64,
            Err(_) => 0,
        };
        // 与 effective_total_bytes 比较（而非更弱的 db_downloaded）：预分配后合规
        // 续传文件长度恒 >= effective_total_bytes，低于即为外部截断/删除。
        if db_downloaded > 0
            && (file_len == 0 || file_len < db_downloaded || file_len < effective_total_bytes)
        {
            log_info!(
                "[coordinator] task {} file integrity mismatch: file_len={}, db_downloaded={}, expected>={}. Resetting.",
                task_id,
                file_len,
                db_downloaded,
                effective_total_bytes
            );
            for seg in segments.values_mut() {
                seg.downloaded_bytes = 0;
                seg.state = SegState::Pending;
            }
            db.reset_segments_progress(task_id).await?;
        }
    }

    // ----- 2. Pre-allocate file to full size --------------------------------
    // 平台策略（fallocate / SetFileInformationByHandle / set_len 回退）见
    // preallocate_file_len——就地扩容（TrueSizeLarger）复用同一助手延长文件。
    preallocate_file_len(dest, effective_total_bytes as u64).await?;

    // ----- 3. Shared state for progress reporting ---------------------------
    let total_downloaded = Arc::new(AtomicI64::new(
        segments.values().map(|s| s.downloaded_bytes).sum::<i64>(),
    ));

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

    // 本任务的连接数硬上限：规划段数 ∧ MAX_SEGMENTS ∧ 域名学习缓存。
    let worker_cap = {
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
        cap
    };
    // 当前允许的活跃连接额度（ramp 控制变量，1..=worker_cap 内调整）。
    let mut allowed_workers = worker_cap.min(RAMP_INITIAL_WORKERS);
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

    // worker 生成上下文：启动与 ramp 扩容共用同一 spawn 路径。
    let ctx = WorkerSpawnCtx {
        event_tx,
        task_id: task_id.to_string(),
        url: url.to_string(),
        dest: dest.to_path_buf(),
        planned_total: planned_total.clone(),
        size_is_estimate,
        first_validators: first_validators.clone(),
        client: client.clone(),
        worker_cancel: worker_cancel.clone(),
        conn_sensitive: conn_sensitive.clone(),
        reconnect_hostile: reconnect_hostile.clone(),
        total_downloaded: total_downloaded.clone(),
        seg_states: seg_states.clone(),
        db: db.clone(),
        progress_tx: progress_tx.clone(),
        speed_limiter: speed_limiter.clone(),
        spec: spec.clone(),
        etag: etag.to_string(),
        last_modified: last_modified.to_string(),
        sync_gate: sync_gate.clone(),
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
            let _ = assign_tx.try_send(assignment);
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
        for handle in &mut worker_handles {
            if let Some(h) = handle.take() {
                let _ = h.await;
            }
        }
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

    // Proactive split timer: pre-create Pending segments so the next idle
    // worker can pick one up immediately without a split in the hot path.
    let mut proactive_interval =
        tokio::time::interval(Duration::from_secs(PROACTIVE_SPLIT_INTERVAL_SECS));
    proactive_interval.tick().await; // consume the immediate first tick

    // ---- 渐进/自适应连接调度状态 ----
    // ramp_frozen：停止继续扩容（扩容无吞吐增益，或出现过拒绝信号）。
    let mut ramp_frozen = false;
    // 分级降级计数：第 1 次拒绝 → 乘性减半（保留存活连接）；第 2 次 → 串行模式。
    let mut reject_strikes: u32 = 0;
    // 上个 ramp tick 刚扩容，本 tick 用窗口吞吐评估扩容效果。
    let mut awaiting_ramp_eval = false;
    let mut pre_grow_throughput = 0.0_f64;
    // ramp 独立采样窗口（与上方 min_split 采样解耦，互不污染窗口边界）。
    let mut ramp_last_bytes = total_downloaded.load(Ordering::Relaxed);
    let mut ramp_last_time = Instant::now();
    let mut ramp_interval = tokio::time::interval(Duration::from_secs(RAMP_TICK_SECS));
    ramp_interval.tick().await; // consume the immediate first tick

    loop {
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
                            persist_segment_change(
                                db, task_id, &segments, child_idx, Some(seg_index),
                            ).await;
                            send_split_event(
                                sink, task_id, seg_index, child_idx, &segments, false,
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
                                last_throughput_bytes = current_bytes;
                                last_throughput_time = now;
                            }
                        }

                        let next_work = if serial_mode {
                            let other_active = segments.values()
                                .any(|s| s.state == SegState::Active);
                            if other_active {
                                // 还有其他 worker 在下载 → 退休当前 worker，等它完成
                                None
                            } else {
                                // 无其他活跃连接 → 取一个 Pending 分段（不拆分）
                                find_next_pending_only(&mut segments)
                            }
                        } else if worker_assign_txs.iter().filter(|t| t.is_some()).count()
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
                            )
                        };

                        if let Some(next) = next_work {
                            let new_seg_idx = next.assignment.seg_index;

                            // Persist new/updated segments to DB.
                            persist_segment_change(
                                db, task_id, &segments,
                                new_seg_idx, next.split_parent,
                            ).await;

                            // Notify host about the split event (if this came from a split).
                            if let Some(parent_idx) = next.split_parent {
                                send_split_event(
                                    sink, task_id, parent_idx, new_seg_idx,
                                    &segments, false,
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
                                if let Err(e) =
                                    preallocate_file_len(dest, reported_total as u64).await
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
                                    },
                                );
                                effective_total_bytes = reported_total;
                                planned_total.store(reported_total, Ordering::Relaxed);
                                persist_segment_change(
                                    db, task_id, &segments, tail_idx, None,
                                ).await;
                                let _ = db
                                    .update_task_total_bytes(task_id, reported_total)
                                    .await;
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
                            let next_work = if serial_mode {
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
                                )
                            };
                            if let Some(next) = next_work {
                                let new_seg_idx = next.assignment.seg_index;
                                persist_segment_change(
                                    db, task_id, &segments,
                                    new_seg_idx, next.split_parent,
                                ).await;
                                if let Some(parent_idx) = next.split_parent {
                                    send_split_event(
                                        sink, task_id, parent_idx, new_seg_idx,
                                        &segments, false,
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
                                *slot = None;
                            }
                            continue;
                        }
                        // 失败处置分三类：
                        //   (1) 403/429 服务器拒绝多连接 + 有其它段在工作；
                        //   (2) 瞬时 RangeNotSupported——已下载过数据（any_data，证明
                        //       Range 工作过），本次却收到 200 全量响应（alist 代理迅雷/
                        //       光鸭云盘在连接压力下偶发）；
                        //   (3) 真·无 Range（从未拿到 206）或其它致命错误。
                        // (1)(2) 走串行降级：保数据、退休失败 worker、保活其它 worker、
                        // 不 cancel、不清文件；(3) 走 Path B：仅停 workers 并上报错误。
                        let is_range_err =
                            matches!(error, DownloadError::RangeNotSupported(_));
                        // 是否已有任意段真正拿到过 206 并写入数据（含 resume 起始进度）。
                        let any_data = total_downloaded.load(Ordering::Relaxed) > 0;
                        let other_working = segments.values().any(|s| {
                            s.index != seg_index
                                && matches!(s.state, SegState::Active | SegState::Completed)
                        });

                        // 瞬时 200：Range 工作过（any_data）却收到 RangeNotSupported。
                        // 绝不能当永久错误（取消任务 + 删数据 + 投毒主机）处理。
                        let transient_range = is_range_err && any_data;
                        let server_rejection = is_server_rejection(&error);
                        // 400 亦按连接级拒绝处理，但仅当 other_working（同 URL 其他
                        // 连接工作正常，证明请求本身合法）：配额型端点（如 fnOS
                        // multiple-download 的 token 请求次数配额）对超额连接回 400
                        // 而非 403/429。与 403/429 不同，400 语义宽泛，绝不写入
                        // 域名连接数缓存（下方两处 record 仍只看 server_rejection）。
                        let conn_rejection = server_rejection || is_http_400(&error);

                        if (conn_rejection && other_working) || transient_range {
                            // ---- 分级自适应降级 ----
                            // 第 1 次拒绝且存活连接充足：乘性减半连接额度，冻结扩容，
                            // 保留所有存活连接（超额部分完成当前段后自然退休）；
                            // 再次拒绝（或本就只有 <=2 条连接）才降到串行模式。
                            reject_strikes += 1;
                            ramp_frozen = true;
                            awaiting_ramp_eval = false;
                            let reason = if transient_range {
                                "transient-200"
                            } else {
                                "server-rejection"
                            };
                            let alive = worker_assign_txs
                                .iter()
                                .filter(|tx| tx.is_some())
                                .count();
                            if !serial_mode && reject_strikes == 1 && alive > 2 {
                                allowed_workers = (alive / 2).max(1);
                                log_info!(
                                    "[adaptive] task {} ramp down: {} -> {}, reason={}, \
                                     preserve_active=true",
                                    task_id,
                                    alive,
                                    allowed_workers,
                                    reason
                                );
                                // 仅 403/429（真·连接数限制）记录域名连接上限缓存。
                                // 瞬时 200 不记录（服务器明确支持 Range，见下方注释）。
                                if server_rejection {
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
                                // 仅 403/429（真·连接数限制）记录域名单连接缓存。瞬时 200
                                // 【绝不】记录——服务器明确支持 Range（已服务过半 206），
                                // 一次偶发 200 不应把整个主机打成单连接 24h、阻断续传与
                                // 多段吞吐（BUG-COORD-TRANSIENT-200-POISONS-HOST）。
                                if server_rejection {
                                    record_single_conn_domain_persist(url, db);
                                }
                            }

                            // 将失败分段标记为 Pending，等待串行下载
                            if let Some(seg) = segments.get_mut(&seg_index) {
                                seg.state = SegState::Pending;
                            }

                            // ---- 开放式首段吸收（连接配额自救）----
                            // 服务器拒绝新连接，但开放式首段的响应流仍活着：把它
                            // 右侧【字节连续且零进度】的 Pending 段全部并入该段。
                            // 开放式请求（bytes=X-）的流本就覆盖到文件末尾，worker
                            // 写循环每 chunk 重读共享 end_byte，预算扩大后自然续写
                            // ——无需任何新请求即可下完整个文件。这是"一个 token
                            // 只允许固定次数成功 GET"的配额型端点（fnOS
                            // multiple-download）唯一能下完的方式。
                            if let Some(open_idx) = open_ended_streaming
                                && let Some(open_end) = segments
                                    .get(&open_idx)
                                    .filter(|s| s.state == SegState::Active)
                                    .map(|s| s.end_byte)
                            {
                                let mut new_end = open_end;
                                let mut absorbed: Vec<i32> = Vec::new();
                                while let Some((idx, end)) = segments
                                    .values()
                                    .find(|s| {
                                        s.state == SegState::Pending
                                            && s.downloaded_bytes == 0
                                            && s.start_byte == new_end + 1
                                    })
                                    .map(|s| (s.index, s.end_byte))
                                {
                                    new_end = end;
                                    absorbed.push(idx);
                                }
                                if !absorbed.is_empty() {
                                    // DB 先行（单事务）：失败则放弃合并（内存不动，
                                    // 降级照常进行），避免内存/DB 段布局分叉。
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
                                            rebuild_seg_states(&segments, &seg_states);
                                            // 吸收合并 = 配额型端点实锤：这条
                                            // 流从此是唯一生命线，重连必被拒。
                                            // 提高其停滞容忍，禁止轻易掐流。
                                            reconnect_hostile
                                                .store(true, Ordering::Relaxed);
                                            log_info!(
                                                "[coordinator] task {} 开放式首段 #{} 吸收 {} 个 \
                                                 Pending 段（新终点 {}），复用现有连接续流到文件尾",
                                                task_id,
                                                open_idx,
                                                absorbed.len(),
                                                new_end
                                            );
                                        }
                                        Err(e) => {
                                            log_info!(
                                                "[coordinator] task {} persist_merge 失败：{}，\
                                                 放弃合并走常规降级",
                                                task_id,
                                                e
                                            );
                                        }
                                    }
                                }
                            }

                            // 退休当前失败的 worker（关闭其分配通道）
                            if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                                *slot = None;
                            }

                            // 安全检查：如果所有 worker 都已退休且无活跃分段，
                            // 说明服务器甚至拒绝单连接 → 无法继续。
                            let any_alive = worker_assign_txs.iter().any(|tx| tx.is_some())
                                || segments.values().any(|s| s.state == SegState::Active);
                            if !any_alive && !all_done(&segments) {
                                final_error = Some(DownloadError::Other(format!(
                                    "服务器拒绝所有下载连接（包括单连接），无法继续下载：{error}"
                                )));
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
                                final_error = Some(error);
                            }
                            break;
                        }
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
                if !serial_mode && !conn_sensitive.load(Ordering::Relaxed) && !all_done(&segments) {
                    sync_downloaded_from_shared(&mut segments, &seg_states);
                    // Try proactive split at the normal threshold first; if that fails
                    // (last segment has < current_min_split but >= TAIL_MIN_SPLIT_BYTES
                    // remaining), also try the tail micro threshold so the proactive
                    // timer covers the full range from current_min_split down to 64 KB.
                    let work = try_proactive_split(
                        &mut segments,
                        &mut next_index,
                        current_min_split,
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
                        persist_segment_change(
                            db, task_id, &segments,
                            new_seg_idx, next.split_parent,
                        ).await;
                        if let Some(parent_idx) = next.split_parent {
                            send_split_event(
                                sink, task_id, parent_idx, new_seg_idx,
                                &segments, true,
                            );
                        }
                        rebuild_seg_states(&segments, &seg_states);
                    }
                }
            }

            // --- Adaptive ramp timer -------------------------------------
            // 渐进启动 + 自适应扩容：每个窗口测一次总吞吐；上次扩容有净收益且
            // 无拒绝/连接敏感信号时倍增连接额度，否则冻结在当前规模；随后按
            // 额度补充 worker（优先领 Pending 段，否则拆分最大 Active 段）。
            _ = ramp_interval.tick() => {
                // 1. 测量本窗口总吞吐（独立采样窗口，与 min_split 采样解耦）。
                let now = Instant::now();
                let bytes = total_downloaded.load(Ordering::Relaxed);
                let elapsed = now.duration_since(ramp_last_time).as_secs_f64();
                let throughput = if elapsed > 0.0 {
                    (bytes - ramp_last_bytes).max(0) as f64 / elapsed
                } else {
                    0.0
                };
                ramp_last_bytes = bytes;
                ramp_last_time = now;

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

                if !serial_mode && !all_done(&segments) {
                    // 2. 评估上一次扩容的效果：吞吐无足够增益 → 冻结在当前规模
                    //   （已找到服务器/链路的实际并发甜点，继续加只有风控风险）。
                    if awaiting_ramp_eval {
                        awaiting_ramp_eval = false;
                        // `<=` 而非 `<`：扩容前后吞吐同为 0（服务器停滞）也判
                        // 定为无增益并冻结，避免对停滞服务器反而打满并发。
                        if throughput <= pre_grow_throughput * RAMP_IMPROVE_FACTOR {
                            ramp_frozen = true;
                            log_info!(
                                "[adaptive] task {} ramp freeze at {} conns: throughput \
                                 {:.0} -> {:.0} B/s (no gain)",
                                task_id,
                                allowed_workers,
                                pre_grow_throughput,
                                throughput
                            );
                        }
                    }

                    // 3. 扩容决策：未冻结、无连接敏感信号、当前额度已用满、未达上限。
                    if !ramp_frozen
                        && !conn_sensitive.load(Ordering::Relaxed)
                        && alive >= allowed_workers
                        && allowed_workers < worker_cap
                    {
                        pre_grow_throughput = throughput;
                        awaiting_ramp_eval = true;
                        let old = allowed_workers;
                        allowed_workers = (allowed_workers * 2).min(worker_cap);
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

                    // 4. 按额度补充 worker：与 Done 派工同一逻辑（Pending 优先，
                    //    否则拆分最大 Active 段），启动与扩容共用 ctx.spawn 路径。
                    while alive < allowed_workers {
                        sync_downloaded_from_shared(&mut segments, &seg_states);
                        let Some(next) = find_next_work(
                            &mut segments,
                            &mut next_index,
                            effective_total_bytes,
                            current_min_split,
                        ) else {
                            break;
                        };
                        let new_seg_idx = next.assignment.seg_index;
                        persist_segment_change(
                            db, task_id, &segments,
                            new_seg_idx, next.split_parent,
                        ).await;
                        if let Some(parent_idx) = next.split_parent {
                            send_split_event(
                                sink, task_id, parent_idx, new_seg_idx,
                                &segments, false,
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
                }

                // 5. 存活性兜底：ctx 持有 event_tx，worker 全部退出（如 panic）
                //    不再触发 channel 关闭。若无存活 worker、任务未完成且上面的
                //    补充循环也无法开工，退出事件循环——交由第 8 步完整性检查
                //    报错，与旧实现 channel-close 路径语义等价。
                if alive == 0 && !all_done(&segments) {
                    log_info!(
                        "[coordinator] task {} 所有 worker 已退出但任务未完成，退出事件循环",
                        task_id
                    );
                    break;
                }
            }

        }
    }

    // ----- 7. Wait for all worker tasks to finish ---------------------------
    for handle in &mut worker_handles {
        if let Some(h) = handle.take() {
            let _ = h.await;
        }
    }

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

    // Flush the authoritative in-memory downloaded_bytes (already capped to
    // segment size) back to the DB in a single transaction.  This is the
    // canonical final state: any overshoot from the split race is corrected
    // here, ensuring run_download_inner's integrity check sees correct totals.
    let flush_updates: Vec<(i32, i64)> = segments
        .values()
        .map(|s| (s.index, s.downloaded_bytes))
        .collect();
    if let Err(e) = db.flush_segments_progress(task_id, flush_updates).await {
        log_info!(
            "[coordinator] task {} final flush failed (non-fatal): {}",
            task_id,
            e
        );
    }

    Ok(effective_total_bytes)
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
/// - Windows: SetFileInformationByHandle(FileAllocationInfo) 预分配 NTFS 物理簇
///   （连续优先），提前检测磁盘空间不足；再 SetEndOfFile 设置逻辑大小。
/// - 其它:    回退 set_len()。
async fn preallocate_file_len(dest: &Path, target_len: u64) -> Result<(), DownloadError> {
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
            use std::os::unix::io::AsRawFd;
            let fd = std_file.as_raw_fd();
            // fallocate(fd, 0, 0, len): 预分配 [0, len) 范围的磁盘块，
            // 不写零，ext4/XFS/Btrfs 均支持，耗时 O(1)。
            // mode=0 同时将文件大小设为 max(当前大小, offset+len)。
            let ret = unsafe { libc::fallocate(fd, 0, 0, target_len as libc::off_t) };
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
            use std::os::windows::io::AsRawHandle;
            // FILE_ALLOCATION_INFO: 单字段 AllocationSize (LARGE_INTEGER = i64)
            #[repr(C)]
            struct FileAllocInfo {
                allocation_size: i64,
            }
            let handle = std_file.as_raw_handle();
            // Step 1: 预分配 NTFS 物理簇——立即保留磁盘空间（连续簇优先），
            // 磁盘不足时提前报错（等效 Linux fallocate 的 ENOSPC 检测），
            // 减少多段随机写时的 NTFS 碎片化。
            let info = FileAllocInfo {
                allocation_size: target_len as i64,
            };
            let ret = unsafe {
                windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle(
                    handle,
                    windows_sys::Win32::Storage::FileSystem::FileAllocationInfo,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<FileAllocInfo>() as u32,
                )
            };
            if ret == 0 {
                // FAT32/exFAT/网络驱动器等不支持时仅记录日志，不中断
                log_info!(
                    "[coordinator] SetFileInformationByHandle(FileAllocationInfo) 失败: {}",
                    std::io::Error::last_os_error()
                );
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
        file.set_len(target_len).await?;
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
            },
        );
        db_segs.push((i, start, end));
    }
    (segments, db_segs)
}

// ---------------------------------------------------------------------------
// Segment coverage validation
// ---------------------------------------------------------------------------

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
) -> Option<NextWork> {
    // Strategy 1: existing Pending segment.
    if let Some(seg) = segments.values().find(|s| s.state == SegState::Pending) {
        let assignment = WorkerAssignment {
            seg_index: seg.index,
            seg_start: seg.start_byte,
            actual_start: seg.start_byte + seg.downloaded_bytes,
            seg_end: seg.end_byte,
            open_ended: false,
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
    if let Some(work) = try_split_largest(segments, next_index, min_split) {
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
            try_split_largest(segments, next_index, TAIL_MIN_SPLIT_BYTES)
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

/// IDM-style in-half division: find the active segment with the most remaining
/// bytes and split it at the midpoint of its remaining range.
///
/// Returns a `NextWork` for the **new** segment (upper half), including the
/// index of the parent segment that was shrunk, or `None` if no segment is
/// large enough to split.
fn try_split_largest(
    segments: &mut BTreeMap<i32, LiveSegment>,
    next_index: &mut i32,
    min_split: i64,
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

    // Find the active segment with the most remaining bytes.
    let best_idx = segments
        .values()
        .filter(|s| s.state == SegState::Active && s.remaining() >= min_split)
        .max_by_key(|s| s.remaining())
        .map(|s| s.index)?;

    let best = segments.get(&best_idx)?;

    // The current download position in the best segment.
    let current_pos = best.start_byte + best.downloaded_bytes;
    let remaining = best.end_byte - current_pos + 1;

    if remaining < min_split {
        return None;
    }

    // Split point = midpoint of the remaining range.
    let split_point = current_pos + remaining / 2;

    // Validate: split_point must be within (current_pos, end_byte].
    // This guarantees both halves are non-empty.
    if split_point <= current_pos || split_point > best.end_byte {
        return None;
    }

    let old_end = best.end_byte;

    // New segment covers [split_point, old_end].
    let new_index = *next_index;
    *next_index += 1;

    let new_seg = LiveSegment {
        index: new_index,
        start_byte: split_point,
        end_byte: old_end,
        downloaded_bytes: 0,
        state: SegState::Active,
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

/// Proactively split the largest active segment while other workers are still
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

    // Find the active segment with the most remaining bytes.
    let best_idx = segments
        .values()
        .filter(|s| s.state == SegState::Active && s.remaining() >= min_split)
        .max_by_key(|s| s.remaining())
        .map(|s| s.index)?;

    let best = segments.get(&best_idx)?;
    let current_pos = best.start_byte + best.downloaded_bytes;
    let remaining = best.end_byte - current_pos + 1;

    if remaining < min_split {
        return None;
    }

    let split_point = current_pos + remaining / 2;
    if split_point <= current_pos || split_point > best.end_byte {
        return None;
    }

    let old_end = best.end_byte;
    let new_index = *next_index;
    *next_index += 1;

    // New segment is Pending — a worker will pick it up when idle.
    let new_seg = LiveSegment {
        index: new_index,
        start_byte: split_point,
        end_byte: old_end,
        downloaded_bytes: 0,
        state: SegState::Pending,
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

fn all_done(segments: &BTreeMap<i32, LiveSegment>) -> bool {
    segments.values().all(|s| s.state == SegState::Completed)
}

// ---------------------------------------------------------------------------
// Helpers: shared state synchronization
// ---------------------------------------------------------------------------

/// Build a fresh `Vec<SegmentProgressInfo>` from the segment map.
fn build_seg_state_vec(segments: &BTreeMap<i32, LiveSegment>) -> Vec<SegmentProgressInfo> {
    segments
        .values()
        .map(|s| SegmentProgressInfo {
            index: s.index,
            start_byte: s.start_byte,
            end_byte: s.end_byte,
            downloaded_bytes: s.downloaded_bytes,
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
/// When no parent is given (simple re-assignment), only the child is upserted.
async fn persist_segment_change(
    db: &Db,
    task_id: &str,
    segments: &BTreeMap<i32, LiveSegment>,
    changed_index: i32,
    split_parent: Option<i32>,
) {
    let Some(seg) = segments.get(&changed_index) else {
        return;
    };

    if let Some(parent_idx) = split_parent {
        // Split scenario: atomic transaction for both child + parent.
        if let Some(parent) = segments.get(&parent_idx) {
            if let Err(e) = db
                .persist_split(
                    task_id,
                    seg.index,
                    seg.start_byte,
                    seg.end_byte,
                    seg.downloaded_bytes,
                    parent.index,
                    parent.end_byte,
                )
                .await
            {
                log_info!(
                    "[coordinator] persist_split failed: task={}, child={}, parent={}, err={}",
                    task_id,
                    seg.index,
                    parent.index,
                    e
                );
            }
        } else {
            // Parent not found in map — fall back to child-only upsert.
            if let Err(e) = db
                .upsert_segment(
                    task_id,
                    seg.index,
                    seg.start_byte,
                    seg.end_byte,
                    seg.downloaded_bytes,
                )
                .await
            {
                log_info!(
                    "[coordinator] upsert_segment failed: task={}, seg={}, err={}",
                    task_id,
                    seg.index,
                    e
                );
            }
        }
    } else {
        // No parent — simple upsert (e.g. reassigning a pending segment).
        if let Err(e) = db
            .upsert_segment(
                task_id,
                seg.index,
                seg.start_byte,
                seg.end_byte,
                seg.downloaded_bytes,
            )
            .await
        {
            log_info!(
                "[coordinator] upsert_segment failed: task={}, seg={}, err={}",
                task_id,
                seg.index,
                e
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: send split event to Dart
// ---------------------------------------------------------------------------

/// Emit an `EngineEvent::SegmentSplit` so the host can animate the split.
fn send_split_event(
    sink: &dyn EventSink,
    task_id: &str,
    parent_idx: i32,
    child_idx: i32,
    segments: &BTreeMap<i32, LiveSegment>,
    is_proactive: bool,
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
        parent_new_end: parent.end_byte,
        child_index: child_idx,
        child_start: child.start_byte,
        child_end: child.end_byte,
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
    planned_total: Arc<AtomicI64>,
    size_is_estimate: bool,
    first_validators: Arc<StdMutex<Option<(String, String)>>>,
    client: Client,
    cancel_token: CancellationToken,
    conn_sensitive: Arc<AtomicBool>,
    reconnect_hostile: Arc<AtomicBool>,
    total_downloaded: Arc<AtomicI64>,
    seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    db: Db,
    progress_tx: mpsc::Sender<ProgressUpdate>,
    speed_limiter: SpeedLimiter,
    spec: crate::downloader::RequestSpec,
    etag: String,
    last_modified: String,
    sync_gate: FileSyncGate,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Worker loop: keep accepting assignments until the channel closes.
        while let Some(assignment) = assign_rx.recv().await {
            if cancel_token.is_cancelled() {
                break;
            }

            let result = do_segment_with_retry(
                &task_id,
                assignment.seg_index,
                &url,
                &dest,
                assignment.seg_start,
                assignment.actual_start,
                assignment.seg_end,
                assignment.open_ended,
                &client,
                &cancel_token,
                &conn_sensitive,
                &reconnect_hostile,
                &total_downloaded,
                &planned_total,
                size_is_estimate,
                &first_validators,
                &db,
                &progress_tx,
                &seg_states,
                &speed_limiter,
                &spec,
                &etag,
                &last_modified,
                &sync_gate,
            )
            .await;

            match result {
                Ok(downloaded) => {
                    let _ = event_tx
                        .send(WorkerEvent::Done {
                            worker_id,
                            seg_index: assignment.seg_index,
                            downloaded_bytes: downloaded,
                        })
                        .await;
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
                    let recoverable = matches!(e, DownloadError::TrueSizeLarger(_));
                    let _ = event_tx
                        .send(WorkerEvent::Failed {
                            worker_id,
                            seg_index: assignment.seg_index,
                            error: e,
                        })
                        .await;
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

/// HTTP 400 Bad Request 判定。
///
/// 配额型下载端点（fnOS `multiple-download?token=`：一个 token 只允许固定次数
/// 成功 GET）对超额请求返回 400 而非 403/429。coordinator 仅在有其他连接正常
/// 工作（other_working，证明同一 URL 请求合法）时把 400 归入连接级拒绝走降级；
/// 且 400 绝不写入域名连接数缓存（语义宽泛，避免把坏 URL 学习成主机级降速）。
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
    client: &Client,
    cancel: &CancellationToken,
    conn_sensitive: &AtomicBool,
    reconnect_hostile: &AtomicBool,
    total_downloaded: &AtomicI64,
    planned_total: &AtomicI64,
    size_is_estimate: bool,
    first_validators: &StdMutex<Option<(String, String)>>,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    speed_limiter: &SpeedLimiter,
    spec: &crate::downloader::RequestSpec,
    expected_etag: &str,
    expected_last_modified: &str,
    sync_gate: &FileSyncGate,
) -> Result<i64, DownloadError> {
    let mut attempts = 0u32;

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
            client,
            cancel,
            conn_sensitive,
            reconnect_hostile,
            total_downloaded,
            planned_total,
            size_is_estimate,
            first_validators,
            db,
            progress_tx,
            seg_states,
            speed_limiter,
            spec,
            expected_etag,
            expected_last_modified,
            sync_gate,
        )
        .await
        {
            Ok(dl) => return Ok(dl),
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
            //   • total_downloaded==0：从未有任何段拿到过 206 → 服务器真的无视 Range
            //     （如 FnOS NAS）。立即返回让 coordinator 快速回退单流，不空烧退避。
            //   • total_downloaded>0：Range 明确工作过，本次 200 是瞬时的（alist 代理
            //     云盘在连接压力下偶发全量响应）。落入下方通用 Err(e) 分支像普通瞬时
            //     错误一样带退避重试——换连接重发多数即恢复 206。
            Err(e @ DownloadError::RangeNotSupported(_))
                if total_downloaded.load(Ordering::Relaxed) == 0 =>
            {
                return Err(e);
            }
            Err(e) => {
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
                attempts += 1;
                if attempts >= MAX_RETRIES {
                    return Err(e);
                }
                // 瞬时失败必须留痕：截断/停滞/网络错误此前静默进退避，日志里
                // 只见"最后一击"（如重连撞 400），无法还原首因（断流 vs 停滞）。
                log_info!(
                    "[segment-retry] task {} seg {} 瞬时失败（第 {}/{} 次重试前）：{}",
                    task_id,
                    seg_idx,
                    attempts,
                    MAX_RETRIES,
                    e
                );
                // Recover actual_start *and* seg_end from DB for partial progress.
                // seg_end may have been shrunk by a coordinator split since we started.
                if let Ok(segs) = db.load_segments(task_id).await
                    && let Some(seg) = segs.iter().find(|s| s.index == seg_idx)
                {
                    seg_end = seg.end_byte;
                    actual_start = seg_start + seg.downloaded_bytes;
                    if actual_start > seg_end {
                        // Segment completed during previous attempt.
                        return Ok(seg.downloaded_bytes);
                    }
                }
                let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                tokio::select! {
                    _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
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
    client: &Client,
    cancel: &CancellationToken,
    conn_sensitive: &AtomicBool,
    reconnect_hostile: &AtomicBool,
    total_downloaded: &AtomicI64,
    // 当前规划总大小的共享视图（coordinator 就地扩容时更新，见 planned_total 注释）。
    planned_total: &AtomicI64,
    // `planned_total` 是否为【未经 probe 验证的估计值】（fresh hint 模式）。true 时
    // Content-Range 分母才是权威真实大小，扩容检查采零容差；见下方 size-check 注释。
    size_is_estimate: bool,
    // hint 模式跨段版本一致性基线（首个 206 的 validator latch），见
    // check_cross_segment_validators。
    first_validators: &StdMutex<Option<(String, String)>>,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    speed_limiter: &SpeedLimiter,
    spec: &crate::downloader::RequestSpec,
    expected_etag: &str,
    expected_last_modified: &str,
    sync_gate: &FileSyncGate,
) -> Result<i64, DownloadError> {
    if actual_start > seg_end {
        // Already complete.
        return Ok(seg_end - seg_start + 1);
    }

    // 开放式段不给终点：服务器把流一直送到文件尾，写循环按共享 end_byte 预算
    // 截断。coordinator 可在其余连接被拒时就地扩容本段预算续流（吸收合并），
    // 也天然兼容"对带终点 Range 返回 400"的怪异端点。
    let range = if open_ended {
        format!("bytes={actual_start}-")
    } else {
        format!("bytes={}-{}", actual_start, seg_end)
    };
    // 多段下载始终用 GET——上游 resolve_file_info 已确保 spec.is_get_like()，
    // 此处显式传入 GET 以规避：（1）调用方误传 non-GET spec；（2）spec.method
    // 是 HEAD（HEAD 不携带 body，没有意义）。
    let mut req = crate::downloader::build_request(client, url, reqwest::Method::GET, spec)
        .header("Range", range);
    // If-Range：把"文件是否自 probe 起变化"的判定交给服务器。validator 一致 →
    // 返回 206（正常分段）；不一致 → 返回 200 全量 → 下方 != 206 守卫触发
    // RangeNotSupported，coordinator 取消并回退单流（download_single 的 If-Range
    // 会再判一次），从而**即使 CDN 在 206 上剥离了 ETag/Last-Modified**也能阻止
    // 新旧版本静默拼接（BUG-COORD-XVERSION-NO-CONDITIONAL）。下方逐段 ETag 比对
    // 作为第二道防线保留（应对服务器忽略 If-Range 的情形）。
    // If-Range 必须用【强】validator（RFC 7233 §3.2）：弱 ETag（`W/` 前缀）在
    // If-Range 上语义未定义，部分严格服务器即便文件未变也会回 200，反而误触
    // 下方回退。故强 ETag 优先，弱 ETag 跳过、回退 Last-Modified。
    let validator = if !expected_etag.is_empty() && !expected_etag.starts_with("W/") {
        Some(expected_etag.to_string())
    } else if !expected_last_modified.is_empty() {
        Some(expected_last_modified.to_string())
    } else {
        None
    };
    let validator_sent = validator.is_some();
    if let Some(v) = validator {
        req = req.header("If-Range", v);
    }
    let resp = req.send().await?.error_for_status()?;

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
        // 区分两种 non-206：
        //   (a) 我们发了 If-Range 且响应 validator 与 probe【不同】→ 文件在 probe
        //       与本段请求之间确实变了。这是"版本变化"而非"服务器不支持 Range"——
        //       仅本任务回退单流（RangeNotSupported 会触发 run_download_inner 清理
        //       临时文件并重下新版本）即可，【绝不】把整个主机记入单连接缓存，
        //       否则一次文件变更会牵连该主机后续所有无关下载 24h 失去多段吞吐
        //       （BUG-COORD-IFRANGE-200-POISONS-HOST）。
        //   (b) 未发 validator，或响应 validator 与 probe 相同/缺失 → 服务器确实
        //       无视 Range（如 FnOS NAS）。记录主机，后续任务直接走单流。
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
        let version_changed = validator_sent
            && ((!expected_etag.is_empty() && !resp_etag.is_empty() && resp_etag != expected_etag)
                || (!expected_last_modified.is_empty()
                    && !resp_lm.is_empty()
                    && resp_lm != expected_last_modified));
        // 分两类 non-206 处理：
        //   • version_changed：文件在 probe 与本段请求之间变了（If-Range validator 不
        //     匹配 → 服务器忽略 Range 回 200 全量新版本）。返回 VersionChanged，让上层
        //     清空旧数据重下新版本；【绝不】记录主机单连接缓存（与 Range 能力无关）
        //     （BUG-COORD-IFRANGE-200-POISONS-HOST）。
        if version_changed {
            return Err(DownloadError::VersionChanged(resp.status().to_string()));
        }

        // 开放式首段 + 从 0 起：200 全量流与我们请求的字节完全一致（服务器
        // 忽略/不支持 Range 时对 `bytes=0-` 返回整文件），直接采用——写循环按
        // 共享预算截断，行为与 206 无异。不置 conn_sensitive、不记录单连接
        // 缓存（这条流本身就是最优路径；其余 worker 的非 206 照常走信号路径）。
        let accepted_open_ended_200 =
            open_ended && actual_start == 0 && resp.status() == reqwest::StatusCode::OK;
        if !accepted_open_ended_200 {
            // 连接敏感信号：服务器对 Range 请求返回了非 206（且非版本变化）——典型于 alist
            // 代理云盘在连接压力下偶发全量响应。置位后 coordinator 衰减【主动拆分】以降低连接
            // churn，减少后续瞬时 200 的发生（保留 reactive 拆分做尾段抢救）。一次性 latch，
            // 仅首次置位打日志。
            if !conn_sensitive.swap(true, Ordering::Relaxed) {
                log_info!(
                    "[coordinator] task {} 检测到连接敏感（服务器对 Range 请求返回 {}），\
                 停止主动拆分以降低连接 churn",
                    task_id,
                    resp.status()
                );
            }
            //   • 非版本变化的 200：仅当【从未下载到任何数据】(total_downloaded==0，从头到尾
            //     没有一个段拿到过 206) 才记录主机——这是真·服务器无视 Range（如 FnOS NAS）。
            //     若已下载过数据（Range 明确工作过），本次 200 是【瞬时】的（alist 代理迅雷/
            //     光鸭云盘在连接压力下偶发全量响应），绝不能因一次瞬时 200 把整个主机打成
            //     单连接 24h、阻断续传与多段吞吐（BUG-COORD-TRANSIENT-200-POISONS-HOST）。
            //     判定与 coordinator transient_range 一致，均以 total_downloaded>0 为
            //     "Range 工作过"的证据。
            if total_downloaded.load(Ordering::Relaxed) == 0 {
                record_single_conn_domain_persist(url, db);
            }
            return Err(DownloadError::RangeNotSupported(resp.status().to_string()));
        }
        log_info!(
            "[coordinator] task {} seg {} 开放式首段收到 200 全量流（服务器忽略 Range），\
             从 0 起字节等价，直接采用",
            task_id,
            seg_idx
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
    // different versions.
    //
    // 版本一致性守卫【先于】下方的“真实大小 > 规划”扩容检查执行：文件在下载中途被
    // 替换应被判定为【版本变化】（fail-fast），而不是先触发一次整体扩容重下、再在
    // 陈旧 validator 上失败。（fresh hint 模式下 expected_etag 为空 → 守卫被跳过，此
    // 次序调整只影响 probe 验证过的下载。）
    //
    // Only check when the probe returned a non-empty value AND the segment
    // response also provides the header.  Many CDN edge servers strip these
    // headers on Range responses, so a missing header is not an error.
    if !expected_etag.is_empty()
        && let Some(resp_etag) = resp.headers().get(reqwest::header::ETAG)
        && let Ok(resp_etag_str) = resp_etag.to_str()
        && !resp_etag_str.is_empty()
        && resp_etag_str != expected_etag
    {
        return Err(DownloadError::Other(format!(
            "segment {}: ETag mismatch — probe=\"{}\", segment=\"{}\". \
             The file may have changed on the server during download.",
            seg_idx, expected_etag, resp_etag_str
        )));
    }
    if !expected_last_modified.is_empty()
        && let Some(resp_lm) = resp.headers().get(reqwest::header::LAST_MODIFIED)
        && let Ok(resp_lm_str) = resp_lm.to_str()
        && !resp_lm_str.is_empty()
        && resp_lm_str != expected_last_modified
    {
        return Err(DownloadError::Other(format!(
            "segment {}: Last-Modified mismatch — probe=\"{}\", segment=\"{}\". \
             The file may have changed on the server during download.",
            seg_idx, expected_last_modified, resp_lm_str
        )));
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
        let drift_tolerance = if size_is_estimate {
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
        record_single_conn_domain_persist(url, db);
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
        record_single_conn_domain_persist(url, db);
        return Err(DownloadError::Other(format!(
            "segment {seg_idx}: server returned unsupported Content-Encoding '{unknown}' on a \
             Range response; cannot assemble byte ranges. Please retry in single-stream mode."
        )));
    }

    // 注：旧版本会在 segment 0 响应中提取 Content-Disposition 的"更好文件名"
    // 写入 DB 并通知 Dart UI，run_download_inner 末尾再据此 dedup + 重定向
    // dest_path。该机制已移除——新架构下文件名由 DownloadManager 在
    // do_start_task 同步段统一决策（probe 阶段读取 Content-Disposition），
    // 所有下载器内部不再变更文件名，避免与 manager 的 reserved_temp_paths
    // 协调断裂导致并发冲突（参见 PR #296 自我冲突回归 bug）。

    let mut stream = resp.bytes_stream();

    let file = OpenOptions::new().write(true).open(dest).await?;
    let seg_remaining = seg_end - actual_start + 1;
    let buf_cap = buf_writer_capacity_for_segment(seg_remaining);
    let mut file = tokio::io::BufWriter::with_capacity(buf_cap, file);
    file.seek(std::io::SeekFrom::Start(actual_start as u64))
        .await?;

    let mut seg_downloaded = actual_start - seg_start;
    let mut last_report = Instant::now();
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
                // best-effort fdatasync：cancel 落库的偏移会被 resume 信任，掉电
                // 后页缓存丢失会致空洞。失败不掩盖 Cancelled（见 BUG-COORD-FSYNC）。
                let _ = file.get_ref().sync_data().await;
                update_seg_state(seg_states, seg_idx, seg_downloaded);
                let _ = db.update_segment_progress(task_id, seg_idx, seg_downloaded).await;
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
                        file.flush().await?;
                        let _ = file.get_ref().sync_data().await;
                        update_seg_state(seg_states, seg_idx, seg_downloaded);
                        let _ = db.update_segment_progress(
                            task_id, seg_idx, seg_downloaded,
                        ).await;
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
                            && let Some(s) = states.iter().find(|s| s.index == seg_idx) {
                                effective_end = s.end_byte;
                            }

                        // Calculate the write budget.
                        let current_pos = seg_start + seg_downloaded;
                        let budget = (effective_end - current_pos + 1).max(0) as usize;
                        let write_len = bytes.len().min(budget);

                        if write_len == 0 {
                            // Reached the (possibly shrunk) boundary — stop.
                            boundary_reached = true;
                            break;
                        }

                        let write_slice = &bytes[..write_len];

                        // --- Speed limiter: write in sub-chunks as tokens allow ---
                        let mut offset = 0usize;
                        while offset < write_len {
                            let remaining = (write_len - offset) as u64;
                            let allowed = speed_limiter.consume(remaining).await;
                            let end = offset + allowed as usize;
                            file.write_all(&write_slice[offset..end]).await?;
                            offset = end;
                        }

                        let len = write_len as i64;
                        seg_downloaded += len;
                        total_downloaded.fetch_add(len, Ordering::Relaxed);

                        // Update shared segment state (workers → coordinator channel).
                        // Only `downloaded_bytes` is written — `end_byte` is
                        // exclusively owned by the coordinator.
                        update_seg_state(seg_states, seg_idx, seg_downloaded);

                        // If we truncated the chunk, we hit the boundary.
                        if write_len < bytes.len() {
                            boundary_reached = true;
                            file.flush().await?;
                            let _ = db.update_segment_progress(
                                task_id, seg_idx, seg_downloaded,
                            ).await;
                            break;
                        }

                        // --- Progress report to Dart ---
                        if last_report.elapsed().as_millis() >= UI_REPORT_INTERVAL_MS {
                            let current_total = total_downloaded.load(Ordering::Relaxed);
                            let snapshot = seg_states
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let _ = progress_tx
                                .send(ProgressUpdate {
                                    task_id: task_id.to_string(),
                                    downloaded_bytes: current_total,
                                    total_bytes: planned_total.load(Ordering::Relaxed),
                                    status: 1,
                                    error_message: String::new(),
                                    file_name: String::new(),
                                    segment_details: Some(snapshot),
                                    ..Default::default()
                                })
                                .await;
                            last_report = Instant::now();
                        }

                        // --- DB persistence (periodic) ---
                        // 落库前必须保证偏移已 fdatasync 落盘：DB 记录的偏移是 resume
                        // 的信任来源，若只 flush（用户态→页缓存）就落库，掉电后该区间
                        // 在盘上仍是预分配的 0，resume 据 DB 跳过 → 永久空洞且骗过完整性
                        // 检查（BUG-COORD-FSYNC）。
                        //
                        // 但 fdatasync 刷的是【整个文件 inode】的脏页，与 fd 无关——64 段
                        // 各自每 3s fsync 是重复整盘刷写。改用 FileSyncGate 合并为全局每
                        // MIN_SYNC_GAP 至多一次 fdatasync，并仅把【已被某次 fsync 覆盖】的
                        // 偏移 durable_offset 写入 DB，严格保持上述不变式；未被覆盖的最新
                        // 快照暂存 pending_snap，待后续 fsync 覆盖后提交（滞后约一个周期）。
                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            file.flush().await?;
                            let snap = seg_downloaded;
                            let snap_t = Instant::now();
                            let synced_start = sync_gate.sync_if_stale(file.get_ref()).await?;
                            if synced_start >= snap_t {
                                // fsync 起始于本快照之后 → snap 全部已持久化。
                                durable_offset = snap;
                                pending_snap = None;
                            } else {
                                // 命中一次较早的合并 fsync：snap 尚未被覆盖。先提交上一轮
                                // 挂起快照（若已被本次 fsync 覆盖），再把本次 snap 记为挂起。
                                if let Some((off, t)) = pending_snap
                                    && synced_start >= t
                                {
                                    durable_offset = off;
                                }
                                pending_snap = Some((snap, snap_t));
                            }
                            let _ = db
                                .update_segment_progress(task_id, seg_idx, durable_offset)
                                .await;
                            last_db_save = Instant::now();
                        }
                    }
                    Some(Err(e)) => {
                        file.flush().await?;
                        let _ = file.get_ref().sync_data().await;
                        update_seg_state(seg_states, seg_idx, seg_downloaded);
                        let _ = db
                            .update_segment_progress(task_id, seg_idx, seg_downloaded)
                            .await;
                        return Err(DownloadError::Request(e));
                    }
                    None => break,
                }
            }
        }
    }

    file.flush().await?;
    // 段写入完成后、落库标记 Completed 前做 fdatasync，确保数据真正落盘。
    // coordinator 把 Completed 段视为永久完成、resume 时绝不重取——若此处不
    // 持久化，崩溃/掉电后会留下 "DB 完成但磁盘为 0" 的空洞且通过完整性检查
    // （BUG-COORD-FSYNC）。放在下方 fadvise(DONTNEED) 之前，使已落盘的干净页
    // 可被安全丢弃。同时覆盖紧随其后的截断分支落库（2164 行附近）。
    file.get_ref().sync_data().await?;

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
            && let Some(s) = states.iter().find(|s| s.index == seg_idx)
        {
            effective_end = s.end_byte;
        }
        let next_pos = seg_start + seg_downloaded;
        if next_pos <= effective_end {
            // 已写到磁盘的部分进度已通过 update_seg_state / update_segment_progress
            // 在循环内持久化（见 DB_SAVE_INTERVAL_SECS 分支）。这里仅补一次落库，
            // 确保 do_segment_with_retry 以 seg_start + downloaded_bytes 续传本段，
            // 而非从头重下，也不污染 total_downloaded 计数。
            update_seg_state(seg_states, seg_idx, seg_downloaded);
            let _ = db
                .update_segment_progress(task_id, seg_idx, seg_downloaded)
                .await;
            return Err(DownloadError::Other(format!(
                "segment {} truncated: received {} bytes, expected {} (server closed stream early)",
                seg_idx,
                next_pos - actual_start,
                effective_end - actual_start + 1
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
        unsafe {
            libc::posix_fadvise(
                fd,
                seg_start as libc::off_t,
                seg_downloaded as libc::off_t,
                libc::POSIX_FADV_DONTNEED,
            );
        }
    }

    update_seg_state(seg_states, seg_idx, seg_downloaded);
    let _ = db
        .update_segment_progress(task_id, seg_idx, seg_downloaded)
        .await;

    Ok(seg_downloaded)
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
        && let Some(s) = states.iter_mut().find(|s| s.index == seg_idx)
    {
        s.downloaded_bytes = downloaded_bytes;
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
        FileSyncGate, LiveSegment, MAX_SEGMENTS, MIN_SPLIT_BYTES, MIN_SYNC_GAP, SegState,
        TAIL_MIN_SPLIT_BYTES, all_done, build_seg_state_vec, check_cross_segment_validators,
        conn_cap_cache, dynamic_min_split_bytes, extract_host, find_next_pending_only,
        find_next_work, is_single_conn_domain, rebuild_seg_states, record_domain_conn_cap,
        try_proactive_split, try_split_largest, validate_coverage,
    };
    use crate::downloader::{DownloadError, SegmentProgressInfo, is_server_rejection};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex as StdMutex};

    fn make_seg(index: i32, start: i64, end: i64, downloaded: i64, state: SegState) -> LiveSegment {
        LiveSegment {
            index,
            start_byte: start,
            end_byte: end,
            downloaded_bytes: downloaded,
            state,
        }
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
        // 分类必须与 403/429 区分：400 走连接级拒绝降级（需 other_working 佐证），
        // 但绝不写入域名连接数缓存（record 只看 is_server_rejection）。
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
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
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
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
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
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
        assert!(result.is_none(), "should not exceed MAX_SEGMENTS");
    }

    /// After Fix 1: completed segments do not count toward MAX_SEGMENTS.
    /// 63 Completed + 1 Active of 10 MB should allow a split because
    /// active_or_pending = 1 < MAX_SEGMENTS = 64.
    #[test]
    fn split_allowed_when_completed_segments_free_slots() {
        let total_bytes: i64 = 10_000_000;
        let mut segs = BTreeMap::new();
        // 63 completed segments (minimal placeholder ranges).
        for i in 0..(MAX_SEGMENTS - 1) {
            segs.insert(i, make_seg(i, i as i64, i as i64, 1, SegState::Completed));
        }
        // 1 active segment with 10 MB remaining (well above MIN_SPLIT_BYTES).
        segs.insert(
            MAX_SEGMENTS - 1,
            make_seg(MAX_SEGMENTS - 1, 0, total_bytes - 1, 0, SegState::Active),
        );
        let mut next_idx = MAX_SEGMENTS;

        // With old code: segments.len() == 64 → None (workers retired).
        // With fix: active_or_pending == 1 < 64 → should split successfully.
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
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
    /// when 64 Active/Pending segments exist, no further split is allowed.
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

        // active_or_pending == 64 >= MAX_SEGMENTS → must still return None.
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
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
            let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
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
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
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

        let result = try_split_largest(&mut segs, &mut next, MIN_SPLIT_BYTES);
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
        let result = find_next_work(&mut segs, &mut next_idx, 100_000_001, MIN_SPLIT_BYTES);
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

        let result = find_next_work(&mut segs, &mut next_idx, 10_000_000, MIN_SPLIT_BYTES);
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
        let result = find_next_work(&mut segs, &mut next_idx, 100, MIN_SPLIT_BYTES);
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
        let normal = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
        assert!(normal.is_none(), "normal split should fail for 500 KB");

        // But find_next_work should succeed via tail micro-split (Strategy 3):
        let result = find_next_work(&mut segs, &mut next_idx, remaining, MIN_SPLIT_BYTES);
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
        let result = find_next_work(&mut segs, &mut next_idx, remaining, MIN_SPLIT_BYTES);
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
        let result = find_next_work(&mut segs, &mut next_idx, remaining, TAIL_MIN_SPLIT_BYTES);
        // 100KB >= 64KB so try_split_largest(TAIL) succeeds, but we're testing
        // that when called with TAIL_MIN_SPLIT_BYTES directly, Strategy 2
        // handles it (not Strategy 3 infinite loop).
        // Strategy 2: try_split_largest(segs, next, 64KB) with 100KB remaining → succeeds.
        assert!(
            result.is_some(),
            "Strategy 2 itself should handle TAIL_MIN_SPLIT_BYTES"
        );
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
        let result = find_next_work(&mut segs, &mut next_idx, total, MIN_SPLIT_BYTES);
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
            try_proactive_split(&mut segs, &mut next_idx, MIN_SPLIT_BYTES).is_none(),
            "should not proactively split when Pending segments exist"
        );
    }

    #[test]
    fn proactive_split_creates_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 19_999_999, 0, SegState::Active));
        let mut next = 1;

        let result = try_proactive_split(&mut segs, &mut next, MIN_SPLIT_BYTES);
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

    /// 编译时验证 is_server_rejection 可以接受 DownloadError::Request 变体。
    /// 构造真实的 reqwest::Error(403/429) 需要 `http` crate，此处仅验证类型兼容性。
    #[test]
    fn server_rejection_accepts_request_variant() {
        // 不实际发起 HTTP 请求，仅验证代码路径可编译。
        if false {
            let client = reqwest::Client::new();
            let _fut = async {
                let resp = client.get("http://x").send().await.unwrap();
                let err = resp.error_for_status().unwrap_err();
                let dl_err = DownloadError::Request(err);
                let _ = is_server_rejection(&dl_err);
            };
        }
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
        use super::{parse_conn_caps, serialize_conn_caps};
        let mut map = std::collections::HashMap::new();
        map.insert("a.example.com".to_string(), (4, 1000u64));
        map.insert("b.example.com:8443".to_string(), (1, 2000u64));

        let raw = serialize_conn_caps(&map);
        assert!(raw.starts_with("v1\n"), "首行必须是版本标记");
        assert_eq!(parse_conn_caps(&raw), map, "roundtrip 必须无损");
    }

    #[test]
    fn conn_caps_unknown_version_discarded_entirely() {
        use super::parse_conn_caps;
        // 未来版本/无版本头的数据整体丢弃（重新学习），绝不猜测字段含义
        assert!(parse_conn_caps("v2\na.example.com\t4\t1000").is_empty());
        assert!(parse_conn_caps("a.example.com\t4\t1000").is_empty());
        assert!(parse_conn_caps("").is_empty());
    }

    #[test]
    fn conn_caps_malformed_lines_skipped_within_version() {
        use super::parse_conn_caps;
        let raw = "v1\nok.example.com\t4\t1000\n\
                   缺字段\t4\nbad-cap.example.com\tx\t1\nzero-cap.example.com\t0\t1";
        let map = parse_conn_caps(raw);
        assert_eq!(map.len(), 1, "畸形/非法行应被跳过");
        assert_eq!(map.get("ok.example.com"), Some(&(4, 1000u64)));
    }

    #[tokio::test]
    async fn conn_caps_persist_and_reload_roundtrip() {
        use super::{
            CONN_CAP_CONFIG_KEY, domain_conn_cap, load_domain_conn_caps, now_unix_secs,
            serialize_conn_caps,
        };
        let dir = std::env::temp_dir().join(format!("fluxdown_connp_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db = crate::db::Db::open(&dir).await.expect("open db");

        let fresh_host = "persist-fresh.example.com";
        let stale_host = "persist-stale.example.com";
        // 预清理全局缓存
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(fresh_host);
            cache.remove(stale_host);
        }

        // 写入一条新鲜 + 一条过期（25h 前）的持久化数据
        let now = now_unix_secs();
        let mut map = std::collections::HashMap::new();
        map.insert(fresh_host.to_string(), (4, now));
        map.insert(stale_host.to_string(), (1, now - 25 * 3600));
        db.set_config(CONN_CAP_CONFIG_KEY, &serialize_conn_caps(&map))
            .await
            .expect("set_config");

        load_domain_conn_caps(&db).await;

        assert_eq!(
            domain_conn_cap(&format!("http://{fresh_host}/f")),
            Some(4),
            "新鲜条目应跨重启恢复"
        );
        assert_eq!(
            domain_conn_cap(&format!("http://{stale_host}/f")),
            None,
            "过期条目在加载时应被丢弃"
        );

        // 清理
        if let Ok(mut cache) = conn_cap_cache().lock() {
            cache.remove(fresh_host);
            cache.remove(stale_host);
        }
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // FileSyncGate（fdatasync 合并闸）
    // -----------------------------------------------------------------------

    /// 在系统临时目录创建一个内容非空、可读写的临时文件，返回其路径与已打开的
    /// `tokio::fs::File` 句柄。调用方负责在测试结束时
    /// `let _ = std::fs::remove_file(&path);` 清理（失败无需 panic）。
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

    // 合并闸的核心契约：MIN_SYNC_GAP 窗口内的重复调用必须复用同一次【已完成】
    // fsync 的起始时刻，绝不重复触发整盘 fdatasync。若合并逻辑失效（例如
    // `syncing`/`last_completed_start` 判据被改坏、或误把窗口判定为“已过期”），
    // 本测试会观察到两次不同的 Instant 而失败——这正是 BUG-COORD-FSYNC 想避免的
    // “N 段各自整盘刷写”退化路径。
    #[tokio::test]
    async fn coalesce_within_gap_reuses_same_sync() {
        let (path, file) = open_sync_gate_test_file().await;
        let gate = FileSyncGate::new();

        let s1 = gate
            .sync_if_stale(&file)
            .await
            .expect("first sync_if_stale should succeed");
        let s2 = gate
            .sync_if_stale(&file)
            .await
            .expect("second sync_if_stale should succeed");

        assert_eq!(
            s1, s2,
            "MIN_SYNC_GAP 内的第二次调用必须复用第一次已完成 fsync 的起始时刻；\
             若返回了不同的 Instant，说明合并逻辑失效，退化成了每次都重新 fdatasync"
        );

        let _ = std::fs::remove_file(&path);
    }

    // gap 之外必须真正触发新一轮 fsync：验证“新鲜度”判据没有被写反（例如
    // `elapsed() < MIN_SYNC_GAP` 误写成恒真或恒假）。防两类回归：数据长期得不到
    // 刷新（S 永不前进），或合并彻底失效。用真实 sleep 而非 tokio 暂停时钟，
    // 因为 sync_data() 经 spawn_blocking 落到真实阻塞 IO 线程池，与虚拟时钟
    // 交互不可靠。
    #[tokio::test]
    async fn fresh_sync_after_gap_advances() {
        let (path, file) = open_sync_gate_test_file().await;
        let gate = FileSyncGate::new();

        let s1 = gate
            .sync_if_stale(&file)
            .await
            .expect("first sync_if_stale should succeed");

        // 略大于 MIN_SYNC_GAP，确保确定性地跨过闸门的新鲜度窗口。
        tokio::time::sleep(MIN_SYNC_GAP + std::time::Duration::from_millis(100)).await;

        let s2 = gate
            .sync_if_stale(&file)
            .await
            .expect("second sync_if_stale after gap should succeed");

        assert!(
            s2 > s1,
            "超过 MIN_SYNC_GAP 后必须触发一次新的 fdatasync，其起始时刻应严格晚于上一次；\
             s2 <= s1 意味着 gap 判据失效（要么从不刷新，要么被误判为仍然新鲜）"
        );

        let _ = std::fs::remove_file(&path);
    }

    // 并发突发场景：多个 worker 同时对同一 gate 发起 sync_if_stale，必须全部
    // 无 panic/无死锁地成功返回，且被合并为极少数几次真实 fsync（理想 1 次）。
    // 用共享的同一个 `Arc<tokio::fs::File>` 句柄模拟多段 worker 共享同一文件
    // fd 的真实场景。防两类回归：
    // 1) notify 的 TOCTOU（通知先于等待者注册而丢失）导致等待者永久阻塞、
    //    整个测试挂死；
    // 2) 合并逻辑失效，导致 N 个并发调用各自触发一次整盘 fsync（起始时刻
    //    彼此不同，distinct 数会显著大于 1~2）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callers_coalesce_and_complete() {
        let (path, file) = open_sync_gate_test_file().await;
        let gate = FileSyncGate::new();
        let file = Arc::new(file);

        const N: usize = 16;
        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let gate = gate.clone();
            let file = Arc::clone(&file);
            handles.push(tokio::spawn(async move { gate.sync_if_stale(&file).await }));
        }

        let mut starts = Vec::with_capacity(N);
        for h in handles {
            // join 失败（子任务 panic 或被取消）本身就是需要暴露的回归，
            // 不能被吞掉，否则死锁会被误判为“测试通过”。
            let joined = h
                .await
                .expect("sync_if_stale task must not panic or be cancelled");
            starts.push(joined.expect("sync_if_stale must not return an I/O error"));
        }

        let distinct: std::collections::BTreeSet<std::time::Instant> = starts.into_iter().collect();
        assert!(
            distinct.len() <= 2,
            "{N} 个并发调用应被合并为至多 2 次真实 fsync（理想 1 次），\
             实际观察到 {} 种不同起始时刻，说明合并逻辑失效或退化为逐一 fsync",
            distinct.len()
        );

        let _ = std::fs::remove_file(&path);
    }
}
