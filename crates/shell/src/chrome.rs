//! shell 的可编辑 chrome：按 [`ChromeLayout`] 把路由按钮、窗口级动作与宿主控件摆进
//! 顶栏 / 活动栏 / 状态栏。编辑模式下条目可拖放（落点规则见 [`ChromeLayout::move_item`]），
//! 可隐藏的条目可在角上切换隐藏（[`ChromeLayout::set_hidden`]）；布局变化经宿主回调持久化，
//! 再由 [`ShellView::apply_layout`] 回流。

use std::rc::Rc;

use fluxdown_ui_components::{
    ButtonVariant, FluxIcon, SlidingHighlight, activity_button, button, color_transition,
    nav_icon_color, tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, point, prelude::FluentBuilder as _, px,
    size,
};
use gpui_component::{
    Icon, IconName, h_flex,
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    v_flex,
};
use serde_json::Value;

use crate::view::{RouteId, ShellAction, ShellActionContent, ShellLabel, ShellRoute, ShellView};
use crate::{ChromeFamilies, ChromeFamily, ChromeItemSpec, ChromeLayout, ChromeZone};

/// 活动栏轨宽度：32px 按钮居中 + 左右各 8px 留白。
pub(crate) const ACTIVITY_RAIL_WIDTH: Pixels = px(48.);
/// 活动栏每个按钮位所占的行高（等于按钮自身高度，纵向间距由 `gap` 提供）。
const ACTIVITY_TILE_HEIGHT: Pixels = px(32.);
/// 活动栏按钮尺寸。
pub(crate) const ACTIVITY_BUTTON_SIZE: Pixels = px(32.);
/// 活动栏图标比 `extended.icon.lg` 大的余量（图标 = lg + 2，随 lg 缩放）。
const ACTIVITY_ICON_EXTRA: Pixels = px(2.);
/// 编辑模式下空区域的最小落点宽度（顶栏 / 状态栏）。
const EMPTY_ZONE_MIN_WIDTH: Pixels = px(48.);
/// 编辑模式下活动栏区域的虚线框内缩（框宽 = 轨宽 − 2 × 内缩）。
const RAIL_ZONE_INSET: Pixels = px(4.);
/// 编辑模式下被隐藏条目的不透明度。
const HIDDEN_ITEM_OPACITY: f32 = 0.4;
/// 编辑模式下条目角上隐藏 / 显示按钮的边长。
const VISIBILITY_TOGGLE_SIZE: Pixels = px(16.);

/// 宿主控件渲染：参数为所在区域（决定紧凑形态与菜单弹出方向）；`None` = 当前无内容。
pub type ShellWidgetRender = Rc<dyn Fn(ChromeZone, &mut Window, &mut App) -> Option<AnyElement>>;

/// 布局变化回调：参数是新的持久化值（[`ChromeLayout::to_value`]）。
pub type LayoutChangeHandler = Rc<dyn Fn(Value, &mut App)>;

/// 宿主控件「被隐藏时仍临时显示」的判定（如快捷键唤出的搜索框）。
pub type ShellWidgetReveal = Rc<dyn Fn(&App) -> bool>;

/// 由应用注入、可在 chrome 区域间拖动的控件（速度读数、限速、代理、搜索框…）。
///
/// 控件在 shell 的渲染里绘制：宿主须经 [`ShellView::observe_widget_source`] 登记数据源，
/// 数据变化时 shell 才会重绘。
pub struct ShellWidget {
    id: &'static str,
    label_key: &'static str,
    render: ShellWidgetRender,
    /// 只在该路由活跃时有内容（页面专属控件）；`None` = 任何页面都显示。
    route: Option<RouteId>,
    reveal: Option<ShellWidgetReveal>,
}

impl ShellWidget {
    /// `id` 为布局目录中的稳定 id；`label_key` 用于编辑模式的占位与拖拽预览。
    pub fn new(
        id: &'static str,
        label_key: &'static str,
        render: impl Fn(ChromeZone, &mut Window, &mut App) -> Option<AnyElement> + 'static,
    ) -> Self {
        Self {
            id,
            label_key,
            render: Rc::new(render),
            route: None,
            reveal: None,
        }
    }

    /// 限定为页面专属控件：只在 `route` 活跃时渲染。
    #[must_use]
    pub fn on_route(mut self, route: RouteId) -> Self {
        self.route = Some(route);
        self
    }

