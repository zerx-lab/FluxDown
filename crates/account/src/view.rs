//! 账户页顶层视图：装配 hero / profile / 账号与安全 / 设备 / 云功能卡片，
//! 以及调试构建独有的 FluxCloud 服务器地址卡片。

use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{CloudEndpointDto, method};
use fluxdown_ui_components::BusyHold;
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::WindowExt as _;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;

use crate::controller::AccountController;
use crate::errors::{ErrorContext, error_text};
use crate::host::{AccountHost, AccountHostEvent};
use crate::link::now_unix_ms;
use crate::pages;
use crate::sync_scope::{SyncPhase, relative_time_changes_in, sync_phase};
use crate::{AccountCommand, AccountPort, PortFuture};

/// 顶部居中卡片列的最大宽度。
const CONTENT_MAX_WIDTH: f32 = 760.;

/// 服务器地址设置只在调试构建出现；
/// 正式包既不显示也不向 agent 查询。
const SERVER_ADDRESS_VISIBLE: bool = cfg!(debug_assertions);

/// 相对同步时间的最长复查间隔（未处于「已同步」或时间点可能被新同步改写时）。
const SYNC_TIME_RECHECK: Duration = Duration::from_secs(60);

pub struct AccountView {
    translator: Entity<Translator>,
    host: Entity<AccountHost>,
    last_error: Option<SharedString>,
    origin_id_copied: bool,
    /// agent 回报的 FluxCloud 地址；`None` 表示未加载或正式构建不查询。
    endpoint: Option<CloudEndpointDto>,
    endpoint_input: Entity<InputState>,
    /// 输入框上次同步到的地址；渲染期发现 `endpoint.base_url` 变化时回写输入框。
    endpoint_synced: SharedString,
    endpoint_busy: bool,
    /// profile 卡片「刷新云端信息」在途：按钮转菊花并拒绝重入。
    cloud_refreshing: bool,
    /// 设备卡片「重试」在途：同上，两者互不影响。
    devices_refreshing: bool,
    /// 已配对设备「刷新」在途。
    linked_refreshing: bool,
    /// 三个刷新按钮的最短可见保持（本地调用常在一帧内返回，菊花否则不可见）；仅由用户点击启动。
    cloud_hold: BusyHold,
    devices_hold: BusyHold,
    linked_hold: BusyHold,
    /// 「立即同步」在途。
    sync_now_busy: bool,
    sync_now_hold: BusyHold,
    /// 「退出登录」在途：按钮置灰并拒绝重入。
    signing_out: bool,
    /// 上次观察到的登录态，用于在切换时清掉旧错误。
    had_session: bool,
    _endpoint_subscription: Subscription,
    _host_subscription: Subscription,
}

