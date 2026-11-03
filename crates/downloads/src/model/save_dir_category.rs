//! 「新建下载」保存目录的分类下拉：预测目录、选项列表与「自动」模式的跟随判定。
//!
//! 预测语义与 agent `category_dir.rs::route_save_dir` 逐条对齐（两端共用夹具
//! `native/engine/src/fixtures/category_route_cases.json`）：网址规则固定优先于扩展名 /
//! 正则分类（不看列表位置）；文件名不含 `.` 时用 URL 路径末段（百分号解码）；文件名
//! 规则按序取首个配置了目录且命中的，无规则命中才取「其他」目录；扩展名 / 正则不区分
//! 大小写，扩展名可带前导点，非法正则不命中。

use fluxdown_protocol::{CategoryRouteDto, CategoryRuleDto, CustomCategoryDto};

use super::new_download::{percent_decode_lossy, url_file_segment};

/// 下拉里的一个分类（可见、非 `all`、配置了保存目录）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewDownloadCategory {
    pub id: String,
    /// 内置项类型（显示名走 i18n）；自定义项为 `None`，显示 `name`。
    pub builtin_type: Option<String>,
    pub name: String,
    /// 图标名（经 `category_icon` 映射，与侧栏一致）。
    pub icon: String,
    /// 已去首尾空白的分类目录，必非空。
    pub save_dir: String,
}

/// 保存目录的分类模式。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum CategoryMode {
    /// 按文件名自动选分类目录，提交时沿用 `follows` 路由。
    #[default]
    Auto,
    /// 不分类：目录框 = 初始目录，提交不路由。
    NoCategory,
    /// 显式选择某个分类：目录框 = 该分类目录，提交不路由。
    Category(String),
}

/// 下拉候选：按分类列表顺序（已按 `position` 排序），保留可见、非 `all`、目录非空的项。
pub(crate) fn category_options(categories: &[CustomCategoryDto]) -> Vec<NewDownloadCategory> {
    categories
        .iter()
        .filter(|category| {
            category.visible
                && category.builtin_type.as_deref() != Some("all")
                && !category.save_dir.trim().is_empty()
        })
        .map(|category| NewDownloadCategory {
            id: category.id.clone(),
            builtin_type: category.builtin_type.clone(),
            name: category.name.clone(),
            icon: category.icon.clone(),
            save_dir: category.save_dir.trim().to_owned(),
        })
        .collect()
}

/// 路由命中的分类保存目录；无命中 / 命中分类未配置目录时返回 `None`。
///
/// 网址规则固定优先（不看列表位置）：先按列表顺序取首个命中且配置了目录的 `url`
/// 规则，命中即定落点；扩展名 / 正则分类只处理未被网址规则认领的任务。
pub(crate) fn predict_save_dir<'a>(
    route: &'a CategoryRouteDto,
    file_name: &str,
    url: &str,
) -> Option<&'a str> {
    if let Some(rule) = route.rules.iter().find(|rule| {
        rule.match_mode == "url"
            && !rule.save_dir.is_empty()
            && super::categories::wildcard_matches(rule.url_pattern.as_str(), url)
    }) {
        return Some(rule.save_dir.as_str());
    }
    let url_name;
    let name = if file_name.contains('.') {
        file_name
    } else {
        url_name = url_file_segment(url.trim())
            .map(percent_decode_lossy)
            .filter(|decoded| decoded.contains('.'));
        url_name.as_deref().unwrap_or(file_name)
    };
    if name.is_empty() {
        return None;
    }
    if let Some(rule) = route
        .rules
        .iter()
        .find(|rule| !rule.save_dir.is_empty() && rule_matches(rule, name))
    {
        return Some(rule.save_dir.as_str());
    }
    // 「其他」兜底前的命中检查包含网址规则：命中但没配目录的网址规则同样阻止兜底
    //（与引擎 `save_dir_for` 的 any 判定一致）。
    let claimed = |rule: &CategoryRuleDto| {
        rule_matches(rule, name)
            || (rule.match_mode == "url"
                && super::categories::wildcard_matches(rule.url_pattern.as_str(), url))
    };
    (!route.other_dir.is_empty() && !route.rules.iter().any(claimed))
        .then_some(route.other_dir.as_str())
}

