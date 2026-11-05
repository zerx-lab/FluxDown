//! Windows UAC：常驻 agent 与它拉起的桌面不以提权（完整管理员令牌）身份运行。
//!
//! 提权进程创建的命名管道（NMH 端点、桌面单实例通道）默认安全描述符的属主是 Administrators、
//! 不含当前用户，浏览器拉起的中继（中完整性）只能以只读打开，扩展一直「离线」到用户手动重启
//! （#951 / #787：以管理员身份运行的安装器 / winget 拉起桌面，桌面再拉起 agent，令牌一路继承）。
//! 本模块在 agent 启动时检出 UAC 拆分令牌的完整一半，经资源管理器以未提权身份重新拉起自己，
//! 等新进程确认接手后才退出；接手失败就继续以提权身份运行（诊断页报告该状态），不让 agent 消失。
#![cfg(windows)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION_TYPE, TOKEN_QUERY, TokenElevationType,
    TokenElevationTypeFull,
};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, CoCreateInstance, IDispatch, IServiceProvider,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    CSIDL_DESKTOP, IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellView, IShellWindows,
    SID_STopLevelBrowser, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{BSTR, Interface as _};

/// 一次性模式：`fluxdown-agent --launch-desktop [桌面参数…]`。以管理员身份运行的安装器经它拉起
/// 桌面，桌面就不会继承安装器的提权令牌。
pub const LAUNCH_DESKTOP_ARG: &str = "--launch-desktop";

/// 降权重启的接手标记：`--unelevated-handoff <uuid>`。新进程删除对应的待接手文件即确认接手。
const HANDOFF_ARG: &str = "--unelevated-handoff";
/// 等未提权实例确认接手的上限；资源管理器通常在 1 秒内拉起进程。
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(15);
const HANDOFF_POLL: Duration = Duration::from_millis(100);
/// 经资源管理器重新拉起时带不过去的启动期环境覆盖（新进程用资源管理器的环境块）：存在时不降权，
/// 否则新实例会悄悄回到默认的数据目录 / 端口 / daemon。
const UNFORWARDABLE_ENV: [&str; 7] = [
    "FLUXDOWN_AGENT_DATA_DIR",
    "FLUXDOWN_DATA_DIR",
    "FLUXDOWN_AGENT_TOKEN_FILE",
    "FLUXDOWN_AGENT_BIND",
    "FLUXDOWN_DAEMON_TOKEN_FILE",
    "FLUXDOWN_DAEMON_URL",
    "FLUXDOWN_DAEMON_BIN",
];

/// [`hand_off_if_elevated`] 的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    /// 照常启动本进程。
    Continue,
    /// 未提权实例已接手，本进程应直接退出。
    HandedOff,
}

/// 本进程是否持有 UAC 拆分令牌的完整（提权）一半。
///
/// 关闭 UAC 或内置 Administrator 账户是 `TokenElevationTypeDefault`：资源管理器本身也持有同一种
/// 令牌，进程之间没有完整性差异，不需要降权。
pub fn running_elevated() -> Result<bool, String> {
    // SAFETY: GetCurrentProcess 无前置条件，返回无需关闭的当前进程伪句柄。
    let process = unsafe { GetCurrentProcess() };
    let mut raw = HANDLE::default();
    // SAFETY: process 是当前进程伪句柄；raw 是有效的输出槽，成功后句柄立即交给 Token 守卫。
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw) }
        .map_err(|error| format!("OpenProcessToken failed: {error}"))?;
    let token = Token(raw);
    let mut kind = TOKEN_ELEVATION_TYPE::default();
    let size = u32::try_from(std::mem::size_of::<TOKEN_ELEVATION_TYPE>())
        .map_err(|_| "TOKEN_ELEVATION_TYPE size does not fit u32".to_owned())?;
    let mut written = 0_u32;
    let buffer = std::ptr::from_mut(&mut kind).cast();
    // SAFETY: token 持有带 TOKEN_QUERY 权限的有效令牌句柄；buffer 指向本栈上存活到调用返回的
    // TOKEN_ELEVATION_TYPE，长度参数与其大小一致；written 是有效的输出变量。
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevationType,
            Some(buffer),
            size,
            &mut written,
        )
    }
    .map_err(|error| format!("GetTokenInformation(TokenElevationType) failed: {error}"))?;
    Ok(kind == TokenElevationTypeFull)
}

/// 独占持有 `OpenProcessToken` 返回的令牌句柄。
struct Token(HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: self.0 由成功的 OpenProcessToken 返回、仅此守卫拥有，只在这里关闭一次。
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            tracing::debug!(%error, "could not close the process token");
        }
    }
}

