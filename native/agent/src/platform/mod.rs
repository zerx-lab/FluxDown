//! agent 桌面系统集成：任务文件打开/定位/复制到剪贴板、官方桌面进程唤起、开机自启、
//! `.torrent` 关联与 URL scheme 注册、系统文件图标提取（见 [`file_icon_png`]）。
//!
//! 关联与 URL scheme 的注册目标是官方桌面程序：Windows 指向同级
//! `fluxdown-desktop.exe`，macOS 指向外层 `FluxDown.app` bundle（见
//! [`host_bundle_id`]），Linux 指向打包的 `com.fluxdown.app.desktop`。开机自启的目标是
//! agent 自身（`--autostart`），由 agent 按托盘偏好决定是否再拉起桌面程序。全部函数
//! 同步阻塞，RPC 侧需放入 `spawn_blocking`。
//!
//! macOS 打包布局：agent 与 `fluxdownd` 位于辅助 bundle
//! `FluxDown.app/Contents/Helpers/FluxDownAgent.app/Contents/MacOS/`（其 Info.plist 声明
//! `LSUIElement`，常驻时不占 Dock），桌面程序位于外层 `FluxDown.app/Contents/MacOS/`。
//! 不在该布局内（开发期 `target/release` 平铺）时按同级目录解析。

#[cfg(feature = "desktop")]
pub(crate) mod app_icon;
mod autostart;
#[cfg(feature = "desktop")]
mod clipboard_files;
#[cfg(feature = "desktop")]
mod clipboard_seq;
#[cfg(windows)]
pub mod elevation;
mod file_association;
mod file_icon;
#[cfg(feature = "desktop")]
pub(crate) mod hotkey;
#[cfg(target_os = "macos")]
mod macos_cf;
mod protocol_registry;
#[cfg(windows)]
mod windows_com;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use fluxdown_protocol::PlatformIntegrationDto;

#[cfg(feature = "desktop")]
pub use clipboard_seq::clipboard_change_count;
pub use file_icon::file_icon_png;

/// 自启动条目传给 agent 的参数。
pub const AUTOSTART_ARG: &str = "--autostart";

const DESKTOP_EXECUTABLE_NAME: &str = if cfg!(windows) {
    "fluxdown-desktop.exe"
} else {
    "fluxdown-desktop"
};

/// 官方桌面程序：macOS 辅助 bundle 布局下取外层 `Contents/MacOS/`，否则取 agent 同级；
/// 文件不存在时返回 `None`。
#[must_use]
pub fn desktop_executable() -> Option<PathBuf> {
    let agent = std::env::current_exe().ok()?;
    #[cfg(target_os = "macos")]
    if let Some(host_dir) = host_macos_dir(&agent) {
        let path = host_dir.join(DESKTOP_EXECUTABLE_NAME);
        return path.is_file().then_some(path);
    }
    let path = agent.with_file_name(DESKTOP_EXECUTABLE_NAME);
    path.is_file().then_some(path)
}

/// 辅助 bundle 的 bundle id = 外层 bundle id + 此后缀（打包脚本
/// `scripts/package_gpui_macos.sh` 按此写入辅助 Info.plist）。
#[cfg(target_os = "macos")]
const HELPER_BUNDLE_ID_SUFFIX: &str = ".agent";

/// agent 位于 `<Host>.app/Contents/Helpers/<Helper>.app/Contents/MacOS/` 时返回外层
/// `<Host>.app/Contents/MacOS`；其他位置返回 `None`。
#[cfg(target_os = "macos")]
fn host_macos_dir(agent_exe: &Path) -> Option<PathBuf> {
    let macos_dir = agent_exe.parent()?;
    if !macos_dir.ends_with("Contents/MacOS") {
        return None;
    }
    let helper_app = macos_dir.parent()?.parent()?;
    if helper_app.extension()? != "app" {
        return None;
    }
    let helpers = helper_app.parent()?;
    let contents = helpers.parent()?;
    if helpers.file_name()? != "Helpers" || contents.file_name()? != "Contents" {
        return None;
    }
    if contents.parent()?.extension()? != "app" {
        return None;
    }
    Some(contents.join("MacOS"))
}

/// agent 位于辅助 bundle 布局时的外层 `<Host>.app`（用 `open -n` 重新拉起整个应用）；
/// 其他位置（如未打包的开发布局）返回 `None`。
#[cfg(target_os = "macos")]
pub(crate) fn host_app_bundle() -> Option<PathBuf> {
    let agent = std::env::current_exe().ok()?;
    let contents = host_macos_dir(&agent)?.parent()?.to_path_buf();
    contents.parent().map(Path::to_path_buf)
}

/// `.torrent` 关联与 URL scheme 的注册目标：外层 `FluxDown.app` 的 bundle id。
///
/// 辅助 bundle 内由 Core Foundation 解析出的是辅助 bundle 自身的 id，按约定去掉
/// [`HELPER_BUNDLE_ID_SUFFIX`] 得到外层 id；后缀不符说明打包错误，按不支持处理，
/// 避免把关联登记到不声明 UTI / scheme 的辅助 bundle 上。
#[cfg(target_os = "macos")]
pub(crate) fn host_bundle_id() -> Option<String> {
    let own = macos_cf::main_bundle_id()?;
    let in_helper = std::env::current_exe()
        .ok()
        .is_some_and(|exe| host_macos_dir(&exe).is_some());
    host_id_from(own, in_helper)
}

#[cfg(target_os = "macos")]
fn host_id_from(own: String, in_helper: bool) -> Option<String> {
    if !in_helper {
        return Some(own);
    }
    own.strip_suffix(HELPER_BUNDLE_ID_SUFFIX)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
}

/// 释放关联时的接手程序：候选中第一个不是 FluxDown（`mine`）的 bundle id。
///
/// Launch Services 没有「无默认处理程序」状态，设空 bundle id 只会让系统回落到剩余候选；
/// FluxDown 是唯一候选时返回 `None`，此时系统层面无法让出，由关联 opt-out 在捕获入口拦截。
#[cfg(target_os = "macos")]
fn successor_handler(candidates: Vec<String>, mine: &str) -> Option<String> {
    candidates
        .into_iter()
        .find(|id| !id.is_empty() && !id.eq_ignore_ascii_case(mine))
}

