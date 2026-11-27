//! 下载请求的发送：引擎自己逐跳跟随重定向，每一跳按凭据作用域重建请求。
//!
//! reqwest 自动跟随重定向时，只要主机变化就无条件删除 `Cookie` / `Authorization`，
//! 并把 `Referer` 改写成上一跳 URL。扩展捕获的 Cookie 以普通请求头传入，于是
//! `www.tiktok.com/aweme/v1/play` → `v16-webapp-prime.tiktok.com` 这类同站跨子域跳转
//! 丢掉了浏览器本会携带的 `.tiktok.com` Cookie（目标回 403）；而同主机跳转时手动
//! Cookie 头又会挡住 cookie jar，302 响应里 `Set-Cookie` 下发的新值发不出去。
//!
//! 下载 client 因此一律 `redirect::Policy::none()`，由 [`HttpRequest::send`] 跟随：
//! - 每一跳按 [`crate::credential_scope`] 对锚点重新判定 Cookie / Authorization /
//!   Referer，按站点请求头（插件 `originHeaders`）按该跳 URL 重新匹配；没有原始
//!   Referer 时以上一跳 URL 作 Referer（与浏览器一致）；
//! - 捕获的 Cookie 只与**本条跳转链内**服务器下发的同站 Cookie 按名合并（新值优先）；
//!   jar 跨任务、跨刷新的旧值从不覆盖捕获值；
//! - 方法与请求体按 Fetch 规范改写：301 / 302 的 POST 与 303 的非 HEAD 改为无体 GET，
//!   307 / 308 原样重放；
//! - 超过 [`MAX_REDIRECTS`] 跳或跳往非 http(s) 时报错；`Location` 缺失或无法解析时
//!   把该 3xx 原样交给调用方。

use std::borrow::Cow;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use reqwest::cookie::Jar;
use reqwest::header::{COOKIE, HeaderValue, LOCATION, REFERER, SET_COOKIE};
use reqwest::{Client, ClientBuilder, Method, RequestBuilder, Response, StatusCode, Url};
use tokio::time::Instant;

use crate::credential_scope;
use crate::downloader::{DownloadError, RequestBodyDecoded, RequestSpec};
use crate::logger::log_info;

/// 单个请求最多跟随的重定向跳数（与此前 reqwest `Policy::limited(30)` 一致）。
pub(crate) const MAX_REDIRECTS: usize = 30;

/// 下载 client 的 cookie jar：严格校验证书的 client 共用一份（探测、分段、钉定节点与
/// 备选路径看到同一份服务器下发的 Cookie）；忽略证书错误的 client 另用一份，可被中间人
/// 篡改的连接与已校验连接之间不互通 Cookie。jar 只由 reqwest 在请求未带手动 Cookie 头时
/// 自动附加，[`HttpRequest`] 自己从不读它：jar 跨任务、跨进程生命周期，读出的旧值会盖掉
/// 用户新捕获或插件刷新后的 Cookie。
static VERIFIED_COOKIE_JAR: LazyLock<Arc<Jar>> = LazyLock::new(|| Arc::new(Jar::default()));
static INSECURE_COOKIE_JAR: LazyLock<Arc<Jar>> = LazyLock::new(|| Arc::new(Jar::default()));

/// 给下载 client 装上逐跳重定向所需的配置：关闭 reqwest 自动跟随、按证书策略挂 jar。
pub(crate) fn configure_download_client(
    builder: ClientBuilder,
    ignore_tls_errors: bool,
) -> ClientBuilder {
    let jar = if ignore_tls_errors {
        &INSECURE_COOKIE_JAR
    } else {
        &VERIFIED_COOKIE_JAR
    };
    builder
        .redirect(reqwest::redirect::Policy::none())
        .cookie_provider(Arc::clone(jar))
}

/// 一个待发送的下载请求。构造见 [`crate::downloader::build_request`]；`header` /
/// `timeout` 与 `reqwest::RequestBuilder` 同义，作用于每一跳。
pub(crate) struct HttpRequest<'s> {
    client: Client,
    url: String,
    method: Method,
    spec: &'s RequestSpec,
    /// 凭据锚点；`None` = 请求 URL 本身。
    anchor: Option<String>,
    headers: Vec<(String, String)>,
    timeout: Option<Duration>,
}

