//! 插件市场目录的纯逻辑：按插件归并版本条目、判定安装 / 更新动作、权限增量与撤回标记。
//!
//! 版本选择与引擎 `MarketClient::latest_entry` 同一规则（`yanked == "none"` 中 sequence
//! 最大者），保证列表展示的版本就是 `daemon.plugin.marketInstall` 实际装上的版本。

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

use fluxdown_protocol::{MarketEntryDto, PluginDto};

/// 引擎只安装 `yanked == "none"` 的条目（`MarketClient::install_latest` 不放行任何撤回版本）。
fn installable(entry: &MarketEntryDto) -> bool {
    entry.yanked == "none"
}

/// 每个插件一条：最新可安装版本；全部撤回（下架）的插件不出现在市场列表中。
/// 顺序保持各插件在索引中首次出现的位置。
pub fn latest_per_plugin(entries: &[MarketEntryDto]) -> Vec<MarketEntryDto> {
    let mut order: Vec<&str> = Vec::new();
    let mut best: HashMap<&str, &MarketEntryDto> = HashMap::new();
    for entry in entries {
        let id = entry.plugin_id.as_str();
        match best.get(id) {
            None => {
                order.push(id);
                best.insert(id, entry);
            }
            Some(current) => {
                let better = match (installable(entry), installable(current)) {
                    (true, false) => true,
                    (false, true) => false,
                    _ => entry.sequence > current.sequence,
                };
                if better {
                    best.insert(id, entry);
                }
            }
        }
    }
    order
        .into_iter()
        .filter_map(|id| {
            best.get(id)
                .copied()
                .filter(|entry| installable(entry))
                .cloned()
        })
        .collect()
}

/// 市场条目关键字过滤：名称 / id / 描述 / 作者 / 标签任一命中（大小写不敏感）。
pub fn filter_market<'a>(entries: &'a [MarketEntryDto], query: &str) -> Vec<&'a MarketEntryDto> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return entries.iter().collect();
    }
    let hit = |value: &str| value.to_lowercase().contains(&query);
    entries
        .iter()
        .filter(|entry| {
            hit(&entry.name)
                || hit(&entry.plugin_id)
                || hit(&entry.description)
                || hit(&entry.author)
                || entry.tags.iter().any(|tag| hit(tag))
        })
        .collect()
}

/// 市场列表排序方式；仅内存态，不持久化。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarketSort {
    /// 保持索引顺序。
    #[default]
    Default,
    /// 发布时间倒序。
    Newest,
    /// 收藏数倒序（同数按发布时间倒序）。
    Favorites,
}

impl MarketSort {
    pub const ALL: [Self; 3] = [Self::Default, Self::Newest, Self::Favorites];

    /// 下拉项 / 当前值的 i18n 键。
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Default => "marketSortDefault",
            Self::Newest => "marketSortNewest",
            Self::Favorites => "marketSortFavorites",
        }
    }
}

/// 发布时间降序比较（ISO-8601 字符串序）；空串排最后。
fn newest_first(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => b.cmp(a),
    }
}

/// 收藏筛选 + 排序（与 Web `arrangeMarket` 共用 `market_sort_cases.json`）。
/// 排序均为稳定排序，平手保持输入（索引）顺序。
pub fn arrange_market<'a>(
    mut entries: Vec<&'a MarketEntryDto>,
    sort: MarketSort,
    counts: &BTreeMap<String, u64>,
    mine: &HashSet<String>,
    favorites_only: bool,
) -> Vec<&'a MarketEntryDto> {
    if favorites_only {
        entries.retain(|entry| mine.contains(&entry.plugin_id));
    }
    let count_of = |entry: &MarketEntryDto| counts.get(&entry.plugin_id).copied().unwrap_or(0);
    match sort {
        MarketSort::Default => {}
        MarketSort::Newest => {
            entries.sort_by(|a, b| newest_first(&a.publish_time, &b.publish_time));
        }
        MarketSort::Favorites => entries.sort_by(|a, b| {
            count_of(b)
                .cmp(&count_of(a))
                .then_with(|| newest_first(&a.publish_time, &b.publish_time))
        }),
    }
    entries
}

