//! 重置密码对话框（未登录，登录框「忘记密码？」进入）：邮箱 → 发码 → 验证码 + 新密码。
//!
//! 成功后云端吊销该账号全部会话且不自动登录：提示成功并回到登录对话框，邮箱回填账号框。

use std::sync::Arc;

use fluxdown_protocol::method;
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, dialog_title, field_error, form, form_field, input_with_action,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
    v_flex,
};

use super::email::is_valid_email;
use super::login::{self, parse_send_code};
use super::password::{PasswordIssue, check_new_password};
use super::{code_step, password_field};
use crate::errors::{ErrorContext, error_text};
use crate::verification::{CodeChallenge, spawn_ticker};
use crate::{AccountCommand, AccountPort, t};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    SendCode,
    Reset,
}

struct PasswordResetDialog {
    translator: Entity<Translator>,
    port: Arc<dyn AccountPort>,
    email: Entity<InputState>,
    code: Entity<InputState>,
    new_password: Entity<InputState>,
    confirm: Entity<InputState>,
    challenge: Option<CodeChallenge>,
    ticking: bool,
    /// 发码在途：只锁发码按钮（与收件邮箱）并转圈，其它输入与取消仍可用。
    sending: bool,
    /// 重置请求在途：锁定整个表单。
    submitting: bool,
    closed: bool,
    error: Option<SharedString>,
}

/// 打开重置密码对话框；`email` 非空时预填并直接聚焦发送按钮之后的验证码框。
pub(crate) fn open(
    translator: Entity<Translator>,
    port: Arc<dyn AccountPort>,
    email: String,
    window: &mut Window,
    cx: &mut App,
) {
    let title = t(translator.read(cx), "accountPasswordResetTitle");
    let prefilled = !email.is_empty();
    let view = cx.new(|cx| {
        let email_placeholder = t(translator.read(cx), "accountEmailPlaceholder");
        let email_input = cx.new(|cx| InputState::new(window, cx).placeholder(email_placeholder));
        if prefilled {
            email_input.update(cx, |input, cx| input.set_value(email, window, cx));
        }
        let code = cx.new(|cx| InputState::new(window, cx));
        let new_password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let confirm = cx.new(|cx| InputState::new(window, cx).masked(true));
        password_field::restrict_to_ascii(&new_password, window, cx);
        password_field::restrict_to_ascii(&confirm, window, cx);
        for input in [&email_input, &code, &new_password, &confirm] {
            cx.subscribe_in(
                input,
                window,
                |this: &mut PasswordResetDialog, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit(window, cx);
                    }
                    cx.notify();
                },
            )
            .detach();
        }
        PasswordResetDialog {
            translator,
            port,
            email: email_input,
            code,
            new_password,
            confirm,
            challenge: None,
            ticking: false,
            sending: false,
            submitting: false,
            closed: false,
            error: None,
        }
    });
    let focus = if prefilled {
        view.read(cx).code.clone()
    } else {
        view.read(cx).email.clone()
    };
    window.open_dialog(cx, {
        let view = view.clone();
        move |dialog, _, cx| {
            let submitting = view.read(cx).submitting;
            let on_close = view.clone();
            let content = view.clone();
            dialog
                .title(dialog_title(title.clone(), cx))
                .w(active_theme(cx).text_extent(480.))
                .close_button(!submitting)
                .keyboard(!submitting)
                // 点遮罩不关闭：误触会丢掉已输入的内容与验证码。
                .overlay_closable(false)
                // 回车由输入框 `PressEnter` 提交；默认 Confirm 会抢先关窗。
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| on_close.update(cx, |this, _| this.closed = true))
                .content(move |body, _, _| body.child(content.clone()))
        }
    });
    focus.update(cx, |input, cx| input.focus(window, cx));
}

impl PasswordResetDialog {
    fn text(&self, key: &str, cx: &App) -> SharedString {
        t(self.translator.read(cx), key)
    }

    fn email_value(&self, cx: &App) -> String {
        self.email.read(cx).value().trim().to_owned()
    }

    fn issue(&self, cx: &App) -> Option<PasswordIssue> {
        check_new_password(
            &self.new_password.read(cx).value(),
            &self.confirm.read(cx).value(),
            None,
        )
    }

    fn busy(&self) -> bool {
        self.sending || self.submitting
    }

    fn can_send(&self, cx: &App) -> bool {
        !self.busy()
            && !self.closed
            && is_valid_email(&self.email_value(cx))
            && self.challenge.is_none_or(|c| c.can_resend())
    }

    fn can_submit(&self, cx: &App) -> bool {
        !self.busy()
            && !self.closed
            && is_valid_email(&self.email_value(cx))
            && self.challenge.is_some_and(|c| !c.is_expired())
            && !self.code.read(cx).value().trim().is_empty()
            && self.issue(cx).is_none()
    }

