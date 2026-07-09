//! HLS (HTTP Live Streaming) download engine.
//!
//! Fetches M3U8 playlists, downloads all segments with bounded concurrency,
//! optionally decrypts AES-128-CBC encrypted segments, and merges them into a
//! single `.ts` output file.
//!
//! Architecture:
//! - Master playlist → auto-select highest bandwidth variant
//! - Media playlist → bounded-concurrency segment download with cancellation.
//!   Each segment downloads + decrypts on its own task (permit-gated by a
//!   `Semaphore`); a single writer drains finished segments in `seg_idx` order
//!   so the on-disk byte stream matches the sequential implementation exactly.
//! - AES-128-CBC decryption with shared key caching
//! - Progress reporting via ProgressUpdate channel (writer-side, so byte counts
//!   never double-count under concurrency)
//! - Per-segment retry with exponential backoff

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Semaphore, mpsc};

use crate::downloader::{
    DB_SAVE_INTERVAL_SECS, DownloadError, DownloadParams, ProgressUpdate, TEMP_EXT, dedup_filename,
    extract_from_url, sanitize_filename,
};
use crate::logger::log_info;
use crate::model::HlsQualityOption;
use crate::selection::SelectionOutcome;

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

/// Upper bound on concurrent segment downloads.
///
/// Capped at 16 to bound CDN per-IP connection pressure: HLS playlists often
/// have hundreds of tiny segments, and opening dozens of parallel connections
/// to a single streaming CDN risks tripping per-IP limits or throttling.
/// This ceiling is intentionally independent of `build_client`'s idle-pool
/// size (`pool_max_idle_per_host`, sized for the 64-segment HTTP path) —
/// the pool is large enough to keep every HLS connection warm regardless.
const MAX_HLS_CONCURRENCY: usize = 16;

/// Concurrency used when the user left the segment count on "auto"
/// (`segment_count <= 0`). Conservative enough to help every playlist without
/// hammering small CDNs.
const DEFAULT_HLS_CONCURRENCY: usize = 8;

/// Pick the number of segments to download in parallel.
///
/// Derived from the user-configured `segment_count` (the same knob the
/// multi-segment HTTP downloader uses), clamped to `[1, MAX_HLS_CONCURRENCY]`
/// and never exceeding the number of segments actually left to download.
/// `segment_count <= 0` means "auto" → `DEFAULT_HLS_CONCURRENCY`.
fn hls_concurrency(segment_count: i32, remaining_segments: usize) -> usize {
    let requested = if segment_count <= 0 {
        DEFAULT_HLS_CONCURRENCY
    } else {
        segment_count as usize
    };
    requested
        .clamp(1, MAX_HLS_CONCURRENCY)
        .min(remaining_segments.max(1))
}

