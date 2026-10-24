use std::{collections::HashSet, rc::Rc};

use fluxdown_ui_components::{SidebarState, sidebar_toggle_button};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, AnyView, AnyWindowHandle, App, Context, Entity, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement, Pixels, Render, SharedString,
    Styled, Window, div, img, px,
};
use gpui_component::{Icon, TitleBar, h_flex, menu::AppMenuBar, v_flex};
use serde_json::Value;

use crate::assets::APP_LOGO_PATH;
use crate::chrome::{ACTIVITY_BUTTON_SIZE, LayoutChangeHandler, ShellChrome, ShellWidget};
use crate::window_controls::FixedSizeTitleBar;
use crate::{ChromeFamilies, ChromeItemSpec, ChromeLayout, ChromeZone};

/// 动态（插件）路由 id 的固定前缀；只有该前缀的路由由 [`ShellView::set_dynamic_routes`] 管理。
pub const DYNAMIC_ROUTE_PREFIX: &str = "plugin:";

/// shell 路由的稳定标识。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteId(SharedString);

impl RouteId {
    /// 创建稳定路由标识。
    pub const fn new(value: &'static str) -> Self {
        Self(SharedString::new_static(value))
    }

    /// 插件视图路由：`plugin:<identity>:<view>`。
    #[must_use]
    pub fn plugin(identity: &str, view: &str) -> Self {
        Self(SharedString::from(format!(
            "{DYNAMIC_ROUTE_PREFIX}{identity}:{view}"
        )))
    }

    /// 标识字符串，同时是 chrome 布局里的条目 id。
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }

    /// 是否为动态（插件）路由。
    #[must_use]
    pub fn is_dynamic(&self) -> bool {
        self.as_str().starts_with(DYNAMIC_ROUTE_PREFIX)
    }
}

/// 路由在活动栏中的标签：i18n 键或原文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellLabel {
    Key(&'static str),
    Text(SharedString),
}

/// 由应用装配并注入 shell 的路由元数据与页面内容。
pub struct ShellRoute {
    pub(crate) id: RouteId,
    pub(crate) button_id: SharedString,
    pub(crate) tooltip_id: SharedString,
    pub(crate) label: ShellLabel,
    pub(crate) icon: Icon,
    view: AnyView,
    /// 路由活跃时渲染在统一顶栏中的插槽（靠右内容由插槽自己排布）。
    title_bar: Option<AnyView>,
    /// 页面独立侧栏状态；由 shell 统一提供顶栏收起 / 展开入口。
    sidebar: Option<Entity<SidebarState>>,
    /// 可选路由参与活动栏「是否整体渲染」的判定；固定路由（如下载）不参与。
    pub(crate) optional: bool,
    pub(crate) visible: bool,
    /// `navigate` 切到本路由时调用（经 `cx.defer`，不会重入 shell）。
    on_activate: Option<ShellActionHandler>,
}

impl ShellRoute {
    /// 创建一条 shell 路由；默认固定显示（非可选、可见）。
    pub fn new(
        id: RouteId,
        button_id: &'static str,
        tooltip_id: &'static str,
        label_key: &'static str,
        icon: Icon,
        view: AnyView,
    ) -> Self {
        Self {
            id,
            button_id: SharedString::new_static(button_id),
            tooltip_id: SharedString::new_static(tooltip_id),
            label: ShellLabel::Key(label_key),
            icon,
            view,
            title_bar: None,
            sidebar: None,
            optional: false,
            visible: true,
            on_activate: None,
        }
    }

    /// 创建插件视图路由（`id` 须以 `plugin:` 开头），标签为原文，图标由 app 按插件声明给出。
    pub fn plugin(id: RouteId, label: SharedString, icon: Icon, view: AnyView) -> Self {
        let button_id = SharedString::from(format!("activity-{}", id.as_str()));
        let tooltip_id = SharedString::from(format!("activity-{}-tooltip", id.as_str()));
        Self {
            id,
            button_id,
            tooltip_id,
            label: ShellLabel::Text(label),
            icon,
            view,
            title_bar: None,
            sidebar: None,
            optional: false,
            visible: true,
            on_activate: None,
        }
    }

