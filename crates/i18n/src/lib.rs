//! FluxDown GPUI 客户端对公共翻译资源的零复制源复用层。
//!
//! `assets/i18n/*.json` 仍是唯一资源目录；构建脚本自动发现并嵌入语言文件。
//! 查找语义：locale 精确匹配、主语言匹配、英文
//! 键级回退、空值回退以及 `{name}` 占位插值。
//!
//! 常驻内存：进程内只有一份共享目录（[`I18nCatalog::load_embedded`]），每个语言表在首次查表时
//! 才解析，未用到的语言只占嵌入的静态源文本。

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, LazyLock, OnceLock},
};

use thiserror::Error;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded_locales.rs"));
}

/// 公共翻译资源的最终回退语言。
pub const FALLBACK_LOCALE: &str = "en";

/// 初始桌面 shell 使用的翻译键；完整键集仍可通过字符串访问。
pub mod keys {
    pub const CATEGORY_ALL: &str = "categoryAll";
    pub const CATEGORY_ARCHIVE: &str = "categoryArchive";
    pub const CATEGORY_AUDIO: &str = "categoryAudio";
    pub const CATEGORY_DOCUMENT: &str = "categoryDocument";
    pub const CATEGORY_IMAGE: &str = "categoryImage";
    pub const CATEGORY_OTHER: &str = "categoryOther";
    pub const CATEGORY_PROGRAM: &str = "categoryProgram";
    pub const CATEGORY_VIDEO: &str = "categoryVideo";
    pub const COL_CREATED: &str = "colCreated";
    pub const COL_ETA: &str = "colEta";
    pub const COL_FILE_NAME: &str = "colFileName";
    pub const COL_SIZE: &str = "colSize";
    pub const COL_SPEED: &str = "colSpeed";
    pub const COL_STATUS: &str = "colStatus";
    pub const DELETE: &str = "delete";
    pub const EMPTY_TITLE: &str = "emptyTitle";
    pub const LANGUAGE: &str = "language";
    pub const LANGUAGE_CHINESE: &str = "languageChinese";
    pub const LANGUAGE_DESC: &str = "languageDesc";
    pub const LANGUAGE_ENGLISH: &str = "languageEnglish";
    pub const LATER_QUEUE: &str = "laterQueue";
    pub const MAIN_QUEUE: &str = "mainQueue";
    pub const MENU_FILE: &str = "menuFile";
    pub const MENU_HELP: &str = "menuHelp";
    pub const MENU_TASKS: &str = "menuTasks";
    pub const MENU_TOOLS: &str = "menuTools";
    pub const MOBILE_NAV_DOWNLOADS: &str = "mobileNavDownloads";
    pub const PAUSE: &str = "pause";
    pub const RESUME: &str = "resume";
    pub const NEW_DOWNLOAD: &str = "newDownload";
    pub const SETTINGS: &str = "settings";
    pub const SETTINGS_CAT_APPEARANCE: &str = "settingsCatAppearance";
    pub const SIDEBAR_CATEGORY: &str = "sidebarCategory";
    pub const SIDEBAR_QUEUES: &str = "sidebarQueues";
    pub const SIDEBAR_STATUS: &str = "sidebarStatus";
    pub const STATUS_COMPLETED: &str = "statusCompleted";
    pub const STATUS_INCOMPLETE: &str = "statusIncomplete";
    pub const STATUS_DOWNLOADING: &str = "statusDownloading";
    pub const STATUS_ERROR: &str = "statusError";
    pub const VIEW_COLUMNS_AT_LEAST_ONE: &str = "viewColumnsAtLeastOne";
    pub const VIEW_COLUMNS_RESET_ACTION: &str = "viewColumnsResetAction";
    pub const STATUS_PAUSED: &str = "statusPaused";
    pub const STATUS_SEEDING: &str = "statusSeeding";
    pub const TAB_ALL: &str = "tabAll";
    pub const THEME_MODE: &str = "themeMode";
    pub const THEME_MODE_DARK: &str = "themeModeDark";
    pub const THEME_MODE_DESC: &str = "themeModeDesc";
    pub const THEME_MODE_LIGHT: &str = "themeModeLight";
    pub const TODAY: &str = "today";
}

