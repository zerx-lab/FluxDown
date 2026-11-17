//! 外观：语言、明暗模式、主题（内置 + 已导入，含导入 / 导出 / 删除）、强调色、界面缩放、字体与字体大小。
//!
//! 控件只写偏好（走 `agent.preferences.patch`），不直接改主题 / 语言：app 观察设置存储的偏好
//! 视图（含未回执的本地编辑），经 `fluxdown_ui_theme::apply_appearance_preferences` 与语言切换
//! 统一投影。直接改内存状态会在存储只读（未连接）时与偏好脱节，并被下一次快照回弹。

use fluxdown_ui_components::toast;
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::{
    AccentScheme, AppearancePreferences, BuiltinThemeId, COLOR_SCHEME_KEY, CUSTOM_COLOR_KEY,
    ColorTokens, DARK_THEME_KEY, ExportMode, ExtendedTokens, FONT_SIZE_KEY, FONT_SIZE_RANGE,
    FONT_SIZES, LIGHT_THEME_KEY, THEME_MODE_KEY, ThemeMode, ThemePreference, ThemeSelection,
    UI_SCALE_KEY, UI_SCALE_PERCENTS, active_theme, argb_color, color_argb, foreground_for,
    normalize_ui_scale_percent,
};
use gpui::{
    Anchor, App, AppContext as _, Entity, Hsla, InteractiveElement as _, IntoElement as _,
    ParentElement, PathPromptOptions, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Icon, WindowExt as _,
    button::Button,
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    v_flex,
};

use super::SectionContext;
use crate::store::SettingsStore;
use crate::theme_library::{
    self, ImportError, ParsedImport, delete_theme, diagnostic_counts, export_document,
    export_file_name, parse_import, store_import,
};
use crate::ui::{Control, SettingsPage, SettingsSection, meta_text, row_button};
use fluxdown_ui_components::{ButtonVariant, ControlExt as _, FluxIcon, tooltip_view};

/// 官方主题画廊地址。
const THEME_GALLERY_URL: &str = "https://fluxdown.com/themes";

pub(crate) const LOCALE_KEY: &str = "general.locale";

const THEME_CARD_WIDTH: f32 = 120.;
const THEME_PREVIEW_HEIGHT: f32 = 52.;
const THEME_PREVIEW_SIDEBAR_WIDTH: f32 = 28.;
const THEME_PREVIEW_BAR_HEIGHT: f32 = 3.;
const COLOR_DOT_SIZE: f32 = 28.;

pub(crate) fn page(ctx: &SectionContext, _cx: &mut App) -> SettingsPage {
    SettingsPage::new(
        "appearance",
        ctx.t("settingsCatAppearance"),
        ctx.t("settingsCatAppearanceDesc"),
        FluxIcon::Palette,
    )
    .sections([
        SettingsSection::new().row(ctx.item("language", Some("languageDesc"), language_field(ctx))),
        SettingsSection::new()
            .title(ctx.t("settingsGroupTheme"))
            .row(ctx.item("themeMode", Some("themeModeDesc"), theme_mode_field(ctx)))
            .row(
                ctx.item(
                    "themeSelection",
                    Some("themeSelectionDesc"),
                    theme_cards_field(ctx),
                )
                .vertical(),
            )
            .row(
                ctx.item(
                    "themeColor",
                    Some("themeColorDesc"),
                    color_scheme_field(ctx),
                )
                .vertical(),
            )
            .row(super::icon_pack::item(ctx)),
        SettingsSection::new()
            .title(ctx.t("settingsGroupInterface"))
            .row(ctx.item("uiScale", Some("uiScaleDesc"), ui_scale_field(ctx)))
            .row(ctx.item(
                "fontFamily",
                Some("fontFamilyDesktopHint"),
                super::font_family::field(ctx),
            ))
            .row(ctx.item("fontSize", Some("fontSizeDesc"), font_size_field(ctx)))
            .row(super::app_icon::item(ctx)),
    ])
}

