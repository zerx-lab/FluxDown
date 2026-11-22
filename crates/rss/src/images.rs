//! On-demand image preview for one RSS item.

use std::sync::Arc;

use fluxdown_protocol::ErrorReason;
use fluxdown_ui_components::{dialog_scroll_body, dialog_title};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, Context, Entity, Image, ImageFormat, IntoElement, ObjectFit,
    ParentElement, Render, RenderImage, SharedString, Styled, StyledImage as _, Task, Window, div,
    img, px,
};
use gpui_component::{WindowExt as _, v_flex};

use crate::RssPort;

const MAX_IMAGES: usize = 4;

enum Preview {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
    ProxyDestinationUnverifiable,
}

struct ImageDialog {
    translator: Entity<Translator>,
    previews: Vec<Preview>,
    // Closing the dialog releases this entity and cancels its outstanding requests.
    loads: Vec<Task<()>>,
}

pub(super) fn open(
    translator: Entity<Translator>,
    port: Arc<dyn RssPort>,
    source_id: &str,
    guid: &str,
    count: u8,
    window: &mut Window,
    cx: &mut App,
) {
    let count = usize::from(count).min(MAX_IMAGES);
    let dialog = cx.new(|cx: &mut Context<ImageDialog>| {
        let svg_renderer = cx.svg_renderer();
        let loads = (0..count)
            .map(|index| {
                let load = port.image(source_id.to_owned(), guid.to_owned(), index);
                let svg_renderer = svg_renderer.clone();
                cx.spawn(async move |dialog, cx| {
                    let preview = match load.await {
                        Ok(bytes) => {
                            let decoded = cx
                                .background_spawn(async move {
                                    Image::from_bytes(ImageFormat::Png, bytes)
                                        .to_image_data(svg_renderer)
                                })
                                .await;
                            match decoded {
                                Ok(image) => Preview::Ready(image),
                                Err(_) => Preview::Failed,
                            }
                        }
                        Err(error)
                            if error.reason == Some(ErrorReason::RssImageProxyUnverifiable) =>
                        {
                            Preview::ProxyDestinationUnverifiable
                        }
                        Err(_) => Preview::Failed,
                    };
                    let Ok(()) = dialog.update(cx, |dialog, cx| {
                        if let Some(slot) = dialog.previews.get_mut(index) {
                            *slot = preview;
                            cx.notify();
                        }
                    }) else {
                        // This request belonged to a dialog that was closed.
                        return;
                    };
                })
            })
            .collect();
        ImageDialog {
            translator: translator.clone(),
            previews: (0..count).map(|_| Preview::Loading).collect(),
            loads,
        }
    });
    window.open_dialog(cx, move |builder, window, cx| {
        let title = SharedString::from(translator.read(cx).text("rssViewImages").to_owned());
        let width = active_theme(cx)
            .text_extent(640.)
            .min(window.viewport_size().width * 0.9);
        let dialog = dialog.clone();
        let closing_dialog = dialog.clone();
        builder
            .title(dialog_title(title, cx))
            .w(width)
            .content(move |content, _, _| content.min_h_0().child(dialog.clone()))
            .on_close(move |_, _, cx| {
                closing_dialog.update(cx, |dialog, _| dialog.loads.clear());
            })
    });
}

impl Render for ImageDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens();
        let translator = self.translator.read(cx);
        let height = (window.viewport_size().height * 0.55).min(px(480.));
        let body = v_flex()
            .gap(tokens.spacing.md)
            .children(self.previews.iter().map(|preview| {
                let (key, color) = match preview {
                    Preview::Ready(image) => {
                        return img(image.clone())
                            .w_full()
                            .h(height)
                            .object_fit(ObjectFit::Contain)
                            .into_any_element();
                    }
                    Preview::Loading => ("rssImageLoading", tokens.colors.muted_foreground),
                    Preview::Failed => ("rssImageLoadFailed", tokens.colors.destructive),
                    Preview::ProxyDestinationUnverifiable => {
                        ("rssImageProxyUnverifiable", tokens.colors.destructive)
                    }
                };
                div()
                    .text_color(color)
                    .child(SharedString::from(translator.text(key).to_owned()))
                    .into_any_element()
            }));
        dialog_scroll_body("rss-image-previews", None, body, cx)
    }
}