    /// 路由被导航激活时的回调（在 `cx.defer` 中执行）。
    pub fn on_activate(mut self, handler: ShellActionHandler) -> Self {
        self.on_activate = Some(handler);
        self
    }

    /// 标记该路由是否为「可选」：活动栏在没有任何可见可选项时整体收起。
    pub fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    /// 为该路由挂载统一顶栏插槽：路由活跃时 shell 在标题栏中渲染它。
    ///
    /// 插槽内可交互元素须自行拦截左键 `mouse_down` 冒泡，空白处保持窗口拖拽 / 双击。
    pub fn with_title_bar(mut self, view: impl Into<AnyView>) -> Self {
        self.title_bar = Some(view.into());
        self
    }

    /// 为有侧边菜单的路由登记共享侧栏状态；按钮位置与行为由 shell 统一管理。
    pub fn with_sidebar(mut self, sidebar: Entity<SidebarState>) -> Self {
        self.sidebar = Some(sidebar);
        self
    }
}

type ShellActionHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// 活动栏动作的呈现：标准图标按钮，或由 app 注入的自绘视图（如账户头像）。
pub(crate) enum ShellActionContent {
    Button {
        tooltip_id: &'static str,
        icon: Box<Icon>,
        handler: ShellActionHandler,
    },
    /// 自绘视图自带悬浮提示与点击行为；shell 只提供按钮位。
    View(AnyView),
}

/// 由应用装配并注入 shell 的窗口级动作（默认摆在活动栏底部，可经布局移动）。
pub struct ShellAction {
    pub(crate) button_id: &'static str,
    /// chrome 布局里的条目 id；缺省同 `button_id`。
    pub(crate) layout_id: &'static str,
    /// 悬浮提示 / 编辑模式拖拽预览的文案键。
    pub(crate) label_key: &'static str,
    pub(crate) content: ShellActionContent,
    /// 可选动作参与活动栏「是否整体渲染」的判定；固定动作（如设置）不参与。
    pub(crate) optional: bool,
    pub(crate) visible: bool,
}

impl ShellAction {
    /// 创建不切换主内容路由的动作；默认固定显示（非可选、可见）。
    pub fn new(
        button_id: &'static str,
        tooltip_id: &'static str,
        label_key: &'static str,
        icon: Icon,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            button_id,
            layout_id: button_id,
            label_key,
            content: ShellActionContent::Button {
                tooltip_id,
                icon: Box::new(icon),
                handler: Rc::new(handler),
            },
            optional: false,
            visible: true,
        }
    }

    /// 以自绘视图占据一个按钮位（[`ShellView::ACTION_BUTTON_SIZE`] 见方，居中摆放）。
    /// 视图自行观察数据源并 notify，shell 不重建它。
    pub fn view(button_id: &'static str, label_key: &'static str, view: AnyView) -> Self {
        Self {
            button_id,
            layout_id: button_id,
            label_key,
            content: ShellActionContent::View(view),
            optional: false,
            visible: true,
        }
    }

    /// 设置 chrome 布局里的条目 id。
    pub fn layout_id(mut self, layout_id: &'static str) -> Self {
        self.layout_id = layout_id;
        self
    }

    /// 标记该动作是否为「可选」：活动栏在没有任何可见可选项时整体收起。
    pub fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }
}

/// 使用 FluxDown 自定义标题栏承载任意能力页面的辅助窗口。
pub struct AuxiliaryWindowView {
    _translator: Entity<Translator>,
    title: SharedString,
    /// 已写入 OS 窗口标题的默认标题；与 `title` 不同说明语言切换后需要同步。
    os_title: SharedString,
    /// 宿主设置的动态标题（如任务文件名）；存在时优先于按语言刷新的默认标题。
    title_override: Option<SharedString>,
    content: AnyView,
    /// 窗口是否可由用户调整尺寸；`false` 时 Windows / Linux 自绘只含最小化 + 关闭的标题栏
    /// （系统不会响应最大化，留着只会误导）。macOS 恒用系统交通灯。
    resizable: bool,
    /// 已应用到 macOS 交通灯的纵向偏移；标题栏随文字放大撑高时重新居中。
    traffic_light_y: Pixels,
}

