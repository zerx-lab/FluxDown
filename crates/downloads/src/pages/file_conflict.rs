//! 「文件已存在」聚合窗口内容：所有待确认的 `fileExists` 请求合并在同一个窗口里——单条时是
//! 卡片 + 动作列表，多条时是紧凑列表 + 底部批量按钮。
//!
//! 由 app 经 `session::attach` 驱动（[`SessionConsumer`] 三个同名 `pub fn`）；窗口的打开 /
//! 置前由 app 决定，本视图只在请求清空或用户选择「稍后决定」（关闭窗口，不发 RPC，请求继续
//! 等待直到超时按默认重命名）时发出 [`FileConflictEvent::Close`]。
//!
//! 答复走 `daemon.selection.resolve`：成功或「已被别处答复 / 已过期」（`Conflict` /
//! `NotFound`）都只是把该行移除；其它错误保留该行并弹错误提示，用户可以重试。

mod many;
mod single;

use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, ApplicationErrorCode, DaemonEvent, FileExistsAction,
    SelectionOutcome, SelectionResolutionDto, ServiceEvent,
};
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{BusyExt as _, ControlExt as _, field_hint, tabular_numbers};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Context, Div, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::{
    components::file_icon::install_port,
    controller::{DownloadsCommand, DownloadsPort},
    model::file_conflict::{FileConflictSet, remaining_seconds},
};

/// 窗口建议尺寸（逻辑像素）：固定大小，内容在窗口内滚动。
pub const FILE_CONFLICT_WINDOW_WIDTH: f32 = 600.;
pub const FILE_CONFLICT_WINDOW_HEIGHT: f32 = 520.;

/// 视图对宿主的事件。
pub enum FileConflictEvent {
    /// 关闭承载窗口（无待确认请求，或用户选择稍后决定）。
    Close,
}

/// 在途答复对应的按钮。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answering {
    Skip,
    Overwrite,
    Rename,
    Cancel,
}

/// 一次答复的回执归类。
enum Verdict {
    /// daemon 已接受。
    Resolved,
    /// 已被别处答复或已过期：本地同样视为结束。
    Gone,
    /// 其它失败（网络 / RPC）：保留该行供重试。
    Failed,
}

pub struct FileConflictView {
    strings: ConflictStrings,
    port: Arc<dyn DownloadsPort>,
    set: FileConflictSet,
    /// 已发出答复、等待回执的请求：回执前整行禁用，防重复点击。
    inflight: HashSet<String>,
    /// 与服务的连接中断：答复发不出去，操作禁用，等重连快照。
    stale: bool,
    closed: bool,
    focus_handle: FocusHandle,
    /// 最近一次发出答复的选项，仅用于在途时标出被点击的按钮。
    answering: Option<Answering>,
}

impl EventEmitter<FileConflictEvent> for FileConflictView {}

