//! 已登录 profile 卡片：头像 + 昵称/套餐徽标 + Origin ID 胶囊 + 退出登录。
//! 视觉规则：
//! - 头像：强调色 12% 圆底 + 强调色首字符，无首字符回退 lucide `cloud`。
//! - 套餐徽标：只由 `CloudPlan.badge` 决定是否显示；颜色取 `badge_color`（回退强调色）；
//!   样式 outline / solid / medal / ribbon / plain；`badge_numbered` 时追加 `No.0001`。
//! - Origin ID：强调色 12% 底 + 强调色等宽数字 + 复制图标；无 ID 灰色 `#—` 不可点。

use fluxdown_protocol::{AgentSessionDto, CloudPlan};
use fluxdown_ui_components::{
    ButtonVariant, FluxIcon, button, card, icon_button, tabular_numbers, tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::{ExtendedTokens, SemanticThemeTokens, active_theme};
use gpui::{
    ClickEvent, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, white,
};
use gpui_component::{
    Disableable as _, Icon, Sizable as _, h_flex, spinner::Spinner, switch::Switch, v_flex,
};

use crate::assets::{CLOUD_ICON_PATH, CROWN_ICON_PATH, REFRESH_ICON_PATH};
use crate::billing::format_local_date;
use crate::profile_edit::{ProfileField, can_edit_origin_id};
use crate::view::AccountView;
use crate::{t, t_with};

/// 头像与徽标的浅底透明度。
const TINT_ALPHA: f32 = 0.12;

/// 昵称首字符大写；无可用字符返回 `None`（头像回退图标）。
pub(crate) fn avatar_initial(name: &str) -> Option<String> {
    let trimmed = name.trim();
    let first = trimmed.chars().next()?;
    Some(first.to_uppercase().collect())
}

/// 展示名：昵称为空时取邮箱 `@` 前部分。
pub(crate) fn display_name(session: &AgentSessionDto) -> String {
    if session.user.nickname.trim().is_empty() {
        session
            .user
            .email
            .split('@')
            .next()
            .unwrap_or(&session.user.email)
            .to_owned()
    } else {
        session.user.nickname.clone()
    }
}

/// 套餐徽标完整文字与颜色；`badge` 为空（服务端未配置徽标的套餐）返回 `None`。
pub(crate) fn plan_badge_text(
    plan: &CloudPlan,
    membership_ordinal: Option<i64>,
) -> Option<SharedString> {
    let badge = plan.badge.as_deref().filter(|badge| !badge.is_empty())?;
    Some(with_membership_ordinal(
        badge,
        plan.badge_numbered.then_some(membership_ordinal).flatten(),
        plan.badge_number_digits,
    ))
}

/// profile 卡片的瞬时 UI 状态；与会话数据分开传，避免参数逐个铺开。
pub(crate) struct ProfileState {
    /// Origin ID 刚被复制，胶囊显示「已复制」。
    pub copied: bool,
    /// 本机服务失联，所有动作置灰。
    pub disabled: bool,
    /// 「刷新云端信息」在途。
    pub refreshing: bool,
    /// 卡片底部常驻错误行。
    pub last_error: Option<SharedString>,
    /// 「推介有奖」入口开关当前值（设备本地偏好 `ui.show_settings_referral`）。
    pub show_referral: bool,
}

pub(crate) fn render(
    translator: &Translator,
    tokens: &SemanticThemeTokens,
    session: &AgentSessionDto,
    state: ProfileState,
    cx: &mut Context<AccountView>,
) -> impl IntoElement {
    let ProfileState {
        copied: origin_id_copied,
        disabled,
        refreshing,
        last_error,
        show_referral,
    } = state;
    let display_name = display_name(session);
    let initial = avatar_initial(&display_name);
    let extended = active_theme(cx).extended().clone();
    let plan_badge = session
        .current_plan
        .as_ref()
        .and_then(|plan| plan_badge(tokens, &extended, plan, session.user.membership_ordinal));
    let accent = tokens.colors.accent_foreground;
    let ring = tokens.colors.ring;
    let edit_size = extended.icon.lg * 1.5;
    let edit_gap = tokens.spacing.xs;
    let edit_border = extended.stroke.thin;
    let origin_id = session.user.origin_id;
    let logout_label = t(translator, "accountLogout");
    let refresh_label = t(translator, "accountCloudRefresh");
    let refresh_tooltip = refresh_label.clone();
    let copied_label = t(translator, "accountOriginIdCopied");

    let row = h_flex()
        .w_full()
        .items_center()
        .gap(tokens.spacing.md)
        .child(
            // 头像：3×lg（48）圆底；首字符用标题字号，无字符回退云图标（lg）。
            div()
                .size(extended.icon.lg * 3.)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(tokens.radius.full)
                .bg(accent.opacity(TINT_ALPHA))
                .map(|this| match &initial {
                    Some(initial) => this.child(
                        div()
                            .text_size(extended.title.size)
                            .line_height(extended.title.line_height)
                            .font_weight(extended.title.weight)
                            .text_color(accent)
                            .child(initial.clone()),
                    ),
                    None => this.child(
                        Icon::empty()
                            .path(CLOUD_ICON_PATH)
                            .size(extended.icon.lg)
                            .text_color(accent),
                    ),
                }),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .items_start()
                .gap(tokens.spacing.xs)
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .items_center()
                        .child(
                            h_flex()
                                .group("account-nickname")
                                .min_w_0()
                                .gap_0()
                                .child(
                                    div()
                                        .text_size(extended.title.size)
                                        .line_height(extended.title.line_height)
                                        .font_weight(extended.title.weight)
                                        .truncate()
                                        .text_color(tokens.colors.foreground)
                                        .child(display_name),
                                )
                                .child(
                                    icon_button(
                                        "account-nickname-edit",
                                        t(translator, "accountNicknameEditTooltip"),
                                        Icon::new(FluxIcon::Pen).size(extended.icon.sm),
                                        ButtonVariant::Ghost,
                                        cx,
                                    )
                                    // Keep the tab stop mounted, but collapse its visual footprint.
                                    .h(edit_size)
                                    .w_0()
                                    .min_w_0()
                                    .ml_0()
                                    .border_0()
                                    .overflow_hidden()
                                    .flex_none()
                                    .opacity(0.)
                                    .group_hover("account-nickname", move |style| {
                                        style
                                            .w(edit_size)
                                            .ml(edit_gap)
                                            .border(edit_border)
                                            .opacity(1.)
                                    })
                                    .focus_visible(move |style| {
                                        style
                                            .w(edit_size)
                                            .ml(edit_gap)
                                            .border(edit_border)
                                            .opacity(1.)
                                            .border_color(ring)
                                    })
                                    .disabled(disabled)
                                    .tooltip({
                                        let label = t(translator, "accountNicknameEditTooltip");
                                        move |window, cx| tooltip_view(label.clone(), window, cx)
                                    })
                                    .on_click(cx.listener(
                                        |view, _: &ClickEvent, window, cx| {
                                            view.open_profile_edit(
                                                ProfileField::Nickname,
                                                window,
                                                cx,
                                            )
                                        },
                                    )),
                                ),
                        )
                        .when_some(plan_badge, |this, badge| this.child(badge)),
                )
                .child(
                    h_flex()
                        .group("account-origin-id")
                        .gap(tokens.spacing.xs)
                        .child(origin_id_pill(
                            tokens,
                            &extended,
                            origin_id,
                            origin_id_copied,
                            copied_label,
                            cx,
                        ))
                        .when(can_edit_origin_id(session), |row| {
                            row.child(
                                icon_button(
                                    "account-origin-id-edit",
                                    t(translator, "accountOriginIdEditTooltip"),
                                    Icon::new(FluxIcon::Pen).size(extended.icon.sm),
                                    ButtonVariant::Ghost,
                                    cx,
                                )
                                .size(extended.icon.lg * 1.5)
                                .flex_none()
                                .opacity(0.)
                                .group_hover("account-origin-id", |style| style.opacity(1.))
                                .focus_visible(move |style| style.opacity(1.).border_color(ring))
                                .disabled(disabled)
                                .tooltip({
                                    let label = t(translator, "accountOriginIdEditTooltip");
                                    move |window, cx| tooltip_view(label.clone(), window, cx)
                                })
                                .on_click(cx.listener(
                                    |view, _: &ClickEvent, window, cx| {
                                        view.open_profile_edit(ProfileField::OriginId, window, cx)
                                    },
                                )),
                            )
                        }),
                ),
        )
        .child(
            h_flex()
                .flex_none()
                .items_center()
                .gap(tokens.spacing.xs)
                .child(
                    icon_button(
                        "account-cloud-refresh",
                        refresh_label,
                        if refreshing {
                            Spinner::new()
                                .with_size(extended.icon.md)
                                .icon(Icon::empty().path(REFRESH_ICON_PATH))
                                .color(tokens.colors.accent_foreground)
                                .into_any_element()
                        } else {
                            Icon::empty()
                                .path(REFRESH_ICON_PATH)
                                .size(extended.icon.md)
                                .text_color(tokens.colors.muted_foreground)
                                .into_any_element()
                        },
                        ButtonVariant::Ghost,
                        cx,
                    )
                    .disabled(disabled || refreshing)
                    .tooltip(move |window, cx| tooltip_view(refresh_tooltip.clone(), window, cx))
                    .on_click(cx.listener(
                        move |view, _: &ClickEvent, window: &mut gpui::Window, cx| {
                            view.refresh_cloud_info(window, cx);
                        },
                    )),
                )
                .child(
                    button("account-logout", logout_label, ButtonVariant::Secondary, cx)
                        .disabled(disabled)
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.sign_out(cx);
                        })),
                ),
        );

    card(cx)
        .w_full()
        .p(tokens.spacing.lg)
        .flex()
        .flex_col()
        .gap(tokens.spacing.md)
        .child(row)
        .child(crate::ui::row_divider(cx))
        .child(plan_row(
            translator, tokens, &extended, session, disabled, cx,
        ))
        .child(crate::ui::row_divider(cx))
        .child(referral_row(
            translator,
            tokens,
            show_referral,
            disabled,
            cx,
        ))
        .when_some(last_error, |this, error| {
            this.child(
                div()
                    .text_size(tokens.typography.xs.size)
                    .text_color(tokens.colors.destructive)
                    .child(error),
            )
        })
}

