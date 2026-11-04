//! 内置主题基底：沿用既有预设的 Layer0 颜色 → Base 语义 token，
//! 以及 `extends: "builtin:*"` 的取值。

use gpui::{FontWeight, Hsla, Rgba, px, rgb};
use gpui_base::{ColorTokens, SemanticThemeTokens, ShadowTokens, TextStyleToken, TypographyTokens};
use gpui_component::ThemeMode;

use crate::runtime::semantic_slot;
use crate::{BuiltinThemeId, DefaultExpr, ShadowValue, TOKENS, TokenValue};

/// 字体哨兵：平台默认等宽字体（macOS Menlo / Windows Consolas / Linux DejaVu Sans Mono），
/// 运行时替换为实际字体名；网页预览可直接当 CSS 通用族名使用。
pub const MONO_FONT_SENTINEL: &str = "monospace";
/// 字体哨兵：平台默认界面字体（macOS 系统字体 / Windows 系统界面字体如微软雅黑 UI、Segoe UI /
/// Linux 系统无衬线字体），运行时替换为 gpui 的 `.SystemUIFont`；网页预览可直接当 CSS 通用族名使用。
pub const SANS_FONT_SENTINEL: &str = "system-ui";
const SHADOW_ALPHA: f32 = 46.0 / 255.0;
/// 强调色背景透明度的 8-bit 量化：0.10 / 0.15 / 0.18。
const ACCENT_ALPHA_LIGHT: u8 = 26;
const ACCENT_ALPHA_SOFT: u8 = 38;
const ACCENT_ALPHA_DARK: u8 = 46;

/// `extends` 可取的内置基底。单预设时另一模式取 FluxDown 默认主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinBase {
    /// FluxDown 默认亮暗对。
    Default,
    DefaultDark,
    DefaultLight,
    MidnightBlue,
    Nord,
    WarmLight,
    /// 石墨：近黑三层中性色 + 单一信号蓝，2/4px 方角、1px 焦点线的高密度控制台风格。
    Graphite,
    /// 石墨亮色：与 `Graphite` 同一层级模型与形状的纸面版本。
    GraphiteLight,
}

impl BuiltinBase {
    pub const ALL: [Self; 8] = [
        Self::Default,
        Self::DefaultDark,
        Self::DefaultLight,
        Self::MidnightBlue,
        Self::Nord,
        Self::WarmLight,
        Self::Graphite,
        Self::GraphiteLight,
    ];

    /// 文件中的 `extends` 取值。
    #[must_use]
    pub fn extends_value(self) -> &'static str {
        match self {
            Self::Default => "builtin:default",
            Self::DefaultDark => "builtin:default-dark",
            Self::DefaultLight => "builtin:default-light",
            Self::MidnightBlue => "builtin:midnight-blue",
            Self::Nord => "builtin:nord",
            Self::WarmLight => "builtin:warm-light",
            Self::Graphite => "builtin:graphite",
            Self::GraphiteLight => "builtin:graphite-light",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|base| base.extends_value() == value)
    }

    /// 对应的单个内置预设；`Default` 为亮暗对，返回 `None`。
    #[must_use]
    pub fn preset(self) -> Option<BuiltinThemeId> {
        match self {
            Self::Default => None,
            Self::DefaultDark => Some(BuiltinThemeId::DefaultDark),
            Self::DefaultLight => Some(BuiltinThemeId::DefaultLight),
            Self::MidnightBlue => Some(BuiltinThemeId::MidnightBlue),
            Self::Nord => Some(BuiltinThemeId::Nord),
            Self::WarmLight => Some(BuiltinThemeId::WarmLight),
            Self::Graphite => Some(BuiltinThemeId::Graphite),
            Self::GraphiteLight => Some(BuiltinThemeId::GraphiteLight),
        }
    }

    /// 某模式实际使用的预设调色板。
    #[must_use]
    pub fn preset_for(self, mode: ThemeMode) -> BuiltinThemeId {
        match self.preset() {
            Some(id) if id.appearance() == mode => id,
            _ => BuiltinThemeId::default_for(mode),
        }
    }
}

