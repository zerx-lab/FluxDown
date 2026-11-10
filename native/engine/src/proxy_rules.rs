//! 站点代理规则：按网站（域名）或正则（完整网址）为请求指定出口。
//!
//! 配置键 [`PROXY_RULES_KEY`] 的值是 JSON 数组，数组顺序即优先级，第一条命中的
//! 启用规则生效。本模块是匹配的**唯一实现**；客户端只经 `daemon.config.proxyRoute`
//! 询问（[`validate`] + [`explain`]）。宽松解析与 `fluxdown_protocol::proxy_rules`、
//! Web / Android / iOS 解析器逐条一致：空串 / 非法 JSON / 非数组 → 空列表，非对象
//! 元素跳过但保留数组下标，字段缺失或类型错误回退默认值。
//!
//! 作用范围：只有跟随全局（`tasks.proxy_url == ""`）的任务、未自带代理的 RSS
//! 订阅、插件请求与宿主自身下载才看规则；scheme 只认 http/https/ftp/ftps，
//! 环回地址永不匹配（保持「环回恒直连」不变式）。

use std::collections::HashSet;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};

use regex::{Regex, RegexBuilder};
use serde_json::Value;
use url::{Host, Url};

use crate::auto_proxy::route;
use crate::logger::log_info;
use crate::proxy_config::{
    ProxyConfig, ProxyMode, detect_system_proxy, host_matches_no_proxy, is_loopback_bypassed,
    proxy_bypass_list, system_pac_url,
};

/// daemon 配置键。
pub const PROXY_RULES_KEY: &str = "proxy_rules";
/// 规则条数上限。
pub const MAX_PROXY_RULES: usize = 200;
/// `pattern` 字符数上限。
pub const MAX_PATTERN_CHARS: usize = 2048;
/// `name` 字符数上限。
pub const MAX_NAME_CHARS: usize = 64;
/// `proxy` 字符数上限。
pub const MAX_PROXY_URL_CHARS: usize = 2048;
/// 单条正则的编译体积上限（1 MiB）。
pub const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// 参与规则匹配的 URL scheme；其余（magnet / ed2k / `torrent-file://` …）永不匹配。
pub const ROUTABLE_SCHEMES: [&str; 4] = ["http", "https", "ftp", "ftps"];
/// 自定义代理允许的 scheme（与 [`crate::proxy_config::ProxyType::parse_str`] 的识别集一致）。
const PROXY_SCHEMES: [&str; 6] = ["http", "https", "socks4", "socks4a", "socks5", "socks5h"];
/// 正则错误文本截断长度（字符）。
const REGEX_ERROR_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// 宽松解析
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchKind {
    Domain,
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Direct,
    System,
    Manual,
    Custom,
}

impl MatchKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "domain" => Some(Self::Domain),
            "regex" => Some(Self::Regex),
            _ => None,
        }
    }
}

impl Action {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "system" => Some(Self::System),
            "manual" => Some(Self::Manual),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawRule {
    id: String,
    enabled: bool,
    name: String,
    match_kind: MatchKind,
    pattern: String,
    action: Action,
    proxy: String,
}

fn str_field(obj: &serde_json::Map<String, Value>, key: &str) -> String {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default()
}

/// 宽松解析：返回 `(数组下标, 规则)`；非对象元素跳过但其下标不复用。
fn parse_raw(raw: &str) -> Vec<(usize, RawRule)> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let obj = item.as_object()?;
            Some((
                index,
                RawRule {
                    id: str_field(obj, "id"),
                    enabled: obj.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                    name: str_field(obj, "name"),
                    match_kind: obj
                        .get("match")
                        .and_then(Value::as_str)
                        .and_then(MatchKind::parse)
                        .unwrap_or(MatchKind::Domain),
                    pattern: str_field(obj, "pattern"),
                    action: obj
                        .get("action")
                        .and_then(Value::as_str)
                        .and_then(Action::parse)
                        .unwrap_or(Action::Direct),
                    proxy: str_field(obj, "proxy"),
                },
            ))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 匹配器
// ---------------------------------------------------------------------------

enum DomainEntry {
    /// 域名后缀（含自身）：`github.com` 命中 `github.com` 与 `api.github.com`。
    Suffix(String),
    /// 精确 IP（规范化文本，IPv6 不带方括号）。
    Exact(String),
}

enum RuleMatcher {
    Domains(Vec<DomainEntry>),
    Regex(Regex),
}

/// 匹配器编译失败：`(field, code, detail)`，与严格校验表一致。
type MatcherError = (&'static str, &'static str, String);

fn compile_matcher(kind: MatchKind, pattern: &str) -> Result<RuleMatcher, MatcherError> {
    if pattern.trim().is_empty() {
        return Err(("pattern", "emptyPattern", String::new()));
    }
    if pattern.chars().count() > MAX_PATTERN_CHARS {
        return Err(("pattern", "tooLong", MAX_PATTERN_CHARS.to_string()));
    }
    match kind {
        MatchKind::Domain => parse_domain_entries(pattern)
            .map(RuleMatcher::Domains)
            .map_err(|bad| ("pattern", "invalidDomain", bad)),
        MatchKind::Regex => RegexBuilder::new(pattern)
            .case_insensitive(true)
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map(RuleMatcher::Regex)
            .map_err(|e| match e {
                regex::Error::CompiledTooBig(_) => ("pattern", "regexTooLarge", String::new()),
                other => ("pattern", "invalidRegex", regex_error_text(&other)),
            }),
    }
}

fn regex_error_text(error: &regex::Error) -> String {
    error
        .to_string()
        .replace(['\r', '\n'], " ")
        .chars()
        .take(REGEX_ERROR_CHARS)
        .collect()
}

/// 按 `,` / `;` / 空白切分域名条目；失败返回第一个非法条目原文。
fn parse_domain_entries(pattern: &str) -> Result<Vec<DomainEntry>, String> {
    let mut entries = Vec::new();
    for raw in pattern
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|s| !s.is_empty())
    {
        entries.push(parse_domain_entry(raw).ok_or_else(|| raw.to_string())?);
    }
    if entries.is_empty() {
        return Err(pattern.trim().to_string());
    }
    Ok(entries)
}

