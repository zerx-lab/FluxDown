use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gpui::{App, Global, SharedString, Window};
use gpui_component::{Theme as ComponentTheme, ThemeMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AppearancePreferences, BuiltinThemeId, ComponentTokens, DensityTokens, Diagnostic,
    ExtendedTokens, ResolveOptions, ResolvedTheme, SemanticThemeTokens, ThemeDocument,
    ThemeSelection, resolve_with,
};

/// 用户主题偏好；`System` 在每次安装时解析当前系统明暗模式。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    /// 偏好 wire 值（[`THEME_MODE_KEY`](crate::THEME_MODE_KEY)）。
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn resolve(self, cx: &App) -> ThemeMode {
        match self {
            Self::System => cx.window_appearance().into(),
            Self::Light => ThemeMode::Light,
            Self::Dark => ThemeMode::Dark,
        }
    }
}

/// 亮暗两个槽位各自使用的主题文件。
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeDocuments {
    pub dark: Arc<ThemeDocument>,
    pub light: Arc<ThemeDocument>,
}

impl ThemeDocuments {
    /// 同一文件同时用于两个槽位。
    #[must_use]
    pub fn single(document: Arc<ThemeDocument>) -> Self {
        Self {
            dark: Arc::clone(&document),
            light: document,
        }
    }

    #[must_use]
    pub fn get(&self, mode: ThemeMode) -> &Arc<ThemeDocument> {
        match mode {
            ThemeMode::Dark => &self.dark,
            ThemeMode::Light => &self.light,
        }
    }
}

/// 当前应用主题的完整快照。
///
/// gpui-component 的 legacy `Theme` 无法持久保存自定义 spacing/shadow；本全局状态
/// 因此保留完整解析结果，并在每次切换后重新投影到 `gpui_base::Theme`。
#[derive(Clone)]
pub struct FluxThemeState {
    documents: ThemeDocuments,
    appearance: AppearancePreferences,
    mode: ThemeMode,
    theme: ResolvedTheme,
    diagnostics: Arc<[Diagnostic]>,
    /// 文字倍率 = 所选正文字号相对主题正文字号的倍率 × 系统「文本大小」倍率（都未设置时为 1）。
    font_scale: f32,
    /// 窗口根节点的缺字回退链（汉字黑体，见 `cjk` 模块）；`None` 时沿用平台系统回退。
    font_fallbacks: Option<gpui::FontFallbacks>,
}

impl Global for FluxThemeState {}

impl FluxThemeState {
    /// 当前明暗模式正在使用的主题文件（未缩放）。
    pub fn document(&self) -> &Arc<ThemeDocument> {
        self.documents.get(self.mode)
    }

    /// 两个槽位的主题文件。
    pub fn documents(&self) -> &ThemeDocuments {
        &self.documents
    }

    /// 用户当前的外观选项（主题槽位、强调色、缩放、明暗偏好）。
    pub fn appearance(&self) -> &AppearancePreferences {
        &self.appearance
    }

    /// 用户选择的明暗偏好。
    pub fn preference(&self) -> ThemePreference {
        self.appearance.theme_mode
    }

    /// 已解析的实际明暗模式。
    pub fn mode(&self) -> ThemeMode {
        self.mode
    }

    /// 界面缩放百分比（80 ~ 150）。
    pub fn ui_scale_percent(&self) -> u16 {
        self.appearance.ui_scale_percent
    }

    /// 随文字宽度伸缩的固定尺寸（标签列宽、按字符数估算的列宽等），按界面缩放 × 文字倍率（字体大小
    /// 与系统文本大小）换算 `base`（默认字号下的 px）。高度不要用它：行高另含字体度量抬升，应由排版 /
    /// 密度 token 推导。
    pub fn text_extent(&self, base: f32) -> gpui::Pixels {
        gpui::px(base * self.appearance.ui_scale() * self.font_scale)
    }

    /// 完整运行时主题（已按界面缩放与字体大小适配）。
    pub fn resolved(&self) -> &ResolvedTheme {
        &self.theme
    }

    /// 当前完整 Base token（已按界面缩放）；应用自有组件应只从这里取值。
    pub fn tokens(&self) -> &SemanticThemeTokens {
        &self.theme.base
    }

