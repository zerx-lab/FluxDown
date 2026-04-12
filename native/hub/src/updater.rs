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
//! All HTTP requests go through the website API (`/api/release`, `/api/download/:fn`)
//! so that GITHUB_TOKEN stays server-side — the client never touches GitHub directly.

#[cfg(target_os = "windows")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use rinf::RustSignal;
use serde::Deserialize;
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

#[derive(Deserialize)]
struct ReleaseInfo {
    version: String,
    published_at: String,
    assets: ReleaseAssets,
}

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
    #[allow(dead_code)]
    macos_tarball: Option<AssetInfo>,
    #[allow(dead_code)]
    macos_arm64_tarball: Option<AssetInfo>,
}

#[derive(Deserialize)]
struct AssetInfo {
    #[allow(dead_code)]
    name: String,
    size: i64,
    download_url: String,
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
        // Always use the tar.gz distribution for programmatic in-app updates.
        // DMG is for first-time manual installs only.
        if is_arm64() {
            assets.macos_arm64_tarball.as_ref()
        } else {
            assets.macos_tarball.as_ref()
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Simple semver comparison (major.minor.patch only)
// ---------------------------------------------------------------------------

fn parse_semver(s: &str) -> Result<(u64, u64, u64), UpdateError> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let parts: Vec<&str> = s.split('.').collect();
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
    Ok((major, minor, patch))
}

fn is_newer(latest: &str, current: &str) -> Result<bool, UpdateError> {
    let (lmaj, lmin, lpat) = parse_semver(latest)?;
    let (cmaj, cmin, cpat) = parse_semver(current)?;
    Ok((lmaj, lmin, lpat) > (cmaj, cmin, cpat))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check for updates by querying the website API proxy.
/// Sends `UpdateCheckResult` signal back to Dart.
pub async fn check(current_version: &str) {
    let result = check_inner(current_version).await;
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

async fn check_inner(current_version: &str) -> Result<(), UpdateError> {
    let client = Client::new();
    let url = format!("{UPDATE_API_BASE}/api/release");

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

/// Download the update installer to a temp directory.
/// Sends periodic `UpdateDownloadProgress` signals to Dart.
///
/// Uses multi-segment concurrent downloading (like the main download engine)
/// to maximise throughput and showcase the product's core capability.
/// Falls back to single-stream when the server does not support Range requests.
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
    let without_query = raw
        .split_once('?')
        .map(|(base, _)| base)
        .unwrap_or(raw)
        .split_once('#')
        .map(|(base, _)| base)
        .unwrap_or(raw);

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
    let temp_dir = std::env::temp_dir();
    let file_path = temp_dir.join(&file_name);

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
        download_single_stream(url, version, &file_path, total_bytes, &client).await?;
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
) -> Result<(), UpdateError> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(UpdateError::Other(format!(
            "Download returned status {}",
            resp.status()
        )));
    }

    let mut file = tokio::fs::File::create(file_path).await?;
    let mut stream = resp.bytes_stream();

    let mut downloaded: i64 = 0;
    let mut last_report = std::time::Instant::now();
    let mut last_downloaded_for_speed: i64 = 0;
    let mut last_speed_time = std::time::Instant::now();
    let report_interval = Duration::from_millis(200);

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as i64;

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
        }
    }

    file.flush().await?;
    Ok(())
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

    // Pre-allocate the output file to the full size
    {
        let file = tokio::fs::File::create(file_path).await?;
        file.set_len(total_bytes as u64).await?;
    }

    // Shared progress counters — each segment atomically increments its own
    // counter so the reporter task can sum them lock-free.
    let segment_progress: Arc<Vec<AtomicI64>> =
        Arc::new((0..seg_count).map(|_| AtomicI64::new(0)).collect());
    let active_count = Arc::new(AtomicI64::new(seg_count));

    // Spawn a progress reporter task
    let ver = version.to_string();
    let prog = Arc::clone(&segment_progress);
    let active = Arc::clone(&active_count);
    let reporter = tokio::spawn(async move {
        let report_interval = Duration::from_millis(200);
        let mut last_total: i64 = 0;
        let mut last_time = std::time::Instant::now();

        loop {
            tokio::time::sleep(report_interval).await;

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
        }
    });

    // Spawn one task per segment
    let mut handles = Vec::with_capacity(seg_count as usize);
    for (idx, range) in ranges.into_iter().enumerate() {
        let client = client.clone();
        let url = url.to_string();
        let file_path = file_path.clone();
        let seg_prog = Arc::clone(&segment_progress);
        let active_cnt = Arc::clone(&active_count);

        let handle = tokio::spawn(async move {
            let result = download_segment(&client, &url, &file_path, idx, &range, &seg_prog).await;
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
        // Clean up partial file on error
        let _ = tokio::fs::remove_file(file_path).await;
        return Err(e);
    }

    Ok(())
}

/// Download a single byte-range segment, writing directly to the correct
/// offset in the pre-allocated file.
async fn download_segment(
    client: &Client,
    url: &str,
    file_path: &PathBuf,
    idx: usize,
    range: &SegmentRange,
    progress: &Arc<Vec<AtomicI64>>,
) -> Result<(), UpdateError> {
    let range_header = format!("bytes={}-{}", range.start, range.end);

    let resp = client
        .get(url)
        .header("Range", &range_header)
        .send()
        .await?;

    // Accept both 206 Partial Content and 200 OK (some CDNs ignore Range for
    // small files).  For 200 OK we must NOT write — only segment 0 would be
    // valid.  Return an error so the caller can fall back.
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
    file.seek(std::io::SeekFrom::Start(range.start as u64))
        .await?;

    let mut stream = resp.bytes_stream();
    let mut written: i64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(UpdateError::Http)?;
        file.write_all(&chunk).await?;
        written += chunk.len() as i64;
        progress[idx].store(written, Ordering::Relaxed);
    }

    file.flush().await?;

    log_info!(
        "[updater] segment {} finished: {}-{} ({} bytes written)",
        idx,
        range.start,
        range.end,
        written
    );

    Ok(())
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
            // .tar.gz portable fallback
            install_portable_tarball(installer_path)
        }
    }

    #[cfg(target_os = "macos")]
    {
        // macOS always receives a tar.gz containing the new FluxDown.app bundle.
        install_macos_app(installer_path)
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
         Please download and extract the new version manually from https://fluxdown.app"
    )))
}

/// Resolve the updater binary path, bootstrapping from the ZIP on first run
/// after migration from a version that did not ship the helper.
#[cfg(target_os = "windows")]
fn find_or_bootstrap_updater(zip_path: &str) -> Result<PathBuf, UpdateError> {
    find_updater_bin().or_else(|_| bootstrap_updater_from_zip(zip_path))
}

/// On non-Windows platforms the helper is always expected to be present.
#[cfg(not(target_os = "windows"))]
fn find_or_bootstrap_updater(_zip_path: &str) -> Result<PathBuf, UpdateError> {
    find_updater_bin()
}

// ---------------------------------------------------------------------------
// Windows installers
// ---------------------------------------------------------------------------

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
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let updater = find_updater_bin()?;
    let pid = std::process::id();

    std::process::Command::new(&updater)
        .args(["--pid", &pid.to_string(), "--installer", installer_path])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(UpdateError::Io)?;

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
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

    std::process::Command::new(&updater)
        .args([
            "--pid",
            &pid.to_string(),
            "--zip",
            zip_path,
            "--dir",
            &app_dir.to_string_lossy(),
            "--exe",
            &exe_name,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(UpdateError::Io)?;

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
