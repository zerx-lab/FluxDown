//! HLS (HTTP Live Streaming) download engine.
//!
//! Fetches M3U8 playlists, downloads all segments with bounded concurrency,
//! optionally decrypts AES-128-CBC encrypted segments, and merges them into a
//! single output file — MPEG-TS (`.ts`) for classic playlists, or a
//! fragmented MP4 (`.mp4`) for fMP4/CMAF playlists that carry an
//! EXT-X-MAP initialization segment (#682).
//!
//! Architecture:
//! - Master playlist → auto-select highest bandwidth variant
//! - Media playlist → bounded-concurrency segment download with cancellation.
//!   Each segment downloads + decrypts on its own task; its connection permit
//!   (`Semaphore`) is returned as soon as the body is in memory, so a slow head
//!   segment never idles the other connections. Finished segments wait in a
//!   bounded reorder window (`ReorderWindow`: segments ahead + buffered bytes)
//!   and a single writer appends them in `seg_idx` order — one sequential write
//!   into one temp file, no per-segment part files and no merge pass.
//! - EXT-X-MAP (fMP4/CMAF) init segments are prefetched once per distinct
//!   `(uri, byte_range)` and prepended by the writer whenever the sticky map
//!   in effect changes (see `should_write_init`)
//! - AES-128-CBC decryption with shared key caching
//! - Progress = written bytes (writer side, never double-counted); speed and
//!   host traffic = body bytes as received (`SegmentMeter`), so the display
//!   follows the network instead of segment-sized write bursts. The speed
//!   limiter throttles those reads, not the writes. 点播总大小在全部落盘前由逐段
//!   大小证据(在途响应头 / 已下载 / 已写盘)与码率标注估算(`size_estimate`),
//!   只用于展示,收尾前收敛为实际字节。
//! - Resume checkpoints are throttled to `CHECKPOINT_INTERVAL` and topped up on exit
//! - Per-segment retry with exponential backoff
//! - 变体 AUDIO 组带独立 URI(EXT-X-MEDIA)时并行下载音轨,收尾用 ffmpeg 流复制
//!   与视频 mux;ffmpeg 不可用时只出视频并记录 warning 活动
//! - 直播(播放列表无 EXT-X-ENDLIST):按 EXT-X-TARGETDURATION(1–10s)轮询播放列表,
//!   按 media sequence 只追加新分片,播放列表出现 ENDLIST / 用户暂停或取消 / 播放列表
//!   连续拉取失败时,以已录内容走与点播相同的收尾(remux / 音轨 mux)定稿为完成文件

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, OnceCell, Semaphore, mpsc, watch};

use crate::dash_downloader::{
    build_audio_path, effective_ffmpeg, ffmpeg_copy_to_mp4, ffmpeg_usable,
};
use crate::downloader::{
    DB_SAVE_INTERVAL_SECS, DownloadError, DownloadParams, DownloadSpec, ProgressUpdate,
    RequestSpec, TEMP_EXT, claim_final_name, dedup_filename,
};
use crate::events::EventSink;
use crate::intercept::ctx::{
    ByteRangeCtx, DateRangeCtx, KeyCtx, MapCtx, SegmentItem, SegmentTrack, SegmentsHeader,
    SelectVariantCtx, TaskCtx, VariantCtx,
};
use crate::intercept::drive::{SegmentDecision, run_segments, run_select};
use crate::intercept::{
    ActivityNotes, InterceptStage, ProtocolKind, Refresher, TaskInterceptor, cap_connections, wants,
};
use crate::logger::{log_info, log_warn};
use crate::model::HlsQualityOption;
use crate::naming::{extract_from_url, sanitize_filename};
use crate::output;
use crate::selection::SelectionOutcome;
use crate::transfer_activity::{TaskRuntime, TransferTracker};

mod size_estimate;

use size_estimate::{TaskProgress, TrackProgress, VariantBitrate};

fn hls_runtime(task_id: &str, tracker: &TransferTracker, limit: u32) -> TaskRuntime {
    TaskRuntime {
        task_id: task_id.to_owned(),
        sampled_at_ms: chrono::Utc::now().timestamp_millis(),
        sample_sequence: crate::transfer_activity::next_sample_sequence(),
        active_transfers: Some(tracker.active()),
        connected_peers: None,
        parallelism_limit: Some(limit),
        total_bytes: 0,
        segments: Vec::new(),
        source_bytes: None,
    }
}

/// 一次运行内所有分片请求共享的传输计量与限速(视频、音轨、init 段共用一份)。
///
/// `received` 在 body 读到时即累加(含随后被重试丢弃的字节),是测速与宿主流量
/// 统计的来源;落盘字节只表示进度。限速同样作用在读取侧:写盘侧限速会让乱序
/// 窗口先以全速灌满,网络实际速率超出设定值。
#[derive(Clone)]
struct SegmentMeter {
    tracker: TransferTracker,
    received: Arc<AtomicI64>,
    limiter: crate::speed_limiter::SpeedLimiter,
}

impl SegmentMeter {
    fn new(limiter: crate::speed_limiter::SpeedLimiter) -> Self {
        Self {
            tracker: TransferTracker::new(),
            received: Arc::new(AtomicI64::new(0)),
            limiter,
        }
    }

    fn received(&self) -> i64 {
        self.received.load(Ordering::Relaxed)
    }

    /// 记下刚读到的一块 body,再按全局限速放行;取消时立即返回。
    async fn admit_body(
        &self,
        bytes: usize,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), DownloadError> {
        self.received
            .fetch_add(i64::try_from(bytes).unwrap_or(i64::MAX), Ordering::Relaxed);
        let mut left = bytes as u64;
        while left > 0 {
            let granted = tokio::select! {
                _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                granted = self.limiter.consume(left) => granted.min(left),
            };
            if granted == 0 {
                tokio::task::yield_now().await;
            }
            left -= granted;
        }
        Ok(())
    }
}

/// 点播 / 直播共用的进度帧:落盘字节表示进度,接收字节用于测速,附活跃连接采样。
/// `total` 0 = 未知(直播,或点播首段落盘前且无码率先验)。
fn hls_progress(
    task_id: &str,
    meter: &SegmentMeter,
    written: i64,
    total: i64,
    limit: u32,
) -> ProgressUpdate {
    ProgressUpdate {
        task_id: task_id.to_owned(),
        downloaded_bytes: written,
        transfer_bytes: Some(meter.received()),
        total_bytes: total,
        status: 1,
        runtime: Some(hls_runtime(task_id, &meter.tracker, limit)),
        ..Default::default()
    }
}

/// 下载中的总量(估值或收尾前的实际字节)落库,供暂停 / 出错帧与重启后的列表显示
/// 进度。只是展示值:完成帧与 manager 的完成落库才是终值,故失败只告警。0 = 未知,不写。
async fn persist_display_total(db: &crate::db::Db, task_id: &str, total: i64) {
    if total <= 0 {
        return;
    }
    if let Err(error) = db.update_task_total_bytes(task_id, total).await {
        crate::logger::report_warning("hls-download", "persist_display_total", &error);
    }
}

/// 写续传检查点:先 flush,保证文件里已有检查点声明的全部字节。
async fn save_checkpoint(
    file: &mut File,
    db: &crate::db::Db,
    key: &str,
    value: &str,
) -> Result<(), DownloadError> {
    file.flush().await?;
    db.set_config(key, value).await?;
    Ok(())
}

/// 小请求(playlist / 密钥 / MPD)的整体超时:响应体只有几 KB~几 MB,30s 足够;
/// 段 body 不能用整体超时(单段可达 256MB),见 `SEGMENT_IDLE_TIMEOUT`。
pub(crate) const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 段请求等待响应头的上限。
pub(crate) const SEGMENT_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
/// 段 body 逐 chunk 空闲上限:比 HTTP 路径的 5/10s 宽松,HLS 常见限速 CDN。
pub(crate) const SEGMENT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// 拉取小响应体,并与取消、超时竞速;返回(重定向后的最终 URL, 响应体)。
/// 不在共享 client 上设全局 timeout,否则会误伤普通 HTTP 长下载。
pub(crate) async fn fetch_small(
    req: crate::http_request::HttpRequest<'_>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(String, bytes::Bytes), DownloadError> {
    let work = async {
        let resp = req.send().await?.error_for_status()?;
        let final_url = resp.url().to_string();
        let body = resp.bytes().await?;
        Ok::<_, DownloadError>((final_url, body))
    };
    tokio::select! {
        _ = cancel.cancelled() => Err(DownloadError::Cancelled),
        r = tokio::time::timeout(SMALL_REQUEST_TIMEOUT, work) => match r {
            Ok(r) => r,
            Err(_) => Err(DownloadError::Other(format!(
                "request timed out after {}s",
                SMALL_REQUEST_TIMEOUT.as_secs()
            ))),
        },
    }
}

/// 一次 HLS 请求的公共上下文。请求身份(Cookie / Referer / 请求头 / 按站点请求头)
/// 一律取自运行级共享的 [`DownloadSpec`]:每次发请求前读 `current()`,凭据被插件
/// 刷新后后续请求立即换用新身份。播放列表 / 密钥 / 分片的每个请求(含其重定向的
/// 每一跳)都以用户提交的清单 URL 为凭据锚点按 [`crate::credential_scope`] 判定:
/// Cookie 只发往同站主机,Authorization 只发往同源,Referer 对所有请求附加
/// (防盗链 CDN 缺失即 403;仅 https 来源发往 http 目标时不发)。
#[derive(Clone, Copy)]
struct HlsRequestCtx<'a> {
    spec: &'a DownloadSpec,
    /// 凭据锚点:用户提交的原始清单 URL。
    header_origin_url: &'a str,
}

/// 单次请求尝试失败的性质。
enum RequestFailure {
    /// 走常规重试 / 跳过逻辑的失败(含用户取消)。
    Retryable(DownloadError),
    /// 凭据刷新阶段被插件判为任务失败(`onFailure: "fail"`):重试与"跳过坏分片"都
    /// 无意义,调用方必须直接终止。
    Fatal(DownloadError),
}

impl From<DownloadError> for RequestFailure {
    fn from(error: DownloadError) -> Self {
        Self::Retryable(error)
    }
}

impl RequestFailure {
    /// `refresh_after_rejection` 的错误:用户取消仍按取消处理,其余为致命。
    fn from_refresh(error: DownloadError) -> Self {
        match error {
            DownloadError::Cancelled => Self::Retryable(DownloadError::Cancelled),
            other => Self::Fatal(other),
        }
    }

    fn into_error(self) -> DownloadError {
        match self {
            Self::Retryable(error) | Self::Fatal(error) => error,
        }
    }
}

/// `outcome` 是用 `used` 发出的请求的结果。成功时记录该身份可用;是 401 / 403 / 410
/// 拒绝且插件换发了新凭据时,返回应重发所用的新身份;其余情况(含未声明 `refresh`)
/// 返回 `None`,调用方照常处理原结果。每个请求至多因刷新重发一次。
async fn refreshed_identity<T>(
    spec: &DownloadSpec,
    used: &Arc<RequestSpec>,
    outcome: &Result<T, DownloadError>,
) -> Result<Option<Arc<RequestSpec>>, RequestFailure> {
    let status = match outcome {
        Ok(_) => {
            spec.confirm(used);
            return Ok(None);
        }
        Err(DownloadError::Request(error)) => error.status(),
        Err(_) => None,
    };
    let Some(status) = status else {
        return Ok(None);
    };
    spec.refresh_after_rejection(used, status)
        .await
        .map_err(RequestFailure::from_refresh)
}

/// 刷新后重发的结果:成功时同样记录新身份可用。
fn confirm_resend<T>(
    spec: &DownloadSpec,
    fresh: &Arc<RequestSpec>,
    outcome: Result<T, DownloadError>,
) -> Result<T, DownloadError> {
    if outcome.is_ok() {
        spec.confirm(fresh);
    }
    outcome
}

impl HlsRequestCtx<'_> {
    /// 以请求身份 `identity` 构造 GET。
    fn get<'s>(
        &self,
        client: &Client,
        url: &str,
        identity: &'s RequestSpec,
    ) -> crate::http_request::HttpRequest<'s> {
        crate::downloader::build_request(client, url, reqwest::Method::GET, identity)
            .anchor(self.header_origin_url)
    }

    /// 拉取小体积资源(播放列表 / 密钥);被拒且凭据已刷新时用新身份重发一次。
    async fn fetch_small(
        &self,
        client: &Client,
        url: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(String, bytes::Bytes), RequestFailure> {
        let used = self.spec.current();
        let first = fetch_small(self.get(client, url, &used), cancel).await;
        match refreshed_identity(self.spec, &used, &first).await? {
            Some(fresh) => Ok(confirm_resend(
                self.spec,
                &fresh,
                fetch_small(self.get(client, url, &fresh), cancel).await,
            )?),
            None => Ok(first?),
        }
    }
}