impl<'s> HttpRequest<'s> {
    pub(crate) fn new(client: &Client, url: &str, method: Method, spec: &'s RequestSpec) -> Self {
        Self {
            client: client.clone(),
            url: url.to_owned(),
            method,
            spec,
            anchor: None,
            headers: Vec::new(),
            timeout: None,
        }
    }

    /// 本请求专属的请求头（Range 等），每一跳都附加，覆盖同名的身份头。
    pub(crate) fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// 整条跳转链（含最终响应体读取）的总时限，与 `RequestBuilder::timeout` 同义。
    pub(crate) fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// 凭据锚点：Cookie / Authorization 属于发往该 URL 的请求。请求 URL 不是凭据的
    /// 原始归属时设置（HLS / DASH 分片锚定用户清单、分段直连已学到的重定向终点）。
    pub(crate) fn anchor(mut self, anchor: impl Into<String>) -> Self {
        self.anchor = Some(anchor.into());
        self
    }

    /// 发送并跟随重定向，返回最后一跳的响应（`Response::url` 为最终 URL）。
    /// 超过 [`MAX_REDIRECTS`] 跳或跳往非 http(s) 地址时报错；`Location` 缺失或无法解析
    /// 时把该 3xx 原样返回（与 reqwest 一致）。
    pub(crate) async fn send(self) -> Result<Response, DownloadError> {
        let Ok(mut url) = Url::parse(&self.url) else {
            // 交给 reqwest 产出与直接发送一致的 URL 构建错误。
            return Ok(self
                .client
                .request(self.method.clone(), self.url.as_str())
                .send()
                .await?);
        };
        let anchor = match &self.anchor {
            Some(anchor) => Url::parse(anchor).ok(),
            None => Some(url.clone()),
        };
        let deadline = self.timeout.map(|timeout| Instant::now() + timeout);
        let mut method = self.method.clone();
        let mut keeps_body = true;
        let mut previous: Option<Url> = None;
        // 本条跳转链里服务器下发的 Cookie（下发方 URL, `name=value`）：只在本链内覆盖捕获值。
        let mut learned: Vec<(Url, String)> = Vec::new();
        let mut hops = 0;
        loop {
            let hop = Hop {
                url: &url,
                method: &method,
                keeps_body,
                anchor: anchor.as_ref(),
                previous: previous.as_ref(),
                learned: &learned,
                deadline,
            };
            // 站点代理规则：命中与发送方出口不同的规则时换用规则 client。重定向由本循环
            // 逐跳跟随（下载 client 用 `Policy::none()`），所以每一跳都经过这里。
            let routed = match &self.spec.egress {
                Some(router) => router.client_for(&url).await?,
                None => None,
            };
            let client = routed.as_ref().unwrap_or(&self.client);
            let resp = self.hop(client, &hop).send().await?;
            let status = resp.status();
            if !is_redirect(status) {
                return Ok(resp);
            }
            let Some(next) = resp
                .headers()
                .get(LOCATION)
                .and_then(|location| redirect_target(&url, location))
            else {
                log_info!(
                    "[redirect] {} 的 {} 没有可跟随的 Location，停止跟随",
                    url,
                    status
                );
                return Ok(resp);
            };
            if !matches!(next.scheme(), "http" | "https") {
                return Err(DownloadError::Other(format!(
                    "redirect to unsupported scheme `{}`: {}",
                    next.scheme(),
                    next
                )));
            }
            if hops == MAX_REDIRECTS {
                return Err(DownloadError::Other(format!(
                    "too many redirects (> {MAX_REDIRECTS}) from {}",
                    self.url
                )));
            }
            hops += 1;
            learned.extend(set_cookie_pairs(&resp).map(|pair| (url.clone(), pair)));
            if rewrites_to_get(status, &method) {
                method = Method::GET;
                keeps_body = false;
            }
            drop(resp);
            previous = Some(std::mem::replace(&mut url, next));
        }
    }

