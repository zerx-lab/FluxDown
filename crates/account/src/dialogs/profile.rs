//! Session-bound nickname and one-time Origin ID editors.

use fluxdown_protocol::{RpcErrorData, method};
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, check_row, dialog_title, field_error, field_hint, form,
    form_field,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use crate::{
    AccountCommand, PortFuture,
    errors::{ErrorContext, error_text},
    host::AccountHost,
    profile_edit::{self, EditLifetime, ProfileField},
    t,
};

#[derive(Clone, Copy, PartialEq)]
enum Operation {
    Idle,
    Suggest,
    Check(i64),
    Save(Option<i64>),
}

struct ProfileDialog {
    host: Entity<AccountHost>,
    lifetime: EditLifetime,
    input: Entity<InputState>,
    operation: Operation,
    revision: u64,
    confirmed: bool,
    error: Option<SharedString>,
}

pub(crate) fn open(
    host: &Entity<AccountHost>,
    field: ProfileField,
    window: &mut Window,
    cx: &mut App,
) {
    let controller = &host.read(cx).controller;
    let Some(session) = controller.session() else {
        return;
    };
    let lifetime = EditLifetime::new(session, field);
    if !lifetime.valid(Some(session), controller.is_stale(), None) {
        return;
    }
    let initial = match field {
        ProfileField::Nickname => session.user.nickname.clone(),
        ProfileField::OriginId => session
            .user
            .origin_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
    };
    let host = host.clone();
    let view = cx.new(|cx| {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        cx.subscribe_in(
            &input,
            window,
            |this: &mut ProfileDialog, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                InputEvent::Change => {
                    this.revision += 1;
                    this.confirmed = false;
                    this.error = None;
                    cx.notify();
                }
                _ => {}
            },
        )
        .detach();
        cx.observe_in(&host, window, |this: &mut ProfileDialog, _, window, cx| {
            if !this.lifetime.closed && !this.valid(cx) {
                this.close(window, cx);
            }
            cx.notify();
        })
        .detach();
        ProfileDialog {
            host,
            lifetime,
            input,
            operation: Operation::Idle,
            revision: 0,
            confirmed: false,
            error: None,
        }
    });
    let input = view.read(cx).input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let busy = view.read(cx).busy();
        let title = view.read(cx).text(
            match field {
                ProfileField::Nickname => "accountNicknameEditTitle",
                ProfileField::OriginId => "accountOriginIdEditTitle",
            },
            cx,
        );
        let content = view.clone();
        let closing = view.clone();
        dialog
            .title(dialog_title(title, cx))
            .w(active_theme(cx).text_extent(460.))
            .close_button(!busy)
            .overlay_closable(!busy)
            .keyboard(!busy)
            // 回车由输入框 `PressEnter` 提交；默认 Confirm 会抢先关窗。
            .on_ok(|_, _, _| false)
            .content(move |body, _, _| body.child(content.clone()))
            .on_close(move |_, _, cx| closing.update(cx, |this, _| this.lifetime.closed = true))
    });
    input.update(cx, |input, cx| input.focus(window, cx));
}

impl ProfileDialog {
    fn text(&self, key: &str, cx: &App) -> SharedString {
        t(self.host.read(cx).translator().read(cx), key)
    }

    fn busy(&self) -> bool {
        self.operation != Operation::Idle
    }

    fn valid(&self, cx: &App) -> bool {
        let controller = &self.host.read(cx).controller;
        let saving = match self.operation {
            Operation::Save(value) => value,
            _ => None,
        };
        self.lifetime
            .valid(controller.session(), controller.is_stale(), saving)
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.lifetime.closed {
            self.lifetime.closed = true;
            window.close_dialog(cx);
        }
    }

    fn fail(&mut self, key: &str, cx: &mut Context<Self>) {
        self.operation = Operation::Idle;
        self.error = Some(self.text(key, cx));
        cx.notify();
    }

    fn can_submit(&self, cx: &App) -> bool {
        !self.busy()
            && self.valid(cx)
            && (self.lifetime.field == ProfileField::Nickname || self.confirmed)
            && self
                .host
                .read(cx)
                .controller
                .session()
                .is_some_and(|session| {
                    !self
                        .lifetime
                        .field
                        .unchanged(&self.input.read(cx).value(), session)
                })
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Enter and click share confirmation, no-op and lifecycle guards.
        if !self.can_submit(cx) {
            return;
        }
        let raw = self.input.read(cx).value();
        match self.lifetime.field {
            ProfileField::Nickname => {
                let Some(value) = profile_edit::nickname(&raw) else {
                    self.fail("accountNicknameEditInvalid", cx);
                    return;
                };
                self.request(
                    Operation::Save(None),
                    method::AGENT_PROFILE_CHANGE_NICKNAME,
                    serde_json::json!({"nickname": value}),
                    window,
                    cx,
                );
            }
            ProfileField::OriginId => {
                let Some(value) = profile_edit::origin_id(&raw) else {
                    self.fail("accountOriginIdInvalid", cx);
                    return;
                };
                self.request(
                    Operation::Check(value),
                    method::AGENT_PROFILE_CHECK_ORIGIN_ID,
                    serde_json::json!({"value": value}),
                    window,
                    cx,
                );
            }
        }
    }

