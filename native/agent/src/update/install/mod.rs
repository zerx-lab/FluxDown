//! 更新安装：按安装形态检测 → 预检 → 替换程序文件 → 返回重启计划。
//!
//! 流程由 [`super`]（`UpdateService`）驱动：
//! 1. [`detect`]：启动时一次，只读环境，得出安装形态 / 资产键 / 不可自更新原因；
//! 2. [`preflight`]：确认有新版本后再探测目录可写、提权可用、macOS 签名团队等；
//! 3. [`apply`]：对已校验的更新包做替换（旧进程仍在运行），失败则回滚、原程序保持可用；
//! 4. [`cleanup_leftovers`]：下次启动清理改名让位的旧文件。
//!
//! 各形态的具体行为见同目录的 `windows.rs` / `macos.rs` / `linux.rs` / `server.rs`。
#![cfg_attr(
    not(any(windows, target_os = "linux", target_os = "macos")),
    allow(dead_code)
)]

mod archive;
mod detect;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod replace;
mod server;
#[cfg(test)]
mod test_support;
#[cfg(windows)]
mod windows;

use std::io;
use std::path::{Path, PathBuf};

use fluxdown_protocol::{UpdateFailure, UpdateInstallKind, UpdateManualReason};

use super::restart::RestartPlan;

/// 程序文件名（随平台带扩展名）。
const AGENT_BIN: &str = if cfg!(windows) {
    "fluxdown-agent.exe"
} else {
    "fluxdown-agent"
};
const DAEMON_BIN: &str = if cfg!(windows) {
    "fluxdownd.exe"
} else {
    "fluxdownd"
};
#[cfg(any(windows, target_os = "linux"))]
const DESKTOP_BIN: &str = if cfg!(windows) {
    "fluxdown-desktop.exe"
} else {
    "fluxdown-desktop"
};

/// `/api/release` 的资产来自桌面包还是服务器包（同时决定校验和哨兵名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReleaseComponent {
    Desktop,
    Server,
}

#[derive(Clone, Debug)]
pub(crate) struct InstallTarget {
    pub kind: UpdateInstallKind,
    /// `/api/release` 中取顶层 `assets`（Desktop）还是 `server.assets`（Server）。
    pub component: ReleaseComponent,
    /// 资产键，按优先级（如 `["macos_dmg_arm64", "macos_tarball_arm64"]`）；空 = 本平台无资产。
    pub asset_keys: Vec<&'static str>,
    /// 无需触盘即可确定的不可自更新原因。
    pub manual_reason: Option<UpdateManualReason>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InstallError {
    /// 更新包本身不可用：格式不符、含不安全条目、缺必需文件、签名团队不一致。
    #[error("invalid update package: {0}")]
    InvalidPackage(String),
    /// 暂存 / 解压阶段写盘失败（空间、权限）。
    #[error("{context}: {source}")]
    Storage {
        context: String,
        #[source]
        source: io::Error,
    },
    /// 替换程序文件失败（已回滚；回滚不完整时 `context` 里有说明）。
    #[error("{context}: {source}")]
    Replace {
        context: String,
        #[source]
        source: io::Error,
    },
    /// 当前安装形态没有应用内安装路径。
    #[error("in-app install is not supported for {0:?}")]
    Unsupported(UpdateInstallKind),
    /// 后台任务异常终止。
    #[error("install task failed: {0}")]
    Task(String),
    /// 系统工具（hdiutil / ditto / codesign / apt-get / pacman …）失败。
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[error("{0}")]
    Command(String),
    /// 用户取消了管理员授权。
    #[cfg(target_os = "linux")]
    #[error("administrator authorization was cancelled")]
    ElevationCancelled,
    /// 无法请求管理员授权。
    #[cfg(target_os = "linux")]
    #[error("cannot request administrator authorization: {0}")]
    ElevationUnavailable(String),
}

impl InstallError {
    pub(crate) fn failure(&self) -> UpdateFailure {
        match self {
            Self::InvalidPackage(_) => UpdateFailure::Verify,
            Self::Storage { .. } => UpdateFailure::Storage,
            Self::Replace { .. } | Self::Unsupported(_) | Self::Task(_) => UpdateFailure::Install,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Command(_) => UpdateFailure::Install,
            #[cfg(target_os = "linux")]
            Self::ElevationCancelled => UpdateFailure::ElevationCancelled,
            #[cfg(target_os = "linux")]
            Self::ElevationUnavailable(_) => UpdateFailure::Install,
        }
    }

    /// 暂存 / 解压阶段的 I/O 错误适配器：`.map_err(InstallError::storage("write x"))`。
    pub(super) fn storage(context: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        let context = context.into();
        move |source| Self::Storage { context, source }
    }