impl FileConflictView {
    /// 创建视图并启动每秒一次的倒计时重绘（视图随窗口释放后循环自然结束）。
    pub fn new(
        translator: Entity<Translator>,
        port: Arc<dyn DownloadsPort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        install_port(&port, cx);
        let strings = ConflictStrings::from_translator(translator.read(cx));
        cx.observe(&translator, |this, translator, cx| {
            this.strings = ConflictStrings::from_translator(translator.read(cx));
            cx.notify();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        let focus_handle = cx.focus_handle();
        let initial_focus = focus_handle.clone();
        window.defer(cx, move |window, cx| window.focus(&initial_focus, cx));
        Self {
            strings,
            port,
            set: FileConflictSet::default(),
            inflight: HashSet::new(),
            stale: false,
            closed: false,
            focus_handle,
            answering: None,
        }
    }

    // ---- SessionConsumer（app 经 `attach` 驱动）----

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
        self.stale = false;
        self.absorb(&snapshot.daemon.pending_selections);
        self.after_change(cx);
    }

    pub fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
        let ServiceEvent::Agent(event) = event else {
            return;
        };
        let changed = match event {
            AgentEvent::Daemon(DaemonEvent::SelectionPending(request)) => self.set.upsert(request),
            AgentEvent::Daemon(DaemonEvent::SelectionResolved { request_id }) => {
                self.inflight.remove(request_id);
                self.set.remove(request_id).is_some()
            }
            AgentEvent::DaemonSnapshotReplaced(snapshot)
            | AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(snapshot)) => {
                self.absorb(&snapshot.pending_selections);
                true
            }
            _ => false,
        };
        if changed {
            self.after_change(cx);
        }
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        cx.notify();
    }

    fn absorb(&mut self, requests: &[fluxdown_protocol::SelectionRequestDto]) {
        self.set.replace(requests);
        self.inflight
            .retain(|id| self.set.items().iter().any(|item| item.request_id == *id));
    }

    /// 清空即关窗，否则重绘。
    fn after_change(&mut self, cx: &mut Context<Self>) {
        if self.set.is_empty() {
            self.close(cx);
        } else {
            cx.notify();
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.closed {
            self.closed = true;
            cx.emit(FileConflictEvent::Close);
        }
    }

    // ---- 动作 ----

    /// 该请求的操作是否禁用（断线或答复在途）。
    fn row_busy(&self, request_id: &str) -> bool {
        self.stale || self.inflight.contains(request_id)
    }

    fn any_busy(&self) -> bool {
        self.stale || !self.inflight.is_empty()
    }

    /// 该选项是用户刚点的、且答复仍在途：其按钮显示转圈（其余选项只是禁用）。
    fn is_answering(&self, answer: Answering) -> bool {
        self.answering == Some(answer) && !self.inflight.is_empty()
    }

    fn answer_all(
        &mut self,
        action: FileExistsAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids = self.pending_ids();
        self.answer(ids, SelectionOutcome::FileExists { action }, window, cx);
    }

    fn cancel_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.pending_ids();
        self.answer(ids, SelectionOutcome::Cancelled, window, cx);
    }

    fn pending_ids(&self) -> Vec<String> {
        self.set
            .items()
            .iter()
            .map(|item| item.request_id.clone())
            .collect()
    }

    fn answer(
        &mut self,
        ids: Vec<String>,
        outcome: SelectionOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.stale {
            return;
        }
        let requests: Vec<_> = ids
            .into_iter()
            .filter(|id| self.inflight.insert(id.clone()))
            .map(|request_id| {
                let future =
                    self.port
                        .execute(DownloadsCommand::ResolveSelection(SelectionResolutionDto {
                            request_id: request_id.clone(),
                            outcome: outcome.clone(),
                        }));
                (request_id, future)
            })
            .collect();
        if requests.is_empty() {
            return;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let mut settled = Vec::with_capacity(requests.len());
            for (request_id, future) in requests {
                let verdict = match future.await {
                    Ok(_) => Verdict::Resolved,
                    Err(error)
                        if matches!(
                            error.code,
                            ApplicationErrorCode::Conflict | ApplicationErrorCode::NotFound
                        ) =>
                    {
                        Verdict::Gone
                    }
                    Err(_) => Verdict::Failed,
                };
                settled.push((request_id, verdict));
            }
            let Ok(()) = this.update_in(cx, |this, window, cx| this.settle(settled, window, cx))
            else {
                // 视图或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn settle(
        &mut self,
        settled: Vec<(String, Verdict)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut failed = false;
        for (request_id, verdict) in settled {
            self.inflight.remove(&request_id);
            match verdict {
                Verdict::Resolved | Verdict::Gone => {
                    self.set.remove(&request_id);
                }
                Verdict::Failed => failed = true,
            }
        }
        if failed {
            window.push_notification(toast::error(self.strings.action_failed.clone()), cx);
        }
        self.after_change(cx);
    }

    /// Enter = 重命名（多条时为全部重命名）；Esc = 稍后决定。
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.close(cx),
            // 焦点在按钮上时回车由按钮自己处理，这里只接管焦点在窗口根上的回车。
            "enter" if self.focus_handle.is_focused(window) && !self.any_busy() => {
                self.answering = Some(Answering::Rename);
                self.answer_all(FileExistsAction::Rename, window, cx);
            }
            _ => {}
        }
    }

    // ---- 渲染 ----

    fn render_header(&self, cx: &Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let title_style = theme.extended().title;
        let title = if self.set.len() > 1 {
            SharedString::from(
                self.strings
                    .title_many
                    .replace("{count}", &self.set.len().to_string()),
            )
        } else {
            self.strings.title.clone()
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
                    .child(self.strings.desc.clone()),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let many = self.set.len() > 1;
        let busy = self.any_busy();
        let seconds = self
            .set
            .earliest_deadline()
            .map_or(0, |deadline| remaining_seconds(deadline, now_unix_ms()));
        let countdown = SharedString::from(
            self.strings
                .auto_rename_in
                .replace("{seconds}", &seconds.to_string()),
        );
        let bulk = many.then(|| {
            h_flex()
                .justify_end()
                .gap(tokens.spacing.sm)
                .when(self.set.all_allow(FileExistsAction::Skip), |row| {
                    row.child(
                        Button::new("file-conflict-skip-all")
                            .outline()
                            .control(cx)
                            .label(self.strings.skip_all.clone())
                            .busy(self.is_answering(Answering::Skip))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.answering = Some(Answering::Skip);
                                this.answer_all(FileExistsAction::Skip, window, cx);
                            })),
                    )
                })
                .when(self.set.all_allow(FileExistsAction::Overwrite), |row| {
                    row.child(
                        Button::new("file-conflict-overwrite-all")
                            .outline()
                            .control(cx)
                            .label(self.strings.overwrite_all.clone())
                            .busy(self.is_answering(Answering::Overwrite))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.answering = Some(Answering::Overwrite);
                                this.answer_all(FileExistsAction::Overwrite, window, cx);
                            })),
                    )
                })
                .child(
                    Button::new("file-conflict-rename-all")
                        .primary()
                        .control(cx)
                        .label(self.strings.rename_all.clone())
                        .busy(self.is_answering(Answering::Rename))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.answering = Some(Answering::Rename);
                            this.answer_all(FileExistsAction::Rename, window, cx);
                        })),
                )
        });
        let cancel_label = if many {
            self.strings.cancel_all.clone()
        } else {
            self.strings.cancel_download.clone()
        };
        v_flex()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.md)
            .py(tokens.spacing.sm)
            .bg(extended.colors.chrome)
            .border_t(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .children(bulk)
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        Button::new("file-conflict-cancel")
                            .ghost()
                            .control(cx)
                            .label(cancel_label)
                            .tooltip(self.strings.cancel_hint.clone())
                            .busy(self.is_answering(Answering::Cancel))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.answering = Some(Answering::Cancel);
                                this.cancel_all(window, cx);
                            })),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(tokens.spacing.sm)
                            .child(field_hint(countdown, cx).font_features(tabular_numbers()))
                            .child(
                                Button::new("file-conflict-later")
                                    .outline()
                                    .control(cx)
                                    .label(self.strings.later.clone())
                                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                            ),
                    ),
            )
    }
}

