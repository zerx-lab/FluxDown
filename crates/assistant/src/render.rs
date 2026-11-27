//! 助手页绘制：会话列表、对话流、确认卡片、问题单与输入区。状态与动作在 `view.rs`。

use std::{sync::Arc, time::Duration};

use fluxdown_protocol::{
    AssistantIssueDto, AssistantRunStatus, AssistantSessionDto, AssistantSessionKind,
    AssistantToolCallDto, AssistantToolEffect,
};
use fluxdown_ui_components::{
    Button as ThemedButton, ButtonVariant, ControlExt as _, FluxIcon, StateLayer, button, card,
    choice_chip, field_error, field_label, icon_button, loading_button, tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClickEvent, Context, Div, ElementId,
    ExternalPaths, FontWeight, Hsla, Image, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled,
    StyledImage as _, Window, div, img, prelude::FluentBuilder as _, px, relative,
    transparent_black,
};
use gpui_component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, Textarea},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    text::TextView,
    v_flex,
};

use crate::{
    model::{
        Block, ConfirmView, Gate, SendFailure, ToolState, Turn, arguments_pretty,
        arguments_summary, build_timeline, confirm_view, format_countdown, format_duration,
        pending_elsewhere, remaining_seconds, steps_open, tool_output_display, tool_state,
        tool_status_key,
    },
    view::{AssistantView, Load, now_unix_ms},
};

/// 对话列的最大宽度（随文字缩放）。
const COLUMN_WIDTH: f32 = 800.;
/// 用户气泡的最大宽度。
const BUBBLE_WIDTH: f32 = 560.;
/// 说明页文案列宽度。
const NOTICE_WIDTH: f32 = 420.;
/// 回答 Markdown 的行高倍数。
const MARKDOWN_LINE_HEIGHT: f32 = 1.6;
/// 会话日期取 `updated_at` 的前 10 位（`YYYY-MM-DD`）。
const SESSION_DATE_CHARS: usize = 10;
/// 输入区缩略图边长（随文字缩放）。
const THUMBNAIL_SIDE: f32 = 56.;
/// 用户气泡下缩略图边长。
const BUBBLE_THUMBNAIL_SIDE: f32 = 44.;
/// 缩略图角上移除按钮的直径。
const REMOVE_BUTTON_SIDE: f32 = 18.;
/// 发送 / 停止 / 添加图片按钮的直径。
const ROUND_BUTTON_SIDE: f32 = 32.;
/// 赞 / 踩图标按钮的边长。
const RATE_BUTTON_SIDE: f32 = 24.;

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
        self.ensure_loaded(cx);
        self.flush_pending_submit(window, cx);
        let body = if self.history_open && matches!(self.gate(), Gate::Open { .. }) {
            self.render_history(cx)
        } else {
            self.render_main(cx)
        };
        div()
            .key_context(crate::actions::KEY_CONTEXT)
            .on_action(
                cx.listener(|this, _: &crate::actions::NewChat, window, cx| {
                    this.new_chat(window, cx);
                }),
            )
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(body)
    }
}

impl AssistantView {
    // ───────────────────────── 主区域 ─────────────────────────

