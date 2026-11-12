//! 代理：模式、手动服务器、连通性测试、站点凭据。

use fluxdown_protocol::method;
use fluxdown_protocol::proxy_rules::{PROXY_RULES_KEY, enabled_proxy_rule_count};
use fluxdown_ui_components::{ButtonVariant, ControlExt as _, FluxIcon, loading_button};
use fluxdown_ui_theme::active_theme;
use gpui::prelude::FluentBuilder as _;
use gpui::{App, AppContext as _, Entity, ParentElement, SharedString, Styled, Window};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, h_flex};
use serde_json::json;

use super::{SectionContext, proxy_rules, site_auth};
use crate::store::{PROXY_MANUAL_EXPANDED, rpc_error_text};
use crate::ui::{
    Control, INPUT_WIDTH, SettingsPage, SettingsRow, SettingsSection, body_text, meta_text,
    row_button,
};

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
    // 非手动模式下默认收起，只露一行摘要。
    sections.push(manual_section(ctx, mode == "manual", cx));
    sections.push(bt_section(ctx, cx));
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

/// 手动代理服务器。手动模式下始终展开；其余模式默认收起为一行摘要（类型 · 地址:端口 / 未填写）
/// + 展开按钮，收起的字段仍可被搜索找到。展开状态离开代理页即清除。
fn manual_section(ctx: &SectionContext, active: bool, cx: &mut App) -> SettingsSection {
    let types = vec![
        (SharedString::from("http"), SharedString::from("HTTP")),
        (SharedString::from("https"), SharedString::from("HTTPS")),
        (SharedString::from("socks4"), SharedString::from("SOCKS4")),
        (SharedString::from("socks5"), SharedString::from("SOCKS5")),
    ];
    let fields = [
        ctx.item("proxyType", None, ctx.daemon_dropdown("proxy_type", types)),
        ctx.item(
            "proxyHost",
            Some("proxyHostPlaceholder"),
            ctx.daemon_input("proxy_host"),
        ),
        ctx.item(
            "proxyPort",
            Some("proxyPortPlaceholder"),
            proxy_port_control(ctx),
        ),
        ctx.item(
            "proxyUsername",
            Some("proxyUsernamePlaceholder"),
            ctx.daemon_input("proxy_username"),
        ),
        ctx.item(
            "proxyPassword",
            Some("proxyPasswordPlaceholder"),
            ctx.daemon_input("proxy_password"),
        ),
        ctx.item(
            "proxyNoList",
            Some("proxyNoListDesc"),
            ctx.daemon_input("proxy_no_list"),
        ),
        ctx.item(
            "proxyTestConnection",
            None,
            test_control(ctx, ProxyTestSource::Manual),
        ),
    ];
    let section = SettingsSection::new()
        .title(ctx.t("proxyModeManual"))
        .subtitle(ctx.t(if active {
            "proxyModeManualDesc"
        } else {
            "proxyManualKeptDesc"
        }));
    if active {
        return fields.into_iter().fold(section, SettingsSection::row);
    }
    let store = ctx.store.read(cx);
    let expanded = store
        .transient(PROXY_MANUAL_EXPANDED)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let summary = manual_summary(
        &store.daemon_str("proxy_type"),
        &store.daemon_str("proxy_host"),
        &store.daemon_str("proxy_port"),
    )
    .map_or_else(|| ctx.t("proxyManualNotSet"), SharedString::from);
    let toggle_label = ctx.t(if expanded {
        "proxyManualCollapse"
    } else {
        "proxyManualExpand"
    });
    let toggle_store = ctx.store();
    let toggle = Control::custom(move |_disabled, _key, _window, cx: &mut App| {
        let store = toggle_store.clone();
        row_button(
            "proxy-manual-toggle",
            toggle_label.clone(),
            ButtonVariant::Secondary,
            cx,
        )
        .on_click(move |_, _, cx| {
            store.update(cx, |store, cx| {
                store.set_transient(PROXY_MANUAL_EXPANDED, json!(!expanded), cx);
            });
        })
    });
    let section =
        section.row(SettingsRow::new(ctx.t("proxyManualServer"), toggle).description(summary));
    if expanded {
        fields.into_iter().fold(section, SettingsSection::row)
    } else {
        fields
            .into_iter()
            .fold(section, SettingsSection::collapsed_row)
    }
}

/// 收起时的摘要：`HTTP · 127.0.0.1:1080`；没填主机时 `None`（显示「未填写」）。
fn manual_summary(proxy_type: &str, host: &str, port: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let port = port.trim();
    let endpoint = if port.is_empty() {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    };
    Some(format!("{} · {endpoint}", proxy_type.to_uppercase()))
}

/// BitTorrent 走代理：总开关 + 代理来源 + 节点连接是否也走代理 + 一条提示。
fn bt_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let section = SettingsSection::new()
        .title(ctx.t("proxyBtSection"))
        .row(ctx.item(
            "btProxyEnabled",
            Some("btProxyEnabledDesc"),
            ctx.daemon_switch("bt_proxy_enabled"),
        ));
    let store = ctx.store.read(cx);
    if !store.daemon_bool("bt_proxy_enabled") {
        return section;
    }
    let hint = bt_proxy_hint(
        &store.daemon_str("bt_proxy_source"),
        &store.daemon_str("proxy_type"),
        &store.daemon_str("proxy_host"),
        &store.daemon_str("proxy_port"),
    );
    let mut section = section
        .row(ctx.item(
            "btProxySource",
            Some("btProxySourceDesc"),
            ctx.daemon_enum_dropdown("bt_proxy_source", "btProxySource"),
        ))
        .row(ctx.item(
            "btProxyPeers",
            Some("btProxyPeersDesc"),
            ctx.daemon_switch("bt_proxy_peers"),
        ));
    if let Some(key) = hint {
        let text = ctx.t(key);
        section = section.row(SettingsRow::custom(move |_, _, _, cx: &mut App| {
            info_line(text.clone(), cx)
        }));
    }
    section
}

