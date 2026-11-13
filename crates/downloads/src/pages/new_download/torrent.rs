//! 嵌在现有新建下载窗口里的种子确认。
use std::collections::{HashSet, VecDeque};

use fluxdown_protocol::{ApplicationErrorCode, BtFileDto, RpcErrorData};
use fluxdown_ui_components::{ControlExt as _, field_error, field_hint, form_field};
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, Context, Div, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div, px,
};
use gpui_component::{
    Disableable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    scroll::ScrollableElement as _,
    v_flex,
};

use super::NewDownloadView;
use crate::{
    components::bt_file_picker::{BtFilePickerLabels, bt_file_picker},
    controller::{DownloadsCommand, DownloadsResult, LAST_SAVE_DIR_PREF},
    model::{
        devices::DispatchTarget, new_download::BuiltRequests,
        save_dir_category::category_mode_preference,
    },
    strings::error_text,
    submission::NewDownloadSubmission,
};

mod model;
pub(super) use model::TorrentForm;
use model::{ItemOutcome, TorrentDraft, custom_name_for, custom_name_issue_key};
pub(super) use model::{TorrentInput, TorrentOrigin, split_inputs};

fn cleanup_result(result: &Result<DownloadsResult, RpcErrorData>) -> Option<DownloadsCommand> {
    match result {
        Ok(DownloadsResult::TorrentPreview(preview)) => {
            Some(DownloadsCommand::DiscardTorrentPreview {
                torrent_blob_id: preview.torrent_blob_id.clone(),
            })
        }
        Ok(DownloadsResult::MagnetPreviewTicket(ticket)) => {
            Some(DownloadsCommand::CancelMagnetPreview {
                preview_id: ticket.preview_id.clone(),
            })
        }
        _ => None,
    }
}

impl NewDownloadView {
    pub(super) fn open_torrents(
        &mut self,
        inputs: Vec<TorrentInput>,
        origin: TorrentOrigin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if inputs.is_empty() {
            return;
        }
        let from_host = matches!(origin, TorrentOrigin::Host { .. });
        let (later, queue, external, resume_submit) = match origin {
            TorrentOrigin::Host { external } => (false, None, external, None),
            TorrentOrigin::Picker => (false, None, false, None),
            TorrentOrigin::Submit { later, queue } => {
                (later, queue.clone(), false, Some((later, queue)))
            }
        };
        let options = self.draft_options(later, queue, cx);
        let built = crate::model::new_download::build_requests(&self.entries, &options);
        let drafts = inputs
            .into_iter()
            .map(|input| {
                let request = match &input {
                    TorrentInput::Magnet(url) => built
                        .items
                        .iter()
                        .find(|item| item.url == *url)
                        .map(|item| built.request_for(item))
                        .unwrap_or_else(|| fluxdown_protocol::CreateTaskRequest {
                            url: url.clone(),
                            ..built.common.clone()
                        }),
                    TorrentInput::File(_) => built.common.clone(),
                };
                TorrentDraft {
                    input,
                    options: request,
                    external,
                }
            })
            .collect::<VecDeque<_>>();
        if let Some(form) = self.torrent.as_mut() {
            form.pending.extend(drafts);
            // 用户已经在这个窗口里操作过：后来的输入不再让整批结束后自动关窗。
            form.close_when_idle &= from_host;
            cx.notify();
            return;
        }
        // 种子确认只在本机建任务：先把「下载到」临时切到本机，退出确认时还原用户
        // 原来选的远端目标（否则取消一次种子就把用户的选择弄丢了）。
        let restore_target = self.target.clone();
        let restore_category_edited = self.category_dir_manually_edited;
        self.target = DispatchTarget::Local;
        self.save_dir_menu_open = false;
        self.torrent = Some(TorrentForm {
            pending: drafts,
            preview: None,
            ticket: None,
            selected: HashSet::new(),
            filled_name: String::new(),
            restore_target,
            restore_category_edited,
            close_when_idle: from_host,
            resume_submit,
            submitted_any: false,
            loading: false,
            submitting: false,
            error: None,
            expired: false,
        });
        self.save_dir
            .update(cx, |input, cx| input.focus(window, cx));
        self.reset_torrent_name(window, cx);
        self.load_torrent(window, cx);
    }

