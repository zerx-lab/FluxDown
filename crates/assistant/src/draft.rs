//! 输入区草稿：文本、图片附件（粘贴 / 拖入 / 选择）与图片占位符的同步。
//!
//! 附件在后台线程读取 / 转码；完成后按顺序加入并在光标处插入 `[Image #n]`。账号变化会作废
//! 在途的处理（回写前比较 `account_epoch`）。

use std::{path::PathBuf, sync::Arc};

use fluxdown_ui_components::{size_units::format_bytes, toast};
use gpui::{App, ClipboardItem, Context, Image, PathPromptOptions, Task, Window};
use gpui_component::WindowExt as _;

use crate::{
    attachment::{ImageError, ImageSource, Prepared, clipboard_sources, is_image_path, prepare},
    model::{
        Edit, Gate, Turn, insert_placeholder, map_offset, orphaned_images, placeholder_remnant,
        remove_placeholder,
    },
    view::AssistantView,
};

/// 一张已处理好、待随下一条提问发送的图片。
pub(crate) struct Attachment {
    /// `data:image/<type>;base64,...`。
    pub(crate) data_url: String,
    /// 输入区的缩略图。
    pub(crate) thumbnail: Arc<Image>,
}

/// 刚发出的带图提问：气泡据此显示本地缩略图（云端历史里只有图片 ID）。
pub(crate) struct SentImages {
    pub(crate) session_id: String,
    /// 提问文本（含占位符），用来对上对应的那一轮。
    pub(crate) text: String,
    pub(crate) thumbnails: Vec<Arc<Image>>,
}

/// 拖入的路径里能当图片的部分（按扩展名先过滤，再去计算名额）。
pub(crate) fn dropped_sources(paths: &[PathBuf]) -> Vec<ImageSource> {
    paths
        .iter()
        .filter(|path| is_image_path(path))
        .cloned()
        .map(ImageSource::File)
        .collect()
}

/// 按位置顺序应用编辑（`remove_placeholder` 的结果按位置升序、互不重叠）；越界、不在字符边界或
/// 与前一条重叠的编辑跳过，保持原文。
fn apply_edits(text: &str, edits: &[Edit]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for edit in edits {
        let (start, end) = (edit.range.start, edit.range.end);
        if start < cursor || !text.is_char_boundary(end) {
            continue;
        }
        let Some(kept) = text.get(cursor..start) else {
            continue;
        };
        out.push_str(kept);
        out.push_str(&edit.text);
        cursor = end;
    }
    out.push_str(text.get(cursor..).unwrap_or_default());
    out
}

/// 去掉文本里编号 1..=`count` 的图片占位符（编辑重发时旧提问的图被新附的图取代，
/// 旧占位符不能再指向新图）；超出 `count` 的 `[Image #n]` 是用户自己写的文字，保持原样。
pub(crate) fn strip_placeholders(text: &str, count: usize) -> String {
    let mut text = text.to_owned();
    for remaining in (1..=count).rev() {
        let edits = remove_placeholder(&text, 1, remaining);
        text = apply_edits(&text, &edits);
    }
    text
}

impl AssistantView {
    /// 输入框文本（已去首尾空白）。
    pub(crate) fn draft(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    /// 面板打开时把焦点交给输入框。
    pub fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// 清空草稿（文本与附件）：账号变化后在下一次渲染里生效（那里才有 `window`）。
    pub(crate) fn flush_draft_reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.draft_reset {
            return;
        }
        self.draft_reset = false;
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.sync_input_text(cx);
    }

    /// 云端接收图片时的 `(单条消息最多张数, 单张最大字节数)`；不接收（或张数为 0）为 `None`。
    pub(crate) fn image_limits(&self) -> Option<(usize, u64)> {
        let config = self.state.config.as_ref()?;
        (config.images_supported && config.max_images > 0)
            .then_some((config.max_images as usize, config.max_image_bytes))
    }

    /// 现在能否添加图片（入口可用、粘贴 / 拖入被接管的前提）。
    pub(crate) fn can_attach(&self) -> bool {
        matches!(
            self.gate(),
            Gate::Open {
                quota_exhausted: false
            }
        ) && !self.stale
            && self.outgoing.is_none()
            && self.image_limits().is_some()
    }

    /// 还能再加几张（已有 + 处理中都占名额）。
    fn free_image_slots(&self) -> usize {
        let max = self.image_limits().map_or(0, |(max, _)| max);
        max.saturating_sub(self.attachments.len() + self.attaching)
    }

    /// 图片附件相关的警示 toast。
    fn warn(&self, key: &str, args: &[(&str, &str)], window: &mut Window, cx: &mut Context<Self>) {
        let message = self.with(key, args, cx);
        window.push_notification(toast::warning(message), cx);
    }

