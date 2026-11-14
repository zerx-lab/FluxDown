//! 全应用共用的控件规格：按钮 / 输入框统一高度与字号、分段标签、复选行。
//!
//! 规则（所有窗口与页面一致）：
//! - 唯一一档控件高度 `density.control`（默认 28）、13px 正文字号：页面工具栏、对话框、表单页、
//!   设置行、列表行内、独立窗口里的按钮 / 输入框 / 下拉 / 数字输入全部同高同内边距
//!   → [`ControlExt::control`] / [`IconControlExt::control_icon`]；chrome 纯图标按钮优先 `toolbar_action_button`。
//! - 变体只用 gpui-component 的 `primary()` / `ghost()` / `outline()` / `danger()`，
//!   再调用上面的尺寸方法。不要用 `.small()` / `.xsmall()` / 自定义高度。
//! - 下拉选择 = outline 按钮 + `dropdown_caret(true)` + `.control(cx)`，与输入框等高。
//! - 标签页（对话框内、页面内）一律 [`segmented_tabs`]，不自造按钮组。
//! - 复选框一律 [`check_mark`] / [`check_row`]，不用 gpui-component `Checkbox`。
//! - gpui 的 `hover` 每个元素只能设置一次：本 crate 返回的按钮 / 行已设置悬停，调用方不得再 `.hover()`。
//!   本 crate 按钮的底色悬停 / 按下 / 选中由 [`crate::StateLayer`] 过渡绘制，调用方也不得再 `.active()` 改底色。

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, Bounds, BoxShadow, Div, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, Pixels, RenderOnce, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, linear_color_stop, linear_gradient,
    point, prelude::FluentBuilder as _, px, size,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{Input, NumberInput},
    scroll::ScrollableElement as _,
};

use crate::{CheckState, SlidingHighlight, check_mark, color_transition, tabular_numbers};

/// 统一控件尺寸（带文字按钮与输入框）。
pub trait ControlExt: Sized {
    /// `density.control` 高、13px 字。
    fn control(self, cx: &App) -> Self;
}

/// 纯图标按钮尺寸。
pub trait IconControlExt: Sized {
    /// `density.control` 见方（与同行输入框等高）。
    fn control_icon(self, cx: &App) -> Self;
}

// gpui-component 的按钮文字字号由 `Size` 决定并写在内部 label 上（根节点的
// `text_size` 覆盖不到）：`Medium` 取 `text_base` = 1rem = `typography.sm`(13)，
// 再用实例样式把高度与内边距收回到目标档位。
impl ControlExt for Button {
    fn control(self, cx: &App) -> Self {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        self.with_size(Size::Medium)
            .h(theme.density().control)
            .px(tokens.spacing.sm + tokens.spacing.xxs)
            .text_size(tokens.typography.sm.size)
            .line_height(tokens.typography.sm.line_height)
    }
}

// 图标与带文字按钮的前置图标同为 `Medium`（16px）。
impl IconControlExt for Button {
    fn control_icon(self, cx: &App) -> Self {
        Styled::size(
            self.with_size(Size::Medium),
            active_theme(cx).density().control,
        )
    }
}

/// 按钮进行中状态。gpui-component 的 `Button::loading` 只把**前置图标**换成转圈；
/// 纯文字按钮没有图标，`loading(true)` 只会变得不可点击而看不到任何动画。
/// 一律用 [`BusyExt::busy`]：进行中时补一个前置 spinner。
pub trait BusyExt: Sized {
    fn busy(self, busy: bool) -> Self;
}

impl BusyExt for Button {
    fn busy(self, busy: bool) -> Self {
        self.loading(busy).when(busy, |button| {
            button.icon(gpui_component::spinner::Spinner::new())
        })
    }
}

// 输入框用 `Medium` 取得宽松的横向内边距（`Small` 只有约 6px，显得局促），
// 字号 / 高度由实例样式覆盖（gpui-component 在根节点 `refine_style`，覆盖有效）。
// 高度必须走 `Styled::h`：`Input` 自带的同名 `h()` 只作用于多行输入，单行会被
// 静默忽略而退回 `Size::Medium` 的 2rem（26px），比同行按钮 / 下拉矮一截。
// 纵向内边距必须清零：gpui-component 0.7 的 `Medium` 单行自带上下各 8px，
// 28px 外框扣掉 16px 内边距与 2px 描边只剩 10px 内容区，而单行文本元素固定占
// 1.25rem 行高，文字 / 占位符上下被裁。外框本身 `items_center`，清零后行盒居中。
impl ControlExt for Input {
    fn control(self, cx: &App) -> Self {
        let theme = active_theme(cx);
        Styled::h(self.with_size(Size::Medium), theme.density().control)
            .py_0()
            .text_size(theme.tokens().typography.sm.size)
    }
}

