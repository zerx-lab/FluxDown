use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::downloader::{
    DB_SAVE_INTERVAL_SECS, DownloadError, DownloadParams, DownloadSpec, ProgressUpdate,
    RequestSpec, TEMP_EXT,
};
use crate::events::EventSink;
use crate::hls_downloader::{SEGMENT_HEADER_TIMEOUT, SEGMENT_IDLE_TIMEOUT, SMALL_REQUEST_TIMEOUT};
use crate::logger::log_info;
use crate::model::HlsQualityOption;
use crate::naming::{extract_from_url, sanitize_filename};
use crate::output;
use crate::selection::SelectionOutcome;
use crate::transfer_activity::{TaskRuntime, TaskSegment, TransferTracker};

fn dash_runtime(task_id: &str, state: &ProgressState) -> TaskRuntime {
    TaskRuntime {
        task_id: task_id.to_owned(),
        sampled_at_ms: chrono::Utc::now().timestamp_millis(),
        sample_sequence: crate::transfer_activity::next_sample_sequence(),
        active_transfers: Some(state.tracker.active()),
        connected_peers: None,
        parallelism_limit: Some(1),
        total_bytes: state.total_bytes,
        segments: state
            .geometry
            .iter()
            .map(|s| TaskSegment {
                index: s.index,
                start_byte: s.start_byte,
                end_byte: s.end_byte,
                downloaded_bytes: s.downloaded_bytes,
                active: s.active,
            })
            .collect(),
        source_bytes: None,
    }
}

const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
const QUALITY_SELECTION_TIMEOUT_SECS: u64 = 60;
const MAX_SEGMENTS: usize = 100_000;
/// 轨对模式：单轨超过该大小且探测到全长（= Range 能力有 206 实证）时，
/// 走 segment_coordinator 多段并发下载；否则保持单流。与 advisor 的
/// SINGLE_SEGMENT_THRESHOLD 对齐（2MB 以下拆分无收益）。
const MULTI_SEGMENT_TRACK_THRESHOLD: i64 = 2 * 1024 * 1024;

pub fn is_dash_url(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split('#').next().unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".mpd")
}

#[derive(Clone)]
struct DashSegment {
    /// 清单给出的原始地址。续传指纹只看它，不受插件地址改写影响。
    url: String,
    range: Option<String>,
    /// 仅 DASH SegmentTemplate：`$Number$` 取值。
    number: Option<u64>,
    /// 仅 DASH SegmentTemplate：`$Time$` 取值。
    time: Option<u64>,
    /// 插件 `segments` 阶段改写后的请求地址（如逐片签名）；`None` = 请求 `url`。
    rewritten: Option<String>,
}

impl DashSegment {
    /// 实际发请求用的地址。
    fn fetch_url(&self) -> &str {
        self.rewritten.as_deref().unwrap_or(&self.url)
    }
}

struct ProgressState {
    downloaded_bytes: i64,
    /// 已知总大小（0 = 未知）。track-pair 模式下为两轨 Content-Length 之和。
    total_bytes: i64,
    last_report: std::time::Instant,
    last_db_save: std::time::Instant,
    tracker: TransferTracker,
    geometry: Vec<crate::downloader::SegmentProgressInfo>,
}

/// 一次 DASH 请求发送失败的归类。
enum SendFailure {
    /// 瞬时失败（网络错误、响应头超时）：调用方按各自的重试策略处理。
    Transient(DownloadError),
    /// 不可重试：取消，或插件 `refresh` 阶段要求中止任务。
    Abort(DownloadError),
}

impl SendFailure {
    fn into_error(self) -> DownloadError {
        match self {
            Self::Transient(e) | Self::Abort(e) => e,
        }
    }
}

/// 以请求身份 `spec` 构造一次 DASH GET：以用户清单 `reference_url` 为凭据锚点，Cookie /
/// Authorization / Referer 按 [`crate::credential_scope`] 逐跳判定，按站点请求头随身份切换。
fn dash_get<'s>(
    client: &Client,
    spec: &'s RequestSpec,
    reference_url: &str,
    url: &str,
    range: Option<&str>,
) -> crate::http_request::HttpRequest<'s> {
    let req = crate::downloader::build_request(client, url, reqwest::Method::GET, spec)
        .anchor(reference_url);
    match range {
        Some(r) => req.header("Range", format!("bytes={r}")),
        None => req,
    }
}

/// 一次 DASH 运行内发请求用的上下文。请求身份（Cookie / Referer / 请求头 / 按站点
/// 请求头）全部取自运行级共享的 [`DownloadSpec`]，凭据刷新后其后的请求立即换用新身份。
struct SegmentDownloadContext<'a> {
    client: &'a Client,
    /// 运行级请求身份；被拒（401 / 403 / 410）时经插件 `refresh` 阶段换发。
    spec: &'a DownloadSpec,
    /// Cookie / 凭据类请求头的同源判定基准。
    reference_url: &'a str,
    cancel_token: &'a tokio_util::sync::CancellationToken,
    task_id: &'a str,
    /// 块级进度上报通道（200ms 节流，见 stream_segment）。
    progress_tx: &'a tokio::sync::mpsc::Sender<ProgressUpdate>,
    /// 块级 DB 进度持久化（5s 节流，单调写入防进度回退）。
    db: &'a crate::db::Db,
    sink: &'a dyn EventSink,
}

impl SegmentDownloadContext<'_> {
    /// 以身份 `used` 发出一次请求，与取消、响应头超时竞速；不判定状态码。
    async fn send_once(
        &self,
        used: &RequestSpec,
        what: &str,
        header_timeout: std::time::Duration,
        url: &str,
        range: Option<&str>,
    ) -> Result<reqwest::Response, SendFailure> {
        // 刷新改写了任务 URL 时，发往起跑 URL 的请求改走新地址。
        let routed = self.spec.route_url(url);
        let req = dash_get(
            self.client,
            used,
            self.reference_url,
            routed.as_deref().unwrap_or(url),
            range,
        );
        tokio::select! {
            _ = self.cancel_token.cancelled() => Err(SendFailure::Abort(DownloadError::Cancelled)),
            sent = tokio::time::timeout(header_timeout, req.send()) => match sent {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => Err(SendFailure::Transient(e)),
                Err(_) => Err(SendFailure::Transient(DownloadError::Other(format!(
                    "{what} stalled: no response headers within {header_timeout:?}"
                )))),
            },
        }
    }

    /// 发请求并返回响应（不判定状态码）。被拒（401 / 403 / 410）且有插件声明
    /// `refresh` 阶段时，用换发后的身份重发一次（同一请求至多一次）；无刷新器时
    /// 与直接发送完全一致。
    async fn send(
        &self,
        what: &str,
        header_timeout: std::time::Duration,
        url: &str,
        range: Option<&str>,
    ) -> Result<reqwest::Response, SendFailure> {
        let used = self.spec.current();
        let resp = self
            .send_once(&used, what, header_timeout, url, range)
            .await?;
        if resp.status().is_success() {
            self.spec.confirm(&used);
            return Ok(resp);
        }
        match self
            .spec
            .refresh_after_rejection(&used, resp.status())
            .await
            .map_err(SendFailure::Abort)?
        {
            Some(fresh) => {
                drop(resp);
                let resp = self
                    .send_once(&fresh, what, header_timeout, url, range)
                    .await?;
                if resp.status().is_success() {
                    self.spec.confirm(&fresh);
                }
                Ok(resp)
            }
            None => Ok(resp),
        }
    }
}

fn request_ctx<'a>(
    p: &'a DownloadParams,
    spec: &'a DownloadSpec,
    reference_url: &'a str,
) -> SegmentDownloadContext<'a> {
    SegmentDownloadContext {
        client: &p.client,
        spec,
        reference_url,
        cancel_token: &p.cancel_token,
        task_id: &p.task_id,
        progress_tx: &p.progress_tx,
        db: &p.db,
        sink: p.sink.as_ref(),
    }
}

pub async fn run_dash_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = match run_dash_download_inner(&params).await {
        Ok(total) => {
            // 插件后处理与分类目录复核：成功写状态 3 前执行；替换文件时只更新 total
            // （文件名与目录随后从 DB 读取）。
            match crate::downloader::finalize_output(&params, crate::intercept::ProtocolKind::Dash)
                .await
            {
                Ok(replaced) => {
                    let total = replaced.map_or(total, |(_, size)| size);
                    params
                        .db
                        .update_task_status(&params.task_id, 3, "")
                        .await
                        .map(|()| total)
                        .map_err(DownloadError::Db)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };

    match result {
        Ok(total) => {
            log_info!(
                "[dash-download] task {} completed, total={} bytes",
                task_id_log,
                total
            );

            // 完成期改名可能让最终文件名偏离起飞时的名字（占名冲突换名），
            // 以 DB 为准随完成信号上报（空串 = 保持原名）。
            let final_file_name = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => t.file_name,
                _ => String::new(),
            };
            if params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: total,
                    total_bytes: total,
                    status: 3,
                    error_message: String::new(),
                    file_name: final_file_name,
                    segment_details: None,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("download progress receiver closed");
            }
        }
        Err(DownloadError::Cancelled) => {
            log_info!("[dash-download] task {} cancelled", task_id_log);
        }
        Err(e) => {
            let msg = e.to_string();
            crate::logger::report_error("dash-download", "run task", &e);
            if let Err(db_error) = params.db.update_task_status(&params.task_id, 4, &msg).await {
                crate::logger::report_error(
                    "dash-download",
                    "persist terminal error status",
                    &db_error,
                );
            }

            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => (t.downloaded_bytes, t.total_bytes),
                other => {
                    crate::log_warn!(
                        "[dash-download] task {} warning: failed to read progress from DB: {:?}",
                        task_id_log,
                        other.err()
                    );
                    (0, 0)
                }
            };
            if params
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
                .await
                .is_err()
            {
                tracing::debug!("download progress receiver closed");
            }
        }
    }
}

/// mux 使用的 ffmpeg 路径：manager 解析注入的组件路径，缺省回退 PATH 名。
pub(crate) fn effective_ffmpeg(p: &DownloadParams) -> &Path {
    p.ffmpeg_path.as_deref().unwrap_or(Path::new("ffmpeg"))
}

/// `ffmpeg -version` 能在 5 秒内正常退出即视为可用。
///
/// 用于在下载开始前决定是否值得下载独立音轨 / 走 ffmpeg 流复制——可用性在
/// 收尾阶段才暴露会白白浪费整条音轨的流量。
pub(crate) async fn ffmpeg_usable(ffmpeg: &Path) -> bool {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    crate::proc::no_console_window(&mut cmd);
    let run = cmd
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), run).await,
        Ok(Ok(status)) if status.success()
    )
}

/// 以 `codec_args` 运行一次 ffmpeg 封装；取消或无法启动时清理 `output`。
async fn run_ffmpeg_mux_step(
    ffmpeg: &Path,
    video: &Path,
    audio: Option<&Path>,
    output: &Path,
    codec_args: &[&str],
    cancel_token: &tokio_util::sync::CancellationToken,
) -> Result<std::process::Output, DownloadError> {
    use tokio::process::Command;

    let mut cmd = Command::new(ffmpeg);
    crate::proc::no_console_window(&mut cmd);
    cmd.arg("-y").arg("-i").arg(video);
    if let Some(audio) = audio {
        cmd.arg("-i").arg(audio);
        cmd.args(["-map", "0:v:0", "-map", "1:a:0"]);
    } else {
        cmd.args(["-map", "0:v?", "-map", "0:a?"]);
    }
    cmd.args(codec_args);
    cmd.arg(output)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let output_fut = cmd.output();
    tokio::select! {
        _ = cancel_token.cancelled() => {
            if let Err(error) = tokio::fs::remove_file(output).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("dash-download", "remove_temporary_file", &error);
            }
            Err(DownloadError::Cancelled)
        }
        res = output_fut => {
            match res {
                Ok(o) => Ok(o),
                Err(e) => {
                    if let Err(error) = tokio::fs::remove_file(output).await
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        crate::logger::report_warning("dash-download", "remove_temporary_file", &error);
                    }
                    Err(DownloadError::Other(format!("failed to run ffmpeg: {e}")))
                }
            }
        }
    }
}

