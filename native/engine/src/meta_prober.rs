//! 队列任务元数据探测 — 在任务等待期间后台探测文件名和大小。
//!
//! 支持协议:
//! - HTTP/HTTPS → HEAD 请求获取 Content-Disposition / Content-Length
//! - FTP        → 复用 ftp_downloader::resolve_ftp_file_info（SIZE 命令）
//! - magnet:    → 提取 dn= 参数作为文件名（无大小信息）
//! - torrent-file:// → 跳过（名称由 librqbit 解析后上报）

use tokio::time::Duration;

use crate::naming::{
    NameHints, NameSource, decode_legacy_bytes, extract_filename, sanitize_filename,
};

/// 探测超时（秒）
const PROBE_TIMEOUT_SECS: u64 = 8;

/// 元数据探测结果。
#[derive(Debug, Default)]
pub struct ProbedMeta {
    /// 探测到的文件名；空 = 无法探测或已有名称（`file_name` 参数非空时跳过名称探测）。
    pub file_name: String,
    /// 名字由引擎推断（HTTP 响应没有 Content-Disposition，取自 URL / Content-Type）：
    /// 落库时标记 `tasks.name_inferred`，HTTP 完成期可按实际 GET 响应精修。
    pub name_inferred: bool,
    /// 0 = 未知大小。
    pub total_bytes: i64,
}

impl ProbedMeta {
    /// 由下载器完整探测（HEAD ∥ Range GET，见 `downloader::resolve_file_info_with_ua_fallback`）
    /// 的结果派生启动序幕的名称 / 大小，规则与 [`probe_http_meta`] 一致：
    /// - HTML 页面不提供目标文件名或大小；明确的 HTML 附件保留探测元数据，
    ///   但其名称仍视为推断，不能授权后续响应保存任意网页。
    /// - 其它名字只有 Content-Disposition（含 URL 查询里的 CD）才算权威。
    pub(crate) fn from_file_info(info: &crate::downloader::FileInfo) -> Self {
        if !info.html_attachment
            && crate::downloader::html_evidence(
                &info.content_type,
                &info.file_name,
                info.name_source,
            )
            .is_some()
        {
            return Self::default();
        }
        Self {
            file_name: info.file_name.clone(),
            name_inferred: info.html_attachment
                || !matches!(
                    info.name_source,
                    NameSource::Disposition | NameSource::QueryDisposition
                ),
            total_bytes: info.total_bytes.max(0),
        }
    }
}

/// 探测队列任务的文件名和大小。
///
/// `spec` 携带任务的鉴权上下文（cookies / referrer / extra_headers），HTTP HEAD
/// probe 会用它通过 `downloader::build_request` 重建与真正下载一致的请求
/// （F020）。鉴权站点对缺少 cookies/Referer 的裸 HEAD 常返回登录页 / 错误页，
/// 携带鉴权后才能拿到真实的 Content-Disposition / Content-Length。FTP / magnet
/// 协议无 HTTP 头语义，忽略 `spec`。
pub async fn probe_task_meta(
    url: &str,
    file_name: &str, // DB 中已有的文件名；非空则跳过名称探测
    client: &reqwest::Client,
    proxy_config: &crate::proxy_config::ProxyConfig,
    spec: &crate::downloader::RequestSpec,
) -> ProbedMeta {
    // torrent-file:// 任务的名称由 librqbit 元数据解析后上报，跳过探测
    if url.starts_with("torrent-file://") {
        return ProbedMeta::default();
    }

    // 仅取前 8 字节做协议判断，避免不必要的堆分配
    let lower_prefix = url
        .get(..8)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    // magnet: — 从 dn= 参数提取文件名，无大小
    if lower_prefix.starts_with("magnet:") {
        let name = if file_name.is_empty() {
            extract_dn_from_magnet(url)
        } else {
            String::new()
        };
        return ProbedMeta {
            file_name: name,
            ..ProbedMeta::default()
        };
    }

    // ed2k:// — 链接自带文件名与大小，无网络探测（HEAD 无意义）
    if lower_prefix.starts_with("ed2k://") {
        #[cfg(feature = "ed2k")]
        return match crate::ed2k::link::parse_ed2k_link(url) {
            Ok(link) => ProbedMeta {
                file_name: if file_name.is_empty() {
                    link.file_name
                } else {
                    String::new()
                },
                name_inferred: false,
                total_bytes: link.total_bytes as i64,
            },
            Err(_) => ProbedMeta::default(),
        };
        // 无 eD2K 后端：不可探测，更不能当 HTTP 链接去请求。
        #[cfg(not(feature = "ed2k"))]
        return ProbedMeta::default();
    }

    // ftp:// — 使用现有 FTP 解析逻辑（FTP 无 HTTP 鉴权头语义，忽略 spec）
    if lower_prefix.starts_with("ftp://") {
        return probe_ftp_meta(url, file_name, proxy_config).await;
    }

    // HTTP / HTTPS
    probe_http_meta(url, file_name, client, spec).await
}