fn language_field(ctx: &SectionContext) -> Control {
    let mut options = vec![(SharedString::from("system"), ctx.t("languageSystem"))];
    options.extend(ctx.translator.available_locales().iter().map(|locale| {
        (
            SharedString::from(locale.clone()),
            SharedString::from(ctx.translator.native_name_of(locale).to_owned()),
        )
    }));
    let store = ctx.store();
    let set_store = ctx.store();
    Control::dropdown(
        options,
        move |cx: &App| SharedString::from(store.read(cx).pref_str(LOCALE_KEY, "system")),
        move |value: SharedString, cx: &mut App| {
            set_store.update(cx, |store, cx| {
                store.set_pref_str(LOCALE_KEY, value.to_string(), cx)
            });
        },
    )
}

fn theme_mode_field(ctx: &SectionContext) -> Control {
    let options = vec![
        (SharedString::from("system"), ctx.t("themeModeSystem")),
        (SharedString::from("light"), ctx.t("themeModeLight")),
        (SharedString::from("dark"), ctx.t("themeModeDark")),
    ];
    let store = ctx.store();
    Control::dropdown(
        options,
        move |cx: &App| SharedString::from(active_theme(cx).preference().wire_name()),
        move |value: SharedString, cx: &mut App| {
            let preference = theme_preference(&value);
            store.update(cx, |store, cx| {
                store.set_pref_str(THEME_MODE_KEY, preference.wire_name(), cx)
            });
        },
    )
}

/// 偏好字符串 → 主题偏好；未知值按系统处理。
#[must_use]
pub fn theme_preference(value: &str) -> ThemePreference {
    match value {
        "light" => ThemePreference::Light,
        "dark" => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

// ───────────────────────── 主题卡片 ─────────────────────────

/// 只展示可放进当前明暗槽位的卡片——同外观的内置预设，
/// 以及按 [`theme_library::theme_available_in`] 判定可用的已导入主题（当前选中的导入主题
/// 即使不匹配也显示，便于切走或删除）。卡片下方为导入 / 导出 / 更多主题操作。
fn theme_cards_field(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let dark_label = ctx.t("themeDarkTheme");
    let light_label = ctx.t("themeLightTheme");
    let delete_label = ctx.t("delete");
    let labels: Vec<(BuiltinThemeId, SharedString)> = BuiltinThemeId::ALL
        .into_iter()
        .map(|id| (id, ctx.t(id.label_key())))
        .collect();
    Control::custom(
        move |disabled: bool, _key: &SharedString, _window: &mut Window, cx: &mut App| {
            let state = active_theme(cx);
            let mode = state.mode();
            let selected = state.appearance().theme(mode).clone();
            let tokens = state.tokens().clone();
            let extended = state.extended().clone();
            let card_width = state.text_extent(THEME_CARD_WIDTH);
            let group_label = if mode.is_dark() {
                dark_label.clone()
            } else {
                light_label.clone()
            };

            let builtin_cards = BuiltinThemeId::presets_for(mode).map(|id| {
                let label = labels
                    .iter()
                    .find(|(candidate, _)| *candidate == id)
                    .map_or_else(
                        || SharedString::from(id.wire_name()),
                        |(_, label)| label.clone(),
                    );
                let store = store.clone();
                theme_card(
                    ThemeCard {
                        element_id: SharedString::from(format!("theme-card-{}", id.wire_name())),
                        label,
                        preview: id.colors(),
                        selected: selected == ThemeSelection::Builtin(id),
                    },
                    disabled,
                    &tokens,
                    &extended,
                    card_width,
                    move |cx| select_theme(ThemeSelection::Builtin(id), &store, cx),
                    None,
                )
                .into_any_element()
            });
            let custom_cards = theme_library::imported_themes(cx)
                .into_iter()
                .filter(|theme| theme.available_in(mode) || selected.custom_id() == Some(&theme.id))
                .map(|theme| {
                    let selection = ThemeSelection::Custom(theme.id.clone());
                    let select_store = store.clone();
                    let delete_store = store.clone();
                    let delete_id = theme.id.clone();
                    let delete_translator = translator.clone();
                    theme_card(
                        ThemeCard {
                            element_id: SharedString::from(format!(
                                "theme-card-custom-{}",
                                theme.id
                            )),
                            label: theme.name.clone(),
                            preview: theme.preview(mode),
                            selected: selected == selection,
                        },
                        disabled,
                        &tokens,
                        &extended,
                        card_width,
                        move |cx| select_theme(selection.clone(), &select_store, cx),
                        Some(CardDelete {
                            label: delete_label.clone(),
                            on_delete: Box::new(move |window, cx| {
                                delete_custom_theme(
                                    &delete_id,
                                    &delete_store,
                                    &delete_translator,
                                    window,
                                    cx,
                                );
                            }),
                        }),
                    )
                    .into_any_element()
                });

            v_flex()
                .w_full()
                .gap(tokens.spacing.xs)
                .child(meta_text(cx).child(group_label))
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .flex_wrap()
                        .children(builtin_cards)
                        .children(custom_cards),
                )
                .child(theme_actions(&store, &translator, disabled, cx))
                .into_any_element()
        },
    )
}

fn slot_key(mode: ThemeMode) -> &'static str {
    if mode.is_dark() {
        DARK_THEME_KEY
    } else {
        LIGHT_THEME_KEY
    }
}

