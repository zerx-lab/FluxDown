//! eD2K 网络探测：「常规」页的 HighID / LowID 检测（选目标）与「Kad 节点」页的引导面板。
//!
//! 单选项存在设置 store 的 transient 里，输入框是窗口按行键保存的 `InputState` 实体（与「测试
//! 网址」同一模式）；检测 / 引导结果同样只存 transient（内存），不落配置。参数拼装、校验与
//! 结果 → 文案都是纯函数。

use fluxdown_protocol::{
    Ed2kIdCheckParams, Ed2kIdCheckTarget, Ed2kIdKind, Ed2kKadBootstrapDto, Ed2kKadBootstrapParams,
    Ed2kKadBootstrapSource, Ed2kKadNodeDto, Ed2kKadNodeOrigin, Ed2kKadNodeState, Ed2kNetworkDto,
    Ed2kUpnpStatus, method,
};
use fluxdown_ui_components::{ButtonVariant, loading_button, tabular_numbers};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ElementId, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, Styled, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    h_flex,
    input::{Input, InputState},
    radio::Radio,
    scroll::ScrollableElement as _,
    v_flex,
};
use serde_json::{Value, json};

use super::SectionContext;
use crate::store::{SettingsStore, rpc_error_text};
use crate::ui::{INPUT_WIDTH, NUMBER_WIDTH, SettingsRow, body_text, meta_text};

/// 未知 / 无。
const DASH: &str = "—";

/// ID 检测：忙碌动作键与结果 transient 键。
const ID_CHECK_ACTION: &str = "ed2kIdCheck";
/// ID 检测目标的单选 transient 键（选项下标）。
const ID_TARGET_KEY: &str = "ed2kIdCheckTarget";
/// ID 检测目标，按界面顺序（仿 eMule：指定服务器 / 服务器列表 / 当前会话）。
const ID_TARGETS: [Ed2kIdCheckTarget; 3] = [
    Ed2kIdCheckTarget::Server,
    Ed2kIdCheckTarget::List,
    Ed2kIdCheckTarget::Session,
];
const ID_TARGET_DEFAULT: usize = 1;

/// Kad 引导：忙碌动作键与结果 transient 键。
const BOOTSTRAP_ACTION: &str = "ed2kKadBootstrap";
/// 引导来源的单选 transient 键（选项下标）。
const BOOTSTRAP_SOURCE_KEY: &str = "ed2kKadBootstrapSource";
/// 引导来源，按界面顺序（仿 eMule：IP / 网址 / 已知节点）。
const BOOTSTRAP_SOURCES: [Ed2kKadBootstrapSource; 3] = [
    Ed2kKadBootstrapSource::Node,
    Ed2kKadBootstrapSource::Url,
    Ed2kKadBootstrapSource::Known,
];
const BOOTSTRAP_SOURCE_DEFAULT: usize = 2;
/// 引导结果节点表最多渲染的行数（服务端至多给 100 个）。
const NODE_RENDER_CAP: usize = 100;
/// 节点表限高（界面缩放前的 px）。
const NODE_TABLE_MAX_HEIGHT: f32 = 260.;
// 节点表列宽（界面缩放前的 px）。
const COL_ADDR: f32 = 170.;
const COL_STATE: f32 = 84.;
const COL_ORIGIN: f32 = 76.;
const COL_VERSION: f32 = 52.;

// ───────────────────────── 纯函数 ─────────────────────────

/// transient 里的单选下标；缺失 / 越界取默认。
fn selected_index(stored: Option<&Value>, len: usize, default: usize) -> usize {
    stored
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .filter(|index| *index < len)
        .unwrap_or(default)
}

/// 端口输入：1–65535 的十进制整数（两侧空白忽略）。
pub(crate) fn parse_port(raw: &str) -> Option<u16> {
    raw.trim().parse::<u16>().ok().filter(|port| *port > 0)
}

/// 主机输入：只做 trim，不在客户端解析域名；空串 = 未填。
fn parse_host(raw: &str) -> Option<&str> {
    let host = raw.trim();
    (!host.is_empty()).then_some(host)
}

/// 「指定服务器」的 `host:port`；主机含 `:`（IPv6 字面量）无法无歧义拼接，视为非法。
pub(crate) fn server_addr(host: &str, port: &str) -> Option<String> {
    let host = parse_host(host).filter(|host| !host.contains(':'))?;
    Some(format!("{host}:{}", parse_port(port)?))
}

/// ID 检测参数；「指定服务器」输入非法时为 `None`（不发请求）。
pub(crate) fn id_check_params(
    target: Ed2kIdCheckTarget,
    host: &str,
    port: &str,
) -> Option<Ed2kIdCheckParams> {
    let server = if target == Ed2kIdCheckTarget::Server {
        server_addr(host, port)?
    } else {
        String::new()
    };
    Some(Ed2kIdCheckParams { target, server })
}

