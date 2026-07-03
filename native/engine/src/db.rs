use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use thiserror::Error;

use crate::model::{QueueInfo, TaskInfo};

#[derive(Error, Debug)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("lock poisoned")]
    LockPoisoned,
    #[error("spawn blocking failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(dir: &Path) -> Result<Self, DbError> {
        let db_path = dir.join("flux_down.db");
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;\
             PRAGMA foreign_keys=ON;\
             PRAGMA cache_size=-512;\
             PRAGMA temp_store=MEMORY;\
             PRAGMA mmap_size=0;\
             PRAGMA wal_autocheckpoint=1000;",
        )?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                file_name TEXT NOT NULL,
                save_dir TEXT NOT NULL,
                status INTEGER NOT NULL DEFAULT 0,
                total_bytes INTEGER NOT NULL DEFAULT 0,
                downloaded_bytes INTEGER NOT NULL DEFAULT 0,
                segments INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                error_message TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS task_segments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                segment_index INTEGER NOT NULL,
                start_byte INTEGER NOT NULL,
                end_byte INTEGER NOT NULL,
                downloaded_bytes INTEGER NOT NULL DEFAULT 0,
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
                position INTEGER NOT NULL DEFAULT 0
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
            );",
        )?;

        // --- Schema migrations (safe to re-run) ---

        // 辅助：执行 ALTER TABLE … ADD COLUMN；若 SQLite 报"duplicate column"
        // 则视为列已存在（正常幂等情况），静默忽略；其他错误（SQLITE_FULL、
        // IOERR、corruption 等）向上传播，让 Db::open 返回 Err，避免静默遮盖。
        let add_column = |sql: &str| -> Result<(), DbError> {
            match conn.execute_batch(sql) {
                Ok(_) => Ok(()),
                Err(e) => {
                    if e.to_string().to_lowercase().contains("duplicate column") {
                        Ok(())
                    } else {
                        Err(DbError::Sqlite(e))
                    }
                }
            }
        };

        // Phase 2: per-task proxy URL column
        add_column("ALTER TABLE tasks ADD COLUMN proxy_url TEXT NOT NULL DEFAULT '';")?;

        // Phase 3: named queue assignment column
        add_column("ALTER TABLE tasks ADD COLUMN queue_id TEXT NOT NULL DEFAULT '';")?;

        // Phase 4: per-queue default segment count
        add_column("ALTER TABLE queues ADD COLUMN default_segments INTEGER NOT NULL DEFAULT 0;")?;

        // Phase 5: per-task checksum for integrity verification
        add_column("ALTER TABLE tasks ADD COLUMN checksum TEXT NOT NULL DEFAULT '';")?;

        // Phase 6: per-queue default user-agent
        add_column("ALTER TABLE queues ADD COLUMN default_user_agent TEXT NOT NULL DEFAULT '';")?;

        // Phase 7: BT selected file indices (comma-separated, empty = all files)
        add_column("ALTER TABLE tasks ADD COLUMN bt_selected_files TEXT NOT NULL DEFAULT '';")?;

        // Phase 8: BT custom name — user-specified rename target, stored
        // separately so Phase 1/3 engine callbacks never overwrite it.
        add_column("ALTER TABLE tasks ADD COLUMN bt_custom_name TEXT NOT NULL DEFAULT '';")?;

        // Phase 9: resume 一致性校验所需的【原始】文件版本标识。首次下载（非续传）
        // 时记录 probe 看到的 ETag / Last-Modified；续传时用【这里存的原值】构造
        // If-Range，从而检出"两次会话之间服务器换了文件（即便长度相同）"——避免
        // 把旧前缀 + 新尾部静默拼接（BUG-HTTP-SINGLE-RESUME-SPLICE）。
        add_column("ALTER TABLE tasks ADD COLUMN orig_etag TEXT NOT NULL DEFAULT '';")?;
        add_column("ALTER TABLE tasks ADD COLUMN orig_last_modified TEXT NOT NULL DEFAULT '';")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
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
        let conn = self.conn.clone();
        let id = id.to_owned();
        let url = url.to_owned();
        let file_name = file_name.to_owned();
        let save_dir = save_dir.to_owned();
        let proxy_url = proxy_url.to_owned();
        let queue_id = queue_id.to_owned();
        let checksum = checksum.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let now = chrono_now();
            conn.execute(
                "INSERT INTO tasks (id, url, file_name, save_dir, status, segments, total_bytes, created_at, proxy_url, queue_id, checksum)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![id, url, file_name, save_dir, segments, total_bytes, now, proxy_url, queue_id, checksum],
            )?;
            Ok(())
        })
        .await?
    }

    pub async fn update_task_progress(
        &self,
        id: &str,
        downloaded_bytes: i64,
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET downloaded_bytes = ?1 WHERE id = ?2",
                params![downloaded_bytes, id],
            )?;
            Ok(())
        })
        .await?
    }

    /// 单调进度写入：`downloaded_bytes` 只增不减（SQL 用 `MAX` 钳制）。
    ///
    /// 与 [`update_task_progress`](Self::update_task_progress) 的唯一区别是 SQL
    /// 用 `MAX(downloaded_bytes, ?1)` 而非直接赋值，因此 DB 中的进度只会前进、
    /// 永不回退。
    ///
    /// **动机（F009）**：`progress_reporter` 中 status=1 的进度写入是
    /// fire-and-forget（spawn 后不 await），与 status=3 完成时 awaited 的最终
    /// 写入竞争同一把 `Arc<Mutex<Connection>>` 锁，落库先后顺序不确定。一个先
    /// 发起、携带中途较小 `downloaded_bytes` 的后台写入可能在完成写入之后才抢
    /// 到锁，把 DB 里的 100% 覆盖回中途值，导致重启后进度倒退。单调写入消除了
    /// 这一顺序依赖。
    ///
    /// **不可替代 `update_task_progress`**：downloader / ftp_downloader 在切多段
    /// →单流重下、`File::create` 从头开始时会主动传入 `0` 复位进度；若把那条
    /// 路径也改成 `MAX`，复位会退化成 no-op、残留陈旧高值。因此这里必须是独立
    /// 的新方法，仅供 `progress_reporter` 这类“只前进”的场景使用。
    pub async fn update_task_progress_monotonic(
        &self,
        id: &str,
        downloaded_bytes: i64,
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET downloaded_bytes = MAX(downloaded_bytes, ?1) WHERE id = ?2",
                params![downloaded_bytes, id],
            )?;
            Ok(())
        })
        .await?
    }

    pub async fn update_task_status(
        &self,
        id: &str,
        status: i32,
        error_message: &str,
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        let error_message = error_message.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET status = ?1, error_message = ?2 WHERE id = ?3",
                params![status, error_message, id],
            )?;
            Ok(())
        })
        .await?
    }

    pub async fn update_task_file_info(
        &self,
        id: &str,
        file_name: &str,
        total_bytes: i64,
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        let file_name = file_name.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET file_name = ?1, total_bytes = ?2 WHERE id = ?3",
                params![file_name, total_bytes, id],
            )?;
            Ok(())
        })
        .await?
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
        let conn = self.conn.clone();
        let id = id.to_owned();
        let file_name = file_name.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;

            // Read the currently stored total_bytes.
            let stored_total: i64 = match conn.query_row(
                "SELECT total_bytes FROM tasks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            ) {
                Ok(v) => v,
                Err(rusqlite::Error::QueryReturnedNoRows) => 0,
                Err(e) => return Err(DbError::Sqlite(e)),
            };

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
                conn.execute(
                    "UPDATE tasks SET file_name = ?1, total_bytes = ?2 WHERE id = ?3",
                    params![file_name, probed_total_bytes, id],
                )?;
                probed_total_bytes
            } else {
                // CDN drift within tolerance — only update file_name; preserve
                // existing total_bytes so that segment end_byte boundaries stay
                // consistent with what the coordinator will use.
                conn.execute(
                    "UPDATE tasks SET file_name = ?1 WHERE id = ?2",
                    params![file_name, id],
                )?;
                stored_total
            };

            Ok((effective_total, size_changed))
        })
        .await?
    }

    /// 更新任务文件名（仅当任务文件名为空时，防止覆盖用户自定义名称）
    pub async fn update_task_file_name(
        &self,
        task_id: &str,
        file_name: &str,
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = task_id.to_owned();
        let name = file_name.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET file_name = ?1 WHERE id = ?2 AND (file_name = '' OR file_name IS NULL)",
                params![name, id],
            )?;
            Ok(())
        })
        .await?
    }

    /// 启动时将所有 downloading(1)、pending(0)、preparing(5) 的任务矫正为 paused(2)
    /// 因为重启后没有活跃的下载线程，这些任务实际上处于暂停状态
    pub async fn reset_incomplete_tasks_to_paused(&self) -> Result<u64, DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let affected =
                conn.execute("UPDATE tasks SET status = 2 WHERE status IN (0, 1, 5)", [])?;
            Ok(affected as u64)
        })
        .await?
    }

    pub async fn load_all_tasks(&self) -> Result<Vec<TaskInfo>, DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let mut stmt = conn.prepare(
                "SELECT id, url, file_name, save_dir, status, downloaded_bytes, total_bytes, error_message, created_at, proxy_url, queue_id, checksum
                 FROM tasks ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(TaskInfo {
                    task_id: row.get(0)?,
                    url: row.get(1)?,
                    file_name: row.get(2)?,
                    save_dir: row.get(3)?,
                    status: row.get(4)?,
                    downloaded_bytes: row.get(5)?,
                    total_bytes: row.get(6)?,
                    error_message: row.get(7)?,
                    created_at: row.get(8)?,
                    proxy_url: row.get::<_, String>(9).unwrap_or_default(),
                    queue_id: row.get::<_, String>(10).unwrap_or_default(),
                    checksum: row.get::<_, String>(11).unwrap_or_default(),
                })
            })?;
            let mut tasks = Vec::new();
            for row in rows {
                tasks.push(row?);
            }
            Ok(tasks)
        })
        .await?
    }

    pub async fn load_task_by_id(&self, id: &str) -> Result<Option<TaskInfo>, DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            match conn.query_row(
                "SELECT id, url, file_name, save_dir, status, downloaded_bytes, total_bytes, error_message, created_at, proxy_url, queue_id, checksum
                 FROM tasks WHERE id = ?1",
                params![id],
                |row| {
                    Ok(TaskInfo {
                        task_id: row.get(0)?,
                        url: row.get(1)?,
                        file_name: row.get(2)?,
                        save_dir: row.get(3)?,
                        status: row.get(4)?,
                        downloaded_bytes: row.get(5)?,
                        total_bytes: row.get(6)?,
                        error_message: row.get(7)?,
                        created_at: row.get(8)?,
                        proxy_url: row.get::<_, String>(9).unwrap_or_default(),
                        queue_id: row.get::<_, String>(10).unwrap_or_default(),
                        checksum: row.get::<_, String>(11).unwrap_or_default(),
                    })
                },
            ) {
                Ok(task) => Ok(Some(task)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(DbError::Sqlite(e)),
            }
        })
        .await?
    }

    /// Batch-load multiple tasks by ID in a single `spawn_blocking` call.
    /// Uses chunked IN clauses (same pattern as `delete_tasks_batch`).
    pub async fn load_tasks_by_ids(&self, ids: &[String]) -> Result<Vec<TaskInfo>, DbError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.clone();
        let ids = ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let mut results = Vec::with_capacity(ids.len());
            const CHUNK: usize = 500;
            for chunk in ids.chunks(CHUNK) {
                let placeholders: String = (1..=chunk.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let params: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
                let sql = format!(
                    "SELECT id, url, file_name, save_dir, status, downloaded_bytes, total_bytes, error_message, created_at, proxy_url, queue_id, checksum
                     FROM tasks WHERE id IN ({})",
                    placeholders
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                    Ok(TaskInfo {
                        task_id: row.get(0)?,
                        url: row.get(1)?,
                        file_name: row.get(2)?,
                        save_dir: row.get(3)?,
                        status: row.get(4)?,
                        downloaded_bytes: row.get(5)?,
                        total_bytes: row.get(6)?,
                        error_message: row.get(7)?,
                        created_at: row.get(8)?,
                        proxy_url: row.get::<_, String>(9).unwrap_or_default(),
                        queue_id: row.get::<_, String>(10).unwrap_or_default(),
                        checksum: row.get::<_, String>(11).unwrap_or_default(),
                    })
                })?;
                for row in rows {
                    results.push(row?);
                }
            }
            Ok(results)
        })
        .await?
    }

    pub async fn delete_task(&self, id: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM task_segments WHERE task_id = ?1", params![id])?;
            tx.execute("DELETE FROM torrent_files WHERE task_id = ?1", params![id])?;
            // 任务级 config 行(完成幂等哨兵 bt_completion_top_<id>、HLS 断点
            // hls_resume_<id>)随任务一并清理,防孤儿行累积。
            tx.execute(
                "DELETE FROM config WHERE key IN (?1, ?2)",
                params![
                    format!("bt_completion_top_{}", id),
                    format!("hls_resume_{}", id)
                ],
            )?;
            tx.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    /// Batch-delete multiple tasks in a single transaction.
    /// Uses chunked IN clauses to respect SQLite's 999 variable limit.
    pub async fn delete_tasks_batch(&self, ids: &[String]) -> Result<(), DbError> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.clone();
        let ids = ids.to_vec();
        tokio::task::spawn_blocking(move || {
            // mut 是 rusqlite::Connection::transaction_with_behavior 的必要条件。
            let mut conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            // RAII 事务：任何 `?` 提前返回时 Drop 自动 ROLLBACK，不会泄漏事务。
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // SQLite has a max variable limit of 999; chunk to stay safe.
            const CHUNK: usize = 500;
            for chunk in ids.chunks(CHUNK) {
                let placeholders: String = (1..=chunk.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let params: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

                let sql = format!(
                    "DELETE FROM task_segments WHERE task_id IN ({})",
                    placeholders
                );
                tx.execute(&sql, params.as_slice())?;

                let sql = format!(
                    "DELETE FROM torrent_files WHERE task_id IN ({})",
                    placeholders
                );
                tx.execute(&sql, params.as_slice())?;

                // 任务级 config 行(哨兵/HLS 断点)随任务清理,防孤儿行累积。
                for id in chunk {
                    tx.execute(
                        "DELETE FROM config WHERE key IN (?1, ?2)",
                        params![
                            format!("bt_completion_top_{}", id),
                            format!("hls_resume_{}", id)
                        ],
                    )?;
                }

                let sql = format!("DELETE FROM tasks WHERE id IN ({})", placeholders);
                tx.execute(&sql, params.as_slice())?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?
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
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        let file_bytes = file_bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "INSERT OR REPLACE INTO torrent_files (task_id, file_bytes)
                 VALUES (?1, ?2)",
                params![task_id, file_bytes],
            )?;
            Ok(())
        })
        .await?
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
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        let value = if is_all {
            "all".to_owned()
        } else {
            indices
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET bt_selected_files = ?1 WHERE id = ?2",
                params![value, task_id],
            )?;
            Ok(())
        })
        .await?
    }

    /// Load the persisted BT file selection for a task.
    ///
    /// Returns:
    ///   `None`           — never confirmed; caller should show the dialog.
    ///   `Some([])`       — user confirmed all files; skip dialog & update_only_files.
    ///   `Some([0,2,5])`  — user selected a subset; skip dialog, apply update_only_files.
    pub async fn load_bt_selected_files(&self, task_id: &str) -> Result<Option<Vec<i32>>, DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let value: String = match conn.query_row(
                "SELECT bt_selected_files FROM tasks WHERE id = ?1",
                params![task_id],
                |row| row.get(0),
            ) {
                Ok(v) => v,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(e) => return Err(DbError::Sqlite(e)),
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
        })
        .await?
    }

    /// Persist the user-specified BT custom name (rename target).
    /// This column is independent of `file_name` and is never overwritten
    /// by the download engine's Phase 1 (dn=) or Phase 3 (metadata) updates.
    pub async fn save_bt_custom_name(&self, id: &str, name: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET bt_custom_name = ?1 WHERE id = ?2",
                params![name, id],
            )?;
            Ok(())
        })
        .await?
    }

    /// Load the user-specified BT custom name.  Returns empty string when
    /// the user did not specify a custom name.
    pub async fn load_bt_custom_name(&self, id: &str) -> Result<String, DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let name: String = conn
                .query_row(
                    "SELECT bt_custom_name FROM tasks WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .unwrap_or_default();
            Ok(name)
        })
        .await?
    }

    /// Load raw .torrent file bytes for a task (used when resuming).
    pub async fn load_torrent_file_bytes(&self, task_id: &str) -> Result<Option<Vec<u8>>, DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            match conn.query_row(
                "SELECT file_bytes FROM torrent_files WHERE task_id = ?1",
                params![task_id],
                |row| row.get(0),
            ) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(DbError::Sqlite(e)),
            }
        })
        .await?
    }

    pub async fn insert_segments(
        &self,
        task_id: &str,
        segments: &[(i32, i64, i64)],
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        let segments = segments.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let tx = conn.transaction()?;
            for (index, start, end) in &segments {
                tx.execute(
                    "INSERT INTO task_segments (task_id, segment_index, start_byte, end_byte, downloaded_bytes)
                     VALUES (?1, ?2, ?3, ?4, 0)",
                    params![task_id, index, start, end],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    pub async fn load_segments(&self, task_id: &str) -> Result<Vec<SegmentInfo>, DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let mut stmt = conn.prepare(
                "SELECT segment_index, start_byte, end_byte, downloaded_bytes
                 FROM task_segments WHERE task_id = ?1 ORDER BY segment_index",
            )?;
            let rows = stmt.query_map(params![task_id], |row| {
                Ok(SegmentInfo {
                    index: row.get(0)?,
                    start_byte: row.get(1)?,
                    end_byte: row.get(2)?,
                    downloaded_bytes: row.get(3)?,
                })
            })?;
            let mut segs = Vec::new();
            for row in rows {
                segs.push(row?);
            }
            Ok(segs)
        })
        .await?
    }

    pub async fn update_segment_progress(
        &self,
        task_id: &str,
        segment_index: i32,
        downloaded_bytes: i64,
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE task_segments SET downloaded_bytes = ?1
                 WHERE task_id = ?2 AND segment_index = ?3",
                params![downloaded_bytes, task_id, segment_index],
            )?;
            Ok(())
        })
        .await?
    }

    /// Flush final downloaded_bytes for all segments in a single transaction.
    /// Used by the coordinator after download completes to ensure DB reflects
    /// the authoritative in-memory state (capped to segment size, no overshoot).
    pub async fn flush_segments_progress(
        &self,
        task_id: &str,
        updates: Vec<(i32, i64)>, // (segment_index, downloaded_bytes)
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let tx = conn.transaction()?;
            for (seg_idx, dl_bytes) in &updates {
                tx.execute(
                    "UPDATE task_segments SET downloaded_bytes = ?1
                     WHERE task_id = ?2 AND segment_index = ?3",
                    params![dl_bytes, task_id, seg_idx],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    // -----------------------------------------------------------------------
    // Config KV store
    // -----------------------------------------------------------------------

    /// Get a single config value by key.
    pub async fn get_config(&self, key: &str) -> Result<Option<String>, DbError> {
        let conn = self.conn.clone();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            match conn.query_row(
                "SELECT value FROM config WHERE key = ?1",
                params![key],
                |row| row.get(0),
            ) {
                Ok(v) => Ok(Some(v)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(DbError::Sqlite(e)),
            }
        })
        .await?
    }

    /// Set a config value (insert or update).
    pub async fn set_config(&self, key: &str, value: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let key = key.to_owned();
        let value = value.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "INSERT INTO config (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
        .await?
    }

    /// Delete a config entry by key.
    pub async fn delete_config(&self, key: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute("DELETE FROM config WHERE key = ?1", params![key])?;
            Ok(())
        })
        .await?
    }

    /// Load all config entries as a HashMap.
    pub async fn get_all_config(&self) -> Result<HashMap<String, String>, DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let mut stmt = conn.prepare("SELECT key, value FROM config")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut map = HashMap::new();
            for row in rows {
                let (k, v) = row?;
                map.insert(k, v);
            }
            Ok(map)
        })
        .await?
    }

    /// Insert default config values (only if not already set).
    pub async fn init_default_config(&self, default_save_dir: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let default_save_dir = default_save_dir.to_owned();
        let default_sub_urls = crate::tracker_subscription::default_subscription_urls();
        let default_ed2k_met_urls = crate::ed2k::server_subscription::default_server_met_urls();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let defaults: &[(&str, &str)] = &[
                ("default_save_dir", &default_save_dir),
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
                ("ed2k_nodes_dat_url", "https://upd.emule-security.org/nodes.dat"),
                ("ed2k_nodes_dat_cache", ""),
                ("ed2k_nodes_dat_updated_at", "0"),
            ];
            for (key, value) in defaults {
                conn.execute(
                    "INSERT OR IGNORE INTO config (key, value) VALUES (?1, ?2)",
                    params![key, value],
                )?;
            }
            Ok(())
        })
        .await?
    }

    /// Delete all segment rows for a task (used when total_bytes changes on resume).
    pub async fn delete_segments(&self, task_id: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM task_segments WHERE task_id = ?1",
                params![task_id],
            )?;
            // Also reset downloaded_bytes in the tasks table
            tx.execute(
                "UPDATE tasks SET downloaded_bytes = 0 WHERE id = ?1",
                params![task_id],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    // -----------------------------------------------------------------------
    // ED2K blocks / hashset
    // -----------------------------------------------------------------------

    /// Initialise all block rows (state=0 missing) for an ed2k task.
    /// Idempotent per (task_id, block_index) via INSERT OR IGNORE.
    pub async fn init_ed2k_blocks(&self, task_id: &str, block_count: u64) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let tx = conn.transaction()?;
            for i in 0..block_count {
                tx.execute(
                    "INSERT OR IGNORE INTO ed2k_blocks (task_id, block_index, state, downloaded_bytes, retry_count)
                     VALUES (?1, ?2, 0, 0, 0)",
                    params![task_id, i as i64],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    /// Load all block rows for an ed2k task, ordered by block_index.
    /// Returns `(block_index, state, downloaded_bytes, retry_count)`.
    pub async fn load_ed2k_blocks(
        &self,
        task_id: &str,
    ) -> Result<Vec<(u64, i64, i64, i64)>, DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let mut stmt = conn.prepare(
                "SELECT block_index, state, downloaded_bytes, retry_count
                 FROM ed2k_blocks WHERE task_id = ?1 ORDER BY block_index",
            )?;
            let rows = stmt.query_map(params![task_id], |row| {
                let idx: i64 = row.get(0)?;
                Ok((idx as u64, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .await?
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
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            if bump_retry {
                conn.execute(
                    "UPDATE ed2k_blocks SET state = ?1, downloaded_bytes = ?2, retry_count = retry_count + 1
                     WHERE task_id = ?3 AND block_index = ?4",
                    params![state, downloaded_bytes, task_id, block_index as i64],
                )?;
            } else {
                conn.execute(
                    "UPDATE ed2k_blocks SET state = ?1, downloaded_bytes = ?2
                     WHERE task_id = ?3 AND block_index = ?4",
                    params![state, downloaded_bytes, task_id, block_index as i64],
                )?;
            }
            Ok(())
        })
        .await?
    }

    /// Persist the verified hashset blob (concatenated 16B * part_count block
    /// hashes, network order, no phantom-tail append). Idempotent (REPLACE).
    pub async fn save_ed2k_hashset(&self, task_id: &str, hashes: &[u8]) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        let hashes = hashes.to_vec();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "INSERT OR REPLACE INTO ed2k_hashset (task_id, hashes) VALUES (?1, ?2)",
                params![task_id, hashes],
            )?;
            Ok(())
        })
        .await?
    }

    /// Load the persisted hashset blob, if any.
    pub async fn load_ed2k_hashset(&self, task_id: &str) -> Result<Option<Vec<u8>>, DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            match conn.query_row(
                "SELECT hashes FROM ed2k_hashset WHERE task_id = ?1",
                params![task_id],
                |row| row.get(0),
            ) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(DbError::Sqlite(e)),
            }
        })
        .await?
    }

    /// Reset all segment progress for a task back to zero.
    pub async fn reset_segments_progress(&self, task_id: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE task_segments SET downloaded_bytes = 0 WHERE task_id = ?1",
                params![task_id],
            )?;
            conn.execute(
                "UPDATE tasks SET downloaded_bytes = 0 WHERE id = ?1",
                params![task_id],
            )?;
            Ok(())
        })
        .await?
    }

    /// Update the segment count for a task (e.g. after dynamic calculation).
    pub async fn update_task_segments(&self, id: &str, segments: i32) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET segments = ?1 WHERE id = ?2",
                params![segments, id],
            )?;
            Ok(())
        })
        .await?
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
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            // Atomic DELETE + INSERT inside a transaction.
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM task_segments WHERE task_id = ?1 AND segment_index = ?2",
                params![task_id, segment_index],
            )?;
            tx.execute(
                "INSERT INTO task_segments (task_id, segment_index, start_byte, end_byte, downloaded_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![task_id, segment_index, start_byte, end_byte, downloaded_bytes],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?
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
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE task_segments SET end_byte = ?1
                 WHERE task_id = ?2 AND segment_index = ?3",
                params![end_byte, task_id, segment_index],
            )?;
            Ok(())
        })
        .await?
    }

    /// Atomically persist a segment split: upsert the new child segment **and**
    /// shrink the parent's `end_byte` in a single SQLite transaction.
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
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let tx = conn.transaction()?;
            // 1. Upsert child segment (DELETE + INSERT).
            tx.execute(
                "DELETE FROM task_segments WHERE task_id = ?1 AND segment_index = ?2",
                params![task_id, child_index],
            )?;
            tx.execute(
                "INSERT INTO task_segments (task_id, segment_index, start_byte, end_byte, downloaded_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![task_id, child_index, child_start, child_end, child_downloaded],
            )?;
            // 2. Shrink parent's end_byte.
            tx.execute(
                "UPDATE task_segments SET end_byte = ?1
                 WHERE task_id = ?2 AND segment_index = ?3",
                params![parent_new_end, task_id, parent_index],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    /// Update the total_bytes for a task.
    pub async fn update_task_total_bytes(&self, id: &str, total_bytes: i64) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET total_bytes = ?1 WHERE id = ?2",
                params![total_bytes, id],
            )?;
            Ok(())
        })
        .await?
    }

    /// 记录首次下载时 probe 看到的【原始】版本标识（ETag / Last-Modified）。
    /// 仅在非续传的首次下载阶段写入，作为后续续传 If-Range 一致性校验的基准。
    pub async fn set_task_validator(
        &self,
        id: &str,
        etag: &str,
        last_modified: &str,
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        let etag = etag.to_owned();
        let last_modified = last_modified.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET orig_etag = ?1, orig_last_modified = ?2 WHERE id = ?3",
                params![etag, last_modified, id],
            )?;
            Ok(())
        })
        .await?
    }

    /// 读取首次下载记录的原始版本标识，返回 `(orig_etag, orig_last_modified)`。
    /// 旧任务（升级前创建、列为默认空）或服务器未提供时返回 `("", "")`。
    pub async fn get_task_validator(&self, id: &str) -> Result<(String, String), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            match conn.query_row(
                "SELECT orig_etag, orig_last_modified FROM tasks WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0).unwrap_or_default(),
                        row.get::<_, String>(1).unwrap_or_default(),
                    ))
                },
            ) {
                Ok(v) => Ok(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok((String::new(), String::new())),
                Err(e) => Err(DbError::Sqlite(e)),
            }
        })
        .await?
    }

    /// Manually run a WAL checkpoint to merge the write-ahead log back into the
    /// main database file.  Called when all downloads are idle (no active tasks)
    /// so the WAL doesn't grow unbounded and no background autocheckpoint causes
    /// unexpected disk I/O.
    pub async fn wal_checkpoint(&self) -> Result<(), DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .await?
    }

    /// Get the configured segment count for a task from the tasks table.
    pub async fn get_task_segments(&self, id: &str) -> Result<i32, DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let seg: i32 = conn.query_row(
                "SELECT segments FROM tasks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )?;
            Ok(seg)
        })
        .await?
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
        let conn = self.conn.clone();
        let id = id.to_owned();
        let name = name.to_owned();
        let default_save_dir = default_save_dir.to_owned();
        let default_user_agent = default_user_agent.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "INSERT INTO queues (id, name, speed_limit_kbps, max_concurrent, default_save_dir, position, default_segments, default_user_agent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, name, speed_limit_kbps, max_concurrent, default_save_dir, position, default_segments, default_user_agent],
            )?;
            Ok(())
        })
        .await?
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
        let conn = self.conn.clone();
        let id = id.to_owned();
        let name = name.to_owned();
        let default_save_dir = default_save_dir.to_owned();
        let default_user_agent = default_user_agent.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE queues SET name = ?1, speed_limit_kbps = ?2, max_concurrent = ?3, \
                 default_save_dir = ?4, default_segments = ?5, default_user_agent = ?6 WHERE id = ?7",
                params![name, speed_limit_kbps, max_concurrent, default_save_dir, default_segments, default_user_agent, id],
            )?;
            Ok(())
        })
        .await?
    }

    /// Delete a queue and move its tasks to the default queue (empty queue_id).
    pub async fn delete_queue(&self, id: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let tx = conn.transaction()?;
            // Reassign tasks in the deleted queue to the default queue.
            tx.execute(
                "UPDATE tasks SET queue_id = '' WHERE queue_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM queues WHERE id = ?1", params![id])?;
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    /// Load all named queues ordered by position.
    pub async fn load_all_queues(&self) -> Result<Vec<QueueInfo>, DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let mut stmt = conn.prepare(
                "SELECT id, name, speed_limit_kbps, max_concurrent, default_save_dir, position, default_segments, default_user_agent
                 FROM queues ORDER BY position ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(QueueInfo {
                    queue_id: row.get(0)?,
                    name: row.get(1)?,
                    speed_limit_kbps: row.get(2)?,
                    max_concurrent: row.get(3)?,
                    default_save_dir: row.get(4)?,
                    position: row.get(5)?,
                    default_segments: row.get(6)?,
                    default_user_agent: row.get(7)?,
                })
            })?;
            let mut queues = Vec::new();
            for row in rows {
                queues.push(row?);
            }
            Ok(queues)
        })
        .await?
    }

    /// Move a task to a different queue (empty queue_id = default queue).
    pub async fn move_task_to_queue(&self, task_id: &str, queue_id: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        let queue_id = queue_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "UPDATE tasks SET queue_id = ?1 WHERE id = ?2",
                params![queue_id, task_id],
            )?;
            Ok(())
        })
        .await?
    }

    /// Count the number of rows currently in the queues table.
    pub async fn queue_count(&self) -> Result<i32, DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let count: i32 = conn.query_row("SELECT COUNT(*) FROM queues", [], |row| row.get(0))?;
            Ok(count)
        })
        .await?
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
    fn open_test_db() -> (Db, std::path::PathBuf) {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fluxdown_test_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db = Db::open(&dir).expect("open test db");
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
        let (db, dir) = open_test_db();
        insert_task(&db, "t1").await;

        db.delete_task("t1").await.expect("delete task");

        let result = db.load_task_by_id("t1").await.expect("load after delete");
        assert!(result.is_none(), "task must be absent after delete");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn delete_task_not_present_in_load_all() {
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
        // Deleting an ID that was never inserted must not return an error.
        let result = db.delete_task("phantom-id").await;
        assert!(result.is_ok(), "delete of missing task must succeed");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn delete_same_task_twice_is_idempotent() {
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
        insert_task(&db, "seg-task").await;

        // Insert a segment row directly via the connection.
        {
            let conn = db.conn.clone();
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock().expect("lock");
                conn.execute(
                    "INSERT INTO task_segments (task_id, segment_index, start_byte, end_byte)
                     VALUES (?1, 0, 0, 1024)",
                    rusqlite::params!["seg-task"],
                )
                .expect("insert segment");
            })
            .await
            .expect("spawn_blocking");
        }

        db.delete_task("seg-task").await.expect("delete");

        // Verify no orphan segment rows.
        let conn = db.conn.clone();
        let count: i64 = tokio::task::spawn_blocking(move || {
            let conn = conn.lock().expect("lock");
            conn.query_row(
                "SELECT COUNT(*) FROM task_segments WHERE task_id = 'seg-task'",
                [],
                |row| row.get(0),
            )
            .expect("query count")
        })
        .await
        .expect("spawn_blocking");

        assert_eq!(count, 0, "task_segments must be empty after task delete");
        let _ = std::fs::remove_dir_all(dir);
    }

    // -----------------------------------------------------------------------
    // Performance benchmark: expose the N×WAL-checkpoint bottleneck
    //
    // Run with:  cargo test -p hub -- --nocapture delete_benchmark
    // -----------------------------------------------------------------------

    /// Insert N completed tasks (no active handles) and delete them one by one.
    /// Prints elapsed time so the sequential-WAL-checkpoint cost is visible.
    ///
    /// This test documents the known bottleneck: `delete_task` calls
    /// `db.delete_task()` which spawns a blocking thread per call.
    /// For N = 500 the elapsed time exposes the per-task overhead; at
    /// N = 5 000 the wall time becomes user-visible (several seconds).
    #[tokio::test]
    async fn delete_benchmark_sequential_500_tasks() {
        const N: usize = 500;
        let (db, dir) = open_test_db();

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
             check for WAL-checkpoint or spawn_blocking overhead"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    // -----------------------------------------------------------------------
    // WAL checkpoint
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn wal_checkpoint_succeeds_on_empty_db() {
        let (db, dir) = open_test_db();
        let result = db.wal_checkpoint().await;
        assert!(result.is_ok(), "wal_checkpoint must succeed on empty DB");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn wal_checkpoint_succeeds_after_writes() {
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
        let (db, dir) = open_test_db();
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
}
