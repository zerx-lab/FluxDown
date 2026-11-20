//! 「推送通知」页的云端推送标签：账号条（登录 / 套餐 / 上报开关 / 隐私 / 用量）、
//! 渠道卡片网格、云端投递记录。
//!
//! 数据全部来自 `SettingsStore`（`AgentSnapshot.cloud_notify` 投影 + 投递记录分页），
//! 写操作经 agent 的 `agent.cloudNotify.*`；页面不自建连接，也不在 render 里写状态。

use std::time::Duration;

use fluxdown_protocol::{
    AgentSessionDto, CloudNotifyChannelDto, CloudNotifyKindDto, CloudNotifyOverviewDto,
    CloudNotifyStateDto, CloudNotifyTestResult, RpcErrorData, method,
};
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, DialogIntent, FluxIcon, IconControlExt as _, button, card,
    color_transition, dialog_footer, dialog_title, loading_button, tabular_numbers,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, Div, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement, SharedString, Styled, Window, div,
    prelude::FluentBuilder as _, pulsating_between, px, relative,
};
use gpui_component::{
    Disableable as _, Icon, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    skeleton::Skeleton,
    switch::Switch,
    v_flex,
};
use serde_json::{Value, json};

use super::cloud_channel_dialog;
use super::cloud_dialogs;
use super::webhook::WEBHOOK_EVENTS;
use crate::model::cloud_notify::{
    AddBlock, CardSpec, CardState, ChannelKind, DeliveryTone, FailureKind, GRID_COLUMNS, Meter,
    RecipientSummary, add_blocked, add_options, build_cards, channel_limit_reached,
    classify_failure, daily_meter, delivery_status, exhausted_reset_at, field_separator,
    monthly_meter, privacy_field_keys, recipient_summary, reset_display, short_time,
    show_channel_subtitle, shows_retry,
};
use crate::push_view::PushHost;
use crate::store::{SettingsStore, rpc_error_text};
use crate::ui::{
    body_text, caption_heading, empty_state, meta_text, row_button, row_loading_button,
};

/// 账号条右侧用量块的宽度档位。
const USAGE_BLOCK_WIDTH: f32 = 280.;
/// 卡片上最多直接显示的事件标签数，其余折叠为「+N」。
const EVENT_CHIPS_SHOWN: usize = 3;
/// pending 状态点的脉冲周期与最低不透明度。
const PULSE_PERIOD: Duration = Duration::from_millis(1400);
const PULSE_MIN: f32 = 0.3;
/// 「发送测试通知」进行中标记（`SettingsStore::transient`）。
const TESTING_ALL_KEY: &str = "cloud_test_all";

/// 渲染云端推送页所需的上下文。
pub(crate) struct CloudPage<'a> {
    pub store: &'a Entity<SettingsStore>,
    pub translator: &'a Translator,
    pub host: &'a PushHost,
}

fn t(translator: &Translator, key: &str) -> SharedString {
    SharedString::from(translator.text(key).to_owned())
}

/// 把云端推送相关的 RPC 错误翻译成本地化文案（不透传服务端原文）。
pub(crate) fn failure_text(
    translator: &Translator,
    error: &RpcErrorData,
    max_channels: u32,
) -> String {
    match classify_failure(error.reason) {
        FailureKind::Offline => translator.text("cloudNotifyErrorOffline").to_owned(),
        FailureKind::ChannelLimit => translator.text_with(
            "cloudNotifyErrorChannelLimit",
            &[("limit", &max_channels.to_string())],
        ),
        FailureKind::PlanDisabled => translator.text("cloudNotifyPlanDisabled").to_owned(),
        FailureKind::InvalidCode => translator.text("cloudNotifyEmailCodeInvalid").to_owned(),
        FailureKind::RateLimited => translator.text("accountErrorRateLimited").to_owned(),
        FailureKind::Other => translator.text_with(
            "cloudNotifyErrorGeneric",
            &[("error", &rpc_error_text(translator, error))],
        ),
    }
}

/// 概览拉取失败（只有原因、没有 RPC 错误体）的文案。
fn overview_failure_text(translator: &Translator, state: &CloudNotifyStateDto) -> String {
    if classify_failure(state.last_error_reason) == FailureKind::Offline {
        translator.text("cloudNotifyErrorOffline").to_owned()
    } else {
        translator.text_with(
            "cloudNotifyErrorGeneric",
            &[("error", translator.text("localServiceActionFailed"))],
        )
    }
}

fn event_label(translator: &Translator, wire: &str) -> String {
    WEBHOOK_EVENTS
        .iter()
        .find(|(event, _)| *event == wire)
        .map_or_else(
            || wire.to_owned(),
            |(_, key)| translator.text(key).to_owned(),
        )
}

/// 渲染整个云端推送标签。
pub(crate) fn render(page: &CloudPage, window: &mut Window, cx: &mut App) -> AnyElement {
    let tokens = active_theme(cx).tokens().clone();
    let signed_in = page.store.read(cx).session().is_some();
    let mut column = v_flex()
        .w_full()
        .gap(tokens.spacing.lg + tokens.spacing.xs)
        .child(account_strip(page, cx));
    if signed_in {
        column = column
            .child(channels_section(page, cx))
            .child(log_section(page, window, cx));
    }
    column.into_any_element()
}

// ───────────────────────── 通用小件 ─────────────────────────

/// 首字母圆形徽标（渠道没有合适的单色图标，统一用徽标）。
fn monogram(text: &str, size: f32, muted: bool, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let letter: String = text
        .chars()
        .next()
        .map_or_else(String::new, |ch| ch.to_uppercase().collect());
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full()
        .bg(tokens.colors.muted)
        .text_size(tokens.typography.sm.size)
        .font_weight(FontWeight::MEDIUM)
        .text_color(if muted {
            theme.extended().colors.text_tertiary
        } else {
            tokens.colors.foreground
        })
        .child(SharedString::from(letter))
}

fn status_dot(color: gpui::Hsla) -> Div {
    div().flex_none().size(px(6.)).rounded_full().bg(color)
}

