//! 「完整性校验」：已完成 BT 任务按种子 piece 表重哈希落盘数据（`daemon.task.verify`）。
//!
//! 入口为下载页右键 / 应用菜单（单选、条件见
//! [`crate::components::task_table::TaskMenuFacts::can_verify_integrity`]）。
//! 开始时推送「正在校验」，结果（完好 / 损坏 / 无法校验 / 已在校验 / 失败）推送到
//! 发起时的窗口。损坏文件列表规则与 Web `model/actions.ts::verifyTaskIntegrity` 一致。

use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, TaskVerifyResult, TaskVerifyStatus};
use fluxdown_ui_components::toast;
use fluxdown_ui_i18n::Translator;
use gpui::{Context, Window};
use gpui_component::{WindowExt as _, notification::Notification};

use crate::{
    actions::VerifySelected,
    components::task_table::task_menu_facts_for,
    controller::{DownloadsCommand, DownloadsResult},
    pages::downloads::DownloadView,
    strings::error_text,
};

/// 损坏文件最多逐个列出的数量，其余折叠为「等 N 个文件」。
const LISTED_DAMAGED_FILES: usize = 3;

impl DownloadView {
    /// 右键 / 应用菜单「完整性校验」。
    pub(crate) fn on_verify_selected(
        &mut self,
        _: &VerifySelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let [key] = selected.as_slice() else {
            return;
        };
        let store = self.controller.store();
        if !task_menu_facts_for(store, key).is_some_and(|facts| facts.can_verify_integrity()) {
            return;
        }
        let Some(name) = store.get(key).map(|row| row.name.clone()) else {
            return;
        };
        let translator = self.translator.clone();
        let started = translator
            .read(cx)
            .text_with("verifyTaskStarted", &[("name", &name)]);
        window.push_notification(toast::info(started), cx);

        let verify = self.port.execute(DownloadsCommand::VerifyTask {
            task_id: key.task_id().to_owned(),
        });
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let result = verify.await;
            let notification = translator.read_with(cx, |translator, _| match result {
                Ok(DownloadsResult::TaskVerify(report)) => {
                    report_notice(translator, &name, &report)
                }
                Ok(_) => failure_notice(
                    translator,
                    &name,
                    &RpcErrorData::new(ApplicationErrorCode::Internal, false),
                ),
                Err(error) => failure_notice(translator, &name, &error),
            });
            if let Err(error) = handle.update(cx, |_, window, cx| {
                window.push_notification(notification, cx);
            }) {
                eprintln!("window closed before the integrity check result: {error:#}");
            }
        })
        .detach();
    }
}

fn report_notice(translator: &Translator, name: &str, report: &TaskVerifyResult) -> Notification {
    match report.status {
        TaskVerifyStatus::Intact => toast::success(translator.text_with(
            "verifyTaskIntact",
            &[
                ("name", name),
                ("count", &report.verified_pieces.to_string()),
            ],
        )),
        TaskVerifyStatus::Damaged => toast::warning(translator.text_with(
            "verifyTaskDamaged",
            &[
                ("name", name),
                ("bad", &report.bad_pieces.to_string()),
                ("total", &report.total_pieces.to_string()),
                (
                    "files",
                    &damaged_files_text(translator, &report.damaged_files),
                ),
            ],
        )),
        TaskVerifyStatus::Unverifiable => {
            toast::warning(translator.text_with("verifyTaskUnverifiable", &[("name", name)]))
        }
    }
}

fn failure_notice(translator: &Translator, name: &str, error: &RpcErrorData) -> Notification {
    if error.code == ApplicationErrorCode::Conflict {
        return toast::warning(translator.text_with("verifyTaskBusy", &[("name", name)]));
    }
    toast::error(translator.text_with(
        "verifyTaskFailed",
        &[("name", name), ("error", &error_text(translator, error))],
    ))
}

/// 前 [`LISTED_DAMAGED_FILES`] 个文件逐个列出，其余折叠为计数。
fn damaged_files_text(translator: &Translator, files: &[String]) -> String {
    let listed = files
        .iter()
        .take(LISTED_DAMAGED_FILES)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let rest = files.len().saturating_sub(LISTED_DAMAGED_FILES);
    if rest == 0 {
        listed
    } else {
        translator.text_with(
            "verifyTaskMoreFiles",
            &[("files", &listed), ("count", &rest.to_string())],
        )
    }
}
