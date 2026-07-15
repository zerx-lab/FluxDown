//! Application data directory resolution.
//!
//! Determines where FluxDown stores persistent data (database, logs, NMH manifests).
//!
//! ## Strategy
//!
//! | Platform        | Mode      | Directory                                      |
//! |-----------------|-----------|-------------------------------------------------|
//! | Windows         | 便携版     | `<exe_dir>/portable_data/`                      |
//! | Windows         | 安装版     | `%LOCALAPPDATA%\FluxDown\`                      |
//! | Linux           | —          | `$XDG_DATA_HOME/fluxdown/`                      |
//! | macOS           | —          | `~/Library/Application Support/fluxdown/`        |
//! | Android         | —          | `/data/data/<package>/files/fluxdown/`           |
//!
//! ### 便携模式检测（仅 Windows）
//!
//! exe 同目录下存在 `portable` 标记文件即视为便携模式。
//! 与 `updater.rs` 和 Dart 侧 `isPortableMode()` 保持一致。
//!
//! ### 便携数据迁移（≤ v0.2.x → v0.3+）
//!
//! v0.3 以前，便携数据直接散落在 `<exe_dir>/` 根层（与 exe/DLL 混在一起）。
//! 升级后首次启动时，旧文件自动迁移到 `portable_data/` 子目录：
//!
//! - 迁移幂等——目标已存在则跳过，进程内至多执行一次；
//! - SQLite 三件套（`flux_down.db` / `-wal` / `-shm`）作为原子组迁移，
//!   WAL 持有未 checkpoint 的事务，绝不与主库分离；
//! - 失败的条目原地保留并记录到 `<portable_data>/migration_errors.log`
//!   （GUI 进程无可见 stderr），下次启动自动重试。

use std::path::{Path, PathBuf};

/// Marker file name — a zero-byte file placed next to the exe by the portable
/// ZIP distribution.  Matches `updater::PORTABLE_MARKER` and the Dart-side
/// `_portableMarker` constant.
#[cfg(target_os = "windows")]
const PORTABLE_MARKER: &str = "portable";

/// Errors that can occur while resolving the application data directory.
#[derive(Debug, thiserror::Error)]
pub enum DataDirError {
    /// Failed to create the resolved directory (or one of its ancestors).
    #[error("failed to create data directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Resolve the application data directory (for DB, logs, NMH manifests, etc.).
///
/// `explicit` overrides auto-detection when set (e.g. a CLI `--data-dir` flag
/// or a Server per-tenant directory); pass `None` to fall back to the
/// platform-specific auto-detection below (portable marker / `LOCALAPPDATA` /
/// XDG / macOS Application Support).
///
/// The returned path is guaranteed to exist (created if necessary).
///
/// # Examples
///
/// ```
/// use fluxdown_engine::data_dir::resolve_data_dir;
///
/// // Auto-detect the platform data directory.
/// let dir = resolve_data_dir(None).expect("data dir should be creatable");
/// assert!(dir.is_absolute() || dir.as_os_str() == ".");
/// ```
pub fn resolve_data_dir(explicit: Option<&Path>) -> Result<PathBuf, DataDirError> {
    let dir = match explicit {
        Some(path) => path.to_path_buf(),
        None => resolve_data_dir_inner(),
    };
    std::fs::create_dir_all(&dir).map_err(|source| DataDirError::CreateDir {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

fn resolve_data_dir_inner() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".local").join("share")
            });
        base.join("fluxdown")
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("fluxdown")
    }

    #[cfg(target_os = "windows")]
    {
        if is_portable() {
            let root = exe_dir();
            let new_dir = root.join(PORTABLE_DATA_DIR);
            migrate_portable_data(&root, &new_dir);
            return new_dir;
        }
        // Installed mode: use %LOCALAPPDATA%\FluxDown (always user-writable).
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("FluxDown");
        }
        // Fallback: %APPDATA%\FluxDown
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("FluxDown");
        }
        // Last resort: exe directory (may fail on write, but better than ".").
        exe_dir()
    }

    // Android: 应用内部存储 `/data/data/<package>/files/fluxdown`。
    // 包名 = 进程名（`/proc/self/cmdline` 首个 NUL 之前的内容）。
    // 该目录无需任何存储权限即可读写，与 Dart 侧 `resolveDataDir()` 保持一致。
    #[cfg(target_os = "android")]
    {
        match android_package_name() {
            Some(pkg) => PathBuf::from(format!("/data/data/{pkg}/files/fluxdown")),
            None => exe_dir(),
        }
    }

    // Catch-all for other platforms (e.g. iOS stubs) — should never
    // be reached in practice.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows",
        target_os = "android"
    )))]
    {
        exe_dir()
    }
}

