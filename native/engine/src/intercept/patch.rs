//! `RequestPatch`：`prepare` 阶段与 resolver `request` 字段共用的请求改写。
//!
//! 所有规则先作用在副本上，全部通过后一次性提交；任一规则失败目标保持原样。

use std::collections::HashMap;

use serde::Deserialize;

use super::ctx::{RequestCtx, RunMeta};
use super::{ProtocolKind, protocol_kind};
use crate::downloader::{
    BLOCKED_REQUEST_HEADERS, CapturedRequestBody, OriginHeaders, RequestSpec, decode_captured_body,
    encode_decoded_body, is_valid_referrer,
};

/// 改写后 URL 的长度上限。
const MAX_URL_LEN: usize = 8 * 1024;
/// 按站点请求头规则条数上限。
const MAX_ORIGIN_RULES: usize = 32;
/// 连接上限的取值上界（与显式线程数上限一致）。
const MAX_CONNECTIONS: i32 = 512;
/// 单个 HTTP 请求 Range 跨度上限（`maxRangeBytes`）的取值下界：更小的窗口会把请求数放大到
/// 不合理的程度，也远低于任何已知 CDN 的单请求限制。
const MIN_RANGE_BYTES: i64 = 64 * 1024;

const FTP_HEADER_REJECT: &str = "FTP 任务不支持请求头类改写";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RequestPatch {
    pub url: Option<String>,
    pub method: Option<String>,
    pub body: Option<CapturedRequestBody>,
    pub clear_body: bool,
    pub cookies: Option<String>,
    pub referrer: Option<String>,
    pub user_agent: Option<String>,
    pub set_headers: Option<HashMap<String, String>>,
    pub remove_headers: Option<Vec<String>>,
    pub origin_headers: Option<Vec<OriginHeaders>>,
    pub segments: Option<i32>,
    pub max_connections: Option<i32>,
    /// 仅 HTTP：每个数据请求的 Range 跨度最多 N 字节（≥ 64 KiB）。用于只接受小区间的
    /// 直链（如网盘 CDN）：分段 worker 在段内逐窗口请求，单流 / 回退单流也只发有界 Range。
    pub max_range_bytes: Option<i64>,
    pub file_name: Option<String>,
    /// 一次性 / 防探测凭据：跳过探测，按 hint 路径保守启动（同 resolver `ephemeral`）。
    pub ephemeral: Option<bool>,
    /// 已知大小（仅与 `ephemeral: true` 同用）。
    pub total_bytes: Option<i64>,
    /// 担保服务端支持 Range（仅与 `ephemeral: true` 同用）。
    pub range_supported: Option<bool>,
    /// resolver 旧字段 `extraHeaders`：整表替换请求头。不来自线协议。
    #[serde(skip)]
    pub replace_headers: Option<HashMap<String, String>>,
}

/// 补丁作用的目标（本次运行的请求与并发参数）。
pub struct RequestTarget<'a> {
    pub url: &'a mut String,
    pub spec: &'a mut RequestSpec,
    /// 仅首跑（有启动序幕、尚未定名）时 `Some`。
    pub file_name: Option<&'a mut String>,
    pub segment_count: Option<&'a mut i32>,
    pub connection_cap: Option<&'a mut i32>,
    /// 下载器的大小 hint（0 = 探测；>0 / -1 = 跳过探测）；元数据探测等场景为 `None`。
    pub hint_file_size: Option<&'a mut i64>,
    /// Range 能力是否已验证（hint 路径据此决定是否保守单流起步）。
    pub range_verified: Option<&'a mut bool>,
    pub is_resume: bool,
    pub kind: ProtocolKind,
    pub has_audio_track: bool,
}

/// 补丁应用结果：`changed` 为改动的字段标签（去重、按规则顺序）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PatchOutcome {
    pub changed: Vec<&'static str>,
    pub ignored_file_name: bool,
    /// 补丁声明了 `ephemeral: true`（调用方据此避免任何探测请求）。
    pub ephemeral: bool,
}

impl PatchOutcome {
    fn mark(&mut self, label: &'static str) {
        if !self.changed.contains(&label) {
            self.changed.push(label);
        }
    }
}

fn remove_header_ci(headers: &mut HashMap<String, String>, name: &str) {
    headers.retain(|k, _| !k.eq_ignore_ascii_case(name));
}