// ---------------------------------------------------------------------------
// magnet dn= 提取
// ---------------------------------------------------------------------------

fn extract_dn_from_magnet(url: &str) -> String {
    // magnet:?xt=urn:btih:HASH&dn=NAME&tr=...
    let query = url.split_once('?').map(|x| x.1).unwrap_or("");
    for part in query.split('&') {
        if let Some(val) = part.strip_prefix("dn=") {
            let decoded = url_decode(val);
            if !decoded.is_empty() {
                return sanitize_filename(&decoded);
            }
        }
    }
    String::new()
}

/// 将单个十六进制 ASCII 字节解析为 0..=15 的半字节（nibble）。
///
/// 仅接受 `0-9` / `a-f` / `A-F`；其他字节返回 `None`。供 `url_decode` 按字节
/// 解析 `%XX` 转义使用，避免对 `&str` 切片导致的字符边界 panic。
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 简易 URL 解码（`%XX` 转义 + `+` → 空格），用于 magnet `dn=` 查询参数。
///
/// **按字节解析，绝不对 `&str` 切片**（F018）：原实现用 `&s[i+1..i+3]` 取两位
/// 十六进制，当 `%` 后紧跟原始多字节 UTF-8 字符（如 `dn=name%a你`）时，切片
/// 终点会落在多字节字符内部触发 `byte index N is not a char boundary` panic。
/// 该函数解析的是用户直接粘贴的 magnet 链接（不可信输入），且其 spawn 任务无
/// catch_unwind 兜底，panic 会让整条探测链静默中止。
///
/// **UTF-8 失败时回退旧式字节解码**（F047）：老旧中文资源库常见 GBK 编码的 `dn=`（如
/// `%CE%C4%BC%FE`），与 downloader / ftp_downloader / bt_downloader 共用
/// [`decode_legacy_bytes`]，避免排队态显示乱码、进入下载后又跳变为正确名。
///
/// `dn=` 是 query 参数，按 `application/x-www-form-urlencoded` 语义保留
/// `+`→空格 行为。
fn url_decode(s: &str) -> String {
    let mut result = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            result.push(b' ');
            i += 1;
        } else if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            result.push((hi << 4) | lo);
            i += 3;
        } else {
            // 非法 `%` 转义或普通字节：原样保留。
            result.push(bytes[i]);
            i += 1;
        }
    }
    decode_legacy_bytes(&result, NameHints::default())
}

// ---------------------------------------------------------------------------
// FTP 探测（复用 ftp_downloader 的解析逻辑）
// ---------------------------------------------------------------------------

async fn probe_ftp_meta(
    url: &str,
    file_name: &str, // DB 中已有的文件名；非空则跳过名称覆盖（与 HTTP guard 对称）
    proxy_config: &crate::proxy_config::ProxyConfig,
) -> ProbedMeta {
    let result = tokio::time::timeout(
        Duration::from_secs(PROBE_TIMEOUT_SECS),
        // 元信息探测只要名称 / 大小；服务器时间由下载器完成期按需取。
        crate::ftp_downloader::resolve_ftp_file_info(url, proxy_config, false),
    )
    .await;
    match result {
        Ok(Ok(info)) => ProbedMeta {
            // If the user already set a custom file name, do not let the
            // server-side name overwrite it.  Return an empty name so the
            // caller skips the DB update (mirrors probe_http_meta behaviour).
            file_name: if file_name.is_empty() {
                info.file_name
            } else {
                String::new()
            },
            name_inferred: false,
            total_bytes: info.total_bytes,
        },
        _ => ProbedMeta::default(),
    }
}