/// 嵌入翻译资源无法构造成有效目录时的错误。
#[derive(Clone, Debug, Error)]
pub enum I18nError {
    #[error("duplicate embedded locale: {locale}")]
    DuplicateLocale { locale: String },
    #[error("embedded fallback locale '{locale}' is missing")]
    MissingFallback { locale: &'static str },
}

type Table = BTreeMap<String, String>;

/// 一个语言的静态 JSON 源；表与自称都在首次使用时解析。
struct LocaleSource {
    source: &'static str,
    table: OnceLock<Table>,
    native_name: OnceLock<Option<Box<str>>>,
}

impl LocaleSource {
    fn new(source: &'static str) -> Self {
        Self {
            source,
            table: OnceLock::new(),
            native_name: OnceLock::new(),
        }
    }

    /// 首次访问时解析；资源损坏时报告一次并按空表处理（逐键回退英文 / 键名）。
    fn table(&self, locale: &str) -> &Table {
        self.table.get_or_init(|| match parse_table(self.source) {
            Ok(table) => table,
            Err(error) => invalid_table(locale, &error),
        })
    }

    /// 语言选择器会列出全部语言的自称：未解析的语言只临时解析取名，不常驻整张表。
    fn native_name(&self, locale: &str) -> Option<&str> {
        self.native_name
            .get_or_init(|| {
                if let Some(table) = self.table.get() {
                    return native_name_in(table);
                }
                match parse_table(self.source) {
                    Ok(table) => native_name_in(&table),
                    Err(error) => {
                        native_name_in(self.table.get_or_init(|| invalid_table(locale, &error)))
                    }
                }
            })
            .as_deref()
    }
}

impl fmt::Debug for LocaleSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocaleSource")
            .field("parsed", &self.table.get().is_some())
            .finish_non_exhaustive()
    }
}

fn parse_table(source: &str) -> Result<Table, serde_json::Error> {
    serde_json::from_str(source)
}

/// 嵌入资源损坏（发布构建不应出现）：crate 无日志依赖，经 stderr 诊断后返回空表。
fn invalid_table(locale: &str, error: &serde_json::Error) -> Table {
    eprintln!(
        "fluxdown_ui_i18n: failed to parse embedded locale '{locale}', falling back per key: {error:#}"
    );
    Table::new()
}

fn native_name_in(table: &Table) -> Option<Box<str>> {
    table
        .get("languageNativeName")
        .filter(|value| !value.is_empty())
        .map(|value| Box::from(value.as_str()))
}

static EMBEDDED_CATALOG: LazyLock<Result<Arc<I18nCatalog>, I18nError>> =
    LazyLock::new(|| I18nCatalog::from_sources(embedded::EMBEDDED_LOCALES).map(Arc::new));

/// 不可变翻译目录；可由多个窗口、view 与线程共享，语言表按需懒解析。
#[derive(Debug)]
pub struct I18nCatalog {
    tables: BTreeMap<String, LocaleSource>,
    available: Vec<String>,
}

impl I18nCatalog {
    /// 返回进程内共享的嵌入翻译目录：首次调用登记全部嵌入语言（不解析），之后每次返回同一个
    /// `Arc`。错误只来自语言登记（重复 locale、缺英文回退），同样被缓存。
    pub fn load_embedded() -> Result<Arc<Self>, I18nError> {
        EMBEDDED_CATALOG.clone()
    }

    /// 登记语言源并校验重复 locale / 英文回退；不解析任何语言表。
    fn from_sources(sources: &[(&'static str, &'static str)]) -> Result<Self, I18nError> {
        let mut tables = BTreeMap::new();
        for (locale, source) in sources {
            let locale = normalize_locale(locale);
            if tables
                .insert(locale.clone(), LocaleSource::new(source))
                .is_some()
            {
                return Err(I18nError::DuplicateLocale { locale });
            }
        }

        if !tables.contains_key(FALLBACK_LOCALE) {
            return Err(I18nError::MissingFallback {
                locale: FALLBACK_LOCALE,
            });
        }

        let mut available = tables.keys().cloned().collect::<Vec<_>>();
        available.sort_by(|left, right| {
            locale_rank(left)
                .cmp(&locale_rank(right))
                .then_with(|| left.cmp(right))
        });

        Ok(Self { tables, available })
    }

    /// 可用 locale 顺序：`en`、`zh`，随后按代码排序。不解析语言表。
    pub fn available_locales(&self) -> &[String] {
        &self.available
    }

