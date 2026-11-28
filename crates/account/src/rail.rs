//! 主窗口活动栏底部的账户入口，点击在头像右侧展开账户卡片。
//!
//! - 头像：未登录为人形图标；已登录为首字母，套餐带徽标（如创始会员）时叠加徽标色环与皇冠角标。
//! - 卡片（已登录）：大头像 / 展示名 / 邮箱 / 完整套餐徽标、配置同步开关与状态、云端连接状态，
//!   以及「添加设备 / 账户设置 / 退出登录」。
//! - 卡片（未登录）：说明 + 登录 / 注册 + 账户设置。
//!
//! 打开设置窗口由宿主注入（shell 与账户 crate 都不认识设置窗口）；其余动作复用本 crate 的对话框与端口。
//! 失败以窗口通知呈现，成功结果经会话 / 同步事件回流到 [`AccountHost`] 再重渲染。

use std::rc::Rc;

use fluxdown_protocol::{CloudConnectionState, method};
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{
    ButtonVariant, FluxIcon, activity_button, button, nav_icon_color, sidebar_navigation_button,
    tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, App, Context, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement as _,
    Styled, WeakEntity, Window, div, prelude::FluentBuilder as _, white,
};
use gpui_component::{
    Disableable as _, Icon, WindowExt as _, h_flex, popover::Popover, switch::Switch, v_flex,
};

use crate::assets::CROWN_ICON_PATH;
use crate::errors::{ErrorContext, error_text};
use crate::host::AccountHost;
use crate::pages::cloud_features::sync_subtitle;
use crate::pages::profile::{
    avatar_initial, display_name, parse_hex_color, plan_badge, plan_badge_text,
};
use crate::sync_scope::sync_phase;
use crate::ui::row_divider;
use crate::{AccountCommand, dialogs, t};

/// 活动栏头像直径相对 `icon.lg` 的倍数。
const RAIL_AVATAR_SCALE: f32 = 1.4;
/// 卡片头像直径相对 `icon.lg` 的倍数。
const CARD_AVATAR_SCALE: f32 = 2.5;
/// 头像底色透明度（与 profile 卡片头像一致）。
const TINT_ALPHA: f32 = 0.12;
/// 卡片基准宽度（逻辑像素，随文字缩放经 `text_extent` 放大）。
const CARD_WIDTH: f32 = 288.;

type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// 活动栏账户按钮与其展开卡片；观察 [`AccountHost`]，会话 / 套餐 / 同步 / 语言变化时自行刷新。
pub struct AccountRailButton {
    host: Entity<AccountHost>,
    size: Pixels,
    on_open_settings: ClickHandler,
    open: bool,
}

impl AccountRailButton {
    /// `size` 为活动栏按钮位边长；`on_open_settings` 打开设置窗口的账户页。
    pub fn new(
        host: Entity<AccountHost>,
        size: Pixels,
        on_open_settings: impl Fn(&mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&host, |_, _, cx| cx.notify()).detach();
        Self {
            host,
            size,
            on_open_settings: Rc::new(on_open_settings),
            open: false,
        }
    }
}

/// 已登录账户的展示数据。
struct Identity {
    name: String,
    email: String,
    initial: Option<String>,
    /// 完整徽标文字（`badge` + 可选编号）与徽标色；套餐未配置徽标时为 `None`。
    badge: Option<(SharedString, Hsla)>,
}

fn identity(host: &AccountHost, accent: Hsla) -> Option<Identity> {
    let session = host.controller.session()?;
    let name = display_name(session);
    let badge = session.current_plan.as_ref().and_then(|plan| {
        let text = plan_badge_text(plan, session.user.membership_ordinal)?;
        Some((text, parse_hex_color(&plan.badge_color).unwrap_or(accent)))
    });
    Some(Identity {
        initial: avatar_initial(&name),
        email: session.user.email.clone(),
        name,
        badge,
    })
}