/// 把当前明暗槽位切到 `selection`：写偏好（`builtin:<name>` / `custom:<id>`）。
fn select_theme(selection: ThemeSelection, store: &Entity<SettingsStore>, cx: &mut App) {
    let mode = active_theme(cx).mode();
    if *active_theme(cx).appearance().theme(mode) == selection {
        return;
    }
    store.update(cx, |store, cx| {
        store.set_pref_str(slot_key(mode), selection.pref_value(), cx)
    });
}

/// 删除已导入主题（写墓碑，随同步传到其他设备）；任一槽位正选中它时回退到该槽位的内置默认主题。
fn delete_custom_theme(
    id: &SharedString,
    store: &Entity<SettingsStore>,
    translator: &Translator,
    window: &mut Window,
    cx: &mut App,
) {
    if !store.update(cx, |store, cx| delete_theme(id, store, cx)) {
        window.push_notification(
            toast::error(translator.text("themeDeleteError").to_owned()),
            cx,
        );
        return;
    }
    let fallback: Vec<ThemeMode> = [ThemeMode::Dark, ThemeMode::Light]
        .into_iter()
        .filter(|mode| active_theme(cx).appearance().theme(*mode).custom_id() == Some(id))
        .collect();
    if !fallback.is_empty() {
        store.update(cx, |store, cx| {
            for mode in fallback {
                let builtin = ThemeSelection::Builtin(BuiltinThemeId::default_for(mode));
                store.set_pref_str(slot_key(mode), builtin.pref_value(), cx);
            }
        });
    }
    cx.refresh_windows();
}

struct ThemeCard {
    element_id: SharedString,
    label: SharedString,
    preview: ColorTokens,
    selected: bool,
}

type WindowHandler = Box<dyn Fn(&mut Window, &mut App)>;

struct CardDelete {
    label: SharedString,
    on_delete: WindowHandler,
}