fn parse_domain_entry(raw: &str) -> Option<DomainEntry> {
    let entry = raw.strip_prefix("*.").unwrap_or(raw);
    let entry = entry.trim_start_matches('.').trim_end_matches('.');
    if entry.is_empty() {
        return None;
    }
    if let Some(inner) = entry.strip_prefix('[').and_then(|e| e.strip_suffix(']')) {
        return inner
            .parse::<Ipv6Addr>()
            .ok()
            .map(|ip| DomainEntry::Exact(ip.to_string()));
    }
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return Some(DomainEntry::Exact(ip.to_string()));
    }
    // `github.com:443` 也在此拦下：`Url::port()` 对默认端口返回 None，靠解析结果查不出。
    if entry.contains([':', '/', '?', '#', '@']) {
        return None;
    }
    let url = Url::parse(&format!("http://{entry}/")).ok()?;
    match url.host()? {
        Host::Domain(domain) => Some(DomainEntry::Suffix(domain.to_string())),
        Host::Ipv4(ip) => Some(DomainEntry::Exact(ip.to_string())),
        Host::Ipv6(ip) => Some(DomainEntry::Exact(ip.to_string())),
    }
}

/// URL 主机的规范文本（小写 punycode / IP 文本，IPv6 不带方括号）。
fn canonical_host(url: &Url) -> Option<String> {
    Some(match url.host()? {
        Host::Domain(domain) => domain.to_string(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => ip.to_string(),
    })
}

impl RuleMatcher {
    fn is_match(&self, url: &Url, host: &str) -> bool {
        match self {
            Self::Domains(entries) => entries.iter().any(|entry| match entry {
                DomainEntry::Exact(ip) => host == ip,
                DomainEntry::Suffix(domain) => {
                    host == domain
                        || host
                            .strip_suffix(domain.as_str())
                            .is_some_and(|prefix| prefix.ends_with('.'))
                }
            }),
            Self::Regex(regex) => regex.is_match(url.as_str()),
        }
    }
}

// ---------------------------------------------------------------------------
// 自定义代理地址严格解析
// ---------------------------------------------------------------------------

/// 自定义代理 URL 的严格检查：scheme 在 [`PROXY_SCHEMES`] 内、有 host、有显式非零
/// 端口、无路径。失败返回原因短语（wire `detail`）。
fn check_proxy_url(raw: &str) -> Result<(), String> {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Err("missing scheme".to_string());
    };
    if !PROXY_SCHEMES.contains(&scheme) {
        return Err(format!("unsupported scheme {scheme}"));
    }
    let host_port = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    if host_port.contains(['/', '?', '#']) {
        return Err("unexpected path".to_string());
    }
    let (host, port) = if let Some(after) = host_port.strip_prefix('[') {
        let Some((inner, tail)) = after.split_once(']') else {
            return Err("invalid host".to_string());
        };
        if inner.parse::<Ipv6Addr>().is_err() {
            return Err("invalid host".to_string());
        }
        (inner, tail.strip_prefix(':'))
    } else {
        match host_port.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        }
    };
    if host.is_empty() {
        return Err("missing host".to_string());
    }
    if !host_port.starts_with('[')
        && (host.contains(':') || Url::parse(&format!("http://{host}/")).is_err())
    {
        return Err("invalid host".to_string());
    }
    match port.filter(|p| !p.is_empty()) {
        None => Err("missing port".to_string()),
        Some(port) => match port.parse::<u16>() {
            Ok(0) => Err("missing port".to_string()),
            Ok(_) => Ok(()),
            Err(_) => Err("invalid port".to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// 规则集
// ---------------------------------------------------------------------------

/// 规则出口（未解析）。`System` 需在调用时检测系统代理（阻塞），其余是现成配置。
#[derive(Clone)]
pub(crate) enum RuleEgress {
    Direct,
    System,
    /// 全局手动字段组成的配置；字段不全（host 空或 port 0）为 `None` = 直连。
    Manual(Option<ProxyConfig>),
    Custom(ProxyConfig),
}

impl RuleEgress {
    /// 解析为具体出口配置（模式只会是 `Manual` 或 `None`，恒不携带规则与排除列表）。
    /// `System` 会阻塞检测系统代理，async 调用方须放进 `spawn_blocking`。
    pub(crate) fn resolve(&self) -> ProxyConfig {
        match self {
            Self::Direct | Self::Manual(None) => ProxyConfig::default(),
            Self::System => match detect_system_proxy() {
                Ok(Some(mut proxy)) => {
                    // 命中规则后不再看排除列表；环回直连由 `proxy_bypass_list` 保证。
                    proxy.no_proxy_list.clear();
                    proxy.rules = None;
                    proxy
                }
                Ok(None) => ProxyConfig::default(),
                Err(e) => {
                    log_info!("[proxy-rules] system proxy detection failed: {}", e);
                    ProxyConfig::default()
                }
            },
            Self::Manual(Some(proxy)) | Self::Custom(proxy) => proxy.clone(),
        }
    }

    /// 解析是否需要检测系统代理（只有 `System`）。
    pub(crate) fn needs_detection(&self) -> bool {
        matches!(self, Self::System)
    }

    /// 两个规则出口是否等价：`Direct` 与 `Manual(None)` 都是直连；`System` 只看系统代理，
    /// 与全局设置无关；现成配置逐字段比较端点与凭据。
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Direct | Self::Manual(None), Self::Direct | Self::Manual(None))
            | (Self::System, Self::System) => true,
            (Self::Manual(Some(a)) | Self::Custom(a), Self::Manual(Some(b)) | Self::Custom(b)) => {
                a.mode == b.mode
                    && a.proxy_type == b.proxy_type
                    && a.host == b.host
                    && a.port == b.port
                    && a.username == b.username
                    && a.password == b.password
            }
            _ => false,
        }
    }
}

