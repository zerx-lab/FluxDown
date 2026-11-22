//! 「添加设备」对话框：「账号（推荐）」说明页 + 「直连配对」页。
//!
//! 直连配对页：本机配对码（供对端输入）、局域网发现列表 / 手动地址、输入对端配对码 →
//! `pairBegin` → 核对 SAS → `pairFinish`。对话框存活期间开启发现与配对广播，关闭时收回。

use fluxdown_protocol::{
    LinkDiscoveryParams, LinkPairBeginParams, LinkPairBeginResponse, LinkPairFinishParams,
    LinkPairFinishResponse, LinkPairingCodeDto, method,
};
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, dialog_scroll_body, field_error, field_hint, field_label,
    form_field, input_with_action, segmented_tabs, tabular_numbers,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};

use crate::errors::{ErrorContext, error_text};
use crate::host::AccountHost;
use std::sync::Arc;

use crate::link::{
    DiscoveredEntry, PairFlow, discovered_entries, group_sas, local_deadline_ms, now_unix_ms,
    seconds_until, validate_pair_input,
};
use crate::verification::spawn_ticker;
use crate::{AccountCommand, AccountPort, PortFuture, t, t_with};

/// 发现开启（或「重新搜索」）后多久内列表为空仍算「搜索中」。
const SCAN_WINDOW_MS: i64 = 8_000;

fn discovery_params(enabled: bool) -> serde_json::Value {
    serde_json::to_value(LinkDiscoveryParams { enabled }).unwrap_or_default()
}

/// 放弃一个已建立的发起端配对会话（`pairFinish(accept:false)`）；会话已过期视为已达成。
async fn abandon_session(port: Arc<dyn AccountPort>, token: String) {
    let params = serde_json::to_value(LinkPairFinishParams {
        token,
        accept: false,
    })
    .unwrap_or_default();
    let result = port
        .execute(AccountCommand::Link {
            method: method::AGENT_LINK_PAIR_FINISH,
            params,
        })
        .await;
    if let Err(error) = result {
        if error.reason == Some(fluxdown_protocol::ErrorReason::PairingSessionExpired) {
            return;
        }
        eprintln!(
            "abandoning local pairing session failed: {:?} ({:?})",
            error.code, error.reason
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tab {
    Account,
    Direct,
}

struct AddDeviceDialog {
    host: Entity<AccountHost>,
    tab: Tab,
    address_input: Entity<InputState>,
    code_input: Entity<InputState>,
    /// 直连页已开启发现 / 配对广播（关闭时需要收回）。
    direct_started: bool,
    own_code: Option<LinkPairingCodeDto>,
    /// 本机码的本机截止时刻（收到时刻 + ttlMs；免疫主机时钟偏差）。
    own_code_deadline_ms: i64,
    own_code_busy: bool,
    own_code_error: Option<SharedString>,
    flow: PairFlow,
    error: Option<SharedString>,
    selected_address: Option<String>,
    closed: bool,
    seen_incoming: usize,
    /// 本轮发现开始时间（Unix 毫秒）：8 秒内列表为空显示「搜索中」。
    scan_started_ms: i64,
    /// 「重新搜索」（stop → start）进行中。
    rescanning: bool,
    /// 发起流程代数：放弃 / 取消 / 离开页签时递增，迟到的异步响应据此识别并丢弃。
    generation: u64,
}

/// 打开「添加设备」对话框。已登录时默认停在「账号」页，否则直接进「直连配对」。
pub fn open(host: &Entity<AccountHost>, window: &mut Window, cx: &mut App) {
    let title = t(host.read(cx).translator().read(cx), "addDeviceEntry");
    let view = cx.new(|cx| AddDeviceDialog::new(host.clone(), window, cx));
    let closing = view.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        let closing = closing.clone();
        dialog
            .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(560.))
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
            .on_close(move |_, _, cx| {
                closing.update(cx, |this, cx| this.shutdown(cx));
            })
    });
}

