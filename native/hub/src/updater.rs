//! Auto-update module: version check via website API proxy, multi-segment
//! concurrent download of update packages, and launch installation.
//!
//! Platform strategies – all delegate waiting and file work to the compiled
//! `fluxdown_updater` helper binary that ships alongside the application:
//!
//!   Windows setup  (.exe)   → updater: unblocks MOTW, runs NSIS silently
//!   Windows portable (.zip) → updater: WaitForSingleObject, extracts ZIP, copies, restarts
//!   Linux AppImage          → updater: polls /proc/<pid>, atomic mv + chmod, restarts
//!   Linux deb (.deb)        → updater: polls /proc/<pid>, pkexec dpkg -i, restarts
//!   Linux arch (.pkg.zst)   → updater: polls /proc/<pid>, pkexec pacman -U, restarts
//!   Linux portable (.tar.gz)→ updater: polls /proc/<pid>, extracts tar.gz, copies, restarts
//!   macOS (.tar.gz/.app)    → updater: kill(pid,0) poll, extracts tar.gz, replaces .app, open
//!
//! Cold-start migration: users upgrading from a version that pre-dates
//! `fluxdown_updater` do not have the helper binary in their install directory.
//! `find_updater_bin` falls back to `bootstrap_updater_from_zip`, which extracts
//! the helper from the already-downloaded update ZIP and places a temporary copy
//! in the OS temp directory for this one update cycle.  Subsequent updates will
//! find the helper binary in the install directory (it was written there by the
//! first update).
//!
//! Using a compiled native helper instead of PowerShell/bat/sh scripts avoids:
//!   • PowerShell execution-policy and Smart App Control blocks on Windows 11
//!   • Mark-of-the-Web (MOTW/Zone.Identifier) interference on downloaded scripts
//!   • %VAR% expansion surprises and cmd.exe quoting pitfalls in batch files
//!   • Shell injection and escaping edge-cases in POSIX shell scripts
//!
//! All HTTP requests go through the website API (`/api/release`, `/api/download/:fn`).
//! Desktop auto-update lets `/api/download` do geo-routing: mainland-China clients
//! are 302'd to the self-hosted CN mirror (mirror.fluxdown.com — latest release
//! served locally at full speed, pruned older versions fall back to the GitHub
//! release CDN), everyone else goes straight to GitHub. Both the mirror and the
//! GitHub CDN honor Range requests, so the multi-segment download below works
//! transparently through the 302 redirect.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use rinf::RustSignal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::logger::log_info;
use crate::signals::{UpdateCheckResult, UpdateDownloadProgress};

/// Number of concurrent segments for update downloads.
/// Kept modest — update files are typically 10-30 MB and served from CDN.
const UPDATE_SEGMENTS: i32 = 8;

/// Minimum file size (bytes) to use multi-segment download. Below this
/// threshold the overhead of multiple connections is not worth it.
const MIN_SIZE_FOR_MULTI_SEGMENT: i64 = 2 * 1024 * 1024; // 2 MB

/// Per-segment retry budget for transient network errors. Each retry resumes
/// from the bytes already flushed to disk, never re-downloading the range.
const SEGMENT_RETRIES: u32 = 3;

/// Flush-to-disk interval. Resume progress counters only advance after a
/// flush, so on abrupt failure the sidecar never claims bytes that were still
/// sitting in tokio's write buffer (which would corrupt a resumed download).
const FLUSH_INTERVAL: i64 = 1024 * 1024; // 1 MB

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const UPDATE_API_BASE: &str = "https://fluxdown.zerx.dev";

#[cfg(target_os = "windows")]
const PORTABLE_MARKER: &str = "portable";

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum UpdateError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("semver error: {0}")]
    Semver(String),
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// API response types (matching website /api/release)
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "android"))]
#[derive(Deserialize)]
struct ReleaseInfo {
    version: String,
    published_at: String,
    assets: ReleaseAssets,
}

#[cfg(not(target_os = "android"))]
#[derive(Deserialize)]
struct ReleaseAssets {
    // Windows (unused on Linux but must be present for serde deserialization)
    #[allow(dead_code)]
    setup: Option<AssetInfo>,
    #[allow(dead_code)]
    portable: Option<AssetInfo>,
    #[allow(dead_code)]
    setup_arm64: Option<AssetInfo>,
    #[allow(dead_code)]
    portable_arm64: Option<AssetInfo>,
    // Linux (unused on Windows/macOS but must be present for serde deserialization)
    #[allow(dead_code)]
    linux_appimage: Option<AssetInfo>,
    #[allow(dead_code)]
    linux_deb: Option<AssetInfo>,
    #[allow(dead_code)]
    linux_arch: Option<AssetInfo>,
    #[allow(dead_code)]
    linux_tarball: Option<AssetInfo>,
    // macOS (unused on Windows/Linux but must be present for serde deserialization)
    // Field names MUST match website /api/release JSON keys exactly:
    //   macos_dmg_arm64, macos_dmg_x64, macos_tarball_arm64, macos_tarball_x64
    #[allow(dead_code)]
    macos_dmg_arm64: Option<AssetInfo>,
    #[allow(dead_code)]
    macos_dmg_x64: Option<AssetInfo>,
    #[allow(dead_code)]
    macos_tarball_arm64: Option<AssetInfo>,
    #[allow(dead_code)]
    macos_tarball_x64: Option<AssetInfo>,
}

#[derive(Deserialize)]
struct AssetInfo {
    #[allow(dead_code)]
    name: String,
    size: i64,
    download_url: String,
}

/// Android：`/api/release` 顶层 `mobile` 字段（独立 mobile-v* 版本线）。
/// `mobile` 为 `null` 表示尚无移动端 release —— 视为已是最新，而非错误。
#[cfg(target_os = "android")]
#[derive(Deserialize)]
struct MobileReleaseEnvelope {
    mobile: Option<MobileRelease>,
}

#[cfg(target_os = "android")]
#[derive(Deserialize)]
struct MobileRelease {
    version: String,
    assets: MobileAssets,
}

#[cfg(target_os = "android")]
#[derive(Deserialize)]
struct MobileAssets {
    android_arm64: Option<AssetInfo>,
    android_armv7: Option<AssetInfo>,
    android_x64: Option<AssetInfo>,
    android_universal: Option<AssetInfo>,
}

// ---------------------------------------------------------------------------
// Environment detection
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn is_portable() -> bool {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        return dir.join(PORTABLE_MARKER).exists();
    }
    false
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn is_arm64() -> bool {
    std::env::consts::ARCH == "aarch64"
}

/// Linux installation type detected at runtime.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
enum LinuxInstallType {
    /// Running as an AppImage ($APPIMAGE env is set by the AppImage runtime).
    AppImage,
    /// Installed via .deb package to /opt/fluxdown/ (dpkg can locate the exe).
    Deb,
    /// Installed via .pkg.tar.zst to /opt/fluxdown/ (pacman can locate the exe).
    Arch,
    /// Extracted tar.gz in any user-writable directory.
    Portable,
}

/// Detect how FluxDown was installed on this Linux system.
#[cfg(target_os = "linux")]
fn detect_linux_install_type() -> LinuxInstallType {
    // 1. AppImage: the AppImage runtime always sets $APPIMAGE to the path of
    //    the squashfs image being executed.
    if std::env::var("APPIMAGE").is_ok() {
        return LinuxInstallType::AppImage;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return LinuxInstallType::Portable,
    };
    let exe_str = exe.to_str().unwrap_or("");

    // 2. System package: both deb and arch install to /opt/fluxdown/.
    if exe_str.starts_with("/opt/fluxdown") {
        // Try dpkg first (Debian/Ubuntu).
        let dpkg_found = std::process::Command::new("dpkg")
            .args(["-S", exe_str])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if dpkg_found {
            return LinuxInstallType::Deb;
        }

        // Try pacman (Arch Linux).
        let pacman_found = std::process::Command::new("pacman")
            .args(["-Qo", exe_str])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if pacman_found {
            return LinuxInstallType::Arch;
        }
    }

    // 3. Fallback: portable tar.gz extracted to a user directory.
    LinuxInstallType::Portable
}