// NumberInput 的中间输入框字号跟随 `Size`（`Medium` 只有 text_sm ≈ 11px），外部
// `text_size` 覆盖不到；取 `Large`（text_base = 13）再把外框高度收回到目标档位，
// 内部输入框与加减按钮都是 `h_full`，随外框高度走。
impl ControlExt for NumberInput {
    fn control(self, cx: &App) -> Self {
        self.with_size(Size::Large)
            .h(active_theme(cx).density().control)
    }
}

type TabSelect = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// 分段标签（macOS segmented control 风格）：浅灰轨道内，选中项白底 + 细阴影 +
/// 正文色中等字重，未选中为二级文字色。轨道高 `density.control`，项高再减 4。
///
/// 选中白底是一块随选中项滑动的底块（弹簧驱动，宽度同步过渡），标签文字色同步淡变。
/// 底块按上一帧量得的标签位置定位；首帧尚无测量时由选中项自己画底，下一帧交给底块。
///
/// 容器放不下全部标签时（如窄侧栏里的 BT 详情）轨道收缩到可用宽度，标签行横向滚动：
/// 纵向滚轮同样横滚，被裁一侧显示渐隐提示，选中项变化时自动滚入视野。
pub fn segmented_tabs(
    id: impl Into<ElementId>,
    labels: impl IntoIterator<Item = SharedString>,
    selected: usize,
    on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let colors = tokens.colors;
    let control = theme.density().control;
    let tab_radius = theme.components().tab_radius;
    let track = theme.extended().colors.nav_hover;

    div()
        .flex()
        .flex_shrink(1.)
        .min_w_0()
        .max_w_full()
        .items_center()
        .h(control)
        .p(px(2.))
        .rounded(tab_radius + px(1.))
        .bg(track)
        .child(SegmentedTabs {
            id: id.into(),
            labels: labels.into_iter().collect(),
            selected,
            on_select: Rc::new(on_select),
            style: SegmentedStyle {
                height: control - px(4.),
                radius: tab_radius,
                padding_x: tokens.spacing.md,
                text_size: tokens.typography.sm.size,
                line_height: tokens.typography.sm.line_height,
                surface: colors.surface,
                foreground: colors.foreground,
                muted_foreground: colors.muted_foreground,
                ring: colors.ring,
                track,
                fade_width: tokens.spacing.xl,
                shadow: tokens.shadow.sm.clone(),
            },
        })
}

struct SegmentedStyle {
    height: Pixels,
    radius: Pixels,
    padding_x: Pixels,
    text_size: Pixels,
    line_height: Pixels,
    surface: Hsla,
    foreground: Hsla,
    muted_foreground: Hsla,
    ring: Hsla,
    /// 轨道底色：横向溢出时两端渐隐从它过渡到透明。
    track: Hsla,
    fade_width: Pixels,
    shadow: Vec<BoxShadow>,
}

/// 分段标签的跨帧元素状态（放在元素状态而非实体里，不被依赖追踪，变化时由写入方自己请求一帧）。
///
/// - `tabs`：上一帧量得的各标签相对首个标签的 `(左缘, 宽度)`，由标签行的
///   `on_children_prepainted` 写入（首帧、标签增减、改字号 / 语言时变化）；只存相对值，
///   窗口缩放与滚动不会触发重建。
/// - `scroll`：标签行横向滚动句柄；`revealed`：已滚入视野的选中项，选中变化时重新滚动。
/// - `fades`：本帧渲染的左右渐隐，prepaint 按实际滚动位置复核，不符时请求下一帧。
#[derive(Clone, Default)]
struct SegmentGeometry {
    tabs: Rc<RefCell<Vec<(Pixels, Pixels)>>>,
    scroll: ScrollHandle,
    revealed: Rc<Cell<Option<usize>>>,
    fades: Rc<Cell<(bool, bool)>>,
}

/// 按滚动位置判定两端是否有被裁的标签（半像素容差，避免亚像素抖动反复切换）。
fn scroll_fades(scroll: &ScrollHandle) -> (bool, bool) {
    let scrolled = -scroll.offset().x;
    let max = scroll.max_offset().x;
    (scrolled > px(0.5), scrolled < max - px(0.5))
}

