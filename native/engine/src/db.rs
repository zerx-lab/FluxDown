use std::collections::HashMap;
use std::path::Path;

use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::{AssertSqlSafe, Row};
use thiserror::Error;

use crate::model::{QueueInfo, TaskInfo};

#[derive(Error, Debug)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("unsupported database url: {0}")]
    UnsupportedUrl(String),
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
    bt_selected_files TEXT NOT NULL DEFAULT '',
    bt_custom_name TEXT NOT NULL DEFAULT '',
    orig_etag TEXT NOT NULL DEFAULT '',
    orig_last_modified TEXT NOT NULL DEFAULT '',
    audio_url TEXT NOT NULL DEFAULT '',
    file_missing INTEGER NOT NULL DEFAULT 0
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
    max_concurrent INTEGER NOT NULL DEFAULT 0,
    default_save_dir TEXT NOT NULL DEFAULT '',
    position INTEGER NOT NULL DEFAULT 0,
    default_segments INTEGER NOT NULL DEFAULT 0,
    default_user_agent TEXT NOT NULL DEFAULT ''
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
    bt_selected_files TEXT NOT NULL DEFAULT '',
    bt_custom_name TEXT NOT NULL DEFAULT '',
    orig_etag TEXT NOT NULL DEFAULT '',
    orig_last_modified TEXT NOT NULL DEFAULT '',
    audio_url TEXT NOT NULL DEFAULT '',
    file_missing INTEGER NOT NULL DEFAULT 0
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
    max_concurrent INTEGER NOT NULL DEFAULT 0,
    default_save_dir TEXT NOT NULL DEFAULT '',
    position INTEGER NOT NULL DEFAULT 0,
    default_segments INTEGER NOT NULL DEFAULT 0,
    default_user_agent TEXT NOT NULL DEFAULT ''
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
";

/// SQLite 连接级 PRAGMA（在 `after_connect` 钩子中对每个新连接执行）。
/// `foreign_keys=ON` 是 sqlx-sqlite 的默认值，无需重复设置。
/// `busy_timeout` 让撞上写锁的连接在 5s 内自旋重试而非立即抛
/// `SQLITE_BUSY`（code 5, database is locked）——覆盖多任务并发落库 /
/// WAL checkpoint / 删除事务之间的瞬时写-写冲突。
const SQLITE_PRAGMAS: &str = "PRAGMA journal_mode=WAL;\
 PRAGMA busy_timeout=5000;\
 PRAGMA cache_size=-512;\
 PRAGMA temp_store=MEMORY;\
 PRAGMA mmap_size=0;\
 PRAGMA wal_autocheckpoint=1000;";

#[derive(Clone)]
pub struct Db {
    pool: sqlx::AnyPool,
    backend: Backend,
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
        file_missing: row.try_get::<i32, _>("file_missing").unwrap_or(0) != 0,
    })
}

const TASK_COLUMNS: &str = "id, url, file_name, save_dir, status, downloaded_bytes, total_bytes, error_message, created_at, proxy_url, queue_id, checksum, file_missing";

impl Db {
    /// 在 `dir` 目录下打开（不存在则创建）SQLite 数据库 `flux_down.db`。
    ///
    /// 桌面 App 的默认持久化路径；服务器端可改用 [`Db::connect`] 按 URL
    /// 连接 SQLite 或 PostgreSQL。
    pub async fn open(dir: &Path) -> Result<Self, DbError> {
        // Windows 绝对路径统一为正斜杠 + 单冒号形式（sqlite:C:/…?mode=rwc）。
        let db_path = dir.join("flux_down.db");
        let url = format!(
            "sqlite:{}?mode=rwc",
            db_path.to_string_lossy().replace('\\', "/")
        );
        Self::connect(&url).await
    }