// ---------------------------------------------------------------------------
// HTTP / HTTPS 探测
// ---------------------------------------------------------------------------

async fn probe_http_meta(
    url: &str,
    file_name: &str,
    client: &reqwest::Client,
    spec: &crate::downloader::RequestSpec,
) -> ProbedMeta {
    // F020（完整）：用 build_request 携带任务的 cookies/referrer/extra_headers
    // 重建 HEAD probe，使其与真正下载使用一致的鉴权上下文。HEAD method 显式覆盖
    // spec.method（即便任务是 POST 触发，probe 也只发 HEAD，与原行为一致；
    // build_request 对 GET/HEAD 不会附加 body）。
    let request = crate::downloader::build_request(client, url, reqwest::Method::HEAD, spec);
    let result =
        tokio::time::timeout(Duration::from_secs(PROBE_TIMEOUT_SECS), request.send()).await;

    let Ok(Ok(response)) = result else {
        return ProbedMeta::default();
    };
    // 非 2xx（预签名 GET URL 对 HEAD 回 403、PHP 端点回 405、错误页……）的响应头不描述
    // 目标文件；拿它的 URL 段当名字落库，会让真正下载时 GET 拿到的 Content-Disposition
    // 被这个占位名压住。text/html 同理，多半是登录页 / 错误页（F020）。
    if !response.status().is_success() {
        return ProbedMeta::default();
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let resolved = extract_filename(response.headers(), url, response.url().as_str());
    let html_attachment =
        crate::downloader::is_html_attachment(response.headers(), &resolved.name, resolved.source);
    if (!html_attachment
        || (!file_name.is_empty() && !crate::downloader::filename_looks_like_html(file_name)))
        && crate::downloader::html_evidence(content_type, &resolved.name, resolved.source).is_some()
    {
        return ProbedMeta::default();
    }
    let (file_name, name_inferred) = if file_name.is_empty() {
        let inferred = html_attachment
            || !matches!(
                resolved.source,
                NameSource::Disposition | NameSource::QueryDisposition
            );
        (resolved.name, inferred)
    } else {
        (String::new(), false)
    };
    ProbedMeta {
        file_name,
        name_inferred,
        // HEAD 响应的 `content_length()` 来自 body size_hint，恒为 0；必须读头部。
        total_bytes: head_content_length(response.headers()),
    }
}

/// 取 HEAD 响应头里的 Content-Length。带 Content-Encoding 时长度是压缩后的
/// 传输大小，不代表文件大小，按未知处理。
fn head_content_length(headers: &reqwest::header::HeaderMap) -> i64 {
    let encoded = headers
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && !v.eq_ignore_ascii_case("identity")
        })
        .unwrap_or(false);
    if encoded {
        return 0;
    }
    headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(0)
}

/// 链接探测（剪贴板识别）总超时，含重定向链：独立于元数据探测的 [`PROBE_TIMEOUT_SECS`]。
/// 下载站常见 2～3 跳重定向（SourceForge `latest/download` 实测 2 跳约 2.5s），1.5s 会把它们全判成 Unknown。
const LINK_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// 链接探测判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkVerdict {
    /// 确认是可下载资源。
    Resource,
    /// 确认是网页 / 文本 / 图片预览等非下载资源。
    NotResource,
    /// 网络错误、超时、非 2xx 或无 Content-Type，无法判定。
    Unknown,
}

/// 链接探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkProbe {
    pub verdict: LinkVerdict,
    /// 跟随重定向后的地址；未拿到响应时等于原 url。
    pub final_url: String,
    /// 判定为 `Resource` 时推断出的文件名，否则空。
    pub file_name: String,
    /// 小写 MIME essence（去参数），无则空。
    pub mime: String,
    /// 0 = 未知。
    pub total_bytes: i64,
}

impl LinkProbe {
    fn unknown(url: &str) -> Self {
        Self {
            verdict: LinkVerdict::Unknown,
            final_url: url.to_string(),
            file_name: String::new(),
            mime: String::new(),
            total_bytes: 0,
        }
    }
}