/// 「套餐」行：当前套餐名 + 徽标 + 「升级套餐」。
fn plan_row(
    translator: &Translator,
    tokens: &SemanticThemeTokens,
    extended: &ExtendedTokens,
    session: &AgentSessionDto,
    disabled: bool,
    cx: &mut Context<AccountView>,
) -> impl IntoElement {
    let plan = session.current_plan.as_ref();
    let plan_name = plan
        .map(|plan| {
            if plan.name.is_empty() {
                plan.code.clone()
            } else {
                plan.name.clone()
            }
        })
        .unwrap_or_else(|| "—".to_owned());
    let subscription_until = session
        .active_subscription
        .as_ref()
        .filter(|sub| !sub.ends_at.is_empty())
        .map(|sub| {
            t_with(
                translator,
                "accountSubscriptionUntil",
                &[("date", &format_local_date(&sub.ends_at))],
            )
        });
    let badge =
        plan.and_then(|plan| plan_badge(tokens, extended, plan, session.user.membership_ordinal));

    h_flex()
        .w_full()
        .items_center()
        .gap(tokens.spacing.md)
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(tokens.spacing.sm)
                .child(
                    div()
                        .flex_none()
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .text_color(tokens.colors.muted_foreground)
                        .child(t(translator, "accountPlanLabel")),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tokens.colors.foreground)
                        .child(plan_name),
                )
                .when_some(badge, |this, badge| this.child(badge))
                .when_some(subscription_until, |this, text| {
                    this.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(tokens.typography.xs.size)
                            .line_height(tokens.typography.xs.line_height)
                            .text_color(tokens.colors.muted_foreground)
                            .child(text),
                    )
                }),
        )
        .child(
            h_flex()
                .flex_none()
                .items_center()
                .gap(tokens.spacing.sm)
                .child(
                    button(
                        "account-plan-upgrade",
                        t(translator, "accountPlanUpgrade"),
                        ButtonVariant::Primary,
                        cx,
                    )
                    .disabled(disabled)
                    .on_click(cx.listener(
                        |view, _: &ClickEvent, window, cx| {
                            view.open_plan_purchase(window, cx);
                        },
                    )),
                ),
        )
}