/// 引导参数；「IP 或域名」输入非法时为 `None`（不发请求）。
pub(crate) fn bootstrap_params(
    source: Ed2kKadBootstrapSource,
    host: &str,
    port: &str,
    url: &str,
) -> Option<Ed2kKadBootstrapParams> {
    Some(match source {
        Ed2kKadBootstrapSource::Node => Ed2kKadBootstrapParams {
            source,
            host: parse_host(host)?.to_owned(),
            port: parse_port(port)?,
            ..Ed2kKadBootstrapParams::default()
        },
        Ed2kKadBootstrapSource::Url => Ed2kKadBootstrapParams {
            source,
            url: url.trim().to_owned(),
            ..Ed2kKadBootstrapParams::default()
        },
        Ed2kKadBootstrapSource::Known => Ed2kKadBootstrapParams {
            source,
            ..Ed2kKadBootstrapParams::default()
        },
    })
}

fn upnp_key(status: Ed2kUpnpStatus) -> &'static str {
    match status {
        Ed2kUpnpStatus::Disabled => "ed2kUpnpDisabled",
        Ed2kUpnpStatus::Pending => "ed2kUpnpPending",
        Ed2kUpnpStatus::Mapped => "ed2kUpnpMapped",
        Ed2kUpnpStatus::Failed => "ed2kUpnpFailed",
    }
}

/// ID 检测结果的展示意图（文案在渲染时按语言拼装）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IdCheckView {
    /// 登录成功：HighID / LowID。
    Logged {
        id_key: &'static str,
        server: String,
        port: u16,
        upnp_key: &'static str,
        low: bool,
    },
    /// 「当前已登录的服务器」而此刻没有会话。
    NoSession,
    /// 未能登录（`error` 为宿主原因或 RPC 错误文本，可空）。
    Failed { error: String },
    /// 「指定服务器」主机 / 端口非法，未发请求。
    NeedServer,
}

/// transient 结果 → 展示意图。形状：`{"needServer":true}` | `{"target":t,"rpcError":e}` |
/// `{"target":t,"result":Ed2kNetworkDto}`。
pub(crate) fn id_check_view(stored: &Value) -> IdCheckView {
    if stored.get("needServer").and_then(Value::as_bool) == Some(true) {
        return IdCheckView::NeedServer;
    }
    if let Some(error) = stored.get("rpcError").and_then(Value::as_str) {
        return IdCheckView::Failed {
            error: error.trim().to_owned(),
        };
    }
    let Some(network) = stored
        .get("result")
        .and_then(|result| serde_json::from_value::<Ed2kNetworkDto>(result.clone()).ok())
    else {
        return IdCheckView::Failed {
            error: String::new(),
        };
    };
    let id_key = match network.id {
        Ed2kIdKind::High => "ed2kIdHigh",
        Ed2kIdKind::Low => "ed2kIdLow",
        Ed2kIdKind::None => {
            let error = network.error.trim().to_owned();
            let session = stored.get("target").and_then(Value::as_str) == Some("session");
            return if session && error.is_empty() {
                IdCheckView::NoSession
            } else {
                IdCheckView::Failed { error }
            };
        }
    };
    IdCheckView::Logged {
        id_key,
        server: network.server,
        port: network.listen_port,
        upnp_key: upnp_key(network.upnp),
        low: network.id == Ed2kIdKind::Low,
    }
}

/// 结果行文本：`(主行, LowID 提示, 是否失败)`。
pub(crate) fn id_check_lines(
    translator: &Translator,
    view: &IdCheckView,
) -> (String, Option<String>, bool) {
    match view {
        IdCheckView::Logged {
            id_key,
            server,
            port,
            upnp_key,
            low,
        } => {
            let server = if server.trim().is_empty() {
                DASH
            } else {
                server.as_str()
            };
            let port = if *port == 0 {
                DASH.to_owned()
            } else {
                port.to_string()
            };
            let line = translator.text_with(
                "ed2kIdCheckResult",
                &[
                    ("id", translator.text(id_key)),
                    ("server", server),
                    ("port", &port),
                    ("upnp", translator.text(upnp_key)),
                ],
            );
            let hint = low.then(|| translator.text("ed2kIdLowHint").to_owned());
            (line, hint, false)
        }
        IdCheckView::NoSession => (
            translator.text("ed2kIdCheckNoSession").to_owned(),
            None,
            false,
        ),
        IdCheckView::Failed { error } => {
            let failed = translator.text("ed2kIdCheckFailed");
            let line = if error.is_empty() {
                failed.to_owned()
            } else {
                format!("{failed}: {error}")
            };
            (line, None, true)
        }
        IdCheckView::NeedServer => (
            translator.text("ed2kIdCheckNeedServer").to_owned(),
            None,
            true,
        ),
    }
}

