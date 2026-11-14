//! Unified task edit coordinator. Writer shutdown and file copies never block actor dispatch.
use super::{DownloadManager, is_safe_file_name};
use crate::{
    task_edit::{self, EditTask, PreparedEdit, storage::MovePlan},
    task_jobs::TaskJobGuard,
};
use std::{path::PathBuf, pin::Pin};

/// A pending edit owns its task until the save commits or fails.
pub struct TaskEditSession {
    edit: PreparedEdit,
    guard: TaskJobGuard,
    resume_after: bool,
    #[cfg(feature = "bt")]
    reseed: bool,
}

/// Work which the host must await outside the download actor.
pub type TaskEditJob = Pin<Box<dyn Future<Output = TaskEditOutcome> + Send>>;

/// Next step in one task save.
#[allow(
    clippy::large_enum_variant,
    reason = "The host already boxes pending work; keep inline sessions to avoid a second allocation for writer shutdown."
)]
pub enum TaskEditStep {
    /// Wait for the existing writer's normal completion notification, then continue.
    Waiting(TaskEditSession),
    /// Copy/publish the files outside the actor, then call `finish_task_edit`.
    Started(TaskEditJob),
    /// Settings were committed without moving files.
    Applied,
}

/// Relocation outcome; metadata has not yet changed.
pub struct TaskEditOutcome {
    session: TaskEditSession,
    plan: MovePlan,
    result: Result<(), String>,
}

impl DownloadManager {
    /// Validate a complete save before cancelling a writer or touching files.
    pub async fn begin_task_edit(&mut self, patch: EditTask) -> Result<TaskEditStep, String> {
        let id = patch.task_id.clone();
        if self.edit_jobs.is_running(&id) || self.bt_move_running(&id) {
            return Err("edit-busy".into());
        }
        let edit = task_edit::prepare(&self.db, patch).await?;
        #[cfg(feature = "bt")]
        if edit.relocate
            && super::is_bt_url(&edit.original.url)
            && (self.integrity_checks.is_running(&id)
                || self.file_hash_jobs.is_running(&id)
                || self
                    .bt_session
                    .as_ref()
                    .is_some_and(|bt| bt.is_finishing(&id) || bt.is_reseeding(&id)))
        {
            return Err("edit-busy".into());
        }
        if edit.relocate {
            self.edit_move_plan(&edit).await?;
        }
        let active = self.active_tasks.contains_key(&id) || matches!(edit.original.status, 1 | 5);
        let future = edit.updated.properties.scheduled_at > crate::auth::now_unix();
        let stop = active && (edit.reconnect || edit.relocate || future);
        let resume_after = stop || (edit.original.status == 4 && edit.reconnect);
        if stop {
            self.pause_task_for_reconnect(&id).await;
            if let Some(pending) = self.pending_pauses.get_mut(&id) {
                pending.resume_requested = false;
            }
        }
        let guard = self.edit_jobs.begin(&id).ok_or("edit-busy")?;
        let session = TaskEditSession {
            edit,
            guard,
            resume_after,
            #[cfg(feature = "bt")]
            reseed: false,
        };
        self.continue_task_edit(session).await
    }

    /// Continue only after the previous writer has stopped flushing its files.
    pub async fn continue_task_edit(
        &mut self,
        session: TaskEditSession,
    ) -> Result<TaskEditStep, String> {
        #[cfg(feature = "bt")]
        let mut session = session;
        let id = session.edit.original.task_id.clone();
        if session.guard.token().is_cancelled() {
            self.abandon_task_edit(session).await;
            return Err("edit-cancelled".into());
        }
        if self.pending_pauses.contains_key(&id) {
            return Ok(TaskEditStep::Waiting(session));
        }
        let current = match self.db.load_task_by_id(&id).await {
            Ok(Some(task)) => task,
            Ok(None) => {
                self.abandon_task_edit(session).await;
                return Err("not-found".into());
            }
            Err(error) => {
                self.abandon_task_edit(session).await;
                return Err(format!("db: {error:#}"));
            }
        };
        // A probe may have finalized the name while cancellation was in flight. Never move the wrong file.
        if current.file_name != session.edit.original.file_name
            || current.save_dir != session.edit.original.save_dir
            || (current.status == 3 && session.edit.original.status != 3)
        {
            self.abandon_task_edit(session).await;
            return Err("edit-conflict".into());
        }
        if session.edit.relocate {
            #[cfg(feature = "bt")]
            if super::is_bt_url(&session.edit.original.url)
                && let Err(error) = self.detach_bt_for_task_edit(&mut session).await
            {
                self.abandon_task_edit(session).await;
                return Err(error);
            }
            match self.edit_move_plan(&session.edit).await {
                Ok(mut plan) => {
                    let db = self.db.clone();
                    Ok(TaskEditStep::Started(Box::pin(async move {
                        let result = plan.publish(&db, &session.guard.token()).await;
                        TaskEditOutcome {
                            session,
                            plan,
                            result,
                        }
                    })))
                }
                Err(error) => {
                    self.abandon_task_edit(session).await;
                    Err(error)
                }
            }
        } else {
            self.commit_task_property_edit(session, None).await?;
            Ok(TaskEditStep::Applied)
        }
    }