#[derive(IntoElement)]
struct SegmentedTabs {
    id: ElementId,
    labels: Vec<SharedString>,
    selected: usize,
    on_select: TabSelect,
    style: SegmentedStyle,
}

impl RenderOnce for SegmentedTabs {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            labels,
            selected,
            on_select,
            style,
        } = self;
        let count = labels.len();

        let (geometry, highlight, text_colors) = window.with_id(id.clone(), |window| {
            let geometry =
                window.with_global_id(ElementId::from("geometry"), |global_id, window| {
                    window.with_element_state(global_id, |state: Option<SegmentGeometry>, _| {
                        let state = state.unwrap_or_default();
                        (state.clone(), state)
                    })
                });
            // 选中项的底块外框；尚无测量（首帧）或与标签数不符时本帧由选中项自己画底，
            // 测量写入后会请求下一帧交给底块。
            let highlight = {
                let tabs = geometry.tabs.borrow();
                tabs.get(selected)
                    .filter(|_| tabs.len() == count)
                    .map(|&(left, width)| {
                        Bounds::new(point(left, px(0.)), size(width, style.height))
                    })
            };
            let text_colors: Vec<Hsla> = (0..count)
                .map(|index| {
                    let color = if index == selected {
                        style.foreground
                    } else {
                        style.muted_foreground
                    };
                    color_transition(index, color, window, cx)
                })
                .collect();
            (geometry, highlight, text_colors)
        });

        let has_highlight = highlight.is_some();
        let scroll = geometry.scroll.clone();
        // 选中项变化（含首次渲染）时把它最小幅度滚入视野；需要上一帧的测量与视口宽度。
        if geometry.revealed.get() != Some(selected)
            && let Some(bounds) = highlight
        {
            let viewport = scroll.bounds().size.width;
            let max = scroll.max_offset().x;
            if viewport > px(0.) {
                let current = -scroll.offset().x;
                let (left, right) = (bounds.left(), bounds.right());
                let target = if left < current {
                    left
                } else if right > current + viewport {
                    right - viewport
                } else {
                    current
                };
                let target = target.clamp(px(0.), max.max(px(0.)));
                if target != current {
                    scroll.set_offset(point(-target, px(0.)));
                }
                geometry.revealed.set(Some(selected));
            }
        }
        let (fade_left, fade_right) = scroll_fades(&scroll);
        geometry.fades.set((fade_left, fade_right));
        let fade = |left: bool| {
            let (from, to) = (style.track, style.track.opacity(0.));
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .w(style.fade_width)
                .map(|this| if left { this.left_0() } else { this.right_0() })
                .bg(linear_gradient(
                    if left { 90. } else { 270. },
                    linear_color_stop(from, 0.),
                    linear_color_stop(to, 1.),
                ))
        };
        let content = div()
            .relative()
            .flex_none()
            .h_full()
            .when_some(highlight, |this, bounds| {
                this.child(
                    SlidingHighlight::new(id.clone(), bounds, style.surface)
                        .radius(style.radius)
                        .shadow(style.shadow.clone()),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .h_full()
                    .gap(px(2.))
                    .on_children_prepainted(move |bounds, window, _| {
                        let origin = bounds.first().map_or(px(0.), |first| first.origin.x);
                        let measured: Vec<(Pixels, Pixels)> = bounds
                            .iter()
                            .map(|tab| (tab.origin.x - origin, tab.size.width))
                            .collect();
                        let mut tabs = geometry.tabs.borrow_mut();
                        let fades = scroll_fades(&geometry.scroll);
                        if *tabs != measured || geometry.fades.get() != fades {
                            *tabs = measured;
                            window.request_animation_frame();
                        }
                    })
                    .children(labels.into_iter().zip(text_colors).enumerate().map(
                        |(index, (label, text_color))| {
                            let active = index == selected;
                            let on_select = Rc::clone(&on_select);
                            let (foreground, ring) = (style.foreground, style.ring);
                            div()
                                .id(ElementId::NamedInteger(
                                    SharedString::from(format!("{id}-tab")),
                                    index as u64,
                                ))
                                .flex_none()
                                .h(style.height)
                                .px(style.padding_x)
                                .flex()
                                .items_center()
                                .rounded(style.radius)
                                .text_size(style.text_size)
                                .line_height(style.line_height)
                                .cursor_pointer()
                                .text_color(text_color)
                                .map(|this| {
                                    if !active {
                                        this.hover(move |hover| hover.text_color(foreground))
                                    } else if has_highlight {
                                        this.font_weight(FontWeight::MEDIUM)
                                    } else {
                                        this.font_weight(FontWeight::MEDIUM)
                                            .bg(style.surface)
                                            .shadow(style.shadow.clone())
                                    }
                                })
                                .border_1()
                                .border_color(gpui::transparent_black())
                                .focusable()
                                .tab_stop(true)
                                .focus_visible(move |focus| focus.border_color(ring))
                                .on_click(move |_, window, cx| on_select(index, window, cx))
                                .child(label)
                        },
                    )),
            );
        div()
            .relative()
            .flex_shrink(1.)
            .min_w_0()
            .h_full()
            .child(
                div()
                    .id(ElementId::from(SharedString::from(format!("{id}-scroll"))))
                    .flex()
                    .size_full()
                    .overflow_x_scroll()
                    .track_scroll(&scroll)
                    .child(content),
            )
            .when(fade_left, |this| this.child(fade(true)))
            .when(fade_right, |this| this.child(fade(false)))
    }
}

