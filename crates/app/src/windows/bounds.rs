//! 窗口边界记忆。
//!
//! - 主窗口 / 设置窗口 / 进度与完成窗口记「位置 + 尺寸 + 最大化 + 所在显示器」。
//!   其余可调尺寸的辅助窗口只记尺寸。有主窗口锚点时只取主窗口最近一次有效可还原矩形的中心，
//!   尺寸与夹取仍在该显示器的 `visible_bounds` 内；不取显示器中央（X11 那是整块虚拟桌面，
//!   中央会落在已断电的外接输出上），也不把主窗口尺寸当作区域上限。
//!   同类窗口共用一条记录：所有任务详情窗口共享一份尺寸。
//! - gpui 的窗口坐标以「打开时 `display_id` 指定的显示器（缺省主显示器）」为基准解释：macOS
//!   是相对该显示器的局部坐标，Windows / X11 是全局坐标。位置因此必须连同显示器 UUID（跨重启
//!   稳定；`DisplayId` 不稳定）一起记录，恢复时传回 `display_id`，并只对那台显示器校验可见性。
//!   X11 的 UUID 恒为全零，不是可用身份：不写入、匹配失败。绝对坐标仍是 X 全局坐标，不在
//!   `visible_bounds`（整块虚拟桌面）里居中。最大化且身份不可用时，CreateWindow 用默认尺寸
//!   作还原矩形，原点取已保存原点并夹进当前屏幕，再标记最大化；不用已保存的最大化外框当创建
//!   尺寸（建窗还会把 x 再偏移，整屏宽的窗口会越出单块输出）。缺字段的旧记录仍按主屏坐标解释。
//! - 只在边界真实变化时写入：gpui 每次窗口激活 / 失焦都会触发边界观察。
//! - 写设备本地偏好（`sync:false`），同键 500ms 防抖；退出时强制落盘在途值。

use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, time::Duration};

use gpui::{
    App, Bounds, Context, DisplayId, Entity, Pixels, PlatformDisplay, Point, Size, Window,
    WindowBounds, WindowOptions, point, px, size,
};
use serde_json::{Value, json};

use super::{WindowRegistry, patch_local_preference};
use crate::{agent_client::AgentClient, app::Desktop};

const PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);
/// 持久化记录的合理边长范围（逻辑像素），不是显示器工作区或最小窗口尺寸。
const MIN_EDGE: f32 = 100.;
const MAX_EDGE: f32 = 20_000.;
/// Linux 客户端装饰阴影。观察者挂上时 `client_inset` 可能尚未上报，Cinnamon 仍按这个值
/// 把 `_GTK_FRAME_EXTENTS` 加进 X 窗口，每轴大约扩大两倍。
const LINUX_CLIENT_INSET: f32 = 20.;
/// 帧范围膨胀与请求尺寸的允许误差（逻辑像素）。
const FRAME_EXTENT_TOLERANCE: f32 = 1.;
/// 已落盘尺寸任一边超过默认值的这个倍数，视为帧范围反复计入后的污染记录。
const POLLUTED_SIZE_FACTOR: f32 = 2.;

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

/// 主窗口最近一次有效的可还原边界。只记尺寸的窗口和跟随主窗口的对话框取它的中心居中，
/// 避免在主窗口自己的 update 里再 `handle.update`（那时窗口已被借走）。
/// 取 `window_bounds()` 而不是 `bounds()`：Windows 上最小化的窗口 `bounds()` 约为 (-32000,-32000)。
#[derive(Clone, Copy)]
struct MainAnchor {
    bounds: Bounds<Pixels>,
    display_id: Option<DisplayId>,
}