    /// 替换阶段的 I/O 错误适配器。
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn replace(context: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        let context = context.into();
        move |source| Self::Replace { context, source }
    }
}

/// 启动时调用一次并缓存；只读环境 / 路径 / 轻量命令，不写盘。
pub(crate) fn detect(server_mode: bool) -> InstallTarget {
    detect::detect(server_mode)
}

/// 确认有可安装的新版本后调用：目录可写探测、提权 / 图形会话可用性、macOS 签名团队等。
/// 返回 `Some` 表示只能手动更新。
pub(crate) async fn preflight(target: &InstallTarget) -> Option<UpdateManualReason> {
    if let Some(reason) = target.manual_reason {
        return Some(reason);
    }
    let kind = target.kind;
    #[cfg(target_os = "linux")]
    if matches!(
        kind,
        UpdateInstallKind::LinuxDeb | UpdateInstallKind::LinuxArch
    ) {
        return linux::elevation_unavailable();
    }
    let Some(dir) = install_dir(kind) else {
        return Some(UpdateManualReason::Unsupported);
    };
    let writable = match tokio::task::spawn_blocking(move || dir_writable(&dir)).await {
        Ok(writable) => writable,
        Err(error) => {
            tracing::warn!(%error, "install directory probe task failed");
            false
        }
    };
    if !writable {
        return Some(UpdateManualReason::NotWritable);
    }
    #[cfg(target_os = "macos")]
    if kind == UpdateInstallKind::MacosApp {
        return macos::preflight().await;
    }
    None
}

/// 应用已校验的更新包（`package` 文件名即发布资产名）。`work_dir` 为该版本私有的空暂存目录。
///
/// 成功：程序文件已替换（Windows 安装版除外：安装器在退出后运行），返回待执行的重启计划；
/// 失败：已回滚，原程序保持可用。
pub(crate) async fn apply(
    target: &InstallTarget,
    package: &Path,
    work_dir: &Path,
) -> Result<RestartPlan, InstallError> {
    let package = std::path::absolute(package).map_err(InstallError::storage(format!(
        "resolve {}",
        package.display()
    )))?;
    let work_dir = std::path::absolute(work_dir).map_err(InstallError::storage(format!(
        "resolve {}",
        work_dir.display()
    )))?;
    tracing::info!(kind = ?target.kind, package = %package.display(), "applying update package");
    match target.kind {
        #[cfg(windows)]
        UpdateInstallKind::WindowsSetup => {
            blocking(move || windows::apply_setup(&package, &work_dir)).await
        }
        #[cfg(any(windows, target_os = "linux"))]
        UpdateInstallKind::WindowsPortable | UpdateInstallKind::LinuxPortable => {
            blocking(move || apply_portable(&package, &work_dir)).await
        }
        #[cfg(target_os = "linux")]
        UpdateInstallKind::LinuxAppImage => blocking(move || linux::apply_appimage(&package)).await,
        #[cfg(target_os = "linux")]
        kind @ (UpdateInstallKind::LinuxDeb | UpdateInstallKind::LinuxArch) => {
            linux::apply_package(kind, &package).await
        }
        #[cfg(target_os = "macos")]
        UpdateInstallKind::MacosApp => macos::apply(&package, &work_dir).await,
        UpdateInstallKind::ServerBinary => {
            blocking(move || server::apply(&package, &work_dir)).await
        }
        kind => Err(InstallError::Unsupported(kind)),
    }
}

/// 清理上次替换留下的旧文件：安装目录里的 `*.fluxdown-old`、macOS bundle 旁的
/// `.<Name>.app.old-*` / 暂存目录、AppImage 旁的临时副本。启动时调用，失败只记 debug 日志。
pub(crate) fn cleanup_leftovers() {
    let Some(exe) = detect::current_exe_path() else {
        return;
    };
    #[cfg(target_os = "macos")]
    if let Some(parent) = detect::outer_app_bundle(&exe)
        .as_deref()
        .and_then(Path::parent)
    {
        cleanup_dir(parent, 0, &is_macos_leftover);
        return;
    }
    if let Some(dir) = exe.parent() {
        cleanup_dir(dir, 2, &|name| name.ends_with(replace::ASIDE_SUFFIX));
    }
    if let Some(dir) = detect::appimage_path().as_deref().and_then(Path::parent) {
        cleanup_dir(dir, 0, &|name| {
            name.starts_with('.') && name.contains(".fluxdown-new-")
        });
    }
}

#[cfg(target_os = "macos")]
fn is_macos_leftover(name: &str) -> bool {
    name.starts_with('.') && (name.contains(".app.old-") || name.starts_with(".fluxdown-staging-"))
        || name.ends_with(replace::ASIDE_SUFFIX)
}

/// 删除 `dir` 下名字命中 `is_leftover` 的条目（含 `depth` 层子目录）；不进入受保护目录，不跟随符号链接。
fn cleanup_dir(dir: &Path, depth: u32, is_leftover: &dyn Fn(&str) -> bool) {
    /// 单次清理最多检查的条目数：安装目录很小，防止误落在巨大目录里时长时间遍历。
    const MAX_ENTRIES: usize = 4096;

    let mut pending = vec![(dir.to_path_buf(), depth)];
    let mut seen = 0_usize;
    while let Some((current, remaining)) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::debug!(dir = %current.display(), %error, "cannot scan for update leftovers");
                continue;
            }
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if is_leftover(name) {
                let removed = if file_type.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                match removed {
                    Ok(()) => tracing::debug!(path = %path.display(), "removed update leftover"),
                    Err(error) => {
                        tracing::debug!(path = %path.display(), %error, "could not remove update leftover");
                    }
                }
            } else if file_type.is_dir() && remaining > 0 && name != replace::PROTECTED_DIR {
                pending.push((path, remaining - 1));
            }
        }
    }
}

