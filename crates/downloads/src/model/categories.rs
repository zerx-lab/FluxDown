//! 分类匹配索引：由偏好 `custom_categories` 构建，按扩展名 / 正则 / 网址通配符把任务归类。

use std::collections::HashSet;

use fluxdown_protocol::CustomCategoryDto;

use super::DownloadTaskView;

/// `url` 模式通配符 → 锚定正则：`*` 任意串、`?` 恰一个字符，其余字面；不分大小写。
/// 空 / 非法模式返回 `None`。语义与引擎 `category_route.rs::wildcard_matches` 一致。
fn wildcard_regex(pattern: &str) -> Option<regex::Regex> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return None;
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
        .ok()
}

/// 通配符模式是否命中网址（语义见 [`wildcard_regex`]）；本 crate 各网址匹配入口共用。
pub(crate) fn wildcard_matches(pattern: &str, url: &str) -> bool {
    wildcard_regex(pattern).is_some_and(|regex| regex.is_match(url))
}

enum Matcher {
    /// `builtin_all`：匹配所有任务。
    All,
    /// `builtin_other`：不命中任何其他分类的任务。
    Other,
    Extensions(HashSet<String>),
    /// 编译失败视为不匹配。
    Regex(Option<regex::Regex>),
    /// `url` 模式：通配符（`*` / `?`，不分大小写、锚定完整匹配）匹配来源网址，
    /// 语义与引擎 `category_route.rs::wildcard_matches` 一致；编译失败视为不匹配。
    Url(Option<regex::Regex>),
}

pub(crate) struct CategoryRule {
    pub(crate) dto: CustomCategoryDto,
    matcher: Matcher,
}

/// 按 `position` 排序的分类规则集合。
#[derive(Default)]
pub(crate) struct CategoryIndex {
    rules: Vec<CategoryRule>,
    /// 与 `rules` 一一对应：该规则是否是其分类 id 的首条（重复 id 只认首条）。
    first_with_id: Vec<bool>,
}

impl CategoryIndex {
    pub(crate) fn from_preference(value: Option<&serde_json::Value>) -> Self {
        Self::from_dtos(CustomCategoryDto::from_preference(value))
    }

    pub(crate) fn from_dtos(dtos: Vec<CustomCategoryDto>) -> Self {
        let rules: Vec<CategoryRule> = dtos
            .into_iter()
            .map(|dto| {
                let matcher = match (dto.builtin_type.as_deref(), dto.match_mode.as_str()) {
                    (Some("all"), _) => Matcher::All,
                    (Some("other"), _) => Matcher::Other,
                    // 与 agent 分类目录解析一致：正则不区分
                    // 大小写，空模式不匹配（Rust 空正则会匹配一切）。
                    (_, "regex") => Matcher::Regex(
                        (!dto.regex_pattern.is_empty())
                            .then(|| {
                                regex::RegexBuilder::new(&dto.regex_pattern)
                                    .case_insensitive(true)
                                    .build()
                                    .ok()
                            })
                            .flatten(),
                    ),
                    (_, "url") => Matcher::Url(wildcard_regex(&dto.url_pattern)),
                    _ => Matcher::Extensions(
                        dto.extensions
                            .iter()
                            .map(|ext| ext.trim_start_matches('.').to_ascii_lowercase())
                            .collect(),
                    ),
                };
                CategoryRule { dto, matcher }
            })
            .collect();
        let mut seen = HashSet::with_capacity(rules.len());
        let first_with_id = rules
            .iter()
            .map(|rule| seen.insert(rule.dto.id.as_str()))
            .collect();
        Self {
            rules,
            first_with_id,
        }
    }

    pub(crate) fn rules(&self) -> &[CategoryRule] {
        &self.rules
    }

    /// 可见规则（`visible=false` 的隐藏）。
    pub(crate) fn visible(&self) -> impl Iterator<Item = &CategoryRule> {
        self.rules.iter().filter(|rule| rule.dto.visible)
    }

    fn rule_matches(rule: &CategoryRule, task: &DownloadTaskView) -> bool {
        match &rule.matcher {
            Matcher::All => true,
            Matcher::Other => false,
            Matcher::Extensions(extensions) => task
                .extension()
                .is_some_and(|extension| extensions.contains(extension)),
            Matcher::Regex(regex) => regex
                .as_ref()
                .is_some_and(|regex| regex.is_match(&task.name)),
            Matcher::Url(regex) => regex
                .as_ref()
                .is_some_and(|regex| regex.is_match(task.share_url())),
        }
    }

