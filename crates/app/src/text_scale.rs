//! 跟随 Windows「辅助功能 → 文本大小」（`UISettings.TextScaleFactor`，100% ~ 225%）。
//!
//! 显示缩放（DPI）由 GPUI 按显示器处理（PerMonitorV2 清单 + `WM_DPICHANGED`），这里只补文本大小：
//! 它是独立的系统设置，Win32 应用须自行读取。启动时在首次装主题前读一次，之后每次窗口激活重读
//! （用户在系统设置里改完切回本应用即生效）；值不变时主题不重装。macOS 没有对应设置，Linux 的
//! 文本缩放在 X11 上已折进 Xft.dpi，故仅 Windows 编入。

use gpui::{App, Entity, Window};

/// 读取系统文本大小并交给主题；读取失败保持当前倍率。
pub(crate) fn sync(cx: &mut App) {
    match ::windows::UI::ViewManagement::UISettings::new()
        .and_then(|settings| settings.TextScaleFactor())
    {
        Ok(scale) => fluxdown_ui_theme::set_system_text_scale(scale, cx),
        Err(error) => log::warn!("failed to read the Windows text scale factor: {error}"),
    }
}

/// 窗口激活时重读：系统没有可在 Win32 窗口上可靠收到的文本大小变更通知。
pub(crate) fn follow_on_activation<V: 'static>(
    view: &Entity<V>,
    window: &mut Window,
    cx: &mut App,
) {
    view.update(cx, |_, cx| {
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                // 回调期间本窗口被借用，主题重装要刷新全部窗口：等当前更新栈退出后再做。
                cx.defer(sync);
            }
        })
        .detach();
    });
}
