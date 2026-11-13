use crate::model::devices::DispatchTarget;
use fluxdown_protocol::torrent_preview::{
    CustomNameIssue, TorrentConfirmParams, TorrentPreviewDto,
};
use gpui::SharedString;
use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
};

pub(in crate::pages::new_download) enum TorrentInput {
    File(PathBuf),
    Magnet(String),
}

pub(in crate::pages::new_download) fn split_inputs(
    values: Vec<String>,
    preview_magnets: bool,
) -> (Vec<TorrentInput>, Vec<String>) {
    let mut torrents = Vec::new();
    let mut urls = Vec::new();
    for value in values {
        let magnet = fluxdown_protocol::capture_link::is_magnet_link(&value);
        if magnet && preview_magnets {
            torrents.push(TorrentInput::Magnet(value));
        } else if !fluxdown_protocol::capture_link::is_capture_url(&value) {
            let path = if value.starts_with("file://") {
                fluxdown_protocol::capture_link::torrent_file_path(&value)
            } else {
                let path = PathBuf::from(&value);
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
                    .then_some(path)
            };
            if let Some(path) = path {
                torrents.push(TorrentInput::File(path));
            } else {
                urls.push(value);
            }
        } else {
            urls.push(value);
        }
    }
    (torrents, urls)
}

/// 这一批种子从哪里进来，决定整批结束后新建下载窗口的去向。
pub(in crate::pages::new_download) enum TorrentOrigin {
    /// 宿主交来的输入（系统打开、菜单、拖放）：窗口是为它们开的，处理完没有其他
    /// 链接就关窗。`external` = 系统打开。
    Host { external: bool },
    /// 表单里的「打开种子文件」。
    Picker,
    /// 表单提交时分流出来的磁力链；全部确认后按同一选择继续提交其余链接。
    Submit { later: bool, queue: Option<String> },
}

/// 当前这颗种子的处理结果。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::pages::new_download) enum ItemOutcome {
    /// 已建任务（预览确认，或跳过预览直接添加）。
    Submitted,
    /// 用户跳过：不建任务，磁力链留在表单里。
    Skipped,
}

pub(in crate::pages::new_download) struct TorrentDraft {
    pub(super) input: TorrentInput,
    pub(super) options: fluxdown_protocol::CreateTaskRequest,
    pub(super) external: bool,
}

pub(in crate::pages::new_download) struct TorrentForm {
    pub(super) pending: VecDeque<TorrentDraft>,
    pub(super) preview: Option<Box<TorrentPreviewDto>>,
    pub(super) ticket: Option<String>,
    pub(super) selected: HashSet<i32>,
    /// 上一次由预览成功自动填进「文件名」输入框的名称；用来判断用户是否手动
    /// 改过（磁力解析可能要几十秒，期间用户敲的字不能被迟到结果覆盖）。
    pub(super) filled_name: String,
    /// 进确认前用户选的下载目标与「目录被手动改过」标记，退出确认时原样还原。
    pub(super) restore_target: DispatchTarget,
    pub(super) restore_category_edited: bool,
    /// 整批结束且表单里没有其他链接时关窗（窗口是为这批种子开的）。
    pub(super) close_when_idle: bool,
    /// 表单提交分流出这批磁力链时的「稍后 / 队列」选择；全部确认后用它继续提交
    /// 表单里剩下的普通链接。
    pub(super) resume_submit: Option<(bool, Option<String>)>,
    /// 这一批里已经建过任务。
    pub(super) submitted_any: bool,
    pub(super) loading: bool,
    pub(super) submitting: bool,
    pub(super) error: Option<SharedString>,
    pub(super) expired: bool,
}

/// 把「文件名」输入框里用户敲的内容换算成提交用的改名目标。
///
/// 空输入或与种子原始名称相同 = 没改名（提交空串，引擎继续走原来的命名逻辑，
/// 磁力的 `dn` 回显豁免与 `.torrent` 的真实扩展名都不受影响）。
pub(in crate::pages::new_download) fn custom_name_for(
    preview: &TorrentPreviewDto,
    typed: &str,
) -> String {
    let trimmed = typed.trim();
    if trimmed.is_empty() || trimmed == preview.name {
        String::new()
    } else {
        trimmed.to_owned()
    }
}

