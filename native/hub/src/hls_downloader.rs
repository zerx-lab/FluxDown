//! HLS (HTTP Live Streaming) download engine.
//!
//! Fetches M3U8 playlists, downloads all segments sequentially, optionally
//! decrypts AES-128-CBC encrypted segments, and merges them into a single
//! `.ts` output file.
//!
//! Architecture:
//! - Master playlist → auto-select highest bandwidth variant
//! - Media playlist → sequential segment download with cancellation
//! - AES-128-CBC decryption with key caching
//! - Progress reporting via ProgressUpdate channel
//! - Per-segment retry with exponential backoff

use std::collections::HashMap;
use std::path::PathBuf;

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;

use rinf::RustSignal;

use crate::downloader::{
    DB_SAVE_INTERVAL_SECS, DownloadError, DownloadParams, ProgressUpdate, TEMP_EXT, dedup_filename,
    extract_from_url,
};
use crate::logger::log_info;
use crate::signals::{HlsQualityOption, HlsQualityOptions};

// ---------------------------------------------------------------------------
// Same-origin check for cookie safety
// ---------------------------------------------------------------------------

fn is_same_origin(base_url: &str, target_url: &str) -> bool {
    let base = match url::Url::parse(base_url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    let target = match url::Url::parse(target_url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    base.scheme() == target.scheme()
        && base.host_str() == target.host_str()
        && base.port_or_known_default() == target.port_or_known_default()
}

fn cookies_for_url<'a>(playlist_url: &str, target_url: &str, cookies: &'a str) -> &'a str {
    if cookies.is_empty() {
        return "";
    }
    if is_same_origin(playlist_url, target_url) {
        cookies
    } else {
        ""
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

fn force_ts_extension(name: &str) -> String {
    if let Some(dot_pos) = name.rfind('.') {
        format!("{}.ts", &name[..dot_pos])
    } else {
        format!("{}.ts", name)
    }
}

// ---------------------------------------------------------------------------
// HLS URL detection
// ---------------------------------------------------------------------------

/// Check if a URL points to an HLS manifest (`.m3u8` or `.m3u` extension).
/// Case-insensitive, ignores query parameters and fragments.
pub fn is_hls_url(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split('#').next().unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".m3u8") || lower.ends_with(".m3u")
}

// ---------------------------------------------------------------------------
// HLS types
// ---------------------------------------------------------------------------

/// Parsed M3U8 content — either a master playlist or a media playlist.
#[allow(dead_code)]
pub enum M3u8Content {
    Master {
        variants: Vec<HlsVariant>,
    },
    Media {
        segments: Vec<HlsSegment>,
        total_duration: f32,
        media_sequence: u64,
    },
}

/// A variant stream from a master playlist.
pub struct HlsVariant {
    pub bandwidth: u64,
    pub resolution: Option<(u64, u64)>,
    pub uri: String,
}

/// A single segment from a media playlist.
#[allow(dead_code)]
pub struct HlsSegment {
    pub uri: String,
    pub duration: f32,
    pub key: Option<HlsKey>,
}

/// Encryption key info for a segment.
pub struct HlsKey {
    pub method: HlsKeyMethod,
    pub uri: String,
    pub iv: Option<String>,
}

/// Key encryption method.
#[derive(Clone, PartialEq, Eq)]
pub enum HlsKeyMethod {
    Aes128,
    None,
}

// ---------------------------------------------------------------------------
// URI resolution
// ---------------------------------------------------------------------------

/// Resolve a possibly-relative URI against a base URL.
/// If `uri` starts with `http://` or `https://`, return as-is.
/// Otherwise, strip the path component after the last `/` from `base_url`
/// and append `uri`.
/// Resolve a possibly-relative URI against a base URL using RFC 3986 rules.
fn resolve_uri(base_url: &str, uri: &str) -> String {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return uri.to_string();
    }

    match url::Url::parse(base_url) {
        Ok(base) => match base.join(uri) {
            Ok(resolved) => resolved.to_string(),
            Err(_) => {
                // Fallback: simple concatenation
                if let Some(last_slash) = base_url.rfind('/') {
                    format!("{}/{}", &base_url[..last_slash], uri)
                } else {
                    uri.to_string()
                }
            }
        },
        Err(_) => {
            if let Some(last_slash) = base_url.rfind('/') {
                format!("{}/{}", &base_url[..last_slash], uri)
            } else {
                uri.to_string()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// M3U8 parsing
// ---------------------------------------------------------------------------

/// Fetch and parse an M3U8 playlist from the given URL.
pub async fn parse_m3u8(
    client: &Client,
    url: &str,
    cookies: &str,
    extra_headers: &std::collections::HashMap<String, String>,
) -> Result<M3u8Content, DownloadError> {
    let mut req = client.get(url);
    if !cookies.is_empty() {
        req = req.header("Cookie", cookies);
    }
    // 应用浏览器扩展捕获的额外请求头
    req = crate::downloader::apply_extra_headers(req, extra_headers);

    let resp = req.send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;

    let (_remaining, playlist) = m3u8_rs::parse_playlist(&bytes)
        .map_err(|e| DownloadError::Other(format!("M3U8 parse error: {}", e)))?;

    match playlist {
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            let variants: Vec<HlsVariant> = master
                .variants
                .iter()
                .map(|v| {
                    let resolution = v.resolution.as_ref().map(|r| (r.width, r.height));
                    HlsVariant {
                        bandwidth: v.bandwidth,
                        resolution,
                        uri: resolve_uri(url, &v.uri),
                    }
                })
                .collect();

            if variants.is_empty() {
                return Err(DownloadError::Other(
                    "M3U8 master playlist has no variants".to_string(),
                ));
            }

            Ok(M3u8Content::Master { variants })
        }
        m3u8_rs::Playlist::MediaPlaylist(media) => {
            let media_sequence = media.media_sequence;
            let mut total_duration: f32 = 0.0;
            let mut current_key: Option<HlsKey> = None;
            let mut segments: Vec<HlsSegment> = Vec::with_capacity(media.segments.len());

            for seg in &media.segments {
                total_duration += seg.duration;

                if let Some(ref key) = seg.key {
                    current_key = match &key.method {
                        &m3u8_rs::KeyMethod::AES128 => {
                            let key_uri = match key.uri.as_ref() {
                                Some(u) if !u.is_empty() => resolve_uri(url, u),
                                _ => {
                                    return Err(DownloadError::Other(
                                        "AES-128 KEY tag missing URI".to_string(),
                                    ));
                                }
                            };
                            Some(HlsKey {
                                method: HlsKeyMethod::Aes128,
                                uri: key_uri,
                                iv: key.iv.clone(),
                            })
                        }
                        &m3u8_rs::KeyMethod::None => Some(HlsKey {
                            method: HlsKeyMethod::None,
                            uri: String::new(),
                            iv: None,
                        }),
                        other => {
                            return Err(DownloadError::Other(format!(
                                "unsupported HLS encryption method: {:?}",
                                other
                            )));
                        }
                    };
                }

                let seg_key = current_key.as_ref().and_then(|k| {
                    if k.method == HlsKeyMethod::Aes128 {
                        Some(HlsKey {
                            method: HlsKeyMethod::Aes128,
                            uri: k.uri.clone(),
                            iv: k.iv.clone(),
                        })
                    } else {
                        None
                    }
                });

                segments.push(HlsSegment {
                    uri: resolve_uri(url, &seg.uri),
                    duration: seg.duration,
                    key: seg_key,
                });
            }

            if segments.is_empty() {
                return Err(DownloadError::Other(
                    "M3U8 media playlist has no segments".to_string(),
                ));
            }

            Ok(M3u8Content::Media {
                segments,
                total_duration,
                media_sequence,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// AES-128-CBC decryption
// ---------------------------------------------------------------------------

use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

/// Fetch an AES-128 key from the given URI, with caching.
async fn fetch_key(
    client: &Client,
    key_uri: &str,
    cookies: &str,
    playlist_url: &str,
    key_cache: &mut HashMap<String, Vec<u8>>,
    extra_headers: &std::collections::HashMap<String, String>,
) -> Result<Vec<u8>, DownloadError> {
    if let Some(cached) = key_cache.get(key_uri) {
        return Ok(cached.clone());
    }

    let safe_cookies = cookies_for_url(playlist_url, key_uri, cookies);
    let mut req = client.get(key_uri);
    if !safe_cookies.is_empty() {
        req = req.header("Cookie", safe_cookies);
    }
    // 应用浏览器扩展捕获的额外请求头
    req = crate::downloader::apply_extra_headers(req, extra_headers);

    let resp = req.send().await?.error_for_status()?;
    let key_bytes = resp.bytes().await?.to_vec();

    if key_bytes.len() != 16 {
        return Err(DownloadError::Other(format!(
            "AES-128 key must be 16 bytes, got {} bytes from {}",
            key_bytes.len(),
            key_uri
        )));
    }

    key_cache.insert(key_uri.to_string(), key_bytes.clone());
    Ok(key_bytes)
}

/// Parse an IV hex string (e.g. "0x1234abcd...") into 16 bytes.
fn parse_iv_hex(iv_str: &str) -> Result<[u8; 16], DownloadError> {
    let hex = iv_str
        .strip_prefix("0x")
        .or_else(|| iv_str.strip_prefix("0X"))
        .unwrap_or(iv_str);

    if hex.len() != 32 {
        return Err(DownloadError::Other(format!(
            "IV hex string must be 32 hex chars, got {}: {}",
            hex.len(),
            iv_str
        )));
    }

    let mut iv = [0u8; 16];
    for i in 0..16 {
        iv[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|e| DownloadError::Other(format!("invalid IV hex: {}", e)))?;
    }
    Ok(iv)
}

/// Compute the default IV from media_sequence + segment_index.
/// IV = (media_sequence + segment_index) as 128-bit big-endian.
fn compute_default_iv(media_sequence: u64, segment_index: usize) -> [u8; 16] {
    let sequence_number = media_sequence + segment_index as u64;
    let mut iv = [0u8; 16];
    // Write as 128-bit big-endian: lower 8 bytes at offset 8
    iv[8..16].copy_from_slice(&sequence_number.to_be_bytes());
    iv
}

/// Decrypt AES-128-CBC encrypted segment data in-place.
/// Returns the decrypted data (may be shorter than input due to PKCS7 padding removal).
fn decrypt_segment(data: &mut [u8], key: &[u8], iv: &[u8; 16]) -> Result<Vec<u8>, DownloadError> {
    let key_array: [u8; 16] = key
        .try_into()
        .map_err(|_| DownloadError::Other("AES key must be 16 bytes".to_string()))?;

    let decryptor = Aes128CbcDec::new_from_slices(&key_array, iv)
        .map_err(|e| DownloadError::Other(format!("AES init error: {}", e)))?;

    let decrypted = decryptor
        .decrypt_padded_mut::<Pkcs7>(data)
        .map_err(|e| DownloadError::Other(format!("AES decrypt error: {}", e)))?;

    Ok(decrypted.to_vec())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run_hls_download(mut params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let quality_rx = params.hls_quality_rx.take();
    let result = run_hls_download_inner(&params, quality_rx).await;

    match result {
        Ok(total) => {
            log_info!(
                "[hls-download] task {} completed, total={} bytes",
                task_id_log,
                total
            );
            let _ = params.db.update_task_status(&params.task_id, 3, "").await;
            let _ = params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: total,
                    total_bytes: total,
                    status: 3,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: None,
                })
                .await;
        }
        Err(DownloadError::Cancelled) => {
            log_info!("[hls-download] task {} cancelled", task_id_log);
        }
        Err(e) => {
            let msg = e.to_string();
            log_info!("[hls-download] task {} error: {}", task_id_log, msg);
            let _ = params.db.update_task_status(&params.task_id, 4, &msg).await;

            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => (t.downloaded_bytes, t.total_bytes),
                other => {
                    log_info!(
                        "[hls-download] task {} warning: failed to read progress from DB: {:?}",
                        task_id_log,
                        other.err()
                    );
                    (0, 0)
                }
            };
            let _ = params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: dl,
                    total_bytes: total,
                    status: 4,
                    error_message: msg,
                    file_name: String::new(),
                    segment_details: None,
                })
                .await;
        }
    }
}

// ---------------------------------------------------------------------------
// Variant selection
// ---------------------------------------------------------------------------

/// Timeout for waiting on user quality selection (seconds).
/// After this duration, the best quality is auto-selected.
const QUALITY_SELECTION_TIMEOUT_SECS: u64 = 60;

async fn select_variant(
    task_id: &str,
    variants: &[HlsVariant],
    quality_rx: Option<tokio::sync::oneshot::Receiver<i32>>,
    cancel_token: &tokio_util::sync::CancellationToken,
) -> Result<String, DownloadError> {
    let auto_select_best = || -> Result<String, DownloadError> {
        let best = variants
            .iter()
            .max_by_key(|v| v.bandwidth)
            .ok_or_else(|| DownloadError::Other("no variants in master playlist".to_string()))?;
        log_info!(
            "[hls-download] task {} auto-selected variant: bandwidth={}, resolution={:?}",
            task_id,
            best.bandwidth,
            best.resolution
        );
        Ok(best.uri.clone())
    };

    if let Some(rx) = quality_rx {
        // Skip dialog when there is only one variant — no point asking.
        if variants.len() <= 1 {
            log_info!(
                "[hls-download] task {} only {} variant(s), skipping quality dialog",
                task_id,
                variants.len()
            );
            return auto_select_best();
        }

        let options: Vec<HlsQualityOption> = variants
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let (w, h) = v.resolution.unwrap_or((0, 0));
                HlsQualityOption {
                    index: i as i32,
                    bandwidth: v.bandwidth as i64,
                    width: w as i64,
                    height: h as i64,
                }
            })
            .collect();

        HlsQualityOptions {
            task_id: task_id.to_string(),
            options,
        }
        .send_signal_to_dart();

        log_info!(
            "[hls-download] task {} sent {} quality options to Dart, waiting for selection (timeout={}s)",
            task_id,
            variants.len(),
            QUALITY_SELECTION_TIMEOUT_SECS
        );

        let timeout_duration = std::time::Duration::from_secs(QUALITY_SELECTION_TIMEOUT_SECS);

        tokio::select! {
            _ = cancel_token.cancelled() => {
                Err(DownloadError::Cancelled)
            }
            result = tokio::time::timeout(timeout_duration, rx) => {
                match result {
                    Ok(Ok(idx)) => {
                        let variant = variants.get(idx as usize).ok_or_else(|| {
                            DownloadError::Other(format!(
                                "invalid HLS quality index: {} (have {} variants)",
                                idx,
                                variants.len()
                            ))
                        })?;
                        log_info!(
                            "[hls-download] task {} user selected variant {}: bandwidth={}, resolution={:?}",
                            task_id,
                            idx,
                            variant.bandwidth,
                            variant.resolution
                        );
                        Ok(variant.uri.clone())
                    }
                    Ok(Err(_)) => {
                        // Channel closed (sender dropped) — auto-select best.
                        log_info!(
                            "[hls-download] task {} quality channel closed, auto-selecting best",
                            task_id
                        );
                        auto_select_best()
                    }
                    Err(_) => {
                        // Timeout — auto-select best.
                        log_info!(
                            "[hls-download] task {} quality selection timed out ({}s), auto-selecting best",
                            task_id,
                            QUALITY_SELECTION_TIMEOUT_SECS
                        );
                        auto_select_best()
                    }
                }
            }
        }
    } else {
        auto_select_best()
    }
}

// ---------------------------------------------------------------------------
// Core logic
// ---------------------------------------------------------------------------

async fn run_hls_download_inner(
    p: &DownloadParams,
    quality_rx: Option<tokio::sync::oneshot::Receiver<i32>>,
) -> Result<i64, DownloadError> {
    log_info!("[hls-download] task {} starting, url={}", p.task_id, p.url);

    // Transition to status=5 (preparing)
    let _ = p.db.update_task_status(&p.task_id, 5, "").await;
    let _ = p
        .progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: 0,
            status: 5,
            error_message: String::new(),
            file_name: p.file_name.clone(),
            segment_details: None,
        })
        .await;

    // Parse the M3U8 playlist
    let content = parse_m3u8(&p.client, &p.url, &p.cookies, &p.extra_headers).await?;

    let (segments, media_sequence) = match content {
        M3u8Content::Master { variants } => {
            let selected_uri =
                select_variant(&p.task_id, &variants, quality_rx, &p.cancel_token).await?;

            if p.cancel_token.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }

            let media_content =
                parse_m3u8(&p.client, &selected_uri, &p.cookies, &p.extra_headers).await?;
            match media_content {
                M3u8Content::Media {
                    segments,
                    total_duration: _,
                    media_sequence,
                } => (segments, media_sequence),
                M3u8Content::Master { .. } => {
                    return Err(DownloadError::Other(
                        "nested master playlist not supported".to_string(),
                    ));
                }
            }
        }
        M3u8Content::Media {
            segments,
            total_duration: _,
            media_sequence,
        } => (segments, media_sequence),
    };

    let segment_count = segments.len();
    log_info!(
        "[hls-download] task {} found {} segments, media_sequence={}",
        p.task_id,
        segment_count,
        media_sequence
    );

    if segment_count == 0 {
        return Err(DownloadError::Other(
            "HLS playlist has no segments".to_string(),
        ));
    }

    let auto_name = if p.file_name.is_empty() {
        let url_name = extract_from_url(&p.url).unwrap_or_else(|| "download.ts".to_string());
        force_ts_extension(&url_name)
    } else {
        force_ts_extension(&p.file_name)
    };

    let save_dir = PathBuf::from(&p.save_dir);
    let actual_name = if p.is_resume {
        auto_name.clone()
    } else {
        dedup_filename(&save_dir, &auto_name, &p.reserved_filenames_snapshot).await
    };

    // total_bytes is unknown for HLS until we download all segments
    p.db.update_task_file_info(&p.task_id, &actual_name, 0)
        .await?;

    // 早期取消检查：probe/解析完成后、创建文件之前检测 pause/delete，
    // 防止已取消的任务仍然在磁盘上创建临时文件。
    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    let _ = p.db.update_task_status(&p.task_id, 1, "").await;

    // Notify Dart: downloading started with file name
    let _ = p
        .progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: 0,
            status: 1,
            error_message: String::new(),
            file_name: actual_name.clone(),
            segment_details: None,
        })
        .await;

    let dest_path = save_dir.join(&actual_name);
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));

    // Ensure parent directory exists
    if let Some(parent) = temp_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // --- HLS resume support ---
    // On resume, check if we have a saved segment index from a previous run.
    // If so, skip already-downloaded segments and open the temp file in append mode.
    let resume_seg_key = format!("hls_resume_{}", p.task_id);
    let (mut file, skip_segments, mut downloaded_bytes) = if p.is_resume {
        // Parse checkpoint: new format is "idx:byte_offset", old format is just "idx".
        // Old markers without ':' get saved_bytes = 0 (no truncation — same as before).
        let (saved_idx, saved_bytes): (usize, i64) =
            p.db.get_config(&resume_seg_key)
                .await
                .ok()
                .flatten()
                .and_then(|s| {
                    let mut parts = s.splitn(2, ':');
                    let idx = parts.next()?.parse().ok()?;
                    let bytes = parts.next().and_then(|b| b.parse().ok()).unwrap_or(0i64);
                    Some((idx, bytes))
                })
                .unwrap_or((0, 0));
        let file_size = tokio::fs::metadata(&temp_path)
            .await
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        if saved_idx > 0 && file_size > 0 {
            // Truncate to the exact byte offset of the last fully-completed segment.
            // This removes any partially-written data from a crashed segment.
            let safe_size = if saved_bytes > 0 {
                saved_bytes.min(file_size)
            } else {
                file_size
            };
            if safe_size < file_size {
                log_info!(
                    "[hls] task {} truncating temp file {} -> {} bytes (removing partial segment data)",
                    p.task_id,
                    file_size,
                    safe_size
                );
                let truncate_file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&temp_path)
                    .await?;
                truncate_file.set_len(safe_size as u64).await?;
                drop(truncate_file);
            }
            log_info!(
                "[hls] task {} resuming from segment {} (file size: {} bytes, safe: {} bytes)",
                p.task_id,
                saved_idx,
                file_size,
                safe_size
            );
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&temp_path)
                .await?;
            (f, saved_idx, safe_size)
        } else {
            (File::create(&temp_path).await?, 0, 0i64)
        }
    } else {
        // Clean up any stale resume marker from a previous run
        let _ = p.db.delete_config(&resume_seg_key).await;
        (File::create(&temp_path).await?, 0, 0i64)
    };

    let mut key_cache: HashMap<String, Vec<u8>> = HashMap::new();
    let mut last_report = std::time::Instant::now();
    let mut last_db_save = std::time::Instant::now();

    for (seg_idx, segment) in segments.iter().enumerate() {
        // Skip already-downloaded segments on resume
        if seg_idx < skip_segments {
            continue;
        }
        // Check cancellation between segments
        if p.cancel_token.is_cancelled() {
            file.flush().await?;
            let _ =
                p.db.update_task_progress(&p.task_id, downloaded_bytes)
                    .await;
            return Err(DownloadError::Cancelled);
        }

        // Download segment with retry
        let seg_data = download_segment_with_retry(
            &p.client,
            &segment.uri,
            &p.cookies,
            &p.url,
            &p.cancel_token,
            &p.task_id,
            seg_idx,
            &p.extra_headers,
        )
        .await?;

        // Decrypt if needed
        let output_data = if let Some(ref key_info) = segment.key {
            if key_info.method == HlsKeyMethod::Aes128 && !key_info.uri.is_empty() {
                // Fetch key (cached)
                let key_bytes = fetch_key(
                    &p.client,
                    &key_info.uri,
                    &p.cookies,
                    &p.url,
                    &mut key_cache,
                    &p.extra_headers,
                )
                .await?;

                // Determine IV
                let iv = if let Some(ref iv_str) = key_info.iv {
                    parse_iv_hex(iv_str)?
                } else {
                    compute_default_iv(media_sequence, seg_idx)
                };

                // Decrypt
                let mut data_buf = seg_data;
                decrypt_segment(&mut data_buf, &key_bytes, &iv)?
            } else {
                seg_data
            }
        } else {
            seg_data
        };

        // Apply speed limiter and write to file
        let chunk_len = output_data.len();
        let mut offset = 0usize;
        while offset < chunk_len {
            let remaining = (chunk_len - offset) as u64;
            let allowed = p.speed_limiter.consume(remaining).await;
            let end = offset + allowed as usize;
            file.write_all(&output_data[offset..end]).await?;
            offset = end;
        }

        downloaded_bytes += chunk_len as i64;

        // Save resume checkpoint (segment index + byte offset) for HLS resume support.
        // Format: "next_seg_idx:total_bytes_written" — on resume we truncate to
        // this byte offset to discard any partially-written segment data.
        let _ =
            p.db.set_config(
                &resume_seg_key,
                &format!("{}:{}", seg_idx + 1, downloaded_bytes),
            )
            .await;

        // Progress reporting (every 200ms)
        if last_report.elapsed().as_millis() >= 200 {
            let _ = p
                .progress_tx
                .send(ProgressUpdate {
                    task_id: p.task_id.clone(),
                    downloaded_bytes,
                    total_bytes: 0, // unknown for HLS
                    status: 1,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: None,
                })
                .await;
            last_report = std::time::Instant::now();
        }

        // DB persistence (every DB_SAVE_INTERVAL_SECS)
        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
            let _ =
                p.db.update_task_progress(&p.task_id, downloaded_bytes)
                    .await;
            last_db_save = std::time::Instant::now();
        }

        log_info!(
            "[hls-download] task {} segment {}/{} done, {} bytes total",
            p.task_id,
            seg_idx + 1,
            segment_count,
            downloaded_bytes
        );
    }

    file.flush().await?;
    drop(file);

    // Save final progress
    let _ =
        p.db.update_task_progress(&p.task_id, downloaded_bytes)
            .await;

    // Clean up HLS resume marker on successful completion
    let _ = p.db.delete_config(&resume_seg_key).await;

    tokio::fs::rename(&temp_path, &dest_path)
        .await
        .map_err(|e| {
            DownloadError::Other(format!(
                "failed to rename {} -> {}: {}",
                temp_path.display(),
                dest_path.display(),
                e
            ))
        })?;

    log_info!(
        "[hls-download] task {} renamed {} -> {}",
        p.task_id,
        temp_path.display(),
        dest_path.display()
    );

    if let Some(mp4_path) = remux_ts_to_mp4(&dest_path, &p.task_id).await {
        let mp4_file_name = mp4_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("output.mp4")
            .to_string();
        let mp4_size = tokio::fs::metadata(&mp4_path)
            .await
            .ok()
            .and_then(|m| i64::try_from(m.len()).ok())
            .unwrap_or(downloaded_bytes);

        match p
            .db
            .update_task_file_info(&p.task_id, &mp4_file_name, mp4_size)
            .await
        {
            Ok(_) => {
                let _ = tokio::fs::remove_file(&dest_path).await;
                let _ = p
                    .progress_tx
                    .send(ProgressUpdate {
                        task_id: p.task_id.clone(),
                        downloaded_bytes: mp4_size,
                        total_bytes: mp4_size,
                        status: 1,
                        error_message: String::new(),
                        file_name: mp4_file_name,
                        segment_details: None,
                    })
                    .await;
                return Ok(mp4_size);
            }
            Err(e) => {
                log_info!(
                    "[hls] task {} DB update failed after remux: {}, removing orphan mp4 at {}",
                    p.task_id,
                    e,
                    mp4_path.display()
                );
                // DB update failed: the task record still points to the .ts file name.
                // delete_task uses the DB file_name to locate files, so the .mp4
                // would never be cleaned up. Remove it now to prevent a disk leak.
                let _ = tokio::fs::remove_file(&mp4_path).await;
            }
        }
    }

    Ok(downloaded_bytes)
}

