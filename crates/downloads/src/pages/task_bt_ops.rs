//! 任务右键菜单的 BT 运维项：「强制重新校验」「移动到…」「任务限速…」「超级做种」。
//!
//! 可见性由 [`crate::components::task_table`] 的纯函数判定；处理器下发前按同一事实源再判。
//! 旧 daemon 不认识这些方法时返回的错误经常规错误通知展示。

use std::{rc::Rc, sync::Arc};

use fluxdown_protocol::{
    MoveTaskStorageParams, RpcErrorData, SetTaskBtLimitsParams, SetTaskSuperSeedingParams,
};
use fluxdown_ui_components::{
    ControlExt as _, DialogIntent, dialog_footer, dialog_title, field_hint, form, form_field,
};
use fluxdown_ui_i18n::Translator;
use gpui::{
    App, AppContext as _, Context, ElementId, Entity, IntoElement, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, Window, div, px,
};
use gpui_component::{
    WindowExt as _, h_flex,
    input::{Input, InputState},
    notification::Notification,
};

use crate::{
    actions::{
        ForceRecheckSelected, MoveStorageSelected, TaskLimitsSelected, ToggleSuperSeedingSelected,
    },
    components::task_table::{
        force_recheck_visible, move_storage_visible, super_seeding_targets,
        super_seeding_toggle_target, task_limits_visible, task_menu_facts_for,
    },
    controller::{DownloadsCommand, DownloadsPort},
    model::RowKey,
    pages::downloads::DownloadView,
    strings::error_text,
};

/// 通知里任务名 / 路径的字符上限（超出中间省略，保留开头与结尾——扩展名 / 末段目录）。
const NOTICE_TEXT_MAX_CHARS: usize = 80;

/// 「移动到…」进行中通知的标识类型（按任务 id 区分）。
struct MoveStorageNotice;

/// 通知文本按字符做中间省略：保留开头与结尾（扩展名 / 末段目录），不按字节切。
fn notice_text(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars || max_chars < 3 {
        return value.to_owned();
    }
    let keep = max_chars - 1;
    let tail = keep / 2;
    let head = keep - tail;
    let mut text: String = value.chars().take(head).collect();
    text.push('…');
    text.extend(value.chars().skip(count - tail));
    text
}

/// 字节/秒 → 对话框显示的 KiB/s：正值向上取整；0 与负值（跟随全局等哨兵）显示 0。
fn limit_kib_text(bps: i64) -> String {
    if bps > 0 {
        (bps.saturating_add(1023) / 1024).to_string()
    } else {
        "0".to_owned()
    }
}

/// KiB/s 输入 → 字节/秒：trim 后空 = 0；仅接受非负整数（溢出饱和到 `i64::MAX`）。
fn parse_limit_kib(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    if !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let kib = text.parse::<u64>().unwrap_or(u64::MAX);
    Some(i64::try_from(kib.saturating_mul(1024)).unwrap_or(i64::MAX))
}

/// 对话框提交 → `setBtLimits` 参数：只带与初值不同的字段；都没改为 `Ok(None)`（不下发）；
/// 任一输入非法为 `Err(())`（对话框保持打开）。比较以 KiB 文本解析结果为准，未改动的
/// 输入（包括负值哨兵显示成的 0）不会被写回。
fn bt_limits_params(
    task_id: &str,
    initial: [&str; 2],
    edited: [&str; 2],
) -> Result<Option<SetTaskBtLimitsParams>, ()> {
    let changed = |initial: &str, edited: &str| -> Result<Option<i64>, ()> {
        let next = parse_limit_kib(edited).ok_or(())?;
        Ok((parse_limit_kib(initial) != Some(next)).then_some(next))
    };
    let [initial_download, initial_upload] = initial;
    let [edited_download, edited_upload] = edited;
    let download_limit_bps = changed(initial_download, edited_download)?;
    let upload_limit_bps = changed(initial_upload, edited_upload)?;
    if download_limit_bps.is_none() && upload_limit_bps.is_none() {
        return Ok(None);
    }
    Ok(Some(SetTaskBtLimitsParams {
        task_id: task_id.to_owned(),
        download_limit_bps,
        upload_limit_bps,
    }))
}

/// 「任务限速」对话框内容：下载 / 上传两个 KiB/s 输入 + 说明。
struct LimitsForm {
    download: Entity<InputState>,
    upload: Entity<InputState>,
    initial: [String; 2],
    download_label: SharedString,
    upload_label: SharedString,
    unit: SharedString,
    hint: SharedString,
}

