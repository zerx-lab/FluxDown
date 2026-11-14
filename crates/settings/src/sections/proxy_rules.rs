//! 站点代理规则：列表（拖动排序）、测试网址与作用范围说明。

use fluxdown_protocol::proxy_rules::{
    PROXY_RULES_KEY, ProxyRuleSpec, encode_proxy_rules, parse_proxy_rules, proxy_display_endpoint,
};
use fluxdown_protocol::{ProxyRouteResponse, ProxyRuleIssueDto, method};
use fluxdown_ui_components::{
    ButtonVariant, DialogIntent, FluxIcon, button, dialog_title, loading_button, tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, Context, Div, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled, Subscription,
    Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, Icon, WindowExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    switch::Switch,
    v_flex,
};
use serde_json::{Value, json};

use super::proxy::info_line;
use super::{SectionContext, proxy_rule_dialog};
use crate::store::{SettingsStore, rpc_error_text};
use crate::ui::{
    SettingsRow, SettingsSection, body_text, empty_state, meta_text, row_button, row_danger_button,
};

/// 「测试网址」结果在 `transient` 里的键。
const ROUTE_RESULT_KEY: &str = "proxy_route_result";

// ───────────────────────── 模型辅助 ─────────────────────────

pub(crate) fn read_rules(store: &SettingsStore) -> Vec<ProxyRuleSpec> {
    parse_proxy_rules(&store.daemon_str(PROXY_RULES_KEY))
}

/// 读-改-写规则列表；`edit` 返回 `None` 表示无需写入。
pub(crate) fn mutate_rules(
    store: &mut SettingsStore,
    edit: impl Fn(Vec<ProxyRuleSpec>) -> Option<Vec<ProxyRuleSpec>> + 'static,
    cx: &mut Context<SettingsStore>,
) {
    store.mutate_daemon(
        PROXY_RULES_KEY,
        move |raw| edit(parse_proxy_rules(raw)).and_then(|list| encode_proxy_rules(&list).ok()),
        cx,
    );
}

/// 按 id 覆盖；不存在则追加。
pub(crate) fn upsert(mut list: Vec<ProxyRuleSpec>, draft: ProxyRuleSpec) -> Vec<ProxyRuleSpec> {
    match list.iter_mut().find(|rule| rule.id == draft.id) {
        Some(slot) => *slot = draft,
        None => list.push(draft),
    }
    list
}

pub(crate) fn set_enabled(
    mut list: Vec<ProxyRuleSpec>,
    id: &str,
    enabled: bool,
) -> Vec<ProxyRuleSpec> {
    if let Some(rule) = list.iter_mut().find(|rule| rule.id == id) {
        rule.enabled = enabled;
    }
    list
}

pub(crate) fn remove(mut list: Vec<ProxyRuleSpec>, id: &str) -> Vec<ProxyRuleSpec> {
    list.retain(|rule| rule.id != id);
    list
}

/// 把 `from` 移到 `to` 当前所在位置（先移除再插入：向下拖落在目标之后，向上拖落在目标之前）。
/// 无变化（同一项 / id 不存在）返回 false。
pub(crate) fn reorder(list: &mut Vec<ProxyRuleSpec>, from: &str, to: &str) -> bool {
    let (Some(from), Some(to)) = (
        list.iter().position(|rule| rule.id == from),
        list.iter().position(|rule| rule.id == to),
    ) else {
        return false;
    };
    if from == to {
        return false;
    }
    let rule = list.remove(from);
    list.insert(to, rule);
    true
}

/// 手动代理字段不完整：host 为空，或端口不在 1..=65535。
pub(crate) fn manual_incomplete(host: &str, port: &str) -> bool {
    host.trim().is_empty() || !port.trim().parse::<u16>().is_ok_and(|port| port > 0)
}

