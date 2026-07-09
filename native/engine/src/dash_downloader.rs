use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::downloader::{
    DB_SAVE_INTERVAL_SECS, DownloadError, DownloadParams, ProgressUpdate, TEMP_EXT,
    extract_from_url, sanitize_filename,
};
use crate::logger::log_info;
use crate::model::HlsQualityOption;
use crate::selection::SelectionOutcome;

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

fn cookies_for_url<'a>(manifest_url: &str, target_url: &str, cookies: &'a str) -> &'a str {
    if cookies.is_empty() {
        return "";
    }
    if is_same_origin(manifest_url, target_url) {
        cookies
    } else {
        ""
    }
}

const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
const QUALITY_SELECTION_TIMEOUT_SECS: u64 = 60;
const MAX_SEGMENTS: usize = 100_000;

pub fn is_dash_url(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split('#').next().unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".mpd")
}

#[derive(Clone)]
struct DashSegment {
    url: String,
    range: Option<String>,
}

struct ProgressState {
    downloaded_bytes: i64,
    last_report: std::time::Instant,
    last_db_save: std::time::Instant,
}

struct SegmentDownloadContext<'a> {
    client: &'a Client,
    cookies: &'a str,
    reference_url: &'a str,
    cancel_token: &'a tokio_util::sync::CancellationToken,
    task_id: &'a str,
    /// 浏览器扩展捕获的额外 HTTP 请求头
    extra_headers: &'a std::collections::HashMap<String, String>,
    /// 浏览器扩展捕获的页面 Referer（B站等 CDN 强制要求，缺失即 403）
    referrer: &'a str,
}

