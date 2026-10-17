//! FluxDown GPUI 桌面窗口 shell。
//!
//! 本 crate 只提供窗口 chrome、活动栏、路由和内容槽位；业务页面由 app
//! 创建后以 [`ShellRoute`] 注入。

mod assets;
mod view;
mod window_controls;

use fluxdown_ui_theme::active_theme;
use gpui::{
    App, Pixels, Point, SharedString, Window, WindowDecorations, WindowOptions, point, px, size,
};
use gpui_component::TitleBar;

pub use assets::*;
pub use view::*;

/// 交通灯对齐的顶栏高度（逻辑像素）：窗口创建时写入平台窗口选项，是窗口级而非主题值，
/// 取 `density.titleBar` 的默认值（该 token 同样不随界面缩放）。标题栏本身按 token 渲染。
const TITLE_BAR_HEIGHT_PX: f32 = 40.;
/// macOS 交通灯按钮框高度（AppKit 标准窗口按钮 14×16 的高）。
const TRAFFIC_LIGHT_BUTTON_HEIGHT_PX: f32 = 16.;
/// 交通灯纵向偏移：按钮框在顶栏内垂直居中；横向与纵向留白一致。
const TRAFFIC_LIGHT_INSET_PX: f32 = (TITLE_BAR_HEIGHT_PX - TRAFFIC_LIGHT_BUTTON_HEIGHT_PX) / 2.;

/// 交通灯在 [`TITLE_BAR_HEIGHT_PX`] 高的标题栏内垂直居中的位置。
fn traffic_light_position() -> Point<Pixels> {
    point(px(TRAFFIC_LIGHT_INSET_PX), px(TRAFFIC_LIGHT_INSET_PX))
}

/// 标题栏内控件（`density.control` 高）上下各留的空白：默认 28 + 12 + 12 的 40 恰为 `density.title_bar`。
const TITLE_BAR_CONTROL_PADDING_PX: f32 = 6.;

/// 标题栏实际高度：不低于 `density.title_bar`，文字放大使 `density.control` 变高时随之撑高，
/// 默认（控件 28px）恒为 40px。
pub(crate) fn title_bar_height(cx: &App) -> Pixels {
    let density = active_theme(cx).density();
    density
        .title_bar
        .max(density.control + px(TITLE_BAR_CONTROL_PADDING_PX * 2.))
}

/// 交通灯纵向偏移随标题栏高度居中（macOS）；`applied` 记录已应用的值，未变化时不触碰 AppKit。
#[cfg(target_os = "macos")]
pub(crate) fn sync_traffic_light(window: &Window, height: Pixels, applied: &mut Pixels) {
    let inset_y = (height - px(TRAFFIC_LIGHT_BUTTON_HEIGHT_PX)) / 2.;
    if *applied != inset_y {
        *applied = inset_y;
        window.set_traffic_light_position(point(px(TRAFFIC_LIGHT_INSET_PX), inset_y));
    }
}

/// 非 macOS 无交通灯，无需同步。
#[cfg(not(target_os = "macos"))]
pub(crate) fn sync_traffic_light(_window: &Window, _height: Pixels, _applied: &mut Pixels) {}

/// 交通灯初始纵向偏移（默认 40px 标题栏下的居中值）。
pub(crate) const fn initial_traffic_light_y() -> Pixels {
    px(TRAFFIC_LIGHT_INSET_PX)
}

/// macOS 交通灯按钮区域安全避让宽度（逻辑像素）：
/// 左留白（12px）+ 3 个标准交通灯按钮与间距 + 右侧间距，共 80px。
/// 与 gpui-component 标题栏的 macOS 默认左留白 `px(80.)` 及官网 GPUI 预览一致。
pub const MAC_TRAFFIC_LIGHT_WIDTH_PX: f32 = 80.;

