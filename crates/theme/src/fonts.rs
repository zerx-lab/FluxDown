use gpui::{App, FontWeight, Pixels, SharedString, px};

use crate::ResolvedTheme;

/// 设备本地偏好，不进入主题文档或云同步目录。
pub const FONT_FAMILY_KEY: &str = "desktop.font_family";

/// 设备本地偏好：正文字号（整数 px，界面缩放前，如 `14`）；缺失时沿用主题正文字号。
/// 不进入主题文档或云同步目录。
pub const FONT_SIZE_KEY: &str = "desktop.font_size";

/// 设备本地的正文字重，不进入主题文档或云同步目录。
pub const FONT_WEIGHT_KEY: &str = "desktop.font_weight";

/// 可接受的正文字号范围（px）。
pub const FONT_SIZE_RANGE: std::ops::RangeInclusive<u16> = 10..=24;

/// 设置页提供的正文字号档位（px）。
pub const FONT_SIZES: [u16; 10] = [11, 12, 13, 14, 15, 16, 17, 18, 20, 22];

/// GPUI 当前平台字体后端可用的字体族；不推测或硬编码系统字体。
#[must_use]
pub fn available_font_families(cx: &App) -> Vec<String> {
    normalize_names(cx.text_system().all_font_names())
}

fn normalize_names(mut names: Vec<String>) -> Vec<String> {
    names.retain(|name| !name.trim().is_empty());
    names.sort_unstable();
    names.dedup();
    names
}

pub(crate) fn apply_font_family(
    theme: &mut crate::ResolvedTheme,
    selected: Option<&str>,
    available: &[String],
) {
    if let Some(name) = selected.filter(|name| available.iter().any(|item| item == name)) {
        theme.base.typography.sans = name.to_owned().into();
    }
}

pub(crate) fn apply_font_weight(theme: &mut ResolvedTheme, weight: Option<u16>) {
    if let Some(weight) = weight {
        theme.base.typography.sm.weight = FontWeight(f32::from(weight));
    }
}

/// 字体的自然行高（ascent + |descent|）与字号之比，取平台实际解析到的字体（缺失时为回退字体）。
///
/// GPUI 按行盒居中基线：行高小于该值时字形会伸出行盒，被 `truncate` 等 `overflow_hidden`
/// 容器裁掉上伸 / 下伸笔画。
pub(crate) fn natural_line_ratio(family: &SharedString, weight: FontWeight, cx: &App) -> f32 {
    let text_system = cx.text_system();
    let mut font = gpui::font(family.clone());
    font.weight = weight;
    let font_id = text_system.resolve_font(&font);
    let size = px(100.);
    let ascent = text_system.ascent(font_id, size).as_f32();
    let descent = text_system.descent(font_id, size).as_f32().abs();
    (ascent + descent) / size.as_f32()
}

/// 文字适配：按字体大小倍率缩放全部文字角色（字号与行高），再把行高抬到不小于
/// `line_ratio × 字号`（取整到整像素），最后把承载文字的控件 / 行高度按对应行高的增量加高，
/// 使任意字号与字体下文字都不被行盒或定高容器裁切。
///
/// 控件高度只增不减：字号调小时保持原高度，避免图标与点击区域缩水。图标、间距、圆角、
/// 线宽与复选框不随文字缩放；仅含图标的 `toolbar_button` 与不随缩放的 `title_bar` 保持不变。
pub(crate) fn fit_text(theme: &mut ResolvedTheme, scale: f32, line_ratio: f32) {
    let line_ratio = if line_ratio.is_finite() {
        line_ratio
    } else {
        0.
    };
    let before_sm = theme.base.typography.sm.line_height;
    let before_xs = theme.base.typography.xs.line_height;
    let before_caption = theme.extended.caption.line_height;

    let typography = &mut theme.base.typography;
    for style in [
        &mut typography.xs,
        &mut typography.sm,
        &mut typography.md,
        &mut typography.lg,
        &mut typography.xl,
        &mut typography.mono_md,
        &mut theme.extended.caption,
        &mut theme.extended.title,
    ] {
        style.size *= scale;
        let natural = px((style.size.as_f32() * line_ratio).ceil());
        style.line_height = (style.line_height * scale).max(natural);
    }

    let growth = |after: Pixels, before: Pixels| (after - before).max(px(0.));
    let body = growth(theme.base.typography.sm.line_height, before_sm);
    let small = growth(theme.extended.caption.line_height, before_caption)
        .max(growth(theme.base.typography.xs.line_height, before_xs));
    let density = &mut theme.density;
    density.control += body;
    density.nav_row += body;
    density.status_bar += body;
    density.status_control += body;
    density.section_header += body;
    density.task_row_compact += body;
    // 舒适任务行是正文 + 小号说明两行。
    density.task_row += body + small;
}

#[cfg(test)]
mod tests {
    use super::{
        FONT_FAMILY_KEY, FONT_SIZE_KEY, FONT_WEIGHT_KEY, apply_font_family, apply_font_weight,
        fit_text, normalize_names,
    };
    use crate::{AppearancePreferences, BuiltinThemeId, ThemeDocument, ThemeMode, resolve};
    use gpui::px;
    use serde_json::json;

    #[test]
    fn font_names_are_real_sorted_unique_and_nonempty() {
        assert_eq!(
            normalize_names(vec![
                "中文字体".into(),
                "".into(),
                "  ".into(),
                "A".into(),
                "A".into()
            ]),
            vec!["A", "中文字体"]
        );
    }