    /// Roll back published targets on failure; old files remain intact throughout.
    pub async fn finish_task_edit(&mut self, outcome: TaskEditOutcome) -> Result<(), String> {
        let TaskEditOutcome {
            session,
            plan,
            result,
        } = outcome;
        let result = if session.guard.token().is_cancelled() {
            Err("edit-cancelled".into())
        } else {
            result
        };
        if let Err(mut error) = result {
            if let Err(rollback) = plan.rollback(&self.db).await {
                error.push_str(&format!("; {rollback}"));
            }
            self.abandon_task_edit(session).await;
            return Err(error);
        }
        self.commit_task_property_edit(session, Some(plan)).await
    }

    /// Cancel an unfinished save, restoring only the state interrupted by this save.
    pub async fn abandon_task_edit(&mut self, session: TaskEditSession) {
        let id = session.edit.original.task_id.clone();
        let cancelled = session.guard.token().is_cancelled();
        let resume = session.resume_after && !session.guard.token().is_cancelled();
        #[cfg(feature = "bt")]
        let reseed = session.reseed && !session.guard.token().is_cancelled();
        drop(session);
        if cancelled {
            self.pause_task(&id).await;
            return;
        }
        #[cfg(feature = "bt")]
        if reseed {
            self.restore_after_move(&id, true, false).await;
        }
        if resume {
            self.resume_task(&id).await;
        }
    }

    async fn commit_task_property_edit(
        &mut self,
        session: TaskEditSession,
        plan: Option<MovePlan>,
    ) -> Result<(), String> {
        let TaskEditSession {
            edit,
            guard,
            resume_after,
            #[cfg(feature = "bt")]
            reseed,
        } = session;
        let id = edit.updated.task_id.clone();
        if let Err(error) = self.db.commit_task_edit(&edit).await {
            let mut error = format!("db: {error:#}");
            if let Some(plan) = &plan
                && let Err(rollback) = plan.rollback(&self.db).await
            {
                error.push_str(&format!("; {rollback}"));
            }
            drop(guard);
            if resume_after {
                self.resume_task(&id).await;
            }
            #[cfg(feature = "bt")]
            if reseed {
                self.restore_after_move(&id, true, false).await;
            }
            return Err(error);
        }
        if let Some(limiter) = self.task_limiters.get(&id) {
            limiter.set_task_override(edit.updated.properties.download_limit_bps as u64);
        }
        #[cfg(feature = "bt")]
        if super::is_bt_url(&edit.updated.url) {
            self.apply_task_bt_limits(&edit.updated).await;
        }
        if let Some(queued) = self
            .pending_queue
            .items
            .iter_mut()
            .find(|q| q.task_id == id)
        {
            queued.file_name.clone_from(&edit.updated.file_name);
            queued.save_dir.clone_from(&edit.updated.save_dir);
            queued.proxy_url.clone_from(&edit.updated.proxy_url);
            queued.segments = edit.updated.segments;
            queued.is_resume = true;
        }
        let scheduled = edit.updated.properties.scheduled_at;
        if scheduled > 0 {
            self.scheduled_tasks.insert(id.clone(), scheduled);
        } else {
            self.scheduled_tasks.remove(&id);
        }
        self.auto_failover_pending.remove(&id);
        self.auto_failover_attempts.remove(&id);
        if let Some(plan) = &plan
            && let Err(error) = plan.clean_sources(&self.db).await
        {
            crate::log_warn!("[task-edit] source cleanup failed (journal retained): {error}");
        }
        drop(guard);
        if scheduled > crate::auth::now_unix() {
            self.pause_task_silent(&id).await;
        } else if resume_after {
            self.resume_task(&id).await;
        }
        #[cfg(feature = "bt")]
        if reseed {
            self.restore_after_move(&id, true, false).await;
        }
        self.send_tasks_snapshot().await;
        Ok(())
    }