/// ffmpeg 流复制（`-c copy`）封装 mp4，写到 `output`；独立音轨因编码不被 mp4 容器
/// 支持而失败时，回退为视频直拷 + 音频转码 AAC 再试一次。
///
/// - `audio = Some`：取 `video` 的首个视频流与 `audio` 的首个音频流；
/// - `audio = None`：取 `video` 的全部视频与音频流（丢弃 TS 里 ffmpeg mp4 muxer
///   不支持的 data/字幕流）。
///
/// 输出格式显式指定为 mp4，`output` 可以使用任意临时扩展名。失败或取消时清理
/// `output`。`expected_bytes` 为产物预估大小，用于 ENOSPC 预检：mux 期间输入与
/// 产物并存，峰值 ≈ 2x，空间不足时提前返回 Err，由调用方走各自的降级路径。
pub(crate) async fn ffmpeg_copy_to_mp4(
    video: &Path,
    audio: Option<&Path>,
    output: &Path,
    expected_bytes: u64,
    cancel_token: &tokio_util::sync::CancellationToken,
    ffmpeg: &Path,
) -> Result<(), DownloadError> {
    if cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    // ENOSPC 预检:None(网络盘/超时,无法探测)乐观放行——预检是优化,
    // 安全网是下方既有的 ffmpeg 失败清理路径。
    if let Some(parent) = output.parent() {
        let avail = crate::disk_space::available_space_checked(parent.to_path_buf()).await;
        if let Some(a) = avail
            && a < expected_bytes.saturating_add(crate::disk_space::PRECHECK_MARGIN)
        {
            return Err(DownloadError::Other(format!(
                "insufficient disk space for mux: need ~{expected_bytes}B + margin, have {a}B"
            )));
        }
    }

    // 不加 `+faststart`:它会在封装完成后把整份 mp4 再重写一遍(搬移 moov),
    // 本地播放不需要,只徒增一倍写盘。
    let copy_args = ["-c", "copy", "-f", "mp4"];
    let first_result =
        run_ffmpeg_mux_step(ffmpeg, video, audio, output, &copy_args, cancel_token).await?;

    if first_result.status.success() {
        return Ok(());
    }

    let first_stderr = String::from_utf8_lossy(&first_result.stderr);

    // 若直接 stream copy 失败，判断是否为音频编码/标签不兼容容器导致的错误。
    // 如果存在独立音频轨且未取消，尝试将音频转码为通用 AAC（-c:v copy -c:a aac -b:a 192k），
    // 消除 QuickTime / Apple AVFoundation / Windows Media Player 无声及容器封装失败。
    let is_codec_incompatible = first_stderr.contains("Could not find tag for codec")
        || first_stderr.contains("codec not currently supported in container");

    if audio.is_some() && is_codec_incompatible && !cancel_token.is_cancelled() {
        if let Err(error) = tokio::fs::remove_file(output).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("dash-download", "remove_temporary_file", &error);
        }

        log_info!(
            "[dash] ffmpeg copy-muxing failed ({}); attempting AAC audio transcoding fallback for {}",
            first_stderr.chars().take(200).collect::<String>(),
            output.display()
        );

        let aac_fallback_args = ["-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-f", "mp4"];

        let retry_result = run_ffmpeg_mux_step(
            ffmpeg,
            video,
            audio,
            output,
            &aac_fallback_args,
            cancel_token,
        )
        .await?;

        if retry_result.status.success() {
            log_info!(
                "[dash] ffmpeg AAC transcoding fallback succeeded for {}",
                output.display()
            );
            return Ok(());
        }

        let retry_stderr = String::from_utf8_lossy(&retry_result.stderr);
        let err = std::io::Error::other(format!(
            "copy failed: {}; fallback failed: {}",
            first_stderr.chars().take(200).collect::<String>(),
            retry_stderr.chars().take(200).collect::<String>()
        ));
        crate::logger::report_warning("dash-download", "ffmpeg_mux_fallback_failed", &err);

        if let Err(error) = tokio::fs::remove_file(output).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("dash-download", "remove_temporary_file", &error);
        }

        return Err(DownloadError::Other(format!(
            "ffmpeg copy failed with {}: {}; aac fallback failed with {}: {}",
            first_result.status,
            first_stderr.chars().take(300).collect::<String>(),
            retry_result.status,
            retry_stderr.chars().take(300).collect::<String>(),
        )));
    }

    if let Err(error) = tokio::fs::remove_file(output).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::logger::report_warning("dash-download", "remove_temporary_file", &error);
    }
    Err(DownloadError::Other(format!(
        "ffmpeg exited with {}: {}",
        first_result.status,
        first_stderr.chars().take(500).collect::<String>()
    )))
}

/// Attempt to mux separate audio and video files into a single MP4 using ffmpeg.
///
/// DASH streams split audio and video into separate files.  This function
/// invokes the resolved `ffmpeg` binary (if available) to combine them into a
/// single playable file.  If ffmpeg is not installed, returns an error — the
/// caller should fall back to keeping both files.
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
    ffmpeg: &Path,
) -> Result<(), DownloadError> {
    // Build a temporary output path to avoid overwriting the video while muxing
    let muxed_tmp = video_path.with_extension("muxed.mp4");
    ffmpeg_copy_to_mp4(
        video_path,
        Some(audio_path),
        &muxed_tmp,
        expected_bytes,
        cancel_token,
        ffmpeg,
    )
    .await?;

    // Replace the original video-only file with the muxed version
    if let Err(e) = tokio::fs::rename(&muxed_tmp, video_path).await {
        // rename 失败时清理临时 mux 产物,避免残留临时文件
        if let Err(error) = tokio::fs::remove_file(&muxed_tmp).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("dash-download", "remove_temporary_file", &error);
        }
        return Err(DownloadError::Other(format!(
            "failed to replace video with muxed file: {}",
            e
        )));
    }

    Ok(())
}

