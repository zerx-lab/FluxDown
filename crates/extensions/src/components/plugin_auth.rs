//! 插件平台登录对话框：一次登录后凭据由引擎保存，后续插件请求自动复用。

use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{PluginAuthResponse, RpcErrorData};
use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, FluxIcon, dialog_scroll_body, field_error, form, form_field,
    toast,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, img, px,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};

use super::plugin_auth_challenge::{AuthChallenge, truncate_challenge_text};
use crate::controller::plugin_auth_call;
use crate::{ExtensionsPort, error_text, pages::Frame, ui};

pub struct PluginAuthDialog {
    translator: Entity<Translator>,
    port: Arc<dyn ExtensionsPort>,
    identity: String,
    site: Entity<InputState>,
    _site_subscription: Subscription,
    input: Entity<InputState>,
    session_id: String,
    auth_ref: String,
    status: String,
    challenge: AuthChallenge,
    message: Option<String>,
    message_is_error: bool,
    busy: bool,
    poll_task_active: bool,
    logout_pending: bool,
    /// 用户点击发起、仍在途的动作（`logout` / `poll` / `begin`）；只有对应按钮转圈，
    /// 状态刷新与自动轮询只置 `busy`（禁用按钮，不转圈）。
    action: Option<&'static str>,
}

impl PluginAuthDialog {
    pub fn new(
        translator: Entity<Translator>,
        port: Arc<dyn ExtensionsPort>,
        identity: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let site_placeholder = translator
            .read(cx)
            .text("pluginAuthSitePlaceholder")
            .to_owned();
        let input_placeholder = translator
            .read(cx)
            .text("pluginAuthInputPlaceholder")
            .to_owned();
        let site = cx.new(|cx| InputState::new(window, cx).placeholder(site_placeholder));
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(input_placeholder));
        let _site_subscription =
            cx.subscribe_in(&site, window, |this, input, event, window, cx| {
                if matches!(
                    event,
                    gpui_component::input::InputEvent::Blur
                        | gpui_component::input::InputEvent::PressEnter { .. }
                ) && !this.busy
                {
                    this.request_status(&input.read(cx).value(), window, cx);
                }
            });
        let dialog = Self {
            translator,
            port,
            identity,
            site,
            _site_subscription,
            input,
            session_id: String::new(),
            auth_ref: String::new(),
            status: String::new(),
            challenge: AuthChallenge::default(),
            message: None,
            message_is_error: false,
            busy: true,
            poll_task_active: false,
            logout_pending: false,
            action: None,
        };
        let status_future =
            plugin_auth_call(&dialog.port, &dialog.identity, "status", "", "", "", "");
        cx.spawn_in(window, async move |this, cx| {
            let result = status_future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                this.apply_result(result, false, window, cx);
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
        dialog
    }

    fn request_status(&mut self, site: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = true;
        self.message = None;
        let future = plugin_auth_call(&self.port, &self.identity, "status", site, "", "", "");
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                this.apply_result(result, false, window, cx);
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn call(&mut self, action: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.message = None;
        let identity = self.identity.clone();
        let site = self.site.read(cx).value().to_string();
        let input = self.input.read(cx).value().to_string();
        let session_id = self.session_id.clone();
        let notify_success = matches!(action, "begin" | "poll");
        let future = plugin_auth_call(
            &self.port,
            &identity,
            action,
            &site,
            &self.auth_ref,
            &session_id,
            &input,
        );
        self.logout_pending = action == "logout";
        self.action = Some(action);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                this.action = None;
                this.apply_result(result, notify_success, window, cx);
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 向引擎取消当前登录会话；无会话或忙碌时是 no-op。取消按钮与对话框被
    /// X / Esc / 点击遮罩关闭（不经过按钮，见 `open_plugin_auth` 里
    /// `Dialog::on_cancel` 的钩子）两条路径共用，保证任何关闭方式都不会
    /// 在引擎侧留下悬空的登录会话。
    pub(crate) fn cancel_session(&self, cx: &mut Context<Self>) {
        // 不看 `busy`：自动轮询每 2s 置一次 busy，若此时用户关闭对话框，
        // 仍必须通知引擎释放会话；cancel 是 fire-and-forget，与在途 poll 并存无害。
        if self.session_id.is_empty() {
            return;
        }
        let identity = self.identity.clone();
        let site = self.site.read(cx).value().to_string();
        let session_id = self.session_id.clone();
        let auth_ref = self.auth_ref.clone();
        let future = plugin_auth_call(
            &self.port,
            &identity,
            "cancel",
            &site,
            &auth_ref,
            &session_id,
            "",
        );
        cx.spawn(async move |_this, _cx| {
            if let Err(error) = future.await {
                // 对话框可能已关闭；取消会话失败只留诊断，不回写已释放的 UI。
                eprintln!(
                    "plugin authentication cancellation failed: {:?} ({:?})",
                    error.code, error.reason
                );
            }
        })
        .detach();
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_session(cx);
        window.close_dialog(cx);
    }

    fn apply_response(
        &mut self,
        response: PluginAuthResponse,
        notify_success: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 无条件先取出该标志：不管这次响应的 `status` 是什么，都不能把它带到
        // 下一次非 logout 的响应里误清凭据（logout 返回 error 时尤其如此）。
        let was_logout = std::mem::take(&mut self.logout_pending);
        self.status = response.status.clone();
        self.session_id = response.session_id;
        self.auth_ref = response.auth_ref.unwrap_or_default();
        self.challenge.apply(
            response.challenge,
            response.challenge_type,
            self.status == "pending",
        );
        self.message_is_error = self.status == "error";
        self.message = (!response.message.is_empty()).then_some(response.message);
        if was_logout {
            // logout 无论成功失败，引擎都已无条件删除本地档案；客户端同步清空
            // 登录态，不依赖插件回包内容——`status` 在 logout 语境下表示“注销
            // 请求本身是否成功”，不代表“是否仍处于登录状态”。渲染侧改用
            // `auth_ref` 非空联合判定是否已登录，清空后自然回落到「开始登录」。
            self.auth_ref.clear();
            self.session_id.clear();
            self.challenge.clear();
        }
        if self.status == "pending" && !self.session_id.is_empty() && self.challenge.is_qrcode() {
            self.start_polling(window, cx);
        }
        if notify_success && response.status == "success" {
            window.push_notification(
                toast::success(
                    self.translator
                        .read(cx)
                        .text("pluginAuthSuccess")
                        .to_owned(),
                ),
                cx,
            );
        }
    }

    /// QR 登录每两秒轮询一次；实体销毁后 `update` 失败，任务自然退出。
    fn start_polling(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.poll_task_active {
            return;
        }
        self.poll_task_active = true;
        let port = self.port.clone();
        let identity = self.identity.clone();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let Ok(Some(future)) = this.update(cx, |this, cx| {
                    if this.busy || this.session_id.is_empty() || !this.challenge.is_qrcode() {
                        this.poll_task_active = false;
                        return None;
                    }
                    this.busy = true;
                    cx.notify();
                    Some(plugin_auth_call(
                        &port,
                        &identity,
                        "poll",
                        this.site.read(cx).value().as_ref(),
                        &this.auth_ref,
                        &this.session_id,
                        this.input.read(cx).value().as_ref(),
                    ))
                }) else {
                    break;
                };
                let result = future.await;
                let Ok(()) = this.update_in(cx, |this, window, cx| {
                    this.busy = false;
                    this.apply_result(result, true, window, cx);
                    cx.notify();
                }) else {
                    break;
                };
            }
        })
        .detach();
    }