/// Android：从 `/proc/self/cmdline` 读取当前进程名（= 应用包名）。
/// 进程名可能带 `:subprocess` 后缀，取冒号前部分。
///
/// 供宿主（hub）拼接应用专属外部目录等 Android 路径使用。
///
/// # Examples
///
/// ```ignore
/// // 仅在 Android 目标上可用
/// if let Some(pkg) = fluxdown_engine::data_dir::android_package_name() {
///     let dir = format!("/storage/emulated/0/Android/data/{pkg}/files/Download");
/// }
/// ```
#[cfg(target_os = "android")]
pub fn android_package_name() -> Option<String> {
    let raw = std::fs::read("/proc/self/cmdline").ok()?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let name = std::str::from_utf8(&raw[..end]).ok()?;
    let name = name.split(':').next().unwrap_or(name).trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// 便携数据子目录名，位于 exe 所在目录内。
#[cfg(target_os = "windows")]
const PORTABLE_DATA_DIR: &str = "portable_data";

/// Windows portable detection: `portable` marker file exists next to the exe.
#[cfg(target_os = "windows")]
fn is_portable() -> bool {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        return dir.join(PORTABLE_MARKER).exists();
    }
    false
}

/// SQLite 主库文件名；`-wal` / `-shm` 为其伴生文件（见 [`migrate_db_group`]）。
#[cfg(any(target_os = "windows", test))]
const DB_FILE: &str = "flux_down.db";
#[cfg(any(target_os = "windows", test))]
const DB_WAL: &str = "flux_down.db-wal";
#[cfg(any(target_os = "windows", test))]
const DB_SHM: &str = "flux_down.db-shm";

/// 独立迁移项（不含 DB 三件套——那组走 [`migrate_db_group`] 原子迁移）。
// KEEP IN SYNC with lib/src/services/platform_utils.dart knownItems
#[cfg(any(target_os = "windows", test))]
const KNOWN_ITEMS: &[&str] = &[
    "settings.json",
    "logs",
    "icons",
    "bt_session",
    "plugins",
    "plugins-work",
    "bin",
];

/// 触发旧版便携布局（≤ v0.2.x，数据散落 exe 根层）→ `portable_data/` 的
/// 一次性迁移（`Once` 保证进程内至多执行一次）。
///
/// GUI 路径下 Dart 侧 `migratePortableData` 先行执行（`LogService` 初始化
/// 早于 `initializeRust`），故本函数通常为 no-op；其主要价值在 CLI
/// （`native/cli`）与 headless server 等纯 Rust 入口路径。
///
/// 失败处理：条目原地保留、写 stderr 并落盘
/// `<new_dir>/migration_errors.log`（GUI 进程 stderr 不可见，且文件 logger
/// 尚未初始化——`logs/` 目录本身就是迁移目标之一），下次启动自动重试。
#[cfg(target_os = "windows")]
fn migrate_portable_data(old_root: &Path, new_dir: &Path) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let failures = migrate_portable_layout(old_root, new_dir);
        if failures.is_empty() {
            return;
        }
        for msg in &failures {
            eprintln!("[便携迁移] {msg}");
        }
        persist_migration_failures(new_dir, &failures);
    });
}

