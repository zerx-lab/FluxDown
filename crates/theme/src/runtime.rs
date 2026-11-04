//! 运行时 token 快照：注册表路径 → 类型化结构的映射与界面缩放。

use gpui::{BoxShadow, FontWeight, Hsla, Pixels, SharedString, point, px};
use gpui_base::{SemanticThemeTokens, TextStyleToken, TypographyTokens};

use crate::builtin::{MONO_FONT_SENTINEL, SANS_FONT_SENTINEL};
use crate::{
    ExtendedColors, ExtendedTokens, FocusRingTokens, IconSizes, ResolvedModeTokens, ShadowValue,
    StrokeTokens, TOKENS, TokenValue,
};

/// 不小于此值的圆角表示「尽可能圆」（胶囊 / 圆形），不随界面缩放。
pub const RADIUS_FULL_THRESHOLD: f32 = 1000.;

/// 控件高度阶梯（px，已按界面缩放；`title_bar` 除外）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DensityTokens {
    /// 按钮 / 输入框 / 下拉 / 数字输入统一高度。
    pub control: Pixels,
    /// 侧栏导航行高。
    pub nav_row: Pixels,
    /// 工具栏 / 顶栏图标按钮边长。
    pub toolbar_button: Pixels,
    /// shell 自绘标题栏高度：交通灯按它垂直居中，故不随界面缩放。
    pub title_bar: Pixels,
    /// 状态栏高度。
    pub status_bar: Pixels,
    /// 状态栏内按钮高度。
    pub status_control: Pixels,
    /// 侧栏分区标题行高。
    pub section_header: Pixels,
    /// 任务行高（舒适双行）。
    pub task_row: Pixels,
    /// 任务行高（紧凑单行）。
    pub task_row_compact: Pixels,
    /// 复选框边长。
    pub check_mark: Pixels,
}

/// 组件级 token（px，已按界面缩放）。`input` / `dialog` / `menu` / `tooltip` 圆角由
/// gpui-component 库控件直接读取 `radius.md` / `radius.lg`，此处仅供自绘元素对齐。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ComponentTokens {
    pub button_radius: Pixels,
    pub input_radius: Pixels,
    pub dialog_radius: Pixels,
    pub card_radius: Pixels,
    pub badge_radius: Pixels,
    pub nav_item_radius: Pixels,
    pub progress_radius: Pixels,
    pub progress_height: Pixels,
    pub task_row_radius: Pixels,
    pub checkbox_radius: Pixels,
    pub tab_radius: Pixels,
    pub menu_radius: Pixels,
    pub tooltip_radius: Pixels,
}

/// 单个模式完整解析后的运行时主题（已按界面缩放）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTheme {
    /// gpui-base Base 语义 token。
    pub base: SemanticThemeTokens,
    pub extended: ExtendedTokens,
    pub density: DensityTokens,
    pub components: ComponentTokens,
}

impl ResolvedTheme {
    /// 已解析的扁平 token → 类型化快照，并按 `scale` 缩放 `scales` 标记的尺寸
    /// （「全圆」圆角除外）。
    #[must_use]
    pub fn from_tokens(tokens: &ResolvedModeTokens, scale: f32) -> Self {
        let mut text = TextStyleToken::default();
        text.size = px(0.);
        text.line_height = px(0.);
        text.weight = FontWeight::NORMAL;
        let mut theme = Self {
            base: SemanticThemeTokens::default(),
            extended: ExtendedTokens {
                colors: ExtendedColors::default(),
                caption: text,
                title: text,
                icon: IconSizes::default(),
                stroke: StrokeTokens::default(),
                focus_ring: FocusRingTokens::default(),
            },
            density: DensityTokens::default(),
            components: ComponentTokens::default(),
        };
        for spec in TOKENS {
            let (Some(value), Some(slot)) = (tokens.get(spec.path), theme.slot(spec.path)) else {
                continue;
            };
            let value = if spec.scales && scale != 1. {
                scaled(value, scale)
            } else {
                value.clone()
            };
            slot.set(&value);
        }
        // Base `selection` 不是独立注册表 token：它与扩展 `colors.textSelection`
        // 同义，统一由后者投影，保证 Base 选区与 gpui-component 选区同色。
        theme.base.colors.selection = theme.extended.colors.text_selection;
        if theme.base.typography.sans.as_ref() == SANS_FONT_SENTINEL {
            theme.base.typography.sans = TypographyTokens::default().sans;
        }
        if theme.base.typography.mono.as_ref() == MONO_FONT_SENTINEL {
            theme.base.typography.mono = TypographyTokens::default().mono;
        }
        theme
    }