/// 随作用域结束(含 panic 展开)中止后台任务,避免采样器泄漏。
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl AbortOnDrop {
    fn abort(&self) {
        self.0.abort();
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
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

/// 乱序窗口:派发位置最多领先写盘位置 `并发数 × 该值` 段。队首段慢时其余连接最多
/// 先跑这么远;更大只会让暂停 / 出错时丢弃的内存缓冲变多。
const REORDER_AHEAD_PER_CONNECTION: usize = 8;

/// 续传检查点节流间隔。检查点只声明"已完整落盘的连续前缀",落后不影响正确性
/// (续传时截断到检查点、重下其后的段),退出时另行补写最新前缀。
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(DB_SAVE_INTERVAL_SECS);

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
pub enum M3u8Content {
    Master {
        variants: Vec<HlsVariant>,
    },
    Media {
        segments: Vec<HlsSegment>,
        total_duration: f32,
        media_sequence: u64,
        /// 播放列表中是否存在 EXT-X-MAP(fMP4/CMAF 初始化段)。`true` 时输出
        /// 是分片 MP4(ftyp+moov+[moof+mdat]*)而非 MPEG-TS,下游需相应改变
        /// 输出扩展名并跳过 TS→MP4 remux(#682)。
        has_map: bool,
        /// 播放列表是否为直播/未结束(无 EXT-X-ENDLIST 且非 VOD 类型)。
        live: bool,
        /// EXT-X-TARGETDURATION(秒),直播轮询间隔据此推算。
        target_duration: u64,
    },
}

/// A variant stream from a master playlist.
pub struct HlsVariant {
    pub bandwidth: u64,
    /// EXT-X-STREAM-INF 的 AVERAGE-BANDWIDTH(可选);只用作首段落盘前的总大小先验。
    pub average_bandwidth: Option<u64>,
    pub resolution: Option<(u64, u64)>,
    pub uri: String,
    /// 该变体的音频组(`AUDIO="group"`)指向独立 URI 的 EXT-X-MEDIA 音轨时
    /// 的绝对 URI;`None` 表示音频已内嵌在变体流里。
    pub audio_uri: Option<String>,
}

/// A single segment from a media playlist.
pub struct HlsSegment {
    pub uri: String,
    pub duration: f32,
    pub key: Option<HlsKey>,
    /// EXT-X-BYTERANGE 子区间 `(offset, length)`(字节)。`None` 表示整段就是
    /// 整个 `uri` 资源;`Some` 表示该段只是 `uri` 的一个子区间,下载时必须发
    /// `Range: bytes=offset-(offset+length-1)` 头并要求 206,否则多个段会各自
    /// 下载整文件、拼出 N 份完整副本(巨量损坏 + 撑爆磁盘)。
    pub byte_range: Option<(u64, u64)>,
    /// 本段生效的 EXT-X-BITRATE(kbps,粘性;带 BYTERANGE 的段不适用)。只用于进度估算。
    pub bitrate_kbps: Option<u64>,
    /// 本段是否紧跟 EXT-X-DISCONTINUITY(与前一段存在不连续点)。当前仅解析
    /// 并保留该标志;隐式 IV 仍按 RFC 8216 用绝对 Media Sequence Number 计算
    /// (见 `compute_default_iv` 注释),不连续点不重置该序号。
    pub discontinuity: bool,
    /// 本段生效的 EXT-X-MAP 初始化段(fMP4/CMAF)。RFC 8216 §4.3.2.4 规定该
    /// tag 粘性生效直到下一个 EXT-X-MAP 或播放列表结束,故同一 `HlsMap` 会
    /// 被多个连续段共享(#682)。`None` 表示 MPEG-TS 段,无需初始化段。
    pub map: Option<HlsMap>,
    /// 绝对 media sequence(播放列表 `MEDIA-SEQUENCE` + 位置);过滤分片后仍保持
    /// 原值,AES-128 隐式 IV 与直播游标都依赖它。
    pub sequence: u64,
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

/// fMP4/CMAF 初始化段(EXT-X-MAP)。包含 `ftyp`+`moov` box,须在其粘性覆盖
/// 的首个媒体段之前写入输出文件一次;之后的媒体段只含 `moof`+`mdat`。
#[derive(Clone, PartialEq, Eq)]
pub struct HlsMap {
    pub uri: String,
    /// EXT-X-MAP 的 BYTERANGE 属性 `(offset, length)`。省略 offset 时视为 0
    /// ——不同于 EXT-X-BYTERANGE 段,init 段通常是独立小文件,标准未定义跨
    /// 多次出现的隐式偏移累计。`None` 表示整个 `uri` 资源就是初始化段。
    pub byte_range: Option<(u64, u64)>,
}

/// 初始化段去重键 `(uri, byte_range)`:同一 key 的 init 段只下载 / 写入一次。
type MapKey = (String, Option<(u64, u64)>);

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
async fn parse_m3u8(
    client: &Client,
    url: &str,
    ctx: &HlsRequestCtx<'_>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<FetchedPlaylist, RequestFailure> {
    // 相对 URI 必须以"最终检索到的资源 URL"为 base 解析(RFC 3986 §5.1)。
    // reqwest 默认跟随重定向(见 downloader.rs),播放列表被负载均衡/短链
    // 重定向时,请求 url 与实际返回内容的 URL 不同;若仍用请求前的 url 作
    // base,会把相对段/密钥 URI 拼到错误的主机/路径。无重定向时
    // base_url == url,行为不变。与同仓 downloader.rs 既定做法对齐。
    let (base_url, bytes) = ctx.fetch_small(client, url, cancel).await?;

    let content = parse_m3u8_bytes(&base_url, &bytes)?;
    Ok(FetchedPlaylist {
        content,
        raw: bytes,
    })
}

/// 拉取到的播放列表:解析结果 + 原文(供插件 `manifestText` / 分片附加信息)。
struct FetchedPlaylist {
    content: M3u8Content,
    raw: bytes::Bytes,
}

/// 去掉 UTF-8 BOM 与前导空白:m3u8_rs 要求首行就是 `#EXTM3U`,带 BOM 的
/// 合法播放列表否则会被误判为非播放列表。
fn trim_playlist_prefix(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    &bytes[start..]
}

/// 选出变体音频组里带独立 URI 的音轨(优先 DEFAULT=YES)。
fn external_audio_uri(
    alternatives: &[m3u8_rs::AlternativeMedia],
    group: &str,
    base_url: &str,
) -> Option<String> {
    let mut candidates = alternatives.iter().filter(|a| {
        matches!(a.media_type, m3u8_rs::AlternativeMediaType::Audio)
            && a.group_id == group
            && a.uri.as_deref().is_some_and(|u| !u.is_empty())
    });
    let first = candidates.next()?;
    let chosen = if first.default {
        first
    } else {
        candidates.find(|a| a.default).unwrap_or(first)
    };
    chosen.uri.as_deref().map(|u| resolve_uri(base_url, u))
}

/// Parse already-fetched M3U8 bytes against a resolved base URL.
///
/// Split out from [`parse_m3u8`] purely for unit-testability: the HTTP fetch
/// above needs a live server/mock to exercise, but the playlist-parsing logic
/// (segment/key/map resolution, byte-range accounting) is pure and worth
/// testing directly (see `mod tests`).
fn parse_m3u8_bytes(base_url: &str, bytes: &[u8]) -> Result<M3u8Content, DownloadError> {
    let bytes = trim_playlist_prefix(bytes);
    // 解析失败（含非法内容、缺 #EXTM3U 头）一律归为 NotAnHlsPlaylist——而
    // 非泛化的 Other——使调用方（`run_hls_download`）能与"内容合法但下游
    // 步骤失败"的真实 HLS 错误区分开，把误判为 HLS 的普通文件退回普通 HTTP
    // 下载器，而不是当作终态失败上报。
    let (_remaining, playlist) = m3u8_rs::parse_playlist(bytes)
        .map_err(|e| DownloadError::NotAnHlsPlaylist(format!("M3U8 parse error: {}", e)))?;

    match playlist {
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            // I-frame 变体(EXT-X-I-FRAME-STREAM-INF)只含关键帧,是快进预览流,
            // 不能当画质选项,更不能参与"最高码率"自动选择。
            let variants: Vec<HlsVariant> = master
                .variants
                .iter()
                .filter(|v| !v.is_i_frame)
                .map(|v| {
                    let resolution = v.resolution.as_ref().map(|r| (r.width, r.height));
                    HlsVariant {
                        bandwidth: v.bandwidth,
                        average_bandwidth: v.average_bandwidth,
                        resolution,
                        uri: resolve_uri(base_url, &v.uri),
                        audio_uri: v
                            .audio
                            .as_deref()
                            .and_then(|g| external_audio_uri(&master.alternatives, g, base_url)),
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
            let live = !media.end_list
                && !matches!(media.playlist_type, Some(m3u8_rs::MediaPlaylistType::Vod));
            let mut total_duration: f32 = 0.0;
            let mut current_key: Option<HlsKey> = None;
            // EXT-X-MAP(fMP4/CMAF 初始化段,#682)按 RFC 8216 §4.3.2.4 对其后
            // 所有段粘性生效,直到下一个 EXT-X-MAP 或播放列表结束。m3u8_rs 的
            // 解析器只把 `map` 挂在"标签出现的那一段"上、逐段清空(其
            // parser.rs 里 `map = None` 与 `encryption_key = None` 同步发生),
            // 与 `key` 字段同样是非粘性的——因此这里手动维护 `current_map`
            // 跨段传播,与下方 `current_key` 的处理方式完全一致。
            //
            // 已知限制:若 EXT-X-MAP 之前出现过 EXT-X-KEY(RFC 8216 §4.3.2.4/
            // §4.3.2.5 规定此时 init 段也应使用该密钥解密),m3u8_rs 的 `Map`
            // 结构不携带密钥/IV 信息,本实现无法据此解密——init 段字节按原样
            // (未解密)写入输出。现实中的 fMP4/CMAF 分发绝大多数场景下 init
            // 段本身未加密(仅媒体段加密),此限制不影响这些场景。
            let mut current_map: Option<HlsMap> = None;
            let mut segments: Vec<HlsSegment> = Vec::with_capacity(media.segments.len());
            // EXT-X-BYTERANGE 省略 @offset 时,offset = 同一 uri 上一子区间结束
            // 位置+1(按出现顺序累计)。键为已解析的绝对段 URI,值为该 uri 上
            // "下一个隐式子区间的起始 offset"(即上一子区间的 offset+length)。
            let mut byterange_next_offset: HashMap<String, u64> = HashMap::new();
            // EXT-X-BITRATE(RFC 8216bis §4.4.4.8)粘性生效到下一条同名标签,只作用于
            // 不带 EXT-X-BYTERANGE 的段;仅用于进度估算。
            let mut current_bitrate: Option<u64> = None;

            for seg in &media.segments {
                total_duration += seg.duration;
                if let Some(kbps) = ext_x_bitrate(seg) {
                    current_bitrate = Some(kbps);
                }

                if let Some(m) = &seg.map {
                    let map_byte_range = m
                        .byte_range
                        .as_ref()
                        .map(|br| (br.offset.unwrap_or(0), br.length));
                    current_map = Some(HlsMap {
                        uri: resolve_uri(base_url, &m.uri),
                        byte_range: map_byte_range,
                    });
                }

                if let Some(key) = &seg.key {
                    current_key = match &key.method {
                        &m3u8_rs::KeyMethod::AES128 => {
                            let key_uri = match key.uri.as_ref() {
                                Some(u) if !u.is_empty() => resolve_uri(base_url, u),
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

                // `m3u8-rs` 解析 `#EXT-X-KEY:METHOD=NONE` 失败时,整条标签会被降级进
                // `unknown_tags`(`tag == "X-KEY"`),段上不再有 `key`。而本状态机里
                // `None` 表示"沿用上一段密钥",于是声明为明文的段会被上一段的 AES-128
                // 密钥解密,以 PKCS7 unpad 失败告终。这里把那条被丢掉的标签识别回来,
                // 显式结束加密状态。
                if seg.key.is_none() && has_dropped_ext_x_key_method_none(seg) {
                    current_key = Some(HlsKey {
                        method: HlsKeyMethod::None,
                        uri: String::new(),
                        iv: None,
                    });
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

                let resolved_uri = resolve_uri(base_url, &seg.uri);

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
                    bitrate_kbps: current_bitrate.filter(|_| byte_range.is_none()),
                    discontinuity: seg.discontinuity,
                    map: current_map.clone(),
                    sequence: media_sequence.saturating_add(segments.len() as u64),
                });
            }

            if segments.is_empty() {
                return Err(DownloadError::Other(
                    "M3U8 media playlist has no segments".to_string(),
                ));
            }

            let has_map = segments.iter().any(|s| s.map.is_some());

            Ok(M3u8Content::Media {
                segments,
                total_duration,
                media_sequence,
                has_map,
                live,
                target_duration: media.target_duration,
            })
        }
    }
}

/// `true` 若 `seg` 携带一条被降级进 `unknown_tags` 的 `#EXT-X-KEY:METHOD=NONE`。
///
/// `m3u8-rs` 解析 `METHOD=NONE` 失败后,`alt` 回溯把整条标签降级成
/// `ExtTag { tag: "X-KEY", rest }`,段上不再有 `key`;而 `None` 在本模块的粘性状态机里
/// 表示"沿用上一段密钥",明文段因此会被上一段的密钥解密。RFC 8216 §4.3.2.4 规定
/// `METHOD=NONE` 时其他属性 MUST NOT 出现,故该形态必不带 `IV` —— 正是解析失败的那一类。
///
/// 只按 RFC 8216 §4.2 的严格拼写比对属性项(属性名 `METHOD`、值 `NONE`、`=` 两侧无空白),
/// 不为任何非法拼写放宽。
fn has_dropped_ext_x_key_method_none(seg: &m3u8_rs::MediaSegment) -> bool {
    seg.unknown_tags.iter().any(|tag| {
        tag.tag == "X-KEY"
            && tag
                .rest
                .as_deref()
                .is_some_and(|rest| rest.split(',').any(|attr| attr == "METHOD=NONE"))
    })
}

/// 段上的 `#EXT-X-BITRATE:<kbps>`(m3u8-rs 不认识,落在 `unknown_tags`);非法值忽略。
fn ext_x_bitrate(seg: &m3u8_rs::MediaSegment) -> Option<u64> {
    seg.unknown_tags
        .iter()
        .rev()
        .find(|tag| tag.tag == "X-BITRATE")
        .and_then(|tag| tag.rest.as_deref())
        .and_then(|kbps| kbps.trim().parse().ok())
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

/// Shared AES-128 key cache: key URI → single-flight cell.
///
/// 外层 `Mutex` 只保护内存 HashMap 的查找/插入,绝不跨网络 I/O 持有;并发段
/// 拿到同一个 `OnceCell` 后由它保证同一 URI 只发一次请求(首批 8~16 个段同时
/// 未命中缓存时不会对限流的密钥服务重复请求)。失败不会写入 cell,下次调用重试。
type KeyCache = Arc<Mutex<HashMap<String, Arc<OnceCell<Vec<u8>>>>>>;

/// Fetch an AES-128 key from the given URI, with caching, single-flight and retry.
async fn fetch_key(
    client: &Client,
    key_uri: &str,
    ctx: &HlsRequestCtx<'_>,
    key_cache: &KeyCache,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Vec<u8>, RequestFailure> {
    let cell = key_cache
        .lock()
        .await
        .entry(key_uri.to_string())
        .or_default()
        .clone();
    cell.get_or_try_init(|| fetch_key_with_retry(client, key_uri, ctx, cancel))
        .await
        .cloned()
}

async fn fetch_key_with_retry(
    client: &Client,
    key_uri: &str,
    ctx: &HlsRequestCtx<'_>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Vec<u8>, RequestFailure> {
    let mut attempts = 0u32;
    loop {
        match fetch_key_once(client, key_uri, ctx, cancel).await {
            Ok(key) => return Ok(key),
            Err(fatal @ RequestFailure::Fatal(_)) => return Err(fatal),
            Err(RequestFailure::Retryable(DownloadError::Cancelled)) => {
                return Err(DownloadError::Cancelled.into());
            }
            Err(RequestFailure::Retryable(e)) => {
                attempts += 1;
                if attempts >= MAX_RETRIES {
                    return Err(DownloadError::Other(format!(
                        "HLS key fetch failed after {} attempts: {}",
                        MAX_RETRIES, e
                    ))
                    .into());
                }
                log_info!(
                    "[hls-download] key {} attempt {}/{} failed: {}",
                    key_uri,
                    attempts,
                    MAX_RETRIES,
                    e
                );
                let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                tokio::select! {
                    _ = cancel.cancelled() => return Err(DownloadError::Cancelled.into()),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

async fn fetch_key_once(
    client: &Client,
    key_uri: &str,
    ctx: &HlsRequestCtx<'_>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Vec<u8>, RequestFailure> {
    let (_, body) = ctx.fetch_small(client, key_uri, cancel).await?;
    if body.len() != 16 {
        return Err(DownloadError::Other(format!(
            "AES-128 key must be 16 bytes, got {} bytes from {}",
            body.len(),
            key_uri
        ))
        .into());
    }
    Ok(body.to_vec())
}

/// Parse an IV hex string (e.g. "0x1234abcd...") into 16 bytes.
fn parse_iv_hex(iv_str: &str) -> Result<[u8; 16], DownloadError> {
    let hex = iv_str
        .strip_prefix("0x")
        .or_else(|| iv_str.strip_prefix("0X"))
        .unwrap_or(iv_str);

    // 必须先确认全是 ASCII hex:下面按字节下标切片,多字节字符会切到字符中间 panic。
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
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

/// 续传检查点里记录的所选变体标识。bandwidth+resolution+去 query 的 path:
/// 变体 URI 常带会过期的 CDN 签名 token,续传时不能按完整 URI 匹配。
#[derive(Debug, Clone, PartialEq, Eq)]
struct VariantId {
    bandwidth: u64,
    resolution: Option<(u64, u64)>,
    path: String,
}

impl VariantId {
    fn of(v: &HlsVariant) -> Self {
        let path = match url::Url::parse(&v.uri) {
            Ok(u) => u.path().to_string(),
            Err(_) => v
                .uri
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .to_string(),
        };
        Self {
            bandwidth: v.bandwidth,
            resolution: v.resolution,
            path,
        }
    }

    /// `bandwidth|WxH(无分辨率为 -)|path`;path 放最后,`|` 在 URL path 里会被转义。
    fn encode(&self) -> String {
        let res = match self.resolution {
            Some((w, h)) => format!("{w}x{h}"),
            None => "-".to_string(),
        };
        format!("{}|{}|{}", self.bandwidth, res, self.path)
    }

    fn decode(s: &str) -> Option<Self> {
        let mut parts = s.splitn(3, '|');
        let bandwidth = parts.next()?.parse().ok()?;
        let res = parts.next()?;
        let resolution = if res == "-" {
            None
        } else {
            let (w, h) = res.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        };
        let path = parts.next()?.to_string();
        Some(Self {
            bandwidth,
            resolution,
            path,
        })
    }
}

/// 在重新解析出的变体列表里找回上次所选变体:先完全匹配,再按
/// (bandwidth, resolution) 唯一匹配,最后按 path 唯一匹配;均不成立返回 `None`。
fn match_saved_variant(variants: &[HlsVariant], saved: &VariantId) -> Option<usize> {
    if let Some(i) = variants.iter().position(|v| VariantId::of(v) == *saved) {
        return Some(i);
    }
    let unique = |pred: &dyn Fn(&VariantId) -> bool| -> Option<usize> {
        let mut it = variants
            .iter()
            .enumerate()
            .filter(|(_, v)| pred(&VariantId::of(v)));
        let first = it.next()?.0;
        if it.next().is_some() {
            None
        } else {
            Some(first)
        }
    };
    unique(&|v| v.bandwidth == saved.bandwidth && v.resolution == saved.resolution)
        .or_else(|| unique(&|v| v.path == saved.path))
}

/// 续传检查点:`idx:byte_offset:media_sequence[:variant]`。
fn format_resume_checkpoint(
    idx: usize,
    bytes: i64,
    media_sequence: u64,
    variant: Option<&str>,
) -> String {
    match variant {
        Some(v) => format!("{idx}:{bytes}:{media_sequence}:{v}"),
        None => format!("{idx}:{bytes}:{media_sequence}"),
    }
}

/// Parse an HLS resume checkpoint string.
///
/// 支持的格式(向后兼容):
/// - `"idx:byte_offset:media_sequence[:variant]"`(当前)
/// - `"idx:byte_offset"`(旧,media_sequence 视为未知 → `None`)
/// - `"idx"`(更早,byte_offset 视为 0)
///
/// 返回 `(saved_idx, saved_bytes, saved_media_seq)`;无法解析 idx 时返回
/// `(0, 0, None)`(等同于不 resume)。
fn parse_resume_checkpoint(s: &str) -> (usize, i64, Option<u64>) {
    let mut parts = s.splitn(4, ':');
    let idx = parts.next().and_then(|p| p.parse().ok());
    let Some(idx) = idx else {
        return (0, 0, None);
    };
    let bytes = parts.next().and_then(|b| b.parse().ok()).unwrap_or(0i64);
    let media_seq = parts.next().and_then(|m| m.parse::<u64>().ok());
    (idx, bytes, media_seq)
}

/// 检查点里的所选变体;旧格式(无第四段)返回 `None`。
fn parse_checkpoint_variant(s: &str) -> Option<VariantId> {
    VariantId::decode(s.splitn(4, ':').nth(3)?)
}

/// 是否可以沿用磁盘上的前缀续传。`saved_idx` 超过新播放列表段数说明列表
/// 变短/换了变体,前缀无法与新列表对齐,必须放弃。
fn resume_is_usable(
    saved_idx: usize,
    saved_bytes: i64,
    file_size: i64,
    segment_count: usize,
    media_seq_changed: bool,
    abandon: bool,
) -> bool {
    saved_idx > 0
        && saved_idx <= segment_count
        && file_size > 0
        && saved_bytes > 0
        && !media_seq_changed
        && !abandon
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

/// 执行一次 HLS 下载任务。
///
/// 返回值是调度层（`download_manager`）的退回协议：
/// - `None`：任务已在本函数内跑到终态（完成 / 取消 / 真实 HLS 失败），DB
///   状态与进度事件均已落定，调用方无需再做任何事。
/// - `Some(params)`：URL 仅因 `.m3u8`/`.m3u` 扩展名被误判为 HLS，实际内容
///   不是合法播放列表（[`DownloadError::NotAnHlsPlaylist`]）。只有用户原始
///   URL 的**首次**拉取（且没有续传检查点）会产生该错误；变体 playlist 与
///   已有检查点的续传已确认是 HLS，统一按真实失败上报。此时尚未创建任何
///   临时文件、写入任何字节，未消费的 `params`（含 `.ts` 归一化后的文件名）
///   原样交还——不落终态失败，由调用方把它交给普通 HTTP 下载器，并在此之前
///   把文件名恢复为 URL 派生的原始扩展名，逐字节保存该文件本身。
pub async fn run_hls_download(params: DownloadParams) -> Option<DownloadParams> {
    let task_id_log = params.task_id.clone();
    let result = match run_hls_download_inner(&params).await {
        // 完成前先跑插件后处理(未声明 finalize 阶段时零调用)与分类目录复核;成功后才写
        // 状态 3。转封装已在 inner 内完成,复核看到的是最终的 `.mp4` / `.ts`。
        Ok(total) => match crate::downloader::finalize_output(&params, ProtocolKind::Hls).await {
            Ok(replaced) => params
                .db
                .update_task_status(&params.task_id, 3, "")
                .await
                .map(|()| (total, replaced))
                .map_err(DownloadError::Db),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };

    match result {
        Ok((total, replaced)) => {
            // 插件把产物替换为另一文件时,完成帧带上新文件名与新大小。
            let (total, final_name) = match replaced {
                Some((name, size)) => (size, name),
                None => (total, String::new()),
            };
            log_info!(
                "[hls-download] task {} completed, total={} bytes",
                task_id_log,
                total
            );

            if params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: total,
                    total_bytes: total,
                    status: 3,
                    error_message: String::new(),
                    file_name: final_name,
                    segment_details: None,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("download progress receiver closed");
            }
            None
        }
        Err(DownloadError::Cancelled) => {
            log_info!("[hls-download] task {} cancelled", task_id_log);
            None
        }
        Err(DownloadError::NotAnHlsPlaylist(msg)) => {
            // 尚未落任何终态、未写任何字节——原样交还 params，由调度层
            // 退回普通 HTTP 下载器。
            log_info!(
                "[hls-download] task {} url does not resolve to a valid HLS playlist ({}), \
                 falling back to plain HTTP download",
                task_id_log,
                msg
            );
            Some(params)
        }
        Err(e) => {
            let msg = e.to_string();
            crate::logger::report_error("hls-download", "run task", &e);
            if let Err(db_error) = params.db.update_task_status(&params.task_id, 4, &msg).await {
                crate::logger::report_error(
                    "hls-download",
                    "persist terminal error status",
                    &db_error,
                );
            }

            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => (t.downloaded_bytes, t.total_bytes),
                other => {
                    crate::log_warn!(
                        "[hls-download] task {} warning: failed to read progress from DB: {:?}",
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
            None
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
    unattended: bool,
) -> Result<usize, DownloadError> {
    let auto_select_best = || -> Result<usize, DownloadError> {
        let (best_idx, best) = variants
            .iter()
            .enumerate()
            .max_by_key(|(_, v)| v.bandwidth)
            .ok_or_else(|| DownloadError::Other("no variants in master playlist".to_string()))?;
        log_info!(
            "[hls-download] task {} auto-selected variant: bandwidth={}, resolution={:?}",
            task_id,
            best.bandwidth,
            best.resolution
        );
        Ok(best_idx)
    };

    // Skip the selector entirely when there is only one variant — no point
    // asking — or when the task was created unattended (RSS / silent takeover):
    // popping a dialog nobody is around to answer just delays the download by
    // the selection timeout.
    if variants.len() <= 1 || unattended {
        log_info!(
            "[hls-download] task {} skipping quality dialog ({} variant(s), unattended={})",
            task_id,
            variants.len(),
            unattended
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
            Ok(idx as usize)
        }
    }
}

// ---------------------------------------------------------------------------
// Core logic
// ---------------------------------------------------------------------------

/// 判断写入 `seg_idx` 段字节前是否需要先(重新)写入其 EXT-X-MAP 初始化段
/// (#682)。
///
/// - 当前段没有 map(非 fMP4 段)→ 不需要。
/// - 有 map 且与磁盘上最后写入的不同(该 map 首次出现,或播放列表中途切换
///   了 init 段)→ 需要。
/// - 有 map 且与磁盘上最后写入的相同 → 已经在磁盘上,跳过(避免重复拼接
///   同一个 init 段)。
fn should_write_init(last_written: Option<&MapKey>, current: Option<&MapKey>) -> bool {
    match current {
        None => false,
        Some(c) => last_written != Some(c),
    }
}

async fn run_hls_download_inner(p: &DownloadParams) -> Result<i64, DownloadError> {
    log_info!("[hls-download] task {} starting, url={}", p.task_id, p.url);

    // Transition to status=5 (preparing)
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

    // 续传检查点先于解析读取:(1) 据此沿用上次所选变体、不再重复弹窗并保证
    // 续传前后是同一画质;(2) 已有检查点说明此 URL 早已确认是 HLS,之后解析
    // 失败必须按真实错误上报,不能退回普通 HTTP 下载把清单文本当文件存下。
    let resume_seg_key = format!("hls_resume_{}", p.task_id);
    let saved_checkpoint: Option<String> = if p.is_resume {
        p.db.get_config(&resume_seg_key).await?
    } else {
        None
    };
    let (saved_idx, saved_bytes, saved_media_seq) = saved_checkpoint
        .as_deref()
        .map(parse_resume_checkpoint)
        .unwrap_or((0, 0, None));
    let saved_variant = saved_checkpoint
        .as_deref()
        .and_then(parse_checkpoint_variant);
    let confirmed_hls = saved_checkpoint.is_some();
    // ffmpeg 可用性决定两件事:有独立音轨时是否下载并 mux,以及收尾 TS→MP4 是否走
    // 流复制(不可用才退回内存转换)。
    let ffmpeg: Option<PathBuf> = {
        let candidate = effective_ffmpeg(p);
        ffmpeg_usable(candidate)
            .await
            .then(|| candidate.to_path_buf())
    };

    let task_ctx = crate::intercept::ctx::task_ctx(p, ProtocolKind::Hls);
    let notes = ActivityNotes::of(p);
    // 本次运行所有请求(播放列表 / 分片 / 密钥 / init 段 / 独立音轨)共享同一个请求身份,
    // 被拒(401 / 403 / 410)时可由插件 `refresh` 阶段换发,后续请求立即生效。
    // 未声明 refresh 时没有刷新器,请求序列与无插件时完全一致。
    let spec = Arc::new(
        DownloadSpec::new(p.spec.clone())
            .with_refresher(Refresher::for_params(p, ProtocolKind::Hls)),
    );
    let root_ctx = HlsRequestCtx {
        spec: &spec,
        header_origin_url: &p.url,
    };
    let FetchedPlaylist {
        content,
        raw: root_raw,
    } = parse_m3u8(&p.client, &p.url, &root_ctx, &p.cancel_token)
        .await
        .map_err(RequestFailure::into_error)
        .map_err(|e| if confirmed_hls { reject_not_hls(e) } else { e })?;

    // media_playlist_url 是"实际列出 segment/key 的播放列表 URL"：
    // master→media 两级结构里是选中的 media playlist(selected_uri),
    // 直接 media 路径里就是 p.url 本身。段/密钥的同源 cookie 判定必须以
    // 它为基准——master 与 media playlist 经常跨主机(master 在主域、
    // media+segments+key 在 CDN),用 master URL 判同源会错误剥离 CDN
    // 鉴权 cookie 导致 403/401。直接 media 路径下 media_playlist_url==p.url,
    // 行为不变。
    let mut abandon_resume = false;
    let (
        segments,
        media_sequence,
        media_playlist_url,
        is_fmp4,
        chosen_variant,
        live,
        audio_uri,
        media_raw,
        target_duration,
        variant_bitrate,
    ) = match content {
        M3u8Content::Master { variants } => {
            let saved_match = saved_variant
                .as_ref()
                .and_then(|s| match_saved_variant(&variants, s));
            let variant_idx = match saved_match {
                Some(i) => {
                    log_info!(
                        "[hls-download] task {} resuming with previously selected variant {}",
                        p.task_id,
                        i
                    );
                    i
                }
                None => {
                    // 已有进度但无法确认上次画质(旧检查点且需要用户选择,或已记录
                    // 的变体在新清单里找不到):磁盘前缀可能属于另一画质,拼接会
                    // 产出错位/不可解码的文件,只能全量重下。单变体与无人值守任务
                    // 的自动选择是确定的,旧检查点照常续传。
                    if saved_idx > 0
                        && (saved_variant.is_some() || (variants.len() > 1 && !p.unattended))
                    {
                        log_info!(
                            "[hls-download] task {} cannot identify the previously selected variant, \
                                 re-downloading from scratch",
                            p.task_id
                        );
                        abandon_resume = true;
                    }
                    let plugin_pick =
                        if wants(p.interceptor.as_ref(), InterceptStage::SelectVariant) {
                            let ctx = SelectVariantCtx {
                                task: &task_ctx,
                                format: "hls",
                                variants: variants
                                    .iter()
                                    .enumerate()
                                    .map(|(index, v)| VariantCtx {
                                        index,
                                        bandwidth: v.bandwidth,
                                        width: v.resolution.map(|r| r.0),
                                        height: v.resolution.map(|r| r.1),
                                        uri: v.uri.clone(),
                                        id: None,
                                        codecs: None,
                                    })
                                    .collect(),
                            };
                            match serde_json::to_value(&ctx) {
                                Ok(value) => {
                                    run_select(
                                        p.interceptor.as_ref(),
                                        &notes,
                                        &p.cancel_token,
                                        InterceptStage::SelectVariant,
                                        value,
                                        variants.len(),
                                    )
                                    .await?
                                }
                                Err(e) => {
                                    log_warn!("[hls-download] 画质选择上下文序列化失败: {e}");
                                    None
                                }
                            }
                        } else {
                            None
                        };
                    match plugin_pick {
                        Some(i) => i,
                        None => {
                            select_variant(
                                &p.task_id,
                                &variants,
                                p.selector.as_ref(),
                                &p.cancel_token,
                                p.unattended,
                            )
                            .await?
                        }
                    }
                }
            };

            if p.cancel_token.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }

            let Some(variant) = variants.get(variant_idx) else {
                return Err(DownloadError::Other(format!(
                    "selected HLS variant {} out of range ({} variants)",
                    variant_idx,
                    variants.len()
                )));
            };
            let selected_uri = variant.uri.clone();
            let chosen = VariantId::of(variant);
            let variant_bitrate = VariantBitrate {
                average: variant.average_bandwidth,
                peak: Some(variant.bandwidth),
            };

            // 变体的 AUDIO 组带独立 URI 时需要 ffmpeg 把音轨 mux 进视频;
            // 不可用就只下载视频流并留下 warning 活动,避免静默产出无声文件。
            let audio_uri = match variant.audio_uri.as_deref() {
                Some(uri) if ffmpeg.is_some() => Some(uri.to_owned()),
                Some(_) => {
                    if saved_idx == 0 {
                        crate::log_warn!(
                            "[hls-download] task {} variant uses a separate EXT-X-MEDIA audio rendition \
                                 but ffmpeg is unavailable; only the video stream is downloaded",
                            p.task_id
                        );
                        if let Err(journal_error) = crate::task_activity::record(
                                &p.db,
                                p.sink.as_ref(),
                                &p.task_id,
                                "warning",
                                "该 HLS 流的音频是独立音轨（EXT-X-MEDIA），需要 ffmpeg 才能合并；当前未检测到可用的 ffmpeg，仅下载视频流，输出文件可能没有声音",
                                None,
                            )
                            .await
                            {
                                crate::log_error!(
                                    "[task-activity] failed to persist warning: {}",
                                    journal_error
                                );
                            }
                    }
                    None
                }
                None => None,
            };

            // 所选 variant 可能指向与 p.url 不同源的 CDN:ctx 对 Cookie 与凭据类
            // 请求头都按同源过滤,不会把原站点的会话/鉴权令牌带给第三方。
            // 此时已确认是 HLS,内容不是播放列表按真实失败处理。
            let FetchedPlaylist {
                content: media_content,
                raw: media_raw,
            } = parse_m3u8(&p.client, &selected_uri, &root_ctx, &p.cancel_token)
                .await
                .map_err(RequestFailure::into_error)
                .map_err(reject_not_hls)?;
            match media_content {
                M3u8Content::Media {
                    segments,
                    total_duration: _,
                    media_sequence,
                    has_map,
                    live,
                    target_duration,
                } => (
                    segments,
                    media_sequence,
                    selected_uri,
                    has_map,
                    Some(chosen),
                    live.then_some(target_duration),
                    audio_uri,
                    media_raw,
                    target_duration,
                    variant_bitrate,
                ),
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
            has_map,
            live,
            target_duration,
        } => (
            segments,
            media_sequence,
            p.url.clone(),
            has_map,
            None,
            live.then_some(target_duration),
            None,
            root_raw,
            target_duration,
            VariantBitrate::default(),
        ),
    };

    // 点播视频轨的插件分片过滤:被丢弃的分片不再下载,其余保持原绝对序号(AES-128
    // 隐式 IV 依赖它)。指纹随任务持久化,续传时过滤结果变化则放弃续传全量重下。
    let mut segments = segments;
    let mut is_fmp4 = is_fmp4;
    if live.is_none() {
        let (kept, fingerprint) = filter_vod_track(
            p,
            &task_ctx,
            &notes,
            SegmentTrack::Video,
            &media_playlist_url,
            &media_raw,
            segments,
            media_sequence,
            target_duration,
        )
        .await?;
        segments = kept;
        if sync_filter_fingerprint(
            &p.db,
            &format!("hls_filter_{}", p.task_id),
            p.is_resume,
            &fingerprint,
        )
        .await?
        {
            log_info!(
                "[hls-download] task {} plugin segment filter changed across resume, \
                 re-downloading from scratch",
                p.task_id
            );
            abandon_resume = true;
        }
        is_fmp4 = segments.iter().any(|s| s.map.is_some());
    }

    // 独立音轨的播放列表在创建任何文件之前解析:拿不到音轨就是真实失败,不能
    // 静默退化成无声输出。直播视频搭配直播音轨时二者同步录制;点播视频搭配直播
    // 音轨无法对齐(按点播下载只会得到快照),直接失败;直播视频搭配点播音轨时
    // 丢弃音轨并留下 warning 活动。
    let audio_playlist: Option<AudioPlaylist> = match audio_uri {
        Some(uri) => match parse_m3u8(&p.client, &uri, &root_ctx, &p.cancel_token)
            .await
            .map_err(RequestFailure::into_error)
            .map_err(reject_not_hls)?
        {
            FetchedPlaylist {
                content:
                    M3u8Content::Media {
                        segments: audio_segments,
                        media_sequence: audio_sequence,
                        live: audio_live,
                        target_duration: audio_target_duration,
                        ..
                    },
                raw: audio_raw,
            } => match (live.is_some(), audio_live) {
                (false, true) => {
                    return Err(DownloadError::Other(
                        "HLS 独立音轨播放列表缺少 EXT-X-ENDLIST，无法下载".to_string(),
                    ));
                }
                (true, false) => {
                    record_task_warning(
                        &p.db,
                        p.sink.as_ref(),
                        &p.task_id,
                        "直播流的独立音轨播放列表已有 EXT-X-ENDLIST，无法与直播视频同步录制；仅录制视频流，输出文件可能没有声音",
                    )
                    .await;
                    None
                }
                _ => {
                    let mut audio_segments = audio_segments;
                    let mut filter_changed = false;
                    if !audio_live {
                        let (kept, fingerprint) = filter_vod_track(
                            p,
                            &task_ctx,
                            &notes,
                            SegmentTrack::Audio,
                            &uri,
                            &audio_raw,
                            audio_segments,
                            audio_sequence,
                            audio_target_duration,
                        )
                        .await?;
                        audio_segments = kept;
                        filter_changed = sync_filter_fingerprint(
                            &p.db,
                            &format!("hls_audio_filter_{}", p.task_id),
                            p.is_resume,
                            &fingerprint,
                        )
                        .await?;
                    }
                    Some(AudioPlaylist {
                        uri,
                        segments: audio_segments,
                        media_sequence: audio_sequence,
                        live_target: audio_live.then_some(audio_target_duration),
                        raw: audio_raw,
                        filter_changed,
                    })
                }
            },
            FetchedPlaylist {
                content: M3u8Content::Master { .. },
                ..
            } => {
                return Err(DownloadError::Other(
                    "nested master playlist not supported".to_string(),
                ));
            }
        },
        None => None,
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

    // 文件名沿用 DownloadManager 预订的 `.ts`(dedup / 兄弟任务协调都按它做),
    // fMP4/CMAF 播放列表(存在 EXT-X-MAP)也不例外——其输出是分片 MP4 而非
    // MPEG-TS,但改扩展名统一放到完成后的 finalize(见 `remux_ts_to_mp4` 的
    // is_fmp4 分支:跳过 TS→MP4 转换,走同一套占名协议直接改名 `.mp4`)。#682
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

    p.db.update_task_status(&p.task_id, 1, "").await?;

    // Notify the host: downloading started with file name
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
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));

    // 独立音轨的临时文件紧邻视频:`<stem>.audio.m4a.fdownloading`(与 DASH 音轨
    // sidecar 同名约定,任务删除时按同一规则清理)。
    let audio_track: Option<AudioTrack> = audio_playlist.map(|audio| AudioTrack {
        tag: audio_track_tag(&audio.uri),
        playlist_url: audio.uri,
        segments: audio.segments,
        media_sequence: audio.media_sequence,
        live_target: audio.live_target,
        raw: audio.raw,
        filter_changed: audio.filter_changed,
        temp_path: PathBuf::from(format!(
            "{}{}",
            build_audio_path(&dest_path).display(),
            TEMP_EXT
        )),
        resume_key: audio_resume_key(&p.task_id),
    });
    let audio_temp_path = audio_track.as_ref().map(|t| t.temp_path.clone());

    // Ensure parent directory exists
    output::ensure_parent(&temp_path).await?;

    // 直播:没有总长度可言,从当前播放列表起持续录制,停止后按点播同一套收尾定稿。
    if let Some(target_duration) = live {
        return run_live_recording(
            p,
            LiveRecording {
                segments,
                media_sequence,
                target_duration,
                playlist_url: media_playlist_url,
                is_fmp4,
                playlist_raw: media_raw,
                variant_tag: chosen_variant.as_ref().map(VariantId::encode),
                abandon_resume,
                audio: audio_track,
                ffmpeg,
                temp_path,
                dest_path,
                actual_name,
                save_dir,
                resume_key: resume_seg_key,
                spec,
            },
        )
        .await;
    }

    // --- HLS resume support ---
    // On resume, check if we have a saved segment index from a previous run.
    // If so, skip already-downloaded segments and open the temp file in append mode.
    let (mut file, skip_segments, mut downloaded_bytes) = if p.is_resume {
        // 检查点(saved_*)已在解析前读出。当前格式
        // "idx:byte_offset:media_sequence[:variant]";向后兼容旧格式
        // "idx:byte_offset"(缺 media_sequence 视为未知)与更早的 "idx"
        // (缺 byte_offset 视为 0,不截断)。
        //
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
        if saved_idx > segment_count {
            log_info!(
                "[hls] task {} saved segment index {} exceeds playlist length {}, \
                 abandoning resume and re-downloading from scratch",
                p.task_id,
                saved_idx,
                segment_count
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
        if resume_is_usable(
            saved_idx,
            saved_bytes,
            file_size,
            segment_count,
            media_seq_changed,
            abandon_resume,
        ) {
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
        p.db.delete_config(&resume_seg_key).await?;
        (File::create(&temp_path).await?, 0, 0i64)
    };

    // 所选变体随检查点一起持久化(与 hls_resume_<id> 同生命周期,任务删除时
    // 一并清理)。从头下载时先写一个零进度检查点,使首段落盘前的自动重试
    // 也能沿用同一画质而不是再次弹窗。
    let variant_tag: Option<String> = chosen_variant.as_ref().map(VariantId::encode);
    if skip_segments == 0
        && let Some(tag) = variant_tag.as_deref()
    {
        p.db.set_config(
            &resume_seg_key,
            &format_resume_checkpoint(0, 0, media_sequence, Some(tag)),
        )
        .await?;
    }

    let key_cache: KeyCache = Arc::new(Mutex::new(HashMap::new()));
    let mut last_report = std::time::Instant::now();

    // -----------------------------------------------------------------------
    // Bounded-concurrency segment download.
    //
    // Each remaining segment downloads + decrypts on its own task. Its
    // connection permit (`Semaphore`, `concurrency` permits) is returned as
    // soon as the body is in memory; the buffer then waits in the reorder
    // window (`ReorderWindow`: bounded both in segments ahead of the writer
    // and in buffered bytes), so a slow head segment no longer idles the other
    // connections. A single writer — this function — drains finished segments
    // **strictly in `seg_idx` order** via a `BTreeMap`. Keeping all writes on
    // one task guarantees:
    //   • one sequential append into one temp file — no per-segment part
    //     files and no merge pass; byte order identical to a sequential run,
    //   • `downloaded_bytes` is accumulated exactly once per segment (no
    //     double-counting / loss under concurrency),
    //   • the resume checkpoint advances monotonically by completed prefix.
    // Speed is measured from received body bytes (`SegmentMeter`), and the
    // speed limiter throttles those reads rather than the writes.
    // Each task computes its IV from its own `seg_idx`, so AES-128-CBC
    // decryption stays correct regardless of completion order.
    // -----------------------------------------------------------------------
    let first_idx = skip_segments;
    let remaining = segment_count.saturating_sub(first_idx);
    let concurrency = cap_connections(
        hls_concurrency(p.segment_count, remaining),
        p.connection_cap,
    );
    log_info!(
        "[hls-download] task {} downloading {} remaining segment(s) with concurrency {}",
        p.task_id,
        remaining,
        concurrency
    );

    // -----------------------------------------------------------------------
    // EXT-X-MAP(fMP4/CMAF 初始化段,#682)预取。
    //
    // init 段(ftyp+moov)与媒体段走不同的写入语义——同一个 init 段通常被
    // 多个连续媒体段共享,必须只写入一次而非每段重复拼接。为保持单一有序
    // writer 的现有并发模型不变,这里在生成媒体段下载任务【之前】,顺序
    // 预取本次运行(`first_idx..segment_count`,即尚未落盘的剩余段)会用到
    // 的每个不同 init 段字节,缓存进 `map_bytes`;写入时机与去重判断见下方
    // writer 循环里的 `should_write_init`。
    //
    // 只预取剩余段用到的 map(而非整份播放列表的所有 map):已经落盘的前缀
    // 段对应的 init 段字节已经在磁盘上,无需重新下载。
    //
    // 复用 `download_segment_with_retry`(与媒体段完全相同的重试/退避/
    // Range 请求路径),`seg_idx` 传 `usize::MAX` 仅用于失败时的日志诊断,
    // 不代表真实段序号。
    let meter = SegmentMeter::new(p.speed_limiter.clone());
    // Segments can stay in the body for longer than a completed writer chunk.
    // Sample independently, including while no byte geometry is available.
    // 视频与独立音轨的分段大小证据(在途响应头 / 已下载待写盘 / 已写盘 / 续传前缀)汇总为
    // 任务进度,并据此估算总大小。
    let video_progress = Arc::new(TrackProgress::new(&segments));
    video_progress.resume(downloaded_bytes, skip_segments);
    let audio_progress: Option<Arc<TrackProgress>> = audio_track
        .as_ref()
        .map(|track| Arc::new(TrackProgress::new(&track.segments)));
    let progress = Arc::new(TaskProgress::new(
        Arc::clone(&video_progress),
        audio_progress.clone(),
        variant_bitrate,
    ));
    let mut sampler = spawn_progress_sampler(
        p.progress_tx.clone(),
        p.task_id.clone(),
        meter.clone(),
        ProgressSource::Vod(Arc::clone(&progress)),
        concurrency as u32,
    );
    let seg_ctx = HlsRequestCtx {
        spec: &spec,
        header_origin_url: &p.url,
    };
    let map_bytes: HashMap<MapKey, Vec<u8>> = if is_fmp4 {
        match prefetch_init_segments(
            &p.client,
            &segments,
            first_idx,
            &seg_ctx,
            &p.cancel_token,
            &p.task_id,
            &meter,
            &p.db,
            p.sink.as_ref(),
        )
        .await
        {
            Ok(maps) => maps,
            Err(e) => {
                sampler.abort();
                return Err(e);
            }
        }
    } else {
        HashMap::new()
    };

    // 独立音轨与视频并行下载。音轨出现真实错误时取消整个任务(视频 writer 随之
    // 以取消退出,收尾处用音轨的错误替换);用户取消/暂停时两边都以取消结束。
    let audio_handle: Option<tokio::task::JoinHandle<Result<i64, DownloadError>>> =
        audio_track.zip(audio_progress).map(|(track, written)| {
            let run = AudioRun {
                track,
                client: p.client.clone(),
                spec: Arc::clone(&spec),
                header_origin_url: p.url.clone(),
                cancel: p.cancel_token.child_token(),
                task_id: p.task_id.clone(),
                db: p.db.clone(),
                sink: p.sink.clone(),
                key_cache: key_cache.clone(),
                meter: meter.clone(),
                written,
                segment_limit: p.segment_count,
                connection_cap: p.connection_cap,
                is_resume: p.is_resume,
            };
            let parent = p.cancel_token.clone();
            tokio::spawn(async move {
                let result = run_audio_track(run).await;
                if let Err(e) = &result
                    && !matches!(e, DownloadError::Cancelled)
                {
                    parent.cancel();
                }
                result
            })
        });

    let semaphore = Arc::new(Semaphore::new(concurrency));
    let window = Arc::new(ReorderWindow::new(first_idx, concurrency));
    // 已派发未写盘的段数不超过窗口段数上限,结果入队从不阻塞。
    let (result_tx, mut result_rx) = mpsc::channel::<SegmentOutcome>(window.max_ahead);

    let jobs: Vec<SegmentJob> = segment_jobs(&segments, first_idx);
    let shared = Arc::new(SegmentShared {
        client: p.client.clone(),
        spec: Arc::clone(&spec),
        playlist_url: media_playlist_url.clone(),
        header_origin_url: p.url.clone(),
        cancel: p.cancel_token.clone(),
        task_id: p.task_id.clone(),
        key_cache: key_cache.clone(),
        meter: meter.clone(),
        db: p.db.clone(),
        sink: p.sink.clone(),
        progress: Some(Arc::clone(&video_progress)),
    });
    // dispatcher 持有唯一的发送端;它与其子任务全部结束后 channel 关闭,
    // writer 的 recv 循环才不会在末尾挂起。
    let dispatcher = tokio::spawn(dispatch_segments(
        jobs,
        shared,
        semaphore,
        Arc::clone(&window),
        result_tx,
    ));

    // In-order writer: buffer out-of-order completions and flush the contiguous
    // prefix starting at `next_to_write`.
    let mut pending: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut next_to_write = first_idx;
    // 最近一次落库的检查点覆盖到的段序号(不含)。
    let mut checkpointed = first_idx;
    let mut last_checkpoint = std::time::Instant::now();
    let mut fatal_error: Option<DownloadError> = None;
    // EXT-X-MAP(#682):跟踪"磁盘上最后一次写入的 init 段"(uri, byte_range)。
    // 全新下载(first_idx==0)时磁盘上还没有任何 init 段 → `None`。断点续传
    // (first_idx>0)时没有把"历史 init 段身份"持久化进 checkpoint(见上方
    // 预取处的说明,故意不引入新持久化状态)——保守假设"上一个已完整落盘的
    // 段(segments[first_idx-1])的 map 就是磁盘上最后写入的 init 段"。这是
    // 稳妥的:map 字段在解析期已按 RFC 8216 粘性传播(parse_m3u8_bytes),同一
    // map 的连续段只在其首次出现时触发写入,与首次下载时 writer 的行为一致。
    let mut last_written_map: Option<MapKey> = if first_idx == 0 {
        None
    } else {
        segments
            .get(first_idx - 1)
            .and_then(|s| s.map.as_ref())
            .map(|m| (m.uri.clone(), m.byte_range))
    };

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
                    // Channel closed before producing `next_to_write`: every
                    // worker exited without delivering it. Besides a user
                    // cancel this means a worker died (panic), which must
                    // surface as a failure instead of leaving the task in
                    // "downloading" with nothing running.
                    if fatal_error.is_none() {
                        fatal_error = Some(if p.cancel_token.is_cancelled() {
                            DownloadError::Cancelled
                        } else {
                            DownloadError::Other(format!(
                                "HLS segment workers exited unexpectedly before segment {}",
                                next_to_write
                            ))
                        });
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

            // EXT-X-MAP(#682):若本段生效的 init 段与磁盘上最后写入的不同
            // (首次出现,或播放列表中途切换了 init 段),需要在本段媒体字节
            // 之前先写入 init 字节。两者作为同一次"写入尝试"共享下面的
            // seg_start_pos 回退点与 downloaded_bytes 计数,任一部分失败都整体
            // 回退(避免只落盘半个 init 段或"init 有、媒体段无"的不一致状态)。
            let current_map_key = segments[seg_idx]
                .map
                .as_ref()
                .map(|m| (m.uri.clone(), m.byte_range));
            let init_chunk: Option<&[u8]> =
                if should_write_init(last_written_map.as_ref(), current_map_key.as_ref()) {
                    match current_map_key.as_ref().and_then(|k| map_bytes.get(k)) {
                        Some(data) => Some(data.as_slice()),
                        None => {
                            // 理论不可达:预取阶段已为 first_idx..segment_count 内
                            // 出现的每个不同 map 下载好字节;缺失说明预取逻辑与
                            // 这里的判定不一致(bug)。报错而非静默跳过 init 段——
                            // 跳过会产出不可解码的 fMP4 输出却标记任务完成。
                            fatal_error = Some(DownloadError::Other(format!(
                                "internal error: init segment for {:?} was not prefetched",
                                current_map_key
                            )));
                            break 'writer;
                        }
                    }
                } else {
                    None
                };

            // Write to file (init chunk, if any, then the media segment bytes).
            // 限速已在读取 body 时生效,这里整块追加。
            //
            // seg_start_pos 是本次写入前文件的逻辑长度。resume 时文件已被
            // truncate 到恰好 safe_size(== 初始 downloaded_bytes),此后每次
            // 迭代以 append 方式精确追加写入的字节数,故文件磁盘长度始终等于
            // downloaded_bytes —— 用它作为出错回退点是准确的。
            let seg_start_pos = downloaded_bytes;
            let mut written_len: i64 = 0;
            let mut write_result: Result<(), std::io::Error> = Ok(());
            for chunk in init_chunk
                .into_iter()
                .chain(std::iter::once(output_data.as_slice()))
            {
                if let Err(e) = file.write_all(chunk).await {
                    write_result = Err(e);
                    break;
                }
                written_len += chunk.len() as i64;
            }

            if let Err(e) = write_result {
                // 写入中途失败(常见:磁盘满 ENOSPC)。本次迭代(可能含 init 段
                // + 媒体段)已部分写入,先把文件回退到写入前的长度,避免残留
                // 半截数据污染后续 resume(与 dash_downloader 的
                // set_len(start_pos) 兜底一致)。回退失败仅记录日志,不掩盖
                // 原始写入错误。
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
                fatal_error = Some(write_failure(e));
                break 'writer;
            }

            if init_chunk.is_some() {
                last_written_map = current_map_key;
            }
            downloaded_bytes += written_len;
            next_to_write += 1;
            video_progress.wrote(seg_idx, written_len, downloaded_bytes);
            window.written(next_to_write, output_data.len());

            // Save resume checkpoint for HLS resume support.
            // Format: "next_seg_idx:total_bytes_written:media_sequence" — on resume
            // we truncate to this byte offset to discard any partially-written
            // segment data,并比对 media_sequence 以保证续传段的 IV 计算与首次一致。
            // 因为按 seg_idx 顺序写盘,next_to_write 即"已完整落盘的连续前缀
            // 长度",检查点始终对应一段完整、可安全续传的字节边界。按
            // CHECKPOINT_INTERVAL 节流(落后只让续传多截掉几段重下),退出时补写。
            if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                if let Err(error) = save_checkpoint(
                    &mut file,
                    &p.db,
                    &resume_seg_key,
                    &format_resume_checkpoint(
                        next_to_write,
                        downloaded_bytes,
                        media_sequence,
                        variant_tag.as_deref(),
                    ),
                )
                .await
                {
                    fatal_error = Some(error);
                    p.cancel_token.cancel();
                    break 'writer;
                }
                checkpointed = next_to_write;
                last_checkpoint = std::time::Instant::now();
                persist_display_total(&p.db, &p.task_id, progress.snapshot().1).await;
            }

            // Progress reporting (every 200ms)
            if last_report.elapsed().as_millis() >= 200 {
                let (written, total) = progress.snapshot();
                if p.progress_tx
                    .send(hls_progress(
                        &p.task_id,
                        &meter,
                        written,
                        total,
                        concurrency as u32,
                    ))
                    .await
                    .is_err()
                {
                    tracing::debug!("download progress receiver closed");
                    p.cancel_token.cancel();
                    fatal_error = Some(DownloadError::Cancelled);
                    break 'writer;
                }
                last_report = std::time::Instant::now();
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
    match dispatcher.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if matches!(fatal_error, None | Some(DownloadError::Cancelled)) {
                fatal_error = Some(error);
            } else {
                crate::logger::report_warning("hls-download", "join_segment_dispatcher", &error);
            }
        }
        Err(error) if error.is_cancelled() && p.cancel_token.is_cancelled() => {
            tracing::debug!("HLS segment dispatcher cancelled");
        }
        Err(error) => {
            crate::logger::report_error("hls-download", "join_segment_dispatcher", &error);
            if matches!(fatal_error, None | Some(DownloadError::Cancelled)) {
                fatal_error = Some(DownloadError::Other(format!(
                    "HLS segment dispatcher failed: {error}"
                )));
            }
        }
    }
    // 音轨在采样器停止之前收尾:视频先完成时,音轨剩余进度仍要持续上报。
    let audio_result = match audio_handle {
        Some(handle) => Some(match handle.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() && p.cancel_token.is_cancelled() => {
                tracing::debug!("HLS audio worker cancelled");
                Err(DownloadError::Cancelled)
            }
            Err(error) => {
                crate::logger::report_error("hls-download", "join_audio_worker", &error);
                Err(DownloadError::Other(format!(
                    "HLS audio track task failed: {error}"
                )))
            }
        }),
        None => None,
    };
    sampler.abort();
    if let Err(error) = (&mut sampler.0).await {
        if error.is_cancelled() {
            tracing::debug!("HLS progress sampler stopped");
        } else {
            crate::logger::report_error("hls-download", "join_progress_sampler", &error);
            if matches!(fatal_error, None | Some(DownloadError::Cancelled)) {
                fatal_error = Some(DownloadError::Other(format!(
                    "HLS progress sampler failed: {error}"
                )));
            }
        }
    }
    let mut audio_bytes = 0i64;
    match audio_result {
        Some(Ok(n)) => audio_bytes = n,
        Some(Err(DownloadError::Cancelled)) => {
            if fatal_error.is_none() {
                fatal_error = Some(DownloadError::Cancelled);
            }
        }
        // 音轨的真实错误触发了任务级取消,视频侧只会看到 Cancelled:以音轨错误为准。
        Some(Err(e)) => {
            if matches!(fatal_error, None | Some(DownloadError::Cancelled)) {
                fatal_error = Some(e);
            }
        }
        None => {}
    }

    let checkpoint = format_resume_checkpoint(
        next_to_write,
        downloaded_bytes,
        media_sequence,
        variant_tag.as_deref(),
    );
    if let Some(err) = fatal_error {
        // Persist whatever fully-written prefix we have so a later resume can
        // continue from there (matches the sequential cancel path). 检查点只在
        // flush 成功后补写:文件里必须已有检查点声明的全部字节。
        match file.flush().await {
            Err(error) => {
                if matches!(err, DownloadError::Cancelled) {
                    return Err(write_failure(error));
                }
                crate::logger::report_warning("hls-download", "flush_failed_download", &error);
            }
            Ok(()) => {
                if next_to_write > checkpointed
                    && let Err(error) = p.db.set_config(&resume_seg_key, &checkpoint).await
                {
                    crate::logger::report_warning(
                        "hls-download",
                        "persist_resume_checkpoint",
                        &error,
                    );
                }
            }
        }
        let (written, total) = progress.snapshot();
        if let Err(error) = p.db.update_task_progress(&p.task_id, written).await {
            crate::logger::report_warning("hls-download", "persist_failed_progress", &error);
        }
        persist_display_total(&p.db, &p.task_id, total).await;
        return Err(err);
    }

    file.flush().await?;
    // 收尾(改名 / remux)失败时以完整检查点续传,不必重下尾部分段。
    if next_to_write > checkpointed {
        p.db.set_config(&resume_seg_key, &checkpoint).await?;
    }
    drop(file);

    // 全部分段已落盘:总量从估值收敛为实际字节(进度 = 100%),先落库再上报。manager
    // 对下载中帧有 500ms 节流,最后一帧若被吞,转封装 / mux 期间会一直停在 9x%;
    // 携带文件名的帧必定转发(名字不变,只用于越过节流)。
    let total_written = downloaded_bytes + audio_bytes;
    persist_display_total(&p.db, &p.task_id, total_written).await;
    if p.progress_tx
        .send(ProgressUpdate {
            file_name: actual_name.clone(),
            ..hls_progress(
                &p.task_id,
                &meter,
                total_written,
                total_written,
                concurrency as u32,
            )
        })
        .await
        .is_err()
    {
        tracing::debug!("download progress receiver closed");
    }

    finalize_recording(
        p,
        FinalizeInput {
            temp_path,
            dest_path,
            save_dir,
            audio_temp_path,
            ffmpeg,
            actual_name,
            resume_key: resume_seg_key,
            is_fmp4,
            downloaded_bytes,
            audio_bytes,
        },
        &p.cancel_token,
    )
    .await
}

/// [`finalize_recording`] 的输入:录制阶段结束后落盘产物的全部事实。
struct FinalizeInput {
    temp_path: PathBuf,
    dest_path: PathBuf,
    save_dir: PathBuf,
    audio_temp_path: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
    actual_name: String,
    /// 视频轨续传检查点的 config 键,收尾成功后清理。
    resume_key: String,
    is_fmp4: bool,
    downloaded_bytes: i64,
    audio_bytes: i64,
}

/// 点播下载与直播录制共用的收尾:音轨 mux → 占名改名 → TS→MP4 remux,返回最终字节数。
///
/// 调用方必须已 flush 并关闭视频 temp 文件。`cancel` 只约束 ffmpeg mux / remux:
/// 点播沿用任务令牌(暂停/取消时保留 temp 供续传);直播被用户停止时任务令牌已
/// 取消,调用方传入独立令牌,使已录内容仍能定稿。
async fn finalize_recording(
    p: &DownloadParams,
    input: FinalizeInput,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<i64, DownloadError> {
    let FinalizeInput {
        temp_path,
        dest_path,
        save_dir,
        audio_temp_path,
        ffmpeg,
        actual_name,
        resume_key: resume_seg_key,
        is_fmp4,
        downloaded_bytes,
        audio_bytes,
    } = input;

    // Save final progress
    let total_written = downloaded_bytes + audio_bytes;
    p.db.update_task_progress(&p.task_id, total_written).await?;

    // 独立音轨:两份 temp 直接 ffmpeg 流复制成 mp4,产物取代 .ts 输出。mux 过程中
    // 暂停/取消时 temp 与两个续传检查点原样保留,恢复后只需重做 mux。
    if let Some(audio_temp) = audio_temp_path.as_deref()
        && let Some(ffmpeg_bin) = ffmpeg.as_deref()
    {
        match mux_video_audio(
            p,
            &temp_path,
            audio_temp,
            &actual_name,
            total_written,
            ffmpeg_bin,
            &resume_seg_key,
            cancel,
        )
        .await
        {
            Ok((mp4_path, mp4_size)) => {
                log_info!(
                    "[hls-download] task {} video+audio muxed into {}",
                    p.task_id,
                    mp4_path.display()
                );
                let mp4_file_name = mp4_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("output.mp4")
                    .to_string();
                if p.progress_tx
                    .send(ProgressUpdate {
                        task_id: p.task_id.clone(),
                        downloaded_bytes: mp4_size,
                        total_bytes: mp4_size,
                        status: 1,
                        error_message: String::new(),
                        file_name: mp4_file_name,
                        segment_details: None,
                        ..Default::default()
                    })
                    .await
                    .is_err()
                {
                    tracing::debug!("download progress receiver closed");
                }
                return Ok(mp4_size);
            }
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(e) => {
                // 合并失败:退回仅视频输出,音轨保留为 sidecar 并留下 warning 活动。
                let sidecar = build_audio_path(&dest_path);
                let moved = tokio::fs::rename(audio_temp, &sidecar).await.is_ok();
                crate::log_warn!(
                    "[hls-download] task {} audio/video mux failed: {}; keeping video only",
                    p.task_id,
                    e
                );
                let message = if moved {
                    format!(
                        "音频轨与视频合并失败（{e}），输出仅含视频；音频轨已保存为 {}",
                        sidecar.display()
                    )
                } else {
                    format!("音频轨与视频合并失败（{e}），输出仅含视频，输出文件可能没有声音")
                };
                if let Err(journal_error) = crate::task_activity::record(
                    &p.db,
                    p.sink.as_ref(),
                    &p.task_id,
                    "warning",
                    message,
                    None,
                )
                .await
                {
                    crate::log_error!(
                        "[task-activity] failed to persist warning: {}",
                        journal_error
                    );
                }
                if let Err(error) = p.db.delete_config(&audio_resume_key(&p.task_id)).await {
                    crate::logger::report_warning(
                        "hls-download",
                        "remove_audio_checkpoint",
                        &error,
                    );
                }
            }
        }
    }

    // Clean up HLS resume marker on successful completion
    if let Err(error) = p.db.delete_config(&resume_seg_key).await {
        crate::logger::report_warning("hls-download", "remove_checkpoint", &error);
    }

    // 完成期占名:与 HTTP/ED2K 相同的 create_new 不覆盖语义。原名被占用时
    // dedup 换名(overwrite 策略只对原名删除旧文件),并把最终文件名写回 DB。
    let avoid = sibling_avoid(p).await;
    let chosen = claim_final_name(&temp_path, &save_dir, &actual_name, &p.overwrite, &avoid)
        .await
        .map_err(|e| {
            DownloadError::Other(format!(
                "failed to rename {} -> {}: {}",
                temp_path.display(),
                dest_path.display(),
                e
            ))
        })?;
    let dest_path = if chosen == actual_name {
        dest_path
    } else {
        log_info!(
            "[hls-download] task {} destination '{}' is taken; finalized as '{}'",
            p.task_id,
            actual_name,
            chosen
        );
        if let Err(e) = p.db.set_task_file_name(&p.task_id, &chosen).await {
            log_info!(
                "[hls-download] task {} failed to persist renamed file '{}': {}",
                p.task_id,
                chosen,
                e
            );
        }
        if p.progress_tx
            .send(ProgressUpdate {
                task_id: p.task_id.clone(),
                downloaded_bytes,
                // 录制已结束,改名帧上的字节就是待定稿产物的实际大小,不能退回「未知」。
                total_bytes: downloaded_bytes,
                status: 1,
                error_message: String::new(),
                file_name: chosen.clone(),
                segment_details: None,
                ..Default::default()
            })
            .await
            .is_err()
        {
            tracing::debug!("download progress receiver closed");
        }
        save_dir.join(&chosen)
    };

    log_info!(
        "[hls-download] task {} renamed {} -> {}",
        p.task_id,
        temp_path.display(),
        dest_path.display()
    );

    // 非 fMP4:TS→MP4 转换(ffmpeg 可用时流复制,否则内存转换);fMP4/CMAF(#682):
    // 内容已是分片 MP4,只走占名协议把 `.ts` 改名成 `.mp4`。两者失败都保留 `.ts`
    // 并按原名完成。
    if let Some(mp4_path) = remux_ts_to_mp4(
        &dest_path,
        &p.task_id,
        &p.overwrite,
        is_fmp4,
        ffmpeg.as_deref(),
        cancel,
    )
    .await
    {
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
                if let Err(error) = tokio::fs::remove_file(&dest_path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
                }
                if p.progress_tx
                    .send(ProgressUpdate {
                        task_id: p.task_id.clone(),
                        downloaded_bytes: mp4_size,
                        total_bytes: mp4_size,
                        // 这里只更新最终文件名；外层持久化完成状态后才发终帧。
                        status: 1,
                        error_message: String::new(),
                        file_name: mp4_file_name,
                        segment_details: None,
                        ..Default::default()
                    })
                    .await
                    .is_err()
                {
                    tracing::debug!("download progress receiver closed");
                }
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
                if let Err(error) = tokio::fs::remove_file(&mp4_path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
                }
            }
        }
    }

    Ok(downloaded_bytes)
}

// ---------------------------------------------------------------------------
// TS → MP4 remux (best-effort)
// ---------------------------------------------------------------------------

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

/// `overwrite` 允许替换 `<stem>.mp4` 时:同名 `.mp4` 已作为普通最终文件存在
/// 不触发编号改名——保留原名,占名遇 AlreadyExists 时删除旧文件后重试一次;
/// 目录/删除失败仍走既有失败路径(保留 .ts)。
///
/// `is_fmp4`(#682):播放列表带 EXT-X-MAP,`.ts` 里装的已经是分片 MP4
/// (ftyp+moov+[moof+mdat]*),不做 TS→MP4 转换,跳过空间预检,只按同一
/// 套 dedup + 原子占名协议把文件改名为 `.mp4`;失败同样保留 `.ts`。
///
/// `ffmpeg = Some`:TS→MP4 优先走 ffmpeg 流复制(`-c copy`);取消时保留 `.ts`。
/// ffmpeg 不可用或执行失败时走 [`crate::ts_remux`] 流式重封装:两遍顺序读 TS,
/// 内存只与单帧大小相关,不随文件体积增长,因此不设体积上限。
async fn remux_ts_to_mp4(
    ts_path: &std::path::Path,
    task_id: &str,
    overwrite: &crate::file_exists::OverwritePolicy,
    is_fmp4: bool,
    ffmpeg: Option<&std::path::Path>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Option<PathBuf> {
    let ext = ts_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !ext.eq_ignore_ascii_case("ts") {
        return None;
    }

    let file_len = match tokio::fs::metadata(ts_path).await {
        Ok(m) => m.len(),
        Err(_) => return None,
    };

    let parent = ts_path.parent()?;

    // ENOSPC 预检:空间不足时跳过 remux,走既有"保留 .ts"降级路径。
    // fMP4 只改名,不占额外空间,跳过。
    let avail = if is_fmp4 {
        None
    } else {
        crate::disk_space::available_space_checked(parent.to_path_buf()).await
    };
    if !is_fmp4 && !remux_space_ok(avail, file_len) {
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
    let allow_overwrite = overwrite.permits(&desired_name);
    let unique_name = dedup_filename(
        parent,
        &desired_name,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
        overwrite,
    )
    .await;
    let mp4_path = parent.join(&unique_name);

    let ts_owned = ts_path.to_owned();
    let mp4_owned = mp4_path.clone();
    let mp4_tmp = mp4_path.with_extension("mp4.tmp");
    let mp4_tmp_inner = mp4_tmp.clone();

    let mut ffmpeg_done = false;
    if !is_fmp4 && let Some(ffmpeg_bin) = ffmpeg {
        match ffmpeg_copy_to_mp4(ts_path, None, &mp4_tmp, file_len, cancel, ffmpeg_bin).await {
            Ok(()) => ffmpeg_done = true,
            Err(DownloadError::Cancelled) => return None,
            Err(e) => {
                log_info!(
                    "[hls] task {} ffmpeg remux failed: {}, falling back to streaming remux",
                    task_id,
                    e
                );
            }
        }
    }
    let cancel_inner = cancel.clone();
    match tokio::task::spawn_blocking(move || -> Result<(), std::io::Error> {
        // fMP4:待改名的源就是 .ts 本身;否则转换产物先落 tmp。
        let src = if is_fmp4 {
            ts_owned.clone()
        } else if ffmpeg_done {
            mp4_tmp_inner.clone()
        } else {
            crate::ts_remux::remux_file(&ts_owned, &mp4_tmp_inner, &cancel_inner)?;
            mp4_tmp_inner.clone()
        };
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
            // overwrite 模式:dedup 对已存在的普通 mp4 保留了原名,占名必然
            // AlreadyExists——删除旧文件后重试占名一次(覆盖旧文件)。目标是
            // 目录、删除失败或二次抢占时维持既有失败路径(保留 .ts)。
            let overwrote = allow_overwrite
                && e.kind() == std::io::ErrorKind::AlreadyExists
                && mp4_owned.is_file()
                && std::fs::remove_file(&mp4_owned).is_ok()
                && std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&mp4_owned)
                    .map(drop)
                    .is_ok();
            if !overwrote {
                if !is_fmp4
                    && let Err(error) = std::fs::remove_file(&mp4_tmp_inner)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
                }
                return Err(e);
            }
            log_info!(
                "[hls] overwrote existing '{}' (file_exists_behavior=overwrite)",
                mp4_owned.display()
            );
        }
        if let Err(e) = std::fs::rename(&src, &mp4_owned) {
            // 只清自己的占位与 tmp,fMP4 的源 .ts 是下载数据本身,绝不删。
            if let Err(error) = std::fs::remove_file(&mp4_owned)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
            }
            if !is_fmp4
                && let Err(error) = std::fs::remove_file(&mp4_tmp_inner)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
            }
            return Err(e);
        }
        Ok(())
    })
    .await
    {
        Ok(Ok(())) => {
            if is_fmp4 {
                log_info!("[hls] task {} fMP4 output renamed .ts -> .mp4", task_id);
            } else {
                let mode = if ffmpeg_done { "ffmpeg" } else { "streaming" };
                log_info!("[hls] task {} remuxed TS -> MP4 ({})", task_id, mode);
            }
            Some(mp4_path)
        }
        Ok(Err(e)) => {
            if cancel.is_cancelled() {
                tracing::debug!(task_id, "HLS remux cancelled; keeping .ts");
            } else {
                log_info!(
                    "[hls] task {} MP4 remux failed: {}, keeping .ts",
                    task_id,
                    e
                );
            }
            if let Err(error) = tokio::fs::remove_file(&mp4_tmp).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
            }
            None
        }
        Err(e) => {
            if e.is_cancelled() {
                tracing::debug!(task_id, "HLS remux worker cancelled; keeping .ts");
            } else {
                crate::logger::report_error("hls-download", "join_remux_worker", &e);
            }
            if let Err(error) = tokio::fs::remove_file(&mp4_tmp).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
            }
            None
        }
    }
}

// ---------------------------------------------------------------------------
// 独立音轨(EXT-X-MEDIA)
// ---------------------------------------------------------------------------

/// 音轨并发上限:音频分片小而密,不需要与视频同量级的连接数。
const AUDIO_TRACK_MAX_CONCURRENCY: usize = 4;

/// 音轨续传检查点的任务级 config 键。格式同视频检查点
/// (`idx:bytes:media_sequence:<音轨标识>`)。
fn audio_resume_key(task_id: &str) -> String {
    format!("hls_audio_resume_{task_id}")
}

/// 音轨标识:去 query 的 playlist path(签名 token 会过期,不能按完整 URI 比较),
/// 续传时据此确认磁盘上的前缀属于同一条音轨。
fn audio_track_tag(playlist_url: &str) -> String {
    VariantId::of(&HlsVariant {
        bandwidth: 0,
        average_bandwidth: None,
        resolution: None,
        uri: playlist_url.to_owned(),
        audio_uri: None,
    })
    .encode()
}

/// 段写盘失败 → 任务错误。磁盘满给出明确提示,便于用户区分"磁盘满"与普通 IO 错误。
fn write_failure(e: std::io::Error) -> DownloadError {
    if e.kind() == std::io::ErrorKind::StorageFull || e.raw_os_error() == Some(28) {
        DownloadError::Other("磁盘空间不足，请清理磁盘后重试".to_string())
    } else {
        DownloadError::Io(e)
    }
}

/// 同目录其它未完成任务已登记的文件名(小写),占名换名时避开它们。
async fn sibling_avoid(p: &DownloadParams) -> std::collections::HashSet<String> {
    p.db.list_active_sibling_file_names(&p.save_dir, &p.task_id)
        .await
        .map(|names| names.into_iter().map(|n| n.to_lowercase()).collect())
        .unwrap_or_default()
}

/// 从 `first_idx` 起为每个尚未落盘的段生成下载任务。各段的绝对序号取自
/// `HlsSegment::sequence`(AES-128 无显式 IV 时用作 IV;插件过滤分片后仍保持原值)。
fn segment_jobs(segments: &[HlsSegment], first_idx: usize) -> Vec<SegmentJob> {
    segments
        .iter()
        .enumerate()
        .skip(first_idx)
        .map(|(idx, segment)| SegmentJob {
            idx,
            sequence: segment.sequence,
            uri: segment.uri.clone(),
            byte_range: segment.byte_range,
            // Extract only the encryption fields needed to decrypt this segment.
            key_info: segment.key.as_ref().and_then(|k| {
                if k.method == HlsKeyMethod::Aes128 && !k.uri.is_empty() {
                    Some((k.uri.clone(), k.iv.clone()))
                } else {
                    None
                }
            }),
        })
        .collect()
}

/// 把毫秒级精度的 f32 时长转成 f64(避免 `6.006f32 as f64` 的尾数噪声)。
fn duration_f64(d: f32) -> f64 {
    d.to_string()
        .parse::<f64>()
        .unwrap_or_else(|_| f64::from(d))
}

/// 构造交给插件的分片列表(`range` 为 `segments` 下标区间)。
///
/// 其余字段直接取自 `HlsSegment`;标题 / 绝对时间 / DATERANGE / 未识别标签需要对原文再
/// 解析一次,仅当再解析的分片数与 `segments` 一致时按位置补全,否则保持空值。只有
/// `wants(..)` 为真的阶段才应调用本函数。
fn segment_items(
    raw: &[u8],
    segments: &[HlsSegment],
    range: std::ops::Range<usize>,
) -> Vec<SegmentItem> {
    let reparsed = match m3u8_rs::parse_playlist(trim_playlist_prefix(raw)) {
        Ok((_, m3u8_rs::Playlist::MediaPlaylist(media)))
            if media.segments.len() == segments.len() =>
        {
            Some(media)
        }
        _ => None,
    };
    fn attrs(
        m: &Option<std::collections::HashMap<String, m3u8_rs::QuotedOrUnquoted>>,
    ) -> impl Iterator<Item = (String, String)> + '_ {
        m.iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.as_str().to_string()))
    }
    range
        .filter_map(|i| segments.get(i).map(|seg| (i, seg)))
        .map(|(i, seg)| {
            let mut item = SegmentItem {
                id: seg.sequence,
                uri: seg.uri.clone(),
                duration: Some(duration_f64(seg.duration)),
                byte_range: seg
                    .byte_range
                    .map(|(offset, length)| ByteRangeCtx { offset, length }),
                discontinuity: seg.discontinuity,
                key: seg
                    .key
                    .as_ref()
                    .filter(|k| k.method == HlsKeyMethod::Aes128)
                    .map(|k| KeyCtx {
                        method: "AES-128",
                        uri: k.uri.clone(),
                        iv: k.iv.clone(),
                    }),
                map: seg.map.as_ref().map(|m| MapCtx {
                    uri: m.uri.clone(),
                    byte_range: m
                        .byte_range
                        .map(|(offset, length)| ByteRangeCtx { offset, length }),
                }),
                ..SegmentItem::default()
            };
            if let Some(src) = reparsed.as_ref().and_then(|m| m.segments.get(i)) {
                item.title = src.title.clone().filter(|t| !t.is_empty());
                item.program_date_time = src.program_date_time.map(|d| d.to_rfc3339());
                item.date_range = src.daterange.as_ref().map(|d| DateRangeCtx {
                    id: d.id.clone(),
                    class: d.class.clone(),
                    start_date: d.start_date.to_rfc3339(),
                    end_date: d.end_date.map(|e| e.to_rfc3339()),
                    duration: d.duration,
                    planned_duration: d.planned_duration,
                    end_on_next: d.end_on_next,
                    attributes: attrs(&d.x_prefixed)
                        .chain(attrs(&d.other_attributes))
                        .collect(),
                });
                item.tags = src.unknown_tags.iter().map(ToString::to_string).collect();
            }
            item
        })
        .collect()
}

/// 点播轨道的插件分片过滤。返回保留的分片与"被丢弃分片序号"的指纹(没有丢弃时为空串)。
/// 未声明 `segments` 阶段时原样返回,不构建上下文。
#[allow(clippy::too_many_arguments)]
async fn filter_vod_track(
    p: &DownloadParams,
    task: &TaskCtx,
    notes: &ActivityNotes,
    track: SegmentTrack,
    playlist_url: &str,
    raw: &[u8],
    segments: Vec<HlsSegment>,
    media_sequence: u64,
    target_duration: u64,
) -> Result<(Vec<HlsSegment>, String), DownloadError> {
    if !wants(p.interceptor.as_ref(), InterceptStage::Segments) {
        return Ok((segments, String::new()));
    }
    let items = segment_items(raw, &segments, 0..segments.len());
    let (manifest_text, manifest_text_omitted) = crate::intercept::ctx::manifest_text(raw);
    let header = SegmentsHeader {
        task,
        format: "hls",
        track,
        playlist_url: playlist_url.to_string(),
        manifest_text,
        manifest_text_omitted,
        media_sequence: Some(media_sequence),
        target_duration: Some(target_duration as f64),
        period_index: None,
        representation_id: None,
    };
    let decision = run_segments(
        p.interceptor.as_ref(),
        notes,
        &p.cancel_token,
        InterceptStage::Segments,
        &header,
        &items,
    )
    .await?;
    let SegmentDecision {
        dropped,
        mut rewrites,
    } = decision;
    // 改写只换分片下载地址(key / EXT-X-MAP 地址不动);改写值每次运行都可能不同
    // (逐片签名),因此不参与续传指纹,指纹只由被丢弃的序号决定。
    let fingerprint = if dropped.is_empty() {
        String::new()
    } else {
        let joined = dropped
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(joined.as_bytes()))
    };
    let kept = segments
        .into_iter()
        .filter(|s| !dropped.contains(&s.sequence))
        .map(|mut s| {
            if let Some(uri) = rewrites.remove(&s.sequence) {
                s.uri = uri;
            }
            s
        })
        .collect();
    Ok((kept, fingerprint))
}

/// 与已持久化的过滤指纹对账:不同则写入(`fingerprint` 为空则删除)。返回
/// `is_resume && 有变化`——续传前缀与新过滤结果无法对齐,调用方应放弃续传。
async fn sync_filter_fingerprint(
    db: &crate::db::Db,
    key: &str,
    is_resume: bool,
    fingerprint: &str,
) -> Result<bool, DownloadError> {
    let stored = db.get_config(key).await?.unwrap_or_default();
    if stored == fingerprint {
        return Ok(false);
    }
    if fingerprint.is_empty() {
        db.delete_config(key).await?;
    } else {
        db.set_config(key, fingerprint).await?;
    }
    Ok(is_resume)
}

/// 顺序预取 `first_idx..` 剩余段用到的每个不同 EXT-X-MAP 初始化段字节。
///
/// 已落盘前缀对应的 init 段字节已经在磁盘上,无需重新下载。复用
/// `download_segment_with_retry`(与媒体段相同的重试/退避/Range 路径),`seg_idx`
/// 传 `usize::MAX` 仅用于失败时的日志诊断。
#[allow(clippy::too_many_arguments)]
async fn prefetch_init_segments(
    client: &Client,
    segments: &[HlsSegment],
    first_idx: usize,
    ctx: &HlsRequestCtx<'_>,
    cancel: &tokio_util::sync::CancellationToken,
    task_id: &str,
    meter: &SegmentMeter,
    db: &crate::db::Db,
    sink: &dyn EventSink,
) -> Result<HashMap<MapKey, Vec<u8>>, DownloadError> {
    let mut map_bytes: HashMap<MapKey, Vec<u8>> = HashMap::new();
    for seg in segments.iter().skip(first_idx) {
        let Some(m) = &seg.map else { continue };
        let map_key = (m.uri.clone(), m.byte_range);
        if map_bytes.contains_key(&map_key) {
            continue;
        }
        let init_data = download_segment_with_retry(
            client,
            &m.uri,
            m.byte_range,
            ctx,
            cancel,
            task_id,
            usize::MAX,
            meter,
            db,
            sink,
            // init 段不是媒体段,大小随写盘计入所属分段。
            None,
        )
        .await
        .map_err(RequestFailure::into_error)?;
        log_info!(
            "[hls-download] task {} fetched init segment {} ({} bytes)",
            task_id,
            m.uri,
            init_data.len()
        );
        map_bytes.insert(map_key, init_data);
    }
    Ok(map_bytes)
}

/// 解析完成、待下载的独立音轨。
struct AudioTrack {
    segments: Vec<HlsSegment>,
    media_sequence: u64,
    /// `Some(EXT-X-TARGETDURATION)` 表示该音轨是直播,与直播视频同步录制。
    live_target: Option<u64>,
    playlist_url: String,
    temp_path: PathBuf,
    resume_key: String,
    tag: String,
    /// 播放列表原文(直播音轨的 `liveSegments` 上下文用)。
    raw: bytes::Bytes,
    /// 点播分片过滤指纹相对上次运行是否变化(变化则放弃音轨续传)。
    filter_changed: bool,
}

/// 音轨下载所需的全部句柄(拥有所有权,供 `tokio::spawn`)。
struct AudioRun {
    track: AudioTrack,
    client: Client,
    /// 与视频共享的运行级请求身份(含凭据刷新)。
    spec: Arc<DownloadSpec>,
    /// 用户提交的清单 URL:凭据类请求头只属于它的源站。
    header_origin_url: String,
    /// 任务取消令牌的子令牌:音轨自身失败时只停音轨,由调用方决定是否升级为任务取消。
    cancel: tokio_util::sync::CancellationToken,
    task_id: String,
    db: crate::db::Db,
    sink: Arc<dyn EventSink>,
    key_cache: KeyCache,
    /// 与视频共享的传输计量(测速与限速)。
    meter: SegmentMeter,
    /// 音轨已落盘前缀(含续传前缀),调用方据此汇总任务进度与总大小估算。
    written: Arc<TrackProgress>,
    segment_limit: i32,
    /// 插件给出的连接上限(0 = 不限);独立音轨与视频轨各自受它约束。
    connection_cap: i32,
    is_resume: bool,
}

/// 下载并落盘一条独立音轨,返回音轨总字节数。
///
/// 与视频共用段下载 / 解密 / 重试 / 限速 / 乱序窗口路径;单一有序 writer 保证落盘顺序,
/// 检查点按已完整落盘的连续前缀推进,因此暂停 / 重启后可从同一字节边界续传。
async fn run_audio_track(run: AudioRun) -> Result<i64, DownloadError> {
    let AudioRun {
        track,
        client,
        spec,
        header_origin_url,
        cancel,
        task_id,
        db,
        sink,
        key_cache,
        meter,
        written,
        segment_limit,
        connection_cap,
        is_resume,
    } = run;
    let segment_count = track.segments.len();

    let saved = if is_resume {
        db.get_config(&track.resume_key).await?
    } else {
        db.delete_config(&track.resume_key).await?;
        None
    };
    let (saved_idx, saved_bytes, saved_seq) = saved
        .as_deref()
        .map(parse_resume_checkpoint)
        .unwrap_or((0, 0, None));
    let same_rendition = saved
        .as_deref()
        .and_then(parse_checkpoint_variant)
        .is_some_and(|v| v.encode() == track.tag);
    // 与视频相同:无显式 IV 的加密段其 IV 依赖 media_sequence,漂移则放弃续传。
    let uses_computed_iv = track.segments.iter().any(|s| {
        s.key
            .as_ref()
            .is_some_and(|k| k.method == HlsKeyMethod::Aes128 && k.iv.is_none())
    });
    let media_seq_changed = match saved_seq {
        Some(prev) => prev != track.media_sequence,
        None => uses_computed_iv,
    };
    output::ensure_parent(&track.temp_path).await?;
    let file_size = tokio::fs::metadata(&track.temp_path)
        .await
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    let (mut file, first_idx, mut written_bytes) = if resume_is_usable(
        saved_idx,
        saved_bytes,
        file_size,
        segment_count,
        media_seq_changed,
        !same_rendition || track.filter_changed,
    ) {
        let safe_size = saved_bytes.min(file_size);
        if safe_size < file_size {
            let truncate_file = OpenOptions::new()
                .write(true)
                .open(&track.temp_path)
                .await?;
            truncate_file.set_len(safe_size as u64).await?;
        }
        log_info!(
            "[hls-download] task {} resuming audio track from segment {} ({} bytes)",
            task_id,
            saved_idx,
            safe_size
        );
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&track.temp_path)
            .await?;
        (f, saved_idx, safe_size)
    } else {
        (File::create(&track.temp_path).await?, 0usize, 0i64)
    };
    written.resume(written_bytes, first_idx);
    if first_idx == 0 {
        db.set_config(
            &track.resume_key,
            &format_resume_checkpoint(0, 0, track.media_sequence, Some(&track.tag)),
        )
        .await?;
    }

    let remaining = segment_count.saturating_sub(first_idx);
    if remaining == 0 {
        file.flush().await?;
        return Ok(written_bytes);
    }
    let concurrency = cap_connections(
        hls_concurrency(segment_limit, remaining).min(AUDIO_TRACK_MAX_CONCURRENCY),
        connection_cap,
    );
    log_info!(
        "[hls-download] task {} downloading {} audio segment(s) with concurrency {}",
        task_id,
        remaining,
        concurrency
    );

    let seg_ctx = HlsRequestCtx {
        spec: &spec,
        header_origin_url: &header_origin_url,
    };
    let map_bytes = prefetch_init_segments(
        &client,
        &track.segments,
        first_idx,
        &seg_ctx,
        &cancel,
        &task_id,
        &meter,
        &db,
        sink.as_ref(),
    )
    .await?;

    let semaphore = Arc::new(Semaphore::new(concurrency));
    let window = Arc::new(ReorderWindow::new(first_idx, concurrency));
    let (result_tx, mut result_rx) = mpsc::channel::<SegmentOutcome>(window.max_ahead);
    let shared = Arc::new(SegmentShared {
        client: client.clone(),
        spec: Arc::clone(&spec),
        playlist_url: track.playlist_url.clone(),
        header_origin_url: header_origin_url.clone(),
        cancel: cancel.clone(),
        task_id: task_id.clone(),
        key_cache,
        meter,
        db: db.clone(),
        sink: sink.clone(),
        progress: Some(Arc::clone(&written)),
    });
    let dispatcher = tokio::spawn(dispatch_segments(
        segment_jobs(&track.segments, first_idx),
        shared,
        semaphore,
        Arc::clone(&window),
        result_tx,
    ));

    let mut pending: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut next_to_write = first_idx;
    let mut checkpointed = first_idx;
    let mut last_checkpoint = std::time::Instant::now();
    let mut fatal: Option<DownloadError> = None;
    let mut last_written_map: Option<MapKey> = if first_idx == 0 {
        None
    } else {
        track
            .segments
            .get(first_idx - 1)
            .and_then(|s| s.map.as_ref())
            .map(|m| (m.uri.clone(), m.byte_range))
    };

    'writer: while next_to_write < segment_count {
        if cancel.is_cancelled() {
            fatal = Some(DownloadError::Cancelled);
            break;
        }
        while !pending.contains_key(&next_to_write) {
            match result_rx.recv().await {
                Some((idx, Ok(data))) => {
                    pending.insert(idx, data);
                }
                Some((idx, Err(e))) => {
                    log_info!(
                        "[hls-download] task {} audio segment {} failed: {}",
                        task_id,
                        idx,
                        e
                    );
                    cancel.cancel();
                    fatal = Some(e);
                    break 'writer;
                }
                None => {
                    fatal = Some(if cancel.is_cancelled() {
                        DownloadError::Cancelled
                    } else {
                        DownloadError::Other(format!(
                            "HLS audio segment workers exited unexpectedly before segment {}",
                            next_to_write
                        ))
                    });
                    break 'writer;
                }
            }
        }

        while let Some(data) = pending.remove(&next_to_write) {
            let seg_idx = next_to_write;
            if cancel.is_cancelled() {
                fatal = Some(DownloadError::Cancelled);
                break 'writer;
            }

            let current_map_key = track.segments[seg_idx]
                .map
                .as_ref()
                .map(|m| (m.uri.clone(), m.byte_range));
            let init_chunk: Option<&[u8]> =
                if should_write_init(last_written_map.as_ref(), current_map_key.as_ref()) {
                    match current_map_key.as_ref().and_then(|k| map_bytes.get(k)) {
                        Some(init) => Some(init.as_slice()),
                        None => {
                            fatal = Some(DownloadError::Other(format!(
                                "internal error: audio init segment for {:?} was not prefetched",
                                current_map_key
                            )));
                            break 'writer;
                        }
                    }
                } else {
                    None
                };

            // 写入失败时整体回退到本次迭代之前的长度,不留半截数据污染续传。
            let start_pos = written_bytes;
            let mut chunk_total: i64 = 0;
            let mut write_result: Result<(), std::io::Error> = Ok(());
            for chunk in init_chunk
                .into_iter()
                .chain(std::iter::once(data.as_slice()))
            {
                if let Err(e) = file.write_all(chunk).await {
                    write_result = Err(e);
                    break;
                }
                chunk_total += chunk.len() as i64;
            }
            if let Err(e) = write_result {
                if let Err(trunc_err) = file.set_len(start_pos as u64).await {
                    log_info!(
                        "[hls] task {} audio segment {} rollback set_len({}) failed: {}",
                        task_id,
                        seg_idx,
                        start_pos,
                        trunc_err
                    );
                }
                cancel.cancel();
                fatal = Some(write_failure(e));
                break 'writer;
            }

            if init_chunk.is_some() {
                last_written_map = current_map_key;
            }
            written_bytes += chunk_total;
            next_to_write += 1;
            written.wrote(seg_idx, chunk_total, written_bytes);
            window.written(next_to_write, data.len());
            if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                if let Err(error) = save_checkpoint(
                    &mut file,
                    &db,
                    &track.resume_key,
                    &format_resume_checkpoint(
                        next_to_write,
                        written_bytes,
                        track.media_sequence,
                        Some(&track.tag),
                    ),
                )
                .await
                {
                    fatal = Some(error);
                    cancel.cancel();
                    break 'writer;
                }
                checkpointed = next_to_write;
                last_checkpoint = std::time::Instant::now();
            }
        }
    }

    if fatal.is_some() {
        cancel.cancel();
    }
    // 先关闭接收端再等 dispatcher:出错/取消路径上 writer 已停止 recv,
    // 卡在 `send` 的生产者需要接收端关闭才能退出。
    drop(result_rx);
    match dispatcher.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if matches!(fatal, None | Some(DownloadError::Cancelled)) {
                fatal = Some(error);
            } else {
                crate::logger::report_warning("hls-download", "join_audio_dispatcher", &error);
            }
        }
        Err(error) if error.is_cancelled() && cancel.is_cancelled() => {
            tracing::debug!("HLS audio dispatcher cancelled");
        }
        Err(error) => {
            crate::logger::report_error("hls-download", "join_audio_dispatcher", &error);
            if matches!(fatal, None | Some(DownloadError::Cancelled)) {
                fatal = Some(DownloadError::Other(format!(
                    "HLS audio dispatcher failed: {error}"
                )));
            }
        }
    }
    let checkpoint = format_resume_checkpoint(
        next_to_write,
        written_bytes,
        track.media_sequence,
        Some(&track.tag),
    );
    match fatal {
        Some(e) => {
            // 与视频相同:flush 成功才补写最新前缀的检查点。
            match file.flush().await {
                Err(error) => {
                    if matches!(e, DownloadError::Cancelled) {
                        return Err(write_failure(error));
                    }
                    crate::logger::report_warning("hls-download", "flush_failed_audio", &error);
                }
                Ok(()) => {
                    if next_to_write > checkpointed
                        && let Err(error) = db.set_config(&track.resume_key, &checkpoint).await
                    {
                        crate::logger::report_warning(
                            "hls-download",
                            "persist_audio_checkpoint",
                            &error,
                        );
                    }
                }
            }
            Err(e)
        }
        None => {
            file.flush().await?;
            if next_to_write > checkpointed {
                db.set_config(&track.resume_key, &checkpoint).await?;
            }
            Ok(written_bytes)
        }
    }
}

/// 把视频与音轨的 temp 文件用 ffmpeg 流复制封装成 mp4,占名落盘后清理两份 temp 与
/// 续传检查点,返回 `(mp4 路径, mp4 字节数)`。
///
/// 产物先写 `<name>.mp4.fdownloading`,再按 [`claim_final_name`] 的不覆盖语义占名。
/// 失败或取消时两份 temp 原样保留,由调用方回退(仅视频)或续传。
#[allow(clippy::too_many_arguments)]
async fn mux_video_audio(
    p: &DownloadParams,
    video_temp: &Path,
    audio_temp: &Path,
    actual_name: &str,
    expected_bytes: i64,
    ffmpeg: &Path,
    video_resume_key: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(PathBuf, i64), DownloadError> {
    let save_dir = Path::new(&p.save_dir);
    let stem = Path::new(actual_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let desired = format!("{stem}.mp4");
    let mux_tmp = save_dir.join(format!("{desired}{TEMP_EXT}"));
    ffmpeg_copy_to_mp4(
        video_temp,
        Some(audio_temp),
        &mux_tmp,
        expected_bytes.max(0) as u64,
        cancel,
        ffmpeg,
    )
    .await?;

    let avoid = sibling_avoid(p).await;
    let chosen = match claim_final_name(&mux_tmp, save_dir, &desired, &p.overwrite, &avoid).await {
        Ok(name) => name,
        Err(e) => {
            if let Err(error) = tokio::fs::remove_file(&mux_tmp).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
            }
            return Err(e);
        }
    };
    let mp4_path = save_dir.join(&chosen);
    let mp4_size = tokio::fs::metadata(&mp4_path)
        .await
        .ok()
        .and_then(|m| i64::try_from(m.len()).ok())
        .unwrap_or(expected_bytes);
    if let Err(e) =
        p.db.update_task_file_info(&p.task_id, &chosen, mp4_size)
            .await
    {
        // 任务记录仍指向 .ts,删除任务时不会清理这份 mp4:立即移除,temp 保留供重试。
        if let Err(error) = tokio::fs::remove_file(&mp4_path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
        }
        return Err(DownloadError::Db(e));
    }
    if let Err(error) = tokio::fs::remove_file(video_temp).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
    }
    if let Err(error) = tokio::fs::remove_file(audio_temp).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
    }
    if let Err(error) = p.db.delete_config(video_resume_key).await {
        crate::logger::report_warning("hls-download", "remove_mux_checkpoint", &error);
    }
    if let Err(error) = p.db.delete_config(&audio_resume_key(&p.task_id)).await {
        crate::logger::report_warning("hls-download", "remove_audio_checkpoint", &error);
    }
    Ok((mp4_path, mp4_size))
}

// ---------------------------------------------------------------------------
// 直播录制(播放列表无 EXT-X-ENDLIST)
// ---------------------------------------------------------------------------

/// 直播播放列表轮询间隔的上下限(秒):EXT-X-TARGETDURATION 夹在其间。
const LIVE_POLL_MIN_SECS: u64 = 1;
const LIVE_POLL_MAX_SECS: u64 = 10;
/// 一轮处理耗时已超过轮询间隔时,下次拉取前的最短等待,避免对源站连续猛拉。
const LIVE_POLL_FLOOR: Duration = Duration::from_millis(250);
/// 视频轨因 EXT-X-ENDLIST 结束后,等待音轨拉完最后分片的宽限。
const LIVE_AUDIO_GRACE: Duration = Duration::from_secs(30);

fn live_poll_interval(target_duration: u64) -> Duration {
    Duration::from_secs(target_duration.clamp(LIVE_POLL_MIN_SECS, LIVE_POLL_MAX_SECS))
}

/// 「丢弃」请求注册表(task_id → 令牌):用户**删除**任务时由 manager 置位。
///
/// 任务令牌取消只表达"停止"——暂停、取消、删除对下载器不可区分,而直播在停止时要把
/// 已录内容定稿。删除则不同:用户就是不要这些内容。manager 在取消任务令牌**之前**调用
/// [`request_discard`],直播录制据此退出后清理 temp / 检查点而不定稿;定稿阶段(ffmpeg
/// mux / remux)也以该令牌为取消源,因此删除能中断已经开始的定稿。
static DISCARD_REQUESTS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn discard_requests()
-> std::sync::MutexGuard<'static, HashMap<String, tokio_util::sync::CancellationToken>> {
    // 持锁区间只做 HashMap 增删,不会在中途 panic;即便中毒,数据仍然一致。
    DISCARD_REQUESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 声明任务将被删除,其正在进行(或刚被取消、仍在定稿)的直播录制不得保存产物。
/// 调用方须在删除流程结束后调用 [`clear_discard`] 释放登记。
pub fn request_discard(task_id: &str) {
    discard_requests()
        .entry(task_id.to_owned())
        .or_default()
        .cancel();
}

/// 释放 [`request_discard`] 的登记(幂等)。已经取得令牌的下载器不受影响。
pub fn clear_discard(task_id: &str) {
    discard_requests().remove(task_id);
}

fn discard_token(task_id: &str) -> tokio_util::sync::CancellationToken {
    discard_requests()
        .entry(task_id.to_owned())
        .or_default()
        .clone()
}

/// 直播录制结束时释放自己登记的丢弃令牌,避免正常完成的任务在注册表里残留。
struct DiscardGuard(String);

impl Drop for DiscardGuard {
    fn drop(&mut self) {
        clear_discard(&self.0);
    }
}

/// 删除任务时丢弃直播录制:移除视频 / 音轨 temp 与两个续传检查点,不 remux / mux。
async fn discard_live_recording(
    p: &DownloadParams,
    temp_path: &Path,
    audio_temp_path: Option<&Path>,
    video_resume_key: &str,
) {
    log_info!(
        "[hls-download] task {} deleted during live recording, discarding recorded content",
        p.task_id
    );
    for path in std::iter::once(temp_path).chain(audio_temp_path) {
        if let Err(error) = tokio::fs::remove_file(path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("hls-download", "remove_discarded_temp", &error);
        }
    }
    for key in [video_resume_key, audio_resume_key(&p.task_id).as_str()] {
        if let Err(error) = p.db.delete_config(key).await {
            crate::logger::report_warning("hls-download", "remove_discarded_checkpoint", &error);
        }
    }
}

/// 解析后的独立音轨播放列表(尚未分配 temp 路径)。
struct AudioPlaylist {
    uri: String,
    segments: Vec<HlsSegment>,
    media_sequence: u64,
    /// `Some(EXT-X-TARGETDURATION)` = 直播音轨。
    live_target: Option<u64>,
    /// 播放列表原文。
    raw: bytes::Bytes,
    /// 点播分片过滤指纹相对上次运行是否变化。
    filter_changed: bool,
}

/// 某一时刻拉到的媒体播放列表快照。
struct LivePlaylist {
    segments: Vec<HlsSegment>,
    media_sequence: u64,
    /// `false` = 已出现 EXT-X-ENDLIST,处理完本快照的新分片后录制结束。
    live: bool,
    target_duration: u64,
    /// 播放列表原文(`liveSegments` 上下文用)。
    raw: bytes::Bytes,
}

impl LivePlaylist {
    fn from_fetched(fetched: FetchedPlaylist) -> Result<Self, DownloadError> {
        match fetched.content {
            M3u8Content::Media {
                segments,
                media_sequence,
                live,
                target_duration,
                ..
            } => Ok(Self {
                segments,
                media_sequence,
                live,
                target_duration,
                raw: fetched.raw,
            }),
            M3u8Content::Master { .. } => Err(DownloadError::Other(
                "nested master playlist not supported".to_string(),
            )),
        }
    }
}

/// 一次轮询相对已录进度的分片取舍,见 [`plan_live_segments`]。
#[derive(Debug, PartialEq, Eq)]
struct LivePlan {
    /// 本快照中第一个需要下载的分片下标(此前的分片已录过)。
    start_idx: usize,
    /// 已滚出播放列表窗口、永久错过的分片数(序号跳跃)。
    missed: u64,
    /// 快照整体落后于已录进度(序号回退:源站重启,或 CDN 边缘返回了旧快照)。
    regressed: bool,
}

/// 以"下一个期望序号"对账快照,保证按 media sequence 只追加新分片、不重复:
/// - 首次(`next_seq == None`)取整个当前窗口;
/// - 期望序号已被窗口滚过 → 记录错过的分片数,从窗口首段继续;
/// - 快照整体早于期望序号 → 标记回退,本轮不下载任何分片。
fn plan_live_segments(next_seq: Option<u64>, media_sequence: u64, len: usize) -> LivePlan {
    let Some(next) = next_seq else {
        return LivePlan {
            start_idx: 0,
            missed: 0,
            regressed: false,
        };
    };
    let end = media_sequence.saturating_add(len as u64);
    if end < next {
        return LivePlan {
            start_idx: len,
            missed: 0,
            regressed: true,
        };
    }
    if next < media_sequence {
        return LivePlan {
            start_idx: 0,
            missed: media_sequence - next,
            regressed: false,
        };
    }
    LivePlan {
        start_idx: usize::try_from(next - media_sequence).map_or(len, |n| n.min(len)),
        missed: 0,
        regressed: false,
    }
}

/// 记录一条 warning 活动;写入失败只留日志,不影响录制。
async fn record_task_warning(
    db: &crate::db::Db,
    sink: &dyn EventSink,
    task_id: &str,
    message: impl Into<String>,
) {
    if let Err(journal_error) =
        crate::task_activity::record(db, sink, task_id, "warning", message, None).await
    {
        crate::log_error!(
            "[task-activity] failed to persist warning: {}",
            journal_error
        );
    }
}

/// 已打开的直播录制 temp 文件及其续传位置。
struct LiveTemp {
    file: File,
    /// 已完整落盘的字节数。
    bytes: i64,
    /// 已录分片数(检查点 idx 字段)。
    recorded: usize,
    /// 下一个期望的 media sequence(检查点第三字段);`None` = 从当前窗口首段开始。
    next_seq: Option<u64>,
}

struct LiveTempSpec<'a> {
    is_resume: bool,
    temp_path: &'a Path,
    resume_key: &'a str,
    /// 写入检查点的变体 / 音轨标识。
    tag: Option<&'a str>,
    /// `true`:续传要求检查点标识与 `tag` 一致(音轨);视频的变体匹配由调用方
    /// 经 `abandon` 表达(清单签名轮换时标识可能不同但仍是同一画质)。
    strict_tag: bool,
    abandon: bool,
    /// 从头录制时写入零进度检查点用的序号。
    first_sequence: u64,
}

/// 打开直播录制的 temp 文件。
///
/// 直播检查点沿用点播格式 `idx:bytes:seq[:tag]`,其中 `idx` = 已录分片数,`seq` = 下一个
/// 期望序号。续传时截断到最后一次完整落盘的字节边界并追加;期望序号仍在播放列表窗口内
/// 则无缝接上,否则按序号跳跃记 warning 并从窗口首段继续(停机期间的内容无法补录)。
async fn open_live_temp(
    db: &crate::db::Db,
    spec: LiveTempSpec<'_>,
) -> Result<LiveTemp, DownloadError> {
    output::ensure_parent(spec.temp_path).await?;
    let saved = if spec.is_resume {
        db.get_config(spec.resume_key).await?
    } else {
        db.delete_config(spec.resume_key).await?;
        None
    };
    let (saved_idx, saved_bytes, saved_seq) = saved
        .as_deref()
        .map(parse_resume_checkpoint)
        .unwrap_or((0, 0, None));
    let tag_matches = !spec.strict_tag
        || saved
            .as_deref()
            .and_then(parse_checkpoint_variant)
            .is_some_and(|v| spec.tag == Some(v.encode().as_str()));
    let file_size = tokio::fs::metadata(spec.temp_path)
        .await
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    let resume_seq = if !spec.abandon
        && tag_matches
        && saved_idx > 0
        && saved_bytes > 0
        && file_size >= saved_bytes
    {
        saved_seq
    } else {
        None
    };

    match resume_seq {
        Some(next_seq) => {
            if saved_bytes < file_size {
                let truncate_file = OpenOptions::new().write(true).open(spec.temp_path).await?;
                truncate_file.set_len(saved_bytes as u64).await?;
            }
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(spec.temp_path)
                .await?;
            Ok(LiveTemp {
                file,
                bytes: saved_bytes,
                recorded: saved_idx,
                next_seq: Some(next_seq),
            })
        }
        None => {
            let file = File::create(spec.temp_path).await?;
            if spec.tag.is_some() {
                db.set_config(
                    spec.resume_key,
                    &format_resume_checkpoint(0, 0, spec.first_sequence, spec.tag),
                )
                .await?;
            }
            Ok(LiveTemp {
                file,
                bytes: 0,
                recorded: 0,
                next_seq: None,
            })
        }
    }
}

/// 一条直播轨道(视频或独立音轨)的录制句柄(拥有所有权,供 `tokio::spawn`)。
struct LiveRun {
    /// 日志 / 活动里的轨道名("视频" / "音频")。
    label: &'static str,
    client: Client,
    /// 与本次运行其余请求共享的请求身份(含凭据刷新)。
    spec: Arc<DownloadSpec>,
    /// 用户提交的清单 URL:Cookie 同源判定与凭据类请求头只属于它的源站。
    header_origin_url: String,
    /// 本轨媒体播放列表 URL(每轮轮询重新拉取它)。
    playlist_url: String,
    /// 已解析的首个快照。
    playlist: LivePlaylist,
    /// 用户停止(暂停/取消)令牌:取消即"停止录制并保留已录内容"。
    cancel: tokio_util::sync::CancellationToken,
    task_id: String,
    db: crate::db::Db,
    sink: Arc<dyn EventSink>,
    key_cache: KeyCache,
    /// 本次录制所有轨道共享的传输计量(测速与限速)。
    meter: SegmentMeter,
    /// 本轨已落盘字节数,供进度采样器汇总。
    written: Arc<AtomicI64>,
    concurrency: usize,
    temp: LiveTemp,
    resume_key: String,
    tag: Option<String>,
    /// `Some(另一轨字节数)`:本轨按 [`DB_SAVE_INTERVAL_SECS`] 把两轨合计字节数落库(视频轨负责)。
    persist_with: Option<Arc<AtomicI64>>,
    /// 插件拦截器与 `liveSegments` 上下文所需的任务信息 / 轨道 / 活动记录。
    interceptor: Arc<dyn TaskInterceptor>,
    task: TaskCtx,
    track: SegmentTrack,
    notes: ActivityNotes,
}

/// 一条直播轨道录制结束的原因。
enum LiveEnd {
    /// 播放列表出现 EXT-X-ENDLIST(流正常结束)。
    EndList,
    /// 用户暂停 / 取消。
    Stopped,
    /// 连续拉取播放列表或分片失败,放弃继续录制(附原因)。
    GaveUp(String),
}

struct LiveOutcome {
    bytes: i64,
    end: LiveEnd,
}

/// 追加写入(可选 init 段 + 媒体段),返回写入字节数;任一步失败都回退到写入前的长度。
/// 限速已在读取 body 时生效,这里整块追加。
async fn append_live_chunks(
    file: &mut File,
    init: Option<&[u8]>,
    data: &[u8],
    start_pos: i64,
) -> Result<i64, DownloadError> {
    let mut written = 0i64;
    let mut write_result: Result<(), std::io::Error> = Ok(());
    for chunk in init.into_iter().chain(std::iter::once(data)) {
        if let Err(e) = file.write_all(chunk).await {
            write_result = Err(e);
            break;
        }
        written += chunk.len() as i64;
    }
    if let Err(e) = write_result {
        // 与点播 writer 一致:回退到写入前的长度,避免半截数据污染续传;回退失败
        // 仅记录,不掩盖原始写入错误。
        if let Err(trunc_err) = file.set_len(start_pos as u64).await {
            log_info!(
                "[hls] live rollback set_len({}) failed: {}",
                start_pos,
                trunc_err
            );
        }
        return Err(write_failure(e));
    }
    Ok(written)
}

/// 需要写入 `seg` 的 EXT-X-MAP init 段而缓存里没有时拉取并缓存(只保留最新一份)。
async fn ensure_live_init(
    shared: &SegmentShared,
    seg: &HlsSegment,
    last_map: Option<&MapKey>,
    cache: &mut Option<(MapKey, Vec<u8>)>,
) -> Result<(), RequestFailure> {
    let Some(map) = &seg.map else {
        return Ok(());
    };
    let key: MapKey = (map.uri.clone(), map.byte_range);
    if !should_write_init(last_map, Some(&key)) || cache.as_ref().is_some_and(|(k, _)| *k == key) {
        return Ok(());
    }
    let data = download_segment_with_retry(
        &shared.client,
        &map.uri,
        map.byte_range,
        &shared.ctx(),
        &shared.cancel,
        &shared.task_id,
        usize::MAX,
        &shared.meter,
        &shared.db,
        shared.sink.as_ref(),
        None,
    )
    .await?;
    *cache = Some((key, data));
    Ok(())
}

/// 重新拉取直播播放列表,按现有重试退避(`MAX_RETRIES` 次,指数延迟);用尽则放弃。
async fn refetch_live_playlist(
    shared: &SegmentShared,
    root_ctx: &HlsRequestCtx<'_>,
    label: &str,
) -> Result<LivePlaylist, LiveEnd> {
    let mut attempts = 0u32;
    loop {
        let error = match parse_m3u8(
            &shared.client,
            &shared.playlist_url,
            root_ctx,
            &shared.cancel,
        )
        .await
        {
            Ok(fetched) => match LivePlaylist::from_fetched(fetched) {
                Ok(playlist) => return Ok(playlist),
                Err(error) => error,
            },
            Err(RequestFailure::Retryable(DownloadError::Cancelled)) => {
                return Err(LiveEnd::Stopped);
            }
            Err(RequestFailure::Fatal(error)) => return Err(LiveEnd::GaveUp(error.to_string())),
            Err(RequestFailure::Retryable(error)) => error,
        };
        attempts += 1;
        if attempts >= MAX_RETRIES {
            return Err(LiveEnd::GaveUp(format!(
                "HLS {label}直播播放列表连续 {MAX_RETRIES} 次拉取失败：{error}"
            )));
        }
        log_info!(
            "[hls-download] task {} live {} playlist refresh attempt {}/{} failed: {}",
            shared.task_id,
            label,
            attempts,
            MAX_RETRIES,
            error
        );
        if let Err(journal_error) = crate::task_activity::record(
            &shared.db,
            shared.sink.as_ref(),
            &shared.task_id,
            "retry",
            format!(
                "HLS {label}直播播放列表第 {attempts}/{MAX_RETRIES} 次拉取失败，即将重试：{error}"
            ),
            None,
        )
        .await
        {
            crate::log_error!("[task-activity] failed to persist retry: {}", journal_error);
        }
        let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
        tokio::select! {
            _ = shared.cancel.cancelled() => return Err(LiveEnd::Stopped),
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// 录制一条直播轨道直到:播放列表出现 EXT-X-ENDLIST、用户停止、或连续失败放弃。
///
/// 每一轮:对账快照 → 只下载序号 ≥ 期望序号的新分片(有界并发、按序号顺序落盘)→
/// 每段落盘后 flush 并写检查点 → 睡到下一个轮询点(只在任务运行期间轮询)→ 重新拉取
/// 播放列表。序号跳跃(分片滚出窗口)与单个分片下载失败都记 warning 后跳过,连续
/// `MAX_RETRIES` 个分片失败才放弃。返回 `Err` 只代表无法继续写盘 / 落库(此时 temp 与
/// 检查点原样保留,可续传)。
async fn run_live_track(run: LiveRun) -> Result<LiveOutcome, DownloadError> {
    let LiveRun {
        label,
        client,
        spec,
        header_origin_url,
        playlist_url,
        playlist,
        cancel,
        task_id,
        db,
        sink,
        key_cache,
        meter,
        written,
        concurrency,
        temp,
        resume_key,
        tag,
        persist_with,
        interceptor,
        task,
        track,
        notes,
    } = run;
    let LiveTemp {
        mut file,
        bytes: mut written_bytes,
        mut recorded,
        mut next_seq,
    } = temp;
    written.store(written_bytes, Ordering::Relaxed);

    // 播放列表请求沿用首次解析时的上下文(以用户 URL 为源站基准)。
    let root_ctx = HlsRequestCtx {
        spec: &spec,
        header_origin_url: &header_origin_url,
    };
    let shared = Arc::new(SegmentShared {
        client,
        spec: Arc::clone(&spec),
        playlist_url,
        header_origin_url: header_origin_url.clone(),
        cancel: cancel.clone(),
        task_id: task_id.clone(),
        key_cache,
        meter,
        db: db.clone(),
        sink: Arc::clone(&sink),
        // 直播没有总量可估。
        progress: None,
    });

    let mut playlist = playlist;
    let mut init_cache: Option<(MapKey, Vec<u8>)> = None;
    // 续传后磁盘上最后一份 init 段身份未知,保守地在首段前重写一份(重复 init 对播放器无害)。
    let mut last_map: Option<MapKey> = None;
    let mut regressions = 0u32;
    let mut failures = 0u32;
    let mut last_db_save = std::time::Instant::now();

    let end = 'record: loop {
        let cycle_started = std::time::Instant::now();
        if cancel.is_cancelled() {
            break LiveEnd::Stopped;
        }

        let mut plan =
            plan_live_segments(next_seq, playlist.media_sequence, playlist.segments.len());
        if plan.regressed {
            regressions += 1;
            if regressions >= MAX_RETRIES {
                record_task_warning(
                    &db,
                    sink.as_ref(),
                    &task_id,
                    format!(
                        "HLS {label}直播播放列表序号回退（当前 {}，已录至 {}），按新序号重新开始录制",
                        playlist.media_sequence,
                        next_seq.unwrap_or_default()
                    ),
                )
                .await;
                next_seq = Some(playlist.media_sequence);
                plan =
                    plan_live_segments(next_seq, playlist.media_sequence, playlist.segments.len());
                regressions = 0;
            }
        } else {
            regressions = 0;
        }
        if plan.missed > 0 {
            let first_missed = playlist.media_sequence - plan.missed;
            record_task_warning(
                &db,
                sink.as_ref(),
                &task_id,
                format!(
                    "HLS {label}直播播放列表序号跳跃：序号 {first_missed}–{} 共 {} 个分片已滚出播放列表，已跳过",
                    playlist.media_sequence - 1,
                    plan.missed
                ),
            )
            .await;
        }

        let mut jobs = segment_jobs(&playlist.segments, plan.start_idx);
        // 插件 `liveSegments` 阶段:按本轮待录分片过滤,被丢弃的分片不下载。
        let mut dropped: BTreeSet<u64> = BTreeSet::new();
        let window_len = playlist.segments.len();
        if plan.start_idx < window_len && wants(interceptor.as_ref(), InterceptStage::LiveSegments)
        {
            let items = segment_items(
                &playlist.raw,
                &playlist.segments,
                plan.start_idx..window_len,
            );
            let (manifest_text, manifest_text_omitted) =
                crate::intercept::ctx::manifest_text(&playlist.raw);
            let header = SegmentsHeader {
                task: &task,
                format: "hls",
                track,
                playlist_url: shared.playlist_url.clone(),
                manifest_text,
                manifest_text_omitted,
                media_sequence: Some(playlist.media_sequence),
                target_duration: Some(playlist.target_duration as f64),
                period_index: None,
                representation_id: None,
            };
            let decision = match run_segments(
                interceptor.as_ref(),
                &notes,
                &cancel,
                InterceptStage::LiveSegments,
                &header,
                &items,
            )
            .await
            {
                Ok(decision) => decision,
                Err(abort) => break 'record LiveEnd::GaveUp(abort.to_string()),
            };
            dropped = decision.dropped;
            let mut rewrites = decision.rewrites;
            jobs.retain(|j| !dropped.contains(&j.sequence));
            // 改写只换分片下载地址;游标 / 检查点仍按绝对序号推进,与是否改写无关。
            for job in &mut jobs {
                if let Some(uri) = rewrites.remove(&job.sequence) {
                    job.uri = uri;
                }
            }
        }
        // `buffered` 有界并发且按输入顺序产出,writer 因此按序号顺序落盘。每个 future 自带
        // 所有权(Arc + 分片任务),不借用循环局部变量。
        let mut results = std::pin::pin!(
            futures_util::stream::iter(jobs.into_iter().map(|job| {
                let shared = Arc::clone(&shared);
                async move {
                    let result = download_and_decrypt_segment(&shared, &job).await;
                    (job.idx, job.sequence, result)
                }
            }))
            .buffered(concurrency)
        );
        while let Some((seg_idx, sequence, result)) = results.next().await {
            let seg = &playlist.segments[seg_idx];
            let current_map: Option<MapKey> =
                seg.map.as_ref().map(|m| (m.uri.clone(), m.byte_range));

            let prepared = match result {
                Ok(data) => ensure_live_init(&shared, seg, last_map.as_ref(), &mut init_cache)
                    .await
                    .map(|()| data),
                Err(error) => Err(error),
            };
            let data = match prepared {
                Ok(data) => data,
                Err(RequestFailure::Fatal(error)) => {
                    // 凭据刷新被插件判为任务失败:不是坏分片,不跳过,直接放弃录制。
                    break 'record LiveEnd::GaveUp(error.to_string());
                }
                Err(RequestFailure::Retryable(DownloadError::Cancelled)) => {
                    break 'record LiveEnd::Stopped;
                }
                Err(RequestFailure::Retryable(error)) => {
                    // 该分片(或其 init 段)重试用尽:跳过并继续,避免一个坏分片毁掉整场录制。
                    next_seq = Some(sequence + 1);
                    failures += 1;
                    record_task_warning(
                        &db,
                        sink.as_ref(),
                        &task_id,
                        format!("HLS {label}直播分片（序号 {sequence}）下载失败，已跳过：{error}"),
                    )
                    .await;
                    if failures >= MAX_RETRIES {
                        break 'record LiveEnd::GaveUp(format!(
                            "HLS {label}直播连续 {MAX_RETRIES} 个分片下载失败：{error}"
                        ));
                    }
                    continue;
                }
            };
            failures = 0;

            let init: Option<&[u8]> = if should_write_init(last_map.as_ref(), current_map.as_ref())
            {
                match &init_cache {
                    Some((key, bytes)) if Some(key) == current_map.as_ref() => {
                        Some(bytes.as_slice())
                    }
                    _ => {
                        return Err(DownloadError::Other(format!(
                            "internal error: live init segment for {current_map:?} was not fetched"
                        )));
                    }
                }
            } else {
                None
            };
            let chunk_len = append_live_chunks(&mut file, init, &data, written_bytes).await?;
            if init.is_some() {
                last_map = current_map;
            }
            written_bytes += chunk_len;
            recorded += 1;
            next_seq = Some(sequence + 1);
            written.store(written_bytes, Ordering::Relaxed);

            file.flush().await?;
            db.set_config(
                &resume_key,
                &format_resume_checkpoint(recorded, written_bytes, sequence + 1, tag.as_deref()),
            )
            .await?;
            if let Some(other) = &persist_with
                && last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS
            {
                db.update_task_progress(&task_id, written_bytes + other.load(Ordering::Relaxed))
                    .await?;
                last_db_save = std::time::Instant::now();
            }
            log_info!(
                "[hls-download] task {} live {} segment seq {} done, {} bytes total",
                task_id,
                label,
                sequence,
                written_bytes
            );
        }

        // 被插件丢弃的分片永远不会进入写入循环,游标因此不会越过它们:本轮窗口内
        // 已处理完毕(下载 / 丢弃 / 跳过),下一轮不得重新计划。
        if let (false, Some(last)) = (dropped.is_empty(), playlist.segments.last()) {
            let advanced = last.sequence.saturating_add(1);
            if next_seq.is_none_or(|n| n < advanced) {
                next_seq = Some(advanced);
                db.set_config(
                    &resume_key,
                    &format_resume_checkpoint(recorded, written_bytes, advanced, tag.as_deref()),
                )
                .await?;
            }
        }

        if !playlist.live {
            break LiveEnd::EndList;
        }
        let wait = live_poll_interval(playlist.target_duration)
            .saturating_sub(cycle_started.elapsed())
            .max(LIVE_POLL_FLOOR);
        tokio::select! {
            _ = cancel.cancelled() => break LiveEnd::Stopped,
            _ = tokio::time::sleep(wait) => {}
        }
        match refetch_live_playlist(&shared, &root_ctx, label).await {
            Ok(next) => playlist = next,
            Err(end) => break end,
        }
    };

    file.flush().await.map_err(write_failure)?;
    log_info!(
        "[hls-download] task {} live {} recording ended after {} segment(s), {} bytes",
        task_id,
        label,
        recorded,
        written_bytes
    );
    Ok(LiveOutcome {
        bytes: written_bytes,
        end,
    })
}

/// 进度采样器读取的进度来源。
enum ProgressSource {
    /// 点播:两轨已落盘前缀 + 总大小估算。
    Vod(Arc<TaskProgress>),
    /// 直播:两轨已落盘字节;总大小未知(恒 0,与点播首段前同一「未知」语义)。
    Live {
        video: Arc<AtomicI64>,
        audio: Arc<AtomicI64>,
    },
}

impl ProgressSource {
    /// `(已落盘字节, 总字节)`;总字节 0 = 未知。
    fn sample(&self) -> (i64, i64) {
        match self {
            Self::Vod(progress) => progress.snapshot(),
            Self::Live { video, audio } => (
                video.load(Ordering::Relaxed) + audio.load(Ordering::Relaxed),
                0,
            ),
        }
    }
}

/// 视频(及可选独立音轨)进度采样器:每 200ms 上报一次两轨已落盘字节(进度)、总大小
/// 与本次运行的接收字节(测速)。
fn spawn_progress_sampler(
    tx: mpsc::Sender<ProgressUpdate>,
    task_id: String,
    meter: SegmentMeter,
    source: ProgressSource,
    limit: u32,
) -> AbortOnDrop {
    AbortOnDrop(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(200));
        loop {
            ticker.tick().await;
            let (written, total) = source.sample();
            if tx
                .send(hls_progress(&task_id, &meter, written, total, limit))
                .await
                .is_err()
            {
                break;
            }
        }
    }))
}

/// 直播录制所需的全部事实(由 `run_hls_download_inner` 在创建 temp 文件前收集)。
struct LiveRecording {
    segments: Vec<HlsSegment>,
    media_sequence: u64,
    target_duration: u64,
    playlist_url: String,
    is_fmp4: bool,
    playlist_raw: bytes::Bytes,
    variant_tag: Option<String>,
    abandon_resume: bool,
    audio: Option<AudioTrack>,
    ffmpeg: Option<PathBuf>,
    temp_path: PathBuf,
    dest_path: PathBuf,
    actual_name: String,
    save_dir: PathBuf,
    resume_key: String,
    /// 本次运行的共享请求身份(解析阶段已在用,录制阶段沿用同一份)。
    spec: Arc<DownloadSpec>,
}

/// 把停止前最后已知的两轨合计字节数落库(失败只告警,原始结果优先)。
async fn persist_live_progress(p: &DownloadParams, bytes: i64) {
    if let Err(error) = p.db.update_task_progress(&p.task_id, bytes).await {
        crate::logger::report_warning("hls-download", "persist_failed_progress", &error);
    }
}

/// 直播录制:视频(与可选独立音轨并行)持续录制,结束后与点播共用 [`finalize_recording`]
/// 定稿为完成文件。
///
/// 停止语义:播放列表出现 ENDLIST、用户暂停 / 取消(任务令牌取消)、播放列表连续拉取
/// 失败,三者都以**已录内容定稿并报告完成**,而不是丢弃——直播没有"稍后从断点继续"的
/// 意义(断点之后的内容已经过去了)。只有一个字节都没录到时才按取消 / 失败上报。
/// 写盘 / 落库失败仍是任务错误,temp 与检查点保留,恢复时从检查点追加。
async fn run_live_recording(p: &DownloadParams, rec: LiveRecording) -> Result<i64, DownloadError> {
    let LiveRecording {
        segments,
        media_sequence,
        target_duration,
        playlist_url,
        is_fmp4,
        playlist_raw,
        variant_tag,
        abandon_resume,
        audio,
        ffmpeg,
        temp_path,
        dest_path,
        actual_name,
        save_dir,
        resume_key,
        spec,
    } = rec;
    log_info!(
        "[hls-download] task {} recording live stream, media_sequence={}, {} segment(s) in window",
        p.task_id,
        media_sequence,
        segments.len()
    );
    let discard = discard_token(&p.task_id);
    let _discard_guard = DiscardGuard(p.task_id.clone());

    let concurrency = cap_connections(
        hls_concurrency(p.segment_count, MAX_HLS_CONCURRENCY),
        p.connection_cap,
    );
    let task_ctx = crate::intercept::ctx::task_ctx(p, ProtocolKind::Hls);
    let notes = ActivityNotes::of(p);
    let video_temp = open_live_temp(
        &p.db,
        LiveTempSpec {
            is_resume: p.is_resume,
            temp_path: &temp_path,
            resume_key: &resume_key,
            tag: variant_tag.as_deref(),
            strict_tag: false,
            abandon: abandon_resume,
            first_sequence: media_sequence,
        },
    )
    .await?;
    let audio_parts = match audio {
        Some(track) => {
            let temp = open_live_temp(
                &p.db,
                LiveTempSpec {
                    is_resume: p.is_resume,
                    temp_path: &track.temp_path,
                    resume_key: &track.resume_key,
                    tag: Some(&track.tag),
                    strict_tag: true,
                    abandon: false,
                    first_sequence: track.media_sequence,
                },
            )
            .await?;
            Some((track, temp))
        }
        None => None,
    };
    let audio_temp_path = audio_parts.as_ref().map(|(t, _)| t.temp_path.clone());

    let key_cache: KeyCache = Arc::new(Mutex::new(HashMap::new()));
    let meter = SegmentMeter::new(p.speed_limiter.clone());
    let video_written = Arc::new(AtomicI64::new(video_temp.bytes));
    let audio_written = Arc::new(AtomicI64::new(0));
    let mut sampler = spawn_progress_sampler(
        p.progress_tx.clone(),
        p.task_id.clone(),
        meter.clone(),
        ProgressSource::Live {
            video: Arc::clone(&video_written),
            audio: Arc::clone(&audio_written),
        },
        concurrency as u32,
    );

    // 音轨令牌是任务令牌的子令牌:用户停止时随之取消;视频因 ENDLIST/放弃而先结束时
    // 由本函数单独取消它。音轨的真实错误升级为任务级取消(视频随之以"停止"收尾,
    // 最终以音轨错误为准),与点播一致。
    let audio_cancel = p.cancel_token.child_token();
    let audio_handle = audio_parts.map(|(track, temp)| {
        let run = LiveRun {
            label: "音频",
            client: p.client.clone(),
            spec: Arc::clone(&spec),
            header_origin_url: p.url.clone(),
            playlist_url: track.playlist_url.clone(),
            playlist: LivePlaylist {
                segments: track.segments,
                media_sequence: track.media_sequence,
                live: true,
                target_duration: track.live_target.unwrap_or(target_duration),
                raw: track.raw,
            },
            cancel: audio_cancel.clone(),
            task_id: p.task_id.clone(),
            db: p.db.clone(),
            sink: p.sink.clone(),
            key_cache: key_cache.clone(),
            meter: meter.clone(),
            written: Arc::clone(&audio_written),
            concurrency: concurrency.min(AUDIO_TRACK_MAX_CONCURRENCY),
            temp,
            resume_key: track.resume_key,
            tag: Some(track.tag),
            persist_with: None,
            interceptor: Arc::clone(&p.interceptor),
            task: task_ctx.clone(),
            track: SegmentTrack::Audio,
            notes: notes.clone(),
        };
        let parent = p.cancel_token.clone();
        tokio::spawn(async move {
            let result = run_live_track(run).await;
            if result.is_err() {
                parent.cancel();
            }
            result
        })
    });

    let video = run_live_track(LiveRun {
        label: "视频",
        client: p.client.clone(),
        spec,
        header_origin_url: p.url.clone(),
        playlist_url,
        playlist: LivePlaylist {
            segments,
            media_sequence,
            live: true,
            target_duration,
            raw: playlist_raw,
        },
        cancel: p.cancel_token.clone(),
        task_id: p.task_id.clone(),
        db: p.db.clone(),
        sink: p.sink.clone(),
        key_cache,
        meter,
        written: Arc::clone(&video_written),
        concurrency,
        temp: video_temp,
        resume_key: resume_key.clone(),
        tag: variant_tag,
        persist_with: Some(Arc::clone(&audio_written)),
        interceptor: Arc::clone(&p.interceptor),
        task: task_ctx,
        track: SegmentTrack::Video,
        notes,
    })
    .await;

    // 视频因 ENDLIST 结束时音轨通常只差最后一两个分片:给它一个宽限追平;其余情况
    // (用户停止 / 放弃 / 写盘失败)立即停音轨。
    let video_ended_normally = matches!(&video, Ok(o) if matches!(o.end, LiveEnd::EndList));
    if !video_ended_normally {
        audio_cancel.cancel();
    }
    let audio_result = match audio_handle {
        Some(mut handle) => {
            let joined = if video_ended_normally {
                match tokio::time::timeout(LIVE_AUDIO_GRACE, &mut handle).await {
                    Ok(joined) => joined,
                    Err(_) => {
                        audio_cancel.cancel();
                        (&mut handle).await
                    }
                }
            } else {
                (&mut handle).await
            };
            Some(match joined {
                Ok(result) => result,
                Err(error) => {
                    crate::logger::report_error("hls-download", "join_live_audio_worker", &error);
                    Err(DownloadError::Other(format!(
                        "HLS audio track task failed: {error}"
                    )))
                }
            })
        }
        None => None,
    };
    sampler.abort();
    if let Err(error) = (&mut sampler.0).await
        && !error.is_cancelled()
    {
        crate::logger::report_error("hls-download", "join_progress_sampler", &error);
    }
    // 用户删除:录制循环已随任务令牌退出,这里不再定稿,直接清理。
    if discard.is_cancelled() {
        discard_live_recording(p, &temp_path, audio_temp_path.as_deref(), &resume_key).await;
        return Err(DownloadError::Cancelled);
    }

    let total_so_far =
        video_written.load(Ordering::Relaxed) + audio_written.load(Ordering::Relaxed);
    let video = match video {
        Ok(outcome) => outcome,
        Err(error) => {
            persist_live_progress(p, total_so_far).await;
            return Err(error);
        }
    };
    let audio_outcome = match audio_result {
        Some(Ok(outcome)) => Some(outcome),
        Some(Err(error)) => {
            persist_live_progress(p, total_so_far).await;
            return Err(error);
        }
        None => None,
    };

    if video.bytes == 0 {
        persist_live_progress(p, total_so_far).await;
        return Err(match video.end {
            LiveEnd::Stopped => DownloadError::Cancelled,
            LiveEnd::GaveUp(message) => DownloadError::Other(message),
            LiveEnd::EndList => {
                DownloadError::Other("HLS 直播流结束时没有录到任何分片".to_string())
            }
        });
    }

    match &video.end {
        LiveEnd::GaveUp(message) => {
            record_task_warning(
                &p.db,
                p.sink.as_ref(),
                &p.task_id,
                format!("{message}；已按已录内容完成"),
            )
            .await;
        }
        LiveEnd::Stopped => log_info!(
            "[hls-download] task {} live recording stopped by user, finalizing {} bytes",
            p.task_id,
            video.bytes
        ),
        LiveEnd::EndList => {}
    }

    let (audio_bytes, audio_temp_path) = match (audio_outcome, audio_temp_path) {
        (Some(audio), Some(audio_temp)) => {
            if let LiveEnd::GaveUp(message) = &audio.end {
                record_task_warning(
                    &p.db,
                    p.sink.as_ref(),
                    &p.task_id,
                    format!("{message}；音频轨按已录内容合并"),
                )
                .await;
            }
            if audio.bytes > 0 {
                (audio.bytes, Some(audio_temp))
            } else {
                // 音轨一个分片都没录到:丢弃空音轨,只出视频并留下 warning 活动。
                record_task_warning(
                    &p.db,
                    p.sink.as_ref(),
                    &p.task_id,
                    "未能录到任何音频分片，输出仅含视频，输出文件可能没有声音",
                )
                .await;
                if let Err(error) = tokio::fs::remove_file(&audio_temp).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("hls-download", "remove_temporary_file", &error);
                }
                if let Err(error) = p.db.delete_config(&audio_resume_key(&p.task_id)).await {
                    crate::logger::report_warning(
                        "hls-download",
                        "remove_audio_checkpoint",
                        &error,
                    );
                }
                (0, None)
            }
        }
        _ => (0, None),
    };

    // 定稿期间的 ffmpeg mux / remux 不能沿用任务令牌:暂停 / 取消(= 停止并保存)时它已
    // 取消,会让已录内容无法定稿。改用丢弃令牌——只有删除才会中断定稿。
    let finalize_cancel = discard;
    finalize_recording(
        p,
        FinalizeInput {
            temp_path,
            dest_path,
            save_dir,
            audio_temp_path,
            ffmpeg,
            actual_name,
            resume_key,
            is_fmp4,
            downloaded_bytes: video.bytes,
            audio_bytes,
        },
        &finalize_cancel,
    )
    .await
}