    /// 测试用：首跳的完整请求（不发送）。
    #[cfg(test)]
    pub(crate) fn first_hop(&self) -> reqwest::Request {
        let url = Url::parse(&self.url).unwrap();
        let anchor = self
            .anchor
            .as_deref()
            .map_or_else(|| url.clone(), |anchor| Url::parse(anchor).unwrap());
        self.hop(
            &self.client,
            &Hop {
                url: &url,
                method: &self.method,
                keeps_body: true,
                anchor: Some(&anchor),
                previous: None,
                learned: &[],
                deadline: None,
            },
        )
        .build()
        .unwrap()
    }

    /// 构造一跳。
    fn hop(&self, client: &Client, hop: &Hop<'_>) -> RequestBuilder {
        let Hop {
            url,
            method,
            keeps_body,
            anchor,
            previous,
            learned,
            deadline,
        } = *hop;
        let spec = self.spec;
        let mut req = client.request(method.clone(), url.clone());
        let defaults = spec
            .site_request_headers
            .as_ref()
            .and_then(|rules| rules.headers_for(url));

        if let Some(defaults) = defaults {
            for (name, value) in defaults {
                let key = name.as_str();
                if name != REFERER
                    && !(spec.explicit_user_agent && key == "user-agent")
                    && (keeps_body || !is_body_header(key))
                    && !spec
                        .extra_headers
                        .keys()
                        .any(|k| k.eq_ignore_ascii_case(key))
                    && !self
                        .headers
                        .iter()
                        .any(|(k, _)| k.eq_ignore_ascii_case(key))
                {
                    req = req.header(name.clone(), value.clone());
                }
            }
        }

        let captured = header_value(spec, "cookie").unwrap_or(&spec.cookies).trim();
        if !captured.is_empty() && anchor.is_some_and(|a| credential_scope::cookies_apply(a, url)) {
            // 手动 Cookie 头会让 reqwest 跳过 jar：并入本链内下发、作用于本跳的 Cookie。
            let fresh = learned
                .iter()
                .filter(|(setter, _)| credential_scope::cookies_apply(setter, url))
                .map(|(_, pair)| pair.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            req = req.header(
                COOKIE,
                credential_scope::merge_cookies(captured, Some(&fresh)).as_ref(),
            );
        }

        let referer = header_value(spec, "referer").unwrap_or(&spec.referrer);
        if crate::downloader::is_valid_referrer(referer) {
            if credential_scope::referer_applies(referer, url) {
                req = req.header(REFERER, referer.trim());
            }
        } else if let Some(value) = defaults.and_then(|headers| headers.get(REFERER)) {
            if value
                .to_str()
                .is_ok_and(|value| credential_scope::referer_applies(value, url))
            {
                req = req.header(REFERER, value.clone());
            }
        } else if let Some(previous) = previous {
            // 无原始来源页时与浏览器 / 原 reqwest 一致：以上一跳 URL 作 Referer。
            let mut referer = previous.clone();
            referer.set_fragment(None);
            if referer.set_username("").is_ok()
                && referer.set_password(None).is_ok()
                && credential_scope::referer_applies(referer.as_str(), url)
            {
                req = req.header(REFERER, referer.as_str());
            }
        }

        let authorized = anchor.is_some_and(|a| credential_scope::authorization_applies(a, url));
        req = crate::downloader::apply_extra_headers_where(req, &spec.extra_headers, |name| {
            !(name.eq_ignore_ascii_case("cookie")
                || name.eq_ignore_ascii_case("referer")
                || (!authorized && credential_scope::is_authorization_header(name))
                || (!keeps_body && is_body_header(name)))
        });
        req = crate::downloader::apply_origin_headers(req, url.as_str(), &spec.origin_headers);

        if keeps_body
            && *method != Method::GET
            && *method != Method::HEAD
            && let Some(body) = &spec.body
        {
            req = attach_body(req, body);
        }
        for (name, value) in &self.headers {
            req = req.header(name.as_str(), value.as_str());
        }
        if let Some(deadline) = deadline {
            req = req.timeout(deadline.saturating_duration_since(Instant::now()));
        }
        req
    }
}

/// 一跳的上下文（[`HttpRequest::hop`] 的入参）。
#[derive(Clone, Copy)]
struct Hop<'a> {
    url: &'a Url,
    method: &'a Method,
    keeps_body: bool,
    anchor: Option<&'a Url>,
    /// 上一跳 URL：无原始 Referer 时作为 Referer。
    previous: Option<&'a Url>,
    /// 本链内服务器下发的 Cookie（下发方 URL, `name=value`）。
    learned: &'a [(Url, String)],
    deadline: Option<Instant>,
}

