//! Chrome Native Messaging Host (NMH) manifest generation and registry registration.
//!
//! Registers `com.fluxdown.nmh` for Chrome, Edge, and Firefox so that the
//! browser extension can use `chrome.runtime.connectNative("com.fluxdown.nmh")`
//! to communicate with the FluxDown desktop app via the NMH relay binary.
//!
//! Registry keys (all HKCU — no admin required):
//!   Chrome:  `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.fluxdown.nmh`
//!   Edge:    `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\com.fluxdown.nmh`
//!   Firefox: `HKCU\Software\Mozilla\NativeMessagingHosts\com.fluxdown.nmh`
//!
//! Each key's default value points to a JSON manifest file that describes the NMH.

#[cfg(target_os = "windows")]
mod inner {
    use crate::logger::log_info;
    use serde::Serialize;
    use std::io;
    use std::path::{Path, PathBuf};
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    const NMH_NAME: &str = "com.fluxdown.nmh";
    const NMH_DESCRIPTION: &str = "FluxDown Native Messaging Host";
    const NMH_EXE_NAME: &str = "fluxdown_nmh.exe";

    /// Manifest filename for Chrome/Edge (contains `allowed_origins`).
    const MANIFEST_FILENAME_CHROMIUM: &str = "com.fluxdown.nmh.json";
    /// Manifest filename for Firefox (contains `allowed_extensions`, NO `allowed_origins`).
    /// Firefox schema validation (NativeManifests.sys.mjs via Schemas.normalize) rejects any
    /// field not in its native_manifest.json schema. `allowed_origins` is Chrome-only and
    /// causes Firefox to report "No such native application" (Bugzilla #1361459).
    const MANIFEST_FILENAME_FIREFOX: &str = "com.fluxdown.nmh.firefox.json";

    /// Chrome extension ID — pinned via `key` in wxt.config.ts manifest.
    const CHROME_EXTENSION_ID: &str = "chrome-extension://meleenglfggcmcajknpeeeiobnpfmahc/";

    /// Edge Add-ons store extension ID. Edge ignores the manifest `key` field, so
    /// its store build gets a different ID than Chrome and must be listed
    /// explicitly (Chromium native messaging `allowed_origins` has no wildcard).
    /// Without this, Edge store users get "Access to the specified native
    /// messaging host is forbidden" → extension stuck on "未连接".
    const EDGE_EXTENSION_ID: &str = "chrome-extension://nglkkjbogjghekbhhcnccnpfedjbdhhd/";

    /// Firefox extension ID (matches `browser_specific_settings.gecko.id` in manifest).
    const FIREFOX_EXTENSION_ID: &str = "fluxdown@fluxdown.app";

    /// Chromium (Chrome/Edge) NMH manifest — uses `allowed_origins`.
    #[derive(Serialize)]
    struct NmhManifestChromium {
        name: String,
        description: String,
        path: String,
        #[serde(rename = "type")]
        host_type: String,
        allowed_origins: Vec<String>,
    }

    /// Firefox NMH manifest — uses `allowed_extensions` ONLY.
    /// Firefox schema (native_manifest.json) does not define `allowed_origins`;
    /// including it causes schema validation to fail with "No such native application".
    #[derive(Serialize)]
    struct NmhManifestFirefox {
        name: String,
        description: String,
        path: String,
        #[serde(rename = "type")]
        host_type: String,
        allowed_extensions: Vec<String>,
    }

    /// Strip `\\?\` UNC prefix from a path string (if present).
    fn strip_unc_prefix(s: &str) -> String {
        s.strip_prefix(r"\\?\").unwrap_or(s).to_string()
    }

    /// Find the NMH executable, searching multiple locations.
    ///
    /// Search order:
    /// 1. Same directory as the current app exe (production deployment)
    /// 2. Cargo workspace `target/debug/` (development — `flutter run`)
    /// 3. Cargo workspace `target/release/` (development — release build)
    fn find_nmh_exe() -> Result<PathBuf, io::Error> {
        // 1. Next to current exe (production: NMH ships alongside the app)
        if let Ok(exe) = std::env::current_exe() {
            let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
            if let Some(dir) = canonical.parent() {
                let candidate = dir.join(NMH_EXE_NAME);
                if candidate.exists() {
                    log_info!(
                        "[nmh_registry] found NMH exe next to app: {}",
                        candidate.display()
                    );
                    return Ok(candidate);
                }
            }
        }

        // 2+3. Cargo workspace target directory (development)
        // CARGO_MANIFEST_DIR is baked in at compile time for the hub crate.
        // hub crate is at <workspace>/native/hub, so workspace root is 2 levels up.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = Path::new(manifest_dir).parent().and_then(|p| p.parent());

        if let Some(ws) = workspace_root {
            for profile in &["debug", "release"] {
                let candidate = ws.join("target").join(profile).join(NMH_EXE_NAME);
                if candidate.exists() {
                    log_info!(
                        "[nmh_registry] found NMH exe in cargo target: {}",
                        candidate.display()
                    );
                    return Ok(candidate);
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} not found. Build it with: cargo build -p fluxdown_nmh",
                NMH_EXE_NAME
            ),
        ))
    }

