//! 浏览器扩展一键安装（设置 → 关于）：检测本机 Chrome / Edge / Firefox 与已装扩展版本，
//! 登记指向官方商店的外部扩展并在目标浏览器打开安装页。商店地址、扩展 ID 与更新源见
//! [`fluxdown_protocol::browser_extension`]。
//!
//! 外部扩展登记（与迅雷 / IDM 同一机制，只在用户点击时执行，均为用户级、无需提权）：
//! ```text
//! Windows  HKCU\Software\Google\Chrome\Extensions\<id>   update_url = <商店更新源>
//!          HKCU\Software\Microsoft\Edge\Extensions\<id>  update_url = <商店更新源>
//! macOS    ~/Library/Application Support/{Google/Chrome,Microsoft Edge}/External Extensions/<id>.json
//! Linux    ~/.config/microsoft-edge/External Extensions/<id>.json（Edge；Google Chrome 只认 root 目录，不登记）
//! ```
//! 浏览器从商店拉取最新版本，并弹出「已添加新扩展」由用户启用（Windows 运行中即时生效，
//! 其余平台下次启动时生效）；之后随商店自动更新。删除登记项会让浏览器移除该扩展，因此只在
//! 卸载 FluxDown 时清理（`installer/windows/setup.iss`，两处需保持同步）。Firefox 不支持
//! 外部程序安装，改用 Firefox 打开 AMO 最新签名包触发安装确认。全部函数同步阻塞。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use fluxdown_protocol::browser_extension::{
    BrowserExtensionEntryDto, BrowserExtensionInstallResult, BrowserExtensionStatusDto, BrowserKind,
};

use super::PlatformError;

/// 本机各浏览器的扩展安装状态。
#[must_use]
pub fn status() -> BrowserExtensionStatusDto {
    BrowserExtensionStatusDto {
        browsers: BrowserKind::ALL.into_iter().map(entry).collect(),
    }
}

fn entry(kind: BrowserKind) -> BrowserExtensionEntryDto {
    BrowserExtensionEntryDto {
        browser: kind,
        browser_installed: find_browser(kind).is_some(),
        extension_version: installed_version(kind),
        // 用户在浏览器里移除过外部扩展后，浏览器永久忽略该登记（`extensions.external_uninstalls`），
        // 不再算「已登记待启用」，界面回到可安装（走商店页）。
        external_registered: external::is_registered(kind) && !external_uninstalled(kind),
        external_supported: external::supported(kind),
    }
}

/// 为 `kind` 安装 FluxDown 扩展：平台支持时登记外部扩展，再用该浏览器打开安装页。
///
/// 未检测到浏览器返回 [`PlatformError::NotFound`]。登记失败不阻断：仍打开商店页让用户手动添加。
/// 用户在浏览器里移除过外部扩展时登记不再生效，只打开商店页（结果 `external_registered = false`）。
pub fn install(kind: BrowserKind) -> Result<BrowserExtensionInstallResult, PlatformError> {
    let browser =
        find_browser(kind).ok_or_else(|| PlatformError::NotFound(PathBuf::from(kind.label())))?;
    let blocked = external_uninstalled(kind);
    let external_registered = external::supported(kind)
        && !blocked
        && match external::register(kind) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(browser = kind.as_str(), error = %error, "external extension registration failed; opening the store page only");
                false
            }
        };
    launch(&browser, kind.install_url())?;
    tracing::info!(
        browser = kind.as_str(),
        external_registered,
        external_uninstalled = blocked,
        "browser extension install started"
    );
    Ok(BrowserExtensionInstallResult {
        external_registered,
    })
}

/// 任一 Chromium 配置文件记录过用户移除该外部扩展。
fn external_uninstalled(kind: BrowserKind) -> bool {
    kind.external_update_url().is_some()
        && chromium_user_data_dirs(kind)
            .iter()
            .any(|dir| chromium_external_uninstalled(dir, kind.extension_id()))
}

/// Chromium 用户数据目录下各配置文件的 `Preferences` / `Secure Preferences` 里
/// `extensions.external_uninstalls` 是否含 `id`。
fn chromium_external_uninstalled(user_data: &Path, id: &str) -> bool {
    let Ok(profiles) = std::fs::read_dir(user_data) else {
        return false;
    };
    profiles
        .filter_map(Result::ok)
        .flat_map(|profile| {
            let path = profile.path();
            [path.join("Preferences"), path.join("Secure Preferences")]
        })
        .filter_map(|file| std::fs::read(file).ok())
        .any(|bytes| prefs_list_external_uninstall(&bytes, id))
}

