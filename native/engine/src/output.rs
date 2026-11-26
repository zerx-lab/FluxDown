use std::io;
use std::path::Path;

use crate::db::Db;
use crate::downloader::DownloadError;
use crate::temp_file_guard::TempFileGuard;

/// Ensure that an output directory exists, preserving the original IO kind
/// while adding the directory and operation to the error message.
pub(crate) async fn ensure_dir(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|source| with_output_context("create output directory", dir, source))
}

/// Ensure the parent directory for an output path exists.
pub(crate) async fn ensure_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    ensure_dir(parent).await
}

pub(crate) fn file_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid output file name"))
}

/// Track actual audio output, not the existence of a manifest or audio URL.
pub(crate) async fn reserve_audio_partial(
    db: &Db,
    task: &str,
    path: &Path,
    legacy_checkpoint: bool,
) -> Result<TempFileGuard, DownloadError> {
    ensure_parent(path).await?;
    let lease = TempFileGuard::acquire(path, task).await?;
    let name = file_name(path)?;
    if db.audio_partial_name(task).await? != name {
        if !legacy_checkpoint && tokio::fs::try_exists(path).await? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("unowned audio partial: {}", path.display()),
            )
            .into());
        }
        db.record_audio_partial(task, name).await?;
    }
    Ok(lease)
}

pub(crate) async fn publish_audio(
    db: &Db,
    task: &str,
    temp: &Path,
    dest: &Path,
) -> Result<(), DownloadError> {
    file_name(dest)?;
    crate::downloader::claim_rename(temp, dest).await?;
    register_audio_publication(db, task, temp, dest).await
}

pub(crate) async fn register_audio_publication(
    db: &Db,
    task: &str,
    temp: &Path,
    dest: &Path,
) -> Result<(), DownloadError> {
    let name = file_name(dest)?;
    if let Err(error) = db.record_audio_output(task, name).await {
        if let Err(rollback) = crate::downloader::claim_rename(dest, temp).await {
            crate::logger::report_error("audio-output", "roll back audio publication", &rollback);
        }
        return Err(error.into());
    }
    Ok(())
}

pub(crate) async fn remove_muxed_audio(
    db: &Db,
    task: &str,
    path: &Path,
    temporary: bool,
) -> Result<(), DownloadError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if temporary {
        db.record_audio_partial(task, "").await?;
    } else {
        db.record_audio_output(task, "").await?;
    }
    Ok(())
}

#[cfg(any(feature = "bt", test))]
/// Synchronous counterpart used by blocking protocol completion paths.
pub(crate) fn ensure_dir_sync(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
        .map_err(|source| with_output_context("create output directory", dir, source))
}

fn with_output_context(operation: &str, path: &Path, source: io::Error) -> io::Error {
    io::Error::new(
        source.kind(),
        format!("{operation} '{}': {source}", path.display()),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{Db, ensure_dir, ensure_dir_sync, ensure_parent, publish_audio};

    #[tokio::test]
    async fn publish_audio_restores_partial_if_task_no_longer_exists() {
        let root =
            std::env::temp_dir().join(format!("fluxdown-output-rollback-{}", uuid::Uuid::new_v4()));
        ensure_dir(&root).await.expect("create output directory");
        let partial = root.join("track.audio.m4a.fdownloading");
        let output = root.join("track.audio.m4a");
        let db = Db::connect("sqlite::memory:").await.expect("open database");
        tokio::fs::write(&partial, b"audio")
            .await
            .expect("write partial");

        let result = publish_audio(&db, "missing", &partial, &output).await;
        let partial_bytes = tokio::fs::read(&partial).await;
        let output_exists = output.exists();
        tokio::fs::remove_dir_all(root)
            .await
            .expect("clean test directory");

        assert!(result.is_err(), "missing task must reject registration");
        assert_eq!(partial_bytes.expect("restored partial"), b"audio");
        assert!(
            !output_exists,
            "failed publication must not leave a sidecar"
        );
    }

    #[tokio::test]
    async fn creates_missing_nested_output_directory() {
        let root =
            std::env::temp_dir().join(format!("fluxdown-output-dir-{}", uuid::Uuid::new_v4()));
        let target = root.join("中文").join("nested");
        let file = target.join("payload.bin");

        ensure_parent(&file).await.expect("create parent");
        assert!(target.is_dir());

        if let Err(error) = tokio::fs::remove_dir_all(root).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    #[test]
    fn sync_creation_is_idempotent() {
        let root =
            std::env::temp_dir().join(format!("fluxdown-output-dir-sync-{}", uuid::Uuid::new_v4()));
        ensure_dir_sync(&root).expect("create directory");
        ensure_dir_sync(&root).expect("existing directory is fine");
        assert!(root.is_dir());

        if let Err(error) = std::fs::remove_dir_all(root)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    #[tokio::test]
    async fn reports_the_directory_when_creation_fails() {
        let root =
            std::env::temp_dir().join(format!("fluxdown-output-file-{}", uuid::Uuid::new_v4()));
        std::fs::write(&root, b"occupied").expect("create blocking file");
        let child = root.join("child");

        let error = ensure_dir(&child).await.expect_err("must fail");
        let message = error.to_string();
        assert!(message.contains("create output directory"));
        assert!(message.contains(child.to_string_lossy().as_ref()));

        if let Err(error) = std::fs::remove_file(root) {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "clean test path: {error}"
            );
        }
    }
}
