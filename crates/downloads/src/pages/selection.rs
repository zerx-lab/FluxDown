//! 引擎发起的交互选择（HLS 画质 / BT 文件 / 插件变体）弹出窗口内容。
//!
//! 每个 [`fluxdown_protocol::SelectionRequestDto`] 对应一个独立 `Floating` 窗口
//! （见 `crate::windows::selection`，app 侧）；窗口的开启/关闭由 app 监听会话事件
//! 完成，本视图只负责收集用户选择并调用 `daemon.selection.resolve`。
//! `FileExists` 不走本视图，由 [`crate::FileConflictView`] 聚合承载。
//! BT 文件选择带「保存目录」：预填任务当前目录，用户改了才在答复里带回，
//! 引擎在真正下载前搬目录并重开任务。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxdown_protocol::{
    BtFileDto, HlsQualityOptionDto, PluginCandidateDto, ResolveVariantOptionDto, SelectionKind,
    SelectionOutcome, SelectionRequestDto, SelectionResolutionDto,
};
use fluxdown_ui_components::{
    BusyExt as _, CheckState, ControlExt as _, FluxIcon, check_mark, check_row, field_hint,
    form_field, input_with_action, tabular_numbers,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, InteractiveElement as _,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    scroll::ScrollableElement as _,
    v_flex,
};

use crate::{
    controller::{DownloadsCommand, DownloadsPort},
    model::format_bytes,
};

mod plugin_choice;

pub struct SelectionView {
    strings: SelectionStrings,
    request: SelectionRequestDto,
    task_name: String,
    port: Arc<dyn DownloadsPort>,
    state: SelectionState,
    /// BT 保存目录输入；其它选择种类没有。
    save_dir_input: Option<Entity<InputState>>,
    /// 系统目录选择器打开中，避免重入。
    picking_save_dir: bool,
    submitting: bool,
}

enum SelectionState {
    Hls {
        options: Vec<HlsQualityOptionDto>,
        selected: i32,
    },
    Bt {
        files: Vec<BtFileDto>,
        selected: HashSet<i32>,
        /// 请求预填的当前保存目录。空串 = 旧 daemon 未带目录。
        save_dir: String,
    },
    Variant {
        options: Vec<ResolveVariantOptionDto>,
        selected: i32,
    },
    Plugin {
        site: String,
        url: String,
        candidates: Vec<PluginCandidateDto>,
        /// 引擎给出的默认候选（徽标所在项）；与初始预选一致，之后不随用户选择变化。
        default_identity: String,
        selected: String,
        remember: bool,
    },
}

/// 单请求选择窗口（HLS 画质 / BT 文件 / 插件变体）能承载的请求及其默认选择。
/// `FileExists` 由聚合的「文件已存在」窗口承载（见 [`crate::FileConflictView`]），没有计划。
pub struct SelectionPlan {
    state: SelectionState,
}

impl SelectionPlan {
    #[must_use]
    pub fn for_request(request: &SelectionRequestDto) -> Option<Self> {
        let state = match request.kind.clone() {
            SelectionKind::Hls { options } => {
                let selected = match request.default_choice {
                    SelectionOutcome::Hls { index } => index,
                    _ => options.first().map_or(0, |option| option.index),
                };
                SelectionState::Hls { options, selected }
            }
            SelectionKind::Bt { files, save_dir } => {
                let selected = match &request.default_choice {
                    SelectionOutcome::Bt { indices, .. } if !indices.is_empty() => {
                        indices.iter().copied().collect()
                    }
                    _ => files.iter().map(|file| file.index).collect(),
                };
                SelectionState::Bt {
                    files,
                    selected,
                    save_dir,
                }
            }
            SelectionKind::Variant { options } => {
                let selected = match request.default_choice {
                    SelectionOutcome::Variant { index } => index,
                    _ => options.first().map_or(0, |option| option.index),
                };
                SelectionState::Variant { options, selected }
            }
            SelectionKind::Plugin {
                site,
                url,
                candidates,
            } => {
                let selected =
                    plugin_choice::initial_selection(&candidates, &request.default_choice);
                SelectionState::Plugin {
                    site,
                    url,
                    default_identity: selected.clone(),
                    selected,
                    remember: false,
                    candidates,
                }
            }
            SelectionKind::FileExists { .. } => return None,
        };
        Some(Self { state })
    }
}