    /// 任务是否属于分类 `id`；未知 id 视为不匹配。
    pub(crate) fn matches(&self, id: &str, task: &DownloadTaskView) -> bool {
        let Some(rule) = self.rules.iter().find(|rule| rule.dto.id == id) else {
            return false;
        };
        if !task.category_override.is_empty() {
            return task.category_override == id || matches!(rule.matcher, Matcher::All);
        }
        match rule.matcher {
            Matcher::Other => !self
                .rules
                .iter()
                .filter(|other| !matches!(other.matcher, Matcher::All | Matcher::Other))
                .any(|other| Self::rule_matches(other, task)),
            _ => Self::rule_matches(rule, task),
        }
    }

    /// 任务命中的首个（非 all/other）分类 id；无 → `builtin_other`。
    /// 网址规则固定优先：先在网址规则里找首条命中，再退回文件名规则（列表顺序），
    /// 与引擎落点路由 `save_dir_for` 的优先级一致。
    pub(crate) fn category_of<'a>(&'a self, task: &'a DownloadTaskView) -> &'a str {
        if !task.category_override.is_empty() {
            return &task.category_override;
        }
        let specific = || {
            self.rules
                .iter()
                .filter(|rule| !matches!(rule.matcher, Matcher::All | Matcher::Other))
        };
        let hit = specific()
            .find(|rule| matches!(rule.matcher, Matcher::Url(_)) && Self::rule_matches(rule, task))
            .or_else(|| {
                specific().find(|rule| {
                    !matches!(rule.matcher, Matcher::Url(_)) && Self::rule_matches(rule, task)
                })
            });
        hit.map_or("builtin_other", |rule| rule.dto.id.as_str())
    }

