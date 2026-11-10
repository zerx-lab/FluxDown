//! API 服务：本机网关的功能开关、端口、访问令牌与 LAN 暴露。

use fluxdown_protocol::GatewayPatchParams;
use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, FluxIcon, button, icon_button, loading_icon_button,
    tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClipboardItem, Entity, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, Window,
};
use gpui_component::{
    Icon, WindowExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
};

use super::SectionContext;
use crate::ui::{Control, INPUT_WIDTH, NUMBER_WIDTH, SettingsPage, SettingsSection, body_text};

mod userscript;

pub(crate) fn page(ctx: &SectionContext, cx: &mut App) -> SettingsPage {
    SettingsPage::new(
        "api",
        ctx.t("settingsCatApiService"),
        ctx.t("settingsCatApiServiceDesc"),
        FluxIcon::Code,
    )
    .sections([service_section(ctx, cx), features_section(ctx)])
}

fn service_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let gateway = ctx.store.read(cx).gateway().clone();
    let address = SharedString::from(format!("http://127.0.0.1:{}", gateway.port));
    let address_for_copy = address.clone();
    let copied = ctx.t("apiServiceCopied");
    let copy_label = ctx.t("apiServiceCopy");

    SettingsSection::new()
        .title(ctx.t("settingsCatApiService"))
        .row(
            ctx.item("apiServicePort", None, port_field(ctx))
                .description(SharedString::from(format!(
                    "{} {}",
                    ctx.t("apiServicePortDesc"),
                    if gateway.port_editable {
                        ctx.t("apiServicePortRestartHint")
                    } else {
                        ctx.t("apiServicePortFixed")
                    },
                )))
                .disabled(!gateway.port_editable),
        )
        .row(
            ctx.item(
                "apiServiceAddress",
                None,
                Control::custom(move |_, _, _, cx: &mut App| {
                    let tokens = active_theme(cx).tokens();
                    let address = address_for_copy.clone();
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .items_center()
                        .child(body_text(cx).child(address.clone()))
                        .child(
                            button(
                                "api-copy-address",
                                copy_label.clone(),
                                ButtonVariant::Secondary,
                                cx,
                            )
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    address.to_string(),
                                ));
                            }),
                        )
                }),
            )
            .keywords([copied.clone()]),
        )
        .row(
            // agent 只在启动时按此开关绑定监听地址，切换后要重启本机服务才生效。
            ctx.item(
                "apiServiceLanEnable",
                None,
                gateway_switch(ctx, GatewayFlag::Lan),
            )
            .description(SharedString::from(format!(
                "{} {}",
                ctx.t("apiServiceLanEnableDesc"),
                ctx.t("apiServiceLanRestartHint"),
            ))),
        )
        .row(ctx.item(
            "apiServiceToken",
            Some("apiServiceTokenDesc"),
            token_field(ctx),
        ))
}

fn features_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("apiServiceFeaturesTitle"))
        .subtitle(ctx.t("apiServiceFeaturesDesc"))
        .row(ctx.item(
            "apiServiceTakeover",
            Some("apiServiceTakeoverDesc"),
            gateway_switch(ctx, GatewayFlag::Takeover),
        ))
        .row(ctx.item(
            "apiServiceUserscript",
            Some("apiServiceUserscriptDesc"),
            userscript_field(ctx),
        ))
        .row(ctx.item(
            "apiServiceJsonrpc",
            Some("apiServiceJsonrpcDesc"),
            gateway_switch(ctx, GatewayFlag::Jsonrpc),
        ))
        .row(ctx.item(
            "apiServiceApi",
            Some("apiServiceApiDesc"),
            gateway_switch(ctx, GatewayFlag::Api),
        ))
        .row(ctx.item(
            "apiServiceMcp",
            Some("apiServiceMcpDesc"),
            gateway_switch(ctx, GatewayFlag::Mcp),
        ))
        .row(ctx.item(
            "apiServiceCorsAllowAll",
            Some("apiServiceCorsAllowAllDesc"),
            gateway_switch(ctx, GatewayFlag::Cors),
        ))
}

struct PortSlot {
    input: Entity<InputState>,
    last_synced: SharedString,
    submitted: bool,
    _subscription: gpui::Subscription,
}

