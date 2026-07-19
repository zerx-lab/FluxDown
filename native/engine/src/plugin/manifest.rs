//! 插件 manifest（`manifest.json`）的 serde 类型 + 手写校验器 + `url_glob_match`。
//!
//! **不引入** jsonschema/globset/regex（依赖约束）。校验器为闭合枚举 match。
//!
//! ## `pattern` 校验的 v1 说明（记录在案的偏离）
//! 计划原文写「pattern 为 Rust regex 语法」，但依赖约束禁止新增 `regex` crate。
//! 由于 `plugins` feature 恒带 rquickjs（一个完整 JS 引擎），本实现将 `pattern`
//! 语义改为 **JS RegExp 语法**（在插件运行时用 `new RegExp(pattern)` 编译、
//! `RegExp.test(value)` 匹配，见 [`super::manager`]）——对写 JS 的插件作者更自然。
//! 本文件（纯模块）只做结构校验：`pattern` 仅 string 有效、非空即存储；实际编译/
//! 匹配在持有 runtime 的 manager 侧完成。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::runtime::PluginError;

/// 设置项数据类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingType {
    String,
    Number,
    Boolean,
}

impl SettingType {
    fn as_str(self) -> &'static str {
        match self {
            SettingType::String => "string",
            SettingType::Number => "number",
            SettingType::Boolean => "boolean",
        }
    }
}

/// 设置项 UI 控件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingWidget {
    Text,
    Password,
    Textarea,
    Select,
    Toggle,
    Number,
    Folder,
}

impl SettingWidget {
    fn as_str(self) -> &'static str {
        match self {
            SettingWidget::Text => "text",
            SettingWidget::Password => "password",
            SettingWidget::Textarea => "textarea",
            SettingWidget::Select => "select",
            SettingWidget::Toggle => "toggle",
            SettingWidget::Number => "number",
            SettingWidget::Folder => "folder",
        }
    }
}

/// select 控件的选项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingOption {
    pub value: String,
    pub label: String,
}

/// 声明式设置项。engine 侧唯一校验语义源。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingField {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "type")]
    pub ty: SettingType,
    /// 缺省按 type 推导（string→text、number→number、boolean→toggle）。
    #[serde(default)]
    pub widget: Option<SettingWidget>,
    #[serde(default)]
    pub options: Vec<SettingOption>,
    /// 默认值（跨端一律字符串序列化）。
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub required: bool,
    /// 仅 number 有效，闭区间下界。
    #[serde(default)]
    pub min: Option<f64>,
    /// 仅 number 有效，闭区间上界。
    #[serde(default)]
    pub max: Option<f64>,
    /// 仅 string 有效，JS RegExp 语法（见模块文档）。
    #[serde(default)]
    pub pattern: Option<String>,
    /// 可选「辅助脚本」：非空时宿主在该字段旁渲染一个复制按钮，把脚本原文
    /// 复制到剪贴板，供用户粘贴到浏览器开发者工具 Console 执行（典型用途：
    /// 在目标站点上提取 document.cookie 填入 cookie 设置）。宿主绝不执行该
    /// 脚本，仅复制文本。仅 string 类型字段有效。
    #[serde(default)]
    pub helper_script: Option<String>,
    /// 辅助脚本按钮文案（空则宿主用默认文案「复制获取脚本」）。
    #[serde(default)]
    pub helper_label: Option<String>,
}

impl SettingField {
    /// widget 缺省按 type 推导。
    pub fn effective_widget(&self) -> SettingWidget {
        self.widget.unwrap_or(match self.ty {
            SettingType::String => SettingWidget::Text,
            SettingType::Number => SettingWidget::Number,
            SettingType::Boolean => SettingWidget::Toggle,
        })
    }
}

/// resolver 的 URL 匹配声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchDecl {
    pub urls: Vec<String>,
}

/// 单个 resolver 声明。v1 每插件至多一个。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolverDecl {
    #[serde(rename = "match")]
    pub match_decl: MatchDecl,
    pub entry: String,
    /// 单次 resolve 超时（毫秒）。可下调宿主预算，不可超 30s 硬顶。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// 声明该 resolver 支持多文件清单前置预解析（`ResolveResult::manifest`）。
    /// `false`（默认）的单文件插件不会被前置预解析无谓触发（避免解析昂贵的
    /// 单文件插件白跑一次）；未声明却在 start 阶段返回清单的插件仍由引擎
    /// `on_resolve_ready` 的自动裂变兜底（D6），不依赖本字段。
    #[serde(default)]
    pub multi: bool,
}

