//! 套餐购买对话框：选择套餐 → 扫码支付 → 成功。
//!
//! 打开时并行拉取套餐目录与资料（`purchaseCreditMinor`）；下单后本地编码微信二维码，
//! 每 3 秒轮询订单、每秒刷新倒计时。关闭对话框即终止全部定时任务（`flow` 代数自增 + `closed`）。

use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{
    CloudOrder, CloudPlan, CloudProfile, CloudReferralValidateResult, CloudSubscription,
    RpcErrorData, method,
};
use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, FluxIcon, QrPalette, button, caption_number, card,
    dialog_scroll_body, dialog_title, icon_button, loading_button, qr_image, tabular_numbers,
    tooltip_view,
};
use fluxdown_ui_theme::{SemanticThemeTokens, active_theme};
use gpui::{
    App, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, FontWeight, Image,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div, img,
    prelude::FluentBuilder as _, relative,
};
use gpui_component::{
    Icon, Sizable as _, WindowExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    skeleton::Skeleton,
    spinner::Spinner,
    v_flex,
};

use crate::{
    AccountCommand,
    billing::{
        CardState, PollAction, campaign_progress, card_state, format_countdown, format_local_date,
        format_minor, is_subscription_plan, normalize_code, owned_price, owned_sort, payable,
        plan_icon, poll_action, price_display, purchasable_plans, remaining_secs,
        validate_reason_key,
    },
    errors::{ErrorContext, error_text},
    host::AccountHost,
    link::now_unix_ms,
    pages::profile::{parse_hex_color, plan_badge},
    rpc::{decode, is_transient},
    t, t_with,
    ui::{NoticeTone, notice},
};

/// 订单轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// 倒计时刷新间隔。
const TICK_INTERVAL: Duration = Duration::from_secs(1);
/// 选择步骤套餐列表最大高度占窗口高度的比例。
const LIST_MAX_HEIGHT_RATIO: f32 = 0.48;
/// 选择步骤对话框宽度 / 支付与成功步骤对话框宽度（随字号缩放）。
const SELECT_WIDTH: f32 = 580.;
const NARROW_WIDTH: f32 = 420.;
/// 二维码显示边长。
const QR_SIZE: f32 = 208.;
/// 套餐图标块的浅底透明度。
const ICON_TINT_ALPHA: f32 = 0.14;
/// 选中卡片的柔和底色透明度。
const SELECTED_TINT_ALPHA: f32 = 0.08;
/// 不可选卡片的透明度。
const BLOCKED_OPACITY: f32 = 0.6;
/// 加载骨架卡片数量。
const SKELETON_CARDS: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Select,
    Paying,
    Success,
}

enum Catalog {
    Loading,
    Failed(SharedString),
    Ready,
}

struct Paying {
    order: CloudOrder,
    qr: Option<Arc<Image>>,
    now_ms: i64,
}