/// 可点击的复选行：[`check_mark`] + 文字（可选尾部计数/说明）。整行可点。
pub fn check_row(
    id: impl Into<ElementId>,
    checked: bool,
    label: impl IntoElement,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
    cx: &App,
) -> gpui::Stateful<Div> {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let hover = theme.extended().colors.row_hover;
    let ring = tokens.colors.ring;
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(tokens.spacing.sm)
        .min_h(theme.density().control)
        .px(tokens.spacing.xs)
        .rounded(tokens.radius.md)
        .cursor_pointer()
        .text_size(tokens.typography.sm.size)
        .line_height(tokens.typography.sm.line_height)
        .text_color(tokens.colors.foreground)
        .border_1()
        .border_color(gpui::transparent_black())
        .hover(move |style| style.bg(hover))
        // gpui 对获得焦点的可点击元素把 Enter / Space 转成点击，这里只需可聚焦并给出焦点环。
        .focusable()
        .tab_stop(true)
        .focus_visible(move |style| style.border_color(ring))
        .on_click(move |_, window, cx| on_toggle(!checked, window, cx))
        .child(check_mark(
            if checked {
                CheckState::Checked
            } else {
                CheckState::Unchecked
            },
            cx,
        ))
        .child(label)
}

/// 小号计数 / 数字徽标文字样式（caption + 等宽数字 + 三级文字色）。
pub fn caption_number(text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = active_theme(cx);
    let caption = theme.extended().caption;
    div()
        .text_size(caption.size)
        .line_height(caption.line_height)
        .font_features(tabular_numbers())
        .text_color(theme.extended().colors.text_tertiary)
        .child(text.into())
}

/// 对话框主操作的语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogIntent {
    /// 普通确认（primary）。
    Confirm,
    /// 破坏性确认（danger）。
    Destructive,
}

/// 对话框主操作按钮外层的键位上下文：打开即聚焦，`space` 在此上下文绑定到对话框
/// `Confirm`（`enter` 由 gpui-component 的 `Dialog` 上下文冒泡处理）。
pub const DIALOG_PRIMARY_KEY_CONTEXT: &str = "DialogPrimaryAction";

/// 统一的对话框底栏：右对齐，「取消」(outline) 在左、主操作在右，均为 28 高控件。
///
/// 取代 `DialogButtonProps`（其默认按钮是 32 高、不经过 [`ControlExt`]）。取消经
/// `DialogClose` 关闭对话框并触发 `on_cancel`，主操作经 `DialogAction` 分发
/// `Confirm`，与默认底栏同一路径，`on_ok` 语义不变。`cancel` 为 `None` 时只显示主操作。
/// 主操作打开时默认聚焦（显示焦点环），回车 / 空格即确认。
pub fn dialog_footer(
    cancel: Option<SharedString>,
    ok: impl Into<SharedString>,
    intent: DialogIntent,
    cx: &App,
) -> DialogFooter {
    let spacing = active_theme(cx).tokens().spacing;
    let ok = Button::new("dialog-ok").label(ok.into());
    let ok = match intent {
        DialogIntent::Confirm => ok.primary(),
        DialogIntent::Destructive => ok.danger(),
    }
    .control(cx);
    DialogFooter::new()
        .gap(spacing.sm)
        .when_some(cancel, |footer, cancel| {
            footer.child(
                DialogClose::new().child(
                    Button::new("dialog-cancel")
                        .outline()
                        .label(cancel)
                        .control(cx),
                ),
            )
        })
        .child(DialogAction::new().child(AutofocusAction { child: ok, intent }))
}

