//! 默认与内置主题经新解析管线得到的运行时值，与 v2 之前的实现逐项相等（零视觉差）。
//!
//! `legacy` 模块是旧实现的原样参考：Base token（gpui-base 默认 + 13/18 正文 + 46/255 阴影；
//! sans 字体为有意变更——由 MiSans 改为平台默认界面字体，即 gpui-base 默认 `.SystemUIFont`）、
//! 强调色重算、亮色主色对比度调整、扩展色派生、界面缩放，以及各 crate
//! 中原先硬编码的尺寸常量。

use fluxdown_ui_theme::{
    AccentScheme, BuiltinThemeId, ColorTokens, ResolveOptions, ResolvedTheme, SemanticThemeTokens,
    ThemeDocument, ThemeMode, resolve, resolve_with,
};
use gpui::{FontWeight, Hsla, Rgba, px, rgb};

mod legacy {
    use fluxdown_ui_theme::{
        ColorTokens, SemanticThemeTokens, ShadowTokens, TextStyleToken, TypographyTokens,
    };
    use gpui::{FontWeight, Hsla, Rgba, px, rgb};

    pub fn semantic_tokens(colors: ColorTokens) -> SemanticThemeTokens {
        let mut typography = TypographyTokens::default();
        typography.sm = {
            let mut style = TextStyleToken::default();
            style.size = px(13.);
            style.line_height = px(18.);
            style.weight = FontWeight::NORMAL;
            style
        };
        let mut tokens = SemanticThemeTokens::default();
        tokens.colors = colors;
        tokens.typography = typography;
        tokens.shadow = ShadowTokens::elevations(Hsla {
            h: 0.,
            s: 0.,
            l: 0.,
            a: 46.0 / 255.0,
        });
        tokens
    }

