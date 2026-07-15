//! ffmpeg 组件：宿主侧受控二进制，服务 DASH/轨对任务的音视频 mux。
//!
//! 路径解析优先级（[`resolve_ffmpeg`]）：
//! 1. 手动指定（config `component.ffmpeg.path`，用户显式覆盖，最高优先）
//! 2. 托管安装（数据目录 `bin/ffmpeg[.exe]`，用户在组件页主动安装的版本）
//! 3. 系统 PATH（默认探测；多数 Linux 发行版经包管理器安装）
//!
//! 托管安装（`install_ffmpeg`，`components` feature 门控）从
//! BtbN/FFmpeg-Builds 的 GitHub `latest` Release 下载静态构建：该 Release
//! 同时携带多个 `n<major.minor>` 版本资产，一次请求即可列出全部可选版本
//! （[`list_versions`]），用户可钉住特定版本或更新到最新稳定版。
//! macOS 无 BtbN 构建，仅支持系统探测 + 手动指定。

use std::path::{Path, PathBuf};

use super::{ComponentError, ComponentSource};
use crate::db::Db;

/// config 键：手动指定的 ffmpeg 绝对路径（空/缺失 = 未指定）。
pub const CONFIG_FFMPEG_PATH: &str = "component.ffmpeg.path";
/// config 键：托管安装的版本号（如 `7.1`；空/缺失 = 未安装托管版本）。
pub const CONFIG_FFMPEG_MANAGED_VERSION: &str = "component.ffmpeg.managed_version";

/// ffmpeg 组件状态快照（探测结果，供设置页展示）。
#[derive(Debug, Clone)]
pub struct FfmpegStatus {
    /// 生效路径的来源。
    pub source: ComponentSource,
    /// 生效的可执行文件路径（`source == None` 时为空）。
    pub path: String,
    /// `ffmpeg -version` 探测到的版本串（探测失败/未找到时为空）。
    pub version: String,
    /// 托管安装记录的版本号（config；与 `version` 独立——生效的可能是
    /// 手动/系统路径）。
    pub managed_version: String,
    /// 系统 PATH 中探测到的 ffmpeg 路径（无论是否生效，供 UI 展示；空 = 无）。
    pub system_path: String,
    /// 当前平台是否提供托管安装（BtbN 构建）。`false` = macOS 等无官方静态
    /// 构建的平台——UI 应隐藏托管安装入口，只引导系统 PATH / 手动指定，
    /// 避免反复弹「不支持安装」。仅按 OS+架构判定（不含 musl 运行时差异，
    /// 后者在安装探测阶段兜底）。
    pub managed_supported: bool,
}

/// 托管 ffmpeg 的目标路径：`<data_dir>/bin/ffmpeg[.exe]`。
pub fn managed_ffmpeg_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join(ffmpeg_binary_name())
}

fn ffmpeg_binary_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// 托管 ffprobe 的目标路径：`<data_dir>/bin/ffprobe[.exe]`（随 ffmpeg 一并安装）。
pub fn managed_ffprobe_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join(ffprobe_binary_name())
}

fn ffprobe_binary_name() -> &'static str {
    if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    }
}

