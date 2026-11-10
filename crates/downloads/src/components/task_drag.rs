//! 任务行拖出到系统文件管理器 / 桌面。
//!
//! 已完成且文件仍在下载目录时，按住文件名列即可拖动（任意方向，包括窗口上沿或下沿
//! 之外的文件夹），不用修饰键。其他列是划选。拖拽离开窗口时 gpui 把它提升为平台原生文件拖拽
//! （`external_drag_payload`）。gpui-pre 目前只有 macOS 与 Linux Wayland 实现了该能力；
//! 其他平台（Windows、X11）拖出窗口无动作，窗口内拖拽也不改变任何状态。

use std::path::PathBuf;

use fluxdown_ui_icon_pack::FileKind;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, Context, Entity, ExternalDragPayload, FileDragPaths, IntoElement,
    ParentElement, Pixels, Point, Render, SharedString, Styled, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{h_flex, table::TableState};

use crate::{
    components::{file_icon::named_file_icon, task_table::DownloadTableDelegate},
    model::RowKey,
    pages::downloads::DownloadView,
};

/// 预览与光标的间距。
const PREVIEW_CURSOR_GAP: f32 = 12.;
/// 预览最大宽度（长文件名截断）。
const PREVIEW_MAX_WIDTH: f32 = 320.;

/// 行拖拽值：随行渲染构造，只持有锚点与弱引用；参与拖出的路径在拖起 / 离开窗口时
/// 按当时的选区与磁盘现状现算。
#[derive(Clone)]
pub(crate) struct DraggedTasks {
    pub(crate) anchor: RowKey,
    pub(crate) kind: FileKind,
    /// 小写文件名（图标包按它匹配）。
    pub(crate) name_fold: SharedString,
    pub(crate) name: SharedString,
    pub(crate) table: WeakEntity<TableState<DownloadTableDelegate>>,
    pub(crate) host: Option<WeakEntity<DownloadView>>,
}

impl DraggedTasks {
    /// `on_drag` 构造器：跟随光标的预览（锚点文件名 + 同时拖出的其余文件数）。
    pub(crate) fn preview(
        &self,
        click_offset: Point<Pixels>,
        cx: &mut App,
    ) -> Entity<TaskDragPreview> {
        let count = self.table.upgrade().map_or(1, |table| {
            table.update(cx, |table, _| {
                // 拖放已经开始：随后的单击不再把选区改成这一行。
                table.delegate_mut().note_file_drag_started();
                table.delegate().drag_paths(&self.anchor).len()
            })
        });
        let preview = TaskDragPreview {
            kind: self.kind,
            name_fold: self.name_fold.clone(),
            name: self.name.clone(),
            extra: count.saturating_sub(1),
            click_offset,
        };
        cx.new(|_| preview)
    }

    /// 拖出窗口时交给平台的文件：逐个探测磁盘现状，已不在的跳过并立即触发一次文件
    /// 重扫（丢失标记经 `fileMissingChanged` 回流到行上）；一个都不剩时不发起平台拖拽。
    pub(crate) fn external_payload(&self, cx: &mut App) -> Option<ExternalDragPayload> {
        let table = self.table.upgrade()?;
        let paths = table.read(cx).delegate().drag_paths(&self.anchor);
        let (entries, any_missing) = probe_drag_entries(paths);
        if any_missing && let Some(host) = self.host.as_ref().and_then(WeakEntity::upgrade) {
            host.update(cx, |view, cx| view.rescan_files_now(cx));
        }
        (!entries.is_empty()).then(|| ExternalDragPayload::Files(FileDragPaths::new(entries)))
    }
}

/// 探测每个路径：存在的连同「是否目录」（多文件 BT 任务的产物是目录）返回；
/// 第二项表示是否有路径已不在磁盘上。
fn probe_drag_entries(paths: Vec<PathBuf>) -> (Vec<(PathBuf, bool)>, bool) {
    let mut any_missing = false;
    let entries = paths
        .into_iter()
        .filter_map(|path| match std::fs::metadata(&path) {
            Ok(metadata) => Some((path, metadata.is_dir())),
            Err(_) => {
                any_missing = true;
                None
            }
        })
        .collect();
    (entries, any_missing)
}

/// 窗口内拖拽时跟随光标的预览。
pub(crate) struct TaskDragPreview {
    kind: FileKind,
    name_fold: SharedString,
    name: SharedString,
    /// 除锚点外同时拖出的文件数。
    extra: usize,
    /// 按下点相对行原点的偏移：gpui 把预览原点放在「光标 − 偏移」，补回后预览贴着光标。
    click_offset: Point<Pixels>,
}

impl Render for TaskDragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let icon_size = active_theme(cx).extended().icon.md;
        let icon = named_file_icon(&self.name_fold, self.kind, icon_size, window, cx);
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        div()
            .pl(self.click_offset.x + px(PREVIEW_CURSOR_GAP))
            .pt(self.click_offset.y + px(PREVIEW_CURSOR_GAP))
            .child(
                h_flex()
                    .max_w(theme.text_extent(PREVIEW_MAX_WIDTH))
                    .px(tokens.spacing.md)
                    .py(tokens.spacing.xs)
                    .gap(tokens.spacing.sm)
                    .rounded(tokens.radius.sm)
                    .border(extended.stroke.thin)
                    .border_color(tokens.colors.border)
                    .bg(tokens.colors.surface)
                    .shadow(tokens.shadow.sm.clone())
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .text_color(tokens.colors.surface_foreground)
                    .child(icon)
                    .child(div().min_w_0().truncate().child(self.name.clone()))
                    .when(self.extra > 0, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .px(tokens.spacing.xs)
                                .rounded(tokens.radius.sm)
                                .bg(tokens.colors.accent)
                                .text_size(tokens.typography.xs.size)
                                .line_height(tokens.typography.xs.line_height)
                                .text_color(tokens.colors.accent_foreground)
                                .child(SharedString::from(format!("+{}", self.extra))),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::probe_drag_entries;

    #[test]
    fn probe_keeps_existing_files_and_directories_and_flags_missing() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!("fluxdown-drag-probe-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bt-folder"))?;
        std::fs::write(dir.join("file.bin"), b"x")?;

        let (entries, any_missing) = probe_drag_entries(vec![
            dir.join("file.bin"),
            dir.join("gone.bin"),
            dir.join("bt-folder"),
        ]);
        assert_eq!(
            entries,
            [(dir.join("file.bin"), false), (dir.join("bt-folder"), true)]
        );
        assert!(any_missing);

        let (entries, any_missing) = probe_drag_entries(vec![dir.join("gone.bin")]);
        assert!(entries.is_empty());
        assert!(any_missing);

        std::fs::remove_dir_all(&dir)
    }
}
