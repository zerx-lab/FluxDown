//! 插件贡献点装配：app 是唯一 composition root，这里把 daemon 推来的插件列表翻译成各界面的注入。
//!
//! - 主窗口 shell：活动栏插件页（`ShellView::set_dynamic_routes`，视图懒创建）；
//! - 下载页：任务右键命令与详情 tab；独立任务详情窗口：详情 tab；
//! - 设置窗口：插件设置视图工厂；命令面板：`palette:true` 的插件命令；
//! - 事件：可移植视图帧 / 会话结束 / 插件 toast，以及连接恢复后的会话重同步。
//!
//! 贡献只在「插件列表推导出的贡献实际变化」时重新应用，进度帧等无关事件不会触发重建。

use std::{collections::HashMap, rc::Rc, sync::Arc};

use fluxdown_protocol::{AgentEvent, DaemonEvent, RpcErrorData, ServiceEvent, method};
use fluxdown_ui_command_palette::{PaletteItem, PaletteItemKind};
use fluxdown_ui_components::FluxIcon;
use fluxdown_ui_downloads::{
    DownloadView, PluginCommandRunner, PluginTabViewFactory, PluginTaskCommand, PluginTaskTab,
    TaskDetailView,
};
use fluxdown_ui_extensions::{PluginViewFactory, PluginViewSlot};
use fluxdown_ui_plugin_ui::{
    ViewContainer,
    contrib::{CommandRef, Contributions, ViewRef, view_for},
};
use fluxdown_ui_shell::{RouteId, ShellRoute};
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, Global, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, WeakEntity, Window, div,
};
use gpui_component::{Icon, WindowExt as _, notification::Notification};
use serde_json::{Value, json};

use crate::{
    activity::{DOWNLOADS_ROUTE, RSS_ROUTE, WEBHOOKS_ROUTE},
    app::Desktop,
    capability_ports::AgentPluginUiPort,
    session::{SessionSignal, agent_body},
    windows::WindowRegistry,
};

/// 进程级状态：当前贡献、已创建的活动栏页实体、存活的独立任务详情视图。
#[derive(Default)]
struct PluginContributionState {
    current: Contributions,
    /// 路由 id → (声明, 实体)；同 id 同声明的路由在重算时复用，不重建会话。
    routes: HashMap<String, (ViewRef, Entity<PluginRouteView>)>,
    detail_views: Vec<WeakEntity<TaskDetailView>>,
}

impl Global for PluginContributionState {}

// ---------------------------------------------------------------- 纯函数

/// 一条活动栏插件页。
#[derive(Debug, Clone, PartialEq)]
struct RouteSpec {
    id: String,
    label: String,
    view: ViewRef,
}

fn route_specs(contributions: &Contributions) -> Vec<RouteSpec> {
    contributions
        .activity
        .iter()
        .map(|view| RouteSpec {
            id: RouteId::plugin(&view.identity, &view.view.id)
                .as_str()
                .to_owned(),
            label: view.view.title.clone(),
            view: view.clone(),
        })
        .collect()
}

/// 命令面板条目：`(使用记录键, 标题, identity, 命令 id)`。
fn palette_specs(contributions: &Contributions) -> Vec<(String, String, String, String)> {
    contributions
        .palette
        .iter()
        .map(|command| {
            (
                format!("plugin:{}:{}", command.identity, command.command.id),
                format!("{}: {}", command.plugin_name, command.command.title),
                command.identity.clone(),
                command.command.id.clone(),
            )
        })
        .collect()
}

fn task_tabs(contributions: &Contributions) -> Vec<PluginTaskTab> {
    contributions
        .task_tabs
        .iter()
        .map(|view| PluginTaskTab {
            identity: view.identity.clone(),
            view: view.view.id.clone(),
            title: SharedString::from(view.view.title.clone()),
            revision: view.package_revision,
        })
        .collect()
}