/// 下单流程里的失败：RPC 错误走账户错误映射，本地判定（邀请码无效等）直接给文案键。
enum PurchaseError {
    Rpc(RpcErrorData),
    Key(&'static str),
}

impl From<RpcErrorData> for PurchaseError {
    fn from(error: RpcErrorData) -> Self {
        Self::Rpc(error)
    }
}

enum Poll {
    Continue,
    Stop,
    Paid(Box<CloudOrder>),
}

struct PlanPurchaseDialog {
    host: Entity<AccountHost>,
    referral_input: Entity<InputState>,
    step: Step,
    catalog: Catalog,
    plans: Vec<CloudPlan>,
    credit_minor: i64,
    selected: Option<String>,
    notice: Option<(NoticeTone, SharedString)>,
    /// 邀请码校验 / 下单在途。
    busy: bool,
    paying: Option<Paying>,
    paid_plan: SharedString,
    closed: bool,
    /// 异步流程代数：任何新流程（加载 / 支付）或关闭都会自增，旧任务发现不一致即退出。
    flow: u64,
    _input_subscription: Subscription,
}

pub(crate) fn open(host: &Entity<AccountHost>, window: &mut Window, cx: &mut App) {
    if host.read(cx).controller.session().is_none() {
        return;
    }
    let host = host.clone();
    let view = cx.new(|cx| PlanPurchaseDialog::new(host, window, cx));
    view.update(cx, |this, cx| this.load(window, cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        let state = view.read(cx);
        let busy = state.busy;
        let title = state.text(
            match state.step {
                Step::Paying => "accountPlanPayingTitle",
                Step::Select | Step::Success => "accountPlanDialogTitle",
            },
            cx,
        );
        let width = match state.step {
            Step::Select => SELECT_WIDTH,
            Step::Paying | Step::Success => NARROW_WIDTH,
        };
        let content = view.clone();
        let closing = view.clone();
        dialog
            .title(dialog_title(title, cx))
            .w(active_theme(cx).text_extent(width))
            .close_button(!busy)
            .overlay_closable(!busy)
            .keyboard(!busy)
            .on_ok(|_, _, _| false)
            .content(move |body, _, _| body.min_h_0().child(content.clone()))
            .on_close(move |_, _, cx| {
                closing.update(cx, |this, _| {
                    this.closed = true;
                    this.flow += 1;
                });
            })
    });
}

impl PlanPurchaseDialog {
    fn new(host: Entity<AccountHost>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = t(
            host.read(cx).translator().read(cx),
            "accountPlanReferralCodePlaceholder",
        );
        let referral_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()));
        let _input_subscription = cx.subscribe_in(
            &referral_input,
            window,
            |this: &mut Self, input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    // 邀请码实时大写；已是大写时不回写，避免循环触发。
                    let value = input.read(cx).value().to_string();
                    let upper = value.to_uppercase();
                    if upper != value {
                        input.update(cx, |input, cx| input.set_value(upper, window, cx));
                    }
                    if this.notice.take().is_some() {
                        cx.notify();
                    }
                }
            },
        );
        // 登出后账号维度的流程失效：直接关闭对话框。
        cx.observe_in(&host, window, |this: &mut Self, host, window, cx| {
            if !this.closed && host.read(cx).controller.session().is_none() {
                this.close(window, cx);
            }
            cx.notify();
        })
        .detach();
        Self {
            host,
            referral_input,
            step: Step::Select,
            catalog: Catalog::Loading,
            plans: Vec::new(),
            credit_minor: 0,
            selected: None,
            notice: None,
            busy: false,
            paying: None,
            paid_plan: SharedString::default(),
            closed: false,
            flow: 0,
            _input_subscription,
        }
    }

    fn text(&self, key: &str, cx: &App) -> SharedString {
        t(self.host.read(cx).translator().read(cx), key)
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closed {
            self.closed = true;
            self.flow += 1;
            window.close_dialog(cx);
        }
    }

    /// 当前套餐以用户 `plan` 为准（`current_plan` 只是展示配置，可能缺失）。
    fn current_plan_code(&self, cx: &App) -> Option<String> {
        self.host
            .read(cx)
            .controller
            .session()
            .map(|session| session.user.plan.clone())
            .filter(|code| !code.is_empty())
    }

    /// 生效中的周期会员卡（来自 agent 会话；`/me` 刷新会更新）。
    fn active_subscription(&self, cx: &App) -> Option<CloudSubscription> {
        self.host
            .read(cx)
            .controller
            .session()
            .and_then(|session| session.active_subscription.clone())
    }

    /// 某套餐的卡片状态（统一取会话里的当前套餐 / 会员卡）。
    fn state_of_plan(&self, plan: &CloudPlan, cx: &App) -> CardState {
        let current = self.current_plan_code(cx);
        let owned = owned_price(&self.plans, current.as_deref());
        let base_sort = owned_sort(&self.plans, current.as_deref());
        card_state(
            plan,
            current.as_deref(),
            owned,
            self.credit_minor,
            self.active_subscription(cx).as_ref(),
            base_sort,
        )
    }

    fn device_id(&self, cx: &App) -> Option<String> {
        self.host
            .read(cx)
            .controller
            .session()
            .map(|session| session.device.device_id.clone())
    }

    fn fail_text(&self, error: &RpcErrorData, cx: &App) -> SharedString {
        error_text(
            self.host.read(cx).translator().read(cx),
            error,
            ErrorContext::General,
        )
    }

    // ───────────────────────── 加载 ─────────────────────────

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flow += 1;
        let flow = self.flow;
        self.catalog = Catalog::Loading;
        self.notice = None;
        cx.notify();
        let port = self.host.read(cx).port();
        let plans = port.execute(AccountCommand::Plan {
            method: method::AGENT_PLAN_LIST,
            params: serde_json::json!({}),
        });
        let profile = port.execute(AccountCommand::Auth {
            method: method::AGENT_AUTH_REFRESH_PROFILE,
            params: serde_json::json!({}),
        });
        cx.spawn_in(window, async move |this, cx| {
            // 两个请求并行：资料刷新放到后台执行器上先行启动。
            let profile = cx.background_executor().spawn(profile);
            let plans = match plans.await {
                Ok(value) => decode!(value => Vec<CloudPlan>),
                Err(error) => Err(error),
            };
            let profile = match profile.await {
                Ok(value) => decode!(value => CloudProfile),
                Err(error) => Err(error),
            };
            let Ok(()) = this.update(cx, |this, cx| {
                if this.closed || this.flow != flow {
                    return;
                }
                match plans {
                    Ok(plans) => {
                        this.plans = plans;
                        this.catalog = Catalog::Ready;
                    }
                    Err(error) => {
                        this.catalog = Catalog::Failed(this.fail_text(&error, cx));
                    }
                }
                this.credit_minor = match profile {
                    Ok(profile) => profile.purchase_credit_minor.max(0),
                    Err(error) => {
                        // 抵扣额取不到时按 0 展示；服务端下单时仍会按真实抵扣额计价。
                        log::warn!("refresh profile for plan dialog failed: {:?}", error.code);
                        0
                    }
                };
                let still_available = this.selected.as_ref().is_some_and(|code| {
                    this.card_state_of(code, cx)
                        .is_some_and(CardState::selectable)
                });
                if !still_available {
                    this.selected = None;
                }
                cx.notify();
            }) else {
                // 对话框已释放，停止回写。
                return;
            };
        })
        .detach();
    }

    fn card_state_of(&self, code: &str, cx: &App) -> Option<CardState> {
        let plan = self.plans.iter().find(|plan| plan.code == code)?;
        Some(self.state_of_plan(plan, cx))
    }

    fn select(&mut self, code: &str, cx: &mut Context<Self>) {
        if self.busy
            || !self
                .card_state_of(code, cx)
                .is_some_and(CardState::selectable)
        {
            return;
        }
        self.selected = Some(code.to_owned());
        self.notice = None;
        cx.notify();
    }

    // ───────────────────────── 下单 ─────────────────────────

    fn purchase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.step != Step::Select {
            return;
        }
        let Some(plan_code) = self.selected.clone() else {
            return;
        };
        let Some(device_id) = self.device_id(cx) else {
            return;
        };
        if !self
            .card_state_of(&plan_code, cx)
            .is_some_and(CardState::selectable)
        {
            return;
        }
        let referral = normalize_code(&self.referral_input.read(cx).value());
        self.busy = true;
        self.notice = None;
        cx.notify();
        let port = self.host.read(cx).port();
        self.flow += 1;
        let flow = self.flow;
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<CloudOrder, PurchaseError> = async {
                if !referral.is_empty() {
                    let value = port
                        .execute(AccountCommand::Referral {
                            method: method::AGENT_REFERRAL_VALIDATE,
                            params: serde_json::json!({
                                "code": referral,
                                "planCode": plan_code,
                            }),
                        })
                        .await?;
                    let validation = decode!(value => CloudReferralValidateResult)?;
                    if !validation.valid {
                        return Err(PurchaseError::Key(validate_reason_key(
                            validation.reason.as_deref(),
                        )));
                    }
                }
                let mut params = serde_json::json!({
                    "planCode": plan_code,
                    "deviceId": device_id,
                });
                if !referral.is_empty() {
                    params["referralCode"] = serde_json::Value::String(referral);
                }
                let value = port
                    .execute(AccountCommand::Order {
                        method: method::AGENT_ORDER_CREATE,
                        params,
                    })
                    .await?;
                Ok(decode!(value => CloudOrder)?)
            }
            .await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if this.closed || this.flow != flow {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(order) => this.enter_paying(order, window, cx),
                    Err(PurchaseError::Key(key)) => {
                        this.notice = Some((NoticeTone::Danger, this.text(key, cx)));
                    }
                    Err(PurchaseError::Rpc(error)) => {
                        this.notice = Some((NoticeTone::Danger, this.fail_text(&error, cx)));
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

    fn enter_paying(&mut self, order: CloudOrder, window: &mut Window, cx: &mut Context<Self>) {
        // 客户端订单固定微信：`codeUrl` 必须能本地编码成二维码，否则无法支付。
        let qr = order
            .code_url
            .as_deref()
            .and_then(|url| qr_image(url, QrPalette::MONOCHROME))
            .map(Arc::new);
        if qr.is_none() {
            self.notice = Some((NoticeTone::Danger, self.text("accountPlanErrorGateway", cx)));
            return;
        }
        self.step = Step::Paying;
        self.paying = Some(Paying {
            order,
            qr,
            now_ms: now_unix_ms(),
        });
        self.flow += 1;
        self.spawn_ticker(self.flow, window, cx);
        self.spawn_poller(self.flow, window, cx);
    }

    fn spawn_ticker(&self, flow: u64, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK_INTERVAL).await;
                let Ok(alive) = this.update(cx, |this, cx| {
                    if this.closed || this.flow != flow || this.step != Step::Paying {
                        return false;
                    }
                    if let Some(paying) = this.paying.as_mut() {
                        paying.now_ms = now_unix_ms();
                    }
                    cx.notify();
                    true
                }) else {
                    // 对话框已释放，结束倒计时。
                    return;
                };
                if !alive {
                    return;
                }
            }
        })
        .detach();
    }

    fn spawn_poller(&self, flow: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(order_no) = self
            .paying
            .as_ref()
            .map(|paying| paying.order.order_no.clone())
        else {
            return;
        };
        let port = self.host.read(cx).port();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let alive = this.update(cx, |this, _| {
                    !this.closed && this.flow == flow && this.step == Step::Paying
                });
                if !matches!(alive, Ok(true)) {
                    // 对话框已关闭 / 已离开支付步骤，结束轮询。
                    return;
                }
                let result = match port
                    .execute(AccountCommand::Order {
                        method: method::AGENT_ORDER_GET,
                        params: serde_json::json!({ "orderNo": order_no }),
                    })
                    .await
                {
                    Ok(value) => decode!(value => CloudOrder),
                    Err(error) => Err(error),
                };
                let Ok(poll) = this.update(cx, |this, cx| {
                    if this.closed || this.flow != flow {
                        return Poll::Stop;
                    }
                    match result {
                        Ok(order) => match poll_action(&order.status) {
                            PollAction::Paid => Poll::Paid(Box::new(order)),
                            PollAction::Ended => {
                                this.back_to_select(
                                    this.text("accountPlanOrderExpired", cx),
                                    NoticeTone::Warning,
                                    cx,
                                );
                                Poll::Stop
                            }
                            PollAction::Pending => Poll::Continue,
                        },
                        // 暂时性错误（断线 / 超时）继续下一轮；其它错误回到选择步骤。
                        Err(error) if is_transient(&error) => Poll::Continue,
                        Err(error) => {
                            this.back_to_select(this.fail_text(&error, cx), NoticeTone::Danger, cx);
                            Poll::Stop
                        }
                    }
                }) else {
                    return;
                };
                match poll {
                    Poll::Continue => {}
                    Poll::Stop => return,
                    Poll::Paid(order) => {
                        // 刷新资料让徽标 / 权益立即更新；失败只记录，不阻断成功提示。
                        let refreshed = port
                            .execute(AccountCommand::Auth {
                                method: method::AGENT_AUTH_REFRESH_PROFILE,
                                params: serde_json::json!({}),
                            })
                            .await;
                        if let Err(error) = refreshed {
                            log::warn!("refresh profile after payment failed: {:?}", error.code);
                        }
                        let Ok(()) = this.update(cx, |this, cx| {
                            if this.closed || this.flow != flow {
                                return;
                            }
                            this.flow += 1;
                            this.paid_plan = SharedString::from(if order.plan_name.is_empty() {
                                order.plan_code
                            } else {
                                order.plan_name
                            });
                            this.step = Step::Success;
                            cx.notify();
                        }) else {
                            return;
                        };
                        return;
                    }
                }
            }
        })
        .detach();
    }

    fn back_to_select(&mut self, text: SharedString, tone: NoticeTone, cx: &mut Context<Self>) {
        self.flow += 1;
        self.step = Step::Select;
        self.paying = None;
        self.busy = false;
        self.notice = Some((tone, text));
        cx.notify();
    }

    fn copy_order_no(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paying) = &self.paying else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(paying.order.order_no.clone()));
        window.push_notification(Notification::success(self.text("apiServiceCopied", cx)), cx);
    }
}