    fn luminance(value: Hsla) -> f32 {
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

    pub fn foreground_for(accent: Hsla) -> Hsla {
        if luminance(accent) > 0.5 {
            Hsla::from(rgb(0x09090B))
        } else {
            Hsla::from(rgb(0xFFFFFF))
        }
    }

    pub fn apply_accent(colors: &mut ColorTokens, accent: Hsla) {
        let auto_foreground = colors.primary_foreground == foreground_for(colors.primary);
        colors.primary = accent;
        if auto_foreground {
            colors.primary_foreground = foreground_for(accent);
        }
        colors.accent = accent.alpha(colors.accent.a);
        colors.accent_foreground = accent;
        colors.ring = accent;
        colors.selection = accent.alpha(0.3);
    }

    pub fn ensure_primary_contrast(tokens: &mut SemanticThemeTokens, dark: bool) {
        if dark {
            return;
        }
        let colors = &mut tokens.colors;
        let foreground = luminance(colors.primary_foreground);
        let mut primary = colors.primary;
        for _ in 0..40 {
            let background = luminance(primary);
            let (light, dark) = if foreground > background {
                (foreground, background)
            } else {
                (background, foreground)
            };
            if (light + 0.05) / (dark + 0.05) >= 4.5 || foreground < background {
                break;
            }
            primary.l = (primary.l - 0.01).max(0.);
        }
        colors.primary = primary;
        colors.accent_foreground = primary;
        colors.ring = primary;
        // `colors.textSelection` 引用解析后的 primary，对比度调整后随之变化。
        colors.selection = primary.alpha(0.3);
    }

    pub fn scale_tokens(tokens: &mut SemanticThemeTokens, scale: f32) {
        if scale == 1. {
            return;
        }
        let radius = &mut tokens.radius;
        radius.sm *= scale;
        radius.md *= scale;
        radius.lg *= scale;
        radius.xl *= scale;
        let spacing = &mut tokens.spacing;
        for value in [
            &mut spacing.xxs,
            &mut spacing.xs,
            &mut spacing.sm,
            &mut spacing.md,
            &mut spacing.lg,
            &mut spacing.xl,
            &mut spacing.xxl,
        ] {
            *value *= scale;
        }
        let typography = &mut tokens.typography;
        for text in [
            &mut typography.xs,
            &mut typography.sm,
            &mut typography.md,
            &mut typography.lg,
            &mut typography.xl,
            &mut typography.mono_md,
        ] {
            text.size *= scale;
            text.line_height *= scale;
        }
        let shadow = &mut tokens.shadow;
        for level in [&mut shadow.sm, &mut shadow.md, &mut shadow.lg] {
            for box_shadow in level.iter_mut() {
                box_shadow.offset.x *= scale;
                box_shadow.offset.y *= scale;
                box_shadow.blur_radius *= scale;
                box_shadow.spread_radius *= scale;
            }
        }
    }

    pub fn mix(from: Hsla, to: Hsla, amount: f32) -> Hsla {
        let a = from.to_rgb();
        let b = to.to_rgb();
        let t = amount.clamp(0., 1.);
        Hsla::from(Rgba {
            r: a.r + (b.r - a.r) * t,
            g: a.g + (b.g - a.g) * t,
            b: a.b + (b.b - a.b) * t,
            a: 1.,
        })
    }

    /// 旧 `ExtendedColors` 的 8 个派生色：success, warning, text_tertiary, hairline,
    /// row_hover, chrome, nav_hover, nav_selected。
    pub fn extended_colors(colors: &ColorTokens, dark: bool) -> [Hsla; 8] {
        let (success, warning) = if dark {
            (rgb(0x30D158), rgb(0xFFB340))
        } else {
            (rgb(0x1F9D55), rgb(0xC77C02))
        };
        let chrome = if dark {
            mix(colors.background, colors.surface, 0.35)
        } else {
            mix(colors.background, colors.muted, 0.35)
        };
        let nav_selected = if dark {
            mix(colors.muted, colors.surface, 0.15)
        } else {
            mix(colors.muted, colors.border, 0.65)
        };
        [
            success.into(),
            warning.into(),
            mix(colors.muted_foreground, colors.surface, 0.38),
            mix(colors.border, colors.surface, if dark { 0.35 } else { 0.4 }),
            mix(colors.muted, colors.surface, if dark { 0.45 } else { 0.4 }),
            chrome,
            mix(chrome, nav_selected, 0.55),
            nav_selected,
        ]
    }
}

fn color(value: u32) -> Hsla {
    Hsla::from(rgb(value))
}

fn color_with_alpha(value: u32, alpha: f32) -> Hsla {
    color(value).alpha(alpha)
}

fn runtime(id: BuiltinThemeId, accent: Hsla, scale: f32) -> ResolvedTheme {
    let options = ResolveOptions {
        accent: Some(accent),
        ensure_primary_contrast: true,
    };
    let (values, diagnostics) =
        resolve_with(&ThemeDocument::builtin(id), id.appearance(), &options);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    values.to_theme(scale)
}

fn legacy_tokens(id: BuiltinThemeId, accent: Hsla, scale: f32) -> SemanticThemeTokens {
    let mut colors = id.colors();
    legacy::apply_accent(&mut colors, accent);
    let mut tokens = legacy::semantic_tokens(colors);
    legacy::scale_tokens(&mut tokens, scale);
    legacy::ensure_primary_contrast(&mut tokens, id.appearance().is_dark());
    tokens
}

/// 旧版运行时只有调色板预设；石墨等带整套形状 / 密度的预设不在对比范围内。
#[test]
fn builtin_runtime_tokens_equal_legacy_for_every_preset_accent_and_scale() {
    for id in BuiltinThemeId::ALL
        .into_iter()
        .filter(|id| !matches!(id, BuiltinThemeId::Graphite | BuiltinThemeId::GraphiteLight))
    {
        for scheme in AccentScheme::ALL {
            let accent = scheme.color(0xFFFA_FAFA);
            for scale in [0.8, 1., 1.5] {
                let theme = runtime(id, accent, scale);
                let legacy = legacy_tokens(id, accent, scale);
                assert_eq!(theme.base, legacy, "{id:?} {scheme:?} {scale}");

                let dark = id.appearance().is_dark();
                let [
                    success,
                    warning,
                    tertiary,
                    hairline,
                    row_hover,
                    chrome,
                    nav_hover,
                    nav_selected,
                ] = legacy::extended_colors(&legacy.colors, dark);
                let ext = theme.extended.colors;
                assert_eq!(
                    [
                        ext.success,
                        ext.warning,
                        ext.text_tertiary,
                        ext.hairline,
                        ext.row_hover,
                        ext.chrome,
                        ext.nav_hover,
                        ext.nav_selected
                    ],
                    [
                        success,
                        warning,
                        tertiary,
                        hairline,
                        row_hover,
                        chrome,
                        nav_hover,
                        nav_selected
                    ],
                    "{id:?} {scheme:?}"
                );

                // downloads/src/components/task_table.rs `progress_bar_color` / `status_color` /
                // `progress_track_color`。
                let colors = legacy.colors;
                assert_eq!(ext.status_downloading, colors.primary);
                assert_eq!(ext.status_failed, colors.destructive);
                assert_eq!(ext.status_paused, colors.muted_foreground);
                assert_eq!(ext.status_completed, tertiary);
                assert_eq!(ext.status_queued, tertiary);
                assert_eq!(ext.progress_fill, colors.primary);
                assert_eq!(ext.progress_track, colors.muted_foreground.opacity(0.16));

                let caption = theme.extended.caption;
                assert_eq!(
                    (caption.size, caption.line_height),
                    (px(11. * scale), px(14. * scale))
                );
                assert_eq!(caption.weight, FontWeight::NORMAL);
                let title = theme.extended.title;
                assert_eq!(
                    (title.size, title.line_height),
                    (px(15. * scale), px(20. * scale))
                );
                assert_eq!(title.weight, FontWeight::SEMIBOLD);
                let icon = theme.extended.icon;
                assert_eq!(
                    (icon.sm, icon.md, icon.lg),
                    (px(12. * scale), px(14. * scale), px(16. * scale))
                );
            }
        }
    }
}

/// 尺寸 token 默认值等于各 crate 原先的常量（缩放 1 时）。
#[test]
fn size_tokens_equal_legacy_constants() {
    let theme = runtime(BuiltinThemeId::DefaultLight, color(0x3B82F6), 1.);
    let density = theme.density;
    assert_eq!(density.control, fluxdown_ui_theme::CONTROL_HEIGHT);
    assert_eq!(density.nav_row, px(28.));
    assert_eq!(density.toolbar_button, px(28.));
    assert_eq!(density.title_bar, px(40.));
    assert_eq!(density.status_bar, px(28.));
    assert_eq!(density.status_control, px(22.));
    assert_eq!(density.section_header, px(24.));
    assert_eq!(density.task_row, px(44.));
    assert_eq!(density.task_row_compact, px(30.));
    assert_eq!(density.check_mark, px(16.));

    assert_eq!(theme.extended.stroke.thin, px(1.));
    assert_eq!(theme.extended.stroke.strong, px(2.));
    assert_eq!(theme.extended.focus_ring.width, px(2.));
    assert_eq!(theme.extended.focus_ring.offset, px(1.));

    let radius = theme.base.radius;
    let components = theme.components;
    assert_eq!(components.button_radius, radius.md);
    assert_eq!(components.input_radius, radius.md);
    assert_eq!(components.dialog_radius, radius.lg);
    assert_eq!(components.card_radius, radius.lg);
    assert_eq!(components.badge_radius, radius.full);
    assert_eq!(components.nav_item_radius, radius.md);
    assert_eq!(components.progress_radius, radius.full);
    assert_eq!(components.progress_height, px(4.));
    assert_eq!(components.task_row_radius, radius.md);
    assert_eq!(components.checkbox_radius, radius.sm + px(1.));
    assert_eq!(components.tab_radius, radius.md);
    assert_eq!(components.menu_radius, radius.md);
    assert_eq!(components.tooltip_radius, radius.md);
}

#[test]
fn scaling_keeps_title_bar_strokes_and_full_radius() {
    let theme = runtime(BuiltinThemeId::DefaultDark, color(0x3B82F6), 1.5);
    assert_eq!(theme.density.title_bar, px(40.));
    assert_eq!(theme.density.nav_row, px(42.));
    assert_eq!(theme.extended.stroke.thin, px(1.));
    assert_eq!(theme.extended.focus_ring.width, px(2.));
    assert_eq!(theme.base.radius.full, px(9999.));
    assert_eq!(theme.components.progress_radius, px(9999.));
    assert_eq!(theme.components.checkbox_radius, px(6.));
    assert_eq!(theme.components.progress_height, px(6.));
}

fn default_colors(mode: ThemeMode) -> ColorTokens {
    resolve(&ThemeDocument::fluxdown_default(), mode)
        .0
        .to_theme(1.)
        .base
        .colors
}

#[test]
fn default_light_colors_match_flutter_tokens() {
    assert_eq!(default_colors(ThemeMode::Light), {
        let mut expected = ColorTokens::default();
        expected.background = color(0xF8F9FA);
        expected.foreground = color(0x09090B);
        expected.surface = color(0xFFFFFF);
        expected.surface_foreground = color(0x09090B);
        expected.primary = color(0x3B82F6);
        expected.primary_foreground = color(0xFFFFFF);
        expected.secondary = color(0xF1F3F5);
        expected.secondary_foreground = color(0x09090B);
        expected.muted = color(0xF1F3F5);
        expected.muted_foreground = color(0x71717A);
        expected.accent = color_with_alpha(0x3B82F6, 26.0 / 255.0);
        expected.accent_foreground = color(0x3B82F6);
        expected.destructive = color(0xEF4444);
        expected.destructive_foreground = color(0xFFFFFF);
        expected.border = color(0xE4E4E7);
        expected.input = color(0xE4E4E7);
        expected.ring = color(0x3B82F6);
        expected.selection = color_with_alpha(0x3B82F6, 0.3);
        expected
    });
}

#[test]
fn default_dark_colors_match_flutter_tokens() {
    assert_eq!(default_colors(ThemeMode::Dark), {
        let mut expected = ColorTokens::default();
        expected.background = color(0x1C1C1E);
        expected.foreground = color(0xF5F5F7);
        expected.surface = color(0x2C2C2E);
        expected.surface_foreground = color(0xF5F5F7);
        expected.primary = color(0x3B82F6);
        expected.primary_foreground = color(0xFFFFFF);
        expected.secondary = color(0x3A3A3C);
        expected.secondary_foreground = color(0xF5F5F7);
        expected.muted = color(0x3A3A3C);
        expected.muted_foreground = color(0xA1A1A6);
        expected.accent = color_with_alpha(0x3B82F6, 46.0 / 255.0);
        expected.accent_foreground = color(0x3B82F6);
        expected.destructive = color(0xEF4444);
        expected.destructive_foreground = color(0xFFFFFF);
        expected.border = color(0x48484A);
        expected.input = color(0x48484A);
        expected.ring = color(0x3B82F6);
        expected.selection = color_with_alpha(0x3B82F6, 0.3);
        expected
    });
}

#[test]
fn builtin_presets_map_flutter_layer0_colors() {
    let colors = |id: BuiltinThemeId, mode: ThemeMode| {
        resolve(&ThemeDocument::builtin(id), mode)
            .0
            .to_theme(1.)
            .base
            .colors
    };
    let nord = colors(BuiltinThemeId::Nord, ThemeMode::Dark);
    assert_eq!(nord.background, color(0x2E3440));
    assert_eq!(nord.surface, color(0x3B4252));
    assert_eq!(nord.muted, color(0x434C5E));
    assert_eq!(nord.muted_foreground, color(0xD8DEE9));
    assert_eq!(nord.primary, color(0x88C0D0));
    assert_eq!(nord.primary_foreground, color(0x2E3440));
    assert_eq!(nord.accent, color_with_alpha(0x88C0D0, 38.0 / 255.0));
    assert_eq!(nord.destructive, color(0xBF616A));
    assert_eq!(nord.border, color(0x4C566A));
    assert_eq!(
        colors(BuiltinThemeId::Nord, ThemeMode::Light),
        default_colors(ThemeMode::Light)
    );

    let warm = colors(BuiltinThemeId::WarmLight, ThemeMode::Light);
    assert_eq!(warm.background, color(0xFFFBEB));
    assert_eq!(warm.primary, color(0xE11D48));
    assert_eq!(warm.primary_foreground, color(0xFFFFFF));
    assert_eq!(warm.accent, color_with_alpha(0xE11D48, 26.0 / 255.0));
    assert_eq!(warm.destructive, color(0xDC2626));

    let midnight = colors(BuiltinThemeId::MidnightBlue, ThemeMode::Dark);
    assert_eq!(midnight.background, color(0x0F172A));
    assert_eq!(midnight.ring, color(0x60A5FA));
}

#[test]
fn light_primary_reaches_aa_contrast_and_dark_is_untouched() {
    let luminance = |value: Hsla| {
        let rgba = Rgba::from(value);
        let linear = |c: f32| {
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
    };
    let blue = color(0x3B82F6);
    let light = runtime(BuiltinThemeId::DefaultLight, blue, 1.).base.colors;
    let (fg, bg) = (
        luminance(light.primary_foreground),
        luminance(light.primary),
    );
    assert!((fg + 0.05) / (bg + 0.05) >= 4.5);
    assert!(light.primary.l < blue.l);
    assert_eq!(light.accent_foreground, light.primary);
    assert_eq!(light.ring, light.primary);

    let dark = runtime(BuiltinThemeId::DefaultDark, blue, 1.).base.colors;
    assert_eq!(dark.primary, blue);
}
