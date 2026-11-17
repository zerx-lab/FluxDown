//! 推荐码对话框：创建（自定义 / 随机）与删除确认。

use fluxdown_protocol::method;
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, DialogIntent, button, dialog_footer, dialog_title, field_error,
    field_hint, loading_button,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, WeakEntity, Window,
};
use gpui_component::{
    WindowExt as _,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use super::ReferralView;
use crate::{
    AccountCommand,
    billing::{create_code_is_valid, normalize_code},
    errors::{ErrorContext, error_text},
    host::AccountHost,
    t,
};

const DIALOG_WIDTH: f32 = 440.;

struct CreateCodeDialog {
    host: Entity<AccountHost>,
    view: WeakEntity<ReferralView>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    busy: bool,
    closed: bool,
    _subscription: Subscription,
}

pub(super) fn open_create(
    host: &Entity<AccountHost>,
    view: WeakEntity<ReferralView>,
    window: &mut Window,
    cx: &mut App,
) {
    let host = host.clone();
    let dialog = cx.new(|cx| {
        let placeholder = t(
            host.read(cx).translator().read(cx),
            "accountReferralCodeCreatePlaceholder",
        );
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()));
        let _subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut CreateCodeDialog, input, event, window, cx| match event {
                InputEvent::Change => {
                    // 实时大写；已是大写时不回写，避免循环触发。
                    let value = input.read(cx).value().to_string();
                    let upper = value.to_uppercase();
                    if upper != value {
                        input.update(cx, |input, cx| input.set_value(upper, window, cx));
                    }
                    if this.error.take().is_some() {
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                _ => {}
            },
        );
        CreateCodeDialog {
            host,
            view,
            input,
            error: None,
            busy: false,
            closed: false,
            _subscription,
        }
    });
    let input = dialog.read(cx).input.clone();
    window.open_dialog(cx, move |dialog_builder, _, cx| {
        let state = dialog.read(cx);
        let busy = state.busy;
        let title = state.text("accountReferralCodeCreateTitle", cx);
        let content = dialog.clone();
        let closing = dialog.clone();
        dialog_builder
            .title(dialog_title(title, cx))
            .w(active_theme(cx).text_extent(DIALOG_WIDTH))
            .close_button(!busy)
            .overlay_closable(!busy)
            .keyboard(!busy)
            // 回车由输入框 `PressEnter` 提交；默认 Confirm 会抢先关窗。
            .on_ok(|_, _, _| false)
            .content(move |body, _, _| body.child(content.clone()))
            .on_close(move |_, _, cx| closing.update(cx, |this, _| this.closed = true))
    });
    input.update(cx, |input, cx| input.focus(window, cx));
}

impl CreateCodeDialog {
    fn text(&self, key: &str, cx: &App) -> SharedString {
        t(self.host.read(cx).translator().read(cx), key)
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closed {
            self.closed = true;
            window.close_dialog(cx);
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.closed {
            return;
        }
        let raw = self.input.read(cx).value();
        if !create_code_is_valid(&raw) {
            self.error = Some(self.text("accountReferralCodeCreateInvalid", cx));
            cx.notify();
            return;
        }
        let code = normalize_code(&raw);
        let params = if code.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({ "code": code })
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let future = self.host.read(cx).port().execute(AccountCommand::Referral {
            method: method::AGENT_REFERRAL_CREATE_CODE,
            params,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(_) => {
                        // 成功：关闭并刷新列表（新码出现在首位，回到第一页）。
                        this.close(window, cx);
                        if let Err(error) = this.view.update(cx, |view, cx| {
                            view.codes.page = 1;
                            view.load_codes(window, cx);
                        }) {
                            log::debug!("referral view released before refresh: {error:#}");
                        }
                    }
                    Err(error) => {
                        this.error = Some(error_text(
                            this.host.read(cx).translator().read(cx),
                            &error,
                            ErrorContext::General,
                        ));
                    }
                }
                cx.notify();
            }) else {
                // 对话框已释放，停止回写。
                return;
            };
        })
        .detach();
    }
}

impl Render for CreateCodeDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let spacing = active_theme(cx).tokens().spacing;
        v_flex()
            .w_full()
            .gap(spacing.md)
            .child(field_hint(
                self.text("accountReferralCodeCreateDesc", cx),
                cx,
            ))
            .child(
                Input::new(&self.input)
                    .control(cx)
                    .disabled(self.busy)
                    .w_full(),
            )
            .children(self.error.clone().map(|error| field_error(error, cx)))
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .justify_end()
                    .gap(spacing.sm)
                    .child(
                        button(
                            "referral-create-cancel",
                            self.text("cancel", cx),
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .disabled(self.busy)
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| {
                                this.close(window, cx);
                            },
                        )),
                    )
                    .child(
                        loading_button(
                            "referral-create-ok",
                            self.text("accountReferralCodeCreate", cx),
                            ButtonVariant::Primary,
                            self.busy,
                            cx,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| {
                                this.submit(window, cx);
                            },
                        )),
                    ),
            )
    }
}

/// 删除确认；确认后调用 `agent.referral.deleteCode`，成功刷新列表，失败 toast 错误文案。
pub(super) fn confirm_delete(
    host: &Entity<AccountHost>,
    view: WeakEntity<ReferralView>,
    id: String,
    window: &mut Window,
    cx: &mut App,
) {
    let translator = host.read(cx).translator().read(cx).clone();
    let title = t(&translator, "accountReferralCodeDeleteConfirmTitle");
    let description = t(&translator, "accountReferralCodeDeleteConfirmDesc");
    let ok = t(&translator, "delete");
    let cancel = t(&translator, "cancel");
    let host = host.clone();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let host = host.clone();
        let view = view.clone();
        let id = id.clone();
        alert
            .title(dialog_title(title.clone(), cx))
            .description(description.clone())
            .footer(dialog_footer(
                Some(cancel.clone()),
                ok.clone(),
                DialogIntent::Destructive,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let future = host.read(cx).port().execute(AccountCommand::Referral {
                    method: method::AGENT_REFERRAL_DELETE_CODE,
                    params: serde_json::json!({ "id": id }),
                });
                if let Err(error) =
                    view.update(cx, |view, cx| view.set_deleting(Some(id.clone()), cx))
                {
                    log::debug!("referral view released before delete: {error:#}");
                    return true;
                }
                let host = host.clone();
                let view = view.clone();
                window
                    .spawn(cx, async move |cx| {
                        let result = future.await;
                        let Ok(()) = cx.update(|window, cx| {
                            if let Err(error) = &result {
                                let message = error_text(
                                    host.read(cx).translator().read(cx),
                                    error,
                                    ErrorContext::General,
                                );
                                window.push_notification(toast::error(message), cx);
                            }
                            if let Err(error) = view.update(cx, |view, cx| {
                                view.set_deleting(None, cx);
                                if result.is_ok() {
                                    view.load_codes(window, cx);
                                }
                            }) {
                                log::debug!("referral view released after delete: {error:#}");
                            }
                        }) else {
                            // 窗口已释放，结束回调，不再更新状态。
                            return;
                        };
                    })
                    .detach();
                true
            })
    });
}
