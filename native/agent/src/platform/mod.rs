//! agent 桌面系统集成：任务文件打开/定位、官方桌面进程唤起、开机自启、
//! `.torrent` 关联与 URL scheme 注册。
//!
//! 所有注册的目标都是官方桌面程序而非 agent 自身：Windows 指向同级
//! `fluxdown-desktop.exe`，macOS 指向 agent 所在的 `.app` bundle，Linux 指向
//! 打包的 `com.fluxdown.app.desktop`。全部函数同步阻塞，RPC 侧需放入
//! `spawn_blocking`。

mod autostart;
mod file_association;
#[cfg(target_os = "macos")]
mod macos_cf;
mod protocol_registry;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use fluxdown_protocol::PlatformIntegrationDto;

/// 上次为捕获拉起桌面程序的 unix 毫秒时间戳（0 = 从未拉起）。
static LAST_CAPTURE_LAUNCH_MS: AtomicI64 = AtomicI64::new(0);
/// 两次捕获拉起之间的最小间隔：已有 UI 客户端连接时不拉起，断线重连抖动也不重复拉起。
const CAPTURE_LAUNCH_COOLDOWN_MS: i64 = 10_000;

const DESKTOP_EXECUTABLE_NAME: &str = if cfg!(windows) {
    "fluxdown-desktop.exe"
} else {
    "fluxdown-desktop"
};

/// 与 agent 同目录的官方桌面程序；文件不存在时返回 `None`。
#[must_use]
pub fn desktop_executable() -> Option<PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .with_file_name(DESKTOP_EXECUTABLE_NAME);
    path.is_file().then_some(path)
}

pub fn open_task(task: &fluxdown_protocol::TaskDto) -> Result<(), PlatformError> {
    launch_path(&PathBuf::from(&task.save_dir).join(&task.file_name), false)
}

pub fn reveal_task(task: &fluxdown_protocol::TaskDto) -> Result<(), PlatformError> {
    launch_path(&PathBuf::from(&task.save_dir).join(&task.file_name), true)
}

/// 用系统默认程序打开 `path`；`reveal` 为 true 时改为在文件管理器中定位。
pub fn open_path(path: &Path, reveal: bool) -> Result<(), PlatformError> {
    launch_path(path, reveal)
}

/// 无 UI 客户端连接（`ui_clients == 0`）且距上次拉起 ≥ 10s 才需要为捕获拉起桌面程序。
fn should_launch_desktop_for_capture(
    ui_clients: &AtomicUsize,
    last_launch_ms: i64,
    now_ms: i64,
) -> bool {
    ui_clients.load(Ordering::Acquire) == 0
        && now_ms.saturating_sub(last_launch_ms) >= CAPTURE_LAUNCH_COOLDOWN_MS
}

/// 待确认捕获入队时，若当前无已连接的桌面 UI 才拉起同级桌面程序进入 `--capture` 模式；
/// 已有 UI 或距上次拉起不足 10s 时静默跳过。
pub fn launch_desktop_for_capture(ui_clients: &AtomicUsize) -> Result<(), PlatformError> {
    let now = now_unix_ms();
    let last = LAST_CAPTURE_LAUNCH_MS.load(Ordering::Acquire);
    if !should_launch_desktop_for_capture(ui_clients, last, now) {
        return Ok(());
    }
    if LAST_CAPTURE_LAUNCH_MS
        .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // 另一并发调用抢先更新了时间戳，视为已处理。
        return Ok(());
    }
    let executable = desktop_executable().ok_or(PlatformError::Unsupported(
        "fluxdown-desktop is not installed next to fluxdown-agent",
    ))?;
    let mut command = std::process::Command::new(executable);
    command.arg("--capture");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    set_no_console_window(&mut command);
    command.spawn()?;
    Ok(())
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
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
        autostart_enabled: target.is_some_and(autostart::is_enabled),
        file_association_supported: file_association::supported(target),
        torrent_associated: file_association::is_associated(),
        url_protocol_supported: protocol_registry::supported(target),
        url_protocols,
        desktop_executable: desktop
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
    }
}

