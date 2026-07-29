//! 队列任务元数据探测 — 在任务等待期间后台探测文件名和大小。
//!
//! 支持协议:
//! - HTTP/HTTPS → HEAD 请求获取 Content-Disposition / Content-Length
//! - FTP        → 复用 ftp_downloader::resolve_ftp_file_info（SIZE 命令）
//! - magnet:    → 提取 dn= 参数作为文件名（无大小信息）
//! - torrent-file:// → 跳过（名称由 librqbit 解析后上报）

use tokio::time::Duration;

use crate::downloader::extract_filename;

/// 探测超时（秒）
const PROBE_TIMEOUT_SECS: u64 = 8;

/// 探测队列任务的文件名和大小。
///
/// 返回 `(file_name, total_bytes)`。
/// - `file_name` 为空表示无法探测或已有名称（file_name 参数非空时跳过名称探测）
/// - `total_bytes` 为 0 表示未知大小
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
) -> (String, i64) {
    // torrent-file:// 任务的名称由 librqbit 元数据解析后上报，跳过探测
    if url.starts_with("torrent-file://") {
        return (String::new(), 0);
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
        return (name, 0);
    }

    // ed2k:// — 链接自带文件名与大小，无网络探测（HEAD 无意义）
    if lower_prefix.starts_with("ed2k://") {
        return match crate::ed2k::link::parse_ed2k_link(url) {
            Ok(link) => {
                let name = if file_name.is_empty() {
                    link.file_name
                } else {
                    String::new()
                };
                (name, link.total_bytes as i64)
            }
            Err(_) => (String::new(), 0),
        };
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
                return crate::downloader::sanitize_filename(&decoded);
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
/// **UTF-8 失败时回退 GBK**（F047）：老旧中文资源库常见 GBK 编码的 `dn=`（如
/// `%CE%C4%BC%FE`），与 downloader / ftp_downloader / bt_downloader 三处保持
/// 一致，避免排队态显示乱码、进入下载后又跳变为正确名。
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
    crate::downloader::decode_bytes_utf8_or_gbk(&result).unwrap_or_else(|_| s.to_string())
}

// ---------------------------------------------------------------------------
// FTP 探测（复用 ftp_downloader 的解析逻辑）
// ---------------------------------------------------------------------------

async fn probe_ftp_meta(
    url: &str,
    file_name: &str, // DB 中已有的文件名；非空则跳过名称覆盖（与 HTTP guard 对称）
    proxy_config: &crate::proxy_config::ProxyConfig,
) -> (String, i64) {
    let result = tokio::time::timeout(
        Duration::from_secs(PROBE_TIMEOUT_SECS),
        crate::ftp_downloader::resolve_ftp_file_info(url, proxy_config),
    )
    .await;
    match result {
        Ok(Ok(info)) => {
            // If the user already set a custom file name, do not let the
            // server-side name overwrite it.  Return an empty name so the
            // caller skips the DB update (mirrors probe_http_meta behaviour).
            let name = if file_name.is_empty() {
                info.file_name
            } else {
                String::new()
            };
            (name, info.total_bytes)
        }
        _ => (String::new(), 0),
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
) -> (String, i64) {
    // F020（完整）：用 build_request 携带任务的 cookies/referrer/extra_headers
    // 重建 HEAD probe，使其与真正下载使用一致的鉴权上下文。HEAD method 显式覆盖
    // spec.method（即便任务是 POST 触发，probe 也只发 HEAD，与原行为一致；
    // build_request 对 GET/HEAD 不会附加 body）。
    let request = crate::downloader::build_request(client, url, reqwest::Method::HEAD, spec);
    let result =
        tokio::time::timeout(Duration::from_secs(PROBE_TIMEOUT_SECS), request.send()).await;

    match result {
        Ok(Ok(response)) => {
            // F020（部分）：若 HEAD 响应为 text/html，多半是被重定向到登录页或
            // 返回了错误页（即便携带了鉴权，cookies 过期 / 资源已失效仍会如此）。
            // 此时 Content-Disposition/Content-Length 属于错误页面，解析出的
            // 文件名会污染 DB。跳过名称提取，返回空名。
            let is_html = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|ct| {
                    let mime = ct
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase();
                    mime == "text/html" || mime == "application/xhtml+xml"
                })
                .unwrap_or(false);
            let name = if file_name.is_empty() && !is_html {
                extract_filename(response.headers(), url, response.url().as_str())
            } else {
                String::new()
            };
            // HTML 错误页的 Content-Length 同样不可信，置 0（未知大小）。
            let size = if is_html {
                0
            } else {
                response.content_length().map(|s| s as i64).unwrap_or(0)
            };
            (name, size)
        }
        _ => (String::new(), 0),
    }
}

#[cfg(test)]
mod tests {
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
}