/// 解析 `MAJOR.MINOR.PATCH`（可带前导 `v`，忽略预发布 / 构建后缀），与引擎
/// `plugin::semver::parse_semver` 同语义；UI crate 不依赖引擎，故本地保留一份。
fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// `candidate` 严格新于 `current` 时为真；任一侧无法解析视为不可比较（不提示更新）。
pub fn version_newer(candidate: &str, current: &str) -> bool {
    match (parse_semver(candidate), parse_semver(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// 市场条目相对本机安装状态的可执行动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketAction {
    Install,
    Update,
    Installed,
    /// 市场只剩撤回版本：引擎会拒绝安装。
    Unavailable,
}

/// 开发模式插件的源码由本地目录管理，不从市场覆盖。
pub fn market_action(entry: &MarketEntryDto, installed: Option<&PluginDto>) -> MarketAction {
    match installed {
        None if installable(entry) => MarketAction::Install,
        None => MarketAction::Unavailable,
        Some(plugin)
            if !plugin.dev_mode
                && installable(entry)
                && version_newer(&entry.version, &plugin.version) =>
        {
            MarketAction::Update
        }
        Some(_) => MarketAction::Installed,
    }
}

/// 需要用户确认的权限：新装取条目全部权限，更新只取已安装版本没有的新增权限。
pub fn permissions_to_confirm(
    entry: &MarketEntryDto,
    installed: Option<&PluginDto>,
) -> Vec<String> {
    let granted: &[String] = installed.map_or(&[], |plugin| &plugin.permissions);
    entry
        .permissions
        .iter()
        .filter(|permission| !granted.contains(permission))
        .cloned()
        .collect()
}

/// 用户确认权限之后市场索引又变化时，原确认是否仍覆盖将要安装的内容：
/// 版本不变，且需确认的权限与网络主机都没有超出用户已确认的集合。
pub fn confirmation_holds(
    confirmed_version: &str,
    confirmed_permissions: &[String],
    confirmed_hosts: &[String],
    latest: &MarketEntryDto,
    installed: Option<&PluginDto>,
) -> bool {
    latest.version == confirmed_version
        && permissions_to_confirm(latest, installed)
            .iter()
            .all(|permission| confirmed_permissions.contains(permission))
        && hosts_to_confirm(latest, installed)
            .iter()
            .all(|host| confirmed_hosts.contains(host))
}

/// 已安装版本在市场中的撤回标记（`deprecated` / `vulnerable` / `malicious`）；未撤回或
/// 市场无此版本返回 `None`。
pub fn installed_version_yanked<'a>(
    entries: &'a [MarketEntryDto],
    plugin: &PluginDto,
) -> Option<&'a str> {
    if plugin.dev_mode {
        return None;
    }
    entries
        .iter()
        .find(|entry| entry.plugin_id == plugin.identity && entry.version == plugin.version)
        .map(|entry| entry.yanked.as_str())
        .filter(|yanked| !yanked.is_empty() && *yanked != "none")
}

/// 权限集合对应的风险等级（与引擎 `PluginManifest::risk` 同规则，夹具共享）。
pub fn risk_of(permissions: &[String]) -> &'static str {
    let has = |name: &str| permissions.iter().any(|p| p == name);
    if has("desktop") || has("pageScripts") {
        "full"
    } else if [
        "ffmpeg",
        "ytdlp",
        "auth",
        "tasks",
        "browserRules",
        "background",
        "intercept",
        "archive",
    ]
    .iter()
    .any(|name| has(name))
    {
        "elevated"
    } else {
        "standard"
    }
}

fn sorted_set(values: &[String]) -> Vec<&str> {
    let mut set: Vec<&str> = values.iter().map(String::as_str).collect();
    set.sort_unstable();
    set.dedup();
    set
}