/// hooks 声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HooksDecl {
    pub entry: String,
    pub events: Vec<String>,
    #[serde(rename = "match", default)]
    pub match_decl: Option<MatchDecl>,
}

/// 插件 manifest。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    pub identity: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub icon: String,
    /// 宿主版本门槛，加载时三段整数比较。
    #[serde(default)]
    pub min_app_version: String,
    #[serde(default)]
    pub resolvers: Vec<ResolverDecl>,
    #[serde(default)]
    pub hooks: Option<HooksDecl>,
    #[serde(default)]
    pub settings: Vec<SettingField>,
    /// 声明式能力权限（v1 仅 `"ffmpeg"`）。空 = 无额外能力。授予的能力经宿主
    /// 门控注入对应 `flux.*` 门面（见 [`super::runtime::HostContext`]）。
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// 合法事件名（manifest `hooks.events`）。
pub const VALID_EVENTS: [&str; 4] = ["onStart", "onError", "onDone", "onMetaProbed"];

/// ffmpeg 能力权限名（manifest `permissions`）。
pub const PERMISSION_FFMPEG: &str = "ffmpeg";
/// yt-dlp 能力权限名（manifest `permissions`）。
pub const PERMISSION_YTDLP: &str = "ytdlp";
/// 合法能力权限（manifest `permissions`）。
pub const VALID_PERMISSIONS: [&str; 2] = [PERMISSION_FFMPEG, PERMISSION_YTDLP];

impl PluginManifest {
    /// 从 JSON 字节解析（不校验语义，仅结构）。
    pub fn parse(bytes: &[u8]) -> Result<Self, PluginError> {
        serde_json::from_slice(bytes)
            .map_err(|e| PluginError::ManifestInvalid(format!("JSON 解析失败: {e}")))
    }

    /// 手写语义校验（闭合枚举 match）。
    pub fn validate(&self) -> Result<(), PluginError> {
        // identity：^[a-z0-9_-]+@[a-z0-9_-]+$，禁 '.'（防 config 键分隔符碰撞）。
        if !is_valid_identity(&self.identity) {
            return Err(PluginError::ManifestInvalid(format!(
                "identity '{}' 非法：须匹配 ^[a-z0-9_-]+@[a-z0-9_-]+$",
                self.identity
            )));
        }
        if self.name.trim().is_empty() {
            return Err(PluginError::ManifestInvalid("name 不可为空".to_string()));
        }
        if super::semver::parse_semver(&self.version).is_none() {
            return Err(PluginError::ManifestInvalid(format!(
                "version '{}' 非法：须为 MAJOR.MINOR.PATCH",
                self.version
            )));
        }
        if !self.min_app_version.is_empty()
            && super::semver::parse_semver(&self.min_app_version).is_none()
        {
            return Err(PluginError::ManifestInvalid(format!(
                "minAppVersion '{}' 非法",
                self.min_app_version
            )));
        }
        if !self.icon.is_empty() && !is_safe_relative_path(&self.icon) {
            return Err(PluginError::ManifestInvalid(format!(
                "icon 路径 '{}' 非法（禁 ..、绝对路径、盘符、分隔符开头）",
                self.icon
            )));
        }

        // resolvers：v1 长度必须 == 1（若声明了 resolver）。允许 0（纯 hook 插件）。
        if self.resolvers.len() > 1 {
            return Err(PluginError::ManifestInvalid(
                "v1 每插件至多一个 resolver".to_string(),
            ));
        }
        for r in &self.resolvers {
            if !is_safe_relative_path(&r.entry) {
                return Err(PluginError::ManifestInvalid(format!(
                    "resolver entry 路径 '{}' 非法",
                    r.entry
                )));
            }
            if r.match_decl.urls.is_empty() {
                return Err(PluginError::ManifestInvalid(
                    "resolver match.urls 不可为空".to_string(),
                ));
            }
            if let Some(t) = r.timeout_ms
                && t == 0
            {
                return Err(PluginError::ManifestInvalid(
                    "resolver timeoutMs 不可为 0".to_string(),
                ));
            }
        }

        // hooks。
        if let Some(h) = &self.hooks {
            if !is_safe_relative_path(&h.entry) {
                return Err(PluginError::ManifestInvalid(format!(
                    "hooks entry 路径 '{}' 非法",
                    h.entry
                )));
            }
            if h.events.is_empty() {
                return Err(PluginError::ManifestInvalid(
                    "hooks.events 不可为空".to_string(),
                ));
            }
            for ev in &h.events {
                if !VALID_EVENTS.contains(&ev.as_str()) {
                    return Err(PluginError::ManifestInvalid(format!(
                        "未知事件 '{ev}'，合法：{VALID_EVENTS:?}"
                    )));
                }
            }
            if let Some(m) = &h.match_decl
                && m.urls.is_empty()
            {
                return Err(PluginError::ManifestInvalid(
                    "hooks match.urls 不可为空".to_string(),
                ));
            }
        }

        // settings：键唯一 + widget×type 矩阵 + 值域约束。
        let mut seen: HashSet<&str> = HashSet::new();
        for f in &self.settings {
            if f.key.trim().is_empty() {
                return Err(PluginError::ManifestInvalid(
                    "setting key 不可为空".to_string(),
                ));
            }
            if !seen.insert(f.key.as_str()) {
                return Err(PluginError::ManifestInvalid(format!(
                    "setting key '{}' 重复",
                    f.key
                )));
            }
            validate_setting_field(f)?;
        }

        // permissions：仅收已知能力名（拒未知，靠 minAppVersion 前向兼容）。
        for perm in &self.permissions {
            if !VALID_PERMISSIONS.contains(&perm.as_str()) {
                return Err(PluginError::ManifestInvalid(format!(
                    "未知权限 '{perm}'，合法：{VALID_PERMISSIONS:?}"
                )));
            }
        }
        Ok(())
    }

