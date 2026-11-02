//! 插件、插件市场与受管组件（ffmpeg / yt-dlp）capability。

mod components;
mod controller;
mod market;
mod pages;
mod ui;

use std::{future::Future, pin::Pin, rc::Rc, sync::Arc};

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, ApplicationErrorCode, ErrorReason, RpcErrorData, ServiceEvent,
};
use fluxdown_ui_components::segmented_tabs;
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement,
    Render, SharedString, Styled, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{WindowExt as _, h_flex, notification::Notification, v_flex};

use components::plugin_live::LivePlugins;
use controller::{COMPONENT_KINDS, component_slot};
pub use controller::{ExtensionsController, ExtensionsSignal};
use pages::{managed_components::ComponentUi, plugins::PluginsUi};

pub type PortFuture<T> = Pin<Box<dyn Future<Output = Result<T, RpcErrorData>> + Send + 'static>>;

/// 插件视图的承载位置：宿主（app）据此决定视图的 `ViewContainer`（能否关闭承载它的容器）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginViewSlot {
    /// 当前窗口里最上层的对话框；插件请求关闭（`ctx.close()` / `fluxdown.close()`）即关闭它。
    Dialog,
}

/// 由 app 注入的插件视图工厂：`(插件, 视图声明, 任务 id, 承载位置, window, cx)` → 视图。
/// 扩展页的插件设置对话框与登录对话框用它渲染 `location == "settings"` / `"auth"` 的视图。
pub type PluginViewFactory = Rc<
    dyn Fn(
        &fluxdown_protocol::PluginDto,
        &fluxdown_protocol::PluginViewDeclDto,
        Option<String>,
        PluginViewSlot,
        &mut Window,
        &mut App,
    ) -> AnyView,
>;

pub trait ExtensionsPort: Send + Sync {
    fn call(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> PortFuture<serde_json::Value>;
}

/// 扩展分类的子页：插件、站点默认插件与受管组件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionsTab {
    Plugins,
    SiteDefaults,
    Components,
}

impl ExtensionsTab {
    const ALL: [Self; 3] = [Self::Plugins, Self::SiteDefaults, Self::Components];
}

pub struct ExtensionsView {
    translator: Entity<Translator>,
    controller: ExtensionsController,
    tab: ExtensionsTab,
    last_error: Option<String>,
    plugins: PluginsUi,
    components: [ComponentUi; 2],
    plugin_view_factory: Option<PluginViewFactory>,
    /// 打开中的插件对话框跟随的实时插件列表。
    live_plugins: Entity<LivePlugins>,
}

/// 需要宿主（app）完成的动作：capability 不直接开窗。
#[derive(Clone, Debug)]
pub enum ExtensionsEvent {
    /// 打开插件某个视图的桌面界面（`contributes.views[].desktop`，window 位置）。
    OpenPluginView {
        plugin: fluxdown_protocol::PluginDto,
        view: String,
    },
}

impl EventEmitter<ExtensionsEvent> for ExtensionsView {}

impl ExtensionsView {
    pub fn new(
        translator: Entity<Translator>,
        port: Arc<dyn ExtensionsPort>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        Self {
            translator,
            controller: ExtensionsController::new(port),
            tab: ExtensionsTab::Plugins,
            last_error: None,
            plugins: PluginsUi::default(),
            components: [ComponentUi::default(), ComponentUi::default()],
            plugin_view_factory: None,
            live_plugins: cx.new(|_| LivePlugins::default()),
        }
    }

    /// 把控制器的最新插件列表推给已打开的对话框；断连期间列表不可信，保持上一份已知状态。
    fn sync_live_plugins(&self, cx: &mut Context<Self>) {
        let stale = self.controller.is_stale();
        self.live_plugins.update(cx, |live, cx| {
            live.replace(self.controller.plugins(), stale, cx);
        });
    }

