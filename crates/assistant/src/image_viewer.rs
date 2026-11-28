//! 附图大图查看器：覆盖助手面板，显示原图。右上角工具条：缩小 / 比例（点一下复原）/ 放大 / 关闭；
//! ⌘ / Ctrl + 滚轮或触控板捏合缩放，双击在「适应」与 2 倍之间切换；放大后拖动或滚动平移；Esc 关闭。
//!
//! 缩放以「适应面板」为 1：图片盒子是查看区的 `zoom` 倍、居中后再叠加平移量，`object-fit: contain`
//! 让图片在盒子里等比缩放，因此不需要知道图片本身的像素尺寸。

use std::sync::Arc;

use fluxdown_ui_components::FluxIcon;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, FontWeight, Image, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    ParentElement as _, PinchEvent, Pixels, Point, ScrollEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, div, img, point,
    prelude::FluentBuilder as _, px, relative,
};
use gpui_component::{Icon, h_flex};

use crate::{
    view::AssistantView,
    widgets::{CircleTone, circle_button},
};

/// 缩放范围（1 = 适应面板）。
const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 8.;
/// 按钮每次缩放的倍数。
const ZOOM_STEP: f32 = 1.25;
/// 双击放大到的倍数。
const DOUBLE_CLICK_ZOOM: f32 = 2.;
/// 滚轮缩放灵敏度：滚动这么多像素缩放 e 倍。
const WHEEL_ZOOM_PIXELS: f32 = 240.;
/// 行滚动（鼠标滚轮）换算成像素时每行的高度。
const WHEEL_LINE_HEIGHT: f32 = 16.;
/// 工具条按钮边长。
const TOOL_SIDE: f32 = 28.;
/// 查看区顶部给工具条留的高度。
const TOOLBAR_RESERVE: f32 = 48.;

/// 一次打开的大图预览：`image` 为原图。
pub(crate) struct ImagePreview {
    image: Arc<Image>,
    zoom: f32,
    /// 图片相对居中位置的平移量。只在放大（`zoom > 1`）时非零。
    offset: Point<Pixels>,
    /// 拖动平移中：上一次的指针位置。
    drag_from: Option<Point<Pixels>>,
}

impl ImagePreview {
    pub(crate) fn new(image: Arc<Image>) -> Self {
        Self {
            image,
            zoom: 1.,
            offset: Point::default(),
            drag_from: None,
        }
    }

    /// 按倍数缩放（以查看区中心为准，平移量随之等比，视野中心不跳）；没变化返回 `false`。
    fn zoom_by(&mut self, factor: f32) -> bool {
        if !factor.is_finite() || factor <= 0. {
            return false;
        }
        self.zoom_to(self.zoom * factor)
    }

    fn zoom_to(&mut self, zoom: f32) -> bool {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() <= f32::EPSILON {
            return false;
        }
        let ratio = zoom / self.zoom;
        self.zoom = zoom;
        self.offset = if zoom > 1. {
            point(self.offset.x * ratio, self.offset.y * ratio)
        } else {
            Point::default()
        };
        true
    }

    /// 复原到「适应面板」。
    fn reset(&mut self) -> bool {
        let changed = (self.zoom - 1.).abs() > f32::EPSILON || self.offset != Point::default();
        self.zoom = 1.;
        self.offset = Point::default();
        changed
    }

    /// 双击：适应 ↔ 2 倍。
    fn toggle(&mut self) -> bool {
        if (self.zoom - 1.).abs() > f32::EPSILON {
            self.reset()
        } else {
            self.zoom_to(DOUBLE_CLICK_ZOOM)
        }
    }

    /// 平移；未放大时不动。
    fn pan(&mut self, delta: Point<Pixels>) -> bool {
        if self.zoom <= 1. || delta == Point::default() {
            return false;
        }
        self.offset = point(self.offset.x + delta.x, self.offset.y + delta.y);
        true
    }

    fn begin_drag(&mut self, at: Point<Pixels>) -> bool {
        if self.zoom <= 1. {
            return false;
        }
        self.drag_from = Some(at);
        true
    }

    /// 拖到 `at`；`pressed` 为假（按键已在别处松开）时结束拖动。
    fn drag_to(&mut self, at: Point<Pixels>, pressed: bool) -> bool {
        let Some(from) = self.drag_from else {
            return false;
        };
        if !pressed {
            self.drag_from = None;
            return true;
        }
        self.drag_from = Some(at);
        self.pan(point(at.x - from.x, at.y - from.y))
    }