/// 手动代理端点 `type://host:port`（不含凭据）。
pub(crate) fn manual_endpoint(store: &SettingsStore) -> String {
    let kind = store.daemon_str("proxy_type");
    let kind = if kind.trim().is_empty() {
        "http"
    } else {
        kind.trim()
    };
    let host = store.daemon_str("proxy_host");
    let host = host.trim();
    let port = store.daemon_str("proxy_port");
    if host.contains(':') && !host.starts_with('[') {
        format!("{kind}://[{host}]:{}", port.trim())
    } else {
        format!("{kind}://{host}:{}", port.trim())
    }
}

/// 规则在列表里的显示名：名称为空时退回 pattern。
fn display_name(rule: &ProxyRuleSpec) -> String {
    let name = rule.name.trim();
    if name.is_empty() {
        rule.pattern.trim().to_owned()
    } else {
        name.to_owned()
    }
}

/// 出口标签：`custom` 附带去凭据的代理端点。
fn action_label(translator: &Translator, rule: &ProxyRuleSpec) -> String {
    match rule.action.as_str() {
        "system" => translator.text("proxyRuleActionSystem").to_owned(),
        "manual" => translator.text("proxyRuleActionManual").to_owned(),
        "custom" => format!(
            "{} · {}",
            translator.text("proxyRuleActionCustom"),
            proxy_display_endpoint(&rule.proxy)
        ),
        _ => translator.text("proxyRuleActionDirect").to_owned(),
    }
}

// ───────────────────────── 服务端结果 → 文案 ─────────────────────────

/// issue → 文案（四端一致的映射）。
pub(crate) fn issue_text(translator: &Translator, issue: &ProxyRuleIssueDto) -> String {
    match issue.code.as_str() {
        "emptyPattern" => translator.text("proxyRuleErrEmpty").to_owned(),
        "invalidDomain" => translator.text_with("proxyRuleErrDomain", &[("detail", &issue.detail)]),
        "invalidRegex" => translator.text_with("proxyRuleErrRegex", &[("detail", &issue.detail)]),
        "regexTooLarge" => translator.text_with("proxyRuleErrRegex", &[("detail", "1 MiB")]),
        "tooLong" => translator.text_with("proxyRuleErrTooLong", &[("max", &issue.detail)]),
        "invalidProxy" => translator.text_with("proxyRuleErrProxy", &[("detail", &issue.detail)]),
        code => translator.text_with("proxyRuleErrGeneric", &[("detail", code)]),
    }
}

/// 单条结果的展示内容：第一行（可无）与第二行。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RouteLines {
    /// `(文案, 是否强调色)`。
    pub title: Option<(String, bool)>,
    /// 第二行分段：`(文案, 是否等宽)`。
    pub detail: Vec<(String, bool)>,
    /// 第二行是否用 warning 色。
    pub warning: bool,
}

/// `reason == ""`（请求没有给出网址）时返回 `None`。
pub(crate) fn route_lines(
    translator: &Translator,
    response: &ProxyRouteResponse,
) -> Option<RouteLines> {
    let reason = response.reason.as_str();
    if reason.is_empty() {
        return None;
    }
    let title = if matches!(reason, "loopback" | "unsupported" | "invalidUrl") {
        None
    } else if let Some(hit) = &response.matched {
        let index = (hit.index + 1).to_string();
        let name = hit.name.trim();
        Some((
            if name.is_empty() {
                translator.text_with("proxyRouteRuleHit", &[("index", &index)])
            } else {
                translator.text_with(
                    "proxyRouteRuleHitNamed",
                    &[("index", &index), ("name", name)],
                )
            },
            true,
        ))
    } else {
        Some((translator.text("proxyRouteNoRule").to_owned(), false))
    };
    let plain = |key: &str| vec![(translator.text(key).to_owned(), false)];
    let detail = match reason {
        "direct" => plain("proxyRouteDirect"),
        "proxy" => {
            // 端点用等宽字体：先放占位再切开。
            const MARK: &str = "\u{1}";
            let text = translator.text_with("proxyRouteProxy", &[("endpoint", MARK)]);
            match text.split_once(MARK) {
                Some((head, tail)) => [
                    (head.to_owned(), false),
                    (response.endpoint.clone(), true),
                    (tail.to_owned(), false),
                ]
                .into_iter()
                .filter(|(part, _)| !part.is_empty())
                .collect(),
                None => vec![(text, false)],
            }
        }
        "systemUndetected" => plain("proxyRouteSystemUndetected"),
        "manualIncomplete" => plain("proxyRouteManualIncomplete"),
        "modeNone" => plain("proxyRouteModeNone"),
        "bypass" => plain("proxyRouteBypass"),
        "pac" => plain("proxyRouteModePac"),
        "auto" => plain("proxyRouteModeAuto"),
        "loopback" => plain("proxyRouteLoopback"),
        "unsupported" => plain("proxyRouteUnsupported"),
        "invalidUrl" => plain("proxyRouteInvalidUrl"),
        other => vec![(other.to_owned(), false)],
    };
    Some(RouteLines {
        title,
        detail,
        warning: matches!(
            reason,
            "systemUndetected" | "manualIncomplete" | "invalidUrl"
        ),
    })
}