// ───────────────────────── 渲染 ─────────────────────────

impl Render for PlanPurchaseDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let body = match self.step {
            Step::Select => self.render_select(window, &tokens, cx).into_any_element(),
            Step::Paying => self.render_paying(&tokens, cx).into_any_element(),
            Step::Success => self.render_success(&tokens, cx).into_any_element(),
        };
        v_flex()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(body)
    }
}

impl PlanPurchaseDialog {
    fn render_select(
        &mut self,
        window: &mut Window,
        tokens: &SemanticThemeTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ready = matches!(self.catalog, Catalog::Ready);
        let visible: Vec<CloudPlan> = purchasable_plans(&self.plans)
            .into_iter()
            .cloned()
            .collect();
        let max_height = window.viewport_size().height * LIST_MAX_HEIGHT_RATIO;

        let list = match &self.catalog {
            Catalog::Loading => self.render_skeleton(tokens, cx).into_any_element(),
            Catalog::Failed(message) => self
                .render_failed(message.clone(), tokens, cx)
                .into_any_element(),
            Catalog::Ready if visible.is_empty() => {
                self.render_empty(tokens, cx).into_any_element()
            }
            Catalog::Ready => {
                let mut cards: Vec<gpui::AnyElement> = Vec::new();
                let (periodic, lifetime): (Vec<&CloudPlan>, Vec<&CloudPlan>) =
                    visible.iter().partition(|plan| is_subscription_plan(plan));
                let grouped = !periodic.is_empty() && !lifetime.is_empty();
                for (key, group) in [
                    ("accountPurchaseGroupLifetime", lifetime),
                    ("accountPurchaseGroupCards", periodic),
                ] {
                    if group.is_empty() {
                        continue;
                    }
                    if grouped {
                        cards.push(
                            div()
                                .w_full()
                                .text_size(tokens.typography.xs.size)
                                .line_height(tokens.typography.xs.line_height)
                                .text_color(tokens.colors.muted_foreground)
                                .child(self.text(key, cx))
                                .into_any_element(),
                        );
                    }
                    for plan in group {
                        cards.push(self.render_plan_card(plan, tokens, cx).into_any_element());
                    }
                }
                dialog_scroll_body(
                    "plan-list",
                    Some(max_height),
                    v_flex().w_full().gap(tokens.spacing.md).children(cards),
                    cx,
                )
                .into_any_element()
            }
        };

        v_flex()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.md)
            .child(list)
            .when(ready && !visible.is_empty(), |this| {
                this.child(
                    Input::new(&self.referral_input)
                        .control(cx)
                        .disabled(self.busy)
                        .w_full(),
                )
            })
            .when_some(self.notice.clone(), |this, (tone, text)| {
                this.child(notice(tone, text, cx))
            })
            .child(self.render_select_footer(tokens, cx))
    }

    fn render_select_footer(
        &self,
        tokens: &SemanticThemeTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let can_buy = matches!(self.catalog, Catalog::Ready) && self.selected.is_some();
        let mut buy_label = self.text("accountPlanBuy", cx).to_string();
        if let Some(plan) = self
            .selected
            .as_ref()
            .and_then(|code| self.plans.iter().find(|plan| &plan.code == code))
        {
            if self.state_of_plan(plan, cx) == CardState::Renewable {
                buy_label = self.text("accountRenew", cx).to_string();
            }
            let credit = if is_subscription_plan(plan) {
                0
            } else {
                self.credit_minor
            };
            buy_label = format!(
                "{buy_label} · {}",
                format_minor(payable(plan, credit), &plan.currency)
            );
        }
        h_flex()
            .w_full()
            .items_center()
            .gap(tokens.spacing.sm)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .text_color(tokens.colors.muted_foreground)
                    .child(self.text("accountPlanOneTime", cx)),
            )
            .child(
                button(
                    "plan-cancel",
                    self.text("cancel", cx),
                    ButtonVariant::Secondary,
                    cx,
                )
                .disabled(self.busy)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))),
            )
            .child(
                loading_button("plan-buy", buy_label, ButtonVariant::Primary, self.busy, cx)
                    .disabled(self.busy || !can_buy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.purchase(window, cx);
                    })),
            )
    }

    fn render_skeleton(&self, tokens: &SemanticThemeTokens, cx: &App) -> impl IntoElement {
        let theme = active_theme(cx);
        let icon_block = theme.extended().icon.lg * 2.;
        let line = tokens.typography.sm.line_height;
        let cards: Vec<_> = (0..SKELETON_CARDS)
            .map(|_| {
                card(cx)
                    .w_full()
                    .p(tokens.spacing.md)
                    .flex()
                    .items_center()
                    .gap(tokens.spacing.md)
                    .child(
                        Skeleton::new()
                            .size(icon_block)
                            .rounded(tokens.radius.md)
                            .flex_none(),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(tokens.spacing.sm)
                            .child(
                                Skeleton::new()
                                    .w(theme.text_extent(120.))
                                    .h(line)
                                    .rounded(tokens.radius.sm),
                            )
                            .child(
                                Skeleton::new()
                                    .w(theme.text_extent(220.))
                                    .h(tokens.typography.xs.line_height)
                                    .rounded(tokens.radius.sm),
                            ),
                    )
                    .child(
                        Skeleton::new()
                            .w(theme.text_extent(72.))
                            .h(tokens.typography.lg.line_height)
                            .rounded(tokens.radius.sm),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex().w_full().gap(tokens.spacing.md).children(cards)
    }

    fn render_failed(
        &self,
        message: SharedString,
        tokens: &SemanticThemeTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .items_center()
            .gap(tokens.spacing.md)
            .py(tokens.spacing.xl)
            .child(notice(
                NoticeTone::Danger,
                self.text("accountPlanLoadFailed", cx),
                cx,
            ))
            .child(
                div()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .text_color(tokens.colors.muted_foreground)
                    .child(message),
            )
            .child(
                button(
                    "plan-retry",
                    self.text("accountDevicesRetry", cx),
                    ButtonVariant::Secondary,
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.load(window, cx))),
            )
    }

    fn render_empty(&self, tokens: &SemanticThemeTokens, cx: &App) -> impl IntoElement {
        let extended = active_theme(cx).extended().clone();
        v_flex()
            .w_full()
            .items_center()
            .gap(tokens.spacing.sm)
            .py(tokens.spacing.xl)
            .child(
                Icon::new(FluxIcon::Package)
                    .size(extended.icon.lg * 2.)
                    .text_color(extended.colors.text_tertiary),
            )
            .child(
                div()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .text_color(tokens.colors.muted_foreground)
                    .child(self.text("accountPlanEmpty", cx)),
            )
    }

    /// 套餐卡片：左图标块 / 中名称 + 标签 + 描述 + 亮点 + 活动进度 / 右价格区。
    fn render_plan_card(
        &self,
        plan: &CloudPlan,
        tokens: &SemanticThemeTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = active_theme(cx);
        let extended = theme.extended().clone();
        let components = theme.components();
        let accent = tokens.colors.accent_foreground;
        let plan_color = parse_hex_color(&plan.color).unwrap_or(accent);
        let state = self.state_of_plan(plan, cx);
        let subscription_conflict = is_subscription_plan(plan)
            && state == CardState::TierBlocked
            && self.active_subscription(cx).is_some();
        let conflict_tip = self.text("errReasonSubscriptionConflict", cx);
        let period_tag = plan.billing_period_days.map(|days| {
            t_with(
                self.host.read(cx).translator().read(cx),
                "accountPeriodDays",
                &[("days", &days.to_string())],
            )
        });
        let selectable = state.selectable();
        let selected = self.selected.as_deref() == Some(plan.code.as_str());
        let membership = self
            .host
            .read(cx)
            .controller
            .session()
            .and_then(|session| session.user.membership_ordinal);
        let badge = plan_badge(tokens, &extended, plan, membership);
        let price = price_display(plan, state, self.credit_minor);
        let progress = campaign_progress(plan);
        let ends = plan
            .campaign
            .as_ref()
            .and_then(|campaign| campaign.end_at.as_deref())
            .filter(|end| !end.is_empty())
            .map(format_local_date);
        let code = plan.code.clone();
        let text_sm = tokens.typography.sm;
        let text_xs = tokens.typography.xs;

        let state_tag = match state {
            CardState::Current | CardState::Renewable => {
                Some((self.text("accountPlanCurrent", cx), accent))
            }
            CardState::TierBlocked | CardState::CreditBlocked => Some((
                self.text("accountPlanLowerTier", cx),
                tokens.colors.muted_foreground,
            )),
            CardState::Available => None,
        };

        let highlights: Vec<_> = plan
            .highlights
            .iter()
            .map(|item| {
                h_flex()
                    .items_start()
                    .gap(tokens.spacing.xs)
                    .child(
                        div()
                            .flex_none()
                            .h(text_xs.line_height)
                            .flex()
                            .items_center()
                            .child(
                                Icon::new(FluxIcon::Check)
                                    .size(extended.icon.sm)
                                    .text_color(extended.colors.success),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(text_xs.size)
                            .line_height(text_xs.line_height)
                            .text_color(tokens.colors.foreground)
                            .child(item.clone()),
                    )
                    .into_any_element()
            })
            .collect();

        let campaign_block = (progress.is_some() || ends.is_some()).then(|| {
            let mut block = v_flex().w_full().gap(tokens.spacing.xs);
            if let Some((sold, quota)) = progress {
                let ratio = if quota > 0 {
                    (sold as f32 / quota as f32).clamp(0., 1.)
                } else {
                    1.
                };
                block = block.child(
                    div()
                        .w_full()
                        .h(tokens.spacing.xs)
                        .rounded(tokens.radius.full)
                        .overflow_hidden()
                        .bg(extended.colors.progress_track)
                        .child(
                            div()
                                .h_full()
                                .w(relative(ratio))
                                .rounded(tokens.radius.full)
                                .bg(extended.colors.progress_fill),
                        ),
                );
            }
            let mut caption = Vec::new();
            if let Some((sold, quota)) = progress {
                caption.push(
                    t_with(
                        self.host.read(cx).translator().read(cx),
                        "accountPlanLimitedSold",
                        &[("sold", &sold.to_string()), ("quota", &quota.to_string())],
                    )
                    .to_string(),
                );
            }
            if let Some(date) = &ends {
                caption.push(
                    t_with(
                        self.host.read(cx).translator().read(cx),
                        "accountPlanCampaignEnds",
                        &[("date", date)],
                    )
                    .to_string(),
                );
            }
            block.child(
                caption_number(caption.join(" · "), cx).text_color(tokens.colors.muted_foreground),
            )
        });

        let price_column = {
            let mut column = v_flex()
                .flex_none()
                .items_end()
                .gap(tokens.spacing.xxs)
                .child(
                    div()
                        .text_size(tokens.typography.lg.size)
                        .line_height(tokens.typography.lg.line_height)
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_features(tabular_numbers())
                        .text_color(tokens.colors.foreground)
                        .child(price.main.clone()),
                );
            if let Some(strike) = &price.strike {
                column = column.child(
                    div()
                        .text_size(text_xs.size)
                        .line_height(text_xs.line_height)
                        .font_features(tabular_numbers())
                        .line_through()
                        .text_color(extended.colors.text_tertiary)
                        .child(strike.clone()),
                );
            }
            if let Some(credit) = &price.credit {
                column = column.child(
                    div()
                        .text_size(text_xs.size)
                        .line_height(text_xs.line_height)
                        .text_color(extended.colors.success)
                        .child(t_with(
                            self.host.read(cx).translator().read(cx),
                            "accountPlanCreditApplied",
                            &[("amount", credit)],
                        )),
                );
            }
            if let Some(label) = &price.stage_label {
                column = column.child(
                    div()
                        .text_size(text_xs.size)
                        .line_height(text_xs.line_height)
                        .text_color(tokens.colors.muted_foreground)
                        .child(label.clone()),
                );
            }
            column
        };

        let selected_border = accent;
        let hairline = extended.colors.hairline;
        let row_hover = extended.colors.row_hover;
        div()
            .id(SharedString::from(format!("plan-card-{}", plan.code)))
            .w_full()
            .flex()
            .items_start()
            .gap(tokens.spacing.md)
            .p(tokens.spacing.md)
            .rounded(components.card_radius)
            .border(extended.stroke.thin)
            .border_color(if selected { selected_border } else { hairline })
            .bg(if selected {
                accent.opacity(SELECTED_TINT_ALPHA)
            } else {
                tokens.colors.surface
            })
            .when(!selectable, |this| this.opacity(BLOCKED_OPACITY))
            .when(subscription_conflict, |this| {
                this.tooltip(move |window, cx| tooltip_view(conflict_tip.clone(), window, cx))
            })
            .when(selectable && !selected, |this| {
                this.hover(move |style| style.bg(row_hover))
            })
            .when(selectable, |this| {
                this.cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select(&code, cx);
                    }))
            })
            .child(
                div()
                    .flex_none()
                    .size(extended.icon.lg * 2.)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(tokens.radius.md)
                    .bg(plan_color.opacity(ICON_TINT_ALPHA))
                    .child(
                        Icon::new(plan_icon(&plan.icon))
                            .size(extended.icon.lg)
                            .text_color(plan_color),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(tokens.spacing.xs)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(tokens.spacing.sm)
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(text_sm.size)
                                    .line_height(text_sm.line_height)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(tokens.colors.foreground)
                                    .child(plan.name.clone()),
                            )
                            .when_some(badge, |this, badge| this.child(badge))
                            .when_some(period_tag, |this, label| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .text_size(extended.caption.size)
                                        .line_height(extended.caption.line_height)
                                        .text_color(tokens.colors.muted_foreground)
                                        .child(label),
                                )
                            })
                            .when_some(state_tag, |this, (label, color)| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .px(tokens.spacing.sm)
                                        .py(tokens.spacing.xxs)
                                        .rounded(components.badge_radius)
                                        .bg(color.opacity(ICON_TINT_ALPHA))
                                        .text_size(extended.caption.size)
                                        .line_height(extended.caption.line_height)
                                        .text_color(color)
                                        .child(label),
                                )
                            }),
                    )
                    .when(!plan.description.is_empty(), |this| {
                        this.child(
                            div()
                                .text_size(text_xs.size)
                                .line_height(text_xs.line_height)
                                .text_color(tokens.colors.muted_foreground)
                                .child(plan.description.clone()),
                        )
                    })
                    .when(!highlights.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap(tokens.spacing.xxs)
                                .pt(tokens.spacing.xxs)
                                .children(highlights),
                        )
                    })
                    .when_some(campaign_block, |this, block| this.child(block)),
            )
            .child(price_column)
    }

    fn render_paying(
        &self,
        tokens: &SemanticThemeTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = active_theme(cx);
        let extended = theme.extended().clone();
        let Some(paying) = &self.paying else {
            return div().into_any_element();
        };
        let order = &paying.order;
        let translator = self.host.read(cx).translator().read(cx).clone();
        let remaining = remaining_secs(&order.expires_at, paying.now_ms);
        let amount = format_minor(order.amount_minor, &order.currency);
        let qr_size = theme.text_extent(QR_SIZE);
        let plan_name = if order.plan_name.is_empty() {
            order.plan_code.clone()
        } else {
            order.plan_name.clone()
        };
        let small = |text: SharedString, color| {
            div()
                .text_size(tokens.typography.xs.size)
                .line_height(tokens.typography.xs.line_height)
                .font_features(tabular_numbers())
                .text_color(color)
                .child(text)
        };

        v_flex()
            .w_full()
            .items_center()
            .gap(tokens.spacing.md)
            .child(
                // 二维码自带白色静区；外框只加描边与圆角裁切。
                div()
                    .flex_none()
                    .rounded(tokens.radius.md)
                    .overflow_hidden()
                    .border(extended.stroke.thin)
                    .border_color(extended.colors.hairline)
                    .children(paying.qr.clone().map(|image| img(image).size(qr_size))),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap(tokens.spacing.xxs)
                    .child(
                        div()
                            .text_size(tokens.typography.xl.size)
                            .line_height(tokens.typography.xl.line_height)
                            .font_weight(FontWeight::SEMIBOLD)
                            .font_features(tabular_numbers())
                            .text_color(tokens.colors.foreground)
                            .child(amount),
                    )
                    .child(small(plan_name.into(), tokens.colors.muted_foreground)),
            )
            .when(order.credit_minor > 0, |this| {
                this.child(small(
                    t_with(
                        &translator,
                        "accountPlanCreditApplied",
                        &[("amount", &format_minor(order.credit_minor, &order.currency))],
                    ),
                    extended.colors.success,
                ))
            })
            .when(order.referral_discount_minor > 0, |this| {
                this.child(small(
                    t_with(
                        &translator,
                        "accountPlanReferralDiscountApplied",
                        &[(
                            "amount",
                            &format_minor(order.referral_discount_minor, &order.currency),
                        )],
                    ),
                    extended.colors.success,
                ))
            })
            .child(
                div()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .text_color(tokens.colors.foreground)
                    .child(t(&translator, "accountPlanScanHint")),
            )
            .child(small(
                t_with(
                    &translator,
                    "accountPlanExpiresIn",
                    &[("time", &format_countdown(remaining))],
                ),
                tokens.colors.muted_foreground,
            ))
            .child(
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.xs)
                    .child(
                        Spinner::new()
                            .with_size(extended.icon.sm)
                            .color(tokens.colors.muted_foreground),
                    )
                    .child(small(
                        t(&translator, "accountPlanWaitingPayment"),
                        tokens.colors.muted_foreground,
                    )),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.xxs)
                    .child(small(
                        t_with(
                            &translator,
                            "accountPlanOrderNo",
                            &[("orderNo", &order.order_no)],
                        ),
                        extended.colors.text_tertiary,
                    ))
                    .child({
                        let tip = t(&translator, "accountReferralCodeCopy");
                        let tooltip = tip.clone();
                        icon_button(
                            "plan-copy-order",
                            tip,
                            Icon::new(FluxIcon::Copy)
                                .size(extended.icon.sm)
                                .text_color(extended.colors.text_tertiary),
                            ButtonVariant::Ghost,
                            cx,
                        )
                        .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| {
                                this.copy_order_no(window, cx);
                            },
                        ))
                    }),
            )
            .child(
                div().w_full().flex().justify_end().child(
                    button(
                        "plan-paying-cancel",
                        t(&translator, "cancel"),
                        ButtonVariant::Secondary,
                        cx,
                    )
                    .on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| {
                            this.close(window, cx);
                        },
                    )),
                ),
            )
            .into_any_element()
    }

    fn render_success(
        &self,
        tokens: &SemanticThemeTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let extended = active_theme(cx).extended().clone();
        let success = extended.colors.success;
        v_flex()
            .w_full()
            .items_center()
            .gap(tokens.spacing.md)
            .child(
                div()
                    .size(extended.icon.lg * 3.)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(tokens.radius.full)
                    .bg(success.opacity(ICON_TINT_ALPHA))
                    .child(
                        Icon::new(FluxIcon::Check)
                            .size(extended.icon.lg * 1.5)
                            .text_color(success),
                    ),
            )
            .child(
                div()
                    .text_size(extended.title.size)
                    .line_height(extended.title.line_height)
                    .font_weight(extended.title.weight)
                    .text_color(tokens.colors.foreground)
                    .child(self.text("accountPlanPaidSuccess", cx)),
            )
            .child(
                div()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .text_color(tokens.colors.muted_foreground)
                    .child(self.paid_plan.clone()),
            )
            .child(
                div().w_full().flex().justify_end().child(
                    button(
                        "plan-success-close",
                        self.text("close", cx),
                        ButtonVariant::Primary,
                        cx,
                    )
                    .on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| {
                            this.close(window, cx);
                        },
                    )),
                ),
            )
    }
}
