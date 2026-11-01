//! 「新建下载」表单的纯模型：链接解析、代理 / UA / 线程预设与请求构建。
//!
//! 链接解析、任务代理、UA 预设与线程选择的纯规则，不依赖 GPUI。

use std::collections::{BTreeMap, HashMap, HashSet};

use fluxdown_protocol::{CreateTaskItem, CreateTaskRequest, MAX_TASK_SEGMENTS};

use super::aria2c::{RequestHints, join_continuations, parse_command};

/// 一条解析出的下载条目（aria2 风格：URL + 可选 `out=` / `checksum=` 选项行）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UrlEntry {
    pub(crate) url: String,
    pub(crate) file_name: String,
    pub(crate) checksum: String,
}

impl UrlEntry {
    fn with_url(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            ..Self::default()
        }
    }
}

/// 外部捕获 → 条目：捕获文件名与链接路径末段（百分号解码后）逐字相同时省略 `out=`。
///
/// 这种 `out=` 与自动识别结果一致，却会在链接框里多出一行缩进的选项行，看起来像链接
/// 被错误折行，还会把自动名变成显式名而阻断引擎完成期定名；只有真正改名（如
/// Content-Disposition 给出的名字）才保留。
pub(crate) fn capture_entry(url: &str, file_name: &str) -> UrlEntry {
    let file_name = file_name.trim();
    let redundant = url_file_segment(url).is_some_and(|segment| {
        segment == file_name
            || percent_decode_utf8(segment).is_some_and(|decoded| decoded == file_name)
    });
    UrlEntry {
        url: url.to_owned(),
        file_name: if redundant {
            String::new()
        } else {
            file_name.to_owned()
        },
        checksum: String::new(),
    }
}

/// 百分号解码为字节；非法 `%` 序列按字面保留。
fn percent_decode_bytes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(&hi), Some(&lo)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(hi), Some(lo)) = (hex(hi), hex(lo))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// 百分号解码为 UTF-8；解码结果不是合法 UTF-8 时返回 `None`。非法 `%` 序列按字面保留。
pub(crate) fn percent_decode_utf8(text: &str) -> Option<String> {
    String::from_utf8(percent_decode_bytes(text)).ok()
}

/// 百分号解码为 UTF-8，非法字节以 U+FFFD 替换（与 agent 分类目录解析的 URL 末段解码一致）。
pub(crate) fn percent_decode_lossy(text: &str) -> String {
    String::from_utf8_lossy(&percent_decode_bytes(text)).into_owned()
}

/// `scheme://host/a/b.zip?x#y` → `b.zip`；无路径或以 `/` 结尾时为 `None`。
pub(crate) fn url_file_segment(url: &str) -> Option<&str> {
    let rest = &url[url.find("://")? + 3..];
    let rest = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    let path = &rest[rest.find('/')?..];
    path.rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
}

/// 解析多行文本为下载条目。
///
/// - 原始行以空格 / Tab 开头且不以链接开头 = 选项行，附着到上一条（`out=` / `checksum=`）；
///   以链接开头的缩进行（从网页 / 聊天复制常带前导空白）按普通行解析；
/// - 零宽字符与 BOM（U+200B/C/D、U+2060、U+FEFF）先整体剔除，它们不可见且不属于链接；
/// - `#` 开头为注释，空行跳过；
/// - 含 `magnet:?` / `ed2k://` 时从该位置截取到行尾；
/// - 其余行：`loose` 为 true 时取行内首个 `http(s)/ftp/thunder` 链接并去掉尾部标点
///   （TXT 导入），否则要求链接位于行首（手动输入）。`thunder://` 原样保留，由引擎
///   建任务时解出真实地址（#696）。
/// - 行首为 `aria2c` 的命令（可带路径 / 引号，续行已合并）提取其中的链接与 `-o` /
///   `--checksum`（#299，见 [`crate::model::aria2c`]）；不是 aria2c 的行不受影响。
pub(crate) fn parse_entries(text: &str, loose: bool) -> Vec<UrlEntry> {
    parse_input(text, loose).entries
}

/// [`parse_entries`] 的完整结果：条目 + 首条 aria2c 命令带出的请求信息。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ParsedInput {
    pub(crate) entries: Vec<UrlEntry>,
    /// 第一条产出了条目的 aria2c 命令的头部信息（其余命令的不采用）；没有时为空。
    pub(crate) hints: RequestHints,
}

