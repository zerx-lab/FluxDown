use std::rc::Rc;

use fluxdown_ui_components::{
    FluxIcon, SidebarState, SlidingHighlight, activity_button as activity_bar_button,
    color_transition, nav_icon_color, toolbar_action_button,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, AnyView, App, Bounds, Context, Div, Entity, FontWeight, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, img, percentage, point, px, size,
};
use gpui_component::{Icon, TitleBar, h_flex, menu::AppMenuBar, tooltip::Tooltip, v_flex};

use crate::assets::APP_LOGO_PATH;
use crate::window_controls::FixedSizeTitleBar;

/// shell 路由的稳定标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteId(&'static str);

impl RouteId {
    /// 创建稳定路由标识。
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }
}

/// 由应用装配并注入 shell 的路由元数据与页面内容。
pub struct ShellRoute {
    id: RouteId,
    button_id: &'static str,
    tooltip_id: &'static str,
    label_key: &'static str,
    icon: Icon,
    view: AnyView,
    /// 路由活跃时渲染在统一顶栏中的插槽（靠右内容由插槽自己排布）。
    title_bar: Option<AnyView>,
    /// 页面独立侧栏状态；由 shell 统一提供顶栏收起 / 展开入口。
    sidebar: Option<Entity<SidebarState>>,
    /// 可选路由参与活动栏「是否整体渲染」的判定；固定路由（如下载）不参与。
    optional: bool,
    visible: bool,
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
            button_id,
            tooltip_id,
            label_key,
            icon,
            view,
            title_bar: None,
            sidebar: None,
            optional: false,
            visible: true,
        }
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
enum ShellActionContent {
    Button {
        tooltip_id: &'static str,
        label_key: &'static str,
        icon: Box<Icon>,
        handler: ShellActionHandler,
    },
    /// 自绘视图自带悬浮提示与点击行为；shell 只提供按钮位。
    View(AnyView),
}

/// 由应用装配并注入 shell 的窗口级活动栏动作。
pub struct ShellAction {
    button_id: &'static str,
    content: ShellActionContent,
    /// 可选动作参与活动栏「是否整体渲染」的判定；固定动作（如设置）不参与。
    optional: bool,
    visible: bool,
}