    /// 对任务命中的每个分类 id 调用一次 `emit`：与对每个不同 id 逐个调用 [`Self::matches`]
    /// 等价（重复 id 只认首条规则；`other` 仅在没有任何具体分类命中时命中），但任务只
    /// 对各规则求值一次，供一次扫描得出全部分类计数。
    pub(crate) fn for_each_match<'a>(
        &'a self,
        task: &DownloadTaskView,
        mut emit: impl FnMut(&'a str),
    ) {
        let mut specific_hit = !task.category_override.is_empty();
        let mut other_ids: Vec<&'a str> = Vec::new();
        for (rule, first_with_id) in self.rules.iter().zip(&self.first_with_id) {
            let is_other = matches!(rule.matcher, Matcher::Other);
            let hit = if task.category_override.is_empty() {
                !is_other && Self::rule_matches(rule, task)
            } else {
                rule.dto.id == task.category_override || matches!(rule.matcher, Matcher::All)
            };
            specific_hit |= hit && (!is_other || !task.category_override.is_empty());
            if !first_with_id {
                continue;
            }
            if is_other {
                other_ids.push(&rule.dto.id);
            } else if hit {
                emit(&rule.dto.id);
            }
        }
        if !specific_hit {
            for id in other_ids {
                emit(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::CustomCategoryDto;

    use super::CategoryIndex;
    use crate::model::DownloadTaskView;

    fn task(name: &str) -> DownloadTaskView {
        let dto = serde_json::from_value::<fluxdown_protocol::TaskDto>(serde_json::json!({
            "taskId":"t","url":"https://example.com/x","fileName":name,
            "saveDir":"/tmp","status":3,"downloadedBytes":1,"totalBytes":1,
            "errorMessage":"","createdAt":"1","proxyUrl":"","queueId":"main","checksum":""
        }))
        .expect("task");
        DownloadTaskView::local(&dto, None, false)
    }

    #[test]
    fn builtin_extension_and_other_matching() {
        let index = CategoryIndex::from_dtos(CustomCategoryDto::builtin_defaults());
        assert!(index.matches("builtin_all", &task("a.mp4")));
        assert!(index.matches("builtin_video", &task("a.MP4")));
        assert!(!index.matches("builtin_video", &task("a.zip")));
        assert!(index.matches("builtin_other", &task("README")));
        assert!(!index.matches("builtin_other", &task("a.zip")));
        assert_eq!(index.category_of(&task("a.zip")), "builtin_archive");
    }

    #[test]
    fn regex_category_and_invalid_pattern() {
        let mut dtos = CustomCategoryDto::builtin_defaults();
        dtos.push(CustomCategoryDto {
            id: "c1".to_owned(),
            name: "Episodes".to_owned(),
            icon: "file".to_owned(),
            match_mode: "regex".to_owned(),
            extensions: Vec::new(),
            regex_pattern: r"S\d+E\d+".to_owned(),
            url_pattern: String::new(),
            position: 9,
            visible: true,
            is_builtin: false,
            builtin_type: None,
            save_dir: String::new(),
        });
        dtos.push(CustomCategoryDto {
            id: "bad".to_owned(),
            name: "Bad".to_owned(),
            icon: "file".to_owned(),
            match_mode: "regex".to_owned(),
            extensions: Vec::new(),
            regex_pattern: "(".to_owned(),
            url_pattern: String::new(),
            position: 10,
            visible: true,
            is_builtin: false,
            builtin_type: None,
            save_dir: String::new(),
        });
        dtos.push(CustomCategoryDto {
            id: "empty".to_owned(),
            name: "Empty".to_owned(),
            icon: "file".to_owned(),
            match_mode: "regex".to_owned(),
            extensions: Vec::new(),
            regex_pattern: String::new(),
            url_pattern: String::new(),
            position: 11,
            visible: true,
            is_builtin: false,
            builtin_type: None,
            save_dir: String::new(),
        });
        let index = CategoryIndex::from_dtos(dtos);
        assert!(index.matches("c1", &task("show.S01E02.mkv")));
        assert!(index.matches("c1", &task("show.s01e02.mkv")));
        assert!(!index.matches("bad", &task("anything")));
        assert!(!index.matches("empty", &task("anything.mkv")));
    }

    #[test]
    fn url_wildcard_category_matches_origin_url() {
        fn task_at(file_name: &str, url: &str, origin_url: &str) -> DownloadTaskView {
            let dto = serde_json::from_value::<fluxdown_protocol::TaskDto>(serde_json::json!({
                "taskId":"t","url":url,"originUrl":origin_url,"fileName":file_name,
                "saveDir":"/tmp","status":3,"downloadedBytes":1,"totalBytes":1,
                "errorMessage":"","createdAt":"1","proxyUrl":"","queueId":"main","checksum":""
            }))
            .expect("task");
            DownloadTaskView::local(&dto, None, false)
        }
        let mut dtos = CustomCategoryDto::builtin_defaults();
        dtos.push(CustomCategoryDto {
            id: "site".to_owned(),
            name: "Site".to_owned(),
            icon: "globe".to_owned(),
            match_mode: "url".to_owned(),
            extensions: Vec::new(),
            regex_pattern: String::new(),
            url_pattern: "https://mirror.test/*".to_owned(),
            position: 9,
            visible: true,
            is_builtin: false,
            builtin_type: None,
            save_dir: String::new(),
        });
        dtos.push(CustomCategoryDto {
            id: "bad".to_owned(),
            name: "Bad".to_owned(),
            icon: "globe".to_owned(),
            match_mode: "url".to_owned(),
            extensions: Vec::new(),
            regex_pattern: String::new(),
            url_pattern: "(".to_owned(),
            position: 10,
            visible: true,
            is_builtin: false,
            builtin_type: None,
            save_dir: String::new(),
        });
        let index = CategoryIndex::from_dtos(dtos);
        assert!(index.matches(
            "site",
            &task_at("a.zip", "https://x.test/a.zip", "https://mirror.test/a.zip")
        ));
        // 无 origin 时回退 url；锚定匹配不分大小写。
        assert!(index.matches("site", &task_at("a.zip", "https://MIRROR.TEST/a.zip", "")));
        assert!(!index.matches(
            "site",
            &task_at("a.zip", "https://x.test/https://mirror.test/a", "")
        ));
        // 非法模式不命中。
        assert!(!index.matches("bad", &task_at("a.zip", "https://mirror.test/a.zip", "")));
        // 网址规则固定优先：即使排在压缩包后面，命中网址的 zip 也归入网址分类。
        assert_eq!(
            index.category_of(&task_at("blob", "https://mirror.test/a.zip", "")),
            "site"
        );
        assert_eq!(
            index.category_of(&task_at("a.zip", "https://mirror.test/a.zip", "")),
            "site"
        );
        // 其他站点的 zip 照旧按扩展名归入压缩包。
        assert_eq!(
            index.category_of(&task_at("a.zip", "https://other.test/a.zip", "")),
            "builtin_archive"
        );
    }
}
