//! 修改 / 设置密码对话框：当前密码或邮箱验证码二选一验证。
//!
//! 账号尚未设置密码（`hasPassword == Some(false)`）时只有验证码模式，标题为「设置密码」。
//! 成功后 agent 持久化最新资料并发布 `SessionChanged`，本对话框只负责提示并关闭。

use fluxdown_protocol::method;
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, dialog_title, field_error, field_hint, form, form_field,
    input_with_action,
};
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

use super::login::parse_send_code;
use super::{code_step, password_field};
use crate::errors::{ErrorContext, error_text};
use crate::host::AccountHost;
use crate::verification::{CodeChallenge, spawn_ticker};
use crate::{AccountCommand, t, t_with};

/// 新密码最少字符数（与注册、云端一致）。
const MIN_PASSWORD_CHARS: usize = 8;

/// 新密码输入的本地校验结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasswordIssue {
    TooShort,
    Mismatch,
    SameAsCurrent,
}

impl PasswordIssue {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::TooShort => "accountErrorPasswordTooShort",
            Self::Mismatch => "accountPasswordMismatch",
            Self::SameAsCurrent => "accountPasswordSameAsCurrent",
        }
    }
}

/// 校验新密码：长度按字符数计（≥8），两次输入一致，且（仅当前密码模式，`current` 为 `Some`）不同于旧密码。
pub(crate) fn check_new_password(
    new: &str,
    confirm: &str,
    current: Option<&str>,
) -> Option<PasswordIssue> {
    if new.chars().count() < MIN_PASSWORD_CHARS {
        Some(PasswordIssue::TooShort)
    } else if new != confirm {
        Some(PasswordIssue::Mismatch)
    } else if current.is_some_and(|current| current == new) {
        Some(PasswordIssue::SameAsCurrent)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    CurrentPassword,
    EmailCode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    SendCode,
    Confirm,
}

struct PasswordDialog {
    host: Entity<AccountHost>,
    user_id: String,
    email: String,
    /// 账号是否已有密码；`None`（旧云端未下发）按已设置处理。
    has_password: Option<bool>,
    mode: Mode,
    current: Entity<InputState>,
    code: Entity<InputState>,
    new_password: Entity<InputState>,
    confirm: Entity<InputState>,
    challenge: Option<CodeChallenge>,
    ticking: bool,
    /// 发码在途：只锁发码按钮并转圈，其它输入与取消仍可用。
    sending: bool,
    /// 确认修改在途：锁定整个表单。
    submitting: bool,
    closed: bool,
    invalidated: bool,
    error: Option<SharedString>,
}

fn password_input(window: &mut Window, cx: &mut App) -> Entity<InputState> {
    let input = cx.new(|cx| InputState::new(window, cx).masked(true));
    password_field::restrict_to_ascii(&input, window, cx);
    input
}

pub(crate) fn open(host: &Entity<AccountHost>, window: &mut Window, cx: &mut App) {
    let controller = &host.read(cx).controller;
    if controller.is_stale() {
        return;
    }
    let Some(session) = controller.session() else {
        return;
    };
    let user_id = session.user.id.clone();
    let email = session.user.email.clone();
    let has_password = session.user.has_password;
    let view = cx.new(|cx| {
        let current = password_input(window, cx);
        let new_password = password_input(window, cx);
        let confirm = password_input(window, cx);
        let code = cx.new(|cx| InputState::new(window, cx));
        for input in [&current, &code, &new_password, &confirm] {
            cx.subscribe_in(
                input,
                window,
                |this: &mut PasswordDialog, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit(window, cx);
                    }
                    cx.notify();
                },
            )
            .detach();
        }
        cx.observe(host, |this: &mut PasswordDialog, _, cx| {
            if !this.same_session(cx) {
                this.invalidated = true;
            }
            cx.notify();
        })
        .detach();
        PasswordDialog {
            host: host.clone(),
            user_id,
            email,
            has_password,
            mode: if has_password == Some(false) {
                Mode::EmailCode
            } else {
                Mode::CurrentPassword
            },
            current,
            code,
            new_password,
            confirm,
            challenge: None,
            ticking: false,
            sending: false,
            submitting: false,
            closed: false,
            invalidated: false,
            error: None,
        }
    });
    view.update(cx, |this, cx| this.resume_code(window, cx));
    let focus = view.read(cx).first_input();
    window.open_dialog(cx, {
        let view = view.clone();
        move |dialog, _, cx| {
            let submitting = view.read(cx).submitting;
            let title = view.read(cx).text(
                if view.read(cx).has_password == Some(false) {
                    "accountPasswordSetTitle"
                } else {
                    "accountPasswordChangeTitle"
                },
                cx,
            );
            let on_close = view.clone();
            let content = view.clone();
            dialog
                .title(dialog_title(title, cx))
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

impl PasswordDialog {
    fn text(&self, key: &str, cx: &App) -> SharedString {
        t(self.host.read(cx).translator().read(cx), key)
    }

    fn first_input(&self) -> Entity<InputState> {
        match self.mode {
            Mode::CurrentPassword => self.current.clone(),
            Mode::EmailCode => self.code.clone(),
        }
    }

    fn same_session(&self, cx: &App) -> bool {
        let controller = &self.host.read(cx).controller;
        !controller.is_stale()
            && controller
                .session()
                .is_some_and(|session| session.user.id == self.user_id)
    }

    fn enabled(&self, cx: &App) -> bool {
        !self.closed && !self.invalidated && self.same_session(cx)
    }

    fn busy(&self) -> bool {
        self.sending || self.submitting
    }

    fn code_key(&self) -> String {
        format!("password:{}", self.user_id)
    }

    /// 对话框关掉后重开：刚发过的验证码恢复倒计时，不重复发码。
    fn resume_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(challenge) = self.host.read(cx).sent_codes.restore(&self.code_key()) {
            self.challenge = Some(challenge);
            self.start_ticker(window, cx);
        }
    }

    fn issue(&self, cx: &App) -> Option<PasswordIssue> {
        let current = self.current.read(cx).value();
        check_new_password(
            &self.new_password.read(cx).value(),
            &self.confirm.read(cx).value(),
            (self.mode == Mode::CurrentPassword).then_some(current.as_ref()),
        )
    }

    fn can_send(&self, cx: &App) -> bool {
        !self.busy() && self.enabled(cx) && self.challenge.is_none_or(|c| c.can_resend())
    }

    fn can_submit(&self, cx: &App) -> bool {
        if self.busy() || !self.enabled(cx) || self.issue(cx).is_some() {
            return false;
        }
        match self.mode {
            Mode::CurrentPassword => !self.current.read(cx).value().is_empty(),
            Mode::EmailCode => {
                self.challenge.is_some_and(|c| !c.is_expired())
                    && !self.code.read(cx).value().trim().is_empty()
            }
        }
    }

    fn switch_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.mode == mode {
            return;
        }
        self.mode = mode;
        self.error = None;
        self.first_input()
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn send_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        self.run(Operation::SendCode, serde_json::json!({}), window, cx);
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        let new_password = self.new_password.read(cx).value().to_string();
        let params = match self.mode {
            Mode::CurrentPassword => serde_json::json!({
                "newPassword": new_password,
                "currentPassword": self.current.read(cx).value().to_string(),
            }),
            Mode::EmailCode => serde_json::json!({
                "newPassword": new_password,
                "code": self.code.read(cx).value().trim(),
            }),
        };
        self.run(Operation::Confirm, params, window, cx);
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
            Operation::Confirm => self.submitting = true,
        }
        self.error = None;
        cx.notify();
        let method = match operation {
            Operation::SendCode => method::AGENT_PROFILE_SEND_PASSWORD_CODE,
            Operation::Confirm => method::AGENT_PROFILE_CHANGE_PASSWORD,
        };
        let future = self
            .host
            .read(cx)
            .port()
            .execute(AccountCommand::Profile { method, params });
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                match operation {
                    Operation::SendCode => this.sending = false,
                    Operation::Confirm => this.submitting = false,
                }
                if !this.enabled(cx) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(_) if operation == Operation::Confirm => {
                        let key = this.code_key();
                        this.host.update(cx, |host, _| host.sent_codes.forget(&key));
                        window.push_notification(
                            toast::success(this.text("accountPasswordChangeSuccess", cx)),
                            cx,
                        );
                        this.closed = true;
                        window.close_dialog(cx);
                    }
                    Ok(value) => match parse_send_code(&value) {
                        Some(challenge) => {
                            let key = this.code_key();
                            let ttl = challenge.ttl_remaining;
                            this.host
                                .update(cx, |host, _| host.sent_codes.record(key, ttl));
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
                            this.host.read(cx).translator().read(cx),
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

impl PasswordDialog {
    fn render_fields(&self, disabled: bool, cx: &mut Context<Self>) -> gpui::Div {
        let translator = self.host.read(cx).translator().read(cx).clone();
        let issue = self.issue(cx);
        let new_error = (!self.new_password.read(cx).value().is_empty())
            .then_some(issue)
            .flatten()
            .filter(|issue| *issue != PasswordIssue::Mismatch)
            .map(|issue| self.text(issue.key(), cx));
        let confirm_error = (!self.confirm.read(cx).value().is_empty()
            && issue == Some(PasswordIssue::Mismatch))
        .then(|| self.text(PasswordIssue::Mismatch.key(), cx));
        let password = |state: &Entity<InputState>, cx: &mut Context<Self>| {
            Input::new(state)
                .control(cx)
                .disabled(disabled)
                .w_full()
                .content_type(InputContentType::Password)
                .mask_toggle()
        };
        let mut body = form(cx);
        match self.mode {
            Mode::CurrentPassword => {
                body = body.child(form_field(
                    self.text("accountPasswordCurrentPlaceholder", cx),
                    password(&self.current, cx),
                    None,
                    cx,
                ));
            }
            Mode::EmailCode => {
                let hint = if self.challenge.is_some() {
                    t_with(
                        &translator,
                        "accountPasswordCodeSubtitle",
                        &[("email", &self.email)],
                    )
                } else {
                    t_with(
                        &translator,
                        "accountPasswordCodeHint",
                        &[("email", &self.email)],
                    )
                };
                let (send_label, send_enabled) = match &self.challenge {
                    Some(challenge) => (
                        code_step::resend_label(&translator, challenge),
                        challenge.can_resend(),
                    ),
                    None => (t(&translator, "accountSendCode"), true),
                };
                body = body.child(field_hint(hint, cx)).child(form_field(
                    self.text("accountFieldCode", cx),
                    input_with_action(
                        Input::new(&self.code)
                            .control(cx)
                            .disabled(disabled)
                            .w_full(),
                        Button::new("account-password-send-code")
                            .outline()
                            .label(send_label)
                            .control(cx)
                            .busy(self.sending)
                            .disabled(
                                !self.sending && (disabled || !send_enabled || !self.can_send(cx)),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.send_code(window, cx);
                            })),
                        cx,
                    ),
                    self.challenge
                        .as_ref()
                        .map(|challenge| code_step::countdown_text(&translator, challenge)),
                    cx,
                ));
            }
        }
        body.child(form_field(
            self.text("accountPasswordNewPlaceholder", cx),
            password(&self.new_password, cx),
            new_error,
            cx,
        ))
        .child(form_field(
            self.text("accountPasswordConfirmPlaceholder", cx),
            password(&self.confirm, cx),
            confirm_error,
            cx,
        ))
    }

    fn render_footer(&self, disabled: bool, cx: &mut Context<Self>) -> impl IntoElement {
        // 切换验证方式的链接：只有「已有密码」的账号才能回到当前密码模式。
        let switch = match self.mode {
            Mode::CurrentPassword => Some((Mode::EmailCode, "accountPasswordUseEmailCode")),
            Mode::EmailCode if self.has_password != Some(false) => {
                Some((Mode::CurrentPassword, "accountPasswordUseCurrentPassword"))
            }
            Mode::EmailCode => None,
        };
        h_flex()
            .w_full()
            .justify_between()
            .gap(active_theme(cx).tokens().spacing.sm)
            .child(h_flex().when_some(switch, |row, (mode, key)| {
                row.child(
                    Button::new("account-password-switch")
                        .ghost()
                        .label(self.text(key, cx))
                        .control(cx)
                        .disabled(disabled || self.busy())
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.switch_mode(mode, window, cx);
                        })),
                )
            }))
            .child(
                h_flex()
                    .gap(active_theme(cx).tokens().spacing.sm)
                    .child(
                        Button::new("account-password-cancel")
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
                        Button::new("account-password-confirm")
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

impl Render for PasswordDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        // 只有确认请求才锁输入框；发码在途时仍可先填新密码。
        let disabled = self.submitting || !self.enabled(cx);
        v_flex()
            .gap(tokens.spacing.lg)
            .child(self.render_fields(disabled, cx))
            .when_some(self.error.clone(), |column, error| {
                column.child(field_error(error, cx))
            })
            .when(!self.enabled(cx), |column| {
                column.child(field_error(self.text("localServiceDisconnected", cx), cx))
            })
            .child(self.render_footer(disabled, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_boundary_is_eight_characters() {
        assert_eq!(
            check_new_password("1234567", "1234567", None),
            Some(PasswordIssue::TooShort)
        );
        assert_eq!(check_new_password("12345678", "12345678", None), None);
    }

    #[test]
    fn length_counts_chars_not_bytes() {
        // 4 个汉字 = 12 字节，但只有 4 个字符。
        assert_eq!(
            check_new_password("密码密码", "密码密码", None),
            Some(PasswordIssue::TooShort)
        );
        assert_eq!(
            check_new_password("密码密码密码密码", "密码密码密码密码", None),
            None
        );
    }

    #[test]
    fn mismatch_is_reported_after_length() {
        assert_eq!(
            check_new_password("12345678", "12345679", None),
            Some(PasswordIssue::Mismatch)
        );
        assert_eq!(
            check_new_password("short", "other", None),
            Some(PasswordIssue::TooShort)
        );
    }

    #[test]
    fn same_as_current_only_checked_when_current_given() {
        assert_eq!(
            check_new_password("12345678", "12345678", Some("12345678")),
            Some(PasswordIssue::SameAsCurrent)
        );
        assert_eq!(
            check_new_password("12345678", "12345678", Some("87654321")),
            None
        );
        assert_eq!(check_new_password("12345678", "12345678", None), None);
    }

    #[test]
    fn issue_keys_are_distinct() {
        assert_eq!(
            PasswordIssue::TooShort.key(),
            "accountErrorPasswordTooShort"
        );
        assert_eq!(PasswordIssue::Mismatch.key(), "accountPasswordMismatch");
        assert_eq!(
            PasswordIssue::SameAsCurrent.key(),
            "accountPasswordSameAsCurrent"
        );
    }
}