    /// Write two NMH manifest JSON files next to the NMH executable:
    /// - Chromium manifest (Chrome/Edge): contains `allowed_origins`
    /// - Firefox manifest: contains `allowed_extensions` ONLY (no `allowed_origins`)
    ///
    /// Returns `(chromium_manifest_path, firefox_manifest_path)`.
    fn write_manifests(nmh_exe: &Path) -> Result<(PathBuf, PathBuf), io::Error> {
        let nmh_path_str = strip_unc_prefix(&nmh_exe.to_string_lossy());
        let dir = nmh_exe
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no parent dir"))?;

        // Chromium manifest (Chrome + Edge)
        let chromium = NmhManifestChromium {
            name: NMH_NAME.to_string(),
            description: NMH_DESCRIPTION.to_string(),
            path: nmh_path_str.clone(),
            host_type: "stdio".to_string(),
            allowed_origins: vec![
                CHROME_EXTENSION_ID.to_string(),
                EDGE_EXTENSION_ID.to_string(),
            ],
        };
        let chromium_json = serde_json::to_string_pretty(&chromium)
            .map_err(|e| io::Error::other(format!("JSON serialize error: {}", e)))?;
        let chromium_path = dir.join(MANIFEST_FILENAME_CHROMIUM);
        std::fs::write(&chromium_path, chromium_json)?;

        // Firefox manifest — NO `allowed_origins` field (Bugzilla #1361459)
        let firefox = NmhManifestFirefox {
            name: NMH_NAME.to_string(),
            description: NMH_DESCRIPTION.to_string(),
            path: nmh_path_str,
            host_type: "stdio".to_string(),
            allowed_extensions: vec![FIREFOX_EXTENSION_ID.to_string()],
        };
        let firefox_json = serde_json::to_string_pretty(&firefox)
            .map_err(|e| io::Error::other(format!("JSON serialize error: {}", e)))?;
        let firefox_path = dir.join(MANIFEST_FILENAME_FIREFOX);
        std::fs::write(&firefox_path, firefox_json)?;

        Ok((chromium_path, firefox_path))
    }

    /// Chromium-family registry paths on Windows.
    ///
    /// Brave, Vivaldi, Opera and most other Chromium forks fall back to reading
    /// Chrome's `Software\Google\Chrome\NativeMessagingHosts` registry key when
    /// their own key is absent (verified via KeePassXC source and Chromium
    /// source).  Only Chrome and Edge need dedicated keys.
    const CHROMIUM_REG_PATHS: &[&str] = &[
        r"Software\Google\Chrome\NativeMessagingHosts",
        r"Software\Microsoft\Edge\NativeMessagingHosts",
    ];

    /// Register each browser's registry key pointing to its dedicated manifest.
    /// Chrome and Edge use the Chromium manifest; Firefox uses the Firefox-only manifest.
    /// Other Chromium browsers (Brave, Vivaldi, Opera) fall back to Chrome's key.
    fn register_registry(chromium_manifest: &str, firefox_manifest: &str) -> Result<(), io::Error> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        for reg_path in CHROMIUM_REG_PATHS {
            let full_path = format!("{}\\{}", reg_path, NMH_NAME);
            let (key, _) = hkcu.create_subkey_with_flags(&full_path, KEY_WRITE)?;
            key.set_value("", &chromium_manifest)?;
            log_info!("[nmh_registry] registered at HKCU\\{}", full_path);
        }

        let firefox_reg = format!("{}\\{}", r"Software\Mozilla\NativeMessagingHosts", NMH_NAME);
        let (key, _) = hkcu.create_subkey_with_flags(&firefox_reg, KEY_WRITE)?;
        key.set_value("", &firefox_manifest)?;
        log_info!("[nmh_registry] registered at HKCU\\{}", firefox_reg);

