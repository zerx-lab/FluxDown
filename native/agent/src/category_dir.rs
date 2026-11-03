//! 外部捕获的分类保存目录：由 `custom_categories` 偏好构建随建任务交给 daemon 的路由规则
//! （[`CategoryRouteDto`]），并为确认表单按捕获名预填命中的分类目录。
//!
//! 匹配语义与引擎 `category_route.rs` 逐条对齐（引擎在建任务与下载完成时按同一规则定落点），
//! 两端共用夹具 `native/engine/src/fixtures/category_route_cases.json`；原生 Android
//! `Categories.kt::CategoryRules.saveDirFor` 镜像同一语义。只看可见分类，按 `position`
//! 顺序；先找首个配置了目录且命中的普通分类（非 all / other），否则当文件不命中任何普通
//! 分类时取 `other` 的目录。文件名为空或不含 `.` 时用 URL 路径末段（解码后且含 `.`）参与
//! 匹配。扩展名比较不区分大小写，正则不区分大小写，非法正则视为不命中；`url` 模式按
//! 通配符（`*` / `?`，不分大小写、锚定完整匹配）匹配来源网址，与文件名规则同序竞争。

use std::collections::BTreeMap;

use fluxdown_protocol::{CUSTOM_CATEGORIES_PREF_KEY, CategoryRouteDto, CategoryRuleDto};
use serde_json::Value;

/// 偏好中的授权路由；无分类规则时仍返回空规则路由，供插件归档分类使用。
pub(crate) fn category_route(preferences: &BTreeMap<String, Value>) -> Option<CategoryRouteDto> {
    CategoryRouteDto::from_preference(preferences.get(CUSTOM_CATEGORIES_PREF_KEY))
}

/// 命中的分类保存目录；无命中 / 命中分类未配置目录时返回 `None`。
///
/// 网址规则固定优先（不看列表位置）：先按列表顺序取首个命中且配置了目录的 `url`
/// 规则，命中即定落点；扩展名 / 正则分类只处理未被网址规则认领的任务。
#[must_use]
pub fn route_save_dir<'a>(
    route: &'a CategoryRouteDto,
    file_name: &str,
    url: &str,
) -> Option<&'a str> {
    if let Some(rule) = route.rules.iter().find(|rule| {
        rule.match_mode == "url"
            && !rule.save_dir.is_empty()
            && wildcard_matches(&rule.url_pattern, url)
    }) {
        return Some(rule.save_dir.as_str());
    }
    let url_name;
    let name = if file_name.contains('.') {
        file_name
    } else {
        url_name = file_name_from_url(url);
        url_name.as_deref().unwrap_or(file_name)
    };
    // 文件名未知时只有网址规则参与：扩展名 / 正则与「其他」兜底保持原语义（不路由）。
    if name.is_empty() {
        return None;
    }
    if let Some(rule) = route
        .rules
        .iter()
        .find(|rule| !rule.save_dir.is_empty() && matches(rule, name, url))
    {
        return Some(rule.save_dir.as_str());
    }
    (!route.other_dir.is_empty() && !route.rules.iter().any(|rule| matches(rule, name, url)))
        .then_some(route.other_dir.as_str())
}

fn matches(rule: &CategoryRuleDto, name: &str, url: &str) -> bool {
    if rule.match_mode == "url" {
        return wildcard_matches(&rule.url_pattern, url);
    }
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

/// 通配符网址匹配，语义与引擎 `category_route.rs::wildcard_matches` 一致：
/// `*` 任意串、`?` 恰一个字符，其余字面；不分大小写；锚定完整匹配整个网址；
/// 空 / 非法模式不命中。
fn wildcard_matches(pattern: &str, url: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    let mut source = String::with_capacity(pattern.len() + 2);
    source.push('^');
    for ch in pattern.chars() {
        match ch {
            '*' => source.push_str(".*"),
            '?' => source.push('.'),
            other => source.push_str(&regex::escape(&other.to_string())),
        }
    }
    source.push('$');
    regex::RegexBuilder::new(&source)
        .case_insensitive(true)
        .build()
        .is_ok_and(|regex| regex.is_match(url))
}

/// URL 路径末段（百分号解码）且含 `.` 时作为文件名。
fn file_name_from_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    let last = parsed.path_segments()?.next_back()?;
    let decoded = percent_decode(last);
    decoded.contains('.').then_some(decoded)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(byte) = bytes
                .get(index + 1..index + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::CategoryRouteDto;
    use serde_json::json;

    use super::{category_route, route_save_dir};

    fn prefs(
        categories: serde_json::Value,
    ) -> std::collections::BTreeMap<String, serde_json::Value> {
        std::collections::BTreeMap::from([("custom_categories".to_owned(), categories)])
    }

    fn category(
        id: &str,
        builtin: Option<&str>,
        extensions: &[&str],
        save_dir: &str,
        position: i64,
    ) -> serde_json::Value {
        json!({
            "id": id, "name": id, "matchMode": "extension", "extensions": extensions,
            "position": position, "visible": true, "isBuiltin": builtin.is_some(),
            "builtinType": builtin, "saveDir": save_dir,
        })
    }

    fn save_dir(
        preferences: &std::collections::BTreeMap<String, serde_json::Value>,
        file_name: &str,
        url: &str,
    ) -> Option<String> {
        let route = category_route(preferences)?;
        route_save_dir(&route, file_name, url).map(str::to_owned)
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        name: String,
        route: CategoryRouteDto,
        file_name: String,
        url: String,
        expected: Option<String>,
    }

    /// 与引擎 `category_route.rs` 共用的跨实现用例。
    #[test]
    fn shared_cases_match_engine() {
        let cases: Vec<Case> = serde_json::from_str(include_str!(
            "../../engine/src/fixtures/category_route_cases.json"
        ))
        .expect("category route fixture");
        assert!(!cases.is_empty());
        for case in cases {
            assert_eq!(
                route_save_dir(&case.route, &case.file_name, &case.url),
                case.expected.as_deref(),
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn route_follows_visible_categories_in_position_order() {
        let preferences = prefs(json!([
            category("video2", None, &["mp4"], "/second", 5),
            category("video", Some("video"), &["mp4", "mkv"], "/videos", 1),
            category("docs", Some("document"), &["pdf"], "", 2),
        ]));
        assert_eq!(
            save_dir(&preferences, "Movie.MP4", "").as_deref(),
            Some("/videos")
        );
        assert_eq!(save_dir(&preferences, "a.pdf", ""), None);
    }

    #[test]
    fn other_dir_applies_only_when_no_normal_category_matches() {
        let preferences = prefs(json!([
            category("video", Some("video"), &["mp4"], "", 1),
            category("other", Some("other"), &[], "/other", 7),
        ]));
        assert_eq!(
            save_dir(&preferences, "setup.exe", "").as_deref(),
            Some("/other")
        );
        assert_eq!(save_dir(&preferences, "clip.mp4", ""), None);
    }

    #[test]
    fn hidden_categories_are_ignored() {
        let mut hidden = category("music", None, &["flac"], "/music-hidden", 1);
        hidden["visible"] = json!(false);
        let preferences = prefs(json!([
            hidden,
            category("audio", Some("audio"), &["flac"], "/music", 2),
        ]));
        assert_eq!(
            save_dir(&preferences, "", "https://x.test/a/My%20Song.flac?x=1").as_deref(),
            Some("/music")
        );
    }
}