/// 引擎下载中临时文件后缀（`fluxdown_engine::downloader::TEMP_EXT`）；agent 不依赖引擎，
/// 此处镜像同一字面量。
pub(crate) const DOWNLOADING_SUFFIX: &str = ".fdownloading";

/// 用系统默认程序打开任务产物；最终文件尚不存在（下载中 / 暂停 / 已被移走）时报错，
/// 不交给系统命令静默失败。
pub fn open_task(task: &fluxdown_protocol::TaskDto) -> Result<(), PlatformError> {
    open_task_file(Path::new(&task.save_dir), &task.file_name)
}

fn open_task_file(save_dir: &Path, file_name: &str) -> Result<(), PlatformError> {
    let path = save_dir.join(file_name);
    if file_name.is_empty() || !path.exists() {
        return Err(PlatformError::NotFound(path));
    }
    launch_path(&path, false)
}

/// 在文件管理器中定位任务；目标见 [`reveal_target`]。
pub fn reveal_task(task: &fluxdown_protocol::TaskDto) -> Result<(), PlatformError> {
    reveal_task_file(Path::new(&task.save_dir), &task.file_name)
}

fn reveal_task_file(save_dir: &Path, file_name: &str) -> Result<(), PlatformError> {
    let (path, reveal) = reveal_target(save_dir, file_name)?;
    launch_path(&path, reveal)
}

/// 任务状态码「已完成」（`TaskDto::status`：0=pending, 1=downloading, 2=paused, 3=completed …）。
const TASK_STATUS_COMPLETED: i32 = 3;