fn render_route_lines(lines: &RouteLines, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    let detail_color = if lines.warning {
        extended.colors.warning
    } else {
        tokens.colors.muted_foreground
    };
    let mut detail = h_flex().flex_wrap().items_center().min_w_0();
    for (text, mono) in &lines.detail {
        detail = detail.child(
            div()
                .min_w_0()
                .when(*mono, |span| {
                    span.font_family(tokens.typography.mono.clone())
                })
                .child(SharedString::from(text.clone())),
        );
    }
    v_flex()
        .w_full()
        .min_w_0()
        .gap(tokens.spacing.xxs)
        .when_some(lines.title.as_ref(), |column, (text, accent)| {
            column.child(
                meta_text(cx)
                    .when(*accent, |line| line.text_color(tokens.colors.primary))
                    .child(SharedString::from(text.clone())),
            )
        })
        .child(meta_text(cx).text_color(detail_color).child(detail))
}

// ───────────────────────── 分区 ─────────────────────────

pub(crate) fn group(ctx: &SectionContext, _cx: &mut App) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("proxyRulesTitle"))
        .subtitle(ctx.t("proxyRulesDesc"))
        .row(list_item(ctx))
        .row(route_test_item(ctx))
        .row(scope_item(ctx))
}

fn scope_item(ctx: &SectionContext) -> SettingsRow {
    let scope = ctx.t("proxyRulesScope");
    SettingsRow::custom(move |_, _, _, cx: &mut App| info_line(scope.clone(), cx))
}

// ───────────────────────── 规则列表 ─────────────────────────

/// 拖拽中的规则行：载荷是规则 id，预览显示抓手 + 名称。
#[derive(Clone)]
struct DraggedProxyRule {
    id: String,
    label: SharedString,
}

impl Render for DraggedProxyRule {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        h_flex()
            .px(tokens.spacing.md)
            .py(tokens.spacing.xs)
            .gap(tokens.spacing.sm)
            .items_center()
            .rounded(tokens.radius.sm)
            .border_1()
            .border_color(tokens.colors.border)
            .bg(tokens.colors.surface)
            .shadow_sm()
            .text_color(tokens.colors.surface_foreground)
            .child(Icon::new(FluxIcon::GripVertical).size(extended.icon.md))
            .child(self.label.clone())
    }
}