fn theme_card(
    card: ThemeCard,
    disabled: bool,
    tokens: &fluxdown_ui_theme::SemanticThemeTokens,
    extended: &ExtendedTokens,
    width: gpui::Pixels,
    on_select: impl Fn(&mut App) + 'static,
    delete: Option<CardDelete>,
) -> impl gpui::IntoElement {
    let ThemeCard {
        element_id,
        label,
        preview,
        selected,
    } = card;
    let colors = tokens.colors;
    let row_hover = extended.colors.row_hover;
    let check_size = extended.icon.sm;
    let bar = |width: gpui::DefiniteLength, color: Hsla| {
        div()
            .w(width)
            .h(px(THEME_PREVIEW_BAR_HEIGHT))
            .rounded_full()
            .bg(color)
    };
    let preview_element = h_flex()
        .w_full()
        .h(px(THEME_PREVIEW_HEIGHT))
        .rounded(tokens.radius.md)
        .border_1()
        .border_color(preview.border)
        .bg(preview.background)
        .overflow_hidden()
        .child(
            v_flex()
                .w(px(THEME_PREVIEW_SIDEBAR_WIDTH))
                .h_full()
                .items_center()
                .justify_center()
                .gap(px(THEME_PREVIEW_BAR_HEIGHT))
                .bg(preview.surface)
                .child(bar(px(16.).into(), preview.primary))
                .child(bar(px(16.).into(), preview.muted_foreground.opacity(0.35)))
                .child(bar(px(16.).into(), preview.muted_foreground.opacity(0.35))),
        )
        .child(
            v_flex()
                .flex_1()
                .h_full()
                .p(tokens.spacing.xs)
                .justify_center()
                .gap(px(THEME_PREVIEW_BAR_HEIGHT))
                .child(bar(gpui::relative(1.), preview.foreground.opacity(0.4)))
                .child(bar(
                    gpui::relative(1.),
                    preview.muted_foreground.opacity(0.4),
                ))
                .child(bar(
                    gpui::relative(0.6),
                    preview.muted_foreground.opacity(0.4),
                )),
        );
    let delete_button = delete.filter(|_| !disabled).map(|delete| {
        let CardDelete { label, on_delete } = delete;
        div()
            .id(SharedString::from(format!("{element_id}-delete")))
            .flex_shrink_0()
            .cursor_pointer()
            .rounded(tokens.radius.sm)
            .text_color(colors.muted_foreground)
            .hover(move |style| style.text_color(colors.destructive))
            .tooltip(move |window, cx| tooltip_view(label.clone(), window, cx))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                on_delete(window, cx);
            })
            .child(Icon::new(FluxIcon::X).size(check_size))
    });

    div()
        .id(element_id)
        .w(width)
        .p(tokens.spacing.sm)
        .rounded(tokens.radius.lg)
        .border_1()
        .border_color(if selected {
            colors.primary
        } else {
            colors.border
        })
        .bg(colors.surface)
        .when(!disabled, |this| {
            this.cursor_pointer()
                .when(!selected, |this| {
                    this.hover(move |style| style.bg(row_hover))
                })
                .on_click(move |_, _, cx| on_select(cx))
        })
        .child(preview_element)
        .child(
            h_flex()
                .mt(tokens.spacing.xs)
                .gap(tokens.spacing.xs)
                .justify_between()
                .items_center()
                .text_size(tokens.typography.xs.size)
                .line_height(tokens.typography.xs.line_height)
                .text_color(colors.foreground)
                .child(div().min_w_0().truncate().child(label))
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap(tokens.spacing.xxs)
                        .items_center()
                        .when(selected, |this| {
                            this.child(
                                Icon::new(FluxIcon::Check)
                                    .size(check_size)
                                    .text_color(colors.primary),
                            )
                        })
                        .children(delete_button),
                ),
        )
}

// ───────────────────────── 导入 / 导出 ─────────────────────────

/// 「导入」「导出 ▾（仅差异 / 完整主题）」「更多主题」。
fn theme_actions(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    disabled: bool,
    cx: &App,
) -> impl gpui::IntoElement {
    let tokens = active_theme(cx).tokens();
    let import_store = store.clone();
    let import_translator = translator.clone();
    let export_items = [
        (ExportMode::Diff, "themeExportDiff"),
        (ExportMode::Full, "themeExportFull"),
    ]
    .map(|(mode, key)| {
        (
            mode,
            SharedString::from(translator.text(key).to_owned()),
            translator.clone(),
        )
    });
    h_flex()
        .mt(tokens.spacing.xs)
        .gap(tokens.spacing.sm)
        .flex_wrap()
        .child(
            row_button(
                "appearance-theme-import",
                translator.text("themeImport").to_owned(),
                ButtonVariant::Secondary,
                cx,
            )
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                import_themes(import_store.clone(), import_translator.clone(), window, cx)
            }),
        )
        .child(
            Button::new("appearance-theme-export")
                .outline()
                .control(cx)
                .label(translator.text("themeExport").to_owned())
                .dropdown_caret(true)
                .disabled(disabled)
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                    export_items
                        .iter()
                        .fold(menu, |menu, (mode, label, translator)| {
                            let mode = *mode;
                            let translator = translator.clone();
                            menu.item(PopupMenuItem::new(label.clone()).on_click(
                                move |_, window, cx| {
                                    export_theme(mode, translator.clone(), window, cx)
                                },
                            ))
                        })
                }),
        )
        .child(
            row_button(
                "appearance-theme-more",
                translator.text("themeMore").to_owned(),
                ButtonVariant::Link,
                cx,
            )
            .on_click(|_, _, cx| cx.open_url(THEME_GALLERY_URL)),
        )
}