// ---------------------------------------------------------------------------
// Per-segment download + decrypt (concurrency unit)
// ---------------------------------------------------------------------------

/// (seg_idx, 下载+解密结果)。连接许可在下载结束时已归还,数据留在乱序窗口里直到写盘。
type SegmentOutcome = (usize, Result<Vec<u8>, DownloadError>);

struct SegmentJob {
    idx: usize,
    /// 绝对 media sequence:无显式 IV 的 AES-128 段据此推导 IV(直播里下标与序号不再等差)。
    sequence: u64,
    uri: String,
    byte_range: Option<(u64, u64)>,
    /// `Some((key_uri, iv))` 仅用于带非空密钥 URI 的 AES-128 段。
    key_info: Option<(String, Option<String>)>,
}

/// 所有段任务共享的只读上下文与句柄。
struct SegmentShared {
    client: Client,
    spec: Arc<DownloadSpec>,
    playlist_url: String,
    header_origin_url: String,
    cancel: tokio_util::sync::CancellationToken,
    task_id: String,
    key_cache: KeyCache,
    meter: SegmentMeter,
    db: crate::db::Db,
    sink: Arc<dyn EventSink>,
    /// 点播轨的大小证据:段任务上报在途响应头与下载完成的字节;直播为 `None`。
    progress: Option<Arc<TrackProgress>>,
}

