//! Host-wide logging preferences, read before a process installs its logger.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Level;

pub const FILE_NAME: &str = "logging.conf";
pub const KEY: &str = "log_level";

/// Parse a persisted setting. `None` means automatic selection.
pub fn parse_setting(value: &str) -> io::Result<Option<Level>> {
    match value.trim() {
        "auto" => Ok(None),
        "error" => Ok(Some(Level::Error)),
        "warn" => Ok(Some(Level::Warn)),
        "info" => Ok(Some(Level::Info)),
        "debug" => Ok(Some(Level::Debug)),
        _ => Err(invalid_setting()),
    }
}

fn invalid_setting() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "log_level must be auto, debug, info, warn, or error",
    )
}

/// Canonical spelling for a persisted setting.
pub const fn setting_name(level: Option<Level>) -> &'static str {
    match level {
        None => "auto",
        Some(Level::Error) => "error",
        Some(Level::Warn) => "warn",
        Some(Level::Info) => "info",
        Some(Level::Debug) => "debug",
        Some(Level::Trace) => "trace",
    }
}

/// Read the setting from the data root. A missing file means automatic selection.
pub fn read_level(data_dir: &Path) -> io::Result<Option<Level>> {
    let contents = match fs::read_to_string(data_dir.join(FILE_NAME)) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let value = contents
        .trim()
        .strip_prefix("log_level=")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "expected log_level=<value>"))?;
    parse_setting(value).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Atomically replace the setting. `None` persists `auto`.
pub fn write_level(data_dir: &Path, level: Option<Level>) -> io::Result<()> {
    if level == Some(Level::Trace) {
        return Err(invalid_setting());
    }
    let value = setting_name(level);
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join(FILE_NAME);
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let (temp_path, mut temp) = loop {
        let seq = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp_path = data_dir.join(format!(".{FILE_NAME}.{}.{seq}.tmp", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => break (temp_path, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let written = writeln!(temp, "{KEY}={value}").and_then(|()| temp.sync_all());
    drop(temp);
    let result = written.and_then(|()| fs::rename(&temp_path, path));
    if let Err(error) = result {
        if let Err(cleanup) = fs::remove_file(&temp_path) {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}; failed to remove {}: {cleanup}",
                    temp_path.display()
                ),
            ));
        }
        return Err(error);
    }
    Ok(())
}

/// Build-profile default when no level was specified.
pub const fn default_level() -> Level {
    if cfg!(debug_assertions) {
        Level::Debug
    } else {
        Level::Info
    }
}

/// Resolve the level when no explicit `RUST_LOG` directives are configured.
/// Hosts handle `RUST_LOG` before calling this function, according to their sink.
pub fn resolve_level(persisted: Option<Level>) -> Level {
    resolve_level_from(
        std::env::var("FLUXDOWN_LOG_LEVEL").ok().as_deref(),
        persisted,
    )
}

/// Pure resolution for callers with an explicit environment value. `trace` is
/// supported as a temporary override, but not in the persisted preference.
pub fn resolve_level_from(env_level: Option<&str>, persisted: Option<Level>) -> Level {
    env_level
        .and_then(Level::parse)
        .or(persisted)
        .unwrap_or_else(default_level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_then_persisted_then_build_default() {
        assert_eq!(
            resolve_level_from(Some(" TrAcE "), Some(Level::Error)),
            Level::Trace
        );
        assert_eq!(
            resolve_level_from(Some("bogus"), Some(Level::Warn)),
            Level::Warn
        );
        assert_eq!(resolve_level_from(None, None), default_level());
    }

    #[test]
    fn config_round_trip_and_validation() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown-logging-conf-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        fs::create_dir(&dir)?;
        let result = (|| {
            assert_eq!(read_level(&dir)?, None);
            write_level(&dir, Some(Level::Warn))?;
            assert_eq!(read_level(&dir)?, Some(Level::Warn));
            assert!(write_level(&dir, Some(Level::Trace)).is_err());
            assert_eq!(read_level(&dir)?, Some(Level::Warn));
            write_level(&dir, None)?;
            assert_eq!(read_level(&dir)?, None);
            fs::write(dir.join(FILE_NAME), "log_level=debug\nlog_level=info\n")?;
            assert_eq!(
                read_level(&dir).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            Ok(())
        })();
        let cleanup = fs::remove_dir_all(&dir);
        result.and(cleanup)
    }
}
