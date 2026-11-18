//! 外观偏好：PC/Web 共享的
//! 内置主题 ID、强调色方案、界面缩放，以及偏好快照的解析；另含桌面专属的字体 / 字号偏好。

use std::collections::BTreeMap;

use gpui::{Hsla, Rgba, SharedString, rgb};
use gpui_component::ThemeMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ThemePreference;

/// 云同步目录键：主题模式（`system` | `light` | `dark`）。
pub const THEME_MODE_KEY: &str = "appearance.theme_mode";
/// 云同步目录键：`builtin:<wire_name>` 或 `custom:<id>`（本机主题库中的导入主题）。
pub const DARK_THEME_KEY: &str = "appearance.dark_theme";
/// 云同步目录键：同 [`DARK_THEME_KEY`]，亮色槽位。
pub const LIGHT_THEME_KEY: &str = "appearance.light_theme";
/// 云同步目录键：强调色方案（`blue` | `green` | `violet` | `rose` | `custom`）。
pub const COLOR_SCHEME_KEY: &str = "appearance.color_scheme";
/// 云同步目录键：ARGB 整数。
pub const CUSTOM_COLOR_KEY: &str = "appearance.custom_color";
/// 设备本地键：缩放倍率（`0.8` ~ `1.5`）。
pub const UI_SCALE_KEY: &str = "ui_scale";

/// 自定义强调色的初始颜色。
pub const DEFAULT_CUSTOM_COLOR: u32 = 0xFF63_66F1;
/// 界面缩放选择器提供的档位（百分比）。
pub const UI_SCALE_PERCENTS: [u16; 7] = [80, 90, 100, 110, 120, 130, 150];
const UI_SCALE_MIN: u16 = 80;
const UI_SCALE_MAX: u16 = 150;

/// 内置主题；wire 名称保持既有持久化格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuiltinThemeId {
    DefaultDark,
    DefaultLight,
    MidnightBlue,
    Nord,
    WarmLight,
    Graphite,
    GraphiteLight,
}

impl BuiltinThemeId {
    /// 全部内置主题，顺序即 UI 显示顺序。
    pub const ALL: [Self; 7] = [
        Self::DefaultDark,
        Self::DefaultLight,
        Self::MidnightBlue,
        Self::Nord,
        Self::WarmLight,
        Self::Graphite,
        Self::GraphiteLight,
    ];

    /// 持久化 wire 名称。
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::DefaultDark => "defaultDark",
            Self::DefaultLight => "defaultLight",
            Self::MidnightBlue => "midnightBlue",
            Self::Nord => "nord",
            Self::WarmLight => "warmLight",
            Self::Graphite => "graphite",
            Self::GraphiteLight => "graphiteLight",
        }
    }

    /// 解析 wire 名称；接受 `builtin:` 前缀（sync_catalog 编码）与裸名。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let name = value.strip_prefix("builtin:").unwrap_or(value);
        Self::ALL.into_iter().find(|id| id.wire_name() == name)
    }

    /// 写入 `appearance.dark_theme` / `appearance.light_theme` 的值。
    #[must_use]
    pub fn pref_value(self) -> String {
        format!("builtin:{}", self.wire_name())
    }

    /// 主题本身的明暗外观。
    #[must_use]
    pub fn appearance(self) -> ThemeMode {
        match self {
            Self::DefaultDark | Self::MidnightBlue | Self::Nord | Self::Graphite => ThemeMode::Dark,
            Self::DefaultLight | Self::WarmLight | Self::GraphiteLight => ThemeMode::Light,
        }
    }

    /// i18n 文案键（`themeDefaultDark` 等）。
    #[must_use]
    pub fn label_key(self) -> &'static str {
        match self {
            Self::DefaultDark => "themeDefaultDark",
            Self::DefaultLight => "themeDefaultLight",
            Self::MidnightBlue => "themeMidnightBlue",
            Self::Nord => "themeNord",
            Self::WarmLight => "themeWarmLight",
            Self::Graphite => "themeGraphite",
            Self::GraphiteLight => "themeGraphiteLight",
        }
    }

    /// 指定明暗槽位的默认主题。
    #[must_use]
    pub fn default_for(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::DefaultDark,
            ThemeMode::Light => Self::DefaultLight,
        }
    }

    /// 指定明暗槽位可选的主题，仅展示与当前模式同外观的卡片。
    pub fn presets_for(mode: ThemeMode) -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(move |id| id.appearance() == mode)
    }
}

/// 强调色方案。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccentScheme {
    #[default]
    Blue,
    Green,
    Violet,
    Rose,
    Custom,
}

