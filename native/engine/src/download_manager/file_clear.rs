use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{DELETE_FILE_CLEANUP_TIMEOUT, DownloadManager, is_safe_file_name, task_target_path};
use crate::db::DbError;
use crate::downloader::DownloadError;
use crate::events::EngineEvent;
use crate::model::FileClearState;

// Filesystem/device, file index, and creation time on platforms that expose it.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
struct FileId(u64, u64, u64);

#[cfg(unix)]
fn file_id(_: &Path, metadata: &fs::Metadata) -> io::Result<FileId> {
    use std::os::unix::fs::MetadataExt as _;

    if metadata.ino() == 0 {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "file identity unavailable",
        ));
    }
    Ok(FileId(metadata.dev(), metadata.ino(), 0))
}

#[cfg(windows)]
fn file_id(path: &Path, metadata: &fs::Metadata) -> io::Result<FileId> {
    use std::os::windows::{fs::OpenOptionsExt as _, io::AsRawHandle as _};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        GetFileInformationByHandle,
    };

    let flags = if metadata.is_dir() {
        // Save directories may be junctions or volume mount points. Identify the target.
        FILE_FLAG_BACKUP_SEMANTICS
    } else {
        FILE_FLAG_OPEN_REPARSE_POINT
    };
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(flags)
        .open(path)?;
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: the owned handle is live and info points to writable storage for the output.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful call initialized the output structure.
    let info = unsafe { info.assume_init() };
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    if index == 0 {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "file identity unavailable",
        ));
    }
    let created = (u64::from(info.ftCreationTime.dwHighDateTime) << 32)
        | u64::from(info.ftCreationTime.dwLowDateTime);
    Ok(FileId(u64::from(info.dwVolumeSerialNumber), index, created))
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
struct OutputId {
    id: FileId,
    len: u64,
    modified: Option<u128>,
}

fn output_id(path: &Path, metadata: &fs::Metadata) -> io::Result<OutputId> {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|time| time.as_nanos());
    Ok(OutputId {
        id: file_id(path, metadata)?,
        len: metadata.len(),
        modified,
    })
}

fn directory_id(path: &Path) -> io::Result<FileId> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            ErrorKind::NotADirectory,
            "save directory unavailable",
        ));
    }
    file_id(path, &metadata)
}

#[derive(Serialize, Deserialize)]
struct ClearIntent {
    directory: FileId,
    primary: OutputId,
    artifacts: Vec<(String, OutputId)>,
}

impl ClearIntent {
    fn capture(primary: &Path, artifacts: Vec<PathBuf>) -> io::Result<Self> {
        let parent = primary.parent().ok_or_else(unavailable_path)?;
        let directory = directory_id(parent)?;
        let primary_id = capture_output(primary)?
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "task output unavailable"))?;
        let mut captured = Vec::with_capacity(artifacts.len());
        for artifact in artifacts {
            if artifact != primary
                && let Some(id) = capture_output(&artifact)?
            {
                let name = artifact
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(unavailable_path)?;
                captured.push((name.to_owned(), id));
            }
        }
        let intent = Self {
            directory,
            primary: primary_id,
            artifacts: captured,
        };
        intent.verify_directory(parent)?;
        Ok(intent)
    }

    fn verify_directory(&self, parent: &Path) -> io::Result<()> {
        if directory_id(parent)? != self.directory {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                "original save directory unavailable",
            ));
        }
        Ok(())
    }

    // Recovery may finish the sidecars, but only an explicit clear can unlink the primary.
    fn reconcile(
        self,
        primary: PathBuf,
        unlink_primary: bool,
        stop: &CancellationToken,
    ) -> io::Result<bool> {
        let parent = primary.parent().ok_or_else(unavailable_path)?;
        if !self.reconcile_output(parent, &primary, &self.primary, unlink_primary, stop)? {
            return Ok(false);
        }
        for (name, id) in &self.artifacts {
            if !is_safe_file_name(name) {
                return Err(unavailable_path());
            }
            self.reconcile_output(parent, &parent.join(name), id, true, stop)?;
        }
        // A missing path on a detached mount must never be accepted as a successful unlink.
        self.verify_directory(parent)?;
        Ok(true)
    }

    // False means the original output is still present and this pass must not unlink it.
    fn reconcile_output(
        &self,
        parent: &Path,
        path: &Path,
        id: &OutputId,
        unlink: bool,
        stop: &CancellationToken,
    ) -> io::Result<bool> {
        self.verify_directory(parent)?;
        if !still_matches(path, id)? {
            return Ok(true);
        }
        if !unlink {
            return Ok(false);
        }
        if stop.is_cancelled() {
            return Err(io::Error::new(ErrorKind::Interrupted, "file clear stopped"));
        }
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != ErrorKind::NotFound {
                return Err(error);
            }
            self.verify_directory(parent)?;
        }
        Ok(true)
    }
}

fn unavailable_path() -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, "invalid task output path")
}