    /// 注入插件视图工厂（app 在装配时调用）。
    pub fn set_plugin_view_factory(&mut self, factory: PluginViewFactory, cx: &mut Context<Self>) {
        self.plugin_view_factory = Some(factory);
        cx.notify();
    }

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
        self.last_error = None;
        self.controller.replace_snapshot(snapshot);
        self.prune_awaiting_consent();
        self.invalidate_site_defaults();
        self.sync_live_plugins(cx);
        self.plugins
            .market
            .favorites
            .observe_session(snapshot.session.is_some());
        cx.notify();
    }

    pub fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
        let relevant = ExtensionsController::affects_view(event);
        match self.controller.apply_event(event) {
            Some(ExtensionsSignal::ComponentProgress {
                kind,
                downloaded_bytes,
                total_bytes,
            }) => {
                let ui = &mut self.components[component_slot(kind)];
                ui.installing = true;
                ui.downloaded_bytes = downloaded_bytes;
                ui.total_bytes = total_bytes;
            }
            Some(ExtensionsSignal::ComponentResult { kind, ok, message }) => {
                let ui = &mut self.components[component_slot(kind)];
                ui.last_result = Some((ok, message));
                // 非本视图发起的安装（如 Web UI）：结果到达即结束进度展示。
                if !ui.install_pending {
                    ui.installing = false;
                }
            }
            // 熔断提示由 app 层全局弹出；本视图只刷新列表（控制器已更新本地状态）。
            Some(ExtensionsSignal::PluginAutoDisabled { .. }) => self.sync_live_plugins(cx),
            None => {}
        }
        // 登录 / 登出以增量事件到达：刷新收藏（星标、「我的收藏」）无需重进设置页。
        let session_changed = match event {
            ServiceEvent::Agent(AgentEvent::SessionChanged(session)) => self
                .plugins
                .market
                .favorites
                .observe_session(session.is_some()),
            _ => false,
        };
        if ExtensionsController::replaces_plugins(event) {
            self.prune_awaiting_consent();
            self.invalidate_site_defaults();
            self.sync_live_plugins(cx);
        }
        let cleared_error = !self.controller.is_stale() && self.last_error.take().is_some();
        // 无关事件（如下载进度帧）不重绘，保住 retained 渲染的复用。
        if relevant || cleared_error || session_changed {
            cx.notify();
        }
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.last_error = Some(
            self.translator
                .read(cx)
                .text("localServiceDisconnected")
                .to_owned(),
        );
        self.controller.mark_stale();
        self.sync_live_plugins(cx);
        cx.notify();
    }

    pub(crate) fn show_tab(&mut self, tab: ExtensionsTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            // 每次进入站点默认子页都重新读取（下载过程中可能新增自动记录）。
            if tab == ExtensionsTab::SiteDefaults {
                self.invalidate_site_defaults();
            }
            cx.notify();
        }
    }

    pub(crate) fn toast_success(
        &self,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.push_notification(Notification::success(message), cx);
    }

    pub(crate) fn toast_error(&self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(Notification::error(message).autohide(false), cx);
    }
}