impl AccentScheme {
    /// 全部方案，顺序即 UI 显示顺序。
    pub const ALL: [Self; 5] = [
        Self::Blue,
        Self::Green,
        Self::Violet,
        Self::Rose,
        Self::Custom,
    ];

    /// 持久化 wire 名称。
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Violet => "violet",
            Self::Rose => "rose",
            Self::Custom => "custom",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|scheme| scheme.wire_name() == value)
    }

    /// i18n 文案键（`colorBlue` 等）。
    #[must_use]
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Blue => "colorBlue",
            Self::Green => "colorGreen",
            Self::Violet => "colorViolet",
            Self::Rose => "colorRose",
            Self::Custom => "colorCustom",
        }
    }

    /// 预设色；`Custom` 为其占位色。
    #[must_use]
    pub fn preset_argb(self) -> u32 {
        match self {
            Self::Blue => 0xFF3B_82F6,
            Self::Green => 0xFF22_C55E,
            Self::Violet => 0xFF8B_5CF6,
            Self::Rose => 0xFFF4_3F5E,
            Self::Custom => DEFAULT_CUSTOM_COLOR,
        }
    }

    /// 生效的强调色：预设取固定色，`Custom` 取用户 ARGB。
    #[must_use]
    pub fn color(self, custom_argb: u32) -> Hsla {
        let argb = match self {
            Self::Custom => custom_argb,
            preset => preset.preset_argb(),
        };
        argb_color(argb)
    }
}

/// ARGB → 不透明颜色；强调色忽略 alpha 字节。
#[must_use]
pub fn argb_color(argb: u32) -> Hsla {
    Hsla::from(rgb(argb & 0x00FF_FFFF))
}

/// 颜色 → 不透明 ARGB（alpha 固定 `FF`）。
#[must_use]
pub fn color_argb(color: Hsla) -> u32 {
    let rgba = Rgba::from(color);
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    0xFF00_0000 | (channel(rgba.r) << 16) | (channel(rgba.g) << 8) | channel(rgba.b)
}

/// 把缩放百分比限制到允许范围（80 ~ 150）并按 10 取整。
#[must_use]
pub fn normalize_ui_scale_percent(percent: u16) -> u16 {
    let clamped = percent.clamp(UI_SCALE_MIN, UI_SCALE_MAX);
    ((clamped + 5) / 10) * 10
}

/// 明暗槽位选中的主题。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThemeSelection {
    Builtin(BuiltinThemeId),
    /// 本机主题库中的主题 id；文件内容由调用方经 [`crate::register_custom_theme`] 提供，
    /// 未提供时该槽位回退到内置默认主题。
    Custom(SharedString),
}

impl ThemeSelection {
    /// 偏好值前缀：`custom:<id>`。
    pub const CUSTOM_PREFIX: &'static str = "custom:";

    /// 写入 `appearance.dark_theme` / `appearance.light_theme` 的值。
    #[must_use]
    pub fn pref_value(&self) -> String {
        match self {
            Self::Builtin(id) => id.pref_value(),
            Self::Custom(id) => format!("{}{id}", Self::CUSTOM_PREFIX),
        }
    }

    /// 解析偏好值；内置主题只接受与槽位外观一致的预设。
    #[must_use]
    pub fn parse(value: &str, mode: ThemeMode) -> Option<Self> {
        if let Some(id) = value.strip_prefix(Self::CUSTOM_PREFIX) {
            let id = id.trim();
            return (!id.is_empty()).then(|| Self::Custom(SharedString::from(id.to_owned())));
        }
        BuiltinThemeId::parse(value)
            .filter(|id| id.appearance() == mode)
            .map(Self::Builtin)
    }

    /// 自定义主题 id。
    #[must_use]
    pub fn custom_id(&self) -> Option<&SharedString> {
        match self {
            Self::Custom(id) => Some(id),
            Self::Builtin(_) => None,
        }
    }
}