fn section_header(
    title: SharedString,
    description: SharedString,
    right: impl IntoElement,
    cx: &App,
) -> Div {
    let tokens = active_theme(cx).tokens().clone();
    h_flex()
        .w_full()
        .items_end()
        .justify_between()
        .gap(tokens.spacing.md)
        .px(tokens.spacing.xs)
        .pb(tokens.spacing.sm)
        .child(
            v_flex()
                .min_w_0()
                .gap(tokens.spacing.xxs)
                .child(caption_heading(cx).child(title))
                .child(meta_text(cx).child(description)),
        )
        .child(right)
}

fn skeleton_bar(width: f32, height: f32, cx: &App) -> Skeleton {
    Skeleton::new()
        .w(px(width))
        .h(px(height))
        .rounded(active_theme(cx).tokens().radius.sm)
}

/// 失败的异步结果用 toast 展示；成功返回 `true`。
fn toast_failure(window: &mut Window, cx: &mut App, text: String) {
    window.push_notification(toast::error(text), cx);
}

/// 发起 agent RPC，结果在窗口内回调（可弹 toast / 关对话框）。
fn call_in_window(
    store: &Entity<SettingsStore>,
    method: &'static str,
    params: Value,
    window: &mut Window,
    cx: &mut App,
    on_done: impl FnOnce(Result<Value, RpcErrorData>, &mut Window, &mut App) + 'static,
) {
    let future = store.read(cx).raw_call(method, params);
    window
        .spawn(cx, async move |cx| {
            let result = future.await;
            let Ok(()) = cx.update(|window, cx| on_done(result, window, cx)) else {
                // 窗口已关闭，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
}

// ───────────────────────── 账号条 ─────────────────────────

fn account_strip(page: &CloudPage, cx: &mut App) -> Div {
    let Some(session) = page.store.read(cx).session().cloned() else {
        return signed_out_strip(page, cx);
    };
    let state = page.store.read(cx).cloud_notify().clone();
    let read_only = page.store.read(cx).is_read_only();
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let extended = theme.extended().colors;
    let translator = page.translator;

    let identity = identity_block(&session, cx);
    let usage = usage_block(page, &state, cx);

    let store = page.store.clone();
    let reporting_busy = page.store.read(cx).is_busy("cloudReporting");
    let reporting = h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap(tokens.spacing.lg)
        .child(
            v_flex()
                .min_w_0()
                .gap(tokens.spacing.xxs)
                .child(body_text(cx).child(t(translator, "cloudNotifyReportThisDevice")))
                .child(meta_text(cx).child(t(translator, "cloudNotifyReportDesc"))),
        )
        .child(
            Switch::new("cloud-reporting")
                .checked(state.reporting)
                .disabled(read_only || reporting_busy)
                .on_click({
                    let translator = translator.clone();
                    move |checked: &bool, window, cx| {
                        if *checked {
                            // 开启需显式同意：同意前开关保持关闭（状态仍是 agent 的 false）。
                            cloud_dialogs::open_consent(
                                store.clone(),
                                translator.clone(),
                                window,
                                cx,
                            );
                        } else {
                            store.update(cx, |store, cx| store.set_cloud_reporting(false, cx));
                        }
                    }
                }),
        );

    let fields: Vec<String> = privacy_field_keys(state.include_url, state.include_save_dir)
        .into_iter()
        .map(|key| translator.text(key).to_owned())
        .collect();
    let summary = translator.text_with(
        "cloudNotifyPrivacySummary",
        &[("fields", &fields.join(field_separator(translator.locale())))],
    );
    let privacy = h_flex()
        .w_full()
        .items_center()
        .gap(tokens.spacing.sm)
        .child(
            meta_text(cx)
                .min_w_0()
                .truncate()
                .child(SharedString::from(summary)),
        )
        .child(
            row_button(
                "cloud-privacy",
                t(translator, "cloudNotifyPrivacySettings"),
                ButtonVariant::Link,
                cx,
            )
            .on_click({
                let store = page.store.clone();
                let translator = translator.clone();
                move |_, window, cx| {
                    cloud_dialogs::open_privacy(store.clone(), translator.clone(), window, cx);
                }
            }),
        );

    let action_error = page
        .store
        .read(cx)
        .cloud_action_error()
        .map(|error| failure_text(translator, error, 0));

    let divider = || div().w_full().h(px(1.)).bg(extended.hairline);
    card(cx)
        .w_full()
        .flex()
        .flex_col()
        .gap(tokens.spacing.md)
        .p(tokens.spacing.lg)
        .child(
            h_flex()
                .w_full()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(tokens.spacing.lg)
                .child(identity)
                .child(usage),
        )
        .child(divider())
        .child(reporting)
        .child(privacy)
        .children(action_error.map(|text| {
            meta_text(cx)
                .text_color(tokens.colors.destructive)
                .child(SharedString::from(text))
        }))
}

fn signed_out_strip(page: &CloudPage, cx: &mut App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let translator = page.translator;
    let open_account = page.host.open_account.clone();
    card(cx)
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap(tokens.spacing.lg)
        .p(tokens.spacing.lg)
        .child(
            h_flex()
                .min_w_0()
                .items_center()
                .gap(tokens.spacing.md)
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(36.))
                        .rounded_full()
                        .bg(tokens.colors.muted)
                        .child(
                            Icon::new(FluxIcon::Bell)
                                .size(theme.extended().icon.lg)
                                .text_color(theme.extended().colors.text_tertiary),
                        ),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .gap(tokens.spacing.xxs)
                        .child(
                            body_text(cx)
                                .font_weight(FontWeight::MEDIUM)
                                .child(t(translator, "cloudNotifySignedOutTitle")),
                        )
                        .child(meta_text(cx).child(t(translator, "cloudNotifySignedOutDesc"))),
                ),
        )
        .child(
            button(
                "cloud-sign-in",
                t(translator, "cloudNotifySignIn"),
                ButtonVariant::Primary,
                cx,
            )
            .flex_shrink_0()
            .on_click(move |_, window, cx| open_account(window, cx)),
        )
}

fn identity_block(session: &AgentSessionDto, cx: &App) -> Div {
    let tokens = active_theme(cx).tokens().clone();
    let plan = session
        .current_plan
        .as_ref()
        .map(|plan| plan.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| session.user.plan.clone());
    let initial =
        crate::model::cloud_notify::avatar_initial(&session.user.nickname, &session.user.email);
    h_flex()
        .min_w_0()
        .items_center()
        .gap(tokens.spacing.md)
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(36.))
                .rounded_full()
                .bg(tokens.colors.primary)
                .text_color(tokens.colors.primary_foreground)
                .text_size(tokens.typography.md.size)
                .font_weight(FontWeight::MEDIUM)
                .child(SharedString::from(initial)),
        )
        .child(
            v_flex()
                .min_w_0()
                .gap(tokens.spacing.xxs)
                .child(
                    body_text(cx)
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(SharedString::from(session.user.email.clone())),
                )
                .child(meta_text(cx).truncate().child(SharedString::from(plan))),
        )
}

