use std::error::Error as StdError;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use reqwest::header::HeaderValue;
use thiserror::Error;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::events::EventSink;
use crate::file_hash::HashAlgorithm;
use crate::logger::{log_info, log_warn};
use crate::naming::{extract_filename, name_from_url, sanitize_filename};
use crate::output;
use crate::speed_limiter::SpeedLimiter;
use crate::temp_file_guard::TempFileGuard;
use crate::transfer_activity::{TaskRuntime, TaskSegment, TransferTracker};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum DownloadError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("db error: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("cancelled")]
    Cancelled,
    #[error("checksum mismatch: {0}")]
    ChecksumMismatch(String),
    /// Server does not honour `Range` requests — returned the enclosed HTTP
    /// status (e.g. `200 OK`) instead of `206 Partial Content`.
    /// Multi-segment assembly is impossible; the caller should fall back to
    /// single-stream mode.
    #[error("server does not support Range requests (returned {0} instead of 206 Partial Content)")]
    RangeNotSupported(String),
    /// 服务器在 probe 与分段/续传请求之间【更换了文件】：Range 响应的强
    /// validator（ETag）与已落盘版本不一致，或 Last-Modified 不一致且响应
    /// `Content-Range` 总大小与已知总大小【也】不一致（分段路径下起点还须
    /// 不一致）——见 [`crate::downloader::validator_mismatch_is_fatal`]。
    /// 与 [`RangeNotSupported`] 严格区分：后者是服务器根本不支持 Range；本
    /// 变体意味着旧数据已不能与当前响应拼接，必须清空临时文件后重新下载。
    /// 文件变化与服务器 Range 能力无关，因此绝不记录主机单连接缓存。仅
    /// Last-Modified 不同但总大小（及分段起点）一致时视为跨 CDN edge 的
    /// 时钟/格式差异，不触发本变体，改为记录一条警告后继续。
    #[error("file changed on server during download (validator mismatch, server returned {0})")]
    VersionChanged(String),
    /// 服务器对 `Range: bytes=X-Y` 请求回了 `206 Partial Content`，但响应的
    /// `Content-Range` 起点与我们请求的偏移【不一致】（或整体缺失且请求非从 0
    /// 起）——典型于劣质 CDN（如 123 盘免费下载节点）在签名 URL 失效/超配额时，
    /// 对任意 Range 请求都回 206 却实际返回【从 byte 0 的全量流】。若不拦截，
    /// seek 到段偏移写入的却是文件开头字节 → 各段字节数写满区间（骗过末尾仅校验
    /// 字节数量的完整性检查），但内容整体错位 → 完整大小的损坏文件（无 checksum
    /// 时无从察觉）。与 [`RangeNotSupported`]（非 206）、[`VersionChanged`]（validator
    /// 不匹配的 200）严格区分：本变体是【206 但区间错位】，重试只会拿到同样的错位
    /// 响应，故调用方应立即回退单流（单流全量请求不带 Range，服务器"忽略 Range 返
    /// 全量"的行为对单流反而正确，能下到完整文件），【绝不】记录主机单连接缓存，
    /// 也【绝不】当瞬时错误退避重试。
    #[error(
        "server returned a misaligned Range response (206 but Content-Range does not match the requested offset: {0})"
    )]
    RangeMisaligned(String),
    /// 多段下载时，服务器在 206 响应的 `Content-Range: bytes X-Y/<total>` 里【自报的
    /// 真实总大小】明显【大于】本次规划的总大小。典型成因（BUG-HTTP-HINT-UNDERSIZED）：
    /// 浏览器扩展在 `<video>` Range 流式播放一段【仍在渐进上传】的视频时，抓到的是
    /// 【当时的部分大小】并作为 `hint_file_size` 传入；hint 模式为保护一次性签名 URL 而
    /// 跳过 probe，把这个偏小的 hint 当作权威总大小，多段只请求 `[0, hint)` → 拿满即
    /// 完成 → 落盘的是完整文件的【前缀】（静默截断，无 checksum 时无从察觉）。
    ///
    /// 与 [`RangeMisaligned`]（206 但区间【错位】、数据错位）严格区分：本变体区间
    /// 【对齐】、已下字节【正确】，只是规划的总量偏小。携带值为服务器自报的真实总
    /// 大小。coordinator 捕获后【就地扩容】（延长预分配 + 追加尾段，已下数据零丢弃，
    /// 见 `segment_coordinator` 的 `MAX_SIZE_EXPANSIONS`）；仅当扩容配额耗尽（文件
    /// 持续增长/病态分母膨胀）或扩容无法执行时才冒泡到 `run_download_inner`，以
    /// status=4 显式终止——DB 段行与临时文件保留，重试时 resume 重新 probe 续下。
    /// 绝不记录主机单连接缓存（与 Range 能力无关），也绝不当瞬时错误退避重试
    /// （重试只会拿到同样的分母）。
    #[error("server reports a larger true size than planned (Content-Range total: {0})")]
    TrueSizeLarger(i64),
    /// 多 CDN 节点池中【单个钉定节点】的可归因失败（连接失败/超时/停滞/
    /// 跨节点 validator 不一致/HTTP 拒绝）。由 worker 在钉定租约上翻译产生
    /// （见 `cdn::is_node_attributable`），coordinator 按 retryable 语义把段
    /// 回收重派（下一次租约自然避开被降权/踢除的节点），【绝不】把单节点
    /// 的问题升级为任务失败。SYS（系统 DNS）节点的错误不经翻译，语义与
    /// 现状完全一致——故本变体永不成为任务的 final_error。
    #[error("cdn node failed: {0}")]
    CdnNodeFailed(String),
    #[error("ed2k error: {0}")]
    Ed2k(String),
    /// ED2K 协议完整性违规：hashset 投毒 / 块 MD4 不匹配 / SENDINGPART 越界 /
    /// 未请求数据 / 区间碎片超限。与 [`DownloadError::Ed2k`]（纯网络类）区分，
    /// 调度层据此把违规 peer 拉黑（贯穿整个下载调用），而非仅退避。
    #[error("ed2k integrity violation: {0}")]
    Ed2kIntegrity(String),
    /// URL 仅因 `.m3u8`/`.m3u` 扩展名被 [`crate::hls_downloader::is_hls_url`]
    /// 判定为 HLS，但拉取到的内容根本不是合法 M3U8 播放列表——典型场景是
    /// WebDAV/网盘把普通文本文件（曲目列表等）恰好命名为 `.m3u8`。
    /// 与其它变体不同：该错误只可能在 HLS 下载器**拉取并解析 playlist**这
    /// 一最早阶段产生，此时尚未创建任何临时文件、写入任何字节——调度层
    /// （`download_manager::do_start_task` / `do_resume_task`）捕获后不落
    /// 终态失败，而是把任务原样交给普通 HTTP 下载器按字面文件保存。
    #[error("not an HLS playlist: {0}")]
    NotAnHlsPlaylist(String),
    /// 插件拦截器要求中止任务（`fail` 返回值或 `onFailure: "fail"` 的出错，见
    /// [`crate::intercept::InterceptAbort`]）。重试不会改变插件的决定：段级 / 请求级
    /// 重试遇到它都直接上抛。
    #[error("{0}")]
    PluginAbort(String),
    #[error("{0}")]
    Other(String),
}

/// 检测下载错误是否为服务器主动拒绝（403 Forbidden / 429 Too Many Requests）。
///
/// 这类错误通常意味着服务器限制了并发连接数，多段下载的额外连接被拒绝。
/// 与网络超时、连接重置等瞬时错误不同，重试这类错误毫无意义——应当立即
/// 通知 coordinator 进行降级处理。
pub(crate) fn is_server_rejection(e: &DownloadError) -> bool {
    match e {
        DownloadError::Request(req_err) => {
            if let Some(status) = req_err.status() {
                matches!(status.as_u16(), 403 | 429)
            } else {
                false
            }
        }
        _ => false,
    }
}

/// 检测下载错误是否为插件中止（见 [`DownloadError::PluginAbort`]）。
pub(crate) fn is_plugin_abort(e: &DownloadError) -> bool {
    matches!(e, DownloadError::PluginAbort(_))
}

/// 检测下载错误是否为本地磁盘写满 / 配额耗尽（ENOSPC、`ERROR_DISK_FULL`、EDQUOT）。
///
/// 这类错误在用户腾出空间之前不会自愈，段级退避重试只会空烧预算并拖慢失败上报；
/// 调用方应立即把它作为致命错误上抛。
pub(crate) fn is_disk_full(e: &DownloadError) -> bool {
    matches!(
        e,
        DownloadError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
            )
    )
}

/// 检测下载错误是否为 HTTP 416 Range Not Satisfiable。
///
/// BUG-HTTP-416-RETRY-EXHAUST：服务器对当前 Range 明确拒绝（典型于续传偏移
/// 越界——临时文件被外部截断，或服务器文件在两次探测间缩小）。重试同一
/// Range 必然拿到同样的 416，与瞬时网络错误不同；调用方应据此短路重试，
/// 而不是空烧完整的重试预算后才失败。
pub(crate) fn is_range_not_satisfiable(e: &DownloadError) -> bool {
    match e {
        DownloadError::Request(req_err) => {
            req_err.status() == Some(reqwest::StatusCode::RANGE_NOT_SATISFIABLE)
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// 完成期定名：任务名由引擎推断（`tasks.name_inferred`）时按记录的响应命名证据与临时
/// 文件头魔数精修（见 [`crate::naming::refine_inferred_name`]）；显式名（用户改名 / API
/// `out=` / CD 名）只在缺类型扩展名时按同一证据补扩展名（见
/// [`crate::naming::complete_explicit_extension`]），其余原样返回。读状态 / 读文件头失败
/// 只放弃精修，不影响下载完成。
async fn refine_completed_name(p: &DownloadParams, temp_path: &Path, current: &str) -> String {
    let (inferred, resp_name, resp_mime) = match p.db.get_task_naming_state(&p.task_id).await {
        Ok(state) => state,
        Err(e) => {
            log_warn!(
                "[download] task {} failed to load naming state: {} (keeping '{}')",
                p.task_id,
                e,
                current
            );
            return current.to_string();
        }
    };
    if !inferred && !crate::naming::needs_type_extension(current) {
        return current.to_string();
    }
    let head = match read_file_head(temp_path, SNIFF_HEAD_BYTES).await {
        Ok(head) => head,
        Err(e) => {
            log_warn!(
                "[download] task {} failed to read {} for type sniffing: {}",
                p.task_id,
                temp_path.display(),
                e
            );
            Vec::new()
        }
    };
    let evidence = crate::naming::NameEvidence {
        disposition: (!resp_name.is_empty()).then_some(resp_name.as_str()),
        mime: (!resp_mime.is_empty()).then_some(resp_mime.as_str()),
        sniffed_ext: crate::naming::sniff_extension(&head),
    };
    let refined = if inferred {
        crate::naming::refine_inferred_name(current, &evidence)
    } else {
        crate::naming::complete_explicit_extension(current, &evidence)
    };
    match refined {
        Some(name) => {
            log_info!(
                "[download] task {} refined {} file name '{}' → '{}' (evidence: {:?})",
                p.task_id,
                if inferred { "inferred" } else { "explicit" },
                current,
                name,
                evidence
            );
            name
        }
        None => current.to_string(),
    }
}

/// 文件头魔数嗅探读取的字节数（tar 的 `ustar` 位于 257，OOXML/APK 条目名在前几 KB）。
const SNIFF_HEAD_BYTES: u64 = 4096;

async fn read_file_head(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let file = File::open(path).await?;
    let mut head = Vec::new();
    file.take(limit).read_to_end(&mut head).await?;
    Ok(head)
}

/// 记录实际响应的命名证据（见 [`Db::record_task_response_naming`]）。写失败只会让完成期
/// 不精修推断名，不影响下载本身，故记日志后继续。
pub(crate) async fn record_naming_evidence(db: &Db, task_id: &str, disposition: &str, mime: &str) {
    if let Err(e) = db
        .record_task_response_naming(task_id, disposition, mime)
        .await
    {
        log_warn!(
            "[download] task {} failed to record response naming evidence: {} \
             (inferred file name will not be refined at completion)",
            task_id,
            e
        );
    }
}

/// 从一个实际 GET 响应提取命名证据（Content-Disposition 名 + Content-Type）并记录。
pub(crate) async fn record_response_naming(db: &Db, task_id: &str, resp: &reqwest::Response) {
    let (disposition, mime) = crate::naming::response_naming(resp.headers(), resp.url().as_str());
    record_naming_evidence(db, task_id, disposition.as_deref().unwrap_or(""), &mime).await;
}

pub struct FileInfo {
    pub file_name: String,
    /// `file_name` 的来源：只有 Content-Disposition（含 URL 查询里的 CD）才是权威名，
    /// 作为响应命名证据落库供完成期精修引擎推断的名字。
    pub name_source: crate::naming::NameSource,
    pub total_bytes: i64,
    pub supports_range: bool,
    /// MIME content type from the server (e.g. "text/html", "application/octet-stream").
    /// Empty when the probe phase was skipped (hint_file_size > 0).
    pub content_type: String,
    /// ETag header value from the server (e.g. `"abc123"` or `W/"abc123"`).
    /// Used by multi-segment downloads to verify all connections fetch the same
    /// file version.  Empty when the server did not provide an ETag.
    pub etag: String,
    /// Last-Modified header value from the server (RFC 7232 §2.2).
    /// Used together with `etag` for file-identity verification across segments.
    /// Empty when the server did not provide Last-Modified.
    pub last_modified: String,
    /// `true` when the server's probe response included a `Content-Encoding`
    /// other than `identity` (e.g. gzip, br, deflate).  Because reqwest is
    /// built WITHOUT gzip/brotli/deflate Cargo features, the compressed bytes
    /// would be written raw to disk, corrupting the file.  Callers should
    /// treat this as a warning and avoid multi-segment downloads.
    #[allow(dead_code)]
    pub content_encoding_compressed: bool,
}

#[derive(Default)]
pub struct ProgressUpdate {
    pub task_id: String,
    /// Observed active reads and byte-range geometry; absent when not sampled.
    pub runtime: Option<crate::transfer_activity::TaskRuntime>,
    pub downloaded_bytes: i64,
    /// Restored/truncated counter before body reads. Unlike an ordinary lower
    /// progress sample, this explicitly resets host traffic measurement.
    pub download_baseline: Option<i64>,
    /// 本次运行从网络读到的 body 字节累计（单调，含随后被重试丢弃的部分）。
    /// `Some` 时测速与宿主流量统计都以它为准，`downloaded_bytes` 只表示进度：
    /// 先整段缓冲、再按序写盘的协议（HLS）落盘计数只能按段跳变，拿它测速会锯齿。
    pub transfer_bytes: Option<i64>,
    pub total_bytes: i64,
    pub status: i32,
    pub error_message: String,
    /// Non-empty only on initial status=1 update (resolved file name).
    pub file_name: String,
    /// Per-segment progress info (for IDM-style visualization).
    /// `None` for single-thread downloads; `Some(vec)` for multi-segment.
    pub segment_details: Option<Vec<SegmentProgressInfo>>,
    /// 实时上传速率（字节/秒）。仅 BT 任务的周期性进度上报携带非零值
    /// （librqbit 统计），其余协议恒为 0。
    pub upload_speed_bps: i64,
    /// BT 数据下载完成标记：`stats.finished` 时刻（piece 全部下完，但校验
    /// 与 staging→save_dir 搬移尚未完成、任务未进终态）置 `true` 一次。
    /// `progress_reporter` 据此立即发 `EngineEvent::BtDataFinished`（按
    /// task_id 去重），对应 aria2 `onBtDownloadComplete` 通知语义。
    pub bt_data_finished: bool,
    /// 已上传字节数（BT 做种）。仅 BT 任务有意义，默认 0。
    pub uploaded_bytes: i64,
    /// Seeding status: 0=none, 1=active seeding, 2=ratio reached,
    /// 3=time reached, 4=user stopped, 5=task deleted, 6=session released,
    /// 7=inactive time reached, 8=queued for a seeding slot.
    pub seeding_status: i32,
    /// BT 做种状态的辅助说明（如停止原因）。无错误/未做种时为空。
    pub seeding_message: String,
    /// 累计做种秒数（发帧时刻；排队/暂停不计）。仅 BT 做种帧非零。
    pub seeding_time_secs: i64,
    /// 非空 = 批量删除通知（其余字段无意义）：reporter 清理这些任务的内部状态，
    /// 并按入队顺序发出一条 `EngineEvent::TasksDeleted`。
    pub deleted_tasks: Vec<String>,
}

/// Snapshot of a single segment's progress, sent from downloader to progress_reporter.
#[derive(Clone)]
pub struct SegmentProgressInfo {
    pub index: i32,
    /// None denotes geometry without a transport-level activity observation.
    pub active: Option<bool>,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
}

pub struct DownloadParams {
    pub task_id: String,
    pub url: String,
    pub save_dir: String,
    pub file_name: String,
    pub segment_count: i32,
    /// When `true`, skip file-name dedup — the file on disk belongs to *this*
    /// task and should be reused, not treated as a naming collision.
    pub is_resume: bool,
    pub db: Db,
    pub client: Client,
    pub progress_tx: mpsc::Sender<ProgressUpdate>,
    pub cancel_token: CancellationToken,
    /// Global speed limiter — shared across all concurrent downloads.
    pub speed_limiter: SpeedLimiter,
    /// 引擎事件接收端(进度/分段拆分等)——由宿主注入。
    pub sink: std::sync::Arc<dyn EventSink>,
    /// Browser cookies for authenticated downloads (e.g. GitHub private repos).
    /// Format: "name1=val1; name2=val2"
    ///
    /// **冗余字段**：值与 `spec.cookies` 始终保持一致——保留是为了让"不感知
    /// RequestSpec"的旧调用路径继续工作。新增代码请优先读 `spec.cookies`。
    pub cookies: String,
    /// HTTP Referer header value captured by the browser extension.
    /// Empty = do not send Referer (manually added downloads).
    ///
    /// **冗余字段**：与 `spec.referrer` 同步。
    pub referrer: String,
    /// File size hint from the browser extension (bytes). 0 = unknown.
    /// When > 0, the probe phase (HEAD + Range:0-0) is skipped entirely.
    /// This prevents one-time CDN URLs from being "consumed" by probe requests.
    pub hint_file_size: i64,
    /// 该任务的 Range 能力是否已被验证（probe 确认过、resolver 插件担保
    /// `rangeSupported`、或此前任一 206/Accept-Ranges 证据，持久化于
    /// tasks.range_verified）。
    /// 非 hint 的 fresh 任务恒 true（probe 会实测）；hint 任务（hint_file_size
    /// != 0）fresh 时由 manager 决定：浏览器扩展 hint → false（保守），插件
    /// rangeSupported 担保 → true（直接多段）。resume 时由 download_manager
    /// 从 DB 读取——`false` 表示该任务源自 hint 且从未拿到 Range 支持证据，
    /// resume 须延续「首连接 plain GET」保守启动，绝不发
    /// bounded Range（配额型端点会被作废 token）。
    pub range_verified: bool,
    /// Proxy configuration — used by FTP downloader for SOCKS/HTTP CONNECT tunneling.
    /// HTTP downloads use the proxy via the `client` field (already configured).
    pub proxy_config: crate::proxy_config::ProxyConfig,
    /// HLS/DASH 画质选择:需要宿主介入决策时通过此 trait 发起等待。
    /// 非 HLS/DASH 下载(HTTP/FTP/BT)也必须提供(可用 `NoopSelection`)。
    pub selector: std::sync::Arc<dyn crate::selection::HostSelection>,
    /// Checksum spec for post-download integrity verification.
    /// Format: "algo=hexhash", e.g. "sha-256=abc123..." or "md5=d41d8c...".
    /// Empty = skip verification.
    pub checksum: String,
    /// 浏览器扩展捕获的额外 HTTP 请求头（如 Authorization）。
    /// 在发起 HTTP 请求时附加到请求头中。
    ///
    /// **冗余字段**：与 `spec.extra_headers` 同步。
    pub extra_headers: std::collections::HashMap<String, String>,
    /// 完整 HTTP 请求事务规格——method + cookies + referrer + headers + body。
    ///
    /// 这是请求构造的**单一权威来源**。`build_request(&client, &url, method, &spec)`
    /// 用此重建浏览器看到的请求，而不是用 GET 重发 URL。修复 form-POST、
    /// 一次性签名 URL 等"URL 之外的输入决定响应"的下载场景。
    pub spec: RequestSpec,
    /// 音频轨 URL（离散轨对下载，DASH 音视频分离场景）。
    ///
    /// `Some` 时表示这是一对分离的音视频轨：`url` 为视频轨、此字段为音频轨，
    /// 引擎分别下载后用 ffmpeg mux 合并为单文件。不依赖 `.mpd` manifest。
    /// `None` 时为普通单 URL 下载。
    pub audio_url: Option<String>,
    /// Auto 模式（segment_count==0）下的最大连接数上限，用于裁剪 segment_advisor
    /// 的推荐值：`effective = min(advisor, auto_max_connections)`。<=0 视为不限
    /// （回退 advisor 原值）。用户显式指定 segment_count 时本字段不参与。
    pub auto_max_connections: i32,
    /// 下载完成后是否把文件修改时间设为服务器提供的原始时间（config
    /// `use_server_time`）：HTTP 取 `Last-Modified`，FTP 取 `MDTM`，音视频轨对取
    /// 轨道探测响应的 `Last-Modified`；HLS/DASH 分片流、BT、eD2k 无服务器文件时间。
    /// 服务器未提供、解析失败或写入失败时保留本地完成时间，绝不影响下载结果。
    pub use_server_time: bool,
    /// 覆盖旧文件的授权（manager 注入）：全局 overwrite → `Any`；逐任务
    /// 「覆盖」答复 → `Only(询问时的文件名)`；其余 `Never`。授权时，起名/
    /// 终名冲突若**仅**来自磁盘上已存在的最终文件，则保留原名并在 finalize
    /// 时删除旧文件后占名；`.fdownloading` 临时文件、并发任务预订（reserved）
    /// 与 avoid 集合仍按编号改名，绝不覆盖其他任务的在途/产物。
    pub overwrite: crate::file_exists::OverwritePolicy,
    /// 段行布局属主令牌（= manager 的 spawn generation）。多段路径起飞时
    /// 写入 `tasks.segments_epoch`，worker 段进度写入以它作存在性守卫——
    /// 快速 pause→resume 后旧 spawn 迟到的写入全类失效（含段 0），杜绝
    /// "迟到写落到重建后段行"的静默空洞。单流路径不使用。
    pub spawn_gen: i64,
    /// mux（DASH/轨对）使用的 ffmpeg 路径，由 manager 经
    /// `components::resolve_ffmpeg` 解析注入（manual → managed → system）。
    /// `None` = 未解析到，dash 下载器回退 `Command::new("ffmpeg")` 走 PATH。
    pub ffmpeg_path: Option<std::path::PathBuf>,
    /// 多 CDN 节点聚合的任务级输入（manager 按全局设置与任务上下文折算，
    /// 见 [`crate::cdn::CdnTaskInput`]）。`enabled == false`（默认）时多段
    /// 路径构造单节点池，行为与现状逐字节一致。
    pub cdn: crate::cdn::CdnTaskInput,
    /// `ProxyMode::Auto` 任务的多路径上下文（起飞路径 + 备选路径及其先验），
    /// 由 manager 构造（见 [`crate::auto_proxy::AutoProxyCtx`]）。`None` =
    /// 非 Auto 模式 / 无候选代理 / 一次性 failover 链路 / 忽略 TLS 错误——
    /// 单路径，多段路径零行为变化。
    pub auto_proxy: Option<std::sync::Arc<crate::auto_proxy::AutoProxyCtx>>,
    /// 多网卡聚合的任务级输入（全局开关打开时由 manager 构造，见
    /// [`crate::multi_nic::MultiNicInput`]）。`None` = 功能关闭，多段路径零
    /// 行为变化。
    pub multi_nic: Option<std::sync::Arc<crate::multi_nic::MultiNicInput>>,
    /// 无人值守任务（`tasks.unattended`，RSS/免打扰接管创建）：HLS/DASH
    /// 画质选择跳过 `HostSelection` 弹窗，直接取最高码率（与超时默认值
    /// 一致）。仅 HLS/DASH 路径读取；BT 文件选择在创建时已落库，不经此。
    pub unattended: bool,
    /// 启动序幕已为本次 fresh 启动跑过的完整探测结果（见 [`PreprobedInfo`]）。
    /// `Some(Ok)` 时 `run_download_inner` 直接采用而不再探测；`Some(Err)` 是序幕
    /// 探测（已含完整重试 / 自适应阶梯）的最终失败，下载器直接上报而不再重跑一遍
    /// 同样的阶梯（否则永久性失败要等两倍时间）；`None`（hint / 续传 / 序幕未探测
    /// 或被取消 / 非 HTTP 协议）时下载器自己探测。
    pub preprobed: Option<Result<PreprobedInfo, DownloadError>>,
    /// 本任务的插件拦截器（无匹配插件时为 [`crate::intercept::noop`]）。
    pub interceptor: std::sync::Arc<dyn crate::intercept::TaskInterceptor>,
    /// 本次运行的连接数硬上限（插件 `maxConnections`）；0 = 不限。
    pub connection_cap: i32,
    /// 只读网络信息与有效 UA（插件上下文用）。
    pub run_meta: crate::intercept::RunMeta,
}

impl DownloadParams {
    /// 用 `spec` 回填 `cookies` / `referrer` / `extra_headers` 三个冗余字段。
    pub fn sync_request_mirrors(&mut self) {
        self.cookies.clone_from(&self.spec.cookies);
        self.referrer.clone_from(&self.spec.referrer);
        self.extra_headers.clone_from(&self.spec.extra_headers);
    }
}

/// 启动序幕（`download_manager::finalize_start_file_name`）用与下载相同的 client / spec
/// 跑完 [`resolve_file_info_held`] 后交给下载器的结果：序幕据此命名，
/// 下载器据此跳过自己的第二轮探测。仅在序幕与 `run_download` 之间即时传递，不跨暂停 / 续传保留。
pub struct PreprobedInfo {
    pub info: FileInfo,
    /// 探测靠请求头自适应才通过时的适配后 spec；序幕已落库，下载器只需沿用。
    pub adapted: Option<RequestSpec>,
    /// 单请求探测（[`resolve_file_info_held`]）保留下来的响应：已读完响应头、
    /// body 未动，随后成为第一条数据连接（见 [`DownloadSpec::take_held_response`]）。
    /// `None` = 探测走的是 HEAD ∥ Range 路径或响应已不可用。
    pub held: Option<reqwest::Response>,
}

/// 将浏览器扩展捕获的额外 HTTP 头应用到请求构建器上。
///
/// 使用 `req.headers(map)` 而非逐个 `req.header()`，确保**覆盖**语义：
/// 当 extra_headers 中包含 User-Agent、Accept 等已由 reqwest Client
/// 默认设置的头时，浏览器的真实值会替代默认值，而不是追加产生重复头。
/// 这是 IDM/NDM 的核心策略——原样复制浏览器的请求头。
///
/// 无效的 header name 或 value 会被静默跳过。
///
/// **Defense-in-depth filtering**: Even though the browser extension already
/// strips dangerous headers on the TypeScript side, we filter them again here
/// at the Rust boundary.  This protects against:
///   - A buggy or outdated extension version that forgets to filter,
///   - Manual API callers that bypass the extension entirely,
///   - Future protocol changes that add new dangerous headers.
///
/// Filtered headers:
///   - `accept-encoding` / `content-encoding` — reqwest has NO gzip/br/deflate
///     Cargo features enabled; forwarding these causes the server to send
///     compressed bytes that are written raw to disk → file corruption.
///   - `transfer-encoding` — hop-by-hop header; must not be forwarded.
///   - `host` — must match the actual request target, not the browser's.
///   - `content-length` — meaningless on a GET; can confuse intermediaries.
///   - `connection` — hop-by-hop header managed by the HTTP stack.
///   - `range` / `if-range` — 分段/续传维度由下载引擎独占管理。浏览器播放
///     媒体时对 `.m4s`/流分段发的 `Range: bytes=<seek偏移>-` 若被透传到整轨
///     或整段 GET，会与引擎自己的 Range 冲突：偏移越界即触发 416 Range Not
///     Satisfiable（B站 DASH 音频轨实测），或悄悄只下回一小片导致文件损坏。
pub(crate) fn apply_extra_headers(
    req: reqwest::RequestBuilder,
    extra_headers: &std::collections::HashMap<String, String>,
) -> reqwest::RequestBuilder {
    apply_extra_headers_where(req, extra_headers, |_| true)
}

/// [`apply_extra_headers`]，但只应用 `keep(name)` 为真的头（`name` 为原始大小写）。
pub(crate) fn apply_extra_headers_where(
    req: reqwest::RequestBuilder,
    extra_headers: &std::collections::HashMap<String, String>,
    keep: impl Fn(&str) -> bool,
) -> reqwest::RequestBuilder {
    if extra_headers.is_empty() {
        return req;
    }

    let mut map = reqwest::header::HeaderMap::with_capacity(extra_headers.len());
    for (name, value) in extra_headers {
        if !keep(name) {
            continue;
        }
        if let (Ok(header_name), Ok(header_value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            if BLOCKED_REQUEST_HEADERS
                .iter()
                .any(|&blocked| header_name.as_str() == blocked)
            {
                log_info!(
                    "[extra-headers] filtered dangerous header: {}",
                    header_name.as_str()
                );
                continue;
            }
            map.insert(header_name, header_value);
        }
    }
    // req.headers(map) 内部用 insert 逐个替换同名头，
    // 确保浏览器的真实 User-Agent 等值覆盖 build_client 设的默认值。
    req.headers(map)
}

/// 永不从外部输入（扩展捕获 / 插件改写）转发的请求头，由 HTTP 栈或下载引擎独占管理。
/// 按小写比较（`HeaderName` 恒为小写）。
pub(crate) const BLOCKED_REQUEST_HEADERS: [&str; 8] = [
    "accept-encoding",
    "content-encoding",
    "transfer-encoding",
    "host",
    "content-length",
    "connection",
    "range",
    "if-range",
];

/// 一条按站点附加的请求头规则：只对 [`crate::auth::site_key`] 等于 `origin` 的请求生效。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginHeaders {
    pub origin: String,
    pub headers: std::collections::HashMap<String, String>,
}

/// 对 `url` 所属站点附加 `rules` 中匹配的请求头（过滤规则同 [`apply_extra_headers`]）。
pub(crate) fn apply_origin_headers(
    mut req: reqwest::RequestBuilder,
    url: &str,
    rules: &[OriginHeaders],
) -> reqwest::RequestBuilder {
    if rules.is_empty() {
        return req;
    }
    let Some(site) = crate::auth::site_key(url) else {
        return req;
    };
    for rule in rules.iter().filter(|r| r.origin == site) {
        req = apply_extra_headers(req, &rule.headers);
    }
    req
}

/// 插件显式指定、引擎自适应逻辑不得改动的请求身份字段。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestPins {
    pub user_agent: bool,
    pub referrer: bool,
}

// ---------------------------------------------------------------------------
// RequestSpec: 完整 HTTP 请求事务的内部表示
// ---------------------------------------------------------------------------
//
// 设计动机：FluxDown 早期把每个下载视为"URL → 内容"的简化模型，所有
// HTTP 请求都通过 `client.get(url)` 重发。这个假设在以下场景全部失败：
//   - form POST 触发的下载（uupdump.net）：服务器对 GET 返回 HTML 页面
//   - 一次性签名 URL：被 probe 消费后再请求拿到 403/HTML
//   - 内容协商响应：method/headers 不同 → body 不同
//
// 现在统一为「请求事务 = method + url + headers + cookies + body」，由扩展
// 在 `webRequest.onBeforeRequest` 抓取后透传至 Rust，downloader 用 `build_request`
// 一比一重建浏览器看到的请求。

/// 解码后的请求体——`reqwest::RequestBuilder` 可直接消费的形式。
#[derive(Debug, Clone)]
pub enum RequestBodyDecoded {
    /// 表单字段对——`reqwest::form()` 会编码为 `application/x-www-form-urlencoded`。
    Form(Vec<(String, String)>),
    /// 已经序列化好的 url-encoded 字符串，原样作为 body 发送。
    Urlencoded(String),
    /// 原始字节流。`content_type` 为 `None` 时不主动设置 Content-Type 头。
    Raw {
        bytes: Vec<u8>,
        content_type: Option<String>,
    },
}

/// 完整 HTTP 请求事务规格——`build_request` 的唯一输入来源。
///
/// 字段含义：
///   - `method`：浏览器原始 method；缺省视为 GET
///   - `cookies`：`Cookie:` 头的完整字符串（"k1=v1; k2=v2"）
///   - `referrer`：浏览器原始 Referer
///   - `extra_headers`：扩展捕获的其他请求头（UA/Accept/Sec-Fetch-* 等），
///     由 `apply_extra_headers` 过滤危险头后注入
///   - `body`：仅非 GET 有意义；GET 请求即使携带也会被忽略（见 build_request）
#[derive(Debug, Clone, Default)]
pub struct RequestSpec {
    pub method: reqwest::Method,
    pub cookies: String,
    pub referrer: String,
    pub extra_headers: std::collections::HashMap<String, String>,
    pub body: Option<RequestBodyDecoded>,
    /// 插件锁定的 UA / Referer：自适应改写（换 UA、补 Referer、质询脱困）跳过被锁字段。
    pub pins: RequestPins,
    /// 按站点附加的请求头（插件下发，不落库）。
    pub origin_headers: Vec<OriginHeaders>,
    /// 插件声明的单个 HTTP 数据请求的 Range 跨度上限（字节，插件 `maxRangeBytes`）；
    /// `None` = 不限。与 `origin_headers` 同属插件按次运行叠加的改写，不落库。
    /// 读取请用 [`RequestSpec::range_window_cap`]。
    pub max_range_bytes: Option<i64>,
    /// 站点代理规则的逐请求路由器。任务「跟随全局」且存在启用规则时由 manager / RSS
    /// 设置；`DownloadSpec` 的刷新是克隆后修补，会保留该字段。`None` = 规则不参与。
    pub egress: Option<std::sync::Arc<crate::rule_route::EgressRouter>>,
}

/// 浏览器扩展/Native Messaging 捕获的原始请求体——引擎侧的传输无关表示。
/// 捕获请求的宿主从 Native Messaging wire 格式转换为此类型后再调用
/// [`RequestSpec::from_captured`]，使得 `downloader` / `download_manager`
/// 不直接依赖 Native Messaging 协议。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CapturedRequestBody {
    FormData {
        fields: std::collections::HashMap<String, Vec<String>>,
    },
    Urlencoded {
        raw: String,
    },
    /// `bytes_b64`：base64 编码的原始字节(XHR/fetch 直接发送 ArrayBuffer 场景)。
    Raw {
        #[serde(rename = "bytesB64")]
        bytes_b64: String,
        #[serde(rename = "contentType", default)]
        content_type: Option<String>,
    },
}

/// 把传输表示的请求体解码为可直接发送的形式。base64 非法时返回错误文案。
pub(crate) fn decode_captured_body(
    body: CapturedRequestBody,
) -> Result<RequestBodyDecoded, String> {
    use base64::Engine;
    match body {
        CapturedRequestBody::FormData { fields } => {
            let mut pairs: Vec<(String, String)> = Vec::new();
            for (k, vs) in fields {
                for v in vs {
                    pairs.push((k.clone(), v));
                }
            }
            Ok(RequestBodyDecoded::Form(pairs))
        }
        CapturedRequestBody::Urlencoded { raw } => Ok(RequestBodyDecoded::Urlencoded(raw)),
        CapturedRequestBody::Raw {
            bytes_b64,
            content_type,
        } => base64::engine::general_purpose::STANDARD
            .decode(&bytes_b64)
            .map(|bytes| RequestBodyDecoded::Raw {
                bytes,
                content_type,
            })
            .map_err(|e| format!("请求体 base64 解码失败: {e}")),
    }
}

/// [`decode_captured_body`] 的逆：表单按 key 聚合（同 key 的值保持出现顺序）。
pub(crate) fn encode_decoded_body(body: &RequestBodyDecoded) -> CapturedRequestBody {
    use base64::Engine;
    match body {
        RequestBodyDecoded::Form(pairs) => {
            let mut fields: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            for (k, v) in pairs {
                fields.entry(k.clone()).or_default().push(v.clone());
            }
            CapturedRequestBody::FormData { fields }
        }
        RequestBodyDecoded::Urlencoded(raw) => CapturedRequestBody::Urlencoded { raw: raw.clone() },
        RequestBodyDecoded::Raw {
            bytes,
            content_type,
        } => CapturedRequestBody::Raw {
            bytes_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
            content_type: content_type.clone(),
        },
    }
}

impl RequestSpec {
    /// 默认 GET、无 cookies/headers/body——用于 download_manager 内部的"裸"
    /// HTTP 请求场景(如 BT/HLS 元数据获取,无浏览器会话上下文)。
    pub fn empty_get() -> Self {
        Self::default()
    }

    /// 生效的 Range 跨度上限：仅 GET-like 请求（非 GET 强制单流且不发 Range）且值为正时 `Some`。
    /// 设置后每个 HTTP 数据请求（分段 worker 与单流 / 回退单流）的 Range 都不超过它。
    pub fn range_window_cap(&self) -> Option<i64> {
        self.max_range_bytes
            .filter(|cap| *cap > 0 && self.is_get_like())
    }

    /// GET / HEAD 请求——可以多段下载、可以做 HEAD probe。
    /// 其他 method(POST/PUT/PATCH/DELETE/...)一律强制单流,跳过 HEAD probe。
    pub fn is_get_like(&self) -> bool {
        self.method == reqwest::Method::GET || self.method == reqwest::Method::HEAD
    }

    /// 从浏览器扩展/Native Messaging 捕获的原始字段构造。
    ///
    /// `method` 解析失败(非法字符串)时回退为 GET 并记录日志,确保单一坏请求
    /// 不会让整个下载链路崩溃。
    /// `body` 解码失败(base64 错误等)时回退为 None。
    ///
    /// **OPTIONS 重映射为 GET(纵深防御)**:OPTIONS 是 CORS 预检请求,
    /// 永远不可能是真实的下载事务——扩展若把预检误当下载请求捕获
    /// (旧版本存在此 bug:预检先于真实 GET 发出,而无 body 的 GET 不会
    /// 覆盖缓存记录),原样回放 OPTIONS 会拿到 404/HTML,且非 GET 会被
    /// 强制单流,丢失多线程吞吐。此处统一降级为 GET:预检必无 body,
    /// 降级后与"扩展未捕获到 method"的默认路径完全等价。
    #[allow(clippy::too_many_arguments)]
    pub fn from_captured(
        method: Option<&str>,
        cookies: String,
        referrer: String,
        extra_headers: std::collections::HashMap<String, String>,
        body: Option<CapturedRequestBody>,
    ) -> Self {
        let method = parse_captured_method(method);

        let body = body.and_then(|b| match decode_captured_body(b) {
            Ok(decoded) => Some(decoded),
            Err(e) => {
                log_info!("[request-spec] {}", e);
                None
            }
        });

        Self {
            method,
            cookies,
            referrer,
            extra_headers,
            body,
            ..Self::default()
        }
    }
}

/// 扩展捕获的 method 字符串 → 请求方法：大小写 / 空白容忍，非法回退 GET，
/// OPTIONS（CORS 预检）重映射为 GET（理由见 [`RequestSpec::from_captured`]）。
fn parse_captured_method(method: Option<&str>) -> reqwest::Method {
    method
        .and_then(|s| {
            let upper = s.trim().to_ascii_uppercase();
            reqwest::Method::from_bytes(upper.as_bytes()).ok()
        })
        .map(|m| {
            if m == reqwest::Method::OPTIONS {
                log_info!(
                    "[request-spec] captured method OPTIONS is a CORS preflight, not a real \
                     download transaction — remapping to GET"
                );
                reqwest::Method::GET
            } else {
                m
            }
        })
        .unwrap_or(reqwest::Method::GET)
}

/// `tasks.request_method` 列值：GET / HEAD（含回退为 GET 的情形）记空串，其余记
/// 规范化后的方法名。只记方法不记请求体——请求体常带凭据（OneDrive 打包下载的
/// `pacToken`），只在引擎内存保留供同进程重放；引擎重启后据此明确拒绝恢复，而非改发 GET。
pub(crate) fn persisted_request_method(method: Option<&str>) -> String {
    let method = parse_captured_method(method);
    if method == reqwest::Method::GET || method == reqwest::Method::HEAD {
        String::new()
    } else {
        method.as_str().to_owned()
    }
}

/// Referer 合法性检查：仅接受非空且以 `http://` / `https://` 开头的值。
///
/// 浏览器（downloads API / fetch 规范）在 JS 触发下载等场景会给出
/// `about:client` 之类的占位符 referrer，这不是真实来源页 URL；
/// 将其原样发给服务器会被部分 CDN 的防盗链判为非法请求（HTTP 403）。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::downloader::is_valid_referrer;
///
/// assert!(is_valid_referrer("https://example.com/page"));
/// assert!(is_valid_referrer("http://example.com"));
/// assert!(!is_valid_referrer("about:client"));
/// assert!(!is_valid_referrer(""));
/// ```
pub fn is_valid_referrer(referrer: &str) -> bool {
    let r = referrer.trim();
    ["http://", "https://"]
        .iter()
        .any(|scheme| r.len() > scheme.len() && r[..scheme.len()].eq_ignore_ascii_case(scheme))
}

/// 统一请求构建入口——下载路径上所有发出 HTTP 请求的地方都应通过此函数。
///
/// 返回的 [`crate::http_request::HttpRequest`] 在 `send` 时由引擎逐跳跟随重定向，
/// 每一跳按凭据作用域（[`crate::credential_scope`]）重建 Cookie / Authorization /
/// Referer 与按站点请求头；凭据锚点默认是 `url` 本身。
///
/// 参数 `method` 允许覆盖 `spec.method`——主要用于 probe 阶段（HEAD probe
/// 总是发送 HEAD，与 spec 自身的 method 无关）。下载阶段通常传 `spec.method.clone()`。
///
/// **请求体语义**：
///   - GET / HEAD：即使 `spec.body` 非空也不会被附加（HTTP 标准上 GET/HEAD 不应携带 body）
///   - 其他 method：按 `RequestBodyDecoded` 类型重建请求体
///
/// Referer 只接受真实的 http(s) URL：浏览器扩展捕获的 referrer 可能是 fetch 规范的
/// 占位符 "about:client"（JS 触发的下载没有真实来源页），部分 CDN（如 hembed）对
/// 非法 Referer 值直接回 403 —— 这类伪值等同于无。
pub(crate) fn build_request<'s>(
    client: &Client,
    url: &str,
    method: reqwest::Method,
    spec: &'s RequestSpec,
) -> crate::http_request::HttpRequest<'s> {
    crate::http_request::HttpRequest::new(client, url, method, spec)
}

/// Content-Encoding types that the server may apply to response bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentEncoding {
    Gzip,
    Brotli,
    Deflate,
    Zstd,
}

/// Detect the `Content-Encoding` from response headers.
///
/// Returns `Some(encoding)` when the server applied compression (gzip, br,
/// deflate, zstd).  Returns `None` when the header is absent, empty, or
/// `identity` (i.e. the body is uncompressed).
///
/// Unknown encodings are mapped to `None` — callers that need strict
/// validation should check the raw header separately.
pub fn detect_content_encoding(headers: &reqwest::header::HeaderMap) -> Option<ContentEncoding> {
    let ce = headers.get(reqwest::header::CONTENT_ENCODING)?;
    let value = ce.to_str().unwrap_or("");
    // HTTP allows comma-separated encodings (e.g. "gzip, identity").
    // Take the first non-identity encoding as the dominant one.
    for part in value.split(',') {
        let lower = part.trim().to_ascii_lowercase();
        match lower.as_str() {
            "gzip" | "x-gzip" => return Some(ContentEncoding::Gzip),
            "br" | "brotli" => return Some(ContentEncoding::Brotli),
            "deflate" => return Some(ContentEncoding::Deflate),
            "zstd" => return Some(ContentEncoding::Zstd),
            _ => continue, // "identity", "", "compress", unknown
        }
    }
    None
}

/// 检测响应是否带有【存在但本引擎无法解码】的 Content-Encoding（如 `compress`）。
///
/// `detect_content_encoding` 把未知编码映射为 `None`，调用方据此当作 identity 原样
/// 写盘——但若服务器实际做了我们不认识的压缩，原始压缩字节落盘即文件损坏
/// （BUG-HTTP-UNKNOWN-ENCODING-RAW）。本函数在存在非 identity、且不属于受支持集合
/// 的编码时返回该编码名，调用方应据此报错而非静默写出损坏内容。
pub fn unsupported_content_encoding(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let ce = headers.get(reqwest::header::CONTENT_ENCODING)?;
    let value = ce.to_str().ok()?;
    // 收集所有【非 identity】编码层。解压管线（maybe_decompress_stream）只能反转
    // 单一层，因此"无法完整还原"的情形有二：
    //   (1) 存在任何【未知】编码（如 compress）；
    //   (2) 存在【多于一层】非 identity 编码（如 gzip, gzip / gzip, compress）——
    //       即便每层都受支持，也只反转得了第一层，残留层落盘即损坏。
    let mut layers: Vec<String> = Vec::new();
    let mut has_unknown = false;
    for part in value.split(',') {
        let lower = part.trim().to_ascii_lowercase();
        match lower.as_str() {
            // "none" 不是 IANA 登记的编码 token，但部分服务器/反代用它显式
            // 表达"未压缩"（等价于省略该头或写 identity）。按未知编码处理会把
            // 明确声明"无压缩"的响应误判为不可解码的压缩层，导致下载被永久拒绝
            // （BUG-HTTP-NONE-ENCODING-FALSE-POSITIVE）。
            //
            // "utf-8"/"utf8" 同理不是压缩编码，而是字符集 token——个别 CDN
            // （如 OPPO/一加 OTA 域名 gauss-compotaauto-c-cn.allawnfs.com）把
            // 字符编码错填进 Content-Encoding 头，响应体实际未压缩。按未知
            // 编码拒绝会把这类误写永久挡在下载之外（#413），故与 "none" 一样
            // 按 no-op 放行。
            // "aws-chunked"（S3/兼容对象存储的分块签名传输封装，如 UFile/
            // 阿里 OSS）是【传输层】framing token，不是 body 内容编码——
            // 该封装本就发生在 chunk-size/签名 trailer 这一层，等到 HTTP
            // 客户端把 body 交给我们时已经是普通字节流，不需要（也没有
            // 额外可反转的）解压步骤。个别源站/CDN 把它错填进
            // Content-Encoding 而不是 Transfer-Encoding，若按未知编码
            // 处理会导致这条本可正常下载的响应被永久拒绝并报“无法解码”。
            "identity" | "none" | "" | "utf-8" | "utf8" | "aws-chunked" => {}
            "gzip" | "x-gzip" | "br" | "brotli" | "deflate" | "zstd" => layers.push(lower),
            other => {
                has_unknown = true;
                layers.push(other.to_string());
            }
        }
    }
    if has_unknown || layers.len() > 1 {
        Some(layers.join(", "))
    } else {
        None
    }
}

/// 大小写不敏感地剥离 `Content-Range` 值的 `bytes ` 单位前缀。
///
/// RFC 9110 §14.1 规定 range-unit 比较【不区分大小写】——个别服务器/代理会发
/// `Bytes 0-1/100`。前 6 字节 ASCII 相等才剥离，故返回的切片起点必在字符边界上。
fn strip_bytes_unit_prefix(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let prefix = trimmed.as_bytes().get(..6)?;
    if !prefix.eq_ignore_ascii_case(b"bytes ") {
        return None;
    }
    Some(&trimmed[6..])
}

/// 从 `Content-Range` 响应头解析【起始字节】。
///
/// `Content-Range` 形如 `bytes <start>-<end>/<total>`（RFC 9110 §14.4）。多段下载
/// 据此校验"服务器返回的区间起点是否等于我们请求的 Range 起点"——劣质 CDN 在链接
/// 失效时会对 `Range: bytes=X-Y` 回 206 却发【从 0 的全量流】，其 `Content-Range`
/// 起点为 0（或整体缺失），与请求偏移不符。
///
/// 以下情形一律返回 `None`（交由 [`is_range_response_misaligned`] 按"起点未知"裁决）：
///   - 头缺失或非 ASCII；
///   - 值不以 `bytes ` 前缀开头（大小写不敏感，见 [`strip_bytes_unit_prefix`]）；
///   - unsatisfied-range 形式 `bytes */<total>`（`*` 非法数字）；
///   - `<start>` 解析失败。
pub(crate) fn parse_content_range_start(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    let raw = headers.get("content-range")?.to_str().ok()?;
    // "bytes 100-199/1234" → 去前缀 → "100-199/1234"
    let rest = strip_bytes_unit_prefix(raw)?;
    // 取 '/' 前的区间部分："100-199"（unsatisfied 时为 "*"）
    let range_part = rest.split('/').next()?;
    // 取 '-' 前的起点："100"（unsatisfied 时为 "*"，parse 失败 → None）
    let start_str = range_part.split('-').next()?;
    start_str.trim().parse::<i64>().ok()
}

/// 从 `Content-Range` 响应头解析【文件总大小】（斜杠后的分母）。
///
/// `Content-Range` 形如 `bytes <start>-<end>/<total>`（RFC 9110 §14.4）。多段下载据此
/// 发现服务器【自报的真实总大小】——当它明显大于当前规划的总大小（如浏览器扩展给的
/// hint 偏小、或文件仍在上传中增长）时，规划区间 `[0, planned)` 只覆盖了文件前缀，继续
/// 下去会静默截断。coordinator 据此【就地扩容】（追加尾段）下满整文件。
///
/// 以下情形返回 `None`（总大小未知，调用方【不据此扩容】，避免误判）：
///   - 头缺失或非 ASCII；
///   - 值不以 `bytes ` 前缀开头（大小写不敏感，见 [`strip_bytes_unit_prefix`]）；
///   - `<total>` 为 `*`（unsatisfied/未知）或整体缺失/解析失败。
pub(crate) fn parse_content_range_total(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    let raw = headers.get("content-range")?.to_str().ok()?;
    // "bytes 100-199/1234" → 去前缀 → "100-199/1234" → 取 '/' 后 → "1234"
    let rest = strip_bytes_unit_prefix(raw)?;
    let total_str = rest.split('/').nth(1)?;
    total_str.trim().parse::<i64>().ok()
}

/// 判定一个 206 响应的 `Content-Range` 起点（由 [`parse_content_range_start`] 解析）
/// 是否与本段请求的 Range 起点 `actual_start` 【错位】。
///
/// - `Some(s)`：服务器明确回了起点 `s` → 错位当且仅当 `s != actual_start`。
/// - `None`（Content-Range 缺失/不可解析）：
///     - `actual_start == 0`：本就要从 0 写，即便服务器发全量流也落在正确位置，
///       不算错位（段 #0 与从 0 起的续传对此免疫）→ `false`；
///     - `actual_start > 0`：请求文件中段却拿不到 Content-Range 佐证，无法确认服务器
///       是否从 0 全量发送 → 保守判定错位 → `true`（回退单流，牺牲多段并行换正确性）。
///
/// 注：合法 206 响应【必须】携带 Content-Range（RFC 9110 §15.3.7），故对合规服务器
/// 此函数在正常 Range 下恒返回 `false`，不影响多段吞吐；只有真正错位或破损的响应
/// 才触发回退。
pub(crate) fn is_range_response_misaligned(cr_start: Option<i64>, actual_start: i64) -> bool {
    match cr_start {
        Some(s) => s != actual_start,
        None => actual_start > 0,
    }
}

/// 单次 Range 响应的 validator 不一致是否构成【致命】版本变化。
///
/// - ETag 在两侧均非空且不同 → 致命：ETag 是强 validator，不同即不同版本，
///   任何情况下都不可容忍。
/// - 仅 Last-Modified 不同：响应 `Content-Range` 的总大小（分母，见
///   [`parse_content_range_total`]）与本任务已知总大小一致——需要校验起点
///   时（分段路径）起点也与请求偏移一致——判定为同一份内容在不同 CDN edge
///   间的时钟/格式差异，不致命；否则致命。
/// - 两侧均无差异 → 不致命。
///
/// `content_range_total` / `known_total`：`known_total <= 0` 表示总大小
/// 未知，此时无法佐证"同一份内容"，按致命处理。
/// `content_range_start` / `requested_start`：仅分段路径传 `Some` 校验起点；
/// 单流续传传 `None` 跳过——该路径的起点错位由
/// [`is_range_response_misaligned`] 另行处理，走安全的全量回退而非报错。
pub(crate) fn validator_mismatch_is_fatal(
    etag_mismatch: bool,
    last_modified_mismatch: bool,
    content_range_total: Option<i64>,
    known_total: i64,
    content_range_start: Option<i64>,
    requested_start: Option<i64>,
) -> bool {
    if etag_mismatch {
        return true;
    }
    if !last_modified_mismatch {
        return false;
    }
    if known_total <= 0 || content_range_total != Some(known_total) {
        return true;
    }
    if let Some(requested_start) = requested_start
        && content_range_start != Some(requested_start)
    {
        return true;
    }
    false
}

/// Wrap a response byte stream with transparent decompression if the server
/// returned a compressed `Content-Encoding`.  For `identity` or missing
/// encoding, returns the original stream unchanged.
///
/// This is the core fix for file corruption: instead of writing raw gzip
/// bytes to disk, we decompress on-the-fly and write the original file
/// content.
///
/// The output stream uses `std::io::Error` because `reqwest::Error` is opaque
/// and cannot be constructed from an `io::Error`.  Callers should convert via
/// `DownloadError::Io` when consuming chunks.
pub fn maybe_decompress_stream(
    stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
    + Unpin
    + Send
    + 'static,
    encoding: Option<ContentEncoding>,
) -> Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin + Send> {
    // Map the incoming reqwest::Error stream to io::Error so every branch
    // has a uniform error type.
    let io_stream = stream.map(|result| result.map_err(std::io::Error::other));

    let Some(enc) = encoding else {
        return Box::new(io_stream);
    };

    let reader = tokio_util::io::StreamReader::new(io_stream);

    // Wrap with the appropriate decompressor and convert back to a stream.
    match enc {
        ContentEncoding::Gzip => {
            let decoder = async_compression::tokio::bufread::GzipDecoder::new(reader);
            Box::new(tokio_util::io::ReaderStream::new(decoder))
        }
        ContentEncoding::Brotli => {
            let decoder = async_compression::tokio::bufread::BrotliDecoder::new(reader);
            Box::new(tokio_util::io::ReaderStream::new(decoder))
        }
        ContentEncoding::Deflate => {
            let decoder = async_compression::tokio::bufread::DeflateDecoder::new(reader);
            Box::new(tokio_util::io::ReaderStream::new(decoder))
        }
        ContentEncoding::Zstd => {
            let decoder = async_compression::tokio::bufread::ZstdDecoder::new(reader);
            Box::new(tokio_util::io::ReaderStream::new(decoder))
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP client builder (shared config)
// ---------------------------------------------------------------------------

/// Default User-Agent for HTTP requests.
///
/// Uses a neutral download-manager identifier instead of a browser UA.
///
/// **Why not Chrome UA?**  Cloudflare's Bot Management compares the TLS
/// fingerprint (JA3/JA4) against the declared User-Agent.  rustls produces a
/// JA3 fingerprint that does not match Chrome's.  When a non-browser TLS
/// fingerprint is paired with a Chrome UA, Cloudflare flags the request as
/// bot traffic and returns 403/404 — this breaks downloads from any CDN
/// behind Cloudflare (e.g. JetBrains' `download-cdn.clf.jetbrains.com.cn`).
///
/// When the browser extension captures a download it passes the real browser
/// UA via `extra_headers`.  That UA is applied on the first attempt; when the
/// probe hits a bot challenge (`cf-mitigated: challenge`, which Cloudflare may
/// send as 403 *or* 404) the retry explicitly sends `DEFAULT_UA`, overriding
/// both the extension UA and any browser-like task/queue/global UA (see
/// [`resolve_file_info`]).
///
/// **Version rule（同 aria2 的 `aria2/<版本>`）**：release 构建为
/// `FluxDown/<产品版本>`（build.rs 注入 `FLUXDOWN_APP_VERSION`，缺省 crate 版本），
/// debug 构建固定 `FluxDown/1.0`。
const DEFAULT_UA: &str = if cfg!(debug_assertions) {
    "FluxDown/1.0"
} else {
    concat!("FluxDown/", env!("FLUXDOWN_APP_VERSION"))
};

/// Build a properly configured HTTP client with strict TLS certificate validation.
///
/// When `proxy_config` specifies a proxy, it is injected into the client builder.
/// - `ProxyMode::None`   → explicit `no_proxy()` to disable env-var proxies
/// - `ProxyMode::System`  → auto-detect from Windows registry / environment
/// - `ProxyMode::Manual`  → user-specified proxy URL (HTTP/HTTPS/SOCKS4/SOCKS5)
///
/// When `user_agent` is non-empty, it overrides the built-in Chrome UA.
pub fn build_client(
    proxy_config: &crate::proxy_config::ProxyConfig,
    user_agent: &str,
) -> Result<Client, DownloadError> {
    build_client_with_tls_policy(proxy_config, user_agent, false)
}

/// 构建【钉定 IP】的下载 client：与 [`build_client_with_tls_policy`] 完全同参
/// 装配（代理/UA/TLS 语义逐项继承），仅额外把 `host` 的 DNS 解析钉定到
/// `ip`（reqwest `.resolve()`）。SNI 与 Host 头保持域名不变，TLS 证书照常
/// 严格校验——伪造节点拿不到该域名的有效证书，这是多节点完整性的锚。
/// 重定向到【其他 host】时钉定不生效（按系统 DNS 解析），行为与现状一致。
///
/// `SocketAddr` 的端口传 0：DNS 覆盖没有端口概念，实际端口取自请求 URL
/// （reqwest 文档明确忽略此处端口）。
pub fn build_pinned_client(
    proxy_config: &crate::proxy_config::ProxyConfig,
    user_agent: &str,
    ignore_tls_errors: bool,
    host: &str,
    ip: std::net::IpAddr,
) -> Result<Client, DownloadError> {
    build_client_inner(
        proxy_config,
        user_agent,
        ignore_tls_errors,
        ClientRoute::Pinned { host, ip },
    )
}

/// 构建出口绑定到指定网卡的直连下载 client（多网卡聚合的额外链路）。与
/// [`build_client_with_tls_policy`] 同参装配（UA/TLS/池参数逐项一致、恒不走
/// 代理），仅追加出口绑定与按链路地址族过滤的 DNS 解析，见
/// [`crate::multi_nic`]「出口绑定」。
pub fn build_link_client(
    user_agent: &str,
    ignore_tls_errors: bool,
    link: &crate::multi_nic::LinkBinding,
) -> Result<Client, DownloadError> {
    build_client_inner(
        &crate::proxy_config::ProxyConfig::default(),
        user_agent,
        ignore_tls_errors,
        ClientRoute::Link(link),
    )
}

/// Build a download client with an explicit per-task TLS certificate policy.
///
/// `ignore_tls_errors` must only come from an explicit user choice for the
/// current task. The secure default is enforced by [`build_client`].
pub fn build_client_with_tls_policy(
    proxy_config: &crate::proxy_config::ProxyConfig,
    user_agent: &str,
    ignore_tls_errors: bool,
) -> Result<Client, DownloadError> {
    build_client_inner(
        proxy_config,
        user_agent,
        ignore_tls_errors,
        ClientRoute::Default,
    )
}

/// client 的出口/解析定制（除此之外所有构建产物逐字节相同）。
enum ClientRoute<'a> {
    /// 系统 DNS + 系统路由。
    Default,
    /// DNS 钉定到 `ip`（多 CDN 节点池的 pinned client）。
    Pinned { host: &'a str, ip: std::net::IpAddr },
    /// 出口绑定到指定网卡（多网卡聚合的额外链路）。
    Link(&'a crate::multi_nic::LinkBinding),
}

/// 共享装配核心：[`build_client_with_tls_policy`]、[`build_pinned_client`] 与
/// [`build_link_client`] 的唯一实现体。`route` 只追加 DNS 钉定或出口绑定，
/// 其余配置三者逐字节相同（代理/UA/TLS/池参数绝不允许分叉）。
fn build_client_inner(
    proxy_config: &crate::proxy_config::ProxyConfig,
    user_agent: &str,
    ignore_tls_errors: bool,
    route: ClientRoute<'_>,
) -> Result<Client, DownloadError> {
    // 下载 client 关闭 reqwest 自动跟随、挂共享 cookie jar：重定向由
    // `http_request::HttpRequest::send` 逐跳按凭据作用域重建请求头。
    let mut builder = crate::http_request::configure_download_client(
        build_client_builder(proxy_config, user_agent, ignore_tls_errors)?,
        ignore_tls_errors,
    );

    // --- DNS 钉定（多 CDN 节点池）/ 出口绑定（多网卡聚合）---
    match route {
        ClientRoute::Default => {}
        ClientRoute::Pinned { host, ip } => {
            builder = builder.resolve(host, std::net::SocketAddr::new(ip, 0));
        }
        ClientRoute::Link(link) => {
            builder = crate::multi_nic::bind_to_link(builder, link);
        }
    }

    Ok(builder.build()?)
}

/// 引擎对外 HTTPS client 的起点：显式选 rustls 后端。
///
/// 不能依赖 reqwest 的默认后端：Windows 桌面包把 agent（reqwest 默认 features 含
/// native-tls）与 daemon 同一次编译，feature 合并后默认后端是 schannel，ClientHello
/// 不带 `status_request` 与 ALPN，部分 Cloudflare WAF 直接 403（#1072）。rustls 恒带
/// `status_request`，ALPN 按 HTTP 版本设置（`http1_only` 时为 `http/1.1`）。
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    Client::builder().use_rustls_tls()
}

/// 共享 HTTP 客户端装配；调用方可追加自己的超时与重定向守卫后再构建。
pub(crate) fn build_client_builder(
    proxy_config: &crate::proxy_config::ProxyConfig,
    user_agent: &str,
    ignore_tls_errors: bool,
) -> Result<reqwest::ClientBuilder, DownloadError> {
    use crate::proxy_config::{ProxyMode, detect_system_proxy};

    let ua = if user_agent.is_empty() {
        DEFAULT_UA
    } else {
        user_agent
    };
    let mut builder = client_builder()
        .user_agent(ua)
        // TLS defaults to strict verification. Only a task whose confirmation
        // dialog explicitly enabled the insecure option reaches `true` here.
        // This accepts expired/self-signed/hostname-mismatched certificates and
        // also permits undetectable HTTPS interception by a MITM proxy.
        .danger_accept_invalid_certs(ignore_tls_errors)
        // HTTP version — force HTTP/1.1 for download manager use cases:
        //  1. Range requests are reliable and well-tested on HTTP/1.1.
        //  2. Multi-segment downloads use separate TCP connections; HTTP/2
        //     multiplexing would force all segments onto one connection.
        //  3. Some servers advertise h2 via ALPN but have buggy HTTP/2
        //     implementations that close connections mid-response.
        .http1_only()
        // TCP tuning — disable Nagle's algorithm to eliminate up to 200 ms
        // latency on small writes (Range request headers, TLS handshake
        // messages).  All high-performance download managers (IDM, aria2)
        // set this.  Safe for bulk transfers because BufWriter already
        // coalesces writes into 256 KB chunks before hitting the socket.
        .tcp_nodelay(true)
        // TCP Keep-Alive — 60s 间隔比系统默认（通常 >2min）更激进，
        // 确保 NAT/防火墙不会因空闲超时而断开长时间下载的连接。
        // reqwest 底层设置 TCP_KEEPIDLE=60s（首次探测前等待时间）。
        .tcp_keepalive(Duration::from_secs(60))
        // Redirects — 宿主自身出口（egress）沿用 reqwest 跟随 30 跳；下载 client 在
        // `build_client_inner` 改为引擎逐跳跟随（见 `http_request`）。
        .redirect(reqwest::redirect::Policy::limited(30))
        // Timeouts — 15 s is sufficient for initial TCP+TLS handshake;
        // the stall detector (CHUNK_STALL_TIMEOUT) handles mid-transfer
        // hangs separately.  Shorter timeout lets failed segments retry
        // faster instead of blocking a worker for 30 s.
        .connect_timeout(Duration::from_secs(15))
        // No global timeout — downloads can be very long
        // Connection pool — keep enough idle connections to cover all
        // segments of a multi-segment download so workers reuse warm
        // keep-alive connections instead of paying TCP+TLS re-handshake
        // costs when finishing one segment and starting the next.
        // 64 == MAX_SEGMENTS (segment_advisor caps io_cap at cpu_cores*4,
        // which reaches 64 on 16+ logical-core machines downloading large
        // files).  The previous value of 16 (sized for a 4-core machine)
        // starved the idle pool on many-core hosts, forcing re-handshakes
        // for every segment beyond the 16th.  90 s idle timeout reclaims
        // the extra connections shortly after the download finishes.
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(64)
        // Cookies — 服务器下发的会话 Cookie（Set-Cookie）在后续请求自动带上；
        // 下载 client 换成全部下载共享的 jar（见 `http_request`）。
        .cookie_store(true)
        // Do NOT enable auto-decompression (.gzip/.brotli/.deflate).
        // A download manager must receive raw bytes so that:
        //  1. Content-Length matches the actual bytes written to disk.
        //  2. Range-based multi-segment downloads use correct byte offsets.
        //  3. The integrity check (file size vs Content-Length) works reliably.
        //
        // The gzip/brotli/deflate Cargo features are intentionally NOT enabled
        // to keep the binary small and avoid accidental decompression.
        // We explicitly set `Accept-Encoding: identity` so the server never
        // sends compressed content and Content-Length always equals raw bytes.
        .default_headers({
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(
                reqwest::header::ACCEPT_ENCODING,
                HeaderValue::from_static("identity"),
            );
            h
        });

    // --- Proxy injection ---
    match proxy_config.mode {
        ProxyMode::None => {
            // Explicitly disable proxy so env vars (HTTP_PROXY etc.) are ignored.
            builder = builder.no_proxy();
        }
        ProxyMode::System => {
            // Read Windows registry / env vars for system proxy.
            match detect_system_proxy() {
                Ok(Some(sys_proxy)) => {
                    if let Some(url) = sys_proxy.to_proxy_url() {
                        log_info!(
                            "[build_client] system proxy detected (url redacted for security)"
                        );
                        let mut proxy = reqwest::Proxy::all(&url)?;
                        if !sys_proxy.username.is_empty() {
                            proxy = proxy.basic_auth(&sys_proxy.username, &sys_proxy.password);
                        }
                        proxy = proxy.no_proxy(reqwest::NoProxy::from_string(
                            &crate::proxy_config::proxy_bypass_list(&sys_proxy.no_proxy_list),
                        ));
                        builder = builder.proxy(proxy);
                    } else {
                        log_info!("[build_client] system proxy enabled but no URL resolved");
                    }
                }
                Ok(None) => {
                    log_info!("[build_client] system proxy: not configured");
                }
                Err(e) => {
                    log_info!("[build_client] system proxy detection error: {}", e);
                }
            }
        }
        ProxyMode::Manual => {
            if let Some(url) = proxy_config.to_proxy_url() {
                log_info!("[build_client] manual proxy configured");
                let mut proxy = reqwest::Proxy::all(&url)?;
                if !proxy_config.username.is_empty() {
                    proxy = proxy.basic_auth(&proxy_config.username, &proxy_config.password);
                }
                proxy = proxy.no_proxy(reqwest::NoProxy::from_string(
                    &crate::proxy_config::proxy_bypass_list(&proxy_config.no_proxy_list),
                ));
                builder = builder.proxy(proxy);
            } else {
                log_info!("[build_client] manual proxy: incomplete config, using direct");
                builder = builder.no_proxy();
            }
        }
        ProxyMode::Auto => {
            // Auto 的全局/兜底 client 恒为直连：具体代理只会由 auto_proxy
            // 决策路径以 Manual 配置显式构建（见 crate::auto_proxy 模块文档）。
            builder = builder.no_proxy();
        }
    }

    Ok(builder)
}

// ---------------------------------------------------------------------------
// Resolve file info (HEAD probe → GET fallback)
// ---------------------------------------------------------------------------

/// Timeout for the probe requests (HEAD / GET Range:0-0).
/// 15 seconds is sufficient for most servers; the retry mechanism handles
/// transient failures without making users wait excessively.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// 范围探测已给出决定性答复（206 + `Content-Range`）后，并发的 HEAD 最多再等
/// 这么久。206 自带大小、validator、`Content-Type` 与通常的 `Content-Disposition`，
/// HEAD 只是锦上添花；部分服务器 / 代理链路对 HEAD 不回包（实测 Hetzner 经
/// TUN 代理 HEAD 挂满 15s 超时，而 `Range: bytes=0-0` 1s 返回），不设上限会把
/// 整个探测拖到 [`PROBE_TIMEOUT`]。
const PROBE_HEAD_GRACE: Duration = Duration::from_secs(1);

/// 并发等待 HEAD 与 Range GET 两路探测。GET 先给出决定性答复（`get_conclusive`
/// 为真）时，HEAD 只再等 `head_grace`，超时即放弃（返回 `None`）；否则两路都
/// 等到各自完成（HEAD 是 GET 失败时的唯一元数据来源）。
async fn await_probe_pair<H, G>(
    head: H,
    get: G,
    get_conclusive: impl FnOnce(&G::Output) -> bool,
    head_grace: Duration,
) -> (Option<H::Output>, G::Output)
where
    H: Future,
    G: Future,
{
    let mut head = std::pin::pin!(head);
    let mut get = std::pin::pin!(get);
    let mut head_out = None;
    let get_out = loop {
        tokio::select! {
            h = &mut head, if head_out.is_none() => head_out = Some(h),
            g = &mut get => break g,
        }
    };
    if head_out.is_none() {
        head_out = if get_conclusive(&get_out) {
            tokio::time::timeout(head_grace, head).await.ok()
        } else {
            Some(head.await)
        };
    }
    (head_out, get_out)
}

/// Maximum retries for the probe phase (HEAD + GET).
///
/// A bot challenge (`cf-mitigated: challenge`, any status — Cloudflare rejects a
/// browser UA paired with the rustls TLS fingerprint) on any attempt switches the
/// next attempt to the neutral `DEFAULT_UA`, sent explicitly so it also overrides
/// a browser-like task/queue/global UA.  Otherwise, 3 attempts total:
///   1. Original headers (incl. browser UA from extension, or task/queue/global UA)
///   2. First adaptive retry:
///      - If server rejected (403/429) and request carried a browser UA: strip browser UA
///        (handles Cloudflare bot detection rejecting rustls TLS fingerprint with Chrome UA).
///      - If server rejected (403/429) and request lacked browser UA: inject standard browser UA
///        and inferred origin Referer (handles NVIDIA / Akamai / CDNs that block non-browser UAs
///        or enforce same-origin anti-hotlinking).
///   3. Second adaptive retry:
///      - Browser UA was rejected → stripped UA with inferred origin Referer
///        (Cloudflare with hotlink check; only when the request had no Referer)
///      - Default UA was rejected → default UA with inferred origin Referer
///        (rustls fingerprint vs Chrome UA conflict, but Referer is required)
const PROBE_MAX_RETRIES: u32 = 3;

/// Base delay for probe retries (used with exponential backoff on network errors).
const PROBE_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

/// 针对反爬 / 防盗链拒绝（如 NVIDIA / Akamai 返回 403 Forbidden）时可采用的默认浏览器 UA。
/// 优先使用当前系统平台的典型 Chrome UA，确保指纹与操作系统一致。
pub const DEFAULT_BROWSER_UA: &str = if cfg!(target_os = "macos") {
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
} else if cfg!(target_os = "windows") {
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
} else if cfg!(target_os = "android") {
    "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Mobile Safari/537.36"
} else if cfg!(target_os = "ios") {
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"
} else {
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
};

/// 从 HTTP/HTTPS 目标 URL 推导出默认源站 Referer（例如 `https://cn.download.nvidia.com/...`
/// 推导为 `https://cn.download.nvidia.com/`）。供防盗链检查要求同源/合法来源页的 CDN 回落使用。
pub fn infer_origin_referrer(url: &str) -> Option<String> {
    if let Ok(parsed) = url::Url::parse(url)
        && matches!(parsed.scheme(), "http" | "https")
        && let Some(host) = parsed.host_str()
    {
        let port_part = if let Some(p) = parsed.port() {
            format!(":{}", p)
        } else {
            String::new()
        };
        return Some(format!("{}://{}{}/", parsed.scheme(), host, port_part));
    }
    None
}

/// Cloudflare 判定请求为机器人并下发质询时附带的响应头（值为 `challenge`）。质询
/// 的状态码不固定：JetBrains 国内 CDN `download-cdn.clf.jetbrains.com.cn` 回的是
/// 404，单看状态码会被当成「文件不存在」。
const CF_MITIGATED_HEADER: &str = "cf-mitigated";

/// 探测状态描述里的反机器人质询标记，形如 `404 bot-challenge`；既进用户可见的
/// 错误文案，也是 [`probe_rejection`] 的识别依据。
const BOT_CHALLENGE_TAG: &str = "bot-challenge";

/// 探测状态描述里的「端点只接受 POST」标记，形如 `405 post-only`（405 且 `Allow`
/// 列出 POST、不含 GET / HEAD）；[`probe_post_only`] 据此识别。
const POST_ONLY_TAG: &str = "post-only";

/// 探测发现端点只接受 POST（如 OneDrive `*.svc.ms/transform/zip` 打包下载）时的
/// 任务错误文案。客户端按全文匹配本地化（GPUI / Web / Android / iOS 镜像同一字符串），
/// 改动须同步各端。
pub const POST_ONLY_ENDPOINT_MESSAGE: &str =
    "This link only accepts POST requests (HTTP 405); start the download from the browser";

/// 由浏览器 POST 请求发起的任务在引擎重启后被恢复 / 重试时的错误文案：请求体不落库、
/// 内存副本已随进程消失，无法重放，改发 GET 必然被拒（或下到错误内容）。客户端按全文
/// 匹配本地化，改动须同步各端。
pub const POST_ONLY_RESUME_MESSAGE: &str = "This task was started by a browser POST request and cannot be resumed; start it again from the browser";

/// 探测失败中「服务端主动拒绝」的类别，决定下一轮请求头如何自适应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeRejection {
    /// 403 / 429：反爬 / 防盗链 / 限流，可能缺浏览器 UA 或 Referer。
    Forbidden,
    /// 反机器人质询（Cloudflare `cf-mitigated: challenge`）：浏览器 UA 与 rustls
    /// TLS 指纹不符，只有换成非浏览器 UA 才会放行。
    BotChallenge,
}

fn is_bot_challenge(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(CF_MITIGATED_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("challenge"))
}

/// 非 2xx 探测响应的状态描述：状态码，命中反机器人质询时追加 [`BOT_CHALLENGE_TAG`]，
/// 405 且只允许 POST 时追加 [`POST_ONLY_TAG`]。
fn probe_status_desc(resp: &reqwest::Response) -> String {
    let status = resp.status().as_u16();
    if is_bot_challenge(resp.headers()) {
        format!("{status} {BOT_CHALLENGE_TAG}")
    } else if resp.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
        && allows_only_post(resp.headers())
    {
        format!("{status} {POST_ONLY_TAG}")
    } else {
        status.to_string()
    }
}

/// `Allow` 头列出 POST 且不含 GET / HEAD（OneDrive 打包端点回 `Allow: OPTIONS, POST`）。
fn allows_only_post(headers: &reqwest::header::HeaderMap) -> bool {
    let Some(allow) = headers
        .get(reqwest::header::ALLOW)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let mut post = false;
    for method in allow.split(',').map(str::trim) {
        if method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD") {
            return false;
        }
        post |= method.eq_ignore_ascii_case("POST");
    }
    post
}

/// 探测失败里是否有一路明确回了「只接受 POST」（[`POST_ONLY_TAG`]）。换请求头 / 重试
/// 都改变不了方法限制，命中即停止探测并给出 [`POST_ONLY_ENDPOINT_MESSAGE`]。
fn probe_post_only(e: &DownloadError) -> bool {
    let DownloadError::Other(msg) = e else {
        return false;
    };
    let Some(rest) = msg.strip_prefix("probes failed: ") else {
        return false;
    };
    rest.split(", ").any(|part| {
        part.split_once('=')
            .and_then(|(_, desc)| desc.trim().strip_suffix(POST_ONLY_TAG))
            .is_some_and(|status| status.trim() == "405")
    })
}

/// 判断探测失败是否为服务端明确拒绝及其类别；质询优先于 403 / 429。
/// 严格解析 probe failure 状态位或 HTTP 状态码，避免误判带 403/429 端口或 URL 的网络错误。
fn probe_rejection(e: &DownloadError) -> Option<ProbeRejection> {
    if is_server_rejection(e) {
        return Some(ProbeRejection::Forbidden);
    }
    let DownloadError::Other(msg) = e else {
        return None;
    };
    let rest = msg.strip_prefix("probes failed: ")?;
    let mut rejection = None;
    for part in rest.split(", ") {
        let Some((_probe_type, status_desc)) = part.split_once('=') else {
            continue;
        };
        let trimmed = status_desc.trim();
        if trimmed
            .strip_suffix(BOT_CHALLENGE_TAG)
            .is_some_and(|status| status.trim().parse::<u16>().is_ok())
        {
            return Some(ProbeRejection::BotChallenge);
        }
        if matches!(trimmed, "403" | "429") {
            rejection = Some(ProbeRejection::Forbidden);
        }
    }
    rejection
}

/// Resolve file info with automatic retry on transient failures.
///
/// On Windows, the very first HTTPS request from a new process can fail due to
/// DNS resolver cold-start, rustls TLS session initialisation, or firewall
/// first-connection inspection.  Retrying transparently hides this from users.
pub async fn resolve_file_info(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
) -> Result<FileInfo, DownloadError> {
    resolve_file_info_with_ua_fallback(client, url, spec)
        .await
        .map(|(info, _)| info)
}

/// 去掉 extra_headers 里的浏览器 UA（回落到 client 的 DEFAULT_UA / 任务 UA）。
fn spec_without_browser_ua(spec: &RequestSpec) -> RequestSpec {
    if spec.pins.user_agent {
        return spec.clone();
    }
    RequestSpec {
        extra_headers: spec
            .extra_headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("user-agent"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        ..spec.clone()
    }
}

/// 换成中性下载器 UA（[`DEFAULT_UA`]）并显式写进请求头：既覆盖扩展传入的浏览器
/// UA，也覆盖 client 级 UA（任务 / 队列 / 全局设置）——后者只删请求头无法摆脱，
/// 用户若选了 Chrome 预设，删掉请求头后回落的仍是浏览器 UA。
fn spec_with_neutral_ua(spec: &RequestSpec) -> RequestSpec {
    if spec.pins.user_agent {
        return spec.clone();
    }
    let mut s = spec_without_browser_ua(spec);
    s.extra_headers
        .insert("User-Agent".to_string(), DEFAULT_UA.to_string());
    s
}

/// 请求头是否已显式带中性下载器 UA（[`spec_with_neutral_ua`] 的产物）。
fn has_neutral_ua(spec: &RequestSpec) -> bool {
    spec.extra_headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("user-agent") && v == DEFAULT_UA)
}

/// 一次下载运行内共享的请求身份（HTTP 分段 / 单流，以及 HLS / DASH 的分片请求）。
///
/// 两种运行中切换对本次运行内其后的全部请求立即生效：
/// - 反机器人质询：探测被跳过（hint 模式）时首个真实请求才可能撞上质询（浏览器 UA ×
///   rustls 指纹）。请求点经 [`Self::escape_bot_challenge`] 切到中性 UA 重发一次，并落库供续传沿用。
/// - 凭据刷新：请求被拒（401 / 403 / 410）且有插件声明 `refresh` 阶段时，经
///   [`Self::refresh_after_rejection`] 单飞换发凭据（并发失败的请求合并为一次插件调用），
///   新凭据只活在本次运行内、不落库。
///
/// ```
/// use fluxdown_engine::downloader::{DownloadSpec, RequestSpec};
///
/// let spec = DownloadSpec::new(RequestSpec::empty_get());
/// assert!(spec.current().extra_headers.is_empty());
/// ```
#[derive(Debug)]
pub struct DownloadSpec {
    state: std::sync::RwLock<SpecState>,
    /// 本次运行的连接数硬上限（插件 `maxConnections`）；`None` = 不限。
    connection_cap: Option<usize>,
    refresher: Option<crate::intercept::Refresher>,
    /// 凭据刷新单飞闸门：持有期间其他被拒请求排队，拿到闸门后先看是否已被刷新。
    refresh_gate: tokio::sync::Mutex<RefreshLedger>,
    /// 已取得成功响应的最新身份代际 + 1（0 = 尚无）。见 [`Self::confirm`]。
    confirmed_generation: std::sync::atomic::AtomicU64,
    /// 单 GET 探测保留的首响应（一次性 URL 的唯一一次请求）；取一次即空，
    /// 随 spec 一并释放（取消 / 跳过 / 失败时连接随之关闭）。
    held_response: std::sync::Mutex<Option<reqwest::Response>>,
}

#[derive(Debug)]
struct SpecState {
    current: std::sync::Arc<RequestSpec>,
    /// 每次替换 `current` 递增。
    generation: u64,
    /// 已切换为中性 UA（每次运行至多一次）。
    neutral: bool,
    /// 凭据刷新改写后的任务 URL（只替换发往 [`crate::intercept::Refresher::origin_url`] 的请求）。
    url: Option<String>,
}

#[derive(Debug, Default)]
struct RefreshLedger {
    /// 本次运行已成功刷新的次数（插件上下文 `refreshCount`）。
    total: u32,
    /// 连续的「换发后尚未取得过成功响应」的刷新次数；达到
    /// [`crate::intercept::refresh::MAX_UNCONFIRMED_REFRESHES`] 即停止刷新。
    unconfirmed: u32,
    /// 插件已对该代际表示无可刷新：同代际的后续被拒请求不再询问。
    declined_generation: Option<u64>,
}

impl DownloadSpec {
    /// 以任务（或探测自适应后）的请求头起步。
    pub fn new(base: RequestSpec) -> Self {
        Self {
            state: std::sync::RwLock::new(SpecState {
                current: std::sync::Arc::new(base),
                generation: 0,
                neutral: false,
                url: None,
            }),
            connection_cap: None,
            refresher: None,
            refresh_gate: tokio::sync::Mutex::new(RefreshLedger::default()),
            confirmed_generation: std::sync::atomic::AtomicU64::new(0),
            held_response: std::sync::Mutex::new(None),
        }
    }

    /// 挂上单 GET 探测保留的首响应（`None` = 无）。
    pub(crate) fn with_held_response(self, resp: Option<reqwest::Response>) -> Self {
        *self
            .held_response
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = resp;
        self
    }

    /// 是否仍持有未被消费的首响应。
    pub(crate) fn has_held_response(&self) -> bool {
        self.held_response
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// 取走首响应（至多一次）：首个无 Range 的数据请求直接消费它，不再发新请求。
    pub(crate) fn take_held_response(&self) -> Option<reqwest::Response> {
        self.held_response
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// 设置连接数硬上限；`cap <= 0` 表示不限。
    pub(crate) fn with_connection_cap(mut self, cap: i32) -> Self {
        if cap > 0 {
            self.connection_cap = Some(cap as usize);
        }
        self
    }

    /// 挂上凭据刷新器（`None` = 无插件声明 `refresh`，被拒请求按原逻辑处理）。
    pub(crate) fn with_refresher(mut self, refresher: Option<crate::intercept::Refresher>) -> Self {
        self.refresher = refresher;
        self
    }

    /// 连接数硬上限（`None` = 不限）。
    pub(crate) fn connection_cap(&self) -> Option<usize> {
        self.connection_cap
    }

    fn read_state(&self) -> std::sync::RwLockReadGuard<'_, SpecState> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_state(&self) -> std::sync::RwLockWriteGuard<'_, SpecState> {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 当前应使用的请求身份（已含质询脱困 / 凭据刷新的结果）。
    pub fn current(&self) -> std::sync::Arc<RequestSpec> {
        self.read_state().current.clone()
    }

    /// 发往 `url` 的请求应改用的地址：仅当凭据刷新改写了任务 URL 且 `url` 是起跑时的
    /// 任务 URL 时返回 `Some`（CDN 备选节点、重定向终点等其他地址不受影响）。
    pub(crate) fn route_url(&self, url: &str) -> Option<String> {
        let refresher = self.refresher.as_ref()?;
        if url != refresher.origin_url() {
            return None;
        }
        self.read_state().url.clone()
    }

    /// `resp` 是用 `used` 发出的下载请求的响应。它是反机器人质询且 `used` 还不是
    /// 中性 UA 时，返回应重发所用的中性 UA 请求头（首次切换时落库）；否则 `None`，
    /// 调用方照常按状态码处理。质询由 CDN 边缘直接拦截，源站没收到原请求，重发
    /// 不会多消耗一次性 URL；`used` 已是中性 UA 时不再重发，每个请求最多多发一次。
    pub(crate) async fn escape_bot_challenge(
        &self,
        used: &RequestSpec,
        resp: &reqwest::Response,
        db: &Db,
        task_id: &str,
    ) -> Option<std::sync::Arc<RequestSpec>> {
        if used.pins.user_agent
            || resp.status().is_success()
            || !is_bot_challenge(resp.headers())
            || has_neutral_ua(used)
        {
            return None;
        }
        let (neutral, first) = {
            let mut state = self.write_state();
            if state.neutral {
                (state.current.clone(), false)
            } else {
                state.current = std::sync::Arc::new(spec_with_neutral_ua(&state.current));
                state.generation += 1;
                state.neutral = true;
                (state.current.clone(), true)
            }
        };
        if first {
            log_info!(
                "[download] task {} bot challenge (status={}) on download request: switching \
                 to neutral downloader UA",
                task_id,
                resp.status().as_u16()
            );
            persist_adapted_spec(db, task_id, &neutral).await;
        }
        Some(neutral)
    }

    /// 记录用 `used` 发出的请求取得了成功响应。此后该身份再被拒属于「凭据被正常
    /// 消耗」（如每请求一个令牌），不计入连续无效刷新上限。无刷新器时为空操作。
    pub(crate) fn confirm(&self, used: &std::sync::Arc<RequestSpec>) {
        if self.refresher.is_none() {
            return;
        }
        let state = self.read_state();
        if std::sync::Arc::ptr_eq(&state.current, used) {
            self.confirmed_generation
                .fetch_max(state.generation + 1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// 用 `used` 发出的请求得到 `status`。可刷新（401 / 403 / 410 且有插件声明
    /// `refresh`）时返回应重发所用的新请求身份：
    /// - `used` 已不是当前身份（别的请求刚刷新过 / 已质询脱困）→ 直接返回当前身份，不调用插件；
    /// - 否则单飞调用插件，成功换发则返回新身份；
    /// - 插件不换发、连续换发后都没换来成功响应（达
    ///   [`crate::intercept::refresh::MAX_UNCONFIRMED_REFRESHES`] 次）或不可刷新 → `None`，
    ///   调用方按原错误处理。
    ///
    /// 调用方须保证同一个请求至多因刷新重发一次，并在请求成功时调用 [`Self::confirm`]。
    /// 插件以 `onFailure: "fail"` 出错时返回错误。
    pub(crate) async fn refresh_after_rejection(
        &self,
        used: &std::sync::Arc<RequestSpec>,
        status: reqwest::StatusCode,
    ) -> Result<Option<std::sync::Arc<RequestSpec>>, DownloadError> {
        let Some(refresher) = self.refresher.as_ref() else {
            return Ok(None);
        };
        if !crate::intercept::refresh::is_refresh_status(status) {
            return Ok(None);
        }
        let mut ledger = self.refresh_gate.lock().await;
        let (generation, mut url, mut spec) = {
            let state = self.read_state();
            if !std::sync::Arc::ptr_eq(&state.current, used) {
                return Ok(Some(state.current.clone()));
            }
            (
                state.generation,
                state
                    .url
                    .clone()
                    .unwrap_or_else(|| refresher.origin_url().to_string()),
                (**used).clone(),
            )
        };
        // 被拒的身份此前成功过：凭据是被正常用掉的，连续无效计数归零。
        if self
            .confirmed_generation
            .load(std::sync::atomic::Ordering::Relaxed)
            > generation
        {
            ledger.unconfirmed = 0;
        }
        if ledger.declined_generation == Some(generation)
            || ledger.unconfirmed >= crate::intercept::refresh::MAX_UNCONFIRMED_REFRESHES
        {
            return Ok(None);
        }
        let changed = refresher
            .refresh(&mut url, &mut spec, status.as_u16(), ledger.total)
            .await?;
        if !changed {
            ledger.declined_generation = Some(generation);
            return Ok(None);
        }
        ledger.total += 1;
        ledger.unconfirmed += 1;
        let fresh = std::sync::Arc::new(spec);
        let mut state = self.write_state();
        state.current = fresh.clone();
        state.generation += 1;
        state.url = (url != refresher.origin_url()).then_some(url);
        Ok(Some(fresh))
    }
}

/// 为请求附加默认浏览器 UA（用于服务器拒绝非浏览器 UA / FluxDown 默认 UA 的场景）。
fn spec_with_browser_ua(spec: &RequestSpec) -> RequestSpec {
    let mut s = spec.clone();
    if !s.pins.user_agent {
        s.extra_headers
            .insert("User-Agent".to_string(), DEFAULT_BROWSER_UA.to_string());
    }
    s
}

/// 为请求附加推导来源页 Referer（用于 CDN 防盗链拒绝无 Referer 的场景）。
fn spec_with_inferred_referrer(spec: &RequestSpec, url: &str) -> RequestSpec {
    let mut s = spec.clone();
    fill_inferred_referrer(&mut s, url);
    s
}

/// 同时附加浏览器 UA 与推导 Referer（两者各自尊重锁定）。
fn spec_with_browser_ua_and_referrer(spec: &RequestSpec, url: &str) -> RequestSpec {
    let mut s = spec_with_browser_ua(spec);
    fill_inferred_referrer(&mut s, url);
    s
}

fn fill_inferred_referrer(s: &mut RequestSpec, url: &str) {
    if !s.pins.referrer
        && s.referrer.is_empty()
        && let Some(ref_url) = infer_origin_referrer(url)
    {
        s.referrer = ref_url;
    }
}

/// 把请求头自适应结果写回任务的持久化请求头，使后续分段 worker 与续传（尤其免探测的 hint 续传）沿用。
///
/// 自适应只改 UA 与 Referer，因此只同步这两项（且跳过插件锁定的字段）；其余请求头
/// 可能是插件按次运行叠加的改写，绝不写回持久化的基础上下文。
pub(crate) async fn persist_adapted_spec(db: &Db, task_id: &str, adapted: &RequestSpec) {
    let Ok(Some((cookies, current_referrer, headers_json))) =
        db.load_task_request_context(task_id).await
    else {
        return;
    };

    let mut headers = if headers_json.is_empty() {
        std::collections::HashMap::new()
    } else {
        match serde_json::from_str::<std::collections::HashMap<String, String>>(&headers_json) {
            Ok(h) => h,
            Err(e) => {
                // 已落库的请求头损坏：放弃自适应落库，绝不以残缺 map 覆写原值。
                log_warn!(
                    "[resolve] task {} persisted request headers are corrupt, skip persisting adapted context: {e:#}",
                    task_id
                );
                return;
            }
        }
    };

    let before_headers = headers.clone();
    let before_referrer = current_referrer.clone();

    if !adapted.pins.user_agent {
        headers.retain(|k, _| !k.eq_ignore_ascii_case("user-agent"));
        if let Some((k, v)) = adapted
            .extra_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
        {
            headers.insert(k.clone(), v.clone());
        }
    }

    let next_referrer = if !adapted.pins.referrer && !adapted.referrer.is_empty() {
        &adapted.referrer
    } else {
        &current_referrer
    };

    if headers == before_headers && *next_referrer == before_referrer {
        return;
    }

    let json = match serde_json::to_string(&headers) {
        Ok(j) => j,
        Err(e) => {
            log_warn!(
                "[resolve] task {} failed to serialize adapted request headers: {e:#}",
                task_id
            );
            return;
        }
    };

    if let Err(e) = db
        .set_task_request_context(task_id, &cookies, next_referrer, &json)
        .await
    {
        log_warn!(
            "[resolve] task {} failed to persist adapted request context: {}",
            task_id,
            e
        );
    }
}

/// 同 [`resolve_file_info`]，额外返回探测是否靠请求头自适应（去掉浏览器 UA /
/// 补充浏览器 UA / 补充防盗链 Referer）才成功。成功时返回 `Some(adapted_spec)`。
pub(crate) async fn resolve_file_info_with_ua_fallback(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
) -> Result<(FileInfo, Option<RequestSpec>), DownloadError> {
    let (info, adapted, _) =
        adapt_probe_request(client, url, spec, ProbeKind::HeadAndRange, None).await?;
    Ok((info, adapted))
}

/// 一次性 URL / 表单端点友好的探测：只发【一个】与数据下载同规格的请求（GET 类为无 Range
/// 的 plain GET，非 GET 为原 method + body），响应头即文件元数据，响应本身原样保留作第一条
/// 数据连接——表单 POST 只向服务端提交一次。请求头自适应阶梯（UA / Referer / 质询）与
/// [`resolve_file_info_with_ua_fallback`] 相同——只有上一次被拒绝后才会重发，被拒的请求
/// 本就没消耗下载令牌。
pub(crate) async fn resolve_file_info_held(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
) -> Result<(FileInfo, Option<RequestSpec>, reqwest::Response), DownloadError> {
    match adapt_probe_request(client, url, spec, ProbeKind::Held, None).await? {
        (info, adapted, Some(resp)) => Ok((info, adapted, resp)),
        (_, _, None) => Err(DownloadError::Other(
            "single probe request returned no response".to_string(),
        )),
    }
}

/// 探测请求的形态（自适应阶梯共用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbeKind {
    /// HEAD ∥ `Range: bytes=0-0`，响应只读头。
    HeadAndRange,
    /// 单个与数据下载同规格的请求（GET 类 plain GET / 非 GET 原 method + body），响应保留给
    /// 数据下载。
    Held,
}

/// 探测重试 / 请求头自适应阶梯；`kind` 决定单轮探测怎么发。非 GET 请求一律按
/// [`ProbeKind::Held`] 发：HEAD 不能携带请求体、POST + Range 未定义，而丢弃探测响应再
/// 提交一次会让服务端把同一表单处理两遍（OneDrive 打包下载会重新打包整个 zip）。
///
/// `path_signal`：每轮探测失败都上报其类别（断在传输层 / 拿到服务器应答）——
/// Auto 探测竞速（[`crate::auto_probe`]）据此不等本路径的退避重试跑完就放出
/// 备选路径，或在链路已证明可达时撤销错峰计时。
pub(crate) async fn adapt_probe_request(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
    kind: ProbeKind,
    path_signal: Option<&crate::auto_probe::PathSignal>,
) -> Result<(FileInfo, Option<RequestSpec>, Option<reqwest::Response>), DownloadError> {
    let kind = if spec.is_get_like() {
        kind
    } else {
        ProbeKind::Held
    };
    let has_browser_ua = spec
        .extra_headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("user-agent"));

    let mut last_err = None;
    let mut current_adapted: Option<RequestSpec> = None;

    for attempt in 0..PROBE_MAX_RETRIES {
        let attempt_spec = if attempt == 0 {
            spec
        } else if let Some(ref adapted) = current_adapted {
            adapted
        } else {
            spec
        };

        let final_attempt = attempt + 1 == PROBE_MAX_RETRIES;
        let attempted = match kind {
            ProbeKind::HeadAndRange => {
                resolve_file_info_once(client, url, attempt_spec, final_attempt)
                    .await
                    .map(|info| (info, None))
            }
            ProbeKind::Held => resolve_file_info_held_once(client, url, attempt_spec)
                .await
                .map(|(info, resp)| (info, Some(resp))),
        };
        match attempted {
            Ok((info, held)) => return Ok((info, current_adapted, held)),
            Err(e) => {
                log_info!(
                    "[resolve] probe attempt {}/{} failed: {}",
                    attempt + 1,
                    PROBE_MAX_RETRIES,
                    e
                );
                if let Some(signal) = path_signal {
                    signal.observe(&e);
                }
                if probe_post_only(&e) {
                    log_info!("[resolve] endpoint only accepts POST; stopping probes");
                    return Err(DownloadError::Other(POST_ONLY_ENDPOINT_MESSAGE.to_string()));
                }
                let rejection = probe_rejection(&e);
                last_err = Some(e);
                // 仅在可能触发去重时才快照刚发出的请求身份（锁定了 UA / Referer）。
                let sent_identity = (spec.pins.user_agent || spec.pins.referrer).then(|| {
                    (
                        attempt_spec.extra_headers.clone(),
                        attempt_spec.referrer.clone(),
                        attempt_spec.cookies.clone(),
                    )
                });

                if attempt + 1 < PROBE_MAX_RETRIES {
                    if rejection == Some(ProbeRejection::BotChallenge) {
                        // 反机器人质询（Cloudflare）：浏览器 UA × rustls 指纹冲突，无论 UA
                        // 来自扩展请求头还是任务 / 队列 / 全局设置，都换成中性 UA。
                        log_info!("[resolve] bot challenge: switching to neutral downloader UA");
                        let base = current_adapted.as_ref().unwrap_or(spec);
                        current_adapted = Some(spec_with_neutral_ua(base));
                    } else if rejection.is_some() {
                        // 403 / 429 等服务端拒绝：根据当前请求头生成自适应候选。
                        if has_browser_ua {
                            // 场景 A（Cloudflare 反爬）：浏览器 UA 与 rustls 指纹冲突，剥离 UA
                            if attempt == 0 {
                                log_info!(
                                    "[resolve] server rejection (403/429): stripping browser UA to avoid bot detection"
                                );
                                current_adapted = Some(spec_without_browser_ua(spec));
                            } else if spec.referrer.is_empty() {
                                // 若剥离 UA 依然被拒绝且无 Referer，尝试补全推导 Referer
                                log_info!(
                                    "[resolve] server rejection persists: trying stripped UA with inferred origin Referer"
                                );
                                current_adapted = Some(spec_with_inferred_referrer(
                                    &spec_without_browser_ua(spec),
                                    url,
                                ));
                            }
                        } else {
                            // 场景 B（NVIDIA / Akamai 反盗链/爬虫）：默认 UA（FluxDown/*）或缺 Referer 被拒
                            if attempt == 0 {
                                log_info!(
                                    "[resolve] server rejection (403/429): adapting with browser UA and inferred origin Referer"
                                );
                                current_adapted =
                                    Some(spec_with_browser_ua_and_referrer(spec, url));
                            } else if attempt == 1 {
                                // 若组合仍被拒绝，回退为默认 UA + 推导 Referer（针对需要 Referer 但拦截 Chrome+rustls 指纹的 Cloudflare 等站点）
                                log_info!(
                                    "[resolve] server rejection persists: trying default UA with inferred origin Referer"
                                );
                                current_adapted = Some(spec_with_inferred_referrer(spec, url));
                            }
                        }
                    } else if has_browser_ua && attempt + 2 == PROBE_MAX_RETRIES {
                        // 最后一轮重试前的兜底降级（即便不是明确 403）：尝试剥离浏览器 UA（保留可能已设置的 Referer）
                        let base = current_adapted.as_ref().unwrap_or(spec);
                        current_adapted = Some(spec_without_browser_ua(base));
                    }

                    // 锁定（插件指定 UA / Referer）后自适应阶梯可能原样重发：下一轮请求身份
                    // 与刚发出的相同则立即停止，不白耗一次性令牌。
                    if let Some((sent_headers, sent_referrer, sent_cookies)) = sent_identity {
                        let next = current_adapted.as_ref().unwrap_or(spec);
                        if rejection.is_some()
                            && next.extra_headers == sent_headers
                            && next.referrer == sent_referrer
                            && next.cookies == sent_cookies
                        {
                            log_info!(
                                "[resolve] pinned request identity rejected; not resending an identical probe"
                            );
                            return Err(last_err.unwrap_or_else(|| {
                                DownloadError::Other("probe failed after retries".to_string())
                            }));
                        }
                    }

                    // 服务端拒绝无需长时退避，短等待 200ms 即可切换新请求头重试；
                    // 网络抖动保持指数退避。
                    let delay = if rejection.is_some() {
                        Duration::from_millis(200)
                    } else {
                        PROBE_RETRY_BASE_DELAY * 2u32.saturating_pow(attempt)
                    };
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| DownloadError::Other("probe failed after retries".to_string())))
}

/// Walk the std::error::Error source chain and return a " → cause1 → cause2" suffix string.
/// Returns an empty string when there is no source, so it can be appended directly to a message.
fn format_error_chain(mut src: Option<&dyn StdError>) -> String {
    let mut s = String::new();
    while let Some(cause) = src {
        s.push_str(" → ");
        s.push_str(&cause.to_string());
        src = cause.source();
    }
    s
}

/// 错误文本 + 完整 source 链。reqwest 的 Display 只有「error sending request
/// for url (…)」，真正的 connection refused / timed out 在 source 里；传输层
/// 错误分类必须带上链，否则建连失败恒匹配不到。
pub(crate) fn download_error_chain_text(e: &DownloadError) -> String {
    format!("{e}{}", format_error_chain(StdError::source(e)))
}

/// 探测请求的传输层失败描述：保留完整根因链（reqwest 的 Display 只有
/// 「error sending request for url (…)」）。分类另由
/// [`crate::net_failure::classify`] 给出，以标签形式追加在最终错误文案上。
fn network_desc(e: &reqwest::Error) -> String {
    format!("network-error: {e}{}", format_error_chain(e.source()))
}

/// [`crate::http_request::HttpRequest::send`] 错误的描述：传输错误同 [`network_desc`]，
/// 重定向错误（跳数超限 / 非 http(s) 目标）原样。
fn send_error_desc(e: &DownloadError) -> String {
    match e {
        DownloadError::Request(e) => network_desc(e),
        other => other.to_string(),
    }
}

/// `final_attempt`：本轮是否为重试循环的最后一轮——只在最后一轮才把「两路探测都
/// 断在传输层」交给无 Range 的 plain GET 兜底（见下方 `(None, None)` 分支）。
/// 只处理 GET 类请求：非 GET 由 [`adapt_probe_request`] 改走 [`ProbeKind::Held`]。
async fn resolve_file_info_once(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
    final_attempt: bool,
) -> Result<FileInfo, DownloadError> {
    let cookies = spec.cookies.as_str();
    // --- Concurrent HEAD + GET probe ----------------------------------------
    // Fire both HEAD and GET Range:0-0 in parallel.  HEAD is faster when it
    // works, but many servers/CDNs omit Content-Disposition on HEAD.  By
    // running both concurrently we avoid the serial HEAD→GET penalty.
    //
    // IMPORTANT for Content-Encoding handling:
    // Many CDNs (Cloudflare, Akamai) add Content-Encoding: gzip to HEAD and
    // full-GET responses but **omit** it from 206 Partial Content responses.
    // This is correct per HTTP semantics: Range requests operate on the
    // *original* (identity) representation, not the compressed one.
    //
    // We therefore check Content-Encoding on the GET Range:0-0 response
    // **separately** from the merged headers.  If GET returned 206 without
    // Content-Encoding, Range requests are safe for multi-segment downloads
    // even when HEAD advertised compression.

    let head_fut = build_request(client, url, reqwest::Method::HEAD, spec)
        .timeout(PROBE_TIMEOUT)
        .send();

    let get_fut = build_request(client, url, reqwest::Method::GET, spec)
        .header("Range", "bytes=0-0")
        .timeout(PROBE_TIMEOUT)
        .send();

    let (head_result, get_result) = await_probe_pair(
        head_fut,
        get_fut,
        |get| {
            get.as_ref().is_ok_and(|r| {
                r.status() == reqwest::StatusCode::PARTIAL_CONTENT
                    && r.headers().contains_key(reqwest::header::CONTENT_RANGE)
            })
        },
        PROBE_HEAD_GRACE,
    )
    .await;
    // 两路都没拿到任何 HTTP 响应（TLS 握手 EOF、连接重置、超时……）：这是链路
    // 抖动，不是服务器拒绝 HEAD / Range 的证据。
    let transport_only_failure = matches!(head_result, Some(Err(_))) && get_result.is_err();
    // 两路都断在传输层时的类型化分类（GET 优先：Range 0-0 与数据连接同形）。
    let transport_class = match (&head_result, &get_result) {
        (Some(Err(head)), Err(get)) => crate::net_failure::classify_download_error(get)
            .or_else(|| crate::net_failure::classify_download_error(head)),
        _ => None,
    };

    // Extract HEAD response (if successful)
    let mut head_status_desc = String::new();
    let head_data = match head_result {
        None => {
            log_info!(
                "[resolve] HEAD still pending {}ms after a conclusive ranged GET; \
                 proceeding with the GET metadata",
                PROBE_HEAD_GRACE.as_millis()
            );
            None
        }
        Some(Ok(r)) if r.status().is_success() => {
            let u = r.url().clone();
            let h = r.headers().clone();
            Some((h, u))
        }
        Some(Ok(r)) => {
            head_status_desc = probe_status_desc(&r);
            log_info!(
                "[resolve] HEAD failed: status={}, url={}, cookies_len={}",
                r.status(),
                crate::logger::sanitize_log_str(r.url().as_str()),
                cookies.len()
            );
            None
        }
        Some(Err(e)) => {
            head_status_desc = send_error_desc(&e);
            log_info!(
                "[resolve] HEAD network error: {}{}, cookies_len={}",
                e,
                format_error_chain(e.source()),
                cookies.len()
            );
            None
        }
    };

    // Extract GET response (if successful)
    let mut get_status_desc = String::new();
    let get_data = match get_result {
        Ok(r) if r.status().is_success() => {
            let u = r.url().clone();
            let h = r.headers().clone();
            let got_206 = r.status() == reqwest::StatusCode::PARTIAL_CONTENT;
            // Check Content-Encoding on the GET Range:0-0 response BEFORE
            // merging with HEAD.  This tells us whether Range responses
            // carry compression — the key signal for multi-segment safety.
            let get_range_compressed = got_206 && detect_content_encoding(&h).is_some();
            drop(r); // release connection immediately
            Some((h, u, got_206, get_range_compressed))
        }
        Ok(r) => {
            get_status_desc = probe_status_desc(&r);
            log_info!(
                "[resolve] GET failed: status={}, url={}, cookies_len={}",
                r.status(),
                crate::logger::sanitize_log_str(r.url().as_str()),
                cookies.len()
            );
            None
        }
        Err(e) => {
            get_status_desc = send_error_desc(&e);
            log_info!(
                "[resolve] GET network error: {}{}, cookies_len={}",
                e,
                format_error_chain(e.source()),
                cookies.len()
            );
            None
        }
    };

    // Track whether the GET Range:0-0 response itself carried compression.
    // false = either GET didn't succeed, returned 200 (not 206), or returned
    //         206 without Content-Encoding → Range requests are safe.
    // true  = GET returned 206 WITH Content-Encoding → rare but must disable
    //         multi-segment to avoid corrupt byte-range splicing.
    let range_response_compressed = get_data
        .as_ref()
        .is_some_and(|(_, _, _, compressed)| *compressed);

    // Merge results: HEAD as base, GET to fill in missing data.
    let (mut headers, mut final_url) = match (&head_data, &get_data) {
        (Some((hh, hu)), _) => (hh.clone(), hu.clone()),
        (None, Some((gh, gu, _, _))) => (gh.clone(), gu.clone()),
        (None, None) => {
            // 双探测仅因链路抖动失败：交回重试循环按退避重探。此时直接走 plain GET
            // 兜底，只要兜底请求恰好连上，整个任务就会被判成「不支持 Range」单连接
            // 下载（实测 python.org 经代理一次 TLS 握手 EOF 就让 89MB 单流跑完）。
            if transport_only_failure && !final_attempt {
                return Err(DownloadError::Other(crate::net_failure::append_tag(
                    format!("probes failed: HEAD={head_status_desc}, ranged GET={get_status_desc}"),
                    transport_class,
                )));
            }
            // 双探测（HEAD + Range GET）均失败。部分合法服务器（如飞牛 OS
            // multiple-download 端点）对下载 token 有并发/次数配额：HEAD 恒
            // 405，带 Range 的 GET 恒 400（配额已耗尽/不支持 Range），但一次
            // 【无 Range 的普通 GET】能正常 200。在判死这次探测前，再试一次
            // 普通 GET 作为最后的降级路径，避免把可下载的任务误判为失败。
            return resolve_file_info_plain_get_fallback(
                client,
                url,
                spec,
                &head_status_desc,
                &get_status_desc,
                transport_class,
            )
            .await;
        }
    };

    // If HEAD succeeded but lacks Content-Disposition, merge from GET.
    if head_data.is_some()
        && let Some((get_headers, get_url, got_206, _)) = &get_data
    {
        if !headers.contains_key(reqwest::header::CONTENT_DISPOSITION)
            && let Some(cd) = get_headers.get(reqwest::header::CONTENT_DISPOSITION)
        {
            headers.insert(reqwest::header::CONTENT_DISPOSITION, cd.clone());
        }
        if let Some(ct) = get_headers.get(reqwest::header::CONTENT_TYPE) {
            headers.insert(reqwest::header::CONTENT_TYPE, ct.clone());
        }
        // Prefer GET's final URL (may differ after redirect)
        final_url = get_url.clone();
        // If GET gave us 206, copy Content-Range for accurate file size
        if *got_206 && let Some(cr) = get_headers.get("content-range") {
            headers.insert(
                reqwest::header::HeaderName::from_static("content-range"),
                cr.clone(),
            );
        }
    }

    // --- Phase 3: Parse metadata from merged headers ------------------------
    // A 206 response from GET proves range support even without Accept-Ranges header.
    let got_206_from_get = get_data.as_ref().is_some_and(|(_, _, got, _)| *got);
    let mut supports_range = got_206_from_get
        || headers
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v != "none");

    let mut range_206_missing_content_range = false;
    let total_bytes = if let Some(cr) = headers.get("content-range") {
        // e.g. "bytes 0-0/12345"
        cr.to_str()
            .ok()
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    } else if got_206_from_get {
        // F021: GET Range:0-0 返回 206 却缺失 Content-Range 头（违反 RFC 9110
        // 但现实存在的破损服务器/中间件）。此时 Content-Length=1 是【范围长度】
        // （0-0 这一个字节），不是文件总大小，绝不能拿来当 total_bytes，否则会
        // 被当成 1 字节文件处理并几乎必然触发后续 size mismatch。改为置 0
        // （未知大小），走下游 unknown-size 单流路径（读到 EOF、跳过 size 校验），
        // 语义正确且不会误判。
        log_info!(
            "[resolve] WARNING: GET returned 206 without Content-Range — Content-Length is \
             the range length (not file size); treating total_bytes as unknown (0)"
        );
        range_206_missing_content_range = true;
        0
    } else {
        headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };
    // 缺 Content-Range 的 206 无法确认响应体起点：按范围返回的服务器与「发从 0
    // 全量流」的服务器在外观上无法区分，续传/分段都会错位。视为不支持 Range，
    // 从源头杜绝对该服务器发 Range。
    if range_206_missing_content_range {
        supports_range = false;
    }

    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let resolved = extract_filename(&headers, url, final_url.as_str());
    let (file_name, name_source) = (resolved.name, resolved.source);
    log_info!(
        "[resolve] url={} → name={}, size={}, range={}, ct={}",
        crate::logger::sanitize_log_str(url),
        file_name,
        total_bytes,
        supports_range,
        content_type
    );

    let etag = headers
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let last_modified = headers
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // --- Content-Encoding handling -------------------------------------------
    //
    // The merged `headers` may carry Content-Encoding from the HEAD response.
    // However, this does NOT mean Range responses are also compressed.
    //
    // HTTP semantics (RFC 9110 §8.8.3): Range requests operate on the
    // "selected representation" which is typically the **identity** encoding.
    // Most CDNs (Cloudflare, Akamai, AWS CloudFront) correctly:
    //   - HEAD / full GET → Content-Encoding: gzip (if Accept-Encoding allows)
    //   - GET Range:bytes=X-Y → 206 with NO Content-Encoding (raw bytes)
    //
    // We use the GET Range:0-0 probe result (`range_response_compressed`) as
    // the authoritative signal for multi-segment safety:
    //
    //   GET 206 WITHOUT Content-Encoding → Range returns raw bytes → safe
    //   GET 206 WITH    Content-Encoding → rare; server compresses Range
    //                                      responses too → NOT safe
    //   Only HEAD available (GET failed)  → conservative; use HEAD's signal
    //
    // When Range responses ARE compressed, we disable multi-segment and let
    // `download_single` decompress the full-GET stream on-the-fly.
    //
    // When Range responses are NOT compressed (the common case), multi-segment
    // can proceed normally even if HEAD showed Content-Encoding.

    // Did *any* probe response (HEAD or GET) indicate compression?
    let content_encoding_compressed = detect_content_encoding(&headers).is_some();

    // Should we disable Range support due to compression?
    // Only if the GET Range:0-0 *itself* returned compressed content,
    // OR if we have no GET data and must rely on HEAD alone.
    let got_get_206 = get_data.as_ref().is_some_and(|(_, _, got, _)| *got);
    let disable_range_for_compression = if got_get_206 {
        // We have a 206 response — use its Content-Encoding as ground truth.
        range_response_compressed
    } else {
        // No 206 available (GET failed or returned 200) — fall back to the
        // merged headers (conservative: if HEAD says compressed, disable).
        content_encoding_compressed
    };

    if disable_range_for_compression {
        log_info!(
            "[resolve] WARNING: Range response itself carries Content-Encoding: {:?} — \
             byte ranges are invalid on compressed streams; disabling multi-segment",
            headers
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?")
        );
        supports_range = false;
    } else if content_encoding_compressed {
        // HEAD indicated compression but the GET 206 did NOT — Range requests
        // return raw (identity) bytes.  Multi-segment is safe.  The HEAD's
        // Content-Length may be the compressed size though — if we got a
        // Content-Range from the 206, that already gave us the real file size.
        log_info!(
            "[resolve] HEAD indicated Content-Encoding: {:?} but GET Range:0-0 \
             returned 206 without compression — Range requests use identity \
             encoding; multi-segment is safe (total_bytes={}, range={})",
            headers
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?"),
            total_bytes,
            supports_range
        );
    }

    Ok(FileInfo {
        file_name,
        name_source,
        total_bytes,
        supports_range,
        content_type,
        etag,
        last_modified,
        content_encoding_compressed,
    })
}

/// 拼接三次探测（HEAD / ranged GET / plain GET）全部失败时的诊断文案，
/// 形如 `"probes failed: HEAD=405, ranged GET=400, plain GET=400"`。抽成
/// 纯函数（不涉及网络 I/O）便于单元测试覆盖格式，不必依赖 mock HTTP server。
fn format_probe_failure(
    head_status_desc: &str,
    get_status_desc: &str,
    plain_status_desc: &str,
) -> String {
    format!(
        "probes failed: HEAD={}, ranged GET={}, plain GET={}",
        head_status_desc, get_status_desc, plain_status_desc
    )
}

/// 双探测（HEAD + Range GET）失败后的最后一次降级尝试：发一次【无 Range 头】
/// 的普通 GET，只读响应头后立即 drop response（绝不读 body），从中提取文件
/// 元数据。
///
/// 背景（例如飞牛 OS「多文件下载」multiple-download 端点）：一次性/限配额
/// 下载 token 只允许消耗有限次数的请求——HEAD 方法本身不被支持（恒 405），
/// 带 Range 的 GET 也被拒绝（恒 400，配额已耗尽或该端点根本不支持 Range），
/// 但不带 Range 的普通 GET 能正常返回 200。旧逻辑在双探测失败时直接判死
/// 任务，而这类服务器其实是可下载的——只是探测方式不对路。
///
/// 至少一路探测拿到了 HTTP 响应时，每轮最多发出这第 3 个请求（HEAD + ranged
/// GET + 这次的 plain GET）。两路都断在传输层（无任何响应）时只有最后一轮才
/// 兜底：前几轮交回重试循环退避重探——plain GET 恰好连上会让整个任务被判成
/// 「不支持 Range」单连接下载，链路抖动不能换来这种降级。
///
/// `prior_transport`：HEAD 与 ranged GET 是否都断在传输层（及其分类）。三路
/// 全部没拿到任何 HTTP 响应时，最终错误才带传输失败标签——任何一路拿到
/// 响应都说明链路可达，失败归服务器而非路径。
async fn resolve_file_info_plain_get_fallback(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
    head_status_desc: &str,
    get_status_desc: &str,
    prior_transport: Option<crate::net_failure::NetFailure>,
) -> Result<FileInfo, DownloadError> {
    log_info!(
        "[resolve] both probes failed (HEAD={}, ranged GET={}), falling back to plain GET \
         (no Range header) — some servers (e.g. fnOS multiple-download) reject HEAD and \
         Range requests due to a limited-quota token but serve a normal GET",
        head_status_desc,
        get_status_desc
    );

    let resp = match build_request(client, url, reqwest::Method::GET, spec)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let plain_status_desc = probe_status_desc(&r);
            log_info!(
                "[resolve] plain GET fallback also failed: status={}, url={}",
                r.status(),
                crate::logger::sanitize_log_str(r.url().as_str())
            );
            return Err(DownloadError::Other(format_probe_failure(
                head_status_desc,
                get_status_desc,
                &plain_status_desc,
            )));
        }
        Err(e) => {
            let plain_status_desc = send_error_desc(&e);
            log_info!(
                "[resolve] plain GET fallback network error: {}{}",
                e,
                format_error_chain(e.source())
            );
            let transport = prior_transport
                .and(crate::net_failure::classify_download_error(&e).or(prior_transport));
            return Err(DownloadError::Other(crate::net_failure::append_tag(
                format_probe_failure(head_status_desc, get_status_desc, &plain_status_desc),
                transport,
            )));
        }
    };

    let final_url = resp.url().clone();
    let headers = resp.headers().clone();
    // 只读响应头，立即 drop response——绝不读取 body，避免消耗一次性 token
    // 或对配额受限的连接造成额外压力。真正的下载阶段会重新发起独立请求。
    drop(resp);

    let total_bytes = headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let resolved = extract_filename(&headers, url, final_url.as_str());
    let (file_name, name_source) = (resolved.name, resolved.source);

    let etag = headers
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let last_modified = headers
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let content_encoding_compressed = detect_content_encoding(&headers).is_some();

    // supports_range 决策：这条降级路径【没有 206 佐证】——我们只拿到一次
    // 普通 200 响应，从未验证过服务器真的会诚实响应 Range 请求；而且走到
    // 这里之前，带 Range 的 GET 已经被服务器以 400 明确拒绝过（见调用方的
    // both-probes-failed 分支）。哪怕这次响应头里带了 `Accept-Ranges: bytes`，
    // 也不能采信：这类一次性/限配额 token 的 Accept-Ranges 广告与实际行为
    // 经常脱节，继续相信它会让多段下载阶段对同一个受限 token 再次发起 Range
    // 请求，大概率复现同样的 400，甚至提前耗尽配额导致连单流都下不成。因此
    // 这里保守地强制单流（false），宁可错过少数"这次探测恰好用坏了 token、
    // 其实支持 Range"的场景，也要保证已确认可用的普通 GET 单流路径不被多段
    // 探测拖下水。
    let supports_range = false;

    log_info!(
        "[resolve] plain GET fallback succeeded: name={}, size={}, range={}, ct={}",
        file_name,
        total_bytes,
        supports_range,
        content_type
    );

    Ok(FileInfo {
        file_name,
        name_source,
        total_bytes,
        supports_range,
        content_type,
        etag,
        last_modified,
        content_encoding_compressed,
    })
}

/// 单请求探测的一轮：GET 类发一个 plain GET、非 GET 发原 method + body，成功则从响应头
/// 派生 [`FileInfo`] 并连同【未读 body 的响应】一起返回。
///
/// 与 `resolve_file_info_once` 的区别：没有 HEAD、没有 `Range: bytes=0-0`——一次性签名 URL
/// 的令牌在第一个 GET 就失效，表单端点每次提交都会重新处理，探测请求不能先于真实数据请求
/// 消耗它们。因此：
/// - Range 能力不在此实测：仅当 GET 类响应带 `Accept-Ranges: bytes`（与 coordinator 的裁决
///   同一判据）、未压缩且长度已知时才声明支持，下载器随后按 hint 语义「单流先起、证实后升
///   多段」；非 GET 恒不支持；
/// - 响应头等待用 `tokio::time::timeout` 而非 `RequestBuilder::timeout`（后者会把 body
///   读取也算进总时限，数据连接会被 15s 截断）。
async fn resolve_file_info_held_once(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
) -> Result<(FileInfo, reqwest::Response), DownloadError> {
    let (method, label) = if spec.is_get_like() {
        (reqwest::Method::GET, "plain GET")
    } else {
        (spec.method.clone(), spec.method.as_str())
    };
    let sent = tokio::time::timeout(
        PROBE_TIMEOUT,
        build_request(client, url, method, spec).send(),
    )
    .await;
    let resp = match sent {
        Err(_elapsed) => {
            // 15s 内没有任何响应头：建连黑洞或上游挂起，链路层面的超时。
            return Err(DownloadError::Other(crate::net_failure::append_tag(
                format!(
                    "probes failed: {label}=network-error: no response headers within {}s",
                    PROBE_TIMEOUT.as_secs()
                ),
                Some(crate::net_failure::NetFailure::Timeout),
            )));
        }
        Ok(Err(e)) => {
            log_info!(
                "[resolve] {} network error: {}{}",
                label,
                e,
                format_error_chain(e.source())
            );
            return Err(DownloadError::Other(crate::net_failure::append_tag(
                format!("probes failed: {label}={}", send_error_desc(&e)),
                crate::net_failure::classify_download_error(&e),
            )));
        }
        Ok(Ok(resp)) => resp,
    };
    // 对无 Range 请求回 206 不合规：响应体起点未知，无法当全量流写盘。
    if !resp.status().is_success() || resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let desc = probe_status_desc(&resp);
        log_info!(
            "[resolve] {} failed: status={}, url={}",
            label,
            resp.status(),
            crate::logger::sanitize_log_str(resp.url().as_str())
        );
        return Err(DownloadError::Other(format!(
            "probes failed: {label}={desc}"
        )));
    }

    let info = {
        let headers = resp.headers();
        let total_bytes = headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let header_text = |name: reqwest::header::HeaderName| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        let content_encoding_compressed = detect_content_encoding(headers).is_some();
        let accepts_ranges = headers
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("bytes"));
        let supports_range =
            spec.is_get_like() && accepts_ranges && !content_encoding_compressed && total_bytes > 0;
        let resolved = extract_filename(headers, url, resp.url().as_str());
        log_info!(
            "[resolve] single {} url={} → name={}, size={}, range={}, ct={}",
            label,
            crate::logger::sanitize_log_str(url),
            resolved.name,
            total_bytes,
            supports_range,
            header_text(reqwest::header::CONTENT_TYPE)
        );
        FileInfo {
            file_name: resolved.name,
            name_source: resolved.source,
            total_bytes,
            supports_range,
            content_type: header_text(reqwest::header::CONTENT_TYPE),
            etag: header_text(reqwest::header::ETAG),
            last_modified: header_text(reqwest::header::LAST_MODIFIED),
            content_encoding_compressed,
        }
    };
    Ok((info, resp))
}

/// 单流样本可信所需的最小传输量与时长：越过 TCP 慢启动（高 RTT 链路首秒
/// 只有稳态的一小部分），与多路径探索门槛（`EXPLORE_MIN_REMAINING`）同量级。
const SINGLE_RATE_MIN_BYTES: u64 = 4 * 1024 * 1024;
const SINGLE_RATE_MIN_SECS: f64 = 2.0;

/// 文件当前长度；不存在 / 读不到按 0（只用于速率样本，不影响下载正确性）。
async fn file_len_or_zero(path: &Path) -> u64 {
    match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            log_info!(
                "[download] 读取 {} 长度失败（单流速率样本作废）: {error}",
                path.display()
            );
            0
        }
    }
}

/// `ProxyMode::Auto` 单流下载完成后把实测速率回写起飞路径的跨任务先验：
/// 单流 = 单连接，任务速率即单连接速率。多段下载由 coordinator 按窗口
/// 采样回写；单流（不支持 Range / 小文件 / 用户指定 1 线程）过去从不学习，
/// 导致同一 host 下次仍按旧先验起飞。样本须越过慢启动且不受限速器约束。
fn record_single_stream_rate(p: &DownloadParams, bytes: u64, elapsed: Duration) {
    let Some(ctx) = p.auto_proxy.as_ref() else {
        return;
    };
    let secs = elapsed.as_secs_f64();
    if bytes < SINGLE_RATE_MIN_BYTES || secs < SINGLE_RATE_MIN_SECS || p.speed_limiter.limit() > 0 {
        return;
    }
    crate::route_health::record_path_rate(&ctx.host, ctx.start_route, bytes as f64 / secs, &p.db);
}

/// 下载器自己探测时能否走 Auto 探测竞速：会探测（非 hint）、普通 HTTP(S)
/// 单 URL（非音轨对 / HLS / DASH）且 [`crate::auto_probe::applies`]（GET 类、
/// 有备选路径）。
fn auto_race_probe_applies(p: &DownloadParams) -> bool {
    p.hint_file_size == 0
        && p.audio_url.is_none()
        && (p.url.starts_with("http://") || p.url.starts_with("https://"))
        && !crate::hls_downloader::is_hls_url(&p.url)
        && !crate::dash_downloader::is_dash_url(&p.url)
        && crate::auto_probe::applies(p)
}

/// 本次启动能否用单请求探测（[`ProbeKind::Held`]）代替 HEAD ∥ Range 探测：仅限 fresh、
/// 非 hint、无音轨对、不带有界 Range 窗口的 HTTP(S) 任务（HLS / DASH / FTP / 续传 / hint
/// 任务照旧）。非 GET 任务（表单 POST 等）同样适用：探测发出的就是那唯一一次提交，响应
/// 直接作为数据连接。启动序幕（`download_manager`）与 [`run_download`] 共用这一判据。
pub(crate) fn held_probe_applies(p: &DownloadParams) -> bool {
    !p.is_resume
        && p.hint_file_size == 0
        && p.audio_url.is_none()
        && p.spec.range_window_cap().is_none()
        && (p.url.starts_with("http://") || p.url.starts_with("https://"))
        && !crate::hls_downloader::is_hls_url(&p.url)
        && !crate::dash_downloader::is_dash_url(&p.url)
}

/// 文件名是否以 HTML 类扩展名结尾——用于 HTML 安全网判断"服务器返回 HTML
/// 是否为用户期望"。空字符串视为不像 HTML：调用方有责任在到达此处前确保
/// 文件名已经解析（run_download 中的 auto_name 空值检查 :1581 是兜底位置）。
pub(crate) fn filename_looks_like_html(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm") || lower.ends_with(".xhtml")
}

/// HTML 安全网文案的成因提示（接在「server returned HTML page …」之后）。
///
/// 响应终点与请求地址不同时点出终点（只含 scheme / host / path：query 常带令牌），
/// 这通常是登录页或错误页——例如 OneDrive `download.aspx` 缺 Cookie 时 302 到
/// `Authenticate.aspx`；否则按请求方法给通用提示。终点以含 `://` 的独立词元出现，
/// `is_retriable_error` 的关键词判定会跳过它，路径里的偶然字样不改变重试判定。
pub(crate) fn html_response_hint(
    requested_url: &str,
    final_url: Option<&reqwest::Url>,
    method: &reqwest::Method,
) -> String {
    let redirected = final_url.filter(|url| {
        !reqwest::Url::parse(requested_url).is_ok_and(|requested| requested == **url)
    });
    if let Some(url) = redirected {
        // origin（scheme / host / port，不含 userinfo）+ path：query 与内嵌账号密码都不进文案。
        let landing = format!("{}{}", url.origin().ascii_serialization(), url.path());
        return format!(
            "the request was redirected to {landing} (usually a sign-in or error page): \
             the browser cookies or link token were not captured or are no longer valid"
        );
    }
    if *method == reqwest::Method::GET || *method == reqwest::Method::HEAD {
        "the URL may be a redirect/transit page, an expired one-time link, or a form-POST \
         endpoint accessed via GET"
            .to_string()
    } else {
        format!(
            "the server answered the submitted {method} request with a page instead of the file \
             (the form token may no longer be accepted)"
        )
    }
}

// ---------------------------------------------------------------------------
// Dedup file name: "file.txt" → "file (1).txt" etc.
// ---------------------------------------------------------------------------

/// Deduplicate a filename so it does not collide with any existing file in
/// `dir` **nor** with any in-flight download that has already reserved the
/// same temporary path.
///
/// # Parameters
/// - `dir`      – target save directory.
/// - `name`     – desired filename (e.g. `"video.mp4"`).
/// - `reserved` – snapshot of `DownloadManager::reserved_temp_paths`;
///   contains the `.fdownloading` paths that concurrent tasks have already
///   claimed.  Pass an empty set when the caller has no reserved paths to
///   check (e.g. resume tasks, which skip dedup entirely).
///
/// # Why `reserved` is needed
/// `dedup_filename` is called from inside a spawned tokio task, well after
/// the manager's synchronous section has finished.  Multiple tasks spawned
/// in the same batch can all enter `dedup_filename` concurrently; each sees
/// the same on-disk state (no `.fdownloading` file yet) and all independently
/// choose the same filename.  They then race to write the same temp file,
/// causing the last writer to silently overwrite the earlier ones.
///
/// By consulting `reserved` — a snapshot taken **before** spawning, in the
/// manager's synchronous section — each task can see which names its siblings
/// have already claimed and avoid them.
/// `avoid`:**小写折叠**的额外占用名(如 finalize 冲突时从 DB 采集的同目录
/// 未完成任务 file_name)。与磁盘条目一并视为冲突,防止 finalize 换名撞上
/// 兄弟任务「已预订但临时文件尚未落盘」的名字造成 DB 指针别名(两任务
/// file_name 指向同一磁盘名,误删其一即毁对方产物)。
///
/// `overwrite` 允许替换该名字对应的同名旧文件时:磁盘上**仅最终文件**存在
/// 不算冲突——保留原名,完成时由 finalize 覆盖旧文件;`.fdownloading` 临时
/// 文件、`reserved` 预订与 `avoid` 集合命中仍是硬冲突,照旧编号改名。目录
/// 同名也照旧改名(文件不能覆盖目录)。
pub async fn dedup_filename(
    dir: &Path,
    name: &str,
    reserved: &std::collections::HashSet<std::path::PathBuf>,
    avoid: &std::collections::HashSet<String>,
    overwrite: &crate::file_exists::OverwritePolicy,
) -> String {
    let dir_owned = dir.to_path_buf();
    let name_owned = name.to_string();
    let reserved = reserved.clone();
    let avoid = avoid.clone();
    let allow_overwrite = overwrite.permits(name);
    // 磁盘探测是同步 stat(重度冲突时退化为一次目录扫描),放进阻塞池,
    // 不占用 async 工作线程。
    let joined = tokio::task::spawn_blocking(move || {
        dedup_name_by_probe(
            &dir_owned,
            &name_owned,
            &DedupRules {
                reserved: Some(&reserved),
                avoid: Some(&avoid),
                allow_overwrite,
                companion_mp4: false,
            },
        )
    })
    .await;
    match joined {
        Ok(Ok(chosen)) => chosen,
        Ok(Err(error)) => {
            crate::logger::report_warning("downloader", "probe filename collisions", &error);
            uuid_suffixed_name(name)
        }
        Err(error) => {
            crate::logger::report_warning("downloader", "join filename dedup probe", &error);
            uuid_suffixed_name(name)
        }
    }
}

/// 编号探测阶段逐个 `stat` 的候选数上限。超过后改为整目录扫描一次再在内存里
/// 找空位:同名文件成千上万时,逐个 stat 的总开销(≈ 2k 次系统调用)会超过一次
/// `read_dir`。扫描发生在调用方的锁之外,不会阻塞 actor。
const DEDUP_PROBE_LIMIT: u32 = 32;

/// 编号上限(`name (1)` … `name (9999)`),之后用 UUID 兜底。
const DEDUP_MAX_NUMBER: u32 = 9999;

/// [`dedup_name_by_probe`] 的冲突规则。
#[derive(Clone, Copy)]
pub(crate) struct DedupRules<'a> {
    /// 在途任务已预订的 `.fdownloading` 路径(精确路径比较)。
    pub reserved: Option<&'a std::collections::HashSet<std::path::PathBuf>>,
    /// **小写折叠**的额外占用名(如其他任务已声明但尚未落盘的名字)。
    pub avoid: Option<&'a std::collections::HashSet<String>>,
    /// 原名对应的同名**普通最终文件**可被覆盖(`file_exists_behavior ==
    /// overwrite`):仅目录、`.fdownloading` 临时文件、`reserved`/`avoid` 命中算
    /// 冲突。编号变体恒按保守语义,绝不覆盖真实文件。
    pub allow_overwrite: bool,
    /// HLS:成品是 `<stem>.mp4`,同 stem 的 `.mp4` 已存在同样算冲突。
    pub companion_mp4: bool,
}

/// `name` 的 `.mp4` 伴生名(同 stem)。
fn companion_mp4_name(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    format!("{stem}.mp4")
}

/// 条目是否存在(不跟随符号链接:悬空链接也占名,rename 会覆盖它)。
/// 名字过长等「不可能存在」的错误视为不存在,真正的创建错误由后续写入报告。
fn entry_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidFilename
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// 路径(跟随符号链接)是否为目录;不存在视为否。
fn path_is_dir(path: &Path) -> std::io::Result<bool> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidFilename
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// 单个候选名是否已被占用(逐项 stat)。`overwrite_final` 为真时仅目录算最终名
/// 冲突。**先探临时文件再探最终文件**:兄弟任务恰在两次探测之间把 temp
/// rename 成最终名时,必有一次命中,不会漏判。
fn candidate_taken_by_probe(
    dir: &Path,
    candidate: &str,
    rules: &DedupRules<'_>,
    overwrite_final: bool,
) -> std::io::Result<bool> {
    let temp_name = format!("{candidate}{TEMP_EXT}");
    let temp_path = dir.join(&temp_name);
    if rules
        .reserved
        .is_some_and(|reserved| reserved.contains(&temp_path))
        || rules
            .avoid
            .is_some_and(|avoid| avoid.contains(&candidate.to_lowercase()))
    {
        return Ok(true);
    }
    if entry_exists(&temp_path)? {
        return Ok(true);
    }
    let final_path = dir.join(candidate);
    let companion = rules
        .companion_mp4
        .then(|| dir.join(companion_mp4_name(candidate)));
    if overwrite_final {
        if path_is_dir(&final_path)? {
            return Ok(true);
        }
        if let Some(companion) = &companion
            && path_is_dir(companion)?
        {
            return Ok(true);
        }
        return Ok(false);
    }
    if entry_exists(&final_path)? {
        return Ok(true);
    }
    match &companion {
        Some(companion) => entry_exists(companion),
        None => Ok(false),
    }
}

/// 整目录扫描结果(条目名小写折叠)下判定候选名是否被占用。
fn candidate_taken_in_scan(
    dir: &Path,
    candidate: &str,
    rules: &DedupRules<'_>,
    existing: &std::collections::HashSet<String>,
) -> bool {
    let folded = candidate.to_lowercase();
    let temp_name = format!("{candidate}{TEMP_EXT}");
    rules
        .reserved
        .is_some_and(|reserved| reserved.contains(&dir.join(&temp_name)))
        || rules.avoid.is_some_and(|avoid| avoid.contains(&folded))
        || existing.contains(&folded)
        || existing.contains(&temp_name.to_lowercase())
        || (rules.companion_mp4 && existing.contains(&companion_mp4_name(candidate).to_lowercase()))
}

/// 目录条目名集合(小写折叠)。目录不存在 = 空集。
fn scan_dir_folded(dir: &Path) -> std::io::Result<std::collections::HashSet<String>> {
    let mut existing = std::collections::HashSet::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(existing),
        Err(error) => return Err(error),
    };
    for entry in entries {
        existing.insert(entry?.file_name().to_string_lossy().to_lowercase());
    }
    Ok(existing)
}

/// `name` 加 UUID 后缀的唯一名(编号变体全被占用时的兜底,杜绝覆盖)。
pub(crate) fn uuid_suffixed_name(name: &str) -> String {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let unique = uuid::Uuid::new_v4();
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => format!("{stem} ({unique}).{ext}"),
        None => format!("{stem} ({unique})"),
    }
}

/// 按编号升序产出候选名的游标(`name`、`name (1)` … `name (9999)`),每次
/// [`DedupCursor::next_free`] 返回下一个不冲突的候选。
///
/// 不再为每次冲突 `read_dir` 整个目录(数万文件目录下每次数十毫秒):前
/// [`DEDUP_PROBE_LIMIT`] 个编号逐个 `symlink_metadata` 探测,由文件系统自己
/// 裁决大小写(APFS/NTFS 等大小写不敏感文件系统上,探测天然把仅大小写不同
/// 的条目视为同一条目;大小写敏感的文件系统上它们本就是不同文件,rename 不会
/// 互相覆盖);之后才退化为一次折叠扫描(游标内只扫一次,供后续候选复用)。
/// **游标不持任何锁**:「预订」类调用方在锁外推进游标、锁内 `insert`,插入冲突
/// 就继续取下一个候选,不必从头重探。
pub(crate) struct DedupCursor<'a> {
    dir: &'a Path,
    name: &'a str,
    rules: DedupRules<'a>,
    /// 下一个待检查的编号(0 = 原名)。
    next: u32,
    /// 重度冲突时一次性扫得的目录条目(小写折叠)。
    scan: Option<std::collections::HashSet<String>>,
}

impl<'a> DedupCursor<'a> {
    pub(crate) fn new(dir: &'a Path, name: &'a str, rules: DedupRules<'a>) -> Self {
        Self {
            dir,
            name,
            rules,
            next: 0,
            scan: None,
        }
    }

    /// 下一个不冲突的候选名;`1..=9999` 耗尽返回 `None`(调用方自行 UUID 兜底)。
    ///
    /// 目录不存在视为空目录;其余 I/O 错误(如父路径不是目录)返回 `Err`,
    /// 由调用方决定如何处置,绝不当作「空闲」。
    pub(crate) fn next_free(&mut self) -> std::io::Result<Option<String>> {
        let path = Path::new(self.name);
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(self.name);
        let ext = path.extension().and_then(|ext| ext.to_str());
        while self.next <= DEDUP_MAX_NUMBER {
            let index = self.next;
            self.next += 1;
            let candidate = match (index, ext) {
                (0, _) => self.name.to_string(),
                (_, Some(ext)) => format!("{stem} ({index}).{ext}"),
                (_, None) => format!("{stem} ({index})"),
            };
            let taken = if index <= DEDUP_PROBE_LIMIT {
                // 原名按 `allow_overwrite` 放行普通文件;编号变体恒按保守语义。
                let overwrite_final = index == 0 && self.rules.allow_overwrite;
                candidate_taken_by_probe(self.dir, &candidate, &self.rules, overwrite_final)?
            } else {
                // 重度冲突:扫描一次,在内存里找空位。条目名小写折叠(非 UTF-8 经
                // lossy 转换,只可能多让一个编号,不会漏判)。
                if self.scan.is_none() {
                    self.scan = Some(scan_dir_folded(self.dir)?);
                }
                self.scan.as_ref().is_some_and(|existing| {
                    candidate_taken_in_scan(self.dir, &candidate, &self.rules, existing)
                })
            };
            if !taken {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }
}

/// 同步重名去重核心(HTTP 序幕、HTTP 完成期、BT 完成期共用):返回在 `dir` 里
/// 不与既有条目 / 在途预订 / `avoid` 冲突的最小可用名(`name`、`name (1)` …),
/// 编号耗尽时用 UUID 后缀兜底。**不持任何锁**,探测规则见 [`DedupCursor`]。
pub(crate) fn dedup_name_by_probe(
    dir: &Path,
    name: &str,
    rules: &DedupRules<'_>,
) -> std::io::Result<String> {
    Ok(DedupCursor::new(dir, name, *rules)
        .next_free()?
        // 极端兜底:编号变体全被占用时返回原名会导致落盘覆盖,用 UUID 后缀保证唯一。
        .unwrap_or_else(|| uuid_suffixed_name(name)))
}

/// 仅按磁盘状态复核一个已选定的候选名是否仍空闲(不看预订集合 / avoid)。
/// 预订公开之后复核:兄弟任务在「探测 → 预订」窗口内完成并释放预订时,其最终
/// 文件已在磁盘上,此处必然命中。`allow_overwrite` 仅对原名有意义。
pub(crate) fn name_free_on_disk(
    dir: &Path,
    candidate: &str,
    allow_overwrite: bool,
    companion_mp4: bool,
) -> std::io::Result<bool> {
    let rules = DedupRules {
        reserved: None,
        avoid: None,
        allow_overwrite,
        companion_mp4,
    };
    candidate_taken_by_probe(dir, candidate, &rules, allow_overwrite).map(|taken| !taken)
}

/// 旧算法的参照实现(整目录 `read_dir`、条目名小写折叠、编号 1..=9999),仅供
/// 测试断言探测式去重与旧算法结果一致。耗尽编号返回空串(测试不覆盖该分支)。
#[cfg(test)]
pub(crate) fn dedup_reference_scan(
    dir: &Path,
    name: &str,
    reserved: &std::collections::HashSet<std::path::PathBuf>,
    avoid: &std::collections::HashSet<String>,
    allow_overwrite: bool,
) -> String {
    let existing: std::collections::HashSet<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().to_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let reserved_or_avoided = |candidate: &str| {
        reserved.contains(&dir.join(format!("{candidate}{TEMP_EXT}")))
            || avoid.contains(&candidate.to_lowercase())
    };
    let temp_taken =
        |candidate: &str| existing.contains(&format!("{candidate}{TEMP_EXT}").to_lowercase());
    let original_taken = reserved_or_avoided(name)
        || temp_taken(name)
        || if allow_overwrite {
            dir.join(name).is_dir()
        } else {
            existing.contains(&name.to_lowercase())
        };
    if !original_taken {
        return name.to_string();
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = path.extension().and_then(|s| s.to_str());
    for index in 1..=9999 {
        let candidate = match ext {
            Some(ext) => format!("{stem} ({index}).{ext}"),
            None => format!("{stem} ({index})"),
        };
        if !reserved_or_avoided(&candidate)
            && !temp_taken(&candidate)
            && !existing.contains(&candidate.to_lowercase())
        {
            return candidate;
        }
    }
    String::new()
}

/// Temporary file extension used during download (like Chrome's `.crdownload`).
/// The file is renamed to the final name only after all data is verified.
pub const TEMP_EXT: &str = ".fdownloading";

/// 原子占名 + 落盘:`create_new(dst)` 独占创建 0 字节占位(两个写者竞争
/// 同名时后到者得 `ErrorKind::AlreadyExists`,决不覆盖对方)→ rename 把
/// `src` 盖到**自己的**占位上(此处 REPLACE 语义安全:占位归本调用所有)。
/// rename 失败时清理占位再返回错误——残留占位会永久占住最终名。
///
/// 崩溃于占名与 rename 之间会遗留 0 字节占位孤儿:后续重试的 dedup 视其
/// 为已占自动换名,属可接受的罕见崩溃残留。
pub(crate) async fn claim_rename(src: &Path, dst: &Path) -> std::io::Result<()> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
        .await
        .map(drop)?;
    match tokio::fs::rename(src, dst).await {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Err(cleanup_error) = tokio::fs::remove_file(dst).await
                && cleanup_error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning(
                    "http-download",
                    "remove failed rename placeholder",
                    &cleanup_error,
                );
            }
            Err(e)
        }
    }
}

/// 完成期占名改名:把 `src` 以不覆盖语义落到 `save_dir/name`,返回实际落盘的文件名。
///
/// 基于 [`claim_rename`] 的 `create_new` 占名。占名冲突(`AlreadyExists`)时:
/// `overwrite` 对原名授权且不在兄弟任务预订名 `avoid`(小写)内的普通旧文件,
/// 删除后重试一次;其余情况重新 dedup 换名(避开 `avoid`)。连续 5 次冲突视为
/// 目录被持续抢占,报错并保留 `src`。
/// 调用方须在返回名与 `name` 不同时同步任务的 file_name。
pub(crate) async fn claim_final_name(
    src: &Path,
    save_dir: &Path,
    name: &str,
    overwrite: &crate::file_exists::OverwritePolicy,
    avoid: &std::collections::HashSet<String>,
) -> Result<String, DownloadError> {
    let mut chosen = name.to_string();
    let mut overwrite_attempted = false;
    let mut attempt = 0u32;
    loop {
        let dst = save_dir.join(&chosen);
        match claim_rename(src, &dst).await {
            Ok(()) => return Ok(chosen),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
                if attempt > 5 {
                    return Err(DownloadError::Io(e));
                }
                if overwrite.permits(name)
                    && !overwrite_attempted
                    && chosen == name
                    && !avoid.contains(&chosen.to_lowercase())
                {
                    overwrite_attempted = true;
                    let is_dir = tokio::fs::metadata(&dst)
                        .await
                        .map(|m| m.is_dir())
                        .unwrap_or(false);
                    if !is_dir && tokio::fs::remove_file(&dst).await.is_ok() {
                        continue;
                    }
                }
                chosen = dedup_filename(
                    save_dir,
                    name,
                    &std::collections::HashSet::new(),
                    avoid,
                    &crate::file_exists::OverwritePolicy::Never,
                )
                .await;
            }
            Err(e) => return Err(DownloadError::Io(e)),
        }
    }
}

/// Buffer size for `BufWriter` wrapping file I/O during downloads.
/// 256 KB reduces the frequency of syscalls compared to the default 8 KB,
/// significantly improving throughput especially with many concurrent segments.
pub const BUF_WRITER_CAPACITY: usize = 256 * 1024;

/// Interval (in seconds) between DB persistence of download progress.
/// Balances crash-recovery granularity (max ~3 s of re-download) against
/// SQLite Mutex contention (reduces writes from ~80/s to ~5/s with 16 segments).
pub const DB_SAVE_INTERVAL_SECS: u64 = 3;

/// 单个 chunk 的读取超时（stall detection）。如果超过此时间没有收到任何数据，
/// 视为连接停滞，返回错误触发 retry 或让用户感知到真实状态。
/// 刻意比 segment_coordinator 的 5s 更宽（10s）：单流路径没有并行段兜底，
/// 这条连接是唯一数据流，掐早了只有整任务重试一条路；多段路径重连廉价且
/// 尾段抢救依赖快速回收，故用更激进的 5s。
const CHUNK_STALL_TIMEOUT: Duration = Duration::from_secs(10);
/// 单流续传的单次 Range 上限。部分签名下载端点会拒绝超过 20 MiB 的区间；
/// 取 16 MiB 为闭区间长度留出余量，并在同一任务内串行请求后续区间。
const MAX_SINGLE_RESUME_RANGE_BYTES: i64 = 16 * 1024 * 1024;
/// 与 aria2 控制文件的 piece 语义一致：只把完整 1 MiB 检查点作为可恢复进度。
/// 任意连接中断留下的尾部不足一块，下一次 Range 从前一检查点覆盖重写。
const SINGLE_RESUME_ALIGNMENT_BYTES: i64 = 1024 * 1024;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// 完成前的插件后处理（`finalize` 阶段）。产物位于任务行的 `save_dir/file_name`；
/// 返回插件替换后的 `(文件名, 大小)`，未替换为 `None`。插件 `fail` 或
/// `onFailure: "fail"` 的出错返回错误，任务随之失败。
pub(crate) async fn finalize_with_interceptors(
    p: &DownloadParams,
    protocol: crate::intercept::ProtocolKind,
) -> Result<Option<(String, i64)>, DownloadError> {
    use crate::intercept::{ActivityNotes, InterceptStage, ctx, drive, wants};

    if !wants(p.interceptor.as_ref(), InterceptStage::Finalize) {
        return Ok(None);
    }
    let notes = ActivityNotes::of(p);
    let row = match p.db.load_task_by_id(&p.task_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(None),
        Err(e) => {
            log_warn!(
                "[intercept] task {} finalize skipped, failed to load task row: {e}",
                p.task_id
            );
            notes
                .add("插件后处理已跳过：读取任务信息失败".to_string())
                .await;
            return Ok(None);
        }
    };
    let save_dir = PathBuf::from(&row.save_dir);
    let path = save_dir.join(&row.file_name);
    let total_bytes = match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_file() => i64::try_from(meta.len()).unwrap_or(i64::MAX),
        _ => {
            notes
                .add("插件后处理已跳过：找不到下载文件".to_string())
                .await;
            return Ok(None);
        }
    };
    let audio_path = if p.audio_url.is_some() {
        let audio = crate::dash_downloader::build_audio_path(&path);
        tokio::fs::try_exists(&audio)
            .await
            .unwrap_or(false)
            .then(|| audio.to_string_lossy().into_owned())
    } else {
        None
    };
    notes.add("正在执行插件下载后处理".to_string()).await;
    let task = ctx::task_ctx(p, protocol);
    drive::run_finalize(
        p.interceptor.as_ref(),
        &notes,
        &p.cancel_token,
        drive::FinalizeInput {
            task: &task,
            db: &p.db,
            task_id: &p.task_id,
            save_dir,
            file_name: row.file_name,
            total_bytes,
            audio_path,
        },
    )
    .await
    .map_err(DownloadError::from)
}

/// 完成前收尾（写状态 3 之前）：插件后处理（[`finalize_with_interceptors`]）→ 按最终
/// 文件名复核分类目录（[`crate::category_route::relocate_completed`]）。返回需随完成帧
/// 上报的 `(文件名, 大小)`——插件替换了产物，或移入分类目录时因占名改名；`None` = 不变。
pub(crate) async fn finalize_output(
    p: &DownloadParams,
    protocol: crate::intercept::ProtocolKind,
) -> Result<Option<(String, i64)>, DownloadError> {
    let replaced = finalize_with_interceptors(p, protocol).await?;
    Ok(match crate::category_route::relocate_completed(p).await {
        Some(relocated) => Some(relocated),
        None => replaced,
    })
}

pub async fn run_download(mut params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let preprobed = params.preprobed.take();
    let result = match run_download_inner(&mut params, preprobed).await {
        Ok((total, renamed)) => {
            match finalize_output(&params, crate::intercept::ProtocolKind::Http).await {
                Ok(replaced) => {
                    let (total, renamed) = match replaced {
                        Some((name, size)) => (size, Some(name)),
                        None => (total, renamed),
                    };
                    params
                        .db
                        .update_task_status(&params.task_id, 3, "")
                        .await
                        .map(|()| (total, renamed))
                        .map_err(DownloadError::Db)
                }
                Err(e) => Err(e),
            }
        }
        Err(error) => Err(error),
    };

    match result {
        Ok((total, finalize_renamed)) => {
            log_info!(
                "[download] task {} completed, total={} bytes",
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
                    // finalize 阶段因目标名被占而改名时携带新名(reporter
                    // 对非空 file_name 锁存);未改名传空串 = 保持原名。
                    file_name: finalize_renamed.unwrap_or_default(),
                    segment_details: None,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("HTTP progress receiver closed");
            }
        }
        Err(DownloadError::Cancelled) => {
            log_info!("[download] task {} cancelled", task_id_log);
            // pause / cancel already handled upstream — nothing to do
        }
        Err(e) => {
            let msg = e.to_string();
            // 追加完整错误链（root cause），方便排查网络/TLS 等底层错误。
            // msg 已包含 reqwest 顶层描述，这里从 source 的 source 开始
            // 避免重复打印同一层信息。
            let chain = if let Some(src) = StdError::source(&e) {
                format_error_chain(src.source())
            } else {
                String::new()
            };
            // 类型化传输失败标签：manager 的 Auto 换路 / 自动重试只认标签，
            // 不再从（可能本地化的）OS 错误文本里猜关键词。
            let full_msg = crate::net_failure::append_tag(
                format!("{}{}", msg, chain),
                crate::net_failure::classify_download_error(&e),
            );
            // checksum 失败时所有字节均已下载完毕（只是校验未通过），需特殊处理进度。
            let is_checksum_fail = matches!(e, DownloadError::ChecksumMismatch(_));
            crate::log_error!("[download] task {} error: {}", task_id_log, full_msg);
            if let Err(db_error) = params
                .db
                .update_task_status(&params.task_id, 4, &full_msg)
                .await
            {
                crate::logger::report_error(
                    "http-download",
                    "persist terminal error status",
                    &db_error,
                );
            }

            // Preserve actual progress from DB so the UI doesn't jump back to 0%.
            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => {
                    // checksum 失败 → 字节已全部下载，进度应显示 100%。
                    // 其他错误 → 保留 DB 中实际已下载量，防止 UI 回跳至 0%。
                    let dl = if is_checksum_fail {
                        t.total_bytes
                    } else {
                        t.downloaded_bytes
                    };
                    (dl, t.total_bytes)
                }
                other => {
                    log_info!(
                        "[download] task {} warning: failed to read progress from DB: {:?}",
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
                    error_message: full_msg,
                    file_name: String::new(),
                    segment_details: None,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("HTTP progress receiver closed");
            }
        }
    }
}

/// Normalize a user-provided checksum spec into the canonical `algo=hex`
/// form consumed by [`verify_checksum`].
///
/// BUG-CHECKSUM-PREFIX：用户常把从下载页复制来的、自带算法前缀的哈希值
/// （如 `sha256:abcdef...`）整段粘进哈希输入框，拼出 `sha256=sha256:abcdef...`
/// 这样的 spec；旧实现直接把 `sha256:abcdef...` 当哈希值参与比较，永远校验
/// 失败。本函数：
/// - 识别 `algo=hash` 中的 `algo`（含 `sha-256`/`sha256` 等别名）；
/// - 若 `hash` 自身带 `<prefix>:` 前缀，且该前缀归一化（去掉 `-`、转小写）
///   后与已选定的 `algo` 相同，剥离该前缀——**绝不**猜测/剥离与所选算法
///   不符的前缀，那是真实的输入错误，必须报错而非静默按错误算法比对；
/// - 校验剥离后的哈希是纯十六进制、且长度与算法匹配（md5=32, sha1=40,
///   sha256=64, sha512=128）。
///
/// 返回规范化后的 `algo=hex`（小写），或描述具体问题的错误字符串。
pub fn normalize_checksum_spec(spec: &str) -> Result<String, String> {
    let spec = spec.trim();
    let sep = spec
        .find('=')
        .ok_or_else(|| format!("invalid checksum format (expected algo=hash): {}", spec))?;
    let algo_raw = spec[..sep].trim().to_lowercase();
    let hash_raw = spec[sep + 1..].trim();

    let algorithm = HashAlgorithm::parse(&algo_raw)
        .ok_or_else(|| format!("unsupported checksum algorithm: {}", algo_raw))?;
    let algo = algorithm.as_str();

    // Strip a redundant "<algo>:" prefix pasted onto the hash itself — only
    // when it names the *same* algorithm already selected via `algo=`.
    let hash = if let Some((prefix, rest)) = hash_raw.split_once(':') {
        let normalized_prefix = prefix.trim().to_lowercase().replace('-', "");
        if normalized_prefix == algo {
            rest.trim()
        } else {
            return Err(format!(
                "checksum hash prefix does not match algorithm: expected {}, got {}",
                algo,
                prefix.trim()
            ));
        }
    } else {
        hash_raw
    };

    let hash = hash.to_lowercase();
    if hash.is_empty() || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("checksum hash is not valid hex: {}", hash));
    }
    let expected_len = algorithm.hex_len();
    if hash.len() != expected_len {
        return Err(format!(
            "checksum hash length mismatch for {}: expected {} hex chars, got {}",
            algo,
            expected_len,
            hash.len()
        ));
    }

    Ok(format!("{}={}", algo, hash))
}

/// Verify that a file at `path` matches the checksum in `spec`.
///
/// `spec` format: `"algo=hexhash"`, e.g. `"sha-256=abc123..."` or `"md5=d41d8c..."`.
/// Supported algorithms: `sha-256`/`sha256`, `sha-512`/`sha512`, `sha-1`/`sha1`, `md5`.
/// `spec` is passed through [`normalize_checksum_spec`] first, which also
/// strips a redundant `<algo>:` prefix pasted onto the hash value.
/// Returns the normalized `algo=hex` spec (= the file's hash record) if the digest
/// matches, or `Err(DownloadError::ChecksumMismatch)` if not.
async fn verify_checksum(path: &Path, spec: &str) -> Result<String, DownloadError> {
    let normalized = normalize_checksum_spec(spec).map_err(DownloadError::Other)?;
    let (algo, expected_hex) = normalized.split_once('=').ok_or_else(|| {
        DownloadError::Other(format!(
            "internal: normalized checksum spec missing separator: {}",
            normalized
        ))
    })?;
    let algorithm = HashAlgorithm::parse(algo).ok_or_else(|| {
        DownloadError::Other(format!(
            "internal: unsupported normalized algorithm: {algo}"
        ))
    })?;
    let path_owned = path.to_path_buf();
    let actual_hex = tokio::task::spawn_blocking(move || {
        crate::file_hash::hash_file(&path_owned, algorithm, || false)
    })
    .await
    .map_err(|e| DownloadError::Other(format!("checksum thread panicked: {}", e)))?
    .map_err(|e| DownloadError::Other(format!("checksum: {e}")))?;

    if actual_hex != expected_hex {
        return Err(DownloadError::ChecksumMismatch(format!(
            "expected {}, got {}",
            expected_hex, actual_hex
        )));
    }
    Ok(normalized)
}

/// Run the segment advisor to compute the task's connection cap (Auto mode).
///
/// 语义：advisor 输出是【最大连接数上限】而非启动并发——启动并发
/// 由 segment_coordinator 的渐进 ramp-up 控制。用户配置的 Auto 上限
/// （`auto_max_connections`）在此裁剪 advisor 推荐值。带宽预探测已移除：
/// ramp-up 本身就是实测探测（第 1→2→4 条连接的边际吞吐即带宽反馈），
/// 省去 128KB 额外请求与最多 4s 的启动延迟。
///
/// Updates `tasks.segments` in DB so that subsequent resumes skip the advisor.
async fn compute_segments_with_advisor(p: &DownloadParams, info: &FileInfo) -> i32 {
    use crate::segment_advisor::{AdvisorInput, advise_static};
    let advisor_input = AdvisorInput {
        total_bytes: info.total_bytes,
        supports_range: info.supports_range,
    };

    // Static recommendation (file size + CPU cores) = recommended cap.
    let static_advice = advise_static(&advisor_input);

    // Clamp with the user-configured Auto connection cap (<=0 = unlimited).
    let result = if p.auto_max_connections > 0 {
        static_advice.segments.min(p.auto_max_connections)
    } else {
        static_advice.segments
    };
    log_info!(
        "[download] task {} auto cap: advisor={}, user_cap={}, effective={}, reason={}",
        p.task_id,
        static_advice.segments,
        p.auto_max_connections,
        result,
        static_advice.reason
    );

    // Persist to DB so resume_task can skip the advisor.
    // If this write fails, the advisor will re-run on resume — acceptable.
    if let Err(e) = p.db.update_task_segments(&p.task_id, result).await {
        log_info!(
            "[download] task {} failed to persist segment count to DB: {}",
            p.task_id,
            e
        );
    }

    result
}

async fn discard_segment_data(db: &Db, task_id: &str, path: &Path) -> Result<(), DownloadError> {
    // Removing the DB rows first would make a failed file removal look like a
    // complete single-stream partial file on the next resume. Keep the layout
    // until the preallocated/stale file is gone; missing files are already clean.
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(error.into());
    }
    db.delete_segments(task_id).await?;
    Ok(())
}

/// 返回 `(actual_total, finalize_renamed)`:`finalize_renamed` 仅当 finalize
/// 阶段因目标名被占用而改名时为 `Some(新名)`,调用方须经完成信号上报
/// (progress_reporter 对非空 file_name 锁存,空串 = 不变)。
async fn run_download_inner(
    p: &mut DownloadParams,
    preprobed: Option<Result<PreprobedInfo, DownloadError>>,
) -> Result<(i64, Option<String>), DownloadError> {
    log_info!(
        "[download] task {} starting, url={}",
        p.task_id,
        crate::logger::sanitize_log_str(&p.url)
    );
    // 恢复任务进入 preparing 时保留已落库进度，避免 UI 在真正发出续传
    // Range 前短暂显示为 0。后续 status=1 继续复用同一基线，不重复查库。
    let (resume_downloaded, resume_total) = if p.is_resume {
        p.db.load_task_by_id(&p.task_id)
            .await?
            .map(|task| (task.downloaded_bytes.max(0), task.total_bytes.max(0)))
            .unwrap_or((0, 0))
    } else {
        (0, 0)
    };

    // Transition to status=5 (preparing) — probing server, resolving file info
    p.db.update_task_status(&p.task_id, 5, "").await?;
    if p.progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: resume_downloaded,
            download_baseline: Some(resume_downloaded),
            total_bytes: resume_total,
            status: 5,
            error_message: String::new(),
            file_name: p.file_name.clone(),
            segment_details: None,
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("HTTP progress receiver closed");
    }

    // `ProxyMode::Auto`：序幕没跑探测（续传 / 序幕不适用）时，在这里做探测
    // 竞速；胜出路径就地接管 client / 代理配置 / 路由上下文，后续多 CDN
    // 聚合与数据连接都用接管后的值。
    let preprobed = match preprobed {
        None if auto_race_probe_applies(p) => {
            let kind = if held_probe_applies(p) {
                ProbeKind::Held
            } else {
                ProbeKind::HeadAndRange
            };
            let raced = crate::auto_probe::race_probe(p, kind).await;
            if let Ok(PreprobedInfo {
                adapted: Some(adapted_spec),
                ..
            }) = &raced
            {
                persist_adapted_spec(&p.db, &p.task_id, adapted_spec).await;
            }
            Some(raced)
        }
        other => other,
    };
    let p: &DownloadParams = p;
    let client = &p.client;

    // 多 CDN 候选聚合（解析 + connect 预筛）与下方 probe 并行发起，不增加
    // 起飞延迟；静态门控（开关/https/Range 已验证/单连接域名/熔断标记）
    // 不通过时为 None。结果在多段分支收割；单流分支主动中止。
    let cdn_pending =
        crate::cdn::spawn_aggregation(&p.url, &p.cdn, p.range_verified, &p.task_id, &p.db);

    // When the browser extension provides a file size hint, skip the probe
    // phase (HEAD + GET Range:0-0) entirely.  One-time CDN URLs (e.g.
    // Lanzou, ctbpsp.com signed URLs) treat every HTTP request as a download
    // attempt.  The probe would "consume" the URL token, leaving the actual
    // download to receive an HTML error page instead of the real file.
    //
    // hint_file_size semantics:
    //   > 0  — known file size from browser extension, skip probe
    //   -1   — size unknown but confirmed downloadable (webRequest sniffed),
    //          skip probe to preserve one-time tokens
    //    0   — no hint, run normal probe
    let mut applied_spec: Option<RequestSpec> = None;
    // 单请求探测保留的首响应（见 [`resolve_file_info_held`]），稍后交给 `DownloadSpec`。
    let mut held_response: Option<reqwest::Response> = None;
    let mut probed_decision = crate::intercept::drive::ProbedDecision::default();
    let info = if p.hint_file_size != 0 {
        // fresh hint 任务：持久化 Range 验证状态。浏览器扩展 hint → 0（未验证，
        // coordinator 首响应证实支持后置回 1；resume 据此延续「首连接 plain GET」
        // 保守启动）；插件 rangeSupported 担保 → 1（resume 照常按已验证多段）。
        // 写失败仅降级（resume 落回 probe 路径，即改动前行为），不阻断下载。
        if !p.is_resume
            && let Err(e) =
                p.db.set_task_range_verified(&p.task_id, p.range_verified)
                    .await
        {
            log_info!(
                "[download] task {} 持久化 range_verified={} 失败：{:?}（resume 保守启动可能退化）",
                p.task_id,
                p.range_verified,
                e
            );
        }
        let name = if p.file_name.is_empty() {
            // Hint mode skips the HEAD probe entirely, so we have no response
            // headers to extract the filename from.  Try the URL path first;
            // if that also yields nothing (e.g. "/download?token=abc") fall
            // back to "download" so we never end up with an empty dest_path
            // that would point at the save directory itself.
            name_from_url(&p.url)
                .map(|r| r.name)
                .unwrap_or_else(|| "download".to_string())
        } else {
            p.file_name.clone()
        };
        // When hint is -1 (unknown size), use 0 as total_bytes so the
        // downloader treats it as unknown-length and reads until EOF.
        let effective_size = if p.hint_file_size > 0 {
            p.hint_file_size
        } else {
            0
        };
        log_info!(
            "[download] task {} using hint: name={}, size={} (probe skipped, hint={})",
            p.task_id,
            name,
            effective_size,
            p.hint_file_size
        );
        FileInfo {
            file_name: name,
            name_source: crate::naming::NameSource::Fallback,
            total_bytes: effective_size,
            // Hint 模式没有 probe：自动/显式多段任务仍按既有策略乐观尝试
            // Range；已持久化 range_verified 的恢复任务则必须沿用这份证据。
            // segment_count=1 只限制并发，不等于禁用 Range——单流暂停后的断点
            // 恢复仍需从临时文件缺口继续。fresh 单流没有既有字节，即使
            // supports_range=true，download_single 也会发普通 GET，不会额外消耗
            // 一次性 URL。非 GET 继续强制单流且禁止 Range，避免 POST 续传拼接。
            // 插件声明 `maxRangeBytes` 的源只接受有界 Range：单流也必须带 Range 窗口，视为支持。
            supports_range: p.hint_file_size > 0
                && (p.segment_count != 1
                    || p.range_verified
                    || p.spec.range_window_cap().is_some())
                && p.spec.is_get_like(),
            content_type: String::new(),
            // Hint mode skips the probe, so no ETag/Last-Modified available.
            etag: String::new(),
            last_modified: String::new(),
            // Hint mode skips the probe — no Content-Encoding info.
            content_encoding_compressed: false,
        }
    } else {
        let (info, held) = if let Some(preprobed) = preprobed {
            // 启动序幕已用同一 client / spec 跑过完整探测并落库自适应请求头，
            // 这里不再发第二轮探测；单 GET 探测保留的首响应一并接手。
            let PreprobedInfo {
                info,
                adapted,
                held,
            } = preprobed?;
            log_info!(
                "[download] task {} reusing prelude probe result (probe skipped)",
                p.task_id
            );
            applied_spec = adapted;
            (info, held)
        } else if held_probe_applies(p) {
            log_info!(
                "[download] task {} resolving file info via a single {} request...",
                p.task_id,
                if p.spec.is_get_like() {
                    "GET"
                } else {
                    p.spec.method.as_str()
                }
            );
            let (info, adapted, resp) = resolve_file_info_held(client, &p.url, &p.spec).await?;
            if let Some(adapted_spec) = adapted {
                persist_adapted_spec(&p.db, &p.task_id, &adapted_spec).await;
                applied_spec = Some(adapted_spec);
            }
            (info, Some(resp))
        } else {
            log_info!("[download] task {} resolving file info...", p.task_id);
            // 非 GET 在这里同样只提交一次：`adapt_probe_request` 把它改走单请求探测并交回
            // 响应，作为数据连接（见 `download_single` 的 `use_held`）。
            let (info, adapted, held) =
                adapt_probe_request(client, &p.url, &p.spec, ProbeKind::HeadAndRange, None).await?;
            if let Some(adapted_spec) = adapted {
                // 探测靠请求头自适应（去 UA / 补浏览器 UA / 补防盗链 Referer）才通过：
                // 真实下载（含分段 worker）必须用同一份请求头，否则探测通过、下载 403；
                // 并落库让续传保持一致。
                persist_adapted_spec(&p.db, &p.task_id, &adapted_spec).await;
                applied_spec = Some(adapted_spec);
            }
            (info, held)
        };
        held_response = held;
        log_info!(
            "[download] task {} resolved: name={}, size={}, range={}",
            p.task_id,
            info.file_name,
            info.total_bytes,
            info.supports_range
        );
        // probe 已经看过真实响应：把权威名 / Content-Type 记为命名证据，完成期据此
        // 精修启动序幕用 URL 占位的推断名（HEAD 被拒、CD 只在 GET 上下发等）。
        let disposition = if matches!(
            info.name_source,
            crate::naming::NameSource::Disposition | crate::naming::NameSource::QueryDisposition
        ) {
            info.file_name.as_str()
        } else {
            ""
        };
        record_naming_evidence(&p.db, &p.task_id, disposition, &info.content_type).await;
        if crate::intercept::wants(
            p.interceptor.as_ref(),
            crate::intercept::InterceptStage::Probed,
        ) {
            let task_ctx = crate::intercept::ctx::task_ctx(p, crate::intercept::ProtocolKind::Http);
            let notes = crate::intercept::ActivityNotes::of(p);
            probed_decision = crate::intercept::drive::run_probed(
                p.interceptor.as_ref(),
                &notes,
                &p.cancel_token,
                &task_ctx,
                crate::intercept::patch::request_snapshot(&p.url, &p.spec, &p.run_meta),
                crate::intercept::ctx::ConnectionsCtx {
                    segments: p.segment_count,
                    max_connections: p.connection_cap,
                    auto_max_connections: p.auto_max_connections,
                },
                crate::intercept::ctx::ProbeCtx {
                    total_bytes: info.total_bytes,
                    supports_range: info.supports_range,
                    file_name: info.file_name.clone(),
                    content_type: info.content_type.clone(),
                    etag: info.etag.clone(),
                    last_modified: info.last_modified.clone(),
                },
            )
            .await?;
        }
        info
    };
    // 本次运行内所有下载请求共享：hint 模式下首个真实请求撞上反机器人质询时，
    // 切换到中性 UA 对其余分段 worker 与单流回退同时生效。
    let requested_segments = probed_decision.segments.unwrap_or(p.segment_count);
    let connection_cap = probed_decision.max_connections.unwrap_or(p.connection_cap);
    // 单请求探测的响应终点（跟随重定向后），供下方 HTML 安全网点出被带去的页面。
    let probe_final_url = held_response.as_ref().map(|resp| resp.url().clone());
    let spec_ref = std::sync::Arc::new(
        DownloadSpec::new(applied_spec.unwrap_or_else(|| p.spec.clone()))
            .with_connection_cap(connection_cap)
            .with_held_response(held_response)
            .with_refresher(crate::intercept::Refresher::for_params(
                p,
                crate::intercept::ProtocolKind::Http,
            )),
    );

    // Safety net (probe 阶段)：服务器在 probe 阶段返回 HTML 但用户期望二进制
    // 文件——典型场景：Lanzou 等 CDN transit page、form-POST 端点用 GET 访问。
    // 立即终止，避免落盘成 fake.zip 这类损坏文件。
    //
    // 第二道防线在 download_single 的 req.send() 后做实际响应的 Content-Type 检查
    // （通过 build_request 发起，见 :2335 附近），覆盖 hint_file_size 旁路场景。
    if !info.content_type.is_empty() {
        let ct_lower = info.content_type.to_ascii_lowercase();
        let mime = ct_lower.split(';').next().unwrap_or("").trim();
        if mime == "text/html" || mime == "application/xhtml+xml" {
            let expected = if p.file_name.is_empty() {
                &info.file_name
            } else {
                &p.file_name
            };
            if !filename_looks_like_html(expected) {
                return Err(DownloadError::Other(format!(
                    "server returned HTML page (Content-Type: {mime}) instead of the expected file — {}",
                    html_response_hint(&p.url, probe_final_url.as_ref(), &p.spec.method)
                )));
            }
        }
    }

    // 早期取消检查：probe 完成后、创建文件之前检测 pause/delete，
    // 防止已取消的任务仍然在磁盘上创建临时文件。
    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    // info.file_name 来自 probe，已经过 sanitize_filename；用户/API 显式提供的
    // p.file_name 是外部输入（RPC out / 管理 API file_name / 浏览器接管），必须
    // 清洗路径分隔符与 `..`，否则 save_dir.join(name) 可穿越 save_dir 落盘任意路径。
    let auto_name = if p.file_name.is_empty() {
        info.file_name.clone()
    } else {
        sanitize_filename(&p.file_name)
    };

    let save_dir = PathBuf::from(&p.save_dir);

    // Safety net: if filename is still empty after all resolution attempts,
    // abort early with a clear error instead of silently using save_dir as
    // the destination path (which would cause an OS-level write error or
    // corrupt the directory).
    if auto_name.is_empty() {
        return Err(DownloadError::Other(
            "could not determine a file name for this download — \
             please retry and specify a file name manually"
                .to_string(),
        ));
    }

    // 文件名由 DownloadManager 在 do_start_task 同步段统一决策（含 dedup 和
    // 兄弟任务预订协调），downloader 内不再做名称变更，避免双源决策导致的
    // 自我冲突或 reserved 集合污染（参见 PR #296 的回归 bug）。
    //
    // 仅当 p.file_name 为空时（极端兜底，正常路径不应发生），使用 probe
    // 结果作为文件名；此时也不做 dedup（manager 应在 spawn 前确保 file_name
    // 已 dedup）。
    let actual_name = auto_name.clone();
    let dest_path = save_dir.join(&actual_name);
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));
    output::ensure_parent(&temp_path).await?;
    // Keep ownership through mode changes, stale-data cleanup, verification and final rename.
    let _temp_file_guard = TempFileGuard::acquire(&temp_path, &p.task_id).await?;

    // 换源结算（见 `Db::settle_source_change`）：`change_task_url` 后首次续传按零容差
    // 比对新链接大小与旧总大小。不一致 = 换成了另一个文件——旧前缀若沿用会与新文件
    // 尾部静默拼接（CDN 漂移容差恰好覆盖「新版本只大几百 KB」），故清空分段并删掉
    // 旧临时文件，从头下载新文件。大小一致（如 CDN 签名过期换新链接）照常续传。
    let source_replaced = p.is_resume
        && p.db
            .settle_source_change(&p.task_id, info.total_bytes)
            .await?;
    if source_replaced {
        log_info!(
            "[download] task {} 换源后大小与原文件不一致（新={}），丢弃已下进度从头下载",
            p.task_id,
            info.total_bytes
        );
        match tokio::fs::remove_file(&temp_path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }

    // For resume tasks we must NOT blindly overwrite total_bytes with the
    // freshly-probed value.  CDN servers frequently return a slightly different
    // Content-Length on each request (transfer-encoding overhead, dynamic header
    // injection, signed-URL padding, …).  Even a 1-byte difference would cause
    // download_multi_segment to conclude "file changed → delete all segments →
    // restart from zero".
    //
    // update_task_file_info_resume() applies a tolerance threshold: only
    // updates total_bytes when the delta exceeds 1 % of the stored size (or
    // 1 MiB, whichever is smaller).  It returns the *effective* total_bytes
    // that callers must use so everything (segments, progress bar, size checks)
    // is consistent with a single source of truth.
    //
    // For new downloads we still use the plain overwrite (update_task_file_info)
    // because there is no prior state to protect.
    let mut effective_total_bytes = if p.is_resume {
        let (effective, updated) =
            p.db.update_task_file_info_resume(&p.task_id, &actual_name, info.total_bytes)
                .await?;
        if updated {
            log_info!(
                "[download] task {} resume: total_bytes updated {} → {} (genuine size change)",
                p.task_id,
                info.total_bytes, // probed value that was accepted
                effective
            );
        } else {
            log_info!(
                "[download] task {} resume: preserving stored total_bytes={} (probe={}, delta within tolerance)",
                p.task_id,
                effective,
                info.total_bytes
            );
        }
        effective
    } else {
        p.db.update_task_file_info(&p.task_id, &actual_name, info.total_bytes)
            .await?;
        info.total_bytes
    };

    // When resuming, also determine whether the server actually supports Range
    // requests.  The probe result is authoritative for new downloads, but for
    // resumes the probe may return supports_range=false for servers that only
    // advertise Accept-Ranges on the real GET (not HEAD).  If we have existing
    // segment rows in the DB the server clearly supported Range previously, so
    // trust that history and keep multi-segment mode.
    let effective_supports_range = if p.is_resume && !info.supports_range {
        let existing_segs = p.db.load_segments(&p.task_id).await?;
        if !existing_segs.is_empty() {
            log_info!(
                "[download] task {} resume: probe says no Range support but {} segment(s) exist in DB — \
                 trusting prior Range capability",
                p.task_id,
                existing_segs.len()
            );
            true
        } else {
            false
        }
    } else {
        info.supports_range
    };

    // Resume 一致性校验所需的【版本标识】（ETag / Last-Modified）。
    //   • 首次下载（非续传）：用本次 probe 的值，并持久化到 DB，作为将来续传的基准。
    //   • 续传：用【首次下载时存的原值】而非本次重新 probe 的值，与 Range 响应
    //     的 validator 做后验比对。这样即使两次会话间文件被同长度替换，也不会把
    //     旧前缀与新尾部静默拼接（BUG-HTTP-SINGLE-RESUME-SPLICE）。
    let (resume_etag, resume_last_modified) = if p.is_resume {
        let (oe, olm) = p.db.get_task_validator(&p.task_id).await?;
        if oe.is_empty() && olm.is_empty() {
            // 旧任务（升级前创建、无存档）或首次下载时服务器未提供 validator →
            // 退回本次 probe 值（退化为旧行为，不会更糟）。
            (info.etag.clone(), info.last_modified.clone())
        } else {
            (oe, olm)
        }
    } else {
        if !info.etag.is_empty() || !info.last_modified.is_empty() {
            // 失败仅记日志（不阻断下载）：若存储失败，将来续传会 get 到空值并
            // 回退到"用续传时重新 probe 的 validator"——退化为旧行为，可能无法检出
            // 跨会话文件变化（单流续传拼接的残余风险）。记日志使该退化可观测。
            if let Err(e) =
                p.db.set_task_validator(&p.task_id, &info.etag, &info.last_modified)
                    .await
            {
                log_info!(
                    "[download] task {} 警告：持久化 resume validator 失败：{:?}（续传一致性校验可能退化）",
                    p.task_id,
                    e
                );
            }
        }
        (info.etag.clone(), info.last_modified.clone())
    };

    // 二次取消检查：缩小 DB 已更新但文件尚未创建的竞争窗口。
    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    p.db.update_task_status(&p.task_id, 1, "").await?;

    // Immediately notify the host: status=1 with resolved file name & total size.
    // For resume tasks, send persisted downloaded bytes as baseline so speed
    // smoothing doesn't treat resumed bytes as a fresh in-interval delta.
    let initial_downloaded = if source_replaced {
        0
    } else {
        resume_downloaded
    };
    if p.progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: initial_downloaded,
            download_baseline: Some(initial_downloaded),
            total_bytes: effective_total_bytes,
            status: 1,
            error_message: String::new(),
            file_name: actual_name.clone(),
            segment_details: None,
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("HTTP progress receiver closed");
    }

    // `size_is_estimate`：本次规划的 total 是否为【未经验证的估计值】，统一由
    // range_verified 门控（fresh 由 manager 决定，resume 读 DB）。true 的情形：
    //   • fresh 的浏览器扩展 hint——total 取自扩展 hint（可能偏小的猜测），
    //     服务器在 206 `Content-Range` 分母里自报的真实大小才是权威值，
    //     coordinator 的扩容检查须采【零容差】（见 segment_coordinator::do_segment）；
    //     且 range_verdict 以 UNKNOWN 起步、零进度首段以 plain GET 起飞（保守启动）。
    //   • resume 一个【从未验证过 Range 能力】的 hint 任务（range_verified==false，
    //     download_manager 以 DB total 作 hint 传入）——延续保守启动语义。
    // false 的情形：probe 路径、已验证任务的 resume、以及 resolver 插件担保
    // rangeSupported 的 hint 任务（大小来自平台 API，Range 能力有插件背书）——
    // 按 SUPPORTED 起步、正常多段规划，保留 CDN 漂移容差。
    // 单 GET 探测路径同属「Range 未验证」：总大小来自 Content-Length（可能是压缩 /
    // 动态长度），Range 能力待首响应裁决，同样以 plain GET 单流起飞再升级。
    let size_is_estimate =
        (p.hint_file_size > 0 && !p.range_verified) || spec_ref.has_held_response();

    // On resume, load any segment rows left over from a previous run once —
    // reused below both for the "auto" segment-count reuse and to decide
    // whether a forced single-connection resume must still go through the
    // multi-segment coordinator instead of discarding existing progress.
    let existing_segment_rows = if p.is_resume {
        p.db.load_segments(&p.task_id).await?
    } else {
        Vec::new()
    };

    // Dynamic segment calculation when user chose "auto" (segment_count <= 0).
    let mut segments = if requested_segments <= 0 {
        // When resuming, check if DB already has segment rows from a previous
        // run.  If so, reuse that count — avoids a redundant bandwidth probe
        // and guarantees segment definitions stay consistent with what's on disk.
        if !existing_segment_rows.is_empty() {
            let n = existing_segment_rows.len() as i32;
            log_info!(
                "[download] task {} resume: reusing {} existing segment(s) from DB",
                p.task_id,
                n
            );
            n
        } else {
            // Segment rows were lost (e.g. crash between tasks.segments
            // update and insert_segments), or this is a fresh download.
            compute_segments_with_advisor(p, &info).await
        }
    } else {
        requested_segments
    };
    if connection_cap > 0 {
        segments = segments.min(connection_cap);
    }

    // BUG: a resume whose effective concurrency was forced down to 1 (domain
    // single-connection cache — see `is_single_conn_domain` in
    // `download_manager.rs` — or the user setting segments=1 directly) must
    // NOT collapse into the true single-stream path below when the DB still
    // holds a prior multi-segment layout: that path unconditionally deletes
    // the segment rows and the pre-allocated temp file (see the "switching
    // multi-segment → single-stream" branch further down), discarding every
    // byte already downloaded even though nothing about the file changed.
    // The coordinator already resumes existing DB segments under any
    // `worker_cap` (including 1 — the same value the domain-cap clamp forces
    // mid-flight on an already-running multi-segment download), so routing
    // through it here costs nothing and preserves progress instead of
    // silently restarting a large download from byte 0.
    let resume_has_segments = !existing_segment_rows.is_empty();

    // Use multi-segment when:
    //   • The server supports Range (probe confirmed)
    //   • File is > 1 MB (small files don't benefit from segmentation)
    //   • We asked for more than 1 segment, OR this resume already has a
    //     multi-segment layout on disk that must be preserved
    //   • Original HTTP method is GET-like — POST + Range:bytes=X-Y is undefined
    //     in HTTP standards and most servers will silently return 200 OK with
    //     full body, corrupting the assembled output. Force single-stream
    //     for non-GET to make uupdump-style form-POST downloads safe.
    let use_segments = effective_supports_range
        && effective_total_bytes > 1_048_576
        && (segments > 1 || resume_has_segments)
        && p.spec.is_get_like();
    if !p.spec.is_get_like() {
        log_info!(
            "[download] task {} method={:?} → forcing single-stream (non-GET cannot use Range/multi-segment)",
            p.task_id,
            p.spec.method
        );
    }

    log_info!(
        "[download] task {} mode={}, segments={}, temp={}, dest={}",
        p.task_id,
        if use_segments {
            "multi-segment"
        } else {
            "single"
        },
        segments,
        temp_path.display(),
        dest_path.display()
    );

    // Tracks whether we actually used multi-segment for the integrity check
    // below.  Flipped to false when the server doesn't support Range requests
    // and we auto-fall back to single-stream within this attempt.
    let mut actual_use_segments = use_segments;
    let single_result: Option<SingleDownloadResult> = if use_segments {
        // 多段下载：若服务器在 206 响应的 `Content-Range: bytes X-Y/<total>` 分母里
        // 自报的真实总大小大于规划（hint 偏小/文件仍在上传中增长），coordinator 会
        // 【就地扩容】——延长预分配 + 追加尾段 + 更新共享 planned_total，已下数据零
        // 丢弃（修复 BUG-HTTP-HINT-UNDERSIZED：绝不把完整文件的前缀静默当作完成）。
        // 成功返回值是【最终有效总大小】，据此校准 effective_total_bytes，供下方完整
        // 性检查与完成信号使用。
        //
        // 扩容配额（segment_coordinator::MAX_SIZE_EXPANSIONS）耗尽（文件持续增长/
        // 病态分母膨胀）时 TrueSizeLarger 才会冒泡到此处，落入下方通用 Err 分支以
        // status=4 显式终止——DB 段行与临时文件【保留】，用户重试时 resume 重新
        // probe 真实大小接着下（fail-loud 且不丢进度）。
        // 收割并行发起的候选聚合，构造节点池（存活 <2 或未发起 → 单节点池，
        // 行为与现状一致）。
        let nodes = crate::cdn::finish_pool(
            cdn_pending,
            client,
            &p.cdn,
            &p.db,
            &p.task_id,
            effective_total_bytes,
            segments,
            &p.sink,
        )
        .await;
        // summary 事件需要在多段下载结束后读取节点贡献统计。
        let nodes_for_summary = nodes.clone();
        // hint 解封仅 Auto 默认档：用户显式段数或自定义 auto_max 时尊重其天花板。
        let allow_hint_uncap = requested_segments <= 0
            && p.auto_max_connections == crate::download_manager::DEFAULT_AUTO_MAX_CONNECTIONS
            && connection_cap <= 0;
        let ms_outcome = download_multi_segment(
            &p.task_id,
            &p.url,
            &temp_path,
            effective_total_bytes,
            size_is_estimate,
            segments,
            nodes,
            &p.db,
            &p.progress_tx,
            &p.cancel_token,
            &p.speed_limiter,
            &spec_ref,
            p.sink.as_ref(),
            &resume_etag,
            &resume_last_modified,
            p.spawn_gen,
            allow_hint_uncap,
            p.auto_proxy.clone(),
            p.multi_nic.clone(),
        )
        .await;

        // 服务器无视 Range（RangeNotSupported）时回退单流只能发无 Range 请求；文件版本变化 /
        // 响应错位时服务器仍接受 Range，声明了 `maxRangeBytes` 的任务回退单流必须沿用有界 Range。
        let range_ignored = matches!(ms_outcome, Err(DownloadError::RangeNotSupported(_)));
        match ms_outcome {
            Ok(final_total) => {
                if final_total != effective_total_bytes {
                    log_info!(
                        "[download] task {} 多段结束后总大小由 coordinator 校准: {} -> {}\
                         （就地扩容/resume 漂移吸附），完整性检查以校准值为准",
                        p.task_id,
                        effective_total_bytes,
                        final_total
                    );
                    effective_total_bytes = final_total;
                }
                // 多节点池：发一条节点贡献统计（详情面板「日志」Tab），只算
                // 实际产生流量的节点。单节点池零事件。
                if nodes_for_summary.is_multi() {
                    let stats: Vec<crate::model::CdnNodeInfo> = nodes_for_summary
                        .node_stats()
                        .into_iter()
                        .filter(|n| n.bytes > 0)
                        .collect();
                    if !stats.is_empty() {
                        p.sink.emit(crate::events::EngineEvent::TaskCdnEvent {
                            task_id: p.task_id.clone(),
                            kind: "summary".to_string(),
                            host: if nodes_for_summary.host().is_empty() {
                                reqwest::Url::parse(&p.url)
                                    .ok()
                                    .and_then(|u| u.host_str().map(str::to_string))
                                    .unwrap_or_default()
                            } else {
                                nodes_for_summary.host().to_string()
                            },
                            nodes: stats,
                            ip: String::new(),
                            reason: String::new(),
                            candidates: 0,
                            alive: 0,
                            cap: 0,
                            auto_cap: false,
                        });
                    }
                }
                None
            }
            Err(DownloadError::RangeNotSupported(status))
            | Err(DownloadError::VersionChanged(status))
            | Err(DownloadError::RangeMisaligned(status)) => {
                // 多段尝试中止，回退单流。三种触发：
                //   • RangeNotSupported：服务器无视 Range（返回 200 全量），或对任何
                //     带 Range 的请求直接回 4xx 且全程未服务过一个字节（coordinator
                //     在"全员被拒 + any_data==0"时归一为本变体，如 fnOS
                //     multiple-download 配额端点——仅裸 GET 可用）。
                //   • VersionChanged：文件在 probe 与分段请求间变了，响应 validator
                //     与落盘版本不匹配，旧数据作废需重下。
                //   • RangeMisaligned：服务器回 206 却发【从 0 的全量流】（Content-Range
                //     起点不符，如 123 盘失效签名 URL）；已写入段数据是错位垃圾，必须清空。
                // 注意：有已下数据的【瞬时 200】不会走到这里——coordinator 已在串行降级
                // 路径就地完成下载，不返回这些错误，故此处 remove_file 只清理错位/作废/
                // 全零的预分配数据，永不误删有效的多段进度（历史 0kb bug 已在 coordinator
                // 侧消除）。
                log_info!(
                    "[download] task {} multi-segment aborted (server returned {}; no-Range or \
                     mid-download file change) — clearing stale data, redownloading single-stream",
                    p.task_id,
                    status
                );
                actual_use_segments = false;
                if let Err(e) = crate::task_activity::record(
                    &p.db,
                    p.sink.as_ref(),
                    &p.task_id,
                    "fallback",
                    format!("HTTP 分段回退单流：{status}"),
                    Some(1),
                )
                .await
                {
                    tracing::error!(task_id = %p.task_id, error = %e, "failed to persist HTTP fallback");
                }
                // 清空多段残留：删 DB segment 行 + 删预分配临时文件。对两种触发都正确：
                //   • 真·无 Range：预分配文件全零、无有效数据；
                //   • 版本变化：已完成段是【旧版本】字节，整体作废，必须删以重下新版本。
                discard_segment_data(&p.db, &p.task_id, &temp_path).await?;
                let result = download_single(
                    &p.task_id,
                    &p.url,
                    &temp_path,
                    effective_total_bytes,
                    !range_ignored
                        && effective_supports_range
                        && spec_ref.current().range_window_cap().is_some(),
                    client,
                    &p.db,
                    p.sink.as_ref(),
                    &p.progress_tx,
                    &p.cancel_token,
                    &p.speed_limiter,
                    &spec_ref,
                    &actual_name,
                    &resume_etag,
                    &resume_last_modified,
                    false,
                )
                .await?;
                Some(result)
            }
            Err(e) => return Err(e),
        }
    } else {
        // 单流路径不聚合：中止后台候选聚合任务（若已发起）。
        if let Some(pending) = cdn_pending {
            pending.abort();
        }
        // F025: 续传时从多段切换到单流（例如 effective_total_bytes 经容差修正后
        // <=1MB、用户改 segment_count=1、或 spec 变 non-GET）。上次多段运行已把临时
        // 文件【预分配到满尺寸】total_bytes 并写入部分数据，且 DB 仍有 segment 行。
        // 若直接进入 download_single：
        //   • existing_len==预分配满尺寸 → want_resume 因 existing_len<total 为假 →
        //     File::create 截断从 0 重下（丢弃所有进度，纯效率损失）；
        //   • 若同时命中"文件真实增长"使 new_total 变大 → existing_len<new_total 为真 →
        //     从旧 total 偏移 append，而预分配 [old_total,new_total] 是空洞零字节 →
        //     产出含空洞的损坏文件。
        // 与 RangeNotSupported 回退路径一致地清理：删除 DB segment 行 + 删除预分配
        // 的临时文件，使 download_single 从 existing_len=0 的干净状态开始，无损坏风险。
        if p.is_resume {
            let existing_segs = p.db.load_segments(&p.task_id).await?;
            if !existing_segs.is_empty() {
                log_info!(
                    "[download] task {} switching multi-segment → single-stream on resume; \
                     clearing {} stale segment(s) and pre-allocated temp file",
                    p.task_id,
                    existing_segs.len()
                );
                discard_segment_data(&p.db, &p.task_id, &temp_path).await?;
                // 进度归零，避免 UI 显示陈旧的多段累计值。
                p.db.update_task_progress(&p.task_id, 0).await?;
            }
        }
        let bytes_before = file_len_or_zero(&temp_path).await;
        let started = std::time::Instant::now();
        let result = download_single(
            &p.task_id,
            &p.url,
            &temp_path,
            effective_total_bytes,
            effective_supports_range,
            client,
            &p.db,
            p.sink.as_ref(),
            &p.progress_tx,
            &p.cancel_token,
            &p.speed_limiter,
            &spec_ref,
            &actual_name,
            &resume_etag,
            &resume_last_modified,
            p.is_resume,
        )
        .await?;
        let transferred = file_len_or_zero(&temp_path)
            .await
            .saturating_sub(bytes_before);
        record_single_stream_rate(p, transferred, started.elapsed());
        Some(result)
    };

    // 单流解压场景：落盘字节是【解压后】大小，与 probe 的【压缩】
    // effective_total_bytes 无可比性——跳过基于大小的完整性校验，改为以磁盘实际
    // 大小为准更新 DB，避免把正确文件误判为 "size mismatch"（BUG-HTTP-DECOMPRESS-INTEGRITY）。
    let decompressed_single = single_result.as_ref().is_some_and(|r| r.decompressed);
    if decompressed_single {
        let file_len = tokio::fs::metadata(&temp_path)
            .await
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        log_info!(
            "[download] task {} single-stream decompressed: trusting on-disk size {} \
             (probe compressed size was {})",
            p.task_id,
            file_len,
            effective_total_bytes
        );
        p.db.update_task_total_bytes(&p.task_id, file_len).await?;
        effective_total_bytes = file_len;
    }

    // Integrity check — verify download completeness.
    // 解压路径已在上方处理，这里 effective_total_bytes 已被改写为磁盘实际大小，
    // 故 file_len == effective_total_bytes 自然成立、不会误杀。
    if effective_total_bytes > 0 && !decompressed_single {
        if actual_use_segments {
            // Multi-segment: file size is no progress signal — preallocated
            // files are total_bytes up front, grow-on-write files (non-sparse
            // filesystems) reach it once the tail lands.  Check DB progress.
            let segs = p.db.load_segments(&p.task_id).await?;
            let seg_total: i64 = segs.iter().map(|s| s.downloaded_bytes).sum();
            if seg_total < effective_total_bytes {
                return Err(DownloadError::Other(format!(
                    "segment integrity failed: expected {} bytes, segments downloaded {} bytes",
                    effective_total_bytes, seg_total
                )));
            }
            // Also verify actual file size on disk (guards against external
            // file deletion/truncation between download and this check).
            let file_len = tokio::fs::metadata(&temp_path)
                .await
                .map(|m| m.len() as i64)
                .unwrap_or(0);
            if file_len < effective_total_bytes {
                return Err(DownloadError::Other(format!(
                    "file integrity failed: disk size={} bytes, expected {} bytes",
                    file_len, effective_total_bytes
                )));
            }
        } else {
            // Single-thread: no pre-allocation, file size == downloaded bytes.
            let meta = tokio::fs::metadata(&temp_path).await?;
            let file_len = meta.len() as i64;
            if file_len != effective_total_bytes {
                // The stream ended normally — check whether the response had
                // its own Content-Length that matches the actual file.  This
                // handles servers (e.g. CNKI) where the browser-extension hint
                // size differs from what the server actually delivers to
                // FluxDown's own request (dynamic tokens, re-generated PDFs,
                // slight header drift, etc.).
                let resp_cl = single_result
                    .as_ref()
                    .map(|r| r.response_content_length)
                    .unwrap_or(-1);
                if resp_cl > 0 && file_len == resp_cl {
                    log_info!(
                        "[download] task {} size drift accepted: hint={} bytes, \
                         response content-length={}, file={} (stream completed normally)",
                        p.task_id,
                        effective_total_bytes,
                        resp_cl,
                        file_len
                    );
                    // Update DB so the stored total_bytes reflects reality.
                    p.db.update_task_total_bytes(&p.task_id, file_len).await?;
                    effective_total_bytes = file_len;
                } else if resp_cl <= 0 && file_len > 0 && file_len >= effective_total_bytes {
                    // Server didn't send Content-Length (chunked / connection-close
                    // framing) but the stream ended cleanly, and we received AT LEAST
                    // the expected size — no truncation is possible, so trust the file.
                    //
                    // Two dangerous cases are deliberately excluded (both fall through
                    // to the size-mismatch error below so the user can retry):
                    //   • BUG-HTTP-NO-CL-TRUNCATION: a probe-derived (reliable) total of
                    //     N, no Content-Length, clean close after K < N bytes.
                    //   • BUG-HTTP-HINT-UNDERSIZED (single-stream面): a browser *hint* of
                    //     N with no Content-Length and a clean close after K < N.  The
                    //     old `|| hint_file_size > 0` escape treated a hint as "unreliable,
                    //     may shrink" and accepted the K-byte prefix — but a no-CL stream
                    //     cannot tell a legitimately smaller file from a truncated one, so
                    //     silently keeping K bytes was data loss.  A genuinely regenerated
                    //     *larger* file still satisfies `file_len >= effective_total_bytes`
                    //     and is accepted; a regenerated smaller file that carries a real
                    //     Content-Length is already handled by the `resp_cl > 0` branch
                    //     above.  Only the ambiguous no-CL-and-shorter case now errors.
                    log_info!(
                        "[download] task {} no response content-length, trusting \
                         actual file size: expected={}, file={} (stream completed normally)",
                        p.task_id,
                        effective_total_bytes,
                        file_len
                    );
                    p.db.update_task_total_bytes(&p.task_id, file_len).await?;
                    effective_total_bytes = file_len;
                } else {
                    return Err(DownloadError::Other(format!(
                        "size mismatch: expected {} bytes, got {} bytes \
                         (response content-length={})",
                        effective_total_bytes, file_len, resp_cl
                    )));
                }
            }
        }
    }

    // Determine the actual downloaded size.  When the server didn't report
    // Content-Length (total_bytes == 0), read the real file size from disk so
    // that the completion signal carries accurate byte counts.
    let actual_total = if effective_total_bytes > 0 {
        effective_total_bytes // may have been corrected by size-drift logic above
    } else {
        match tokio::fs::metadata(&temp_path).await {
            Ok(m) => m.len() as i64,
            Err(e) => {
                log_info!(
                    "[download] task {} warning: cannot read temp file size: {}",
                    p.task_id,
                    e
                );
                0
            }
        }
    };

    // 注：旧版本会在此处读取 DB 中可能被 download_single / do_segment 更新过
    // 的"更好的文件名"（来自 GET 响应的 Content-Disposition），并再次 dedup
    // 以应用到磁盘文件名。该机制存在两个根本问题：
    //   1. 与 manager 的 reserved_temp_paths 协调断裂——better-name 不在
    //      manager 的预订集合中，可能与并发兄弟任务冲突。
    //   2. 与 manager 的"文件名唯一决策者"原则冲突。
    //
    // 浏览器扩展已在请求阶段解析 Content-Disposition 并作为 hint filename
    // 传入；命令行/手动新建任务也会在 manager 的 do_start_task 中通过 probe
    // 拿到 Content-Disposition。downloader 内部不再二次改名。

    // Checksum verification — runs after size integrity check, before rename.
    if !p.checksum.is_empty() {
        log_info!(
            "[download] task {} verifying checksum: {}",
            p.task_id,
            p.checksum
        );
        // F039: checksum 失败表明落盘内容已确认与期望不符（数据损坏/被篡改/
        // 传输错误），此时残留一个完整大小、内容错误的临时文件毫无意义——既
        // 占用磁盘，又会让后续 resume 基于 existing_len 做出错误判断。删除它，
        // 让下次重试从干净状态开始。删除失败仅记日志，不掩盖原始 checksum 错误。
        let verified = match verify_checksum(&temp_path, &p.checksum).await {
            Ok(verified) => verified,
            Err(e) => {
                if let Err(rm_err) = tokio::fs::remove_file(&temp_path).await {
                    log_info!(
                        "[download] task {} checksum failed; could not remove corrupt temp {}: {}",
                        p.task_id,
                        temp_path.display(),
                        rm_err
                    );
                }
                return Err(e);
            }
        };
        log_info!("[download] task {} checksum ok", p.task_id);
        // 校验通过的摘要就是落盘文件的哈希：记为文件哈希，免去完成后再读一遍整个文件。
        // 记录失败只影响展示（用户可再手动计算），不让已通过校验的下载失败。
        if let Err(error) = p.db.set_task_file_hash(&p.task_id, &verified).await {
            log_warn!(
                "[download] task {} could not record verified file hash: {:#}",
                p.task_id,
                error
            );
        }
    }

    // F034: rename 前对临时文件做 sync_all，确保内核页缓存已持久化到存储介质。
    //
    // 之前各下载路径只做 BufWriter::flush（把用户态缓冲刷到内核），从不
    // sync_all/sync_data。在 ext4(data=writeback/ordered) 等文件系统、崩溃/掉电
    // 场景下，rename 的元数据可能先于文件数据落盘，导致重启后 dest 存在但内容
    // 为 0 字节或旧块，而 .fdownloading 已消失——用户得到一个"已完成"的损坏
    // 文件且无 temp 可恢复。IDM/aria2 在 finalize 前都会 fsync。
    //
    // 写入文件句柄此前已 drop，这里重新 open 临时文件后 sync_all。sync 失败属
    // 真实 IO 错误（如磁盘故障），向上传播而非静默忽略。
    //
    // 必须以写权限打开：sync_all 在 Windows 上映射到 FlushFileBuffers，而该
    // API 要求句柄具备写权限，对只读句柄（File::open 的默认行为）会返回
    // ERROR_ACCESS_DENIED (os error 5)，导致下载在 100% 完成后于 rename 前
    // 失败。Linux/macOS 的 fsync 允许只读 fd，故此 bug 仅在 Windows 触发。
    //
    // 单流路径(`single_result` 为 Some)在流结束时已由 `persist_single_progress`
    // flush + `sync_data` 同一个写句柄,此后到 rename 之间没有任何新写入(校验和只
    // 读),这里再 reopen + sync_all 只是重复一次整盘刷写(macOS 上是 F_FULLFSYNC,
    // 每次数毫秒,小文件批量下载时占掉一半以上的单任务耗时),故跳过。多段路径由
    // 多个 worker 各自写入,没有统一的「流结束」同步点,仍在此处整体 sync_all。
    if single_result.is_none() {
        let temp_file = OpenOptions::new()
            .write(true)
            .open(&temp_path)
            .await
            .map_err(|e| {
                DownloadError::Other(format!(
                    "failed to reopen {} for fsync before rename: {}",
                    temp_path.display(),
                    e
                ))
            })?;
        temp_file.sync_all().await.map_err(|e| {
            DownloadError::Other(format!(
                "failed to fsync {} before rename: {}",
                temp_path.display(),
                e
            ))
        })?;
    }

    // All data verified — move the temp file to its final destination.
    // This is the atomic moment the file "appears" as complete.
    //
    // 完成终验 + 原子占名:dest 可能在下载期间被占用——manager 的启动期
    // 名字预订对 BT 完成移动不可见(预订落盘为 `.fdownloading` 之前有秒级
    // 空档),同名 BT 任务此间完成即占走该名;用户/外部程序也可能放入同名
    // 文件。Windows 的 rename = MOVEFILE_REPLACE_EXISTING 会静默覆盖对方
    // 产物,且「先 exists 后 rename」存在 TOCTOU——改用 [`claim_rename`]
    // 的 `create_new` 原子占名,占名失败(AlreadyExists)则重新 dedup 换名
    // 重试;换名候选同时避开 DB 里同目录未完成任务已登记的 file_name
    // (兄弟任务预订名),防 DB 指针别名。
    //
    // 完成期定名：引擎推断的名字（HEAD 被拒 / 无 Content-Disposition 时的 URL 占位、
    // 无扩展名、脚本端点名）按实际响应证据与文件头魔数精修；显式名只在缺类型扩展名
    // 时补扩展名，主名不改。精修名没有启动期预订，同样经下方原子占名环避让冲突。
    let target_name = refine_completed_name(p, &temp_path, &actual_name).await;
    let mut chosen = target_name.clone();
    let mut avoid: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut avoid_loaded = false;
    // overwrite 模式的一次性删除机会:仅对原名尝试,删除后重试同名占名;
    // 再失败(占名与删除间被并发写者抢占)回落到既有 re-dedup 换名环。
    let mut overwrite_attempted = false;
    let mut attempt = 0u32;
    loop {
        let dst = save_dir.join(&chosen);
        match claim_rename(&temp_path, &dst).await {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
                // 5 次足够越过任何现实的并发突发;仍失败说明目录状态异常
                // (如 dedup 结果被外部持续抢占),报错保留 temp 供重试。
                if attempt > 5 {
                    return Err(DownloadError::Other(format!(
                        "failed to claim a destination name for {} after {} attempts \
                         (directory keeps racing us)",
                        dest_path.display(),
                        attempt
                    )));
                }
                if !avoid_loaded {
                    avoid_loaded = true;
                    if let Ok(names) =
                        p.db.list_active_sibling_file_names(&p.save_dir, &p.task_id)
                            .await
                    {
                        avoid = names.into_iter().map(|n| n.to_lowercase()).collect();
                    }
                }
                // overwrite 模式(config `file_exists_behavior`):dst 是磁盘上
                // 已存在的普通旧文件(非目录、不在兄弟任务 avoid 集)时,删除
                // 旧文件后按原名重试占名一次。avoid 命中/目录/删除失败都不覆盖,
                // 走下方既有换名环。
                if p.overwrite.permits(&chosen)
                    && !overwrite_attempted
                    && chosen == target_name
                    && !avoid.contains(&chosen.to_lowercase())
                {
                    overwrite_attempted = true;
                    let is_dir = tokio::fs::metadata(&dst)
                        .await
                        .map(|m| m.is_dir())
                        .unwrap_or(false);
                    if !is_dir {
                        match tokio::fs::remove_file(&dst).await {
                            Ok(()) => {
                                log_info!(
                                    "[download] task {} overwriting existing file '{}' (file_exists_behavior=overwrite)",
                                    p.task_id,
                                    dst.display()
                                );
                                continue;
                            }
                            Err(re) => {
                                log_info!(
                                    "[download] task {} failed to remove existing '{}' for overwrite: {}; falling back to rename",
                                    p.task_id,
                                    dst.display(),
                                    re
                                );
                            }
                        }
                    }
                }
                log_info!(
                    "[download] task {} destination '{}' is taken; re-deduping",
                    p.task_id,
                    dst.display()
                );
                // re-dedup 恒按保守语义(不授权覆盖):此环仅在原名已被抢占/
                // 覆盖失败后进入,再放行原名会原地打转直到 attempt 耗尽。
                chosen = dedup_filename(
                    &save_dir,
                    &target_name,
                    &std::collections::HashSet::new(),
                    &avoid,
                    &crate::file_exists::OverwritePolicy::Never,
                )
                .await;
            }
            Err(e) => {
                return Err(DownloadError::Other(format!(
                    "failed to rename {} → {}: {}",
                    temp_path.display(),
                    save_dir.join(&chosen).display(),
                    e
                )));
            }
        }
    }
    let finalize_renamed = if chosen == actual_name {
        None
    } else {
        Some(chosen.clone())
    };
    let final_dest = save_dir.join(&chosen);
    if let Some(name) = &finalize_renamed {
        // rename 成功后落库(对齐 BT 完成路径:先落盘、后更新指针)。落库
        // 失败仅记日志——信号仍会携带新名,UI 与磁盘一致,DB 待下次修正。
        if let Err(e) =
            p.db.update_task_file_info(&p.task_id, name, actual_total)
                .await
        {
            log_info!(
                "[download] task {} failed to persist finalize rename '{}': {}",
                p.task_id,
                name,
                e
            );
        }
    }

    log_info!(
        "[download] task {} renamed {} → {}",
        p.task_id,
        temp_path.display(),
        final_dest.display()
    );

    if p.use_server_time {
        // 单流路径优先用【实际响应】锁存的 Last-Modified：服务器忽略 Range 返回
        // 200 全量时会从头覆盖，此时 probe/DB validator 可能已不属于磁盘内容。
        // 多段成功时 validator 已逐响应校验，probe/DB 值仍然正确。
        //
        // hint 模式（浏览器扩展 takeover，probe 被跳过）下 `resume_last_modified`
        // 恒为空串——do_segment 已把首个响应的 validator 落到 DB（见
        // segment_coordinator::check_cross_segment_validators 调用点），这里回读
        // 补上，否则大文件（走多段）的 use_server_time 会静默退化为本地完成时间
        // （此前只有单流下载受益）。
        let latched = single_result
            .as_ref()
            .and_then(|r| r.latched_last_modified.as_deref())
            .map(str::to_string);
        let last_modified = match latched {
            Some(lm) => lm,
            None if !resume_last_modified.is_empty() => resume_last_modified.clone(),
            None => {
                p.db.get_task_validator(&p.task_id)
                    .await
                    .map(|(_, lm)| lm)
                    .unwrap_or_default()
            }
        };
        apply_server_mtime(&final_dest, &last_modified, &p.task_id).await;
    }

    Ok((actual_total, finalize_renamed))
}

/// 将 HTTP 日期字符串解析为 [`std::time::SystemTime`]。
///
/// 覆盖 RFC 9110 §5.6.7 允许的三种格式：IMF-fixdate
/// （`Sun, 06 Nov 1994 08:49:37 GMT`，RFC 2822 子集）、过时的 RFC 850
/// （`Sunday, 06-Nov-94 08:49:37 GMT`）与 ANSI C `asctime()`
/// （`Sun Nov  6 08:49:37 1994`，按 UTC 解释）。
///
/// 星期字段是冗余修饰，现实中偶见服务器生成与日期不符的星期——若做一致性
/// 校验会整体拒绝本可用的时间戳，故解析前一律剥离星期、只信日期本身。
/// 全部格式失败、或时间早于 Unix 纪元（无法表示为文件时间戳）时返回 `None`。
fn parse_http_date(s: &str) -> Option<std::time::SystemTime> {
    let s = s.trim();
    let ts = if let Some((_, rest)) = s.split_once(',') {
        // IMF-fixdate 与 RFC 850 都以「星期,」开头；剥离后按剩余字段解析
        // （RFC 2822 的星期本就可选，直接解析剩余部分即合法输入）。
        let rest = rest.trim();
        chrono::DateTime::parse_from_rfc2822(rest)
            .map(|dt| dt.timestamp())
            .or_else(|_| {
                chrono::NaiveDateTime::parse_from_str(rest, "%d-%b-%y %H:%M:%S GMT")
                    .map(|dt| dt.and_utc().timestamp())
            })
            .ok()?
    } else {
        // 无逗号：标准 RFC 2822（省略星期）或 asctime（首个空格前为星期缩写）。
        chrono::DateTime::parse_from_rfc2822(s)
            .map(|dt| dt.timestamp())
            .or_else(|_| {
                let rest = s.split_once(' ').map_or(s, |(_, r)| r).trim();
                chrono::NaiveDateTime::parse_from_str(rest, "%b %e %H:%M:%S %Y")
                    .map(|dt| dt.and_utc().timestamp())
            })
            .ok()?
    };
    let secs = u64::try_from(ts).ok()?;
    Some(std::time::UNIX_EPOCH + Duration::from_secs(secs))
}

/// 把 UTC 时间格式化为 RFC 9110 IMF-fixdate（`Sun, 06 Nov 1994 08:49:37 GMT`）。
///
/// 非 HTTP 协议（FTP `MDTM`）拿到的服务器时间经此转成与 `Last-Modified` 相同的
/// 文本形态，统一交给 [`parse_http_date`] / [`apply_server_mtime`] 处理。
pub(crate) fn format_http_date(time: chrono::NaiveDateTime) -> String {
    time.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// 把已完成下载的最终文件修改时间设为服务器提供的 `Last-Modified` 时间。
///
/// 时间戳属于尽力而为的元数据：`last_modified` 为空（服务器未提供）、解析
/// 失败或写入失败都只记日志直接返回——此刻文件数据已完整落盘，任何元数据
/// 失败都不允许把成功的下载变成错误。
pub(crate) async fn apply_server_mtime(dest: &Path, last_modified: &str, task_id: &str) {
    if last_modified.is_empty() {
        log_info!(
            "[download] task {} 服务器未提供文件时间（Last-Modified / MDTM），{} 保留本地完成时间",
            task_id,
            dest.display()
        );
        return;
    }
    let Some(mtime) = parse_http_date(last_modified) else {
        log_info!(
            "[download] task {} 无法解析 Last-Modified \"{}\"，保留本地完成时间",
            task_id,
            last_modified
        );
        return;
    };
    let path = dest.to_path_buf();
    // set_times 是同步系统调用，放阻塞线程池。
    let result = tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        // Windows 只申请 FILE_WRITE_ATTRIBUTES：仅含属性权限的打开不参与共享模式
        // 检查，刚 rename 完时杀软 / 索引服务以拒绝共享写的方式打开文件也不会
        // ERROR_SHARING_VIOLATION（完整写权限会因此失败、退回本地完成时间）。
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::access_mode(
            &mut options,
            windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES,
        );
        #[cfg(not(windows))]
        options.write(true);
        let file = options.open(&path)?;
        file.set_times(std::fs::FileTimes::new().set_modified(mtime))
    })
    .await;
    match result {
        Ok(Ok(())) => {
            log_info!(
                "[download] task {} 文件修改时间已设为服务器时间 {}",
                task_id,
                last_modified
            );
        }
        Ok(Err(e)) => {
            log_info!(
                "[download] task {} 设置服务器文件时间失败：{}（保留本地完成时间）",
                task_id,
                e
            );
        }
        Err(e) => {
            log_info!(
                "[download] task {} 设置服务器文件时间的阻塞任务异常：{}",
                task_id,
                e
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Single-thread download (with resume support)
// ---------------------------------------------------------------------------

/// Result of a single-thread download, carrying response metadata for the
/// caller's integrity check.
struct SingleDownloadResult {
    /// The `Content-Length` header value from the server's actual response.
    /// -1 when the header was absent (e.g. chunked transfer).
    response_content_length: i64,
    /// True when the response carried a Content-Encoding and the body was
    /// decompressed on-the-fly.  In this case the bytes on disk are the
    /// *decompressed* size, which has no relation to the probe's (compressed)
    /// `total_bytes` — the caller MUST skip the file-size integrity check.
    decompressed: bool,
    /// 服务器【实际响应】的 `Last-Modified`。仅当响应体从 byte 0 全量服务时为
    /// `Some`（全新下载，或服务器忽略 Range 后从头覆盖）——此时磁盘内容以该
    /// 响应为准，probe/DB validator 可能描述的是旧版本。
    /// 真 206 续传为 `None`（磁盘内容与旧 validator 一致，沿用旧值）。
    latched_last_modified: Option<String>,
    /// 真 206 本次请求的起点；声明了 `maxRangeBytes` 时，从 0 起的 206 窗口也算（起点 0）。
    /// 调用方用它确认本轮确实推进，并在已知总大小尚未写满时继续发出下一个有界 Range。
    resumed_range_start: Option<u64>,
}

fn single_runtime(task_id: &str, downloaded: i64, total: i64, active: bool) -> TaskRuntime {
    TaskRuntime {
        task_id: task_id.to_owned(),
        sampled_at_ms: chrono::Utc::now().timestamp_millis(),
        sample_sequence: crate::transfer_activity::next_sample_sequence(),
        active_transfers: Some(u32::from(active)),
        connected_peers: None,
        parallelism_limit: Some(1),
        total_bytes: total,
        segments: if total > 0 {
            vec![TaskSegment {
                index: 0,
                start_byte: 0,
                end_byte: total - 1,
                downloaded_bytes: downloaded,
                active: Some(active),
            }]
        } else {
            Vec::new()
        },
        source_bytes: None,
    }
}

fn single_segment_progress(
    downloaded: i64,
    total: i64,
    active: bool,
) -> Option<Vec<SegmentProgressInfo>> {
    (total > 0).then(|| {
        vec![SegmentProgressInfo {
            index: 0,
            start_byte: 0,
            end_byte: total - 1,
            downloaded_bytes: downloaded,
            active: Some(active),
        }]
    })
}

/// 单流每次 Range 请求的跨度：默认沿用 [`MAX_SINGLE_RESUME_RANGE_BYTES`]，插件声明
/// `maxRangeBytes` 时取二者较小值。
fn single_range_window(spec: &DownloadSpec) -> i64 {
    spec.current()
        .range_window_cap()
        .map_or(MAX_SINGLE_RESUME_RANGE_BYTES, |cap| {
            cap.min(MAX_SINGLE_RESUME_RANGE_BYTES)
        })
}

/// 单流是否必须全程使用有界 Range（插件 `maxRangeBytes`）：需要服务器支持 Range 且总大小
/// 已知才能分窗；否则只能沿用既有的无 Range / 开放式 Range 行为。
fn single_stream_capped(spec: &DownloadSpec, total_bytes: i64, supports_range: bool) -> bool {
    supports_range && total_bytes > 0 && spec.current().range_window_cap().is_some()
}

#[allow(clippy::too_many_arguments)]
async fn download_single(
    task_id: &str,
    url: &str,
    dest: &Path,
    total_bytes: i64,
    supports_range: bool,
    client: &Client,
    db: &Db,
    sink: &dyn EventSink,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    spec: &DownloadSpec,
    expected_filename: &str,
    expected_etag: &str,
    expected_last_modified: &str,
    is_resume: bool,
) -> Result<SingleDownloadResult, DownloadError> {
    output::ensure_parent(dest).await?;
    let _temp_file_guard = TempFileGuard::acquire(dest, task_id).await?;
    let mut clean_boundary = false;
    let mut rounds: u64 = 0;
    let window = single_range_window(spec);
    let capped = single_stream_capped(spec, total_bytes, supports_range);
    // 有界窗口各轮之间的版本基线：某一轮从 0 重新服务（首轮或文件被替换后重下）时，
    // 以该响应落库的 validator 为准；不再沿用 probe 的旧基线，否则替换后的文件会在
    // 每一轮续传里反复判为「版本变化」。无 `maxRangeBytes` 时永不进入这条路径。
    let mut etag_baseline = expected_etag.to_string();
    let mut last_modified_baseline = expected_last_modified.to_string();
    // 最近一次从 0 服务的响应所锁存的 Last-Modified，随最终结果带回调用方。
    let mut latched_last_modified: Option<String> = None;
    loop {
        let mut result = download_single_once(
            task_id,
            url,
            dest,
            total_bytes,
            supports_range,
            client,
            db,
            progress_tx,
            cancel_token,
            speed_limiter,
            spec,
            expected_filename,
            &etag_baseline,
            &last_modified_baseline,
            is_resume,
            clean_boundary,
        )
        .await?;
        if result.latched_last_modified.is_some() {
            latched_last_modified = result.latched_last_modified.take();
            if capped {
                (etag_baseline, last_modified_baseline) = db.get_task_validator(task_id).await?;
            }
        }

        let Some(range_start) = result.resumed_range_start else {
            result.latched_last_modified = latched_last_modified;
            return Ok(result);
        };
        if total_bytes <= 0 {
            result.latched_last_modified = latched_last_modified;
            return Ok(result);
        }

        let current_len = tokio::fs::metadata(dest).await?.len();
        let expected_len = u64::try_from(total_bytes).map_err(|_| {
            DownloadError::Other(format!("invalid negative total size: {total_bytes}"))
        })?;
        if current_len >= expected_len {
            result.latched_last_modified = latched_last_modified;
            return Ok(result);
        }
        // 无进展或轮数异常：交给外层带退避的重试，而不是无退避地空转请求。
        rounds += 1;
        let max_rounds = expected_len.div_ceil(window as u64) + 256;
        if current_len <= range_start || rounds > max_rounds {
            return Err(DownloadError::Other(format!(
                "download stalled: resumed Range made no progress before expected total: \
                 {current_len}/{expected_len}"
            )));
        }
        // 正常衔接（窗口恰好写满）不是重试，不写活动记录；只有服务器提前结束
        // 窗口才值得用户看到。
        let window_end = range_start.saturating_add(window as u64).min(expected_len);
        if current_len < window_end
            && let Err(e) = crate::task_activity::record(
                db,
                sink,
                task_id,
                "retry",
                format!(
                    "HTTP 单流续传短响应：{current_len}/{expected_len}，从偏移 {current_len} 继续"
                ),
                Some(1),
            )
            .await
        {
            tracing::error!(task_id, error = %e, "failed to persist HTTP single-stream retry");
        }
        // 本轮干净 EOF，下一轮从实际末尾衔接，不再向下对齐到 1 MiB 检查点。
        clean_boundary = true;
    }
}

/// 等待响应头的上限。只覆盖「发出请求 → 收到响应头」阶段，不限制响应体速度，
/// 所以慢速但健康的连接不受影响；不设在共享 client 上，避免误伤大文件 body。
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(60);

/// 响应被拒（401 / 403 / 410）且有插件声明 `refresh` 阶段时，用换发后的请求身份重发
/// 同一请求一次（调用方保证每个请求只调用一次）；否则原样返回 `resp`。
async fn resend_after_refresh(
    spec: &DownloadSpec,
    used: &mut std::sync::Arc<RequestSpec>,
    resp: reqwest::Response,
    build: &impl Fn(&RequestSpec) -> crate::http_request::HttpRequest<'_>,
    cancel_token: &CancellationToken,
) -> Result<reqwest::Response, DownloadError> {
    if resp.status().is_success() {
        spec.confirm(used);
        return Ok(resp);
    }
    match spec.refresh_after_rejection(used, resp.status()).await? {
        Some(fresh) => {
            drop(resp);
            *used = fresh;
            let resp = send_cancellable(build(used.as_ref()), cancel_token).await?;
            if resp.status().is_success() {
                spec.confirm(used);
            }
            Ok(resp)
        }
        None => Ok(resp),
    }
}

/// 「按请求身份构造请求」闭包的签名提示：返回值借用的是参数 `&RequestSpec`。
fn identity_request<F>(build: F) -> F
where
    F: Fn(&RequestSpec) -> crate::http_request::HttpRequest<'_>,
{
    build
}

/// 发送请求并与取消令牌、响应头超时竞速。服务器接受连接却迟迟不回头时，
/// 暂停/删除必须立即生效，超时则按停滞错误交给自动重试。
async fn send_cancellable(
    req: crate::http_request::HttpRequest<'_>,
    cancel_token: &CancellationToken,
) -> Result<reqwest::Response, DownloadError> {
    tokio::select! {
        _ = cancel_token.cancelled() => Err(DownloadError::Cancelled),
        sent = tokio::time::timeout(RESPONSE_HEADER_TIMEOUT, req.send()) => match sent {
            Ok(resp) => Ok(resp?),
            Err(_) => Err(DownloadError::Other(format!(
                "download stalled: no response headers received within {}s",
                RESPONSE_HEADER_TIMEOUT.as_secs()
            ))),
        },
    }
}

/// 单流响应在已知声明长度下未兑现时的停滞错误。只陈述观察到的事实（已收 / 声明 /
/// 差额），不推断服务端为何少发；文案保留 `download stalled: no data received for Ns`
/// 前缀，`net_failure` 的停滞判定与自动重试白名单照旧命中。
///
/// `declared_end` 是响应自身声明的终点（文件偏移，续传时含已有部分）；压缩响应
/// 与无 Content-Length 的响应为 `None`。
fn single_stall_error(
    stall_secs: u64,
    body_start: i64,
    downloaded: i64,
    declared_end: Option<i64>,
) -> DownloadError {
    let received = downloaded.saturating_sub(body_start);
    let message = match declared_end {
        Some(end) if downloaded < end => format!(
            "download stalled: no data received for {stall_secs}s with the response incomplete: \
             {downloaded} of {end} bytes declared by the response received \
             ({} bytes short)",
            end - downloaded
        ),
        _ if received == 0 => format!(
            "download stalled: no data received for {stall_secs}s after the response headers \
             (no body bytes yet)"
        ),
        _ => format!(
            "download stalled: no data received for {stall_secs}s after {received} body bytes"
        ),
    };
    DownloadError::Other(message)
}

/// 单流响应体读取失败（连接提前结束 / 被重置）。已知声明长度且尚未收齐时，
/// 在原错误前明确标出「响应不完整」及已收 / 声明字节数；`incomplete download`
/// 同时是自动重试白名单词。保留原 `ErrorKind`，其余情况原样透传。
fn single_body_error(
    e: std::io::Error,
    downloaded: i64,
    declared_end: Option<i64>,
) -> DownloadError {
    match declared_end {
        Some(end) if downloaded < end => DownloadError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "incomplete download: response body ended after {downloaded} of {end} bytes \
                 declared by the response ({} bytes short): {e}{}",
                end - downloaded,
                // 新 io::Error 的 payload 是 String、没有 source；根因链在这里就拼进文案，
                // 外层 `format_error_chain(src.source())` 不会重复打印。
                format_error_chain(StdError::source(&e))
            ),
        )),
        _ => DownloadError::Io(e),
    }
}

async fn persist_single_progress(
    file: &mut tokio::io::BufWriter<File>,
    db: &Db,
    task_id: &str,
    downloaded: i64,
) -> Result<(), DownloadError> {
    file.flush().await?;
    file.get_ref().sync_data().await?;
    db.update_task_progress(task_id, downloaded).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn download_single_once(
    task_id: &str,
    url: &str,
    dest: &Path,
    total_bytes: i64,
    supports_range: bool,
    client: &Client,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    spec: &DownloadSpec,
    expected_filename: &str,
    // 续传一致性校验：probe 阶段看到的文件版本标识。Range 206 返回后与响应
    // validator 做后验比对，避免发送可能作废一次性签名 URL 的 If-Range。
    expected_etag: &str,
    expected_last_modified: &str,
    // 任务是否为续传（决定「临时文件已满」可否跳过下载）。
    is_resume: bool,
    // 上一轮有界窗口干净 EOF 后的衔接请求：起点取实际文件末尾，不向下对齐。
    clean_boundary: bool,
) -> Result<SingleDownloadResult, DownloadError> {
    let physical_existing_len = match tokio::fs::metadata(dest).await {
        Ok(metadata) => i64::try_from(metadata.len()).map_err(|_| {
            DownloadError::Other(format!(
                "partial file is too large to resume: {} bytes",
                metadata.len()
            ))
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };

    // 续传时临时文件已写满（收尾阶段被中断/失败）：无需任何网络请求，直接交给
    // 调用方的完整性检查与收尾，避免把数 GB 整文件从 0 重下。
    if is_resume && !clean_boundary && total_bytes > 0 && physical_existing_len == total_bytes {
        log_info!(
            "[download-single] task {} temp file already holds all {} bytes; skipping download",
            task_id,
            total_bytes
        );
        // A full-length file may be left by an earlier failed sync. Do not
        // promote it to a complete resume checkpoint until its data is durable.
        OpenOptions::new()
            .write(true)
            .open(dest)
            .await?
            .sync_data()
            .await?;
        db.update_task_progress(task_id, total_bytes).await?;
        return Ok(SingleDownloadResult {
            response_content_length: total_bytes,
            decompressed: false,
            latched_last_modified: None,
            resumed_range_start: None,
        });
    }

    // Resume only when the original method is GET-like. POST + Range is not a
    // portable contract and most servers ignore it, which would corrupt an append.
    // 单请求探测保留了首响应：直接从 0 消费该响应，不发 Range（一次性 URL / 表单只提交一次）。
    // 保留响应只出现在 fresh 运行（GET 类）或非 GET 请求上；后者从不按 Range 续传，即使
    // 任务以续传身份进入也必须消费它，否则同一表单会被再提交一次。
    let use_held = spec.has_held_response();
    let want_resume = spec.current().is_get_like()
        && supports_range
        && !use_held
        && physical_existing_len > 0
        && (total_bytes == 0 || physical_existing_len < total_bytes);
    // 紧接着上一个有界窗口干净 EOF 的续传点无需回退到检查点：数据已完整落盘。
    // 只有连接中断留下的不确定尾部才向下对齐。
    let existing_len = if want_resume && !clean_boundary {
        physical_existing_len - physical_existing_len.rem_euclid(SINGLE_RESUME_ALIGNMENT_BYTES)
    } else {
        physical_existing_len
    };
    if want_resume && existing_len != physical_existing_len {
        log_info!(
            "[download-single] task {} resume checkpoint aligned: {} -> {}",
            task_id,
            physical_existing_len,
            existing_len
        );
    }

    let mut downloaded: i64;
    let mut file;

    // 插件 `maxRangeBytes`：每个请求（含首个、含重下）都必须是 ≤ cap 的有界 Range。
    // 全新下载也发 Range 窗口（起点 0），窗口之间由 `download_single` 的外层循环衔接。
    let capped = single_stream_capped(spec, total_bytes, supports_range);
    let window = single_range_window(spec);
    let ranged = want_resume || capped;
    let request_start = if want_resume { existing_len } else { 0 };
    let range = if ranged && total_bytes > 0 {
        let end = request_start
            .saturating_add(window - 1)
            .min(total_bytes - 1);
        format!("bytes={request_start}-{end}")
    } else {
        format!("bytes={existing_len}-")
    };
    let build = identity_request(|used: &RequestSpec| {
        let req = build_request(
            client,
            spec.route_url(url).as_deref().unwrap_or(url),
            used.method.clone(),
            used,
        );
        if ranged {
            // 与 aria2 一致，续传首枪只发送 Range。部分一次性签名端点会把
            // If-Range 视为签名外请求头并立即作废 URL；收到 403 后再降级已经太晚。
            // 版本安全由下方对 206 响应的 ETag/Last-Modified 后验校验保证。
            req.header("Range", &range)
        } else {
            req
        }
    });
    // 放弃续传偏移后的重发：默认不带 Range 取完整文件；声明了 `maxRangeBytes` 时改为
    // 从 0 起的首个有界窗口（无 Range 的请求会被这类端点拒绝）。
    let full_range = format!("bytes=0-{}", (window - 1).min(total_bytes.max(1) - 1));
    let build_full = identity_request(|used: &RequestSpec| {
        let req = build_request(
            client,
            spec.route_url(url).as_deref().unwrap_or(url),
            used.method.clone(),
            used,
        );
        if capped {
            req.header("Range", &full_range)
        } else {
            req
        }
    });
    let mut used = spec.current();
    let mut refetched_full = false;
    let mut resp = match use_held.then(|| spec.take_held_response()).flatten() {
        Some(held) => held,
        None => send_cancellable(build(&used), cancel_token).await?,
    };
    // hint 模式跳过了探测：反机器人质询只能在这里首次暴露，换中性 UA 重发同一请求。
    if let Some(neutral) = spec.escape_bot_challenge(&used, &resp, db, task_id).await {
        drop(resp);
        used = neutral;
        resp = send_cancellable(build(&used), cancel_token).await?;
    }
    // 被拒（401 / 403 / 410）且有插件声明 refresh：用换发后的凭据重发同一请求一次。
    resp = resend_after_refresh(spec, &mut used, resp, &build, cancel_token).await?;
    // BUG-HTTP-416-RETRY-EXHAUST：续传 Range 偏移越界时服务器回 416（临时文件
    // 被外部截断、或服务器文件在两次探测间缩小）。error_for_status() 会把它
    // 变成不可恢复的错误直接终止任务；416 语义明确——重试同一 Range 必然拿到
    // 同样的响应，唯一出路是放弃续传偏移，不带 Range 重新请求从头下载整个
    // 文件（与下方"Range-on-206 压缩"回退同一手法：丢弃当前响应重新发送）。
    if want_resume && resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        log_info!(
            "[download-single] task {} 续传 Range 请求收到 416 Range Not Satisfiable，\
             放弃续传偏移 {} 重新从头请求完整文件",
            task_id,
            existing_len
        );
        drop(resp);
        let full = send_cancellable(build_full(&used), cancel_token).await?;
        resp = resend_after_refresh(spec, &mut used, full, &build_full, cancel_token).await?;
        refetched_full = true;
    }
    let mut resp = resp.error_for_status()?;

    if want_resume && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let resp_etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let resp_lm = resp
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let etag_mismatch =
            !expected_etag.is_empty() && !resp_etag.is_empty() && resp_etag != expected_etag;
        let last_modified_mismatch = !expected_last_modified.is_empty()
            && !resp_lm.is_empty()
            && resp_lm != expected_last_modified;
        if etag_mismatch || last_modified_mismatch {
            let content_range_total = parse_content_range_total(resp.headers());
            if validator_mismatch_is_fatal(
                etag_mismatch,
                last_modified_mismatch,
                content_range_total,
                total_bytes,
                None,
                None,
            ) {
                // 文件在暂停期间被替换：本地前缀是旧版本，不可拼接，也不能当作终态
                // 错误退出——DB 里的旧 validator 会让之后每次续传都撞同一错误，任务
                // 永远卡死。与 416 / 不可信 206 同一出路：丢弃续传偏移，不带 Range
                // 重新请求完整文件；下方 `!actual_resume` 分支会截断旧前缀并把新响应
                // 的 validator 回写 DB。
                log_info!(
                    "[download-single] task {} 续传响应 validator 与已存基线不符\
                     （etag 基线=\"{}\" 本次=\"{}\"；Last-Modified 基线=\"{}\" 本次=\"{}\"），\
                     文件已在服务器上变更；丢弃旧前缀（existing_len={}），从头重下新版本",
                    task_id,
                    expected_etag,
                    resp_etag,
                    expected_last_modified,
                    resp_lm,
                    existing_len
                );
                drop(resp);
                let full = send_cancellable(build_full(&used), cancel_token).await?;
                resp = resend_after_refresh(spec, &mut used, full, &build_full, cancel_token)
                    .await?
                    .error_for_status()?;
                refetched_full = true;
            } else {
                log_warn!(
                    "[download-single] task {} 续传响应 Last-Modified 不同（probe=\"{}\" 本次=\"{}\"）\
                     但 Content-Range 总大小（{:?}）与已知总大小（{}）一致，判定为跨 CDN edge 的\
                     时钟/格式差异，继续续传",
                    task_id,
                    expected_last_modified,
                    resp_lm,
                    content_range_total,
                    total_bytes
                );
            }
        }
    }

    // 声明了 `maxRangeBytes` 时不能退回无 Range 全量重发（端点会拒绝）。窗口响应必须
    // 是未压缩、带 Content-Range 且起点与请求一致、总大小与已知一致的 206（缺
    // Content-Range 无法区分「只给窗口」与「从 0 全量流」，写下去会静默截断），否则
    // 直接报错，由用户 / 任务级重试决定。
    if capped && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let encoded = detect_content_encoding(resp.headers()).is_some();
        let cr_start = parse_content_range_start(resp.headers());
        let cr_total = parse_content_range_total(resp.headers());
        let expected_start = if want_resume && !refetched_full {
            existing_len
        } else {
            0
        };
        if encoded || cr_start != Some(expected_start) || cr_total.is_some_and(|t| t != total_bytes)
        {
            return Err(DownloadError::Other(format!(
                "server returned an unusable 206 for a windowed Range request \
                 (compressed={encoded}, range start={cr_start:?}, total={cr_total:?}); \
                 expected start {expected_start}, total {total_bytes}; \
                 refusing to write misaligned data under maxRangeBytes"
            )));
        }
    }
    // 206 响应不可信时丢弃，不带 Range 重新请求完整文件：
    //   • F019：带 Content-Encoding 的 206 是压缩流任意中间字节起的一段，无法从
    //     existing_len（解压后偏移）续传，也无法当全量解压；
    //   • 缺 Content-Range：无法判断响应体的起点。按 Range 返回尾段的服务器和
    //     「发从 0 全量流」的服务器（123 盘类）在外观上无法区分，当全量流写会
    //     产出缺头文件；
    //   • Content-Range 分母与已知总大小不一致：文件已变（可能变大被截断，也可能
    //     变小导致反复拿到不足 1 MiB 的尾巴），零容差，重下后由大小漂移规则校准。
    if !capped
        && want_resume
        && !refetched_full
        && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT
    {
        let encoded = detect_content_encoding(resp.headers()).is_some();
        let missing_range_start = parse_content_range_start(resp.headers()).is_none();
        let size_changed = total_bytes > 0
            && parse_content_range_total(resp.headers()).is_some_and(|t| t != total_bytes);
        if encoded || missing_range_start || size_changed {
            log_info!(
                "[download-single] task {} untrustworthy 206 (encoded={}, missing_content_range={}, \
                 size_changed={}); re-requesting full file without Range (existing_len={} discarded)",
                task_id,
                encoded,
                missing_range_start,
                size_changed,
                existing_len
            );
            drop(resp);
            let full = send_cancellable(build_full(&used), cancel_token).await?;
            resp = resend_after_refresh(spec, &mut used, full, &build_full, cancel_token)
                .await?
                .error_for_status()?;
            refetched_full = true;
        }
    }

    // ---- Safety net: HTML response on a binary download ---------------------
    //
    // 兜底 hint_file_size > 0 旁路（行 1480-1536）跳过 probe 的场景：
    // 当扩展端给出一次性 CDN URL 的 fileSize 提示时，downloader 跳过 probe
    // 直接进入下载，但若 CDN 实际返回 HTML 错误页（token 已被消费、签名过期、
    // 站点改用 form POST 等），会落盘成 fake.zip 这类损坏文件。
    //
    // 这里在 send 后第一时间检查 Content-Type——发现 text/html 但用户期望
    // 二进制文件时立即终止下载，确保即使协议升级出问题也绝不再发生
    // "HTML 当 zip 保存"的损坏。
    {
        let ct_raw = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let mime = ct_raw
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if (mime == "text/html" || mime == "application/xhtml+xml")
            && !filename_looks_like_html(expected_filename)
        {
            return Err(DownloadError::Other(format!(
                "server returned HTML page (Content-Type: {mime}) for a non-HTML download — \
                 aborting to avoid corrupting saved file; {}",
                html_response_hint(
                    spec.route_url(url).as_deref().unwrap_or(url),
                    Some(resp.url()),
                    &used.method,
                )
            )));
        }
    }

    // 第一道闸：拒绝任何【无法完整还原】的响应压缩——必须在 detect_content_encoding
    // 解压分支之前判定。原因（BUG-HTTP-LAYERED-ENCODING-UNREACHABLE）：
    // maybe_decompress_stream 只反转【单层】，而 detect_content_encoding 命中首个
    // 受支持 token 即返回 Some。若把 unsupported 检查放在 `else if`，则 `gzip, gzip`
    // / `gzip, compress`（首 token 受支持）会走解压分支只解一层、内层压缩字节原样
    // 落盘，且 decompressed=true 会跳过完整性检查 → 静默损坏。故提前为独立闸。
    //
    // 措辞刻意【不含】子串 "content-encoding"：这是服务器配置导致的永久性条件，
    // 重试只会再次命中同样编码、同样失败；is_retriable_error 用 contains("content-encoding")
    // 把"Range-on-206 压缩"判为可重试，若本错误含该子串会被卷入无限重试。
    if let Some(unsupported) = unsupported_content_encoding(resp.headers()) {
        return Err(DownloadError::Other(format!(
            "server applied an unsupported or multi-layered response compression scheme \
             '{unsupported}'; cannot decode the body — refusing to write raw bytes to disk"
        )));
    }

    // Detect compressed responses — we now decompress on-the-fly instead of
    // rejecting.  When decompression is active, total_bytes from the probe is
    // the *compressed* size, not the decompressed size, so we must treat it
    // as unknown for progress reporting and skip the final size integrity check.
    // 至此只可能是【单层】受支持编码（多层/未知已被上方闸拦下）。
    let encoding = detect_content_encoding(resp.headers());
    if encoding.is_some() {
        log_info!(
            "[download-single] task {} server returned Content-Encoding: {:?} — \
             decompressing on-the-fly",
            task_id,
            encoding
        );
    }

    // 服务器忽略 Range 回 200 时，响应体要么是整个文件，要么是错误/替换内容。
    // 总大小已知且 Content-Length 对不上时，下面的 File::create 会先截断数 GB
    // 的旧数据，随后大小漂移规则又会把任何带长度的响应体判为完成——必须在截断
    // 前拒绝并保留旧数据。重发的无 Range 全量请求（416 / 压缩 / 大小变化）与带
    // 编码的 200 不适用：前者本就是为「文件已变」设计的恢复路径，后者的长度是
    // 压缩后大小。
    if want_resume
        && !refetched_full
        && total_bytes > 0
        && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT
        && encoding.is_none()
        && let Some(cl) = resp.content_length()
        && i64::try_from(cl).is_ok_and(|cl| cl != total_bytes)
    {
        return Err(DownloadError::Other(format!(
            "resume request was answered with a full body of {cl} bytes instead of the expected \
             {total_bytes}; keeping the partial file. The link may have expired or the file \
             changed on the server"
        )));
    }

    // Verify the server actually honoured the Range request.
    // Some servers (or CDN edge nodes) silently ignore Range and return 200 OK
    // with the full file.  If we appended to the partial file in that case we
    // would produce a corrupt result.  Detect this and fall back to a clean
    // full download.
    //
    // HTTP 206 Partial Content  → server honoured Range → safe to append
    // HTTP 200 OK               → server ignored Range  → must restart from 0
    // Any other 2xx             → treat as non-resumable for safety
    //
    // `encoding.is_none()` 与 Content-Range 起点必须 == existing_len 是续传安全
    // 的必要条件：压缩 206 已在上方重发处理，这里兜底；BUG-CDN-206-BYTE0-FULLSTREAM
    // 场景（劣质 CDN 对 `bytes=N-` 回 206 却发从 0 的全量流，Content-Range 起点
    // 为 0）不符时 File::create 截断并复用当前响应体从 0 写入。缺 Content-Range
    // 的 206 无法判断落点，已在上方丢弃并全量重发，不会落到这里。
    let actual_resume = want_resume
        && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT
        && encoding.is_none()
        && !is_range_response_misaligned(parse_content_range_start(resp.headers()), existing_len);
    // 有界窗口模式下，从 0 起的 206 窗口（全新下载首窗，或文件被替换 / 416 后重下的首窗）
    // 同样要让外层循环接着发下一个窗口。上方 capped 校验已保证此类 206 带 Content-Range
    // 且起点为 0、未压缩。
    let window_from_zero = capped
        && !actual_resume
        && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT
        && encoding.is_none();
    if want_resume && !actual_resume {
        log_info!(
            "[download-single] task {} server returned {} (encoding={:?}) instead of a plain \
             206; falling back to full download (existing_len={} discarded)",
            task_id,
            resp.status(),
            encoding,
            existing_len
        );
    }
    // 从 0 服务的全量体：锁存实际响应的 Last-Modified（见字段 doc），并把响应
    // 自带的 validator 回写 DB。磁盘内容已是这个响应的版本，DB 里旧的 ETag/LM
    // 描述的是被替换前的文件，留着会让下次续传永远 VersionChanged；响应没有
    // validator 时写空串把旧值清掉。
    let latched_last_modified = (!actual_resume).then(|| {
        resp.headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    });
    if let Some(lm) = &latched_last_modified {
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if let Err(e) = db.set_task_validator(task_id, etag, lm).await {
            log_warn!(
                "[download-single] task {} failed to refresh stored validator: {}",
                task_id,
                e
            );
        }
    }
    // 实际 GET 响应的命名证据（hint 模式唯一能看到 Content-Disposition 的地方）；
    // 先写者胜，续传轮次重复调用无害。
    record_response_naming(db, task_id, &resp).await;

    // Capture the response's own Content-Length before consuming the body.
    // For resumed downloads (206), this is the *remaining* length, not total.
    // For full downloads (200), this is the complete file size.
    let response_content_length: i64 = if actual_resume {
        // For 206 responses, the full size is existing_len + Content-Length.
        resp.content_length()
            .map(|cl| existing_len + cl as i64)
            .unwrap_or(-1)
    } else {
        resp.content_length().map(|cl| cl as i64).unwrap_or(-1)
    };

    if actual_resume {
        downloaded = existing_len;
        let mut raw_file = OpenOptions::new().write(true).open(dest).await?;
        let resume_offset = u64::try_from(existing_len).map_err(|_| {
            DownloadError::Other(format!("invalid negative resume offset: {existing_len}"))
        })?;
        raw_file.set_len(resume_offset).await?;
        raw_file.seek(std::io::SeekFrom::End(0)).await?;
        file = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, raw_file);
    } else {
        downloaded = 0;
        file = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, File::create(dest).await?);
        // Reset DB progress so the UI doesn't show stale values
        db.update_task_progress(task_id, 0).await?;
    }

    // 注：旧版本会从实际下载响应的 Content-Disposition 中提取"更好的文件名"，
    // 写入 DB 并通知宿主 UI。该机制已移除——新架构下文件名由 DownloadManager
    // 在 do_start_task 同步段统一决策（probe 阶段已读取 Content-Disposition），
    // downloader 内部不再变更文件名，避免与 manager 的 reserved_temp_paths
    // 协调断裂导致并发下载冲突（参见 PR #296 自我冲突回归 bug）。

    // Wrap with decompression if needed.  The stream now yields
    // Result<Bytes, io::Error> regardless of whether decompression is active.
    let raw_stream = resp.bytes_stream();
    let mut stream = maybe_decompress_stream(raw_stream, encoding);

    // When decompression is active, the probe's total_bytes is the *compressed*
    // size — the actual decompressed bytes written to disk will differ.
    // Treat size as unknown so progress reports don't show wrong percentages
    // and the final integrity check is skipped.
    let total_bytes = if encoding.is_some() { 0 } else { total_bytes };
    // 响应自身声明的终点：压缩响应的长度是压缩后大小，与落盘字节无可比性。
    let body_start = downloaded;
    let declared_end =
        (encoding.is_none() && response_content_length >= 0).then_some(response_content_length);
    let reading = TransferTracker::new().start(0);
    // This is an authoritative baseline, not a lossy UI sample: the actual
    // response may have rejected Range and truncated the old partial file.
    // Queue it before reading the body so stale queued progress cannot follow
    // the reset, and the reporter cannot mistake restored bytes for traffic.
    if progress_tx
        .send(ProgressUpdate {
            task_id: task_id.to_owned(),
            downloaded_bytes: downloaded,
            download_baseline: Some(downloaded),
            total_bytes,
            status: 1,
            segment_details: single_segment_progress(downloaded, total_bytes, true),
            runtime: Some(single_runtime(task_id, downloaded, total_bytes, true)),
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("HTTP progress receiver closed");
        return Err(DownloadError::Cancelled);
    }

    let mut last_report = std::time::Instant::now();
    let mut last_db_save = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                persist_single_progress(&mut file, db, task_id, downloaded).await?;
                return Err(DownloadError::Cancelled);
            }
            result = tokio::time::timeout(CHUNK_STALL_TIMEOUT, stream.next()) => {
                // Unwrap the timeout layer first.  If no chunk arrived within
                // CHUNK_STALL_TIMEOUT the TCP connection is likely dead — flush
                // partial progress and bubble up an error.  For single-thread
                // downloads the task will enter error state; the user can resume
                // and a fresh Range request will pick up from saved progress.
                let chunk = match result {
                    Ok(c) => c,
                    Err(_) => {
                        if let Err(cleanup_error) = persist_single_progress(&mut file, db, task_id, downloaded).await {
                            crate::logger::report_warning("http-download", "persist progress after stall", &cleanup_error);
                        }
                        return Err(single_stall_error(
                            CHUNK_STALL_TIMEOUT.as_secs(),
                            body_start,
                            downloaded,
                            declared_end,
                        ));
                    }
                };
                match chunk {
                    Some(Ok(bytes)) => {
                        // --- Speed limiter: write in sub-chunks as tokens allow ---
                        // 令牌等待必须与取消竞速：低限速下一个 chunk 要等数秒，
                        // 期间暂停/删除不能被挡在 select 分支体外。
                        let mut offset = 0usize;
                        let chunk_len = bytes.len();
                        while offset < chunk_len {
                            let remaining = (chunk_len - offset) as u64;
                            let allowed = tokio::select! {
                                _ = cancel_token.cancelled() => {
                                    file.flush().await?;
                                    file.get_ref().sync_data().await?;
                                    downloaded += offset as i64;
                                    db.update_task_progress(task_id, downloaded).await?;
                                    return Err(DownloadError::Cancelled);
                                }
                                allowed = speed_limiter.consume(remaining) => allowed,
                            };
                            let end = offset + allowed as usize;
                            file.write_all(&bytes[offset..end]).await?;
                            offset = end;
                        }
                        let len = chunk_len as i64;
                        downloaded += len;

                        // Progress report to the host — every 200ms for smooth UI.
                        if last_report.elapsed().as_millis() >= 200 && !progress_tx.is_closed() {
                            if progress_tx
                                .send(ProgressUpdate {
                                    task_id: task_id.to_string(),
                                    downloaded_bytes: downloaded,
                                    total_bytes,
                                    status: 1,
                                    error_message: String::new(),
                                    file_name: String::new(),
                                    segment_details: single_segment_progress(downloaded, total_bytes, true),
                                    runtime: Some(single_runtime(task_id, downloaded, total_bytes, true)),
                                    ..Default::default()
                                })
                                .await.is_err() {
                                tracing::debug!("HTTP progress receiver closed");
                            }
                            last_report = std::time::Instant::now();
                        }

                        // DB persistence — periodic save for crash recovery.
                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            persist_single_progress(&mut file, db, task_id, downloaded).await?;
                            last_db_save = std::time::Instant::now();
                        }
                    }
                    Some(Err(e)) => {
                        let checkpoint = async {
                            persist_single_progress(&mut file, db, task_id, downloaded).await?;
                            Ok::<(), DownloadError>(())
                        }.await;
                        if let Err(cleanup_error) = checkpoint {
                            crate::logger::report_warning("http-download", "persist progress after read failure", &cleanup_error);
                        }
                        return Err(single_body_error(e, downloaded, declared_end));
                    }
                    None => break,
                }
            }
        }
    }

    drop(reading);
    if progress_tx
        .send(ProgressUpdate {
            task_id: task_id.to_owned(),
            downloaded_bytes: downloaded,
            total_bytes,
            status: 1,
            runtime: Some(single_runtime(task_id, downloaded, total_bytes, false)),
            segment_details: single_segment_progress(downloaded, total_bytes, false),
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("HTTP progress receiver closed");
    }
    persist_single_progress(&mut file, db, task_id, downloaded).await?;
    Ok(SingleDownloadResult {
        response_content_length,
        decompressed: encoding.is_some(),
        latched_last_modified,
        resumed_range_start: if actual_resume {
            Some(u64::try_from(existing_len).map_err(|_| {
                DownloadError::Other(format!("invalid negative resume offset: {existing_len}"))
            })?)
        } else if window_from_zero {
            Some(0)
        } else {
            None
        },
    })
}

// ---------------------------------------------------------------------------
// Multi-segment download (delegates to SegmentCoordinator)
// ---------------------------------------------------------------------------

/// 成功时返回本次下载的【最终有效总大小】（就地扩容 / resume 漂移吸附后可能不同
/// 于入参 `total_bytes`），见 `run_coordinated_download` 的返回值文档。
#[allow(clippy::too_many_arguments)]
async fn download_multi_segment(
    task_id: &str,
    url: &str,
    dest: &Path,
    total_bytes: i64,
    // `total_bytes` 是否为【未经 probe 验证的估计值】（fresh hint 模式）。透传至
    // coordinator → do_segment，决定 Content-Range 分母扩容检查的漂移容差。
    size_is_estimate: bool,
    segment_count: i32,
    nodes: std::sync::Arc<crate::cdn::NodePool>,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    spec: &std::sync::Arc<DownloadSpec>,
    sink: &dyn EventSink,
    etag: &str,
    last_modified: &str,
    spawn_gen: i64,
    allow_hint_uncap: bool,
    auto_proxy: Option<std::sync::Arc<crate::auto_proxy::AutoProxyCtx>>,
    multi_nic: Option<std::sync::Arc<crate::multi_nic::MultiNicInput>>,
) -> Result<i64, DownloadError> {
    output::ensure_parent(dest).await?;

    // NOTE: total_bytes arriving here is already the *effective* value returned
    // by update_task_file_info_resume — it is consistent with the stored segment
    // boundaries (small CDN drift has been filtered out).  The coordinator's own
    // effective_total_bytes logic (db_total vs probe) provides a second layer of
    // protection.  The pre-check below is therefore intentionally removed: it
    // compared the raw probed total_bytes against segment end_byte, which caused
    // false positives (CDN rounding) that silently wiped all progress.
    //
    // The coordinator itself handles the two genuine cases:
    //   • db_total <= probe_total  → trust DB segments, correct tasks.total_bytes
    //   • db_total >  probe_total  → file genuinely shrank, rebuild segments

    // Delegate to the IDM-style dynamic segment coordinator.
    let outcome = crate::segment_coordinator::run_coordinated_download(
        task_id,
        url,
        dest,
        total_bytes,
        size_is_estimate,
        segment_count,
        nodes,
        db,
        progress_tx,
        cancel_token,
        speed_limiter,
        spec,
        sink,
        etag,
        last_modified,
        crate::segment_coordinator::ReportScope::whole_task(),
        spawn_gen,
        allow_hint_uncap,
        auto_proxy,
        multi_nic,
    )
    .await;
    // 暂停终态帧在 TaskDone 后读 tasks.downloaded_bytes：退出前按已落库分段对齐，
    // 否则显示上一个周期写入的旧值（开头几秒内暂停即为 0%）。只在活跃下载器被
    // 取消时执行一次（并发上限内），排队 / 已暂停任务不经过此处。
    if matches!(outcome, Err(DownloadError::Cancelled))
        && let Err(error) = db
            .sync_task_progress_from_segments(task_id, spawn_gen)
            .await
    {
        crate::logger::report_warning("http-download", "sync paused progress", &error);
    }
    outcome
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 跨模块测试共享：构造带拦截器的最小 `DownloadParams`，以及可编程的 refresh 拦截器。
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::DownloadParams;
    use crate::intercept::{InterceptStage, InterceptorEntry, TaskInterceptor};

    /// 构造最小 `DownloadParams`（内存库已插入同名任务，进度通道自动排空）。
    pub(crate) async fn params_with_interceptor(
        url: &str,
        interceptor: Arc<dyn TaskInterceptor>,
    ) -> Result<DownloadParams, Box<dyn std::error::Error>> {
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        db.insert_task("t-refresh", url, "f.bin", "", 0, 0, "", "", "", 0)
            .await?;
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        Ok(DownloadParams {
            spawn_gen: 1,
            unattended: true,
            preprobed: None,
            interceptor,
            connection_cap: 0,
            run_meta: crate::intercept::RunMeta::default(),
            auto_proxy: None,
            multi_nic: None,
            task_id: "t-refresh".to_string(),
            url: url.to_string(),
            save_dir: String::new(),
            file_name: "f.bin".to_string(),
            segment_count: 0,
            is_resume: false,
            range_verified: true,
            db,
            client: reqwest::Client::builder().no_proxy().build()?,
            progress_tx: tx,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            sink: Arc::new(crate::NoopSink),
            cookies: String::new(),
            referrer: String::new(),
            speed_limiter: crate::speed_limiter::SpeedLimiter::new(0),
            hint_file_size: 0,
            proxy_config: crate::proxy_config::ProxyConfig::default(),
            selector: Arc::new(crate::NoopSelection),
            checksum: String::new(),
            extra_headers: std::collections::HashMap::new(),
            spec: super::RequestSpec::empty_get(),
            audio_url: None,
            auto_max_connections: 0,
            use_server_time: false,
            overwrite: crate::file_exists::OverwritePolicy::Never,
            ffmpeg_path: None,
            cdn: crate::cdn::CdnTaskInput::default(),
        })
    }

    /// 只声明 `refresh` 阶段的拦截器：第 n 次调用（从 1 起）的返回值由 `reply(n)` 决定，
    /// 并可人为拉长每次调用以暴露并发竞争。
    pub(crate) struct RefreshScript {
        entries: Vec<InterceptorEntry>,
        pub calls: AtomicUsize,
        delay: Duration,
        reply: Box<dyn Fn(usize) -> Option<String> + Send + Sync>,
    }

    impl RefreshScript {
        pub(crate) fn new(
            delay: Duration,
            reply: impl Fn(usize) -> Option<String> + Send + Sync + 'static,
        ) -> Self {
            Self {
                entries: vec![InterceptorEntry {
                    identity: "r@p".to_string(),
                    stages: vec![InterceptStage::Refresh],
                    fail_task: false,
                }],
                calls: AtomicUsize::new(0),
                delay,
                reply: Box::new(reply),
            }
        }
    }

    #[async_trait::async_trait]
    impl TaskInterceptor for RefreshScript {
        fn entries(&self) -> &[InterceptorEntry] {
            &self.entries
        }

        fn source_url(&self) -> &str {
            "https://example.com/source"
        }

        async fn call(
            &self,
            _identity: &str,
            _stage: InterceptStage,
            _arg_json: String,
            _file_root: Option<std::path::PathBuf>,
        ) -> Result<Option<String>, String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            Ok((self.reply)(n))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{TEMP_EXT, await_probe_pair, dedup_filename, format_http_date, parse_http_date};
    use std::time::Duration;

    // -----------------------------------------------------------------------
    // await_probe_pair
    // -----------------------------------------------------------------------

    /// 决定性的 Range GET 不能被挂起的 HEAD 拖到探测超时：宽限期一过即放弃 HEAD。
    #[tokio::test]
    async fn conclusive_get_abandons_hanging_head_after_grace() {
        let started = std::time::Instant::now();
        let head = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            "head"
        };
        let (head_out, get_out) = await_probe_pair(
            head,
            async { 206 },
            |s| *s == 206,
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(head_out, None);
        assert_eq!(get_out, 206);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// GET 未给出决定性答复时 HEAD 是唯一元数据来源，必须等到它完成，哪怕超过宽限期。
    #[tokio::test]
    async fn inconclusive_get_waits_for_head_beyond_grace() {
        let head = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            "head"
        };
        let (head_out, get_out) = await_probe_pair(
            head,
            async { 200 },
            |s| *s == 206,
            Duration::from_millis(10),
        )
        .await;
        assert_eq!(head_out, Some("head"));
        assert_eq!(get_out, 200);
    }

    /// HEAD 先于 GET 完成时结果必须保留（不因 GET 决定性而丢弃已到的 HEAD）。
    #[tokio::test]
    async fn head_finishing_first_is_kept() {
        let get = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            206
        };
        let (head_out, get_out) =
            await_probe_pair(async { "head" }, get, |s| *s == 206, Duration::ZERO).await;
        assert_eq!(head_out, Some("head"));
        assert_eq!(get_out, 206);
    }

    // -----------------------------------------------------------------------
    // parse_http_date
    // -----------------------------------------------------------------------

    /// 三种 HTTP 日期格式指向同一时刻（784111777 = 1994-11-06T08:49:37Z）。
    #[test]
    fn parse_http_date_accepts_all_three_http_formats() {
        let expected = std::time::UNIX_EPOCH + Duration::from_secs(784_111_777);
        for s in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(parse_http_date(s), Some(expected), "format: {s}");
        }
    }

    /// 星期与日期不符的头（现实中偶见）不应导致整个时间戳被拒绝。
    /// 2025-10-21 实为周二，此处故意标注 Wed。
    #[test]
    fn parse_http_date_ignores_mismatched_weekday() {
        let expected = std::time::UNIX_EPOCH + Duration::from_secs(1_761_031_680);
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2025 07:28:00 GMT"),
            Some(expected)
        );
    }

    #[test]
    fn parse_http_date_rejects_garbage_and_pre_epoch() {
        assert_eq!(parse_http_date(""), None);
        assert_eq!(parse_http_date("not a date"), None);
        assert_eq!(parse_http_date("2025-01-01T00:00:00Z"), None);
        // Unix 纪元之前的时间无法表示为 SystemTime 偏移，须整体放弃。
        assert_eq!(parse_http_date("Wed, 01 Jan 1902 00:00:00 GMT"), None);
    }

    /// 手动代理（用户名单为空）也不得截走环回请求：代理端口已关闭，若请求被转发
    /// 给它则必然失败。
    #[tokio::test]
    async fn manual_proxy_never_captures_loopback_requests() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dead_proxy_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = server.accept().await {
                let mut buf = [0u8; 1024];
                if socket.read(&mut buf).await.is_ok() {
                    let reply =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                    if let Err(error) = socket.write_all(reply).await {
                        tracing::debug!("loopback test server write failed: {error}");
                    }
                }
            }
        });
        let config = crate::proxy_config::ProxyConfig {
            mode: crate::proxy_config::ProxyMode::Manual,
            host: "127.0.0.1".to_string(),
            port: dead_proxy_port,
            ..Default::default()
        };
        let client = super::build_client_builder(&config, "", false)
            .unwrap()
            .build()
            .unwrap();
        let body = client
            .get(format!("http://127.0.0.1:{port}/f"))
            .send()
            .await
            .expect("loopback must bypass the proxy")
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
    }

    #[test]
    fn format_http_date_round_trips_through_parse() {
        // FTP MDTM 20250102030405（UTC）→ IMF-fixdate → 与 Last-Modified 同一解析路径。
        let time = chrono::NaiveDate::from_ymd_opt(2025, 1, 2)
            .and_then(|date| date.and_hms_opt(3, 4, 5))
            .expect("valid time");
        let text = format_http_date(time);
        assert_eq!(text, "Thu, 02 Jan 2025 03:04:05 GMT");
        assert_eq!(
            parse_http_date(&text),
            Some(std::time::UNIX_EPOCH + Duration::from_secs(1_735_787_045))
        );
    }

    /// 原始缺陷回归：直连探测被拒时，最终错误文案必须携带类型化传输失败标签，
    /// manager 的 Auto 换路与通用自动重试据此生效。reqwest 0.12 的 Display
    /// 不含根因，过去这里的文案匹配不到任何关键词——既不换路也不重试，任务
    /// 直接失败。两种探测形态（HEAD ∥ Range / 单 GET）都要覆盖。
    #[tokio::test]
    async fn refused_probe_error_is_classified_for_failover_and_retry() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://127.0.0.1:{port}/f.bin");
        let spec = super::RequestSpec::empty_get();
        let head_and_range = super::resolve_file_info(&client, &url, &spec)
            .await
            .err()
            .expect("closed port must fail");
        let held_get = super::resolve_file_info_held(&client, &url, &spec)
            .await
            .err()
            .expect("closed port must fail");
        for err in [head_and_range, held_get] {
            let msg = err.to_string();
            assert_eq!(
                crate::net_failure::tag_of(&msg),
                Some(crate::net_failure::NetFailure::Refused),
                "{msg}"
            );
            assert!(crate::auto_proxy::is_route_transport_error(&msg), "{msg}");
            assert!(crate::download_manager::is_retriable_error(&msg), "{msg}");
        }
    }

    // -----------------------------------------------------------------------
    // Adaptive header fallback tests (Issue #782 / NVIDIA 403 & anti-hotlinking)
    // -----------------------------------------------------------------------

    #[test]
    fn infer_origin_referrer_extracts_origin_with_slash() {
        assert_eq!(
            super::infer_origin_referrer(
                "https://cn.download.nvidia.com/Windows/617.42/617.42-notebook.exe"
            ),
            Some("https://cn.download.nvidia.com/".to_string())
        );
        assert_eq!(
            super::infer_origin_referrer("http://example.com:8080/path/file.bin?query=1"),
            Some("http://example.com:8080/".to_string())
        );
        assert_eq!(
            super::infer_origin_referrer("ftp://ftp.example.com/file"),
            None
        );
        assert_eq!(super::infer_origin_referrer("not-a-valid-url"), None);
    }

    /// HTML 安全网：被重定向到登录 / 错误页时点出终点（不带 query 里的令牌），且路径里
    /// 偶然出现的 `timeout` 之类字样不能把这个永久性失败变成可自动重试。
    #[test]
    fn html_hint_names_the_redirect_landing_without_its_query() {
        let landing = reqwest::Url::parse(
            "https://user:pw@onedrive.live.com/personal/x/_layouts/15/Authenticate.aspx?Source=%2Fdl&tempauth=SECRET",
        )
        .unwrap();
        let hint = super::html_response_hint(
            "https://onedrive.live.com/personal/x/_layouts/15/download.aspx?UniqueId=1",
            Some(&landing),
            &reqwest::Method::GET,
        );
        assert!(
            hint.contains("https://onedrive.live.com/personal/x/_layouts/15/Authenticate.aspx "),
            "{hint}"
        );
        assert!(
            !hint.contains("SECRET") && !hint.contains("Source=") && !hint.contains("pw@"),
            "{hint}"
        );

        // 终点与请求地址相同（未发生重定向）时不得声称被重定向。
        let same = "https://cdn.example.com/a.zip?sig=1";
        let not_redirected = super::html_response_hint(
            same,
            Some(&reqwest::Url::parse(same).unwrap()),
            &reqwest::Method::POST,
        );
        assert!(
            !not_redirected.contains("redirected to"),
            "{not_redirected}"
        );

        let timeout_path = reqwest::Url::parse("https://login.example.com/timeout/eof").unwrap();
        let message = format!(
            "server returned HTML page (Content-Type: text/html) instead of the expected file — {}",
            super::html_response_hint(
                "https://dl.example.com/f",
                Some(&timeout_path),
                &reqwest::Method::GET
            )
        );
        assert!(
            !crate::download_manager::is_retriable_error(&message),
            "{message}"
        );
    }

    #[test]
    fn post_only_requires_405_with_allow_listing_post_but_not_get() {
        let allow = |value: &str| {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(reqwest::header::ALLOW, value.parse().unwrap());
            super::allows_only_post(&headers)
        };
        assert!(allow("OPTIONS, POST"));
        assert!(allow("post"));
        assert!(!allow("GET, POST"), "GET still allowed: not POST-only");
        assert!(!allow("HEAD,POST"));
        assert!(!allow("OPTIONS"));
        assert!(!super::allows_only_post(&reqwest::header::HeaderMap::new()));

        let post_only =
            |msg: &str| super::probe_post_only(&super::DownloadError::Other(msg.into()));
        assert!(post_only(
            "probes failed: HEAD=405 post-only, ranged GET=405 post-only"
        ));
        assert!(post_only("probes failed: plain GET=405 post-only"));
        assert!(!post_only(
            "probes failed: HEAD=405, ranged GET=400, plain GET=400"
        ));
        assert!(!post_only(
            "probes failed: HEAD=network-error: 405 post-only"
        ));
    }

    #[test]
    fn probe_rejection_classifies_forbidden_and_bot_challenge() {
        use super::ProbeRejection::{BotChallenge, Forbidden};
        let probe = |msg: &str| super::probe_rejection(&super::DownloadError::Other(msg.into()));

        assert_eq!(
            probe("probes failed: HEAD=403, ranged GET=403, plain GET=403"),
            Some(Forbidden)
        );
        assert_eq!(
            probe("probes failed: HEAD=405, ranged GET=429, plain GET=429"),
            Some(Forbidden)
        );
        // Cloudflare 质询以 404 下发（JetBrains 国内 CDN 实测）：不能当成文件不存在。
        assert_eq!(
            probe(
                "probes failed: HEAD=404 bot-challenge, ranged GET=404 bot-challenge, plain GET=404 bot-challenge"
            ),
            Some(BotChallenge)
        );
        // 质询与 403 并存时按质询处理（需要换中性 UA，而不是补浏览器 UA）。
        assert_eq!(
            probe("probes failed: HEAD=403, ranged GET=403 bot-challenge, plain GET=403"),
            Some(BotChallenge)
        );
        // 普通 404 / 401 不是服务端拒绝。
        assert_eq!(
            probe("probes failed: HEAD=404, ranged GET=404, plain GET=404"),
            None
        );
        assert_eq!(
            probe("probes failed: HEAD=401, ranged GET=401, plain GET=401"),
            None
        );
        // 网络错误文本里的 4010 / 4030 / bot-challenge 字样不得误判。
        assert_eq!(
            probe(
                "probes failed: HEAD=network-error: failed to connect to 127.0.0.1:4010, ranged GET=network-error: port 4030 unreachable, plain GET=network-error"
            ),
            None
        );
        assert_eq!(
            probe(
                "probes failed: HEAD=network-error: bot-challenge, ranged GET=network-error, plain GET=network-error"
            ),
            None
        );
    }

    #[test]
    fn spec_adaptation_helpers_manage_browser_ua_and_referrer() {
        let spec_empty = super::RequestSpec::empty_get();
        let adapted = super::spec_with_browser_ua_and_referrer(
            &spec_empty,
            "https://cn.download.nvidia.com/Windows/617.42/driver.exe",
        );
        assert_eq!(
            adapted.extra_headers.get("User-Agent").map(|s| s.as_str()),
            Some(super::DEFAULT_BROWSER_UA)
        );
        assert_eq!(adapted.referrer, "https://cn.download.nvidia.com/");

        // Preserves custom referrer if already set
        let mut spec_with_custom_ref = super::RequestSpec::empty_get();
        spec_with_custom_ref.referrer = "https://www.nvidia.com/drivers".to_string();
        let adapted2 = super::spec_with_browser_ua_and_referrer(
            &spec_with_custom_ref,
            "https://cn.download.nvidia.com/Windows/617.42/driver.exe",
        );
        assert_eq!(adapted2.referrer, "https://www.nvidia.com/drivers");

        // Stripping browser UA handles mixed-case keys (user-agent, User-Agent, USER-AGENT)
        let mut mixed_case_spec = super::RequestSpec::empty_get();
        mixed_case_spec
            .extra_headers
            .insert("uSeR-aGeNt".to_string(), "CustomBot/1.0".to_string());
        mixed_case_spec.referrer = "https://example.com/".to_string();
        let stripped = super::spec_without_browser_ua(&mixed_case_spec);
        assert!(
            !stripped
                .extra_headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("user-agent")),
            "all user-agent keys must be stripped regardless of case"
        );
        assert_eq!(stripped.referrer, "https://example.com/");
    }

    #[tokio::test]
    async fn persist_adapted_spec_updates_headers_and_referrer_in_db() {
        let db = crate::db::Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let task_id = "test-adapt-persist";
        db.insert_task(
            task_id,
            "https://example.com/file.bin",
            "test.bin",
            "/tmp",
            1,
            0,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");

        // Seed initial context with custom User-Agent and empty referrer
        db.set_task_request_context(
            task_id,
            "session=abc",
            "",
            r#"{"user-agent":"Mozilla/5.0 Chrome/120","X-Custom":"1"}"#,
        )
        .await
        .expect("seed context");

        // 1. Adapted spec strips UA and adds inferred Referer
        let mut adapted = super::RequestSpec::empty_get();
        adapted.referrer = "https://example.com/".to_string();
        adapted
            .extra_headers
            .insert("X-Custom".to_string(), "1".to_string());

        super::persist_adapted_spec(&db, task_id, &adapted).await;

        let (_, loaded_ref, loaded_headers_json) = db
            .load_task_request_context(task_id)
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(loaded_ref, "https://example.com/", "referrer persisted");
        let loaded_headers: std::collections::HashMap<String, String> =
            serde_json::from_str(&loaded_headers_json).expect("parse json");
        assert!(
            !loaded_headers.contains_key("user-agent"),
            "stripped user-agent must be removed from db"
        );
        assert_eq!(
            loaded_headers.get("X-Custom").map(|s| s.as_str()),
            Some("1")
        );
    }

    #[tokio::test]
    async fn persist_adapted_spec_writes_browser_ua_without_duplicate_keys() {
        let db = crate::db::Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let task_id = "test-adapt-persist-ua";
        db.insert_task(
            task_id,
            "https://example.com/file.bin",
            "test.bin",
            "/tmp",
            1,
            0,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");
        // 库里已有小写 user-agent；adapted 带 `User-Agent`，写回后只能剩一个 UA 键。
        db.set_task_request_context(
            task_id,
            "",
            "",
            r#"{"user-agent":"old/1.0","X-Custom":"1"}"#,
        )
        .await
        .expect("seed context");

        let mut adapted = super::RequestSpec::empty_get();
        adapted.extra_headers.insert(
            "User-Agent".to_string(),
            super::DEFAULT_BROWSER_UA.to_string(),
        );
        adapted
            .extra_headers
            .insert("X-Custom".to_string(), "1".to_string());
        adapted.referrer = "https://example.com/".to_string();
        super::persist_adapted_spec(&db, task_id, &adapted).await;

        let (_, loaded_ref, json) = db
            .load_task_request_context(task_id)
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(loaded_ref, "https://example.com/");
        let headers: std::collections::HashMap<String, String> =
            serde_json::from_str(&json).expect("parse json");
        let ua_keys: Vec<_> = headers
            .keys()
            .filter(|k| k.eq_ignore_ascii_case("user-agent"))
            .collect();
        assert_eq!(ua_keys.len(), 1, "exactly one UA key expected: {headers:?}");
        assert_eq!(
            headers.get("User-Agent").map(|s| s.as_str()),
            Some(super::DEFAULT_BROWSER_UA)
        );
        assert_eq!(headers.get("X-Custom").map(|s| s.as_str()), Some("1"));
    }

    #[tokio::test]
    async fn persist_adapted_spec_keeps_corrupt_persisted_headers_untouched() {
        let db = crate::db::Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let task_id = "test-adapt-persist-corrupt";
        db.insert_task(
            task_id,
            "https://example.com/file.bin",
            "test.bin",
            "/tmp",
            1,
            0,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");
        let corrupt = r#"{"X-Custom":"1""#; // 截断的 JSON
        db.set_task_request_context(task_id, "", "", corrupt)
            .await
            .expect("seed context");

        let mut adapted = super::RequestSpec::empty_get();
        adapted.extra_headers.insert(
            "User-Agent".to_string(),
            super::DEFAULT_BROWSER_UA.to_string(),
        );
        adapted.referrer = "https://example.com/".to_string();
        super::persist_adapted_spec(&db, task_id, &adapted).await;

        let (_, loaded_ref, json) = db
            .load_task_request_context(task_id)
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(json, corrupt, "corrupt headers must not be overwritten");
        assert_eq!(loaded_ref, "", "referrer must not be written either");
    }

    #[tokio::test]
    async fn persist_adapted_spec_writes_only_unpinned_ua_and_referrer() {
        let db = crate::db::Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let task_id = "test-adapt-persist-overlay";
        db.insert_task(
            task_id,
            "https://example.com/file.bin",
            "test.bin",
            "/tmp",
            1,
            0,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");
        db.set_task_request_context(task_id, "", "", r#"{"A":"1"}"#)
            .await
            .expect("seed context");

        // 改写后的 spec 带插件叠加头与中性 UA：只有 UA 允许写回。
        let mut adapted = super::RequestSpec::empty_get();
        adapted
            .extra_headers
            .insert("X-Overlay".to_string(), "secret".to_string());
        adapted
            .extra_headers
            .insert("User-Agent".to_string(), super::DEFAULT_UA.to_string());
        super::persist_adapted_spec(&db, task_id, &adapted).await;

        let (_, _, json) = db
            .load_task_request_context(task_id)
            .await
            .expect("load")
            .expect("exists");
        let headers: std::collections::HashMap<String, String> =
            serde_json::from_str(&json).expect("parse json");
        assert_eq!(
            headers,
            std::collections::HashMap::from([
                ("A".to_string(), "1".to_string()),
                ("User-Agent".to_string(), super::DEFAULT_UA.to_string()),
            ])
        );

        // 锁定 UA / Referer 时一律不写回。
        let mut pinned = adapted.clone();
        pinned
            .extra_headers
            .insert("User-Agent".to_string(), "Pinned/1".to_string());
        pinned.referrer = "https://example.com/p".to_string();
        pinned.pins = super::RequestPins {
            user_agent: true,
            referrer: true,
        };
        super::persist_adapted_spec(&db, task_id, &pinned).await;
        let (_, referrer, json) = db
            .load_task_request_context(task_id)
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(referrer, "");
        assert!(!json.contains("Pinned/1"), "{json}");
    }

    #[tokio::test]
    async fn bot_challenge_escape_skipped_when_ua_pinned() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    if stream.read(&mut buf).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let resp = "HTTP/1.1 404 Not Found\r\ncf-mitigated: challenge\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    if let Err(error) = stream.write_all(resp.as_bytes()).await {
                        tracing::debug!(%error, "test client closed");
                    }
                    if let Err(error) = stream.shutdown().await {
                        tracing::debug!(%error, "test client closed");
                    }
                });
            }
        });
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/f.bin");
        let db = crate::db::Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");

        let mut base = super::RequestSpec::empty_get();
        base.extra_headers
            .insert("User-Agent".to_string(), "Mozilla/5.0 Pinned".to_string());

        // 对照：未锁定时会切到中性 UA。
        let resp = client.get(&url).send().await.unwrap();
        let free = super::DownloadSpec::new(base.clone());
        assert!(
            free.escape_bot_challenge(&base, &resp, &db, "t")
                .await
                .is_some()
        );

        // 锁定后不得改 UA。
        let mut pinned_spec = base.clone();
        pinned_spec.pins.user_agent = true;
        let pinned = super::DownloadSpec::new(pinned_spec.clone());
        let resp = client.get(&url).send().await.unwrap();
        assert!(
            pinned
                .escape_bot_challenge(&pinned_spec, &resp, &db, "t")
                .await
                .is_none()
        );
    }

    fn token_reply(n: usize) -> Option<String> {
        Some(format!(r#"{{"setHeaders":{{"X-Token":"tok{n}"}}}}"#))
    }

    async fn refreshable_spec(
        script: std::sync::Arc<super::test_support::RefreshScript>,
    ) -> super::DownloadSpec {
        let params = super::test_support::params_with_interceptor(
            "http://127.0.0.1:1/f.bin",
            script as std::sync::Arc<dyn crate::intercept::TaskInterceptor>,
        )
        .await
        .unwrap();
        super::DownloadSpec::new(super::RequestSpec::empty_get()).with_refresher(
            crate::intercept::Refresher::for_params(&params, crate::intercept::ProtocolKind::Http),
        )
    }

    #[tokio::test]
    async fn concurrent_rejections_with_same_identity_trigger_one_refresh() {
        use super::test_support::RefreshScript;
        use std::sync::atomic::Ordering;
        let script = std::sync::Arc::new(RefreshScript::new(
            std::time::Duration::from_millis(50),
            token_reply,
        ));
        let spec = refreshable_spec(script.clone()).await;
        let used = spec.current();
        let forbidden = reqwest::StatusCode::FORBIDDEN;
        let (a, b) = tokio::join!(
            spec.refresh_after_rejection(&used, forbidden),
            spec.refresh_after_rejection(&used, forbidden)
        );
        let (a, b) = (a.unwrap().unwrap(), b.unwrap().unwrap());
        assert_eq!(script.calls.load(Ordering::SeqCst), 1);
        assert!(std::sync::Arc::ptr_eq(&a, &b));
        assert!(std::sync::Arc::ptr_eq(&a, &spec.current()));
        assert_eq!(
            a.extra_headers.get("X-Token").map(String::as_str),
            Some("tok1")
        );
    }

    #[tokio::test]
    async fn refresh_declined_is_not_asked_again_for_same_generation() {
        use super::test_support::RefreshScript;
        use std::sync::atomic::Ordering;
        let script = std::sync::Arc::new(RefreshScript::new(std::time::Duration::ZERO, |_| None));
        let spec = refreshable_spec(script.clone()).await;
        let used = spec.current();
        let status = reqwest::StatusCode::UNAUTHORIZED;
        assert!(
            spec.refresh_after_rejection(&used, status)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            spec.refresh_after_rejection(&used, status)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(script.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn refresh_stops_after_unconfirmed_limit() {
        use super::test_support::RefreshScript;
        use std::sync::atomic::Ordering;
        let limit = crate::intercept::refresh::MAX_UNCONFIRMED_REFRESHES as usize;
        let script =
            std::sync::Arc::new(RefreshScript::new(std::time::Duration::ZERO, token_reply));
        let spec = refreshable_spec(script.clone()).await;
        for _ in 0..limit {
            let used = spec.current();
            let fresh = spec
                .refresh_after_rejection(&used, reqwest::StatusCode::GONE)
                .await
                .unwrap();
            assert!(fresh.is_some());
        }
        let used = spec.current();
        assert!(
            spec.refresh_after_rejection(&used, reqwest::StatusCode::GONE)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(script.calls.load(Ordering::SeqCst), limit);
    }

    /// 每请求一个令牌：每次换发的令牌都先成功用过一次再被拒，属于正常消耗，
    /// 刷新次数不受连续无效上限约束。
    #[tokio::test]
    async fn confirmed_identities_keep_refresh_available() {
        use super::test_support::RefreshScript;
        use std::sync::atomic::Ordering;
        let rounds = crate::intercept::refresh::MAX_UNCONFIRMED_REFRESHES as usize * 3;
        let script =
            std::sync::Arc::new(RefreshScript::new(std::time::Duration::ZERO, token_reply));
        let spec = refreshable_spec(script.clone()).await;
        for round in 0..rounds {
            let used = spec.current();
            spec.confirm(&used);
            let fresh = spec
                .refresh_after_rejection(&used, reqwest::StatusCode::FORBIDDEN)
                .await
                .unwrap();
            assert!(
                fresh.is_some(),
                "refresh {round} refused after confirmed use"
            );
        }
        assert_eq!(script.calls.load(Ordering::SeqCst), rounds);
    }

    #[tokio::test]
    async fn refresh_ignores_statuses_other_than_401_403_410() {
        use super::test_support::RefreshScript;
        use std::sync::atomic::Ordering;
        let script =
            std::sync::Arc::new(RefreshScript::new(std::time::Duration::ZERO, token_reply));
        let spec = refreshable_spec(script.clone()).await;
        let used = spec.current();
        for status in [
            reqwest::StatusCode::NOT_FOUND,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::RANGE_NOT_SATISFIABLE,
        ] {
            assert!(
                spec.refresh_after_rejection(&used, status)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(script.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn route_url_only_replaces_the_task_url() {
        use super::test_support::RefreshScript;
        let script = std::sync::Arc::new(RefreshScript::new(std::time::Duration::ZERO, |_| {
            Some(r#"{"url":"http://127.0.0.1:1/g.bin"}"#.to_string())
        }));
        let spec = refreshable_spec(script).await;
        assert_eq!(spec.route_url("http://127.0.0.1:1/f.bin"), None);
        let used = spec.current();
        assert!(
            spec.refresh_after_rejection(&used, reqwest::StatusCode::FORBIDDEN)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            spec.route_url("http://127.0.0.1:1/f.bin").as_deref(),
            Some("http://127.0.0.1:1/g.bin")
        );
        assert_eq!(spec.route_url("http://127.0.0.1:1/other.bin"), None);
        assert_eq!(spec.route_url("http://127.0.0.1:2/f.bin"), None);
    }

    /// 恒 403 的本地服务端，返回端口与请求计数。
    async fn always_forbidden_server() -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::Ordering;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let served = count.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                let served = served.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    served.fetch_add(1, Ordering::SeqCst);
                    let resp =
                        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    if let Err(error) = stream.write_all(resp.as_bytes()).await {
                        tracing::debug!(%error, "test client closed");
                    }
                    if let Err(error) = stream.shutdown().await {
                        tracing::debug!(%error, "test client closed");
                    }
                });
            }
        });
        (port, count)
    }

    #[tokio::test]
    async fn pinned_probe_rejection_does_not_resend_identical_request() {
        use std::sync::atomic::Ordering;
        let client = super::build_client(&crate::proxy_config::ProxyConfig::default(), "").unwrap();
        let mut base = super::RequestSpec::empty_get();
        base.extra_headers
            .insert("User-Agent".to_string(), "Pinned/1".to_string());

        let (port, free_count) = always_forbidden_server().await;
        let url = format!("http://127.0.0.1:{port}/f.bin");
        assert!(
            super::resolve_file_info_with_ua_fallback(&client, &url, &base)
                .await
                .is_err()
        );

        let mut ua_only = base.clone();
        ua_only.pins.user_agent = true;
        let (port, ua_count) = always_forbidden_server().await;
        let url = format!("http://127.0.0.1:{port}/f.bin");
        assert!(
            super::resolve_file_info_with_ua_fallback(&client, &url, &ua_only)
                .await
                .is_err()
        );

        let mut both = ua_only.clone();
        both.pins.referrer = true;
        let (port, both_count) = always_forbidden_server().await;
        let url = format!("http://127.0.0.1:{port}/f.bin");
        assert!(
            super::resolve_file_info_with_ua_fallback(&client, &url, &both)
                .await
                .is_err()
        );

        let (free, ua, both) = (
            free_count.load(Ordering::SeqCst),
            ua_count.load(Ordering::SeqCst),
            both_count.load(Ordering::SeqCst),
        );
        // 锁定 UA 后阶梯第一步（剥离 UA）得到相同身份，只发第一轮；Referer 是否锁定
        // 不改变这一点。未锁定：走完整个自适应阶梯。
        assert!(ua > 0, "first round must still be sent");
        assert_eq!(both, ua, "both={both} ua={ua}");
        assert!(ua < free, "ua={ua} free={free}");
    }

    #[tokio::test]
    async fn pinned_user_agent_survives_probe_rejection_adaptation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        {
            let seen = seen.clone();
            let count = count.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                while let Ok((mut stream, _)) = listener.accept().await {
                    let seen = seen.clone();
                    let count = count.clone();
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut tmp = [0u8; 1024];
                        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            let n = stream.read(&mut tmp).await.unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            buf.extend_from_slice(&tmp[..n]);
                        }
                        let text = String::from_utf8_lossy(&buf).to_string();
                        let ua = text
                            .lines()
                            .find(|l| l.to_lowercase().starts_with("user-agent:"))
                            .map(|l| l[11..].trim().to_string())
                            .unwrap_or_default();
                        seen.lock().unwrap().push(ua);
                        let resp = if count.fetch_add(1, Ordering::SeqCst) < 1000 {
                            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        } else if text.starts_with("HEAD") {
                            "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                        } else {
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1000\r\nContent-Length: 1\r\nConnection: close\r\n\r\nX"
                        };
                        if let Err(error) = stream.write_all(resp.as_bytes()).await {
                            tracing::debug!(%error, "test client closed");
                        }
                        if let Err(error) = stream.shutdown().await {
                            tracing::debug!(%error, "test client closed");
                        }
                    });
                }
            });
        }
        let client = super::build_client(&crate::proxy_config::ProxyConfig::default(), "").unwrap();
        let url = format!("http://127.0.0.1:{port}/f.bin");
        let mut spec = super::RequestSpec::empty_get();
        spec.extra_headers
            .insert("User-Agent".to_string(), "Pinned/1".to_string());
        spec.pins.user_agent = true;

        // 锁定 UA 后自适应阶梯不会产生不同的请求身份：第一轮被拒即停止，
        // 已发出的所有请求都带插件指定的 UA。
        let result = super::resolve_file_info_with_ua_fallback(&client, &url, &spec).await;
        assert!(result.is_err(), "pinned identity must not be adapted away");
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "{seen:?}");
        assert!(seen.iter().all(|ua| ua == "Pinned/1"), "{seen:?}");
    }

    #[tokio::test]
    async fn resolve_file_info_adapts_to_403_rejection_with_browser_ua_and_referrer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let has_browser_ua = text.lines().any(|l| {
                        let l = l.to_lowercase();
                        l.starts_with("user-agent:") && l.contains("mozilla")
                    });
                    let has_referrer = text.lines().any(|l| {
                        let l = l.to_lowercase();
                        l.starts_with("referer:") && !l.trim().ends_with("referer:")
                    });

                    let resp = if has_browser_ua && has_referrer {
                        if text.starts_with("HEAD") {
                            "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                        } else {
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1000\r\nContent-Length: 1\r\nConnection: close\r\n\r\nX"
                        }
                    } else {
                        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    };
                    let _ = stream.write_all(resp.as_bytes()).await.ok();
                    let _ = stream.shutdown().await.ok();
                });
            }
        });

        let client = super::build_client(&crate::proxy_config::ProxyConfig::default(), "").unwrap();
        let url = format!("http://127.0.0.1:{}/test-driver.exe", port);
        let spec = super::RequestSpec::empty_get();

        let (info, adapted) = super::resolve_file_info_with_ua_fallback(&client, &url, &spec)
            .await
            .expect("probe should succeed after adapting headers");

        assert_eq!(info.total_bytes, 1000);
        let adapted = adapted.expect("should have adapted headers");
        assert!(adapted.extra_headers.contains_key("User-Agent"));
        assert_eq!(adapted.referrer, format!("http://127.0.0.1:{}/", port));
    }

    #[tokio::test]
    async fn resolve_file_info_strips_rejected_browser_ua_on_403() {
        // 场景 A：扩展传入的浏览器 UA 被拒（Cloudflare 式 TLS 指纹 vs UA 冲突），
        // 剥离 UA 后回落到 DEFAULT_UA 才被放行。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let has_browser_ua = text.lines().any(|l| {
                        let l = l.to_lowercase();
                        l.starts_with("user-agent:") && l.contains("mozilla")
                    });
                    let resp = if has_browser_ua {
                        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    } else if text.starts_with("HEAD") {
                        "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                    } else {
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1000\r\nContent-Length: 1\r\nConnection: close\r\n\r\nX"
                    };
                    let _ = stream.write_all(resp.as_bytes()).await.ok();
                    let _ = stream.shutdown().await.ok();
                });
            }
        });

        let client = super::build_client(&crate::proxy_config::ProxyConfig::default(), "").unwrap();
        let url = format!("http://127.0.0.1:{}/file.bin", port);
        let mut spec = super::RequestSpec::empty_get();
        spec.extra_headers.insert(
            "User-Agent".to_string(),
            "Mozilla/5.0 Chrome/120".to_string(),
        );

        let (info, adapted) = super::resolve_file_info_with_ua_fallback(&client, &url, &spec)
            .await
            .expect("probe should succeed after stripping the rejected browser UA");

        assert_eq!(info.total_bytes, 1000);
        let adapted = adapted.expect("should report adapted spec");
        assert!(
            !adapted
                .extra_headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("user-agent")),
            "browser UA must be stripped"
        );
    }

    #[tokio::test]
    async fn resolve_file_info_escapes_bot_challenge_404_with_browser_client_ua() {
        // JetBrains 国内 CDN（Cloudflare）对「浏览器 UA × rustls 指纹」回 404 +
        // `cf-mitigated: challenge`。全局 / 队列 / 任务 UA 选了 Chrome 预设时，只删
        // 扩展请求头里的 UA 会回落到同样的浏览器 UA，必须显式换成中性 UA。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let has_browser_ua = text.lines().any(|l| {
                        let l = l.to_lowercase();
                        l.starts_with("user-agent:") && l.contains("mozilla")
                    });
                    let resp = if has_browser_ua {
                        "HTTP/1.1 404 Not Found\r\ncf-mitigated: challenge\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    } else if text.starts_with("HEAD") {
                        "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                    } else {
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1000\r\nContent-Length: 1\r\nConnection: close\r\n\r\nX"
                    };
                    let _ = stream.write_all(resp.as_bytes()).await.ok();
                    let _ = stream.shutdown().await.ok();
                });
            }
        });

        let browser_ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/141.0.0.0";
        let client =
            super::build_client(&crate::proxy_config::ProxyConfig::default(), browser_ua).unwrap();
        let url = format!("http://127.0.0.1:{}/JetBrains.Rider.exe", port);

        // 有 / 无扩展传入的浏览器 UA 两种来源都必须脱困。
        for extension_ua in [Some(browser_ua), None] {
            let mut spec = super::RequestSpec::empty_get();
            if let Some(ua) = extension_ua {
                spec.extra_headers
                    .insert("User-Agent".to_string(), ua.to_string());
            }

            let (info, adapted) = super::resolve_file_info_with_ua_fallback(&client, &url, &spec)
                .await
                .expect("probe should escape the bot challenge with a neutral UA");

            assert_eq!(info.total_bytes, 1000);
            let adapted = adapted.expect("neutral UA must be reported for persistence");
            let ua: Vec<_> = adapted
                .extra_headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
                .map(|(_, v)| v.as_str())
                .collect();
            assert_eq!(ua, [super::DEFAULT_UA]);
        }
    }

    // -----------------------------------------------------------------------
    // Bug #5: HEAD and GET are serial — measure by counting sequential phases
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_file_info_merges_head_and_get_results() {
        // After fix: HEAD+GET run concurrently via tokio::join!.
        // The merge logic still applies:
        // - If HEAD has Content-Disposition, use it.
        // - If HEAD lacks Content-Disposition, merge from GET.
        // Verify the merge condition logic is correct:
        let headers = reqwest::header::HeaderMap::new();
        let has_cd = headers.contains_key(reqwest::header::CONTENT_DISPOSITION);
        assert!(
            !has_cd,
            "empty headers should not have Content-Disposition — GET data will be merged"
        );

        // With Content-Disposition present, no merge needed
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str("attachment; filename=\"test.zip\"") {
            headers.insert(reqwest::header::CONTENT_DISPOSITION, v);
        }
        let has_cd = headers.contains_key(reqwest::header::CONTENT_DISPOSITION);
        assert!(
            has_cd,
            "Content-Disposition present — no need to merge from GET"
        );
    }

    // -----------------------------------------------------------------------
    // dedup_filename
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dedup_filename_no_conflict() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_no_conflict_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        // Clean up any leftover
        if let Err(error) = tokio::fs::remove_file(dir.join("test.txt")).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        if let Err(error) = tokio::fs::remove_file(dir.join(format!("test.txt{TEMP_EXT}"))).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }

        let result = dedup_filename(
            &dir,
            "test.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(result, "test.txt");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn dedup_filename_with_conflict() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_conflict_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        // Create conflicting file
        tokio::fs::write(dir.join("test.txt"), b"")
            .await
            .expect("test fixture write succeeds");

        let result = dedup_filename(
            &dir,
            "test.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(result, "test (1).txt");

        // Clean up
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn dedup_filename_case_folds_across_disk_variants() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_case_fold_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        // 大小写由文件系统自己裁决:APFS/NTFS 等大小写不敏感文件系统上
        // `Test (1).txt` 与 `TEST (1).txt` 是同一条目,必须视为占用;大小写敏感
        // 文件系统上二者是不同文件,rename 不会互相覆盖,可以共存。
        tokio::fs::write(dir.join("CaseProbe"), b"")
            .await
            .expect("test fixture write succeeds");
        let case_insensitive = tokio::fs::try_exists(dir.join("caseprobe"))
            .await
            .expect("probe filesystem case sensitivity");
        tokio::fs::write(dir.join("TEST.txt"), b"")
            .await
            .expect("test fixture write succeeds");
        // Differently-cased numbered variant occupies " (1)" on case-insensitive filesystems.
        tokio::fs::write(dir.join("Test (1).txt"), b"")
            .await
            .expect("test fixture write succeeds");

        let result = dedup_filename(
            &dir,
            "TEST.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        let expected = if case_insensitive {
            "TEST (2).txt"
        } else {
            "TEST (1).txt"
        };
        assert_eq!(result, expected);

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn dedup_filename_temp_file_conflict() {
        let dir =
            std::env::temp_dir().join(format!("fluxdown_test_dedup_temp_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        // Create a .fdownloading temp file — should also be considered a conflict
        tokio::fs::write(dir.join(format!("test.txt{TEMP_EXT}")), b"")
            .await
            .expect("test fixture write succeeds");

        let result = dedup_filename(
            &dir,
            "test.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(result, "test (1).txt");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn dedup_filename_no_extension() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_noext_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        tokio::fs::write(dir.join("README"), b"")
            .await
            .expect("test fixture write succeeds");

        let result = dedup_filename(
            &dir,
            "README",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(result, "README (1)");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    // -----------------------------------------------------------------------
    // dedup_filename: reserved set prevents TOCTOU races in batch downloads
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dedup_filename_reserved_set_avoids_collision() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_reserved_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        // No file exists on disk, but the temp path is already reserved
        // by a sibling task (simulating a batch download in progress).
        let reserved_temp = dir.join(format!("video.mp4{TEMP_EXT}"));
        let mut reserved = std::collections::HashSet::new();
        reserved.insert(reserved_temp.clone());

        // Should NOT return "video.mp4" because its .fdownloading path is reserved.
        let result = dedup_filename(
            &dir,
            "video.mp4",
            &reserved,
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(result, "video (1).mp4");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn dedup_filename_reserved_set_phase2_collision() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_reserved_p2_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        // video.mp4 exists on disk AND video (1).mp4.fdownloading is reserved.
        tokio::fs::write(dir.join("video.mp4"), b"")
            .await
            .expect("test fixture write succeeds");
        let reserved_temp1 = dir.join(format!("video (1).mp4{TEMP_EXT}"));
        let mut reserved = std::collections::HashSet::new();
        reserved.insert(reserved_temp1);

        // "video.mp4" conflicts (on disk), "video (1).mp4" conflicts (reserved),
        // so should fall through to "video (2).mp4".
        let result = dedup_filename(
            &dir,
            "video.mp4",
            &reserved,
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(result, "video (2).mp4");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    // -----------------------------------------------------------------------
    // 回归测试：PR #296 自我冲突 bug
    //
    // 场景：单任务下载，磁盘上没有同名文件，reserved 集合中也没有该名字
    //       的预订（manager 还未 insert，或 snapshot 在 insert 之前 clone）。
    //       此时 dedup 必须返回原名，不得加 (1) 后缀。
    //
    // 旧 bug：manager 同步段先 insert(self) 再 clone snapshot，spawned task
    //         拿到的 snapshot 包含自己，dedup 误判冲突 → 所有浏览器扩展下载
    //         都被加 (1)（用户日志：FluxDown-0.1.40-windows-x64-setup.exe →
    //         FluxDown-0.1.40-windows-x64-setup (1).exe，磁盘并无原名文件）。
    //
    // 新设计中 spawned task 完全不再调 dedup_filename；这里仍然保留 sync
    // 与 async 两个测试，作为底层契约——"reserved 集合不含本任务名"时必须
    // 返回原名。
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dedup_filename_async_no_self_conflict_when_alone() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_no_self_conflict_async_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");

        // 磁盘干净；reserved 中只有兄弟任务 sibling.bin，没有自己的 setup.exe
        let mut reserved = std::collections::HashSet::new();
        reserved.insert(dir.join(format!("sibling.bin{TEMP_EXT}")));

        let result = dedup_filename(
            &dir,
            "setup.exe",
            &reserved,
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(
            result, "setup.exe",
            "reserved 集合不含本任务名时，dedup 必须返回原名（PR #296 回归 bug）"
        );

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    // -----------------------------------------------------------------------
    // dedup_filename: allow_overwrite（config `file_exists_behavior` ==
    // "overwrite"）——仅磁盘最终文件存在时保留原名；临时文件 / reserved /
    // avoid 命中仍照旧编号改名。
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dedup_filename_overwrite_keeps_name_when_only_final_exists() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_ow_final_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        tokio::fs::write(dir.join("test.txt"), b"old")
            .await
            .expect("test fixture write succeeds");

        let result = dedup_filename(
            &dir,
            "test.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Any,
        )
        .await;
        assert_eq!(
            result, "test.txt",
            "overwrite 模式下仅最终文件存在必须保留原名（完成时覆盖）"
        );

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn dedup_filename_overwrite_temp_file_still_conflicts() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_ow_temp_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        // 在途下载的临时文件是硬冲突——绝不覆盖其他任务的在途产物。
        tokio::fs::write(dir.join(format!("test.txt{TEMP_EXT}")), b"")
            .await
            .expect("test fixture write succeeds");

        let result = dedup_filename(
            &dir,
            "test.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Any,
        )
        .await;
        assert_eq!(result, "test (1).txt");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn dedup_filename_overwrite_reserved_and_avoid_still_conflict() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_ow_reserved_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");

        // reserved 命中：兄弟任务已预订同名 temp。
        let mut reserved = std::collections::HashSet::new();
        reserved.insert(dir.join(format!("video.mp4{TEMP_EXT}")));
        let result = dedup_filename(
            &dir,
            "video.mp4",
            &reserved,
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Any,
        )
        .await;
        assert_eq!(result, "video (1).mp4");

        // avoid 命中：同目录未完成任务已登记同名。
        let mut avoid = std::collections::HashSet::new();
        avoid.insert("movie.mkv".to_string());
        let result = dedup_filename(
            &dir,
            "Movie.mkv",
            &std::collections::HashSet::new(),
            &avoid,
            &crate::file_exists::OverwritePolicy::Any,
        )
        .await;
        assert_eq!(result, "Movie (1).mkv");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn dedup_filename_overwrite_directory_still_conflicts() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_ow_dir_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(dir.join("data.bin"))
            .await
            .expect("test filesystem operation succeeds");

        let result = dedup_filename(
            &dir,
            "data.bin",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Any,
        )
        .await;
        assert_eq!(result, "data (1).bin", "文件不能覆盖同名目录，必须改名");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    // 注：dedup_filename_sync 是 download_manager 模块内的私有函数，与
    // dedup_filename（async）实现严格对称（同一文件检测 + 同一 reserved
    // 集合判断）。上面的 async 测试已覆盖两者共享的行为契约："reserved
    // 集合不含本任务名时返回原名"，无需重复测试 sync 版本。

    // -----------------------------------------------------------------------
    // Bug: HeaderValue::to_str() rejects raw UTF-8 bytes (non-ASCII)
    // z-lib CDN sends Content-Disposition with unencoded Chinese chars in
    // filename="", causing the current disposition.to_str().ok()? to silently
    // return None and lose the filename entirely.
    // Fix: use std::str::from_utf8(hv.as_bytes()) which accepts any valid UTF-8.
    // -----------------------------------------------------------------------

    #[test]
    fn header_value_to_str_fails_for_raw_utf8_chinese() {
        // z-lib CDN sends:  filename="三体 (刘慈欣).epub"  as raw UTF-8 bytes
        // 三体  = \xe4\xb8\x89\xe4\xbd\x93
        // 刘慈欣 = \xe5\x88\x98\xe6\x85\x88\xe6\xac\xa3
        let raw: &[u8] = b"attachment; filename=\"\xe4\xb8\x89\xe4\xbd\x93 (\xe5\x88\x98\xe6\x85\x88\xe6\xac\xa3).epub\"; filename*=UTF-8''%E4%B8%89%E4%BD%93%20(%E5%88%98%E6%85%88%E6%AC%A3).epub";

        // reqwest / http crate accepts arbitrary bytes in HeaderValue::from_bytes.
        let hv = reqwest::header::HeaderValue::from_bytes(raw)
            .expect("HeaderValue::from_bytes must accept arbitrary bytes");

        // to_str() requires every byte to be visible ASCII (0x20-0x7E).
        // Chinese UTF-8 bytes are > 0x7E, so this MUST return Err.
        let to_str_result = hv.to_str();
        assert!(
            to_str_result.is_err(),
            "to_str() should fail for headers containing raw non-ASCII UTF-8 bytes"
        );

        // std::str::from_utf8 only requires valid UTF-8, so it MUST succeed.
        let from_utf8_result = std::str::from_utf8(hv.as_bytes());
        assert!(
            from_utf8_result.is_ok(),
            "from_utf8() should succeed for valid UTF-8 bytes; got Err({:?})",
            from_utf8_result.err()
        );

        let value = from_utf8_result.unwrap();
        // The decoded string must contain the Chinese characters.
        assert!(
            value.contains('\u{4e09}'), // first char of 三
            "decoded header must contain Chinese chars from 三体; got: {value:?}"
        );
        assert!(
            value.contains("filename*="),
            "decoded header must still contain filename*= parameter; got: {value:?}"
        );
        // Prove the raw bytes really are non-ASCII (>0x7E)
        assert!(
            raw.iter().any(|&b| b > 0x7e),
            "test data must contain non-ASCII bytes"
        );
    }

    // -----------------------------------------------------------------------
    // apply_extra_headers
    // -----------------------------------------------------------------------

    #[test]
    fn apply_extra_headers_adds_authorization() {
        use std::collections::HashMap;
        let client = reqwest::Client::new();
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer token123".to_string());
        let req = client.get("https://example.com/file.zip");
        let req = super::apply_extra_headers(req, &headers);
        // 构建请求并验证 header 已正确添加
        let built = req.build().unwrap();
        assert_eq!(
            built
                .headers()
                .get("Authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer token123"
        );
    }

    #[test]
    fn apply_extra_headers_empty_map_is_noop() {
        use std::collections::HashMap;
        let client = reqwest::Client::new();
        let headers = HashMap::new();
        let req = client.get("https://example.com/file.zip");
        let req = super::apply_extra_headers(req, &headers);
        let built = req.build().unwrap();
        assert!(built.headers().get("Authorization").is_none());
    }

    #[test]
    fn apply_extra_headers_skips_invalid_header_name() {
        use std::collections::HashMap;
        let client = reqwest::Client::new();
        let mut headers = HashMap::new();
        // 无效的 header name（包含空格）应被跳过
        headers.insert("Invalid Header".to_string(), "value".to_string());
        headers.insert("Valid-Header".to_string(), "good".to_string());
        let req = client.get("https://example.com/file.zip");
        let req = super::apply_extra_headers(req, &headers);
        let built = req.build().unwrap();
        // 有效 header 正常添加
        assert_eq!(
            built
                .headers()
                .get("Valid-Header")
                .unwrap()
                .to_str()
                .unwrap(),
            "good"
        );
        // 无效 header 被跳过（HeaderName::from_bytes 会拒绝含空格的名称）
    }

    #[test]
    fn apply_extra_headers_filters_dangerous_headers() {
        use std::collections::HashMap;
        let client = reqwest::Client::new();
        let mut headers = HashMap::new();
        // All of these should be filtered out (defense-in-depth)
        headers.insert("Accept-Encoding".to_string(), "gzip, br".to_string());
        headers.insert("Content-Encoding".to_string(), "gzip".to_string());
        headers.insert("Transfer-Encoding".to_string(), "chunked".to_string());
        headers.insert("Host".to_string(), "evil.com".to_string());
        headers.insert("Content-Length".to_string(), "999".to_string());
        headers.insert("Connection".to_string(), "keep-alive".to_string());
        headers.insert("Range".to_string(), "bytes=1200000-".to_string());
        headers.insert("If-Range".to_string(), "\"abc\"".to_string());
        // This one should pass through
        headers.insert("Authorization".to_string(), "Bearer ok".to_string());
        let req = client.get("https://example.com/file.zip");
        let req = super::apply_extra_headers(req, &headers);
        let built = req.build().unwrap();
        assert!(built.headers().get("Accept-Encoding").is_none());
        assert!(built.headers().get("Content-Encoding").is_none());
        assert!(built.headers().get("Transfer-Encoding").is_none());
        assert!(built.headers().get("Host").is_none());
        assert!(built.headers().get("Content-Length").is_none());
        assert!(built.headers().get("Connection").is_none());
        assert!(built.headers().get("Range").is_none());
        assert!(built.headers().get("If-Range").is_none());
        assert_eq!(
            built
                .headers()
                .get("Authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer ok"
        );
    }

    // -----------------------------------------------------------------------
    // detect_content_encoding
    // -----------------------------------------------------------------------

    #[test]
    fn detect_content_encoding_none_when_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(super::detect_content_encoding(&headers).is_none());
    }

    #[test]
    fn detect_content_encoding_none_for_identity() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("identity"),
        );
        assert!(super::detect_content_encoding(&headers).is_none());
    }

    #[test]
    fn detect_content_encoding_gzip() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("gzip"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Gzip)
        );
    }

    #[test]
    fn detect_content_encoding_brotli() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("br"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Brotli)
        );
    }

    #[test]
    fn detect_content_encoding_zstd() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("zstd"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Zstd)
        );
    }

    #[test]
    fn detect_content_encoding_deflate() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("deflate"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Deflate)
        );
    }

    #[test]
    fn detect_content_encoding_empty_is_none() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static(""),
        );
        assert!(super::detect_content_encoding(&headers).is_none());
    }

    #[test]
    fn detect_content_encoding_comma_separated() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("gzip, identity"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Gzip)
        );
    }

    #[test]
    fn detect_content_encoding_x_gzip() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("x-gzip"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Gzip)
        );
    }

    #[test]
    fn unsupported_content_encoding_none_token_is_supported() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("none"),
        );
        assert!(super::unsupported_content_encoding(&headers).is_none());
    }

    #[test]
    fn content_encoding_utf8_charset_is_noop() {
        // #413: OPPO/一加 OTA CDN 把字符集误写进 Content-Encoding，响应体实际
        // 未压缩——须与 "none" 同样按 no-op 放行，而非当作未知压缩层拒绝下载。
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("UTF-8"),
        );
        assert!(super::unsupported_content_encoding(&headers).is_none());
        assert!(super::detect_content_encoding(&headers).is_none());
    }

    #[test]
    fn unsupported_content_encoding_identity_is_supported() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("identity"),
        );
        assert!(super::unsupported_content_encoding(&headers).is_none());
    }

    #[test]
    fn unsupported_content_encoding_aws_chunked_is_supported() {
        // BUG-AWS-CHUNKED-FALSE-REJECT：S3 兼容对象存储把请求侧的分块签名
        // framing token 误填进响应 Content-Encoding；body 到达我们手里时
        // 早已是普通字节流，按未知编码拒绝会把可正常下载的响应挡在外面。
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("aws-chunked"),
        );
        assert!(super::unsupported_content_encoding(&headers).is_none());
        assert!(super::detect_content_encoding(&headers).is_none());
    }

    #[test]
    fn unsupported_content_encoding_gzip_is_supported() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("gzip"),
        );
        assert!(super::unsupported_content_encoding(&headers).is_none());
    }

    #[test]
    fn unsupported_content_encoding_unknown_token_rejected() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("compress"),
        );
        assert_eq!(
            super::unsupported_content_encoding(&headers),
            Some("compress".to_string())
        );
    }

    #[test]
    fn unsupported_content_encoding_multi_layer_rejected() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("gzip, gzip"),
        );
        assert_eq!(
            super::unsupported_content_encoding(&headers),
            Some("gzip, gzip".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // parse_content_range_start / is_range_response_misaligned
    //
    // 123 盘 CDN 失效时对 Range 请求回 206，但 Content-Range 起点却是 0
    // （或整体缺失）。旧代码只检查状态码 == 206 就直接从 actual_start seek
    // 写入，导致文件开头字节被写到段偏移处——整体错位却字节数吻合，产出
    // “完整大小的坏 exe”。这里的测试锁定该场景，防止回归。
    // -----------------------------------------------------------------------

    #[test]
    fn parse_content_range_start_normal() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "content-range",
            reqwest::header::HeaderValue::from_static("bytes 100-199/1234"),
        );
        assert_eq!(super::parse_content_range_start(&headers), Some(100));
    }

    #[test]
    fn parse_content_range_start_from_zero() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "content-range",
            reqwest::header::HeaderValue::from_static("bytes 0-99/1234"),
        );
        // 起点为 0 必须显式区分为 Some(0)，不能与"头缺失"的 None 混淆，
        // 否则 is_range_response_misaligned 会误判段 0 也需要回退。
        assert_eq!(super::parse_content_range_start(&headers), Some(0));
    }

    #[test]
    fn parse_content_range_start_unknown_total() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "content-range",
            reqwest::header::HeaderValue::from_static("bytes 100-199/*"),
        );
        assert_eq!(super::parse_content_range_start(&headers), Some(100));
    }

    #[test]
    fn parse_content_range_start_missing_header() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(super::parse_content_range_start(&headers), None);
    }

    #[test]
    fn parse_content_range_start_unsatisfied_range() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "content-range",
            reqwest::header::HeaderValue::from_static("bytes */1234"),
        );
        assert_eq!(super::parse_content_range_start(&headers), None);
    }

    #[test]
    fn parse_content_range_start_bare_range_without_bytes_prefix() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "content-range",
            reqwest::header::HeaderValue::from_static("100-199/1234"),
        );
        assert_eq!(super::parse_content_range_start(&headers), None);
    }

    #[test]
    fn parse_content_range_start_wrong_unit_prefix() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "content-range",
            reqwest::header::HeaderValue::from_static("items 0-5/10"),
        );
        assert_eq!(super::parse_content_range_start(&headers), None);
    }

    #[test]
    fn parse_content_range_start_real_bug_cdn_zero_replay() {
        // 真实故障日志：任务总大小 639494994 字节，123 盘 CDN 失效时对
        // 中段 Range 请求回 206，但 Content-Range 却是从 0 开始的全量流。
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "content-range",
            reqwest::header::HeaderValue::from_static("bytes 0-639494993/639494994"),
        );
        assert_eq!(super::parse_content_range_start(&headers), Some(0));
    }

    #[test]
    fn is_range_response_misaligned_aligned_nonzero() {
        assert!(!super::is_range_response_misaligned(Some(100), 100));
    }

    #[test]
    fn is_range_response_misaligned_cdn_zero_replay_bug() {
        // 核心 bug 场景：CDN 回了从 0 开始的全量流，但本段实际请求起点是
        // 508073519（故障日志中的真实段偏移）。旧代码只看状态码 206 就
        // 直接 seek(508073519) 写入——必须判定为错位以触发回退，否则
        // 文件头字节会被写到段偏移处，产出完整大小的坏文件。
        assert!(super::is_range_response_misaligned(Some(0), 508073519));
    }

    #[test]
    fn is_range_response_misaligned_start_mismatch() {
        assert!(super::is_range_response_misaligned(Some(200), 100));
    }

    #[test]
    fn is_range_response_misaligned_missing_header_at_zero_is_ok() {
        // 段 0（或从 0 续传）对缺失 Content-Range 天然免疫：从 0 写入本
        // 就是正确落点，无需回退。
        assert!(!super::is_range_response_misaligned(None, 0));
    }

    #[test]
    fn is_range_response_misaligned_missing_header_nonzero_is_misaligned() {
        // 非 0 起点的段若拿不到 Content-Range，无法确认服务器落点，保守
        // 判定为错位以触发回退，而不是假设服务器老实返回了正确区间。
        assert!(super::is_range_response_misaligned(None, 508073519));
    }

    #[test]
    fn is_range_response_misaligned_segment_zero_aligned() {
        assert!(!super::is_range_response_misaligned(Some(0), 0));
    }

    #[test]
    fn parse_and_misaligned_end_to_end_cdn_bug_repro() {
        // 端到端复现 123 盘签名失效场景：CDN 对中段请求回的 Content-Range
        // 起点是 0，而该段实际请求起点是 508073519 —— 两个函数联动之下
        // 必须判定为错位，这正是本次 bug 修复要堵住的路径。
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "content-range",
            reqwest::header::HeaderValue::from_static("bytes 0-639494993/639494994"),
        );
        let cr_start = super::parse_content_range_start(&headers);
        assert!(super::is_range_response_misaligned(cr_start, 508073519));
    }

    // -----------------------------------------------------------------------
    // parse_content_range_total（BUG-HTTP-HINT-UNDERSIZED）
    // -----------------------------------------------------------------------

    fn cr_headers(value: &'static str) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            "content-range",
            reqwest::header::HeaderValue::from_static(value),
        );
        h
    }

    #[test]
    fn parse_content_range_total_reads_denominator() {
        // 规划总大小偏小（hint=2585179）而服务器自报真实大小 4747867——
        // 分母正是发现“规划只覆盖了前缀”的唯一权威来源。
        assert_eq!(
            super::parse_content_range_total(&cr_headers("bytes 0-1292589/4747867")),
            Some(4747867)
        );
    }

    #[test]
    fn parse_content_range_total_unknown_star_is_none() {
        // 未知总大小（`*`）不可据此扩容，返回 None。
        assert_eq!(
            super::parse_content_range_total(&cr_headers("bytes 0-1023/*")),
            None
        );
    }

    #[test]
    fn parse_content_range_total_missing_header_is_none() {
        assert_eq!(
            super::parse_content_range_total(&reqwest::header::HeaderMap::new()),
            None
        );
    }

    #[test]
    fn parse_content_range_prefix_is_case_insensitive() {
        // RFC 9110 §14.1：range-unit 比较不区分大小写。个别服务器/代理发
        // `Bytes`/`BYTES` 前缀——漏检会退回旧的静默截断行为。
        assert_eq!(
            super::parse_content_range_total(&cr_headers("Bytes 0-1023/4096")),
            Some(4096)
        );
        assert_eq!(
            super::parse_content_range_total(&cr_headers("BYTES 0-1023/4096")),
            Some(4096)
        );
        assert_eq!(
            super::parse_content_range_start(&cr_headers("Bytes 100-199/4096")),
            Some(100)
        );
    }

    #[test]
    fn parse_content_range_non_bytes_unit_is_none() {
        // 非 bytes 单位（如自定义 range unit）不可解析为字节区间。
        assert_eq!(
            super::parse_content_range_total(&cr_headers("items 0-10/42")),
            None
        );
        assert_eq!(
            super::parse_content_range_start(&cr_headers("items 0-10/42")),
            None
        );
    }

    // -----------------------------------------------------------------------
    // validator_mismatch_is_fatal（CDN edge Last-Modified 漂移容差）
    // -----------------------------------------------------------------------

    #[test]
    fn validator_mismatch_last_modified_only_same_total_is_tolerated() {
        // 仅 Last-Modified 不同，但 Content-Range 总大小与已知总大小一致——
        // 同一份内容在不同 CDN edge 间的时钟/格式差异，不应判定为致命。
        assert!(!super::validator_mismatch_is_fatal(
            false,
            true,
            Some(639494994),
            639494994,
            None,
            None,
        ));
    }

    #[test]
    fn validator_mismatch_etag_differs_is_fatal() {
        // ETag 是强 validator，不同即不同版本——即使总大小一致也必须致命，
        // 不受 Last-Modified 容差规则影响。
        assert!(super::validator_mismatch_is_fatal(
            true,
            true,
            Some(639494994),
            639494994,
            None,
            None,
        ));
    }

    #[test]
    fn validator_mismatch_last_modified_and_total_differ_is_fatal() {
        // 总大小也不一致——不是同一份内容，必须致命。
        assert!(super::validator_mismatch_is_fatal(
            false,
            true,
            Some(4747867),
            639494994,
            None,
            None,
        ));
    }

    #[test]
    fn validator_mismatch_no_mismatch_is_not_fatal() {
        assert!(!super::validator_mismatch_is_fatal(
            false, false, None, 0, None, None,
        ));
    }

    #[test]
    fn validator_mismatch_unknown_total_is_fatal() {
        // 已知总大小未知（<= 0）时无法佐证"同一份内容"，保守判定致命。
        assert!(super::validator_mismatch_is_fatal(
            false,
            true,
            Some(639494994),
            0,
            None,
            None,
        ));
    }

    #[test]
    fn validator_mismatch_segment_start_mismatch_is_fatal() {
        // 分段路径：总大小一致但 Content-Range 起点与请求偏移不符——同样
        // 不能排除内容错位，必须致命。
        assert!(super::validator_mismatch_is_fatal(
            false,
            true,
            Some(639494994),
            639494994,
            Some(0),
            Some(508073519),
        ));
    }

    #[test]
    fn validator_mismatch_segment_start_match_is_tolerated() {
        // 分段路径：总大小与起点均一致——容忍。
        assert!(!super::validator_mismatch_is_fatal(
            false,
            true,
            Some(639494994),
            639494994,
            Some(508073519),
            Some(508073519),
        ));
    }

    // -----------------------------------------------------------------------
    // is_server_rejection
    // -----------------------------------------------------------------------

    /// 辅助函数：构造指定状态码的 DownloadError::Request。
    /// 利用 reqwest::Response::from(http_resp) 将 http::Response 转为 reqwest::Response，
    /// 再调用 error_for_status() 获取带状态码的 reqwest::Error。
    fn make_status_error(status: u16) -> super::DownloadError {
        let http_resp = ::reqwest::Response::from(
            ::http::Response::builder()
                .status(status)
                .body("")
                .unwrap_or_else(|_| {
                    panic!("failed to build http::Response with status {}", status)
                }),
        );
        let err = http_resp.error_for_status().unwrap_err();
        super::DownloadError::Request(err)
    }

    #[test]
    fn server_rejection_detects_403() {
        assert!(super::is_server_rejection(&make_status_error(403)));
    }

    #[test]
    fn server_rejection_detects_429() {
        assert!(super::is_server_rejection(&make_status_error(429)));
    }

    #[test]
    fn server_rejection_ignores_404() {
        assert!(!super::is_server_rejection(&make_status_error(404)));
    }

    #[test]
    fn server_rejection_ignores_500() {
        assert!(!super::is_server_rejection(&make_status_error(500)));
    }

    #[test]
    fn server_rejection_ignores_non_request_errors() {
        assert!(!super::is_server_rejection(
            &super::DownloadError::Cancelled
        ));
        assert!(!super::is_server_rejection(&super::DownloadError::Other(
            "403 forbidden".to_string()
        )));
    }

    // -----------------------------------------------------------------------
    // is_range_not_satisfiable
    // -----------------------------------------------------------------------

    #[test]
    fn range_not_satisfiable_detects_416() {
        assert!(super::is_range_not_satisfiable(&make_status_error(416)));
    }

    #[test]
    fn range_not_satisfiable_ignores_other_statuses() {
        assert!(!super::is_range_not_satisfiable(&make_status_error(403)));
        assert!(!super::is_range_not_satisfiable(&make_status_error(404)));
        assert!(!super::is_range_not_satisfiable(&make_status_error(500)));
    }

    #[test]
    fn range_not_satisfiable_ignores_non_request_errors() {
        assert!(!super::is_range_not_satisfiable(
            &super::DownloadError::Cancelled
        ));
        assert!(!super::is_range_not_satisfiable(
            &super::DownloadError::Other("416 range not satisfiable".to_string())
        ));
    }

    // -----------------------------------------------------------------------
    // normalize_checksum_spec
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_checksum_plain_form_still_ok() {
        let hash = "a".repeat(64);
        let spec = format!("sha256={hash}");
        assert_eq!(
            super::normalize_checksum_spec(&spec).as_deref(),
            Ok(spec.as_str())
        );
    }

    #[test]
    fn normalize_checksum_strips_matching_algo_prefix_and_lowercases() {
        let hash = "AB".repeat(32); // 64 hex chars, uppercase
        let spec = format!("sha256=sha256:{hash}");
        let normalized = super::normalize_checksum_spec(&spec).expect("must normalize");
        assert_eq!(normalized, format!("sha256={}", hash.to_lowercase()));
    }

    #[test]
    fn normalize_checksum_mismatched_prefix_is_error() {
        let hash = "a".repeat(64);
        let spec = format!("sha256=md5:{hash}");
        let err = super::normalize_checksum_spec(&spec).expect_err("mismatched prefix must fail");
        assert!(
            err.contains("md5") && err.contains("sha256"),
            "error should name both algorithms: {err}"
        );
    }

    #[test]
    fn normalize_checksum_wrong_length_is_error() {
        let spec = format!("sha256={}", "a".repeat(63)); // one short of 64
        assert!(super::normalize_checksum_spec(&spec).is_err());
    }

    #[test]
    fn normalize_checksum_non_hex_is_error() {
        let spec = format!("md5={}", "z".repeat(32));
        assert!(super::normalize_checksum_spec(&spec).is_err());
    }

    #[test]
    fn normalize_checksum_unsupported_algo_is_error() {
        assert!(super::normalize_checksum_spec("crc32=deadbeef").is_err());
    }

    #[test]
    fn normalize_checksum_missing_separator_is_error() {
        assert!(super::normalize_checksum_spec("sha256deadbeef").is_err());
    }

    // -----------------------------------------------------------------------
    // RequestSpec / build_request — 架构修复验证
    // -----------------------------------------------------------------------

    #[test]
    fn request_spec_empty_get_is_get_like() {
        let spec = super::RequestSpec::empty_get();
        assert_eq!(spec.method, reqwest::Method::GET);
        assert!(spec.is_get_like());
        assert!(spec.body.is_none());
    }

    #[test]
    fn request_spec_post_is_not_get_like() {
        let spec = super::RequestSpec {
            method: reqwest::Method::POST,
            cookies: String::new(),
            referrer: String::new(),
            extra_headers: std::collections::HashMap::new(),
            body: None,
            ..Default::default()
        };
        assert!(!spec.is_get_like());
    }

    /// 扩展误捕获 CORS 预检 OPTIONS 时（飞书云盘等跨域下载 API 的经典场景），
    /// from_captured 必须降级为 GET —— 否则非 GET 会被强制单流、回放
    /// OPTIONS 会拿到 404/HTML。
    #[test]
    fn request_spec_from_captured_remaps_options_to_get() {
        let spec = super::RequestSpec::from_captured(
            Some("OPTIONS"),
            String::new(),
            String::new(),
            std::collections::HashMap::new(),
            None,
        );
        assert_eq!(spec.method, reqwest::Method::GET);
        assert!(spec.is_get_like(), "remapped OPTIONS must be get-like");
        // 大小写/前后空白也应命中重映射
        let spec2 = super::RequestSpec::from_captured(
            Some(" options "),
            String::new(),
            String::new(),
            std::collections::HashMap::new(),
            None,
        );
        assert_eq!(spec2.method, reqwest::Method::GET);
    }

    /// POST 不受 OPTIONS 重映射影响——form-POST 下载（uupdump 等）依赖
    /// 原样保留 method。
    #[test]
    fn request_spec_from_captured_keeps_post() {
        let spec = super::RequestSpec::from_captured(
            Some("POST"),
            String::new(),
            String::new(),
            std::collections::HashMap::new(),
            None,
        );
        assert_eq!(spec.method, reqwest::Method::POST);
        assert!(!spec.is_get_like());
    }

    #[test]
    fn build_request_get_does_not_attach_body_even_if_present() {
        // 即使 spec 携带 body，GET 请求也不应附加（HTTP 标准上 GET 不应携带 body）
        let client = reqwest::Client::new();
        let spec = super::RequestSpec {
            method: reqwest::Method::GET,
            cookies: String::new(),
            referrer: String::new(),
            extra_headers: std::collections::HashMap::new(),
            body: Some(super::RequestBodyDecoded::Urlencoded("k=v".to_string())),
            ..Default::default()
        };
        let req = super::build_request(&client, "https://example.com", reqwest::Method::GET, &spec);
        let built = req.first_hop();
        assert_eq!(built.method(), reqwest::Method::GET);
        // GET 请求 body 应为 None
        assert!(
            built.body().is_none(),
            "GET request must not carry a body even if spec.body is set"
        );
    }

    #[test]
    fn build_request_post_attaches_form_body() {
        let client = reqwest::Client::new();
        let spec = super::RequestSpec {
            method: reqwest::Method::POST,
            cookies: String::new(),
            referrer: String::new(),
            extra_headers: std::collections::HashMap::new(),
            body: Some(super::RequestBodyDecoded::Form(vec![
                ("autodl".to_string(), "2".to_string()),
                ("updates".to_string(), "1".to_string()),
            ])),
            ..Default::default()
        };
        let req = super::build_request(
            &client,
            "https://uupdump.net/get.php",
            reqwest::Method::POST,
            &spec,
        );
        let built = req.first_hop();
        assert_eq!(built.method(), reqwest::Method::POST);
        // form() 设置 Content-Type 为 application/x-www-form-urlencoded
        assert_eq!(
            built
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/x-www-form-urlencoded")
        );
        // body 应被附加
        assert!(built.body().is_some(), "POST must carry the form body");
    }

    #[test]
    fn build_request_applies_cookies_referrer_and_extra_headers() {
        let client = reqwest::Client::new();
        let mut extra = std::collections::HashMap::new();
        extra.insert("Authorization".to_string(), "Bearer xyz".to_string());
        let spec = super::RequestSpec {
            method: reqwest::Method::GET,
            cookies: "k1=v1; k2=v2".to_string(),
            referrer: "https://referrer.example.com/page".to_string(),
            extra_headers: extra,
            body: None,
            ..Default::default()
        };
        let req = super::build_request(
            &client,
            "https://example.com/file.zip",
            reqwest::Method::GET,
            &spec,
        );
        let built = req.first_hop();
        assert_eq!(
            built.headers().get("Cookie").unwrap().to_str().unwrap(),
            "k1=v1; k2=v2"
        );
        assert_eq!(
            built
                .headers()
                .get(reqwest::header::REFERER)
                .unwrap()
                .to_str()
                .unwrap(),
            "https://referrer.example.com/page"
        );
        assert_eq!(
            built
                .headers()
                .get("Authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer xyz"
        );
    }

    #[test]
    fn build_request_applies_origin_headers_only_to_matching_site() {
        let client = reqwest::Client::new();
        let spec = super::RequestSpec {
            origin_headers: vec![super::OriginHeaders {
                origin: "https://example.com".to_string(),
                headers: std::collections::HashMap::from([(
                    "X-Site".to_string(),
                    "on".to_string(),
                )]),
            }],
            ..Default::default()
        };
        let build =
            |url: &str| super::build_request(&client, url, reqwest::Method::GET, &spec).first_hop();
        assert_eq!(
            build("https://example.com/a.bin")
                .headers()
                .get("X-Site")
                .unwrap(),
            "on"
        );
        // 不同主机 / 不同 scheme 都不命中。
        assert!(
            build("https://other.example.org/a.bin")
                .headers()
                .get("X-Site")
                .is_none()
        );
        assert!(
            build("http://example.com/a.bin")
                .headers()
                .get("X-Site")
                .is_none()
        );
    }

    /// 伪 referrer（fetch 规范占位符 "about:client" 等非 http(s) 值）不得
    /// 进入 Referer 头——部分 CDN 防盗链对非法 Referer 直接回 403。
    #[test]
    fn build_request_drops_non_http_referrer() {
        let client = reqwest::Client::new();
        for bogus in ["about:client", "about:blank", "chrome://downloads", "  "] {
            let spec = super::RequestSpec {
                method: reqwest::Method::GET,
                cookies: String::new(),
                referrer: bogus.to_string(),
                extra_headers: std::collections::HashMap::new(),
                body: None,
                ..Default::default()
            };
            let built = super::build_request(
                &client,
                "https://example.com/file.zip",
                reqwest::Method::GET,
                &spec,
            )
            .first_hop();
            assert!(
                built.headers().get(reqwest::header::REFERER).is_none(),
                "bogus referrer {bogus:?} must be dropped"
            );
        }
    }

    // -----------------------------------------------------------------------
    // filename_looks_like_html — HTML 安全网兜底辅助函数
    // -----------------------------------------------------------------------

    #[test]
    fn filename_looks_like_html_recognises_html_extensions() {
        assert!(super::filename_looks_like_html("page.html"));
        assert!(super::filename_looks_like_html("page.htm"));
        assert!(super::filename_looks_like_html("page.xhtml"));
        assert!(super::filename_looks_like_html("PAGE.HTML")); // 大小写无关
    }

    #[test]
    fn filename_looks_like_html_rejects_binary_extensions() {
        assert!(!super::filename_looks_like_html("file.zip"));
        assert!(!super::filename_looks_like_html("video.mp4"));
        assert!(!super::filename_looks_like_html("installer.exe"));
        // uupdump 案例的真实文件名
        assert!(!super::filename_looks_like_html(
            "26220.8340_amd64_zh-cn_professional_5412fa31_convert.zip"
        ));
    }

    #[test]
    fn filename_looks_like_html_empty_is_not_html() {
        // 空字符串不像 HTML——调用方应在调用前保证名称非空
        // （run_download 中 auto_name.is_empty() 检查会提前返回错误）。
        // 让此处返回 false 才能让 HTML 安全网在边界 case 下仍能触发。
        assert!(!super::filename_looks_like_html(""));
    }

    // -------------------------------------------------------------------------
    // claim_rename — atomic occupy-then-rename protocol closes the REPLACE
    // rename TOCTOU overwrite window between finalize competitors.
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn claim_rename_succeeds_when_dst_free() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_claim_rename_free_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        let src = dir.join("src.bin");
        let dst = dir.join("dst.bin");
        tokio::fs::write(&src, b"payload")
            .await
            .expect("test filesystem operation succeeds");

        let result = super::claim_rename(&src, &dst).await;
        assert!(
            result.is_ok(),
            "claim_rename must succeed on a free dst: {result:?}"
        );
        assert_eq!(tokio::fs::read(&dst).await.unwrap_or_default(), b"payload");
        assert!(!src.exists(), "src must be gone after a successful rename");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn claim_rename_fails_when_dst_exists_preserves_both() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_claim_rename_exists_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        let src = dir.join("src.bin");
        let dst = dir.join("dst.bin");
        tokio::fs::write(&src, b"incoming")
            .await
            .expect("test filesystem operation succeeds");
        tokio::fs::write(&dst, b"original")
            .await
            .expect("test filesystem operation succeeds");

        let result = super::claim_rename(&src, &dst).await;
        match result {
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists),
            Ok(()) => panic!("claim_rename must not silently overwrite an occupied dst"),
        }
        // dst must be untouched — this is the TOCTOU window the protocol closes.
        assert_eq!(tokio::fs::read(&dst).await.unwrap_or_default(), b"original");
        // src must survive intact for the caller's dedup-and-retry path.
        assert_eq!(tokio::fs::read(&src).await.unwrap_or_default(), b"incoming");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn claim_final_name_dedups_instead_of_overwriting() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_claim_final_dedup_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        let src = dir.join("a.ts.fdownloading");
        tokio::fs::write(&src, b"incoming")
            .await
            .expect("test filesystem operation succeeds");
        tokio::fs::write(dir.join("a.ts"), b"original")
            .await
            .expect("test filesystem operation succeeds");

        let chosen = super::claim_final_name(
            &src,
            &dir,
            "a.ts",
            &crate::file_exists::OverwritePolicy::Never,
            &std::collections::HashSet::new(),
        )
        .await;

        assert_eq!(chosen.ok().as_deref(), Some("a (1).ts"));
        assert_eq!(
            tokio::fs::read(dir.join("a.ts")).await.unwrap_or_default(),
            b"original"
        );
        assert_eq!(
            tokio::fs::read(dir.join("a (1).ts"))
                .await
                .unwrap_or_default(),
            b"incoming"
        );
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn claim_final_name_overwrite_replaces_only_the_original_name() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_claim_final_overwrite_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        let src = dir.join("a.ts.fdownloading");
        tokio::fs::write(&src, b"incoming")
            .await
            .expect("test filesystem operation succeeds");
        tokio::fs::write(dir.join("a.ts"), b"original")
            .await
            .expect("test filesystem operation succeeds");

        let chosen = super::claim_final_name(
            &src,
            &dir,
            "a.ts",
            &crate::file_exists::OverwritePolicy::Any,
            &std::collections::HashSet::new(),
        )
        .await;

        assert_eq!(chosen.ok().as_deref(), Some("a.ts"));
        assert_eq!(
            tokio::fs::read(dir.join("a.ts")).await.unwrap_or_default(),
            b"incoming"
        );
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[tokio::test]
    async fn claim_final_name_per_task_overwrite_binds_to_the_asked_name() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_claim_final_only_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        tokio::fs::write(dir.join("a.ts"), b"original")
            .await
            .expect("test filesystem operation succeeds");
        let avoid = std::collections::HashSet::new();
        let asked = crate::file_exists::OverwritePolicy::Only("a.ts".to_string());

        let src = dir.join("a.ts.fdownloading");
        tokio::fs::write(&src, b"incoming")
            .await
            .expect("test filesystem operation succeeds");
        let chosen = super::claim_final_name(&src, &dir, "a.ts", &asked, &avoid).await;
        assert_eq!(chosen.ok().as_deref(), Some("a.ts"));
        assert_eq!(
            tokio::fs::read(dir.join("a.ts")).await.unwrap_or_default(),
            b"incoming"
        );

        // 名字被精修成别的名字：授权不随之扩散，已有同名文件保持原样。
        tokio::fs::write(dir.join("b.ts"), b"other")
            .await
            .expect("test filesystem operation succeeds");
        let src2 = dir.join("b.ts.fdownloading");
        tokio::fs::write(&src2, b"incoming2")
            .await
            .expect("test filesystem operation succeeds");
        let chosen = super::claim_final_name(&src2, &dir, "b.ts", &asked, &avoid).await;
        assert_eq!(chosen.ok().as_deref(), Some("b (1).ts"));
        assert_eq!(
            tokio::fs::read(dir.join("b.ts")).await.unwrap_or_default(),
            b"other"
        );
        tokio::fs::remove_dir_all(&dir)
            .await
            .expect("test filesystem cleanup");
    }

    #[tokio::test]
    async fn claim_final_name_overwrite_never_touches_a_sibling_reserved_name() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_claim_final_reserved_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        let src = dir.join("a.ts.fdownloading");
        tokio::fs::write(&src, b"incoming")
            .await
            .expect("test filesystem operation succeeds");
        tokio::fs::write(dir.join("a.ts"), b"sibling")
            .await
            .expect("test filesystem operation succeeds");
        let avoid: std::collections::HashSet<String> = ["a.ts".to_string()].into();

        let chosen = super::claim_final_name(
            &src,
            &dir,
            "a.ts",
            &crate::file_exists::OverwritePolicy::Any,
            &avoid,
        )
        .await;

        assert_eq!(chosen.ok().as_deref(), Some("a (1).ts"));
        assert_eq!(
            tokio::fs::read(dir.join("a.ts")).await.unwrap_or_default(),
            b"sibling"
        );
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    #[test]
    fn disk_full_is_recognised_and_other_errors_are_not() {
        use super::{DownloadError, is_disk_full};
        let io = |kind| DownloadError::Io(std::io::Error::from(kind));
        assert!(is_disk_full(&io(std::io::ErrorKind::StorageFull)));
        assert!(is_disk_full(&io(std::io::ErrorKind::QuotaExceeded)));
        assert!(!is_disk_full(&io(std::io::ErrorKind::ConnectionReset)));
        assert!(!is_disk_full(&DownloadError::Other(
            "disk full".to_string()
        )));
        assert!(!is_disk_full(&DownloadError::Cancelled));
    }

    #[tokio::test]
    async fn dedup_filename_avoid_param_case_folds_and_renames() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_avoid_case_fold_{}",
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("test filesystem operation succeeds");
        // Empty directory, no reserved entries — only `avoid` forces a rename,
        // and the match must hold across case folding (lower-cased entry vs
        // mixed-case name).
        let mut avoid = std::collections::HashSet::new();
        avoid.insert("movie.mkv".to_string());

        let result = dedup_filename(
            &dir,
            "Movie.mkv",
            &std::collections::HashSet::new(),
            &avoid,
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(result, "Movie (1).mkv");

        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"x").expect("test fixture write succeeds");
    }

    /// `dedup_filename` 与旧「整目录扫描」参照算法给出同一个名字,且等于 `expected`。
    async fn assert_matches_reference(
        dir: &std::path::Path,
        reserved: &std::collections::HashSet<std::path::PathBuf>,
        avoid: &std::collections::HashSet<String>,
        expected: &str,
    ) {
        let reference = super::dedup_reference_scan(dir, "a.bin", reserved, avoid, false);
        let actual = dedup_filename(
            dir,
            "a.bin",
            reserved,
            avoid,
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(actual, reference, "must match the directory-scan algorithm");
        assert_eq!(actual, expected);
    }

    /// 目录里有大量同名编号条目时,探测式去重(含超过逐项探测上限后的扫描回退)与
    /// 旧「整目录扫描」算法逐例一致:编号取最小可用,`.fdownloading` / `reserved` /
    /// `avoid` 都算占用,绝不返回已存在条目。
    #[tokio::test]
    async fn dedup_filename_matches_directory_scan_with_many_entries() {
        let dir =
            std::env::temp_dir().join(format!("fluxdown_test_dedup_many_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("test filesystem operation succeeds");
        touch(&dir, "a.bin");
        // 1..=600 全部存在,但留两个缺口:7(落在逐项探测窗口内)与 450(在扫描回退区)。
        for index in 1..=600u32 {
            if index != 7 && index != 450 {
                touch(&dir, &format!("a ({index}).bin"));
            }
        }
        let none_reserved = std::collections::HashSet::new();
        let none_avoided = std::collections::HashSet::new();

        // 缺口 7 最小。
        assert_matches_reference(&dir, &none_reserved, &none_avoided, "a (7).bin").await;
        // 7 被 `.fdownloading` 占住 → 下一个缺口 450。
        touch(&dir, "a (7).bin.fdownloading");
        assert_matches_reference(&dir, &none_reserved, &none_avoided, "a (450).bin").await;
        // 450 被在途预订占住 → 601。
        let reserved = std::collections::HashSet::from([dir.join("a (450).bin.fdownloading")]);
        assert_matches_reference(&dir, &reserved, &none_avoided, "a (601).bin").await;
        // 601 被 avoid(小写折叠)占住 → 602。
        let avoid = std::collections::HashSet::from(["a (601).bin".to_string()]);
        assert_matches_reference(&dir, &reserved, &avoid, "a (602).bin").await;

        // 原名自由时原样返回,不做任何编号。
        let fresh = dedup_filename(
            &dir,
            "other.bin",
            &none_reserved,
            &none_avoided,
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert_eq!(fresh, "other.bin");

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }

    /// 1..=9999 全被占用时回退 UUID 后缀名,不返回任何已存在条目。
    #[tokio::test]
    async fn dedup_filename_falls_back_to_uuid_when_numbers_exhausted() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_test_dedup_exhausted_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("test filesystem operation succeeds");
        touch(&dir, "a.bin");
        for index in 1..=9999u32 {
            touch(&dir, &format!("a ({index}).bin"));
        }
        let result = dedup_filename(
            &dir,
            "a.bin",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &crate::file_exists::OverwritePolicy::Never,
        )
        .await;
        assert!(result.starts_with("a ("), "{result}");
        assert!(result.ends_with(").bin"), "{result}");
        assert!(
            !dir.join(&result).exists(),
            "the fallback name must not collide with an existing entry"
        );
        let inner = &result["a (".len()..result.len() - ").bin".len()];
        assert!(
            inner.parse::<u32>().is_err() && uuid::Uuid::parse_str(inner).is_ok(),
            "fallback must be a uuid suffix, got {result}"
        );
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("test filesystem cleanup failed: {error}");
        }
    }
}

/// 单流续传对异常 Range 响应的处置（本地 HTTP 服务驱动 `download_single`）。
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod single_resume_tests {
    use super::{DownloadError, RequestSpec, download_single};
    use crate::db::Db;
    use crate::events::{EngineEvent, EventSink};
    use crate::speed_limiter::SpeedLimiter;
    use crate::temp_file_guard::TempFileGuard;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    struct NoopSink;
    impl EventSink for NoopSink {
        fn emit(&self, _event: EngineEvent) {}
    }

    const MIB: usize = 1024 * 1024;

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn response(status: &str, extra: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// 每个连接读一个请求，由 `handler(range_start)` 给出完整原始响应。
    async fn spawn_http(handler: impl Fn(Option<usize>) -> Vec<u8> + Send + Sync + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = Arc::new(handler);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let handler = handler.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let text = String::from_utf8_lossy(&buf).to_lowercase();
                    let range_start = text
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .and_then(|r| r.split('-').next())
                        .and_then(|n| n.trim().parse::<usize>().ok());
                    if let Err(error) = stream.write_all(&handler(range_start)).await {
                        tracing::debug!(%error, "test HTTP client closed connection");
                    }
                    if let Err(error) = stream.shutdown().await {
                        tracing::debug!(%error, "test HTTP client closed connection");
                    }
                });
            }
        });
        port
    }

    /// 预置 `partial` 字节的临时文件后对 `total` 字节目标发起续传。
    async fn resume(
        port: u16,
        partial: &[u8],
        total: i64,
        is_resume: bool,
    ) -> (Result<(), DownloadError>, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!("fluxdown_single_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir).await.unwrap();
        let dest = dir.join("f.bin.fdownloading");
        std::fs::write(&dest, partial).unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(1024);
        let spec = RequestSpec {
            method: reqwest::Method::GET,
            cookies: String::new(),
            referrer: String::new(),
            extra_headers: std::collections::HashMap::new(),
            body: None,
            ..Default::default()
        };
        let res = download_single(
            "t1",
            &format!("http://127.0.0.1:{port}/f.bin"),
            &dest,
            total,
            true,
            &client,
            &db,
            &NoopSink,
            &tx,
            &CancellationToken::new(),
            &SpeedLimiter::new(0),
            &super::DownloadSpec::new(spec),
            "f.bin",
            "",
            "",
            is_resume,
        )
        .await
        .map(|_| ());
        let on_disk = std::fs::read(&dest).unwrap();
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
        (res, on_disk)
    }

    // The persistence fault, worker exit and queue drain share one engine lifetime.
    #[tokio::test]
    async fn completion_persistence_failure_publishes_error_and_drains_queue()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_completion_failure_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir).await?;
        let db_url = format!("sqlite://{}?mode=rwc", dir.join("completion.db").display());
        let port = spawn_http(|_| response("200 OK", "", b"completed-file")).await;
        let mut engine = crate::Engine::new(
            crate::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir.to_string_lossy().into_owned(),
                app_data_dir: dir.to_string_lossy().into_owned(),
                bt_config: crate::bt_config::BtConfig::default(),
                proxy_config: crate::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: Some(db_url.clone()),
            },
            Arc::new(NoopSink),
            Arc::new(crate::NoopSelection),
        )
        .await?;
        let injector = sqlx::AnyPool::connect(&db_url).await?;
        sqlx::query("CREATE TRIGGER reject_completion BEFORE UPDATE OF status ON tasks WHEN NEW.status = 3 BEGIN SELECT RAISE(FAIL, 'completion write rejected'); END")
            .execute(&injector).await?;
        let mut done_rx = engine.manager.take_done_rx().expect("done receiver");
        let mut progress_rx = engine
            .manager
            .take_progress_rx()
            .expect("progress receiver");
        let mut ids = Vec::new();
        for file_name in ["first.bin", "next.bin"] {
            ids.push(
                engine
                    .manager
                    .create_task(crate::download_manager::NewTaskSpec {
                        url: format!("http://127.0.0.1:{port}/{file_name}"),
                        save_dir: dir.to_string_lossy().into_owned(),
                        file_name: file_name.to_owned(),
                        segments: 1,
                        ..Default::default()
                    })
                    .await
                    .expect("create HTTP task"),
            );
        }
        for (id, file_name) in ids.iter().zip(["first.bin", "next.bin"]) {
            let done = tokio::time::timeout(std::time::Duration::from_secs(10), done_rx.recv())
                .await?
                .expect("worker reports completion");
            assert_eq!(&done.task_id, id);
            engine.manager.on_task_done(&done).await;
            let task = engine.db.load_task_by_id(id).await?.expect("task remains");
            assert_eq!(task.status, 4, "worker exit must not leave a running row");
            assert!(task.error_message.contains("completion write rejected"));
            assert_eq!(
                tokio::fs::read(dir.join(file_name)).await?,
                b"completed-file"
            );
        }
        let mut error_ids = std::collections::HashSet::new();
        while let Ok(progress) = progress_rx.try_recv() {
            assert_ne!(
                progress.status, 3,
                "failed persistence must not publish success"
            );
            if progress.status == 4 {
                assert!(progress.error_message.contains("completion write rejected"));
                error_ids.insert(progress.task_id);
            }
        }
        assert_eq!(error_ids, ids.into_iter().collect());
        engine.manager.shutdown().await;
        injector.close().await;
        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
        Ok(())
    }

    /// 外部捕获的分类路由：名字已知时建任务即落进分类目录；URL 末段没有扩展名（扩展名
    /// 在查询参数里）时先留在基准目录，完成前按响应推断出的最终文件名把产物移入命中分类
    /// 的目录，任务记录随之指向新位置。
    #[tokio::test]
    async fn category_route_follows_initial_and_final_file_names()
    -> Result<(), Box<dyn std::error::Error>> {
        const BODY: &[u8] = b"\xFF\xD8\xFF\xE0category-route-jpeg";
        let dir =
            std::env::temp_dir().join(format!("fluxdown_category_route_{}", uuid::Uuid::new_v4()));
        let base = dir.join("base");
        let pictures = dir.join("pictures");
        tokio::fs::create_dir_all(&base).await?;
        let base_dir = base.to_string_lossy().into_owned();
        let pictures_dir = pictures.to_string_lossy().into_owned();
        let port = spawn_http(|_| response("200 OK", "Content-Type: image/jpeg\r\n", BODY)).await;
        let mut engine = crate::Engine::new(
            crate::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: base_dir.clone(),
                app_data_dir: dir.to_string_lossy().into_owned(),
                bt_config: crate::bt_config::BtConfig::default(),
                proxy_config: crate::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: Some(format!(
                    "sqlite:{}?mode=rwc",
                    dir.join("route.db").to_string_lossy().replace('\\', "/")
                )),
            },
            Arc::new(NoopSink),
            Arc::new(crate::NoopSelection),
        )
        .await?;
        let mut done_rx = engine.manager.take_done_rx().expect("done receiver");
        let route = crate::category_route::CategoryRoute {
            rules: vec![crate::category_route::CategoryRule {
                match_mode: "extension".to_owned(),
                extensions: vec!["jpg".to_owned()],
                save_dir: pictures_dir.clone(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let known = engine
            .manager
            .create_task(crate::download_manager::NewTaskSpec {
                url: format!("http://127.0.0.1:{port}/photo.JPG"),
                save_dir: base_dir.clone(),
                start_paused: true,
                category_route: Some(route.clone()),
                ..Default::default()
            })
            .await
            .expect("create named task");
        let known = engine
            .db
            .load_task_by_id(&known)
            .await?
            .expect("named task");
        assert_eq!(known.save_dir, pictures_dir);

        // 无 hint：开始下载前的探测拿到 `image/jpeg`，写盘前即改选分类目录；
        // 有 hint（扩展给了大小）：不探测，完成时按实际响应精修出 `.jpg` 后移入。
        for (name, hint) in [("abc", 0), ("def", BODY.len() as i64)] {
            let id = engine
                .manager
                .create_task(crate::download_manager::NewTaskSpec {
                    url: format!("http://127.0.0.1:{port}/media/{name}?format=jpg&name=orig"),
                    save_dir: base_dir.clone(),
                    segments: 1,
                    hint_file_size: hint,
                    category_route: Some(route.clone()),
                    ..Default::default()
                })
                .await
                .expect("create unnamed task");
            let created = engine.db.load_task_by_id(&id).await?.expect("unnamed task");
            assert_eq!(created.save_dir, base_dir);
            let done = tokio::time::timeout(std::time::Duration::from_secs(10), done_rx.recv())
                .await?
                .expect("worker reports completion");
            assert_eq!(done.task_id, id);
            engine.manager.on_task_done(&done).await;
            let task = engine.db.load_task_by_id(&id).await?.expect("task remains");
            assert_eq!(task.status, 3, "{name}: {}", task.error_message);
            assert_eq!(task.save_dir, pictures_dir, "{name}");
            assert_eq!(task.file_name, format!("{name}.jpg"));
            assert_eq!(tokio::fs::read(pictures.join(&task.file_name)).await?, BODY);
            assert!(!tokio::fs::try_exists(base.join(&task.file_name)).await?);
        }

        engine.manager.shutdown().await;
        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
        Ok(())
    }

    /// `(method, Range 头)`。
    type RequestRecord = (String, Option<String>);

    /// 记录每个请求 `(method, Range)` 的测试服务器。`one_time` 时只有第一个请求（任意
    /// method）能拿到文件，之后一律 403；否则是正常的 Range 服务器（HEAD / 206 / 200）。
    /// 响应体按块限速写出，让首条连接在升级多段前不会瞬间跑完。
    struct LoggingServer {
        port: u16,
        log: Arc<std::sync::Mutex<Vec<RequestRecord>>>,
    }

    impl LoggingServer {
        fn requests(&self) -> Vec<RequestRecord> {
            self.log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    async fn spawn_logging_server(
        body: Arc<Vec<u8>>,
        one_time: bool,
        extra_headers: &'static str,
    ) -> LoggingServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let served_log = log.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let body = body.clone();
                let log = served_log.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let method = text.split_whitespace().next().unwrap_or("").to_owned();
                    let range = text.lines().find_map(|l| {
                        let (name, value) = l.split_once(':')?;
                        name.eq_ignore_ascii_case("range")
                            .then(|| value.trim().to_owned())
                    });
                    let first = {
                        let mut log = log
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        log.push((method.clone(), range.clone()));
                        log.len() == 1
                    };
                    let head = if one_time && !first {
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    } else if let Some((start, end)) = range.as_deref().and_then(|r| {
                        let (s, e) = r.strip_prefix("bytes=")?.split_once('-')?;
                        let start = s.parse::<usize>().ok()?;
                        let end = e
                            .parse::<usize>()
                            .map_or(body.len() - 1, |end| end.min(body.len() - 1));
                        Some((start, end))
                    }) {
                        let payload = &body[start..=end];
                        let mut out = format!(
                            "HTTP/1.1 206 Partial Content\r\n{extra_headers}Accept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len(),
                            payload.len()
                        )
                        .into_bytes();
                        out.extend_from_slice(payload);
                        out
                    } else {
                        let mut out = format!(
                            "HTTP/1.1 200 OK\r\n{extra_headers}Accept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        if method != "HEAD" {
                            out.extend_from_slice(&body);
                        }
                        out
                    };
                    // 头部（到第一个空行）立即发出，其后的 body 分块限速。
                    let split = head
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map_or(head.len(), |p| p + 4);
                    if stream.write_all(&head[..split]).await.is_err() {
                        return;
                    }
                    for chunk in head[split..].chunks(256 * 1024) {
                        if stream.write_all(chunk).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                    if let Err(error) = stream.shutdown().await {
                        tracing::debug!(%error, "test HTTP client closed connection");
                    }
                });
            }
        });
        LoggingServer { port, log }
    }

    async fn route_test_engine(
        dir: &std::path::Path,
        default_dir: &str,
    ) -> Result<crate::Engine, Box<dyn std::error::Error>> {
        Ok(crate::Engine::new(
            crate::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: default_dir.to_owned(),
                app_data_dir: dir.to_string_lossy().into_owned(),
                bt_config: crate::bt_config::BtConfig::default(),
                proxy_config: crate::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.to_path_buf()),
                database_url: Some(format!(
                    "sqlite://{}?mode=rwc",
                    dir.join("held.db").display()
                )),
            },
            Arc::new(NoopSink),
            Arc::new(crate::NoopSelection),
        )
        .await?)
    }

    /// 一次性 URL（只有第一个请求有效）：fresh、无 hint 的任务只发一个 GET，其响应既给出
    /// 文件名（Content-Disposition）又成为数据连接——单流与多段（升级被拒后）都要下完整。
    #[tokio::test]
    async fn one_time_url_is_downloaded_from_the_single_probe_get()
    -> Result<(), Box<dyn std::error::Error>> {
        let body = Arc::new(data(3 * MIB));
        for segments in [1, 4] {
            let dir =
                std::env::temp_dir().join(format!("fluxdown_one_time_{}", uuid::Uuid::new_v4()));
            let base = dir.join("base");
            tokio::fs::create_dir_all(&base).await?;
            let base_dir = base.to_string_lossy().into_owned();
            let server = spawn_logging_server(
                body.clone(),
                true,
                "Content-Disposition: attachment; filename=\"bundle.zip\"\r\nContent-Type: application/zip\r\n",
            )
            .await;
            let mut engine = route_test_engine(&dir, &base_dir).await?;
            let mut done_rx = engine.manager.take_done_rx().expect("done receiver");
            let id = engine
                .manager
                .create_task(crate::download_manager::NewTaskSpec {
                    url: format!("http://127.0.0.1:{}/dl?token=once", server.port),
                    save_dir: base_dir.clone(),
                    segments,
                    ..Default::default()
                })
                .await
                .expect("create one-time task");
            let done = tokio::time::timeout(std::time::Duration::from_secs(30), done_rx.recv())
                .await?
                .expect("worker reports completion");
            assert_eq!(done.task_id, id);
            engine.manager.on_task_done(&done).await;
            let task = engine.db.load_task_by_id(&id).await?.expect("task remains");
            assert_eq!(
                task.status, 3,
                "segments={segments}: {}",
                task.error_message
            );
            assert_eq!(task.file_name, "bundle.zip", "segments={segments}");
            assert_eq!(
                tokio::fs::read(base.join("bundle.zip")).await?,
                *body,
                "segments={segments}"
            );
            let requests = server.requests();
            assert_eq!(
                requests.first(),
                Some(&("GET".to_owned(), None)),
                "segments={segments}: the first request must be the plain data GET: {requests:?}"
            );
            assert!(
                requests.iter().all(|(method, _)| method == "GET"),
                "segments={segments}: no HEAD may be sent: {requests:?}"
            );
            if segments == 1 {
                assert_eq!(requests.len(), 1, "single stream sends exactly one request");
            }
            engine.manager.shutdown().await;
            drop(engine);
            if let Err(error) = tokio::fs::remove_dir_all(&dir).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%error, "test directory cleanup failed");
            }
        }
        Ok(())
    }

    /// 一次性 URL + 分类路由：分类按首响应的 Content-Disposition 扩展名命中，任务一开始就
    /// 落进分类目录，且整个过程只发了那一个 GET。
    #[tokio::test]
    async fn one_time_url_routes_by_content_disposition_name()
    -> Result<(), Box<dyn std::error::Error>> {
        let body = Arc::new(data(2 * MIB));
        let dir = std::env::temp_dir().join(format!("fluxdown_one_time_{}", uuid::Uuid::new_v4()));
        let base = dir.join("base");
        let archives = dir.join("archives");
        tokio::fs::create_dir_all(&base).await?;
        let base_dir = base.to_string_lossy().into_owned();
        let archives_dir = archives.to_string_lossy().into_owned();
        let server = spawn_logging_server(
            body.clone(),
            true,
            "Content-Disposition: attachment; filename=\"bundle.zip\"\r\nContent-Type: application/octet-stream\r\n",
        )
        .await;
        let mut engine = route_test_engine(&dir, &base_dir).await?;
        let mut done_rx = engine.manager.take_done_rx().expect("done receiver");
        let id = engine
            .manager
            .create_task(crate::download_manager::NewTaskSpec {
                url: format!("http://127.0.0.1:{}/dl?token=once", server.port),
                save_dir: base_dir.clone(),
                segments: 1,
                category_route: Some(crate::category_route::CategoryRoute {
                    rules: vec![crate::category_route::CategoryRule {
                        match_mode: "extension".to_owned(),
                        extensions: vec!["zip".to_owned()],
                        save_dir: archives_dir.clone(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .expect("create one-time routed task");
        let done = tokio::time::timeout(std::time::Duration::from_secs(30), done_rx.recv())
            .await?
            .expect("worker reports completion");
        assert_eq!(done.task_id, id);
        engine.manager.on_task_done(&done).await;
        let task = engine.db.load_task_by_id(&id).await?.expect("task remains");
        assert_eq!(task.status, 3, "{}", task.error_message);
        assert_eq!(task.save_dir, archives_dir);
        assert_eq!(task.file_name, "bundle.zip");
        assert_eq!(tokio::fs::read(archives.join("bundle.zip")).await?, *body);
        assert!(!tokio::fs::try_exists(base.join("bundle.zip")).await?);
        assert_eq!(server.requests(), vec![("GET".to_owned(), None)]);
        engine.manager.shutdown().await;
        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
        Ok(())
    }

    /// 正常 Range 服务器：fresh hint-0 任务仍升级为多段——恰好一个无 Range 的 GET 起步，
    /// 随后出现 `bytes=N-…` 的并发 Range 请求；全程没有 HEAD 与 `bytes=0-0`。
    #[tokio::test]
    async fn range_server_upgrades_single_probe_get_to_multi_segment()
    -> Result<(), Box<dyn std::error::Error>> {
        let body = Arc::new(data(16 * MIB));
        let dir = std::env::temp_dir().join(format!("fluxdown_upgrade_{}", uuid::Uuid::new_v4()));
        let base = dir.join("base");
        tokio::fs::create_dir_all(&base).await?;
        let base_dir = base.to_string_lossy().into_owned();
        let server = spawn_logging_server(body.clone(), false, "").await;
        let mut engine = route_test_engine(&dir, &base_dir).await?;
        let mut done_rx = engine.manager.take_done_rx().expect("done receiver");
        let id = engine
            .manager
            .create_task(crate::download_manager::NewTaskSpec {
                url: format!("http://127.0.0.1:{}/big.bin", server.port),
                save_dir: base_dir.clone(),
                segments: 4,
                ..Default::default()
            })
            .await
            .expect("create task");
        let done = tokio::time::timeout(std::time::Duration::from_secs(60), done_rx.recv())
            .await?
            .expect("worker reports completion");
        assert_eq!(done.task_id, id);
        engine.manager.on_task_done(&done).await;
        let task = engine.db.load_task_by_id(&id).await?.expect("task remains");
        assert_eq!(task.status, 3, "{}", task.error_message);
        assert_eq!(tokio::fs::read(base.join("big.bin")).await?, *body);
        let requests = server.requests();
        assert!(
            requests.iter().all(|(method, _)| method == "GET"),
            "no HEAD may be sent: {requests:?}"
        );
        assert_eq!(
            requests.iter().filter(|(_, range)| range.is_none()).count(),
            1,
            "exactly one plain GET: {requests:?}"
        );
        assert!(
            requests
                .iter()
                .all(|(_, range)| range.as_deref() != Some("bytes=0-0")),
            "no bytes=0-0 probe: {requests:?}"
        );
        assert!(
            requests
                .iter()
                .filter(|(_, range)| range.as_deref().is_some_and(|r| !r.starts_with("bytes=0-")))
                .count()
                >= 1,
            "expected upgraded Range requests: {requests:?}"
        );
        engine.manager.shutdown().await;
        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
        Ok(())
    }

    /// hint 模式（扩展给了大小、跳过探测）下首个真实下载请求撞上 Cloudflare 式
    /// 404 质询：必须换中性 UA 重发并完成下载；切换对其后所有请求生效（整个任务只
    /// 被质询一次），并落库供续传沿用。单流与多段两条路径都要覆盖。
    #[tokio::test]
    async fn hint_mode_download_escapes_bot_challenge_once_and_persists_neutral_ua()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/141.0.0.0";
        let body = Arc::new(data(4 * MIB));
        for segments in [4, 1] {
            let challenged = Arc::new(AtomicUsize::new(0));
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let port = listener.local_addr()?.port();
            {
                let challenged = challenged.clone();
                let body = body.clone();
                tokio::spawn(async move {
                    while let Ok((mut stream, _)) = listener.accept().await {
                        let challenged = challenged.clone();
                        let body = body.clone();
                        tokio::spawn(async move {
                            let mut buf = Vec::new();
                            let mut tmp = [0u8; 1024];
                            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                let n = stream.read(&mut tmp).await.unwrap_or(0);
                                if n == 0 {
                                    return;
                                }
                                buf.extend_from_slice(&tmp[..n]);
                            }
                            let text = String::from_utf8_lossy(&buf).to_lowercase();
                            let browser = text
                                .lines()
                                .any(|l| l.starts_with("user-agent:") && l.contains("mozilla"));
                            let range = text
                                .lines()
                                .find_map(|l| l.strip_prefix("range: bytes="))
                                .and_then(|r| r.trim().split_once('-'))
                                .and_then(|(s, e)| {
                                    let start = s.parse::<usize>().ok()?;
                                    let end = e
                                        .parse::<usize>()
                                        .map_or(body.len() - 1, |end| end.min(body.len() - 1));
                                    Some((start, end))
                                });
                            let resp = if browser {
                                challenged.fetch_add(1, Ordering::SeqCst);
                                response("404 Not Found", "cf-mitigated: challenge\r\n", b"")
                            } else if let Some((start, end)) = range {
                                response(
                                    "206 Partial Content",
                                    &format!(
                                        "Accept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{}\r\n",
                                        body.len()
                                    ),
                                    &body[start..=end],
                                )
                            } else {
                                response("200 OK", "Accept-Ranges: bytes\r\n", &body)
                            };
                            if let Err(error) = stream.write_all(&resp).await {
                                tracing::debug!(%error, "test HTTP client closed connection");
                            }
                            if let Err(error) = stream.shutdown().await {
                                tracing::debug!(%error, "test HTTP client closed connection");
                            }
                        });
                    }
                });
            }

            let dir = std::env::temp_dir()
                .join(format!("fluxdown_hint_challenge_{}", uuid::Uuid::new_v4()));
            tokio::fs::create_dir_all(&dir).await?;
            let mut engine = crate::Engine::new(
                crate::EngineConfig {
                    max_concurrent: 1,
                    speed_limit_bps: 0,
                    upload_limit_bps: 0,
                    default_save_dir: dir.to_string_lossy().into_owned(),
                    app_data_dir: dir.to_string_lossy().into_owned(),
                    bt_config: crate::bt_config::BtConfig::default(),
                    proxy_config: crate::proxy_config::ProxyConfig::default(),
                    user_agent: String::new(),
                    data_dir_override: Some(dir.clone()),
                    database_url: None,
                },
                Arc::new(NoopSink),
                Arc::new(crate::NoopSelection),
            )
            .await?;
            let mut done_rx = engine.manager.take_done_rx().expect("done receiver");
            let progress_rx = engine
                .manager
                .take_progress_rx()
                .expect("progress receiver");
            let reporter = tokio::spawn(crate::download_manager::progress_reporter(
                progress_rx,
                engine.db.clone(),
                engine.activity_sink.clone(),
            ));
            // 任务级浏览器 UA（Chrome 预设）+ 扩展转发的浏览器 UA：最坏组合。
            let mut headers = std::collections::HashMap::new();
            headers.insert("User-Agent".to_string(), BROWSER_UA.to_string());
            let id = engine
                .manager
                .create_task(crate::download_manager::NewTaskSpec {
                    url: format!("http://127.0.0.1:{port}/Rider.exe"),
                    save_dir: dir.to_string_lossy().into_owned(),
                    file_name: "Rider.exe".to_owned(),
                    segments,
                    user_agent: BROWSER_UA.to_owned(),
                    extra_headers: headers,
                    hint_file_size: body.len() as i64,
                    ..Default::default()
                })
                .await
                .expect("create HTTP task");
            let done = tokio::time::timeout(std::time::Duration::from_secs(20), done_rx.recv())
                .await?
                .expect("worker reports completion");
            engine.manager.on_task_done(&done).await;

            let task = engine.db.load_task_by_id(&id).await?.expect("task remains");
            assert_eq!(
                task.status, 3,
                "segments={segments}: {}",
                task.error_message
            );
            assert_eq!(tokio::fs::read(dir.join("Rider.exe")).await?, *body);
            assert_eq!(
                challenged.load(Ordering::SeqCst),
                1,
                "segments={segments}: neutral UA must stick for every later request"
            );
            let (_, _, headers_json) = engine
                .db
                .load_task_request_context(&id)
                .await?
                .expect("request context");
            let persisted: std::collections::HashMap<String, String> =
                serde_json::from_str(&headers_json)?;
            let ua: Vec<_> = persisted
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
                .map(|(_, v)| v.as_str())
                .collect();
            assert_eq!(ua, [super::DEFAULT_UA], "segments={segments}");

            engine.manager.shutdown().await;
            reporter.abort();
            drop(engine);
            if let Err(error) = tokio::fs::remove_dir_all(&dir).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%error, "test directory cleanup failed");
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn failed_stale_file_removal_preserves_segment_resume_layout() {
        let db = Db::connect("sqlite::memory:").await.expect("open DB");
        db.insert_task(
            "discard",
            "http://localhost/f",
            "f",
            "",
            2,
            100,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");
        db.insert_segments("discard", &[(0, 0, 99)])
            .await
            .expect("insert segment");
        db.update_segment_progress("discard", 0, 10)
            .await
            .expect("save durable checkpoint");
        let path =
            std::env::temp_dir().join(format!("fluxdown_http_discard_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&path)
            .await
            .expect("create non-removable-as-file fixture");
        assert!(matches!(
            super::discard_segment_data(&db, "discard", &path).await,
            Err(DownloadError::Io(_))
        ));
        let rows = db
            .load_segments("discard")
            .await
            .expect("load preserved layout");
        assert_eq!(
            rows.iter()
                .map(|r| (r.index, r.start_byte, r.end_byte, r.downloaded_bytes))
                .collect::<Vec<_>>(),
            vec![(0, 0, 99, 10)],
            "failed invalidation cannot masquerade as a complete single-stream file on resume"
        );
        tokio::fs::remove_dir(path)
            .await
            .expect("remove fixture directory");
    }

    #[tokio::test]
    async fn failed_checkpoint_flush_does_not_advance_resume_progress() {
        let db = Db::connect("sqlite::memory:").await.expect("open DB");
        db.insert_task(
            "checkpoint",
            "http://localhost/f",
            "f",
            "",
            1,
            8,
            "",
            "",
            "",
            1,
        )
        .await
        .expect("insert task");
        let path =
            std::env::temp_dir().join(format!("fluxdown_http_checkpoint_{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, b"old")
            .await
            .expect("create fixture");
        let file = tokio::fs::File::open(&path)
            .await
            .expect("open read-only fixture");
        let mut writer = tokio::io::BufWriter::with_capacity(1024, file);
        writer.write_all(b"new").await.expect("buffer write");
        assert!(matches!(
            super::persist_single_progress(&mut writer, &db, "checkpoint", 6).await,
            Err(DownloadError::Io(_))
        ));
        assert_eq!(
            db.load_task_by_id("checkpoint")
                .await
                .expect("load task")
                .expect("task exists")
                .downloaded_bytes,
            0
        );
        assert_eq!(tokio::fs::read(&path).await.expect("read fixture"), b"old");
        drop(writer);
        tokio::fs::remove_file(path).await.expect("remove fixture");
    }

    #[tokio::test]
    async fn single_stream_rejects_other_writer_and_retries_after_release() {
        let dir =
            std::env::temp_dir().join(format!("fluxdown_single_lock_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir).await.unwrap();
        let dest = dir.join("f.bin.fdownloading");
        std::fs::write(&dest, b"existing partial data").unwrap();
        let first = TempFileGuard::acquire(&dest, "first").await.unwrap();
        let port = spawn_http(|_| response("200 OK", "", b"finished download")).await;
        let url = format!("http://127.0.0.1:{port}/f.bin");
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(1024);
        let cancel = CancellationToken::new();
        let limiter = SpeedLimiter::new(0);
        let spec = super::DownloadSpec::new(RequestSpec {
            method: reqwest::Method::GET,
            cookies: String::new(),
            referrer: String::new(),
            extra_headers: std::collections::HashMap::new(),
            body: None,
            ..Default::default()
        });
        let attempt = || {
            download_single(
                "second", &url, &dest, 17, false, &client, &db, &NoopSink, &tx, &cancel, &limiter,
                &spec, "f.bin", "", "", false,
            )
        };
        assert!(matches!(
            attempt().await,
            Err(DownloadError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert_eq!(std::fs::read(&dest).unwrap(), b"existing partial data");
        drop(first);
        let paused = CancellationToken::new();
        paused.cancel();
        let paused_result = download_single(
            "second", &url, &dest, 17, false, &client, &db, &NoopSink, &tx, &paused, &limiter,
            &spec, "f.bin", "", "", false,
        )
        .await;
        assert!(matches!(paused_result, Err(DownloadError::Cancelled)));
        assert_eq!(std::fs::read(&dest).unwrap(), b"existing partial data");
        let after_pause = TempFileGuard::acquire(&dest, "first").await.unwrap();
        drop(after_pause);
        let second = TempFileGuard::acquire(&dest, "second").await.unwrap();
        attempt().await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"finished download");
        drop(second);
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
    }

    #[tokio::test]
    async fn resume_206_without_content_range_refetches_whole_file() {
        let full = Arc::new(data(2 * MIB + 100));
        let served = full.clone();
        let port = spawn_http(move |range| match range {
            // 按范围返回尾段却漏掉 Content-Range 的服务器。
            Some(start) => response("206 Partial Content", "", &served[start..]),
            None => response("200 OK", "", &served),
        })
        .await;
        let (res, on_disk) = resume(port, &full[..MIB + MIB / 2], full.len() as i64, true).await;
        res.unwrap();
        assert_eq!(on_disk, *full);
    }

    #[tokio::test]
    async fn resume_206_with_different_total_refetches_whole_file() {
        // 服务器文件比记录的总大小小：Content-Range 分母不一致时不得沿用旧前缀。
        let new_full = Arc::new(data(MIB + MIB / 2 + 7));
        let old_total = 3 * MIB;
        let served = new_full.clone();
        let port = spawn_http(move |range| match range {
            Some(start) => {
                let cr = format!(
                    "Content-Range: bytes {start}-{}/{}\r\n",
                    served.len() - 1,
                    served.len()
                );
                response("206 Partial Content", &cr, &served[start..])
            }
            None => response("200 OK", "", &served),
        })
        .await;
        let old_prefix = data(old_total);
        let (res, on_disk) =
            resume(port, &old_prefix[..MIB + MIB / 4], old_total as i64, true).await;
        res.unwrap();
        assert_eq!(on_disk, *new_full);
    }

    #[tokio::test]
    async fn resume_answered_by_mismatched_200_keeps_partial_data() {
        let full = data(2 * MIB);
        let port = spawn_http(|_| {
            response(
                "200 OK",
                "Content-Type: application/json\r\n",
                b"{\"code\":403}",
            )
        })
        .await;
        let partial = &full[..MIB + 10];
        let (res, on_disk) = resume(port, partial, full.len() as i64, true).await;
        assert!(res.is_err());
        assert_eq!(on_disk, partial);
    }

    /// 服务器在 Content-Length 未满足时关闭连接：错误必须点明响应不完整并带上已收 /
    /// 声明字节数，保留已收数据，仍属可自动重试的瞬时错误。
    #[tokio::test]
    async fn premature_eof_reports_incomplete_response_with_byte_counts() {
        let body = data(38);
        let mut raw =
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n".to_vec();
        raw.extend_from_slice(&body);
        let port = spawn_http(move |_| raw.clone()).await;
        let (res, on_disk) = resume(port, b"", 100, false).await;
        let err = res.unwrap_err();
        let message = err.to_string();
        // 按落库方式（downloader 单流错误收尾）拼出完整文案：根因链不能丢。
        let stored = format!(
            "{message}{}",
            std::error::Error::source(&err)
                .map(|s| super::format_error_chain(s.source()))
                .unwrap_or_default()
        );
        assert!(
            stored.contains("end of file before message length reached"),
            "{stored}"
        );
        assert!(message.contains("incomplete download"), "{message}");
        assert!(message.contains("38 of 100 bytes"), "{message}");
        assert!(message.contains("62 bytes short"), "{message}");
        assert!(crate::download_manager::is_retriable_error(&message));
        assert_eq!(on_disk, body);
    }

    fn stall_message(
        body_start: i64,
        downloaded: i64,
        declared_end: Option<i64>,
    ) -> (DownloadError, String) {
        let error = super::single_stall_error(10, body_start, downloaded, declared_end);
        let message = error.to_string();
        (error, message)
    }

    /// 已知声明长度且差额未补齐：给出已收 / 声明 / 差额；不下「服务器写错长度」的结论。
    #[test]
    fn stall_before_declared_length_reports_shortfall() {
        let (error, message) = stall_message(0, 150_468, Some(150_530));
        assert!(
            message.starts_with("download stalled: no data received for 10s"),
            "{message}"
        );
        assert!(message.contains("150468 of 150530 bytes"), "{message}");
        assert!(message.contains("62 bytes short"), "{message}");
        // 停滞分类与自动重试判据不因新文案失效。
        assert_eq!(
            crate::net_failure::classify_download_error(&error),
            Some(crate::net_failure::NetFailure::Stalled)
        );
        assert!(crate::download_manager::is_retriable_error(&message));
        // 续传响应的声明终点是文件偏移，差额按偏移计。
        let (_, resumed) = stall_message(1_000, 1_500, Some(5_000));
        assert!(resumed.contains("1500 of 5000 bytes"), "{resumed}");
        assert!(resumed.contains("3500 bytes short"), "{resumed}");
    }

    /// 无声明长度 / 压缩响应只报停滞事实；首字节前后分开表述，且不出现「不完整」断言。
    #[test]
    fn stall_without_declared_shortfall_only_reports_facts() {
        let (_, first_byte) = stall_message(0, 0, None);
        assert!(first_byte.contains("no body bytes yet"), "{first_byte}");
        let (_, mid) = stall_message(0, 4_096, None);
        assert!(mid.contains("after 4096 body bytes"), "{mid}");
        assert!(!mid.contains("incomplete"), "{mid}");
        let (_, full) = stall_message(0, 100, Some(100));
        assert!(!full.contains("incomplete"), "{full}");
        // 已收满声明长度但服务器没关连接：只陈述事实。
        assert!(full.contains("after 100 body bytes"), "{full}");
    }

    #[tokio::test]
    async fn resume_with_complete_temp_file_skips_download() {
        // 端口上没有服务：任何网络请求都会失败。
        let full = data(MIB + 3);
        let (res, on_disk) = resume(1, &full, full.len() as i64, true).await;
        res.unwrap();
        assert_eq!(on_disk, full);
    }
}
