/// 在系统/用户指定的文件管理器中打开目录或定位文件。
///
/// 调用方传入：
/// - `path` — 文件或目录的绝对路径（自动检测类型）
/// - `tpl`  — 用户自定义文件管理器命令模板，空表示使用平台默认
///
/// 模板占位符（文件/目录共用同一条命令）：
/// - `{path}` — 当前路径：文件场景 = 文件完整路径，目录场景 = 目录路径
/// - `{dir}`  — 目录路径（文件 → 父目录，目录 → 自身）
///
/// 占位符在替换时会做平台 shell 转义，用户无需在模板中再加引号。
///
/// 平台默认行为（无模板时）：
/// | 平台    | 文件                                                   | 目录                          |
/// |---------|--------------------------------------------------------|-------------------------------|
/// | Windows | `SHOpenFolderAndSelectItems`（选中），失败回退 open 动词  | `ShellExecuteW("open", dir)`  |
/// | macOS   | `open -R path`                                         | `open path`                   |
/// | Linux   | D-Bus `FileManager1.ShowItems`，失败 fallback xdg-open | `xdg-open dir`                |
pub fn reveal(path: &str, tpl: &str) {
    use std::path::Path;

    // 判定 file/dir：路径若不存在则按"末段是否含 . "猜测，与 Dart 端旧逻辑一致。
    let p = Path::new(path);
    let is_file = match std::fs::metadata(p) {
        Ok(m) => m.is_file(),
        Err(_) => p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains('.'))
            .unwrap_or(false),
    };

    // 推算目录路径
    let dir: String = if is_file {
        p.parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string())
    } else {
        path.to_string()
    };

    // 优先走用户自定义模板（文件/目录共用一条，占位符 {path} 已按场景填好）
    if !tpl.trim().is_empty() {
        if run_template(tpl, path, &dir) {
            return;
        }
        crate::logger::log_info!(
            "[reveal] custom template failed, falling back to platform default"
        );
    }

    // 平台默认
    if is_file {
        platform_reveal_file(path);
    } else {
        platform_open_dir(&dir);
    }
}

