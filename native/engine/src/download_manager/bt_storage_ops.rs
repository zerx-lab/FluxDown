//! BT 任务的存储操作：重命名文件（`daemon.task.renameBtFile`）、强制重新校验
//! （`daemon.task.recheck`）与移动数据（`daemon.task.moveStorage`）。
//!
//! 会话里的任务交给 librqbit 当场处理（改名 / 重新校验）；不在会话里的任务直接改 staging
//! 里的文件，或以暂停态加入会话做一次校验。移动一律在会话之外进行：先把任务摘出会话
//! （释放文件句柄，保留快速恢复位图），再按 [`crate::bt_move`] 挪动保存目录下属于任务的
//! 顶层条目，落库新位置后恢复原来的运行状态。

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use super::{
    DownloadManager, SEEDING_STATUS_ACTIVE, SEEDING_STATUS_QUEUED, is_bt_url, is_magnet,
    is_safe_file_name,
};
use crate::bt_downloader::{self, BtHandle, SharedBtSession, TorrentSource};
use crate::bt_move::{self, MoveError, MoveJournal};
use crate::bt_seeding::SeedingStopReason;
use crate::bt_task_files::{
    BtTaskOpError, format_rename_table, normalize_rename, parse_rename_table, rename_staged_file,
    with_renames,
};
use crate::logger::{log_info, log_warn};
use crate::model::TaskInfo;
use crate::task_jobs::TaskJobGuard;

/// 移动期间做种任务显示的说明。
const MOVING_MESSAGE: &str = "moving data";

/// [`DownloadManager::begin_bt_move`] 的结果。
pub enum BtMoveStart {
    /// 目标就是当前保存目录：无事可做。
    Unchanged,
    /// 任务刚被暂停、下载循环还在退出：稍后以同样的参数（`resume_after` 原样带回）再调一次。
    Wait { resume_after: bool },
    /// 已开始：在 actor 之外 `await` 它，结果交回 [`DownloadManager::finish_bt_move`]。
    Started(BtMoveJob),
}

/// 在 actor 之外执行的搬移作业。
pub type BtMoveJob = Pin<Box<dyn Future<Output = BtMoveOutcome> + Send>>;

/// 搬移作业的结果，交回 actor 落库并恢复任务状态。
pub struct BtMoveOutcome {
    task_id: String,
    journal: MoveJournal,
    resume_after: bool,
    result: Result<usize, MoveError>,
    guard: TaskJobGuard,
}

impl DownloadManager {
    /// 任务的数据是否正在搬移（搬移期间拒绝恢复 / 删除 / 改文件等操作）。
    pub(super) fn bt_move_running(&self, task_id: &str) -> bool {
        self.bt_moves.is_running(task_id)
    }

    /// 读出可做存储操作的 BT 任务：不存在 / 不是 BT 任务为 `NotFound`，数据搬移中为
    /// `StateConflict`。
    async fn load_bt_task_for_op(&self, task_id: &str) -> Result<TaskInfo, BtTaskOpError> {
        let task = self
            .db
            .load_task_by_id(task_id)
            .await?
            .ok_or(BtTaskOpError::NotFound)?;
        if !is_bt_url(&task.url) {
            return Err(BtTaskOpError::NotFound);
        }
        if self.bt_move_running(task_id) {
            return Err(BtTaskOpError::StateConflict(
                "the task's data is being moved".to_owned(),
            ));
        }
        Ok(task)
    }