    fn send_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let params = serde_json::json!({ "email": self.email_value(cx) });
        self.run(Operation::SendCode, params, window, cx);
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        let params = serde_json::json!({
            "email": self.email_value(cx),
            "code": self.code.read(cx).value().trim(),
            "newPassword": self.new_password.read(cx).value().to_string(),
        });
        self.run(Operation::Reset, params, window, cx);
    }

    fn start_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ticking {
            return;
        }
        self.ticking = true;
        spawn_ticker(window, cx, |this: &mut Self, _, cx| {
            let active = this.challenge.as_mut().is_some_and(|challenge| {
                challenge.tick();
                !challenge.is_idle()
            });
            this.ticking = active && !this.closed;
            cx.notify();
            this.ticking
        });
    }

    fn run(
        &mut self,
        operation: Operation,
        params: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match operation {
            Operation::SendCode => self.sending = true,
            Operation::Reset => self.submitting = true,
        }
        self.error = None;
        cx.notify();
        let method = match operation {
            Operation::SendCode => method::AGENT_AUTH_SEND_PASSWORD_RESET_CODE,
            Operation::Reset => method::AGENT_AUTH_RESET_PASSWORD,
        };
        let future = self.port.execute(AccountCommand::Auth { method, params });
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                match operation {
                    Operation::SendCode => this.sending = false,
                    Operation::Reset => this.submitting = false,
                }
                if this.closed {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(_) if operation == Operation::Reset => {
                        window.push_notification(
                            toast::success(this.text("accountPasswordResetSuccess", cx)),
                            cx,
                        );
                        this.closed = true;
                        let email = this.email_value(cx);
                        let translator = this.translator.clone();
                        let port = this.port.clone();
                        window.close_dialog(cx);
                        login::open_with_account(translator, port, email, window, cx);
                    }
                    Ok(value) => match parse_send_code(&value) {
                        Some(challenge) => {
                            this.challenge = Some(challenge);
                            this.code.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                                input.focus(window, cx);
                            });
                            this.start_ticker(window, cx);
                        }
                        None => this.error = Some(this.text("accountErrorUnknown", cx)),
                    },
                    Err(error) => {
                        this.error = Some(error_text(
                            this.translator.read(cx),
                            &error,
                            ErrorContext::Code,
                        ))
                    }
                }
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写。
                return;
            };
        })
        .detach();
    }
}

impl Render for PasswordResetDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let translator = self.translator.read(cx).clone();
        // 只有重置请求才锁输入框；发码在途时只锁收件邮箱，其它字段仍可先填。
        let disabled = self.submitting;
        let email_value = self.email_value(cx);
        let email_error = (!email_value.is_empty() && !is_valid_email(&email_value))
            .then(|| self.text("accountErrorInvalidEmail", cx));
        let issue = self.issue(cx);
        let new_error = (!self.new_password.read(cx).value().is_empty())
            .then_some(issue)
            .flatten()
            .filter(|issue| *issue != PasswordIssue::Mismatch)
            .map(|issue| self.text(issue.key(), cx));
        let confirm_error = (!self.confirm.read(cx).value().is_empty()
            && issue == Some(PasswordIssue::Mismatch))
        .then(|| self.text(PasswordIssue::Mismatch.key(), cx));
        let (send_label, send_enabled) = match &self.challenge {
            Some(challenge) => (
                code_step::resend_label(&translator, challenge),
                challenge.can_resend(),
            ),
            None => (t(&translator, "accountSendCode"), true),
        };
        let fields = form(cx)
            .child(form_field(
                self.text("accountEmailPlaceholder", cx),
                Input::new(&self.email)
                    .control(cx)
                    .disabled(disabled || self.sending)
                    .w_full(),
                email_error,
                cx,
            ))
            .child(form_field(
                self.text("accountFieldCode", cx),
                input_with_action(
                    Input::new(&self.code)
                        .control(cx)
                        .disabled(disabled)
                        .w_full(),
                    Button::new("account-password-reset-send-code")
                        .outline()
                        .label(send_label)
                        .control(cx)
                        .busy(self.sending)
                        .disabled(!self.sending && (!send_enabled || !self.can_send(cx)))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.send_code(window, cx);
                        })),
                    cx,
                ),
                self.challenge
                    .as_ref()
                    .map(|challenge| code_step::countdown_text(&translator, challenge)),
                cx,
            ))
            .child(form_field(
                self.text("accountPasswordNewPlaceholder", cx),
                Input::new(&self.new_password)
                    .control(cx)
                    .disabled(disabled)
                    .w_full()
                    .content_type(InputContentType::Password)
                    .mask_toggle(),
                new_error,
                cx,
            ))
            .child(form_field(
                self.text("accountPasswordConfirmPlaceholder", cx),
                Input::new(&self.confirm)
                    .control(cx)
                    .disabled(disabled)
                    .w_full()
                    .content_type(InputContentType::Password)
                    .mask_toggle(),
                confirm_error,
                cx,
            ));
        v_flex()
            .gap(tokens.spacing.lg)
            .child(fields)
            .when_some(self.error.clone(), |column, error| {
                column.child(field_error(error, cx))
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("account-password-reset-cancel")
                            .outline()
                            .label(self.text("cancel", cx))
                            .control(cx)
                            .disabled(self.submitting)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.closed = true;
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("account-password-reset-confirm")
                            .primary()
                            .label(self.text("confirm", cx))
                            .control(cx)
                            .busy(self.submitting)
                            .disabled(!self.submitting && !self.can_submit(cx))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
    }
}