async fn run_dash_download_inner(p: &DownloadParams) -> Result<i64, DownloadError> {
    log_info!("[dash-download] task {} starting, url={}", p.task_id, p.url);

    p.db.update_task_status(&p.task_id, 5, "").await?;
    if p.progress_tx
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
        .await
        .is_err()
    {
        tracing::debug!("download progress receiver closed");
    }

    // 本次运行共享的请求身份：MPD 拉取、分片请求、轨对探测与多段协调器都取自它。
    // 无插件声明 `refresh` 阶段时刷新器为 None，被拒请求按原逻辑处理。
    let spec = std::sync::Arc::new(
        DownloadSpec::new(p.spec.clone())
            .with_connection_cap(p.connection_cap)
            .with_refresher(crate::intercept::Refresher::for_params(
                p,
                crate::intercept::ProtocolKind::Dash,
            )),
    );

    // 离散音视频轨对旁路：调用方已直接给出视频轨(p.url)+音频轨(p.audio_url)两条
    // 直链，无需（也无法）拉取 .mpd manifest 解析。直接把两条 URL 当单分段下载后 mux。
    if let Some(audio_url) = p.audio_url.as_deref()
        && !audio_url.is_empty()
    {
        return run_track_pair_inner(p, &spec, audio_url).await;
    }

    // 相对 BaseURL/SegmentTemplate 必须以清单的最终 URL(重定向后)为基准解析。
    let task_ctx = crate::intercept::ctx::task_ctx(p, crate::intercept::ProtocolKind::Dash);
    let notes = crate::intercept::ActivityNotes::of(p);
    let (mpd, mpd_base_url, mpd_raw) = fetch_and_parse_mpd(p, &spec).await?;
    // 直播清单只有当前时间窗的段,按 VOD 下载会静默产出几十秒的快照。
    if mpd
        .mpdtype
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("dynamic"))
    {
        return Err(DownloadError::Other(
            "暂不支持直播（type=dynamic）的 DASH 清单".to_string(),
        ));
    }
    if !p.is_resume {
        // 全新开始:清掉上一轮遗留的续传检查点,避免误续传过期数据。
        p.db.delete_config(&dash_resume_key(&p.task_id)).await?;
    }
    // 多 Period DASH 只支持下载单个 Period（不做拼接）：默认首个，
    // 插件可通过 selectPeriod 阶段指定其它 Period。
    let period_index = pick_period(
        p.interceptor.as_ref(),
        &p.cancel_token,
        &task_ctx,
        &notes,
        &mpd,
        &mpd_raw,
    )
    .await?;
    if mpd.periods.len() > 1 {
        log_info!(
            "[dash-download] task {} MPD contains {} Period(s), 仅下载第 {} 个 Period",
            p.task_id,
            mpd.periods.len(),
            period_index + 1,
        );
    }
    let period = mpd
        .periods
        .get(period_index)
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

    let repr_index = match pick_variant(
        p.interceptor.as_ref(),
        &p.cancel_token,
        &task_ctx,
        &notes,
        best_video,
    )
    .await?
    {
        Some(i) => i,
        None => {
            select_representation(
                &p.task_id,
                representations,
                best_video,
                p.selector.as_ref(),
                &p.cancel_token,
                p.unattended,
            )
            .await?
        }
    };
    let repr = representations
        .get(repr_index)
        .ok_or_else(|| DownloadError::Other("selected representation missing".to_string()))?;

    let (video_init, video_segments) =
        build_segment_list(&mpd_base_url, &mpd, period, best_video, repr)?;
    let video_segments = filter_segments(
        p.interceptor.as_ref(),
        &p.cancel_token,
        &task_ctx,
        &notes,
        crate::intercept::ctx::SegmentTrack::Video,
        &p.url,
        period_index,
        repr.id.clone(),
        &mpd_raw,
        video_segments,
    )
    .await?;

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

    p.db.update_task_status(&p.task_id, 1, "").await?;

    if p.progress_tx
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
        .await
        .is_err()
    {
        tracing::debug!("download progress receiver closed");
    }

    let dest_path = save_dir.join(&actual_name);

    let mut progress_state = ProgressState {
        downloaded_bytes: 0,
        total_bytes: 0,
        last_report: std::time::Instant::now(),
        last_db_save: std::time::Instant::now(),
        tracker: TransferTracker::new(),
        geometry: Vec::new(),
    };

    let (video_bytes, dest_path) = download_track(
        p,
        &spec,
        &p.url,
        &video_init,
        &video_segments,
        &dest_path,
        &mut progress_state,
        Some(DashTrack::Video),
        DashTrack::Video,
    )
    .await?;

    if video_bytes > 0 {
        progress_state.geometry = vec![crate::downloader::SegmentProgressInfo {
            index: -1,
            start_byte: 0,
            end_byte: video_bytes - 1,
            downloaded_bytes: video_bytes,
            active: Some(false),
        }];
    }
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
                build_segment_list(&mpd_base_url, &mpd, period, audio, audio_repr)?;
            let audio_segments = filter_segments(
                p.interceptor.as_ref(),
                &p.cancel_token,
                &task_ctx,
                &notes,
                crate::intercept::ctx::SegmentTrack::Audio,
                &p.url,
                period_index,
                audio_repr.id.clone(),
                &mpd_raw,
                audio_segments,
            )
            .await?;
            if audio_segments.is_empty() {
                0
            } else {
                let audio_path = build_audio_path(&dest_path);
                match download_track(
                    p,
                    &spec,
                    &p.url,
                    &audio_init,
                    &audio_segments,
                    &audio_path,
                    &mut progress_state,
                    Some(DashTrack::Audio),
                    DashTrack::Audio,
                )
                .await
                {
                    Ok((bytes, _)) => bytes,
                    Err(e) => {
                        // 音频轨失败/暂停时保留其 .fdownloading 与检查点供续传;
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
    let _local_work = crate::transfer_activity::LocalWorkGuard::new(p.sink.clone());

    // --- Audio+Video muxing ---
    // DASH streams typically have separate audio and video tracks.
    // Try to mux them into a single file using ffmpeg if available.
    // If ffmpeg is not installed, keep both files — the user can mux manually.
    // 是否最终合并为单文件：无音轨时本就是单文件；有音轨时取决于 mux 是否成功。
    let mut mux_succeeded = true;
    if audio_bytes > 0 {
        let audio_path = build_audio_path(&dest_path);
        let expected = (video_bytes + audio_bytes).max(0) as u64;
        let total_downloaded = video_bytes + audio_bytes;
        if let Err(e) = p.db.update_task_status(&p.task_id, 5, "").await {
            crate::logger::report_warning("dash-download", "update_task_status_preparing", &e);
        }
        if p.progress_tx
            .send(ProgressUpdate {
                task_id: p.task_id.clone(),
                downloaded_bytes: total_downloaded,
                total_bytes: total_downloaded,
                status: 5,
                error_message: String::new(),
                file_name: String::new(),
                segment_details: None,
                ..Default::default()
            })
            .await
            .is_err()
        {
            tracing::debug!("download progress receiver closed");
        }

        match mux_audio_video(
            &dest_path,
            &audio_path,
            expected,
            &p.cancel_token,
            effective_ffmpeg(p),
        )
        .await
        {
            Ok(()) => {
                log_info!(
                    "[dash] task {} audio+video muxed successfully, cleaning up audio track",
                    p.task_id
                );
                // Remove the separate audio file after successful mux
                if let Err(error) = tokio::fs::remove_file(&audio_path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("dash-download", "remove_temporary_file", &error);
                }
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
    p.db.update_task_progress(&p.task_id, total).await?;
    if let Err(error) = p.db.delete_config(&dash_resume_key(&p.task_id)).await {
        crate::logger::report_warning("dash-download", "remove_checkpoint", &error);
    }
    Ok(total)
}

/// 轨道探测结果：全长与服务器 `Last-Modified`。
#[derive(Debug, Default)]
struct TrackProbe {
    /// `Content-Range` 全长；`None` = 未知（探测失败 / 服务器未给全长）。
    len: Option<i64>,
    /// 响应的 `Last-Modified`（空串 = 未提供或探测失败），供「使用服务器文件时间」。
    last_modified: String,
}

/// 用 `Range: bytes=0-0` 探测轨道全长（解析 `Content-Range: bytes 0-0/N` 的 N）
/// 并顺带记下 `Last-Modified`。单次尝试、失败返回空结果（调用方退化为未知大小 /
/// 无服务器时间），绝不阻塞下载主流程。
async fn probe_track(
    p: &DownloadParams,
    spec: &DownloadSpec,
    url: &str,
) -> Result<TrackProbe, DownloadError> {
    // 轨自身即同源基准：探测按该轨的请求身份发出，不做跨源凭据过滤。
    let ctx = request_ctx(p, spec, url);
    match ctx
        .send("DASH probe", SMALL_REQUEST_TIMEOUT, url, Some("0-0"))
        .await
    {
        Ok(resp) => Ok(track_probe_from_headers(resp.headers())),
        // 取消由调用方随后的取消检查甄别；插件要求中止的错误须传播为任务失败。
        Err(SendFailure::Abort(DownloadError::Cancelled)) | Err(SendFailure::Transient(_)) => {
            Ok(TrackProbe::default())
        }
        Err(SendFailure::Abort(e)) => Err(e),
    }
}

fn track_probe_from_headers(headers: &reqwest::header::HeaderMap) -> TrackProbe {
    // 形如 "bytes 0-0/240334643"；"*" 表示服务器未知全长。
    let len = headers
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next())
        .and_then(|total| total.parse::<i64>().ok())
        .filter(|n| *n > 0);
    let last_modified = headers
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    TrackProbe { len, last_modified }
}

/// 轨对模式的单轨下载调度：优先多段并发，条件不满足或多段被服务器拒绝时
/// 回退单流（既有 `download_track` 路径），保持既往行为为兜底。
///
/// 多段前提（全部满足）：
///   • `track_len = Some(n)`——`probe_track` 拿到全长即服务器已用
///     206 + Content-Range 履行过 Range 请求，Range 能力有实证；
///   • n 超过 [`MULTI_SEGMENT_TRACK_THRESHOLD`]（小轨拆分无收益）；
///   • 原始请求是 GET 族（POST + Range 未定义，禁多段）。
#[allow(clippy::too_many_arguments)]
async fn download_track_best_effort(
    p: &DownloadParams,
    spec: &std::sync::Arc<DownloadSpec>,
    url: &str,
    single_segs: &[DashSegment],
    dest_path: &Path,
    role: DashTrack,
    track_len: Option<i64>,
    progress_base: i64,
    pair_total: i64,
    progress_state: &mut ProgressState,
) -> Result<(i64, PathBuf), DownloadError> {
    if let Some(len) = track_len
        && len > MULTI_SEGMENT_TRACK_THRESHOLD
        && p.spec.is_get_like()
    {
        match download_track_coordinated(
            p,
            spec,
            url,
            dest_path,
            role,
            len,
            progress_base,
            pair_total,
        )
        .await
        {
            Ok(done) => return Ok(done),
            // 与 HTTP 路径相同的三类"多段不可行"错误 → 清理多段残留（段行 +
            // 预分配临时文件）后回退单流；其余错误（取消/IO/磁盘满等）原样上抛
            // ——取消时段行与临时文件由 coordinated 路径【保留】以支持真续传。
            Err(DownloadError::RangeNotSupported(status))
            | Err(DownloadError::VersionChanged(status))
            | Err(DownloadError::RangeMisaligned(status)) => {
                log_info!(
                    "[dash-download] task {} track multi-segment aborted ({}) — \
                     falling back to single-stream",
                    p.task_id,
                    status
                );
                p.db.delete_segments(&p.task_id).await?;
                let temp = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));
                match tokio::fs::remove_file(&temp).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(DownloadError::Io(e)),
                }
            }
            Err(e) => return Err(e),
        }
    } else {
        // C1 防线：cancel 与「探测失败」在 probe_track 里同为未知全长。
        // 暂停绝不能落进破坏性单流回退（清行 + File::create 截断 temp），
        // 先显式甄别取消。
        if p.cancel_token.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        // 本轨存在跨暂停保留的多段进度、而本次探测拿不到全长（直链过期 403/
        // 瞬时网络错）：单流回退会清行 + 截断 temp，GB 级进度全灭。fail-loud
        // 保现场——置错误态等自动重试/下次 resume 重新 resolve 拿新直链。
        // 探测成功但低于阈值/非 GET 时（track_len=Some 分支未命中）说明有
        // 权威新信息，残留行确属陈旧，清掉走单流是安全的。
        if track_len.is_none() {
            let existing = p.db.load_segments(&p.task_id).await?;
            if !existing.is_empty() {
                return Err(DownloadError::Other(format!(
                    "track probe failed with {} resumable segment row(s) retained; \
                     refusing single-stream fallback that would destroy resume state",
                    existing.len()
                )));
            }
        }
        // 单流路径不消费段行：清掉可能残留的多段行（探测成功但低于阈值/
        // 非 GET 的画质切换残留），防止暂停/重启时渲染陈旧分布。
        p.db.delete_segments(&p.task_id).await?;
    }
    download_track(
        p,
        spec,
        url,
        &None,
        single_segs,
        dest_path,
        progress_state,
        None,
        role,
    )
    .await
}

/// 用 segment_coordinator 多段并发下载一条完整轨道（IDM 式动态分段 + 分布图）。
///
/// coordinator 以【本轨相对坐标】工作（段行/Range/扩容），任务级映射经
/// [`ReportScope`] 在其发射边界完成：进度 +`progress_base`、总量覆盖为两轨
/// 合计 `pair_total`、快照/拆分事件平移并合成前序轨 100% 前缀段；
/// `owns_task_total=false` 抑制 tasks.total_bytes 被单轨长度覆盖。
///
/// 段行生命周期（真续传）：
///   • 起飞【不】清行——coordinator 的 resume-from-rows 直接续传上次暂停的
///     本轨进度（行归属由其 coverage/漂移校验兜底：若行实为另一轨/另一画质
///     的残留，尺寸不符会触发自动重建，绝不错拼字节）；
///   • 成功后清行 + rename 临时文件；
///   • 取消（暂停）【保留】行与临时文件，resume 续传；
///   • 其余错误保留现场（fail-loud + 数据保留，与 HTTP 路径一致）。
#[allow(clippy::too_many_arguments)]
async fn download_track_coordinated(
    p: &DownloadParams,
    spec: &std::sync::Arc<DownloadSpec>,
    url: &str,
    dest_path: &Path,
    role: DashTrack,
    track_len: i64,
    progress_base: i64,
    pair_total: i64,
) -> Result<(i64, PathBuf), DownloadError> {
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));

    let seg_count = if p.segment_count > 0 {
        p.segment_count
    } else {
        let advice = crate::segment_advisor::advise_static(&crate::segment_advisor::AdvisorInput {
            total_bytes: track_len,
            supports_range: true,
        });
        if p.auto_max_connections > 0 {
            advice.segments.min(p.auto_max_connections)
        } else {
            advice.segments
        }
    };
    let seg_count = if p.connection_cap > 0 {
        seg_count.min(p.connection_cap).max(1)
    } else {
        seg_count
    };
    log_info!(
        "[dash-download] task {} track multi-segment: len={}, segments={}, base={}",
        p.task_id,
        track_len,
        seg_count,
        progress_base
    );

    let scope = crate::segment_coordinator::ReportScope {
        base: progress_base,
        // 总量取「两轨探测合计」与「前序轨实际 + 本轨」的较大者：
        //   • 单侧 probe 失败（pair_total=0）→ 用后者作下界；
        //   • 前序轨实际字节 > 其探测值（重定向/就地扩容）→ 同样不越界。
        // 恒保证 downloaded ≤ total、快照前缀段不出界（绝不让 UI 超 100%）。
        total_override: pair_total.max(progress_base + track_len),
        owns_task_total: false,
        // track_len 来自本轨 206 Content-Range 分母，权威值：resume 漂移
        // 吸附与扩容检查零容差，防近长跨轨/跨画质残留行被错认（C2）。
        strict_total: true,
    };
    let result = crate::segment_coordinator::run_coordinated_download(
        &p.task_id,
        url,
        &temp_path,
        track_len,
        false, // 全长来自 206 Content-Range 分母，权威值
        seg_count,
        // DASH 轨对不聚合（P0 边界：per-节点 ALPN 例外延后），单节点池
        // 包裹任务 client，行为与现状一致。
        crate::cdn::NodePool::single(p.client.clone()),
        &p.db,
        &p.progress_tx,
        &p.cancel_token,
        &p.speed_limiter,
        spec,
        p.sink.as_ref(),
        "",
        "",
        scope,
        p.spawn_gen,
        false, // DASH 轨：段数由轨长顾问决定，不走 hint 解封
        // DASH 轨对不做 Auto 多路径（v1 边界：轨对是短分段串行流，改道收益
        // 低且与 mux 时序纠缠）；启动期先验起飞/failover 已覆盖代理选择。
        None,
        // 轨对是大文件 HTTP 分段流，额外网卡链路直接增益；规划与挂载全在
        // coordinator 内完成，与 mux 时序无关。
        p.multi_nic.clone(),
    )
    .await;

    match result {
        Ok(final_total) => {
            // 本轨完成：段行使命结束，清掉（下一轨/完成态不再引用）。
            p.db.delete_segments(&p.task_id).await?;
            let final_path = finalize_track_rename(p, &temp_path, dest_path, role).await?;
            Ok((final_total, final_path))
        }
        // 取消 = 暂停：保留段行 + 临时文件，resume 经 coordinator
        // resume-from-rows 真续传（ephemeral 直链过期无妨——resume 重新
        // resolve 拿新 URL，字节区间对同一内容仍有效）。
        Err(e) => Err(e),
    }
}

/// 轨道完成落盘：`temp` → `dest`，返回实际落盘路径。
///
/// - 视频轨是对外可见的最终文件：按 [`crate::downloader::claim_final_name`] 的
///   `create_new` 占名不覆盖同名旧文件；原名被占时 dedup 换名并把新文件名写回 DB
///   （完成信号随后上报）。覆盖授权（`p.overwrite`）或 `is_resume`（原名上的
///   文件是本任务续传遗留）时对原名删除旧文件后重试一次。
/// - 音频轨是内部 sidecar（`build_audio_path`），归属本任务：替换遗留同名文件。
async fn finalize_track_rename(
    p: &DownloadParams,
    temp: &Path,
    dest: &Path,
    role: DashTrack,
) -> Result<PathBuf, DownloadError> {
    let rename_failed = |e: std::io::Error| {
        DownloadError::Other(format!(
            "failed to rename {} -> {}: {}",
            temp.display(),
            dest.display(),
            e
        ))
    };
    if role == DashTrack::Audio {
        match tokio::fs::remove_file(dest).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(rename_failed(e)),
        }
        crate::downloader::claim_rename(temp, dest)
            .await
            .map_err(rename_failed)?;
        return Ok(dest.to_path_buf());
    }

    let (Some(save_dir), Some(name)) = (dest.parent(), dest.file_name().and_then(|n| n.to_str()))
    else {
        return Err(DownloadError::Other(format!(
            "invalid destination path: {}",
            dest.display()
        )));
    };
    let avoid: std::collections::HashSet<String> =
        p.db.list_active_sibling_file_names(&p.save_dir, &p.task_id)
            .await
            .map(|names| names.into_iter().map(|n| n.to_lowercase()).collect())
            .unwrap_or_default();
    let chosen = crate::downloader::claim_final_name(
        temp,
        save_dir,
        name,
        &if p.is_resume {
            crate::file_exists::OverwritePolicy::Any
        } else {
            p.overwrite.clone()
        },
        &avoid,
    )
    .await?;
    if chosen == name {
        return Ok(dest.to_path_buf());
    }
    log_info!(
        "[dash-download] task {} destination '{}' is taken; finalized as '{}'",
        p.task_id,
        name,
        chosen
    );
    if let Err(e) = p.db.set_task_file_name(&p.task_id, &chosen).await {
        log_info!(
            "[dash-download] task {} failed to persist renamed file '{}': {}",
            p.task_id,
            chosen,
            e
        );
    }
    Ok(save_dir.join(chosen))
}