/// 「推介有奖」入口开关（紧随套餐行）：设备本地偏好 `ui.show_settings_referral`，只控制设置侧栏
/// 是否出现该页，不进云同步目录；写偏好由 app 响应 `ShowReferralRequested`。
fn referral_row(
    translator: &Translator,
    tokens: &SemanticThemeTokens,
    checked: bool,
    disabled: bool,
    cx: &mut Context<AccountView>,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap(tokens.spacing.md)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tokens.colors.foreground)
                        .child(t(translator, "accountFeatureReferral")),
                )
                .child(
                    div()
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .text_color(tokens.colors.muted_foreground)
                        .child(t(translator, "accountFeatureReferralDesc")),
                ),
        )
        .child(
            Switch::new("account-referral-visible")
                .checked(checked)
                .disabled(disabled)
                .on_click(cx.listener(|view, checked: &bool, _, cx| {
                    view.request_show_referral(*checked, cx);
                })),
        )
}

/// 套餐徽标：`badge` 为空则不渲染（免费/专业版等服务端未配置徽标的套餐）。
pub(crate) fn plan_badge(
    tokens: &SemanticThemeTokens,
    extended: &ExtendedTokens,
    plan: &CloudPlan,
    membership_ordinal: Option<i64>,
) -> Option<gpui::AnyElement> {
    let text = plan_badge_text(plan, membership_ordinal)?;
    let color = parse_hex_color(&plan.badge_color).unwrap_or(tokens.colors.accent_foreground);
    Some(plan_tag(tokens, extended, text, color, &plan.badge_style))
}