impl LimitsForm {
    fn new(
        labels: [SharedString; 4],
        initial: [String; 2],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = |value: &str| {
            let value = value.to_owned();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx)
                    .placeholder("0")
                    .validate(|text, _| text.chars().all(|c| c.is_ascii_digit()));
                state.set_value(value, window, cx);
                state
            })
        };
        let download = input(&initial[0]);
        let upload = input(&initial[1]);
        let [download_label, upload_label, unit, hint] = labels;
        Self {
            download,
            upload,
            initial,
            download_label,
            upload_label,
            unit,
            hint,
        }
    }

    /// 对话框标签：下载 / 上传 / 单位 / 说明。
    fn labels(translator: &Translator) -> [SharedString; 4] {
        [
            "btTaskLimitDownload",
            "btTaskLimitUpload",
            "btGuardUnitKibps",
            "btTaskLimitHint",
        ]
        .map(|key| SharedString::from(translator.text(key).to_owned()))
    }

    fn params(&self, task_id: &str, cx: &App) -> Result<Option<SetTaskBtLimitsParams>, ()> {
        let download = self.download.read(cx).value();
        let upload = self.upload.read(cx).value();
        bt_limits_params(
            task_id,
            [self.initial[0].as_str(), self.initial[1].as_str()],
            [download.as_ref(), upload.as_ref()],
        )
    }

    fn field(
        &self,
        label: SharedString,
        state: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        form_field(
            label,
            h_flex()
                .w_full()
                .gap(px(6.))
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(state).control(cx).w_full()),
                )
                .child(div().flex_shrink_0().child(self.unit.clone())),
            None,
            cx,
        )
    }
}

impl Render for LimitsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (download_state, upload_state) = (self.download.clone(), self.upload.clone());
        let download = self.field(self.download_label.clone(), &download_state, cx);
        let upload = self.field(self.upload_label.clone(), &upload_state, cx);
        form(cx)
            .child(download)
            .child(upload)
            .child(field_hint(self.hint.clone(), cx))
    }
}

impl DownloadView {
    /// 当前选区（键 + 菜单事实），与右键菜单同一事实源。
    fn bt_selection(
        &self,
        cx: &Context<Self>,
    ) -> Vec<(RowKey, crate::components::task_table::TaskMenuFacts)> {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let store = self.controller.store();
        selected
            .into_iter()
            .filter_map(|key| task_menu_facts_for(store, &key).map(|facts| (key, facts)))
            .collect()
    }