pub async fn run_dash_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = run_dash_download_inner(&params).await;

    match result {
        Ok(total) => {
            log_info!(
                "[dash-download] task {} completed, total={} bytes",
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
            log_info!("[dash-download] task {} cancelled", task_id_log);
        }
        Err(e) => {
            let msg = e.to_string();
            log_info!("[dash-download] task {} error: {}", task_id_log, msg);
            let _ = params.db.update_task_status(&params.task_id, 4, &msg).await;

            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => (t.downloaded_bytes, t.total_bytes),
                other => {
                    log_info!(
                        "[dash-download] task {} warning: failed to read progress from DB: {:?}",
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

/// Attempt to mux separate audio and video files into a single MP4 using ffmpeg.
///
/// DASH streams split audio and video into separate files.  This function
/// invokes the system's `ffmpeg` (if available) to combine them into a single
/// playable file.  If ffmpeg is not installed, returns an error — the caller
/// should fall back to keeping both files.
///
/// The muxing is done with `-c copy` (stream copy, no re-encoding) which is
/// near-instant regardless of file size.
///
/// `expected_bytes`:muxed 产物预估大小(≈ video+audio 之和,stream copy
/// 无转码)。用于 ENOSPC 预检——mux 期间 video、audio、muxed_tmp 三文件
/// 并存,峰值 ≈ 2x;空间不足时提前返回 Err,复用调用方"mux 失败保留
/// 双文件"的既有降级分支,避免 ffmpeg 中途 ENOSPC。
async fn mux_audio_video(
    video_path: &Path,
    audio_path: &Path,
    expected_bytes: u64,
    cancel_token: &tokio_util::sync::CancellationToken,
) -> Result<(), DownloadError> {
    use tokio::process::Command;

    // Build a temporary output path to avoid overwriting the video while muxing
    let muxed_tmp = video_path.with_extension("muxed.mp4");

    // ENOSPC 预检:None(网络盘/超时,无法探测)乐观放行——预检是优化,
    // 安全网是下方既有的 ffmpeg 失败清理路径。
    if let Some(parent) = video_path.parent() {
        let avail = crate::disk_space::available_space_checked(parent.to_path_buf()).await;
        if let Some(a) = avail
            && a < expected_bytes.saturating_add(crate::disk_space::PRECHECK_MARGIN)
        {
            return Err(DownloadError::Other(format!(
                "insufficient disk space for mux: need ~{expected_bytes}B + margin, have {a}B"
            )));
        }
    }

    let video_str = video_path.to_string_lossy().to_string();
    let audio_str = audio_path.to_string_lossy().to_string();
    let muxed_str = muxed_tmp.to_string_lossy().to_string();

    // Spawn ffmpeg with -c copy (stream copy, no re-encoding).
    // `.kill_on_drop(true)` ensures if we're cancelled (select! drops the
    // future), the child process is killed automatically.
    let output_fut = Command::new("ffmpeg")
        .args([
            "-y", // overwrite output without asking
            "-i",
            &video_str,
            "-i",
            &audio_str,
            "-map",
            "0:v:0", // select first video stream from first input
            "-map",
            "1:a:0", // select first audio stream from second input
            "-c",
            "copy", // stream copy, no re-encoding
            "-movflags",
            "+faststart", // web-optimized MP4
            &muxed_str,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output();

    let output: std::process::Output = tokio::select! {
        _ = cancel_token.cancelled() => {
            // The future is dropped here; kill_on_drop ensures the child is killed.
            // Clean up the partial muxed temp file.
            let _ = tokio::fs::remove_file(&muxed_tmp).await;
            return Err(DownloadError::Cancelled);
        }
        o = output_fut => o.map_err(|e| DownloadError::Other(format!("failed to run ffmpeg: {}", e)))?,
    };

    if !output.status.success() {
        // Clean up the partial muxed file
        let _ = tokio::fs::remove_file(&muxed_tmp).await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DownloadError::Other(format!(
            "ffmpeg exited with {}: {}",
            output.status,
            stderr.chars().take(500).collect::<String>()
        )));
    }

    // Replace the original video-only file with the muxed version
    if let Err(e) = tokio::fs::rename(&muxed_tmp, video_path).await {
        // rename 失败时清理临时 mux 产物,避免残留临时文件
        let _ = tokio::fs::remove_file(&muxed_tmp).await;
        return Err(DownloadError::Other(format!(
            "failed to replace video with muxed file: {}",
            e
        )));
    }

    Ok(())
}

async fn run_dash_download_inner(p: &DownloadParams) -> Result<i64, DownloadError> {
    log_info!("[dash-download] task {} starting, url={}", p.task_id, p.url);

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

    // 离散音视频轨对旁路：调用方已直接给出视频轨(p.url)+音频轨(p.audio_url)两条
    // 直链，无需（也无法）拉取 .mpd manifest 解析。直接把两条 URL 当单分段下载后 mux。
    if let Some(audio_url) = p.audio_url.as_deref()
        && !audio_url.is_empty()
    {
        return run_track_pair_inner(p, audio_url).await;
    }

    let mpd = fetch_and_parse_mpd(&p.client, &p.url, &p.cookies, &p.extra_headers).await?;
    // 多 Period DASH 尚未完全支持，仅下载首个 Period；后续 Period 被静默忽略
    // 可能导致内容不完整。这里记录警告，便于用户排查。
    if mpd.periods.len() > 1 {
        log_info!(
            "[dash-download] task {} warning: MPD contains {} Period(s), \
             多 Period DASH 未完全支持，仅下载首个 Period，后续 {} 个 Period 将被忽略",
            p.task_id,
            mpd.periods.len(),
            mpd.periods.len() - 1,
        );
    }
    let period = mpd
        .periods
        .first()
        .ok_or_else(|| DownloadError::Other("MPD contains no Period".to_string()))?;

    let video_adaptations: Vec<&dash_mpd::AdaptationSet> = period
        .adaptations
        .iter()
        .filter(|a| is_video_adaptation(a))
        .collect();
    if video_adaptations.is_empty() {
        return Err(DownloadError::Other(
            "MPD contains no video AdaptationSet".to_string(),
        ));
    }

    let best_video = select_best_adaptation(&video_adaptations)?;
    let representations = &best_video.representations;
    if representations.is_empty() {
        return Err(DownloadError::Other(
            "video AdaptationSet has no Representation".to_string(),
        ));
    }

    let repr_index = select_representation(
        &p.task_id,
        representations,
        best_video,
        p.selector.as_ref(),
        &p.cancel_token,
    )
    .await?;
    let repr = representations
        .get(repr_index)
        .ok_or_else(|| DownloadError::Other("selected representation missing".to_string()))?;

    let (video_init, video_segments) = build_segment_list(&p.url, &mpd, period, best_video, repr)?;

    if video_segments.is_empty() {
        return Err(DownloadError::Other(
            "DASH representation has no media segments".to_string(),
        ));
    }

    let auto_name = if p.file_name.is_empty() {
        let url_name = extract_from_url(&p.url).unwrap_or_else(|| "download.mpd".to_string());
        if let Some(dot_pos) = url_name.rfind('.') {
            format!("{}.mp4", &url_name[..dot_pos])
        } else {
            format!("{}.mp4", url_name)
        }
    } else {
        sanitize_filename(&p.file_name)
    };

    let save_dir = PathBuf::from(&p.save_dir);
    // 文件名由 DownloadManager 在 do_start_task 同步段统一决策（含 dedup 和
    // 兄弟任务预订协调），DASH downloader 内不再做名称变更——保留
    // p.file_name 即可，仅当为空时（兜底）使用 URL 解析结果。
    let actual_name = auto_name.clone();

    p.db.update_task_file_info(&p.task_id, &actual_name, 0)
        .await?;

    // 早期取消检查：MPD 解析完成后、创建文件之前检测 pause/delete，
    // 防止已取消的任务仍然在磁盘上创建临时文件。
    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    let _ = p.db.update_task_status(&p.task_id, 1, "").await;

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

    let mut progress_state = ProgressState {
        downloaded_bytes: 0,
        last_report: std::time::Instant::now(),
        last_db_save: std::time::Instant::now(),
    };

    let video_bytes = download_track(
        p,
        &p.url,
        &video_init,
        &video_segments,
        &dest_path,
        &mut progress_state,
    )
    .await?;

    let audio_adaptation = period.adaptations.iter().find(|a| is_audio_adaptation(a));
    let audio_bytes = if let Some(audio) = audio_adaptation {
        if audio.representations.is_empty() {
            0
        } else {
            let audio_repr = audio
                .representations
                .iter()
                .max_by_key(|r| r.bandwidth.unwrap_or(0))
                .ok_or_else(|| DownloadError::Other("audio Representation missing".to_string()))?;
            let (audio_init, audio_segments) =
                build_segment_list(&p.url, &mpd, period, audio, audio_repr)?;
            if audio_segments.is_empty() {
                0
            } else {
                let audio_path = build_audio_path(&dest_path);
                match download_track(
                    p,
                    &p.url,
                    &audio_init,
                    &audio_segments,
                    &audio_path,
                    &mut progress_state,
                )
                .await
                {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        // download_track 失败时已清理自身的 .fdownloading 临时文件;
                        // 不要在此删除最终 audio_path——rename 未发生时它尚不存在,
                        // 误删反而可能清掉上一轮遗留的同名文件。
                        return Err(e);
                    }
                }
            }
        }
    } else {
        0
    };

    // --- Audio+Video muxing ---
    // DASH streams typically have separate audio and video tracks.
    // Try to mux them into a single file using ffmpeg if available.
    // If ffmpeg is not installed, keep both files — the user can mux manually.
    // 是否最终合并为单文件：无音轨时本就是单文件；有音轨时取决于 mux 是否成功。
    let mut mux_succeeded = true;
    if audio_bytes > 0 {
        let audio_path = build_audio_path(&dest_path);
        let expected = (video_bytes + audio_bytes).max(0) as u64;
        match mux_audio_video(&dest_path, &audio_path, expected, &p.cancel_token).await {
            Ok(()) => {
                log_info!(
                    "[dash] task {} audio+video muxed successfully, cleaning up audio track",
                    p.task_id
                );
                // Remove the separate audio file after successful mux
                let _ = tokio::fs::remove_file(&audio_path).await;
            }
            Err(DownloadError::Cancelled) => {
                return Err(DownloadError::Cancelled);
            }
            Err(e) => {
                // mux 失败（ffmpeg 未安装或失败）：音频保留为独立文件。
                // 警告用户：需要 ffmpeg 才能合并，音频为独立文件。
                log_info!(
                    "[dash] task {} warning: muxing failed ({}). \
                     需要 ffmpeg 才能合并音视频；音频已保存为独立文件: {}。\
                     视频文件: {}",
                    p.task_id,
                    e,
                    audio_path.display(),
                    dest_path.display(),
                );
                // Don't fail the download — both files are valid, just not merged
                mux_succeeded = false;
            }
        }
    }

    // 上报大小（BUG-DASH-MUX-SIZE-MISMATCH）：
    //   • 已合并为单文件（无音轨 或 mux 成功）→ 以 dest_path 实际磁盘大小为准
    //     （容器重打包后与 video+audio 之和不同）。
    //   • mux 失败 → 磁盘上是 video + 独立 audio 两个文件，dest_path 仅为 video，
    //     故以 video_bytes + audio_bytes 之和上报，避免漏算 audio。
    let total = if mux_succeeded {
        match tokio::fs::metadata(&dest_path).await {
            Ok(meta) => meta.len() as i64,
            Err(_) => video_bytes + audio_bytes,
        }
    } else {
        video_bytes + audio_bytes
    };
    let _ = p.db.update_task_progress(&p.task_id, total).await;
    Ok(total)
}

/// 离散音视频轨对下载旁路：`p.url` 为视频轨、`audio_url` 为音频轨，两条均为
/// 可直接 GET 的完整轨道直链（如 DASH 分离的 m4s）。分别下载后用 ffmpeg mux
/// 合并为单 mp4。与 MPD manifest 解析路径共用 `download_track` / `mux_audio_video`
/// / `build_audio_path`，不依赖 `dash_mpd` 解析，因此对任何提供离散轨对的站点通用。
async fn run_track_pair_inner(
    p: &DownloadParams,
    audio_url: &str,
) -> Result<i64, DownloadError> {
    log_info!(
        "[dash-download] task {} track-pair mode: video={} audio={}",
        p.task_id,
        p.url,
        audio_url
    );

    // 输出文件名：轨对合并产物统一为 .mp4（视频轨常见扩展名 .m4s 不适合最终容器）。
    let auto_name = if p.file_name.is_empty() {
        let url_name = extract_from_url(&p.url).unwrap_or_else(|| "download".to_string());
        match url_name.rfind('.') {
            Some(dot) => format!("{}.mp4", &url_name[..dot]),
            None => format!("{}.mp4", url_name),
        }
    } else {
        sanitize_filename(&p.file_name)
    };

    let save_dir = PathBuf::from(&p.save_dir);
    let dest_path = save_dir.join(&auto_name);

    p.db.update_task_file_info(&p.task_id, &auto_name, 0).await?;

    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    let _ = p.db.update_task_status(&p.task_id, 1, "").await;
    let _ = p
        .progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: 0,
            status: 1,
            error_message: String::new(),
            file_name: auto_name.clone(),
            segment_details: None,
        })
        .await;

    let mut progress_state = ProgressState {
        downloaded_bytes: 0,
        last_report: std::time::Instant::now(),
        last_db_save: std::time::Instant::now(),
    };

    // 每条轨道即一个完整媒体流 → 单分段、无 init、无 range。
    let video_seg = [DashSegment {
        url: p.url.clone(),
        range: None,
    }];
    let audio_seg = [DashSegment {
        url: audio_url.to_string(),
        range: None,
    }];

    let video_bytes = download_track(
        p,
        &p.url,
        &None,
        &video_seg,
        &dest_path,
        &mut progress_state,
    )
    .await?;

    let audio_path = build_audio_path(&dest_path);
    let audio_bytes = download_track(
        p,
        audio_url,
        &None,
        &audio_seg,
        &audio_path,
        &mut progress_state,
    )
    .await?;

    // 合并两轨；ffmpeg 缺失/失败时保留双文件（与 manifest 路径一致的优雅降级）。
    let mut mux_succeeded = true;
    if audio_bytes > 0 {
        let expected = (video_bytes + audio_bytes).max(0) as u64;
        match mux_audio_video(&dest_path, &audio_path, expected, &p.cancel_token).await {
            Ok(()) => {
                log_info!(
                    "[dash] task {} track-pair muxed successfully, cleaning up audio track",
                    p.task_id
                );
                let _ = tokio::fs::remove_file(&audio_path).await;
            }
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(e) => {
                log_info!(
                    "[dash] task {} warning: track-pair muxing failed ({}). \
                     需要 ffmpeg 才能合并音视频；音频已保存为独立文件: {}。视频文件: {}",
                    p.task_id,
                    e,
                    audio_path.display(),
                    dest_path.display(),
                );
                mux_succeeded = false;
            }
        }
    }

    let total = if mux_succeeded {
        match tokio::fs::metadata(&dest_path).await {
            Ok(meta) => meta.len() as i64,
            Err(_) => video_bytes + audio_bytes,
        }
    } else {
        video_bytes + audio_bytes
    };
    let _ = p.db.update_task_progress(&p.task_id, total).await;
    Ok(total)
}

pub(crate) fn build_audio_path(video_path: &Path) -> PathBuf {
    let stem = video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("video");
    let file_name = format!("{}.audio.m4a", stem);
    match video_path.parent() {
        Some(parent) => parent.join(file_name),
        None => PathBuf::from(file_name),
    }
}

async fn fetch_and_parse_mpd(
    client: &Client,
    url: &str,
    cookies: &str,
    extra_headers: &std::collections::HashMap<String, String>,
) -> Result<dash_mpd::MPD, DownloadError> {
    let mut req = client.get(url);
    if !cookies.is_empty() {
        req = req.header("Cookie", cookies);
    }
    // 应用浏览器扩展捕获的额外请求头
    req = crate::downloader::apply_extra_headers(req, extra_headers);
    let resp = req.send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    let xml = String::from_utf8(bytes.to_vec())
        .map_err(|e| DownloadError::Other(format!("MPD utf8 error: {e}")))?;
    dash_mpd::parse(&xml).map_err(|e| DownloadError::Other(format!("MPD parse error: {e}")))
}

fn is_video_adaptation(a: &dash_mpd::AdaptationSet) -> bool {
    if let Some(ref ct) = a.contentType
        && ct.eq_ignore_ascii_case("video")
    {
        return true;
    }
    if let Some(ref mt) = a.mimeType {
        return mt.to_ascii_lowercase().starts_with("video/");
    }
    false
}

fn is_audio_adaptation(a: &dash_mpd::AdaptationSet) -> bool {
    if let Some(ref ct) = a.contentType
        && ct.eq_ignore_ascii_case("audio")
    {
        return true;
    }
    if let Some(ref mt) = a.mimeType {
        return mt.to_ascii_lowercase().starts_with("audio/");
    }
    false
}

fn select_best_adaptation<'a>(
    adaptations: &'a [&dash_mpd::AdaptationSet],
) -> Result<&'a dash_mpd::AdaptationSet, DownloadError> {
    let best = adaptations
        .iter()
        .max_by_key(|a| {
            let max_bw = a
                .representations
                .iter()
                .map(|r| r.bandwidth.unwrap_or(0))
                .max()
                .unwrap_or(0);
            (a.representations.len() as u64, max_bw)
        })
        .ok_or_else(|| DownloadError::Other("no video adaptation".to_string()))?;
    Ok(*best)
}

async fn select_representation(
    task_id: &str,
    representations: &[dash_mpd::Representation],
    adaptation: &dash_mpd::AdaptationSet,
    selector: &dyn crate::selection::HostSelection,
    cancel_token: &tokio_util::sync::CancellationToken,
) -> Result<usize, DownloadError> {
    let auto_select_best = || -> Result<usize, DownloadError> {
        let best = representations
            .iter()
            .enumerate()
            .max_by_key(|(_, r)| r.bandwidth.unwrap_or(0))
            .map(|(i, _)| i)
            .ok_or_else(|| DownloadError::Other("no representations".to_string()))?;
        log_info!(
            "[dash-download] task {} auto-selected representation index {}",
            task_id,
            best
        );
        Ok(best)
    };

    if representations.len() <= 1 {
        log_info!(
            "[dash-download] task {} only {} representation(s), skipping quality dialog",
            task_id,
            representations.len()
        );
        return auto_select_best();
    }

    let options: Vec<HlsQualityOption> = representations
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let width = r.width.or(adaptation.width).unwrap_or(0) as i64;
            let height = r.height.or(adaptation.height).unwrap_or(0) as i64;
            HlsQualityOption {
                index: i as i32,
                bandwidth: r.bandwidth.unwrap_or(0) as i64,
                width,
                height,
            }
        })
        .collect();

    log_info!(
        "[dash-download] task {} requesting quality selection ({} representations) via HostSelection (timeout={}s)",
        task_id,
        representations.len(),
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
                        "[dash-download] task {} user selected representation {}",
                        task_id, idx
                    );
                    *idx
                }
                SelectionOutcome::TimedOutDefaulted(idx) => {
                    log_info!(
                        "[dash-download] task {} quality selection timed out ({}s), defaulting to representation {}",
                        task_id, QUALITY_SELECTION_TIMEOUT_SECS, idx
                    );
                    *idx
                }
                SelectionOutcome::NoSelectorConfigured(idx) => {
                    log_info!(
                        "[dash-download] task {} no selector configured, defaulting to representation {}",
                        task_id, idx
                    );
                    *idx
                }
            };
            let _ = representations.get(idx as usize).ok_or_else(|| {
                DownloadError::Other(format!(
                    "invalid DASH quality index: {} (have {} representations)",
                    idx,
                    representations.len()
                ))
            })?;
            Ok(idx as usize)
        }
    }
}