impl AccountView {
    pub fn new(
        translator: Entity<Translator>,
        host: Entity<AccountHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        let had_session = host.read(cx).controller.session().is_some();
        cx.observe(&host, |this, host, cx| {
            // 会话登入/登出切换后，上一状态遗留的错误不再适用。
            let has_session = host.read(cx).controller.session().is_some();
            if has_session != this.had_session {
                this.had_session = has_session;
                this.last_error = None;
            }
            cx.notify();
        })
        .detach();
        let _host_subscription = cx.subscribe(&host, |this, _, event, cx| {
            if matches!(event, AccountHostEvent::SnapshotReplaced) {
                this.last_error = None;
                this.load_endpoint(cx);
            }
        });
        let endpoint_input = cx.new(|cx| InputState::new(window, cx));
        let _endpoint_subscription =
            cx.subscribe_in(&endpoint_input, window, |this, input, event, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let value = input.read(cx).value().trim().to_owned();
                    this.commit_endpoint_input(value, window, cx);
                }
            });
        let mut this = Self {
            translator,
            host,
            last_error: None,
            origin_id_copied: false,
            endpoint: None,
            endpoint_input,
            endpoint_synced: SharedString::default(),
            endpoint_busy: false,
            cloud_refreshing: false,
            devices_refreshing: false,
            linked_refreshing: false,
            cloud_hold: BusyHold::default(),
            devices_hold: BusyHold::default(),
            linked_hold: BusyHold::default(),
            sync_now_busy: false,
            sync_now_hold: BusyHold::default(),
            signing_out: false,
            had_session,
            _endpoint_subscription,
            _host_subscription,
        };
        this.load_endpoint(cx);
        Self::spawn_sync_time_ticker(cx);
        this
    }

    /// 「上次同步 N 分钟前」由时钟推进，不在任何被追踪的状态里：retained 渲染只在依赖变化时
    /// 重建本视图，必须在相对时间跨档时主动 notify。视图释放后循环随 `update` 失败结束。
    fn spawn_sync_time_ticker(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut delay = SYNC_TIME_RECHECK;
            loop {
                cx.background_executor().timer(delay).await;
                let Ok(next) = this.update(cx, |this, cx| {
                    let controller = this.controller(cx);
                    let logged_in = controller.session().is_some();
                    let SyncPhase::Synced(Some(at)) =
                        sync_phase(logged_in, controller.sync_status())
                    else {
                        return SYNC_TIME_RECHECK;
                    };
                    cx.notify();
                    // 新一次同步会改写时间点，跨档时刻随之变化：最多等一个复查周期。
                    relative_time_changes_in(now_unix_ms(), at).min(SYNC_TIME_RECHECK)
                }) else {
                    // 账户页已释放，结束定时刷新。
                    return;
                };
                delay = next;
            }
        })
        .detach();
    }

    pub(crate) fn controller<'a>(&self, cx: &'a App) -> &'a AccountController {
        &self.host.read(cx).controller
    }

    pub(crate) fn port(&self, cx: &App) -> Arc<dyn AccountPort> {
        self.host.read(cx).port()
    }

    pub(crate) fn open_profile_edit(
        &mut self,
        field: crate::profile_edit::ProfileField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.signing_out {
            crate::dialogs::profile::open(&self.host, field, window, cx);
        }
    }

    pub(crate) fn open_email_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.signing_out {
            crate::dialogs::email::open(&self.host, window, cx);
        }
    }

    pub(crate) fn open_password_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.signing_out {
            crate::dialogs::password::open(&self.host, window, cx);
        }
    }

    /// 套餐购买对话框；断线 / 退出中不打开。
    pub(crate) fn open_plan_purchase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.signing_out && !self.controller(cx).is_stale() {
            crate::dialogs::plan_purchase::open(&self.host, window, cx);
        }
    }

    /// 「推介有奖」入口开关：交给 app 写设备本地偏好。
    pub(crate) fn request_show_referral(&mut self, value: bool, cx: &mut Context<Self>) {
        self.host
            .update(cx, |host, cx| host.request_show_referral(value, cx));
    }

    /// 通用「发起命令 → 失败提示」；成功结果由快照/事件驱动的重渲染呈现。
    pub(crate) fn spawn_action(
        &mut self,
        future: PortFuture<serde_json::Value>,
        context: ErrorContext,
        cx: &mut Context<Self>,
    ) {
        self.last_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.last_error = Some(error_text(this.translator.read(cx), &error, context));
                }
                cx.notify();
            }) else {
                // 账户视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 「立即同步」：在途期间按钮转菊花并拒绝重入；最短可见时长由 [`BusyHold`] 保证。
    pub(crate) fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.sync_now_busy {
            return;
        }
        let future = self.port(cx).execute(AccountCommand::Sync {
            method: method::AGENT_SYNC_NOW,
            params: serde_json::json!({}),
        });
        self.sync_now_busy = true;
        self.sync_now_hold.start(cx);
        self.last_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, |this, cx| {
                this.sync_now_busy = false;
                if let Err(error) = result {
                    this.last_error = Some(error_text(
                        this.translator.read(cx),
                        &error,
                        ErrorContext::Sync,
                    ));
                }
                cx.notify();
            }) else {
                // 账户视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 登出：用户主动结束会话，agent 不发撤销提示。
    pub(crate) fn sign_out(&mut self, cx: &mut Context<Self>) {
        if self.signing_out {
            return;
        }
        let future = self.port(cx).execute(AccountCommand::Auth {
            method: method::AGENT_AUTH_LOGOUT,
            params: serde_json::json!({}),
        });
        self.signing_out = true;
        self.last_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, |this, cx| {
                this.signing_out = false;
                // agent 先清本地会话再回报吊销失败（如离线）；本机已退出时该错误只会误导。
                let signed_out = this.controller(cx).session().is_none();
                if let Err(error) = result
                    && !signed_out
                {
                    this.last_error = Some(error_text(
                        this.translator.read(cx),
                        &error,
                        ErrorContext::General,
                    ));
                }
                cx.notify();
            }) else {
                // 账户视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 已配对设备「刷新」：探测在线状态；结果经 `LinkedDevicesChanged` 回流。
    pub(crate) fn refresh_linked(&mut self, cx: &mut Context<Self>) {
        if self.linked_refreshing {
            return;
        }
        let future = self.port(cx).execute(AccountCommand::Link {
            method: method::AGENT_LINK_REFRESH,
            params: serde_json::json!({}),
        });
        self.linked_refreshing = true;
        self.linked_hold.start(cx);
        self.last_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, |this, cx| {
                this.linked_refreshing = false;
                if let Err(error) = result {
                    this.last_error = Some(error_text(
                        this.translator.read(cx),
                        &error,
                        ErrorContext::Pairing,
                    ));
                }
                cx.notify();
            }) else {
                // 账户视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 复制 Origin ID 到剪贴板；2 秒内胶囊文案切换为「已复制」。
    pub(crate) fn copy_origin_id(&mut self, origin_id: i64, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(origin_id.to_string()));
        self.origin_id_copied = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let Ok(()) = this.update(cx, |this, cx| {
                this.origin_id_copied = false;
                cx.notify();
            }) else {
                // 账户视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 手动刷新云端全量信息：资料/套餐/能力 + 设备名册。
    /// 在途期间按钮转菊花并拒绝重入；完成后以 toast 告知成功/失败，
    /// 数据本身经会话/设备事件回流重渲染。
    pub(crate) fn refresh_cloud_info(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cloud_refreshing {
            return;
        }
        let port = self.port(cx);
        let profile = port.execute(AccountCommand::Auth {
            method: method::AGENT_AUTH_REFRESH_PROFILE,
            params: serde_json::json!({}),
        });
        let devices = port.execute(AccountCommand::Device {
            method: method::AGENT_DEVICE_LIST,
            params: serde_json::json!({}),
        });
        self.cloud_refreshing = true;
        self.cloud_hold.start(cx);
        self.last_error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = match profile.await {
                Ok(_) => devices.await,
                Err(error) => Err(error),
            };
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.cloud_refreshing = false;
                this.notify_refresh_result(result.map(|_| ()), window, cx);
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    pub(crate) fn refresh_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.devices_refreshing || self.host.read(cx).controller.is_stale() {
            return;
        }
        let reconnect = !self.host.read(cx).controller.cloud_connected();
        let port = self.port(cx);
        self.devices_refreshing = true;
        self.last_error = None;
        self.devices_hold.start(cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                if reconnect {
                    port.execute(AccountCommand::Device {
                        method: method::AGENT_REMOTE_RECONNECT,
                        params: serde_json::json!({}),
                    })
                    .await?;
                }
                port.execute(AccountCommand::Device {
                    method: method::AGENT_DEVICE_LIST,
                    params: serde_json::json!({}),
                })
                .await
            }
            .await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.devices_refreshing = false;
                if reconnect && result.is_ok() {
                    window.push_notification(
                        Notification::success(crate::t(
                            this.translator.read(cx),
                            "cloudConnectionRetryStarted",
                        )),
                        cx,
                    );
                    cx.notify();
                } else {
                    this.notify_refresh_result(result.map(|_| ()), window, cx);
                }
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 刷新类操作的统一结果反馈：成功 toast「云端信息已刷新」，失败 toast 错误文案
    /// 并同时落到卡片内错误行（错误需要可停留阅读）。
    fn notify_refresh_result(
        &mut self,
        result: Result<(), fluxdown_protocol::RpcErrorData>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translator = self.translator.read(cx);
        match result {
            Ok(()) => window.push_notification(
                Notification::success(crate::t(translator, "accountCloudRefreshDone")),
                cx,
            ),
            Err(error) => {
                let message = error_text(translator, &error, ErrorContext::General);
                self.last_error = Some(message.clone());
                window.push_notification(Notification::error(message), cx);
            }
        }
        cx.notify();
    }

    /// 调试构建下向 agent 读取当前 FluxCloud 地址；正式构建直接跳过。
    fn load_endpoint(&mut self, cx: &mut Context<Self>) {
        if !SERVER_ADDRESS_VISIBLE {
            return;
        }
        let future = self.port(cx).execute(AccountCommand::CloudEndpoint {
            method: method::AGENT_CLOUD_ENDPOINT_GET,
            params: serde_json::json!({}),
        });
        cx.spawn(async move |this, cx| {
            let Ok(value) = future.await else {
                return;
            };
            let Ok(endpoint) = serde_json::from_value::<CloudEndpointDto>(value) else {
                return;
            };
            let Ok(()) = this.update(cx, |this, cx| {
                this.endpoint = Some(endpoint);
                cx.notify();
            }) else {
                // 账户视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 输入框失焦/回车：空值或未变化时回滚显示，否则提交覆盖。
    fn commit_endpoint_input(
        &mut self,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(endpoint) = &self.endpoint else {
            return;
        };
        if value.is_empty() || value == endpoint.base_url {
            let current = SharedString::from(endpoint.base_url.clone());
            self.endpoint_input
                .update(cx, |input, cx| input.set_value(current, window, cx));
            return;
        }
        self.set_endpoint(value, window, cx);
    }

    /// 提交 `agent.cloud.endpointSet`（空串 = 恢复默认）；成功/失败均以通知提示，
    /// 失败时把输入框回滚到当前生效地址。
    pub(crate) fn set_endpoint(
        &mut self,
        base_url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.endpoint_busy {
            return;
        }
        self.endpoint_busy = true;
        cx.notify();
        let future = self.port(cx).execute(AccountCommand::CloudEndpoint {
            method: method::AGENT_CLOUD_ENDPOINT_SET,
            params: serde_json::json!({ "baseUrl": base_url }),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.endpoint_busy = false;
                let translator = this.translator.read(cx);
                match result.and_then(|value| {
                    serde_json::from_value::<CloudEndpointDto>(value).map_err(|_| {
                        fluxdown_protocol::RpcErrorData::new(
                            fluxdown_protocol::ApplicationErrorCode::Internal,
                            false,
                        )
                    })
                }) {
                    Ok(endpoint) => {
                        window.push_notification(
                            Notification::success(crate::t(
                                translator,
                                "accountServerAddressSaved",
                            )),
                            cx,
                        );
                        this.endpoint = Some(endpoint);
                    }
                    Err(error) => {
                        let message = if error.code
                            == fluxdown_protocol::ApplicationErrorCode::InvalidArgument
                        {
                            crate::t(translator, "accountServerAddressInvalid")
                        } else {
                            error_text(translator, &error, ErrorContext::General)
                        };
                        window.push_notification(Notification::error(message), cx);
                        if let Some(endpoint) = &this.endpoint {
                            let current = SharedString::from(endpoint.base_url.clone());
                            this.endpoint_input
                                .update(cx, |input, cx| input.set_value(current, window, cx));
                        }
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

    /// 渲染前把 agent 回报的地址回写到输入框（仅在生效地址变化时）。
    fn sync_endpoint_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(endpoint) = &self.endpoint else {
            return;
        };
        if self.endpoint_synced.as_ref() == endpoint.base_url {
            return;
        }
        self.endpoint_synced = SharedString::from(endpoint.base_url.clone());
        let value = self.endpoint_synced.clone();
        self.endpoint_input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

impl Render for AccountView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_endpoint_input(window, cx);
        let translator = self.translator.read(cx).clone();
        let tokens = active_theme(cx).tokens().clone();
        let (disabled, session, devices, linked, sync) = {
            let controller = self.controller(cx);
            (
                controller.is_stale(),
                controller.session().cloned(),
                controller.devices().to_vec(),
                controller.linked_devices().to_vec(),
                controller.sync_status().clone(),
            )
        };
        // 连接断开时只读并提示；恢复后由快照清除。
        let last_error = if disabled {
            Some(crate::t(&translator, "localServiceDisconnected"))
        } else {
            self.last_error.clone()
        };
        let origin_id_copied = self.origin_id_copied;
        let port = self.port(cx);
        let server_address = self
            .endpoint
            .as_ref()
            .filter(|endpoint| SERVER_ADDRESS_VISIBLE && endpoint.editable)
            .cloned();

        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(CONTENT_MAX_WIDTH))
            .gap(tokens.spacing.xl);

        column = match &session {
            None => column.child(pages::hero::render(
                &translator,
                &self.translator,
                &tokens,
                port.clone(),
                disabled,
                last_error,
                cx,
            )),
            Some(session) => column.child(pages::profile::render(
                &translator,
                &tokens,
                session,
                pages::profile::ProfileState {
                    copied: origin_id_copied,
                    disabled: disabled || self.signing_out,
                    refreshing: self.cloud_refreshing || self.cloud_hold.is_held(),
                    last_error,
                    show_referral: self.host.read(cx).show_referral(),
                },
                cx,
            )),
        };

        let logged_in = session.is_some();
        if let Some(session) = &session {
            column = column.child(pages::security::render(
                &translator,
                &tokens,
                session,
                disabled || self.signing_out,
                cx,
            ));
        }
        // 已配对设备与账号无关：未登录也能管理；云设备仅登录后显示。
        column = column.child(pages::devices::render(
            &translator,
            &tokens,
            &pages::devices::DevicesState {
                host: &self.host,
                devices: &devices,
                linked: &linked,
                logged_in,
                disabled,
                refreshing: self.devices_refreshing || self.devices_hold.is_held(),
                linked_refreshing: self.linked_refreshing || self.linked_hold.is_held(),
            },
            cx,
        ));
        column = column.child(pages::cloud_features::render(
            &translator,
            &tokens,
            pages::cloud_features::CloudFeaturesState {
                logged_in,
                disabled,
                sync_now_busy: self.sync_now_busy || self.sync_now_hold.is_held(),
            },
            &sync,
            (!self.host.read(cx).controller.is_stale()
                && self.host.read(cx).controller.cloud_connected())
            .then_some(devices.as_slice()),
            self.host.read(cx).controller.max_online_devices(),
            cx,
        ));
        if let Some(endpoint) = server_address {
            column = column.child(pages::server_address::render(
                &translator,
                &tokens,
                &endpoint,
                &self.endpoint_input,
                disabled || self.endpoint_busy,
                cx,
            ));
        }

        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .child(column)
    }
}