/// 引导结果的展示意图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BootstrapView {
    /// 「IP 或域名」主机 / 端口非法，未发请求。
    NeedNode,
    /// 没有可联系的节点（宿主 `error`）或 RPC 失败。
    Failed { error: String },
    Done {
        contacted: u32,
        responded: u32,
        discovered: u32,
        total: u32,
        nodes: Vec<Ed2kKadNodeDto>,
    },
}

/// transient 结果 → 展示意图。形状：`{"needNode":true}` | `{"rpcError":e}` |
/// `{"result":Ed2kKadBootstrapDto}`。
pub(crate) fn bootstrap_view(stored: &Value) -> BootstrapView {
    if stored.get("needNode").and_then(Value::as_bool) == Some(true) {
        return BootstrapView::NeedNode;
    }
    if let Some(error) = stored.get("rpcError").and_then(Value::as_str) {
        return BootstrapView::Failed {
            error: error.trim().to_owned(),
        };
    }
    let Some(result) = stored
        .get("result")
        .and_then(|result| serde_json::from_value::<Ed2kKadBootstrapDto>(result.clone()).ok())
    else {
        return BootstrapView::Failed {
            error: String::new(),
        };
    };
    let error = result.error.trim();
    if !error.is_empty() {
        return BootstrapView::Failed {
            error: error.to_owned(),
        };
    }
    let mut nodes = result.nodes;
    nodes.truncate(NODE_RENDER_CAP);
    BootstrapView::Done {
        contacted: result.contacted,
        responded: result.responded,
        discovered: result.discovered,
        total: result.node_count,
        nodes,
    }
}

/// 引导结果行：`(主行, 无应答警示, 是否失败)`。
pub(crate) fn bootstrap_lines(
    translator: &Translator,
    view: &BootstrapView,
) -> (String, Option<String>, bool) {
    match view {
        BootstrapView::NeedNode => (
            translator.text("ed2kKadBootstrapNeedNode").to_owned(),
            None,
            true,
        ),
        BootstrapView::Failed { error } => {
            let error = if error.is_empty() {
                DASH
            } else {
                error.as_str()
            };
            (
                translator.text_with("ed2kKadBootstrapFailed", &[("error", error)]),
                None,
                true,
            )
        }
        BootstrapView::Done {
            contacted,
            responded,
            discovered,
            total,
            ..
        } => (
            translator.text_with(
                "ed2kKadBootstrapResult",
                &[
                    ("contacted", &contacted.to_string()),
                    ("responded", &responded.to_string()),
                    ("discovered", &discovered.to_string()),
                    ("total", &total.to_string()),
                ],
            ),
            (*responded == 0).then(|| translator.text("ed2kKadBootstrapNoReply").to_owned()),
            false,
        ),
    }
}

/// Kad ID：空或全 0（未知）显示 `—`。
pub(crate) fn kad_id_text(id: &str) -> &str {
    let id = id.trim();
    if id.chars().all(|c| c == '0') {
        DASH
    } else {
        id
    }
}

fn kad_version_text(version: u8) -> String {
    if version == 0 {
        DASH.to_owned()
    } else {
        version.to_string()
    }
}

fn kad_state_key(state: Ed2kKadNodeState) -> &'static str {
    match state {
        Ed2kKadNodeState::Known => "ed2kKadNodeStateKnown",
        Ed2kKadNodeState::Queried => "ed2kKadNodeStateQueried",
        Ed2kKadNodeState::Responded => "ed2kKadNodeStateResponded",
    }
}

fn kad_origin_key(origin: Ed2kKadNodeOrigin) -> &'static str {
    match origin {
        Ed2kKadNodeOrigin::Bootstrap => "ed2kKadNodeOriginBootstrap",
        Ed2kKadNodeOrigin::Discovered => "ed2kKadNodeOriginDiscovered",
    }
}

// ───────────────────────── 渲染 ─────────────────────────

/// 一行探测控件的输入框（窗口按行键保存，跨帧保留草稿）。
struct InputsSlot {
    inputs: Vec<Entity<InputState>>,
}

fn inputs_slot(
    key: &SharedString,
    placeholders: &[SharedString],
    window: &mut Window,
    cx: &mut App,
) -> Vec<Entity<InputState>> {
    let placeholders = placeholders.to_vec();
    let slot = window.use_keyed_state(
        ElementId::from(SharedString::from(format!("{key}-inputs"))),
        cx,
        move |window, cx| InputsSlot {
            inputs: placeholders
                .into_iter()
                .map(|placeholder| {
                    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
                })
                .collect(),
        },
    );
    slot.read(cx).inputs.clone()
}