impl From<BuiltinThemeId> for BuiltinBase {
    fn from(id: BuiltinThemeId) -> Self {
        match id {
            BuiltinThemeId::DefaultDark => Self::DefaultDark,
            BuiltinThemeId::DefaultLight => Self::DefaultLight,
            BuiltinThemeId::MidnightBlue => Self::MidnightBlue,
            BuiltinThemeId::Nord => Self::Nord,
            BuiltinThemeId::WarmLight => Self::WarmLight,
            BuiltinThemeId::Graphite => Self::Graphite,
            BuiltinThemeId::GraphiteLight => Self::GraphiteLight,
        }
    }
}

/// 基底在某模式下的全部 token 值（注册表顺序）：Base token 来自调色板，
/// 另含预设对非 Base token 的固定取值（[`preset_overrides`]）。
///
/// `accent` 为用户强调色，用于重算
/// `primary` / `accent` / `ring` 系列；文件中显式给出的值仍优先于基底。
pub(crate) fn base_values(
    base: BuiltinBase,
    mode: ThemeMode,
    accent: Option<Hsla>,
) -> Vec<(&'static str, TokenValue)> {
    let preset = base.preset_for(mode);
    let mut colors = color_tokens(palette(preset));
    if let Some(accent) = accent {
        apply_accent(&mut colors, accent);
    }
    let mut tokens = semantic_tokens(colors);
    let overrides = preset_overrides(preset);
    TOKENS
        .iter()
        .filter_map(|spec| {
            if let Some((_, value)) = overrides.iter().find(|(path, _)| *path == spec.path) {
                return Some((spec.path, value.clone()));
            }
            if spec.default != DefaultExpr::Base {
                return None;
            }
            semantic_slot(&mut tokens, spec.path).map(|slot| (spec.path, slot.get()))
        })
        .collect()
}

/// 预设在调色板之外固定的 token（形状、密度、排版、阴影与扩展色）。
///
/// 只用于「整套设计语言」类预设；不得包含 [`ACCENT_TOKEN_PATHS`]（会钉死用户强调色）
/// 与 `kitBound` token（只跟随 `radius.md/lg`）。主题文件中的显式值仍优先。
fn preset_overrides(id: BuiltinThemeId) -> Vec<(&'static str, TokenValue)> {
    match id {
        BuiltinThemeId::DefaultDark
        | BuiltinThemeId::DefaultLight
        | BuiltinThemeId::MidnightBlue
        | BuiltinThemeId::Nord
        | BuiltinThemeId::WarmLight => Vec::new(),
        BuiltinThemeId::Graphite => graphite_overrides(&GraphiteChrome {
            success: 0x25E192,
            warning: 0xFFCC56,
            info: 0x23C4F8,
            text_tertiary: 0x747B8B,
            hairline: 0x16181D,
            row_hover: 0xECEE_F20A,
            chrome: 0x121317,
            nav_hover: 0x16181D,
            nav_selected: 0x1D2027,
            shadow_alpha: [0x1A, 0x33, 0x66],
        }),
        BuiltinThemeId::GraphiteLight => graphite_overrides(&GraphiteChrome {
            success: 0x1EB475,
            warning: 0xD99A00,
            info: 0x0EA5D9,
            text_tertiary: 0x80828D,
            hairline: 0xEFF0F3,
            row_hover: 0x1E1F_240A,
            chrome: 0xF9F9FB,
            nav_hover: 0xEFF0F3,
            nav_selected: 0xE7E8EC,
            shadow_alpha: [0x1A, 0x14, 0x29],
        }),
    }
}

/// 石墨亮暗两套共用的形状 / 密度，只有扩展色与浮层阴影浓度随外观变化。
struct GraphiteChrome {
    success: u32,
    warning: u32,
    info: u32,
    text_tertiary: u32,
    hairline: u32,
    /// RGBA。
    row_hover: u32,
    chrome: u32,
    nav_hover: u32,
    nav_selected: u32,
    /// `shadow.{sm,md,lg}` 的黑色 alpha。
    shadow_alpha: [u32; 3],
}