    fn suggest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || !self.valid(cx) {
            return;
        }
        self.confirmed = false;
        self.request(
            Operation::Suggest,
            method::AGENT_PROFILE_RANDOM_ORIGIN_ID,
            serde_json::json!({}),
            window,
            cx,
        );
    }

    fn request(
        &mut self,
        operation: Operation,
        method: &'static str,
        params: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operation = operation;
        self.error = None;
        let revision = self.revision;
        let future: PortFuture<serde_json::Value> = self
            .host
            .read(cx)
            .port()
            .execute(AccountCommand::Profile { method, params });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if !this.valid(cx) {
                    this.close(window, cx);
                    return;
                }
                if this.revision != revision || this.operation != operation {
                    this.operation = Operation::Idle;
                    cx.notify();
                    return;
                }
                this.complete(operation, result, window, cx);
            }) else {
                // The editor/window was released; its result must not affect another dialog.
                return;
            };
        })
        .detach();
    }

    fn complete(
        &mut self,
        operation: Operation,
        result: Result<serde_json::Value, RpcErrorData>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operation = Operation::Idle;
        match result {
            Err(error) => {
                self.error = Some(error_text(
                    self.host.read(cx).translator().read(cx),
                    &error,
                    ErrorContext::General,
                ))
            }
            Ok(value) => match operation {
                Operation::Suggest => match value
                    .get("originId")
                    .and_then(serde_json::Value::as_i64)
                    .filter(|id| *id >= 10000)
                {
                    Some(id) => {
                        self.confirmed = false;
                        self.input
                            .update(cx, |input, cx| input.set_value(id.to_string(), window, cx));
                    }
                    None => self.fail("accountErrorUnknown", cx),
                },
                Operation::Check(id) => {
                    match value.get("available").and_then(serde_json::Value::as_bool) {
                        Some(true) if self.confirmed && self.valid(cx) => {
                            self.request(
                                Operation::Save(Some(id)),
                                method::AGENT_PROFILE_CHANGE_ORIGIN_ID,
                                serde_json::json!({"originId": id}),
                                window,
                                cx,
                            );
                        }
                        Some(true) => self.fail("accountOriginIdEditWarning", cx),
                        Some(false) => self.fail(
                            if value.get("reason").and_then(serde_json::Value::as_str)
                                == Some("invalid")
                            {
                                "accountOriginIdInvalid"
                            } else {
                                "accountOriginIdErrorTaken"
                            },
                            cx,
                        ),
                        None => self.fail("accountErrorUnknown", cx),
                    }
                }
                Operation::Save(_) => {
                    let key = match self.lifetime.field {
                        ProfileField::Nickname => "accountNicknameEditSuccess",
                        ProfileField::OriginId => "accountOriginIdEditSuccess",
                    };
                    window.push_notification(toast::success(self.text(key, cx)), cx);
                    self.close(window, cx);
                }
                Operation::Idle => unreachable!("only in-flight operations receive results"),
            },
        }
        cx.notify();
    }
}

impl ProfileDialog {
    fn origin_options(&self, disabled: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let confirmation = cx.entity();
        let gap = active_theme(cx).tokens().spacing.lg;
        v_flex()
            .gap(gap)
            .child(
                Button::new("profile-origin-suggest")
                    .outline()
                    .label(self.text("accountOriginIdEditRoll", cx))
                    .control(cx)
                    .disabled(disabled)
                    .busy(self.operation == Operation::Suggest)
                    .on_click(cx.listener(|this, _, window, cx| this.suggest(window, cx))),
            )
            .child(
                check_row(
                    "profile-origin-confirm",
                    self.confirmed,
                    div()
                        .flex_1()
                        .child(self.text("accountOriginIdEditWarning", cx)),
                    move |checked, _, cx| {
                        confirmation.update(cx, |this, cx| {
                            if !this.busy() && this.valid(cx) {
                                this.confirmed = checked;
                                this.error = None;
                                cx.notify();
                            }
                        })
                    },
                    cx,
                )
                .when(disabled, |row| row.opacity(0.5)),
            )
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let gap = active_theme(cx).tokens().spacing.sm;
        let save_label = if self.lifetime.field == ProfileField::OriginId {
            "accountOriginIdEditConfirm"
        } else {
            "confirm"
        };
        h_flex()
            .w_full()
            .justify_end()
            .gap(gap)
            .child(
                Button::new("profile-edit-cancel")
                    .outline()
                    .label(self.text("cancel", cx))
                    .control(cx)
                    .disabled(self.busy())
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            )
            .child(
                Button::new("profile-edit-save")
                    .primary()
                    .label(self.text(save_label, cx))
                    .control(cx)
                    .busy(matches!(
                        self.operation,
                        Operation::Check(_) | Operation::Save(_)
                    ))
                    .disabled(!self.can_submit(cx))
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            )
    }
}

impl Render for ProfileDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let origin = self.lifetime.field == ProfileField::OriginId;
        let disabled = self.busy() || !self.valid(cx);
        let input_label = self.text(
            if origin {
                "accountOriginIdEditPlaceholder"
            } else {
                "accountNicknameEditTitle"
            },
            cx,
        );
        v_flex()
            .w_full()
            .gap(tokens.spacing.lg)
            .when(origin, |body| {
                body.child(field_hint(self.text("accountOriginIdEditDesc", cx), cx))
            })
            .child(
                form(cx).child(form_field(
                    input_label,
                    Input::new(&self.input)
                        .control(cx)
                        .disabled(disabled)
                        .w_full(),
                    None,
                    cx,
                )),
            )
            .when(origin, |body| body.child(self.origin_options(disabled, cx)))
            .when_some(self.error.clone(), |body, error| {
                body.child(field_error(error, cx))
            })
            .child(self.footer(cx))
    }
}