    fn render_main(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
            Gate::NotOpen => self.render_notice(
                FluxIcon::Sparkles,
                "assistantUnavailableTitle",
                "assistantUnavailableDesc",
                Vec::new(),
                cx,
            ),
            Gate::PlanDisabled => {
                let action = Button::new("assistant-view-plans")
                    .primary()
                    .label(self.t("assistantViewPlans", cx))
                    .control(cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        (this.host.open_account)(window, cx);
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
    fn render_notice(
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

    fn render_loading(&self, cx: &App) -> AnyElement {
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
    fn render_chat(&mut self, quota_exhausted: bool, cx: &mut Context<Self>) -> AnyElement {
        let now_ms = now_unix_ms();

        let session_id = self.current.clone().unwrap_or_default();
        let turns = build_timeline(
            &session_id,
            &self.detail.messages,
            &self.detail.issues,
            &self.state.runs,
            &self.issue_overrides,
        );

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
        column = column.child(self.render_conversation(&turns, now_ms, cx));
        column = column.child(self.render_composer(quota_exhausted, &turns, cx));
        column.into_any_element()
    }

    /// 顶部提示条（警示色），可带一个动作。
    fn banner(&self, text: SharedString, action: Option<AnyElement>, cx: &App) -> AnyElement {
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

    fn render_conversation(
        &mut self,
        turns: &[Turn],
        now_ms: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let width = theme.text_extent(COLUMN_WIDTH);
        let history_loading =
            self.current.is_some() && self.detail.load == Load::Loading && turns.is_empty();
        let history_failed =
            self.current.is_some() && self.detail.load == Load::Failed && turns.is_empty();

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
        } else if turns.is_empty() {
            self.render_notice(
                FluxIcon::Sparkles,
                "assistantEmptyTitle",
                "assistantEmptyDesc",
                Vec::new(),
                cx,
            )
        } else {
            let mut list = v_flex()
                .w_full()
                .max_w(width)
                .mx_auto()
                .gap(tokens.spacing.xl)
                .px(tokens.spacing.lg)
                .py(tokens.spacing.lg);
            for (index, turn) in turns.iter().enumerate() {
                let last = index + 1 == turns.len();
                list = list.child(self.render_turn(turn, last, now_ms, cx));
            }
            list.into_any_element()
        };

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
            .into_any_element()
    }

    fn render_turn(
        &self,
        turn: &Turn,
        last: bool,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let bubble_width = theme.text_extent(BUBBLE_WIDTH);
        let icon_sm = theme.extended().icon.sm;
        let colors = tokens.colors;
        let running = turn.run_active();

        let mut bubble_column = v_flex().w_full().items_end().gap(tokens.spacing.xxs).child(
            body_text(cx)
                .max_w(bubble_width)
                .px(tokens.spacing.md)
                .py(tokens.spacing.sm)
                .rounded(tokens.radius.lg)
                .bg(colors.muted)
                .child(SharedString::from(turn.question.clone())),
        );
        match self.local_thumbnails(turn, last) {
            Some(thumbnails) => {
                bubble_column = bubble_column.child(sent_thumbnails(thumbnails, cx));
            }
            None if turn.image_count > 0 => {
                bubble_column = bubble_column.child(meta_text(cx).child(self.with(
                    "assistantImages",
                    &[("count", &turn.image_count.to_string())],
                    cx,
                )));
            }
            None => {}
        }

        let mut reply = v_flex().w_full().gap(tokens.spacing.sm);
        if !turn.steps.is_empty() {
            reply = reply.child(self.render_steps(turn, now_ms, cx));
        }
        if let Some(answer) = &turn.answer {
            reply = reply.child(markdown_block(&turn.key, answer, cx));
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
                    let mut note = v_flex().gap(tokens.spacing.xxs).child(
                        body_text(cx)
                            .text_color(colors.destructive)
                            .child(self.t("assistantRunFailed", cx)),
                    );
                    if let Some(error) = live.error.as_ref().filter(|error| !error.is_empty()) {
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
            let mut actions = h_flex()
                .items_center()
                .gap(tokens.spacing.xxs)
                .child(self.render_copy_turn(&turn.key, cx));
            if let Some(message_id) = turn.rateable_message() {
                actions = actions.child(self.render_rating(message_id, turn.rating.as_deref(), cx));
            }
            reply = reply.child(actions);
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

    // ───────────────────────── 过程与工具调用 ─────────────────────────

    fn render_steps(&self, turn: &Turn, now_ms: i64, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = tokens.colors;
        let ext = theme.extended();
        let running = turn.run_active();
        let open = steps_open(turn, self.steps_choice.get(&turn.key).copied(), now_ms);
        let tools = turn.tool_count();
        let key = turn.key.clone();
        let count_label = (tools > 0)
            .then(|| self.with("assistantStepsCount", &[("count", &tools.to_string())], cx));

        let header = h_flex()
            .id(SharedString::from(format!("assistant-steps-{}", turn.key)))
            .items_center()
            .gap(tokens.spacing.xs)
            .py(tokens.spacing.xxs)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_steps(&key, open, cx);
            }))
            .child(
                Icon::new(if open {
                    FluxIcon::ChevronDown
                } else {
                    FluxIcon::ChevronRight
                })
                .size(ext.icon.sm)
                .text_color(ext.colors.text_tertiary),
            )
            .child(
                meta_text(cx)
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.t("assistantStepsTitle", cx)),
            )
            .when_some(count_label, |this, label| {
                this.child(meta_text(cx).child(label))
            })
            .when(running && turn.answer.is_none(), |this| {
                this.child(spinner(ext.icon.sm))
            });

        let mut container = v_flex().w_full().gap(tokens.spacing.xxs).child(header);
        if open {
            let mut list = v_flex()
                .w_full()
                .gap(tokens.spacing.sm)
                .pl(tokens.spacing.md)
                .ml(tokens.spacing.xs)
                .border_l(ext.stroke.thin)
                .border_color(ext.colors.hairline);
            for block in &turn.steps {
                match block {
                    Block::Text(text) => {
                        list = list.child(
                            body_text(cx)
                                .text_color(colors.muted_foreground)
                                .child(SharedString::from(text.clone())),
                        );
                    }
                    Block::Tool(call) => {
                        list = list.child(self.render_tool(call, running, now_ms, cx));
                    }
                    // 问题单草稿在 `Turn::issues` 单独渲染，不会进过程列表。
                    Block::Issue(_) => {}
                }
            }
            container = container.child(list);
        }
        container.into_any_element()
    }

    fn render_tool(
        &self,
        call: &AssistantToolCallDto,
        run_active: bool,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = tokens.colors;
        let ext = theme.extended();
        let icon_size = ext.icon.sm;
        let state = tool_state(call, run_active);
        let confirmation = confirm_view(call, run_active, now_ms);
        let client = call.kind == "client";

        // 状态文案与图标：待确认 / 未执行优先于调用状态本身。
        let label_key = tool_status_key(confirmation, state);
        let tone = match label_key {
            "assistantToolWaiting" | "assistantToolInterrupted" => ext.colors.warning,
            "assistantToolSkipped" | "assistantToolRunning" => ext.colors.text_tertiary,
            "assistantToolDone" => ext.colors.success,
            _ => colors.destructive,
        };
        let icon: AnyElement = match (confirmation, state) {
            (Some(ConfirmView::Pending { .. }), _) => Icon::new(FluxIcon::Clock)
                .size(icon_size)
                .text_color(tone)
                .into_any_element(),
            (_, ToolState::Running) if confirmation.is_none() => {
                spinner(icon_size).into_any_element()
            }
            (_, ToolState::Succeeded)
                if !matches!(
                    confirmation,
                    Some(ConfirmView::Declined | ConfirmView::Expired)
                ) =>
            {
                Icon::new(FluxIcon::CircleCheck)
                    .size(icon_size)
                    .text_color(tone)
                    .into_any_element()
            }
            (Some(ConfirmView::Approved), ToolState::Running) => {
                spinner(icon_size).into_any_element()
            }
            _ => Icon::new(FluxIcon::CircleAlert)
                .size(icon_size)
                .text_color(tone)
                .into_any_element(),
        };

        // 详情（参数 / 结果）默认折叠，点工具行展开；有确认卡片的调用由卡片自己展示参数。
        let arguments = client
            .then_some(call.arguments.as_ref())
            .flatten()
            .filter(|arguments| arguments_summary(Some(arguments)).is_some())
            .map(arguments_pretty);
        let output = call
            .output_preview
            .as_deref()
            .filter(|preview| client && !preview.trim().is_empty())
            .map(tool_output_display);
        let expandable = confirmation.is_none() && (arguments.is_some() || output.is_some());
        let detail_key = format!("detail:{}", call.call_id);
        let detail_open = expandable && self.args_open.contains(&detail_key);

        let header = h_flex()
            .id(SharedString::from(format!(
                "assistant-tool-{}",
                call.call_id
            )))
            .w_full()
            .items_center()
            .gap(tokens.spacing.xs)
            .flex_wrap()
            .child(icon)
            .child(
                body_text(cx)
                    .font_family(tokens.typography.mono.clone())
                    .child(SharedString::from(call.name.clone())),
            )
            .when(client, |this| {
                this.child(badge(cx).child(self.t("assistantToolLocal", cx)))
            })
            .child(meta_text(cx).text_color(tone).child(self.t(label_key, cx)))
            .when_some(call.duration_ms, |this, duration| {
                this.child(meta_text(cx).child(format_duration(duration)))
            });
        let header = if expandable {
            let label = self.t(
                if detail_open {
                    "assistantToolDetailsHide"
                } else {
                    "assistantToolDetailsShow"
                },
                cx,
            );
            let hover = ext.colors.nav_hover;
            header
                .px(tokens.spacing.xxs)
                .mx(-tokens.spacing.xxs)
                .rounded(tokens.radius.sm)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .tooltip(move |window, cx| tooltip_view(label.clone(), window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_arguments(&detail_key, cx);
                }))
                .child(div().flex_1())
                .child(
                    Icon::new(if detail_open {
                        FluxIcon::ChevronDown
                    } else {
                        FluxIcon::ChevronRight
                    })
                    .size(icon_size)
                    .text_color(ext.colors.text_tertiary),
                )
        } else {
            header
        };

        let mut row = v_flex().w_full().gap(tokens.spacing.xxs).child(header);
        if detail_open {
            let section = |title: SharedString, text: String, cx: &App| {
                v_flex()
                    .w_full()
                    .gap(tokens.spacing.xxs)
                    .child(meta_text(cx).font_weight(FontWeight::MEDIUM).child(title))
                    .child(
                        meta_text(cx)
                            .w_full()
                            .text_color(colors.foreground)
                            .font_family(tokens.typography.mono.clone())
                            .child(SharedString::from(text)),
                    )
            };
            // 不设内部滚动：嵌在对话流滚动区里会与外层抢滚轮；结果本身已是截断后的摘要。
            let mut detail = v_flex()
                .w_full()
                .gap(tokens.spacing.sm)
                .p(tokens.spacing.sm)
                .rounded(tokens.radius.sm)
                .bg(colors.muted);
            if let Some(arguments) = arguments {
                detail = detail.child(section(self.t("assistantToolArguments", cx), arguments, cx));
            }
            if let Some(output) = output {
                detail = detail.child(section(self.t("assistantToolOutput", cx), output, cx));
            }
            row = row.child(detail);
        }
        if let Some(view) = confirmation {
            row = row.child(self.render_confirm(call, view, cx));
        }
        row.into_any_element()
    }

    /// 工具确认卡片：待确认时可允许 / 拒绝；过期或已处理后按钮禁用并显示结果。
    fn render_confirm(
        &self,
        call: &AssistantToolCallDto,
        view: ConfirmView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = tokens.colors;
        let ext = theme.extended();
        let request_id = call.request_id.clone();
        let busy = request_id
            .as_ref()
            .is_some_and(|id| self.confirming.contains(id));
        let enabled = view.is_pending() && !self.stale && !busy && request_id.is_some();

        let effect = match call.effect {
            AssistantToolEffect::Network => Some((FluxIcon::Globe, "assistantEffectNetwork")),
            AssistantToolEffect::Write => Some((FluxIcon::Pen, "assistantEffectWrite")),
            AssistantToolEffect::Read => None,
        };
        let status: AnyElement = match view {
            ConfirmView::Pending { remaining_ms } => match remaining_ms {
                Some(ms) => meta_text(cx)
                    .child(self.with(
                        "assistantConfirmRemaining",
                        &[("time", &format_countdown(remaining_seconds(ms)))],
                        cx,
                    ))
                    .into_any_element(),
                None => div().into_any_element(),
            },
            ConfirmView::Approved => result_label(
                FluxIcon::CircleCheck,
                self.t("assistantConfirmApproved", cx),
                ext.colors.success,
                cx,
            ),
            ConfirmView::Declined => result_label(
                FluxIcon::X,
                self.t("assistantConfirmDeclined", cx),
                colors.muted_foreground,
                cx,
            ),
            ConfirmView::Expired => result_label(
                FluxIcon::Clock,
                self.t("assistantConfirmExpired", cx),
                ext.colors.warning,
                cx,
            ),
        };

        let args_key = call.call_id.clone();
        let args_open = self.args_open.contains(&args_key);
        let summary = arguments_summary(call.arguments.as_ref());
        let pretty = call.arguments.as_ref().map(arguments_pretty);
        let toggle_key = args_key.clone();

        let allow_id = request_id.clone();
        let deny_id = request_id;
        let id_suffix = call.call_id.clone();

        let mut body = v_flex()
            .w_full()
            .gap(tokens.spacing.xs)
            .child(
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.xs)
                    .flex_wrap()
                    .child(
                        Icon::new(FluxIcon::CircleAlert)
                            .size(ext.icon.sm)
                            .text_color(ext.colors.warning),
                    )
                    .child(
                        body_text(cx)
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.t("assistantConfirmTitle", cx)),
                    )
                    .when_some(effect, |this, (icon, key)| {
                        this.child(
                            badge(cx)
                                .flex()
                                .items_center()
                                .gap(tokens.spacing.xxs)
                                .child(Icon::new(icon).size(ext.icon.sm))
                                .child(self.t(key, cx)),
                        )
                    }),
            )
            .child(
                body_text(cx)
                    .font_family(tokens.typography.mono.clone())
                    .child(SharedString::from(call.name.clone())),
            );
        if let Some(summary) = summary {
            body = body.child(
                meta_text(cx)
                    .when(!args_open, |this| this.truncate())
                    .font_family(tokens.typography.mono.clone())
                    .child(SharedString::from(summary)),
            );
        }
        if let Some(pretty) = pretty.filter(|_| call.arguments.is_some()) {
            body = body.child(
                h_flex()
                    .id(SharedString::from(format!("assistant-args-{id_suffix}")))
                    .items_center()
                    .gap(tokens.spacing.xxs)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_arguments(&toggle_key, cx);
                    }))
                    .child(
                        Icon::new(if args_open {
                            FluxIcon::ChevronDown
                        } else {
                            FluxIcon::ChevronRight
                        })
                        .size(ext.icon.sm)
                        .text_color(ext.colors.accent_text),
                    )
                    .child(
                        meta_text(cx)
                            .text_color(ext.colors.accent_text)
                            .child(self.t(
                                if args_open {
                                    "assistantArgsHide"
                                } else {
                                    "assistantArgsShow"
                                },
                                cx,
                            )),
                    ),
            );
            if args_open {
                body = body.child(
                    div()
                        .w_full()
                        .p(tokens.spacing.sm)
                        .rounded(tokens.radius.sm)
                        .bg(colors.muted)
                        .child(
                            meta_text(cx)
                                .text_color(colors.foreground)
                                .font_family(tokens.typography.mono.clone())
                                .child(SharedString::from(pretty)),
                        ),
                );
            }
        }

        card(cx)
            .w_full()
            .border_color(ext.colors.warning.opacity(0.5))
            .p(tokens.spacing.md)
            .child(
                v_flex().w_full().gap(tokens.spacing.sm).child(body).child(
                    h_flex()
                        .items_center()
                        .gap(tokens.spacing.sm)
                        .child(div().flex_1().min_w_0().child(status))
                        .child(
                            button(
                                SharedString::from(format!("assistant-deny-{id_suffix}")),
                                self.t("assistantDeny", cx),
                                ButtonVariant::Secondary,
                                cx,
                            )
                            .disabled(!enabled)
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    if let Some(id) = &deny_id {
                                        this.confirm_tool(id.clone(), false, cx);
                                    }
                                },
                            )),
                        )
                        .child(
                            loading_button(
                                SharedString::from(format!("assistant-allow-{id_suffix}")),
                                self.t("assistantAllow", cx),
                                ButtonVariant::Primary,
                                busy,
                                cx,
                            )
                            .disabled(!enabled)
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    if let Some(id) = &allow_id {
                                        this.confirm_tool(id.clone(), true, cx);
                                    }
                                },
                            )),
                        ),
                ),
            )
            .into_any_element()
    }

    // ───────────────────────── 问题单 ─────────────────────────

    fn render_issue(&self, issue: &AssistantIssueDto, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let colors = tokens.colors;
        let ext = theme.extended().clone();
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
        cx: &mut Context<Self>,
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

    // ───────────────────────── 复制 ─────────────────────────

    /// 一轮回答下的复制按钮：复制提问、全部过程（工具调用参数与结果）与回答；复制后短暂显示勾。
    fn render_copy_turn(&self, turn_key: &str, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let ext = theme.extended();
        let side = theme.text_extent(RATE_BUTTON_SIDE);
        let copied = self.copied_turn.as_deref() == Some(turn_key);
        let label = self.t(
            if copied {
                "assistantCopied"
            } else {
                "assistantCopyTurn"
            },
            cx,
        );
        let tip = label.clone();
        let key = turn_key.to_owned();
        icon_button(
            SharedString::from(format!("assistant-copy-{turn_key}")),
            label,
            Icon::new(if copied {
                FluxIcon::Check
            } else {
                FluxIcon::Copy
            })
            .size(ext.icon.md),
            ButtonVariant::Ghost,
            cx,
        )
        .size(side)
        .text_color(if copied {
            ext.colors.success
        } else {
            theme.tokens().colors.muted_foreground
        })
        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.copy_turn(&key, cx);
        }))
        .into_any_element()
    }

    // ───────────────────────── 评分 ─────────────────────────

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
        let side = theme.text_extent(RATE_BUTTON_SIDE);
        let shown = self.shown_rating(message_id, server);
        let up_label = self.t("assistantRateUp", cx);
        let down_label = self.t("assistantRateDown", cx);
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
        let build = |value: &'static str,
                     icon: IconName,
                     label: SharedString,
                     tone: Hsla,
                     cx: &Context<Self>| {
            let target = message_id.to_owned();
            let tip = label.clone();
            let server = server.clone();
            icon_button(
                SharedString::from(format!("assistant-rate-{value}-{message_id}")),
                label.clone(),
                Icon::new(icon).size(ext.icon.md),
                ButtonVariant::Ghost,
                cx,
            )
            .size(side)
            .text_color(tone)
            .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
            .disabled(self.stale)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.rate(target.clone(), value, server.clone(), cx);
            }))
        };
        h_flex()
            .items_center()
            .gap(tokens.spacing.xxs)
            .child(build("up", IconName::ThumbsUp, up_label, up_tone, cx))
            .child(build(
                "down",
                IconName::ThumbsDown,
                down_label,
                down_tone,
                cx,
            ))
            .into_any_element()
    }

    // ───────────────────────── 输入区 ─────────────────────────

    fn render_composer(
        &self,
        quota_exhausted: bool,
        turns: &[Turn],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let width = theme.text_extent(COLUMN_WIDTH);
        let outer = v_flex()
            .flex_none()
            .w_full()
            .items_center()
            .px(tokens.spacing.lg)
            .pb(tokens.spacing.md)
            .pt(tokens.spacing.xs);

        if quota_exhausted {
            let limit = self
                .state
                .config
                .as_ref()
                .map_or(0, |config| config.daily_limit);
            return outer
                .child(
                    card(cx).w_full().max_w(width).p(tokens.spacing.md).child(
                        v_flex()
                            .gap(tokens.spacing.xs)
                            .child(
                                body_text(cx)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.t("assistantQuotaTitle", cx)),
                            )
                            .child(meta_text(cx).child(self.with(
                                "assistantQuotaDesc",
                                &[("limit", &limit.to_string())],
                                cx,
                            ))),
                    ),
                )
                .into_any_element();
        }

        let failure = self.failure_text(cx);
        let draft = self.draft(cx);
        let limit = self
            .state
            .config
            .as_ref()
            .map_or(0, |config| config.max_message_chars as usize);
        let count = draft.chars().count();
        let too_long = limit > 0 && count > limit;
        let running = self.current_run().is_some();
        let can_send = self.can_send(cx);
        let new_chat = self.current.is_none() && turns.is_empty();
        let kind = self.current_kind();
        let ext = theme.extended();
        let colors = tokens.colors;
        let can_attach = self.can_attach();

        // 提问 / 反馈切换收成小号（状态栏控件高 + caption 字号），与左右的圆形按钮同一视觉档。
        let chip_height = theme.density().status_control;
        let caption = ext.caption;
        let kind_chip = |id: &'static str, key: &'static str, value: AssistantSessionKind| {
            choice_chip(id, self.t(key, cx), kind == value, cx)
                .h(chip_height)
                .px(tokens.spacing.sm)
                .text_size(caption.size)
                .line_height(caption.line_height)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_kind(value, cx);
                }))
        };
        let left: AnyElement = if new_chat {
            h_flex()
                .items_center()
                .gap(tokens.spacing.xs)
                .child(kind_chip(
                    "assistant-kind-ask",
                    "assistantKindAsk",
                    AssistantSessionKind::Ask,
                ))
                .child(kind_chip(
                    "assistant-kind-feedback",
                    "assistantKindFeedback",
                    AssistantSessionKind::Feedback,
                ))
                .into_any_element()
        } else if too_long {
            field_error(
                self.with(
                    "assistantTooLong",
                    &[("count", &count.to_string()), ("max", &limit.to_string())],
                    cx,
                ),
                cx,
            )
            .into_any_element()
        } else {
            div().into_any_element()
        };

        let side = theme.text_extent(ROUND_BUTTON_SIDE);
        let action: AnyElement = if running {
            let content: AnyElement = if self.cancelling {
                spinner(ext.icon.sm).into_any_element()
            } else {
                // 停止 = 实心小方块。
                div()
                    .size(px(10.))
                    .rounded(px(2.))
                    .bg(colors.primary_foreground)
                    .into_any_element()
            };
            let button = circle_button(
                "assistant-stop",
                self.t("assistantStop", cx),
                content,
                side,
                CircleTone::Filled,
                !self.stale && !self.cancelling,
                cx,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop(cx)))
            .into_any_element();
            busy_halo(side, button, cx)
        } else {
            let content: AnyElement = if self.sending {
                spinner(ext.icon.sm).into_any_element()
            } else {
                Icon::new(FluxIcon::ArrowUp)
                    .size(ext.icon.md)
                    .into_any_element()
            };
            circle_button(
                "assistant-send",
                self.t("assistantSend", cx),
                content,
                side,
                CircleTone::Filled,
                can_send,
                cx,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.send(window, cx)))
            .into_any_element()
        };
        let add: Option<AnyElement> = self.image_limits().is_some().then(|| {
            circle_button(
                "assistant-add-image",
                self.t("assistantAttach", cx),
                Icon::new(FluxIcon::Plus).size(ext.icon.md),
                side,
                CircleTone::Ghost,
                can_attach,
                cx,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.pick_images(window, cx);
            }))
            .into_any_element()
        });

        // 粘贴图片 / 复制的图片文件由输入框的粘贴钩子接管；其余内容仍按文字粘贴。
        let view = cx.entity();
        let editor = Textarea::new(&self.input)
            .appearance(false)
            .bordered(false)
            .when(can_attach, |this| {
                this.on_paste(move |item, window, cx| {
                    view.update(cx, |this, cx| this.paste_images(item, window, cx))
                })
            });
        let accent = ext.colors.accent_text;
        let mut field = v_flex()
            .w_full()
            .min_w_0()
            .rounded(tokens.radius.xl)
            .border(ext.stroke.thin)
            .border_color(colors.border)
            .bg(colors.surface)
            .shadow(tokens.shadow.sm.clone())
            .when(can_attach, |this| {
                this.on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                    this.drop_files(paths.paths(), window, cx);
                }))
                .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(accent))
            });
        if !self.attachments.is_empty() || self.attaching > 0 {
            field = field.child(self.render_attachments(cx));
        }
        field = field
            .child(
                div()
                    .w_full()
                    .px(tokens.spacing.xs)
                    .pt(tokens.spacing.xs)
                    .child(editor),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(tokens.spacing.sm)
                    .px(tokens.spacing.sm)
                    .pb(tokens.spacing.sm)
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap(tokens.spacing.xs)
                            .children(add)
                            .child(left),
                    )
                    .child(action),
            );

        let mut inner = v_flex().w_full().max_w(width).gap(tokens.spacing.xs);
        if let Some(text) = failure {
            inner = inner.child(field_error(text, cx));
        }
        inner = inner.child(field);
        outer.child(inner).into_any_element()
    }

    /// 输入卡片内上方的缩略图条：每张带编号「#n」与移除按钮；还在处理的图以转圈占位。
    fn render_attachments(&self, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let colors = tokens.colors;
        let caption = ext.caption;
        let side = theme.text_extent(THUMBNAIL_SIDE);
        let remove_side = theme.text_extent(REMOVE_BUTTON_SIDE);
        let removable = !self.sending && !self.stale;
        let mut strip = h_flex()
            .w_full()
            .flex_wrap()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.md)
            .pt(tokens.spacing.md);
        for (index, attachment) in self.attachments.iter().enumerate() {
            let number = index + 1;
            let label = self.with("assistantImageRemove", &[("n", &number.to_string())], cx);
            let remove = circle_button(
                SharedString::from(format!("assistant-image-remove-{number}")),
                label,
                Icon::new(FluxIcon::X).size(px(10.)),
                remove_side,
                CircleTone::Overlay,
                removable,
                cx,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.remove_image(number, window, cx);
            }));
            strip = strip.child(
                div()
                    .relative()
                    .flex_none()
                    .child(thumbnail(&attachment.thumbnail, side, cx))
                    .child(
                        div()
                            .absolute()
                            .left(px(4.))
                            .bottom(px(4.))
                            .px(tokens.spacing.xs)
                            .rounded(tokens.radius.sm)
                            .bg(colors.background.opacity(0.8))
                            .text_size(caption.size)
                            .line_height(caption.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.foreground)
                            .child(SharedString::from(format!("#{number}"))),
                    )
                    .child(div().absolute().top(px(-6.)).right(px(-6.)).child(remove)),
            );
        }
        for _ in 0..self.attaching {
            strip = strip.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(side)
                    .rounded(tokens.radius.md)
                    .border(ext.stroke.thin)
                    .border_color(ext.colors.hairline)
                    .bg(colors.muted)
                    .child(spinner(ext.icon.md)),
            );
        }
        strip.into_any_element()
    }

    /// 发送 / 其他动作失败的提示文案。
    fn failure_text(&self, cx: &App) -> Option<SharedString> {
        if let Some(failure) = self.send_failure {
            let key = match failure {
                SendFailure::SignedOut => "assistantErrSignedOut",
                SendFailure::NotOpen => "assistantErrNotOpen",
                SendFailure::QuotaExceeded => "assistantErrQuota",
                SendFailure::Busy => "assistantErrBusy",
                SendFailure::Unavailable => "assistantErrUnavailable",
                SendFailure::Rejected => "assistantErrRejected",
                SendFailure::Failed => "assistantErrFailed",
            };
            return Some(self.t(key, cx));
        }
        self.action_failed
            .then(|| self.t("assistantActionFailed", cx))
    }

    // ───────────────────────── 历史会话 ─────────────────────────

    /// 历史视图：云端会话列表铺满面板；选中会话即回到对话视图。
    fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let ext = theme.extended().clone();
        let open = matches!(self.gate(), Gate::Open { .. });

        let mut list = v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .px(tokens.spacing.sm)
            .pb(tokens.spacing.sm)
            .gap(tokens.spacing.xxs);
        if open {
            match self.sessions.load {
                Load::Idle | Load::Loading if self.sessions.items.is_empty() => {
                    list = list.child(
                        h_flex()
                            .items_center()
                            .gap(tokens.spacing.sm)
                            .p(tokens.spacing.md)
                            .child(spinner(ext.icon.sm))
                            .child(meta_text(cx).child(self.t("assistantSessionsLoading", cx))),
                    );
                }
                Load::Failed if self.sessions.items.is_empty() => {
                    list = list.child(
                        v_flex()
                            .items_start()
                            .gap(tokens.spacing.sm)
                            .p(tokens.spacing.md)
                            .child(meta_text(cx).child(self.t("assistantSessionsLoadFailed", cx)))
                            .child(
                                button(
                                    "assistant-sessions-retry",
                                    self.t("assistantRetry", cx),
                                    ButtonVariant::Secondary,
                                    cx,
                                )
                                .disabled(self.stale)
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, _, cx| {
                                        this.reload_sessions(cx);
                                    },
                                )),
                            ),
                    );
                }
                Load::Idle | Load::Loading | Load::Ready | Load::Failed => {
                    if self.sessions.items.is_empty() {
                        list = list.child(
                            div()
                                .p(tokens.spacing.md)
                                .child(meta_text(cx).child(self.t("assistantSessionsEmpty", cx))),
                        );
                    }
                    for session in &self.sessions.items {
                        list = list.child(self.render_session_row(session, cx));
                    }
                    if self.sessions.has_more() {
                        list = list.child(
                            loading_button(
                                "assistant-sessions-more",
                                self.t("assistantLoadMore", cx),
                                ButtonVariant::Ghost,
                                self.sessions.more_loading,
                                cx,
                            )
                            .disabled(self.stale || self.sessions.more_loading)
                            .on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    this.load_more_sessions(cx);
                                },
                            )),
                        );
                    }
                }
            }
        }

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .flex_none()
                    .gap(tokens.spacing.sm)
                    .p(tokens.spacing.md)
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap(tokens.spacing.xs)
                            .child(
                                meta_text(cx)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.t("assistantSessionsTitle", cx)),
                            )
                            .when(open && !self.sessions.items.is_empty(), |this| {
                                this.child(self.render_clear_button(cx))
                            }),
                    ),
            )
            .child(list.overflow_y_scrollbar())
            .into_any_element()
    }

    /// 「清空」：点一次后同一位置变为危险态的「确认清空」，再点才执行；鼠标移出即取消。
    fn render_clear_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let confirming = self.clear_pending;
        let label = if self.clearing {
            self.t("assistantClearingSessions", cx)
        } else if confirming {
            self.t("assistantClearSessionsAgain", cx)
        } else {
            self.t("assistantClearSessions", cx)
        };
        let button = compact_button("assistant-sessions-clear", label, confirming, cx)
            .disabled(self.stale || self.clearing)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if this.clear_pending {
                    this.clear_sessions(cx);
                } else {
                    this.request_clear(cx);
                }
            }));
        div()
            .id("assistant-sessions-clear-zone")
            .flex_none()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.cancel_clear(cx);
                }
            }))
            .child(button)
            .into_any_element()
    }

    fn render_session_row(
        &self,
        session: &AssistantSessionDto,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let id = session.id.clone();
        let selected = self.current.as_deref() == Some(id.as_str());
        let confirming = self.delete_pending.as_deref() == Some(id.as_str());
        let title = {
            let title = session.title.trim();
            if title.is_empty() {
                self.t("assistantUntitled", cx)
            } else {
                SharedString::from(title.to_owned())
            }
        };
        let date = session
            .updated_at
            .get(..SESSION_DATE_CHARS)
            .unwrap_or(&session.updated_at)
            .to_owned();
        let running = self
            .state
            .runs
            .iter()
            .any(|run| run.session_id == id && run.status.is_active());
        let feedback = session.kind == "feedback";

        let select_id = id.clone();
        let request_id = id.clone();
        let delete_id = id.clone();
        let hover_id = id.clone();

        // 行 = 标题 / 日期 + 删除按钮。点删除后同一位置的按钮变为危险态，再点一次才删除（无需移动鼠标）；
        // 鼠标移出该行即取消。确认中日期行改为提示文字。
        let mut row = v_flex()
            .id(SharedString::from(format!("assistant-session-row-{id}")))
            .w_full()
            .rounded(tokens.radius.md)
            .when(selected, |this| this.bg(ext.colors.nav_selected))
            .when(!selected, |this| {
                this.hover(|style| style.bg(ext.colors.nav_hover))
            })
            .when(confirming, |this| {
                this.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if !*hovered {
                        this.cancel_delete(&hover_id, cx);
                    }
                }))
            });

        let label = v_flex()
            .flex_1()
            .min_w_0()
            .gap(tokens.spacing.xxs)
            .child(
                body_text(cx)
                    .truncate()
                    .when(selected, |this| {
                        this.text_color(ext.colors.nav_selected_foreground)
                    })
                    .child(title),
            )
            .child(if confirming {
                meta_text(cx)
                    .truncate()
                    .text_color(tokens.colors.destructive)
                    .child(self.t("assistantDeleteAgain", cx))
            } else {
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.xs)
                    .child(meta_text(cx).child(SharedString::from(date)))
                    .when(feedback, |this| {
                        this.child(meta_text(cx).child(self.t("assistantFeedbackBadge", cx)))
                    })
                    .when(running, |this| this.child(spinner(ext.icon.sm)))
            });
        // 删除按钮收成状态栏控件见方；确认态只换危险色图标 + 极淡危险底，不整块红色填充。
        let delete_size = active_theme(cx).density().status_control;
        let delete = if confirming {
            icon_button(
                SharedString::from(format!("assistant-session-delete-yes-{id}")),
                self.t("assistantDeleteAgain", cx),
                Icon::new(FluxIcon::Trash2).size(ext.icon.sm),
                ButtonVariant::Ghost,
                cx,
            )
            .size(delete_size)
            .text_color(tokens.colors.destructive)
            .bg(tokens.colors.destructive.opacity(0.12))
            .disabled(self.stale)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.delete_session(delete_id.clone(), cx);
            }))
        } else {
            icon_button(
                SharedString::from(format!("assistant-session-delete-{id}")),
                self.t("assistantDelete", cx),
                Icon::new(FluxIcon::Trash2).size(ext.icon.sm),
                ButtonVariant::Ghost,
                cx,
            )
            .size(delete_size)
            .disabled(self.stale)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.request_delete(request_id.clone(), cx);
            }))
        };
        row = row.child(
            h_flex()
                .w_full()
                .items_center()
                .gap(tokens.spacing.xxs)
                .child(
                    div()
                        .id(SharedString::from(format!("assistant-session-{id}")))
                        .flex_1()
                        .min_w_0()
                        .px(tokens.spacing.sm)
                        .py(tokens.spacing.xs)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.select_session(select_id.clone(), window, cx);
                        }))
                        .child(label),
                )
                .child(delete),
        );
        row.into_any_element()
    }
}

