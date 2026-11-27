//! URL scheme（`fluxdown://` 深链、`ed2k://`、`magnet:`）系统默认处理程序注册。
//!
//! 注册目标是官方桌面程序：Windows 写入 HKCU 注册表并指向同级
//! `fluxdown-desktop.exe`；Linux 通过 `xdg-mime` 指向打包的
//! `com.fluxdown.app.desktop`；macOS 通过 Launch Services 指向当前 `.app`
//! bundle（scheme 必须已在 `CFBundleURLTypes` 中声明）。
//!
//! Windows 注册表结构（与 Inno Setup 安装器一致）：
//! ```text
//! HKCU\Software\Classes\<scheme>                    → "URL:<desc>"
//! HKCU\Software\Classes\<scheme>  "URL Protocol"    → ""
//! HKCU\Software\Classes\<scheme>\DefaultIcon        → "\"<exe>\",0"
//! HKCU\Software\Classes\<scheme>\shell\open\command → "\"<exe>\" \"%1\""
//! ```
//! 运行期直接写 winreg，不在安装器 [Registry] 跟踪范围内，因此
//! `installer/windows/setup.iss` 卸载时显式清理 `fluxdown`/`ed2k`/`magnet`
//! 键——两处需保持同步。

/// 本程序可声明为系统默认处理程序的 URL scheme。
#[derive(Clone, Copy)]
pub struct UrlScheme {
    /// 不含 `://` 的小写 scheme 名（如 `fluxdown`）。
    pub scheme: &'static str,
    /// Windows shell 描述（class 键默认值）；Linux/macOS 由 `.desktop` /
    /// bundle id 命名处理程序，不使用。
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub desc: &'static str,
}

/// 本程序自有深链 scheme。
pub const FLUXDOWN: UrlScheme = UrlScheme {
    scheme: "fluxdown",
    desc: "URL:FluxDown Protocol",
};

/// eDonkey2000 链接（`ed2k://|file|…`）；eMule/aMule 等客户端会合法竞争。
pub const ED2K: UrlScheme = UrlScheme {
    scheme: "ed2k",
    desc: "URL:ed2k Protocol",
};

/// BitTorrent magnet 链接（`magnet:?xt=urn:btih:…`）。
pub const MAGNET: UrlScheme = UrlScheme {
    scheme: "magnet",
    desc: "URL:Magnet Link",
};

/// `PlatformIntegrationDto.url_protocols` 的固定枚举顺序。
pub const SCHEMES: [UrlScheme; 3] = [MAGNET, ED2K, FLUXDOWN];