    /// 被用户隐藏时，`reveal` 为真则仍临时显示（不改变布局里的隐藏标记）。
    #[must_use]
    pub fn revealed_when(mut self, reveal: impl Fn(&App) -> bool + 'static) -> Self {
        self.reveal = Some(Rc::new(reveal));
        self
    }
}

/// shell 的 chrome 配置：宿主控件、布局目录（含路由 / 动作的布局 id）与持久化回调。
pub struct ShellChrome {
    pub widgets: Vec<ShellWidget>,
    pub catalog: Vec<ChromeItemSpec>,
    pub on_layout_change: LayoutChangeHandler,
}

/// 拖拽载荷：条目 id、预览文案与可进入的承载条（落点据此决定是否高亮）。
#[derive(Clone)]
struct ChromeDrag {
    id: SharedString,
    label: SharedString,
    families: ChromeFamilies,
}

impl Render for ChromeDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        h_flex()
            .px(tokens.spacing.sm)
            .py(tokens.spacing.xxs)
            .gap(tokens.spacing.xs)
            .items_center()
            .rounded(tokens.radius.sm)
            .border_1()
            .border_color(tokens.colors.border)
            .bg(tokens.colors.surface)
            .shadow_sm()
            .text_size(extended.caption.size)
            .line_height(extended.caption.line_height)
            .text_color(tokens.colors.surface_foreground)
            .child(Icon::new(FluxIcon::GripVertical).size(extended.icon.sm))
            .child(self.label.clone())
    }
}

/// 一个已渲染的条目。
struct Placed {
    element: AnyElement,
    /// 当前活跃路由的按钮（活动栏据此画滑动选中底）。
    selected: bool,
    /// 是否让活动栏「值得显示」：宿主控件与可选路由 / 动作；固定入口（下载、设置）不算。
    anchors_rail: bool,
}