        Ok(())
    }

    /// Returns `true` if NMH registration is missing or stale and needs to be (re)written.
    ///
    /// Checks that:
    ///   1. Chrome/Edge registry keys exist and point to the Chromium manifest.
    ///   2. Each registered manifest file exists and references the current NMH exe.
    ///   3. The registered NMH's parent directory matches the current exe's directory
    ///      (detects version switches: dev → portable / installed).
    ///   4. Firefox is treated as optional — its absence does not trigger re-registration.
    ///
    /// If the NMH exe cannot be found, returns `true` so that `register()` can
    /// report the proper "exe not found" error.
    pub fn needs_update() -> bool {
        let Ok(nmh_exe) = find_nmh_exe() else {
            return true;
        };
        // 清单由 serde_json 写出，路径中的 `\` 被转义为 `\\`；
        // 用转义后的形式做内容匹配，否则 Windows 上永远不匹配、每次启动都重注册。
        let expected_exe_json = strip_unc_prefix(&nmh_exe.to_string_lossy()).replace('\\', "\\\\");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // --- 版本切换检测 ---
        // 读取已注册 Chrome 清单中的 NMH path，与当前 exe 目录对比。
        // 目录不同说明用户切换了版本（dev → portable / installed），强制重新注册。
        // canonicalize 会加 `\\?\` UNC 前缀，而清单里的 path 写入时已去前缀；
        // 比较前同样去掉，否则永远判定"目录变了"、每次启动都重注册。
        let current_exe_dir = std::env::current_exe()
            .ok()
            .map(|exe| std::fs::canonicalize(&exe).unwrap_or(exe))
            .and_then(|p| {
                p.parent()
                    .map(|d| PathBuf::from(strip_unc_prefix(&d.to_string_lossy())))
            });

        if let Some(exe_dir) = &current_exe_dir {
            let chrome_reg = format!(
                "{}\\{}",
                r"Software\Google\Chrome\NativeMessagingHosts", NMH_NAME
            );
            if let Ok(key) = hkcu.open_subkey_with_flags(&chrome_reg, KEY_READ)
                && let Ok(manifest_str) = key.get_value::<String, _>("")
                && let Ok(content) = std::fs::read_to_string(&manifest_str)
                && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(registered_str) = json["path"].as_str()
            {
                let registered_dir = Path::new(registered_str).parent();
                if registered_dir
                    .map(|d| d != exe_dir.as_path())
                    .unwrap_or(true)
                {
                    log_info!(
                        "[nmh_registry] exe dir changed: registered NMH dir={:?}, current exe dir={:?} → needs update",
                        registered_dir,
                        exe_dir
                    );
                    return true;
                }
            }
        }
        // ---------------------

        // Check Chrome and Edge point to the Chromium manifest with the correct path.
        // Other Chromium browsers (Brave, Vivaldi, Opera) fall back to Chrome's key.
        for reg_path in CHROMIUM_REG_PATHS {
            let full_path = format!("{}\\{}", reg_path, NMH_NAME);
            let Ok(key) = hkcu.open_subkey_with_flags(&full_path, KEY_READ) else {
                return true;
            };
            let Ok(manifest_str): Result<String, _> = key.get_value("") else {
                return true;
            };
            if !manifest_str.ends_with(MANIFEST_FILENAME_CHROMIUM) {
                return true; // pointing to wrong manifest
            }
            if !Path::new(&manifest_str).exists() {
                return true;
            }
            let Ok(content) = std::fs::read_to_string(&manifest_str) else {
                return true;
            };
            if !content.contains(&expected_exe_json) {
                return true;
            }
            // Content versioning: an existing manifest predating Edge support
            // lacks the Edge origin. Force a rewrite so upgraded users get it
            // (path-only checks above would otherwise return false and skip register()).
            if !content.contains(EDGE_EXTENSION_ID) {
                return true;
            }
        }

        // Firefox 键缺失也要重注册：能走到这里说明 Chromium 键完好（本机曾完整
        // 注册过），此时 Firefox 键被外部删除（杀毒/清理工具）应当自愈；
        // register() 无条件写 Firefox 键，对未安装 Firefox 的机器同样无害幂等。
        let firefox_reg = format!("{}\\{}", r"Software\Mozilla\NativeMessagingHosts", NMH_NAME);
        match hkcu.open_subkey_with_flags(&firefox_reg, KEY_READ) {
            Err(_) => return true,
            Ok(key) => {
                let Ok(manifest_str): Result<String, _> = key.get_value("") else {
                    return true;
                };
                if !manifest_str.ends_with(MANIFEST_FILENAME_FIREFOX) {
                    return true; // still pointing to old shared manifest
                }
                if !Path::new(&manifest_str).exists() {
                    return true;
                }
                let Ok(content) = std::fs::read_to_string(&manifest_str) else {
                    return true;
                };
                if !content.contains(&expected_exe_json) {
                    return true;
                }
            }
        }

        false
    }

    /// Register the NMH for all supported browsers.
    ///
    /// Writes two separate manifest files:
    /// - Chromium manifest (Chrome/Edge): contains `allowed_origins`
    /// - Firefox manifest: contains `allowed_extensions` ONLY
    ///
    /// This is idempotent — safe to call on every startup.
    pub fn register() -> Result<(), io::Error> {
        let nmh_exe = find_nmh_exe()?;
        let (chromium_path, firefox_path) = write_manifests(&nmh_exe)?;
        let chromium_str = strip_unc_prefix(&chromium_path.to_string_lossy());
        let firefox_str = strip_unc_prefix(&firefox_path.to_string_lossy());
        let nmh_str = strip_unc_prefix(&nmh_exe.to_string_lossy());
        register_registry(&chromium_str, &firefox_str)?;
        log_info!(
            "[nmh_registry] NMH registered: exe={}, chromium_manifest={}, firefox_manifest={}",
            nmh_str,
            chromium_str,
            firefox_str,
        );
        Ok(())
    }

    /// Remove NMH registration for all browsers and delete manifest files.
    #[allow(dead_code)]
    pub fn unregister() -> Result<(), io::Error> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Remove all Chromium-family browser registry keys
        for reg_path in CHROMIUM_REG_PATHS {
            match hkcu.open_subkey_with_flags(reg_path, KEY_WRITE) {
                Ok(parent) => {
                    let _ = parent.delete_subkey(NMH_NAME);
                }
                Err(_) => continue,
            }
        }
        // Remove Firefox registry key
        if let Ok(parent) =
            hkcu.open_subkey_with_flags(r"Software\Mozilla\NativeMessagingHosts", KEY_WRITE)
        {
            let _ = parent.delete_subkey(NMH_NAME);
        }

        // Remove both manifest files if NMH exe is found.
        if let Ok(nmh_exe) = find_nmh_exe()
            && let Some(dir) = nmh_exe.parent()
        {
            let _ = std::fs::remove_file(dir.join(MANIFEST_FILENAME_CHROMIUM));
            let _ = std::fs::remove_file(dir.join(MANIFEST_FILENAME_FIREFOX));
        }

        log_info!("[nmh_registry] NMH registration removed");
        Ok(())
    }

    #[cfg(test)]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    mod tests {
        use super::{NMH_NAME, needs_update, register};
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};

        /// 本机注册表冒烟：Firefox 键被外部删除后 `needs_update()` 必须自愈判定。
        ///
        /// 依赖真实 HKCU 注册表与已构建的 `fluxdown_nmh.exe`（`cargo build -p fluxdown_nmh`），
        /// 会改写本机 NMH 注册（指向 workspace target 目录，安装版启动时会自行纠正），
        /// 故标记 ignore，手动执行：
        /// `cargo test -p hub -- --ignored firefox_key_self_heal`
        #[test]
        #[ignore]
        fn firefox_key_self_heal() {
            // 基线：全量注册后一切匹配。
            register().expect("register");
            assert!(!needs_update(), "fresh register must be up to date");

            // 模拟外部删除 Firefox 键（杀毒/清理工具场景）。
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let parent = hkcu
                .open_subkey_with_flags(r"Software\Mozilla\NativeMessagingHosts", KEY_WRITE)
                .expect("open Mozilla NMH parent");
            parent.delete_subkey(NMH_NAME).expect("delete firefox key");

            // 修复点：缺失必须触发重注册（旧代码此处返回 false）。
            assert!(
                needs_update(),
                "missing Firefox key must trigger re-registration"
            );

            // register() 自愈恢复。
            register().expect("re-register");
            assert!(
                !needs_update(),
                "self-healed registration must be up to date"
            );
        }
    }
}