impl AuxiliaryWindowView {
    /// 创建跟随共享语言状态更新标题的辅助窗口 chrome。
    pub fn new(
        translator: Entity<Translator>,
        title_key: &'static str,
        content: AnyView,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = SharedString::from(translator.read(cx).text(title_key).to_owned());
        cx.observe(&translator, move |this, translator, cx| {
            this.title = SharedString::from(translator.read(cx).text(title_key).to_owned());
            cx.notify();
        })
        .detach();
        Self {
            _translator: translator,
            os_title: title.clone(),
            title,
            title_override: None,
            content,
            resizable: true,
            traffic_light_y: crate::initial_traffic_light_y(),
        }
    }

    /// 声明承载窗口是否可调整尺寸（默认可调）；应与窗口选项的 `is_resizable` 保持一致。
    #[must_use]
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    /// 以动态文本覆盖标题（传 `None` 恢复按语言键显示的默认标题）。
    pub fn set_title(&mut self, title: Option<SharedString>, cx: &mut Context<Self>) {
        if self.title_override != title {
            if title.is_none() {
                // 宿主自己写过 OS 标题；恢复默认时需要重新同步。
                self.os_title = SharedString::default();
            }
            self.title_override = title;
            cx.notify();
        }
    }

    /// 辅助窗口标题栏：与主窗口统一顶栏同为 chrome 底 + hairline 底线；标题 sm MEDIUM，
    /// 三平台一致左对齐（macOS 紧随交通灯，Windows/Linux 自窗口左缘起）。
    ///
    /// 标题行以绝对定位铺满拖动区、不参与内容宽度：gpui-component 的拖动区不允许收缩，
    /// 在流内的单行标题会按其不换行的最小宽度把右侧窗口控制按钮挤出窗口。
    /// 不可调尺寸的窗口在 Windows / Linux 改用 [`FixedSizeTitleBar`]（无最大化按钮）。
    fn render_title_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended().colors;
        let spacing = tokens.spacing;
        let typography = tokens.typography.clone();
        let height = crate::title_bar_height(cx);
        let title_row = h_flex()
            .absolute()
            .inset_0()
            .items_center()
            .pl(crate::title_bar_left_padding(
                cfg!(target_os = "macos"),
                spacing.sm,
            ))
            .pr(spacing.md)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(typography.sm.size)
                    .line_height(typography.sm.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.colors.foreground)
                    .child(
                        self.title_override
                            .clone()
                            .unwrap_or_else(|| self.title.clone()),
                    ),
            );

        // 显式 `.bg` 覆盖 gpui-component 默认渐变；`.h` 经 refine_style 覆盖默认 34px。
        if !self.resizable && !cfg!(target_os = "macos") {
            return FixedSizeTitleBar::new()
                .pl(spacing.sm)
                .h(height)
                .bg(extended.chrome)
                .border_color(extended.hairline)
                .child(title_row)
                .into_any_element();
        }
        let title_bar = TitleBar::new();
        #[cfg(not(target_os = "macos"))]
        let title_bar = title_bar.pl(spacing.sm);
        title_bar
            .h(height)
            .bg(extended.chrome)
            .border_color(extended.hairline)
            .child(title_row)
            .into_any_element()
    }
}

impl Render for AuxiliaryWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::sync_traffic_light(
            window,
            crate::title_bar_height(cx),
            &mut self.traffic_light_y,
        );
        // 构造时拿不到 Window：语言切换后在渲染期把默认标题同步到 OS 窗口标题。
        if self.title_override.is_none() && self.os_title != self.title {
            window.set_window_title(&self.title);
            self.os_title = self.title.clone();
        }
        let colors = active_theme(cx).tokens().colors;
        v_flex()
            .size_full()
            .relative()
            .bg(colors.surface)
            .text_color(colors.foreground)
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .bg(colors.surface)
                    .child(self.content.clone()),
            )
    }
}