fn input_value(input: Option<&Entity<InputState>>, cx: &App) -> String {
    input.map_or_else(String::new, |input| input.read(cx).value().to_string())
}

/// 纵向单选：每项一行，`extra` 为选中项同行右侧的输入框。
fn radio_option(
    id: SharedString,
    label: SharedString,
    checked: bool,
    disabled: bool,
    on_pick: impl Fn(&mut App) + 'static,
    extra: Vec<gpui::AnyElement>,
    cx: &App,
) -> gpui::Div {
    let tokens = active_theme(cx).tokens();
    h_flex()
        .w_full()
        .min_w_0()
        .flex_wrap()
        .items_center()
        .gap(tokens.spacing.sm)
        .child(
            Radio::new(ElementId::from(id))
                .label(label)
                .checked(checked)
                .disabled(disabled)
                .on_click(move |_, _, cx| on_pick(cx)),
        )
        .children(extra)
}

fn pick(
    store: &Entity<SettingsStore>,
    key: &'static str,
    index: usize,
) -> impl Fn(&mut App) + 'static {
    let store = store.clone();
    move |cx| {
        store.update(cx, |store, cx| store.set_transient(key, json!(index), cx));
    }
}

fn result_lines(
    line: String,
    hint: Option<String>,
    failed: bool,
    hint_color: Hsla,
    cx: &App,
) -> gpui::Div {
    let tokens = active_theme(cx).tokens();
    let color = if failed {
        tokens.colors.destructive
    } else {
        tokens.colors.muted_foreground
    };
    v_flex()
        .w_full()
        .min_w_0()
        .gap(tokens.spacing.xxs)
        .child(
            meta_text(cx)
                .text_color(color)
                .child(SharedString::from(line)),
        )
        .children(hint.map(|hint| {
            meta_text(cx)
                .text_color(hint_color)
                .child(SharedString::from(hint))
        }))
}

fn row_header(title: &SharedString, description: &SharedString, cx: &App) -> gpui::Div {
    v_flex()
        .gap(active_theme(cx).tokens().spacing.xxs)
        .child(body_text(cx).child(title.clone()))
        .child(meta_text(cx).child(description.clone()))
}

fn run_id_check(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    params: Option<Ed2kIdCheckParams>,
    cx: &mut App,
) {
    let translator = translator.clone();
    store.update(cx, |store, cx| {
        let Some(params) = params else {
            store.set_transient(ID_CHECK_ACTION, json!({ "needServer": true }), cx);
            return;
        };
        let target = json!(params.target);
        let Ok(value) = serde_json::to_value(&params) else {
            store.set_transient(
                ID_CHECK_ACTION,
                json!({ "target": target, "rpcError": "" }),
                cx,
            );
            return;
        };
        store.call_with(
            ID_CHECK_ACTION,
            method::DAEMON_ED2K_ID_CHECK,
            value,
            cx,
            move |store, result, cx| {
                let stored = match result {
                    Ok(result) => json!({ "target": target, "result": result }),
                    Err(error) => json!({
                        "target": target,
                        "rpcError": rpc_error_text(&translator, &error),
                    }),
                };
                store.set_transient(ID_CHECK_ACTION, stored, cx);
            },
        );
    });
}