/// agent 端口不透传服务端 message：有细分原因时按原因给出可操作文案，否则按码映射通用文案。
pub(crate) fn error_text(translator: &Translator, error: &RpcErrorData) -> String {
    let reason_key = error.reason.and_then(|reason| match reason {
        ErrorReason::MarketUnreachable => Some("pluginErrorMarketUnreachable"),
        ErrorReason::MarketIndexInvalid => Some("pluginErrorMarketIndexInvalid"),
        ErrorReason::MarketIndexRollback => Some("pluginErrorMarketIndexRollback"),
        ErrorReason::PluginNotInMarket => Some("pluginErrorNotInMarket"),
        ErrorReason::PluginYanked => Some("pluginErrorYanked"),
        ErrorReason::PluginDownloadFailed => Some("pluginErrorDownloadFailed"),
        ErrorReason::PluginPackageTooLarge => Some("pluginErrorPackageTooLarge"),
        ErrorReason::PluginPackageInvalid => Some("pluginErrorPackageInvalid"),
        ErrorReason::MarketVersionChanged => Some("pluginErrorMarketVersionChanged"),
        // 账户 / 远程任务 / 局域网配对等其他能力的原因不在插件页展示，退回按码的通用文案。
        _ => None,
    });
    let key = reason_key.unwrap_or(match error.code {
        ApplicationErrorCode::Unavailable | ApplicationErrorCode::Timeout => {
            "localServiceDisconnected"
        }
        ApplicationErrorCode::InvalidArgument | ApplicationErrorCode::NotFound => {
            "localServiceInvalidArgument"
        }
        ApplicationErrorCode::Conflict => "localServiceConflict",
        ApplicationErrorCode::Unsupported => "settingsUnsupportedOnPlatform",
        ApplicationErrorCode::ProtocolIncompatible
        | ApplicationErrorCode::Unauthorized
        | ApplicationErrorCode::Cancelled
        | ApplicationErrorCode::Internal => "localServiceActionFailed",
    });
    translator.text(key).to_owned()
}

impl Render for ExtensionsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 站点默认子页只在有记录时出现：常驻读取（仅失效后重读），当前停留在该页时保留标签，
        // 避免移除最后一条记录时被弹走。
        self.ensure_site_defaults_loaded(window, cx);
        let show_site_defaults = !self.plugins.site_defaults.entries.is_empty()
            || self.tab == ExtensionsTab::SiteDefaults;
        let visible: Vec<ExtensionsTab> = ExtensionsTab::ALL
            .into_iter()
            .filter(|tab| *tab != ExtensionsTab::SiteDefaults || show_site_defaults)
            .collect();
        let tab = self.tab;
        let body = match tab {
            ExtensionsTab::Plugins => self.render_plugins(window, cx).into_any_element(),
            ExtensionsTab::SiteDefaults => self.render_site_defaults(window, cx).into_any_element(),
            ExtensionsTab::Components => self.render_components(window, cx).into_any_element(),
        };
        let translator = self.translator.read(cx);
        let labels: Vec<SharedString> = visible
            .iter()
            .map(|tab| {
                let key = match tab {
                    ExtensionsTab::Plugins => "settingsCatPlugins",
                    ExtensionsTab::SiteDefaults => "pluginSiteDefaultsTab",
                    ExtensionsTab::Components => "settingsCatComponents",
                };
                SharedString::from(translator.text(key).to_owned())
            })
            .collect();
        let selected = visible.iter().position(|t| *t == tab).unwrap_or(0);
        let tokens = active_theme(cx).tokens();
        let (spacing, typography, destructive) = (
            tokens.spacing,
            tokens.typography.xs,
            tokens.colors.destructive,
        );
        let view = cx.entity().downgrade();
        v_flex()
            .w_full()
            .gap(spacing.md)
            .when_some(self.last_error.clone(), |this, error| {
                this.child(
                    div()
                        .text_size(typography.size)
                        .line_height(typography.line_height)
                        .text_color(destructive)
                        .child(error),
                )
            })
            // 标签条左对齐、保持自然宽度（外层行吸收纵向容器的横向拉伸）。
            .child(h_flex().w_full().child(segmented_tabs(
                "extensions-tabs",
                labels,
                selected,
                move |index, _, cx| {
                    if let Some(tab) = visible.get(index) {
                        let Ok(()) = view.update(cx, |this, cx| this.show_tab(*tab, cx)) else {
                            // 扩展视图已释放，结束回调，不再更新状态。
                            return;
                        };
                    }
                },
                cx,
            )))
            .child(body)
    }
}

/// 组件 UI 状态数组的固定顺序与 [`COMPONENT_KINDS`] 对齐。
const _: () = assert!(COMPONENT_KINDS.len() == 2);