    /// 重命名 BT 任务里的一个文件（`path` 是任务目录内的新相对路径，`/` 或 `\` 分隔）。会话
    /// 里的任务由 librqbit 当场挪动文件；不在会话里的任务直接挪 staging 里的文件（还没落盘
    /// 的只改记录）。重命名表落库，之后每次加入会话都带上。
    ///
    /// # Errors
    /// 任务不存在 / 不是 BT 任务（`NotFound`）；已完成、正在收尾 / 搬移、文件选择尚未确认或
    /// 还没有元数据（`StateConflict`）；路径无效或与其他文件冲突（`InvalidArgument`）；staging
    /// 里新路径已被别的文件占用（`Conflict`）。
    pub async fn rename_bt_file(
        &mut self,
        task_id: &str,
        index: u32,
        path: &str,
    ) -> Result<(), BtTaskOpError> {
        let task = self.load_bt_task_for_op(task_id).await?;
        if task.status == 3 {
            return Err(BtTaskOpError::StateConflict(
                "the download has already completed".to_owned(),
            ));
        }
        if self.db.load_bt_selected_files(task_id).await?.is_none() {
            return Err(BtTaskOpError::StateConflict(
                "the files to download have not been chosen yet".to_owned(),
            ));
        }
        let (_levels, renames_text) = self
            .db
            .get_task_bt_file_state(task_id)
            .await?
            .unwrap_or_default();
        let mut table = parse_rename_table(&renames_text);
        let layout = self.stored_bt_layout(&task).await?;
        let live = match &self.bt_session {
            Some(bt) if bt.cached_handle(task_id).await.is_some() => {
                Some(bt.rename_file(task_id, index, path).await?)
            }
            _ => None,
        };
        let normalized = match live {
            Some(normalized) => normalized,
            None => {
                let layout = layout.as_ref().ok_or_else(|| {
                    BtTaskOpError::StateConflict("the torrent metadata is not known yet".to_owned())
                })?;
                let current = with_renames(&layout.files, &table);
                let normalized = normalize_rename(&current, index, path, cfg!(windows))?;
                let from = usize::try_from(index)
                    .ok()
                    .and_then(|slot| current.get(slot))
                    .map(|file| file.path.clone())
                    .ok_or_else(|| {
                        BtTaskOpError::InvalidArgument(format!("file {index} does not exist"))
                    })?;
                if from != normalized {
                    let stage = bt_downloader::bt_stage_dir(&task.save_dir, task_id);
                    let to = normalized.clone();
                    let moved =
                        tokio::task::spawn_blocking(move || rename_staged_file(&stage, &from, &to))
                            .await
                            .map_err(|error| {
                                BtTaskOpError::Operation(format!("rename worker failed: {error}"))
                            })?;
                    match moved {
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            return Err(BtTaskOpError::Conflict(format!(
                                "'{normalized}' already exists in the task's folder"
                            )));
                        }
                        Err(error) => {
                            return Err(BtTaskOpError::Operation(format!(
                                "failed to rename the file: {error}"
                            )));
                        }
                    }
                }
                normalized
            }
        };
        // 改回种子里的原路径就是撤销重命名：表里不留这一项。
        let published = layout.as_ref().and_then(|layout| {
            usize::try_from(index)
                .ok()
                .and_then(|slot| layout.files.get(slot))
                .map(|file| file.path.clone())
        });
        if published.as_deref() == Some(normalized.as_str()) {
            table.remove(&index);
        } else {
            table.insert(index, normalized.clone());
        }
        self.db
            .set_task_bt_file_renames(task_id, &format_rename_table(&table))
            .await?;
        if let Some(bt) = &self.bt_session {
            bt.register_file_renames(task_id, session_renames(&table))
                .await;
        }
        log_info!(
            "[manager] BT task {} file {} renamed to '{}'",
            task_id,
            index,
            normalized
        );
        Ok(())
    }

    /// 强制重新校验未完成的 BT 任务：丢弃已有进度记录，按 staging 里的数据全量重新校验，
    /// 之后回到原状态。下载中的任务在会话里当场校验（随后继续下载缺的部分）；暂停 / 出错的
    /// 任务以暂停态校验，结束后把新进度落库并广播，仍停在暂停。
    ///
    /// # Errors
    /// 任务不存在 / 不是 BT 任务（`NotFound`）；已完成（改用完整性校验）、尚未开始、正在
    /// 收尾 / 搬移、文件选择未确认或还没有元数据（`StateConflict`）；同一种子已由别的任务加入
    /// 会话（`Conflict`）；加入会话失败（`Operation`）。
    pub async fn recheck_bt_task(&mut self, task_id: &str) -> Result<(), BtTaskOpError> {
        let task = self.load_bt_task_for_op(task_id).await?;
        match task.status {
            1 | 2 | 4 => {}
            3 => {
                return Err(BtTaskOpError::StateConflict(
                    "the download has already completed; use the integrity check instead"
                        .to_owned(),
                ));
            }
            _ => {
                return Err(BtTaskOpError::StateConflict(
                    "the download has not started yet".to_owned(),
                ));
            }
        }
        let selection = self
            .db
            .load_bt_selected_files(task_id)
            .await?
            .ok_or_else(|| {
                BtTaskOpError::StateConflict(
                    "the files to download have not been chosen yet".to_owned(),
                )
            })?;
        if self.bt_session.is_none() {
            self.ensure_bt_session()
                .await
                .map_err(|error| BtTaskOpError::Operation(error.to_string()))?;
        }
        let bt = self.bt_session.clone().ok_or_else(|| {
            BtTaskOpError::Operation("the BitTorrent session is not available".to_owned())
        })?;
        let running = self.active_tasks.contains_key(task_id);
        if let Some(handle) = bt.cached_handle(task_id).await {
            let in_error = matches!(handle.stats().state, librqbit::TorrentStatsState::Error);
            if running && !in_error && !handle.is_paused() {
                // 下载循环继续上报校验与之后的下载进度。
                bt.force_recheck(task_id).await?;
                log_info!("[manager] BT task {} rechecking while running", task_id);
                return Ok(());
            }
            if running {
                return Err(BtTaskOpError::StateConflict(
                    "the task is starting; try again in a moment".to_owned(),
                ));
            }
            if handle.is_paused() && !in_error {
                let handle = bt.force_recheck(task_id).await?;
                self.watch_offline_check(&bt, handle, task_id);
                log_info!("[manager] BT task {} rechecking while paused", task_id);
                return Ok(());
            }
            // 出错的句柄：摘掉后按下面的离线路径以暂停态重新加入并校验。
            bt.evict(task_id, false)
                .await
                .map_err(|error| BtTaskOpError::Operation(error.to_string()))?;
        } else if running {
            return Err(BtTaskOpError::StateConflict(
                "the torrent metadata is not known yet".to_owned(),
            ));
        }
        let source = self.bt_check_source(&task, &bt).await?;
        let only_files = (!selection.is_empty()).then(|| {
            selection
                .iter()
                .filter_map(|index| usize::try_from(*index).ok())
                .collect::<Vec<_>>()
        });
        let options = self.register_bt_download(&bt, &task).await;
        let stage = bt_downloader::bt_stage_dir(&task.save_dir, task_id);
        let handle = bt
            .add_paused_for_check(
                task_id,
                &source,
                &stage,
                only_files,
                options.upload_limit_bps,
            )
            .await?;
        // 校验中的暂停任务也算 BT 活动：不让空闲回收把会话连同校验一起释放。
        self.bt_paused_idle_since = None;
        self.watch_offline_check(&bt, handle, task_id);
        log_info!("[manager] BT task {} rechecking offline", task_id);
        Ok(())
    }

    /// 不在会话里的任务的种子字节（离线校验要靠它加入会话）：.torrent 建的任务取库里的，
    /// 磁力任务取会话目录缓存的 `{info_hash}.torrent`（须与任务一致）。
    async fn bt_check_source(
        &self,
        task: &TaskInfo,
        bt: &SharedBtSession,
    ) -> Result<TorrentSource, BtTaskOpError> {
        let not_known =
            || BtTaskOpError::StateConflict("the torrent metadata is not known yet".to_owned());
        if !is_magnet(&task.url) {
            let bytes = self
                .db
                .load_torrent_file_bytes(&task.task_id)
                .await?
                .filter(|bytes| !bytes.is_empty())
                .ok_or_else(not_known)?;
            return Ok(TorrentSource::TorrentFileBytes(bytes));
        }
        let expected = TorrentSource::Magnet(task.url.clone())
            .info_hash_hex()
            .ok_or_else(not_known)?;
        let bytes = match tokio::fs::read(bt.torrent_path(&expected)).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(not_known());
            }
            Err(error) => {
                return Err(BtTaskOpError::Operation(format!(
                    "failed to read the cached torrent: {error}"
                )));
            }
        };
        let matches = librqbit::torrent_from_bytes(&bytes)
            .is_ok_and(|parsed| parsed.info_hash.as_string().eq_ignore_ascii_case(&expected));
        if !matches {
            return Err(not_known());
        }
        Ok(TorrentSource::TorrentFileBytes(bytes))
    }

    /// 等暂停态的校验结束，把校验出的进度落库并广播任务行（暂停的任务没有下载循环替它上报）。
    fn watch_offline_check(&self, bt: &Arc<SharedBtSession>, handle: BtHandle, task_id: &str) {
        let db = self.db.clone();
        let sink = self.sink.clone();
        let task_id = task_id.to_owned();
        bt.runtime_handle().spawn(async move {
            if let Err(error) = handle.wait_until_initialized().await {
                log_warn!(
                    "[BT] task={} recheck ended with an error: {error:#}",
                    task_id
                );
            }
            let stats = handle.stats();
            let checked = i64::try_from(stats.progress_bytes).unwrap_or(i64::MAX);
            if let Err(error) = db.update_task_progress(&task_id, checked).await {
                crate::logger::report_error("BT", "persist recheck progress", &error);
            }
            log_info!(
                "[BT] task={} recheck finished: {} / {} bytes",
                task_id,
                stats.progress_bytes,
                stats.total_bytes
            );
            bt_downloader::publish_bt_location(&db, sink.as_ref(), &task_id).await;
        });
    }

    /// 开始把 BT 任务的数据移动到 `save_dir`（`daemon.task.moveStorage`）。`resume_after` 首次
    /// 调用传 `false`，收到 [`BtMoveStart::Wait`] 后原样带回。
    ///
    /// 下载中的任务先暂停，等下载循环退出（返回 `Wait`）；做种中的先停止做种。随后把任务摘出
    /// BT 会话、写下搬移日志，返回在 actor 之外执行的作业。目标与当前目录相同为 `Unchanged`。
    ///
    /// # Errors
    /// 任务不存在 / 不是 BT 任务（`NotFound`）；尚未开始、正在收尾 / 校验 / 搬移，或上一次搬移
    /// 到别处还没完成（`StateConflict`）；目标为空、不是绝对路径或落在任务自己的数据里面
    /// （`InvalidArgument`）；目标目录里已有同名条目（`Conflict`）；建目录 / 摘出会话失败
    /// （`Operation`）。
    pub async fn begin_bt_move(
        &mut self,
        task_id: &str,
        save_dir: &str,
        resume_after: bool,
    ) -> Result<BtMoveStart, BtTaskOpError> {
        let started = self
            .begin_bt_move_inner(task_id, save_dir, resume_after)
            .await;
        // 为这次搬移暂停的下载，搬移没开始就恢复原状。
        if resume_after && matches!(started, Ok(BtMoveStart::Unchanged) | Err(_)) {
            self.resume_task_inner(task_id).await;
        }
        started
    }

    async fn begin_bt_move_inner(
        &mut self,
        task_id: &str,
        save_dir: &str,
        resume_after: bool,
    ) -> Result<BtMoveStart, BtTaskOpError> {
        let task = self.load_bt_task_for_op(task_id).await?;
        let to = bt_downloader::normalize_save_dir(save_dir);
        if to.is_empty() {
            return Err(BtTaskOpError::InvalidArgument(
                "the destination folder is empty".to_owned(),
            ));
        }
        if !Path::new(&to).is_absolute() {
            return Err(BtTaskOpError::InvalidArgument(
                "the destination folder must be an absolute path".to_owned(),
            ));
        }
        let key = bt_move::journal_key(task_id);
        if let Some(journal) = self
            .db
            .get_config(&key)
            .await?
            .and_then(|text| MoveJournal::parse(&text))
        {
            // 上一次搬移没做完（数据分散在两处）：只能接着搬到原定的目标。
            if bt_downloader::save_dirs_differ(&journal.to, &to).await {
                return Err(BtTaskOpError::StateConflict(format!(
                    "a previous move to '{}' has not finished; move the task there first",
                    journal.to
                )));
            }
            let guard = self.begin_move_guard(task_id)?;
            log_info!(
                "[manager] BT task {} finishing an interrupted move to {}",
                task_id,
                journal.to
            );
            return Ok(BtMoveStart::Started(move_job(
                task_id.to_owned(),
                journal,
                resume_after,
                guard,
                true,
            )));
        }
        if !bt_downloader::save_dirs_differ(&task.save_dir, &to).await {
            return Ok(BtMoveStart::Unchanged);
        }
        if !matches!(task.status, 1..=4) {
            return Err(BtTaskOpError::StateConflict(
                "the download has not started yet".to_owned(),
            ));
        }
        if let Some(bt) = &self.bt_session
            && (bt.is_finishing(task_id) || bt.is_reseeding(task_id))
        {
            return Err(BtTaskOpError::StateConflict(
                "the task is busy finishing or checking its data".to_owned(),
            ));
        }
        if self.integrity_checks.is_running(task_id) || self.file_hash_jobs.is_running(task_id) {
            return Err(BtTaskOpError::StateConflict(
                "a check of the task's files is running".to_owned(),
            ));
        }
        // 目标检查在暂停之前：被拒的搬移不打断下载。
        let from = PathBuf::from(&task.save_dir);
        let to_path = PathBuf::from(&to);
        let names = self.bt_move_names(&task).await?;
        self.check_bt_move_target(&from, &to_path, &names).await?;
        // 下载中 / 排队中：先静默暂停，等下载循环退出（它的 TaskDone 结束暂停流程）。
        let queued = self.pending_queue.iter().any(|q| q.task_id == task_id);
        if self.active_tasks.contains_key(task_id) || queued {
            self.pause_task_silent(task_id).await;
            return Ok(BtMoveStart::Wait { resume_after: true });
        }
        if self.pending_pauses.contains_key(task_id) {
            return Ok(BtMoveStart::Wait { resume_after });
        }

        // 下载循环已退出：此刻的条目才是最终要搬的，再查一遍后建目标目录。
        let names = self.bt_move_names(&task).await?;
        self.check_bt_move_target(&from, &to_path, &names).await?;
        bt_downloader::ensure_save_dir(&to_path)
            .await
            .map_err(|error| BtTaskOpError::Operation(error.to_string()))?;

        let guard = self.begin_move_guard(task_id)?;
        let reseed = task.status == 3
            && (task.seeding_status == SEEDING_STATUS_ACTIVE
                || task.seeding_status == SEEDING_STATUS_QUEUED);
        if let Some(bt) = self.bt_session.clone() {
            // 先摘出会话（失败时做种照旧），再注销做种者。
            bt.evict(task_id, true)
                .await
                .map_err(|error| BtTaskOpError::Operation(error.to_string()))?;
            if reseed {
                if let Some(seed) = bt.unregister_seeder(task_id).await
                    && let Err(error) = self
                        .db
                        .set_task_seeding_time(task_id, seed.seed_time_secs)
                        .await
                {
                    crate::logger::report_error("download-manager", "persist seeding time", &error);
                }
                // 让出的做种槽位立即给排队中的下一个。
                self.reconcile_seeding_slots().await;
            }
        }
        if reseed {
            self.db
                .update_task_seeding_status(task_id, SEEDING_STATUS_QUEUED, MOVING_MESSAGE)
                .await?;
            self.emit_progress_from_db(task_id, 3, SEEDING_STATUS_QUEUED, MOVING_MESSAGE, 0)
                .await;
        }
        let journal = MoveJournal {
            from: task.save_dir.clone(),
            to,
            names,
            reseed,
        };
        self.db.set_config(&key, &journal.encode()).await?;
        log_info!(
            "[manager] BT task {} moving {:?} from {} to {}",
            task_id,
            journal.names,
            journal.from,
            journal.to
        );
        Ok(BtMoveStart::Started(move_job(
            task_id.to_owned(),
            journal,
            resume_after,
            guard,
            false,
        )))
    }

    async fn check_bt_move_target(
        &self,
        from: &Path,
        to: &Path,
        names: &[String],
    ) -> Result<(), BtTaskOpError> {
        let (from, to, names) = (from.to_path_buf(), to.to_path_buf(), names.to_vec());
        tokio::task::spawn_blocking(move || check_move_target(&from, &to, &names))
            .await
            .map_err(|error| {
                BtTaskOpError::Operation(format!("destination check failed: {error}"))
            })?
    }

    fn begin_move_guard(&self, task_id: &str) -> Result<TaskJobGuard, BtTaskOpError> {
        self.bt_moves.begin(task_id).ok_or_else(|| {
            BtTaskOpError::StateConflict("the task's data is already being moved".to_owned())
        })
    }

    /// 保存目录下属于任务、需要随任务搬走的顶层条目：未完成任务的 staging 目录与完成收尾
    /// 中途已落下的顶层名，已完成任务的落盘名与登记的其余产物（以及残留的 staging）。
    async fn bt_move_names(&self, task: &TaskInfo) -> Result<Vec<String>, BtTaskOpError> {
        let stage_name = format!("{}{}", bt_downloader::BT_STAGE_PREFIX, task.task_id);
        let mut names = vec![stage_name];
        if task.status == 3 {
            names.push(task.file_name.clone());
            names.extend(self.db.load_task_artifacts(&task.task_id).await?);
        } else if let Some(top) = self
            .db
            .get_config(&format!("bt_completion_top_{}", task.task_id))
            .await?
        {
            names.push(top);
        }
        let mut seen = std::collections::HashSet::new();
        names.retain(|name| is_safe_file_name(name) && seen.insert(name.clone()));
        let from = PathBuf::from(&task.save_dir);
        let existing = tokio::task::spawn_blocking(move || {
            names
                .into_iter()
                .filter(|name| std::fs::symlink_metadata(from.join(name)).is_ok())
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|error| BtTaskOpError::Operation(format!("listing task data failed: {error}")))?;
        Ok(existing)
    }

    /// 把搬移作业的结果落库，并恢复任务原来的运行状态：成功则在新位置，失败（已回滚）则在
    /// 旧位置。数据分散在两处（回滚也失败）时任务标为出错，日志保留到下次启动接着搬。
    ///
    /// # Errors
    /// 搬移失败（`Operation`），或新位置落库失败（`Operation`，日志保留，下次启动补齐）。
    pub async fn finish_bt_move(&mut self, outcome: BtMoveOutcome) -> Result<(), BtTaskOpError> {
        let BtMoveOutcome {
            task_id,
            journal,
            resume_after,
            result,
            guard,
        } = outcome;
        let key = bt_move::journal_key(&task_id);
        let Some(task) = self.db.load_task_by_id(&task_id).await? else {
            if let Err(error) = self.db.delete_config(&key).await {
                crate::logger::report_warning("download-manager", "drop move journal", &error);
            }
            return Ok(());
        };
        let (result, restore) = match result {
            Ok(moved) => {
                if let Err(error) = self
                    .db
                    .set_task_location(&task_id, &journal.to, &task.file_name)
                    .await
                {
                    drop(guard);
                    return Err(BtTaskOpError::Operation(format!(
                        "the data was moved but the new location could not be saved ({error:#}); it is saved on the next start"
                    )));
                }
                log_info!(
                    "[manager] BT task {} moved {} item(s) to {}",
                    task_id,
                    moved,
                    journal.to
                );
                self.drop_move_journal(&key).await;
                (Ok(()), true)
            }
            Err(MoveError::RolledBack(message)) => {
                log_warn!(
                    "[manager] BT task {} move rolled back: {}",
                    task_id,
                    message
                );
                self.drop_move_journal(&key).await;
                (Err(BtTaskOpError::Operation(message)), true)
            }
            Err(MoveError::Conflict(name)) => {
                self.drop_move_journal(&key).await;
                (
                    Err(BtTaskOpError::Conflict(format!(
                        "'{name}' already exists in the destination folder"
                    ))),
                    true,
                )
            }
            Err(MoveError::Split(message)) => {
                crate::log_error!(
                    "[manager] BT task {} move left data split: {}",
                    task_id,
                    message
                );
                let marked = if task.status == 3 {
                    self.db
                        .update_task_seeding_status(
                            &task_id,
                            SeedingStopReason::UserStopped.as_i32(),
                            &message,
                        )
                        .await
                } else {
                    self.db.update_task_status(&task_id, 4, &message).await
                };
                if let Err(error) = marked {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                }
                (Err(BtTaskOpError::Operation(message)), false)
            }
        };
        drop(guard);
        bt_downloader::publish_bt_location(&self.db, self.sink.as_ref(), &task_id).await;
        if restore {
            self.restore_after_move(&task_id, journal.reseed, resume_after)
                .await;
        }
        result
    }

    async fn drop_move_journal(&self, key: &str) {
        if let Err(error) = self.db.delete_config(key).await {
            // 留下的日志在下次启动时按「已搬完」处理，只是多一次空跑。
            crate::logger::report_warning("download-manager", "drop move journal", &error);
        }
    }

    /// 搬移结束后回到原状态：做种中的重新挂载做种，下载中的继续下载。
    async fn restore_after_move(&mut self, task_id: &str, reseed: bool, resume_after: bool) {
        if reseed {
            let mut task = match self.db.load_task_by_id(task_id).await {
                Ok(Some(task)) => task,
                Ok(None) => return,
                Err(error) => {
                    crate::logger::report_error("download-manager", "reload moved task", &error);
                    return;
                }
            };
            // 挂载失败时回落到「已停止」并附原因，而不是停在「移动中」。
            let stopped = SeedingStopReason::UserStopped;
            task.seeding_status = stopped.as_i32();
            task.seeding_message = stopped.message().to_owned();
            if self.bt_session.is_none()
                && let Err(error) = self.ensure_bt_session().await
            {
                log_warn!(
                    "[manager] BT task {} cannot seed after the move: {}",
                    task_id,
                    error
                );
                if let Err(error) = self
                    .db
                    .update_task_seeding_status(task_id, stopped.as_i32(), stopped.message())
                    .await
                {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                }
                self.emit_progress_from_db(task_id, 3, stopped.as_i32(), stopped.message(), 0)
                    .await;
                return;
            }
            if let Some(bt) = self.bt_session.clone() {
                self.spawn_reseed_from_disk(bt, &task).await;
            }
        } else if resume_after {
            self.resume_task_inner(task_id).await;
        }
    }

    /// 启动时把上次中断的 BT 数据搬移做完（见 [`crate::bt_move`]），须在扫描 staging 与续种
    /// 之前调用。做不完的保留日志，任务标为出错。
    pub(super) async fn recover_bt_moves(&self) {
        let journals = match self
            .db
            .list_config_with_prefix(bt_move::JOURNAL_PREFIX)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                crate::logger::report_warning("download-manager", "load move journals", &error);
                return;
            }
        };
        for (key, text) in journals {
            let Some(task_id) = key.strip_prefix(bt_move::JOURNAL_PREFIX) else {
                continue;
            };
            let task = match self.db.load_task_by_id(task_id).await {
                Ok(Some(task)) => task,
                Ok(None) => {
                    self.drop_move_journal(&key).await;
                    continue;
                }
                Err(error) => {
                    crate::logger::report_warning("download-manager", "load moved task", &error);
                    continue;
                }
            };
            let Some(journal) = MoveJournal::parse(&text) else {
                self.drop_move_journal(&key).await;
                continue;
            };
            let from = PathBuf::from(&journal.from);
            let to = PathBuf::from(&journal.to);
            let names = journal.names.clone();
            let result = tokio::task::spawn_blocking(move || {
                bt_move::move_entries(&from, &to, &names, true)
            })
            .await
            .unwrap_or_else(|error| Err(MoveError::Split(format!("move worker failed: {error}"))));
            match result {
                Ok(moved) => {
                    if let Err(error) = self
                        .db
                        .set_task_location(task_id, &journal.to, &task.file_name)
                        .await
                    {
                        crate::logger::report_error("download-manager", "save moved task", &error);
                        continue;
                    }
                    log_info!(
                        "[manager] startup: finished moving BT task {} to {} ({} item(s))",
                        task_id,
                        journal.to,
                        moved
                    );
                    self.drop_move_journal(&key).await;
                }
                Err(error) => {
                    let message = format!(
                        "moving the data to '{}' did not finish: {error}",
                        journal.to
                    );
                    log_warn!("[manager] startup: BT task {} {}", task_id, message);
                    if task.status != 3
                        && let Err(error) = self.db.update_task_status(task_id, 4, &message).await
                    {
                        crate::logger::report_error(
                            "download-manager",
                            "persist task state",
                            &error,
                        );
                    }
                }
            }
        }
    }
}