fn validate_header(name: &str, value: &str) -> Result<(), String> {
    let parsed = reqwest::header::HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| format!("请求头 '{name}' 非法"))?;
    reqwest::header::HeaderValue::from_str(value).map_err(|_| format!("请求头 '{name}' 非法"))?;
    if BLOCKED_REQUEST_HEADERS.contains(&parsed.as_str()) {
        return Err(format!("请求头 '{name}' 由引擎管理，不可设置"));
    }
    Ok(())
}

/// 文件名合法性：非空，不含路径分隔符、`..` 与控制字符。
pub(crate) fn valid_file_name(name: &str) -> bool {
    !name.trim().is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.chars().any(char::is_control)
}

/// 应用补丁。失败时返回固定文案，目标不变。
pub fn apply_request_patch(
    target: &mut RequestTarget<'_>,
    patch: &RequestPatch,
) -> Result<PatchOutcome, String> {
    let kind = target.kind;
    let is_ftp = kind == ProtocolKind::Ftp;
    let mut url = target.url.clone();
    let mut spec = target.spec.clone();
    let mut file_name = target.file_name.as_deref().cloned();
    let mut segments = target.segment_count.as_deref().copied();
    let mut cap = target.connection_cap.as_deref().copied();
    let mut out = PatchOutcome::default();

    // 1. resolver extraHeaders：整表替换。
    if let Some(headers) = &patch.replace_headers {
        spec.extra_headers = headers.clone();
        if headers.keys().any(|k| k.eq_ignore_ascii_case("user-agent")) {
            spec.pins.user_agent = true;
        }
        if headers.keys().any(|k| k.eq_ignore_ascii_case("referer")) {
            spec.pins.referrer = true;
        }
        out.mark("请求头");
    }

    // 2. url：同主机、同协议类别。
    if let Some(next) = &patch.url {
        if next.len() > MAX_URL_LEN {
            return Err("url 超过 8 KiB".to_string());
        }
        let parsed = reqwest::Url::parse(next).map_err(|_| "url 非法".to_string())?;
        let current_host = reqwest::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
        if parsed.host_str().map(str::to_string) != current_host {
            return Err("url 只能在同一主机内改写（跨站改写请使用 resolver）".to_string());
        }
        if protocol_kind(next, target.has_audio_track) != kind {
            return Err("url 不能改变下载协议类别".to_string());
        }
        url = next.clone();
        out.mark("链接");
    }

    // 3. method。
    if let Some(method) = &patch.method {
        if kind != ProtocolKind::Http {
            return Err("仅 HTTP 任务可改写 method".to_string());
        }
        let upper = method.trim().to_ascii_uppercase();
        let parsed =
            reqwest::Method::from_bytes(upper.as_bytes()).map_err(|_| "method 非法".to_string())?;
        if parsed == reqwest::Method::OPTIONS {
            return Err("method 不能为 OPTIONS".to_string());
        }
        spec.method = parsed;
        out.mark("请求方法");
    }

    // 4. clearBody / body。
    if patch.clear_body || patch.body.is_some() {
        if kind != ProtocolKind::Http {
            return Err("仅 HTTP 任务可改写请求体".to_string());
        }
        spec.body = if patch.clear_body {
            None
        } else if let Some(body) = &patch.body {
            Some(decode_captured_body(body.clone())?)
        } else {
            None
        };
        out.mark("请求体");
    }

    // 5. cookies。
    if let Some(cookies) = &patch.cookies {
        if is_ftp {
            return Err(FTP_HEADER_REJECT.to_string());
        }
        spec.cookies = cookies.clone();
        remove_header_ci(&mut spec.extra_headers, "cookie");
        out.mark("Cookie");
    }

    // 6. referrer。
    if let Some(referrer) = &patch.referrer {
        if is_ftp {
            return Err(FTP_HEADER_REJECT.to_string());
        }
        if !referrer.is_empty() && !is_valid_referrer(referrer) {
            return Err("referrer 必须是 http(s) 链接".to_string());
        }
        spec.referrer = referrer.clone();
        remove_header_ci(&mut spec.extra_headers, "referer");
        spec.pins.referrer = true;
        out.mark("Referer");
    }

    // 7. userAgent。
    if let Some(ua) = &patch.user_agent {
        if is_ftp {
            return Err(FTP_HEADER_REJECT.to_string());
        }
        if ua.is_empty()
            || ua.chars().any(char::is_control)
            || reqwest::header::HeaderValue::from_str(ua).is_err()
        {
            return Err("userAgent 非法".to_string());
        }
        remove_header_ci(&mut spec.extra_headers, "user-agent");
        spec.extra_headers
            .insert("User-Agent".to_string(), ua.clone());
        spec.pins.user_agent = true;
        out.mark("User-Agent");
    }

    // 8. removeHeaders。
    if let Some(names) = &patch.remove_headers {
        if is_ftp {
            return Err(FTP_HEADER_REJECT.to_string());
        }
        for name in names {
            remove_header_ci(&mut spec.extra_headers, name);
            if name.eq_ignore_ascii_case("cookie") {
                spec.cookies.clear();
            } else if name.eq_ignore_ascii_case("referer") {
                spec.referrer.clear();
                spec.pins.referrer = true;
            } else if name.eq_ignore_ascii_case("user-agent") {
                spec.pins.user_agent = true;
            }
        }
        out.mark("请求头");
    }

    // 9. setHeaders。
    if let Some(headers) = &patch.set_headers {
        if is_ftp {
            return Err(FTP_HEADER_REJECT.to_string());
        }
        for (name, value) in headers {
            validate_header(name, value)?;
        }
        for (name, value) in headers {
            remove_header_ci(&mut spec.extra_headers, name);
            spec.extra_headers.insert(name.clone(), value.clone());
            if name.eq_ignore_ascii_case("user-agent") {
                spec.pins.user_agent = true;
            } else if name.eq_ignore_ascii_case("referer") {
                spec.pins.referrer = true;
            }
        }
        out.mark("请求头");
    }

    // 10. originHeaders。
    if let Some(rules) = &patch.origin_headers {
        if is_ftp {
            return Err(FTP_HEADER_REJECT.to_string());
        }
        if rules.len() > MAX_ORIGIN_RULES {
            return Err("originHeaders 最多 32 条".to_string());
        }
        for rule in rules {
            let origin = crate::auth::normalize_site(&rule.origin)
                .ok_or_else(|| format!("originHeaders.origin '{}' 非法", rule.origin))?;
            for (name, value) in &rule.headers {
                validate_header(name, value)?;
            }
            let idx = match spec.origin_headers.iter().position(|r| r.origin == origin) {
                Some(i) => i,
                None => {
                    spec.origin_headers.push(OriginHeaders {
                        origin,
                        headers: HashMap::new(),
                    });
                    spec.origin_headers.len() - 1
                }
            };
            let merged = &mut spec.origin_headers[idx].headers;
            for (name, value) in &rule.headers {
                remove_header_ci(merged, name);
                merged.insert(name.clone(), value.clone());
            }
        }
        out.mark("按站点请求头");
    }

    // 11. segments。
    if let Some(n) = patch.segments {
        if !(0..=crate::segment_coordinator::MAX_SEGMENTS).contains(&n) {
            return Err("segments 超出 0..=512".to_string());
        }
        if let Some(slot) = segments.as_mut() {
            *slot = n;
            out.mark("线程数");
        }
    }

    // 12. maxConnections。
    if let Some(n) = patch.max_connections {
        if !(1..=MAX_CONNECTIONS).contains(&n) {
            return Err("maxConnections 超出 1..=512".to_string());
        }
        if let Some(slot) = cap.as_mut() {
            *slot = n;
            out.mark("连接上限");
        }
    }

    // 12b. maxRangeBytes。
    if let Some(n) = patch.max_range_bytes {
        if kind != ProtocolKind::Http {
            return Err("仅 HTTP 任务可设置 maxRangeBytes".to_string());
        }
        if n < MIN_RANGE_BYTES {
            return Err(format!(
                "maxRangeBytes 不能小于 {MIN_RANGE_BYTES}（64 KiB）"
            ));
        }
        spec.max_range_bytes = Some(n);
        out.mark("Range 跨度上限");
    }

    // 13. fileName。
    if let Some(name) = &patch.file_name {
        if !valid_file_name(name) {
            return Err("fileName 非法".to_string());
        }
        match file_name.as_mut() {
            Some(slot) => {
                *slot = name.clone();
                out.mark("文件名");
            }
            None => out.ignored_file_name = true,
        }
    }

    // 14. ephemeral / totalBytes / rangeSupported。
    let mut hint = target.hint_file_size.as_deref().copied();
    let mut verified = target.range_verified.as_deref().copied();
    let ephemeral = patch.ephemeral == Some(true);
    if !ephemeral && (patch.total_bytes.is_some() || patch.range_supported.is_some()) {
        return Err("totalBytes / rangeSupported 只能与 ephemeral: true 同用".to_string());
    }
    if ephemeral {
        if kind != ProtocolKind::Http {
            return Err("仅 HTTP 任务可声明 ephemeral".to_string());
        }
        if patch.total_bytes.is_some_and(|n| n <= 0) {
            return Err("totalBytes 必须大于 0".to_string());
        }
        out.ephemeral = true;
        if let Some(slot) = hint.as_mut() {
            *slot = match patch.total_bytes {
                Some(n) => n,
                None if *slot != 0 => *slot,
                None => -1,
            };
            out.mark("跳过探测");
        }
        if let Some(slot) = verified.as_mut() {
            let supported = patch.range_supported.unwrap_or(false);
            // 首跑：未担保即保守单流（同 resolver hint）；续传：保留已有 Range 证据。
            *slot = if target.is_resume {
                *slot || supported
            } else {
                supported
            };
        }
    }

    // 全部通过：一次性提交。
    *target.url = url;
    *target.spec = spec;
    if let (Some(slot), Some(v)) = (target.file_name.as_deref_mut(), file_name) {
        *slot = v;
    }
    if let (Some(slot), Some(v)) = (target.segment_count.as_deref_mut(), segments) {
        *slot = v;
    }
    if let (Some(slot), Some(v)) = (target.connection_cap.as_deref_mut(), cap) {
        *slot = v;
    }
    if let (Some(slot), Some(v)) = (target.hint_file_size.as_deref_mut(), hint) {
        *slot = v;
    }
    if let (Some(slot), Some(v)) = (target.range_verified.as_deref_mut(), verified) {
        *slot = v;
    }
    Ok(out)
}

