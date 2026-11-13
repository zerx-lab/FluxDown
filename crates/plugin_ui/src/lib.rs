//! 插件界面 capability，两层：
//! - [`portable`]：跨端可移植视图。daemon 常驻宿主（manifest `main`）产出数据树，经
//!   `PluginViewFrame` 帧同步到这里，按 `native/protocol/src/plugin_view_schema.json` 的节点表
//!   原生渲染；Web 有同构实现。
//! - 桌面脚本（`desktop`）：用 gpui-kit 的 GPUI Shell 在本进程运行 `contributes.views[].desktop`
//!   的 ES module（默认导出继承 `View` 的类，可 import `gpui-kit` / `gpui-base` /
//!   `gpui-component` 与本 crate 注册的 `fluxdown` 模块），只在 GPUI 桌面端存在。
//!
//! 边界：
//! - 插件后台逻辑、hook 与常驻宿主始终在 daemon；[`contrib`] 只把已加载、已启用、无待授权的
//!   插件声明整理成贡献点，装配由 app 完成。
//! - 桌面脚本跑在 UI 主线程（gpui-shell 的 VM 不可跨线程），宿主按 gpui-shell 的中断预算
//!   掐断失控的 `render` / 事件处理。
//! - 脚本经 `fluxdown.call(method, params)` 访问 agent `/rpc`，与原生移动端的通用通道
//!   一样只放行 `daemon.*` / `agent.*`。

mod audit;
mod container;
pub mod contrib;
mod desktop;
mod grant;
mod host_value;
pub mod portable;
#[cfg(test)]
mod tests;

pub use container::{CloseHandler, Navigator, ViewContainer, navigate, set_navigator};
pub use desktop::{desktop_view, load_desktop_view, load_failure_view};

/// 为独立窗口包一层脚本浮层宿主 `ShellRoot`。窗口根须是 `Root(ShellRoot(…))`
/// （`ShellRoot::find` 只认根自身或 `Root` 的直接内层），app 以此包住标题栏与脚本视图。
pub fn wrap_shell_root(
    content: gpui::AnyView,
    window: &mut gpui::Window,
    cx: &mut App,
) -> gpui::AnyView {
    use gpui::AppContext as _;
    cx.new(|cx| gpui_shell::ShellRoot::new(content, window, cx))
        .into()
}

use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    rc::Rc,
    sync::Arc,
};

use fluxdown_protocol::{PluginDesktopEntryDto, PluginViewDeclDto, RpcErrorData};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::{FluxThemeState, active_theme};
use gpui::{App, Entity, Global, WeakEntity};
use gpui_shell::{FrozenComponentRegistry, HostModule, ScriptView};

pub type PortFuture<T> = Pin<Box<dyn Future<Output = Result<T, RpcErrorData>> + Send + 'static>>;

/// 插件界面访问 agent 的端口（由 app 注入统一 agent client）。
pub trait PluginUiPort: Send + Sync {
    fn call(&self, method: String, params: serde_json::Value) -> PortFuture<serde_json::Value>;
}

#[derive(Debug, thiserror::Error)]
pub enum PluginUiError {
    #[error("plugin UI component catalog failed: {0}")]
    Catalog(String),
    #[error("plugin UI runtime failed: {0}")]
    Runtime(String),
    #[error("plugin `{identity}` has no desktop interface")]
    NoWindow { identity: String },
    #[error("plugin `{identity}` interface failed to load: {message}")]
    Load { identity: String, message: String },
}

/// 一个待打开的插件桌面界面（来自 `PluginDto`：本机 daemon 的包目录 + 某个 view 的 `desktop` 入口）。
#[derive(Clone, Debug, PartialEq)]
pub struct PluginWindowSpec {
    pub identity: String,
    pub name: String,
    pub view_id: String,
    pub view_title: String,
    pub package_dir: PathBuf,
    pub entry: PluginDesktopEntryDto,
}

impl PluginWindowSpec {
    /// 只有已加载、该 view 带 `desktop` 入口且给出包目录的插件才可打开桌面界面。
    pub fn from_view(
        plugin: &fluxdown_protocol::PluginDto,
        view: &PluginViewDeclDto,
    ) -> Option<Self> {
        if plugin.load_status == "Failed" {
            return None;
        }
        Some(Self {
            identity: plugin.identity.clone(),
            name: plugin.name.clone(),
            view_id: view.id.clone(),
            view_title: view.title.clone(),
            package_dir: PathBuf::from(plugin.package_dir.as_ref()?),
            entry: view.desktop.clone()?,
        })
    }
}

/// 进程级 gpui-shell 宿主（UI 线程独占）。
///
/// QuickJS VM（含 JIT）在首次打开插件界面时才创建：没有插件界面的会话不付这份内存。
pub struct PluginScriptHost {
    pub(crate) components: FrozenComponentRegistry,
    /// 每插件独立的 runtime（首个视图加载时创建，视图全部释放后丢弃）。
    pub(crate) runtimes: HashMap<String, desktop::RuntimeEntry>,
    pub(crate) port: Arc<dyn PluginUiPort>,
    pub(crate) data_root: PathBuf,
    /// 存活的脚本视图（主题变化时刷新）。
    pub(crate) views: Vec<WeakEntity<ScriptView>>,
    pub(crate) audit: audit::AuditBuffer,
    /// 每次 FluxDown 主题变化加一（`fluxdown.theme().revision`）。
    pub(crate) theme_revision: u32,
    /// 界面语言来源（`fluxdown.locale()`）；未设置时用系统语言。
    pub(crate) translator: Option<Entity<Translator>>,
    /// 主题变化时额外通知的监听者（如可移植视图宿主）。
    pub(crate) theme_listeners: Vec<ThemeListener>,
}

