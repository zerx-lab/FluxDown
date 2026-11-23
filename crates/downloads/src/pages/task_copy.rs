//! 「复制文件」：把选中的已完成任务产物以文件形式写入系统剪贴板（`agent.platform.copyTaskFiles`），
//! 之后可在文件管理器 / 聊天软件里直接粘贴。
//!
//! 由常驻 agent 写入而不是界面进程：GPUI 剪贴板只支持文本 / 图片，且 X11 上剪贴板内容归写入进程
//! 所有，界面关闭后 agent 仍在提供。入口为下载页右键 / 应用菜单，条件见
//! [`crate::components::task_table::TaskMenuFacts::has_product_on_disk`]（可多选、要求全体成立）；
//! 与 Web `model/actions.ts::copyTaskFiles` 一致。

use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData};
use fluxdown_ui_components::toast;
use fluxdown_ui_i18n::Translator;
use gpui::{Context, Window};
use gpui_component::WindowExt as _;

use crate::{
    actions::CopySelectedFiles, components::task_table::task_menu_facts_for,
    controller::DownloadsCommand, pages::downloads::DownloadView, strings::error_text,
};

impl DownloadView {
    /// 右键 / 应用菜单「复制文件」。
    pub(crate) fn on_copy_selected_files(
        &mut self,
        _: &CopySelectedFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let store = self.controller.store();
        // 应用菜单不随选区隐藏：选区里有任一任务产物不可用就整体不做（与右键交集语义一致）。
        let copyable = !selected.is_empty()
            && selected.iter().all(|key| {
                task_menu_facts_for(store, key).is_some_and(|facts| facts.has_product_on_disk())
            });
        if !copyable {
            return;
        }
        let task_ids = selected
            .iter()
            .map(|key| key.task_id().to_owned())
            .collect();
        let copy = self
            .port
            .execute(DownloadsCommand::CopyTaskFiles { task_ids });
        let translator = self.translator.clone();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = copy.await;
            // 产物已被删除 / 移走：立即重扫，让行上的丢失标记跟上磁盘现状（同「打开文件」失败）。
            if matches!(&result, Err(error) if error.code == ApplicationErrorCode::NotFound)
                && this
                    .update(cx, |this, cx| this.rescan_files_now(cx))
                    .is_err()
            {
                // 视图已释放：窗口随之关闭，没有可通知的对象。
                return;
            }
            let notification = translator.read_with(cx, |translator, _| match &result {
                Ok(_) => toast::success(translator.text("fileCopiedToClipboard").to_owned()),
                Err(error) => toast::error(failure_text(translator, error)),
            });
            if let Err(error) = handle.update(cx, |_, window, cx| {
                window.push_notification(notification, cx);
            }) {
                eprintln!("window closed before the copy-files result: {error:#}");
            }
        })
        .detach();
    }
}

fn failure_text(translator: &Translator, error: &RpcErrorData) -> String {
    match error.code {
        ApplicationErrorCode::NotFound => translator.text("copyFileErrNotFound").to_owned(),
        ApplicationErrorCode::Unsupported => translator.text("copyFileErrUnsupported").to_owned(),
        _ => format!(
            "{} {}",
            translator.text("copyFileFailed"),
            error_text(translator, error)
        ),
    }
}
