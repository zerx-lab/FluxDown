use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use thiserror::Error;

use crate::signals::TaskInfo;

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
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
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
            );",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn insert_task(
        &self,
        id: &str,
        url: &str,
        file_name: &str,
        save_dir: &str,
        segments: i32,
        total_bytes: i64,
    ) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        let url = url.to_owned();
        let file_name = file_name.to_owned();
        let save_dir = save_dir.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let now = chrono_now();
            conn.execute(
                "INSERT INTO tasks (id, url, file_name, save_dir, status, segments, total_bytes, created_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)",
                params![id, url, file_name, save_dir, segments, total_bytes, now],
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

    /// 启动时将所有 downloading(1)、pending(0)、preparing(5) 的任务矫正为 paused(2)
    /// 因为重启后没有活跃的下载线程，这些任务实际上处于暂停状态
    pub async fn reset_incomplete_tasks_to_paused(&self) -> Result<u64, DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let affected = conn.execute(
                "UPDATE tasks SET status = 2 WHERE status IN (0, 1, 5)",
                [],
            )?;
            Ok(affected as u64)
        })
        .await?
    }

    pub async fn load_all_tasks(&self) -> Result<Vec<TaskInfo>, DbError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let mut stmt = conn.prepare(
                "SELECT id, url, file_name, save_dir, status, downloaded_bytes, total_bytes, error_message, created_at
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
                "SELECT id, url, file_name, save_dir, status, downloaded_bytes, total_bytes, error_message, created_at
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

    pub async fn delete_task(&self, id: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute("DELETE FROM task_segments WHERE task_id = ?1", params![id])?;
            conn.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
            Ok(())
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

    pub async fn load_segments(
        &self,
        task_id: &str,
    ) -> Result<Vec<SegmentInfo>, DbError> {
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

    // -----------------------------------------------------------------------
    // Config KV store
    // -----------------------------------------------------------------------

    /// Get a single config value by key.
    #[allow(dead_code)]
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
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute_batch(&format!(
                "INSERT OR IGNORE INTO config (key, value) VALUES
                    ('default_save_dir', '{}'),
                    ('default_segments', '0'),
                    ('max_concurrent_tasks', '5'),
                    ('speed_limit_bytes', '0'),
                    ('auto_resume_on_start', 'false'),
                    ('close_to_tray', 'true'),
                    ('auto_startup', 'false'),
                    ('auto_check_update', 'true');",
                default_save_dir.replace('\'', "''")
            ))?;
            Ok(())
        })
        .await?
    }

    /// Delete all segment rows for a task (used when total_bytes changes on resume).
    pub async fn delete_segments(&self, task_id: &str) -> Result<(), DbError> {
        let conn = self.conn.clone();
        let task_id = task_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| DbError::LockPoisoned)?;
            conn.execute(
                "DELETE FROM task_segments WHERE task_id = ?1",
                params![task_id],
            )?;
            // Also reset downloaded_bytes in the tasks table
            conn.execute(
                "UPDATE tasks SET downloaded_bytes = 0 WHERE id = ?1",
                params![task_id],
            )?;
            Ok(())
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