fn rule_matches(rule: &CategoryRuleDto, name: &str) -> bool {
    if rule.match_mode == "regex" {
        return !rule.regex_pattern.is_empty()
            && regex::RegexBuilder::new(&rule.regex_pattern)
                .case_insensitive(true)
                .build()
                .is_ok_and(|regex| regex.is_match(name));
    }
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    !extension.is_empty()
        && rule.extensions.iter().any(|candidate| {
            candidate
                .trim_start_matches('.')
                .eq_ignore_ascii_case(extension)
        })
}

/// 预测目录对应的下拉项：第一个目录等于预测目录的分类。
pub(crate) fn matched_category<'a>(
    options: &'a [NewDownloadCategory],
    predicted: Option<&str>,
) -> Option<&'a NewDownloadCategory> {
    let predicted = predicted?;
    options.iter().find(|option| option.save_dir == predicted)
}

/// 目录框是否仍是表单写入的值（用户没手动改过）：当前值（去空白）为空；表单写过目录框时
/// 等于上一次写入值 `last_written`；尚未写过时等于初始目录。用户改回初始目录也算手改。
pub(crate) fn dir_untouched(current: &str, offered: &str, last_written: Option<&str>) -> bool {
    let current = current.trim();
    current.is_empty()
        || last_written.map_or(current == offered.trim(), |written| written == current)
}

/// 「自动」模式下预测输入变化时目录框应改成的值：只有保存目录仍遵循自动意图时才跟随。
pub(crate) fn auto_follow_target(
    current: &str,
    offered: &str,
    last_written: Option<&str>,
    predicted: Option<&str>,
    manually_edited: bool,
) -> Option<String> {
    save_dir_untouched(current, offered, last_written, manually_edited)
        .then(|| predicted.unwrap_or(offered.trim()).to_owned())
}

/// Whether the save-directory field still follows automatic intent.
pub(crate) fn save_dir_untouched(
    current: &str,
    offered: &str,
    last_written: Option<&str>,
    manually_edited: bool,
) -> bool {
    !manually_edited && dir_untouched(current, offered, last_written)
}

/// Whether category routing is eligible for this selection state.
pub(crate) fn category_route_eligible(mode: &CategoryMode, save_dir_untouched: bool) -> bool {
    *mode == CategoryMode::Auto && save_dir_untouched
}

/// 常见的二级公共后缀（`co.uk` 这类）：三段域名以它结尾时注册域占两段，最左段不是子域，
/// 不能通配。刻意只列常见项，不引入公共后缀库依赖。
const TWO_LEVEL_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "me.uk", "com.cn", "net.cn", "org.cn", "gov.cn",
    "edu.cn", "com.au", "net.au", "org.au", "edu.au", "co.jp", "ne.jp", "or.jp", "ac.jp", "co.nz",
    "co.kr", "com.br", "com.tw", "com.hk", "co.in", "co.za",
];