#[cfg(not(target_os = "android"))]
fn select_asset(assets: &ReleaseAssets) -> Option<&AssetInfo> {
    #[cfg(target_os = "windows")]
    {
        match (is_portable(), is_arm64()) {
            (true, true) => assets.portable_arm64.as_ref(),
            (true, false) => assets.portable.as_ref(),
            (false, true) => assets.setup_arm64.as_ref(),
            (false, false) => assets.setup.as_ref(),
        }
    }

    #[cfg(target_os = "linux")]
    {
        match detect_linux_install_type() {
            LinuxInstallType::AppImage => assets.linux_appimage.as_ref(),
            LinuxInstallType::Deb => assets.linux_deb.as_ref(),
            LinuxInstallType::Arch => assets.linux_arch.as_ref(),
            LinuxInstallType::Portable => assets.linux_tarball.as_ref(),
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Without an Apple Developer signing identity we cannot perform a
        // silent in-place .app replacement (Gatekeeper / quarantine /
        // App Translocation will block the result).  Instead we ship the DMG
        // and hand it to Finder via `open`, replicating the first-install UX
        // (drag .app to Applications).  Fall back to tarball if the release
        // happens to be missing a DMG asset.
        if is_arm64() {
            assets
                .macos_dmg_arm64
                .as_ref()
                .or(assets.macos_tarball_arm64.as_ref())
        } else {
            assets
                .macos_dmg_x64
                .as_ref()
                .or(assets.macos_tarball_x64.as_ref())
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Android：按运行时 ABI 选择 APK 资产，缺失时兜底 universal。
///
/// `std::env::consts::ARCH`：`aarch64` → arm64-v8a、`arm` → armeabi-v7a、
/// `x86_64` → x86_64。未知 ABI 直接取 universal。
#[cfg(target_os = "android")]
fn select_mobile_asset(assets: &MobileAssets) -> Option<&AssetInfo> {
    let preferred = match std::env::consts::ARCH {
        "aarch64" => assets.android_arm64.as_ref(),
        "arm" => assets.android_armv7.as_ref(),
        "x86_64" => assets.android_x64.as_ref(),
        _ => None,
    };
    preferred.or(assets.android_universal.as_ref())
}

// ---------------------------------------------------------------------------
// SemVer comparison (major.minor.patch with optional prerelease suffix)
// ---------------------------------------------------------------------------
//
// Release channels map to the SemVer prerelease suffix: stable builds are
// tagged `vX.Y.Z`, frontier builds `vX.Y.Z-rc.N` (any `-suffix`). Comparison
// follows SemVer 2.0 precedence (§11) so that:
//   * a stable release outranks its own prereleases: `1.3.0 > 1.3.0-rc.2`
//   * a prerelease of a higher core outranks a lower stable: `1.4.0-rc.1 > 1.3.0`
//   * prereleases order by dot-separated identifiers (numeric < non-numeric,
//     numeric compared numerically, text lexically).
// Build metadata (`+meta`) is ignored, as SemVer requires.

/// A single prerelease identifier. Numeric identifiers compare numerically and
/// always rank below alphanumeric ones (SemVer 2.0 §11.4).
enum PreId {
    Num(u64),
    Text(String),
}

/// Parsed semantic version: `major.minor.patch` core plus optional prerelease
/// identifiers. Only the subset FluxDown emits (`X.Y.Z` / `X.Y.Z-pre`) is used.
struct SemVer {
    core: (u64, u64, u64),
    pre: Vec<PreId>,
}

fn parse_semver(s: &str) -> Result<SemVer, UpdateError> {
    let s = s.strip_prefix('v').unwrap_or(s);
    // Drop build metadata (everything after the first '+').
    let s = s.split('+').next().unwrap_or(s);
    // Split the core from the prerelease suffix on the first '-'.
    let (core_str, pre_str) = match s.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (s, None),
    };

    let parts: Vec<&str> = core_str.split('.').collect();
    if parts.len() != 3 {
        return Err(UpdateError::Semver(format!("invalid version: {s}")));
    }
    let major = parts[0]
        .parse::<u64>()
        .map_err(|_| UpdateError::Semver(format!("invalid major: {}", parts[0])))?;
    let minor = parts[1]
        .parse::<u64>()
        .map_err(|_| UpdateError::Semver(format!("invalid minor: {}", parts[1])))?;
    let patch = parts[2]
        .parse::<u64>()
        .map_err(|_| UpdateError::Semver(format!("invalid patch: {}", parts[2])))?;

    let pre = match pre_str {
        None => Vec::new(),
        Some("") => {
            return Err(UpdateError::Semver(format!("empty prerelease: {s}")));
        }
        Some(p) => p
            .split('.')
            .map(|id| match id.parse::<u64>() {
                Ok(n) => PreId::Num(n),
                Err(_) => PreId::Text(id.to_string()),
            })
            .collect(),
    };

    Ok(SemVer {
        core: (major, minor, patch),
        pre,
    })
}

/// Compare prerelease identifier lists (SemVer 2.0 §11.4).
fn cmp_pre(a: &[PreId], b: &[PreId]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = match (x, y) {
            (PreId::Num(m), PreId::Num(n)) => m.cmp(n),
            (PreId::Text(m), PreId::Text(n)) => m.cmp(n),
            (PreId::Num(_), PreId::Text(_)) => Ordering::Less,
            (PreId::Text(_), PreId::Num(_)) => Ordering::Greater,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    // All shared identifiers equal → the longer set has higher precedence.
    a.len().cmp(&b.len())
}

/// SemVer 2.0 precedence. When cores are equal, a version with no prerelease
/// outranks one that carries a prerelease suffix.
fn cmp_semver(a: &SemVer, b: &SemVer) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match a.core.cmp(&b.core) {
        Ordering::Equal => {}
        non_eq => return non_eq,
    }
    match (a.pre.is_empty(), b.pre.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater, // stable > prerelease
        (false, true) => Ordering::Less,    // prerelease < stable
        (false, false) => cmp_pre(&a.pre, &b.pre),
    }
}

fn is_newer(latest: &str, current: &str) -> Result<bool, UpdateError> {
    let l = parse_semver(latest)?;
    let c = parse_semver(current)?;
    Ok(cmp_semver(&l, &c) == std::cmp::Ordering::Greater)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check for updates by querying the website API proxy.
/// Sends `UpdateCheckResult` signal back to Dart.
pub async fn check(current_version: &str, channel: &str) {
    let result = check_inner(current_version, channel).await;
    match result {
        Ok(()) => {} // signal already sent inside check_inner
        Err(e) => {
            UpdateCheckResult {
                has_update: false,
                latest_version: String::new(),
                current_version: current_version.to_string(),
                download_url: String::new(),
                file_size: 0,
                published_at: String::new(),
                error_message: e.to_string(),
            }
            .send_signal_to_dart();
        }
    }
}

#[cfg(not(target_os = "android"))]
async fn check_inner(current_version: &str, channel: &str) -> Result<(), UpdateError> {
    let client = Client::new();
    let url = format!("{UPDATE_API_BASE}/api/release?channel={channel}");

    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(15))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(UpdateError::Other(format!(
            "API returned status {}",
            resp.status()
        )));
    }

    let release: ReleaseInfo = resp.json().await?;
    let has_update = is_newer(&release.version, current_version).unwrap_or(false);

    let (download_url, file_size) = match select_asset(&release.assets) {
        Some(asset) => {
            let full_url = if asset.download_url.starts_with('/') {
                format!("{UPDATE_API_BASE}{}", asset.download_url)
            } else {
                asset.download_url.clone()
            };
            // 桌面自动更新经 /api/download 地域路由：大陆用户走国内镜像
            // (mirror.fluxdown.com，命中最新版满速、旧版回落 GitHub CDN)，海外直连 GitHub CDN。
            // 镜像与 GitHub CDN 均支持 Range，多段分段下载透过 302 重定向正常工作。
            (full_url, asset.size)
        }
        None => (String::new(), 0),
    };

    UpdateCheckResult {
        has_update,
        latest_version: release.version,
        current_version: current_version.to_string(),
        download_url,
        file_size,
        published_at: release.published_at,
        error_message: String::new(),
    }
    .send_signal_to_dart();

    Ok(())
}

/// Android 检查：解析 `/api/release` 顶层 `mobile` 字段（独立 mobile-v* 版本线，
/// 与桌面 `version` 无关）。`mobile == null`（尚无移动端 release）→ 已是最新。
#[cfg(target_os = "android")]
async fn check_inner(current_version: &str, channel: &str) -> Result<(), UpdateError> {
    let client = Client::new();
    let url = format!("{UPDATE_API_BASE}/api/release?channel={channel}");

    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(15))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(UpdateError::Other(format!(
            "API returned status {}",
            resp.status()
        )));
    }

    let envelope: MobileReleaseEnvelope = resp.json().await?;

    let Some(mobile) = envelope.mobile else {
        // 尚无 mobile-v* release —— 视为已是最新版本。
        UpdateCheckResult {
            has_update: false,
            latest_version: current_version.to_string(),
            current_version: current_version.to_string(),
            download_url: String::new(),
            file_size: 0,
            published_at: String::new(),
            error_message: String::new(),
        }
        .send_signal_to_dart();
        return Ok(());
    };

    let has_update = is_newer(&mobile.version, current_version).unwrap_or(false);

    let (download_url, file_size) = match select_mobile_asset(&mobile.assets) {
        Some(asset) => {
            let full_url = if asset.download_url.starts_with('/') {
                format!("{UPDATE_API_BASE}{}", asset.download_url)
            } else {
                asset.download_url.clone()
            };
            (full_url, asset.size)
        }
        None => (String::new(), 0),
    };

    UpdateCheckResult {
        // 没有可下载资产时不提示更新（无法完成下载流程）。
        has_update: has_update && !download_url.is_empty(),
        latest_version: mobile.version,
        current_version: current_version.to_string(),
        download_url,
        file_size,
        published_at: String::new(),
        error_message: String::new(),
    }
    .send_signal_to_dart();

    Ok(())
}

