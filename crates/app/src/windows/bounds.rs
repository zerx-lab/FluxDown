//! 窗口边界记忆。
//!
//! - 主窗口 / 设置窗口 / 进度与完成窗口记「位置 + 尺寸 + 最大化 + 所在显示器」。
//!   其余可调尺寸的辅助窗口只记尺寸，位置仍按各自规则居中（新建下载 / BT 选择跟随主窗口
//!   所在显示器）。同类窗口共用一条记录：所有任务详情窗口共享一份尺寸。
//! - gpui 的窗口坐标以「打开时 `display_id` 指定的显示器（缺省主显示器）」为基准解释：macOS
//!   是相对该显示器的局部坐标，Windows / X11 是全局坐标。位置因此必须连同显示器 UUID（跨重启
//!   稳定；`DisplayId` 不稳定）一起记录，恢复时传回 `display_id`，并只对那台显示器校验可见性。
//! - 只在边界真实变化时写入：gpui 每次窗口激活 / 失焦都会触发边界观察。
//! - 写设备本地偏好（`sync:false`），同键 500ms 防抖；退出时强制落盘在途值。

use std::{collections::HashMap, rc::Rc, sync::Arc, time::Duration};

use gpui::{
    App, Bounds, Context, Entity, Pixels, PlatformDisplay, Size, Window, WindowBounds,
    WindowOptions, point, px, size,
};
use serde_json::{Value, json};

use super::{WindowRegistry, patch_local_preference};
use crate::{agent_client::AgentClient, app::Desktop};

const PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);
/// 持久化记录的合理边长范围（逻辑像素），不是显示器工作区或最小窗口尺寸。
const MIN_EDGE: f32 = 100.;
const MAX_EDGE: f32 = 20_000.;

/// 需要记忆边界的窗口类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RememberedWindow {
    Main,
    Settings,
    Progress,
    NewDownload,
    QueueManager,
    TaskDetail,
    GroupDetail,
    BtSelection,
}

impl RememberedWindow {
    const fn pref_key(self) -> &'static str {
        match self {
            Self::Main => "desktop.window.main",
            Self::Settings => "desktop.window.settings",
            Self::Progress => "desktop.window.progress",
            Self::NewDownload => "desktop.window.new_download",
            Self::QueueManager => "desktop.window.queue_manager",
            Self::TaskDetail => "desktop.window.task_detail",
            Self::GroupDetail => "desktop.window.group_detail",
            Self::BtSelection => "desktop.window.bt_selection",
        }
    }

    const fn remembers_position(self) -> bool {
        matches!(self, Self::Main | Self::Settings | Self::Progress)
    }
}

/// 注册表持有的边界状态（按偏好键）。
#[derive(Default)]
pub(super) struct BoundsMemory {
    /// 本进程观察到的最新边界：重开窗口优先于偏好快照，防抖或回执在途时也不回退旧值。
    latest: HashMap<&'static str, Value>,
    /// 尚未落盘的边界（退出时强制写）。
    pending: HashMap<&'static str, Value>,
}

impl BoundsMemory {
    pub(super) fn drain_pending(&mut self) -> Vec<(&'static str, Value)> {
        self.pending.drain().collect()
    }
}