/// 选择一个或多个主题文件（本格式 / v1 / Flutter FluxThemeJson），逐个解析后把原文原样写入
/// 主题偏好（随配置同步到其他设备），由偏好投影注册；结束后以通知汇总成功数、诊断（迁移 /
/// 未知键 / 非法值 / 越界 / 新版本等）与失败原因。
fn import_themes(
    store: Entity<SettingsStore>,
    translator: Translator,
    window: &mut Window,
    cx: &mut App,
) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: Some(SharedString::from(
            translator.text("themeImport").to_owned(),
        )),
    });
    let window_handle = window.window_handle();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = receiver.await else {
            return;
        };
        let results = cx
            .background_spawn(async move {
                paths
                    .into_iter()
                    .map(|path| {
                        let name = path.file_name().map_or_else(
                            || path.display().to_string(),
                            |name| name.to_string_lossy().into_owned(),
                        );
                        let result = std::fs::read_to_string(&path)
                            .map_err(ImportError::Read)
                            .and_then(parse_import);
                        (name, result)
                    })
                    .collect::<Vec<_>>()
            })
            .await;
        let report = cx.update(|cx| import_report(results, &store, &translator, cx));

        let Ok(()) = window_handle.update(cx, move |_, window, cx| {
            for notification in report {
                window.push_notification(notification, cx);
            }
        }) else {
            // 设置视图或窗口已释放，结束回调，不再更新状态。
            return;
        };
    })
    .detach();
}

/// 写入成功项并生成汇总通知：成功数 / 各文件诊断计数 / 失败原因。
fn import_report(
    results: Vec<(String, Result<ParsedImport, ImportError>)>,
    store: &Entity<SettingsStore>,
    translator: &Translator,
    cx: &mut App,
) -> Vec<Notification> {
    let mut imported = 0usize;
    let mut adjusted = Vec::new();
    let mut failed = Vec::new();
    for (name, result) in results {
        let result = result.and_then(|parsed| {
            let counts = diagnostic_counts(&parsed.diagnostics);
            store
                .update(cx, |store, cx| store_import(parsed, store, cx))
                .map(|_| counts)
        });
        match result {
            Ok(counts) => {
                imported += 1;
                if !counts.is_empty() {
                    let summary = counts
                        .into_iter()
                        .map(|(key, count)| format!("{} {count}", translator.text(key)))
                        .collect::<Vec<_>>()
                        .join(" · ");
                    adjusted.push(format!("{name}: {summary}"));
                }
            }
            Err(error) => {
                let reason = translator.text(error.i18n_key());
                failed.push(match error.io_detail() {
                    Some(detail) => format!("{name}: {reason} ({detail})"),
                    None => format!("{name}: {reason}"),
                });
            }
        }
    }
    if imported > 0 {
        cx.refresh_windows();
    }

    let mut notifications = Vec::new();
    if imported > 0 {
        notifications.push(toast::success(format!(
            "{} ({imported})",
            translator.text("themeImportSuccess")
        )));
    }
    if !adjusted.is_empty() {
        notifications.push(
            toast::warning(adjusted.join("\n"))
                .title(translator.text("themeImportDiagnosticsTitle").to_owned())
                .autohide(false),
        );
    }
    if !failed.is_empty() {
        notifications.push(
            toast::error(failed.join("\n"))
                .title(translator.text("themeImportError").to_owned())
                .autohide(false),
        );
    }
    notifications
}

/// 把当前明暗模式生效的主题导出为文件：`Diff` 只写与基底不同的值，`Full` 写出全部 token。
/// 内置主题连同用户强调色一起导出。
fn export_theme(mode: ExportMode, translator: Translator, window: &mut Window, cx: &mut App) {
    let document = export_document(cx);
    let text = document.to_json_pretty(mode);
    let file_name = export_file_name(&document);
    let directory = std::env::home_dir().unwrap_or_else(std::env::temp_dir);
    let receiver = cx.prompt_for_new_path(&directory, Some(&file_name));
    let window_handle = window.window_handle();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(path))) = receiver.await else {
            return;
        };
        let result = cx
            .background_spawn(async move { std::fs::write(&path, text) })
            .await;

        let Ok(()) = window_handle.update(cx, move |_, window, cx| {
            let notification = match result {
                Ok(()) => toast::success(translator.text("themeExportSuccess").to_owned()),
                Err(error) => {
                    toast::error(format!("{}: {error}", translator.text("themeExportError")))
                }
            };
            window.push_notification(notification, cx);
        }) else {
            // 设置视图或窗口已释放，结束回调，不再更新状态。
            return;
        };
    })
    .detach();
}