/// 标题栏内容的安全左留白：macOS 固定保留交通灯宽度，其他平台采用标准间距。
pub(crate) fn title_bar_left_padding(is_macos: bool, non_mac_padding: Pixels) -> Pixels {
    if is_macos {
        px(MAC_TRAFFIC_LIGHT_WIDTH_PX)
    } else {
        non_mac_padding
    }
}

/// 以 gpui-component `TitleBar` 窗口选项为基础，交通灯改为对齐 shell 标题栏高度。
fn shell_window_options() -> WindowOptions {
    let mut options = TitleBar::window_options();
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.traffic_light_position = Some(traffic_light_position());
    }
    options.window_min_size = Some(size(px(720.), px(520.)));
    options.window_decorations = Some(WindowDecorations::Client);
    options
}

/// 主窗口的系统级标题：产品名，不随语言变化。任务栏 / Alt+Tab / Dock 窗口列表 /
/// 窗口管理器与读屏软件都读它；界面内标题栏由 shell 自绘，不显示此文本。
const MAIN_WINDOW_TITLE: &str = "FluxDown";

/// 构造 FluxDown 主窗口选项。
pub fn main_window_options() -> WindowOptions {
    auxiliary_window_options(MAIN_WINDOW_TITLE)
}

/// 构造使用 FluxDown 自定义标题栏的辅助窗口选项。
pub fn auxiliary_window_options(title: impl Into<SharedString>) -> WindowOptions {
    let mut options = shell_window_options();
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some(title.into());
    }
    options
}

#[cfg(test)]
mod tests {
    use gpui::{point, px, size};

    use super::{auxiliary_window_options, main_window_options};

    #[test]
    fn main_window_preserves_custom_titlebar_platform_contract() {
        let options = main_window_options();

        assert!(options.app_owns_titlebar_drag);
        assert_eq!(
            options
                .titlebar
                .as_ref()
                .and_then(|titlebar| titlebar.traffic_light_position),
            Some(point(px(12.), px(12.)))
        );
        assert_eq!(
            options
                .titlebar
                .as_ref()
                .and_then(|titlebar| titlebar.title.as_deref()),
            Some("FluxDown")
        );
        assert_eq!(options.window_min_size, Some(size(px(720.), px(520.))));
        assert_eq!(
            options.window_decorations,
            Some(gpui::WindowDecorations::Client)
        );
    }

    #[test]
    fn auxiliary_window_preserves_custom_titlebar_platform_contract() {
        let options = auxiliary_window_options("Settings");

        assert!(options.app_owns_titlebar_drag);
        assert_eq!(
            options
                .titlebar
                .as_ref()
                .and_then(|titlebar| titlebar.title.as_deref()),
            Some("Settings")
        );
        assert_eq!(
            options
                .titlebar
                .as_ref()
                .and_then(|titlebar| titlebar.traffic_light_position),
            Some(point(px(12.), px(12.)))
        );
        assert_eq!(options.window_min_size, Some(size(px(720.), px(520.))));
        assert_eq!(
            options.window_decorations,
            Some(gpui::WindowDecorations::Client)
        );
    }

    #[test]
    fn title_bar_left_padding_reserves_traffic_lights_on_macos() {
        use super::{MAC_TRAFFIC_LIGHT_WIDTH_PX, title_bar_left_padding};

        // macOS 下必须保留 80px 避让区，避免遮挡 AppKit 交通灯
        assert_eq!(
            title_bar_left_padding(true, px(8.)),
            px(MAC_TRAFFIC_LIGHT_WIDTH_PX)
        );
        assert_eq!(title_bar_left_padding(true, px(0.)), px(80.));

        // 非 macOS 下保留传入的常规内边距
        assert_eq!(title_bar_left_padding(false, px(8.)), px(8.));
        assert_eq!(title_bar_left_padding(false, px(0.)), px(0.));

        // 交通灯安全避让宽度须大于交通灯本身占据的范围 (12 + 16*3 = 60px)
        assert!(title_bar_left_padding(true, px(0.)) >= px(64.));
    }
}