impl WindowRegistry {
    /// 按记忆填写 `options.window_bounds`；无记录或记录失效时以 `default_size` 居中。
    ///
    /// 无记录时尊重 `options.display_id`；记位置的窗口优先回到记录所在的显示器。
    /// 旧记录无 UUID 时仍按主显示器解释；记录屏断开则回调用方选择的屏或主屏。
    /// 尺寸与最小尺寸均受目标显示器可用工作区约束，整个窗口始终可达。
    pub fn restore_bounds(
        which: RememberedWindow,
        options: &mut WindowOptions,
        default_size: Size<Pixels>,
        cx: &App,
    ) {
        let key = which.pref_key();
        let stored = cx
            .global::<Self>()
            .bounds
            .borrow()
            .latest
            .get(key)
            .cloned()
            .or_else(|| Desktop::pref(cx, key));
        let frame = which
            .remembers_position()
            .then(|| stored.as_ref().and_then(parse_frame))
            .flatten();
        let remembered = frame
            .as_ref()
            .and_then(|frame| frame.display.as_deref())
            .and_then(|uuid| {
                cx.displays()
                    .into_iter()
                    .find(|display| display_uuid(display.as_ref()).as_deref() == Some(uuid))
            });
        let preferred = options.display_id.and_then(|id| cx.find_display(id));
        let (display, keep_position) =
            select_display(frame.as_ref(), remembered, preferred, cx.primary_display());
        // 不保留失效 DisplayId；窗口创建和几何计算必须使用同一台显示器。
        options.display_id = display.as_ref().map(|display| display.id());
        let wanted = frame.as_ref().map_or_else(
            || {
                if which.remembers_position() {
                    default_size
                } else {
                    stored.as_ref().and_then(parse_size).unwrap_or(default_size)
                }
            },
            |frame| frame.bounds.size,
        );
        let bounds = match display {
            Some(display) => restore_in_area(
                options,
                wanted,
                frame
                    .as_ref()
                    .filter(|_| keep_position)
                    .map(|frame| frame.bounds),
                display.visible_bounds(),
            ),
            None => Bounds::centered(None, wanted, cx),
        };
        options.window_bounds = Some(restored_window_bounds(
            which,
            bounds,
            frame.as_ref().is_some_and(|frame| frame.maximized),
        ));
    }

    /// 在根视图上挂窗口边界观察：边界真实变化后防抖写入设备本地偏好。
    pub fn persist_bounds<V: 'static>(
        which: RememberedWindow,
        client: Arc<AgentClient>,
        root: &Entity<V>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let key = which.pref_key();
        let memory = Rc::clone(&cx.global::<Self>().bounds);
        let mut last = capture(which, window, cx);
        root.update(cx, |_, cx: &mut Context<V>| {
            cx.observe_window_bounds(window, move |_, window, cx| {
                let value = capture(which, window, cx);
                if value == last {
                    return;
                }
                // gpui-fast 0.1.14 在 Windows 关窗时，HWND 销毁后仍派发一次边界回调，
                // `GetWindowPlacement` 失败得到零尺寸矩形；恢复时读不回的值一律不记，
                // 也不更新 `last`，否则会覆盖用户上一次的有效边界。
                if !is_restorable(which, &value) {
                    log::debug!("ignored unrestorable window bounds {key}: {value}");
                    return;
                }
                last = value.clone();
                {
                    let mut memory = memory.borrow_mut();
                    memory.latest.insert(key, value.clone());
                    memory.pending.insert(key, value.clone());
                }
                let memory = Rc::clone(&memory);
                let client = Arc::clone(&client);
                cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(PERSIST_DEBOUNCE).await;
                    // 期间同键出现了更新的值：交给那次变化自己的定时器写。
                    let pending = {
                        let mut memory = memory.borrow_mut();
                        if memory.pending.get(key) != Some(&value) {
                            return;
                        }
                        let Some(pending) = memory.pending.remove(key) else {
                            return;
                        };
                        pending
                    };
                    if let Err(error) = patch_local_preference(&client, key, pending).await {
                        log::warn!("failed to persist window bounds {key}: {:?}", error.code);
                        let mut memory = memory.borrow_mut();
                        if memory.latest.get(key) == Some(&value) {
                            // 保存失败的当前值仍交给既有退出落盘流程，不能覆盖更新的边界。
                            memory.pending.entry(key).or_insert(value);
                        }
                    }
                })
                .detach();
            })
            .detach();
        });
    }
}

/// 记录的窗口位置。
#[derive(Debug, PartialEq)]
struct StoredFrame {
    bounds: Bounds<Pixels>,
    maximized: bool,
    /// 所在显示器 UUID；旧记录没有，按主显示器解释。
    display: Option<String>,
}