/// 解析规则见 [`parse_entries`]。行首为 `aria2c` 的命令（先合并其续行）按 shell 规则切词，
/// 提取链接 / `-o` / `--checksum`（#299，详见 [`crate::model::aria2c`]），头部信息随结果
/// 返回；其余行的行为不变。
pub(crate) fn parse_input(text: &str, loose: bool) -> ParsedInput {
    let visible = strip_invisible(text);
    let text = join_continuations(&visible);
    let mut entries = Vec::new();
    let mut hints: Option<RequestHints> = None;
    let mut current: Option<UrlEntry> = None;
    for line in text.split('\n') {
        if (line.starts_with(' ') || line.starts_with('\t')) && !starts_with_link(line.trim()) {
            let Some(entry) = current.as_mut() else {
                continue;
            };
            let trimmed = line.trim();
            if let Some(name) = trimmed.strip_prefix("out=") {
                entry.file_name = name.to_owned();
            } else if let Some(checksum) = trimmed.strip_prefix("checksum=") {
                entry.checksum = checksum.to_owned();
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(entry) = current.take() {
            entries.push(entry);
        }
        if let Some(command) = parse_command(trimmed) {
            let mut produced = command.entries;
            if !produced.is_empty() && hints.is_none() {
                hints = Some(command.hints);
            }
            current = produced.pop();
            entries.extend(produced);
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(index) = lower.find("magnet:?") {
            current = Some(UrlEntry::with_url(&trimmed[index..]));
        } else if let Some(index) = lower.find("ed2k://") {
            current = Some(UrlEntry::with_url(&trimmed[index..]));
        } else if loose {
            if let Some(url) = find_url(trimmed) {
                let url = trim_url_tail(url);
                if !url.is_empty() {
                    current = Some(UrlEntry::with_url(url));
                }
            }
        } else if let Some(url) = leading_manual_url(trimmed) {
            current = Some(UrlEntry::with_url(url));
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    ParsedInput {
        entries,
        hints: hints.unwrap_or_default(),
    }
}

/// 剔除零宽字符与 BOM；无此类字符时不分配。
fn strip_invisible(text: &str) -> std::borrow::Cow<'_, str> {
    const INVISIBLE: [char; 5] = ['\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}'];
    if text.contains(INVISIBLE) {
        std::borrow::Cow::Owned(text.replace(INVISIBLE, ""))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// `^(https?|ftp|thunder)://\S+`（忽略大小写）。
fn leading_url(text: &str) -> Option<&str> {
    let scheme = ["https://", "http://", "ftp://", "thunder://"]
        .into_iter()
        .find(|scheme| {
            text.len() >= scheme.len()
                && text.as_bytes()[..scheme.len()].eq_ignore_ascii_case(scheme.as_bytes())
        })?;
    let rest = &text[scheme.len()..];
    let body = rest.find(char::is_whitespace).unwrap_or(rest.len());
    (body > 0).then(|| &text[..scheme.len() + body])
}

/// 手动输入：行首链接。查询串里未编码的空格属于链接本身（`a.zip?n=aaa bbb`，浏览器
/// 照常请求，服务器据此命名，#131），所以链接已进入查询串时延伸到行尾或下一个链接
/// 之前；没有查询串时空白之后的内容仍视为附注丢弃。
fn leading_manual_url(text: &str) -> Option<&str> {
    let head = leading_url(text)?;
    if !head.contains('?') || head.len() == text.len() {
        return Some(head);
    }
    let end = text[head.len()..]
        .char_indices()
        .map(|(index, _)| head.len() + index)
        .find(|&index| {
            text[..index].ends_with(char::is_whitespace) && starts_with_link(&text[index..])
        })
        .unwrap_or(text.len());
    Some(text[..end].trim_end())
}

/// 是否以可建任务的链接开头（`http(s)://` / `ftp://` / `thunder://` / `magnet:?` /
/// `ed2k://`，忽略大小写）。
pub(super) fn starts_with_link(text: &str) -> bool {
    [
        "https://",
        "http://",
        "ftp://",
        "thunder://",
        "magnet:?",
        "ed2k://",
    ]
    .into_iter()
    .any(|scheme| {
        text.len() >= scheme.len()
            && text.as_bytes()[..scheme.len()].eq_ignore_ascii_case(scheme.as_bytes())
    })
}

/// 行内首个 `(https?|ftp|thunder)://\S+` 匹配（忽略大小写）。
fn find_url(line: &str) -> Option<&str> {
    line.char_indices()
        .find_map(|(index, _)| leading_url(&line[index..]))
}

/// 去掉 URL 末尾常见标点（TXT 文本中 URL 后可能跟随句号 / 逗号等）。
fn trim_url_tail(url: &str) -> &str {
    url.trim_end_matches(|c: char| ".,;:!?()[]{}".contains(c))
}

/// 条目 → aria2 风格文本（含 `out=` / `checksum=` 选项行）。
pub(crate) fn entry_to_text(entry: &UrlEntry) -> String {
    let mut text = entry.url.clone();
    if !entry.file_name.is_empty() {
        text.push_str("\n  out=");
        text.push_str(&entry.file_name);
    }
    if !entry.checksum.is_empty() {
        text.push_str("\n  checksum=");
        text.push_str(&entry.checksum);
    }
    text
}

/// 把条目追加到现有文本末尾：原文逐字保留（含注释 / 尚未输完的行 / aria2c 命令），已存在
/// 的 URL 跳过。用于外部捕获与 TXT 导入陆续进入正在编辑的表单。
pub(crate) fn append_entries(
    existing_text: &str,
    entries: impl IntoIterator<Item = UrlEntry>,
) -> String {
    let mut seen = parse_entries(existing_text, false)
        .into_iter()
        .map(|entry| entry.url)
        .collect::<HashSet<_>>();
    let mut text = existing_text.trim_end().to_owned();
    for entry in entries {
        if !seen.insert(entry.url.clone()) {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&entry_to_text(&entry));
    }
    text
}

/// 强制直连哨兵值。
pub(crate) const PROXY_DIRECT_SENTINEL: &str = "direct://";
/// 跟随系统代理哨兵值。
pub(crate) const PROXY_SYSTEM_SENTINEL: &str = "system://";

/// 任务代理选择项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProxyChoice {
    FollowGlobal,
    Direct,
    System,
    GlobalManual,
    Custom,
}

impl ProxyChoice {
    /// 下拉顺序。
    pub(crate) const ALL: [Self; 5] = [
        Self::FollowGlobal,
        Self::Direct,
        Self::System,
        Self::GlobalManual,
        Self::Custom,
    ];

    /// 生成 `proxy_url` wire 值；`manual_url` 为全局手动代理 URL（空 = 未配置）。
    pub(crate) fn wire(self, manual_url: &str, custom_url: &str) -> String {
        match self {
            Self::FollowGlobal => String::new(),
            Self::Direct => PROXY_DIRECT_SENTINEL.to_owned(),
            Self::System => PROXY_SYSTEM_SENTINEL.to_owned(),
            Self::GlobalManual => manual_url.to_owned(),
            Self::Custom => custom_url.trim().to_owned(),
        }
    }
}

/// 从 daemon 配置拼出全局手动代理 URL：`type://[user[:pass]@]host:port`。
///
/// host 为空或端口解析为 0 / 非法时返回空串（= 未配置），userinfo 百分号编码。
pub(crate) fn manual_proxy_url(config: &BTreeMap<String, String>) -> String {
    let trimmed = |key: &str| config.get(key).map_or("", |value| value.trim());
    let host = trimmed("proxy_host");
    let port = trimmed("proxy_port").parse::<u32>().unwrap_or(0);
    if host.is_empty() || port == 0 {
        return String::new();
    }
    let proxy_type = match trimmed("proxy_type") {
        "" => "http",
        value => value,
    };
    let user = config.get("proxy_username").map_or("", String::as_str);
    let password = config.get("proxy_password").map_or("", String::as_str);
    let mut url = format!("{proxy_type}://");
    if !user.is_empty() {
        encode_component(user, &mut url);
        if !password.is_empty() {
            url.push(':');
            encode_component(password, &mut url);
        }
        url.push('@');
    }
    url.push_str(host);
    url.push(':');
    url.push_str(&port.to_string());
    url
}

/// URI 组件编码：除 `A-Za-z0-9-_.!~*'()` 外全部按 UTF-8 百分号编码。
fn encode_component(value: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
}

/// 预设 UA（key → UA 字符串）。
pub(crate) const UA_PRESETS: &[(&str, &str)] = &[
    (
        "chrome",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
    ),
    (
        "firefox",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:147.0) Gecko/20100101 Firefox/147.0",
    ),
    (
        "edge",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.3800.70",
    ),
    (
        "safari",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15",
    ),
];

/// 继承全局 UA 的预设键（输入框留空）。
pub(crate) const UA_PRESET_DEFAULT: &str = "default";
/// 自定义 UA 的预设键（输入框自由填写）。
pub(crate) const UA_PRESET_CUSTOM: &str = "custom";

/// UA 预设下拉顺序：继承全局 → 各浏览器 → 自定义。
pub(crate) fn ua_preset_keys() -> impl Iterator<Item = &'static str> {
    std::iter::once(UA_PRESET_DEFAULT)
        .chain(UA_PRESETS.iter().map(|(key, _)| *key))
        .chain(std::iter::once(UA_PRESET_CUSTOM))
}

/// UA 字符串 → 预设键；空 = `default`，未命中 = `custom`。
pub(crate) fn detect_ua_preset(ua: &str) -> &'static str {
    if ua.is_empty() {
        return UA_PRESET_DEFAULT;
    }
    UA_PRESETS
        .iter()
        .find(|(_, value)| *value == ua)
        .map_or(UA_PRESET_CUSTOM, |(key, _)| key)
}

/// 预设键 → UA 字符串（`default` → 空）。
pub(crate) fn ua_preset_value(key: &str) -> &'static str {
    UA_PRESETS
        .iter()
        .find(|(preset, _)| *preset == key)
        .map_or("", |(_, value)| value)
}

/// 线程数下拉预设（`自动` 与 `自定义` 之间的固定档位）。
pub(crate) const THREAD_PRESETS: &[i32] = &[4, 8, 16, 32, 64];
/// 自定义线程数上限（= 引擎单任务连接上限）。
pub(crate) const MAX_THREADS: i32 = MAX_TASK_SEGMENTS;

/// 线程数选择：自动 / 预设档位 / 自定义输入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadChoice {
    Auto,
    Preset(i32),
    Custom,
}

