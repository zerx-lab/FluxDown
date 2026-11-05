//! 任务详情常规页的「文件哈希」行：已完成本地任务展示 `algo=hex`，可换算法重算。
//!
//! 布局：标签位是算法下拉（`SHA-256 ▾`，即摘要的标签），值位是 `摘要（中间省略，悬浮看全文）
//! [✓ / ✕] [复制]`。打开详情时尚无任何记录的任务短暂停留后自动计算（停留门槛避免方向键
//! 浏览列表时逐行读盘）；切到尚无记录的算法同样即时计算。自动计算失败不弹通知，值位回落为
//! 「计算」按钮，手动重试时再提示原因。
//!
//! 计算走视图自己的 [`DownloadsPort`]（停靠面板与独立窗口同一条路径），手动发起的失败通知推到
//! 发起时的窗口。成功不弹通知、也不回写：行内哈希只来自任务行（daemon 写回后发布 `TaskChanged`）。

use std::{collections::HashSet, time::Duration};

use fluxdown_protocol::{ApplicationErrorCode, ErrorReason, HASH_ALGORITHMS, RpcErrorData};
use fluxdown_ui_components::{
    ControlExt as _, FluxIcon, IconControlExt as _, tabular_numbers, tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, AnyWindowHandle, App, ClipboardItem, Context, Global,
    InteractiveElement as _, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, Icon, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
};

use crate::{
    controller::{DownloadsCommand, DownloadsResult},
    model::{
        DownloadTaskView, RowKey, TaskSource, TaskState,
        file_hash::{
            DEFAULT_HASH_ALGORITHM, file_hash_algorithm, file_hash_matches_checksum,
            hash_algorithm_label,
        },
    },
    strings::error_text,
};

use super::{TaskDetailView, detail_row};

/// 本进程在途的文件哈希请求（任务 id）。放在全局而不是视图里：停靠面板切换任务再切回、
/// 停靠面板与独立任务窗口之间都能看到同一任务仍在计算（与 Web 模块级在途集合同义）。
/// 只记本进程发起的请求，daemon 自发的「下载完成后计算」不在其中。
#[derive(Default)]
struct FileHashInflight(HashSet<String>);

impl Global for FileHashInflight {}

fn file_hash_inflight(cx: &App, task_id: &str) -> bool {
    cx.try_global::<FileHashInflight>()
        .is_some_and(|inflight| inflight.0.contains(task_id))
}

impl TaskDetailView {
    /// 在途集合变化时重绘（渲染读全局，须由观察者通知）。
    pub(super) fn observe_file_hash_inflight(cx: &mut Context<Self>) {
        cx.observe_global::<FileHashInflight>(|_, cx| cx.notify())
            .detach();
    }

    /// 任务切换或行内 `fileHash` 算法变化时重置算法选择；随后检查是否需要「打开即计算」。
    pub(super) fn sync_file_hash(&mut self, cx: &mut Context<Self>) {
        let stored = self.stored_file_hash();
        let task_changed = self.file_hash_task.as_deref() != Some(self.task_id.as_str());
        if task_changed {
            self.file_hash_copied = 0;
        }
        let noted = self.file_hash_noted.as_deref().unwrap_or("");
        if task_changed || file_hash_algorithm(noted) != file_hash_algorithm(&stored) {
            self.file_hash_algorithm = file_hash_algorithm(&stored)
                .unwrap_or(DEFAULT_HASH_ALGORITHM)
                .to_owned();
        }
        if self.file_hash_noted.as_deref() != Some(stored.as_str()) {
            self.file_hash_noted = Some(stored);
        }
        if task_changed {
            self.file_hash_task = Some(self.task_id.clone());
        }
        self.schedule_auto_file_hash(cx);
    }