/// 把 wire scheme 名解析为允许列表中的 [`UrlScheme`]。
///
/// 注册原语直接写 shell/注册表，scheme 绝不能是调用方任意字符串。
pub fn from_name(name: &str) -> Option<UrlScheme> {
    match name {
        "fluxdown" => Some(FLUXDOWN),
        "ed2k" => Some(ED2K),
        "magnet" => Some(MAGNET),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
mod inner {
    use std::path::Path;

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    use super::UrlScheme;
    use crate::platform::{PlatformError, registry_executable};

    pub fn supported(desktop: Option<&Path>) -> bool {
        desktop.is_some()
    }

    /// `proto` 是否已注册到**桌面程序**。
    ///
    /// 仅当 `HKCU\Software\Classes\<scheme>` 存在、带 `URL Protocol` 值，且
    /// `shell\open\command` 指向同级 `fluxdown-desktop.exe` 时为 true。exe
    /// 路径比对能识别程序移动/升级后的过期注册，也把其他客户端（ed2k 竞争者）
    /// 的注册判定为“未注册”而不是冒领。桌面程序不可定位时退回“值存在”语义，
    /// 避免瞬时 I/O 错误导致误判。
    pub fn is_registered(proto: UrlScheme, desktop: Option<&Path>) -> bool {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let scheme = proto.scheme;
        let Ok(key) = hkcu.open_subkey_with_flags(format!("Software\\Classes\\{scheme}"), KEY_READ)
        else {
            return false;
        };
        if key.get_value::<String, _>("URL Protocol").is_err() {
            return false;
        }
        let Ok(target) = registry_executable(desktop) else {
            return true;
        };
        match read_command_exe(&hkcu, scheme) {
            Some(registered) => paths_equivalent(&registered, &target),
            None => false,
        }
    }

    /// 读取 `Software\Classes\<scheme>\shell\open\command` 默认值中的 exe
    /// （首对双引号内的 token）。
    fn read_command_exe(hkcu: &RegKey, scheme: &str) -> Option<String> {
        let cmd_key = hkcu
            .open_subkey_with_flags(
                format!("Software\\Classes\\{scheme}\\shell\\open\\command"),
                KEY_READ,
            )
            .ok()?;
        let command: String = cmd_key.get_value("").ok()?;
        let after_first = command.find('"')? + 1;
        let rest = &command[after_first..];
        let end = rest.find('"')?;
        Some(rest[..end].to_owned())
    }

    /// 规范化后大小写不敏感比较两个 Windows exe 路径。
    fn paths_equivalent(a: &str, b: &str) -> bool {
        let norm = |s: &str| -> String {
            let canonical = std::fs::canonicalize(s)
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| s.to_owned());
            canonical
                .strip_prefix(r"\\?\")
                .unwrap_or(&canonical)
                .to_ascii_lowercase()
        };
        norm(a) == norm(b)
    }

    /// 把 `proto` 注册到桌面程序。
    pub fn register(proto: UrlScheme, desktop: Option<&Path>) -> Result<(), PlatformError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let exe = registry_executable(desktop)?;
        let scheme = proto.scheme;

        let (proto_key, _) =
            hkcu.create_subkey_with_flags(format!("Software\\Classes\\{scheme}"), KEY_WRITE)?;
        proto_key.set_value("", &proto.desc)?;
        // 空的 "URL Protocol" 值是 Windows 识别 URL 协议处理程序的标记。
        proto_key.set_value("URL Protocol", &"")?;

        let (icon_key, _) = hkcu.create_subkey_with_flags(
            format!("Software\\Classes\\{scheme}\\DefaultIcon"),
            KEY_WRITE,
        )?;
        icon_key.set_value("", &format!("\"{exe}\",0"))?;

        let (cmd_key, _) = hkcu.create_subkey_with_flags(
            format!("Software\\Classes\\{scheme}\\shell\\open\\command"),
            KEY_WRITE,
        )?;
        cmd_key.set_value("", &format!("\"{exe}\" \"%1\""))?;

        crate::platform::windows_shell::notify_association_changed();
        tracing::info!(scheme, exe, "registered URL protocol");
        Ok(())
    }

    /// 移除本程序对 `proto` 的注册；其他客户端的注册不受影响。
    pub fn unregister(proto: UrlScheme, desktop: Option<&Path>) -> Result<(), PlatformError> {
        let scheme = proto.scheme;
        if !is_registered(proto, desktop) {
            tracing::info!(
                scheme,
                "URL protocol not registered to FluxDown, skipping removal"
            );
            return Ok(());
        }
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        // 删除每用户 scheme 树；其他客户端的 HKLM 注册（若有）重新生效。
        let classes = hkcu.open_subkey_with_flags("Software\\Classes", KEY_WRITE)?;
        // 并发卸载已删除 scheme 键是预期幂等结果；权限和其它错误必须反馈给调用方。
        if let Err(error) = classes.delete_subkey_all(scheme)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.into());
        }
        crate::platform::windows_shell::notify_association_changed();
        tracing::info!(scheme, "removed URL protocol registration");
        Ok(())
    }
}

// Linux：XDG `x-scheme-handler/<scheme>` MIME 类型，与 `file_association`
// 处理 `application/x-bittorrent` 的方式一致。
#[cfg(target_os = "linux")]
mod inner {
    use std::path::Path;

    use super::UrlScheme;
    use crate::platform::PlatformError;
    use crate::platform::xdg::{DESKTOP_ENTRY, query_default_is_fluxdown, remove_default};

    pub fn supported(_desktop: Option<&Path>) -> bool {
        true
    }

    fn mime_type(proto: UrlScheme) -> String {
        format!("x-scheme-handler/{}", proto.scheme)
    }

    /// FluxDown 是否为 `proto` 的默认处理程序。
    pub fn is_registered(proto: UrlScheme, _desktop: Option<&Path>) -> bool {
        query_default_is_fluxdown(&mime_type(proto))
    }

    /// 把 FluxDown 注册为 `proto` 的默认处理程序。
    ///
    /// 要求打包的 `com.fluxdown.app.desktop` 已安装到 XDG applications 目录并在
    /// `MimeType` 中声明该 scheme。
    pub fn register(proto: UrlScheme, _desktop: Option<&Path>) -> Result<(), PlatformError> {
        let status = std::process::Command::new("xdg-mime")
            .args(["default", DESKTOP_ENTRY, &mime_type(proto)])
            .status()?;
        if !status.success() {
            return Err(PlatformError::Failed(format!(
                "xdg-mime default exited with {status}"
            )));
        }
        tracing::info!(scheme = proto.scheme, "registered URL protocol");
        Ok(())
    }

    /// 移除用户级覆盖，把 `proto` 交还系统默认。
    pub fn unregister(proto: UrlScheme, _desktop: Option<&Path>) -> Result<(), PlatformError> {
        remove_default(&mime_type(proto))?;
        tracing::info!(scheme = proto.scheme, "removed URL protocol registration");
        Ok(())
    }
}