fn graphite_overrides(chrome: &GraphiteChrome) -> Vec<(&'static str, TokenValue)> {
    let number = TokenValue::Number;
    let solid = |value: u32| TokenValue::Color(color(value));
    // (x, y, blur, spread, alpha)
    let shadow = |layers: &[(f32, f32, f32, f32, u32)]| {
        TokenValue::Shadow(
            layers
                .iter()
                .map(|&(x, y, blur, spread, alpha)| ShadowValue {
                    x,
                    y,
                    blur,
                    spread,
                    color: Hsla::from(gpui::rgba(alpha)),
                    inset: false,
                })
                .collect(),
        )
    };
    let [sm, md, lg] = chrome.shadow_alpha;
    vec![
        ("colors.success", solid(chrome.success)),
        ("colors.warning", solid(chrome.warning)),
        ("colors.info", solid(chrome.info)),
        ("colors.textTertiary", solid(chrome.text_tertiary)),
        ("colors.hairline", solid(chrome.hairline)),
        (
            "colors.rowHover",
            TokenValue::Color(Hsla::from(gpui::rgba(chrome.row_hover))),
        ),
        ("colors.chrome", solid(chrome.chrome)),
        ("colors.navHover", solid(chrome.nav_hover)),
        ("colors.navSelected", solid(chrome.nav_selected)),
        ("radius.sm", number(2.)),
        ("radius.md", number(2.)),
        ("radius.lg", number(4.)),
        ("radius.xl", number(6.)),
        ("typography.title.size", number(14.)),
        ("typography.title.weight", number(500.)),
        // 常驻面无阴影；只有浮层（菜单 / 对话框 / 浮动面板）轻微抬起。
        (
            "shadow.sm",
            shadow(&[(0., 4., 6., -1., sm), (0., 2., 4., -2., sm)]),
        ),
        ("shadow.md", shadow(&[(0., 4., 12., 0., md)])),
        ("shadow.lg", shadow(&[(0., 8., 24., 0., lg)])),
        ("focusRing.width", number(1.)),
        ("focusRing.offset", number(1.)),
        ("density.control", number(32.)),
        ("density.navRow", number(32.)),
        ("components.navItem.radius", number(3.)),
        ("components.checkbox.radius", number(2.)),
        ("components.tab.radius", number(4.)),
        ("components.progress.radius", number(2.)),
    ]
}

fn semantic_tokens(colors: ColorTokens) -> SemanticThemeTokens {
    // 桌面正文基线 13/18（macOS 正文字号，Windows/Linux 桌面列表同样取 13），
    // gpui-component 的 rem 也取 `sm`，因此整套 rem 工具类随之对齐。
    // gpui-base 0.9 起 token 结构体为 `#[non_exhaustive]`，只能 Default 后写字段。
    let mut typography = TypographyTokens::default();
    typography.sans = SANS_FONT_SENTINEL.into();
    typography.mono = MONO_FONT_SENTINEL.into();
    typography.sm = text_style_token(13., 18.);

    let mut tokens = SemanticThemeTokens::default();
    tokens.colors = colors;
    tokens.typography = typography;
    tokens.shadow = ShadowTokens::elevations(Hsla {
        h: 0.,
        s: 0.,
        l: 0.,
        a: SHADOW_ALPHA,
    });
    tokens
}

fn text_style_token(size: f32, line_height: f32) -> TextStyleToken {
    let mut style = TextStyleToken::default();
    style.size = px(size);
    style.line_height = px(line_height);
    style.weight = FontWeight::NORMAL;
    style
}

/// 预设中映射到 Base token 的 Layer0 颜色。
struct Palette {
    accent: u32,
    accent_background_alpha: u8,
    /// 预设固定的强调色前景（如 Nord）；`None` 按强调色亮度自动取黑/白。
    accent_foreground: Option<u32>,
    background: u32,
    surface1: u32,
    surface2: u32,
    text_primary: u32,
    text_secondary: u32,
    border: u32,
    status_error: u32,
}