/// Download the update installer to a temp directory.
/// Sends periodic `UpdateDownloadProgress` signals to Dart.
///
/// Uses multi-segment concurrent downloading (like the main download engine)
/// to maximise throughput and showcase the product's core capability.
/// Falls back to single-stream when the server does not support Range requests.
/// A failed or interrupted download leaves the partial artifact plus a
/// `<file>.resume.json` sidecar behind; the next download attempt for the
/// same version resumes from the recorded per-segment offsets.
pub async fn download(url: &str, version: &str, file_size: i64) {
    let result = download_inner(url, version, file_size).await;
    if let Err(e) = result {
        UpdateDownloadProgress {
            version: version.to_string(),
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 0,
            status: 2, // error
            installer_path: String::new(),
            error_message: e.to_string(),
            segments: 0,
            active_segments: 0,
        }
        .send_signal_to_dart();
    }
}

// ---------------------------------------------------------------------------
// Security: filename & script interpolation sanitizers
// ---------------------------------------------------------------------------

/// Sanitize a filename extracted from a URL to prevent path-traversal attacks.
/// Strips directory components, `..` sequences, NUL bytes, and URL query
/// strings (`?…`) that would produce OS-invalid filenames on Windows.
/// Falls back to a safe default if the result is empty.
fn sanitize_filename(raw: &str) -> String {
    // Strip query string and fragment before extracting the filename.
    // Each fallback must land on the previously-stripped value, never the
    // original `raw` — otherwise a URL without a `#` fragment would restore
    // the query string and yield an OS-invalid Windows filename.
    let base = raw.split_once('?').map(|(b, _)| b).unwrap_or(raw);
    let without_query = base.split_once('#').map(|(b, _)| b).unwrap_or(base);

    let name = without_query
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .replace("..", "")
        .replace('\0', "");
    let name = name.trim();
    if name.is_empty() || name == "." {
        "FluxDown-update".to_string()
    } else {
        name.to_string()
    }
}

/// Where to deposit the downloaded update artifact.
///
/// On macOS we land in `~/Downloads` so users can find the DMG in the place
/// they expect after a "download from the web" — the in-app update UX is
/// indistinguishable from a manual download until they double-click.
/// On Windows/Linux we keep `temp_dir` because the helper binary handles the
/// install end-to-end and the artifact is disposable.
/// On Android we use the app-private cache dir (`/data/data/<pkg>/cache`) —
/// `TMPDIR` is not guaranteed in an Android app process, and the FileProvider
/// declared in the manifest exposes exactly this directory for the install
/// intent on the Dart/Kotlin side.
fn pick_download_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let downloads = PathBuf::from(home).join("Downloads");
            if downloads.is_dir() {
                return downloads;
            }
            // Best-effort create — fall through to temp_dir on failure.
            if std::fs::create_dir_all(&downloads).is_ok() {
                return downloads;
            }
        }
    }
    #[cfg(target_os = "android")]
    {
        if let Some(pkg) = fluxdown_engine::data_dir::android_package_name() {
            let cache = PathBuf::from(format!("/data/data/{pkg}/cache"));
            if cache.is_dir() || std::fs::create_dir_all(&cache).is_ok() {
                return cache;
            }
        }
    }
    std::env::temp_dir()
}

async fn download_inner(url: &str, version: &str, hint_file_size: i64) -> Result<(), UpdateError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;

    // ── Phase 1: Probe Range support via GET Range:0-0 ──────────────────
    // We already know the file size from the check phase (`hint_file_size`).
    // HEAD requests often fail through API proxies / CDN redirects (returning
    // 0 Content-Length), so we probe Range support with a tiny GET instead.
    let mut supports_range = false;
    let mut total_bytes = hint_file_size;

    let probe_resp = client.get(url).header("Range", "bytes=0-0").send().await;

    if let Ok(resp) = probe_resp {
        if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            supports_range = true;
            // Try to extract total size from Content-Range: bytes 0-0/<total>
            if let Some(cr) = resp.headers().get("content-range")
                && let Ok(cr_str) = cr.to_str()
                && let Some(slash_pos) = cr_str.rfind('/')
                && let Ok(size) = cr_str[slash_pos + 1..].parse::<i64>()
                && size > 0
            {
                total_bytes = size;
            }
        } else if resp.status().is_success() {
            // Server ignored Range, returned 200 OK — no Range support.
            // Try to get Content-Length as fallback for total_bytes.
            if total_bytes <= 0 {
                total_bytes = resp.content_length().unwrap_or(0) as i64;
            }
        }
    }

    // If we still don't know the size, it's not critical — progress will
    // show as indeterminate. But we can't do multi-segment without knowing it.

    let raw_name = url
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("FluxDown-update");
    let file_name = sanitize_filename(raw_name);
    let download_dir = pick_download_dir();
    let file_path = download_dir.join(&file_name);

    let use_multi = supports_range
        && total_bytes > 0
        && total_bytes >= MIN_SIZE_FOR_MULTI_SEGMENT
        && UPDATE_SEGMENTS > 1;

    log_info!(
        "[updater] download {} total_bytes={} (hint={}) supports_range={} multi={}",
        file_name,
        total_bytes,
        hint_file_size,
        supports_range,
        use_multi
    );

    if use_multi {
        download_multi_segment(url, version, &file_path, total_bytes, &client).await?;
    } else {
        download_single_stream(
            url,
            version,
            &file_path,
            total_bytes,
            &client,
            supports_range,
        )
        .await?;
    }

    let installer_path = file_path.to_string_lossy().to_string();

    // Send completion signal
    UpdateDownloadProgress {
        version: version.to_string(),
        downloaded_bytes: total_bytes,
        total_bytes,
        speed: 0,
        status: 1, // completed
        installer_path,
        error_message: String::new(),
        segments: if use_multi { UPDATE_SEGMENTS } else { 1 },
        active_segments: 0,
    }
    .send_signal_to_dart();

    Ok(())
}