/// 已解析的布局条目。
enum Item<'a> {
    Route(&'a ShellRoute),
    Action(&'a ShellAction),
    Widget(&'a ShellWidget),
}

impl ShellView {
    /// 宿主偏好回流：按目录归一化后与当前布局不同才重绘。
    pub fn apply_layout(&mut self, stored: Option<&Value>, cx: &mut Context<Self>) {
        self.stored_layout = stored.cloned();
        let layout = ChromeLayout::from_value(stored, &self.catalog);
        if layout != self.layout {
            self.layout = layout;
            cx.notify();
        }
    }

    /// 登记宿主控件的数据源：其 notify 时 shell 重绘（控件在 shell 的渲染里读取它）。
    pub fn observe_widget_source<T: 'static>(
        &mut self,
        source: &Entity<T>,
        cx: &mut Context<Self>,
    ) {
        cx.observe(source, |_, _, cx| cx.notify()).detach();
    }

    /// 进入 / 退出布局编辑模式。
    pub fn set_editing(&mut self, editing: bool, cx: &mut Context<Self>) {
        if self.editing != editing {
            self.editing = editing;
            cx.notify();
        }
    }

    fn reset_layout(&mut self, cx: &mut Context<Self>) {
        let layout = ChromeLayout::default_for(&self.catalog);
        if layout != self.layout {
            self.layout = layout;
            self.persist_layout(cx);
            cx.notify();
        }
    }

    fn drop_item(
        &mut self,
        id: &str,
        zone: ChromeZone,
        target: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if self.layout.move_item(&self.catalog, id, zone, target) {
            self.persist_layout(cx);
            cx.notify();
        }
    }

    fn set_item_hidden(&mut self, id: &str, hidden: bool, cx: &mut Context<Self>) {
        if self.layout.set_hidden(&self.catalog, id, hidden) {
            self.persist_layout(cx);
            cx.notify();
        }
    }

    fn persist_layout(&mut self, cx: &mut App) {
        let value = self.layout.to_value();
        self.stored_layout = Some(value.clone());
        let handler = Rc::clone(&self.on_layout_change);
        handler(value, cx);
    }

    fn item(&self, id: &str) -> Option<Item<'_>> {
        if let Some(route) = self.routes.iter().find(|route| route.id.as_str() == id) {
            return Some(Item::Route(route));
        }
        if let Some(action) = self.actions.iter().find(|action| action.layout_id == id) {
            return Some(Item::Action(action));
        }
        self.widgets
            .iter()
            .find(|widget| widget.id == id)
            .map(Item::Widget)
    }

    fn label(&self, key: &str, cx: &App) -> SharedString {
        SharedString::from(self.translator.read(cx).text(key).to_owned())
    }

    fn resolve_label(&self, label: &ShellLabel, cx: &App) -> SharedString {
        match label {
            ShellLabel::Key(key) => self.label(key, cx),
            ShellLabel::Text(text) => text.clone(),
        }
    }

    /// 渲染一个条目；隐藏的路由 / 动作、（非编辑模式下）被用户隐藏或无内容的条目返回 `None`。
    /// 编辑模式下被用户隐藏的条目照常摆放但变淡，便于取消隐藏。
    fn place_item(
        &self,
        id: &SharedString,
        zone: ChromeZone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Placed> {
        let rail = zone.family() == ChromeFamily::Rail;
        let item = self.item(id)?;
        let hidden = self.layout.is_hidden(id);
        if hidden && !self.editing {
            let revealed = match &item {
                Item::Widget(widget) => widget.reveal.as_ref().is_some_and(|reveal| reveal(cx)),
                Item::Route(_) | Item::Action(_) => false,
            };
            if !revealed {
                return None;
            }
        }
        let (element, selected, anchors_rail, label) = match item {
            Item::Route(route) => {
                if !route.visible {
                    return None;
                }
                let selected = self.active_route.as_ref() == Some(&route.id);
                let element = if rail {
                    self.rail_route_button(route, selected, window, cx)
                } else {
                    self.inline_route_button(route, zone, selected, window, cx)
                };
                (element, selected, route.optional, route.label.clone())
            }
            Item::Action(action) => {
                if !action.visible {
                    return None;
                }
                let element = if rail {
                    self.rail_action_button(action, cx)
                } else {
                    self.inline_action_button(action, zone, cx)
                };
                (
                    element,
                    false,
                    action.optional,
                    ShellLabel::Key(action.label_key),
                )
            }
            Item::Widget(widget) => {
                let render = Rc::clone(&widget.render);
                let in_scope = widget
                    .route
                    .as_ref()
                    .is_none_or(|route| self.active_route.as_ref() == Some(route));
                let rendered = if in_scope {
                    render(zone, window, cx)
                } else {
                    None
                };
                let element = match rendered {
                    Some(element) if rail => rail_tile().child(element).into_any_element(),
                    Some(element) => element,
                    None if self.editing => self.widget_placeholder(widget, rail, cx),
                    None => return None,
                };
                (element, false, true, ShellLabel::Key(widget.label_key))
            }
        };
        let element = if self.editing {
            let element = if hidden {
                div()
                    .opacity(HIDDEN_ITEM_OPACITY)
                    .child(element)
                    .into_any_element()
            } else {
                element
            };
            self.editable(
                id,
                zone,
                element,
                hidden,
                self.resolve_label(&label, cx),
                cx,
            )
        } else if zone.family() == ChromeFamily::Title {
            // 顶栏条目拦截左键按下：否则冒泡到标题栏开始拖动窗口。
            div()
                .flex_none()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(element)
                .into_any_element()
        } else {
            element
        };
        Some(Placed {
            element,
            selected,
            anchors_rail,
        })
    }

    fn place_zone(
        &self,
        zone: ChromeZone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Placed> {
        self.layout
            .items(zone)
            .iter()
            .filter_map(|id| self.place_item(id, zone, window, cx))
            .collect()
    }

    /// 编辑模式的条目外壳：顶层遮罩接管指针（条目自身的点击 / 菜单失效），可拖动，
    /// 也是落点（占据该条目的位置）；可隐藏的条目右上角另有隐藏 / 显示按钮。
    fn editable(
        &self,
        id: &SharedString,
        zone: ChromeZone,
        element: AnyElement,
        hidden: bool,
        label: SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let accent = tokens.colors.primary;
        let family = zone.family();
        let spec = self.catalog.iter().find(|spec| spec.id == *id);
        let families = spec.map_or(ChromeFamilies::default(), |spec| spec.families);
        let hideable = spec.is_some_and(|spec| spec.hideable);
        let drag = ChromeDrag {
            id: id.clone(),
            label,
            families,
        };
        let target = id.clone();
        div()
            .relative()
            .flex_none()
            // 拦截左键按下：顶栏里不触发窗口拖动（遮罩的拖拽监听先于此执行）。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(element)
            .child(
                div()
                    .id(SharedString::from(format!("shell-chrome-edit-{id}")))
                    .absolute()
                    .inset_0()
                    .occlude()
                    .cursor_grab()
                    .rounded(tokens.radius.sm)
                    .border_1()
                    .border_dashed()
                    .border_color(tokens.colors.border)
                    .hover(move |style| style.border_color(accent))
                    .drag_over::<ChromeDrag>(move |style, drag, _, _| {
                        if drag.families.contains(family) {
                            style.border_color(accent).bg(accent.opacity(0.2))
                        } else {
                            style
                        }
                    })
                    .on_drop(cx.listener(move |this, drag: &ChromeDrag, _, cx| {
                        this.drop_item(&drag.id, zone, Some(&target), cx);
                    }))
                    .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone())),
            )
            .when(hideable, |this| {
                this.child(self.visibility_toggle(id, hidden, cx))
            })
            .into_any_element()
    }

    /// 条目右上角的隐藏 / 显示按钮：位于拖拽遮罩之上，按下不触发拖动。
    fn visibility_toggle(
        &self,
        id: &SharedString,
        hidden: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let accent = tokens.colors.primary;
        let tooltip = self.label(
            if hidden {
                "chromeLayoutShowItem"
            } else {
                "chromeLayoutHideItem"
            },
            cx,
        );
        let target = id.clone();
        let offset = VISIBILITY_TOGGLE_SIZE / 3.;
        div()
            .id(SharedString::from(format!("shell-chrome-visibility-{id}")))
            .absolute()
            .top(-offset)
            .right(-offset)
            .size(VISIBILITY_TOGGLE_SIZE)
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .cursor_pointer()
            .rounded_full()
            .border_1()
            .border_color(tokens.colors.border)
            .bg(tokens.colors.surface)
            .text_color(if hidden {
                accent
            } else {
                tokens.colors.muted_foreground
            })
            .hover(move |style| style.border_color(accent).text_color(accent))
            .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_item_hidden(&target, !hidden, cx);
            }))
            .child(
                Icon::new(if hidden {
                    IconName::EyeOff
                } else {
                    IconName::Eye
                })
                .size(extended.icon.sm),
            )
            .into_any_element()
    }

    /// 编辑模式下当前无内容的控件：显示名称，保证仍可被拖走。
    fn widget_placeholder(
        &self,
        widget: &ShellWidget,
        rail: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let extended = theme.extended();
        let muted = theme.tokens().colors.muted_foreground;
        if rail {
            return rail_tile()
                .child(
                    Icon::new(FluxIcon::GripVertical)
                        .size(extended.icon.sm)
                        .text_color(muted),
                )
                .into_any_element();
        }
        div()
            .flex_none()
            .px(theme.tokens().spacing.xs)
            .text_size(extended.caption.size)
            .line_height(extended.caption.line_height)
            .text_color(muted)
            .child(self.label(widget.label_key, cx))
            .into_any_element()
    }

    fn rail_route_button(
        &self,
        route: &ShellRoute,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let icon_size = active_theme(cx).extended().icon.lg + ACTIVITY_ICON_EXTRA;
        let icon_color = color_transition(
            route.button_id.clone(),
            nav_icon_color(selected, cx),
            window,
            cx,
        );
        let label = self.resolve_label(&route.label, cx);
        let tooltip_label = label.clone();
        let route_id = route.id.clone();
        rail_tile()
            .id(route.tooltip_id.clone())
            .tooltip(move |window, cx| tooltip_view(tooltip_label.clone(), window, cx))
            .child(
                activity_button(
                    route.button_id.clone(),
                    label,
                    route.icon.clone().size(icon_size).text_color(icon_color),
                    selected,
                    ACTIVITY_BUTTON_SIZE,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.navigate(route_id.clone(), cx))),
            )
            .into_any_element()
    }

    /// 顶栏 / 状态栏里的路由按钮：尺寸随承载条，选中底直接画在按钮下（横向没有滑动底块）。
    fn inline_route_button(
        &self,
        route: &ShellRoute,
        zone: ChromeZone,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let radius = theme.components().nav_item_radius;
        let selected_bg = theme.extended().colors.nav_selected;
        let (button_size, icon_size) = inline_metrics(zone, cx);
        let icon_color = color_transition(
            route.button_id.clone(),
            nav_icon_color(selected, cx),
            window,
            cx,
        );
        let label = self.resolve_label(&route.label, cx);
        let tooltip_label = label.clone();
        let route_id = route.id.clone();
        div()
            .id(route.tooltip_id.clone())
            .flex_none()
            .rounded(radius)
            .when(selected, |this| this.bg(selected_bg))
            .tooltip(move |window, cx| tooltip_view(tooltip_label.clone(), window, cx))
            .child(
                activity_button(
                    route.button_id.clone(),
                    label,
                    route.icon.clone().size(icon_size).text_color(icon_color),
                    selected,
                    button_size,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.navigate(route_id.clone(), cx))),
            )
            .into_any_element()
    }

    fn rail_action_button(&self, action: &ShellAction, cx: &mut Context<Self>) -> AnyElement {
        let icon_size = active_theme(cx).extended().icon.lg + ACTIVITY_ICON_EXTRA;
        self.action_element(action, rail_tile(), ACTIVITY_BUTTON_SIZE, icon_size, cx)
    }

    fn inline_action_button(
        &self,
        action: &ShellAction,
        zone: ChromeZone,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (button_size, icon_size) = inline_metrics(zone, cx);
        let frame = div().flex_none().flex().items_center().justify_center();
        self.action_element(action, frame, button_size, icon_size, cx)
    }

    /// 动作按钮：标准图标按钮按给定尺寸绘制；自绘视图（账户头像）保持自身尺寸。
    fn action_element(
        &self,
        action: &ShellAction,
        frame: gpui::Div,
        button_size: Pixels,
        icon_size: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (tooltip_id, icon, handler) = match &action.content {
            ShellActionContent::View(view) => return frame.child(view.clone()).into_any_element(),
            ShellActionContent::Button {
                tooltip_id,
                icon,
                handler,
            } => (*tooltip_id, Icon::clone(icon), Rc::clone(handler)),
        };
        let label = self.label(action.label_key, cx);
        let tooltip_label = label.clone();
        let icon_color = active_theme(cx).tokens().colors.muted_foreground;
        frame
            .id(tooltip_id)
            .tooltip(move |window, cx| tooltip_view(tooltip_label.clone(), window, cx))
            .child(
                activity_button(
                    action.button_id,
                    label,
                    icon.size(icon_size).text_color(icon_color),
                    false,
                    button_size,
                    cx,
                )
                .on_click(move |_, window, cx| handler(window, cx)),
            )
            .into_any_element()
    }

    /// 右键菜单：进入 / 完成布局编辑、恢复默认布局。
    fn chrome_menu(
        &self,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let shell = cx.entity().downgrade();
        let editing = self.editing;
        let toggle_label = self.label(
            if editing {
                "chromeLayoutDone"
            } else {
                "chromeLayoutCustomize"
            },
            cx,
        );
        let reset_label = self.label("chromeLayoutReset", cx);
        move |menu, _, _| {
            let toggle_shell = shell.clone();
            let reset_shell = shell.clone();
            menu.item(
                PopupMenuItem::new(toggle_label.clone()).on_click(move |_, _, cx| {
                    let Ok(()) = toggle_shell.update(cx, |this, cx| this.set_editing(!editing, cx))
                    else {
                        // 窗口已关闭，无需切换。
                        return;
                    };
                }),
            )
            .item(
                PopupMenuItem::new(reset_label.clone()).on_click(move |_, _, cx| {
                    let Ok(()) = reset_shell.update(cx, |this, cx| this.reset_layout(cx)) else {
                        // 窗口已关闭，无需重置。
                        return;
                    };
                }),
            )
        }
    }

    /// 横向区域的容器：编辑模式下画虚线框并作为「追加到末尾」的落点。
    fn horizontal_zone(
        &self,
        zone: ChromeZone,
        placed: Vec<Placed>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        if placed.is_empty() && !self.editing {
            return None;
        }
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let spacing = tokens.spacing;
        let accent = tokens.colors.primary;
        let family = zone.family();
        let control = match family {
            ChromeFamily::Title => theme.density().toolbar_button,
            ChromeFamily::Rail | ChromeFamily::Status => theme.density().status_control,
        };
        let gap = match family {
            ChromeFamily::Title => spacing.sm,
            ChromeFamily::Rail | ChromeFamily::Status => spacing.xs,
        };
        let row = h_flex()
            .id(zone.key())
            .flex_none()
            .min_w_0()
            .items_center()
            .gap(gap)
            .min_h(control)
            .children(placed.into_iter().map(|placed| placed.element));
        Some(if self.editing {
            row.min_w(EMPTY_ZONE_MIN_WIDTH)
                .p(spacing.xxs)
                .rounded(tokens.radius.sm)
                .border_1()
                .border_dashed()
                .border_color(tokens.colors.border)
                .drag_over::<ChromeDrag>(move |style, drag, _, _| {
                    if drag.families.contains(family) {
                        style.border_color(accent).bg(accent.opacity(0.08))
                    } else {
                        style
                    }
                })
                .on_drop(cx.listener(move |this, drag: &ChromeDrag, _, cx| {
                    this.drop_item(&drag.id, zone, None, cx);
                }))
        } else {
            row
        })
    }

    /// 顶栏区域（左侧在页面插槽之前，右侧在其后）。
    pub(crate) fn render_title_zone(
        &self,
        zone: ChromeZone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let placed = self.place_zone(zone, window, cx);
        let menu = self.chrome_menu(cx);
        self.horizontal_zone(zone, placed, cx)
            .map(|row| row.context_menu(menu).into_any_element())
    }

    /// 状态栏：两个区域都没有可见条目时整条收起（编辑模式除外）。
    pub(crate) fn render_status_bar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let leading = self.place_zone(ChromeZone::StatusLeading, window, cx);
        let trailing = self.place_zone(ChromeZone::StatusTrailing, window, cx);
        if leading.is_empty() && trailing.is_empty() && !self.editing {
            return None;
        }
        let leading = self.horizontal_zone(ChromeZone::StatusLeading, leading, cx);
        let trailing = self.horizontal_zone(ChromeZone::StatusTrailing, trailing, cx);
        let theme = active_theme(cx);
        let spacing = theme.tokens().spacing;
        let extended = theme.extended();
        Some(
            h_flex()
                .id("shell-status-bar")
                .w_full()
                .h(theme.density().status_bar)
                .flex_none()
                .items_center()
                .justify_between()
                .gap(spacing.md)
                .px(spacing.sm)
                .bg(extended.colors.chrome)
                .border_t(extended.stroke.thin)
                .border_color(extended.colors.hairline)
                .children(leading)
                .child(div().flex_1())
                .children(trailing)
                .children(self.status_accessory.clone())
                .context_menu(self.chrome_menu(cx))
                .into_any_element(),
        )
    }

    /// 活动栏：只有宿主控件或可选路由 / 动作可见时才渲染（只剩固定入口时整体收起），
    /// 编辑模式下始终渲染以便拖入。
    pub(crate) fn render_activity_bar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let top = self.place_zone(ChromeZone::RailTop, window, cx);
        let bottom = self.place_zone(ChromeZone::RailBottom, window, cx);
        let anchored = top.iter().chain(&bottom).any(|placed| placed.anchors_rail);
        if !anchored && !self.editing {
            return None;
        }
        let top = self.rail_zone(ChromeZone::RailTop, top, cx);
        let bottom = self.rail_zone(ChromeZone::RailBottom, bottom, cx);
        let theme = active_theme(cx);
        let spacing = theme.tokens().spacing;
        let extended = theme.extended();
        // 与侧栏同为 chrome 底，靠右侧 hairline 结构线分界（与侧栏|内容的 resizable 把手同色同粗）。
        Some(
            v_flex()
                .id("shell-activity-bar")
                .h_full()
                .w(ACTIVITY_RAIL_WIDTH)
                .flex_none()
                .justify_between()
                .gap(spacing.sm)
                .bg(extended.colors.chrome)
                .border_r(extended.stroke.thin)
                .border_color(extended.colors.hairline)
                .pt(spacing.sm)
                .pb(spacing.sm)
                .child(top)
                .child(bottom)
                .context_menu(self.chrome_menu(cx))
                .into_any_element(),
        )
    }

    /// 活动栏的一个区域：选中路由的中性底是一块随选中项滑动的底块（按钮自身不画选中底）；
    /// 编辑模式下不画滑块（条目外壳会改变位置），改画虚线框落点。
    fn rail_zone(
        &self,
        zone: ChromeZone,
        placed: Vec<Placed>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let gap = tokens.spacing.xs;
        let highlight = (!self.editing)
            .then(|| placed.iter().position(|placed| placed.selected))
            .flatten()
            .map(|index| {
                let inset = (ACTIVITY_RAIL_WIDTH - ACTIVITY_BUTTON_SIZE) / 2.;
                let top = (ACTIVITY_TILE_HEIGHT + gap) * index as f32
                    + (ACTIVITY_TILE_HEIGHT - ACTIVITY_BUTTON_SIZE) / 2.;
                SlidingHighlight::new(
                    SharedString::from(format!("shell-activity-selection-{}", zone.key())),
                    Bounds::new(
                        point(inset, top),
                        size(ACTIVITY_BUTTON_SIZE, ACTIVITY_BUTTON_SIZE),
                    ),
                    theme.extended().colors.nav_selected,
                )
                .radius(theme.components().nav_item_radius)
            });
        let column = v_flex()
            .id(zone.key())
            .relative()
            .items_center()
            .gap(gap)
            .children(highlight)
            .children(placed.into_iter().map(|placed| placed.element));
        if !self.editing {
            return column.into_any_element();
        }
        let accent = tokens.colors.primary;
        column
            .w(ACTIVITY_RAIL_WIDTH - RAIL_ZONE_INSET * 2.)
            .mx(RAIL_ZONE_INSET)
            .min_h(ACTIVITY_TILE_HEIGHT)
            .py(tokens.spacing.xxs)
            .rounded(tokens.radius.sm)
            .border_1()
            .border_dashed()
            .border_color(tokens.colors.border)
            .drag_over::<ChromeDrag>(move |style, drag, _, _| {
                if drag.families.contains(ChromeFamily::Rail) {
                    style.border_color(accent).bg(accent.opacity(0.08))
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |this, drag: &ChromeDrag, _, cx| {
                this.drop_item(&drag.id, zone, None, cx);
            }))
            .into_any_element()
    }

    /// 编辑模式提示条：说明 + 恢复默认 + 完成；Esc 同样退出。
    pub(crate) fn render_edit_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.editing {
            return None;
        }
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        Some(
            h_flex()
                .w_full()
                .flex_none()
                .items_center()
                .gap(tokens.spacing.sm)
                .px(tokens.spacing.md)
                .py(tokens.spacing.xs)
                .bg(extended.colors.chrome)
                .border_b(extended.stroke.thin)
                .border_color(extended.colors.hairline)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(extended.caption.size)
                        .line_height(extended.caption.line_height)
                        .text_color(tokens.colors.muted_foreground)
                        .child(self.label("chromeLayoutHint", cx)),
                )
                .child(
                    button(
                        "shell-layout-reset",
                        self.label("chromeLayoutReset", cx),
                        ButtonVariant::Ghost,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.reset_layout(cx))),
                )
                .child(
                    button(
                        "shell-layout-done",
                        self.label("chromeLayoutDone", cx),
                        ButtonVariant::Primary,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.set_editing(false, cx))),
                )
                .into_any_element(),
        )
    }

    /// 编辑模式下 Esc 退出。
    pub(crate) fn on_edit_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing && event.keystroke.key == "escape" {
            self.set_editing(false, cx);
            cx.stop_propagation();
        }
    }
}

/// 活动栏按钮位：轨宽 × 按钮高，内容居中。
fn rail_tile() -> gpui::Div {
    div()
        .w(ACTIVITY_RAIL_WIDTH)
        .h(ACTIVITY_TILE_HEIGHT)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
}

/// 横向区域里图标按钮的边长与图标尺寸：顶栏用工具栏按钮，状态栏用状态栏按钮。
fn inline_metrics(zone: ChromeZone, cx: &App) -> (Pixels, Pixels) {
    let theme = active_theme(cx);
    let icon = theme.extended().icon;
    match zone.family() {
        ChromeFamily::Title => (theme.density().toolbar_button, icon.md),
        ChromeFamily::Rail | ChromeFamily::Status => (theme.density().status_control, icon.sm),
    }
}