/// librqbit 的重命名表：落库表的下标与 `/` 分隔路径转成 `usize` 与 `PathBuf`。
pub(super) fn session_renames(
    table: &std::collections::BTreeMap<u32, String>,
) -> std::collections::BTreeMap<usize, PathBuf> {
    table
        .iter()
        .filter_map(|(index, path)| Some((usize::try_from(*index).ok()?, PathBuf::from(path))))
        .collect()
}

fn move_job(
    task_id: String,
    journal: MoveJournal,
    resume_after: bool,
    guard: TaskJobGuard,
    resume: bool,
) -> BtMoveJob {
    Box::pin(async move {
        let from = PathBuf::from(&journal.from);
        let to = PathBuf::from(&journal.to);
        let names = journal.names.clone();
        let result =
            tokio::task::spawn_blocking(move || bt_move::move_entries(&from, &to, &names, resume))
                .await
                // 工作线程中途崩溃：不知道挪到了哪一步，按分散处理、保留日志。
                .unwrap_or_else(|error| {
                    Err(MoveError::Split(format!("move worker failed: {error}")))
                });
        BtMoveOutcome {
            task_id,
            journal,
            resume_after,
            result,
            guard,
        }
    })
}

/// 目标目录不能落在要搬的条目里面（按规范化路径比较，挡住符号链接 / 大小写别名），也不能是
/// 当前目录的另一种写法；已存在时不能已有同名条目。目标不必存在。阻塞调用。
fn check_move_target(from: &Path, to: &Path, names: &[String]) -> Result<(), BtTaskOpError> {
    let target = canonicalize_lenient(to).map_err(|error| {
        BtTaskOpError::Operation(format!(
            "cannot resolve the destination folder '{}': {error}",
            to.display()
        ))
    })?;
    if std::fs::canonicalize(from).is_ok_and(|source| source == target) {
        // 换了一种写法的同一个目录：条目都在原处，按冲突报出，不做任何改动。
        return Err(BtTaskOpError::Conflict(
            "the destination is the task's current folder".to_owned(),
        ));
    }
    for name in names {
        if let Ok(entry) = std::fs::canonicalize(from.join(name))
            && target.starts_with(&entry)
        {
            return Err(BtTaskOpError::InvalidArgument(format!(
                "the destination is inside the task's own data ('{name}')"
            )));
        }
    }
    if let Some(name) = bt_move::find_conflict(to, names) {
        return Err(BtTaskOpError::Conflict(format!(
            "'{name}' already exists in '{}'",
            to.display()
        )));
    }
    Ok(())
}