/// 返回目标显示器与是否仍可解释旧位置。无 UUID 的旧记录沿用主屏坐标语义；
/// 有 UUID 但屏幕已断开时，旧坐标不能套用到调用方选择的新屏幕上。
fn select_display<T>(
    frame: Option<&StoredFrame>,
    remembered: Option<T>,
    preferred: Option<T>,
    primary: Option<T>,
) -> (Option<T>, bool) {
    match frame {
        Some(frame) if frame.display.is_none() => (primary, true),
        Some(_) => match remembered {
            Some(display) => (Some(display), true),
            None => (preferred.or(primary), false),
        },
        None => (preferred.or(primary), false),
    }
}

fn restored_window_bounds(
    which: RememberedWindow,
    bounds: Bounds<Pixels>,
    maximized: bool,
) -> WindowBounds {
    if which.remembers_position() && maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

fn restore_in_area(
    options: &mut WindowOptions,
    wanted: Size<Pixels>,
    remembered: Option<Bounds<Pixels>>,
    area: Bounds<Pixels>,
) -> Bounds<Pixels> {
    // 原生平台也执行最小尺寸约束，必须同步裁小，而不只是裁 window_bounds。
    options.window_min_size = options.window_min_size.map(|min| min.min(&area.size));
    match remembered {
        Some(bounds) => fit_bounds(bounds, area, options.window_min_size),
        None => centered_in_area(wanted, area, options.window_min_size),
    }
}

/// 工作区与窗口使用 gpui 的平台坐标，不再转换 DPI 或叠加屏幕 origin：
/// macOS 两者均为屏幕局部坐标；Windows / X11 均为全局逻辑坐标。
fn centered_in_area(
    wanted: Size<Pixels>,
    area: Bounds<Pixels>,
    min_size: Option<Size<Pixels>>,
) -> Bounds<Pixels> {
    let size = fit_size(wanted, area.size, min_size);
    Bounds {
        origin: point(
            area.origin.x + (area.size.width - size.width) / 2.,
            area.origin.y + (area.size.height - size.height) / 2.,
        ),
        size,
    }
}

fn fit_size(
    wanted: Size<Pixels>,
    area: Size<Pixels>,
    min_size: Option<Size<Pixels>>,
) -> Size<Pixels> {
    min_size.map_or(wanted, |min| wanted.max(&min)).min(&area)
}

/// 即使原窗口部分可见，也必须缩小并夹紧整个矩形；仅验证交集会把底部动作留在屏外。
fn fit_bounds(
    bounds: Bounds<Pixels>,
    area: Bounds<Pixels>,
    min_size: Option<Size<Pixels>>,
) -> Bounds<Pixels> {
    let size = fit_size(bounds.size, area.size, min_size);
    Bounds {
        origin: point(
            bounds
                .origin
                .x
                .max(area.origin.x)
                .min(area.origin.x + area.size.width - size.width),
            bounds
                .origin
                .y
                .max(area.origin.y)
                .min(area.origin.y + area.size.height - size.height),
        ),
        size,
    }
}

fn display_uuid(display: &dyn PlatformDisplay) -> Option<String> {
    display.uuid().ok().map(|uuid| uuid.to_string())
}

/// Windows 上最小化时 `window_bounds()` 按还原矩形报告 `Windowed`，
/// 而平台的最大化标志在最小化后仍保持，两者任一为真都应记为最大化。
fn resolve_maximized(bounds_maximized: bool, platform_maximized: bool) -> bool {
    bounds_maximized || platform_maximized
}

/// 当前窗口边界的记录值：记位置的窗口含最大化状态（全屏按最大化记）与所在显示器，
/// 只记尺寸的窗口取还原尺寸。
fn capture(which: RememberedWindow, window: &Window, cx: &App) -> Value {
    let (rect, bounds_maximized) = match window.window_bounds() {
        WindowBounds::Windowed(rect) => (rect, false),
        WindowBounds::Maximized(rect) | WindowBounds::Fullscreen(rect) => (rect, true),
    };
    let maximized =
        which.remembers_position() && resolve_maximized(bounds_maximized, window.is_maximized());
    if !which.remembers_position() {
        return size_value(rect.size);
    }
    let display = window
        .display(cx)
        .and_then(|display| display_uuid(display.as_ref()));
    frame_value(rect, maximized, display.as_deref())
}

/// 记录能否被 [`WindowRegistry::restore_bounds`] 读回；与恢复共用同一套校验。
fn is_restorable(which: RememberedWindow, value: &Value) -> bool {
    if which.remembers_position() {
        parse_frame(value).is_some()
    } else {
        parse_size(value).is_some()
    }
}

fn size_value(size: Size<Pixels>) -> Value {
    json!({ "w": f64::from(size.width), "h": f64::from(size.height) })
}

fn frame_value(rect: Bounds<Pixels>, maximized: bool, display: Option<&str>) -> Value {
    let mut value = json!({
        "x": f64::from(rect.origin.x),
        "y": f64::from(rect.origin.y),
        "w": f64::from(rect.size.width),
        "h": f64::from(rect.size.height),
        "maximized": maximized,
    });
    if let (Some(display), Some(object)) = (display, value.as_object_mut()) {
        object.insert("display".to_owned(), Value::from(display));
    }
    value
}

fn number(value: &Value, key: &str) -> Option<f32> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .map(|number| number as f32)
}

