//! 「快捷键」设置页：应用内 / 全局两个 Tab，逐动作展示触发器、录制新触发器、恢复默认。
//!
//! 偏好为设备本地的 agent 偏好（`desktop.shortcuts.app` / `desktop.shortcuts.global`，
//! 不在云同步目录内）；纯逻辑见 [`crate::shortcut_record`]。

use std::rc::Rc;
use std::time::Instant;

use fluxdown_protocol::shortcuts::{
    GlobalShortcutBackend, GlobalShortcutState, GlobalShortcutStatusDto, ModifierKey,
    ResolvedShortcut, ShortcutDomain, ShortcutScope, ShortcutSpec, ShortcutTrigger,
    resolve_shortcuts, shortcuts_pref_key,
};
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{ButtonVariant, CalloutTone, FluxIcon, RevealCallout, tooltip_view};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, Context, ElementId, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ModifiersChangedEvent, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{Icon, WindowExt as _, h_flex, v_flex};
use serde_json::Value;

use super::SectionContext;
use crate::shortcut_display::{set_shortcut_recording, shortcut_keycaps, shortcut_label};
use crate::shortcut_record::{
    DoubleTapRecorder, KeyInterpretation, RecordError, assign_trigger, cross_scope_matches,
    interpret_keystroke, pressed_modifiers, remove_trigger, reset_trigger,
};
use crate::store::SettingsStore;
use crate::ui::{
    Control, SettingsPage, SettingsRow, SettingsSection, SettingsTab, meta_text, row_button,
};

const OPEN_ACCESSIBILITY_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// 符号键帽（⌘⌥⇧⌃、方向键、标点）相对正文字号的放大倍数：这些字形在同字号下只有字母约
/// 七成高，放大后与字母键帽视觉等高，字母仍保持正文字号。
const SYMBOL_CAP_SCALE: f32 = 1.3;

/// 单个非字母数字字符的键帽按符号放大；`Ctrl`、`Space`、`F12` 这类单词与字母保持正文字号。
fn is_symbol_cap(cap: &str) -> bool {
    let mut chars = cap.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if !c.is_alphanumeric())
}

pub(crate) fn page(ctx: &SectionContext, cx: &mut App) -> SettingsPage {
    SettingsPage::new(
        "shortcuts",
        ctx.t("settingsCatShortcuts"),
        ctx.t("settingsCatShortcutsDesc"),
        FluxIcon::Keyboard,
    )
    .tab(
        SettingsTab::new("app", ctx.t("shortcutTabApp")).sections(scope_sections(
            ctx,
            ShortcutScope::App,
            cx,
        )),
    )
    .tab(
        SettingsTab::new("global", ctx.t("shortcutTabGlobal")).sections(scope_sections(
            ctx,
            ShortcutScope::Global,
            cx,
        )),
    )
}

/// 一个 Tab 内所有行共享的只读环境（页面每帧重建，快照随之刷新）。
struct RowEnv {
    scope: ShortcutScope,
    store: gpui::Entity<SettingsStore>,
    translator: Translator,
    backend: GlobalShortcutBackend,
    status: Vec<GlobalShortcutStatusDto>,
    /// 另一作用域的当前生效绑定（跨作用域重复提示用）。
    other_scope: Vec<ResolvedShortcut>,
    /// 全局热键后端不可用：禁止录制新触发器。
    record_blocked: bool,
}

fn scope_sections(ctx: &SectionContext, scope: ShortcutScope, cx: &App) -> Vec<SettingsSection> {
    let store = ctx.store.read(cx);
    let overrides = store.pref(shortcuts_pref_key(scope)).cloned();
    let other = match scope {
        ShortcutScope::App => ShortcutScope::Global,
        ShortcutScope::Global => ShortcutScope::App,
    };
    let other_overrides = store.pref(shortcuts_pref_key(other));
    let shell = store.shell().global_shortcuts.clone();
    let resolved = resolve_shortcuts(scope, overrides.as_ref());
    let any_customized = resolved.iter().any(|entry| entry.customized);
    let env = Rc::new(RowEnv {
        scope,
        store: ctx.store(),
        translator: ctx.translator.clone(),
        backend: shell.backend,
        record_blocked: scope == ShortcutScope::Global
            && shell.backend == GlobalShortcutBackend::Unavailable,
        status: shell.entries,
        other_scope: resolve_shortcuts(other, other_overrides),
    });

    let mut sections = Vec::new();
    if scope == ShortcutScope::Global {
        sections.extend(global_status_sections(ctx, &env));
    }
    match scope {
        ShortcutScope::App => {
            let groups = [
                (ShortcutDomain::Everywhere, "shortcutGroupEverywhere"),
                (ShortcutDomain::DownloadList, "shortcutGroupDownloadList"),
            ];
            for (index, (domain, title_key)) in groups.into_iter().enumerate() {
                let entries: Vec<&ResolvedShortcut> = resolved
                    .iter()
                    .filter(|entry| entry.spec.domain == domain)
                    .collect();
                let mut section = rows_section(ctx, &env, entries).title(ctx.t(title_key));
                if index == 0 {
                    section = section.subtitle(ctx.t("shortcutAppDesc"));
                }
                sections.push(section);
            }
        }
        ShortcutScope::Global => {
            sections.push(
                rows_section(ctx, &env, resolved.iter().collect())
                    .title(ctx.t("shortcutGroupGlobal"))
                    .subtitle(ctx.t("shortcutGlobalDesc")),
            );
        }
    }
    sections.push(reset_all_section(ctx, scope, any_customized));
    sections
}