/// 「常规」页：HighID / LowID 检测行（选目标 + 按钮 + 结果）。
pub(crate) fn id_check_item(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let title = ctx.t("ed2kIdCheck");
    let description = ctx.t("ed2kIdCheckDesc");
    let labels = [
        ctx.t("ed2kIdCheckTargetServer"),
        ctx.t("ed2kIdCheckTargetList"),
        ctx.t("ed2kIdCheckTargetSession"),
    ];
    let placeholders = [ctx.t("ed2kHostPlaceholder"), ctx.t("ed2kPortPlaceholder")];
    let run = ctx.t("ed2kIdCheckRun");
    let running = ctx.t("ed2kIdCheckRunning");
    let render_title = title.clone();
    let render_description = description.clone();
    let mut row =
        SettingsRow::custom(
            move |disabled: bool, key: &SharedString, window: &mut Window, cx: &mut App| {
                let inputs = inputs_slot(key, &placeholders, window, cx);
                let (busy, connected, selected, stored) = {
                    let state = store.read(cx);
                    (
                        state.is_busy(ID_CHECK_ACTION),
                        state.daemon_connected(),
                        selected_index(
                            state.transient(ID_TARGET_KEY),
                            ID_TARGETS.len(),
                            ID_TARGET_DEFAULT,
                        ),
                        state.transient(ID_CHECK_ACTION).cloned(),
                    )
                };
                let disabled = disabled || !connected;
                let theme = active_theme(cx);
                let tokens = theme.tokens().clone();
                let warning = theme.extended().colors.warning;
                let server_inputs: Vec<gpui::AnyElement> = inputs
                    .iter()
                    .zip([INPUT_WIDTH * 0.75, NUMBER_WIDTH * 0.7])
                    .map(|(input, width)| {
                        Input::new(input)
                            .w(active_theme(cx).text_extent(width))
                            .disabled(disabled || busy || selected != 0)
                            .into_any_element()
                    })
                    .collect();
                let mut extras = [server_inputs, Vec::new(), Vec::new()].into_iter();
                let options = v_flex().w_full().gap(tokens.spacing.xs).children(
                    labels.iter().enumerate().map(|(index, label)| {
                        radio_option(
                            SharedString::from(format!("{key}-target-{index}")),
                            label.clone(),
                            selected == index,
                            disabled || busy,
                            pick(&store, ID_TARGET_KEY, index),
                            extras.next().unwrap_or_default(),
                            cx,
                        )
                    }),
                );
                let click_store = store.clone();
                let click_translator = translator.clone();
                let click_inputs = inputs.clone();
                let button = loading_button(
                    "ed2k-id-check-run",
                    if busy { running.clone() } else { run.clone() },
                    ButtonVariant::Secondary,
                    busy,
                    cx,
                )
                .disabled(busy || disabled)
                .on_click(move |_, _, cx| {
                    let target = ID_TARGETS.get(selected).copied().unwrap_or_default();
                    let host = input_value(click_inputs.first(), cx);
                    let port = input_value(click_inputs.get(1), cx);
                    let params = id_check_params(target, &host, &port);
                    run_id_check(&click_store, &click_translator, params, cx);
                });
                let result = stored.map(|stored| {
                    let (line, hint, failed) = id_check_lines(&translator, &id_check_view(&stored));
                    result_lines(line, hint, failed, warning, cx)
                });
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(tokens.spacing.sm)
                    .child(row_header(&render_title, &render_description, cx))
                    .child(options)
                    .child(h_flex().child(button))
                    .children(result)
                    .into_any_element()
            },
        );
    row.title = title;
    row.description(description)
}

fn run_bootstrap(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    params: Option<Ed2kKadBootstrapParams>,
    cx: &mut App,
) {
    let translator = translator.clone();
    store.update(cx, |store, cx| {
        let Some(params) = params else {
            store.set_transient(BOOTSTRAP_ACTION, json!({ "needNode": true }), cx);
            return;
        };
        let Ok(value) = serde_json::to_value(&params) else {
            store.set_transient(BOOTSTRAP_ACTION, json!({ "rpcError": "" }), cx);
            return;
        };
        store.call_with(
            BOOTSTRAP_ACTION,
            method::DAEMON_ED2K_KAD_BOOTSTRAP,
            value,
            cx,
            move |store, result, cx| {
                let stored = match result {
                    Ok(result) => json!({ "result": result }),
                    Err(error) => json!({ "rpcError": rpc_error_text(&translator, &error) }),
                };
                store.set_transient(BOOTSTRAP_ACTION, stored, cx);
            },
        );
    });
}