/// 只发一次 HEAD（绝不 GET / Range GET：一次性令牌链接会被消耗）判定链接是否为下载资源。
/// 总超时 [`LINK_PROBE_TIMEOUT`]；任何网络错误按 `Unknown`。
pub async fn probe_link_kind(
    url: &str,
    client: &reqwest::Client,
    spec: &crate::downloader::RequestSpec,
) -> LinkProbe {
    let request = crate::downloader::build_request(client, url, reqwest::Method::HEAD, spec);
    let Ok(Ok(response)) = tokio::time::timeout(LINK_PROBE_TIMEOUT, request.send()).await else {
        return LinkProbe::unknown(url);
    };
    classify_probe_response(
        response.status(),
        response.headers(),
        url,
        response.url().as_str(),
    )
}

/// 由 HEAD 响应（状态码、头、最终地址）判定链接类型；纯函数，便于表驱动测试。
fn classify_probe_response(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    url: &str,
    final_url: &str,
) -> LinkProbe {
    let mut probe = LinkProbe::unknown(final_url);
    if !status.is_success() {
        return probe;
    }
    probe.mime = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let attachment = headers
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v.starts_with("attachment") || v.contains("filename")
        })
        .unwrap_or(false);
    probe.verdict = if attachment {
        LinkVerdict::Resource
    } else if probe.mime.is_empty() {
        LinkVerdict::Unknown
    } else if is_page_like_mime(&probe.mime) {
        LinkVerdict::NotResource
    } else {
        LinkVerdict::Resource
    };
    if probe.verdict == LinkVerdict::Resource {
        probe.file_name = extract_filename(headers, url, final_url).name;
        probe.total_bytes = head_content_length(headers);
    }
    probe
}

/// 网页 / 文本 / 结构化数据 / 图片类 MIME（无 attachment 时不当下载）。
fn is_page_like_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime.starts_with("image/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/xhtml+xml"
        )
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
}