impl ThreadChoice {
    /// 由默认分段数推导初始选择：0 = 自动，预设内 = 预设，其余 = 自定义。
    pub(crate) fn from_segments(segments: i32) -> Self {
        if segments <= 0 {
            Self::Auto
        } else if THREAD_PRESETS.contains(&segments) {
            Self::Preset(segments)
        } else {
            Self::Custom
        }
    }
}

/// 自定义输入 → `segments`：非法 / 小于 1 = 0（自动），上限 [`MAX_THREADS`]。
pub(crate) fn custom_segments(text: &str) -> i32 {
    match text.trim().parse::<i32>() {
        Ok(value) if value >= 1 => value.min(MAX_THREADS),
        _ => 0,
    }
}

/// 哈希校验算法（wire 字面量，无本地化），默认 `sha-256`。
pub(crate) const HASH_ALGORITHMS: &[&str] = &["md5", "sha-1", "sha-256", "sha-512"];
pub(crate) const DEFAULT_HASH_ALGORITHM: &str = "sha-256";

/// `algo=hexhash`；哈希值为空时返回空串（跳过校验）。
pub(crate) fn checksum_spec(algorithm: &str, hash: &str) -> String {
    let hash = hash.trim();
    if hash.is_empty() {
        String::new()
    } else {
        format!("{algorithm}={hash}")
    }
}