/// 一条已编译的启用规则。
pub struct CompiledRule {
    index: usize,
    id: String,
    name: String,
    matcher: RuleMatcher,
    egress: RuleEgress,
}

impl CompiledRule {
    /// 规则在配置数组中的下标（从 0 起）。
    pub fn index(&self) -> usize {
        self.index
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn egress(&self) -> &RuleEgress {
        &self.egress
    }

    /// 本规则决定任务出口时写入 `tasks.auto_route` 的标签；`resolved` 是
    /// [`RuleEgress::resolve`] 的结果（直连一律 `rule:direct`）。
    pub(crate) fn route_label(&self, resolved: &ProxyConfig) -> &'static str {
        if resolved.mode == ProxyMode::None {
            return route::RULE_DIRECT;
        }
        match self.egress {
            RuleEgress::Direct => route::RULE_DIRECT,
            RuleEgress::System => route::RULE_SYSTEM,
            RuleEgress::Manual(_) => route::RULE_MANUAL,
            RuleEgress::Custom(_) => route::RULE_CUSTOM,
        }
    }
}

/// 已编译的启用规则集（按优先级排列）。无效规则在编译时跳过并记日志。
pub struct ProxyRuleSet {
    rules: Vec<CompiledRule>,
}

impl std::fmt::Debug for ProxyRuleSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyRuleSet")
            .field("enabled", &self.rules.len())
            .finish()
    }
}

