//! yt-dlp 组件：宿主侧受控的单文件可执行程序，服务插件的媒体直链提取
//! （resolve 平面经 `flux.ytdlp` 调用）与站点下载能力——与插件沙箱（QuickJS）
//! 正交（见 [`crate::plugin`]）。
//!
//! 路径解析优先级（[`resolve_ytdlp`]）：
//! 1. 手动指定（config `component.ytdlp.path`，最高优先）
//! 2. 托管安装（数据目录 `bin/yt-dlp[.exe]`）
//! 3. 系统 PATH
//!
//! 托管安装（`install_ytdlp`，`components` feature 门控）从 yt-dlp/yt-dlp 的
//! GitHub Release 直接下载**单个平台二进制**（无归档解压，区别于 ffmpeg）：
//! Windows/Linux(glibc+musl)/macOS 全平台官方构建。可安装版本 = 近期 Release
//! 的日期 tag（[`list_ytdlp_versions`]）；二进制不随安装包分发，运行时按需由
//! 用户主动触发下载（合规边界）。

use std::path::{Path, PathBuf};

use super::{ComponentError, ComponentSource};
use crate::db::Db;

/// config 键：手动指定的 yt-dlp 绝对路径（空/缺失 = 未指定）。
pub const CONFIG_YTDLP_PATH: &str = "component.ytdlp.path";
/// config 键：托管安装的版本 tag（如 `2026.07.04`；空/缺失 = 未安装托管版本）。
pub const CONFIG_YTDLP_MANAGED_VERSION: &str = "component.ytdlp.managed_version";

/// yt-dlp 组件状态快照（探测结果，供设置页展示）。字段语义同
/// [`super::FfmpegStatus`]。
#[derive(Debug, Clone)]
pub struct YtdlpStatus {
    /// 生效路径的来源。
    pub source: ComponentSource,
    /// 生效的可执行文件路径（`source == None` 时为空）。
    pub path: String,
    /// `yt-dlp --version` 探测到的版本串（探测失败/未找到时为空）。
    pub version: String,
    /// 托管安装记录的版本 tag（config；与 `version` 独立）。
    pub managed_version: String,
    /// 系统 PATH 中探测到的 yt-dlp 路径（无论是否生效，供 UI 展示；空 = 无）。
    pub system_path: String,
    /// 当前平台是否提供托管安装（yt-dlp 官方全平台均提供，故常为 `true`）。
    pub managed_supported: bool,
}

/// 托管 yt-dlp 的目标路径：`<data_dir>/bin/yt-dlp[.exe]`。
pub fn managed_ytdlp_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join(ytdlp_binary_name())
}

fn ytdlp_binary_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

/// 当前平台对应的 yt-dlp 官方 Release 资产名。`None` = 无官方构建（几乎不
/// 会命中——yt-dlp 覆盖所有主流桌面/服务器平台）。musl 目标选 musllinux
/// 构建（FluxDown Linux 发行版为 musl-static），glibc 目标选普通 linux 构建。
fn platform_asset() -> Option<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("yt-dlp.exe")
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        Some("yt-dlp_arm64.exe")
    } else if cfg!(all(target_os = "windows", target_arch = "x86")) {
        Some("yt-dlp_x86.exe")
    } else if cfg!(target_os = "macos") {
        // 官方 macOS 构建为 universal2（x86_64 + arm64 通用）。
        Some("yt-dlp_macos")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "musl"
    )) {
        Some("yt-dlp_musllinux")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("yt-dlp_linux")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "aarch64",
        target_env = "musl"
    )) {
        Some("yt-dlp_musllinux_aarch64")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("yt-dlp_linux_aarch64")
    } else {
        None
    }
}

/// 当前平台是否提供 yt-dlp 托管安装（几乎所有平台均可）。
fn managed_install_supported() -> bool {
    platform_asset().is_some()
}

/// 扫描系统 PATH 寻找 yt-dlp 可执行文件。
pub fn find_system_ytdlp() -> Option<PathBuf> {
    super::find_in_path(ytdlp_binary_name())
}