#[cfg(test)]
mod link_probe_tests {
    use super::{LinkVerdict, classify_probe_response};
    use reqwest::StatusCode;
    use reqwest::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap};

    fn headers(pairs: &[(reqwest::header::HeaderName, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(k.clone(), v.parse().expect("valid header value"));
        }
        h
    }

    #[test]
    fn classify_probe_response_table() {
        let ok = StatusCode::OK;
        let cases: Vec<(&str, StatusCode, HeaderMap, LinkVerdict)> = vec![
            (
                "attachment",
                ok,
                headers(&[
                    (CONTENT_TYPE, "text/html"),
                    (CONTENT_DISPOSITION, "attachment; filename=\"a.zip\""),
                ]),
                LinkVerdict::Resource,
            ),
            (
                "octet-stream",
                ok,
                headers(&[(CONTENT_TYPE, "application/octet-stream")]),
                LinkVerdict::Resource,
            ),
            (
                "html",
                ok,
                headers(&[(CONTENT_TYPE, "text/html; charset=utf-8")]),
                LinkVerdict::NotResource,
            ),
            (
                "json",
                ok,
                headers(&[(CONTENT_TYPE, "application/json")]),
                LinkVerdict::NotResource,
            ),
            (
                "image without attachment",
                ok,
                headers(&[(CONTENT_TYPE, "image/png")]),
                LinkVerdict::NotResource,
            ),
            (
                "image with attachment",
                ok,
                headers(&[
                    (CONTENT_TYPE, "image/png"),
                    (CONTENT_DISPOSITION, "attachment"),
                ]),
                LinkVerdict::Resource,
            ),
            (
                "+xml suffix",
                ok,
                headers(&[(CONTENT_TYPE, "application/atom+xml")]),
                LinkVerdict::NotResource,
            ),
            (
                "non-2xx",
                StatusCode::FORBIDDEN,
                headers(&[(CONTENT_TYPE, "application/zip")]),
                LinkVerdict::Unknown,
            ),
            (
                "no content-type",
                ok,
                HeaderMap::new(),
                LinkVerdict::Unknown,
            ),
        ];
        for (name, status, h, want) in cases {
            let got = classify_probe_response(status, &h, "https://x.test/d", "https://x.test/d");
            assert_eq!(got.verdict, want, "case: {name}");
        }
    }

    #[test]
    fn resource_carries_mime_name_size_and_final_url() {
        let h = headers(&[
            (CONTENT_TYPE, "Application/Zip; foo=bar"),
            (CONTENT_LENGTH, "42"),
        ]);
        let p = classify_probe_response(
            StatusCode::OK,
            &h,
            "https://x.test/d",
            "https://cdn.test/files/pkg.zip",
        );
        assert_eq!(p.verdict, LinkVerdict::Resource);
        assert_eq!(p.mime, "application/zip");
        assert_eq!(p.total_bytes, 42);
        assert_eq!(p.final_url, "https://cdn.test/files/pkg.zip");
        assert!(!p.file_name.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::head_content_length;

    #[test]
    fn head_content_length_reads_header_and_skips_encoded() {
        use reqwest::header::{CONTENT_ENCODING, CONTENT_LENGTH, HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        assert_eq!(head_content_length(&h), 0);
        h.insert(CONTENT_LENGTH, HeaderValue::from_static("1234"));
        assert_eq!(head_content_length(&h), 1234);
        h.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        assert_eq!(head_content_length(&h), 0);
    }

    use super::{extract_dn_from_magnet, url_decode};

    #[test]
    fn url_decode_basic_percent_and_plus() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        // `+` 在 query 参数中表示空格（form-urlencoded 语义）。
        assert_eq!(url_decode("hello+world"), "hello world");
    }

    #[test]
    fn url_decode_no_panic_on_non_char_boundary() {
        // F018: `%` 紧跟原始多字节 UTF-8 字符时，旧实现 `&s[i+1..i+3]` 会在
        // 非字符边界处 panic。按字节解析后应安全地把 `%` 当字面量保留。
        assert_eq!(url_decode("name%a你"), "name%a你");
        assert_eq!(url_decode("50%折扣"), "50%折扣");
    }

    #[test]
    fn url_decode_gbk_fallback() {
        // F047: 老旧中文资源库的 GBK 编码 dn=（"文件" 的 GBK = CE C4 BC FE）
        // 应回退 GBK 解码，而非保留乱码 %XX 串。
        assert_eq!(url_decode("%CE%C4%BC%FE"), "文件");
    }

    #[test]
    fn url_decode_utf8_chinese() {
        // UTF-8 编码的 "中文" = E4 B8 AD E6 96 87
        assert_eq!(url_decode("%E4%B8%AD%E6%96%87"), "中文");
    }

    #[test]
    fn extract_dn_gbk_magnet() {
        // 整条 magnet 链路：GBK 编码的 dn= 应解出可读中文文件名。
        let name = extract_dn_from_magnet("magnet:?xt=urn:btih:abc&dn=%CE%C4%BC%FE.txt&tr=x");
        assert_eq!(name, "文件.txt");
    }

    fn file_info(
        name: &str,
        source: crate::naming::NameSource,
        content_type: &str,
        total: i64,
    ) -> crate::downloader::FileInfo {
        crate::downloader::FileInfo {
            file_name: name.to_string(),
            name_source: source,
            html_attachment: false,
            total_bytes: total,
            supports_range: true,
            content_type: content_type.to_string(),
            etag: String::new(),
            last_modified: String::new(),
            content_encoding_compressed: false,
        }
    }

    #[test]
    fn from_file_info_marks_only_disposition_names_authoritative() {
        use crate::naming::NameSource;
        let cd = super::ProbedMeta::from_file_info(&file_info(
            "a.bin",
            NameSource::Disposition,
            "application/octet-stream",
            42,
        ));
        assert_eq!(
            (cd.file_name.as_str(), cd.name_inferred, cd.total_bytes),
            ("a.bin", false, 42)
        );
        let query_cd = super::ProbedMeta::from_file_info(&file_info(
            "b.bin",
            NameSource::QueryDisposition,
            "",
            0,
        ));
        assert!(!query_cd.name_inferred);
        let fallback = super::ProbedMeta::from_file_info(&file_info(
            "download",
            NameSource::Fallback,
            "application/octet-stream",
            -1,
        ));
        assert!(fallback.name_inferred);
        assert_eq!(fallback.total_bytes, 0);
    }

    #[test]
    fn from_file_info_html_page_yields_no_name_or_size() {
        use crate::naming::NameSource;
        for ct in [
            "text/html",
            "Text/HTML; charset=utf-8",
            "application/xhtml+xml",
        ] {
            let meta = super::ProbedMeta::from_file_info(&file_info(
                "login.php",
                NameSource::Disposition,
                ct,
                512,
            ));
            assert!(meta.file_name.is_empty(), "{ct}");
            assert_eq!(meta.total_bytes, 0, "{ct}");
        }
    }
}