fn prefs_list_external_uninstall(prefs: &[u8], id: &str) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(prefs) else {
        return false;
    };
    value
        .pointer("/extensions/external_uninstalls")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|ids| ids.iter().any(|entry| entry.as_str() == Some(id)))
}

// ---------------------------------------------------------------------------
// 已装版本检测（只在用户打开关于页 / 点击时读取，不做周期扫描）。
// ---------------------------------------------------------------------------

fn installed_version(kind: BrowserKind) -> Option<String> {
    let versions = match kind {
        BrowserKind::Firefox => firefox_profile_roots()
            .iter()
            .filter_map(|root| firefox_version(root, kind.extension_id()))
            .collect::<Vec<_>>(),
        BrowserKind::Chrome | BrowserKind::Edge => chromium_user_data_dirs(kind)
            .iter()
            .filter_map(|dir| chromium_version(dir, kind.extension_id()))
            .collect(),
    };
    highest(versions)
}

/// Chromium 用户数据目录下各配置文件（含 `Preferences` 的子目录）里该扩展的最高版本。
/// 扩展解包在 `<profile>/Extensions/<id>/<version>_<n>/`。
fn chromium_version(user_data: &Path, id: &str) -> Option<String> {
    let profiles = std::fs::read_dir(user_data).ok()?;
    let versions = profiles
        .filter_map(Result::ok)
        .map(|profile| profile.path())
        .filter(|profile| profile.join("Preferences").is_file())
        .filter_map(|profile| std::fs::read_dir(profile.join("Extensions").join(id)).ok())
        .flat_map(|dirs| dirs.filter_map(Result::ok))
        .filter(|dir| dir.path().join("manifest.json").is_file())
        .filter_map(|dir| dir.file_name().to_str().map(chromium_dir_version))
        .collect();
    highest(versions)
}

/// `0.6.8_0` → `0.6.8`（去掉 Chromium 的安装序号后缀）。
fn chromium_dir_version(name: &str) -> String {
    match name.rsplit_once('_') {
        Some((version, serial)) if serial.chars().all(|c| c.is_ascii_digit()) => version.to_owned(),
        _ => name.to_owned(),
    }
}

/// Firefox 配置文件根（`Profiles/` 或 `~/.mozilla/firefox/`）下各配置文件 `extensions.json` 里该扩展的最高版本。
fn firefox_version(root: &Path, id: &str) -> Option<String> {
    let profiles = std::fs::read_dir(root).ok()?;
    let versions = profiles
        .filter_map(Result::ok)
        .filter_map(|profile| std::fs::read(profile.path().join("extensions.json")).ok())
        .filter_map(|bytes| firefox_addon_version(&bytes, id))
        .collect();
    highest(versions)
}

fn firefox_addon_version(extensions_json: &[u8], id: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(extensions_json).ok()?;
    value
        .get("addons")?
        .as_array()?
        .iter()
        .find(|addon| addon.get("id").and_then(serde_json::Value::as_str) == Some(id))?
        .get("version")?
        .as_str()
        .map(str::to_owned)
}

/// 按点分数字比较取最高版本（非数字段按 0）。
fn highest(versions: Vec<String>) -> Option<String> {
    let key = |version: &String| -> Vec<u64> {
        version
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    };
    versions.into_iter().max_by(|a, b| key(a).cmp(&key(b)))
}

fn base_dirs() -> Option<directories::BaseDirs> {
    directories::BaseDirs::new()
}

/// Chromium 系浏览器的用户数据目录（含 Linux Flatpak 布局）。
fn chromium_user_data_dirs(kind: BrowserKind) -> Vec<PathBuf> {
    let Some(dirs) = base_dirs() else {
        return Vec::new();
    };
    chromium_user_data_dirs_in(kind, &dirs)
}

#[cfg(windows)]
fn chromium_user_data_dirs_in(kind: BrowserKind, dirs: &directories::BaseDirs) -> Vec<PathBuf> {
    let local = dirs.data_local_dir();
    match kind {
        BrowserKind::Chrome => vec![local.join("Google").join("Chrome").join("User Data")],
        BrowserKind::Edge => vec![local.join("Microsoft").join("Edge").join("User Data")],
        BrowserKind::Firefox => Vec::new(),
    }
}