/// 整批共享的表单选项。`rename` / HTTP 认证 / 高级面板校验值只作用于单条提交。
#[derive(Clone, Debug, Default)]
pub(crate) struct DraftOptions {
    pub(crate) save_dir: String,
    pub(crate) segments: i32,
    pub(crate) cookies: String,
    pub(crate) proxy_url: String,
    pub(crate) user_agent: String,
    pub(crate) queue_id: String,
    pub(crate) checksum: String,
    pub(crate) ignore_tls_errors: bool,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) start_paused: bool,
    pub(crate) rename: String,
    pub(crate) http_user: String,
    pub(crate) http_password: String,
    pub(crate) save_site_auth: bool,
}

/// 一次提交的建任务请求：整批共享的 `common`（`url` / `fileName` / `checksum` 为空）+
/// 逐条链接。共享选项只存一份，不随条目复制。
#[derive(Clone, Debug)]
pub(crate) struct BuiltRequests {
    pub(crate) common: CreateTaskRequest,
    pub(crate) items: Vec<CreateTaskItem>,
}

impl BuiltRequests {
    /// 条目还原成完整的单条请求（单条预解析 / 外部捕获确认需要完整请求）。
    pub(crate) fn request_for(&self, item: &CreateTaskItem) -> CreateTaskRequest {
        compose_request(&self.common, item.clone())
    }