fn task_commands(contributions: &Contributions) -> Vec<PluginTaskCommand> {
    contributions
        .task_menu
        .iter()
        .map(
            |CommandRef {
                 identity, command, ..
             }| PluginTaskCommand {
                identity: identity.clone(),
                command: command.id.clone(),
                title: SharedString::from(command.title.clone()),
            },
        )
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

fn toast_kind(level: &str) -> ToastKind {
    match level {
        "success" => ToastKind::Success,
        "warning" => ToastKind::Warning,
        "error" => ToastKind::Error,
        _ => ToastKind::Info,
    }
}

/// 新插件列表推导出的贡献与当前不同时返回 `Some(新贡献)`。
fn changed_contributions(
    current: &Contributions,
    plugins: &[fluxdown_protocol::PluginDto],
) -> Option<Contributions> {
    let next = Contributions::from_plugins(plugins);
    (next != *current).then_some(next)
}

// ---------------------------------------------------------------- 懒加载路由视图

/// 活动栏插件页：路由被激活（或首次渲染）时才创建内容视图并缓存。
struct PluginRouteView {
    view: ViewRef,
    inner: Option<AnyView>,
}

impl PluginRouteView {
    fn ensure_inner(&mut self, window: &mut Window, cx: &mut App) {
        if self.inner.is_none() {
            self.inner = Some(view_for(
                &self.view,
                None,
                ViewContainer::default(),
                window,
                cx,
            ));
        }
    }
}

impl Render for PluginRouteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_inner(window, cx);
        div().size_full().children(self.inner.clone())
    }
}

/// 占位空视图（贡献已消失时的详情 tab）。
struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

// ---------------------------------------------------------------- 装配入口

/// 应用启动时安装：可移植视图运行环境、会话订阅、全局状态。须先于任何窗口创建。
pub(crate) fn install(cx: &mut App) {
    let (client, translator, session) = {
        let desktop = Desktop::global(cx);
        (
            desktop.client.clone(),
            desktop.translator.clone(),
            desktop.session.clone(),
        )
    };
    fluxdown_ui_plugin_ui::portable::install(
        cx,
        Arc::new(AgentPluginUiPort::new(client)),
        translator,
    );
    let mut state = PluginContributionState::default();
    if let Some(body) = session.read(cx).agent_snapshot() {
        state.current = Contributions::from_plugins(&body.daemon.plugins);
    }
    cx.set_global(state);
    fluxdown_ui_plugin_ui::set_navigator(cx, Rc::new(navigate_for_plugin));
    cx.subscribe(&session, |_, signal, cx| match signal {
        SessionSignal::Snapshot(snapshot) => {
            if let Some(body) = agent_body(snapshot) {
                plugins_updated(&body.daemon.plugins, cx);
            }
            fluxdown_ui_plugin_ui::portable::resync_all(cx);
        }
        SessionSignal::Event(frame) => handle_event(&frame.event, cx),
        SessionSignal::Stale
        | SessionSignal::Fatal(_)
        | SessionSignal::ServiceStopped
        | SessionSignal::ServiceFailed(_) => {}
    })
    .detach();
}

fn handle_event(event: &ServiceEvent, cx: &mut App) {
    let daemon = match event {
        ServiceEvent::Daemon(event) | ServiceEvent::Agent(AgentEvent::Daemon(event)) => event,
        ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(snapshot)) => {
            plugins_updated(&snapshot.plugins, cx);
            fluxdown_ui_plugin_ui::portable::resync_all(cx);
            return;
        }
        ServiceEvent::Agent(_) => return,
    };
    match daemon {
        DaemonEvent::PluginsChanged(plugins) => plugins_updated(plugins, cx),
        DaemonEvent::SnapshotReplaced(snapshot) => {
            plugins_updated(&snapshot.plugins, cx);
            fluxdown_ui_plugin_ui::portable::resync_all(cx);
        }
        DaemonEvent::PluginViewFrame(frame) => {
            fluxdown_ui_plugin_ui::portable::dispatch_frame(frame.clone(), cx);
        }
        DaemonEvent::PluginViewClosed { session, reason } => {
            fluxdown_ui_plugin_ui::portable::dispatch_closed(session, reason, cx);
        }
        DaemonEvent::PluginToast { level, text, .. } => show_toast(level, text, cx),
        _ => {}
    }
}