impl SegmentShared {
    fn ctx(&self) -> HlsRequestCtx<'_> {
        HlsRequestCtx {
            spec: &self.spec,
            header_origin_url: &self.header_origin_url,
        }
    }
}

/// 乱序窗口的共享状态:writer 推进,dispatcher 据此放行新段。
#[derive(Clone, Copy, Debug)]
struct WindowState {
    /// 下一个待写盘的段序号。
    next_to_write: usize,
    /// 已下载完成、尚未写盘的字节数。
    buffered: u64,
}

/// 网络并发与按序写盘之间的乱序窗口。
///
/// 段下载完即归还连接许可,数据在这里等待按序写盘;派发只受窗口约束:领先写盘
/// 位置不超过 `max_ahead` 段,且缓冲字节未达按本机内存分档的
/// [`crate::mem_budget::MemoryBudget::hls_reorder_bytes`](在途分段不计入,内存峰值约为
/// 它加上并发数 × 单段大小)。队首段永远
/// 放行:派发严格按序,队首段一定已经派发,预算再满也不会卡住 writer。
struct ReorderWindow {
    state: watch::Sender<WindowState>,
    /// 派发位置最多领先写盘位置的段数(也是结果通道容量)。
    max_ahead: usize,
    budget: u64,
}

impl ReorderWindow {
    fn new(first_idx: usize, concurrency: usize) -> Self {
        Self {
            state: watch::Sender::new(WindowState {
                next_to_write: first_idx,
                buffered: 0,
            }),
            max_ahead: concurrency
                .max(1)
                .saturating_mul(REORDER_AHEAD_PER_CONNECTION),
            budget: crate::mem_budget::current().hls_reorder_bytes,
        }
    }