    fn apply_result(
        &mut self,
        result: Result<serde_json::Value, RpcErrorData>,
        notify_success: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(value) => match serde_json::from_value::<PluginAuthResponse>(value) {
                Ok(response) => self.apply_response(response, notify_success, window, cx),
                Err(_) => {
                    self.message_is_error = true;
                    self.message = Some(
                        self.translator
                            .read(cx)
                            .text("pluginAuthInvalidResponse")
                            .to_owned(),
                    )
                }
            },
            Err(error) => {
                self.message_is_error = true;
                let text = error_text(self.translator.read(cx), &error);
                self.message = Some(
                    self.translator
                        .read(cx)
                        .text_with("pluginAuthFailed", &[("message", &text)]),
                );
            }
        }
    }

    /// 复制挑战原文到剪贴板；展示侧的截断（见 [`truncate_challenge_text`]）
    /// 不影响复制的完整内容。
    fn copy_challenge(
        &self,
        value: String,
        copied_label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(value));
        window.push_notification(toast::success(copied_label), cx);
    }
}

impl Render for PluginAuthDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let translator = self.translator.read(cx);
        let frame = Frame {
            translator,
            tokens: theme.tokens(),
            extended: theme.extended(),
            components: theme.components(),
            stale: false,
        };
        let tokens = frame.tokens;
        let begin = translator.text("pluginAuthBegin").to_owned();
        let site_label = SharedString::from(translator.text("pluginAuthSiteLabel").to_owned());
        let input_label = SharedString::from(translator.text("pluginAuthInputLabel").to_owned());
        let poll = translator.text("pluginAuthPoll").to_owned();
        let cancel = translator.text("cancel").to_owned();
        let logout_label = translator.text("pluginAuthLogout").to_owned();
        let copy_label = translator.text("apiServiceCopy").to_owned();
        let copied_label = translator.text("apiServiceCopied").to_owned();
        // `status` 在不同 action 下语义不同（logout 的 "success" 是“注销成功”而
        // 非“已登录”），故是否已登录额外联合 `auth_ref` 非空判定；logout 完成
        // 后 `apply_response` 已清空 `auth_ref`，这里自然回落到「开始登录」。
        let is_logged_in = self.status == "success" && !self.auth_ref.is_empty();
        let is_qrcode = self.challenge.is_qrcode();
        let session_pending = self.status == "pending" && !self.session_id.is_empty();
        // 状态色：已登录 success、进行中 primary；其余不显示状态行。
        let status = if is_logged_in {
            Some((
                FluxIcon::CircleCheck,
                frame.extended.colors.success,
                translator.text("pluginAuthSuccess").to_owned(),
            ))
        } else if session_pending {
            Some((
                FluxIcon::Clock,
                tokens.colors.primary,
                translator.text("pluginAuthPending").to_owned(),
            ))
        } else {
            None
        };
        let challenge = self.challenge.value().map(str::to_owned);
        let challenge_type_label = self.challenge.kind().unwrap_or_default().to_owned();
        let challenge_image = self.challenge.image().cloned();
        let body = v_flex()
            .w_full()
            .gap(tokens.spacing.lg)
            .child(
                form(cx)
                    .child(ui::meta_text(
                        translator.text("pluginAuthDescription").to_owned(),
                        frame,
                    ))
                    .child(form_field(
                        site_label,
                        Input::new(&self.site)
                            .control(cx)
                            .w_full()
                            .disabled(self.busy),
                        None,
                        cx,
                    ))
                    .child(form_field(
                        input_label,
                        Input::new(&self.input)
                            .control(cx)
                            .w_full()
                            .disabled(self.busy),
                        None,
                        cx,
                    )),
            )
            .when_some(status, |this, (icon, color, text)| {
                this.child(
                    ui::status_line(icon, color, text, frame)
                        .text_size(tokens.typography.sm.size)
                        .text_color(tokens.colors.foreground),
                )
            })
            .when_some(challenge, |this, value| {
                // 挑战变化时已缓存图片；普通 qrcode 文本在本地编码，不访问登录 URL。
                let image = challenge_image;
                let fallback_text = image.is_none().then(|| truncate_challenge_text(&value));
                this.child(
                    v_flex()
                        .gap(tokens.spacing.xs)
                        .p(tokens.spacing.sm)
                        .rounded(tokens.radius.md)
                        .bg(tokens.colors.muted)
                        .child(ui::meta_text(challenge_type_label, frame))
                        .when_some(image, |this, image| {
                            this.child(
                                div()
                                    .w_full()
                                    .flex()
                                    .justify_center()
                                    .child(img(image).size(px(240.))),
                            )
                        })
                        .when_some(fallback_text, |this, text| {
                            this.child(
                                ui::meta_text(text, frame).text_color(tokens.colors.foreground),
                            )
                        })
                        .child({
                            let copy_value = value;
                            Button::new("plugin-auth-copy-challenge")
                                .outline()
                                .control(cx)
                                .icon(FluxIcon::Copy)
                                .label(copy_label)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.copy_challenge(
                                        copy_value.clone(),
                                        copied_label.clone(),
                                        window,
                                        cx,
                                    );
                                }))
                        }),
                )
            });
        // 表单与挑战（二维码）可能高过窗口：正文滚动，错误行与底栏常驻可见。
        v_flex()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(dialog_scroll_body("plugin-auth-body", None, body, cx))
            .when_some(self.message.clone(), |this, message| {
                if self.message_is_error {
                    this.child(field_error(message, cx))
                } else {
                    this.child(ui::meta_text(message, frame))
                }
            })
            .child(
                // 底栏右对齐：取消(outline) 在左、主操作在右，统一控件高度。
                h_flex()
                    .w_full()
                    .pt(tokens.spacing.sm)
                    .justify_end()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("plugin-auth-cancel")
                            .outline()
                            .control(cx)
                            .label(cancel)
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        div()
                            .when(is_logged_in, |this| {
                                this.child(
                                    Button::new("plugin-auth-logout")
                                        .outline()
                                        .control(cx)
                                        .label(logout_label)
                                        .busy(self.action == Some("logout"))
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.call("logout", window, cx);
                                        })),
                                )
                            })
                            .when(!is_logged_in && session_pending && !is_qrcode, |this| {
                                // 非二维码挑战没有自动轮询，提供手动「检查状态」。
                                this.child(
                                    Button::new("plugin-auth-poll")
                                        .primary()
                                        .control(cx)
                                        .label(poll)
                                        .busy(self.action == Some("poll"))
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.call("poll", window, cx);
                                        })),
                                )
                            })
                            .when(!is_logged_in && !session_pending, |this| {
                                // 凡非 success（已登录）/ pending（会话进行中）
                                // 均视为未登录，统一显示「开始登录」。
                                this.child(
                                    Button::new("plugin-auth-begin")
                                        .primary()
                                        .control(cx)
                                        .label(begin)
                                        .busy(self.action == Some("begin"))
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.call("begin", window, cx);
                                        })),
                                )
                            }),
                    ),
            )
    }
}
