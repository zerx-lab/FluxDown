//! 代理：模式、手动服务器、连通性测试、站点凭据。

use fluxdown_protocol::method;
use fluxdown_protocol::proxy_rules::{PROXY_RULES_KEY, enabled_proxy_rule_count};
use fluxdown_ui_components::{ButtonVariant, FluxIcon, loading_button};
use fluxdown_ui_theme::active_theme;
use gpui::{App, ParentElement, SharedString, Styled};
use gpui_component::{Icon, h_flex};
use serde_json::json;

use super::{SectionContext, proxy_rules, site_auth};
use crate::store::rpc_error_text;
use crate::ui::{Control, SettingsPage, SettingsRow, SettingsSection, body_text, meta_text};

pub(crate) fn page(ctx: &SectionContext, cx: &mut App) -> SettingsPage {
    let mode = ctx.store.read(cx).daemon_str("proxy_mode");
    if mode == "system"
        && ctx.store.read(cx).system_proxy().is_none()
        && !ctx.store.read(cx).load_attempted("systemProxy")
        && ctx
            .store
            .update(cx, |store, _| store.begin_load("systemProxy"))
    {
        ctx.store
            .update(cx, |store, cx| store.load_system_proxy(cx));
    }
    let rule_count = enabled_proxy_rule_count(&ctx.store.read(cx).daemon_str(PROXY_RULES_KEY));
    let mut sections = vec![mode_section(ctx, rule_count)];
    match mode.as_str() {
        "auto" => sections.push(auto_desc_section(ctx)),
        "system" => sections.push(system_section(ctx, cx)),
        _ => {}
    }
    // 手动服务器在任何模式下都可编辑：「自动」把它当备选路径，站点规则与任务的「手动代理」出口也读它。
    sections.push(manual_section(ctx, mode == "manual"));
    sections.push(proxy_rules::group(ctx, cx));
    sections.push(site_auth::group(ctx, cx));
    SettingsPage::new(
        "proxy",
        ctx.t("settingsCatProxy"),
        ctx.t("settingsCatProxyDesc"),
        FluxIcon::Globe,
    )
    .sections(sections)
}

fn mode_section(ctx: &SectionContext, rule_count: usize) -> SettingsSection {
    let options = vec![
        (SharedString::from("none"), ctx.t("proxyModeNone")),
        (SharedString::from("system"), ctx.t("proxyModeSystem")),
        (SharedString::from("manual"), ctx.t("proxyModeManual")),
        (SharedString::from("auto"), ctx.t("proxyModeAuto")),
    ];
    let get = ctx.store();
    let set = ctx.store();
    let control = Control::dropdown(
        options,
        move |cx: &App| SharedString::from(get.read(cx).daemon_str("proxy_mode")),
        move |value: SharedString, cx: &mut App| {
            set.update(cx, |store, cx| {
                store.set_daemon("proxy_mode", value.to_string(), cx);
                // 切到系统代理模式时立即重新检测（系统设置可能在两次切换之间变化）。
                if value.as_ref() == "system" && !store.in_flight("systemProxy") {
                    store.load_system_proxy(cx);
                }
            });
        },
    );
    let mut section = SettingsSection::new()
        .title(ctx.t("proxySettings"))
        .subtitle(ctx.t("proxyBtNote"))
        .row(ctx.item("proxySettings", Some("proxySettingsDesc"), control));
    if rule_count > 0 {
        let hint = SharedString::from(ctx.translator.text_with(
            "proxyRulesActiveHint",
            &[("count", &rule_count.to_string())],
        ));
        section = section.row(SettingsRow::custom(move |_, _, _, cx: &mut App| {
            info_line(hint.clone(), cx)
        }));
    }
    section
}

/// `auto` 模式：只读说明行，无可编辑表单。
fn auto_desc_section(ctx: &SectionContext) -> SettingsSection {
    let desc = ctx.t("proxyModeAutoDesc");
    SettingsSection::new().row(SettingsRow::custom(move |_, _, _, cx: &mut App| {
        info_line(desc.clone(), cx)
    }))
}