/// 账号条右侧：用量 / 满额 / 套餐未开通 / 骨架 / 拉取失败。
fn usage_block(page: &CloudPage, state: &CloudNotifyStateDto, cx: &mut App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let translator = page.translator;
    let width = theme.text_extent(USAGE_BLOCK_WIDTH);
    let block = v_flex().flex_none().w(width).gap(tokens.spacing.sm);
    let open_account = page.host.open_account.clone();
    let upgrade = move |cx: &App| {
        let open_account = open_account.clone();
        row_button(
            "cloud-upgrade",
            t(translator, "cloudNotifyUpgrade"),
            ButtonVariant::Link,
            cx,
        )
        .on_click(move |_, window, cx| open_account(window, cx))
    };

    let Some(overview) = &state.overview else {
        if overview_failed(state) {
            return block.child(
                meta_text(cx)
                    .text_color(tokens.colors.destructive)
                    .child(SharedString::from(overview_failure_text(translator, state))),
            );
        }
        return block
            .child(skeleton_bar(USAGE_BLOCK_WIDTH, 10., cx))
            .child(skeleton_bar(USAGE_BLOCK_WIDTH, 10., cx));
    };
    if !overview.enabled {
        return block.child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(tokens.spacing.sm)
                .child(
                    meta_text(cx)
                        .text_color(theme.extended().colors.warning)
                        .child(t(translator, "cloudNotifyPlanDisabled")),
                )
                .child(upgrade(cx)),
        );
    }
    let unlimited = translator.text("cloudNotifyUsageUnlimited").to_owned();
    let usage = &overview.usage;
    // 不限额的周期没有「重置」可言，不显示；时间无法解析时同样留空（与 Web 一致）。
    let reset_text = |meter: Meter, reset_at: &str| {
        reset_display(meter, reset_at)
            .map(|time| translator.text_with("cloudNotifyUsageResetAt", &[("time", &time)]))
    };
    let mut block = block
        .child(meter_row(
            translator.text_with(
                "cloudNotifyUsageToday",
                &[
                    ("used", &usage.daily_used.to_string()),
                    ("limit", &daily_meter(usage).limit_label(&unlimited)),
                ],
            ),
            reset_text(daily_meter(usage), &usage.daily_reset_at),
            daily_meter(usage),
            "cloud-meter-daily",
            cx,
        ))
        .child(meter_row(
            translator.text_with(
                "cloudNotifyUsageMonth",
                &[
                    ("used", &usage.monthly_used.to_string()),
                    ("limit", &monthly_meter(usage).limit_label(&unlimited)),
                ],
            ),
            reset_text(monthly_meter(usage), &usage.monthly_reset_at),
            monthly_meter(usage),
            "cloud-meter-monthly",
            cx,
        ));
    if let Some(reset_at) = exhausted_reset_at(usage) {
        block = block.child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(tokens.spacing.sm)
                .child(
                    meta_text(cx)
                        .min_w_0()
                        .text_color(tokens.colors.destructive)
                        .child(SharedString::from(translator.text_with(
                            "cloudNotifyQuotaExhausted",
                            &[("time", &short_time(reset_at))],
                        ))),
                )
                .child(upgrade(cx)),
        );
    }
    block
}

fn overview_failed(state: &CloudNotifyStateDto) -> bool {
    state.overview.is_none() && !state.loading && state.last_error_reason.is_some()
}

/// 一条细进度条：标题（含 x/y）在左、重置时间在右（`reset` 为空则不显示），轨道在下；
/// 满额标题变危险色，不限只留空轨道。与 Web `CloudAccountBar` 的 `Meter` 同布局。
fn meter_row(
    label: String,
    reset: Option<String>,
    meter: Meter,
    id: &'static str,
    cx: &App,
) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let (fill, label_color) = if meter.exhausted() {
        (tokens.colors.destructive, tokens.colors.destructive)
    } else {
        (tokens.colors.primary, tokens.colors.foreground)
    };
    v_flex()
        .w_full()
        .gap(tokens.spacing.xs)
        .child(
            h_flex()
                .w_full()
                .items_baseline()
                .justify_between()
                .gap(tokens.spacing.sm)
                .child(
                    meta_text(cx)
                        .font_features(tabular_numbers())
                        .text_color(label_color)
                        .when(meter.exhausted(), |this| {
                            this.font_weight(FontWeight::MEDIUM)
                        })
                        .child(SharedString::from(label)),
                )
                .children(reset.map(|text| {
                    meta_text(cx)
                        .min_w_0()
                        .truncate()
                        .font_features(tabular_numbers())
                        .text_color(theme.extended().colors.text_tertiary)
                        .child(SharedString::from(text))
                })),
        )
        .child(
            div()
                .id(id)
                .w_full()
                .h(px(4.))
                .rounded_full()
                .bg(tokens.colors.muted)
                .overflow_hidden()
                .child(
                    div()
                        .h_full()
                        .w(relative(meter.ratio()))
                        .rounded_full()
                        .bg(fill),
                ),
        )
}