/// 用系统默认程序打开文件（等价于在资源管理器里双击）。
///
/// 关键点：
/// - 必须用**裸路径**而非 `file://` URL。Windows 上 UWP/Store 应用注册的文件
///   处理器（如 .mp4 默认的“电影和电视”/媒体播放器）无法通过 `file://` URL 被
///   ShellExecute 激活，表现为“点开没反应”；裸路径与双击一致，能正确解析扩展名
///   关联（含 UWP 与经典 Win32 handler）。
/// - Windows 走 `explorer.exe <file>` 而非 `cmd /c start`：explorer 把打开动作
///   委托给已运行的用户态 shell（中完整性级别），因此即便 App 以管理员身份运行也
///   能激活 UWP 关联应用（提权进程直接激活 UWP 会被系统拒绝），且无 cmd 黑框闪
///   烁。文件场景不存在“强制用 Explorer 当文件管理器”的顾虑（那是打开目录才有）。
/// - **压缩包例外**：explorer.exe 收到 .zip/.rar 等路径会直接以“压缩文件夹”
///   浏览进去，无视第三方压缩软件的关联。检测到第三方默认 handler 时直接调
///   `ShellExecuteW`（详见 `shell_execute_open` / `archive_handler_is_third_party`）。
///
/// | 平台    | 命令                |
/// |---------|---------------------|
/// | Windows | `explorer.exe path`（第三方关联的压缩包 → `ShellExecuteW`） |
/// | macOS   | `open path`         |
/// | Linux   | `xdg-open path`     |
pub fn open_file(path: &str) {
    #[cfg(target_os = "windows")]
    {
        // 压缩包特例：explorer.exe 收到 .zip/.rar 等路径会直接以"压缩文件夹"
        // 浏览进去，无视用户注册的第三方压缩软件关联。若检测到该扩展名的默认
        // 处理程序是第三方（7-Zip/WinRAR/Bandizip 等），直接调 ShellExecuteW
        // （与 Telegram/Chromium/Qt 的做法一致，即"双击"的 API 本体），尊重
        // 关联。压缩软件均为 Win32 程序，不受"提权进程无法激活 UWP"的限制。
        if is_archive(path) && archive_handler_is_third_party(path) {
            if shell_execute_open(path) {
                return;
            }
            crate::logger::log_info!(
                "[open_file] ShellExecuteW failed; falling back to explorer.exe"
            );
        }

        if let Err(e) = std::process::Command::new("explorer.exe").arg(path).spawn() {
            crate::logger::log_info!("[open_file] explorer.exe failed: {e}");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = std::process::Command::new("open").arg(path).spawn() {
            crate::logger::log_info!("[open_file] open failed: {e}");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = std::process::Command::new("xdg-open").arg(path).spawn() {
            crate::logger::log_info!("[open_file] xdg-open failed: {e}");
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        crate::logger::log_info!("[open_file] not supported on this platform: {path}");
    }
}

/// 直接调 Win32 `ShellExecuteW`（"open" 默认 verb）打开路径——这是双击的
/// API 本体，Telegram Desktop / Chromium / Qt `QDesktopServices` 的同款实现。
/// 相比 `cmd /c start`：不额外起 cmd 进程、无引号转义问题、天然无黑框。
/// 返回值 > 32 表示成功（Win32 约定）。
#[cfg(target_os = "windows")]
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
/// 文档要求先 CoInitialize：本函数运行在 spawn_blocking/NMH 线程上，这里
/// 做防御性初始化（同 `shortcut_icon.rs` 的模式）——`hr < 0` 视为失败；
/// S_OK/S_FALSE 都会取得本线程初始化引用，结尾须配对 `CoUninitialize`。
#[cfg(target_os = "windows")]
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
        crate::logger::log_info!("[reveal] CoInitializeEx failed: {hr:#x}");
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
        crate::logger::log_info!("[reveal] SHParseDisplayName failed: {hr_parse:#x}");
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
        crate::logger::log_info!("[reveal] SHOpenFolderAndSelectItems failed: {hr_select:#x}");
        return false;
    }
    true
}

/// 常见压缩包扩展名（含复合扩展名 .tar.gz 等——只需匹配末段即可）。
#[cfg(target_os = "windows")]
fn is_archive(path: &str) -> bool {
    const ARCHIVE_EXTS: &[&str] = &[
        "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst", "lz4", "cab",
        "arj", "lzh", "wim",
    ];
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            ARCHIVE_EXTS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// Windows：该文件扩展名的默认打开程序是否为第三方（非 Explorer）。
///
/// 解析顺序与 Explorer 双击一致：
/// 1. `HKCU\...\Explorer\FileExts\<.ext>\UserChoice` 的 `ProgId`（用户在
///    "打开方式→始终"里选择的结果，优先级最高）
/// 2. 回退 `HKCR\<.ext>` 默认值指向的 ProgId
///
/// 再读 `HKCR\<ProgId>\shell\open\command` 解析可执行文件名。ProgId 为
/// `CompressedFolder`（Explorer 内建 zip）或命令解析为 explorer.exe /
/// 解析失败时返回 `false`（保持 explorer.exe 现状，行为不回退）。
#[cfg(target_os = "windows")]
fn archive_handler_is_third_party(path: &str) -> bool {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};

    let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    else {
        return false;
    };
    let ext = format!(".{}", ext.to_ascii_lowercase());

    // 1) UserChoice ProgId（用户显式选择）
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let user_choice = hkcu
        .open_subkey(format!(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{ext}\UserChoice"
        ))
        .and_then(|k| k.get_value::<String, _>("ProgId"))
        .ok();

    // 2) 回退 HKCR\<.ext> 默认值
    let hkcr = RegKey::predef(HKEY_CLASSES_ROOT);
    let progid = match user_choice.filter(|p| !p.trim().is_empty()) {
        Some(p) => p,
        None => match hkcr
            .open_subkey(&ext)
            .and_then(|k| k.get_value::<String, _>(""))
        {
            Ok(p) if !p.trim().is_empty() => p,
            _ => return false,
        },
    };

    // Explorer 内建压缩文件夹 handler（zip/cab 默认）。
    if progid.eq_ignore_ascii_case("CompressedFolder") || progid.eq_ignore_ascii_case("CABFolder") {
        return false;
    }

    let Ok(cmd) = hkcr
        .open_subkey(format!(r"{progid}\shell\open\command"))
        .and_then(|k| k.get_value::<String, _>(""))
    else {
        return false;
    };
    match exe_basename(&cmd) {
        Some(name) => !name.eq_ignore_ascii_case("explorer.exe"),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// 模板执行：占位符替换 + shell 解析
// ---------------------------------------------------------------------------
//
// 设计理由：
// 用户提供的命令是字符串（含空格、引号、管道等），最稳的执行方式是交给系统
// shell 解析。Windows 用 `cmd /c`，Unix 用 `sh -c`。占位符替换前对路径做
// 平台 shell 转义，用户在模板里写 `nautilus --select {path}` 即可，不需要
// 自己包引号。

/// 构造传给 `cmd.exe /c` 的参数：把整条用户命令再包一层最外层引号。
///
/// 必不可少。当 `cmdline` 以引号开头且含超过两个引号时（可执行文件装在含
/// 空格的目录，如 `C:\Program Files\...`，叠加被 shell_quote 包裹的
/// `{path}`/`{dir}`），`cmd /c` 会剥掉命令行的首尾引号，把 exe 路径从空格
/// 处截断（报 `'C:\Program' is not recognized`）。外层引号确保 cmd 剥掉的
/// 是这一层，还原出完整的用户命令。规则见 `cmd /?`。
///
/// Windows `CREATE_NO_WINDOW`：cmd.exe 是控制台程序，不设此标志会闪现黑框。
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(target_os = "windows")]
fn windows_cmd_c_arg(cmdline: &str) -> String {
    format!("/c \"{cmdline}\"")
}

fn run_template(tpl: &str, path: &str, dir: &str) -> bool {
    let cmdline = substitute(tpl, path, dir);
    crate::logger::log_info!("[reveal] running custom: {cmdline}");

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // 见 windows_cmd_c_arg：整条命令须再包一层外层引号，否则 cmd /c
        // 会剥掉用户命令的首尾引号（exe 装在含空格目录时把路径截断）。
        match std::process::Command::new("cmd.exe")
            .raw_arg(windows_cmd_c_arg(&cmdline))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(_) => true,
            Err(e) => {
                crate::logger::log_info!("[reveal] cmd /c spawn failed: {e}");
                false
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        match std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmdline)
            .spawn()
        {
            Ok(_) => true,
            Err(e) => {
                crate::logger::log_info!("[reveal] sh -c spawn failed: {e}");
                false
            }
        }
    }
}

fn substitute(tpl: &str, path: &str, dir: &str) -> String {
    let path_q = shell_quote(path);
    let dir_q = shell_quote(dir);
    tpl.replace("{path}", &path_q).replace("{dir}", &dir_q)
}

#[cfg(target_os = "windows")]
fn shell_quote(s: &str) -> String {
    // cmd 引号规则：包在 "..." 中；内层 " 在 cmd 上下文里需写成 \"，
    // 同时为了对付 cmd 的 ^ & | < > 等元字符，整串再用 ^ 转义会破坏路径，
    // 所以最务实做法是禁止路径中出现 "（实际文件名也不允许 " 字符）。
    if s.contains('"') {
        // 极端兜底：替换为下划线避免命令注入
        let cleaned: String = s.chars().map(|c| if c == '"' { '_' } else { c }).collect();
        format!("\"{cleaned}\"")
    } else {
        format!("\"{s}\"")
    }
}

#[cfg(not(target_os = "windows"))]
fn shell_quote(s: &str) -> String {
    // POSIX 单引号转义：单引号本身写成 '\''
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

// ---------------------------------------------------------------------------
// 平台默认：reveal 文件（标准 Shell API 选中，失败回退 open 动词）
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn platform_reveal_file(path: &str) {
    // 优先走标准 Shell API「打开父目录并选中」：SHOpenFolderAndSelectItems
    // 由系统 Shell 打开文件夹视图并选中目标，不硬编码 explorer.exe（见
    // sh_open_folder_and_select）。explorer /select 是 Explorer 私有语法、
    // 会绕过 open 关联，不再使用。API 失败时回退 open 动词打开父目录
    // （不选中），保证至少有响应。
    if sh_open_folder_and_select(path) {
        return;
    }
    crate::logger::log_info!(
        "[reveal] SHOpenFolderAndSelectItems failed; falling back to open verb"
    );
    let dir = std::path::Path::new(path)
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    platform_open_dir(&dir);
}

/// 返回裸路径字符串中首个（不区分大小写）以 `.exe` 结尾的字节偏移；找不到
/// 时返回 `None`。`.exe` 全为 ASCII，`to_ascii_lowercase` 不改变字节长度
/// 与 UTF-8 边界，返回的偏移量可直接用于原字符串按字节切片。
#[cfg(target_os = "windows")]
fn find_exe_end(cmd: &str) -> Option<usize> {
    cmd.to_ascii_lowercase().find(".exe").map(|idx| idx + 4)
}

/// 从注册表 shell command 字符串解析出可执行文件的文件名（basename）。
/// 支持带引号路径（`"C:\..\fm.exe" "%1"`）与裸路径
/// (`%SystemRoot%\Explorer.exe /idlist,...`)；返回 `None` 表示无法解析。
#[cfg(target_os = "windows")]
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

#[cfg(target_os = "macos")]
fn platform_reveal_file(path: &str) {
    if let Err(e) = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
    {
        crate::logger::log_info!("[reveal] open -R failed: {e}");
    }
}

#[cfg(target_os = "linux")]
fn platform_reveal_file(path: &str) {
    let uri = path_to_file_uri(path);
    let ok = std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("array:string:{uri}"),
            "string:",
        ])
        .spawn()
        .map(|mut c| c.wait().map(|s| s.success()).unwrap_or(false))
        .unwrap_or(false);

    if !ok {
        let dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        platform_open_dir(&dir);
    }
}