/// 用户在外观页可调的全部选项；与偏好快照一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppearancePreferences {
    /// 本机界面字体；空值沿用主题，不覆盖等宽字体。
    pub font_family: Option<String>,
    pub theme_mode: ThemePreference,
    pub dark_theme: ThemeSelection,
    pub light_theme: ThemeSelection,
    pub color_scheme: AccentScheme,
    /// ARGB；仅在 `color_scheme == Custom` 时生效。
    pub custom_color: u32,
    /// 80 ~ 150，步进 10。
    pub ui_scale_percent: u16,
    /// 本机正文字号（px，界面缩放前）；`None` 沿用主题。其余文字角色与承载文字的行高按
    /// 「所选字号 / 主题正文字号」同比缩放，再与界面缩放相乘。
    pub font_size: Option<u16>,
    /// 本机正文字重；`None` 不覆盖主题。
    pub font_weight: Option<u16>,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            font_family: None,
            theme_mode: ThemePreference::System,
            dark_theme: ThemeSelection::Builtin(BuiltinThemeId::DefaultDark),
            light_theme: ThemeSelection::Builtin(BuiltinThemeId::DefaultLight),
            color_scheme: AccentScheme::Blue,
            custom_color: DEFAULT_CUSTOM_COLOR,
            ui_scale_percent: 100,
            font_size: None,
            font_weight: None,
        }
    }
}

impl AppearancePreferences {
    /// 从偏好快照解析；缺失或非法的键回退到默认值。
    #[must_use]
    pub fn from_values(values: &BTreeMap<String, Value>) -> Self {
        let defaults = Self::default();
        Self {
            font_family: values
                .get(crate::FONT_FAMILY_KEY)
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned),
            theme_mode: values
                .get(THEME_MODE_KEY)
                .and_then(Value::as_str)
                .map_or(defaults.theme_mode, parse_theme_mode),
            dark_theme: parse_selection(values.get(DARK_THEME_KEY), ThemeMode::Dark),
            light_theme: parse_selection(values.get(LIGHT_THEME_KEY), ThemeMode::Light),
            color_scheme: values
                .get(COLOR_SCHEME_KEY)
                .and_then(Value::as_str)
                .and_then(AccentScheme::parse)
                .unwrap_or(defaults.color_scheme),
            custom_color: values
                .get(CUSTOM_COLOR_KEY)
                .and_then(parse_argb)
                .unwrap_or(defaults.custom_color),
            ui_scale_percent: values
                .get(UI_SCALE_KEY)
                .and_then(parse_scale_percent)
                .unwrap_or(defaults.ui_scale_percent),
            font_size: values.get(crate::FONT_SIZE_KEY).and_then(parse_font_size),
            font_weight: values.get(crate::FONT_WEIGHT_KEY).and_then(parse_font_weight),
        }
    }

    /// 生效的强调色。
    #[must_use]
    pub fn accent(&self) -> Hsla {
        self.color_scheme.color(self.custom_color)
    }

    /// 缩放倍率（`ui_scale_percent / 100`）。
    #[must_use]
    pub fn ui_scale(&self) -> f32 {
        f32::from(self.ui_scale_percent) / 100.
    }

    /// 写入 [`UI_SCALE_KEY`] 的浮点缩放倍率。
    #[must_use]
    pub fn ui_scale_pref_value(&self) -> f64 {
        f64::from(self.ui_scale_percent) / 100.
    }

    /// 文字缩放倍率：所选正文字号相对主题正文字号 `theme_body_size`（px，界面缩放前）。
    #[must_use]
    pub fn font_scale(&self, theme_body_size: f32) -> f32 {
        match self.font_size {
            Some(size) if theme_body_size > 0. => f32::from(size) / theme_body_size,
            _ => 1.,
        }
    }

    /// 指定槽位当前选中的主题。
    #[must_use]
    pub fn theme(&self, mode: ThemeMode) -> &ThemeSelection {
        match mode {
            ThemeMode::Dark => &self.dark_theme,
            ThemeMode::Light => &self.light_theme,
        }
    }

    /// 指定槽位当前选中的内置主题；选中自定义主题时为该槽位的内置默认主题（即其回退）。
    #[must_use]
    pub fn builtin_theme(&self, mode: ThemeMode) -> BuiltinThemeId {
        match self.theme(mode) {
            ThemeSelection::Builtin(id) => *id,
            ThemeSelection::Custom(_) => BuiltinThemeId::default_for(mode),
        }
    }

    /// 设置指定槽位的内置主题。
    pub fn set_builtin_theme(&mut self, mode: ThemeMode, id: BuiltinThemeId) {
        self.set_theme(mode, ThemeSelection::Builtin(id));
    }

    /// 设置指定槽位的主题。
    pub fn set_theme(&mut self, mode: ThemeMode, selection: ThemeSelection) {
        match mode {
            ThemeMode::Dark => self.dark_theme = selection,
            ThemeMode::Light => self.light_theme = selection,
        }
    }

    /// 影响主题定义（而非仅模式/缩放）的字段是否一致。
    #[must_use]
    pub fn same_palette(&self, other: &Self) -> bool {
        self.dark_theme == other.dark_theme
            && self.light_theme == other.light_theme
            && self.color_scheme == other.color_scheme
            && (self.color_scheme != AccentScheme::Custom
                || self.custom_color == other.custom_color)
    }
}

