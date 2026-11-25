//! 云端渠道连接 / 编辑对话框：名称、推送事件多选；邮件渠道有「收件邮箱 n/5」列表
//! （账号邮箱 + 经验证码验证的其他邮箱，内联添加表单）；Telegram 新建走一次性绑定码
//! （深链 + 二维码 + 2s 轮询绑定结果）。
//!
//! 保存可用性与收件列表规则在 `model::cloud_notify`（与 Web 端同一组样例）；本文件只负责状态与渲染。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{
    CloudNotifyChannelCreateParams, CloudNotifyChannelDto, CloudNotifyChannelUpdateParams,
    CloudNotifyEmailCodeParams, CloudNotifyEmailCodeResult, CloudNotifyEmailVerifyParams,
    CloudNotifyTelegramBindDto, CloudNotifyTelegramBindStatusDto, RpcErrorData, method,
};
use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, FluxIcon, IconControlExt as _, QrPalette, card, check_row,
    dialog_scroll_body, field_error, field_hint, form_field, input_with_action, qr_image,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Animation, AnimationExt as _, App, AppContext as _, ClickEvent, ClipboardItem, Context, Div,
    Entity, Image, InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, ease_in_out, img, prelude::FluentBuilder as _, relative,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    spinner::Spinner,
    v_flex,
};
use serde_json::{Value, json};

use super::cloud_push::failure_text;
use super::webhook::cloud_events;
use crate::model::cloud_notify::{
    AddMode, AddressIssue, BindPhase, ChannelKind, DEFAULT_EVENTS, MAX_RECIPIENTS, RecipientBadge,
    add_mode, add_recipient, address_issue, bind_phase, can_add_direct, can_add_more, can_remove,
    can_save, can_send_code, can_verify, code_minutes, email_can_save, initial_recipients,
    ordered_events, recipient_badge, remove_recipient,
};
use crate::store::SettingsStore;
use crate::ui::{dialog_footer, meta_text};

/// 对话框宽度档位：与 Webhook 端点对话框一致。
const DIALOG_WIDTH: f32 = 900.;
/// 正文区最大高度（超出在正文内滚动，标题 / 底栏常驻）。
const BODY_MAX_HEIGHT: f32 = 480.;
/// Telegram 绑定结果轮询间隔。
const BIND_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 连续轮询失败多少次后放弃并提示重试。
const BIND_POLL_MAX_FAILURES: u8 = 5;
/// 二维码显示边长档位（100% 字号基准，经 `text_extent` 缩放）。
const QR_DISPLAY_SIZE: f32 = 176.;
/// 内联添加表单的淡入时长。
const ADD_FORM_FADE: Duration = Duration::from_millis(180);

/// Telegram 绑定流程状态。
enum BindState {
    /// 非 Telegram 新建对话框。
    Idle,
    /// 正在向云端申请绑定码。
    Starting,
    /// 已出码，轮询等待用户在 Telegram 里点 Start。
    Waiting {
        code: String,
        deep_link: String,
        qr: Option<Arc<Image>>,
    },
    /// 绑定码过期。
    Expired,
    /// 申请 / 轮询失败（本地化文案）。
    Failed(SharedString),
}

pub(crate) struct CloudChannelDialog {
    store: Entity<SettingsStore>,
    translator: Translator,
    kind: ChannelKind,
    existing: Option<CloudNotifyChannelDto>,
    name: Entity<InputState>,
    /// 邮件收件列表（至多 5 个）与内联「添加邮箱」表单状态（地址 / 验证码输入、重发倒计时）。
    recipients: Vec<String>,
    adding: bool,
    /// 表单展开次数：用作淡入动画的 id，每次展开重放。
    form_generation: u64,
    address: Entity<InputState>,
    code: Entity<InputState>,
    /// 距离允许再次发送验证码的剩余秒数；0 = 可发送。
    cooldown: u32,
    /// 倒计时代际：重新发送 / 表单关闭 / 对话框关闭后旧的倒计时自行退出。
    cooldown_generation: u64,
    sending_code: bool,
    verifying: bool,
    /// 验证码已发送的提示 / 发送或验证失败的原因。
    code_notice: Option<SharedString>,
    code_error: Option<SharedString>,
    events: BTreeSet<String>,
    saving: bool,
    error: Option<SharedString>,
    bind: BindState,
    /// 绑定流程代际：重新生成 / 对话框关闭后旧的回调与轮询自行退出。
    bind_generation: u64,
    copied: bool,
}