// Linux: write NMH manifest files to XDG browser directories.
#[cfg(target_os = "linux")]
mod inner {
    use crate::logger::log_info;
    use serde::Serialize;
    use std::io;
    use std::path::{Path, PathBuf};

    const NMH_NAME: &str = "com.fluxdown.nmh";
    const NMH_DESCRIPTION: &str = "FluxDown Native Messaging Host";
    const NMH_EXE_NAME: &str = "fluxdown_nmh";
    /// Shell wrapper script registered in the NMH manifest.
    /// Provides a stable path even for AppImage builds where the real binary
    /// lives at a random FUSE mount point that changes on every launch.
    const NMH_WRAPPER_NAME: &str = "fluxdown_nmh.sh";
    const MANIFEST_FILENAME_CHROMIUM: &str = "com.fluxdown.nmh.json";
    const MANIFEST_FILENAME_FIREFOX: &str = "com.fluxdown.nmh.json";
    const CHROME_EXTENSION_ID: &str = "chrome-extension://meleenglfggcmcajknpeeeiobnpfmahc/";
    /// Edge Add-ons store extension ID — differs from Chrome (Edge ignores the
    /// manifest `key`) and must be whitelisted explicitly, else Edge store users
    /// get "forbidden" on connectNative → stuck on "未连接".
    const EDGE_EXTENSION_ID: &str = "chrome-extension://nglkkjbogjghekbhhcnccnpfedjbdhhd/";
    const FIREFOX_EXTENSION_ID: &str = "fluxdown@fluxdown.app";

    #[derive(Serialize)]
    struct NmhManifestChromium {
        name: String,
        description: String,
        path: String,
        #[serde(rename = "type")]
        host_type: String,
        allowed_origins: Vec<String>,
    }

    #[derive(Serialize)]
    struct NmhManifestFirefox {
        name: String,
        description: String,
        path: String,
        #[serde(rename = "type")]
        host_type: String,
        allowed_extensions: Vec<String>,
    }

    fn home_dir() -> Option<PathBuf> {
        std::env::var("HOME").ok().map(PathBuf::from)
    }

    /// All Chromium-family NMH manifest directories on Linux.
    ///
    /// Covers standard deb/rpm/tar.gz installs as well as Flatpak and Snap
    /// variants, which use isolated profile directories under ~/.var/app/ and
    /// ~/snap/ respectively.
    fn chromium_nmh_dirs() -> Vec<PathBuf> {
        let Some(home) = home_dir() else {
            return vec![];
        };
        let config = home.join(".config");
        let var_app = home.join(".var").join("app");
        let snap = home.join("snap");
        vec![
            // ── Standard deb/rpm/tar.gz installs ──
            config.join("google-chrome").join("NativeMessagingHosts"),
            config.join("chromium").join("NativeMessagingHosts"),
            config.join("microsoft-edge").join("NativeMessagingHosts"),
            // Brave Browser (verified via KeePassXC source)
            config
                .join("BraveSoftware")
                .join("Brave-Browser")
                .join("NativeMessagingHosts"),
            // Vivaldi (verified via KeePassXC source)
            config.join("vivaldi").join("NativeMessagingHosts"),
            // ── Flatpak variants ──
            // Flatpak Chrome
            var_app
                .join("com.google.Chrome")
                .join("config")
                .join("google-chrome")
                .join("NativeMessagingHosts"),
            // Flatpak Chromium
            var_app
                .join("org.chromium.Chromium")
                .join("config")
                .join("chromium")
                .join("NativeMessagingHosts"),
            // Flatpak Edge
            var_app
                .join("com.microsoft.Edge")
                .join("config")
                .join("microsoft-edge")
                .join("NativeMessagingHosts"),
            // Flatpak Brave
            var_app
                .join("com.brave.Browser")
                .join("config")
                .join("BraveSoftware")
                .join("Brave-Browser")
                .join("NativeMessagingHosts"),
            // ── Snap variants ──
            // Snap Chromium
            snap.join("chromium")
                .join("common")
                .join(".config")
                .join("chromium")
                .join("NativeMessagingHosts"),
        ]
    }