/// 首字母头像；`badge_color` 存在时外加徽标色环与右上角皇冠角标。
fn avatar(initial: Option<&str>, badge_color: Option<Hsla>, large: bool, cx: &App) -> AnyElement {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    let accent = tokens.colors.accent_foreground;
    let (diameter, text) = if large {
        (extended.icon.lg * CARD_AVATAR_SCALE, &extended.title)
    } else {
        (extended.icon.lg * RAIL_AVATAR_SCALE, &extended.caption)
    };
    let face = div()
        .size(diameter)
        .flex()
        .items_center()
        .justify_center()
        .rounded(tokens.radius.full)
        .bg(accent.opacity(TINT_ALPHA))
        .text_color(accent)
        .text_size(text.size)
        .line_height(text.line_height)
        .font_weight(FontWeight::SEMIBOLD)
        .map(|this| match initial {
            Some(initial) => this.child(initial.to_owned()),
            None => this.child(Icon::new(FluxIcon::User).size(diameter * 0.6)),
        });
    let Some(color) = badge_color else {
        return face.into_any_element();
    };
    let ring = extended.stroke.thin * 2.;
    let crown = if large {
        extended.icon.md
    } else {
        extended.icon.sm
    };
    div()
        .relative()
        .size(diameter + ring * 2.)
        .flex()
        .items_center()
        .justify_center()
        .rounded(tokens.radius.full)
        .border(ring)
        .border_color(color)
        .child(face)
        .child(
            div()
                .absolute()
                .top(-crown / 2.)
                .right(-crown / 2.)
                .size(crown)
                .flex()
                .items_center()
                .justify_center()
                .rounded(tokens.radius.full)
                .bg(color)
                .child(
                    Icon::empty()
                        .path(CROWN_ICON_PATH)
                        .size(crown * 0.7)
                        .text_color(white()),
                ),
        )
        .into_any_element()
}

/// 收起卡片（受控 Popover 的开合状态由本视图持有）。
fn set_open(this: &WeakEntity<AccountRailButton>, open: bool, cx: &mut App) {
    let Ok(()) = this.update(cx, |this, cx| {
        if this.open != open {
            this.open = open;
            cx.notify();
        }
    }) else {
        // 头像视图已随主窗口释放，没有需要开合的卡片。
        return;
    };
}

/// 发起账户命令；失败在当前窗口弹出错误通知。`sign_out` 时若本机会话已清除则不提示——
/// agent 先清本地会话再回报远端吊销失败（如离线），此时错误只会误导。
fn run(
    host: &Entity<AccountHost>,
    command: AccountCommand,
    context: ErrorContext,
    sign_out: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let future = host.read(cx).port().execute(command);
    let host = host.clone();
    window
        .spawn(cx, async move |cx| {
            let Err(error) = future.await else {
                return;
            };
            let Ok(()) = cx.update(|window, cx| {
                let host = host.read(cx);
                if sign_out && host.controller.session().is_none() {
                    return;
                }
                let message = error_text(host.translator().read(cx), &error, context);
                window.push_notification(toast::error(message), cx);
            }) else {
                // 窗口已关闭，失败无处提示。
                return;
            };
        })
        .detach();
}

/// 卡片里的菜单行：图标 + 文字，悬停浅底。
fn menu_row(
    id: &'static str,
    label: SharedString,
    icon: FluxIcon,
    color: Option<Hsla>,
    cx: &App,
) -> fluxdown_ui_components::Button {
    let theme = active_theme(cx);
    let icon_color = color.unwrap_or_else(|| nav_icon_color(false, cx));
    sidebar_navigation_button(
        id,
        label,
        Icon::new(icon)
            .size(theme.extended().icon.md)
            .text_color(icon_color),
        div(),
        false,
        cx,
    )
    .when_some(color, |this, color| this.text_color(color))
}

