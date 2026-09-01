//! agent 任务文件打开、定位与官方桌面进程唤起。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

static DESKTOP_LAUNCHED: AtomicBool = AtomicBool::new(false);

pub fn open_task(task: &fluxdown_protocol::TaskDto) -> Result<(), PlatformError> {
    launch_path(&PathBuf::from(&task.save_dir).join(&task.file_name), false)
}

pub fn reveal_task(task: &fluxdown_protocol::TaskDto) -> Result<(), PlatformError> {
    launch_path(&PathBuf::from(&task.save_dir).join(&task.file_name), true)
}

/// 首个待确认捕获在无 UI 时只拉起一次同级桌面程序。
pub fn launch_desktop_once() -> Result<(), PlatformError> {
    if DESKTOP_LAUNCHED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let current = std::env::current_exe()?;
    let executable = current.with_file_name(if cfg!(windows) {
        "fluxdown-desktop.exe"
    } else {
        "fluxdown-desktop"
    });
    let mut command = std::process::Command::new(executable);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    set_no_console_window(&mut command);
    command.spawn()?;
    Ok(())
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
        // 「在文件夹中显示」：优先标准 Shell API「打开父目录并选中」
        // （SHOpenFolderAndSelectItems，与 CLaunch 的 openParentFolder /
        // hub reveal_file.rs 同款实现）；API 失败回退 open 动词——目录开
        // 自身、文件开父目录。explorer /select 是 Explorer 私有语法，不再
        // 使用。
        if !path.is_dir() && sh_open_folder_and_select(&path.to_string_lossy()) {
            return Ok(());
        }
        tracing::debug!(
            "reveal: SHOpenFolderAndSelectItems failed; falling back to ShellExecuteW open"
        );
        let target = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().unwrap_or(path).to_path_buf()
        };
        if !shell_execute_open(&target.to_string_lossy()) {
            return Err(PlatformError::Io(std::io::Error::other(
                "ShellExecuteW(\"open\") failed",
            )));
        }
        return Ok(());
    }
    let mut command = std::process::Command::new("cmd.exe");
    command.arg("/c").arg("start").arg("").arg(path);
    set_no_console_window(&mut command);
    command.spawn()?;
    Ok(())
}

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
/// 该项（见 MSDN 备注）。实现与 CLaunch 的 `openParentFolder` / hub
/// `reveal_file.rs` 的同名函数保持一致：`SHParseDisplayName` 解析绝对 PIDL
/// + `CoTaskMemFree` 释放 + 防御性 COM 初始化；失败返回 false，调用方回退
/// 为 open 动词打开父目录。
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
        tracing::debug!(hr = format!("{hr_select:#x}"), "SHOpenFolderAndSelectItems failed");
        return false;
    }
    true
}

#[cfg(windows)]
fn set_no_console_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn set_no_console_window(_command: &mut std::process::Command) {}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("platform action failed: {0}")]
    Io(#[from] std::io::Error),
}