    fn admits(&self, state: &WindowState, idx: usize) -> bool {
        idx <= state.next_to_write
            || (idx - state.next_to_write < self.max_ahead && state.buffered < self.budget)
    }

    /// 等到 `idx` 可以派发;窗口已关闭时返回 `false`(发送端随窗口存活,实际不会发生)。
    async fn admit(&self, idx: usize) -> bool {
        let mut changes = self.state.subscribe();
        changes
            .wait_for(|state| self.admits(state, idx))
            .await
            .is_ok()
    }

    /// 一段下载完成、进入缓冲。只会收紧窗口,不必唤醒等待者。
    fn buffered(&self, bytes: usize) {
        self.state.send_if_modified(|state| {
            state.buffered = state.buffered.saturating_add(bytes as u64);
            false
        });
    }

    /// writer 写完一段:推进写盘位置并释放这段缓冲。
    fn written(&self, next_to_write: usize, bytes: usize) {
        self.state.send_modify(|state| {
            state.next_to_write = next_to_write;
            state.buffered = state.buffered.saturating_sub(bytes as u64);
        });
    }
}

/// 按 seg_idx 顺序派发段任务:先等乱序窗口放行,再取连接许可。
///
/// 必须由单一任务顺序派发:若各段任务自行抢许可,首次 poll 顺序不保证等于 idx
/// 顺序,许可可能全被后续段占住而队首段拿不到,writer 永远等不到它。等窗口时不占
/// 连接,同时回收结束的段任务:崩溃的段任务送不回结果,writer 与窗口都会一直等它,
/// 只能在这里发现并取消整轮下载。
async fn dispatch_segments(
    jobs: Vec<SegmentJob>,
    shared: Arc<SegmentShared>,
    semaphore: Arc<Semaphore>,
    window: Arc<ReorderWindow>,
    tx: mpsc::Sender<SegmentOutcome>,
) -> Result<(), DownloadError> {
    let mut failure = None;
    let mut children = tokio::task::JoinSet::new();
    'dispatch: for job in jobs {
        loop {
            tokio::select! {
                _ = shared.cancel.cancelled() => break 'dispatch,
                admitted = window.admit(job.idx) => {
                    if admitted {
                        break;
                    }
                    break 'dispatch;
                }
                Some(joined) = children.join_next() => {
                    if let Some(error) = reap_segment_worker(joined, &shared.cancel) {
                        failure.get_or_insert(error);
                        break 'dispatch;
                    }
                }
            }
        }
        let permit = tokio::select! {
            _ = shared.cancel.cancelled() => break,
            acquired = Arc::clone(&semaphore).acquire_owned() => match acquired {
                Ok(permit) => permit,
                // Semaphore closed — runtime shutting down; nothing to send.
                Err(_) => break,
            },
        };
        // 及时回收已结束的子任务,避免长播放列表下 JoinSet 累积。
        while let Some(joined) = children.try_join_next() {
            if let Some(error) = reap_segment_worker(joined, &shared.cancel) {
                failure.get_or_insert(error);
            }
        }
        if failure.is_some() {
            break;
        }
        let child_shared = Arc::clone(&shared);
        let child_window = Arc::clone(&window);
        let child_tx = tx.clone();
        children.spawn(async move {
            let outcome = if child_shared.cancel.is_cancelled() {
                Err(DownloadError::Cancelled)
            } else {
                download_and_decrypt_segment(&child_shared, &job)
                    .await
                    .map_err(RequestFailure::into_error)
            };
            // 下载(含解密)结束即归还连接;数据进入乱序缓冲,写盘后才出窗口。
            drop(permit);
            if let Ok(data) = &outcome {
                child_window.buffered(data.len());
                if let Some(track) = &child_shared.progress {
                    track.downloaded(job.idx, data.len());
                }
            }
            // Always emit a result for this index so the in-order writer never
            // blocks forever waiting on a task that failed.
            if child_tx.send((job.idx, outcome)).await.is_err() {
                // writer 出错/取消后关闭通道,让生产者退出。
                tracing::debug!("HLS segment writer closed");
            }
        });
    }
    drop(tx);
    while let Some(joined) = children.join_next().await {
        if let Some(error) = reap_segment_worker(joined, &shared.cancel) {
            failure.get_or_insert(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// 回收一个结束的段任务:崩溃(不是取消引起的中止)时上报、取消整轮下载并返回失败。
fn reap_segment_worker(
    joined: Result<(), tokio::task::JoinError>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Option<DownloadError> {
    let error = joined.err()?;
    if error.is_cancelled() && cancel.is_cancelled() {
        tracing::debug!("HLS segment worker cancelled");
        return None;
    }
    crate::logger::report_error("hls-download", "join_segment_worker", &error);
    cancel.cancel();
    Some(DownloadError::Other(format!(
        "HLS segment worker failed: {error}"
    )))
}

/// 把非 HLS 播放列表错误转为真实失败:已确认是 HLS 后不允许退回普通 HTTP 下载。
fn reject_not_hls(e: DownloadError) -> DownloadError {
    match e {
        DownloadError::NotAnHlsPlaylist(msg) => {
            DownloadError::Other(format!("invalid HLS playlist: {msg}"))
        }
        other => other,
    }
}

/// Download a single segment (with retry) and, if encrypted, decrypt it.
///
/// This is the unit of work each concurrent task runs. It is purely
/// download + decrypt: it performs no disk writes, progress reporting, or
/// checkpointing — those stay on the single ordered writer so byte counts and
/// on-disk order remain correct under concurrency.
///
/// The IV is computed from this segment's own `idx` (`compute_default_iv`)
/// when not explicitly provided, so AES-128-CBC stays correct regardless of
/// the order tasks complete in.
async fn download_and_decrypt_segment(
    shared: &SegmentShared,
    job: &SegmentJob,
) -> Result<Vec<u8>, RequestFailure> {
    let ctx = shared.ctx();
    let seg_data = download_segment_with_retry(
        &shared.client,
        &job.uri,
        job.byte_range,
        &ctx,
        &shared.cancel,
        &shared.task_id,
        job.idx,
        &shared.meter,
        &shared.db,
        shared.sink.as_ref(),
        shared.progress.as_deref(),
    )
    .await?;

    let Some((key_uri, iv_str)) = job.key_info.as_ref() else {
        return Ok(seg_data);
    };

    // Fetch key (shared single-flight cache across all concurrent tasks).
    let key_bytes = fetch_key(
        &shared.client,
        key_uri,
        &ctx,
        &shared.key_cache,
        &shared.cancel,
    )
    .await?;

    // Determine IV — explicit IV from the playlist, else derived from this
    // segment's own index so concurrency never changes the IV.
    let iv = match iv_str {
        Some(iv_hex) => parse_iv_hex(iv_hex)?,
        None => compute_default_iv(job.sequence, 0),
    };

    let mut data_buf = seg_data;
    Ok(decrypt_segment(&mut data_buf, &key_bytes, &iv, job.idx)?)
}

// ---------------------------------------------------------------------------
// Per-segment download with retry
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn download_segment_with_retry(
    client: &Client,
    url: &str,
    byte_range: Option<(u64, u64)>,
    ctx: &HlsRequestCtx<'_>,
    cancel_token: &tokio_util::sync::CancellationToken,
    task_id: &str,
    seg_idx: usize,
    meter: &SegmentMeter,
    db: &crate::db::Db,
    sink: &dyn EventSink,
    progress: Option<&TrackProgress>,
) -> Result<Vec<u8>, RequestFailure> {
    let transport = SegmentTransport {
        client,
        ctx,
        cancel_token,
        meter,
        progress,
        header_timeout: SEGMENT_HEADER_TIMEOUT,
        idle_timeout: SEGMENT_IDLE_TIMEOUT,
    };
    let mut attempts = 0u32;

    loop {
        match download_segment_once(&transport, url, byte_range, seg_idx).await {
            Ok(data) => return Ok(data),
            Err(fatal @ RequestFailure::Fatal(_)) => return Err(fatal),
            Err(RequestFailure::Retryable(DownloadError::Cancelled)) => {
                return Err(DownloadError::Cancelled.into());
            }
            Err(RequestFailure::Retryable(e)) => {
                attempts += 1;
                if attempts >= MAX_RETRIES {
                    return Err(DownloadError::Other(format!(
                        "HLS segment {} failed after {} retries: {}",
                        seg_idx, MAX_RETRIES, e
                    ))
                    .into());
                }
                log_info!(
                    "[hls-download] task {} segment {} attempt {}/{} failed: {}",
                    task_id,
                    seg_idx,
                    attempts,
                    MAX_RETRIES,
                    e
                );
                if let Err(journal_error) = crate::task_activity::record(
                    db,
                    sink,
                    task_id,
                    "retry",
                    format!(
                        "HLS 段 {seg_idx} 第 {attempts}/{MAX_RETRIES} 次尝试失败，即将重试：{e}"
                    ),
                    None,
                )
                .await
                {
                    crate::log_error!("[task-activity] failed to persist retry: {}", journal_error);
                }
                let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                tokio::select! {
                    _ = cancel_token.cancelled() => return Err(DownloadError::Cancelled.into()),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

struct SegmentTransport<'a> {
    client: &'a Client,
    ctx: &'a HlsRequestCtx<'a>,
    cancel_token: &'a tokio_util::sync::CancellationToken,
    /// 活跃读取计数、接收字节与限速。
    meter: &'a SegmentMeter,
    /// 点播轨的大小证据:响应头到达即上报本段声明的 body 长度。
    progress: Option<&'a TrackProgress>,
    /// 等待响应头的上限;超时按 stalled 报错,复用段重试与自动重试白名单。
    header_timeout: Duration,
    /// body 逐 chunk 空闲上限(不是整段超时:单段可达 256MB)。
    idle_timeout: Duration,
}

/// 发一次段请求;被拒(401 / 403 / 410)且插件换发了凭据时,用新身份重发一次
/// (每个请求至多一次)。未声明 `refresh` 时与直接发送完全一致。
async fn download_segment_once(
    transport: &SegmentTransport<'_>,
    url: &str,
    byte_range: Option<(u64, u64)>,
    seg_idx: usize,
) -> Result<Vec<u8>, RequestFailure> {
    let used = transport.ctx.spec.current();
    let first = download_segment_as(transport, &used, url, byte_range, seg_idx).await;
    match refreshed_identity(transport.ctx.spec, &used, &first).await? {
        Some(fresh) => Ok(confirm_resend(
            transport.ctx.spec,
            &fresh,
            download_segment_as(transport, &fresh, url, byte_range, seg_idx).await,
        )?),
        None => Ok(first?),
    }
}

/// 以请求身份 `identity` 下载一个分片(无刷新逻辑)。
async fn download_segment_as(
    transport: &SegmentTransport<'_>,
    identity: &RequestSpec,
    url: &str,
    byte_range: Option<(u64, u64)>,
    seg_idx: usize,
) -> Result<Vec<u8>, DownloadError> {
    let mut req = transport.ctx.get(transport.client, url, identity);

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
        _ = transport.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
        r = tokio::time::timeout(transport.header_timeout, req.send()) => match r {
            Ok(r) => r?.error_for_status()?,
            Err(_) => {
                return Err(DownloadError::Other(format!(
                    "HLS segment {} stalled: no response headers within {:?}",
                    seg_idx, transport.header_timeout
                )));
            }
        },
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
    if let (Some(track), Some(length)) = (transport.progress, declared_len) {
        track.announce(seg_idx, length);
    }
    let raw_stream = resp.bytes_stream();
    let mut stream = crate::downloader::maybe_decompress_stream(raw_stream, encoding);

    /// Maximum allowed size for a single HLS segment (256 MB).
    /// Prevents OOM if a malicious or misconfigured server sends an oversized segment.
    const MAX_SEGMENT_BYTES: usize = 256 * 1024 * 1024;

    let mut buf = Vec::new();
    loop {
        let chunk = {
            let _transfer = transport.meter.tracker.start(seg_idx as i32);
            tokio::select! {
                _ = transport.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
                c = tokio::time::timeout(transport.idle_timeout, stream.next()) => match c {
                    Ok(c) => c,
                    Err(_) => {
                        return Err(DownloadError::Other(format!(
                            "HLS segment {} stalled: no data for {:?}",
                            seg_idx, transport.idle_timeout
                        )));
                    }
                },
            }
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
        transport
            .meter
            .admit_body(chunk_data.len(), transport.cancel_token)
            .await?;
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
        DEFAULT_HLS_CONCURRENCY, DownloadError, HlsKeyMethod, M3u8Content, MAX_HLS_CONCURRENCY,
        compute_default_iv, decrypt_segment, hls_concurrency, is_hls_url, parse_iv_hex,
        parse_m3u8_bytes, parse_resume_checkpoint, remux_space_ok, resolve_uri, should_write_init,
    };
    use aes::Aes128;
    use cbc::cipher::block_padding::{NoPadding, Pkcs7};
    use cbc::cipher::{BlockEncryptMut, KeyIvInit};

    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    #[tokio::test]
    async fn body_activity_excludes_response_setup_and_ends_at_eof()
    -> Result<(), Box<dyn std::error::Error>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await?;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n")
                .await?;
            release_rx
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            socket.write_all(b"hello").await?;
            Ok::<(), std::io::Error>(())
        });
        let meter = super::SegmentMeter::new(crate::speed_limiter::SpeedLimiter::new(0));
        let tracker = meter.tracker.clone();
        let client = reqwest::Client::builder().no_proxy().build()?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let url = format!("http://{address}/segment.ts");
        let download = tokio::spawn({
            let meter = meter.clone();
            async move {
                let spec = crate::downloader::DownloadSpec::new(
                    crate::downloader::RequestSpec::empty_get(),
                );
                let ctx = super::HlsRequestCtx {
                    spec: &spec,
                    header_origin_url: &url,
                };
                let transport = super::SegmentTransport {
                    client: &client,
                    ctx: &ctx,
                    cancel_token: &cancel,
                    meter: &meter,
                    progress: None,
                    header_timeout: std::time::Duration::from_secs(30),
                    idle_timeout: std::time::Duration::from_secs(30),
                };
                super::download_segment_once(&transport, &url, None, 2)
                    .await
                    .map_err(super::RequestFailure::into_error)
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while tracker.active() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(tracker.is_active(2));
        assert!(release_tx.send(()).is_ok());
        assert_eq!(download.await??, b"hello");
        server.await??;
        assert_eq!(tracker.active(), 0);
        Ok(())
    }

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
    fn test_parse_m3u8_bytes_rejects_non_playlist_content() {
        // 拿 .m3u8 扩展名装普通文件列表的场景（WebDAV/网盘把曲目清单恰好命
        // 名为 .m3u8，无 #EXTM3U 头）：分类必须是 NotAnHlsPlaylist 而非泛化
        // 的 Other，这样调用方（`run_hls_download`）才能把它与"内容合法但
        // 下游步骤失败"的真实 HLS 错误区分开，退回普通 HTTP 下载器而不是
        // 上报终态失败。
        let content = "01. \u{9727}\u{6708}\u{306f}\u{308b}\u{304b} - break time.flac\r\n\
                        02. \u{9727}\u{6708}\u{306f}\u{308b}\u{304b} - vocal off.flac\r\n"
            .as_bytes();
        let err = match parse_m3u8_bytes("https://example.com/list.m3u8", content) {
            Ok(_) => panic!("non-playlist content must not parse as a valid M3U8 playlist"),
            Err(e) => e,
        };
        assert!(
            matches!(err, DownloadError::NotAnHlsPlaylist(_)),
            "expected NotAnHlsPlaylist, got {err:?}"
        );
    }

    #[test]
    fn test_parse_m3u8_bytes_accepts_valid_playlist() {
        // 对照组：合法播放列表必须继续正常解析，不能被误分类为
        // NotAnHlsPlaylist。
        let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n\
                         #EXTINF:6.000,\nseg0.ts\n#EXT-X-ENDLIST\n";
        assert!(parse_m3u8_bytes("https://example.com/list.m3u8", playlist.as_bytes()).is_ok());
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

    // --- #682: EXT-X-MAP (fMP4/CMAF init segment) support ---

    #[test]
    fn test_parse_m3u8_bytes_without_map_is_ts() {
        // Baseline regression: plain MPEG-TS media playlists (no EXT-X-MAP)
        // must keep parsing with `has_map=false` and no `map` on any segment.
        let base_url = "https://cdn.example.com/live/media.m3u8";
        let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n\
#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:6.000,\nseg0.ts\n#EXT-X-ENDLIST\n";
        let content = match parse_m3u8_bytes(base_url, playlist.as_bytes()) {
            Ok(c) => c,
            Err(e) => panic!("plain TS playlist must parse, got error: {e}"),
        };
        match content {
            M3u8Content::Media {
                segments, has_map, ..
            } => {
                assert!(!has_map, "no EXT-X-MAP present -> has_map must be false");
                assert!(segments[0].map.is_none());
            }
            M3u8Content::Master { .. } => panic!("expected media playlist"),
        }
    }

    #[test]
    fn test_parse_m3u8_bytes_ext_x_map_sticky_across_segments() {
        // EXT-X-MAP must no longer be rejected (#682) and must stick to every
        // segment that follows it (RFC 8216 §4.3.2.4), not just the segment the
        // tag is textually attached to.
        let base_url = "https://cdn.example.com/live/media.m3u8";
        let playlist = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n\
#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-MAP:URI=\"init.mp4\"\n\
#EXTINF:6.000,\nseg0.m4s\n#EXTINF:6.000,\nseg1.m4s\n#EXT-X-ENDLIST\n";
        let content = match parse_m3u8_bytes(base_url, playlist.as_bytes()) {
            Ok(c) => c,
            Err(e) => panic!("EXT-X-MAP playlist must parse, got error: {e}"),
        };
        match content {
            M3u8Content::Media {
                segments, has_map, ..
            } => {
                assert!(has_map);
                assert_eq!(segments.len(), 2);
                let expected_uri = "https://cdn.example.com/live/init.mp4";
                for (i, seg) in segments.iter().enumerate() {
                    match &seg.map {
                        Some(m) => {
                            assert_eq!(m.uri, expected_uri, "segment {i} map uri mismatch");
                            assert_eq!(m.byte_range, None);
                        }
                        None => panic!("segment {i} must carry the sticky EXT-X-MAP"),
                    }
                }
                // Stickiness: both segments resolve to the identical map.
                assert!(
                    segments[0].map == segments[1].map,
                    "map must be identical across segments sharing one EXT-X-MAP tag"
                );
            }
            M3u8Content::Master { .. } => panic!("expected media playlist"),
        }
    }

    #[test]
    fn test_parse_m3u8_bytes_ext_x_map_byte_range() {
        // EXT-X-MAP's BYTERANGE attribute uses the same `<length>@<offset>`
        // grammar as EXT-X-BYTERANGE; unlike segment byte-ranges, a missing
        // offset means 0 (no cross-tag implicit chaining for init segments).
        let base_url = "https://cdn.example.com/live/media.m3u8";
        let playlist = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n\
#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"800@100\"\n\
#EXTINF:6.000,\nseg0.m4s\n#EXT-X-ENDLIST\n";
        let content = match parse_m3u8_bytes(base_url, playlist.as_bytes()) {
            Ok(c) => c,
            Err(e) => panic!("EXT-X-MAP with BYTERANGE must parse, got error: {e}"),
        };
        match content {
            M3u8Content::Media { segments, .. } => match &segments[0].map {
                Some(m) => assert_eq!(m.byte_range, Some((100, 800)), "(offset, length)"),
                None => panic!("segment must carry map"),
            },
            M3u8Content::Master { .. } => panic!("expected media playlist"),
        }
    }

    // --- #682: writer-side init-segment prepend decision ---

    #[test]
    fn test_should_write_init() {
        let map_a = ("https://cdn.example.com/init.mp4".to_string(), None);
        let map_b = (
            "https://cdn.example.com/init2.mp4".to_string(),
            Some((0u64, 100u64)),
        );

        // MPEG-TS segment (no map in effect) never needs an init write.
        assert!(!should_write_init(None, None));
        assert!(!should_write_init(Some(&map_a), None));

        // First appearance of a map (nothing written to disk yet) -> write it.
        assert!(should_write_init(None, Some(&map_a)));

        // Same map as the one already on disk -> skip, avoid duplicating bytes.
        assert!(!should_write_init(Some(&map_a), Some(&map_a)));

        // Playlist switched to a different init segment -> write again.
        assert!(should_write_init(Some(&map_a), Some(&map_b)));
    }

    use super::{
        HlsRequestCtx, HlsVariant, SegmentTransport, VariantId, format_resume_checkpoint,
        match_saved_variant, parse_checkpoint_variant, resume_is_usable,
    };

    fn variant(bandwidth: u64, resolution: Option<(u64, u64)>, uri: &str) -> HlsVariant {
        HlsVariant {
            bandwidth,
            average_bandwidth: None,
            resolution,
            uri: uri.to_string(),
            audio_uri: None,
        }
    }

    #[test]
    fn parse_iv_hex_rejects_multibyte_without_panic() {
        // 32 字节但含多字节字符:按字节切片会落在字符中间。
        let iv = format!("0中{}", "a".repeat(28));
        assert_eq!(iv.len(), 32);
        assert!(parse_iv_hex(&iv).is_err());
    }

    #[test]
    fn checkpoint_roundtrips_selected_variant_and_stays_backward_compatible() {
        let v = variant(
            2_000_000,
            Some((1280, 720)),
            "https://cdn.example.com/v/720/index.m3u8?token=abc",
        );
        let id = VariantId::of(&v);
        assert_eq!(id.path, "/v/720/index.m3u8");
        let cp = format_resume_checkpoint(5, 1024, 0, Some(&id.encode()));
        assert_eq!(parse_resume_checkpoint(&cp), (5, 1024, Some(0)));
        assert_eq!(parse_checkpoint_variant(&cp), Some(id));
        // 旧格式没有变体段。
        assert_eq!(parse_checkpoint_variant("5:1024:0"), None);
        assert_eq!(parse_checkpoint_variant("5:1024"), None);
        // 无分辨率的变体同样可往返。
        let no_res = VariantId::of(&variant(1, None, "https://a.example/x.m3u8"));
        assert_eq!(VariantId::decode(&no_res.encode()), Some(no_res));
    }

    #[test]
    fn match_saved_variant_survives_rotated_signature_token() {
        let saved = VariantId::of(&variant(
            2_000_000,
            Some((1280, 720)),
            "https://a.example/720/i.m3u8?token=old",
        ));
        let variants = [
            variant(
                800_000,
                Some((640, 360)),
                "https://a.example/360/i.m3u8?token=new",
            ),
            variant(
                2_000_000,
                Some((1280, 720)),
                "https://a.example/720/i.m3u8?token=new",
            ),
        ];
        assert_eq!(match_saved_variant(&variants, &saved), Some(1));
    }

    #[test]
    fn match_saved_variant_rejects_ambiguous_or_missing() {
        let variants = [
            variant(2_000_000, Some((1280, 720)), "https://a.example/a/x.m3u8"),
            variant(2_000_000, Some((1280, 720)), "https://a.example/b/y.m3u8"),
        ];
        // 画质相同无法区分、path 也对不上 -> 不能猜。
        let ambiguous = VariantId {
            bandwidth: 2_000_000,
            resolution: Some((1280, 720)),
            path: "/c/z.m3u8".to_string(),
        };
        assert_eq!(match_saved_variant(&variants, &ambiguous), None);
        // 画质变了但 path 唯一对应 -> 按 path 找回。
        let by_path = VariantId {
            bandwidth: 1,
            resolution: None,
            path: "/b/y.m3u8".to_string(),
        };
        assert_eq!(match_saved_variant(&variants, &by_path), Some(1));
        let missing = VariantId {
            bandwidth: 1,
            resolution: None,
            path: "/nope.m3u8".to_string(),
        };
        assert_eq!(match_saved_variant(&variants, &missing), None);
    }

    #[test]
    fn resume_is_usable_guards_prefix_validity() {
        assert!(resume_is_usable(3, 100, 120, 10, false, false));
        // 检查点段数超过新播放列表:前缀无法对齐。
        assert!(!resume_is_usable(11, 100, 120, 10, false, false));
        assert!(resume_is_usable(10, 100, 120, 10, false, false));
        assert!(!resume_is_usable(0, 100, 120, 10, false, false));
        assert!(!resume_is_usable(3, 0, 120, 10, false, false));
        assert!(!resume_is_usable(3, 100, 0, 10, false, false));
        assert!(!resume_is_usable(3, 100, 120, 10, true, false));
        assert!(!resume_is_usable(3, 100, 120, 10, false, true));
    }

    /// 清单引出的每个请求都以用户清单 URL 为凭据锚点：Cookie 发往同站主机（含子域与其他
    /// 端口），Authorization 只发往同源；跨站与 https → http 降级两者都不带。
    #[test]
    fn segment_credentials_follow_manifest_scope() {
        let mut identity = crate::downloader::RequestSpec::empty_get();
        identity.cookies = "sid=1".to_string();
        identity
            .extra_headers
            .insert("Authorization".to_string(), "Bearer t".to_string());
        identity
            .extra_headers
            .insert("Proxy-Authorization".to_string(), "Basic x".to_string());
        identity
            .extra_headers
            .insert("X-Token".to_string(), "keep".to_string());
        let spec = crate::downloader::DownloadSpec::new(identity.clone());
        let ctx = HlsRequestCtx {
            spec: &spec,
            header_origin_url: "https://www.site.example/master.m3u8",
        };
        let client = reqwest::Client::new();
        let sent = |url: &str| {
            let request = ctx.get(&client, url, &identity).first_hop();
            let has = |name: &str| request.headers().contains_key(name);
            (
                has("cookie"),
                has("authorization") && has("proxy-authorization"),
                has("x-token"),
            )
        };

        assert_eq!(sent("https://www.site.example/seg0.ts"), (true, true, true));
        assert_eq!(
            sent("https://cdn.site.example/seg0.ts"),
            (true, false, true)
        );
        assert_eq!(
            sent("https://www.site.example:8443/seg0.ts"),
            (true, false, true)
        );
        assert_eq!(
            sent("https://cdn.other.example/seg0.ts"),
            (false, false, true)
        );
        assert_eq!(
            sent("http://www.site.example/seg0.ts"),
            (false, false, true)
        );
    }

    #[test]
    fn playlist_with_bom_and_leading_whitespace_parses() {
        let playlist = b"\xEF\xBB\xBF\n  #EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.0,\nseg0.ts\n#EXT-X-ENDLIST\n";
        assert!(parse_m3u8_bytes("https://example.com/a.m3u8", playlist).is_ok());
    }

    #[test]
    fn ext_x_bitrate_is_sticky_and_skips_byte_range_segments() -> Result<(), DownloadError> {
        let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n\
                        #EXT-X-BITRATE:1200\n#EXTINF:6,\ns0.ts\n#EXTINF:6,\ns1.ts\n\
                        #EXT-X-BYTERANGE:100@0\n#EXTINF:6,\nall.ts\n\
                        #EXT-X-BITRATE:800\n#EXTINF:6,\ns3.ts\n#EXT-X-ENDLIST\n";
        let M3u8Content::Media { segments, .. } =
            parse_m3u8_bytes("https://example.com/a.m3u8", playlist.as_bytes())?
        else {
            panic!("expected media playlist");
        };
        let rates: Vec<Option<u64>> = segments.iter().map(|s| s.bitrate_kbps).collect();
        assert_eq!(rates, [Some(1200), Some(1200), None, Some(800)]);
        Ok(())
    }

    #[test]
    fn media_playlist_live_flag() {
        let base = "https://example.com/a.m3u8";
        let live_of = |body: &str| match parse_m3u8_bytes(base, body.as_bytes()) {
            Ok(M3u8Content::Media { live, .. }) => live,
            _ => panic!("expected media playlist"),
        };
        let head = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n";
        let seg = "#EXTINF:6.0,\nseg0.ts\n";
        assert!(live_of(&format!("{head}{seg}")));
        assert!(!live_of(&format!("{head}{seg}#EXT-X-ENDLIST\n")));
        assert!(!live_of(&format!("{head}#EXT-X-PLAYLIST-TYPE:VOD\n{seg}")));
        assert!(live_of(&format!("{head}#EXT-X-PLAYLIST-TYPE:EVENT\n{seg}")));
    }

    #[test]
    fn master_skips_iframe_variants_and_reports_external_audio() {
        let master = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"en\",DEFAULT=YES,URI=\"audio/en.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,AUDIO=\"aud\"\n\
v360.m3u8\n\
#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=9000000,RESOLUTION=1920x1080,URI=\"iframe.m3u8\"\n";
        match parse_m3u8_bytes("https://example.com/m.m3u8", master.as_bytes()) {
            Ok(M3u8Content::Master { variants }) => {
                assert_eq!(variants.len(), 1, "I-frame variant must be skipped");
                assert_eq!(variants[0].bandwidth, 800_000);
                assert_eq!(
                    variants[0].audio_uri.as_deref(),
                    Some("https://example.com/audio/en.m3u8")
                );
            }
            _ => panic!("expected master playlist"),
        }
    }

    /// 起一个只接受一次连接的服务器:`send_headers` 为 true 时先发响应头和部分
    /// body 再挂住,否则读完请求后什么都不发;返回地址与释放信号。
    async fn stalling_server(
        send_headers: bool,
    ) -> Result<(std::net::SocketAddr, tokio::sync::oneshot::Sender<()>), std::io::Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = [0u8; 1024];
            let received = socket
                .read(&mut request)
                .await
                .expect("read stalled fixture request");
            if received == 0 {
                return;
            }
            if send_headers {
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nhello",
                    )
                    .await
                    .expect("write stalled fixture response");
            }
            release_rx.await.expect("release stalled fixture");
        });
        Ok((address, release_tx))
    }

    async fn stalled_segment_error(
        send_headers: bool,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let (address, release_tx) = stalling_server(send_headers).await?;
        let client = reqwest::Client::builder().no_proxy().build()?;
        let meter = super::SegmentMeter::new(crate::speed_limiter::SpeedLimiter::new(0));
        let cancel = tokio_util::sync::CancellationToken::new();
        let url = format!("http://{address}/segment.ts");
        let spec =
            crate::downloader::DownloadSpec::new(crate::downloader::RequestSpec::empty_get());
        let ctx = HlsRequestCtx {
            spec: &spec,
            header_origin_url: &url,
        };
        let transport = SegmentTransport {
            client: &client,
            ctx: &ctx,
            cancel_token: &cancel,
            meter: &meter,
            progress: None,
            header_timeout: std::time::Duration::from_millis(200),
            idle_timeout: std::time::Duration::from_millis(200),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::download_segment_once(&transport, &url, None, 0),
        )
        .await?;
        release_tx
            .send(())
            .map_err(|()| "stalled fixture exited before release")?;
        match result {
            Err(super::RequestFailure::Retryable(DownloadError::Other(message))) => Ok(message),
            other => Err(format!("expected stalled error, got ok={}", other.is_ok()).into()),
        }
    }

    #[tokio::test]
    async fn segment_body_stall_is_reported_as_stalled() -> Result<(), Box<dyn std::error::Error>> {
        let message = stalled_segment_error(true).await?;
        assert!(message.contains("stalled"), "{message}");
        Ok(())
    }

    #[tokio::test]
    async fn segment_header_stall_is_reported_as_stalled() -> Result<(), Box<dyn std::error::Error>>
    {
        let message = stalled_segment_error(false).await?;
        assert!(message.contains("stalled"), "{message}");
        Ok(())
    }

    #[test]
    fn audio_track_tag_ignores_signed_query() {
        assert_eq!(
            super::audio_track_tag("https://cdn.example.com/a/en.m3u8?token=aaa&exp=1"),
            super::audio_track_tag("https://cdn.example.com/a/en.m3u8?token=bbb&exp=2")
        );
        assert_ne!(
            super::audio_track_tag("https://cdn.example.com/a/en.m3u8"),
            super::audio_track_tag("https://cdn.example.com/a/fr.m3u8")
        );
    }

    #[test]
    fn write_failure_reports_full_disk_as_actionable_error() {
        let full = std::io::Error::from(std::io::ErrorKind::StorageFull);
        match super::write_failure(full) {
            DownloadError::Other(message) => assert!(message.contains("磁盘空间不足")),
            other => panic!("expected Other, got {other}"),
        }
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(super::write_failure(denied), DownloadError::Io(_)));
    }

    /// 按路径返回固定 body 的服务器(404 兜底),记录收到的请求路径。
    async fn static_server(
        routes: Vec<(&'static str, Vec<u8>)>,
    ) -> Result<
        (
            std::net::SocketAddr,
            std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        ),
        std::io::Error,
    > {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let served = std::sync::Arc::clone(&hits);
        let routes = std::sync::Arc::new(routes);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let routes = std::sync::Arc::clone(&routes);
                let served = std::sync::Arc::clone(&served);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = request
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    if let Ok(mut hits) = served.lock() {
                        hits.push(path.clone());
                    }
                    let body = routes
                        .iter()
                        .find(|(route, _)| *route == path)
                        .map(|(_, body)| body.clone());
                    let response = match body {
                        Some(body) => {
                            let mut response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .into_bytes();
                            response.extend(body);
                            response
                        }
                        None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec(),
                    };
                    if let Err(error) = socket.write_all(&response).await {
                        tracing::debug!(%error, "HLS fixture client disconnected");
                    }
                });
            }
        });
        Ok((address, hits))
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir =
            std::env::temp_dir().join(format!("fluxdown_hls_{tag}_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create HLS fixture directory");
        dir
    }

    const AUDIO_SEGMENTS: [&[u8]; 3] = [b"AAAA-0000", b"BBBB-1111-22", b"CCCC-3"];

    /// 以固定的 3 段音轨跑一次 `run_audio_track`,返回 `(总字节, 进度原子值)`。
    async fn run_audio_fixture(
        address: std::net::SocketAddr,
        temp_path: &std::path::Path,
        db: &crate::db::Db,
        is_resume: bool,
    ) -> Result<(i64, i64), Box<dyn std::error::Error>> {
        let playlist_url = format!("http://{address}/audio/index.m3u8");
        let segments: Vec<super::HlsSegment> = (0..AUDIO_SEGMENTS.len())
            .map(|i| super::HlsSegment {
                uri: format!("http://{address}/audio/seg{i}.ts"),
                duration: 1.0,
                key: None,
                byte_range: None,
                bitrate_kbps: None,
                discontinuity: false,
                map: None,
                sequence: i as u64,
            })
            .collect();
        let written = std::sync::Arc::new(super::TrackProgress::new(&segments));
        let run = super::AudioRun {
            track: super::AudioTrack {
                segments,
                media_sequence: 0,
                live_target: None,
                raw: bytes::Bytes::new(),
                filter_changed: false,
                playlist_url: playlist_url.clone(),
                temp_path: temp_path.to_path_buf(),
                resume_key: super::audio_resume_key("t-audio"),
                tag: super::audio_track_tag(&playlist_url),
            },
            client: reqwest::Client::builder().no_proxy().build()?,
            spec: std::sync::Arc::new(crate::downloader::DownloadSpec::new(
                crate::downloader::RequestSpec::empty_get(),
            )),
            header_origin_url: playlist_url,
            cancel: tokio_util::sync::CancellationToken::new(),
            task_id: "t-audio".to_owned(),
            db: db.clone(),
            sink: std::sync::Arc::new(crate::NoopSink),
            key_cache: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            meter: super::SegmentMeter::new(crate::speed_limiter::SpeedLimiter::new(0)),
            written: std::sync::Arc::clone(&written),
            segment_limit: 2,
            connection_cap: 0,
            is_resume,
        };
        let total = super::run_audio_track(run).await?;
        Ok((total, written.written()))
    }

    fn audio_routes() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("/audio/seg0.ts", AUDIO_SEGMENTS[0].to_vec()),
            ("/audio/seg1.ts", AUDIO_SEGMENTS[1].to_vec()),
            ("/audio/seg2.ts", AUDIO_SEGMENTS[2].to_vec()),
        ]
    }

    #[tokio::test]
    async fn audio_track_checkpoint_failure_preserves_durable_prefix_and_can_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let (address, _) = static_server(audio_routes()).await?;
        let dir = scratch_dir("audio_checkpoint_failure");
        let temp = dir.join("clip.audio.m4a.fdownloading");
        let url = format!("sqlite://{}?mode=rwc", dir.join("checkpoint.db").display());
        let db = crate::db::Db::connect(&url).await?;
        let pool = sqlx::AnyPool::connect(&url).await?;
        sqlx::query("CREATE TRIGGER fail_checkpoint BEFORE INSERT ON config WHEN NEW.key = 'hls_audio_resume_t-audio' AND NEW.value NOT LIKE '0:0:%' BEGIN SELECT RAISE(ABORT, 'checkpoint write failed'); END")
            .execute(&pool).await?;
        let result = run_audio_fixture(address, &temp, &db, false).await;
        assert!(
            matches!(&result, Err(error) if error.to_string().contains("checkpoint write failed")),
            "{result:?}"
        );
        // 未落库检查点之后的段可能已在磁盘上(检查点节流);续传必须把它们截掉重下。
        assert!(std::fs::read(&temp)?.starts_with(AUDIO_SEGMENTS[0]));
        let checkpoint = db
            .get_config(&super::audio_resume_key("t-audio"))
            .await?
            .unwrap_or_default();
        assert!(checkpoint.starts_with("0:0:"), "{checkpoint}");
        sqlx::query("DROP TRIGGER fail_checkpoint")
            .execute(&pool)
            .await?;
        run_audio_fixture(address, &temp, &db, true).await?;
        assert_eq!(std::fs::read(&temp)?, AUDIO_SEGMENTS.concat());
        pool.close().await;
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            crate::logger::report_warning("hls-test", "remove_checkpoint_fixture", &error);
        }
        Ok(())
    }

    #[tokio::test]
    async fn audio_track_is_written_in_order_with_progress()
    -> Result<(), Box<dyn std::error::Error>> {
        let (address, hits) = static_server(audio_routes()).await?;
        let dir = scratch_dir("audio_order");
        let temp = dir.join("clip.audio.m4a.fdownloading");
        let db = crate::db::Db::connect("sqlite::memory:").await?;

        let (total, progress) = run_audio_fixture(address, &temp, &db, false).await?;

        let expected: Vec<u8> = AUDIO_SEGMENTS.concat();
        assert_eq!(std::fs::read(&temp)?, expected);
        assert_eq!(total, expected.len() as i64);
        assert_eq!(progress, expected.len() as i64);
        assert_eq!(hits.lock().map(|h| h.len()).unwrap_or_default(), 3);
        let checkpoint = db
            .get_config(&super::audio_resume_key("t-audio"))
            .await?
            .unwrap_or_default();
        assert!(
            checkpoint.starts_with(&format!("3:{}:0:", expected.len())),
            "{checkpoint}"
        );
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn audio_track_resumes_from_checkpoint_and_drops_partial_tail()
    -> Result<(), Box<dyn std::error::Error>> {
        let (address, hits) = static_server(audio_routes()).await?;
        let dir = scratch_dir("audio_resume");
        let temp = dir.join("clip.audio.m4a.fdownloading");
        let db = crate::db::Db::connect("sqlite::memory:").await?;

        // 前两段已完整落盘,第三段写了半截垃圾(崩溃残留)。
        let prefix: Vec<u8> = AUDIO_SEGMENTS[..2].concat();
        let mut on_disk = prefix.clone();
        on_disk.extend_from_slice(b"CC");
        std::fs::write(&temp, &on_disk)?;
        let playlist_url = format!("http://{address}/audio/index.m3u8");
        db.set_config(
            &super::audio_resume_key("t-audio"),
            &super::format_resume_checkpoint(
                2,
                prefix.len() as i64,
                0,
                Some(&super::audio_track_tag(&playlist_url)),
            ),
        )
        .await?;

        let (total, _) = run_audio_fixture(address, &temp, &db, true).await?;

        let expected: Vec<u8> = AUDIO_SEGMENTS.concat();
        assert_eq!(std::fs::read(&temp)?, expected);
        assert_eq!(total, expected.len() as i64);
        let requested = hits.lock().map(|h| h.clone()).unwrap_or_default();
        assert_eq!(requested, vec!["/audio/seg2.ts".to_owned()]);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn audio_track_restarts_when_checkpoint_belongs_to_another_rendition()
    -> Result<(), Box<dyn std::error::Error>> {
        let (address, hits) = static_server(audio_routes()).await?;
        let dir = scratch_dir("audio_other");
        let temp = dir.join("clip.audio.m4a.fdownloading");
        let db = crate::db::Db::connect("sqlite::memory:").await?;

        let prefix: Vec<u8> = AUDIO_SEGMENTS[..2].concat();
        std::fs::write(&temp, &prefix)?;
        db.set_config(
            &super::audio_resume_key("t-audio"),
            &super::format_resume_checkpoint(
                2,
                prefix.len() as i64,
                0,
                Some(&super::audio_track_tag("https://other.example.com/fr.m3u8")),
            ),
        )
        .await?;

        let (total, _) = run_audio_fixture(address, &temp, &db, true).await?;

        // 磁盘前缀属于另一条音轨:不能拼接,必须整条重下。
        let expected: Vec<u8> = AUDIO_SEGMENTS.concat();
        assert_eq!(std::fs::read(&temp)?, expected);
        assert_eq!(total, expected.len() as i64);
        assert_eq!(hits.lock().map(|h| h.len()).unwrap_or_default(), 3);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn unusable_ffmpeg_is_not_reported_available() {
        assert!(
            !crate::dash_downloader::ffmpeg_usable(std::path::Path::new(
                "/nonexistent/dir/ffmpeg-does-not-exist"
            ))
            .await
        );
    }

    /// 用 `FLUXDOWN_TEST_FFMPEG` 指向的真实 ffmpeg 生成 1 秒测试 TS;未设置返回 `None`
    /// (CI 无 ffmpeg 时跳过真实执行)。
    fn ffmpeg_fixture(
        dir: &std::path::Path,
        name: &str,
        streams: &[&str],
    ) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let ffmpeg = std::path::PathBuf::from(std::env::var("FLUXDOWN_TEST_FFMPEG").ok()?);
        let out = dir.join(name);
        let mut cmd = std::process::Command::new(&ffmpeg);
        cmd.args(["-y", "-loglevel", "error"]);
        for stream in streams {
            match *stream {
                "video" => {
                    cmd.args(["-f", "lavfi", "-i", "testsrc=size=64x64:rate=10:duration=1"]);
                }
                _ => {
                    cmd.args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"]);
                }
            }
        }
        for (i, stream) in streams.iter().enumerate() {
            let codec = if *stream == "video" { "-c:v" } else { "-c:a" };
            let name = if *stream == "video" { "mpeg4" } else { "aac" };
            cmd.args(["-map", &format!("{i}"), codec, name]);
        }
        let status = cmd.args(["-f", "mpegts"]).arg(&out).status().ok()?;
        status.success().then_some((ffmpeg, out))
    }

    fn probe_streams(ffmpeg: &std::path::Path, file: &std::path::Path) -> String {
        let output = std::process::Command::new(ffmpeg)
            .arg("-i")
            .arg(file)
            .output();
        output
            .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn remux_streams_ts_into_mp4_when_ffmpeg_is_available() {
        let dir = scratch_dir("remux_ffmpeg");
        let Some((ffmpeg, ts)) = ffmpeg_fixture(&dir, "clip.ts", &["video", "audio"]) else {
            eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过真实 ffmpeg remux");
            std::fs::remove_dir_all(&dir).expect("remove HLS fixture directory");
            return;
        };
        let cancel = tokio_util::sync::CancellationToken::new();

        let mp4 = super::remux_ts_to_mp4(
            &ts,
            "t",
            &crate::file_exists::OverwritePolicy::Never,
            false,
            Some(ffmpeg.as_path()),
            &cancel,
        )
        .await;

        let Some(mp4) = mp4 else {
            panic!("ffmpeg remux must produce an mp4");
        };
        let Ok(bytes) = std::fs::read(&mp4) else {
            panic!("read mp4");
        };
        assert_eq!(&bytes[4..8], b"ftyp");
        assert!(ts.exists(), "remux 不删除源 .ts,由调用方在落库后清理");
        let probe = probe_streams(&ffmpeg, &mp4);
        assert!(
            probe.contains("Video:") && probe.contains("Audio:"),
            "{probe}"
        );
        std::fs::remove_dir_all(&dir).expect("remove HLS fixture directory");
    }

    /// 无 ffmpeg 时走流式重封装:产出可播放的音视频 MP4,源 `.ts` 与临时文件都不留残。
    #[tokio::test]
    async fn remux_without_ffmpeg_streams_h264_ts_into_playable_mp4() {
        let Some(ffmpeg) = std::env::var_os("FLUXDOWN_TEST_FFMPEG").map(std::path::PathBuf::from)
        else {
            eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过流式 remux 真实素材");
            return;
        };
        let dir = scratch_dir("remux_streaming");
        let ts = dir.join("clip.ts");
        let encoded = std::process::Command::new(&ffmpeg)
            .args(["-y", "-loglevel", "error"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=25:duration=3",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=3",
            ])
            .args(["-c:v", "libx264", "-c:a", "aac", "-f", "mpegts"])
            .arg(&ts)
            .status()
            .is_ok_and(|status| status.success());
        if !encoded {
            eprintln!("[skip] ffmpeg 无法编码 H.264 测试素材");
            std::fs::remove_dir_all(&dir).expect("remove HLS fixture directory");
            return;
        }
        let cancel = tokio_util::sync::CancellationToken::new();

        let mp4 = super::remux_ts_to_mp4(
            &ts,
            "t",
            &crate::file_exists::OverwritePolicy::Never,
            false,
            None,
            &cancel,
        )
        .await;

        let Some(mp4) = mp4 else {
            panic!("streaming remux must produce an mp4");
        };
        assert_eq!(mp4.file_name().and_then(|n| n.to_str()), Some("clip.mp4"));
        assert!(ts.exists(), "remux 不删除源 .ts,由调用方在落库后清理");
        assert!(!dir.join("clip.mp4.tmp").exists());
        let probe = probe_streams(&ffmpeg, &mp4);
        assert!(
            probe.contains("Video: h264") && probe.contains("Audio: aac"),
            "{probe}"
        );
        std::fs::remove_dir_all(&dir).expect("remove HLS fixture directory");
    }

    #[tokio::test]
    async fn ffmpeg_copy_merges_separate_audio_into_video() {
        let dir = scratch_dir("mux_ffmpeg");
        let Some((ffmpeg, video)) = ffmpeg_fixture(&dir, "video.ts", &["video"]) else {
            eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过真实 ffmpeg mux");
            std::fs::remove_dir_all(&dir).expect("remove HLS fixture directory");
            return;
        };
        let Some((_, audio)) = ffmpeg_fixture(&dir, "audio.ts", &["audio"]) else {
            std::fs::remove_dir_all(&dir).expect("remove HLS fixture directory");
            return;
        };
        let out = dir.join("merged.mp4.fdownloading");
        let cancel = tokio_util::sync::CancellationToken::new();

        let result = crate::dash_downloader::ffmpeg_copy_to_mp4(
            &video,
            Some(audio.as_path()),
            &out,
            1024,
            &cancel,
            &ffmpeg,
        )
        .await;

        assert!(result.is_ok(), "{result:?}");
        let Ok(bytes) = std::fs::read(&out) else {
            panic!("read merged");
        };
        assert_eq!(&bytes[4..8], b"ftyp");
        let probe = probe_streams(&ffmpeg, &out);
        assert!(
            probe.contains("Video:") && probe.contains("Audio:"),
            "{probe}"
        );
        std::fs::remove_dir_all(&dir).expect("remove HLS fixture directory");
    }

    // ---------------------------------------------------------------------
    // EXT-X-KEY:METHOD=NONE — 结束加密,而非沿用上一段密钥
    // ---------------------------------------------------------------------

    #[test]
    fn test_parse_m3u8_bytes_ext_x_key_method_none_ends_encryption() {
        // METHOD=NONE 声明其后的段为明文。该标签若被忽略,明文段会沿用上一段的
        // AES-128 密钥,下载阶段以 PKCS7 unpad 失败告终。
        let base_url = "https://cdn.example.com/live/media.m3u8";
        let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n\
#EXT-X-MEDIA-SEQUENCE:0\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x00000000000000000000000000000001\n\
#EXTINF:6.000,\nseg0.ts\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:6.000,\nseg1.ts\n#EXT-X-ENDLIST\n";
        let content = match parse_m3u8_bytes(base_url, playlist.as_bytes()) {
            Ok(c) => c,
            Err(e) => panic!("playlist must parse, got error: {e}"),
        };
        match content {
            M3u8Content::Media { segments, .. } => {
                assert_eq!(segments.len(), 2);
                let encrypted = match &segments[0].key {
                    Some(k) => k,
                    None => panic!("segment 0 must stay AES-128 encrypted"),
                };
                assert!(
                    encrypted.method == HlsKeyMethod::Aes128,
                    "segment 0 must stay AES-128 encrypted"
                );
                assert_eq!(encrypted.uri, "https://cdn.example.com/live/key.bin");
                assert!(
                    segments[1].key.is_none(),
                    "METHOD=NONE must end encryption instead of inheriting the previous key"
                );
            }
            M3u8Content::Master { .. } => panic!("expected media playlist"),
        }
    }

    #[test]
    fn test_parse_m3u8_bytes_unrelated_unknown_tag_keeps_sticky_key() {
        // 兜底只认 METHOD=NONE:其他未知标签不得改动粘性密钥状态。
        let base_url = "https://cdn.example.com/live/media.m3u8";
        let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n\
#EXT-X-MEDIA-SEQUENCE:0\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x00000000000000000000000000000001\n\
#EXTINF:6.000,\nseg0.ts\n#EXT-X-UNKNOWN-TAG:whatever\n#EXTINF:6.000,\nseg1.ts\n#EXT-X-ENDLIST\n";
        let content = match parse_m3u8_bytes(base_url, playlist.as_bytes()) {
            Ok(c) => c,
            Err(e) => panic!("playlist must parse, got error: {e}"),
        };
        match content {
            M3u8Content::Media { segments, .. } => {
                assert_eq!(segments.len(), 2);
                let inherited = match &segments[1].key {
                    Some(k) => k,
                    None => panic!("AES-128 must stay in effect across segments"),
                };
                assert!(
                    inherited.method == HlsKeyMethod::Aes128,
                    "AES-128 must stay in effect across segments"
                );
                assert_eq!(inherited.uri, "https://cdn.example.com/live/key.bin");
            }
            M3u8Content::Master { .. } => panic!("expected media playlist"),
        }
    }

    // ---------------------------------------------------------------------
    // 直播录制
    // ---------------------------------------------------------------------

    #[test]
    fn discard_registry_cancels_registered_and_late_tokens_and_clears() {
        let id = "discard-registry-unit";
        let early = super::discard_token(id);
        assert!(!early.is_cancelled());
        super::request_discard(id);
        assert!(
            early.is_cancelled(),
            "request must reach an already held token"
        );
        assert!(
            super::discard_token(id).is_cancelled(),
            "a downloader starting after the request must observe it"
        );
        super::clear_discard(id);
        let fresh = super::discard_token(id);
        assert!(!fresh.is_cancelled(), "clear must forget the request");
        super::clear_discard(id);
    }

    #[test]
    fn live_poll_interval_is_clamped_to_one_to_ten_seconds() {
        assert_eq!(
            super::live_poll_interval(0),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            super::live_poll_interval(1),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            super::live_poll_interval(6),
            std::time::Duration::from_secs(6)
        );
        assert_eq!(
            super::live_poll_interval(30),
            std::time::Duration::from_secs(10)
        );
    }

    #[test]
    fn plan_live_segments_dedups_and_reports_gaps() {
        use super::{LivePlan, plan_live_segments};
        // 首次:取整个当前窗口。
        assert_eq!(
            plan_live_segments(None, 100, 4),
            LivePlan {
                start_idx: 0,
                missed: 0,
                regressed: false
            }
        );
        // 窗口滑动,只取序号 ≥ 期望序号的新分片。
        assert_eq!(
            plan_live_segments(Some(102), 100, 4),
            LivePlan {
                start_idx: 2,
                missed: 0,
                regressed: false
            }
        );
        // 无新分片(期望序号恰为窗口末尾之后)。
        assert_eq!(
            plan_live_segments(Some(104), 100, 4),
            LivePlan {
                start_idx: 4,
                missed: 0,
                regressed: false
            }
        );
        // 序号跳跃:期望的 98、99 已滚出窗口。
        assert_eq!(
            plan_live_segments(Some(98), 100, 4),
            LivePlan {
                start_idx: 0,
                missed: 2,
                regressed: false
            }
        );
        // 快照整体落后于已录进度(源站重启 / 旧缓存):不下载任何分片。
        assert_eq!(
            plan_live_segments(Some(500), 0, 4),
            LivePlan {
                start_idx: 4,
                missed: 0,
                regressed: true
            }
        );
    }

    #[test]
    fn segment_jobs_carry_absolute_sequence_for_iv() {
        let mut segments: Vec<super::HlsSegment> = (0..4)
            .map(|i| super::HlsSegment {
                uri: format!("https://example.com/s{i}.ts"),
                duration: 1.0,
                key: None,
                byte_range: None,
                bitrate_kbps: None,
                discontinuity: false,
                map: None,
                sequence: 1000 + i as u64,
            })
            .collect();
        // 插件过滤掉中间一段后,剩余分片仍保留原绝对序号(隐式 IV 依赖它)。
        segments.remove(1);
        let jobs = super::segment_jobs(&segments, 1);
        assert_eq!(
            jobs.iter().map(|j| (j.idx, j.sequence)).collect::<Vec<_>>(),
            vec![(1, 1002), (2, 1003)]
        );
    }

    /// 同一路径按请求次序依次返回不同 body(用尽后沿用最后一个);`None` = 404。
    async fn sequenced_server(
        routes: Vec<(String, Vec<Option<Vec<u8>>>)>,
    ) -> Result<std::net::SocketAddr, std::io::Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let routes = std::sync::Arc::new(routes);
        let counts = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            usize,
        >::new()));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let routes = std::sync::Arc::clone(&routes);
                let counts = std::sync::Arc::clone(&counts);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = request
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let nth = match counts.lock() {
                        Ok(mut counts) => {
                            let hits = counts.entry(path.clone()).or_insert(0);
                            *hits += 1;
                            *hits - 1
                        }
                        Err(_) => 0,
                    };
                    let body =
                        routes
                            .iter()
                            .find(|(route, _)| *route == path)
                            .and_then(|(_, bodies)| {
                                bodies
                                    .get(nth.min(bodies.len().saturating_sub(1)))
                                    .cloned()
                                    .flatten()
                            });
                    let response = match body {
                        Some(body) => {
                            let mut response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .into_bytes();
                            response.extend(body);
                            response
                        }
                        None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec(),
                    };
                    if let Err(error) = socket.write_all(&response).await {
                        tracing::debug!(%error, "HLS fixture client disconnected");
                    }
                });
            }
        });
        Ok(address)
    }

    fn live_segment_body(sequence: u64) -> Vec<u8> {
        format!("S{sequence}-{}", "x".repeat(sequence as usize + 1)).into_bytes()
    }

    fn live_segment_routes(sequences: std::ops::Range<u64>) -> Vec<(String, Vec<Option<Vec<u8>>>)> {
        sequences
            .map(|n| (format!("/live/s{n}.ts"), vec![Some(live_segment_body(n))]))
            .collect()
    }

    fn live_playlist_text(media_sequence: u64, count: u64, end: bool) -> String {
        let mut text = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n"
        );
        for n in media_sequence..media_sequence + count {
            text.push_str(&format!("#EXTINF:1.0,\ns{n}.ts\n"));
        }
        if end {
            text.push_str("#EXT-X-ENDLIST\n");
        }
        text
    }

    fn live_playlist_body(media_sequence: u64, count: u64, end: bool) -> Option<Vec<u8>> {
        Some(live_playlist_text(media_sequence, count, end).into_bytes())
    }

    fn live_task_ctx() -> crate::intercept::TaskCtx {
        crate::intercept::TaskCtx {
            id: "t-live".to_owned(),
            url: "http://127.0.0.1/live/index.m3u8".to_owned(),
            source_url: "http://127.0.0.1/live/index.m3u8".to_owned(),
            file_name: "live.ts".to_owned(),
            save_dir: String::new(),
            protocol: "hls",
            track_pair: false,
            is_resume: false,
        }
    }

    async fn live_run_fixture(
        address: std::net::SocketAddr,
        first_playlist: &str,
        temp: super::LiveTemp,
        db: &crate::db::Db,
        cancel: tokio_util::sync::CancellationToken,
        written: std::sync::Arc<std::sync::atomic::AtomicI64>,
    ) -> Result<super::LiveRun, Box<dyn std::error::Error>> {
        let playlist_url = format!("http://{address}/live/index.m3u8");
        let playlist = super::LivePlaylist::from_fetched(super::FetchedPlaylist {
            content: super::parse_m3u8_bytes(&playlist_url, first_playlist.as_bytes())?,
            raw: bytes::Bytes::copy_from_slice(first_playlist.as_bytes()),
        })?;
        Ok(super::LiveRun {
            label: "视频",
            client: reqwest::Client::builder().no_proxy().build()?,
            spec: std::sync::Arc::new(crate::downloader::DownloadSpec::new(
                crate::downloader::RequestSpec::empty_get(),
            )),
            header_origin_url: playlist_url.clone(),
            playlist_url,
            playlist,
            cancel,
            task_id: "t-live".to_owned(),
            db: db.clone(),
            sink: std::sync::Arc::new(crate::NoopSink),
            key_cache: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            meter: super::SegmentMeter::new(crate::speed_limiter::SpeedLimiter::new(0)),
            written,
            concurrency: 3,
            temp,
            resume_key: "hls_resume_t-live".to_owned(),
            tag: None,
            persist_with: None,
            interceptor: crate::intercept::noop(),
            task: live_task_ctx(),
            track: super::SegmentTrack::Video,
            notes: crate::intercept::ActivityNotes::new(
                db.clone(),
                std::sync::Arc::new(crate::NoopSink),
                "t-live".to_owned(),
            ),
        })
    }

    async fn fresh_live_temp(path: &std::path::Path) -> Result<super::LiveTemp, std::io::Error> {
        Ok(super::LiveTemp {
            file: super::File::create(path).await?,
            bytes: 0,
            recorded: 0,
            next_seq: None,
        })
    }

    fn concat_segments(sequences: &[u64]) -> Vec<u8> {
        sequences
            .iter()
            .flat_map(|n| live_segment_body(*n))
            .collect()
    }

    #[tokio::test]
    async fn live_track_appends_new_segments_per_poll_until_endlist()
    -> Result<(), Box<dyn std::error::Error>> {
        // 首个快照 5,6;第二次轮询窗口滑到 6,7(6 重叠);第三次 7,8 且 ENDLIST。
        let mut routes = live_segment_routes(5..9);
        routes.push((
            "/live/index.m3u8".to_owned(),
            vec![
                live_playlist_body(6, 2, false),
                live_playlist_body(7, 2, true),
            ],
        ));
        let address = sequenced_server(routes).await?;
        let dir = scratch_dir("live_append");
        let temp = dir.join("live.ts.fdownloading");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let written = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
        let run = live_run_fixture(
            address,
            &live_playlist_text(5, 2, false),
            fresh_live_temp(&temp).await?,
            &db,
            tokio_util::sync::CancellationToken::new(),
            std::sync::Arc::clone(&written),
        )
        .await?;

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_live_track(run),
        )
        .await??;

        let expected = concat_segments(&[5, 6, 7, 8]);
        assert!(matches!(outcome.end, super::LiveEnd::EndList));
        assert_eq!(
            std::fs::read(&temp)?,
            expected,
            "every segment once, in order"
        );
        assert_eq!(outcome.bytes, expected.len() as i64);
        assert_eq!(
            written.load(std::sync::atomic::Ordering::Relaxed),
            expected.len() as i64
        );
        let checkpoint = db
            .get_config("hls_resume_t-live")
            .await?
            .unwrap_or_default();
        assert_eq!(checkpoint, format!("4:{}:9", expected.len()));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn live_track_sequence_gap_is_skipped_not_fatal() -> Result<(), Box<dyn std::error::Error>>
    {
        // 首个快照 0,1;下次轮询窗口已滚到 5,6(2–4 永久错过)且 ENDLIST。
        let mut routes = live_segment_routes(0..7);
        routes.push((
            "/live/index.m3u8".to_owned(),
            vec![live_playlist_body(5, 2, true)],
        ));
        let address = sequenced_server(routes).await?;
        let dir = scratch_dir("live_gap");
        let temp = dir.join("live.ts.fdownloading");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let run = live_run_fixture(
            address,
            &live_playlist_text(0, 2, false),
            fresh_live_temp(&temp).await?,
            &db,
            tokio_util::sync::CancellationToken::new(),
            std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
        )
        .await?;

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_live_track(run),
        )
        .await??;

        assert!(matches!(outcome.end, super::LiveEnd::EndList));
        assert_eq!(std::fs::read(&temp)?, concat_segments(&[0, 1, 5, 6]));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn live_track_stop_keeps_recorded_segments() -> Result<(), Box<dyn std::error::Error>> {
        // 播放列表永不结束;录到两个分片后用户停止。
        let mut routes = live_segment_routes(0..2);
        routes.push((
            "/live/index.m3u8".to_owned(),
            vec![live_playlist_body(0, 2, false)],
        ));
        let address = sequenced_server(routes).await?;
        let dir = scratch_dir("live_stop");
        let temp = dir.join("live.ts.fdownloading");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let written = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
        let run = live_run_fixture(
            address,
            &live_playlist_text(0, 2, false),
            fresh_live_temp(&temp).await?,
            &db,
            cancel.clone(),
            std::sync::Arc::clone(&written),
        )
        .await?;
        let expected = concat_segments(&[0, 1]);

        let handle = tokio::spawn(super::run_live_track(run));
        for _ in 0..400 {
            if written.load(std::sync::atomic::Ordering::Relaxed) >= expected.len() as i64 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        cancel.cancel();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await???;

        assert!(matches!(outcome.end, super::LiveEnd::Stopped));
        assert_eq!(std::fs::read(&temp)?, expected);
        assert_eq!(outcome.bytes, expected.len() as i64);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn live_track_gives_up_after_playlist_retries_but_keeps_recording()
    -> Result<(), Box<dyn std::error::Error>> {
        // 首个快照之后播放列表一直 404:按 MAX_RETRIES 退避后放弃,已录内容保留。
        let mut routes = live_segment_routes(0..2);
        routes.push(("/live/index.m3u8".to_owned(), vec![None]));
        let address = sequenced_server(routes).await?;
        let dir = scratch_dir("live_give_up");
        let temp = dir.join("live.ts.fdownloading");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let run = live_run_fixture(
            address,
            &live_playlist_text(0, 2, false),
            fresh_live_temp(&temp).await?,
            &db,
            tokio_util::sync::CancellationToken::new(),
            std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
        )
        .await?;

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            super::run_live_track(run),
        )
        .await??;

        match &outcome.end {
            super::LiveEnd::GaveUp(message) => {
                assert!(
                    message.contains(&super::MAX_RETRIES.to_string()),
                    "{message}"
                );
            }
            _ => panic!("expected the recording to give up"),
        }
        assert_eq!(std::fs::read(&temp)?, concat_segments(&[0, 1]));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn live_temp_resume_continues_from_saved_sequence()
    -> Result<(), Box<dyn std::error::Error>> {
        // 上次已录序号 3、4 并写了检查点(下一个期望序号 5),temp 末尾还有半截脏数据;
        // 续传后截到检查点字节边界,只追加 5、6。
        let address = sequenced_server({
            let mut routes = live_segment_routes(3..7);
            routes.push((
                "/live/index.m3u8".to_owned(),
                vec![live_playlist_body(3, 4, true)],
            ));
            routes
        })
        .await?;
        let dir = scratch_dir("live_resume");
        let temp = dir.join("live.ts.fdownloading");
        let prefix = concat_segments(&[3, 4]);
        let mut on_disk = prefix.clone();
        on_disk.extend_from_slice(b"partial-garbage");
        std::fs::write(&temp, &on_disk)?;
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        db.set_config(
            "hls_resume_t-live",
            &super::format_resume_checkpoint(2, prefix.len() as i64, 5, None),
        )
        .await?;

        let opened = super::open_live_temp(
            &db,
            super::LiveTempSpec {
                is_resume: true,
                temp_path: &temp,
                resume_key: "hls_resume_t-live",
                tag: None,
                strict_tag: false,
                abandon: false,
                first_sequence: 3,
            },
        )
        .await?;
        assert_eq!(opened.next_seq, Some(5));
        assert_eq!(opened.recorded, 2);
        let run = live_run_fixture(
            address,
            &live_playlist_text(3, 4, false),
            opened,
            &db,
            tokio_util::sync::CancellationToken::new(),
            std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
        )
        .await?;

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_live_track(run),
        )
        .await??;

        assert!(matches!(outcome.end, super::LiveEnd::EndList));
        assert_eq!(std::fs::read(&temp)?, concat_segments(&[3, 4, 5, 6]));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    // --- 插件拦截:分片过滤 / 指纹 / 直播过滤 / 分片上下文 ---

    use crate::intercept::InterceptStage;
    use crate::intercept::drive::tests::{ScriptedInterceptor, entry};

    fn scripted(stage: InterceptStage, reply: &str) -> std::sync::Arc<ScriptedInterceptor> {
        std::sync::Arc::new(
            ScriptedInterceptor::new(vec![entry("p@a", &[stage], false)]).reply(
                "p@a",
                stage,
                Ok(Some(reply)),
            ),
        )
    }

    /// 把进度通道排空,避免下载器的进度上报在有界通道上阻塞。
    fn drained_progress() -> tokio::sync::mpsc::Sender<crate::downloader::ProgressUpdate> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        tx
    }

    fn vod_params(
        task_id: &str,
        url: &str,
        dir: &std::path::Path,
        db: &crate::db::Db,
        interceptor: std::sync::Arc<dyn crate::intercept::TaskInterceptor>,
    ) -> Result<crate::downloader::DownloadParams, Box<dyn std::error::Error>> {
        Ok(crate::downloader::DownloadParams {
            spawn_gen: 1,
            unattended: true,
            preprobed: None,
            interceptor,
            connection_cap: 0,
            run_meta: crate::intercept::RunMeta::default(),
            auto_proxy: None,
            multi_nic: None,
            task_id: task_id.to_string(),
            url: url.to_string(),
            save_dir: dir.to_string_lossy().to_string(),
            file_name: "v.ts".to_string(),
            segment_count: 0,
            is_resume: false,
            range_verified: true,
            db: db.clone(),
            client: reqwest::Client::builder().no_proxy().build()?,
            progress_tx: drained_progress(),
            cancel_token: tokio_util::sync::CancellationToken::new(),
            sink: std::sync::Arc::new(crate::NoopSink),
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
        })
    }

    #[tokio::test]
    async fn filtered_aes_playlist_decrypts_with_original_sequence_iv()
    -> Result<(), Box<dyn std::error::Error>> {
        // 无显式 IV 的 AES-128:IV 来自绝对序号(5、6、7)。丢弃序号 6 后,序号 7 的分片
        // 在过滤后列表中下标变成 1,但仍必须用序号 7 的 IV 解密。
        let key = [7u8; 16];
        let plain: [&[u8]; 3] = [b"AAAA", b"BBBB", b"CCCC"];
        let encrypted: Vec<Vec<u8>> = plain
            .iter()
            .enumerate()
            .map(|(i, p)| encrypt_pkcs7(p, &key, &compute_default_iv(5, i)))
            .collect::<Option<_>>()
            .ok_or("encrypt failed")?;
        let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:5\n\
                        #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n\
                        #EXTINF:2,\ns0.ts\n#EXTINF:2,\ns1.ts\n#EXTINF:2,\ns2.ts\n#EXT-X-ENDLIST\n";
        let (address, hits) = static_server(vec![
            ("/v.m3u8", playlist.as_bytes().to_vec()),
            ("/key.bin", key.to_vec()),
            ("/s0.ts", encrypted[0].clone()),
            ("/s1.ts", encrypted[1].clone()),
            ("/s2.ts", encrypted[2].clone()),
        ])
        .await?;
        let dir = scratch_dir("filtered_aes");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let url = format!("http://{address}/v.m3u8");
        let dir_text = dir.to_string_lossy().to_string();
        db.insert_task("t-aes", &url, "v.ts", &dir_text, 0, 0, "", "", "", 0)
            .await?;
        let params = vod_params(
            "t-aes",
            &url,
            &dir,
            &db,
            scripted(InterceptStage::Segments, r#"{"drop":[6]}"#),
        )?;

        let total = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_hls_download_inner(&params),
        )
        .await??;

        let task = db
            .load_task_by_id("t-aes")
            .await?
            .ok_or("task row missing")?;
        assert_eq!(std::fs::read(dir.join(&task.file_name))?, b"AAAACCCC");
        assert_eq!(total, 8);
        assert!(
            !hits
                .lock()
                .map_err(|e| e.to_string())?
                .iter()
                .any(|path| path == "/s1.ts"),
            "dropped segment must never be requested"
        );
        assert!(
            db.get_config("hls_filter_t-aes")
                .await?
                .is_some_and(|fp| !fp.is_empty()),
            "dropping segments must persist a fingerprint"
        );
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// 队首分片卡住时,其余连接继续下载后续分片(不再整批等队首写盘);测速用的接收
    /// 字节随网络增长,落盘字节停在 0;放行后按序写成唯一一份最终文件。
    #[tokio::test]
    async fn slow_head_segment_keeps_other_connections_busy_and_speed_visible()
    -> Result<(), Box<dyn std::error::Error>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const SEGMENTS: usize = 12;
        let bodies: Vec<Vec<u8>> = (0..SEGMENTS)
            .map(|i| format!("S{i:02}").repeat(4096).into_bytes())
            .collect();
        let playlist: String = std::iter::once(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n".to_owned(),
        )
        .chain((0..SEGMENTS).map(|i| format!("#EXTINF:2,\ns{i}.ts\n")))
        .chain(std::iter::once("#EXT-X-ENDLIST\n".to_owned()))
        .collect();
        let head_gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let requested = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        {
            let bodies = std::sync::Arc::new(bodies.clone());
            let playlist = std::sync::Arc::new(playlist);
            let head_gate = std::sync::Arc::clone(&head_gate);
            let requested = std::sync::Arc::clone(&requested);
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let bodies = std::sync::Arc::clone(&bodies);
                    let playlist = std::sync::Arc::clone(&playlist);
                    let head_gate = std::sync::Arc::clone(&head_gate);
                    let requested = std::sync::Arc::clone(&requested);
                    tokio::spawn(async move {
                        let mut buf = [0u8; 2048];
                        let Ok(n) = socket.read(&mut buf).await else {
                            return;
                        };
                        let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                        let path = request
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or_default()
                            .to_owned();
                        if let Ok(mut seen) = requested.lock() {
                            seen.push(path.clone());
                        }
                        let segment = path
                            .strip_prefix("/s")
                            .and_then(|rest| rest.strip_suffix(".ts"))
                            .and_then(|index| index.parse::<usize>().ok())
                            .and_then(|index| bodies.get(index));
                        let (status, body) = match (path.as_str(), segment) {
                            ("/v.m3u8", _) => ("200 OK", playlist.as_bytes().to_vec()),
                            (_, Some(body)) => ("200 OK", body.clone()),
                            _ => ("404 Not Found", Vec::new()),
                        };
                        // 队首分片是慢连接:先给响应头和半个 body,其余等放行。
                        let split = if path == "/s0.ts" {
                            body.len() / 2
                        } else {
                            body.len()
                        };
                        let served = async {
                            socket
                                .write_all(
                                    format!(
                                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        body.len()
                                    )
                                    .as_bytes(),
                                )
                                .await?;
                            socket.write_all(&body[..split]).await?;
                            if split < body.len() {
                                let _released =
                                    head_gate.acquire().await.map_err(std::io::Error::other)?;
                                socket.write_all(&body[split..]).await?;
                            }
                            Ok::<(), std::io::Error>(())
                        }
                        .await;
                        if let Err(error) = served {
                            tracing::debug!(%error, "HLS fixture client disconnected");
                        }
                    });
                }
            });
        }

        let samples = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(i64, Option<i64>)>::new()));
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(64);
        {
            let samples = std::sync::Arc::clone(&samples);
            tokio::spawn(async move {
                while let Some(update) = progress_rx.recv().await {
                    let update: crate::downloader::ProgressUpdate = update;
                    if update.status == 1
                        && let Ok(mut seen) = samples.lock()
                    {
                        seen.push((update.downloaded_bytes, update.transfer_bytes));
                    }
                }
            });
        }
        let dir = scratch_dir("slow_head");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let url = format!("http://{address}/v.m3u8");
        let dir_text = dir.to_string_lossy().to_string();
        db.insert_task("t-slow", &url, "v.ts", &dir_text, 0, 0, "", "", "", 0)
            .await?;
        let mut params = vod_params("t-slow", &url, &dir, &db, crate::intercept::noop())?;
        // 两条连接:旧调度下队首卡住时最多只能再下一个分片。
        params.segment_count = 2;
        params.progress_tx = progress_tx;
        let download = tokio::spawn(async move { super::run_hls_download_inner(&params).await });

        let later: Vec<String> = (1..SEGMENTS).map(|i| format!("/s{i}.ts")).collect();
        let all_later_requested = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !requested
                .lock()
                .is_ok_and(|seen| later.iter().all(|path| seen.contains(path)))
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        let received_behind_head: i64 =
            bodies[1..].iter().map(|b| b.len() as i64).sum::<i64>() + (bodies[0].len() / 2) as i64;
        let blocked_sample = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let hit = samples.lock().ok().and_then(|seen| {
                    seen.iter()
                        .copied()
                        .find(|(_, received)| received.is_some_and(|r| r >= received_behind_head))
                });
                if let Some(sample) = hit {
                    return sample;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .ok();
        head_gate.add_permits(1);
        let total = tokio::time::timeout(std::time::Duration::from_secs(30), download).await???;

        assert!(
            all_later_requested,
            "segments behind a stalled head must keep downloading: {requested:?}"
        );
        let (written, received) = blocked_sample.ok_or("speed never saw the received bytes")?;
        assert_eq!(written, 0, "nothing can be written before the head segment");
        assert!(received.is_some_and(|r| r >= received_behind_head));
        let expected = bodies.concat();
        assert_eq!(total, expected.len() as i64);
        let task = db
            .load_task_by_id("t-slow")
            .await?
            .ok_or("task row missing")?;
        assert_eq!(std::fs::read(dir.join(&task.file_name))?, expected);
        let remaining: Vec<std::ffi::OsString> = std::fs::read_dir(&dir)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<_, _>>()?;
        assert_eq!(
            remaining,
            [std::ffi::OsString::from(&task.file_name)],
            "only the final file may remain"
        );
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// 点播进度:在途分段的响应头长度即计入总量估算(未落盘完永不满格),全部落盘后先发
    /// 一帧收敛到实际字节的 100%(带文件名越过 manager 节流),库内总量终值 = 产物实际大小。
    #[tokio::test]
    async fn vod_progress_estimates_total_and_converges_to_actual_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        let bodies: [Vec<u8>; 3] = [vec![b'A'; 65_536], vec![b'B'; 65_536], vec![b'C'; 65_536]];
        let actual: i64 = bodies.iter().map(|b| b.len() as i64).sum();
        let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n\
                        #EXTINF:4,\ns0.ts\n#EXTINF:4,\ns1.ts\n#EXTINF:4,\ns2.ts\n#EXT-X-ENDLIST\n";
        let [s0, s1, s2] = bodies;
        let (address, _hits) = static_server(vec![
            ("/v.m3u8", playlist.as_bytes().to_vec()),
            ("/s0.ts", s0),
            ("/s1.ts", s1),
            ("/s2.ts", s2),
        ])
        .await?;
        let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(i64, i64, String)>::new()));
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(256);
        let collector = {
            let frames = std::sync::Arc::clone(&frames);
            tokio::spawn(async move {
                while let Some(update) = progress_rx.recv().await {
                    let update: crate::downloader::ProgressUpdate = update;
                    if update.status == 1
                        && let Ok(mut seen) = frames.lock()
                    {
                        seen.push((
                            update.downloaded_bytes,
                            update.total_bytes,
                            update.file_name,
                        ));
                    }
                }
            })
        };
        let dir = scratch_dir("vod_progress");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let url = format!("http://{address}/v.m3u8");
        let dir_text = dir.to_string_lossy().to_string();
        db.insert_task("t-prog", &url, "v.ts", &dir_text, 0, 0, "", "", "", 0)
            .await?;
        let mut params = vod_params("t-prog", &url, &dir, &db, crate::intercept::noop())?;
        // 单连接 + 64 KiB/s:每段 body 至少传 0.75s,采样器必然报出「段在途、尚未落盘」的帧。
        params.segment_count = 1;
        params.speed_limiter = crate::speed_limiter::SpeedLimiter::new(65_536);
        params.speed_limiter.spawn_refill_task();
        params.progress_tx = progress_tx;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_hls_download_inner(&params),
        )
        .await??;
        drop(params);
        tokio::time::timeout(std::time::Duration::from_secs(5), collector).await??;

        let frames = frames.lock().map_err(|e| e.to_string())?.clone();
        for (written, total, _) in &frames {
            assert!(
                *total == 0 || *written <= *total,
                "progress past 100%: {written}/{total}"
            );
            if *written < actual {
                assert!(
                    *total == 0 || *total > *written,
                    "100% shown before every segment is written: {written}/{total}"
                );
            }
        }
        assert!(
            frames
                .iter()
                .any(|(written, total, _)| *written == 0 && *total == actual),
            "the in-flight head segment's Content-Length must feed the estimate before any write: {frames:?}"
        );
        assert!(
            frames
                .iter()
                .any(|(written, total, _)| *written == 65_536 && *total == actual),
            "after the first write the estimate must stay on the real total: {frames:?}"
        );
        assert!(
            frames
                .iter()
                .any(|(written, total, name)| *written == actual
                    && *total == actual
                    && !name.is_empty()),
            "all segments written must report exactly 100% with a forwarded frame: {frames:?}"
        );
        let (last_written, last_total, _) = frames.last().ok_or("no downloading frame")?;
        assert_eq!(last_written, last_total, "must not end below 100%");

        let task = db
            .load_task_by_id("t-prog")
            .await?
            .ok_or("task row missing")?;
        let on_disk = std::fs::metadata(dir.join(&task.file_name))?.len() as i64;
        assert_eq!(result, on_disk);
        assert_eq!(
            task.total_bytes, on_disk,
            "stored total must be the real artifact size, never the estimate"
        );
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn filter_fingerprint_change_abandons_resume() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let key = "hls_filter_t-fp";
        // 首次运行:写入指纹,但不是续传,不放弃。
        assert!(!super::sync_filter_fingerprint(&db, key, false, "aaa").await?);
        assert_eq!(db.get_config(key).await?.as_deref(), Some("aaa"));
        // 续传且指纹不变:沿用进度。
        assert!(!super::sync_filter_fingerprint(&db, key, true, "aaa").await?);
        // 续传且指纹变化:放弃续传并更新。
        assert!(super::sync_filter_fingerprint(&db, key, true, "bbb").await?);
        assert_eq!(db.get_config(key).await?.as_deref(), Some("bbb"));
        // 续传时插件不再丢弃任何分片:放弃续传并清掉指纹。
        assert!(super::sync_filter_fingerprint(&db, key, true, "").await?);
        assert_eq!(db.get_config(key).await?, None);
        // 从未过滤过且仍不过滤:无变化。
        assert!(!super::sync_filter_fingerprint(&db, key, true, "").await?);

        // 指纹 = hex(sha256(被丢弃序号以 "," 连接)),与具体插件无关。
        let segments: Vec<super::HlsSegment> = (3..6)
            .map(|n| super::HlsSegment {
                uri: format!("http://127.0.0.1/s{n}.ts"),
                duration: 1.0,
                key: None,
                byte_range: None,
                bitrate_kbps: None,
                discontinuity: false,
                map: None,
                sequence: n,
            })
            .collect();
        let dir = scratch_dir("filter_fp");
        let params = vod_params(
            "t-fp",
            "http://127.0.0.1/v.m3u8",
            &dir,
            &db,
            scripted(InterceptStage::Segments, r#"{"drop":[3,5]}"#),
        )?;
        let task = live_task_ctx();
        let notes = crate::intercept::ActivityNotes::new(
            db.clone(),
            std::sync::Arc::new(crate::NoopSink),
            "t-fp".to_owned(),
        );
        let (kept, fingerprint) = super::filter_vod_track(
            &params,
            &task,
            &notes,
            super::SegmentTrack::Video,
            "http://127.0.0.1/v.m3u8",
            b"",
            segments,
            3,
            1,
        )
        .await?;
        assert_eq!(kept.iter().map(|s| s.sequence).collect::<Vec<_>>(), [4]);
        assert_eq!(
            fingerprint,
            hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"3,5"))
        );
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn vod_segment_rewrite_fetches_signed_uris() -> Result<(), Box<dyn std::error::Error>> {
        // 插件把每片改写为带签名的地址:服务端只认签名地址,原地址一次都不会被请求。
        let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n\
                        #EXTINF:2,\ns0.ts\n#EXTINF:2,\ns1.ts\n#EXTINF:2,\ns2.ts\n#EXT-X-ENDLIST\n";
        let (address, hits) = static_server(vec![
            ("/v.m3u8", playlist.as_bytes().to_vec()),
            ("/signed/s0.ts?sig=A", b"AAAA".to_vec()),
            ("/signed/s1.ts?sig=A", b"BBBB".to_vec()),
            ("/signed/s2.ts?sig=A", b"CCCC".to_vec()),
        ])
        .await?;
        let dir = scratch_dir("vod_rewrite");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let url = format!("http://{address}/v.m3u8");
        let dir_text = dir.to_string_lossy().to_string();
        db.insert_task("t-rw", &url, "v.ts", &dir_text, 0, 0, "", "", "", 0)
            .await?;
        let rewrite = format!(
            r#"{{"rewrite":[{{"id":0,"uri":"http://{address}/signed/s0.ts?sig=A"}},{{"id":1,"uri":"http://{address}/signed/s1.ts?sig=A"}},{{"id":2,"uri":"http://{address}/signed/s2.ts?sig=A"}}]}}"#
        );
        let params = vod_params(
            "t-rw",
            &url,
            &dir,
            &db,
            scripted(InterceptStage::Segments, &rewrite),
        )?;

        let total = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_hls_download_inner(&params),
        )
        .await??;

        let task = db
            .load_task_by_id("t-rw")
            .await?
            .ok_or("task row missing")?;
        assert_eq!(std::fs::read(dir.join(&task.file_name))?, b"AAAABBBBCCCC");
        assert_eq!(total, 12);
        let requested = hits.lock().map_err(|e| e.to_string())?.clone();
        for original in ["/s0.ts", "/s1.ts", "/s2.ts"] {
            assert!(
                !requested.iter().any(|path| path == original),
                "original address {original} must never be requested: {requested:?}"
            );
        }
        // 只改写不丢弃时没有指纹:逐片签名每次运行都不同,不能影响续传判定。
        assert!(
            db.get_config("hls_filter_t-rw")
                .await?
                .is_none_or(|fp| fp.is_empty()),
            "rewrite alone must not persist a filter fingerprint"
        );
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn rewrite_signature_change_keeps_fingerprint_and_resume()
    -> Result<(), Box<dyn std::error::Error>> {
        // 同样的丢弃集合、不同的逐片签名:指纹相同,续传不被放弃;保留分片带着本次的改写地址。
        let segments = || -> Vec<super::HlsSegment> {
            (3..6)
                .map(|n| super::HlsSegment {
                    uri: format!("http://127.0.0.1/s{n}.ts"),
                    duration: 1.0,
                    key: None,
                    byte_range: None,
                    bitrate_kbps: None,
                    discontinuity: false,
                    map: None,
                    sequence: n,
                })
                .collect()
        };
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let dir = scratch_dir("rewrite_fp");
        let task = live_task_ctx();
        let notes = crate::intercept::ActivityNotes::new(
            db.clone(),
            std::sync::Arc::new(crate::NoopSink),
            "t-rwfp".to_owned(),
        );
        let mut outcomes = Vec::new();
        for signature in ["A", "B"] {
            let reply = format!(
                r#"{{"drop":[4],"rewrite":[{{"id":3,"uri":"http://127.0.0.1:9/signed/s3.ts?sig={signature}"}},{{"id":5,"uri":"http://127.0.0.1:9/signed/s5.ts?sig={signature}"}}]}}"#
            );
            let params = vod_params(
                "t-rwfp",
                "http://127.0.0.1/v.m3u8",
                &dir,
                &db,
                scripted(InterceptStage::Segments, &reply),
            )?;
            outcomes.push(
                super::filter_vod_track(
                    &params,
                    &task,
                    &notes,
                    super::SegmentTrack::Video,
                    "http://127.0.0.1/v.m3u8",
                    b"",
                    segments(),
                    3,
                    1,
                )
                .await?,
            );
        }
        let (kept_a, fp_a) = &outcomes[0];
        let (kept_b, fp_b) = &outcomes[1];
        assert_eq!(fp_a, fp_b);
        assert_eq!(
            *fp_a,
            hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"4"))
        );
        assert_eq!(
            kept_a.iter().map(|s| s.uri.as_str()).collect::<Vec<_>>(),
            [
                "http://127.0.0.1:9/signed/s3.ts?sig=A",
                "http://127.0.0.1:9/signed/s5.ts?sig=A"
            ]
        );
        assert_eq!(
            kept_b.iter().map(|s| s.uri.as_str()).collect::<Vec<_>>(),
            [
                "http://127.0.0.1:9/signed/s3.ts?sig=B",
                "http://127.0.0.1:9/signed/s5.ts?sig=B"
            ]
        );
        // 首跑落指纹,续传时(换了签名)指纹一致 → 不放弃续传。
        let key = "hls_filter_t-rwfp";
        assert!(!super::sync_filter_fingerprint(&db, key, false, fp_a).await?);
        assert!(!super::sync_filter_fingerprint(&db, key, true, fp_b).await?);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// 播放列表不设防;`.ts` 分片只接受 `X-Token: new`,否则 403。记录 (路径, 请求携带的令牌)。
    async fn token_gated_server(
        playlist: &'static str,
        segments: Vec<(&'static str, Vec<u8>)>,
    ) -> Result<
        (
            std::net::SocketAddr,
            std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
        ),
        std::io::Error,
    > {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let served = std::sync::Arc::clone(&hits);
        let segments = std::sync::Arc::new(segments);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let segments = std::sync::Arc::clone(&segments);
                let served = std::sync::Arc::clone(&served);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = request
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let token = request
                        .lines()
                        .skip(1)
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("x-token")
                                .then(|| value.trim().to_owned())
                        })
                        .unwrap_or_default();
                    if let Ok(mut hits) = served.lock() {
                        hits.push((path.clone(), token.clone()));
                    }
                    let (status, body): (&str, Vec<u8>) = if path == "/v.m3u8" {
                        ("200 OK", playlist.as_bytes().to_vec())
                    } else if token != "new" {
                        ("403 Forbidden", Vec::new())
                    } else {
                        match segments.iter().find(|(route, _)| *route == path) {
                            Some((_, body)) => ("200 OK", body.clone()),
                            None => ("404 Not Found", Vec::new()),
                        }
                    };
                    let mut response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    response.extend(body);
                    if let Err(error) = socket.write_all(&response).await {
                        tracing::debug!(%error, "HLS fixture client disconnected");
                    }
                });
            }
        });
        Ok((address, hits))
    }

    const GATED_PLAYLIST: &str =
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\ns0.ts\n#EXTINF:2,\ns1.ts\n#EXT-X-ENDLIST\n";

    /// 带旧令牌的任务参数 + 只声明 `refresh` 阶段的插件(`reply` 为其返回值)。
    fn token_params(
        task_id: &str,
        url: &str,
        dir: &std::path::Path,
        db: &crate::db::Db,
        reply: Option<&str>,
    ) -> Result<
        (
            crate::downloader::DownloadParams,
            std::sync::Arc<ScriptedInterceptor>,
        ),
        Box<dyn std::error::Error>,
    > {
        let mut script =
            ScriptedInterceptor::new(vec![entry("p@a", &[InterceptStage::Refresh], false)]);
        if let Some(reply) = reply {
            script = script.reply("p@a", InterceptStage::Refresh, Ok(Some(reply)));
        }
        let interceptor = std::sync::Arc::new(script);
        let mut params = vod_params(task_id, url, dir, db, interceptor.clone())?;
        // 单并发让请求顺序确定。
        params.segment_count = 1;
        params.spec = crate::downloader::RequestSpec {
            extra_headers: std::collections::HashMap::from([(
                "X-Token".to_owned(),
                "old".to_owned(),
            )]),
            ..Default::default()
        };
        Ok((params, interceptor))
    }

    fn refresh_calls(
        interceptor: &ScriptedInterceptor,
    ) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
        let seen = interceptor.seen.lock().map_err(|e| e.to_string())?;
        Ok(seen
            .iter()
            .filter(|(_, stage, _)| *stage == InterceptStage::Refresh)
            .map(|(_, _, ctx)| ctx.clone())
            .collect())
    }

    #[tokio::test]
    async fn segment_rejection_refreshes_credentials_once() -> Result<(), Box<dyn std::error::Error>>
    {
        let (address, hits) = token_gated_server(
            GATED_PLAYLIST,
            vec![("/s0.ts", b"AAAA".to_vec()), ("/s1.ts", b"BBBB".to_vec())],
        )
        .await?;
        let dir = scratch_dir("refresh_ok");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let url = format!("http://{address}/v.m3u8");
        let dir_text = dir.to_string_lossy().to_string();
        db.insert_task("t-rf", &url, "v.ts", &dir_text, 0, 0, "", "", "", 0)
            .await?;
        let (params, interceptor) = token_params(
            "t-rf",
            &url,
            &dir,
            &db,
            Some(r#"{"setHeaders":{"X-Token":"new"}}"#),
        )?;

        let total = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_hls_download_inner(&params),
        )
        .await??;

        let task = db
            .load_task_by_id("t-rf")
            .await?
            .ok_or("task row missing")?;
        assert_eq!(std::fs::read(dir.join(&task.file_name))?, b"AAAABBBB");
        assert_eq!(total, 8);
        // s0 带旧令牌被拒 → 刷新 → 仅重发一次;刷新后的身份对 s1 立即生效,不再被拒。
        let requested = hits.lock().map_err(|e| e.to_string())?.clone();
        let segment_requests: Vec<(&str, &str)> = requested
            .iter()
            .filter(|(path, _)| path.ends_with(".ts"))
            .map(|(path, token)| (path.as_str(), token.as_str()))
            .collect();
        assert_eq!(
            segment_requests,
            [("/s0.ts", "old"), ("/s0.ts", "new"), ("/s1.ts", "new")]
        );
        let calls = refresh_calls(&interceptor)?;
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["status"], 403);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn segment_rejection_without_refresh_result_fails_as_before()
    -> Result<(), Box<dyn std::error::Error>> {
        // 插件不换发凭据:不多发任何请求,段按原有重试预算失败,插件只被问一次。
        let (address, hits) = token_gated_server(
            GATED_PLAYLIST,
            vec![("/s0.ts", b"AAAA".to_vec()), ("/s1.ts", b"BBBB".to_vec())],
        )
        .await?;
        let dir = scratch_dir("refresh_declined");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let url = format!("http://{address}/v.m3u8");
        let dir_text = dir.to_string_lossy().to_string();
        db.insert_task("t-rd", &url, "v.ts", &dir_text, 0, 0, "", "", "", 0)
            .await?;
        let (params, interceptor) = token_params("t-rd", &url, &dir, &db, None)?;

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            super::run_hls_download_inner(&params),
        )
        .await?;

        assert!(
            result.is_err(),
            "declined refresh must keep the task failing"
        );
        let requested = hits.lock().map_err(|e| e.to_string())?.clone();
        let s0: Vec<&str> = requested
            .iter()
            .filter(|(path, _)| path == "/s0.ts")
            .map(|(_, token)| token.as_str())
            .collect();
        assert_eq!(s0, ["old"; 3], "no extra request beyond the retry budget");
        assert_eq!(refresh_calls(&interceptor)?.len(), 1);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn live_filter_skips_dropped_sequences_and_advances_cursor()
    -> Result<(), Box<dyn std::error::Error>> {
        // 首个快照 5,6;第二次轮询 7,8 且 ENDLIST。插件在每轮都要求丢弃序号 8:第一轮里
        // 8 不在上下文中(未知 id,按 continue 策略忽略),第二轮才生效。8 是窗口末尾,
        // 游标必须越过它(next_seq = 9),检查点随之推进。
        let mut routes = live_segment_routes(5..9);
        routes.push((
            "/live/index.m3u8".to_owned(),
            vec![live_playlist_body(7, 2, true)],
        ));
        let address = sequenced_server(routes).await?;
        let dir = scratch_dir("live_filter");
        let temp = dir.join("live.ts.fdownloading");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let mut run = live_run_fixture(
            address,
            &live_playlist_text(5, 2, false),
            fresh_live_temp(&temp).await?,
            &db,
            tokio_util::sync::CancellationToken::new(),
            std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
        )
        .await?;
        run.interceptor = scripted(InterceptStage::LiveSegments, r#"{"drop":[8]}"#);

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_live_track(run),
        )
        .await??;

        let expected = concat_segments(&[5, 6, 7]);
        assert!(matches!(outcome.end, super::LiveEnd::EndList));
        assert_eq!(
            std::fs::read(&temp)?,
            expected,
            "dropped sequence 8 must not be written"
        );
        let checkpoint = db
            .get_config("hls_resume_t-live")
            .await?
            .unwrap_or_default();
        assert_eq!(checkpoint, format!("3:{}:9", expected.len()));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn live_rewrite_fetches_signed_uri_and_keeps_cursor()
    -> Result<(), Box<dyn std::error::Error>> {
        // 快照 5,6,7 且 ENDLIST。插件把序号 6 改写到签名地址:原地址 /live/s6.ts 没有路由
        // (404),只要被请求就会失败;输出仍按序号顺序,检查点游标按绝对序号推进。
        let mut routes = live_segment_routes(5..8);
        routes.retain(|(path, _)| path != "/live/s6.ts");
        routes.push(("/signed/s6.ts".to_owned(), vec![Some(live_segment_body(6))]));
        let address = sequenced_server(routes).await?;
        let dir = scratch_dir("live_rewrite");
        let temp = dir.join("live.ts.fdownloading");
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let mut run = live_run_fixture(
            address,
            &live_playlist_text(5, 3, true),
            fresh_live_temp(&temp).await?,
            &db,
            tokio_util::sync::CancellationToken::new(),
            std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
        )
        .await?;
        let reply = format!(r#"{{"rewrite":[{{"id":6,"uri":"http://{address}/signed/s6.ts"}}]}}"#);
        run.interceptor = scripted(InterceptStage::LiveSegments, &reply);

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            super::run_live_track(run),
        )
        .await??;

        let expected = concat_segments(&[5, 6, 7]);
        assert!(matches!(outcome.end, super::LiveEnd::EndList));
        assert_eq!(std::fs::read(&temp)?, expected);
        let checkpoint = db
            .get_config("hls_resume_t-live")
            .await?
            .unwrap_or_default();
        assert_eq!(checkpoint, format!("3:{}:8", expected.len()));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn segment_items_expose_tags_title_and_dates() -> Result<(), Box<dyn std::error::Error>> {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:10\n\
                    #EXTINF:2,first\ns0.ts\n\
                    #EXT-X-CUE-OUT:2\n#EXTINF:2,\nad.ts\n\
                    #EXT-X-CUE-IN\n#EXT-X-PROGRAM-DATE-TIME:2024-01-01T00:00:00.000Z\n\
                    #EXTINF:2,\ns1.ts\n#EXT-X-ENDLIST\n";
        let base = "http://127.0.0.1/v.m3u8";
        let M3u8Content::Media { segments, .. } = parse_m3u8_bytes(base, text.as_bytes())? else {
            return Err("expected media playlist".into());
        };
        let items = super::segment_items(text.as_bytes(), &segments, 0..segments.len());
        assert_eq!(
            items.iter().map(|i| i.id).collect::<Vec<_>>(),
            [10, 11, 12],
            "ids are absolute media sequences"
        );
        assert_eq!(items[0].title.as_deref(), Some("first"));
        assert_eq!(items[0].duration, Some(2.0));
        assert!(items[0].tags.is_empty());
        assert_eq!(items[1].tags, ["#EXT-X-CUE-OUT:2"]);
        assert_eq!(items[2].tags, ["#EXT-X-CUE-IN"]);
        assert_eq!(
            items[2].program_date_time.as_deref(),
            Some("2024-01-01T00:00:00+00:00")
        );
        // 只取窗口子区间时 id 仍是绝对序号。
        let tail = super::segment_items(text.as_bytes(), &segments, 1..3);
        assert_eq!(tail.iter().map(|i| i.id).collect::<Vec<_>>(), [11, 12]);
        Ok(())
    }
}