fn render_card(
    this: &WeakEntity<AccountRailButton>,
    host: &Entity<AccountHost>,
    on_open_settings: &ClickHandler,
    cx: &App,
) -> AnyElement {
    let host_ref = host.read(cx);
    let translator_entity = host_ref.translator().clone();
    let translator = translator_entity.read(cx);
    let controller = &host_ref.controller;
    let stale = controller.is_stale();
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    let colors = tokens.colors;
    let typography = &tokens.typography;

    let settings_row = menu_row(
        "account-card-settings",
        t(translator, "accountOpenSettings"),
        FluxIcon::Settings,
        None,
        cx,
    )
    .on_click({
        let this = this.clone();
        let on_open_settings = Rc::clone(on_open_settings);
        move |_, window, cx| {
            set_open(&this, false, cx);
            on_open_settings(window, cx);
        }
    });
    let card = v_flex()
        .w_full()
        .p(tokens.spacing.md)
        .gap(tokens.spacing.sm);

    let Some(identity) = identity(host_ref, colors.accent_foreground) else {
        let port = host_ref.port();
        let login_port = port.clone();
        let login_translator = translator_entity.clone();
        let login_this = this.clone();
        let register_translator = translator_entity.clone();
        let register_this = this.clone();
        let header = v_flex()
            .w_full()
            .items_center()
            .gap(tokens.spacing.xs)
            .child(
                div()
                    .size(extended.icon.lg * CARD_AVATAR_SCALE)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(tokens.radius.full)
                    .bg(colors.accent)
                    .child(
                        Icon::new(FluxIcon::CircleUser)
                            .size(extended.icon.lg * 1.5)
                            .text_color(colors.accent_foreground),
                    ),
            )
            .child(
                div()
                    .text_size(typography.sm.size)
                    .line_height(typography.sm.line_height)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.foreground)
                    .child(t(translator, "accountLoginDialogTitle")),
            )
            .child(
                div()
                    .text_size(typography.xs.size)
                    .line_height(typography.xs.line_height)
                    .text_color(colors.muted_foreground)
                    .text_center()
                    .child(t(translator, "accountHeroSubtitle")),
            );
        let actions = h_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(
                button(
                    "account-card-login",
                    t(translator, "accountLogin"),
                    ButtonVariant::Primary,
                    cx,
                )
                .flex_1()
                .disabled(stale)
                .on_click(move |_, window, cx| {
                    set_open(&login_this, false, cx);
                    dialogs::login::open(login_translator.clone(), login_port.clone(), window, cx);
                }),
            )
            .child(
                button(
                    "account-card-register",
                    t(translator, "accountRegister"),
                    ButtonVariant::Secondary,
                    cx,
                )
                .flex_1()
                .disabled(stale)
                .on_click(move |_, window, cx| {
                    set_open(&register_this, false, cx);
                    dialogs::register::open(register_translator.clone(), port.clone(), window, cx);
                }),
            );
        return card
            .child(header)
            .child(actions)
            .child(row_divider(cx))
            .child(settings_row)
            .into_any_element();
    };

    let session = controller.session();
    let header = h_flex()
        .w_full()
        .items_center()
        .gap(tokens.spacing.md)
        .child(avatar(
            identity.initial.as_deref(),
            identity.badge.as_ref().map(|(_, color)| *color),
            true,
            cx,
        ))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(tokens.spacing.xxs)
                .child(
                    div()
                        .truncate()
                        .text_size(typography.sm.size)
                        .line_height(typography.sm.line_height)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.foreground)
                        .child(identity.name),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(typography.xs.size)
                        .line_height(typography.xs.line_height)
                        .text_color(colors.muted_foreground)
                        .child(identity.email),
                ),
        );
    let badge = session.and_then(|session| {
        let plan = session.current_plan.as_ref()?;
        plan_badge(tokens, extended, plan, session.user.membership_ordinal)
    });

    let sync = controller.sync_status();
    let sync_host = host.clone();
    let sync_row = h_flex()
        .w_full()
        .items_center()
        .gap(tokens.spacing.sm)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(typography.sm.size)
                        .line_height(typography.sm.line_height)
                        .text_color(colors.foreground)
                        .child(t(translator, "cloudSyncTitle")),
                )
                .child(
                    div()
                        .text_size(typography.xs.size)
                        .line_height(typography.xs.line_height)
                        .text_color(colors.muted_foreground)
                        .child(sync_subtitle(translator, sync_phase(true, sync))),
                ),
        )
        .child(
            Switch::new("account-card-sync")
                .checked(sync.enabled)
                .disabled(stale)
                .accessibility_label(t(translator, "cloudSyncTitle"))
                .on_click(move |checked, window, cx| {
                    let method = if *checked {
                        method::AGENT_SYNC_ENABLE
                    } else {
                        method::AGENT_SYNC_DISABLE
                    };
                    run(
                        &sync_host,
                        AccountCommand::Sync {
                            method,
                            params: serde_json::json!({}),
                        },
                        ErrorContext::Sync,
                        false,
                        window,
                        cx,
                    );
                }),
        );

    let (connection_key, connection_color) = match controller.cloud_connection().state {
        CloudConnectionState::Connected => ("cloudConnectionConnected", extended.colors.success),
        CloudConnectionState::Connecting => ("cloudConnectionConnecting", extended.colors.warning),
        CloudConnectionState::Reconnecting => {
            ("cloudConnectionReconnecting", extended.colors.warning)
        }
        CloudConnectionState::Disconnected => {
            ("cloudConnectionDisconnected", extended.colors.text_tertiary)
        }
    };
    let connection_row = h_flex()
        .w_full()
        .items_center()
        .gap(tokens.spacing.sm)
        .child(
            div()
                .size(extended.icon.sm * 0.5)
                .flex_none()
                .rounded(tokens.radius.full)
                .bg(connection_color),
        )
        .child(
            div()
                .text_size(typography.xs.size)
                .line_height(typography.xs.line_height)
                .text_color(colors.muted_foreground)
                .child(t(translator, connection_key)),
        );

    let add_device_host = host.clone();
    let add_device_this = this.clone();
    let add_device_row = menu_row(
        "account-card-add-device",
        t(translator, "addDeviceEntry"),
        FluxIcon::Plus,
        None,
        cx,
    )
    .disabled(stale)
    .on_click(move |_, window, cx| {
        set_open(&add_device_this, false, cx);
        dialogs::add_device::open(&add_device_host, window, cx);
    });
    let logout_host = host.clone();
    let logout_this = this.clone();
    let logout_row = menu_row(
        "account-card-logout",
        t(translator, "accountLogout"),
        FluxIcon::Power,
        Some(colors.destructive),
        cx,
    )
    .disabled(stale)
    .on_click(move |_, window, cx| {
        set_open(&logout_this, false, cx);
        run(
            &logout_host,
            AccountCommand::Auth {
                method: method::AGENT_AUTH_LOGOUT,
                params: serde_json::json!({}),
            },
            ErrorContext::General,
            true,
            window,
            cx,
        );
    });

    card.child(header)
        .children(badge.map(|badge| h_flex().w_full().child(badge)))
        .child(row_divider(cx))
        .child(sync_row)
        .child(connection_row)
        .child(row_divider(cx))
        .child(
            v_flex()
                .w_full()
                .child(add_device_row)
                .child(settings_row)
                .child(logout_row),
        )
        .into_any_element()
}