/// 注册表持有的边界状态（按偏好键）。
#[derive(Default)]
pub(super) struct BoundsMemory {
    /// 本进程观察到的最新边界：重开窗口优先于偏好快照，防抖或回执在途时也不回退旧值。
    latest: HashMap<&'static str, Value>,
    /// 尚未落盘的边界（退出时强制写）。
    pending: HashMap<&'static str, Value>,
    /// 主窗口最近一次有效边界。关窗后保留，对话框仍能落到那块输出上。
    main_anchor: Option<MainAnchor>,
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
    /// 旧记录无 UUID 时仍按主显示器解释。全零 UUID 与显式 null 不能匹配输出，但保留绝对坐标，
    /// 不在 `visible_bounds` 里居中；最大化时创建尺寸用 `default_size`，不用已保存的最大化外框。
    /// 记录屏断开则回调用方选择的屏或主屏。只记尺寸的窗口有可用的主窗口锚点（所在显示器仍连接）时，
    /// 以锚点中心为中心、锚点所在显示器为目标；尺寸与最小尺寸只受该显示器可见区域约束，
    /// 不受主窗口尺寸约束，整个窗口始终可达。
    pub fn restore_bounds(
        which: RememberedWindow,
        options: &mut WindowOptions,
        default_size: Size<Pixels>,
        cx: &App,
    ) {
        let key = which.pref_key();
        let stored_latest = cx.global::<Self>().bounds.borrow().latest.get(key).cloned();
        let anchor = if which.remembers_position() {
            None
        } else {
            main_anchor_target(cx)
        };
        let stored = stored_latest.or_else(|| Desktop::pref(cx, key));
        let frame = which
            .remembers_position()
            .then(|| stored.as_ref().and_then(parse_frame))
            .flatten();
        let remembered = frame.as_ref().and_then(|frame| match &frame.display {
            StoredDisplay::Known(uuid) => cx
                .displays()
                .into_iter()
                .find(|display| display_uuid(display.as_ref()).as_deref() == Some(uuid.as_str())),
            StoredDisplay::Legacy | StoredDisplay::Unavailable => None,
        });
        let preferred = options.display_id.and_then(|id| cx.find_display(id));
        let (display, keep_position) =
            select_display(frame.as_ref(), remembered, preferred, cx.primary_display());
        let wanted = if let Some(frame) = frame.as_ref() {
            create_size(frame, default_size)
        } else if which.remembers_position() {
            default_size
        } else {
            stored
                .as_ref()
                .and_then(parse_size)
                .map(|size| remembered_size(size, default_size))
                .unwrap_or(default_size)
        };
        let saved_rect = frame
            .as_ref()
            .and_then(|frame| remembered_rect(frame, default_size, keep_position));
        let (area, display_id, center) = if let Some((center, anchor_display)) = anchor.as_ref() {
            (
                Some(anchor_display.visible_bounds()),
                Some(anchor_display.id()),
                Some(*center),
            )
        } else if let Some(display) = display.as_ref() {
            (Some(display.visible_bounds()), Some(display.id()), None)
        } else {
            (None, None, None)
        };
        // 不保留失效 DisplayId；窗口创建和几何计算必须使用同一台显示器。
        options.display_id = display_id;
        let bounds = match area {
            Some(area) => restore_in_area(options, wanted, saved_rect, area, center),
            None => saved_rect.unwrap_or_else(|| Bounds::centered(None, wanted, cx)),
        };
        options.window_bounds = Some(restored_window_bounds(
            which,
            bounds,
            frame.as_ref().is_some_and(|frame| frame.maximized),
        ));
    }