impl Render for FileConflictView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let body = if self.set.len() > 1 {
            self.render_many(window, cx)
        } else {
            self.render_single(window, cx)
        };
        v_flex()
            .size_full()
            .key_context("FileConflict")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .bg(tokens.colors.surface)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_footer(cx))
    }
}

/// 本窗口用到的文案；语言切换时整体重取，渲染热路径只克隆 `SharedString`。
#[derive(Clone)]
struct ConflictStrings {
    title: SharedString,
    title_many: SharedString,
    desc: SharedString,
    existing: SharedString,
    incoming: SharedString,
    size_unknown: SharedString,
    modified: SharedString,
    rename: SharedString,
    rename_as: SharedString,
    overwrite: SharedString,
    overwrite_hint: SharedString,
    skip: SharedString,
    skip_hint: SharedString,
    rename_all: SharedString,
    overwrite_all: SharedString,
    skip_all: SharedString,
    cancel_download: SharedString,
    cancel_hint: SharedString,
    cancel_all: SharedString,
    later: SharedString,
    auto_rename_in: SharedString,
    action_failed: SharedString,
}

impl ConflictStrings {
    fn from_translator(translator: &Translator) -> Self {
        let text = |key: &str| SharedString::from(translator.text(key).to_owned());
        Self {
            title: text("fileConflictTitle"),
            title_many: text("fileConflictTitleMany"),
            desc: text("fileConflictDesc"),
            existing: text("fileConflictExisting"),
            incoming: text("fileConflictIncoming"),
            size_unknown: text("fileConflictSizeUnknown"),
            modified: text("fileConflictModified"),
            rename: text("fileConflictRename"),
            rename_as: text("fileConflictRenameAs"),
            overwrite: text("fileConflictOverwrite"),
            overwrite_hint: text("fileConflictOverwriteHint"),
            skip: text("fileConflictSkip"),
            skip_hint: text("fileConflictSkipHint"),
            rename_all: text("fileConflictRenameAll"),
            overwrite_all: text("fileConflictOverwriteAll"),
            skip_all: text("fileConflictSkipAll"),
            cancel_download: text("fileConflictCancelDownload"),
            cancel_hint: text("fileConflictCancelHint"),
            cancel_all: text("fileConflictCancelAll"),
            later: text("fileConflictLater"),
            auto_rename_in: text("fileConflictAutoRenameIn"),
            action_failed: text("localServiceActionFailed"),
        }
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}
