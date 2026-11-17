//! 「复制磁力链接」「导出 .torrent 文件…」：BT 任务的分享材料（`daemon.task.torrentShare`）。
//!
//! 入口为下载页右键 / 应用菜单（单选、条件见
//! [`crate::components::task_table::TaskMenuFacts::can_share_torrent`]），任意任务状态可用。
//! 磁力链接由 agent 拼好（含种子自带与会话追加的 tracker）；`.torrent` 字节经 RPC 取回后写到
//! 本机（即便连接的是远端主机），与 Web `model/actions.ts` 的浏览器下载同构。

use std::path::PathBuf;

use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData};
use fluxdown_ui_components::toast;
use gpui::{AppContext as _, ClipboardItem, Context, Window};
use gpui_component::{WindowExt as _, notification::Notification};

use crate::{
    actions::{CopyMagnetSelected, ExportTorrentSelected},
    components::task_table::task_menu_facts_for,
    controller::{DownloadsCommand, DownloadsResult, TorrentShare},
    pages::downloads::DownloadView,
    strings::error_text,
};

/// 导出文件名主干的字符上限（不含 `.torrent`）。
const MAX_FILE_STEM_CHARS: usize = 150;

impl DownloadView {
    /// 右键 / 应用菜单「复制磁力链接」。
    pub(crate) fn on_copy_magnet_selected(
        &mut self,
        _: &CopyMagnetSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self.selected_torrent_task(cx) else {
            return;
        };
        let share = self.fetch_torrent_share(task_id, false);
        let translator = self.translator.clone();
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let result = share.await;
            let notification = translator.read_with(cx, |translator, _| match &result {
                Ok(_) => toast::success(translator.text("btMagnetCopied").to_owned()),
                Err(error) => toast::error(error_text(translator, error)),
            });
            let magnet = result.ok().map(|share| share.magnet);
            if let Err(error) = handle.update(cx, |_, window, cx| {
                if let Some(magnet) = magnet {
                    cx.write_to_clipboard(ClipboardItem::new_string(magnet));
                }
                window.push_notification(notification, cx);
            }) {
                eprintln!("window closed before the magnet link result: {error:#}");
            }
        })
        .detach();
    }

    /// 右键 / 应用菜单「导出 .torrent 文件…」。
    pub(crate) fn on_export_torrent_selected(
        &mut self,
        _: &ExportTorrentSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self.selected_torrent_task(cx) else {
            return;
        };
        let share = self.fetch_torrent_share(task_id, true);
        let translator = self.translator.clone();
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let notify = |notification: Notification, cx: &mut gpui::AsyncApp| {
                if let Err(error) = handle.update(cx, |_, window, cx| {
                    window.push_notification(notification, cx);
                }) {
                    eprintln!("window closed before the torrent export result: {error:#}");
                }
            };
            let share = match share.await {
                Ok(share) => share,
                Err(error) => {
                    let notification = translator.read_with(cx, |translator, _| {
                        toast::error(error_text(translator, &error))
                    });
                    notify(notification, cx);
                    return;
                }
            };
            let TorrentShare { name, torrent, .. } = share;
            let Some(bytes) = torrent else {
                let notification = translator.read_with(cx, |translator, _| {
                    toast::error(translator.text("btNoTorrentFile").to_owned())
                });
                notify(notification, cx);
                return;
            };
            let file_name = torrent_file_name(&name);
            let directory = std::env::home_dir().unwrap_or_else(std::env::temp_dir);
            let receiver = match handle.update(cx, |_, _, cx| {
                cx.prompt_for_new_path(&directory, Some(&file_name))
            }) {
                Ok(receiver) => receiver,
                Err(error) => {
                    eprintln!("window closed before the torrent save dialog: {error:#}");
                    return;
                }
            };
            let path: PathBuf = match receiver.await {
                Ok(Ok(Some(path))) => path,
                // 用户取消：不提示。
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    let notification = toast::error(format!("{error:#}"));
                    notify(notification, cx);
                    return;
                }
                Err(error) => {
                    eprintln!("torrent save dialog closed unexpectedly: {error:#}");
                    return;
                }
            };
            let written = cx
                .background_spawn(async move { std::fs::write(&path, bytes) })
                .await;
            let notification = translator.read_with(cx, |translator, _| match written {
                Ok(()) => toast::success(translator.text("btTorrentExported").to_owned()),
                Err(error) => toast::error(format!("{error}")),
            });
            notify(notification, cx);
        })
        .detach();
    }

    /// 单选且为本机 BT 任务时返回其任务 ID。
    fn selected_torrent_task(&self, cx: &Context<Self>) -> Option<String> {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let [key] = selected.as_slice() else {
            return None;
        };
        task_menu_facts_for(self.controller.store(), key)
            .is_some_and(|facts| facts.can_share_torrent())
            .then(|| key.task_id().to_owned())
    }

    fn fetch_torrent_share(
        &self,
        task_id: String,
        include_torrent: bool,
    ) -> impl Future<Output = Result<TorrentShare, RpcErrorData>> + use<> {
        let pending = self.port.execute(DownloadsCommand::TorrentShare {
            task_id,
            include_torrent,
        });
        async move {
            match pending.await? {
                DownloadsResult::TorrentShare(share) => Ok(share),
                _ => Err(RpcErrorData::new(ApplicationErrorCode::Internal, false)),
            }
        }
    }
}

/// 导出对话框的默认文件名：`<任务名>.torrent`，去掉路径分隔符 / 保留字符与首尾空白、点。
fn torrent_file_name(name: &str) -> String {
    let stem = name
        .strip_suffix(".torrent")
        .or_else(|| name.strip_suffix(".TORRENT"))
        .unwrap_or(name);
    let cleaned: String = stem
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .take(MAX_FILE_STEM_CHARS)
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c.is_whitespace() || c == '.');
    if trimmed.is_empty() {
        "torrent.torrent".to_owned()
    } else {
        format!("{trimmed}.torrent")
    }
}

#[cfg(test)]
mod tests {
    use super::torrent_file_name;

    #[test]
    fn file_name_replaces_reserved_characters() {
        assert_eq!(torrent_file_name("a/b:c*d"), "a_b_c_d.torrent");
    }

    #[test]
    fn file_name_does_not_double_the_extension() {
        assert_eq!(torrent_file_name("ubuntu.torrent"), "ubuntu.torrent");
    }

    #[test]
    fn file_name_falls_back_when_empty() {
        assert_eq!(torrent_file_name(""), "torrent.torrent");
        assert_eq!(torrent_file_name(" .. "), "torrent.torrent");
    }

    #[test]
    fn file_name_keeps_cjk_and_truncates_by_char() {
        let long = "中".repeat(400);
        let name = torrent_file_name(&long);
        assert_eq!(name.chars().count(), 150 + ".torrent".len());
    }
}