/// 常驻 agent 启动前调用（日志已初始化）：先确认自己是否是降权重启出来的接手者，再在持有完整
/// 管理员令牌时把常驻职责交给经资源管理器拉起的未提权实例。
pub fn hand_off_if_elevated(args: &[String]) -> Startup {
    let relaunched = accept_handoff(args);
    match running_elevated() {
        Ok(false) => return Startup::Continue,
        Ok(true) => {}
        Err(error) => {
            tracing::warn!(%error, "could not read the UAC elevation of fluxdown-agent; continuing");
            return Startup::Continue;
        }
    }
    if relaunched {
        // 资源管理器自己就是提权的（极少见的配置）：再重启只会循环。
        tracing::warn!(
            "fluxdown-agent is still elevated after relaunching through the shell; continuing elevated, the browser extension cannot connect"
        );
        return Startup::Continue;
    }
    if let Some(variable) = UNFORWARDABLE_ENV
        .iter()
        .find(|name| std::env::var_os(name).is_some())
    {
        tracing::warn!(
            variable,
            "fluxdown-agent runs elevated, but this override cannot be forwarded through the shell; continuing elevated"
        );
        return Startup::Continue;
    }
    match relaunch_unelevated(args) {
        Ok(()) => {
            tracing::info!("fluxdown-agent was started elevated; an unelevated instance took over");
            Startup::HandedOff
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "could not hand off to an unelevated fluxdown-agent; continuing elevated, the browser extension cannot connect until FluxDown is started normally"
            );
            Startup::Continue
        }
    }
}

/// `--launch-desktop` 一次性模式：提权时经资源管理器以未提权身份拉起同级桌面程序，否则直接拉起。
/// 经资源管理器拉起失败时退回直接拉起，保证安装结束后界面一定出现（agent 自己仍会降权）。
pub fn launch_desktop(args: &[String]) -> Result<(), String> {
    let desktop = super::desktop_executable()
        .ok_or_else(|| "fluxdown-desktop is not installed next to fluxdown-agent".to_owned())?;
    let elevated = running_elevated().unwrap_or_else(|error| {
        tracing::warn!(%error, "could not read the UAC elevation; launching the desktop directly");
        false
    });
    if elevated {
        let forwarded: Vec<OsString> = args.iter().map(OsString::from).collect();
        let command_line = crate::permission::windows_command_line(&forwarded);
        match shell_execute_unelevated(&desktop, &command_line) {
            Ok(()) => {
                tracing::info!("launched fluxdown-desktop unelevated through the shell");
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(%error, "could not launch fluxdown-desktop unelevated; launching it directly");
            }
        }
    }
    std::process::Command::new(&desktop)
        .args(args)
        .spawn()
        .map(drop)
        .map_err(|error| format!("could not launch {}: {error}", desktop.display()))
}

/// 降权重启出来的进程：删除待接手文件，告诉等待中的提权实例可以退出。返回是否带接手标记。
fn accept_handoff(args: &[String]) -> bool {
    let Some(index) = args.iter().position(|arg| arg == HANDOFF_ARG) else {
        return false;
    };
    let Some(id) = args
        .get(index + 1)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
    else {
        tracing::warn!("ignoring a malformed unelevated hand-off marker");
        return true;
    };
    let path = match pending_path(id) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, "could not locate the unelevated hand-off marker");
            return true;
        }
    };
    match std::fs::remove_file(&path) {
        Ok(()) => tracing::info!("took over from the elevated fluxdown-agent"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("the elevated fluxdown-agent stopped waiting for this hand-off");
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not acknowledge the unelevated hand-off");
        }
    }
    true
}

/// 待接手文件：放在 agent 数据目录，提权与未提权实例按同一已知文件夹解析到同一位置。
fn pending_path(id: uuid::Uuid) -> Result<PathBuf, String> {
    let dir = crate::runtime::resolve_agent_data_dir().map_err(|error| error.to_string())?;
    Ok(dir.join(format!("unelevated-handoff-{}.pending", id.hyphenated())))
}

/// 经资源管理器以未提权身份重新拉起本程序（原参数 + 接手标记），等新实例删除待接手文件。
fn relaunch_unelevated(args: &[String]) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not locate fluxdown-agent: {error}"))?;
    let id = uuid::Uuid::new_v4();
    let pending = pending_path(id)?;
    if let Some(parent) = pending.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|error| format!("could not create {}: {error}", pending.display()))?;
    let mut forwarded: Vec<OsString> = args.iter().map(OsString::from).collect();
    forwarded.push(HANDOFF_ARG.into());
    forwarded.push(id.hyphenated().to_string().into());
    let command_line = crate::permission::windows_command_line(&forwarded);
    if let Err(error) = shell_execute_unelevated(&executable, &command_line) {
        discard_pending(&pending);
        return Err(error);
    }
    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    loop {
        match std::fs::symlink_metadata(&pending) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                discard_pending(&pending);
                return Err(format!("could not watch {}: {error}", pending.display()));
            }
            Ok(_) if Instant::now() >= deadline => {
                // 到点前一刻才确认的接手仍算成功：删除失败于 NotFound 即对方已删。
                return match std::fs::remove_file(&pending) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(format!(
                        "the unelevated fluxdown-agent did not take over within {}s; could not remove {}: {error}",
                        HANDOFF_TIMEOUT.as_secs(),
                        pending.display()
                    )),
                    Ok(()) => Err(format!(
                        "the unelevated fluxdown-agent did not take over within {}s",
                        HANDOFF_TIMEOUT.as_secs()
                    )),
                };
            }
            Ok(_) => std::thread::sleep(HANDOFF_POLL),
        }
    }
}