fn port_field(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let invalid = ctx.t("apiServicePortInvalid");
    Control::field(
        NUMBER_WIDTH,
        move |disabled, key, width, window: &mut Window, cx: &mut App| {
            let snapshot = store.read(cx);
            let gateway = snapshot.gateway();
            let value = SharedString::from(gateway.port.to_string());
            let busy = snapshot.is_busy("gateway");
            let slot = window.use_keyed_state(SharedString::from(format!("{key}-port")), cx, {
                let store = store.clone();
                let value = value.clone();
                let invalid = invalid.clone();
                move |window, cx| {
                    let input =
                        cx.new(|cx| InputState::new(window, cx).default_value(value.clone()));
                    let _subscription = cx.subscribe_in(
                        &input,
                        window,
                        move |slot: &mut PortSlot, input, event: &InputEvent, window, cx| {
                            if !matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. })
                                || store.read(cx).in_flight("gateway")
                                || !store.read(cx).gateway().port_editable
                            {
                                return;
                            }
                            let text = input.read(cx).value();
                            let Ok(port) = text.trim().parse::<u16>() else {
                                window.push_notification(Notification::error(invalid.clone()), cx);
                                input.update(cx, |input, cx| {
                                    input.set_value(slot.last_synced.clone(), window, cx);
                                });
                                return;
                            };
                            if port < 1024 {
                                window.push_notification(Notification::error(invalid.clone()), cx);
                                input.update(cx, |input, cx| {
                                    input.set_value(slot.last_synced.clone(), window, cx);
                                });
                                return;
                            }
                            if port != store.read(cx).gateway().port {
                                slot.submitted = true;
                                store.update(cx, |store, cx| {
                                    store.patch_gateway(
                                        GatewayPatchParams {
                                            port: Some(port),
                                            ..Default::default()
                                        },
                                        cx,
                                    );
                                });
                            }
                        },
                    );
                    PortSlot {
                        input,
                        last_synced: value,
                        submitted: false,
                        _subscription,
                    }
                }
            });
            slot.update(cx, |slot, cx| {
                if !busy && (slot.last_synced != value || slot.submitted) {
                    slot.last_synced = value.clone();
                    slot.submitted = false;
                    slot.input
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                }
            });
            Input::new(&slot.read(cx).input)
                .control(cx)
                .w(width.unwrap_or_else(|| active_theme(cx).text_extent(NUMBER_WIDTH)))
                .disabled(disabled || busy)
        },
    )
}

fn userscript_field(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let label = ctx.t("apiServiceCopyScript");
    let copied = ctx.t("apiServiceScriptCopied");
    Control::custom(move |disabled, _, _, cx: &mut App| {
        let snapshot = store.read(cx);
        let port = snapshot.gateway().port;
        let token = snapshot
            .transient("gateway_user_token")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let busy = snapshot.is_busy("gateway") || snapshot.is_busy("gatewayToken");
        let token_ready = !snapshot.gateway_token_needs_reveal()
            && (!snapshot.gateway().user_token_configured
                || snapshot.transient("gateway_user_token").is_some());
        let copied = copied.clone();
        button(
            "api-copy-userscript",
            label.clone(),
            ButtonVariant::Secondary,
            cx,
        )
        .disabled(disabled || busy || !token_ready)
        .on_click(
            move |_, window, cx| match userscript::generate(port, &token) {
                Ok(script) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(script));
                    window.push_notification(Notification::success(copied.clone()), cx);
                }
                Err(error) => {
                    window.push_notification(Notification::error(error.to_string()), cx);
                }
            },
        )
    })
}

#[derive(Clone, Copy)]
enum GatewayFlag {
    Takeover,
    Jsonrpc,
    Api,
    Mcp,
    Cors,
    Lan,
}

fn gateway_switch(ctx: &SectionContext, flag: GatewayFlag) -> Control {
    let get = ctx.store();
    let set = ctx.store();
    Control::switch(
        move |cx: &App| {
            let gateway = get.read(cx).gateway();
            match flag {
                GatewayFlag::Takeover => gateway.takeover_enabled,
                GatewayFlag::Jsonrpc => gateway.jsonrpc_enabled,
                GatewayFlag::Api => gateway.api_enabled,
                GatewayFlag::Mcp => gateway.mcp_enabled,
                GatewayFlag::Cors => gateway.cors_enabled,
                GatewayFlag::Lan => gateway.lan_enabled,
            }
        },
        move |value, cx: &mut App| {
            let mut patch = GatewayPatchParams::default();
            match flag {
                GatewayFlag::Takeover => patch.takeover_enabled = Some(value),
                GatewayFlag::Jsonrpc => patch.jsonrpc_enabled = Some(value),
                GatewayFlag::Api => patch.api_enabled = Some(value),
                GatewayFlag::Mcp => patch.mcp_enabled = Some(value),
                GatewayFlag::Cors => patch.cors_enabled = Some(value),
                GatewayFlag::Lan => patch.lan_enabled = Some(value),
            }
            set.update(cx, |store, cx| store.patch_gateway(patch, cx));
        },
    )
}

struct TokenSlot {
    input: Entity<InputState>,
    last_synced: SharedString,
    _subscription: gpui::Subscription,
}