fn resolve_url_template(
    template: &str,
    repr_id: &str,
    bandwidth: u64,
    number: Option<u64>,
    time: Option<u64>,
) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        if matches!(chars.peek(), Some('$')) {
            let _ = chars.next();
            out.push('$');
            continue;
        }

        let mut token = String::new();
        let mut closed = false;
        for nc in chars.by_ref() {
            if nc == '$' {
                closed = true;
                break;
            }
            token.push(nc);
        }

        if !closed {
            out.push('$');
            out.push_str(&token);
            break;
        }

        let (name, width) = parse_token_format(&token);
        let replacement = match name.as_str() {
            "RepresentationID" => Some(repr_id.to_string()),
            "Bandwidth" => Some(format_with_width(bandwidth, width)),
            "Number" => number.map(|v| format_with_width(v, width)),
            "Time" => time.map(|v| format_with_width(v, width)),
            _ => None,
        };

        if let Some(value) = replacement {
            out.push_str(&value);
        } else {
            out.push('$');
            out.push_str(&token);
            out.push('$');
        }
    }
    out
}

fn parse_token_format(token: &str) -> (String, Option<usize>) {
    if let Some((name, fmt)) = token.split_once('%') {
        let width = fmt
            .trim_end_matches('d')
            .trim_start_matches('0')
            .parse::<usize>()
            .ok();
        (name.to_string(), width)
    } else {
        (token.to_string(), None)
    }
}