// ---------------------------------------------------------------------------
// Single-stream fallback (original behaviour)
// ---------------------------------------------------------------------------

async fn download_single_stream(
    url: &str,
    version: &str,
    file_path: &PathBuf,
    total_bytes: i64,
    client: &Client,
    supports_range: bool,
) -> Result<(), UpdateError> {
    // Resume is possible only when the server honours Range and the final
    // size is known (the file is then pre-allocated to full size, exactly
    // like the multi-segment path, so `load_resume`'s length check applies).
    let track_resume = supports_range && total_bytes > 0;
    let ranges = [SegmentRange {
        start: 0,
        end: total_bytes - 1,
    }];

    let mut resume_from: i64 = 0;
    if track_resume && let Some(done) = load_resume(file_path, version, total_bytes, &ranges).await
    {
        resume_from = done[0].min(total_bytes);
    }
    if track_resume && resume_from >= total_bytes {
        // Everything already on disk from a previous attempt.
        let _ = tokio::fs::remove_file(resume_path(file_path)).await;
        return Ok(());
    }

    let mut request = client.get(url);
    if resume_from > 0 {
        request = request.header("Range", format!("bytes={resume_from}-"));
    }
    let resp = request.send().await?;
    if !resp.status().is_success() {
        return Err(UpdateError::Other(format!(
            "Download returned status {}",
            resp.status()
        )));
    }
    if resume_from > 0 && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        // Server ignored the Range header after all — restart from scratch.
        log_info!(
            "[updater] resume rejected (got {}), restarting",
            resp.status()
        );
        resume_from = 0;
    }

    let mut file = if track_resume {
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(file_path)
            .await?;
        file.set_len(total_bytes as u64).await?;
        file
    } else {
        tokio::fs::File::create(file_path).await?
    };
    if resume_from > 0 {
        file.seek(std::io::SeekFrom::Start(resume_from as u64))
            .await?;
    }

    let mut stream = resp.bytes_stream();

    let mut downloaded: i64 = resume_from;
    let mut flushed: i64 = resume_from;
    let mut unflushed: i64 = 0;
    let mut last_report = std::time::Instant::now();
    let mut last_downloaded_for_speed: i64 = downloaded;
    let mut last_speed_time = std::time::Instant::now();
    let report_interval = Duration::from_millis(200);
    let mut ticks: u32 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                // Persist progress (flushed bytes only) so the next attempt
                // resumes instead of restarting.
                if track_resume {
                    save_resume(file_path, version, total_bytes, &ranges, &[flushed]).await;
                }
                return Err(UpdateError::Http(e));
            }
        };
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as i64;
        unflushed += chunk.len() as i64;
        if unflushed >= FLUSH_INTERVAL {
            file.flush().await?;
            flushed = downloaded;
            unflushed = 0;
        }

        let now = std::time::Instant::now();
        if now.duration_since(last_report) >= report_interval {
            let elapsed_secs = now.duration_since(last_speed_time).as_secs_f64();
            let speed = if elapsed_secs > 0.0 {
                ((downloaded - last_downloaded_for_speed) as f64 / elapsed_secs) as i64
            } else {
                0
            };
            last_downloaded_for_speed = downloaded;
            last_speed_time = now;

            UpdateDownloadProgress {
                version: version.to_string(),
                downloaded_bytes: downloaded,
                total_bytes,
                speed,
                status: 0,
                installer_path: String::new(),
                error_message: String::new(),
                segments: 1,
                active_segments: 1,
            }
            .send_signal_to_dart();

            last_report = now;
            ticks += 1;
            if track_resume && ticks.is_multiple_of(5) {
                save_resume(file_path, version, total_bytes, &ranges, &[flushed]).await;
            }
        }
    }

    file.flush().await?;
    flushed = downloaded;

    if track_resume {
        if downloaded < total_bytes {
            // Connection closed early — keep state for the next attempt.
            save_resume(file_path, version, total_bytes, &ranges, &[flushed]).await;
            return Err(UpdateError::Other(format!(
                "connection closed early: {downloaded}/{total_bytes} bytes"
            )));
        }
        let _ = tokio::fs::remove_file(resume_path(file_path)).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Resume state (sidecar file next to the partially-downloaded artifact)
// ---------------------------------------------------------------------------

/// Sidecar recording per-segment progress so a failed/interrupted update
/// download resumes instead of restarting. The `version` field guards against
/// resuming a stale partial from a previous release (asset filenames are
/// version-less, so the artifact path alone cannot disambiguate).
#[derive(Serialize, Deserialize)]
struct ResumeState {
    version: String,
    total_bytes: i64,
    starts: Vec<i64>,
    ends: Vec<i64>,
    done: Vec<i64>,
}

/// `<artifact>.resume.json` next to the download artifact.
fn resume_path(file_path: &Path) -> PathBuf {
    let mut name = file_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".resume.json");
    file_path.with_file_name(name)
}

/// Load and validate resume state. Returns the per-segment completed byte
/// counts only when the artifact is already pre-allocated to `total_bytes`
/// and the sidecar matches version, size, and the exact segment layout.
async fn load_resume(
    file_path: &Path,
    version: &str,
    total_bytes: i64,
    ranges: &[SegmentRange],
) -> Option<Vec<i64>> {
    let meta = tokio::fs::metadata(file_path).await.ok()?;
    if meta.len() as i64 != total_bytes {
        return None;
    }
    let bytes = tokio::fs::read(resume_path(file_path)).await.ok()?;
    let state: ResumeState = serde_json::from_slice(&bytes).ok()?;
    if state.version != version
        || state.total_bytes != total_bytes
        || state.starts.len() != ranges.len()
        || state.ends.len() != ranges.len()
        || state.done.len() != ranges.len()
    {
        return None;
    }
    for (i, r) in ranges.iter().enumerate() {
        let seg_len = r.end - r.start + 1;
        if state.starts[i] != r.start
            || state.ends[i] != r.end
            || state.done[i] < 0
            || state.done[i] > seg_len
        {
            return None;
        }
    }
    Some(state.done)
}

/// Best-effort persist of resume state; failures are ignored (worst case the
/// next attempt starts fresh).
async fn save_resume(
    file_path: &Path,
    version: &str,
    total_bytes: i64,
    ranges: &[SegmentRange],
    done: &[i64],
) {
    let state = ResumeState {
        version: version.to_string(),
        total_bytes,
        starts: ranges.iter().map(|r| r.start).collect(),
        ends: ranges.iter().map(|r| r.end).collect(),
        done: done.to_vec(),
    };
    if let Ok(bytes) = serde_json::to_vec(&state) {
        let _ = tokio::fs::write(resume_path(file_path), bytes).await;
    }
}

// ---------------------------------------------------------------------------
// Multi-segment concurrent download
// ---------------------------------------------------------------------------

/// Per-segment byte range [start, end] (inclusive).
struct SegmentRange {
    start: i64,
    end: i64,
}