/// 插件列表到达：已开插件窗口按包修订号原地重挂；贡献（含包修订号）变化才重新装配路由与 tab。
fn plugins_updated(plugins: &[fluxdown_protocol::PluginDto], cx: &mut App) {
    crate::windows::plugin::remount_changed(plugins, cx);
    let Some(next) =
        changed_contributions(&cx.global::<PluginContributionState>().current, plugins)
    else {
        return;
    };
    cx.global_mut::<PluginContributionState>().current = next;
    apply_main_window(cx);
    apply_detail_windows(cx);
}

/// 主窗口创建后调用：清掉旧窗口的页缓存并按当前贡献装配。
pub(crate) fn attach_main_window(cx: &mut App) {
    cx.global_mut::<PluginContributionState>().routes.clear();
    apply_main_window(cx);
}

/// 独立任务详情窗口创建后调用：立即注入当前 tab，并登记后续变化。
pub(crate) fn attach_detail_window(detail: &Entity<TaskDetailView>, cx: &mut App) {
    let tabs = task_tabs(&cx.global::<PluginContributionState>().current);
    detail.update(cx, |detail, cx| detail.set_plugin_tabs(tabs, cx));
    cx.global_mut::<PluginContributionState>()
        .detail_views
        .push(detail.downgrade());
}

fn apply_main_window(cx: &mut App) {
    let desktop = Desktop::global(cx);
    let shell = desktop.main_shell.as_ref().and_then(WeakEntity::upgrade);
    let downloads = desktop
        .main_downloads
        .as_ref()
        .and_then(WeakEntity::upgrade);
    if let Some(shell) = shell {
        let routes = build_routes(cx);
        shell.update(cx, |shell, cx| shell.set_dynamic_routes(routes, cx));
    }
    if let Some(downloads) = downloads {
        let current = &cx.global::<PluginContributionState>().current;
        let (tabs, commands) = (task_tabs(current), task_commands(current));
        downloads.update(cx, |downloads: &mut DownloadView, cx| {
            downloads.set_plugin_contributions(tabs, commands, cx);
        });
    }
}

fn apply_detail_windows(cx: &mut App) {
    let state = cx.global_mut::<PluginContributionState>();
    state.detail_views.retain(|view| view.upgrade().is_some());
    let views: Vec<_> = state
        .detail_views
        .iter()
        .filter_map(WeakEntity::upgrade)
        .collect();
    let tabs = task_tabs(&state.current);
    for view in views {
        let tabs = tabs.clone();
        view.update(cx, |detail, cx| detail.set_plugin_tabs(tabs, cx));
    }
}

fn build_routes(cx: &mut App) -> Vec<ShellRoute> {
    let (specs, mut cache) = {
        let state = cx.global_mut::<PluginContributionState>();
        (
            route_specs(&state.current),
            std::mem::take(&mut state.routes),
        )
    };
    let mut next_cache = HashMap::with_capacity(specs.len());
    let mut routes = Vec::with_capacity(specs.len());
    for spec in specs {
        let entity = match cache.remove(&spec.id) {
            Some((view, entity)) if view == spec.view => entity,
            _ => {
                let view = spec.view.clone();
                cx.new(|_| PluginRouteView { view, inner: None })
            }
        };
        let weak = entity.downgrade();
        let icon = match &spec.view.view.icon_svg {
            Some(svg) => Icon::default().data(svg.as_bytes()),
            None => Icon::new(FluxIcon::Sparkles),
        };
        let route = ShellRoute::plugin(
            RouteId::plugin(&spec.view.identity, &spec.view.view.id),
            SharedString::from(spec.label),
            icon,
            entity.clone().into(),
        )
        .on_activate(Rc::new(move |window, cx| {
            if let Some(entity) = weak.upgrade() {
                entity.update(cx, |route, cx| {
                    route.ensure_inner(window, cx);
                    cx.notify();
                });
            }
        }));
        routes.push(route);
        next_cache.insert(spec.id, (spec.view, entity));
    }
    cx.global_mut::<PluginContributionState>().routes = next_cache;
    routes
}