    /// 把一条完整请求拆回「共享选项 + 单条」（预解析后沿普通任务路径提交）。
    pub(crate) fn from_single(mut request: CreateTaskRequest) -> Self {
        let item = CreateTaskItem {
            url: std::mem::take(&mut request.url),
            file_name: std::mem::take(&mut request.file_name),
            checksum: std::mem::take(&mut request.checksum),
        };
        Self {
            common: request,
            items: vec![item],
        }
    }
}

/// 共享选项 + 一条链接 → 完整请求。
pub(crate) fn compose_request(
    common: &CreateTaskRequest,
    item: CreateTaskItem,
) -> CreateTaskRequest {
    let mut request = common.clone();
    request.url = item.url;
    request.file_name = item.file_name;
    request.checksum = item.checksum;
    request
}

/// 每条条目一个 [`CreateTaskItem`]，共享字段来自 [`DraftOptions`]。
///
/// 单条时重命名优先于 `out=`、高级面板校验值优先于
/// `checksum=`、附带 HTTP 认证；多条时仅使用条目自带的文件名 / 校验值。
pub(crate) fn build_requests(entries: &[UrlEntry], options: &DraftOptions) -> BuiltRequests {
    let single = entries.len() == 1;
    let items = entries
        .iter()
        .map(|entry| CreateTaskItem {
            url: entry.url.clone(),
            file_name: if single && !options.rename.is_empty() {
                options.rename.clone()
            } else {
                entry.file_name.clone()
            },
            checksum: if single && !options.checksum.is_empty() {
                options.checksum.clone()
            } else {
                entry.checksum.clone()
            },
        })
        .collect();
    let common = CreateTaskRequest {
        url: String::new(),
        file_name: String::new(),
        save_dir: options.save_dir.clone(),
        segments: options.segments,
        cookies: options.cookies.clone(),
        referrer: String::new(),
        proxy_url: options.proxy_url.clone(),
        user_agent: options.user_agent.clone(),
        queue_id: options.queue_id.clone(),
        checksum: String::new(),
        ignore_tls_errors: options.ignore_tls_errors,
        headers: (!options.headers.is_empty()).then(|| options.headers.clone()),
        torrent_b64: None,
        method: None,
        body: None,
        audio_url: None,
        start_paused: options.start_paused,
        http_user: if single {
            options.http_user.clone()
        } else {
            String::new()
        },
        http_password: if single {
            options.http_password.clone()
        } else {
            String::new()
        },
        save_site_auth: single && options.save_site_auth,
    };
    BuiltRequests { common, items }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::{
        DraftOptions, ProxyChoice, ThreadChoice, UrlEntry, append_entries, build_requests,
        capture_entry, checksum_spec, custom_segments, detect_ua_preset, manual_proxy_url,
        parse_entries, parse_input,
    };

    #[test]
    fn append_entries_keeps_existing_text_verbatim_and_skips_known_urls() {
        let existing = "# mine\nhttps://a.example/one.zip\nhttps://half-typ\n";
        let text = append_entries(
            existing,
            [
                UrlEntry {
                    url: "https://a.example/one.zip".to_owned(),
                    file_name: "dup.zip".to_owned(),
                    ..UrlEntry::default()
                },
                UrlEntry {
                    url: "https://b.example/two.bin".to_owned(),
                    file_name: "two.bin".to_owned(),
                    ..UrlEntry::default()
                },
            ],
        );
        assert_eq!(
            text,
            "# mine\nhttps://a.example/one.zip\nhttps://half-typ\nhttps://b.example/two.bin\n  out=two.bin"
        );
        assert_eq!(parse_entries(&text, false)[2].file_name, "two.bin");
        assert_eq!(
            append_entries(
                "",
                [UrlEntry {
                    url: "https://c.example/x".to_owned(),
                    ..UrlEntry::default()
                }]
            ),
            "https://c.example/x"
        );
    }

    #[test]
    fn capture_entry_drops_only_file_names_matching_the_url_segment() {
        let name = |url, file| capture_entry(url, file).file_name;
        assert_eq!(
            name(
                "https://release.files.ghostty.org/1.3.1/Ghostty.dmg",
                "Ghostty.dmg"
            ),
            ""
        );
        assert_eq!(name("https://a.example/dl/x.zip?sig=1#top", "x.zip"), "");
        assert_eq!(
            name("https://a.example/dl/x.zip", "renamed.zip"),
            "renamed.zip"
        );
        assert_eq!(name("https://a.example/a%20b.zip", "a b.zip"), "");
        assert_eq!(name("https://x.example/%E4%B8%AD.zip", "中.zip"), "");
        assert_eq!(
            name("https://x.example/%E4%B8%AD.zip", "other.zip"),
            "other.zip"
        );
        assert_eq!(name("https://x.example/%E4%B8.zip", "%E4%B8.zip"), "");
        assert_eq!(name("https://x.example/%B6%D4.zip", "对.zip"), "对.zip");
        assert_eq!(name("https://a.example/", "index.html"), "index.html");
        assert_eq!(name("https://a.example?f=x.zip", "x.zip"), "x.zip");
    }

    #[test]
    fn strict_parse_requires_url_at_line_start_and_attaches_options() {
        let text = "https://a.example/one.zip\n  out=renamed.zip\n\tchecksum=sha-256=abc\n# comment\nsee https://b.example/skip\nMAGNET link: magnet:?xt=urn:btih:xyz\nfoo ed2k://|file|x|\nFTP://c.example/f.bin trailing";
        let entries = parse_entries(text, false);
        assert_eq!(
            entries,
            vec![
                UrlEntry {
                    url: "https://a.example/one.zip".to_owned(),
                    file_name: "renamed.zip".to_owned(),
                    checksum: "sha-256=abc".to_owned(),
                },
                UrlEntry {
                    url: "magnet:?xt=urn:btih:xyz".to_owned(),
                    ..UrlEntry::default()
                },
                UrlEntry {
                    url: "ed2k://|file|x|".to_owned(),
                    ..UrlEntry::default()
                },
                UrlEntry {
                    url: "FTP://c.example/f.bin".to_owned(),
                    ..UrlEntry::default()
                },
            ]
        );
    }

    #[test]
    fn strict_parse_keeps_unencoded_spaces_inside_query() {
        let entries = parse_entries(
            "http://a.example/x.zip?n=aaa bbb ccc  \nhttps://b.example/y?q=1 2 https://c.example/z.bin",
            false,
        );
        let urls: Vec<_> = entries.iter().map(|entry| entry.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "http://a.example/x.zip?n=aaa bbb ccc",
                "https://b.example/y?q=1 2"
            ]
        );
    }

    #[test]
    fn loose_parse_extracts_inline_urls_and_trims_tail_punctuation() {
        let entries = parse_entries("see https://a.example/x.zip). next\nnothing here\n", true);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://a.example/x.zip");
    }

    /// #696：`thunder://` 迅雷链接与 http(s)/ftp 同等识别（解封装由引擎建任务时完成）。
    #[test]
    fn parse_accepts_thunder_links() {
        let strict = parse_entries(
            "thunder://QUFodHRwOi8vYS5iL2MuemlwWlo=\n  out=c.zip\nTHUNDER://QUFodHRwOi8vYS5iL2QuemlwWlo= 迅雷\nhttps://a.example/x?q=1 2 thunder://QUFodHRwOi8vYS5iL2MuemlwWlo=",
            false,
        );
        assert_eq!(
            strict,
            vec![
                UrlEntry {
                    url: "thunder://QUFodHRwOi8vYS5iL2MuemlwWlo=".to_owned(),
                    file_name: "c.zip".to_owned(),
                    ..UrlEntry::default()
                },
                UrlEntry {
                    url: "THUNDER://QUFodHRwOi8vYS5iL2QuemlwWlo=".to_owned(),
                    ..UrlEntry::default()
                },
                UrlEntry {
                    url: "https://a.example/x?q=1 2".to_owned(),
                    ..UrlEntry::default()
                },
            ]
        );
        let loose = parse_entries("迅雷：thunder://QUFodHRwOi8vYS5iL2MuemlwWlo=.\n", true);
        assert_eq!(loose.len(), 1);
        assert_eq!(loose[0].url, "thunder://QUFodHRwOi8vYS5iL2MuemlwWlo=");
    }

    /// 从网页 / 聊天复制的链接常带前导空白或零宽字符 / BOM：仍识别为链接，选项行语义不变。
    #[test]
    fn parse_tolerates_leading_whitespace_and_invisible_chars() {
        let url = "https://a.example/x.exe";
        for text in [
            format!(" {url}"),
            format!("\t{url}"),
            format!("\u{200B}{url}"),
            format!("\u{FEFF}{url}"),
            format!("  \u{2060}{url}\u{200D}"),
        ] {
            let entries = parse_entries(&text, false);
            assert_eq!(entries.len(), 1, "{text:?}");
            assert_eq!(entries[0].url, url, "{text:?}");
        }
        let entries = parse_entries(
            "https://a.example/a\n  out=a.bin\n  https://a.example/b\n\tchecksum=md5=ff",
            false,
        );
        assert_eq!(
            entries,
            vec![
                UrlEntry {
                    url: "https://a.example/a".to_owned(),
                    file_name: "a.bin".to_owned(),
                    ..UrlEntry::default()
                },
                UrlEntry {
                    url: "https://a.example/b".to_owned(),
                    checksum: "md5=ff".to_owned(),
                    ..UrlEntry::default()
                },
            ]
        );
        assert!(parse_entries("  out=orphan.bin", false).is_empty());
    }

    #[test]
    fn import_appends_by_url_and_keeps_option_lines() {
        let merged = append_entries(
            "https://a.example/x\n  out=x.bin",
            vec![
                UrlEntry {
                    url: "https://a.example/x".to_owned(),
                    file_name: "dup.bin".to_owned(),
                    checksum: String::new(),
                },
                UrlEntry {
                    url: "https://b.example/y".to_owned(),
                    file_name: String::new(),
                    checksum: "md5=ff".to_owned(),
                },
            ],
        );
        assert_eq!(
            merged,
            "https://a.example/x\n  out=x.bin\nhttps://b.example/y\n  checksum=md5=ff"
        );
    }

    /// TXT 导入不得改写已有文本：aria2c 命令（及其 Cookie 等头部信息）原样保留。
    #[test]
    fn import_keeps_existing_aria2c_command_and_its_hints() {
        let existing = "aria2c -x16 \\\n  --header 'Cookie: sid=abc' \\\n  -o a.zip 'https://a.example/a?t=1&u=2'";
        let imported = vec![
            UrlEntry {
                url: "https://b.example/b.zip".to_owned(),
                ..UrlEntry::default()
            },
            UrlEntry {
                url: "https://a.example/a?t=1&u=2".to_owned(),
                ..UrlEntry::default()
            },
        ];
        let text = append_entries(existing, imported);
        assert_eq!(text, format!("{existing}\nhttps://b.example/b.zip"));
        let parsed = parse_input(&text, false);
        assert_eq!(parsed.hints.cookie, "sid=abc");
        let urls: Vec<_> = parsed.entries.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(
            urls,
            ["https://a.example/a?t=1&u=2", "https://b.example/b.zip"]
        );
        assert_eq!(parsed.entries[0].file_name, "a.zip");
    }

    #[test]
    fn proxy_choice_wire_values_match_dart() {
        assert_eq!(ProxyChoice::FollowGlobal.wire("http://m:1", "x"), "");
        assert_eq!(ProxyChoice::Direct.wire("", ""), "direct://");
        assert_eq!(ProxyChoice::System.wire("", ""), "system://");
        assert_eq!(
            ProxyChoice::GlobalManual.wire("socks5://m:1080", ""),
            "socks5://m:1080"
        );
        assert_eq!(
            ProxyChoice::Custom.wire("", "  http://c:8080 "),
            "http://c:8080"
        );
    }

    #[test]
    fn manual_proxy_url_encodes_userinfo_and_requires_host_port() {
        let mut config = BTreeMap::new();
        assert_eq!(manual_proxy_url(&config), "");
        config.insert("proxy_host".to_owned(), " 127.0.0.1 ".to_owned());
        config.insert("proxy_port".to_owned(), "1080".to_owned());
        assert_eq!(manual_proxy_url(&config), "http://127.0.0.1:1080");
        config.insert("proxy_type".to_owned(), "socks5".to_owned());
        config.insert("proxy_username".to_owned(), "us er".to_owned());
        config.insert("proxy_password".to_owned(), "p@ss".to_owned());
        assert_eq!(
            manual_proxy_url(&config),
            "socks5://us%20er:p%40ss@127.0.0.1:1080"
        );
        config.insert("proxy_port".to_owned(), "0".to_owned());
        assert_eq!(manual_proxy_url(&config), "");
    }

    #[test]
    fn ua_thread_and_checksum_helpers() {
        assert_eq!(detect_ua_preset(""), "default");
        assert_eq!(detect_ua_preset(super::UA_PRESETS[1].1), "firefox");
        assert_eq!(detect_ua_preset("FluxDown/1.0"), "custom");
        assert_eq!(ThreadChoice::from_segments(0), ThreadChoice::Auto);
        assert_eq!(ThreadChoice::from_segments(16), ThreadChoice::Preset(16));
        assert_eq!(ThreadChoice::from_segments(10), ThreadChoice::Custom);
        assert_eq!(custom_segments("999"), 512);
        assert_eq!(custom_segments("0"), 0);
        assert_eq!(custom_segments("abc"), 0);
        assert_eq!(checksum_spec("md5", "  "), "");
        assert_eq!(checksum_spec("sha-1", " abc "), "sha-1=abc");
    }

    #[test]
    fn single_request_applies_rename_checksum_and_auth_but_batch_does_not() {
        let options = DraftOptions {
            save_dir: "/downloads".to_owned(),
            segments: 8,
            queue_id: "later".to_owned(),
            checksum: "sha-256=deadbeef".to_owned(),
            headers: HashMap::from([("X-Test".to_owned(), "1".to_owned())]),
            start_paused: true,
            rename: "renamed.bin".to_owned(),
            http_user: "u".to_owned(),
            http_password: "p".to_owned(),
            save_site_auth: true,
            ..DraftOptions::default()
        };
        let entry = UrlEntry {
            url: "https://a.example/x".to_owned(),
            file_name: "out.bin".to_owned(),
            checksum: "md5=ff".to_owned(),
        };
        let single = build_requests(std::slice::from_ref(&entry), &options);
        assert_eq!(single.items.len(), 1);
        assert_eq!(single.items[0].file_name, "renamed.bin");
        assert_eq!(single.items[0].checksum, "sha-256=deadbeef");
        assert_eq!(single.common.http_user, "u");
        assert!(single.common.save_site_auth);
        assert!(single.common.start_paused);
        assert_eq!(single.common.queue_id, "later");
        assert_eq!(
            single.common.headers.as_ref().and_then(|h| h.get("X-Test")),
            Some(&"1".to_owned())
        );
        // 共享选项里不带逐条字段，条目还原后才是完整请求。
        assert!(single.common.url.is_empty());
        assert!(single.common.file_name.is_empty());
        assert!(single.common.checksum.is_empty());
        let full = single.request_for(&single.items[0]);
        assert_eq!(full.url, "https://a.example/x");
        assert_eq!(full.file_name, "renamed.bin");
        assert_eq!(full.checksum, "sha-256=deadbeef");

        let batch = build_requests(&[entry.clone(), entry], &options);
        assert_eq!(batch.items.len(), 2);
        assert_eq!(batch.items[0].file_name, "out.bin");
        assert_eq!(batch.items[0].checksum, "md5=ff");
        assert_eq!(batch.common.http_user, "");
        assert!(!batch.common.save_site_auth);
        assert_eq!(batch.common.segments, 8);
    }

    #[test]
    fn empty_headers_serialize_as_absent() {
        let requests = build_requests(
            &[UrlEntry {
                url: "https://a.example/x".to_owned(),
                ..UrlEntry::default()
            }],
            &DraftOptions::default(),
        );
        assert!(requests.common.headers.is_none());
    }
}