fn discard_pending(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not remove the hand-off marker");
        }
    }
}

/// 经桌面窗口的 Shell 自动化对象以资源管理器的（未提权）令牌与环境块打开 `file`
/// （Raymond Chen「How can I launch an unelevated process from my elevated process」的做法）。
/// 工作目录取 `file` 所在目录。成功只表示 Shell 接受了请求，不代表进程已启动。
fn shell_execute_unelevated(file: &Path, args: &str) -> Result<(), String> {
    let directory = file
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", file.display()))?;
    let csidl_desktop =
        i32::try_from(CSIDL_DESKTOP).map_err(|_| "CSIDL_DESKTOP does not fit i32".to_owned())?;
    let file = BSTR::from_wide(&wide(file.as_os_str()));
    let arguments = VARIANT::from(BSTR::from(args));
    let directory = VARIANT::from(BSTR::from_wide(&wide(directory.as_os_str())));
    let operation = VARIANT::from(BSTR::from("open"));
    let show = VARIANT::from(SW_SHOWNORMAL.0);
    let location = VARIANT::from(csidl_desktop);
    let root = VARIANT::default();
    let _com = super::windows_com::Com::init()?;
    // 闭包内的 COM 接口都在 `_com` 结束 apartment 之前释放。
    let shell = || -> windows::core::Result<()> {
        // SAFETY: 本线程已由 `_com` 初始化为 STA；ShellWindows 是资源管理器的进程外 coclass，
        // 返回的接口引用由类型化包装持有。
        let windows: IShellWindows =
            unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER) }?;
        let mut hwnd = 0_i32;
        // SAFETY: location / root 是存活到调用返回的 VARIANT（VT_I4 CSIDL_DESKTOP / VT_EMPTY）；
        // hwnd 是有效输出；找不到桌面窗口（S_FALSE + 空指针）时包装返回错误而非空接口。
        let desktop: IDispatch = unsafe {
            windows.FindWindowSW(&location, &root, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)
        }
        .map_err(|error| {
            if error.code().is_ok() {
                windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "the shell desktop window was not found",
                )
            } else {
                error
            }
        })?;
        let provider: IServiceProvider = desktop.cast()?;
        // SAFETY: SID_STopLevelBrowser 是 'static GUID；返回的浏览器接口由包装持有。
        let browser: IShellBrowser = unsafe { provider.QueryService(&SID_STopLevelBrowser) }?;
        // SAFETY: browser 是本 STA 上存活的接口；返回的视图接口由包装持有。
        let view: IShellView = unsafe { browser.QueryActiveShellView() }?;
        // SAFETY: SVGIO_BACKGROUND 请求视图背景对象，IID 由类型参数 IDispatch 给出。
        let background: IDispatch = unsafe { view.GetItemObject(SVGIO_BACKGROUND) }?;
        let folder_view: IShellFolderViewDual = background.cast()?;
        // SAFETY: folder_view 是存活的接口；返回的 Shell.Application 对象由包装持有。
        let application: IDispatch = unsafe { folder_view.Application() }?;
        let dispatch: IShellDispatch2 = application.cast()?;
        // SAFETY: 全部参数是存活到调用返回的 BSTR / VARIANT，资源管理器在调用内复制它们。
        unsafe { dispatch.ShellExecute(&file, &arguments, &directory, &operation, &show) }
    };
    shell().map_err(|error| format!("shell launch through the desktop window failed: {error}"))
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().collect()
}

#[cfg(test)]
mod tests {
    use super::{HANDOFF_ARG, accept_handoff};

    /// 没有接手标记的普通启动不是降权重启。
    #[test]
    fn plain_start_is_not_a_handoff() {
        assert!(!accept_handoff(&["--autostart".to_owned()]));
    }

    /// 标记存在但 id 不是 UUID：仍视为降权重启（防止再次重启循环），且不会拼出任意路径。
    #[test]
    fn malformed_marker_still_counts_as_relaunch() {
        assert!(accept_handoff(&[
            HANDOFF_ARG.to_owned(),
            "..\\evil".to_owned()
        ]));
        assert!(accept_handoff(&[HANDOFF_ARG.to_owned()]));
    }
}
