//! 任务详情「文件」页签（BT）：文件表（名称 / 大小 / 进度 / 优先级）、多选批量改优先级、
//! 右键菜单（优先级子菜单 / 重命名）。
//!
//! 轮询生命周期与 Tracker / 连接页一致（见 `task_detail_bt.rs`）：页签渲染时登记可见并启动
//! 2 s 轮询，不可见超过宽限期即停；任务切换作废旧循环；失败保留旧数据并在页内提示一行。

use std::{collections::BTreeSet, ops::Range, rc::Rc, time::Instant};

use fluxdown_protocol::{
    BtFilePriorityDto, BtFilesDto, RenameBtFileParams, RpcErrorData, SetTaskFilePrioritiesParams,
};
use fluxdown_ui_components::{
    ControlExt as _, DialogIntent, dialog_footer, dialog_title, form, form_field, tabular_numbers,
    tooltip_view,
};
use gpui::{
    Anchor, AnyElement, App, AppContext as _, ClickEvent, Context, InteractiveElement as _,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement as _, Styled, WeakEntity,
    Window, div, prelude::FluentBuilder as _, px, relative, uniform_list,
};
use gpui_component::{
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    v_flex,
};

use super::{
    DetailTab, TaskDetailView,
    bt::{
        BT_POLL_INTERVAL, BT_VISIBLE_GRACE, Look, empty_state, fixed, header_cell, header_row,
        note, skeleton_rows, table_frame,
    },
};
use crate::{
    controller::{DownloadsCommand, DownloadsResult, PortFuture},
    model::{
        bt_files::{
            FileSelection, PriorityLevel, TargetLevel, apply_changes, leaves_nothing_to_download,
            priority_changes, progress_fraction, progress_percent_text, split_path, target_level,
        },
        format_bytes,
    },
    strings::error_text,
};

// 列宽（界面缩放前的 px）。
const COL_SIZE: f32 = 84.;
const COL_PROGRESS: f32 = 140.;
const COL_PRIORITY: f32 = 128.;
/// 进度列里百分比文字的宽度。
const PROGRESS_TEXT: f32 = 48.;

/// 页签的轮询状态、最近一次成功数据与选区（随任务切换整体重置）。
#[derive(Default)]
pub(super) struct BtFilesLive {
    data: Option<BtFilesDto>,
    /// 最近一次请求失败：保留旧数据，只在页内显示一行提示。
    failed: bool,
    /// 轮询循环存活中。
    polling: bool,
    /// 任务切换代次：旧循环 / 旧响应据此作废。
    generation: u64,
    /// 本地改动代次：乐观更新后，改动前发出的轮询响应作废（避免把旧优先级刷回来）。
    epoch: u64,
    last_render: Option<Instant>,
    selection: FileSelection,
}

impl BtFilesLive {
    /// 切换任务：丢弃数据与选区并作废在途循环。
    pub(super) fn reset(&mut self) {
        self.data = None;
        self.failed = false;
        self.polling = false;
        self.generation = self.generation.wrapping_add(1);
        self.epoch = self.epoch.wrapping_add(1);
        self.last_render = None;
        self.selection.clear();
    }
}

/// 四档优先级的本地化文案（按 [`PriorityLevel::ALL`] 顺序）与「混合」。
struct PriorityLabels {
    levels: [SharedString; 4],
    mixed: SharedString,
}

impl PriorityLabels {
    fn of(&self, level: PriorityLevel) -> SharedString {
        let at = PriorityLevel::ALL
            .iter()
            .position(|item| *item == level)
            .unwrap_or(1);
        self.levels.get(at).cloned().unwrap_or_default()
    }
}

/// 行渲染共用的取值（每次渲染构造一次）。
struct FileRowText {
    look: Look,
    labels: Rc<PriorityLabels>,
    rename: SharedString,
    priority_menu: SharedString,
    editable: bool,
    view: WeakEntity<TaskDetailView>,
}

/// 优先级下拉 / 菜单项：把 `level` 应用到 `targets`。
fn apply_level(
    view: &WeakEntity<TaskDetailView>,
    targets: BTreeSet<u32>,
    level: PriorityLevel,
    window: &mut Window,
    cx: &mut App,
) {
    let Ok(()) = view.update(cx, |this, cx| {
        this.bt_files_set_priority(&targets, level, window, cx);
    }) else {
        // 视图已释放。
        return;
    };
}