/// 解析生效的 yt-dlp 路径（manual → managed → system）。
///
/// 供插件 `flux.ytdlp` 桥低成本调用：只做存在性检查，不探测版本。
pub async fn resolve_ytdlp(db: &Db, data_dir: &Path) -> Option<PathBuf> {
    if let Ok(Some(p)) = db.get_config(CONFIG_YTDLP_PATH).await
        && !p.trim().is_empty()
    {
        let manual = PathBuf::from(p.trim());
        if manual.is_file() {
            return Some(manual);
        }
        crate::log_info!(
            "[components] manual yt-dlp path invalid, falling back: {}",
            manual.display()
        );
    }
    let managed = managed_ytdlp_path(data_dir);
    if managed.is_file() {
        return Some(managed);
    }
    find_system_ytdlp()
}

/// 运行 `<path> --version` 解析版本串（yt-dlp 首行即版本，如 `2026.07.04`）。
pub async fn probe_ytdlp_version(path: &Path) -> Option<String> {
    super::exec_probe(path, "--version", None).await.ok()
}

/// 完整状态探测（设置页用）：解析生效路径 + 版本 + 系统路径展示。
pub async fn ytdlp_status(db: &Db, data_dir: &Path) -> YtdlpStatus {
    let managed_version = db
        .get_config(CONFIG_YTDLP_MANAGED_VERSION)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let system = find_system_ytdlp();
    let manual = match db.get_config(CONFIG_YTDLP_PATH).await {
        Ok(Some(p)) if !p.trim().is_empty() => Some(PathBuf::from(p.trim())),
        _ => None,
    };
    let managed = managed_ytdlp_path(data_dir);

    let (source, path) = if let Some(m) = manual.filter(|m| m.is_file()) {
        (ComponentSource::Manual, m)
    } else if managed.is_file() {
        (ComponentSource::Managed, managed)
    } else if let Some(s) = system.clone() {
        (ComponentSource::System, s)
    } else {
        (ComponentSource::None, PathBuf::new())
    };

    // yt-dlp.exe 是 PyInstaller 单文件包，`--version` 每次都要冷启动解包内置
    // Python 运行时（数百 ms~数秒），导致每次进设置页都可见地转圈加载。托管
    // 安装源的版本号即安装时记录的 tag（yt-dlp 版本号本身就是日期 tag，与
    // `--version` 首行输出一致），直接复用缓存值，跳过冷启动；手动/系统源无
    // 可信缓存版本，仍按需探测。
    let version = if source == ComponentSource::None {
        String::new()
    } else if source == ComponentSource::Managed && !managed_version.is_empty() {
        managed_version.clone()
    } else {
        probe_ytdlp_version(&path).await.unwrap_or_default()
    };

    YtdlpStatus {
        source,
        path: path.to_string_lossy().into_owned(),
        version,
        managed_version,
        system_path: system
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        managed_supported: managed_install_supported(),
    }
}

/// 卸载托管安装：删除 `bin/yt-dlp[.exe]` 与版本记录。手动/系统路径不受影响。
pub async fn uninstall_ytdlp(db: &Db, data_dir: &Path) -> Result<(), ComponentError> {
    let managed = managed_ytdlp_path(data_dir);
    match tokio::fs::remove_file(&managed).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ComponentError::Io(e.to_string())),
    }
    db.delete_config(CONFIG_YTDLP_MANAGED_VERSION)
        .await
        .map_err(|e| ComponentError::Db(e.to_string()))?;
    Ok(())
}

#[cfg(feature = "components")]
pub use install::{YtdlpVersions, install_ytdlp, list_ytdlp_versions};

/// 托管安装实现（`components` feature 门控：mobile/CLI 不背安装依赖）。
#[cfg(feature = "components")]
mod install {
    use std::path::Path;

    use super::super::ComponentError;
    use super::{CONFIG_YTDLP_MANAGED_VERSION, YtdlpStatus, managed_ytdlp_path, platform_asset};
    use crate::db::Db;

    /// yt-dlp Release 列表（分页）与指定 tag / latest 的 GitHub API 端点。
    const RELEASES_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases?per_page=30";
    const RELEASE_TAG_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/tags/";
    const RELEASE_LATEST_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
    /// 资产与校验文件必须位于此上游 release 下载前缀内。
    const UPSTREAM_DOWNLOAD_PREFIX: &str = "https://github.com/yt-dlp/yt-dlp/releases/download/";
    /// yt-dlp 每个 release 附带的 sha256sum 格式校验文件。
    const CHECKSUM_ASSET: &str = "SHA2-256SUMS";

