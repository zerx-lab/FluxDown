//! 外部捕获链接语法：系统 URL scheme、命令行参数与剪贴板共用的判定与 `fluxdown:` 深链解码。
//!
//! 浏览器扩展、桌面进程与 agent 对同一段链接必须得出同一结论，因此只在这里定义一次。

use std::path::PathBuf;

/// 可直接建下载任务的链接前缀（大小写不敏感）。
const CAPTURE_SCHEMES: [&str; 8] = [
    "magnet:",
    "ed2k://",
    "thunder://",
    "fluxdown:",
    "http://",
    "https://",
    "ftp://",
    "ftps://",
];

/// 判定 `value` 是否为可直接建任务的链接。
///
/// ```
/// use fluxdown_protocol::capture_link::is_capture_url;
/// assert!(is_capture_url("MAGNET:?xt=urn:btih:abc"));
/// assert!(!is_capture_url("/tmp/a.torrent"));
/// ```
#[must_use]
pub fn is_capture_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    CAPTURE_SCHEMES
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// `fluxdown:` 深链 → 实际下载链接：`fluxdown://download?url=<encoded>`（Firefox 等
/// 会规整成 `fluxdown://download/?url=`）或 `fluxdown:<url>`；其他 scheme 原样返回。
///
/// ```
/// use fluxdown_protocol::capture_link::normalize_capture_url;
/// assert_eq!(
///     normalize_capture_url("fluxdown://download?url=https%3A%2F%2Fa.b%2Fc"),
///     "https://a.b/c"
/// );
/// ```
#[must_use]
pub fn normalize_capture_url(value: &str) -> String {
    let Some(rest) = deep_link_rest(value) else {
        return value.to_owned();
    };
    if let Some(url) = deep_link_param(rest, "url") {
        return url;
    }
    percent_decode(rest)
}

/// `fluxdown://download?url=…&filename=…` 携带的文件名（浏览器扩展协议模式会带上
/// 它已识别的名字）；不是深链或未带名字时为 `None`。
///
/// ```
/// use fluxdown_protocol::capture_link::deep_link_file_name;
/// assert_eq!(
///     deep_link_file_name("fluxdown://download/?url=x&filename=a+b.zip").as_deref(),
///     Some("a b.zip")
/// );
/// ```
#[must_use]
pub fn deep_link_file_name(value: &str) -> Option<String> {
    deep_link_param(deep_link_rest(value)?, "filename").filter(|name| !name.trim().is_empty())
}

/// 去掉 `fluxdown:` 与其后的 `/`；不是深链时为 `None`。
fn deep_link_rest(value: &str) -> Option<&str> {
    const SCHEME: &str = "fluxdown:";
    let rest = value.get(SCHEME.len()..)?;
    value[..SCHEME.len()]
        .eq_ignore_ascii_case(SCHEME)
        .then(|| rest.trim_start_matches('/'))
}

/// `download?…` / `download/?…` 查询串里的参数值（form-urlencoded：`+` 为空格）。
fn deep_link_param(rest: &str, key: &str) -> Option<String> {
    let query = rest
        .strip_prefix("download?")
        .or_else(|| rest.strip_prefix("download/?"))?;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| percent_decode(&value.replace('+', " ")))
    })
}

/// `.torrent` 本机路径或 `file://` URL → 路径（不检查文件是否存在）。
///
/// ```
/// use fluxdown_protocol::capture_link::torrent_file_path;
/// assert_eq!(
///     torrent_file_path("file:///tmp/a%20b.torrent"),
///     Some(std::path::PathBuf::from("/tmp/a b.torrent"))
/// );
/// assert_eq!(torrent_file_path("https://a.b/c.torrent.html"), None);
/// ```
#[must_use]
pub fn torrent_file_path(value: &str) -> Option<PathBuf> {
    let raw = value.strip_prefix("file://").unwrap_or(value);
    let decoded = percent_decode(raw);
    decoded
        .to_ascii_lowercase()
        .ends_with(".torrent")
        .then(|| PathBuf::from(decoded))
}

/// `%XX` 百分号解码；非法序列原样保留，结果不是 UTF-8 时返回原文。
#[must_use]
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Some(hex) = value.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_owned())
}

