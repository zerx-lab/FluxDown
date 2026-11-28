//! 助手页绘制：根视图、说明页、对话流与每轮的提问 / 回答。历史会话在 `render_history.rs`，过程与确认
//! 卡片在 `render_steps.rs`，问题单在 `render_issue.rs`，输入区在 `render_composer.rs`。状态与动作
//! 在 `view.rs` 及其兄弟模块。

use fluxdown_protocol::AssistantRunStatus;
use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, FluxIcon, button, card, loading_button,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled, Window,
    div, prelude::FluentBuilder as _, px, relative,
};
use gpui_component::{
    Icon, IconName,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    scroll::ScrollableElement as _,
    v_flex,
};
use std::sync::Arc;

use crate::{
    actions::{Dismiss, HistoryNext, HistoryOpen, HistoryPrev, KEY_CONTEXT, NewChat},
    attachment::LocalImage,
    dates::local_now,
    history::Load,
    images::ImageSlot,
    markdown::{AnswerLabels, answer_view},
    model::{
        Gate, MAX_RATING_COMMENT_CHARS, Turn, pending_elsewhere, rating_comment, run_error_key,
        run_error_known, show_scroll_to_bottom,
    },
    view::{AssistantView, CopyKind, RateComment},
    widgets::{action_button, body_text, meta_text, spinner, thumbnail, thumbnail_slot},
};

/// 对话列的最大宽度（随文字缩放）。
pub(crate) const COLUMN_WIDTH: f32 = 800.;
/// 用户气泡的最大宽度。
const BUBBLE_WIDTH: f32 = 560.;
/// 说明页文案列宽度。
const NOTICE_WIDTH: f32 = 420.;
/// 回答 Markdown 的行高倍数。
const MARKDOWN_LINE_HEIGHT: f32 = 1.6;
/// 用户气泡下缩略图边长（随文字缩放）。
const BUBBLE_THUMBNAIL_SIDE: f32 = 44.;
/// 官网文档 / 常见问题入口。
const DOCS_URL: &str = "https://fluxdown.com/docs";