// ───────────────────────── 强调色 ─────────────────────────

struct CustomColorSlot {
    picker: Entity<ColorPickerState>,
    last_synced: u32,
    _subscription: Subscription,
}

/// 4 个预设色点 + 自定义；选中自定义时展开取色器。
fn color_scheme_field(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let labels: Vec<(AccentScheme, SharedString)> = AccentScheme::ALL
        .into_iter()
        .map(|scheme| (scheme, ctx.t(scheme.label_key())))
        .collect();
    let custom_label = ctx.t("colorCustom");
    Control::custom(
        move |disabled: bool, _key: &SharedString, window: &mut Window, cx: &mut App| {
            let state = active_theme(cx);
            let appearance = state.appearance().clone();
            let tokens = state.tokens().clone();
            let icon_size = state.extended().icon.md;

            let dots = h_flex()
                .gap(tokens.spacing.sm)
                .flex_wrap()
                .children(labels.iter().map(|(scheme, label)| {
                    color_dot(
                        *scheme,
                        label.clone(),
                        appearance.clone(),
                        disabled,
                        &tokens,
                        icon_size,
                        store.clone(),
                    )
                }));

            let mut column = v_flex().w_full().gap(tokens.spacing.md).child(dots);
            if appearance.color_scheme == AccentScheme::Custom {
                column = column.child(custom_color_picker(
                    appearance.custom_color,
                    custom_label.clone(),
                    disabled,
                    store.clone(),
                    window,
                    cx,
                ));
            }
            column.into_any_element()
        },
    )
}

fn color_dot(
    scheme: AccentScheme,
    label: SharedString,
    appearance: AppearancePreferences,
    disabled: bool,
    tokens: &fluxdown_ui_theme::SemanticThemeTokens,
    icon_size: Pixels,
    store: Entity<SettingsStore>,
) -> impl gpui::IntoElement {
    let colors = tokens.colors;
    let selected = appearance.color_scheme == scheme;
    let color = scheme.color(appearance.custom_color);
    let icon: Option<Icon> = if selected {
        Some(FluxIcon::Check.into())
    } else if scheme == AccentScheme::Custom {
        Some(FluxIcon::Palette.into())
    } else {
        None
    };
    let tooltip_label = label;
    div()
        .id(SharedString::from(format!(
            "accent-dot-{}",
            scheme.wire_name()
        )))
        .size(px(COLOR_DOT_SIZE))
        .rounded_full()
        .bg(color)
        .border_2()
        .border_color(if selected { colors.foreground } else { color })
        .flex()
        .items_center()
        .justify_center()
        .tooltip(move |window, cx| tooltip_view(tooltip_label.clone(), window, cx))
        .when(!disabled, |this| {
            this.cursor_pointer()
                .when(!selected, |this| {
                    this.hover(move |style| style.border_color(colors.muted_foreground))
                })
                .on_click(move |_, _, cx| {
                    if active_theme(cx).appearance().color_scheme == scheme {
                        return;
                    }
                    store.update(cx, |store, cx| {
                        store.set_pref_str(COLOR_SCHEME_KEY, scheme.wire_name(), cx);
                    });
                })
        })
        .when_some(icon, |this, icon| {
            this.child(icon.size(icon_size).text_color(foreground_for(color)))
        })
}

