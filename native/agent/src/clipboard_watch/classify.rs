//! L0：纯本地、微秒级的链接判定（扩展名 / 特殊协议 / 下载信号），不做任何网络访问。

use std::collections::HashSet;
use std::net::IpAddr;

use fluxdown_protocol::capture_link::percent_decode;
use reqwest::Url;

/// 内置监视扩展名（小写、不带点）。刻意不含 `ts`（TypeScript）、图片、txt、md、html。
const BUILTIN_EXTENSIONS: &[&str] = &[
    "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "cab", "iso", "img", "dmg", "exe",
    "msi", "msix", "appx", "pkg", "deb", "rpm", "appimage", "apk", "ipa", "xapk", "mp4", "mkv",
    "avi", "mov", "wmv", "flv", "webm", "m4v", "rmvb", "rm", "3gp", "mpg", "mpeg", "m3u8", "mp3",
    "flac", "wav", "aac", "ogg", "m4a", "wma", "opus", "ape", "pdf", "doc", "docx", "xls", "xlsx",
    "ppt", "pptx", "epub", "mobi", "azw3", "torrent", "bin", "jar",
];

/// 网页类扩展名：只有带下载信号才值得探测。
const WEB_EXTENSIONS: &[&str] = &[
    "html", "htm", "shtml", "php", "asp", "aspx", "jsp", "js", "css", "json", "xml",
];

const SIGNAL_SEGMENTS: &[&str] = &[
    "download",
    "downloads",
    "dl",
    "get",
    "attachment",
    "attachments",
    "file",
    "files",
    "fetch",
    "mirror",
    "release",
    "releases",
];

const SIGNAL_PARAMS: &[&str] = &[
    "download",
    "dl",
    "attachment",
    "file",
    "fileid",
    "file_id",
    "filename",
    "fid",
    "response-content-disposition",
];

const SENSITIVE_SEGMENTS: &[&str] = &[
    "login",
    "signin",
    "signup",
    "logout",
    "verify",
    "verification",
    "confirm",
    "reset",
    "password",
    "auth",
    "oauth",
    "callback",
    "unsubscribe",
    "invite",
    "activate",
    "magic",
];

const SENSITIVE_PARAMS: &[&str] = &["token", "code", "key", "otp", "ticket", "sig", "signature"];
const SENSITIVE_VALUE_LEN: usize = 16;

/// 取文件名候选的查询参数名。
const FILENAME_PARAMS: &[&str] = &[
    "filename",
    "file",
    "fn",
    "name",
    "response-content-disposition",
];

/// L0 判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 确定是下载资源。
    Accept,
    /// 不是下载资源。
    Reject,
    /// 带下载信号但无法仅凭文本判定，需 L1 联网探测。
    NeedsProbe,
}

/// 监视扩展名表：内置 ∪ 用户偏好 `general.clipboard_watch_extensions`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchedExtensions {
    custom: HashSet<String>,
}

impl WatchedExtensions {
    /// 解析用户追加的扩展名：逗号 / 分号 / 空白 / 换行分隔，可带前导点，大小写不敏感。
    pub fn parse(raw: &str) -> Self {
        let custom = raw
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '，' | '；' | '、'))
            .map(|item| item.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|item| !item.is_empty() && item.chars().all(|c| c.is_ascii_alphanumeric()))
            .collect();
        Self { custom }
    }

    fn contains(&self, extension: &str) -> bool {
        BUILTIN_EXTENSIONS.contains(&extension) || self.custom.contains(extension)
    }
}