    /// 打开即计算：可计算且尚无任何记录的任务，停留 [`AUTO_FILE_HASH_DELAY`] 后仍是当前任务
    /// 才发起。每次打开只排一次（打开时还没完成的任务在完成那一刻排）。等待期间行内已显示
    /// 「计算中」，不先闪一下「计算」按钮。
    fn schedule_auto_file_hash(&mut self, cx: &mut Context<Self>) {
        if file_hash_inflight(cx, &self.task_id)
            || self.file_hash_auto.as_deref() == Some(&self.task_id)
        {
            return;
        }
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return;
        };
        let eligible = file_hash_computable(&row) && row.file_hash.is_empty();
        drop(row);
        if !eligible {
            return;
        }
        self.file_hash_auto = Some(self.task_id.clone());
        self.file_hash_pending = true;
        let task_id = self.task_id.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTO_FILE_HASH_DELAY).await;
            let Ok(()) = this.update(cx, |this, cx| {
                if this.task_id != task_id {
                    return;
                }
                this.file_hash_pending = false;
                if this.stored_file_hash().is_empty() {
                    this.compute_file_hash(None, cx);
                }
                cx.notify();
            }) else {
                // 视图已释放（详情已关闭）：不再计算。
                return;
            };
        })
        .detach();
    }

    pub(super) fn render_file_hash_row(
        &self,
        row: &DownloadTaskView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        // 打开即计算的停留期同样显示「计算中」。
        let inflight = self.file_hash_pending || file_hash_inflight(cx, &self.task_id);
        // 只在下拉选中的算法就是记录的算法时显示摘要：下拉同时充当摘要的标签。
        let recorded = row.file_hash.as_str();
        let digest = (file_hash_algorithm(recorded) == Some(self.file_hash_algorithm.as_str()))
            .then(|| recorded.split_once('=').map(|(_, hex)| hex.trim()))
            .flatten()
            .filter(|hex| !hex.is_empty());

        let label = self.file_hash_algorithm_menu(inflight || row.file_missing, cx);
        let value = if inflight {
            div()
                .text_color(tokens.colors.muted_foreground)
                .child(self.t(cx, "fileHashComputing"))
                .into_any_element()
        } else if let Some(hex) = digest {
            let checksum = self
                .current_dto()
                .map(|dto| dto.checksum.as_str())
                .unwrap_or("");
            let matched = file_hash_matches_checksum(recorded, checksum);
            let full = SharedString::from(hex.to_owned());
            let tip = full.clone();
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap(tokens.spacing.sm)
                .child(
                    div()
                        .id("detail-file-hash-digest")
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_middle()
                        .font_family(tokens.typography.mono.clone())
                        .font_features(tabular_numbers())
                        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
                        .child(full.clone()),
                )
                .when_some(matched, |this, matched| {
                    let (icon, color, tip) = if matched {
                        (
                            FluxIcon::CircleCheck,
                            extended.colors.success,
                            self.t(cx, "fileHashMatch"),
                        )
                    } else {
                        (
                            FluxIcon::CircleAlert,
                            tokens.colors.destructive,
                            self.t(cx, "fileHashMismatch"),
                        )
                    };
                    this.child(
                        div()
                            .id("detail-file-hash-match")
                            .flex_none()
                            .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
                            .child(Icon::new(icon).size(extended.icon.sm).text_color(color)),
                    )
                })
                .child(self.file_hash_copy_button(full, cx))
                .into_any_element()
        } else if row.file_missing {
            div()
                .text_color(tokens.colors.muted_foreground)
                .child(self.t(cx, "fileHashNotComputed"))
                .into_any_element()
        } else {
            Button::new("detail-file-hash-compute")
                .outline()
                .control(cx)
                .label(self.t(cx, "fileHashCompute"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.compute_file_hash(Some(window.window_handle()), cx);
                }))
                .into_any_element()
        };
        detail_row(label, value, cx).into_any_element()
    }

    fn file_hash_copy_button(&self, hex: SharedString, cx: &mut Context<Self>) -> Button {
        let copied = self.file_hash_copied != 0;
        let success = active_theme(cx).extended().colors.success;
        let tooltip = self.t(
            cx,
            if copied {
                "fileHashCopied"
            } else {
                "fileHashCopy"
            },
        );
        Button::new("detail-file-hash-copy")
            .ghost()
            .control_icon(cx)
            .icon(if copied {
                FluxIcon::Check
            } else {
                FluxIcon::Copy
            })
            .when(copied, |this| this.text_color(success))
            .tooltip(tooltip)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.copy_file_hash(&hex, cx);
            }))
    }

    /// 标签位的算法下拉：ghost 样式、三级文字色，左缘用负外边距抵消内边距，与其它行标签文字
    /// 对齐。计算中或文件已丢失时只读；切到尚无记录的算法即开始计算（只存一份哈希，切回
    /// 原算法同样重算）。
    fn file_hash_algorithm_menu(&self, disabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let pad = theme.tokens().spacing.xs;
        let tertiary = theme.extended().colors.text_tertiary;
        let current = self.file_hash_algorithm.clone();
        let label = SharedString::from(hash_algorithm_label(&current));
        let this = cx.weak_entity();
        let options: Vec<(SharedString, SharedString)> = HASH_ALGORITHMS
            .iter()
            .map(|algo| {
                (
                    SharedString::from(*algo),
                    SharedString::from(hash_algorithm_label(algo)),
                )
            })
            .collect();
        Button::new("detail-file-hash-algorithm")
            .ghost()
            .control(cx)
            .px(pad)
            .ml(-pad)
            .text_color(tertiary)
            .tooltip(self.t(cx, "hashAlgorithm"))
            .disabled(disabled)
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                options.iter().fold(menu, |menu, (value, label)| {
                    let checked = value.as_ref() == current.as_str();
                    let this = this.clone();
                    let value = value.clone();
                    menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(
                        move |_, window, cx| {
                            let Ok(()) = this.update(cx, |this, cx| {
                                if this.file_hash_algorithm.as_str() == value.as_ref() {
                                    return;
                                }
                                this.file_hash_algorithm = value.to_string();
                                cx.notify();
                                let recorded = this.stored_file_hash();
                                if file_hash_algorithm(&recorded) != Some(value.as_ref()) {
                                    this.compute_file_hash(Some(window.window_handle()), cx);
                                }
                            }) else {
                                // 视图已释放，结束这次选择。
                                return;
                            };
                        },
                    ))
                })
            })
            .into_any_element()
    }

    fn copy_file_hash(&mut self, hex: &str, cx: &mut Context<Self>) {
        if hex.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(hex.to_owned()));
        self.flash_copied(cx, |this| &mut this.file_hash_copied);
    }

    /// 发起一次计算。`notify` = 失败通知推到的窗口；`None`（打开即计算）不弹通知，
    /// 失败后值位回落为「计算」按钮。
    fn compute_file_hash(&mut self, notify: Option<AnyWindowHandle>, cx: &mut Context<Self>) {
        if file_hash_inflight(cx, &self.task_id) {
            return;
        }
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return;
        };
        if !file_hash_computable(&row) {
            return;
        }
        let task_id = self.task_id.clone();
        let name = row.name.clone();
        drop(row);
        let algorithm = if HASH_ALGORITHMS.contains(&self.file_hash_algorithm.as_str()) {
            self.file_hash_algorithm.clone()
        } else {
            DEFAULT_HASH_ALGORITHM.to_owned()
        };
        cx.default_global::<FileHashInflight>()
            .0
            .insert(task_id.clone());

        let future = self.port.execute(DownloadsCommand::HashTask {
            task_id: task_id.clone(),
            algorithm,
        });
        let translator = self.translator.clone();
        cx.spawn(async move |_, cx| {
            let result = future.await;
            // 成功不回写：daemon 写回后先发布任务行（TaskChanged）再回执，行内哈希随之更新。
            // 视图已释放（详情已关闭）也要撤掉在途标记，其它详情视图随全局通知重绘。
            cx.update(|cx| {
                cx.default_global::<FileHashInflight>().0.remove(&task_id);
            });
            let Some(handle) = notify else {
                return;
            };
            if let Some(notification) = hash_notification(&translator, &name, &result, cx)
                && let Err(error) = handle.update(cx, |_, window, cx| {
                    window.push_notification(notification, cx);
                })
            {
                eprintln!("window closed before the file hash result: {error:#}");
            }
        })
        .detach();
    }

    fn stored_file_hash(&self) -> String {
        self.store
            .get(&RowKey::Local(self.task_id.clone()))
            .map(|row| row.file_hash.clone())
            .unwrap_or_default()
    }
}