/// 规范化一个不一定存在的路径：最近的已存在上级按文件系统解析（符号链接、`..`、大小写），
/// 其余组件原样接上。
fn canonicalize_lenient(path: &Path) -> std::io::Result<PathBuf> {
    let mut rest = Vec::new();
    let mut current = path;
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut resolved) => {
                for component in rest.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
                    return Err(error);
                };
                rest.push(name.to_os_string());
                current = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn move_target_rejects_own_data_aliases_and_taken_names() {
        let root =
            std::env::temp_dir().join(format!("fluxdown-move-target-{}", uuid::Uuid::new_v4()));
        let from = root.join("from");
        let to = root.join("to");
        std::fs::create_dir_all(from.join("Show/Season 1")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let names = vec!["Show".to_owned()];

        assert!(check_move_target(&from, &to, &names).is_ok());
        // 目标还不存在也能判断，且判断本身不建目录。
        assert!(check_move_target(&from, &to.join("new/deeper"), &names).is_ok());
        assert!(!to.join("new").exists());
        // 搬进自己的数据里（已存在 / 还不存在的目录）。
        for inside in [from.join("Show/Season 1"), from.join("Show/not yet/here")] {
            assert!(
                matches!(
                    check_move_target(&from, &inside, &names),
                    Err(BtTaskOpError::InvalidArgument(_))
                ),
                "{inside:?}"
            );
        }
        assert!(!from.join("Show/not yet").exists());
        // 当前目录的另一种写法。
        assert!(matches!(
            check_move_target(&from, &root.join("to/../from"), &names),
            Err(BtTaskOpError::Conflict(_))
        ));
        // 目标里已有同名条目。
        std::fs::create_dir_all(to.join("Show")).unwrap();
        assert!(matches!(
            check_move_target(&from, &to, &names),
            Err(BtTaskOpError::Conflict(_))
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn move_target_sees_through_symlinked_folders() {
        let root =
            std::env::temp_dir().join(format!("fluxdown-move-link-{}", uuid::Uuid::new_v4()));
        let from = root.join("from");
        std::fs::create_dir_all(from.join("Show/inner")).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&from, &link).unwrap();
        let names = vec!["Show".to_owned()];
        assert!(matches!(
            check_move_target(&from, &link, &names),
            Err(BtTaskOpError::Conflict(_))
        ));
        assert!(matches!(
            check_move_target(&from, &link.join("Show/inner"), &names),
            Err(BtTaskOpError::InvalidArgument(_))
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn session_renames_convert_indices_and_paths() {
        let table = std::collections::BTreeMap::from([
            (0u32, "a/b.mkv".to_owned()),
            (7, "c.srt".to_owned()),
        ]);
        let converted = session_renames(&table);
        assert_eq!(converted.get(&0), Some(&PathBuf::from("a/b.mkv")));
        assert_eq!(converted.get(&7), Some(&PathBuf::from("c.srt")));
        assert_eq!(converted.len(), 2);
    }
}