/// 主题变化监听者。
pub type ThemeListener = Rc<dyn Fn(&mut App)>;

impl Global for PluginScriptHost {}

/// 启动期初始化。**替代** app 原先的 `gpui_component::init` 调用、并须先于主题初始化：
/// gpui-shell 的组件目录初始化内部调用 `gpui_component::init`（会重置主题全局）。
///
/// 无论成败都完成 `gpui_component` 初始化；返回错误只表示插件界面不可用，桌面端照常运行。
pub fn init(
    cx: &mut App,
    port: Arc<dyn PluginUiPort>,
    data_root: PathBuf,
) -> Result<(), PluginUiError> {
    let components = match catalog() {
        Ok(components) => components,
        Err(error) => {
            gpui_component::init(cx);
            return Err(error);
        }
    };
    gpui_shell::init_with_components(cx, &components);
    cx.set_global(PluginScriptHost {
        components,
        runtimes: HashMap::new(),
        port,
        data_root,
        views: Vec::new(),
        audit: audit::AuditBuffer::default(),
        theme_revision: 0,
        translator: None,
        theme_listeners: Vec::new(),
    });
    // 主题守护：FluxDown 主题变化 → 刷新所有存活脚本视图与已订阅的宿主；
    // 脚本改写了全局 `gpui_base::Theme`（`set_theme`）→ 恢复 FluxDown 投影。
    cx.observe_global::<FluxThemeState>(on_theme_changed)
        .detach();
    cx.observe_global::<gpui_base::Theme>(restore_theme_if_overridden)
        .detach();
    Ok(())
}

/// 设置界面语言来源（`fluxdown.locale()`）。
pub fn set_translator(cx: &mut App, translator: Entity<Translator>) {
    if let Some(host) = host_mut(cx) {
        host.translator = Some(translator);
    }
}

/// 订阅 FluxDown 主题变化（可移植视图宿主据此 `notify`）。监听者随进程存活。
pub fn subscribe_theme_changes(cx: &mut App, listener: ThemeListener) {
    if let Some(host) = host_mut(cx) {
        host.theme_listeners.push(listener);
    }
}

/// FluxDown 主题变化钩子（`init` 已按 `FluxThemeState` 注册；也可手动调用）：递增
/// 修订号、刷新所有存活脚本视图并通知订阅者。
pub fn on_theme_changed(cx: &mut App) {
    let Some(host) = host_mut(cx) else {
        return;
    };
    host.theme_revision = host.theme_revision.wrapping_add(1);
    host.views.retain(|view| view.upgrade().is_some());
    let views: Vec<_> = host.views.iter().filter_map(WeakEntity::upgrade).collect();
    let listeners = host.theme_listeners.clone();
    for view in views {
        view.update(cx, |view, cx| view.refresh(cx));
    }
    for listener in listeners {
        listener(cx);
    }
}

/// 脚本改写了 `gpui_base::Theme` 的颜色（与 FluxDown 当前主题不一致）时，延后重装当前主题。
/// 重装后两者一致，不会循环。
fn restore_theme_if_overridden(cx: &mut App) {
    if !cx.has_global::<FluxThemeState>() || !cx.has_global::<gpui_base::Theme>() {
        return;
    }
    if gpui_base::Theme::global(cx).tokens.colors != active_theme(cx).tokens().colors {
        cx.defer(fluxdown_ui_theme::reinstall_current);
    }
}

pub(crate) fn host_mut(cx: &mut App) -> Option<&mut PluginScriptHost> {
    if cx.has_global::<PluginScriptHost>() {
        Some(cx.global_mut::<PluginScriptHost>())
    } else {
        None
    }
}

fn catalog() -> Result<FrozenComponentRegistry, PluginUiError> {
    gpui_shell::components::components().map_err(|error| PluginUiError::Catalog(error.to_string()))
}

/// 不连任何 agent 的端口（类型声明生成器用：只需要声明文本，不执行）。
struct NullPort;

impl PluginUiPort for NullPort {
    fn call(&self, _method: String, _params: serde_json::Value) -> PortFuture<serde_json::Value> {
        Box::pin(async move {
            Err(RpcErrorData::new(
                fluxdown_protocol::ApplicationErrorCode::Unsupported,
                false,
            ))
        })
    }
}

/// 只含声明与空实现的 `fluxdown` 模块，供 SDK 类型生成器用（与运行时模块同一份声明）。
#[must_use]
pub fn fluxdown_module_declarations_only() -> HostModule {
    desktop::fluxdown_module("", None, ViewContainer::default(), Arc::new(NullPort))
}

/// 插件包目录是否就是本机可读路径（远端主机的插件目录在本机不存在）。
pub fn package_available(spec: &PluginWindowSpec) -> bool {
    Path::new(&spec.package_dir)
        .join(&spec.entry.entry)
        .is_file()
}

#[cfg(test)]
mod method_tests {
    use crate::desktop::is_allowed_method;

    #[test]
    fn only_daemon_and_agent_methods_are_reachable() {
        assert!(is_allowed_method("daemon.task.list"));
        assert!(is_allowed_method("agent.preferences.patch"));
        assert!(!is_allowed_method("system.shutdown"));
        assert!(!is_allowed_method("daemonx.task.list"));
    }
}
