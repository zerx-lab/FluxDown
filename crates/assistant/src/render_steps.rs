//! 助手页的「过程」折叠区、工具调用行与确认卡片。

use fluxdown_protocol::{AssistantToolCallDto, AssistantToolEffect};
use fluxdown_ui_components::{ButtonVariant, FluxIcon, button, card, loading_button, tooltip_view};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{Icon, h_flex, v_flex};

use crate::{
    model::{
        Block, ConfirmView, ToolState, Turn, arguments_pretty, arguments_summary, confirm_view,
        format_countdown, format_duration, remaining_seconds, steps_open, tool_output_display,
        tool_state, tool_status_key,
    },
    preview::{preview_body, preview_has_content},
    view::AssistantView,
    widgets::{badge, body_text, compact_button, meta_text, result_label, spinner},
};

impl AssistantView {
    pub(crate) fn render_steps(&self, turn: &Turn, now_ms: i64, cx: &Context<Self>) -> AnyElement {
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
                    Block::Text(text) | Block::Step(text) => {
                        list = list.child(
                            body_text(cx)
                                .text_color(colors.muted_foreground)
                                .child(SharedString::from(text.clone())),
                        );
                    }
                    Block::Tool(call) => {
                        // 旁观执行的确认只能在发起它的设备上处理：不显示确认卡片与按钮。
                        let element = if turn.is_observing() && call.confirmation.is_some() {
                            let mut observed = call.clone();
                            observed.confirmation = None;
                            self.render_tool(&observed, running, now_ms, cx)
                        } else {
                            self.render_tool(call, running, now_ms, cx)
                        };
                        list = list.child(element);
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
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = tokens.colors;
        let ext = theme.extended();
        let icon_size = ext.icon.sm;
        let state = tool_state(call, run_active);
        let confirmation = confirm_view(call, run_active, now_ms);
        let client = call.kind == "client";
        let translator = self.translator.read(cx);
        let t = |key: &str, args: &[(&str, &str)]| translator.text_with(key, args);

        // 状态文案与图标：待确认 / 未执行优先于调用状态本身。
        let label_key = tool_status_key(confirmation, state);
        let tone = match label_key {
            "assistantToolWaiting" | "assistantToolInterrupted" | "assistantToolResultUnknown" => {
                ext.colors.warning
            }
            "assistantToolSkipped" | "assistantToolRunning" => ext.colors.text_tertiary,
            "assistantToolDone" => ext.colors.success,
            _ => colors.destructive,
        };
        let icon: AnyElement = match (confirmation, state) {
            (Some(ConfirmView::Pending { .. }), _) => Icon::new(FluxIcon::Clock)
                .size(icon_size)
                .text_color(tone)
                .into_any_element(),
            (None, ToolState::Running) | (Some(ConfirmView::Approved), ToolState::Running) => {
                spinner(icon_size).into_any_element()
            }
            (Some(ConfirmView::Declined | ConfirmView::Expired), _) => {
                Icon::new(FluxIcon::CircleAlert)
                    .size(icon_size)
                    .text_color(tone)
                    .into_any_element()
            }
            (_, ToolState::Succeeded) => Icon::new(FluxIcon::CircleCheck)
                .size(icon_size)
                .text_color(tone)
                .into_any_element(),
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
        // 没有调用 ID 的历史工具以调用名 + 状态兜底，展开状态才不会互相串。
        let identity = if call.call_id.is_empty() {
            format!("{}-{}", call.name, label_key)
        } else {
            call.call_id.clone()
        };
        let detail_key = format!("detail:{identity}");
        let detail_open = expandable && self.args_open.contains(&detail_key);

        let header = h_flex()
            .id(SharedString::from(format!("assistant-tool-{identity}")))
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
                this.child(meta_text(cx).child(format_duration(duration, &t)))
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
            let section = |title: SharedString, text: String| {
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
                detail = detail.child(section(self.t("assistantToolArguments", cx), arguments));
            }
            if let Some(output) = output {
                detail = detail.child(section(self.t("assistantToolOutput", cx), output));
            }
            row = row.child(detail);
        }
        if let Some(view) = confirmation {
            row = row.child(self.render_confirm(call, view, identity, cx));
        }
        row.into_any_element()
    }

    /// 工具确认卡片：待确认时可允许 / 拒绝；过期或已处理后按钮禁用并显示结果。
    /// 有可读预览时显示预览，没有就回退成原始参数摘要；破坏性操作用危险色与不可撤销提示。
    fn render_confirm(
        &self,
        call: &AssistantToolCallDto,
        view: ConfirmView,
        identity: String,
        cx: &Context<Self>,
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
        let danger = call.destructive;
        let translator = self.translator.read(cx);
        let t = |key: &str, args: &[(&str, &str)]| translator.text_with(key, args);
        let preview = call
            .preview
            .as_ref()
            .filter(|preview| preview_has_content(preview));

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
                        &[("time", &format_countdown(remaining_seconds(ms), &t))],
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

        let args_open = self.args_open.contains(&identity);
        let summary = arguments_summary(call.arguments.as_ref());
        let pretty = call.arguments.as_ref().map(arguments_pretty);
        let toggle_key = identity.clone();

        let allow_id = request_id.clone();
        let deny_id = request_id;
        let accent = if danger {
            colors.destructive
        } else {
            ext.colors.warning
        };

        let mut body = v_flex().w_full().gap(tokens.spacing.xs).child(
            h_flex()
                .items_center()
                .gap(tokens.spacing.xs)
                .flex_wrap()
                .child(
                    Icon::new(FluxIcon::CircleAlert)
                        .size(ext.icon.sm)
                        .text_color(accent),
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
        );
        if let Some(preview) = preview {
            body = body.child(preview_body(preview, &t, cx));
        } else {
            body = body.child(
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
        }
        if danger {
            body = body.child(
                meta_text(cx)
                    .text_color(colors.destructive)
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.t("assistantConfirmDestructive", cx)),
            );
        }
        if let Some(pretty) = pretty.filter(|_| call.arguments.is_some()) {
            body = body.child(
                h_flex()
                    .id(SharedString::from(format!("assistant-args-{identity}")))
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

        let deny = button(
            SharedString::from(format!("assistant-deny-{identity}")),
            self.t("assistantDeny", cx),
            ButtonVariant::Secondary,
            cx,
        )
        .disabled(!enabled)
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if let Some(id) = &deny_id {
                this.confirm_tool(id.clone(), false, cx);
            }
        }));
        let allow_label = self.t("assistantAllow", cx);
        let allow_id_attr = SharedString::from(format!("assistant-allow-{identity}"));
        let allow: AnyElement = if danger {
            // 危险操作的允许按钮用危险色文字 + 极淡危险底，不做整块红色填充。
            compact_button(allow_id_attr, allow_label, true, cx)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(id) = &allow_id {
                        this.confirm_tool(id.clone(), true, cx);
                    }
                }))
                .into_any_element()
        } else {
            loading_button(allow_id_attr, allow_label, ButtonVariant::Primary, busy, cx)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(id) = &allow_id {
                        this.confirm_tool(id.clone(), true, cx);
                    }
                }))
                .into_any_element()
        };

        card(cx)
            .w_full()
            .border_color(accent.opacity(0.5))
            .p(tokens.spacing.md)
            .child(
                v_flex().w_full().gap(tokens.spacing.sm).child(body).child(
                    h_flex()
                        .items_center()
                        .gap(tokens.spacing.sm)
                        .child(div().flex_1().min_w_0().child(status))
                        .child(deny)
                        .child(allow),
                ),
            )
            .into_any_element()
    }
}