/// GPUI 窗口外壳：窗口 chrome（顶栏 / 活动栏 / 状态栏）、路由与内容槽位；
/// chrome 条目的摆放由 [`ChromeLayout`] 决定（渲染见 `chrome.rs`）。
pub struct ShellView {
    pub(crate) translator: Entity<Translator>,
    pub(crate) active_route: Option<RouteId>,
    pub(crate) routes: Vec<ShellRoute>,
    pub(crate) actions: Vec<ShellAction>,
    pub(crate) widgets: Vec<ShellWidget>,
    pub(crate) catalog: Vec<ChromeItemSpec>,
    pub(crate) layout: ChromeLayout,
    pub(crate) on_layout_change: LayoutChangeHandler,
    /// 布局编辑模式：条目可拖放，自身交互暂停。
    pub(crate) editing: bool,
    /// 已应用到 macOS 交通灯的纵向偏移；标题栏随文字放大撑高时重新居中。
    traffic_light_y: Pixels,
    /// 首帧渲染时记下的窗口句柄，用于 `on_activate` 回调。
    window: Option<AnyWindowHandle>,
    /// 宿主最近一次下发（或本窗口写回）的布局偏好；动态路由变化时据此重新归一化。
    pub(crate) stored_layout: Option<Value>,
    /// Windows / Linux 标题栏内的应用菜单；`None` = 不显示（宿主按偏好注入，macOS 恒为 `None`）。
    menu_bar: Option<Entity<AppMenuBar>>,
}

impl ShellView {
    /// 创建 shell，并使用传入顺序的首条路由作为初始页面；布局先取目录默认值，
    /// 宿主随后经 [`Self::apply_layout`] 应用偏好。
    pub fn new(
        translator: Entity<Translator>,
        routes: Vec<ShellRoute>,
        actions: Vec<ShellAction>,
        chrome: ShellChrome,
        cx: &mut Context<Self>,
    ) -> Self {
        let active_route = routes.first().map(|route| route.id.clone());
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        for route in &routes {
            Self::observe_sidebar(route, cx);
        }
        let ShellChrome {
            widgets,
            catalog,
            on_layout_change,
        } = chrome;
        Self {
            translator,
            active_route,
            routes,
            actions,
            widgets,
            layout: ChromeLayout::default_for(&catalog),
            catalog,
            on_layout_change,
            editing: false,
            traffic_light_y: crate::initial_traffic_light_y(),
            window: None,
            stored_layout: None,
            menu_bar: None,
        }
    }

    /// 切换到指定路由（宿主导航入口）；路由不存在时忽略。
    pub fn navigate(&mut self, route: RouteId, cx: &mut Context<Self>) {
        if self.active_route.as_ref() == Some(&route) || !self.routes.iter().any(|r| r.id == route)
        {
            return;
        }
        self.active_route = Some(route.clone());
        self.activate(&route, cx);
        cx.notify();
    }

    /// 路由被激活：经 `cx.defer` 调用其 `on_activate`（避免重入 shell）；窗口句柄在首帧渲染时补记。
    fn activate(&self, route: &RouteId, cx: &mut Context<Self>) {
        let Some(handler) = self
            .routes
            .iter()
            .find(|r| &r.id == route)
            .and_then(|r| r.on_activate.clone())
        else {
            return;
        };
        let Some(window) = self.window else {
            return;
        };
        cx.defer(move |cx| {
            // 窗口已关闭时无需激活，更新失败即可忽略。
            let Ok(()) = window.update(cx, |_, window, cx| handler(window, cx)) else {
                // 窗口已关闭，无需激活。
                return;
            };
        });
    }

    fn observe_sidebar(route: &ShellRoute, cx: &mut Context<Self>) {
        if let Some(sidebar) = &route.sidebar {
            let route_id = route.id.clone();
            cx.observe(sidebar, move |this, _, cx| {
                if this.active_route.as_ref() == Some(&route_id) {
                    cx.notify();
                }
            })
            .detach();
        }
    }