fn parse_theme_mode(value: &str) -> ThemePreference {
    match value {
        "light" => ThemePreference::Light,
        "dark" => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

/// 槽位接受与其外观一致的内置主题或 `custom:<id>`；未知值回退默认。
fn parse_selection(value: Option<&Value>, mode: ThemeMode) -> ThemeSelection {
    value
        .and_then(Value::as_str)
        .and_then(|value| ThemeSelection::parse(value, mode))
        .unwrap_or_else(|| ThemeSelection::Builtin(BuiltinThemeId::default_for(mode)))
}

/// 接受 ARGB 整数（sync_catalog）或 6/8 位十六进制串（KvStore / 手输）。
fn parse_argb(value: &Value) -> Option<u32> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_f64().map(|v| v as u64))
            .and_then(|v| u32::try_from(v).ok()),
        Value::String(text) => parse_hex_argb(text),
        _ => None,
    }
}

/// `#RRGGBB` / `RRGGBB` / `AARRGGBB` → ARGB；alpha 缺省为不透明。
#[must_use]
pub fn parse_hex_argb(text: &str) -> Option<u32> {
    let hex = text.trim().trim_start_matches('#');
    let parsed = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(0xFF00_0000 | parsed),
        8 => Some(parsed),
        _ => None,
    }
}

/// ARGB → `RRGGBB` 大写十六进制（色盘输入框格式）。
#[must_use]
pub fn rgb_hex(argb: u32) -> String {
    format!("{:06X}", argb & 0x00FF_FFFF)
}