    /// resolver 的 match.urls（v1 只有 0 或 1 个 resolver）。
    pub fn resolver_urls(&self) -> &[String] {
        self.resolvers
            .first()
            .map(|r| r.match_decl.urls.as_slice())
            .unwrap_or(&[])
    }

    /// manifest 是否声明了指定能力权限。
    pub fn has_permission(&self, perm: &str) -> bool {
        self.permissions.iter().any(|p| p == perm)
    }
}

/// 逐字段校验 widget×type 矩阵与值域约束。
pub fn validate_setting_field(f: &SettingField) -> Result<(), PluginError> {
    let w = f.effective_widget();
    let t = f.ty;
    let err = |msg: String| Err(PluginError::ManifestInvalid(msg));

    // widget × type 合法矩阵。
    let matrix_ok = matches!(
        (w, t),
        (SettingWidget::Text, SettingType::String)
            | (SettingWidget::Password, SettingType::String)
            | (SettingWidget::Textarea, SettingType::String)
            | (SettingWidget::Folder, SettingType::String)
            | (SettingWidget::Select, SettingType::String)
            | (SettingWidget::Toggle, SettingType::Boolean)
            | (SettingWidget::Number, SettingType::Number)
    );
    if !matrix_ok {
        return err(format!(
            "setting '{}': widget '{}' 不允许 type '{}'",
            f.key,
            w.as_str(),
            t.as_str()
        ));
    }

    // select：options 非空，default（若有）∈ options.value。
    if w == SettingWidget::Select {
        if f.options.is_empty() {
            return err(format!("setting '{}': select 的 options 不可为空", f.key));
        }
        if let Some(d) = &f.default
            && !f.options.iter().any(|o| &o.value == d)
        {
            return err(format!(
                "setting '{}': default '{}' 不在 options 中",
                f.key, d
            ));
        }
    }

    // min/max：仅 number 有效、闭区间、非有限数一律非法。
    if t != SettingType::Number && (f.min.is_some() || f.max.is_some()) {
        return err(format!("setting '{}': min/max 仅 number 有效", f.key));
    }
    if let Some(m) = f.min
        && !m.is_finite()
    {
        return err(format!("setting '{}': min 非有限数", f.key));
    }
    if let Some(m) = f.max
        && !m.is_finite()
    {
        return err(format!("setting '{}': max 非有限数", f.key));
    }
    if let (Some(lo), Some(hi)) = (f.min, f.max)
        && lo > hi
    {
        return err(format!("setting '{}': min({lo}) > max({hi})", f.key));
    }

    // pattern：仅 string 有效（编译/匹配在 manager 侧用 JS RegExp）。
    if f.pattern.is_some() && t != SettingType::String {
        return err(format!("setting '{}': pattern 仅 string 有效", f.key));
    }

    // helperScript：仅 string 有效；长度设限防 manifest 膨胀。
    if f.helper_script.is_some() && t != SettingType::String {
        return err(format!("setting '{}': helperScript 仅 string 有效", f.key));
    }
    if let Some(s) = &f.helper_script
        && (s.is_empty() || s.len() > 4 * 1024)
    {
        return err(format!("setting '{}': helperScript 须非空且 ≤4KB", f.key));
    }
    if let Some(l) = &f.helper_label
        && (l.chars().count() > 60 || f.helper_script.is_none())
    {
        return err(format!(
            "setting '{}': helperLabel 须 ≤60 字符且须与 helperScript 同时出现",
            f.key
        ));
    }

    // number default 必须能解析为数字，且落在 min/max 闭区间。
    if t == SettingType::Number
        && let Some(d) = &f.default
    {
        match d.parse::<f64>() {
            Ok(v) if v.is_finite() => {
                if let Some(lo) = f.min
                    && v < lo
                {
                    return err(format!("setting '{}': default {v} < min {lo}", f.key));
                }
                if let Some(hi) = f.max
                    && v > hi
                {
                    return err(format!("setting '{}': default {v} > max {hi}", f.key));
                }
            }
            _ => return err(format!("setting '{}': number default '{d}' 非法", f.key)),
        }
    }

    // boolean default 必须是 "true"/"false"。
    if t == SettingType::Boolean
        && let Some(d) = &f.default
        && d != "true"
        && d != "false"
    {
        return err(format!(
            "setting '{}': boolean default 必须为 'true'/'false'",
            f.key
        ));
    }

    Ok(())
}