fn palette(id: BuiltinThemeId) -> &'static Palette {
    match id {
        BuiltinThemeId::DefaultDark => &Palette {
            accent: 0x3B82F6,
            accent_background_alpha: ACCENT_ALPHA_DARK,
            accent_foreground: None,
            background: 0x1C1C1E,
            surface1: 0x2C2C2E,
            surface2: 0x3A3A3C,
            text_primary: 0xF5F5F7,
            text_secondary: 0xA1A1A6,
            border: 0x48484A,
            status_error: 0xEF4444,
        },
        BuiltinThemeId::DefaultLight => &Palette {
            accent: 0x3B82F6,
            accent_background_alpha: ACCENT_ALPHA_LIGHT,
            accent_foreground: None,
            background: 0xF8F9FA,
            surface1: 0xFFFFFF,
            surface2: 0xF1F3F5,
            text_primary: 0x09090B,
            text_secondary: 0x71717A,
            border: 0xE4E4E7,
            status_error: 0xEF4444,
        },
        BuiltinThemeId::MidnightBlue => &Palette {
            accent: 0x60A5FA,
            accent_background_alpha: ACCENT_ALPHA_SOFT,
            accent_foreground: None,
            background: 0x0F172A,
            surface1: 0x1E293B,
            surface2: 0x334155,
            text_primary: 0xF1F5F9,
            text_secondary: 0x94A3B8,
            border: 0x334155,
            status_error: 0xEF4444,
        },
        BuiltinThemeId::Nord => &Palette {
            accent: 0x88C0D0,
            accent_background_alpha: ACCENT_ALPHA_SOFT,
            accent_foreground: Some(0x2E3440),
            background: 0x2E3440,
            surface1: 0x3B4252,
            surface2: 0x434C5E,
            text_primary: 0xECEFF4,
            text_secondary: 0xD8DEE9,
            border: 0x4C566A,
            status_error: 0xBF616A,
        },
        BuiltinThemeId::WarmLight => &Palette {
            accent: 0xE11D48,
            accent_background_alpha: ACCENT_ALPHA_LIGHT,
            accent_foreground: None,
            background: 0xFFFBEB,
            surface1: 0xFFFFFF,
            surface2: 0xFEF3C7,
            text_primary: 0x1C1917,
            text_secondary: 0x78716C,
            border: 0xE7E5E4,
            status_error: 0xDC2626,
        },
        BuiltinThemeId::Graphite => &Palette {
            accent: 0x4E74F8,
            accent_background_alpha: ACCENT_ALPHA_LIGHT,
            accent_foreground: None,
            background: 0x0A0C10,
            surface1: 0x121317,
            surface2: 0x23262E,
            text_primary: 0xECEEF2,
            text_secondary: 0xADB4C2,
            border: 0x23262E,
            status_error: 0xE5484D,
        },
        BuiltinThemeId::GraphiteLight => &Palette {
            accent: 0x4E74F8,
            accent_background_alpha: ACCENT_ALPHA_LIGHT,
            accent_foreground: None,
            background: 0xFCFCFD,
            surface1: 0xF9F9FB,
            surface2: 0xE7E8EC,
            text_primary: 0x1E1F24,
            text_secondary: 0x62636C,
            border: 0xE0E1E6,
            status_error: 0xE5484D,
        },
    }
}

impl BuiltinThemeId {
    /// 预设自带强调色的颜色 token（主题卡片预览用，不受用户强调色影响）。
    #[must_use]
    pub fn colors(self) -> ColorTokens {
        color_tokens(palette(self))
    }
}

/// 文本选区底色相对强调色的透明度，与注册表 `colors.textSelection` 的默认
/// `RefAlpha("colors.primary", 0.3)` 一致。
const TEXT_SELECTION_ALPHA: f32 = 0.3;