fn rows_section(
    ctx: &SectionContext,
    env: &Rc<RowEnv>,
    entries: Vec<&ResolvedShortcut>,
) -> SettingsSection {
    let mut section = SettingsSection::new();
    for entry in entries {
        let spec = entry.spec;
        let triggers = entry.triggers.clone();
        let customized = entry.customized;
        let env = env.clone();
        section = section.row(SettingsRow::new(
            ctx.t(spec.label_key),
            Control::custom(
                move |_disabled, key: &SharedString, window: &mut Window, cx: &mut App| {
                    row_control(&env, spec, &triggers, customized, key, window, cx)
                },
            ),
        ));
    }
    section
}

fn reset_all_section(ctx: &SectionContext, scope: ShortcutScope, enabled: bool) -> SettingsSection {
    let store = ctx.store();
    let label = ctx.t("shortcutResetAll");
    SettingsSection::new().row(SettingsRow::custom(
        move |_disabled, key: &SharedString, _window, cx: &mut App| {
            let store = store.clone();
            h_flex().w_full().justify_end().child(
                row_button(
                    ElementId::from(SharedString::from(format!("{key}-reset-all"))),
                    label.clone(),
                    ButtonVariant::Secondary,
                    cx,
                )
                .disabled(!enabled)
                .on_click(move |_, _, cx| {
                    store.update(cx, |store, cx| {
                        store.set_pref(shortcuts_pref_key(scope), Value::Null, cx);
                    });
                }),
            )
        },
    ))
}

/// 全局 Tab 顶部的环境提示：后端不可用 / Wayland portal / macOS 辅助功能授权。
fn global_status_sections(ctx: &SectionContext, env: &Rc<RowEnv>) -> Vec<SettingsSection> {
    let mut sections = Vec::new();
    match env.backend {
        GlobalShortcutBackend::Unavailable => {
            sections.push(callout_section(
                ctx,
                CalloutTone::Warning,
                "shortcutGlobalUnavailableTitle",
                "shortcutGlobalUnavailableDesc",
                None,
            ));
        }
        GlobalShortcutBackend::WaylandPortal => {
            sections.push(callout_section(
                ctx,
                CalloutTone::Warning,
                "shortcutWaylandTitle",
                "shortcutWaylandDesc",
                None,
            ));
        }
        GlobalShortcutBackend::MacOs
            if env
                .status
                .iter()
                .any(|entry| entry.state == GlobalShortcutState::PermissionRequired) =>
        {
            sections.push(callout_section(
                ctx,
                CalloutTone::Warning,
                "shortcutPermissionTitle",
                "shortcutPermissionDesc",
                Some(ctx.t("shortcutOpenAccessibility")),
            ));
        }
        GlobalShortcutBackend::Windows
        | GlobalShortcutBackend::MacOs
        | GlobalShortcutBackend::X11 => {}
    }
    sections
}

fn callout_section(
    ctx: &SectionContext,
    tone: CalloutTone,
    title_key: &str,
    desc_key: &str,
    open_settings_label: Option<SharedString>,
) -> SettingsSection {
    let title = ctx.t(title_key);
    let desc = ctx.t(desc_key);
    SettingsSection::new().row(SettingsRow::custom(
        move |_disabled, key: &SharedString, _window, cx: &mut App| {
            let spacing = active_theme(cx).tokens().spacing;
            v_flex()
                .w_full()
                .gap(spacing.sm)
                .child(RevealCallout::new(
                    ElementId::from(SharedString::from(format!("{key}-callout"))),
                    true,
                    tone,
                    title.clone(),
                    desc.clone(),
                ))
                .when_some(open_settings_label.clone(), |this, label| {
                    this.child(
                        h_flex().child(
                            row_button(
                                ElementId::from(SharedString::from(format!("{key}-open-system"))),
                                label,
                                ButtonVariant::Secondary,
                                cx,
                            )
                            .on_click(|_, _, cx| cx.open_url(OPEN_ACCESSIBILITY_URL)),
                        ),
                    )
                })
        },
    ))
}