fn list_item(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let empty_title = ctx.t("proxyRulesEmptyTitle");
    let empty_desc = ctx.t("proxyRulesEmptyDesc");
    let add = ctx.t("proxyRulesAdd");
    SettingsRow::custom(
        move |disabled: bool, _key: &SharedString, _window: &mut Window, cx: &mut App| {
            let tokens = active_theme(cx).tokens().clone();
            let (list, connected, manual_missing) = {
                let state = store.read(cx);
                (
                    read_rules(state),
                    state.daemon_connected(),
                    manual_incomplete(
                        &state.daemon_str("proxy_host"),
                        &state.daemon_str("proxy_port"),
                    ),
                )
            };
            let disabled = disabled || !connected;
            let mut column = v_flex().w_full().gap(tokens.spacing.xxs);
            if list.is_empty() {
                column = column.child(empty_state(
                    FluxIcon::Network,
                    empty_title.clone(),
                    empty_desc.clone(),
                    cx,
                ));
            }
            for (index, rule) in list.iter().enumerate() {
                column = column.child(rule_row(
                    index,
                    rule,
                    &store,
                    &translator,
                    disabled,
                    manual_missing,
                    cx,
                ));
            }
            let add_store = store.clone();
            let add_translator = translator.clone();
            column
                .child(
                    h_flex().w_full().pt(tokens.spacing.sm).justify_end().child(
                        button("proxy-rule-add", add.clone(), ButtonVariant::Primary, cx)
                            .disabled(disabled)
                            .on_click(move |_, window, cx| {
                                proxy_rule_dialog::open(
                                    add_store.clone(),
                                    add_translator.clone(),
                                    None,
                                    window,
                                    cx,
                                );
                            }),
                    ),
                )
                .into_any_element()
        },
    )
    .keywords([
        ctx.t("proxyRulesTitle"),
        ctx.t("proxyRuleMatchDomain"),
        ctx.t("proxyRuleMatchRegex"),
        ctx.t("proxyRuleActionDirect"),
        ctx.t("proxyRuleActionSystem"),
        ctx.t("proxyRuleActionManual"),
        ctx.t("proxyRuleActionCustom"),
    ])
}

/// 行内小标签：`fg` 为文字色，`border` 为描边色（透明则只有底色）。
fn tag(text: SharedString, fg: gpui::Hsla, border: Option<gpui::Hsla>, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    div()
        .flex_none()
        .px(tokens.spacing.xs)
        .rounded(tokens.radius.sm)
        .text_color(fg)
        .text_size(extended.caption.size)
        .line_height(extended.caption.line_height)
        .when_some(border, |tag, color| tag.border_1().border_color(color))
        .when(border.is_none(), |tag| tag.bg(tokens.colors.muted))
        .child(text)
}