// ───────────────────────── 渠道区 ─────────────────────────

fn channels_section(page: &CloudPage, cx: &mut App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let translator = page.translator;
    let state = page.store.read(cx).cloud_notify().clone();
    let read_only = page.store.read(cx).is_read_only();

    let Some(overview) = state.overview.clone() else {
        let body = if overview_failed(&state) {
            overview_error_panel(page, &state, cx).into_any_element()
        } else {
            skeleton_grid(cx).into_any_element()
        };
        return v_flex()
            .w_full()
            .child(section_header(
                t(translator, "cloudNotifyChannelsTitle"),
                t(translator, "cloudNotifyChannelsDesc"),
                div(),
                cx,
            ))
            .child(body);
    };

    let catalog = catalog_of(page, cx);
    let specs = build_cards(&overview, &catalog);

    // 没有任何渠道：精致空态，主按钮就是同一个「添加渠道」下拉；区块头不再重复一个按钮。
    if specs.is_empty() {
        let add = add_dropdown(page, &overview, "cloud-add-empty", true, cx);
        let blocked_hint = (!overview.enabled).then(|| t(translator, "cloudNotifyPlanDisabled"));
        return v_flex()
            .w_full()
            .child(section_header(
                t(translator, "cloudNotifyChannelsTitle"),
                t(translator, "cloudNotifyChannelsDesc"),
                div(),
                cx,
            ))
            .child(
                card(cx)
                    .w_full()
                    .border_dashed()
                    .border_color(tokens.colors.border)
                    .child(
                        v_flex()
                            .w_full()
                            .items_center()
                            .gap(tokens.spacing.md)
                            .py(tokens.spacing.xl)
                            .px(tokens.spacing.lg)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(48.))
                                    .rounded_full()
                                    .bg(tokens.colors.muted)
                                    .child(
                                        Icon::new(FluxIcon::Bell)
                                            .size(theme.extended().icon.lg)
                                            .text_color(theme.extended().colors.text_tertiary),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .items_center()
                                    .gap(tokens.spacing.xxs)
                                    .child(
                                        body_text(cx)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(t(translator, "cloudNotifyChannelsEmptyTitle")),
                                    )
                                    .child(
                                        meta_text(cx)
                                            .text_center()
                                            .child(t(translator, "cloudNotifyChannelsEmptyDesc")),
                                    ),
                            )
                            .child(add)
                            .children(blocked_hint.map(|text| {
                                meta_text(cx)
                                    .text_color(theme.extended().colors.warning)
                                    .child(text)
                            })),
                    ),
            );
    }

    let limit_text = translator.text_with(
        "cloudNotifyChannelLimit",
        &[
            ("used", &overview.channels.len().to_string()),
            (
                "limit",
                &if overview.max_channels == 0 {
                    translator.text("cloudNotifyUsageUnlimited").to_owned()
                } else {
                    overview.max_channels.to_string()
                },
            ),
        ],
    );
    let limit_reached = channel_limit_reached(&overview);
    let right = h_flex()
        .items_center()
        .gap(tokens.spacing.sm)
        .child(
            meta_text(cx)
                .font_features(tabular_numbers())
                .text_color(if limit_reached {
                    theme.extended().colors.warning
                } else {
                    tokens.colors.muted_foreground
                })
                .child(SharedString::from(limit_text)),
        )
        .child(add_dropdown(page, &overview, "cloud-add-menu", false, cx));

    // 排列：1 个渠道占半宽（与两列时的单卡同宽，不撑满）；2 个及以上两列等宽，行尾不足补空位。
    let cards: Vec<AnyElement> = specs
        .into_iter()
        .enumerate()
        .map(|(index, spec)| card_view(page, &overview, spec, index, read_only, cx))
        .collect();
    let mut grid = v_flex().w_full().gap(tokens.spacing.md);
    let mut cards = cards.into_iter();
    loop {
        let row: Vec<AnyElement> = cards.by_ref().take(GRID_COLUMNS).collect();
        if row.is_empty() {
            break;
        }
        let missing = GRID_COLUMNS - row.len();
        grid = grid.child(
            h_flex()
                .w_full()
                .items_stretch()
                .gap(tokens.spacing.md)
                .children(
                    row.into_iter()
                        .map(|card| div().flex().flex_1().min_w_0().child(card)),
                )
                .children((0..missing).map(|_| div().flex_1().min_w_0())),
        );
    }

    v_flex()
        .w_full()
        .child(section_header(
            t(translator, "cloudNotifyChannelsTitle"),
            t(translator, "cloudNotifyChannelsDesc"),
            right,
            cx,
        ))
        .child(grid)
}

fn overview_error_panel(page: &CloudPage, state: &CloudNotifyStateDto, cx: &App) -> Div {
    let translator = page.translator;
    let store = page.store.clone();
    let busy = page.store.read(cx).is_busy("cloudRefresh");
    card(cx)
        .w_full()
        .child(empty_state(
            FluxIcon::TriangleAlert,
            SharedString::from(overview_failure_text(translator, state)),
            SharedString::default(),
            cx,
        ))
        .child(
            h_flex()
                .w_full()
                .justify_center()
                .pb(active_theme(cx).tokens().spacing.lg)
                .child(
                    row_loading_button(
                        "cloud-overview-retry",
                        t(translator, "cloudNotifyRefresh"),
                        ButtonVariant::Secondary,
                        busy,
                        cx,
                    )
                    .on_click(move |_, _, cx| {
                        store.update(cx, |store, cx| store.refresh_cloud_notify(true, cx));
                    }),
                ),
        )
}