async fn download_multi_segment(
    url: &str,
    version: &str,
    file_path: &PathBuf,
    total_bytes: i64,
    client: &Client,
) -> Result<(), UpdateError> {
    let seg_count = UPDATE_SEGMENTS as i64;

    // Compute byte ranges for each segment
    let seg_size = total_bytes / seg_count;
    let mut ranges: Vec<SegmentRange> = Vec::with_capacity(seg_count as usize);
    for i in 0..seg_count {
        let start = i * seg_size;
        let end = if i == seg_count - 1 {
            total_bytes - 1
        } else {
            (i + 1) * seg_size - 1
        };
        ranges.push(SegmentRange { start, end });
    }
    let ranges = Arc::new(ranges);

    // Resume: a matching sidecar + full-size artifact from a previous failed
    // attempt lets every segment continue where it stopped.
    let resumed = load_resume(file_path, version, total_bytes, &ranges).await;
    if resumed.is_none() {
        // Fresh start: pre-allocate the output file to the full size.
        let file = tokio::fs::File::create(file_path).await?;
        file.set_len(total_bytes as u64).await?;
    }
    let initial: Vec<i64> = resumed.unwrap_or_else(|| vec![0; seg_count as usize]);
    let initial_total: i64 = initial.iter().sum();
    if initial_total > 0 {
        log_info!(
            "[updater] resuming update download: {}/{} bytes already on disk",
            initial_total,
            total_bytes
        );
    }

    // Shared progress counters — each segment atomically advances its own
    // counter (absolute completed bytes within the segment) so the reporter
    // task can sum them lock-free and persist resume state.
    let segment_progress: Arc<Vec<AtomicI64>> =
        Arc::new(initial.iter().map(|v| AtomicI64::new(*v)).collect());
    let active_count = Arc::new(AtomicI64::new(seg_count));

    // Spawn a progress reporter task (also persists resume state ~1×/s)
    let ver = version.to_string();
    let prog = Arc::clone(&segment_progress);
    let active = Arc::clone(&active_count);
    let reporter_ranges = Arc::clone(&ranges);
    let reporter_path = file_path.clone();
    let reporter = tokio::spawn(async move {
        let report_interval = Duration::from_millis(200);
        let mut last_total: i64 = initial_total;
        let mut last_time = std::time::Instant::now();
        let mut ticks: u32 = 0;

        loop {
            tokio::time::sleep(report_interval).await;
            ticks += 1;

            let downloaded: i64 = prog.iter().map(|a| a.load(Ordering::Relaxed)).sum();
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(last_time).as_secs_f64();
            let speed = if elapsed > 0.0 {
                ((downloaded - last_total) as f64 / elapsed) as i64
            } else {
                0
            };
            last_total = downloaded;
            last_time = now;

            let cur_active = active.load(Ordering::Relaxed) as i32;

            UpdateDownloadProgress {
                version: ver.clone(),
                downloaded_bytes: downloaded,
                total_bytes,
                speed,
                status: 0,
                installer_path: String::new(),
                error_message: String::new(),
                segments: UPDATE_SEGMENTS,
                active_segments: cur_active,
            }
            .send_signal_to_dart();

            // All bytes received — stop reporting
            if downloaded >= total_bytes {
                break;
            }

            if ticks.is_multiple_of(5) {
                let done: Vec<i64> = prog.iter().map(|a| a.load(Ordering::Relaxed)).collect();
                save_resume(&reporter_path, &ver, total_bytes, &reporter_ranges, &done).await;
            }
        }
    });

    // Spawn one task per segment
    let mut handles = Vec::with_capacity(seg_count as usize);
    for idx in 0..seg_count as usize {
        let client = client.clone();
        let url = url.to_string();
        let file_path = file_path.clone();
        let seg_prog = Arc::clone(&segment_progress);
        let active_cnt = Arc::clone(&active_count);
        let ranges = Arc::clone(&ranges);

        let handle = tokio::spawn(async move {
            let result =
                download_segment(&client, &url, &file_path, idx, &ranges[idx], &seg_prog).await;
            active_cnt.fetch_sub(1, Ordering::Relaxed);
            result
        });
        handles.push(handle);
    }

    // Await all segment tasks and collect errors
    let mut first_error: Option<UpdateError> = None;
    for handle in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
            Err(join_err) => {
                if first_error.is_none() {
                    first_error = Some(UpdateError::Other(format!(
                        "segment task panicked: {join_err}"
                    )));
                }
            }
        }
    }

    // Stop the reporter
    reporter.abort();
    let _ = reporter.await;

    if let Some(e) = first_error {
        // Keep the partial artifact and persist final progress so the next
        // attempt resumes instead of restarting from zero.
        let done: Vec<i64> = segment_progress
            .iter()
            .map(|a| a.load(Ordering::Relaxed))
            .collect();
        save_resume(file_path, version, total_bytes, &ranges, &done).await;
        return Err(e);
    }

    let _ = tokio::fs::remove_file(resume_path(file_path)).await;
    Ok(())
}

/// Download a single byte-range segment with retry. Each retry (and each
/// fresh `download` invocation, via the resume sidecar) continues from the
/// bytes already flushed to disk instead of re-downloading the whole range.
async fn download_segment(
    client: &Client,
    url: &str,
    file_path: &PathBuf,
    idx: usize,
    range: &SegmentRange,
    progress: &Arc<Vec<AtomicI64>>,
) -> Result<(), UpdateError> {
    let seg_len = range.end - range.start + 1;
    let mut attempt: u32 = 0;

    loop {
        let done = progress[idx].load(Ordering::Relaxed).clamp(0, seg_len);
        if done >= seg_len {
            return Ok(());
        }
        match download_segment_attempt(client, url, file_path, idx, range, done, progress).await {
            Ok(()) => {
                log_info!(
                    "[updater] segment {} finished: {}-{}",
                    idx,
                    range.start,
                    range.end
                );
                return Ok(());
            }
            Err(e) => {
                attempt += 1;
                if attempt > SEGMENT_RETRIES {
                    return Err(e);
                }
                log_info!(
                    "[updater] segment {} attempt {} failed: {}; retrying",
                    idx,
                    attempt,
                    e
                );
                tokio::time::sleep(Duration::from_secs(2u64 << (attempt - 1))).await;
            }
        }
    }
}

/// One attempt at a segment: requests only the missing tail of the range and
/// writes at the correct offset. `done` = bytes already valid on disk.
///
/// The progress counter (which feeds the resume sidecar) only advances after
/// `flush`, so an abrupt failure can never record bytes still sitting in
/// tokio's write buffer.
async fn download_segment_attempt(
    client: &Client,
    url: &str,
    file_path: &PathBuf,
    idx: usize,
    range: &SegmentRange,
    done: i64,
    progress: &Arc<Vec<AtomicI64>>,
) -> Result<(), UpdateError> {
    let start = range.start + done;
    let range_header = format!("bytes={}-{}", start, range.end);

    let resp = client
        .get(url)
        .header("Range", &range_header)
        .send()
        .await?;

    // Accept only 206 Partial Content. A 200 OK would be the whole file —
    // writing it at this offset would corrupt the artifact.
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(UpdateError::Other(format!(
            "segment {idx} expected 206 Partial Content, got {}",
            resp.status()
        )));
    }

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(file_path)
        .await?;
    file.seek(std::io::SeekFrom::Start(start as u64)).await?;

    let expected = range.end - start + 1;
    let mut stream = resp.bytes_stream();
    let mut written: i64 = 0;
    let mut unflushed: i64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(UpdateError::Http)?;
        if written + chunk.len() as i64 > expected {
            return Err(UpdateError::Other(format!(
                "segment {idx} server sent more bytes than requested"
            )));
        }
        file.write_all(&chunk).await?;
        written += chunk.len() as i64;
        unflushed += chunk.len() as i64;
        if unflushed >= FLUSH_INTERVAL {
            file.flush().await?;
            unflushed = 0;
            progress[idx].store(done + written, Ordering::Relaxed);
        }
    }

    file.flush().await?;

    if written < expected {
        // Short read: keep flushed progress, report as retryable error.
        progress[idx].store(done + written, Ordering::Relaxed);
        return Err(UpdateError::Other(format!(
            "segment {idx} truncated: got {written} of {expected} bytes"
        )));
    }

    progress[idx].store(done + written, Ordering::Relaxed);
    log_info!(
        "[updater] segment {} wrote {} bytes at offset {}",
        idx,
        written,
        start
    );
    Ok(())
}

/// Name of the marker file the updater helper writes when an automatic
/// portable update fails to overwrite the program files (e.g. the install
/// directory is read-only or a file was locked). Kept in sync with the
/// constant of the same purpose in `fluxdown_updater`.
const FAILURE_MARKER_NAME: &str = "update_failed.marker";

