//! `.torrent` 文件关联。
//!
//! Windows 通过 HKCU 注册表指向同级 `fluxdown-desktop.exe`（与 Inno Setup
//! 安装器写入的结构一致）：
//! ```text
//! HKCU\Software\Classes\.torrent                                → "FluxDown.TorrentFile"
//! HKCU\Software\Classes\FluxDown.TorrentFile                    → "BitTorrent File"
//! HKCU\Software\Classes\FluxDown.TorrentFile\DefaultIcon        → "<exe>,0"
//! HKCU\Software\Classes\FluxDown.TorrentFile\shell\open\command → "\"<exe>\" \"%1\""
//! ```
//! 运行期直接写 winreg，不在安装器 [Registry] 跟踪范围内，因此
//! `installer/windows/setup.iss` 卸载时显式执行 `RemoveTorrentAssociation`
//! ——两处需保持同步。
//!
//! Linux 通过 `xdg-mime` 把 `application/x-bittorrent` 交给打包的
//! `com.fluxdown.app.desktop`；macOS（`desktop` feature）按临时 `.torrent` 文件经
//! `NSWorkspace` 查询 / 设置「这类文件」的默认程序，与 Finder 同口径（其他程序导出的同扩展名
//! UTI 同样覆盖），macOS 12+ 阻塞到用户在系统确认框作答。

#[cfg(target_os = "windows")]
mod inner {
    use std::path::Path;

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    use crate::platform::{PlatformError, registry_executable};

    const PROG_ID: &str = "FluxDown.TorrentFile";
    const PROG_DESC: &str = "BitTorrent File";
    const EXT: &str = ".torrent";

    pub fn supported(desktop: Option<&Path>) -> bool {
        desktop.is_some()
    }

    /// `.torrent` 当前是否关联到 FluxDown。
    ///
    /// 只比较 `HKCU\Software\Classes\.torrent` 默认值是否为
    /// `FluxDown.TorrentFile`，不比较命令中的 exe 路径：安装器与运行进程的路径
    /// 表示可能不同（UNC 前缀、大小写、短名），ProgID 足以确认归属。
    pub fn is_associated() -> bool {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let Ok(ext_key) =
            hkcu.open_subkey_with_flags(format!("Software\\Classes\\{EXT}"), KEY_READ)
        else {
            return false;
        };
        ext_key
            .get_value::<String, _>("")
            .is_ok_and(|prog_id| prog_id == PROG_ID)
    }

    /// 把 `.torrent` 关联到桌面程序。
    pub fn associate(desktop: Option<&Path>) -> Result<(), PlatformError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let exe = registry_executable(desktop)?;

        let (ext_key, _) =
            hkcu.create_subkey_with_flags(format!("Software\\Classes\\{EXT}"), KEY_WRITE)?;
        ext_key.set_value("", &PROG_ID)?;

        let (prog_key, _) =
            hkcu.create_subkey_with_flags(format!("Software\\Classes\\{PROG_ID}"), KEY_WRITE)?;
        prog_key.set_value("", &PROG_DESC)?;

        let (icon_key, _) = hkcu.create_subkey_with_flags(
            format!("Software\\Classes\\{PROG_ID}\\DefaultIcon"),
            KEY_WRITE,
        )?;
        icon_key.set_value("", &format!("\"{exe}\",0"))?;

        let (cmd_key, _) = hkcu.create_subkey_with_flags(
            format!("Software\\Classes\\{PROG_ID}\\shell\\open\\command"),
            KEY_WRITE,
        )?;
        cmd_key.set_value("", &format!("\"{exe}\" \"%1\""))?;