fn skeleton_grid(cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let columns = GRID_COLUMNS;
    h_flex()
        .w_full()
        .gap(tokens.spacing.md)
        .children((0..columns).map(|_| {
            card(cx)
                .flex_1()
                .min_w_0()
                .p(tokens.spacing.md)
                .flex()
                .flex_col()
                .gap(tokens.spacing.sm)
                .child(
                    h_flex()
                        .items_center()
                        .gap(tokens.spacing.sm)
                        .child(Skeleton::new().size(px(32.)).rounded_full())
                        .child(skeleton_bar(96., 12., cx)),
                )
                .child(skeleton_bar(160., 10., cx))
                .child(skeleton_bar(64., 10., cx))
        }))
}

/// 当前服务端目录（云端关闭时页面不会渲染到这里，缺省按空处理）。
fn catalog_of(page: &CloudPage, cx: &App) -> Vec<CloudNotifyKindDto> {
    page.store
        .read(cx)
        .cloud_notify()
        .catalog
        .clone()
        .unwrap_or_default()
}

/// 「添加渠道」下拉：目录里每个种类一项（种类名 + 一句说明），配置未就绪的置灰并标「暂不可用」；
/// 套餐未开通 / 渠道数已满 / 断线只读时整个按钮禁用，悬浮提示原因。
/// `primary` = 空态里的主按钮，否则是区块头里的次要按钮。
fn add_dropdown(
    page: &CloudPage,
    overview: &CloudNotifyOverviewDto,
    id: &'static str,
    primary: bool,
    cx: &App,
) -> impl IntoElement {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let tertiary = theme.extended().colors.text_tertiary;
    let translator = page.translator.clone();
    let store = page.store.clone();
    let read_only = store.read(cx).is_read_only();
    let options = add_options(&catalog_of(page, cx));
    let blocked = add_blocked(overview);
    let disabled = read_only || blocked.is_some() || options.is_empty();
    let reason = blocked.map(|block| match block {
        AddBlock::PlanDisabled => t(&translator, "cloudNotifyPlanDisabled"),
        AddBlock::LimitReached => SharedString::from(translator.text_with(
            "cloudNotifyErrorChannelLimit",
            &[("limit", &overview.max_channels.to_string())],
        )),
    });
    let button = Button::new(id)
        .icon(FluxIcon::Plus)
        .label(t(&translator, "cloudNotifyAddChannel"))
        .dropdown_caret(true)
        .control(cx)
        .disabled(disabled);
    let button = if primary {
        button.primary()
    } else {
        button.outline()
    };
    let button = match reason {
        Some(reason) => button.tooltip(reason),
        None => button,
    };
    button.dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
        options.iter().fold(menu, |menu, option| {
            let kind = option.kind;
            let available = option.available;
            let name = t(&translator, kind.name_key());
            let desc = t(&translator, kind.desc_key());
            let unavailable = t(&translator, "cloudNotifyStatusUnavailable");
            let foreground = tokens.colors.foreground;
            let muted = tokens.colors.muted_foreground;
            let (sm, xs) = (tokens.typography.sm, tokens.typography.xs);
            let gap = tokens.spacing.xxs;
            let open = {
                let store = store.clone();
                let translator = translator.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    cloud_channel_dialog::open(
                        store.clone(),
                        translator.clone(),
                        kind,
                        None,
                        window,
                        cx,
                    );
                }
            };
            menu.item(
                PopupMenuItem::element(move |_, _| {
                    v_flex()
                        .gap(gap)
                        .py(gap)
                        .child(
                            h_flex()
                                .items_center()
                                .gap(gap * 2.)
                                .child(
                                    div()
                                        .text_size(sm.size)
                                        .line_height(sm.line_height)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(foreground)
                                        .child(name.clone()),
                                )
                                .when(!available, |row| {
                                    row.child(
                                        div()
                                            .text_size(xs.size)
                                            .line_height(xs.line_height)
                                            .text_color(tertiary)
                                            .child(unavailable.clone()),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_size(xs.size)
                                .line_height(xs.line_height)
                                .text_color(muted)
                                .child(desc.clone()),
                        )
                })
                .disabled(!available)
                .on_click(open),
            )
        })
    })
}