    /// 有主窗口锚点时，以主窗口中心放置 `size`，夹进主窗口所在显示器的可见区域，并返回该显示器。
    /// 窗口尺寸不受主窗口尺寸限制。
    ///
    /// 没有可用锚点（主窗口未出现过，或锚点所在显示器已断开）时回退到 `fallback_display`
    /// （缺省主显示器）的可见区域。X11 上那块区域是整块虚拟桌面。
    pub fn place_following_main(
        size: Size<Pixels>,
        fallback_display: Option<DisplayId>,
        cx: &App,
    ) -> (Option<DisplayId>, Bounds<Pixels>) {
        if let Some((center, display)) = main_anchor_target(cx) {
            (
                Some(display.id()),
                centered_at(center, size, display.visible_bounds(), None),
            )
        } else {
            (
                fallback_display,
                Bounds::centered(fallback_display, size, cx),
            )
        }
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
        note_main_anchor(which, window, cx, &memory);
        let mut last = capture(which, window, cx);
        let mut guard = InflationGuard::new(parse_size(&last));
        root.update(cx, |_, cx: &mut Context<V>| {
            cx.observe_window_bounds(window, move |_, window, cx| {
                note_main_anchor(which, window, cx, &memory);
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
                // 合成器把阴影加进 X 窗口后，相对上次大约大 2×inset。这一步不写偏好；
                // 记位置的窗口同时留下上一次有效的位置和最大化标志。膨胀量由 `guard` 记下，
                // 之后落盘的尺寸都会扣掉它，同尺寸的移动不会把膨胀后的外框写进去。
                let Some(value) = guard.admit(which, value, client_inset_px(window)) else {
                    log::debug!("ignored frame-extent size inflation {key}: {last}");
                    return;
                };
                if !is_restorable(which, &value) {
                    log::debug!("ignored unrestorable deflated window bounds {key}: {value}");
                    return;
                }
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

/// 落盘的显示器身份。
#[derive(Debug, PartialEq)]
enum StoredDisplay {
    /// 旧记录没有 `display` 字段，沿用主屏坐标语义。
    Legacy,
    /// 平台报了身份但不可用（X11 全零 UUID，或显式 null）。不能匹配输出，绝对坐标仍保留。
    Unavailable,
    Known(String),
}

/// 记录的窗口位置。
#[derive(Debug, PartialEq)]
struct StoredFrame {
    bounds: Bounds<Pixels>,
    maximized: bool,
    display: StoredDisplay,
}

/// 返回目标显示器与是否仍可解释旧位置。无 UUID 的旧记录沿用主屏坐标语义。
/// 全零 UUID 不能匹配输出，但 X 全局坐标仍然有效，不在 `visible_bounds` 里居中。
/// 有 UUID 但屏幕已断开时，旧坐标不能套用到新屏幕上。
fn select_display<T>(
    frame: Option<&StoredFrame>,
    remembered: Option<T>,
    preferred: Option<T>,
    primary: Option<T>,
) -> (Option<T>, bool) {
    match frame.map(|frame| &frame.display) {
        Some(StoredDisplay::Legacy) => (primary, true),
        Some(StoredDisplay::Known(_)) => match remembered {
            Some(display) => (Some(display), true),
            None => (preferred.or(primary), false),
        },
        Some(StoredDisplay::Unavailable) => (preferred.or(primary), true),
        None => (preferred.or(primary), false),
    }
}

fn unavailable_maximized(frame: &StoredFrame) -> bool {
    frame.maximized && matches!(frame.display, StoredDisplay::Unavailable)
}
/// 交给 CreateWindow 的尺寸。身份不可用的最大化窗口不用已保存外框：
/// 整屏宽的窗口在 x 再偏移后会越出单块输出，被帧范围加过的尺寸同样不能用。
fn create_size(frame: &StoredFrame, default_size: Size<Pixels>) -> Size<Pixels> {
    if unavailable_maximized(frame) {
        default_size
    } else {
        frame.bounds.size
    }
}

/// 仍可解释的旧矩形。最大化且身份不可用时只保留原点，尺寸改用默认值，再由调用方夹进屏幕。
fn remembered_rect(
    frame: &StoredFrame,
    default_size: Size<Pixels>,
    keep_position: bool,
) -> Option<Bounds<Pixels>> {
    if !keep_position {
        return None;
    }
    Some(Bounds {
        origin: frame.bounds.origin,
        size: create_size(frame, default_size),
    })
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
    center: Option<Point<Pixels>>,
) -> Bounds<Pixels> {
    // 原生平台也执行最小尺寸约束，必须同步裁小，而不只是裁 window_bounds。
    options.window_min_size = options.window_min_size.map(|min| min.min(&area.size));
    match (remembered, center) {
        (Some(bounds), _) => fit_bounds(bounds, area, options.window_min_size),
        (None, Some(center)) => centered_at(center, wanted, area, options.window_min_size),
        (None, None) => centered_in_area(wanted, area, options.window_min_size),
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

/// 以 `center` 为中心放置 `wanted`；尺寸与夹取都限于 `area`，中心靠近边缘时整块窗口平移回区域内。
fn centered_at(
    center: Point<Pixels>,
    wanted: Size<Pixels>,
    area: Bounds<Pixels>,
    min_size: Option<Size<Pixels>>,
) -> Bounds<Pixels> {
    let size = fit_size(wanted, area.size, min_size);
    fit_bounds(Bounds::centered_at(center, size), area, min_size)
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
    let text = display.uuid().ok()?.to_string();
    (!is_nil_display_uuid(&text)).then_some(text)
}

fn is_nil_display_uuid(value: &str) -> bool {
    let mut seen_zero = false;
    for character in value.chars() {
        match character {
            '0' => seen_zero = true,
            '-' => {}
            _ => return false,
        }
    }
    seen_zero
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
    frame_value(rect, maximized, &captured_display(window, cx))
}

fn captured_display(window: &Window, cx: &App) -> StoredDisplay {
    let Some(display) = window.display(cx) else {
        return StoredDisplay::Legacy;
    };
    match display.uuid() {
        Ok(uuid) => {
            let text = uuid.to_string();
            if is_nil_display_uuid(&text) {
                StoredDisplay::Unavailable
            } else {
                StoredDisplay::Known(text)
            }
        }
        Err(_) => StoredDisplay::Legacy,
    }
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

fn frame_value(rect: Bounds<Pixels>, maximized: bool, display: &StoredDisplay) -> Value {
    let mut value = json!({
        "x": f64::from(rect.origin.x),
        "y": f64::from(rect.origin.y),
        "w": f64::from(rect.size.width),
        "h": f64::from(rect.size.height),
        "maximized": maximized,
    });
    if let Some(object) = value.as_object_mut() {
        match display {
            StoredDisplay::Legacy => {}
            StoredDisplay::Unavailable => {
                object.insert("display".to_owned(), Value::Null);
            }
            StoredDisplay::Known(uuid) => {
                object.insert("display".to_owned(), Value::from(uuid.clone()));
            }
        }
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
        display: parse_display(value),
    })
}

fn parse_display(value: &Value) -> StoredDisplay {
    match value.get("display") {
        None => StoredDisplay::Legacy,
        Some(Value::Null) => StoredDisplay::Unavailable,
        Some(Value::String(text)) if is_nil_display_uuid(text) => StoredDisplay::Unavailable,
        Some(Value::String(text)) => StoredDisplay::Known(text.clone()),
        Some(_) => StoredDisplay::Legacy,
    }
}

/// 主窗口锚点的中心与所在显示器。
fn main_anchor_target(cx: &App) -> Option<(Point<Pixels>, Rc<dyn PlatformDisplay>)> {
    let anchor = cx.global::<WindowRegistry>().bounds.borrow().main_anchor;
    resolve_anchor(anchor, cx)
}

/// 锚点在主窗口关闭后仍保留，期间外接屏可能被拔掉；平台没报显示器或显示器已不在线时不用锚点。
fn resolve_anchor(
    anchor: Option<MainAnchor>,
    cx: &App,
) -> Option<(Point<Pixels>, Rc<dyn PlatformDisplay>)> {
    let anchor = anchor?;
    let display = cx.find_display(anchor.display_id?)?;
    Some((anchor.bounds.center(), display))
}

fn note_main_anchor(
    which: RememberedWindow,
    window: &Window,
    cx: &App,
    memory: &RefCell<BoundsMemory>,
) {
    if which != RememberedWindow::Main {
        return;
    }
    let bounds = window.window_bounds().get_bounds();
    if f32::from(bounds.size.width) < MIN_EDGE || f32::from(bounds.size.height) < MIN_EDGE {
        return;
    }
    memory.borrow_mut().main_anchor = Some(MainAnchor {
        bounds,
        display_id: window.display(cx).map(|display| display.id()),
    });
}

fn client_inset_px(window: &Window) -> f32 {
    let reported = window.client_inset().map(f32::from).unwrap_or(0.);
    if reported > 0. {
        reported
    } else if cfg!(target_os = "linux") {
        LINUX_CLIENT_INSET
    } else {
        0.
    }
}

/// 合成器把客户端阴影加进窗口后，相对上次观察大约每轴 +2×inset。只拒绝这一步，
/// 任意其它增量视为用户拖拽。只记尺寸的窗口要求两轴同时迈出这一步。
fn is_frame_extent_inflation(previous: Size<Pixels>, next: Size<Pixels>, inset: f32) -> bool {
    let step = inset * 2.;
    if step < 1. {
        return false;
    }
    near(f32::from(next.width) - f32::from(previous.width), step)
        && near(f32::from(next.height) - f32::from(previous.height), step)
}

/// 记位置的窗口：宽或高增加约 2×inset，且另一轴不减。这一步不更新位置和最大化标志。
fn is_position_frame_inflation(previous: Size<Pixels>, next: Size<Pixels>, inset: f32) -> bool {
    let step = inset * 2.;
    if step < 1. {
        return false;
    }
    let width_delta = f32::from(next.width) - f32::from(previous.width);
    let height_delta = f32::from(next.height) - f32::from(previous.height);
    axis_inflated(width_delta, height_delta, step) || axis_inflated(height_delta, width_delta, step)
}

fn axis_inflated(grown: f32, other: f32, step: f32) -> bool {
    near(grown, step) && other >= -FRAME_EXTENT_TOLERANCE
}

fn rejects_frame_extent_step(
    which: RememberedWindow,
    previous: Size<Pixels>,
    next: Size<Pixels>,
    inset: f32,
) -> bool {
    if which.remembers_position() {
        is_position_frame_inflation(previous, next, inset)
    } else {
        is_frame_extent_inflation(previous, next, inset)
    }
}

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() <= FRAME_EXTENT_TOLERANCE
}

/// 帧范围膨胀的追踪。合成器把客户端阴影加进 X 窗口后，窗口会一直比请求尺寸大这一步：
/// 被拒绝的步进累计成偏移，之后落盘的尺寸扣掉它，否则下一次同尺寸的移动会把膨胀后的尺寸写进偏好。
struct InflationGuard {
    /// 上一次观察到的原始（未扣偏移）尺寸。
    observed: Option<Size<Pixels>>,
    /// 已被拒绝的膨胀累计量，按轴。
    offset: Size<Pixels>,
}

impl InflationGuard {
    fn new(initial: Option<Size<Pixels>>) -> Self {
        Self {
            observed: initial,
            offset: Size::default(),
        }
    }

    /// `None`：这一步是帧范围膨胀，不落盘。`Some`：要落盘的值，尺寸已扣掉累计的膨胀量。
    fn admit(&mut self, which: RememberedWindow, mut value: Value, inset: f32) -> Option<Value> {
        let Some(next) = parse_size(&value) else {
            return Some(value);
        };
        let previous = self.observed.replace(next);
        if let Some(previous) = previous
            && rejects_frame_extent_step(which, previous, next, inset)
        {
            self.offset = self.offset + inflated_axes(previous, next, inset);
            return None;
        }
        if self.offset != Size::default() {
            set_size(&mut value, next - self.offset);
        }
        Some(value)
    }
}

/// 一步中真正属于帧范围膨胀的轴向增量；其它轴（用户同时拖拽的部分）不计入。
fn inflated_axes(previous: Size<Pixels>, next: Size<Pixels>, inset: f32) -> Size<Pixels> {
    let step = inset * 2.;
    let axis = |previous: Pixels, next: Pixels| {
        let delta = f32::from(next) - f32::from(previous);
        if near(delta, step) { px(delta) } else { px(0.) }
    };
    size(
        axis(previous.width, next.width),
        axis(previous.height, next.height),
    )
}

fn set_size(value: &mut Value, size: Size<Pixels>) {
    if let Some(object) = value.as_object_mut() {
        object.insert("w".to_owned(), Value::from(f64::from(size.width)));
        object.insert("h".to_owned(), Value::from(f64::from(size.height)));
    }
}

/// 任一边超过默认值 [`POLLUTED_SIZE_FACTOR`] 倍时回退默认尺寸。用户主动拉大到两倍以内仍保留。
fn remembered_size(stored: Size<Pixels>, default_size: Size<Pixels>) -> Size<Pixels> {
    let polluted = f32::from(stored.width) > f32::from(default_size.width) * POLLUTED_SIZE_FACTOR
        || f32::from(stored.height) > f32::from(default_size.height) * POLLUTED_SIZE_FACTOR;
    if polluted { default_size } else { stored }
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, DisplayId, WindowBounds, WindowOptions, point, px, size};
    use serde_json::{Value, json};

    use super::{
        InflationGuard, MainAnchor, RememberedWindow, StoredDisplay, StoredFrame, centered_at,
        centered_in_area, create_size, fit_bounds, fit_size, frame_value,
        is_frame_extent_inflation, is_position_frame_inflation, is_restorable, parse_frame,
        parse_size, remembered_rect, remembered_size, resolve_anchor, resolve_maximized,
        restore_in_area, restored_window_bounds, select_display, size_value,
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
            restore_in_area(&mut options, size(px(600.), px(360.)), None, tiny, None),
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
            restore_in_area(
                &mut options,
                size(px(600.), px(360.)),
                Some(saved),
                area,
                None
            ),
            saved
        );
        assert_eq!(options.window_min_size, Some(size(px(480.), px(280.))));
        // 无记忆时600×360是默认而非最小尺寸；用户保存更大的尺寸仍然保留。
        assert_eq!(
            restore_in_area(&mut options, size(px(600.), px(360.)), None, area, None),
            rect(-900., 244., 600., 360.)
        );
    }

    #[test]
    fn display_selection_preserves_saved_screen_and_rejects_disconnected_ids() {
        let saved = StoredFrame {
            bounds: rect(20., 30., 600., 360.),
            maximized: false,
            display: StoredDisplay::Known("saved-screen".into()),
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
            display: StoredDisplay::Legacy,
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
        let value = frame_value(
            rect(-1800., 20., 900., 600.),
            true,
            &StoredDisplay::Known("display-a".into()),
        );
        assert_eq!(
            parse_frame(&value),
            Some(StoredFrame {
                bounds: rect(-1800., 20., 900., 600.),
                maximized: true,
                display: StoredDisplay::Known("display-a".to_owned()),
            })
        );
        // 旧记录没有显示器字段。
        let legacy = json!({"x": 10.0, "y": 20.0, "w": 800.0, "h": 600.0});
        assert_eq!(
            parse_frame(&legacy),
            Some(StoredFrame {
                bounds: rect(10., 20., 800., 600.),
                maximized: false,
                display: StoredDisplay::Legacy,
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
                &frame_value(degenerate, false, &StoredDisplay::Known("display-a".into()))
            ));
            assert!(!is_restorable(
                RememberedWindow::TaskDetail,
                &size_value(degenerate.size)
            ));
        }
        let resized = rect(120., 80., 900., 560.);
        assert!(is_restorable(
            RememberedWindow::Settings,
            &frame_value(resized, true, &StoredDisplay::Legacy)
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

    #[test]
    fn nil_display_uuid_keeps_global_origin_unlike_a_matched_display() {
        let nil = "00000000-0000-0000-0000-000000000000";
        let value = json!({
            "x": 0.0,
            "y": 0.0,
            "w": 1920.0,
            "h": 1040.0,
            "maximized": true,
            "display": nil,
        });
        let frame = parse_frame(&value).expect("nil uuid frame");
        assert_eq!(frame.display, StoredDisplay::Unavailable);
        assert!(frame.maximized);
        // 全零 UUID 不能匹配输出，不采用 remembered 显示器，但保留绝对坐标。
        assert_eq!(
            select_display(Some(&frame), Some(9), Some(3), Some(1)),
            (Some(3), true)
        );
        assert_eq!(
            select_display(Some(&frame), Some(9), None, Some(1)),
            (Some(1), true)
        );
        let written = frame_value(
            rect(0., 0., 1920., 1040.),
            true,
            &StoredDisplay::Unavailable,
        );
        assert!(written.get("display").is_some_and(Value::is_null));
        assert!(!written.to_string().contains(nil));
        assert_eq!(
            parse_frame(&written).expect("null display frame").display,
            StoredDisplay::Unavailable
        );
        let legacy = parse_frame(&json!({"x": 0.0, "y": 0.0, "w": 1920.0, "h": 1040.0}))
            .expect("legacy frame");
        assert_eq!(legacy.display, StoredDisplay::Legacy);
        assert_eq!(
            select_display(Some(&legacy), None, Some(3), Some(1)),
            (Some(1), true)
        );
    }

    #[test]
    fn unavailable_maximized_frame_keeps_origin_on_virtual_desktop() {
        let nil = "00000000-0000-0000-0000-000000000000";
        let desktop = rect(0., 0., 3840., 1080.);
        let default_size = size(px(1120.), px(760.));
        let main = parse_frame(&json!({
            "x": 0.0,
            "y": 0.0,
            "w": 1920.0,
            "h": 1040.0,
            "maximized": true,
            "display": nil,
        }))
        .expect("main frame");
        let (_, keep) = select_display(Some(&main), None::<u8>, None, Some(1));
        assert!(keep);
        let placed = place_frame(
            RememberedWindow::Main,
            &main,
            default_size,
            desktop,
            keep,
            Some(size(px(720.), px(520.))),
        );
        assert!(matches!(
            placed,
            WindowBounds::Maximized(bounds)
                if bounds.origin == point(px(0.), px(0.)) && bounds.size == default_size
        ));
        let centered = centered_in_area(size(px(1920.), px(1040.)), desktop, None);
        assert!(f32::from(centered.origin.x) > 900.);
        assert_ne!(centered.origin, point(px(0.), px(0.)));

        let inflated = parse_frame(&json!({
            "x": 0.0,
            "y": 0.0,
            "w": 1960.0,
            "h": 1080.0,
            "maximized": true,
            "display": nil,
        }))
        .expect("inflated frame");
        let inflated_placed = place_frame(
            RememberedWindow::Main,
            &inflated,
            default_size,
            desktop,
            true,
            None,
        );
        assert!(matches!(
            inflated_placed,
            WindowBounds::Maximized(bounds)
                if bounds.origin == point(px(0.), px(0.))
                    && bounds.size == default_size
                    && bounds.size != size(px(1960.), px(1080.))
                    && bounds.size != size(px(1920.), px(1040.))
        ));

        let settings = parse_frame(&json!({
            "x": 30.0,
            "y": 30.0,
            "w": 1359.0,
            "h": 879.0,
            "maximized": false,
            "display": nil,
        }))
        .expect("settings frame");
        let settings_placed = place_frame(
            RememberedWindow::Settings,
            &settings,
            size(px(1240.), px(760.)),
            desktop,
            true,
            Some(size(px(1000.), px(600.))),
        );
        assert!(matches!(
            settings_placed,
            WindowBounds::Windowed(bounds)
                if bounds.origin == point(px(30.), px(30.))
                    && f32::from(bounds.origin.x) + f32::from(bounds.size.width) < 1920.
        ));

        let progress = parse_frame(&json!({
            "x": 331.0,
            "y": 71.0,
            "w": 1077.0,
            "h": 837.0,
            "maximized": false,
            "display": nil,
        }))
        .expect("progress frame");
        let progress_placed = place_frame(
            RememberedWindow::Progress,
            &progress,
            size(px(480.), px(360.)),
            desktop,
            true,
            None,
        );
        let progress_centered = centered_in_area(size(px(1077.), px(837.)), desktop, None);
        assert!(f32::from(progress_centered.origin.x) > 1000.);
        assert!(matches!(
            progress_placed,
            WindowBounds::Windowed(bounds)
                if bounds.origin == point(px(331.), px(71.))
                    && bounds.origin != progress_centered.origin
        ));

        let known = StoredFrame {
            bounds: rect(40., 24., 1280., 800.),
            maximized: true,
            display: StoredDisplay::Known("display-a".into()),
        };
        let known_placed = place_frame(
            RememberedWindow::Main,
            &known,
            default_size,
            rect(0., 0., 1920., 1080.),
            true,
            None,
        );
        assert!(matches!(
            known_placed,
            WindowBounds::Maximized(bounds) if bounds == known.bounds
        ));
    }

    fn place_frame(
        which: RememberedWindow,
        frame: &StoredFrame,
        default_size: gpui::Size<gpui::Pixels>,
        area: Bounds<gpui::Pixels>,
        keep_position: bool,
        min_size: Option<gpui::Size<gpui::Pixels>>,
    ) -> WindowBounds {
        let mut options = WindowOptions {
            window_min_size: min_size,
            ..WindowOptions::default()
        };
        let placed = restore_in_area(
            &mut options,
            create_size(frame, default_size),
            remembered_rect(frame, default_size, keep_position),
            area,
            None,
        );
        restored_window_bounds(which, placed, frame.maximized)
    }

    #[test]
    fn frame_extent_growth_is_not_persisted() {
        let start = size(px(640.), px(530.));
        let grown = |steps: f32| size(px(640. + 40. * steps), px(530. + 40. * steps));
        assert_eq!(
            persisted_after_observations(start, &[grown(1.), grown(2.)], 20.),
            start
        );
        let resized = size(px(800.), px(620.));
        assert_eq!(
            persisted_after_observations(start, &[resized], 20.),
            resized
        );
        // 忽略一次膨胀后窗口仍比请求尺寸大 40：用户再拖到的尺寸要扣掉它才是真实尺寸。
        assert_eq!(
            persisted_after_observations(start, &[grown(1.), resized], 20.),
            size(px(760.), px(580.))
        );
        // 膨胀后尺寸不再变化时，落盘的仍是未膨胀的原尺寸。
        assert_eq!(
            persisted_after_observations(start, &[grown(1.), grown(1.)], 20.),
            start
        );
        assert!(!is_frame_extent_inflation(start, grown(2.), 20.));
    }

    fn persisted_after_observations(
        start: gpui::Size<gpui::Pixels>,
        steps: &[gpui::Size<gpui::Pixels>],
        inset: f32,
    ) -> gpui::Size<gpui::Pixels> {
        let mut guard = InflationGuard::new(Some(start));
        let mut saved = start;
        for &next in steps {
            if let Some(value) = guard.admit(RememberedWindow::NewDownload, size_value(next), inset)
            {
                saved = parse_size(&value).expect("persisted size");
            }
        }
        saved
    }

    #[test]
    fn polluted_size_only_record_restores_the_default() {
        let default = size(px(640.), px(530.));
        assert_eq!(
            remembered_size(size(px(1960.), px(1080.)), default),
            default
        );
        assert_eq!(remembered_size(size(px(700.), px(1200.)), default), default);
        assert_eq!(
            remembered_size(size(px(800.), px(600.)), default),
            size(px(800.), px(600.))
        );
        // 恰好两倍仍视为用户尺寸。
        assert_eq!(
            remembered_size(size(px(1280.), px(1060.)), default),
            size(px(1280.), px(1060.))
        );
    }

    #[test]
    fn position_frame_extent_growth_keeps_last_valid_record() {
        let start = Observed {
            x: 0.,
            y: 0.,
            size: size(px(1920.), px(1040.)),
            maximized: true,
        };
        let pushed = Observed {
            x: 1880.,
            y: 0.,
            size: size(px(1960.), px(1080.)),
            maximized: true,
        };
        assert_eq!(persisted_position_after(start, &[pushed], 20.), start);
        let width_only = Observed {
            x: 1900.,
            y: 0.,
            size: size(px(1960.), px(1040.)),
            maximized: true,
        };
        assert_eq!(persisted_position_after(start, &[width_only], 20.), start);
        let moved = Observed {
            x: 40.,
            y: 20.,
            size: start.size,
            maximized: true,
        };
        assert_eq!(persisted_position_after(start, &[moved], 20.), moved);
        let user_resize = Observed {
            x: 80.,
            y: 40.,
            size: size(px(1400.), px(900.)),
            maximized: false,
        };
        assert_eq!(
            persisted_position_after(start, &[user_resize], 20.),
            user_resize
        );
        let shrunk = Observed {
            x: 10.,
            y: 10.,
            size: size(px(1960.), px(1000.)),
            maximized: false,
        };
        assert_eq!(persisted_position_after(start, &[shrunk], 20.), shrunk);
        assert!(!is_frame_extent_inflation(start.size, width_only.size, 20.));
        assert!(is_position_frame_inflation(
            start.size,
            width_only.size,
            20.
        ));
        assert!(is_position_frame_inflation(start.size, pushed.size, 20.));
    }

    #[test]
    fn same_size_move_after_a_rejected_inflation_persists_the_uninflated_size() {
        let start = Observed {
            x: 30.,
            y: 30.,
            size: size(px(1359.), px(879.)),
            maximized: false,
        };
        let inflated = Observed {
            x: 30.,
            y: 30.,
            size: size(px(1399.), px(919.)),
            maximized: false,
        };
        let moved = Observed {
            x: 200.,
            y: 120.,
            size: inflated.size,
            maximized: false,
        };
        // 被拒绝的膨胀步之后，窗口只移动不缩放：位置更新，尺寸仍是原尺寸。
        assert_eq!(
            persisted_position_after(start, &[inflated, moved], 20.),
            Observed {
                size: start.size,
                ..moved
            }
        );
        let moved_again = Observed {
            x: 260.,
            y: 180.,
            ..moved
        };
        assert_eq!(
            persisted_position_after(start, &[inflated, moved, moved_again], 20.),
            Observed {
                size: start.size,
                ..moved_again
            }
        );
        // 只有宽度膨胀时只扣宽度。
        let width_inflated = Observed {
            size: size(px(1399.), px(879.)),
            ..start
        };
        let width_moved = Observed {
            x: 60.,
            y: 50.,
            ..width_inflated
        };
        assert_eq!(
            persisted_position_after(start, &[width_inflated, width_moved], 20.),
            Observed {
                size: start.size,
                ..width_moved
            }
        );
        // 膨胀后用户再缩放：落盘的是扣掉膨胀量后的真实尺寸，不是膨胀后的外框。
        let resized = Observed {
            x: 80.,
            y: 40.,
            size: size(px(1400.), px(900.)),
            maximized: false,
        };
        assert_eq!(
            persisted_position_after(start, &[inflated, resized], 20.),
            Observed {
                size: size(px(1360.), px(860.)),
                ..resized
            }
        );
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Observed {
        x: f32,
        y: f32,
        size: gpui::Size<gpui::Pixels>,
        maximized: bool,
    }

    fn persisted_position_after(start: Observed, steps: &[Observed], inset: f32) -> Observed {
        let mut guard = InflationGuard::new(Some(start.size));
        let mut saved = start;
        for step in steps {
            let value = frame_value(
                rect(
                    step.x,
                    step.y,
                    f32::from(step.size.width),
                    f32::from(step.size.height),
                ),
                step.maximized,
                &StoredDisplay::Legacy,
            );
            if let Some(value) = guard.admit(RememberedWindow::Main, value, inset) {
                let frame = parse_frame(&value).expect("persisted frame");
                saved = Observed {
                    x: f32::from(frame.bounds.origin.x),
                    y: f32::from(frame.bounds.origin.y),
                    size: frame.bounds.size,
                    maximized: frame.maximized,
                };
            }
        }
        saved
    }

    #[test]
    fn dialog_centers_on_the_main_window_without_shrinking_to_it() {
        // X11：可见区是整块 3840×1080 虚拟桌面，主窗口在右输出且只有最小尺寸 720×520。
        let desktop = rect(0., 0., 3840., 1080.);
        let main = rect(2400., 280., 720., 520.);
        let min = size(px(560.), px(400.));
        let mut options = WindowOptions {
            window_min_size: Some(min),
            ..WindowOptions::default()
        };
        // BT 选择 760×640 比主窗口还大：保持请求尺寸，中心仍是主窗口中心。
        let wanted = size(px(760.), px(640.));
        let placed = restore_in_area(&mut options, wanted, None, desktop, Some(main.center()));
        assert_eq!(placed.size, wanted);
        assert_eq!(placed.center(), main.center());
        // 最小尺寸只受目标显示器约束，不被夹到主窗口尺寸。
        assert_eq!(options.window_min_size, Some(min));
        let on_desktop = centered_in_area(wanted, desktop, None);
        assert!(f32::from(on_desktop.origin.x) < f32::from(main.origin.x));
        assert_ne!(placed.origin, on_desktop.origin);

        // 主窗口贴着虚拟桌面右缘时，整块对话框平移回可见区内，尺寸不变。
        let edge = point(px(3800.), px(100.));
        assert_eq!(
            centered_at(edge, wanted, desktop, None),
            rect(3080., 0., 760., 640.)
        );
    }

    #[test]
    fn dialog_larger_than_the_display_is_capped_to_the_display_not_the_main_window() {
        let area = rect(0., 0., 1920., 1080.);
        let center = point(px(960.), px(540.));
        assert_eq!(
            centered_at(center, size(px(2400.), px(1400.)), area, None),
            area
        );
        // 请求尺寸小于最小尺寸时按最小尺寸放置。
        assert_eq!(
            centered_at(
                center,
                size(px(300.), px(200.)),
                area,
                Some(size(px(400.), px(300.)))
            ),
            rect(760., 390., 400., 300.)
        );
    }

    #[test]
    fn offscreen_anchor_center_never_leaves_the_display() {
        // Windows 最小化窗口的 `bounds()` 约为 (-32000,-32000)；即便误取到，也只会贴回可见区。
        let area = rect(0., 0., 1920., 1080.);
        let wanted = size(px(640.), px(530.));
        let lost = point(px(-31700.), px(-31700.));
        assert_eq!(
            centered_at(lost, wanted, area, None),
            rect(0., 0., 640., 530.)
        );
    }

    #[gpui::test]
    fn anchor_on_an_unplugged_display_is_dropped(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let online = cx.primary_display().expect("test display").id();
            let anchor = |display_id| MainAnchor {
                bounds: rect(100., 100., 800., 600.),
                display_id,
            };
            let (center, display) =
                resolve_anchor(Some(anchor(Some(online))), cx).expect("online anchor");
            assert_eq!(center, point(px(500.), px(400.)));
            assert!(display.id() == online);
            // 主窗口关闭后锚点仍保留，期间外接屏被拔掉：不能再用它。
            assert!(resolve_anchor(Some(anchor(Some(DisplayId::new(u64::MAX)))), cx).is_none());
            assert!(resolve_anchor(Some(anchor(None)), cx).is_none());
            assert!(resolve_anchor(None, cx).is_none());
        });
    }
}