/// 节点表（限高可滚动）。
fn node_table(nodes: &[Ed2kKadNodeDto], translator: &Translator, cx: &App) -> gpui::AnyElement {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let extended = theme.extended().clone();
    let extent = |base: f32| theme.text_extent(base);
    let cell = |width: Option<f32>| {
        let base = div().min_w_0().truncate();
        match width {
            Some(width) => base.flex_none().w(extent(width)),
            None => base.flex_1(),
        }
    };
    let header = h_flex()
        .w_full()
        .gap(tokens.spacing.sm)
        .px(tokens.spacing.sm)
        .py(tokens.spacing.xs)
        .border_b_1()
        .border_color(extended.colors.hairline)
        .text_size(tokens.typography.xs.size)
        .line_height(tokens.typography.xs.line_height)
        .text_color(tokens.colors.muted_foreground)
        .child(cell(Some(COL_ADDR)).child(translator.text("ed2kKadNodeAddr").to_owned()))
        .child(cell(None).child(translator.text("ed2kKadNodeId").to_owned()))
        .child(cell(Some(COL_STATE)).child(translator.text("ed2kKadNodeState").to_owned()))
        .child(cell(Some(COL_ORIGIN)).child(translator.text("ed2kKadNodeOrigin").to_owned()))
        .child(
            cell(Some(COL_VERSION))
                .text_right()
                .child(translator.text("ed2kKadNodeVersion").to_owned()),
        );
    let rows = nodes.iter().enumerate().map(|(index, node)| {
        let addr = SharedString::from(node.addr.trim().to_owned());
        let id = SharedString::from(kad_id_text(&node.id).to_owned());
        let state_color = match node.state {
            Ed2kKadNodeState::Known => extended.colors.text_tertiary,
            Ed2kKadNodeState::Queried => extended.colors.warning,
            Ed2kKadNodeState::Responded => extended.colors.success,
        };
        h_flex()
            .id(("ed2k-bootstrap-node", index))
            .w_full()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.sm)
            .py(tokens.spacing.xs)
            .when(index + 1 < nodes.len(), |this| {
                this.border_b_1().border_color(extended.colors.hairline)
            })
            .text_size(tokens.typography.sm.size)
            .line_height(tokens.typography.sm.line_height)
            .text_color(tokens.colors.foreground)
            .font_features(tabular_numbers())
            .hover({
                let hover = extended.colors.row_hover;
                move |style| style.bg(hover)
            })
            .child(
                cell(Some(COL_ADDR))
                    .font_family(tokens.typography.mono.clone())
                    .child(addr),
            )
            .child(
                cell(None)
                    .font_family(tokens.typography.mono.clone())
                    .text_color(tokens.colors.muted_foreground)
                    .child(id),
            )
            .child(
                cell(Some(COL_STATE))
                    .text_color(state_color)
                    .child(translator.text(kad_state_key(node.state)).to_owned()),
            )
            .child(
                cell(Some(COL_ORIGIN))
                    .text_color(tokens.colors.muted_foreground)
                    .child(translator.text(kad_origin_key(node.origin)).to_owned()),
            )
            .child(
                cell(Some(COL_VERSION))
                    .text_right()
                    .text_color(tokens.colors.muted_foreground)
                    .child(kad_version_text(node.version)),
            )
    });
    v_flex()
        .w_full()
        .border_1()
        .border_color(extended.colors.hairline)
        .rounded(tokens.radius.md)
        .child(header)
        .child(
            v_flex()
                .w_full()
                .max_h(extent(NODE_TABLE_MAX_HEIGHT))
                .children(rows)
                .overflow_y_scrollbar(),
        )
        .into_any_element()
}