/// 一张渠道卡（只对应已添加的渠道）：头部 = 种类徽标 + 渠道名 + 状态，第二行 = 收件目标。
fn card_view(
    page: &CloudPage,
    overview: &CloudNotifyOverviewDto,
    spec: CardSpec,
    index: usize,
    read_only: bool,
    cx: &App,
) -> AnyElement {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let extended = theme.extended().colors;
    let translator = page.translator;
    let kind_name = t(translator, spec.kind.name_key());
    let plan_enabled = overview.enabled;
    let id = SharedString::from(format!("cloud-card-{index}"));
    let channel = spec.channel.clone();

    let (status_color, status_key) = match spec.state {
        CardState::Unavailable => (extended.text_tertiary, "cloudNotifyStatusUnavailable"),
        CardState::Failing => (tokens.colors.destructive, "cloudNotifyStatusFailing"),
        CardState::Paused => (extended.text_tertiary, "cloudNotifyStatusPaused"),
        CardState::Connected => (extended.success, "cloudNotifyStatusConnected"),
    };
    let menu = channel_menu(page, overview, &channel, spec.kind, read_only, cx);
    let events: Vec<String> = channel
        .events
        .iter()
        .map(|wire| event_label(translator, wire))
        .collect();
    let hidden_events = events.len().saturating_sub(EVENT_CHIPS_SHOWN);
    let chip = |text: String| {
        div()
            .px(tokens.spacing.xs + tokens.spacing.xxs)
            .py(tokens.spacing.xxs)
            .rounded(tokens.radius.sm)
            .bg(tokens.colors.muted)
            .text_size(theme.extended().caption.size)
            .line_height(theme.extended().caption.line_height)
            .text_color(tokens.colors.muted_foreground)
            .child(SharedString::from(text))
    };

    // 第二行：收件目标（邮件地址 / 多个时「{email} 等 {n} 个邮箱」/ Telegram 账号）；
    // 渠道名与种类名不同时前缀种类名。
    let target_text = match recipient_summary(&channel) {
        RecipientSummary::None => None,
        RecipientSummary::One(address) => Some(address),
        RecipientSummary::Many { first, total } => Some(translator.text_with(
            "cloudNotifyEmailMore",
            &[("email", &first), ("n", &total.to_string())],
        )),
    };
    let second_line = match (
        show_channel_subtitle(&channel.name, &kind_name),
        target_text,
    ) {
        (true, Some(target)) => Some(format!("{kind_name} · {target}")),
        (true, None) => Some(kind_name.to_string()),
        (false, Some(target)) => Some(target),
        (false, None) => None,
    };

    let dimmed = !plan_enabled || matches!(spec.state, CardState::Paused | CardState::Unavailable);
    let open_edit = {
        let store = page.store.clone();
        let translator = translator.clone();
        let channel = channel.clone();
        let kind = spec.kind;
        move |window: &mut Window, cx: &mut App| {
            cloud_channel_dialog::open(
                store.clone(),
                translator.clone(),
                kind,
                Some(channel.clone()),
                window,
                cx,
            );
        }
    };

    card(cx)
        .id(id)
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(tokens.spacing.sm)
        .p(tokens.spacing.md)
        .hover(move |style| style.bg(extended.row_hover))
        .when(spec.state == CardState::Unavailable, |card| {
            card.opacity(0.6)
        })
        .child(
            h_flex()
                .w_full()
                .items_start()
                .gap(tokens.spacing.sm)
                .child(monogram(&kind_name, 32., dimmed, cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            body_text(cx)
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(SharedString::from(channel.name.clone())),
                        )
                        .children(
                            second_line.map(|text| {
                                meta_text(cx).truncate().child(SharedString::from(text))
                            }),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .h(theme.density().control)
                        .items_center()
                        .gap(tokens.spacing.xs + tokens.spacing.xxs)
                        .child(status_dot(status_color))
                        .child(
                            meta_text(cx)
                                .text_color(status_color)
                                .child(t(translator, status_key)),
                        ),
                )
                .child(menu),
        )
        .child(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap(tokens.spacing.xs)
                .children(events.into_iter().take(EVENT_CHIPS_SHOWN).map(chip))
                .when(hidden_events > 0, |row| {
                    row.child(chip(format!("+{hidden_events}")))
                }),
        )
        .when(spec.state == CardState::Failing, |card| {
            let reason = channel.last_error.clone().unwrap_or_default();
            card.child(
                h_flex()
                    .w_full()
                    .items_start()
                    .justify_between()
                    .gap(tokens.spacing.sm)
                    .child(
                        meta_text(cx)
                            .min_w_0()
                            .line_clamp(2)
                            .text_color(tokens.colors.destructive)
                            .child(SharedString::from(
                                translator.text_with("cloudNotifyLastError", &[("error", &reason)]),
                            )),
                    )
                    .child(
                        row_button(
                            SharedString::from(format!("cloud-fix-{}", channel.id)),
                            t(translator, "cloudNotifyFix"),
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .disabled(read_only)
                        .on_click(move |_, window, cx| open_edit(window, cx)),
                    ),
            )
        })
        .into_any_element()
}

/// 卡片右上角 `···` 菜单：发送测试 / 编辑 / 暂停或恢复 / 断开。
fn channel_menu(
    page: &CloudPage,
    overview: &CloudNotifyOverviewDto,
    channel: &CloudNotifyChannelDto,
    kind: ChannelKind,
    read_only: bool,
    cx: &App,
) -> impl IntoElement {
    let translator = page.translator.clone();
    let store = page.store.clone();
    let channel = channel.clone();
    let max_channels = overview.max_channels;
    let plan_enabled = overview.enabled;
    Button::new(SharedString::from(format!("cloud-menu-{}", channel.id)))
        .ghost()
        .icon(FluxIcon::Ellipsis)
        .control_icon(cx)
        .disabled(read_only)
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let pause_label = t(
                &translator,
                if channel.enabled {
                    "cloudNotifyPause"
                } else {
                    "cloudNotifyResume"
                },
            );
            let test = {
                let store = store.clone();
                let translator = translator.clone();
                let channel = channel.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    test_one(&store, &translator, &channel, max_channels, window, cx);
                }
            };
            let edit = {
                let store = store.clone();
                let translator = translator.clone();
                let channel = channel.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    cloud_channel_dialog::open(
                        store.clone(),
                        translator.clone(),
                        kind,
                        Some(channel.clone()),
                        window,
                        cx,
                    );
                }
            };
            let toggle = {
                let store = store.clone();
                let translator = translator.clone();
                let channel = channel.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    set_enabled(&store, &translator, &channel, max_channels, window, cx);
                }
            };
            let disconnect = {
                let store = store.clone();
                let translator = translator.clone();
                let channel = channel.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    confirm_disconnect(&store, &translator, &channel, max_channels, window, cx);
                }
            };
            menu.item(
                PopupMenuItem::new(t(&translator, "cloudNotifyTest"))
                    .disabled(!plan_enabled)
                    .on_click(test),
            )
            .item(PopupMenuItem::new(t(&translator, "cloudNotifyEdit")).on_click(edit))
            .item(PopupMenuItem::new(pause_label).on_click(toggle))
            .separator()
            .item(PopupMenuItem::new(t(&translator, "cloudNotifyDisconnect")).on_click(disconnect))
        })
}

/// 单个渠道的测试结果文案：`Ok(())` 成功，`Err(详情)` 失败。
fn test_outcome(
    translator: &Translator,
    result: Result<Value, RpcErrorData>,
    max_channels: u32,
) -> Result<(), String> {
    match result {
        Err(error) => Err(failure_text(translator, &error, max_channels)),
        Ok(value) => match serde_json::from_value::<CloudNotifyTestResult>(value) {
            Ok(result) if result.success => Ok(()),
            Ok(result) => Err(result
                .error
                .filter(|error| !error.is_empty())
                .unwrap_or_else(|| translator.text("localServiceActionFailed").to_owned())),
            Err(_) => Err(translator.text("localServiceActionFailed").to_owned()),
        },
    }
}