    fn warn_limit(&self, window: &mut Window, cx: &mut Context<Self>) {
        let max = self.image_limits().map_or(0, |(max, _)| max);
        self.warn(
            "assistantAttachErrTooMany",
            &[("max", &max.to_string())],
            window,
            cx,
        );
    }

    /// 输入框收到粘贴：剪贴板里是图片（或全是图片文件）就接管，返回 `true`；否则交还输入框按文字粘贴。
    pub(crate) fn paste_images(
        &mut self,
        item: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.can_attach() {
            return false;
        }
        let sources = clipboard_sources(item);
        if sources.is_empty() {
            return false;
        }
        self.add_images(sources, window, cx);
        true
    }

    /// 把文件拖进面板：先按扩展名挑出图片，再按剩余名额收下；一张图片都没有时给类型提示。
    pub(crate) fn drop_files(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_attach() {
            return;
        }
        let sources = dropped_sources(paths);
        if sources.is_empty() {
            self.warn("assistantAttachErrType", &[], window, cx);
            return;
        }
        self.add_images(sources, window, cx);
    }

    /// 点「+」：系统文件选择器（可多选）。
    pub(crate) fn pick_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_attach() {
            return;
        }
        if self.free_image_slots() == 0 {
            self.warn_limit(window, cx);
            return;
        }
        let account = self.account_epoch;
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let paths = match receiver.await {
                Ok(Ok(Some(paths))) => paths,
                // 用户取消了选择。
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    log::warn!("assistant image picker failed: {error:#}");
                    return;
                }
                Err(error) => {
                    log::warn!("assistant image picker closed: {error}");
                    return;
                }
            };
            let sources = paths.into_iter().map(ImageSource::File).collect();
            let Ok(()) = this.update_in(cx, move |this, window, cx| {
                if this.account_epoch == account {
                    this.add_images(sources, window, cx);
                }
            }) else {
                // 助手页已随窗口释放，无需回写。
                return;
            };
        })
        .detach();
    }

    /// 把一批图片交给后台读取 / 转码；完成后按顺序加入附件并在光标处插入占位符。
    fn add_images(
        &mut self,
        mut sources: Vec<ImageSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, max_bytes)) = self.image_limits() else {
            return;
        };
        let room = self.free_image_slots();
        if sources.len() > room {
            self.warn_limit(window, cx);
            sources.truncate(room);
        }
        if sources.is_empty() {
            return;
        }
        self.attaching += sources.len();
        let account = self.account_epoch;
        let executor = cx.background_executor().clone();
        let tasks: Vec<Task<Result<Prepared, ImageError>>> = sources
            .into_iter()
            .map(|source| executor.spawn(async move { prepare(source, max_bytes) }))
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let mut results = Vec::with_capacity(tasks.len());
            for task in tasks {
                results.push(task.await);
            }
            let Ok(()) = this.update_in(cx, move |this, window, cx| {
                if this.account_epoch == account {
                    this.finish_adding(results, max_bytes, window, cx);
                }
            }) else {
                // 助手页已随窗口释放，无需回写。
                return;
            };
        })
        .detach();
        cx.notify();
    }

    fn finish_adding(
        &mut self,
        results: Vec<Result<Prepared, ImageError>>,
        max_bytes: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attaching = self.attaching.saturating_sub(results.len());
        let max_images = self.image_limits().map_or(0, |(max, _)| max);
        let mut added = false;
        let mut over_limit = false;
        let mut unsupported = false;
        let mut unreadable = false;
        let mut too_large = false;
        for result in results {
            match result {
                // 占位符编号取「加入后的张数」，与 `attachments` 的顺序一致。
                Ok(prepared) if self.attachments.len() < max_images => {
                    if self.attachments.is_empty() {
                        self.drop_kept_images(window, cx);
                    }
                    self.attachments.push(Attachment {
                        data_url: prepared.data_url,
                        thumbnail: prepared.thumbnail,
                    });
                    let number = self.attachments.len();
                    self.insert_image_placeholder(number, window, cx);
                    added = true;
                }
                Ok(_) => over_limit = true,
                Err(ImageError::Unsupported) => unsupported = true,
                Err(ImageError::Unreadable) => unreadable = true,
                Err(ImageError::TooLarge) => too_large = true,
            }
        }
        if added {
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
        if over_limit {
            self.warn_limit(window, cx);
        }
        if too_large {
            let size = format_bytes(max_bytes, 2);
            self.warn("assistantAttachErrTooLarge", &[("size", &size)], window, cx);
        }
        if unsupported {
            self.warn("assistantAttachErrType", &[], window, cx);
        }
        if unreadable {
            self.warn("assistantAttachErrRead", &[], window, cx);
        }
        cx.notify();
    }

    /// 编辑重发时用户附了新图：旧提问的图不再沿用，文本里指向它们的占位符一并去掉。
    fn drop_kept_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self
            .editing
            .as_mut()
            .filter(|editing| editing.image_count > 0)
        else {
            return;
        };
        let old = std::mem::take(&mut editing.image_count);
        let text = self.input.read(cx).value();
        let stripped = strip_placeholders(&text, old);
        if stripped != text.as_ref() {
            self.input
                .update(cx, |input, cx| input.set_value(stripped, window, cx));
            self.sync_input_text(cx);
        }
    }

    /// 在输入框光标处（有选区则替换选区）放入第 `number` 张图的占位符。
    fn insert_image_placeholder(
        &mut self,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |input, cx| {
            let text = input.value();
            let edit = insert_placeholder(&text, input.selected_range(), number);
            input.set_selected_range(edit.range, cx);
            input.replace(edit.text, window, cx);
        });
        self.sync_input_text(cx);
    }

    /// 记下输入框当前文本：程序改动（`set_value` 不发 `Change`、占位符增删）后调用，
    /// 免得下一次用户编辑被拿来和过期文本比较。
    pub(crate) fn sync_input_text(&mut self, cx: &App) {
        self.input_text = self.input.read(cx).value();
    }

    /// 输入框文本变了：删到一半的占位符整体删掉；占位符已不在文本里的图片随之移除
    /// （手动删除、剪切、撤销插入等）。
    pub(crate) fn input_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value();
        let previous = std::mem::replace(&mut self.input_text, text.clone());
        if let Some(range) = placeholder_remnant(&previous, &text, self.attachments.len()) {
            self.input.update(cx, |input, cx| {
                input.set_selected_range(range, cx);
                input.replace("", window, cx);
            });
            self.sync_input_text(cx);
        }
        // 从大到小移除，前面的编号才不受重排影响。
        let orphaned = orphaned_images(&self.input_text, self.attachments.len());
        for number in orphaned.into_iter().rev() {
            self.remove_image(number, window, cx);
        }
        cx.notify();
    }

    /// 移除第 `number` 张图：删掉文本里它的占位符，后面的占位符与缩略图编号依次减 1。
    pub(crate) fn remove_image(
        &mut self,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.attachments.len();
        if self.outgoing.is_some() || number == 0 || number > count {
            return;
        }
        self.attachments.remove(number - 1);
        let text = self.input.read(cx).value();
        let edits = remove_placeholder(&text, number, count);
        if !edits.is_empty() {
            self.input.update(cx, |input, cx| {
                let selection = input.selected_range();
                // 从后往前改，前面的偏移才不会失效；逐处替换保留撤销历史与滚动位置。
                for edit in edits.iter().rev() {
                    input.set_selected_range(edit.range.clone(), cx);
                    input.replace(edit.text.clone(), window, cx);
                }
                let start = map_offset(selection.start, &edits);
                let end = map_offset(selection.end, &edits);
                input.set_selected_range(start..end, cx);
            });
            self.sync_input_text(cx);
        }
        cx.notify();
    }

    /// 这一轮提问的本地缩略图：只认刚发出的那条（最后一轮、文本一致、张数对得上）。
    pub(crate) fn local_thumbnails(&self, turn: &Turn, last: bool) -> Option<&[Arc<Image>]> {
        let sent = self.sent_images.as_ref()?;
        let session = self.current.as_deref()?;
        let same = last
            && sent.session_id == session
            && sent.text == turn.question
            && (turn.live.is_some() || turn.image_count == sent.thumbnails.len());
        same.then_some(sent.thumbnails.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_keep_only_images_before_counting() {
        let paths = [
            PathBuf::from("/tmp/a.png"),
            PathBuf::from("/tmp/notes.txt"),
            PathBuf::from("/tmp/B.JPG"),
            PathBuf::from("/tmp/folder"),
        ];
        let sources = dropped_sources(&paths);
        assert_eq!(
            sources,
            vec![
                ImageSource::File(PathBuf::from("/tmp/a.png")),
                ImageSource::File(PathBuf::from("/tmp/B.JPG")),
            ]
        );
        assert!(dropped_sources(&[PathBuf::from("/tmp/readme.md")]).is_empty());
    }

    #[test]
    fn strip_removes_old_placeholders_and_keeps_user_text() {
        assert_eq!(
            strip_placeholders("compare [Image #1] with [Image #2] please", 2),
            "compare with please"
        );
        // 编号超出旧图张数的是用户自己写的文字。
        assert_eq!(
            strip_placeholders("see [Image #1] and [Image #5]", 1),
            "see and [Image #5]"
        );
        assert_eq!(strip_placeholders("no images here", 3), "no images here");
        assert_eq!(strip_placeholders("[Image #1]", 1), "");
    }
}