pub fn set_autostart(enabled: bool) -> Result<(), PlatformError> {
    if !enabled {
        return autostart::disable();
    }
    let desktop = desktop_executable().ok_or(PlatformError::Unsupported(
        "fluxdown-desktop is not installed next to fluxdown-agent",
    ))?;
    autostart::enable(&desktop)
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

#[cfg(target_os = "linux")]
fn launch_path(path: &Path, _reveal: bool) -> Result<(), PlatformError> {
    std::process::Command::new("xdg-open")
        .arg(path)
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
        // 1) 第三方默认文件管理器兜底（#122，同 hub reveal_file.rs 的
        //    platform_reveal_file）：OneCommander / Total Commander / Files
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

/// 打开任意路径（文件走默认关联程序、目录走默认文件管理器）。
///
/// 与 hub `reveal_file.rs` 的 `platform_open_dir` 同一策略：优先直接调 Win32
/// `ShellExecuteW`（"open" 默认 verb，双击的 API 本体，无 cmd 引号/元字符
/// 解析风险）；失败才回退 `cmd /c start "" <path>`（start 内部同样走 open
/// 关联；第一个空引号串是窗口标题，不能省）。
#[cfg(windows)]
fn open_with_shell(path: &Path) -> Result<(), PlatformError> {
    use std::os::windows::process::CommandExt;

    let text = path.to_string_lossy();
    if shell_execute_open(&text) {
        return Ok(());
    }
    tracing::debug!("ShellExecuteW failed; falling back to cmd /c start");
    let mut command = std::process::Command::new("cmd.exe");
    command.raw_arg(format!(r#"/c start "" "{text}""#));
    set_no_console_window(&mut command);
    command.spawn()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 打开/定位的 Shell 调用与注册表探测：与 hub `reveal_file.rs` 同款实现的有意
// 复制（crate 边界隔离），下列每个函数在 hub 都有同名对应，修改务必双份同步。
// ---------------------------------------------------------------------------

/// 直接调 Win32 `ShellExecuteW`（"open" 默认 verb）打开路径——微软官方的
/// 「打开」调用（双击的 API 本体），系统按 open 动词关联解析默认处理程序。
/// 与 `hub/src/reveal_file.rs` 的同名实现保持一致。
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
/// **同步注意**：本函数与 hub `reveal_file.rs` 的同名函数是有意复制的两份
/// （crate 边界隔离），修改任一份务必同步另一份。
///
/// 文档要求先 CoInitialize：本函数运行在 RPC 处理线程上，这里做防御性
/// 初始化——`hr < 0` 视为失败；S_OK/S_FALSE 都会取得本线程初始化引用，
/// 结尾须配对 `CoUninitialize`。
#[cfg(windows)]
fn sh_open_folder_and_select(path: &str) -> bool {
    use windows_sys::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize};
    use windows_sys::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};

    /// `COINIT_APARTMENTTHREADED`。
    const COINIT_APARTMENTTHREADED: u32 = 2;

    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wide 为有效的 NUL 结尾 UTF-16 缓冲，在调用期间存活；其余参数
    // 按文档允许为空。
    let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED) };
    if hr < 0 {
        tracing::debug!(hr = format!("{hr:#x}"), "CoInitializeEx failed");
        return false;
    }

    let mut pidl = std::ptr::null_mut();
    // SAFETY: wide 存活于调用期间；ppidl 接收输出，sfgaoIn/psfgaoOut 传空。
    // pbc 为 *mut c_void，须用 null_mut()——Rust 无 *const → *mut 隐式转换。
    let hr_parse = unsafe {
        SHParseDisplayName(
            wide.as_ptr(),
            std::ptr::null_mut(),
            &mut pidl,
            0,
            std::ptr::null_mut(),
        )
    };
    if hr_parse < 0 || pidl.is_null() {
        tracing::debug!(hr = format!("{hr_parse:#x}"), "SHParseDisplayName failed");
        // SAFETY: 与上方取得初始化引用的 CoInitializeEx 配对。
        unsafe { CoUninitialize() };
        return false;
    }

    // cidl=0 简写：pidlFolder 直接指向要选中的项，系统打开其父目录并选中它。
    // SAFETY: pidl 为 SHParseDisplayName 成功返回的有效 PIDL，调用后立即释放。
    let hr_select = unsafe { SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0) };
    // SAFETY: 释放 SHParseDisplayName 按 COM 分配器返回的 PIDL。
    unsafe { CoTaskMemFree(pidl.cast()) };
    // SAFETY: 与上方取得初始化引用的 CoInitializeEx 配对。
    unsafe { CoUninitialize() };
    if hr_select < 0 {
        tracing::debug!(
            hr = format!("{hr_select:#x}"),
            "SHOpenFolderAndSelectItems failed"
        );
        return false;
    }
    true
}