/// 离散音视频轨对下载旁路：`p.url` 为视频轨、`audio_url` 为音频轨，两条均为
/// 可直接 GET 的完整轨道直链（如 DASH 分离的 m4s）。分别下载后用 ffmpeg mux
/// 合并为单 mp4。与 MPD manifest 解析路径共用 `download_track` / `mux_audio_video`
/// / `build_audio_path`，不依赖 `dash_mpd` 解析，因此对任何提供离散轨对的站点通用。
async fn run_track_pair_inner(
    p: &DownloadParams,
    spec: &std::sync::Arc<DownloadSpec>,
    audio_url: &str,
) -> Result<i64, DownloadError> {
    log_info!(
        "[dash-download] task {} track-pair mode: video={} audio={}",
        p.task_id,
        p.url,
        audio_url
    );

    // 持久化音频轨标记：插件惰性 resolve 产生的轨对在 create_task 时 audio_url
    // 为空，DB 无记录 → 删除路径的 sidecar 门控（load_audio_url().is_some()）
    // 永远不命中，音频文件成孤儿。此处在下载起点兜底落库（幂等 UPDATE；
    // ephemeral 链接过期无妨——删除门控只看 is_some，resume 会重新 resolve）。
    if let Err(e) = p.db.save_audio_url(&p.task_id, audio_url).await {
        log_info!(
            "[dash-download] task {} save_audio_url failed: {}（删除任务时 sidecar 清理可能失效）",
            p.task_id,
            e
        );
    }

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

    // 只更新文件名，不触碰 total_bytes：resume 再入时 total 已是有意义的
    // 轨对合计，若像旧代码那样 update_task_file_info(name, 0) 会在任一
    // re-probe 失败时把总量永久归零（C5），拖垮暂停/重启后的分布图比例尺。
    p.db.update_task_file_name(&p.task_id, &auto_name, false)
        .await?;

    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    p.db.update_task_status(&p.task_id, 1, "").await?;
    if p.progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: 0,
            status: 1,
            error_message: String::new(),
            file_name: auto_name.clone(),
            segment_details: None,
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("download progress receiver closed");
    }

    // 两轨大小：Range 0-0 探测 Content-Range 全长（googlevideo 等 CDN 通用；
    // 失败 → None = 未知，UI 退化为只显示已下载字节，不阻塞下载）。
    // 探测成功本身就是服务器履行 Range 请求（206 + Content-Range）的硬证据，
    // 下方据此决定该轨能否走多段并发；失败（配额型端点等）自动保持单流。
    let video_probe = probe_track(p, spec, &p.url).await?;
    let audio_probe = probe_track(p, spec, audio_url).await?;
    let (video_len, audio_len) = (video_probe.len, audio_probe.len);
    let total_bytes = match (video_len, audio_len) {
        (Some(v), Some(a)) => v + a,
        _ => 0,
    };
    if total_bytes > 0 {
        p.db.update_task_file_info(&p.task_id, &auto_name, total_bytes)
            .await?;
    }

    let mut progress_state = ProgressState {
        downloaded_bytes: 0,
        total_bytes,
        last_report: std::time::Instant::now(),
        last_db_save: std::time::Instant::now(),
        tracker: TransferTracker::new(),
        geometry: Vec::new(),
    };

    // 每条轨道即一个完整媒体流 → 单分段、无 init、无 range（单流回退路径用）。
    let video_seg = [DashSegment {
        url: p.url.clone(),
        range: None,
        number: None,
        time: None,
        rewritten: None,
    }];
    let audio_seg = [DashSegment {
        url: audio_url.to_string(),
        range: None,
        number: None,
        time: None,
        rewritten: None,
    }];

    // 轨级真续传：视频轨完成判定（文件系统真值）。dest 只在整轨成功后才由
    // rename 产生（单流/多段皆然），且 mux 只在两轨齐备后发生——因此
    // 「dest 存在 + 无 .fdownloading 临时文件 + 大小与本次探测精确相等」
    // 即视频轨已完成，跳过重下（暂停在音频阶段的任务 resume 不再重下 1GB
    // 视频轨）。探测失败（video_len 未知）绝不跳过——无法与 mux 产物/旧画质
    // 残留区分，宁可重下不可错拼。
    let video_temp = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));
    let video_done = match video_len {
        Some(v) => {
            !tokio::fs::try_exists(&video_temp).await.unwrap_or(false)
                && tokio::fs::metadata(&dest_path)
                    .await
                    .map(|m| m.len() as i64 == v)
                    .unwrap_or(false)
        }
        None => false,
    };

    let (video_bytes, dest_path) = if video_done {
        let v = video_len.unwrap_or(0);
        log_info!(
            "[dash-download] task {} video track already complete ({} bytes) — skipping",
            p.task_id,
            v
        );
        // 立即上报一帧，让 UI 恢复后马上显示视频轨部分的进度与分布前缀。
        if p.progress_tx
            .send(ProgressUpdate {
                task_id: p.task_id.clone(),
                downloaded_bytes: v,
                total_bytes,
                status: 1,
                error_message: String::new(),
                file_name: String::new(),
                segment_details: Some(vec![crate::downloader::SegmentProgressInfo {
                    index: -1,
                    start_byte: 0,
                    end_byte: v - 1,
                    downloaded_bytes: v,
                    active: Some(false),
                }]),
                ..Default::default()
            })
            .await
            .is_err()
        {
            tracing::debug!("download progress receiver closed");
        }
        (v, dest_path)
    } else {
        download_track_best_effort(
            p,
            spec,
            &p.url,
            &video_seg,
            &dest_path,
            DashTrack::Video,
            video_len,
            0,
            total_bytes,
            &mut progress_state,
        )
        .await?
    };
    // 多段/跳过路径不经 progress_state 累计（coordinator 自行上报），此处校准
    // 基线，保证音频轨阶段的单流进度上报包含视频轨已下字节。
    progress_state.downloaded_bytes = progress_state.downloaded_bytes.max(video_bytes);
    if video_bytes > 0 {
        progress_state.geometry = vec![crate::downloader::SegmentProgressInfo {
            index: -1,
            start_byte: 0,
            end_byte: video_bytes - 1,
            downloaded_bytes: video_bytes,
            active: Some(false),
        }];
    }

    let audio_path = build_audio_path(&dest_path);
    // 音频轨 FS 真值跳过（与视频轨对称）：mux 阶段取消的任务 resume 时，
    // 音频轨已 rename 就位（段行已清），缺此判定会整轨重下（C4/D3）。
    let audio_temp = PathBuf::from(format!("{}{}", audio_path.display(), TEMP_EXT));
    let audio_done = match audio_len {
        Some(a) => {
            !tokio::fs::try_exists(&audio_temp).await.unwrap_or(false)
                && tokio::fs::metadata(&audio_path)
                    .await
                    .map(|m| m.len() as i64 == a)
                    .unwrap_or(false)
        }
        None => false,
    };
    let audio_bytes = if audio_done {
        let a = audio_len.unwrap_or(0);
        log_info!(
            "[dash-download] task {} audio track already complete ({} bytes) — skipping",
            p.task_id,
            a
        );
        a
    } else {
        download_track_best_effort(
            p,
            spec,
            audio_url,
            &audio_seg,
            &audio_path,
            DashTrack::Audio,
            audio_len,
            video_bytes,
            total_bytes,
            &mut progress_state,
        )
        .await?
        .0
    };
    // 终态校准：任务级总大小 = 两轨实际字节合计。coordinator 的任务级写入已被
    // owns_task_total=false 门控，此处是唯一权威落点——同时兜住「探测失败、
    // 前面从未写过 total」的路径（此时 DB 里还是旧值/0）。
    p.db.update_task_total_bytes(&p.task_id, video_bytes + audio_bytes)
        .await?;
    // 终帧分布快照：把两轨合成为一个 100% 全覆盖段。音频轨走单流时不产生
    // 分段快照，宿主缓存的最后一帧仍是视频轨阶段的（只覆盖 [0, 视频轨长)），
    // 完成后分布图尾部会留灰——此帧以任务级坐标系覆盖全量，消除残留。
    let pair_actual = video_bytes + audio_bytes;
    if pair_actual > 0
        && p.progress_tx
            .send(ProgressUpdate {
                task_id: p.task_id.clone(),
                downloaded_bytes: pair_actual,
                total_bytes: pair_actual,
                status: 1,
                error_message: String::new(),
                file_name: String::new(),
                segment_details: Some(vec![crate::downloader::SegmentProgressInfo {
                    index: 0,
                    start_byte: 0,
                    end_byte: pair_actual - 1,
                    downloaded_bytes: pair_actual,
                    active: Some(false),
                }]),
                ..Default::default()
            })
            .await
            .is_err()
    {
        tracing::debug!("download progress receiver closed");
    }
    let _local_work = crate::transfer_activity::LocalWorkGuard::new(p.sink.clone());

    // 合并两轨；ffmpeg 缺失/失败时保留双文件（与 manifest 路径一致的优雅降级）。
    let mut mux_succeeded = true;
    if audio_bytes > 0 {
        let expected = (video_bytes + audio_bytes).max(0) as u64;
        let pair_actual = video_bytes + audio_bytes;
        if let Err(e) = p.db.update_task_status(&p.task_id, 5, "").await {
            crate::logger::report_warning("dash-download", "update_task_status_preparing", &e);
        }
        if p.progress_tx
            .send(ProgressUpdate {
                task_id: p.task_id.clone(),
                downloaded_bytes: pair_actual,
                total_bytes: pair_actual,
                status: 5,
                error_message: String::new(),
                file_name: String::new(),
                segment_details: None,
                ..Default::default()
            })
            .await
            .is_err()
        {
            tracing::debug!("download progress receiver closed");
        }

        match mux_audio_video(
            &dest_path,
            &audio_path,
            expected,
            &p.cancel_token,
            effective_ffmpeg(p),
        )
        .await
        {
            Ok(()) => {
                log_info!(
                    "[dash] task {} track-pair muxed successfully, cleaning up audio track",
                    p.task_id
                );
                if let Err(error) = tokio::fs::remove_file(&audio_path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("dash-download", "remove_temporary_file", &error);
                }
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

    if p.use_server_time {
        let separate_audio = audio_bytes > 0 && !mux_succeeded;
        for (path, last_modified) in track_pair_mtime_targets(
            &dest_path,
            &audio_path,
            &video_probe.last_modified,
            &audio_probe.last_modified,
            audio_bytes > 0,
            separate_audio,
        ) {
            crate::downloader::apply_server_mtime(&path, last_modified, &p.task_id).await;
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
    p.db.update_task_progress(&p.task_id, total).await?;
    Ok(total)
}

/// 轨对完成后「使用服务器文件时间」的落点：
/// - 未产生音频（`has_audio = false`）：只有视频文件，取视频轨时间；
/// - mux 成功：合并产物在 `dest`，取视频轨时间，视频轨未提供时退用音频轨；
/// - mux 失败（`separate_audio`）：两个独立文件各取本轨时间。
fn track_pair_mtime_targets<'a>(
    dest: &Path,
    audio_path: &Path,
    video_last_modified: &'a str,
    audio_last_modified: &'a str,
    has_audio: bool,
    separate_audio: bool,
) -> Vec<(PathBuf, &'a str)> {
    if separate_audio {
        return vec![
            (dest.to_path_buf(), video_last_modified),
            (audio_path.to_path_buf(), audio_last_modified),
        ];
    }
    let merged = if video_last_modified.is_empty() && has_audio {
        audio_last_modified
    } else {
        video_last_modified
    };
    vec![(dest.to_path_buf(), merged)]
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

/// `selectPeriod` 阶段：多 Period 且有插件声明时让插件选择要下载的 Period，
/// 否则（或插件未选择）取首个。
async fn pick_period(
    ic: &dyn crate::intercept::TaskInterceptor,
    cancel: &tokio_util::sync::CancellationToken,
    task: &crate::intercept::ctx::TaskCtx,
    notes: &crate::intercept::ActivityNotes,
    mpd: &dash_mpd::MPD,
    raw: &[u8],
) -> Result<usize, DownloadError> {
    use crate::intercept::{InterceptStage, ctx, drive, wants};

    if mpd.periods.len() <= 1 || !wants(ic, InterceptStage::SelectPeriod) {
        return Ok(0);
    }
    let (manifest_text, manifest_text_omitted) = ctx::manifest_text(raw);
    let periods = mpd
        .periods
        .iter()
        .enumerate()
        .map(|(index, period)| ctx::PeriodCtx {
            index,
            id: period.id.clone(),
            start_seconds: period.start.map(|d| d.as_secs_f64()),
            duration_seconds: period.duration.map(|d| d.as_secs_f64()),
            has_video: period.adaptations.iter().any(is_video_adaptation),
            has_audio: period.adaptations.iter().any(is_audio_adaptation),
        })
        .collect();
    let arg = serde_json::to_value(ctx::SelectPeriodCtx {
        task,
        manifest_text,
        manifest_text_omitted,
        periods,
    })
    .map_err(|e| DownloadError::Other(format!("构造 selectPeriod 上下文失败: {e}")))?;
    let picked = drive::run_select(
        ic,
        notes,
        cancel,
        InterceptStage::SelectPeriod,
        arg,
        mpd.periods.len(),
    )
    .await?;
    Ok(picked.unwrap_or(0))
}

/// `selectVariant` 阶段：候选为视频 AdaptationSet 的 Representation；
/// 插件未声明或未选择时返回 `None`，由调用方走原有选择逻辑。
async fn pick_variant(
    ic: &dyn crate::intercept::TaskInterceptor,
    cancel: &tokio_util::sync::CancellationToken,
    task: &crate::intercept::ctx::TaskCtx,
    notes: &crate::intercept::ActivityNotes,
    best_video: &dash_mpd::AdaptationSet,
) -> Result<Option<usize>, DownloadError> {
    use crate::intercept::{InterceptStage, ctx, drive, wants};

    if !wants(ic, InterceptStage::SelectVariant) {
        return Ok(None);
    }
    let variants: Vec<ctx::VariantCtx> = best_video
        .representations
        .iter()
        .enumerate()
        .map(|(index, r)| ctx::VariantCtx {
            index,
            bandwidth: r.bandwidth.unwrap_or(0),
            width: r.width,
            height: r.height,
            uri: r
                .BaseURL
                .first()
                .map(|b| b.base.clone())
                .unwrap_or_default(),
            id: r.id.clone(),
            codecs: r.codecs.clone(),
        })
        .collect();
    let count = variants.len();
    let arg = serde_json::to_value(ctx::SelectVariantCtx {
        task,
        format: "dash",
        variants,
    })
    .map_err(|e| DownloadError::Other(format!("构造 selectVariant 上下文失败: {e}")))?;
    Ok(drive::run_select(ic, notes, cancel, InterceptStage::SelectVariant, arg, count).await?)
}

/// `a-b` 形式的 Range 转为 `{offset, length}`；解析失败为 `None`。
fn parse_byte_range(range: &str) -> Option<crate::intercept::ctx::ByteRangeCtx> {
    let (a, b) = range.trim().split_once('-')?;
    let offset: u64 = a.trim().parse().ok()?;
    let end: u64 = b.trim().parse().ok()?;
    let length = end.checked_sub(offset)?.checked_add(1)?;
    Some(crate::intercept::ctx::ByteRangeCtx { offset, length })
}

/// `segments` 阶段：按插件的 drop 结果删除媒体分片（init 不在列表内，天然保留），
/// 并把 rewrite 结果记为分片的请求地址；其余分片保持原有顺序。
#[allow(clippy::too_many_arguments)]
async fn filter_segments(
    ic: &dyn crate::intercept::TaskInterceptor,
    cancel: &tokio_util::sync::CancellationToken,
    task: &crate::intercept::ctx::TaskCtx,
    notes: &crate::intercept::ActivityNotes,
    track: crate::intercept::ctx::SegmentTrack,
    playlist_url: &str,
    period_index: usize,
    representation_id: Option<String>,
    raw: &[u8],
    segments: Vec<DashSegment>,
) -> Result<Vec<DashSegment>, DownloadError> {
    use crate::intercept::{InterceptStage, ctx, drive, wants};

    if !wants(ic, InterceptStage::Segments) || segments.is_empty() {
        return Ok(segments);
    }
    let (manifest_text, manifest_text_omitted) = ctx::manifest_text(raw);
    let header = ctx::SegmentsHeader {
        task,
        format: "dash",
        track,
        playlist_url: playlist_url.to_string(),
        manifest_text,
        manifest_text_omitted,
        media_sequence: None,
        target_duration: None,
        period_index: Some(period_index),
        representation_id,
    };
    let items: Vec<ctx::SegmentItem> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| ctx::SegmentItem {
            id: i as u64,
            uri: s.url.clone(),
            byte_range: s.range.as_deref().and_then(parse_byte_range),
            number: s.number,
            time: s.time,
            ..Default::default()
        })
        .collect();
    let mut decision =
        drive::run_segments(ic, notes, cancel, InterceptStage::Segments, &header, &items).await?;
    if decision.dropped.is_empty() && decision.rewrites.is_empty() {
        return Ok(segments);
    }
    // 改写只写入 `rewritten`：`url` 保持清单原样，续传指纹据此计算，
    // 逐次签名不同（甚至路径不同）的地址不会让续传失效。
    Ok(segments
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !decision.dropped.contains(&(*i as u64)))
        .map(|(i, mut s)| {
            s.rewritten = decision.rewrites.remove(&(i as u64));
            s
        })
        .collect())
}

async fn fetch_and_parse_mpd(
    p: &DownloadParams,
    spec: &DownloadSpec,
) -> Result<(dash_mpd::MPD, String, bytes::Bytes), DownloadError> {
    // 请求的就是用户提交的清单 URL 本身：它同时是同源判定基准。
    let ctx = request_ctx(p, spec, &p.url);
    let resp = ctx
        .send("DASH manifest", SMALL_REQUEST_TIMEOUT, &p.url, None)
        .await
        .map_err(SendFailure::into_error)?
        .error_for_status()?;
    // 返回重定向后的最终 URL:调用方据此解析相对 BaseURL(RFC 3986 §5.1)。
    let final_url = resp.url().to_string();
    let bytes = tokio::select! {
        _ = p.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
        body = tokio::time::timeout(SMALL_REQUEST_TIMEOUT, resp.bytes()) => match body {
            Ok(body) => body?,
            Err(_) => {
                return Err(DownloadError::Other(format!(
                    "request timed out after {}s",
                    SMALL_REQUEST_TIMEOUT.as_secs()
                )));
            }
        },
    };
    let xml = String::from_utf8(bytes.to_vec())
        .map_err(|e| DownloadError::Other(format!("MPD utf8 error: {e}")))?;
    let mpd =
        dash_mpd::parse(&xml).map_err(|e| DownloadError::Other(format!("MPD parse error: {e}")))?;
    Ok((mpd, final_url, bytes))
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
    unattended: bool,
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

    // 单一码率不必问；无人值守任务（RSS/免打扰接管）也不弹——没人会来点
    // 弹窗，白等一个选择超时。两者都直接取最高码率（与超时默认值一致）。
    if representations.len() <= 1 || unattended {
        log_info!(
            "[dash-download] task {} skipping quality dialog ({} representation(s), unattended={})",
            task_id,
            representations.len(),
            unattended
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
                    && crate::credential_scope::same_origin(&base, &joined)
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
            number: None,
            time: None,
            rewritten: None,
        })
    } else if let Some(elem) = template.Initialization.as_ref() {
        // <Initialization> 子元素：sourceURL 与 range 来自同一元素，可配对使用
        if let Some(src_url) = elem.sourceURL.as_deref() {
            let resolved = resolve_url_template(src_url, &repr_id, bandwidth, None, None);
            let full_url = join_base(base, &resolved)?;
            Some(DashSegment {
                url: full_url,
                range: elem.range.clone(),
                number: None,
                time: None,
                rewritten: None,
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
                    number: Some(number),
                    time: Some(time_value),
                    rewritten: None,
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
                number: Some(number),
                time: Some(time_value),
                rewritten: None,
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
                number: None,
                time: None,
                rewritten: None,
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
            number: None,
            time: None,
            rewritten: None,
        });
        if media_segments.len() > MAX_SEGMENTS {
            return Err(DownloadError::Other(format!(
                "segment count exceeds MAX_SEGMENTS ({MAX_SEGMENTS})"
            )));
        }
    }

    Ok((init, media_segments))
}

/// MPD 路径下载的轨道标识,用于断点检查点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DashTrack {
    Video,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TrackProgress {
    /// 整轨已下载并改名到最终路径,`bytes` 为轨道字节数。
    Done { bytes: i64, fingerprint: u64 },
    /// 前 `idx` 个段(含 init)已完整落盘,累计 `bytes` 字节。
    Partial {
        idx: usize,
        bytes: i64,
        fingerprint: u64,
    },
}

impl TrackProgress {
    fn encode(&self) -> String {
        match self {
            Self::Done { bytes, fingerprint } => format!("done:{bytes}:{fingerprint:x}"),
            Self::Partial {
                idx,
                bytes,
                fingerprint,
            } => format!("seg:{idx}:{bytes}:{fingerprint:x}"),
        }
    }

    fn decode(s: &str) -> Option<Self> {
        let mut it = s.split(':');
        match it.next()? {
            "done" => Some(Self::Done {
                bytes: it.next()?.parse().ok()?,
                fingerprint: u64::from_str_radix(it.next()?, 16).ok()?,
            }),
            "seg" => Some(Self::Partial {
                idx: it.next()?.parse().ok()?,
                bytes: it.next()?.parse().ok()?,
                fingerprint: u64::from_str_radix(it.next()?, 16).ok()?,
            }),
            _ => None,
        }
    }
}

/// MPD 下载的续传检查点:`dash1;v=<进度>;a=<进度>`。一个任务要么是 HLS 要么是
/// DASH,故复用 `hls_resume_<id>` 这一任务级 config 键,任务删除时由 DB 层一并清理。
#[derive(Default, Clone, Debug, PartialEq, Eq)]
struct DashCheckpoint {
    video: Option<TrackProgress>,
    audio: Option<TrackProgress>,
}

impl DashCheckpoint {
    fn encode(&self) -> String {
        let mut out = String::from("dash1");
        if let Some(v) = &self.video {
            out.push_str(";v=");
            out.push_str(&v.encode());
        }
        if let Some(a) = &self.audio {
            out.push_str(";a=");
            out.push_str(&a.encode());
        }
        out
    }

    fn parse(s: &str) -> Self {
        let mut cp = Self::default();
        let mut parts = s.split(';');
        if parts.next() != Some("dash1") {
            return cp;
        }
        for part in parts {
            if let Some(v) = part.strip_prefix("v=") {
                cp.video = TrackProgress::decode(v);
            } else if let Some(a) = part.strip_prefix("a=") {
                cp.audio = TrackProgress::decode(a);
            }
        }
        cp
    }

    fn get(&self, track: DashTrack) -> Option<&TrackProgress> {
        match track {
            DashTrack::Video => self.video.as_ref(),
            DashTrack::Audio => self.audio.as_ref(),
        }
    }

    fn set(&mut self, track: DashTrack, progress: TrackProgress) {
        match track {
            DashTrack::Video => self.video = Some(progress),
            DashTrack::Audio => self.audio = Some(progress),
        }
    }
}

fn dash_resume_key(task_id: &str) -> String {
    format!("hls_resume_{task_id}")
}

async fn load_dash_checkpoint(p: &DownloadParams) -> Result<DashCheckpoint, DownloadError> {
    Ok(p.db
        .get_config(&dash_resume_key(&p.task_id))
        .await?
        .map(|s| DashCheckpoint::parse(&s))
        .unwrap_or_default())
}

async fn save_dash_checkpoint(
    p: &DownloadParams,
    cp: &DashCheckpoint,
) -> Result<(), DownloadError> {
    p.db.set_config(&dash_resume_key(&p.task_id), &cp.encode())
        .await?;
    Ok(())
}

/// 段列表指纹(FNV-1a):续传前比对,清单变了(段数/路径/Range)就不能沿用磁盘前缀。
/// URL 去掉 query/fragment——签名 token 每次拉取都会变,但段本身没变。
fn segments_fingerprint(init_seg: &Option<DashSegment>, media_segs: &[DashSegment]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for seg in init_seg.iter().chain(media_segs.iter()) {
        let path = seg.url.split(['?', '#']).next().unwrap_or_default();
        feed(path.as_bytes());
        feed(b"\0");
        feed(seg.range.as_deref().unwrap_or_default().as_bytes());
        feed(b"\n");
    }
    hash
}

/// `resume_track = Some(_)`(MPD 路径)时按段持久化检查点,暂停/出错后保留
/// 临时文件供续传;`None`(轨对单流回退)保持出错即清理的旧行为。
#[allow(clippy::too_many_arguments)]
async fn download_track(
    p: &DownloadParams,
    spec: &std::sync::Arc<DownloadSpec>,
    reference_url: &str,
    init_seg: &Option<DashSegment>,
    media_segs: &[DashSegment],
    dest_path: &Path,
    progress_state: &mut ProgressState,
    resume_track: Option<DashTrack>,
    role: DashTrack,
) -> Result<(i64, PathBuf), DownloadError> {
    // 薄包装: 计算临时文件路径;无检查点的轨道在内部任意错误路径(取消/分段
    // 失败/flush/rename)退出后统一 best-effort 异步清理 .fdownloading 临时文件,
    // 避免泄漏。带检查点的轨道保留它:检查点只记录已完整落盘的段,续传时
    // 会截断到该偏移,丢弃半截数据。
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));
    let result = download_track_inner(
        p,
        spec,
        reference_url,
        init_seg,
        media_segs,
        dest_path,
        &temp_path,
        progress_state,
        resume_track,
        role,
    )
    .await;
    if result.is_err()
        && resume_track.is_none()
        && let Err(error) = tokio::fs::remove_file(&temp_path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::logger::report_warning("dash-download", "remove_temporary_file", &error);
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn download_track_inner(
    p: &DownloadParams,
    spec: &std::sync::Arc<DownloadSpec>,
    reference_url: &str,
    init_seg: &Option<DashSegment>,
    media_segs: &[DashSegment],
    dest_path: &Path,
    temp_path: &Path,
    progress_state: &mut ProgressState,
    resume_track: Option<DashTrack>,
    role: DashTrack,
) -> Result<(i64, PathBuf), DownloadError> {
    output::ensure_parent(temp_path).await?;

    let fingerprint = segments_fingerprint(init_seg, media_segs);
    let total_segments = init_seg.iter().count() + media_segs.len();
    let mut checkpoint = DashCheckpoint::default();
    let mut start_idx = 0usize;
    let mut total_track: i64 = 0;
    let mut resumed_file: Option<File> = None;

    if let Some(track) = resume_track {
        checkpoint = load_dash_checkpoint(p).await?;
        match checkpoint.get(track) {
            Some(TrackProgress::Done {
                bytes,
                fingerprint: fp,
            }) if *fp == fingerprint => {
                // 整轨已完成并改名:以磁盘实际大小核对后直接跳过,避免暂停在音频
                // 阶段时重下整条视频轨。
                let on_disk = tokio::fs::metadata(dest_path)
                    .await
                    .ok()
                    .and_then(|m| i64::try_from(m.len()).ok());
                if on_disk == Some(*bytes) {
                    progress_state.downloaded_bytes += *bytes;
                    return Ok((*bytes, dest_path.to_path_buf()));
                }
            }
            Some(TrackProgress::Partial {
                idx,
                bytes,
                fingerprint: fp,
            }) if *fp == fingerprint && *idx <= total_segments && *bytes > 0 => {
                let on_disk = tokio::fs::metadata(temp_path)
                    .await
                    .ok()
                    .and_then(|m| i64::try_from(m.len()).ok())
                    .unwrap_or(0);
                // 磁盘不足检查点记录的字节数(上次崩溃丢了缓冲)时无法续传。
                if on_disk >= *bytes {
                    let mut f = tokio::fs::OpenOptions::new()
                        .write(true)
                        .open(temp_path)
                        .await?;
                    f.set_len(*bytes as u64).await?;
                    f.seek(std::io::SeekFrom::Start(*bytes as u64)).await?;
                    resumed_file = Some(f);
                    start_idx = *idx;
                    total_track = *bytes;
                    progress_state.downloaded_bytes += *bytes;
                    log_info!(
                        "[dash-download] task {} resuming track from segment {} ({} bytes)",
                        p.task_id,
                        idx,
                        bytes
                    );
                }
            }
            _ => {}
        }
    }

    let mut file = match resumed_file {
        Some(f) => f,
        None => File::create(temp_path).await?,
    };

    let ctx = request_ctx(p, spec, reference_url);

    let segment_iter = init_seg.iter().chain(media_segs.iter());

    for (idx, segment) in segment_iter.enumerate().skip(start_idx) {
        if p.cancel_token.is_cancelled() {
            file.flush().await?;
            // 使用单调写入：resume 从 0 开始重下时，不覆盖 DB 中更大的存量进度值，
            // 避免进度回退（BUG-DASH-RESUME-FULL-REDOWNLOAD）。
            p.db.update_task_progress_monotonic(&p.task_id, progress_state.downloaded_bytes)
                .await?;
            return Err(DownloadError::Cancelled);
        }

        let seg_bytes = match download_segment_with_retry(
            &ctx,
            segment.fetch_url(),
            segment.range.as_deref(),
            idx,
            &mut file,
            &p.speed_limiter,
            progress_state,
        )
        .await
        {
            Ok(b) => b,
            Err(e) => {
                // 落盘已完成的前缀,使检查点记录的字节数在续传时可信。
                if let Err(error) = file.flush().await {
                    if matches!(e, DownloadError::Cancelled) {
                        return Err(DownloadError::Io(error));
                    }
                    crate::logger::report_warning("dash-download", "flush_failed_track", &error);
                }
                // 使用单调写入，同上原因（BUG-DASH-RESUME-FULL-REDOWNLOAD）。
                if let Err(error) =
                    p.db.update_task_progress_monotonic(&p.task_id, progress_state.downloaded_bytes)
                        .await
                {
                    crate::logger::report_warning(
                        "dash-download",
                        "persist_failed_progress",
                        &error,
                    );
                }
                return Err(e);
            }
        };

        // 块内已由 stream_segment 逐 chunk 累加 progress_state，
        // 此处只累计轨道字节数，避免重复计数。
        total_track += seg_bytes;

        if let Some(track) = resume_track {
            checkpoint.set(
                track,
                TrackProgress::Partial {
                    idx: idx + 1,
                    bytes: total_track,
                    fingerprint,
                },
            );
            file.flush().await?;
            save_dash_checkpoint(p, &checkpoint).await?;
        }

        if progress_state.last_report.elapsed().as_millis() >= 200 {
            if p.progress_tx
                .send(ProgressUpdate {
                    task_id: p.task_id.clone(),
                    downloaded_bytes: progress_state.downloaded_bytes,
                    total_bytes: progress_state.total_bytes,
                    status: 1,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: if progress_state.geometry.is_empty() {
                        None
                    } else {
                        Some(progress_state.geometry.clone())
                    },
                    runtime: Some(dash_runtime(&p.task_id, progress_state)),
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("download progress receiver closed");
                return Err(DownloadError::Cancelled);
            }
            progress_state.last_report = std::time::Instant::now();
        }

        if progress_state.last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
            // 使用单调写入，同上原因（BUG-DASH-RESUME-FULL-REDOWNLOAD）。
            p.db.update_task_progress_monotonic(&p.task_id, progress_state.downloaded_bytes)
                .await?;
            progress_state.last_db_save = std::time::Instant::now();
        }
    }

    file.flush().await?;
    drop(file);

    let final_path = finalize_track_rename(p, temp_path, dest_path, role).await?;

    if let Some(track) = resume_track {
        checkpoint.set(
            track,
            TrackProgress::Done {
                bytes: total_track,
                fingerprint,
            },
        );
        save_dash_checkpoint(p, &checkpoint).await?;
    }

    Ok((total_track, final_path))
}

#[allow(clippy::too_many_arguments)]
async fn download_segment_with_retry(
    ctx: &SegmentDownloadContext<'_>,
    url: &str,
    range: Option<&str>,
    seg_idx: usize,
    file: &mut File,
    speed_limiter: &crate::speed_limiter::SpeedLimiter,
    progress_state: &mut ProgressState,
) -> Result<i64, DownloadError> {
    let mut attempts = 0u32;
    loop {
        let start_pos = file
            .stream_position()
            .await
            .map_err(|e| DownloadError::Other(format!("failed to get file position: {e}")))?;
        // 快照进度：失败重试会 set_len 回退本段已写字节，进度须同步回滚。
        let progress_snapshot = progress_state.downloaded_bytes;
        let geometry_len = progress_state.geometry.len();

        let outcome = match open_segment(ctx, url, range).await {
            Ok(resp) => stream_segment(ctx, resp, range, file, speed_limiter, progress_state).await,
            // 取消 / 插件要求中止：不是瞬时错误，不进入重试。
            Err(SendFailure::Abort(e)) => return Err(e),
            Err(SendFailure::Transient(e)) => Err(e),
        };
        match outcome {
            Ok(written) => return Ok(written),
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(DownloadError::Db(error)) => return Err(DownloadError::Db(error)),
            Err(e) => {
                file.set_len(start_pos).await?;
                file.seek(std::io::SeekFrom::Start(start_pos)).await?;
                progress_state.downloaded_bytes = progress_snapshot;
                progress_state.geometry.truncate(geometry_len);

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
                if let Err(journal_error) = crate::task_activity::record(
                    ctx.db,
                    ctx.sink,
                    ctx.task_id,
                    "retry",
                    format!(
                        "DASH 段 {seg_idx} 第 {attempts}/{MAX_RETRIES} 次尝试失败，即将重试：{e}"
                    ),
                    None,
                )
                .await
                {
                    crate::log_error!("[task-activity] failed to persist retry: {}", journal_error);
                }
                let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                tokio::select! {
                    _ = ctx.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

/// 发出分片请求并取得 2xx 响应（被拒时先经插件刷新凭据重发一次）。
async fn open_segment(
    ctx: &SegmentDownloadContext<'_>,
    url: &str,
    range: Option<&str>,
) -> Result<reqwest::Response, SendFailure> {
    // 等待响应头设上限:挂起的连接按 stalled 报错,复用段重试与 auto-retry。
    ctx.send("DASH segment", SEGMENT_HEADER_TIMEOUT, url, range)
        .await?
        .error_for_status()
        .map_err(|e| SendFailure::Transient(DownloadError::from(e)))
}

/// Stream a single segment's response directly to file with speed limiting and
/// cancellation support on each chunk.  Returns bytes written.
async fn stream_segment(
    ctx: &SegmentDownloadContext<'_>,
    resp: reqwest::Response,
    range: Option<&str>,
    file: &mut File,
    speed_limiter: &crate::speed_limiter::SpeedLimiter,
    progress_state: &mut ProgressState,
) -> Result<i64, DownloadError> {
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

    let geometry_index = progress_state.geometry.len() as i32;
    let geometry_pos = progress_state.downloaded_bytes;
    if let Some(length) = resp
        .content_length()
        .filter(|_| encoding.is_none())
        .and_then(|n| i64::try_from(n).ok())
        && length > 0
    {
        progress_state
            .geometry
            .push(crate::downloader::SegmentProgressInfo {
                index: geometry_index,
                start_byte: geometry_pos,
                end_byte: geometry_pos.saturating_add(length).saturating_sub(1),
                downloaded_bytes: 0,
                active: Some(true),
            });
    }
    let raw_stream = resp.bytes_stream();
    let mut stream = crate::downloader::maybe_decompress_stream(raw_stream, encoding);
    let mut written: i64 = 0;

    loop {
        let chunk = {
            let _transfer = progress_state.tracker.start(geometry_index);
            tokio::select! {
                _ = ctx.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
                c = tokio::time::timeout(SEGMENT_IDLE_TIMEOUT, stream.next()) => match c {
                    Ok(c) => c,
                    Err(_) => {
                        return Err(DownloadError::Other(format!(
                            "DASH segment stalled: no data for {:?}",
                            SEGMENT_IDLE_TIMEOUT
                        )));
                    }
                },
            }
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
        progress_state.downloaded_bytes += chunk_len as i64;
        if let Some(segment) = progress_state.geometry.last_mut()
            && segment.index == geometry_index
        {
            segment.downloaded_bytes = written;
        }

        // 块级进度上报（200ms 节流）：track-pair 模式整轨即单 segment，若只在
        // segment 完成后上报，UI 将全程无进度（BUG：YouTube 240MB 轨 0% 挂满全场）。
        if progress_state.last_report.elapsed().as_millis() >= 200 {
            if ctx
                .progress_tx
                .send(ProgressUpdate {
                    task_id: ctx.task_id.to_string(),
                    downloaded_bytes: progress_state.downloaded_bytes,
                    total_bytes: progress_state.total_bytes,
                    status: 1,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: if progress_state.geometry.is_empty() {
                        None
                    } else {
                        Some(progress_state.geometry.clone())
                    },
                    runtime: Some(dash_runtime(ctx.task_id, progress_state)),
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("download progress receiver closed");
                return Err(DownloadError::Cancelled);
            }
            progress_state.last_report = std::time::Instant::now();
        }
        // 块级 DB 持久化（5s 节流，单调写入防进度回退）。
        if progress_state.last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
            ctx.db
                .update_task_progress_monotonic(ctx.task_id, progress_state.downloaded_bytes)
                .await?;
            progress_state.last_db_save = std::time::Instant::now();
        }
    }

    // 提前 EOF 截断检测（F032）：流正常结束（stream.next() 返回 None）可能源自
    // 服务器在分段中途关闭连接（TCP RST / chunked 提前 EOF，而非 reqwest error），
    // 此时分段只写入了部分字节却被当作成功。若服务器声明了精确 Content-Length 且
    // 无压缩，写入字节数应与之相等；不等说明被截断，返回 Err 触发重试（重试前由
    // download_segment_with_retry 的 set_len(start_pos) 回退本段已写字节）。
    if let Some(segment) = progress_state.geometry.last_mut()
        && segment.index == geometry_index
    {
        segment.active = Some(false);
    }
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
    fn template_segments_carry_number_and_time() {
        let timeline = dash_mpd::SegmentTimeline {
            segments: vec![s_elem(Some(100), 10, Some(1)), s_elem(None, 5, None)],
        };
        let mut template = template_with_timeline(timeline, Some(1), None);
        template.startNumber = Some(7);
        let period = dash_mpd::Period::default();
        let repr = dash_mpd::Representation::default();
        let (init, segs) = build_from_template(
            "https://cdn.example.com/base/",
            &period,
            &repr,
            &template,
            None,
        )
        .expect("build_from_template should succeed");
        assert!(init.is_none());
        let got: Vec<(Option<u64>, Option<u64>)> =
            segs.iter().map(|s| (s.number, s.time)).collect();
        assert_eq!(
            got,
            vec![
                (Some(7), Some(100)),
                (Some(8), Some(110)),
                (Some(9), Some(120)),
            ]
        );
    }

    struct NullSink;
    impl crate::events::EventSink for NullSink {
        fn emit(&self, _event: crate::events::EngineEvent) {}
    }

    async fn test_notes() -> crate::intercept::ActivityNotes {
        let db = crate::db::Db::connect("sqlite::memory:").await.expect("db");
        db.insert_task(
            "t1",
            "https://example.com/a.mpd",
            "a.mp4",
            "/tmp",
            1,
            0,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert task");
        crate::intercept::ActivityNotes::new(db, std::sync::Arc::new(NullSink), "t1".to_string())
    }

    fn test_task_ctx() -> crate::intercept::ctx::TaskCtx {
        crate::intercept::ctx::TaskCtx {
            id: "t1".to_string(),
            url: "https://example.com/a.mpd".to_string(),
            source_url: "https://example.com/a.mpd".to_string(),
            file_name: "a.mp4".to_string(),
            save_dir: "/tmp".to_string(),
            protocol: crate::intercept::ProtocolKind::Dash.as_str(),
            track_pair: false,
            is_resume: false,
        }
    }

    #[tokio::test]
    async fn dash_drop_keeps_init_and_order() {
        use crate::intercept::InterceptStage;
        use crate::intercept::drive::tests::{ScriptedInterceptor, entry};

        let ic = ScriptedInterceptor::new(vec![entry("a@b", &[InterceptStage::Segments], false)])
            .reply("a@b", InterceptStage::Segments, Ok(Some(r#"{"drop":[1]}"#)));
        let notes = test_notes().await;
        let init = seg("https://cdn.example.com/init.mp4", None);
        let media = vec![
            seg("https://cdn.example.com/s0.m4s", Some("0-99")),
            seg("https://cdn.example.com/s1.m4s", None),
            seg("https://cdn.example.com/s2.m4s", None),
        ];
        let kept = super::filter_segments(
            &ic,
            &tokio_util::sync::CancellationToken::new(),
            &test_task_ctx(),
            &notes,
            crate::intercept::ctx::SegmentTrack::Video,
            "https://example.com/a.mpd",
            0,
            Some("v1".to_string()),
            b"<MPD/>",
            media,
        )
        .await
        .expect("filter");
        let urls: Vec<&str> = kept.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://cdn.example.com/s0.m4s",
                "https://cdn.example.com/s2.m4s"
            ]
        );
        // init 分片独立于媒体列表，不参与过滤。
        assert_eq!(init.url, "https://cdn.example.com/init.mp4");

        let seen = ic.seen.lock().expect("seen lock");
        let (_, _, ctx) = &seen[0];
        assert_eq!(ctx["format"], "dash");
        assert_eq!(ctx["track"], "video");
        assert_eq!(ctx["representationId"], "v1");
        assert_eq!(ctx["periodIndex"], 0);
        assert_eq!(ctx["segments"][0]["id"], 0);
        assert_eq!(ctx["segments"][0]["byteRange"]["offset"], 0);
        assert_eq!(ctx["segments"][0]["byteRange"]["length"], 100);
        assert!(ctx["segments"][1]["byteRange"].is_null());
    }

    #[tokio::test]
    async fn period_selection_defaults_to_first() {
        use crate::intercept::InterceptStage;
        use crate::intercept::drive::tests::{ScriptedInterceptor, entry};

        let mpd = dash_mpd::MPD {
            periods: vec![dash_mpd::Period::default(), dash_mpd::Period::default()],
            ..Default::default()
        };
        let notes = test_notes().await;
        let cancel = tokio_util::sync::CancellationToken::new();
        let task = test_task_ctx();

        // 无插件：默认首个。
        let none = crate::intercept::noop();
        let idx = super::pick_period(none.as_ref(), &cancel, &task, &notes, &mpd, b"")
            .await
            .expect("pick");
        assert_eq!(idx, 0);

        // 插件未返回 select：仍为首个。
        let silent =
            ScriptedInterceptor::new(vec![entry("a@b", &[InterceptStage::SelectPeriod], false)]);
        let idx = super::pick_period(&silent, &cancel, &task, &notes, &mpd, b"")
            .await
            .expect("pick");
        assert_eq!(idx, 0);

        // 插件选择第二个。
        let chooser =
            ScriptedInterceptor::new(vec![entry("a@b", &[InterceptStage::SelectPeriod], false)])
                .reply(
                    "a@b",
                    InterceptStage::SelectPeriod,
                    Ok(Some(r#"{"select":1}"#)),
                );
        let idx = super::pick_period(&chooser, &cancel, &task, &notes, &mpd, b"<MPD/>")
            .await
            .expect("pick");
        assert_eq!(idx, 1);
    }

    type Handler = std::sync::Arc<dyn Fn(&str) -> (u16, Vec<u8>) + Send + Sync>;
    type Hits = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// 请求行里的目标（路径 + 查询）。
    fn request_target(request: &str) -> &str {
        request.split_whitespace().nth(1).unwrap_or_default()
    }

    /// 按 `handler(原始请求)` 返回 `(状态码, body)` 的本地服务器，记录收到的原始请求。
    async fn serve(handler: Handler) -> (std::net::SocketAddr, Hits) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let address = listener.local_addr().expect("local addr");
        let hits: Hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&hits);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let handler = std::sync::Arc::clone(&handler);
                let log = std::sync::Arc::clone(&log);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let (status, body) = handler(&request);
                    log.lock().expect("hits lock").push(request);
                    let reason = match status {
                        200 => "OK",
                        403 => "Forbidden",
                        _ => "Not Found",
                    };
                    let mut response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    response.extend(body);
                    if let Err(error) = socket.write_all(&response).await {
                        tracing::debug!(%error, "DASH fixture client disconnected");
                    }
                });
            }
        });
        (address, hits)
    }

    fn hit_targets(hits: &Hits) -> Vec<String> {
        hits.lock()
            .expect("hits lock")
            .iter()
            .map(|r| request_target(r).to_string())
            .collect()
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_dash_{tag}_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create DASH fixture directory");
        dir
    }

    /// 把进度通道排空，避免下载器的进度上报在有界通道上阻塞。
    fn drained_progress() -> tokio::sync::mpsc::Sender<crate::downloader::ProgressUpdate> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        tx
    }

    fn dash_params(
        url: &str,
        dir: &std::path::Path,
        db: &crate::db::Db,
        interceptor: std::sync::Arc<dyn crate::intercept::TaskInterceptor>,
        is_resume: bool,
    ) -> crate::downloader::DownloadParams {
        crate::downloader::DownloadParams {
            spawn_gen: 1,
            unattended: true,
            preprobed: None,
            interceptor,
            connection_cap: 0,
            run_meta: crate::intercept::RunMeta::default(),
            auto_proxy: None,
            multi_nic: None,
            task_id: "t-dash".to_string(),
            url: url.to_string(),
            save_dir: dir.to_string_lossy().to_string(),
            file_name: "out.mp4".to_string(),
            segment_count: 0,
            is_resume,
            range_verified: true,
            db: db.clone(),
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("client"),
            progress_tx: drained_progress(),
            cancel_token: tokio_util::sync::CancellationToken::new(),
            sink: std::sync::Arc::new(NullSink),
            cookies: String::new(),
            referrer: String::new(),
            speed_limiter: crate::speed_limiter::SpeedLimiter::new(0),
            hint_file_size: 0,
            proxy_config: crate::proxy_config::ProxyConfig::default(),
            selector: std::sync::Arc::new(crate::NoopSelection),
            checksum: String::new(),
            extra_headers: std::collections::HashMap::new(),
            spec: crate::downloader::RequestSpec::empty_get(),
            audio_url: None,
            auto_max_connections: 0,
            use_server_time: false,
            overwrite: crate::file_exists::OverwritePolicy::Never,
            ffmpeg_path: None,
            cdn: crate::cdn::CdnTaskInput::default(),
        }
    }

    async fn dash_db(dir: &std::path::Path) -> crate::db::Db {
        let db = crate::db::Db::connect("sqlite::memory:").await.expect("db");
        db.insert_task(
            "t-dash",
            "http://127.0.0.1/m.mpd",
            "out.mp4",
            &dir.to_string_lossy(),
            0,
            0,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert task");
        db
    }

    fn fresh_progress() -> super::ProgressState {
        super::ProgressState {
            downloaded_bytes: 0,
            total_bytes: 0,
            last_report: std::time::Instant::now(),
            last_db_save: std::time::Instant::now(),
            tracker: super::TransferTracker::new(),
            geometry: Vec::new(),
        }
    }

    /// 与 `run_dash_download_inner` 相同的运行级请求身份。
    fn run_spec(p: &crate::downloader::DownloadParams) -> std::sync::Arc<super::DownloadSpec> {
        std::sync::Arc::new(super::DownloadSpec::new(p.spec.clone()).with_refresher(
            crate::intercept::Refresher::for_params(p, crate::intercept::ProtocolKind::Dash),
        ))
    }

    /// 按 `id` 顺序给每个分片返回改写地址的 `segments` 阶段，对清单分片做过滤。
    async fn rewritten_run(
        address: std::net::SocketAddr,
        prefix: &str,
        sig: &str,
    ) -> Vec<DashSegment> {
        use crate::intercept::InterceptStage;
        use crate::intercept::drive::tests::{ScriptedInterceptor, entry};

        let reply = format!(
            r#"{{"rewrite":[{{"id":0,"uri":"http://{address}/{prefix}/s0.m4s?sig={sig}"}},{{"id":1,"uri":"http://{address}/{prefix}/s1.m4s?sig={sig}"}}]}}"#
        );
        let ic = ScriptedInterceptor::new(vec![entry("a@b", &[InterceptStage::Segments], false)])
            .reply("a@b", InterceptStage::Segments, Ok(Some(&reply)));
        let notes = test_notes().await;
        super::filter_segments(
            &ic,
            &tokio_util::sync::CancellationToken::new(),
            &test_task_ctx(),
            &notes,
            crate::intercept::ctx::SegmentTrack::Video,
            &format!("http://{address}/m.mpd"),
            0,
            Some("v1".to_string()),
            b"<MPD/>",
            manifest_segments(address),
        )
        .await
        .expect("filter")
    }

    fn manifest_segments(address: std::net::SocketAddr) -> Vec<DashSegment> {
        vec![
            seg(&format!("http://{address}/s0.m4s"), None),
            seg(&format!("http://{address}/s1.m4s"), None),
        ]
    }

    #[tokio::test]
    async fn dash_rewrite_keeps_resume_fingerprint() {
        let handler: Handler = std::sync::Arc::new(|request| match request_target(request) {
            "/a/s0.m4s?sig=1" => (200, b"AAAA".to_vec()),
            "/a/s1.m4s?sig=1" => (200, b"BBBB".to_vec()),
            _ => (404, Vec::new()),
        });
        let (address, hits) = serve(handler).await;

        // 同一清单两次运行：改写出不同签名、甚至不同路径的地址。
        let run_a = rewritten_run(address, "a", "1").await;
        let run_b = rewritten_run(address, "b", "2").await;
        let plain = manifest_segments(address);
        assert_ne!(run_a[0].fetch_url(), run_b[0].fetch_url());
        assert_eq!(
            run_a[0].fetch_url(),
            format!("http://{address}/a/s0.m4s?sig=1")
        );
        assert_eq!(run_a[0].url, plain[0].url, "清单原始地址保持不变");
        let fingerprint = |segs: &[DashSegment]| segments_fingerprint(&None, segs);
        assert_eq!(fingerprint(&run_a), fingerprint(&plain));
        assert_eq!(fingerprint(&run_a), fingerprint(&run_b));

        let dir = scratch_dir("rewrite_resume");
        let db = dash_db(&dir).await;
        let manifest_url = format!("http://{address}/m.mpd");
        let dest = dir.join("out.mp4");

        // 第一次运行：请求的是改写后的地址。
        let first = dash_params(&manifest_url, &dir, &db, crate::intercept::noop(), false);
        let (bytes, _) = super::download_track(
            &first,
            &run_spec(&first),
            &manifest_url,
            &None,
            &run_a,
            &dest,
            &mut fresh_progress(),
            Some(DashTrack::Video),
            DashTrack::Video,
        )
        .await
        .expect("first run");
        assert_eq!(bytes, 8);
        assert_eq!(
            hit_targets(&hits),
            vec!["/a/s0.m4s?sig=1".to_string(), "/a/s1.m4s?sig=1".to_string()]
        );
        assert_eq!(std::fs::read(&dest).expect("read output"), b"AAAABBBB");

        // 第二次运行（续传）：改写地址全变，检查点仍判定整轨已完成，不再发任何请求。
        let second = dash_params(&manifest_url, &dir, &db, crate::intercept::noop(), true);
        let (bytes, _) = super::download_track(
            &second,
            &run_spec(&second),
            &manifest_url,
            &None,
            &run_b,
            &dest,
            &mut fresh_progress(),
            Some(DashTrack::Video),
            DashTrack::Video,
        )
        .await
        .expect("second run");
        assert_eq!(bytes, 8);
        assert_eq!(hit_targets(&hits).len(), 2, "续传不应重新请求任何分片");
        assert_eq!(std::fs::read(&dest).expect("read output"), b"AAAABBBB");
    }

    /// 刷新阶段换发 `X-Token`；带令牌的请求才放行。
    fn token_gate(body: &'static [u8]) -> Handler {
        std::sync::Arc::new(move |request| {
            if request.to_ascii_lowercase().contains("x-token: tok-new") {
                (200, body.to_vec())
            } else {
                (403, Vec::new())
            }
        })
    }

    fn refresh_interceptor(
        fail_task: bool,
        reply: Result<Option<&str>, &str>,
    ) -> std::sync::Arc<crate::intercept::drive::tests::ScriptedInterceptor> {
        use crate::intercept::InterceptStage;
        use crate::intercept::drive::tests::{ScriptedInterceptor, entry};
        std::sync::Arc::new(
            ScriptedInterceptor::new(vec![entry("a@b", &[InterceptStage::Refresh], fail_task)])
                .reply("a@b", InterceptStage::Refresh, reply),
        )
    }

    #[tokio::test]
    async fn dash_segment_rejection_refreshes_credentials() {
        let (address, hits) = serve(token_gate(b"SEG!")).await;
        let ic = refresh_interceptor(false, Ok(Some(r#"{"setHeaders":{"X-Token":"tok-new"}}"#)));
        let dir = scratch_dir("segment_refresh");
        let db = dash_db(&dir).await;
        let manifest_url = format!("http://{address}/m.mpd");
        let p = dash_params(&manifest_url, &dir, &db, ic.clone(), false);
        let spec = run_spec(&p);
        let dest = dir.join("out.mp4");

        let (bytes, _) = super::download_track(
            &p,
            &spec,
            &manifest_url,
            &None,
            &manifest_segments(address),
            &dest,
            &mut fresh_progress(),
            Some(DashTrack::Video),
            DashTrack::Video,
        )
        .await
        .expect("download after refresh");
        assert_eq!(bytes, 8);
        assert_eq!(std::fs::read(&dest).expect("read output"), b"SEG!SEG!");

        // 首片被拒一次、刷新后重发成功；第二片直接带新身份，不再被拒。
        assert_eq!(hit_targets(&hits), vec!["/s0.m4s", "/s0.m4s", "/s1.m4s"]);
        {
            let raw = hits.lock().expect("hits lock");
            assert!(!raw[0].to_ascii_lowercase().contains("x-token"));
            assert!(raw[1].to_ascii_lowercase().contains("x-token: tok-new"));
        }
        let seen = ic.seen.lock().expect("seen lock");
        assert_eq!(seen.len(), 1, "单飞：整次运行只询问插件一次");
        assert_eq!(seen[0].2["status"], 403);
        assert_eq!(seen[0].2["stage"], "refresh");
    }

    #[tokio::test]
    async fn dash_manifest_rejection_refreshes_credentials() {
        let (address, hits) = serve(token_gate(
            br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"><Period/></MPD>"#,
        ))
        .await;
        let ic = refresh_interceptor(false, Ok(Some(r#"{"setHeaders":{"X-Token":"tok-new"}}"#)));
        let dir = scratch_dir("manifest_refresh");
        let db = dash_db(&dir).await;
        let p = dash_params(
            &format!("http://{address}/m.mpd"),
            &dir,
            &db,
            ic.clone(),
            false,
        );

        let (mpd, _, raw) = super::fetch_and_parse_mpd(&p, &run_spec(&p))
            .await
            .expect("manifest after refresh");
        assert_eq!(mpd.periods.len(), 1);
        assert!(!raw.is_empty());
        assert_eq!(hit_targets(&hits), vec!["/m.mpd", "/m.mpd"]);
        assert_eq!(ic.seen.lock().expect("seen lock").len(), 1);
    }

    #[tokio::test]
    async fn dash_refresh_abort_is_not_retried_as_transient() {
        // 插件以 onFailure=fail 中止：任务立即失败，不进入分片重试（否则会反复询问插件）。
        let (address, hits) = serve(std::sync::Arc::new(|_| (403, Vec::new()))).await;
        let ic = refresh_interceptor(true, Err("boom"));
        let dir = scratch_dir("refresh_abort");
        let db = dash_db(&dir).await;
        let manifest_url = format!("http://{address}/m.mpd");
        let p = dash_params(&manifest_url, &dir, &db, ic.clone(), false);

        let err = super::download_track(
            &p,
            &run_spec(&p),
            &manifest_url,
            &None,
            &manifest_segments(address),
            &dir.join("out.mp4"),
            &mut fresh_progress(),
            Some(DashTrack::Video),
            DashTrack::Video,
        )
        .await
        .expect_err("refresh abort fails the task");
        assert!(err.to_string().contains("boom"), "got: {err}");
        assert_eq!(hit_targets(&hits).len(), 1);
        assert_eq!(ic.seen.lock().expect("seen lock").len(), 1);
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

    use super::{DashCheckpoint, DashSegment, DashTrack, TrackProgress, segments_fingerprint};

    fn seg(url: &str, range: Option<&str>) -> DashSegment {
        DashSegment {
            url: url.to_string(),
            range: range.map(str::to_string),
            number: None,
            time: None,
            rewritten: None,
        }
    }

    #[test]
    fn dash_checkpoint_roundtrips_both_tracks() {
        let mut cp = DashCheckpoint::default();
        cp.set(
            DashTrack::Video,
            TrackProgress::Done {
                bytes: 1234,
                fingerprint: 0xdead_beef,
            },
        );
        cp.set(
            DashTrack::Audio,
            TrackProgress::Partial {
                idx: 7,
                bytes: 99,
                fingerprint: 0x1,
            },
        );
        assert_eq!(DashCheckpoint::parse(&cp.encode()), cp);
    }

    #[test]
    fn dash_checkpoint_rejects_foreign_or_garbled_values() {
        // 旧版 HLS 检查点("idx:bytes:seq")不是 DASH 检查点,绝不能被解读为进度。
        assert_eq!(DashCheckpoint::parse("5:1024:0"), DashCheckpoint::default());
        assert_eq!(DashCheckpoint::parse(""), DashCheckpoint::default());
        let partly = DashCheckpoint::parse("dash1;v=seg:x:1:2;a=done:5:ff");
        assert_eq!(partly.video, None);
        assert_eq!(
            partly.audio,
            Some(TrackProgress::Done {
                bytes: 5,
                fingerprint: 0xff
            })
        );
    }

    #[test]
    fn segments_fingerprint_ignores_signature_but_tracks_manifest_changes() {
        let init = Some(seg("https://cdn.example/init.mp4?sig=a", None));
        let segs = vec![
            seg("https://cdn.example/s1.m4s?sig=a", None),
            seg("https://cdn.example/s2.m4s?sig=a", Some("0-99")),
        ];
        let base = segments_fingerprint(&init, &segs);

        let rotated_init = Some(seg("https://cdn.example/init.mp4?sig=b", None));
        let rotated = vec![
            seg("https://cdn.example/s1.m4s?sig=b", None),
            seg("https://cdn.example/s2.m4s?sig=b", Some("0-99")),
        ];
        assert_eq!(segments_fingerprint(&rotated_init, &rotated), base);

        let mut shorter = segs.clone();
        shorter.pop();
        assert_ne!(segments_fingerprint(&init, &shorter), base);

        let mut other_range = segs.clone();
        other_range[1].range = Some("0-199".to_string());
        assert_ne!(segments_fingerprint(&init, &other_range), base);
    }

    #[tokio::test]
    async fn test_ffmpeg_copy_to_mp4_fallback() {
        let Some(ffmpeg_bin) = std::env::var("FLUXDOWN_TEST_FFMPEG")
            .ok()
            .map(std::path::PathBuf::from)
        else {
            eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过真实 ffmpeg 回退测试");
            return;
        };
        if !ffmpeg_bin.exists() {
            eprintln!("[skip] FLUXDOWN_TEST_FFMPEG 指定的二进制不存在: {ffmpeg_bin:?}");
            return;
        }

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let temp_dir_path =
            std::env::temp_dir().join(format!("fluxdown_mux_test_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&temp_dir_path).expect("create temp dir");

        struct ScopedDir(std::path::PathBuf);
        impl Drop for ScopedDir {
            fn drop(&mut self) {
                if let Err(e) = std::fs::remove_dir_all(&self.0)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    eprintln!("failed to clean up test dir: {e}");
                }
            }
        }
        let _guard = ScopedDir(temp_dir_path.clone());

        let v_path = temp_dir_path.join("test_v.mp4");
        let a_path = temp_dir_path.join("test_a.wav");
        let out_path = temp_dir_path.join("out.mp4");

        // 1. 生成 0.2 秒的 x264 视频
        let mut gen_v = std::process::Command::new(&ffmpeg_bin);
        gen_v
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=32x32:rate=10",
                "-t",
                "0.2",
                "-c:v",
                "libx264",
            ])
            .arg(&v_path);
        assert!(
            gen_v.status().map(|s| s.success()).unwrap_or(false),
            "生成测试视频失败"
        );

        // 2. 生成 0.2 秒的 adpcm_ms 音频（WAV 容器，纯内置原生编解码器）
        // MP4 容器 muxer 明确不支持将 adpcm_ms 裸流直接复制（-c copy）入容器，
        // 必然触发 "Could not find tag for codec adpcm_ms in stream #1, codec not currently supported in container"
        let mut gen_a = std::process::Command::new(&ffmpeg_bin);
        gen_a
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440",
                "-t",
                "0.2",
                "-c:a",
                "adpcm_ms",
            ])
            .arg(&a_path);
        assert!(
            gen_a.status().map(|s| s.success()).unwrap_or(false),
            "生成测试音频失败"
        );

        // 3. 验证初次纯 -c copy 确实会失败，以确保测试并非在空转
        let mut verify_fail = std::process::Command::new(&ffmpeg_bin);
        verify_fail
            .args(["-y", "-i"])
            .arg(&v_path)
            .arg("-i")
            .arg(&a_path)
            .args(["-c", "copy", "-f", "mp4"])
            .arg(temp_dir_path.join("direct_copy.mp4"));
        let direct_copy_status = verify_fail.status().map(|s| s.success()).unwrap_or(false);
        assert!(
            !direct_copy_status,
            "direct -c copy 必须失败才能证明回退生效"
        );

        // 4. 调用 ffmpeg_copy_to_mp4，断言其通过 AAC 回退路径成功输出
        let cancel = tokio_util::sync::CancellationToken::new();
        let res = super::ffmpeg_copy_to_mp4(
            &v_path,
            Some(&a_path),
            &out_path,
            100_000,
            &cancel,
            &ffmpeg_bin,
        )
        .await;
        assert!(
            res.is_ok(),
            "ffmpeg_copy_to_mp4 必须通过 AAC 回退成功封装: {res:?}"
        );
        assert!(out_path.exists(), "产物 out.mp4 必须存在");

        // 5. probe 产物：验证音频轨确实被重编码为 aac
        let probe = std::process::Command::new(&ffmpeg_bin)
            .arg("-i")
            .arg(&out_path)
            .output()
            .expect("probe output");
        let probe_stderr = String::from_utf8_lossy(&probe.stderr);
        assert!(
            probe_stderr.contains("Audio: aac"),
            "回退产物必须为 aac 音频: {probe_stderr}"
        );

        // 6. 验证取消语义：当 cancel_token 取消时，应当返回 Cancelled 且清理产物
        let cancel_tok = tokio_util::sync::CancellationToken::new();
        cancel_tok.cancel();
        let out_cancelled = temp_dir_path.join("out_cancelled.mp4");
        let res_cancelled = super::ffmpeg_copy_to_mp4(
            &v_path,
            Some(&a_path),
            &out_cancelled,
            100_000,
            &cancel_tok,
            &ffmpeg_bin,
        )
        .await;
        assert!(
            matches!(res_cancelled, Err(super::DownloadError::Cancelled)),
            "已取消的调用必须返回 DownloadError::Cancelled: {res_cancelled:?}"
        );
        assert!(!out_cancelled.exists(), "取消调用的输出文件必须被清理");

        // 7. 测试 audio = None 时正常复制，不走回退
        let out_video_only = temp_dir_path.join("out_v_only.mp4");
        let res_v = super::ffmpeg_copy_to_mp4(
            &v_path,
            None,
            &out_video_only,
            100_000,
            &cancel,
            &ffmpeg_bin,
        )
        .await;
        assert!(res_v.is_ok(), "audio = None 时纯视频封装应成功");
        assert!(out_video_only.exists(), "out_video_only.mp4 产物应存在");
    }

    #[test]
    fn track_probe_reads_length_and_last_modified() {
        use reqwest::header::{CONTENT_RANGE, HeaderMap, HeaderValue, LAST_MODIFIED};
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_static("bytes 0-0/240334643"),
        );
        headers.insert(
            LAST_MODIFIED,
            HeaderValue::from_static("Wed, 18 Mar 2026 12:28:19 GMT"),
        );
        let probe = super::track_probe_from_headers(&headers);
        assert_eq!(probe.len, Some(240_334_643));
        assert_eq!(probe.last_modified, "Wed, 18 Mar 2026 12:28:19 GMT");

        // 全长未知（`*`）不影响服务器时间；无 Last-Modified 时为空串。
        let mut unknown = HeaderMap::new();
        unknown.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 0-0/*"));
        let probe = super::track_probe_from_headers(&unknown);
        assert_eq!(probe.len, None);
        assert!(probe.last_modified.is_empty());
    }

    #[test]
    fn track_pair_mtime_targets_cover_merged_and_separate_outputs() {
        use std::path::{Path, PathBuf};
        let dest = Path::new("/d/pair.mp4");
        let audio = Path::new("/d/pair.audio.m4a");
        let video_lm = "Mon, 01 Jan 2024 00:00:00 GMT";
        let audio_lm = "Tue, 02 Jan 2024 00:00:00 GMT";

        // mux 成功：合并产物取视频轨时间。
        assert_eq!(
            super::track_pair_mtime_targets(dest, audio, video_lm, audio_lm, true, false),
            vec![(PathBuf::from("/d/pair.mp4"), video_lm)]
        );
        // 视频轨未提供时间：合并产物退用音频轨。
        assert_eq!(
            super::track_pair_mtime_targets(dest, audio, "", audio_lm, true, false),
            vec![(PathBuf::from("/d/pair.mp4"), audio_lm)]
        );
        // 没有音频产物：视频文件不得借用音频轨时间。
        assert_eq!(
            super::track_pair_mtime_targets(dest, audio, "", audio_lm, false, false),
            vec![(PathBuf::from("/d/pair.mp4"), "")]
        );
        // mux 失败：两个独立文件各取本轨时间。
        assert_eq!(
            super::track_pair_mtime_targets(dest, audio, video_lm, audio_lm, true, true),
            vec![
                (PathBuf::from("/d/pair.mp4"), video_lm),
                (PathBuf::from("/d/pair.audio.m4a"), audio_lm),
            ]
        );
    }
}