impl ProxyRuleSet {
    /// 编译配置值；`manual` 是全局手动字段组成的 `Manual` 配置（供 `manual` 动作）。
    pub fn compile(raw: &str, manual: &ProxyConfig) -> Self {
        let manual_egress = (!manual.host.is_empty() && manual.port != 0).then(|| ProxyConfig {
            mode: ProxyMode::Manual,
            no_proxy_list: String::new(),
            rules: None,
            ..manual.clone()
        });
        let mut rules = Vec::new();
        for (index, rule) in parse_raw(raw) {
            if !rule.enabled {
                continue;
            }
            let matcher = match compile_matcher(rule.match_kind, &rule.pattern) {
                Ok(matcher) => matcher,
                Err((_, code, _)) => {
                    log_info!("[proxy-rules] skip rule #{}: {}", index, code);
                    continue;
                }
            };
            let egress = match rule.action {
                Action::Direct => RuleEgress::Direct,
                Action::System => RuleEgress::System,
                Action::Manual => RuleEgress::Manual(manual_egress.clone()),
                Action::Custom => {
                    if rule.proxy.chars().count() > MAX_PROXY_URL_CHARS
                        || check_proxy_url(&rule.proxy).is_err()
                    {
                        log_info!("[proxy-rules] skip rule #{}: invalidProxy", index);
                        continue;
                    }
                    RuleEgress::Custom(ProxyConfig::from_proxy_url(&rule.proxy))
                }
            };
            rules.push(CompiledRule {
                index,
                id: rule.id,
                name: rule.name,
                matcher,
                egress,
            });
        }
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// 启用且有效的规则条数。
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// 第一条命中 `url` 的规则；URL 无法解析时 `None`。
    pub fn first_match(&self, url: &str) -> Option<&CompiledRule> {
        let parsed = Url::parse(url).ok()?;
        self.first_match_url(&parsed)
    }

    /// 见 [`Self::first_match`]：不可路由 scheme 与环回主机永不匹配。
    pub(crate) fn first_match_url(&self, url: &Url) -> Option<&CompiledRule> {
        if self.rules.is_empty() || !ROUTABLE_SCHEMES.contains(&url.scheme()) {
            return None;
        }
        let host = canonical_host(url)?;
        if is_loopback_bypassed(&host) {
            return None;
        }
        self.rules
            .iter()
            .find(|rule| rule.matcher.is_match(url, &host))
    }
}

/// 单槽编译缓存：`Egress::proxy()`、插件市场、patch 预检都会反复 `from_config_map`，
/// 每次重编最多 200 条正则代价太高。键 = 原文 + 手动字段。
static LAST: Mutex<Option<(String, Arc<ProxyRuleSet>)>> = Mutex::new(None);

/// 从配置 map 取出并编译规则（带缓存）；键缺失、值为空或无一条有效启用规则时 `None`。
pub(crate) fn rules_from_config_map(
    map: &std::collections::HashMap<String, String>,
    manual: &ProxyConfig,
) -> Option<Arc<ProxyRuleSet>> {
    let raw = map.get(PROXY_RULES_KEY)?;
    if raw.trim().is_empty() {
        return None;
    }
    let key = format!(
        "{raw}\u{1f}{}|{}|{}|{}|{}",
        manual.proxy_type.as_str(),
        manual.host,
        manual.port,
        manual.username,
        manual.password
    );
    let cached = LAST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .filter(|(cached_key, _)| *cached_key == key)
        .map(|(_, set)| Arc::clone(set));
    let set = match cached {
        Some(set) => set,
        None => {
            let set = Arc::new(ProxyRuleSet::compile(raw, manual));
            *LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some((key, Arc::clone(&set)));
            set
        }
    };
    (!set.is_empty()).then_some(set)
}

// ---------------------------------------------------------------------------
// 严格校验
// ---------------------------------------------------------------------------

/// 一条严格校验问题；`index` 为数组下标，`field` 为空表示整表 / 整元素问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleIssue {
    pub index: usize,
    pub field: &'static str,
    pub code: &'static str,
    pub detail: String,
}

impl RuleIssue {
    fn new(index: usize, field: &'static str, code: &'static str, detail: String) -> Self {
        Self {
            index,
            field,
            code,
            detail,
        }
    }

    /// 英文错误文本（daemon 写入拒绝时的 `InvalidArgument` 消息）。
    pub fn message(&self) -> String {
        let head = if self.field.is_empty() {
            format!("proxy_rules: {}", self.code)
        } else {
            format!("proxy_rules[{}].{}: {}", self.index, self.field, self.code)
        };
        if self.detail.is_empty() {
            head
        } else {
            format!("{head}: {}", self.detail)
        }
    }
}

fn raw_text(value: Option<&Value>) -> String {
    match value {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// 严格校验配置值（disabled 规则同样校验）；空串无问题。
pub fn validate(raw: &str) -> Vec<RuleIssue> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    let items = match serde_json::from_str::<Value>(raw) {
        Ok(Value::Array(items)) => items,
        Ok(_) => {
            return vec![RuleIssue::new(
                0,
                "",
                "invalidJson",
                "expected a JSON array".to_string(),
            )];
        }
        Err(e) => return vec![RuleIssue::new(0, "", "invalidJson", e.to_string())],
    };
    let mut issues = Vec::new();
    if items.len() > MAX_PROXY_RULES {
        issues.push(RuleIssue::new(
            0,
            "",
            "tooManyRules",
            MAX_PROXY_RULES.to_string(),
        ));
    }
    let mut ids = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            issues.push(RuleIssue::new(index, "", "notObject", String::new()));
            continue;
        };
        match obj.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => {
                if !ids.insert(id) {
                    issues.push(RuleIssue::new(index, "id", "duplicateId", id.to_string()));
                }
            }
            _ => issues.push(RuleIssue::new(index, "id", "missingId", String::new())),
        }
        if let Some(name) = obj.get("name").and_then(Value::as_str)
            && name.chars().count() > MAX_NAME_CHARS
        {
            issues.push(RuleIssue::new(
                index,
                "name",
                "tooLong",
                MAX_NAME_CHARS.to_string(),
            ));
        }
        let match_kind = obj
            .get("match")
            .and_then(Value::as_str)
            .and_then(MatchKind::parse);
        let pattern = obj.get("pattern").and_then(Value::as_str).unwrap_or("");
        match match_kind {
            None => {
                issues.push(RuleIssue::new(
                    index,
                    "match",
                    "invalidMatch",
                    raw_text(obj.get("match")),
                ));
                if pattern.trim().is_empty() {
                    issues.push(RuleIssue::new(
                        index,
                        "pattern",
                        "emptyPattern",
                        String::new(),
                    ));
                }
            }
            Some(kind) => {
                if let Err((field, code, detail)) = compile_matcher(kind, pattern) {
                    issues.push(RuleIssue::new(index, field, code, detail));
                }
            }
        }
        let action = obj
            .get("action")
            .and_then(Value::as_str)
            .and_then(Action::parse);
        if action.is_none() {
            issues.push(RuleIssue::new(
                index,
                "action",
                "invalidAction",
                raw_text(obj.get("action")),
            ));
        }
        let proxy = obj.get("proxy").and_then(Value::as_str).unwrap_or("");
        if proxy.chars().count() > MAX_PROXY_URL_CHARS {
            issues.push(RuleIssue::new(
                index,
                "proxy",
                "tooLong",
                MAX_PROXY_URL_CHARS.to_string(),
            ));
        } else if action == Some(Action::Custom) {
            if proxy.is_empty() {
                issues.push(RuleIssue::new(
                    index,
                    "proxy",
                    "invalidProxy",
                    "missing".to_string(),
                ));
            } else if let Err(reason) = check_proxy_url(proxy) {
                issues.push(RuleIssue::new(index, "proxy", "invalidProxy", reason));
            }
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// 解释（「测试网址」）
// ---------------------------------------------------------------------------

/// 命中的规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedRule {
    pub index: usize,
    pub id: String,
    pub name: String,
}