/// 「Kad 节点」页：引导面板（来源单选 + 输入 + 按钮 + 结果 + 节点表）。
pub(crate) fn bootstrap_item(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let title = ctx.t("ed2kKadBootstrap");
    let description = ctx.t("ed2kKadBootstrapDesc");
    let placeholders = [
        ctx.t("ed2kHostPlaceholder"),
        ctx.t("ed2kPortPlaceholder"),
        ctx.t("ed2kKadBootstrapUrlPlaceholder"),
    ];
    let run = ctx.t("ed2kKadBootstrapRun");
    let running = ctx.t("ed2kKadBootstrapRunning");
    let render_title = title.clone();
    let render_description = description.clone();
    let mut row = SettingsRow::custom(
        move |disabled: bool, key: &SharedString, window: &mut Window, cx: &mut App| {
            let inputs = inputs_slot(key, &placeholders, window, cx);
            let (busy, connected, selected, known, stored) = {
                let state = store.read(cx);
                (
                    state.is_busy(BOOTSTRAP_ACTION),
                    state.daemon_connected(),
                    selected_index(
                        state.transient(BOOTSTRAP_SOURCE_KEY),
                        BOOTSTRAP_SOURCES.len(),
                        BOOTSTRAP_SOURCE_DEFAULT,
                    ),
                    state.daemon_i64("ed2k_nodes_dat_count").max(0),
                    state.transient(BOOTSTRAP_ACTION).cloned(),
                )
            };
            let disabled = disabled || !connected;
            let theme = active_theme(cx);
            let tokens = theme.tokens().clone();
            let warning = theme.extended().colors.warning;
            let input_el = |index: usize, width: f32, enabled: bool, cx: &App| {
                inputs.get(index).map(|input| {
                    Input::new(input)
                        .w(active_theme(cx).text_extent(width))
                        .disabled(disabled || busy || !enabled)
                        .into_any_element()
                })
            };
            let node_inputs: Vec<gpui::AnyElement> = [
                input_el(0, INPUT_WIDTH * 0.75, selected == 0, cx),
                input_el(1, NUMBER_WIDTH * 0.7, selected == 0, cx),
            ]
            .into_iter()
            .flatten()
            .collect();
            let url_inputs: Vec<gpui::AnyElement> =
                input_el(2, INPUT_WIDTH * 1.5, selected == 1, cx)
                    .into_iter()
                    .collect();
            let labels = [
                SharedString::from(translator.text("ed2kKadBootstrapFromNode").to_owned()),
                SharedString::from(translator.text("ed2kKadBootstrapFromUrl").to_owned()),
                SharedString::from(
                    translator.text_with("ed2kKadBootstrapFromKnown", &[("n", &known.to_string())]),
                ),
            ];
            let mut extras = [node_inputs, url_inputs, Vec::new()].into_iter();
            let options = v_flex().w_full().gap(tokens.spacing.xs).children(
                labels.into_iter().enumerate().map(|(index, label)| {
                    radio_option(
                        SharedString::from(format!("{key}-source-{index}")),
                        label,
                        selected == index,
                        disabled || busy,
                        pick(&store, BOOTSTRAP_SOURCE_KEY, index),
                        extras.next().unwrap_or_default(),
                        cx,
                    )
                }),
            );
            let click_store = store.clone();
            let click_translator = translator.clone();
            let click_inputs = inputs.clone();
            let button = loading_button(
                "ed2k-kad-bootstrap-run",
                if busy { running.clone() } else { run.clone() },
                ButtonVariant::Secondary,
                busy,
                cx,
            )
            .disabled(busy || disabled)
            .on_click(move |_, _, cx| {
                let source = BOOTSTRAP_SOURCES.get(selected).copied().unwrap_or_default();
                let host = input_value(click_inputs.first(), cx);
                let port = input_value(click_inputs.get(1), cx);
                let url = input_value(click_inputs.get(2), cx);
                let params = bootstrap_params(source, &host, &port, &url);
                run_bootstrap(&click_store, &click_translator, params, cx);
            });
            let (result, table) = match stored.map(|stored| bootstrap_view(&stored)) {
                Some(view) => {
                    let (line, hint, failed) = bootstrap_lines(&translator, &view);
                    let table = match &view {
                        BootstrapView::Done { nodes, .. } if !nodes.is_empty() => {
                            Some(node_table(nodes, &translator, cx))
                        }
                        _ => None,
                    };
                    (Some(result_lines(line, hint, failed, warning, cx)), table)
                }
                None => (None, None),
            };
            v_flex()
                .w_full()
                .min_w_0()
                .gap(tokens.spacing.sm)
                .child(row_header(&render_title, &render_description, cx))
                .child(options)
                .child(h_flex().child(button))
                .children(result)
                .children(table)
                .into_any_element()
        },
    );
    row.title = title;
    row.description(description)
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{Ed2kIdCheckTarget, Ed2kKadBootstrapSource};
    use serde_json::json;

    use super::{
        BootstrapView, IdCheckView, bootstrap_params, bootstrap_view, id_check_params,
        id_check_view, kad_id_text, parse_port, selected_index, server_addr,
    };

    #[test]
    fn ed2k_port_parsing_bounds() {
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("65536"), None);
        assert_eq!(parse_port("abc"), None);
        assert_eq!(parse_port(""), None);
        assert_eq!(parse_port("-1"), None);
        assert_eq!(parse_port(" 4661 "), Some(4661));
        assert_eq!(parse_port("65535"), Some(65_535));
    }

    #[test]
    fn ed2k_server_addr_rejects_empty_host_and_ipv6() {
        assert_eq!(server_addr("  ", "4661"), None);
        assert_eq!(server_addr("::1", "4661"), None);
        assert_eq!(server_addr("[::1]", "4661"), None);
        assert_eq!(server_addr("1.2.3.4", "0"), None);
        assert_eq!(
            server_addr(" ed2k.example.org ", "4661"),
            Some("ed2k.example.org:4661".into())
        );
    }

    #[test]
    fn ed2k_id_check_params_by_target() {
        assert_eq!(id_check_params(Ed2kIdCheckTarget::Server, "", "4661"), None);
        let server = id_check_params(Ed2kIdCheckTarget::Server, "1.2.3.4", "4661");
        assert_eq!(
            server.map(|params| params.server),
            Some("1.2.3.4:4661".into())
        );
        // 其余目标忽略输入框。
        let list = id_check_params(Ed2kIdCheckTarget::List, "", "x");
        assert_eq!(
            serde_json::to_value(list).ok(),
            Some(json!({ "target": "list" }))
        );
        let session = id_check_params(Ed2kIdCheckTarget::Session, "1.2.3.4", "4661");
        assert_eq!(
            serde_json::to_value(session).ok(),
            Some(json!({ "target": "session" }))
        );
    }

    #[test]
    fn ed2k_bootstrap_params_by_source() {
        assert_eq!(
            bootstrap_params(Ed2kKadBootstrapSource::Node, " ", "4672", ""),
            None
        );
        assert_eq!(
            bootstrap_params(Ed2kKadBootstrapSource::Node, "a.b", "65536", ""),
            None
        );
        // 引导的主机与端口分开传，IPv6 字面量不需要拼接。
        let node = bootstrap_params(Ed2kKadBootstrapSource::Node, " ::1 ", "4672", "x");
        assert_eq!(
            serde_json::to_value(node).ok(),
            Some(json!({ "source": "node", "host": "::1", "port": 4672 }))
        );
        let url = bootstrap_params(Ed2kKadBootstrapSource::Url, "", "", "  ");
        assert_eq!(
            serde_json::to_value(url).ok(),
            Some(json!({ "source": "url" }))
        );
        let known = bootstrap_params(Ed2kKadBootstrapSource::Known, "h", "1", "u");
        assert_eq!(
            serde_json::to_value(known).ok(),
            Some(json!({ "source": "known" }))
        );
    }

    #[test]
    fn ed2k_selected_index_falls_back_to_default() {
        assert_eq!(selected_index(None, 3, 1), 1);
        assert_eq!(selected_index(Some(&json!(2)), 3, 1), 2);
        assert_eq!(selected_index(Some(&json!(3)), 3, 1), 1);
        assert_eq!(selected_index(Some(&json!("x")), 3, 2), 2);
    }

    fn network(id: &str, error: &str) -> serde_json::Value {
        json!({
            "id": id, "server": "1.2.3.4:4661", "listenPort": 4662, "upnp": "mapped",
            "upnpEnabled": true, "kadEnabled": true, "kadNodes": 0, "serverCount": 3,
            "error": error
        })
    }

    #[test]
    fn ed2k_id_check_maps_high_and_low() {
        assert_eq!(
            id_check_view(&json!({ "target": "list", "result": network("high", "") })),
            IdCheckView::Logged {
                id_key: "ed2kIdHigh",
                server: "1.2.3.4:4661".into(),
                port: 4662,
                upnp_key: "ed2kUpnpMapped",
                low: false,
            }
        );
        assert!(matches!(
            id_check_view(&json!({ "target": "server", "result": network("low", "") })),
            IdCheckView::Logged { low: true, .. }
        ));
    }

    #[test]
    fn ed2k_id_check_session_without_session() {
        assert_eq!(
            id_check_view(&json!({ "target": "session", "result": network("none", "") })),
            IdCheckView::NoSession
        );
        // 会话目标但宿主给了原因 → 失败。
        assert_eq!(
            id_check_view(&json!({ "target": "session", "result": network("none", " boom ") })),
            IdCheckView::Failed {
                error: "boom".into()
            }
        );
        // 非会话目标没有原因同样是失败。
        assert_eq!(
            id_check_view(&json!({ "target": "list", "result": network("none", "") })),
            IdCheckView::Failed {
                error: String::new()
            }
        );
    }

    #[test]
    fn ed2k_id_check_need_server_and_rpc_error() {
        assert_eq!(
            id_check_view(&json!({ "needServer": true })),
            IdCheckView::NeedServer
        );
        assert_eq!(
            id_check_view(&json!({ "target": "list", "rpcError": "timeout" })),
            IdCheckView::Failed {
                error: "timeout".into()
            }
        );
    }

    fn bootstrap(responded: u32, error: &str) -> serde_json::Value {
        json!({
            "result": {
                "contacted": 5, "responded": responded, "discovered": 7, "nodeCount": 40,
                "nodes": [{ "addr": "1.2.3.4:4672", "id": "00", "origin": "bootstrap", "state": "queried" }],
                "error": error
            }
        })
    }

    #[test]
    fn ed2k_bootstrap_view_error_and_no_reply() {
        assert_eq!(
            bootstrap_view(&bootstrap(0, " no known nodes ")),
            BootstrapView::Failed {
                error: "no known nodes".into()
            }
        );
        assert_eq!(
            bootstrap_view(&json!({ "rpcError": "busy" })),
            BootstrapView::Failed {
                error: "busy".into()
            }
        );
        assert_eq!(
            bootstrap_view(&json!({ "needNode": true })),
            BootstrapView::NeedNode
        );
        let BootstrapView::Done {
            responded,
            total,
            nodes,
            ..
        } = bootstrap_view(&bootstrap(0, ""))
        else {
            panic!("expected Done");
        };
        assert_eq!((responded, total, nodes.len()), (0, 40, 1));
    }

    #[test]
    fn ed2k_kad_id_unknown_is_dash() {
        assert_eq!(kad_id_text(""), "—");
        assert_eq!(kad_id_text("00000000000000000000000000000000"), "—");
        assert_eq!(kad_id_text(" 0a0b "), "0a0b");
    }
}