/// 把「已经确认改过名的输入」翻译成本地化提示键；`None` = 可以用。
///
/// 只对**真正的改名**调用：种子作者起的原始名称本身可能带 `:`、`/`、超长等
/// 落不了盘的字符，那是「不改名」，不能挡住确认按钮。
pub(in crate::pages::new_download) fn custom_name_issue_key(custom: &str) -> Option<&'static str> {
    match fluxdown_protocol::torrent_preview::validate_custom_name(custom) {
        Ok(()) | Err(CustomNameIssue::Empty) => None,
        Err(CustomNameIssue::IllegalCharacter(_) | CustomNameIssue::TrailingDotOrSpace) => {
            Some("torrentConfirmNameInvalid")
        }
        Err(CustomNameIssue::ReservedName) => Some("torrentConfirmNameReserved"),
        Err(CustomNameIssue::TooLong) => Some("torrentConfirmNameTooLong"),
    }
}

impl TorrentForm {
    pub(super) fn begin_item(&mut self) -> String {
        self.filled_name.clear();
        self.pending
            .front()
            .map(|draft| draft.options.file_name.clone())
            .unwrap_or_default()
    }

    pub(super) fn preview_name(&mut self, current: &str, name: &str) -> Option<String> {
        if current.trim().is_empty() || current == self.filled_name {
            self.filled_name = name.to_owned();
            Some(name.to_owned())
        } else {
            None
        }
    }

    /// 当前待处理的输入是否为磁力链（只有磁力链能跳过预览直接添加）。
    pub(super) fn current_is_magnet(&self) -> bool {
        matches!(
            self.pending.front().map(|draft| &draft.input),
            Some(TorrentInput::Magnet(_))
        )
    }

    /// 跳过预览直接添加：磁力链按普通建任务请求提交，沿用确认前的草稿选项与当前
    /// 保存目录；文件选择回到任务拿到元数据之后的选择窗口。预览已就绪或正在提交时
    /// 不适用。
    pub(super) fn direct_request(
        &self,
        save_dir: &str,
        typed_name: &str,
    ) -> Option<fluxdown_protocol::CreateTaskRequest> {
        if self.submitting || self.preview.is_some() || save_dir.trim().is_empty() {
            return None;
        }
        let draft = self.pending.front()?;
        let TorrentInput::Magnet(url) = &draft.input else {
            return None;
        };
        let mut request = draft.options.clone();
        request.url.clone_from(url);
        request.save_dir = save_dir.trim().to_owned();
        let typed_name = typed_name.trim();
        if !typed_name.is_empty()
            && fluxdown_protocol::torrent_preview::validate_custom_name(typed_name).is_ok()
        {
            request.file_name = typed_name.to_owned();
        }
        Some(request)
    }

    pub(in crate::pages::new_download) fn restores_remote(&self) -> bool {
        self.restore_target != DispatchTarget::Local
    }

    pub(super) fn confirmation_queue(&self, later: bool, explicit: Option<String>) -> String {
        explicit.unwrap_or_else(|| {
            let draft = self.pending.front().map(|draft| &draft.options);
            // 草稿本来就是暂停态时保留它显式指定的队列；只有「立即下载」的草稿
            // 新选了「稍后下载」，才换成稍后队列。
            if later && !draft.is_some_and(|options| options.start_paused) {
                fluxdown_protocol::LATER_QUEUE_ID.to_owned()
            } else {
                draft
                    .map(|options| options.queue_id.clone())
                    .unwrap_or_default()
            }
        })
    }

    pub(super) fn confirmation(
        &self,
        save_dir: &str,
        queue_id: String,
        later: bool,
        custom_name: String,
    ) -> Option<TorrentConfirmParams> {
        if self.expired
            || self.loading
            || self.submitting
            || self.selected.is_empty()
            || save_dir.trim().is_empty()
        {
            return None;
        }
        let preview = self.preview.as_ref()?;
        // 名字不合法就不提交：按钮同时被 `ready` 禁用，这里再兜一层，避免
        // 快捷键 / 迟到状态绕过去让 daemon 用 InvalidArgument 拒绝。
        if !custom_name.is_empty()
            && fluxdown_protocol::torrent_preview::validate_custom_name(&custom_name).is_err()
        {
            return None;
        }
        // 磁力链要作为任务来源带回 daemon：任务以种子字节落库，但 `origin_url`
        // /「复制链接」/ URL 去重仍应显示用户点进来的这条磁力链。
        let draft = self.pending.front()?;
        let source_url = match &draft.input {
            TorrentInput::Magnet(url) => url.clone(),
            _ => String::new(),
        };
        Some(TorrentConfirmParams {
            torrent_blob_id: preview.torrent_blob_id.clone(),
            save_dir: save_dir.trim().to_owned(),
            selected_file_indices: {
                // 勾选集无序；按原始下标升序提交，结果与点选顺序无关。
                let mut indices = self.selected.iter().copied().collect::<Vec<_>>();
                indices.sort_unstable();
                indices
            },
            queue_id,
            start_paused: later,
            custom_name,
            source_url,
            options: Some(draft.options.clone()),
        })
    }
}