    /// 替换全部 `plugin:` 前缀的动态路由（以及活动栏目录里对应条目）；位置由已存布局偏好重新归一化，
    /// 因此晚到或重装的插件能找回原位置。本方法从不写回偏好。
    pub fn set_dynamic_routes(&mut self, mut routes: Vec<ShellRoute>, cx: &mut Context<Self>) {
        let mut seen = HashSet::new();
        routes.retain(|route| route.id.is_dynamic() && seen.insert(route.id.clone()));
        for route in &routes {
            Self::observe_sidebar(route, cx);
        }
        self.routes.retain(|route| !route.id.is_dynamic());
        self.routes.extend(routes);
        self.catalog
            .retain(|spec| !spec.id.starts_with(DYNAMIC_ROUTE_PREFIX));
        self.catalog
            .extend(self.routes.iter().filter(|r| r.id.is_dynamic()).map(|r| {
                ChromeItemSpec::new(
                    r.id.0.clone(),
                    ChromeZone::RailTop,
                    ChromeFamilies::RAIL_TITLE,
                )
            }));
        self.layout = ChromeLayout::from_value(self.stored_layout.as_ref(), &self.catalog);
        let active_exists = self
            .active_route
            .as_ref()
            .is_some_and(|active| self.routes.iter().any(|r| &r.id == active));
        if !active_exists {
            let next = self
                .routes
                .iter()
                .find(|r| r.visible)
                .or(self.routes.first())
                .map(|r| r.id.clone());
            self.active_route = next.clone();
            if let Some(next) = next {
                self.activate(&next, cx);
            }
        }
        cx.notify();
    }

    /// 按活动栏按钮 id 设置可选路由或动作的可见性（宿主偏好回流入口）；
    /// 隐藏当前活跃路由时自动切到首条可见路由。
    pub fn set_entry_visible(&mut self, button_id: &str, visible: bool, cx: &mut Context<Self>) {
        if let Some(route) = self.routes.iter_mut().find(|r| &*r.button_id == button_id) {
            if route.visible == visible {
                return;
            }
            route.visible = visible;
            let id = route.id.clone();
            if !visible && self.active_route.as_ref() == Some(&id) {
                self.active_route = self.routes.iter().find(|r| r.visible).map(|r| r.id.clone());
            }
            cx.notify();
            return;
        }
        let Some(action) = self.actions.iter_mut().find(|a| a.button_id == button_id) else {
            return;
        };
        if action.visible == visible {
            return;
        }
        action.visible = visible;
        cx.notify();
    }

    /// 设置标题栏应用菜单栏（宿主偏好回流入口）；`None` 隐藏。
    pub fn set_menu_bar(&mut self, menu_bar: Option<Entity<AppMenuBar>>, cx: &mut Context<Self>) {
        if self.menu_bar.as_ref().map(Entity::entity_id) == menu_bar.as_ref().map(Entity::entity_id)
        {
            return;
        }
        self.menu_bar = menu_bar;
        cx.notify();
    }

    /// 统一顶栏：macOS 交通灯 / Windows·Linux logo + 应用菜单，其后是顶栏左区、当前活跃路由的
    /// 插槽与顶栏右区；Windows·Linux 的窗口按钮由 `TitleBar` 自带。
    fn render_title_bar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = active_theme(cx);
        let spacing = theme.tokens().spacing;
        let extended = theme.extended().colors;
        let is_macos = cfg!(target_os = "macos");
        // macOS 交通灯占据左侧，不渲染 logo；应用菜单走原生菜单栏。
        let leading = (!is_macos).then(|| {
            h_flex()
                .h_full()
                .flex_none()
                .items_center()
                .gap(spacing.sm)
                .child(img(APP_LOGO_PATH).size(px(16.)))
                .children(self.menu_bar.clone().map(|menu_bar| {
                    h_flex()
                        .h_full()
                        .items_center()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(menu_bar)
                }))
        });
        let slot = self.active_title_bar();
        let title_leading = self.render_title_zone(ChromeZone::TitleLeading, window, cx);
        let title_trailing = self.render_title_zone(ChromeZone::TitleTrailing, window, cx);
        let sidebar_toggle = self
            .active_route
            .as_ref()
            .and_then(|id| self.routes.iter().find(|route| route.id == *id))
            .and_then(|route| route.sidebar.as_ref())
            .filter(|sidebar| sidebar.read(cx).is_available())
            .map(|sidebar| self.render_sidebar_toggle(sidebar, cx));
        let title_bar = TitleBar::new();
        #[cfg(not(target_os = "macos"))]
        let title_bar = title_bar.pl(spacing.sm);

