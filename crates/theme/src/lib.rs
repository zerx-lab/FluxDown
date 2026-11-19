//! FluxDown GPUI 客户端的主题系统：token 注册表、主题文件（`ThemeDocument`）、
//! 解析与运行时装配。
//!
//! - [`TOKENS`]：全部 token 的唯一事实源（类型、默认表达式、范围、分组）。
//! - [`ThemeDocument`]：主题文件 wire 格式（含 v1 / Flutter 迁移与规范导出）。
//! - [`resolve`]：文件 + 明暗模式 → 每个 token 的最终值（引用、透明度、混色、clamp、诊断）。
//! - [`FluxThemeState`]：已缩放的运行时快照，并投影到 gpui-component 与 gpui-base。
//!
//! 业务 feature 不在本 crate 中；主题库的文件存取由调用方实现。

#[macro_use]
mod json;

mod appearance;
mod builtin;
mod cjk;
mod document;
mod extended;
mod flutter;
mod fonts;
mod manager;
mod migrate;
mod registry;
mod resolve;
mod runtime;
mod schema;
mod value;

pub use appearance::*;
pub use builtin::{
    ACCENT_TOKEN_PATHS, BuiltinBase, MONO_FONT_SENTINEL, SANS_FONT_SENTINEL, foreground_for,
};
pub use cjk::{HanFontPreference, HanScript, set_han_font_preference};
pub use document::{
    Diagnostic, DiagnosticKind, ExportMode, THEME_FORMAT, THEME_SCHEMA_URL, THEME_SCHEMA_VERSION,
    ThemeDocument, ThemeMeta, ThemeParseError, TokenLayer,
};
pub use extended::*;
pub use flutter::FLUTTER_COLOR_MAP;
pub use fonts::{
    FONT_FAMILY_KEY, FONT_SIZE_KEY, FONT_SIZE_RANGE, FONT_SIZES, FONT_WEIGHT_KEY,
    available_font_families,
};
pub use gpui_base::{
    ColorTokens, RadiusTokens, SemanticThemeTokens, ShadowTokens, SpacingTokens, TextStyleToken,
    TypographyTokens,
};
pub use gpui_component::ThemeMode;
pub use json::OrderedJson;
pub use manager::*;
pub use migrate::{MIGRATIONS, Migration, V1_KEY_RENAMES};
pub use registry::{
    ByMode, DefaultExpr, GROUPS, Literal, TOKENS, TokenKind, TokenSpec, registry_json, token,
};
pub use resolve::{ResolveOptions, ResolvedModeTokens, resolve, resolve_with, resolved_snapshot};
pub use runtime::{ComponentTokens, DensityTokens, RADIUS_FULL_THRESHOLD, ResolvedTheme};
pub use schema::json_schema;
pub use value::{ShadowValue, TokenValue, color_hex, parse_hex_color};

/// 控件高度：全应用唯一一档，页面工具栏、对话框、表单、设置行、列表行内与独立窗口里的
/// 按钮 / 输入框 / 下拉 / 数字输入统一取此值（即 `density.control` 的默认值，未缩放）。
///
/// gpui-component 的 `Size::Small` 只有 21px、`Size::Medium` 是 26px（2rem）；统一取 28px，
/// 与活动栏、表头等 chrome 元素的节奏一致。
pub const CONTROL_HEIGHT: gpui::Pixels = gpui::px(28.);

/// 生成物的规范文本：两空格缩进 JSON + 末尾换行（`registry.json`、Schema 与
/// `*.resolved.json` 均用此格式，漂移守卫按字节比较）。
#[must_use]
pub fn to_pretty_json(value: &OrderedJson) -> String {
    value.to_pretty()
}