fn rule_row(
    index: usize,
    rule: &ProxyRuleSpec,
    store: &Entity<SettingsStore>,
    translator: &Translator,
    disabled: bool,
    manual_missing: bool,
    cx: &mut App,
) -> gpui::Stateful<Div> {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    let id = rule.id.clone();
    let name = SharedString::from(display_name(rule));
    let text_color = if rule.enabled {
        None
    } else {
        Some(extended.colors.text_tertiary)
    };
    let match_label = SharedString::from(
        translator
            .text(if rule.match_kind == "regex" {
                "proxyRuleMatchRegexShort"
            } else {
                "proxyRuleMatchDomain"
            })
            .to_owned(),
    );
    let action_text = SharedString::from(action_label(translator, rule));
    let is_direct = !matches!(rule.action.as_str(), "system" | "manual" | "custom");
    let show_missing = rule.action == "manual" && manual_missing;
    let order_tip = SharedString::from(
        translator.text_with("proxyRuleOrder", &[("index", &(index + 1).to_string())]),
    );
    let accent = tokens.colors.primary;
    let drag = DraggedProxyRule {
        id: rule.id.clone(),
        label: name.clone(),
    };
    let drop_store = store.clone();
    let toggle_store = store.clone();
    let edit_store = store.clone();
    let edit_translator = translator.clone();
    let edit_rule = rule.clone();
    let delete_store = store.clone();
    let delete_translator = translator.clone();
    let delete_rule = rule.clone();
    let id_drop = id.clone();
    let id_toggle = id.clone();

    h_flex()
        .id(SharedString::from(format!("proxy-rule-row-{id}")))
        .w_full()
        .items_center()
        .gap(tokens.spacing.sm)
        .px(tokens.spacing.sm)
        .py(tokens.spacing.xs)
        .rounded(tokens.radius.md)
        .border_1()
        .border_color(gpui::transparent_black())
        .hover(|style| style.bg(extended.colors.row_hover))
        // 整行是落点；只有左侧抓手可发起拖拽。禁用态不可拖。
        .when(!disabled, |row| {
            row.drag_over::<DraggedProxyRule>(move |style, _, _, _| style.border_color(accent))
                .on_drop(move |drag: &DraggedProxyRule, _, cx| {
                    let (from, to) = (drag.id.clone(), id_drop.clone());
                    drop_store.update(cx, |store, cx| {
                        mutate_rules(
                            store,
                            move |mut list| reorder(&mut list, &from, &to).then_some(list),
                            cx,
                        );
                    });
                })
        })
        .child(
            div()
                .id(SharedString::from(format!("proxy-rule-grip-{id}")))
                .flex_none()
                .p(tokens.spacing.xxs)
                .rounded(tokens.radius.sm)
                .text_color(extended.colors.text_tertiary)
                .when(!disabled, |grip| {
                    grip.cursor_grab()
                        .hover(|style| {
                            style
                                .bg(tokens.colors.muted)
                                .text_color(tokens.colors.foreground)
                        })
                        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                })
                .child(Icon::new(FluxIcon::GripVertical).size(extended.icon.md)),
        )
        .child(
            div()
                .id(SharedString::from(format!("proxy-rule-order-{id}")))
                .flex_none()
                .min_w(tokens.spacing.xl)
                .text_center()
                .font_family(tokens.typography.mono.clone())
                .text_size(tokens.typography.xs.size)
                .line_height(tokens.typography.xs.line_height)
                .text_color(extended.colors.text_tertiary)
                .tooltip(move |window, cx| tooltip_view(order_tip.clone(), window, cx))
                .child(SharedString::from((index + 1).to_string())),
        )
        .child(
            Switch::new(SharedString::from(format!("proxy-rule-enabled-{id}")))
                .checked(rule.enabled)
                .disabled(disabled)
                .on_click(move |checked: &bool, _, cx| {
                    let (id, checked) = (id_toggle.clone(), *checked);
                    toggle_store.update(cx, |store, cx| {
                        mutate_rules(store, move |list| Some(set_enabled(list, &id, checked)), cx);
                    });
                }),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(tokens.spacing.xxs)
                .child(
                    body_text(cx)
                        .truncate()
                        .when_some(text_color, |title, color| title.text_color(color))
                        .child(name),
                )
                .child(
                    meta_text(cx)
                        .flex()
                        .items_center()
                        .gap(tokens.spacing.xs)
                        .when_some(text_color, |line, color| line.text_color(color))
                        .child(tag(match_label, tokens.colors.muted_foreground, None, cx))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .font_family(tokens.typography.mono.clone())
                                .child(SharedString::from(rule.pattern.trim().to_owned())),
                        ),
                ),
        )
        .child(if is_direct {
            tag(action_text, tokens.colors.muted_foreground, None, cx)
        } else {
            tag(action_text, accent, Some(accent), cx)
        })
        .when(show_missing, |row| {
            row.child(tag(
                SharedString::from(translator.text("proxyNotConfigured").to_owned()),
                extended.colors.warning,
                Some(extended.colors.warning),
                cx,
            ))
        })
        .child(
            row_button(
                SharedString::from(format!("proxy-rule-edit-{id}")),
                SharedString::from(translator.text("proxyRuleEdit").to_owned()),
                ButtonVariant::Secondary,
                cx,
            )
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                proxy_rule_dialog::open(
                    edit_store.clone(),
                    edit_translator.clone(),
                    Some(edit_rule.clone()),
                    window,
                    cx,
                );
            }),
        )
        .child(
            row_danger_button(
                SharedString::from(format!("proxy-rule-delete-{id}")),
                SharedString::from(translator.text("delete").to_owned()),
                cx,
            )
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                confirm_delete(
                    delete_store.clone(),
                    &delete_translator,
                    &delete_rule,
                    false,
                    window,
                    cx,
                );
            }),
        )
}

