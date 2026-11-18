//! 显式保存的主机名 / IP 连接限制；语法与校验由 daemon 负责。

use fluxdown_ui_components::{ButtonVariant, button, loading_button};
use fluxdown_ui_theme::{CONTROL_HEIGHT, active_theme};
use gpui::{
    App, AppContext as _, Entity, IntoElement as _, ParentElement, SharedString, Styled,
    Subscription, Window, px,
};
use gpui_component::{
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    v_flex,
};

use super::SectionContext;
use crate::store::SettingsStore;
use crate::ui::{Control, SettingsRow};

const KEY: &str = "connection_rules";

struct Editor {
    state: Entity<TextareaState>,
    baseline: String,
    dirty: bool,
    submitted: Option<String>,
    _subscription: Subscription,
}

impl Editor {
    fn create(
        value: String,
        placeholder: SharedString,
        store: Entity<SettingsStore>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .default_value(value.clone())
                .placeholder(placeholder)
        });
        let subscription = cx.subscribe(
            &state,
            move |this: &mut Self, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.dirty = state.read(cx).value().as_str() != this.baseline.as_str();
                    store.update(cx, |_, cx| cx.notify());
                }
            },
        );
        Self {
            state,
            baseline: value,
            dirty: false,
            submitted: None,
            _subscription: subscription,
        }
    }

    fn refresh(
        &mut self,
        value: String,
        pending: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if !pending && let Some(submitted) = self.submitted.take() {
            // 只有确认成功才释放草稿；失败回滚不能抹掉正在修正的规则。
            if submitted == value {
                self.dirty = false;
            }
        }
        if !self.dirty && self.baseline != value {
            self.baseline = value.clone();
            self.state
                .update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }
}

pub(crate) fn item(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let placeholder = ctx.t("connectionRulesPlaceholder");
    let save = ctx.t("proxyRuleSave");
    let cancel = ctx.t("cancel");
    let control = Control::custom(move |disabled, key: &SharedString, window, cx: &mut App| {
        let current = store.read(cx).daemon_str(KEY);
        let pending = store.read(cx).daemon_pending(KEY);
        let slot = window.use_keyed_state(SharedString::from(format!("{key}-editor")), cx, {
            let current = current.clone();
            let placeholder = placeholder.clone();
            let store = store.clone();
            move |window, cx| Editor::create(current, placeholder, store, window, cx)
        });
        slot.update(cx, |editor, cx| {
            editor.refresh(current.clone(), pending || disabled, window, cx)
        });
        let editor = slot.read(cx);
        let state = editor.state.clone();
        let dirty = editor.dirty;
        let unchanged = state.read(cx).value().trim() == current;
        let disabled = disabled || pending;
        let tokens = active_theme(cx).tokens();
        let cancel_slot = slot.clone();
        let cancel_store = store.clone();
        let save_slot = slot.clone();
        let save_store = store.clone();
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(
                Textarea::new(&state)
                    .h(px(120.) + (active_theme(cx).density().control - CONTROL_HEIGHT) * 6.)
                    .w_full()
                    .disabled(disabled),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap(tokens.spacing.sm)
                    .child(
                        button(
                            "connection-rules-cancel",
                            cancel.clone(),
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .disabled(disabled || !dirty)
                        .on_click(move |_, window, cx| {
                            let value = cancel_store.read(cx).daemon_str(KEY);
                            cancel_slot.update(cx, |editor, cx| {
                                editor.dirty = false;
                                editor.submitted = None;
                                editor.baseline = value.clone();
                                editor
                                    .state
                                    .update(cx, |state, cx| state.set_value(value, window, cx));
                            });
                            cancel_store.update(cx, |_, cx| cx.notify());
                        }),
                    )
                    .child(
                        loading_button(
                            "connection-rules-save",
                            save.clone(),
                            ButtonVariant::Primary,
                            pending,
                            cx,
                        )
                        .disabled(disabled || !dirty || unchanged)
                        .on_click(move |_, _, cx| {
                            let text = save_slot.read(cx).state.read(cx).value().to_string();
                            save_slot.update(cx, |editor, _| {
                                editor.submitted = Some(text.trim().to_owned())
                            });
                            save_store.update(cx, |store, cx| store.set_daemon(KEY, text, cx));
                        }),
                    ),
            )
            .into_any_element()
    });
    ctx.item("connectionRules", Some("connectionRulesDesc"), control)
        .vertical()
}