/// BT 代理提示（i18n 键）。系统代理来源：说明检测时机与生效方式。手动代理来源：未配置服务器
/// 优先，其次是 BT 不支持 / 无法转发 UDP 的类型；SOCKS5 无提示。
fn bt_proxy_hint(source: &str, proxy_type: &str, host: &str, port: &str) -> Option<&'static str> {
    if source == "system" {
        return Some("btProxySystemNote");
    }
    if host.trim().is_empty() || port.trim().is_empty() {
        return Some("btProxyNoServer");
    }
    match proxy_type {
        "https" => Some("btProxyUnsupportedType"),
        "http" | "socks4" => Some("btProxyNoUdp"),
        _ => None,
    }
}

struct PortSlot {
    input: Entity<InputState>,
    /// 最近一次与 store 对齐的值；store 变化（同步 / 外部改动）时才回写输入框。
    last_synced: SharedString,
    _subscription: gpui::Subscription,
}

/// 输入中：只保留数字；空串或 `1..=65535` 内的值立即提交，越界（`0`、`70000`）留待失焦。
fn live_port(text: &str) -> Option<String> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return Some(digits);
    }
    digits
        .parse::<u32>()
        .is_ok_and(|port| (1..=65535).contains(&port))
        .then_some(digits)
}

/// 失焦 / 回车：越界钳到 `1..=65535`（位数过长视同超上限），空串保持为空（未配置）。
fn settle_port(text: &str) -> String {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return digits;
    }
    digits
        .parse::<u32>()
        .map_or(65535, |port| port.clamp(1, 65535))
        .to_string()
}

/// 端口号输入：过滤非数字字符；越界值在失焦 / 回车时才夹取到 `1..=65535`，输入中可自由删改。
fn proxy_port_control(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    Control::field(
        INPUT_WIDTH,
        move |disabled, key, width, window: &mut Window, cx: &mut App| {
            let value = SharedString::from(store.read(cx).daemon_str("proxy_port"));
            let slot = window.use_keyed_state(SharedString::from(format!("{key}-port")), cx, {
                let store = store.clone();
                let value = value.clone();
                move |window, cx| {
                    let input =
                        cx.new(|cx| InputState::new(window, cx).default_value(value.clone()));
                    let _subscription = cx.subscribe_in(
                        &input,
                        window,
                        move |slot: &mut PortSlot, input, event: &InputEvent, window, cx| {
                            let text = input.read(cx).value();
                            let next = match event {
                                InputEvent::Change => live_port(&text),
                                InputEvent::Blur | InputEvent::PressEnter { .. } => {
                                    Some(settle_port(&text))
                                }
                                _ => None,
                            };
                            let Some(next) = next else {
                                return;
                            };
                            let next = SharedString::from(next);
                            if next != text {
                                input.update(cx, |input, cx| {
                                    input.set_value(next.clone(), window, cx);
                                });
                            }
                            if next != slot.last_synced {
                                slot.last_synced = next.clone();
                                store.update(cx, |store, cx| {
                                    store.set_daemon("proxy_port", next.to_string(), cx);
                                });
                            }
                        },
                    );
                    PortSlot {
                        input,
                        last_synced: value,
                        _subscription,
                    }
                }
            });
            slot.update(cx, |slot, cx| {
                if slot.last_synced != value {
                    slot.last_synced = value.clone();
                    slot.input
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                }
            });
            Input::new(&slot.read(cx).input)
                .control(cx)
                .disabled(disabled)
                .map(|this| match width {
                    Some(width) => this.w(width),
                    None => this.w_full(),
                })
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

#[cfg(test)]
mod tests {
    use super::bt_proxy_hint;

    fn manual(proxy_type: &str, host: &str, port: &str) -> Option<&'static str> {
        bt_proxy_hint("manual", proxy_type, host, port)
    }

    #[test]
    fn missing_host_or_port_wins() {
        assert_eq!(manual("socks5", "", "1080"), Some("btProxyNoServer"));
        assert_eq!(manual("https", "proxy.lan", " "), Some("btProxyNoServer"));
    }

    #[test]
    fn https_is_unsupported() {
        assert_eq!(
            manual("https", "proxy.lan", "443"),
            Some("btProxyUnsupportedType")
        );
    }

    #[test]
    fn http_and_socks4_cannot_relay_udp() {
        assert_eq!(manual("http", "proxy.lan", "8080"), Some("btProxyNoUdp"));
        assert_eq!(manual("socks4", "proxy.lan", "1080"), Some("btProxyNoUdp"));
    }

    #[test]
    fn socks5_with_server_has_no_hint() {
        assert_eq!(manual("socks5", "proxy.lan", "1080"), None);
    }

    #[test]
    fn system_source_ignores_the_manual_server() {
        assert_eq!(
            bt_proxy_hint("system", "https", "", ""),
            Some("btProxySystemNote")
        );
        assert_eq!(
            bt_proxy_hint("system", "socks5", "proxy.lan", "1080"),
            Some("btProxySystemNote")
        );
    }
}