/// `system` 模式：检测状态 + 只读的系统代理详情（不可编辑）。
fn system_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let busy = ctx.store.read(cx).is_busy("systemProxy");
    let snapshot = ctx.store.read(cx).system_proxy().cloned();

    let status_text = if busy {
        ctx.t("proxySystemDetecting")
    } else {
        match &snapshot {
            Some(dto) if dto.detected => ctx.t("proxySystemDetected"),
            Some(_) => ctx.t("proxySystemNotConfigured"),
            None => ctx.t("proxySystemDetecting"),
        }
    };

    let mut section = SettingsSection::new()
        .title(ctx.t("proxyModeSystem"))
        .subtitle(ctx.t("proxyModeSystemDesc"))
        .row(SettingsRow::custom(move |_, _, _, cx: &mut App| {
            info_line(status_text.clone(), cx)
        }));

    if let Some(dto) = snapshot.as_ref()
        && dto.detected
        && !busy
    {
        section = section
            .row(ctx.item(
                "proxyType",
                None,
                readonly_control(SharedString::from(dto.proxy_type.to_uppercase())),
            ))
            .row(ctx.item(
                "proxyHost",
                None,
                readonly_control(SharedString::from(dto.host.clone())),
            ))
            .row(ctx.item(
                "proxyPort",
                None,
                readonly_control(SharedString::from(dto.port.to_string())),
            ));
        if !dto.no_list.is_empty() {
            section = section.row(ctx.item(
                "proxyNoList",
                None,
                readonly_control(SharedString::from(dto.no_list.clone())),
            ));
        }
        section = section.row(ctx.item(
            "proxyTestConnection",
            None,
            test_control(ctx, ProxyTestSource::System),
        ));
    }
    section
}

fn manual_section(ctx: &SectionContext, active: bool) -> SettingsSection {
    let types = vec![
        (SharedString::from("http"), SharedString::from("HTTP")),
        (SharedString::from("https"), SharedString::from("HTTPS")),
        (SharedString::from("socks4"), SharedString::from("SOCKS4")),
        (SharedString::from("socks5"), SharedString::from("SOCKS5")),
    ];
    SettingsSection::new()
        .title(ctx.t("proxyModeManual"))
        .subtitle(ctx.t(if active {
            "proxyModeManualDesc"
        } else {
            "proxyManualKeptDesc"
        }))
        .row(ctx.item("proxyType", None, ctx.daemon_dropdown("proxy_type", types)))
        .row(ctx.item(
            "proxyHost",
            Some("proxyHostPlaceholder"),
            ctx.daemon_input("proxy_host"),
        ))
        .row(ctx.item(
            "proxyPort",
            Some("proxyPortPlaceholder"),
            proxy_port_control(ctx),
        ))
        .row(ctx.item(
            "proxyUsername",
            Some("proxyUsernamePlaceholder"),
            ctx.daemon_input("proxy_username"),
        ))
        .row(ctx.item(
            "proxyPassword",
            Some("proxyPasswordPlaceholder"),
            ctx.daemon_input("proxy_password"),
        ))
        .row(ctx.item(
            "proxyNoList",
            Some("proxyNoListDesc"),
            ctx.daemon_input("proxy_no_list"),
        ))
        .row(ctx.item(
            "proxyTestConnection",
            None,
            test_control(ctx, ProxyTestSource::Manual),
        ))
}

/// 端口号输入：过滤非数字字符并夹取到 `1..=65535`。
fn proxy_port_control(ctx: &SectionContext) -> Control {
    let get = ctx.store();
    let set = ctx.store();
    Control::input(
        move |cx: &App| SharedString::from(get.read(cx).daemon_str("proxy_port")),
        move |value: SharedString, cx: &mut App| {
            let digits: String = value.chars().filter(char::is_ascii_digit).collect();
            let clamped = digits
                .parse::<u32>()
                .ok()
                .map(|port| port.clamp(1, 65535).to_string())
                .unwrap_or_default();
            set.update(cx, |store, cx| store.set_daemon("proxy_port", clamped, cx));
        },
    )
}