    /// 把系统或用户 locale 解析为实际可用代码。只比对 locale 代码，不解析语言表。
    pub fn resolve_locale<'catalog>(&'catalog self, locale: &str) -> &'catalog str {
        let normalized = normalize_locale(locale);
        if let Some((code, _)) = self.tables.get_key_value(&normalized) {
            return code;
        }

        let mut subtags = normalized.split('-');
        let prefix = subtags.next().unwrap_or(&normalized);
        if prefix == "zh" {
            let preferred = match subtags.next() {
                Some("hant" | "tw" | "hk" | "mo") => "zh-tw",
                _ => "zh",
            };
            if let Some((code, _)) = self.tables.get_key_value(preferred) {
                return code;
            }
        }
        if let Some((code, _)) = self.tables.get_key_value(prefix) {
            return code;
        }

        for code in &self.available {
            let code_prefix = code
                .split_once('-')
                .map_or(code.as_str(), |(prefix, _)| prefix);
            if code_prefix == prefix {
                return code;
            }
        }

        FALLBACK_LOCALE
    }

    /// 返回语言文件声明的自称；缺失时返回 locale 代码。未解析的语言只临时解析取名。
    pub fn native_name<'catalog>(&'catalog self, locale: &'catalog str) -> &'catalog str {
        self.tables
            .get(locale)
            .and_then(|source| source.native_name(locale))
            .unwrap_or(locale)
    }

    /// 创建持有本目录的翻译器，避免每次渲染重复解析 locale。
    pub fn translator(self: &Arc<Self>, locale: &str) -> Translator {
        Translator {
            catalog: Arc::clone(self),
            locale: self.resolve_locale(locale).to_owned(),
        }
    }

    /// 已登记 locale 的语言表；首次访问时解析。
    fn table(&self, locale: &str) -> Option<&Table> {
        self.tables
            .get_key_value(locale)
            .map(|(code, source)| source.table(code))
    }

    fn lookup_resolved<'catalog>(
        &'catalog self,
        locale: &str,
        key: &'catalog str,
    ) -> &'catalog str {
        self.table(locale)
            .and_then(|table| table.get(key))
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.table(FALLBACK_LOCALE)
                    .and_then(|table| table.get(key))
                    .filter(|value| !value.is_empty())
            })
            .map_or(key, String::as_str)
    }
}

/// 已解析 locale 的轻量翻译器；普通查表不分配。
#[derive(Clone, Debug)]
pub struct Translator {
    catalog: Arc<I18nCatalog>,
    locale: String,
}

impl Translator {
    /// 当前实际 locale 代码。
    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// 切换 locale；返回实际 locale 是否变化。
    pub fn set_locale(&mut self, locale: &str) -> bool {
        let resolved = self.catalog.resolve_locale(locale);
        if self.locale == resolved {
            return false;
        }
        self.locale.clear();
        self.locale.push_str(resolved);
        true
    }

    /// 查表；当前语言空值/缺键时回退英文，再回退键名。
    pub fn text<'translator>(&'translator self, key: &'translator str) -> &'translator str {
        self.catalog.lookup_resolved(&self.locale, key)
    }

    /// 查表并执行 `{name}` 占位插值；仅参数化字符串发生分配。
    pub fn text_with(&self, key: &str, arguments: &[(&str, &str)]) -> String {
        let mut value = self.text(key).to_owned();
        for (name, replacement) in arguments {
            let mut placeholder = String::with_capacity(name.len() + 2);
            placeholder.push('{');
            placeholder.push_str(name);
            placeholder.push('}');
            value = value.replace(&placeholder, replacement);
        }
        value
    }

    /// 当前语言的自称。
    pub fn native_name(&self) -> &str {
        self.catalog.native_name(&self.locale)
    }

    /// 目录中的全部可用 locale。
    pub fn available_locales(&self) -> &[String] {
        self.catalog.available_locales()
    }

    /// 任一可用 locale 的自称。
    pub fn native_name_of<'translator>(
        &'translator self,
        locale: &'translator str,
    ) -> &'translator str {
        self.catalog.native_name(locale)
    }
}

/// 系统界面语言：`LC_ALL` → `LC_MESSAGES` → `LANG` 中第一个非空值（去掉编码后缀）优先，
/// 便于 Unix 用户显式覆盖；三者都未设置或为 `C` / `POSIX` 时回退系统 API
/// （Windows、从 Finder 启动的 macOS），仍取不到为 `en`。偏好 `general.locale` 为
/// `system` 时使用，桌面、设置页与 agent 托盘共用同一推导，保证文案同语言。
#[must_use]
pub fn system_locale() -> String {
    resolve_system_locale(|key| std::env::var(key).ok(), sys_locale::get_locale)
}