// ---------------------------------------------------------------- 插件跳转

/// 插件请求的路由 id → 主窗口路由。内置页恒可用；`plugin:` 路由须是发起方插件当前的活动栏视图。
fn resolve_route(
    identity: &str,
    route: &str,
    contributions: &Contributions,
) -> Result<RouteId, String> {
    match route {
        "downloads" => return Ok(DOWNLOADS_ROUTE),
        "rss" => return Ok(RSS_ROUTE),
        "webhooks" => return Ok(WEBHOOKS_ROUTE),
        _ => {}
    }
    let Some(view) = route.strip_prefix(&format!("plugin:{identity}:")) else {
        return Err(format!("unknown navigation target '{route}'"));
    };
    contributions
        .activity
        .iter()
        .any(|candidate| candidate.identity == identity && candidate.view.id == view)
        .then(|| RouteId::plugin(identity, view))
        .ok_or_else(|| format!("view '{view}' is not an activity view of this plugin"))
}

/// 安装给 plugin_ui 的跳转器：同步校验目标存在，随后 defer 到安全时机把主窗口带到前台并切页
/// （调用方可能正处于任意窗口的 update 栈内）。
fn navigate_for_plugin(identity: &str, route: &str, cx: &mut App) -> Result<(), String> {
    let target = resolve_route(
        identity,
        route,
        &cx.global::<PluginContributionState>().current,
    )?;
    cx.defer(move |cx| crate::command_palette::navigate_now(cx, target));
    Ok(())
}

// ---------------------------------------------------------------- 下载页 / 设置 / 面板

/// 下载页任务右键命令的执行回调。
pub(crate) fn command_runner() -> PluginCommandRunner {
    Rc::new(|identity, command, task_ids, _window, cx| {
        run_command(identity, command, task_ids, cx);
    })
}

/// 下载页 / 任务详情窗口的插件 tab 视图工厂：按 tab 的 `(identity, 视图 id)` 取当前贡献，
/// 贡献已消失时给占位空视图。
pub(crate) fn tab_view_factory() -> PluginTabViewFactory {
    Rc::new(|tab, task_id, window, cx| {
        let view = cx
            .try_global::<PluginContributionState>()
            .and_then(|state| {
                state
                    .current
                    .task_tabs
                    .iter()
                    .find(|view| view.identity == tab.identity && view.view.id == tab.view)
                    .cloned()
            });
        match view {
            Some(view) => view_for(&view, Some(task_id), ViewContainer::default(), window, cx),
            None => cx.new(|_| Blank).into(),
        }
    })
}

/// 扩展页对话框（插件设置 / 登录）里的插件视图工厂：承载位置决定视图的容器能力。
pub(crate) fn dialog_view_factory() -> PluginViewFactory {
    Rc::new(|plugin, view, task_id, slot, window, cx| {
        let view = ViewRef {
            identity: plugin.identity.clone(),
            plugin_name: plugin.name.clone(),
            view: view.clone(),
            package_dir: plugin.package_dir.clone(),
            package_revision: plugin.package_revision,
        };
        let container = match slot {
            PluginViewSlot::Dialog => ViewContainer::dialog(window),
        };
        view_for(&view, task_id, container, window, cx)
    })
}

/// 命令面板里的插件命令；taskIds 取主窗口下载页当前选中的本机任务。
pub(crate) fn palette_items(cx: &App) -> Vec<PaletteItem> {
    let Some(state) = cx.try_global::<PluginContributionState>() else {
        return Vec::new();
    };
    palette_specs(&state.current)
        .into_iter()
        .map(|(id, title, identity, command)| {
            PaletteItem::new(PaletteItemKind::Command, id, title, move |_, cx| {
                let task_ids = selected_task_ids(cx);
                run_command(identity.clone(), command.clone(), task_ids, cx);
            })
            .icon(Icon::new(FluxIcon::Sparkles))
        })
        .collect()
}