/// `refresh` 阶段只允许换发凭据：拒绝请求方法、请求体、并发、文件名与探测类字段。
/// 非 HTTP 任务的 `url` 也不可在运行中改写（分片地址由清单决定）。
pub fn check_refresh_patch(patch: &RequestPatch, kind: ProtocolKind) -> Result<(), String> {
    let forbidden = [
        ("method", patch.method.is_some()),
        ("body", patch.body.is_some() || patch.clear_body),
        ("segments", patch.segments.is_some()),
        ("maxConnections", patch.max_connections.is_some()),
        ("maxRangeBytes", patch.max_range_bytes.is_some()),
        ("fileName", patch.file_name.is_some()),
        (
            "ephemeral",
            patch.ephemeral.is_some()
                || patch.total_bytes.is_some()
                || patch.range_supported.is_some(),
        ),
    ];
    if let Some((name, _)) = forbidden.iter().find(|(_, set)| *set) {
        return Err(format!("refresh 阶段不能改写 {name}"));
    }
    if patch.url.is_some() && kind != ProtocolKind::Http {
        return Err("仅 HTTP 任务的 refresh 可改写 url".to_string());
    }
    Ok(())
}

/// 当前请求的只读快照（插件上下文 `request`）。
pub fn request_snapshot(url: &str, spec: &RequestSpec, meta: &RunMeta) -> RequestCtx {
    let user_agent = spec
        .extra_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
        .map(|(_, v)| v.clone())
        .or_else(|| {
            spec.site_user_agent_rules.as_ref()?;
            let url = reqwest::Url::parse(url).ok()?;
            let ua = spec.site_user_agent(&url)?;
            std::str::from_utf8(ua.as_bytes()).ok().map(str::to_owned)
        })
        .unwrap_or_else(|| meta.user_agent.clone());
    RequestCtx {
        url: url.to_string(),
        method: spec.method.as_str().to_string(),
        cookies: spec.cookies.clone(),
        referrer: spec.referrer.clone(),
        user_agent,
        headers: spec.extra_headers.clone(),
        body: spec.body.as_ref().map(encode_decoded_body),
        origin_headers: spec.origin_headers.clone(),
        proxy_url: meta.proxy_url.clone(),
        ignore_tls_errors: meta.ignore_tls_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        url: String,
        spec: RequestSpec,
        file_name: String,
        segments: i32,
        cap: i32,
        hint: i64,
        range_verified: bool,
    }

    impl Fixture {
        fn new(url: &str) -> Self {
            Self {
                url: url.to_string(),
                spec: RequestSpec::default(),
                file_name: "a.bin".to_string(),
                segments: 1,
                cap: 0,
                hint: 0,
                range_verified: true,
            }
        }

        fn apply(
            &mut self,
            kind: ProtocolKind,
            started: bool,
            patch: &RequestPatch,
        ) -> Result<PatchOutcome, String> {
            let mut target = RequestTarget {
                url: &mut self.url,
                spec: &mut self.spec,
                file_name: if started {
                    None
                } else {
                    Some(&mut self.file_name)
                },
                segment_count: Some(&mut self.segments),
                connection_cap: Some(&mut self.cap),
                hint_file_size: Some(&mut self.hint),
                range_verified: Some(&mut self.range_verified),
                is_resume: started,
                kind,
                has_audio_track: false,
            };
            apply_request_patch(&mut target, patch)
        }
    }

    #[test]
    fn ephemeral_switches_fresh_run_to_conservative_hint_path() {
        let mut f = Fixture::new("https://example.com/a.bin");
        let patch = RequestPatch {
            ephemeral: Some(true),
            ..Default::default()
        };
        let out = f.apply(ProtocolKind::Http, false, &patch).expect("patch");
        assert!(out.ephemeral);
        assert_eq!((f.hint, f.range_verified), (-1, false));

        let mut sized = Fixture::new("https://example.com/a.bin");
        let patch = RequestPatch {
            ephemeral: Some(true),
            total_bytes: Some(4096),
            range_supported: Some(true),
            ..Default::default()
        };
        sized
            .apply(ProtocolKind::Http, false, &patch)
            .expect("patch");
        assert_eq!((sized.hint, sized.range_verified), (4096, true));
    }

    #[test]
    fn ephemeral_resume_keeps_existing_range_evidence() {
        let mut f = Fixture::new("https://example.com/a.bin");
        f.hint = 2048;
        let patch = RequestPatch {
            ephemeral: Some(true),
            ..Default::default()
        };
        f.apply(ProtocolKind::Http, true, &patch).expect("patch");
        assert_eq!((f.hint, f.range_verified), (2048, true));
    }

    #[test]
    fn ephemeral_rejected_outside_http_and_size_requires_ephemeral() {
        let mut f = Fixture::new("https://example.com/v.m3u8");
        let patch = RequestPatch {
            ephemeral: Some(true),
            ..Default::default()
        };
        assert_eq!(
            f.apply(ProtocolKind::Hls, false, &patch),
            Err("仅 HTTP 任务可声明 ephemeral".to_string())
        );
        let mut http = Fixture::new("https://example.com/a.bin");
        let patch = RequestPatch {
            total_bytes: Some(10),
            ..Default::default()
        };
        assert_eq!(
            http.apply(ProtocolKind::Http, false, &patch),
            Err("totalBytes / rangeSupported 只能与 ephemeral: true 同用".to_string())
        );
        assert_eq!(http.hint, 0);
    }

    #[test]
    fn refresh_patch_allows_only_credentials() {
        let creds = RequestPatch {
            cookies: Some("a=1".to_string()),
            set_headers: Some(HashMap::from([("X-Token".to_string(), "t".to_string())])),
            url: Some("https://example.com/a.bin?sig=2".to_string()),
            ..Default::default()
        };
        assert_eq!(check_refresh_patch(&creds, ProtocolKind::Http), Ok(()));
        assert_eq!(
            check_refresh_patch(&creds, ProtocolKind::Hls),
            Err("仅 HTTP 任务的 refresh 可改写 url".to_string())
        );
        let segments = RequestPatch {
            segments: Some(2),
            ..Default::default()
        };
        assert_eq!(
            check_refresh_patch(&segments, ProtocolKind::Http),
            Err("refresh 阶段不能改写 segments".to_string())
        );
    }

    #[test]
    fn patch_user_agent_pins_and_replaces_case_insensitively() {
        let mut f = Fixture::new("https://example.com/a.bin");
        f.spec
            .extra_headers
            .insert("user-agent".to_string(), "a".to_string());
        let patch = RequestPatch {
            user_agent: Some("B".to_string()),
            ..Default::default()
        };
        let out = f.apply(ProtocolKind::Http, false, &patch).expect("patch");
        assert_eq!(out.changed, vec!["User-Agent"]);
        assert_eq!(f.spec.extra_headers.len(), 1);
        assert_eq!(
            f.spec.extra_headers.get("User-Agent").map(String::as_str),
            Some("B")
        );
        assert!(f.spec.pins.user_agent);
        assert!(!f.spec.pins.referrer);
    }

    #[test]
    fn patch_rejects_engine_managed_headers() {
        let mut f = Fixture::new("https://example.com/a.bin");
        let patch = RequestPatch {
            set_headers: Some(HashMap::from([(
                "Range".to_string(),
                "bytes=0-".to_string(),
            )])),
            ..Default::default()
        };
        let err = f
            .apply(ProtocolKind::Http, false, &patch)
            .expect_err("range is engine managed");
        assert_eq!(err, "请求头 'Range' 由引擎管理，不可设置");
        assert!(f.spec.extra_headers.is_empty());
    }

    #[test]
    fn patch_url_keeps_host_and_protocol() {
        let mut f = Fixture::new("https://example.com/v.m3u8");
        let cross_host = RequestPatch {
            url: Some("https://cdn.example.com/v.m3u8".to_string()),
            ..Default::default()
        };
        assert_eq!(
            f.apply(ProtocolKind::Hls, false, &cross_host),
            Err("url 只能在同一主机内改写（跨站改写请使用 resolver）".to_string())
        );
        let cross_kind = RequestPatch {
            url: Some("https://example.com/v.mp4".to_string()),
            ..Default::default()
        };
        assert_eq!(
            f.apply(ProtocolKind::Hls, false, &cross_kind),
            Err("url 不能改变下载协议类别".to_string())
        );
        assert_eq!(f.url, "https://example.com/v.m3u8");

        let same = RequestPatch {
            url: Some("https://example.com/v2.m3u8?t=1".to_string()),
            ..Default::default()
        };
        f.apply(ProtocolKind::Hls, false, &same).expect("same host");
        assert_eq!(f.url, "https://example.com/v2.m3u8?t=1");
    }

    #[test]
    fn patch_method_body_only_for_http() {
        let mut f = Fixture::new("https://example.com/v.m3u8");
        let patch = RequestPatch {
            method: Some("post".to_string()),
            ..Default::default()
        };
        assert_eq!(
            f.apply(ProtocolKind::Hls, false, &patch),
            Err("仅 HTTP 任务可改写 method".to_string())
        );
        let mut http = Fixture::new("https://example.com/a.bin");
        let patch = RequestPatch {
            method: Some(" post ".to_string()),
            body: Some(CapturedRequestBody::Urlencoded {
                raw: "k=v".to_string(),
            }),
            ..Default::default()
        };
        http.apply(ProtocolKind::Http, false, &patch).expect("http");
        assert_eq!(http.spec.method, reqwest::Method::POST);
        assert!(http.spec.body.is_some());
    }

    #[test]
    fn patch_file_name_ignored_after_start() {
        let mut f = Fixture::new("https://example.com/a.bin");
        let patch = RequestPatch {
            file_name: Some("b.bin".to_string()),
            ..Default::default()
        };
        let out = f.apply(ProtocolKind::Http, true, &patch).expect("patch");
        assert!(out.ignored_file_name);
        assert!(out.changed.is_empty());
        assert_eq!(f.file_name, "a.bin");
    }

    #[test]
    fn patch_is_atomic_on_late_error() {
        let mut f = Fixture::new("https://example.com/a.bin");
        let patch = RequestPatch {
            user_agent: Some("Valid/1".to_string()),
            segments: Some(4),
            max_connections: Some(0),
            ..Default::default()
        };
        assert_eq!(
            f.apply(ProtocolKind::Http, false, &patch),
            Err("maxConnections 超出 1..=512".to_string())
        );
        assert!(f.spec.extra_headers.is_empty());
        assert!(!f.spec.pins.user_agent);
        assert_eq!(f.segments, 1);
    }

    #[test]
    fn patch_origin_headers_merge_per_site() {
        let mut f = Fixture::new("https://example.com/a.bin");
        let rule = |v: &str| OriginHeaders {
            origin: "https://Example.com/".to_string(),
            headers: HashMap::from([("X-Token".to_string(), v.to_string())]),
        };
        let first = RequestPatch {
            origin_headers: Some(vec![rule("1")]),
            ..Default::default()
        };
        f.apply(ProtocolKind::Http, false, &first).expect("first");
        let second = RequestPatch {
            origin_headers: Some(vec![rule("2")]),
            ..Default::default()
        };
        f.apply(ProtocolKind::Http, false, &second).expect("second");
        assert_eq!(f.spec.origin_headers.len(), 1);
        assert_eq!(
            f.spec.origin_headers[0]
                .headers
                .get("X-Token")
                .map(String::as_str),
            Some("2")
        );
        let ftp = RequestPatch {
            origin_headers: Some(vec![rule("3")]),
            ..Default::default()
        };
        assert_eq!(
            f.apply(ProtocolKind::Ftp, false, &ftp),
            Err("FTP 任务不支持请求头类改写".to_string())
        );
    }

    #[test]
    fn max_range_bytes_parses_from_wire_and_sets_spec() {
        let patch: RequestPatch =
            serde_json::from_str(r#"{"maxRangeBytes": 5242880}"#).expect("camelCase wire field");
        assert_eq!(patch.max_range_bytes, Some(5_242_880));

        let mut f = Fixture::new("https://example.com/a.bin");
        let out = f.apply(ProtocolKind::Http, false, &patch).expect("patch");
        assert_eq!(out.changed, vec!["Range 跨度上限"]);
        assert_eq!(f.spec.max_range_bytes, Some(5_242_880));
        assert_eq!(f.spec.range_window_cap(), Some(5_242_880));

        // 不声明时不改动：行为零变化。
        let mut untouched = Fixture::new("https://example.com/a.bin");
        untouched
            .apply(ProtocolKind::Http, false, &RequestPatch::default())
            .expect("empty patch");
        assert_eq!(untouched.spec.max_range_bytes, None);
        assert_eq!(untouched.spec.range_window_cap(), None);
    }

    #[test]
    fn max_range_bytes_rejects_small_values_and_non_http() {
        let too_small = format!("maxRangeBytes 不能小于 {MIN_RANGE_BYTES}（64 KiB）");
        for n in [i64::MIN, -1, 0, 1, MIN_RANGE_BYTES - 1] {
            let mut f = Fixture::new("https://example.com/a.bin");
            let patch = RequestPatch {
                max_range_bytes: Some(n),
                ..Default::default()
            };
            assert_eq!(
                f.apply(ProtocolKind::Http, false, &patch),
                Err(too_small.clone()),
                "{n}"
            );
            assert_eq!(f.spec.max_range_bytes, None);
        }
        let mut min = Fixture::new("https://example.com/a.bin");
        let patch = RequestPatch {
            max_range_bytes: Some(MIN_RANGE_BYTES),
            ..Default::default()
        };
        min.apply(ProtocolKind::Http, false, &patch)
            .expect("minimum accepted");
        assert_eq!(min.spec.max_range_bytes, Some(MIN_RANGE_BYTES));

        for (kind, url) in [
            (ProtocolKind::Ftp, "ftp://example.com/a.bin"),
            (ProtocolKind::Hls, "https://example.com/v.m3u8"),
        ] {
            let mut f = Fixture::new(url);
            let patch = RequestPatch {
                max_range_bytes: Some(1 << 20),
                ..Default::default()
            };
            assert_eq!(
                f.apply(kind, false, &patch),
                Err("仅 HTTP 任务可设置 maxRangeBytes".to_string())
            );
            assert_eq!(f.spec.max_range_bytes, None);
        }
    }

    #[test]
    fn max_range_bytes_is_atomic_with_other_fields_and_not_refreshable() {
        let mut f = Fixture::new("https://example.com/a.bin");
        let patch = RequestPatch {
            user_agent: Some("Valid/1".to_string()),
            max_range_bytes: Some(1),
            ..Default::default()
        };
        assert!(f.apply(ProtocolKind::Http, false, &patch).is_err());
        assert!(!f.spec.pins.user_agent);

        let patch = RequestPatch {
            max_range_bytes: Some(1 << 20),
            ..Default::default()
        };
        assert_eq!(
            check_refresh_patch(&patch, ProtocolKind::Http),
            Err("refresh 阶段不能改写 maxRangeBytes".to_string())
        );
    }

    #[test]
    fn range_window_cap_ignores_non_get_requests() {
        let spec = RequestSpec {
            method: reqwest::Method::POST,
            max_range_bytes: Some(1 << 20),
            ..Default::default()
        };
        assert_eq!(spec.range_window_cap(), None);
        let spec = RequestSpec {
            max_range_bytes: Some(0),
            ..Default::default()
        };
        assert_eq!(spec.range_window_cap(), None);
    }
}