/// identity 校验：`^[a-z0-9_-]+@[a-z0-9_-]+$`，禁 '.'。
fn is_valid_identity(s: &str) -> bool {
    let Some((author, name)) = s.split_once('@') else {
        return false;
    };
    if author.is_empty() || name.is_empty() {
        return false;
    }
    // 恰好一个 '@'。
    if name.contains('@') {
        return false;
    }
    let ok = |seg: &str| {
        seg.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    };
    ok(author) && ok(name)
}

/// 相对路径安全：禁 `..`、绝对路径、`/` 或 `\` 开头、盘符（`C:`）、空段。
pub fn is_safe_relative_path(p: &str) -> bool {
    if p.is_empty() {
        return false;
    }
    // 开头分隔符 = 绝对路径。
    if p.starts_with('/') || p.starts_with('\\') {
        return false;
    }
    // Windows 盘符（C:\ 或 C:/ 或裸 C:）。
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return false;
    }
    // 逐段检查：禁 `..`、空段、当前目录 `.`。
    for seg in p.split(['/', '\\']) {
        if seg == ".." || seg.is_empty() || seg == "." {
            return false;
        }
    }
    // 禁控制字符。
    if p.chars().any(|c| c.is_control()) {
        return false;
    }
    true
}

/// URL glob 匹配：`*` 是唯一通配符，按 `*` 分段顺序子串匹配（首段前缀锚定、尾段后缀
/// 锚定）。scheme/host 大小写不敏感（整体小写化后比较）。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::plugin::manifest::url_glob_match;
///
/// assert!(url_glob_match("*://www.youtube.com/watch*", "https://www.youtube.com/watch?v=abc"));
/// assert!(url_glob_match("*", "anything"));
/// assert!(url_glob_match("https://x.com/a", "https://x.com/a"));
/// assert!(!url_glob_match("*://x.com/*", "https://y.com/a"));
/// ```
pub fn url_glob_match(pattern: &str, url: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let url = url.to_ascii_lowercase();
    glob_segments(&pattern, &url)
}