        // 显式 `.bg` 覆盖 gpui-component 默认渐变；`.h` 经 refine_style 覆盖默认 34px。
        title_bar
            .h(crate::title_bar_height(cx))
            .bg(extended.chrome)
            .border_color(extended.hairline)
            .child(
                // 绝对定位铺满拖动区：标题行不参与 gpui-component 拖动区的内容宽度，窗口控制按钮不会被挤出。
                h_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .gap(spacing.sm)
                    .pl(crate::title_bar_left_padding(is_macos, spacing.sm))
                    .pr(if is_macos { spacing.md } else { spacing.sm })
                    .children(leading)
                    .children(sidebar_toggle)
                    .children(title_leading)
                    .child(
                        h_flex()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .children(slot),
                    )
                    .children(title_trailing),
            )
    }

    fn render_sidebar_toggle(
        &self,
        sidebar: &Entity<SidebarState>,
        cx: &App,
    ) -> impl IntoElement + use<> {
        let key = if sidebar.read(cx).is_collapsed() {
            "expandSidebar"
        } else {
            "collapseSidebar"
        };
        let label = SharedString::from(self.translator.read(cx).text(key).to_owned());
        sidebar_toggle_button("shell-sidebar-toggle", label, sidebar, cx)
    }

    /// 当前活跃路由挂载的顶栏插槽。
    fn active_title_bar(&self) -> Option<AnyView> {
        self.active_route
            .as_ref()
            .and_then(|active| self.routes.iter().find(|route| route.id == *active))
            .and_then(|route| route.title_bar.clone())
    }

    /// 自绘动作视图的按钮位尺寸（视图按此尺寸绘制以与图标按钮对齐）。
    pub const ACTION_BUTTON_SIZE: Pixels = ACTIVITY_BUTTON_SIZE;

    fn active_content(&self) -> AnyElement {
        self.active_route
            .as_ref()
            .and_then(|active| self.routes.iter().find(|route| route.id == *active))
            .map_or_else(
                || div().into_any_element(),
                |route| route.view.clone().into_any_element(),
            )
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.window.is_none() {
            self.window = Some(window.window_handle());
            if let Some(route) = self.active_route.clone() {
                self.activate(&route, cx);
            }
        }
        crate::sync_traffic_light(
            window,
            crate::title_bar_height(cx),
            &mut self.traffic_light_y,
        );
        let colors = active_theme(cx).tokens().colors;
        let title_bar = self.render_title_bar(window, cx);
        let banner = self.render_edit_banner(cx);
        let activity_bar = self.render_activity_bar(window, cx);
        let status_bar = self.render_status_bar(window, cx);
        v_flex()
            .size_full()
            .relative()
            .bg(colors.background)
            .text_color(colors.foreground)
            .on_key_down(cx.listener(Self::on_edit_key_down))
            .child(title_bar)
            .children(banner)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .children(activity_bar)
                    .child(
                        v_flex()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .child(self.active_content()),
                            )
                            .children(status_bar),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc, sync::Arc};

    use fluxdown_ui_components::FluxIcon;
    use fluxdown_ui_i18n::I18nCatalog;
    use gpui::{AppContext as _, Empty, TestApp};
    use serde_json::json;

    use super::*;

    struct Host {
        shell: Entity<ShellView>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            Empty
        }
    }

    fn view(cx: &mut App) -> AnyView {
        cx.new(|_| Empty).into()
    }

    fn route(cx: &mut App, id: &'static str) -> ShellRoute {
        ShellRoute::plugin(
            RouteId::new(id),
            SharedString::new_static(id),
            Icon::new(FluxIcon::Sparkles),
            view(cx),
        )
    }

    fn route_ids(shell: &ShellView) -> Vec<String> {
        shell
            .routes
            .iter()
            .map(|r| r.id.as_str().to_owned())
            .collect()
    }

    fn open(app: &mut TestApp) -> gpui::TestAppWindow<Host> {
        app.open_window(|window, cx| {
            let catalog = Arc::new(I18nCatalog::load_embedded().expect("embedded translations"));
            let translator = cx.new(|_| catalog.translator("en"));
            let downloads = ShellRoute::new(
                RouteId::new("downloads"),
                "activity-downloads",
                "activity-downloads-tooltip",
                "downloads",
                Icon::new(FluxIcon::Sparkles),
                view(cx),
            );
            let chrome = ShellChrome {
                widgets: Vec::new(),
                catalog: vec![ChromeItemSpec::new(
                    "downloads",
                    ChromeZone::RailTop,
                    ChromeFamilies::ALL,
                )],
                on_layout_change: Rc::new(|_, _| {}),
            };
            let shell =
                cx.new(|cx| ShellView::new(translator, vec![downloads], Vec::new(), chrome, cx));
            let handle = window.window_handle();
            shell.update(cx, |shell, _| shell.window = Some(handle));
            Host { shell }
        })
    }

    #[test]
    fn dynamic_routes_follow_plugin_entries() {
        let mut app = TestApp::new();
        let mut win = open(&mut app);
        let activated = Rc::new(Cell::new(0_u32));

        win.update(|host, _, cx| {
            host.shell.update(cx, |shell, cx| {
                let counter = Rc::clone(&activated);
                let one = route(cx, "plugin:a:one")
                    .on_activate(Rc::new(move |_, _| counter.set(counter.get() + 1)));
                let two = route(cx, "plugin:a:two");
                // 非 `plugin:` 前缀的路由必须被丢弃。
                let foreign = route(cx, "foreign");
                shell.set_dynamic_routes(vec![one, two, foreign], cx);
                assert_eq!(
                    route_ids(shell),
                    ["downloads", "plugin:a:one", "plugin:a:two"]
                );
                assert!(shell.catalog.iter().any(|s| s.id == "plugin:a:one"));
                assert!(shell.layout.zone_of("plugin:a:two").is_some());
                shell.navigate(RouteId::new("plugin:a:one"), cx);
                assert_eq!(shell.active_route, Some(RouteId::new("plugin:a:one")));
                // 不存在的路由被忽略。
                shell.navigate(RouteId::new("plugin:zzz"), cx);
                assert_eq!(shell.active_route, Some(RouteId::new("plugin:a:one")));
            });
        });
        assert_eq!(activated.get(), 1);

        win.update(|host, _, cx| {
            host.shell.update(cx, |shell, cx| {
                let two = route(cx, "plugin:a:two");
                shell.set_dynamic_routes(vec![two], cx);
                assert_eq!(route_ids(shell), ["downloads", "plugin:a:two"]);
                assert_eq!(shell.active_route, Some(RouteId::new("downloads")));
                assert!(!shell.catalog.iter().any(|s| s.id == "plugin:a:one"));
                assert_eq!(shell.layout.zone_of("plugin:a:one"), None);

                shell.set_dynamic_routes(Vec::new(), cx);
                assert_eq!(route_ids(shell), ["downloads"]);
                assert_eq!(shell.catalog.len(), 1);
            });
        });
        assert_eq!(activated.get(), 1);
    }

    #[test]
    fn stored_preference_restores_position_after_reinstall() {
        let mut app = TestApp::new();
        let mut win = open(&mut app);
        win.update(|host, _, cx| {
            host.shell.update(cx, |shell, cx| {
                shell.apply_layout(Some(&json!({"railBottom": ["plugin:a:one"]})), cx);
                assert_eq!(shell.layout.zone_of("plugin:a:one"), None);
                let one = route(cx, "plugin:a:one");
                shell.set_dynamic_routes(vec![one], cx);
                assert_eq!(
                    shell.layout.zone_of("plugin:a:one"),
                    Some(ChromeZone::RailBottom)
                );
            });
        });
    }
}