/// Android/iOS 等移动平台：无桌面文件管理器概念，仅记日志。
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn platform_reveal_file(path: &str) {
    crate::logger::log_info!("[reveal] reveal file not supported on this platform: {path}");
}

// ---------------------------------------------------------------------------
// 平台默认：打开目录（不选中）
// ---------------------------------------------------------------------------
//
// Windows: ShellExecuteW("open") —— 微软官方的"打开"调用（双击的 API 本体），
// 系统按 Directory/Folder 的 open 动词关联解析默认文件管理器；`cmd /c start`
// 仅作 ShellExecuteW 失败时的回退（start 内部同样走该关联）。
// macOS: open <dir> 走 LaunchServices，尊重 `public.folder` 默认 handler。
// Linux: xdg-open 走 mimeapps.list 的 inode/directory 默认。

#[cfg(target_os = "windows")]
fn platform_open_dir(dir: &str) {
    if shell_execute_open(dir) {
        return;
    }
    crate::logger::log_info!("[reveal] ShellExecuteW failed; falling back to cmd /c start");
    use std::os::windows::process::CommandExt;
    // start 的第一个引号串是窗口标题，必须保留为空，否则 cmd 会把目录路径
    // 当成标题而打开新 cmd 窗口。
    let arg = format!(r#"/c start "" "{}""#, dir);
    if let Err(e) = std::process::Command::new("cmd.exe")
        .raw_arg(&arg)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        crate::logger::log_info!("[reveal] cmd /c start failed: {e}");
    }
}