/// Layer0 → Base 语义 token 的固定映射。
fn color_tokens(palette: &Palette) -> ColorTokens {
    let accent = color(palette.accent);
    let mut tokens = ColorTokens::default();
    tokens.background = color(palette.background);
    tokens.foreground = color(palette.text_primary);
    tokens.surface = color(palette.surface1);
    tokens.surface_foreground = color(palette.text_primary);
    tokens.primary = accent;
    tokens.primary_foreground = palette
        .accent_foreground
        .map_or_else(|| foreground_for(accent), color);
    tokens.secondary = color(palette.surface2);
    tokens.secondary_foreground = color(palette.text_primary);
    tokens.muted = color(palette.surface2);
    tokens.muted_foreground = color(palette.text_secondary);
    tokens.accent = accent.alpha(f32::from(palette.accent_background_alpha) / 255.);
    tokens.accent_foreground = accent;
    tokens.destructive = color(palette.status_error);
    tokens.destructive_foreground = color(0xFFFFFF);
    tokens.border = color(palette.border);
    tokens.input = color(palette.border);
    tokens.ring = accent;
    tokens.selection = accent.alpha(TEXT_SELECTION_ALPHA);
    tokens
}

/// 用户强调色覆盖的 token 路径：`apply_accent` 重算的全部 `ColorTokens` 字段。
///
/// 文件里这些路径上的字面量即使等于基底也有意义（运行时强调色只作用于基底，显式值才能
/// 把它们固定住），所以仅差异导出永不删除它们；经 `registry.json` 的 `accentTokenPaths`
/// 导出给网站端。
pub const ACCENT_TOKEN_PATHS: [&str; 5] = [
    "colors.primary",
    "colors.primaryForeground",
    "colors.accent",
    "colors.accentForeground",
    "colors.ring",
];

/// 重算由强调色派生的 token。`primary_foreground` 仅在原值是按亮度自动
/// 推导时才跟随新强调色；预设固定的前景（Nord）保持不变。
fn apply_accent(colors: &mut ColorTokens, accent: Hsla) {
    let auto_foreground = colors.primary_foreground == foreground_for(colors.primary);
    colors.primary = accent;
    if auto_foreground {
        colors.primary_foreground = foreground_for(accent);
    }
    colors.accent = accent.alpha(colors.accent.a);
    colors.accent_foreground = accent;
    colors.ring = accent;
    colors.selection = accent.alpha(TEXT_SELECTION_ALPHA);
}

/// 强调色相对亮度 > 0.5 取近黑，否则取白。
#[must_use]
pub fn foreground_for(accent: Hsla) -> Hsla {
    if relative_luminance(accent) > 0.5 {
        color(0x09090B)
    } else {
        color(0xFFFFFF)
    }
}

/// WCAG 相对亮度。
pub(crate) fn relative_luminance(value: Hsla) -> f32 {
    fn linear(channel: f32) -> f32 {
        if channel <= 0.039_28 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }
    let rgba = Rgba::from(value);
    0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
}

fn color(value: u32) -> Hsla {
    Hsla::from(rgb(value))
}

#[cfg(test)]
mod tests {
    use gpui::{Hsla, rgb};
    use gpui_component::ThemeMode;

    use super::{ACCENT_TOKEN_PATHS, BuiltinBase, base_values};
    use crate::{BuiltinThemeId, TokenValue};

    fn color(value: u32) -> Hsla {
        Hsla::from(rgb(value))
    }

    fn value(base: BuiltinBase, mode: ThemeMode, path: &str) -> Option<TokenValue> {
        base_values(base, mode, None)
            .into_iter()
            .find(|(candidate, _)| *candidate == path)
            .map(|(_, value)| value)
    }

    #[test]
    fn single_preset_keeps_default_for_other_mode() {
        assert_eq!(
            value(BuiltinBase::Nord, ThemeMode::Dark, "colors.background"),
            Some(TokenValue::Color(color(0x2E3440)))
        );
        assert_eq!(
            value(BuiltinBase::Nord, ThemeMode::Light, "colors.background"),
            Some(TokenValue::Color(color(0xF8F9FA)))
        );
        assert_eq!(
            value(BuiltinBase::WarmLight, ThemeMode::Dark, "colors.background"),
            Some(TokenValue::Color(color(0x1C1C1E)))
        );
        assert_eq!(
            BuiltinBase::parse("builtin:midnight-blue"),
            Some(BuiltinBase::MidnightBlue)
        );
        assert_eq!(BuiltinBase::parse("builtin:midnightBlue"), None);
        assert_eq!(
            BuiltinBase::from(BuiltinThemeId::WarmLight).extends_value(),
            "builtin:warm-light"
        );
    }