/// 主机是否该把最左段通配：IP 字面量（IPv4 / 方括号 IPv6）与含空段的主机永不通配；
/// ≥4 段通配；3 段时仅当末两段不是常见二级公共后缀才通配。
fn wildcard_leftmost_label(host: &str) -> bool {
    if host.starts_with('[') {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.iter().any(|label| label.is_empty())
        || labels
            .iter()
            .all(|label| label.chars().all(|ch| ch.is_ascii_digit()))
    {
        return false;
    }
    match labels.len() {
        0..=2 => false,
        3 => {
            let suffix = format!("{}.{}", labels[1], labels[2]).to_ascii_lowercase();
            !TWO_LEVEL_SUFFIXES.contains(&suffix.as_str())
        }
        _ => true,
    }
}

/// 由下载链接推断网址规则模式，保证建议必然命中来源链接本身：
/// - 域名最左段按 [`wildcard_leftmost_label`] 通配（覆盖 `filecache44` / `supporter-files`
///   这类轮换、多 CDN 子域）；端口原样保留，用户信息整体通配为 `*@`（不把凭据写进规则）；
/// - 路径丢掉末段文件名，目录段从头保留到第一个易变段（长度 ≤3、纯十六进制、含通配
///   字符或空段）为止，其后一律交给末尾的 `*`（跨 `/` 匹配子路径），不跳过中间段；
/// - 查询串 / 片段恒丢弃。
///
/// 非 http(s) 或解析不出域名的链接返回空串（对话框仍可手填）。
pub(crate) fn suggest_url_pattern(url: &str) -> String {
    let trimmed = url.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return String::new();
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return String::new();
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    if authority.contains('*') {
        return String::new();
    }
    let (userinfo, host_port) = authority
        .rsplit_once('@')
        .map_or(("", authority), |(_, host_port)| ("*@", host_port));
    let (host, port) = if host_port.starts_with('[') {
        match host_port.find(']') {
            Some(end) => host_port.split_at(end + 1),
            None => return String::new(),
        }
    } else {
        host_port
            .find(':')
            .map_or((host_port, ""), |at| host_port.split_at(at))
    };
    if host.is_empty() || host == "[]" {
        return String::new();
    }
    let pattern_host = if wildcard_leftmost_label(host) {
        host.split_once('.')
            .map_or_else(|| host.to_owned(), |(_, rest)| format!("*.{rest}"))
    } else {
        host.to_owned()
    };
    let origin = format!("{scheme}://{userinfo}{pattern_host}{port}");
    let Some(path) = tail.strip_prefix('/') else {
        // 没有路径（`https://host`、`https://host?x`）：不带 `/`，直接以 `*` 收尾。
        return format!("{origin}*");
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut segments: Vec<&str> = path.split('/').collect();
    segments.pop();
    let stable: Vec<&str> = segments
        .into_iter()
        .take_while(|segment| {
            segment.len() > 3
                && !segment.chars().all(|ch| ch.is_ascii_hexdigit())
                && !segment.contains(['*', '?'])
        })
        .collect();
    if stable.is_empty() {
        format!("{origin}/*")
    } else {
        format!("{origin}/{}*", stable.join("/"))
    }
}

/// 网址规则的名称建议：域名末两段（注册域），取不到域名为空串（需手填）。
pub(crate) fn suggest_rule_name(url: &str) -> String {
    let host = super::url_host(url);
    if host.is_empty() {
        return String::new();
    }
    let labels: Vec<&str> = host.split('.').collect();
    match labels.len() {
        0 => String::new(),
        1 => host.to_owned(),
        n => format!("{}.{}", labels[n - 2], labels[n - 1]),
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{CategoryRouteDto, CustomCategoryDto};
    use serde::Deserialize;

    use super::{
        CategoryMode, auto_follow_target, category_options, category_route_eligible, dir_untouched,
        matched_category, predict_save_dir, save_dir_untouched, suggest_rule_name,
        suggest_url_pattern,
    };

    #[test]
    fn url_pattern_suggestions_cover_cdn_rotation_and_hash_paths() {
        // 多段域名:最左段通配;稳定路径段保留,末尾 * 跨子路径。
        assert_eq!(
            suggest_url_pattern("https://filecache44.gamebanana.com/mods/v832.zip"),
            "https://*.gamebanana.com/mods*"
        );
        // 哈希分片 / 短码段(≤3 字符或纯十六进制)视为易变,整段路径丢弃;查询串恒丢弃。
        assert_eq!(
            suggest_url_pattern(
                "https://supporter-files.nexus-cdn.com/9e/d6/ab12cd34ef.zip?md5=x&expires=1"
            ),
            "https://*.nexus-cdn.com/*"
        );
        // 两段域名不通配子域。
        assert_eq!(
            suggest_url_pattern("https://example.com/files/setup.exe"),
            "https://example.com/files*"
        );
        assert_eq!(
            suggest_url_pattern("https://example.com/a.zip"),
            "https://example.com/*"
        );
        // 非 http(s) / 解析不出域名:空建议。
        assert_eq!(suggest_url_pattern("magnet:?xt=urn:btih:abc"), "");
        assert_eq!(suggest_url_pattern("ed2k://|file|x|1|a|/"), "");
        assert_eq!(suggest_url_pattern("not a url"), "");
    }

    #[test]
    fn url_pattern_suggestions_always_match_their_source_url() {
        let cases = [
            // 中间段易变:截断在第一个易变段,不跳过去拼后面的段。
            (
                "https://github.com/abc/releases/download/v1/app.zip",
                "https://github.com/*",
            ),
            (
                "https://example.com/files/2024/setup/app.exe",
                "https://example.com/files*",
            ),
            (
                "https://example.com/files/2024/setup/",
                "https://example.com/files*",
            ),
            // 端口 / 用户信息保留在 authority。
            (
                "http://cdn1.example.com:8080/mods/a.zip",
                "http://*.example.com:8080/mods*",
            ),
            (
                "https://user:pw@dl.example.com/mods/a.zip",
                "https://*@*.example.com/mods*",
            ),
            // IP 字面量不通配、不被改写。
            (
                "http://192.168.1.10:9000/dl/a.zip",
                "http://192.168.1.10:9000/*",
            ),
            (
                "http://[2001:db8::1]:8080/pub/a.zip",
                "http://[2001:db8::1]:8080/*",
            ),
            ("http://[::1]/mods/a.zip", "http://[::1]/mods*"),
            // 二级公共后缀的注册域不通配;更深的子域仍通配。
            (
                "https://example.co.uk/files/a.zip",
                "https://example.co.uk/files*",
            ),
            (
                "https://cdn.example.co.uk/files/a.zip",
                "https://*.example.co.uk/files*",
            ),
            (
                "https://a.b.example.com/files/a.zip",
                "https://*.b.example.com/files*",
            ),
            (
                "https://filecache44.gamebanana.com/mods/v832.zip",
                "https://*.gamebanana.com/mods*",
            ),
            // 没有路径 / 仅查询串。
            ("https://example.com", "https://example.com*"),
            ("https://example.com?x=1", "https://example.com*"),
        ];
        for (url, expected) in cases {
            let pattern = suggest_url_pattern(url);
            assert_eq!(pattern, expected, "{url}");
            assert!(
                super::super::categories::wildcard_matches(&pattern, url),
                "{pattern} should match {url}"
            );
        }
    }

    #[test]
    fn rule_name_suggestion_uses_the_registrable_host() {
        assert_eq!(
            suggest_rule_name("https://filecache44.gamebanana.com/mods/a.zip"),
            "gamebanana.com"
        );
        assert_eq!(
            suggest_rule_name("https://supporter-files.nexus-cdn.com/x"),
            "nexus-cdn.com"
        );
        assert_eq!(
            suggest_rule_name("https://example.com/a.zip"),
            "example.com"
        );
        assert_eq!(suggest_rule_name("magnet:?xt=abc"), "");
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        name: String,
        route: CategoryRouteDto,
        file_name: String,
        url: String,
        expected: Option<String>,
    }

    #[test]
    fn prediction_matches_the_shared_route_fixture() {
        let cases: Vec<Case> = serde_json::from_str(include_str!(
            "../../../../native/engine/src/fixtures/category_route_cases.json"
        ))
        .expect("fixture parses");
        assert!(!cases.is_empty());
        for case in cases {
            assert_eq!(
                predict_save_dir(&case.route, &case.file_name, &case.url),
                case.expected.as_deref(),
                "{}",
                case.name
            );
        }
    }

    fn category(id: &str, builtin: Option<&str>, dir: &str, visible: bool) -> CustomCategoryDto {
        CustomCategoryDto {
            id: id.to_owned(),
            name: id.to_owned(),
            icon: "file".to_owned(),
            match_mode: "extension".to_owned(),
            extensions: Vec::new(),
            regex_pattern: String::new(),
            url_pattern: String::new(),
            position: 0,
            visible,
            is_builtin: builtin.is_some(),
            builtin_type: builtin.map(str::to_owned),
            save_dir: dir.to_owned(),
        }
    }

    #[test]
    fn options_keep_visible_non_all_categories_with_a_directory() {
        let options = category_options(&[
            category("all", Some("all"), "/all", true),
            category("video", Some("video"), " /videos ", true),
            category("hidden", Some("audio"), "/music", false),
            category("blank", Some("image"), "  ", true),
            category("mine", None, "/mine", true),
            category("other", Some("other"), "/other", true),
        ]);
        let ids: Vec<_> = options.iter().map(|option| option.id.as_str()).collect();
        assert_eq!(ids, ["video", "mine", "other"]);
        assert_eq!(options[0].save_dir, "/videos");
        assert_eq!(
            matched_category(&options, Some("/mine")).map(|option| option.id.as_str()),
            Some("mine")
        );
        assert!(matched_category(&options, Some("/nowhere")).is_none());
        assert!(matched_category(&options, None).is_none());
    }

    #[test]
    fn empty_picker_options_do_not_remove_automatic_route_authorization() {
        let options = category_options(&[]);
        assert!(options.is_empty());
        assert!(category_route_eligible(&CategoryMode::Auto, true));
    }

    #[test]
    fn manual_directory_intent_survives_prediction_and_default_changes() {
        let mode = CategoryMode::Auto;
        for chosen in ["/downloads", "/videos"] {
            for offered in ["/downloads", "/new-default"] {
                // 手选旧默认目录或分类目录，即使路径仍等于程序写入值，也不得覆盖或路由。
                assert_eq!(
                    auto_follow_target(chosen, offered, Some(chosen), Some("/programs"), true),
                    None
                );
                assert!(!category_route_eligible(
                    &mode,
                    save_dir_untouched(chosen, offered, Some(chosen), true),
                ));

                // 显式重选「自动」清除手动意图，恢复预测；写回的目录可继续参与提交路由。
                let restored =
                    auto_follow_target(chosen, offered, Some(chosen), Some("/programs"), false)
                        .expect("automatic directory follows the new prediction");
                assert_eq!(restored, "/programs");
                assert!(category_route_eligible(
                    &mode,
                    save_dir_untouched(&restored, offered, Some(&restored), false),
                ));
            }
        }
    }

    #[test]
    fn auto_follow_only_while_the_user_has_not_edited_the_directory() {
        // 空 / 尚未写过时等于初始目录 / 等于上次写入值：跟随预测（无预测回到初始目录）。
        assert_eq!(
            auto_follow_target("", "/dl", None, Some("/programs"), false).as_deref(),
            Some("/programs")
        );
        assert_eq!(
            auto_follow_target(" /dl ", "/dl", None, None, false).as_deref(),
            Some("/dl")
        );
        assert_eq!(
            auto_follow_target(
                "/programs",
                "/dl",
                Some("/programs"),
                Some("/videos"),
                false
            )
            .as_deref(),
            Some("/videos")
        );
        assert_eq!(
            auto_follow_target("/programs", "/dl", Some("/programs"), None, false).as_deref(),
            Some("/dl")
        );
        // Explicit manual edits and edits back to an earlier value must not be overwritten.
        assert_eq!(
            auto_follow_target("/mine", "/dl", Some("/programs"), Some("/videos"), true),
            None
        );
        assert_eq!(
            auto_follow_target("/programs", "/dl", None, Some("/videos"), true),
            None
        );
        assert_eq!(
            auto_follow_target("/dl", "/dl", Some("/programs"), Some("/videos"), true),
            None
        );
        assert!(!dir_untouched("/dl", "/dl", Some("/programs")));
        // 改成另一个分类的目录同样算手改。
        assert!(!dir_untouched("/videos", "/dl", Some("/programs")));
        assert!(dir_untouched(" /programs ", "/dl", Some("/programs")));
        assert!(dir_untouched("  ", "/dl", Some("/programs")));
    }
}
