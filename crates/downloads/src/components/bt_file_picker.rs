//! BT 文件勾选：引擎发起的文件选择窗口与新建下载里的种子确认共用同一套列表与勾选
//! 逻辑，文件夹树、连选、筛选等能力只需在这里加一次。

use std::collections::HashSet;

use fluxdown_protocol::BtFileDto;
use fluxdown_ui_components::{ControlExt as _, check_row, tabular_numbers};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    ClickEvent, Context, Div, ParentElement as _, SharedString, Styled as _, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{button::Button, h_flex, scroll::ScrollableElement as _, v_flex};

use crate::model::format_bytes;

/// 勾选区用到的文案。
#[derive(Clone)]
pub(crate) struct BtFilePickerLabels {
    select_all: SharedString,
    deselect_all: SharedString,
    /// 含 `{n}` 占位的「已选 N 个」。
    selected_count: SharedString,
}

impl BtFilePickerLabels {
    pub(crate) fn from_translator(translator: &Translator) -> Self {
        Self {
            select_all: translator.text("btFileSelectAll").to_owned().into(),
            deselect_all: translator.text("deselectAll").to_owned().into(),
            selected_count: translator.text("selectedCount").to_owned().into(),
        }
    }
}

/// 从视图里取出当前可编辑的文件表与勾选集；此刻不可编辑（状态已切走、正在提交）时
/// 返回 `None`，点击不生效。
pub(crate) type BtFilePickerState<V> =
    for<'a> fn(&'a mut V) -> Option<(&'a [BtFileDto], &'a mut HashSet<i32>)>;

/// 全选：勾选集取文件表里的全部原始下标。
pub(crate) fn select_all(files: &[BtFileDto], selected: &mut HashSet<i32>) {
    *selected = files.iter().map(|file| file.index).collect();
}

/// 已勾选文件的总字节数；种子元数据里的负数大小按 0 计，求和饱和。
pub(crate) fn selected_bytes(files: &[BtFileDto], selected: &HashSet<i32>) -> u64 {
    files
        .iter()
        .filter(|file| selected.contains(&file.index))
        .map(|file| u64::try_from(file.size).unwrap_or(0))
        .fold(0, u64::saturating_add)
}

/// 「全部文件 / 取消全选」工具栏、已选汇总与可滚动的文件勾选列表。
///
/// 返回的容器占满父级剩余高度（`flex_1` + `min_h_0`），父级须是定高的纵向 flex。
pub(crate) fn bt_file_picker<V: 'static>(
    files: &[BtFileDto],
    selected: &HashSet<i32>,
    labels: &BtFilePickerLabels,
    state: BtFilePickerState<V>,
    cx: &mut Context<V>,
) -> Div {
    let tokens = active_theme(cx).tokens().clone();
    let toolbar = h_flex()
        .items_center()
        .gap(tokens.spacing.sm)
        .child(
            Button::new("bt-select-all")
                .outline()
                .control(cx)
                .label(labels.select_all.clone())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some((files, selected)) = state(this) {
                        select_all(files, selected);
                    }
                    cx.notify();
                })),
        )
        .child(
            Button::new("bt-deselect-all")
                .outline()
                .control(cx)
                .label(labels.deselect_all.clone())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some((_, selected)) = state(this) {
                        selected.clear();
                    }
                    cx.notify();
                })),
        )
        .child(
            div()
                .flex_1()
                .text_right()
                .text_size(tokens.typography.xs.size)
                .line_height(tokens.typography.xs.line_height)
                .font_features(tabular_numbers())
                .text_color(tokens.colors.muted_foreground)
                .child(SharedString::from(format!(
                    "{} · {}",
                    labels
                        .selected_count
                        .replace("{n}", &selected.len().to_string()),
                    format_bytes(selected_bytes(files, selected))
                ))),
        );
    let list = v_flex()
        .flex_1()
        .min_h_0()
        .children(files.iter().enumerate().map(|(position, file)| {
            let checked = selected.contains(&file.index);
            let index = file.index;
            let depth = file.path.matches('/').count().min(4) as f32;
            let name = file
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&file.path)
                .to_owned();
            let this = cx.weak_entity();
            check_row(
                ("bt-file", position),
                checked,
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(tokens.spacing.sm)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(tokens.typography.xs.size)
                            .font_features(tabular_numbers())
                            .text_color(tokens.colors.muted_foreground)
                            .child(SharedString::from(format_bytes(
                                u64::try_from(file.size).unwrap_or(0),
                            ))),
                    ),
                move |check, _, cx| {
                    let Ok(()) = this.update(cx, |this, cx| {
                        if let Some((_, selected)) = state(this) {
                            if check {
                                selected.insert(index);
                            } else {
                                selected.remove(&index);
                            }
                        }
                        cx.notify();
                    }) else {
                        // 视图已释放，结束回调而不再更新状态。
                        return;
                    };
                },
                cx,
            )
            .pl(tokens.spacing.xs + tokens.spacing.lg * depth)
            .pr(tokens.spacing.sm)
            .when(checked, |row| row.bg(tokens.colors.accent))
        }))
        .overflow_y_scrollbar();
    v_flex()
        .flex_1()
        .min_h_0()
        .gap(tokens.spacing.sm)
        .child(toolbar)
        .child(div().flex_1().min_h_0().child(list))
}

#[cfg(test)]
mod tests {
    use super::{select_all, selected_bytes};
    use fluxdown_protocol::BtFileDto;
    use std::collections::HashSet;

    fn files() -> Vec<BtFileDto> {
        // 下标不连续：种子里的 padding 文件被预览过滤，原始下标必须原样保留。
        [(0, "a.txt", 4), (2, "目录/b.txt", 6), (5, "broken", -1)]
            .into_iter()
            .map(|(index, path, size)| BtFileDto {
                index,
                path: path.to_owned(),
                size,
            })
            .collect()
    }

    #[test]
    fn select_all_uses_original_indices_and_summary_ignores_unselected_and_negative_sizes() {
        let files = files();
        let mut selected = HashSet::from([9]);
        select_all(&files, &mut selected);
        assert_eq!(selected, HashSet::from([0, 2, 5]));
        assert_eq!(selected_bytes(&files, &selected), 10);
        selected.remove(&2);
        assert_eq!(selected_bytes(&files, &selected), 4);
        selected.clear();
        assert_eq!(selected_bytes(&files, &selected), 0);
    }
}