fn test_one(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    channel: &CloudNotifyChannelDto,
    max_channels: u32,
    window: &mut Window,
    cx: &mut App,
) {
    let translator = translator.clone();
    call_in_window(
        store,
        method::AGENT_CLOUD_NOTIFY_TEST_CHANNEL,
        json!({ "id": channel.id }),
        window,
        cx,
        move |result, window, cx| {
            let notification = match test_outcome(&translator, result, max_channels) {
                Ok(()) => toast::success(translator.text("cloudNotifyTestOk").to_owned()),
                Err(detail) => {
                    toast::error(translator.text_with("cloudNotifyTestFail", &[("error", &detail)]))
                }
            };
            window.push_notification(notification, cx);
        },
    );
}

/// 暂停 / 恢复：成功后由 agent 刷新概览并推送，失败 toast。
fn set_enabled(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    channel: &CloudNotifyChannelDto,
    max_channels: u32,
    window: &mut Window,
    cx: &mut App,
) {
    let translator = translator.clone();
    call_in_window(
        store,
        method::AGENT_CLOUD_NOTIFY_UPDATE_CHANNEL,
        json!({ "id": channel.id, "enabled": !channel.enabled }),
        window,
        cx,
        move |result, window, cx| {
            if let Err(error) = result {
                toast_failure(window, cx, failure_text(&translator, &error, max_channels));
            }
        },
    );
}

fn confirm_disconnect(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    channel: &CloudNotifyChannelDto,
    max_channels: u32,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(
        translator.text_with("cloudNotifyDisconnectConfirm", &[("name", &channel.name)]),
    );
    let ok = t(translator, "cloudNotifyDisconnect");
    let cancel = t(translator, "cancel");
    let id = channel.id.clone();
    let store = store.clone();
    let translator = translator.clone();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let store = store.clone();
        let translator = translator.clone();
        let id = id.clone();
        alert
            .title(dialog_title(title.clone(), cx))
            .footer(dialog_footer(
                Some(cancel.clone()),
                ok.clone(),
                DialogIntent::Destructive,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let translator = translator.clone();
                call_in_window(
                    &store,
                    method::AGENT_CLOUD_NOTIFY_DELETE_CHANNEL,
                    json!({ "id": id }),
                    window,
                    cx,
                    move |result, window, cx| {
                        if let Err(error) = result {
                            toast_failure(
                                window,
                                cx,
                                failure_text(&translator, &error, max_channels),
                            );
                        }
                    },
                );
                true
            })
    });
}

// ───────────────────────── 云端投递记录 ─────────────────────────