/// Windows：系统「打开目录」的默认处理程序是否已被替换成第三方文件管理器。
///
/// 读取 `HKCR\Directory\shell\<默认 verb>\command` 并解析其可执行文件名。
/// 非 `explorer.exe` 时返回 `true`；键缺失、读取失败或仍是 Explorer 时返回
/// `false`（保留 Shell API 的选中体验）。`<默认 verb>` 取 `Directory\shell`
/// 的默认值，为空或 `none` 时回退到 `open`（第三方替换的常用写法）。只改了
/// 此键的第三方 FM（OneCommander 等）拦截不到 `SHOpenFolderAndSelectItems`，
/// 必须靠它兜底。与 hub `reveal_file.rs` 的同名函数保持一致。
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

#[cfg(not(windows))]
fn set_no_console_window(_command: &mut std::process::Command) {}

/// 注册表命令行使用的桌面程序路径：canonicalize 解析符号链接后去掉 `\\?\`
/// 前缀，便于与安装器写入的值比较。
#[cfg(windows)]
fn registry_executable(desktop: Option<&Path>) -> Result<String, PlatformError> {
    let path = desktop.ok_or(PlatformError::Unsupported(
        "fluxdown-desktop.exe is not installed next to fluxdown-agent",
    ))?;
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = canonical.to_string_lossy();
    Ok(text.strip_prefix(r"\\?\").unwrap_or(&*text).to_owned())
}

#[cfg(windows)]
mod windows_shell {
    /// `SHChangeNotify(SHCNE_ASSOCCHANGED)` 通知资源管理器关联已变化。
    ///
    /// 直接声明 FFI，避免为一个符号引入 `windows-sys` 的 `Win32_UI_Shell`。
    pub fn notify_association_changed() {
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn SHChangeNotify(
                wEventId: i32,
                uFlags: u32,
                dwItem1: *const std::ffi::c_void,
                dwItem2: *const std::ffi::c_void,
            );
        }
        // SAFETY: SHCNE_ASSOCCHANGED (0x0800_0000) + SHCNF_IDLIST (0) 不读取
        // item 指针，传 null 合法。
        unsafe {
            SHChangeNotify(0x0800_0000, 0, std::ptr::null(), std::ptr::null());
        }
    }
}

#[cfg(target_os = "linux")]
mod xdg {
    use std::io::{BufRead, Write};

    use super::PlatformError;

    /// 打包安装的桌面入口（`linux/com.fluxdown.app.desktop`）。
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
    #[error("unknown URL scheme: {0}")]
    InvalidScheme(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_executable_lives_next_to_agent() {
        let current = std::env::current_exe().expect("current exe");
        let sibling = current.with_file_name(DESKTOP_EXECUTABLE_NAME);
        assert_eq!(desktop_executable(), sibling.is_file().then_some(sibling));
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
    fn should_launch_desktop_for_capture_requires_no_ui_clients_and_cooldown_elapsed() {
        let zero = AtomicUsize::new(0);
        let one = AtomicUsize::new(1);
        assert!(should_launch_desktop_for_capture(&zero, 0, 20_000));
        assert!(!should_launch_desktop_for_capture(&one, 0, 20_000));
        assert!(!should_launch_desktop_for_capture(&zero, 15_000, 20_000));
        assert!(should_launch_desktop_for_capture(&zero, 0, 10_000));
    }
}