    /// All Firefox-family NMH manifest directories on Linux.
    ///
    /// Returns multiple paths: standard location, Flatpak sandboxed variants,
    /// and Firefox-fork browsers (LibreWolf, Waterfox).
    /// Registration writes to all paths; needs_update requires every path's
    /// manifest to exist and match (self-heals external deletion on restart).
    fn firefox_nmh_dirs() -> Vec<PathBuf> {
        let Some(home) = home_dir() else {
            return vec![];
        };
        let var_app = home.join(".var").join("app");
        vec![
            // Standard Firefox
            home.join(".mozilla").join("native-messaging-hosts"),
            // Flatpak Firefox
            var_app
                .join("org.mozilla.firefox")
                .join(".mozilla")
                .join("native-messaging-hosts"),
            // LibreWolf (privacy-focused Firefox fork, verified via official FAQ)
            home.join(".librewolf").join("native-messaging-hosts"),
            // Zen Browser (Firefox fork, uses its own ~/.zen profile root, #313)
            home.join(".zen").join("native-messaging-hosts"),
            // Flatpak LibreWolf
            var_app
                .join("io.gitlab.librewolf-community")
                .join(".librewolf")
                .join("native-messaging-hosts"),
        ]
    }

    fn find_nmh_exe() -> Result<PathBuf, io::Error> {
        // 1. Next to current exe (production deployment, including AppImage mount)
        if let Ok(exe) = std::env::current_exe() {
            let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
            if let Some(dir) = canonical.parent() {
                let candidate = dir.join(NMH_EXE_NAME);
                if candidate.exists() {
                    log_info!(
                        "[nmh_registry] found NMH exe next to app: {}",
                        candidate.display()
                    );
                    return Ok(candidate);
                }
            }
        }

        // 2. Cargo workspace target directory (development)
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = Path::new(manifest_dir).parent().and_then(|p| p.parent());

        if let Some(ws) = workspace_root {
            for profile in &["debug", "release"] {
                let candidate = ws.join("target").join(profile).join(NMH_EXE_NAME);
                if candidate.exists() {
                    log_info!(
                        "[nmh_registry] found NMH exe in cargo target: {}",
                        candidate.display()
                    );
                    return Ok(candidate);
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} not found. Build it with: cargo build -p fluxdown_nmh",
                NMH_EXE_NAME
            ),
        ))
    }

    /// Stable wrapper script path: ~/.local/share/fluxdown/fluxdown_nmh.sh
    ///
    /// NMH manifests always point to this wrapper rather than the real binary.
    /// This decouples the manifest from AppImage mount points (which change on
    /// every launch) and from Cargo target directories (which are dev-only).
    fn wrapper_path() -> Option<PathBuf> {
        home_dir().map(|h| {
            h.join(".local")
                .join("share")
                .join("fluxdown")
                .join(NMH_WRAPPER_NAME)
        })
    }

    /// Write the shell wrapper script that exec's the real NMH binary.
    ///
    /// By registering a wrapper script instead of the binary directly, we
    /// provide a stable path even when the binary lives in a temporary AppImage
    /// mount point.  On every app launch the wrapper is rewritten to point at
    /// the current binary path, so it stays correct after updates.
    fn write_wrapper_script(nmh_exe: &Path) -> Result<PathBuf, io::Error> {
        let Some(wp) = wrapper_path() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cannot determine home directory for wrapper script",
            ));
        };
        if let Some(parent) = wp.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let exe_str = nmh_exe.to_string_lossy();
        let script = format!("#!/bin/sh\nexec '{}' \"$@\"\n", exe_str);
        std::fs::write(&wp, script)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wp, std::fs::Permissions::from_mode(0o755))?;
        Ok(wp)
    }

    fn write_chromium_manifest(wrapper: &Path, dir: &Path) -> Result<PathBuf, io::Error> {
        std::fs::create_dir_all(dir)?;
        let manifest = NmhManifestChromium {
            name: NMH_NAME.to_string(),
            description: NMH_DESCRIPTION.to_string(),
            path: wrapper.to_string_lossy().into_owned(),
            host_type: "stdio".to_string(),
            allowed_origins: vec![
                CHROME_EXTENSION_ID.to_string(),
                EDGE_EXTENSION_ID.to_string(),
            ],
        };
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| io::Error::other(format!("JSON error: {}", e)))?;
        let path = dir.join(MANIFEST_FILENAME_CHROMIUM);
        std::fs::write(&path, json)?;
        Ok(path)
    }

    fn write_firefox_manifest(wrapper: &Path, dir: &Path) -> Result<PathBuf, io::Error> {
        std::fs::create_dir_all(dir)?;
        let manifest = NmhManifestFirefox {
            name: NMH_NAME.to_string(),
            description: NMH_DESCRIPTION.to_string(),
            path: wrapper.to_string_lossy().into_owned(),
            host_type: "stdio".to_string(),
            allowed_extensions: vec![FIREFOX_EXTENSION_ID.to_string()],
        };
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| io::Error::other(format!("JSON error: {}", e)))?;
        let path = dir.join(MANIFEST_FILENAME_FIREFOX);
        std::fs::write(&path, json)?;
        Ok(path)
    }

    pub fn needs_update() -> bool {
        let Ok(nmh_exe) = find_nmh_exe() else {
            return true;
        };
        let expected_exe = nmh_exe.to_string_lossy().into_owned();

        // Check that the wrapper script exists and points at the current binary.
        let Some(wp) = wrapper_path() else {
            return true;
        };
        if !wp.exists() {
            return true;
        }
        let wrapper_ok = std::fs::read_to_string(&wp)
            .map(|c| c.contains(&expected_exe))
            .unwrap_or(false);
        if !wrapper_ok {
            log_info!("[nmh_registry] wrapper script outdated → needs update");
            return true;
        }

        let wrapper_str = wp.to_string_lossy().into_owned();

        // At least one Chromium dir must have a manifest pointing to the wrapper
        // AND containing the Edge origin (content versioning: rewrite manifests
        // predating Edge support so upgraded users get the Edge allowed_origins).
        let chromium_ok = chromium_nmh_dirs().iter().any(|dir| {
            let path = dir.join(MANIFEST_FILENAME_CHROMIUM);
            std::fs::read_to_string(path)
                .map(|c| c.contains(&wrapper_str) && c.contains(EDGE_EXTENSION_ID))
                .unwrap_or(false)
        });

        // Firefox 清单缺失也要重注册（自愈外部删除）；register() 对所有目录
        // 无条件写入，未安装 Firefox 时写入同样无害幂等。
        let firefox_ok = firefox_nmh_dirs().iter().all(|dir| {
            let path = dir.join(MANIFEST_FILENAME_FIREFOX);
            std::fs::read_to_string(path)
                .map(|c| c.contains(&wrapper_str))
                .unwrap_or(false)
        });

        !(chromium_ok && firefox_ok)
    }

    pub fn register() -> Result<(), io::Error> {
        let nmh_exe = find_nmh_exe()?;

        // Write wrapper script first; manifests point to it.
        let wrapper = write_wrapper_script(&nmh_exe)?;
        log_info!("[nmh_registry] NMH wrapper script: {}", wrapper.display());

        for dir in chromium_nmh_dirs() {
            match write_chromium_manifest(&wrapper, &dir) {
                Ok(path) => {
                    log_info!("[nmh_registry] Chromium manifest: {}", path.display());
                }
                Err(e) => {
                    log_info!(
                        "[nmh_registry] Chromium manifest error ({}): {}",
                        dir.display(),
                        e
                    );
                }
            }
        }

        for dir in firefox_nmh_dirs() {
            match write_firefox_manifest(&wrapper, &dir) {
                Ok(path) => {
                    log_info!("[nmh_registry] Firefox manifest: {}", path.display());
                }
                Err(e) => {
                    log_info!(
                        "[nmh_registry] Firefox manifest error ({}): {}",
                        dir.display(),
                        e
                    );
                }
            }
        }

        log_info!(
            "[nmh_registry] NMH registered: exe={}, wrapper={}",
            nmh_exe.display(),
            wrapper.display()
        );
        Ok(())
    }

    #[allow(dead_code)]
    pub fn unregister() -> Result<(), io::Error> {
        for dir in chromium_nmh_dirs() {
            let _ = std::fs::remove_file(dir.join(MANIFEST_FILENAME_CHROMIUM));
        }
        for dir in firefox_nmh_dirs() {
            let _ = std::fs::remove_file(dir.join(MANIFEST_FILENAME_FIREFOX));
        }
        if let Some(wp) = wrapper_path() {
            let _ = std::fs::remove_file(wp);
        }
        log_info!("[nmh_registry] NMH registration removed");
        Ok(())
    }
}