/// 已确认集合与待授权集合（权限、网络主机分别排序去重）完全相等时，无需再弹授权框。
pub fn consent_matches(
    confirmed_permissions: &[String],
    confirmed_hosts: &[String],
    pending_permissions: &[String],
    pending_hosts: &[String],
) -> bool {
    sorted_set(confirmed_permissions) == sorted_set(pending_permissions)
        && sorted_set(confirmed_hosts) == sorted_set(pending_hosts)
}

/// 安装 / 更新成功后对返回插件的授权处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentFlow {
    /// 无待授权项。
    None,
    /// 用户安装前已确认的集合正好等于待授权项：直接调用 `daemon.plugin.consent`。
    Direct,
    /// 弹出授权对话框。
    Dialog,
}

/// `confirmed` 为市场安装前用户确认的 `(权限, 主机)`；其他路径传 `None`。
pub fn consent_flow(plugin: &PluginDto, confirmed: Option<(&[String], &[String])>) -> ConsentFlow {
    if plugin.consent_pending.is_empty() && plugin.consent_pending_hosts.is_empty() {
        return ConsentFlow::None;
    }
    match confirmed {
        Some((permissions, hosts))
            if consent_matches(
                permissions,
                hosts,
                &plugin.consent_pending,
                &plugin.consent_pending_hosts,
            ) =>
        {
            ConsentFlow::Direct
        }
        _ => ConsentFlow::Dialog,
    }
}

/// 市场条目中尚未授权的网络主机（更新时只含新增主机）。
pub fn hosts_to_confirm(entry: &MarketEntryDto, installed: Option<&PluginDto>) -> Vec<String> {
    let granted: &[String] = installed.map_or(&[], |plugin| &plugin.network_hosts);
    entry
        .network_hosts
        .iter()
        .filter(|host| !granted.contains(host))
        .cloned()
        .collect()
}

/// 目录最长可信时长：超过后再次进入插件页会重新拉取，保证「更新」徽标不陈旧。
pub const MARKET_MAX_AGE: Duration = Duration::from_secs(10 * 60);