    fn reset_torrent_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.torrent.as_mut() else {
            return;
        };
        let name = form.begin_item();
        self.torrent_name
            .update(cx, |input, cx| input.set_value(name, window, cx));
    }

    fn load_torrent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.torrent.as_mut() else {
            return;
        };
        if form.loading || form.submitting || form.preview.is_some() {
            return;
        }
        let command = match form.pending.front().map(|draft| &draft.input) {
            Some(TorrentInput::File(path)) => DownloadsCommand::PreviewTorrentFile {
                path: path.to_string_lossy().into_owned(),
            },
            Some(TorrentInput::Magnet(url)) => {
                DownloadsCommand::PrepareMagnetPreview { url: url.clone() }
            }
            None => return,
        };
        form.loading = true;
        form.error = None;
        form.expired = false;
        self.torrent_generation = self.torrent_generation.wrapping_add(1);
        self.run_torrent_preview(command, self.torrent_generation, window, cx);
        cx.notify();
    }

    pub(super) fn refresh_torrent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load_torrent(window, cx);
    }

    fn run_torrent_preview(
        &self,
        command: DownloadsCommand,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let port = self.port.clone();
        let request = port.execute(command);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let cleanup = cleanup_result(&result);
            let accepted = this
                .update_in(cx, |this, window, cx| {
                    if this.torrent_generation != generation || this.torrent.is_none() {
                        return false;
                    }
                    this.finish_torrent_preview(result, generation, window, cx);
                    true
                })
                .unwrap_or(false);
            // 表单已关闭 / 已取消时仍可能收到迟到的上传或预约，这里负责清理。
            if !accepted
                && let Some(command) = cleanup
                && let Err(error) = port.execute(command).await
            {
                eprintln!("late torrent preview cleanup failed: {:?}", error.code);
            }
        })
        .detach();
    }

    fn finish_torrent_preview(
        &mut self,
        result: Result<DownloadsResult, RpcErrorData>,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.torrent.as_mut() else {
            return;
        };
        match result {
            Ok(DownloadsResult::MagnetPreviewTicket(ticket)) => {
                form.ticket = Some(ticket.preview_id.clone());
                self.run_torrent_preview(
                    DownloadsCommand::ResolveMagnetPreview {
                        preview_id: ticket.preview_id,
                    },
                    generation,
                    window,
                    cx,
                );
            }
            Ok(DownloadsResult::TorrentPreview(preview)) => {
                form.ticket = None;
                form.selected = preview.files.iter().map(|file| file.index).collect();
                // 每换一个待确认种子都把「文件名」重置成它自己的原始名称，避免上一个
                // 种子的改名串到下一个；但如果用户已经在输入框里敲了别的东西（磁力
                // 解析很慢时很常见），迟到的结果不能把他敲的内容覆盖掉。
                let current = self.torrent_name.read(cx).value().to_string();
                if let Some(name) = form.preview_name(&current, &preview.name) {
                    self.torrent_name.update(cx, |input, cx| {
                        input.set_value(name, window, cx);
                    });
                }
                form.preview = Some(preview);
                form.loading = false;
            }
            result => {
                let error = result
                    .err()
                    .unwrap_or_else(|| RpcErrorData::new(ApplicationErrorCode::Internal, false));
                form.error = Some(error_text(self.translator.read(cx), &error).into());
                form.loading = false;
                if let Some(preview_id) = form.ticket.take() {
                    let cleanup = self
                        .port
                        .execute(DownloadsCommand::CancelMagnetPreview { preview_id });
                    cx.background_executor()
                        .spawn(async move {
                            if let Err(error) = cleanup.await {
                                eprintln!("magnet cleanup failed: {:?}", error.code);
                            }
                        })
                        .detach();
                }
            }
        }
        cx.notify();
    }

    /// 把种子确认期间临时改写的共享状态（下载目标 / 目录手动编辑标记）还给通用表单。
    fn restore_dispatch_state(&mut self, form: &TorrentForm) {
        self.target = form.restore_target.clone();
        self.category_dir_manually_edited = form.restore_category_edited;
    }

    /// 整批结束：还原目标状态。窗口是为这批种子开的、或已经建过任务，且表单里没有
    /// 其他链接时关窗；表单提交分流出来的磁力链全部建完任务后，按原来的「稍后 / 队列」
    /// 选择继续提交剩下的普通链接（还留着磁力链说明有被跳过的，不替用户重提）。
    fn finish_torrent_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.torrent.take() else {
            return;
        };
        self.restore_dispatch_state(&form);
        if self.entries.is_empty() {
            if form.submitted_any || form.close_when_idle {
                window.remove_window();
            }
        } else if let Some((later, queue)) = form.resume_submit
            && !self.entries.iter().any(|entry| {
                self.capture_for(&entry.url).is_none()
                    && fluxdown_protocol::capture_link::is_magnet_link(&entry.url)
            })
        {
            self.submit(later, queue, window, cx);
        }
        cx.notify();
    }

    /// 当前这颗种子处理完：换下一颗，或结束整批。已建任务的磁力链从表单的链接框里
    /// 移除；被跳过的留着。
    fn advance_torrent(
        &mut self,
        outcome: ItemOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.torrent.as_mut() else {
            return;
        };
        form.preview = None;
        form.ticket = None;
        form.selected.clear();
        form.loading = false;
        form.submitting = false;
        form.error = None;
        form.expired = false;
        let source = form.pending.pop_front();
        let more = !form.pending.is_empty();
        if outcome == ItemOutcome::Submitted {
            form.submitted_any = true;
            if let Some(TorrentDraft {
                input: TorrentInput::Magnet(url),
                ..
            }) = source
            {
                self.entries.retain(|entry| entry.url != url);
                let text = self
                    .entries
                    .iter()
                    .map(crate::model::new_download::entry_to_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.urls
                    .update(cx, |input, cx| input.set_value(text, window, cx));
            }
        }
        if more {
            self.reset_torrent_name(window, cx);
            self.load_torrent(window, cx);
            cx.notify();
        } else {
            self.finish_torrent_batch(window, cx);
        }
    }

    fn spawn_torrent_cleanup(&self, commands: Vec<DownloadsCommand>, cx: &mut App) {
        for command in commands {
            let cleanup = self.port.execute(command);
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = cleanup.await {
                        eprintln!("torrent preview cleanup failed: {:?}", error.code);
                    }
                })
                .detach();
        }
    }

    /// 放弃当前这颗种子的预览资源（已上传的 blob、在途的磁力预约）。换代后迟到的
    /// 预览结果不再被表单接受，由 `run_torrent_preview` 的清理分支释放。
    fn discard_current_torrent(&mut self, cx: &mut App) {
        self.torrent_generation = self.torrent_generation.wrapping_add(1);
        let Some(form) = self.torrent.as_mut() else {
            return;
        };
        let mut commands = Vec::new();
        if let Some(preview) = form.preview.take() {
            commands.push(DownloadsCommand::DiscardTorrentPreview {
                torrent_blob_id: preview.torrent_blob_id,
            });
        }
        if let Some(preview_id) = form.ticket.take() {
            commands.push(DownloadsCommand::CancelMagnetPreview { preview_id });
        }
        form.loading = false;
        self.spawn_torrent_cleanup(commands, cx);
    }

    pub(super) fn release_torrent(&mut self, cx: &mut App) {
        self.torrent_generation = self.torrent_generation.wrapping_add(1);
        let Some(mut form) = self.torrent.take() else {
            return;
        };
        self.restore_dispatch_state(&form);
        let mut commands = Vec::new();
        // 确认已发出（submitting）时不要丢弃 blob：daemon 把确认与丢弃串行在同一把
        // 提交锁上，丢弃抢到锁就会让用户明确确认过的种子以 NotFound 失败，而窗口已经
        // 关了、没人看得到提示。残留 blob 由 TTL（10 分钟）兜底。
        if !form.submitting
            && let Some(preview) = form.preview.take()
        {
            commands.push(DownloadsCommand::DiscardTorrentPreview {
                torrent_blob_id: preview.torrent_blob_id,
            });
        }
        if let Some(preview_id) = form.ticket.take() {
            commands.push(DownloadsCommand::CancelMagnetPreview { preview_id });
        }
        self.spawn_torrent_cleanup(commands, cx);
    }

    /// 取消整批：不建任务，释放预览资源。窗口是为这批种子开的（或已经建过任务）
    /// 且表单里没有其他链接时一并关窗，否则回到表单。
    fn cancel_torrent_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.torrent.as_ref() else {
            return;
        };
        if form.submitting {
            return;
        }
        let close = (form.close_when_idle || form.submitted_any) && self.entries.is_empty();
        self.release_torrent(cx);
        if close {
            window.remove_window();
        }
        cx.notify();
    }

    /// 跳过当前这颗：不建任务，继续处理排在后面的。
    fn skip_torrent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.torrent.as_ref().is_none_or(|form| form.submitting) {
            return;
        }
        self.discard_current_torrent(cx);
        self.advance_torrent(ItemOutcome::Skipped, window, cx);
    }

    /// 跳过预览直接添加当前这条磁力链：沿普通建任务路径提交，文件选择回到任务拿到
    /// 元数据之后的选择窗口。元数据迟迟解析不出来时，任务仍能先进列表继续等。
    fn add_magnet_directly(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking {
            return;
        }
        let save_dir = self.save_dir.read(cx).value().trim().to_owned();
        let typed_name = self.torrent_name.read(cx).value().to_string();
        let Some(request) = self
            .torrent
            .as_ref()
            .and_then(|form| form.direct_request(&save_dir, &typed_name))
        else {
            return;
        };
        self.discard_current_torrent(cx);
        self.record_save_dir_history(&save_dir, cx);
        let BuiltRequests { common, items } = BuiltRequests::from_single(request);
        (self.on_submit)(
            NewDownloadSubmission::Tasks {
                common: Box::new(common),
                items,
                captures: Vec::new(),
                // 目录是确认界面里用户看到并可改的那个，原样使用，不再按分类改道。
                category_route: None,
                last_category_mode: category_mode_preference(&self.category_mode),
                offered_save_dir: self.context.save_dir.clone(),
            },
            window,
            cx,
        );
        self.advance_torrent(ItemOutcome::Submitted, window, cx);
    }

    pub(super) fn confirm_torrent(
        &mut self,
        later: bool,
        queue: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let save_dir = self.save_dir.read(cx).value().trim().to_owned();
        let custom_name = self.torrent_custom_name(cx);
        let Some(form) = self.torrent.as_mut() else {
            return;
        };
        if self.picking {
            return;
        }
        let queue_id = form.confirmation_queue(later, queue);
        let Some(params) = form.confirmation(&save_dir, queue_id, later, custom_name) else {
            return;
        };
        form.submitting = true;
        form.error = None;
        let generation = self.torrent_generation;
        let external = form.pending.front().is_some_and(|draft| draft.external);
        let view = cx.entity().downgrade();
        let handle = window.window_handle();
        let done: super::TorrentConfirmationDone = Box::new(move |result, cx: &mut App| {
            matches!(
                handle.update(cx, |_, window, cx| {
                    view.update(cx, |this, cx| {
                        if generation != this.torrent_generation {
                            return false;
                        }
                        let Some(form) = this.torrent.as_mut() else {
                            return false;
                        };
                        form.submitting = false;
                        match result {
                            Ok(_) => {
                                this.record_save_dir_history(&save_dir, cx);
                                this.write_local_preference(
                                    LAST_SAVE_DIR_PREF,
                                    save_dir.clone().into(),
                                    cx,
                                );
                                // daemon 建任务后已消费 blob，这里只推进队列。
                                this.advance_torrent(ItemOutcome::Submitted, window, cx);
                            }
                            Err(error) => {
                                form.expired = error.code == ApplicationErrorCode::NotFound;
                                form.error = Some(if form.expired {
                                    this.translator
                                        .read(cx)
                                        .text("torrentConfirmExpired")
                                        .to_owned()
                                        .into()
                                } else {
                                    error_text(this.translator.read(cx), &error).into()
                                });
                                // 失败原因已经显示在表单里了，不再弹一次语义不同的 toast：
                                // 过期场景的 toast 会走通用的 invalidArgument 文案，和上面的
                                //「预览已过期」互相矛盾。
                            }
                        }
                        cx.notify();
                        true
                    })
                }),
                Ok(Ok(true))
            )
        });
        (self.on_confirm)(Box::new(params), external, done, cx);
        cx.notify();
    }

    fn torrent_text(&self, key: &str, cx: &App) -> SharedString {
        self.translator.read(cx).text(key).to_owned().into()
    }

    /// 提交用的改名目标（见 [`custom_name_for`]）。
    fn torrent_custom_name(&self, cx: &App) -> String {
        let Some(preview) = self
            .torrent
            .as_ref()
            .and_then(|form| form.preview.as_deref())
        else {
            return String::new();
        };
        custom_name_for(preview, self.torrent_name.read(cx).value().as_ref())
    }

    /// 「文件名」输入不合法时的本地化说明；`None` = 可用（「留空 / 没改动」也算可用）。
    ///
    /// 判据来自 protocol，与 daemon 校验同一套规则；这里只负责把它翻成人话，
    /// 让用户在点按钮之前就看到问题。要点：**只有真的改过名才校验**——预填的原始
    /// 名称可能带 `:`、`/` 或超长，那是种子作者起的名字，不是用户的改名意图。
    fn torrent_name_issue(&self, cx: &App) -> Option<SharedString> {
        let preview = self.torrent.as_ref()?.preview.as_deref()?;
        let typed = self.torrent_name.read(cx).value();
        let custom = custom_name_for(preview, typed.as_ref());
        let key = custom_name_issue_key(&custom)?;
        Some(self.translator.read(cx).text(key).to_owned().into())
    }

    pub(super) fn render_torrent(&self, cx: &mut Context<Self>) -> Div {
        let Some(form) = &self.torrent else {
            return div();
        };
        let tokens = active_theme(cx).tokens().clone();
        let ready = self.torrent_ready(form, cx);
        v_flex()
            .size_full()
            .p(tokens.spacing.lg)
            .gap(tokens.spacing.md)
            .bg(tokens.colors.surface)
            .child(self.render_torrent_fields(form, cx))
            .child(self.render_torrent_files(form, cx))
            .child(self.render_torrent_actions(form, ready, cx))
    }

    /// 确认按钮是否可用：预览就绪、至少选了一个文件、目录与名称都合法。
    fn torrent_ready(&self, form: &TorrentForm, cx: &App) -> bool {
        !form.expired
            && !form.loading
            && !form.submitting
            && !self.picking
            && form.preview.is_some()
            && !form.selected.is_empty()
            && !self.save_dir.read(cx).value().trim().is_empty()
            && self.torrent_name_issue(cx).is_none()
    }

    /// 表单上段：标题、说明、保存目录、文件名与本地校验提示。
    ///
    /// `h_auto()` 不能省：`overflow_y_scrollbar` 的包装层自带 `size_full()`，元素自己的
    /// 高会经 `refine_style` 变成「父高的 100%」——放进 `size_full` 的列里就是一块永远
    /// 等于 `max_h` 的固定区域，把下面的文件表挤成 0 高（确认框只剩「全选 / 取消全选」、
    /// 看不到任何文件，就是这个原因）。按内容定高后，只有内容真的超过 `max_h` 时字段区
    /// 才在内部滚动，小窗口下底部按钮也不会被挤出可视区。
    fn render_torrent_fields(
        &self,
        form: &TorrentForm,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let mut fields = v_flex()
            .w_full()
            .flex_none()
            .h_auto()
            .gap(tokens.spacing.md)
            .max_h(px(320.))
            .overflow_y_scrollbar()
            .child(div().child(self.torrent_text("torrentConfirmTitle", cx)))
            .child(field_hint(self.torrent_text("torrentConfirmHint", cx), cx))
            .child(self.render_save_dir(cx))
            .child(form_field(
                self.torrent_text("torrentConfirmName", cx),
                Input::new(&self.torrent_name).control(cx).w_full(),
                Some(self.torrent_text("torrentConfirmNameHint", cx)),
                cx,
            ))
            .children(
                self.torrent_name_issue(cx)
                    .map(|issue| field_error(issue, cx)),
            )
            .children(form.error.clone().map(|error| field_hint(error, cx)));
        // 排在当前种子后面还有几颗种子：只有多颗时才有信息量。
        if form.pending.len() > 1 {
            fields = fields.child(field_hint(
                self.translator.read(cx).text_with(
                    "torrentConfirmRemaining",
                    &[("count", &form.pending.len().to_string())],
                ),
                cx,
            ));
        }
        fields
    }

    /// 文件选择区：原始名称提示与文件勾选（与引擎发起的 BT 文件选择窗口共用同一个
    /// 组件）。预览未就绪时显示读取状态，磁力链可以跳过预览直接添加。
    fn render_torrent_files(&self, form: &TorrentForm, cx: &mut Context<Self>) -> Div {
        let tokens = active_theme(cx).tokens().clone();
        let mut area = v_flex().w_full().flex_1().min_h_0().gap(tokens.spacing.sm);
        let Some(preview) = &form.preview else {
            area = area.child(field_hint(
                self.torrent_text(
                    if form.loading {
                        "torrentConfirmLoading"
                    } else {
                        "torrentConfirmFailed"
                    },
                    cx,
                ),
                cx,
            ));
            let magnet = form.current_is_magnet();
            let mut actions = h_flex().gap(tokens.spacing.sm);
            if !form.loading {
                actions = actions.child(
                    Button::new("torrent-retry")
                        .label(self.torrent_text("torrentConfirmRetry", cx))
                        .control(cx)
                        .on_click(cx.listener(|this, _, window, cx| this.load_torrent(window, cx))),
                );
            }
            if magnet {
                actions = actions.child(
                    Button::new("torrent-add-directly")
                        .label(self.torrent_text("torrentConfirmAddDirectly", cx))
                        .control(cx)
                        .disabled(
                            self.picking
                                || form.submitting
                                || self.save_dir.read(cx).value().trim().is_empty(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_magnet_directly(window, cx);
                        })),
                );
            }
            area = area.child(actions);
            if magnet {
                area = area.child(field_hint(
                    self.torrent_text("torrentConfirmAddDirectlyHint", cx),
                    cx,
                ));
            }
            return area;
        };
        // 只有真的改过名才提示原始名称：没改名时输入框里就是它，这一行纯属重复。
        let typed = self.torrent_name.read(cx).value();
        if !custom_name_for(preview, typed.as_ref()).is_empty() {
            area = area.child(
                // 原始名称可能是超长中文 / 带换行符的种子名：单行省略，完整值在
                // 上面的输入框里本来就看得到。
                field_hint(
                    self.translator
                        .read(cx)
                        .text_with("torrentConfirmOriginalName", &[("name", &preview.name)]),
                    cx,
                )
                .w_full()
                .min_w_0()
                .truncate(),
            );
        }
        let labels = BtFilePickerLabels::from_translator(self.translator.read(cx));
        area = area.child(bt_file_picker(
            &preview.files,
            &form.selected,
            &labels,
            Self::torrent_picker_state,
            cx,
        ));
        if form.expired {
            area = area.child(self.torrent_expired_retry(cx));
        }
        area
    }

    /// 文件勾选组件读写的状态：预览就绪且未在提交时才可改。
    fn torrent_picker_state(&mut self) -> Option<(&[BtFileDto], &mut HashSet<i32>)> {
        let form = self.torrent.as_mut()?;
        if form.submitting {
            return None;
        }
        let preview = form.preview.as_deref()?;
        Some((preview.files.as_slice(), &mut form.selected))
    }

    fn torrent_expired_retry(&self, cx: &mut Context<Self>) -> Div {
        div().child(
            Button::new("torrent-expired-retry")
                .label(self.torrent_text("torrentConfirmRetry", cx))
                .control(cx)
                .on_click(cx.listener(|this, _, window, cx| {
                    if let Some(form) = &mut this.torrent
                        && !form.submitting
                        && !form.loading
                    {
                        form.preview = None; // NotFound means the daemon no longer retains this blob.
                        form.selected.clear();
                        this.load_torrent(window, cx);
                    }
                })),
        )
    }

    fn render_torrent_actions(
        &self,
        form: &TorrentForm,
        ready: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let tokens = active_theme(cx).tokens().clone();
        let paused = form
            .pending
            .front()
            .is_some_and(|draft| draft.options.start_paused);
        // 排着多颗种子时：「取消」明确写成「全部取消」，并提供只跳过当前这颗的入口。
        let multiple = form.pending.len() > 1;
        h_flex()
            .w_full()
            .flex_none()
            .justify_end()
            .gap(tokens.spacing.sm)
            .child(
                Button::new("torrent-cancel")
                    .label(if multiple {
                        self.torrent_text("torrentConfirmCancelAll", cx)
                    } else {
                        self.strings.cancel.clone()
                    })
                    .control(cx)
                    .disabled(form.submitting)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.cancel_torrent_batch(window, cx);
                    })),
            )
            .children(multiple.then(|| {
                Button::new("torrent-skip")
                    .label(self.torrent_text("torrentConfirmSkip", cx))
                    .control(cx)
                    .disabled(form.submitting)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.skip_torrent(window, cx);
                    }))
            }))
            .children((!paused).then(|| {
                Button::new("torrent-later")
                    .label(self.strings.download_later.clone())
                    .control(cx)
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.confirm_torrent(true, None, window, cx)
                    }))
            }))
            .child(
                Button::new("torrent-start")
                    .primary()
                    .label(if paused {
                        self.strings.download_later.clone()
                    } else {
                        self.torrent_text("startDownload", cx)
                    })
                    .control(cx)
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let later = this
                            .torrent
                            .as_ref()
                            .and_then(|form| form.pending.front())
                            .is_some_and(|draft| draft.options.start_paused);
                        this.confirm_torrent(later, None, window, cx)
                    })),
            )
    }
}

#[cfg(test)]
mod tests;