/// 只读值展示（禁用态，不可编辑）。
fn readonly_control(value: SharedString) -> Control {
    Control::custom(move |_disabled, _key, _window, cx: &mut App| {
        let tokens = active_theme(cx).tokens();
        body_text(cx)
            .text_color(tokens.colors.muted_foreground)
            .child(value.clone())
    })
}

/// info 图标 + 说明文字的整行（无标题/控件结构）。
pub(super) fn info_line(text: SharedString, cx: &mut App) -> gpui::Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    h_flex()
        .gap(tokens.spacing.sm)
        .items_center()
        .child(
            Icon::new(FluxIcon::Info)
                .size(extended.icon.md)
                .text_color(extended.colors.text_tertiary),
        )
        .child(meta_text(cx).flex_1().min_w_0().child(text))
}

#[derive(Clone, Copy)]
enum ProxyTestSource {
    Manual,
    System,
}

impl ProxyTestSource {
    /// 手动与系统两行可同屏（系统模式），按钮 id、忙碌键与结果键各自独立。
    fn keys(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Manual => (
                "proxy-test-manual",
                "proxyTestManual",
                "proxy_test_result_manual",
            ),
            Self::System => (
                "proxy-test-system",
                "proxyTestSystem",
                "proxy_test_result_system",
            ),
        }
    }
}

fn test_control(ctx: &SectionContext, source: ProxyTestSource) -> Control {
    let store = ctx.store();
    let label = ctx.t("proxyTestConnection");
    let testing = ctx.t("proxyTesting");
    let translator = ctx.translator.clone();
    let (button_id, busy_key, result_key) = source.keys();
    Control::custom(move |_disabled, _key, _window, cx: &mut App| {
        let tokens = active_theme(cx).tokens();
        let busy = store.read(cx).is_busy(busy_key);
        let result = store
            .read(cx)
            .transient(result_key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let click_store = store.clone();
        h_flex()
            .gap(tokens.spacing.sm)
            .items_center()
            .child(meta_text(cx).child(SharedString::from(result.unwrap_or_default())))
            .child(
                loading_button(
                    button_id,
                    if busy { testing.clone() } else { label.clone() },
                    ButtonVariant::Secondary,
                    busy,
                    cx,
                )
                .disabled(busy)
                .on_click({
                    let translator = translator.clone();
                    move |_, _, cx| {
                        let translator = translator.clone();
                        click_store.update(cx, |store, cx| {
                            let params = match source {
                                ProxyTestSource::Manual => json!({
                                    "proxyType": store.daemon_str("proxy_type"),
                                    "host": store.daemon_str("proxy_host"),
                                    "port": store.daemon_str("proxy_port"),
                                    "username": store.daemon_str("proxy_username"),
                                    "password": store.daemon_str("proxy_password"),
                                }),
                                ProxyTestSource::System => {
                                    let dto = store.system_proxy().cloned().unwrap_or_default();
                                    json!({
                                        "proxyType": dto.proxy_type,
                                        "host": dto.host,
                                        "port": dto.port.to_string(),
                                        "username": "",
                                        "password": "",
                                    })
                                }
                            };
                            store.call_with(
                                busy_key,
                                method::DAEMON_CONFIG_PROXY_TEST,
                                params,
                                cx,
                                move |store, result, cx| {
                                    let text = match result {
                                        Ok(value) => {
                                            let ms = value
                                                .get("latencyMs")
                                                .and_then(serde_json::Value::as_i64)
                                                .unwrap_or(0)
                                                .to_string();
                                            translator.text_with("proxyTestSuccess", &[("ms", &ms)])
                                        }
                                        Err(error) => translator.text_with(
                                            "proxyTestFailed",
                                            &[("error", &rpc_error_text(&translator, &error))],
                                        ),
                                    };
                                    store.set_transient(result_key, json!(text), cx);
                                },
                            );
                        });
                    }
                }),
            )
    })
}