impl SelectionView {
    /// 按计划初始化默认选择，并启动每秒刷新一次的倒计时。
    /// BT 选择额外建保存目录输入框（预填请求里的当前目录；旧 daemon 为空，只显示占位）。
    pub fn new(
        translator: Entity<Translator>,
        request: SelectionRequestDto,
        plan: SelectionPlan,
        task_name: String,
        port: Arc<dyn DownloadsPort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let strings = SelectionStrings::from_translator(translator.read(cx));
        let state = plan.state;
        let save_dir_input = match &state {
            SelectionState::Bt { save_dir, .. } => {
                let initial = save_dir.clone();
                let placeholder = strings.save_dir_placeholder.clone();
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(initial)
                        .placeholder(placeholder)
                });
                // 确认按钮是否可点取决于目录是否为空，输入变化必须重绘。
                cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
                .detach();
                Some(input)
            }
            _ => None,
        };
        cx.observe_in(&translator, window, |this, translator, window, cx| {
            this.strings = SelectionStrings::from_translator(translator.read(cx));
            if let Some(input) = &this.save_dir_input {
                let placeholder = this.strings.save_dir_placeholder.clone();
                input.update(cx, |input, cx| {
                    input.set_placeholder(placeholder, window, cx)
                });
            }
            cx.notify();
        })
        .detach();
        // 倒计时走字；无截止（用户关闭了自动选择默认项）时没有可更新的内容，不每秒重绘文件列表。
        if request.has_deadline() {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        Self {
            strings,
            request,
            task_name,
            port,
            state,
            save_dir_input,
            picking_save_dir: false,
            submitting: false,
        }
    }

    /// HLS 无取消入口，与 daemon 超时后按默认值强制解析的语义一致。
    fn cancellable(&self) -> bool {
        !matches!(self.state, SelectionState::Hls { .. })
    }

    /// 距截止的整秒数；无截止（用户关闭了自动选择默认项）为 `None`，页脚不显示倒计时。
    fn remaining_seconds(&self) -> Option<i64> {
        self.request
            .has_deadline()
            .then(|| ((self.request.deadline_unix_ms - now_unix_ms()) / 1000).max(0))
    }

    /// 引擎把空 BT 选择回退为全部文件，所以全不选时不允许确认（与 web 一致）。
    /// 保存目录是否可空见 [`bt_save_dir_confirmable`]。
    fn can_confirm(&self, cx: &App) -> bool {
        match &self.state {
            SelectionState::Bt {
                selected, save_dir, ..
            } => {
                !selected.is_empty()
                    && bt_save_dir_confirmable(save_dir, &self.save_dir_input_value(cx))
            }
            SelectionState::Plugin { selected, .. } => !selected.is_empty(),
            _ => true,
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_confirm(cx) {
            return;
        }
        let outcome = match &self.state {
            SelectionState::Hls { selected, .. } => SelectionOutcome::Hls { index: *selected },
            SelectionState::Bt {
                selected, save_dir, ..
            } => SelectionOutcome::Bt {
                indices: selected.iter().copied().collect(),
                save_dir: bt_save_dir_outcome(save_dir, &self.save_dir_input_value(cx)),
            },
            SelectionState::Variant { selected, .. } => {
                SelectionOutcome::Variant { index: *selected }
            }
            SelectionState::Plugin {
                selected, remember, ..
            } => match plugin_choice::confirm_outcome(selected, *remember) {
                Some(outcome) => outcome,
                None => return,
            },
        };
        self.resolve(outcome, window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.resolve(SelectionOutcome::Cancelled, window, cx);
    }

    fn resolve(&mut self, outcome: SelectionOutcome, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        self.submitting = true;
        cx.notify();
        let future =
            self.port
                .execute(DownloadsCommand::ResolveSelection(SelectionResolutionDto {
                    request_id: self.request.request_id.clone(),
                    outcome,
                }));
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.submitting = false;
                if result.is_err() {
                    window.push_notification(
                        Notification::error(this.strings.action_failed.clone()),
                        cx,
                    );
                }
                cx.notify();
            }) else {
                // 视图或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn save_dir_input_value(&self, cx: &App) -> SharedString {
        self.save_dir_input
            .as_ref()
            .map(|input| input.read(cx).value())
            .unwrap_or_default()
    }

    /// 系统目录选择器只能选本机目录。打开期间禁止重入；选中后写入输入框。
    fn pick_save_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking_save_dir || self.submitting || self.save_dir_input.is_none() {
            return;
        }
        self.picking_save_dir = true;
        cx.notify();
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.strings.save_dir_placeholder.clone()),
        });
        cx.spawn_in(window, async move |this, cx| {
            // 取消不是失败；选择器本身出错或通道断开才提示，不把错误吞掉。
            let (picked, failed) = match receiver.await {
                Ok(Ok(Some(paths))) => {
                    (paths.first().map(|path| path.display().to_string()), false)
                }
                Ok(Ok(None)) => (None, false),
                Ok(Err(_)) => (None, true),
                Err(_) => (None, true),
            };
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.picking_save_dir = false;
                if let Some(path) = picked
                    && let Some(input) = &this.save_dir_input
                {
                    input.update(cx, |input, cx| input.set_value(path, window, cx));
                }
                if failed {
                    window.push_notification(
                        Notification::error(this.strings.file_picker_error.clone()),
                        cx,
                    );
                }
                // set_value 不发 Change，确认按钮是否可点要靠这次重绘。
                cx.notify();
            }) else {
                // 视图或窗口已释放，不再回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 保存目录在全选工具栏之上：标签 + 输入框 + 浏览。长路径留在输入框里滚动，不撑开窗口。
    fn render_save_dir(&self, cx: &mut Context<Self>) -> Option<Div> {
        let input = self.save_dir_input.as_ref()?;
        Some(
            form_field(
                self.strings.save_dir.clone(),
                input_with_action(
                    Input::new(input).control(cx).w_full(),
                    Button::new("bt-save-dir-browse")
                        .outline()
                        .icon(FluxIcon::FolderOpen)
                        .label(self.strings.browse.clone())
                        .control(cx)
                        .disabled(self.picking_save_dir || self.submitting)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.pick_save_dir(window, cx);
                        })),
                    cx,
                ),
                None,
                cx,
            )
            .flex_none(),
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let title_style = theme.extended().title;
        let (title, description) = match &self.state {
            SelectionState::Hls { .. } => (
                self.strings.hls_title.clone(),
                self.strings.hls_desc.clone(),
            ),
            SelectionState::Bt { files, .. } => (
                self.strings.bt_title.clone(),
                if files.len() == 1 {
                    self.strings.bt_desc_single.clone()
                } else {
                    SharedString::from(
                        self.strings
                            .bt_desc
                            .replace("{count}", &files.len().to_string()),
                    )
                },
            ),
            SelectionState::Variant { .. } => (
                self.strings.variant_title.clone(),
                self.strings.variant_desc.clone(),
            ),
            SelectionState::Plugin { .. } => {
                (self.strings.plugin_title.clone(), SharedString::default())
            }
        };
        v_flex()
            .gap(tokens.spacing.xs)
            .p(tokens.spacing.md)
            .child(
                div()
                    .text_size(title_style.size)
                    .line_height(title_style.line_height)
                    .font_weight(title_style.weight)
                    .child(title),
            )
            .child(
                div()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .text_color(tokens.colors.muted_foreground)
                    .child(if matches!(self.state, SelectionState::Plugin { .. }) {
                        // 站点名加粗；任务名已在窗口标题里，不重复。
                        self.plugin_description(cx)
                    } else if self.task_name.is_empty() {
                        description.into_any_element()
                    } else {
                        SharedString::from(format!("{description} · {}", self.task_name))
                            .into_any_element()
                    }),
            )
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        // HLS / 画质选择窗口固定尺寸：文字放大后选项可能超出窗口，列表内滚动。
        match &self.state {
            SelectionState::Hls { options, selected } => div()
                .id("selection-hls-scroll")
                .size_full()
                .overflow_y_scroll()
                .child(self.render_hls(options, *selected, cx))
                .into_any_element(),
            SelectionState::Bt {
                files, selected, ..
            } => self.render_bt(files, selected, cx).into_any_element(),
            SelectionState::Variant { options, selected } => div()
                .id("selection-variant-scroll")
                .size_full()
                .overflow_y_scroll()
                .child(self.render_variant(options, *selected, cx))
                .into_any_element(),
            SelectionState::Plugin { .. } => self.render_plugin(cx),
        }
    }

    fn render_hls(
        &self,
        options: &[HlsQualityOptionDto],
        selected: i32,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let hover = theme.extended().colors.row_hover;
        v_flex()
            .gap(tokens.spacing.xxs)
            .px(tokens.spacing.md)
            .children(options.iter().map(|option| {
                let checked = option.index == selected;
                let index = option.index;
                let label = SharedString::from(format!(
                    "{}×{} · {} kbps",
                    option.width,
                    option.height,
                    option.bandwidth / 1000
                ));
                h_flex()
                    .id(("hls-option", index as usize))
                    .cursor_pointer()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .px(tokens.spacing.sm)
                    .py(tokens.spacing.xs)
                    .rounded(tokens.radius.md)
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .font_features(tabular_numbers())
                    .map(|row| {
                        if checked {
                            row.bg(tokens.colors.accent)
                        } else {
                            row.hover(move |style| style.bg(hover))
                        }
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let SelectionState::Hls { selected, .. } = &mut this.state {
                            *selected = index;
                        }
                        cx.notify();
                    }))
                    .child(check_mark(check_state(checked), cx))
                    .child(div().flex_1().child(label))
            }))
    }

    fn render_variant(
        &self,
        options: &[ResolveVariantOptionDto],
        selected: i32,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let hover = theme.extended().colors.row_hover;
        v_flex()
            .gap(tokens.spacing.xxs)
            .px(tokens.spacing.md)
            .children(options.iter().map(|option| {
                let checked = option.index == selected;
                let index = option.index;
                let label = SharedString::from(option.label.clone());
                let meta = SharedString::from(format!(
                    "{} · {}×{}",
                    option.container, option.width, option.height
                ));
                h_flex()
                    .id(("variant-option", index as usize))
                    .cursor_pointer()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .px(tokens.spacing.sm)
                    .py(tokens.spacing.xs)
                    .rounded(tokens.radius.md)
                    .map(|row| {
                        if checked {
                            row.bg(tokens.colors.accent)
                        } else {
                            row.hover(move |style| style.bg(hover))
                        }
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let SelectionState::Variant { selected, .. } = &mut this.state {
                            *selected = index;
                        }
                        cx.notify();
                    }))
                    .child(check_mark(check_state(checked), cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(tokens.spacing.xxs)
                            .child(
                                div()
                                    .text_size(tokens.typography.sm.size)
                                    .line_height(tokens.typography.sm.line_height)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_size(tokens.typography.xs.size)
                                    .line_height(tokens.typography.xs.line_height)
                                    .font_features(tabular_numbers())
                                    .text_color(tokens.colors.muted_foreground)
                                    .child(meta),
                            ),
                    )
            }))
    }

    fn render_bt(
        &self,
        files: &[BtFileDto],
        selected: &HashSet<i32>,
        cx: &mut Context<Self>,
    ) -> Div {
        let tokens = active_theme(cx).tokens().clone();
        let selected_size: i64 = files
            .iter()
            .filter(|file| selected.contains(&file.index))
            .map(|file| file.size)
            .sum();
        let toolbar = h_flex()
            .items_center()
            .gap(tokens.spacing.sm)
            .child(
                Button::new("bt-select-all")
                    .outline()
                    .control(cx)
                    .label(self.strings.select_all.clone())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let SelectionState::Bt {
                            files, selected, ..
                        } = &mut this.state
                        {
                            *selected = files.iter().map(|file| file.index).collect();
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("bt-deselect-all")
                    .outline()
                    .control(cx)
                    .label(self.strings.deselect_all.clone())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let SelectionState::Bt { selected, .. } = &mut this.state {
                            selected.clear();
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .text_right()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .font_features(tabular_numbers())
                    .text_color(tokens.colors.muted_foreground)
                    .child(SharedString::from(format!(
                        "{} · {}",
                        self.strings
                            .selected_count
                            .replace("{n}", &selected.len().to_string()),
                        format_bytes(selected_size.max(0) as u64)
                    ))),
            );
        let list = v_flex()
            .flex_1()
            .min_h_0()
            .children(files.iter().map(|file| {
                let checked = selected.contains(&file.index);
                let index = file.index;
                let depth = file.path.matches('/').count().min(4) as f32;
                let name = file
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&file.path)
                    .to_owned();
                let this = cx.weak_entity();
                check_row(
                    ("bt-file", index as usize),
                    checked,
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(tokens.spacing.sm)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(name)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(tokens.typography.xs.size)
                                .font_features(tabular_numbers())
                                .text_color(tokens.colors.muted_foreground)
                                .child(SharedString::from(format_bytes(file.size.max(0) as u64))),
                        ),
                    move |check, _, cx| {
                        let Ok(()) = this.update(cx, |this, cx| {
                            if let SelectionState::Bt { selected, .. } = &mut this.state {
                                if check {
                                    selected.insert(index);
                                } else {
                                    selected.remove(&index);
                                }
                            }
                            cx.notify();
                        }) else {
                            // 视图已释放，结束回调而不再更新状态。
                            return;
                        };
                    },
                    cx,
                )
                .pl(tokens.spacing.xs + tokens.spacing.lg * depth)
                .pr(tokens.spacing.sm)
                .when(checked, |row| row.bg(tokens.colors.accent))
            }))
            .overflow_y_scrollbar();
        // 保存目录在全选工具栏之上，不进文件列表的滚动区。
        v_flex()
            .size_full()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.md)
            .pb(tokens.spacing.md)
            .children(self.render_save_dir(cx))
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(list))
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let countdown = self.remaining_seconds().map(|remaining| {
            SharedString::from(
                self.strings
                    .auto_default_in
                    .replace("{seconds}", &remaining.to_string()),
            )
        });
        let confirm_label = if let SelectionState::Bt {
            files, selected, ..
        } = &self.state
        {
            let size: i64 = files
                .iter()
                .filter(|file| selected.contains(&file.index))
                .map(|file| file.size)
                .sum();
            SharedString::from(
                self.strings
                    .bt_confirm
                    .replace("{count}", &selected.len().to_string())
                    .replace("{size}", &format_bytes(size.max(0) as u64)),
            )
        } else if matches!(self.state, SelectionState::Plugin { .. }) {
            self.strings.plugin_confirm.clone()
        } else {
            self.strings.confirm.clone()
        };
        h_flex()
            .justify_between()
            .items_center()
            .px(tokens.spacing.md)
            .py(tokens.spacing.sm)
            .bg(extended.colors.chrome)
            .border_t(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .child(countdown.map_or_else(div, |countdown| {
                field_hint(countdown, cx).font_features(tabular_numbers())
            }))
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .when(self.cancellable(), |row| {
                        row.child(
                            Button::new("selection-cancel")
                                .outline()
                                .control(cx)
                                .label(self.strings.cancel.clone())
                                .disabled(self.submitting)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.cancel(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("selection-confirm")
                            .primary()
                            .control(cx)
                            .label(confirm_label)
                            .busy(self.submitting)
                            .disabled(self.submitting || !self.can_confirm(cx))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx);
                            })),
                    ),
            )
    }
}