/// 执行旧便携布局 → `portable_data/` 的迁移，返回失败描述列表（空 = 全部成功）。
///
/// 幂等：目标已存在的条目跳过。迁移通过 `rename` 完成，成功后源文件即被移除。
///
/// 并发语义：旧版本进程仍在运行（DB 句柄未释放）时 `rename` 因共享冲突失败，
/// 条目原地保留并记录，等下次启动重试；两个新版进程并发迁移无害——`rename`
/// 原子，输家仅多一条失败记录。
#[cfg(any(target_os = "windows", test))]
fn migrate_portable_layout(old_root: &Path, new_dir: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    if let Err(e) = std::fs::create_dir_all(new_dir) {
        failures.push(format!("创建目录失败 {}: {e}", new_dir.display()));
        return failures;
    }
    migrate_db_group(old_root, new_dir, &mut failures);
    for name in KNOWN_ITEMS {
        let old_path = old_root.join(name);
        let new_path = new_dir.join(name);
        if old_path.exists()
            && !new_path.exists()
            && let Err(e) = std::fs::rename(&old_path, &new_path)
        {
            failures.push(format!(
                "移动失败 {} → {}: {e}",
                old_path.display(),
                new_path.display()
            ));
        }
    }
    failures
}

/// SQLite 三件套（主库 / WAL / SHM）原子组迁移。
///
/// WAL 持有未 checkpoint 的最近事务，必须与主库同进退，否则新目录里的
/// 主库会静默丢失最近提交：
///
/// - 旧主库不存在（含孤儿 WAL）或新主库已存在 → 整组跳过，绝不单独搬 WAL；
/// - 主库 rename 失败 → 整组放弃；
/// - WAL rename 失败 → 已移动的主库回滚回原位，下次启动重试整组；
/// - SHM 是共享内存索引，SQLite 会按需重建——失败仅记录、不回滚。
#[cfg(any(target_os = "windows", test))]
fn migrate_db_group(old_root: &Path, new_dir: &Path, failures: &mut Vec<String>) {
    let old_db = old_root.join(DB_FILE);
    let new_db = new_dir.join(DB_FILE);
    if !old_db.exists() || new_db.exists() {
        return;
    }
    if let Err(e) = std::fs::rename(&old_db, &new_db) {
        failures.push(format!(
            "移动失败 {} → {}: {e}",
            old_db.display(),
            new_db.display()
        ));
        return;
    }
    let old_wal = old_root.join(DB_WAL);
    let new_wal = new_dir.join(DB_WAL);
    if old_wal.exists()
        && let Err(e) = std::fs::rename(&old_wal, &new_wal)
    {
        failures.push(format!(
            "移动失败 {} → {}: {e}",
            old_wal.display(),
            new_wal.display()
        ));
        // WAL 搬不动 → 主库回滚，保持三件套同处一地。
        if let Err(e) = std::fs::rename(&new_db, &old_db) {
            failures.push(format!(
                "回滚失败 {} → {}: {e}",
                new_db.display(),
                old_db.display()
            ));
        }
        return;
    }
    let old_shm = old_root.join(DB_SHM);
    let new_shm = new_dir.join(DB_SHM);
    if old_shm.exists()
        && !new_shm.exists()
        && let Err(e) = std::fs::rename(&old_shm, &new_shm)
    {
        failures.push(format!(
            "移动失败 {} → {}: {e}",
            old_shm.display(),
            new_shm.display()
        ));
    }
}

/// 迁移失败信息落盘：`<new_dir>/migration_errors.log`（追加）。
///
/// 放数据目录根层而非 `logs/`——迁移失败时若在此处预创建 `logs/` 目录，
/// 会让下次启动误判 `logs` 已迁移而永久跳过它。
#[cfg(any(target_os = "windows", test))]
fn persist_migration_failures(new_dir: &Path, failures: &[String]) {
    use std::io::Write;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(new_dir.join("migration_errors.log"))
    else {
        return;
    };
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    for msg in failures {
        let _ = writeln!(file, "{ts} [便携迁移] {msg}");
    }
}