/// 某个网址最终走哪条出口的原因（wire 值见 [`Self::as_str`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteReason {
    Direct,
    Proxy,
    SystemUndetected,
    ManualIncomplete,
    Loopback,
    Unsupported,
    ModeNone,
    Bypass,
    Pac,
    Auto,
    InvalidUrl,
}

impl RouteReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Proxy => "proxy",
            Self::SystemUndetected => "systemUndetected",
            Self::ManualIncomplete => "manualIncomplete",
            Self::Loopback => "loopback",
            Self::Unsupported => "unsupported",
            Self::ModeNone => "modeNone",
            Self::Bypass => "bypass",
            Self::Pac => "pac",
            Self::Auto => "auto",
            Self::InvalidUrl => "invalidUrl",
        }
    }
}

/// [`explain`] 的结论；`endpoint` 仅 `Proxy` 时非空，形如 `socks5://host:port`，绝不带凭据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteExplanation {
    pub matched: Option<MatchedRule>,
    pub reason: RouteReason,
    pub endpoint: String,
}

fn endpoint_of(proxy: &ProxyConfig) -> String {
    let host = if proxy.host.contains(':') && !proxy.host.starts_with('[') {
        format!("[{}]", proxy.host)
    } else {
        proxy.host.clone()
    };
    format!("{}://{}:{}", proxy.proxy_type.as_str(), host, proxy.port)
}

fn via(proxy: &ProxyConfig, matched: Option<MatchedRule>) -> RouteExplanation {
    RouteExplanation {
        matched,
        reason: RouteReason::Proxy,
        endpoint: endpoint_of(proxy),
    }
}

fn because(reason: RouteReason, matched: Option<MatchedRule>) -> RouteExplanation {
    RouteExplanation {
        matched,
        reason,
        endpoint: String::new(),
    }
}