/// 本构建与当前会话能否把文件写入系统剪贴板：需编入桌面剪贴板（`desktop` 特性，headless / NAS
/// 构建没有）；Linux 另需图形会话（`WAYLAND_DISPLAY` / `DISPLAY`），否则没有剪贴板可写。网关据此
/// 按连接下发 `agent.copyTaskFiles`，客户端只在能真正生效时展示「复制文件」。
#[must_use]
pub fn copies_files_to_clipboard() -> bool {
    if !cfg!(all(
        feature = "desktop",
        any(windows, target_os = "macos", target_os = "linux")
    )) {
        return false;
    }
    !cfg!(target_os = "linux")
        || ["WAYLAND_DISPLAY", "DISPLAY"]
            .iter()
            .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

/// 把任务产物以文件形式写入系统剪贴板（可在文件管理器 / 聊天软件里粘贴）。任一任务未完成或产物
/// 已不在磁盘上即整体报 [`PlatformError::NotFound`]，不写入只含部分文件的剪贴板。
pub fn copy_task_files(tasks: &[fluxdown_protocol::TaskDto]) -> Result<(), PlatformError> {
    let paths = clipboard_targets(tasks)?;
    write_clipboard_files(&paths)
}

/// 剪贴板要写入的产物路径（去重，保持顺序）；校验见 [`copy_task_files`]。
fn clipboard_targets(tasks: &[fluxdown_protocol::TaskDto]) -> Result<Vec<PathBuf>, PlatformError> {
    let mut paths: Vec<PathBuf> = Vec::with_capacity(tasks.len());
    for task in tasks {
        let path = Path::new(&task.save_dir).join(&task.file_name);
        if task.status != TASK_STATUS_COMPLETED || task.file_name.is_empty() || !path.exists() {
            return Err(PlatformError::NotFound(path));
        }
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

#[cfg(feature = "desktop")]
fn write_clipboard_files(paths: &[PathBuf]) -> Result<(), PlatformError> {
    clipboard_files::write(paths)
}

#[cfg(not(feature = "desktop"))]
fn write_clipboard_files(_paths: &[PathBuf]) -> Result<(), PlatformError> {
    Err(PlatformError::Unsupported(
        "copying files to the clipboard requires the desktop build",
    ))
}

/// 用系统默认的目录处理程序打开已存在的目录；不存在时报错，不交给系统命令静默失败。
fn open_dir(dir: &Path) -> Result<(), PlatformError> {
    if !dir.is_dir() {
        return Err(PlatformError::NotFound(dir.to_path_buf()));
    }
    launch_path(dir, false)
}

/// 一次性系统打开模式（见 [`fluxdown_protocol::shell_open`]）。`args` 不含 argv[0]；首个参数
/// 不是该模式的标志时返回 `None`（照常启动），否则执行并返回进程退出码——参数不合法也
/// 不会落入常驻启动。
#[must_use]
pub fn run_shell_open(args: &[String]) -> Option<i32> {
    use fluxdown_protocol::shell_open::{
        EXIT_FAILED, EXIT_NOT_FOUND, EXIT_OK, EXIT_UNSUPPORTED, EXIT_USAGE, OPEN_DIR_ARG,
        OPEN_TASK_ARG, REVEAL_TASK_ARG,
    };

    let (flag, rest) = args.split_first()?;
    let outcome = match (flag.as_str(), rest) {
        (OPEN_TASK_ARG, [save_dir, file_name]) => open_task_file(Path::new(save_dir), file_name),
        (REVEAL_TASK_ARG, [save_dir, file_name]) => {
            reveal_task_file(Path::new(save_dir), file_name)
        }
        (OPEN_DIR_ARG, [dir]) => open_dir(Path::new(dir)),
        (OPEN_TASK_ARG | REVEAL_TASK_ARG | OPEN_DIR_ARG, _) => return Some(EXIT_USAGE),
        _ => return None,
    };
    Some(match outcome {
        Ok(()) => EXIT_OK,
        Err(PlatformError::NotFound(_)) => EXIT_NOT_FOUND,
        Err(PlatformError::Unsupported(_)) => EXIT_UNSUPPORTED,
        Err(PlatformError::Io(_) | PlatformError::Failed(_) | PlatformError::InvalidScheme(_)) => {
            EXIT_FAILED
        }
    })
}

/// 定位目标按优先级：最终产物 → 下载中临时文件 `<name>.fdownloading` → 保存目录本身
/// （`false` = 打开目录而非选中条目）。未完成任务的最终文件不存在，直接 `open -R` /
/// `explorer /select` 会静默失败或跳到无关目录。保存目录也不存在时报错。
fn reveal_target(save_dir: &Path, file_name: &str) -> Result<(PathBuf, bool), PlatformError> {
    if !file_name.is_empty() {
        let final_path = save_dir.join(file_name);
        if final_path.exists() {
            return Ok((final_path, true));
        }
        let mut temp = final_path.into_os_string();
        temp.push(DOWNLOADING_SUFFIX);
        let temp = PathBuf::from(temp);
        if temp.exists() {
            return Ok((temp, true));
        }
    }
    if save_dir.is_dir() {
        return Ok((save_dir.to_path_buf(), false));
    }
    Err(PlatformError::NotFound(save_dir.to_path_buf()))
}

/// 用系统默认程序打开 `path`；`reveal` 为 true 时改为在文件管理器中定位。
pub fn open_path(path: &Path, reveal: bool) -> Result<(), PlatformError> {
    launch_path(path, reveal)
}

/// 拉起同级桌面程序；桌面已在运行时新进程经单实例通道转发激活/链接后立即退出。
pub fn launch_desktop(args: &[&str]) -> Result<(), PlatformError> {
    launch_desktop_observed(args, || {})
}

/// 同 [`launch_desktop`]，另在桌面进程退出后（含把请求转发给已有主实例后退出）于回收线程
/// 调用 `on_exit`；返回错误时进程没有启动，`on_exit` 不会被调用。
pub fn launch_desktop_observed(
    args: &[&str],
    on_exit: impl FnOnce() + Send + 'static,
) -> Result<(), PlatformError> {
    let executable = desktop_executable().ok_or(PlatformError::Unsupported(
        "fluxdown-desktop is not installed next to fluxdown-agent",
    ))?;
    let mut command = std::process::Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_from_agent(&mut command);
    // The detached thread owns the child until wait completes.
    drop(spawn_desktop_reaper(command, on_exit)?);
    Ok(())
}

/// Start the reaper first: thread-creation failure must not leave an unreaped child.
/// Only spawning is synchronous; waiting never blocks the tray/controller or RPC caller.
/// `on_exit` runs on the reaper thread once the spawned child has exited (never on spawn failure).
fn spawn_desktop_reaper(
    mut command: std::process::Command,
    on_exit: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<Option<std::process::ExitStatus>>> {
    let (started, receiver) = std::sync::mpsc::sync_channel(1);
    let reaper = std::thread::Builder::new()
        .name("fluxdown-desktop-reaper".to_owned())
        .spawn(move || {
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    if started.send(Err(error)).is_err() {
                        tracing::warn!("desktop launch caller disconnected before spawn failure");
                    }
                    return None;
                }
            };
            if started.send(Ok(())).is_err() {
                tracing::warn!("desktop launch caller disconnected; still reaping its child");
            }
            let status = match child.wait() {
                Ok(status) => {
                    if !status.success() {
                        tracing::warn!(%status, "fluxdown-desktop exited unsuccessfully");
                    }
                    Some(status)
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to reap fluxdown-desktop");
                    None
                }
            };
            on_exit();
            status
        })?;
    receiver.recv().map_err(std::io::Error::other)??;
    Ok(reaper)
}

/// Unix 下桌面进程进入独立进程组：从终端启动的 agent 收到 Ctrl-C 不连带终止界面。
#[cfg(unix)]
fn detach_from_agent(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach_from_agent(command: &mut std::process::Command) {
    set_no_console_window(command);
}

/// 当前系统集成状态快照。
#[must_use]
pub fn integration_status() -> PlatformIntegrationDto {
    let desktop = desktop_executable();
    let target = desktop.as_deref();
    let url_protocols = protocol_registry::SCHEMES
        .iter()
        .map(|scheme| {
            (
                scheme.scheme.to_owned(),
                protocol_registry::is_registered(*scheme, target),
            )
        })
        .collect();
    PlatformIntegrationDto {
        autostart_supported: autostart::supported(target),
        autostart_enabled: target.is_some()
            && agent_executable().is_ok_and(|agent| autostart::is_enabled(&agent)),
        file_association_supported: file_association::supported(target),
        torrent_associated: file_association::is_associated(),
        url_protocol_supported: protocol_registry::supported(target),
        url_protocols,
        desktop_executable: desktop
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
    }
}

/// 开机自启的实际状态：区分「用户没开」与「开了但被系统级开关禁用」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartState {
    /// 没有桌面程序可拉起（开发构建 / 精简安装）或平台不支持。
    Unsupported,
    /// 没有指向当前 agent 的自启条目。
    Off,
    Enabled,
    /// 条目在，但用户在系统设置 / 任务管理器 / 桌面环境里禁用了它。
    DisabledBySystem,
}

/// 读取开机自启状态（阻塞：读注册表 / 文件 / 调 `launchctl`）。
#[must_use]
pub fn autostart_state() -> AutostartState {
    let desktop = desktop_executable();
    if !autostart::supported(desktop.as_deref()) {
        return AutostartState::Unsupported;
    }
    let Ok(agent) = agent_executable() else {
        return AutostartState::Unsupported;
    };
    if !autostart::is_registered(&agent) {
        AutostartState::Off
    } else if autostart::is_enabled(&agent) {
        AutostartState::Enabled
    } else {
        AutostartState::DisabledBySystem
    }
}

/// Doctor 引导用户自行放行的系统设置页（这些开关只能由用户本人在系统里打开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPane {
    /// 系统通知设置（Windows「通知」、macOS「通知」）。
    Notifications,
    /// macOS「隐私与安全性 › 文件与文件夹」：隐私授权（TCC）拒绝写入时。
    FilesAndFolders,
}

impl SettingsPane {
    /// `agent.diagnostics.repair` 的 `target` 名。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Notifications => "notifications",
            Self::FilesAndFolders => "files_and_folders",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Notifications, Self::FilesAndFolders]
            .into_iter()
            .find(|pane| pane.name() == name)
    }

    /// 本平台能直接打开的设置页地址；没有对应页面时为 `None`。
    #[must_use]
    pub fn uri(self) -> Option<&'static str> {
        if cfg!(target_os = "macos") {
            Some(match self {
                Self::Notifications => {
                    "x-apple.systempreferences:com.apple.preference.notifications"
                }
                Self::FilesAndFolders => {
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders"
                }
            })
        } else if cfg!(windows) {
            match self {
                Self::Notifications => Some("ms-settings:notifications"),
                Self::FilesAndFolders => None,
            }
        } else {
            None
        }
    }
}