    /// 可安装版本列表（近期 Release 的日期 tag，降序）。
    #[derive(Debug, Clone)]
    pub struct YtdlpVersions {
        /// 降序排列的版本 tag（如 `["2026.07.04", "2026.06.30"]`）。
        pub versions: Vec<String>,
        /// 最新稳定版（= `versions` 首个；空 = 解析失败）。
        pub latest_stable: String,
    }

    /// 列出近期可安装版本（排除 draft/prerelease；GitHub 返回即按新→旧）。
    pub async fn list_ytdlp_versions(
        db: &Db,
        egress: &crate::egress::Egress,
    ) -> Result<YtdlpVersions, ComponentError> {
        platform_asset().ok_or(ComponentError::Unsupported)?;
        let clients = super::super::egress_clients(egress).await?;
        let mirror_base = super::super::component_mirror_base(db).await;
        let releases =
            super::super::fetch_versions_json(&clients, "ytdlp", RELEASES_API, &mirror_base)
                .await?;
        let empty = Vec::new();
        let arr = releases.as_array().unwrap_or(&empty);
        let versions: Vec<String> = arr
            .iter()
            .filter(|r| {
                !r["draft"].as_bool().unwrap_or(false)
                    && !r["prerelease"].as_bool().unwrap_or(false)
            })
            .filter_map(|r| r["tag_name"].as_str())
            .map(str::to_string)
            .collect();
        let latest_stable = versions.first().cloned().unwrap_or_default();
        Ok(YtdlpVersions {
            versions,
            latest_stable,
        })
    }

