//! 任务详情的「来源页」（`referrer`）交互：常规页副标题的来源站点、高级页「来源页」行。
//!
//! - http(s) 来源页渲染为链接：左键在系统浏览器打开，悬停下划线；
//!   其他协议（不可信输入，见 [`DownloadTaskView::source_page_url`]）只显示与复制。
//! - 右键菜单：打开来源页（仅 http(s)）/ 复制来源页（toast 反馈）。
//! - 高级页另有常驻复制按钮（勾选态反馈，同错误信息复制）：GPUI 文本不可选中，复制必须有显式入口。

use fluxdown_ui_components::{FluxIcon, IconControlExt as _, tooltip_view};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, PopupMenuItem},
    notification::Notification,
};

use super::{TaskDetailView, detail_row};
use crate::model::DownloadTaskView;

/// 来源页链接的固定文案（右键菜单与 toast 用，闭包内需 `'static`）。
#[derive(Clone)]
struct SourcePageLabels {
    open: SharedString,
    copy: SharedString,
    copied: SharedString,
}

impl TaskDetailView {
    fn source_page_labels(&self, cx: &App) -> SourcePageLabels {
        SourcePageLabels {
            open: self.t(cx, "detailOpenSourcePage"),
            copy: self.t(cx, "detailCopySourcePage"),
            copied: self.t(cx, "detailSourcePageCopied"),
        }
    }

    /// 常规页副标题「协议 · 来源站点」：来源页可打开时站点名即链接，悬停显示完整地址。
    pub(super) fn render_source_line(&self, row: &DownloadTaskView, cx: &App) -> Div {
        let tokens = active_theme(cx).tokens().clone();
        let site = SharedString::from(row.source_site().to_owned());
        let site = match row.source_page_url() {
            Some(url) => source_page_link(
                "detail-source-site",
                site,
                SharedString::from(url.to_owned()),
                true,
                self.source_page_labels(cx),
                cx,
            ),
            None => site.into_any_element(),
        };
        h_flex()
            .min_w_0()
            .gap(tokens.spacing.xs)
            .text_size(tokens.typography.xs.size)
            .line_height(tokens.typography.xs.line_height)
            .text_color(tokens.colors.muted_foreground)
            .child(div().flex_none().child(row.protocol.label()))
            .child(div().flex_none().child("·"))
            .child(div().min_w_0().truncate().child(site))
    }

    /// 高级页「来源页」行：未设置显示占位；否则完整地址（http(s) 为链接）+ 复制按钮。
    pub(super) fn render_source_page_row(
        &self,
        referrer: SharedString,
        is_web: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let label = self.t(cx, "infoSourcePage");
        if referrer.is_empty() {
            return detail_row(label, self.t(cx, "detailNotSet"), cx);
        }
        let tokens = active_theme(cx).tokens().clone();
        let success = active_theme(cx).extended().colors.success;
        let labels = self.source_page_labels(cx);
        let copied = self.source_page_copied != 0;
        let copy_tooltip = if copied {
            labels.copied.clone()
        } else {
            labels.copy.clone()
        };
        let copy_value = referrer.clone();
        let link = source_page_link(
            "detail-source-page",
            referrer.clone(),
            referrer,
            is_web,
            labels,
            cx,
        );
        detail_row(
            label,
            h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap(tokens.spacing.sm)
                .child(div().flex_1().min_w_0().child(link))
                .child(
                    Button::new("detail-copy-source-page")
                        .ghost()
                        .control_icon(cx)
                        .icon(if copied {
                            FluxIcon::Check
                        } else {
                            FluxIcon::Copy
                        })
                        .when(copied, |this| this.text_color(success))
                        .tooltip(copy_tooltip)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                copy_value.to_string(),
                            ));
                            this.flash_copied(cx, |this| &mut this.source_page_copied);
                        })),
                ),
            cx,
        )
    }
}

/// 来源页文本：`is_web` 时左键打开、悬停下划线并显示完整地址；右键菜单打开 / 复制。
/// 副标题里显示的是站点名（`text` ≠ `url`），因此悬停提示总是给出完整地址。
fn source_page_link(
    id: &'static str,
    text: SharedString,
    url: SharedString,
    is_web: bool,
    labels: SourcePageLabels,
    cx: &App,
) -> AnyElement {
    let accent = active_theme(cx).tokens().colors.primary;
    let open_url = url.clone();
    let tooltip = url.clone();
    div()
        .id(id)
        .min_w_0()
        .child(text)
        .when(is_web, move |this| {
            this.cursor_pointer()
                .text_color(accent)
                .hover(|style| style.underline())
                .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                .on_click(move |_, _, cx| cx.open_url(&open_url))
        })
        .context_menu(move |menu, _window, _cx| {
            let menu = if is_web {
                let url = url.clone();
                menu.item(
                    PopupMenuItem::new(labels.open.clone())
                        .icon(FluxIcon::ExternalLink)
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
            } else {
                menu
            };
            let url = url.clone();
            let copied = labels.copied.clone();
            menu.item(
                PopupMenuItem::new(labels.copy.clone())
                    .icon(FluxIcon::Copy)
                    .on_click(move |_, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
                        window.push_notification(Notification::success(copied.clone()), cx);
                    }),
            )
        })
        .into_any_element()
}