    /// 右键「强制重新校验」：每个任务一次 `RecheckTask`；全部结束后一次提示（成功取首个
    /// 任务名，失败取第一个错误）。
    pub(crate) fn on_force_recheck_selected(
        &mut self,
        _: &ForceRecheckSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.bt_selection(cx);
        let facts: Vec<_> = rows.iter().map(|(_, facts)| *facts).collect();
        if !force_recheck_visible(&facts) {
            return;
        }
        let store = self.controller.store();
        let first_name = rows
            .first()
            .and_then(|(key, _)| store.get(key).map(|row| row.name.clone()))
            .unwrap_or_default();
        let futures: Vec<_> = rows
            .iter()
            .map(|(key, _)| {
                self.controller.execute(DownloadsCommand::RecheckTask {
                    task_id: key.task_id().to_owned(),
                })
            })
            .collect();
        let translator = self.translator.clone();
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let mut first_error: Option<RpcErrorData> = None;
            for future in futures {
                if let Err(error) = future.await {
                    first_error.get_or_insert(error);
                }
            }
            let notification = translator.read_with(cx, |translator, _| match &first_error {
                None => Notification::info(translator.text_with(
                    "btForceRecheckStarted",
                    &[(
                        "name",
                        notice_text(&first_name, NOTICE_TEXT_MAX_CHARS).as_str(),
                    )],
                )),
                Some(error) => Notification::error(error_text(translator, error)),
            });
            if let Err(error) = handle.update(cx, |_, window, cx| {
                window.push_notification(notification, cx);
            }) {
                eprintln!("window closed before the force recheck result: {error:#}");
            }
        })
        .detach();
    }

    /// 右键「移动到…」：选目录后下发 `MoveStorage`（慢方法），进行中常驻提示；同一任务
    /// 移动完成前不再发起第二次。
    pub(crate) fn on_move_storage_selected(
        &mut self,
        _: &MoveStorageSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.bt_selection(cx);
        let facts: Vec<_> = rows.iter().map(|(_, facts)| *facts).collect();
        let ([(key, _)], true) = (rows.as_slice(), move_storage_visible(&facts)) else {
            return;
        };
        let task_id = key.task_id().to_owned();
        if self.bt_moving.contains(&task_id) {
            return;
        }
        let Some(name) = self.controller.store().get(key).map(|row| row.name.clone()) else {
            return;
        };
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.strings.bt_move_storage.clone()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match receiver.await {
                Ok(Ok(Some(paths))) => paths.first().map(|path| path.display().to_string()),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    eprintln!("move storage folder picker failed: {error:#}");
                    None
                }
                Err(error) => {
                    eprintln!("move storage folder picker dropped: {error:#}");
                    None
                }
            };
            let Some(save_dir) = picked else {
                return;
            };
            let Ok(Some(future)) = this.update_in(cx, |this, window, cx| {
                // 选择器打开期间可能已有另一次移动发起。
                if !this.bt_moving.insert(task_id.clone()) {
                    return None;
                }
                let name = notice_text(&name, NOTICE_TEXT_MAX_CHARS);
                let working = this
                    .translator
                    .read(cx)
                    .text_with("btMoveStorageWorking", &[("name", name.as_str())]);
                window.push_notification(
                    Notification::info(working)
                        .autohide(false)
                        .id1::<MoveStorageNotice>(ElementId::Name(SharedString::from(
                            task_id.clone(),
                        ))),
                    cx,
                );
                Some(this.controller.execute(DownloadsCommand::MoveStorage(
                    MoveTaskStorageParams {
                        task_id: task_id.clone(),
                        save_dir: save_dir.clone(),
                    },
                )))
            }) else {
                return;
            };
            let result = future.await;
            if let Err(error) = this.update_in(cx, |this, window, cx| {
                this.bt_moving.remove(&task_id);
                window.remove_notification1::<MoveStorageNotice>(
                    ElementId::Name(SharedString::from(task_id.clone())),
                    cx,
                );
                let translator = this.translator.read(cx);
                let notification = match &result {
                    Ok(_) => Notification::success(translator.text_with(
                        "btMoveStorageDone",
                        &[(
                            "path",
                            notice_text(&save_dir, NOTICE_TEXT_MAX_CHARS).as_str(),
                        )],
                    )),
                    Err(error) => Notification::error(error_text(translator, error)),
                };
                window.push_notification(notification, cx);
            }) {
                eprintln!("download view closed before the move storage result: {error:#}");
            }
        })
        .detach();
    }

    /// 右键「任务限速…」：两个 KiB/s 输入，确认后只下发改动的字段。
    pub(crate) fn on_task_limits_selected(
        &mut self,
        _: &TaskLimitsSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.bt_selection(cx);
        let facts: Vec<_> = rows.iter().map(|(_, facts)| *facts).collect();
        let ([(key, _)], true) = (rows.as_slice(), task_limits_visible(&facts)) else {
            return;
        };
        let Some(initial) = self.controller.store().get(key).map(|row| {
            [
                limit_kib_text(row.bt_download_limit_bps),
                limit_kib_text(row.seed_upload_limit_bps),
            ]
        }) else {
            return;
        };
        let task_id = key.task_id().to_owned();
        let translator = self.translator.clone();
        let title = SharedString::from(translator.read(cx).text("btTaskLimitsTitle").to_owned());
        let ok_label = self.strings.confirm.clone();
        let cancel_label = self.strings.cancel.clone();
        let labels = LimitsForm::labels(translator.read(cx));
        let form_entity = cx.new(|cx| LimitsForm::new(labels, initial, window, cx));
        let dialog_form = form_entity.clone();
        let port: Arc<dyn DownloadsPort> = Arc::clone(&self.port);
        let task_id = Rc::new(task_id);
        window.open_dialog(cx, move |dialog, _, cx| {
            let content_form = dialog_form.clone();
            let ok_form = dialog_form.clone();
            let port = Arc::clone(&port);
            let task_id = Rc::clone(&task_id);
            let translator = translator.clone();
            dialog
                .title(dialog_title(title.clone(), cx))
                .w(px(440.))
                .content(move |content, _, _| content.child(content_form.clone()))
                .footer(dialog_footer(
                    Some(cancel_label.clone()),
                    ok_label.clone(),
                    DialogIntent::Confirm,
                    cx,
                ))
                .on_ok(move |_, window, cx| {
                    let params = match ok_form.read(cx).params(&task_id, cx) {
                        Err(()) => return false,
                        Ok(None) => return true,
                        Ok(Some(params)) => params,
                    };
                    let future = port.execute(DownloadsCommand::SetBtLimits(params));
                    let translator = translator.clone();
                    let handle = window.window_handle();
                    cx.spawn(async move |cx| {
                        let Err(error) = future.await else {
                            return;
                        };
                        let notification = translator.read_with(cx, |translator, _| {
                            Notification::error(error_text(translator, &error))
                        });
                        if let Err(error) = handle.update(cx, |_, window, cx| {
                            window.push_notification(notification, cx);
                        }) {
                            eprintln!("window closed before the task limits result: {error:#}");
                        }
                    })
                    .detach();
                    true
                })
        });
        let input = form_entity.read(cx).download.clone();
        input.update(cx, |state, cx| state.focus(window, cx));
    }

    /// 右键「超级做种」勾选：写入 `!全部已开`，只对值不同的任务下发。
    pub(crate) fn on_toggle_super_seeding_selected(
        &mut self,
        _: &ToggleSuperSeedingSelected,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.bt_selection(cx);
        let facts: Vec<_> = rows.iter().map(|(_, facts)| *facts).collect();
        let Some(enabled) = super_seeding_toggle_target(&facts) else {
            return;
        };
        let commands = super_seeding_targets(&facts, enabled)
            .into_iter()
            .filter_map(|index| rows.get(index))
            .map(|(key, _)| {
                DownloadsCommand::SetSuperSeeding(SetTaskSuperSeedingParams {
                    task_id: key.task_id().to_owned(),
                    enabled,
                })
            })
            .collect();
        self.execute_commands(commands, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{bt_limits_params, limit_kib_text, notice_text, parse_limit_kib};

    #[test]
    fn limit_display_rounds_up_and_hides_sentinels() {
        assert_eq!(limit_kib_text(0), "0");
        assert_eq!(limit_kib_text(-1), "0");
        assert_eq!(limit_kib_text(-2), "0");
        assert_eq!(limit_kib_text(1), "1");
        assert_eq!(limit_kib_text(1024), "1");
        assert_eq!(limit_kib_text(1500), "2");
        assert_eq!(limit_kib_text(3 * 1024 * 1024 * 1024), "3145728");
        assert_eq!(limit_kib_text(i64::MAX), (i64::MAX / 1024).to_string());
    }

    #[test]
    fn limit_input_parses_kib_with_saturation() {
        assert_eq!(parse_limit_kib(""), Some(0));
        assert_eq!(parse_limit_kib("  "), Some(0));
        assert_eq!(parse_limit_kib(" 512 "), Some(512 * 1024));
        assert_eq!(parse_limit_kib("99999999999999999999999"), Some(i64::MAX));
        assert_eq!(parse_limit_kib("-5"), None);
        assert_eq!(parse_limit_kib("1.5"), None);
        assert_eq!(parse_limit_kib("１２"), None);
    }

    #[test]
    fn limits_params_only_carry_changed_fields() {
        assert_eq!(bt_limits_params("t", ["0", "0"], ["0", ""]), Ok(None));
        assert_eq!(bt_limits_params("t", ["2", "0"], [" 2", "0"]), Ok(None));
        let params = bt_limits_params("t", ["0", "0"], ["100", "0"])
            .expect("valid")
            .expect("changed");
        assert_eq!(params.task_id, "t");
        assert_eq!(params.download_limit_bps, Some(100 * 1024));
        assert_eq!(params.upload_limit_bps, None);
        let params = bt_limits_params("t", ["5", "7"], ["", "8"])
            .expect("valid")
            .expect("changed");
        assert_eq!(params.download_limit_bps, Some(0));
        assert_eq!(params.upload_limit_bps, Some(8 * 1024));
        assert_eq!(bt_limits_params("t", ["0", "0"], ["-1", "0"]), Err(()));
        assert_eq!(bt_limits_params("t", ["0", "0"], ["0", "x"]), Err(()));
    }

    #[test]
    fn notice_text_keeps_head_and_extension() {
        assert_eq!(notice_text("short.mkv", 80), "short.mkv");
        let long = format!("{}.mkv", "电影 ".repeat(60));
        let text = notice_text(&long, 20);
        assert_eq!(text.chars().count(), 20);
        assert!(text.ends_with(".mkv"));
        assert!(text.starts_with("电影"));
        assert!(text.contains('…'));
        let unc = format!(r"\\nas\share\{}\Programs", "深层 目录\\".repeat(30));
        let text = notice_text(&unc, 40);
        assert!(text.starts_with(r"\\nas"));
        assert!(text.ends_with("Programs"));
    }
}