#[cfg(target_os = "macos")]
fn platform_open_dir(dir: &str) {
    if let Err(e) = std::process::Command::new("open").arg(dir).spawn() {
        crate::logger::log_info!("[reveal] open dir failed: {e}");
    }
}

#[cfg(target_os = "linux")]
fn platform_open_dir(dir: &str) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(dir).spawn() {
        crate::logger::log_info!("[reveal] xdg-open failed: {e}");
    }
}

/// Android/iOS 等移动平台：无桌面文件管理器概念，仅记日志。
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn platform_open_dir(dir: &str) {
    crate::logger::log_info!("[reveal] open dir not supported on this platform: {dir}");
}

#[cfg(target_os = "linux")]
fn path_to_file_uri(path: &str) -> String {
    let encoded: String = path
        .chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                c.to_string()
                    .as_bytes()
                    .iter()
                    .flat_map(|b| format!("%{b:02X}").chars().collect::<Vec<_>>())
                    .collect()
            }
        })
        .collect();
    format!("file://{encoded}")
}

#[cfg(all(test, target_os = "windows"))]
mod exe_basename_tests {
    use super::exe_basename;

    #[test]
    fn quoted_path_with_spaces_and_arg_returns_exe_name() {
        assert_eq!(
            exe_basename("\"C:\\Program Files\\OneCommander\\OneCommander.exe\" \"%1\""),
            Some("OneCommander.exe".to_string())
        );
    }