/// 打开系统设置页（阻塞拉起系统打开程序，不等设置窗口关闭）。
pub fn open_system_settings(pane: SettingsPane) -> Result<(), PlatformError> {
    let uri = pane.uri().ok_or(PlatformError::Unsupported(
        "this settings page does not exist on this platform",
    ))?;
    open_uri(uri)
}

#[cfg(target_os = "macos")]
fn open_uri(uri: &str) -> Result<(), PlatformError> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(uri)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(PlatformError::Failed(format!("open {uri}: {status}")))
    }
}

#[cfg(windows)]
fn open_uri(uri: &str) -> Result<(), PlatformError> {
    if shell_execute_open(uri) {
        Ok(())
    } else {
        Err(PlatformError::Failed(format!("could not open {uri}")))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn open_uri(_uri: &str) -> Result<(), PlatformError> {
    Err(PlatformError::Unsupported(
        "system settings pages are not available on this platform",
    ))
}

/// 开机自启 agent（`--autostart`）；要求桌面程序已安装在同级目录，保证自启后能拉起界面。
pub fn set_autostart(enabled: bool) -> Result<(), PlatformError> {
    if !enabled {
        return autostart::disable();
    }
    desktop_executable().ok_or(PlatformError::Unsupported(
        "fluxdown-desktop is not installed next to fluxdown-agent",
    ))?;
    autostart::enable(&agent_executable()?)
}

/// 旧版自启条目直接拉起桌面程序（`fluxdown-desktop --minimized`）；启动时改写为 agent，
/// 托盘驻留与「启动时最小化到托盘」才能在不开界面的情况下生效。只改写启动目标：
/// 用户在系统层禁用的条目迁移后仍保持禁用。
pub fn migrate_legacy_autostart() -> Result<(), PlatformError> {
    let Some(desktop) = desktop_executable() else {
        return Ok(());
    };
    let agent = agent_executable()?;
    if autostart::is_registered(&agent) || !autostart::targets(&desktop) {
        return Ok(());
    }
    tracing::info!("migrating legacy desktop autostart entry to fluxdown-agent");
    autostart::retarget(&agent)
}

/// 自启条目指向的可执行文件。AppImage 运行时 `current_exe` 是每次启动都不同的临时挂载点
/// （`/tmp/.mount_*`），改为指向 AppImage 文件本身；其 `AppRun` 把 `--autostart` 转交 agent。
fn agent_executable() -> Result<PathBuf, PlatformError> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("APPIMAGE").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(appimage));
    }
    Ok(std::env::current_exe()?)
}

pub fn set_file_association(enabled: bool) -> Result<(), PlatformError> {
    if enabled {
        file_association::associate(desktop_executable().as_deref())
    } else {
        file_association::disassociate()
    }
}

/// `scheme` 只接受 `magnet` / `ed2k` / `fluxdown`。
pub fn set_url_protocol(scheme: &str, enabled: bool) -> Result<(), PlatformError> {
    let scheme = protocol_registry::from_name(scheme)
        .ok_or_else(|| PlatformError::InvalidScheme(scheme.to_owned()))?;
    let desktop = desktop_executable();
    if enabled {
        protocol_registry::register(scheme, desktop.as_deref())
    } else {
        protocol_registry::unregister(scheme, desktop.as_deref())
    }
}

/// 本构建能否在 agent 所在主机上打开 / 定位路径（与下方 `launch_path` 的 cfg 一一对应）。
/// 网关据此决定是否向连接下发 `agent.openTaskFiles` 能力。
pub const LAUNCHES_PATHS: bool = cfg!(any(windows, target_os = "linux", target_os = "macos"));