fn format_with_width<T: std::fmt::Display>(value: T, width: Option<usize>) -> String {
    if let Some(w) = width {
        format!("{:0width$}", value, width = w)
    } else {
        value.to_string()
    }
}

fn resolve_base_url(
    mpd_url: &str,
    mpd: &dash_mpd::MPD,
    period: &dash_mpd::Period,
    adaptation: &dash_mpd::AdaptationSet,
    representation: &dash_mpd::Representation,
) -> Result<String, DownloadError> {
    let mut base = mpd_url.to_string();

    if let Some(url) = first_base(&mpd.base_url) {
        base = join_base(&base, url)?;
    }
    if let Some(url) = first_base(&period.BaseURL) {
        base = join_base(&base, url)?;
    }
    if let Some(url) = first_base(&adaptation.BaseURL) {
        base = join_base(&base, url)?;
    }
    if let Some(url) = first_base(&representation.BaseURL) {
        base = join_base(&base, url)?;
    }

    Ok(base)
}

fn first_base(urls: &[dash_mpd::BaseURL]) -> Option<&str> {
    urls.iter().map(|u| u.base.as_str()).find(|s| !s.is_empty())
}

fn join_base(current: &str, next: &str) -> Result<String, DownloadError> {
    if next.starts_with("http://") || next.starts_with("https://") {
        return Ok(next.to_string());
    }
    match url::Url::parse(current) {
        Ok(base) => match base.join(next) {
            Ok(mut joined) => {
                if joined.query().is_none()
                    && let Some(base_query) = base.query()
                    && matches!(joined.scheme(), "http" | "https")
                    && is_same_origin(current, joined.as_str())
                {
                    joined.set_query(Some(base_query));
                }
                Ok(joined.to_string())
            }
            Err(e) => Err(DownloadError::Other(format!(
                "invalid base url join: base={current}, next={next}, error={e}"
            ))),
        },
        Err(e) => Err(DownloadError::Other(format!(
            "invalid base url: base={current}, error={e}"
        ))),
    }
}

fn build_segment_list(
    mpd_url: &str,
    mpd: &dash_mpd::MPD,
    period: &dash_mpd::Period,
    adaptation: &dash_mpd::AdaptationSet,
    representation: &dash_mpd::Representation,
) -> Result<(Option<DashSegment>, Vec<DashSegment>), DownloadError> {
    let base = resolve_base_url(mpd_url, mpd, period, adaptation, representation)?;
    if let Some(template) = merged_segment_template(period, adaptation, representation) {
        let fallback_duration_secs = mpd.mediaPresentationDuration.map(|d| d.as_secs_f64());
        return build_from_template(
            &base,
            period,
            representation,
            &template,
            fallback_duration_secs,
        );
    }
    if let Some(list) = merged_segment_list(period, adaptation, representation) {
        return build_from_list(&base, &list);
    }
    Err(DownloadError::Other(
        "no SegmentTemplate or SegmentList for DASH representation".to_string(),
    ))
}

fn merged_segment_template(
    period: &dash_mpd::Period,
    adaptation: &dash_mpd::AdaptationSet,
    representation: &dash_mpd::Representation,
) -> Option<dash_mpd::SegmentTemplate> {
    let mut out = period.SegmentTemplate.clone().unwrap_or_default();
    let mut has_any = period.SegmentTemplate.is_some();

    if let Some(t) = &adaptation.SegmentTemplate {
        has_any = true;
        if t.media.is_some() {
            out.media = t.media.clone();
        }
        if t.initialization.is_some() {
            out.initialization = t.initialization.clone();
        }
        if t.timescale.is_some() {
            out.timescale = t.timescale;
        }
        if t.duration.is_some() {
            out.duration = t.duration;
        }
        if t.startNumber.is_some() {
            out.startNumber = t.startNumber;
        }
        if t.SegmentTimeline.is_some() {
            out.SegmentTimeline = t.SegmentTimeline.clone();
        }
        if t.Initialization.is_some() {
            out.Initialization = t.Initialization.clone();
        }
        if t.presentationTimeOffset.is_some() {
            out.presentationTimeOffset = t.presentationTimeOffset;
        }
    }

    if let Some(t) = &representation.SegmentTemplate {
        has_any = true;
        if t.media.is_some() {
            out.media = t.media.clone();
        }
        if t.initialization.is_some() {
            out.initialization = t.initialization.clone();
        }
        if t.timescale.is_some() {
            out.timescale = t.timescale;
        }
        if t.duration.is_some() {
            out.duration = t.duration;
        }
        if t.startNumber.is_some() {
            out.startNumber = t.startNumber;
        }
        if t.SegmentTimeline.is_some() {
            out.SegmentTimeline = t.SegmentTimeline.clone();
        }
        if t.Initialization.is_some() {
            out.Initialization = t.Initialization.clone();
        }
        if t.presentationTimeOffset.is_some() {
            out.presentationTimeOffset = t.presentationTimeOffset;
        }
    }

    if has_any { Some(out) } else { None }
}