fn glob_segments(pattern: &str, text: &str) -> bool {
    // 无通配符 = 精确匹配。
    if !pattern.contains('*') {
        return pattern == text;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let n = parts.len();
    let mut pos = 0usize;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // 首段前缀锚定。
            if !text[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if i == n - 1 {
            // 尾段后缀锚定。
            if !text[pos..].ends_with(part) {
                return false;
            }
            // 后缀存在即通过（后缀可与已消费区重叠？不会——ends_with 在剩余串上）。
        } else {
            // 中段：从 pos 起找首个出现。
            match text[pos..].find(part) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::PluginManifest;

    fn parse_ok(json: &str) -> PluginManifest {
        PluginManifest::parse(json.as_bytes()).expect("parse")
    }

    #[test]
    fn valid_manifest_passes() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "resolvers":[{"match":{"urls":["*://x.com/*"]},"entry":"r.js"}],
                "settings":[{"key":"q","title":"Q","type":"string","widget":"select",
                             "options":[{"value":"a","label":"A"}],"default":"a"}]}"#,
        );
        assert!(m.validate().is_ok());
    }

    #[test]
    fn rejects_bad_identity() {
        let m = parse_ok(r#"{"identity":"bad.id@x","name":"N","version":"1.0.0"}"#);
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_two_resolvers() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0","resolvers":[
                {"match":{"urls":["*"]},"entry":"a.js"},
                {"match":{"urls":["*"]},"entry":"b.js"}]}"#,
        );
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_select_without_options() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "settings":[{"key":"q","title":"Q","type":"string","widget":"select"}]}"#,
        );
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_widget_type_mismatch() {
        // toggle 只允许 boolean。
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "settings":[{"key":"q","title":"Q","type":"string","widget":"toggle"}]}"#,
        );
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_min_gt_max() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "settings":[{"key":"n","title":"N","type":"number","widget":"number","min":10,"max":1}]}"#,
        );
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_setting_key() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "settings":[{"key":"q","title":"Q1","type":"string"},
                            {"key":"q","title":"Q2","type":"string"}]}"#,
        );
        assert!(m.validate().is_err());
    }
    #[test]
    fn helper_script_only_valid_on_string_fields() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "settings":[{"key":"q","title":"Q","type":"boolean","widget":"toggle","helperScript":"copy(1)"}]}"#,
        );
        assert!(m.validate().is_err());
        let ok = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "settings":[{"key":"q","title":"Q","type":"string","widget":"textarea","helperScript":"copy(document.cookie)","helperLabel":"复制"}]}"#,
        );
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn helper_label_requires_helper_script() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "settings":[{"key":"q","title":"Q","type":"string","helperLabel":"复制"}]}"#,
        );
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_entry_path_traversal() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "resolvers":[{"match":{"urls":["*"]},"entry":"../evil.js"}]}"#,
        );
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_unknown_hook_event() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "hooks":{"entry":"h.js","events":["onBogus"]}}"#,
        );
        assert!(m.validate().is_err());
    }
    use super::{is_safe_relative_path, is_valid_identity, url_glob_match};

    #[test]
    fn identity_rules() {
        assert!(is_valid_identity("zerx@youtube"));
        assert!(is_valid_identity("a_b-c@x-y_z"));
        assert!(!is_valid_identity("zerx.dev@youtube")); // 禁 '.'
        assert!(!is_valid_identity("Zerx@youtube")); // 禁大写
        assert!(!is_valid_identity("noat"));
        assert!(!is_valid_identity("@youtube"));
        assert!(!is_valid_identity("zerx@"));
        assert!(!is_valid_identity("a@b@c"));
    }

    #[test]
    fn path_safety() {
        assert!(is_safe_relative_path("resolve.js"));
        assert!(is_safe_relative_path("dir/resolve.js"));
        assert!(!is_safe_relative_path("../evil.js"));
        assert!(!is_safe_relative_path("/etc/passwd"));
        assert!(!is_safe_relative_path("\\windows\\x"));
        assert!(!is_safe_relative_path("C:/x"));
        assert!(!is_safe_relative_path("a/../b"));
        assert!(!is_safe_relative_path(""));
    }

    #[test]
    fn glob_prefix_anchor() {
        assert!(url_glob_match(
            "*://www.youtube.com/watch*",
            "https://www.youtube.com/watch?v=abc"
        ));
        assert!(!url_glob_match(
            "*://www.youtube.com/watch*",
            "https://www.youtube.com/embed/abc"
        ));
    }

    #[test]
    fn glob_subdomain() {
        assert!(url_glob_match("*://*.x.com/*", "https://a.x.com/p"));
        assert!(url_glob_match("*://*.x.com/*", "http://deep.a.x.com/p"));
        assert!(!url_glob_match("*://*.x.com/*", "https://x.com/p"));
    }

    #[test]
    fn glob_exact() {
        assert!(url_glob_match("https://x.com/a", "https://x.com/a"));
        assert!(!url_glob_match("https://x.com/a", "https://x.com/b"));
    }

    #[test]
    fn glob_star_all() {
        assert!(url_glob_match("*", "anything at all"));
        assert!(url_glob_match("*", ""));
    }

    #[test]
    fn glob_case_insensitive() {
        assert!(url_glob_match(
            "*://WWW.YouTube.com/*",
            "https://www.youtube.com/x"
        ));
    }
}