/// 打开详情后停留多久才自动计算：方向键浏览列表时每行停留远短于此，不会逐行读盘。
const AUTO_FILE_HASH_DELAY: Duration = Duration::from_millis(600);

/// 可计算哈希：本机引擎、已完成、文件仍在。
fn file_hash_computable(row: &DownloadTaskView) -> bool {
    row.source == TaskSource::Local && row.state == TaskState::Completed && !row.file_missing
}

fn hash_notification(
    translator: &gpui::Entity<Translator>,
    name: &str,
    result: &Result<DownloadsResult, RpcErrorData>,
    cx: &gpui::AsyncApp,
) -> Option<Notification> {
    let failed = match result {
        Ok(DownloadsResult::TaskHash(_)) => return None,
        Ok(_) => RpcErrorData::new(ApplicationErrorCode::Internal, false),
        Err(error) => error.clone(),
    };
    Some(translator.read_with(cx, |translator, _| {
        hash_failure_notice(translator, name, &failed)
    }))
}

fn hash_failure_notice(translator: &Translator, name: &str, error: &RpcErrorData) -> Notification {
    if error.code == ApplicationErrorCode::Conflict {
        if error.reason == Some(ErrorReason::TaskStateConflict) {
            return Notification::error(translator.text_with("fileHashChanged", &[("name", name)]));
        }
        if error.reason.is_none() {
            return Notification::warning(translator.text_with("fileHashBusy", &[("name", name)]));
        }
    }
    let detail = error_text(translator, error);
    Notification::error(translator.text_with(
        "fileHashFailed",
        &[("name", name), ("error", detail.as_str())],
    ))
}
