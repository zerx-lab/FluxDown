//! 多条请求：可滚动的紧凑列表，每行带自己的「重命名 / 替换 / 跳过」按钮；批量按钮在底栏。

use fluxdown_protocol::{FileExistsAction, SelectionOutcome};
use fluxdown_ui_components::{ControlExt as _, tabular_numbers};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, IntoElement, ParentElement, Styled, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, button::Button, h_flex, scroll::ScrollableElement as _, v_flex,
};

use super::FileConflictView;
use crate::{components::file_icon::name_file_icon, model::file_conflict::FileConflictItem};

impl FileConflictView {
    pub(super) fn render_many(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let spacing = active_theme(cx).tokens().spacing;
        let mut rows = Vec::with_capacity(self.set.len());
        for (ix, item) in self.set.items().iter().enumerate() {
            rows.push(self.render_row(ix, item, window, cx));
        }
        let list = v_flex()
            .flex_1()
            .min_h_0()
            .gap(spacing.xs)
            .children(rows)
            .overflow_y_scrollbar();
        v_flex()
            .size_full()
            .px(spacing.md)
            .pb(spacing.md)
            .child(div().flex_1().min_h_0().child(list))
            .into_any_element()
    }

    fn render_row(
        &self,
        ix: usize,
        item: &FileConflictItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let busy = self.row_busy(&item.request_id);
        let unknown = self.strings.size_unknown.clone();
        let sizes = h_flex()
            .flex_none()
            .gap(tokens.spacing.xxs)
            .text_size(tokens.typography.xs.size)
            .line_height(tokens.typography.xs.line_height)
            .font_features(tabular_numbers())
            .text_color(tokens.colors.muted_foreground)
            .child(item.existing_size_text().unwrap_or_else(|| unknown.clone()))
            .child("→")
            .child(item.incoming_size_text().unwrap_or(unknown));

        let mut buttons = h_flex()
            .flex_none()
            .gap(tokens.spacing.xs)
            .child(self.row_button(
                ("file-conflict-row-rename", ix),
                self.strings.rename.clone(),
                FileExistsAction::Rename,
                &item.request_id,
                busy,
                cx,
            ));
        if item.allows(FileExistsAction::Overwrite) {
            buttons = buttons.child(self.row_button(
                ("file-conflict-row-overwrite", ix),
                self.strings.overwrite.clone(),
                FileExistsAction::Overwrite,
                &item.request_id,
                busy,
                cx,
            ));
        }
        if item.allows(FileExistsAction::Skip) {
            buttons = buttons.child(self.row_button(
                ("file-conflict-row-skip", ix),
                self.strings.skip.clone(),
                FileExistsAction::Skip,
                &item.request_id,
                busy,
                cx,
            ));
        }

        h_flex()
            .items_center()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.sm)
            .py(tokens.spacing.xs)
            .rounded(tokens.radius.md)
            .border(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .when(busy, |row| row.opacity(0.55))
            .child(name_file_icon(
                &item.name_fold,
                item.kind,
                &item.extension,
                extended.icon.lg * 1.25,
                window,
                cx,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(tokens.spacing.xxs)
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_middle()
                            .text_size(tokens.typography.sm.size)
                            .line_height(tokens.typography.sm.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .child(item.file_name.clone()),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(tokens.spacing.sm)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_middle()
                                    .text_size(tokens.typography.xs.size)
                                    .line_height(tokens.typography.xs.line_height)
                                    .text_color(tokens.colors.muted_foreground)
                                    .child(item.save_dir.clone()),
                            )
                            .child(sizes),
                    ),
            )
            .child(buttons)
            .into_any_element()
    }

    fn row_button(
        &self,
        id: (&'static str, usize),
        label: gpui::SharedString,
        action: FileExistsAction,
        request_id: &str,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> Button {
        let request_id = request_id.to_owned();
        Button::new(id)
            .outline()
            .control(cx)
            .label(label)
            .disabled(disabled)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.answer(
                    vec![request_id.clone()],
                    SelectionOutcome::FileExists { action },
                    window,
                    cx,
                );
            }))
    }
}