    #[test]
    fn accent_preserves_pinned_preset_foreground() {
        let green = color(0x22C55E);
        let values = base_values(BuiltinBase::Nord, ThemeMode::Dark, Some(green));
        let get = |path: &str| {
            values
                .iter()
                .find(|(candidate, _)| *candidate == path)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(get("colors.primary"), Some(TokenValue::Color(green)));
        assert_eq!(
            get("colors.primaryForeground"),
            Some(TokenValue::Color(color(0x2E3440)))
        );
        assert_eq!(
            get("colors.accent"),
            Some(TokenValue::Color(green.alpha(38. / 255.)))
        );
    }

    #[test]
    fn accent_changes_exactly_the_declared_paths() {
        // 亮色强调色使自动前景翻转为近黑，默认基底上五条路径全部变化。
        let yellow = color(0xFACC15);
        for base in BuiltinBase::ALL {
            for mode in [ThemeMode::Dark, ThemeMode::Light] {
                let plain = base_values(base, mode, None);
                let accented = base_values(base, mode, Some(yellow));
                let changed: Vec<&str> = plain
                    .iter()
                    .zip(&accented)
                    .filter(|(a, b)| a != b)
                    .map(|((path, _), _)| *path)
                    .collect();
                assert!(
                    changed.iter().all(|path| ACCENT_TOKEN_PATHS.contains(path)),
                    "{base:?} {mode:?}: {changed:?}"
                );
                if base == BuiltinBase::Default {
                    assert_eq!(changed, ACCENT_TOKEN_PATHS, "{mode:?}");
                }
            }
        }
    }

    /// 预设固定值必须是合法、可被文件覆盖的 token：不钉强调色、不碰 kitBound、类型匹配。
    #[test]
    fn preset_overrides_are_valid_tokens() {
        for id in BuiltinThemeId::ALL {
            for (path, value) in super::preset_overrides(id) {
                let spec = crate::token(path).unwrap_or_else(|| panic!("{id:?} {path}"));
                assert!(!spec.kit_bound, "{id:?} {path}");
                assert!(!ACCENT_TOKEN_PATHS.contains(&path), "{id:?} {path}");
                assert!(value.fits(spec.kind), "{id:?} {path}");
            }
        }
    }

    /// 石墨的形状 / 密度经 `extends` 生效，主题文件显式值仍优先，另一模式回退默认亮色。
    #[test]
    fn graphite_preset_flows_through_extends() {
        use crate::{ThemeDocument, resolve};
        let number = |values: &crate::ResolvedModeTokens, path: &str| {
            values
                .to_flat_json()
                .get(path)
                .and_then(serde_json::Value::as_f64)
        };
        let (dark, diagnostics) = resolve(
            &ThemeDocument::builtin(BuiltinThemeId::Graphite),
            ThemeMode::Dark,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(number(&dark, "radius.md"), Some(2.));
        assert_eq!(number(&dark, "components.input.radius"), Some(2.));
        assert_eq!(number(&dark, "components.dialog.radius"), Some(4.));
        assert_eq!(number(&dark, "density.control"), Some(32.));
        assert_eq!(number(&dark, "focusRing.width"), Some(1.));

        let (document, _) = ThemeDocument::parse(
            r#"{"schemaVersion":2,"extends":"builtin:graphite","tokens":{"density":{"control":28}}}"#,
        )
        .expect("parse");
        let (custom, _) = resolve(&document, ThemeMode::Dark);
        assert_eq!(number(&custom, "density.control"), Some(28.));
        let (light, _) = resolve(&document, ThemeMode::Light);
        assert_eq!(number(&light, "density.control"), Some(28.));
        assert_eq!(number(&light, "radius.md"), Some(6.));

        let (paper, diagnostics) = resolve(
            &ThemeDocument::builtin(BuiltinThemeId::GraphiteLight),
            ThemeMode::Light,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(number(&paper, "radius.md"), Some(2.));
        assert_eq!(number(&paper, "density.control"), Some(32.));
        assert_eq!(number(&paper, "focusRing.width"), Some(1.));
    }
}