impl AddDeviceDialog {
    fn new(host: Entity<AccountHost>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let translator = host.read(cx).translator().clone();
        let address_placeholder = SharedString::from("192.168.1.20:17800");
        let code_placeholder = t(translator.read(cx), "localPairingCodePlaceholder");
        cx.observe(&host, |this, host, cx| {
            // 对端输入本机码后码即被 agent 消耗；出现新的入站请求时换新码，避免界面仍展示已失效的旧码。
            let incoming = host.read(cx).pending_pairing_requests().len();
            if incoming > this.seen_incoming && this.direct_started && !this.closed {
                this.refresh_own_code(cx);
            }
            this.seen_incoming = incoming;
            cx.notify();
        })
        .detach();
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        let logged_in = host.read(cx).controller.session().is_some();
        let seen_incoming = host.read(cx).pending_pairing_requests().len();
        let mut this = Self {
            host,
            tab: if logged_in { Tab::Account } else { Tab::Direct },
            address_input: cx
                .new(|cx| InputState::new(window, cx).placeholder(address_placeholder)),
            code_input: cx.new(|cx| InputState::new(window, cx).placeholder(code_placeholder)),
            direct_started: false,
            own_code: None,
            own_code_deadline_ms: 0,
            own_code_busy: false,
            own_code_error: None,
            flow: PairFlow::Form,
            error: None,
            selected_address: None,
            closed: false,
            seen_incoming,
            scan_started_ms: now_unix_ms(),
            rescanning: false,
            generation: 0,
        };
        spawn_ticker(window, cx, |this: &mut Self, _, cx| {
            if this.closed {
                return false;
            }
            // 本机配对码到期（或局域网刚生效而还没有码）自动换新；失败后不再自动重试，
            // 交给「重新生成」。局域网未生效时不申请码。
            if this.direct_started
                && this.lan_active(cx)
                && !this.own_code_busy
                && this.own_code_error.is_none()
                && this
                    .own_code
                    .as_ref()
                    .is_none_or(|_| seconds_until(this.own_code_deadline_ms, now_unix_ms()) == 0)
            {
                this.refresh_own_code(cx);
            }
            if this.direct_started {
                cx.notify();
            }
            true
        });
        // 手动改地址后，发现列表的选中高亮不再对应当前输入。
        cx.subscribe(
            &this.address_input,
            |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change)
                    && this
                        .selected_address
                        .as_deref()
                        .is_some_and(|selected| selected != input.read(cx).value().as_str())
                {
                    this.selected_address = None;
                    cx.notify();
                }
            },
        )
        .detach();
        if this.tab == Tab::Direct {
            this.start_direct(cx);
        }
        this
    }

    fn t(&self, key: &str, cx: &App) -> SharedString {
        t(self.host.read(cx).translator().read(cx), key)
    }

    fn link_call(
        &self,
        method: &'static str,
        params: serde_json::Value,
        cx: &App,
    ) -> PortFuture<serde_json::Value> {
        self.host
            .read(cx)
            .port()
            .execute(AccountCommand::Link { method, params })
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        match tab {
            Tab::Direct => self.start_direct(cx),
            // 离开直连页 = 对直连而言等同关闭对话框：停发现、撤回广播、放弃进行中的发起流程。
            Tab::Account => self.stop_direct(cx),
        }
        cx.notify();
    }

    fn lan_active(&self, cx: &App) -> bool {
        self.host.read(cx).controller.lan_active()
    }

    /// 进入直连页：开启局域网发现；本机局域网已生效时才生成本机配对码
    /// （未生效时本机只能作为发起方，对端无法回连）。
    fn start_direct(&mut self, cx: &mut Context<Self>) {
        if self.direct_started {
            return;
        }
        self.direct_started = true;
        self.scan_started_ms = now_unix_ms();
        self.set_discovery(true, cx);
        if self.lan_active(cx) {
            self.refresh_own_code(cx);
        }
    }

    /// 放弃当前发起流程：递增代数让迟到响应失效；核对中的会话立即释放。
    fn abandon_flow(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(token) = self.flow.session_token().map(str::to_owned) {
            let port = self.host.read(cx).port();
            cx.background_spawn(abandon_session(port, token)).detach();
        }
        self.flow = PairFlow::Form;
    }

    /// 收回发现与配对广播并放弃发起流程（离开直连页 / 关闭对话框）。幂等。
    fn stop_direct(&mut self, cx: &mut Context<Self>) {
        self.abandon_flow(cx);
        if !std::mem::replace(&mut self.direct_started, false) {
            return;
        }
        let stopped_code = self.own_code.take().map(|code| code.code);
        self.own_code_error = None;
        self.rescanning = false;
        self.selected_address = None;
        let discovery = self.link_call(
            method::AGENT_LINK_DISCOVERY_SET,
            discovery_params(false),
            cx,
        );
        let stop_params = match stopped_code {
            Some(code) => serde_json::json!({ "code": code }),
            None => serde_json::json!({}),
        };
        let stop = self.link_call(method::AGENT_LINK_STOP_PAIRING, stop_params, cx);
        cx.background_spawn(async move {
            if let Err(error) = discovery.await {
                // 发现停止失败也必须继续撤回配对广播。
                eprintln!(
                    "stopping local discovery failed: {:?} ({:?})",
                    error.code, error.reason
                );
            }
            if let Err(error) = stop.await {
                eprintln!(
                    "stopping local pairing broadcast failed: {:?} ({:?})",
                    error.code, error.reason
                );
            }
        })
        .detach();
        cx.notify();
    }

    fn set_discovery(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let params = serde_json::to_value(LinkDiscoveryParams { enabled }).unwrap_or_default();
        let future = self.link_call(method::AGENT_LINK_DISCOVERY_SET, params, cx);
        cx.spawn(async move |this, cx| {
            if let Err(error) = future.await {
                if !enabled {
                    // 停止发现不回写已关闭的对话框，但仍记录真实失败。
                    eprintln!(
                        "disabling local device discovery failed: {:?} ({:?})",
                        error.code, error.reason
                    );
                    return;
                }
                let Ok(()) = this.update(cx, |this, cx| {
                    this.error = Some(error_text(
                        this.host.read(cx).translator().read(cx),
                        &error,
                        ErrorContext::Pairing,
                    ));
                    cx.notify();
                }) else {
                    // 账户视图或窗口已释放，结束回调，不再更新状态。
                    return;
                };
            }
        })
        .detach();
    }

    fn refresh_own_code(&mut self, cx: &mut Context<Self>) {
        if self.own_code_busy || !self.lan_active(cx) {
            return;
        }
        self.own_code_busy = true;
        self.own_code_error = None;
        cx.notify();
        let future = self.link_call(method::AGENT_LINK_PAIRING_CODE, serde_json::json!({}), cx);
        cx.spawn(async move |this, cx| {
            let result = future.await.and_then(|value| {
                serde_json::from_value::<LinkPairingCodeDto>(value).map_err(|_| {
                    fluxdown_protocol::RpcErrorData::new(
                        fluxdown_protocol::ApplicationErrorCode::Internal,
                        false,
                    )
                })
            });
            let Ok(()) = this.update(cx, |this, cx| {
                this.own_code_busy = false;
                // 期间已离开直连页 / 关闭对话框：广播已撤回，结果作废（回到页签后由 ticker 重新申请）。
                if this.direct_started && !this.closed {
                    match result {
                        Ok(code) => {
                            this.own_code_deadline_ms = local_deadline_ms(
                                code.ttl_ms,
                                code.expires_at_unix_ms,
                                now_unix_ms(),
                            );
                            this.own_code = Some(code);
                        }
                        Err(error) => {
                            this.own_code_error = Some(error_text(
                                this.host.read(cx).translator().read(cx),
                                &error,
                                ErrorContext::Pairing,
                            ));
                        }
                    }
                }
                cx.notify();
            }) else {
                // 账户视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 重新扫描：串行 stop → start，让 agent 重新发起一轮发现。
    fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.rescanning || !self.direct_started {
            return;
        }
        self.error = None;
        self.rescanning = true;
        self.scan_started_ms = now_unix_ms();
        cx.notify();
        let port = self.host.read(cx).port();
        cx.spawn(async move |this, cx| {
            let discovery = |enabled: bool| AccountCommand::Link {
                method: method::AGENT_LINK_DISCOVERY_SET,
                params: discovery_params(enabled),
            };
            if let Err(error) = port.execute(discovery(false)).await {
                eprintln!(
                    "stopping local discovery for rescan failed: {:?} ({:?})",
                    error.code, error.reason
                );
            }
            let still_open = this
                .update(cx, |this, _| this.direct_started && !this.closed)
                .unwrap_or(false);
            let started = if still_open {
                Some(port.execute(discovery(true)).await)
            } else {
                None
            };
            let leaked = this
                .update(cx, |this, cx| {
                    this.rescanning = false;
                    let open = this.direct_started && !this.closed;
                    let started_ok = matches!(started, Some(Ok(_)));
                    if let Some(Err(error)) = started
                        && open
                    {
                        this.error = Some(error_text(
                            this.host.read(cx).translator().read(cx),
                            &error,
                            ErrorContext::Pairing,
                        ));
                    }
                    cx.notify();
                    started_ok && !open
                })
                .unwrap_or(false);
            if leaked && let Err(error) = port.execute(discovery(false)).await {
                // 重新搜索期间对话框被关闭：补一次停止，避免发现残留。
                eprintln!(
                    "stopping leaked local discovery failed: {:?} ({:?})",
                    error.code, error.reason
                );
            }
        })
        .detach();
    }

    fn select_peer(
        &mut self,
        entry: &DiscoveredEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entry.paired || !self.flow.can_begin() {
            return;
        }
        self.selected_address = Some(entry.address.clone());
        let address = SharedString::from(entry.address.clone());
        self.address_input
            .update(cx, |input, cx| input.set_value(address, window, cx));
        self.code_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn begin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.flow.can_begin() {
            return;
        }
        let address = self.address_input.read(cx).value().to_string();
        let code = self.code_input.read(cx).value().to_string();
        let (address, code) = match validate_pair_input(&address, &code) {
            Ok(input) => input,
            Err(error) => {
                self.error = Some(self.t(error.key(), cx));
                cx.notify();
                return;
            }
        };
        self.error = None;
        self.flow = PairFlow::Beginning;
        let generation = self.generation;
        let port = self.host.read(cx).port();
        cx.notify();
        let params = serde_json::to_value(LinkPairBeginParams {
            address,
            code,
            request_control: false,
        })
        .unwrap_or_default();
        let future = self.link_call(method::AGENT_LINK_PAIR_BEGIN, params, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(|value| {
                serde_json::from_value::<LinkPairBeginResponse>(value).map_err(|_| {
                    fluxdown_protocol::RpcErrorData::new(
                        fluxdown_protocol::ApplicationErrorCode::Internal,
                        false,
                    )
                })
            });
            let token = result.as_ref().ok().map(|response| response.token.clone());
            let applied = this.update_in(cx, |this, _, cx| {
                // 对话框已关 / 已离开页签 / 已取消：迟到的响应不再回写界面。
                if this.closed || this.generation != generation {
                    return false;
                }
                match result {
                    Ok(response) => {
                        let flow = std::mem::replace(&mut this.flow, PairFlow::Form);
                        this.flow = flow.began(response.token, response.sas, response.peer_name);
                    }
                    Err(error) => {
                        this.error = Some(error_text(
                            this.host.read(cx).translator().read(cx),
                            &error,
                            ErrorContext::Pairing,
                        ));
                        this.flow = std::mem::replace(&mut this.flow, PairFlow::Form).failed();
                    }
                }
                cx.notify();
                true
            });
            // 响应没有被界面接收（对话框已释放 / 已放弃）：立刻释放 agent 上的会话，
            // 否则对端会留下一条陈旧的配对请求。
            if !matches!(applied, Ok(true))
                && let Some(token) = token
            {
                abandon_session(port, token).await;
            }
        })
        .detach();
    }

    /// 核对 SAS 后确认（`accept=true`）或放弃（回到表单）。
    fn finish(&mut self, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        let PairFlow::Verify { token, .. } = self.flow.clone() else {
            return;
        };
        self.error = None;
        if !accept {
            // 放弃：只需通知 agent 释放会话，结果不影响界面。
            self.flow = PairFlow::Form;
            let port = self.host.read(cx).port();
            cx.background_spawn(abandon_session(port, token)).detach();
            cx.notify();
            return;
        }
        let flow = std::mem::replace(&mut self.flow, PairFlow::Form);
        self.flow = flow.confirming();
        let generation = self.generation;
        cx.notify();
        let params =
            serde_json::to_value(LinkPairFinishParams { token, accept }).unwrap_or_default();
        let future = self.link_call(method::AGENT_LINK_PAIR_FINISH, params, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(|value| {
                serde_json::from_value::<LinkPairFinishResponse>(value).map_err(|_| {
                    fluxdown_protocol::RpcErrorData::new(
                        fluxdown_protocol::ApplicationErrorCode::Internal,
                        false,
                    )
                })
            });
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                // 用户已取消等待 / 离开页签 / 关闭对话框：结果不再回写界面
                // （若对端仍确认，设备会出现在已配对列表里）。
                if this.closed || this.generation != generation {
                    return;
                }
                match result {
                    Ok(response) => {
                        let paired = response.paired;
                        let name = response.device.map(|device| device.name);
                        let flow = std::mem::replace(&mut this.flow, PairFlow::Form);
                        this.flow = flow.finished(paired, name);
                        if let PairFlow::Done { device_name } = &this.flow {
                            let message = t_with(
                                this.host.read(cx).translator().read(cx),
                                "localPairingPaired",
                                &[("device", device_name)],
                            );
                            window.push_notification(toast::success(message), cx);
                        } else {
                            this.error = Some(this.t("errReasonPairingRejected", cx));
                        }
                    }
                    Err(error) => {
                        this.error = Some(error_text(
                            this.host.read(cx).translator().read(cx),
                            &error,
                            ErrorContext::Pairing,
                        ));
                        this.flow = std::mem::replace(&mut this.flow, PairFlow::Form).failed();
                    }
                }
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 等待对端确认期间取消：回到表单并放弃等待。
    fn cancel_waiting(&mut self, cx: &mut Context<Self>) {
        if matches!(self.flow, PairFlow::Finishing { .. }) {
            // 通知 agent 中止在途确认，否则对端之后批准仍会悄悄配对成功。
            self.abandon_flow(cx);
            cx.notify();
        }
    }

    /// 对话框关闭：收回发现与配对广播；发起流程中途关闭视为放弃。幂等。
    fn shutdown(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        self.stop_direct(cx);
    }

    fn copy(&self, text: String, message_key: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let message = self.t(message_key, cx);
        window.push_notification(toast::success(message), cx);
    }

    // ───────────────────────── 渲染 ─────────────────────────

    fn render_account_tab(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = active_theme(cx).tokens().clone();
        let translator = self.host.read(cx).translator().read(cx).clone();
        let session = self.host.read(cx).controller.session().cloned();
        let logged_in = session.is_some();
        let body = match session {
            Some(session) => t_with(
                &translator,
                "addDeviceAccountSynced",
                &[("account", &session.user.email)],
            ),
            None => t(&translator, "addDeviceLoginRequired"),
        };
        v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .child(
                div()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.colors.foreground)
                    .child(body),
            )
            .child(field_hint(t(&translator, "addDeviceHint"), cx))
            .child(field_hint(t(&translator, "addDeviceAccountFooter"), cx))
            .when(!logged_in, |column| {
                column.child(
                    h_flex().w_full().justify_start().child(
                        Button::new("add-device-login")
                            .primary()
                            .label(t(&translator, "accountLogin"))
                            .control(cx)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                let translator = this.host.read(cx).translator().clone();
                                let port = this.host.read(cx).port();
                                this.shutdown(cx);
                                window.close_dialog(cx);
                                crate::dialogs::login::open(translator, port, window, cx);
                            })),
                    ),
                )
            })
            .into_any_element()
    }

    fn render_own_code(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = active_theme(cx).tokens().clone();
        let extended = active_theme(cx).extended().clone();
        let translator = self.host.read(cx).translator().read(cx).clone();
        let (lan_enabled, lan_active) = {
            let controller = &self.host.read(cx).controller;
            (controller.lan_enabled(), controller.lan_active())
        };
        let mut section = v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(field_label(t(&translator, "localPairingMyCodeTitle"), cx));
        if !lan_active {
            // 本机局域网未生效：不展示也不申请配对码，只说明原因（未开启给「前往设置」，
            // 已开启但未重启给重启提示）。
            if lan_enabled {
                return section
                    .child(field_hint(t(&translator, "localPairingLanRestartHint"), cx))
                    .into_any_element();
            }
            return section
                .child(field_hint(
                    t(&translator, "localPairingLanDisabledHint"),
                    cx,
                ))
                .child(
                    h_flex().w_full().justify_start().child(
                        Button::new("add-device-open-lan-settings")
                            .outline()
                            .label(t(&translator, "localPairingOpenLanSettings"))
                            .control(cx)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.host
                                    .update(cx, |host, cx| host.request_open_lan_settings(cx));
                            })),
                    ),
                )
                .into_any_element();
        }
        section = section.child(field_hint(t(&translator, "localPairingMyCodeHint"), cx));
        section = section.child(field_hint(t(&translator, "linkControlPairCodeHint"), cx));
        if let Some(error) = self.own_code_error.clone() {
            section = section.child(field_error(error, cx));
        }
        if let Some(code) = &self.own_code {
            let remaining = seconds_until(self.own_code_deadline_ms, now_unix_ms());
            let status = if remaining == 0 {
                t(&translator, "localPairingMyCodeExpired")
            } else {
                t_with(
                    &translator,
                    "localDeviceCodeRemaining",
                    &[("seconds", &remaining.to_string())],
                )
            };
            let code_text = code.code.clone();
            section = section.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(tokens.spacing.md)
                    .child(
                        v_flex()
                            .gap(tokens.spacing.xxs)
                            .child(
                                div()
                                    .text_size(extended.title.size * 1.6)
                                    .line_height(extended.title.line_height * 1.6)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .font_features(tabular_numbers())
                                    .text_color(tokens.colors.foreground)
                                    .child(code.code.clone()),
                            )
                            .child(field_hint(status, cx)),
                    )
                    .child(
                        h_flex()
                            .gap(tokens.spacing.sm)
                            .child(
                                Button::new("add-device-copy-code")
                                    .outline()
                                    .label(self.t("localDeviceCodeCopy", cx))
                                    .control(cx)
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.copy(
                                                code_text.clone(),
                                                "localDeviceCodeCopied",
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                            )
                            .child(
                                Button::new("add-device-regenerate-code")
                                    .outline()
                                    .label(self.t("localPairingMyCodeRefresh", cx))
                                    .control(cx)
                                    .busy(self.own_code_busy)
                                    .disabled(self.own_code_busy)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.refresh_own_code(cx);
                                    })),
                            ),
                    ),
            );
            if code.addresses.is_empty() {
                section = section.child(field_hint(t(&translator, "localPairingNoAddress"), cx));
            } else {
                section = section
                    .child(field_label(t(&translator, "localPairingMyAddresses"), cx))
                    .children(code.addresses.iter().map(|address| {
                        div()
                            .text_size(tokens.typography.sm.size)
                            .line_height(tokens.typography.sm.line_height)
                            .font_features(tabular_numbers())
                            .text_color(tokens.colors.foreground)
                            .child(address.clone())
                    }))
                    .child(field_hint(t(&translator, "localDeviceAddressHint"), cx));
            }
        } else if self.own_code_busy {
            section = section.child(field_hint(t(&translator, "localPairingDiscovering"), cx));
        } else if self.own_code_error.is_some() {
            // 首次生成失败后 ticker 不再自动重试：提供手动重试入口。
            section = section.child(
                h_flex().w_full().justify_start().child(
                    Button::new("add-device-retry-code")
                        .outline()
                        .label(self.t("localPairingMyCodeRefresh", cx))
                        .control(cx)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.refresh_own_code(cx);
                        })),
                ),
            );
        }
        section.into_any_element()
    }

    fn render_discovery(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = active_theme(cx).tokens().clone();
        let extended = active_theme(cx).extended().clone();
        let translator = self.host.read(cx).translator().read(cx).clone();
        let entries = {
            let host = self.host.read(cx);
            discovered_entries(
                host.controller.discovered(),
                host.controller.linked_devices(),
            )
        };
        let selectable = self.flow.can_begin();
        let mut list = v_flex().w_full().gap(tokens.spacing.xxs);
        let has_entries = !entries.is_empty();
        // 搜索中：发现开启 / 重新搜索后 8 秒内列表仍为空；之后仍为空才是「没有发现设备」。
        let scanning = !has_entries
            && (self.rescanning || now_unix_ms() - self.scan_started_ms < SCAN_WINDOW_MS);
        for (index, entry) in entries.into_iter().enumerate() {
            let selected = self.selected_address.as_deref() == Some(entry.address.as_str());
            let hover = extended.colors.row_hover;
            let subtitle = match &entry.platform {
                Some(platform) if !platform.is_empty() => format!("{platform} · {}", entry.address),
                _ => entry.address.clone(),
            };
            let picked = entry.clone();
            list = list.child(
                h_flex()
                    .id(("add-device-peer", index))
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(tokens.spacing.md)
                    .px(tokens.spacing.sm)
                    .py(tokens.spacing.xs)
                    .rounded(tokens.radius.md)
                    .when(selected, |row| row.bg(tokens.colors.muted))
                    .when(selectable && !entry.paired, |row| {
                        row.cursor_pointer().hover(move |style| style.bg(hover))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_peer(&picked, window, cx);
                    }))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(tokens.typography.sm.size)
                                    .line_height(tokens.typography.sm.line_height)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tokens.colors.foreground)
                                    .child(entry.name.clone()),
                            )
                            .child(field_hint(subtitle, cx)),
                    )
                    .when(entry.paired, |row| {
                        row.child(field_hint(t(&translator, "localDevicePairedTag"), cx))
                    }),
            );
        }
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(if has_entries {
                        field_hint(t(&translator, "localPairingNearbyTitle"), cx).into_any_element()
                    } else if scanning {
                        h_flex()
                            .items_center()
                            .gap(tokens.spacing.xs)
                            .child(
                                Spinner::new()
                                    .with_size(extended.icon.sm)
                                    .color(tokens.colors.muted_foreground),
                            )
                            .child(field_hint(t(&translator, "localPairingDiscovering"), cx))
                            .into_any_element()
                    } else {
                        field_hint(t(&translator, "localPairingNoDevices"), cx).into_any_element()
                    })
                    .child(
                        Button::new("add-device-rescan")
                            .ghost()
                            .label(self.t("localPairingRetryScan", cx))
                            .control(cx)
                            .disabled(self.rescanning)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.rescan(cx);
                            })),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn render_pair_form(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = active_theme(cx).tokens().clone();
        let busy = self.flow.is_busy();
        let lan_active = self.lan_active(cx);
        v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .when(!lan_active, |column| {
                column.child(field_hint(self.t("localPairingOneWayHint", cx), cx))
            })
            .child(self.render_discovery(cx))
            .child(form_field(
                self.t("localPairingManualAddress", cx),
                Input::new(&self.address_input).control(cx).w_full(),
                Some(self.t("localPairingAddressHint", cx)),
                cx,
            ))
            .child(form_field(
                self.t("localPairingCodeLabel", cx),
                input_with_action(
                    Input::new(&self.code_input).control(cx).w_full(),
                    Button::new("add-device-connect")
                        .primary()
                        .label(self.t("localPairingConnect", cx))
                        .control(cx)
                        .busy(busy)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.begin(window, cx);
                        })),
                    cx,
                ),
                Some(self.t("localPairingCodeHint", cx)),
                cx,
            ))
            .into_any_element()
    }

    fn render_verify(
        &self,
        sas: &str,
        peer_name: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tokens = active_theme(cx).tokens().clone();
        let extended = active_theme(cx).extended().clone();
        v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .child(field_label(self.t("localPairingSasTitle", cx), cx))
            .child(field_hint(self.t("localPairingSasHint", cx), cx))
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .py(tokens.spacing.md)
                    .rounded(tokens.radius.md)
                    .bg(tokens.colors.muted)
                    .text_size(extended.title.size * 2.)
                    .line_height(extended.title.line_height * 2.)
                    .font_weight(FontWeight::SEMIBOLD)
                    .font_features(tabular_numbers())
                    .text_color(tokens.colors.foreground)
                    .child(group_sas(sas)),
            )
            .child(field_hint(peer_name.to_owned(), cx))
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("add-device-sas-reject")
                            .outline()
                            .label(self.t("localPairingReject", cx))
                            .control(cx)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.finish(false, window, cx);
                            })),
                    )
                    .child(
                        Button::new("add-device-sas-confirm")
                            .primary()
                            .label(self.t("localPairingConfirm", cx))
                            .control(cx)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.finish(true, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_direct_tab(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = active_theme(cx).tokens().clone();
        let translator = self.host.read(cx).translator().read(cx).clone();
        let pair_section = match &self.flow {
            PairFlow::Form | PairFlow::Beginning => self.render_pair_form(cx),
            PairFlow::Verify { sas, peer_name, .. } => self.render_verify(sas, peer_name, cx),
            PairFlow::Finishing { .. } => v_flex()
                .w_full()
                .gap(tokens.spacing.md)
                .child(field_hint(t(&translator, "localPairingWaitingPeer"), cx))
                .child(
                    h_flex().w_full().justify_end().child(
                        Button::new("add-device-finishing-cancel")
                            .outline()
                            .label(self.t("cancel", cx))
                            .control(cx)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.cancel_waiting(cx);
                            })),
                    ),
                )
                .into_any_element(),
            PairFlow::Done { device_name } => v_flex()
                .w_full()
                .gap(tokens.spacing.md)
                .child(
                    div()
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tokens.colors.foreground)
                        .child(t_with(
                            &translator,
                            "localPairingPaired",
                            &[("device", device_name)],
                        )),
                )
                .child(
                    h_flex().w_full().justify_end().child(
                        Button::new("add-device-done")
                            .primary()
                            .label(self.t("close", cx))
                            .control(cx)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.shutdown(cx);
                                window.close_dialog(cx);
                            })),
                    ),
                )
                .into_any_element(),
        };
        v_flex()
            .w_full()
            .gap(tokens.spacing.lg)
            .child(field_hint(t(&translator, "localPairingHint"), cx))
            .child(self.render_own_code(cx))
            .child(
                div()
                    .w_full()
                    .h(active_theme(cx).extended().stroke.thin)
                    .bg(active_theme(cx).extended().colors.hairline),
            )
            .child(pair_section)
            .when_some(self.error.clone(), |column, error| {
                column.child(field_error(error, cx))
            })
            .into_any_element()
    }
}

impl Render for AddDeviceDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let labels = [
            self.t("addDeviceTabAccount", cx),
            self.t("addDeviceTabLocal", cx),
        ];
        let selected = usize::from(self.tab == Tab::Direct);
        let view = cx.entity();
        let body = match self.tab {
            Tab::Account => self.render_account_tab(cx),
            Tab::Direct => self.render_direct_tab(cx),
        };
        v_flex()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(segmented_tabs(
                "add-device-tabs",
                labels,
                selected,
                move |index, _, cx| {
                    let tab = if index == 0 {
                        Tab::Account
                    } else {
                        Tab::Direct
                    };
                    view.update(cx, |this, cx| this.select_tab(tab, cx));
                },
                cx,
            ))
            .child(dialog_scroll_body(
                "add-device-body",
                Some(px(460.)),
                body,
                cx,
            ))
    }
}