    async fn edit_move_plan(&self, edit: &PreparedEdit) -> Result<MovePlan, String> {
        let old = &edit.original;
        let new = &edit.updated;
        if !old.file_name.is_empty() && !is_safe_file_name(&old.file_name) {
            return Err("invalid-original-name".into());
        }
        let old_dir = PathBuf::from(&old.save_dir);
        let new_dir = PathBuf::from(&new.save_dir);
        let old_path = old_dir.join(&old.file_name);
        let new_path = new_dir.join(&new.file_name);
        let mut candidates = Vec::new();
        #[cfg(feature = "bt")]
        if super::is_bt_url(&old.url) {
            for name in self
                .bt_move_names(old)
                .await
                .map_err(|e| format!("move: {e:#}"))?
            {
                candidates.push((old_dir.join(&name), new_dir.join(name)));
            }
        }
        if !super::is_bt_url(&old.url) && !old.file_name.is_empty() {
            candidates.push((old_path.clone(), new_path.clone()));
            candidates.push((
                PathBuf::from(format!(
                    "{}{}",
                    old_path.display(),
                    crate::downloader::TEMP_EXT
                )),
                PathBuf::from(format!(
                    "{}{}",
                    new_path.display(),
                    crate::downloader::TEMP_EXT
                )),
            ));
            if crate::hls_downloader::is_hls_url(&old.url)
                || crate::dash_downloader::is_dash_url(&old.url)
                || self
                    .db
                    .load_audio_url(&old.task_id)
                    .await
                    .map_err(|e| format!("db: {e:#}"))?
                    .is_some()
            {
                let from = crate::dash_downloader::build_audio_path(&old_path);
                let to = crate::dash_downloader::build_audio_path(&new_path);
                candidates.push((from.clone(), to.clone()));
                candidates.push((
                    PathBuf::from(format!("{}{}", from.display(), crate::downloader::TEMP_EXT)),
                    PathBuf::from(format!("{}{}", to.display(), crate::downloader::TEMP_EXT)),
                ));
            }
        }
        for name in self
            .db
            .load_task_artifacts(&old.task_id)
            .await
            .map_err(|e| format!("db: {e:#}"))?
        {
            if !is_safe_file_name(&name) {
                return Err("invalid-artifact-name".into());
            }
            let pair = (old_dir.join(&name), new_dir.join(&name));
            if !candidates.contains(&pair) {
                candidates.push(pair);
            }
        }
        let mut pairs = Vec::new();
        let resolved_target = resolve_directory(&new_dir).await?;
        if super::file_name_claimed_by_others(
            &self.db,
            &new.save_dir,
            &new.file_name,
            std::slice::from_ref(&new.task_id),
        )
        .await
        {
            return Err("target-exists".into());
        }
        for (source, target) in candidates {
            if source == target {
                continue;
            }
            match tokio::fs::symlink_metadata(&source).await {
                Ok(meta)
                    if meta.file_type().is_symlink() || (!meta.is_file() && !meta.is_dir()) =>
                {
                    return Err("unsupported-task-file-type".into());
                }
                Ok(meta) => {
                    if super::file_name_claimed_by_others(
                        &self.db,
                        &old.save_dir,
                        source
                            .file_name()
                            .and_then(|name| name.to_str())
                            .ok_or("invalid-original-name")?,
                        std::slice::from_ref(&old.task_id),
                    )
                    .await
                    {
                        return Err("shared-source-file".into());
                    }
                    if meta.is_dir() {
                        let canonical = tokio::fs::canonicalize(&source)
                            .await
                            .map_err(|e| format!("move: {e}"))?;
                        if resolved_target.starts_with(canonical) {
                            return Err("destination-inside-source".into());
                        }
                    }
                    if tokio::fs::try_exists(&target)
                        .await
                        .map_err(|e| format!("move: {e}"))?
                    {
                        return Err("target-exists".into());
                    }
                    pairs.push((source, target));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("move: {e}")),
            }
        }
        Ok(MovePlan {
            task_id: old.task_id.clone(),
            old_dir: old.save_dir.clone(),
            old_name: old.file_name.clone(),
            new_dir: new.save_dir.clone(),
            new_name: new.file_name.clone(),
            pairs,
            created: Vec::new(),
        })
    }