// macOS: write NMH manifest files to ~/Library/Application Support browser directories.
#[cfg(target_os = "macos")]
mod inner {
    use crate::logger::log_info;
    use serde::Serialize;
    use std::io;
    use std::path::{Path, PathBuf};

    const NMH_NAME: &str = "com.fluxdown.nmh";
    const NMH_DESCRIPTION: &str = "FluxDown Native Messaging Host";
    const NMH_EXE_NAME: &str = "fluxdown_nmh";
    /// Shell wrapper script name registered in NMH manifest.
    /// Chrome/Firefox spawn this shell (a system-signed binary) which then
    /// exec's the actual fluxdown_nmh binary, bypassing macOS AMFI's
    /// requirement that processes spawned by Hardened-Runtime apps must
    /// carry a trusted Developer ID signature (adhoc-only binaries are
    /// rejected with "Unrecoverable CT signature issue").
    const NMH_WRAPPER_NAME: &str = "fluxdown_nmh.sh";
    const MANIFEST_FILENAME: &str = "com.fluxdown.nmh.json";
    const CHROME_EXTENSION_ID: &str = "chrome-extension://meleenglfggcmcajknpeeeiobnpfmahc/";
    /// Edge Add-ons store extension ID — differs from Chrome (Edge ignores the
    /// manifest `key`) and must be whitelisted explicitly, else Edge store users
    /// get "forbidden" on connectNative → stuck on "未连接".
    const EDGE_EXTENSION_ID: &str = "chrome-extension://nglkkjbogjghekbhhcnccnpfedjbdhhd/";
    const FIREFOX_EXTENSION_ID: &str = "fluxdown@fluxdown.app";

    #[derive(Serialize)]
    struct NmhManifestChromium {
        name: String,
        description: String,
        path: String,
        #[serde(rename = "type")]
        host_type: String,
        allowed_origins: Vec<String>,
    }

    #[derive(Serialize)]
    struct NmhManifestFirefox {
        name: String,
        description: String,
        path: String,
        #[serde(rename = "type")]
        host_type: String,
        allowed_extensions: Vec<String>,
    }

