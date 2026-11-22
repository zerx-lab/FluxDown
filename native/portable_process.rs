//! Windows portable process leases, shared by the packaged binaries and the update worker.
//! The intent lock serializes admission; the lifetime lock drains old processes without PID reuse.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::sync::OnceLock;

pub(crate) const INTENT: &str = ".update-intent.lock";
pub(crate) const RUNNING: &str = ".update-process.lock";
pub(crate) const DATA_DIR: &str = "portable_data";

#[cfg(test)]
#[path = "portable_process_tests.rs"]
mod tests;

/// Admit a packaged process, or return false when an update has paused startup.
/// The OS releases its shared lifetime lease when the process exits, even on a crash.
pub(crate) fn admit() -> io::Result<bool> {
    static RUNNING_LEASE: OnceLock<File> = OnceLock::new();
    let exe = std::env::current_exe()?;
    Ok(while_starting(&exe, |data| {
        if let Some(data) = data {
            let running = shared_lease(data, RUNNING)?;
            running.try_lock_shared().map_err(io::Error::from)?;
            RUNNING_LEASE.get_or_init(|| running);
        }
        Ok(())
    })?
    .is_some())
}

/// Serialize process admission or a launch with the updater's transition to draining.
pub(crate) fn while_starting<T>(
    exe: &Path,
    start: impl FnOnce(Option<&Path>) -> io::Result<T>,
) -> io::Result<Option<T>> {
    let Some(dir) = exe.parent() else {
        return start(None).map(Some);
    };
    if !dir.join("portable").try_exists()? {
        return start(None).map(Some);
    }
    let data = dir.join(DATA_DIR);
    let intent = match shared_lease(&data, INTENT) {
        Ok(intent) => intent,
        // A read-only portable install can still run with an external data dir. It cannot
        // start an updater (which requires these same writable coordination files).
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) && !data.join(INTENT).try_exists()? =>
        {
            return start(None).map(Some);
        }
        Err(error) => return Err(error),
    };
    match intent.try_lock_shared() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    start(Some(&data)).map(Some)
}

pub(crate) fn lease(data: &Path, name: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join(name))
}

fn shared_lease(data: &Path, name: &str) -> io::Result<File> {
    match File::open(data.join(name)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(data)?;
            lease(data, name)
        }
        result => result,
    }
}