impl Render for SelectionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        v_flex()
            .size_full()
            .bg(tokens.colors.surface)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
            .child(self.render_footer(cx))
    }
}

fn check_state(checked: bool) -> CheckState {
    if checked {
        CheckState::Checked
    } else {
        CheckState::Unchecked
    }
}

#[derive(Clone)]
struct SelectionStrings {
    hls_title: SharedString,
    hls_desc: SharedString,
    bt_title: SharedString,
    bt_desc: SharedString,
    bt_desc_single: SharedString,
    bt_confirm: SharedString,
    variant_title: SharedString,
    variant_desc: SharedString,
    plugin_title: SharedString,
    plugin_desc: String,
    plugin_default_badge: SharedString,
    plugin_remember: String,
    plugin_remember_hint: SharedString,
    plugin_confirm: SharedString,
    select_all: SharedString,
    deselect_all: SharedString,
    selected_count: SharedString,
    save_dir: SharedString,
    save_dir_placeholder: SharedString,
    browse: SharedString,
    file_picker_error: SharedString,
    confirm: SharedString,
    cancel: SharedString,
    auto_default_in: SharedString,
    action_failed: SharedString,
}

impl SelectionStrings {
    fn from_translator(translator: &Translator) -> Self {
        Self {
            hls_title: shared(translator.text("hlsQualityTitle")),
            hls_desc: shared(translator.text("hlsQualityDesc")),
            bt_title: shared(translator.text("btFileSelectTitle")),
            bt_desc: shared(translator.text("btFileSelectDesc")),
            bt_desc_single: shared(translator.text("btFileSelectDescSingle")),
            bt_confirm: shared(translator.text("btFileSelectConfirm")),
            variant_title: shared(translator.text("resolveVariantTitle")),
            variant_desc: shared(translator.text("resolveVariantDesc")),
            plugin_title: shared(translator.text("pluginChoiceTitle")),
            plugin_desc: translator.text("pluginChoiceDesc").to_owned(),
            plugin_default_badge: shared(translator.text("pluginChoiceDefaultBadge")),
            plugin_remember: translator.text("pluginChoiceRemember").to_owned(),
            plugin_remember_hint: shared(translator.text("pluginChoiceRememberHint")),
            plugin_confirm: shared(translator.text("pluginChoiceConfirm")),
            select_all: shared(translator.text("btFileSelectAll")),
            deselect_all: shared(translator.text("deselectAll")),
            selected_count: shared(translator.text("selectedCount")),
            save_dir: shared(translator.text("saveDir")),
            save_dir_placeholder: shared(translator.text("selectSaveDir")),
            browse: shared(translator.text("browse")),
            file_picker_error: shared(translator.text("filePickerErrorNative")),
            confirm: shared(translator.text("confirm")),
            cancel: shared(translator.text("cancel")),
            auto_default_in: shared(translator.text("selectionAutoDefaultIn")),
            action_failed: shared(translator.text("localServiceActionFailed")),
        }
    }
}