/// gpui-component 取色器：色板 + HSLA 滑块 + 十六进制输入，提交即生效并写入偏好。
fn custom_color_picker(
    custom_color: u32,
    label: SharedString,
    disabled: bool,
    store: Entity<SettingsStore>,
    window: &mut Window,
    cx: &mut App,
) -> impl gpui::IntoElement {
    let slot = window.use_keyed_state(SharedString::from("settings-accent-custom"), cx, {
        let store = store.clone();
        move |window, cx| {
            let picker = cx.new(|cx| {
                ColorPickerState::new(window, cx).default_value(argb_color(custom_color))
            });
            let _subscription = cx.subscribe(
                &picker,
                move |slot: &mut CustomColorSlot, _, event: &ColorPickerEvent, cx| {
                    let ColorPickerEvent::Change(Some(color)) = event else {
                        return;
                    };
                    let argb = color_argb(*color);
                    if argb == slot.last_synced {
                        return;
                    }
                    slot.last_synced = argb;
                    // 自定义色只在 `color_scheme == custom` 时生效：两键一起写，否则偏好里仍是旧方案，
                    // 投影会把强调色回退。云同步目录约定整数 ARGB。
                    store.update(cx, |store, cx| {
                        store.set_pref_str(COLOR_SCHEME_KEY, AccentScheme::Custom.wire_name(), cx);
                        store.set_pref_i64(CUSTOM_COLOR_KEY, i64::from(argb), cx);
                    });
                },
            );
            CustomColorSlot {
                picker,
                last_synced: custom_color,
                _subscription,
            }
        }
    });
    slot.update(cx, |slot, cx| {
        if slot.last_synced != custom_color {
            slot.last_synced = custom_color;
            slot.picker.update(cx, |picker, cx| {
                picker.set_value(argb_color(custom_color), window, cx);
            });
        }
    });
    let picker = slot.read(cx).picker.clone();
    // 不传 featured colors：gpui-component 用 `color-{hex}` 作为色块 ElementId，且特色行与
    // 调色板行同处一个无 id 的父级；预设色（如 #3b82f6 / #22c55e）与调色板色重复时会产生
    // 重复的 a11y 节点 id，调试构建直接 panic。预设色已由上方色点提供，这里只保留调色板。
    div().when(disabled, |this| this.opacity(0.5)).child(
        ColorPicker::new(&picker)
            .label(label)
            .featured_colors(Vec::new()),
    )
}

// ───────────────────────── 界面缩放 / 字体大小 ─────────────────────────

fn ui_scale_field(ctx: &SectionContext) -> Control {
    let options: Vec<(SharedString, SharedString)> = UI_SCALE_PERCENTS
        .iter()
        .map(|percent| {
            (
                SharedString::from(percent.to_string()),
                SharedString::from(format!("{percent}%")),
            )
        })
        .collect();
    let store = ctx.store();
    Control::dropdown(
        options,
        move |cx: &App| SharedString::from(active_theme(cx).ui_scale_percent().to_string()),
        move |value: SharedString, cx: &mut App| {
            let Ok(percent) = value.parse::<u16>() else {
                return;
            };
            let mut appearance = active_theme(cx).appearance().clone();
            appearance.ui_scale_percent = normalize_ui_scale_percent(percent);
            let scale = appearance.ui_scale_pref_value();
            store.update(cx, |store, cx| {
                store.set_pref(UI_SCALE_KEY, serde_json::Value::from(scale), cx);
            });
        },
    )
}

/// 下拉中「跟随主题」项的值；写入偏好时为 `null`（清除本机字号）。
const FONT_SIZE_THEME: &str = "theme";

/// 本机正文字号（px）：其余文字与承载文字的行 / 控件高度同比适配，再与界面缩放相乘。
fn font_size_field(ctx: &SectionContext) -> Control {
    let mut options = vec![(
        SharedString::from(FONT_SIZE_THEME),
        ctx.t("fontSizeFollowTheme"),
    )];
    options.extend(FONT_SIZES.iter().map(|size| {
        (
            SharedString::from(size.to_string()),
            SharedString::from(format!("{size} px")),
        )
    }));
    let store = ctx.store();
    Control::dropdown(
        options,
        move |cx: &App| {
            active_theme(cx)
                .appearance()
                .font_size
                .map_or(SharedString::from(FONT_SIZE_THEME), |size| {
                    SharedString::from(size.to_string())
                })
        },
        move |value: SharedString, cx: &mut App| {
            let pref = match value.parse::<u16>() {
                Ok(size) if FONT_SIZE_RANGE.contains(&size) => serde_json::Value::from(size),
                Ok(_) => return,
                Err(_) => serde_json::Value::Null,
            };
            store.update(cx, |store, cx| store.set_pref(FONT_SIZE_KEY, pref, cx));
        },
    )
}