/// L0 判定一条候选链接。
pub fn classify(link: &str, extensions: &WatchedExtensions) -> Verdict {
    let lower = link.to_ascii_lowercase();
    if lower.starts_with("magnet:") {
        return accept_if(lower.contains("xt=urn:btih:") || lower.contains("xt=urn:btmh:"));
    }
    if lower.starts_with("ed2k://") {
        return accept_if(lower.starts_with("ed2k://|file|"));
    }
    if lower.starts_with("thunder://") || lower.starts_with("fluxdown:") {
        return Verdict::Accept;
    }
    if !["http://", "https://", "ftp://", "ftps://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        return Verdict::Reject;
    }
    let Ok(url) = Url::parse(link) else {
        return Verdict::Reject;
    };
    if !host_is_remote(&url) {
        return Verdict::Reject;
    }

    let segments = path_segments(&url);
    let query: Vec<(String, String)> = url
        .query_pairs()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.into_owned()))
        .collect();

    let candidates = file_name_candidates(&segments, &query);
    if candidates
        .iter()
        .filter_map(|name| extension_of(name))
        .any(|extension| extensions.contains(&extension))
    {
        return Verdict::Accept;
    }

    // 最末路径段的扩展名：网页类、或「像扩展名但不认识」（图片、.ts、.txt…）。
    let last_extension = segments.last().and_then(|name| extension_of(name));
    let probeable = match last_extension.as_deref() {
        None => true,
        Some(extension) => WEB_EXTENSIONS.contains(&extension),
    };
    if probeable && has_download_signal(&segments, &query) && !is_sensitive(&segments, &query) {
        return Verdict::NeedsProbe;
    }
    Verdict::Reject
}

/// 探测后的兜底：最终地址的最末路径段扩展名命中监视扩展名表。
pub fn final_url_has_watched_extension(final_url: &str, extensions: &WatchedExtensions) -> bool {
    let Ok(url) = Url::parse(final_url) else {
        return false;
    };
    path_segments(&url)
        .last()
        .and_then(|name| extension_of(name))
        .is_some_and(|extension| extensions.contains(&extension))
}

fn accept_if(condition: bool) -> Verdict {
    if condition {
        Verdict::Accept
    } else {
        Verdict::Reject
    }
}

/// 回环主机、无点的内网短主机名不是可下载的远端资源。
fn host_is_remote(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !ip.is_loopback();
    }
    let domain = host.trim_end_matches('.').to_ascii_lowercase();
    domain != "localhost" && !domain.ends_with(".localhost") && domain.contains('.')
}

/// 百分号解码后的非空路径段。
fn path_segments(url: &Url) -> Vec<String> {
    url.path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .map(percent_decode)
                .collect()
        })
        .unwrap_or_default()
}

/// 文件名候选：全部路径段（末段与 `/file.zip/download` 这类中间段）、文件名类查询参数。
fn file_name_candidates(segments: &[String], query: &[(String, String)]) -> Vec<String> {
    let mut names: Vec<String> = segments.to_vec();
    for (name, value) in query {
        if !FILENAME_PARAMS.contains(&name.as_str()) {
            continue;
        }
        if name == "response-content-disposition" {
            if let Some(file_name) = disposition_file_name(value) {
                names.push(file_name);
            }
        } else {
            names.push(value.clone());
        }
    }
    names
}