// macOS：Launch Services 默认处理程序；scheme 必须在 `CFBundleURLTypes`
// （packaging/macos/Info.plist）中声明，Launch Services 才接受本 bundle 为候选。
// macOS 12+ 改默认程序会弹系统确认框：经 `NSWorkspace` 确认式接口阻塞到用户作答。
#[cfg(all(target_os = "macos", feature = "desktop"))]
mod inner {
    use std::path::Path;

    use super::UrlScheme;
    use crate::platform::macos_cf;
    use crate::platform::macos_default_app as workspace;
    use crate::platform::{PlatformError, host_bundle_id, successor_handler};

    pub fn supported(_desktop: Option<&Path>) -> bool {
        host_bundle_id().is_some()
    }

    fn handled_by(scheme: &str, bundle_id: &str) -> bool {
        matches!(
            macos_cf::default_scheme_handler(scheme),
            Ok(Some(handler)) if handler.eq_ignore_ascii_case(bundle_id)
        )
    }

    /// 本 bundle 是否为 `proto` 的默认处理程序。
    pub fn is_registered(proto: UrlScheme, _desktop: Option<&Path>) -> bool {
        host_bundle_id().is_some_and(|mine| handled_by(proto.scheme, &mine))
    }

    /// 把 `proto` 交给 `app`：macOS 12+ 阻塞到用户在系统确认框作答，macOS 11 直接写入。
    fn set_handler(proto: UrlScheme, app: &workspace::App) -> Result<(), PlatformError> {
        if workspace::confirmed_setters_available() {
            workspace::set_default_scheme_app(app, proto.scheme)
        } else {
            macos_cf::set_scheme_handler(proto.scheme, &app.id)
                .map_err(|error| PlatformError::Failed(error.to_string()))
        }
    }

    /// 把本 bundle 设为 `proto` 的默认处理程序；用户拒绝返回 [`PlatformError::Declined`]。
    pub fn register(proto: UrlScheme, _desktop: Option<&Path>) -> Result<(), PlatformError> {
        let app = workspace::host_app().ok_or(PlatformError::Unsupported(
            "fluxdown-agent is not running inside an app bundle",
        ))?;
        if handled_by(proto.scheme, &app.id) {
            return Ok(());
        }
        set_handler(proto, &app)?;
        if !workspace::settles(|| handled_by(proto.scheme, &app.id)) {
            tracing::info!(scheme = proto.scheme, "URL protocol registration declined");
            return Err(PlatformError::Declined);
        }
        tracing::info!(
            scheme = proto.scheme,
            bundle_id = app.id,
            "registered URL protocol"
        );
        Ok(())
    }

    /// 把 `proto` 移交给另一个已安装的候选程序；仅在当前由我们持有时执行，
    /// 不覆盖其他客户端的选择。用户选「继续使用 FluxDown」返回 [`PlatformError::Declined`]。
    ///
    /// Launch Services 没有“取消”原语，设空 bundle id 会回落到剩余候选——FluxDown 是唯一
    /// 候选时仍是 FluxDown。因此显式挑选另一个候选；没有候选时保持现状，由
    /// `<scheme>_assoc_user_disabled` 在捕获入口拦截系统交来的链接。
    pub fn unregister(proto: UrlScheme, _desktop: Option<&Path>) -> Result<(), PlatformError> {
        let Some(mine) = host_bundle_id() else {
            return Ok(());
        };
        if !handled_by(proto.scheme, &mine) {
            tracing::info!(
                scheme = proto.scheme,
                "URL protocol not registered to FluxDown, skipping removal"
            );
            return Ok(());
        }
        let candidates = macos_cf::scheme_handlers(proto.scheme)?;
        let Some(successor) =
            successor_handler(candidates, &mine).and_then(|id| workspace::app_for_bundle_id(&id))
        else {
            tracing::info!(
                scheme = proto.scheme,
                "no other handler installed; FluxDown stays the system default"
            );
            return Ok(());
        };
        set_handler(proto, &successor)?;
        if !workspace::settles(|| !handled_by(proto.scheme, &mine)) {
            tracing::info!(
                scheme = proto.scheme,
                successor = successor.id,
                "URL protocol handover declined"
            );
            return Err(PlatformError::Declined);
        }
        tracing::info!(
            scheme = proto.scheme,
            successor = successor.id,
            "handed URL protocol over"
        );
        Ok(())
    }
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "linux",
    all(target_os = "macos", feature = "desktop")
)))]
mod inner {
    use std::path::Path;

    use super::UrlScheme;
    use crate::platform::PlatformError;

    pub fn supported(_desktop: Option<&Path>) -> bool {
        false
    }

    pub fn is_registered(_proto: UrlScheme, _desktop: Option<&Path>) -> bool {
        false
    }

    pub fn register(_proto: UrlScheme, _desktop: Option<&Path>) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "URL protocol registration is not supported on this platform",
        ))
    }

    pub fn unregister(_proto: UrlScheme, _desktop: Option<&Path>) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "URL protocol registration is not supported on this platform",
        ))
    }
}

pub use inner::{is_registered, register, supported, unregister};