#[cfg(target_os = "linux")]
fn launch_path(path: &Path, reveal: bool) -> Result<(), PlatformError> {
    // xdg-open 无「选中」语义：定位时打开所在目录，避免直接打开（未完成的）文件。
    let target = if reveal {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    std::process::Command::new("xdg-open")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_path(path: &Path, reveal: bool) -> Result<(), PlatformError> {
    let mut command = std::process::Command::new("open");
    if reveal {
        command.arg("-R");
    }
    command.arg(path).spawn()?;
    Ok(())
}

#[cfg(windows)]
fn launch_path(path: &Path, reveal: bool) -> Result<(), PlatformError> {
    if reveal {
        // 「在文件夹中显示」：
        // 1) 第三方默认文件管理器兜底（#122）：OneCommander / Total Commander / Files
        //    等只改 HKCR\Directory\shell\open\command、未挂 Explorer
        //    Replacement 钩子的 FM 拦截不到 SHOpenFolderAndSelectItems——API
        //    会直接拉起 Explorer 且返回成功，永远走不到回退；必须先探测，
        //    命中即退化为「用第三方 FM 打开父目录」（不选中）。
        // 2) Explorer 仍是默认：走标准 Shell API「打开父目录并选中」（见
        //    sh_open_folder_and_select），失败回退 open 动词打开父目录，
        //    保证至少有响应。
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().unwrap_or(path).to_path_buf()
        };
        if default_dir_handler_is_third_party() {
            tracing::debug!("reveal: third-party default file manager detected; opening dir");
            return open_with_shell(&dir);
        }
        if !path.is_dir() && sh_open_folder_and_select(&path.to_string_lossy()) {
            return Ok(());
        }
        tracing::debug!(
            "reveal: SHOpenFolderAndSelectItems failed; falling back to ShellExecuteW open"
        );
        return open_with_shell(&dir);
    }
    open_with_shell(path)
}

/// Android / iOS：由宿主应用（Intent / UIDocumentInteraction）打开文件，agent 进程内不可用。
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn launch_path(_path: &Path, _reveal: bool) -> Result<(), PlatformError> {
    Err(PlatformError::Unsupported(
        "opening files is handled by the host application on this platform",
    ))
}

/// 打开任意路径（文件走默认关联程序、目录走默认文件管理器）。
///
/// 优先直接调 Win32
/// `ShellExecuteW`（"open" 默认 verb，双击的 API 本体，无 cmd 引号/元字符
/// 解析风险）；失败回退 `explorer.exe <path>`——同样按关联打开，且参数经标准 argv 引用
/// 传递，不经 cmd 解析。只接受绝对路径，避免被 explorer 当成命令行开关。
#[cfg(windows)]
fn open_with_shell(path: &Path) -> Result<(), PlatformError> {
    let text = path.to_string_lossy();
    if shell_execute_open(&text) {
        return Ok(());
    }
    if !path.is_absolute() {
        return Err(PlatformError::Failed(
            "refusing to open a non-absolute path".to_owned(),
        ));
    }
    tracing::debug!("ShellExecuteW failed; falling back to explorer.exe");
    let mut command = std::process::Command::new("explorer.exe");
    command.arg(path);
    set_no_console_window(&mut command);
    command.spawn()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 打开/定位的 Shell 调用与注册表探测。
// ---------------------------------------------------------------------------

/// 直接调 Win32 `ShellExecuteW`（"open" 默认 verb）打开路径——微软官方的
/// 「打开」调用（双击的 API 本体），系统按 open 动词关联解析默认处理程序。
/// 返回值 > 32 表示成功（Win32 约定）。
#[cfg(windows)]
fn shell_execute_open(path: &str) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wide/verb 均为有效的 NUL 结尾 UTF-16 缓冲，在调用期间存活；
    // 其余参数按文档允许为空。
    let h = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    h as usize > 32
}

/// 标准 Shell API：打开 `path` 所在父目录并选中 `path`（文件/目录皆可）。
///
/// `SHOpenFolderAndSelectItems` 是 Windows Shell 的标准「定位到文件夹视图」
/// 调用，不硬编码 explorer.exe——文件夹视图由系统 Shell 打开。用 cidl=0 的
/// 简写形式：`pidlFolder` 直接指向要选中的项，系统自动打开其父目录并选中
/// 该项（见 MSDN 备注）。实现与 CLaunch 的 `openParentFolder` 同款：
/// `SHParseDisplayName` 解析绝对 PIDL + `CoTaskMemFree` 释放 + 防御性 COM
/// 初始化；失败返回 false，调用方回退为 open 动词打开父目录。
///
/// 文档要求先 CoInitialize：本函数运行在 RPC 处理线程上，这里做防御性
/// 初始化——`hr < 0` 视为失败；S_OK/S_FALSE 都会取得本线程初始化引用，
/// 结尾须配对 `CoUninitialize`。
#[cfg(windows)]
fn sh_open_folder_and_select(path: &str) -> bool {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows_sys::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};

    struct Pidl(*mut ITEMIDLIST);
    impl Drop for Pidl {
        fn drop(&mut self) {
            // SAFETY: SHParseDisplayName allocated this PIDL with the COM allocator; null is allowed.
            unsafe { CoTaskMemFree(self.0.cast()) };
        }
    }

    if path.contains('\0') {
        tracing::debug!("Shell selection path contains NUL");
        return false;
    }
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let _com = match windows_com::Com::init() {
        Ok(com) => com,
        Err(error) => {
            tracing::debug!(%error, "Shell selection COM initialization failed");
            return false;
        }
    };
    let mut pidl = Pidl(std::ptr::null_mut());
    // SAFETY: wide is NUL terminated and live; pidl is an owned output slot; other pointers may be null.
    let hr_parse = unsafe {
        SHParseDisplayName(
            wide.as_ptr(),
            std::ptr::null_mut(),
            &mut pidl.0,
            0,
            std::ptr::null_mut(),
        )
    };
    if hr_parse < 0 || pidl.0.is_null() {
        tracing::debug!(hr = format!("{hr_parse:#x}"), "SHParseDisplayName failed");
        return false;
    }
    // SAFETY: successful parse produced a live absolute PIDL; cidl=0 selects that item in its parent.
    let hr_select = unsafe { SHOpenFolderAndSelectItems(pidl.0, 0, std::ptr::null(), 0) };
    if hr_select < 0 {
        tracing::debug!(
            hr = format!("{hr_select:#x}"),
            "SHOpenFolderAndSelectItems failed"
        );
        return false;
    }
    true // PIDL drops before the thread-bound COM guard on all return paths.
}