// ───────────────────────── 录制器 ─────────────────────────

/// 一行的录制状态：焦点句柄 + 录制中 / 错误提示 + 双击识别。
struct RecorderState {
    focus: FocusHandle,
    recording: bool,
    error: Option<RecordError>,
    taps: DoubleTapRecorder,
    _blur: Subscription,
    _release: Subscription,
}

impl RecorderState {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_focus_out(&focus, window, |this, _event, _window, cx| this.stop(cx));
        // 录制中被整行销毁（切页 / 关窗）时不会有 blur，释放时补清全局录制标记。
        let release = cx.on_release(|this, cx| {
            if this.recording {
                set_shortcut_recording(false, cx);
            }
        });
        Self {
            focus,
            recording: false,
            error: None,
            taps: DoubleTapRecorder::default(),
            _blur: blur,
            _release: release,
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.recording = true;
        self.error = None;
        self.taps.reset();
        set_shortcut_recording(true, cx);
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if self.recording {
            self.recording = false;
            set_shortcut_recording(false, cx);
        }
        self.error = None;
        self.taps.reset();
        cx.notify();
    }

    fn reject(&mut self, error: RecordError, cx: &mut Context<Self>) {
        self.taps.reset();
        self.error = Some(error);
        cx.notify();
    }
}

/// 把录到的触发器写入偏好（冲突动作同写摘除并提示），结束录制。
fn commit(
    env: &RowEnv,
    spec: &'static ShortcutSpec,
    trigger: &ShortcutTrigger,
    state: &gpui::Entity<RecorderState>,
    window: &mut Window,
    cx: &mut App,
) {
    let key = shortcuts_pref_key(env.scope);
    let overrides = env.store.read(cx).pref(key).cloned();
    let assignment = assign_trigger(env.scope, overrides.as_ref(), spec, trigger);
    env.store
        .update(cx, |store, cx| store.set_pref(key, assignment.value, cx));
    for displaced in assignment.displaced {
        let action = env.translator.text(displaced.label_key).to_owned();
        let text = env
            .translator
            .text_with("shortcutDisplaced", &[("action", &action)]);
        window.push_notification(toast::info(text), cx);
    }
    state.update(cx, |state, cx| state.stop(cx));
}

fn record_hint(env: &RowEnv) -> String {
    let double_tap_ok =
        env.backend != GlobalShortcutBackend::WaylandPortal || env.scope == ShortcutScope::App;
    if !double_tap_ok {
        return env
            .translator
            .text("shortcutRecordHintChordOnly")
            .to_owned();
    }
    let keys = ModifierKey::ALL
        .iter()
        .map(|modifier| modifier.label())
        .collect::<Vec<_>>()
        .join(" / ");
    env.translator
        .text_with("shortcutRecordHint", &[("keys", &keys)])
}

// ───────────────────────── 行控件 ─────────────────────────