    /// 按连接 URL 打开数据库。
    ///
    /// - `sqlite:/path/to/db?mode=rwc` / `sqlite::memory:` → SQLite
    /// - `postgres://user:pass@host:5432/db` → PostgreSQL
    ///
    /// 其余 scheme 返回 [`DbError::UnsupportedUrl`]。
    pub async fn connect(url: &str) -> Result<Self, DbError> {
        // 幂等：内部由 Once 保护，可安全多次调用。
        sqlx::any::install_default_drivers();
        let backend = Backend::from_url(url)?;
        // `sqlite::memory:` 下每个池连接是彼此独立的内存库——必须钳制为
        // 单连接，否则连接轮换会"丢库"（主要影响测试）。
        let max_connections = if backend == Backend::Sqlite && url.contains(":memory:") {
            1
        } else {
            5
        };
        let pool = AnyPoolOptions::new()
            .max_connections(max_connections)
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    if conn.backend_name() == "SQLite" {
                        sqlx::raw_sql(SQLITE_PRAGMAS).execute(&mut *conn).await?;
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        let db = Self { pool, backend };
        db.init_schema().await?;
        Ok(db)
    }

    async fn init_schema(&self) -> Result<(), DbError> {
        let schema = match self.backend {
            Backend::Sqlite => SQLITE_SCHEMA,
            Backend::Postgres => POSTGRES_SCHEMA,
        };
        sqlx::raw_sql(schema).execute(&self.pool).await?;

        // --- Schema migrations（幂等，只为升级旧库；新库建表已含全量列） ---
        self.add_column_if_missing("tasks", "proxy_url", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("tasks", "queue_id", "TEXT NOT NULL DEFAULT ''")
            .await?;
        self.add_column_if_missing("queues", "default_segments", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.add_column_if_missing("tasks", "checksum", "TEXT NOT NULL DEFAULT ''")
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
    ) -> Result<(), DbError> {
        let now = chrono_now();
        sqlx::query(
            "INSERT INTO tasks (id, url, file_name, save_dir, status, segments, total_bytes, created_at, proxy_url, queue_id, checksum)
             VALUES ($1, $2, $3, $4, 0, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(url)
        .bind(file_name)
        .bind(save_dir)
        .bind(segments)
        .bind(total_bytes)
        .bind(now)
        .bind(proxy_url)
        .bind(queue_id)
        .bind(checksum)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_task_progress(
        &self,
        id: &str,
        downloaded_bytes: i64,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET downloaded_bytes = $1 WHERE id = $2")
            .bind(downloaded_bytes)
            .bind(id)
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

    pub async fn update_task_status(
        &self,
        id: &str,
        status: i32,
        error_message: &str,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET status = $1, error_message = $2 WHERE id = $3")
            .bind(status)
            .bind(error_message)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
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
        let mut tx = self.pool.begin().await?;

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

    /// 更新任务文件名（仅当任务文件名为空时，防止覆盖用户自定义名称）
    pub async fn update_task_file_name(
        &self,
        task_id: &str,
        file_name: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE tasks SET file_name = $1 WHERE id = $2 AND (file_name = '' OR file_name IS NULL)",
        )
        .bind(file_name)
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 启动时将所有 downloading(1)、pending(0)、preparing(5) 的任务矫正为 paused(2)
    /// 因为重启后没有活跃的下载线程，这些任务实际上处于暂停状态
    pub async fn reset_incomplete_tasks_to_paused(&self) -> Result<u64, DbError> {
        let result = sqlx::query("UPDATE tasks SET status = 2 WHERE status IN (0, 1, 5)")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
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

    pub async fn delete_task(&self, id: &str) -> Result<(), DbError> {
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
        // 任务级 config 行(完成幂等哨兵 bt_completion_top_<id>、HLS 断点
        // hls_resume_<id>)随任务一并清理,防孤儿行累积。
        sqlx::query("DELETE FROM config WHERE key IN ($1, $2)")
            .bind(format!("bt_completion_top_{id}"))
            .bind(format!("hls_resume_{id}"))
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM tasks WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Batch-delete multiple tasks in a single transaction.
    /// Uses chunked IN clauses to respect SQLite's 999 variable limit.
    pub async fn delete_tasks_batch(&self, ids: &[String]) -> Result<(), DbError> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        const CHUNK: usize = 500;
        for chunk in ids.chunks(CHUNK) {
            let placeholders: String = (1..=chunk.len())
                .map(|i| format!("${i}"))
                .collect::<Vec<_>>()
                .join(",");

            for table in ["task_segments", "torrent_files"] {
                let sql = format!("DELETE FROM {table} WHERE task_id IN ({placeholders})");
                let mut query = sqlx::query(AssertSqlSafe(sql));
                for id in chunk {
                    query = query.bind(id.as_str());
                }
                query.execute(&mut *tx).await?;
            }

            // 任务级 config 行(哨兵/HLS 断点)随任务清理,防孤儿行累积。
            for id in chunk {
                sqlx::query("DELETE FROM config WHERE key IN ($1, $2)")
                    .bind(format!("bt_completion_top_{id}"))
                    .bind(format!("hls_resume_{id}"))
                    .execute(&mut *tx)
                    .await?;
            }

            let sql = format!("DELETE FROM tasks WHERE id IN ({placeholders})");
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id.as_str());
            }
            query.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
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
        let default_sub_urls = crate::tracker_subscription::default_subscription_urls();
        let default_ed2k_met_urls = crate::ed2k::server_subscription::default_server_met_urls();
        let defaults: &[(&str, &str)] = &[
            ("default_save_dir", default_save_dir),
            ("default_segments", "0"),
            ("max_concurrent_tasks", "5"),
            ("speed_limit_bytes", "0"),
            // 自动重试：-1=无限，0=关闭，1..10=次数。延迟（秒）固定基值×已重试次数。
            ("max_auto_retries", "3"),
            ("auto_retry_delay_secs", "5"),
            ("auto_resume_on_start", "false"),
            ("close_to_tray", "true"),
            ("auto_startup", "false"),
            ("auto_check_update", "true"),
            ("bt_enable_dht", "true"),
            ("bt_enable_upnp", "true"),
            ("bt_port_start", "6881"),
            ("bt_port_end", "6891"),
            ("bt_custom_trackers", ""),
            // Tracker 订阅：默认启用，订阅社区流行的两个精选列表
            // （XIU2/TrackersListCollection + ngosang/trackerslist）。
            // cache 由订阅刷新流程写入，updated_at=0 表示从未更新。
            ("bt_tracker_sub_enabled", "true"),
            ("bt_tracker_sub_urls", &default_sub_urls),
            ("bt_tracker_sub_cache", ""),
            ("bt_tracker_sub_updated_at", "0"),
            ("torrent_assoc_prompted", "false"),
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
        ];
        for (key, value) in defaults {
            sqlx::query(
                "INSERT INTO config (key, value) VALUES ($1, $2)
                 ON CONFLICT (key) DO NOTHING",
            )
            .bind(*key)
            .bind(*value)
            .execute(&self.pool)
            .await?;
        }
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
        sqlx::query("UPDATE tasks SET downloaded_bytes = 0 WHERE id = $1")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // ED2K blocks / hashset
    // -----------------------------------------------------------------------

    /// Initialise all block rows (state=0 missing) for an ed2k task.
    /// Idempotent per (task_id, block_index) via ON CONFLICT DO NOTHING.
    pub async fn init_ed2k_blocks(&self, task_id: &str, block_count: u64) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        for i in 0..block_count {
            sqlx::query(
                "INSERT INTO ed2k_blocks (task_id, block_index, state, downloaded_bytes, retry_count)
                 VALUES ($1, $2, 0, 0, 0)
                 ON CONFLICT (task_id, block_index) DO NOTHING",
            )
            .bind(task_id)
            .bind(i as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
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
        let sql = if bump_retry {
            "UPDATE ed2k_blocks SET state = $1, downloaded_bytes = $2, retry_count = retry_count + 1
             WHERE task_id = $3 AND block_index = $4"
        } else {
            "UPDATE ed2k_blocks SET state = $1, downloaded_bytes = $2
             WHERE task_id = $3 AND block_index = $4"
        };
        sqlx::query(sql)
            .bind(state)
            .bind(downloaded_bytes)
            .bind(task_id)
            .bind(block_index as i64)
            .execute(&self.pool)
            .await?;
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
        sqlx::query("UPDATE tasks SET downloaded_bytes = 0 WHERE id = $1")
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

    /// Update only the end_byte of a segment (used when a segment is shrunk by a split).
    ///
    /// NOTE: Currently unused — `persist_split` handles both child upsert and
    /// parent shrink atomically. Kept for potential future use.
    #[allow(dead_code)]
    pub async fn update_segment_end_byte(
        &self,
        task_id: &str,
        segment_index: i32,
        end_byte: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE task_segments SET end_byte = $1
             WHERE task_id = $2 AND segment_index = $3",
        )
        .bind(end_byte)
        .bind(task_id)
        .bind(segment_index)
        .execute(&self.pool)
        .await?;
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
        max_concurrent: i32,
        default_save_dir: &str,
        position: i32,
        default_segments: i32,
        default_user_agent: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO queues (id, name, speed_limit_kbps, max_concurrent, default_save_dir, position, default_segments, default_user_agent)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(name)
        .bind(speed_limit_kbps)
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
        max_concurrent: i32,
        default_save_dir: &str,
        default_segments: i32,
        default_user_agent: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE queues SET name = $1, speed_limit_kbps = $2, max_concurrent = $3, \
             default_save_dir = $4, default_segments = $5, default_user_agent = $6 WHERE id = $7",
        )
        .bind(name)
        .bind(speed_limit_kbps)
        .bind(max_concurrent)
        .bind(default_save_dir)
        .bind(default_segments)
        .bind(default_user_agent)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete a queue and move its tasks to the default queue (empty queue_id).
    pub async fn delete_queue(&self, id: &str) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        // Reassign tasks in the deleted queue to the default queue.
        sqlx::query("UPDATE tasks SET queue_id = '' WHERE queue_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM queues WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Load all named queues ordered by position.
    pub async fn load_all_queues(&self) -> Result<Vec<QueueInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT id, name, speed_limit_kbps, max_concurrent, default_save_dir, position, default_segments, default_user_agent
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
                max_concurrent: row.try_get("max_concurrent")?,
                default_save_dir: row.try_get("default_save_dir")?,
                position: row.try_get("position")?,
                default_segments: row.try_get("default_segments")?,
                default_user_agent: row.try_get("default_user_agent")?,
            });
        }
        Ok(queues)
    }

    /// Move a task to a different queue (empty queue_id = default queue).
    pub async fn move_task_to_queue(&self, task_id: &str, queue_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE tasks SET queue_id = $1 WHERE id = $2")
            .bind(queue_id)
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
}

pub struct SegmentInfo {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
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
    /// Returns (Db, dir_path) — caller should remove the dir when done.
    async fn open_test_db() -> (Db, std::path::PathBuf) {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fluxdown_test_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db = Db::open(&dir).await.expect("open test db");
        (db, dir)
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
        )
        .await
        .expect("insert task");
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
        let _ = std::fs::remove_dir_all(dir);
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
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn delete_nonexistent_task_succeeds() {
        let (db, dir) = open_test_db().await;
        // Deleting an ID that was never inserted must not return an error.
        let result = db.delete_task("phantom-id").await;
        assert!(result.is_ok(), "delete of missing task must succeed");
        let _ = std::fs::remove_dir_all(dir);
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
        let _ = std::fs::remove_dir_all(dir);
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
        let _ = std::fs::remove_dir_all(dir);
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
        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
    }

    // -----------------------------------------------------------------------
    // WAL checkpoint
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn wal_checkpoint_succeeds_on_empty_db() {
        let (db, dir) = open_test_db().await;
        let result = db.wal_checkpoint().await;
        assert!(result.is_ok(), "wal_checkpoint must succeed on empty DB");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn wal_checkpoint_succeeds_after_writes() {
        let (db, dir) = open_test_db().await;
        for i in 0..10 {
            insert_task(&db, &format!("cp-{i}")).await;
        }
        let result = db.wal_checkpoint().await;
        assert!(result.is_ok(), "wal_checkpoint must succeed after writes");
        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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
        let _ = std::fs::remove_dir_all(dir);
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
        let _ = std::fs::remove_dir_all(dir);
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
        let _ = std::fs::remove_dir_all(dir);
    }

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
        let _ = std::fs::remove_dir_all(dir);
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
        }
        {
            let db = Db::open(&dir).await.expect("second open");
            let task = db
                .load_task_by_id("persist-1")
                .await
                .expect("load")
                .expect("survives reopen");
            assert_eq!(task.task_id, "persist-1");
        }
        let _ = std::fs::remove_dir_all(dir);
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

        db.delete_task(&id).await.expect("clean");
        db.delete_config("pg_smoke_key").await.expect("clean cfg");
    }

    // -----------------------------------------------------------------------
    // 文件跟踪（FluxDown #11）：update_task_file_missing / file_missing 读回一致性
    // -----------------------------------------------------------------------

    /// 对 completed(status=3) 任务落库 file_missing=true 必须成功（返回
    /// true）且能读回。
    #[tokio::test]
    async fn update_task_file_missing_marks_completed_task() {
        let db = Db::connect("sqlite::memory:").await.expect("connect mem db");
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
        let db = Db::connect("sqlite::memory:").await.expect("connect mem db");
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

    /// 不存在的任务 id：更新必须是空操作而不是报错。
    #[tokio::test]
    async fn update_task_file_missing_noop_for_unknown_task_id() {
        let db = Db::connect("sqlite::memory:").await.expect("connect mem db");

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
        let db = Db::connect("sqlite::memory:").await.expect("connect mem db");
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
        assert!(by_id.file_missing, "load_task_by_id must reflect the update");
        assert!(by_all.file_missing, "load_all_tasks must reflect the same update");
    }

    /// 新插入的普通任务默认没有音频轨：`audio_url` 列默认空串，
    /// load_audio_url 必须归一化为 None，否则恢复逻辑会把单 URL 任务误当轨对任务处理。
    #[tokio::test]
    async fn load_audio_url_returns_none_for_plain_task_without_audio_track() {
        let (db, dir) = open_test_db().await;
        insert_task(&db, "plain1").await;

        let audio_url = db.load_audio_url("plain1").await.expect("load audio_url");
        assert_eq!(audio_url, None, "plain task must not be mistaken for a paired-track task");

        let _ = std::fs::remove_dir_all(dir);
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

        let _ = std::fs::remove_dir_all(dir);
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
        assert_eq!(audio_url, None, "clearing the audio track must fall back to the default state");

        let _ = std::fs::remove_dir_all(dir);
    }
}