fn shared(value: &str) -> SharedString {
    SharedString::from(value.to_owned())
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// 保存目录输入是否允许确认。
///
/// 请求带了当前目录时，去空白后必须非空（清空不能确认，避免空路径开下）。
/// 请求没带目录（旧 daemon，`initial` 去空白后为空）时，空输入也允许确认，
/// 答复里 `save_dir = None`，引擎沿用任务当前目录。
fn bt_save_dir_confirmable(initial: &str, input: &str) -> bool {
    initial.trim().is_empty() || !input.trim().is_empty()
}

/// 把保存目录输入收成 [`SelectionOutcome::Bt`] 的 `save_dir`。
///
/// `Some` 仅当去空白后非空，且与预填目录（同样只去首尾空白，不改大小写、不换分隔符）
/// 不同：引擎据此在真正下载前搬到新目录并重开任务。未改、只改了首尾空白、或空输入
/// 都是 `None`（沿用当前目录）。
#[must_use]
fn bt_save_dir_outcome(initial: &str, input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed == initial.trim() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{bt_save_dir_confirmable, bt_save_dir_outcome};

    #[test]
    fn unchanged_dir_keeps_current() {
        assert_eq!(bt_save_dir_outcome("/data", "/data"), None);
        assert_eq!(bt_save_dir_outcome(r"D:\Movies", r"D:\Movies"), None);
        assert_eq!(
            bt_save_dir_outcome(r"\\nas\share\下载", r"\\nas\share\下载"),
            None
        );
    }

    #[test]
    fn changed_dir_is_sent_trimmed() {
        assert_eq!(
            bt_save_dir_outcome("/data", "/new"),
            Some("/new".to_owned())
        );
        assert_eq!(
            bt_save_dir_outcome("/data", "  /new  "),
            Some("/new".to_owned())
        );
        assert_eq!(
            bt_save_dir_outcome("/storage/emulated/0/Download", "/sdcard/Movies"),
            Some("/sdcard/Movies".to_owned())
        );
    }

    #[test]
    fn surrounding_whitespace_is_not_a_change() {
        assert_eq!(bt_save_dir_outcome("/data", "  /data  "), None);
        assert_eq!(bt_save_dir_outcome("  /data  ", "/data"), None);
        assert_eq!(
            bt_save_dir_outcome(
                "/var/mobile/Containers/Data/Application/uuid/Documents",
                "  /var/mobile/Containers/Data/Application/uuid/Documents  "
            ),
            None
        );
    }

    #[test]
    fn empty_input_keeps_current() {
        assert_eq!(bt_save_dir_outcome("/data", ""), None);
        assert_eq!(bt_save_dir_outcome("/data", "   "), None);
        assert_eq!(bt_save_dir_outcome("", ""), None);
        assert_eq!(bt_save_dir_outcome("", "   "), None);
    }

    #[test]
    fn empty_initial_with_typed_dir_is_sent() {
        assert_eq!(
            bt_save_dir_outcome("", "  /picked  "),
            Some("/picked".to_owned())
        );
        assert_eq!(
            bt_save_dir_outcome("", "  C:\\Users\\zero\\Downloads  "),
            Some("C:\\Users\\zero\\Downloads".to_owned())
        );
    }

    #[test]
    fn path_text_is_sent_as_typed_beyond_trimming() {
        // 客户端只去首尾空白、不做路径规范化：内部空格与末尾分隔符原样带回，
        // 是否同一目录由引擎（`normalize_save_dir`）判定。
        assert_eq!(
            bt_save_dir_outcome("/data", "/data/My  Movies"),
            Some("/data/My  Movies".to_owned())
        );
        assert_eq!(
            bt_save_dir_outcome(r"D:\Movies", "D:\\Movies\\"),
            Some("D:\\Movies\\".to_owned())
        );
    }

    #[test]
    fn confirmable_requires_dir_unless_old_daemon() {
        assert!(!bt_save_dir_confirmable("/data", ""));
        assert!(!bt_save_dir_confirmable("/data", "   "));
        assert!(bt_save_dir_confirmable("/data", "/data"));
        assert!(bt_save_dir_confirmable("/data", "/new"));
        // 旧 daemon 没带目录：空输入仍可确认（答复 None）。
        assert!(bt_save_dir_confirmable("", ""));
        assert!(bt_save_dir_confirmable("", "   "));
        assert!(bt_save_dir_confirmable("  ", ""));
        assert!(bt_save_dir_confirmable("", r"D:\Movies"));
    }
}