/// 当前平台是否提供 ffmpeg 托管安装（BtbN/FFmpeg-Builds 官方静态构建）。
///
/// BtbN 仅发布 win64/winarm64/linux64/linuxarm64 构建，无 macOS 构建。仅按
/// 编译期 OS + 架构判定，供状态探测在**不触网**的前提下让 UI 决定是否展示
/// 托管安装入口——避免在 macOS 等平台上每次打开组件页都发起注定失败的版本
/// 拉取并弹错。
///
/// 注意：不判定 musl/glibc。Linux 服务器二进制多为 musl 静态链接，但宿主
/// 通常带 glibc、能运行下载来的 glibc 构建，故这里仍返回 `true`；真正 musl
/// 用户态（Alpine/OpenWrt）无法运行的情况由 [`install`] 的安装后 `-version`
/// 探测兜底（返回 [`ComponentError::Verify`] 并给出改用系统包管理器的提示）。
fn managed_install_supported() -> bool {
    cfg!(all(
        target_os = "windows",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )) || cfg!(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
}

/// 扫描系统 PATH 寻找 ffmpeg 可执行文件。
pub fn find_system_ffmpeg() -> Option<PathBuf> {
    super::find_in_path(ffmpeg_binary_name())
}

/// 解析生效的 ffmpeg 路径（manual → managed → system）。
///
/// 供下载链路（mux）低成本调用：只做存在性检查，不探测版本。
/// 返回 `None` 时调用方可回退 `Command::new("ffmpeg")` 保持旧行为。
pub async fn resolve_ffmpeg(db: &Db, data_dir: &Path) -> Option<PathBuf> {
    if let Ok(Some(p)) = db.get_config(CONFIG_FFMPEG_PATH).await
        && !p.trim().is_empty()
    {
        let manual = PathBuf::from(p.trim());
        if manual.is_file() {
            return Some(manual);
        }
        // 手动路径失效：不静默回退到其它来源之外，仅记录（用户显式指定优先，
        // 但坏路径不该让 mux 彻底失能）。
        crate::log_info!(
            "[components] manual ffmpeg path invalid, falling back: {}",
            manual.display()
        );
    }
    let managed = managed_ffmpeg_path(data_dir);
    if managed.is_file() {
        return Some(managed);
    }
    find_system_ffmpeg()
}

/// 解析生效的 ffprobe 路径（手动 ffmpeg 同目录 → 托管 → 系统 PATH）。
///
/// 供插件 `flux.ffprobe` 结构化探测（`ffprobe -print_format json -show_format
/// -show_streams`）用；随托管 ffmpeg 一并安装，yt-dlp 也经 `--ffmpeg-location`
/// 所在目录自动发现它。
pub async fn resolve_ffprobe(db: &Db, data_dir: &Path) -> Option<PathBuf> {
    if let Ok(Some(p)) = db.get_config(CONFIG_FFMPEG_PATH).await
        && !p.trim().is_empty()
    {
        // 用户手动指定 ffmpeg 时，同目录通常也有 ffprobe。
        let manual = PathBuf::from(p.trim());
        if let Some(dir) = manual.parent() {
            let cand = dir.join(ffprobe_binary_name());
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    let managed = managed_ffprobe_path(data_dir);
    if managed.is_file() {
        return Some(managed);
    }
    super::find_in_path(ffprobe_binary_name())
}

/// 运行 `<path> -version` 解析版本串（如 `7.1` / `n7.1-...` 原样 token）。
pub async fn probe_version(path: &Path) -> Option<String> {
    let output = tokio::process::Command::new(path)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 首行形如 "ffmpeg version 7.1-static ..."，取第三个 token。
    let first_line = stdout.lines().next()?;
    let token = first_line.split_whitespace().nth(2)?;
    Some(token.to_string())
}

/// 完整状态探测（设置页用）：解析生效路径 + 版本 + 系统路径展示。
pub async fn ffmpeg_status(db: &Db, data_dir: &Path) -> FfmpegStatus {
    let managed_version = db
        .get_config(CONFIG_FFMPEG_MANAGED_VERSION)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let system = find_system_ffmpeg();
    let manual = match db.get_config(CONFIG_FFMPEG_PATH).await {
        Ok(Some(p)) if !p.trim().is_empty() => Some(PathBuf::from(p.trim())),
        _ => None,
    };
    let managed = managed_ffmpeg_path(data_dir);

    let (source, path) = if let Some(m) = manual.filter(|m| m.is_file()) {
        (ComponentSource::Manual, m)
    } else if managed.is_file() {
        (ComponentSource::Managed, managed)
    } else if let Some(s) = system.clone() {
        (ComponentSource::System, s)
    } else {
        (ComponentSource::None, PathBuf::new())
    };

    let version = if source == ComponentSource::None {
        String::new()
    } else {
        probe_version(&path).await.unwrap_or_default()
    };

    FfmpegStatus {
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

/// 卸载托管安装：删除 `bin/ffmpeg[.exe]` 与版本记录。手动/系统路径不受影响。
pub async fn uninstall_ffmpeg(db: &Db, data_dir: &Path) -> Result<(), ComponentError> {
    let managed = managed_ffmpeg_path(data_dir);
    match tokio::fs::remove_file(&managed).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ComponentError::Io(e.to_string())),
    }
    // ffprobe 随 ffmpeg 一并安装，一并清除（best-effort，缺失不报错）。
    let _ = tokio::fs::remove_file(managed_ffprobe_path(data_dir)).await;
    db.delete_config(CONFIG_FFMPEG_MANAGED_VERSION)
        .await
        .map_err(|e| ComponentError::Db(e.to_string()))?;
    Ok(())
}

#[cfg(feature = "components")]
pub use install::{FfmpegVersions, install_ffmpeg, list_versions};

/// 托管安装实现（`components` feature 门控：mobile/CLI 不背 zip 依赖）。
#[cfg(feature = "components")]
mod install {
    use std::path::Path;

    use super::{CONFIG_FFMPEG_MANAGED_VERSION, ComponentError, FfmpegStatus, managed_ffmpeg_path};
    use crate::db::Db;

    /// BtbN/FFmpeg-Builds latest Release 的 GitHub API 端点。
    const RELEASE_API: &str = "https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/latest";

    /// 可安装版本列表（解析自 latest Release 的资产名）。
    #[derive(Debug, Clone)]
    pub struct FfmpegVersions {
        /// 降序排列的稳定版本号（如 `["8.0", "7.1", "6.1"]`）。
        pub versions: Vec<String>,
        /// 最新稳定版（= `versions` 首个；空 = 解析失败）。
        pub latest_stable: String,
    }

    /// 当前平台在 BtbN 资产名中的标识。`None` = 平台无托管构建。
    fn platform_tag() -> Option<&'static str> {
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Some("win64")
        } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
            Some("winarm64")
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some("linux64")
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some("linuxarm64")
        } else {
            None
        }
    }

    /// 版本号比较键：`"7.1"` → `[7, 1]`。
    fn version_key(v: &str) -> Vec<u64> {
        v.split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    }

    /// 从资产名提取稳定版本号。资产名形如
    /// `ffmpeg-n7.1-latest-win64-gpl-7.1.zip`；master 构建（无 `n<ver>`）忽略。
    fn parse_asset_version(name: &str, plat: &str) -> Option<String> {
        let rest = name.strip_prefix("ffmpeg-n")?;
        let (ver, tail) = rest.split_once("-latest-")?;
        // 只收 gpl 非 shared 构建：`<plat>-gpl-<ver>.<ext>`
        let expected = format!("{plat}-gpl-{ver}");
        if !(tail == format!("{expected}.zip") || tail == format!("{expected}.tar.xz")) {
            return None;
        }
        Some(ver.to_string())
    }

    /// 列出当前平台可安装的稳定版本（降序）。
    pub async fn list_versions(client: &reqwest::Client) -> Result<FfmpegVersions, ComponentError> {
        let plat = platform_tag().ok_or(ComponentError::Unsupported)?;
        let release = super::super::fetch_versions_json(client, "ffmpeg", RELEASE_API).await?;
        let empty = Vec::new();
        let assets = release["assets"].as_array().unwrap_or(&empty);
        let mut versions: Vec<String> = assets
            .iter()
            .filter_map(|a| a["name"].as_str())
            .filter_map(|n| parse_asset_version(n, plat))
            .collect();
        versions.sort_by_key(|v| std::cmp::Reverse(version_key(v)));
        versions.dedup();
        let latest_stable = versions.first().cloned().unwrap_or_default();
        Ok(FfmpegVersions {
            versions,
            latest_stable,
        })
    }

    /// 下载并安装指定版本（`None` = 最新稳定版）到数据目录 `bin/`。
    ///
    /// `progress(downloaded, total)`：下载进度回调（total=0 表示未知）。
    /// 成功后写 config `component.ffmpeg.managed_version` 并返回新状态。
    pub async fn install_ffmpeg(
        db: &Db,
        data_dir: &Path,
        client: &reqwest::Client,
        version: Option<&str>,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<FfmpegStatus, ComponentError> {
        let plat = platform_tag().ok_or(ComponentError::Unsupported)?;
        let release = super::super::fetch_github_json(client, RELEASE_API).await?;
        let empty = Vec::new();
        let assets = release["assets"].as_array().unwrap_or(&empty);

        // 选定资产：钉住版本或最新稳定版。
        let mut candidates: Vec<(String, &str)> = assets
            .iter()
            .filter_map(|a| {
                let name = a["name"].as_str()?;
                let url = a["browser_download_url"].as_str()?;
                let ver = parse_asset_version(name, plat)?;
                Some((ver, url))
            })
            .collect();
        candidates.sort_by_key(|(v, _)| std::cmp::Reverse(version_key(v)));
        let (chosen_ver, url) = match version {
            Some(want) => candidates
                .iter()
                .find(|(v, _)| v == want)
                .ok_or_else(|| ComponentError::NotFound(format!("version {want} ({plat})")))?,
            None => candidates
                .first()
                .ok_or_else(|| ComponentError::NotFound(format!("no builds for {plat}")))?,
        };
        let chosen_ver = chosen_ver.clone();
        let url = url.to_string();

        // 流式下载到 bin/ 下的临时文件。
        let bin_dir = data_dir.join("bin");
        tokio::fs::create_dir_all(&bin_dir)
            .await
            .map_err(|e| ComponentError::Io(e.to_string()))?;
        let archive_ext = if url.ends_with(".zip") {
            "zip"
        } else {
            "tar.xz"
        };
        let archive_path = bin_dir.join(format!("ffmpeg.download.{archive_ext}"));
        super::super::download_to_file(client, &url, &archive_path, progress).await?;

        // 解压出 `bin/ffmpeg[.exe]`（必需）+ `bin/ffprobe[.exe]`（best-effort）。
        let extract_result = extract_binaries(&archive_path, &bin_dir).await;
        let _ = tokio::fs::remove_file(&archive_path).await;
        extract_result?;

        // 安装后验证：能跑 `-version` 才算成功。
        let target = managed_ffmpeg_path(data_dir);
        let probed = super::probe_version(&target).await;
        if probed.is_none() {
            let _ = tokio::fs::remove_file(&target).await;
            return Err(ComponentError::Verify(
                "downloaded ffmpeg failed to run; this system may use musl libc \
                 (e.g. Alpine/OpenWrt) which cannot run the official glibc build — \
                 install ffmpeg via your system package manager and set a manual path"
                    .to_string(),
            ));
        }
        db.set_config(CONFIG_FFMPEG_MANAGED_VERSION, &chosen_ver)
            .await
            .map_err(|e| ComponentError::Db(e.to_string()))?;
        crate::log_info!(
            "[components] ffmpeg {} installed to {}",
            chosen_ver,
            target.display()
        );
        Ok(super::ffmpeg_status(db, data_dir).await)
    }

    /// 从归档提取 `bin/ffmpeg[.exe]`（必需）与 `bin/ffprobe[.exe]`（best-effort，
    /// 供 yt-dlp 后处理 / 插件 `flux.ffprobe` 结构化探测用）到 `bin_dir`。
    async fn extract_binaries(archive: &Path, bin_dir: &Path) -> Result<(), ComponentError> {
        let ffmpeg = super::ffmpeg_binary_name();
        let ffprobe = super::ffprobe_binary_name();
        if cfg!(windows) {
            extract_from_zip(archive, bin_dir, ffmpeg, ffprobe).await
        } else {
            extract_from_tar_xz(archive, bin_dir, ffmpeg, ffprobe).await
        }
    }

    /// Windows：zip crate（同步 API，spawn_blocking）。一次打开定位并提取 ffmpeg.exe
    /// （必需）+ ffprobe.exe（best-effort）。
    async fn extract_from_zip(
        archive: &Path,
        bin_dir: &Path,
        ffmpeg: &str,
        ffprobe: &str,
    ) -> Result<(), ComponentError> {
        let archive = archive.to_path_buf();
        let ffmpeg_target = bin_dir.join(ffmpeg);
        let ffprobe_target = bin_dir.join(ffprobe);
        tokio::task::spawn_blocking(move || -> Result<(), ComponentError> {
            let file =
                std::fs::File::open(&archive).map_err(|e| ComponentError::Io(e.to_string()))?;
            let mut zip =
                zip::ZipArchive::new(file).map_err(|e| ComponentError::Archive(e.to_string()))?;
            let (mut ffmpeg_idx, mut ffprobe_idx) = (None, None);
            for i in 0..zip.len() {
                let entry = zip
                    .by_index(i)
                    .map_err(|e| ComponentError::Archive(e.to_string()))?;
                let name = entry.name().replace('\\', "/");
                if name.ends_with("/bin/ffmpeg.exe") {
                    ffmpeg_idx = Some(i);
                } else if name.ends_with("/bin/ffprobe.exe") {
                    ffprobe_idx = Some(i);
                }
            }
            let idx = ffmpeg_idx.ok_or_else(|| {
                ComponentError::Archive("ffmpeg.exe not found in archive".to_string())
            })?;
            extract_zip_entry(&mut zip, idx, &ffmpeg_target)?;
            // ffprobe best-effort：缺失/失败只记日志，不影响 ffmpeg 安装成功。
            match ffprobe_idx {
                Some(pi) => {
                    if let Err(e) = extract_zip_entry(&mut zip, pi, &ffprobe_target) {
                        crate::log_info!("[components] ffprobe extract skipped: {}", e);
                    }
                }
                None => crate::log_info!("[components] ffprobe not in archive, skipped"),
            }
            Ok(())
        })
        .await
        .map_err(|e| ComponentError::Io(format!("join error: {e}")))?
    }

    /// 提取 zip 第 `idx` 条目到 `target`（tmp + rename 原子替换）。
    fn extract_zip_entry(
        zip: &mut zip::ZipArchive<std::fs::File>,
        idx: usize,
        target: &Path,
    ) -> Result<(), ComponentError> {
        let mut entry = zip
            .by_index(idx)
            .map_err(|e| ComponentError::Archive(e.to_string()))?;
        let tmp = target.with_extension("tmp");
        let mut out = std::fs::File::create(&tmp).map_err(|e| ComponentError::Io(e.to_string()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| ComponentError::Io(e.to_string()))?;
        drop(out);
        std::fs::rename(&tmp, target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            ComponentError::Io(e.to_string())
        })
    }

    /// Linux：tar.xz 经系统 `tar -xJf` 解压（tar+xz 为发行版基础组件，避免引入
    /// xz2/tar crate 新依赖）。一次解压取 ffmpeg（必需）+ ffprobe（best-effort）。
    async fn extract_from_tar_xz(
        archive: &Path,
        bin_dir: &Path,
        ffmpeg: &str,
        ffprobe: &str,
    ) -> Result<(), ComponentError> {
        let extract_dir = bin_dir.join("ffmpeg.extract.tmp");
        let _ = tokio::fs::remove_dir_all(&extract_dir).await;
        tokio::fs::create_dir_all(&extract_dir)
            .await
            .map_err(|e| ComponentError::Io(e.to_string()))?;
        let output = tokio::process::Command::new("tar")
            .arg("-xJf")
            .arg(archive)
            .arg("-C")
            .arg(&extract_dir)
            .output()
            .await
            .map_err(|e| ComponentError::Archive(format!("failed to run tar: {e}")))?;
        if !output.status.success() {
            let _ = tokio::fs::remove_dir_all(&extract_dir).await;
            return Err(ComponentError::Archive(format!(
                "tar exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(300)
                    .collect::<String>()
            )));
        }
        // 归档内布局：ffmpeg-nX.Y-latest-<plat>-gpl-X.Y/bin/{ffmpeg,ffprobe}
        let result = match find_in_extract(&extract_dir, ffmpeg).await {
            Some(src) => move_into_bin(&src, &bin_dir.join(ffmpeg)).await,
            None => Err(ComponentError::Archive(
                "ffmpeg not found in archive".to_string(),
            )),
        };
        // ffprobe best-effort。
        if result.is_ok() {
            match find_in_extract(&extract_dir, ffprobe).await {
                Some(src) => {
                    if let Err(e) = move_into_bin(&src, &bin_dir.join(ffprobe)).await {
                        crate::log_info!("[components] ffprobe extract skipped: {}", e);
                    }
                }
                None => crate::log_info!("[components] ffprobe not in archive, skipped"),
            }
        }
        let _ = tokio::fs::remove_dir_all(&extract_dir).await;
        result
    }

    /// 把解压出的二进制搬到 `bin/` 目标位置并置可执行位（跨设备回退 copy）。
    async fn move_into_bin(src: &Path, target: &Path) -> Result<(), ComponentError> {
        match tokio::fs::rename(src, target).await {
            Ok(()) => {}
            Err(_) => {
                tokio::fs::copy(src, target)
                    .await
                    .map_err(|e| ComponentError::Io(e.to_string()))?;
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            tokio::fs::set_permissions(target, perms)
                .await
                .map_err(|e| ComponentError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// 在解压目录下寻找 `*/bin/<name>`（两层定位，避免全量递归）。
    async fn find_in_extract(extract_dir: &Path, name: &str) -> Option<std::path::PathBuf> {
        let mut top = tokio::fs::read_dir(extract_dir).await.ok()?;
        while let Ok(Some(entry)) = top.next_entry().await {
            let candidate = entry.path().join("bin").join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    #[cfg(test)]
    #[allow(clippy::unwrap_used)]
    mod tests {
        use super::{parse_asset_version, version_key};

        #[test]
        fn asset_version_parsing() {
            assert_eq!(
                parse_asset_version("ffmpeg-n7.1-latest-win64-gpl-7.1.zip", "win64"),
                Some("7.1".to_string())
            );
            assert_eq!(
                parse_asset_version("ffmpeg-n6.1-latest-linux64-gpl-6.1.tar.xz", "linux64"),
                Some("6.1".to_string())
            );
            // master 构建无 n 前缀 → 忽略
            assert_eq!(
                parse_asset_version("ffmpeg-master-latest-win64-gpl.zip", "win64"),
                None
            );
            // shared 构建 → 忽略
            assert_eq!(
                parse_asset_version("ffmpeg-n7.1-latest-win64-gpl-shared-7.1.zip", "win64"),
                None
            );
            // 平台不符 → 忽略
            assert_eq!(
                parse_asset_version("ffmpeg-n7.1-latest-linux64-gpl-7.1.tar.xz", "win64"),
                None
            );
        }

        #[test]
        fn version_ordering() {
            let mut v = vec!["6.1".to_string(), "8.0".to_string(), "7.1".to_string()];
            v.sort_by_key(|x| std::cmp::Reverse(version_key(x)));
            assert_eq!(v, vec!["8.0", "7.1", "6.1"]);
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn zip_extract_pulls_ffmpeg_and_ffprobe() {
            use std::io::Write as _;
            let dir = std::env::temp_dir().join(format!(
                "fluxdown_zipx_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));

            let make_zip = |path: &std::path::Path, entries: &[(&str, &[u8])]| {
                let f = std::fs::File::create(path).unwrap();
                let mut zw = zip::ZipWriter::new(f);
                let opts = zip::write::SimpleFileOptions::default();
                for (name, bytes) in entries {
                    zw.start_file(*name, opts).unwrap();
                    zw.write_all(bytes).unwrap();
                }
                zw.finish().unwrap();
            };

            // 两者齐全：ffmpeg + ffprobe 均提取，内容精确。
            let both = dir.join("both.zip");
            let bin1 = dir.join("bin1");
            std::fs::create_dir_all(&bin1).unwrap();
            make_zip(
                &both,
                &[
                    ("build/bin/ffmpeg.exe", b"FFMPEG_BIN"),
                    ("build/bin/ffprobe.exe", b"FFPROBE_BIN"),
                ],
            );
            super::extract_from_zip(&both, &bin1, "ffmpeg.exe", "ffprobe.exe")
                .await
                .unwrap();
            assert_eq!(
                std::fs::read(bin1.join("ffmpeg.exe")).unwrap(),
                b"FFMPEG_BIN"
            );
            assert_eq!(
                std::fs::read(bin1.join("ffprobe.exe")).unwrap(),
                b"FFPROBE_BIN"
            );

            // 只有 ffmpeg：ffprobe best-effort 缺失不报错，ffmpeg 仍提取。
            let only = dir.join("only.zip");
            let bin2 = dir.join("bin2");
            std::fs::create_dir_all(&bin2).unwrap();
            make_zip(&only, &[("build/bin/ffmpeg.exe", b"ONLY_FF")]);
            super::extract_from_zip(&only, &bin2, "ffmpeg.exe", "ffprobe.exe")
                .await
                .unwrap();
            assert!(bin2.join("ffmpeg.exe").is_file());
            assert!(!bin2.join("ffprobe.exe").exists());

            // 无 ffmpeg：必错。
            let none = dir.join("none.zip");
            let bin3 = dir.join("bin3");
            std::fs::create_dir_all(&bin3).unwrap();
            make_zip(&none, &[("build/readme.txt", b"x")]);
            assert!(
                super::extract_from_zip(&none, &bin3, "ffmpeg.exe", "ffprobe.exe")
                    .await
                    .is_err()
            );

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::managed_ffmpeg_path;
    use std::path::Path;

    #[test]
    fn managed_path_layout() {
        let p = managed_ffmpeg_path(Path::new("/data"));
        let expected = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        assert!(p.ends_with(Path::new("bin").join(expected)));
    }
}