    /// FluxDown 扩展 token（扩展/状态/进度色、caption/title、图标、线宽、焦点环；已按界面缩放）。
    pub fn extended(&self) -> &ExtendedTokens {
        &self.theme.extended
    }

    /// 控件高度阶梯（已按界面缩放，标题栏除外）。
    pub fn density(&self) -> &DensityTokens {
        &self.theme.density
    }

    /// 组件级 token（已按界面缩放）。
    pub fn components(&self) -> &ComponentTokens {
        &self.theme.components
    }

    /// 窗口根节点的缺字回退链；由每个窗口的 Root 插件挂到根文本样式上。
    pub fn font_fallbacks(&self) -> Option<&gpui::FontFallbacks> {
        self.font_fallbacks.as_ref()
    }

    /// 当前模式主题文件的解析诊断。
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

/// 本机主题库中已加载的自定义主题（`custom:<id>` → 文件）。主题库的存取由调用方
/// 实现，本 crate 只保存已解析的文件。
#[derive(Default)]
struct CustomThemes(HashMap<SharedString, Arc<ThemeDocument>>);

impl Global for CustomThemes {}

/// 提供（或替换）一个自定义主题；当前槽位正在使用该 id 时立即重新安装。
pub fn register_custom_theme(
    id: impl Into<SharedString>,
    document: Arc<ThemeDocument>,
    cx: &mut App,
) {
    let id = id.into();
    cx.default_global::<CustomThemes>()
        .0
        .insert(id.clone(), document);
    reinstall_if_selected(&id, cx);
}

/// 移除一个自定义主题；当前槽位正在使用该 id 时回退到内置默认主题。
pub fn unregister_custom_theme(id: &str, cx: &mut App) {
    let removed = cx
        .try_global::<CustomThemes>()
        .is_some_and(|themes| themes.0.contains_key(id));
    if removed {
        cx.global_mut::<CustomThemes>().0.remove(id);
        reinstall_if_selected(id, cx);
    }
}

/// 已提供的自定义主题。
pub fn custom_theme(id: &str, cx: &App) -> Option<Arc<ThemeDocument>> {
    cx.try_global::<CustomThemes>()
        .and_then(|themes| themes.0.get(id).cloned())
}

fn reinstall_if_selected(id: &str, cx: &mut App) {
    let Some(state) = cx.try_global::<FluxThemeState>() else {
        return;
    };
    let appearance = state.appearance.clone();
    let selected = [&appearance.dark_theme, &appearance.light_theme]
        .into_iter()
        .any(|selection| {
            selection
                .custom_id()
                .is_some_and(|custom| custom.as_ref() == id)
        });
    if selected {
        let documents = selection_documents(&appearance, cx);
        install(documents, appearance, None, cx);
    }
}

/// 槽位选择 → 主题文件；未提供的自定义主题回退到该槽位的内置默认主题。
fn selection_documents(appearance: &AppearancePreferences, cx: &App) -> ThemeDocuments {
    let document = |mode: ThemeMode| match appearance.theme(mode) {
        ThemeSelection::Builtin(id) => Arc::new(ThemeDocument::builtin(*id)),
        ThemeSelection::Custom(id) => custom_theme(id, cx)
            .unwrap_or_else(|| Arc::new(ThemeDocument::builtin(BuiltinThemeId::default_for(mode)))),
    };
    ThemeDocuments {
        dark: document(ThemeMode::Dark),
        light: document(ThemeMode::Light),
    }
}

/// 在 `gpui_component::init` 后、创建任何窗口前安装默认主题，并注册把汉字回退链挂到每个
/// 窗口根节点的 Root 插件。
pub fn init(cx: &mut App) {
    crate::cjk::register_root_plugin(cx);
    let appearance = AppearancePreferences::default();
    let documents = selection_documents(&appearance, cx);
    install(documents, appearance, None, cx);
}

/// 返回当前完整主题状态。
///
/// 调用方必须先执行 [`init`] 或 [`install_document`]；桌面 shell 在创建任何 view 前
/// 建立这一不变量。
pub fn active_theme(cx: &App) -> &FluxThemeState {
    cx.global::<FluxThemeState>()
}

/// 把一个主题文件同时装入亮暗两个槽位并同步到 gpui-component 与 gpui-base。
///
/// 其余外观选项（强调色、缩放）沿用当前状态；未初始化时取默认值。之后的
/// [`apply_appearance_preferences`] 在主题槽位与强调色不变时继续沿用该文件。
pub fn install_document(
    document: Arc<ThemeDocument>,
    preference: ThemePreference,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    let mut appearance = cx
        .try_global::<FluxThemeState>()
        .map_or_else(AppearancePreferences::default, |state| {
            state.appearance.clone()
        });
    appearance.theme_mode = preference;
    install(ThemeDocuments::single(document), appearance, window, cx);
}

/// 应用一组外观选项。主题槽位/强调色未变时沿用当前文件（含通过
/// [`install_document`] 装入的文件），否则按槽位选择重新取文件。
///
/// 界面缩放：GPUI 只有逐窗口的 rem 尺寸；gpui-component 的 `Root` 每帧把 `Theme::font_size`
/// 写入窗口 rem，因此缩放排版/间距/圆角 token 即驱动所有窗口，无需逐窗口 `set_rem_size`。
///
/// 不对外公开：外观是偏好的纯投影，只经 [`apply_appearance_preferences`] 修改。界面若绕过偏好
/// 直接改外观，下一次偏好快照 / 事件回流就会把它改回去。
fn set_appearance(appearance: AppearancePreferences, window: Option<&mut Window>, cx: &mut App) {
    let documents = match cx.try_global::<FluxThemeState>() {
        Some(state) if state.appearance.same_palette(&appearance) => state.documents.clone(),
        _ => selection_documents(&appearance, cx),
    };
    install(documents, appearance, window, cx);
}

/// 偏好快照 → 外观。读取 `appearance.theme_mode` / `appearance.dark_theme` /
/// `appearance.light_theme` / `appearance.color_scheme` / `appearance.custom_color` /
/// `ui_scale` / `desktop.font_family` / `desktop.font_size`；与当前状态一致时不做任何事，
/// 可在每次快照/偏好事件上幂等调用。
pub fn apply_appearance_preferences(values: &BTreeMap<String, Value>, cx: &mut App) {
    let appearance = AppearancePreferences::from_values(values);
    if cx
        .try_global::<FluxThemeState>()
        .is_some_and(|state| state.appearance == appearance)
    {
        return;
    }
    set_appearance(appearance, None, cx);
}

/// 系统外观变化时按当前偏好重新解析 `System`；显式亮/暗偏好保持不变。
///
/// 每个窗口都订阅系统外观，同一次变化会回调多次：解析结果与当前模式一致时不重装。
pub fn sync_system_theme(window: &mut Window, cx: &mut App) {
    let state = active_theme(cx);
    if state.preference() == ThemePreference::System
        && ThemePreference::System.resolve(cx) != state.mode
    {
        let appearance = state.appearance.clone();
        set_appearance(appearance, Some(window), cx);
    }
}

/// 用当前状态的主题文件与外观重新安装（不修改保存的偏好）：重新查询平台可用字体，
/// 并在脚本改写了 gpui-component / gpui-base 全局主题后恢复 FluxDown 投影。
pub fn reinstall_current(cx: &mut App) {
    let state = active_theme(cx);
    install(state.documents.clone(), state.appearance.clone(), None, cx);
}

/// 系统辅助功能「文本大小」倍率（Windows `UISettings.TextScaleFactor`）；由宿主读取后经
/// [`set_system_text_scale`] 写入，未写入时为 1。与显示缩放（DPI）无关：后者由 GPUI 按显示器处理。
#[derive(Clone, Copy)]
struct SystemTextScale(f32);

impl Global for SystemTextScale {}

/// 系统文本大小倍率的允许范围（Windows 辅助功能「文本大小」为 100% ~ 225%）。
const SYSTEM_TEXT_SCALE_RANGE: std::ops::RangeInclusive<f32> = 1.0..=2.25;

fn system_text_scale(cx: &App) -> f32 {
    cx.try_global::<SystemTextScale>()
        .map_or(1., |scale| scale.0)
}

/// 记录系统「文本大小」倍率，变化时按当前主题与偏好重装：与字体大小偏好相乘，只放大文字与承载
/// 文字的行 / 控件高度（同字体大小，见 `fonts::fit_text`），图标、间距不变。非有限值按 1，超出
/// 100% ~ 225% 的值截到边界；与当前值相同时不做任何事，可在每次窗口激活时调用。
pub fn set_system_text_scale(scale: f64, cx: &mut App) {
    let scale = if scale.is_finite() {
        (scale as f32).clamp(
            *SYSTEM_TEXT_SCALE_RANGE.start(),
            *SYSTEM_TEXT_SCALE_RANGE.end(),
        )
    } else {
        1.
    };
    if (system_text_scale(cx) - scale).abs() < f32::EPSILON {
        return;
    }
    cx.set_global(SystemTextScale(scale));
    if cx.has_global::<FluxThemeState>() {
        reinstall_current(cx);
    }
}

/// 当前主题完整已解析 token：注册表路径 → wire 值（颜色为 `#rrggbbaa`，长度 / 圆角 / 字重 / 比例
/// 为数值，字体为字符串，阴影为层数组）。
#[must_use]
pub fn resolved_token_map(cx: &App) -> BTreeMap<String, Value> {
    active_theme(cx)
        .resolved()
        .token_values()
        .into_iter()
        .map(|(path, value)| (path.to_owned(), value.to_json()))
        .collect()
}

fn install(
    documents: ThemeDocuments,
    appearance: AppearancePreferences,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    let mode = appearance.theme_mode.resolve(cx);
    let options = ResolveOptions {
        accent: Some(appearance.accent()),
        ensure_primary_contrast: true,
    };
    let (values, diagnostics) = resolve_with(documents.get(mode), mode, &options);
    let mut theme = values.to_theme(appearance.ui_scale());
    if appearance.font_family.is_some() {
        crate::fonts::apply_font_family(
            &mut theme,
            appearance.font_family.as_deref(),
            &crate::available_font_families(cx),
        );
    }
    let han_fonts = crate::cjk::plan(
        crate::cjk::current_preference(cx),
        &theme.base.typography.sans,
        cx,
    );
    if let Some(primary) = han_fonts.primary {
        theme.base.typography.sans = primary;
    }
    // 行高下限取界面字体与等宽字体中较高的自然行高：两者都会出现在同一套行高 token 下。
    let typography = &theme.base.typography;
    let line_ratio = crate::fonts::natural_line_ratio(&typography.sans, cx)
        .max(crate::fonts::natural_line_ratio(&typography.mono, cx));
    let theme_body_size = theme.base.typography.sm.size.as_f32() / appearance.ui_scale();
    let font_scale = appearance.font_scale(theme_body_size) * system_text_scale(cx);
    crate::fonts::fit_text(&mut theme, font_scale, line_ratio);
    let tokens = &theme.base;
    let extended = &theme.extended;

    ComponentTheme::change(mode, None, cx);
    project_onto_kit(ComponentTheme::global_mut(cx), &theme);
    ComponentTheme::sync_base(cx);
    {
        let base_theme = gpui_base::Theme::global_mut(cx);
        base_theme.tokens = tokens.clone();
        // 面板分隔把手（侧栏|内容、详情面板）与其他结构线一致用 hairline。
        base_theme.resizable.handle = Some(extended.colors.hairline);
    }
    cx.set_global(FluxThemeState {
        documents,
        appearance,
        mode,
        theme,
        diagnostics: diagnostics.into(),
        font_scale,
        font_fallbacks: han_fonts.fallbacks,
    });

    for handle in cx.windows() {
        if handle.update(cx, |_, window, _| window.refresh()).is_err() {
            // 已关闭的窗口不用刷新，继续更新其他窗口的主题。
            continue;
        }
    }
    if let Some(window) = window {
        window.refresh();
    }
}

/// 同时写 gpui-component 的 legacy 颜色字段与同名 `tokens` 字段（控件分别读两者）。
macro_rules! paint {
    ($kit:ident, $($field:ident = $value:expr),+ $(,)?) => {
        $(
            let value: gpui::Hsla = $value;
            $kit.$field = value;
            $kit.tokens.$field = value.into();
        )+
    };
}

/// 把解析后的 FluxDown token 投影到 gpui-component 主题。
///
/// `ComponentTheme::change` 按库默认主题（primary = neutral-900 / neutral-50）推导全部派生色，
/// `apply_semantic_tokens` 只覆盖 17 个 Base 色；凡是库控件会读、又应跟随 FluxDown 主题的派生色
/// 都必须在这里重写，否则停留在库默认的黑 / 白 / 灰。强调色派生取注册表 token，主题文件可覆盖。
fn project_onto_kit(kit: &mut ComponentTheme, theme: &ResolvedTheme) {
    let tokens = &theme.base;
    let colors = tokens.colors;
    let extended = theme.extended.colors;
    kit.apply_semantic_tokens(tokens);
    kit.focus_ring = false;
    // gpui-component 的 `text_base` 取 `font_size`（默认映射到 md=16px），
    // 对桌面密度偏大；按桌面端 13px 正文基线取 sm。
    kit.font_size = tokens.typography.sm.size;

    let primary_hover = shift_toward_contrast(colors.primary, 0.08);
    let primary_active = shift_toward_contrast(colors.primary, 0.13);
    let secondary_hover = shift_toward_contrast(colors.secondary, 0.05);
    let secondary_active = shift_toward_contrast(colors.secondary, 0.09);
    let danger_hover = shift_toward_contrast(colors.destructive, 0.08);
    let danger_active = shift_toward_contrast(colors.destructive, 0.13);
    // 下载列表不画网格：行分隔线与表头竖线都取透明，只靠悬停 / 选中底色
    // 区分行；表头与内容同底色。列宽拖拽柄在悬停表头时仍按 `border` 显示。
    let no_line = colors.surface.opacity(0.);
    paint!(
        kit,
        // 对话框、输入框、Root 等取 `background`；FluxDown 的内容面统一是 `surface`
        // （chrome 区由各页显式着色），这里对齐，避免对话框发灰与白色内容区不一致。
        background = colors.surface,
        title_bar = extended.chrome,
        title_bar_border = extended.hairline,
        // Sidebar / Settings 侧栏只读 sidebar_* 系列。
        sidebar = colors.surface,
        sidebar_foreground = colors.surface_foreground,
        sidebar_border = colors.border,
        sidebar_accent = colors.accent,
        sidebar_accent_foreground = colors.accent_foreground,
        sidebar_primary = colors.primary,
        sidebar_primary_foreground = colors.primary_foreground,
        // Button / Link 系列：`Button::primary()` 等。
        primary_hover = primary_hover,
        primary_active = primary_active,
        secondary_hover = secondary_hover,
        secondary_active = secondary_active,
        danger_hover = danger_hover,
        danger_active = danger_active,
        button_primary = colors.primary,
        button_primary_foreground = colors.primary_foreground,
        button_primary_hover = primary_hover,
        button_primary_active = primary_active,
        button_secondary = colors.secondary,
        button_secondary_foreground = colors.secondary_foreground,
        button_secondary_hover = secondary_hover,
        button_secondary_active = secondary_active,
        button_danger = colors.destructive,
        button_danger_foreground = colors.destructive_foreground,
        button_danger_hover = danger_hover,
        button_danger_active = danger_active,
        // 强调色派生：链接、输入框光标与选区、列表选中、进度条、滑块、拖放。
        link = extended.accent_text,
        link_hover = extended.accent_text,
        link_active = extended.accent_text,
        caret = extended.caret,
        selection = extended.text_selection,
        list_active = colors.accent,
        list_active_border = colors.primary,
        progress_bar = extended.progress_fill,
        slider_bar = colors.primary,
        slider_thumb = colors.primary_foreground,
        drag_border = extended.drag_border,
        drop_target = extended.drop_target,
        table = colors.surface,
        table_active = colors.accent,
        table_active_border = colors.primary,
        table_even = colors.surface,
        table_head = colors.surface,
        table_head_foreground = extended.text_tertiary,
        table_hover = extended.row_hover,
        table_row_border = no_line,
    );
}

/// 向对比方向偏移亮度：亮色变暗、暗色变亮（hover / active 派生）。
fn shift_toward_contrast(color: gpui::Hsla, amount: f32) -> gpui::Hsla {
    let delta = if color.l >= 0.5 { -amount } else { amount };
    gpui::Hsla {
        l: (color.l + delta).clamp(0., 1.),
        ..color
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestApp;

    use super::*;
    use crate::value::number_value;
    use crate::{TOKENS, color_hex};

    fn app() -> TestApp {
        let mut app = TestApp::new();
        app.update(|cx| {
            gpui_component::init(cx);
            init(cx);
        });
        app
    }

    fn is_hex8(value: &Value) -> bool {
        value.as_str().is_some_and(|text| {
            text.len() == 9
                && text.starts_with('#')
                && text[1..]
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
    }

    #[test]
    fn token_map_covers_registry_with_wire_values() {
        let app = app();
        let map = app.read(resolved_token_map);
        assert_eq!(map.len(), TOKENS.len());
        assert!(
            is_hex8(&map["colors.primary"]),
            "{:?}",
            map["colors.primary"]
        );
        assert!(map["spacing.md"].is_number());
        assert!(
            map["typography.sans"]
                .as_str()
                .is_some_and(|f| !f.is_empty())
        );
    }

    #[test]
    fn token_map_matches_active_theme() {
        let app = app();
        app.read(|cx| {
            let map = resolved_token_map(cx);
            let theme = active_theme(cx);
            assert_eq!(
                map["colors.primary"],
                Value::from(color_hex(theme.tokens().colors.primary))
            );
            assert_eq!(
                map["colors.chrome"],
                Value::from(color_hex(theme.extended().colors.chrome))
            );
            assert_eq!(
                map["spacing.md"],
                number_value(theme.tokens().spacing.md.as_f32())
            );
        });
    }

    #[test]
    fn reinstall_current_restores_projected_theme() {
        let mut app = app();
        app.update(|cx| {
            gpui_base::Theme::global_mut(cx).tokens.colors.primary = gpui::red();
            reinstall_current(cx);
        });
        app.read(|cx| {
            assert_eq!(
                gpui_base::Theme::global(cx).tokens.colors.primary,
                active_theme(cx).tokens().colors.primary
            );
        });
    }

    #[test]
    fn system_text_scale_enlarges_text_and_survives_preference_changes() {
        let mut app = app();
        let body = app.read(|cx| active_theme(cx).tokens().typography.sm.size.as_f32());
        app.update(|cx| set_system_text_scale(1.5, cx));
        app.read(|cx| {
            let theme = active_theme(cx);
            assert!((theme.tokens().typography.sm.size.as_f32() - body * 1.5).abs() < 0.01);
            // 间距不随文字放大。
            assert!((theme.text_extent(10.).as_f32() - 15.).abs() < 0.01);
        });
        // 偏好快照回流（改界面缩放）会重装主题，系统文本大小必须仍然生效。
        let values = BTreeMap::from([(crate::UI_SCALE_KEY.to_owned(), Value::from(1.2))]);
        app.update(|cx| apply_appearance_preferences(&values, cx));
        app.read(|cx| {
            let size = active_theme(cx).tokens().typography.sm.size.as_f32();
            assert!((size - body * 1.2 * 1.5).abs() < 0.01, "{size}");
        });
    }

    #[test]
    fn system_text_scale_is_clamped_and_rejects_non_finite_values() {
        let mut app = app();
        let body = app.read(|cx| active_theme(cx).tokens().typography.sm.size.as_f32());
        let size_after = |app: &mut TestApp, scale: f64| {
            app.update(|cx| set_system_text_scale(scale, cx));
            app.read(|cx| active_theme(cx).tokens().typography.sm.size.as_f32())
        };
        assert!((size_after(&mut app, 9.) - body * 2.25).abs() < 0.01);
        assert!((size_after(&mut app, 0.5) - body).abs() < 0.01);
        assert!((size_after(&mut app, 2.) - body * 2.).abs() < 0.01);
        assert!((size_after(&mut app, f64::NAN) - body).abs() < 0.01);
    }
}
