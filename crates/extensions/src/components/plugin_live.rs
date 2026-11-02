//! 插件列表的实时视图：已打开的对话框按 identity 跟随最新列表，而不是持有打开时的快照。
//!
//! `ExtensionsView` 在插件列表整体替换（快照 / `PluginsChanged` / 熔断）后更新 [`LivePlugins`]，
//! 对话框内的实体通过 `observe_in` 读取它；插件被卸载时对话框自行关闭。

use fluxdown_protocol::PluginDto;
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, IntoElement, Render, Subscription, Window,
};
use gpui_component::WindowExt as _;

/// 最近一次已知的插件列表（断连期间保持不变，避免把「未知」当成「已卸载」）。
#[derive(Default)]
pub struct LivePlugins {
    plugins: Vec<PluginDto>,
    stale: bool,
}

impl LivePlugins {
    pub fn plugin(&self, identity: &str) -> Option<&PluginDto> {
        self.plugins
            .iter()
            .find(|plugin| plugin.identity == identity)
    }

    /// 整体替换；断连时保留最后快照，只更新 stale 标记。
    pub fn replace(&mut self, plugins: &[PluginDto], stale: bool, cx: &mut Context<Self>) {
        if !stale {
            self.plugins = plugins.to_vec();
        }
        let changed = self.stale != stale || !stale;
        self.stale = stale;
        if changed {
            cx.notify();
        }
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }
}

/// 透明包装：内容原样渲染；所属插件从列表消失（被卸载）时关闭当前最上层对话框
/// （与插件视图 `PluginViewSlot::Dialog` 的「最上层对话框」约定一致）。只关一次：
/// 卸载后的后续列表更新不会再关掉别的对话框。
pub struct CloseWhenRemoved {
    child: AnyView,
    closed: bool,
    _subscription: Subscription,
}

impl CloseWhenRemoved {
    pub fn wrap(
        child: impl Into<AnyView>,
        identity: String,
        live: &Entity<LivePlugins>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let child = child.into();
        cx.new(|cx| {
            let subscription =
                cx.observe_in(live, window, move |this: &mut Self, live, window, cx| {
                    if !this.closed && live.read(cx).plugin(&identity).is_none() {
                        this.closed = true;
                        window.close_dialog(cx);
                    }
                });
            Self {
                child,
                closed: false,
                _subscription: subscription,
            }
        })
    }
}

impl Render for CloseWhenRemoved {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.child.clone()
    }
}