/// 令牌：可见可编辑（回车/失焦提交自定义值）+ 复制 / 生成 / 清空。
/// 文本经 `agent.gateway.revealToken` 按需读取，不走快照。
fn token_field(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let generate = ctx.t("apiServiceTokenGenerate");
    let clear = ctx.t("apiServiceTokenClear");
    let copy = ctx.t("apiServiceCopy");
    let copied = ctx.t("apiServiceCopied");
    let placeholder = ctx.t("proxyNotConfigured");
    Control::field(
        INPUT_WIDTH,
        move |disabled, key, width, window: &mut Window, cx: &mut App| {
            let theme = active_theme(cx);
            let tokens = theme.tokens().clone();
            let icon_size = theme.extended().icon.md;
            let snapshot = store.read(cx);
            let token: SharedString = snapshot
                .transient("gateway_user_token")
                .and_then(serde_json::Value::as_str)
                .map(|token| SharedString::from(token.to_owned()))
                .unwrap_or_default();
            let busy = snapshot.is_busy("gateway") || snapshot.is_busy("gatewayToken");
            let regenerating = snapshot.is_busy_tagged("gateway", "regenerateToken");
            let clearing = snapshot.is_busy_tagged("gateway", "clearToken");
            let just_copied = snapshot
                .transient("gateway_token_copied")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if snapshot.should_reveal_gateway_token() {
                store.update(cx, |store, cx| store.reveal_gateway_token(cx));
            }

            let slot = window.use_keyed_state(SharedString::from(format!("{key}-token")), cx, {
                let store = store.clone();
                let token = token.clone();
                let placeholder = placeholder.clone();
                move |window, cx| {
                    let input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .default_value(token.clone())
                            .placeholder(placeholder)
                    });
                    let _subscription = cx.subscribe(
                        &input,
                        move |slot: &mut TokenSlot, input, event: &InputEvent, cx| {
                            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                                let value =
                                    SharedString::from(input.read(cx).value().trim().to_owned());
                                if value != slot.last_synced {
                                    slot.last_synced = value.clone();
                                    store.update(cx, |store, cx| {
                                        store.patch_gateway(
                                            GatewayPatchParams {
                                                user_token: Some(value.to_string()),
                                                ..Default::default()
                                            },
                                            cx,
                                        );
                                    });
                                }
                            }
                        },
                    );
                    TokenSlot {
                        input,
                        last_synced: token,
                        _subscription,
                    }
                }
            });
            slot.update(cx, |slot, cx| {
                if slot.last_synced != token {
                    slot.last_synced = token.clone();
                    slot.input
                        .update(cx, |input, cx| input.set_value(token.clone(), window, cx));
                }
            });
            let input = slot.read(cx).input.clone();

            let copy_store = store.clone();
            let copy_token = token.clone();
            let generate_store = store.clone();
            let clear_store = store.clone();
            let copy_tooltip = if just_copied {
                copied.clone()
            } else {
                copy.clone()
            };
            let generate_tooltip = generate.clone();
            let clear_tooltip = clear.clone();
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(tokens.spacing.sm)
                .child(
                    Input::new(&input)
                        .control(cx)
                        .w(width.unwrap_or_else(|| active_theme(cx).text_extent(INPUT_WIDTH)))
                        .disabled(disabled || busy),
                )
                .child(
                    icon_button(
                        "api-token-copy",
                        copy.clone(),
                        Icon::new(FluxIcon::Copy).size(icon_size),
                        ButtonVariant::Secondary,
                        cx,
                    )
                    .disabled(token.is_empty())
                    .tooltip(move |window, cx| tooltip_view(copy_tooltip.clone(), window, cx))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_token.to_string()));
                        copy_store.update(cx, |store, cx| {
                            store.set_transient(
                                "gateway_token_copied",
                                serde_json::json!(true),
                                cx,
                            );
                        });
                        let reset = copy_store.clone();
                        cx.spawn(async move |cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_secs(2))
                                .await;
                            reset.update(cx, |store, cx| {
                                store.set_transient(
                                    "gateway_token_copied",
                                    serde_json::json!(false),
                                    cx,
                                );
                            });
                        })
                        .detach();
                    }),
                )
                .child(
                    loading_icon_button(
                        "api-token-generate",
                        generate.clone(),
                        Icon::new(FluxIcon::RotateCw).size(icon_size),
                        ButtonVariant::Secondary,
                        regenerating,
                        cx,
                    )
                    .disabled(disabled || busy)
                    .tooltip(move |window, cx| tooltip_view(generate_tooltip.clone(), window, cx))
                    .on_click(move |_, _, cx| {
                        generate_store.update(cx, |store, cx| {
                            store.patch_gateway(
                                GatewayPatchParams {
                                    regenerate_user_token: true,
                                    ..Default::default()
                                },
                                cx,
                            );
                            store.tag_busy("gateway", "regenerateToken");
                        });
                    }),
                )
                .child(
                    loading_icon_button(
                        "api-token-clear",
                        clear.clone(),
                        Icon::new(FluxIcon::Trash2)
                            .size(icon_size)
                            .text_color(tokens.colors.destructive),
                        ButtonVariant::Secondary,
                        clearing,
                        cx,
                    )
                    .disabled(disabled || busy || token.is_empty())
                    .tooltip(move |window, cx| tooltip_view(clear_tooltip.clone(), window, cx))
                    .on_click(move |_, _, cx| {
                        clear_store.update(cx, |store, cx| {
                            store.patch_gateway(
                                GatewayPatchParams {
                                    user_token: Some(String::new()),
                                    ..Default::default()
                                },
                                cx,
                            );
                            store.tag_busy("gateway", "clearToken");
                        });
                    }),
                )
        },
    )
}