/// 优先级下拉按钮：`current` 为 `None` 时显示「混合」（只用于级别不一致的多选）。
fn priority_dropdown(
    id: impl Into<gpui::ElementId>,
    labels: &Rc<PriorityLabels>,
    current: Option<PriorityLevel>,
    targets: BTreeSet<u32>,
    view: WeakEntity<TaskDetailView>,
    cx: &App,
) -> AnyElement {
    let label = current.map_or_else(|| labels.mixed.clone(), |level| labels.of(level));
    let labels = Rc::clone(labels);
    Button::new(id)
        .ghost()
        .control(cx)
        .label(label)
        .dropdown_caret(true)
        .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
            PriorityLevel::ALL.iter().fold(menu, |menu, level| {
                let (level, view, targets) = (*level, view.clone(), targets.clone());
                menu.item(
                    PopupMenuItem::new(labels.of(level))
                        .checked(current == Some(level))
                        .on_click(move |_, window, cx| {
                            apply_level(&view, targets.clone(), level, window, cx);
                        }),
                )
            })
        })
        .into_any_element()
}

impl TaskDetailView {
    /// 页签渲染时调用：登记「仍可见」并在没有轮询循环时启动一个。
    pub(super) fn bt_files_touch(&mut self, cx: &mut Context<Self>) {
        self.bt_files.last_render = Some(Instant::now());
        if self.bt_files.polling || self.closed || !self.activity_online || !self.is_bt() {
            return;
        }
        self.bt_files.polling = true;
        let generation = self.bt_files.generation;
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(request) = this.update(cx, |this, _| this.bt_files_poll_request(generation))
                else {
                    // 视图已释放。
                    return;
                };
                let Some((future, epoch)) = request else {
                    return;
                };
                let result = future.await;
                let Ok(keep_going) = this.update(cx, |this, cx| {
                    this.bt_files_poll_finish(generation, epoch, result, cx)
                }) else {
                    // 视图已释放。
                    return;
                };
                if !keep_going {
                    return;
                }
                cx.background_executor().timer(BT_POLL_INTERVAL).await;
            }
        })
        .detach();
    }

    fn bt_files_command(&self) -> DownloadsCommand {
        DownloadsCommand::BtFiles {
            task_id: self.task_id.clone(),
        }
    }

    /// 发出下一次请求（附本地改动代次）；不再需要轮询时返回 `None` 并让循环退出。
    fn bt_files_poll_request(
        &mut self,
        generation: u64,
    ) -> Option<(PortFuture<DownloadsResult>, u64)> {
        if self.bt_files.generation != generation {
            // 任务已切换：新代次的循环归新代次管理，不动 `polling`。
            return None;
        }
        let visible = self
            .bt_files
            .last_render
            .is_some_and(|at| at.elapsed() < BT_VISIBLE_GRACE);
        if self.closed
            || !self.activity_online
            || self.tab != DetailTab::Files
            || !visible
            || !self.is_bt()
        {
            self.bt_files.polling = false;
            return None;
        }
        Some((
            self.port.execute(self.bt_files_command()),
            self.bt_files.epoch,
        ))
    }

    /// 应用一次响应；返回循环是否继续。失败时保留旧数据。
    fn bt_files_poll_finish(
        &mut self,
        generation: u64,
        epoch: u64,
        result: Result<DownloadsResult, RpcErrorData>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.bt_files.generation != generation {
            return false;
        }
        if self.bt_files.epoch != epoch {
            // 请求发出后本地改过优先级：这份响应可能是旧值，丢弃，等下一轮。
            return true;
        }
        match result {
            Ok(DownloadsResult::BtFiles(dto)) => {
                let present: Vec<u32> = dto.files.iter().map(|file| file.index).collect();
                self.bt_files.selection.prune(&present);
                self.bt_files.data = Some(dto);
                self.bt_files.failed = false;
            }
            Ok(_) | Err(_) => self.bt_files.failed = true,
        }
        cx.notify();
        true
    }

    /// 改动失败 / 重命名后立即补拉一次（不影响轮询循环）。
    fn bt_files_refetch(&mut self, cx: &mut Context<Self>) {
        if self.closed || !self.activity_online {
            return;
        }
        self.bt_files.epoch = self.bt_files.epoch.wrapping_add(1);
        let (generation, epoch) = (self.bt_files.generation, self.bt_files.epoch);
        let future = self.port.execute(self.bt_files_command());
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(_keep_going) = this.update(cx, |this, cx| {
                this.bt_files_poll_finish(generation, epoch, result, cx)
            }) else {
                // 视图已释放。
                return;
            };
        })
        .detach();
    }

    fn bt_files_push_error(&self, error: &RpcErrorData, window: &mut Window, cx: &mut App) {
        let text = error_text(self.translator.read(cx), error);
        window.push_notification(Notification::error(text), cx);
    }

    /// 把 `targets` 设为 `level`：只发有变化的条目；会导致没有文件可下载时拦下并提示。
    fn bt_files_set_priority(
        &mut self,
        targets: &BTreeSet<u32>,
        level: PriorityLevel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(data) = self.bt_files.data.as_mut() else {
            return;
        };
        if !data.editable {
            return;
        }
        let changes = priority_changes(&data.files, targets, level);
        if changes.is_empty() {
            return;
        }
        if leaves_nothing_to_download(&data.files, &changes) {
            let text = self.t(cx, "btFilesKeepOne");
            window.push_notification(Notification::warning(text), cx);
            return;
        }
        apply_changes(&mut data.files, &changes);
        self.bt_files.epoch = self.bt_files.epoch.wrapping_add(1);
        cx.notify();
        self.bt_files_send_priorities(changes, window, cx);
    }

    fn bt_files_send_priorities(
        &mut self,
        priorities: Vec<BtFilePriorityDto>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending = self.port.execute(DownloadsCommand::SetFilePriorities(
            SetTaskFilePrioritiesParams {
                task_id: self.task_id.clone(),
                priorities,
            },
        ));
        cx.spawn_in(window, async move |this, cx| {
            let Err(error) = pending.await else {
                // 成功：下一轮轮询带回服务端确认值。
                return;
            };
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.bt_files_push_error(&error, window, cx);
                this.bt_files_refetch(cx);
            }) else {
                // 视图或窗口已释放。
                return;
            };
        })
        .detach();
    }

    fn bt_files_click(&mut self, index: u32, event: &ClickEvent, cx: &mut Context<Self>) {
        let Some(data) = self.bt_files.data.as_ref() else {
            return;
        };
        let order: Vec<u32> = data.files.iter().map(|file| file.index).collect();
        let modifiers = event.modifiers();
        self.bt_files
            .selection
            .click(&order, index, modifiers.shift, modifiers.secondary(cx));
        cx.notify();
    }

    /// 「重命名」对话框：输入框预填当前相对路径，确认后下发 `RenameBtFile`。
    fn bt_files_open_rename(&mut self, index: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.bt_files.data.as_ref() else {
            return;
        };
        if !data.editable {
            return;
        }
        let Some(current) = data
            .files
            .iter()
            .find(|file| file.index == index)
            .map(|file| file.path.clone())
        else {
            return;
        };
        let title = self.t(cx, "btFileRenameTitle");
        let label = self.t(cx, "btFileColumnName");
        let hint = self.t(cx, "btFileRenameHint");
        let ok_label = self.strings.confirm.clone();
        let cancel_label = self.strings.cancel.clone();
        let task_id = self.task_id.clone();
        let view = cx.weak_entity();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current.clone()));
        let dialog_input = input.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let content_input = dialog_input.clone();
            let ok_input = dialog_input.clone();
            let (label, hint) = (label.clone(), hint.clone());
            let (view, task_id, current) = (view.clone(), task_id.clone(), current.clone());
            dialog
                .title(dialog_title(title.clone(), cx))
                .w(px(520.))
                .content(move |content, _, cx| {
                    content.child(form(cx).child(form_field(
                        label.clone(),
                        Input::new(&content_input).control(cx).w_full(),
                        Some(hint.clone()),
                        cx,
                    )))
                })
                .footer(dialog_footer(
                    Some(cancel_label.clone()),
                    ok_label.clone(),
                    DialogIntent::Confirm,
                    cx,
                ))
                .on_ok(move |_, window, cx| {
                    let path = ok_input.read(cx).value().to_string();
                    if path.trim().is_empty() {
                        return false;
                    }
                    if path == current {
                        return true;
                    }
                    let params = RenameBtFileParams {
                        task_id: task_id.clone(),
                        index,
                        path,
                    };
                    let Ok(()) = view.update(cx, |this, cx| {
                        this.bt_files_rename(params, window, cx);
                    }) else {
                        // 视图已释放：关闭对话框即可。
                        return true;
                    };
                    true
                })
        });
        input.update(cx, |state, cx| state.focus(window, cx));
    }

    fn bt_files_rename(
        &mut self,
        params: RenameBtFileParams,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending = self.port.execute(DownloadsCommand::RenameBtFile(params));
        cx.spawn_in(window, async move |this, cx| {
            let result = pending.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if let Err(error) = &result {
                    this.bt_files_push_error(error, window, cx);
                }
                this.bt_files_refetch(cx);
            }) else {
                // 视图或窗口已释放。
                return;
            };
        })
        .detach();
    }

    pub(super) fn render_bt_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let look = Look::new(cx);
        let failed = self
            .bt_files
            .failed
            .then(|| note(&look, self.strings.action_failed.clone(), look.warning));
        let Some(data) = self.bt_files.data.as_ref() else {
            let content = if self.bt_files.failed {
                div().into_any_element()
            } else {
                skeleton_rows(&look, 3).into_any_element()
            };
            return v_flex()
                .w_full()
                .gap(look.sm)
                .children(failed)
                .child(content)
                .into_any_element();
        };
        if data.files.is_empty() {
            return v_flex()
                .w_full()
                .gap(look.sm)
                .children(failed)
                .child(empty_state(&look, self.t(cx, "btFilesEmpty")))
                .into_any_element();
        }
        let labels = Rc::new(PriorityLabels {
            levels: PriorityLevel::ALL.map(|level| self.t(cx, level.label_key())),
            mixed: self.t(cx, "btFilePriorityMixed"),
        });
        let view = cx.weak_entity();
        let selected = self.bt_files.selection.selected();
        // 批量下拉只在有选区时出现：无选区时没有可显示的级别，「混合」只属于级别不一致的多选。
        let toolbar = (data.editable && !selected.is_empty()).then(|| {
            let current = match target_level(&data.files, selected) {
                TargetLevel::Same(level) => Some(level),
                TargetLevel::Mixed | TargetLevel::None => None,
            };
            let batch = priority_dropdown(
                "bt-files-batch-priority",
                &labels,
                current,
                selected.clone(),
                view.clone(),
                cx,
            );
            h_flex()
                .items_center()
                .gap(look.sm)
                .child(
                    div()
                        .text_size(look.xs_size)
                        .line_height(look.xs_line)
                        .text_color(look.muted)
                        .child(self.t(cx, "btFileColumnPriority")),
                )
                .child(batch)
        });
        let header = header_row(
            &look,
            vec![
                header_cell(&look, self.t(cx, "btFileColumnName"), None, false),
                header_cell(&look, self.t(cx, "btFileColumnSize"), Some(COL_SIZE), true),
                header_cell(
                    &look,
                    self.t(cx, "btFileColumnProgress"),
                    Some(COL_PROGRESS),
                    false,
                ),
                header_cell(
                    &look,
                    self.t(cx, "btFileColumnPriority"),
                    Some(COL_PRIORITY),
                    false,
                ),
            ],
        );
        let text = Rc::new(FileRowText {
            look: look.clone(),
            labels,
            rename: self.t(cx, "btFileRename"),
            priority_menu: self.t(cx, "btFileColumnPriority"),
            editable: data.editable,
            view,
        });
        let rows = uniform_list(
            "bt-files-rows",
            data.files.len(),
            cx.processor(move |this, range: Range<usize>, _window, cx| {
                range
                    .map(|position| this.bt_file_row(position, &text, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .w_full();
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .gap(look.sm)
            .children(failed)
            .children(toolbar)
            .child(
                table_frame(&look)
                    .flex_1()
                    .min_h_0()
                    .child(header)
                    .child(rows),
            )
            .into_any_element()
    }

    /// 虚拟列表的第 `position` 行。
    fn bt_file_row(&self, position: usize, text: &FileRowText, cx: &App) -> AnyElement {
        let Some(file) = self
            .bt_files
            .data
            .as_ref()
            .and_then(|data| data.files.get(position))
        else {
            return div().into_any_element();
        };
        let look = &text.look;
        let index = file.index;
        let selected = self.bt_files.selection.contains(index);
        let (dir, name) = split_path(&file.path);
        let full_path = SharedString::from(file.path.clone());
        let fraction = progress_fraction(file.downloaded, file.size);
        let level = PriorityLevel::of(file.priority);

        let name_cell = h_flex()
            .id(("bt-file-name", position))
            .flex_1()
            .min_w_0()
            .gap(look.xs)
            .whitespace_nowrap()
            .child(
                div()
                    .min_w_0()
                    .max_w(relative(0.75))
                    .flex_shrink(1.)
                    .overflow_hidden()
                    .text_ellipsis_middle()
                    .text_color(look.fg)
                    .child(SharedString::from(name.to_owned())),
            )
            .when(!dir.is_empty(), |this| {
                this.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis_middle()
                        .text_color(look.tertiary)
                        .child(SharedString::from(dir.to_owned())),
                )
            })
            .tooltip(move |window, cx| tooltip_view(full_path.clone(), window, cx));
        let size_cell = fixed(look, COL_SIZE)
            .text_right()
            .font_features(tabular_numbers())
            .text_color(look.muted)
            .child(SharedString::from(format_bytes(file.size)));
        let bar_color = if fraction >= 1. {
            look.success
        } else {
            look.accent
        };
        let progress_cell = h_flex()
            .flex_none()
            .w(look.ext(COL_PROGRESS))
            .items_center()
            .gap(look.xs)
            .child(
                div()
                    .flex_1()
                    .h(look.ext(4.))
                    .rounded(look.radius)
                    .bg(look.hairline)
                    .overflow_hidden()
                    .child(div().h_full().w(relative(fraction as f32)).bg(bar_color)),
            )
            .child(
                fixed(look, PROGRESS_TEXT)
                    .text_right()
                    .font_features(tabular_numbers())
                    .text_color(look.muted)
                    .child(SharedString::from(progress_percent_text(fraction))),
            );
        let priority_cell = if text.editable {
            fixed(look, COL_PRIORITY).child(priority_dropdown(
                ("bt-file-priority", position),
                &text.labels,
                Some(level),
                BTreeSet::from([index]),
                text.view.clone(),
                cx,
            ))
        } else {
            fixed(look, COL_PRIORITY)
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(if level == PriorityLevel::Skip {
                    look.tertiary
                } else {
                    look.muted
                })
                .child(text.labels.of(level))
        };
        let view = text.view.clone();
        let row = h_flex()
            .id(("bt-file-row", position))
            .w_full()
            .items_center()
            .gap(look.sm)
            .px(look.sm)
            .py(look.xs)
            .text_size(look.sm_size)
            .line_height(look.sm_line)
            .border_b(look.thin)
            .border_color(look.hairline)
            .when(selected, |this| this.bg(look.accent.opacity(0.12)))
            .when(!selected, |this| this.hover(|this| this.bg(look.hover)))
            .on_click(move |event, _, cx| {
                let Ok(()) = view.update(cx, |this, cx| this.bt_files_click(index, event, cx))
                else {
                    // 视图已释放。
                    return;
                };
            })
            .child(name_cell)
            .child(size_cell)
            .child(progress_cell)
            .child(priority_cell);
        if !text.editable {
            return row.into_any_element();
        }
        let labels = Rc::clone(&text.labels);
        let (rename, priority_menu) = (text.rename.clone(), text.priority_menu.clone());
        let view = text.view.clone();
        row.context_menu(move |menu, window, cx| {
            let Some(entity) = view.upgrade() else {
                return menu;
            };
            let (targets, current) = {
                let this = entity.read(cx);
                let targets = this.bt_files.selection.targets_for(index);
                let current = this
                    .bt_files
                    .data
                    .as_ref()
                    .map_or(TargetLevel::None, |data| {
                        target_level(&data.files, &targets)
                    });
                (targets, current)
            };
            let single = targets.len() == 1;
            let (submenu_labels, submenu_view, submenu_targets) =
                (Rc::clone(&labels), view.clone(), targets.clone());
            let menu = menu.submenu(priority_menu.clone(), window, cx, move |menu, _, _| {
                PriorityLevel::ALL.iter().fold(menu, |menu, level| {
                    let (level, view, targets) =
                        (*level, submenu_view.clone(), submenu_targets.clone());
                    menu.item(
                        PopupMenuItem::new(submenu_labels.of(level))
                            .checked(current == TargetLevel::Same(level))
                            .on_click(move |_, window, cx| {
                                apply_level(&view, targets.clone(), level, window, cx);
                            }),
                    )
                })
            });
            if !single {
                return menu;
            }
            let view = view.clone();
            menu.separator()
                .item(
                    PopupMenuItem::new(rename.clone()).on_click(move |_, window, cx| {
                        let Ok(()) = view.update(cx, |this, cx| {
                            this.bt_files_open_rename(index, window, cx);
                        }) else {
                            // 视图已释放。
                            return;
                        };
                    }),
                )
        })
        .into_any_element()
    }
}
