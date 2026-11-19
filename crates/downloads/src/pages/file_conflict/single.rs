//! 单条请求：文件卡片 + 新旧对比 + 动作列表（重命名 / 替换 / 跳过）。

use fluxdown_protocol::{FileExistsAction, SelectionOutcome};
use fluxdown_ui_components::{FluxIcon, card, tabular_numbers};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{Icon, h_flex, v_flex};

use super::FileConflictView;
use crate::{components::file_icon::name_file_icon, model::file_conflict::FileConflictItem};

/// 动作行的色调：主动作描边强调，替换用警示色，其余中性。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Primary,
    Warning,
    Neutral,
}

impl FileConflictView {
    pub(super) fn render_single(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.set.items().first() else {
            return div().into_any_element();
        };
        let tokens = active_theme(cx).tokens().clone();
        let busy = self.row_busy(&item.request_id);
        let request_id = item.request_id.clone();

        let rename_caption = SharedString::from(
            self.strings
                .rename_as
                .replace("{name}", &item.rename_preview),
        );
        let mut actions = vec![self.option_row(
            "file-conflict-rename",
            self.strings.rename.clone(),
            Some(rename_caption),
            Tone::Primary,
            busy,
            FileExistsAction::Rename,
            &request_id,
            cx,
        )];
        if item.allows(FileExistsAction::Overwrite) {
            actions.push(self.option_row(
                "file-conflict-overwrite",
                self.strings.overwrite.clone(),
                Some(self.strings.overwrite_hint.clone()),
                Tone::Warning,
                busy,
                FileExistsAction::Overwrite,
                &request_id,
                cx,
            ));
        }
        if item.allows(FileExistsAction::Skip) {
            actions.push(self.option_row(
                "file-conflict-skip",
                self.strings.skip.clone(),
                Some(self.strings.skip_hint.clone()),
                Tone::Neutral,
                busy,
                FileExistsAction::Skip,
                &request_id,
                cx,
            ));
        }

        let content = v_flex()
            .gap(tokens.spacing.md)
            .px(tokens.spacing.md)
            .pb(tokens.spacing.md)
            .child(self.file_card(item, window, cx))
            .child(self.compare(item, cx))
            .child(v_flex().gap(tokens.spacing.sm).children(actions));
        div()
            .id("file-conflict-single-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(content)
            .into_any_element()
    }

    /// 文件图标 + 文件名 + 保存目录（中间省略，两端的盘符 / 文件夹名都保留）。
    fn file_card(
        &self,
        item: &FileConflictItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let icon_size = theme.extended().icon.lg * 2.;
        h_flex()
            .items_center()
            .gap(tokens.spacing.md)
            .p(tokens.spacing.md)
            .child(name_file_icon(
                &item.name_fold,
                item.kind,
                &item.extension,
                icon_size,
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
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_middle()
                            .text_size(tokens.typography.xs.size)
                            .line_height(tokens.typography.xs.line_height)
                            .text_color(tokens.colors.muted_foreground)
                            .child(item.save_dir.clone()),
                    ),
            )
            .map(|row| card(cx).child(row))
    }

    /// 「已有文件」与「新下载」两列并排：大小 + 已有文件的修改时间。
    fn compare(&self, item: &FileConflictItem, cx: &Context<Self>) -> impl IntoElement {
        let spacing = active_theme(cx).tokens().spacing;
        let modified = item
            .existing_modified
            .as_ref()
            .map(|time| SharedString::from(self.strings.modified.replace("{time}", time)));
        h_flex()
            .items_stretch()
            .gap(spacing.sm)
            .child(self.compare_column(
                self.strings.existing.clone(),
                item.existing_size_text(),
                modified,
                cx,
            ))
            .child(self.compare_column(
                self.strings.incoming.clone(),
                item.incoming_size_text(),
                None,
                cx,
            ))
    }

    fn compare_column(
        &self,
        label: SharedString,
        size: Option<SharedString>,
        detail: Option<SharedString>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let known = size.is_some();
        card(cx).flex_1().min_w_0().p(tokens.spacing.md).child(
            v_flex()
                .gap(tokens.spacing.xxs)
                .child(
                    div()
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .text_color(tokens.colors.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .font_features(tabular_numbers())
                        .when(!known, |this| {
                            this.text_color(tokens.colors.muted_foreground)
                        })
                        .child(size.unwrap_or_else(|| self.strings.size_unknown.clone())),
                )
                .children(detail.map(|detail| {
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .font_features(tabular_numbers())
                        .text_color(theme.extended().colors.text_tertiary)
                        .child(detail)
                })),
        )
    }

    /// 一行可点击的动作：标题 + 说明，右侧箭头；答复在途或断线时整行变淡且不可点。
    #[allow(
        clippy::too_many_arguments,
        reason = "one row needs identity, copy, tone, state and the action it answers with"
    )]
    fn option_row(
        &self,
        id: &'static str,
        title: SharedString,
        caption: Option<SharedString>,
        tone: Tone,
        disabled: bool,
        action: FileExistsAction,
        request_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let (border, title_color): (Hsla, Hsla) = match tone {
            Tone::Primary => (tokens.colors.primary, tokens.colors.foreground),
            Tone::Warning => (extended.colors.hairline, extended.colors.warning),
            Tone::Neutral => (extended.colors.hairline, tokens.colors.foreground),
        };
        let hover = extended.colors.row_hover;
        let request_id = request_id.to_owned();
        h_flex()
            .id(id)
            .items_center()
            .gap(tokens.spacing.md)
            .px(tokens.spacing.md)
            .py(tokens.spacing.sm)
            .rounded(tokens.radius.md)
            .border(extended.stroke.thin)
            .border_color(border)
            .bg(tokens.colors.surface)
            .when(tone == Tone::Primary, |row| {
                row.bg(tokens.colors.primary.opacity(0.06))
            })
            .when(disabled, |row| row.opacity(0.55))
            .when(!disabled, |row| {
                row.cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.answer(
                            vec![request_id.clone()],
                            SelectionOutcome::FileExists { action },
                            window,
                            cx,
                        );
                    }))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(tokens.spacing.xxs)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(tokens.typography.sm.size)
                            .line_height(tokens.typography.sm.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(title_color)
                            .child(title),
                    )
                    .children(caption.map(|caption| {
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_size(tokens.typography.xs.size)
                            .line_height(tokens.typography.xs.line_height)
                            .text_color(tokens.colors.muted_foreground)
                            .child(caption)
                    })),
            )
            .child(
                Icon::new(FluxIcon::ChevronRight)
                    .size(extended.icon.md)
                    .text_color(tokens.colors.muted_foreground),
            )
            .into_any_element()
    }
}