    #[test]
    fn bare_path_with_env_var_and_idlist_args_preserves_case() {
        assert_eq!(
            exe_basename("%SystemRoot%\\Explorer.exe /idlist,%I,%L"),
            Some("Explorer.exe".to_string())
        );
    }

    #[test]
    fn forward_slash_path_with_trailing_arg() {
        assert_eq!(
            exe_basename("C:/tools/fm.exe arg"),
            Some("fm.exe".to_string())
        );
    }

    #[test]
    fn quoted_path_without_extra_args() {
        assert_eq!(
            exe_basename("\"C:\\a b\\fm.exe\""),
            Some("fm.exe".to_string())
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(exe_basename("   C:\\x\\y.exe  "), Some("y.exe".to_string()));
    }

    #[test]
    fn empty_string_returns_none() {
        assert_eq!(exe_basename(""), None);
    }

    #[test]
    fn whitespace_only_returns_none() {
        assert_eq!(exe_basename("   "), None);
    }

    #[test]
    fn bare_path_with_spaces_and_quoted_percent1_arg_returns_exe_name() {
        assert_eq!(
            exe_basename("C:\\Program Files\\OneCommander\\OneCommander.exe -\"%1\""),
            Some("OneCommander.exe".to_string())
        );
    }

    #[test]
    fn bare_path_with_spaces_and_no_args_returns_exe_name() {
        assert_eq!(
            exe_basename("C:\\Program Files\\App\\App.exe"),
            Some("App.exe".to_string())
        );
    }

    #[test]
    fn bare_path_with_spaces_uppercase_extension_preserves_case() {
        assert_eq!(
            exe_basename("C:\\Tools\\FM.EXE /x"),
            Some("FM.EXE".to_string())
        );
    }
}

#[cfg(all(test, target_os = "windows"))]
mod cmd_arg_tests {
    use super::windows_cmd_c_arg;

    #[test]
    fn wraps_whole_command_in_outer_quotes() {
        let got = windows_cmd_c_arg(r#""C:\Program Files\app\a.exe" /x "C:\d ir""#);
        assert_eq!(got, r#"/c ""C:\Program Files\app\a.exe" /x "C:\d ir"""#);
        // 首尾必须是引号：cmd /c 剥掉这层后还原出用户的完整命令。
        assert!(got.starts_with("/c \""));
        assert!(got.ends_with('"'));
    }
}