/// 删除确认：确认后移除规则。`close_all` 为真时（从编辑对话框触发）连同编辑框一起关闭。
pub(crate) fn confirm_delete(
    store: Entity<SettingsStore>,
    translator: &Translator,
    rule: &ProxyRuleSpec,
    close_all: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let id = rule.id.clone();
    let title = SharedString::from(
        translator.text_with("proxyRuleDeleteConfirm", &[("name", &display_name(rule))]),
    );
    let description = SharedString::from(translator.text("proxyRuleDeleteDesc").to_owned());
    let cancel = SharedString::from(translator.text("cancel").to_owned());
    let confirm = SharedString::from(translator.text("delete").to_owned());
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let store = store.clone();
        let id = id.clone();
        alert
            .title(dialog_title(title.clone(), cx))
            .description(description.clone())
            .footer(fluxdown_ui_components::dialog_footer(
                Some(cancel.clone()),
                confirm.clone(),
                DialogIntent::Destructive,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let id = id.clone();
                store.update(cx, |store, cx| {
                    mutate_rules(store, move |list| Some(remove(list, &id)), cx);
                });
                if close_all {
                    // 确认框与编辑框一起关掉；返回 false 避免基座再弹出一层。
                    window.close_all_dialogs(cx);
                    return false;
                }
                true
            })
    });
}

// ───────────────────────── 测试网址 ─────────────────────────