    fn end_drag(&mut self) -> bool {
        self.drag_from.take().is_some()
    }

    fn percent_label(&self) -> SharedString {
        SharedString::from(format!("{:.0}%", self.zoom * 100.))
    }
}

impl AssistantView {
    pub(crate) fn open_image_preview(&mut self, image: Arc<Image>, cx: &mut Context<Self>) {
        self.image_preview = Some(ImagePreview::new(image));
        cx.notify();
    }

    pub(crate) fn close_image_preview(&mut self, cx: &mut Context<Self>) {
        if self.image_preview.take().is_some() {
            cx.notify();
        }
    }

    /// 改预览状态，有变化才重绘。
    fn update_preview(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut ImagePreview) -> bool,
    ) {
        if let Some(preview) = self.image_preview.as_mut()
            && change(preview)
        {
            cx.notify();
        }
    }

    /// 大图预览覆盖层。
    pub(crate) fn render_image_preview(
        &self,
        preview: &ImagePreview,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let zoom = preview.zoom;
        let inset = (1. - zoom) / 2.;
        let zoomed = zoom > 1.;
        let dragging = preview.drag_from.is_some();
        let picture = img(preview.image.clone())
            .absolute()
            .left(relative(inset))
            .top(relative(inset))
            .w(relative(zoom))
            .h(relative(zoom))
            .object_fit(ObjectFit::Contain);
        let stage = div()
            .id("assistant-image-preview-stage")
            .absolute()
            .top(px(TOOLBAR_RESERVE))
            .bottom(tokens.spacing.lg)
            .left(tokens.spacing.lg)
            .right(tokens.spacing.lg)
            .when(zoomed, |stage| {
                stage.cursor(if dragging {
                    CursorStyle::ClosedHand
                } else {
                    CursorStyle::OpenHand
                })
            })
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(WHEEL_LINE_HEIGHT));
                if event.modifiers.secondary(cx) {
                    let factor = (-f32::from(delta.y) / WHEEL_ZOOM_PIXELS).exp();
                    this.update_preview(cx, |preview| preview.zoom_by(factor));
                } else {
                    this.update_preview(cx, |preview| preview.pan(delta));
                }
                cx.stop_propagation();
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, _, cx| {
                this.update_preview(cx, |preview| preview.zoom_by(1. + event.delta));
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if event.click_count >= 2 {
                        this.update_preview(cx, ImagePreview::toggle);
                    } else {
                        this.update_preview(cx, |preview| preview.begin_drag(event.position));
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let pressed = event.pressed_button == Some(MouseButton::Left);
                this.update_preview(cx, |preview| preview.drag_to(event.position, pressed));
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.update_preview(cx, ImagePreview::end_drag);
                }),
            )
            .child(
                div()
                    .absolute()
                    .size_full()
                    .left(preview.offset.x)
                    .top(preview.offset.y)
                    .child(picture),
            );
        div()
            .id("assistant-image-preview")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .overflow_hidden()
            .bg(gpui::black().opacity(0.72))
            .child(stage)
            .child(self.render_preview_toolbar(preview, cx))
            .into_any_element()
    }

    /// 右上角工具条：缩小 / 比例（点一下复原）/ 放大 / 关闭。
    fn render_preview_toolbar(&self, preview: &ImagePreview, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = tokens.colors;
        let caption = theme.extended().caption;
        let side = theme.text_extent(TOOL_SIDE);
        let icon_size = px(14.);
        let reset_label = self.t("assistantImageZoomReset", cx);
        let zoom_out = circle_button(
            "assistant-image-zoom-out",
            self.t("assistantImageZoomOut", cx),
            Icon::new(FluxIcon::Minus).size(icon_size),
            side,
            CircleTone::Overlay,
            preview.zoom > MIN_ZOOM + f32::EPSILON,
            cx,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.update_preview(cx, |preview| preview.zoom_by(1. / ZOOM_STEP));
        }));
        let zoom_in = circle_button(
            "assistant-image-zoom-in",
            self.t("assistantImageZoomIn", cx),
            Icon::new(FluxIcon::Plus).size(icon_size),
            side,
            CircleTone::Overlay,
            preview.zoom < MAX_ZOOM - f32::EPSILON,
            cx,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.update_preview(cx, |preview| preview.zoom_by(ZOOM_STEP));
        }));
        let percent = div()
            .id("assistant-image-zoom-reset")
            .flex()
            .items_center()
            .justify_center()
            .h(side)
            .min_w(theme.text_extent(52.))
            .px(tokens.spacing.sm)
            .rounded(tokens.radius.full)
            .bg(colors.foreground)
            .text_color(colors.background)
            .text_size(caption.size)
            .line_height(caption.line_height)
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .tooltip(move |window, cx| {
                fluxdown_ui_components::tooltip_view(reset_label.clone(), window, cx)
            })
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.update_preview(cx, ImagePreview::reset);
            }))
            .child(preview.percent_label());
        let close = circle_button(
            "assistant-image-preview-close",
            self.t("assistantImagePreviewClose", cx),
            Icon::new(FluxIcon::X).size(icon_size),
            side,
            CircleTone::Overlay,
            true,
            cx,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.close_image_preview(cx);
        }));
        h_flex()
            .absolute()
            .top(tokens.spacing.sm)
            .right(tokens.spacing.sm)
            .items_center()
            .gap(tokens.spacing.xs)
            .child(zoom_out)
            .child(percent)
            .child(zoom_in)
            .child(div().w(tokens.spacing.sm))
            .child(close)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::ImageFormat;

    use super::*;

    fn preview() -> ImagePreview {
        ImagePreview::new(Arc::new(Image::from_bytes(ImageFormat::Png, Vec::new())))
    }

    #[test]
    fn zoom_is_clamped_and_reports_changes() {
        let mut preview = preview();
        assert!(preview.zoom_by(100.));
        assert!((preview.zoom - MAX_ZOOM).abs() < f32::EPSILON);
        assert!(!preview.zoom_by(2.), "already at the maximum");
        assert!(preview.zoom_by(0.001));
        assert!((preview.zoom - MIN_ZOOM).abs() < f32::EPSILON);
        assert!(!preview.zoom_by(f32::NAN));
        assert!(!preview.zoom_by(0.));
        assert_eq!(preview.percent_label().as_ref(), "50%");
    }

    #[test]
    fn panning_only_moves_a_zoomed_image_and_scales_with_zoom() {
        let mut preview = preview();
        assert!(
            !preview.pan(point(px(10.), px(10.))),
            "fit image does not pan"
        );
        assert!(!preview.begin_drag(point(px(0.), px(0.))));
        preview.zoom_to(2.);
        assert!(preview.pan(point(px(10.), px(-6.))));
        assert_eq!(preview.offset, point(px(10.), px(-6.)));
        // 以中心缩放：平移量随倍数等比。
        preview.zoom_to(4.);
        assert_eq!(preview.offset, point(px(20.), px(-12.)));
        // 缩回适应及以下：平移清零。
        preview.zoom_to(1.);
        assert_eq!(preview.offset, Point::default());
    }

    #[test]
    fn dragging_pans_and_stops_when_the_button_is_released_elsewhere() {
        let mut preview = preview();
        preview.zoom_to(2.);
        assert!(preview.begin_drag(point(px(100.), px(100.))));
        assert!(preview.drag_to(point(px(130.), px(90.)), true));
        assert_eq!(preview.offset, point(px(30.), px(-10.)));
        assert!(preview.drag_to(point(px(200.), px(200.)), false));
        assert!(preview.drag_from.is_none());
        assert_eq!(preview.offset, point(px(30.), px(-10.)));
        assert!(
            !preview.drag_to(point(px(0.), px(0.)), true),
            "no drag in progress"
        );
    }

    #[test]
    fn double_click_toggles_between_fit_and_double_and_reset_restores_fit() {
        let mut preview = preview();
        assert!(preview.toggle());
        assert!((preview.zoom - DOUBLE_CLICK_ZOOM).abs() < f32::EPSILON);
        preview.pan(point(px(5.), px(5.)));
        assert!(preview.toggle());
        assert!((preview.zoom - 1.).abs() < f32::EPSILON);
        assert_eq!(preview.offset, Point::default());
        assert!(!preview.reset(), "already at fit");
    }
}