// ---------------------------------------------------------------------------
// TS → MP4 remux (best-effort)
// ---------------------------------------------------------------------------

const MAX_REMUX_BYTES: u64 = 512 * 1024 * 1024;

async fn remux_ts_to_mp4(ts_path: &std::path::Path, task_id: &str) -> Option<PathBuf> {
    let ext = ts_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !ext.eq_ignore_ascii_case("ts") {
        return None;
    }

    let file_len = match tokio::fs::metadata(ts_path).await {
        Ok(m) => m.len(),
        Err(_) => return None,
    };
    if file_len > MAX_REMUX_BYTES {
        log_info!(
            "[hls] task {} skipping TS→MP4 remux: file is {} bytes (limit {}), keeping .ts",
            task_id,
            file_len,
            MAX_REMUX_BYTES
        );
        return None;
    }

    let parent = ts_path.parent()?;
    let stem = ts_path.file_stem().and_then(|s| s.to_str())?;
    let desired_name = format!("{}.mp4", stem);
    let unique_name =
        dedup_filename(parent, &desired_name, &std::collections::HashSet::new()).await;
    let mp4_path = parent.join(&unique_name);

    let ts_owned = ts_path.to_owned();
    let mp4_owned = mp4_path.clone();
    let mp4_tmp = mp4_path.with_extension("mp4.tmp");
    let mp4_tmp_inner = mp4_tmp.clone();

    match tokio::task::spawn_blocking(move || -> Result<(), std::io::Error> {
        let ts_data = std::fs::read(&ts_owned)?;
        let mp4_data = ts2mp4::convert_ts_to_mp4(&ts_data)?;
        drop(ts_data);
        std::fs::write(&mp4_tmp_inner, &mp4_data)?;
        drop(mp4_data);
        if mp4_owned.exists() {
            let _ = std::fs::remove_file(&mp4_tmp_inner);
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "mp4 target appeared after dedup",
            ));
        }
        std::fs::rename(&mp4_tmp_inner, &mp4_owned)?;
        Ok(())
    })
    .await
    {
        Ok(Ok(())) => {
            log_info!("[hls] task {} remuxed TS -> MP4", task_id);
            Some(mp4_path)
        }
        Ok(Err(e)) => {
            log_info!(
                "[hls] task {} MP4 remux failed: {}, keeping .ts",
                task_id,
                e
            );
            let _ = tokio::fs::remove_file(&mp4_tmp).await;
            None
        }
        Err(e) => {
            log_info!(
                "[hls] task {} MP4 remux join error: {}, keeping .ts",
                task_id,
                e
            );
            let _ = tokio::fs::remove_file(&mp4_tmp).await;
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Per-segment download with retry
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn download_segment_with_retry(
    client: &Client,
    url: &str,
    cookies: &str,
    playlist_url: &str,
    cancel_token: &tokio_util::sync::CancellationToken,
    task_id: &str,
    seg_idx: usize,
    extra_headers: &std::collections::HashMap<String, String>,
) -> Result<Vec<u8>, DownloadError> {
    let mut attempts = 0u32;

    loop {
        match download_segment_once(
            client,
            url,
            cookies,
            playlist_url,
            extra_headers,
            cancel_token,
        )
        .await
        {
            Ok(data) => return Ok(data),
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(e) => {
                attempts += 1;
                if attempts >= MAX_RETRIES {
                    return Err(DownloadError::Other(format!(
                        "HLS segment {} failed after {} retries: {}",
                        seg_idx, MAX_RETRIES, e
                    )));
                }
                log_info!(
                    "[hls-download] task {} segment {} attempt {}/{} failed: {}",
                    task_id,
                    seg_idx,
                    attempts,
                    MAX_RETRIES,
                    e
                );
                let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                tokio::select! {
                    _ = cancel_token.cancelled() => return Err(DownloadError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

async fn download_segment_once(
    client: &Client,
    url: &str,
    cookies: &str,
    playlist_url: &str,
    extra_headers: &std::collections::HashMap<String, String>,
    cancel_token: &tokio_util::sync::CancellationToken,
) -> Result<Vec<u8>, DownloadError> {
    let safe_cookies = cookies_for_url(playlist_url, url, cookies);
    let mut req = client.get(url);
    if !safe_cookies.is_empty() {
        req = req.header("Cookie", safe_cookies);
    }
    // 应用浏览器扩展捕获的额外请求头
    req = crate::downloader::apply_extra_headers(req, extra_headers);

    let resp = tokio::select! {
        _ = cancel_token.cancelled() => return Err(DownloadError::Cancelled),
        r = req.send() => r?.error_for_status()?,
    };

    // Transparently decompress if the server returned compressed content.
    let encoding = crate::downloader::detect_content_encoding(resp.headers());
    let raw_stream = resp.bytes_stream();
    let mut stream = crate::downloader::maybe_decompress_stream(raw_stream, encoding);

    /// Maximum allowed size for a single HLS segment (256 MB).
    /// Prevents OOM if a malicious or misconfigured server sends an oversized segment.
    const MAX_SEGMENT_BYTES: usize = 256 * 1024 * 1024;

    let mut buf = Vec::new();
    loop {
        let chunk = tokio::select! {
            _ = cancel_token.cancelled() => return Err(DownloadError::Cancelled),
            c = stream.next() => c,
        };
        let Some(chunk_result) = chunk else {
            break;
        };
        let chunk_data = chunk_result.map_err(DownloadError::Io)?;
        if buf.len() + chunk_data.len() > MAX_SEGMENT_BYTES {
            return Err(DownloadError::Other(format!(
                "HLS segment too large: exceeds {} MB limit",
                MAX_SEGMENT_BYTES / (1024 * 1024)
            )));
        }
        buf.extend_from_slice(&chunk_data);
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{compute_default_iv, is_hls_url, parse_iv_hex, resolve_uri};

    #[test]
    fn test_is_hls_url_m3u8() {
        assert!(is_hls_url("https://example.com/stream.m3u8"));
        assert!(is_hls_url("https://example.com/stream.M3U8"));
        assert!(is_hls_url("https://example.com/stream.m3u8?token=abc"));
        assert!(is_hls_url("https://example.com/path/index.m3u8#fragment"));
    }

    #[test]
    fn test_is_hls_url_m3u() {
        assert!(is_hls_url("https://example.com/stream.m3u"));
        assert!(is_hls_url("https://example.com/stream.M3U"));
    }

    #[test]
    fn test_is_hls_url_not_hls() {
        assert!(!is_hls_url("https://example.com/video.mp4"));
        assert!(!is_hls_url("https://example.com/stream.mpd"));
        assert!(!is_hls_url("https://example.com/file.ts"));
    }

    #[test]
    fn test_resolve_uri_absolute() {
        assert_eq!(
            resolve_uri(
                "https://cdn.example.com/live/master.m3u8",
                "https://other.com/seg.ts"
            ),
            "https://other.com/seg.ts"
        );
    }

    #[test]
    fn test_resolve_uri_relative() {
        assert_eq!(
            resolve_uri("https://cdn.example.com/live/master.m3u8", "segment0.ts"),
            "https://cdn.example.com/live/segment0.ts"
        );
    }

    #[test]
    fn test_resolve_uri_absolute_path() {
        assert_eq!(
            resolve_uri("https://cdn.example.com/live/master.m3u8", "/data/seg.ts"),
            "https://cdn.example.com/data/seg.ts"
        );
    }

    #[test]
    fn test_parse_iv_hex_with_prefix() {
        let iv = parse_iv_hex("0x00000000000000000000000000000001").unwrap_or([0; 16]);
        let mut expected = [0u8; 16];
        expected[15] = 1;
        assert_eq!(iv, expected);
    }

    #[test]
    fn test_parse_iv_hex_without_prefix() {
        let iv = parse_iv_hex("00000000000000000000000000000002").unwrap_or([0; 16]);
        let mut expected = [0u8; 16];
        expected[15] = 2;
        assert_eq!(iv, expected);
    }

    #[test]
    fn test_compute_default_iv() {
        let iv = compute_default_iv(0, 0);
        assert_eq!(iv, [0u8; 16]);

        let iv = compute_default_iv(0, 1);
        let mut expected = [0u8; 16];
        expected[15] = 1;
        assert_eq!(iv, expected);

        let iv = compute_default_iv(100, 5);
        let mut expected = [0u8; 16];
        let seq: u64 = 105;
        expected[8..16].copy_from_slice(&seq.to_be_bytes());
        assert_eq!(iv, expected);
    }

    #[test]
    fn test_is_hls_ftp_m3u8() {
        // FTP URL with .m3u8 extension — still detected as HLS
        assert!(is_hls_url("ftp://example.com/stream.m3u8"));
    }
}