struct RouteSlot {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// 结果与「当时的模式 + 规则原文」绑定；两者任一变化后旧结果自动隐藏。
fn route_stamp(mode: &str, rules_raw: &str) -> String {
    format!("{mode}|{rules_raw}")
}

fn run_route_check(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    url: &str,
    cx: &mut App,
) {
    let url = url.trim().to_owned();
    if url.is_empty() {
        return;
    }
    let translator = translator.clone();
    store.update(cx, |store, cx| {
        // 发送界面当前渲染所用的规则原文：乐观写入未落到 daemon 时结果也与界面一致。
        let rules_raw = store.daemon_str(PROXY_RULES_KEY);
        let stamp = route_stamp(&store.daemon_str("proxy_mode"), &rules_raw);
        store.call_with(
            "proxyRoute",
            method::DAEMON_CONFIG_PROXY_ROUTE,
            json!({ "url": url, "rules": rules_raw }),
            cx,
            move |store, result, cx| {
                let value = match result {
                    Ok(response) => json!({ "stamp": stamp, "response": response }),
                    Err(error) => {
                        json!({ "stamp": stamp, "error": rpc_error_text(&translator, &error) })
                    }
                };
                store.set_transient(ROUTE_RESULT_KEY, value, cx);
            },
        );
    });
}

fn route_test_item(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let title = ctx.t("proxyRouteTest");
    let description = ctx.t("proxyRouteTestDesc");
    let placeholder = ctx.t("proxyRouteUrlPlaceholder");
    let check = ctx.t("proxyRouteCheck");
    let checking = ctx.t("proxyRouteChecking");
    let render_title = title.clone();
    let render_description = description.clone();
    let mut row = SettingsRow::custom(
        move |disabled: bool, key: &SharedString, window: &mut Window, cx: &mut App| {
            let tokens = active_theme(cx).tokens().clone();
            let slot = window.use_keyed_state(
                ElementId::from(SharedString::from(format!("{key}-url"))),
                cx,
                {
                    let store = store.clone();
                    let translator = translator.clone();
                    let placeholder = placeholder.clone();
                    move |window, cx| {
                        let input =
                            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
                        let _subscription = cx.subscribe(
                            &input,
                            move |_: &mut RouteSlot, input, event: &InputEvent, cx| match event {
                                InputEvent::PressEnter { .. } => {
                                    let url = input.read(cx).value().to_string();
                                    run_route_check(&store, &translator, &url, cx);
                                }
                                // 输入内容影响按钮可用性：通知整页重绘。
                                InputEvent::Change => store.update(cx, |_, cx| cx.notify()),
                                _ => {}
                            },
                        );
                        RouteSlot {
                            input,
                            _subscription,
                        }
                    }
                },
            );
            let input = slot.read(cx).input.clone();
            let url = input.read(cx).value().to_string();
            let (busy, connected, mode, rules_raw, stored) = {
                let state = store.read(cx);
                (
                    state.is_busy("proxyRoute"),
                    state.daemon_connected(),
                    state.daemon_str("proxy_mode"),
                    state.daemon_str(PROXY_RULES_KEY),
                    state.transient(ROUTE_RESULT_KEY).cloned(),
                )
            };
            let disabled = disabled || !connected;
            let stamp = route_stamp(&mode, &rules_raw);
            let result = stored
                .as_ref()
                .and_then(|value| render_result(value, &stamp, &translator, cx));
            let click_store = store.clone();
            let click_translator = translator.clone();
            let click_input = input.clone();
            let controls = h_flex()
                .w_full()
                .gap(tokens.spacing.sm)
                .items_center()
                .child(Input::new(&input).w_full().disabled(disabled))
                .child(
                    loading_button(
                        "proxy-route-check",
                        if busy {
                            checking.clone()
                        } else {
                            check.clone()
                        },
                        ButtonVariant::Secondary,
                        busy,
                        cx,
                    )
                    .disabled(busy || disabled || url.trim().is_empty())
                    .on_click(move |_, _, cx| {
                        let url = click_input.read(cx).value().to_string();
                        run_route_check(&click_store, &click_translator, &url, cx);
                    }),
                );
            v_flex()
                .w_full()
                .min_w_0()
                .gap(tokens.spacing.sm)
                .child(
                    v_flex()
                        .gap(tokens.spacing.xxs)
                        .child(body_text(cx).child(render_title.clone()))
                        .child(meta_text(cx).child(render_description.clone())),
                )
                .child(controls)
                .children(result)
                .into_any_element()
        },
    );
    // 自渲染不丢失设置搜索所需的标题与说明。
    row.title = title;
    row.description(description)
}

/// 仅在 stamp 与当前一致时渲染结果。
fn render_result(
    stored: &Value,
    current_stamp: &str,
    translator: &Translator,
    cx: &App,
) -> Option<Div> {
    if stored.get("stamp").and_then(Value::as_str) != Some(current_stamp) {
        return None;
    }
    if let Some(error) = stored.get("error").and_then(Value::as_str) {
        let color = active_theme(cx).extended().colors.warning;
        return Some(
            div().child(
                meta_text(cx)
                    .text_color(color)
                    .child(SharedString::from(error.to_owned())),
            ),
        );
    }
    let response: ProxyRouteResponse =
        serde_json::from_value(stored.get("response")?.clone()).ok()?;
    let lines = route_lines(translator, &response)?;
    Some(render_route_lines(&lines, cx))
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::proxy_rules::new_proxy_rule;
    use fluxdown_protocol::{ProxyRouteMatchDto, ProxyRuleIssueDto};
    use fluxdown_ui_i18n::{I18nCatalog, I18nError};

    use super::*;

    fn rules(ids: &[&str]) -> Vec<ProxyRuleSpec> {
        ids.iter()
            .map(|id| new_proxy_rule((*id).to_owned()))
            .collect()
    }

    fn ids(list: &[ProxyRuleSpec]) -> Vec<&str> {
        list.iter().map(|rule| rule.id.as_str()).collect()
    }

    #[test]
    fn proxy_rules_reorder_moves_to_target_slot_both_directions() {
        let mut list = rules(&["a", "b", "c"]);
        // 向下拖：a 落到 c 之后。
        assert!(reorder(&mut list, "a", "c"));
        assert_eq!(ids(&list), ["b", "c", "a"]);
        // 向上拖回原位。
        assert!(reorder(&mut list, "a", "b"));
        assert_eq!(ids(&list), ["a", "b", "c"]);
        // 同一项 / id 不存在：无变化。
        assert!(!reorder(&mut list, "a", "a"));
        assert!(!reorder(&mut list, "missing", "a"));
        assert!(!reorder(&mut list, "a", "missing"));
        assert_eq!(ids(&list), ["a", "b", "c"]);
    }

    #[test]
    fn proxy_rules_transforms_are_idempotent() {
        let list = rules(&["a", "b"]);
        let mut edited = new_proxy_rule("b".to_owned());
        edited.name = "B".to_owned();
        let once = upsert(list.clone(), edited.clone());
        assert_eq!(upsert(once.clone(), edited.clone()), once);
        assert_eq!(once[1].name, "B");
        let appended = upsert(list, new_proxy_rule("c".to_owned()));
        assert_eq!(ids(&appended), ["a", "b", "c"]);
        let off = set_enabled(appended, "a", false);
        assert!(!off[0].enabled);
        assert_eq!(set_enabled(off.clone(), "a", false), off);
        let removed = remove(off, "b");
        assert_eq!(ids(&removed), ["a", "c"]);
        assert_eq!(remove(removed.clone(), "b"), removed);
    }

    #[test]
    fn proxy_rules_manual_incomplete_needs_host_and_valid_port() {
        assert!(manual_incomplete("", "8080"));
        assert!(manual_incomplete("  ", "8080"));
        assert!(manual_incomplete("h", ""));
        assert!(manual_incomplete("h", "0"));
        assert!(manual_incomplete("h", "65536"));
        assert!(!manual_incomplete("h", "65535"));
    }

    #[test]
    fn proxy_rules_issue_and_route_text_follow_shared_mapping() -> Result<(), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;
        let t = catalog.translator("en");
        let issue = |code: &str, detail: &str| ProxyRuleIssueDto {
            index: 0,
            field: "pattern".to_owned(),
            code: code.to_owned(),
            detail: detail.to_owned(),
        };
        assert_eq!(
            issue_text(&t, &issue("emptyPattern", "")),
            "Enter a domain or expression"
        );
        assert_eq!(
            issue_text(&t, &issue("tooLong", "64")),
            "Too long (at most 64 characters)"
        );
        assert_eq!(
            issue_text(&t, &issue("regexTooLarge", "")),
            "Invalid regular expression: 1 MiB"
        );
        assert_eq!(issue_text(&t, &issue("weird", "")), "Invalid rule: weird");

        let response = |reason: &str, matched: Option<(u32, &str)>| ProxyRouteResponse {
            issues: Vec::new(),
            matched: matched.map(|(index, name)| ProxyRouteMatchDto {
                index,
                id: "x".to_owned(),
                name: name.to_owned(),
            }),
            reason: reason.to_owned(),
            endpoint: "socks5://127.0.0.1:1".to_owned(),
        };
        assert_eq!(route_lines(&t, &response("", None)), None);
        let hit = route_lines(&t, &response("proxy", Some((0, "GitHub")))).expect("route lines");
        assert_eq!(
            hit.title,
            Some(("Matched rule 1 \"GitHub\"".to_owned(), true))
        );
        assert_eq!(
            hit.detail,
            vec![
                ("Through proxy ".to_owned(), false),
                ("socks5://127.0.0.1:1".to_owned(), true)
            ]
        );
        assert!(!hit.warning);
        let miss = route_lines(&t, &response("systemUndetected", None)).expect("route lines");
        assert_eq!(miss.title, Some(("No rule matched".to_owned(), false)));
        assert!(miss.warning);
        let loopback = route_lines(&t, &response("loopback", None)).expect("route lines");
        assert_eq!(loopback.title, None);
        Ok(())
    }
}