fn log_section(page: &CloudPage, window: &mut Window, cx: &mut App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let extended = theme.extended().colors;
    let translator = page.translator;
    let store = page.store.clone();
    let read_only = store.read(cx).is_read_only();
    // 第一页就是 agent 推送的 `recent_deliveries`（实时更新）；「加载更多」追加的更早记录并在其后。
    let deliveries = store.read(cx).cloud_deliveries();
    let has_more = store.read(cx).cloud_deliveries_next_cursor().is_some();
    let load_error = store.read(cx).cloud_log().error.clone();
    let state = store.read(cx).cloud_notify().clone();
    let testable = state
        .overview
        .as_ref()
        .is_some_and(|overview| overview.enabled && overview.channels.iter().any(|c| c.enabled));
    let testing_all = store
        .read(cx)
        .transient(TESTING_ALL_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let refreshing = store.read(cx).is_busy("cloudRefresh");
    let loading_more = store.read(cx).is_busy("cloudDeliveries");

    let actions = h_flex()
        .items_center()
        .gap(tokens.spacing.sm)
        .child(
            loading_button(
                "cloud-log-test",
                t(translator, "cloudNotifyLogSendTest"),
                ButtonVariant::Secondary,
                testing_all,
                cx,
            )
            .flex_shrink_0()
            .disabled(read_only || !testable || testing_all)
            .on_click({
                let store = store.clone();
                let translator = translator.clone();
                move |_, window, cx| test_all(&store, &translator, window, cx)
            }),
        )
        .child(
            // 记录实时更新；刷新按钮只是让 agent 立即重新向云端同步概览与第一页。
            Button::new("cloud-log-refresh")
                .ghost()
                .icon(FluxIcon::RotateCw)
                .control_icon(cx)
                .tooltip(t(translator, "cloudNotifyRefresh"))
                .disabled(read_only || refreshing)
                .loading(refreshing)
                .on_click({
                    let store = store.clone();
                    move |_, _, cx| {
                        store.update(cx, |store, cx| store.refresh_cloud_notify(true, cx));
                    }
                }),
        );

    let body: AnyElement = if deliveries.is_empty() {
        // 概览还没到（首屏加载中）才显示骨架；已确认没有记录（含拉取失败）显示空态。
        if state.overview.is_none() && !overview_failed(&state) {
            card(cx)
                .w_full()
                .flex()
                .flex_col()
                .children((0..3).map(|_| {
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap(tokens.spacing.md)
                        .px(tokens.spacing.md)
                        .py(tokens.spacing.sm)
                        .child(
                            v_flex()
                                .gap(tokens.spacing.xs)
                                .child(skeleton_bar(220., 12., cx))
                                .child(skeleton_bar(140., 10., cx)),
                        )
                        .child(skeleton_bar(72., 10., cx))
                }))
                .into_any_element()
        } else {
            card(cx)
                .w_full()
                .child(empty_state(
                    FluxIcon::Bell,
                    t(translator, "cloudNotifyLogEmpty"),
                    SharedString::default(),
                    cx,
                ))
                .into_any_element()
        }
    } else {
        let mut list = card(cx).w_full().flex().flex_col().overflow_hidden();
        for (index, delivery) in deliveries.iter().enumerate() {
            let (status_key, tone) = delivery_status(&delivery.status);
            let target_color = match tone {
                DeliveryTone::Neutral => extended.text_tertiary,
                DeliveryTone::Success => extended.success,
                DeliveryTone::Error => tokens.colors.destructive,
                DeliveryTone::Warning => extended.warning,
            };
            // 状态变化（pending → sent / failed …）时点与文字颜色平滑过渡。
            let tone_color = color_transition(
                SharedString::from(format!("cloud-tone-{}", delivery.id)),
                target_color,
                window,
                cx,
            );
            let pending = delivery.status == "pending";
            let mut status_text = status_key.map_or_else(
                || delivery.status.clone(),
                |key| translator.text(key).to_owned(),
            );
            if shows_retry(delivery) {
                status_text.push_str(" · ");
                status_text.push_str(&translator.text_with(
                    "cloudNotifyLogRetry",
                    &[
                        ("n", &delivery.attempts.to_string()),
                        ("max", &delivery.max_attempts.to_string()),
                    ],
                ));
            }
            let target = translator.text_with(
                "cloudNotifyLogTarget",
                &[
                    ("channel", &delivery.channel_name),
                    ("device", &delivery.device_name),
                ],
            );
            let title = format!(
                "{} · {}",
                event_label(translator, &delivery.event),
                delivery.title
            );
            let detail = match delivery.error.as_deref().filter(|e| !e.is_empty()) {
                Some(error) => format!("{target} · {error}"),
                None => target,
            };
            let dot = status_dot(tone_color);
            // 进行中（pending）的点轻微脉冲；其它状态静止。
            let dot = if pending {
                dot.with_animation(
                    SharedString::from(format!("cloud-pulse-{}", delivery.id)),
                    Animation::new(PULSE_PERIOD)
                        .repeat()
                        .with_easing(pulsating_between(PULSE_MIN, 1.)),
                    |dot, delta| dot.opacity(delta),
                )
                .into_any_element()
            } else {
                dot.into_any_element()
            };
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("cloud-log-{}", delivery.id)))
                    .w_full()
                    .items_center()
                    .gap(tokens.spacing.md)
                    .px(tokens.spacing.md)
                    .py(tokens.spacing.sm)
                    .when(index > 0, |row| {
                        row.border_t_1().border_color(extended.hairline)
                    })
                    .hover(move |style| style.bg(extended.row_hover))
                    .child(dot)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(tokens.spacing.xxs)
                            .child(body_text(cx).truncate().child(SharedString::from(title)))
                            .child(meta_text(cx).truncate().child(SharedString::from(detail))),
                    )
                    .child(
                        v_flex()
                            .flex_none()
                            .items_end()
                            .gap(tokens.spacing.xxs)
                            .child(
                                meta_text(cx)
                                    .font_features(tabular_numbers())
                                    .text_color(extended.text_tertiary)
                                    .child(SharedString::from(short_time(&delivery.created_at))),
                            )
                            .child(
                                meta_text(cx)
                                    .text_color(tone_color)
                                    .child(SharedString::from(status_text)),
                            ),
                    ),
            );
        }
        let load_error_text =
            load_error.map(|error| SharedString::from(failure_text(translator, &error, 0)));
        let more = (has_more || load_error_text.is_some()).then(|| {
            let store = store.clone();
            v_flex()
                .w_full()
                .items_center()
                .gap(tokens.spacing.xs)
                .pt(tokens.spacing.sm)
                .children(load_error_text.map(|text| {
                    meta_text(cx)
                        .text_color(tokens.colors.destructive)
                        .child(text)
                }))
                .when(has_more, |col| {
                    col.child(
                        row_loading_button(
                            "cloud-log-more",
                            t(translator, "cloudNotifyLogMore"),
                            ButtonVariant::Secondary,
                            loading_more,
                            cx,
                        )
                        .disabled(read_only || loading_more)
                        .on_click(move |_, _, cx| {
                            store.update(cx, |store, cx| store.load_more_cloud_deliveries(cx));
                        }),
                    )
                })
        });
        v_flex()
            .w_full()
            .child(list)
            .children(more)
            .into_any_element()
    };

    v_flex()
        .w_full()
        .child(section_header(
            t(translator, "cloudNotifyLogTitle"),
            SharedString::from(format!(
                "{} · {}",
                translator.text("cloudNotifyLogSubtitle"),
                translator.text("cloudNotifyLogSendTestHint")
            )),
            actions,
            cx,
        ))
        .child(body)
}

/// 对每个已启用渠道各发一条测试（不计额度），汇总成一条 toast。
fn test_all(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    window: &mut Window,
    cx: &mut App,
) {
    let state = store.read(cx).cloud_notify().clone();
    let Some(overview) = state.overview else {
        return;
    };
    let max_channels = overview.max_channels;
    let jobs: Vec<_> = overview
        .channels
        .iter()
        .filter(|channel| channel.enabled)
        .map(|channel| {
            (
                channel.name.clone(),
                store.read(cx).raw_call(
                    method::AGENT_CLOUD_NOTIFY_TEST_CHANNEL,
                    json!({ "id": channel.id }),
                ),
            )
        })
        .collect();
    if jobs.is_empty() {
        return;
    }
    store.update(cx, |store, cx| {
        store.set_transient(TESTING_ALL_KEY, Value::Bool(true), cx);
    });
    let translator = translator.clone();
    let store = store.clone();
    window
        .spawn(cx, async move |cx| {
            let mut failures = Vec::new();
            for (name, future) in jobs {
                if let Err(detail) = test_outcome(&translator, future.await, max_channels) {
                    failures.push(format!("{name}: {detail}"));
                }
            }
            let Ok(()) = cx.update(|window, cx| {
                store.update(cx, |store, cx| {
                    store.set_transient(TESTING_ALL_KEY, Value::Bool(false), cx);
                });
                let notification = if failures.is_empty() {
                    toast::success(translator.text("cloudNotifyTestOk").to_owned())
                } else {
                    toast::error(
                        translator
                            .text_with("cloudNotifyTestFail", &[("error", &failures.join("; "))]),
                    )
                };
                window.push_notification(notification, cx);
            }) else {
                // 窗口已关闭，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
}
