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
//!
//! 官方插件索引（`zerx-lab/fluxdown-plugin-index`）用已发布 SDK 的 `dist/manifest.mjs`
//! （`packages/plugin-sdk/src/manifest.ts::validatePackage`）做准入，市场收录的包必须能在客户端
//! 安装：改字段、权限、事件、设置矩阵或安装限制时同步 SDK 并发版，索引仓自动同步。

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
    /// 单次 resolve 的 QuickJS 堆上限（MiB）。缺省取宿主默认 64 MiB；可上调，超过
    /// [`super::manager::RESOLVE_MEMORY_CEILING`] 按硬顶裁剪；不可为 0。供要在脚本内
    /// 处理大体量数据的 resolver 声明（如把 YouTube 播放器 JS 解析成 AST）。
    #[serde(default)]
    pub memory_limit_mb: Option<u64>,
    /// 声明该 resolver 支持多文件清单前置预解析（`ResolveResult::manifest`）。
    /// `false`（默认）的单文件插件不会被前置预解析无谓触发（避免解析昂贵的
    /// 单文件插件白跑一次）；未声明却在 start 阶段返回清单的插件仍由引擎
    /// `on_resolve_ready` 的自动裂变兜底（D6），不依赖本字段。
    #[serde(default)]
    pub multi: bool,
}

/// 单个订阅 provider 声明。v1 每插件至多一个 provider。
///
/// 插件入口函数名固定为 `subscribe`；`entry` 指向包含该函数的脚本文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionDecl {
    /// 公共订阅记录中的 `providerId`，由插件稳定维护。
    pub provider_id: String,
    /// provider 脚本入口文件。
    pub entry: String,
    /// 单次调用超时（毫秒），只能下调宿主预算。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
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

/// 拦截器失败策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterceptFailure {
    /// 记录后继续下载。
    #[default]
    Continue,
    /// 使任务失败。
    Fail,
}

/// 拦截平面声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterceptorsDecl {
    pub entry: String,
    pub stages: Vec<String>,
    #[serde(rename = "match")]
    pub match_decl: MatchDecl,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub on_failure: InterceptFailure,
}

/// 平台登录入口。脚本实现 `globalThis.authenticate(ctx)`，由 daemon 以
/// `begin`/`poll`/`cancel`/`logout`/`status` action 驱动，登录成功后脚本调用
/// `flux.auth.save`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthDecl {
    pub entry: String,
    /// 单次 auth 调用超时（毫秒）。独立于 resolver 的 timeoutMs（M-2：登录轮询
    /// 与 resolve 分属不同的信号量/预算平面，不共享 resolver 的低超时配置）；
    /// 未声明时宿主默认 30s，30s 硬顶。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `main`：可移植视图 / 命令 / 后台逻辑的常驻脚本（classic script，由 daemon 内的
/// QuickJS 宿主执行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MainDecl {
    pub entry: String,
    /// 投递给宿主的事件（⊆ [`VALID_EVENTS`]）。
    #[serde(default)]
    pub events: Vec<String>,
    /// 是否随 daemon 启动常驻（需 `background` 权限）。
    #[serde(default)]
    pub background: bool,
}

/// 视图放置位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ViewLocation {
    Activity,
    Settings,
    TaskDetail,
    Window,
    /// 登录弹窗承载的视图（至多 1 个，要求声明 `auth`）。
    Auth,
}

impl ViewLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewLocation::Activity => "activity",
            ViewLocation::Settings => "settings",
            ViewLocation::TaskDetail => "taskDetail",
            ViewLocation::Auth => "auth",
            ViewLocation::Window => "window",
        }
    }
}

/// 视图的桌面增强实现。`entry` 是由 GPUI Shell 在桌面进程内执行的 ESM 脚本
/// （`import` 自 "gpui-kit"/"gpui-base"/"gpui-component"），引擎只校验路径与存在性，
/// 从不编译或执行它；daemon 亦不执行。需要 `desktop` 权限。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopEntryDecl {
    pub entry: String,
    /// 脚本允许访问的网络主机（小写 DNS 主机名或 IP 字面量）。
    #[serde(default)]
    pub network_hosts: Vec<String>,
    /// 初始窗口宽度（仅 `location == window`）。
    #[serde(default)]
    pub width: Option<f32>,
    /// 初始窗口高度（仅 `location == window`）。
    #[serde(default)]
    pub height: Option<f32>,
}

/// 视图贡献。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewDecl {
    pub id: String,
    pub title: String,
    pub location: ViewLocation,
    #[serde(default)]
    pub desktop: Option<DesktopEntryDecl>,
    /// 声明了 `desktop` 的 view 是否同时由 `main` 提供可移植实现（Web / 移动端可用）。
    /// 仅允许与 `desktop` 同时出现；未声明 `desktop` 的 view 恒为可移植。
    #[serde(default)]
    pub portable: bool,
    /// 视图图标：包内相对路径的 `.svg` 文件（单色，按宿主主题着色）。加载期读入并随
    /// 插件 DTO 内联下发；未声明时宿主用默认图标。
    #[serde(default)]
    pub icon: Option<String>,
}

impl ViewDecl {
    /// 是否有可移植实现（daemon 经 `main` 渲染）：无 `desktop`，或显式 `portable: true`。
    pub fn is_portable(&self) -> bool {
        self.desktop.is_none() || self.portable
    }
}

/// 命令贡献。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandDecl {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub palette: bool,
    #[serde(default)]
    pub task_menu: bool,
}

/// 页面脚本运行时机（对应浏览器 `userScripts` 的 `runAt`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageScriptRunAt {
    DocumentStart,
    DocumentEnd,
    #[default]
    DocumentIdle,
}