/// Check for a leftover "update failed" marker written by the helper binary on
/// a previous (failed) update attempt, returning the human-readable message if
/// present. The marker is consumed (deleted) so the warning is shown only once.
///
/// The helper writes the marker to the install directory when possible, falling
/// back to the OS temp directory when the install dir is not writable — so we
/// look in both places, preferring the install directory.
pub fn check_failure_marker() -> Option<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join(FAILURE_MARKER_NAME));
    }
    candidates.push(std::env::temp_dir().join(FAILURE_MARKER_NAME));

    for path in candidates {
        if let Ok(content) = std::fs::read_to_string(&path) {
            // Best-effort delete so the warning is shown only once.
            let _ = std::fs::remove_file(&path);
            // First line is a machine reason tag; the rest is the message body.
            let message = content
                .split_once('\n')
                .map(|(_, rest)| rest)
                .unwrap_or(content.as_str())
                .trim()
                .to_string();
            if message.is_empty() {
                return Some("The previous automatic update failed.".to_string());
            }
            return Some(message);
        }
    }
    None
}

/// Verify that the application's install directory is writable before starting
/// an update. Returns an error describing the problem otherwise.
///
/// This catches the common portable-build failure mode up-front (folder placed
/// under "Program Files", a read-only volume, etc.) so the UI can warn the user
/// *before* the app exits — instead of the helper failing silently after exit.
///
/// Only meaningful for portable installs that overwrite files in place
/// (Windows ZIP, Linux tar.gz). For installer-based updates the OS installer
/// handles permissions/elevation, so the check is skipped.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn ensure_install_dir_writable() -> Result<(), UpdateError> {
    let exe = std::env::current_exe().map_err(UpdateError::Io)?;
    let dir = exe
        .parent()
        .ok_or_else(|| UpdateError::Other("cannot determine app directory".to_string()))?;

    let probe = dir.join(format!(".fluxdown_write_test_{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(UpdateError::Other(format!(
            "The install folder is not writable, so the update cannot replace \
             the program files automatically.\n\nFolder: {}\n\nThis usually \
             happens when FluxDown is in a protected location such as \
             \"Program Files\" or on a read-only drive. Move FluxDown to a \
             normal folder (e.g. your user directory) and try again, or download \
             the latest version from https://fluxdown.zerx.dev\n\n({e})",
            dir.display()
        ))),
    }
}