fn selected_task_ids(cx: &App) -> Vec<String> {
    Desktop::global(cx)
        .main_downloads
        .as_ref()
        .and_then(WeakEntity::upgrade)
        .map(|downloads| downloads.read(cx).selected_local_task_ids(cx))
        .unwrap_or_default()
}

/// 调 `daemon.plugin.command`；失败在浮层窗口弹 error 通知。
fn run_command(identity: String, command: String, task_ids: Vec<String>, cx: &mut App) {
    let future = Desktop::global(cx).client.call::<Value, Value>(
        method::DAEMON_PLUGIN_COMMAND,
        Some(json!({ "identity": identity, "command": command, "taskIds": task_ids })),
    );
    cx.spawn(async move |cx| {
        if let Err(error) = future.await {
            cx.update(|cx| command_failed(&error, cx));
        }
    })
    .detach();
}

fn command_failed(error: &RpcErrorData, cx: &mut App) {
    let message = format!("{:?}", error.code);
    let text = Desktop::global(cx)
        .translator
        .read(cx)
        .text_with("pluginCommandFailed", &[("message", &message)]);
    push_notification(Notification::error(text), cx);
}

fn show_toast(level: &str, text: &str, cx: &mut App) {
    let text = text.to_owned();
    let note = match toast_kind(level) {
        ToastKind::Info => Notification::info(text),
        ToastKind::Success => Notification::success(text),
        ToastKind::Warning => Notification::warning(text),
        ToastKind::Error => Notification::error(text),
    };
    push_notification(note, cx);
}