fn resolve_system_locale(
    env: impl Fn(&str) -> Option<String>,
    system: impl FnOnce() -> Option<String>,
) -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(env)
        .find(|value| !value.trim().is_empty())
        .and_then(|value| clean_locale(&value))
        .or_else(|| system().and_then(|value| clean_locale(&value)))
        .unwrap_or_else(|| "en".to_owned())
}

fn clean_locale(raw: &str) -> Option<String> {
    let locale = raw.split('.').next()?.trim();
    (!locale.is_empty() && locale != "C" && locale != "POSIX").then(|| locale.to_owned())
}

fn normalize_locale(locale: &str) -> String {
    locale.trim().to_ascii_lowercase().replace('_', "-")
}

fn locale_rank(locale: &str) -> u8 {
    match locale {
        "en" => 0,
        "zh" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, sync::Arc};

    use super::{I18nCatalog, I18nError, embedded, parse_table, resolve_system_locale};

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn system_locale_prefers_env_then_system_api() {
        let sys = || Some("zh-Hans-CN".to_owned());
        assert_eq!(
            resolve_system_locale(env_of(&[("LANG", "fr_FR.UTF-8")]), sys),
            "fr_FR"
        );
        assert_eq!(
            resolve_system_locale(env_of(&[("LC_ALL", ""), ("LANG", "C")]), sys),
            "zh-Hans-CN"
        );
        assert_eq!(resolve_system_locale(env_of(&[]), sys), "zh-Hans-CN");
        assert_eq!(resolve_system_locale(env_of(&[]), || None), "en");
    }

    #[test]
    fn system_locale_forms_normalize_to_zh() -> Result<(), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;
        assert_eq!(catalog.resolve_locale("zh-Hans-CN"), "zh");
        assert_eq!(catalog.resolve_locale("zh_CN"), "zh");
        Ok(())
    }

    #[test]
    fn chinese_scripts_take_precedence_over_regions() -> Result<(), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;
        for locale in [
            "zh_TW",
            "zh-Hant",
            "zh-Hant-TW",
            "zh-Hant-CN",
            "zh-HK",
            "zh-MO",
            "zh-TW-u-nu-hanidec",
            "zh-HK-u-nu-hanidec",
            " ZH_hant_hk ",
        ] {
            assert_eq!(catalog.resolve_locale(locale), "zh-tw", "{locale}");
        }
        for locale in [
            "zh",
            "zh-CN",
            "zh-SG",
            "zh-Hans",
            "zh-Hans-TW",
            "zh-hantfoo",
        ] {
            assert_eq!(catalog.resolve_locale(locale), "zh", "{locale}");
        }
        Ok(())
    }

    #[test]
    fn traditional_chinese_falls_back_when_not_bundled() -> Result<(), I18nError> {
        let catalog = I18nCatalog::from_sources(&[("en", "{}"), ("zh", "{}")])?;
        assert_eq!(catalog.resolve_locale("zh-Hant-TW"), "zh");
        assert_eq!(catalog.resolve_locale("zh-HK"), "zh");
        Ok(())
    }

    #[test]
    fn resolves_exact_prefix_and_fallback_locales() -> Result<(), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;

        assert_eq!(catalog.resolve_locale("zh_CN"), "zh");
        assert_eq!(catalog.resolve_locale("en-US"), "en");
        assert_eq!(catalog.resolve_locale("not-a-locale"), "en");
        Ok(())
    }

    #[test]
    fn falls_back_per_key_and_interpolates() -> Result<(), I18nError> {
        let catalog = Arc::new(I18nCatalog::from_sources(&[
            (
                "en",
                r#"{"languageNativeName":"English","hello":"Hello {name}","fallback":"Fallback"}"#,
            ),
            (
                "zh",
                r#"{"languageNativeName":"中文","hello":"你好，{name}","fallback":""}"#,
            ),
        ])?);
        let translator = catalog.translator("zh-CN");

        assert_eq!(
            translator.text_with("hello", &[("name", "FluxDown")]),
            "你好，FluxDown"
        );
        assert_eq!(translator.text("fallback"), "Fallback");
        assert_eq!(translator.text("missing"), "missing");
        Ok(())
    }

    #[test]
    fn baseline_locales_have_matching_keys() -> Result<(), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;
        let english = catalog
            .table("en")
            .map(|table| table.keys().map(String::as_str).collect::<BTreeSet<_>>());
        let chinese = catalog
            .table("zh")
            .map(|table| table.keys().map(String::as_str).collect::<BTreeSet<_>>());

        assert_eq!(english, chinese);
        Ok(())
    }

    /// 语言表懒解析，损坏的嵌入资源要到运行时才暴露；这里逐个解析，测试期即发现。
    #[test]
    fn every_embedded_locale_parses() {
        let failures = embedded::EMBEDDED_LOCALES
            .iter()
            .filter_map(|(locale, source)| {
                parse_table(source)
                    .err()
                    .map(|error| format!("{locale}: {error:#}"))
            })
            .collect::<Vec<_>>();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn only_queried_locales_are_parsed() -> Result<(), I18nError> {
        let catalog = Arc::new(I18nCatalog::from_sources(&[
            ("en", r#"{"languageNativeName":"English","hello":"Hello"}"#),
            ("zh", r#"{"languageNativeName":"中文","hello":"你好"}"#),
            (
                "ja",
                r#"{"languageNativeName":"日本語","hello":"こんにちは"}"#,
            ),
        ])?);
        let parsed = |locale: &str| {
            catalog
                .tables
                .get(locale)
                .is_some_and(|source| source.table.get().is_some())
        };

        assert_eq!(catalog.available_locales(), ["en", "zh", "ja"]);
        assert_eq!(catalog.resolve_locale("ja-JP"), "ja");
        assert_eq!(catalog.native_name("ja"), "日本語");
        let translator = catalog.translator("zh-CN");
        assert!(!parsed("en") && !parsed("zh") && !parsed("ja"));

        assert_eq!(translator.text("hello"), "你好");
        assert!(parsed("zh"));
        assert!(!parsed("ja"), "non-active locale must stay unparsed");
        assert!(!parsed("en"), "fallback is parsed only on a missing key");

        assert_eq!(translator.text("missing"), "missing");
        assert!(parsed("en"));
        assert!(!parsed("ja"));
        Ok(())
    }

    /// `{name}` / `{task.fileName}` 形式的占位符（JSON 示例等含引号空格的花括号不算）。
    fn placeholders(value: &str) -> BTreeSet<&str> {
        let mut found = BTreeSet::new();
        let mut rest = value;
        while let Some(start) = rest.find('{') {
            let Some(after) = rest.get(start + 1..) else {
                break;
            };
            let Some(end) = after.find('}') else {
                break;
            };
            let name = after.get(..end).unwrap_or_default();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            {
                found.insert(name);
            }
            rest = after;
        }
        found
    }

    /// 非基线语言（翻译自动化 / 人工维护）不能引入调用方不传的占位符（会原样显示 `{x}`），
    /// 也不能丢掉 en 与 zh 都保留的占位符（会丢信息）。
    #[test]
    fn translated_locales_keep_baseline_placeholders() -> Result<(), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;
        let (english, chinese) = (catalog.table("en"), catalog.table("zh"));
        assert!(
            english.is_some() && chinese.is_some(),
            "baseline locales must be bundled"
        );
        let (Some(english), Some(chinese)) = (english, chinese) else {
            return Ok(());
        };
        let mut violations = Vec::new();
        for (locale, source) in &catalog.tables {
            let table = source.table(locale);
            if locale == "en" || locale == "zh" {
                continue;
            }
            for (key, value) in table {
                let Some(en_value) = english.get(key) else {
                    continue;
                };
                let en_names = placeholders(en_value);
                let zh_names = chinese
                    .get(key)
                    .map(|v| placeholders(v))
                    .unwrap_or_default();
                let names = placeholders(value);
                let unknown: Vec<_> = names
                    .iter()
                    .filter(|n| !en_names.contains(*n) && !zh_names.contains(*n))
                    .collect();
                let dropped: Vec<_> = en_names
                    .intersection(&zh_names)
                    .filter(|n| !names.contains(*n))
                    .collect();
                if !unknown.is_empty() || !dropped.is_empty() {
                    violations.push(format!(
                        "{locale}.{key}: unknown {unknown:?}, dropped {dropped:?}"
                    ));
                }
            }
        }
        assert!(violations.is_empty(), "{violations:#?}");
        Ok(())
    }
}