// ───────────────────────── 小部件 ─────────────────────────

/// 元信息文字：xs + 三级文字色。
fn meta_text(cx: &App) -> Div {
    let theme = active_theme(cx);
    let xs = theme.tokens().typography.xs;
    div()
        .text_size(xs.size)
        .line_height(xs.line_height)
        .text_color(theme.extended().colors.text_tertiary)
}

/// 正文：sm + 前景色。
fn body_text(cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let sm = tokens.typography.sm;
    div()
        .text_size(sm.size)
        .line_height(sm.line_height)
        .text_color(tokens.colors.foreground)
}

/// 中性小徽标。
fn badge(cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let caption = theme.extended().caption;
    div()
        .px(tokens.spacing.xs + tokens.spacing.xxs)
        .py(px(1.))
        .rounded(tokens.radius.sm)
        .bg(tokens.colors.muted)
        .text_size(caption.size)
        .line_height(caption.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(tokens.colors.muted_foreground)
}

/// 图标 + 文字的结果标签（已允许 / 已过期 / 已提交等）。
fn result_label(icon: FluxIcon, text: SharedString, tone: Hsla, cx: &App) -> AnyElement {
    let theme = active_theme(cx);
    h_flex()
        .items_center()
        .gap(theme.tokens().spacing.xs)
        .child(
            Icon::new(icon)
                .size(theme.extended().icon.sm)
                .text_color(tone),
        )
        .child(meta_text(cx).text_color(tone).child(text))
        .into_any_element()
}

fn spinner(size: Pixels) -> Spinner {
    Spinner::new().with_size(size)
}

/// 回复 / 执行中的停止按钮外圈：两道错相向外扩散淡出的光环 + 一圈旋转弧线。
fn busy_halo(side: Pixels, button: AnyElement, cx: &App) -> AnyElement {
    let primary = active_theme(cx).tokens().colors.primary;
    let ripple = move |id: &'static str, phase: f32| {
        div()
            .absolute()
            .rounded_full()
            .border_2()
            .border_color(primary)
            .with_animation(
                id,
                Animation::new(Duration::from_millis(1800)).repeat(),
                move |this, delta| {
                    let t = (delta + phase) % 1.;
                    let grow = side * (1. + 0.7 * t);
                    let offset = (side - grow) / 2.;
                    this.size(grow)
                        .top(offset)
                        .left(offset)
                        .opacity(0.55 * (1. - t) * (1. - t))
                },
            )
    };
    let ring = side + px(8.);
    div()
        .relative()
        .flex_none()
        .size(side)
        .child(ripple("assistant-busy-ripple-a", 0.))
        .child(ripple("assistant-busy-ripple-b", 0.5))
        .child(
            div()
                .absolute()
                .top(px(-4.))
                .left(px(-4.))
                .size(ring)
                .child(spinner(ring).color(primary)),
        )
        .child(div().absolute().inset_0().child(button))
        .into_any_element()
}

