//! 助手页的问题单卡片：草稿（可编辑、提交、丢弃）与处理后的状态。

use fluxdown_protocol::AssistantIssueDto;
use fluxdown_ui_components::{ButtonVariant, FluxIcon, button, card, field_label, loading_button};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Icon, h_flex,
    input::{Input, Textarea},
    v_flex,
};

use crate::{
    view::AssistantView,
    widgets::{badge, body_text, meta_text, result_label},
};

impl AssistantView {
    pub(crate) fn render_issue(&self, issue: &AssistantIssueDto, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = tokens.colors;
        let ext = theme.extended();
        let id = issue.id.clone();
        let busy = self.issue_busy.contains(&id);
        let editing = self
            .issue_editor
            .as_ref()
            .filter(|editor| editor.issue_id == id);
        let kind_key = if issue.kind == "feature" {
            "assistantIssueFeature"
        } else {
            "assistantIssueBug"
        };

        let mut body = v_flex().w_full().gap(tokens.spacing.sm).child(
            h_flex()
                .items_center()
                .gap(tokens.spacing.xs)
                .child(badge(cx).child(self.t(kind_key, cx)))
                .when(issue.status == "draft", |this| {
                    this.child(meta_text(cx).child(self.t("assistantIssueDraftHint", cx)))
                }),
        );

        if let Some(editor) = editing {
            let title_empty = editor.title.read(cx).value().trim().is_empty();
            let body_empty = editor.body.read(cx).value().trim().is_empty();
            let submit_id = id.clone();
            body = body
                .child(field_label(self.t("assistantIssueTitleLabel", cx), cx))
                .child(Input::new(&editor.title))
                .child(field_label(self.t("assistantIssueBodyLabel", cx), cx))
                .child(Textarea::new(&editor.body))
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .child(
                            loading_button(
                                SharedString::from(format!("assistant-issue-submit-{id}")),
                                self.t("assistantIssueSubmit", cx),
                                ButtonVariant::Primary,
                                busy,
                                cx,
                            )
                            .disabled(self.stale || busy || title_empty || body_empty)
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    let Some(editor) = &this.issue_editor else {
                                        return;
                                    };
                                    let title = editor.title.read(cx).value().trim().to_owned();
                                    let body = editor.body.read(cx).value().trim().to_owned();
                                    this.confirm_issue(submit_id.clone(), Some((title, body)), cx);
                                },
                            )),
                        )
                        .child(
                            button(
                                SharedString::from(format!("assistant-issue-cancel-{id}")),
                                self.t("assistantCancel", cx),
                                ButtonVariant::Secondary,
                                cx,
                            )
                            .disabled(busy)
                            .on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    this.cancel_issue_edit(cx);
                                },
                            )),
                        ),
                );
        } else {
            body = body
                .child(
                    body_text(cx)
                        .font_weight(FontWeight::MEDIUM)
                        .child(SharedString::from(issue.title.clone())),
                )
                .child(
                    body_text(cx)
                        .text_color(colors.muted_foreground)
                        .child(SharedString::from(issue.body.clone())),
                );
            if !issue.labels.is_empty() {
                body = body.child(
                    h_flex().flex_wrap().gap(tokens.spacing.xs).children(
                        issue
                            .labels
                            .iter()
                            .map(|label| badge(cx).child(SharedString::from(label.clone()))),
                    ),
                );
            }
            body = body.child(self.render_issue_status(issue, busy, cx));
        }

        card(cx)
            .w_full()
            .p(tokens.spacing.md)
            .border_color(ext.colors.hairline)
            .child(body)
            .into_any_element()
    }

    /// 问题单底部：草稿的操作按钮，或已处理后的状态说明。
    fn render_issue_status(
        &self,
        issue: &AssistantIssueDto,
        busy: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = tokens.colors;
        let ext = theme.extended();
        let id = issue.id.clone();

        if issue.status == "draft" {
            let confirm_id = id.clone();
            let edit_id = id.clone();
            let discard_id = id.clone();
            return h_flex()
                .flex_wrap()
                .gap(tokens.spacing.sm)
                .child(
                    loading_button(
                        SharedString::from(format!("assistant-issue-confirm-{id}")),
                        self.t("assistantIssueSubmit", cx),
                        ButtonVariant::Primary,
                        busy,
                        cx,
                    )
                    .disabled(self.stale || busy)
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.confirm_issue(confirm_id.clone(), None, cx);
                        },
                    )),
                )
                .child(
                    button(
                        SharedString::from(format!("assistant-issue-edit-{id}")),
                        self.t("assistantIssueEdit", cx),
                        ButtonVariant::Secondary,
                        cx,
                    )
                    .disabled(self.stale || busy)
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.edit_issue(&edit_id, window, cx);
                        },
                    )),
                )
                .child(
                    button(
                        SharedString::from(format!("assistant-issue-discard-{id}")),
                        self.t("assistantIssueDiscard", cx),
                        ButtonVariant::Ghost,
                        cx,
                    )
                    .disabled(self.stale || busy)
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.discard_issue(discard_id.clone(), cx);
                        },
                    )),
                )
                .into_any_element();
        }

        let (icon, key, tone) = match issue.status.as_str() {
            "pending_review" => (FluxIcon::Clock, "assistantIssuePending", ext.colors.warning),
            "submitted" => (
                FluxIcon::CircleCheck,
                "assistantIssueSubmitted",
                ext.colors.success,
            ),
            "rejected" => (FluxIcon::X, "assistantIssueRejected", colors.destructive),
            "failed" => (
                FluxIcon::CircleAlert,
                "assistantIssueFailed",
                colors.destructive,
            ),
            _ => (
                FluxIcon::X,
                "assistantIssueDiscarded",
                colors.muted_foreground,
            ),
        };
        let mut status = v_flex().w_full().gap(tokens.spacing.xs).child(result_label(
            icon,
            self.t(key, cx),
            tone,
            cx,
        ));
        let detail = issue
            .review_note
            .as_ref()
            .filter(|note| !note.trim().is_empty())
            .map(|note| self.with("assistantIssueReviewNote", &[("note", note)], cx))
            .or_else(|| {
                (issue.status == "failed")
                    .then(|| issue.error.clone())
                    .flatten()
                    .filter(|error| !error.trim().is_empty())
            });
        if let Some(detail) = detail {
            status = status.child(meta_text(cx).child(SharedString::from(detail)));
        }
        if let Some(url) = issue
            .github_url
            .as_ref()
            .filter(|url| url.starts_with("https://"))
        {
            let number = issue
                .github_number
                .map_or_else(String::new, |number| number.to_string());
            let url = url.clone();
            status = status.child(
                h_flex()
                    .id(SharedString::from(format!("assistant-issue-link-{id}")))
                    .items_center()
                    .gap(tokens.spacing.xxs)
                    .cursor_pointer()
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.open_url(&url)))
                    .child(
                        Icon::new(FluxIcon::ExternalLink)
                            .size(ext.icon.sm)
                            .text_color(ext.colors.accent_text),
                    )
                    .child(
                        meta_text(cx)
                            .text_color(ext.colors.accent_text)
                            .child(self.with(
                                "assistantIssueOpenGithub",
                                &[("number", &number)],
                                cx,
                            )),
                    ),
            );
        }
        status.into_any_element()
    }
}