#[cfg(target_os = "macos")]
fn chromium_user_data_dirs_in(kind: BrowserKind, dirs: &directories::BaseDirs) -> Vec<PathBuf> {
    let support = dirs.home_dir().join("Library").join("Application Support");
    match kind {
        BrowserKind::Chrome => vec![support.join("Google").join("Chrome")],
        BrowserKind::Edge => vec![support.join("Microsoft Edge")],
        BrowserKind::Firefox => Vec::new(),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn chromium_user_data_dirs_in(kind: BrowserKind, dirs: &directories::BaseDirs) -> Vec<PathBuf> {
    let home = dirs.home_dir();
    let config = home.join(".config");
    let flatpak = home.join(".var").join("app");
    match kind {
        BrowserKind::Chrome => vec![
            config.join("google-chrome"),
            flatpak
                .join("com.google.Chrome")
                .join("config")
                .join("google-chrome"),
        ],
        BrowserKind::Edge => vec![
            config.join("microsoft-edge"),
            flatpak
                .join("com.microsoft.Edge")
                .join("config")
                .join("microsoft-edge"),
        ],
        BrowserKind::Firefox => Vec::new(),
    }
}

/// Firefox 配置文件根目录（其下每个子目录是一个配置文件）。
fn firefox_profile_roots() -> Vec<PathBuf> {
    let Some(dirs) = base_dirs() else {
        return Vec::new();
    };
    firefox_profile_roots_in(&dirs)
}

#[cfg(windows)]
fn firefox_profile_roots_in(dirs: &directories::BaseDirs) -> Vec<PathBuf> {
    vec![
        dirs.data_dir()
            .join("Mozilla")
            .join("Firefox")
            .join("Profiles"),
    ]
}

#[cfg(target_os = "macos")]
fn firefox_profile_roots_in(dirs: &directories::BaseDirs) -> Vec<PathBuf> {
    vec![
        dirs.home_dir()
            .join("Library")
            .join("Application Support")
            .join("Firefox")
            .join("Profiles"),
    ]
}

#[cfg(not(any(windows, target_os = "macos")))]
fn firefox_profile_roots_in(dirs: &directories::BaseDirs) -> Vec<PathBuf> {
    let home = dirs.home_dir();
    vec![
        home.join(".mozilla").join("firefox"),
        home.join(".var")
            .join("app")
            .join("org.mozilla.firefox")
            .join(".mozilla")
            .join("firefox"),
        home.join("snap")
            .join("firefox")
            .join("common")
            .join(".mozilla")
            .join("firefox"),
    ]
}

// ---------------------------------------------------------------------------
// 外部扩展登记。
// ---------------------------------------------------------------------------

/// macOS / Linux 外部扩展偏好文件内容。
#[cfg(any(not(windows), test))]
fn external_pref_json(update_url: &str) -> String {
    serde_json::json!({ "external_update_url": update_url }).to_string()
}

#[cfg(windows)]
mod external {
    use fluxdown_protocol::browser_extension::BrowserKind;
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

    use crate::platform::PlatformError;

    /// 与 `installer/windows/setup.iss` 卸载清理的键一致。
    fn key_path(kind: BrowserKind) -> Option<String> {
        let root = match kind {
            BrowserKind::Chrome => r"Software\Google\Chrome\Extensions",
            BrowserKind::Edge => r"Software\Microsoft\Edge\Extensions",
            BrowserKind::Firefox => return None,
        };
        Some(format!(r"{root}\{}", kind.extension_id()))
    }

    pub fn supported(kind: BrowserKind) -> bool {
        kind.external_update_url().is_some()
    }

    pub fn is_registered(kind: BrowserKind) -> bool {
        let (Some(path), Some(url)) = (key_path(kind), kind.external_update_url()) else {
            return false;
        };
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(path, KEY_READ)
            .and_then(|key| key.get_value::<String, _>("update_url"))
            .is_ok_and(|value| value == url)
    }

    pub fn register(kind: BrowserKind) -> Result<(), PlatformError> {
        let (Some(path), Some(url)) = (key_path(kind), kind.external_update_url()) else {
            return Err(PlatformError::Unsupported(
                "this browser has no external extension registry",
            ));
        };
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(path)?;
        key.set_value("update_url", &url)?;
        Ok(())
    }
}

#[cfg(not(windows))]
mod external {
    use std::path::PathBuf;

    use fluxdown_protocol::browser_extension::BrowserKind;

    use crate::platform::PlatformError;

    /// 用户级 `External Extensions/` 目录；Linux 下 Flatpak 布局只在其配置目录已存在时登记。
    fn dirs(kind: BrowserKind) -> Vec<PathBuf> {
        let Some(base) = super::base_dirs() else {
            return Vec::new();
        };
        super::chromium_external_dirs_in(kind, &base)
    }

    pub fn supported(kind: BrowserKind) -> bool {
        kind.external_update_url().is_some() && !dirs(kind).is_empty()
    }

    pub fn is_registered(kind: BrowserKind) -> bool {
        let file = format!("{}.json", kind.extension_id());
        dirs(kind).iter().any(|dir| dir.join(&file).is_file())
    }

    pub fn register(kind: BrowserKind) -> Result<(), PlatformError> {
        let Some(url) = kind.external_update_url() else {
            return Err(PlatformError::Unsupported(
                "this browser has no external extension directory",
            ));
        };
        let targets = dirs(kind);
        if targets.is_empty() {
            return Err(PlatformError::Unsupported(
                "this browser has no user-level external extension directory",
            ));
        }
        let json = super::external_pref_json(url);
        let file = format!("{}.json", kind.extension_id());
        for dir in targets {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join(&file), &json)?;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn chromium_external_dirs_in(kind: BrowserKind, dirs: &directories::BaseDirs) -> Vec<PathBuf> {
    if kind.external_update_url().is_none() {
        return Vec::new();
    }
    chromium_user_data_dirs_in(kind, dirs)
        .into_iter()
        .map(|dir| dir.join("External Extensions"))
        .collect()
}

/// Linux：Google Chrome 品牌版只读 root 目录（`/opt/google/chrome/extensions/`），不登记；
/// Edge 读用户级 `~/.config/microsoft-edge/External Extensions/`。
#[cfg(not(any(windows, target_os = "macos")))]
fn chromium_external_dirs_in(kind: BrowserKind, dirs: &directories::BaseDirs) -> Vec<PathBuf> {
    if kind != BrowserKind::Edge {
        return Vec::new();
    }
    chromium_user_data_dirs_in(kind, dirs)
        .into_iter()
        .enumerate()
        .filter(|(index, dir)| *index == 0 || dir.is_dir())
        .map(|(_, dir)| dir.join("External Extensions"))
        .collect()
}

// ---------------------------------------------------------------------------
// 浏览器定位与拉起。
// ---------------------------------------------------------------------------

/// 可拉起的浏览器。
#[derive(Debug)]
enum Browser {
    /// 直接执行，URL 作为参数。
    #[cfg(not(target_os = "macos"))]
    Executable(PathBuf),
    /// macOS `.app` bundle，经 `open -a` 拉起。
    #[cfg(target_os = "macos")]
    Bundle(PathBuf),
}

fn launch(browser: &Browser, url: &str) -> Result<(), PlatformError> {
    let mut command = match browser {
        #[cfg(not(target_os = "macos"))]
        Browser::Executable(path) => {
            let mut command = std::process::Command::new(path);
            command.arg(url);
            command
        }
        #[cfg(target_os = "macos")]
        Browser::Bundle(bundle) => {
            let mut command = std::process::Command::new("/usr/bin/open");
            command.arg("-a").arg(bundle).arg(url);
            command
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    super::detach_from_agent(&mut command);
    let mut child = command.spawn()?;
    // 浏览器未运行时子进程就是浏览器本身：在独立线程回收，避免常驻 agent 留下僵尸进程。
    std::thread::Builder::new()
        .name("fluxdown-browser-reaper".to_owned())
        .spawn(move || {
            if let Err(error) = child.wait() {
                tracing::debug!(%error, "failed to reap the launched browser");
            }
        })?;
    Ok(())
}

#[cfg(windows)]
fn find_browser(kind: BrowserKind) -> Option<Browser> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};

    let exe = windows_exe_name(kind);
    let app_path = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
    let registered = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .filter_map(|root| {
            RegKey::predef(root)
                .open_subkey_with_flags(&app_path, KEY_READ)
                .and_then(|key| key.get_value::<String, _>(""))
                .ok()
        })
        .map(|value| PathBuf::from(value.trim().trim_matches('"')));
    let env_dir = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let fallback = windows_install_dirs(kind)
        .iter()
        .filter_map(|&(env, relative)| env_dir(env).map(|dir| dir.join(relative)));
    registered
        .chain(fallback)
        .find(|path| path.is_file())
        .map(Browser::Executable)
}

#[cfg(windows)]
fn windows_exe_name(kind: BrowserKind) -> &'static str {
    match kind {
        BrowserKind::Chrome => "chrome.exe",
        BrowserKind::Edge => "msedge.exe",
        BrowserKind::Firefox => "firefox.exe",
    }
}

/// App Paths 缺失时的常见安装位置（环境变量 + 相对路径）。
#[cfg(windows)]
fn windows_install_dirs(kind: BrowserKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        BrowserKind::Chrome => &[
            ("ProgramFiles", r"Google\Chrome\Application\chrome.exe"),
            ("ProgramFiles(x86)", r"Google\Chrome\Application\chrome.exe"),
            ("LOCALAPPDATA", r"Google\Chrome\Application\chrome.exe"),
        ],
        BrowserKind::Edge => &[
            (
                "ProgramFiles(x86)",
                r"Microsoft\Edge\Application\msedge.exe",
            ),
            ("ProgramFiles", r"Microsoft\Edge\Application\msedge.exe"),
        ],
        BrowserKind::Firefox => &[
            ("ProgramFiles", r"Mozilla Firefox\firefox.exe"),
            ("ProgramFiles(x86)", r"Mozilla Firefox\firefox.exe"),
            ("LOCALAPPDATA", r"Mozilla Firefox\firefox.exe"),
        ],
    }
}

#[cfg(target_os = "macos")]
fn find_browser(kind: BrowserKind) -> Option<Browser> {
    let bundle = match kind {
        BrowserKind::Chrome => "Google Chrome.app",
        BrowserKind::Edge => "Microsoft Edge.app",
        BrowserKind::Firefox => "Firefox.app",
    };
    let user_apps = base_dirs().map(|dirs| dirs.home_dir().join("Applications"));
    [Some(PathBuf::from("/Applications")), user_apps]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(bundle))
        .find(|path| path.is_dir())
        .map(Browser::Bundle)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn find_browser(kind: BrowserKind) -> Option<Browser> {
    let names: &[&str] = match kind {
        BrowserKind::Chrome => &["google-chrome-stable", "google-chrome"],
        BrowserKind::Edge => &["microsoft-edge-stable", "microsoft-edge"],
        BrowserKind::Firefox => &["firefox", "firefox-esr"],
    };
    let flatpak_id = match kind {
        BrowserKind::Chrome => "com.google.Chrome",
        BrowserKind::Edge => "com.microsoft.Edge",
        BrowserKind::Firefox => "org.mozilla.firefox",
    };
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    let mut candidates: Vec<PathBuf> = names
        .iter()
        .flat_map(|name| path_dirs.iter().map(move |dir| dir.join(name)))
        .collect();
    candidates.push(PathBuf::from("/var/lib/flatpak/exports/bin").join(flatpak_id));
    if let Some(dirs) = base_dirs() {
        candidates.push(
            dirs.home_dir()
                .join(".local/share/flatpak/exports/bin")
                .join(flatpak_id),
        );
    }
    if kind == BrowserKind::Firefox {
        candidates.push(PathBuf::from("/snap/bin/firefox"));
    }
    candidates
        .into_iter()
        .find(|path| is_executable(path))
        .map(Browser::Executable)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fluxdown-ext-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn chromium_version_reads_the_highest_unpacked_version_across_profiles() {
        let root = temp_dir("chromium");
        let id = "meleenglfggcmcajknpeeeiobnpfmahc";
        for (profile, version) in [("Default", "0.6.7_0"), ("Profile 1", "0.6.10_0")] {
            let dir = root.join(profile);
            std::fs::create_dir_all(dir.join("Extensions").join(id).join(version))
                .expect("create extension dir");
            std::fs::write(dir.join("Preferences"), b"{}").expect("write prefs");
            std::fs::write(
                dir.join("Extensions")
                    .join(id)
                    .join(version)
                    .join("manifest.json"),
                b"{}",
            )
            .expect("write manifest");
        }
        // 没有 Preferences 的目录不是配置文件；缺 manifest 的残留版本目录不算安装。
        std::fs::create_dir_all(
            root.join("System Profile")
                .join("Extensions")
                .join(id)
                .join("9.9.9_0"),
        )
        .expect("create stray dir");
        std::fs::create_dir_all(
            root.join("Default")
                .join("Extensions")
                .join(id)
                .join("1.0.0_0"),
        )
        .expect("create half-written dir");

        assert_eq!(chromium_version(&root, id).as_deref(), Some("0.6.10"));
        assert_eq!(chromium_version(&root, "other"), None);
        assert_eq!(chromium_version(&root.join("missing"), id), None);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn chromium_dir_version_strips_only_numeric_serials() {
        assert_eq!(chromium_dir_version("0.6.8_0"), "0.6.8");
        assert_eq!(chromium_dir_version("0.6.8_12"), "0.6.8");
        assert_eq!(chromium_dir_version("0.6.8"), "0.6.8");
        assert_eq!(chromium_dir_version("beta_x"), "beta_x");
    }

    #[test]
    fn firefox_version_reads_extensions_json_per_profile() {
        let root = temp_dir("firefox");
        let profile = root.join("abc.default-release");
        std::fs::create_dir_all(&profile).expect("create profile");
        std::fs::write(
            profile.join("extensions.json"),
            br#"{"addons":[{"id":"other@x","version":"9"},{"id":"fluxdown@fluxdown.app","version":"0.6.8.1"}]}"#,
        )
        .expect("write extensions.json");
        std::fs::create_dir_all(root.join("broken")).expect("create broken profile");
        std::fs::write(root.join("broken").join("extensions.json"), b"{").expect("write broken");

        assert_eq!(
            firefox_version(&root, "fluxdown@fluxdown.app").as_deref(),
            Some("0.6.8.1")
        );
        assert_eq!(firefox_version(&root, "missing@x"), None);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn external_uninstalls_are_read_from_any_profile_preferences() {
        let id = "meleenglfggcmcajknpeeeiobnpfmahc";
        assert!(prefs_list_external_uninstall(
            format!(r#"{{"extensions":{{"external_uninstalls":["x","{id}"]}}}}"#).as_bytes(),
            id
        ));
        assert!(!prefs_list_external_uninstall(
            br#"{"extensions":{"external_uninstalls":["x"]}}"#,
            id
        ));
        assert!(!prefs_list_external_uninstall(br#"{"extensions":{}}"#, id));
        assert!(!prefs_list_external_uninstall(b"{", id));

        let root = temp_dir("uninstalls");
        std::fs::create_dir_all(root.join("Default")).expect("profile");
        std::fs::create_dir_all(root.join("Profile 2")).expect("profile");
        std::fs::write(root.join("Default").join("Preferences"), b"{}").expect("prefs");
        assert!(!chromium_external_uninstalled(&root, id));
        std::fs::write(
            root.join("Profile 2").join("Secure Preferences"),
            format!(r#"{{"extensions":{{"external_uninstalls":["{id}"]}}}}"#),
        )
        .expect("secure prefs");
        assert!(chromium_external_uninstalled(&root, id));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn highest_compares_numerically() {
        let versions = ["0.6.9", "0.6.10", "0.6.2"].map(str::to_owned).to_vec();
        assert_eq!(highest(versions).as_deref(), Some("0.6.10"));
        assert_eq!(highest(Vec::new()), None);
    }

    #[test]
    fn external_pref_points_at_the_store_update_url() {
        let json: serde_json::Value = serde_json::from_str(&external_pref_json(
            fluxdown_protocol::browser_extension::CHROME_UPDATE_URL,
        ))
        .expect("valid json");
        assert_eq!(
            json["external_update_url"],
            "https://clients2.google.com/service/update2/crx"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_registers_chrome_and_edge_under_user_external_extensions() {
        let dirs = directories::BaseDirs::new().expect("base dirs");
        let chrome = chromium_external_dirs_in(BrowserKind::Chrome, &dirs);
        assert_eq!(chrome.len(), 1);
        assert!(
            chrome[0].ends_with("Library/Application Support/Google/Chrome/External Extensions")
        );
        let edge = chromium_external_dirs_in(BrowserKind::Edge, &dirs);
        assert!(
            edge[0].ends_with("Library/Application Support/Microsoft Edge/External Extensions")
        );
        assert!(chromium_external_dirs_in(BrowserKind::Firefox, &dirs).is_empty());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_registers_only_edge_and_never_google_chrome() {
        let dirs = directories::BaseDirs::new().expect("base dirs");
        assert!(chromium_external_dirs_in(BrowserKind::Chrome, &dirs).is_empty());
        let edge = chromium_external_dirs_in(BrowserKind::Edge, &dirs);
        assert!(edge[0].ends_with(".config/microsoft-edge/External Extensions"));
    }
}