/// Install a downloaded update package and restart the application.
///
/// On success the function does not return — it exits the process.
/// On failure it returns an error so the caller can report it to the UI.
pub fn install(installer_path: &str) -> Result<(), UpdateError> {
    #[cfg(target_os = "windows")]
    {
        let path = Path::new(installer_path);
        let is_zip = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));

        if is_zip {
            // Portable: we overwrite files in place — verify writability first
            // so we can surface a clear error in the UI before the app exits.
            ensure_install_dir_writable()?;
            install_portable(installer_path)
        } else {
            install_setup(installer_path)
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Dispatch based on file extension / suffix.
        if installer_path.ends_with(".AppImage") {
            install_appimage(installer_path)
        } else if installer_path.ends_with(".deb") {
            install_deb(installer_path)
        } else if installer_path.ends_with(".pkg.tar.zst") {
            install_arch(installer_path)
        } else {
            // .tar.gz portable fallback — overwrites files in place.
            ensure_install_dir_writable()?;
            install_portable_tarball(installer_path)
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Dispatch by extension: DMG → hand to Finder; tar.gz → legacy helper.
        if installer_path.to_ascii_lowercase().ends_with(".dmg") {
            install_macos_dmg(installer_path)
        } else {
            install_macos_app(installer_path)
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = installer_path;
        Err(UpdateError::Other(
            "Auto-update install is not supported on this platform".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Windows ZIP bootstrap (uses `zip` crate, Windows-only dependency)
// ---------------------------------------------------------------------------
// See `bootstrap_updater_from_zip` and `find_or_bootstrap_updater` below,
// defined together with the rest of the helper-location logic.

// ---------------------------------------------------------------------------
// macOS installer
// ---------------------------------------------------------------------------

/// macOS DMG update: clear quarantine, hand the DMG to Finder, return.
///
/// The DMG already lives in `~/Downloads` (see `pick_download_dir`).  We do NOT
/// exit the running app — the user must drag the new .app to /Applications,
/// which requires the source DMG window to remain open.  Quitting the app is
/// the user's responsibility (the standard first-install flow).
///
/// We strip the Gatekeeper quarantine xattr first so unsigned/unnotarized
/// builds at least skip the "downloaded from the internet" warning when the
/// user double-clicks the mounted .app.  This is best-effort — if the
/// `xattr` binary is missing, mounting still works.
#[cfg(target_os = "macos")]
fn install_macos_dmg(dmg_path: &str) -> Result<(), UpdateError> {
    // Best-effort quarantine removal on the DMG itself.
    let _ = std::process::Command::new("xattr")
        .args(["-dr", "com.apple.quarantine", dmg_path])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // `open` returns immediately; Finder mounts the DMG and shows its window
    // (which typically contains FluxDown.app + an Applications symlink).
    let status = std::process::Command::new("open")
        .arg(dmg_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(UpdateError::Io)?;

    if !status.success() {
        return Err(UpdateError::Other(format!(
            "`open {dmg_path}` exited with status {status}"
        )));
    }

    log_info!("[updater] mounted DMG via `open`: {}", dmg_path);
    Ok(())
}

/// macOS update: spawn `fluxdown_updater` and exit immediately.
///
/// The updater polls `kill(pid, 0)` until this process exits, then:
///   1. Extracts the tar.gz to a temp directory.
///   2. Removes the Gatekeeper quarantine attribute from the new .app bundle.
///   3. Replaces the existing .app (atomic `rename` when possible, `cp -a` fallback).
///   4. Relaunches the updated bundle via `open`.
///
/// The updater binary lives at `FluxDown.app/Contents/MacOS/fluxdown_updater`,
/// which is the same directory as the main executable — `find_updater_bin()`
/// finds it automatically via `current_exe().parent()`.
///
/// Reachable only when a release ships without a DMG and we fall back to
/// tar.gz. This path requires the app bundle to be signed/notarized to avoid
/// Gatekeeper killing the helper after replacement.
#[cfg(target_os = "macos")]
fn install_macos_app(tarball_path: &str) -> Result<(), UpdateError> {
    let exe = std::env::current_exe().map_err(UpdateError::Io)?;

    // FluxDown.app/Contents/MacOS/flux_down
    //              ↑ parent  ↑ parent  ↑ parent  →  FluxDown.app
    let app_bundle = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or_else(|| UpdateError::Other("cannot locate .app bundle".to_string()))?;

    // Parent of FluxDown.app  →  /Applications (or wherever the user placed it)
    let install_dir = app_bundle
        .parent()
        .ok_or_else(|| UpdateError::Other("cannot locate install directory".to_string()))?;

    let app_name = app_bundle
        .file_name()
        .ok_or_else(|| UpdateError::Other("cannot determine .app bundle name".to_string()))?
        .to_string_lossy();

    let updater = find_updater_bin()?;
    let pid = std::process::id();

    std::process::Command::new(&updater)
        .args([
            "--pid",
            &pid.to_string(),
            "--tarball",
            tarball_path,
            "--app-bundle",
            &app_name,
            "--install-dir",
            &install_dir.to_string_lossy(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(UpdateError::Io)?;

    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// Updater helper binary location & cold-start bootstrap
// ---------------------------------------------------------------------------

/// Locate the `fluxdown_updater[.exe]` helper binary that is shipped alongside
/// the main application in the same directory as the running executable.
///
/// Returns `Err` when the binary is absent (e.g. the user is upgrading from a
/// version that pre-dates the helper).  Callers should fall back to
/// `bootstrap_updater_from_zip` in that case.
#[cfg(not(target_os = "android"))]
fn find_updater_bin() -> Result<PathBuf, UpdateError> {
    let exe = std::env::current_exe().map_err(UpdateError::Io)?;
    let dir = exe
        .parent()
        .ok_or_else(|| UpdateError::Other("cannot determine app directory".to_string()))?;

    #[cfg(target_os = "windows")]
    let name = "fluxdown_updater.exe";
    #[cfg(not(target_os = "windows"))]
    let name = "fluxdown_updater";

    let updater = dir.join(name);
    if updater.exists() {
        Ok(updater)
    } else {
        Err(UpdateError::Other(format!(
            "updater helper not found: {}",
            updater.display()
        )))
    }
}

/// Bootstrap the updater helper for users upgrading from a version that did
/// not ship `fluxdown_updater[.exe]`.
///
/// Scans the already-downloaded update ZIP for the helper binary, extracts it
/// to a private temp file (named with the current PID to avoid collisions),
/// and returns that path.  The next full update will write the helper into the
/// install directory, so this bootstrap path is only needed once.
#[cfg(target_os = "windows")]
fn bootstrap_updater_from_zip(zip_path: &str) -> Result<PathBuf, UpdateError> {
    use std::io;

    const HELPER_NAME: &str = "fluxdown_updater.exe";

    let file = std::fs::File::open(zip_path).map_err(UpdateError::Io)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| UpdateError::Other(e.to_string()))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| UpdateError::Other(e.to_string()))?;

        // Match the bare filename regardless of directory nesting inside the ZIP.
        let entry_name = entry.name().to_string();
        let file_name = entry_name.rsplit('/').next().unwrap_or(entry_name.as_str());

        if file_name.eq_ignore_ascii_case(HELPER_NAME) {
            let dest = std::env::temp_dir()
                .join(format!("fluxdown_updater_boot_{}.exe", std::process::id()));
            let mut out = std::fs::File::create(&dest).map_err(UpdateError::Io)?;
            io::copy(&mut entry, &mut out).map_err(UpdateError::Io)?;
            return Ok(dest);
        }
    }

    Err(UpdateError::Other(format!(
        "{HELPER_NAME} was not found inside the downloaded archive. \
         The package may be from an older release. \
         Please download and extract the new version manually from https://fluxdown.zerx.dev"
    )))
}

/// Resolve the updater binary path, bootstrapping from the ZIP on first run
/// after migration from a version that did not ship the helper.
#[cfg(target_os = "windows")]
fn find_or_bootstrap_updater(zip_path: &str) -> Result<PathBuf, UpdateError> {
    find_updater_bin().or_else(|_| bootstrap_updater_from_zip(zip_path))
}

/// On Linux/macOS the helper is always expected to be present. Android never
/// reaches here — APK installation is driven from the Dart/Kotlin side.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn find_or_bootstrap_updater(_zip_path: &str) -> Result<PathBuf, UpdateError> {
    find_updater_bin()
}

// ---------------------------------------------------------------------------
// Windows installers
// ---------------------------------------------------------------------------

/// Spawn a detached process, working around Windows error 740
/// (`ERROR_ELEVATION_REQUIRED`, "请求的操作需要提升").
///
/// `CreateProcess` fails with 740 when the target executable is marked
/// "Run as administrator" — set manually via the file-properties
/// compatibility checkbox, or automatically by the Program Compatibility
/// Assistant (which is prone to flagging unsigned binaries whose name
/// contains "updater"). None of the binaries we launch actually need admin
/// rights (portable updates write to the user-owned install dir; the Inno
/// installer is built with `PrivilegesRequired=lowest`), so on 740 we retry
/// once with the `RunAsInvoker` compatibility layer, which overrides both
/// the HKCU and HKLM compat flags without requiring elevation.
#[cfg(target_os = "windows")]
fn spawn_no_elevation(program: &Path, args: &[&str]) -> Result<(), UpdateError> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const ERROR_ELEVATION_REQUIRED: i32 = 740;

    match std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
            log_info!(
                "[updater] spawn hit ERROR_ELEVATION_REQUIRED (740); retrying with RunAsInvoker: {}",
                program.display()
            );
            std::process::Command::new(program)
                .args(args)
                .env("__COMPAT_LAYER", "RunAsInvoker")
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map(drop)
                .map_err(UpdateError::Io)
        }
        Err(e) => Err(UpdateError::Io(e)),
    }
}

/// Windows setup update: spawn `fluxdown_updater` and exit immediately.
///
/// The updater waits for this process to exit via `WaitForSingleObject`, then
/// removes the Mark-of-the-Web `Zone.Identifier` alternate data stream from
/// the downloaded installer (equivalent to "Unblock" in Explorer) so that
/// Windows 11 Smart App Control does not block it, and finally runs the NSIS
/// installer silently.  Because the updater binary is already installed and
/// trusted, the unblock operation succeeds even with SAC enabled.
#[cfg(target_os = "windows")]
fn install_setup(installer_path: &str) -> Result<(), UpdateError> {
    let updater = find_updater_bin()?;
    let pid = std::process::id();

    spawn_no_elevation(
        &updater,
        &["--pid", &pid.to_string(), "--installer", installer_path],
    )?;

    std::process::exit(0);
}

/// Windows portable update: spawn `fluxdown_updater` and exit immediately.
///
/// The updater uses `WaitForSingleObject` for precise OS-level process-exit
/// detection (no polling, no tasklist), then:
///   1. Renames itself aside so the ZIP can overwrite `fluxdown_updater.exe`.
///   2. Extracts the ZIP to a private temp directory.
///   3. Copies files into the app directory with exponential-backoff retry
///      for files transiently locked by antivirus scanners.
///   4. Deletes the stale self-copy and the downloaded ZIP.
///   5. Restarts the application.
///
/// No PowerShell, cmd.exe, or script interpreters are involved.
///
/// Cold-start migration: if `fluxdown_updater.exe` is absent (upgrade from an
/// older version), the helper is bootstrapped directly from the downloaded ZIP
/// and run from the OS temp directory for this one cycle.
#[cfg(target_os = "windows")]
fn install_portable(zip_path: &str) -> Result<(), UpdateError> {
    let exe = std::env::current_exe().map_err(UpdateError::Io)?;
    let app_dir = exe
        .parent()
        .ok_or_else(|| UpdateError::Other("cannot determine app directory".to_string()))?;
    let exe_name = exe
        .file_name()
        .ok_or_else(|| UpdateError::Other("cannot determine exe name".to_string()))?
        .to_string_lossy();

    // Fall back to bootstrapping the helper from the ZIP when upgrading from
    // an older version that did not ship fluxdown_updater.exe.
    let updater = find_or_bootstrap_updater(zip_path)?;
    let pid = std::process::id();

    spawn_no_elevation(
        &updater,
        &[
            "--pid",
            &pid.to_string(),
            "--zip",
            zip_path,
            "--dir",
            &app_dir.to_string_lossy(),
            "--exe",
            &exe_name,
        ],
    )?;

    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// Linux installers
// ---------------------------------------------------------------------------

/// Linux AppImage update: spawn `fluxdown_updater` and exit immediately.
///
/// The updater polls `/proc/<pid>` until this process exits, then atomically
/// replaces the running AppImage file with the new one (`mv -f`), sets the
/// executable bit, and restarts the application.  No root required.
#[cfg(target_os = "linux")]
fn install_appimage(new_appimage_path: &str) -> Result<(), UpdateError> {
    let current_appimage = std::env::var("APPIMAGE").map_err(|_| {
        UpdateError::Other(
            "$APPIMAGE not set; cannot determine the current AppImage path".to_string(),
        )
    })?;

    let updater = find_updater_bin()?;
    let pid = std::process::id();

    std::process::Command::new(&updater)
        .args([
            "--pid",
            &pid.to_string(),
            "--appimage-src",
            new_appimage_path,
            "--appimage-dst",
            &current_appimage,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(UpdateError::Io)?;

    std::process::exit(0);
}

/// Linux deb update: spawn `fluxdown_updater` and exit immediately.
///
/// The updater polls `/proc/<pid>`, then runs `pkexec dpkg -i` (which shows
/// the distro's native password dialog), and restarts the application.
#[cfg(target_os = "linux")]
fn install_deb(deb_path: &str) -> Result<(), UpdateError> {
    let updater = find_updater_bin()?;
    let pid = std::process::id();

    std::process::Command::new(&updater)
        .args(["--pid", &pid.to_string(), "--package-deb", deb_path])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(UpdateError::Io)?;

    std::process::exit(0);
}

/// Linux Arch update: spawn `fluxdown_updater` and exit immediately.
///
/// The updater polls `/proc/<pid>`, then runs `pkexec pacman -U` (which shows
/// the distro's native password dialog), and restarts the application.
#[cfg(target_os = "linux")]
fn install_arch(pkg_path: &str) -> Result<(), UpdateError> {
    let updater = find_updater_bin()?;
    let pid = std::process::id();

    std::process::Command::new(&updater)
        .args(["--pid", &pid.to_string(), "--package-arch", pkg_path])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(UpdateError::Io)?;

    std::process::exit(0);
}

/// Linux portable tar.gz update: spawn `fluxdown_updater` and exit immediately.
///
/// The updater polls `/proc/<pid>`, extracts the tarball to a temp directory,
/// unwraps single-folder archives, copies files into the app directory with
/// retry, cleans up, and restarts the application.  No root required.
#[cfg(target_os = "linux")]
fn install_portable_tarball(tarball_path: &str) -> Result<(), UpdateError> {
    let exe = std::env::current_exe().map_err(UpdateError::Io)?;
    let app_dir = exe
        .parent()
        .ok_or_else(|| UpdateError::Other("cannot determine app directory".to_string()))?;
    let exe_name = exe
        .file_name()
        .ok_or_else(|| UpdateError::Other("cannot determine exe name".to_string()))?
        .to_string_lossy()
        .into_owned();

    let updater = find_updater_bin()?;
    let pid = std::process::id();

    std::process::Command::new(&updater)
        .args([
            "--pid",
            &pid.to_string(),
            "--tarball",
            tarball_path,
            "--dir",
            &app_dir.to_string_lossy(),
            "--exe",
            &exe_name,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(UpdateError::Io)?;

    std::process::exit(0);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{SegmentRange, is_newer, load_resume, resume_path, sanitize_filename, save_resume};

    /// Stable channel: standard three-part precedence, no prerelease involved.
    #[test]
    fn is_newer_stable() {
        assert!(is_newer("1.3.0", "1.2.5").unwrap());
        assert!(is_newer("1.2.6", "1.2.5").unwrap());
        assert!(!is_newer("1.2.5", "1.2.5").unwrap());
        assert!(!is_newer("1.2.4", "1.2.5").unwrap());
        // Leading `v` on either side is tolerated.
        assert!(is_newer("v2.0.0", "1.9.9").unwrap());
    }

    /// Frontier channel prerelease precedence (SemVer 2.0 §11).
    #[test]
    fn is_newer_prerelease() {
        // A stable release outranks its own prereleases.
        assert!(is_newer("1.3.0", "1.3.0-rc.1").unwrap());
        assert!(is_newer("1.3.0", "1.3.0-rc.2").unwrap());
        // Later prerelease outranks earlier one (numeric identifier).
        assert!(is_newer("1.3.0-rc.2", "1.3.0-rc.1").unwrap());
        // A prerelease never outranks the matching stable.
        assert!(!is_newer("1.3.0-rc.1", "1.3.0").unwrap());
        assert!(!is_newer("1.3.0-rc.1", "1.3.0-rc.2").unwrap());
        // A prerelease of a higher core outranks a lower stable (frontier user
        // on 1.3.0 gets offered the next line's first RC).
        assert!(is_newer("1.4.0-rc.1", "1.3.0").unwrap());
        // Numeric identifiers rank below alphanumeric ones.
        assert!(is_newer("1.3.0-rc", "1.3.0-1").unwrap());
    }

    fn ranges() -> Vec<SegmentRange> {
        vec![
            SegmentRange { start: 0, end: 499 },
            SegmentRange {
                start: 500,
                end: 999,
            },
        ]
    }

    /// `sanitize_filename` must strip URL query strings so downloaded update
    /// artifacts never carry `?` (invalid on Windows → OS error 123). The
    /// desktop updater always appends `?source=github`, so the no-fragment
    /// query case is the real-world path (regression: issue #86).
    #[test]
    fn sanitize_strips_query_and_fragment() {
        assert_eq!(
            sanitize_filename("FluxDown-0.2.0-windows-x64-setup.exe?source=github"),
            "FluxDown-0.2.0-windows-x64-setup.exe"
        );
        assert_eq!(sanitize_filename("pkg.zip#frag"), "pkg.zip");
        assert_eq!(sanitize_filename("pkg.zip?a=1#frag"), "pkg.zip");
        // Plain name (no query/fragment) is unchanged.
        assert_eq!(sanitize_filename("plain.exe"), "plain.exe");
        // Path components are stripped.
        assert_eq!(sanitize_filename("a/b/c.exe?x=1"), "c.exe");
        // Empty / degenerate input falls back to the safe default.
        assert_eq!(sanitize_filename("?only=query"), "FluxDown-update");
    }

    /// Roundtrip: saved progress is loaded back verbatim when the artifact
    /// exists at full size and version/layout match.
    #[tokio::test]
    async fn resume_roundtrip() {
        let dir = std::env::temp_dir().join(format!("fd_resume_rt_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let artifact = dir.join("pkg.zip");
        tokio::fs::write(&artifact, vec![0u8; 1000]).await.unwrap();

        let r = ranges();
        save_resume(&artifact, "1.2.3", 1000, &r, &[100, 250]).await;
        assert_eq!(
            load_resume(&artifact, "1.2.3", 1000, &r).await,
            Some(vec![100, 250])
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Stale state must be rejected: version mismatch (new release, same
    /// filename), artifact size mismatch, and segment-layout mismatch.
    #[tokio::test]
    async fn resume_rejects_stale_state() {
        let dir = std::env::temp_dir().join(format!("fd_resume_stale_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let artifact = dir.join("pkg.zip");
        tokio::fs::write(&artifact, vec![0u8; 1000]).await.unwrap();

        let r = ranges();
        save_resume(&artifact, "1.2.3", 1000, &r, &[100, 250]).await;

        // Different version → fresh start.
        assert_eq!(load_resume(&artifact, "1.2.4", 1000, &r).await, None);
        // Different total size → fresh start.
        assert_eq!(load_resume(&artifact, "1.2.3", 2000, &r).await, None);
        // Different segment layout → fresh start.
        let other = vec![SegmentRange { start: 0, end: 999 }];
        assert_eq!(load_resume(&artifact, "1.2.3", 1000, &other).await, None);
        // Artifact truncated on disk → fresh start.
        tokio::fs::write(&artifact, vec![0u8; 400]).await.unwrap();
        assert_eq!(load_resume(&artifact, "1.2.3", 1000, &r).await, None);
        // Missing sidecar → fresh start.
        tokio::fs::write(&artifact, vec![0u8; 1000]).await.unwrap();
        tokio::fs::remove_file(resume_path(&artifact))
            .await
            .unwrap();
        assert_eq!(load_resume(&artifact, "1.2.3", 1000, &r).await, None);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Done counts outside the segment range invalidate the sidecar.
    #[tokio::test]
    async fn resume_rejects_out_of_range_done() {
        let dir = std::env::temp_dir().join(format!("fd_resume_oor_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let artifact = dir.join("pkg.zip");
        tokio::fs::write(&artifact, vec![0u8; 1000]).await.unwrap();

        let r = ranges();
        // 600 > segment length (500) → invalid.
        save_resume(&artifact, "1.2.3", 1000, &r, &[600, 0]).await;
        assert_eq!(load_resume(&artifact, "1.2.3", 1000, &r).await, None);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