    /// 按注册表顺序列出每个 token 的运行时值（已按界面缩放，字体为实际字体名）。
    #[must_use]
    pub fn token_values(&self) -> Vec<(&'static str, TokenValue)> {
        let mut theme = self.clone();
        TOKENS
            .iter()
            .filter_map(|spec| theme.slot(spec.path).map(|slot| (spec.path, slot.get())))
            .collect()
    }

    fn slot(&mut self, path: &str) -> Option<Slot<'_>> {
        if let Some(slot) = semantic_slot(&mut self.base, path) {
            return Some(slot);
        }
        let extended = &mut self.extended;
        let colors = &mut extended.colors;
        let density = &mut self.density;
        let components = &mut self.components;
        Some(match path {
            "colors.success" => Slot::Color(&mut colors.success),
            "colors.warning" => Slot::Color(&mut colors.warning),
            "colors.info" => Slot::Color(&mut colors.info),
            "colors.textTertiary" => Slot::Color(&mut colors.text_tertiary),
            "colors.hairline" => Slot::Color(&mut colors.hairline),
            "colors.rowHover" => Slot::Color(&mut colors.row_hover),
            "colors.chrome" => Slot::Color(&mut colors.chrome),
            "colors.navHover" => Slot::Color(&mut colors.nav_hover),
            "colors.navSelected" => Slot::Color(&mut colors.nav_selected),
            "colors.navSelectedForeground" => Slot::Color(&mut colors.nav_selected_foreground),
            "colors.navSelectedIcon" => Slot::Color(&mut colors.nav_selected_icon),
            "colors.accentText" => Slot::Color(&mut colors.accent_text),
            "colors.caret" => Slot::Color(&mut colors.caret),
            "colors.textSelection" => Slot::Color(&mut colors.text_selection),
            "colors.dragBorder" => Slot::Color(&mut colors.drag_border),
            "colors.dropTarget" => Slot::Color(&mut colors.drop_target),
            "colors.statusDownloading" => Slot::Color(&mut colors.status_downloading),
            "colors.statusCompleted" => Slot::Color(&mut colors.status_completed),
            "colors.statusFailed" => Slot::Color(&mut colors.status_failed),
            "colors.statusPaused" => Slot::Color(&mut colors.status_paused),
            "colors.statusQueued" => Slot::Color(&mut colors.status_queued),
            "colors.progressTrack" => Slot::Color(&mut colors.progress_track),
            "colors.progressFill" => Slot::Color(&mut colors.progress_fill),
            "typography.caption.size" => Slot::Px(&mut extended.caption.size),
            "typography.caption.lineHeight" => Slot::Px(&mut extended.caption.line_height),
            "typography.caption.weight" => Slot::Weight(&mut extended.caption.weight),
            "typography.title.size" => Slot::Px(&mut extended.title.size),
            "typography.title.lineHeight" => Slot::Px(&mut extended.title.line_height),
            "typography.title.weight" => Slot::Weight(&mut extended.title.weight),
            "stroke.thin" => Slot::Px(&mut extended.stroke.thin),
            "stroke.strong" => Slot::Px(&mut extended.stroke.strong),
            "focusRing.width" => Slot::Px(&mut extended.focus_ring.width),
            "focusRing.offset" => Slot::Px(&mut extended.focus_ring.offset),
            "icon.sm" => Slot::Px(&mut extended.icon.sm),
            "icon.md" => Slot::Px(&mut extended.icon.md),
            "icon.lg" => Slot::Px(&mut extended.icon.lg),
            "density.control" => Slot::Px(&mut density.control),
            "density.navRow" => Slot::Px(&mut density.nav_row),
            "density.toolbarButton" => Slot::Px(&mut density.toolbar_button),
            "density.titleBar" => Slot::Px(&mut density.title_bar),
            "density.statusBar" => Slot::Px(&mut density.status_bar),
            "density.statusControl" => Slot::Px(&mut density.status_control),
            "density.sectionHeader" => Slot::Px(&mut density.section_header),
            "density.taskRow" => Slot::Px(&mut density.task_row),
            "density.taskRowCompact" => Slot::Px(&mut density.task_row_compact),
            "density.checkMark" => Slot::Px(&mut density.check_mark),
            "components.button.radius" => Slot::Px(&mut components.button_radius),
            "components.input.radius" => Slot::Px(&mut components.input_radius),
            "components.dialog.radius" => Slot::Px(&mut components.dialog_radius),
            "components.card.radius" => Slot::Px(&mut components.card_radius),
            "components.badge.radius" => Slot::Px(&mut components.badge_radius),
            "components.navItem.radius" => Slot::Px(&mut components.nav_item_radius),
            "components.progress.radius" => Slot::Px(&mut components.progress_radius),
            "components.progress.height" => Slot::Px(&mut components.progress_height),
            "components.taskRow.radius" => Slot::Px(&mut components.task_row_radius),
            "components.checkbox.radius" => Slot::Px(&mut components.checkbox_radius),
            "components.tab.radius" => Slot::Px(&mut components.tab_radius),
            "components.menu.radius" => Slot::Px(&mut components.menu_radius),
            "components.tooltip.radius" => Slot::Px(&mut components.tooltip_radius),
            _ => return None,
        })
    }
}