fn row_control(
    env: &Rc<RowEnv>,
    spec: &'static ShortcutSpec,
    triggers: &[ShortcutTrigger],
    customized: bool,
    key: &SharedString,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let (tokens, extended, max_width) = {
        let theme = active_theme(cx);
        (
            theme.tokens().clone(),
            theme.extended().clone(),
            theme.text_extent(520.),
        )
    };
    let state = window.use_keyed_state(
        SharedString::from(format!("{key}-recorder")),
        cx,
        RecorderState::new,
    );
    let (recording, error, focus) = {
        let state = state.read(cx);
        (state.recording, state.error, state.focus.clone())
    };

    let chips: Vec<AnyElement> = triggers
        .iter()
        .enumerate()
        .map(|(index, trigger)| {
            let chip_id = format!("{key}-chip-{index}");
            let badge = status_badge(env, spec, trigger, &chip_id, cx);
            let remove_trigger_value = trigger.clone();
            let remove_env = env.clone();
            let remove_hint = env.translator.text("shortcutRemove").to_owned();
            let full_label = shortcut_label(trigger, &env.translator);
            // 字母 / 单词键帽与正文同字号；⌘⌥⇧ 与标点这类单字符号在同字号下字面明显偏小，
            // 单独放大到与字母视觉等高。
            let body = tokens.typography.sm;
            let keycaps = shortcut_keycaps(trigger).into_iter().map(|cap| {
                let size = if is_symbol_cap(cap) {
                    body.size * SYMBOL_CAP_SCALE
                } else {
                    body.size
                };
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .min_w(px(22.))
                    .h(px(22.))
                    .px(px(5.))
                    .rounded(tokens.radius.sm)
                    .border_1()
                    .border_color(extended.colors.hairline)
                    .bg(tokens.colors.background)
                    .text_size(size)
                    .line_height(px(22.))
                    .text_color(tokens.colors.foreground)
                    .child(cap)
            });
            h_flex()
                .items_center()
                .gap(tokens.spacing.xs)
                .child(
                    h_flex()
                        .id(ElementId::from(SharedString::from(format!(
                            "{chip_id}-caps"
                        ))))
                        .items_center()
                        .gap(px(3.))
                        .h(px(30.))
                        .pl(px(4.))
                        .pr(px(2.))
                        .rounded(tokens.radius.md)
                        .border_1()
                        .border_color(extended.colors.hairline)
                        .bg(extended.colors.chrome)
                        .tooltip(move |window, cx| tooltip_view(full_label.clone(), window, cx))
                        .children(keycaps)
                        .child(
                            div()
                                .id(ElementId::from(SharedString::from(format!("{chip_id}-x"))))
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(22.))
                                .rounded(tokens.radius.sm)
                                .cursor_pointer()
                                .text_color(extended.colors.text_tertiary)
                                .hover(|this| this.bg(extended.colors.row_hover))
                                .tooltip(move |window, cx| {
                                    tooltip_view(remove_hint.clone(), window, cx)
                                })
                                .on_click(move |_, _, cx| {
                                    let pref = shortcuts_pref_key(remove_env.scope);
                                    let overrides = remove_env.store.read(cx).pref(pref).cloned();
                                    let value = remove_trigger(
                                        remove_env.scope,
                                        overrides.as_ref(),
                                        spec,
                                        &remove_trigger_value,
                                    );
                                    remove_env
                                        .store
                                        .update(cx, |store, cx| store.set_pref(pref, value, cx));
                                })
                                .child(Icon::new(FluxIcon::X).size(extended.icon.md)),
                        ),
                )
                .children(badge)
                .into_any_element()
        })
        .collect();

    let record_area = if recording {
        let cancel_state = state.clone();
        h_flex()
            .items_center()
            .gap(tokens.spacing.sm)
            .child(
                meta_text(cx)
                    .text_color(extended.colors.accent_text)
                    .child(SharedString::from(record_hint(env))),
            )
            .child(
                row_button(
                    ElementId::from(SharedString::from(format!("{key}-cancel"))),
                    env.translator.text("cancel").to_owned(),
                    ButtonVariant::Ghost,
                    cx,
                )
                .on_click(move |_, _, cx| cancel_state.update(cx, |state, cx| state.stop(cx))),
            )
            .into_any_element()
    } else {
        let start_state = state.clone();
        let focus = focus.clone();
        row_button(
            ElementId::from(SharedString::from(format!("{key}-add"))),
            env.translator.text("shortcutAdd").to_owned(),
            ButtonVariant::Secondary,
            cx,
        )
        .disabled(env.record_blocked)
        .on_click(move |_, window, cx| {
            start_state.update(cx, |state, cx| state.start(cx));
            focus.focus(window, cx);
        })
        .into_any_element()
    };

    let reset = customized.then(|| {
        let reset_env = env.clone();
        row_button(
            ElementId::from(SharedString::from(format!("{key}-reset"))),
            env.translator.text("shortcutReset").to_owned(),
            ButtonVariant::Ghost,
            cx,
        )
        .on_click(move |_, _, cx| {
            let pref = shortcuts_pref_key(reset_env.scope);
            let overrides = reset_env.store.read(cx).pref(pref).cloned();
            let value = reset_trigger(overrides.as_ref(), spec);
            reset_env
                .store
                .update(cx, |store, cx| store.set_pref(pref, value, cx));
        })
    });

    let warnings: Vec<AnyElement> = triggers
        .iter()
        .flat_map(|trigger| {
            cross_scope_matches(&env.other_scope, trigger)
                .into_iter()
                .map(|other| {
                    let key = match env.scope {
                        ShortcutScope::App => "shortcutCrossScopeGlobal",
                        ShortcutScope::Global => "shortcutCrossScopeApp",
                    };
                    let action = env.translator.text(other.spec.label_key).to_owned();
                    meta_text(cx)
                        .text_color(extended.colors.warning)
                        .child(SharedString::from(
                            env.translator.text_with(key, &[("action", &action)]),
                        ))
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        })
        .collect();

    let on_key_down = {
        let env = env.clone();
        let state = state.clone();
        move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
            if !state.read(cx).recording {
                return;
            }
            cx.stop_propagation();
            match interpret_keystroke(&event.keystroke.modifiers, &event.keystroke.key, env.scope) {
                KeyInterpretation::ModifierOnly => {}
                KeyInterpretation::Cancel => state.update(cx, |state, cx| state.stop(cx)),
                KeyInterpretation::Rejected(error) => {
                    state.update(cx, |state, cx| state.reject(error, cx));
                }
                KeyInterpretation::Trigger(trigger) => {
                    commit(&env, spec, &trigger, &state, window, cx);
                }
            }
        }
    };
    let on_modifiers = {
        let env = env.clone();
        let state = state.clone();
        move |event: &ModifiersChangedEvent, window: &mut Window, cx: &mut App| {
            let tapped = state.update(cx, |state, _| {
                state.recording.then(|| {
                    state
                        .taps
                        .modifiers_changed(&pressed_modifiers(&event.modifiers), Instant::now())
                })
            });
            let Some(Some(modifier)) = tapped else {
                return;
            };
            if env.scope == ShortcutScope::Global
                && env.backend == GlobalShortcutBackend::WaylandPortal
            {
                state.update(cx, |state, cx| {
                    state.reject(RecordError::DoubleTapUnsupported, cx);
                });
                return;
            }
            commit(
                &env,
                spec,
                &ShortcutTrigger::DoubleTap(modifier),
                &state,
                window,
                cx,
            );
        }
    };

    let content = h_flex()
        .flex_wrap()
        .justify_end()
        .items_center()
        .gap(tokens.spacing.sm)
        .max_w(max_width)
        .children(chips)
        .when(triggers.is_empty() && !recording, |this| {
            this.child(
                meta_text(cx)
                    .text_color(extended.colors.text_tertiary)
                    .child(SharedString::from(
                        env.translator.text("shortcutUnbound").to_owned(),
                    )),
            )
        })
        .child(record_area)
        .children(reset);

    v_flex()
        .items_end()
        .gap(tokens.spacing.xxs)
        .child(
            div()
                .id(ElementId::from(SharedString::from(format!(
                    "{key}-recorder-surface"
                ))))
                .track_focus(&focus)
                .capture_key_down(on_key_down)
                .on_modifiers_changed(on_modifiers)
                .child(content),
        )
        .when_some(error, |this, error| {
            this.child(meta_text(cx).text_color(tokens.colors.destructive).child(
                SharedString::from(env.translator.text(error.i18n_key()).to_owned()),
            ))
        })
        .children(warnings)
        .into_any_element()
}