    #[test]
    fn font_preference_defaults_and_invalid_values_follow_theme() {
        for value in [json!(null), json!(42), json!(""), json!("  ")] {
            let prefs =
                AppearancePreferences::from_values(&[(FONT_FAMILY_KEY.into(), value)].into());
            assert_eq!(prefs.font_family, None);
        }
        let prefs = AppearancePreferences::from_values(
            &[(FONT_FAMILY_KEY.into(), json!("中文字体"))].into(),
        );
        assert_eq!(prefs.font_family.as_deref(), Some("中文字体"));
        assert!(prefs.same_palette(&AppearancePreferences::default()));
        assert_ne!(prefs, AppearancePreferences::default());
    }

    #[test]
    fn font_override_survives_theme_modes_without_changing_mono_or_documents() {
        let document = ThemeDocument::builtin(BuiltinThemeId::DefaultDark);
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let (values, _) = resolve(&document, mode);
            let original = values.to_theme(1.);
            for selected in [None, Some("missing")] {
                let mut theme = original.clone();
                apply_font_family(&mut theme, selected, &["Installed".into()]);
                assert_eq!(theme, original);
            }
            let mut theme = original.clone();
            apply_font_family(&mut theme, Some("Installed"), &["Installed".into()]);
            assert_eq!(theme.base.typography.sans.as_ref(), "Installed");
            assert_eq!(theme.base.typography.mono, original.base.typography.mono);
            assert_eq!(values.to_theme(1.), original);
        }
    }

    fn default_theme(scale: f32) -> crate::ResolvedTheme {
        let (values, _) = resolve(
            &ThemeDocument::builtin(BuiltinThemeId::DefaultLight),
            ThemeMode::Light,
        );
        values.to_theme(scale)
    }

    #[test]
    fn font_weight_changes_only_body_weight_and_invalid_values_leave_theme_intact() {
        let apply = |value| {
            let prefs =
                AppearancePreferences::from_values(&[(FONT_WEIGHT_KEY.into(), value)].into());
            let mut theme = default_theme(1.);
            apply_font_weight(&mut theme, prefs.font_weight);
            theme
        };
        let original = default_theme(1.);
        let mut expected = original.clone();
        expected.base.typography.sm.weight = gpui::FontWeight(600.);
        assert_ne!(expected, original);
        assert_eq!(apply(json!(600)), expected);
        assert_eq!(apply(json!("600")), expected);
        for invalid in [json!(null), json!(99), json!(650), json!(1000), json!(600.5)] {
            assert_eq!(apply(invalid), original);
        }
    }

    #[test]
    fn font_size_preference_is_px_relative_to_theme_body() {
        let parse = |value| {
            AppearancePreferences::from_values(&[(FONT_SIZE_KEY.into(), value)].into()).font_size
        };
        assert_eq!(parse(json!(14)), Some(14));
        assert_eq!(parse(json!("16")), Some(16));
        for invalid in [json!(9), json!(40), json!("big"), json!(null)] {
            assert_eq!(parse(invalid), None);
        }
        // 超出范围的值视为未设置，回到主题字号。
        let prefs = AppearancePreferences::from_values(&[(FONT_SIZE_KEY.into(), json!(26))].into());
        assert_eq!(prefs.font_scale(13.), 1.);
        let prefs = AppearancePreferences::from_values(&[(FONT_SIZE_KEY.into(), json!(19))].into());
        assert!(prefs.same_palette(&AppearancePreferences::default()));
        assert!((prefs.font_scale(13.) - 19. / 13.).abs() < f32::EPSILON);
        assert_eq!(AppearancePreferences::default().font_scale(13.), 1.);
    }

    #[test]
    fn default_font_and_size_leave_theme_untouched() {
        let original = default_theme(1.);
        let mut theme = original.clone();
        // 系统字体的自然行高约 1.2em，低于 13/18 正文基线。
        fit_text(&mut theme, 1., 1.2);
        assert_eq!(theme, original);
    }

    #[test]
    fn larger_text_scales_type_and_grows_text_rows_only() {
        let original = default_theme(1.);
        let mut theme = original.clone();
        fit_text(&mut theme, 1.5, 1.2);
        let typography = &theme.base.typography;
        assert_eq!(typography.sm.size, px(19.5));
        assert_eq!(typography.sm.line_height, px(27.));
        assert_eq!(theme.extended.caption.line_height, px(21.));
        assert_eq!(theme.density.control, px(37.));
        assert_eq!(theme.density.nav_row, px(37.));
        // 正文行高 +9，小号行高取 caption(+7) 与 xs(16→24，+8) 的较大增量。
        assert_eq!(theme.density.task_row, px(44. + 9. + 8.));
        assert_eq!(theme.density.title_bar, original.density.title_bar);
        assert_eq!(
            theme.density.toolbar_button,
            original.density.toolbar_button
        );
        assert_eq!(theme.extended.icon, original.extended.icon);
        assert_eq!(theme.base.spacing, original.base.spacing);
    }

    #[test]
    fn tall_font_metrics_raise_line_height_and_rows() {
        let mut theme = default_theme(1.);
        // 自然行高 1.6em：13px 正文需要 ceil(20.8) = 21px 行高。
        fit_text(&mut theme, 1., 1.6);
        assert_eq!(theme.base.typography.sm.size, px(13.));
        assert_eq!(theme.base.typography.sm.line_height, px(21.));
        assert_eq!(theme.density.nav_row, px(31.));
    }

    #[test]
    fn smaller_text_keeps_control_heights() {
        let original = default_theme(1.2);
        let mut theme = original.clone();
        fit_text(&mut theme, 0.8, 1.2);
        assert!(theme.base.typography.sm.size < original.base.typography.sm.size);
        assert_eq!(theme.density, original.density);
    }
}