/// 首帧把焦点移到自身的包装：焦点句柄存于元素状态，对话框关闭后随元素一起释放，
/// 下次打开重新创建并再次聚焦。聚焦时在按钮外描一圈与主操作同色、带留白的焦点环
/// （线宽 / 留白取 `focusRing.width` / `focusRing.offset`，同 Web 的 `ring-offset-1 ring-2`）。
///
/// 不用 gpui-component 的 `focus_ring_style`：FluxDown 主题关闭了 `focus_ring`，
/// 那条路径只改边框色，对无边框的包装层不可见。
#[derive(IntoElement)]
struct AutofocusAction {
    child: Button,
    intent: DialogIntent,
}

impl RenderOnce for AutofocusAction {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state("dialog-primary-focus", cx, |window, cx| {
                let handle = cx.focus_handle();
                let target = handle.clone();
                window.defer(cx, move |window, cx| window.focus(&target, cx));
                handle
            })
            .read(cx)
            .clone();
        let focused = focus.is_focused(window);
        let focus_ring = active_theme(cx).extended().focus_ring;
        let theme = cx.theme();
        let ring_color = match self.intent {
            DialogIntent::Confirm => theme.primary,
            DialogIntent::Destructive => theme.danger,
        };
        let outset = focus_ring.offset + focus_ring.width;
        let ring_radius = theme.radius + outset;
        div()
            .relative()
            .key_context(DIALOG_PRIMARY_KEY_CONTEXT)
            .track_focus(&focus)
            .child(self.child)
            .when(focused, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(-outset)
                        .left(-outset)
                        .right(-outset)
                        .bottom(-outset)
                        .border(focus_ring.width)
                        .border_color(ring_color)
                        .rounded(ring_radius),
                )
            })
    }
}

/// 对话框 / 确认框标题：`extended.title`（15/20 半粗）+ 正文色，底部 `spacing.xs`
/// （与 gpui-component 对话框内置 8px 间距合成规范的 12px）。所有窗口共用。
pub fn dialog_title(text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = active_theme(cx);
    let title = theme.extended().title;
    div()
        .pb(theme.tokens().spacing.xs)
        .text_size(title.size)
        .line_height(title.line_height)
        .font_weight(title.weight)
        .text_color(theme.tokens().colors.foreground)
        .child(text.into())
}

/// 对话框正文滚动区：正文高过窗口（或 `max_height` 设计上限）时纵向滚动，标题 / 底栏保持可见。
/// 所有对话框共用。
///
/// gpui-component 的 `DialogContent` 是 `flex_1` 却保留内容最小高度，内容超出窗口时只会被对话框
/// 裁剪、永远滚不动。用法：`.content(|content, ..| content.min_h_0().child(…))`；若中间隔着视图根，
/// 视图根也要 `min_h_0()`。可变高度的正文放进本滚动区，错误行、按钮行作为兄弟节点放在外面。
///
/// 上限只能加在滚动元素外层：直接给 `overflow_y_scrollbar` 元素设 `max_h` 时内部滚动区按内容高度
/// 排版，滚不到底。四周留 `spacing.xxs` 给输入框聚焦环（负外边距抵消，正文与标题仍左右对齐）。
pub fn dialog_scroll_body(
    id: impl Into<ElementId>,
    max_height: Option<Pixels>,
    body: impl IntoElement,
    cx: &App,
) -> Div {
    let inset = active_theme(cx).tokens().spacing.xxs;
    div()
        .flex()
        .flex_col()
        .min_h_0()
        .mx(-inset)
        .when_some(max_height, |this, max_height| this.max_h(max_height))
        .child(
            div()
                .overflow_y_scrollbar()
                .id(id)
                .child(div().p(inset).child(body)),
        )
}

// ── 表单原语 ─────────────────────────────────────────────────────────────
// 所有对话框与表单页（新建订阅、Webhook、分类、新建下载、队列、登录注册、插件设置…）
// 统一用下面几块拼装，保证字段间距、标签、说明、开关分组在各窗口一模一样。

/// 表单字段之间的纵向间距（字段组 → 字段组）。
pub fn form_gap(cx: &App) -> Pixels {
    active_theme(cx).tokens().spacing.lg
}