/// 助手回答的 Markdown 块：`key` 稳定，流式增量只更新文本。
fn markdown_block(key: &str, text: &str, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let sm = tokens.typography.sm;
    div()
        .w_full()
        .min_w_0()
        .text_size(sm.size)
        .line_height(relative(MARKDOWN_LINE_HEIGHT))
        .text_color(tokens.colors.foreground)
        .child(
            TextView::markdown(
                SharedString::from(format!("assistant-md-{key}")),
                text.to_owned(),
            )
            .selectable(true),
        )
}

/// 方形缩略图：圆角 + 细边框，图片按 cover 裁切。
fn thumbnail(image: &Arc<Image>, side: Pixels, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let ext = theme.extended();
    div()
        .relative()
        .flex_none()
        .size(side)
        .rounded(tokens.radius.md)
        .border(ext.stroke.thin)
        .border_color(ext.colors.hairline)
        .child(
            img(Arc::clone(image))
                .size_full()
                .rounded(tokens.radius.md)
                .object_fit(ObjectFit::Cover),
        )
}

/// 用户气泡下方、本地已知的图片缩略图（靠右，与气泡对齐）。
fn sent_thumbnails(thumbnails: &[Arc<Image>], cx: &App) -> AnyElement {
    let theme = active_theme(cx);
    let side = theme.text_extent(BUBBLE_THUMBNAIL_SIDE);
    h_flex()
        .max_w(theme.text_extent(BUBBLE_WIDTH))
        .flex_wrap()
        .justify_end()
        .gap(theme.tokens().spacing.xs)
        .children(thumbnails.iter().map(|image| thumbnail(image, side, cx)))
        .into_any_element()
}