impl Render for AssistantView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 语言切换后同步输入框 placeholder。
        let placeholder = self.t("assistantInputPlaceholder", cx);
        if placeholder != self.input_placeholder {
            self.input_placeholder = placeholder.clone();
            self.input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx);
            });
        }
        let search_placeholder = self.t("assistantSearchPlaceholder", cx);
        if search_placeholder != self.search_placeholder {
            self.search_placeholder = search_placeholder.clone();
            self.search.update(cx, |input, cx| {
                input.set_placeholder(search_placeholder, window, cx);
            });
        }
        self.ensure_loaded(cx);
        self.flush_draft_reset(window, cx);
        self.flush_pending_submit(window, cx);
        let history = self.history_open && matches!(self.gate(), Gate::Open { .. });
        let body = if history {
            self.now_local = local_now();
            self.render_history(cx)
        } else {
            self.render_main(cx)
        };
        div()
            .id("assistant-root")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.root_focus)
            .on_action(cx.listener(|this, _: &NewChat, window, cx| {
                if matches!(this.gate(), Gate::Open { .. }) && !this.stale {
                    this.new_chat(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(cx.listener(|this, _: &HistoryPrev, _, cx| this.history_step(false, cx)))
            .on_action(cx.listener(|this, _: &HistoryNext, _, cx| this.history_step(true, cx)))
            .on_action(cx.listener(|this, _: &HistoryOpen, window, cx| {
                this.history_open_cursor(window, cx);
            }))
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .child(body)
            .when_some(self.image_preview.as_ref(), |this, preview| {
                this.child(self.render_image_preview(preview, cx))
            })
    }
}

impl AssistantView {
    // ───────────────────────── 主区域 ─────────────────────────

    fn render_main(&self, cx: &Context<Self>) -> AnyElement {
        match self.gate() {
            Gate::SignedOut => {
                let action = Button::new("assistant-sign-in")
                    .primary()
                    .label(self.t("assistantSignIn", cx))
                    .control(cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        (this.host.open_account)(window, cx);
                    }))
                    .into_any_element();
                self.render_notice(
                    FluxIcon::User,
                    "assistantSignedOutTitle",
                    "assistantSignedOutDesc",
                    vec![action],
                    cx,
                )
            }
            Gate::Loading => self.render_loading(cx),
            Gate::NotOpen => {
                // 说明页提到文档与常见问题：给一个直达官网文档的按钮。
                let docs = Button::new("assistant-open-docs")
                    .outline()
                    .label(self.t("assistantOpenDocs", cx))
                    .control(cx)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.open_url(DOCS_URL)))
                    .into_any_element();
                self.render_notice(
                    FluxIcon::Sparkles,
                    "assistantUnavailableTitle",
                    "assistantUnavailableDesc",
                    vec![docs],
                    cx,
                )
            }
            Gate::PlanDisabled => {
                // 套餐不含助手：直接打开套餐购买对话框，而不只是账户页。
                let action = Button::new("assistant-view-plans")
                    .primary()
                    .label(self.t("assistantViewPlans", cx))
                    .control(cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        (this.host.open_plan_purchase)(window, cx);
                    }))
                    .into_any_element();
                self.render_notice(
                    FluxIcon::Crown,
                    "assistantPlanTitle",
                    "assistantPlanDesc",
                    vec![action],
                    cx,
                )
            }
            Gate::ServiceError => {
                let action = loading_button(
                    "assistant-config-retry",
                    self.t("assistantRetry", cx),
                    ButtonVariant::Primary,
                    self.config_loading,
                    cx,
                )
                .disabled(self.stale || self.config_loading)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.fetch_config(true, cx);
                }))
                .into_any_element();
                self.render_notice(
                    FluxIcon::CircleAlert,
                    "assistantServiceTitle",
                    "assistantServiceDesc",
                    vec![action],
                    cx,
                )
            }
            Gate::Open { quota_exhausted } => self.render_chat(quota_exhausted, cx),
        }
    }

    /// 居中的说明页：图标、标题、说明与可选动作。
    pub(crate) fn render_notice(
        &self,
        icon: FluxIcon,
        title_key: &str,
        desc_key: &str,
        actions: Vec<AnyElement>,
        cx: &App,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = tokens.colors;
        let ext = theme.extended();
        let md = tokens.typography.md;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(tokens.spacing.md)
            .p(tokens.spacing.xl)
            .child(
                Icon::new(icon)
                    .size(px(32.))
                    .text_color(ext.colors.text_tertiary),
            )
            .child(
                div()
                    .text_size(md.size)
                    .line_height(md.line_height)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.foreground)
                    .child(self.t(title_key, cx)),
            )
            .child(
                div()
                    .max_w(theme.text_extent(NOTICE_WIDTH))
                    .text_center()
                    .child(
                        body_text(cx)
                            .text_color(colors.muted_foreground)
                            .child(self.t(desc_key, cx)),
                    ),
            )
            .when(!actions.is_empty(), |this| {
                this.child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .mt(tokens.spacing.xs)
                        .children(actions),
                )
            })
            .into_any_element()
    }

    pub(crate) fn render_loading(&self, cx: &App) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        h_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(tokens.spacing.sm)
            .child(spinner(theme.extended().icon.md))
            .child(meta_text(cx).child(self.t("assistantLoading", cx)))
            .into_any_element()
    }

    /// 可对话状态：提示条 + 对话流 + 输入区（标题与用量在面板顶栏）。
    fn render_chat(&self, quota_exhausted: bool, cx: &Context<Self>) -> AnyElement {
        let now_ms = self.agent_now();
        let mut column = v_flex().size_full().min_w_0();
        if self.stale {
            column = column.child(self.banner(self.t("localServiceDisconnected", cx), None, cx));
        }
        if let Some(other) = pending_elsewhere(&self.state, self.current.as_deref(), now_ms) {
            let open = Button::new("assistant-pending-open")
                .outline()
                .label(self.t("assistantPendingElsewhereOpen", cx))
                .control(cx)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.select_session(other.clone(), window, cx);
                }))
                .into_any_element();
            column =
                column.child(self.banner(self.t("assistantPendingElsewhere", cx), Some(open), cx));
        }
        // 当前会话的历史读取失败、但已有实时内容可显示：顶部给一条带重试的提示。
        if self.detail.load == Load::Failed
            && !self.timeline.is_empty()
            && let Some(session) = self.current.clone()
        {
            let retry = button(
                "assistant-detail-retry-inline",
                self.t("assistantRetry", cx),
                ButtonVariant::Secondary,
                cx,
            )
            .disabled(self.stale)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.load_detail(session.clone(), cx);
            }))
            .into_any_element();
            column =
                column.child(self.banner(self.t("assistantDetailFailed", cx), Some(retry), cx));
        }
        column = column.child(self.render_conversation(now_ms, cx));
        column = column.child(self.render_composer(quota_exhausted, cx));
        column.into_any_element()
    }

    /// 顶部提示条（警示色），可带一个动作。
    pub(crate) fn banner(
        &self,
        text: SharedString,
        action: Option<AnyElement>,
        cx: &App,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        h_flex()
            .flex_none()
            .items_center()
            .gap(tokens.spacing.md)
            .px(tokens.spacing.lg)
            .py(tokens.spacing.xs)
            .border_b(ext.stroke.thin)
            .border_color(ext.colors.hairline)
            .bg(ext.colors.warning.opacity(0.1))
            .child(
                Icon::new(FluxIcon::CircleAlert)
                    .size(ext.icon.sm)
                    .text_color(ext.colors.warning),
            )
            .child(body_text(cx).flex_1().min_w_0().child(text))
            .when_some(action, |this, action| this.child(action))
            .into_any_element()
    }

    // ───────────────────────── 对话流 ─────────────────────────

    fn render_conversation(&self, now_ms: i64, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let width = theme.text_extent(COLUMN_WIDTH);
        let outgoing = self.visible_outgoing();
        let empty = self.timeline.is_empty() && outgoing.is_none();
        let history_loading = self.current.is_some() && self.detail.load == Load::Loading && empty;
        let history_failed = self.current.is_some() && self.detail.load == Load::Failed && empty;

        let body: AnyElement = if history_loading {
            self.render_loading(cx)
        } else if history_failed {
            let session = self.current.clone().unwrap_or_default();
            let retry = button(
                "assistant-detail-retry",
                self.t("assistantRetry", cx),
                ButtonVariant::Secondary,
                cx,
            )
            .disabled(self.stale)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.load_detail(session.clone(), cx);
            }))
            .into_any_element();
            self.render_notice(
                FluxIcon::CircleAlert,
                "assistantDetailFailed",
                "assistantServiceDesc",
                vec![retry],
                cx,
            )
        } else if empty {
            self.render_notice(
                FluxIcon::Sparkles,
                "assistantEmptyTitle",
                "assistantEmptyDesc",
                Vec::new(),
                cx,
            )
        } else {
            let labels = AnswerLabels {
                copy_code: self.t("assistantCopyCode", cx),
                copied: self.t("assistantCopied", cx),
            };
            let mut list = v_flex()
                .w_full()
                .max_w(width)
                .mx_auto()
                .gap(tokens.spacing.xl)
                .px(tokens.spacing.lg)
                .py(tokens.spacing.lg);
            for (index, turn) in self.timeline.iter().enumerate() {
                let last = index + 1 == self.timeline.len() && outgoing.is_none();
                list = list.child(self.render_turn(turn, last, now_ms, &labels, cx));
            }
            if let Some(outgoing) = outgoing {
                list = list.child(self.render_outgoing(
                    &outgoing.text,
                    &outgoing.images,
                    &outgoing.image_ids,
                    outgoing.image_count,
                    cx,
                ));
            }
            list.into_any_element()
        };

        // 离底部超过一屏：显示「回到最新」。
        let show_latest = !empty
            && show_scroll_to_bottom(
                f32::from(self.scroll.max_offset().y),
                f32::from(self.scroll.offset().y),
                f32::from(self.scroll.bounds().size.height),
            );
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                div()
                    .id("assistant-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
            .vertical_scrollbar(&self.scroll)
            .when(show_latest, |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom(tokens.spacing.md)
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            button(
                                "assistant-scroll-bottom",
                                self.t("assistantScrollToBottom", cx),
                                ButtonVariant::Secondary,
                                cx,
                            )
                            .on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    this.scroll_to_latest(cx);
                                },
                            )),
                        ),
                )
            })
            .into_any_element()
    }

    /// 用户气泡：提问文字，靠右。
    fn question_bubble(&self, text: &str, cx: &App) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        body_text(cx)
            .max_w(theme.text_extent(BUBBLE_WIDTH))
            .px(tokens.spacing.md)
            .py(tokens.spacing.sm)
            .rounded(tokens.radius.lg)
            .bg(tokens.colors.muted)
            .child(SharedString::from(text.to_owned()))
            .into_any_element()
    }

    /// 已发出、还没被实时执行接管显示的提问：本地气泡 + 思考中。
    fn render_outgoing(
        &self,
        text: &str,
        thumbnails: &[LocalImage],
        image_ids: &[String],
        image_count: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let mut bubble_column = v_flex()
            .w_full()
            .items_end()
            .gap(tokens.spacing.xxs)
            .child(self.question_bubble(text, cx));
        if !thumbnails.is_empty() {
            bubble_column = bubble_column.child(self.sent_thumbnails("outgoing", thumbnails, cx));
        } else if !image_ids.is_empty() {
            // 沿用旧图（重新生成 / 编辑重发）：经附件缓存显示。
            bubble_column = bubble_column.child(self.history_images(image_ids, cx));
        } else if image_count > 0 {
            bubble_column = bubble_column.child(self.images_text(image_count, cx));
        }
        v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .child(bubble_column)
            .child(
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .child(spinner(theme.extended().icon.sm))
                    .child(meta_text(cx).child(self.t("assistantThinking", cx))),
            )
            .into_any_element()
    }

    fn images_text(&self, count: usize, cx: &App) -> AnyElement {
        meta_text(cx)
            .child(self.with("assistantImages", &[("count", &count.to_string())], cx))
            .into_any_element()
    }

    /// 提问下方的图片：刚发出的本地缩略图 > 历史附图 > 张数占位文字。
    fn render_question_images(
        &self,
        turn: &Turn,
        last: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(thumbnails) = self.local_thumbnails(turn, last) {
            return Some(self.sent_thumbnails(&turn.key, thumbnails, cx));
        }
        if !turn.image_ids.is_empty() {
            let history = self.history_images(&turn.image_ids, cx);
            let fresh = turn.new_image_count();
            // 编辑重发时新附的图在云端历史里没有附件 ID：只显示张数。
            return Some(if fresh > 0 {
                v_flex()
                    .items_end()
                    .gap(active_theme(cx).tokens().spacing.xxs)
                    .child(history)
                    .child(self.images_text(fresh, cx))
                    .into_any_element()
            } else {
                history
            });
        }
        (turn.image_count > 0).then(|| self.images_text(turn.image_count, cx))
    }

    /// 历史提问的附图：已解码的缩略图；读取中转圈；失败显示占位，点一下重试。
    fn history_images(&self, ids: &[String], cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let side = theme.text_extent(BUBBLE_THUMBNAIL_SIDE);
        let mut row = h_flex()
            .max_w(theme.text_extent(BUBBLE_WIDTH))
            .flex_wrap()
            .justify_end()
            .gap(tokens.spacing.xs);
        for (index, id) in ids.iter().enumerate() {
            let cell: AnyElement = match self.images.get(id) {
                Some(ImageSlot::Ready(image)) => self
                    .previewable_thumbnail(
                        SharedString::from(format!("assistant-image-ready-{id}")),
                        index + 1,
                        image,
                        side,
                        cx,
                    )
                    .into_any_element(),
                Some(ImageSlot::Failed) => {
                    let label = self.with(
                        "assistantImageLoadFailed",
                        &[("n", &(index + 1).to_string())],
                        cx,
                    );
                    let retry_id = id.clone();
                    thumbnail_slot(side, cx)
                        .id(SharedString::from(format!("assistant-image-failed-{id}")))
                        .cursor_pointer()
                        .tooltip(move |window, cx| {
                            fluxdown_ui_components::tooltip_view(label.clone(), window, cx)
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.fetch_image(retry_id.clone(), cx);
                        }))
                        .child(
                            Icon::new(FluxIcon::CircleAlert)
                                .size(ext.icon.sm)
                                .text_color(tokens.colors.destructive),
                        )
                        .into_any_element()
                }
                Some(ImageSlot::Loading) | None => thumbnail_slot(side, cx)
                    .child(spinner(ext.icon.sm))
                    .into_any_element(),
            };
            row = row.child(cell);
        }
        row.into_any_element()
    }

    fn render_turn(
        &self,
        turn: &Turn,
        last: bool,
        now_ms: i64,
        labels: &AnswerLabels,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let icon_sm = ext.icon.sm;
        let colors = tokens.colors;
        let running = turn.run_active();

        let mut bubble_column = v_flex()
            .w_full()
            .items_end()
            .gap(tokens.spacing.xxs)
            .child(self.question_bubble(&turn.question, cx));
        if let Some(images) = self.render_question_images(turn, last, cx) {
            bubble_column = bubble_column.child(images);
        }
        if turn.user_message_id.is_some() && !running && self.can_resend() {
            let key = turn.key.clone();
            bubble_column = bubble_column.child(
                action_button(
                    SharedString::from(format!("assistant-edit-{}", turn.key)),
                    self.t("assistantEditResend", cx),
                    Icon::new(FluxIcon::Pen).size(ext.icon.md),
                    colors.muted_foreground,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.start_edit(&key, window, cx);
                })),
            );
        }

        let mut reply = v_flex().w_full().gap(tokens.spacing.sm);
        if turn.is_observing() && running {
            reply = reply.child(meta_text(cx).child(self.t("assistantRunObserving", cx)));
        }
        if !turn.steps.is_empty() {
            reply = reply.child(self.render_steps(turn, now_ms, cx));
        }
        if let Some(answer) = &turn.answer {
            reply = reply.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(tokens.typography.sm.size)
                    .line_height(relative(MARKDOWN_LINE_HEIGHT))
                    .text_color(colors.foreground)
                    .child(answer_view(&turn.key, answer, labels, |url, cx| {
                        cx.open_url(url);
                    })),
            );
            if turn.answer_interrupted {
                reply = reply.child(
                    meta_text(cx)
                        .text_color(ext.colors.warning)
                        .child(self.t("assistantAnswerInterrupted", cx)),
                );
            }
        } else if running && turn.steps.is_empty() {
            reply = reply.child(
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .child(spinner(icon_sm))
                    .child(meta_text(cx).child(self.t("assistantThinking", cx))),
            );
        }
        if let Some(live) = &turn.live {
            match live.status {
                AssistantRunStatus::Failed => {
                    let reason = live.reason.as_deref();
                    let mut note = v_flex().gap(tokens.spacing.xxs).child(
                        body_text(cx)
                            .text_color(colors.destructive)
                            .child(self.t("assistantRunFailed", cx)),
                    );
                    note = note.child(meta_text(cx).child(self.t(run_error_key(reason), cx)));
                    // 未识别的失败原因：附带原文，便于反馈。
                    if !run_error_known(reason)
                        && let Some(error) = live.error.as_ref().filter(|error| !error.is_empty())
                    {
                        note = note.child(meta_text(cx).child(SharedString::from(error.clone())));
                    }
                    reply = reply.child(note);
                }
                AssistantRunStatus::Canceled => {
                    reply = reply.child(meta_text(cx).child(self.t("assistantStopped", cx)));
                }
                AssistantRunStatus::Starting
                | AssistantRunStatus::Running
                | AssistantRunStatus::Completed
                | AssistantRunStatus::Replied => {}
            }
        }
        if !running {
            reply = reply.child(self.render_turn_actions(turn, last, cx));
            if let Some(comment) = self
                .rate_comment
                .as_ref()
                .filter(|comment| turn.rateable_message() == Some(comment.message_id.as_str()))
            {
                reply = reply.child(self.render_rate_comment(comment, cx));
            }
        }
        if turn.membership_offer {
            reply = reply.child(self.render_membership_offer(&turn.key, cx));
        }
        for issue in &turn.issues {
            reply = reply.child(self.render_issue(issue, cx));
        }

        v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .child(bubble_column)
            .child(reply)
            .into_any_element()
    }

    /// 一轮回答下方的操作行：复制本轮 / 复制回答 / 评价 / 重新生成。
    fn render_turn_actions(&self, turn: &Turn, last: bool, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let muted = tokens.colors.muted_foreground;
        let mut actions = h_flex()
            .items_center()
            .gap(tokens.spacing.xxs)
            .child(self.render_copy(&turn.key, CopyKind::Turn, cx));
        if turn.answer.is_some() {
            actions = actions.child(self.render_copy(&turn.key, CopyKind::Answer, cx));
        }
        if let Some(message_id) = turn.rateable_message() {
            actions = actions.child(self.render_rating(message_id, turn.rating.as_deref(), cx));
        }
        // 失败轮次：一键重试（已落库的替换原提问，未落库的重发同一问题）；其余沿用重新生成。
        if last && self.retry_available().is_some() {
            actions = actions.child(
                action_button(
                    SharedString::from(format!("assistant-retry-{}", turn.key)),
                    self.t("assistantRetry", cx),
                    Icon::new(FluxIcon::RotateCw).size(ext.icon.md),
                    muted,
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.retry_failed(window, cx);
                })),
            );
        } else if last && turn.user_message_id.is_some() && self.can_regenerate() {
            actions = actions.child(
                action_button(
                    SharedString::from(format!("assistant-regenerate-{}", turn.key)),
                    self.t("assistantRegenerate", cx),
                    Icon::new(FluxIcon::RotateCw).size(ext.icon.md),
                    muted,
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.regenerate(window, cx);
                })),
            );
        }
        actions.into_any_element()
    }

    /// 复制按钮：整轮（含过程）或只复制回答；复制后短暂显示勾。
    fn render_copy(&self, turn_key: &str, kind: CopyKind, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let ext = theme.extended();
        let copied = self
            .copied
            .as_ref()
            .is_some_and(|(key, copied_kind)| key == turn_key && *copied_kind == kind);
        let label = self.t(
            if copied {
                "assistantCopied"
            } else if kind == CopyKind::Turn {
                "assistantCopyTurn"
            } else {
                "assistantCopyAnswer"
            },
            cx,
        );
        let key = turn_key.to_owned();
        let id = match kind {
            CopyKind::Turn => format!("assistant-copy-{turn_key}"),
            CopyKind::Answer => format!("assistant-copy-answer-{turn_key}"),
        };
        action_button(
            SharedString::from(id),
            label,
            Icon::new(match (copied, kind) {
                (true, _) => FluxIcon::Check,
                (false, CopyKind::Turn) => FluxIcon::Copy,
                (false, CopyKind::Answer) => FluxIcon::FileText,
            })
            .size(ext.icon.md),
            if copied {
                ext.colors.success
            } else {
                theme.tokens().colors.muted_foreground
            },
            cx,
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| match kind {
            CopyKind::Turn => this.copy_turn(&key, cx),
            CopyKind::Answer => this.copy_answer(&key, cx),
        }))
        .into_any_element()
    }

    /// 一条回答下的赞 / 踩：只有图标，没有边框与底色框；选中的图标变色（赞 = 强调色，
    /// 踩 = 危险色），未选中为弱色，悬停只淡淡加一层底。`server` 为云端历史里该回答的评价。
    fn render_rating(
        &self,
        message_id: &str,
        server: Option<&str>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let shown = self.shown_rating(message_id, server);
        let muted = tokens.colors.muted_foreground;
        let up_tone = if shown == Some("up") {
            ext.colors.accent_text
        } else {
            muted
        };
        let down_tone = if shown == Some("down") {
            tokens.colors.destructive
        } else {
            muted
        };
        let server = server.map(str::to_owned);
        let build = |value: &'static str, icon: IconName, label: SharedString, tone| {
            let target = message_id.to_owned();
            let server = server.clone();
            action_button(
                SharedString::from(format!("assistant-rate-{value}-{message_id}")),
                label,
                Icon::new(icon).size(ext.icon.md),
                tone,
                cx,
            )
            .disabled(self.stale)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.rate(target.clone(), value, server.clone(), window, cx);
            }))
        };
        h_flex()
            .items_center()
            .gap(tokens.spacing.xxs)
            .child(build(
                "up",
                IconName::ThumbsUp,
                self.t("assistantRateUp", cx),
                up_tone,
            ))
            .child(build(
                "down",
                IconName::ThumbsDown,
                self.t("assistantRateDown", cx),
                down_tone,
            ))
            .into_any_element()
    }

    /// 开通会员卡片：不带套餐数据（价格实时取），按钮打开现有套餐购买对话框。
    fn render_membership_offer(&self, turn_key: &str, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let colors = tokens.colors;

        let heading = h_flex()
            .items_center()
            .gap(tokens.spacing.xs)
            .child(
                Icon::new(FluxIcon::Crown)
                    .size(ext.icon.sm)
                    .text_color(ext.colors.warning),
            )
            .child(
                body_text(cx)
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.t("assistantMembershipTitle", cx)),
            );
        let action = Button::new(SharedString::from(format!(
            "assistant-membership-{turn_key}"
        )))
        .primary()
        .label(self.t("assistantMembershipAction", cx))
        .control(cx)
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            (this.host.open_plan_purchase)(window, cx);
        }));

        card(cx)
            .w_full()
            .p(tokens.spacing.md)
            .border_color(ext.colors.hairline)
            .child(
                v_flex()
                    .w_full()
                    .gap(tokens.spacing.sm)
                    .child(heading)
                    .child(
                        body_text(cx)
                            .text_color(colors.muted_foreground)
                            .child(self.t("assistantMembershipBody", cx)),
                    )
                    .child(h_flex().child(action)),
            )
            .into_any_element()
    }

    /// 可点开大图的缩略图：显示缩略图，点开看原图。
    pub(crate) fn previewable_thumbnail(
        &self,
        id: SharedString,
        number: usize,
        image: &LocalImage,
        side: Pixels,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let target = Arc::clone(&image.full);
        let label = self.with("assistantImagePreview", &[("n", &number.to_string())], cx);
        thumbnail(&image.thumbnail, side, cx)
            .id(id)
            .cursor_pointer()
            .tooltip(move |window, cx| {
                fluxdown_ui_components::tooltip_view(label.clone(), window, cx)
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                // 焦点回到面板根，Esc 才能关闭预览。
                window.focus(&this.root_focus, cx);
                this.open_image_preview(Arc::clone(&target), cx);
            }))
    }

    /// 用户气泡下方、本地已知的图片缩略图（靠右，与气泡对齐）。`scope` 区分不同轮次的元素 ID。
    fn sent_thumbnails(
        &self,
        scope: &str,
        thumbnails: &[LocalImage],
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let side = theme.text_extent(BUBBLE_THUMBNAIL_SIDE);
        h_flex()
            .max_w(theme.text_extent(BUBBLE_WIDTH))
            .flex_wrap()
            .justify_end()
            .gap(theme.tokens().spacing.xs)
            .children(thumbnails.iter().enumerate().map(|(index, image)| {
                self.previewable_thumbnail(
                    SharedString::from(format!("assistant-sent-thumb-{scope}-{index}")),
                    index + 1,
                    image,
                    side,
                    cx,
                )
            }))
            .into_any_element()
    }

    /// 点踩后就地出现的可选说明：输入框 + 提交 / 跳过；提交后短暂显示感谢。
    fn render_rate_comment(&self, comment: &RateComment, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        if comment.thanks {
            return meta_text(cx)
                .child(self.t("assistantRateCommentThanks", cx))
                .into_any_element();
        }
        let chars = self.rate_comment_chars(cx);
        let over = chars > MAX_RATING_COMMENT_CHARS;
        let counter = meta_text(cx)
            .when(over, |this| this.text_color(tokens.colors.destructive))
            .child(self.with(
                "assistantRenameCount",
                &[
                    ("count", &chars.to_string()),
                    ("max", &MAX_RATING_COMMENT_CHARS.to_string()),
                ],
                cx,
            ));
        // 此前的评价请求（不带说明）还在途中时不能提交，免得它后到把云端的说明清空。
        let can_submit = !self.stale
            && !comment.busy
            && !self.rating_busy.contains(&comment.message_id)
            && rating_comment(&comment.input.read(cx).value()).is_some();
        let submit = loading_button(
            "assistant-rate-comment-submit",
            self.t("assistantRateCommentSubmit", cx),
            ButtonVariant::Primary,
            comment.busy,
            cx,
        )
        .disabled(!can_submit)
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.submit_rate_comment(cx)));
        let skip = button(
            "assistant-rate-comment-skip",
            self.t("assistantRateCommentSkip", cx),
            ButtonVariant::Ghost,
            cx,
        )
        .disabled(comment.busy)
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_rate_comment(cx)));
        v_flex()
            .w_full()
            .gap(tokens.spacing.xs)
            .child(meta_text(cx).child(self.t("assistantRateCommentPrompt", cx)))
            .child(Input::new(&comment.input).control(cx).w_full())
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(tokens.spacing.sm)
                    .child(counter)
                    .child(h_flex().gap(tokens.spacing.xs).child(skip).child(submit)),
            )
            .into_any_element()
    }
}