/// `attachment; filename="a.zip"` / `filename*=UTF-8''a.zip` 中的文件名。
fn disposition_file_name(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let start = lower.find("filename")? + "filename".len();
    let rest = value[start..].trim_start_matches('*').trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let rest = rest
        .strip_prefix("UTF-8''")
        .or_else(|| rest.strip_prefix("utf-8''"))
        .unwrap_or(rest);
    let name = rest
        .trim_start_matches('"')
        .split(['"', ';'])
        .next()
        .map(percent_decode)?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// 末个 `.` 之后的扩展名（小写）；无点、点在开头、纯数字（版本号 `v1.2.3`）或过长视为无扩展名。
fn extension_of(name: &str) -> Option<String> {
    let dot = name.rfind('.')?;
    if dot == 0 {
        return None;
    }
    let extension = &name[dot + 1..];
    let valid = !extension.is_empty()
        && extension.len() <= 10
        && extension.chars().all(|c| c.is_ascii_alphanumeric())
        && !extension.chars().all(|c| c.is_ascii_digit());
    valid.then(|| extension.to_ascii_lowercase())
}

fn has_download_signal(segments: &[String], query: &[(String, String)]) -> bool {
    segments
        .iter()
        .any(|segment| SIGNAL_SEGMENTS.contains(&segment.to_ascii_lowercase().as_str()))
        || query
            .iter()
            .any(|(name, _)| SIGNAL_PARAMS.contains(&name.as_str()))
}

fn is_sensitive(segments: &[String], query: &[(String, String)]) -> bool {
    segments.iter().any(|segment| {
        let segment = segment.to_ascii_lowercase();
        SENSITIVE_SEGMENTS.iter().any(|word| segment.contains(word))
    }) || query.iter().any(|(name, value)| {
        SENSITIVE_PARAMS.contains(&name.as_str()) && value.len() >= SENSITIVE_VALUE_LEN
    })
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixture {
        resource: Vec<String>,
        not_resource: Vec<String>,
        probe: Vec<String>,
    }

    fn fixture() -> Fixture {
        serde_json::from_str(include_str!("fixtures/links.json")).expect("links fixture parses")
    }

    #[test]
    fn fixture_groups_classify_as_declared() {
        let extensions = WatchedExtensions::default();
        let fixture = fixture();
        for link in &fixture.not_resource {
            assert_eq!(classify(link, &extensions), Verdict::Reject, "{link}");
        }
        for link in &fixture.resource {
            assert_eq!(classify(link, &extensions), Verdict::Accept, "{link}");
        }
        for link in &fixture.probe {
            assert_eq!(classify(link, &extensions), Verdict::NeedsProbe, "{link}");
        }
    }

    #[test]
    fn custom_extension_takes_effect() {
        let link = "https://example.com/comics/vol1.cbz";
        assert_eq!(
            classify(link, &WatchedExtensions::default()),
            Verdict::Reject
        );
        let custom = WatchedExtensions::parse("CBZ, .mobi;\n epub");
        assert_eq!(classify(link, &custom), Verdict::Accept);
        assert_eq!(
            classify("https://example.com/a.cbz?x=1", &custom),
            Verdict::Accept
        );
    }

    #[test]
    fn parse_extensions_accepts_mixed_separators() {
        let parsed = WatchedExtensions::parse(" .CBZ,mobi；epub\n7z、 ");
        assert!(parsed.custom.contains("cbz"));
        assert!(parsed.custom.contains("mobi"));
        assert!(parsed.custom.contains("epub"));
        assert!(parsed.custom.contains("7z"));
        assert_eq!(parsed.custom.len(), 4);
        assert_eq!(
            WatchedExtensions::parse("a/b, c d!"),
            WatchedExtensions::parse("c")
        );
    }

    #[test]
    fn magnet_and_ed2k_need_valid_payload() {
        let extensions = WatchedExtensions::default();
        assert_eq!(
            classify("magnet:?dn=only-name", &extensions),
            Verdict::Reject
        );
        assert_eq!(
            classify("magnet:?xt=urn:btmh:1220abcd", &extensions),
            Verdict::Accept
        );
        assert_eq!(
            classify("ed2k://|server|1.2.3.4|4661|/", &extensions),
            Verdict::Reject
        );
    }

    #[test]
    fn sensitive_links_are_never_probed() {
        let extensions = WatchedExtensions::default();
        assert_eq!(
            classify("https://example.com/login/download", &extensions),
            Verdict::Reject
        );
        assert_eq!(
            classify(
                "https://example.com/dl?token=0123456789abcdef0123",
                &extensions
            ),
            Verdict::Reject
        );
        assert_eq!(
            classify("https://example.com/dl?token=short", &extensions),
            Verdict::NeedsProbe
        );
    }

    #[test]
    fn web_extension_probes_only_with_signal() {
        let extensions = WatchedExtensions::default();
        assert_eq!(
            classify("https://example.com/index.html", &extensions),
            Verdict::Reject
        );
        assert_eq!(
            classify("https://example.com/download.php?id=3", &extensions),
            Verdict::Reject
        );
        assert_eq!(
            classify("https://example.com/downloads/get.php", &extensions),
            Verdict::NeedsProbe
        );
    }

    #[test]
    fn disposition_param_supplies_file_name() {
        let extensions = WatchedExtensions::default();
        assert_eq!(
            classify(
                "https://example.com/x?response-content-disposition=attachment%3B%20filename%3D%22a.zip%22",
                &extensions
            ),
            Verdict::Accept
        );
    }

    #[test]
    fn final_url_extension_fallback() {
        let extensions = WatchedExtensions::default();
        assert!(final_url_has_watched_extension(
            "https://cdn.example.com/a/b.zip?x=1",
            &extensions
        ));
        assert!(!final_url_has_watched_extension(
            "https://cdn.example.com/a/b",
            &extensions
        ));
        assert!(!final_url_has_watched_extension("not a url", &extensions));
    }
}