/// 缩放单个尺寸值；「全圆」圆角保持不变。
fn scaled(value: &TokenValue, scale: f32) -> TokenValue {
    match value {
        TokenValue::Number(number) if *number < RADIUS_FULL_THRESHOLD => {
            TokenValue::Number(number * scale)
        }
        TokenValue::Shadow(layers) => TokenValue::Shadow(
            layers
                .iter()
                .map(|layer| ShadowValue {
                    x: layer.x * scale,
                    y: layer.y * scale,
                    blur: layer.blur * scale,
                    spread: layer.spread * scale,
                    ..*layer
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// 类型化结构中某个 token 字段的可变引用。
pub(crate) enum Slot<'a> {
    Color(&'a mut Hsla),
    Px(&'a mut Pixels),
    Weight(&'a mut FontWeight),
    Font(&'a mut SharedString),
    Shadow(&'a mut Vec<BoxShadow>),
}

impl Slot<'_> {
    pub(crate) fn get(&self) -> TokenValue {
        match self {
            Self::Color(color) => TokenValue::Color(**color),
            Self::Px(pixels) => TokenValue::Number(pixels.as_f32()),
            Self::Weight(weight) => TokenValue::Number(weight.0),
            Self::Font(font) => TokenValue::Font(font.to_string()),
            Self::Shadow(layers) => TokenValue::Shadow(
                layers
                    .iter()
                    .map(|layer| ShadowValue {
                        x: layer.offset.x.as_f32(),
                        y: layer.offset.y.as_f32(),
                        blur: layer.blur_radius.as_f32(),
                        spread: layer.spread_radius.as_f32(),
                        color: layer.color,
                        inset: layer.inset,
                    })
                    .collect(),
            ),
        }
    }

    /// 类型不符时不写入（解析器保证类型一致）。
    pub(crate) fn set(self, value: &TokenValue) {
        match (self, value) {
            (Self::Color(slot), TokenValue::Color(color)) => *slot = *color,
            (Self::Px(slot), TokenValue::Number(number)) => *slot = px(*number),
            (Self::Weight(slot), TokenValue::Number(number)) => *slot = FontWeight(*number),
            (Self::Font(slot), TokenValue::Font(font)) => *slot = font.clone().into(),
            (Self::Shadow(slot), TokenValue::Shadow(layers)) => {
                *slot = layers
                    .iter()
                    .map(|layer| BoxShadow {
                        color: layer.color,
                        offset: point(px(layer.x), px(layer.y)),
                        blur_radius: px(layer.blur),
                        spread_radius: px(layer.spread),
                        inset: layer.inset,
                    })
                    .collect();
            }
            _ => {}
        }
    }
}

/// gpui-base Base token 的字段访问（注册表路径）。
pub(crate) fn semantic_slot<'a>(
    tokens: &'a mut SemanticThemeTokens,
    path: &str,
) -> Option<Slot<'a>> {
    let (group, key) = path.split_once('.')?;
    match group {
        "colors" => {
            let colors = &mut tokens.colors;
            Some(Slot::Color(match key {
                "background" => &mut colors.background,
                "foreground" => &mut colors.foreground,
                "surface" => &mut colors.surface,
                "surfaceForeground" => &mut colors.surface_foreground,
                "primary" => &mut colors.primary,
                "primaryForeground" => &mut colors.primary_foreground,
                "secondary" => &mut colors.secondary,
                "secondaryForeground" => &mut colors.secondary_foreground,
                "muted" => &mut colors.muted,
                "mutedForeground" => &mut colors.muted_foreground,
                "accent" => &mut colors.accent,
                "accentForeground" => &mut colors.accent_foreground,
                "destructive" => &mut colors.destructive,
                "destructiveForeground" => &mut colors.destructive_foreground,
                "border" => &mut colors.border,
                "input" => &mut colors.input,
                "ring" => &mut colors.ring,
                _ => return None,
            }))
        }
        "radius" => {
            let radius = &mut tokens.radius;
            Some(Slot::Px(match key {
                "none" => &mut radius.none,
                "sm" => &mut radius.sm,
                "md" => &mut radius.md,
                "lg" => &mut radius.lg,
                "xl" => &mut radius.xl,
                "full" => &mut radius.full,
                _ => return None,
            }))
        }
        "spacing" => {
            let spacing = &mut tokens.spacing;
            Some(Slot::Px(match key {
                "xxs" => &mut spacing.xxs,
                "xs" => &mut spacing.xs,
                "sm" => &mut spacing.sm,
                "md" => &mut spacing.md,
                "lg" => &mut spacing.lg,
                "xl" => &mut spacing.xl,
                "xxl" => &mut spacing.xxl,
                _ => return None,
            }))
        }
        "shadow" => {
            let shadow = &mut tokens.shadow;
            Some(Slot::Shadow(match key {
                "sm" => &mut shadow.sm,
                "md" => &mut shadow.md,
                "lg" => &mut shadow.lg,
                _ => return None,
            }))
        }
        "typography" => {
            let typography = &mut tokens.typography;
            match key {
                "sans" => return Some(Slot::Font(&mut typography.sans)),
                "mono" => return Some(Slot::Font(&mut typography.mono)),
                _ => {}
            }
            let (role, prop) = key.split_once('.')?;
            let text = match role {
                "xs" => &mut typography.xs,
                "sm" => &mut typography.sm,
                "md" => &mut typography.md,
                "lg" => &mut typography.lg,
                "xl" => &mut typography.xl,
                "monoMd" => &mut typography.mono_md,
                _ => return None,
            };
            match prop {
                "size" => Some(Slot::Px(&mut text.size)),
                "lineHeight" => Some(Slot::Px(&mut text.line_height)),
                "weight" => Some(Slot::Weight(&mut text.weight)),
                _ => None,
            }
        }
        _ => None,
    }
}