/// 响应 `Set-Cookie` 的 `name=value` 部分（属性丢弃；作用范围由下发方 URL 按站点判定）。
fn set_cookie_pairs(resp: &Response) -> impl Iterator<Item = String> + '_ {
    resp.headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .map(str::trim)
        .filter(|pair| {
            pair.split_once('=')
                .is_some_and(|(name, _)| !name.trim().is_empty())
        })
        .map(str::to_owned)
}

/// `extra_headers` 里名为 `name`（不分大小写）的值：扩展捕获的同名头覆盖 spec 字段。
fn header_value<'a>(spec: &'a RequestSpec, name: &str) -> Option<&'a str> {
    spec.extra_headers
        .iter()
        .find(|(key, value)| key.eq_ignore_ascii_case(name) && !value.trim().is_empty())
        .map(|(_, value)| value.as_str())
}

/// 改为无体 GET 后不再适用的请求体描述头（Fetch 规范 request-body-header name）。
fn is_body_header(name: &str) -> bool {
    [
        "content-type",
        "content-language",
        "content-location",
        "content-encoding",
    ]
    .iter()
    .any(|body| name.eq_ignore_ascii_case(body))
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// Fetch 规范 HTTP-redirect fetch 第 12 步：301 / 302 的 POST、303 的非 GET/HEAD 改为 GET。
fn rewrites_to_get(status: StatusCode, method: &Method) -> bool {
    match status.as_u16() {
        301 | 302 => *method == Method::POST,
        303 => *method != Method::GET && *method != Method::HEAD,
        _ => false,
    }
}

/// 解析 `Location`（相对地址按当前 URL 解析）；只跟随 http(s)。非 UTF-8 的原始字节
/// （GBK 等未编码的文件名）逐字节百分号编码后再解析。scheme 由调用方判定。
fn redirect_target(current: &Url, location: &HeaderValue) -> Option<Url> {
    let raw = location.as_bytes();
    let text = match std::str::from_utf8(raw) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(percent_encode_high_bytes(raw)),
    };
    current.join(text.trim()).ok()
}

fn percent_encode_high_bytes(raw: &[u8]) -> String {
    fn hex(nibble: u8) -> char {
        char::from(if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        })
    }
    let mut out = String::with_capacity(raw.len() * 3);
    for &byte in raw {
        if byte.is_ascii() {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(hex(byte >> 4));
            out.push(hex(byte & 0x0F));
        }
    }
    out
}