fn parse_size(value: &Value) -> Option<Size<Pixels>> {
    let (w, h) = (number(value, "w")?, number(value, "h")?);
    let edge = MIN_EDGE..MAX_EDGE;
    (edge.contains(&w) && edge.contains(&h)).then(|| size(px(w), px(h)))
}

fn parse_frame(value: &Value) -> Option<StoredFrame> {
    let size = parse_size(value)?;
    let (x, y) = (number(value, "x")?, number(value, "y")?);
    let coordinate = -MAX_EDGE..MAX_EDGE;
    if !coordinate.contains(&x) || !coordinate.contains(&y) {
        return None;
    }
    Some(StoredFrame {
        bounds: Bounds {
            origin: point(px(x), px(y)),
            size,
        },
        maximized: value
            .get("maximized")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        display: value
            .get("display")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, WindowBounds, WindowOptions, point, px, size};
    use serde_json::json;

    use super::{
        RememberedWindow, StoredFrame, centered_in_area, fit_bounds, fit_size, frame_value,
        is_restorable, parse_frame, parse_size, resolve_maximized, restore_in_area,
        restored_window_bounds, select_display, size_value,
    };

    #[test]
    fn minimized_maximized_window_keeps_maximized() {
        // Windows 上最小化的窗口边界报告为 Windowed，但窗口仍带最大化样式。
        assert!(resolve_maximized(false, true));
        assert!(resolve_maximized(true, false));
        assert!(!resolve_maximized(false, false));
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<gpui::Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        }
    }

    #[test]
    fn partially_visible_and_oversized_frames_fit_the_entire_work_area() {
        // 菜单栏位于上方、Dock/任务栏位于左侧和底部，不能只检查 display.bounds。
        let area = rect(80., 30., 1200., 700.);
        assert_eq!(
            fit_bounds(rect(1100., 650., 900., 600.), area, None),
            rect(380., 130., 900., 600.)
        );
        assert_eq!(fit_bounds(rect(100., 100., 1800., 1000.), area, None), area);
        assert_eq!(
            fit_bounds(rect(-100., -40., 800., 600.), area, None),
            rect(80., 30., 800., 600.)
        );
        let valid = rect(100., 50., 800., 600.);
        assert_eq!(fit_bounds(valid, area, None), valid);
    }

    #[test]
    fn negative_global_coordinates_and_vertical_screen_arrangements_stay_reachable() {
        let left = rect(-2560., 30., 2560., 1370.);
        let above = rect(0., -1050., 1920., 1020.);
        let below = rect(0., 1110., 1920., 1020.);
        for area in [left, above, below] {
            let centered = centered_in_area(size(px(600.), px(360.)), area, None);
            assert_eq!(fit_bounds(centered, area, None), centered);
            assert_eq!(centered.center(), area.center());
            assert_eq!(
                fit_bounds(rect(4000., 4000., 600., 360.), area, None),
                rect(
                    f32::from(area.origin.x + area.size.width) - 600.,
                    f32::from(area.origin.y + area.size.height) - 360.,
                    600.,
                    360.,
                )
            );
        }
    }

    #[test]
    fn macos_local_work_area_keeps_menu_bar_and_dock_offsets() {
        // macOS visible_bounds/window bounds 已处于同一屏幕的局部左上坐标；
        // NSScreen 全局坐标即使在主屏上方/左侧也不能再次叠加到窗口 origin。
        let area = rect(70., 24., 1370., 816.);
        assert_eq!(
            centered_in_area(size(px(600.), px(360.)), area, None),
            rect(455., 252., 600., 360.)
        );
        assert_eq!(fit_bounds(rect(-10., -20., 2000., 1200.), area, None), area);
    }

    #[test]
    fn logical_resolution_shrink_and_min_larger_than_area_prioritize_accessibility() {
        let min = Some(size(px(480.), px(280.)));
        let smaller = rect(-900., 40., 850., 600.);
        assert_eq!(
            fit_bounds(rect(-1500., 200., 1200., 900.), smaller, min),
            smaller
        );
        let tiny = rect(40., 30., 400., 220.);
        assert_eq!(
            fit_size(size(px(600.), px(360.)), tiny.size, min),
            tiny.size
        );
        assert_eq!(centered_in_area(size(px(600.), px(360.)), tiny, min), tiny);
        assert_eq!(fit_bounds(rect(0., 0., 100., 100.), tiny, min), tiny);
    }

    #[test]
    fn restore_updates_native_minimum_and_preserves_requested_geometry_when_it_fits() {
        let mut options = WindowOptions {
            window_min_size: Some(size(px(480.), px(280.))),
            ..WindowOptions::default()
        };
        let tiny = rect(80., 24., 400., 220.);
        assert_eq!(
            restore_in_area(&mut options, size(px(600.), px(360.)), None, tiny),
            tiny
        );
        assert_eq!(options.window_min_size, Some(tiny.size));
        let mut options = WindowOptions {
            window_min_size: Some(size(px(480.), px(280.))),
            ..WindowOptions::default()
        };
        let area = rect(-1200., 24., 1200., 800.);
        let saved = rect(-1100., 100., 800., 500.);
        assert_eq!(
            restore_in_area(&mut options, size(px(600.), px(360.)), Some(saved), area),
            saved
        );
        assert_eq!(options.window_min_size, Some(size(px(480.), px(280.))));
        // 无记忆时600×360是默认而非最小尺寸；用户保存更大的尺寸仍然保留。
        assert_eq!(
            restore_in_area(&mut options, size(px(600.), px(360.)), None, area),
            rect(-900., 244., 600., 360.)
        );
    }

    #[test]
    fn display_selection_preserves_saved_screen_and_rejects_disconnected_ids() {
        let saved = StoredFrame {
            bounds: rect(20., 30., 600., 360.),
            maximized: false,
            display: Some("saved-screen".into()),
        };
        assert_eq!(
            select_display(Some(&saved), Some(2), Some(3), Some(1)),
            (Some(2), true)
        );
        assert_eq!(
            select_display(Some(&saved), None, Some(3), Some(1)),
            (Some(3), false)
        );
        // 调用方失效 ID 经 find_display 变为 None，断屏后回主屏且重新居中。
        assert_eq!(
            select_display(Some(&saved), None, None, Some(1)),
            (Some(1), false)
        );
        assert_eq!(
            select_display::<u8>(Some(&saved), None, None, None),
            (None, false)
        );
        let legacy = StoredFrame {
            display: None,
            ..saved
        };
        assert_eq!(
            select_display(Some(&legacy), None, Some(3), Some(1)),
            (Some(1), true)
        );
    }

    #[test]
    fn missing_or_invalid_frames_honor_the_callers_display() {
        for value in [
            json!(null),
            json!({"w": -20, "h": 360}),
            json!({"x": "invalid"}),
        ] {
            let frame = parse_frame(&value);
            assert!(frame.is_none());
            assert_eq!(
                select_display(frame.as_ref(), None, Some(3), Some(1)),
                (Some(3), false)
            );
            assert_eq!(
                select_display(frame.as_ref(), None, None, Some(1)),
                (Some(1), false)
            );
        }
    }

    #[test]
    fn progress_and_completion_share_position_and_maximized_memory() {
        let which = RememberedWindow::Progress;
        assert_eq!(which.pref_key(), "desktop.window.progress");
        assert!(which.remembers_position());
        let bounds = rect(30., 40., 480., 340.);
        assert!(matches!(
            restored_window_bounds(which, bounds, true),
            WindowBounds::Maximized(restored) if restored == bounds
        ));
        assert!(matches!(
            restored_window_bounds(which, bounds, false),
            WindowBounds::Windowed(restored) if restored == bounds
        ));
    }

    #[test]
    fn frame_round_trips_with_display_and_maximized_flag() {
        let value = frame_value(rect(-1800., 20., 900., 600.), true, Some("display-a"));
        assert_eq!(
            parse_frame(&value),
            Some(StoredFrame {
                bounds: rect(-1800., 20., 900., 600.),
                maximized: true,
                display: Some("display-a".to_owned()),
            })
        );
        // 旧记录没有显示器字段。
        let legacy = json!({"x": 10.0, "y": 20.0, "w": 800.0, "h": 600.0});
        assert_eq!(
            parse_frame(&legacy),
            Some(StoredFrame {
                bounds: rect(10., 20., 800., 600.),
                maximized: false,
                display: None,
            })
        );
    }

    #[test]
    fn malformed_or_degenerate_records_are_ignored() {
        assert!(parse_frame(&json!({"x": 1})).is_none());
        assert!(parse_frame(&json!({"x": 0.0, "y": 0.0, "w": 50.0, "h": 600.0})).is_none());
        assert!(parse_frame(&json!({"x": 1e9, "y": 0.0, "w": 800.0, "h": 600.0})).is_none());
        assert_eq!(
            parse_size(&json!({"w": 700.0, "h": 500.0})),
            Some(size(px(700.), px(500.)))
        );
        assert!(parse_size(&json!({"w": 700.0})).is_none());
    }

    #[test]
    fn destroyed_window_captures_are_not_persisted() {
        // Windows 关窗后 `GetWindowPlacement` 失败：全零矩形扣掉边框偏移得到零或负尺寸。
        for degenerate in [rect(0., 0., 0., 0.), rect(8., 0., -16., -39.)] {
            assert!(!is_restorable(
                RememberedWindow::Main,
                &frame_value(degenerate, false, Some("display-a"))
            ));
            assert!(!is_restorable(
                RememberedWindow::TaskDetail,
                &size_value(degenerate.size)
            ));
        }
        let resized = rect(120., 80., 900., 560.);
        assert!(is_restorable(
            RememberedWindow::Settings,
            &frame_value(resized, true, None)
        ));
        assert!(is_restorable(
            RememberedWindow::NewDownload,
            &size_value(resized.size)
        ));
    }

    #[test]
    fn restored_size_fits_display_but_respects_min_size() {
        let area = size(px(1440.), px(900.));
        let min = Some(size(px(640.), px(480.)));
        assert_eq!(
            fit_size(size(px(2000.), px(700.)), area, min),
            size(px(1440.), px(700.))
        );
        assert_eq!(
            fit_size(size(px(300.), px(200.)), area, min),
            size(px(640.), px(480.))
        );
        assert_eq!(
            fit_size(size(px(800.), px(600.)), area, None),
            size(px(800.), px(600.))
        );
    }
}
