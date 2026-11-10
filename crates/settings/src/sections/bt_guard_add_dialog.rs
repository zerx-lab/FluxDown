//! 手动添加封禁 / 限速 / 停止上传 / 白名单条目的对话框：IP 或 CIDR + 动作 + 时长 + 备注。
//!
//! CIDR 在本地先做格式校验（[`normalize_cidr`]）；过宽网段、主机位等取舍由 daemon 裁决，
//! 其错误原样回显在表单底部。

use fluxdown_protocol::{BtGuardEntriesAddParams, RpcErrorData, method};
use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, choice_chip, dialog_scroll_body, dialog_title, field_error,
    form, form_field, loading_button,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    WindowExt as _,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
};
use serde_json::Value;

use super::bt_guard::label_or_raw;
use crate::model::bt_guard::{ADD_ACTIONS, DurationChoice, normalize_cidr};
use crate::store::{SettingsStore, rpc_error_text};
use crate::ui::dialog_footer;

pub(crate) struct AddEntryDialog {
    store: Entity<SettingsStore>,
    translator: Translator,
    cidr: Entity<InputState>,
    reason: Entity<InputState>,
    action: &'static str,
    duration: DurationChoice,
    cidr_error: Option<SharedString>,
    form_error: Option<SharedString>,
    submitting: bool,
}

/// 打开添加对话框。
pub(crate) fn open(
    store: Entity<SettingsStore>,
    translator: Translator,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(translator.text("btGuardAddTitle").to_owned());
    let view = cx.new(|cx| AddEntryDialog::new(store, translator, window, cx));
    let cidr = view.read(cx).cidr.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(520.))
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
    cidr.update(cx, |input, cx| input.focus(window, cx));
}

impl AddEntryDialog {
    fn new(
        store: Entity<SettingsStore>,
        translator: Translator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = |key: &str| SharedString::from(translator.text(key).to_owned());
        let cidr =
            cx.new(|cx| InputState::new(window, cx).placeholder(text("btGuardAddCidrPlaceholder")));
        let reason = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(
            &cidr,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.cidr_error = None;
                    this.form_error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                _ => {}
            },
        )
        .detach();
        Self {
            store,
            translator,
            cidr,
            reason,
            action: ADD_ACTIONS[0],
            duration: DurationChoice::Forever,
            cidr_error: None,
            form_error: None,
            submitting: false,
        }
    }

    fn t(&self, key: &str) -> SharedString {
        SharedString::from(self.translator.text(key).to_owned())
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        let cidr = match normalize_cidr(&self.cidr.read(cx).value()) {
            Ok(cidr) => cidr,
            Err(error) => {
                self.cidr_error = Some(self.t(error.message_key()));
                cx.notify();
                return;
            }
        };
        let params = BtGuardEntriesAddParams {
            cidr: cidr.clone(),
            action: self.action.to_owned(),
            duration_secs: self.duration.secs(),
            reason: self.reason.read(cx).value().trim().to_owned(),
        };
        let Ok(params) = serde_json::to_value(&params) else {
            self.form_error = Some(self.t("btGuardAddFailed"));
            cx.notify();
            return;
        };
        let future = self
            .store
            .read(cx)
            .raw_call(method::DAEMON_BT_GUARD_ENTRIES_ADD, params);
        self.submitting = true;
        self.form_error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Value, RpcErrorData> = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.submitting = false;
                match result {
                    Ok(_) => {
                        this.store
                            .update(cx, |store, cx| store.bump_bt_guard_revision(cx));
                        window.close_dialog(cx);
                        window.push_notification(
                            Notification::success(
                                this.translator
                                    .text_with("btGuardAdded", &[("cidr", &cidr)]),
                            ),
                            cx,
                        );
                    }
                    Err(error) => {
                        this.form_error = Some(SharedString::from(format!(
                            "{}: {}",
                            this.translator.text("btGuardAddFailed"),
                            rpc_error_text(&this.translator, &error)
                        )));
                        cx.notify();
                    }
                }
            }) else {
                // 对话框已关闭，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    fn set_action(&mut self, action: &'static str, cx: &mut Context<Self>) {
        if self.action != action {
            self.action = action;
            cx.notify();
        }
    }

    fn set_duration(&mut self, duration: DurationChoice, cx: &mut Context<Self>) {
        if self.duration != duration {
            self.duration = duration;
            cx.notify();
        }
    }
}

impl Render for AddEntryDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let actions = h_flex()
            .flex_wrap()
            .gap(tokens.spacing.sm)
            .children(ADD_ACTIONS.iter().map(|action| {
                let action: &'static str = action;
                choice_chip(
                    SharedString::from(format!("bt-guard-add-action-{action}")),
                    label_or_raw(&self.translator, "btGuardAction", action),
                    self.action == action,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_action(action, cx);
                }))
            }));
        let durations =
            h_flex()
                .flex_wrap()
                .gap(tokens.spacing.sm)
                .children(DurationChoice::ALL.iter().map(|choice| {
                    let choice = *choice;
                    choice_chip(
                        SharedString::from(format!("bt-guard-add-duration-{}", choice.label_key())),
                        self.t(choice.label_key()),
                        self.duration == choice,
                        cx,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.set_duration(choice, cx);
                        },
                    ))
                }));
        let body = form(cx)
            .child(
                form_field(
                    self.t("btGuardAddCidr"),
                    Input::new(&self.cidr)
                        .control(cx)
                        .w_full()
                        .font_family(tokens.typography.mono.clone()),
                    None,
                    cx,
                )
                .when_some(self.cidr_error.clone(), |field, error| {
                    field.child(field_error(error, cx))
                }),
            )
            .child(form_field(self.t("btGuardAddAction"), actions, None, cx))
            .child(form_field(
                self.t("btGuardAddDuration"),
                durations,
                None,
                cx,
            ))
            .child(form_field(
                self.t("btGuardAddReason"),
                Input::new(&self.reason).control(cx).w_full(),
                None,
                cx,
            ));
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(dialog_scroll_body(
                "bt-guard-add-body",
                Some(px(480.)),
                body,
                cx,
            ))
            .when_some(self.form_error.clone(), |this, error| {
                this.child(field_error(error, cx))
            })
            .child(
                dialog_footer(
                    None,
                    [
                        Button::new("bt-guard-add-cancel")
                            .outline()
                            .label(self.t("cancel"))
                            .control(cx)
                            .on_click(|_, window, cx| window.close_dialog(cx))
                            .into_any_element(),
                        loading_button(
                            "bt-guard-add-confirm",
                            self.t("btGuardAddConfirm"),
                            ButtonVariant::Primary,
                            self.submitting,
                            cx,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.submit(window, cx);
                        }))
                        .into_any_element(),
                    ],
                    cx,
                )
                .pt(tokens.spacing.sm),
            )
    }
}