    /// Returns the current user's home directory.
    ///
    /// Prefers `$HOME` but falls back to the passwd database via `getpwuid_r`
    /// so that the correct path is returned even when the process is launched
    /// by a system service (launchd) that may not set `$HOME`.
    fn home_dir() -> Option<PathBuf> {
        if let Ok(h) = std::env::var("HOME") {
            if !h.is_empty() {
                return Some(PathBuf::from(h));
            }
        }
        use std::ffi::CStr;
        let uid = unsafe { libc::getuid() };
        let buf_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
        let buf_size = if buf_size > 0 {
            buf_size as usize
        } else {
            1024
        };
        let mut buf = vec![0i8; buf_size];
        let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let ret = unsafe {
            libc::getpwuid_r(
                uid,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr(),
                buf_size,
                &mut result,
            )
        };
        if ret == 0 && !result.is_null() {
            let pwd = unsafe { pwd.assume_init() };
            if !pwd.pw_dir.is_null() {
                let cstr = unsafe { CStr::from_ptr(pwd.pw_dir) };
                if let Ok(s) = cstr.to_str() {
                    if !s.is_empty() {
                        return Some(PathBuf::from(s));
                    }
                }
            }
        }
        None
    }

    /// macOS Chromium-family NMH manifest directories.
    /// Ref: https://developer.chrome.com/docs/apps/nativeMessaging/#native-messaging-host-location-macos
    fn chromium_nmh_dirs() -> Vec<PathBuf> {
        let Some(home) = home_dir() else {
            return vec![];
        };
        let lib = home.join("Library").join("Application Support");
        vec![
            // Google Chrome (stable / beta / canary)
            lib.join("Google")
                .join("Chrome")
                .join("NativeMessagingHosts"),
            lib.join("Google")
                .join("Chrome Beta")
                .join("NativeMessagingHosts"),
            lib.join("Google")
                .join("Chrome Canary")
                .join("NativeMessagingHosts"),
            // Open-source Chromium
            lib.join("Chromium").join("NativeMessagingHosts"),
            // Microsoft Edge (stable / beta)
            lib.join("Microsoft Edge").join("NativeMessagingHosts"),
            lib.join("Microsoft Edge Beta").join("NativeMessagingHosts"),
            // Arc
            lib.join("Arc")
                .join("User Data")
                .join("NativeMessagingHosts"),
            // Brave Browser (verified via KeePassXC source)
            lib.join("BraveSoftware")
                .join("Brave-Browser")
                .join("NativeMessagingHosts"),
            // Vivaldi (verified via KeePassXC source)
            lib.join("Vivaldi").join("NativeMessagingHosts"),
        ]
    }

    /// macOS Firefox NMH manifest directory.
    fn firefox_nmh_dir() -> Option<PathBuf> {
        home_dir().map(|h| {
            h.join("Library")
                .join("Application Support")
                .join("Mozilla")
                .join("NativeMessagingHosts")
        })
    }