fn merged_segment_list(
    period: &dash_mpd::Period,
    adaptation: &dash_mpd::AdaptationSet,
    representation: &dash_mpd::Representation,
) -> Option<dash_mpd::SegmentList> {
    let mut out = period.SegmentList.clone().unwrap_or_default();
    let mut has_any = period.SegmentList.is_some();

    if let Some(list) = &adaptation.SegmentList {
        has_any = true;
        if list.Initialization.is_some() {
            out.Initialization = list.Initialization.clone();
        }
        if !list.segment_urls.is_empty() {
            out.segment_urls = list.segment_urls.clone();
        }
        if list.timescale.is_some() {
            out.timescale = list.timescale;
        }
        if list.duration.is_some() {
            out.duration = list.duration;
        }
    }

    if let Some(list) = &representation.SegmentList {
        has_any = true;
        if list.Initialization.is_some() {
            out.Initialization = list.Initialization.clone();
        }
        if !list.segment_urls.is_empty() {
            out.segment_urls = list.segment_urls.clone();
        }
        if list.timescale.is_some() {
            out.timescale = list.timescale;
        }
        if list.duration.is_some() {
            out.duration = list.duration;
        }
    }

    if has_any { Some(out) } else { None }
}

fn build_from_template(
    base: &str,
    period: &dash_mpd::Period,
    representation: &dash_mpd::Representation,
    template: &dash_mpd::SegmentTemplate,
    fallback_duration_secs: Option<f64>,
) -> Result<(Option<DashSegment>, Vec<DashSegment>), DownloadError> {
    let media_tpl_str = template.media.as_deref().unwrap_or("");
    let init_tpl_str = template.initialization.as_deref().unwrap_or("");
    let needs_repr_id =
        media_tpl_str.contains("$RepresentationID$") || init_tpl_str.contains("$RepresentationID$");
    let needs_bandwidth =
        media_tpl_str.contains("$Bandwidth$") || init_tpl_str.contains("$Bandwidth$");

    if needs_repr_id && representation.id.is_none() {
        return Err(DownloadError::Other(
            "SegmentTemplate references $RepresentationID$ but Representation@id is missing"
                .to_string(),
        ));
    }
    if needs_bandwidth && representation.bandwidth.is_none() {
        return Err(DownloadError::Other(
            "SegmentTemplate references $Bandwidth$ but Representation@bandwidth is missing"
                .to_string(),
        ));
    }

    let repr_id = representation
        .id
        .clone()
        .unwrap_or_else(|| "repr".to_string());
    let bandwidth = representation.bandwidth.unwrap_or(0);

    // 正确配对 init URL 与 @range：
    // - @initialization 属性（URL 字符串）优先，但它本身不携带 @range；
    // - <Initialization> 子元素的 sourceURL 与 range 来自同一元素，可一起使用。
    // 若把 @initialization 属性 URL 与 <Initialization> 元素的 range 混用，
    // 会把不相关的 URL 和 range 组合，导致 HTTP Range 请求错误（BUG-DASH-INIT-RANGE-MIX）。
    let init_seg: Option<DashSegment> = if let Some(attr_url) = template.initialization.as_deref() {
        // @initialization 属性给出 URL，不附带 range
        let resolved = resolve_url_template(attr_url, &repr_id, bandwidth, None, None);
        let full_url = join_base(base, &resolved)?;
        Some(DashSegment {
            url: full_url,
            range: None,
        })
    } else if let Some(elem) = template.Initialization.as_ref() {
        // <Initialization> 子元素：sourceURL 与 range 来自同一元素，可配对使用
        if let Some(src_url) = elem.sourceURL.as_deref() {
            let resolved = resolve_url_template(src_url, &repr_id, bandwidth, None, None);
            let full_url = join_base(base, &resolved)?;
            Some(DashSegment {
                url: full_url,
                range: elem.range.clone(),
            })
        } else {
            None
        }
    } else {
        None
    };

    let media_template = template
        .media
        .clone()
        .ok_or_else(|| DownloadError::Other("SegmentTemplate missing media".to_string()))?;

    let start_number = template.startNumber.unwrap_or(1);

    let mut media_segments = Vec::new();

    if let Some(ref timeline) = template.SegmentTimeline {
        let mut current_time: u64 = 0;
        let mut number = start_number;
        let timescale = template.timescale.unwrap_or(1);
        let period_end_time = fallback_duration_secs
            .or_else(|| period.duration.map(|d| d.as_secs_f64()))
            .and_then(|secs| {
                if secs.is_finite() {
                    let units = (secs * timescale as f64).round();
                    if units.is_finite() && units >= 0.0 {
                        Some(units as u64)
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

        for (idx, s) in timeline.segments.iter().enumerate() {
            if s.d == 0 {
                return Err(DownloadError::Other(
                    "SegmentTimeline contains zero duration".to_string(),
                ));
            }
            let mut time_value = s.t.unwrap_or(current_time);
            let repeat = match s.r {
                None => 0u64,
                Some(r) if r >= 0 => r as u64,
                Some(-1) => {
                    let next_t = timeline.segments.get(idx + 1).and_then(|next| next.t);
                    if let Some(t) = next_t {
                        // 后续 S 元素带显式 @t：本段须精确填满 [time_value, t)。
                        // 连续时间轴中 (t - time_value) 应为 s.d 的整数倍，故用
                        // floor 计数；若用 ceil 会多生成一个越过下一段起点的
                        // 重叠/越界分段。
                        let total = t.saturating_sub(time_value);
                        if total == 0 {
                            // 区间长度为 0（畸形/重复时间戳）：跳过该 S，不生成
                            // 任何分段，避免越界伪段（F030）。
                            current_time = time_value;
                            continue;
                        }
                        (total / s.d).saturating_sub(1)
                    } else if let Some(end) = period_end_time {
                        // 末段：以 period/MPD 时长作结束时间。period_end_time 以
                        // 0 为基线，而时间轴 @t 可能以 presentationTimeOffset
                        // (PTO) 为基线；需把 PTO 加回 end_time 才能与 time_value
                        // 处于同一坐标系，否则 saturating_sub 会下溢为 0 而只生成
                        // 1 个分段，造成严重内容缺失（F028）。
                        let pto = template.presentationTimeOffset.unwrap_or(0);
                        let end_time = pto.saturating_add(end);
                        let total = end_time.saturating_sub(time_value);
                        if total == 0 {
                            // 末段 @t 恰为 period 结束（或上述下溢场景）：区间为空，
                            // 跳过该 S，消除原实现产生的越界伪分段（F030）。
                            current_time = time_value;
                            continue;
                        }
                        // 末段时长通常不是分段时长的整数倍：必须用 ceil 才能涵盖
                        // 最后一个不足整段的分段，否则视频结尾被截断（F027）。
                        // 注意此处与 @duration 分支（下方 div_ceil）语义对齐。
                        total.div_ceil(s.d).saturating_sub(1)
                    } else {
                        return Err(DownloadError::Other(
                            "cannot determine r=-1 repeat count".to_string(),
                        ));
                    }
                }
                Some(other) => {
                    return Err(DownloadError::Other(format!(
                        "unsupported SegmentTimeline repeat count: {other}"
                    )));
                }
            };

            let needed = media_segments
                .len()
                .saturating_add(repeat as usize)
                .saturating_add(1);
            if needed > MAX_SEGMENTS {
                return Err(DownloadError::Other(format!(
                    "segment count exceeds MAX_SEGMENTS ({MAX_SEGMENTS})"
                )));
            }

            for _ in 0..=repeat {
                let url = resolve_url_template(
                    &media_template,
                    &repr_id,
                    bandwidth,
                    Some(number),
                    Some(time_value),
                );
                media_segments.push(DashSegment {
                    url: join_base(base, &url)?,
                    range: None,
                });
                number += 1;
                time_value = time_value.saturating_add(s.d);
            }
            current_time = time_value;
        }
    } else if let Some(duration) = template.duration {
        if duration == 0.0 {
            return Err(DownloadError::Other(
                "SegmentTemplate@duration is zero".to_string(),
            ));
        }
        let timescale = template.timescale.unwrap_or(1);
        if timescale == 0 {
            return Err(DownloadError::Other(
                "SegmentTemplate@timescale is zero".to_string(),
            ));
        }
        let period_duration_nanos = period
            .duration
            .map(|d| d.as_nanos())
            .or_else(|| {
                fallback_duration_secs.and_then(|s| {
                    std::time::Duration::try_from_secs_f64(s)
                        .ok()
                        .map(|d| d.as_nanos())
                })
            })
            .ok_or_else(|| {
                DownloadError::Other(
                    "SegmentTemplate missing SegmentTimeline and period/MPD duration".to_string(),
                )
            })?;
        let total_units = period_duration_nanos
            .checked_mul(timescale as u128)
            .and_then(|v| v.checked_div(1_000_000_000))
            .ok_or_else(|| {
                DownloadError::Other(
                    "segment count calculation overflow (duration * timescale)".to_string(),
                )
            })?;
        // duration 为 f64（清单中常见小数，如 2.5），不能 as u128 向零截断；
        // 用浮点除法+ceil 计算段数，避免 duration=2.5 时少/多算一段（BUG-DASH-DURATION-TRUNCATION）。
        let seg_count_f = (total_units as f64 / duration).ceil();
        if !seg_count_f.is_finite() || seg_count_f < 0.0 {
            return Err(DownloadError::Other(
                "SegmentTemplate@duration yields non-finite segment count".to_string(),
            ));
        }
        let seg_count_usize = seg_count_f as usize;
        if seg_count_usize > MAX_SEGMENTS {
            return Err(DownloadError::Other(format!(
                "segment count exceeds MAX_SEGMENTS ({MAX_SEGMENTS})"
            )));
        }
        let pto = template.presentationTimeOffset.unwrap_or(0);
        for (number, n) in (start_number..).zip(0..seg_count_usize) {
            // $Time$ 按每段独立计算（而非累加），消除小数 duration 的逐段漂移：
            //   time = presentationTimeOffset + round((n - 0) * duration)
            // n 从 0 开始，number 从 startNumber 开始（BUG-DASH-DURATION-TRUNCATION）。
            let time_value = pto.saturating_add((n as f64 * duration).round() as u64);
            let url = resolve_url_template(
                &media_template,
                &repr_id,
                bandwidth,
                Some(number),
                Some(time_value),
            );
            media_segments.push(DashSegment {
                url: join_base(base, &url)?,
                range: None,
            });
        }
    } else {
        return Err(DownloadError::Other(
            "SegmentTemplate missing SegmentTimeline and duration".to_string(),
        ));
    }

    Ok((init_seg, media_segments))
}

fn build_from_list(
    base: &str,
    list: &dash_mpd::SegmentList,
) -> Result<(Option<DashSegment>, Vec<DashSegment>), DownloadError> {
    let init = list
        .Initialization
        .as_ref()
        .and_then(|i| i.sourceURL.clone())
        .map(|u| {
            Ok::<DashSegment, DownloadError>(DashSegment {
                url: join_base(base, &u)?,
                range: list.Initialization.as_ref().and_then(|i| i.range.clone()),
            })
        })
        .transpose()?;

    let mut media_segments = Vec::new();
    for seg in &list.segment_urls {
        let seg_url = if let Some(ref media) = seg.media {
            join_base(base, media)?
        } else {
            base.to_string()
        };
        media_segments.push(DashSegment {
            url: seg_url,
            range: seg.mediaRange.clone(),
        });
        if media_segments.len() > MAX_SEGMENTS {
            return Err(DownloadError::Other(format!(
                "segment count exceeds MAX_SEGMENTS ({MAX_SEGMENTS})"
            )));
        }
    }

    Ok((init, media_segments))
}

async fn download_track(
    p: &DownloadParams,
    reference_url: &str,
    init_seg: &Option<DashSegment>,
    media_segs: &[DashSegment],
    dest_path: &Path,
    progress_state: &mut ProgressState,
) -> Result<i64, DownloadError> {
    // 薄包装: 计算临时文件路径并在内部任意错误路径(取消/分段失败/flush/
    // rename)退出后统一 best-effort 异步清理 .fdownloading 临时文件,避免泄漏。
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));
    let result = download_track_inner(
        p,
        reference_url,
        init_seg,
        media_segs,
        dest_path,
        &temp_path,
        progress_state,
    )
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    result
}

async fn download_track_inner(
    p: &DownloadParams,
    reference_url: &str,
    init_seg: &Option<DashSegment>,
    media_segs: &[DashSegment],
    dest_path: &Path,
    temp_path: &Path,
    progress_state: &mut ProgressState,
) -> Result<i64, DownloadError> {
    if let Some(parent) = temp_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut file = File::create(temp_path).await?;
    let mut total_track: i64 = 0;

    let ctx = SegmentDownloadContext {
        client: &p.client,
        cookies: &p.cookies,
        reference_url,
        cancel_token: &p.cancel_token,
        task_id: &p.task_id,
        extra_headers: &p.extra_headers,
        referrer: &p.referrer,
    };

    let segment_iter = init_seg.iter().chain(media_segs.iter());

    for (idx, segment) in segment_iter.enumerate() {
        if p.cancel_token.is_cancelled() {
            file.flush().await?;
            // 使用单调写入：resume 从 0 开始重下时，不覆盖 DB 中更大的存量进度值，
            // 避免进度回退（BUG-DASH-RESUME-FULL-REDOWNLOAD）。
            let _ =
                p.db.update_task_progress_monotonic(&p.task_id, progress_state.downloaded_bytes)
                    .await;
            return Err(DownloadError::Cancelled);
        }

        let seg_bytes = match download_segment_with_retry(
            &ctx,
            &segment.url,
            segment.range.as_deref(),
            idx,
            &mut file,
            &p.speed_limiter,
        )
        .await
        {
            Ok(b) => b,
            Err(e) => {
                // 使用单调写入，同上原因（BUG-DASH-RESUME-FULL-REDOWNLOAD）。
                let _ = p
                    .db
                    .update_task_progress_monotonic(&p.task_id, progress_state.downloaded_bytes)
                    .await;
                return Err(e);
            }
        };

        total_track += seg_bytes;
        progress_state.downloaded_bytes += seg_bytes;

        if progress_state.last_report.elapsed().as_millis() >= 200 {
            let _ = p
                .progress_tx
                .send(ProgressUpdate {
                    task_id: p.task_id.clone(),
                    downloaded_bytes: progress_state.downloaded_bytes,
                    total_bytes: 0,
                    status: 1,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: None,
                })
                .await;
            progress_state.last_report = std::time::Instant::now();
        }

        if progress_state.last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
            // 使用单调写入，同上原因（BUG-DASH-RESUME-FULL-REDOWNLOAD）。
            let _ =
                p.db.update_task_progress_monotonic(&p.task_id, progress_state.downloaded_bytes)
                    .await;
            progress_state.last_db_save = std::time::Instant::now();
        }
    }

    file.flush().await?;
    drop(file);

    if tokio::fs::metadata(dest_path).await.is_ok() {
        let _ = tokio::fs::remove_file(dest_path).await;
    }

    tokio::fs::rename(temp_path, dest_path).await.map_err(|e| {
        DownloadError::Other(format!(
            "failed to rename {} -> {}: {}",
            temp_path.display(),
            dest_path.display(),
            e
        ))
    })?;

    Ok(total_track)
}

async fn download_segment_with_retry(
    ctx: &SegmentDownloadContext<'_>,
    url: &str,
    range: Option<&str>,
    seg_idx: usize,
    file: &mut File,
    speed_limiter: &crate::speed_limiter::SpeedLimiter,
) -> Result<i64, DownloadError> {
    let mut attempts = 0u32;
    loop {
        let start_pos = file
            .stream_position()
            .await
            .map_err(|e| DownloadError::Other(format!("failed to get file position: {e}")))?;

        match download_segment_streaming(ctx, url, range, file, speed_limiter).await {
            Ok(written) => return Ok(written),
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(e) => {
                file.set_len(start_pos).await?;
                file.seek(std::io::SeekFrom::Start(start_pos)).await?;

                attempts += 1;
                if attempts >= MAX_RETRIES {
                    return Err(DownloadError::Other(format!(
                        "DASH segment {} failed after {} attempts: {}",
                        seg_idx, MAX_RETRIES, e
                    )));
                }
                log_info!(
                    "[dash-download] task {} segment {} attempt {}/{} failed: {}",
                    ctx.task_id,
                    seg_idx,
                    attempts,
                    MAX_RETRIES,
                    e
                );
                let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                tokio::select! {
                    _ = ctx.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

/// Stream a single segment directly to file with speed limiting and
/// cancellation support on each chunk.  Returns bytes written.
async fn download_segment_streaming(
    ctx: &SegmentDownloadContext<'_>,
    url: &str,
    range: Option<&str>,
    file: &mut File,
    speed_limiter: &crate::speed_limiter::SpeedLimiter,
) -> Result<i64, DownloadError> {
    let safe_cookies = cookies_for_url(ctx.reference_url, url, ctx.cookies);
    let mut req = ctx.client.get(url);
    if let Some(r) = range {
        req = req.header("Range", format!("bytes={}", r));
    }
    if !safe_cookies.is_empty() {
        req = req.header("Cookie", safe_cookies);
    }
    // B站等 CDN 对 .m4s 分段强制要求 Referer（缺失即 403）。放在
    // apply_extra_headers 之前：扩展捕获的真实请求头若含 referer 会覆盖此默认值。
    if !ctx.referrer.is_empty() {
        req = req.header(reqwest::header::REFERER, ctx.referrer);
    }
    // 应用浏览器扩展捕获的额外请求头
    req = crate::downloader::apply_extra_headers(req, ctx.extra_headers);

    let resp = tokio::select! {
        _ = ctx.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
        r = req.send() => r?.error_for_status()?,
    };

    // Range 请求必须返回 206 Partial Content。若服务器/中间代理忽略 Range 返回
    // 200 全量响应，本函数会把整份资源写入文件——而这些 range 段会被拼接到同一
    // 文件，导致重复/超长内容，文件损坏且不可播放。这里显式拒绝非 206 响应，让
    // download_segment_with_retry 重试或最终失败（与 segment_coordinator.rs 的
    // 既有防护对齐）。SegmentTemplate 路径 range 恒为 None，不受影响（F029/F032）。
    if range.is_some() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(DownloadError::Other(format!(
            "DASH range segment expected 206 Partial Content, got {}",
            resp.status()
        )));
    }

    // Transparently decompress if the server returned compressed content.
    let encoding = crate::downloader::detect_content_encoding(resp.headers());
    if encoding.is_some() {
        log_info!(
            "[dash] segment decompressing Content-Encoding: {:?}",
            encoding
        );
    }

    // 提前 EOF 截断防护（F032）：仅当无 Content-Encoding（写入字节数即原始字节数）
    // 且服务器声明了 Content-Length、且非 Range 段时，记录期望字节数；EOF 后断言
    // written == expected。压缩段解压后 written 必然 != 压缩后的 content_length，
    // 故必须跳过，否则会误伤合法压缩分段。Range 段已由上方 206 校验把关，且其
    // content_length 语义为区间长度，这里不重复断言以避免与解压/区间逻辑纠缠。
    let expected_len: Option<u64> = if encoding.is_none() && range.is_none() {
        resp.content_length()
    } else {
        None
    };

    let raw_stream = resp.bytes_stream();
    let mut stream = crate::downloader::maybe_decompress_stream(raw_stream, encoding);
    let mut written: i64 = 0;

    loop {
        let chunk = tokio::select! {
            _ = ctx.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
            c = stream.next() => c,
        };
        let Some(chunk_result) = chunk else {
            break;
        };
        let chunk_data = chunk_result.map_err(DownloadError::Io)?;
        if chunk_data.is_empty() {
            continue;
        }

        let mut offset = 0usize;
        let chunk_len = chunk_data.len();
        while offset < chunk_len {
            let remaining = (chunk_len - offset) as u64;
            let allowed = tokio::select! {
                _ = ctx.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
                a = speed_limiter.consume(remaining) => a.min(remaining),
            };
            if allowed == 0 {
                tokio::task::yield_now().await;
                continue;
            }
            let end = offset + allowed as usize;
            file.write_all(&chunk_data[offset..end]).await?;
            offset = end;
        }
        written += chunk_len as i64;
    }

    // 提前 EOF 截断检测（F032）：流正常结束（stream.next() 返回 None）可能源自
    // 服务器在分段中途关闭连接（TCP RST / chunked 提前 EOF，而非 reqwest error），
    // 此时分段只写入了部分字节却被当作成功。若服务器声明了精确 Content-Length 且
    // 无压缩，写入字节数应与之相等；不等说明被截断，返回 Err 触发重试（重试前由
    // download_segment_with_retry 的 set_len(start_pos) 回退本段已写字节）。
    if let Some(expected) = expected_len
        && written as u64 != expected
    {
        return Err(DownloadError::Other(format!(
            "DASH segment truncated: wrote {written} bytes but Content-Length declared {expected}"
        )));
    }

    Ok(written)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{build_from_template, is_dash_url, resolve_url_template};

    /// 构造一个仅含 SegmentTimeline 的最小 SegmentTemplate，用于驱动
    /// build_from_template 的 r=-1 末段计数测试。
    fn template_with_timeline(
        timeline: dash_mpd::SegmentTimeline,
        timescale: Option<u64>,
        pto: Option<u64>,
    ) -> dash_mpd::SegmentTemplate {
        dash_mpd::SegmentTemplate {
            media: Some("seg-$Time$.m4s".to_string()),
            timescale,
            startNumber: Some(1),
            SegmentTimeline: Some(timeline),
            presentationTimeOffset: pto,
            ..Default::default()
        }
    }

    fn s_elem(t: Option<u64>, d: u64, r: Option<i64>) -> dash_mpd::S {
        dash_mpd::S {
            t,
            d,
            r,
            ..Default::default()
        }
    }

    /// F027: r=-1 末段计数应使用 ceil，否则丢失最后一个不足整段的分段。
    /// period_end_time=25, d=10 → floor 给 2 段（错误），ceil 给 3 段（正确）。
    #[test]
    fn test_timeline_r_minus_one_uses_ceil_for_last_segment() {
        let timeline = dash_mpd::SegmentTimeline {
            segments: vec![s_elem(Some(0), 10, Some(-1))],
        };
        let template = template_with_timeline(timeline, Some(1), None);
        let period = dash_mpd::Period::default();
        let repr = dash_mpd::Representation::default();
        let (_init, segs) = build_from_template(
            "https://cdn.example.com/base/",
            &period,
            &repr,
            &template,
            Some(25.0),
        )
        .expect("build_from_template should succeed");
        assert_eq!(segs.len(), 3, "ceil(25/10) = 3 segments expected");
    }

    /// F030: r=-1 且区间长度为 0（period_end_time == t）时不应生成任何伪分段。
    #[test]
    fn test_timeline_r_minus_one_zero_interval_produces_no_segments() {
        let timeline = dash_mpd::SegmentTimeline {
            segments: vec![s_elem(Some(25), 10, Some(-1))],
        };
        let template = template_with_timeline(timeline, Some(1), None);
        let period = dash_mpd::Period::default();
        let repr = dash_mpd::Representation::default();
        let (_init, segs) = build_from_template(
            "https://cdn.example.com/base/",
            &period,
            &repr,
            &template,
            Some(25.0),
        )
        .expect("build_from_template should succeed");
        assert!(
            segs.is_empty(),
            "zero-length interval must not produce a pseudo-segment"
        );
    }

    /// F028: 时间轴 @t 以 presentationTimeOffset 为基线时，末段结束时间须加回
    /// PTO 才能正确计数；否则 saturating_sub 下溢为 0，只生成 1（或 0）段。
    /// pto=1000, t=1000, period_end_time(相对)=30, d=10 → 对齐后 total=30,
    /// ceil(30/10)=3 段。
    #[test]
    fn test_timeline_r_minus_one_honours_presentation_time_offset() {
        let timeline = dash_mpd::SegmentTimeline {
            segments: vec![s_elem(Some(1000), 10, Some(-1))],
        };
        let template = template_with_timeline(timeline, Some(1), Some(1000));
        let period = dash_mpd::Period::default();
        let repr = dash_mpd::Representation::default();
        let (_init, segs) = build_from_template(
            "https://cdn.example.com/base/",
            &period,
            &repr,
            &template,
            Some(30.0),
        )
        .expect("build_from_template should succeed");
        assert_eq!(
            segs.len(),
            3,
            "PTO-aligned end_time should yield ceil(30/10) = 3 segments"
        );
    }

    /// 回归保护：带显式 next_t 的 r=-1 分段须用 floor（精确填满区间），避免
    /// 生成越过下一段起点的重叠/越界分段。t0=0,d=10,next_t=30 → 3 段（第一个 S），
    /// next S 自身再生成 1 段，共 4 段。
    #[test]
    fn test_timeline_r_minus_one_with_next_t_uses_floor() {
        let timeline = dash_mpd::SegmentTimeline {
            segments: vec![s_elem(Some(0), 10, Some(-1)), s_elem(Some(30), 10, None)],
        };
        let template = template_with_timeline(timeline, Some(1), None);
        let period = dash_mpd::Period::default();
        let repr = dash_mpd::Representation::default();
        let (_init, segs) = build_from_template(
            "https://cdn.example.com/base/",
            &period,
            &repr,
            &template,
            Some(100.0),
        )
        .expect("build_from_template should succeed");
        // 第一个 S: floor((30-0)/10) = 3 段; 第二个 S: 1 段 → 共 4 段。
        assert_eq!(segs.len(), 4, "floor count for explicit next_t boundary");
    }

    #[test]
    fn test_is_dash_url() {
        assert!(is_dash_url("https://example.com/manifest.mpd"));
        assert!(is_dash_url("https://example.com/manifest.MPD"));
        assert!(is_dash_url("https://example.com/manifest.mpd?token=abc"));
        assert!(is_dash_url("https://example.com/manifest.mpd#frag"));
        assert!(!is_dash_url("https://example.com/stream.m3u8"));
    }

    #[test]
    fn test_resolve_url_template_basic() {
        let out = resolve_url_template(
            "video/$RepresentationID$/$Number$.m4s",
            "v1",
            1000,
            Some(5),
            Some(10),
        );
        assert_eq!(out, "video/v1/5.m4s");
    }

    #[test]
    fn test_resolve_url_template_format() {
        let out = resolve_url_template(
            "seg-$Number%05d$-$Time%08d$.m4s",
            "v1",
            1000,
            Some(7),
            Some(42),
        );
        assert_eq!(out, "seg-00007-00000042.m4s");
    }

    #[test]
    fn test_resolve_url_template_escape() {
        let out = resolve_url_template("seg-$$-$Bandwidth$.m4s", "v1", 500, None, None);
        assert_eq!(out, "seg-$-500.m4s");
    }

    #[test]
    fn test_resolve_url_template_unclosed() {
        let out = resolve_url_template("seg-$Number", "v1", 0, Some(7), None);
        assert_eq!(out, "seg-$Number");
    }
}
