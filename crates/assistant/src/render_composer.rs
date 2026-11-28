//! 助手页输入区：提问 / 反馈切换、图片附件、发送与停止、编辑重发提示与各类失败提示。

use fluxdown_protocol::AssistantSessionKind;
use fluxdown_ui_components::{ButtonVariant, FluxIcon, button, card, choice_chip, field_error};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, ClickEvent, Context, ExternalPaths, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, SharedString, Styled, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{Icon, h_flex, input::Textarea, v_flex};

use crate::{
    model::SessionQuota,
    render::COLUMN_WIDTH,
    view::AssistantView,
    widgets::{
        CircleTone, body_text, circle_button, compact_button, dot_matrix, meta_text, spinner,
    },
};

/// 输入区缩略图边长（随文字缩放）。
const THUMBNAIL_SIDE: f32 = 56.;
/// 缩略图角上移除按钮的直径。
const REMOVE_BUTTON_SIDE: f32 = 18.;
/// 发送 / 停止 / 添加图片按钮的直径。
const ROUND_BUTTON_SIDE: f32 = 32.;
/// 停止按钮内容的悬停组：悬停时点阵换成停止方块。
const STOP_GROUP: &str = "assistant-stop-content";

impl AssistantView {
    pub(crate) fn render_composer(&self, quota_exhausted: bool, cx: &Context<Self>) -> AnyElement {
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

        // 当天最后一次提问进行中：云端已把它计入额度，但停止按钮与输入区仍要保留（同会话上限卡）。
        if quota_exhausted && self.current_run().is_none() && self.visible_outgoing().is_none() {
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

        if self.session_limit_reached() {
            let limit = self
                .state
                .config
                .as_ref()
                .map_or(0, |config| config.session_limit);
            return outer
                .child(
                    card(cx).w_full().max_w(width).p(tokens.spacing.md).child(
                        v_flex()
                            .items_start()
                            .gap(tokens.spacing.sm)
                            .child(
                                v_flex()
                                    .gap(tokens.spacing.xs)
                                    .child(
                                        body_text(cx)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(self.t("assistantSessionLimitTitle", cx)),
                                    )
                                    .child(meta_text(cx).child(self.with(
                                        "assistantSessionLimitDesc",
                                        &[("limit", &limit.to_string())],
                                        cx,
                                    ))),
                            )
                            .child(
                                button(
                                    "assistant-session-limit-new",
                                    self.t("assistantNewChat", cx),
                                    ButtonVariant::Primary,
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.new_chat(window, cx);
                                    },
                                )),
                            ),
                    ),
                )
                .into_any_element();
        }

        let failure = self.failure_text(cx);
        let limit = self
            .state
            .config
            .as_ref()
            .map_or(0, |config| config.max_message_chars as usize);
        let count = if limit > 0 {
            self.input.read(cx).value().trim().chars().count()
        } else {
            0
        };
        let too_long = limit > 0 && count > limit;
        let working = self.current_run().is_some() || self.visible_outgoing().is_some();
        let can_send = self.can_send(cx);
        let new_chat =
            self.current.is_none() && self.timeline.is_empty() && self.outgoing.is_none();
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
        } else {
            div().into_any_element()
        };

        let side = theme.text_extent(ROUND_BUTTON_SIDE);
        let action: AnyElement = if working {
            // 等待回复：按钮内是旋转点阵；悬停时换成停止方块，提示可以点击停止。
            let can_stop = self.can_stop();
            let dots = if can_stop {
                colors.primary_foreground
            } else {
                colors.muted_foreground
            };
            let content = div()
                .group(STOP_GROUP)
                .relative()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .when(can_stop, |this| {
                            this.group_hover(STOP_GROUP, |style| style.invisible())
                        })
                        .child(dot_matrix(side * 0.56, dots)),
                )
                .when(can_stop, |this| {
                    this.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .invisible()
                            .group_hover(STOP_GROUP, |style| style.visible())
                            .child(
                                // 停止 = 实心小方块。
                                div()
                                    .size(px(10.))
                                    .rounded(px(2.))
                                    .bg(colors.primary_foreground),
                            ),
                    )
                });
            circle_button(
                "assistant-stop",
                self.t("assistantStop", cx),
                content,
                side,
                crate::widgets::CircleTone::Filled,
                can_stop,
                cx,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop(cx)))
            .into_any_element()
        } else {
            let content: AnyElement = if self.sending() {
                dot_matrix(side * 0.56, colors.muted_foreground)
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
            .id("assistant-composer-field")
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
        if self.editing.is_some() {
            inner = inner.child(self.render_editing_banner(cx));
        }
        if too_long {
            inner = inner.child(field_error(
                self.with(
                    "assistantTooLong",
                    &[("count", &count.to_string()), ("max", &limit.to_string())],
                    cx,
                ),
                cx,
            ));
        }
        if let Some(text) = failure {
            inner = inner.child(field_error(text, cx));
        }
        inner = inner.child(field);
        if let SessionQuota::Low(remaining) = self.current_session_quota() {
            inner = inner.child(meta_text(cx).px(tokens.spacing.sm).child(self.with(
                "assistantSessionRemaining",
                &[("count", &remaining.to_string())],
                cx,
            )));
        }
        outer.child(inner).into_any_element()
    }

    /// 编辑重发提示：说明发送会替换这条提问及其后的对话，并可放弃编辑。
    fn render_editing_banner(&self, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        h_flex()
            .w_full()
            .items_center()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.sm)
            .py(tokens.spacing.xs)
            .rounded(tokens.radius.md)
            .bg(ext.colors.warning.opacity(0.1))
            .child(
                Icon::new(FluxIcon::Pen)
                    .size(ext.icon.sm)
                    .text_color(ext.colors.warning),
            )
            .child(
                meta_text(cx)
                    .flex_1()
                    .min_w_0()
                    .text_color(tokens.colors.foreground)
                    .child(self.t("assistantEditing", cx)),
            )
            .child(
                compact_button(
                    "assistant-edit-cancel",
                    self.t("assistantEditCancel", cx),
                    false,
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.cancel_edit(window, cx);
                })),
            )
            .into_any_element()
    }

    /// 输入卡片内上方的缩略图条：每张带编号「#n」与移除按钮，点缩略图看大图；还在处理的图以转圈占位。
    fn render_attachments(&self, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let colors = tokens.colors;
        let caption = ext.caption;
        let side = theme.text_extent(THUMBNAIL_SIDE);
        let remove_side = theme.text_extent(REMOVE_BUTTON_SIDE);
        let removable = self.outgoing.is_none() && !self.stale;
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
                // 移除按钮压在缩略图角上：别让同一次点击再打开大图。
                cx.stop_propagation();
                this.remove_image(number, window, cx);
            }));
            strip = strip.child(
                div()
                    .relative()
                    .flex_none()
                    .child(self.previewable_thumbnail(
                        SharedString::from(format!("assistant-draft-thumb-{number}")),
                        number,
                        &attachment.image,
                        side,
                        cx,
                    ))
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
            return Some(self.t(failure.key(), cx));
        }
        self.action_error.map(|error| self.t(error.key(), cx))
    }
}