/// 打开连接（`existing = None`）或编辑对话框。
pub(crate) fn open(
    store: Entity<SettingsStore>,
    translator: Translator,
    kind: ChannelKind,
    existing: Option<CloudNotifyChannelDto>,
    window: &mut Window,
    cx: &mut App,
) {
    let kind_name = translator.text(kind.name_key()).to_owned();
    let title = SharedString::from(translator.text_with(
        if existing.is_some() {
            "cloudNotifyEditTitle"
        } else {
            "cloudNotifyConnectTitle"
        },
        &[("kind", &kind_name)],
    ));
    let view = cx.new(|cx| CloudChannelDialog::new(store, translator, kind, existing, window, cx));
    let name = view.read(cx).name.clone();
    let telegram_create = view.read(cx).is_telegram_create();
    let dialog_view = view.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = dialog_view.clone();
        dialog
            .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(DIALOG_WIDTH))
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
    if telegram_create {
        view.update(cx, |this, cx| this.start_bind(window, cx));
    } else {
        name.update(cx, |input, cx| input.focus(window, cx));
    }
}

impl CloudChannelDialog {
    fn new(
        store: Entity<SettingsStore>,
        translator: Translator,
        kind: ChannelKind,
        existing: Option<CloudNotifyChannelDto>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let entry = existing.as_ref();
        let default_name = entry.map_or_else(
            || translator.text(kind.name_key()).to_owned(),
            |channel| channel.name.clone(),
        );
        let name = cx.new(|cx| InputState::new(window, cx).default_value(default_name));
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let events = entry.map_or_else(
            || {
                DEFAULT_EVENTS
                    .iter()
                    .map(|event| (*event).to_owned())
                    .collect()
            },
            |channel| channel.events.iter().cloned().collect(),
        );
        // 邮件：新建默认只有账号邮箱，编辑沿用渠道现有收件列表。
        let account_email = store
            .read(cx)
            .cloud_notify()
            .overview
            .as_ref()
            .map(|overview| overview.account_email.clone())
            .unwrap_or_default();
        let recipients = if kind == ChannelKind::Email {
            initial_recipients(entry, &account_email)
        } else {
            Vec::new()
        };
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("name@example.com"));
        cx.observe(&address, |_, _, cx| cx.notify()).detach();
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("123456"));
        cx.observe(&code, |_, _, cx| cx.notify()).detach();
        let bind = if kind == ChannelKind::Telegram && existing.is_none() {
            BindState::Starting
        } else {
            BindState::Idle
        };
        Self {
            store,
            translator,
            kind,
            existing,
            name,
            recipients,
            adding: false,
            form_generation: 0,
            address,
            code,
            cooldown: 0,
            cooldown_generation: 0,
            sending_code: false,
            verifying: false,
            code_notice: None,
            code_error: None,
            events,
            saving: false,
            error: None,
            bind,
            bind_generation: 0,
            copied: false,
        }
    }

    fn t(&self, key: &str) -> SharedString {
        SharedString::from(self.translator.text(key).to_owned())
    }

    fn is_telegram_create(&self) -> bool {
        self.kind == ChannelKind::Telegram && self.existing.is_none()
    }

    fn max_channels(&self, cx: &App) -> u32 {
        self.store
            .read(cx)
            .cloud_notify()
            .overview
            .as_ref()
            .map_or(0, |overview| overview.max_channels)
    }

    fn account_email(&self, cx: &App) -> String {
        self.store
            .read(cx)
            .cloud_notify()
            .overview
            .as_ref()
            .map(|overview| overview.account_email.clone())
            .unwrap_or_default()
    }

    fn can_save(&self, cx: &App) -> bool {
        let name = self.name.read(cx).value();
        if self.kind == ChannelKind::Email {
            email_can_save(&name, self.events.len(), &self.recipients)
        } else {
            can_save(&name, self.events.len())
        }
    }

    // ───────────────────────── Telegram 绑定 ─────────────────────────

    fn start_bind(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.bind_generation += 1;
        let generation = self.bind_generation;
        self.bind = BindState::Starting;
        self.copied = false;
        cx.notify();
        let future = self
            .store
            .read(cx)
            .raw_call(method::AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_START, json!({}));
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(parse::<CloudNotifyTelegramBindDto>);
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if this.bind_generation != generation {
                    return;
                }
                match result {
                    Ok(ticket) => {
                        // 二维码按当前主题配色（深码浅底）在出码时编码一次，渲染期只读。
                        let qr = qr_image(&ticket.deep_link, QrPalette::themed(cx)).map(Arc::new);
                        this.bind = BindState::Waiting {
                            code: ticket.code,
                            deep_link: ticket.deep_link,
                            qr,
                        };
                        this.poll_bind(generation, window, cx);
                    }
                    Err(error) => {
                        let text = failure_text(&this.translator, &error, this.max_channels(cx));
                        this.bind = BindState::Failed(SharedString::from(text));
                    }
                }
                cx.notify();
            }) else {
                // 对话框已关闭，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 每 2s 查询一次绑定结果：绑定成功自动关闭对话框，过期 / 连续失败转为可重试状态。
    fn poll_bind(&mut self, generation: u64, window: &mut Window, cx: &mut Context<Self>) {
        let BindState::Waiting { code, .. } = &self.bind else {
            return;
        };
        let code = code.clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut failures = 0u8;
            loop {
                cx.background_executor().timer(BIND_POLL_INTERVAL).await;
                let Ok(Some(future)) = this.update(cx, |this, cx| {
                    (this.bind_generation == generation
                        && matches!(this.bind, BindState::Waiting { .. }))
                    .then(|| {
                        this.store.read(cx).raw_call(
                            method::AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_STATUS,
                            json!({ "code": code }),
                        )
                    })
                }) else {
                    // 对话框已关闭或流程已被取代，结束轮询。
                    return;
                };
                let result = future
                    .await
                    .and_then(parse::<CloudNotifyTelegramBindStatusDto>);
                let Ok(finished) = this.update_in(cx, |this, window, cx| {
                    if this.bind_generation != generation {
                        return true;
                    }
                    match result {
                        Ok(status) => {
                            failures = 0;
                            match bind_phase(&status.status) {
                                BindPhase::Pending => false,
                                BindPhase::Bound => {
                                    // 云端 SSE 通常会触发刷新，这里主动拉一次，避免新卡片晚到。
                                    this.store.update(cx, |store, cx| {
                                        store.refresh_cloud_notify(true, cx)
                                    });
                                    window.close_dialog(cx);
                                    true
                                }
                                BindPhase::Expired => {
                                    this.bind = BindState::Expired;
                                    cx.notify();
                                    true
                                }
                            }
                        }
                        Err(error) => {
                            failures += 1;
                            if failures < BIND_POLL_MAX_FAILURES {
                                return false;
                            }
                            let text =
                                failure_text(&this.translator, &error, this.max_channels(cx));
                            this.bind = BindState::Failed(SharedString::from(text));
                            cx.notify();
                            true
                        }
                    }
                }) else {
                    // 对话框已关闭，结束轮询。
                    return;
                };
                if finished {
                    return;
                }
            }
        })
        .detach();
    }

    fn copy_link(&mut self, link: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(link.to_owned()));
        self.copied = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let Ok(()) = this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            }) else {
                // 对话框已关闭，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    // ───────────────────────── 保存 ─────────────────────────

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !self.can_save(cx) {
            return;
        }
        let events = ordered_events(&self.events, cloud_events().map(|(wire, _)| *wire));
        let name = self.name.read(cx).value().trim().to_owned();
        // 邮件保存整个收件列表；Telegram 没有收件地址。
        let is_email = self.kind == ChannelKind::Email;
        let (rpc, params) = match &self.existing {
            None => (
                method::AGENT_CLOUD_NOTIFY_CREATE_CHANNEL,
                serde_json::to_value(CloudNotifyChannelCreateParams {
                    kind: self.kind.wire().to_owned(),
                    name,
                    events,
                    addresses: if is_email {
                        self.recipients.clone()
                    } else {
                        Vec::new()
                    },
                }),
            ),
            Some(channel) => (
                method::AGENT_CLOUD_NOTIFY_UPDATE_CHANNEL,
                serde_json::to_value(CloudNotifyChannelUpdateParams {
                    id: channel.id.clone(),
                    name: Some(name),
                    enabled: None,
                    events: Some(events),
                    addresses: is_email.then(|| self.recipients.clone()),
                }),
            ),
        };
        let params = params.unwrap_or_else(|_| json!({}));
        let future = self.store.read(cx).raw_call(rpc, params);
        self.saving = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    // 概览由 agent 在 RPC 成功后刷新并推送，列表自行更新。
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => {
                        this.error = Some(SharedString::from(failure_text(
                            &this.translator,
                            &error,
                            this.max_channels(cx),
                        )));
                    }
                }
                cx.notify();
            }) else {
                // 对话框已关闭，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 展开内联添加表单（清空上一次的输入与状态）。
    fn open_add_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_add_form(window, cx);
        self.adding = true;
        self.form_generation += 1;
        self.address.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn close_add_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_add_form(window, cx);
        self.adding = false;
        cx.notify();
    }

    fn reset_add_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 倒计时代际 +1：让还在跑的旧倒计时自行退出。
        self.cooldown = 0;
        self.cooldown_generation += 1;
        self.code_notice = None;
        self.code_error = None;
        self.address
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.code
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    /// 账号邮箱被移除后加回：不需要验证码，直接入列。
    fn add_direct(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let account_email = self.account_email(cx);
        let address = self.address.read(cx).value().to_string();
        if add_recipient(&mut self.recipients, &address, &account_email) {
            self.close_add_form(window, cx);
        }
    }

    /// 向待添加的邮箱发验证码；成功后开始重发倒计时并提示有效期。
    fn send_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let account_email = self.account_email(cx);
        let address = self.address.read(cx).value().trim().to_owned();
        if !can_send_code(
            &address,
            &self.recipients,
            &account_email,
            self.cooldown,
            self.sending_code,
        ) {
            return;
        }
        let params = serde_json::to_value(CloudNotifyEmailCodeParams {
            address: address.clone(),
        })
        .unwrap_or_else(|_| json!({}));
        let future = self
            .store
            .read(cx)
            .raw_call(method::AGENT_CLOUD_NOTIFY_SEND_EMAIL_CODE, params);
        self.sending_code = true;
        self.code_error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(parse::<CloudNotifyEmailCodeResult>);
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.sending_code = false;
                match result {
                    Ok(sent) => {
                        this.code_notice = Some(SharedString::from(this.translator.text_with(
                            "cloudNotifyEmailCodeSent",
                            &[
                                ("email", &address),
                                ("minutes", &code_minutes(sent.expires_in_secs).to_string()),
                            ],
                        )));
                        this.start_cooldown(sent.resend_after_secs, window, cx);
                    }
                    Err(error) => {
                        this.code_notice = None;
                        this.code_error = Some(SharedString::from(failure_text(
                            &this.translator,
                            &error,
                            this.max_channels(cx),
                        )));
                    }
                }
                cx.notify();
            }) else {
                // 对话框已关闭，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 提交验证码：通过后该地址记为本账号已验证，加入列表并收起表单（可继续添加下一个）。
    fn verify(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let account_email = self.account_email(cx);
        let address = self.address.read(cx).value().trim().to_owned();
        let code = self.code.read(cx).value().trim().to_owned();
        if !can_verify(
            &address,
            &code,
            &self.recipients,
            &account_email,
            self.verifying,
        ) {
            return;
        }
        let params = serde_json::to_value(CloudNotifyEmailVerifyParams {
            address: address.clone(),
            code,
        })
        .unwrap_or_else(|_| json!({}));
        let future = self
            .store
            .read(cx)
            .raw_call(method::AGENT_CLOUD_NOTIFY_VERIFY_EMAIL, params);
        self.verifying = true;
        self.code_error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.verifying = false;
                match result {
                    Ok(_) => {
                        let account_email = this.account_email(cx);
                        add_recipient(&mut this.recipients, &address, &account_email);
                        this.close_add_form(window, cx);
                    }
                    Err(error) => {
                        this.code_error = Some(SharedString::from(failure_text(
                            &this.translator,
                            &error,
                            this.max_channels(cx),
                        )));
                    }
                }
                cx.notify();
            }) else {
                // 对话框已关闭，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 重发倒计时：每秒减一，到 0 或对话框关闭 / 被新的倒计时取代即退出。
    fn start_cooldown(&mut self, secs: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.cooldown = secs;
        self.cooldown_generation += 1;
        let generation = self.cooldown_generation;
        if secs == 0 {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(done) = this.update(cx, |this, cx| {
                    if this.cooldown_generation != generation {
                        return true;
                    }
                    this.cooldown = this.cooldown.saturating_sub(1);
                    cx.notify();
                    this.cooldown == 0
                }) else {
                    // 对话框已关闭，结束倒计时。
                    return;
                };
                if done {
                    return;
                }
            }
        })
        .detach();
    }

    // ───────────────────────── 渲染 ─────────────────────────

    fn render_name(&self, cx: &mut Context<Self>) -> Div {
        form_field(
            self.t("cloudNotifyChannelName"),
            Input::new(&self.name).control(cx).w_full(),
            None,
            cx,
        )
    }

    /// 邮件「收件邮箱 n/5」：列表（地址 + 徽标 + 移除）+「+ 添加邮箱」内联验证表单。
    /// Telegram 走绑定流程，没有这一块。
    fn render_kind_field(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.kind != ChannelKind::Email {
            return None;
        }
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().colors;
        let account_email = self.account_email(cx);
        let removable = can_remove(&self.recipients);
        let can_add = can_add_more(&self.recipients);

        let radius =
            (theme.components().card_radius - theme.extended().stroke.thin).max(gpui::px(0.));
        let mut list = card(cx).w_full().flex().flex_col();
        for (index, address) in self.recipients.iter().enumerate() {
            let (badge_key, badge_color) = match recipient_badge(address, &account_email) {
                RecipientBadge::Account => ("cloudNotifyEmailUseAccount", tokens.colors.foreground),
                RecipientBadge::Verified => ("cloudNotifyEmailVerified", extended.success),
            };
            let target = address.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("cloud-recipient-{index}")))
                    .w_full()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .pl(tokens.spacing.md)
                    .pr(tokens.spacing.xs)
                    .py(tokens.spacing.xs)
                    .when(index > 0, |row| {
                        row.border_t_1().border_color(extended.hairline)
                    })
                    .when(index == 0, |row| row.rounded_t(radius))
                    .when(index + 1 == self.recipients.len(), |row| {
                        row.rounded_b(radius)
                    })
                    .hover(move |style| style.bg(extended.row_hover))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(tokens.typography.sm.size)
                            .line_height(tokens.typography.sm.line_height)
                            .child(SharedString::from(address.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px(tokens.spacing.xs + tokens.spacing.xxs)
                            .py(tokens.spacing.xxs)
                            .rounded(tokens.radius.sm)
                            .bg(tokens.colors.muted)
                            .text_size(theme.extended().caption.size)
                            .line_height(theme.extended().caption.line_height)
                            .text_color(badge_color)
                            .child(self.t(badge_key)),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "cloud-recipient-remove-{index}"
                        )))
                        .ghost()
                        .icon(FluxIcon::X)
                        .control_icon(cx)
                        .tooltip(self.t("cloudNotifyEmailRemove"))
                        .disabled(!removable)
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                if remove_recipient(&mut this.recipients, &target) {
                                    cx.notify();
                                }
                            },
                        )),
                    ),
            );
        }

        let add_row = h_flex()
            .w_full()
            .items_center()
            .gap(tokens.spacing.sm)
            .child(
                Button::new("cloud-email-add")
                    .outline()
                    .icon(FluxIcon::Plus)
                    .label(self.t("cloudNotifyEmailAdd"))
                    .control(cx)
                    .disabled(!can_add || self.adding)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_add_form(window, cx);
                    })),
            )
            .when(!can_add, |row| {
                row.child(field_hint(
                    SharedString::from(self.translator.text_with(
                        "cloudNotifyEmailMaxReached",
                        &[("max", &MAX_RECIPIENTS.to_string())],
                    )),
                    cx,
                ))
            });

        let label = SharedString::from(self.translator.text_with(
            "cloudNotifyEmailRecipients",
            &[
                ("n", &self.recipients.len().to_string()),
                ("max", &MAX_RECIPIENTS.to_string()),
            ],
        ));
        let mut body = v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(list)
            .child(add_row);
        if self.adding {
            body = body.child(self.render_add_form(&account_email, cx));
        }
        Some(form_field(label, body, None, cx))
    }

    /// 内联添加表单：地址（即时校验）→（账号邮箱直接加回，其余）发验证码 → 验证码 → 验证并加入列表。
    /// 出现时淡入（每次展开重放）。
    fn render_add_form(
        &self,
        account_email: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = active_theme(cx).tokens().clone();
        let address = self.address.read(cx).value().to_string();
        let code = self.code.read(cx).value().to_string();
        let direct = add_mode(&address, account_email) == AddMode::Direct;
        let issue = address_issue(&address, &self.recipients);
        let can_send = can_send_code(
            &address,
            &self.recipients,
            account_email,
            self.cooldown,
            self.sending_code,
        );
        let can_verify = can_verify(
            &address,
            &code,
            &self.recipients,
            account_email,
            self.verifying,
        );
        let can_direct = can_add_direct(&address, &self.recipients, account_email);
        let send_label = if self.cooldown > 0 {
            SharedString::from(self.translator.text_with(
                "cloudNotifyEmailResendIn",
                &[("s", &self.cooldown.to_string())],
            ))
        } else {
            self.t("cloudNotifyEmailSendCode")
        };

        let address_field = {
            let field = form_field(
                self.t("cloudNotifyEmailAddress"),
                Input::new(&self.address).control(cx).w_full(),
                None,
                cx,
            );
            match issue {
                Some(AddressIssue::Invalid) => {
                    field.child(field_error(self.t("cloudNotifyEmailAddressInvalid"), cx))
                }
                Some(AddressIssue::Exists) => {
                    field.child(field_error(self.t("cloudNotifyEmailExists"), cx))
                }
                None if !direct => {
                    field.child(field_hint(self.t("cloudNotifyEmailVerifyHint"), cx))
                }
                None => field,
            }
        };

        let mut form = v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .p(tokens.spacing.md)
            .rounded(tokens.radius.md)
            .bg(tokens.colors.muted)
            .child(address_field);
        if !direct {
            let code_field = form_field(
                self.t("cloudNotifyEmailCode"),
                input_with_action(
                    Input::new(&self.code).control(cx).w_full(),
                    Button::new("cloud-email-send-code")
                        .outline()
                        .label(send_label)
                        .control(cx)
                        .busy(self.sending_code)
                        .disabled(!can_send)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.send_code(window, cx);
                        })),
                    cx,
                ),
                None,
                cx,
            );
            form = form.child(match (&self.code_error, &self.code_notice) {
                (Some(error), _) => code_field.child(field_error(error.clone(), cx)),
                (None, Some(notice)) => code_field.child(field_hint(notice.clone(), cx)),
                (None, None) => code_field,
            });
        }
        let confirm = if direct {
            Button::new("cloud-email-confirm")
                .primary()
                .label(self.t("cloudNotifyEmailAdd"))
                .control(cx)
                .disabled(!can_direct)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.add_direct(window, cx);
                }))
        } else {
            Button::new("cloud-email-confirm")
                .primary()
                .label(self.t("cloudNotifyEmailVerify"))
                .control(cx)
                .busy(self.verifying)
                .disabled(!can_verify)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.verify(window, cx);
                }))
        };
        form.child(
            h_flex()
                .w_full()
                .justify_end()
                .gap(tokens.spacing.sm)
                .child(
                    Button::new("cloud-email-cancel")
                        .outline()
                        .label(self.t("cancel"))
                        .control(cx)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.close_add_form(window, cx);
                        })),
                )
                .child(confirm),
        )
        .with_animation(
            SharedString::from(format!("cloud-email-add-form-{}", self.form_generation)),
            Animation::new(ADD_FORM_FADE).with_easing(ease_in_out),
            |form, delta| form.opacity(delta),
        )
    }

    fn render_events(&self, cx: &mut Context<Self>) -> Div {
        let tokens = active_theme(cx).tokens().clone();
        let view = cx.entity();
        let mut grid = div().flex().flex_wrap().w_full().p(tokens.spacing.xs);
        for (wire, label_key) in cloud_events() {
            let wire = *wire;
            let view = view.clone();
            grid = grid.child(div().w(relative(0.5)).min_w_0().child(check_row(
                SharedString::from(format!("cloud-event-{wire}")),
                self.events.contains(wire),
                self.t(label_key),
                move |checked, _, cx| {
                    view.update(cx, |this, cx| {
                        if checked {
                            this.events.insert(wire.to_owned());
                        } else {
                            this.events.remove(wire);
                        }
                        cx.notify();
                    });
                },
                cx,
            )));
        }
        let field = form_field(self.t("cloudNotifyEvents"), card(cx).child(grid), None, cx);
        if self.events.is_empty() {
            field.child(field_error(self.t("cloudNotifyEventsEmpty"), cx))
        } else {
            field
        }
    }

    fn render_telegram_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let mut panel = v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .child(meta_text(cx).child(self.t("cloudNotifyTelegramStep")));
        match &self.bind {
            BindState::Waiting { deep_link, qr, .. } => {
                let link = deep_link.clone();
                let open_link = link.clone();
                let copy_link = link.clone();
                let hairline = theme.extended().colors.hairline;
                let qr_size = theme.text_extent(QR_DISPLAY_SIZE);
                let details = v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(tokens.spacing.md)
                    .child(
                        card(cx)
                            .w_full()
                            .px(tokens.spacing.md)
                            .py(tokens.spacing.sm)
                            .font_family(tokens.typography.mono.clone())
                            .text_size(tokens.typography.xs.size)
                            .line_height(tokens.typography.xs.line_height)
                            .text_color(tokens.colors.foreground)
                            .child(SharedString::from(link)),
                    )
                    .child(
                        h_flex()
                            .gap(tokens.spacing.sm)
                            .child(
                                Button::new("cloud-telegram-open")
                                    .primary()
                                    .icon(FluxIcon::ExternalLink)
                                    .label(self.t("cloudNotifyTelegramOpen"))
                                    .control(cx)
                                    .on_click(move |_, _, cx| cx.open_url(&open_link)),
                            )
                            .child(
                                Button::new("cloud-telegram-copy")
                                    .outline()
                                    .icon(FluxIcon::Copy)
                                    .label(self.t(if self.copied {
                                        "webhookCopied"
                                    } else {
                                        "webhookCopy"
                                    }))
                                    .control(cx)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.copy_link(&copy_link, cx);
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(tokens.spacing.sm)
                            .child(Spinner::new())
                            .child(meta_text(cx).child(self.t("cloudNotifyTelegramWaiting"))),
                    );
                // 二维码本地编码并缓存在绑定状态里；编码失败（理论上不会）只剩深链与按钮。
                let qr = qr.clone().map(|image| {
                    div()
                        .flex_none()
                        .border_1()
                        .border_color(hairline)
                        .child(img(image).size(qr_size))
                });
                panel = panel.child(
                    h_flex()
                        .w_full()
                        .items_start()
                        .gap(tokens.spacing.lg)
                        .children(qr)
                        .child(details),
                );
            }
            BindState::Starting | BindState::Idle => {
                panel = panel.child(
                    h_flex()
                        .items_center()
                        .gap(tokens.spacing.sm)
                        .child(Spinner::new())
                        .child(meta_text(cx).child(self.t("cloudNotifyTelegramWaiting"))),
                );
            }
            BindState::Expired | BindState::Failed(_) => {
                let message = match &self.bind {
                    BindState::Failed(text) => text.clone(),
                    _ => self.t("cloudNotifyTelegramExpired"),
                };
                panel = panel.child(
                    h_flex()
                        .items_center()
                        .gap(tokens.spacing.sm)
                        .child(
                            meta_text(cx)
                                .text_color(tokens.colors.destructive)
                                .child(message),
                        )
                        .child(
                            Button::new("cloud-telegram-retry")
                                .outline()
                                .label(self.t("cloudNotifyTelegramRetry"))
                                .control(cx)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.start_bind(window, cx);
                                })),
                        ),
                );
            }
        }
        panel
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let tokens = active_theme(cx).tokens().clone();
        let can_save = self.can_save(cx);
        let telegram_create = self.is_telegram_create();
        let mut actions = vec![
            Button::new("cloud-channel-cancel")
                .outline()
                .label(self.t("cancel"))
                .control(cx)
                .on_click(|_, window, cx| window.close_dialog(cx))
                .into_any_element(),
        ];
        if !telegram_create {
            actions.push(
                Button::new("cloud-channel-save")
                    .primary()
                    .label(self.t("cloudNotifySave"))
                    .control(cx)
                    .busy(self.saving)
                    .disabled(!can_save || self.saving)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)))
                    .into_any_element(),
            );
        }
        let leading = self.error.clone().map(|text| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(tokens.typography.xs.size)
                .line_height(tokens.typography.xs.line_height)
                .text_color(tokens.colors.destructive)
                .child(text)
                .into_any_element()
        });
        dialog_footer(leading, actions, cx)
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, RpcErrorData> {
    serde_json::from_value(value)
        .map_err(|_| RpcErrorData::new(fluxdown_protocol::ApplicationErrorCode::Internal, false))
}

impl Render for CloudChannelDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let body = if self.is_telegram_create() {
            self.render_telegram_panel(cx)
        } else {
            let mut left = v_flex()
                .w_full()
                .gap(tokens.spacing.lg)
                .child(self.render_name(cx));
            if let Some(field) = self.render_kind_field(cx) {
                left = left.child(field);
            }
            left.child(self.render_events(cx))
        };
        v_flex()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(dialog_scroll_body(
                "cloud-channel-body",
                Some(active_theme(cx).text_extent(BODY_MAX_HEIGHT)),
                body,
                cx,
            ))
            .child(self.render_footer(cx))
    }
}