/// 解释 `url` 在全局配置 `config`（含其 `rules`）下的出口。会阻塞（检测系统代理）。
pub fn explain(config: &ProxyConfig, url: &str) -> RouteExplanation {
    let Ok(parsed) = Url::parse(url.trim()) else {
        return because(RouteReason::InvalidUrl, None);
    };
    if !ROUTABLE_SCHEMES.contains(&parsed.scheme()) {
        return because(RouteReason::Unsupported, None);
    }
    let Some(host) = canonical_host(&parsed) else {
        return because(RouteReason::InvalidUrl, None);
    };
    if is_loopback_bypassed(&host) {
        return because(RouteReason::Loopback, None);
    }
    if let Some(rule) = config
        .rules
        .as_ref()
        .and_then(|rules| rules.first_match_url(&parsed))
    {
        let matched = Some(MatchedRule {
            index: rule.index,
            id: rule.id.clone(),
            name: rule.name.clone(),
        });
        return match &rule.egress {
            RuleEgress::Direct => because(RouteReason::Direct, matched),
            RuleEgress::Manual(None) => because(RouteReason::ManualIncomplete, matched),
            RuleEgress::Manual(Some(proxy)) | RuleEgress::Custom(proxy) => via(proxy, matched),
            RuleEgress::System => {
                let resolved = rule.egress.resolve();
                if resolved.mode == ProxyMode::None {
                    because(RouteReason::SystemUndetected, matched)
                } else {
                    via(&resolved, matched)
                }
            }
        };
    }
    match config.mode {
        ProxyMode::None => because(RouteReason::ModeNone, None),
        ProxyMode::Auto => because(RouteReason::Auto, None),
        ProxyMode::Manual => {
            if config.host.is_empty() || config.port == 0 {
                because(RouteReason::ManualIncomplete, None)
            } else if host_matches_no_proxy(&host, &proxy_bypass_list(&config.no_proxy_list)) {
                because(RouteReason::Bypass, None)
            } else {
                via(config, None)
            }
        }
        ProxyMode::System => match detect_system_proxy() {
            Ok(Some(system)) => {
                if host_matches_no_proxy(&host, &proxy_bypass_list(&system.no_proxy_list)) {
                    because(RouteReason::Bypass, None)
                } else {
                    via(&system, None)
                }
            }
            Ok(None) | Err(_) => {
                if system_pac_url().is_some() {
                    because(RouteReason::Pac, None)
                } else {
                    because(RouteReason::SystemUndetected, None)
                }
            }
        },
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::proxy_config::ProxyType;
    use std::collections::HashMap;

    fn manual_fields() -> ProxyConfig {
        ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Http,
            host: "10.0.0.2".into(),
            port: 3128,
            ..ProxyConfig::default()
        }
    }

    fn set(raw: &str) -> ProxyRuleSet {
        ProxyRuleSet::compile(raw, &manual_fields())
    }

    fn hit(rules: &ProxyRuleSet, url: &str) -> Option<usize> {
        rules.first_match(url).map(CompiledRule::index)
    }

    fn domain(pattern: &str) -> String {
        serde_json::json!([{"id":"a","match":"domain","pattern":pattern,"action":"direct"}])
            .to_string()
    }

    fn codes(raw: &str) -> Vec<(usize, &'static str, &'static str)> {
        validate(raw)
            .into_iter()
            .map(|i| (i.index, i.field, i.code))
            .collect()
    }

    #[test]
    fn lenient_parse_defaults_and_keeps_indices() {
        let raw =
            r#"[1, {"id":"a","enabled":"x","match":"glob","action":"tunnel","pattern":3}, {}]"#;
        let parsed = parse_raw(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, 1);
        let rule = &parsed[0].1;
        assert_eq!(rule.id, "a");
        assert!(rule.enabled);
        assert_eq!(rule.match_kind, MatchKind::Domain);
        assert_eq!(rule.action, Action::Direct);
        assert_eq!(rule.pattern, "");
        assert_eq!(parsed[1].0, 2);
        for raw in ["", "{", "{}", "\"x\""] {
            assert!(parse_raw(raw).is_empty());
        }
        let rules = set(r#"["skip", {"id":"b","pattern":"a.com"}]"#);
        assert_eq!(hit(&rules, "https://a.com/"), Some(1));
    }

    #[test]
    fn domain_suffix_prefix_idn_and_ip() {
        let rules = set(&domain(
            "*.github.com, .example.org; bücher.de 10.1.2.3 [2001:db8::1]",
        ));
        assert_eq!(hit(&rules, "https://github.com/a"), Some(0));
        assert_eq!(hit(&rules, "https://API.GitHub.com/a"), Some(0));
        assert_eq!(hit(&rules, "http://x.example.org/"), Some(0));
        assert_eq!(hit(&rules, "https://xn--bcher-kva.de/"), Some(0));
        assert_eq!(hit(&rules, "https://bücher.de/"), Some(0));
        assert_eq!(hit(&rules, "ftp://10.1.2.3/f"), Some(0));
        assert_eq!(hit(&rules, "http://[2001:db8::1]:8080/"), Some(0));
        assert_eq!(hit(&rules, "https://evilgithub.com/"), None);
        assert_eq!(hit(&rules, "https://10.1.2.30/"), None);
    }

    #[test]
    fn regex_is_case_insensitive_and_sees_path_and_query() {
        let rules = set(
            &serde_json::json!([{"id":"r","match":"regex","pattern":"/DL/.*token=","action":"direct"}])
                .to_string(),
        );
        assert_eq!(hit(&rules, "https://a.test/dl/x?token=1"), Some(0));
        assert_eq!(hit(&rules, "https://a.test/dl/x"), None);
    }

    #[test]
    fn first_enabled_match_wins_and_exclusions_hold() {
        let raw = serde_json::json!([
            {"id":"a","enabled":false,"match":"domain","pattern":"a.com","action":"direct"},
            {"id":"b","match":"regex","pattern":"a\\.com","action":"system"},
            {"id":"c","match":"domain","pattern":"a.com","action":"direct"},
            {"id":"d","match":"domain","pattern":"localhost 127.0.0.1","action":"direct"}
        ])
        .to_string();
        let rules = set(&raw);
        assert_eq!(rules.len(), 3);
        assert_eq!(hit(&rules, "https://a.com/"), Some(1));
        assert_eq!(hit(&rules, "http://localhost/"), None);
        assert_eq!(hit(&rules, "http://127.0.0.1/"), None);
        assert_eq!(hit(&rules, "magnet:?xt=urn:btih:a.com"), None);
        assert_eq!(hit(&rules, "not a url"), None);
    }

    #[test]
    fn invalid_domain_entries() {
        for bad in [
            "github.com:443",
            "a.com?x",
            "a.com/x",
            "u@a.com",
            "[zz]",
            "*.",
        ] {
            assert_eq!(
                codes(&domain(bad)),
                vec![(0, "pattern", "invalidDomain")],
                "{bad}"
            );
        }
        assert!(codes(&domain("[::1] ::1 1.2.3.4")).is_empty());
        let rules = set(&domain("[::2]"));
        assert_eq!(hit(&rules, "http://[::2]/"), Some(0));
        assert_eq!(hit(&rules, "http://[::3]/"), None);
    }

    #[test]
    fn validation_table() {
        assert_eq!(codes(""), vec![]);
        assert_eq!(codes("{"), vec![(0, "", "invalidJson")]);
        assert_eq!(codes("{}"), vec![(0, "", "invalidJson")]);
        let many = serde_json::Value::Array(
            (0..201)
                .map(|i| serde_json::json!({"id":format!("r{i}"),"match":"domain","pattern":"a.com","action":"direct"}))
                .collect(),
        )
        .to_string();
        assert_eq!(codes(&many), vec![(0, "", "tooManyRules")]);
        let ok = r#"{"id":"x","match":"domain","pattern":"a.com","action":"direct"}"#;
        assert_eq!(codes(&format!("[1,{ok}]")), vec![(0, "", "notObject")]);
        assert_eq!(
            codes(r#"[{"match":"domain","pattern":"a.com","action":"direct"}]"#),
            vec![(0, "id", "missingId")]
        );
        assert_eq!(
            codes(&format!("[{ok},{ok}]")),
            vec![(1, "id", "duplicateId")]
        );
        let long_name = "n".repeat(65);
        assert_eq!(
            codes(&format!(
                r#"[{{"id":"x","name":"{long_name}","match":"domain","pattern":"a.com","action":"direct"}}]"#
            )),
            vec![(0, "name", "tooLong")]
        );
        assert_eq!(
            codes(r#"[{"id":"x","match":"glob","pattern":"a.com","action":"direct"}]"#),
            vec![(0, "match", "invalidMatch")]
        );
        assert_eq!(
            codes(r#"[{"id":"x","match":"domain","pattern":"  ","action":"direct"}]"#),
            vec![(0, "pattern", "emptyPattern")]
        );
        let long_pattern = "a".repeat(2049);
        assert_eq!(
            codes(&domain(&long_pattern)),
            vec![(0, "pattern", "tooLong")]
        );
        let issues = validate(r#"[{"id":"x","match":"regex","pattern":"(","action":"direct"}]"#);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "invalidRegex");
        assert!(!issues[0].detail.contains('\n'));
        assert!(issues[0].detail.chars().count() <= 200);
        assert_eq!(
            codes(
                r#"[{"id":"x","match":"regex","pattern":"\\w{1000}\\w{1000}","action":"direct"}]"#
            ),
            vec![(0, "pattern", "regexTooLarge")]
        );
        assert_eq!(
            codes(r#"[{"id":"x","match":"domain","pattern":"a.com","action":"tunnel"}]"#),
            vec![(0, "action", "invalidAction")]
        );
        assert_eq!(
            codes(
                r#"[{"id":"x","match":"domain","pattern":"a.com","action":"custom","proxy":""}]"#
            ),
            vec![(0, "proxy", "invalidProxy")]
        );
        let long_proxy = format!("http://{}:1", "h".repeat(2050));
        assert_eq!(
            codes(&format!(
                r#"[{{"id":"x","match":"domain","pattern":"a.com","action":"custom","proxy":"{long_proxy}"}}]"#
            )),
            vec![(0, "proxy", "tooLong")]
        );
        for (proxy, reason) in [
            ("ftp://h:1", "unsupported scheme ftp"),
            ("socks5://:1", "missing host"),
            ("socks5://h", "missing port"),
            ("socks5://h:0", "missing port"),
            ("http://h:80/", "unexpected path"),
            ("h:1", "missing scheme"),
        ] {
            let issues = validate(&format!(
                r#"[{{"id":"x","match":"domain","pattern":"a.com","action":"custom","proxy":"{proxy}"}}]"#
            ));
            assert_eq!(issues.len(), 1, "{proxy}");
            assert_eq!(issues[0].code, "invalidProxy");
            assert_eq!(issues[0].detail, reason, "{proxy}");
        }
        assert!(
            codes(r#"[{"id":"x","enabled":false,"match":"domain","pattern":"a.com","action":"custom","proxy":"socks5://u:p@[::1]:1080"}]"#)
                .is_empty()
        );
        // disabled 规则同样校验
        assert_eq!(
            codes(
                r#"[{"id":"x","enabled":false,"match":"domain","pattern":"","action":"direct"}]"#
            ),
            vec![(0, "pattern", "emptyPattern")]
        );
    }

    #[test]
    fn message_format() {
        let issue = RuleIssue::new(2, "pattern", "invalidDomain", "a:1".into());
        assert_eq!(
            issue.message(),
            "proxy_rules[2].pattern: invalidDomain: a:1"
        );
        let issue = RuleIssue::new(0, "", "tooManyRules", "200".into());
        assert_eq!(issue.message(), "proxy_rules: tooManyRules: 200");
    }

    #[test]
    fn egress_resolution_and_labels() {
        let raw = serde_json::json!([
            {"id":"d","match":"domain","pattern":"d.test","action":"direct"},
            {"id":"m","match":"domain","pattern":"m.test","action":"manual"},
            {"id":"c","match":"domain","pattern":"c.test","action":"custom","proxy":"socks5://u:p@127.0.0.1:9"},
            {"id":"bad","match":"domain","pattern":"b.test","action":"custom","proxy":"socks5://h"}
        ])
        .to_string();
        let rules = set(&raw);
        assert_eq!(rules.len(), 3, "invalid custom rule skipped");
        let rule = rules.first_match("https://c.test/").expect("custom");
        let resolved = rule.egress().resolve();
        assert_eq!(resolved.mode, ProxyMode::Manual);
        assert_eq!(resolved.port, 9);
        assert_eq!(resolved.username, "u");
        assert!(resolved.rules.is_none());
        assert_eq!(rule.route_label(&resolved), route::RULE_CUSTOM);
        let rule = rules.first_match("https://m.test/").expect("manual");
        let resolved = rule.egress().resolve();
        assert_eq!(resolved.host, "10.0.0.2");
        assert!(resolved.no_proxy_list.is_empty());
        assert_eq!(rule.route_label(&resolved), route::RULE_MANUAL);
        let rule = rules.first_match("https://d.test/").expect("direct");
        assert_eq!(
            rule.route_label(&rule.egress().resolve()),
            route::RULE_DIRECT
        );
        assert!(!rule.egress().needs_detection());

        let incomplete = ProxyRuleSet::compile(&raw, &ProxyConfig::default());
        let rule = incomplete.first_match("https://m.test/").expect("manual");
        let resolved = rule.egress().resolve();
        assert_eq!(resolved.mode, ProxyMode::None);
        assert_eq!(rule.route_label(&resolved), route::RULE_DIRECT);
    }

    fn global(mode: &str, rules: &str) -> ProxyConfig {
        let map: HashMap<String, String> = [
            ("proxy_mode", mode),
            ("proxy_type", "socks5"),
            ("proxy_host", "10.0.0.2"),
            ("proxy_port", "1080"),
            ("proxy_username", "user"),
            ("proxy_password", "secret"),
            ("proxy_no_list", "*.lan"),
            ("proxy_rules", rules),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        ProxyConfig::from_config_map(&map)
    }

    #[test]
    fn explain_reasons_across_modes() {
        let rules = serde_json::json!([
            {"id":"g","name":"GitHub","match":"domain","pattern":"github.com","action":"custom","proxy":"http://user:pw@127.0.0.1:9"},
            {"id":"d","match":"domain","pattern":"direct.test","action":"direct"},
            {"id":"m","match":"domain","pattern":"manual.test","action":"manual"}
        ])
        .to_string();

        let none = global("none", &rules);
        assert!(none.rules.is_some());
        let hit = explain(&none, "https://x.github.com/a");
        assert_eq!(hit.reason, RouteReason::Proxy);
        assert_eq!(hit.endpoint, "http://127.0.0.1:9");
        assert_eq!(
            hit.matched,
            Some(MatchedRule {
                index: 0,
                id: "g".into(),
                name: "GitHub".into()
            })
        );
        assert_eq!(
            explain(&none, "https://direct.test/").reason,
            RouteReason::Direct
        );
        let manual_hit = explain(&none, "https://manual.test/");
        assert_eq!(manual_hit.reason, RouteReason::Proxy);
        assert_eq!(manual_hit.endpoint, "socks5://10.0.0.2:1080");
        assert_eq!(
            explain(&none, "https://other.test/").reason,
            RouteReason::ModeNone
        );
        assert_eq!(
            explain(&none, "http://localhost:1/").reason,
            RouteReason::Loopback
        );
        assert_eq!(
            explain(&none, "magnet:?xt=1").reason,
            RouteReason::Unsupported
        );
        assert_eq!(explain(&none, "nope").reason, RouteReason::InvalidUrl);

        let manual = global("manual", &rules);
        let via_manual = explain(&manual, "https://other.test/");
        assert_eq!(via_manual.reason, RouteReason::Proxy);
        assert!(!via_manual.endpoint.contains("secret"));
        assert_eq!(
            explain(&manual, "https://nas.lan/").reason,
            RouteReason::Bypass
        );
        // 命中规则后不看全局排除列表
        assert_eq!(
            explain(&manual, "https://direct.test/").reason,
            RouteReason::Direct
        );

        let auto = global("auto", "");
        assert!(auto.rules.is_none());
        assert_eq!(
            explain(&auto, "https://other.test/").reason,
            RouteReason::Auto
        );

        let mut incomplete = global("manual", "");
        incomplete.host.clear();
        assert_eq!(
            explain(&incomplete, "https://other.test/").reason,
            RouteReason::ManualIncomplete
        );

        let system = global("system", "");
        let reason = explain(&system, "https://other.test/").reason;
        assert!(matches!(
            reason,
            RouteReason::Proxy
                | RouteReason::Bypass
                | RouteReason::Pac
                | RouteReason::SystemUndetected
        ));
    }

    #[test]
    fn config_map_cache_reuses_compiled_set() {
        let rules = domain("cache.test");
        let a = global("none", &rules).rules.expect("rules");
        let b = global("none", &rules).rules.expect("rules");
        assert!(Arc::ptr_eq(&a, &b));
        assert!(global("none", "[]").rules.is_none());
        assert!(
            global("none", r#"[{"id":"x","enabled":false,"pattern":"a.com"}]"#)
                .rules
                .is_none()
        );
        let derived = global("manual", &rules).resolve();
        assert!(derived.rules.is_none());
    }
}