/// 表单整体：纵向排列字段，间距 [`form_gap`]。
pub fn form(cx: &App) -> Div {
    div().flex().flex_col().w_full().gap(form_gap(cx))
}

/// 字段标签：12px 中等字重、正文色（比说明文字更醒目，比输入内容更轻）。
pub fn field_label(text: impl Into<SharedString>, cx: &App) -> Div {
    let tokens = active_theme(cx).tokens();
    div()
        .text_size(tokens.typography.xs.size)
        .line_height(tokens.typography.xs.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(tokens.colors.foreground)
        .child(text.into())
}

/// 字段说明：12px 三级文字色。
pub fn field_hint(text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = active_theme(cx);
    let xs = theme.tokens().typography.xs;
    div()
        .text_size(xs.size)
        .line_height(xs.line_height)
        .text_color(theme.extended().colors.text_tertiary)
        .child(text.into())
}

/// 字段错误：12px 危险色。
pub fn field_error(text: impl Into<SharedString>, cx: &App) -> Div {
    let xs = active_theme(cx).tokens().typography.xs;
    div()
        .text_size(xs.size)
        .line_height(xs.line_height)
        .text_color(active_theme(cx).tokens().colors.destructive)
        .child(text.into())
}

/// 标准字段：标签 → 控件（输入框 / 下拉 / 输入框 + 同行按钮）→ 可选说明。
/// 标签与控件间距 6px，控件与说明间距 6px。
pub fn form_field(
    label: impl Into<SharedString>,
    control: impl IntoElement,
    hint: Option<SharedString>,
    cx: &App,
) -> Div {
    let spacing = active_theme(cx).tokens().spacing;
    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .gap(spacing.xs + spacing.xxs)
        .child(field_label(label, cx))
        .child(control)
        .when_some(hint, |this, hint| this.child(field_hint(hint, cx)))
}

/// 同一行并排的多个字段（如「刷新间隔 | 队列」、「最小 | 最大」），等分宽度。
pub fn form_row(fields: impl IntoIterator<Item = AnyElement>, cx: &App) -> Div {
    let spacing = active_theme(cx).tokens().spacing;
    div().flex().w_full().gap(spacing.md).children(
        fields
            .into_iter()
            .map(|field| div().flex_1().min_w_0().child(field)),
    )
}

/// 输入框 + 同行操作按钮（如「浏览」「验证」）：输入框吃满剩余宽度。
/// 两者都应经统一控件档（[`ControlExt::control`]）以保证等高。
pub fn input_with_action(input: impl IntoElement, action: impl IntoElement, cx: &App) -> Div {
    let spacing = active_theme(cx).tokens().spacing;
    div()
        .flex()
        .items_center()
        .w_full()
        .gap(spacing.sm)
        .child(div().flex_1().min_w_0().child(input))
        .child(div().flex_none().child(action))
}

/// 开关行：左侧标题（13px 正文色）+ 说明（12px 二级文字色），右侧开关控件。
/// 放进 [`option_group`] 里使用，行高随内容、上下内边距 10px。
pub fn option_row(
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
    let tokens = active_theme(cx).tokens();
    div()
        .flex()
        .items_center()
        .w_full()
        .gap(tokens.spacing.lg)
        .px(tokens.spacing.md)
        .py(tokens.spacing.sm + tokens.spacing.xxs)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(tokens.spacing.xxs)
                .child(
                    div()
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .text_color(tokens.colors.foreground)
                        .child(title.into()),
                )
                .when_some(description, |this, description| {
                    this.child(
                        div()
                            .text_size(tokens.typography.xs.size)
                            .line_height(tokens.typography.xs.line_height)
                            .text_color(tokens.colors.muted_foreground)
                            .child(description),
                    )
                }),
        )
        .child(div().flex_none().child(control))
}

/// 开关 / 选项分组：一张 [`crate::card`]，行与行之间 hairline 分隔（macOS 分组表单风格）。
pub fn option_group(rows: impl IntoIterator<Item = AnyElement>, cx: &App) -> Div {
    let theme = active_theme(cx);
    let hairline = theme.extended().colors.hairline;
    let stroke = theme.extended().stroke.thin;
    let mut group = crate::card(cx).flex().flex_col().w_full();
    for (index, row) in rows.into_iter().enumerate() {
        group = group.child(
            div()
                .w_full()
                .when(index > 0, |this| {
                    this.border_t(stroke).border_color(hairline)
                })
                .child(row),
        );
    }
    group
}