    /// Reuse the host's existing scheduler tick, with no filesystem reads while unscheduled.
    pub async fn tick_task_schedules(&mut self) {
        let now = crate::auth::now_unix();
        let due: Vec<_> = self
            .scheduled_tasks
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            if self.edit_jobs.is_running(&id) {
                continue;
            }
            let task = match self.db.load_task_by_id(&id).await {
                Ok(Some(task)) => task,
                Ok(None) => {
                    self.scheduled_tasks.remove(&id);
                    continue;
                }
                Err(error) => {
                    crate::logger::report_error("task-edit", "load scheduled task", &error);
                    continue;
                }
            };
            if self
                .queues
                .get(&task.queue_id)
                .is_some_and(|q| !q.is_running)
            {
                continue;
            }
            if let Err(error) = self.clear_task_schedule(&id).await {
                crate::logger::report_error("task-edit", "consume one-shot schedule", &error);
                continue;
            }
            self.resume_task(&id).await;
        }
    }

    pub(super) async fn clear_task_schedule(
        &mut self,
        task_id: &str,
    ) -> Result<(), crate::db::DbError> {
        if self.scheduled_tasks.contains_key(task_id) {
            let mut task = self
                .db
                .load_task_by_id(task_id)
                .await?
                .ok_or(sqlx::Error::RowNotFound)?;
            task.properties.scheduled_at = 0;
            let raw = serde_json::to_string(&task.properties).map_err(|_| {
                crate::db::DbError::InvalidConfig("schedule encoding failed".into())
            })?;
            sqlx::query("UPDATE tasks SET edit_properties = $1 WHERE id = $2")
                .bind(raw)
                .bind(task_id)
                .execute(self.db.pool())
                .await?;
            self.scheduled_tasks.remove(task_id);
        }
        Ok(())
    }

    pub(super) fn limiter_for_task(
        &mut self,
        task_id: &str,
        queue_id: &str,
        limit: i64,
    ) -> crate::speed_limiter::SpeedLimiter {
        let limiter = self
            .queue_limiter_for(queue_id)
            .with_task_override(limit.max(0) as u64);
        self.task_limiters
            .insert(task_id.to_owned(), limiter.clone());
        limiter
    }
}

#[cfg(feature = "bt")]
impl DownloadManager {
    async fn detach_bt_for_task_edit(
        &mut self,
        session: &mut TaskEditSession,
    ) -> Result<(), String> {
        let task = &session.edit.original;
        if let Some(bt) = self.bt_session.clone() {
            bt.evict(&task.task_id, true)
                .await
                .map_err(|e| format!("move: {e:#}"))?;
            session.reseed = task.status == 3
                && matches!(
                    task.seeding_status,
                    super::SEEDING_STATUS_ACTIVE | super::SEEDING_STATUS_QUEUED
                );
            if let Some(seed) = bt.unregister_seeder(&task.task_id).await {
                self.db
                    .set_task_seeding_time(&task.task_id, seed.seed_time_secs)
                    .await
                    .map_err(|e| format!("db: {e:#}"))?;
            }
        }
        Ok(())
    }
}

async fn resolve_directory(path: &std::path::Path) -> Result<PathBuf, String> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        match tokio::fs::canonicalize(ancestor).await {
            Ok(mut resolved) => {
                if !tokio::fs::metadata(&resolved)
                    .await
                    .map_err(|e| format!("move: {e}"))?
                    .is_dir()
                {
                    return Err("invalid-directory".into());
                }
                for name in suffix.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(ancestor.file_name().ok_or("invalid-directory")?.to_owned());
                ancestor = ancestor.parent().ok_or("invalid-directory")?;
            }
            Err(error) => return Err(format!("move: {error}")),
        }
    }
}