    /// 下载并安装指定版本（`None` = 最新稳定版）到数据目录 `bin/yt-dlp[.exe]`。
    ///
    /// `progress(downloaded, total)`：下载进度回调（total=0 表示未知）。
    /// 成功后写 config `component.ytdlp.managed_version` 并返回新状态。
    pub async fn install_ytdlp(
        db: &Db,
        data_dir: &Path,
        egress: &crate::egress::Egress,
        version: Option<&str>,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<YtdlpStatus, ComponentError> {
        let asset = platform_asset().ok_or(ComponentError::Unsupported)?;
        let clients = super::super::egress_clients(egress).await?;
        let mirror_base = super::super::component_mirror_base(db).await;
        let url = match version {
            Some(tag) => format!("{RELEASE_TAG_API}{tag}"),
            None => RELEASE_LATEST_API.to_string(),
        };
        let release = match super::super::fetch_github_json_with_mirror(&clients, &url, &mirror_base)
            .await
        {
            Ok(release) => release,
            Err(error) => {
                // 官网缓存的是列表，不代理单个 tag。只使用与安装请求匹配的条目。
                let Ok(releases) = super::super::fetch_official_versions_json(&clients, "ytdlp")
                    .await
                else {
                    return Err(error);
                };
                select_stable_release(releases, version).ok_or(error)?
            }
        };
        let chosen_ver = release["tag_name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ComponentError::NotFound("release tag_name".to_string()))?
            .to_string();
        let empty = Vec::new();
        let assets = release["assets"].as_array().unwrap_or(&empty);
        let asset_url = |name: &str| -> Option<String> {
            assets
                .iter()
                .find(|a| a["name"].as_str() == Some(name))
                .and_then(|a| a["browser_download_url"].as_str())
                .map(str::to_string)
        };
        let dl_url = asset_url(asset)
            .ok_or_else(|| ComponentError::NotFound(format!("asset {asset} ({chosen_ver})")))?;
        let sums_url = asset_url(CHECKSUM_ASSET).ok_or_else(|| {
            ComponentError::NotFound(format!("asset {CHECKSUM_ASSET} ({chosen_ver})"))
        })?;
        // 版本 JSON 可能经镜像而来：资产与校验文件都必须落在上游发布路径下。
        super::super::require_upstream_asset_url(&dl_url, UPSTREAM_DOWNLOAD_PREFIX)?;
        super::super::require_upstream_asset_url(&sums_url, UPSTREAM_DOWNLOAD_PREFIX)?;
        // 校验和是完整性锚：只走代理，不交给插件改写。
        let expected_sha256 =
            super::super::fetch_expected_sha256(&clients, &sums_url, &mirror_base, asset).await?;

        // 单文件二进制：流式下载到 bin/ 下临时文件，哈希 + 探版都通过后才原子替换
        // 目标，失败不碰原有托管二进制。Windows 上临时文件须保留 `.exe` 才能探版。
        let bin_dir = data_dir.join("bin");
        tokio::fs::create_dir_all(&bin_dir)
            .await
            .map_err(|e| ComponentError::Io(e.to_string()))?;
        let target = managed_ytdlp_path(data_dir);
        let tmp = bin_dir.join(if cfg!(windows) {
            "yt-dlp.download.exe"
        } else {
            "yt-dlp.download"
        });
        let route = egress.route_verified(&dl_url).await;
        super::super::download_to_file(
            &clients,
            &route,
            &mirror_base,
            &tmp,
            &expected_sha256,
            progress,
        )
        .await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            if let Err(e) = tokio::fs::set_permissions(&tmp, perms).await {
                if let Err(error) = tokio::fs::remove_file(&tmp).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("components", "remove_ytdlp_download", &error);
                }
                return Err(ComponentError::Io(e.to_string()));
            }
        }

        // 安装前验证：临时文件能跑 `--version` 才算成功。
        if super::probe_ytdlp_version(&tmp).await.is_none() {
            if let Err(error) = tokio::fs::remove_file(&tmp).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("components", "remove_unverified_ytdlp", &error);
            }
            return Err(ComponentError::Verify(
                "downloaded yt-dlp failed to run; the binary may be incompatible with \
                 this system — install yt-dlp via your system package manager (or pip) \
                 and set a manual path"
                    .to_string(),
            ));
        }
        if let Err(e) = tokio::fs::rename(&tmp, &target).await {
            if let Err(error) = tokio::fs::remove_file(&tmp).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("components", "remove_ytdlp_download", &error);
            }
            return Err(ComponentError::Io(e.to_string()));
        }
        db.set_config(CONFIG_YTDLP_MANAGED_VERSION, &chosen_ver)
            .await
            .map_err(|e| ComponentError::Db(e.to_string()))?;
        crate::log_info!(
            "[components] yt-dlp {} installed to {}",
            chosen_ver,
            target.display()
        );
        Ok(super::ytdlp_status(db, data_dir).await)
    }

    fn select_stable_release(
        releases: serde_json::Value,
        version: Option<&str>,
    ) -> Option<serde_json::Value> {
        let serde_json::Value::Array(releases) = releases else {
            return None;
        };
        releases.into_iter().find(|release| {
            release["draft"].as_bool() == Some(false)
                && release["prerelease"].as_bool() == Some(false)
                && release["tag_name"]
                    .as_str()
                    .is_some_and(|tag| !tag.is_empty() && version.is_none_or(|v| v == tag))
        })
    }

    #[cfg(test)]
    #[test]
    fn install_fallback_uses_requested_tag_or_first_stable_release() {
        let releases = serde_json::json!([
            {"tag_name": "nightly", "draft": false, "prerelease": true},
            {"tag_name": "draft", "draft": true, "prerelease": false},
            {"tag_name": "new", "draft": false, "prerelease": false},
            {"tag_name": "old", "draft": false, "prerelease": false}
        ]);
        assert_eq!(
            select_stable_release(releases.clone(), None).unwrap()["tag_name"],
            "new"
        );
        assert_eq!(
            select_stable_release(releases.clone(), Some("old")).unwrap()["tag_name"],
            "old"
        );
        assert!(select_stable_release(releases.clone(), Some("nightly")).is_none());
        assert!(select_stable_release(releases, Some("missing")).is_none());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{managed_ytdlp_path, platform_asset};
    use std::path::Path;

    #[test]
    fn managed_path_layout() {
        let p = managed_ytdlp_path(Path::new("/data"));
        let expected = if cfg!(windows) {
            "yt-dlp.exe"
        } else {
            "yt-dlp"
        };
        assert!(p.ends_with(Path::new("bin").join(expected)));
    }

    #[test]
    fn platform_asset_present_on_supported_targets() {
        // 主流桌面/服务器目标均应有官方资产（本测试在 CI 目标上运行时命中）。
        if cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )) {
            assert!(platform_asset().is_some());
        }
    }
}