impl ShellAction {
    /// 创建不切换主内容路由的活动栏动作；默认固定显示（非可选、可见）。
    pub fn new(
        button_id: &'static str,
        tooltip_id: &'static str,
        label_key: &'static str,
        icon: Icon,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            button_id,
            content: ShellActionContent::Button {
                tooltip_id,
                label_key,
                icon: Box::new(icon),
                handler: Rc::new(handler),
            },
            optional: false,
            visible: true,
        }
    }

    /// 以自绘视图占据一个活动栏按钮位（`ACTIVITY_BUTTON_SIZE` 见方，居中摆放）。
    /// 视图自行观察数据源并 notify，shell 不重建它。
    pub fn view(button_id: &'static str, view: AnyView) -> Self {
        Self {
            button_id,
            content: ShellActionContent::View(view),
            optional: false,
            visible: true,
        }
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

/// 活动栏轨宽度：32px 按钮居中 + 左右各 8px 留白。
const ACTIVITY_RAIL_WIDTH: gpui::Pixels = px(48.);
/// 活动栏每个按钮位所占的行高（等于按钮自身高度，纵向间距由 `gap` 提供）。
const ACTIVITY_TILE_HEIGHT: gpui::Pixels = px(32.);
/// 活动栏按钮尺寸。
const ACTIVITY_BUTTON_SIZE: gpui::Pixels = px(32.);
/// 活动栏图标比 `extended.icon.lg` 大的余量（图标 = lg + 2，随 lg 缩放）。
const ACTIVITY_ICON_EXTRA: gpui::Pixels = px(2.);
/// GPUI 窗口外壳：只负责窗口 chrome、活动栏、路由与内容槽位。
pub struct ShellView {
    translator: Entity<Translator>,
    active_route: Option<RouteId>,
    routes: Vec<ShellRoute>,
    actions: Vec<ShellAction>,
    /// Windows / Linux 标题栏内的应用菜单；macOS 走原生菜单不渲染。
    menu_bar: Option<Entity<AppMenuBar>>,
    /// 已应用到 macOS 交通灯的纵向偏移；标题栏随文字放大撑高时重新居中。
    traffic_light_y: Pixels,
}

impl ShellView {
    /// 创建 shell，并使用传入顺序的首条路由作为初始页面。
    pub fn new(
        translator: Entity<Translator>,
        routes: Vec<ShellRoute>,
        actions: Vec<ShellAction>,
        menu_bar: Option<Entity<AppMenuBar>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let active_route = routes.first().map(|route| route.id);
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        for route in &routes {
            if let Some(sidebar) = &route.sidebar {
                let route_id = route.id;
                cx.observe(sidebar, move |this, _, cx| {
                    if this.active_route == Some(route_id) {
                        cx.notify();
                    }
                })
                .detach();
            }
        }
        Self {
            translator,
            active_route,
            routes,
            actions,
            menu_bar,
            traffic_light_y: crate::initial_traffic_light_y(),
        }
    }

    /// 切换到指定路由（宿主导航入口）。
    pub fn navigate(&mut self, route: RouteId, cx: &mut Context<Self>) {
        if self.active_route != Some(route) {
            self.active_route = Some(route);
            cx.notify();
        }
    }

    /// 按活动栏按钮 id 设置可选路由或动作的可见性（宿主偏好回流入口）；
    /// 隐藏当前活跃路由时自动切到首条可见路由。
    pub fn set_entry_visible(&mut self, button_id: &str, visible: bool, cx: &mut Context<Self>) {
        if let Some(route) = self.routes.iter_mut().find(|r| r.button_id == button_id) {
            if route.visible == visible {
                return;
            }
            route.visible = visible;
            let id = route.id;
            if !visible && self.active_route == Some(id) {
                self.active_route = self.routes.iter().find(|r| r.visible).map(|r| r.id);
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

    /// 统一顶栏：macOS 交通灯 / Windows·Linux logo + 应用菜单，其后是当前活跃路由的插槽；
    /// Windows·Linux 的窗口按钮由 `TitleBar` 自带。
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let spacing = theme.tokens().spacing;
        let extended = theme.extended().colors;
        let is_macos = cfg!(target_os = "macos");
        // macOS 走原生菜单且交通灯占据左侧，不渲染 logo 与应用内菜单。
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
        let sidebar_toggle = self
            .active_route
            .and_then(|id| self.routes.iter().find(|route| route.id == id))
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
                    .child(
                        h_flex()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .children(slot),
                    ),
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
        let state = sidebar.clone();
        div()
            .id("shell-sidebar-toggle-tooltip")
            .flex_none()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .tooltip({
                let label = label.clone();
                move |window, cx| Tooltip::new(label.clone()).build(window, cx)
            })
            .child(
                toolbar_action_button(
                    "shell-sidebar-toggle",
                    label,
                    Icon::new(FluxIcon::PanelRight)
                        .size(active_theme(cx).extended().icon.md)
                        .rotate(percentage(0.5)),
                    false,
                    false,
                    cx,
                )
                .on_click(move |_, _, cx| state.update(cx, |state, cx| state.toggle(cx))),
            )
    }

    /// 当前活跃路由挂载的顶栏插槽。
    fn active_title_bar(&self) -> Option<AnyView> {
        self.active_route
            .and_then(|active| self.routes.iter().find(|route| route.id == active))
            .and_then(|route| route.title_bar.clone())
    }

    fn route_button(
        &self,
        route: &ShellRoute,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.active_route == Some(route.id);
        let icon_size = active_theme(cx).extended().icon.lg + ACTIVITY_ICON_EXTRA;
        let icon_color =
            color_transition(route.button_id, nav_icon_color(selected, cx), window, cx);
        let label = SharedString::from(self.translator.read(cx).text(route.label_key).to_owned());
        let tooltip_label = label.clone();
        let route_id = route.id;

        div()
            .id(route.tooltip_id)
            .w(ACTIVITY_RAIL_WIDTH)
            .h(ACTIVITY_TILE_HEIGHT)
            .flex()
            .items_center()
            .justify_center()
            .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
            .child(
                activity_bar_button(
                    route.button_id,
                    label,
                    route.icon.clone().size(icon_size).text_color(icon_color),
                    selected,
                    ACTIVITY_BUTTON_SIZE,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.active_route != Some(route_id) {
                        this.active_route = Some(route_id);
                        cx.notify();
                    }
                })),
            )
            .into_any_element()
    }

    /// 路由按钮列：选中底是一块随选中项滑动的底块（按钮自身不画选中底）。
    fn route_buttons(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let gap = theme.tokens().spacing.xs;
        let radius = theme.components().nav_item_radius;
        let selected_color = theme.extended().colors.nav_selected;
        let highlight = self
            .routes
            .iter()
            .filter(|route| route.visible)
            .position(|route| self.active_route == Some(route.id))
            .map(|index| {
                let inset = (ACTIVITY_RAIL_WIDTH - ACTIVITY_BUTTON_SIZE) / 2.;
                let top = (ACTIVITY_TILE_HEIGHT + gap) * index as f32
                    + (ACTIVITY_TILE_HEIGHT - ACTIVITY_BUTTON_SIZE) / 2.;
                SlidingHighlight::new(
                    "shell-activity-selection",
                    Bounds::new(
                        point(inset, top),
                        size(ACTIVITY_BUTTON_SIZE, ACTIVITY_BUTTON_SIZE),
                    ),
                    selected_color,
                )
                .radius(radius)
            });
        let buttons: Vec<AnyElement> = self
            .routes
            .iter()
            .filter(|route| route.visible)
            .map(|route| self.route_button(route, window, cx))
            .collect();
        v_flex()
            .relative()
            .gap(gap)
            .children(highlight)
            .children(buttons)
    }

    fn action_button(&self, action: &ShellAction, cx: &mut Context<Self>) -> AnyElement {
        let tile = div()
            .w(ACTIVITY_RAIL_WIDTH)
            .h(ACTIVITY_TILE_HEIGHT)
            .flex()
            .items_center()
            .justify_center();
        let (tooltip_id, label_key, icon, handler) = match &action.content {
            ShellActionContent::View(view) => return tile.child(view.clone()).into_any_element(),
            ShellActionContent::Button {
                tooltip_id,
                label_key,
                icon,
                handler,
            } => (
                *tooltip_id,
                *label_key,
                Icon::clone(icon),
                Rc::clone(handler),
            ),
        };
        let label = SharedString::from(self.translator.read(cx).text(label_key).to_owned());
        let tooltip_label = label.clone();
        let theme = active_theme(cx);
        let icon_size = theme.extended().icon.lg + ACTIVITY_ICON_EXTRA;
        let icon_color = theme.tokens().colors.muted_foreground;

        tile.id(tooltip_id)
            .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
            .child(
                activity_bar_button(
                    action.button_id,
                    label,
                    icon.size(icon_size).text_color(icon_color),
                    false,
                    ACTIVITY_BUTTON_SIZE,
                    cx,
                )
                .on_click(move |_, window, cx| handler(window, cx)),
            )
            .into_any_element()
    }

    /// 自绘动作视图的按钮位尺寸（视图按此尺寸绘制以与图标按钮对齐）。
    pub const ACTION_BUTTON_SIZE: Pixels = ACTIVITY_BUTTON_SIZE;

    fn action_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let gap = active_theme(cx).tokens().spacing.xs;
        v_flex().gap(gap).children(
            self.actions
                .iter()
                .filter(|action| action.visible)
                .map(|action| self.action_button(action, cx)),
        )
    }

    /// 是否存在至少一个可见的「可选」路由或动作；活动栏仅在此为真时渲染。
    fn has_visible_optional_item(&self) -> bool {
        self.routes
            .iter()
            .any(|route| route.optional && route.visible)
            || self
                .actions
                .iter()
                .any(|action| action.optional && action.visible)
    }

    fn render_activity_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.has_visible_optional_item() {
            return None;
        }
        let theme = active_theme(cx);
        let spacing = theme.tokens().spacing;
        let extended = theme.extended();
        // 与侧栏同为 chrome 底，靠右侧 hairline 结构线分界（与侧栏|内容的 resizable 把手同色同粗）。
        Some(
            v_flex()
                .h_full()
                .w(ACTIVITY_RAIL_WIDTH)
                .flex_none()
                .justify_between()
                .bg(extended.colors.chrome)
                .border_r(extended.stroke.thin)
                .border_color(extended.colors.hairline)
                .pt(spacing.sm)
                .pb(spacing.sm)
                .child(self.route_buttons(window, cx))
                .child(self.action_buttons(cx)),
        )
    }

    fn active_content(&self) -> AnyElement {
        self.active_route
            .and_then(|active| self.routes.iter().find(|route| route.id == active))
            .map_or_else(
                || div().into_any_element(),
                |route| route.view.clone().into_any_element(),
            )
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::sync_traffic_light(
            window,
            crate::title_bar_height(cx),
            &mut self.traffic_light_y,
        );
        let colors = active_theme(cx).tokens().colors;
        v_flex()
            .size_full()
            .relative()
            .bg(colors.background)
            .text_color(colors.foreground)
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .children(self.render_activity_bar(window, cx))
                    .child(
                        div()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(self.active_content()),
                    ),
            )
    }
}