impl PageScriptRunAt {
    pub fn as_str(self) -> &'static str {
        match self {
            PageScriptRunAt::DocumentStart => "document_start",
            PageScriptRunAt::DocumentEnd => "document_end",
            PageScriptRunAt::DocumentIdle => "document_idle",
        }
    }
}

/// 页面脚本运行世界：`isolated` 有 `fluxdown` 桥，`main` 为页面 world（无桥）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageScriptWorld {
    #[default]
    Isolated,
    Main,
}

impl PageScriptWorld {
    pub fn as_str(self) -> &'static str {
        match self {
            PageScriptWorld::Isolated => "isolated",
            PageScriptWorld::Main => "main",
        }
    }
}

/// 页面脚本贡献：由浏览器扩展经 `userScripts` API 注册到匹配网页。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageScriptDecl {
    pub id: String,
    pub title: String,
    pub matches: Vec<String>,
    #[serde(default)]
    pub exclude_matches: Vec<String>,
    pub js: Vec<String>,
    #[serde(default)]
    pub run_at: PageScriptRunAt,
    #[serde(default)]
    pub world: PageScriptWorld,
    #[serde(default)]
    pub all_frames: bool,
}

/// Signed archive classification rules; requires the existing `archive` permission.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveRulesDecl {
    pub entry: String,
    pub public_key: String,
    #[serde(default)]
    pub update_url: Option<String>,
}

/// Contributions. Empty arrays are equivalent to absent declarations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContributesDecl {
    #[serde(default)]
    pub views: Vec<ViewDecl>,
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    /// 浏览器扩展嗅探规则包（相对路径，`.json`），需 `browserRules` 权限。
    #[serde(default)]
    pub sniff_rules: Option<String>,
    /// 浏览器页面脚本（0..=16 条），需 `pageScripts` 权限。
    #[serde(default)]
    pub page_scripts: Vec<PageScriptDecl>,
    /// 归档脚本（相对路径，`.js`），需 `archive` 权限。
    #[serde(default)]
    pub archive: Option<String>,
    /// Independently updateable, publisher-signed archive classification rules.
    #[serde(default)]
    pub archive_rules: Option<ArchiveRulesDecl>,
}

/// 插件风险等级（由权限推导）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginRisk {
    Standard,
    Elevated,
    Full,
}

impl PluginRisk {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginRisk::Standard => "standard",
            PluginRisk::Elevated => "elevated",
            PluginRisk::Full => "full",
        }
    }
}

/// 窗口尺寸下限/上限（逻辑像素）。
pub const WINDOW_SIZE_MIN: f32 = 240.0;
pub const WINDOW_SIZE_MAX: f32 = 4096.0;
/// `desktop.networkHosts` 条目数上限。
pub const NETWORK_HOSTS_MAX: usize = 64;
/// `contributes.views` / `contributes.commands` 条目数上限。
pub const VIEWS_MAX: usize = 16;
pub const COMMANDS_MAX: usize = 32;
/// view / command 标题的最大字符数。
const TITLE_MAX_CHARS: usize = 40;
/// 视图图标（`contributes.views[].icon`）文件大小上限。
pub const VIEW_ICON_MAX_BYTES: usize = 16 * 1024;
/// sniff 规则包文件大小与规则数上限。
pub const SNIFF_PACK_MAX_BYTES: usize = 256 * 1024;
pub const SNIFF_PACK_MAX_RULES: usize = 500;
/// `contributes.pageScripts` 条目数、每条 `matches` / `excludeMatches` / `js` 条数上限。
pub const PAGE_SCRIPTS_MAX: usize = 16;
pub const PAGE_SCRIPT_MATCHES_MAX: usize = 32;
pub const PAGE_SCRIPT_JS_MAX: usize = 8;
/// 页面脚本单个 js 文件大小上限。
pub const PAGE_SCRIPT_FILE_MAX_BYTES: usize = 256 * 1024;
/// 单条页面脚本（各 js 文件以 `"\n;\n"` 拼接后）大小上限。
pub const PAGE_SCRIPT_MAX_BYTES: usize = 384 * 1024;

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
    /// 订阅 provider 声明；v1 每插件至多一个。
    #[serde(default)]
    pub subscriptions: Vec<SubscriptionDecl>,
    #[serde(default)]
    pub hooks: Option<HooksDecl>,
    /// 可选的拦截平面声明（v1 至多一个）。
    #[serde(default)]
    pub interceptors: Option<InterceptorsDecl>,
    /// 可选的平台特有登录入口。
    #[serde(default)]
    pub auth: Option<AuthDecl>,
    /// 可选的常驻脚本声明（可移植视图 / 命令 / 后台逻辑）。
    #[serde(default)]
    pub main: Option<MainDecl>,
    /// 可选的贡献点（视图 / 命令 / 嗅探规则）。
    #[serde(default)]
    pub contributes: Option<ContributesDecl>,
    #[serde(default)]
    pub settings: Vec<SettingField>,
    /// 声明式能力权限（如 `"auth"`、`"ffmpeg"`）。空 = 无额外能力。授予的能力经宿主
    /// 门控注入对应 `flux.*` 门面（见 [`super::runtime::HostContext`]）。
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// 合法事件名（manifest `hooks.events`）。
pub const VALID_EVENTS: [&str; 5] = ["onStart", "onError", "onDone", "onMetaProbed", "onCancel"];