    fn find_nmh_exe() -> Result<PathBuf, io::Error> {
        // 1. Next to current exe (production: inside .app bundle Contents/MacOS/)
        if let Ok(exe) = std::env::current_exe() {
            let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
            if let Some(dir) = canonical.parent() {
                let candidate = dir.join(NMH_EXE_NAME);
                if candidate.exists() {
                    log_info!(
                        "[nmh_registry] found NMH exe next to app: {}",
                        candidate.display()
                    );
                    return Ok(candidate);
                }
            }
        }

        // 2. Cargo workspace target directory (development)
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = Path::new(manifest_dir).parent().and_then(|p| p.parent());

        if let Some(ws) = workspace_root {
            for profile in &["debug", "release"] {
                let candidate = ws.join("target").join(profile).join(NMH_EXE_NAME);
                if candidate.exists() {
                    log_info!(
                        "[nmh_registry] found NMH exe in cargo target: {}",
                        candidate.display()
                    );
                    return Ok(candidate);
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} not found. Build it with: cargo build -p fluxdown_nmh",
                NMH_EXE_NAME
            ),
        ))
    }

    /// Write a shell wrapper script that exec's the real NMH binary.
    ///
    /// macOS AMFI rejects adhoc-signed (non-Developer-ID) binaries when they
    /// are spawned by Hardened Runtime processes such as Chrome or Firefox.
    /// `/bin/sh` is a system binary with an Apple-signed certificate and is
    /// always permitted. By registering the *shell script* as the NMH path,
    /// the browser spawns `/bin/sh`, which in turn exec's `fluxdown_nmh`.
    /// The shell inherits the NMH stdin/stdout pipe and transparently relays
    /// it to the binary — zero overhead, no extra process.
    fn write_wrapper_script(nmh_exe: &Path) -> Result<PathBuf, io::Error> {
        let Some(home) = home_dir() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cannot determine home directory",
            ));
        };
        let dir = home
            .join("Library")
            .join("Application Support")
            .join("fluxdown");
        std::fs::create_dir_all(&dir)?;
        let script_path = dir.join(NMH_WRAPPER_NAME);
        let exe_str = nmh_exe.to_string_lossy();
        // Use `exec` so the shell process is replaced by the binary (no extra
        // zombie process). Pass "$@" to forward any arguments Chrome may add.
        let script = format!("#!/bin/sh\nexec '{}' \"$@\"\n", exe_str);
        std::fs::write(&script_path, script)?;
        // The script must be executable.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
        Ok(script_path)
    }

    fn write_chromium_manifest(wrapper: &Path, dir: &Path) -> Result<PathBuf, io::Error> {
        std::fs::create_dir_all(dir)?;
        let manifest = NmhManifestChromium {
            name: NMH_NAME.to_string(),
            description: NMH_DESCRIPTION.to_string(),
            path: wrapper.to_string_lossy().into_owned(),
            host_type: "stdio".to_string(),
            allowed_origins: vec![
                CHROME_EXTENSION_ID.to_string(),
                EDGE_EXTENSION_ID.to_string(),
            ],
        };
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| io::Error::other(format!("JSON error: {}", e)))?;
        let path = dir.join(MANIFEST_FILENAME);
        std::fs::write(&path, json)?;
        Ok(path)
    }

    fn write_firefox_manifest(wrapper: &Path, dir: &Path) -> Result<PathBuf, io::Error> {
        std::fs::create_dir_all(dir)?;
        let manifest = NmhManifestFirefox {
            name: NMH_NAME.to_string(),
            description: NMH_DESCRIPTION.to_string(),
            path: wrapper.to_string_lossy().into_owned(),
            host_type: "stdio".to_string(),
            allowed_extensions: vec![FIREFOX_EXTENSION_ID.to_string()],
        };
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| io::Error::other(format!("JSON error: {}", e)))?;
        let path = dir.join(MANIFEST_FILENAME);
        std::fs::write(&path, json)?;
        Ok(path)
    }

    pub fn needs_update() -> bool {
        let Ok(nmh_exe) = find_nmh_exe() else {
            return true;
        };
        // The manifest now points to the shell wrapper, but the wrapper
        // contains the path to the real binary. Check that the wrapper exists
        // and that its content references the current NMH exe path.
        let expected_exe = nmh_exe.to_string_lossy().into_owned();

        // 版本切换检测：wrapper 内容里包含的 NMH exe 路径是否与当前一致。
        let wrapper_path = home_dir().map(|h| {
            h.join("Library")
                .join("Application Support")
                .join("fluxdown")
                .join(NMH_WRAPPER_NAME)
        });

        if let Some(ref wp) = wrapper_path {
            if !wp.exists() {
                return true;
            }
            let wrapper_ok = std::fs::read_to_string(wp)
                .map(|c| c.contains(&expected_exe))
                .unwrap_or(false);
            if !wrapper_ok {
                log_info!(
                    "[nmh_registry] wrapper script outdated or missing exe path → needs update"
                );
                return true;
            }
        } else {
            return true;
        }

        let wrapper_str = wrapper_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        // At least one Chromium dir must have a manifest pointing to the wrapper
        // AND containing the Edge origin (content versioning: rewrite manifests
        // predating Edge support so upgraded users get the Edge allowed_origins).
        let chromium_ok = chromium_nmh_dirs().iter().any(|dir| {
            let path = dir.join(MANIFEST_FILENAME);
            std::fs::read_to_string(path)
                .map(|c| c.contains(&wrapper_str) && c.contains(EDGE_EXTENSION_ID))
                .unwrap_or(false)
        });

        // Firefox 清单缺失也要重注册（自愈外部删除）；register() 无条件写入，
        // 未安装 Firefox 时写入同样无害幂等。
        let firefox_ok = firefox_nmh_dir()
            .map(|dir| {
                let path = dir.join(MANIFEST_FILENAME);
                std::fs::read_to_string(path)
                    .map(|c| c.contains(&wrapper_str))
                    .unwrap_or(false)
            })
            .unwrap_or(true);

        !(chromium_ok && firefox_ok)
    }

    pub fn register() -> Result<(), io::Error> {
        let nmh_exe = find_nmh_exe()?;

        // Write the shell wrapper script first; manifests point to it.
        let wrapper = write_wrapper_script(&nmh_exe)?;
        log_info!("[nmh_registry] NMH wrapper script: {}", wrapper.display());

        for dir in chromium_nmh_dirs() {
            match write_chromium_manifest(&wrapper, &dir) {
                Ok(path) => {
                    log_info!("[nmh_registry] Chromium manifest: {}", path.display());
                }
                Err(e) => {
                    log_info!(
                        "[nmh_registry] Chromium manifest error ({}): {}",
                        dir.display(),
                        e
                    );
                }
            }
        }

        if let Some(dir) = firefox_nmh_dir() {
            match write_firefox_manifest(&wrapper, &dir) {
                Ok(path) => {
                    log_info!("[nmh_registry] Firefox manifest: {}", path.display());
                }
                Err(e) => {
                    log_info!("[nmh_registry] Firefox manifest error: {}", e);
                }
            }
        }

        log_info!(
            "[nmh_registry] NMH registered: exe={}, wrapper={}",
            nmh_exe.display(),
            wrapper.display()
        );
        Ok(())
    }

    #[allow(dead_code)]
    pub fn unregister() -> Result<(), io::Error> {
        for dir in chromium_nmh_dirs() {
            let _ = std::fs::remove_file(dir.join(MANIFEST_FILENAME));
        }
        if let Some(dir) = firefox_nmh_dir() {
            let _ = std::fs::remove_file(dir.join(MANIFEST_FILENAME));
        }
        // Remove wrapper script.
        if let Some(home) = home_dir() {
            let wrapper = home
                .join("Library")
                .join("Application Support")
                .join("fluxdown")
                .join(NMH_WRAPPER_NAME);
            let _ = std::fs::remove_file(wrapper);
        }
        log_info!("[nmh_registry] NMH registration removed");
        Ok(())
    }
}

// All other non-Windows, non-Linux, non-macOS platforms — no-op.
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
mod inner {
    use std::io;

    pub fn needs_update() -> bool {
        false
    }

    pub fn register() -> Result<(), io::Error> {
        Ok(())
    }

    #[allow(dead_code)]
    pub fn unregister() -> Result<(), io::Error> {
        Ok(())
    }
}

#[allow(unused_imports)]
pub use inner::{needs_update, register, unregister};
