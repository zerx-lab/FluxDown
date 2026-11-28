//! Windows / Linux 上按需裁剪窗口按钮的自绘标题栏（macOS 使用系统交通灯，不走本组件）：
//! - 固定尺寸辅助窗口：只提供最小化与关闭，不画最大化。gpui-component 的 `TitleBar` 内部的
//!   `WindowControls` 是私有的，并且在 Windows / Linux 上总是绘制最大化按钮；对
//!   `is_resizable = false` 的窗口，最大化区域在 Windows 上会退化为拖动区、点了没有任何效果。
//! - 主窗口打开右侧面板时的主区顶栏：不画任何按钮——窗口按钮随窗口右上角移到面板顶栏
//!   （面板顶栏用 gpui-component 的 `TitleBar`）。
//!
//! 按同一版本 `TitleBar` 的结构与行为重绘（高度、按钮宽度、图标尺寸、悬停配色与之一致）：
//! - Windows：按钮标记 `WindowControlArea::Min / Close`，悬停与点击由原生非客户区命中测试处理；
//!   整条拖动区标记 `WindowControlArea::Drag`（双击最大化同样由系统处理）。
//! - Linux（客户端装饰）：点击直接调用窗口 API，拖动用 `start_window_move`，可调尺寸的窗口
//!   双击拖动区最大化；服务端装饰时窗口管理器自带标题栏，不重复绘制。

use gpui::{
    AnyElement, App, Decorations, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Pixels, RenderOnce, StatefulInteractiveElement as _, StyleRefinement, Styled, Window,
    WindowControlArea, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, InteractiveElementExt as _, Sizable as _, StyledExt as _,
    TITLE_BAR_HEIGHT, h_flex, window_corner_radii,
};

/// 单个控制按钮的宽度：与 gpui-component 的窗口控制按钮一致。
const CONTROL_WIDTH: Pixels = TITLE_BAR_HEIGHT;

#[derive(Clone, Copy)]
enum Control {
    Minimize,
    Close,
}

impl Control {
    fn id(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Close => "close",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Minimize => IconName::WindowMinimize,
            Self::Close => IconName::WindowClose,
        }
    }

    fn area(self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Close => WindowControlArea::Close,
        }
    }
}

fn control_button(control: Control, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let is_close = matches!(control, Control::Close);
    let (hover_fg, hover_bg, active_bg) = if is_close {
        (theme.danger_foreground, theme.danger, theme.danger_active)
    } else {
        (
            theme.secondary_foreground,
            theme.secondary_hover,
            theme.secondary_active,
        )
    };

    div()
        .id(control.id())
        .flex()
        .w(CONTROL_WIDTH)
        .h_full()
        .flex_shrink_0()
        .justify_center()
        .content_center()
        .items_center()
        .text_color(theme.foreground)
        .hover(|style| style.bg(hover_bg).text_color(hover_fg))
        .active(|style| style.bg(active_bg).text_color(hover_fg))
        .when(cfg!(target_os = "windows"), |this| {
            this.window_control_area(control.area())
        })
        .when(cfg!(target_os = "linux"), |this| {
            this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                match control {
                    Control::Minimize => window.minimize_window(),
                    Control::Close => window.remove_window(),
                }
            })
        })
        .child(Icon::new(control.icon()).small())
}

/// 拖动手势状态（Linux：按下后移动才发起窗口移动）。
struct DragState {
    should_move: bool,
}

/// 标题栏右端的窗口按钮。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Controls {
    /// 最小化 + 关闭：固定尺寸辅助窗口。
    MinimizeClose,
    /// 不画：按钮由同一窗口右上角的另一条标题栏承担（主窗口打开右侧面板时）。
    Elsewhere,
}

/// 按需裁剪窗口按钮的标题栏；子元素放在拖动区内，样式经 [`Styled`] 覆盖默认外观。
#[derive(IntoElement)]
pub(crate) struct CustomTitleBar {
    controls: Controls,
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl CustomTitleBar {
    /// 固定尺寸窗口：只含最小化 + 关闭。
    pub(crate) fn fixed_size() -> Self {
        Self::with_controls(Controls::MinimizeClose)
    }

    /// 可调尺寸窗口左侧的顶栏：不画按钮（窗口按钮在右侧面板顶栏），Linux 双击最大化。
    pub(crate) fn without_controls() -> Self {
        Self::with_controls(Controls::Elsewhere)
    }

    fn with_controls(controls: Controls) -> Self {
        Self {
            controls,
            style: StyleRefinement::default(),
            children: Vec::new(),
        }
    }
}

impl Styled for CustomTitleBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for CustomTitleBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for CustomTitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_linux = cfg!(target_os = "linux");
        let is_client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });
        let state = window.use_state(cx, |_, _| DragState { should_move: false });
        // 服务端装饰（X11 无合成器 / Wayland 合成器授予）时窗口管理器已画了控制按钮。
        let draw_controls = self.controls == Controls::MinimizeClose
            && !cfg!(target_os = "macos")
            && (!is_linux || is_client_decorated);
        // 只有左上角是窗口圆角：右端要么是窗口按钮（固定尺寸窗口另算右上角），要么紧邻右侧面板。
        let radii = window_corner_radii(window, cx);
        let zoomable = self.controls == Controls::Elsewhere && is_linux;
        let supported = window.window_controls();

        div().flex_shrink_0().child(
            div()
                .id("title-bar")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .h(TITLE_BAR_HEIGHT)
                .rounded_tl(radii.top_left)
                .when(self.controls == Controls::MinimizeClose, |this| {
                    this.rounded_tr(radii.top_right)
                })
                .border_b_1()
                .border_color(cx.theme().title_bar_border)
                .bg(cx.theme().title_bar)
                .refine_style(&self.style)
                .when(zoomable, |this| {
                    this.on_double_click(|_, window, _| window.zoom_window())
                })
                .on_mouse_down_out(window.listener_for(&state, |state, _, _, _| {
                    state.should_move = false;
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    window.listener_for(&state, |state, _, _, _| {
                        state.should_move = true;
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    window.listener_for(&state, |state, _, _, _| {
                        state.should_move = false;
                    }),
                )
                .on_mouse_move(window.listener_for(&state, |state, _, window, _| {
                    if state.should_move {
                        state.should_move = false;
                        window.start_window_move();
                    }
                }))
                .child(
                    h_flex()
                        .id("bar")
                        .h_full()
                        .flex_1()
                        .min_w_0()
                        .window_control_area(WindowControlArea::Drag)
                        .when(is_linux && is_client_decorated, |this| {
                            this.child(
                                div()
                                    .top_0()
                                    .left_0()
                                    .absolute()
                                    .size_full()
                                    .on_mouse_down(MouseButton::Right, |event, window, _| {
                                        window.show_window_menu(event.position)
                                    }),
                            )
                        })
                        .children(self.children),
                )
                .when(draw_controls, |this| {
                    this.child(
                        h_flex()
                            .id("window-controls")
                            .items_center()
                            .flex_shrink_0()
                            .h_full()
                            .when(supported.minimize, |this| {
                                this.child(control_button(Control::Minimize, cx))
                            })
                            .child(control_button(Control::Close, cx)),
                    )
                }),
        )
    }
}