        crate::platform::windows_shell::notify_association_changed();
        tracing::info!(exe, "associated .torrent with FluxDown");
        Ok(())
    }

    /// 移除 FluxDown 的 `.torrent` 关联；其他程序的关联不受影响。
    pub fn disassociate() -> Result<(), PlatformError> {
        if !is_associated() {
            tracing::info!(".torrent not associated to FluxDown, skipping removal");
            return Ok(());
        }
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let classes = hkcu.open_subkey_with_flags("Software\\Classes", KEY_WRITE)?;
        let mut first_error = None;
        for key in [EXT, PROG_ID] {
            if let Err(error) = classes.delete_subkey_all(key)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                // 仍尝试移除另一棵树；只忽略并发卸载已经删除的键，保留首个真实错误。
                tracing::warn!(key, %error, "could not remove torrent association registry key");
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        crate::platform::windows_shell::notify_association_changed();
        if let Some(error) = first_error {
            return Err(error.into());
        }
        tracing::info!("removed .torrent association");
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod inner {
    use std::path::Path;

    use crate::platform::PlatformError;
    use crate::platform::xdg::{DESKTOP_ENTRY, query_default_is_fluxdown, remove_default};

    const MIME_TYPE: &str = "application/x-bittorrent";

    pub fn supported(_desktop: Option<&Path>) -> bool {
        true
    }

    /// `.torrent` 当前是否关联到 FluxDown。
    pub fn is_associated() -> bool {
        query_default_is_fluxdown(MIME_TYPE)
    }

    /// 把 FluxDown 注册为 `.torrent` 默认处理程序。
    ///
    /// 要求打包的 `com.fluxdown.app.desktop` 已安装到 XDG applications 目录。
    pub fn associate(_desktop: Option<&Path>) -> Result<(), PlatformError> {
        let status = std::process::Command::new("xdg-mime")
            .args(["default", DESKTOP_ENTRY, MIME_TYPE])
            .status()?;
        if !status.success() {
            return Err(PlatformError::Failed(format!(
                "xdg-mime default exited with {status}"
            )));
        }
        tracing::info!("associated .torrent with FluxDown");
        Ok(())
    }

    /// 移除用户级覆盖，把 `.torrent` 交还系统默认。
    pub fn disassociate() -> Result<(), PlatformError> {
        remove_default(MIME_TYPE)?;
        tracing::info!("removed .torrent association");
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "desktop"))]
mod inner {
    use std::path::Path;

    use crate::platform::macos_cf;
    use crate::platform::macos_default_app::{self as workspace, ProbeFile};
    use crate::platform::{PlatformError, host_bundle_id, successor_handler};

    /// Info.plist 中声明的 `.torrent` UTI。
    const TORRENT_UTI: &str = "org.bittorrent.torrent";
    const TORRENT_EXTENSION: &str = "torrent";

    pub fn supported(_desktop: Option<&Path>) -> bool {
        host_bundle_id().is_some()
    }

    fn is_mine(probe: &ProbeFile, mine: &str) -> bool {
        probe
            .default_app()
            .is_some_and(|id| id.eq_ignore_ascii_case(mine))
    }

    /// 打开 `.torrent` 文件的默认程序是否为本 bundle（按临时 `.torrent` 文件查询，与 Finder
    /// 同口径：其他程序导出的同扩展名 UTI 同样覆盖）。
    pub fn is_associated() -> bool {
        let Some(mine) = host_bundle_id() else {
            return false;
        };
        match ProbeFile::create(TORRENT_EXTENSION) {
            Ok(probe) => is_mine(&probe, &mine),
            Err(error) => {
                tracing::warn!(%error, "failed to probe the .torrent default handler");
                false
            }
        }
    }

    /// 把本 bundle 设为 `.torrent` 文件的默认程序。macOS 12+ 阻塞到用户在系统确认框作答；
    /// 用户拒绝返回 [`PlatformError::Declined`]。
    pub fn associate(_desktop: Option<&Path>) -> Result<(), PlatformError> {
        let app = workspace::host_app().ok_or(PlatformError::Unsupported(
            "fluxdown-agent is not running inside an app bundle",
        ))?;
        let probe = ProbeFile::create(TORRENT_EXTENSION)?;
        if is_mine(&probe, &app.id) {
            return Ok(());
        }
        if workspace::confirmed_setters_available() {
            probe.set_default_app(&app)?;
        } else {
            set_all_content_types(&app.id)?;
        }
        if !workspace::settles(|| is_mine(&probe, &app.id)) {
            tracing::info!(".torrent association declined");
            return Err(PlatformError::Declined);
        }
        tracing::info!(bundle_id = app.id, "associated .torrent with FluxDown");
        Ok(())
    }

    /// 把 `.torrent` 移交给另一个已安装的候选程序；仅在当前由本 bundle 持有时执行。
    /// macOS 12+ 阻塞到用户作答，用户选「继续使用 FluxDown」返回 [`PlatformError::Declined`]。
    ///
    /// 没有其他候选时 Launch Services 必然回落到本 bundle，无从让出：保持现状，
    /// 由 `torrent_assoc_user_disabled` 在捕获入口拦截系统交来的文件。
    pub fn disassociate() -> Result<(), PlatformError> {
        let Some(mine) = host_bundle_id() else {
            return Ok(());
        };
        let probe = ProbeFile::create(TORRENT_EXTENSION)?;
        if !is_mine(&probe, &mine) {
            tracing::info!(".torrent not associated to FluxDown, skipping removal");
            return Ok(());
        }
        if workspace::confirmed_setters_available() {
            let mut candidates = probe.candidate_apps();
            let ids = candidates.iter().map(|app| app.id.clone()).collect();
            let Some(successor) = successor_handler(ids, &mine) else {
                return no_successor();
            };
            let Some(index) = candidates.iter().position(|app| app.id == successor) else {
                return no_successor();
            };
            probe.set_default_app(&candidates.swap_remove(index))?;
        } else {
            let candidates = macos_cf::content_handlers(TORRENT_UTI)?;
            let Some(successor) = successor_handler(candidates, &mine) else {
                return no_successor();
            };
            set_all_content_types(&successor)?;
        }
        if !workspace::settles(|| !is_mine(&probe, &mine)) {
            tracing::info!(".torrent handover declined");
            return Err(PlatformError::Declined);
        }
        tracing::info!("handed .torrent association over");
        Ok(())
    }

    fn no_successor() -> Result<(), PlatformError> {
        tracing::info!("no other .torrent handler installed; FluxDown stays the system default");
        Ok(())
    }

    /// macOS 11（无确认式接口、不弹框）：把扩展名映射到的全部 UTI（恒含 [`TORRENT_UTI`]）交给
    /// `bundle_id`。其他程序导出的同扩展名 UTI 压过本 bundle 的导入声明时，只设
    /// `org.bittorrent.torrent` 不会改变 Finder 的选择。
    fn set_all_content_types(bundle_id: &str) -> Result<(), PlatformError> {
        let mut types = macos_cf::content_types_for_extension(TORRENT_EXTENSION)?;
        if !types
            .iter()
            .any(|uti| uti.eq_ignore_ascii_case(TORRENT_UTI))
        {
            types.push(TORRENT_UTI.to_owned());
        }
        for content_type in types {
            macos_cf::set_content_handler(&content_type, bundle_id)
                .map_err(|error| PlatformError::Failed(format!("{content_type}: {error}")))?;
        }
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

    use crate::platform::PlatformError;

    pub fn supported(_desktop: Option<&Path>) -> bool {
        false
    }

    pub fn is_associated() -> bool {
        false
    }

    pub fn associate(_desktop: Option<&Path>) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "file association is not supported on this platform",
        ))
    }

    pub fn disassociate() -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "file association is not supported on this platform",
        ))
    }
}

pub use inner::{associate, disassociate, is_associated, supported};