/// 全局快捷键的注册状态徽标（仅非 Active）；按（动作 id, 规范触发器文本）匹配 agent 状态。
fn status_badge(
    env: &RowEnv,
    spec: &ShortcutSpec,
    trigger: &ShortcutTrigger,
    id: &str,
    cx: &App,
) -> Option<AnyElement> {
    if env.scope != ShortcutScope::Global {
        return None;
    }
    let canonical = trigger.to_string();
    let entry = env
        .status
        .iter()
        .find(|entry| entry.action == spec.id && entry.trigger == canonical)?;
    let theme = active_theme(cx);
    let colors = theme.extended().colors;
    let (label_key, color) = match entry.state {
        GlobalShortcutState::Active => return None,
        GlobalShortcutState::InUse => ("shortcutStateInUse", theme.tokens().colors.destructive),
        GlobalShortcutState::Unsupported => ("shortcutStateUnsupported", colors.warning),
        GlobalShortcutState::PermissionRequired => ("shortcutStatePermission", colors.warning),
        GlobalShortcutState::Failed => ("shortcutStateFailed", theme.tokens().colors.destructive),
    };
    let label = SharedString::from(env.translator.text(label_key).to_owned());
    let detail = entry.detail.clone().map(SharedString::from);
    Some(
        meta_text(cx)
            .id(ElementId::from(SharedString::from(format!("{id}-badge"))))
            .text_color(color)
            .when_some(detail, |this, detail| {
                this.tooltip(move |window, cx| tooltip_view(detail.clone(), window, cx))
            })
            .child(label)
            .into_any_element(),
    )
}