/// Windows：系统「打开目录」的默认处理程序是否已被替换成第三方文件管理器。
///
/// 读取 `HKCR\Directory\shell\<默认 verb>\command` 并解析其可执行文件名。
/// 非 `explorer.exe` 时返回 `true`；键缺失、读取失败或仍是 Explorer 时返回
/// `false`（保留 Shell API 的选中体验）。`<默认 verb>` 取 `Directory\shell`
/// 的默认值，为空或 `none` 时回退到 `open`（第三方替换的常用写法）。只改了
/// 此键的第三方 FM（OneCommander 等）拦截不到 `SHOpenFolderAndSelectItems`，
/// 必须靠它兜底。
#[cfg(windows)]
fn default_dir_handler_is_third_party() -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CLASSES_ROOT;

    let hkcr = RegKey::predef(HKEY_CLASSES_ROOT);
    let Ok(shell) = hkcr.open_subkey(r"Directory\shell") else {
        return false;
    };
    let verb = shell.get_value::<String, _>("").unwrap_or_default();
    let verb = verb.trim();
    let verb = if verb.is_empty() || verb.eq_ignore_ascii_case("none") {
        "open"
    } else {
        verb
    };
    let Ok(cmd_key) = hkcr.open_subkey(format!(r"Directory\shell\{verb}\command")) else {
        return false;
    };
    let Ok(cmd) = cmd_key.get_value::<String, _>("") else {
        return false;
    };
    match exe_basename(&cmd) {
        Some(name) => !name.eq_ignore_ascii_case("explorer.exe"),
        None => false,
    }
}

/// 返回裸路径字符串中首个（不区分大小写）以 `.exe` 结尾的字节偏移；找不到
/// 时返回 `None`。`.exe` 全为 ASCII，`to_ascii_lowercase` 不改变字节长度
/// 与 UTF-8 边界，返回的偏移量可直接用于原字符串按字节切片。
#[cfg(windows)]
fn find_exe_end(cmd: &str) -> Option<usize> {
    cmd.to_ascii_lowercase().find(".exe").map(|idx| idx + 4)
}

/// 从注册表 shell command 字符串解析出可执行文件的文件名（basename）。
/// 支持带引号路径（`"C:\..\fm.exe" "%1"`）与裸路径
/// (`%SystemRoot%\Explorer.exe /idlist,...`)；返回 `None` 表示无法解析。
#[cfg(windows)]
fn exe_basename(cmd: &str) -> Option<String> {
    let cmd = cmd.trim();
    let exe = if let Some(rest) = cmd.strip_prefix('"') {
        rest.split('"').next().unwrap_or(rest)
    } else {
        // 裸路径可能含空格且未加引号写入注册表（如部分第三方文件管理器的安装
        // 程序），不能简单按空白切分；取字符串中首个（不区分大小写）以
        // ".exe" 结尾的位置，把它之前的内容整体当作可执行文件路径，大小写
        // 按原样保留。找不到 ".exe" 时退回按空白切分。
        match find_exe_end(cmd) {
            Some(end) => &cmd[..end],
            None => cmd.split_whitespace().next().unwrap_or(cmd),
        }
    };
    let base = exe.rsplit(['\\', '/']).next().unwrap_or(exe).trim();
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

#[cfg(windows)]
fn set_no_console_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(any(windows, unix)))]
fn set_no_console_window(_command: &mut std::process::Command) {}