/// 圆形按钮的配色。
#[derive(Clone, Copy)]
enum CircleTone {
    /// 主色填充（发送 / 停止）；不可用时灰底。
    Filled,
    /// 无底色的图标按钮（添加图片）。
    Ghost,
    /// 缩略图角上的移除按钮：前景色填充、反色图标。
    Overlay,
}

/// 圆形图标按钮：与 `icon_button` 同一套结构（无障碍 / 焦点环 / 状态层），悬停 / 按下由状态层在底色
/// 上叠一层极淡的对比色；不可用时无指针手型与悬停反馈。
fn circle_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    content: impl IntoElement,
    size: Pixels,
    tone: CircleTone,
    enabled: bool,
    cx: &App,
) -> ThemedButton {
    let label = label.into();
    let tip = label.clone();
    let theme = active_theme(cx);
    let colors = theme.tokens().colors;
    let radius = theme.tokens().radius.full;
    let stroke = theme.extended().stroke.thin;
    let (fill, foreground, overlay) = match (tone, enabled) {
        (CircleTone::Filled, true) => (
            colors.primary,
            colors.primary_foreground,
            colors.primary_foreground,
        ),
        (CircleTone::Filled, false) => (colors.muted, colors.muted_foreground, colors.foreground),
        (CircleTone::Ghost, true) => (
            transparent_black(),
            colors.muted_foreground,
            colors.foreground,
        ),
        (CircleTone::Ghost, false) => (
            transparent_black(),
            colors.muted_foreground.opacity(0.5),
            colors.foreground,
        ),
        (CircleTone::Overlay, _) => (colors.foreground, colors.background, colors.background),
    };
    let button = ThemedButton::new(id)
        .size(size)
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius)
        .border(stroke)
        .border_color(transparent_black())
        .bg(fill)
        .text_color(foreground)
        .focus_visible(move |style| style.border_color(colors.ring))
        .accessibility_label(label)
        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        .disabled(!enabled);
    let button = if enabled {
        button.cursor_pointer().child(
            StateLayer::new(radius)
                .hover(overlay.opacity(0.12))
                .press(overlay.opacity(0.22)),
        )
    } else {
        button
    };
    button.child(content)
}

/// 行内小号文字按钮（确认 / 取消这类次要动作）：在幽灵按钮上收成 `density.statusControl` 高、
/// caption 字号；`danger` = 危险色文字 + 极淡危险底色，不做整块红色填充。
fn compact_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    danger: bool,
    cx: &App,
) -> ThemedButton {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let caption = theme.extended().caption;
    let colors = tokens.colors;
    button(id, label, ButtonVariant::Ghost, cx)
        .h(theme.density().status_control)
        .px(tokens.spacing.sm)
        .text_size(caption.size)
        .line_height(caption.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(if danger {
            colors.destructive
        } else {
            colors.muted_foreground
        })
        .when(danger, |this| this.bg(colors.destructive.opacity(0.12)))
}