pub(crate) fn force_ts_extension(name: &str) -> String {
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
    /// EXT-X-BYTERANGE 子区间 `(offset, length)`(字节)。`None` 表示整段就是
    /// 整个 `uri` 资源;`Some` 表示该段只是 `uri` 的一个子区间,下载时必须发
    /// `Range: bytes=offset-(offset+length-1)` 头并要求 206,否则多个段会各自
    /// 下载整文件、拼出 N 份完整副本(巨量损坏 + 撑爆磁盘)。
    pub byte_range: Option<(u64, u64)>,
    /// 本段是否紧跟 EXT-X-DISCONTINUITY(与前一段存在不连续点)。当前仅解析
    /// 并保留该标志;隐式 IV 仍按 RFC 8216 用绝对 Media Sequence Number 计算
    /// (见 `compute_default_iv` 注释),不连续点不重置该序号。
    pub discontinuity: bool,
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
    // 相对 URI 必须以"最终检索到的资源 URL"为 base 解析(RFC 3986 §5.1)。
    // reqwest 默认跟随重定向(见 downloader.rs),播放列表被负载均衡/短链
    // 重定向时,请求 url 与实际返回内容的 URL 不同;若仍用请求前的 url 作
    // base,会把相对段/密钥 URI 拼到错误的主机/路径。无重定向时
    // base_url == url,行为不变。与同仓 downloader.rs 既定做法对齐。
    let base_url = resp.url().to_string();
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
                        uri: resolve_uri(&base_url, &v.uri),
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
            // EXT-X-BYTERANGE 省略 @offset 时,offset = 同一 uri 上一子区间结束
            // 位置+1(按出现顺序累计)。键为已解析的绝对段 URI,值为该 uri 上
            // "下一个隐式子区间的起始 offset"(即上一子区间的 offset+length)。
            let mut byterange_next_offset: HashMap<String, u64> = HashMap::new();

            for seg in &media.segments {
                total_duration += seg.duration;

                // EXT-X-MAP(fMP4/CMAF 初始化段)目前无法正确产出可解码的 fMP4
                // 输出:本引擎只拼接媒体分片(moof+mdat),缺少前置 ftyp+moov
                // 初始化段则文件不可解码。为杜绝"静默产出不可播放文件却标记完成",
                // 检测到 EXT-X-MAP 即报错而非继续(安全退化方案)。
                if seg.map.is_some() {
                    return Err(DownloadError::Other(
                        "EXT-X-MAP (fMP4/CMAF 初始化段) 暂不支持".to_string(),
                    ));
                }

                if let Some(ref key) = seg.key {
                    current_key = match &key.method {
                        &m3u8_rs::KeyMethod::AES128 => {
                            let key_uri = match key.uri.as_ref() {
                                Some(u) if !u.is_empty() => resolve_uri(&base_url, u),
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

                let resolved_uri = resolve_uri(&base_url, &seg.uri);

                // EXT-X-BYTERANGE 解析:同一 uri 的多个段共享底层大文件的不同
                // 子区间。@offset 缺省时按出现顺序在该 uri 上累计(上一子区间
                // 结束位置)。offset+length 可能溢出 u64 → checked_add 报错而非
                // 回绕(回绕会请求错误区间、拼出损坏数据)。
                let byte_range = match &seg.byte_range {
                    Some(br) => {
                        let offset = match br.offset {
                            Some(o) => o,
                            None => byterange_next_offset
                                .get(&resolved_uri)
                                .copied()
                                .unwrap_or(0),
                        };
                        let next = offset.checked_add(br.length).ok_or_else(|| {
                            DownloadError::Other(format!(
                                "EXT-X-BYTERANGE offset+length overflow (offset={}, length={})",
                                offset, br.length
                            ))
                        })?;
                        byterange_next_offset.insert(resolved_uri.clone(), next);
                        Some((offset, br.length))
                    }
                    None => None,
                };

                segments.push(HlsSegment {
                    uri: resolved_uri,
                    duration: seg.duration,
                    key: seg_key,
                    byte_range,
                    discontinuity: seg.discontinuity,
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
use cbc::cipher::block_padding::{NoPadding, Pkcs7};
use cbc::cipher::{BlockDecryptMut, KeyIvInit};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

/// AES block size in bytes (AES-128-CBC operates on 16-byte blocks).
const AES_BLOCK_SIZE: usize = 16;

/// Shared AES-128 key cache: key URI → 16-byte key.
///
/// Wrapped in an async `Mutex` so concurrent segment tasks can share one cache
/// without re-fetching the same key. The lock is held only across the in-memory
/// `HashMap` access; the network fetch happens outside any lock (see
/// `fetch_key`), so a slow key fetch never blocks other tasks.
type KeyCache = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Fetch an AES-128 key from the given URI, with caching.
///
/// Two segments referencing the same key URI may race and both perform the
/// network fetch; this is harmless (idempotent GET) and far simpler than
/// holding the cache lock across I/O — the last writer wins and both observe an
/// identical 16-byte key.
async fn fetch_key(
    client: &Client,
    key_uri: &str,
    cookies: &str,
    playlist_url: &str,
    key_cache: &KeyCache,
    extra_headers: &std::collections::HashMap<String, String>,
) -> Result<Vec<u8>, DownloadError> {
    if let Some(cached) = key_cache.lock().await.get(key_uri) {
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

    key_cache
        .lock()
        .await
        .insert(key_uri.to_string(), key_bytes.clone());
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

/// Parse an HLS resume checkpoint string.
///
/// 支持的格式(向后兼容):
/// - `"idx:byte_offset:media_sequence"`(当前)
/// - `"idx:byte_offset"`(旧,media_sequence 视为未知 → `None`)
/// - `"idx"`(更早,byte_offset 视为 0)
///
/// 返回 `(saved_idx, saved_bytes, saved_media_seq)`;无法解析 idx 时返回
/// `(0, 0, None)`(等同于不 resume)。
fn parse_resume_checkpoint(s: &str) -> (usize, i64, Option<u64>) {
    let mut parts = s.splitn(3, ':');
    let idx = parts.next().and_then(|p| p.parse().ok());
    let Some(idx) = idx else {
        return (0, 0, None);
    };
    let bytes = parts.next().and_then(|b| b.parse().ok()).unwrap_or(0i64);
    let media_seq = parts.next().and_then(|m| m.parse::<u64>().ok());
    (idx, bytes, media_seq)
}

/// Compute the default IV from media_sequence + segment_index.
/// IV = (media_sequence + segment_index) as 128-bit big-endian.
///
/// `sequence_number` 是该段的绝对 Media Sequence Number(RFC 8216 §5.2:无显式
/// IV 时以段的 Media Sequence Number 作 IV)。该序号在整个播放列表内单调递增,
/// **不**在 EXT-X-DISCONTINUITY 处重置——不连续点改变的是 Discontinuity
/// Sequence Number,而非 Media Sequence Number。因此跟踪到的 `discontinuity`
/// 标志不参与隐式 IV 计算;若在此处"重置"序号反而会让合规加密流解出乱码。
///
/// 用 `saturating_add` 而非 `+`:序号接近 `u64::MAX` 时无检查加法在 debug 下
/// panic、在 release 下回绕得到错误 IV;饱和到 `u64::MAX` 不 panic,且此规模
/// 的段索引在现实播放列表中不可能出现,饱和值不会影响真实解密。
fn compute_default_iv(media_sequence: u64, segment_index: usize) -> [u8; 16] {
    let sequence_number = media_sequence.saturating_add(segment_index as u64);
    let mut iv = [0u8; 16];
    // Write as 128-bit big-endian: lower 8 bytes at offset 8
    iv[8..16].copy_from_slice(&sequence_number.to_be_bytes());
    iv
}

/// Decrypt AES-128-CBC encrypted segment data in-place.
///
/// Returns the decrypted data (may be shorter than input due to PKCS7 padding removal).
///
/// RFC 8216 要求 AES-128-CBC 段使用 PKCS7 填充,故首选 Pkcs7 解密。但现实中
/// 存在两类合规变体:某些 CDN/编码器(尤其转封装管线)产出"无填充"的密文,
/// 其总长度可能不是 16 的倍数 —— 此时 Pkcs7 解密必然失败,但数据本身有效。
/// 因此:
/// - 当 `data.len() % 16 != 0`(段本身非块对齐,说明源省略了填充):用
///   NoPadding 解密前 `(len/16)*16` 字节,尾部不足一块的字节丢弃。
/// - 当 `data.len() % 16 == 0` 但 Pkcs7 失败:**不** fallback,保留报错。
///   对齐却解不开通常意味着密钥/IV 错误,fallback 会掩盖真实解密失败、
///   产出垃圾数据。
///
/// `seg_idx` 仅用于在出错时给出可诊断的段索引。
fn decrypt_segment(
    data: &mut [u8],
    key: &[u8],
    iv: &[u8; 16],
    seg_idx: usize,
) -> Result<Vec<u8>, DownloadError> {
    // 空输入短路:加密段下载到 0 字节时,走对齐分支调
    // `decrypt_padded_mut::<Pkcs7>(&mut [])` 会返回 UnpadError,导致整个下载被
    // 当作永久失败中止。空密文解密只能是空明文,直接返回 `Vec::new()`,放在
    // 取模 / PKCS7 逻辑之前。
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let key_array: [u8; 16] = key
        .try_into()
        .map_err(|_| DownloadError::Other("AES key must be 16 bytes".to_string()))?;

    // 非块对齐:源省略了 PKCS7 填充。用 NoPadding 解密对齐前缀,丢弃尾部
    // 不足一块的残余字节(它们无法构成完整密文块)。
    if !data.len().is_multiple_of(AES_BLOCK_SIZE) {
        let aligned = (data.len() / AES_BLOCK_SIZE) * AES_BLOCK_SIZE;
        if aligned == 0 {
            return Err(DownloadError::Other(format!(
                "decrypt_segment: segment {} too short to decrypt ({} bytes, < one AES block)",
                seg_idx,
                data.len()
            )));
        }
        let decryptor = Aes128CbcDec::new_from_slices(&key_array, iv)
            .map_err(|e| DownloadError::Other(format!("AES init error: {}", e)))?;
        let decrypted = decryptor
            .decrypt_padded_mut::<NoPadding>(&mut data[..aligned])
            .map_err(|e| {
                DownloadError::Other(format!(
                    "decrypt_segment: segment {} NoPadding decrypt error: {}",
                    seg_idx, e
                ))
            })?;
        return Ok(decrypted.to_vec());
    }

    // 块对齐:按 RFC 8216 用 PKCS7 解密。失败不 fallback,直接报错(疑似
    // 密钥/IV 错误),避免掩盖真实解密失败。
    let decryptor = Aes128CbcDec::new_from_slices(&key_array, iv)
        .map_err(|e| DownloadError::Other(format!("AES init error: {}", e)))?;

    let decrypted = decryptor.decrypt_padded_mut::<Pkcs7>(data).map_err(|e| {
        DownloadError::Other(format!(
            "decrypt_segment: segment {} PKCS7 decrypt error (likely wrong key/IV): {}",
            seg_idx, e
        ))
    })?;

    Ok(decrypted.to_vec())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run_hls_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = run_hls_download_inner(&params).await;

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
                    ..Default::default()
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
                    ..Default::default()
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
    selector: &dyn crate::selection::HostSelection,
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

    // Skip the selector entirely when there is only one variant — no point asking.
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

    log_info!(
        "[hls-download] task {} requesting quality selection ({} variants) via HostSelection (timeout={}s)",
        task_id,
        variants.len(),
        QUALITY_SELECTION_TIMEOUT_SECS
    );

    let timeout_duration = std::time::Duration::from_secs(QUALITY_SELECTION_TIMEOUT_SECS);

    tokio::select! {
        _ = cancel_token.cancelled() => {
            Err(DownloadError::Cancelled)
        }
        outcome = selector.select_hls_quality(task_id, &options, timeout_duration) => {
            let idx = match &outcome {
                SelectionOutcome::UserChose(idx) => {
                    log_info!(
                        "[hls-download] task {} user selected variant {}",
                        task_id, idx
                    );
                    *idx
                }
                SelectionOutcome::TimedOutDefaulted(idx) => {
                    log_info!(
                        "[hls-download] task {} quality selection timed out ({}s), defaulting to variant {}",
                        task_id, QUALITY_SELECTION_TIMEOUT_SECS, idx
                    );
                    *idx
                }
                SelectionOutcome::NoSelectorConfigured(idx) => {
                    log_info!(
                        "[hls-download] task {} no selector configured, defaulting to variant {}",
                        task_id, idx
                    );
                    *idx
                }
            };
            let variant = variants.get(idx as usize).ok_or_else(|| {
                DownloadError::Other(format!(
                    "invalid HLS quality index: {} (have {} variants)",
                    idx,
                    variants.len()
                ))
            })?;
            log_info!(
                "[hls-download] task {} using variant: bandwidth={}, resolution={:?}",
                task_id, variant.bandwidth, variant.resolution
            );
            Ok(variant.uri.clone())
        }
    }
}

// ---------------------------------------------------------------------------
// Core logic
// ---------------------------------------------------------------------------

async fn run_hls_download_inner(p: &DownloadParams) -> Result<i64, DownloadError> {
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
            ..Default::default()
        })
        .await;

    // Parse the M3U8 playlist
    let content = parse_m3u8(&p.client, &p.url, &p.cookies, &p.extra_headers).await?;

    // media_playlist_url 是"实际列出 segment/key 的播放列表 URL"：
    // master→media 两级结构里是选中的 media playlist(selected_uri),
    // 直接 media 路径里就是 p.url 本身。段/密钥的同源 cookie 判定必须以
    // 它为基准——master 与 media playlist 经常跨主机(master 在主域、
    // media+segments+key 在 CDN),用 master URL 判同源会错误剥离 CDN
    // 鉴权 cookie 导致 403/401。直接 media 路径下 media_playlist_url==p.url,
    // 行为不变。
    let (segments, media_sequence, media_playlist_url) = match content {
        M3u8Content::Master { variants } => {
            let selected_uri =
                select_variant(&p.task_id, &variants, p.selector.as_ref(), &p.cancel_token).await?;

            if p.cancel_token.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }

            // 拉取所选 variant 时按同源过滤 cookie：selected_uri 可能指向
            // 与 p.url 不同源的 CDN，无条件透传 p.cookies 会把用户为原站点
            // 提供的会话/鉴权令牌泄露给第三方。与 fetch_key/download_segment
            // 的同源策略保持一致。
            let variant_cookies = cookies_for_url(&p.url, &selected_uri, &p.cookies);
            let media_content =
                parse_m3u8(&p.client, &selected_uri, variant_cookies, &p.extra_headers).await?;
            match media_content {
                M3u8Content::Media {
                    segments,
                    total_duration: _,
                    media_sequence,
                } => (segments, media_sequence, selected_uri),
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
        } => (segments, media_sequence, p.url.clone()),
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
        force_ts_extension(&sanitize_filename(&p.file_name))
    };

    let save_dir = PathBuf::from(&p.save_dir);
    // 文件名由 DownloadManager 在 do_start_task 同步段统一决策（含 dedup 和
    // 兄弟任务预订协调），HLS downloader 内不再做名称变更——保留
    // p.file_name 即可，仅当为空时（兜底）使用 URL 解析结果。
    let actual_name = auto_name.clone();

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
            ..Default::default()
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
        // Parse checkpoint. 当前格式 "idx:byte_offset:media_sequence";
        // 向后兼容旧格式 "idx:byte_offset"(缺 media_sequence 视为未知)与
        // 更早的 "idx"(缺 byte_offset 视为 0,不截断)。
        //
        // saved_media_seq = None 表示该字段未知(旧 checkpoint)。
        let (saved_idx, saved_bytes, saved_media_seq): (usize, i64, Option<u64>) =
            p.db.get_config(&resume_seg_key)
                .await
                .ok()
                .flatten()
                .map(|s| parse_resume_checkpoint(&s))
                .unwrap_or((0, 0, None));

        // IV 计算(无显式 IV 的加密段)依赖 media_sequence。若服务器在两次
        // 抓取之间重写了 EXT-X-MEDIA-SEQUENCE(VOD 被 CDN 重新生成等),已
        // 跳过的段与新解析的 media_sequence 组合会让续传段用错 IV,解密出
        // 垃圾数据。检测到不一致时放弃 resume,走全量重下保证 IV 与首次一致。
        // 旧格式 checkpoint(saved_media_seq=None)无法判断服务器是否改写了
        // EXT-X-MEDIA-SEQUENCE。仅当播放列表确实含"AES-128 且无显式 IV"的段
        // (其 IV=compute_default_iv(media_sequence,idx),依赖 media_sequence)时,
        // media_sequence 漂移才会导致解密错位;此时对旧 checkpoint 保守放弃 resume
        // 全量重下。明文 / 显式 IV / 常量 media_sequence(VOD 通常恒为 0)等常见场景
        // 不受影响,继续 resume 以保留有效进度。
        let uses_computed_iv = segments.iter().any(|s| {
            s.key
                .as_ref()
                .is_some_and(|k| k.method == HlsKeyMethod::Aes128 && k.iv.is_none())
        });
        let media_seq_changed = match saved_media_seq {
            Some(prev) => prev != media_sequence,
            None => uses_computed_iv,
        };
        if media_seq_changed {
            log_info!(
                "[hls] task {} media_sequence changed across resume (saved={:?}, now={}), \
                 abandoning resume and re-downloading from scratch to keep IV consistent",
                p.task_id,
                saved_media_seq,
                media_sequence
            );
        }

        let file_size = tokio::fs::metadata(&temp_path)
            .await
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        // 续传要求 saved_bytes > 0(三字段 checkpoint 记录的已完整落盘字节数)。
        // 早期版本只写 "idx"(无字节数)的旧 checkpoint 解析出 saved_bytes=0,此时
        // 无法确认磁盘上第 saved_idx 段是否完整——若上次硬崩在 write_all 中途,
        // file_size 会含残字节;旧逻辑用 file_size 当 safe_size 不截断,会把残字节当
        // 有效数据、后续段追加其后导致输出损坏。故对无字节偏移的旧 checkpoint 保守
        // 放弃 resume、全量重下(仅影响从早期版本升级、且恰好硬崩在段中途的遗留任务)。
        if saved_idx > 0 && file_size > 0 && saved_bytes > 0 && !media_seq_changed {
            // Truncate to the exact byte offset of the last fully-completed segment.
            // This removes any partially-written data from a crashed segment.
            let safe_size = saved_bytes.min(file_size);
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

    let key_cache: KeyCache = Arc::new(Mutex::new(HashMap::new()));
    let mut last_report = std::time::Instant::now();
    let mut last_db_save = std::time::Instant::now();

    // -----------------------------------------------------------------------
    // Bounded-concurrency segment download.
    //
    // Each remaining segment downloads + decrypts on its own task, gated by a
    // `Semaphore` so at most `concurrency` are in flight (and at most that many
    // decrypted buffers are buffered waiting to be written). A single writer —
    // this function — drains finished segments **strictly in `seg_idx` order**
    // via a `BTreeMap`, then runs the *same* speed-limit / write / rollback /
    // checkpoint / progress code the sequential implementation used. Keeping
    // all writes on one task guarantees:
    //   • the on-disk byte order is identical to the sequential version,
    //   • `downloaded_bytes` is accumulated exactly once per segment (no
    //     double-counting / loss under concurrency),
    //   • the speed limiter is consulted on the single write path,
    //   • the resume checkpoint advances monotonically by completed prefix.
    // Each task computes its IV from its own `seg_idx`, so AES-128-CBC
    // decryption stays correct regardless of completion order.
    // -----------------------------------------------------------------------
    let first_idx = skip_segments;
    let remaining = segment_count.saturating_sub(first_idx);
    let concurrency = hls_concurrency(p.segment_count, remaining);
    log_info!(
        "[hls-download] task {} downloading {} remaining segment(s) with concurrency {}",
        p.task_id,
        remaining,
        concurrency
    );

    let semaphore = Arc::new(Semaphore::new(concurrency));
    // Channel capacity == concurrency: at most `concurrency` permit-holding
    // producers can each enqueue exactly one result before releasing their
    // permit, so a producer never blocks on `send` waiting for the writer —
    // this prevents a permit-starvation deadlock when the writer is waiting on
    // an earlier index.
    let (result_tx, mut result_rx) =
        mpsc::channel::<(usize, Result<Vec<u8>, DownloadError>)>(concurrency.max(1));

    // Spawn one download+decrypt task per remaining segment. Tasks own all the
    // data they need (clones of cheap Arc/handle types).
    let mut producers: Vec<tokio::task::JoinHandle<()>> = Vec::with_capacity(remaining);
    for seg_idx in first_idx..segment_count {
        let Some(segment) = segments.get(seg_idx) else {
            break;
        };
        let uri = segment.uri.clone();
        let byte_range = segment.byte_range;
        // Extract only the encryption fields needed to decrypt this segment.
        let key_info: Option<(String, Option<String>)> = segment.key.as_ref().and_then(|k| {
            if k.method == HlsKeyMethod::Aes128 && !k.uri.is_empty() {
                Some((k.uri.clone(), k.iv.clone()))
            } else {
                None
            }
        });

        let client = p.client.clone();
        let cookies = p.cookies.clone();
        let playlist_url = media_playlist_url.clone();
        let cancel = p.cancel_token.clone();
        let task_id = p.task_id.clone();
        let extra_headers = p.extra_headers.clone();
        let key_cache = key_cache.clone();
        let sem = semaphore.clone();
        let tx = result_tx.clone();

        producers.push(tokio::spawn(async move {
            // Hold the permit across download + decrypt + send so in-flight work
            // (and buffered decrypted bytes) stays bounded by `concurrency`.
            let _permit = match sem.acquire().await {
                Ok(permit) => permit,
                // Semaphore closed — runtime shutting down; nothing to send.
                Err(_) => return,
            };
            if cancel.is_cancelled() {
                let _ = tx.send((seg_idx, Err(DownloadError::Cancelled))).await;
                return;
            }
            let outcome = download_and_decrypt_segment(
                &client,
                &uri,
                byte_range,
                &cookies,
                &playlist_url,
                &cancel,
                &task_id,
                seg_idx,
                &extra_headers,
                key_info.as_ref(),
                &key_cache,
                media_sequence,
            )
            .await;
            // Always emit a result for this index so the in-order writer never
            // blocks forever waiting on a task that failed.
            let _ = tx.send((seg_idx, outcome)).await;
        }));
    }
    // Drop our keep-alive sender so `result_rx` closes once every producer
    // finishes — otherwise the writer's recv loop would hang at the end.
    drop(result_tx);

    // In-order writer: buffer out-of-order completions and flush the contiguous
    // prefix starting at `next_to_write`.
    let mut pending: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut next_to_write = first_idx;
    let mut fatal_error: Option<DownloadError> = None;

    'writer: while next_to_write < segment_count {
        // Writer-side cancellation check (mirrors the original between-segment
        // check). Cancel the token so producers abort, flush progress, exit.
        if p.cancel_token.is_cancelled() {
            fatal_error = Some(DownloadError::Cancelled);
            break;
        }

        // If the next segment isn't buffered yet, wait for more completions.
        while !pending.contains_key(&next_to_write) {
            match result_rx.recv().await {
                Some((idx, Ok(data))) => {
                    pending.insert(idx, data);
                }
                Some((idx, Err(e))) => {
                    // A segment failed permanently. Cancel siblings and stop;
                    // the partial prefix already on disk is kept for resume.
                    log_info!(
                        "[hls-download] task {} segment {} failed: {}",
                        p.task_id,
                        idx,
                        e
                    );
                    p.cancel_token.cancel();
                    fatal_error = Some(e);
                    break 'writer;
                }
                None => {
                    // Channel closed before producing `next_to_write`. This can
                    // only happen if a producer was dropped without sending
                    // (e.g. semaphore closed during shutdown); treat as cancel.
                    if fatal_error.is_none() {
                        fatal_error = Some(DownloadError::Cancelled);
                    }
                    break 'writer;
                }
            }
        }

        // Flush every contiguous segment we already have, in order.
        while let Some(output_data) = pending.remove(&next_to_write) {
            let seg_idx = next_to_write;

            // Stop flushing promptly on cancellation. Segments already written
            // (the contiguous prefix) stay on disk with a matching checkpoint,
            // so a later resume continues cleanly; this in-memory segment and
            // the rest of `pending` are discarded (they will be re-downloaded).
            if p.cancel_token.is_cancelled() {
                drop(output_data);
                fatal_error = Some(DownloadError::Cancelled);
                break 'writer;
            }

            // Apply speed limiter and write to file.
            //
            // seg_start_pos 是本段写入前文件的逻辑长度。resume 时文件已被
            // truncate 到恰好 safe_size(== 初始 downloaded_bytes),此后每段以
            // append 方式精确追加 chunk_len 字节,故文件磁盘长度始终等于
            // downloaded_bytes —— 用它作为出错回退点是准确的。
            let chunk_len = output_data.len();
            let seg_start_pos = downloaded_bytes;
            let mut offset = 0usize;
            let mut write_result: Result<(), std::io::Error> = Ok(());
            while offset < chunk_len {
                let remaining_bytes = (chunk_len - offset) as u64;
                let allowed = p.speed_limiter.consume(remaining_bytes).await;
                let end = offset + allowed as usize;
                if let Err(e) = file.write_all(&output_data[offset..end]).await {
                    write_result = Err(e);
                    break;
                }
                offset = end;
            }

            if let Err(e) = write_result {
                // 写入中途失败(常见:磁盘满 ENOSPC)。本段已部分写入,先把文件
                // 回退到本段写入前的长度,避免残留半截分段污染后续 resume(与
                // dash_downloader 的 set_len(start_pos) 兜底一致)。回退失败仅记录
                // 日志,不掩盖原始写入错误。
                if let Err(trunc_err) = file.set_len(seg_start_pos as u64).await {
                    log_info!(
                        "[hls] task {} segment {} rollback set_len({}) failed: {}",
                        p.task_id,
                        seg_idx,
                        seg_start_pos,
                        trunc_err
                    );
                }
                p.cancel_token.cancel();
                // 磁盘空间不足(ENOSPC, errno 28 / ErrorKind::StorageFull)给出
                // 明确提示,便于用户区分"磁盘满"与普通 IO 错误。
                if e.kind() == std::io::ErrorKind::StorageFull || e.raw_os_error() == Some(28) {
                    fatal_error = Some(DownloadError::Other(
                        "磁盘空间不足，请清理磁盘后重试".to_string(),
                    ));
                } else {
                    fatal_error = Some(DownloadError::Io(e));
                }
                break 'writer;
            }

            downloaded_bytes += chunk_len as i64;
            next_to_write += 1;

            // Save resume checkpoint for HLS resume support.
            // Format: "next_seg_idx:total_bytes_written:media_sequence" — on resume
            // we truncate to this byte offset to discard any partially-written
            // segment data,并比对 media_sequence 以保证续传段的 IV 计算与首次一致。
            // 因为按 seg_idx 顺序写盘,next_to_write 即"已完整落盘的连续前缀
            // 长度",检查点始终对应一段完整、可安全续传的字节边界。
            let _ =
                p.db.set_config(
                    &resume_seg_key,
                    &format!("{}:{}:{}", next_to_write, downloaded_bytes, media_sequence),
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
                        ..Default::default()
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
    }

    // Ensure producers stop and are reaped before we touch the file further.
    // On error/cancel the token is already cancelled; either way drain the
    // handles so no task outlives this function.
    if fatal_error.is_some() {
        p.cancel_token.cancel();
    }
    // Drop the receiver FIRST: on the error/cancel path the writer stopped
    // recv'ing, so a producer parked in `tx.send().await` (bounded channel at
    // capacity) would block forever and hang the join below. Closing the
    // receiver makes those sends return `Err` immediately, letting every
    // producer unwind. On the success path the channel is already drained, so
    // this is a no-op.
    drop(result_rx);
    for handle in producers {
        let _ = handle.await;
    }

    if let Some(err) = fatal_error {
        // Persist whatever fully-written prefix we have so a later resume can
        // continue from there (matches the sequential cancel path).
        let _ = file.flush().await;
        let _ =
            p.db.update_task_progress(&p.task_id, downloaded_bytes)
                .await;
        return Err(err);
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
                        // remux 成功即完成,发 status=3(完成);外层 run_hls_download
                        // 还会再发一次 status=3,Dart 端有 oldStatus!=completed 守卫,
                        // 不会重复触发完成回调。
                        status: 3,
                        error_message: String::new(),
                        file_name: mp4_file_name,
                        segment_details: None,
                        ..Default::default()
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

/// remux 需要 dest 卷至少还有 `file_len`(mp4 产物 ≈ ts 体积,仅重封装
/// 无转码)+ 安全余量——remux 期间 `.ts` 与 `.mp4` 并存,峰值 ≈ 2x。
/// `avail=None`(网络盘/权限/超时,无法探测)按放行处理:预检是优化,
/// 安全网是下方既有的写失败清理路径。
fn remux_space_ok(avail: Option<u64>, file_len: u64) -> bool {
    match avail {
        Some(a) => a >= file_len.saturating_add(crate::disk_space::PRECHECK_MARGIN),
        None => true,
    }
}

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

    // ENOSPC 预检:空间不足时跳过 remux,走既有"保留 .ts"降级路径。
    let avail = crate::disk_space::available_space_checked(parent.to_path_buf()).await;
    if !remux_space_ok(avail, file_len) {
        log_info!(
            "[hls] task {} skipping TS→MP4 remux: insufficient disk space (avail={:?}, need {}+margin), keeping .ts",
            task_id,
            avail,
            file_len
        );
        return None;
    }
    let stem = ts_path.file_stem().and_then(|s| s.to_str())?;
    let desired_name = format!("{}.mp4", stem);
    let unique_name = dedup_filename(
        parent,
        &desired_name,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
    )
    .await;
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
        // 原子占名(同 `downloader::claim_rename` 协议,此处在阻塞线程内走
        // 同步 API):create_new 独占创建占位——dedup 与落盘之间若有并发
        // 写者(同名 HTTP/BT 任务完成)抢得该名,后到者得 AlreadyExists,
        // 决不覆盖;rename 覆盖的是自己的占位。失败清理占位与 tmp。
        if let Err(e) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&mp4_owned)
            .map(drop)
        {
            let _ = std::fs::remove_file(&mp4_tmp_inner);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&mp4_tmp_inner, &mp4_owned) {
            let _ = std::fs::remove_file(&mp4_owned);
            let _ = std::fs::remove_file(&mp4_tmp_inner);
            return Err(e);
        }
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
// Per-segment download + decrypt (concurrency unit)
// ---------------------------------------------------------------------------

/// Download a single segment (with retry) and, if encrypted, decrypt it.
///
/// This is the unit of work each concurrent task runs. It is purely
/// download + decrypt: it performs no disk writes, progress reporting, or
/// checkpointing — those stay on the single ordered writer so byte counts and
/// on-disk order remain correct under concurrency.
///
/// `key_info` is `Some((key_uri, iv))` only for AES-128 segments with a
/// non-empty key URI; the IV is computed from this segment's own `seg_idx`
/// (`compute_default_iv`) when not explicitly provided, so AES-128-CBC stays
/// correct regardless of the order tasks complete in.
#[allow(clippy::too_many_arguments)]
async fn download_and_decrypt_segment(
    client: &Client,
    uri: &str,
    byte_range: Option<(u64, u64)>,
    cookies: &str,
    playlist_url: &str,
    cancel_token: &tokio_util::sync::CancellationToken,
    task_id: &str,
    seg_idx: usize,
    extra_headers: &std::collections::HashMap<String, String>,
    key_info: Option<&(String, Option<String>)>,
    key_cache: &KeyCache,
    media_sequence: u64,
) -> Result<Vec<u8>, DownloadError> {
    let seg_data = download_segment_with_retry(
        client,
        uri,
        byte_range,
        cookies,
        playlist_url,
        cancel_token,
        task_id,
        seg_idx,
        extra_headers,
    )
    .await?;

    let Some((key_uri, iv_str)) = key_info else {
        return Ok(seg_data);
    };

    // Fetch key (shared cache across all concurrent tasks).
    let key_bytes = fetch_key(
        client,
        key_uri,
        cookies,
        // 同源判定以实际列出该密钥的 media playlist 为基准。
        playlist_url,
        key_cache,
        extra_headers,
    )
    .await?;

    // Determine IV — explicit IV from the playlist, else derived from this
    // segment's own index so concurrency never changes the IV.
    let iv = match iv_str {
        Some(iv_hex) => parse_iv_hex(iv_hex)?,
        None => compute_default_iv(media_sequence, seg_idx),
    };

    let mut data_buf = seg_data;
    decrypt_segment(&mut data_buf, &key_bytes, &iv, seg_idx)
}

// ---------------------------------------------------------------------------
// Per-segment download with retry
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn download_segment_with_retry(
    client: &Client,
    url: &str,
    byte_range: Option<(u64, u64)>,
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
            byte_range,
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
    byte_range: Option<(u64, u64)>,
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

    // EXT-X-BYTERANGE:同一 uri 的多段是底层大文件的不同子区间,必须发
    // `Range: bytes=offset-(offset+length-1)` 头只取本段区间。否则每段都拉整
    // 文件,N 段拼成 N 份完整副本(巨量损坏 + 撑爆磁盘)。range_end 用
    // checked 运算防溢出(offset 已在解析期与 length 一起校验过,这里再保一道)。
    if let Some((offset, length)) = byte_range {
        if length == 0 {
            return Err(DownloadError::Other(
                "EXT-X-BYTERANGE length must be > 0".to_string(),
            ));
        }
        let range_end = match offset.checked_add(length).and_then(|e| e.checked_sub(1)) {
            Some(end) => end,
            None => {
                return Err(DownloadError::Other(format!(
                    "EXT-X-BYTERANGE range overflow (offset={}, length={})",
                    offset, length
                )));
            }
        };
        req = req.header("Range", format!("bytes={}-{}", offset, range_end));
    }

    let resp = tokio::select! {
        _ = cancel_token.cancelled() => return Err(DownloadError::Cancelled),
        r = req.send() => r?.error_for_status()?,
    };

    // ranged 请求(EXT-X-BYTERANGE)必须得到 206 Partial Content。若服务器忽略
    // Range 头返回 200 全量,则收到的是整个底层文件而非本段子区间;放行会把
    // 整文件当成本段拼进输出造成损坏。故对 ranged 请求强制要求 206,否则报错
    // (触发上层重试,仍失败则整任务失败,绝不静默产出损坏文件)。
    if byte_range.is_some() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(DownloadError::Other(format!(
            "EXT-X-BYTERANGE 请求未返回 206 Partial Content (got {}); \
             服务器不支持 Range,无法正确切分子区间",
            resp.status()
        )));
    }

    // Transparently decompress if the server returned compressed content.
    let encoding = crate::downloader::detect_content_encoding(resp.headers());
    // 声明的 body 字节数(EOF 后据此做截断校验)。
    // - 对 ranged 请求:用 EXT-X-BYTERANGE 声明的 length 作为期望长度,而非
    //   响应的 Content-Length(206 的 Content-Length 是子区间长度,正常应相等,
    //   但以播放列表声明为准更稳妥),且 ranged 子区间通常未压缩。
    // - 对普通请求:仅当响应"无 Content-Encoding"时 content_length 才等于实际
    //   写入字节数;压缩响应解压后 buf 长度必然 != content_length(压缩后的值),
    //   故只对未压缩响应启用,避免误伤合法压缩分段。
    let declared_len = match byte_range {
        // byte_range 长度只在【未压缩】时等于落盘字节数；若服务器对 206 仍压缩
        // （正常不会，因强制 Accept-Encoding: identity），解压后长度 != 请求长度，
        // 会误报截断。与下方 None 分支一致地在有 encoding 时跳过大小校验。
        Some((_, length)) => {
            if encoding.is_none() {
                Some(length)
            } else {
                None
            }
        }
        None => {
            if encoding.is_none() {
                resp.content_length()
            } else {
                None
            }
        }
    };
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

    // 完整性校验：当有期望长度(普通请求的 Content-Length / ranged 请求的
    // EXT-X-BYTERANGE length)时,EOF 后实际字节必须恰好等于该值。服务器在分段
    // 中途关闭连接(TCP RST / chunked 提前 EOF)会让 stream 返回 None 被当作正常
    // 结束,只写入部分字节;不校验会把截断分段静默 append 进输出造成缺帧/花屏,
    // 而任务被标记完成。对 ranged 请求,收到字节数也据此对齐到本子区间长度
    // (而非整文件)。返回 Err 触发上层 download_segment_with_retry 重试。
    if let Some(expected) = declared_len
        && buf.len() as u64 != expected
    {
        return Err(DownloadError::Other(format!(
            "HLS segment truncated: got {} bytes, expected {}",
            buf.len(),
            expected
        )));
    }

    Ok(buf)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_HLS_CONCURRENCY, MAX_HLS_CONCURRENCY, compute_default_iv, decrypt_segment,
        hls_concurrency, is_hls_url, parse_iv_hex, parse_resume_checkpoint, remux_space_ok,
        resolve_uri,
    };
    use aes::Aes128;
    use cbc::cipher::block_padding::{NoPadding, Pkcs7};
    use cbc::cipher::{BlockEncryptMut, KeyIvInit};

    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    /// PKCS7-encrypt `plaintext` with the given key/iv, returning ciphertext.
    /// 返回 `None` 时由调用方断言失败,避免在测试中使用 `unwrap`/`expect`。
    fn encrypt_pkcs7(plaintext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Option<Vec<u8>> {
        let enc = Aes128CbcEnc::new_from_slices(key, iv).ok()?;
        // 输出缓冲需容纳 padding(最多多一整块)。
        let mut buf = vec![0u8; plaintext.len() + 16];
        let ct = enc
            .encrypt_padded_b2b_mut::<Pkcs7>(plaintext, &mut buf)
            .ok()?;
        Some(ct.to_vec())
    }

    // ---------------------------------------------------------------------
    // remux_space_ok — ENOSPC 预检阈值(remux 期间 .ts 与 .mp4 并存 ≈ 2x)。
    // ---------------------------------------------------------------------

    #[test]
    fn remux_space_ok_thresholds() {
        const MARGIN: u64 = crate::disk_space::PRECHECK_MARGIN;
        let file_len = 100 * 1024 * 1024u64;
        // 无法探测(网络盘/超时)→ 乐观放行。
        assert!(remux_space_ok(None, file_len));
        // 恰好够(file_len + margin)→ 放行。
        assert!(remux_space_ok(Some(file_len + MARGIN), file_len));
        // 差 1 字节 → 拒绝。
        assert!(!remux_space_ok(Some(file_len + MARGIN - 1), file_len));
        // 溢出安全:file_len 接近 u64::MAX 时 saturating_add 不回绕。
        assert!(!remux_space_ok(Some(u64::MAX - 1), u64::MAX));
    }

    /// No-padding encrypt (input must be block-aligned), returning ciphertext.
    fn encrypt_nopad(plaintext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Option<Vec<u8>> {
        let enc = Aes128CbcEnc::new_from_slices(key, iv).ok()?;
        let mut buf = plaintext.to_vec();
        let len = buf.len();
        let ct = enc.encrypt_padded_mut::<NoPadding>(&mut buf, len).ok()?;
        Some(ct.to_vec())
    }

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

    // --- F036: decrypt_segment padding handling ---

    #[test]
    fn test_decrypt_segment_pkcs7_roundtrip() {
        // 块对齐明文经 PKCS7 加密后,decrypt_segment 应正确解出原文。
        let key = [0x11u8; 16];
        let iv = [0x22u8; 16];
        let plaintext = b"hello world, hls!".to_vec(); // 17 bytes -> padded to 32
        let Some(mut ct) = encrypt_pkcs7(&plaintext, &key, &iv) else {
            panic!("test fixture encryption failed");
        };
        assert_eq!(ct.len() % 16, 0, "pkcs7 ciphertext must be block-aligned");
        let out = decrypt_segment(&mut ct, &key, &iv, 0);
        match out {
            Ok(decoded) => assert_eq!(decoded, plaintext),
            Err(e) => panic!("pkcs7 decrypt should succeed: {e}"),
        }
    }

    #[test]
    fn test_decrypt_segment_nopadding_when_unaligned() {
        // 源省略填充导致密文非块对齐:decrypt_segment 应走 NoPadding 解出
        // 对齐前缀,丢弃尾部不足一块的残余字节,而非整体失败(F036 核心)。
        let key = [0x33u8; 16];
        let iv = [0x44u8; 16];
        let plaintext = [0xABu8; 48]; // 3 blocks, no padding
        let Some(ct_aligned) = encrypt_nopad(&plaintext, &key, &iv) else {
            panic!("test fixture nopadding encryption failed");
        };
        // 追加 5 字节"残余",模拟非块对齐密文(总长 53)。
        let mut ct = ct_aligned.clone();
        ct.extend_from_slice(&[0x99u8; 5]);
        assert_ne!(ct.len() % 16, 0, "fixture must be unaligned");

        let out = decrypt_segment(&mut ct, &key, &iv, 7);
        match out {
            // 仅解出对齐前缀(48 字节),尾部 5 字节被丢弃。
            Ok(decoded) => assert_eq!(decoded, plaintext.to_vec()),
            Err(e) => panic!("nopadding fallback should succeed for unaligned data: {e}"),
        }
    }

    #[test]
    fn test_decrypt_segment_aligned_wrong_key_errors() {
        // 块对齐但 PKCS7 解密失败(错误密钥)时,必须报错而非 fallback,
        // 避免掩盖真实解密失败(F036 安全约束)。
        let key = [0x55u8; 16];
        let iv = [0x66u8; 16];
        let plaintext = b"some aligned data here padded".to_vec();
        let Some(mut ct) = encrypt_pkcs7(&plaintext, &key, &iv) else {
            panic!("test fixture encryption failed");
        };
        let wrong_key = [0x00u8; 16];
        // 用错误密钥解密块对齐数据:绝大多数情况下 PKCS7 校验失败。
        let out = decrypt_segment(&mut ct, &wrong_key, &iv, 3);
        // 不强制必然 Err(理论上极小概率出现"看似合法"的尾字节),但若 Err
        // 必须携带段索引以便诊断;此处主要验证不会 panic 且未走 NoPadding
        // 静默通过——只要是 Ok 也应解码为非原文。
        match out {
            Err(e) => assert!(
                e.to_string().contains("segment 3"),
                "error must carry segment index for diagnostics: {e}"
            ),
            Ok(decoded) => assert_ne!(decoded, plaintext),
        }
    }

    #[test]
    fn test_decrypt_segment_too_short_errors() {
        // 不足一个完整块(< 16 字节)无法解密,应返回携带段索引的错误。
        let key = [0x77u8; 16];
        let iv = [0x88u8; 16];
        let mut data = vec![0x01u8; 10];
        let out = decrypt_segment(&mut data, &key, &iv, 9);
        match out {
            Err(e) => assert!(e.to_string().contains("segment 9"), "got: {e}"),
            Ok(_) => panic!("data shorter than one AES block must error"),
        }
    }

    #[test]
    fn test_decrypt_segment_empty_is_ok_empty() {
        // BUG-HLS-EMPTY-SEGMENT-PKCS7:加密段下载到 0 字节时必须短路返回空,
        // 而非走 PKCS7 分支报 UnpadError 把整个下载当永久失败中止。
        let key = [0x12u8; 16];
        let iv = [0x34u8; 16];
        let mut data: Vec<u8> = Vec::new();
        match decrypt_segment(&mut data, &key, &iv, 0) {
            Ok(out) => assert!(out.is_empty(), "empty ciphertext must decrypt to empty"),
            Err(e) => panic!("empty input must not error: {e}"),
        }
    }

    // --- BUG-HLS-MEDIASEQ-OVERFLOW: saturating IV sequence ---

    #[test]
    fn test_compute_default_iv_saturates_no_overflow() {
        // media_sequence 接近 u64::MAX 时,序号加法必须饱和而非 panic/回绕。
        let iv = compute_default_iv(u64::MAX, 5);
        let mut expected = [0u8; 16];
        // 饱和到 u64::MAX → 低 8 字节全 0xFF。
        expected[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(iv, expected);
    }

    // --- F040: resume checkpoint parsing (backward compatibility) ---

    #[test]
    fn test_parse_resume_checkpoint_three_fields() {
        assert_eq!(parse_resume_checkpoint("5:1024:42"), (5, 1024, Some(42)));
    }

    #[test]
    fn test_parse_resume_checkpoint_two_fields_legacy() {
        // 旧格式无 media_sequence -> None。
        assert_eq!(parse_resume_checkpoint("3:512"), (3, 512, None));
    }

    #[test]
    fn test_parse_resume_checkpoint_idx_only_legacy() {
        // 更早格式仅有 idx -> byte_offset 视为 0,media_sequence 未知。
        assert_eq!(parse_resume_checkpoint("7"), (7, 0, None));
    }

    #[test]
    fn test_parse_resume_checkpoint_garbage() {
        // 完全无法解析 -> (0, 0, None),等同于不 resume。
        assert_eq!(parse_resume_checkpoint("not-a-number"), (0, 0, None));
        assert_eq!(parse_resume_checkpoint(""), (0, 0, None));
    }

    // --- F016: relative URI resolution against (redirect-final) base ---

    #[test]
    fn test_resolve_uri_cross_host_base() {
        // media playlist 重定向到 CDN 后,相对段 URI 应拼到 CDN 主机。
        assert_eq!(
            resolve_uri("https://cdn.example.com/path/media.m3u8", "seg1.ts"),
            "https://cdn.example.com/path/seg1.ts"
        );
    }

    // --- #275: concurrency selection bounds ---

    #[test]
    fn test_hls_concurrency_auto_uses_default() {
        // segment_count <= 0 means "auto": fall back to DEFAULT, but never
        // exceed the number of remaining segments.
        assert_eq!(hls_concurrency(0, 100), DEFAULT_HLS_CONCURRENCY);
        assert_eq!(hls_concurrency(-1, 100), DEFAULT_HLS_CONCURRENCY);
        assert_eq!(hls_concurrency(0, 3), 3);
    }

    #[test]
    fn test_hls_concurrency_respects_user_value() {
        assert_eq!(hls_concurrency(4, 100), 4);
        assert_eq!(hls_concurrency(1, 100), 1);
    }

    #[test]
    fn test_hls_concurrency_clamped_to_max() {
        // Never exceed the connection-pool ceiling even if the user asks for more.
        assert_eq!(hls_concurrency(999, 100), MAX_HLS_CONCURRENCY);
        assert_eq!(hls_concurrency(i32::MAX, 100), MAX_HLS_CONCURRENCY);
    }

    #[test]
    fn test_hls_concurrency_never_below_one() {
        // Even with zero remaining (shouldn't happen — guarded earlier), the
        // semaphore must be created with at least one permit.
        assert_eq!(hls_concurrency(8, 0), 1);
        assert_eq!(hls_concurrency(0, 1), 1);
    }

    #[test]
    fn test_hls_concurrency_capped_by_remaining() {
        // Spawning more workers than segments left is wasteful; cap at remaining.
        assert_eq!(hls_concurrency(16, 5), 5);
        assert_eq!(hls_concurrency(8, 2), 2);
    }
}
