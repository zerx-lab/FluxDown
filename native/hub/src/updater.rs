//! Auto-update module: version check via website API proxy, download installer,
//! and launch Inno Setup silent install.
//!
//! All requests go through the website API (`/api/release`, `/api/download/:fn`)
//! so that GITHUB_TOKEN stays server-side — the client never touches GitHub directly.

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use rinf::RustSignal;
use serde::Deserialize;
use thiserror::Error;
use tokio::io::AsyncWriteExt;

use crate::signals::{UpdateCheckResult, UpdateDownloadProgress};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Base URL of the website API that proxies GitHub Release info.
/// Change this to your actual deployment domain.
const UPDATE_API_BASE: &str = "https://fluxdown.zerx.dev";

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
    setup: Option<AssetInfo>,
    #[allow(dead_code)]
    portable: Option<AssetInfo>,
}

#[derive(Deserialize)]
struct AssetInfo {
    #[allow(dead_code)]
    name: String,
    size: i64,
    download_url: String,
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

    let (download_url, file_size) = match &release.assets.setup {
        Some(asset) => {
            // download_url from API is relative like "/api/download/FluxDown-xxx.exe"
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
pub async fn download(url: &str, version: &str) {
    let result = download_inner(url, version).await;
    if let Err(e) = result {
        UpdateDownloadProgress {
            version: version.to_string(),
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 0,
            status: 2, // error
            installer_path: String::new(),
            error_message: e.to_string(),
        }
        .send_signal_to_dart();
    }
}

async fn download_inner(url: &str, version: &str) -> Result<(), UpdateError> {
    let client = Client::new();

    let resp = client
        .get(url)
        .timeout(Duration::from_secs(600)) // 10 min max for large installer
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(UpdateError::Other(format!(
            "Download returned status {}",
            resp.status()
        )));
    }

    let total_bytes = resp.content_length().unwrap_or(0) as i64;
    let file_name = format!("FluxDown-{version}-windows-setup.exe");
    let temp_dir = std::env::temp_dir();
    let file_path = temp_dir.join(&file_name);

    let mut file = tokio::fs::File::create(&file_path).await?;
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
                status: 0, // downloading
                installer_path: String::new(),
                error_message: String::new(),
            }
            .send_signal_to_dart();

            last_report = now;
        }
    }

    file.flush().await?;
    drop(file);

    let installer_path = file_path.to_string_lossy().to_string();

    // Send completion signal
    UpdateDownloadProgress {
        version: version.to_string(),
        downloaded_bytes: downloaded,
        total_bytes,
        speed: 0,
        status: 1, // completed
        installer_path,
        error_message: String::new(),
    }
    .send_signal_to_dart();

    Ok(())
}

/// Launch the Inno Setup installer with silent flags and exit the current process.
pub fn install(installer_path: &str) -> Result<(), UpdateError> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW = 0x08000000
        std::process::Command::new(installer_path)
            .args(["/SILENT", "/CLOSEAPPLICATIONS", "/RESTARTAPPLICATIONS"])
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| UpdateError::Io(e))?;

        // Give installer a moment to start, then exit
        std::thread::sleep(Duration::from_millis(500));
        std::process::exit(0);
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = installer_path;
        Err(UpdateError::Other(
            "Auto-update install is only supported on Windows".to_string(),
        ))
    }
}