/// 用户可在设置中关闭的「默认打开方式」关联：系统把 `.torrent` 文件、`magnet:` 或
/// `ed2k://` 链接交给 FluxDown 时，它们的去留受对应 opt-out 偏好约束。
///
/// wire 形态为小写名（`"torrent"` / `"magnet"` / `"ed2k"`），作为
/// `agent.capture.submit` / `agent.capture.submitTorrentFile` 的 `association` 参数，
/// 标记该请求来自系统默认处理程序。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenAssociation {
    Torrent,
    Magnet,
    Ed2k,
}

impl OpenAssociation {
    /// 系统交来的原始链接所属关联；`fluxdown:` 深链与普通直链不属于可关闭的关联。
    /// 须在 [`normalize_capture_url`] 之前判定：深链解码出的 `magnet:` 不受 magnet
    /// 关联开关约束。
    ///
    /// ```
    /// use fluxdown_protocol::capture_link::OpenAssociation;
    /// assert_eq!(
    ///     OpenAssociation::of_url("MAGNET:?xt=urn:btih:abc"),
    ///     Some(OpenAssociation::Magnet)
    /// );
    /// assert_eq!(OpenAssociation::of_url("fluxdown:magnet:?xt=urn:btih:abc"), None);
    /// ```
    #[must_use]
    pub fn of_url(value: &str) -> Option<Self> {
        let lower = value.to_ascii_lowercase();
        if lower.starts_with("magnet:") {
            Some(Self::Magnet)
        } else if lower.starts_with("ed2k://") {
            Some(Self::Ed2k)
        } else {
            None
        }
    }

    /// 用户在设置中手动关闭该关联时持久化的设备本地偏好键（布尔，`true` = 已关闭）。
    ///
    /// ```
    /// use fluxdown_protocol::capture_link::OpenAssociation;
    /// assert_eq!(
    ///     OpenAssociation::Torrent.opt_out_pref_key(),
    ///     "torrent_assoc_user_disabled"
    /// );
    /// ```
    #[must_use]
    pub const fn opt_out_pref_key(self) -> &'static str {
        match self {
            Self::Torrent => "torrent_assoc_user_disabled",
            Self::Magnet => "magnet_assoc_user_disabled",
            Self::Ed2k => "ed2k_assoc_user_disabled",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_fluxdown_scheme_variants() {
        assert_eq!(
            normalize_capture_url("fluxdown://download?url=https%3A%2F%2Fa.b%2Fc"),
            "https://a.b/c"
        );
        assert_eq!(
            normalize_capture_url("fluxdown:https://a.b/c"),
            "https://a.b/c"
        );
        assert_eq!(normalize_capture_url("magnet:?x"), "magnet:?x");
        // #743：Firefox 把 `fluxdown://download?` 规整成 `download/?`。
        let link =
            "fluxdown://download/?url=http%3A%2F%2Fh%2Fa%3Fq%3D1%2B2&filename=%E4%B8%AD+1.bin";
        assert_eq!(normalize_capture_url(link), "http://h/a?q=1+2");
        assert_eq!(deep_link_file_name(link).as_deref(), Some("中 1.bin"));
        assert_eq!(deep_link_file_name("fluxdown:https://a.b/c"), None);
        assert_eq!(deep_link_file_name("https://a.b/c?filename=x"), None);
    }

    #[test]
    fn thunder_links_are_capture_urls() {
        assert!(is_capture_url("thunder://QUFodHRwOi8vYS5iL2MuemlwWlo="));
        assert!(is_capture_url("THUNDER://x"));
    }

    #[test]
    fn percent_decode_keeps_malformed_and_multibyte_sequences() {
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
        // 百分号紧邻多字节字符：切片不能落在字符中间。
        assert_eq!(percent_decode("%中文"), "%中文");
    }

    #[test]
    fn torrent_paths_require_torrent_suffix() {
        assert_eq!(
            torrent_file_path("/tmp/X.TORRENT"),
            Some(PathBuf::from("/tmp/X.TORRENT"))
        );
        assert_eq!(torrent_file_path("/tmp/x.zip"), None);
    }
}