/// 安装位置（用于可写探测）：AppImage 文件所在目录、macOS 外层 bundle 的父目录，其余为程序所在目录。
fn install_dir(kind: UpdateInstallKind) -> Option<PathBuf> {
    if kind == UpdateInstallKind::LinuxAppImage {
        return detect::appimage_path()?.parent().map(Path::to_path_buf);
    }
    let exe = detect::current_exe_path()?;
    #[cfg(target_os = "macos")]
    if kind == UpdateInstallKind::MacosApp {
        return detect::outer_app_bundle(&exe)?
            .parent()
            .map(Path::to_path_buf);
    }
    exe.parent().map(Path::to_path_buf)
}

/// 创建并删除一个探测文件：比权限位 / `readonly` 属性更可靠（ACL、只读卷、沙箱都会如实失败）。
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(
        ".fluxdown-write-probe-{}",
        uuid::Uuid::new_v4().simple()
    ));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            if let Err(error) = std::fs::remove_file(&probe) {
                tracing::debug!(path = %probe.display(), %error, "could not remove write probe");
            }
            true
        }
        Err(error) => {
            tracing::debug!(dir = %dir.display(), %error, "install directory is not writable");
            false
        }
    }
}

async fn blocking<T, F>(work: F) -> Result<T, InstallError>
where
    F: FnOnce() -> Result<T, InstallError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| InstallError::Task(error.to_string()))?
}

/// 便携形态（Windows 便携版 / Linux tarball）：解压并替换程序目录里包内存在的文件，
/// 然后重启桌面。
#[cfg(any(windows, target_os = "linux"))]
fn apply_portable(package: &Path, work_dir: &Path) -> Result<RestartPlan, InstallError> {
    let exe = current_exe_and_dir()?;
    replace::apply_archive(
        package,
        work_dir,
        &exe.1,
        replace::Selection::All {
            required: &[AGENT_BIN],
        },
    )?;
    Ok(desktop_relaunch(&exe.0))
}

/// 桌面重启计划：同目录 `fluxdown-desktop[.exe] --after-update`。
#[cfg(any(windows, target_os = "linux"))]
fn desktop_relaunch(agent_exe: &Path) -> RestartPlan {
    RestartPlan::desktop(
        agent_exe.with_file_name(DESKTOP_BIN),
        vec![super::restart::AFTER_UPDATE_ARG.into()],
    )
}

/// 替换**之前**取得的当前程序路径与目录（Linux 上替换后 `/proc/self/exe` 指向让位的旧文件）。
fn current_exe_and_dir() -> Result<(PathBuf, PathBuf), InstallError> {
    let exe = detect::current_exe_path().ok_or_else(|| InstallError::Storage {
        context: "locate the running program".to_owned(),
        source: io::Error::new(io::ErrorKind::NotFound, "current executable unknown"),
    })?;
    let dir = exe
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| InstallError::Storage {
            context: format!("locate the directory of {}", exe.display()),
            source: io::Error::new(io::ErrorKind::NotFound, "no parent directory"),
        })?;
    Ok((exe, dir))
}

/// 运行外部工具并等待结束（带超时与 `kill_on_drop`）；调用方自行判断退出码。
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn run_tool<I, S>(
    program: &Path,
    args: I,
    timeout: std::time::Duration,
) -> Result<std::process::Output, InstallError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(InstallError::Command(format!(
            "{}: {error}",
            program.display()
        ))),
        Err(_) => Err(InstallError::Command(format!(
            "{}: no result within {}s",
            program.display(),
            timeout.as_secs()
        ))),
    }
}

/// 退出码 0 视为成功，否则把 stderr 末尾带进错误。
#[cfg(target_os = "macos")]
fn ensure_success(program: &Path, output: &std::process::Output) -> Result<(), InstallError> {
    if output.status.success() {
        return Ok(());
    }
    Err(InstallError::Command(format!(
        "{} failed ({}): {}",
        program.display(),
        output.status,
        stderr_tail(&output.stderr)
    )))
}

#[cfg(target_os = "macos")]
fn stderr_tail(stderr: &[u8]) -> String {
    const MAX_CHARS: usize = 600;
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    let skip = text.chars().count().saturating_sub(MAX_CHARS);
    text.chars().skip(skip).collect()
}