fn attach_body(req: RequestBuilder, body: &RequestBodyDecoded) -> RequestBuilder {
    match body {
        RequestBodyDecoded::Form(pairs) => req.form(pairs),
        RequestBodyDecoded::Urlencoded(raw) => req
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(raw.clone()),
        RequestBodyDecoded::Raw {
            bytes,
            content_type,
        } => {
            let req = match content_type {
                Some(ct) => req.header(reqwest::header::CONTENT_TYPE, ct),
                None => req,
            };
            req.body(bytes.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{MAX_REDIRECTS, configure_download_client};
    use crate::downloader::{RequestBodyDecoded, RequestSpec, build_request};

    /// 服务端看到的一次请求（头名小写）。
    #[derive(Clone, Debug)]
    struct Seen {
        host: String,
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: String,
    }

    impl Seen {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers.get(name).map(String::as_str)
        }
    }

    type Log = Arc<Mutex<Vec<Seen>>>;

    /// 每个连接只处理一个请求（`Connection: close`）。`route` 返回完整的响应报文。
    async fn serve(route: fn(&Seen, u16) -> String) -> (u16, Log) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Log = Arc::default();
        let seen_log = Arc::clone(&log);
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let log = Arc::clone(&seen_log);
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buf = [0u8; 4096];
                    let head_end = loop {
                        let n = stream.read(&mut buf).await.unwrap();
                        raw.extend_from_slice(&buf[..n]);
                        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i;
                        }
                    };
                    let head = String::from_utf8(raw[..head_end].to_vec()).unwrap();
                    let mut lines = head.split("\r\n");
                    let mut start = lines.next().unwrap().split(' ');
                    let method = start.next().unwrap().to_string();
                    let path = start.next().unwrap().to_string();
                    let headers: HashMap<String, String> = lines
                        .filter_map(|l| l.split_once(':'))
                        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                        .collect();
                    let len: usize = headers
                        .get("content-length")
                        .map_or(0, |v| v.parse().unwrap());
                    let mut body = raw[head_end + 4..].to_vec();
                    while body.len() < len {
                        let n = stream.read(&mut buf).await.unwrap();
                        body.extend_from_slice(&buf[..n]);
                    }
                    let seen = Seen {
                        host: headers["host"].split(':').next().unwrap().to_string(),
                        method,
                        path,
                        headers,
                        body: String::from_utf8(body).unwrap(),
                    };
                    let response = route(&seen, port);
                    log.lock().unwrap().push(seen);
                    stream.write_all(response.as_bytes()).await.unwrap();
                    stream.shutdown().await.unwrap();
                });
            }
        });
        (port, log)
    }

    #[tokio::test]
    async fn site_request_headers_fill_missing_values_without_crossing_origins()
    -> anyhow::Result<()> {
        use crate::site_request_headers::SiteRequestHeaders;

        fn route(seen: &Seen, port: u16) -> String {
            if seen.path == "/redirect" {
                redirect(
                    "302 Found",
                    &format!("http://cdn.rules.test:{port}/file"),
                    "",
                )
            } else {
                response("200 OK", "", "")
            }
        }

        let (port, log) = serve(route).await;
        let client = client(port, &["files.rules.test", "cdn.rules.test"]);
        let origin = format!("http://files.rules.test:{port}");
        let raw = serde_json::json!([{
            "origin": origin,
            "headers": {"Referer": "http://source.rules.test/page", "X-Mode": "default"}
        }]);
        let rules =
            Arc::new(SiteRequestHeaders::parse(&raw.to_string()).map_err(anyhow::Error::msg)?);
        let mut spec = RequestSpec {
            site_request_headers: Some(rules),
            ..RequestSpec::empty_get()
        };
        for path in ["/first", "/redirect"] {
            build_request(
                &client,
                &format!("{origin}{path}"),
                reqwest::Method::GET,
                &spec,
            )
            .send()
            .await?
            .error_for_status()?;
        }
        spec.referrer = "http://source.rules.test/explicit".to_owned();
        spec.extra_headers
            .insert("x-MODE".to_owned(), "explicit".to_owned());
        build_request(
            &client,
            &format!("{origin}/explicit"),
            reqwest::Method::HEAD,
            &spec,
        )
        .send()
        .await?
        .error_for_status()?;

        let seen = hops(&log);
        for hop in &seen[..2] {
            assert_eq!(hop.header("referer"), Some("http://source.rules.test/page"));
            assert_eq!(hop.header("x-mode"), Some("default"));
        }
        assert_eq!(seen[2].host, "cdn.rules.test");
        assert_eq!(
            seen[2].header("referer"),
            Some(format!("{origin}/redirect").as_str())
        );
        assert_eq!(seen[2].header("x-mode"), None);
        assert_eq!(
            seen[3].header("referer"),
            Some("http://source.rules.test/explicit")
        );
        assert_eq!(seen[3].header("x-mode"), Some("explicit"));
        Ok(())
    }

    fn response(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn redirect(status: &str, location: &str, extra: &str) -> String {
        response(status, &format!("Location: {location}\r\n{extra}"), "")
    }

    /// 解析到本机测试服务器的下载 client（与生产同样关闭自动跟随、挂共享 jar）。
    fn client(port: u16, hosts: &[&str]) -> reqwest::Client {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        hosts
            .iter()
            .fold(
                configure_download_client(reqwest::Client::builder().no_proxy(), false),
                |builder, host| builder.resolve(host, addr),
            )
            .build()
            .unwrap()
    }

    fn hops(log: &Log) -> Vec<Seen> {
        log.lock().unwrap().clone()
    }

    /// TikTok 形态：`www` 302 到同站 CDN 子域，CDN 只认浏览器的域 Cookie。Cookie 跟到
    /// 同站子域、Authorization 只留在源站、Referer 保持原值；跳出站点后两者都不带。
    #[tokio::test]
    async fn redirect_hops_carry_credentials_by_scope() {
        fn route(seen: &Seen, port: u16) -> String {
            match (seen.host.as_str(), seen.path.as_str()) {
                ("www.site.test", "/play") => redirect(
                    "302 Found",
                    &format!("http://cdn.site.test:{port}/video"),
                    "",
                ),
                ("www.site.test", "/away") => {
                    redirect("302 Found", &format!("http://evil.test:{port}/x"), "")
                }
                ("cdn.site.test", "/video")
                    if seen.header("cookie").is_some_and(|c| c.contains("tt=1")) =>
                {
                    response("200 OK", "", "media")
                }
                ("evil.test", "/x") => response("200 OK", "", "x"),
                _ => response("403 Forbidden", "", ""),
            }
        }
        let (port, log) = serve(route).await;
        let client = client(port, &["www.site.test", "cdn.site.test", "evil.test"]);
        let mut spec = RequestSpec {
            cookies: "tt=1".to_string(),
            referrer: "http://www.site.test/feed".to_string(),
            ..RequestSpec::empty_get()
        };
        spec.extra_headers
            .insert("Authorization".to_string(), "Bearer t".to_string());

        let play = format!("http://www.site.test:{port}/play");
        let resp = build_request(&client, &play, reqwest::Method::GET, &spec)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.url().host_str(), Some("cdn.site.test"));
        let seen = hops(&log);
        let cdn = &seen[1];
        assert_eq!(cdn.header("cookie"), Some("tt=1"));
        assert_eq!(cdn.header("authorization"), None);
        assert_eq!(cdn.header("referer"), Some("http://www.site.test/feed"));
        assert_eq!(seen[0].header("authorization"), Some("Bearer t"));

        let away = format!("http://www.site.test:{port}/away");
        let resp = build_request(&client, &away, reqwest::Method::GET, &spec)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let evil = hops(&log).pop().unwrap();
        assert_eq!(evil.host, "evil.test");
        assert_eq!(evil.header("cookie"), None);
        assert_eq!(evil.header("authorization"), None);
    }

    /// 302 里 `Set-Cookie` 下发的值要和捕获的 Cookie 一起发往下一跳（同名以新值为准）。
    #[tokio::test]
    async fn cookies_set_during_redirect_merge_with_captured() {
        fn route(seen: &Seen, _: u16) -> String {
            let cookie = seen.header("cookie").unwrap_or("");
            match seen.path.as_str() {
                "/gate" => redirect("302 Found", "/file", "Set-Cookie: ticket=new; Path=/\r\n"),
                "/file" if cookie.contains("sid=1") && cookie.contains("ticket=new") => {
                    response("200 OK", "", "data")
                }
                _ => response("403 Forbidden", "", ""),
            }
        }
        let (port, log) = serve(route).await;
        let client = client(port, &["jar.merge.test"]);
        let spec = RequestSpec {
            cookies: "sid=1; ticket=stale".to_string(),
            ..RequestSpec::empty_get()
        };
        let gate = format!("http://jar.merge.test:{port}/gate");
        let resp = build_request(&client, &gate, reqwest::Method::GET, &spec)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(hops(&log)[1].header("cookie"), Some("sid=1; ticket=new"));
    }

    /// 302 把 POST 改为无体 GET（连同请求体描述头），307 原样重放方法与请求体。
    #[tokio::test]
    async fn redirect_status_decides_method_and_body() {
        fn route(seen: &Seen, _: u16) -> String {
            match seen.path.as_str() {
                "/found" => redirect("302 Found", "/landing", ""),
                "/temporary" => redirect("307 Temporary Redirect", "/landing", ""),
                _ => response("200 OK", "", "ok"),
            }
        }
        let (port, log) = serve(route).await;
        let client = client(port, &["form.method.test"]);
        let spec = RequestSpec {
            method: reqwest::Method::POST,
            body: Some(RequestBodyDecoded::Urlencoded("k=v".to_string())),
            ..RequestSpec::empty_get()
        };
        for path in ["/found", "/temporary"] {
            let url = format!("http://form.method.test:{port}{path}");
            build_request(&client, &url, reqwest::Method::POST, &spec)
                .send()
                .await
                .unwrap();
        }
        let seen = hops(&log);
        let after_302 = &seen[1];
        assert_eq!(after_302.method, "GET");
        assert_eq!(after_302.body, "");
        assert_eq!(after_302.header("content-type"), None);
        let after_307 = &seen[3];
        assert_eq!(after_307.method, "POST");
        assert_eq!(after_307.body, "k=v");
    }

    /// 重定向环在上限处、跳往非 http(s) 地址时都报错，绝不把 3xx 交给调用方当成数据响应。
    #[tokio::test]
    async fn unfollowable_redirects_are_errors() {
        fn route(seen: &Seen, _: u16) -> String {
            match seen.path.as_str() {
                "/ftp" => redirect("302 Found", "ftp://mirror.example/f.iso", ""),
                _ => redirect("302 Found", "/loop", ""),
            }
        }
        let (port, log) = serve(route).await;
        let client = client(port, &["loop.limit.test"]);
        let spec = RequestSpec::empty_get();
        let send = |path: &str| {
            let url = format!("http://loop.limit.test:{port}{path}");
            build_request(&client, &url, reqwest::Method::GET, &spec).send()
        };
        let looped = send("/loop").await.unwrap_err();
        assert!(
            looped.to_string().contains("too many redirects"),
            "{looped}"
        );
        assert_eq!(hops(&log).len(), MAX_REDIRECTS + 1);
        let ftp = send("/ftp").await.unwrap_err();
        assert!(ftp.to_string().contains("unsupported scheme"), "{ftp}");
    }

    /// 跨请求、跨任务时捕获（或插件刷新）的 Cookie 优先：先前响应下发的同名旧值不得盖掉它；
    /// 没有原始 Referer 时重定向后一跳以上一跳 URL 作 Referer。
    #[tokio::test]
    async fn captured_cookies_win_across_requests_and_referer_falls_back() {
        fn route(seen: &Seen, _: u16) -> String {
            match seen.path.as_str() {
                "/login" => response("200 OK", "Set-Cookie: sid=old; Path=/\r\n", ""),
                "/dl" => redirect("302 Found", "/file", ""),
                _ => response("200 OK", "", "ok"),
            }
        }
        let (port, log) = serve(route).await;
        let client = client(port, &["stale.jar.test"]);
        let base = format!("http://stale.jar.test:{port}");
        let first = RequestSpec {
            cookies: "sid=first".to_string(),
            ..RequestSpec::empty_get()
        };
        build_request(
            &client,
            &format!("{base}/login"),
            reqwest::Method::GET,
            &first,
        )
        .send()
        .await
        .unwrap();
        let fresh = RequestSpec {
            cookies: "sid=fresh".to_string(),
            ..RequestSpec::empty_get()
        };
        build_request(&client, &format!("{base}/dl"), reqwest::Method::GET, &fresh)
            .send()
            .await
            .unwrap();
        let seen = hops(&log);
        let file = &seen[2];
        assert_eq!(file.header("cookie"), Some("sid=fresh"));
        assert_eq!(file.header("referer"), Some(format!("{base}/dl").as_str()));
    }
}