/// 注册表命令行使用的可执行文件路径：canonicalize 解析符号链接后去掉 `\\?\`
/// 前缀，便于与安装器写入的值比较。
#[cfg(windows)]
fn registry_executable(executable: Option<&Path>) -> Result<String, PlatformError> {
    let path = executable.ok_or(PlatformError::Unsupported(
        "fluxdown-desktop.exe is not installed next to fluxdown-agent",
    ))?;
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = canonical.to_string_lossy();
    Ok(text.strip_prefix(r"\\?\").unwrap_or(&*text).to_owned())
}

/// 通知资源管理器重新读取注册表里的环境变量：之后由它启动的程序（开始菜单 / 任务栏打开的
/// 浏览器）才拿到新值；已在运行的进程（包括本进程）仍沿用启动时继承的环境块。
#[cfg(windows)]
pub fn notify_environment_changed() -> Result<(), PlatformError> {
    windows_shell::notify_environment_changed().map_err(PlatformError::Io)
}

#[cfg(windows)]
mod windows_shell {
    /// `SHChangeNotify(SHCNE_ASSOCCHANGED)` 通知资源管理器关联已变化。
    pub fn notify_association_changed() {
        use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

        // SAFETY: SHCNE_ASSOCCHANGED + SHCNF_IDLIST 不读取 item 指针，传 null 合法。
        unsafe {
            SHChangeNotify(
                SHCNE_ASSOCCHANGED as i32,
                SHCNF_IDLIST,
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }

    /// 广播 `WM_SETTINGCHANGE("Environment")`；失败返回系统错误。
    pub fn notify_environment_changed() -> std::io::Result<()> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        };

        /// 每个顶层窗口的等待上限；已挂起的窗口由 `SMTO_ABORTIFHUNG` 直接跳过。
        const PER_WINDOW_TIMEOUT_MS: u32 = 2000;
        let area = windows_sys::w!("Environment") as isize;
        let mut reply = 0usize;
        // SAFETY: HWND_BROADCAST 是文档定义的广播伪句柄；lParam 指向 `w!` 生成的 'static、
        // NUL 结尾 UTF-16 常量，接收方只读取它（系统负责跨进程封送）；lpdwResult 指向本栈上
        // 存活到调用返回的 usize。调用同步完成，不保留任何指针。
        let sent = unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                area,
                SMTO_ABORTIFHUNG,
                PER_WINDOW_TIMEOUT_MS,
                &mut reply,
            )
        };
        if sent == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod xdg {
    use std::io::{BufRead, Write};

    use super::PlatformError;

    /// 打包安装的桌面入口（`packaging/linux/com.fluxdown.app.desktop`）。
    pub const DESKTOP_ENTRY: &str = "com.fluxdown.app.desktop";

    /// `xdg-mime query default <mime>` 是否返回 FluxDown 的桌面入口。
    pub fn query_default_is_fluxdown(mime: &str) -> bool {
        let Ok(output) = std::process::Command::new("xdg-mime")
            .args(["query", "default", mime])
            .output()
        else {
            return false;
        };
        String::from_utf8_lossy(&output.stdout)
            .to_lowercase()
            .contains("fluxdown")
    }

    /// 从 `~/.config/mimeapps.list` 删除指向 FluxDown 的 `<mime>=…` 行。
    ///
    /// xdg-mime 没有“取消默认”命令，只能直接编辑用户覆盖文件。
    pub fn remove_default(mime: &str) -> Result<(), PlatformError> {
        let base = directories::BaseDirs::new()
            .ok_or(PlatformError::Unsupported("home directory unavailable"))?;
        let path = base.config_dir().join("mimeapps.list");
        if !path.exists() {
            return Ok(());
        }
        let file = std::fs::File::open(&path)?;
        let lines = std::io::BufReader::new(file)
            .lines()
            .collect::<Result<Vec<String>, _>>()?;
        let prefix = format!("{}=", mime.to_lowercase());
        let mut out = std::fs::File::create(&path)?;
        for line in lines {
            let lower = line.to_lowercase();
            if lower.starts_with(&prefix) && lower.contains("fluxdown") {
                continue;
            }
            writeln!(out, "{line}")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("platform action failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("platform integration unsupported: {0}")]
    Unsupported(&'static str),
    #[error("platform integration failed: {0}")]
    Failed(String),
    /// 要打开 / 定位的任务产物或保存目录已不在磁盘上。
    #[error("path not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("unknown URL scheme: {0}")]
    InvalidScheme(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip_task(dir: &Path, id: &str, file_name: &str, status: i32) -> fluxdown_protocol::TaskDto {
        serde_json::from_value(serde_json::json!({
            "taskId": id, "url": "https://example.com/f", "fileName": file_name,
            "saveDir": dir.to_string_lossy(), "status": status, "downloadedBytes": 1,
            "totalBytes": 1, "errorMessage": "", "createdAt": "1", "proxyUrl": "",
            "queueId": "", "checksum": ""
        }))
        .expect("task dto")
    }

    #[test]
    fn clipboard_targets_need_every_task_completed_with_its_product_on_disk() {
        let dir = std::env::temp_dir().join(format!("fluxdown-clip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).expect("create save dir");
        std::fs::write(dir.join("a.zip"), b"a").expect("write product");
        std::fs::create_dir(dir.join("album")).expect("create multi-file product");
        let ok = clip_task(&dir, "1", "a.zip", TASK_STATUS_COMPLETED);

        // 多文件 BT 产物是目录，同样可复制；指向同一产物的任务只写一次。
        let paths = clipboard_targets(&[
            ok.clone(),
            clip_task(&dir, "2", "album", TASK_STATUS_COMPLETED),
            clip_task(&dir, "3", "a.zip", TASK_STATUS_COMPLETED),
        ])
        .expect("targets");
        assert_eq!(paths, vec![dir.join("a.zip"), dir.join("album")]);

        // 未完成（BT 下载中产物已就位）、产物已删、无文件名：整体拒绝，不写半份剪贴板。
        for bad in [
            clip_task(&dir, "4", "a.zip", 1),
            clip_task(&dir, "5", "gone.zip", TASK_STATUS_COMPLETED),
            clip_task(&dir, "6", "", TASK_STATUS_COMPLETED),
        ] {
            assert!(matches!(
                clipboard_targets(&[ok.clone(), bad]),
                Err(PlatformError::NotFound(_))
            ));
        }
        std::fs::remove_dir_all(&dir).expect("remove save dir");
    }

    #[cfg(unix)]
    #[test]
    fn desktop_reaper_returns_before_exit_and_reports_exit() {
        let dir = std::env::temp_dir().join(format!("fluxdown-reaper-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).expect("create release gate directory");
        let release = dir.join("release");
        let mut command = std::process::Command::new("/bin/sh");
        // A bounded gate proves launch returns while the child is alive, without a timing
        // threshold. It also prevents a broken reaper from hanging the test indefinitely.
        command
            .args([
                "-c",
                r#"i=0; while [ ! -f "$1" ]; do i=$((i+1)); [ "$i" -lt 100 ] || exit 99; sleep 0.05; done; exit 7"#,
                "fluxdown-reaper-test",
            ])
            .arg(&release);
        detach_from_agent(&mut command);
        let (exited_tx, exited) = std::sync::mpsc::channel();
        let reaper = spawn_desktop_reaper(command, move || {
            exited_tx.send(()).expect("report exit");
        })
        .expect("spawn child and reaper");
        let returned_before_exit = !reaper.is_finished();
        let reported_before_exit = exited.try_recv().is_ok();
        std::fs::write(&release, []).expect("release child");
        let status = reaper
            .join()
            .expect("reaper thread")
            .expect("wait succeeded");
        std::fs::remove_dir_all(dir).expect("remove release gate directory");
        assert!(returned_before_exit, "launch must not wait for child exit");
        assert!(
            !reported_before_exit,
            "exit is reported only after the child exits"
        );
        assert!(exited.try_recv().is_ok(), "exit is reported once reaped");
        assert_eq!(status.code(), Some(7), "collect the real child exit status");
    }

    #[test]
    fn desktop_reaper_reports_spawn_failure_without_exit_callback() {
        let missing =
            std::env::temp_dir().join(format!("fluxdown-missing-{}", uuid::Uuid::new_v4()));
        let (exited_tx, exited) = std::sync::mpsc::channel();
        let error = spawn_desktop_reaper(std::process::Command::new(missing), move || {
            exited_tx.send(()).expect("report exit");
        })
        .expect_err("missing executable must fail synchronously");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        // The callback is dropped with the reaper thread; it must never run for a child that
        // never started, or launch bookkeeping would release the same slot twice.
        assert!(
            exited.recv().is_err(),
            "spawn failure must not report an exit"
        );
    }

    #[test]
    fn desktop_executable_lives_next_to_agent() {
        let current = std::env::current_exe().expect("current exe");
        let sibling = current.with_file_name(DESKTOP_EXECUTABLE_NAME);
        assert_eq!(desktop_executable(), sibling.is_file().then_some(sibling));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn helper_bundle_resolves_host_macos_dir() {
        let agent = Path::new(
            "/Applications/FluxDown.app/Contents/Helpers/FluxDownAgent.app/Contents/MacOS/fluxdown-agent",
        );
        assert_eq!(
            host_macos_dir(agent),
            Some(PathBuf::from("/Applications/FluxDown.app/Contents/MacOS"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn flat_or_foreign_layouts_are_not_helper_bundles() {
        for agent in [
            "/Applications/FluxDown.app/Contents/MacOS/fluxdown-agent",
            "/repo/target/release/fluxdown-agent",
            "/x/Helpers/FluxDownAgent.app/Contents/MacOS/fluxdown-agent",
            "/x/Contents/Helpers/FluxDownAgent.app/Contents/MacOS/fluxdown-agent",
            "/Applications/FluxDown.app/Contents/Helpers/Agent/Contents/MacOS/fluxdown-agent",
        ] {
            assert_eq!(host_macos_dir(Path::new(agent)), None, "{agent}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn host_bundle_id_strips_helper_suffix_only_inside_helper() {
        assert_eq!(
            host_id_from("com.fluxdown.app.agent".to_owned(), true).as_deref(),
            Some("com.fluxdown.app")
        );
        assert_eq!(
            host_id_from("com.fluxdown.app".to_owned(), false).as_deref(),
            Some("com.fluxdown.app")
        );
        assert_eq!(host_id_from("com.fluxdown.app".to_owned(), true), None);
        assert_eq!(host_id_from(".agent".to_owned(), true), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn release_hands_over_to_first_other_candidate_only() {
        let mine = "com.fluxdown.app";
        assert_eq!(
            successor_handler(
                vec![
                    "COM.FLUXDOWN.APP".to_owned(),
                    String::new(),
                    "org.qbittorrent.qBittorrent".to_owned(),
                    "org.transmissionbt.Transmission".to_owned(),
                ],
                mine,
            )
            .as_deref(),
            Some("org.qbittorrent.qBittorrent")
        );
        // 唯一候选是自己：Launch Services 无处可让。
        assert_eq!(successor_handler(vec![mine.to_owned()], mine), None);
        assert_eq!(successor_handler(Vec::new(), mine), None);
    }

    #[test]
    fn integration_status_reports_all_schemes() {
        let status = integration_status();
        assert_eq!(
            status.url_protocols.keys().cloned().collect::<Vec<_>>(),
            ["ed2k", "fluxdown", "magnet"]
        );
        assert!(!status.autostart_supported || !status.desktop_executable.is_empty());
    }

    #[test]
    fn unknown_scheme_is_rejected_before_touching_the_system() {
        assert!(matches!(
            set_url_protocol("javascript", true),
            Err(PlatformError::InvalidScheme(_))
        ));
    }

    #[test]
    fn reveal_target_prefers_final_then_temp_then_directory() {
        let dir =
            std::env::temp_dir().join(format!("fluxdown-agent-reveal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");

        // 暂停 / 下载中：只有临时文件。
        let temp = dir.join("a.dmg.fdownloading");
        std::fs::write(&temp, b"partial").expect("write temp");
        assert_eq!(reveal_target(&dir, "a.dmg").expect("temp"), (temp, true));

        // 已完成：最终文件优先。
        let final_path = dir.join("a.dmg");
        std::fs::write(&final_path, b"done").expect("write final");
        assert_eq!(
            reveal_target(&dir, "a.dmg").expect("final"),
            (final_path, true)
        );

        // 尚未落盘（排队 / 名称未知）：打开保存目录。
        assert_eq!(
            reveal_target(&dir, "missing.bin").expect("dir"),
            (dir.clone(), false)
        );
        assert_eq!(reveal_target(&dir, "").expect("dir"), (dir.clone(), false));

        std::fs::remove_dir_all(&dir).expect("cleanup");
        assert!(matches!(
            reveal_target(&dir, "a.dmg"),
            Err(PlatformError::NotFound(path)) if path == dir
        ));
    }

    #[test]
    fn shell_open_mode_claims_only_its_own_flags() {
        use fluxdown_protocol::shell_open::{
            EXIT_USAGE, OPEN_DIR_ARG, OPEN_TASK_ARG, REVEAL_TASK_ARG,
        };
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(run_shell_open(&[]), None);
        assert_eq!(run_shell_open(&args(&[AUTOSTART_ARG])), None);
        // 标志不在首位不是该模式：照常启动，不会误把普通参数当成打开请求。
        assert_eq!(run_shell_open(&args(&["x", OPEN_DIR_ARG, "y"])), None);
        // 保留标志即使参数个数不对也消费本次启动，绝不落入常驻启动。
        for malformed in [
            args(&[OPEN_TASK_ARG]),
            args(&[OPEN_TASK_ARG, "dir", "name", "extra"]),
            args(&[REVEAL_TASK_ARG, "dir"]),
            args(&[OPEN_DIR_ARG]),
            args(&[OPEN_DIR_ARG, "a", "b"]),
        ] {
            assert_eq!(
                run_shell_open(&malformed),
                Some(EXIT_USAGE),
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn shell_open_reports_missing_targets_before_touching_the_system() {
        use fluxdown_protocol::shell_open::{
            EXIT_NOT_FOUND, OPEN_DIR_ARG, OPEN_TASK_ARG, REVEAL_TASK_ARG,
        };
        let missing = std::env::temp_dir()
            .join(format!("fluxdown-shell-open-{}", uuid::Uuid::new_v4()))
            .display()
            .to_string();
        for args in [
            [OPEN_TASK_ARG, missing.as_str(), "a.zip"].as_slice(),
            // 文件名未知（尚未定名）的任务没有可打开的产物。
            [OPEN_TASK_ARG, missing.as_str(), ""].as_slice(),
            // 产物、临时文件、保存目录都不在：定位也报不存在，而不是打开无关目录。
            [REVEAL_TASK_ARG, missing.as_str(), "a.zip"].as_slice(),
            [OPEN_DIR_ARG, missing.as_str()].as_slice(),
        ] {
            let args = args
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>();
            assert_eq!(run_shell_open(&args), Some(EXIT_NOT_FOUND), "{args:?}");
        }
    }
}