fn capture_output(path: &Path) -> io::Result<Option<OutputId>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(output_id(path, &metadata)?)),
        Ok(_) => Err(unavailable_path()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn still_matches(path: &Path, id: &OutputId) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(output_id(path, &metadata)? == *id),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn unavailable() -> DownloadError {
    DownloadError::Other("Task output is not eligible for cleanup".into())
}

impl DownloadManager {
    // Runs before the first snapshot. A timed-out worker is remembered just as during a user clear.
    pub(crate) async fn recover_pending_file_clears(&mut self) -> Result<(), DbError> {
        for (id, dir, name) in self.db.pending_task_file_clears().await? {
            match self.reconcile_file_clear(&id, &dir, &name, false).await {
                Ok(_) => {}
                Err(DownloadError::Db(error)) => return Err(error),
                Err(error) => {
                    crate::logger::report_warning("download-manager", "recover file clear", &error)
                }
            }
        }
        Ok(())
    }

    /// Remove the outputs of a completed non-BT task without resetting its history.
    pub async fn clear_task_file(&mut self, task_id: &str) -> Result<(), DownloadError> {
        // A relocation can publish its destination before the stored path is updated.
        if self.edit_jobs.is_running(task_id) {
            return Err(unavailable());
        }
        let task = self
            .db
            .load_task_by_id(task_id)
            .await?
            .ok_or_else(unavailable)?;
        if task.status != 3
            || super::is_bt_url(&task.url)
            || !is_safe_file_name(&task.file_name)
            || task.file_missing
        {
            return Err(unavailable());
        }
        let removed = if task.file_clear_state == FileClearState::Pending {
            self.reconcile_file_clear(task_id, &task.save_dir, &task.file_name, true)
                .await?
        } else {
            let mut plans = self
                .plan_batch_deletion(std::slice::from_ref(&task.task_id), true)
                .await?;
            let files = plans.remove(task_id).ok_or_else(unavailable)?;
            if !files.owns_final {
                return Err(unavailable());
            }
            self.file_hash_jobs.cancel(task_id);
            let primary =
                task_target_path(&task.save_dir, &task.file_name).ok_or_else(unavailable)?;
            let intent = tokio::time::timeout(
                DELETE_FILE_CLEANUP_TIMEOUT,
                tokio::task::spawn_blocking(move || {
                    ClearIntent::capture(&primary, files.artifacts)
                }),
            )
            .await
            .map_err(|error| DownloadError::Other(format!("capture file clear intent: {error}")))?
            .map_err(|error| {
                DownloadError::Other(format!("file clear capture worker failed: {error}"))
            })??;
            let raw = serde_json::to_string(&intent)
                .map_err(|error| DbError::InvalidConfig(error.to_string()))?;
            if !self.db.begin_task_file_clear(task_id, &raw).await? {
                return Err(unavailable());
            }
            self.run_file_clear(task_id, &task.save_dir, &task.file_name, true, intent)
                .await?
        };
        if !removed {
            return Err(unavailable());
        }
        self.emit_task_file_cleared(task_id);
        Ok(())
    }

    pub(super) async fn reconcile_file_clear(
        &mut self,
        task_id: &str,
        save_dir: &str,
        file_name: &str,
        explicit: bool,
    ) -> Result<bool, DownloadError> {
        if self
            .pending_file_unlinks
            .get(task_id)
            .is_some_and(|worker| !worker.is_finished())
        {
            return Err(unavailable());
        }
        self.pending_file_unlinks.remove(task_id);
        let raw = self.db.load_task_file_clear_intent(task_id).await?;
        // Older intents with no directory witness cannot prove unlink, even when the parent exists.
        if raw.is_empty() {
            return Err(unavailable());
        }
        let intent = serde_json::from_str(&raw)
            .map_err(|error| DownloadError::Other(format!("invalid file clear intent: {error}")))?;
        self.run_file_clear(task_id, save_dir, file_name, explicit, intent)
            .await
    }

    async fn run_file_clear(
        &mut self,
        task_id: &str,
        save_dir: &str,
        file_name: &str,
        explicit: bool,
        mut intent: ClearIntent,
    ) -> Result<bool, DownloadError> {
        let primary = task_target_path(save_dir, file_name).ok_or_else(unavailable)?;
        let facts = self
            .db
            .load_deletion_facts(&[], &[save_dir.to_owned()])
            .await?;
        let mut key = (save_dir.to_owned(), String::new());
        let mut claimed = |name: &str| {
            key.1.clear();
            key.1.push_str(name);
            facts
                .file_owners
                .get(&key)
                .is_some_and(|owners| owners.iter().any(|owner| owner != task_id))
        };
        // Recheck on every pass: a persisted artifact name does not reserve that path.
        intent.artifacts.retain(|(name, _)| !claimed(name));
        let stop = CancellationToken::new();
        let cancel = stop.clone().drop_guard();
        let unlink_primary = explicit && !claimed(file_name);
        let mut worker =
            tokio::task::spawn_blocking(move || intent.reconcile(primary, unlink_primary, &stop));
        let removed = match tokio::time::timeout(DELETE_FILE_CLEANUP_TIMEOUT, &mut worker).await {
            Ok(result) => result.map_err(|error| {
                DownloadError::Other(format!("file clear worker failed: {error}"))
            })??,
            Err(error) => {
                drop(cancel);
                worker.abort();
                self.pending_file_unlinks
                    .retain(|_, worker| !worker.is_finished());
                self.pending_file_unlinks.insert(task_id.to_owned(), worker);
                return Err(DownloadError::Other(format!("file cleanup: {error}")));
            }
        };
        self.db.finish_task_file_clear(task_id, removed).await?;
        Ok(removed)
    }

    pub(super) fn emit_task_file_cleared(&self, task_id: &str) {
        self.sink.emit(EngineEvent::FileMissingChanged(vec![(
            task_id.to_owned(),
            true,
        )]));
    }
}

#[cfg(test)]
pub(super) fn capture_test_intent(primary: &Path, artifacts: Vec<PathBuf>) -> String {
    serde_json::to_string(&ClearIntent::capture(primary, artifacts).unwrap()).unwrap()
}