/// 会员序号：`{badge} No.{ordinal 补零到 digits(1..=6)}`。
fn with_membership_ordinal(base: &str, ordinal: Option<i64>, digits: i64) -> SharedString {
    match ordinal {
        Some(ordinal) => {
            let width = usize::try_from(digits.clamp(1, 6)).unwrap_or(1);
            SharedString::from(format!("{base} No.{ordinal:0width$}"))
        }
        None => SharedString::from(base.to_owned()),
    }
}

/// 套餐徽标：outline | solid | medal | ribbon | plain，全部纯色/描边，不用渐变。
/// 字号统一 caption（11/14），皇冠图标 `icon.sm`，内边距走 spacing token。
fn plan_tag(
    tokens: &SemanticThemeTokens,
    extended: &ExtendedTokens,
    text: SharedString,
    color: Hsla,
    style: &str,
) -> gpui::AnyElement {
    let spacing = tokens.spacing;
    let caption = &extended.caption;
    let icon_size = extended.icon.sm;
    let stroke = extended.stroke.thin;
    let crown = move |tint: Hsla| {
        Icon::empty()
            .path(CROWN_ICON_PATH)
            .size(icon_size)
            .text_color(tint)
    };
    let label = |weight: FontWeight, tint: Hsla| {
        div()
            .text_size(caption.size)
            .line_height(caption.line_height)
            .font_weight(weight)
            .text_color(tint)
            .child(text.clone())
    };
    let pill = || h_flex().items_center().flex_none().rounded_full();
    match style {
        "outline" => pill()
            .gap(spacing.xxs)
            .px(spacing.sm)
            .border(stroke)
            .border_color(color)
            .bg(color.opacity(0.08))
            .child(crown(color))
            .child(label(FontWeight::SEMIBOLD, color))
            .into_any_element(),
        "solid" => pill()
            .px(spacing.sm)
            .bg(color)
            .child(label(FontWeight::BOLD, white()))
            .into_any_element(),
        "medal" => pill()
            .overflow_hidden()
            .border(stroke)
            .border_color(color)
            .child(
                div()
                    .flex()
                    .items_center()
                    .self_stretch()
                    .px(spacing.xs)
                    .bg(color)
                    .child(crown(white())),
            )
            .child(
                div()
                    .px(spacing.xs)
                    .child(label(FontWeight::SEMIBOLD, color)),
            )
            .into_any_element(),
        "ribbon" => pill()
            .px(spacing.sm)
            .bg(color)
            .child(label(FontWeight::EXTRA_BOLD, white()))
            .into_any_element(),
        _ => pill()
            .px(spacing.xs + spacing.xxs)
            .bg(color.opacity(TINT_ALPHA))
            .child(label(FontWeight::SEMIBOLD, color))
            .into_any_element(),
    }
}

/// 接受可选 `#` 前缀的 6 位 `RRGGBB` 或 8 位 `AARRGGBB`。
pub(crate) fn parse_hex_color(value: &str) -> Option<Hsla> {
    let hex = value.trim().trim_start_matches('#');
    let parsed = u32::from_str_radix(hex, 16).ok()?;
    let (argb, alpha) = match hex.len() {
        6 => (parsed, 1.),
        8 => (parsed & 0x00FF_FFFF, f32::from((parsed >> 24) as u8) / 255.),
        _ => return None,
    };
    let mut color: Hsla = gpui::rgb(argb).into();
    color.a = alpha;
    Some(color)
}

fn origin_id_pill(
    tokens: &SemanticThemeTokens,
    extended: &ExtendedTokens,
    origin_id: Option<i64>,
    copied: bool,
    copied_label: SharedString,
    cx: &mut Context<AccountView>,
) -> impl IntoElement {
    let color = if origin_id.is_some() {
        tokens.colors.accent_foreground
    } else {
        tokens.colors.muted_foreground
    };
    let label = if copied {
        copied_label
    } else {
        match origin_id {
            Some(id) => SharedString::from(format!("#{id}")),
            None => SharedString::from("#—"),
        }
    };
    let pill = h_flex()
        .id("account-origin-id")
        .items_center()
        .px(tokens.spacing.sm)
        .py(tokens.spacing.xxs)
        .gap(tokens.spacing.xs)
        .rounded_full()
        .bg(color.opacity(TINT_ALPHA))
        .text_size(tokens.typography.xs.size)
        .line_height(tokens.typography.xs.line_height)
        .font_weight(FontWeight::MEDIUM)
        .font_features(tabular_numbers())
        .text_color(color)
        .child(label);
    if let Some(origin_id) = origin_id {
        pill.cursor_pointer()
            .child(
                Icon::new(FluxIcon::Copy)
                    .size(extended.icon.sm)
                    .text_color(color),
            )
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.copy_origin_id(origin_id, cx);
            }))
    } else {
        pill
    }
}
