use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fs2::FileExt;
use sqlx::Any;
use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::pool::PoolConnection;
use sqlx::{AssertSqlSafe, Row};
use thiserror::Error;

use crate::model::{GroupInfo, MAIN_QUEUE_ID, QueueInfo, SourceBytes, TaskInfo};
use crate::rss::model::{RssItemInfo, RssItemStatus, RssSourceInfo};

mod bt_guard;
pub use bt_guard::{
    BT_GUARD_EVENT_RING, BtGuardBatch, BtGuardEntryOp, BtGuardEntryQuery, BtGuardEntryRow,
    BtGuardEventQuery, BtGuardEventRow, BtGuardLedgerRow, BtGuardPrune, BtGuardStrikeRow,
};

#[derive(Error, Debug)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("unsupported database url: {0}")]
    UnsupportedUrl(String),
    #[error("database I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("engine writer lease is already held: {0}")]
    WriterLeaseHeld(String),
    #[error("invalid stored configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid task activity query: {0}")]
    InvalidActivityQuery(&'static str),
}

/// 原子配置补丁失败。
#[derive(Error, Debug)]
pub enum ConfigPatchError {
    #[error(transparent)]
    Database(#[from] DbError),
    #[error("config revision conflict: expected {expected}, current {current}")]
    RevisionConflict { expected: u64, current: u64 },
    #[error("config revision overflow")]
    RevisionOverflow,
    #[error("stored config revision is invalid: {0}")]
    InvalidStoredRevision(String),
}

/// [`Db::load_deletion_facts`] 的结果：批量删除决定逐任务删哪些文件所需的全部库内事实。
#[derive(Debug, Default)]
pub struct DeletionFacts {
    /// 任务 → 「文件已存在」决定原值（'' / rename / overwrite / skip）；缺失 = 任务不存在。
    pub exists_decisions: HashMap<String, String>,
    /// 带独立音轨（`audio_url` 非空）的任务。
    pub has_audio_url: HashSet<String>,
    /// 任务 → 已登记的衍生产物文件名。
    pub artifacts: HashMap<String, Vec<String>>,
    /// `(save_dir, file_name)` → 把该名字登记为自己 `file_name` 的全部任务 id。
    pub file_owners: HashMap<(String, String), Vec<String>>,
}

/// 一条 `DELETE FROM config WHERE key IN (...)` 覆盖的任务数（每任务 5 个键，
/// 参数个数保持在 SQLite 999 上限内）。
const CONFIG_KEY_TASKS_PER_STATEMENT: usize = 180;

/// `$1,$2,…,$n` 占位符列表（两后端共用的编号占位符）。
fn numbered_placeholders(n: usize) -> String {
    (1..=n)
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// 数据库后端类型，由连接 URL 的 scheme 决定。
///
/// 仅在**无法统一 SQL 文本**的少数分支处使用（DDL 方言差异、
/// `wal_checkpoint` 等 SQLite 专属操作）；常规查询两后端共用同一份
/// `$N` 占位符 SQL。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Sqlite,
    Postgres,
}

impl Backend {
    fn from_url(url: &str) -> Result<Self, DbError> {
        let lower = url.trim_start().to_ascii_lowercase();
        if lower.starts_with("sqlite:") {
            Ok(Self::Sqlite)
        } else if lower.starts_with("postgres:") || lower.starts_with("postgresql:") {
            Ok(Self::Postgres)
        } else {
            Err(DbError::UnsupportedUrl(url.to_owned()))
        }
    }
}

/// 建表 DDL（SQLite 方言）。
///
/// 新库直接建出**全量列**（含历史迁移新增列）；`add_column_if_missing`
/// 只为升级旧桌面库服务。
///
/// 注意 `task_segments` 使用复合主键 `(task_id, segment_index)`——
/// 旧库的 `id INTEGER PRIMARY KEY AUTOINCREMENT` 列全代码库从不读取，
/// 新建库不再包含；旧库因 `CREATE TABLE IF NOT EXISTS` 不受影响。
const SQLITE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    file_name TEXT NOT NULL,
    save_dir TEXT NOT NULL,
    status INTEGER NOT NULL DEFAULT 0,
    total_bytes INTEGER NOT NULL DEFAULT 0,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    segments INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    error_message TEXT NOT NULL DEFAULT '',
    proxy_url TEXT NOT NULL DEFAULT '',
    queue_id TEXT NOT NULL DEFAULT '',
    checksum TEXT NOT NULL DEFAULT '',
    ignore_tls_errors INTEGER NOT NULL DEFAULT 0,
    bt_selected_files TEXT NOT NULL DEFAULT '',
    bt_custom_name TEXT NOT NULL DEFAULT '',
    orig_etag TEXT NOT NULL DEFAULT '',
    orig_last_modified TEXT NOT NULL DEFAULT '',
    audio_url TEXT NOT NULL DEFAULT '',
    file_missing INTEGER NOT NULL DEFAULT 0,
    file_hash TEXT NOT NULL DEFAULT '',
    range_verified INTEGER NOT NULL DEFAULT 1,
    name_inferred INTEGER NOT NULL DEFAULT 0,
    resp_name TEXT NOT NULL DEFAULT '',
    resp_mime TEXT NOT NULL DEFAULT '',
    source_changed INTEGER NOT NULL DEFAULT 0,
    category_route TEXT NOT NULL DEFAULT '',
    request_method TEXT NOT NULL DEFAULT '',
    queue_order INTEGER NOT NULL DEFAULT 0,
    uploaded_bytes BIGINT NOT NULL DEFAULT 0,
    uploaded_at_completion BIGINT NOT NULL DEFAULT 0,
    seeding_status INTEGER NOT NULL DEFAULT 0,
    seeding_message TEXT NOT NULL DEFAULT '',
    seeding_started_at INTEGER NOT NULL DEFAULT 0,
    seeding_time_secs INTEGER NOT NULL DEFAULT 0,
    seed_ratio_limit_milli INTEGER NOT NULL DEFAULT -2,
    seed_post_ratio_limit_milli INTEGER NOT NULL DEFAULT -2,
    seed_time_limit_minutes INTEGER NOT NULL DEFAULT -2,
    seed_inactive_time_limit_minutes INTEGER NOT NULL DEFAULT -2,
    seed_upload_limit_bps INTEGER NOT NULL DEFAULT 0,
    seed_limits_exempt INTEGER NOT NULL DEFAULT 0,
    bt_sequential INTEGER NOT NULL DEFAULT 0,
    bt_first_last_pieces_first INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS task_segments (
    task_id TEXT NOT NULL,
    segment_index INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (task_id, segment_index),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS torrent_files (
    task_id TEXT PRIMARY KEY,
    file_bytes BLOB NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS queues (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    speed_limit_kbps INTEGER NOT NULL DEFAULT 0,
    upload_limit_kbps INTEGER NOT NULL DEFAULT 0,
    max_concurrent INTEGER NOT NULL DEFAULT 0,
    default_save_dir TEXT NOT NULL DEFAULT '',
    position INTEGER NOT NULL DEFAULT 0,
    default_segments INTEGER NOT NULL DEFAULT 0,
    default_user_agent TEXT NOT NULL DEFAULT '',
    is_running INTEGER NOT NULL DEFAULT 1,
    schedule_enabled INTEGER NOT NULL DEFAULT 0,
    schedule_start TEXT NOT NULL DEFAULT '',
    schedule_stop TEXT NOT NULL DEFAULT '',
    schedule_days INTEGER NOT NULL DEFAULT 127
);
CREATE INDEX IF NOT EXISTS idx_task_segments_task_id ON task_segments(task_id);
CREATE TABLE IF NOT EXISTS ed2k_blocks (
    task_id TEXT NOT NULL,
    block_index INTEGER NOT NULL,
    state INTEGER NOT NULL DEFAULT 0,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    retry_count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (task_id, block_index),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS ed2k_hashset (
    task_id TEXT PRIMARY KEY,
    hashes BLOB NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS task_artifacts (
    task_id TEXT NOT NULL,
    file_name TEXT NOT NULL,
    PRIMARY KEY (task_id, file_name),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS task_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    source_url TEXT NOT NULL DEFAULT '',
    save_dir TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS link_devices (
    fingerprint TEXT PRIMARY KEY,
    identity_pub BLOB NOT NULL,
    name TEXT NOT NULL,
    platform TEXT NOT NULL DEFAULT '',
    link_secret BLOB NOT NULL,
    candidates TEXT NOT NULL DEFAULT '',
    paired_at INTEGER NOT NULL DEFAULT 0,
    last_seen_at INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS rss_sources (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL DEFAULT 'rss',
    provider_config TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    auto_download INTEGER NOT NULL DEFAULT 1,
    start_paused INTEGER NOT NULL DEFAULT 0,
    queue_id TEXT NOT NULL DEFAULT '',
    save_dir TEXT NOT NULL DEFAULT '',
    interval_minutes INTEGER NOT NULL DEFAULT 30,
    include_pattern TEXT NOT NULL DEFAULT '',
    exclude_pattern TEXT NOT NULL DEFAULT '',
    use_regex INTEGER NOT NULL DEFAULT 0,
    smart_episode INTEGER NOT NULL DEFAULT 0,
    size_min_bytes INTEGER NOT NULL DEFAULT 0,
    size_max_bytes INTEGER NOT NULL DEFAULT 0,
    send_referer INTEGER NOT NULL DEFAULT 1,
    notify_on_download INTEGER NOT NULL DEFAULT 1,
    max_per_fetch INTEGER NOT NULL DEFAULT 20,
    cookies TEXT NOT NULL DEFAULT '',
    user_agent TEXT NOT NULL DEFAULT '',
    proxy_url TEXT NOT NULL DEFAULT '',
    last_fetch_at INTEGER NOT NULL DEFAULT 0,
    last_success_at INTEGER NOT NULL DEFAULT 0,
    last_error TEXT NOT NULL DEFAULT '',
    fail_count INTEGER NOT NULL DEFAULT 0,
    seeded INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS rss_items (
    source_id TEXT NOT NULL,
    guid TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    link TEXT NOT NULL DEFAULT '',
    enclosure_url TEXT NOT NULL DEFAULT '',
    resolver_item TEXT NOT NULL DEFAULT '',
    enclosure_length INTEGER NOT NULL DEFAULT 0,
    pub_date INTEGER NOT NULL DEFAULT 0,
    fetched_at INTEGER NOT NULL DEFAULT 0,
    status INTEGER NOT NULL DEFAULT 0,
    task_id TEXT NOT NULL DEFAULT '',
    episode_key TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL DEFAULT '',
    enclosure_type TEXT NOT NULL DEFAULT '',
    fetch_failures INTEGER NOT NULL DEFAULT 0,
    retry_after INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (source_id, guid),
    FOREIGN KEY (source_id) REFERENCES rss_sources(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_rss_items_source ON rss_items(source_id, pub_date);
CREATE INDEX IF NOT EXISTS idx_rss_items_episode ON rss_items(source_id, episode_key);
CREATE TABLE IF NOT EXISTS webhook_deliveries (
    delivery_id TEXT PRIMARY KEY,
    timestamp_ms INTEGER NOT NULL DEFAULT 0,
    event TEXT NOT NULL DEFAULT '',
    endpoint_id TEXT NOT NULL DEFAULT '',
    endpoint_name TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    request_headers TEXT NOT NULL DEFAULT '',
    request_body TEXT NOT NULL DEFAULT '',
    status_code INTEGER NOT NULL DEFAULT 0,
    response_body TEXT NOT NULL DEFAULT '',
    latency_ms INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    success INTEGER NOT NULL DEFAULT 0,
    error TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_ts ON webhook_deliveries(timestamp_ms);
CREATE TABLE IF NOT EXISTS task_activity (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    timestamp_ms BIGINT NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    status INTEGER
);
CREATE INDEX IF NOT EXISTS idx_task_activity_task_id ON task_activity(task_id, id);
CREATE INDEX IF NOT EXISTS idx_task_activity_time ON task_activity(timestamp_ms);
CREATE TABLE IF NOT EXISTS task_activity_retention (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    pruned_through_id BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS task_speed_history (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    bucket_secs BIGINT NOT NULL,
    samples TEXT NOT NULL,
    pending_sum BIGINT NOT NULL,
    pending_secs BIGINT NOT NULL,
    active_secs BIGINT NOT NULL,
    transferred_bytes BIGINT NOT NULL,
    peak_bps BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS plugin_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms BIGINT NOT NULL,
    plugin_id TEXT NOT NULL,
    origin TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_plugin_audit_plugin ON plugin_audit(plugin_id, id);
-- 批量建任务幂等表：同一 batch_id 重放时直接返回首次结果。
-- 与任务插入同事务写入，超过保留期的记录在同一事务中顺带清理。
CREATE TABLE IF NOT EXISTS task_create_batches (
    batch_id TEXT PRIMARY KEY,
    results TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_create_batches_created ON task_create_batches(created_at);
-- BT 防吸血（bt_guard）：过滤条目 / strike / 账本 / 事件环（语义见 bt_guard 模块）。
-- 新表一律 CREATE IF NOT EXISTS：旧库升级即建，无需列迁移。
CREATE TABLE IF NOT EXISTS bt_guard_entries (
    id BIGINT PRIMARY KEY,
    layer TEXT NOT NULL,
    cidr TEXT NOT NULL,
    action TEXT NOT NULL,
    rule TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL DEFAULT '',
    evidence TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    expires_at BIGINT
);
CREATE INDEX IF NOT EXISTS idx_bt_guard_entries_layer ON bt_guard_entries(layer, source);
CREATE TABLE IF NOT EXISTS bt_guard_strikes (
    ip_group TEXT PRIMARY KEY,
    strikes BIGINT NOT NULL,
    last_strike_at BIGINT NOT NULL,
    last_ban_until BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS bt_guard_ledger (
    info_hash TEXT NOT NULL,
    ip_group TEXT NOT NULL,
    delivered_bytes BIGINT NOT NULL,
    dup_bytes BIGINT NOT NULL,
    delivered_bits BLOB NOT NULL,
    claimed_bits BLOB NOT NULL,
    rerequests BIGINT NOT NULL,
    probed_at BIGINT NOT NULL DEFAULT 0,
    first_seen BIGINT NOT NULL,
    last_seen BIGINT NOT NULL,
    lag_cleared_at BIGINT NOT NULL DEFAULT 0,
    excess_baseline BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (info_hash, ip_group)
);
CREATE INDEX IF NOT EXISTS idx_bt_guard_ledger_seen ON bt_guard_ledger(last_seen);
CREATE TABLE IF NOT EXISTS bt_guard_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at BIGINT NOT NULL,
    kind TEXT NOT NULL,
    ip_group TEXT NOT NULL DEFAULT '',
    info_hash TEXT NOT NULL DEFAULT '',
    rule TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL DEFAULT '',
    action TEXT NOT NULL DEFAULT '',
    detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_bt_guard_events_group ON bt_guard_events(ip_group);
-- 云名单文档元数据（单行 JSON：sequence / 时间 / 署名 / clientRules），随 flush 批次写入。
CREATE TABLE IF NOT EXISTS bt_guard_cloud_meta (
    id INTEGER PRIMARY KEY,
    data TEXT NOT NULL
);
";

/// 建表 DDL（PostgreSQL 方言）。
///
/// 与 [`SQLITE_SCHEMA`] 的差异仅有：`BLOB`→`BYTEA`；字节偏移列
/// （`total_bytes`/`downloaded_bytes`/`start_byte`/`end_byte`/
/// `speed_limit_kbps`/ed2k 数值列）用 `BIGINT`——pg 的 `INTEGER` 是
/// 4 字节，>2GB 下载会静默截断。
const POSTGRES_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    file_name TEXT NOT NULL,
    save_dir TEXT NOT NULL,
    status INTEGER NOT NULL DEFAULT 0,
    total_bytes BIGINT NOT NULL DEFAULT 0,
    downloaded_bytes BIGINT NOT NULL DEFAULT 0,
    segments INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    error_message TEXT NOT NULL DEFAULT '',
    proxy_url TEXT NOT NULL DEFAULT '',
    queue_id TEXT NOT NULL DEFAULT '',
    checksum TEXT NOT NULL DEFAULT '',
    ignore_tls_errors INTEGER NOT NULL DEFAULT 0,
    bt_selected_files TEXT NOT NULL DEFAULT '',
    bt_custom_name TEXT NOT NULL DEFAULT '',
    orig_etag TEXT NOT NULL DEFAULT '',
    orig_last_modified TEXT NOT NULL DEFAULT '',
    audio_url TEXT NOT NULL DEFAULT '',
    file_missing INTEGER NOT NULL DEFAULT 0,
    file_hash TEXT NOT NULL DEFAULT '',
    range_verified INTEGER NOT NULL DEFAULT 1,
    name_inferred INTEGER NOT NULL DEFAULT 0,
    resp_name TEXT NOT NULL DEFAULT '',
    resp_mime TEXT NOT NULL DEFAULT '',
    source_changed INTEGER NOT NULL DEFAULT 0,
    category_route TEXT NOT NULL DEFAULT '',
    request_method TEXT NOT NULL DEFAULT '',
    queue_order INTEGER NOT NULL DEFAULT 0,
    uploaded_bytes BIGINT NOT NULL DEFAULT 0,
    uploaded_at_completion BIGINT NOT NULL DEFAULT 0,
    seeding_status INTEGER NOT NULL DEFAULT 0,
    seeding_message TEXT NOT NULL DEFAULT '',
    seeding_started_at INTEGER NOT NULL DEFAULT 0,
    seeding_time_secs INTEGER NOT NULL DEFAULT 0,
    seed_ratio_limit_milli INTEGER NOT NULL DEFAULT -2,
    seed_post_ratio_limit_milli INTEGER NOT NULL DEFAULT -2,
    seed_time_limit_minutes INTEGER NOT NULL DEFAULT -2,
    seed_inactive_time_limit_minutes INTEGER NOT NULL DEFAULT -2,
    seed_upload_limit_bps BIGINT NOT NULL DEFAULT 0,
    seed_limits_exempt INTEGER NOT NULL DEFAULT 0,
    bt_sequential INTEGER NOT NULL DEFAULT 0,
    bt_first_last_pieces_first INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS task_segments (
    task_id TEXT NOT NULL,
    segment_index INTEGER NOT NULL,
    start_byte BIGINT NOT NULL,
    end_byte BIGINT NOT NULL,
    downloaded_bytes BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (task_id, segment_index),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS torrent_files (
    task_id TEXT PRIMARY KEY,
    file_bytes BYTEA NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS queues (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    speed_limit_kbps BIGINT NOT NULL DEFAULT 0,
    upload_limit_kbps BIGINT NOT NULL DEFAULT 0,
    max_concurrent INTEGER NOT NULL DEFAULT 0,
    default_save_dir TEXT NOT NULL DEFAULT '',
    position INTEGER NOT NULL DEFAULT 0,
    default_segments INTEGER NOT NULL DEFAULT 0,
    default_user_agent TEXT NOT NULL DEFAULT '',
    is_running INTEGER NOT NULL DEFAULT 1,
    schedule_enabled INTEGER NOT NULL DEFAULT 0,
    schedule_start TEXT NOT NULL DEFAULT '',
    schedule_stop TEXT NOT NULL DEFAULT '',
    schedule_days INTEGER NOT NULL DEFAULT 127
);
CREATE INDEX IF NOT EXISTS idx_task_segments_task_id ON task_segments(task_id);
CREATE TABLE IF NOT EXISTS ed2k_blocks (
    task_id TEXT NOT NULL,
    block_index BIGINT NOT NULL,
    state BIGINT NOT NULL DEFAULT 0,
    downloaded_bytes BIGINT NOT NULL DEFAULT 0,
    retry_count BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (task_id, block_index),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS ed2k_hashset (
    task_id TEXT PRIMARY KEY,
    hashes BYTEA NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS task_artifacts (
    task_id TEXT NOT NULL,
    file_name TEXT NOT NULL,
    PRIMARY KEY (task_id, file_name),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS task_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    source_url TEXT NOT NULL DEFAULT '',
    save_dir TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS link_devices (
    fingerprint TEXT PRIMARY KEY,
    identity_pub BYTEA NOT NULL,
    name TEXT NOT NULL,
    platform TEXT NOT NULL DEFAULT '',
    link_secret BYTEA NOT NULL,
    candidates TEXT NOT NULL DEFAULT '',
    paired_at BIGINT NOT NULL DEFAULT 0,
    last_seen_at BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS rss_sources (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL DEFAULT 'rss',
    provider_config TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    auto_download INTEGER NOT NULL DEFAULT 1,
    start_paused INTEGER NOT NULL DEFAULT 0,
    queue_id TEXT NOT NULL DEFAULT '',
    save_dir TEXT NOT NULL DEFAULT '',
    interval_minutes INTEGER NOT NULL DEFAULT 30,
    include_pattern TEXT NOT NULL DEFAULT '',
    exclude_pattern TEXT NOT NULL DEFAULT '',
    use_regex INTEGER NOT NULL DEFAULT 0,
    smart_episode INTEGER NOT NULL DEFAULT 0,
    size_min_bytes BIGINT NOT NULL DEFAULT 0,
    size_max_bytes BIGINT NOT NULL DEFAULT 0,
    send_referer INTEGER NOT NULL DEFAULT 1,
    notify_on_download INTEGER NOT NULL DEFAULT 1,
    max_per_fetch INTEGER NOT NULL DEFAULT 20,
    cookies TEXT NOT NULL DEFAULT '',
    user_agent TEXT NOT NULL DEFAULT '',
    proxy_url TEXT NOT NULL DEFAULT '',
    last_fetch_at BIGINT NOT NULL DEFAULT 0,
    last_success_at BIGINT NOT NULL DEFAULT 0,
    last_error TEXT NOT NULL DEFAULT '',
    fail_count INTEGER NOT NULL DEFAULT 0,
    seeded INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS rss_items (
    source_id TEXT NOT NULL,
    guid TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    link TEXT NOT NULL DEFAULT '',
    enclosure_url TEXT NOT NULL DEFAULT '',
    resolver_item TEXT NOT NULL DEFAULT '',
    enclosure_length BIGINT NOT NULL DEFAULT 0,
    pub_date BIGINT NOT NULL DEFAULT 0,
    fetched_at BIGINT NOT NULL DEFAULT 0,
    status INTEGER NOT NULL DEFAULT 0,
    task_id TEXT NOT NULL DEFAULT '',
    episode_key TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL DEFAULT '',
    enclosure_type TEXT NOT NULL DEFAULT '',
    fetch_failures BIGINT NOT NULL DEFAULT 0,
    retry_after BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (source_id, guid),
    FOREIGN KEY (source_id) REFERENCES rss_sources(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_rss_items_source ON rss_items(source_id, pub_date);
CREATE INDEX IF NOT EXISTS idx_rss_items_episode ON rss_items(source_id, episode_key);
CREATE TABLE IF NOT EXISTS webhook_deliveries (
    delivery_id TEXT PRIMARY KEY,
    timestamp_ms BIGINT NOT NULL DEFAULT 0,
    event TEXT NOT NULL DEFAULT '',
    endpoint_id TEXT NOT NULL DEFAULT '',
    endpoint_name TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    request_headers TEXT NOT NULL DEFAULT '',
    request_body TEXT NOT NULL DEFAULT '',
    status_code INTEGER NOT NULL DEFAULT 0,
    response_body TEXT NOT NULL DEFAULT '',
    latency_ms BIGINT NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    success INTEGER NOT NULL DEFAULT 0,
    error TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_ts ON webhook_deliveries(timestamp_ms);
CREATE TABLE IF NOT EXISTS task_activity (
    id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    timestamp_ms BIGINT NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    status INTEGER
);
CREATE INDEX IF NOT EXISTS idx_task_activity_task_id ON task_activity(task_id, id);
CREATE INDEX IF NOT EXISTS idx_task_activity_time ON task_activity(timestamp_ms);
CREATE TABLE IF NOT EXISTS task_activity_retention (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    pruned_through_id BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS task_speed_history (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    bucket_secs BIGINT NOT NULL,
    samples TEXT NOT NULL,
    pending_sum BIGINT NOT NULL,
    pending_secs BIGINT NOT NULL,
    active_secs BIGINT NOT NULL,
    transferred_bytes BIGINT NOT NULL,
    peak_bps BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS plugin_audit (
    id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    at_ms BIGINT NOT NULL,
    plugin_id TEXT NOT NULL,
    origin TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_plugin_audit_plugin ON plugin_audit(plugin_id, id);
-- 批量建任务幂等表（语义见 SQLITE_SCHEMA）。
CREATE TABLE IF NOT EXISTS task_create_batches (
    batch_id TEXT PRIMARY KEY,
    results TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_create_batches_created ON task_create_batches(created_at);
-- BT 防吸血（语义见 SQLITE_SCHEMA；字节列 BYTEA）。
CREATE TABLE IF NOT EXISTS bt_guard_entries (
    id BIGINT PRIMARY KEY,
    layer TEXT NOT NULL,
    cidr TEXT NOT NULL,
    action TEXT NOT NULL,
    rule TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL DEFAULT '',
    evidence TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    expires_at BIGINT
);
CREATE INDEX IF NOT EXISTS idx_bt_guard_entries_layer ON bt_guard_entries(layer, source);
CREATE TABLE IF NOT EXISTS bt_guard_strikes (
    ip_group TEXT PRIMARY KEY,
    strikes BIGINT NOT NULL,
    last_strike_at BIGINT NOT NULL,
    last_ban_until BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS bt_guard_ledger (
    info_hash TEXT NOT NULL,
    ip_group TEXT NOT NULL,
    delivered_bytes BIGINT NOT NULL,
    dup_bytes BIGINT NOT NULL,
    delivered_bits BYTEA NOT NULL,
    claimed_bits BYTEA NOT NULL,
    rerequests BIGINT NOT NULL,
    probed_at BIGINT NOT NULL DEFAULT 0,
    first_seen BIGINT NOT NULL,
    last_seen BIGINT NOT NULL,
    lag_cleared_at BIGINT NOT NULL DEFAULT 0,
    excess_baseline BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (info_hash, ip_group)
);
CREATE INDEX IF NOT EXISTS idx_bt_guard_ledger_seen ON bt_guard_ledger(last_seen);
CREATE TABLE IF NOT EXISTS bt_guard_events (
    id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    at BIGINT NOT NULL,
    kind TEXT NOT NULL,
    ip_group TEXT NOT NULL DEFAULT '',
    info_hash TEXT NOT NULL DEFAULT '',
    rule TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL DEFAULT '',
    action TEXT NOT NULL DEFAULT '',
    detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_bt_guard_events_group ON bt_guard_events(ip_group);
CREATE TABLE IF NOT EXISTS bt_guard_cloud_meta (
    id INTEGER PRIMARY KEY,
    data TEXT NOT NULL
);
";

/// SQLite 连接级 PRAGMA（在 after_connect 钩子中对每个新连接执行）。
/// foreign_keys=ON 是 sqlx-sqlite 的默认值，无需重复设置。
/// busy_timeout 让撞上写锁的连接在 5s 内自旋重试而非立即抛
/// SQLITE_BUSY（code 5, database is locked）——覆盖多任务并发落库 /
/// WAL checkpoint / 删除事务之间的瞬时写-写冲突。
/// synchronous=NORMAL（WAL 下的推荐档）：提交不再每次 fsync WAL，只在
/// checkpoint 时同步；进程崩溃不丢已提交事务，仅整机掉电可能丢失最近
/// 数秒的提交（数据库本身不会损坏）。下载进度/任务元数据可由续传重建，
/// 换来批量导入与进度落库的 fsync 开销归零。
/// cache_size（负值单位为 KiB）：23 万任务级别的库里 UUID 主键的 B 树随机访问，
/// 512KiB 页缓存几乎必然未命中并反复 pread；标准档 16MiB 足以容纳常用索引的热页。
/// 该值是「每连接」上限且按需增长（空库不预占），连接池上限 5 条；按本机有效内存
/// 分档（[`crate::mem_budget`]：<1.5GiB 取 4MiB、<3GiB 取 8MiB），在 after_connect
/// 中单独设置。
const SQLITE_PRAGMAS: &str = "PRAGMA journal_mode=WAL;\
 PRAGMA synchronous=NORMAL;\
 PRAGMA busy_timeout=5000;\
 PRAGMA temp_store=MEMORY;\
 PRAGMA mmap_size=0;\
 PRAGMA wal_autocheckpoint=1000;";
const POSTGRES_ENGINE_ADVISORY_LOCK: i64 = 0x464C_5558_444F_574E_i64;

/// 引擎唯一写入者租约。必须与 [`Db`] 一起保留到引擎完全关闭。
pub struct EngineWriteGuard {
    file: File,
    postgres_connection: Option<tokio::sync::Mutex<PoolConnection<Any>>>,
}

impl std::fmt::Debug for EngineWriteGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngineWriteGuard")
            .field("postgres", &self.postgres_connection.is_some())
            .finish_non_exhaustive()
    }
}

impl EngineWriteGuard {
    /// 校验写入者租约仍然有效，供宿主定期心跳调用。
    ///
    /// PostgreSQL 的 advisory lock 寿命等于持锁连接的寿命；连接被 LB/NAT/主备切换
    /// 断掉后，另一个实例即可拿到锁而本实例毫不知情。在持锁会话上重入加锁：
    /// 连接已断则 SQL 直接报错，会话仍在则重入成功（随即解一次锁保持计数平衡）。
    /// SQLite 后端的文件锁由内核随进程持有，无需校验。
    pub async fn verify_lease(&self) -> Result<(), DbError> {
        let Some(connection) = &self.postgres_connection else {
            return Ok(());
        };
        let mut connection = connection.lock().await;
        let still_held = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(POSTGRES_ENGINE_ADVISORY_LOCK)
            .fetch_one(&mut **connection)
            .await?;
        if !still_held {
            return Err(DbError::WriterLeaseHeld(
                "PostgreSQL advisory lock lost".to_owned(),
            ));
        }
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(POSTGRES_ENGINE_ADVISORY_LOCK)
            .execute(&mut **connection)
            .await?;
        Ok(())
    }
}

impl Drop for EngineWriteGuard {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            // Closing the file releases the lease even if explicit unlocking fails.
            crate::logger::report_warning("database", "unlock engine writer lease", &error);
        }
    }
}

fn sqlite_url(path: &Path) -> String {
    format!(
        "sqlite:{}?mode=rwc",
        path.to_string_lossy().replace('\\', "/")
    )
}

fn acquire_engine_file_lease(data_dir: &Path) -> Result<EngineWriteGuard, DbError> {
    std::fs::create_dir_all(data_dir)?;
    let lock_path: PathBuf = data_dir.join("engine.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    if let Err(error) = file.try_lock_exclusive() {
        // Windows 上 fs2 返回 ERROR_LOCK_VIOLATION，std 不把它映射为 WouldBlock；
        // 与 fs2 自身的争用错误码比较才能识别「已有写入者」。
        let contended = error.kind() == std::io::ErrorKind::WouldBlock
            || (error.raw_os_error().is_some()
                && error.raw_os_error() == fs2::lock_contended_error().raw_os_error());
        if contended {
            return Err(DbError::WriterLeaseHeld(lock_path.display().to_string()));
        }
        return Err(DbError::Io(error));
    }
    Ok(EngineWriteGuard {
        file,
        postgres_connection: None,
    })
}

#[derive(Clone)]
pub struct Db {
    pool: sqlx::AnyPool,
    backend: Backend,
    /// CDN 上传租约已知为空的内存缓存，空闲 peek 据此免读数据库。
    cdn_lease_known_empty: Arc<AtomicBool>,
    activity_lock: Arc<tokio::sync::Mutex<()>>,
    /// 任务整表快照「读库 + 发出」与终态进度帧发出之间的顺序门，见
    /// [`Self::lock_event_order`]。
    event_order_gate: Arc<tokio::sync::Mutex<()>>,
    /// 下载中任务的速度历史（进度报告器与 daemon 查询共享），见 [`crate::speed_history`]。
    speed_history_live: Arc<crate::speed_history::SpeedHistoryLive>,
}

/// 把 `AnyRow` 手动映射为 [`TaskInfo`]（列名 `id`→字段 `task_id`）。
///
/// 迁移新增列（`proxy_url`/`queue_id`/`checksum`/`file_missing`）用防御性
/// `unwrap_or_default`/`unwrap_or`，与既有字段风格一致；运行路径下这些列已由
/// `add_column_if_missing` 补齐。
fn task_from_row(row: &AnyRow) -> Result<TaskInfo, sqlx::Error> {
    Ok(TaskInfo {
        task_id: row.try_get("id")?,
        url: row.try_get("url")?,
        file_name: row.try_get("file_name")?,
        save_dir: row.try_get("save_dir")?,
        status: row.try_get("status")?,
        downloaded_bytes: row.try_get("downloaded_bytes")?,
        total_bytes: row.try_get("total_bytes")?,
        error_message: row.try_get("error_message")?,
        created_at: row.try_get("created_at")?,
        proxy_url: row.try_get("proxy_url").unwrap_or_default(),
        queue_id: row.try_get("queue_id").unwrap_or_default(),
        checksum: row.try_get("checksum").unwrap_or_default(),
        ignore_tls_errors: row.try_get::<i32, _>("ignore_tls_errors").unwrap_or(0) != 0,
        file_missing: row.try_get::<i32, _>("file_missing").unwrap_or(0) != 0,
        completed_at: row.try_get("completed_at").unwrap_or_default(),
        file_hash: row.try_get("file_hash").unwrap_or_default(),
        segments: row.try_get("segments").unwrap_or(0),
        queue_order: row.try_get("queue_order").unwrap_or(0),
        uploaded_bytes: row.try_get("uploaded_bytes").unwrap_or_default(),
        uploaded_at_completion: row.try_get("uploaded_at_completion").unwrap_or_default(),
        seeding_status: row.try_get("seeding_status").unwrap_or_default(),
        seeding_message: row.try_get("seeding_message").unwrap_or_default(),
        seeding_time_secs: row.try_get("seeding_time_secs").unwrap_or_default(),
        seed_ratio_limit_milli: row.try_get("seed_ratio_limit_milli").unwrap_or(-2),
        seed_post_ratio_limit_milli: row.try_get("seed_post_ratio_limit_milli").unwrap_or(-2),
        seed_time_limit_minutes: row.try_get("seed_time_limit_minutes").unwrap_or(-2),
        seed_inactive_time_limit_minutes: row
            .try_get("seed_inactive_time_limit_minutes")
            .unwrap_or(-2),
        seed_upload_limit_bps: row.try_get("seed_upload_limit_bps").unwrap_or(0),
        bt_sequential: row.try_get::<i32, _>("bt_sequential").unwrap_or(0) != 0,
        bt_first_last_pieces_first: row
            .try_get::<i32, _>("bt_first_last_pieces_first")
            .unwrap_or(0)
            != 0,
        referrer: row.try_get("referrer").unwrap_or_default(),
        group_id: row.try_get("group_id").unwrap_or_default(),
        rss_source_id: row.try_get("rss_source_id").unwrap_or_default(),
        origin_url: row.try_get("origin_url").unwrap_or_default(),
        auto_route: row.try_get("auto_route").unwrap_or_default(),
        source_bytes: SourceBytes {
            cdn: row.try_get("src_cdn_bytes").unwrap_or_default(),
            proxy: row.try_get("src_proxy_bytes").unwrap_or_default(),
            nic: row.try_get("src_nic_bytes").unwrap_or_default(),
        },
    })
}

const TASK_COLUMNS: &str = "id, url, file_name, save_dir, status, downloaded_bytes, total_bytes, error_message, created_at, proxy_url, queue_id, checksum, ignore_tls_errors, file_missing, completed_at, file_hash, segments, queue_order, uploaded_bytes, uploaded_at_completion, seeding_status, seeding_message, seeding_time_secs, seed_ratio_limit_milli, seed_post_ratio_limit_milli, seed_time_limit_minutes, seed_inactive_time_limit_minutes, seed_upload_limit_bps, bt_sequential, bt_first_last_pieces_first, referrer, group_id, rss_source_id, origin_url, auto_route, src_cdn_bytes, src_proxy_bytes, src_nic_bytes";

/// 新建任务的完整落库行（[`Db::insert_new_tasks`]）。
///
/// 一行 INSERT 携带创建时已知的全部列，取代「INSERT + 多条 UPDATE」；
/// 字段全部借用调用方的准备结果，避免批量创建时逐字段复制。
pub struct NewTaskRow<'a> {
    pub id: &'a str,
    pub url: &'a str,
    pub file_name: &'a str,
    /// 文件名来源（见 `name_inferred` 迁移注释）。
    pub name_inferred: bool,
    pub save_dir: &'a str,
    pub segments: i32,
    /// 初始状态：0 pending / 2 paused / 4 error（创建即失败）。
    pub status: i32,
    pub error_message: &'a str,
    pub proxy_url: &'a str,
    pub queue_id: &'a str,
    pub checksum: &'a str,
    pub ignore_tls_errors: bool,
    pub cookies: &'a str,
    pub referrer: &'a str,
    /// 请求头 JSON；空串 = 无。
    pub extra_headers_json: &'a str,
    pub origin_url: &'a str,
    pub audio_url: &'a str,
    pub resolver_plugin_id: &'a str,
    /// 多个插件同时匹配且站点无默认插件：首次起跑时询问用户（`resolver_choice_pending`）。
    pub resolver_choice_pending: bool,
    pub group_id: &'a str,
    pub resolver_item: &'a str,
    pub unattended: bool,
    /// `bt_selected_files` 列的编码值（`""` / `"all"` / `"0,2,5"`）。
    pub bt_selected_files: &'a str,
    /// 分类目录路由列值（`CategoryRoute` JSON；空串 = 不路由）。
    pub category_route: &'a str,
    /// 浏览器捕获的非 GET 请求方法（`POST` 等）；空串 = GET。请求体不落库。
    pub request_method: &'a str,
    /// `.torrent` 字节；非空时同事务写入 `torrent_files`。
    pub torrent_file_bytes: &'a [u8],
    /// BT 选片（新任务按设置默认值落库）：按顺序下载 / 先下载首尾文件块。
    pub bt_sequential: bool,
    pub bt_first_last_pieces_first: bool,
}

/// 批量建任务的幂等记录（[`Db::insert_new_tasks`] 与任务行同事务写入）。
pub struct CreateBatchRecord<'a> {
    pub batch_id: &'a str,
    /// 序列化后的逐项结果。
    pub results: &'a str,
}

/// 批量建任务幂等记录的保留期（秒）。过期记录在写入新记录的同一事务中清理，
/// 不设周期任务（空闲零唤醒）。
const CREATE_BATCH_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;

/// 文件跟踪扫描的最小任务投影（[`Db::load_file_tracking_rows`]）。扫描只需
/// 要判定「目标路径是否被活跃任务占用」和「已完成任务的产物是否还在盘上」，
/// 走 [`TASK_COLUMNS`] 的全列反序列化在几万任务规模下是纯浪费。
#[derive(Debug, Clone)]
pub struct FileTrackingRow {
    pub task_id: String,
    pub save_dir: String,
    pub file_name: String,
    /// 库中当前的「文件已丢失」标志，用于只上报真正的边沿变化。
    pub file_missing: bool,
    /// 0/1/5 = 活跃（占用目标路径），3 = 已完成（待探测）。
    pub status: i32,
}

/// 把 `AnyRow` 映射为 [`GroupInfo`]。
fn group_from_row(row: &AnyRow) -> Result<GroupInfo, sqlx::Error> {
    Ok(GroupInfo {
        group_id: row.try_get("id")?,
        name: row.try_get("name")?,
        source_url: row.try_get("source_url").unwrap_or_default(),
        save_dir: row.try_get("save_dir").unwrap_or_default(),
        created_at: row.try_get("created_at")?,
    })
}

/// [`Db::fission_into_group`] 的单个兄弟任务描述（清单中母任务之外的条目）。
#[derive(Debug, Clone, Default)]
pub struct GroupSiblingSpec {
    pub id: String,
    pub file_name: String,
    pub save_dir: String,
    pub resolver_item: String,
    pub total_bytes: i64,
    /// 0=pending（正常裂变，随后按容量 start/入队）或 2=paused（清单总大小
    /// 超阈值静默转 paused，见 `FISSION_AUTO_START_MAX_TOTAL_BYTES`）。
    pub status: i32,
}

/// [`Db::fission_into_group`] 的裂变请求：组行字段 + 母任务改写字段 + 兄弟
/// 任务列表。母任务的队列/代理/TLS 策略/分段数/resolver 插件绑定/请求上下文
/// （cookies/referrer/extra_headers）由 `fission_into_group` 内部从 DB 读回
/// 并复制给每个兄弟任务，无需在此重复携带。
#[derive(Debug, Clone, Default)]
pub struct FissionSpec {
    pub group_id: String,
    pub group_name: String,
    pub group_save_dir: String,
    pub group_source_url: String,
    pub mother_task_id: String,
    pub mother_resolver_item: String,
    pub mother_file_name: String,
    pub mother_save_dir: String,
    pub mother_total_bytes: i64,
    pub mother_status: i32,
    pub siblings: Vec<GroupSiblingSpec>,
}

/// 全库最多保留五万条活动和七天历史，两条约束都在写事务内执行。
const TASK_ACTIVITY_MAX_ROWS: i64 = 50_000;
const TASK_ACTIVITY_MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 容量清理的滞回比例分母：行数超过 `max_rows + max_rows / 10` 才触发，
/// 触发后一次性清回 `max_rows`。避免到达上限后每条新增都付一次整表偏移扫描。
const TASK_ACTIVITY_PRUNE_SLACK_DIVISOR: i64 = 10;

async fn prune_task_activity(
    tx: &mut sqlx::Transaction<'_, Any>,
    cutoff_ms: i64,
    max_rows: i64,
) -> Result<(), DbError> {
    let high_water = max_rows.saturating_add(max_rows / TASK_ACTIVITY_PRUNE_SLACK_DIVISOR);
    // 容量阈值：0 表示不做容量清理（id 自增且 > 0，`id <= 0` 恒假）。
    // 行数 <= id 跨度 + 1，跨度未达高水位时必然未超限，省掉偏移扫描。
    // MAX 与 MIN 分成两个子查询：SQLite 只对单独的 min()/max() 走主键 O(log n)
    // 优化，写在同一个 SELECT 里会退化为整表扫描（每次追加都在写锁内付一次）。
    let (span, oldest_ms): (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT (SELECT MAX(id) FROM task_activity) - (SELECT MIN(id) FROM task_activity), \
         (SELECT MIN(timestamp_ms) FROM task_activity)",
    )
    .fetch_one(&mut **tx)
    .await?;
    let mut capacity_threshold = 0_i64;
    if span.is_some_and(|span| span >= high_water) {
        let over: Option<i64> =
            sqlx::query_scalar("SELECT id FROM task_activity ORDER BY id DESC LIMIT 1 OFFSET $1")
                .bind(high_water)
                .fetch_optional(&mut **tx)
                .await?;
        if over.is_some() {
            let keep_boundary: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM task_activity ORDER BY id DESC LIMIT 1 OFFSET $1",
            )
            .bind(max_rows)
            .fetch_optional(&mut **tx)
            .await?;
            capacity_threshold = keep_boundary.unwrap_or(0);
        }
    }
    // 既无过期行也未超容量：常见情形直接返回，不在写锁内跑带 OR 的聚合 / 删除扫描。
    if capacity_threshold == 0 && oldest_ms.is_none_or(|oldest| oldest >= cutoff_ms) {
        return Ok(());
    }
    // 先按 task_id 聚合记录被清理的最大 id，再删除；同一条件两处使用，保证一致。
    sqlx::query(
        "INSERT INTO task_activity_retention (task_id, pruned_through_id) \
         SELECT task_id, MAX(id) FROM task_activity \
         WHERE timestamp_ms < $1 OR id <= $2 GROUP BY task_id \
         ON CONFLICT(task_id) DO UPDATE SET pruned_through_id = \
         CASE WHEN excluded.pruned_through_id > task_activity_retention.pruned_through_id \
              THEN excluded.pruned_through_id ELSE task_activity_retention.pruned_through_id END",
    )
    .bind(cutoff_ms)
    .bind(capacity_threshold)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM task_activity WHERE timestamp_ms < $1 OR id <= $2")
        .bind(cutoff_ms)
        .bind(capacity_threshold)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// 插件审计保留策略：90 天与最近 2 万条两条约束同时生效。
#[cfg(feature = "plugins")]
const PLUGIN_AUDIT_MAX_AGE_MS: i64 = 90 * 24 * 60 * 60 * 1000;
#[cfg(feature = "plugins")]
const PLUGIN_AUDIT_MAX_ROWS: i64 = 20_000;

#[cfg(feature = "plugins")]
impl Db {
    /// 单事务批量写入插件审计行（`id` 由数据库分配，入参中的 `id` 被忽略）。
    pub async fn insert_plugin_audit(
        &self,
        rows: &[crate::plugin::audit::PluginAuditRow],
    ) -> Result<(), DbError> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for row in rows {
            sqlx::query(
                "INSERT INTO plugin_audit (at_ms, plugin_id, origin, action, target, outcome, detail) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(row.at_ms)
            .bind(&row.plugin_id)
            .bind(&row.origin)
            .bind(&row.action)
            .bind(&row.target)
            .bind(&row.outcome)
            .bind(&row.detail)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 按 id 降序分页读取审计；`plugin_id` 为空表示全部插件。
    pub async fn list_plugin_audit(
        &self,
        plugin_id: Option<&str>,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<crate::plugin::audit::PluginAuditRow>, DbError> {
        let before = before_id.unwrap_or(i64::MAX);
        let limit = i64::from(limit);
        let rows = match plugin_id {
            Some(plugin_id) => {
                sqlx::query(
                    "SELECT id, at_ms, plugin_id, origin, action, target, outcome, detail \
                     FROM plugin_audit WHERE plugin_id = $1 AND id < $2 ORDER BY id DESC LIMIT $3",
                )
                .bind(plugin_id)
                .bind(before)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, at_ms, plugin_id, origin, action, target, outcome, detail \
                     FROM plugin_audit WHERE id < $1 ORDER BY id DESC LIMIT $2",
                )
                .bind(before)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.into_iter()
            .map(|row| {
                Ok(crate::plugin::audit::PluginAuditRow {
                    id: row.try_get("id")?,
                    at_ms: row.try_get("at_ms")?,
                    plugin_id: row.try_get("plugin_id")?,
                    origin: row.try_get("origin")?,
                    action: row.try_get("action")?,
                    target: row.try_get("target")?,
                    outcome: row.try_get("outcome")?,
                    detail: row.try_get("detail")?,
                })
            })
            .collect()
    }

    /// 清空审计；`plugin_id` 为空表示全部。
    pub async fn clear_plugin_audit(&self, plugin_id: Option<&str>) -> Result<(), DbError> {
        match plugin_id {
            Some(plugin_id) => {
                sqlx::query("DELETE FROM plugin_audit WHERE plugin_id = $1")
                    .bind(plugin_id)
                    .execute(&self.pool)
                    .await?;
            }
            None => {
                sqlx::query("DELETE FROM plugin_audit")
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(())
    }

    /// 删除 90 天前以及超出最近 2 万条的审计行。
    pub async fn prune_plugin_audit(&self, now_ms: i64) -> Result<(), DbError> {
        sqlx::query(
            "DELETE FROM plugin_audit WHERE at_ms < $1 OR id <= COALESCE(\
             (SELECT id FROM plugin_audit ORDER BY id DESC LIMIT 1 OFFSET $2), 0)",
        )
        .bind(now_ms - PLUGIN_AUDIT_MAX_AGE_MS)
        .bind(PLUGIN_AUDIT_MAX_ROWS)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

impl Db {
    /// 异步持久化一条源端活动；主键由数据库 sequence 分配，跨重启单调。
    /// 删除中的任务不会产生孤儿行。事务同时执行七日和全库容量清理。
    pub async fn append_task_activity(
        &self,
        activity: &crate::task_activity::TaskActivity,
    ) -> Result<Option<crate::task_activity::TaskActivity>, DbError> {
        Ok(self
            .append_task_activities(std::slice::from_ref(activity))
            .await?
            .pop()
            .flatten())
    }

    /// 批量持久化源端活动：一个事务内按入参顺序逐条插入（ID 随之递增）、只裁剪一次。
    /// 返回与入参一一对应的结果：任务已删除（不产生孤儿行）的项为 `None`。
    /// 整个批次要么全部提交要么全部回滚。
    pub async fn append_task_activities(
        &self,
        activities: &[crate::task_activity::TaskActivity],
    ) -> Result<Vec<Option<crate::task_activity::TaskActivity>>, DbError> {
        self.append_task_activities_pruning(
            activities,
            crate::task_activity::timestamp_ms() - TASK_ACTIVITY_MAX_AGE_MS,
            TASK_ACTIVITY_MAX_ROWS,
        )
        .await
    }

    /// [`Self::append_task_activities`] 的保留参数显式版本（测试用小容量覆盖裁剪边界）。
    async fn append_task_activities_pruning(
        &self,
        activities: &[crate::task_activity::TaskActivity],
        cutoff_ms: i64,
        max_rows: i64,
    ) -> Result<Vec<Option<crate::task_activity::TaskActivity>>, DbError> {
        if activities.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = self.activity_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let mut saved = Vec::with_capacity(activities.len());
        let mut inserted_any = false;
        for activity in activities {
            let id: Option<i64> = sqlx::query_scalar(
                "INSERT INTO task_activity (task_id, timestamp_ms, kind, message, status) \
                 SELECT $1, $2, $3, $4, $5 WHERE EXISTS (SELECT 1 FROM tasks WHERE id = $1) RETURNING id",
            )
            .bind(&activity.task_id)
            .bind(activity.timestamp_ms)
            .bind(&activity.kind)
            .bind(&activity.message)
            .bind(activity.status)
            .fetch_optional(&mut *tx)
            .await?;
            inserted_any |= id.is_some();
            saved.push(id.map(|id| crate::task_activity::TaskActivity {
                id,
                ..activity.clone()
            }));
        }
        if inserted_any {
            prune_task_activity(&mut tx, cutoff_ms, max_rows).await?;
        }
        tx.commit().await?;
        Ok(saved)
    }

    /// 按任务分页。最新页和 before 向前分页均先查 DESC 再升序返回；
    /// after 直接 ASC，断线补齐不会漏掉页内中间的活动。
    pub async fn query_task_activity(
        &self,
        query: &crate::task_activity::TaskActivityQuery,
    ) -> Result<crate::task_activity::TaskActivityPage, DbError> {
        use crate::task_activity::{TaskActivity, TaskActivityPage};
        if query.before_id.is_some() && query.after_id.is_some() {
            return Err(DbError::InvalidActivityQuery(
                "beforeId and afterId are mutually exclusive",
            ));
        }
        if query.before_id.is_some_and(|id| id <= 0) || query.after_id.is_some_and(|id| id < 0) {
            return Err(DbError::InvalidActivityQuery(
                "cursor must be a positive ID (afterId may be zero)",
            ));
        }
        if query.task_id.is_empty() {
            return Err(DbError::InvalidActivityQuery("taskId is required"));
        }
        let limit = if query.limit == 0 {
            100
        } else {
            query.limit.min(500)
        } as i64;
        let _guard = self.activity_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        prune_task_activity(
            &mut tx,
            crate::task_activity::timestamp_ms() - TASK_ACTIVITY_MAX_AGE_MS,
            TASK_ACTIVITY_MAX_ROWS,
        )
        .await?;
        let (oldest_id, newest_id): (Option<i64>, Option<i64>) = {
            let row = sqlx::query(
                "SELECT MIN(id) AS oldest, MAX(id) AS newest FROM task_activity WHERE task_id = $1",
            )
            .bind(&query.task_id)
            .fetch_one(&mut *tx)
            .await?;
            (row.try_get("oldest")?, row.try_get("newest")?)
        };
        let pruned_through: Option<i64> = sqlx::query_scalar(
            "SELECT pruned_through_id FROM task_activity_retention WHERE task_id = $1",
        )
        .bind(&query.task_id)
        .fetch_optional(&mut *tx)
        .await?;
        let rows = if let Some(after) = query.after_id {
            sqlx::query("SELECT id, task_id, timestamp_ms, kind, message, status FROM task_activity WHERE task_id = $1 AND id > $2 ORDER BY id ASC LIMIT $3")
                .bind(&query.task_id).bind(after).bind(limit + 1).fetch_all(&mut *tx).await?
        } else {
            sqlx::query("SELECT id, task_id, timestamp_ms, kind, message, status FROM task_activity WHERE task_id = $1 AND id < $2 ORDER BY id DESC LIMIT $3")
                .bind(&query.task_id).bind(query.before_id.unwrap_or(i64::MAX)).bind(limit + 1).fetch_all(&mut *tx).await?
        };
        let has_more = rows.len() > limit as usize;
        let mut entries: Vec<TaskActivity> = rows
            .into_iter()
            .take(limit as usize)
            .map(|row| {
                Ok(TaskActivity {
                    id: row.try_get("id")?,
                    task_id: row.try_get("task_id")?,
                    timestamp_ms: row.try_get("timestamp_ms")?,
                    kind: row.try_get("kind")?,
                    message: row.try_get("message")?,
                    status: row.try_get("status")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()?;
        if query.after_id.is_none() {
            entries.reverse();
        }
        tx.commit().await?;
        Ok(TaskActivityPage {
            entries,
            has_more,
            oldest_id,
            newest_id,
            truncated: pruned_through.is_some(),
        })
    }

    /// 在 `dir` 目录下打开（不存在则创建）SQLite 数据库 `flux_down.db`。
    ///
    /// 桌面 App 的默认持久化路径；服务器端可改用 [`Db::connect`] 按 URL
    /// 连接 SQLite 或 PostgreSQL。
    pub async fn open(dir: &Path) -> Result<Self, DbError> {
        let db_path = dir.join("flux_down.db");
        let url = sqlite_url(&db_path);
        Self::connect(&url).await
    }

    /// 打开 SQLite 数据库并在任何连接或 schema 变更前获取唯一写入者文件租约。
    pub async fn open_exclusive(dir: &Path) -> Result<(Self, EngineWriteGuard), DbError> {
        let guard = acquire_engine_file_lease(dir)?;
        let db_path = dir.join("flux_down.db");
        let url = sqlite_url(&db_path);
        let db = Self::connect_uninitialized(&url).await?;
        db.init_schema().await?;
        Ok((db, guard))
    }

    /// 按连接 URL 打开数据库。
    ///
    /// - `sqlite:/path/to/db?mode=rwc` / `sqlite::memory:` → SQLite
    /// - `postgres://user:pass@host:5432/db` → PostgreSQL
    pub async fn connect(url: &str) -> Result<Self, DbError> {
        let db = Self::connect_uninitialized(url).await?;
        db.init_schema().await?;
        Ok(db)
    }

    /// 获取本机文件租约；PostgreSQL 还会在 schema 初始化前获取全局 advisory lock。
    pub async fn connect_exclusive(
        url: &str,
        data_dir: &Path,
    ) -> Result<(Self, EngineWriteGuard), DbError> {
        let mut guard = acquire_engine_file_lease(data_dir)?;
        let db = Self::connect_uninitialized(url).await?;
        if db.backend == Backend::Postgres {
            let mut connection = db.pool.acquire().await?;
            let acquired = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
                .bind(POSTGRES_ENGINE_ADVISORY_LOCK)
                .fetch_one(&mut *connection)
                .await?;
            if !acquired {
                return Err(DbError::WriterLeaseHeld(
                    "PostgreSQL advisory lock".to_owned(),
                ));
            }
            connection.close_on_drop();
            guard.postgres_connection = Some(tokio::sync::Mutex::new(connection));
        }
        db.init_schema().await?;
        Ok((db, guard))
    }

    async fn connect_uninitialized(url: &str) -> Result<Self, DbError> {
        sqlx::any::install_default_drivers();
        let backend = Backend::from_url(url)?;
        let max_connections = if backend == Backend::Sqlite && url.contains(":memory:") {
            1
        } else {
            5
        };
        let mut options = AnyPoolOptions::new().max_connections(max_connections);
        if backend == Backend::Sqlite {
            // 常驻一条连接且不回收：最后一条连接关闭会触发 checkpoint 并删除
            // -wal/-shm，之后下次查询重建，空闲时周期性唤醒 NAS 硬盘。
            options = options
                .min_connections(1)
                .idle_timeout(None)
                .max_lifetime(None);
        }
        let pool = options
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    if conn.backend_name() == "SQLite" {
                        sqlx::raw_sql(SQLITE_PRAGMAS).execute(&mut *conn).await?;
                        let cache = format!(
                            "PRAGMA cache_size=-{};",
                            crate::mem_budget::current().sqlite_cache_kib
                        );
                        sqlx::raw_sql(AssertSqlSafe(cache))
                            .execute(&mut *conn)
                            .await?;
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        Ok(Self {
            pool,
            backend,
            activity_lock: Arc::new(tokio::sync::Mutex::new(())),
            event_order_gate: Arc::new(tokio::sync::Mutex::new(())),
            cdn_lease_known_empty: Arc::new(AtomicBool::new(false)),
            speed_history_live: Arc::default(),
        })
    }

    /// 供同 crate 的子系统模块（如 [`crate::speed_history`]）执行自有表的查询。
    pub(crate) fn pool(&self) -> &sqlx::AnyPool {
        &self.pool
    }

    pub(crate) fn speed_history_live(&self) -> &crate::speed_history::SpeedHistoryLive {
        &self.speed_history_live
    }

    /// 获取事件顺序门。同一数据库句柄（含所有克隆）共用一把门。
    ///
    /// 整表快照先读库、后发帧；任务完成则是先落库 `status=3`、后发终态进度帧。
    /// 两者并发时，读库早于落库的快照可能晚于终态帧发出，把已完成的行改回
    /// 「下载中」（该任务之后没有新快照时会永久停在旧状态）。
    ///
    /// 约定：快照在持门期间完成「读库 + 发出」；进度汇报器发非活跃态
    /// （完成 / 失败 / 暂停）帧前先取门；actor 之外先落库再读库发帧的路径
    /// （如做种状态帧）同样在持门期间「读库 + 发出」。这样落库发生在快照读取之后的任务，
    /// 其终态帧必然排在该快照之后；落库先于读取的任务，快照读到的已是新状态。
    /// 持门期间不得等待进度汇报器。
    pub async fn lock_event_order(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.event_order_gate.lock().await
    }

    async fn init_schema(&self) -> Result<(), DbError> {
        let schema = match self.backend {
            Backend::Sqlite => SQLITE_SCHEMA,
            Backend::Postgres => POSTGRES_SCHEMA,
        };
        sqlx::raw_sql(schema).execute(&self.pool).await?;

        // --- Schema migrations（幂等，只为升级旧库；新库建表已含全量列） ---
        // bt_guard：strike 衰减改按「最近一次封禁到期时刻」起算。
        self.add_column_if_missing(
            "bt_guard_strikes",
            "last_ban_until",
            "BIGINT NOT NULL DEFAULT 0",
        )
        .await?;
        // bt_guard 账本：lag 清除时刻 / excessive 封禁基线。
        self.add_column_if_missing(
            "bt_guard_ledger",
            "lag_cleared_at",
            "BIGINT NOT NULL DEFAULT 0",
        )
        .await?;
        self.add_column_if_missing(
            "bt_guard_ledger",
            "excess_baseline",
            "BIGINT NOT NULL DEFAULT 0",
        )
        .await?;
        // 订阅 provider 扩展：旧 RSS 源默认归入内置 `rss` provider。
        self.add_column_if_missing("rss_sources", "provider_id", "TEXT NOT NULL DEFAULT 'rss'")
            .await?;
        self.add_column_if_missing("rss_sources", "provider_config", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("rss_items", "resolver_item", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("rss_items", "enclosure_type", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("rss_items", "fetch_failures", "BIGINT NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("rss_items", "retry_after", "BIGINT NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("tasks", "proxy_url", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "queue_id", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("queues", "default_segments", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("tasks", "checksum", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // 单任务 TLS 策略：旧任务安全迁移为严格验证（0）。
        self.add_column_if_missing("tasks", "ignore_tls_errors", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("queues", "default_user_agent", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "bt_selected_files", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "bt_custom_name", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "orig_etag", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "orig_last_modified", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "file_missing", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("tasks", "audio_url", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // 任务请求上下文（cookies/referrer/extra_headers JSON）持久化：resume
        // 时恢复鉴权上下文。鉴权站点（cookie+token 双因子的 fnOS、带
        // Authorization 的私有服务）没有它们 resume 必然 4xx。
        self.add_column_if_missing("tasks", "cookies", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "referrer", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "extra_headers", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // Range 能力验证标记：hint 任务（跳过 probe、Range 未验证）建任务后置
        // 0，首响应证实支持（206/Accept-Ranges）时置回 1。resume 读取它决定
        // 是否延续「首连接 plain GET」保守启动（配额型端点对 bounded Range
        // 一律 400 且作废 token，resume 若落回默认 probe 会重新烧毁 token）。
        // 默认 1 = 旧任务/probe 任务行为完全不变。
        self.add_column_if_missing("tasks", "range_verified", "INTEGER NOT NULL DEFAULT 1")
            .await?;
        // 文件名来源：1 = 引擎自行推断（探测所得但非 Content-Disposition、URL 兜底），
        // HTTP 完成期可按实际响应证据精修；0 = 显式名 / CD 名 / 用户改名，主名永不改写，
        // 仅缺类型扩展名时按响应证据补扩展名。
        self.add_column_if_missing("tasks", "name_inferred", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        // 实际 GET 响应观察到的命名证据（CD 解出的名字 / Content-Type），先写者胜。
        self.add_column_if_missing("tasks", "resp_name", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "resp_mime", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // 插件惰性解析：仅存 resolver 插件 ID（不存解析结果，见 plugin 系统设计）。
        self.add_column_if_missing("tasks", "resolver_plugin_id", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // 段行布局属主令牌（spawn generation）：每次多段下载 spawn 起飞时先写入
        // 自己的 generation，worker 段进度写入以它作存在性守卫——快速
        // pause→resume 后旧 spawn 迟到的写入（含 start_byte 恒 0 的段 0）全类
        // 失效，彻底关闭"迟到写落到重建后段行"的静默空洞窗口。进程内单调
        // （DownloadManager.generation），跨进程无需单调（旧进程已死）。
        self.add_column_if_missing("tasks", "segments_epoch", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        // 任务结束时间（Unix 秒，字符串；空 = 尚未完成）。仅记录下载真正
        // 完成（status→3）的时刻，插件 onDone 等 hook 后处理不计入；任务
        // 重新开始下载（status→0/1/5）时清空，供重下后重新记录。
        self.add_column_if_missing("tasks", "completed_at", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // 落盘文件的哈希记录（`algo=hex`，空 = 未计算）；维护规则见 `update_task_status`。
        self.add_column_if_missing("tasks", "file_hash", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // 队列内启动顺序（0 = 未显式排序，按 created_at 先来先启动）。
        self.add_column_if_missing("tasks", "queue_order", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        // 新任务追加顺序与队列内排序共用覆盖索引；否则每次 MAX 都会扫描
        // 已有任务，大量逐条导入会在数据库层退化为 O(N²)。
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_tasks_queue_order ON tasks(queue_id, queue_order)",
        )
        .execute(&self.pool)
        .await?;
        // 队列启停状态与每日定时计划（IDM 式队列控制）。
        self.add_column_if_missing("queues", "is_running", "INTEGER NOT NULL DEFAULT 1")
            .await?;
        self.add_column_if_missing("queues", "schedule_enabled", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("queues", "schedule_start", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("queues", "schedule_stop", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("queues", "schedule_days", "INTEGER NOT NULL DEFAULT 127")
            .await?;
        // 多文件任务组：group_id 关联 task_groups.id（空 = 不属于任何组）；
        // resolver_item 为二段解析标识（不透明字符串，专用 getter，不进 TaskInfo）。
        self.add_column_if_missing("tasks", "group_id", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "resolver_item", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // RSS 订阅溯源：任务由哪条订阅自动创建（空 = 非 RSS 来源）。
        // 反向回链在 `rss_items.task_id`，两侧都有是为了任一侧单独查询都
        // 不必扫另一张表（任务详情「来源」行 / 条目流「已下载」跳转）。
        self.add_column_if_missing("tasks", "rss_source_id", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // 展示用原始来源链接。`.torrent` 文件任务的 `url` 是本地哨兵,
        // 右键「复制下载链接」拿到的是 `torrent-file://local` 这种噪音;
        // 有真实来源(RSS enclosure 直链)时写这里。空 = 回退 `url`。
        self.add_column_if_missing("tasks", "origin_url", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // ProxyMode::Auto 的任务级最终链路（可追溯性）：wire 标签见
        // `auto_proxy::route`（direct / direct:sampled / direct:pinned /
        // direct:failover / proxy:cached / proxy:sampled / proxy:failover）。
        // 空 = 非 Auto 模式或任务从未启动。每次任务启动时由 manager 重写，
        // 运行中主导链路变化由 coordinator 侧多路径调度更新。
        self.add_column_if_missing("tasks", "auto_route", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // 无人值守创建标记（外部接管/RSS 等自动化入口 + 「免打扰跳过二次选择」
        // 开启时置 1）：start/resume 读它决定 HLS/DASH 画质与插件变体选择是否
        // 跳过 HostSelection 弹窗、直接取默认值。BT 文件选择不读此列——建任务
        // 时已按「全部文件」写 bt_selected_files（既有三态语义，见 create_task）。
        self.add_column_if_missing("tasks", "unattended", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        // 多插件冲突待选标记：建任务时多个已启用插件的 resolver 同时匹配且站点无可用默认
        // 插件 → 1，resolver_plugin_id 先绑回退插件；首次 resolve 前询问用户（无人值守
        // 按站点记录 / 回退插件）后改绑并清 0。任何显式改绑（set_task_resolver）同样清 0。
        self.add_column_if_missing(
            "tasks",
            "resolver_choice_pending",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        // 文件已存在询问的逐任务决定（'' / rename / overwrite / skip）：
        // 绑定「询问时的文件名」语义由 manager 保证；重命名/改目录/重新下载时清空。
        self.add_column_if_missing("tasks", "exists_decision", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // BT 做种：上传量累计 / 完成时基线 / 做种状态机 / 起始时间
        // （见 bt_seeding.rs）。旧库缺列时所有做种写库与 TASK_COLUMNS
        // 查询都会直接报 "no such column"，必须幂等补齐。
        self.add_column_if_missing("tasks", "uploaded_bytes", "BIGINT NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing(
            "tasks",
            "uploaded_at_completion",
            "BIGINT NOT NULL DEFAULT 0",
        )
        .await?;
        self.add_column_if_missing("tasks", "seeding_status", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("tasks", "seeding_message", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "seeding_started_at", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("tasks", "seeding_time_secs", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        // 任务级做种限制覆盖（-2=跟随全局、-1=不限、>=0 自定义；比率为千分比）。
        self.add_column_if_missing(
            "tasks",
            "seed_ratio_limit_milli",
            "INTEGER NOT NULL DEFAULT -2",
        )
        .await?;
        self.add_column_if_missing(
            "tasks",
            "seed_post_ratio_limit_milli",
            "INTEGER NOT NULL DEFAULT -2",
        )
        .await?;
        self.add_column_if_missing(
            "tasks",
            "seed_time_limit_minutes",
            "INTEGER NOT NULL DEFAULT -2",
        )
        .await?;
        self.add_column_if_missing(
            "tasks",
            "seed_inactive_time_limit_minutes",
            "INTEGER NOT NULL DEFAULT -2",
        )
        .await?;
        // 任务级做种上传限速（B/s；0 = 无单任务限制）。add 时烘焙进
        // librqbit AddTorrentOptions，live 句柄不热改。
        self.add_column_if_missing(
            "tasks",
            "seed_upload_limit_bps",
            "BIGINT NOT NULL DEFAULT 0",
        )
        .await?;
        // 手动恢复做种后的限制豁免标记（1 = 不受分享率 / 时长 / 不活跃限制约束，直到
        // 用户再次停止做种）。不进 `TASK_COLUMNS`，只经专用访问器读写。
        self.add_column_if_missing("tasks", "seed_limits_exempt", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        // BT 选片方式（对齐 qBittorrent 右键「按顺序下载」「先下载首尾文件块」；
        // 都为 0 = 最稀有优先）。add 时烘焙，live 句柄经 SharedBtSession 热改。
        self.add_column_if_missing("tasks", "bt_sequential", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing(
            "tasks",
            "bt_first_last_pieces_first",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        // 队列级上传限速（KB/s；0 = 不限）。BT add/re-add 时与任务级
        // 覆盖一起折算成 librqbit 上传上限，见 download_manager。
        self.add_column_if_missing("queues", "upload_limit_kbps", "BIGINT NOT NULL DEFAULT 0")
            .await?;
        // 加速来源累计字节（多 CDN / 智能代理 / 多网卡；源站 = 已下载 − 三者之和）。
        // 与 `downloaded_bytes` 同步复位：见 `update_task_progress(_, 0)` /
        // `delete_segments` / `reset_segments_progress`。
        self.add_column_if_missing("tasks", "src_cdn_bytes", "BIGINT NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("tasks", "src_proxy_bytes", "BIGINT NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("tasks", "src_nic_bytes", "BIGINT NOT NULL DEFAULT 0")
            .await?;
        // 换源标记：`change_task_url` 置 1，下次续传探测后由 `settle_source_change`
        // 按零容差比对新旧大小（不一致即清空进度）并清回 0。
        self.add_column_if_missing("tasks", "source_changed", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        // 外部捕获的分类目录路由（JSON，见 `category_route.rs`；空 = 保存目录即最终目录）。
        self.add_column_if_missing("tasks", "category_route", "TEXT NOT NULL DEFAULT ''")
            .await?;
        // 浏览器 POST 接管任务的请求方法（空 = GET）。请求体（常含一次性令牌）不落库，
        // 恢复 / 重试时据此明确拒绝，而不是改发 GET。
        self.add_column_if_missing("tasks", "request_method", "TEXT NOT NULL DEFAULT ''")
            .await?;
        Ok(())
    }

    /// 幂等加列。PostgreSQL 有原生 `ADD COLUMN IF NOT EXISTS`；SQLite 没有
    /// 该语法，只能执行裸 `ADD COLUMN` 并把 "duplicate column"（列已存在的
    /// 正常幂等情形）静默视为成功，其他错误（磁盘满、损坏等）照常上抛。
    async fn add_column_if_missing(
        &self,
        table: &str,
        column: &str,
        decl: &str,
    ) -> Result<(), DbError> {
        match self.backend {
            Backend::Postgres => {
                let sql = format!("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS {column} {decl}");
                sqlx::raw_sql(AssertSqlSafe(sql))
                    .execute(&self.pool)
                    .await?;
            }
            Backend::Sqlite => {
                let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {decl}");
                if let Err(e) = sqlx::raw_sql(AssertSqlSafe(sql)).execute(&self.pool).await
                    && !e.to_string().to_lowercase().contains("duplicate column")
                {
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }

    /// 插入新任务。`initial_status` 支持 0（pending，正常创建）与
    /// 2（paused，「稍后下载」——建任务不启动）；`queue_order` 自动追加
    /// 到目标队列末尾（现有最大值 +1）。
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_task(
        &self,
        id: &str,
        url: &str,
        file_name: &str,
        save_dir: &str,
        segments: i32,
        total_bytes: i64,
        proxy_url: &str,
        queue_id: &str,
        checksum: &str,
        initial_status: i32,
    ) -> Result<(), DbError> {
        self.insert_task_with_tls_policy(
            id,
            url,
            file_name,
            save_dir,
            segments,
            total_bytes,
            proxy_url,
            queue_id,
            checksum,
            false,
            initial_status,
        )
        .await
    }

    /// 插入带显式 TLS 证书策略的新任务。普通调用方使用 [`Self::insert_task`]，
    /// 默认严格验证；仅下载确认链路应传入 `ignore_tls_errors = true`。
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_task_with_tls_policy(
        &self,
        id: &str,
        url: &str,
        file_name: &str,
        save_dir: &str,
        segments: i32,
        total_bytes: i64,
        proxy_url: &str,
        queue_id: &str,
        checksum: &str,
        ignore_tls_errors: bool,
        initial_status: i32,
    ) -> Result<(), DbError> {
        let now = chrono_now();
        // 进程内建任务串行（单线程 actor）；跨进程并发（CLI --local 与 App
        // 同库）的罕见撞序由 (queue_order, created_at) 复合排序自然容忍。
        let next_order: i64 = sqlx::query_scalar(
            "SELECT CAST(COALESCE(MAX(queue_order), 0) + 1 AS BIGINT) FROM tasks WHERE queue_id = $1",
        )
        .bind(queue_id)
        .fetch_one(&self.pool)
        .await?;
        sqlx::query(
            "INSERT INTO tasks (id, url, file_name, save_dir, status, segments, total_bytes, created_at, proxy_url, queue_id, checksum, ignore_tls_errors, queue_order)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(id)
        .bind(url)
        .bind(file_name)
        .bind(save_dir)
        .bind(initial_status)
        .bind(segments)
        .bind(total_bytes)
        .bind(now)
        .bind(proxy_url)
        .bind(queue_id)
        .bind(checksum)
        .bind(i32::from(ignore_tls_errors))
        .bind(next_order as i32)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取批量建任务的幂等记录；`None` = 该 `batch_id` 尚未提交过。
    pub async fn load_create_batch_results(
        &self,
        batch_id: &str,
    ) -> Result<Option<String>, DbError> {
        let results: Option<String> =
            sqlx::query_scalar("SELECT results FROM task_create_batches WHERE batch_id = $1")
                .bind(batch_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(results)
    }

    /// 单事务落库一批新任务（单个与批量创建共用同一路径）。
    ///
    /// - `queue_order`：每个目标队列只读一次 `MAX+1`，其后在内存递增；
    ///   与逐条追加得到的序列一致。
    /// - 每行一条携带全部列的 INSERT（不再 INSERT 后补 UPDATE），`.torrent`
    ///   字节同事务写入。
    /// - `batch` 非空时幂等记录与任务行同事务提交，并顺带清理过期记录；
    ///   `rows` 为空也会写入记录（全部被去重/拒绝的批次重放结果必须稳定）。
    /// - 返回与 `rows` 同序的持久化任务事实（事务内单次 `IN` 查询读回）。
    ///
    /// 任一步失败整个事务回滚，不留半成品。
    pub async fn insert_new_tasks(
        &self,
        rows: &[NewTaskRow<'_>],
        batch: Option<CreateBatchRecord<'_>>,
    ) -> Result<Vec<TaskInfo>, DbError> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let created_at = now_secs.to_string();
        let mut tx = self.begin_write().await?;
        // 进程内建任务串行（单线程 actor）；跨进程并发（CLI --local 与 App
        // 同库）由 BEGIN IMMEDIATE 串行化写入，撞序仍由
        // (queue_order, created_at) 复合排序容忍。
        let mut next_order: HashMap<&str, i64> = HashMap::new();
        for row in rows {
            let order = match next_order.get_mut(row.queue_id) {
                Some(next) => {
                    let order = *next;
                    *next += 1;
                    order
                }
                None => {
                    let first: i64 = sqlx::query_scalar(
                        "SELECT CAST(COALESCE(MAX(queue_order), 0) + 1 AS BIGINT) FROM tasks WHERE queue_id = $1",
                    )
                    .bind(row.queue_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    next_order.insert(row.queue_id, first + 1);
                    first
                }
            };
            sqlx::query(
                "INSERT INTO tasks (id, url, file_name, name_inferred, save_dir, segments, status, error_message, \
                 created_at, proxy_url, queue_id, checksum, ignore_tls_errors, queue_order, cookies, referrer, \
                 extra_headers, origin_url, audio_url, resolver_plugin_id, group_id, resolver_item, unattended, \
                 bt_selected_files, resolver_choice_pending, category_route, request_method, \
                 bt_sequential, bt_first_last_pieces_first) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, \
                 $20, $21, $22, $23, $24, $25, $26, $27, $28, $29)",
            )
            .bind(row.id)
            .bind(row.url)
            .bind(row.file_name)
            .bind(i32::from(row.name_inferred))
            .bind(row.save_dir)
            .bind(row.segments)
            .bind(row.status)
            .bind(row.error_message)
            .bind(created_at.as_str())
            .bind(row.proxy_url)
            .bind(row.queue_id)
            .bind(row.checksum)
            .bind(i32::from(row.ignore_tls_errors))
            .bind(order as i32)
            .bind(row.cookies)
            .bind(row.referrer)
            .bind(row.extra_headers_json)
            .bind(row.origin_url)
            .bind(row.audio_url)
            .bind(row.resolver_plugin_id)
            .bind(row.group_id)
            .bind(row.resolver_item)
            .bind(i32::from(row.unattended))
            .bind(row.bt_selected_files)
            .bind(i32::from(row.resolver_choice_pending))
            .bind(row.category_route)
            .bind(row.request_method)
            .bind(i32::from(row.bt_sequential))
            .bind(i32::from(row.bt_first_last_pieces_first))
            .execute(&mut *tx)
            .await?;
            if !row.torrent_file_bytes.is_empty() {
                sqlx::query(
                    "INSERT INTO torrent_files (task_id, file_bytes) VALUES ($1, $2)
                     ON CONFLICT (task_id) DO UPDATE SET file_bytes = excluded.file_bytes",
                )
                .bind(row.id)
                .bind(row.torrent_file_bytes)
                .execute(&mut *tx)
                .await?;
            }
        }

        let mut loaded: HashMap<String, TaskInfo> = HashMap::with_capacity(rows.len());
        // SQLite 绑定变量上限 999；与 `load_tasks_by_ids` 同样分块。
        const CHUNK: usize = 500;
        for chunk in rows.chunks(CHUNK) {
            let placeholders: String = (1..=chunk.len())
                .map(|i| format!("${i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id IN ({placeholders})");
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for row in chunk {
                query = query.bind(row.id);
            }
            let fetched = query.fetch_all(&mut *tx).await?;
            for fetched_row in &fetched {
                let task = task_from_row(fetched_row)?;
                loaded.insert(task.task_id.clone(), task);
            }
        }
        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            tasks.push(loaded.remove(row.id).ok_or(sqlx::Error::RowNotFound)?);
        }

        if let Some(batch) = batch {
            sqlx::query(
                "INSERT INTO task_create_batches (batch_id, results, created_at) VALUES ($1, $2, $3)",
            )
            .bind(batch.batch_id)
            .bind(batch.results)
            .bind(now_secs as i64)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM task_create_batches WHERE created_at < $1")
                .bind(now_secs as i64 - CREATE_BATCH_RETENTION_SECS)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(tasks)
    }

    /// 持久化任务的浏览器请求上下文（cookies / referrer / extra_headers JSON），
    /// 供 resume 恢复鉴权。`extra_headers_json` 为空串表示无额外请求头。
    pub async fn set_task_request_context(
        &self,
        id: &str,
        cookies: &str,
        referrer: &str,
        extra_headers_json: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET cookies = $1, referrer = $2, extra_headers = $3 WHERE id = $4",
        )
        .bind(cookies)
        .bind(referrer)
        .bind(extra_headers_json)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取任务的请求上下文，返回 `(cookies, referrer, extra_headers_json)`。
    /// 任务不存在返回 `None`；旧库缺列已由 init_schema 迁移兜底（列恒存在）。
    pub async fn load_task_request_context(
        &self,
        id: &str,
    ) -> Result<Option<(String, String, String)>, DbError> {
        let row = sqlx::query("SELECT cookies, referrer, extra_headers FROM tasks WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| {
            (
                r.try_get("cookies").unwrap_or_default(),
                r.try_get("referrer").unwrap_or_default(),
                r.try_get("extra_headers").unwrap_or_default(),
            )
        }))
    }

    /// 直接覆盖任务进度。`downloaded_bytes == 0` 表示进度复位（重下 / 清盘 /
    /// 多段转单流），同一条语句内把加速来源累计一并清零。
    pub async fn update_task_progress(
        &self,
        id: &str,
        downloaded_bytes: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET downloaded_bytes = $1,
                 src_cdn_bytes = CASE WHEN $2 = 0 THEN 0 ELSE src_cdn_bytes END,
                 src_proxy_bytes = CASE WHEN $3 = 0 THEN 0 ELSE src_proxy_bytes END,
                 src_nic_bytes = CASE WHEN $4 = 0 THEN 0 ELSE src_nic_bytes END
             WHERE id = $5",
        )
        .bind(downloaded_bytes)
        .bind(downloaded_bytes)
        .bind(downloaded_bytes)
        .bind(downloaded_bytes)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取任务持久化的加速来源累计字节；任务不存在按全 0 处理。
    pub async fn load_task_source_bytes(&self, task_id: &str) -> Result<SourceBytes, DbError> {
        let row = sqlx::query(
            "SELECT src_cdn_bytes, src_proxy_bytes, src_nic_bytes FROM tasks WHERE id = $1",
        )
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map_or_else(SourceBytes::default, |r| SourceBytes {
            cdn: r.try_get("src_cdn_bytes").unwrap_or_default(),
            proxy: r.try_get("src_proxy_bytes").unwrap_or_default(),
            nic: r.try_get("src_nic_bytes").unwrap_or_default(),
        }))
    }

    /// 累加本次运行新增的加速来源字节。用 `segments_epoch` 守卫：被更新 spawn
    /// 夺权 / 复位后的旧 coordinator 迟到写入命中 0 行，不会复活已清零的计数。
    pub async fn add_task_source_bytes(
        &self,
        task_id: &str,
        epoch: i64,
        delta: SourceBytes,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET src_cdn_bytes = src_cdn_bytes + $1,
                 src_proxy_bytes = src_proxy_bytes + $2,
                 src_nic_bytes = src_nic_bytes + $3
             WHERE id = $4 AND segments_epoch = $5",
        )
        .bind(delta.cdn)
        .bind(delta.proxy)
        .bind(delta.nic)
        .bind(task_id)
        .bind(epoch)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 单调进度写入：`downloaded_bytes` 只增不减（SQL 用 `MAX` 钳制）。
    ///
    /// 与 [`update_task_progress`](Self::update_task_progress) 的唯一区别是 SQL
    /// 用 `MAX(downloaded_bytes, $1)` 而非直接赋值，因此 DB 中的进度只会前进、
    /// 永不回退。
    ///
    /// **动机（F009）**：`progress_reporter` 中 status=1 的进度写入是
    /// fire-and-forget（spawn 后不 await），与 status=3 完成时 awaited 的最终
    /// 写入并发竞争，落库先后顺序不确定。一个先发起、携带中途较小
    /// `downloaded_bytes` 的后台写入可能在完成写入之后才落库，把 DB 里的
    /// 100% 覆盖回中途值，导致重启后进度倒退。单调写入消除了这一顺序依赖。
    ///
    /// **不可替代 `update_task_progress`**：downloader / ftp_downloader 在切多段
    /// →单流重下、`File::create` 从头开始时会主动传入 `0` 复位进度；若把那条
    /// 路径也改成 `MAX`，复位会退化成 no-op、残留陈旧高值。因此这里必须是独立
    /// 的新方法，仅供 `progress_reporter` 这类“只前进”的场景使用。
    ///
    /// 注：`MAX(a, b)`（SQLite 标量 max）与 `GREATEST(a, b)`（pg）方言不同，
    /// 但 pg 无双参 `MAX` 标量函数，这里按后端分支。
    pub async fn update_task_progress_monotonic(
        &self,
        id: &str,
        downloaded_bytes: i64,
    ) -> Result<(), DbError> {
        let sql = match self.backend {
            Backend::Sqlite => {
                "UPDATE tasks SET downloaded_bytes = MAX(downloaded_bytes, $1) WHERE id = $2"
            }
            Backend::Postgres => {
                "UPDATE tasks SET downloaded_bytes = GREATEST(downloaded_bytes, $1) WHERE id = $2"
            }
        };
        sqlx::query(sql)
            .bind(downloaded_bytes)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 以已落库的分段进度回写任务级 `downloaded_bytes`（单调，只前进）。
    ///
    /// 多段下载在运行期只按 `DB_SAVE_INTERVAL_SECS` 周期写任务级进度，而暂停 /
    /// 取消时 worker 会 fsync 后把每段最终偏移写进 `task_segments`。暂停终态帧与
    /// 重启后的列表都读 `tasks.downloaded_bytes`，若不在下载器退出前对齐，就会
    /// 显示上一个周期的旧值（开始几秒内暂停即为 0%）。
    ///
    /// 一条语句内聚合 + 写入（主键前缀索引，≤ 512 行），不把段行搬进内存；
    /// `segments_epoch` 守卫使被新 spawn 夺权后的旧下载器 0 行生效。与
    /// `restart_task` 的 `delete_segments`（同事务删段 + 进度归零）并发时不得
    /// 出现「读到删前段和、写在归零之后」：SQLite 单语句天然原子；pg 读已提交
    /// 下子查询快照早于行锁，故先 `FOR UPDATE` 锁任务行，再以锁后的新快照聚合。
    pub async fn sync_task_progress_from_segments(
        &self,
        id: &str,
        epoch: i64,
    ) -> Result<(), DbError> {
        match self.backend {
            Backend::Sqlite => {
                sqlx::query(
                    "UPDATE tasks SET downloaded_bytes = MAX(downloaded_bytes,
                         (SELECT COALESCE(SUM(downloaded_bytes), 0)
                            FROM task_segments WHERE task_id = $1))
                     WHERE id = $1 AND segments_epoch = $2",
                )
                .bind(id)
                .bind(epoch)
                .execute(&self.pool)
                .await?;
            }
            Backend::Postgres => {
                let mut tx = self.pool.begin().await?;
                sqlx::query("SELECT 1 FROM tasks WHERE id = $1 FOR UPDATE")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?;
                sqlx::query(
                    "UPDATE tasks SET downloaded_bytes = GREATEST(downloaded_bytes,
                         (SELECT COALESCE(SUM(downloaded_bytes), 0)::BIGINT
                            FROM task_segments WHERE task_id = $1))
                     WHERE id = $1 AND segments_epoch = $2",
                )
                .bind(id)
                .bind(epoch)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
            }
        }
        Ok(())
    }

    /// 更新任务状态与错误信息，并同步维护 `completed_at`（任务结束时间）：
    /// - `status = 3`（下载完成）且尚未记录 → 写入当前 Unix 秒。此写入发生在
    ///   下载数据落盘完成之时，早于插件 onDone 等 hook 后处理，故结束时间
    ///   不含 hook 耗时；重复写 3（幂等竞态）不会覆盖首次记录。
    /// - `status ∈ {0, 1, 5}`（重新排队/下载/准备）→ 清空，重下后重新记录。
    /// - 其余状态（暂停/错误）保持不变。
    ///
    /// `file_hash` 描述的是上一次落盘的文件：同样在 `status ∈ {0, 1, 5}` 时清空。
    pub async fn update_task_status(
        &self,
        id: &str,
        status: i32,
        error_message: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET status = $1, error_message = $2,
                 completed_at = CASE
                     WHEN $1 = 3 AND completed_at = '' THEN $3
                     WHEN $1 IN (0, 1, 5) THEN ''
                     ELSE completed_at
                 END,
                 file_hash = CASE WHEN $1 IN (0, 1, 5) THEN '' ELSE file_hash END
             WHERE id = $4",
        )
        .bind(status)
        .bind(error_message)
        .bind(chrono_now())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 批量更新任务状态（同一状态写入多任务），`error_message` 统一清空；
    /// `completed_at` / `file_hash` 维护规则与 [`Db::update_task_status`] 一致。批量操作
    /// （停止队列/批量暂停/批量恢复排队）专用：N 任务一条（分块）SQL，
    /// 取代逐任务 N 次 UPDATE。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), fluxdown_engine::db::DbError> {
    /// use fluxdown_engine::db::Db;
    /// let db = Db::connect("sqlite::memory:").await?;
    /// db.update_tasks_status_batch(&["a".to_string(), "b".to_string()], 2)
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn update_tasks_status_batch(
        &self,
        ids: &[String],
        status: i32,
    ) -> Result<(), DbError> {
        if ids.is_empty() {
            return Ok(());
        }
        // SQLite has a max variable limit of 999; chunk to stay safe
        // (same pattern as `load_tasks_by_ids`).  $1 = status, $2 = now.
        const CHUNK: usize = 500;
        for chunk in ids.chunks(CHUNK) {
            let placeholders: String = (3..chunk.len() + 3)
                .map(|i| format!("${i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "UPDATE tasks SET status = $1, error_message = '',
                     completed_at = CASE
                         WHEN $1 = 3 AND completed_at = '' THEN $2
                         WHEN $1 IN (0, 1, 5) THEN ''
                         ELSE completed_at
                     END,
                     file_hash = CASE WHEN $1 IN (0, 1, 5) THEN '' ELSE file_hash END
                 WHERE id IN ({placeholders})"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql))
                .bind(status)
                .bind(chrono_now());
            for id in chunk {
                query = query.bind(id.as_str());
            }
            query.execute(&self.pool).await?;
        }
        Ok(())
    }

    /// 记录任务落盘文件的哈希（`algo=hex`）。下载器在校验和校验通过时调用，此时
    /// 任务尚在下载态，随后的完成写入（status→3）保留该值。
    pub async fn set_task_file_hash(&self, id: &str, file_hash: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET file_hash = $1 WHERE id = $2")
            .bind(file_hash)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 写回一次完成后的哈希计算结果：仅当任务仍是计算开始时那一次完成（status = 3 且
    /// `completed_at` 未变）时写入。返回是否写入；期间被重新下载 / 删除则为 `false`。
    pub async fn commit_task_file_hash(
        &self,
        id: &str,
        completed_at: &str,
        file_hash: &str,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE tasks SET file_hash = $1 WHERE id = $2 AND status = 3 AND completed_at = $3",
        )
        .bind(file_hash)
        .bind(id)
        .bind(completed_at)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 更新任务的「文件已丢失」标志（文件跟踪）。仅当任务仍处于 completed
    /// (`status = 3`) 时生效——文件扫描的「读快照 → 异步 stat → 写回」三阶段间，
    /// 任务可能已被删除或状态变化，`WHERE id AND status = 3` 让这类竞态退化为
    /// 良性空操作，绝不复活已删除的行。返回是否真的更新了行
    /// (`rows_affected > 0`)，供调用方仅对实际变更下发事件。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), fluxdown_engine::db::DbError> {
    /// use fluxdown_engine::db::Db;
    /// let db = Db::connect("sqlite::memory:").await?;
    /// let changed = db.update_task_file_missing("task-1", true).await?;
    /// assert!(!changed); // 无此任务 → 未更新
    /// # Ok(())
    /// # }
    /// ```
    pub async fn update_task_file_missing(&self, id: &str, missing: bool) -> Result<bool, DbError> {
        let result = sqlx::query("UPDATE tasks SET file_missing = $1 WHERE id = $2 AND status = 3")
            .bind(missing as i32)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 批量落库文件跟踪标志：语义与 [`Self::update_task_file_missing`] 完全
    /// 一致（逐行 `WHERE id AND status = 3`），但整批在**同一个事务**里提交。
    /// SQLite 在 WAL + `synchronous=NORMAL` 下提交不再逐次 fsync，但每条独立
    /// UPDATE 仍是一次隐式事务（一次 WAL 写入与写锁往返）；
    /// 外置盘掉线这类「上万条同时翻转」的场景下那是显著阻塞，还会和下载
    /// 落库抢 `busy_timeout`。整批一次 commit 把 N 次事务压成 1 次。
    ///
    /// 返回实际写入的条目（跳过扫描期间已离开 status=3 的行），供调用方据此
    /// 下发事件与自动清理。任一条出错则整批回滚——文件跟踪是幂等的，下一轮
    /// 扫描会重新判定。
    pub async fn update_tasks_file_missing(
        &self,
        updates: &[(String, bool)],
    ) -> Result<Vec<(String, bool)>, DbError> {
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.pool.begin().await?;
        let mut applied = Vec::with_capacity(updates.len());
        for (id, missing) in updates {
            let result =
                sqlx::query("UPDATE tasks SET file_missing = $1 WHERE id = $2 AND status = 3")
                    .bind(*missing as i32)
                    .bind(id.as_str())
                    .execute(&mut *tx)
                    .await?;
            if result.rows_affected() > 0 {
                applied.push((id.clone(), *missing));
            }
        }
        tx.commit().await?;
        Ok(applied)
    }

    /// 更新任务完成时已上传字节数（BT 做种后分享率基准）。
    pub async fn update_task_uploaded_at_completion(
        &self,
        task_id: &str,
        uploaded_at_completion: i64,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET uploaded_at_completion = $1 WHERE id = $2")
            .bind(uploaded_at_completion)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 更新任务做种状态与辅助说明，并清空做种起始时间（用于停止/删除/重置）。
    pub async fn update_task_seeding_status(
        &self,
        task_id: &str,
        seeding_status: i32,
        message: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET seeding_status = $1, seeding_message = $2, seeding_started_at = 0 WHERE id = $3",
        )
        .bind(seeding_status)
        .bind(message)
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 原子增加任务已上传字节数，并返回更新后的累计值（BT 做种增量累计）。
    pub async fn add_task_uploaded_bytes(&self, task_id: &str, delta: i64) -> Result<i64, DbError> {
        sqlx::query("UPDATE tasks SET uploaded_bytes = uploaded_bytes + $1 WHERE id = $2")
            .bind(delta)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        let total: i64 = sqlx::query_scalar("SELECT uploaded_bytes FROM tasks WHERE id = $1")
            .bind(task_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(total)
    }

    /// 做种 tick 的批量落库：单事务内累加各任务的上传增量、写入做种时长快照，
    /// 返回有增量的任务更新后的累计上传字节。N 个做种者一轮只付一次提交代价。
    pub async fn apply_seeding_tick(
        &self,
        uploaded_deltas: &[(String, i64)],
        seeding_times: &[(String, i64)],
    ) -> Result<Vec<(String, i64)>, DbError> {
        if uploaded_deltas.is_empty() && seeding_times.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.pool.begin().await?;
        let mut totals = Vec::with_capacity(uploaded_deltas.len());
        for (id, delta) in uploaded_deltas {
            sqlx::query("UPDATE tasks SET uploaded_bytes = uploaded_bytes + $1 WHERE id = $2")
                .bind(*delta)
                .bind(id.as_str())
                .execute(&mut *tx)
                .await?;
            let total: Option<i64> =
                sqlx::query_scalar("SELECT uploaded_bytes FROM tasks WHERE id = $1")
                    .bind(id.as_str())
                    .fetch_optional(&mut *tx)
                    .await?;
            if let Some(total) = total {
                totals.push((id.clone(), total));
            }
        }
        for (id, secs) in seeding_times {
            sqlx::query("UPDATE tasks SET seeding_time_secs = $1 WHERE id = $2")
                .bind(*secs)
                .bind(id.as_str())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(totals)
    }

    /// 激活做种状态并记录做种起始时间（unix 秒）。
    pub async fn set_task_seeding_active(
        &self,
        task_id: &str,
        started_at_unix: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET seeding_status = 1, seeding_message = '', seeding_started_at = $1 WHERE id = $2",
        )
        .bind(started_at_unix)
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 标记任务为排队做种（活动做种数达上限，等待槽位）。
    /// 不动 seeding_time_secs——排队期间不计时。
    pub async fn set_task_seeding_queued(&self, task_id: &str) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET seeding_status = 8, seeding_message = $1, seeding_started_at = 0 WHERE id = $2",
        )
        .bind(crate::bt_seeding::SEEDING_QUEUED_MESSAGE)
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取任务累计做种秒数（跨暂停/重启累计的基线）。
    pub async fn get_task_seeding_time(&self, task_id: &str) -> Result<i64, DbError> {
        let secs: i64 = sqlx::query_scalar("SELECT seeding_time_secs FROM tasks WHERE id = $1")
            .bind(task_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(secs)
    }

    /// 写入任务累计做种秒数（周期快照或停止时的最终结算值）。
    pub async fn set_task_seeding_time(&self, task_id: &str, secs: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET seeding_time_secs = $1 WHERE id = $2")
            .bind(secs)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 写入任务级做种限制覆盖（哨兵：-2 跟随全局、-1 不限、>=0 自定义；
    /// 比率为千分比）。小于 -2 的入参钳到 -2。`upload_limit_bps` 为
    /// 任务级做种上传限速（B/s），0 = 无限制，负值钳到 0。
    pub async fn set_task_seed_limits(
        &self,
        task_id: &str,
        ratio_limit_milli: i64,
        post_ratio_limit_milli: i64,
        seed_time_limit_minutes: i64,
        inactive_time_limit_minutes: i64,
        upload_limit_bps: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET seed_ratio_limit_milli = $1, seed_post_ratio_limit_milli = $2, \
             seed_time_limit_minutes = $3, seed_inactive_time_limit_minutes = $4, \
             seed_upload_limit_bps = $5 WHERE id = $6",
        )
        .bind(ratio_limit_milli.max(-2))
        .bind(post_ratio_limit_milli.max(-2))
        .bind(seed_time_limit_minutes.max(-2))
        .bind(inactive_time_limit_minutes.max(-2))
        .bind(upload_limit_bps.max(0))
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 写入任务的 BT 选片方式（`None` = 该项不变）。
    pub async fn set_task_piece_picking(
        &self,
        task_id: &str,
        sequential: Option<bool>,
        first_last_pieces_first: Option<bool>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET bt_sequential = COALESCE($1, bt_sequential), \
             bt_first_last_pieces_first = COALESCE($2, bt_first_last_pieces_first) WHERE id = $3",
        )
        .bind(sequential.map(i32::from))
        .bind(first_last_pieces_first.map(i32::from))
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 标记 / 取消任务的做种限制豁免（手动恢复做种置 1，再次停止做种或重新下载完成时清 0）。
    pub async fn set_task_seed_limits_exempt(
        &self,
        task_id: &str,
        exempt: bool,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET seed_limits_exempt = $1 WHERE id = $2")
            .bind(i32::from(exempt))
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 所有带做种限制豁免标记的任务 ID（做种求值 tick 用）。
    pub async fn list_seed_limits_exempt_ids(&self) -> Result<HashSet<String>, DbError> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM tasks WHERE seed_limits_exempt = 1")
                .fetch_all(&self.pool)
                .await?;
        Ok(ids.into_iter().collect())
    }

    /// 查询所有残留做种态的任务（启动恢复/重置用）。
    pub async fn load_tasks_with_seeding_status(
        &self,
        status: i32,
    ) -> Result<Vec<TaskInfo>, DbError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE seeding_status = $1");
        let rows = sqlx::query(AssertSqlSafe(sql))
            .bind(status)
            .fetch_all(&self.pool)
            .await?;
        let mut tasks = Vec::with_capacity(rows.len());
        for row in &rows {
            tasks.push(task_from_row(row)?);
        }
        Ok(tasks)
    }

    /// 读取任务已上传字节数。
    pub async fn get_task_uploaded_bytes(&self, task_id: &str) -> Result<i64, DbError> {
        let uploaded: i64 = sqlx::query_scalar("SELECT uploaded_bytes FROM tasks WHERE id = $1")
            .bind(task_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(uploaded)
    }

    pub async fn update_task_file_info(
        &self,
        id: &str,
        file_name: &str,
        total_bytes: i64,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET file_name = $1, total_bytes = $2 WHERE id = $3")
            .bind(file_name)
            .bind(total_bytes)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Resume-safe variant of `update_task_file_info`.
    ///
    /// Always updates `file_name`.  Whether `total_bytes` is updated depends on
    /// the *direction* and *magnitude* of the change:
    ///
    /// - `probe == stored`  → no update needed.
    ///
    /// - `probe < stored`  (file shrank on the server)
    ///   → Always update.  Keeping the old (larger) value would cause Range
    ///   requests past the server's EOF and 416 errors.
    ///
    /// - `probe > stored`  (server reports a larger file)
    ///   → Two sub-cases, distinguished by a tolerance threshold
    ///   (1 % of stored size, capped at 1 MiB, floor 1 byte):
    ///
    ///   `delta <= threshold` — CDN drift (Transfer-Encoding overhead,
    ///   dynamic header injection, signed-URL padding…).
    ///   Keep `stored` so that segment `end_byte` boundaries stay consistent.
    ///
    ///   `delta > threshold` — File genuinely grew.  Update `total_bytes` to
    ///   `probe` so the segment coordinator rebuilds segments to cover the
    ///   new tail — without this the tail would be silently truncated.
    ///
    /// Returns `(effective_total_bytes, total_bytes_was_updated)`.
    pub async fn update_task_file_info_resume(
        &self,
        id: &str,
        file_name: &str,
        probed_total_bytes: i64,
    ) -> Result<(i64, bool), DbError> {
        // 读-判-写放进同一事务，避免池化并发下的读写间隙。
        let mut tx = self.begin_write().await?;

        let stored_total: i64 = sqlx::query_scalar("SELECT total_bytes FROM tasks WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(0);

        // Threshold: 1 % of stored size, capped at 1 MiB, floor 1 byte.
        // Must be kept in sync with the identical formula in
        // segment_coordinator::run_coordinated_download so both layers
        // always agree on whether a size change is "real".
        let threshold: i64 = if stored_total > 0 {
            (stored_total / 100).clamp(1, 1_048_576)
        } else {
            1
        };

        let size_changed = if stored_total == 0 {
            // First-time probe — always write the value.
            true
        } else if probed_total_bytes < stored_total {
            // File shrank — ALWAYS update to the smaller, authoritative size.
            //
            // 注意：缩小方向【不能】套用 CDN 漂移容差（这是与 grow 方向刻意
            // 不对称的设计，而非 bug）。若保留较大的 stored_total，segment
            // 协调器会算出 db_total==total_bytes（"精确匹配"）从而沿用旧分段，
            // 但末段 end_byte = stored_total-1 已越过服务器真实 EOF →
            // worker 发出越界 Range 请求 → 416 / 截断 → 续传永远失败。
            // 返回较小的 probed 值可让协调器走 db_total>total_bytes 分支，
            // validate_coverage 检出不一致并按新尺寸重建分段，从而成功。
            // （回归修复：此前一次"对称容差"改动破坏了小幅缩小的续传。）
            true
        } else if probed_total_bytes > stored_total {
            // File grew (or CDN drift).  Only treat as a genuine change
            // when the delta exceeds the CDN-drift tolerance threshold.
            // Below the threshold we preserve stored_total so that existing
            // segment end_byte boundaries stay consistent.
            let delta = probed_total_bytes - stored_total;
            delta > threshold
        } else {
            // Exact match.
            false
        };

        let effective_total = if size_changed {
            // Genuine size change (or first-time probe) — update both fields.
            sqlx::query("UPDATE tasks SET file_name = $1, total_bytes = $2 WHERE id = $3")
                .bind(file_name)
                .bind(probed_total_bytes)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            probed_total_bytes
        } else {
            // CDN drift within tolerance — only update file_name; preserve
            // existing total_bytes so that segment end_byte boundaries stay
            // consistent with what the coordinator will use.
            sqlx::query("UPDATE tasks SET file_name = $1 WHERE id = $2")
                .bind(file_name)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            stored_total
        };

        tx.commit().await?;
        Ok((effective_total, size_changed))
    }

    /// 更新任务文件名（仅当任务文件名为空时，防止覆盖用户自定义名称），同时记录名字
    /// 来源 `name_inferred`（见 schema migration 注释）。
    pub async fn update_task_file_name(
        &self,
        task_id: &str,
        file_name: &str,
        inferred: bool,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET file_name = $1, name_inferred = $2 \
             WHERE id = $3 AND (file_name = '' OR file_name IS NULL)",
        )
        .bind(file_name)
        .bind(if inferred { 1i32 } else { 0i32 })
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 改写文件名并同时记录来源 `name_inferred`（单条 UPDATE，二者不会分离）。用户
    /// 显式改名传 `false`：完成期不再按响应证据精修。
    pub async fn set_task_file_name_with_source(
        &self,
        task_id: &str,
        file_name: &str,
        inferred: bool,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET file_name = $1, name_inferred = $2 WHERE id = $3")
            .bind(file_name)
            .bind(if inferred { 1i32 } else { 0i32 })
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 无条件改写任务文件名，保留 `name_inferred`（引擎侧改名：启动序幕的 dedup、
    /// HLS/DASH 扩展名归一化）。来源随名字一起变时用 [`Self::set_task_file_name_with_source`]。
    pub async fn set_task_file_name(&self, task_id: &str, file_name: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET file_name = $1 WHERE id = $2")
            .bind(file_name)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 先读后写事务的入口。SQLite 的 deferred 事务在 SELECT 之后升级为写锁时，
    /// 若写锁被占或读快照已过期会立即返回 SQLITE_BUSY（不走 busy_timeout）；
    /// `BEGIN IMMEDIATE` 在开始时就拿写锁，冲突时按 busy_timeout 等待。
    /// PostgreSQL 的 READ COMMITTED 无此问题，沿用普通事务。
    async fn begin_write(&self) -> Result<sqlx::Transaction<'static, Any>, sqlx::Error> {
        match self.backend {
            Backend::Sqlite => self.pool.begin_with("BEGIN IMMEDIATE").await,
            Backend::Postgres => self.pool.begin().await,
        }
    }

    /// 启动时将所有 downloading(1)、pending(0)、preparing(5) 的任务矫正为 paused(2)
    /// 因为重启后没有活跃的下载线程，这些任务实际上处于暂停状态。
    /// 返回被矫正的任务 ID（按队列内顺序排列），供 `auto_resume_on_start` 恢复，
    /// 避免把用户原本手动暂停的任务一并拉起。
    pub async fn reset_incomplete_tasks_to_paused(&self) -> Result<Vec<String>, DbError> {
        let mut tx = self.pool.begin().await?;
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM tasks WHERE status IN (0, 1, 5) \
             ORDER BY queue_order ASC, created_at ASC, id ASC",
        )
        .fetch_all(&mut *tx)
        .await?;
        if !ids.is_empty() {
            sqlx::query("UPDATE tasks SET status = 2 WHERE status IN (0, 1, 5)")
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(ids)
    }

    pub async fn load_all_tasks(&self) -> Result<Vec<TaskInfo>, DbError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY created_at DESC");
        let rows = sqlx::query(AssertSqlSafe(sql))
            .fetch_all(&self.pool)
            .await?;
        let mut tasks = Vec::with_capacity(rows.len());
        for row in &rows {
            tasks.push(task_from_row(row)?);
        }
        Ok(tasks)
    }

    /// 加载文件跟踪扫描所需的最小投影：活跃（0/1/5，用于目标路径占用判定）
    /// 与已完成（3，待探测）的任务，只取 5 列且不排序。相对
    /// [`Self::load_all_tasks`] 省掉全表全列反序列化与一次排序——扫描每 5 分钟
    /// （headless）或每次窗口获焦（桌面）跑一遍，几万任务时这是主要开销。
    pub async fn load_file_tracking_rows(&self) -> Result<Vec<FileTrackingRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, save_dir, file_name, file_missing, status FROM tasks \
             WHERE status IN (0, 1, 3, 5)",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(FileTrackingRow {
                task_id: row.try_get("id")?,
                save_dir: row.try_get("save_dir").unwrap_or_default(),
                file_name: row.try_get("file_name").unwrap_or_default(),
                file_missing: row.try_get::<i32, _>("file_missing").unwrap_or(0) != 0,
                status: row.try_get("status")?,
            });
        }
        Ok(out)
    }

    pub async fn load_task_by_id(&self, id: &str) -> Result<Option<TaskInfo>, DbError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = $1");
        let row = sqlx::query(AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => Ok(Some(task_from_row(&row)?)),
            None => Ok(None),
        }
    }

    /// URL 去重候选集：活跃/排队状态（pending/downloading/paused/preparing）
    /// 任务的 `(id, url)`。刻意不下推字符串比较到 SQL——`url` 列存的是原始
    /// 值（可能带 fragment），去重比较前需要先按 fragment 归一化，交给调用方
    /// 用同一套归一化规则处理。任务量通常是几十到几百条，一次性取回后在
    /// 内存比较比逐条查询更省 round-trip。
    pub async fn active_task_urls(&self) -> Result<Vec<(String, String)>, DbError> {
        let rows = sqlx::query("SELECT id, url FROM tasks WHERE status IN (0, 1, 2, 5)")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push((row.try_get("id")?, row.try_get("url")?));
        }
        Ok(out)
    }

    /// Batch-load multiple tasks by ID with chunked IN clauses
    /// (same pattern as `delete_tasks_batch`).
    pub async fn load_tasks_by_ids(&self, ids: &[String]) -> Result<Vec<TaskInfo>, DbError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut results = Vec::with_capacity(ids.len());
        // SQLite has a max variable limit of 999; chunk to stay safe.
        const CHUNK: usize = 500;
        for chunk in ids.chunks(CHUNK) {
            let placeholders: String = (1..=chunk.len())
                .map(|i| format!("${i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id IN ({placeholders})");
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id.as_str());
            }
            let rows = query.fetch_all(&self.pool).await?;
            for row in &rows {
                results.push(task_from_row(row)?);
            }
        }
        Ok(results)
    }

    /// 删除任务及其 RSS 回链，返回状态/回链实际改变的订阅源 ID（去重）。
    pub async fn delete_task(&self, id: &str) -> Result<Vec<String>, DbError> {
        // RAII 事务：任何 `?` 提前返回时 Drop 自动 ROLLBACK，不会泄漏事务。
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM task_segments WHERE task_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM torrent_files WHERE task_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM task_artifacts WHERE task_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // 任务级 config 行(完成幂等哨兵 bt_completion_top_<id>、HLS 断点
        // hls_resume_<id>、独立音轨断点 hls_audio_resume_<id> 与分片过滤指纹
        // hls_filter_<id> / hls_audio_filter_<id>)随任务一并清理,防孤儿行累积。
        sqlx::query("DELETE FROM config WHERE key IN ($1, $2, $3, $4, $5)")
            .bind(format!("bt_completion_top_{id}"))
            .bind(format!("hls_resume_{id}"))
            .bind(format!("hls_audio_resume_{id}"))
            .bind(format!("hls_filter_{id}"))
            .bind(format!("hls_audio_filter_{id}"))
            .execute(&mut *tx)
            .await?;
        // 已下载条目变为已读，而非 New：否则下一轮抓取会自动重新派发。
        // 已忽略/过滤等状态原样保留，只清除无效任务回链；与任务删除同事务。
        let mut affected_sources: Vec<String> = sqlx::query_scalar(
            "UPDATE rss_items SET status = CASE WHEN status = $1 THEN $2 ELSE status END, task_id = '' WHERE task_id = $3 RETURNING source_id",
        )
        .bind(RssItemStatus::Downloaded.as_i32())
        .bind(RssItemStatus::Ignored.as_i32())
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM tasks WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        affected_sources.sort_unstable();
        affected_sources.dedup();
        Ok(affected_sources)
    }

    /// Batch-delete multiple tasks in a single transaction.
    /// Uses chunked IN clauses to respect SQLite's 999 variable limit.
    /// 返回状态/回链实际改变的订阅源 ID（去重）。
    pub async fn delete_tasks_batch(&self, ids: &[String]) -> Result<Vec<String>, DbError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut affected_sources = HashSet::new();
        let mut tx = self.pool.begin().await?;
        const CHUNK: usize = 500;
        for chunk in ids.chunks(CHUNK) {
            let placeholders: String = (1..=chunk.len())
                .map(|i| format!("${i}"))
                .collect::<Vec<_>>()
                .join(",");

            for table in ["task_segments", "torrent_files", "task_artifacts"] {
                let sql = format!("DELETE FROM {table} WHERE task_id IN ({placeholders})");
                let mut query = sqlx::query(AssertSqlSafe(sql));
                for id in chunk {
                    query = query.bind(id.as_str());
                }
                query.execute(&mut *tx).await?;
            }

            // 任务级 config 行(哨兵/HLS 视频与音轨断点/分片过滤指纹)随任务清理,
            // 防孤儿行累积。每个任务 5 个键：再分小块，参数个数保持在 SQLite 999 上限内。
            for keys_chunk in chunk.chunks(CONFIG_KEY_TASKS_PER_STATEMENT) {
                let sql = format!(
                    "DELETE FROM config WHERE key IN ({})",
                    numbered_placeholders(keys_chunk.len() * 5)
                );
                let mut query = sqlx::query(AssertSqlSafe(sql));
                for id in keys_chunk {
                    query = query
                        .bind(format!("bt_completion_top_{id}"))
                        .bind(format!("hls_resume_{id}"))
                        .bind(format!("hls_audio_resume_{id}"))
                        .bind(format!("hls_filter_{id}"))
                        .bind(format!("hls_audio_filter_{id}"));
                }
                query.execute(&mut *tx).await?;
            }
            let rss_sql = format!(
                "UPDATE rss_items SET status = CASE WHEN status = {downloaded} THEN {ignored} ELSE status END, task_id = '' WHERE task_id IN ({placeholders}) RETURNING source_id",
                downloaded = RssItemStatus::Downloaded.as_i32(),
                ignored = RssItemStatus::Ignored.as_i32(),
            );
            let mut rss_query = sqlx::query_scalar(AssertSqlSafe(rss_sql));
            for id in chunk {
                rss_query = rss_query.bind(id.as_str());
            }
            let sources: Vec<String> = rss_query.fetch_all(&mut *tx).await?;
            affected_sources.extend(sources);

            let sql = format!("DELETE FROM tasks WHERE id IN ({placeholders})");
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id.as_str());
            }
            query.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        let mut affected_sources: Vec<_> = affected_sources.into_iter().collect();
        affected_sources.sort_unstable();
        Ok(affected_sources)
    }

    // -----------------------------------------------------------------------
    // Torrent file bytes persistence
    // -----------------------------------------------------------------------

    /// Save raw .torrent file bytes for a task (for resume after restart).
    pub async fn save_torrent_file_bytes(
        &self,
        task_id: &str,
        file_bytes: &[u8],
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO torrent_files (task_id, file_bytes) VALUES ($1, $2)
             ON CONFLICT (task_id) DO UPDATE SET file_bytes = excluded.file_bytes",
        )
        .bind(task_id)
        .bind(file_bytes)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    // -----------------------------------------------------------------------
    // Task derived-artifact registry (plugin outputs, e.g. transcoded mp4)
    // -----------------------------------------------------------------------

    /// 登记任务的衍生产物文件名（同 `save_dir` 下的相对文件名，如插件转码
    /// 产物 `<stem>.mp4`）。删除任务且勾选删除文件时随任务文件一并删除。
    ///
    /// 幂等：重复登记同名产物为 no-op。
    pub async fn add_task_artifact(&self, task_id: &str, file_name: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO task_artifacts (task_id, file_name) VALUES ($1, $2)
             ON CONFLICT (task_id, file_name) DO NOTHING",
        )
        .bind(task_id)
        .bind(file_name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取任务已登记的衍生产物文件名列表（可能为空）。
    pub async fn load_task_artifacts(&self, task_id: &str) -> Result<Vec<String>, DbError> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT file_name FROM task_artifacts WHERE task_id = $1")
                .bind(task_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    /// 批量删除前一次读出整批任务的删除上下文，替代逐任务查询
    /// （`get_exists_decision` / `load_audio_url` / `load_task_artifacts` /
    /// `list_task_ids_by_file`）。
    ///
    /// `save_dirs` 是批内任务涉及的保存目录：`file_owners` 只覆盖这些目录下登记的产物名，
    /// 足以回答「某目录下某名字还归不归批外任务」。
    pub async fn load_deletion_facts(
        &self,
        ids: &[String],
        save_dirs: &[String],
    ) -> Result<DeletionFacts, DbError> {
        const CHUNK: usize = 500;
        let mut facts = DeletionFacts::default();
        for chunk in ids.chunks(CHUNK) {
            let placeholders = numbered_placeholders(chunk.len());
            let sql = format!(
                "SELECT id, exists_decision, audio_url FROM tasks WHERE id IN ({placeholders})"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id.as_str());
            }
            for row in query.fetch_all(&self.pool).await? {
                let id: String = row.try_get("id")?;
                let audio_url: String = row.try_get("audio_url")?;
                if !audio_url.is_empty() {
                    facts.has_audio_url.insert(id.clone());
                }
                facts
                    .exists_decisions
                    .insert(id, row.try_get("exists_decision")?);
            }

            let sql = format!(
                "SELECT task_id, file_name FROM task_artifacts WHERE task_id IN ({placeholders})"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id.as_str());
            }
            for row in query.fetch_all(&self.pool).await? {
                facts
                    .artifacts
                    .entry(row.try_get("task_id")?)
                    .or_default()
                    .push(row.try_get("file_name")?);
            }
        }
        for chunk in save_dirs.chunks(CHUNK) {
            let placeholders = numbered_placeholders(chunk.len());
            let sql = format!(
                "SELECT id, save_dir, file_name FROM tasks WHERE save_dir IN ({placeholders})"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for dir in chunk {
                query = query.bind(dir.as_str());
            }
            for row in query.fetch_all(&self.pool).await? {
                facts
                    .file_owners
                    .entry((row.try_get("save_dir")?, row.try_get("file_name")?))
                    .or_default()
                    .push(row.try_get("id")?);
            }
        }
        Ok(facts)
    }

    /// Persist the user's BT file selection so it survives app restart.
    ///
    /// DB encoding:
    ///   `""`        — never confirmed (default, will show dialog on next resume)
    ///   `"all"`     — user confirmed all files (skip dialog, no update_only_files)
    ///   `"0,2,5"`   — user selected a subset (skip dialog, apply update_only_files)
    pub async fn save_bt_selected_files(
        &self,
        task_id: &str,
        indices: &[i32],
        is_all: bool,
    ) -> Result<(), DbError> {
        let value = if is_all {
            "all".to_owned()
        } else {
            indices
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        sqlx::query("UPDATE tasks SET bt_selected_files = $1 WHERE id = $2")
            .bind(value)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Load the persisted BT file selection for a task.
    ///
    /// Returns:
    ///   `None`           — never confirmed; caller should show the dialog.
    ///   `Some([])`       — user confirmed all files; skip dialog & update_only_files.
    ///   `Some([0,2,5])`  — user selected a subset; skip dialog, apply update_only_files.
    pub async fn load_bt_selected_files(&self, task_id: &str) -> Result<Option<Vec<i32>>, DbError> {
        let value: Option<String> =
            sqlx::query_scalar("SELECT bt_selected_files FROM tasks WHERE id = $1")
                .bind(task_id)
                .fetch_optional(&self.pool)
                .await?;
        let Some(value) = value else {
            return Ok(None);
        };
        if value.is_empty() {
            // Never confirmed — show the dialog.
            return Ok(None);
        }
        if value == "all" {
            // Confirmed: download all files.
            return Ok(Some(Vec::new()));
        }
        let indices = value
            .split(',')
            .filter_map(|s| s.trim().parse::<i32>().ok())
            .collect();
        Ok(Some(indices))
    }

    /// 持久化音频轨 URL（离散音视频轨对下载）。空串 = 普通单 URL 任务。
    /// 与 `file_name`/`url` 独立，仅轨对任务写入，供重启恢复时重建轨对下载。
    pub async fn save_audio_url(&self, id: &str, audio_url: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET audio_url = $1 WHERE id = $2")
            .bind(audio_url)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 读取音频轨 URL。`None`/空串 = 非轨对任务。
    pub async fn load_audio_url(&self, id: &str) -> Result<Option<String>, DbError> {
        let value: Option<String> = sqlx::query_scalar("SELECT audio_url FROM tasks WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(value.filter(|v| !v.is_empty()))
    }

    /// Persist the user-specified BT custom name (rename target).
    /// This column is independent of `file_name` and is never overwritten
    /// by the download engine's Phase 1 (dn=) or Phase 3 (metadata) updates.
    pub async fn save_bt_custom_name(&self, id: &str, name: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET bt_custom_name = $1 WHERE id = $2")
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Load the user-specified BT custom name.  Returns empty string when
    /// the user did not specify a custom name (or the task is absent).
    pub async fn load_bt_custom_name(&self, id: &str) -> Result<String, DbError> {
        let name: Option<String> =
            sqlx::query_scalar("SELECT bt_custom_name FROM tasks WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(name.unwrap_or_default())
    }

    /// 持有 `.torrent` 字节的任务 id 与以 `magnet:` 开头的任务 `(id, url, file_name)`；
    /// 本机做种创建前按 info hash 查重用（先取轻量清单，再逐个按需读字节）。
    pub async fn list_bt_task_refs(
        &self,
    ) -> Result<(Vec<(String, String)>, Vec<(String, String, String)>), DbError> {
        let torrents: Vec<(String, String)> = sqlx::query_as(
            "SELECT t.id, t.file_name FROM tasks t \
             WHERE EXISTS (SELECT 1 FROM torrent_files f WHERE f.task_id = t.id)",
        )
        .fetch_all(&self.pool)
        .await?;
        let magnets: Vec<(String, String, String)> =
            sqlx::query_as("SELECT id, url, file_name FROM tasks WHERE url LIKE 'magnet:%'")
                .fetch_all(&self.pool)
                .await?;
        Ok((torrents, magnets))
    }

    /// Load raw .torrent file bytes for a task (used when resuming).
    pub async fn load_torrent_file_bytes(&self, task_id: &str) -> Result<Option<Vec<u8>>, DbError> {
        let bytes: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT file_bytes FROM torrent_files WHERE task_id = $1")
                .bind(task_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(bytes)
    }

    pub async fn insert_segments(
        &self,
        task_id: &str,
        segments: &[(i32, i64, i64)],
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        for (index, start, end) in segments {
            sqlx::query(
                "INSERT INTO task_segments (task_id, segment_index, start_byte, end_byte, downloaded_bytes)
                 VALUES ($1, $2, $3, $4, 0)",
            )
            .bind(task_id)
            .bind(*index)
            .bind(*start)
            .bind(*end)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn load_segments(&self, task_id: &str) -> Result<Vec<SegmentInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT segment_index, start_byte, end_byte, downloaded_bytes
             FROM task_segments WHERE task_id = $1 ORDER BY segment_index",
        )
        .bind(task_id)
        .fetch_all(&self.pool)
        .await?;
        let mut segs = Vec::with_capacity(rows.len());
        for row in &rows {
            segs.push(SegmentInfo {
                index: row.try_get("segment_index")?,
                start_byte: row.try_get("start_byte")?,
                end_byte: row.try_get("end_byte")?,
                downloaded_bytes: row.try_get("downloaded_bytes")?,
            });
        }
        Ok(segs)
    }

    pub async fn update_segment_progress(
        &self,
        task_id: &str,
        segment_index: i32,
        downloaded_bytes: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE task_segments SET downloaded_bytes = $1
             WHERE task_id = $2 AND segment_index = $3",
        )
        .bind(downloaded_bytes)
        .bind(task_id)
        .bind(segment_index)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 写入当前 spawn 的段行布局属主令牌。多段下载 spawn 起飞时【先于】任何
    /// 段行加载/建行调用（顺序即正确性：先夺主权，旧 spawn 的迟到写从这一刻
    /// 起全部失效，不存在"重建后、夺权前"的空窗）。
    pub async fn set_segments_epoch(&self, task_id: &str, epoch: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET segments_epoch = $1 WHERE id = $2")
            .bind(epoch)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// worker 侧的段进度写入：spawn 属主令牌 + `start_byte` 匹配双守卫 + 段长钳制。
    ///
    /// 防三类竞态污染：
    /// (1) 快速 pause→resume 后，旧 spawn 迟到的写入落到新布局的段行——
    ///     `segments_epoch` 存在性守卫使其 0 行受影响（含 start_byte 恒为 0、
    ///     单靠边界匹配无法区分的段 0），该类静默空洞窗口彻底关闭；
    /// (2) 同 spawn 内布局漂移的防御性兜底——`start_byte` 匹配；
    /// (3) 写入值超过当前段长——CASE 钳制。
    /// coordinator 侧的权威写入（`persist_split`/`flush_segments_progress`）
    /// 在事件循环内串行执行、无跨 spawn 竞态，不经此守卫。
    pub async fn update_segment_progress_bounded(
        &self,
        task_id: &str,
        segment_index: i32,
        downloaded_bytes: i64,
        start_byte: i64,
        epoch: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE task_segments SET downloaded_bytes = CASE
                 WHEN $1 > end_byte - start_byte + 1 THEN end_byte - start_byte + 1
                 ELSE $1 END
             WHERE task_id = $2 AND segment_index = $3 AND start_byte = $4
               AND EXISTS (SELECT 1 FROM tasks WHERE id = $5 AND segments_epoch = $6)",
        )
        .bind(downloaded_bytes)
        .bind(task_id)
        .bind(segment_index)
        .bind(start_byte)
        .bind(task_id)
        .bind(epoch)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 批量段进度写入（单事务）：每行复用 [`Self::update_segment_progress_bounded`]
    /// 的 WHERE 守卫语义（epoch 存在性 + `start_byte` 匹配 + 段长钳制）。
    ///
    /// `rows` 为 `(segment_index, downloaded_bytes, start_byte)`；空切片为 no-op。
    /// coordinator 周期/退出抽干 durable 水位时调用；完成写仍走单行 bounded。
    pub(crate) async fn update_segments_progress_batch(
        &self,
        task_id: &str,
        epoch: i64,
        rows: &[(i32, i64, i64)],
    ) -> Result<(), DbError> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for (seg_idx, dl_bytes, start_byte) in rows {
            sqlx::query(
                "UPDATE task_segments SET downloaded_bytes = CASE
                     WHEN $1 > end_byte - start_byte + 1 THEN end_byte - start_byte + 1
                     WHEN downloaded_bytes > $1 THEN downloaded_bytes
                     ELSE $1 END
                 WHERE task_id = $2 AND segment_index = $3 AND start_byte = $4
                   AND EXISTS (SELECT 1 FROM tasks WHERE id = $5 AND segments_epoch = $6)",
            )
            .bind(*dl_bytes)
            .bind(task_id)
            .bind(*seg_idx)
            .bind(*start_byte)
            .bind(task_id)
            .bind(epoch)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Flush final downloaded_bytes for all segments in a single transaction.
    /// Used by the coordinator after download completes to ensure DB reflects
    /// the authoritative in-memory state (capped to segment size, no overshoot).
    pub async fn flush_segments_progress(
        &self,
        task_id: &str,
        updates: Vec<(i32, i64)>, // (segment_index, downloaded_bytes)
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        for (seg_idx, dl_bytes) in &updates {
            sqlx::query(
                "UPDATE task_segments SET downloaded_bytes = $1
                 WHERE task_id = $2 AND segment_index = $3",
            )
            .bind(*dl_bytes)
            .bind(task_id)
            .bind(*seg_idx)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Config KV store
    // -----------------------------------------------------------------------

    /// Get a single config value by key.
    pub async fn get_config(&self, key: &str) -> Result<Option<String>, DbError> {
        let value: Option<String> = sqlx::query_scalar("SELECT value FROM config WHERE key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(value)
    }

    /// Set a config value (insert or update).
    pub async fn set_config(&self, key: &str, value: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO config (key, value) VALUES ($1, $2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 比较 revision 并在一个事务中提交全部配置值与下一 revision。
    pub async fn apply_config_patch_atomic(
        &self,
        expected_revision: u64,
        values: &BTreeMap<String, String>,
    ) -> Result<u64, ConfigPatchError> {
        let mut transaction = self.begin_write().await.map_err(DbError::from)?;
        let stored: Option<String> =
            sqlx::query_scalar("SELECT value FROM config WHERE key = 'daemon_config_revision'")
                .fetch_optional(&mut *transaction)
                .await
                .map_err(DbError::from)?;
        let current = stored
            .as_deref()
            .unwrap_or("0")
            .parse::<u64>()
            .map_err(|error| ConfigPatchError::InvalidStoredRevision(error.to_string()))?;
        if current != expected_revision {
            return Err(ConfigPatchError::RevisionConflict {
                expected: expected_revision,
                current,
            });
        }
        let next = current
            .checked_add(1)
            .ok_or(ConfigPatchError::RevisionOverflow)?;
        for (key, value) in values {
            sqlx::query(
                "INSERT INTO config (key, value) VALUES ($1, $2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(value)
            .execute(&mut *transaction)
            .await
            .map_err(DbError::from)?;
        }
        sqlx::query(
            "INSERT INTO config (key, value) VALUES ('daemon_config_revision', $1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(next.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(DbError::from)?;
        transaction.commit().await.map_err(DbError::from)?;
        Ok(next)
    }

    /// 在一个事务中写入内部配置键，不改变用户配置 revision。
    pub async fn set_config_batch_atomic(
        &self,
        values: &BTreeMap<String, String>,
    ) -> Result<(), DbError> {
        let mut transaction = self.pool.begin().await?;
        for (key, value) in values {
            sqlx::query(
                "INSERT INTO config (key, value) VALUES ($1, $2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(value)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    /// 返回现有 CDN 上传租约，或原子写入新租约并清空旧 pending 快照。
    pub async fn lease_cdn_reports(&self, lease_json: &str) -> Result<String, DbError> {
        self.cdn_lease_known_empty.store(false, Ordering::Release);
        let mut transaction = self.begin_write().await?;
        let existing: Option<String> =
            sqlx::query_scalar("SELECT value FROM config WHERE key = 'cdn_report_lease'")
                .fetch_optional(&mut *transaction)
                .await?;
        if let Some(existing) = existing.filter(|value| !value.trim().is_empty()) {
            transaction.rollback().await?;
            return Ok(existing);
        }
        sqlx::query(
            "INSERT INTO config (key, value) VALUES ('cdn_report_lease', $1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(lease_json)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO config (key, value) VALUES ('cdn_pending_reports', '')
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(lease_json.to_owned())
    }

    /// 只在 batch ID 匹配当前租约时原子清空。
    pub async fn ack_cdn_report_lease(&self, batch_id: &str) -> Result<bool, DbError> {
        let mut transaction = self.pool.begin().await?;
        let existing: Option<String> =
            sqlx::query_scalar("SELECT value FROM config WHERE key = 'cdn_report_lease'")
                .fetch_optional(&mut *transaction)
                .await?;
        let matches = existing
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|value| {
                value
                    .get("batchId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|stored| stored == batch_id);
        if !matches {
            transaction.rollback().await?;
            return Ok(false);
        }
        sqlx::query("UPDATE config SET value = '' WHERE key = 'cdn_report_lease'")
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        self.cdn_lease_known_empty.store(true, Ordering::Release);
        Ok(true)
    }

    /// 读取当前 CDN 上传租约；已知为空时直接返回 None，不触碰数据库。
    pub async fn peek_cdn_report_lease(&self) -> Result<Option<String>, DbError> {
        if self.cdn_lease_known_empty.load(Ordering::Acquire) {
            return Ok(None);
        }
        let lease = self
            .get_config("cdn_report_lease")
            .await?
            .filter(|value| !value.trim().is_empty());
        if lease.is_none() {
            self.cdn_lease_known_empty.store(true, Ordering::Release);
        }
        Ok(lease)
    }

    /// Delete a config entry by key.
    pub async fn delete_config(&self, key: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM config WHERE key = $1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// List all config rows whose key starts with `prefix` (literal match).
    ///
    /// `prefix` 中的 LIKE 通配符(`%` / `_` / `\`)会被转义,保证按字面前缀
    /// 匹配。用于枚举任务级哨兵行(如 `bt_completion_top_<task_id>`——BT 完成
    /// 移动的 claim-aware dedup 需要看到其他任务已声明的顶层名)。
    pub async fn list_config_with_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<(String, String)>, DbError> {
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let rows = sqlx::query("SELECT key, value FROM config WHERE key LIKE $1 ESCAPE '\\'")
            .bind(format!("{escaped}%"))
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push((row.try_get("key")?, row.try_get("value")?));
        }
        Ok(out)
    }

    /// 同一 `save_dir` 下其他**未完成**任务已登记的 `file_name` 列表。
    ///
    /// HTTP finalize 占名冲突时用作 dedup 避让集:兄弟任务在启动期已把
    /// dedup 后的最终名落库,但其 `.fdownloading` 临时文件可能尚未创建,
    /// 仅凭磁盘探测会把该名误判为空闲,造成两条任务 `file_name` 指向同一
    /// 磁盘名(误删其一即毁对方产物)。已完成任务(status=3)无需列出——
    /// 其产物在磁盘上,dedup 的磁盘探测自然避开。
    pub async fn list_active_sibling_file_names(
        &self,
        save_dir: &str,
        exclude_task_id: &str,
    ) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query(
            "SELECT file_name FROM tasks
             WHERE save_dir = $1 AND id <> $2 AND status <> 3 AND file_name <> ''",
        )
        .bind(save_dir)
        .bind(exclude_task_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(row.try_get("file_name")?);
        }
        Ok(out)
    }

    /// 把 `save_dir` 下 `file_name` 登记为自己产物名的全部任务 id。
    ///
    /// 删除任务文件前用来确认该名字没有同时属于别的任务：两条任务的 `file_name`
    /// 指向同一磁盘名时，删除其一不得带走对方的产物。
    pub async fn list_task_ids_by_file(
        &self,
        save_dir: &str,
        file_name: &str,
    ) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query("SELECT id FROM tasks WHERE save_dir = $1 AND file_name = $2")
            .bind(save_dir)
            .bind(file_name)
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(row.try_get("id")?);
        }
        Ok(out)
    }

    /// Load all config entries as a HashMap.
    pub async fn get_all_config(&self) -> Result<HashMap<String, String>, DbError> {
        let rows = sqlx::query("SELECT key, value FROM config")
            .fetch_all(&self.pool)
            .await?;
        let mut map = HashMap::with_capacity(rows.len());
        for row in &rows {
            map.insert(row.try_get("key")?, row.try_get("value")?);
        }
        Ok(map)
    }

    /// Insert default config values (only if not already set).
    pub async fn init_default_config(&self, default_save_dir: &str) -> Result<(), DbError> {
        #[cfg(feature = "bt")]
        let default_sub_urls = crate::tracker_subscription::default_subscription_urls();
        #[cfg(feature = "ed2k")]
        let default_ed2k_met_urls = crate::ed2k::server_subscription::default_server_met_urls();
        #[cfg_attr(
            not(any(feature = "bt", feature = "ed2k")),
            allow(unused_mut, clippy::useless_vec)
        )]
        let mut defaults: Vec<(&str, &str)> = vec![
            ("default_save_dir", default_save_dir),
            ("default_segments", "0"),
            // Auto 模式最大连接数上限：advisor 推荐值经此裁剪。
            ("auto_max_connections", "16"),
            // 多 CDN 节点并发下载（实验性）：默认关；节点上限 0=自动（按文件
            // 大小/并发推导），1..=8 手动。
            ("cdn_multi_enabled", "0"),
            ("cdn_max_nodes", "0"),
            ("multi_nic_enabled", "0"),
            ("max_concurrent_tasks", "5"),
            ("speed_limit_bytes", "0"),
            // 全局 BT 上传限速（B/s）："0" = 不限。与 speed_limit_bytes
            // 解耦：后者只管下载，本键管 BT 上传（下载期上传 + 做种）。
            ("upload_limit_bytes", "0"),
            // 文件已存在时的处理方式："rename" = 自动编号改名（默认，现状）；
            // "overwrite" = 冲突仅来自磁盘上已存在的最终文件时保留原名，
            // 完成时覆盖旧文件（临时文件 / 并发任务预订仍照旧编号改名）。
            ("file_exists_behavior", "rename"),
            // 任务的文件被删除或移动时的动作："keep" = 保留任务记录（默认，
            // 仅标记 file_missing）；"delete" = 扫描到文件消失后自动删除任务记录。
            ("file_missing_action", "keep"),
            // 已完成任务记录保留天数：超期的「已完成」记录被自动清理（只删记录，
            // 不删文件，做种中的任务跳过）；"0" = 永不清理（默认）。
            ("completed_record_retention_days", "0"),
            // 空闲（无活跃/排队任务）时是否仍执行周期性文件跟踪扫描：
            // "0" = 完全空闲期跳过定时扫描（默认，避免唤醒 NAS 休眠硬盘）；
            // "1"/"true" = 照常扫描。窗口聚焦、手动重扫不受影响。
            ("idle_file_scan", "0"),
            // 自动重试：-1=无限，0=关闭，1..10=次数。延迟（秒）固定基值×已重试次数。
            ("max_auto_retries", "3"),
            ("auto_retry_delay_secs", "5"),
            ("auto_resume_on_start", "false"),
            ("close_to_tray", "true"),
            ("auto_startup", "false"),
            ("auto_check_update", "true"),
            // 匿名使用统计（每日活跃事件）；首装事件由 agent `analytics.rs` 一次性上报，不受此开关控制。
            ("analytics_enabled", "true"),
            ("proxy_mode", "none"),
            ("proxy_type", "http"),
            ("proxy_host", ""),
            ("proxy_port", ""),
            ("proxy_username", ""),
            ("proxy_password", ""),
            ("proxy_no_list", ""),
            ("global_user_agent", ""),
            // 本机 API 服务器（axum，见 native/api）：探活 / 脚本接管 /
            // aria2 兼容 / 管理 API。仅监听 127.0.0.1；token 为空表示
            // 接管/aria2 端点不鉴权（仍受自定义请求头门禁 + 下载确认弹框
            // 保护），管理 API 则强制要求 token。
            ("local_server_enabled", "true"),
            ("local_server_port", "17800"),
            ("local_server_token", ""),
            ("local_server_takeover_enabled", "true"),
            ("local_server_jsonrpc_enabled", "true"),
            ("local_server_api_enabled", "false"),
        ];
        // BitTorrent 相关默认值：没有 `bt` feature 的构建（iOS 商店）既不读也不暴露这些键。
        #[cfg(feature = "bt")]
        defaults.extend([
            // 启动自动续种："1" = 上次退出时仍在做种/排队的已完成任务，
            // 重启后自动重新挂载做种；"0" = 保持停止态。
            ("bt_auto_reseed", "1"),
            // 完成后自动做种："1" = BT 任务下载完成后注册做种者继续上传；
            // "0" = 完成即停止做种（librqbit 侧暂停）。完成时实时读库，热生效。
            ("bt_seed_enabled", "1"),
            ("bt_enabled", "true"),
            ("bt_enable_dht", "true"),
            ("bt_enable_upnp", "true"),
            ("bt_enable_utp", "true"),
            ("bt_port_start", "6881"),
            ("bt_port_end", "6891"),
            // MSE 三态逃生开关（disabled/enabled/forced），无 UI，经 config API/CLI 可设。
            ("bt_mse_mode", "enabled"),
            ("bt_custom_trackers", ""),
            ("bt_piece_order", "rarest"),
            ("bt_first_last_pieces_first", "false"),
            // Tracker 订阅：默认启用，订阅社区流行的两个精选列表
            // （XIU2/TrackersListCollection + ngosang/trackerslist）。
            // cache 由订阅刷新流程写入，updated_at=0 表示从未更新。
            ("bt_tracker_sub_enabled", "true"),
            ("bt_tracker_sub_urls", &default_sub_urls),
            ("bt_tracker_sub_cache", ""),
            ("bt_tracker_sub_updated_at", "0"),
            ("torrent_assoc_prompted", "false"),
        ]);
        // eD2K 相关默认值：同上，仅 `ed2k` feature 下播种。
        #[cfg(feature = "ed2k")]
        defaults.extend([
            // eD2K 服务器列表（逗号分隔 host:port）—— 用户手填/覆盖用。
            // 公共服务器高频轮换；订阅缓存（ed2k_server_sub_cache）是主要来源，
            // 二者在找源时合并。以下为写作时常见的长期在线服务器。
            (
                "ed2k_server_list",
                "176.123.5.89:4725,45.82.80.155:5687,85.121.5.137:4232,176.123.2.239:4232,145.239.2.134:4661,91.208.162.87:4232,37.15.61.236:4232",
            ),
            // eD2K 服务器订阅（server.met）：默认启用，订阅社区维护列表。
            // cache 由订阅刷新流程写入，updated_at=0 表示从未更新。
            ("ed2k_server_sub_enabled", "true"),
            ("ed2k_server_sub_urls", &default_ed2k_met_urls),
            ("ed2k_server_sub_cache", ""),
            ("ed2k_server_sub_updated_at", "0"),
            // eD2K 客户端：监听端口（0=OS 选）、UPnP 端口映射争取 HighID、
            // Kad DHT 去中心化找源。UPnP/Kad 默认启用（best-effort，失败回退）。
            ("ed2k_listen_port", "0"),
            ("ed2k_enable_upnp", "true"),
            ("ed2k_enable_kad", "true"),
            // Kad bootstrap：nodes.dat 下载地址（社区维护）+ 缓存（base64）+ 更新时刻。
            (
                "ed2k_nodes_dat_url",
                "https://upd.emule-security.org/nodes.dat",
            ),
            ("ed2k_nodes_dat_cache", ""),
            ("ed2k_nodes_dat_updated_at", "0"),
        ]);
        for (key, value) in &defaults {
            sqlx::query(
                "INSERT INTO config (key, value) VALUES ($1, $2)
                 ON CONFLICT (key) DO NOTHING",
            )
            .bind(*key)
            .bind(*value)
            .execute(&self.pool)
            .await?;
        }
        self.migrate_idle_file_scan_default_off().await?;
        self.migrate_completed_at_backfill().await?;
        #[cfg(feature = "ed2k")]
        self.migrate_ed2k_server_sub_https().await?;
        Ok(())
    }

    /// 一次性迁移：`idle_file_scan` 曾以 "1" 播种却从未被 daemon 读取/暴露，
    /// 现默认改为关闭。仅在迁移标记首次写入成功时把遗留的 "1"/"true" 改为 "0"，
    /// 此后用户自行设置的值不再被覆盖。标记与改值在同一事务。
    async fn migrate_idle_file_scan_default_off(&self) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        let marked = sqlx::query(
            "INSERT INTO config (key, value) VALUES ('migration_idle_file_scan_off', '1')
             ON CONFLICT (key) DO NOTHING",
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if marked > 0 {
            sqlx::query(
                "UPDATE config SET value = '0'
                 WHERE key = 'idle_file_scan' AND value IN ('1', 'true')",
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 一次性迁移：保留期清理依赖 `completed_at`，而旧版本完成的任务可能没有该时间戳。
    /// 仅在迁移标记首次写入成功时，把「已完成但 completed_at 为空」的行补写为当前时刻，
    /// 让遗留记录的保留窗口从升级时开始计算（不会因缺时间戳而被立即清理或永不清理）。
    /// 标记与回填在同一事务。
    async fn migrate_completed_at_backfill(&self) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        let marked = sqlx::query(
            "INSERT INTO config (key, value) VALUES ('migration_completed_at_backfill', '1')
             ON CONFLICT (key) DO NOTHING",
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if marked > 0 {
            sqlx::query(
                "UPDATE tasks SET completed_at = $1 WHERE status = 3 AND completed_at = ''",
            )
            .bind(chrono_now())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 列出已超过保留期的「已完成」任务 id：`status = 3`、`completed_at` 非空且早于
    /// `cutoff_secs`（Unix 秒），并排除做种中 / 排队做种的任务。最多返回 `limit` 条，
    /// 按完成时间从旧到新。
    pub async fn list_expired_completed_task_ids(
        &self,
        cutoff_secs: i64,
        limit: i64,
    ) -> Result<Vec<String>, DbError> {
        let sql = format!(
            "SELECT id FROM tasks
             WHERE status = 3 AND completed_at <> ''
               AND seeding_status NOT IN ({active}, {queued})
               AND (CASE WHEN completed_at = '' THEN 0 ELSE CAST(completed_at AS BIGINT) END) < $1
             ORDER BY completed_at ASC, id ASC
             LIMIT $2",
            active = crate::bt_seeding::SEEDING_STATUS_ACTIVE,
            queued = crate::bt_seeding::SEEDING_STATUS_QUEUED,
        );
        let ids = sqlx::query_scalar(AssertSqlSafe(sql))
            .bind(cutoff_secs)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(ids)
    }

    /// 测试专用：直接写入 `completed_at`（正常路径只会写入当前时刻，无法造出「旧」记录）。
    #[cfg(test)]
    pub(crate) async fn set_completed_at_for_test(
        &self,
        id: &str,
        completed_at: &str,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET completed_at = $1 WHERE id = $2")
            .bind(completed_at)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    #[cfg(feature = "ed2k")]
    /// 一次性迁移：`ed2k_server_sub_urls` 曾以明文
    /// `http://upd.emule-security.org/server.met` 播种，该列表无校验和，
    /// 明文传输可被中间人替换；同一端点 https 内容一致，改为 https。
    /// 仅在迁移标记首次写入成功时改写，此后用户自行填写的 http 地址不再被覆盖。
    async fn migrate_ed2k_server_sub_https(&self) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        let marked = sqlx::query(
            "INSERT INTO config (key, value) VALUES ('migration_ed2k_server_sub_https', '1')
             ON CONFLICT (key) DO NOTHING",
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if marked > 0 {
            sqlx::query(
                "UPDATE config
                 SET value = REPLACE(value, 'http://upd.emule-security.org/server.met',
                                     'https://upd.emule-security.org/server.met')
                 WHERE key = 'ed2k_server_sub_urls'",
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Delete all segment rows for a task (used when total_bytes changes on resume).
    pub async fn delete_segments(&self, task_id: &str) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM task_segments WHERE task_id = $1")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        // Also reset downloaded_bytes in the tasks table
        sqlx::query(
            "UPDATE tasks SET downloaded_bytes = 0,
                 src_cdn_bytes = 0, src_proxy_bytes = 0, src_nic_bytes = 0
             WHERE id = $1",
        )
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // ED2K blocks / hashset
    // -----------------------------------------------------------------------

    /// Initialise missing block rows in bounded batches without overwriting resume state.
    pub async fn init_ed2k_blocks(&self, task_id: &str, block_count: u64) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        let mut first = 0_u64;
        while first < block_count {
            let end = first.saturating_add(256).min(block_count);
            let values = (0..end - first)
                .map(|i| format!("(${}, ${}, 0, 0, 0)", i * 2 + 1, i * 2 + 2))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "INSERT INTO ed2k_blocks (task_id, block_index, state, downloaded_bytes, retry_count) VALUES {values}
                 ON CONFLICT (task_id, block_index) DO NOTHING"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for i in first..end {
                query = query.bind(task_id).bind(i as i64);
            }
            query.execute(&mut *tx).await?;
            first = end;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Invalidated temp data must not retain verified block markers.
    pub async fn reset_ed2k_blocks(&self, task_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE ed2k_blocks SET state = 0, downloaded_bytes = 0 WHERE task_id = $1")
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Load all block rows for an ed2k task, ordered by block_index.
    /// Returns `(block_index, state, downloaded_bytes, retry_count)`.
    pub async fn load_ed2k_blocks(
        &self,
        task_id: &str,
    ) -> Result<Vec<(u64, i64, i64, i64)>, DbError> {
        let rows = sqlx::query(
            "SELECT block_index, state, downloaded_bytes, retry_count
             FROM ed2k_blocks WHERE task_id = $1 ORDER BY block_index",
        )
        .bind(task_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let idx: i64 = row.try_get("block_index")?;
            out.push((
                idx as u64,
                row.try_get("state")?,
                row.try_get("downloaded_bytes")?,
                row.try_get("retry_count")?,
            ));
        }
        Ok(out)
    }

    /// Update one block's state (+ optionally bump retry_count).
    /// `bump_retry` increments retry_count atomically when true.
    pub async fn update_ed2k_block(
        &self,
        task_id: &str,
        block_index: u64,
        state: i64,
        downloaded_bytes: i64,
        bump_retry: bool,
    ) -> Result<(), DbError> {
        self.update_ed2k_blocks(
            task_id,
            &[(block_index, state, downloaded_bytes, bump_retry)],
        )
        .await
    }

    /// Commit changed blocks together; retry increments apply once per supplied row.
    pub async fn update_ed2k_blocks(
        &self,
        task_id: &str,
        blocks: &[(u64, i64, i64, bool)],
    ) -> Result<(), DbError> {
        if blocks.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for chunk in blocks.chunks(128) {
            let values = (0..chunk.len())
                .map(|i| {
                    let p = i * 5;
                    format!(
                        "(${}, ${}, ${}, ${}, ${})",
                        p + 1,
                        p + 2,
                        p + 3,
                        p + 4,
                        p + 5
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "INSERT INTO ed2k_blocks (task_id, block_index, state, downloaded_bytes, retry_count) VALUES {values}
                 ON CONFLICT (task_id, block_index) DO UPDATE SET state = excluded.state,
                 downloaded_bytes = excluded.downloaded_bytes,
                 retry_count = ed2k_blocks.retry_count + excluded.retry_count"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for &(index, state, bytes, bump_retry) in chunk {
                query = query
                    .bind(task_id)
                    .bind(index as i64)
                    .bind(state)
                    .bind(bytes)
                    .bind(i64::from(bump_retry));
            }
            query.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Persist the verified hashset blob (concatenated 16B * part_count block
    /// hashes, network order, no phantom-tail append). Idempotent (upsert).
    pub async fn save_ed2k_hashset(&self, task_id: &str, hashes: &[u8]) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO ed2k_hashset (task_id, hashes) VALUES ($1, $2)
             ON CONFLICT (task_id) DO UPDATE SET hashes = excluded.hashes",
        )
        .bind(task_id)
        .bind(hashes)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load the persisted hashset blob, if any.
    pub async fn load_ed2k_hashset(&self, task_id: &str) -> Result<Option<Vec<u8>>, DbError> {
        let bytes: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT hashes FROM ed2k_hashset WHERE task_id = $1")
                .bind(task_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(bytes)
    }

    /// Reset all segment progress for a task back to zero.
    pub async fn reset_segments_progress(&self, task_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE task_segments SET downloaded_bytes = 0 WHERE task_id = $1")
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "UPDATE tasks SET downloaded_bytes = 0,
                 src_cdn_bytes = 0, src_proxy_bytes = 0, src_nic_bytes = 0
             WHERE id = $1",
        )
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Update the segment count for a task (e.g. after dynamic calculation).
    pub async fn update_task_segments(&self, id: &str, segments: i32) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET segments = $1 WHERE id = $2")
            .bind(segments)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Insert or replace a single segment row (used by dynamic segment coordinator).
    ///
    /// This is the upsert counterpart to `insert_segments` — it handles a single
    /// segment that may or may not already exist in the DB.
    pub async fn upsert_segment(
        &self,
        task_id: &str,
        segment_index: i32,
        start_byte: i64,
        end_byte: i64,
        downloaded_bytes: i64,
    ) -> Result<(), DbError> {
        // Atomic DELETE + INSERT inside a transaction.
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM task_segments WHERE task_id = $1 AND segment_index = $2")
            .bind(task_id)
            .bind(segment_index)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO task_segments (task_id, segment_index, start_byte, end_byte, downloaded_bytes)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(task_id)
        .bind(segment_index)
        .bind(start_byte)
        .bind(end_byte)
        .bind(downloaded_bytes)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Atomically persist a segment split: upsert the new child segment **and**
    /// shrink the parent's `end_byte` in a single transaction.
    ///
    /// This prevents the scenario where the process crashes between the two
    /// operations, leaving overlapping byte ranges that `validate_coverage`
    /// would have to reset.
    #[allow(clippy::too_many_arguments)]
    pub async fn persist_split(
        &self,
        task_id: &str,
        child_index: i32,
        child_start: i64,
        child_end: i64,
        child_downloaded: i64,
        parent_index: i32,
        parent_new_end: i64,
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        // 1. Upsert child segment (DELETE + INSERT).
        sqlx::query("DELETE FROM task_segments WHERE task_id = $1 AND segment_index = $2")
            .bind(task_id)
            .bind(child_index)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO task_segments (task_id, segment_index, start_byte, end_byte, downloaded_bytes)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(task_id)
        .bind(child_index)
        .bind(child_start)
        .bind(child_end)
        .bind(child_downloaded)
        .execute(&mut *tx)
        .await?;
        // 2. Shrink parent's end_byte.
        sqlx::query(
            "UPDATE task_segments SET end_byte = $1
             WHERE task_id = $2 AND segment_index = $3",
        )
        .bind(parent_new_end)
        .bind(task_id)
        .bind(parent_index)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 原子持久化【开放式首段合并】：延长父段 `end_byte` 并删除全部被吸收的
    /// Pending 段行，单事务提交（与 `persist_split` 对称——防止崩溃残留
    /// 重叠/缺口区间，否则 resume 时 `validate_coverage` 会整体重置进度）。
    pub async fn persist_merge(
        &self,
        task_id: &str,
        parent_index: i32,
        parent_new_end: i64,
        absorbed: &[i32],
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE task_segments SET end_byte = $1
             WHERE task_id = $2 AND segment_index = $3",
        )
        .bind(parent_new_end)
        .bind(task_id)
        .bind(parent_index)
        .execute(&mut *tx)
        .await?;
        for idx in absorbed {
            sqlx::query("DELETE FROM task_segments WHERE task_id = $1 AND segment_index = $2")
                .bind(task_id)
                .bind(*idx)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Update the total_bytes for a task.
    pub async fn update_task_total_bytes(&self, id: &str, total_bytes: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET total_bytes = $1 WHERE id = $2")
            .bind(total_bytes)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 记录首次下载时 probe 看到的【原始】版本标识（ETag / Last-Modified）。
    /// 仅在非续传的首次下载阶段写入，作为后续续传 If-Range 一致性校验的基准。
    pub async fn set_task_validator(
        &self,
        id: &str,
        etag: &str,
        last_modified: &str,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET orig_etag = $1, orig_last_modified = $2 WHERE id = $3")
            .bind(etag)
            .bind(last_modified)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 读取首次下载记录的原始版本标识，返回 `(orig_etag, orig_last_modified)`。
    /// 旧任务（升级前创建、列为默认空）或服务器未提供时返回 `("", "")`。
    pub async fn get_task_validator(&self, id: &str) -> Result<(String, String), DbError> {
        let row = sqlx::query("SELECT orig_etag, orig_last_modified FROM tasks WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => Ok((
                row.try_get("orig_etag").unwrap_or_default(),
                row.try_get("orig_last_modified").unwrap_or_default(),
            )),
            None => Ok((String::new(), String::new())),
        }
    }

    /// 设置任务的 Range 能力验证标记（见 schema migration 注释）。
    pub async fn set_task_range_verified(&self, id: &str, verified: bool) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET range_verified = $1 WHERE id = $2")
            .bind(if verified { 1i32 } else { 0i32 })
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 读取任务的 Range 能力验证标记。任务不存在/旧库默认视为已验证（true），
    /// 保证 probe 任务与升级前创建的任务行为完全不变。
    pub async fn get_task_range_verified(&self, id: &str) -> Result<bool, DbError> {
        let row = sqlx::query("SELECT range_verified FROM tasks WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| r.try_get::<i32, _>("range_verified").unwrap_or(1) != 0)
            .unwrap_or(true))
    }

    /// 记录实际响应的命名证据：`name` = Content-Disposition 解出的名字（空 = 无），
    /// `mime` = Content-Type。逐字段先写者胜——多段下载每段首响应都会调用，只有
    /// 第一次写入的非空值生效。
    pub async fn record_task_response_naming(
        &self,
        id: &str,
        name: &str,
        mime: &str,
    ) -> Result<(), DbError> {
        if name.is_empty() && mime.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE tasks SET \
             resp_name = CASE WHEN resp_name = '' THEN $1 ELSE resp_name END, \
             resp_mime = CASE WHEN resp_mime = '' THEN $2 ELSE resp_mime END \
             WHERE id = $3",
        )
        .bind(name)
        .bind(mime)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 清空响应命名证据（换下载地址后旧响应不再代表新资源）。
    pub async fn clear_task_response_naming(&self, id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET resp_name = '', resp_mime = '' WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 读取 `(name_inferred, resp_name, resp_mime)`；任务不存在时为「显式、无证据」，
    /// 完成期据此不做任何改名。
    pub async fn get_task_naming_state(&self, id: &str) -> Result<(bool, String, String), DbError> {
        let row =
            sqlx::query("SELECT name_inferred, resp_name, resp_mime FROM tasks WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some(row) => Ok((
                row.try_get::<i32, _>("name_inferred")? != 0,
                row.try_get("resp_name")?,
                row.try_get("resp_mime")?,
            )),
            None => Ok((false, String::new(), String::new())),
        }
    }

    /// 设置任务的 resolver 插件 ID（空串 = 清除，供「忽略插件重试」逃生舱）。
    /// 仅存 ID、不存解析结果 —— 每次 start/resume 重新 resolve 是惰性防直链过期。
    /// 显式绑定即终结多插件待选（`resolver_choice_pending` 清 0）。
    pub async fn set_task_resolver(
        &self,
        id: &str,
        resolver_plugin_id: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET resolver_plugin_id = $1, resolver_choice_pending = 0 WHERE id = $2",
        )
        .bind(resolver_plugin_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 任务是否仍待多插件选择（读不到 / 任务不存在按 false）。
    pub async fn is_task_resolver_choice_pending(&self, id: &str) -> Result<bool, DbError> {
        let row = sqlx::query("SELECT resolver_choice_pending FROM tasks WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| r.try_get::<i32, _>("resolver_choice_pending").unwrap_or(0) != 0)
            .unwrap_or(false))
    }

    /// 读取任务的 resolver 插件 ID（空串 = 无）。
    pub async fn get_task_resolver(&self, id: &str) -> Result<String, DbError> {
        let row = sqlx::query("SELECT resolver_plugin_id FROM tasks WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| {
                r.try_get::<String, _>("resolver_plugin_id")
                    .unwrap_or_default()
            })
            .unwrap_or_default())
    }

    /// 清除所有绑定到指定 resolver 插件的任务绑定（插件卸载时调用）。
    ///
    /// 不清则留下 orphaned 绑定：resume 时 resolver 已不存在，任务会以
    /// fail-closed 报错卡住。卸载即等价于对受影响任务批量应用「忽略插件、
    /// 按原始链接重跑」逃生舱。多插件待选任务例外：绑定只是回退值，起跑时会在
    /// 剩余候选里重新选择，清空反而会让它绕过仍能处理该链接的插件。返回受影响任务数。
    pub async fn clear_tasks_resolver(&self, resolver_plugin_id: &str) -> Result<u64, DbError> {
        let r = sqlx::query(
            "UPDATE tasks SET resolver_plugin_id = '' \
             WHERE resolver_plugin_id = $1 AND resolver_choice_pending = 0",
        )
        .bind(resolver_plugin_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// Manually run a WAL checkpoint to merge the write-ahead log back into the
    /// main database file.  Called when all downloads are idle (no active tasks)
    /// so the WAL doesn't grow unbounded and no background autocheckpoint causes
    /// unexpected disk I/O.  No-op on PostgreSQL (WAL is server-managed).
    pub async fn wal_checkpoint(&self) -> Result<(), DbError> {
        match self.backend {
            Backend::Sqlite => {
                sqlx::raw_sql("PRAGMA wal_checkpoint(TRUNCATE);")
                    .execute(&self.pool)
                    .await?;
            }
            Backend::Postgres => {}
        }
        Ok(())
    }

    /// Get the configured segment count for a task from the tasks table.
    /// Errors when the task does not exist (mirrors historical behaviour).
    pub async fn get_task_segments(&self, id: &str) -> Result<i32, DbError> {
        let seg: i32 = sqlx::query_scalar("SELECT segments FROM tasks WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(seg)
    }

    // -----------------------------------------------------------------------
    // Named queue CRUD
    // -----------------------------------------------------------------------

    /// Insert a new named download queue.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_queue(
        &self,
        id: &str,
        name: &str,
        speed_limit_kbps: i64,
        upload_limit_kbps: i64,
        max_concurrent: i32,
        default_save_dir: &str,
        position: i32,
        default_segments: i32,
        default_user_agent: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO queues (id, name, speed_limit_kbps, upload_limit_kbps, max_concurrent, default_save_dir, position, default_segments, default_user_agent)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(name)
        .bind(speed_limit_kbps)
        .bind(upload_limit_kbps)
        .bind(max_concurrent)
        .bind(default_save_dir)
        .bind(position)
        .bind(default_segments)
        .bind(default_user_agent)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Update a queue's settings.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_queue(
        &self,
        id: &str,
        name: &str,
        speed_limit_kbps: i64,
        upload_limit_kbps: i64,
        max_concurrent: i32,
        default_save_dir: &str,
        default_segments: i32,
        default_user_agent: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE queues SET name = $1, speed_limit_kbps = $2, upload_limit_kbps = $3, max_concurrent = $4, \
             default_save_dir = $5, default_segments = $6, default_user_agent = $7 WHERE id = $8",
        )
        .bind(name)
        .bind(speed_limit_kbps)
        .bind(upload_limit_kbps)
        .bind(max_concurrent)
        .bind(default_save_dir)
        .bind(default_segments)
        .bind(default_user_agent)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete a queue; its tasks are reassigned to the builtin main queue
    /// (清除显式 `queue_order`，按 `created_at` 先来先启动)。返回原子迁移的任务 id，
    /// 供宿主发送定向任务归属增量，避免其物化快照继续引用已删除队列。
    pub async fn delete_queue(&self, id: &str) -> Result<Vec<String>, DbError> {
        let mut tx = self.pool.begin().await?;
        let moved_ids = sqlx::query_scalar(
            "UPDATE tasks SET queue_id = $1, queue_order = 0 WHERE queue_id = $2 RETURNING id",
        )
        .bind(MAIN_QUEUE_ID)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM queues WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(moved_ids)
    }

    /// Load all named queues ordered by position.
    pub async fn load_all_queues(&self) -> Result<Vec<QueueInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT id, name, speed_limit_kbps, upload_limit_kbps, max_concurrent, default_save_dir, position, default_segments, default_user_agent, is_running, schedule_enabled, schedule_start, schedule_stop, schedule_days
             FROM queues ORDER BY position ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut queues = Vec::with_capacity(rows.len());
        for row in &rows {
            queues.push(QueueInfo {
                queue_id: row.try_get("id")?,
                name: row.try_get("name")?,
                speed_limit_kbps: row.try_get("speed_limit_kbps")?,
                // 迁移新增列：旧库缺列时回退 0（不限）。
                upload_limit_kbps: row.try_get("upload_limit_kbps").unwrap_or(0),
                max_concurrent: row.try_get("max_concurrent")?,
                default_save_dir: row.try_get("default_save_dir")?,
                position: row.try_get("position")?,
                default_segments: row.try_get("default_segments")?,
                default_user_agent: row.try_get("default_user_agent")?,
                // 迁移新增列：与 task_from_row 同风格的防御性回退。
                is_running: row.try_get::<i32, _>("is_running").unwrap_or(1) != 0,
                schedule_enabled: row.try_get::<i32, _>("schedule_enabled").unwrap_or(0) != 0,
                schedule_start: row.try_get("schedule_start").unwrap_or_default(),
                schedule_stop: row.try_get("schedule_stop").unwrap_or_default(),
                schedule_days: row.try_get("schedule_days").unwrap_or(127),
            });
        }
        Ok(queues)
    }

    /// Move a task to a different queue, appending to the target queue's
    /// tail (`queue_order` = 目标队列现有最大值 +1)。
    pub async fn move_task_to_queue(&self, task_id: &str, queue_id: &str) -> Result<(), DbError> {
        let next_order: i64 = sqlx::query_scalar(
            "SELECT CAST(COALESCE(MAX(queue_order), 0) + 1 AS BIGINT) FROM tasks WHERE queue_id = $1",
        )
        .bind(queue_id)
        .fetch_one(&self.pool)
        .await?;
        sqlx::query("UPDATE tasks SET queue_id = $1, queue_order = $2 WHERE id = $3")
            .bind(queue_id)
            .bind(next_order as i32)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Count the number of rows currently in the queues table.
    pub async fn queue_count(&self) -> Result<i32, DbError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queues")
            .fetch_one(&self.pool)
            .await?;
        Ok(count as i32)
    }

    /// Count tasks currently at the given status (e.g. `1` = downloading).
    pub async fn count_tasks_by_status(&self, status: i32) -> Result<i64, DbError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE status = $1")
            .bind(status)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 更新队列运行状态（启动/停止队列的持久化半边）。
    pub async fn set_queue_running(&self, id: &str, running: bool) -> Result<(), DbError> {
        sqlx::query("UPDATE queues SET is_running = $1 WHERE id = $2")
            .bind(if running { 1i32 } else { 0i32 })
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 更新队列的每日定时计划。`start`/`stop` 为 `HH:MM`（空 = 不定时），
    /// `days` 为星期位掩码（bit0=周一 … bit6=周日）。
    pub async fn set_queue_schedule(
        &self,
        id: &str,
        enabled: bool,
        start: &str,
        stop: &str,
        days: i32,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE queues SET schedule_enabled = $1, schedule_start = $2, schedule_stop = $3, schedule_days = $4 WHERE id = $5",
        )
        .bind(if enabled { 1i32 } else { 0i32 })
        .bind(start)
        .bind(stop)
        .bind(days)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 队列启动时应恢复的任务 ID（status ∈ {0 pending, 2 paused}），按
    /// 队列内顺序（`queue_order` → `created_at` → `id`）排列。
    pub async fn queue_startable_task_ids(&self, queue_id: &str) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query_scalar(
            "SELECT id FROM tasks WHERE queue_id = $1 AND status IN (0, 2) ORDER BY queue_order ASC, created_at ASC, id ASC",
        )
        .bind(queue_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 「全局恢复」候选：所有 paused 任务中排除**已停止队列**内的任务
    /// （停止队列由「启动队列」显式恢复；孤儿 queue_id 视作运行中）。
    pub async fn eligible_resume_task_ids(&self) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query_scalar(
            "SELECT t.id FROM tasks t LEFT JOIN queues q ON t.queue_id = q.id \
             WHERE t.status = 2 AND COALESCE(q.is_running, 1) = 1 \
             ORDER BY t.queue_order ASC, t.created_at ASC, t.id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 持久化队列内任务顺序：把 `ordered_ids` 依次写为 1..N 的
    /// `queue_order`（仅更新仍属于该队列的行，容忍并发移动竞态）。
    pub async fn reorder_queue_tasks(
        &self,
        queue_id: &str,
        ordered_ids: &[String],
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        for (i, id) in ordered_ids.iter().enumerate() {
            sqlx::query("UPDATE tasks SET queue_order = $1 WHERE id = $2 AND queue_id = $3")
                .bind((i + 1) as i32)
                .bind(id.as_str())
                .bind(queue_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 播种内置队列（幂等、进程间安全）：
    /// - `main` 主队列（恒运行，无限制）——未指定队列的任务的归属；
    /// - `later` 稍后下载（默认停止）——「稍后下载」的默认落点。
    ///
    /// 同时把存量 `queue_id = ''` 任务迁入主队列，并在 `default_queue_id`
    /// 配置缺失时设为主队列。以 config 键 `builtin_queues_seeded` 的原子
    /// `INSERT … DO NOTHING` 作跨进程互斥：抢不到该行写入的进程直接跳过。
    ///
    /// 每次调用（每次启动）还会把主队列收敛到「运行中、无定时」
    /// （[`crate::model::is_always_running_queue`]），迁移旧版本允许的停止 / 定时状态。
    pub async fn seed_builtin_queues(&self) -> Result<(), DbError> {
        self.seed_builtin_queue_rows().await?;
        self.normalize_main_queue().await
    }

    /// 主队列恒运行且无定时：只在状态不符时写这一行（`WHERE` 过滤），已收敛的库
    /// 启动时零行写入。暂停中的任务保持暂停，不在升级时自动开下。
    async fn normalize_main_queue(&self) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE queues SET is_running = 1, schedule_enabled = 0, schedule_start = '', schedule_stop = '', schedule_days = 127 \
             WHERE id = $1 AND (is_running <> 1 OR schedule_enabled <> 0 OR schedule_start <> '' OR schedule_stop <> '' OR schedule_days <> 127)",
        )
        .bind(MAIN_QUEUE_ID)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn seed_builtin_queue_rows(&self) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        // 第一条语句即写入：SQLite 取得写锁、PostgreSQL 锁定冲突行，
        // 并发的第二个进程在此阻塞，提交后读到 rows_affected == 0 即退出。
        let claimed = sqlx::query(
            "INSERT INTO config (key, value) VALUES ('builtin_queues_seeded', '1') ON CONFLICT (key) DO NOTHING",
        )
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() == 0 {
            return Ok(()); // 已播种（tx 随 drop 回滚，无写入）
        }
        // 已有自定义队列整体后移，内置队列固定占位 0/1。
        sqlx::query("UPDATE queues SET position = position + 2")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO queues (id, name, position, is_running) VALUES ($1, $2, 0, 1) ON CONFLICT (id) DO NOTHING",
        )
        .bind(MAIN_QUEUE_ID)
        .bind("Main Queue")
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO queues (id, name, position, is_running) VALUES ($1, $2, 1, 0) ON CONFLICT (id) DO NOTHING",
        )
        .bind(crate::model::LATER_QUEUE_ID)
        .bind("Download Later")
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE tasks SET queue_id = $1 WHERE queue_id = ''")
            .bind(MAIN_QUEUE_ID)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO config (key, value) VALUES ('default_queue_id', $1) ON CONFLICT (key) DO NOTHING",
        )
        .bind(MAIN_QUEUE_ID)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 插入任务组行。`created_at` 内部生成（Unix 秒字符串），与
    /// [`Self::insert_task_with_tls_policy`] 惯例一致，不暴露给调用方。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), fluxdown_engine::db::DbError> {
    /// use fluxdown_engine::db::Db;
    /// let db = Db::connect("sqlite::memory:").await?;
    /// db.insert_group("g1", "我的相册", "https://pan.example.com/s/x", "/tmp/我的相册").await?;
    /// assert_eq!(db.load_all_groups().await?.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn insert_group(
        &self,
        id: &str,
        name: &str,
        source_url: &str,
        save_dir: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO task_groups (id, name, source_url, save_dir, created_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(name)
        .bind(source_url)
        .bind(save_dir)
        .bind(chrono_now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 全部任务组，按创建时间倒序（新建的组排前面，与 [`Self::load_all_tasks`]
    /// 惯例一致）。
    pub async fn load_all_groups(&self) -> Result<Vec<GroupInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT id, name, source_url, save_dir, created_at FROM task_groups ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut groups = Vec::with_capacity(rows.len());
        for row in &rows {
            groups.push(group_from_row(row)?);
        }
        Ok(groups)
    }

    /// 按 ID 读取单个任务组，不存在返回 `None`。
    pub async fn load_group_by_id(&self, id: &str) -> Result<Option<GroupInfo>, DbError> {
        let row = sqlx::query(
            "SELECT id, name, source_url, save_dir, created_at FROM task_groups WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(group_from_row).transpose()?)
    }

    /// 重命名任务组。空名校验由调用方负责
    /// （`DownloadManager::rename_group` 已 `trim` 拦截空名）。
    pub async fn rename_group(&self, id: &str, name: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE task_groups SET name = $1 WHERE id = $2")
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 设置任务的 `group_id` 归属（空串 = 移出所有组）。
    pub async fn set_task_group(&self, task_id: &str, group_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET group_id = $1 WHERE id = $2")
            .bind(group_id)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 设置任务的二段解析标识（不透明字符串，专用 getter
    /// [`Self::get_task_resolver_item`]，不进 [`TaskInfo`]，与
    /// `resolver_plugin_id`/[`Self::set_task_resolver`] 惯例一致）。
    pub async fn set_task_resolver_item(
        &self,
        task_id: &str,
        resolver_item: &str,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET resolver_item = $1 WHERE id = $2")
            .bind(resolver_item)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 读取任务的二段解析标识（空串 = 无）。
    pub async fn get_task_resolver_item(&self, task_id: &str) -> Result<String, DbError> {
        let row = sqlx::query("SELECT resolver_item FROM tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| r.try_get::<String, _>("resolver_item").unwrap_or_default())
            .unwrap_or_default())
    }

    /// 标记任务为无人值守创建（只置 1，不提供回退——任务级创建时决策）。
    pub async fn set_task_unattended(&self, task_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET unattended = 1 WHERE id = $1")
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 任务是否为无人值守创建（读不到/任务不存在按 false 兜底）。
    pub async fn is_task_unattended(&self, task_id: &str) -> Result<bool, DbError> {
        let row = sqlx::query("SELECT unattended FROM tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| r.try_get::<i32, _>("unattended").unwrap_or(0) != 0)
            .unwrap_or(false))
    }

    /// 读取逐任务「文件已存在」决定（'' / rename / overwrite / skip）；任务不存在按 '' 处理。
    pub async fn get_exists_decision(&self, task_id: &str) -> Result<String, DbError> {
        let row = sqlx::query("SELECT exists_decision FROM tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => Ok(r.try_get::<String, _>("exists_decision")?),
            None => Ok(String::new()),
        }
    }

    /// 写入逐任务「文件已存在」决定；空串 = 清除。
    pub async fn set_exists_decision(&self, task_id: &str, decision: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET exists_decision = $1 WHERE id = $2")
            .bind(decision)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 组内成员任务 ID，按启动顺序排列（`queue_order` → `created_at`）。
    pub async fn group_member_ids(&self, group_id: &str) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query_scalar(
            "SELECT id FROM tasks WHERE group_id = $1 ORDER BY queue_order ASC, created_at ASC",
        )
        .bind(group_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 回收无成员任务组（组生命周期 D8：末个成员删除时自动回收）。返回删除
    /// 的组行数，供调用方仅在真正发生回收时广播
    /// [`crate::events::EngineEvent::GroupsChanged`]。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), fluxdown_engine::db::DbError> {
    /// use fluxdown_engine::db::Db;
    /// let db = Db::connect("sqlite::memory:").await?;
    /// let deleted = db.gc_empty_groups().await?;
    /// assert_eq!(deleted, 0); // 没有任务组时是无操作
    /// # Ok(())
    /// # }
    /// ```
    pub async fn gc_empty_groups(&self) -> Result<u64, DbError> {
        let result = sqlx::query(
            "DELETE FROM task_groups WHERE NOT EXISTS (SELECT 1 FROM tasks WHERE tasks.group_id = task_groups.id)",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// 单条目清单原地改写任务的落盘目标（外部无 UI 入口的自动裂变单条目
    /// 分支，D6）：不建组，仅覆盖 `save_dir`/`file_name`/`total_bytes`/
    /// `category_route`，`checksum` 清空（旧值针对改写前的下载目标，保留必致校验失败）。
    pub async fn rewrite_task_for_item(
        &self,
        id: &str,
        save_dir: &str,
        file_name: &str,
        total_bytes: i64,
        category_route: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET save_dir = $1, file_name = $2, total_bytes = $3, checksum = '', \
             category_route = $4 WHERE id = $5",
        )
        .bind(save_dir)
        .bind(file_name)
        .bind(total_bytes)
        .bind(category_route)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 任务的分类目录路由列值（空串 = 不路由 / 任务不存在）。
    pub async fn load_task_category_route(&self, id: &str) -> Result<String, DbError> {
        let route: Option<String> =
            sqlx::query_scalar("SELECT category_route FROM tasks WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(route.unwrap_or_default())
    }
    /// 持久化单个任务的分类与归档路由选择。
    pub async fn set_task_category_route(&self, id: &str, route: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET category_route = $1 WHERE id = $2")
            .bind(route)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 任务落库的非 GET 请求方法（空串 = GET / 任务不存在）。
    pub async fn load_task_request_method(&self, id: &str) -> Result<String, DbError> {
        let method: Option<String> =
            sqlx::query_scalar("SELECT request_method FROM tasks WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(method.unwrap_or_default())
    }

    /// 产物移入分类目录后同步任务位置（单条 UPDATE，目录与文件名不会分离）。
    pub async fn set_task_location(
        &self,
        id: &str,
        save_dir: &str,
        file_name: &str,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET save_dir = $1, file_name = $2 WHERE id = $3")
            .bind(save_dir)
            .bind(file_name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 多条目清单裂变为任务组（外部无 UI 入口的自动裂变多条目分支，D6）：
    /// 单事务内建组行 + 改写母任务 + 批量插入兄弟任务（复制母行的队列/代理/
    /// TLS 策略/分段数/resolver 插件绑定/请求上下文列），失败整体回滚
    /// （RAII：任何 `?` 早返回时 Drop 自动 ROLLBACK，母任务保持改写前状态，
    /// 调用方据此按 status=4 兜底，见 `on_resolve_ready`）。
    pub async fn fission_into_group(&self, spec: &FissionSpec) -> Result<(), DbError> {
        let mut tx = self.begin_write().await?;
        let row = sqlx::query(
            "SELECT url, proxy_url, queue_id, ignore_tls_errors, segments, resolver_plugin_id, \
             cookies, referrer, extra_headers, queue_order FROM tasks WHERE id = $1",
        )
        .bind(&spec.mother_task_id)
        .fetch_one(&mut *tx)
        .await?;
        let url: String = row.try_get("url")?;
        let proxy_url: String = row.try_get("proxy_url").unwrap_or_default();
        let queue_id: String = row.try_get("queue_id").unwrap_or_default();
        let ignore_tls_errors: i32 = row.try_get::<i32, _>("ignore_tls_errors").unwrap_or(0);
        let segments: i32 = row.try_get("segments").unwrap_or(0);
        let resolver_plugin_id: String = row.try_get("resolver_plugin_id").unwrap_or_default();
        let cookies: String = row.try_get("cookies").unwrap_or_default();
        let referrer: String = row.try_get("referrer").unwrap_or_default();
        let extra_headers: String = row.try_get("extra_headers").unwrap_or_default();
        let base_order: i32 = row.try_get("queue_order").unwrap_or(0);

        let now = chrono_now();
        sqlx::query(
            "INSERT INTO task_groups (id, name, source_url, save_dir, created_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&spec.group_id)
        .bind(&spec.group_name)
        .bind(&spec.group_source_url)
        .bind(&spec.group_save_dir)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        // 母任务成为组成员：组以基准目录为根、成员保持组内结构，不再按分类路由。
        sqlx::query(
            "UPDATE tasks SET group_id = $1, resolver_item = $2, file_name = $3, save_dir = $4, \
             total_bytes = $5, checksum = '', status = $6, category_route = '' WHERE id = $7",
        )
        .bind(&spec.group_id)
        .bind(&spec.mother_resolver_item)
        .bind(&spec.mother_file_name)
        .bind(&spec.mother_save_dir)
        .bind(spec.mother_total_bytes)
        .bind(spec.mother_status)
        .bind(&spec.mother_task_id)
        .execute(&mut *tx)
        .await?;

        for (i, sib) in spec.siblings.iter().enumerate() {
            let queue_order = base_order + 1 + i as i32;
            sqlx::query(
                "INSERT INTO tasks (id, url, file_name, save_dir, status, segments, total_bytes, \
                 created_at, proxy_url, queue_id, checksum, ignore_tls_errors, queue_order, \
                 group_id, resolver_item, resolver_plugin_id, cookies, referrer, extra_headers) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, '', $11, $12, $13, $14, $15, $16, $17, $18)",
            )
            .bind(&sib.id)
            .bind(&url)
            .bind(&sib.file_name)
            .bind(&sib.save_dir)
            .bind(sib.status)
            .bind(segments)
            .bind(sib.total_bytes)
            .bind(&now)
            .bind(&proxy_url)
            .bind(&queue_id)
            .bind(ignore_tls_errors)
            .bind(queue_order)
            .bind(&spec.group_id)
            .bind(&sib.resolver_item)
            .bind(&resolver_plugin_id)
            .bind(&cookies)
            .bind(&referrer)
            .bind(&extra_headers)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    /// 读取旧版遗留的已配对设备名册（按最近活跃降序），仅供 daemon 向 agent
    /// 一次性迁移（`link_migration_export`）；互联状态现由 agent 自行持久化。
    pub async fn link_load_devices(&self) -> Result<Vec<LinkDeviceRow>, DbError> {
        let rows = sqlx::query(
            "SELECT fingerprint, identity_pub, name, platform, link_secret, candidates, paired_at, last_seen_at
             FROM link_devices ORDER BY last_seen_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(link_device_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // RSS subscription CRUD（`rss_sources` / `rss_items`）
    // -----------------------------------------------------------------------

    /// 插入一条订阅。`source.source_id` 由调用方生成（UUID）。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), fluxdown_engine::db::DbError> {
    /// use fluxdown_engine::db::Db;
    /// use fluxdown_engine::rss::model::RssSourceInfo;
    ///
    /// let db = Db::connect("sqlite::memory:").await?;
    /// db.insert_rss_source(&RssSourceInfo {
    ///     source_id: "s1".to_string(),
    ///     url: "https://mikanani.me/RSS/MyBangumi?token=x".to_string(),
    ///     ..Default::default()
    /// })
    /// .await?;
    /// assert_eq!(db.load_all_rss_sources().await?.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn insert_rss_source(&self, source: &RssSourceInfo) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO rss_sources (id, provider_id, provider_config, url, name, enabled, auto_download, start_paused, queue_id, save_dir, \
              interval_minutes, include_pattern, exclude_pattern, use_regex, smart_episode, size_min_bytes, \
             size_max_bytes, send_referer, notify_on_download, max_per_fetch, cookies, user_agent, proxy_url, \
             last_fetch_at, last_success_at, last_error, fail_count, seeded, position) \
              VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, \
              $21, $22, $23, $24, $25, $26, $27, $28, $29)",
        )
        .bind(&source.source_id)
        .bind(&source.provider_id)
        .bind(&source.provider_config)
        .bind(&source.url)
        .bind(&source.name)
        .bind(i32::from(source.enabled))
        .bind(i32::from(source.auto_download))
        .bind(i32::from(source.start_paused))
        .bind(&source.queue_id)
        .bind(&source.save_dir)
        .bind(source.interval_minutes)
        .bind(&source.include_pattern)
        .bind(&source.exclude_pattern)
        .bind(i32::from(source.use_regex))
        .bind(i32::from(source.smart_episode))
        .bind(source.size_min_bytes)
        .bind(source.size_max_bytes)
        .bind(i32::from(source.send_referer))
        .bind(i32::from(source.notify_on_download))
        .bind(source.max_per_fetch)
        .bind(&source.cookies)
        .bind(&source.user_agent)
        .bind(&source.proxy_url)
        .bind(source.last_fetch_at)
        .bind(source.last_success_at)
        .bind(&source.last_error)
        .bind(source.fail_count)
        .bind(i32::from(source.seeded))
        .bind(source.position)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 更新订阅的**用户可编辑字段**（运行态 `last_*`/`fail_count`/`seeded`
    /// 不在此列，由 [`Self::set_rss_source_runtime`] 单独维护——否则一次
    /// UI 保存会把正在进行的退避账本抹掉）。
    pub async fn update_rss_source(&self, source: &RssSourceInfo) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE rss_sources SET provider_id = $1, provider_config = $2, url = $3, name = $4, enabled = $5, auto_download = $6, start_paused = $7, \
             queue_id = $8, save_dir = $9, interval_minutes = $10, include_pattern = $11, exclude_pattern = $12, \
             use_regex = $13, smart_episode = $14, size_min_bytes = $15, size_max_bytes = $16, send_referer = $17, \
             notify_on_download = $18, max_per_fetch = $19, cookies = $20, user_agent = $21, proxy_url = $22 \
             WHERE id = $23",
        )
        .bind(&source.provider_id)
        .bind(&source.provider_config)
        .bind(&source.url)
        .bind(&source.name)
        .bind(i32::from(source.enabled))
        .bind(i32::from(source.auto_download))
        .bind(i32::from(source.start_paused))
        .bind(&source.queue_id)
        .bind(&source.save_dir)
        .bind(source.interval_minutes)
        .bind(&source.include_pattern)
        .bind(&source.exclude_pattern)
        .bind(i32::from(source.use_regex))
        .bind(i32::from(source.smart_episode))
        .bind(source.size_min_bytes)
        .bind(source.size_max_bytes)
        .bind(i32::from(source.send_referer))
        .bind(i32::from(source.notify_on_download))
        .bind(source.max_per_fetch)
        .bind(&source.cookies)
        .bind(&source.user_agent)
        .bind(&source.proxy_url)
        .bind(&source.source_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 抓取结束后回写运行态（成功/失败共用一条 UPDATE，避免两次写）。
    #[allow(clippy::too_many_arguments)]
    pub async fn set_rss_source_runtime(
        &self,
        id: &str,
        last_fetch_at: i64,
        last_success_at: i64,
        last_error: &str,
        fail_count: i32,
        seeded: bool,
        name: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE rss_sources SET last_fetch_at = $1, last_success_at = $2, last_error = $3, \
             fail_count = $4, seeded = $5, name = $6 WHERE id = $7",
        )
        .bind(last_fetch_at)
        .bind(last_success_at)
        .bind(last_error)
        .bind(fail_count)
        .bind(i32::from(seeded))
        .bind(name)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 删除订阅及其全部条目。**已创建的下载任务不删**（§2.2），只把任务上的
    /// 溯源指针清空，避免详情面板指向一条不存在的订阅。
    pub async fn delete_rss_source(&self, id: &str) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE tasks SET rss_source_id = '' WHERE rss_source_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM rss_items WHERE source_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM rss_sources WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 载入全部订阅（按 `position` 排序），顺带算出侧边栏 badge 用的未读
    /// 计数（`unread_count`）——与配置同批返回，避免 UI 两段式闪烁。
    pub async fn load_all_rss_sources(&self) -> Result<Vec<RssSourceInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT s.id, s.provider_id, s.provider_config, s.url, s.name, s.enabled, s.auto_download, s.start_paused, s.queue_id, s.save_dir, \
             s.interval_minutes, s.include_pattern, s.exclude_pattern, s.use_regex, s.smart_episode, \
             s.size_min_bytes, s.size_max_bytes, s.send_referer, s.notify_on_download, s.max_per_fetch, \
             s.cookies, s.user_agent, s.proxy_url, s.last_fetch_at, s.last_success_at, s.last_error, \
             s.fail_count, s.seeded, s.position, \
             CAST((SELECT COUNT(*) FROM rss_items i WHERE i.source_id = s.id AND i.status = 0) AS BIGINT) AS unread \
             FROM rss_sources s ORDER BY s.position ASC, s.id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut sources = Vec::with_capacity(rows.len());
        for row in &rows {
            sources.push(RssSourceInfo {
                source_id: row.try_get("id")?,
                provider_id: row
                    .try_get("provider_id")
                    .unwrap_or_else(|_| "rss".to_string()),
                provider_config: row.try_get("provider_config").unwrap_or_default(),
                url: row.try_get("url")?,
                name: row.try_get("name")?,
                enabled: row.try_get::<i32, _>("enabled").unwrap_or(1) != 0,
                auto_download: row.try_get::<i32, _>("auto_download").unwrap_or(1) != 0,
                start_paused: row.try_get::<i32, _>("start_paused").unwrap_or(0) != 0,
                queue_id: row.try_get("queue_id").unwrap_or_default(),
                save_dir: row.try_get("save_dir").unwrap_or_default(),
                interval_minutes: row.try_get("interval_minutes").unwrap_or(30),
                include_pattern: row.try_get("include_pattern").unwrap_or_default(),
                exclude_pattern: row.try_get("exclude_pattern").unwrap_or_default(),
                use_regex: row.try_get::<i32, _>("use_regex").unwrap_or(0) != 0,
                smart_episode: row.try_get::<i32, _>("smart_episode").unwrap_or(0) != 0,
                size_min_bytes: row.try_get("size_min_bytes").unwrap_or(0),
                size_max_bytes: row.try_get("size_max_bytes").unwrap_or(0),
                send_referer: row.try_get::<i32, _>("send_referer").unwrap_or(1) != 0,
                notify_on_download: row.try_get::<i32, _>("notify_on_download").unwrap_or(1) != 0,
                max_per_fetch: row.try_get("max_per_fetch").unwrap_or(20),
                cookies: row.try_get("cookies").unwrap_or_default(),
                user_agent: row.try_get("user_agent").unwrap_or_default(),
                proxy_url: row.try_get("proxy_url").unwrap_or_default(),
                last_fetch_at: row.try_get("last_fetch_at").unwrap_or(0),
                last_success_at: row.try_get("last_success_at").unwrap_or(0),
                last_error: row.try_get("last_error").unwrap_or_default(),
                fail_count: row.try_get("fail_count").unwrap_or(0),
                seeded: row.try_get::<i32, _>("seeded").unwrap_or(0) != 0,
                position: row.try_get("position").unwrap_or(0),
                unread_count: row.try_get::<i64, _>("unread").unwrap_or(0) as i32,
            });
        }
        Ok(sources)
    }

    /// 新订阅的排序位（现有最大值 +1）。
    pub async fn next_rss_position(&self) -> Result<i32, DbError> {
        let next: i64 = sqlx::query_scalar(
            "SELECT CAST(COALESCE(MAX(position), -1) + 1 AS BIGINT) FROM rss_sources",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(next as i32)
    }

    /// 一个订阅中**待派发**的条目：`status = New` 且不在种子抓取退避期内
    /// （`retry_after <= now`），按发布时间**从旧到新**取前 `limit` 条。
    ///
    /// 从旧到新是刻意的：单轮上限（`max_per_fetch`）把超额条目留在 New 状态
    /// 等下一轮，若按新→旧取，积压的老条目会被后来的新条目永久插队饿死。
    /// 带失败码的条目排在没失败过的条目之后，退避到期重试的坏条目不会占住队首。
    pub async fn rss_dispatchable_items(
        &self,
        source_id: &str,
        limit: i32,
        now: i64,
    ) -> Result<Vec<RssItemInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT source_id, guid, title, link, enclosure_url, enclosure_type, enclosure_length, pub_date, fetched_at, \
             status, task_id, episode_key, resolver_item, reason FROM rss_items \
             WHERE source_id = $1 AND status = 0 AND retry_after <= $3 \
             ORDER BY CASE WHEN reason = '' THEN 0 ELSE 1 END ASC, pub_date ASC, fetched_at ASC, guid ASC LIMIT $2",
        )
        .bind(source_id)
        .bind(limit.max(1))
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(rss_item_from_row)
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }

    /// 一个订阅的条目流（新→旧，最多 `limit` 条）。
    pub async fn load_rss_items(
        &self,
        source_id: &str,
        limit: i32,
    ) -> Result<Vec<RssItemInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT source_id, guid, title, link, enclosure_url, enclosure_type, enclosure_length, pub_date, fetched_at, \
             status, task_id, episode_key, resolver_item, reason FROM rss_items WHERE source_id = $1 \
             ORDER BY pub_date DESC, fetched_at DESC, guid ASC LIMIT $2",
        )
        .bind(source_id)
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(rss_item_from_row)
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }

    /// 单条目（手动下载/忽略前的存在性检查）。
    pub async fn rss_item(
        &self,
        source_id: &str,
        guid: &str,
    ) -> Result<Option<RssItemInfo>, DbError> {
        let items = sqlx::query(
            "SELECT source_id, guid, title, link, enclosure_url, enclosure_type, enclosure_length, pub_date, fetched_at, \
             status, task_id, episode_key, resolver_item, reason FROM rss_items WHERE source_id = $1 AND guid = $2",
        )
        .bind(source_id)
        .bind(guid)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = items else { return Ok(None) };
        Ok(Some(rss_item_from_row(&row)?))
    }

    /// 该源已入库的全部 guid（去重判定的第一层）。
    pub async fn rss_known_guids(&self, source_id: &str) -> Result<HashSet<String>, DbError> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT guid FROM rss_items WHERE source_id = $1")
                .bind(source_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }

    /// 该源**已被占用**的剧集键（智能剧集去重的第二层）。
    ///
    /// 占键的只有 `New`（已通过规则、等待派发）与 `Downloaded`（已建任务）
    /// 两态：被判重的输家、被过滤的、被手动忽略的都不占——否则一次误判会
    /// 永久锁死这一集，而用户手动「忽略」某个字幕组版本后也该允许别的版本
    /// 补位。
    pub async fn rss_taken_episode_keys(
        &self,
        source_id: &str,
    ) -> Result<HashSet<String>, DbError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT episode_key FROM rss_items WHERE source_id = $1 AND episode_key <> '' AND status IN (0, 1)",
        )
        .bind(source_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// 批量落库新条目（已存在的 guid 原样保留——同 guid 内容变化不重下，
    /// guid 即身份，§2.2）。返回实际插入的行数。
    pub async fn insert_rss_items(&self, items: &[RssItemInfo]) -> Result<u64, DbError> {
        if items.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut inserted = 0u64;
        for item in items {
            let r = sqlx::query(
                "INSERT INTO rss_items (source_id, guid, title, link, enclosure_url, enclosure_type, resolver_item, \
                 enclosure_length, pub_date, fetched_at, status, task_id, episode_key, reason) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
                 ON CONFLICT (source_id, guid) DO NOTHING",
            )
            .bind(&item.source_id)
            .bind(&item.guid)
            .bind(&item.title)
            .bind(&item.link)
            .bind(&item.enclosure_url)
            .bind(&item.enclosure_type)
            .bind(&item.resolver_item)
            .bind(item.enclosure_length)
            .bind(item.pub_date)
            .bind(item.fetched_at)
            .bind(item.status.as_i32())
            .bind(&item.task_id)
            .bind(&item.episode_key)
            .bind(&item.reason)
            .execute(&mut *tx)
            .await?;
            inserted += r.rows_affected();
        }
        tx.commit().await?;
        Ok(inserted)
    }

    /// 回填历史条目缺失的发布时间。
    ///
    /// 已知 guid 在抓取阶段就被整条跳过（guid 即身份，内容变化不重下），所以
    /// 解析器新补上的扩展 `pubDate`（Mikan `<torrent>`）到不了 `INSERT`。这里
    /// 专门只补 `pub_date = 0` 的行：非零值一律尊重，绝不覆盖。
    pub async fn backfill_rss_pub_dates(
        &self,
        source_id: &str,
        items: &[(String, i64)],
    ) -> Result<u64, DbError> {
        if items.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut updated = 0u64;
        for (guid, pub_date) in items {
            let r = sqlx::query(
                "UPDATE rss_items SET pub_date = $1 \
                 WHERE source_id = $2 AND guid = $3 AND pub_date = 0",
            )
            .bind(*pub_date)
            .bind(source_id)
            .bind(guid)
            .execute(&mut *tx)
            .await?;
            updated += r.rows_affected();
        }
        tx.commit().await?;
        Ok(updated)
    }

    /// 回填历史插件条目缺失的二段解析标识。
    ///
    /// 订阅条目的 guid 是身份，插件升级后相同 guid 不会重新插入；仅在历史行
    /// 还没有 resolver_item 时补写，保证已有订阅可以无缝切换到精确画质解析。
    pub async fn backfill_rss_resolver_items(
        &self,
        source_id: &str,
        items: &[(String, String)],
    ) -> Result<u64, DbError> {
        if items.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut updated = 0u64;
        for (guid, resolver_item) in items {
            let r = sqlx::query(
                "UPDATE rss_items SET resolver_item = $1 \
                 WHERE source_id = $2 AND guid = $3 AND resolver_item = ''",
            )
            .bind(resolver_item)
            .bind(source_id)
            .bind(guid)
            .execute(&mut *tx)
            .await?;
            updated += r.rows_affected();
        }
        tx.commit().await?;
        Ok(updated)
    }

    /// 改写单条目的处置结果。
    pub async fn set_rss_item_status(
        &self,
        source_id: &str,
        guid: &str,
        status: RssItemStatus,
        reason: &str,
        task_id: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE rss_items SET status = $1, reason = $2, task_id = $3 WHERE source_id = $4 AND guid = $5",
        )
        .bind(status.as_i32())
        .bind(reason)
        .bind(task_id)
        .bind(source_id)
        .bind(guid)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 记一次种子抓取失败：条目保持 `New`、写入失败码，失败计数 +1，并按
    /// `delay_for(新计数)` 秒设置下次允许自动派发的时刻（指数退避）。返回新计数。
    pub async fn record_rss_fetch_failure(
        &self,
        source_id: &str,
        guid: &str,
        reason: &str,
        now: i64,
        delay_for: fn(i64) -> i64,
    ) -> Result<i64, DbError> {
        let mut tx = self.pool.begin().await?;
        let previous: Option<i64> = sqlx::query_scalar(
            "SELECT fetch_failures FROM rss_items WHERE source_id = $1 AND guid = $2",
        )
        .bind(source_id)
        .bind(guid)
        .fetch_optional(&mut *tx)
        .await?;
        let failures = previous.unwrap_or(0).saturating_add(1);
        sqlx::query(
            "UPDATE rss_items SET status = $1, reason = $2, fetch_failures = $3, retry_after = $4 \
             WHERE source_id = $5 AND guid = $6",
        )
        .bind(RssItemStatus::New.as_i32())
        .bind(reason)
        .bind(failures)
        .bind(now.saturating_add(delay_for(failures)))
        .bind(source_id)
        .bind(guid)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(failures)
    }

    /// 清零条目的种子抓取失败计数与退避（抓取成功建出任务，或用户手动下载）。
    pub async fn clear_rss_item_backoff(&self, source_id: &str, guid: &str) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE rss_items SET fetch_failures = 0, retry_after = 0 \
             WHERE source_id = $1 AND guid = $2 AND (fetch_failures <> 0 OR retry_after <> 0)",
        )
        .bind(source_id)
        .bind(guid)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 把该源全部「新」条目标记为已忽略（工具条「全部标记已读」）。
    /// 返回受影响行数。
    pub async fn mark_all_rss_items_read(&self, source_id: &str) -> Result<u64, DbError> {
        let r = sqlx::query("UPDATE rss_items SET status = $1 WHERE source_id = $2 AND status = 0")
            .bind(RssItemStatus::Ignored.as_i32())
            .bind(source_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// 条目保留策略：每源只留最近 `keep` 条，超量的**非已下载**条目按发布/
    /// 抓取时间淘汰最旧的（已下载条目是任务溯源的锚，永不淘汰）。
    pub async fn prune_rss_items(&self, source_id: &str, keep: i32) -> Result<u64, DbError> {
        let r = sqlx::query(
            "DELETE FROM rss_items WHERE source_id = $1 AND status <> 1 AND guid NOT IN ( \
             SELECT guid FROM rss_items WHERE source_id = $1 \
             ORDER BY pub_date DESC, fetched_at DESC, guid ASC LIMIT $2)",
        )
        .bind(source_id)
        .bind(keep.max(1))
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// 打上任务的 RSS 溯源指针。
    pub async fn set_task_rss_source(&self, task_id: &str, source_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET rss_source_id = $1 WHERE id = $2")
            .bind(source_id)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 写入展示用的原始来源链接（`url` 被换成本地哨兵时的补偿）。
    pub async fn set_task_origin_url(&self, task_id: &str, origin: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET origin_url = $1 WHERE id = $2")
            .bind(origin)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 更换任务下载源地址（`change_task_url` 专用）并标记 `source_changed`；不触碰
    /// 分段/进度行（`task_segments` 按 `task_id` 索引，与 `url` 无关）。续传时由
    /// [`Self::settle_source_change`] 决定已下进度能否沿用。用户换上的是普通链接：同时
    /// 清掉浏览器接管时记录的非 GET 方法（`request_method`），新地址按 GET 续传。
    pub async fn update_task_url(&self, task_id: &str, url: &str) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET url = $1, source_changed = 1, request_method = '' WHERE id = $2",
        )
        .bind(url)
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 任务是否带未结算的换源标记（见 [`Self::settle_source_change`]）；任务不存在为 `false`。
    pub async fn task_source_changed(&self, task_id: &str) -> Result<bool, DbError> {
        let flag: Option<i32> =
            sqlx::query_scalar("SELECT source_changed FROM tasks WHERE id = $1")
                .bind(task_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(flag.is_some_and(|flag| flag != 0))
    }

    /// 换源后的首次续传探测结算（单事务读-判-写）。仅当任务带 `source_changed` 时生效：
    /// - 新旧总大小都已知且**不完全相等**（零容差，不套用 CDN 漂移容差）→ 视为已换成
    ///   另一个文件：删除分段行、已下字节与来源计数清零、`total_bytes` 改为新值，返回
    ///   `true`（调用方须删除旧临时文件后从头下载）；
    /// - 其余（大小一致或任一方未知，无证据判定换了文件）→ 沿用进度，返回 `false`。
    ///
    /// 两种情形都清除标记：此后的续传恢复常规容差。
    pub async fn settle_source_change(
        &self,
        task_id: &str,
        probed_total_bytes: i64,
    ) -> Result<bool, DbError> {
        let mut tx = self.begin_write().await?;
        let row = sqlx::query("SELECT source_changed, total_bytes FROM tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(false);
        };
        let flagged: i32 = row.try_get("source_changed")?;
        if flagged == 0 {
            tx.commit().await?;
            return Ok(false);
        }
        let stored_total: i64 = row.try_get("total_bytes")?;
        let replaced =
            stored_total > 0 && probed_total_bytes > 0 && stored_total != probed_total_bytes;
        if replaced {
            sqlx::query("DELETE FROM task_segments WHERE task_id = $1")
                .bind(task_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE tasks SET downloaded_bytes = 0, total_bytes = $1,
                     src_cdn_bytes = 0, src_proxy_bytes = 0, src_nic_bytes = 0,
                     source_changed = 0
                 WHERE id = $2",
            )
            .bind(probed_total_bytes)
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query("UPDATE tasks SET source_changed = 0 WHERE id = $1")
                .bind(task_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(replaced)
    }

    /// 写入 `ProxyMode::Auto` 的任务级最终链路标签（wire 值见
    /// `auto_proxy::route`；空 = 非 Auto 模式）。任务启动时由 manager
    /// 重写基线，运行中主导链路变化由 coordinator 多路径调度更新。
    pub async fn set_task_auto_route(&self, task_id: &str, route: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET auto_route = $1 WHERE id = $2")
            .bind(route)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Webhook 投递日志
    //
    // 落库而不是只留内存环：用户配完端点常常隔天才回来看「昨晚那批到底发出
    // 去没有」，重启清零等于这个面板在最需要它的时候是空的。
    // -----------------------------------------------------------------------

    /// 投递日志（新→旧，最多 `limit` 条）。
    pub async fn load_webhook_deliveries(
        &self,
        limit: i64,
    ) -> Result<Vec<WebhookDeliveryRow>, DbError> {
        let rows = sqlx::query(
            "SELECT delivery_id, timestamp_ms, event, endpoint_id, endpoint_name, url, \
             request_headers, request_body, status_code, response_body, latency_ms, \
             attempts, success, error FROM webhook_deliveries \
             ORDER BY timestamp_ms DESC, delivery_id DESC LIMIT $1",
        )
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(webhook_delivery_from_row)
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }

    /// 落一条投递记录，并把总量裁到 `keep` 条（超出的按时间从旧到新删）。
    ///
    /// 裁剪和插入放在同一个事务里：中途崩了要么两者都在、要么都不在，不会
    /// 留下一个「删了旧的但新的没进来」的空窗。
    pub async fn insert_webhook_delivery(
        &self,
        d: &WebhookDeliveryRow,
        keep: i64,
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO webhook_deliveries (delivery_id, timestamp_ms, event, endpoint_id, \
             endpoint_name, url, request_headers, request_body, status_code, response_body, \
             latency_ms, attempts, success, error) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
             ON CONFLICT (delivery_id) DO NOTHING",
        )
        .bind(&d.delivery_id)
        .bind(d.timestamp_ms)
        .bind(&d.event)
        .bind(&d.endpoint_id)
        .bind(&d.endpoint_name)
        .bind(&d.url)
        .bind(&d.request_headers)
        .bind(&d.request_body)
        .bind(d.status_code)
        .bind(&d.response_body)
        .bind(d.latency_ms)
        .bind(d.attempts)
        .bind(i32::from(d.success))
        .bind(&d.error)
        .execute(&mut *tx)
        .await?;
        // `LIMIT -1` 是 SQLite 的「无上限」写法，pg 不认，那边用 `LIMIT ALL`。
        let prune = match self.backend {
            Backend::Sqlite => {
                "DELETE FROM webhook_deliveries WHERE delivery_id IN ( \
                   SELECT delivery_id FROM webhook_deliveries \
                   ORDER BY timestamp_ms DESC, delivery_id DESC LIMIT -1 OFFSET $1)"
            }
            Backend::Postgres => {
                "DELETE FROM webhook_deliveries WHERE delivery_id IN ( \
                   SELECT delivery_id FROM webhook_deliveries \
                   ORDER BY timestamp_ms DESC, delivery_id DESC LIMIT ALL OFFSET $1)"
            }
        };
        sqlx::query(prune)
            .bind(keep.max(1))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 清空投递日志（用户显式点「清空」时才会发生）。
    pub async fn clear_webhook_deliveries(&self) -> Result<(), DbError> {
        sqlx::query("DELETE FROM webhook_deliveries")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// 一条已落库的投递记录。字段与 `webhook::WebhookDelivery` 一一对应；
/// db 层不依赖 webhook 模块的类型，转换在 `webhook.rs` 里做。
#[derive(Debug, Clone, Default)]
pub struct WebhookDeliveryRow {
    pub delivery_id: String,
    pub timestamp_ms: i64,
    pub event: String,
    pub endpoint_id: String,
    pub endpoint_name: String,
    pub url: String,
    pub request_headers: String,
    pub request_body: String,
    pub status_code: i32,
    pub response_body: String,
    pub latency_ms: i64,
    pub attempts: i32,
    pub success: bool,
    pub error: String,
}

fn webhook_delivery_from_row(row: &AnyRow) -> Result<WebhookDeliveryRow, sqlx::Error> {
    Ok(WebhookDeliveryRow {
        delivery_id: row.try_get("delivery_id")?,
        timestamp_ms: row.try_get("timestamp_ms")?,
        event: row.try_get("event")?,
        endpoint_id: row.try_get("endpoint_id")?,
        endpoint_name: row.try_get("endpoint_name")?,
        url: row.try_get("url")?,
        request_headers: row.try_get("request_headers")?,
        request_body: row.try_get("request_body")?,
        status_code: row.try_get("status_code")?,
        response_body: row.try_get("response_body")?,
        latency_ms: row.try_get("latency_ms")?,
        attempts: row.try_get("attempts")?,
        success: row.try_get::<i32, _>("success")? != 0,
        error: row.try_get("error")?,
    })
}

pub struct SegmentInfo {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
}

/// 已配对设备的持久化行（link 名册）。`candidates` 为 JSON 数组文本；上层
/// （`link::store`）负责反序列化为强类型 `PeerRecord`。
pub struct LinkDeviceRow {
    pub fingerprint: String,
    pub identity_pub: Vec<u8>,
    pub name: String,
    pub platform: String,
    pub link_secret: Vec<u8>,
    pub candidates_json: String,
    pub paired_at: i64,
    pub last_seen_at: i64,
}

fn link_device_from_row(row: &AnyRow) -> Result<LinkDeviceRow, sqlx::Error> {
    Ok(LinkDeviceRow {
        fingerprint: row.try_get("fingerprint")?,
        identity_pub: row.try_get("identity_pub")?,
        name: row.try_get("name")?,
        platform: row.try_get("platform")?,
        link_secret: row.try_get("link_secret")?,
        candidates_json: row.try_get("candidates")?,
        paired_at: row.try_get("paired_at")?,
        last_seen_at: row.try_get("last_seen_at")?,
    })
}

/// 把 `AnyRow` 映射为 [`RssItemInfo`]（三处条目查询共用，列清单必须一致）。
fn rss_item_from_row(row: &AnyRow) -> Result<RssItemInfo, sqlx::Error> {
    Ok(RssItemInfo {
        source_id: row.try_get("source_id")?,
        guid: row.try_get("guid")?,
        title: row.try_get("title").unwrap_or_default(),
        link: row.try_get("link").unwrap_or_default(),
        enclosure_url: row.try_get("enclosure_url").unwrap_or_default(),
        resolver_item: row.try_get("resolver_item").unwrap_or_default(),
        enclosure_type: row.try_get("enclosure_type").unwrap_or_default(),
        enclosure_length: row.try_get("enclosure_length").unwrap_or(0),
        pub_date: row.try_get("pub_date").unwrap_or(0),
        fetched_at: row.try_get("fetched_at").unwrap_or(0),
        status: RssItemStatus::from_i32(row.try_get("status").unwrap_or(0)),
        task_id: row.try_get("task_id").unwrap_or_default(),
        episode_key: row.try_get("episode_key").unwrap_or_default(),
        reason: row.try_get("reason").unwrap_or_default(),
    })
}

fn chrono_now() -> String {
    let now = std::time::SystemTime::now();
    let since_epoch = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", since_epoch.as_secs())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Open a fresh Db in a unique temporary directory.
    /// Returns (Db, dir_path) — caller should clean up via [`close_test_db`].
    ///
    /// 目录名带纳秒时间戳：nextest 每测试一个进程（计数器恒 0），唯一性不能
    /// 押在 pid 上——Windows 激进复用 pid，历史上残留库（见 close_test_db）
    /// 会被 pid 复用后的测试进程继承，产生「多出陈旧任务行」的偶发失败。
    async fn open_test_db() -> (Db, std::path::PathBuf) {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_{}_{}_{}",
            std::process::id(),
            nanos,
            n
        ));
        // 双保险：目录已存在（极端命名碰撞/上次清理失败）时先清空。

        if let Err(error) = std::fs::remove_dir_all(&dir) {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "clean test path: {error}"
            );
        }
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db = Db::open(&dir).await.expect("open test db");
        (db, dir)
    }

    /// 关闭连接池后再删除测试目录。Windows 上 sqlite 文件句柄未释放时
    /// `remove_dir_all` 会静默失败——残留目录曾在 temp 累积 1400+ 个，且是
    /// 历史 flaky（陈旧库被继承）的根因，必须先 close 再删。
    async fn close_test_db(db: &Db, dir: std::path::PathBuf) {
        db.pool.close().await;

        if let Err(error) = std::fs::remove_dir_all(dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    async fn insert_task(db: &Db, id: &str) {
        db.insert_task(
            id,
            "http://example.com/file.bin",
            "file.bin",
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
    }

    /// 换源结算：仅换源后首次生效；大小（零容差）不一致即清空进度并改用新大小，
    /// 一致则沿用进度；结算后标记清除，未换源任务不受影响。
    #[tokio::test]
    async fn settle_source_change_resets_only_after_url_change_with_size_mismatch() {
        let (db, dir) = open_test_db().await;
        for id in ["swapped", "same", "untouched"] {
            insert_task(&db, id).await;
            db.update_task_total_bytes(id, 1_000_000)
                .await
                .expect("total");
            db.insert_segments(id, &[(0, 0, 499_999), (1, 500_000, 999_999)])
                .await
                .expect("segments");
        }
        db.update_task_url("swapped", "http://example.com/other.bin")
            .await
            .expect("swap url");
        db.update_task_url("same", "http://cdn.example.com/file.bin?sig=new")
            .await
            .expect("refresh url");

        // 新文件只大 1 KB：在 CDN 漂移容差内，换源后仍必须判为另一个文件。
        assert!(
            db.settle_source_change("swapped", 1_001_024)
                .await
                .expect("settle")
        );
        assert!(db.load_segments("swapped").await.expect("segs").is_empty());
        let swapped = db
            .load_task_by_id("swapped")
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(
            (swapped.total_bytes, swapped.downloaded_bytes),
            (1_001_024, 0)
        );
        // 标记已清除：后续续传回到常规容差路径。
        assert!(
            !db.settle_source_change("swapped", 2_000_000)
                .await
                .expect("settle")
        );

        assert!(
            !db.settle_source_change("same", 1_000_000)
                .await
                .expect("settle")
        );
        assert_eq!(db.load_segments("same").await.expect("segs").len(), 2);

        assert!(
            !db.settle_source_change("untouched", 2_000_000)
                .await
                .expect("settle")
        );
        assert_eq!(db.load_segments("untouched").await.expect("segs").len(), 2);

        close_test_db(&db, dir).await;
    }
    fn activity(
        task_id: &str,
        message: &str,
        timestamp_ms: i64,
    ) -> crate::task_activity::TaskActivity {
        crate::task_activity::TaskActivity {
            id: 0,
            task_id: task_id.to_owned(),
            timestamp_ms,
            kind: "retry".to_owned(),
            message: message.to_owned(),
            status: None,
        }
    }

    #[tokio::test]
    async fn activity_pagination_reopen_and_delete_preserve_monotonic_ids() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a").await;
        insert_task(&db, "b").await;
        let now = crate::task_activity::timestamp_ms();
        let mut ids = Vec::new();
        for (task_id, text) in [
            ("a", "first"),
            ("b", "other"),
            ("a", "second"),
            ("a", "third"),
            ("a", "fourth"),
        ] {
            ids.push(
                db.append_task_activity(&activity(task_id, text, now))
                    .await
                    .expect("append")
                    .expect("task exists")
                    .id,
            );
        }
        let query = crate::task_activity::TaskActivityQuery {
            task_id: "a".into(),
            limit: 2,
            ..Default::default()
        };
        let latest = db.query_task_activity(&query).await.expect("latest");
        assert_eq!(
            latest.entries.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![ids[3], ids[4]]
        );
        assert!(latest.has_more);
        assert_eq!(
            (latest.oldest_id, latest.newest_id),
            (Some(ids[0]), Some(ids[4]))
        );
        assert!(!latest.truncated);
        let older = db
            .query_task_activity(&crate::task_activity::TaskActivityQuery {
                before_id: Some(ids[3]),
                ..query.clone()
            })
            .await
            .expect("older");
        assert_eq!(
            older.entries.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![ids[0], ids[2]]
        );
        assert!(!older.has_more);
        let catchup = db
            .query_task_activity(&crate::task_activity::TaskActivityQuery {
                after_id: Some(ids[0]),
                ..query.clone()
            })
            .await
            .expect("catchup");
        assert_eq!(
            catchup.entries.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![ids[2], ids[3]]
        );
        assert!(catchup.has_more);
        assert!(matches!(
            db.query_task_activity(&crate::task_activity::TaskActivityQuery {
                before_id: Some(ids[4]),
                after_id: Some(ids[0]),
                ..query
            })
            .await,
            Err(DbError::InvalidActivityQuery(_))
        ));
        db.pool.close().await;
        let reopened = Db::open(&dir).await.expect("reopen");
        let page = reopened
            .query_task_activity(&crate::task_activity::TaskActivityQuery {
                task_id: "a".into(),
                ..Default::default()
            })
            .await
            .expect("persisted history");
        assert_eq!(page.entries.len(), 4);
        reopened.delete_task("a").await.expect("delete");
        assert!(
            reopened
                .query_task_activity(&crate::task_activity::TaskActivityQuery {
                    task_id: "a".into(),
                    ..Default::default()
                })
                .await
                .expect("history after deletion")
                .entries
                .is_empty()
        );
        insert_task(&reopened, "a").await;
        let new_id = reopened
            .append_task_activity(&activity("a", "new", now))
            .await
            .expect("append after delete")
            .expect("exists")
            .id;
        assert!(new_id > ids[4]);
        reopened
            .delete_tasks_batch(&["b".to_owned()])
            .await
            .expect("batch delete");
        assert!(
            reopened
                .query_task_activity(&crate::task_activity::TaskActivityQuery {
                    task_id: "b".into(),
                    ..Default::default()
                })
                .await
                .expect("history after batch deletion")
                .entries
                .is_empty()
        );
        close_test_db(&reopened, dir).await;
    }

    #[tokio::test]
    async fn activity_retention_reports_real_per_task_boundary() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a").await;
        insert_task(&db, "b").await;
        let now = crate::task_activity::timestamp_ms();
        let expired = db
            .append_task_activity(&activity(
                "a",
                "expired",
                now - TASK_ACTIVITY_MAX_AGE_MS - 1,
            ))
            .await
            .expect("append old")
            .expect("exists")
            .id;
        let first = db
            .append_task_activity(&activity("a", "first", now))
            .await
            .expect("append")
            .expect("exists")
            .id;
        let unrelated = db
            .append_task_activity(&activity("b", "unrelated", now))
            .await
            .expect("append")
            .expect("exists")
            .id;
        let second = db
            .append_task_activity(&activity("a", "second", now))
            .await
            .expect("append")
            .expect("exists")
            .id;
        {
            let _guard = db.activity_lock.lock().await;
            let mut tx = db.pool.begin().await.expect("retention transaction");
            prune_task_activity(&mut tx, now - TASK_ACTIVITY_MAX_AGE_MS, 2)
                .await
                .expect("prune capacity");
            tx.commit().await.expect("commit");
        }
        let page = db
            .query_task_activity(&crate::task_activity::TaskActivityQuery {
                task_id: "a".into(),
                after_id: Some(expired),
                ..Default::default()
            })
            .await
            .expect("after expired cursor");
        assert_eq!(
            page.entries.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![second]
        );
        assert!(page.truncated);
        assert_eq!(
            (page.oldest_id, page.newest_id),
            (Some(second), Some(second))
        );
        let other = db
            .query_task_activity(&crate::task_activity::TaskActivityQuery {
                task_id: "b".into(),
                ..Default::default()
            })
            .await
            .expect("other task");
        assert_eq!(other.entries[0].id, unrelated);
        assert!(!other.truncated);
        assert!(second > first);
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn activity_capacity_prune_has_hysteresis_and_aggregates_retention_per_task() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a").await;
        insert_task(&db, "b").await;
        let now = crate::task_activity::timestamp_ms();
        let cutoff = now - TASK_ACTIVITY_MAX_AGE_MS;
        // 容量 20 → 高水位 22；a/b 交替写入，共 22 条正好停在高水位，不清理。
        let first: Vec<_> = (0..22)
            .map(|i| activity(if i % 2 == 0 { "a" } else { "b" }, "m", now))
            .collect();
        let saved = db
            .append_task_activities_pruning(&first, cutoff, 20)
            .await
            .expect("append to high water");
        let ids: Vec<i64> = saved
            .iter()
            .map(|s| s.as_ref().expect("saved").id)
            .collect();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_activity")
            .fetch_one(&db.pool)
            .await
            .expect("count");
        assert_eq!(count, 22, "未超过高水位不得清理");
        let retention_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM task_activity_retention")
                .fetch_one(&db.pool)
                .await
                .expect("retention count");
        assert_eq!(retention_rows, 0);

        // 预置 b 的水位高于即将清理的边界，验证 upsert 取较大值而非被覆盖。
        let b_preset = ids[21] + 1_000;
        sqlx::query(
            "INSERT INTO task_activity_retention (task_id, pruned_through_id) VALUES ('b', $1)",
        )
        .bind(b_preset)
        .execute(&db.pool)
        .await
        .expect("preset retention");

        // 第 23 条越过高水位：一次清回 20 条，即删除最旧 3 条（ids[0..3]）。
        db.append_task_activities_pruning(&[activity("a", "m", now)], cutoff, 20)
            .await
            .expect("cross high water");
        let remaining: Vec<i64> = sqlx::query_scalar("SELECT id FROM task_activity ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .expect("remaining");
        assert_eq!(remaining.len(), 20);
        assert_eq!(remaining[0], ids[3]);
        // 被删：ids[0]=a, ids[1]=b, ids[2]=a → a 的边界是 ids[2]，b 的是 ids[1] 但预置更大。
        let a_through: i64 = sqlx::query_scalar(
            "SELECT pruned_through_id FROM task_activity_retention WHERE task_id = 'a'",
        )
        .fetch_one(&db.pool)
        .await
        .expect("a retention");
        assert_eq!(a_through, ids[2]);
        let b_through: i64 = sqlx::query_scalar(
            "SELECT pruned_through_id FROM task_activity_retention WHERE task_id = 'b'",
        )
        .fetch_one(&db.pool)
        .await
        .expect("b retention");
        assert_eq!(b_through, b_preset, "retention 不得回退");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn activity_batch_is_one_ordered_transaction_with_one_prune_and_no_orphans() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a").await;
        insert_task(&db, "b").await;
        let now = crate::task_activity::timestamp_ms();
        let batch = [
            activity("a", "a1", now),
            activity("ghost", "orphan", now),
            activity("b", "b1", now),
            activity("a", "a2", now),
            activity("a", "a3", now),
        ];
        // 容量 3：批内 4 条有效行，裁剪边界落在批内最早的一条（a1）上。
        let saved = db
            .append_task_activities_pruning(&batch, now - TASK_ACTIVITY_MAX_AGE_MS, 3)
            .await
            .expect("batch append");
        assert_eq!(saved.len(), batch.len());
        assert!(
            saved[1].is_none(),
            "deleted task must not get an orphan row"
        );
        let ids: Vec<i64> = [0, 2, 3, 4]
            .iter()
            .map(|&i| saved[i].as_ref().expect("task exists").id)
            .collect();
        assert!(
            ids.windows(2).all(|w| w[0] < w[1]),
            "ids follow batch order"
        );
        assert_eq!(saved[0].as_ref().map(|a| a.message.as_str()), Some("a1"));

        let remaining: Vec<i64> = sqlx::query_scalar("SELECT id FROM task_activity ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .expect("remaining rows");
        assert_eq!(remaining, ids[1..].to_vec());
        let pruned_through: i64 = sqlx::query_scalar(
            "SELECT pruned_through_id FROM task_activity_retention WHERE task_id = 'a'",
        )
        .fetch_one(&db.pool)
        .await
        .expect("retention row for a");
        assert_eq!(pruned_through, ids[0]);
        let b_retention: Option<i64> = sqlx::query_scalar(
            "SELECT pruned_through_id FROM task_activity_retention WHERE task_id = 'b'",
        )
        .fetch_optional(&db.pool)
        .await
        .expect("retention row for b");
        assert_eq!(b_retention, None);

        assert!(
            db.append_task_activities(&[])
                .await
                .expect("empty batch")
                .is_empty()
        );
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn activity_batch_rolls_back_entirely_when_the_store_fails() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a").await;
        let now = crate::task_activity::timestamp_ms();
        sqlx::query("DROP TABLE task_activity_retention")
            .execute(&db.pool)
            .await
            .expect("break retention bookkeeping");
        // 容量 1 的裁剪必然删除行并写 retention 表 → 失败 → 批内已插入的行必须回滚。
        let batch = [activity("a", "x", now), activity("a", "y", now)];
        assert!(
            db.append_task_activities_pruning(&batch, now - TASK_ACTIVITY_MAX_AGE_MS, 1)
                .await
                .is_err()
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_activity")
            .fetch_one(&db.pool)
            .await
            .expect("count");
        assert_eq!(rows, 0);
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn activity_journal_backlog_is_persisted_in_order_and_published_one_by_one() {
        struct RecordingSink(std::sync::Mutex<Vec<crate::task_activity::TaskActivity>>);
        impl crate::events::EventSink for RecordingSink {
            fn emit(&self, event: crate::events::EngineEvent) {
                if let crate::events::EngineEvent::TaskActivityAdded(activity) = event {
                    self.0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push(activity);
                }
            }
        }
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a").await;
        let sink = std::sync::Arc::new(RecordingSink(std::sync::Mutex::new(Vec::new())));
        let journal = crate::task_activity::JournalSink::start(db.clone(), sink.clone());
        // 积压远超单批上限：必须分批落库，且发布顺序 = 入队顺序。
        let total: i32 = 700;
        for index in 0..total {
            crate::events::EventSink::emit(
                journal.as_ref(),
                crate::events::EngineEvent::SegmentSplit {
                    task_id: "a".to_owned(),
                    parent_index: index,
                    parent_new_end: 0,
                    child_index: index + 1,
                    child_start: 0,
                    child_end: 0,
                    is_proactive: true,
                    total_segments: 2,
                },
            );
        }
        journal.flush().await.expect("flush backlog");
        let published = sink
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        assert_eq!(published.len(), total as usize);
        assert!(published.windows(2).all(|w| w[0].id < w[1].id));
        for (index, entry) in published.iter().enumerate() {
            assert!(
                entry
                    .message
                    .starts_with(&format!("segment {index} split ")),
                "entry {index}: {}",
                entry.message
            );
        }
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_activity")
            .fetch_one(&db.pool)
            .await
            .expect("count");
        assert_eq!(rows, i64::from(total));
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn activity_db_failure_does_not_commit_or_report_success() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a").await;
        struct RecordingSink(std::sync::Mutex<Vec<crate::task_activity::TaskActivity>>);
        impl crate::events::EventSink for RecordingSink {
            fn emit(&self, event: crate::events::EngineEvent) {
                if let crate::events::EngineEvent::TaskActivityAdded(entry) = event {
                    self.0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push(entry);
                }
            }
        }
        let sink = RecordingSink(std::sync::Mutex::new(Vec::new()));
        let saved = crate::task_activity::record(&db, &sink, "a", "retry", "HTTP 503", None)
            .await
            .expect("first record")
            .expect("task exists");
        assert_eq!(
            sink.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            &[saved]
        );

        sqlx::query("DROP TABLE task_activity")
            .execute(&db.pool)
            .await
            .expect("simulate broken activity store");
        assert!(
            crate::task_activity::record(&db, &sink, "a", "retry", "disk failure", None)
                .await
                .is_err()
        );
        assert_eq!(
            sink.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len(),
            1
        );
        close_test_db(&db, dir).await;
    }
    #[tokio::test]
    async fn activity_closed_pool_flush_returns_error_without_publishing_false_success() {
        struct RecordingSink(std::sync::Mutex<Vec<crate::task_activity::TaskActivity>>);
        impl crate::events::EventSink for RecordingSink {
            fn emit(&self, event: crate::events::EngineEvent) {
                if let crate::events::EngineEvent::TaskActivityAdded(activity) = event {
                    self.0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push(activity);
                }
            }
        }
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a").await;
        let sink = std::sync::Arc::new(RecordingSink(std::sync::Mutex::new(Vec::new())));
        let journal = crate::task_activity::JournalSink::start(db.clone(), sink.clone());
        db.pool.close().await;
        crate::events::EventSink::emit(
            journal.as_ref(),
            crate::events::EngineEvent::SegmentSplit {
                task_id: "a".to_owned(),
                parent_index: 0,
                parent_new_end: 99,
                child_index: 1,
                child_start: 100,
                child_end: 199,
                is_proactive: true,
                total_segments: 2,
            },
        );
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), journal.flush())
            .await
            .expect("flush must not hang on closed pool")
            .expect_err("storage failure must be reported");
        assert!(!error.is_empty());
        assert!(
            sink.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
        close_test_db(&db, dir).await;
    }
    #[tokio::test]
    async fn activity_status_edges_do_not_persist_periodic_progress_and_keep_full_error() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a").await;
        let journal = crate::task_activity::JournalSink::start(
            db.clone(),
            std::sync::Arc::new(crate::NoopSink),
        );
        let emit = |bytes, status, error: &str| {
            crate::events::EventSink::emit(
                journal.as_ref(),
                crate::events::EngineEvent::TaskProgress {
                    task_id: "a".to_owned(),
                    status,
                    downloaded_bytes: bytes,
                    total_bytes: 10_000,
                    speed: 100,
                    file_name: "a.bin".to_owned(),
                    save_dir: "/tmp".to_owned(),
                    url: "https://example.test/a.bin".to_owned(),
                    error_message: error.to_owned(),
                    upload_speed_bps: 0,
                    uploaded_bytes: 0,
                    seeding_status: 0,
                    seeding_message: String::new(),
                    seeding_time_secs: 0,
                },
            );
        };
        for i in 0..20 {
            emit(i * 100, 1, "");
        }
        let full_error = format!("HTTP 503: {}", "x".repeat(1024));
        emit(2_000, 4, &full_error);
        journal.flush().await.expect("persist all state edges");
        let page = db
            .query_task_activity(&crate::task_activity::TaskActivityQuery {
                task_id: "a".into(),
                ..Default::default()
            })
            .await
            .expect("history");
        assert_eq!(
            page.entries.len(),
            2,
            "periodic progress must not be journaled"
        );
        assert_eq!(page.entries[0].kind, "status");
        assert_eq!(page.entries[0].status, Some(1));
        assert!(
            page.entries[0].message.is_empty(),
            "status label belongs to localized UI"
        );
        assert_eq!(page.entries[1].kind, "error");
        assert_eq!(page.entries[1].message, full_error);
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn group_crud_roundtrip() {
        let (db, dir) = open_test_db().await;
        db.insert_group(
            "g1",
            "我的相册",
            "https://pan.example.com/s/x",
            "/tmp/我的相册",
        )
        .await
        .expect("insert group");
        let groups = db.load_all_groups().await.expect("load groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, "g1");
        assert_eq!(groups[0].name, "我的相册");

        let loaded = db.load_group_by_id("g1").await.expect("load by id");
        assert!(loaded.is_some());
        assert!(
            db.load_group_by_id("missing")
                .await
                .expect("load missing")
                .is_none()
        );

        db.rename_group("g1", "新名字").await.expect("rename");
        let renamed = db
            .load_group_by_id("g1")
            .await
            .expect("load renamed")
            .expect("exists");
        assert_eq!(renamed.name, "新名字");

        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn seed_upload_limit_column_roundtrip() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "t-seed").await;
        // 新库/迁移库默认 0（无单任务限制）。
        let task = db
            .load_task_by_id("t-seed")
            .await
            .expect("load task")
            .expect("task exists");
        assert_eq!(task.seed_upload_limit_bps, 0);

        db.set_task_seed_limits("t-seed", 1500, -1, -2, 30, 256_000)
            .await
            .expect("set seed limits");
        let task = db
            .load_task_by_id("t-seed")
            .await
            .expect("load task")
            .expect("task exists");
        assert_eq!(task.seed_ratio_limit_milli, 1500);
        assert_eq!(task.seed_inactive_time_limit_minutes, 30);
        assert_eq!(task.seed_upload_limit_bps, 256_000);

        // 负值钳到 0（无限制）；比率哨兵钳到 -2。
        db.set_task_seed_limits("t-seed", -9, -2, -2, -2, -5)
            .await
            .expect("set seed limits");
        let task = db
            .load_task_by_id("t-seed")
            .await
            .expect("load task")
            .expect("task exists");
        assert_eq!(task.seed_ratio_limit_milli, -2);
        assert_eq!(task.seed_upload_limit_bps, 0);

        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn piece_picking_update_leaves_the_omitted_option_alone() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "t-bt").await;
        let load = || async {
            let task = db
                .load_task_by_id("t-bt")
                .await
                .expect("load task")
                .expect("task exists");
            (task.bt_sequential, task.bt_first_last_pieces_first)
        };
        assert_eq!(load().await, (false, false), "rarest first by default");

        db.set_task_piece_picking("t-bt", Some(true), None)
            .await
            .expect("set sequential");
        assert_eq!(load().await, (true, false));
        db.set_task_piece_picking("t-bt", None, Some(true))
            .await
            .expect("set first and last");
        assert_eq!(load().await, (true, true));
        db.set_task_piece_picking("t-bt", Some(false), None)
            .await
            .expect("clear sequential");
        assert_eq!(load().await, (false, true));

        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn set_task_group_and_resolver_item_roundtrip() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "t1").await;
        db.insert_group("g1", "组", "", "/tmp/g1")
            .await
            .expect("insert group");
        db.set_task_group("t1", "g1").await.expect("set group");
        db.set_task_resolver_item("t1", "item1@1080p")
            .await
            .expect("set resolver item");

        let task = db
            .load_task_by_id("t1")
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(task.group_id, "g1");
        assert_eq!(
            db.get_task_resolver_item("t1").await.expect("get item"),
            "item1@1080p"
        );
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn group_member_ids_orders_by_queue_order() {
        let (db, dir) = open_test_db().await;
        db.insert_group("g1", "组", "", "/tmp/g1")
            .await
            .expect("insert group");
        insert_task(&db, "second").await;
        insert_task(&db, "first").await;
        db.set_task_group("second", "g1").await.expect("set group");
        db.set_task_group("first", "g1").await.expect("set group");
        // 显式 queue_order：first(1) 排在 second(2) 前面，即使插入顺序相反。
        db.reorder_queue_tasks("", &["first".to_string(), "second".to_string()])
            .await
            .expect("reorder");
        let ids = db.group_member_ids("g1").await.expect("member ids");
        assert_eq!(ids, vec!["first".to_string(), "second".to_string()]);
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn gc_empty_groups_deletes_orphan_group_rows_only() {
        let (db, dir) = open_test_db().await;
        db.insert_group("empty", "空组", "", "/tmp/empty")
            .await
            .expect("insert empty group");
        db.insert_group("populated", "有成员组", "", "/tmp/populated")
            .await
            .expect("insert populated group");
        insert_task(&db, "member").await;
        db.set_task_group("member", "populated")
            .await
            .expect("set group");

        let deleted = db.gc_empty_groups().await.expect("gc");
        assert_eq!(deleted, 1);
        assert!(db.load_group_by_id("empty").await.expect("load").is_none());
        assert!(
            db.load_group_by_id("populated")
                .await
                .expect("load")
                .is_some()
        );

        // 幂等：无孤儿组时返回 0。
        assert_eq!(db.gc_empty_groups().await.expect("gc again"), 0);
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn rewrite_task_for_item_overwrites_target_and_clears_checksum() {
        let (db, dir) = open_test_db().await;
        db.insert_task(
            "t1",
            "http://example.com/orig",
            "orig.bin",
            "/tmp",
            1,
            0,
            "",
            "",
            "sha256=deadbeef",
            0,
        )
        .await
        .expect("insert task");

        db.rewrite_task_for_item("t1", "/tmp/group/sub", "item.mp4", 12345, "")
            .await
            .expect("rewrite");

        let task = db
            .load_task_by_id("t1")
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(task.save_dir, "/tmp/group/sub");
        assert_eq!(task.file_name, "item.mp4");
        assert_eq!(task.total_bytes, 12345);
        assert_eq!(task.checksum, "");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn fission_into_group_creates_group_and_copies_mother_context() {
        let (db, dir) = open_test_db().await;
        db.insert_task_with_tls_policy(
            "mother",
            "https://pan.example.com/s/x",
            "share.bin",
            "/tmp",
            2,
            0,
            "",
            "",
            "",
            true,
            0,
        )
        .await
        .expect("insert mother");
        db.set_task_request_context("mother", "k=v", "https://ref.example", "{\"X-Test\":\"1\"}")
            .await
            .expect("set request context");
        db.set_task_resolver("mother", "test@plugin")
            .await
            .expect("set resolver");

        let spec = FissionSpec {
            group_id: "g1".to_string(),
            group_name: "我的相册".to_string(),
            group_save_dir: "/tmp/我的相册".to_string(),
            group_source_url: "https://pan.example.com/s/x".to_string(),
            mother_task_id: "mother".to_string(),
            mother_resolver_item: "f1".to_string(),
            mother_file_name: "a.mp4".to_string(),
            mother_save_dir: "/tmp/我的相册/a".to_string(),
            mother_total_bytes: 1000,
            mother_status: 0,
            siblings: vec![GroupSiblingSpec {
                id: "sib1".to_string(),
                file_name: "b.mp4".to_string(),
                save_dir: "/tmp/我的相册/b".to_string(),
                resolver_item: "f2".to_string(),
                total_bytes: 2000,
                status: 0,
            }],
        };
        db.fission_into_group(&spec).await.expect("fission");

        let groups = db.load_all_groups().await.expect("load groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, "g1");

        let mother = db
            .load_task_by_id("mother")
            .await
            .expect("load mother")
            .expect("exists");
        assert_eq!(mother.group_id, "g1");
        assert_eq!(mother.file_name, "a.mp4");
        assert_eq!(mother.save_dir, "/tmp/我的相册/a");
        assert_eq!(mother.total_bytes, 1000);
        assert_eq!(mother.checksum, "");
        assert_eq!(
            db.get_task_resolver_item("mother")
                .await
                .expect("mother item"),
            "f1"
        );

        let sibling = db
            .load_task_by_id("sib1")
            .await
            .expect("load sibling")
            .expect("exists");
        assert_eq!(sibling.group_id, "g1");
        assert_eq!(sibling.file_name, "b.mp4");
        assert_eq!(sibling.total_bytes, 2000);
        assert_eq!(sibling.url, "https://pan.example.com/s/x");
        assert!(sibling.ignore_tls_errors);
        assert_eq!(sibling.segments, 2);
        assert_eq!(
            db.get_task_resolver_item("sib1").await.expect("sib item"),
            "f2"
        );
        assert_eq!(
            db.get_task_resolver("sib1").await.expect("sib resolver"),
            "test@plugin"
        );
        let (cookies, referrer, headers) = db
            .load_task_request_context("sib1")
            .await
            .expect("sib ctx")
            .expect("exists");
        assert_eq!(cookies, "k=v");
        assert_eq!(referrer, "https://ref.example");
        assert_eq!(headers, "{\"X-Test\":\"1\"}");

        let ids = db.group_member_ids("g1").await.expect("member ids");
        assert_eq!(ids, vec!["mother".to_string(), "sib1".to_string()]);

        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn fission_into_group_rolls_back_on_missing_mother() {
        let (db, dir) = open_test_db().await;
        let spec = FissionSpec {
            group_id: "g1".to_string(),
            group_name: "组".to_string(),
            group_save_dir: "/tmp/g1".to_string(),
            mother_task_id: "does-not-exist".to_string(),
            ..Default::default()
        };
        assert!(db.fission_into_group(&spec).await.is_err());
        assert!(db.load_group_by_id("g1").await.expect("load").is_none());
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn task_tls_policy_is_strict_by_default_and_persists_explicit_opt_in() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "strict").await;
        db.insert_task_with_tls_policy(
            "insecure",
            "https://self-signed.example/file.bin",
            "file.bin",
            "/tmp",
            1,
            0,
            "",
            "",
            "",
            true,
            0,
        )
        .await
        .expect("insert task with explicit TLS opt-in");

        let strict = db
            .load_task_by_id("strict")
            .await
            .expect("load strict task")
            .expect("strict task exists");
        let insecure = db
            .load_task_by_id("insecure")
            .await
            .expect("load insecure task")
            .expect("insecure task exists");
        assert!(!strict.ignore_tls_errors);
        assert!(insecure.ignore_tls_errors);
        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // Correctness: delete_task removes all three tables
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_task_removes_from_tasks_table() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "t1").await;

        db.delete_task("t1").await.expect("delete task");

        let result = db.load_task_by_id("t1").await.expect("load after delete");
        assert!(result.is_none(), "task must be absent after delete");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn delete_task_not_present_in_load_all() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "keep").await;
        insert_task(&db, "delete-me").await;

        db.delete_task("delete-me").await.expect("delete task");

        let all = db.load_all_tasks().await.expect("load all");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].task_id, "keep");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn delete_nonexistent_task_succeeds() {
        let (db, dir) = open_test_db().await;
        // Deleting an ID that was never inserted must not return an error.
        let result = db.delete_task("phantom-id").await;
        assert!(result.is_ok(), "delete of missing task must succeed");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn delete_same_task_twice_is_idempotent() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "t1").await;

        db.delete_task("t1").await.expect("first delete");
        let result = db.delete_task("t1").await;
        assert!(
            result.is_ok(),
            "second delete of already-deleted task must succeed"
        );
        close_test_db(&db, dir).await;
    }

    // 删除任务只解除回链，不应让曾经下载的条目重新进入自动派发。
    #[tokio::test]
    async fn delete_task_keeps_linked_rss_item_read_without_task() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "t1").await;
        let source = crate::rss::model::RssSourceInfo {
            source_id: "s1".to_string(),
            ..Default::default()
        };
        db.insert_rss_source(&source).await.expect("insert source");
        let item = crate::rss::model::RssItemInfo {
            source_id: "s1".to_string(),
            guid: "g1".to_string(),
            title: "ep1".to_string(),
            status: crate::rss::model::RssItemStatus::Downloaded,
            task_id: "t1".to_string(),
            ..Default::default()
        };
        db.insert_rss_items(&[item]).await.expect("insert item");

        db.delete_task("t1").await.expect("delete task");

        let loaded = db
            .rss_item("s1", "g1")
            .await
            .expect("load item")
            .expect("item must still exist");
        assert_eq!(
            loaded.status,
            crate::rss::model::RssItemStatus::Ignored,
            "deleted download must remain read rather than auto-dispatched again"
        );
        assert!(
            loaded.task_id.is_empty(),
            "stale task_id back-link must be cleared"
        );
        close_test_db(&db, dir).await;
    }

    // 批量删除保持同样的已读语义。
    #[tokio::test]
    async fn delete_tasks_batch_keeps_linked_rss_items_read_without_tasks() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "t1").await;
        insert_task(&db, "t2").await;
        let source = crate::rss::model::RssSourceInfo {
            source_id: "s1".to_string(),
            ..Default::default()
        };
        db.insert_rss_source(&source).await.expect("insert source");
        for source_id in ["s2", "unaffected"] {
            db.insert_rss_source(&crate::rss::model::RssSourceInfo {
                source_id: source_id.to_string(),
                ..Default::default()
            })
            .await
            .expect("insert source");
        }
        db.insert_rss_items(&[
            crate::rss::model::RssItemInfo {
                source_id: "s1".to_string(),
                guid: "g1".to_string(),
                status: crate::rss::model::RssItemStatus::Downloaded,
                task_id: "t1".to_string(),
                ..Default::default()
            },
            crate::rss::model::RssItemInfo {
                source_id: "s1".to_string(),
                guid: "g2".to_string(),
                status: crate::rss::model::RssItemStatus::Downloaded,
                task_id: "t2".to_string(),
                ..Default::default()
            },
            crate::rss::model::RssItemInfo {
                source_id: "s1".to_string(),
                guid: "ignored".to_string(),
                status: crate::rss::model::RssItemStatus::Ignored,
                task_id: "t1".to_string(),
                ..Default::default()
            },
            crate::rss::model::RssItemInfo {
                source_id: "s2".to_string(),
                guid: "filtered".to_string(),
                status: crate::rss::model::RssItemStatus::Filtered,
                reason: "excluded".to_string(),
                task_id: "t2".to_string(),
                ..Default::default()
            },
            crate::rss::model::RssItemInfo {
                source_id: "unaffected".to_string(),
                guid: "untouched".to_string(),
                status: crate::rss::model::RssItemStatus::Downloaded,
                task_id: "other-task".to_string(),
                ..Default::default()
            },
        ])
        .await
        .expect("insert items");

        assert_eq!(
            db.delete_tasks_batch(&["t1".to_string(), "t2".to_string()])
                .await
                .expect("batch delete"),
            vec!["s1", "s2"]
        );

        for guid in ["g1", "g2"] {
            let loaded = db
                .rss_item("s1", guid)
                .await
                .expect("load item")
                .expect("item must still exist");
            assert_eq!(loaded.status, crate::rss::model::RssItemStatus::Ignored);
            assert!(loaded.task_id.is_empty());
        }
        let ignored = db
            .rss_item("s1", "ignored")
            .await
            .expect("load")
            .expect("item");
        assert_eq!(ignored.status, crate::rss::model::RssItemStatus::Ignored);
        assert!(ignored.task_id.is_empty());
        let filtered = db
            .rss_item("s2", "filtered")
            .await
            .expect("load")
            .expect("item");
        assert_eq!(filtered.status, crate::rss::model::RssItemStatus::Filtered);
        assert_eq!(filtered.reason, "excluded");
        assert!(filtered.task_id.is_empty());
        let untouched = db
            .rss_item("unaffected", "untouched")
            .await
            .expect("load")
            .expect("item");
        assert_eq!(
            untouched.status,
            crate::rss::model::RssItemStatus::Downloaded
        );
        assert_eq!(untouched.task_id, "other-task");
        close_test_db(&db, dir).await;
    }

    // 种子抓取失败进入指数退避：退避期内不参与自动派发，到期后回到派发队列并排在
    // 没失败过的条目之后；清零（成功/手动下载）后立即恢复。
    #[tokio::test]
    async fn rss_fetch_failure_backs_off_dispatch_until_due() {
        let (db, dir) = open_test_db().await;
        db.insert_rss_source(&crate::rss::model::RssSourceInfo {
            source_id: "s1".to_string(),
            ..Default::default()
        })
        .await
        .expect("insert source");
        let item = |guid: &str, pub_date: i64| crate::rss::model::RssItemInfo {
            source_id: "s1".to_string(),
            guid: guid.to_string(),
            pub_date,
            ..Default::default()
        };
        db.insert_rss_items(&[item("bad", 1), item("fresh", 2)])
            .await
            .expect("insert items");
        let ids = |items: Vec<crate::rss::model::RssItemInfo>| -> Vec<String> {
            items.into_iter().map(|i| i.guid).collect()
        };
        let now = 1_000_000;

        assert_eq!(
            ids(db.rss_dispatchable_items("s1", 10, now).await.expect("q")),
            vec!["bad", "fresh"]
        );

        let failures = db
            .record_rss_fetch_failure("s1", "bad", "torrent_fetch_failed", now, |n| 100 * n)
            .await
            .expect("record failure");
        assert_eq!(failures, 1);
        assert_eq!(
            ids(db
                .rss_dispatchable_items("s1", 10, now + 50)
                .await
                .expect("q")),
            vec!["fresh"],
            "item in backoff must not be dispatched"
        );
        assert_eq!(
            ids(db
                .rss_dispatchable_items("s1", 10, now + 100)
                .await
                .expect("q")),
            vec!["fresh", "bad"],
            "due item returns behind items that never failed"
        );

        let failures = db
            .record_rss_fetch_failure("s1", "bad", "torrent_fetch_failed", now, |n| 100 * n)
            .await
            .expect("record failure");
        assert_eq!(failures, 2);
        assert_eq!(
            ids(db
                .rss_dispatchable_items("s1", 10, now + 150)
                .await
                .expect("q")),
            vec!["fresh"],
            "second failure doubles the backoff"
        );

        db.clear_rss_item_backoff("s1", "bad").await.expect("clear");
        assert_eq!(
            ids(db.rss_dispatchable_items("s1", 10, now).await.expect("q")),
            vec!["fresh", "bad"]
        );
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn task_artifacts_roundtrip_and_cascade() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "a1").await;

        // 幂等登记：同名产物重复登记不报错、不重复。
        db.add_task_artifact("a1", "file.mp4").await.expect("add");
        db.add_task_artifact("a1", "file.mp4")
            .await
            .expect("add idempotent");
        db.add_task_artifact("a1", "file.srt").await.expect("add 2");

        let mut names = db.load_task_artifacts("a1").await.expect("load");
        names.sort();
        assert_eq!(names, vec!["file.mp4".to_string(), "file.srt".to_string()]);

        // 未登记任务读取为空。
        assert!(
            db.load_task_artifacts("phantom")
                .await
                .expect("load phantom")
                .is_empty()
        );

        // 删除任务后登记行随任务清理。
        db.delete_task("a1").await.expect("delete");
        assert!(
            db.load_task_artifacts("a1")
                .await
                .expect("load after delete")
                .is_empty(),
            "artifact rows must be removed with the task"
        );
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn task_artifacts_batch_delete_cleans_rows() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "b1").await;
        insert_task(&db, "b2").await;
        db.add_task_artifact("b1", "x.mp4").await.expect("add b1");
        db.add_task_artifact("b2", "y.mp4").await.expect("add b2");

        db.delete_tasks_batch(&["b1".to_string()])
            .await
            .expect("batch delete");

        assert!(
            db.load_task_artifacts("b1")
                .await
                .expect("load b1")
                .is_empty()
        );
        assert_eq!(
            db.load_task_artifacts("b2").await.expect("load b2"),
            vec!["y.mp4".to_string()],
            "unrelated task's artifacts must survive"
        );
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn delete_task_does_not_affect_other_tasks() {
        let (db, dir) = open_test_db().await;
        for i in 0..5 {
            insert_task(&db, &format!("task-{i}")).await;
        }

        db.delete_task("task-2").await.expect("delete task-2");

        let all = db.load_all_tasks().await.expect("load all");
        assert_eq!(all.len(), 4, "four tasks must remain after one delete");
        assert!(
            all.iter().all(|t| t.task_id != "task-2"),
            "deleted task must not appear in load_all"
        );
        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // Correctness: foreign-key cascade (task_segments / torrent_files)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_task_cascades_to_segments() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "seg-task").await;

        // Insert a segment row directly via the pool.
        sqlx::query(
            "INSERT INTO task_segments (task_id, segment_index, start_byte, end_byte)
             VALUES ($1, 0, 0, 1024)",
        )
        .bind("seg-task")
        .execute(&db.pool)
        .await
        .expect("insert segment");

        db.delete_task("seg-task").await.expect("delete");

        // Verify no orphan segment rows.
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM task_segments WHERE task_id = 'seg-task'")
                .fetch_one(&db.pool)
                .await
                .expect("query count");

        assert_eq!(count, 0, "task_segments must be empty after task delete");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn persist_merge_extends_parent_and_deletes_absorbed() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "merge-task").await;
        // 布局：父段 #0 [0,999]，被吸收段 #1/#2，无关段 #3（必须原样保留）。
        db.insert_segments(
            "merge-task",
            &[
                (0, 0, 999),
                (1, 1000, 1999),
                (2, 2000, 2999),
                (3, 3000, 3999),
            ],
        )
        .await
        .expect("insert segments");

        db.persist_merge("merge-task", 0, 2999, &[1, 2])
            .await
            .expect("persist merge");

        let segs = db.load_segments("merge-task").await.expect("load");
        assert_eq!(segs.len(), 2, "absorbed rows must be deleted");
        assert_eq!(segs[0].index, 0);
        assert_eq!(segs[0].end_byte, 2999, "parent end_byte must extend");
        assert_eq!(segs[1].index, 3, "unrelated segment must survive");
        assert_eq!(segs[1].end_byte, 3999);
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn task_request_context_roundtrip() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "ctx-task").await;

        // 新任务：三列由 DEFAULT '' 兜底，读取为空上下文。
        let empty = db
            .load_task_request_context("ctx-task")
            .await
            .expect("load empty");
        assert_eq!(empty, Some((String::new(), String::new(), String::new())));

        db.set_task_request_context(
            "ctx-task",
            "fnos-token=abc; session=xyz",
            "http://nas.example.com/",
            r#"{"Authorization":"Bearer t"}"#,
        )
        .await
        .expect("set context");

        let ctx = db
            .load_task_request_context("ctx-task")
            .await
            .expect("load context");
        assert_eq!(
            ctx,
            Some((
                "fnos-token=abc; session=xyz".to_string(),
                "http://nas.example.com/".to_string(),
                r#"{"Authorization":"Bearer t"}"#.to_string(),
            ))
        );

        // 不存在的任务 → None（区别于空上下文）。
        let missing = db
            .load_task_request_context("phantom")
            .await
            .expect("load missing");
        assert_eq!(missing, None);
        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // Performance benchmark: expose the N×WAL-checkpoint bottleneck
    //
    // Run with:  cargo test -p fluxdown_engine -- --nocapture delete_benchmark
    // -----------------------------------------------------------------------

    /// Insert N completed tasks (no active handles) and delete them one by one.
    /// Prints elapsed time so the per-delete overhead stays visible.
    #[tokio::test]
    async fn delete_benchmark_sequential_500_tasks() {
        const N: usize = 500;
        let (db, dir) = open_test_db().await;

        for i in 0..N {
            insert_task(&db, &format!("bench-{i}")).await;
        }

        let start = std::time::Instant::now();
        for i in 0..N {
            db.delete_task(&format!("bench-{i}")).await.expect("delete");
        }
        let elapsed = start.elapsed();

        // Verify all deleted.
        let remaining = db.load_all_tasks().await.expect("load all");
        assert!(remaining.is_empty(), "all tasks must be gone");

        eprintln!(
            "\n[benchmark] sequential delete of {N} tasks: {elapsed:?} \
             ({:.1} ms/task)",
            elapsed.as_secs_f64() * 1000.0 / N as f64
        );

        // Soft performance assertion: each delete should take < 50 ms on average.
        // This detects catastrophic regression (e.g. 5 s per task) but is
        // intentionally generous to avoid CI flakiness on slow machines.
        let ms_per_task = elapsed.as_secs_f64() * 1000.0 / N as f64;
        assert!(
            ms_per_task < 50.0,
            "average delete latency {ms_per_task:.1} ms exceeds 50 ms — \
             check for WAL-checkpoint or transaction overhead"
        );

        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // WAL checkpoint
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn wal_checkpoint_succeeds_on_empty_db() {
        let (db, dir) = open_test_db().await;
        let result = db.wal_checkpoint().await;
        assert!(result.is_ok(), "wal_checkpoint must succeed on empty DB");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn wal_checkpoint_succeeds_after_writes() {
        let (db, dir) = open_test_db().await;
        for i in 0..10 {
            insert_task(&db, &format!("cp-{i}")).await;
        }
        let result = db.wal_checkpoint().await;
        assert!(result.is_ok(), "wal_checkpoint must succeed after writes");
        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // update_task_file_info_resume
    // -----------------------------------------------------------------------

    /// Helper: insert a task with a specific total_bytes value.
    async fn insert_task_with_size(db: &Db, id: &str, total_bytes: i64) {
        db.insert_task(
            id,
            "http://example.com/file.bin",
            "file.bin",
            "/tmp",
            1,
            total_bytes,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert task with size");
    }

    /// CDN drift within 1 % tolerance must NOT update total_bytes.
    #[tokio::test]
    async fn resume_file_info_cdn_drift_within_tolerance_preserves_total_bytes() {
        let (db, dir) = open_test_db().await;
        let stored: i64 = 100_000_000; // 100 MB
        insert_task_with_size(&db, "r1", stored).await;

        // Probe returns stored + 500 KB — well within 1 % (= 1 MB).
        let probed = stored + 512_000;
        let (effective, updated) = db
            .update_task_file_info_resume("r1", "file.bin", probed)
            .await
            .expect("resume update");

        assert!(!updated, "updated flag must be false for CDN drift");
        assert_eq!(
            effective, stored,
            "effective total_bytes must equal stored value, not probed"
        );

        // DB must still hold the original value.
        let task = db
            .load_task_by_id("r1")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(
            task.total_bytes, stored,
            "DB total_bytes must be unchanged after CDN drift"
        );

        close_test_db(&db, dir).await;
    }

    /// A delta exceeding 1 % must update total_bytes (genuine file change).
    #[tokio::test]
    async fn resume_file_info_genuine_size_change_updates_total_bytes() {
        let (db, dir) = open_test_db().await;
        let stored: i64 = 100_000_000; // 100 MB
        insert_task_with_size(&db, "r2", stored).await;

        // Probe returns stored + 5 MB — exceeds 1 % (= 1 MB).
        let probed = stored + 5_000_000;
        let (effective, updated) = db
            .update_task_file_info_resume("r2", "file.bin", probed)
            .await
            .expect("resume update");

        assert!(updated, "updated flag must be true for genuine size change");
        assert_eq!(
            effective, probed,
            "effective total_bytes must equal probed value after genuine change"
        );

        let task = db
            .load_task_by_id("r2")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(
            task.total_bytes, probed,
            "DB total_bytes must be updated after genuine file size change"
        );

        close_test_db(&db, dir).await;
    }

    /// When stored total_bytes is 0 (first probe), always update.
    #[tokio::test]
    async fn resume_file_info_zero_stored_always_updates() {
        let (db, dir) = open_test_db().await;
        insert_task_with_size(&db, "r3", 0).await;

        let probed: i64 = 50_000_000;
        let (effective, updated) = db
            .update_task_file_info_resume("r3", "file.bin", probed)
            .await
            .expect("resume update");

        assert!(updated, "must update when stored total_bytes is 0");
        assert_eq!(effective, probed);

        let task = db
            .load_task_by_id("r3")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(task.total_bytes, probed);

        close_test_db(&db, dir).await;
    }

    /// Even when total_bytes is preserved, file_name must always be updated.
    #[tokio::test]
    async fn resume_file_info_always_updates_file_name() {
        let (db, dir) = open_test_db().await;
        let stored: i64 = 100_000_000;
        insert_task_with_size(&db, "r4", stored).await;

        // Probe returns same size — no total_bytes update.
        let (_, updated) = db
            .update_task_file_info_resume("r4", "renamed_file.bin", stored)
            .await
            .expect("resume update");

        assert!(
            !updated,
            "total_bytes update flag must be false for same size"
        );

        let task = db
            .load_task_by_id("r4")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(
            task.file_name, "renamed_file.bin",
            "file_name must be updated even when total_bytes is preserved"
        );
        assert_eq!(
            task.total_bytes, stored,
            "total_bytes must remain unchanged"
        );

        close_test_db(&db, dir).await;
    }

    /// 做种 tick 批量落库：上传增量累加到各任务、做种时长整体写入，返回累计值。
    #[tokio::test]
    async fn apply_seeding_tick_accumulates_uploads_and_times_in_one_call() {
        let (db, dir) = open_test_db().await;
        insert_task_with_size(&db, "s1", 100).await;
        insert_task_with_size(&db, "s2", 100).await;
        db.add_task_uploaded_bytes("s1", 50)
            .await
            .expect("seed upload");

        let totals = db
            .apply_seeding_tick(
                &[("s1".to_string(), 25), ("ghost".to_string(), 9)],
                &[("s1".to_string(), 600), ("s2".to_string(), 30)],
            )
            .await
            .expect("tick");

        // 不存在的任务不产出累计值；已有任务在原累计上叠加。
        assert_eq!(totals, vec![("s1".to_string(), 75)]);
        let s1 = db.load_task_by_id("s1").await.expect("load").expect("s1");
        let s2 = db.load_task_by_id("s2").await.expect("load").expect("s2");
        assert_eq!((s1.uploaded_bytes, s1.seeding_time_secs), (75, 600));
        assert_eq!((s2.uploaded_bytes, s2.seeding_time_secs), (0, 30));

        close_test_db(&db, dir).await;
    }

    /// Exact byte-for-byte equality → no update, returns stored value.
    #[tokio::test]
    async fn resume_file_info_exact_match_no_update() {
        let (db, dir) = open_test_db().await;
        let stored: i64 = 42_000_000;
        insert_task_with_size(&db, "r5", stored).await;

        let (effective, updated) = db
            .update_task_file_info_resume("r5", "file.bin", stored)
            .await
            .expect("resume update");

        assert!(!updated);
        assert_eq!(effective, stored);

        close_test_db(&db, dir).await;
    }

    /// Probe returns a *smaller* value beyond tolerance — must update.
    #[tokio::test]
    async fn resume_file_info_server_reports_smaller_file_updates() {
        let (db, dir) = open_test_db().await;
        let stored: i64 = 100_000_000;
        insert_task_with_size(&db, "r6", stored).await;

        // Server now reports 80 MB — 20 % smaller, well beyond tolerance.
        let probed: i64 = 80_000_000;
        let (effective, updated) = db
            .update_task_file_info_resume("r6", "file.bin", probed)
            .await
            .expect("resume update");

        assert!(
            updated,
            "must update when server reports genuinely smaller file"
        );
        assert_eq!(effective, probed);

        let task = db
            .load_task_by_id("r6")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(task.total_bytes, probed);

        close_test_db(&db, dir).await;
    }

    /// Tolerance cap: for a 10 GB file the threshold is capped at 1 MiB,
    /// so a 2 MiB drift must be treated as a genuine change.
    #[tokio::test]
    async fn resume_file_info_threshold_capped_at_1mib_for_large_files() {
        let (db, dir) = open_test_db().await;
        let stored: i64 = 10 * 1024 * 1024 * 1024; // 10 GiB
        insert_task_with_size(&db, "r7", stored).await;

        // 1 % of 10 GiB = 100 MiB, but threshold is capped at 1 MiB.
        // A 2 MiB drift must trigger an update.
        let probed = stored + 2 * 1024 * 1024;
        let (effective, updated) = db
            .update_task_file_info_resume("r7", "file.bin", probed)
            .await
            .expect("resume update");

        assert!(
            updated,
            "2 MiB drift on 10 GiB file must exceed the 1 MiB cap and trigger update"
        );
        assert_eq!(effective, probed);

        close_test_db(&db, dir).await;
    }

    /// A drift of exactly 1 byte beyond the threshold floor must update.
    #[tokio::test]
    async fn resume_file_info_small_file_1byte_drift_updates() {
        let (db, dir) = open_test_db().await;
        // For a 100-byte file, threshold = max(1, min(1, 1_048_576)) = 1 byte.
        // A delta of 2 bytes must trigger an update.
        let stored: i64 = 100;
        insert_task_with_size(&db, "r8", stored).await;

        let probed = stored + 2;
        let (effective, updated) = db
            .update_task_file_info_resume("r8", "file.bin", probed)
            .await
            .expect("resume update");

        assert!(
            updated,
            "2-byte drift on 100-byte file must exceed 1-byte floor threshold"
        );
        assert_eq!(effective, probed);

        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // update_task_progress_monotonic (F009)
    // -----------------------------------------------------------------------

    /// 单调写入只前进：先写大值再写小值，DB 须保留大值（MAX 钳制）。
    /// 复现 F009 的核心场景——陈旧的 status=1 中途写入晚于完成写入落库时，
    /// 不得把已落库的 100% 覆盖回中途值。
    #[tokio::test]
    async fn progress_monotonic_does_not_regress() {
        let (db, dir) = open_test_db().await;
        insert_task_with_size(&db, "m1", 1000).await;

        // 完成写入：最终权威值。
        db.update_task_progress_monotonic("m1", 1000)
            .await
            .expect("monotonic write 1000");
        // 陈旧的中途写入晚到——必须被钳制为 no-op。
        db.update_task_progress_monotonic("m1", 300)
            .await
            .expect("monotonic write 300");

        let task = db
            .load_task_by_id("m1")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(
            task.downloaded_bytes, 1000,
            "陈旧的较小进度写入不得覆盖已落库的较大值"
        );

        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // update_task_status: completed_at 维护
    // -----------------------------------------------------------------------

    /// completed_at 生命周期契约：
    /// - status→3 首次写入时间戳；重复写 3 不覆盖首次记录（幂等竞态）；
    /// - 暂停/错误（2/4）保持不变；
    /// - 重新下载（0/1/5）清空，供重下后重新记录。
    #[tokio::test]
    async fn update_task_status_maintains_completed_at() {
        let (db, dir) = open_test_db().await;
        insert_task_with_size(&db, "c1", 1000).await;

        let load = |db: Db| async move {
            db.load_task_by_id("c1")
                .await
                .expect("load")
                .expect("task exists")
        };

        // 初始为空。
        assert_eq!(load(db.clone()).await.completed_at, "");

        // 下载中不记录。
        db.update_task_status("c1", 1, "").await.expect("status 1");
        assert_eq!(load(db.clone()).await.completed_at, "");

        // 完成 → 记录当前 Unix 秒。
        db.update_task_status("c1", 3, "").await.expect("status 3");
        let first = load(db.clone()).await.completed_at;
        assert!(
            first.parse::<u64>().is_ok_and(|v| v > 0),
            "完成时必须记录非空 Unix 秒时间戳，got {first:?}"
        );

        // 重复写 3（幂等竞态）不覆盖首次记录。
        db.update_task_status("c1", 3, "")
            .await
            .expect("status 3 again");
        assert_eq!(load(db.clone()).await.completed_at, first);

        // 错误态保持不变。
        db.update_task_status("c1", 4, "boom")
            .await
            .expect("status 4");
        assert_eq!(load(db.clone()).await.completed_at, first);

        // 重新下载 → 清空。
        db.update_task_status("c1", 1, "").await.expect("restart");
        assert_eq!(load(db.clone()).await.completed_at, "");

        close_test_db(&db, dir).await;
    }

    /// file_hash 生命周期契约：
    /// - 下载中由校验和校验写入的记录在完成（→3）后保留；暂停/错误（2/4）保持；
    /// - 重新下载（0/1/5，单任务与批量）清空；
    /// - 完成后的计算结果只在任务仍是同一次完成（status=3 且 completed_at 未变）时写回。
    #[tokio::test]
    async fn file_hash_survives_completion_and_clears_on_restart() {
        let (db, dir) = open_test_db().await;
        insert_task_with_size(&db, "h1", 1000).await;
        let load = |db: Db| async move {
            db.load_task_by_id("h1")
                .await
                .expect("load")
                .expect("task exists")
        };

        db.update_task_status("h1", 1, "")
            .await
            .expect("downloading");
        db.set_task_file_hash("h1", "md5=aa")
            .await
            .expect("verified");
        db.update_task_status("h1", 3, "").await.expect("completed");
        let done = load(db.clone()).await;
        assert_eq!(done.file_hash, "md5=aa");
        db.update_task_status("h1", 4, "boom").await.expect("error");
        assert_eq!(load(db.clone()).await.file_hash, "md5=aa");

        db.update_task_status("h1", 3, "")
            .await
            .expect("completed again");
        assert!(
            !db.commit_task_file_hash("h1", "stale-generation", "sha1=bb")
                .await
                .expect("commit stale"),
            "a different completion must not be overwritten"
        );
        assert!(
            db.commit_task_file_hash("h1", &done.completed_at, "sha1=bb")
                .await
                .expect("commit"),
        );
        assert_eq!(load(db.clone()).await.file_hash, "sha1=bb");

        db.update_task_status("h1", 1, "").await.expect("restart");
        assert_eq!(load(db.clone()).await.file_hash, "");
        assert!(
            !db.commit_task_file_hash("h1", &done.completed_at, "sha1=bb")
                .await
                .expect("commit after restart"),
            "results of a restarted task must be discarded"
        );

        db.update_task_status("h1", 3, "")
            .await
            .expect("completed third");
        db.set_task_file_hash("h1", "md5=cc").await.expect("set");
        db.update_tasks_status_batch(&["h1".to_owned()], 0)
            .await
            .expect("batch requeue");
        assert_eq!(load(db.clone()).await.file_hash, "");

        close_test_db(&db, dir).await;
    }

    /// update_tasks_status_batch：一条（分块）SQL 批量置状态——
    /// - 列出的任务全部更新、error_message 统一清空；
    /// - completed_at 维护规则与单任务 update_task_status 一致
    ///   （→2 保持、→0 清空）；
    /// - 未列出的任务不受影响。
    #[tokio::test]
    async fn update_tasks_status_batch_updates_all_listed_tasks() {
        let (db, dir) = open_test_db().await;
        insert_task_with_size(&db, "b1", 100).await;
        insert_task_with_size(&db, "b2", 100).await;
        insert_task_with_size(&db, "b3", 100).await;
        db.update_task_status("b1", 4, "boom")
            .await
            .expect("seed b1 error");
        db.update_task_status("b2", 3, "")
            .await
            .expect("seed b2 completed");
        let b3_before = db
            .load_task_by_id("b3")
            .await
            .expect("load")
            .expect("b3 exists");

        // b1(error) + b2(completed) → paused；b3 未列出必须不受影响。
        let ids = vec!["b1".to_string(), "b2".to_string()];
        db.update_tasks_status_batch(&ids, 2)
            .await
            .expect("batch pause");

        let b1 = db.load_task_by_id("b1").await.expect("load").expect("b1");
        assert_eq!(b1.status, 2);
        assert_eq!(b1.error_message, "", "批量置状态必须清空 error_message");
        let b2 = db.load_task_by_id("b2").await.expect("load").expect("b2");
        assert_eq!(b2.status, 2);
        assert_ne!(b2.completed_at, "", "→2 不得清空 completed_at");
        let b3 = db.load_task_by_id("b3").await.expect("load").expect("b3");
        assert_eq!(b3.status, b3_before.status, "未列出的任务不得被改动");

        // →0（批量恢复排队）清空 completed_at，与单任务规则一致。
        db.update_tasks_status_batch(&ids, 0)
            .await
            .expect("batch pending");
        let b2 = db.load_task_by_id("b2").await.expect("load").expect("b2");
        assert_eq!(b2.status, 0);
        assert_eq!(b2.completed_at, "", "→0 必须清空 completed_at");

        close_test_db(&db, dir).await;
    }

    /// 单调写入对更大的值仍正常前进。
    #[tokio::test]
    async fn progress_monotonic_advances_forward() {
        let (db, dir) = open_test_db().await;
        insert_task_with_size(&db, "m2", 1000).await;

        db.update_task_progress_monotonic("m2", 200)
            .await
            .expect("monotonic write 200");
        db.update_task_progress_monotonic("m2", 800)
            .await
            .expect("monotonic write 800");

        let task = db
            .load_task_by_id("m2")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(task.downloaded_bytes, 800, "更大的进度值必须正常写入");

        close_test_db(&db, dir).await;
    }

    /// 非单调的 `update_task_progress` 必须仍能复位到 0（验证两方法语义不同：
    /// downloader/ftp 的从头重下依赖此行为，不可被 MAX 语义破坏）。
    #[tokio::test]
    async fn plain_progress_can_reset_to_zero() {
        let (db, dir) = open_test_db().await;
        insert_task_with_size(&db, "m3", 1000).await;

        db.update_task_progress_monotonic("m3", 900)
            .await
            .expect("monotonic write 900");
        // 普通写入复位到 0（切多段→单流重下场景）。
        db.update_task_progress("m3", 0)
            .await
            .expect("plain reset to 0");

        let task = db
            .load_task_by_id("m3")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(
            task.downloaded_bytes, 0,
            "update_task_progress 必须能把进度复位到 0（不被 MAX 钳制）"
        );

        close_test_db(&db, dir).await;
    }

    /// 暂停时分段已落库而任务级进度仍是旧周期值：回写后必须等于分段之和，
    /// 且不会把更大的已有值拉低（暂停帧不能显示 0% / 倒退）。
    #[tokio::test]
    async fn task_progress_syncs_from_segments_monotonically() {
        let (db, dir) = open_test_db().await;
        insert_task_with_size(&db, "seg-sync", 2000).await;
        db.insert_segments("seg-sync", &[(0, 0, 999), (1, 1000, 1999)])
            .await
            .expect("insert segments");
        db.set_segments_epoch("seg-sync", 1)
            .await
            .expect("set epoch");
        db.update_segment_progress_bounded("seg-sync", 0, 600, 0, 1)
            .await
            .expect("segment 0");
        db.update_segment_progress_bounded("seg-sync", 1, 300, 1000, 1)
            .await
            .expect("segment 1");

        // 已被新 spawn 夺权的旧下载器：0 行生效。
        db.sync_task_progress_from_segments("seg-sync", 0)
            .await
            .expect("stale sync");
        let task = db
            .load_task_by_id("seg-sync")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(task.downloaded_bytes, 0, "旧世代回写必须被 epoch 拦截");

        db.sync_task_progress_from_segments("seg-sync", 1)
            .await
            .expect("sync");
        let task = db
            .load_task_by_id("seg-sync")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(task.downloaded_bytes, 900, "任务进度必须对齐分段之和");

        db.update_task_progress("seg-sync", 1500)
            .await
            .expect("newer progress");
        db.sync_task_progress_from_segments("seg-sync", 1)
            .await
            .expect("sync again");
        let task = db
            .load_task_by_id("seg-sync")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(task.downloaded_bytes, 1500, "回写不得拉低已有进度");

        // restart 的删段归零之后，迟到回写不得复活旧进度。
        db.delete_segments("seg-sync")
            .await
            .expect("delete segments");
        db.sync_task_progress_from_segments("seg-sync", 1)
            .await
            .expect("sync after delete");
        let task = db
            .load_task_by_id("seg-sync")
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(task.downloaded_bytes, 0, "删段后回写必须保持 0");

        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // ED2K blocks / hashset
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn ed2k_blocks_init_load_roundtrip() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "e1").await;
        db.init_ed2k_blocks("e1", 3).await.expect("init blocks");
        let blocks = db.load_ed2k_blocks("e1").await.expect("load blocks");
        assert_eq!(blocks.len(), 3);
        // (block_index, state, downloaded_bytes, retry_count) 全默认。
        assert_eq!(blocks[0], (0, 0, 0, 0));
        assert_eq!(blocks[2].0, 2);
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn ed2k_block_update_and_retry_bump() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "e2").await;
        db.init_ed2k_blocks("e2", 2).await.expect("init");
        // 标记 block 0 verified（state=3），不 bump。
        db.update_ed2k_block("e2", 0, 3, 100, false)
            .await
            .expect("update");
        // block 1 置 missing 并 bump retry 两次。
        db.update_ed2k_block("e2", 1, 0, 0, true)
            .await
            .expect("bump1");
        db.update_ed2k_block("e2", 1, 0, 0, true)
            .await
            .expect("bump2");
        let blocks = db.load_ed2k_blocks("e2").await.expect("load");
        assert_eq!(blocks[0], (0, 3, 100, 0), "verified, retry 未变");
        assert_eq!(blocks[1], (1, 0, 0, 2), "retry_count 自增两次");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn ed2k_batches_preserve_resume_state_across_reopen() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "e-batch").await;
        db.init_ed2k_blocks("e-batch", 300).await.expect("init");
        let updates: Vec<_> = (0..300)
            .map(|index| {
                if index == 299 {
                    (index, 0, 0, true)
                } else {
                    (index, 3, 1000 + index as i64, false)
                }
            })
            .collect();
        db.update_ed2k_blocks("e-batch", &updates)
            .await
            .expect("batch");
        db.init_ed2k_blocks("e-batch", 302)
            .await
            .expect("idempotent init");
        db.pool.close().await;
        let reopened = Db::open(&dir).await.expect("reopen");
        let rows = reopened.load_ed2k_blocks("e-batch").await.expect("load");
        assert_eq!(rows[0], (0, 3, 1000, 0));
        assert_eq!(rows[127], (127, 3, 1127, 0));
        assert_eq!(rows[128], (128, 3, 1128, 0));
        assert_eq!(rows[255], (255, 3, 1255, 0));
        assert_eq!(rows[256], (256, 3, 1256, 0));
        assert_eq!(rows[299], (299, 0, 0, 1));
        assert_eq!(rows[300], (300, 0, 0, 0));
        assert_eq!(rows[301], (301, 0, 0, 0));
        reopened.reset_ed2k_blocks("e-batch").await.expect("reset");
        let rows = reopened
            .load_ed2k_blocks("e-batch")
            .await
            .expect("load reset");
        assert!(
            rows.iter()
                .all(|(_, state, bytes, _)| *state == 0 && *bytes == 0)
        );
        assert_eq!(rows[299].3, 1, "reset preserves retry history");
        close_test_db(&reopened, dir).await;
    }

    #[tokio::test]
    async fn ed2k_batch_failure_rolls_back_earlier_chunks() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "e-rollback").await;
        db.init_ed2k_blocks("e-rollback", 129).await.expect("init");
        sqlx::query(
            "CREATE TRIGGER reject_ed2k_update BEFORE UPDATE ON ed2k_blocks
            WHEN NEW.block_index = 128 BEGIN SELECT RAISE(FAIL, 'reject update'); END",
        )
        .execute(&db.pool)
        .await
        .expect("trigger");
        let updates: Vec<_> = (0..129).map(|index| (index, 3, 100, false)).collect();
        assert!(db.update_ed2k_blocks("e-rollback", &updates).await.is_err());
        let rows = db.load_ed2k_blocks("e-rollback").await.expect("load");
        assert_eq!(rows[0], (0, 0, 0, 0));
        assert_eq!(rows[127], (127, 0, 0, 0));
        assert_eq!(rows[128], (128, 0, 0, 0));
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn ed2k_hashset_blob_roundtrip() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "e3").await;
        assert!(db.load_ed2k_hashset("e3").await.expect("empty").is_none());
        // 2 个块哈希 = 32 字节（part_count 个，不含 phantom）。
        let blob: Vec<u8> = (0u8..32).collect();
        db.save_ed2k_hashset("e3", &blob).await.expect("save");
        let got = db
            .load_ed2k_hashset("e3")
            .await
            .expect("load")
            .expect("some");
        assert_eq!(got, blob);
        assert_eq!(got.len(), 32, "存 part_count 个块哈希，不含 phantom 追加");
        close_test_db(&db, dir).await;
    }

    #[cfg(feature = "ed2k")]
    #[tokio::test]
    async fn ed2k_server_list_default_parseable() {
        let (db, dir) = open_test_db().await;
        db.init_default_config("/tmp").await.expect("init config");
        let list = db
            .get_config("ed2k_server_list")
            .await
            .expect("get config")
            .expect("default present");
        // 与 server.rs 的解析函数同规则：逗号分隔、每项 host:port。
        let servers: Vec<&str> = list.split(',').filter(|s| !s.is_empty()).collect();
        assert!(!servers.is_empty(), "默认列表非空");
        for s in servers {
            assert!(s.contains(':'), "每项须 host:port: {s}");
            let port = s.rsplit(':').next().expect("has port");
            assert!(port.parse::<u16>().is_ok(), "端口须合法 u16: {s}");
        }
        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // sqlx 双后端专项
    // -----------------------------------------------------------------------

    /// `sqlite::memory:` 路径（服务器测试常用）——须建库成功且读写一致。
    #[tokio::test]
    async fn connect_in_memory_sqlite_works() {
        let db = Db::connect("sqlite::memory:").await.expect("connect mem");
        insert_task(&db, "mem1").await;
        let task = db
            .load_task_by_id("mem1")
            .await
            .expect("load")
            .expect("present");
        assert_eq!(task.task_id, "mem1");
    }

    /// 不支持的 URL scheme 必须返回 UnsupportedUrl 而非 panic/挂起。
    #[tokio::test]
    async fn connect_unsupported_scheme_rejected() {
        let err = Db::connect("mysql://root@localhost/db").await;
        assert!(matches!(err, Err(DbError::UnsupportedUrl(_))));
    }

    /// 重复 open 同一目录（模拟 App 重启）：迁移幂等、数据保留。
    #[tokio::test]
    async fn reopen_same_dir_is_idempotent() {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fluxdown_reopen_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        {
            let db = Db::open(&dir).await.expect("first open");
            insert_task(&db, "persist-1").await;
            db.pool.close().await;
        }
        {
            let db = Db::open(&dir).await.expect("second open");
            let task = db
                .load_task_by_id("persist-1")
                .await
                .expect("load")
                .expect("survives reopen");
            assert_eq!(task.task_id, "persist-1");
            close_test_db(&db, dir).await;
        }
    }

    /// PostgreSQL 冒烟（需要本地 pg 实例）：
    /// `PG_TEST_URL=postgres://postgres:pw@localhost/postgres \
    ///  cargo test -p fluxdown_engine -- --ignored pg_smoke`
    #[tokio::test]
    #[ignore = "requires a running PostgreSQL instance (set PG_TEST_URL)"]
    async fn pg_smoke_roundtrip() {
        let url = std::env::var("PG_TEST_URL")
            .unwrap_or_else(|_| "postgres://postgres:pw@localhost/postgres".to_owned());
        let db = Db::connect(&url).await.expect("connect pg");
        let id = format!("pg-smoke-{}", std::process::id());
        // 清理上次残留（幂等）。
        db.delete_task(&id).await.expect("pre-clean");

        db.insert_task(
            &id,
            "http://example.com/big.bin",
            "big.bin",
            "/tmp",
            8,
            5_000_000_000, // >2GB，验证 BIGINT 列不截断
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert");
        db.update_task_progress(&id, 3_000_000_000)
            .await
            .expect("progress");
        db.update_task_progress_monotonic(&id, 2_000_000_000)
            .await
            .expect("monotonic no-regress");

        let task = db
            .load_task_by_id(&id)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(task.total_bytes, 5_000_000_000);
        assert_eq!(task.downloaded_bytes, 3_000_000_000, "GREATEST 钳制生效");

        // 分段 + 配置 upsert。
        db.insert_segments(
            &id,
            &[(0, 0, 2_499_999_999), (1, 2_500_000_000, 4_999_999_999)],
        )
        .await
        .expect("segments");
        let segs = db.load_segments(&id).await.expect("load segs");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1].end_byte, 4_999_999_999);
        db.set_config("pg_smoke_key", "v1").await.expect("set");
        db.set_config("pg_smoke_key", "v2").await.expect("upsert");
        assert_eq!(
            db.get_config("pg_smoke_key").await.expect("get").as_deref(),
            Some("v2")
        );
        let entry = db
            .append_task_activity(&activity(
                &id,
                "retry after HTTP 503",
                crate::task_activity::timestamp_ms(),
            ))
            .await
            .expect("pg append activity")
            .expect("pg task exists");
        let page = db
            .query_task_activity(&crate::task_activity::TaskActivityQuery {
                task_id: id.clone(),
                ..Default::default()
            })
            .await
            .expect("pg page");
        assert_eq!(page.entries, vec![entry]);
        assert!(!page.truncated);

        db.delete_task(&id).await.expect("clean");
        assert!(
            db.query_task_activity(&crate::task_activity::TaskActivityQuery {
                task_id: id.clone(),
                ..Default::default()
            })
            .await
            .expect("pg activity cascade")
            .entries
            .is_empty()
        );
        db.delete_config("pg_smoke_key").await.expect("clean cfg");
    }

    /// pg 版 bt_guard 往返：BIGINT（> 4 GiB）/ BYTEA / NULL 过期 / ON CONFLICT upsert / 事件环。
    /// 用时间戳派生的唯一 id / infohash / 规则名，对共享库无副作用（事件行除外，受环形上限约束）。
    #[tokio::test]
    #[ignore = "requires a running PostgreSQL instance (set PG_TEST_URL)"]
    async fn pg_smoke_bt_guard_roundtrip() {
        let url = std::env::var("PG_TEST_URL")
            .unwrap_or_else(|_| "postgres://postgres:pw@localhost/postgres".to_owned());
        let db = Db::connect(&url).await.expect("connect pg");
        let tag = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let id_base = 9_000_000_000_000_i64 + i64::try_from(tag % 1_000_000_000).unwrap_or(0) * 4;
        let hash = format!("{tag:040x}");
        let rule = format!("pg_smoke_{tag}");
        let row = |id: i64, expires: Option<i64>| BtGuardEntryRow {
            id,
            layer: "manual".to_owned(),
            cidr: "203.0.113.7/32".to_owned(),
            action: "ban".to_owned(),
            rule: rule.clone(),
            reason: format!("pg smoke {tag}"),
            evidence: "{}".to_owned(),
            source: "user".to_owned(),
            created_at: 1_800_000_000,
            expires_at: expires,
        };
        let bits: Vec<u8> = (0..=255u8).collect();
        let batch = BtGuardBatch {
            entry_ops: vec![
                BtGuardEntryOp::Upsert(row(id_base, None)),
                BtGuardEntryOp::Upsert(row(id_base + 1, Some(4_000_000_000))),
                BtGuardEntryOp::Upsert(row(id_base, Some(5_000_000_000))),
            ],
            strikes: vec![BtGuardStrikeRow {
                ip_group: format!("pg-smoke-{tag}"),
                strikes: 2,
                last_strike_at: 1_800_000_000,
                last_ban_until: 1_800_086_400,
            }],
            ledgers: vec![BtGuardLedgerRow {
                info_hash: hash.clone(),
                ip_group: "203.0.113.7".to_owned(),
                delivered_bytes: 5_000_000_000,
                dup_bytes: 4_999_999_999,
                delivered_bits: bits.clone(),
                claimed_bits: vec![0xff, 0x00],
                rerequests: 2,
                probed_at: 0,
                first_seen: 1_800_000_000,
                last_seen: 1_800_000_000,
                lag_cleared_at: 1_800_000_500,
                excess_baseline: 4_000_000_000,
            }],
            events: (0..3)
                .map(|i| BtGuardEventRow {
                    id: 0,
                    at: 1_800_000_000 + i,
                    kind: "enforced".to_owned(),
                    ip_group: "203.0.113.7".to_owned(),
                    info_hash: hash.clone(),
                    rule: rule.clone(),
                    mode: "enforce".to_owned(),
                    action: "ban".to_owned(),
                    detail: "{}".to_owned(),
                })
                .collect(),
            ..BtGuardBatch::default()
        };
        db.bt_guard_apply_batch(&batch).await.expect("apply");

        let (total, rows) = db
            .bt_guard_list_entries(&BtGuardEntryQuery {
                rule: Some(rule.clone()),
                limit: 10,
                ..BtGuardEntryQuery::default()
            })
            .await
            .expect("list entries");
        assert_eq!(total, 2, "upsert on the same id must not duplicate");
        assert_eq!(
            rows[1].expires_at,
            Some(5_000_000_000),
            "second upsert wins"
        );
        assert_eq!(rows[0].expires_at, Some(4_000_000_000));
        let none_expiry = db
            .bt_guard_apply_batch(&BtGuardBatch {
                entry_ops: vec![BtGuardEntryOp::Upsert(row(id_base + 2, None))],
                ..BtGuardBatch::default()
            })
            .await;
        none_expiry.expect("NULL expires_at");
        let (_, rows) = db
            .bt_guard_list_entries(&BtGuardEntryQuery {
                query: Some(format!("PG SMOKE {tag}")),
                limit: 10,
                ..BtGuardEntryQuery::default()
            })
            .await
            .expect("ilike query");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].expires_at, None);

        let ledger = db
            .bt_guard_load_ledger(1_800_000_000)
            .await
            .expect("ledger");
        let mine = ledger
            .iter()
            .find(|r| r.info_hash == hash)
            .expect("my ledger row");
        assert_eq!(mine.delivered_bytes, 5_000_000_000);
        assert_eq!(mine.delivered_bits, bits);
        assert_eq!(mine.lag_cleared_at, 1_800_000_500);
        assert_eq!(mine.excess_baseline, 4_000_000_000);
        let strikes = db.bt_guard_load_strikes().await.expect("strikes");
        assert!(
            strikes
                .iter()
                .any(|s| s.ip_group == format!("pg-smoke-{tag}") && s.strikes == 2)
        );
        let (events, _) = db
            .bt_guard_list_events(&BtGuardEventQuery {
                rule: Some(rule.clone()),
                limit: 10,
                ..BtGuardEventQuery::default()
            })
            .await
            .expect("events");
        assert_eq!(events, 3);

        db.bt_guard_apply_batch(&BtGuardBatch {
            entry_ops: (0..3)
                .map(|i| BtGuardEntryOp::Delete(id_base + i))
                .collect(),
            strike_deletes: vec![format!("pg-smoke-{tag}")],
            ledger_deletes: vec![(hash, "203.0.113.7".to_owned())],
            ..BtGuardBatch::default()
        })
        .await
        .expect("clean");
    }

    /// flush 失败（连接池已关）必须保留待办写入，换一个可用库重试后全部落盘。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_guard_flush_failure_requeues_pending_writes() {
        use crate::bt_guard::{BtGuard, BtGuardConfig, FilterAction, NewEntry};
        let (broken, broken_dir) = open_test_db().await;
        let guard = BtGuard::new(BtGuardConfig::default());
        guard
            .add_entry(NewEntry {
                cidr: "5.5.5.5".to_owned(),
                action: FilterAction::Ban,
                duration_secs: None,
                reason: "t".to_owned(),
            })
            .expect("add");
        guard.apply_filter_file("https://f.example/a", "7.7.7.0/24\n");
        assert!(guard.needs_flush());
        broken.pool.close().await;
        let waker = Arc::new(tokio::sync::Notify::new());
        guard.set_waker(Arc::clone(&waker));
        assert!(guard.flush(&broken).await.is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), waker.notified())
                .await
                .is_ok(),
            "a failed flush re-queues work and wakes the host"
        );
        assert!(
            guard.needs_flush(),
            "a failed flush must keep the pending writes"
        );

        let (healthy, healthy_dir) = open_test_db().await;
        guard.flush(&healthy).await.expect("retry");
        assert!(!guard.needs_flush());
        let rows = healthy.bt_guard_load_entries(0).await.expect("entries");
        assert_eq!(rows.len(), 2, "manual ban + file entry both landed");
        let (events, _) = healthy
            .bt_guard_list_events(&BtGuardEventQuery {
                limit: 10,
                ..BtGuardEventQuery::default()
            })
            .await
            .expect("events");
        assert_eq!(events, 2, "events queued before the failure are not lost");
        close_test_db(&broken, broken_dir).await;
        close_test_db(&healthy, healthy_dir).await;
    }

    /// 加载失败（账本表不可读）但库内最大条目 id 可查：新条目的 id 必须接在库内最大 id 之后，
    /// 不得从 1 重新分配而覆盖库内已有条目。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_guard_failed_load_still_allocates_ids_past_stored_entries() {
        use crate::bt_guard::{BtGuard, BtGuardConfig, FilterAction, NewEntry};
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let rows = (1..=5)
            .map(|id| BtGuardEntryRow {
                id,
                layer: "manual".to_owned(),
                cidr: format!("203.0.113.{id}/32"),
                action: "ban".to_owned(),
                rule: "manual".to_owned(),
                reason: format!("stored {id}"),
                evidence: String::new(),
                source: "user".to_owned(),
                created_at: 1_800_000_000,
                expires_at: None,
            })
            .map(BtGuardEntryOp::Upsert)
            .collect();
        db.bt_guard_apply_batch(&BtGuardBatch {
            entry_ops: rows,
            ..BtGuardBatch::default()
        })
        .await
        .expect("seed");
        sqlx::query("DROP TABLE bt_guard_ledger")
            .execute(&db.pool)
            .await
            .expect("break the ledger table");

        let guard = BtGuard::new(BtGuardConfig::default());
        assert!(
            guard.load(&db).await.is_err(),
            "load must report the failure"
        );
        let added = guard
            .add_entry(NewEntry {
                cidr: "198.51.100.9".to_owned(),
                action: FilterAction::Ban,
                duration_secs: None,
                reason: "new".to_owned(),
            })
            .expect("ids are still safe: the stored max id is known");
        assert!(
            added.id > 5,
            "new id {} must not collide with stored ids",
            added.id
        );
        sqlx::query(
            "CREATE TABLE bt_guard_ledger (info_hash TEXT NOT NULL, ip_group TEXT NOT NULL, \
             delivered_bytes BIGINT NOT NULL, dup_bytes BIGINT NOT NULL, \
             delivered_bits BLOB NOT NULL, claimed_bits BLOB NOT NULL, rerequests BIGINT NOT NULL, \
             probed_at BIGINT NOT NULL DEFAULT 0, first_seen BIGINT NOT NULL, \
             last_seen BIGINT NOT NULL, lag_cleared_at BIGINT NOT NULL DEFAULT 0, \
             excess_baseline BIGINT NOT NULL DEFAULT 0, PRIMARY KEY (info_hash, ip_group))",
        )
        .execute(&db.pool)
        .await
        .expect("repair the ledger table");
        guard.flush(&db).await.expect("flush");
        let stored = db.bt_guard_load_entries(0).await.expect("entries");
        assert_eq!(stored.len(), 6, "no stored entry was overwritten");
        assert!(stored.iter().any(|r| r.id == 3 && r.reason == "stored 3"));
    }

    /// 加载失败且连库内最大条目 id 也确认不了：禁止产生需落库的新条目，`add_entry` 返回错误，
    /// flush 不写条目表（不会覆盖库内行）。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_guard_failed_load_without_known_max_id_blocks_entry_writes() {
        use crate::bt_guard::{BtGuard, BtGuardConfig, FilterAction, GuardError, NewEntry};
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        sqlx::query("DROP TABLE bt_guard_entries")
            .execute(&db.pool)
            .await
            .expect("break the entries table");

        let guard = BtGuard::new(BtGuardConfig::default());
        assert!(guard.load(&db).await.is_err());
        let result = guard.add_entry(NewEntry {
            cidr: "198.51.100.9".to_owned(),
            action: FilterAction::Ban,
            duration_secs: None,
            reason: "new".to_owned(),
        });
        assert!(
            matches!(result, Err(GuardError::StoreUnavailable(_))),
            "{result:?}"
        );
        assert_eq!(guard.counts().manual, 0, "nothing was added in memory");

        // 自动封禁 / 订阅等内部路径产生的条目只留在内存：flush 丢弃条目写入、不覆盖库内行，
        // 其余内容（事件）照常落库。
        sqlx::query(
            "CREATE TABLE bt_guard_entries (id INTEGER PRIMARY KEY, layer TEXT NOT NULL, \
             cidr TEXT NOT NULL, action TEXT NOT NULL, rule TEXT NOT NULL, \
             reason TEXT NOT NULL, evidence TEXT NOT NULL DEFAULT '', source TEXT NOT NULL, \
             created_at BIGINT NOT NULL, expires_at BIGINT)",
        )
        .execute(&db.pool)
        .await
        .expect("repair");
        guard.apply_filter_file("https://f.example/a", "7.7.7.0/24\n");
        guard
            .flush(&db)
            .await
            .expect("entry writes are dropped, the rest of the batch still lands");
        assert!(!guard.needs_flush());
        assert!(
            db.bt_guard_load_entries(0)
                .await
                .expect("entries")
                .is_empty(),
            "no entry row was written while ids are unknown"
        );
        let (events, _) = db
            .bt_guard_list_events(&BtGuardEventQuery {
                limit: 10,
                ..BtGuardEventQuery::default()
            })
            .await
            .expect("events");
        assert_eq!(events, 1, "the filter_applied event still landed");

        // 成功的重新加载解除封锁。
        guard.load(&db).await.expect("reload");
        guard
            .add_entry(NewEntry {
                cidr: "198.51.100.9".to_owned(),
                action: FilterAction::Ban,
                duration_secs: None,
                reason: "new".to_owned(),
            })
            .expect("writes are allowed again");
    }

    /// 启动加载失败期间：内存里的账本 / strike 不是库内状态的延续，不得覆盖库内记录
    /// （事件照常落库）；重新加载成功后状态以库为准并解除门控。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_guard_failed_load_gates_ledger_and_strike_writes_until_reload() {
        use crate::bt_guard::{BtGuard, BtGuardConfig, GuardCtx, GuardEvent, IpGroup, RuleMode};
        const NOW: i64 = 1_800_000_000;
        let hash = [7u8; 20];
        let ctx = |ip: &str| GuardCtx {
            info_hash: hash,
            addr: std::net::SocketAddr::new(ip.parse().expect("ip"), 6881),
            private: false,
            total_length: 100 << 20,
            piece_length: 1 << 20,
            num_pieces: 100,
            we_are_seeding: true,
        };
        let request = |piece: u32| GuardEvent::Request {
            piece,
            begin: 0,
            length: 16384,
            peer_claims_piece: true,
        };
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.bt_guard_apply_batch(&BtGuardBatch {
            strikes: vec![BtGuardStrikeRow {
                ip_group: "1.2.3.4".to_owned(),
                strikes: 3,
                last_strike_at: NOW - 10,
                last_ban_until: NOW + 100,
            }],
            ledgers: vec![BtGuardLedgerRow {
                info_hash: hex::encode(hash),
                ip_group: "1.2.3.4".to_owned(),
                delivered_bytes: 5_000_000,
                dup_bytes: 0,
                delivered_bits: vec![0xff],
                claimed_bits: vec![0xff],
                rerequests: 0,
                probed_at: 0,
                first_seen: NOW - 100,
                last_seen: NOW - 10,
                lag_cleared_at: 0,
                excess_baseline: 0,
            }],
            ..BtGuardBatch::default()
        })
        .await
        .expect("seed");
        // load 的最后一个查询失败：此时内存状态尚未被替换。
        sqlx::query("DROP TABLE bt_guard_cloud_meta")
            .execute(&db.pool)
            .await
            .expect("break the meta table");

        let cfg = BtGuardConfig {
            rule_rerequest: RuleMode::Enforce,
            ..BtGuardConfig::default()
        };
        let guard = BtGuard::with_clock(cfg, Arc::new(|| NOW));
        assert!(guard.load(&db).await.is_err());
        assert!(guard.load_failed());
        // 加载失败期间同一个组再犯：内存里 strike=1、账本从零开始。
        let bad = ctx("1.2.3.4");
        guard.on_event_at(&bad, request(1), NOW);
        guard.on_event_at(&bad, request(2), NOW);
        assert!(guard.is_banned("1.2.3.4".parse().expect("ip")));
        sqlx::query(
            "CREATE TABLE bt_guard_cloud_meta (id INTEGER PRIMARY KEY, data TEXT NOT NULL)",
        )
        .execute(&db.pool)
        .await
        .expect("repair");
        guard.flush_all(&db).await.expect("flush");
        let strikes = db.bt_guard_load_strikes().await.expect("strikes");
        assert_eq!(strikes.len(), 1);
        assert_eq!(strikes[0].strikes, 3, "stored strike is not overwritten");
        let ledgers = db.bt_guard_load_ledger(0).await.expect("ledger");
        assert_eq!(ledgers.len(), 1);
        assert_eq!(
            ledgers[0].delivered_bytes, 5_000_000,
            "stored ledger is not overwritten"
        );
        let (events, _) = db
            .bt_guard_list_events(&BtGuardEventQuery {
                limit: 10,
                ..BtGuardEventQuery::default()
            })
            .await
            .expect("events");
        assert!(events >= 1, "events are still persisted");
        assert!(!guard.needs_flush_all(), "gated marks are discarded");

        // 重新加载成功：状态来自库，门控解除。
        guard.load(&db).await.expect("reload");
        assert!(!guard.load_failed());
        let group: IpGroup = "1.2.3.4".parse().expect("group");
        assert_eq!(guard.strikes_of(&group), 3);
        assert_eq!(guard.group_delivered_bytes(&hash, &group), Some(5_000_000));
        let other = ctx("5.6.7.8");
        guard.on_event_at(
            &other,
            GuardEvent::Uploaded {
                piece: 0,
                begin: 0,
                length: 16384,
            },
            NOW,
        );
        guard.flush_all(&db).await.expect("flush after reload");
        let ledgers = db.bt_guard_load_ledger(0).await.expect("ledger");
        assert!(
            ledgers.iter().any(|r| r.ip_group == "5.6.7.8"),
            "ledger writes resume after a successful load"
        );
    }

    /// 账本被 LRU 淘汰（排队删除）后又重建，期间一次落库失败被还原：最终库中必须有新账本。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_guard_rebuilt_ledger_survives_a_failed_flush_restore() {
        use crate::bt_guard::{BtGuard, BtGuardConfig, GuardCtx, GuardEvent};
        const NOW: i64 = 1_800_000_000;
        let ip = |i: u32| format!("23.0.{}.{}", i / 256, i % 256);
        let ctx = |i: u32| GuardCtx {
            info_hash: [9u8; 20],
            addr: std::net::SocketAddr::new(ip(i).parse().expect("ip"), 6881),
            private: false,
            total_length: 100_000 << 20,
            piece_length: 1 << 20,
            num_pieces: 100_000,
            we_are_seeding: true,
        };
        let upload = |length: u32| GuardEvent::Uploaded {
            piece: 0,
            begin: 0,
            length,
        };
        let healthy = Db::connect("sqlite::memory:").await.expect("healthy db");
        let broken = Db::connect("sqlite::memory:").await.expect("broken db");
        let guard = BtGuard::with_clock(BtGuardConfig::default(), Arc::new(|| NOW + 5000));
        // 332 组 = 该规模种子的账本上限；全部落库后干净。
        for i in 0..332u32 {
            guard.on_event_at(&ctx(i), upload(16384), NOW + i64::from(i));
        }
        guard.flush_all(&healthy).await.expect("flush");
        // 组 0 被淘汰（干净 → 排队删除），本次落库在坏库上失败并被还原。
        guard.on_event_at(&ctx(332), upload(16384), NOW + 1000);
        broken.pool.close().await;
        assert!(guard.flush_all(&broken).await.is_err());
        // 组 0 回来，账本从零重建；再向好库落库。
        guard.on_event_at(&ctx(0), upload(32768), NOW + 1001);
        guard.flush_all(&healthy).await.expect("retry");
        let rows = healthy.bt_guard_load_ledger(0).await.expect("ledger");
        let rebuilt = rows
            .iter()
            .find(|r| r.ip_group == ip(0))
            .expect("rebuilt ledger exists");
        assert_eq!(rebuilt.delivered_bytes, 32768);
    }

    // -----------------------------------------------------------------------
    // 文件跟踪（FluxDown #11）：update_task_file_missing / file_missing 读回一致性
    // -----------------------------------------------------------------------

    /// 对 completed(status=3) 任务落库 file_missing=true 必须成功（返回
    /// true）且能读回。
    #[tokio::test]
    async fn update_task_file_missing_marks_completed_task() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task(&db, "t1").await;
        db.update_task_status("t1", 3, "")
            .await
            .expect("mark completed");

        let changed = db
            .update_task_file_missing("t1", true)
            .await
            .expect("update file_missing");
        assert!(
            changed,
            "update on a completed task must report a changed row"
        );

        let task = db
            .load_task_by_id("t1")
            .await
            .expect("load")
            .expect("task present");
        assert!(
            task.file_missing,
            "file_missing must read back true after update"
        );
    }

    /// 对非 completed 任务（status=1，下载中）更新必须是空操作：WHERE 子句
    /// 的 `AND status = 3` 保护带竞态窗口的调用方，绝不误改活跃任务的标志。
    #[tokio::test]
    async fn update_task_file_missing_noop_for_non_completed_task() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task(&db, "t1").await;
        db.update_task_status("t1", 1, "")
            .await
            .expect("mark downloading");

        let changed = db
            .update_task_file_missing("t1", true)
            .await
            .expect("update attempt");
        assert!(!changed, "update must be a no-op for tasks not in status=3");

        let task = db
            .load_task_by_id("t1")
            .await
            .expect("load")
            .expect("task present");
        assert!(
            !task.file_missing,
            "file_missing must remain unchanged for a non-completed task"
        );
    }

    /// 批量版与单条版语义一致：completed 行写入并出现在返回值里，非 completed
    /// 行被 `AND status = 3` 挡下且不进返回值——调用方正是据此过滤下发的事件
    /// 与自动清理批次。
    #[tokio::test]
    async fn update_tasks_file_missing_applies_only_to_completed_rows() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task(&db, "done").await;
        insert_task(&db, "busy").await;
        db.update_task_status("done", 3, "")
            .await
            .expect("mark completed");
        db.update_task_status("busy", 1, "")
            .await
            .expect("mark downloading");

        let applied = db
            .update_tasks_file_missing(&[("done".to_string(), true), ("busy".to_string(), true)])
            .await
            .expect("batch update");
        assert_eq!(applied, vec![("done".to_string(), true)]);

        let done = db
            .load_task_by_id("done")
            .await
            .expect("load")
            .expect("task present");
        let busy = db
            .load_task_by_id("busy")
            .await
            .expect("load")
            .expect("task present");
        assert!(done.file_missing, "completed 行必须写入");
        assert!(!busy.file_missing, "非 completed 行必须原样不动");
    }

    /// 文件跟踪投影只取扫描真正要用的行：活跃（0/1/5，用于目标路径占用判定）
    /// 与已完成（3，待探测）。paused/error 不参与扫描，必须在 SQL 里就被滤掉。
    #[tokio::test]
    async fn load_file_tracking_rows_returns_active_and_completed_only() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        for (id, status) in [("done", 3), ("busy", 1), ("paused", 2), ("failed", 4)] {
            insert_task(&db, id).await;
            db.update_task_status(id, status, "")
                .await
                .expect("set status");
        }
        db.update_tasks_file_missing(&[("done".to_string(), true)])
            .await
            .expect("mark missing");

        let mut rows = db.load_file_tracking_rows().await.expect("load rows");
        rows.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        let ids: Vec<&str> = rows.iter().map(|r| r.task_id.as_str()).collect();
        assert_eq!(ids, vec!["busy", "done"]);

        let done = &rows[1];
        assert_eq!(done.status, 3);
        assert!(done.file_missing, "投影必须带上库中的 file_missing 现值");
        assert_eq!(done.save_dir, "/tmp");
        assert_eq!(done.file_name, "file.bin");
    }

    /// 不存在的任务 id：更新必须是空操作而不是报错。
    #[tokio::test]
    async fn update_task_file_missing_noop_for_unknown_task_id() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");

        let changed = db
            .update_task_file_missing("no-such-task", true)
            .await
            .expect("update attempt");
        assert!(!changed, "update on a nonexistent id must report no change");
    }

    /// `load_all_tasks` 与 `load_task_by_id` 共用 `task_from_row` 映射
    /// `file_missing` 列；两条读路径在更新前后必须始终一致，防止迁移新增列
    /// 的防御性映射（`unwrap_or_default`）在某一条路径上失配。
    #[tokio::test]
    async fn load_all_and_load_by_id_agree_on_file_missing_across_states() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task(&db, "t1").await;
        db.update_task_status("t1", 3, "")
            .await
            .expect("mark completed");

        let by_id = db
            .load_task_by_id("t1")
            .await
            .expect("load by id")
            .expect("task present");
        let all = db.load_all_tasks().await.expect("load all");
        let by_all = all
            .iter()
            .find(|t| t.task_id == "t1")
            .expect("task present in load_all");
        assert_eq!(
            by_id.file_missing, by_all.file_missing,
            "both load paths must agree before any scan has run"
        );

        db.update_task_file_missing("t1", true)
            .await
            .expect("mark missing");

        let by_id = db
            .load_task_by_id("t1")
            .await
            .expect("load by id")
            .expect("task present");
        let all = db.load_all_tasks().await.expect("load all");
        let by_all = all
            .iter()
            .find(|t| t.task_id == "t1")
            .expect("task present in load_all");
        assert!(
            by_id.file_missing,
            "load_task_by_id must reflect the update"
        );
        assert!(
            by_all.file_missing,
            "load_all_tasks must reflect the same update"
        );
    }

    /// 新插入的普通任务默认没有音频轨：`audio_url` 列默认空串，
    /// load_audio_url 必须归一化为 None，否则恢复逻辑会把单 URL 任务误当轨对任务处理。
    #[tokio::test]
    async fn load_audio_url_returns_none_for_plain_task_without_audio_track() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "plain1").await;

        let audio_url = db.load_audio_url("plain1").await.expect("load audio_url");
        assert_eq!(
            audio_url, None,
            "plain task must not be mistaken for a paired-track task"
        );

        close_test_db(&db, dir).await;
    }

    /// save_audio_url 写入非空 URL 后，load_audio_url 必须原样读回，
    /// 这是重启恢复重建轨对下载所依赖的往返一致性。
    #[tokio::test]
    async fn save_audio_url_then_load_returns_same_value() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "pair1").await;

        db.save_audio_url("pair1", "http://example.com/audio.m4a")
            .await
            .expect("save audio_url");
        let audio_url = db.load_audio_url("pair1").await.expect("load audio_url");
        assert_eq!(audio_url, Some("http://example.com/audio.m4a".to_string()));

        close_test_db(&db, dir).await;
    }

    /// 先写入非空音频轨、再写入空串：表达“取消轨对”，load 必须回到 None
    /// 而不是残留成 Some("")——这是空串归一化分支的边界行为。
    #[tokio::test]
    async fn save_audio_url_with_empty_string_clears_back_to_none() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "pair2").await;

        db.save_audio_url("pair2", "http://example.com/audio.m4a")
            .await
            .expect("save audio_url");
        db.save_audio_url("pair2", "")
            .await
            .expect("clear audio_url");

        let audio_url = db.load_audio_url("pair2").await.expect("load audio_url");
        assert_eq!(
            audio_url, None,
            "clearing the audio track must fall back to the default state"
        );

        close_test_db(&db, dir).await;
    }

    /// 段行布局属主令牌守卫：持有旧 epoch 的迟到写入必须 0 行生效（含
    /// start_byte 恒为 0 的段 0——单靠边界匹配无法拦截的那一类），持有
    /// 当前 epoch 的写入正常生效且被钳制到段长。
    #[tokio::test]
    async fn stale_epoch_segment_write_is_dropped() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "epoch1").await;
        db.insert_segments("epoch1", &[(0, 0, 999), (1, 1000, 1999)])
            .await
            .expect("insert segments");

        // 新 spawn 夺权（epoch=2）。
        db.set_segments_epoch("epoch1", 2).await.expect("set epoch");

        // 旧 spawn（epoch=1）迟到写段 0：必须被丢弃。
        db.update_segment_progress_bounded("epoch1", 0, 5000, 0, 1)
            .await
            .expect("stale write");
        let segs = db.load_segments("epoch1").await.expect("load");
        assert_eq!(
            segs[0].downloaded_bytes, 0,
            "stale-epoch write on segment 0 must affect zero rows"
        );

        // 当前 spawn（epoch=2）写入：生效且钳制到段长（1000）。
        db.update_segment_progress_bounded("epoch1", 0, 5000, 0, 2)
            .await
            .expect("current write");
        let segs = db.load_segments("epoch1").await.expect("load");
        assert_eq!(
            segs[0].downloaded_bytes, 1000,
            "current-epoch write must land, clamped to the segment span"
        );

        // 边界不匹配（start_byte 错位）在同 epoch 下仍被拒。
        db.update_segment_progress_bounded("epoch1", 1, 500, 999, 2)
            .await
            .expect("mismatched-start write");
        let segs = db.load_segments("epoch1").await.expect("load");
        assert_eq!(
            segs[1].downloaded_bytes, 0,
            "start_byte-mismatched write must affect zero rows"
        );

        close_test_db(&db, dir).await;
    }

    /// 批量段进度：多行一次事务写回；epoch 不匹配 0 生效；空批 no-op。
    #[tokio::test]
    async fn update_segments_progress_batch_writes_and_guards_epoch() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "batch1").await;
        db.insert_segments("batch1", &[(0, 0, 999), (1, 1000, 1999), (2, 2000, 2999)])
            .await
            .expect("insert segments");
        db.set_segments_epoch("batch1", 3).await.expect("set epoch");

        // 空批：no-op，不报错。
        db.update_segments_progress_batch("batch1", 3, &[])
            .await
            .expect("empty batch");

        // 当前 epoch 批量写：生效且段长钳制。
        db.update_segments_progress_batch(
            "batch1",
            3,
            &[(0, 500, 0), (1, 5000, 1000), (2, 100, 2000)],
        )
        .await
        .expect("batch write");
        let segs = db.load_segments("batch1").await.expect("load");
        assert_eq!(segs[0].downloaded_bytes, 500);
        assert_eq!(
            segs[1].downloaded_bytes, 1000,
            "must clamp to segment span (1000)"
        );
        assert_eq!(segs[2].downloaded_bytes, 100);

        // 旧 epoch：0 行生效，水位保持。
        db.update_segments_progress_batch("batch1", 1, &[(0, 900, 0)])
            .await
            .expect("stale epoch batch");
        let segs = db.load_segments("batch1").await.expect("load after stale");
        assert_eq!(
            segs[0].downloaded_bytes, 500,
            "stale-epoch batch must affect zero rows"
        );

        // start_byte 错位：0 行生效。
        db.update_segments_progress_batch("batch1", 3, &[(1, 200, 999)])
            .await
            .expect("mismatched start batch");
        let segs = db
            .load_segments("batch1")
            .await
            .expect("load after mismatch");
        assert_eq!(
            segs[1].downloaded_bytes, 1000,
            "start_byte-mismatched batch row must affect zero rows"
        );

        close_test_db(&db, dir).await;
    }

    /// 加速来源累计：多次增量累加、旧 epoch 迟到写入被拒、`load_task_by_id`
    /// 与专用 getter 读到一致的值；进度复位（`update_task_progress(_, 0)` /
    /// `reset_segments_progress` / `delete_segments`）同步清零，非 0 进度写入不动它。
    #[tokio::test]
    async fn task_source_bytes_accumulate_guard_epoch_and_reset_with_progress() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "src1").await;
        db.set_segments_epoch("src1", 5).await.expect("set epoch");
        assert_eq!(
            db.load_task_source_bytes("src1").await.expect("load"),
            SourceBytes::default()
        );

        let first = SourceBytes {
            cdn: 100,
            proxy: 20,
            nic: 3,
        };
        let second = SourceBytes {
            cdn: 50,
            proxy: 0,
            nic: 7,
        };
        db.add_task_source_bytes("src1", 5, first)
            .await
            .expect("first delta");
        db.add_task_source_bytes("src1", 5, second)
            .await
            .expect("second delta");
        let expected = first.saturating_add(second);
        assert_eq!(
            db.load_task_source_bytes("src1").await.expect("load"),
            expected
        );
        let info = db
            .load_task_by_id("src1")
            .await
            .expect("load task")
            .expect("task exists");
        assert_eq!(info.source_bytes, expected, "TaskInfo must round-trip");

        // 被新 spawn 夺权后，旧 coordinator 迟到写入 0 行生效。
        db.add_task_source_bytes("src1", 4, first)
            .await
            .expect("stale delta");
        assert_eq!(
            db.load_task_source_bytes("src1").await.expect("load"),
            expected,
            "stale-epoch delta must affect zero rows"
        );

        // 非 0 进度写入不清零计数。
        db.update_task_progress("src1", 4096)
            .await
            .expect("progress");
        assert_eq!(
            db.load_task_source_bytes("src1").await.expect("load"),
            expected
        );

        // 三条进度复位路径各自清零。
        db.update_task_progress("src1", 0).await.expect("reset");
        assert_eq!(
            db.load_task_source_bytes("src1").await.expect("load"),
            SourceBytes::default(),
            "update_task_progress(_, 0) must clear counters"
        );
        db.add_task_source_bytes("src1", 5, first)
            .await
            .expect("delta");
        db.reset_segments_progress("src1")
            .await
            .expect("reset segs");
        assert_eq!(
            db.load_task_source_bytes("src1").await.expect("load"),
            SourceBytes::default(),
            "reset_segments_progress must clear counters"
        );
        db.add_task_source_bytes("src1", 5, first)
            .await
            .expect("delta");
        db.delete_segments("src1").await.expect("delete segs");
        assert_eq!(
            db.load_task_source_bytes("src1").await.expect("load"),
            SourceBytes::default(),
            "delete_segments must clear counters"
        );

        // 不存在的任务按全 0 处理。
        assert_eq!(
            db.load_task_source_bytes("missing").await.expect("load"),
            SourceBytes::default()
        );

        close_test_db(&db, dir).await;
    }

    // -----------------------------------------------------------------------
    // 队列控制：播种 / 启停与定时持久化 / 队列内顺序 / 全局恢复候选
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn seed_builtin_queues_is_idempotent_and_migrates_legacy() {
        let (db, dir) = open_test_db().await;
        // 播种前的存量数据：queue_id='' 任务 + 一个自定义队列（position 0）。
        insert_task(&db, "legacy").await;
        db.insert_queue("custom", "我的队列", 0, 0, 0, "", 0, 0, "")
            .await
            .expect("insert custom queue");

        db.seed_builtin_queues().await.expect("seed");
        db.seed_builtin_queues().await.expect("seed twice");

        let queues = db.load_all_queues().await.expect("load queues");
        assert_eq!(queues.len(), 3, "main + later + custom");
        assert_eq!(queues[0].queue_id, MAIN_QUEUE_ID);
        assert!(queues[0].is_running, "main seeds running");
        assert_eq!(queues[1].queue_id, crate::model::LATER_QUEUE_ID);
        assert!(!queues[1].is_running, "later seeds stopped");
        assert_eq!(
            queues[2].queue_id, "custom",
            "existing queues shift behind the builtins"
        );

        let t = db
            .load_task_by_id("legacy")
            .await
            .expect("load")
            .expect("row");
        assert_eq!(t.queue_id, MAIN_QUEUE_ID, "'' tasks migrate to main");
        assert_eq!(
            db.get_config("default_queue_id")
                .await
                .expect("cfg")
                .as_deref(),
            Some(MAIN_QUEUE_ID)
        );
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn queue_upload_limit_column_roundtrip() {
        let (db, dir) = open_test_db().await;
        // 建队列时写入两级限速；新库/迁移库缺省 0（不限）。
        db.insert_queue("q-up", "上传限速", 100, 512, 0, "", 0, 0, "")
            .await
            .expect("insert queue");
        let q = db
            .load_all_queues()
            .await
            .expect("load queues")
            .into_iter()
            .find(|q| q.queue_id == "q-up")
            .expect("queue exists");
        assert_eq!(q.speed_limit_kbps, 100);
        assert_eq!(q.upload_limit_kbps, 512);

        // 更新路径独立持久化上传限速；0 回落为「不限」。
        db.update_queue("q-up", "上传限速", 100, 0, 0, "", 0, "")
            .await
            .expect("update queue");
        let q = db
            .load_all_queues()
            .await
            .expect("load queues")
            .into_iter()
            .find(|q| q.queue_id == "q-up")
            .expect("queue exists");
        assert_eq!(q.upload_limit_kbps, 0, "0 persists as unlimited");

        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn seed_does_not_override_existing_default_queue_config() {
        let (db, dir) = open_test_db().await;
        db.set_config("default_queue_id", "mine")
            .await
            .expect("cfg");
        db.seed_builtin_queues().await.expect("seed");
        assert_eq!(
            db.get_config("default_queue_id")
                .await
                .expect("cfg")
                .as_deref(),
            Some("mine"),
            "an explicit default queue must survive seeding"
        );
        close_test_db(&db, dir).await;
    }

    /// 旧版本允许停止 / 定时主队列：升级后再次播种必须把主队列收敛为运行中、无定时，
    /// 其他队列的启停与定时原样保留。
    #[tokio::test]
    async fn reseeding_migrates_stopped_or_scheduled_main_queue() {
        let (db, dir) = open_test_db().await;
        db.seed_builtin_queues().await.expect("seed");
        for id in [MAIN_QUEUE_ID, crate::model::LATER_QUEUE_ID] {
            db.set_queue_running(id, false).await.expect("stop");
            db.set_queue_schedule(id, true, "08:30", "23:00", 0b001_1111)
                .await
                .expect("schedule");
        }
        db.seed_builtin_queues().await.expect("reseed");
        let queues = db.load_all_queues().await.expect("load");
        let find = |id: &str| queues.iter().find(|q| q.queue_id == id).expect("queue row");
        let main = find(MAIN_QUEUE_ID);
        assert!(main.is_running);
        assert!(!main.schedule_enabled);
        assert!(main.schedule_start.is_empty() && main.schedule_stop.is_empty());
        assert_eq!(main.schedule_days, 127);
        let later = find(crate::model::LATER_QUEUE_ID);
        assert!(!later.is_running);
        assert!(later.schedule_enabled);
        assert_eq!(later.schedule_start, "08:30");
        assert_eq!(later.schedule_stop, "23:00");
        assert_eq!(later.schedule_days, 0b001_1111);
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn appended_queue_order_uses_covering_index() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("memory database");
        let plan = sqlx::query(
            "EXPLAIN QUERY PLAN SELECT CAST(COALESCE(MAX(queue_order), 0) + 1 AS BIGINT) FROM tasks WHERE queue_id = $1",
        )
        .bind(MAIN_QUEUE_ID)
        .fetch_all(&db.pool)
        .await
        .expect("queue append query plan");
        assert!(
            plan.iter().any(|row| row
                .get::<String, _>("detail")
                .contains("COVERING INDEX idx_tasks_queue_order")),
            "appending a task must not scan all historical task rows"
        );
    }

    /// 插入任务自动追加 queue_order；startable 顺序 = queue_order → created_at；
    /// 完成态任务不参与启动。
    #[tokio::test]
    async fn queue_startable_ids_follow_explicit_order() {
        let (db, dir) = open_test_db().await;
        for id in ["a", "b", "c"] {
            db.insert_task(id, "http://e/f", "f", "/tmp", 1, 0, "", "q", "", 2)
                .await
                .expect("insert");
        }
        let c = db.load_task_by_id("c").await.expect("load").expect("row");
        assert_eq!(c.queue_order, 3, "inserts append to the queue tail");

        db.reorder_queue_tasks("q", &["c".into(), "a".into(), "b".into()])
            .await
            .expect("reorder");
        let ids = db.queue_startable_task_ids("q").await.expect("startable");
        assert_eq!(ids, vec!["c", "a", "b"]);

        db.update_task_status("a", 3, "").await.expect("complete");
        let ids = db.queue_startable_task_ids("q").await.expect("startable");
        assert_eq!(ids, vec!["c", "b"], "completed tasks drop out");
        close_test_db(&db, dir).await;
    }

    /// 全局恢复候选排除停止队列内的任务；孤儿 queue_id 视作运行中。
    #[tokio::test]
    async fn eligible_resume_skips_stopped_queues() {
        let (db, dir) = open_test_db().await;
        db.seed_builtin_queues().await.expect("seed");
        for (id, q) in [
            ("m", MAIN_QUEUE_ID),
            ("l", crate::model::LATER_QUEUE_ID),
            ("o", "ghost"),
        ] {
            db.insert_task(id, "http://e/f", "f", "/tmp", 1, 0, "", q, "", 2)
                .await
                .expect("insert");
        }
        let mut ids = db.eligible_resume_task_ids().await.expect("eligible");
        ids.sort();
        assert_eq!(
            ids,
            vec!["m", "o"],
            "tasks inside the stopped later queue must be excluded"
        );
        close_test_db(&db, dir).await;
    }

    /// 移动任务追加到目标队列尾部；删除队列把任务归还主队列并清除显式顺序。
    #[tokio::test]
    async fn move_and_delete_queue_maintain_order() {
        let (db, dir) = open_test_db().await;
        db.seed_builtin_queues().await.expect("seed");
        db.insert_task("x", "http://e/f", "f", "/tmp", 1, 0, "", "q2", "", 0)
            .await
            .expect("insert x");
        db.insert_task("y", "http://e/f", "f", "/tmp", 1, 0, "", "q2", "", 0)
            .await
            .expect("insert y");

        db.move_task_to_queue("x", "q3").await.expect("move");
        let x = db.load_task_by_id("x").await.expect("load").expect("row");
        assert_eq!(x.queue_id, "q3");
        assert_eq!(x.queue_order, 1, "first task in the target queue");

        db.insert_queue("q2", "Q2", 0, 0, 0, "", 9, 0, "")
            .await
            .expect("queue row");
        assert_eq!(db.delete_queue("q2").await.expect("delete"), vec!["y"]);
        let y = db.load_task_by_id("y").await.expect("load").expect("row");
        assert_eq!(y.queue_id, MAIN_QUEUE_ID);
        assert_eq!(y.queue_order, 0, "explicit order resets on reassignment");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn exclusive_open_rejects_second_writer_until_guard_drops() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_writer_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create writer test dir");

        let (first_db, first_guard) = Db::open_exclusive(&dir).await.expect("first writer");
        let second = Db::open_exclusive(&dir).await;
        assert!(matches!(second, Err(DbError::WriterLeaseHeld(_))));

        first_db.pool.close().await;
        drop(first_db);
        drop(first_guard);

        let (next_db, next_guard) = Db::open_exclusive(&dir)
            .await
            .expect("writer after release");
        next_db.pool.close().await;
        drop(next_db);
        drop(next_guard);

        if let Err(error) = std::fs::remove_dir_all(dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    #[tokio::test]
    async fn config_patch_is_atomic_and_revision_guarded() {
        let (db, dir) = open_test_db().await;
        let values = BTreeMap::from([
            ("max_concurrent_tasks".to_owned(), "7".to_owned()),
            ("default_segments".to_owned(), "8".to_owned()),
        ]);
        let revision = db
            .apply_config_patch_atomic(0, &values)
            .await
            .expect("first config patch");
        assert_eq!(revision, 1);
        assert_eq!(
            db.get_config("max_concurrent_tasks").await.expect("read"),
            Some("7".to_owned())
        );

        let stale = BTreeMap::from([("speed_limit_bytes".to_owned(), "100".to_owned())]);
        assert!(matches!(
            db.apply_config_patch_atomic(0, &stale).await,
            Err(ConfigPatchError::RevisionConflict {
                expected: 0,
                current: 1
            })
        ));
        assert_eq!(
            db.get_config("speed_limit_bytes").await.expect("read"),
            None
        );
        close_test_db(&db, dir).await;
    }

    fn new_row<'a>(id: &'a str, queue_id: &'a str) -> NewTaskRow<'a> {
        NewTaskRow {
            id,
            url: "http://example.test/a.bin",
            file_name: "a.bin",
            name_inferred: false,
            save_dir: "/tmp",
            segments: 0,
            status: 2,
            error_message: "",
            proxy_url: "",
            queue_id,
            checksum: "",
            ignore_tls_errors: false,
            cookies: "",
            referrer: "",
            extra_headers_json: "",
            origin_url: "",
            audio_url: "",
            resolver_plugin_id: "",
            group_id: "",
            resolver_item: "",
            unattended: false,
            resolver_choice_pending: false,
            bt_selected_files: "",
            category_route: "",
            request_method: "",
            torrent_file_bytes: &[],
            bt_sequential: false,
            bt_first_last_pieces_first: false,
        }
    }

    #[tokio::test]
    async fn insert_new_tasks_appends_order_per_queue_and_keeps_input_order() {
        let (db, dir) = open_test_db().await;
        let rows = [new_row("a", "q1"), new_row("b", "q2"), new_row("c", "q1")];
        let tasks = db.insert_new_tasks(&rows, None).await.expect("insert");
        let order: Vec<_> = tasks
            .iter()
            .map(|task| (task.task_id.as_str(), task.queue_order))
            .collect();
        assert_eq!(order, [("a", 1), ("b", 1), ("c", 2)]);
        let more = db
            .insert_new_tasks(&[new_row("d", "q1")], None)
            .await
            .expect("append");
        assert_eq!(more[0].queue_order, 3, "a later call continues the tail");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn insert_new_tasks_rolls_back_rows_when_the_batch_record_conflicts() {
        let (db, dir) = open_test_db().await;
        let record = || {
            Some(CreateBatchRecord {
                batch_id: "dup",
                results: "[]",
            })
        };
        db.insert_new_tasks(&[new_row("a", "q")], record())
            .await
            .expect("first");
        assert_eq!(
            db.load_create_batch_results("dup").await.expect("read"),
            Some("[]".to_owned())
        );
        let err = db.insert_new_tasks(&[new_row("b", "q")], record()).await;
        assert!(
            err.is_err(),
            "a committed batch id cannot be recorded twice"
        );
        assert!(
            db.load_task_by_id("b").await.expect("load").is_none(),
            "the whole transaction rolled back"
        );
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn create_batch_records_expire_after_retention_in_the_same_transaction() {
        let (db, dir) = open_test_db().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        for (batch_id, age) in [
            ("old", CREATE_BATCH_RETENTION_SECS + 60),
            ("fresh", CREATE_BATCH_RETENTION_SECS - 3600),
        ] {
            sqlx::query(
                "INSERT INTO task_create_batches (batch_id, results, created_at) VALUES ($1, '[]', $2)",
            )
            .bind(batch_id)
            .bind(now - age)
            .execute(&db.pool)
            .await
            .expect("seed record");
        }
        db.insert_new_tasks(
            &[],
            Some(CreateBatchRecord {
                batch_id: "new",
                results: "[]",
            }),
        )
        .await
        .expect("record only");
        assert_eq!(
            db.load_create_batch_results("old").await.expect("read"),
            None
        );
        assert!(
            db.load_create_batch_results("fresh")
                .await
                .expect("read")
                .is_some()
        );
        assert!(
            db.load_create_batch_results("new")
                .await
                .expect("read")
                .is_some()
        );
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn sqlite_connections_use_synchronous_normal() {
        let (db, dir) = open_test_db().await;
        let level: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&db.pool)
            .await
            .expect("read pragma");
        assert_eq!(level, 1, "NORMAL = 1");
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn cdn_lease_peek_and_ack_preserve_matching_batch() {
        let (db, dir) = open_test_db().await;
        let first = r#"{"batchId":"batch-1","samples":[{"ok":true}]}"#;
        let second = r#"{"batchId":"batch-2","samples":[{"ok":false}]}"#;
        assert_eq!(
            db.lease_cdn_reports(first).await.expect("first lease"),
            first
        );
        assert_eq!(
            db.lease_cdn_reports(second).await.expect("existing lease"),
            first
        );
        assert!(!db.ack_cdn_report_lease("batch-2").await.expect("wrong ack"));
        assert_eq!(
            db.get_config("cdn_report_lease").await.expect("read"),
            Some(first.to_owned())
        );
        assert!(
            db.ack_cdn_report_lease("batch-1")
                .await
                .expect("matching ack")
        );
        assert_eq!(
            db.get_config("cdn_report_lease").await.expect("read"),
            Some(String::new())
        );
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn peek_cdn_lease_caches_empty_and_tracks_lease_changes() {
        let (db, dir) = open_test_db().await;
        assert_eq!(db.peek_cdn_report_lease().await.expect("peek"), None);
        // 已缓存为空：即便绕过 Db API 直接写库，peek 也不再读库。
        db.set_config("cdn_report_lease", "{\"batchId\":\"x\"}")
            .await
            .expect("raw write");
        assert_eq!(db.peek_cdn_report_lease().await.expect("cached"), None);
        // 经 lease API 写入后缓存失效。
        db.set_config("cdn_report_lease", "").await.expect("reset");
        let lease = r#"{"batchId":"b1","samples":[]}"#;
        db.lease_cdn_reports(lease).await.expect("lease");
        assert_eq!(
            db.peek_cdn_report_lease().await.expect("peek"),
            Some(lease.to_owned())
        );
        assert!(db.ack_cdn_report_lease("b1").await.expect("ack"));
        assert_eq!(db.peek_cdn_report_lease().await.expect("peek"), None);
        close_test_db(&db, dir).await;
    }

    #[tokio::test]
    async fn idle_file_scan_legacy_seed_migrates_once() {
        let (db, dir) = open_test_db().await;
        // 新库：默认关闭。
        db.init_default_config("/tmp").await.expect("init");
        assert_eq!(
            db.get_config("idle_file_scan").await.expect("read"),
            Some("0".to_owned())
        );
        // 模拟旧库：值为 "1" 且尚无迁移标记。
        db.delete_config("migration_idle_file_scan_off")
            .await
            .expect("drop marker");
        db.set_config("idle_file_scan", "1").await.expect("legacy");
        db.init_default_config("/tmp").await.expect("migrate");
        assert_eq!(
            db.get_config("idle_file_scan").await.expect("read"),
            Some("0".to_owned())
        );
        // 迁移后用户自行开启，再次启动不得被改回。
        db.set_config("idle_file_scan", "true").await.expect("user");
        db.init_default_config("/tmp").await.expect("restart");
        assert_eq!(
            db.get_config("idle_file_scan").await.expect("read"),
            Some("true".to_owned())
        );
        close_test_db(&db, dir).await;
    }

    #[cfg(feature = "ed2k")]
    #[tokio::test]
    async fn ed2k_server_sub_legacy_http_migrates_once() {
        let (db, dir) = open_test_db().await;
        db.init_default_config("/tmp").await.expect("init");
        let fresh = db
            .get_config("ed2k_server_sub_urls")
            .await
            .expect("read")
            .unwrap_or_default();
        assert!(
            !fresh.contains("http://"),
            "default must be https only: {fresh}"
        );
        // 模拟旧库：明文默认值 + 用户追加的其他地址，尚无迁移标记。
        db.delete_config("migration_ed2k_server_sub_https")
            .await
            .expect("drop marker");
        db.set_config(
            "ed2k_server_sub_urls",
            "http://upd.emule-security.org/server.met\nhttp://example.com/server.met",
        )
        .await
        .expect("legacy");
        db.init_default_config("/tmp").await.expect("migrate");
        assert_eq!(
            db.get_config("ed2k_server_sub_urls").await.expect("read"),
            Some(
                "https://upd.emule-security.org/server.met\nhttp://example.com/server.met"
                    .to_owned()
            )
        );
        // 迁移后用户自行改回 http，再次启动不得被改写。
        let user = "http://upd.emule-security.org/server.met";
        db.set_config("ed2k_server_sub_urls", user)
            .await
            .expect("user");
        db.init_default_config("/tmp").await.expect("restart");
        assert_eq!(
            db.get_config("ed2k_server_sub_urls").await.expect("read"),
            Some(user.to_owned())
        );
        close_test_db(&db, dir).await;
    }

    /// 直接设置测试行的 status / completed_at / seeding_status。
    async fn set_completion(db: &Db, id: &str, status: i32, completed_at: &str, seeding: i32) {
        sqlx::query(
            "UPDATE tasks SET status = $1, completed_at = $2, seeding_status = $3 WHERE id = $4",
        )
        .bind(status)
        .bind(completed_at)
        .bind(seeding)
        .bind(id)
        .execute(&db.pool)
        .await
        .expect("set completion");
    }

    /// 保留期查询契约：只选 status=3、completed_at 非空且严格早于 cutoff、
    /// 非做种（1/8）的任务；其余（较新、空时间戳、未完成、做种中）一律不选。
    #[tokio::test]
    async fn list_expired_completed_selects_only_old_non_seeding_rows() {
        let (db, dir) = open_test_db().await;
        for id in [
            "old", "boundary", "new", "blank", "paused", "seed1", "seed8", "seedstop",
        ] {
            insert_task_with_size(&db, id, 10).await;
        }
        set_completion(&db, "old", 3, "1000", 0).await;
        set_completion(&db, "boundary", 3, "5000", 0).await;
        set_completion(&db, "new", 3, "9000", 0).await;
        set_completion(&db, "blank", 3, "", 0).await;
        set_completion(&db, "paused", 2, "1000", 0).await;
        set_completion(&db, "seed1", 3, "1000", 1).await;
        set_completion(&db, "seed8", 3, "1000", 8).await;
        // 其他做种终态（用户停止等）不是在做种，可清理。
        set_completion(&db, "seedstop", 3, "2000", 2).await;

        let ids = db
            .list_expired_completed_task_ids(5000, 100)
            .await
            .expect("list");
        assert_eq!(ids, ["old", "seedstop"]);

        let limited = db
            .list_expired_completed_task_ids(5000, 1)
            .await
            .expect("limit");
        assert_eq!(limited, ["old"]);
        close_test_db(&db, dir).await;
    }

    /// 迁移契约：已完成但无时间戳的遗留行仅在首次迁移时被补写；
    /// 其他状态与已有时间戳不动，迁移后新出现的空时间戳行不再被改写。
    #[tokio::test]
    async fn completed_at_backfill_migrates_once() {
        let (db, dir) = open_test_db().await;
        db.init_default_config("/tmp").await.expect("init");
        insert_task_with_size(&db, "legacy", 10).await;
        insert_task_with_size(&db, "stamped", 10).await;
        insert_task_with_size(&db, "running", 10).await;
        set_completion(&db, "legacy", 3, "", 0).await;
        set_completion(&db, "stamped", 3, "1234", 0).await;
        set_completion(&db, "running", 1, "", 0).await;

        db.delete_config("migration_completed_at_backfill")
            .await
            .expect("drop marker");
        db.init_default_config("/tmp").await.expect("migrate");
        let load = |id: &'static str| {
            let db = db.clone();
            async move { db.load_task_by_id(id).await.expect("load").expect("row") }
        };
        let legacy = load("legacy").await.completed_at;
        assert!(
            legacy.parse::<u64>().is_ok_and(|v| v > 1_000_000),
            "遗留行必须补写当前时间，got {legacy:?}"
        );
        assert_eq!(load("stamped").await.completed_at, "1234");
        assert_eq!(load("running").await.completed_at, "");

        // 迁移后再次启动：新出现的空时间戳行不得被改写。
        set_completion(&db, "legacy", 3, "", 0).await;
        db.init_default_config("/tmp").await.expect("restart");
        assert_eq!(load("legacy").await.completed_at, "");
        close_test_db(&db, dir).await;
    }
}
