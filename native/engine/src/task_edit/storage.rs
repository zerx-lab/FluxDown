//! Publish relocated task files without overwriting targets or removing the old copy before commit.
use crate::db::Db;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

pub(crate) const JOURNAL_PREFIX: &str = "task_edit_move_";

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct MovePlan {
    pub task_id: String,
    pub old_dir: String,
    pub old_name: String,
    pub new_dir: String,
    pub new_name: String,
    pub pairs: Vec<(PathBuf, PathBuf)>,
    pub created: Vec<PathBuf>,
}

impl MovePlan {
    pub fn key(&self) -> String {
        format!("{JOURNAL_PREFIX}{}", self.task_id)
    }

    async fn save(&self, db: &Db) -> Result<(), String> {
        let raw = serde_json::to_string(self).map_err(|e| format!("move journal: {e}"))?;
        db.set_config(&self.key(), &raw)
            .await
            .map_err(|e| format!("move journal: {e:#}"))
    }

    /// The old files remain authoritative until the metadata transaction succeeds.
    pub async fn publish(&mut self, db: &Db, cancel: &CancellationToken) -> Result<(), String> {
        if self.pairs.is_empty() {
            return Ok(());
        }
        for (_, target) in &self.pairs {
            if tokio::fs::try_exists(target)
                .await
                .map_err(|e| format!("move: {e}"))?
            {
                return Err("target-exists".into());
            }
        }
        self.save(db).await?;
        tokio::fs::create_dir_all(&self.new_dir)
            .await
            .map_err(|e| format!("move: {e}"))?;
        for (source, target) in &self.pairs {
            if cancel.is_cancelled() {
                return Err("edit-cancelled".into());
            }
            let meta = tokio::fs::symlink_metadata(source)
                .await
                .map_err(|e| format!("move: {e}"))?;
            if meta.is_dir() {
                tokio::fs::create_dir(target)
                    .await
                    .map_err(|e| format!("move: {e}"))?;
                self.created.push(target.clone());
                self.save(db).await?;
                copy_directory(source, target, cancel).await?;
            } else {
                let output = create_destination(source, target).await?;
                self.created.push(target.clone());
                self.save(db).await?;
                if let Some(output) = output {
                    tokio::select! {
                        () = cancel.cancelled() => return Err("edit-cancelled".into()),
                        result = copy_file(source, output) => result?,
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn rollback(&self, db: &Db) -> Result<(), String> {
        for target in self.created.iter().rev() {
            match remove_entry(target).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("move rollback: {e}")),
            }
        }
        db.delete_config(&self.key())
            .await
            .map_err(|e| format!("move rollback journal: {e:#}"))
    }

    pub async fn clean_sources(&self, db: &Db) -> Result<(), String> {
        for (source, _) in &self.pairs {
            match remove_entry(source).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("move source cleanup: {e}")),
            }
        }
        db.delete_config(&self.key())
            .await
            .map_err(|e| format!("move cleanup journal: {e:#}"))
    }
}

pub(crate) async fn recover(db: &Db) -> Result<(), String> {
    for (_, raw) in db
        .list_config_with_prefix(JOURNAL_PREFIX)
        .await
        .map_err(|e| format!("move recovery: {e:#}"))?
    {
        let plan: MovePlan =
            serde_json::from_str(&raw).map_err(|_| "invalid task relocation journal")?;
        let task = db
            .load_task_by_id(&plan.task_id)
            .await
            .map_err(|e| format!("move recovery: {e:#}"))?;
        if task.is_some_and(|t| t.save_dir == plan.new_dir && t.file_name == plan.new_name) {
            plan.clean_sources(db).await?;
        } else {
            plan.rollback(db).await?;
        }
    }
    Ok(())
}

async fn remove_entry(path: &std::path::Path) -> Result<(), std::io::Error> {
    let meta = tokio::fs::symlink_metadata(path).await?;
    if meta.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    }
}

async fn create_destination(
    source: &std::path::Path,
    target: &std::path::Path,
) -> Result<Option<tokio::fs::File>, String> {
    match tokio::fs::hard_link(source, target).await {
        Ok(()) => Ok(None),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::CrossesDevices
                    | std::io::ErrorKind::Unsupported
                    | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)
                .await
                .map(Some)
                .map_err(|e| format!("move: {e}"))
        }
        Err(error) => Err(format!("move: {error}")),
    }
}

async fn copy_file(source: &std::path::Path, mut output: tokio::fs::File) -> Result<(), String> {
    let mut input = tokio::fs::File::open(source)
        .await
        .map_err(|e| format!("move: {e}"))?;
    tokio::io::copy(&mut input, &mut output)
        .await
        .map_err(|e| format!("move: {e}"))?;
    output
        .set_permissions(
            input
                .metadata()
                .await
                .map_err(|e| format!("move: {e}"))?
                .permissions(),
        )
        .await
        .map_err(|e| format!("move: {e}"))?;
    output.flush().await.map_err(|e| format!("move: {e}"))?;
    output.sync_all().await.map_err(|e| format!("move: {e}"))
}

async fn copy_directory(
    source: &std::path::Path,
    target: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let mut pending = vec![(source.to_owned(), target.to_owned())];
    while let Some((source, target)) = pending.pop() {
        let mut entries = tokio::fs::read_dir(&source)
            .await
            .map_err(|e| format!("move: {e}"))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("move: {e}"))?
        {
            if cancel.is_cancelled() {
                return Err("edit-cancelled".into());
            }
            let kind = entry.file_type().await.map_err(|e| format!("move: {e}"))?;
            let destination = target.join(entry.file_name());
            if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
                return Err("unsupported-task-file-type".into());
            }
            if kind.is_dir() {
                tokio::fs::create_dir(&destination)
                    .await
                    .map_err(|e| format!("move: {e}"))?;
                pending.push((entry.path(), destination));
            } else if let Some(output) = create_destination(&entry.path(), &destination).await? {
                let source = entry.path();
                tokio::select! {
                    () = cancel.cancelled() => return Err("edit-cancelled".into()),
                    result = copy_file(&source, output) => result?,
                }
            }
        }
        tokio::fs::set_permissions(
            &target,
            tokio::fs::metadata(&source)
                .await
                .map_err(|e| format!("move: {e}"))?
                .permissions(),
        )
        .await
        .map_err(|e| format!("move: {e}"))?;
    }
    Ok(())
}