/// 目录是否需要（重新）加载：从未请求、断连失败待重试、或上次成功加载已过期。
/// 失败（`failed`）且非断连类时不自动重试，避免重绘循环——由手动刷新恢复。
pub fn market_needs_load(
    requested: bool,
    retry_on_reconnect: bool,
    failed: bool,
    loaded_at: Option<Instant>,
    now: Instant,
) -> bool {
    if !requested || retry_on_reconnect {
        return true;
    }
    !failed && loaded_at.is_some_and(|at| now.saturating_duration_since(at) >= MARKET_MAX_AGE)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fluxdown_protocol::{MarketEntryDto, PluginDto};

    use super::{
        MARKET_MAX_AGE, MarketAction, confirmation_holds, filter_market, installed_version_yanked,
        latest_per_plugin, market_action, market_needs_load, permissions_to_confirm, version_newer,
    };

    #[test]
    fn market_reloads_when_never_requested_retrying_or_expired() {
        use std::time::{Duration, Instant};

        let loaded = Instant::now();
        let fresh = loaded + MARKET_MAX_AGE - Duration::from_secs(1);
        let expired = loaded + MARKET_MAX_AGE;
        assert!(market_needs_load(false, false, false, None, loaded));
        assert!(market_needs_load(true, true, true, Some(loaded), loaded));
        assert!(!market_needs_load(true, false, false, Some(loaded), fresh));
        assert!(market_needs_load(true, false, false, Some(loaded), expired));
        // 失败后不自动重试（防重绘循环）；从未成功加载过也不按时间重试。
        assert!(!market_needs_load(true, false, true, Some(loaded), expired));
        assert!(!market_needs_load(true, false, false, None, expired));
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    fn fixture() -> serde_json::Value {
        let text = include_str!("../tests/fixtures/consent_cases.json");
        serde_json::from_str(text).expect("consent fixture parses")
    }

    fn fixture_list(value: &serde_json::Value, key: &str) -> Vec<String> {
        value[key]
            .as_array()
            .expect("array")
            .iter()
            .map(|v| v.as_str().expect("string").to_owned())
            .collect()
    }

    #[test]
    fn arrange_market_follows_shared_fixture() {
        use std::collections::{BTreeMap, HashSet};

        use super::{MarketSort, arrange_market};

        let text = include_str!("../tests/fixtures/market_sort_cases.json");
        let doc: serde_json::Value = serde_json::from_str(text).expect("market sort fixture");
        for case in doc["cases"].as_array().expect("cases") {
            let entries: Vec<MarketEntryDto> = case["entries"]
                .as_array()
                .expect("entries")
                .iter()
                .map(|raw| {
                    let mut built =
                        entry(raw["pluginId"].as_str().expect("id"), "1.0.0", 1, "none");
                    built.publish_time = raw["publishTime"].as_str().expect("time").to_owned();
                    built
                })
                .collect();
            let counts: BTreeMap<String, u64> =
                serde_json::from_value(case["counts"].clone()).expect("counts");
            let mine: HashSet<String> = fixture_list(case, "mine").into_iter().collect();
            let sort = match case["sort"].as_str().expect("sort") {
                "default" => MarketSort::Default,
                "newest" => MarketSort::Newest,
                "favorites" => MarketSort::Favorites,
                other => panic!("unknown sort {other}"),
            };
            let favorites_only = case["favoritesOnly"].as_bool().expect("favoritesOnly");
            let got = arrange_market(
                entries.iter().collect(),
                sort,
                &counts,
                &mine,
                favorites_only,
            )
            .into_iter()
            .map(|entry| entry.plugin_id.clone())
            .collect::<Vec<_>>();
            assert_eq!(got, fixture_list(case, "expected"), "{}", case["name"]);
        }
    }

    #[test]
    fn risk_follows_shared_fixture() {
        for case in fixture()["risk"].as_array().expect("risk cases") {
            let permissions = fixture_list(case, "permissions");
            assert_eq!(
                super::risk_of(&permissions),
                case["risk"].as_str().expect("risk"),
                "{}",
                case["name"]
            );
        }
    }

    #[test]
    fn consent_matches_follows_shared_fixture() {
        for case in fixture()["matches"].as_array().expect("match cases") {
            let got = super::consent_matches(
                &fixture_list(&case["confirmed"], "permissions"),
                &fixture_list(&case["confirmed"], "networkHosts"),
                &fixture_list(&case["pending"], "permissions"),
                &fixture_list(&case["pending"], "networkHosts"),
            );
            assert_eq!(
                got,
                case["matches"].as_bool().expect("bool"),
                "{}",
                case["name"]
            );
        }
    }

    #[test]
    fn consent_flow_decides_direct_or_dialog() {
        use super::{ConsentFlow, consent_flow};
        let mut p = plugin("a@x", "1.0.0", false, &["tasks"]);
        assert_eq!(consent_flow(&p, None), ConsentFlow::None);
        p.consent_pending = strings(&["tasks"]);
        p.consent_pending_hosts = strings(&["api.x.com"]);
        let perms = strings(&["tasks"]);
        let hosts = strings(&["api.x.com"]);
        assert_eq!(consent_flow(&p, None), ConsentFlow::Dialog);
        assert_eq!(
            consent_flow(&p, Some((&perms, &hosts))),
            ConsentFlow::Direct
        );
        assert_eq!(consent_flow(&p, Some((&perms, &[]))), ConsentFlow::Dialog);
    }

    fn entry(id: &str, version: &str, sequence: u64, yanked: &str) -> MarketEntryDto {
        MarketEntryDto {
            plugin_id: id.to_owned(),
            version: version.to_owned(),
            sequence,
            content_hash: String::new(),
            min_app_version: String::new(),
            name: String::new(),
            description: String::new(),
            author: String::new(),
            homepage: String::new(),
            mirrors: Vec::new(),
            publish_time: String::new(),
            yanked: yanked.to_owned(),
            tags: Vec::new(),
            permissions: Vec::new(),
            network_hosts: Vec::new(),
        }
    }

    fn plugin(id: &str, version: &str, dev_mode: bool, permissions: &[&str]) -> PluginDto {
        PluginDto {
            identity: id.to_owned(),
            name: id.to_owned(),
            version: version.to_owned(),
            description: String::new(),
            homepage: String::new(),
            enabled: true,
            dev_mode,
            disabled_reason: "None".to_owned(),
            settings: Vec::new(),
            settings_values: HashMap::new(),
            permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
            auth_supported: false,
            subscription_provider_ids: Vec::new(),
            resolver_urls: Vec::new(),
            load_status: "Loaded".to_owned(),
            load_error: String::new(),
            contributions: fluxdown_protocol::PluginContributionsDto::default(),
            risk: "standard".to_owned(),
            consent_pending: Vec::new(),
            consent_pending_hosts: Vec::new(),
            network_hosts: Vec::new(),
            package_dir: None,
            package_revision: 0,
        }
    }

    #[test]
    fn catalog_keeps_one_row_per_plugin_with_the_version_the_engine_installs() {
        let entries = [
            entry("a@x", "1.0.0", 1, "none"),
            entry("b@y", "0.1.0", 2, "none"),
            entry("a@x", "1.2.0", 3, "none"),
            // 更高 sequence 但已撤回：引擎不会装它。
            entry("a@x", "1.3.0", 4, "vulnerable"),
        ];
        let catalog = latest_per_plugin(&entries);
        let rows = catalog
            .iter()
            .map(|e| (e.plugin_id.as_str(), e.version.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![("a@x", "1.2.0"), ("b@y", "0.1.0")]);
    }

    #[test]
    fn fully_yanked_plugin_is_hidden_from_catalog() {
        let entries = [
            entry("a@x", "1.0.0", 1, "deprecated"),
            entry("a@x", "1.1.0", 2, "malicious"),
            entry("b@y", "1.0.0", 3, "none"),
        ];
        let catalog = latest_per_plugin(&entries);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].plugin_id, "b@y");
    }

    #[test]
    fn semver_comparison_is_numeric_not_lexical() {
        assert!(version_newer("0.10.0", "0.9.9"));
        assert!(version_newer("v2.0.0", "1.99.99"));
        assert!(!version_newer("1.0.0", "1.0.0"));
        assert!(!version_newer("1.0.0-rc.1", "1.0.0"));
        assert!(!version_newer("garbage", "1.0.0"));
        assert!(!version_newer("2.0.0", ""));
    }

    #[test]
    fn action_offers_update_only_for_newer_non_dev_installs() {
        let latest = entry("a@x", "1.2.0", 3, "none");
        assert_eq!(market_action(&latest, None), MarketAction::Install);
        assert_eq!(
            market_action(&latest, Some(&plugin("a@x", "1.0.0", false, &[]))),
            MarketAction::Update
        );
        assert_eq!(
            market_action(&latest, Some(&plugin("a@x", "1.2.0", false, &[]))),
            MarketAction::Installed
        );
        // 本地比市场新（如作者本地调试版）不降级。
        assert_eq!(
            market_action(&latest, Some(&plugin("a@x", "2.0.0", false, &[]))),
            MarketAction::Installed
        );
        assert_eq!(
            market_action(&latest, Some(&plugin("a@x", "1.0.0", true, &[]))),
            MarketAction::Installed
        );
    }

    #[test]
    fn update_confirms_only_newly_requested_permissions() {
        let mut latest = entry("a@x", "1.2.0", 3, "none");
        latest.permissions = vec!["ffmpeg".to_owned(), "ytdlp".to_owned()];
        assert_eq!(
            permissions_to_confirm(&latest, None),
            vec!["ffmpeg".to_owned(), "ytdlp".to_owned()]
        );
        let installed = plugin("a@x", "1.0.0", false, &["ffmpeg"]);
        assert_eq!(
            permissions_to_confirm(&latest, Some(&installed)),
            vec!["ytdlp".to_owned()]
        );
        let full = plugin("a@x", "1.0.0", false, &["ffmpeg", "ytdlp"]);
        assert!(permissions_to_confirm(&latest, Some(&full)).is_empty());
    }

    #[test]
    fn confirmation_breaks_on_new_version_or_extra_permission() {
        let mut latest = entry("a@x", "1.2.0", 3, "none");
        latest.permissions = vec!["ffmpeg".to_owned()];
        let confirmed = vec!["ffmpeg".to_owned()];
        assert!(confirmation_holds("1.2.0", &confirmed, &[], &latest, None));
        // 版本在确认后变了。
        assert!(!confirmation_holds("1.1.0", &confirmed, &[], &latest, None));
        // 新版本多要了权限。
        latest.permissions.push("ytdlp".to_owned());
        assert!(!confirmation_holds("1.2.0", &confirmed, &[], &latest, None));
        // 已安装版本已授予的权限无需再确认。
        let installed = plugin("a@x", "1.0.0", false, &["ytdlp"]);
        assert!(confirmation_holds(
            "1.2.0",
            &confirmed,
            &[],
            &latest,
            Some(&installed)
        ));
        // 新版本多要了网络主机。
        latest.network_hosts = vec!["api.x.com".to_owned()];
        assert!(!confirmation_holds(
            "1.2.0",
            &confirmed,
            &[],
            &latest,
            Some(&installed)
        ));
        assert!(confirmation_holds(
            "1.2.0",
            &confirmed,
            &["api.x.com".to_owned()],
            &latest,
            Some(&installed)
        ));
    }

    #[test]
    fn installed_version_yank_is_reported_even_when_a_fix_exists() {
        let entries = [
            entry("a@x", "1.0.0", 1, "malicious"),
            entry("a@x", "1.1.0", 2, "none"),
        ];
        assert_eq!(
            installed_version_yanked(&entries, &plugin("a@x", "1.0.0", false, &[])),
            Some("malicious")
        );
        assert_eq!(
            installed_version_yanked(&entries, &plugin("a@x", "1.1.0", false, &[])),
            None
        );
        assert_eq!(
            installed_version_yanked(&entries, &plugin("a@x", "1.0.0", true, &[])),
            None
        );
    }

    #[test]
    fn empty_query_keeps_everything() {
        let entries = [
            entry("a", "1.0.0", 1, "none"),
            entry("b", "1.0.0", 2, "none"),
        ];
        assert_eq!(filter_market(&entries, "   ").len(), 2);
    }

    #[test]
    fn query_matches_any_field_case_insensitively() {
        let mut video = entry("video.dl", "1.0.0", 1, "none");
        video.name = "Video".to_owned();
        video.description = "grabs videos".to_owned();
        video.author = "Ann".to_owned();
        video.tags = vec!["media".to_owned()];
        let mut other = entry("other", "1.0.0", 2, "none");
        other.name = "Other".to_owned();
        other.description = "misc".to_owned();
        other.author = "Bob".to_owned();
        other.tags = vec!["tools".to_owned()];
        let entries = [video, other];
        let ids = |query: &str| {
            filter_market(&entries, query)
                .into_iter()
                .map(|entry| entry.plugin_id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("VIDEO"), vec!["video.dl"]);
        assert_eq!(ids("bob"), vec!["other"]);
        assert_eq!(ids("media"), vec!["video.dl"]);
        assert_eq!(ids("grabs"), vec!["video.dl"]);
        assert!(ids("nothing").is_empty());
    }
}