fn push_notification(note: Notification, cx: &mut App) {
    let Some(handle) = WindowRegistry::overlay_window(cx) else {
        log::debug!("plugin notification dropped: no overlay window");
        return;
    };
    if let Err(error) = handle.update(cx, |_, window, cx| window.push_notification(note, cx)) {
        log::debug!("overlay window released before plugin notification: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn plugin(identity: &str, extra: Value) -> fluxdown_protocol::PluginDto {
        let mut base = json!({
            "identity": identity,
            "name": "Demo",
            "version": "1.0.0",
            "enabled": true,
            "devMode": false,
            "disabledReason": "",
            "settings": [],
            "settingsValues": {},
            "permissions": [],
            "authSupported": false,
            "subscriptionProviderIds": [],
            "resolverUrls": [],
            "loadStatus": "Loaded",
            "loadError": "",
            "contributions": {
                "views": [
                    {"id": "a", "title": "Dash", "location": "activity", "portable": true},
                    {"id": "t", "title": "Tab", "location": "taskDetail", "portable": true}
                ],
                "commands": [
                    {"id": "both", "title": "Both", "palette": true, "taskMenu": true},
                    {"id": "menu", "title": "Menu", "palette": false, "taskMenu": true}
                ]
            },
            "risk": "standard",
            "consentPending": [],
            "consentPendingHosts": [],
            "networkHosts": [],
            "packageDir": "/tmp/p"
        });
        if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
        serde_json::from_value(base).expect("plugin dto")
    }

    #[test]
    fn derives_routes_palette_tabs_and_menu() {
        let contributions = Contributions::from_plugins(&[plugin("d@x", json!({}))]);
        let routes = route_specs(&contributions);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].id, "plugin:d@x:a");
        assert_eq!(routes[0].label, "Dash");
        assert_eq!(
            palette_specs(&contributions),
            [(
                "plugin:d@x:both".to_owned(),
                "Demo: Both".to_owned(),
                "d@x".to_owned(),
                "both".to_owned()
            )]
        );
        assert_eq!(task_tabs(&contributions).len(), 1);
        let commands = task_commands(&contributions);
        assert_eq!(
            commands
                .iter()
                .map(|c| c.command.as_str())
                .collect::<Vec<_>>(),
            ["both", "menu"]
        );
    }

    #[test]
    fn only_reports_real_contribution_changes() {
        let plugins = [plugin("d@x", json!({}))];
        let current = Contributions::from_plugins(&plugins);
        // 与进度无关的字段（版本）变化不改变贡献。
        let bumped = [plugin("d@x", json!({"version": "1.0.1"}))];
        assert!(changed_contributions(&current, &bumped).is_none());
        let disabled = [plugin("d@x", json!({"enabled": false}))];
        let next = changed_contributions(&current, &disabled).expect("changed");
        assert!(next.activity.is_empty());
        assert!(changed_contributions(&next, &disabled).is_none());
    }

    #[test]
    fn package_revision_change_invalidates_cached_views() {
        let current = Contributions::from_plugins(&[
            plugin("d@x", json!({"packageRevision": 1})),
            plugin("e@x", json!({"packageRevision": 1})),
        ]);
        let reloaded = [
            plugin("d@x", json!({"packageRevision": 2})),
            plugin("e@x", json!({"packageRevision": 1})),
        ];
        let next = changed_contributions(&current, &reloaded).expect("revision change applies");

        // build_routes 按 (id, ViewRef) 复用路由实体：只有被重载插件的声明不再相等。
        let before = route_specs(&current);
        let after = route_specs(&next);
        let same = |id: &str| {
            let find = |specs: &[RouteSpec]| specs.iter().find(|s| s.id == id).cloned();
            find(&before) == find(&after)
        };
        assert!(!same("plugin:d@x:a"));
        assert!(same("plugin:e@x:a"));
        // 详情 tab 列表随之不等，详情面板据此丢弃旧视图缓存。
        assert_ne!(task_tabs(&current), task_tabs(&next));
    }

    #[test]
    fn task_tabs_from_different_plugins_never_compare_equal() {
        // 同标题、同修订号的 tab 换了插件：详情面板必须作废缓存，不能把旧插件的视图当新的用。
        let first = task_tabs(&Contributions::from_plugins(&[plugin("d@x", json!({}))]));
        let second = task_tabs(&Contributions::from_plugins(&[plugin("e@x", json!({}))]));
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].title, second[0].title);
        assert_eq!(first[0].revision, second[0].revision);
        assert_ne!(first, second);
        assert_eq!(
            (first[0].identity.as_str(), first[0].view.as_str()),
            ("d@x", "t")
        );
    }

    #[test]
    fn resolves_navigation_targets_against_the_current_contributions() {
        let contributions = Contributions::from_plugins(&[plugin("d@x", json!({}))]);
        for (id, expected) in [
            ("downloads", DOWNLOADS_ROUTE),
            ("rss", RSS_ROUTE),
            ("webhooks", WEBHOOKS_ROUTE),
        ] {
            assert_eq!(resolve_route("d@x", id, &contributions), Ok(expected));
        }
        assert_eq!(
            resolve_route("d@x", "plugin:d@x:a", &contributions),
            Ok(RouteId::plugin("d@x", "a"))
        );
        // 只有活动栏视图可跳转：任务详情 tab、不存在的视图、别的插件的页面、乱写的 id 都拒绝。
        assert_eq!(
            resolve_route("d@x", "plugin:d@x:t", &contributions),
            Err("view 't' is not an activity view of this plugin".to_owned())
        );
        assert!(resolve_route("d@x", "plugin:d@x:nope", &contributions).is_err());
        assert!(resolve_route("other@x", "plugin:d@x:a", &contributions).is_err());
        assert!(resolve_route("other@x", "plugin:other@x:a", &contributions).is_err());
        assert!(resolve_route("d@x", "settings", &contributions).is_err());
    }

    #[test]
    fn maps_toast_levels() {
        assert_eq!(toast_kind("success"), ToastKind::Success);
        assert_eq!(toast_kind("warning"), ToastKind::Warning);
        assert_eq!(toast_kind("error"), ToastKind::Error);
        assert_eq!(toast_kind("info"), ToastKind::Info);
        assert_eq!(toast_kind("weird"), ToastKind::Info);
    }
}