/// ffmpeg 能力权限名（manifest `permissions`）。
pub const PERMISSION_FFMPEG: &str = "ffmpeg";
/// yt-dlp 能力权限名（manifest `permissions`）。
pub const PERMISSION_YTDLP: &str = "ytdlp";
/// 通用认证能力权限名（manifest `permissions`）。
pub const PERMISSION_AUTH: &str = "auth";
/// 任务操作能力权限名（`flux.tasks.*`）。
pub const PERMISSION_TASKS: &str = "tasks";
/// 浏览器扩展嗅探规则贡献权限名。
pub const PERMISSION_BROWSER_RULES: &str = "browserRules";
/// 桌面原生脚本权限名（完全信任）。
pub const PERMISSION_DESKTOP: &str = "desktop";
/// 后台常驻权限名。
pub const PERMISSION_BACKGROUND: &str = "background";
/// 下载拦截平面权限名。
pub const PERMISSION_INTERCEPT: &str = "intercept";
/// 浏览器页面脚本权限名（风险 Full：脚本在用户任意匹配网页内运行）。
pub const PERMISSION_PAGE_SCRIPTS: &str = "pageScripts";
/// 归档脚本权限名。
pub const PERMISSION_ARCHIVE: &str = "archive";
/// 合法能力权限（manifest `permissions`）。
pub const VALID_PERMISSIONS: [&str; 10] = [
    PERMISSION_FFMPEG,
    PERMISSION_YTDLP,
    PERMISSION_AUTH,
    PERMISSION_TASKS,
    PERMISSION_BROWSER_RULES,
    PERMISSION_DESKTOP,
    PERMISSION_BACKGROUND,
    PERMISSION_INTERCEPT,
    PERMISSION_PAGE_SCRIPTS,
    PERMISSION_ARCHIVE,
];

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
            if let Some(m) = r.memory_limit_mb
                && m == 0
            {
                return Err(PluginError::ManifestInvalid(
                    "resolver memoryLimitMb 不可为 0".to_string(),
                ));
            }
        }

        // subscriptions：v1 每插件至多一个 provider。
        if self.subscriptions.len() > 1 {
            return Err(PluginError::ManifestInvalid(
                "v1 每插件至多一个 subscription provider".to_string(),
            ));
        }
        for s in &self.subscriptions {
            if !is_valid_provider_id(&s.provider_id) {
                return Err(PluginError::ManifestInvalid(format!(
                    "subscription providerId '{}' 非法：须为小写字母、数字、'_' 或 '-'",
                    s.provider_id
                )));
            }
            if s.provider_id == crate::rss::RSS_PROVIDER_ID {
                return Err(PluginError::ManifestInvalid(format!(
                    "subscription providerId '{}' 为内置 provider 保留字",
                    s.provider_id
                )));
            }
            if !is_safe_relative_path(&s.entry) {
                return Err(PluginError::ManifestInvalid(format!(
                    "subscription entry 路径 '{}' 非法",
                    s.entry
                )));
            }
            if let Some(t) = s.timeout_ms
                && t == 0
            {
                return Err(PluginError::ManifestInvalid(
                    "subscription timeoutMs 不可为 0".to_string(),
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

        // auth：入口同样是插件包内的可执行脚本，且必须显式授予认证能力。
        if let Some(a) = &self.auth {
            if !is_safe_relative_path(&a.entry) {
                return Err(PluginError::ManifestInvalid(format!(
                    "auth entry 路径 '{}' 非法",
                    a.entry
                )));
            }
            if !self.has_permission(PERMISSION_AUTH) {
                return Err(PluginError::ManifestInvalid(
                    "声明 auth.entry 时 permissions 必须包含 auth".to_string(),
                ));
            }
            if let Some(t) = a.timeout_ms
                && t == 0
            {
                return Err(PluginError::ManifestInvalid(
                    "auth timeoutMs 不可为 0".to_string(),
                ));
            }
        }

        // interceptors：拦截平面入口。
        if let Some(i) = &self.interceptors {
            if !is_safe_relative_path(&i.entry) {
                return Err(PluginError::ManifestInvalid(format!(
                    "interceptors entry 路径 '{}' 非法",
                    i.entry
                )));
            }
            if i.stages.is_empty() {
                return Err(PluginError::ManifestInvalid(
                    "interceptors.stages 不可为空".to_string(),
                ));
            }
            let mut seen_stages: HashSet<&str> = HashSet::new();
            for s in &i.stages {
                if !crate::intercept::STAGE_NAMES.contains(&s.as_str()) {
                    return Err(PluginError::ManifestInvalid(format!(
                        "未知拦截阶段 '{s}'，合法：{:?}",
                        crate::intercept::STAGE_NAMES
                    )));
                }
                if !seen_stages.insert(s.as_str()) {
                    return Err(PluginError::ManifestInvalid(format!(
                        "interceptors.stages 重复：'{s}'"
                    )));
                }
            }
            if i.match_decl.urls.is_empty() {
                return Err(PluginError::ManifestInvalid(
                    "interceptors match.urls 不可为空".to_string(),
                ));
            }
            if i.timeout_ms == Some(0) {
                return Err(PluginError::ManifestInvalid(
                    "interceptors timeoutMs 不可为 0".to_string(),
                ));
            }
            if !self.has_permission(PERMISSION_INTERCEPT) {
                return Err(PluginError::ManifestInvalid(
                    "声明 interceptors 时 permissions 必须包含 intercept".to_string(),
                ));
            }
        }

        validate_main(self)?;
        validate_contributes(self)?;

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

    /// 风险等级：`desktop` → Full；其余任一提权能力 → Elevated；否则 Standard。
    pub fn risk(&self) -> PluginRisk {
        if self.has_permission(PERMISSION_DESKTOP) || self.has_permission(PERMISSION_PAGE_SCRIPTS) {
            PluginRisk::Full
        } else if self.permissions.iter().any(|p| {
            matches!(
                p.as_str(),
                PERMISSION_FFMPEG
                    | PERMISSION_YTDLP
                    | PERMISSION_AUTH
                    | PERMISSION_TASKS
                    | PERMISSION_BROWSER_RULES
                    | PERMISSION_BACKGROUND
                    | PERMISSION_INTERCEPT
                    | PERMISSION_ARCHIVE
            )
        }) {
            PluginRisk::Elevated
        } else {
            PluginRisk::Standard
        }
    }

    /// 所有 view `desktop.networkHosts` 的排序去重并集。
    pub fn network_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .contributes
            .iter()
            .flat_map(|c| c.views.iter())
            .filter_map(|v| v.desktop.as_ref())
            .flat_map(|d| d.network_hosts.iter().cloned())
            .chain(
                self.contributes
                    .iter()
                    .filter_map(|c| c.archive_rules.as_ref())
                    .filter_map(|rules| rules.update_url.as_ref())
                    .filter_map(|url| url::Url::parse(url).ok())
                    .filter_map(|url| url.host_str().map(str::to_owned)),
            )
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }

    /// 包内必须存在的文件：`main.entry`、各 `desktop.entry`、各视图 `icon`、`sniffRules`、
    /// 页面脚本 `js`。
    pub fn package_files(&self) -> Vec<String> {
        let mut files = Vec::new();
        if let Some(main) = &self.main {
            files.push(main.entry.clone());
        }
        if let Some(c) = &self.contributes {
            files.extend(
                c.views
                    .iter()
                    .filter_map(|v| v.desktop.as_ref())
                    .map(|d| d.entry.clone()),
            );
            files.extend(c.views.iter().filter_map(|v| v.icon.clone()));
            files.extend(c.sniff_rules.iter().cloned());
            files.extend(c.page_scripts.iter().flat_map(|s| s.js.iter().cloned()));
            files.extend(c.archive.iter().cloned());
            files.extend(c.archive_rules.iter().map(|rules| rules.entry.clone()));
        }
        files
    }

    /// 逐个检查 [`Self::package_files`] 是否存在（`exists` 由调用方提供）。
    pub fn check_package_files(&self, exists: impl Fn(&str) -> bool) -> Result<(), PluginError> {
        for file in self.package_files() {
            if !exists(&file) {
                return manifest_err(format!("包内缺少文件 '{file}'"));
            }
        }
        Ok(())
    }

    /// Validate the signed archive rules seed package content.
    pub fn validate_archive_rules_content(
        &self,
        read: impl Fn(&str) -> Option<Vec<u8>>,
    ) -> Result<(), PluginError> {
        let Some(rules) = self
            .contributes
            .as_ref()
            .and_then(|c| c.archive_rules.as_ref())
        else {
            return Ok(());
        };
        let bytes = read(&rules.entry).ok_or_else(|| {
            PluginError::ManifestInvalid(format!("包内缺少文件 '{}'", rules.entry))
        })?;
        super::archive_rules::VerifiedArchiveRules::verify(&bytes, &rules.public_key).map(|_| ())
    }
}

fn manifest_err<T>(msg: String) -> Result<T, PluginError> {
    Err(PluginError::ManifestInvalid(msg))
}

/// 视图 / 命令 id：`^[a-z][a-z0-9-]{0,31}$`。
fn is_valid_contribution_id(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && s.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn has_extension(path: &str, exts: &[&str]) -> bool {
    let lower = path.to_ascii_lowercase();
    exts.iter().any(|e| lower.ends_with(e))
}

/// 校验 `main` 声明。
fn validate_main(m: &PluginManifest) -> Result<(), PluginError> {
    let Some(main) = &m.main else {
        return Ok(());
    };
    if !is_safe_relative_path(&main.entry) {
        return manifest_err(format!("main entry 路径 '{}' 非法", main.entry));
    }
    if !has_extension(&main.entry, &[".js"]) {
        return manifest_err(format!("main entry '{}' 扩展名须为 .js", main.entry));
    }
    for ev in &main.events {
        if !VALID_EVENTS.contains(&ev.as_str()) {
            return manifest_err(format!(
                "main.events 未知事件 '{ev}'，合法：{VALID_EVENTS:?}"
            ));
        }
    }
    if main.background && !m.has_permission(PERMISSION_BACKGROUND) {
        return manifest_err("main.background 为 true 时 permissions 必须包含 background".into());
    }
    Ok(())
}

/// 校验 `contributes` 声明。
fn validate_contributes(m: &PluginManifest) -> Result<(), PluginError> {
    let Some(c) = &m.contributes else {
        return Ok(());
    };
    if c.views.len() > VIEWS_MAX {
        return manifest_err(format!("contributes.views 至多 {VIEWS_MAX} 项"));
    }
    let mut auth_views = 0usize;
    let mut seen: HashSet<&str> = HashSet::new();
    let mut settings_views = 0usize;
    for v in &c.views {
        if !is_valid_contribution_id(&v.id) {
            return manifest_err(format!(
                "view id '{}' 非法：须匹配 ^[a-z][a-z0-9-]{{0,31}}$",
                v.id
            ));
        }
        if !seen.insert(v.id.as_str()) {
            return manifest_err(format!("view id '{}' 重复", v.id));
        }
        let title_len = v.title.chars().count();
        if !(1..=TITLE_MAX_CHARS).contains(&title_len) {
            return manifest_err(format!(
                "view '{}' title 须为 1–{TITLE_MAX_CHARS} 个字符",
                v.id
            ));
        }
        if v.location == ViewLocation::Settings {
            settings_views += 1;
            if settings_views > 1 {
                return manifest_err("location 为 settings 的 view 至多 1 个".into());
            }
        }
        if v.location == ViewLocation::Auth {
            auth_views += 1;
            if auth_views > 1 {
                return manifest_err("location 为 auth 的 view 至多 1 个".into());
            }
            if m.auth.is_none() {
                return manifest_err("location 为 auth 的 view 要求声明 auth".into());
            }
        }
        match &v.desktop {
            None => {
                if v.portable {
                    return manifest_err(format!(
                        "view '{}' 的 portable 仅允许与 desktop 同时声明",
                        v.id
                    ));
                }
            }
            Some(d) => validate_desktop_entry(m, v, d)?,
        }
        if v.is_portable() && m.main.is_none() {
            return manifest_err(format!("view '{}' 有可移植实现，必须声明 main", v.id));
        }
        if let Some(icon) = &v.icon {
            if !is_safe_relative_path(icon) {
                return manifest_err(format!("view '{}' icon 路径 '{icon}' 非法", v.id));
            }
            if !has_extension(icon, &[".svg"]) {
                return manifest_err(format!("view '{}' icon '{icon}' 扩展名须为 .svg", v.id));
            }
        }
    }
    if c.commands.len() > COMMANDS_MAX {
        return manifest_err(format!("contributes.commands 至多 {COMMANDS_MAX} 项"));
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for cmd in &c.commands {
        if !is_valid_contribution_id(&cmd.id) {
            return manifest_err(format!(
                "command id '{}' 非法：须匹配 ^[a-z][a-z0-9-]{{0,31}}$",
                cmd.id
            ));
        }
        if !seen.insert(cmd.id.as_str()) {
            return manifest_err(format!("command id '{}' 重复", cmd.id));
        }
        let title_len = cmd.title.chars().count();
        if !(1..=TITLE_MAX_CHARS).contains(&title_len) {
            return manifest_err(format!(
                "command '{}' title 须为 1–{TITLE_MAX_CHARS} 个字符",
                cmd.id
            ));
        }
    }
    if !c.commands.is_empty() && m.main.is_none() {
        return manifest_err("声明 commands 时必须声明 main".into());
    }
    if let Some(rules) = &c.sniff_rules {
        if !is_safe_relative_path(rules) {
            return manifest_err(format!("sniffRules 路径 '{rules}' 非法"));
        }
        if !has_extension(rules, &[".json"]) {
            return manifest_err(format!("sniffRules '{rules}' 扩展名须为 .json"));
        }
        if !m.has_permission(PERMISSION_BROWSER_RULES) {
            return manifest_err("声明 sniffRules 时 permissions 必须包含 browserRules".into());
        }
    }
    if let Some(entry) = &c.archive {
        if !is_safe_relative_path(entry) {
            return manifest_err(format!("archive 路径 '{entry}' 非法"));
        }
        if !has_extension(entry, &[".js"]) {
            return manifest_err(format!("archive '{entry}' 扩展名须为 .js"));
        }
        if !m.has_permission(PERMISSION_ARCHIVE) {
            return manifest_err("声明 archive 时 permissions 必须包含 archive".into());
        }
    }
    if let Some(rules) = &c.archive_rules {
        if !is_safe_relative_path(&rules.entry) {
            return manifest_err(format!("archiveRules entry 路径 '{}' 非法", rules.entry));
        }
        if !has_extension(&rules.entry, &[".json"]) {
            return manifest_err("archiveRules entry 扩展名须为 .json".into());
        }
        if rules.public_key.len() != 64 || !rules.public_key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return manifest_err("archiveRules publicKey 须为 64 位十六进制 Ed25519 公钥".into());
        }
        if !m.has_permission(PERMISSION_ARCHIVE) {
            return manifest_err("声明 archiveRules 时 permissions 必须包含 archive".into());
        }
        if let Some(update_url) = &rules.update_url
            && !is_valid_archive_update_url(update_url)
        {
            return manifest_err(
                "archiveRules updateUrl 须为无凭据、无片段的公开 HTTPS URL".into(),
            );
        }
    }
    validate_page_scripts(m, c)?;
    Ok(())
}

/// 校验 view 的 `desktop` 条目：它是由 GPUI Shell 在桌面进程内执行的 ESM 脚本，
/// 引擎只校验路径与存在性，从不编译或执行它。
fn validate_desktop_entry(
    m: &PluginManifest,
    v: &ViewDecl,
    d: &DesktopEntryDecl,
) -> Result<(), PluginError> {
    if !is_safe_relative_path(&d.entry) {
        return manifest_err(format!(
            "view '{}' desktop entry 路径 '{}' 非法",
            v.id, d.entry
        ));
    }
    if !has_extension(&d.entry, &[".js", ".mjs"]) {
        return manifest_err(format!(
            "view '{}' desktop entry '{}' 扩展名须为 .js 或 .mjs",
            v.id, d.entry
        ));
    }
    if !m.has_permission(PERMISSION_DESKTOP) {
        return manifest_err(format!(
            "view '{}' 声明 desktop 时 permissions 必须包含 desktop",
            v.id
        ));
    }
    for (name, size) in [("width", d.width), ("height", d.height)] {
        let Some(size) = size else { continue };
        if v.location != ViewLocation::Window {
            return manifest_err(format!(
                "view '{}' 的 {name} 仅允许用于 location 为 window 的 view",
                v.id
            ));
        }
        if !size.is_finite() || !(WINDOW_SIZE_MIN..=WINDOW_SIZE_MAX).contains(&size) {
            return manifest_err(format!(
                "view '{}' {name} 须为 [{WINDOW_SIZE_MIN}, {WINDOW_SIZE_MAX}] 内的有限数",
                v.id
            ));
        }
    }
    if d.network_hosts.len() > NETWORK_HOSTS_MAX {
        return manifest_err(format!(
            "view '{}' networkHosts 至多 {NETWORK_HOSTS_MAX} 项",
            v.id
        ));
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for host in &d.network_hosts {
        if !is_valid_network_host(host) {
            return manifest_err(format!("view '{}' networkHosts '{host}' 非法", v.id));
        }
        if !seen.insert(host.as_str()) {
            return manifest_err(format!("view '{}' networkHosts '{host}' 重复", v.id));
        }
    }
    Ok(())
}

/// match pattern：`<scheme>://<host><path>`，scheme ∈ http|https|*；host 为 `domain` 或
/// `*.domain`（`domain` 仅 `[a-z0-9.-]`、含 `.`、首尾非 `.`/`-`）；无端口；path 以 `/` 开头。
fn is_valid_match_pattern(pattern: &str) -> bool {
    let Some((scheme, rest)) = pattern.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "http" | "https" | "*") {
        return false;
    }
    let Some(slash) = rest.find('/') else {
        return false;
    };
    let (host, path) = rest.split_at(slash);
    if path.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let domain = host.strip_prefix("*.").unwrap_or(host);
    !domain.is_empty()
        && domain.contains('.')
        && domain
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with(['.', '-'])
        && !domain.contains("..")
}

/// 页面脚本 id：`^[a-z0-9][a-z0-9_-]{0,31}$`。
fn is_valid_page_script_id(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && s.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// 校验 `contributes.pageScripts`。
fn validate_page_scripts(m: &PluginManifest, c: &ContributesDecl) -> Result<(), PluginError> {
    if c.page_scripts.is_empty() {
        return Ok(());
    }
    if c.page_scripts.len() > PAGE_SCRIPTS_MAX {
        return manifest_err(format!(
            "contributes.pageScripts 至多 {PAGE_SCRIPTS_MAX} 项"
        ));
    }
    if !m.has_permission(PERMISSION_PAGE_SCRIPTS) {
        return manifest_err("声明 pageScripts 时 permissions 必须包含 pageScripts".into());
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for s in &c.page_scripts {
        if !is_valid_page_script_id(&s.id) {
            return manifest_err(format!(
                "pageScript id '{}' 非法：须匹配 ^[a-z0-9][a-z0-9_-]{{0,31}}$",
                s.id
            ));
        }
        if !seen.insert(s.id.as_str()) {
            return manifest_err(format!("pageScript id '{}' 重复", s.id));
        }
        let title_len = s.title.chars().count();
        if !(1..=TITLE_MAX_CHARS).contains(&title_len) {
            return manifest_err(format!(
                "pageScript '{}' title 须为 1–{TITLE_MAX_CHARS} 个字符",
                s.id
            ));
        }
        if !(1..=PAGE_SCRIPT_MATCHES_MAX).contains(&s.matches.len()) {
            return manifest_err(format!(
                "pageScript '{}' matches 须为 1–{PAGE_SCRIPT_MATCHES_MAX} 项",
                s.id
            ));
        }
        if s.exclude_matches.len() > PAGE_SCRIPT_MATCHES_MAX {
            return manifest_err(format!(
                "pageScript '{}' excludeMatches 至多 {PAGE_SCRIPT_MATCHES_MAX} 项",
                s.id
            ));
        }
        for (field, list) in [
            ("matches", &s.matches),
            ("excludeMatches", &s.exclude_matches),
        ] {
            for pattern in list {
                if !is_valid_match_pattern(pattern) {
                    return manifest_err(format!(
                        "pageScript '{}' {field} '{pattern}' 非法 match pattern",
                        s.id
                    ));
                }
            }
        }
        if !(1..=PAGE_SCRIPT_JS_MAX).contains(&s.js.len()) {
            return manifest_err(format!(
                "pageScript '{}' js 须为 1–{PAGE_SCRIPT_JS_MAX} 项",
                s.id
            ));
        }
        let mut files: HashSet<&str> = HashSet::new();
        for file in &s.js {
            if !is_safe_relative_path(file) {
                return manifest_err(format!("pageScript '{}' js 路径 '{file}' 非法", s.id));
            }
            if !has_extension(file, &[".js"]) {
                return manifest_err(format!("pageScript '{}' js '{file}' 扩展名须为 .js", s.id));
            }
            if !files.insert(file.as_str()) {
                return manifest_err(format!("pageScript '{}' js '{file}' 重复", s.id));
            }
        }
    }
    Ok(())
}

/// 页面脚本文件内容校验：每个文件 ≤256 KiB 且为合法 UTF-8；拼接后 ≤384 KiB。
/// `read` 按包内相对路径返回文件字节（`None` 视为缺失）。返回拼接后的源码。
pub fn concat_page_script(
    script: &PageScriptDecl,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<String, PluginError> {
    let mut code = String::new();
    for (i, file) in script.js.iter().enumerate() {
        let Some(bytes) = read(file) else {
            return manifest_err(format!("pageScript '{}' 读取 '{file}' 失败", script.id));
        };
        if bytes.len() > PAGE_SCRIPT_FILE_MAX_BYTES {
            return manifest_err(format!(
                "pageScript '{}' 文件 '{file}' 超过 {PAGE_SCRIPT_FILE_MAX_BYTES} 字节",
                script.id
            ));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            PluginError::ManifestInvalid(format!(
                "pageScript '{}' 文件 '{file}' 不是合法 UTF-8",
                script.id
            ))
        })?;
        if i > 0 {
            code.push_str("\n;\n");
        }
        code.push_str(text);
        if code.len() > PAGE_SCRIPT_MAX_BYTES {
            return manifest_err(format!(
                "pageScript '{}' 拼接后超过 {PAGE_SCRIPT_MAX_BYTES} 字节",
                script.id
            ));
        }
    }
    Ok(code)
}

/// 校验 sniff 规则包文件内容：≤256 KiB、合法 JSON、`format` 正确、`rules` 数组 ≤500 条。
pub fn validate_sniff_pack(bytes: &[u8]) -> Result<(), PluginError> {
    if bytes.len() > SNIFF_PACK_MAX_BYTES {
        return manifest_err(format!("sniffRules 文件超过 {SNIFF_PACK_MAX_BYTES} 字节"));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| PluginError::ManifestInvalid(format!("sniffRules JSON 解析失败: {e}")))?;
    if value.get("format").and_then(|f| f.as_str()) != Some("fluxdown-sniff-pack") {
        return manifest_err("sniffRules format 须为 fluxdown-sniff-pack".into());
    }
    let Some(rules) = value.get("rules").and_then(|r| r.as_array()) else {
        return manifest_err("sniffRules rules 须为数组".into());
    };
    if rules.len() > SNIFF_PACK_MAX_RULES {
        return manifest_err(format!("sniffRules 至多 {SNIFF_PACK_MAX_RULES} 条规则"));
    }
    Ok(())
}

/// 校验视图图标文件内容：≤16 KiB、合法 UTF-8、根元素为 `<svg>`（允许前置 BOM / 空白 /
/// XML 声明）。返回图标 SVG 文本。宿主只把它当单色蒙版渲染（GPUI `svg` / Web CSS
/// `mask-image`），这两种上下文都不执行脚本、不加载外部资源。
pub fn validate_view_icon(view_id: &str, bytes: Vec<u8>) -> Result<String, PluginError> {
    if bytes.len() > VIEW_ICON_MAX_BYTES {
        return manifest_err(format!(
            "view '{view_id}' icon 超过 {VIEW_ICON_MAX_BYTES} 字节"
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        PluginError::ManifestInvalid(format!("view '{view_id}' icon 不是合法 UTF-8"))
    })?;
    let head = text
        .trim_start_matches(|c: char| c == '\u{feff}' || c.is_whitespace())
        .to_ascii_lowercase();
    if !(head.starts_with("<svg") || head.starts_with("<?xml")) || !head.contains("<svg") {
        return manifest_err(format!("view '{view_id}' icon 不是 SVG 文档"));
    }
    Ok(text)
}

/// 小写 DNS 主机名（RFC 1123 标签，总长 ≤253）或 IP 字面量；禁 scheme/端口/路径/通配符。
fn is_valid_network_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        // IPv6 字面量必须已是规范小写形式。
        return host == host.to_ascii_lowercase();
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

fn is_valid_archive_update_url(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return super::bridge::is_globally_routable_unicast(ip);
    }
    let host = host.to_ascii_lowercase();
    host != "localhost"
        && !host.ends_with(".localhost")
        && !host.ends_with(".local")
        && is_valid_network_host(&host)
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

/// identity 校验：`^[a-z0-9_-]+@[a-z0-9_-]+$`，禁 '.'。`pub`：`plugin::manager`
/// 的 `purge` 复用同一判据校验失败插件的 identity 是否安全可用于拼路径/config
/// 键（671#8：此前 manager.rs 自留了一份逐字重复的 `is_safe_plugin_identity`）。
pub fn is_valid_identity(s: &str) -> bool {
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

fn is_valid_provider_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
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
    !p.chars().any(char::is_control)
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
    fn subscription_manifest_passes() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "subscriptions":[{"providerId":"demo-json","entry":"subscribe.js","timeoutMs":10000}]}"#,
        );
        assert!(m.validate().is_ok());
        assert_eq!(m.subscriptions[0].provider_id, "demo-json");
    }

    #[test]
    fn rejects_bad_subscription_provider_id() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "subscriptions":[{"providerId":"Demo.JSON","entry":"subscribe.js"}]}"#,
        );
        assert!(m.validate().is_err());
    }

    /// 内置 `rss` 是保留字：插件声明会被内置 provider 遮蔽而永不调用。
    #[test]
    fn rejects_reserved_rss_provider_id() {
        let m = parse_ok(
            r#"{"identity":"a@b","name":"N","version":"1.0.0",
                "subscriptions":[{"providerId":"rss","entry":"subscribe.js"}]}"#,
        );
        assert!(m.validate().is_err());
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

    fn contributes_json(contributes: &str) -> String {
        format!(
            r#"{{"identity":"a@b","name":"N","version":"1.0.0","permissions":["desktop"],
                "main":{{"entry":"main.js"}},"contributes":{contributes}}}"#
        )
    }

    #[test]
    fn accepts_valid_contributes() {
        let m = parse_ok(&contributes_json(
            r#"{"views":[{"id":"win","title":"W","location":"window",
                 "desktop":{"entry":"ui/window.mjs","width":960,"height":640,
                            "networkHosts":["b.example.com","a.example.com","127.0.0.1","::1"]}}],
                "commands":[{"id":"go","title":"Go"}]}"#,
        ));
        assert!(m.validate().is_ok());
        assert_eq!(
            m.network_hosts(),
            vec!["127.0.0.1", "::1", "a.example.com", "b.example.com"]
        );
    }

    #[test]
    fn rejects_invalid_desktop_entry() {
        for desktop in [
            r#"{"entry":"/abs/window.js"}"#,
            r#"{"entry":"../window.js"}"#,
            r#"{"entry":"window.ts"}"#,
            r#"{"entry":"window.js","width":100}"#,
            r#"{"entry":"window.js","height":5000}"#,
            r#"{"entry":"window.js","networkHosts":["https://a.com"]}"#,
            r#"{"entry":"window.js","networkHosts":["a.com:80"]}"#,
            r#"{"entry":"window.js","networkHosts":["*.a.com"]}"#,
            r#"{"entry":"window.js","networkHosts":["A.com"]}"#,
            r#"{"entry":"window.js","networkHosts":["a.com","a.com"]}"#,
        ] {
            let m = parse_ok(&contributes_json(&format!(
                r#"{{"views":[{{"id":"w","title":"W","location":"window","desktop":{desktop}}}]}}"#
            )));
            assert!(m.validate().is_err(), "应拒绝: {desktop}");
        }
    }

    #[test]
    fn rejects_too_many_desktop_hosts() {
        let hosts: Vec<String> = (0..65).map(|i| format!("\"h{i}.example.com\"")).collect();
        let m = parse_ok(&contributes_json(&format!(
            r#"{{"views":[{{"id":"w","title":"W","location":"window",
                 "desktop":{{"entry":"window.js","networkHosts":[{}]}}}}]}}"#,
            hosts.join(",")
        )));
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_unknown_contributes_field() {
        let r = PluginManifest::parse(
            contributes_json(r#"{"views":[{"id":"w","title":"W","location":"window","extra":1}]}"#)
                .as_bytes(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn empty_contributes_arrays_are_undeclared() {
        let m = parse_ok(&contributes_json(r#"{"views":[],"commands":[]}"#));
        assert!(m.validate().is_ok());
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureCase {
        name: String,
        manifest: serde_json::Value,
        files: Vec<String>,
        ok: bool,
        #[serde(default)]
        risk: Option<String>,
        #[serde(default)]
        network_hosts: Option<Vec<String>>,
    }

    /// 跨实现夹具：SDK 校验器与索引仓校验器复用同一文件。
    #[test]
    fn manifest_fixture_cases() {
        let cases: Vec<FixtureCase> =
            serde_json::from_str(include_str!("fixtures/manifest_cases.json")).expect("fixtures");
        assert!(cases.len() >= 30, "夹具不足 30 条");
        for case in cases {
            let bytes = serde_json::to_vec(&case.manifest).expect("serialize");
            let result = PluginManifest::parse(&bytes).and_then(|m| {
                m.validate()?;
                m.check_package_files(|p| case.files.iter().any(|f| f == p))?;
                Ok(m)
            });
            assert_eq!(
                result.is_ok(),
                case.ok,
                "{}: {:?}",
                case.name,
                result.as_ref().err()
            );
            if let Ok(m) = result {
                if let Some(risk) = &case.risk {
                    assert_eq!(m.risk().as_str(), risk, "{}", case.name);
                }
                if let Some(hosts) = &case.network_hosts {
                    assert_eq!(&m.network_hosts(), hosts, "{}", case.name);
                }
            }
        }
    }

    #[test]
    fn sniff_pack_validation() {
        use super::validate_sniff_pack;
        assert!(validate_sniff_pack(br#"{"format":"fluxdown-sniff-pack","rules":[]}"#).is_ok());
        assert!(validate_sniff_pack(br#"{"format":"other","rules":[]}"#).is_err());
        assert!(validate_sniff_pack(br#"{"format":"fluxdown-sniff-pack"}"#).is_err());
        assert!(validate_sniff_pack(b"not json").is_err());
        let many = format!(
            r#"{{"format":"fluxdown-sniff-pack","rules":[{}]}}"#,
            vec!["{}"; 501].join(",")
        );
        assert!(validate_sniff_pack(many.as_bytes()).is_err());
        let big = vec![b' '; super::SNIFF_PACK_MAX_BYTES + 1];
        assert!(validate_sniff_pack(&big).is_err());
    }

    #[test]
    fn view_icon_content_validation() {
        use super::{VIEW_ICON_MAX_BYTES, validate_view_icon};
        let svg = "\u{feff}\n <SVG viewBox=\"0 0 24 24\"><path d=\"M4 12h16\"/></SVG>";
        assert_eq!(validate_view_icon("v", svg.into()).unwrap(), svg);
        let declared = "<?xml version=\"1.0\"?><svg viewBox=\"0 0 1 1\"/>";
        assert!(validate_view_icon("v", declared.into()).is_ok());
        // XML 声明后没有 svg 根、非 SVG 文档、非 UTF-8、超限都拒绝。
        assert!(validate_view_icon("v", "<?xml version=\"1.0\"?><html/>".into()).is_err());
        assert!(validate_view_icon("v", "<html><svg/></html>".into()).is_err());
        assert!(validate_view_icon("v", vec![b'<', b's', b'v', b'g', 0xff]).is_err());
        let mut big = b"<svg>".to_vec();
        big.resize(VIEW_ICON_MAX_BYTES + 1, b' ');
        assert!(validate_view_icon("v", big).is_err());
        let mut edge = b"<svg>".to_vec();
        edge.resize(VIEW_ICON_MAX_BYTES, b' ');
        assert!(validate_view_icon("v", edge).is_ok());
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

    #[test]
    fn page_script_concat_limits() {
        use super::{PageScriptDecl, PageScriptRunAt, PageScriptWorld, concat_page_script};
        let script = |js: &[&str]| PageScriptDecl {
            id: "s".into(),
            title: "T".into(),
            matches: vec!["https://example.com/*".into()],
            exclude_matches: Vec::new(),
            js: js.iter().map(|s| (*s).to_string()).collect(),
            run_at: PageScriptRunAt::default(),
            world: PageScriptWorld::default(),
            all_frames: false,
        };
        let code = concat_page_script(&script(&["a.js", "b.js"]), |p| {
            Some(format!("// {p}").into_bytes())
        })
        .unwrap();
        assert_eq!(code, "// a.js\n;\n// b.js");
        // 缺失 / 非 UTF-8 / 单文件超限 / 拼接超限。
        assert!(concat_page_script(&script(&["a.js"]), |_| None).is_err());
        assert!(concat_page_script(&script(&["a.js"]), |_| Some(vec![0xff, 0xfe])).is_err());
        assert!(
            concat_page_script(&script(&["a.js"]), |_| Some(vec![b'x'; 256 * 1024 + 1])).is_err()
        );
        assert!(concat_page_script(&script(&["a.js"]), |_| Some(vec![b'x'; 200 * 1024])).is_ok());
        assert!(
            concat_page_script(&script(&["a.js", "b.js"]), |_| Some(vec![b'x'; 200 * 1024]))
                .is_err()
        );
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