/// Returns the exe's parent directory, falling back to CWD or ".".
#[allow(dead_code)]
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        DB_FILE, DB_SHM, DB_WAL, migrate_db_group, migrate_portable_layout,
        persist_migration_failures,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    fn fresh_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_portable_migrate_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn migrates_known_items_and_db_group() {
        let root = fresh_root("fresh");
        let new_dir = root.join("portable_data");
        write(&root.join(DB_FILE), "db");
        write(&root.join(DB_WAL), "wal");
        write(&root.join(DB_SHM), "shm");
        write(&root.join("settings.json"), "{}");
        write(&root.join("icons").join("custom_icon.ico"), "ico");
        write(&root.join("logs").join("fluxdown_2026-01-01.log"), "log");
        // exe 根层的非数据文件（exe/DLL）不在清单内，必须原地保留。
        write(&root.join("flux_down.exe"), "bin");

        let failures = migrate_portable_layout(&root, &new_dir);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(fs::read_to_string(new_dir.join(DB_FILE)).unwrap(), "db");
        assert_eq!(fs::read_to_string(new_dir.join(DB_WAL)).unwrap(), "wal");
        assert_eq!(fs::read_to_string(new_dir.join(DB_SHM)).unwrap(), "shm");
        assert!(new_dir.join("icons").join("custom_icon.ico").exists());
        assert!(
            new_dir
                .join("logs")
                .join("fluxdown_2026-01-01.log")
                .exists()
        );
        assert!(!root.join(DB_FILE).exists());
        assert!(!root.join("icons").exists());
        assert!(root.join("flux_down.exe").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn second_run_is_noop() {
        let root = fresh_root("idempotent");
        let new_dir = root.join("portable_data");
        write(&root.join(DB_FILE), "db");
        write(&root.join("settings.json"), "{}");
        assert!(migrate_portable_layout(&root, &new_dir).is_empty());
        let failures = migrate_portable_layout(&root, &new_dir);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(fs::read_to_string(new_dir.join(DB_FILE)).unwrap(), "db");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_target_is_never_overwritten() {
        let root = fresh_root("keep_target");
        let new_dir = root.join("portable_data");
        write(&root.join("settings.json"), "old");
        write(&new_dir.join("settings.json"), "new");
        let failures = migrate_portable_layout(&root, &new_dir);
        assert!(failures.is_empty(), "{failures:?}");
        // 新数据保留，旧文件原地不动。
        assert_eq!(
            fs::read_to_string(new_dir.join("settings.json")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(root.join("settings.json")).unwrap(),
            "old"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn orphan_wal_is_never_moved_alone() {
        let root = fresh_root("orphan_wal");
        let new_dir = root.join("portable_data");
        fs::create_dir_all(&new_dir).unwrap();
        write(&root.join(DB_WAL), "wal");
        let mut failures = Vec::new();
        migrate_db_group(&root, &new_dir, &mut failures);
        assert!(failures.is_empty(), "{failures:?}");
        assert!(root.join(DB_WAL).exists());
        assert!(!new_dir.join(DB_WAL).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn db_group_skipped_when_new_db_exists() {
        let root = fresh_root("group_skip");
        let new_dir = root.join("portable_data");
        write(&root.join(DB_FILE), "old-db");
        write(&root.join(DB_WAL), "old-wal");
        write(&new_dir.join(DB_FILE), "new-db");
        let mut failures = Vec::new();
        migrate_db_group(&root, &new_dir, &mut failures);
        assert!(failures.is_empty(), "{failures:?}");
        // 新主库不被覆盖，旧三件套原地保留——绝不把旧 WAL 混到新主库旁。
        assert_eq!(fs::read_to_string(new_dir.join(DB_FILE)).unwrap(), "new-db");
        assert!(root.join(DB_FILE).exists());
        assert!(root.join(DB_WAL).exists());
        assert!(!new_dir.join(DB_WAL).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failures_are_persisted_to_data_dir_root() {
        let root = fresh_root("persist");
        let new_dir = root.join("portable_data");
        fs::create_dir_all(&new_dir).unwrap();
        persist_migration_failures(&new_dir, &["boom".to_string()]);
        let content = fs::read_to_string(new_dir.join("migration_errors.log")).unwrap();
        assert!(content.contains("[便携迁移] boom"), "{content}");
        // 不得预创建 logs/ 目录（会让下次启动误判 logs 已迁移）。
        assert!(!new_dir.join("logs").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
