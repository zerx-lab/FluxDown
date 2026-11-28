//! 插件桌面界面窗口：contributes.views 中 `location == window` 且带 `desktop` 的脚本，
//! 每个视图一个窗口。
//!
//! 层级：`Root`（FluxDown 浮层）→ `ShellRoot`（脚本的对话框 / sheet / toast）→
//! `AuxiliaryWindowView`（FluxDown 标题栏）→ [`PluginWindowContent`] → 脚本视图。尺寸取
//! manifest，未声明时用默认值。插件包修订号变化（安装 / 升级 / dev 重载）时，已开窗口在原地
//! 重新挂载脚本视图（见 [`remount_changed`]），窗口本身不关。

use std::collections::HashMap;

use fluxdown_protocol::PluginDto;
use fluxdown_ui_plugin_ui::{PluginWindowSpec, ViewContainer};
use fluxdown_ui_shell::{AuxiliaryWindowView, auxiliary_window_options};
use gpui::{
    AnyView, App, AppContext as _, Context, Global, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, WeakEntity, Window, WindowBounds, div, px, size,
};
use gpui_component::Root;

use crate::{
    app::Desktop,
    windows::{WindowKey, WindowRegistry},
};

const DEFAULT_WIDTH: f32 = 960.;
const DEFAULT_HEIGHT: f32 = 640.;
/// 插件可声明的最小尺寸下限（与引擎 manifest 校验 `[240, 4096]` 一致）。
const MIN_EDGE: f32 = 240.;

/// 窗口内可替换的脚本视图槽位。
struct PluginWindowContent {
    view: AnyView,
}

impl Render for PluginWindowContent {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.view.clone())
    }
}

/// 已开插件窗口：挂载时的包修订号与内容槽位。
struct OpenPluginWindow {
    identity: String,
    view_id: String,
    revision: u64,
    content: WeakEntity<PluginWindowContent>,
}

#[derive(Default)]
struct PluginWindows(HashMap<String, OpenPluginWindow>);

impl Global for PluginWindows {}

fn load_content(spec: &PluginWindowSpec, window: &mut Window, cx: &mut App) -> AnyView {
    // 脚本经 `fluxdown.close()` 关闭的是整个插件窗口。
    let container = ViewContainer::window(window);
    match fluxdown_ui_plugin_ui::load_desktop_view(spec, container, window, cx) {
        Ok(view) => view.into(),
        Err(error) => fluxdown_ui_plugin_ui::load_failure_view(&error, cx),
    }
}

fn window_spec(plugin: &PluginDto, view_id: &str) -> Option<PluginWindowSpec> {
    plugin
        .contributions
        .views
        .iter()
        .find(|view| view.id == view_id)
        .and_then(|view| PluginWindowSpec::from_view(plugin, view))
}

/// 打开（或聚焦）插件某个 window 视图的桌面界面。视图没有可用桌面入口时什么也不做。
pub fn open(cx: &mut App, plugin: &PluginDto, view_id: &str) {
    let Some(spec) = window_spec(plugin, view_id) else {
        log::warn!(
            "plugin {} has no loadable desktop interface",
            plugin.identity
        );
        return;
    };
    let desktop = Desktop::global(cx);
    let translator = desktop.translator.clone();
    let title = SharedString::from(format!("{}: {}", spec.name, spec.view_title));

    let width = spec.entry.width.unwrap_or(DEFAULT_WIDTH);
    let height = spec.entry.height.unwrap_or(DEFAULT_HEIGHT);
    let window_size = size(px(width), px(height));
    let mut options = auxiliary_window_options(title.clone());
    options.window_min_size = Some(size(px(width.min(MIN_EDGE)), px(height.min(MIN_EDGE))));
    let (display_id, bounds) = WindowRegistry::place_following_main(window_size, None, cx);
    options.display_id = display_id;
    options.window_bounds = Some(WindowBounds::Windowed(bounds));
    options.is_resizable = true;
    options.is_minimizable = true;

    let key_id = format!("{}/{}", spec.identity, spec.view_id);
    let revision = plugin.package_revision;
    let tracked_key = key_id.clone();
    WindowRegistry::open_or_focus(cx, WindowKey::Plugin(key_id), options, move |window, cx| {
        let view = load_content(&spec, window, cx);
        let content = cx.new(|_| PluginWindowContent { view });
        cx.default_global::<PluginWindows>().0.insert(
            tracked_key,
            OpenPluginWindow {
                identity: spec.identity.clone(),
                view_id: spec.view_id.clone(),
                revision,
                content: content.downgrade(),
            },
        );
        let chrome = cx.new(|cx| {
            let mut chrome = AuxiliaryWindowView::new(
                translator.clone(),
                "pluginWindowTitle",
                content.into(),
                cx,
            );
            chrome.set_title(Some(title.clone()), cx);
            chrome
        });
        window.set_window_title(&title);
        // `ShellRoot::find` 只认窗口根自身或 `Root` 的直接内层：脚本的对话框 / sheet / toast
        // 才能找到宿主，故 `ShellRoot` 必须紧贴在 `Root` 下、包住 FluxDown 标题栏。
        let shell_root = fluxdown_ui_plugin_ui::wrap_shell_root(chrome.into(), window, cx);
        cx.new(|cx| Root::new(shell_root, window, cx))
    });
}

/// 插件列表到达：包修订号变化的已开窗口原地重新挂载脚本视图；已关窗口的登记一并清理。
///
/// 插件已不可加载（卸载 / 禁用 / 失败）时保留旧视图，不在这里关窗。
pub(crate) fn remount_changed(plugins: &[PluginDto], cx: &mut App) {
    let Some(tracked) = cx.try_global::<PluginWindows>() else {
        return;
    };
    let mut stale = Vec::new();
    let mut remount = Vec::new();
    for (key, open) in &tracked.0 {
        let Some(content) = open.content.upgrade() else {
            stale.push(key.clone());
            continue;
        };
        let Some(plugin) = plugins
            .iter()
            .find(|plugin| plugin.identity == open.identity)
        else {
            continue;
        };
        if plugin.package_revision == open.revision {
            continue;
        }
        if let Some(spec) = window_spec(plugin, &open.view_id) {
            remount.push((key.clone(), plugin.package_revision, spec, content));
        }
    }
    let tracked = cx.global_mut::<PluginWindows>();
    for key in stale {
        tracked.0.remove(&key);
    }
    for (key, revision, _, _) in &remount {
        if let Some(open) = tracked.0.get_mut(key) {
            open.revision = *revision;
        }
    }
    for (key, _, spec, content) in remount {
        let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Plugin(key)) else {
            continue;
        };
        let result = handle.update(cx, |_, window, cx| {
            let view = load_content(&spec, window, cx);
            content.update(cx, |content, cx| {
                content.view = view;
                cx.notify();
            });
        });
        if let Err(error) = result {
            log::debug!("plugin window released before remount: {error:#}");
        }
    }
}
