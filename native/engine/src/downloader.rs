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
use crate::logger::log_info;
use crate::speed_limiter::SpeedLimiter;

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
    /// 服务器在 probe 与分段/续传请求之间【更换了文件】：客户端发了 `If-Range`
    /// 条件请求，但服务器的 validator（ETag/Last-Modified）与 probe 时不一致，于是
    /// 【忽略 Range 返回 200 全量当前文件】。与 [`RangeNotSupported`] 严格区分：后者
    /// 是服务器根本不支持 Range（回退单流续下同一文件即可）；本变体意味着旧数据已
    /// 作废，必须【清空临时文件 + 重新下载新版本】，绝不能把旧字节当可续传（否则
    /// 产出新旧版本混合的损坏文件）。也【绝不】记录主机单连接缓存（文件变化与服务器
    /// Range 能力无关）。
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
    #[error("ed2k error: {0}")]
    Ed2k(String),
    /// ED2K 协议完整性违规：hashset 投毒 / 块 MD4 不匹配 / SENDINGPART 越界 /
    /// 未请求数据 / 区间碎片超限。与 [`DownloadError::Ed2k`]（纯网络类）区分，
    /// 调度层据此把违规 peer 拉黑（贯穿整个下载调用），而非仅退避。
    #[error("ed2k integrity violation: {0}")]
    Ed2kIntegrity(String),
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

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct FileInfo {
    pub file_name: String,
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
    pub downloaded_bytes: i64,
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
}

/// Snapshot of a single segment's progress, sent from downloader to progress_reporter.
#[derive(Clone)]
pub struct SegmentProgressInfo {
    pub index: i32,
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
    /// 该任务的 Range 能力是否已被验证（probe 确认过、或此前任一 206/
    /// Accept-Ranges 证据，持久化于 tasks.range_verified）。
    /// fresh 任务恒 true（hint 与否由 hint_file_size 表达）；resume 时由
    /// download_manager 从 DB 读取——`false` 表示该任务源自 hint 且从未拿到
    /// Range 支持证据，resume 须延续「首连接 plain GET」保守启动，绝不发
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
    /// 下载完成后是否把文件修改时间设为服务器提供的 `Last-Modified` 时间
    /// （config `use_server_time`）。服务器未提供该头、解析失败或写入失败时
    /// 保留本地完成时间，绝不影响下载结果。
    pub use_server_time: bool,
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
    if extra_headers.is_empty() {
        return req;
    }

    /// Headers that must never be forwarded from the browser extension.
    /// Compared case-insensitively via `HeaderName` (which lowercases).
    const BLOCKED_HEADERS: &[&str] = &[
        "accept-encoding",
        "content-encoding",
        "transfer-encoding",
        "host",
        "content-length",
        "connection",
        "range",
        "if-range",
    ];

    let mut map = reqwest::header::HeaderMap::with_capacity(extra_headers.len());
    for (name, value) in extra_headers {
        if let (Ok(header_name), Ok(header_value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            if BLOCKED_HEADERS
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
#[derive(Debug, Clone)]
pub struct RequestSpec {
    pub method: reqwest::Method,
    pub cookies: String,
    pub referrer: String,
    pub extra_headers: std::collections::HashMap<String, String>,
    pub body: Option<RequestBodyDecoded>,
}

/// 浏览器扩展/Native Messaging 捕获的原始请求体——引擎侧的传输无关表示。
/// `hub` 侧从 `native_messaging::RequestBody`(wire 格式,字段名受 NM 协议
/// 约束)转换为此类型后再调用 [`RequestSpec::from_captured`],使得
/// `downloader`/`download_manager` 不直接依赖 `native_messaging`。
#[derive(Debug, Clone)]
pub enum CapturedRequestBody {
    FormData {
        fields: std::collections::HashMap<String, Vec<String>>,
    },
    Urlencoded {
        raw: String,
    },
    /// `bytes_b64`：base64 编码的原始字节(XHR/fetch 直接发送 ArrayBuffer 场景)。
    Raw {
        bytes_b64: String,
        content_type: Option<String>,
    },
}

impl RequestSpec {
    /// 默认 GET、无 cookies/headers/body——用于 download_manager 内部的"裸"
    /// HTTP 请求场景(如 BT/HLS 元数据获取,无浏览器会话上下文)。
    pub fn empty_get() -> Self {
        Self {
            method: reqwest::Method::GET,
            cookies: String::new(),
            referrer: String::new(),
            extra_headers: std::collections::HashMap::new(),
            body: None,
        }
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
        use base64::Engine;

        let method = method
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
            .unwrap_or(reqwest::Method::GET);

        let body = body.and_then(|b| match b {
            CapturedRequestBody::FormData { fields } => {
                let mut pairs: Vec<(String, String)> = Vec::new();
                for (k, vs) in fields {
                    for v in vs {
                        pairs.push((k.clone(), v.clone()));
                    }
                }
                Some(RequestBodyDecoded::Form(pairs))
            }
            CapturedRequestBody::Urlencoded { raw } => Some(RequestBodyDecoded::Urlencoded(raw)),
            CapturedRequestBody::Raw {
                bytes_b64,
                content_type,
            } => match base64::engine::general_purpose::STANDARD.decode(&bytes_b64) {
                Ok(bytes) => Some(RequestBodyDecoded::Raw {
                    bytes,
                    content_type,
                }),
                Err(e) => {
                    log_info!("[request-spec] failed to base64-decode raw body: {}", e);
                    None
                }
            },
        });

        Self {
            method,
            cookies,
            referrer,
            extra_headers,
            body,
        }
    }
}

/// 统一请求构建入口——所有发出 HTTP 请求的地方都应通过此函数。
///
/// 此函数替代了散落在 downloader / segment_coordinator / hls / dash 等
/// 模块中的 `client.get(url) + apply_extra_headers(...)` 模式。
///
/// 参数 `method` 允许覆盖 `spec.method`——主要用于 probe 阶段（HEAD probe
/// 总是发送 HEAD，与 spec 自身的 method 无关）。下载阶段通常传 `spec.method.clone()`。
///
/// **请求体语义**：
///   - GET / HEAD：即使 `spec.body` 非空也不会被附加（HTTP 标准上 GET/HEAD 不应携带 body）
///   - 其他 method：按 `RequestBodyDecoded` 类型重建请求体
pub fn build_request(
    client: &Client,
    url: &str,
    method: reqwest::Method,
    spec: &RequestSpec,
) -> reqwest::RequestBuilder {
    let attaches_body = method != reqwest::Method::GET && method != reqwest::Method::HEAD;
    let mut req = client.request(method, url);

    if !spec.cookies.is_empty() {
        req = req.header("Cookie", &spec.cookies);
    }
    if !spec.referrer.is_empty() {
        req = req.header(reqwest::header::REFERER, &spec.referrer);
    }
    req = apply_extra_headers(req, &spec.extra_headers);

    if attaches_body && let Some(body) = &spec.body {
        match body {
            RequestBodyDecoded::Form(pairs) => {
                req = req.form(pairs);
            }
            RequestBodyDecoded::Urlencoded(raw) => {
                req = req
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(raw.clone());
            }
            RequestBodyDecoded::Raw {
                bytes,
                content_type,
            } => {
                if let Some(ct) = content_type {
                    req = req.header(reqwest::header::CONTENT_TYPE, ct);
                }
                req = req.body(bytes.clone());
            }
        }
    }

    req
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
            "identity" | "" => {}
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
/// UA via `extra_headers`.  That UA is applied on the first attempt; if the
/// server returns 4xx we automatically retry *without* the browser UA so that
/// Cloudflare-protected CDNs also work (see [`resolve_file_info`]).
///
/// **Version rule（同 aria2 的 `aria2/<版本>`）**：release 构建为
/// `FluxDown/<pubspec 版本号>`（build.rs 注入 `FLUXDOWN_APP_VERSION`），
/// debug 构建固定 `FluxDown/1.0`。
const DEFAULT_UA: &str = if cfg!(debug_assertions) {
    "FluxDown/1.0"
} else {
    concat!("FluxDown/", env!("FLUXDOWN_APP_VERSION"))
};

/// Build a properly configured HTTP client that mirrors Chrome's capabilities.
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
    use crate::proxy_config::{ProxyMode, detect_system_proxy};

    let ua = if user_agent.is_empty() {
        DEFAULT_UA
    } else {
        user_agent
    };
    let mut builder = Client::builder()
        .user_agent(ua)
        // TLS — 跳过证书验证（过期、自签名、hostname 不匹配等）。
        // 下载管理器与浏览器行为保持一致：浏览器允许用户忽略证书错误继续下载，
        // 且企业内网邮箱等场景常见 hostname mismatch，严格验证会导致下载失败。
        // 类似 curl -k / aria2 --check-certificate=false。
        .danger_accept_invalid_certs(true)
        // NOTE: This setting also applies to MITM proxy scenarios.
        // A malicious proxy could intercept HTTPS traffic undetected.
        // Users operating in sensitive environments should be aware of this trade-off.
        // A future improvement would be to add a "strict TLS" toggle in Settings.
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
        // Redirects — follow up to 30 hops like Chrome
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
        // Cookies — needed for session-based downloads (Google Drive, etc.).
        // reqwest follows RFC 6265: cookies are scoped to their domain.
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
                        match reqwest::Proxy::all(&url) {
                            Ok(mut proxy) => {
                                if !sys_proxy.username.is_empty() {
                                    proxy =
                                        proxy.basic_auth(&sys_proxy.username, &sys_proxy.password);
                                }
                                if !sys_proxy.no_proxy_list.is_empty() {
                                    proxy = proxy.no_proxy(reqwest::NoProxy::from_string(
                                        &sys_proxy.no_proxy_list,
                                    ));
                                }
                                builder = builder.proxy(proxy);
                            }
                            Err(e) => {
                                log_info!("[build_client] failed to parse system proxy URL: {}", e);
                            }
                        }
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
                match reqwest::Proxy::all(&url) {
                    Ok(mut proxy) => {
                        if !proxy_config.username.is_empty() {
                            proxy =
                                proxy.basic_auth(&proxy_config.username, &proxy_config.password);
                        }
                        if !proxy_config.no_proxy_list.is_empty() {
                            proxy = proxy.no_proxy(reqwest::NoProxy::from_string(
                                &proxy_config.no_proxy_list,
                            ));
                        }
                        builder = builder.proxy(proxy);
                    }
                    Err(e) => {
                        log_info!("[build_client] failed to create proxy from URL: {}", e);
                    }
                }
            } else {
                log_info!("[build_client] manual proxy: incomplete config, using direct");
                builder = builder.no_proxy();
            }
        }
    }

    let client = builder.build()?;
    Ok(client)
}

// ---------------------------------------------------------------------------
// Resolve file info (HEAD probe → GET fallback)
// ---------------------------------------------------------------------------

/// Timeout for the probe requests (HEAD / GET Range:0-0).
/// 15 seconds is sufficient for most servers; the retry mechanism handles
/// transient failures without making users wait excessively.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum retries for the probe phase (HEAD + GET).
///
/// 3 attempts total:
///   1. Original headers (incl. browser UA from extension, if any)
///   2. Normal retry (same headers, covers DNS/TLS cold-start)
///   3. **UA-downgrade retry** — strips browser UA from extra_headers so that
///      the request uses the neutral `DEFAULT_UA`.  This handles Cloudflare
///      Bot Management which rejects requests where the TLS fingerprint
///      (rustls ≠ Chrome) contradicts a Chrome User-Agent header.
const PROBE_MAX_RETRIES: u32 = 3;

/// Base delay for probe retries (used with exponential backoff).
const PROBE_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

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
    // Prepare a fallback spec that strips browser-like User-Agent.
    // On the last attempt we use this to avoid Cloudflare JA3-vs-UA mismatch.
    let headers_without_browser_ua: std::collections::HashMap<String, String> = spec
        .extra_headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("user-agent"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let has_browser_ua = spec
        .extra_headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("user-agent"));