fn parse_font_weight(value: &Value) -> Option<u16> {
    let weight = match value {
        Value::Number(number) => u16::try_from(number.as_u64()?).ok()?,
        Value::String(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    ((100..=900).contains(&weight) && weight.is_multiple_of(100)).then_some(weight)
}

/// 接受整数 px（或其字符串形式），超出 [`FONT_SIZE_RANGE`](crate::FONT_SIZE_RANGE) 视为未设置。
fn parse_font_size(value: &Value) -> Option<u16> {
    let size = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    let size = size.round();
    let range = crate::FONT_SIZE_RANGE;
    (size.is_finite() && (f64::from(*range.start())..=f64::from(*range.end())).contains(&size))
        .then_some(size as u16)
}

/// 接受倍率数字（`1.2`）或其字符串形式（KvStore）。
fn parse_scale_percent(value: &Value) -> Option<u16> {
    let scale = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !scale.is_finite() {
        return None;
    }
    let percent = (scale * 100.).round();
    if !(f64::from(UI_SCALE_MIN)..=f64::from(UI_SCALE_MAX)).contains(&percent) {
        return None;
    }
    Some(normalize_ui_scale_percent(percent as u16))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gpui_component::ThemeMode;
    use serde_json::json;

    use super::{
        AccentScheme, AppearancePreferences, BuiltinThemeId, DEFAULT_CUSTOM_COLOR, ThemeSelection,
        argb_color, color_argb, normalize_ui_scale_percent, parse_hex_argb, rgb_hex,
    };
    use crate::ThemePreference;

    fn values(entries: &[(&str, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn empty_snapshot_yields_flutter_defaults() {
        let prefs = AppearancePreferences::from_values(&BTreeMap::new());
        assert_eq!(prefs, AppearancePreferences::default());
        assert_eq!(prefs.custom_color, DEFAULT_CUSTOM_COLOR);
        assert_eq!(prefs.ui_scale_percent, 100);
    }

    #[test]
    fn parses_sync_catalog_encoding() {
        let prefs = AppearancePreferences::from_values(&values(&[
            ("appearance.theme_mode", json!("dark")),
            ("appearance.dark_theme", json!("builtin:nord")),
            ("appearance.light_theme", json!("builtin:warmLight")),
            ("appearance.color_scheme", json!("custom")),
            ("appearance.custom_color", json!(0xFF11_2233_u32)),
            ("ui_scale", json!(1.2)),
        ]));
        assert_eq!(prefs.theme_mode, ThemePreference::Dark);
        assert_eq!(
            prefs.dark_theme,
            ThemeSelection::Builtin(BuiltinThemeId::Nord)
        );
        assert_eq!(
            prefs.light_theme,
            ThemeSelection::Builtin(BuiltinThemeId::WarmLight)
        );
        assert_eq!(prefs.color_scheme, AccentScheme::Custom);
        assert_eq!(prefs.custom_color, 0xFF11_2233);
        assert_eq!(prefs.ui_scale_percent, 120);
    }

    #[test]
    fn invalid_values_fall_back_per_key() {
        let prefs = AppearancePreferences::from_values(&values(&[
            ("appearance.theme_mode", json!("purple")),
            ("appearance.dark_theme", json!("custom:")),
            ("appearance.light_theme", json!("builtin:nord")),
            ("appearance.color_scheme", json!("teal")),
            ("appearance.custom_color", json!("#ABCDEF")),
            ("ui_scale", json!("3.0")),
        ]));
        assert_eq!(prefs.theme_mode, ThemePreference::System);
        assert_eq!(
            prefs.builtin_theme(ThemeMode::Dark),
            BuiltinThemeId::DefaultDark
        );
        assert_eq!(
            prefs.dark_theme,
            ThemeSelection::Builtin(BuiltinThemeId::DefaultDark)
        );
        assert_eq!(
            prefs.light_theme,
            ThemeSelection::Builtin(BuiltinThemeId::DefaultLight)
        );
        assert_eq!(prefs.color_scheme, AccentScheme::Blue);
        assert_eq!(prefs.custom_color, 0xFFAB_CDEF);
        assert_eq!(prefs.ui_scale_percent, 100);
    }

    #[test]
    fn custom_selection_round_trips_and_falls_back_to_default_builtin() {
        let prefs = AppearancePreferences::from_values(&values(&[(
            "appearance.dark_theme",
            json!("custom:1700000000_0"),
        )]));
        let selection = ThemeSelection::Custom("1700000000_0".into());
        assert_eq!(prefs.dark_theme, selection);
        assert_eq!(selection.pref_value(), "custom:1700000000_0");
        assert_eq!(
            prefs.builtin_theme(ThemeMode::Dark),
            BuiltinThemeId::DefaultDark
        );
        let mut other = prefs.clone();
        other.set_builtin_theme(ThemeMode::Dark, BuiltinThemeId::DefaultDark);
        assert!(!prefs.same_palette(&other));
    }

    #[test]
    fn ui_scale_accepts_kv_store_string_and_rounds_to_tenths() {
        let prefs = AppearancePreferences::from_values(&values(&[("ui_scale", json!("0.85"))]));
        assert_eq!(prefs.ui_scale_percent, 90);
        assert_eq!(normalize_ui_scale_percent(10), 80);
        assert_eq!(normalize_ui_scale_percent(144), 140);
        assert_eq!(normalize_ui_scale_percent(200), 150);
    }

    #[test]
    fn builtin_ids_round_trip_wire_names() {
        for id in BuiltinThemeId::ALL {
            assert_eq!(BuiltinThemeId::parse(id.wire_name()), Some(id));
            assert_eq!(BuiltinThemeId::parse(&id.pref_value()), Some(id));
        }
        assert_eq!(
            BuiltinThemeId::presets_for(ThemeMode::Light).collect::<Vec<_>>(),
            vec![
                BuiltinThemeId::DefaultLight,
                BuiltinThemeId::WarmLight,
                BuiltinThemeId::GraphiteLight
            ]
        );
        for scheme in AccentScheme::ALL {
            assert_eq!(AccentScheme::parse(scheme.wire_name()), Some(scheme));
        }
    }

    #[test]
    fn hex_helpers_match_flutter_picker_format() {
        assert_eq!(parse_hex_argb("#6366f1"), Some(0xFF63_66F1));
        assert_eq!(parse_hex_argb("806366F1"), Some(0x8063_66F1));
        assert_eq!(parse_hex_argb("fff"), None);
        assert_eq!(rgb_hex(0xFF63_66F1), "6366F1");
        for argb in [
            0xFF63_66F1,
            0xFF3B_82F6,
            0xFF00_0000,
            0xFFFF_FFFF,
            0x0012_3456,
        ] {
            assert_eq!(color_argb(argb_color(argb)), 0xFF00_0000 | argb);
        }
    }

    #[test]
    fn same_palette_ignores_custom_color_unless_custom_scheme() {
        let base = AppearancePreferences::default();
        let mut other = base.clone();
        other.custom_color = 0xFF00_0000;
        other.ui_scale_percent = 120;
        other.theme_mode = ThemePreference::Dark;
        assert!(base.same_palette(&other));
        other.color_scheme = AccentScheme::Custom;
        assert!(!base.same_palette(&other));
    }
}