impl Render for AccountRailButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self.host.read(cx);
        let translator = host.translator().read(cx);
        let theme = active_theme(cx);
        let extended = theme.extended();
        let identity = identity(host, theme.tokens().colors.accent_foreground);
        let tooltip = match &identity {
            None => t(translator, "accountLogin"),
            Some(identity) => match &identity.badge {
                Some((text, _)) => SharedString::from(format!("{} · {text}", identity.name)),
                None => SharedString::from(identity.name.clone()),
            },
        };
        let glyph = match &identity {
            None => Icon::new(FluxIcon::CircleUser)
                .size(extended.icon.lg + extended.stroke.thin * 2.)
                .into_any_element(),
            Some(identity) => avatar(
                identity.initial.as_deref(),
                identity.badge.as_ref().map(|(_, color)| *color),
                false,
                cx,
            ),
        };
        let trigger = activity_button(
            "activity-account",
            t(translator, "settingsCatAccount"),
            glyph,
            self.open,
            self.size,
            cx,
        );
        let card_width = theme.text_extent(CARD_WIDTH);
        let this = cx.entity().downgrade();
        let change_this = this.clone();
        let host = self.host.clone();
        let on_open_settings = Rc::clone(&self.on_open_settings);
        let popover = Popover::new("account-rail-popover")
            .anchor(Anchor::LeftCenter)
            .p_0()
            .w(card_width)
            .open(self.open)
            .on_open_change(move |open, _, cx| set_open(&change_this, *open, cx))
            .trigger(trigger)
            .content(move |_, _, cx| render_card(&this, &host, &on_open_settings, cx));

        div()
            .id("activity-account-tooltip")
            .when(!self.open, |this| {
                this.tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
            })
            .child(popover)
    }
}