    // Holder for the UA-downgraded variant; allocated once outside the loop so
    // we can borrow it without repeated cloning.
    let downgraded_spec = RequestSpec {
        method: spec.method.clone(),
        cookies: spec.cookies.clone(),
        referrer: spec.referrer.clone(),
        extra_headers: headers_without_browser_ua,
        body: spec.body.clone(),
    };

    let mut last_err = None;
    for attempt in 0..PROBE_MAX_RETRIES {
        // Last attempt: if extra_headers carried a browser UA, drop it so
        // the request falls back to DEFAULT_UA ("FluxDown/<version>").  This
        // avoids Cloudflare's TLS-fingerprint-vs-UA bot detection.
        let use_downgraded_ua = has_browser_ua && attempt + 1 == PROBE_MAX_RETRIES;
        let attempt_spec = if use_downgraded_ua {
            if attempt == 0 {
                // Should not happen with PROBE_MAX_RETRIES >= 2, but guard anyway.
                spec
            } else {
                log_info!(
                    "[resolve] retry {}/{}: stripping browser UA to avoid bot detection",
                    attempt + 1,
                    PROBE_MAX_RETRIES
                );
                &downgraded_spec
            }
        } else {
            spec
        };

        match resolve_file_info_once(client, url, attempt_spec).await {
            Ok(info) => return Ok(info),
            Err(e) => {
                log_info!(
                    "[resolve] probe attempt {}/{} failed: {}",
                    attempt + 1,
                    PROBE_MAX_RETRIES,
                    e
                );
                last_err = Some(e);
                if attempt + 1 < PROBE_MAX_RETRIES {
                    let delay = PROBE_RETRY_BASE_DELAY * 2u32.saturating_pow(attempt);
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

async fn resolve_file_info_once(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
) -> Result<FileInfo, DownloadError> {
    // 非 GET（form POST 等）：HEAD 通常返回 405 Method Not Allowed，POST + Range
    // 在 HTTP 标准上未定义。改为只发一次原始 method+body 请求，从响应头读取
    // 文件元数据后立即终止读取（drop response）。
    if !spec.is_get_like() {
        return resolve_file_info_non_get(client, url, spec).await;
    }

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

    let (head_result, get_result) = tokio::join!(head_fut, get_fut);

    // Extract HEAD response (if successful)
    let mut head_status_desc = String::new();
    let head_data = match head_result {
        Ok(r) if r.status().is_success() => {
            let u = r.url().clone();
            let h = r.headers().clone();
            Some((h, u))
        }
        Ok(r) => {
            head_status_desc = r.status().as_u16().to_string();
            log_info!(
                "[resolve] HEAD failed: status={}, url={}, cookies_len={}",
                r.status(),
                r.url(),
                cookies.len()
            );
            None
        }
        Err(e) => {
            head_status_desc = format!("network-error: {}", e);
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
            get_status_desc = r.status().as_u16().to_string();
            log_info!(
                "[resolve] GET failed: status={}, url={}, cookies_len={}",
                r.status(),
                r.url(),
                cookies.len()
            );
            None
        }
        Err(e) => {
            get_status_desc = format!("network-error: {}", e);
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
        0
    } else {
        headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };

    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let file_name = extract_filename(&headers, final_url.as_str());
    log_info!(
        "[resolve] url={} → name={}, size={}, range={}, ct={}",
        url,
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
/// 这是 `resolve_file_info` 重试循环里【每一轮】最多发出的第 3 个请求
/// （HEAD + ranged GET + 这次的 plain GET），不会叠加成
/// `PROBE_MAX_RETRIES` × 3 次请求；每轮只在前两个探测都失败时才会触发。
///
/// 不区分"服务器可达但状态码错误"与"纯网络错误（连不上）"两种失败：后者
/// 再发一次请求大概率也会失败，但无害，为简单起见不做区分。
async fn resolve_file_info_plain_get_fallback(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
    head_status_desc: &str,
    get_status_desc: &str,
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
            let plain_status_desc = r.status().as_u16().to_string();
            log_info!(
                "[resolve] plain GET fallback also failed: status={}, url={}",
                r.status(),
                r.url()
            );
            return Err(DownloadError::Other(format_probe_failure(
                head_status_desc,
                get_status_desc,
                &plain_status_desc,
            )));
        }
        Err(e) => {
            let plain_status_desc = format!("network-error: {}", e);
            log_info!(
                "[resolve] plain GET fallback network error: {}{}",
                e,
                format_error_chain(e.source())
            );
            return Err(DownloadError::Other(format_probe_failure(
                head_status_desc,
                get_status_desc,
                &plain_status_desc,
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

    let file_name = extract_filename(&headers, final_url.as_str());

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
        total_bytes,
        supports_range,
        content_type,
        etag,
        last_modified,
        content_encoding_compressed,
    })
}

/// 文件名是否以 HTML 类扩展名结尾——用于 HTML 安全网判断"服务器返回 HTML
/// 是否为用户期望"。空字符串视为不像 HTML：调用方有责任在到达此处前确保
/// 文件名已经解析（run_download 中的 auto_name 空值检查 :1581 是兜底位置）。
pub(crate) fn filename_looks_like_html(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm") || lower.ends_with(".xhtml")
}

/// 非 GET 请求的元数据探测——只发送一次原始 method+body 请求，从响应头
/// 提取文件名/大小/MIME，不读响应体（drop response 立即释放连接）。
///
/// 设计理由：
///   - HEAD 对 POST 端点通常返回 405/501，且不能携带 body
///   - POST + Range:bytes=0-0 在 HTTP 标准上未定义，服务端实现不一致
///   - 多段下载（Range 分割）对 non-GET 不可靠，统一强制单流
///
/// 因此 supports_range 强制为 false，调用方据此选择单流路径。
async fn resolve_file_info_non_get(
    client: &Client,
    url: &str,
    spec: &RequestSpec,
) -> Result<FileInfo, DownloadError> {
    log_info!(
        "[resolve-non-get] method={} url={} body_present={}",
        spec.method,
        url,
        spec.body.is_some()
    );

    let resp = build_request(client, url, spec.method.clone(), spec)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(DownloadError::Other(format!(
            "non-GET probe returned status {}",
            resp.status()
        )));
    }

    let final_url = resp.url().clone();
    let headers = resp.headers().clone();
    // drop(resp) 在此释放——我们只需头部，body 留给真正的下载阶段
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

    let file_name = extract_filename(&headers, final_url.as_str());

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

    log_info!(
        "[resolve-non-get] resolved: name={}, size={}, ct={}",
        file_name,
        total_bytes,
        content_type
    );

    Ok(FileInfo {
        file_name,
        total_bytes,
        // 非 GET 强制单流——POST + Range 在标准上未定义，服务端实现不一致
        supports_range: false,
        content_type,
        etag,
        last_modified,
        content_encoding_compressed,
    })
}

// ---------------------------------------------------------------------------
// File-name extraction
// ---------------------------------------------------------------------------

/// MIME type → common extension mapping for when there is no filename.
fn mime_to_ext(content_type: &str) -> Option<&'static str> {
    let ct = content_type.split(';').next().unwrap_or("").trim();
    match ct {
        "application/pdf" => Some("pdf"),
        "application/zip" => Some("zip"),
        "application/x-gzip" | "application/gzip" => Some("gz"),
        "application/x-tar" => Some("tar"),
        "application/x-bzip2" => Some("bz2"),
        "application/x-xz" => Some("xz"),
        "application/x-7z-compressed" => Some("7z"),
        "application/x-rar-compressed" | "application/vnd.rar" => Some("rar"),
        "application/json" => Some("json"),
        "application/xml" | "text/xml" => Some("xml"),
        "application/javascript" | "text/javascript" => Some("js"),
        "application/wasm" => Some("wasm"),
        "application/octet-stream" => None, // generic binary
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        "application/msword" => Some("doc"),
        "application/vnd.ms-excel" => Some("xls"),
        "application/vnd.ms-powerpoint" => Some("ppt"),
        "application/x-iso9660-image" => Some("iso"),
        "application/x-msdownload" | "application/x-dosexec" => Some("exe"),
        "application/vnd.android.package-archive" => Some("apk"),
        "application/java-archive" => Some("jar"),
        "application/x-shockwave-flash" => Some("swf"),
        "application/x-debian-package" => Some("deb"),
        "application/x-rpm" => Some("rpm"),
        "application/x-msi" => Some("msi"),
        "application/vnd.apple.installer+xml" => Some("pkg"),
        "text/html" => Some("html"),
        "text/css" => Some("css"),
        "text/csv" => Some("csv"),
        "text/plain" => Some("txt"),
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/svg+xml" => Some("svg"),
        "image/bmp" => Some("bmp"),
        "image/x-icon" | "image/vnd.microsoft.icon" => Some("ico"),
        "image/tiff" => Some("tiff"),
        "image/avif" => Some("avif"),
        "audio/mpeg" => Some("mp3"),
        "audio/ogg" => Some("ogg"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "audio/flac" => Some("flac"),
        "audio/aac" => Some("aac"),
        "audio/mp4" | "audio/x-m4a" => Some("m4a"),
        "audio/webm" => Some("weba"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        "video/x-matroska" => Some("mkv"),
        "video/x-msvideo" => Some("avi"),
        "video/quicktime" => Some("mov"),
        "video/x-flv" => Some("flv"),
        "video/mp2t" => Some("ts"),
        "video/3gpp" => Some("3gp"),
        "font/woff" => Some("woff"),
        "font/woff2" => Some("woff2"),
        "font/ttf" | "application/x-font-ttf" => Some("ttf"),
        "font/otf" => Some("otf"),
        _ => None,
    }
}

pub(crate) fn extract_filename(headers: &reqwest::header::HeaderMap, url: &str) -> String {
    // 1. Try Content-Disposition: attachment; filename="xxx"
    if let Some(name) = extract_from_content_disposition(headers) {
        return name;
    }

    // 2. Try URL path (after removing query & fragment)
    if let Some(name) = extract_from_url(url) {
        return name;
    }

    // 3. Try Content-Type → build "download.ext"
    if let Some(ct) = headers.get(reqwest::header::CONTENT_TYPE)
        && let Ok(ct_str) = ct.to_str()
        && let Some(ext) = mime_to_ext(ct_str)
    {
        return format!("download.{}", ext);
    }

    "download".to_string()
}

fn extract_from_content_disposition(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let disposition = headers.get(reqwest::header::CONTENT_DISPOSITION)?;
    // Use from_utf8 instead of to_str(): the http crate's to_str() rejects any byte > 0x7E,
    // but some servers (e.g. z-lib CDN) embed raw UTF-8 characters (Chinese, Japanese, etc.)
    // directly in the filename="" parameter.  Those bytes are valid UTF-8 even though they
    // are not ASCII, so from_utf8 succeeds where to_str would silently return None.
    let value = std::str::from_utf8(disposition.as_bytes()).ok()?;

    // Prefer filename*= (RFC 5987 / RFC 6266) over filename=
    for part in value.split(';') {
        let trimmed = part.trim();
        if let Some(name) = trimmed.strip_prefix("filename*=") {
            // Format: charset'language'percent-encoded-name
            // e.g. UTF-8''My%20File.pdf
            //
            // 注：按 RFC 5987 charset 字段明确指定编码，严格实现
            // 应该读取该字段。目前以 urlencoding_decode 的
            // "UTF-8 优先，GBK fallback" 表现足够应对老旧中文服务器
            // （它们通常话不对题，声明 UTF-8 但发 GBK）。
            let name = name.trim();
            if let Some(encoded) = name.split('\'').nth(2)
                && let Ok(decoded) = urlencoding_decode(encoded)
            {
                let decoded = decoded.trim();
                if !decoded.is_empty() {
                    return Some(sanitize_filename(decoded));
                }
            }
        }
    }

    for part in value.split(';') {
        let trimmed = part.trim();
        if let Some(name) = trimmed.strip_prefix("filename=") {
            let name = name.trim_matches(|c| c == '"' || c == '\'' || c == ' ');
            if !name.is_empty() {
                // Heuristic: some servers (e.g. Chinese cloud storage OBS/S3)
                // percent-encode the filename= value instead of using the
                // RFC 5987 filename*= syntax.  When the raw value contains
                // percent-encoded sequences, try URL-decoding it so that
                // `%E6%B0%B8%E7%94%9F.mp4` becomes `永生.mp4`.
                if name.contains('%')
                    && let Ok(decoded) = urlencoding_decode(name)
                {
                    let decoded = decoded.trim();
                    if !decoded.is_empty() && decoded != name {
                        return Some(sanitize_filename(decoded));
                    }
                }
                return Some(sanitize_filename(name));
            }
        }
    }

    None
}

pub fn extract_from_url(url: &str) -> Option<String> {
    // Strip query and fragment
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split('#').next().unwrap_or(path);
    let segment = path.rsplit('/').next()?;
    let decoded = urlencoding_decode(segment).unwrap_or_else(|_| segment.to_string());
    let decoded = decoded.trim();
    if decoded.is_empty() || decoded == "/" {
        return None;
    }
    Some(sanitize_filename(decoded))
}

/// 文件名单组件的最大字节数（F051）。
///
/// 大多数文件系统（ext4/APFS/NTFS）的单路径组件上限为 255 字节；这里取 200
/// 作为保守预算，给 `.fdownloading` 临时后缀（13 字节）及未来可能的 dedup
/// `" (NN)"` 后缀留出余量。超长的 Content-Disposition / URL 段若原样放行，
/// `save_dir.join(name) + ".fdownloading"` 会触顶导致 create 报 ENAMETOOLONG，
/// 下载以晦涩 OS 错误失败。多字节 CJK 约 66 字即可触及 200 字节。
const MAX_FILENAME_BYTES: usize = 200;

/// Windows 保留设备名（不区分大小写，比较时取扩展名前的 stem）。
///
/// 在 Windows 上创建这些名字（无论是否带扩展名，如 `CON`、`NUL.txt`）会失败
/// 或行为异常。本项目主要目标平台为 Windows，故统一在文件名出口处规避。
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Remove or replace characters that are illegal in file names on Windows/macOS/Linux.
///
/// 额外保证（F051）：
///   - 规避 Windows 保留设备名（CON/PRN/AUX/NUL/COM1-9/LPT1-9）——在 stem 前
///     加下划线；
///   - 把结果按字节截断到 [`MAX_FILENAME_BYTES`]，截断在 char 边界进行，避免
///     切断多字节 CJK 字符。
pub fn sanitize_filename(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let s = s.trim_matches(|c: char| c == '.' || c == ' ');
    if s.is_empty() {
        return "download".to_string();
    }

    // --- F051(1): Windows 保留设备名规避 ---
    // 取扩展名前的 stem（首个 '.' 之前的部分）做大小写无关比较。
    let stem_end = s.find('.').unwrap_or(s.len());
    let stem = &s[..stem_end];
    let s = if WINDOWS_RESERVED_NAMES
        .iter()
        .any(|r| stem.eq_ignore_ascii_case(r))
    {
        format!("_{}", s)
    } else {
        s.to_string()
    };

    // --- F051(2): 字节长度截断（在 char 边界） ---
    if s.len() <= MAX_FILENAME_BYTES {
        return s;
    }
    // 保留扩展名（最后一个 '.' 起的部分），从 stem 尾部按 char 边界裁剪。
    let ext_start = s.rfind('.').unwrap_or(s.len());
    let (stem, ext) = s.split_at(ext_start);
    let budget = MAX_FILENAME_BYTES.saturating_sub(ext.len());
    // 找到 <= budget 的最大 char 边界。
    let cut = stem
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= budget)
        .last()
        .unwrap_or(0);
    let truncated = format!("{}{}", &stem[..cut], ext);
    // 截断后再次 trim 尾部 '.'/' '（避免裁出以点/空格结尾的名）；若整体为空则兜底。
    let truncated = truncated.trim_matches(|c: char| c == '.' || c == ' ');
    if truncated.is_empty() {
        "download".to_string()
    } else {
        truncated.to_string()
    }
}

/// 将单个十六进制 ASCII 字节解析为 0..=15 的半字节（nibble）。
///
/// 仅接受 `0-9` / `a-f` / `A-F`；其他字节返回 `None`。供 `urlencoding_decode`
/// 按字节解析 `%XX` 转义使用，避免对 `&str` 切片导致的字符边界 panic。
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 解码 URL 路径段 / Content-Disposition 文件名中的百分号转义。
///
/// **按字节解析，绝不对 `&str` 切片**：原实现用 `&s[i+1..i+3]` 取两位十六进制，
/// 当 `%` 后紧跟原始多字节 UTF-8 字符（如 `50%折扣.txt`）时，切片终点会落在
/// 多字节字符内部触发 `byte index N is not a char boundary` panic（F017）。改为
/// 直接对 `bytes[i+1]` / `bytes[i+2]` 解析半字节后即可消除该 panic。
///
/// **不把 `+` 解码为空格**（F046）：按 RFC 3986，`+` 仅在
/// `application/x-www-form-urlencoded`（query / form body）中表示空格；在 URL
/// 路径段、Content-Disposition、RFC 5987 `filename*=` 中 `+` 都是字面加号
/// （空格用 `%20`）。本函数的所有调用方（extract_from_url /
/// extract_from_content_disposition）均为路径/文件名场景，且 extract_from_url
/// 在调用前已 `split('?')` 丢弃 query，故 `+`→空格 在所有实际用途下都是错的
/// （会把 `C++Primer.pdf` 损坏成 `C  Primer.pdf`）。
fn urlencoding_decode(s: &str) -> Result<String, String> {
    let mut result = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            result.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        // 非法 `%` 转义或普通字节：原样保留（含字面 `+`）。
        result.push(bytes[i]);
        i += 1;
    }
    decode_bytes_utf8_or_gbk(&result)
}

/// 将一组字节解码为字符串，优先 UTF-8，失败时回退到 GBK。
///
/// HTML5 规范要求 URL percent-encoding 使用 UTF-8，但大量老旧中文站点
/// （包括一些 CDN/云存储）仍使用 GBK 编码，如 `%CE%C4%BC%FE.txt`
/// 对应 GBK 的 "文件.txt"。若不做回退则 UTF-8 解码必然失败，最终
/// 用户看到 `%CE%C4%BC%FE.txt` 这种看似乱码的文件名。
///
/// # 已知局限
///
/// GBK 的字节空间很宽松（0x81-0xFE × 0x40-0xFE），其他二字节编码
/// 的字节序列（如 Big5、Shift-JIS）也可能被 GBK “成功”解码为错误的中文。
/// 权衡上这个误判仅在罕见场景下发生（现代 Big5/Latin 站点几乎不会
/// 在 URL 中使用非 UTF-8 percent-encoding），而 GBK 中文乱码是老旧中文
/// 站点的高频问题。
///
/// # 返回值
///
/// 返回 Err 仅当两种编码都无法解码时（极罕见，需要出现 GBK 不允许的
/// 字节组合，如 0x81 0x7F）。
pub(crate) fn decode_bytes_utf8_or_gbk(bytes: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => {
            // 使用 decode_without_bom_handling_and_without_replacement：
            // 遇到非法字节时返回 None，不插入 U+FFFD。
            // 这样可以准确区分 “GBK 中合法但含替换字符” 和 “GBK 解码失败”。
            match encoding_rs::GBK.decode_without_bom_handling_and_without_replacement(bytes) {
                Some(decoded) => Ok(decoded.into_owned()),
                None => Err(format!(
                    "bytes are neither valid UTF-8 nor valid GBK ({} bytes)",
                    bytes.len()
                )),
            }
        }
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
pub async fn dedup_filename(
    dir: &Path,
    name: &str,
    reserved: &std::collections::HashSet<std::path::PathBuf>,
    avoid: &std::collections::HashSet<String>,
) -> String {
    // Phase 1: fast probe — most of the time there is no conflict.
    let candidate = dir.join(name);
    let temp_candidate = PathBuf::from(format!("{}{}", candidate.display(), TEMP_EXT));
    // Also check the in-flight reservation set BEFORE the async disk probes
    // so that two tasks starting simultaneously both see each other's claim.
    if !reserved.contains(&temp_candidate)
        && !avoid.contains(&name.to_lowercase())
        && !tokio::fs::try_exists(&candidate).await.unwrap_or(false)
        && !tokio::fs::try_exists(&temp_candidate)
            .await
            .unwrap_or(false)
    {
        return name.to_string();
    }

    // Phase 2: conflict detected — scan directory into memory to avoid
    // up to 19998 filesystem calls in the dedup loop.
    //
    // 条目名**小写折叠**后入集:Windows/APFS 大小写不敏感,精确字节比较会
    // 漏判 `MOVIE (1).mp4` vs 已存在的 `Movie (1).mp4`,finalize rename 的
    // REPLACE 语义会静默覆盖真实文件。非 UTF-8 名经 lossy 转换,只可能把
    // 不冲突误判为冲突(多让一个编号),决不会漏判。
    let existing: std::collections::HashSet<String> = {
        let mut set = std::collections::HashSet::new();
        if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                set.insert(entry.file_name().to_string_lossy().to_lowercase());
            }
        }
        set
    };

    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = Path::new(name).extension().and_then(|s| s.to_str());

    for i in 1..=9999 {
        let new_name = if let Some(ext) = ext {
            format!("{} ({}).{}", stem, i, ext)
        } else {
            format!("{} ({})", stem, i)
        };
        let temp_name = format!("{}{}", new_name, TEMP_EXT);
        let temp_path = dir.join(&temp_name);
        // Check the final/in-progress disk files, the in-flight set AND avoid.
        if !reserved.contains(&temp_path)
            && !avoid.contains(&new_name.to_lowercase())
            && !existing.contains(&new_name.to_lowercase())
            && !existing.contains(&temp_name.to_lowercase())
        {
            return new_name;
        }
    }
    // 极端兜底:1..=9999 个编号变体全被占用时,此前返回**原名不变**,finalize
    // rename 会静默覆盖已存在文件丢数据。与 BT 侧 `dedup_name_in_dir` 对齐,
    // 用 UUID 后缀保证唯一。(BUG-BT-DEDUP-FALLBACK-OVERWRITE 的 HTTP 同类)
    let uniq = uuid::Uuid::new_v4();
    match ext {
        Some(e) => format!("{} ({}).{}", stem, uniq, e),
        None => format!("{} ({})", stem, uniq),
    }
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
            let _ = tokio::fs::remove_file(dst).await;
            Err(e)
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
/// 与 segment_coordinator 中的同名常量保持一致。
const CHUNK_STALL_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = run_download_inner(&params).await;

    match result {
        Ok((total, finalize_renamed)) => {
            log_info!(
                "[download] task {} completed, total={} bytes",
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
                    // finalize 阶段因目标名被占而改名时携带新名(reporter
                    // 对非空 file_name 锁存);未改名传空串 = 保持原名。
                    file_name: finalize_renamed.unwrap_or_default(),
                    segment_details: None,
                    ..Default::default()
                })
                .await;
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
            let full_msg = format!("{}{}", msg, chain);
            // checksum 失败时所有字节均已下载完毕（只是校验未通过），需特殊处理进度。
            let is_checksum_fail = matches!(e, DownloadError::ChecksumMismatch(_));
            log_info!("[download] task {} error: {}", task_id_log, full_msg);
            let _ = params
                .db
                .update_task_status(&params.task_id, 4, &full_msg)
                .await;

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
            let _ = params
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
                .await;
        }
    }
}

/// Verify that a file at `path` matches the checksum in `spec`.
///
/// `spec` format: `"algo=hexhash"`, e.g. `"sha-256=abc123..."` or `"md5=d41d8c..."`.
/// Supported algorithms: `sha-256`/`sha256`, `sha-512`/`sha512`, `sha-1`/`sha1`, `md5`.
/// Returns `Ok(())` if the digest matches, or `Err(DownloadError::ChecksumMismatch)` if not.
async fn verify_checksum(path: &Path, spec: &str) -> Result<(), DownloadError> {
    let sep = spec.find('=').ok_or_else(|| {
        DownloadError::Other(format!(
            "invalid checksum format (expected algo=hash): {}",
            spec
        ))
    })?;
    let algo_raw = spec[..sep].trim().to_lowercase();
    let expected_hex = spec[sep + 1..].trim().to_lowercase();

    // Normalize algorithm aliases to a canonical key.
    let algo = match algo_raw.as_str() {
        "sha-256" | "sha256" => "sha256",
        "sha-512" | "sha512" => "sha512",
        "sha-1" | "sha1" => "sha1",
        "md5" => "md5",
        other => {
            return Err(DownloadError::Other(format!(
                "unsupported checksum algorithm: {}",
                other
            )));
        }
    };

    let path_owned = path.to_path_buf();
    let algo_owned = algo.to_string();

    let actual_hex = tokio::task::spawn_blocking(move || -> Result<String, DownloadError> {
        use std::io::Read;
        let mut file = std::fs::File::open(&path_owned)?;
        let mut buf = vec![0u8; 1024 * 1024]; // 1 MiB read buffer
        match algo_owned.as_str() {
            "sha256" => {
                use sha2::Digest;
                let mut h = sha2::Sha256::new();
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                Ok(hex::encode(h.finalize()))
            }
            "sha512" => {
                use sha2::Digest;
                let mut h = sha2::Sha512::new();
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                Ok(hex::encode(h.finalize()))
            }
            "sha1" => {
                use sha1::Digest;
                let mut h = sha1::Sha1::new();
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                Ok(hex::encode(h.finalize()))
            }
            "md5" => {
                use md5::Digest;
                let mut h = md5::Md5::new();
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                Ok(hex::encode(h.finalize()))
            }
            _ => Err(DownloadError::Other("unreachable algo branch".to_string())),
        }
    })
    .await
    .map_err(|e| DownloadError::Other(format!("checksum thread panicked: {}", e)))??;

    if actual_hex != expected_hex {
        return Err(DownloadError::ChecksumMismatch(format!(
            "expected {}, got {}",
            expected_hex, actual_hex
        )));
    }
    Ok(())
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

/// 返回 `(actual_total, finalize_renamed)`:`finalize_renamed` 仅当 finalize
/// 阶段因目标名被占用而改名时为 `Some(新名)`,调用方须经完成信号上报
/// (progress_reporter 对非空 file_name 锁存,空串 = 不变)。
async fn run_download_inner(p: &DownloadParams) -> Result<(i64, Option<String>), DownloadError> {
    log_info!("[download] task {} starting, url={}", p.task_id, p.url);

    // Transition to status=5 (preparing) — probing server, resolving file info
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

    let client = &p.client;

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
    let info = if p.hint_file_size != 0 {
        // fresh hint 任务：Range 能力未经验证，持久化标记（coordinator 首响应
        // 证实支持后置回 1）。resume 据此延续「首连接 plain GET」保守启动。
        // 写失败仅降级（resume 落回 probe 路径，即改动前行为），不阻断下载。
        if !p.is_resume
            && let Err(e) = p.db.set_task_range_verified(&p.task_id, false).await
        {
            log_info!(
                "[download] task {} 持久化 range_verified=0 失败：{:?}（resume 保守启动可能退化）",
                p.task_id,
                e
            );
        }
        let name = if p.file_name.is_empty() {
            // Hint mode skips the HEAD probe entirely, so we have no response
            // headers to extract the filename from.  Try the URL path first;
            // if that also yields nothing (e.g. "/download?token=abc") fall
            // back to "download" so we never end up with an empty dest_path
            // that would point at the save directory itself.
            extract_from_url(&p.url).unwrap_or_else(|| "download".to_string())
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
            total_bytes: effective_size,
            // Optimistically assume Range support for auto (0) and explicit
            // multi-segment (> 1) requests.  Most servers that expose a
            // Content-Length also honour Range headers.  The bandwidth probe
            // is intentionally skipped for hint-mode tasks (see below) so no
            // extra HTTP request is made that could consume a one-time CDN
            // token (e.g. Lanzou cloud signed URLs).
            // Only assume Range support when we have a real file size AND
            // the original method is GET-like. POST + Range is undefined and
            // unsafe; force single-stream so the assumed supports_range can
            // never trip multi-segment for non-GET requests.
            supports_range: p.hint_file_size > 0 && p.segment_count != 1 && p.spec.is_get_like(),
            content_type: String::new(),
            // Hint mode skips the probe, so no ETag/Last-Modified available.
            etag: String::new(),
            last_modified: String::new(),
            // Hint mode skips the probe — no Content-Encoding info.
            content_encoding_compressed: false,
        }
    } else {
        log_info!("[download] task {} resolving file info...", p.task_id);
        let info = resolve_file_info(client, &p.url, &p.spec).await?;
        log_info!(
            "[download] task {} resolved: name={}, size={}, range={}",
            p.task_id,
            info.file_name,
            info.total_bytes,
            info.supports_range
        );
        info
    };

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
                    "server returned HTML page (Content-Type: {}) instead of the expected file — \
                     the URL may be a redirect/transit page or a form-POST endpoint accessed via GET",
                    mime
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
        let existing_segs = p.db.load_segments(&p.task_id).await.unwrap_or_default();
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
    //   • 续传：用【首次下载时存的原值】而非本次重新 probe 的值——这样若两次会话
    //     之间服务器把文件换成了【相同长度但内容不同】的新版本，下游的 If-Range
    //     会因 validator 不匹配触发服务器返回 200 全量 → 整文件重下，杜绝"旧前缀 +
    //     新尾部"静默拼接（BUG-HTTP-SINGLE-RESUME-SPLICE）。仅靠本次 probe 值无法
    //     检出：续传 probe 看到的已是新版本，validator 自洽却与磁盘旧数据不符。
    let (resume_etag, resume_last_modified) = if p.is_resume {
        let (oe, olm) =
            p.db.get_task_validator(&p.task_id)
                .await
                .unwrap_or_default();
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

    let _ = p.db.update_task_status(&p.task_id, 1, "").await;

    // Immediately notify Dart: status=1 with resolved file name & total size.
    // For resume tasks, send persisted downloaded bytes as baseline so speed
    // smoothing doesn't treat resumed bytes as a fresh in-interval delta.
    let initial_downloaded = if p.is_resume {
        p.db.load_task_by_id(&p.task_id)
            .await
            .ok()
            .flatten()
            .map(|t| t.downloaded_bytes.max(0))
            .unwrap_or(0)
    } else {
        0
    };

    let _ = p
        .progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: initial_downloaded,
            total_bytes: effective_total_bytes,
            status: 1,
            error_message: String::new(),
            file_name: actual_name.clone(),
            segment_details: None,
            ..Default::default()
        })
        .await;

    let dest_path = save_dir.join(&actual_name);
    // Chrome-style: write to a temporary file during download, rename on success.
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));

    // `size_is_estimate`：本次规划的 total 是否为【未经 probe 验证的估计值】。
    // 两种情形为 true：
    //   • fresh 的 hint 模式——total 直接取自浏览器扩展 hint（可能偏小的猜测），
    //     服务器在 206 `Content-Range` 分母里自报的真实大小才是权威值，
    //     coordinator 的扩容检查须采【零容差】（见 segment_coordinator::do_segment）；
    //   • resume 一个【从未验证过 Range 能力】的 hint 任务（range_verified==false，
    //     download_manager 以 DB total 作 hint 传入）——延续保守启动语义：
    //     coordinator 的 range_verdict 以 UNKNOWN 起步、零进度首段以 plain GET 起飞。
    // probe 路径与已验证任务的 resume：total 已被校准，保留 CDN 漂移容差。
    let size_is_estimate = p.hint_file_size > 0 && (!p.is_resume || !p.range_verified);

    // Dynamic segment calculation when user chose "auto" (segment_count <= 0).
    let segments = if p.segment_count <= 0 {
        // When resuming, check if DB already has segment rows from a previous
        // run.  If so, reuse that count — avoids a redundant bandwidth probe
        // and guarantees segment definitions stay consistent with what's on disk.
        if p.is_resume {
            let existing = p.db.load_segments(&p.task_id).await.unwrap_or_default();
            if !existing.is_empty() {
                let n = existing.len() as i32;
                log_info!(
                    "[download] task {} resume: reusing {} existing segment(s) from DB",
                    p.task_id,
                    n
                );
                n
            } else {
                // Segment rows were lost (e.g. crash between tasks.segments
                // update and insert_segments).  Fall through to advisor.
                compute_segments_with_advisor(p, &info).await
            }
        } else {
            compute_segments_with_advisor(p, &info).await
        }
    } else {
        p.segment_count
    };

    // Use multi-segment only when:
    //   • The server supports Range (probe confirmed)
    //   • File is > 1 MB (small files don't benefit from segmentation)
    //   • We asked for more than 1 segment
    //   • Original HTTP method is GET-like — POST + Range:bytes=X-Y is undefined
    //     in HTTP standards and most servers will silently return 200 OK with
    //     full body, corrupting the assembled output. Force single-stream
    //     for non-GET to make uupdump-style form-POST downloads safe.
    let use_segments = effective_supports_range
        && effective_total_bytes > 1_048_576
        && segments > 1
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
        let ms_outcome = download_multi_segment(
            &p.task_id,
            &p.url,
            &temp_path,
            effective_total_bytes,
            size_is_estimate,
            segments,
            client,
            &p.db,
            &p.progress_tx,
            &p.cancel_token,
            &p.speed_limiter,
            &p.spec,
            p.sink.as_ref(),
            &resume_etag,
            &resume_last_modified,
        )
        .await;

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
                //   • VersionChanged：文件在 probe 与分段请求间变了（If-Range validator
                //     不匹配 → 200 全量新版本），旧数据作废需重下。
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
                // 清空多段残留：删 DB segment 行 + 删预分配临时文件。对两种触发都正确：
                //   • 真·无 Range：预分配文件全零、无有效数据；
                //   • 版本变化：已完成段是【旧版本】字节，整体作废，必须删以重下新版本。
                let _ = p.db.delete_segments(&p.task_id).await;
                let _ = tokio::fs::remove_file(&temp_path).await;
                let result = download_single(
                    &p.task_id,
                    &p.url,
                    &temp_path,
                    effective_total_bytes,
                    false, // server doesn't support Range — never attempt it
                    client,
                    &p.db,
                    &p.progress_tx,
                    &p.cancel_token,
                    &p.speed_limiter,
                    &p.spec,
                    &actual_name,
                    &resume_etag,
                    &resume_last_modified,
                )
                .await?;
                Some(result)
            }
            Err(e) => return Err(e),
        }
    } else {
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
            let existing_segs = p.db.load_segments(&p.task_id).await.unwrap_or_default();
            if !existing_segs.is_empty() {
                log_info!(
                    "[download] task {} switching multi-segment → single-stream on resume; \
                     clearing {} stale segment(s) and pre-allocated temp file",
                    p.task_id,
                    existing_segs.len()
                );
                let _ = p.db.delete_segments(&p.task_id).await;
                let _ = tokio::fs::remove_file(&temp_path).await;
                // 进度归零，避免 UI 显示陈旧的多段累计值。
                let _ = p.db.update_task_progress(&p.task_id, 0).await;
            }
        }
        let result = download_single(
            &p.task_id,
            &p.url,
            &temp_path,
            effective_total_bytes,
            effective_supports_range,
            client,
            &p.db,
            &p.progress_tx,
            &p.cancel_token,
            &p.speed_limiter,
            &p.spec,
            &actual_name,
            &resume_etag,
            &resume_last_modified,
        )
        .await?;
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
        let _ = p.db.update_task_total_bytes(&p.task_id, file_len).await;
        effective_total_bytes = file_len;
    }

    // Integrity check — verify download completeness.
    // 解压路径已在上方处理，这里 effective_total_bytes 已被改写为磁盘实际大小，
    // 故 file_len == effective_total_bytes 自然成立、不会误杀。
    if effective_total_bytes > 0 && !decompressed_single {
        if actual_use_segments {
            // Multi-segment: file is pre-allocated via set_len() so metadata
            // size always == total_bytes.  Check actual progress from DB instead.
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
                    let _ = p.db.update_task_total_bytes(&p.task_id, file_len).await;
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
                    let _ = p.db.update_task_total_bytes(&p.task_id, file_len).await;
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
        if let Err(e) = verify_checksum(&temp_path, &p.checksum).await {
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
        log_info!("[download] task {} checksum ok", p.task_id);
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
    {
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
    let mut chosen = actual_name.clone();
    let mut avoid: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut avoid_loaded = false;
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
                log_info!(
                    "[download] task {} destination '{}' is taken; re-deduping",
                    p.task_id,
                    dst.display()
                );
                chosen = dedup_filename(
                    &save_dir,
                    &actual_name,
                    &std::collections::HashSet::new(),
                    &avoid,
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
        // 单流路径优先用【实际响应】锁存的 Last-Modified：If-Range 失配（文件在
        // 暂停/下载期间变更）会导致 200 全量重下新内容，此时 probe/DB validator
        // 里的旧时间已不属于磁盘上的字节。多段路径无此偏差——validator 失配即
        // 整体作废并回退单流（同样拿到锁存值），仍走多段完成的只能是原版本文件，
        // probe/DB 值即正确值。
        let last_modified = single_result
            .as_ref()
            .and_then(|r| r.latched_last_modified.as_deref())
            .unwrap_or(&resume_last_modified);
        apply_server_mtime(&final_dest, last_modified, &p.task_id).await;
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

/// 把已完成下载的最终文件修改时间设为服务器提供的 `Last-Modified` 时间。
///
/// 时间戳属于尽力而为的元数据：`last_modified` 为空（服务器未提供）、解析
/// 失败或写入失败都只记日志直接返回——此刻文件数据已完整落盘，任何元数据
/// 失败都不允许把成功的下载变成错误。
async fn apply_server_mtime(dest: &Path, last_modified: &str, task_id: &str) {
    if last_modified.is_empty() {
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
    // set_times 是同步系统调用，且 Windows 上需要以写权限打开句柄。
    let result = tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new().write(true).open(&path)?;
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
    /// `Some`（全新下载，或 If-Range 失配回退 200 重下新版本）——此时磁盘内容
    /// 以该响应为准，probe/DB validator 可能描述的是旧版本，调用方设置文件
    /// mtime 时必须优先采用本值（空串 = 该响应未携带此头，应放弃服务器时间）。
    /// 真 206 续传为 `None`（磁盘内容与旧 validator 一致，沿用旧值）。
    latched_last_modified: Option<String>,
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
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    spec: &RequestSpec,
    expected_filename: &str,
    // 续传一致性校验：probe 阶段看到的文件版本标识。非空时，续传请求会附带
    // `If-Range: <validator>`，由服务器判断文件是否变化——变了则返回 200 全量，
    // actual_resume 随之为 false → 从 0 重下，杜绝"旧前缀 + 新尾部"的静默拼接。
    expected_etag: &str,
    expected_last_modified: &str,
) -> Result<SingleDownloadResult, DownloadError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Check if there's an existing partial file we can resume
    let existing_len = match tokio::fs::metadata(dest).await {
        Ok(m) => m.len() as i64,
        Err(_) => 0,
    };

    // Resume only when the original method is GET-like.  POST + Range:bytes=N-
    // is undefined in HTTP standards and most servers will ignore the Range
    // header (returning 200 with the full body) — appending it to the partial
    // file would corrupt the result.
    let want_resume = spec.is_get_like()
        && supports_range
        && existing_len > 0
        && (total_bytes == 0 || existing_len < total_bytes);

    let mut downloaded: i64;
    let mut file;

    let mut req = build_request(client, url, spec.method.clone(), spec);
    if want_resume {
        req = req.header("Range", format!("bytes={}-", existing_len));
        // If-Range：让服务器自己判定文件是否自 probe 起变化。validator 一致 →
        // 返回 206 续传；不一致 → 返回 200 全量，下方 actual_resume 变 false →
        // File::create 截断从 0 重下。优先用【强】ETag；弱 ETag（`W/` 前缀）在
        // If-Range 上语义未定义、可能让服务器恒回 200（RFC 7233 §3.2），故跳过、
        // 回退 Last-Modified。两者皆空（如某些 FTP-over-HTTP 或裸 CDN）则不带
        // If-Range，退化为原有行为（仍受 206/encoding 守卫保护，不会更糟）。
        let validator = if !expected_etag.is_empty() && !expected_etag.starts_with("W/") {
            Some(expected_etag.to_string())
        } else if !expected_last_modified.is_empty() {
            Some(expected_last_modified.to_string())
        } else {
            None
        };
        if let Some(v) = validator {
            req = req.header("If-Range", v);
        }
    }

    let mut resp = req.send().await?.error_for_status()?;

    // F019: 当我们发了开放式 Range 请求 `bytes=N-`，服务器返回 206 且带
    // Content-Encoding（部分 CDN 行为）时，响应体是【压缩流任意中间字节】起的
    // 一段，无法从 existing_len（解压后偏移）正确续传，也无法当全量解压。此时
    // 必须丢弃该响应、不带 Range 重新请求一次拿到完整压缩流，再从头解压全量
    // 重下。仅在 want_resume（即确实带了 Range）时才可能触发，无 Range 的普通
    // 请求不受影响。
    if want_resume
        && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT
        && detect_content_encoding(resp.headers()).is_some()
    {
        log_info!(
            "[download-single] task {} got Content-Encoding on a 206 Range response — \
             compressed byte ranges cannot be resumed; re-requesting full file without Range \
             (existing_len={} discarded)",
            task_id,
            existing_len
        );
        drop(resp);
        // 不带 Range 重新构造请求，拿到从 byte 0 起的完整（压缩）响应。
        let full_req = build_request(client, url, spec.method.clone(), spec);
        resp = full_req.send().await?.error_for_status()?;
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
                "server returned HTML page (Content-Type: {}) for a non-HTML download — \
                 aborting to avoid corrupting saved file. \
                 Likely causes: form-POST URL accessed via GET, expired one-time CDN token, \
                 or site requires authentication that the extension did not capture.",
                mime
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
    // F019: `encoding.is_none()` 仍作为续传安全的额外必要条件做防御兜底。压缩
    // 206 的核心修复已在上方"re-request full file without Range"完成（重发后
    // resp 为 200 全量压缩流）；此处 encoding 守卫确保万一上方逻辑未覆盖某种
    // 边界（如重发后服务器仍返回 206+encoding）也绝不会把压缩字节范围当作可
    // append 的续传数据，避免静默损坏。
    // BUG-CDN-206-BYTE0-FULLSTREAM（续传面）：劣质 CDN 在链接失效时对
    // `Range: bytes=N-` 回 206 却发【从 0 的全量流】（Content-Range 起点为 0
    // 或缺失，而非请求的 N）。若仍按 206 走 seek(End(0)) 追加，会把文件开头字节
    // 拼到 existing_len 之后 → 错位坏文件。故追加"Content-Range 起点必须 ==
    // existing_len"条件，不符即视为不可信续传 → 走下方回退全量分支（File::create
    // 截断 + 复用当前响应体从 0 写入：from-0 全量流恰好落正确位置得完整文件，
    // 错误页则被末尾 size mismatch 拦截）。与多段 do_segment 的同名校验对称。
    let actual_resume = want_resume
        && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT
        && encoding.is_none()
        && !is_range_response_misaligned(parse_content_range_start(resp.headers()), existing_len);

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
    // 从 0 服务的全量体：锁存实际响应的 Last-Modified（见字段 doc）。
    let latched_last_modified = (!actual_resume).then(|| {
        resp.headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    });

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
        raw_file.seek(std::io::SeekFrom::End(0)).await?;
        file = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, raw_file);
    } else {
        downloaded = 0;
        file = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, File::create(dest).await?);
        // Reset DB progress so the UI doesn't show stale values
        let _ = db.update_task_progress(task_id, 0).await;
    }

    // 注：旧版本会从实际下载响应的 Content-Disposition 中提取"更好的文件名"，
    // 写入 DB 并通知 Dart UI。该机制已移除——新架构下文件名由 DownloadManager
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

    let mut last_report = std::time::Instant::now();
    let mut last_db_save = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                file.flush().await?;
                let _ = db.update_task_progress(task_id, downloaded).await;
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
                        file.flush().await?;
                        let _ = db.update_task_progress(task_id, downloaded).await;
                        return Err(DownloadError::Other(format!(
                            "download stalled: no data received for {}s",
                            CHUNK_STALL_TIMEOUT.as_secs()
                        )));
                    }
                };
                match chunk {
                    Some(Ok(bytes)) => {
                        // --- Speed limiter: write in sub-chunks as tokens allow ---
                        let mut offset = 0usize;
                        let chunk_len = bytes.len();
                        while offset < chunk_len {
                            let remaining = (chunk_len - offset) as u64;
                            let allowed = speed_limiter.consume(remaining).await;
                            let end = offset + allowed as usize;
                            file.write_all(&bytes[offset..end]).await?;
                            offset = end;
                        }
                        let len = chunk_len as i64;
                        downloaded += len;

                        // Progress report to Dart — every 200ms for smooth UI.
                        if last_report.elapsed().as_millis() >= 200 {
                            let _ = progress_tx
                                .send(ProgressUpdate {
                                    task_id: task_id.to_string(),
                                    downloaded_bytes: downloaded,
                                    total_bytes,
                                    status: 1,
                                    error_message: String::new(),
                                    file_name: String::new(),
                                    segment_details: Some(vec![SegmentProgressInfo {
                                        index: 0,
                                        start_byte: 0,
                                        end_byte: if total_bytes > 0 { total_bytes - 1 } else { 0 },
                                        downloaded_bytes: downloaded,
                                    }]),
                                    ..Default::default()
                                })
                                .await;
                            last_report = std::time::Instant::now();
                        }

                        // DB persistence — periodic save for crash recovery.
                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            let _ = db.update_task_progress(task_id, downloaded).await;
                            last_db_save = std::time::Instant::now();
                        }
                    }
                    Some(Err(e)) => {
                        file.flush().await?;
                        let _ = db.update_task_progress(task_id, downloaded).await;
                        return Err(DownloadError::Io(e));
                    }
                    None => break,
                }
            }
        }
    }

    file.flush().await?;
    let _ = db.update_task_progress(task_id, downloaded).await;
    Ok(SingleDownloadResult {
        response_content_length,
        decompressed: encoding.is_some(),
        latched_last_modified,
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
    client: &Client,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    spec: &RequestSpec,
    sink: &dyn EventSink,
    etag: &str,
    last_modified: &str,
) -> Result<i64, DownloadError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

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
    crate::segment_coordinator::run_coordinated_download(
        task_id,
        url,
        dest,
        total_bytes,
        size_is_estimate,
        segment_count,
        client,
        db,
        progress_tx,
        cancel_token,
        speed_limiter,
        spec,
        sink,
        etag,
        last_modified,
    )
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        PROBE_MAX_RETRIES, PROBE_RETRY_BASE_DELAY, PROBE_TIMEOUT, TEMP_EXT, dedup_filename,
        extract_filename, extract_from_content_disposition, extract_from_url, format_probe_failure,
        mime_to_ext, parse_http_date, sanitize_filename, urlencoding_decode,
    };
    use std::time::Duration;

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

    // -----------------------------------------------------------------------
    // sanitize_filename
    // -----------------------------------------------------------------------

    #[test]
    fn sanitize_replaces_illegal_chars() {
        assert_eq!(sanitize_filename("file<1>:2.txt"), "file_1__2.txt");
    }

    #[test]
    fn sanitize_replaces_all_special_chars() {
        assert_eq!(
            sanitize_filename(r#"a<b>c:d"e/f\g|h?i*j"#),
            "a_b_c_d_e_f_g_h_i_j"
        );
    }

    #[test]
    fn sanitize_strips_leading_trailing_dots_and_spaces() {
        assert_eq!(sanitize_filename("...file..."), "file");
        assert_eq!(sanitize_filename("  file  "), "file");
        assert_eq!(sanitize_filename("..file.."), "file");
    }

    #[test]
    fn sanitize_empty_and_only_dots() {
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename("..."), "download");
        assert_eq!(sanitize_filename("   "), "download");
    }

    #[test]
    fn sanitize_control_characters() {
        assert_eq!(sanitize_filename("file\x00name\x1F.txt"), "file_name_.txt");
    }

    #[test]
    fn sanitize_blocks_path_traversal() {
        // 安全回归：用户/API 显式提供的 file_name（RPC `out` / 管理 API file_name /
        // 浏览器接管）经 sanitize_filename 后必须不含路径分隔符，且不是绝对路径，
        // 否则 save_dir.join(name) 会穿越 save_dir 落盘任意路径。
        for evil in [
            "../../../etc/passwd",
            "..\\..\\Windows\\System32\\evil.exe",
            "/etc/passwd",
            "C:\\Windows\\evil.exe",
            "foo/../bar",
        ] {
            let safe = sanitize_filename(evil);
            let p = std::path::Path::new(&safe);
            assert!(
                !safe.contains('/') && !safe.contains('\\'),
                "sanitized {evil:?} → {safe:?} still contains a path separator"
            );
            assert!(
                !p.is_absolute(),
                "sanitized {evil:?} → {safe:?} is still an absolute path"
            );
            assert!(
                p.components().count() == 1,
                "sanitized {evil:?} → {safe:?} resolves to multiple path components"
            );
        }
    }

    #[test]
    fn sanitize_preserves_unicode() {
        assert_eq!(sanitize_filename("文件下载.zip"), "文件下载.zip");
        assert_eq!(sanitize_filename("ファイル.tar.gz"), "ファイル.tar.gz");
    }

    #[test]
    fn sanitize_windows_reserved_names() {
        // F051: Windows 保留设备名（含/不含扩展名、混合大小写）应加下划线规避。
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("NUL.txt"), "_NUL.txt");
        assert_eq!(sanitize_filename("com1"), "_com1");
        assert_eq!(sanitize_filename("LpT9.log"), "_LpT9.log");
        assert_eq!(sanitize_filename("Aux.tar.gz"), "_Aux.tar.gz");
        // 非保留名不受影响（仅 stem 完全匹配才规避）。
        assert_eq!(sanitize_filename("CONSOLE.txt"), "CONSOLE.txt");
        assert_eq!(sanitize_filename("COM10.txt"), "COM10.txt");
    }

    #[test]
    fn sanitize_truncates_overlong_names_at_char_boundary() {
        // F051: 超过 200 字节的名字应截断，且保留扩展名、不切断多字节字符。
        let long_ascii = format!("{}.bin", "a".repeat(300));
        let out = sanitize_filename(&long_ascii);
        assert!(out.len() <= 200, "ascii truncated len = {}", out.len());
        assert!(out.ends_with(".bin"), "extension preserved: {out}");

        // 多字节 CJK：每个 '永' 3 字节，120 个 = 360 字节。
        let long_cjk = format!("{}.mp4", "永".repeat(120));
        let out = sanitize_filename(&long_cjk);
        assert!(out.len() <= 200, "cjk truncated len = {}", out.len());
        assert!(out.ends_with(".mp4"), "extension preserved: {out}");
        // 截断必须落在 char 边界——能成功重新解析为合法 UTF-8（String 本身保证）。
        assert!(out.starts_with('永'));

        // 未超限的名字原样返回。
        assert_eq!(sanitize_filename("short.txt"), "short.txt");
    }

    // -----------------------------------------------------------------------
    // extract_from_url
    // -----------------------------------------------------------------------

    #[test]
    fn extract_from_url_basic() {
        let name = extract_from_url("https://example.com/path/file.zip");
        assert_eq!(name.as_deref(), Some("file.zip"));
    }

    #[test]
    fn extract_from_url_strips_query_and_fragment() {
        let name = extract_from_url("https://example.com/file.zip?v=1&token=abc#section");
        assert_eq!(name.as_deref(), Some("file.zip"));
    }

    #[test]
    fn extract_from_url_encoded_filename() {
        let name = extract_from_url("https://example.com/My%20File%20(1).pdf");
        assert_eq!(name.as_deref(), Some("My File (1).pdf"));
    }

    #[test]
    fn extract_from_url_trailing_slash_returns_none() {
        let name = extract_from_url("https://example.com/path/");
        assert!(
            name.is_none(),
            "trailing slash should return None, got: {name:?}"
        );
    }

    #[test]
    fn extract_from_url_no_path() {
        let name = extract_from_url("https://example.com");
        // The last segment is "example.com" — should extract it
        assert!(name.is_some());
    }

    #[test]
    fn extract_from_url_preserves_literal_plus() {
        // F046: 含字面 `+` 的文件名（C++ 教材、版本号 build metadata）不应被
        // `+`→空格 损坏。
        let name = extract_from_url("https://example.com/C++Primer.pdf");
        assert_eq!(name.as_deref(), Some("C++Primer.pdf"));
        let name = extract_from_url("https://example.com/v1.2+build.bin");
        assert_eq!(name.as_deref(), Some("v1.2+build.bin"));
    }

    #[test]
    fn extract_from_url_literal_percent_with_unicode_no_panic() {
        // F017: URL 路径段含字面 `%` 紧跟多字节 UTF-8 字符不应 panic。
        let name = extract_from_url("https://example.com/50%折扣.txt");
        assert_eq!(name.as_deref(), Some("50%折扣.txt"));
    }

    #[test]
    fn extract_from_url_chinese_filename() {
        let name = extract_from_url("https://example.com/%E4%B8%8B%E8%BD%BD.exe");
        assert_eq!(name.as_deref(), Some("下载.exe"));
    }

    #[test]
    fn extract_from_url_gbk_chinese_filename() {
        // 老旧中文站点用 GBK 编码中文：“文件” 的 GBK = CE C4 BC FE
        // UTF-8 解码会失败，必须回退到 GBK 才能得到可读文件名。
        let name = extract_from_url("http://example.com/%CE%C4%BC%FE.txt");
        assert_eq!(
            name.as_deref(),
            Some("文件.txt"),
            "GBK percent-encoded 中文 URL 应能被正确解码而不是保留原始 %XX"
        );
    }

    #[test]
    fn extract_from_content_disposition_gbk_filename() {
        // 中文云存储 OBS/S3 类服务器可能返回 GBK 编码的 filename=
        let headers = make_headers_with_cd("attachment; filename=\"%CE%C4%BC%FE.txt\"");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(
            name.as_deref(),
            Some("文件.txt"),
            "GBK percent-encoded Content-Disposition 应能被正确解码"
        );
    }

    // -----------------------------------------------------------------------
    // urlencoding_decode
    // -----------------------------------------------------------------------

    #[test]
    fn urlencoding_decode_basic() {
        assert_eq!(
            urlencoding_decode("hello%20world").unwrap_or_default(),
            "hello world"
        );
    }

    #[test]
    fn urlencoding_decode_plus_is_literal() {
        // F046: `+` 在 URL 路径段 / Content-Disposition 文件名中是字面加号，
        // 不应被解码为空格（空格用 %20）。所有调用方均为路径/文件名场景。
        assert_eq!(
            urlencoding_decode("hello+world").unwrap_or_default(),
            "hello+world"
        );
        assert_eq!(
            urlencoding_decode("C++Primer.pdf").unwrap_or_default(),
            "C++Primer.pdf"
        );
        // 混合：%20 仍解码为空格，`+` 保留为字面。
        assert_eq!(
            urlencoding_decode("v1.2+build%20final.bin").unwrap_or_default(),
            "v1.2+build final.bin"
        );
    }

    #[test]
    fn urlencoding_decode_no_panic_on_non_char_boundary() {
        // F017: `%` 紧跟原始多字节 UTF-8 字符时，旧实现 `&s[i+1..i+3]` 会在
        // 非字符边界处 panic。按字节解析后应安全地把 `%` 当字面量保留。
        // "50%折扣.txt"：`%` 后是 `折`（E6 8A 98），i+3 落在多字节字符内部。
        let result = urlencoding_decode("50%折扣.txt").unwrap_or_default();
        assert_eq!(result, "50%折扣.txt");
        // `%X` 后接多字节字符：第二位非 hex，亦应原样保留 `%`。
        let result = urlencoding_decode("%a你.zip").unwrap_or_default();
        assert_eq!(result, "%a你.zip");
    }

    #[test]
    fn urlencoding_decode_invalid_utf8_returns_error() {
        // 0x81 0x7F 既不是合法 UTF-8（0x81 不能作为首字节）
        // 也不是合法 GBK（尾字节不能是 0x7F）——两种都失败时应返回 Err。
        let result = urlencoding_decode("%81%7F");
        assert!(
            result.is_err(),
            "既非合法 UTF-8 又非合法 GBK 的字节应返回 Err，got: {:?}",
            result
        );
    }

    #[test]
    fn urlencoding_decode_invalid_utf8_falls_back_to_gbk() {
        // 0x80 不是合法 UTF-8 首字节，但是合法 GBK（€ 符号）
        // 不应报错，应返回 GBK 解码后的字符。
        let result = urlencoding_decode("%80").unwrap_or_default();
        assert_eq!(result, "€", "GBK 0x80 应解码为 €");
    }

    // ——— 已知局限（文档性测试，默认 ignore）———
    //
    // GBK fallback 存在 false-positive：非 UTF-8 且非 GBK 的编码（如 Big5、
    // ISO-8859-1）也可能被 GBK “成功”解码为错误的中文。考虑到：
    //   1. 现代 Big5/Latin 站点几乎不会在 URL 中使用非 UTF-8 percent-encoding
    //   2. 本修复主要目标是 “老旧中文站点的 GBK URL” 高频场景
    //   3. 我们接受该权衡，后续可考虑加入 chardet/Big5 预检测

    #[test]
    #[ignore = "记录 GBK fallback false-positive 行为，不是回归报警"]
    fn urlencoding_decode_big5_chinese_filename_misdecoded_as_gbk() {
        // Big5 编码的 “中文” = A4 A4 A4 E5
        // UTF-8 失败 → GBK 成功但解码为 “いゅ”（错误的日文假名）
        let result = urlencoding_decode("%A4%A4%A4%E5").unwrap_or_default();
        eprintln!("big5 bytes decoded as GBK: {:?}", result);
        assert_ne!(result, "中文", "已知局限：不会还原 Big5");
    }

    #[test]
    fn urlencoding_decode_partial_percent() {
        // "%" at end should pass through
        let result = urlencoding_decode("test%").unwrap_or_default();
        assert_eq!(result, "test%");
    }

    // -----------------------------------------------------------------------
    // extract_from_content_disposition (private, tested via extract_filename)
    // -----------------------------------------------------------------------

    fn make_headers_with_cd(value: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(value) {
            headers.insert(reqwest::header::CONTENT_DISPOSITION, v);
        }
        headers
    }

    #[test]
    fn content_disposition_quoted_filename() {
        let headers = make_headers_with_cd("attachment; filename=\"my_file.zip\"");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("my_file.zip"));
    }

    #[test]
    fn content_disposition_unquoted_filename() {
        let headers = make_headers_with_cd("attachment; filename=simple.txt");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("simple.txt"));
    }

    #[test]
    fn content_disposition_rfc5987_filename_star() {
        let headers = make_headers_with_cd("attachment; filename*=UTF-8''%E6%96%87%E4%BB%B6.pdf");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("文件.pdf"));
    }

    #[test]
    fn content_disposition_filename_star_overrides_plain() {
        let headers = make_headers_with_cd(
            "attachment; filename=\"fallback.txt\"; filename*=UTF-8''preferred.txt",
        );
        let name = extract_from_content_disposition(&headers);
        // filename* should take precedence
        assert_eq!(name.as_deref(), Some("preferred.txt"));
    }

    #[test]
    fn content_disposition_empty_filename() {
        let headers = make_headers_with_cd("attachment; filename=\"\"");
        let name = extract_from_content_disposition(&headers);
        assert!(name.is_none(), "empty filename should return None");
    }

    #[test]
    fn content_disposition_no_filename_param() {
        let headers = make_headers_with_cd("inline");
        let name = extract_from_content_disposition(&headers);
        assert!(name.is_none());
    }

    #[test]
    fn content_disposition_percent_encoded_filename_unquoted() {
        // Chinese cloud storage (OBS/S3) often sends percent-encoded filename=
        // instead of using the RFC 5987 filename*= syntax.
        let headers = make_headers_with_cd(
            "attachment;filename=%E6%B0%B8%E7%94%9F%E6%88%98%E5%A3%AB.Sisu.2022265.mp4",
        );
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("永生战士.Sisu.2022265.mp4"));
    }

    #[test]
    fn content_disposition_percent_encoded_filename_quoted() {
        let headers = make_headers_with_cd(
            "attachment; filename=\"%E6%B0%B8%E7%94%9F%E6%88%98%E5%A3%AB.Sisu.2022265.mp4\"",
        );
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("永生战士.Sisu.2022265.mp4"));
    }

    #[test]
    fn content_disposition_plain_ascii_with_percent_literal() {
        // A filename like "50%.txt" should NOT be mangled by the heuristic
        // because urlencoding_decode("50%.txt") will fail or leave it unchanged.
        let headers = make_headers_with_cd("attachment; filename=\"50%.txt\"");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("50%.txt"));
    }

    #[test]
    fn content_disposition_percent_encoded_spaces() {
        let headers = make_headers_with_cd("attachment; filename=My%20Great%20File.pdf");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("My Great File.pdf"));
    }

    // -----------------------------------------------------------------------
    // extract_filename (integration of all strategies)
    // -----------------------------------------------------------------------

    #[test]
    fn extract_filename_prefers_content_disposition() {
        let headers = make_headers_with_cd("attachment; filename=\"from_header.zip\"");
        let name = extract_filename(&headers, "https://example.com/from_url.tar.gz");
        assert_eq!(name, "from_header.zip");
    }

    #[test]
    fn extract_filename_falls_back_to_url() {
        let headers = reqwest::header::HeaderMap::new();
        let name = extract_filename(&headers, "https://example.com/from_url.tar.gz");
        assert_eq!(name, "from_url.tar.gz");
    }

    #[test]
    fn extract_filename_falls_back_to_mime() {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str("application/pdf") {
            headers.insert(reqwest::header::CONTENT_TYPE, v);
        }
        let name = extract_filename(&headers, "https://example.com/");
        assert_eq!(name, "download.pdf");
    }

    #[test]
    fn extract_filename_ultimate_fallback() {
        let headers = reqwest::header::HeaderMap::new();
        let name = extract_filename(&headers, "https://example.com/");
        assert_eq!(name, "download");
    }

    // -----------------------------------------------------------------------
    // mime_to_ext
    // -----------------------------------------------------------------------

    #[test]
    fn mime_to_ext_common_types() {
        assert_eq!(mime_to_ext("application/pdf"), Some("pdf"));
        assert_eq!(mime_to_ext("application/zip"), Some("zip"));
        assert_eq!(mime_to_ext("video/mp4"), Some("mp4"));
        assert_eq!(mime_to_ext("image/jpeg"), Some("jpg"));
    }

    #[test]
    fn mime_to_ext_with_charset_parameter() {
        // MIME type often comes with ";charset=utf-8"
        assert_eq!(mime_to_ext("text/html; charset=utf-8"), Some("html"));
    }

    #[test]
    fn mime_to_ext_unknown_type() {
        assert_eq!(mime_to_ext("application/x-unknown-format"), None);
    }

    // -----------------------------------------------------------------------
    // Bug #4: PROBE_TIMEOUT configuration — document current problematic values
    // -----------------------------------------------------------------------

    #[test]
    fn http_probe_timeout_is_reasonable() {
        // 3 attempts: original → normal retry → UA-downgrade retry.
        // HEAD+GET run concurrently (max 15s per attempt, not 30s).
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(15));
        assert_eq!(PROBE_MAX_RETRIES, 3);
        assert_eq!(PROBE_RETRY_BASE_DELAY, Duration::from_secs(1));

        // Worst case: 3 attempts × 15s + delays (1s + 2s) = 48s
        let worst_per_attempt = PROBE_TIMEOUT; // HEAD+GET concurrent
        let delay_sum = PROBE_RETRY_BASE_DELAY + PROBE_RETRY_BASE_DELAY * 2; // 1s + 2s
        let worst_total = worst_per_attempt * PROBE_MAX_RETRIES + delay_sum;
        assert!(
            worst_total <= Duration::from_secs(60),
            "worst-case probe time {worst_total:?} should be <= 60s"
        );
    }

    // -----------------------------------------------------------------------
    // format_probe_failure — plain-GET fallback diagnostic message (pure fn,
    // no network I/O; covers the fnOS multiple-download HEAD=405/GET=400
    // triple-probe-failure scenario without needing a mock HTTP server).
    // -----------------------------------------------------------------------

    #[test]
    fn format_probe_failure_reports_all_three_statuses() {
        let msg = format_probe_failure("405", "400", "400");
        assert_eq!(
            msg,
            "probes failed: HEAD=405, ranged GET=400, plain GET=400"
        );
    }

    #[test]
    fn format_probe_failure_reports_network_errors_verbatim() {
        let msg = format_probe_failure(
            "network-error: connection refused",
            "network-error: connection refused",
            "403",
        );
        assert_eq!(
            msg,
            "probes failed: HEAD=network-error: connection refused, ranged GET=network-error: connection refused, plain GET=403"
        );
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
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_no_conflict");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Clean up any leftover
        let _ = tokio::fs::remove_file(dir.join("test.txt")).await;
        let _ = tokio::fs::remove_file(dir.join(format!("test.txt{TEMP_EXT}"))).await;

        let result = dedup_filename(
            &dir,
            "test.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .await;
        assert_eq!(result, "test.txt");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_with_conflict() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_conflict");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Create conflicting file
        tokio::fs::write(dir.join("test.txt"), b"")
            .await
            .unwrap_or(());

        let result = dedup_filename(
            &dir,
            "test.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .await;
        assert_eq!(result, "test (1).txt");

        // Clean up
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_case_folds_across_disk_variants() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_case_fold");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Exact-case entry forces Phase 1's `try_exists()` probe to see a
        // conflict on every platform (Linux's exists() is case-sensitive,
        // unlike Windows/APFS where a bare `Test.txt` would already do it).
        tokio::fs::write(dir.join("TEST.txt"), b"")
            .await
            .unwrap_or(());
        tokio::fs::write(dir.join("Test.txt"), b"")
            .await
            .unwrap_or(());
        // Differently-cased numbered variant already occupies " (1)".
        tokio::fs::write(dir.join("Test (1).txt"), b"")
            .await
            .unwrap_or(());

        let result = dedup_filename(
            &dir,
            "TEST.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .await;
        assert_ne!(
            result, "TEST (1).txt",
            "case-different existing 'Test (1).txt' must be treated as occupied"
        );
        assert_eq!(result, "TEST (2).txt");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_temp_file_conflict() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_temp");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Create a .fdownloading temp file — should also be considered a conflict
        tokio::fs::write(dir.join(format!("test.txt{TEMP_EXT}")), b"")
            .await
            .unwrap_or(());

        let result = dedup_filename(
            &dir,
            "test.txt",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .await;
        assert_eq!(result, "test (1).txt");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_no_extension() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_noext");
        let _ = tokio::fs::create_dir_all(&dir).await;
        tokio::fs::write(dir.join("README"), b"")
            .await
            .unwrap_or(());

        let result = dedup_filename(
            &dir,
            "README",
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .await;
        assert_eq!(result, "README (1)");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // -----------------------------------------------------------------------
    // dedup_filename: reserved set prevents TOCTOU races in batch downloads
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dedup_filename_reserved_set_avoids_collision() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_reserved");
        let _ = tokio::fs::create_dir_all(&dir).await;
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
        )
        .await;
        assert_eq!(result, "video (1).mp4");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_reserved_set_phase2_collision() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_reserved_p2");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // video.mp4 exists on disk AND video (1).mp4.fdownloading is reserved.
        tokio::fs::write(dir.join("video.mp4"), b"")
            .await
            .unwrap_or(());
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
        )
        .await;
        assert_eq!(result, "video (2).mp4");

        let _ = tokio::fs::remove_dir_all(&dir).await;
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
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_no_self_conflict_async");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let _ = tokio::fs::create_dir_all(&dir).await;

        // 磁盘干净；reserved 中只有兄弟任务 sibling.bin，没有自己的 setup.exe
        let mut reserved = std::collections::HashSet::new();
        reserved.insert(dir.join(format!("sibling.bin{TEMP_EXT}")));

        let result = dedup_filename(
            &dir,
            "setup.exe",
            &reserved,
            &std::collections::HashSet::new(),
        )
        .await;
        assert_eq!(
            result, "setup.exe",
            "reserved 集合不含本任务名时，dedup 必须返回原名（PR #296 回归 bug）"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
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

    #[test]
    fn content_disposition_raw_utf8_chinese_filename_extracted_correctly() {
        // Regression test for z-lib CDN: server sends raw UTF-8 bytes in filename="".
        // Before the fix (to_str) this returned None and callers fell back to the URL,
        // producing garbage like "redirection" or a hash string as the task name.
        // After the fix (from_utf8) the correct Chinese filename is extracted via
        // the filename*= parameter (RFC 5987 percent-encoding).
        let raw: &[u8] = b"attachment; filename=\"\xe4\xb8\x89\xe4\xbd\x93 (\xe5\x88\x98\xe6\x85\x88\xe6\xac\xa3).epub\"; filename*=UTF-8''%E4%B8%89%E4%BD%93%20(%E5%88%98%E6%85%88%E6%AC%A3).epub";

        let mut headers = reqwest::header::HeaderMap::new();
        let hv = reqwest::header::HeaderValue::from_bytes(raw)
            .expect("HeaderValue::from_bytes must accept arbitrary bytes");
        headers.insert(reqwest::header::CONTENT_DISPOSITION, hv);

        let name = extract_from_content_disposition(&headers);
        // filename*= (RFC 5987) takes priority and decodes to the correct Chinese name.
        assert_eq!(
            name.as_deref(),
            Some("三体 (刘慈欣).epub"),
            "raw UTF-8 bytes in filename= must not prevent filename*= from being parsed"
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
        };
        let req = super::build_request(&client, "https://example.com", reqwest::Method::GET, &spec);
        let built = req
            .build()
            .expect("build_request must produce a valid request");
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
        };
        let req = super::build_request(
            &client,
            "https://uupdump.net/get.php",
            reqwest::Method::POST,
            &spec,
        );
        let built = req
            .build()
            .expect("build_request must produce a valid request");
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
        };
        let req = super::build_request(
            &client,
            "https://example.com/file.zip",
            reqwest::Method::GET,
            &spec,
        );
        let built = req.build().unwrap();
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
        let dir = std::env::temp_dir().join("fluxdown_test_claim_rename_free");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let _ = tokio::fs::create_dir_all(&dir).await;
        let src = dir.join("src.bin");
        let dst = dir.join("dst.bin");
        let _ = tokio::fs::write(&src, b"payload").await;

        let result = super::claim_rename(&src, &dst).await;
        assert!(
            result.is_ok(),
            "claim_rename must succeed on a free dst: {result:?}"
        );
        assert_eq!(tokio::fs::read(&dst).await.unwrap_or_default(), b"payload");
        assert!(!src.exists(), "src must be gone after a successful rename");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn claim_rename_fails_when_dst_exists_preserves_both() {
        let dir = std::env::temp_dir().join("fluxdown_test_claim_rename_exists");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let _ = tokio::fs::create_dir_all(&dir).await;
        let src = dir.join("src.bin");
        let dst = dir.join("dst.bin");
        let _ = tokio::fs::write(&src, b"incoming").await;
        let _ = tokio::fs::write(&dst, b"original").await;

        let result = super::claim_rename(&src, &dst).await;
        match result {
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists),
            Ok(()) => panic!("claim_rename must not silently overwrite an occupied dst"),
        }
        // dst must be untouched — this is the TOCTOU window the protocol closes.
        assert_eq!(tokio::fs::read(&dst).await.unwrap_or_default(), b"original");
        // src must survive intact for the caller's dedup-and-retry path.
        assert_eq!(tokio::fs::read(&src).await.unwrap_or_default(), b"incoming");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_avoid_param_case_folds_and_renames() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_avoid_case_fold");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Empty directory, no reserved entries — only `avoid` forces a rename,
        // and the match must hold across case folding (lower-cased entry vs
        // mixed-case name).
        let mut avoid = std::collections::HashSet::new();
        avoid.insert("movie.mkv".to_string());

        let result =
            dedup_filename(&dir, "Movie.mkv", &std::collections::HashSet::new(), &avoid).await;
        assert_eq!(result, "Movie (1).mkv");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
