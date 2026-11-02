//! 任务组文件夹行：可见行组装（归组、与任务合并排序、展开为成员行 / 目录行）、组选择、
//! 组行与目录行渲染。纯规则在 [`crate::model::group_tree`]；Web 镜像见
//! `web/src/pages/downloads/model/groupTree.ts`。

use std::{cmp::Ordering, collections::HashMap, rc::Rc};

use fluxdown_ui_components::{CheckState, FluxIcon, check_mark, tabular_numbers, tooltip_view};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement, SharedString, Stateful, StatefulInteractiveElement as _, Styled,
    div, prelude::FluentBuilder as _, px,
};
use gpui_component::{Icon, h_flex, table::TableState, v_flex};

use super::{
    CELL_PADDING_X, DownloadColumnKind, DownloadTableDelegate, GroupBucket, PROGRESS_GAP,
    PROGRESS_LABEL_WIDTH, ROW_ACTION_SIZE, ROW_GROUP, SELECT_CELL_GROUP, SELECTED_INSET_X,
    SelectModifiers, VisibleRow, meta_cell, percent_label, row_at, status_color,
    wrap_row_action_strip,
};
use crate::{
    components::group_visuals::{group_counts_line, group_folder_icon},
    model::{
        DownloadTaskView, RowId, RowKey, TaskSource, TaskState, format_bytes,
        group_tree::{
            GroupAggregate, GroupCounts, SPARKLINE_MAX_BARS, TreeMember, TreeRow, compress_path,
            flatten, group_sort_view, member_dir, sparkline_height, sparkline_sample,
        },
        view_prefs::{ViewDensity, ViewGroupBy},
    },
    pages::downloads::DownloadView,
};

/// 组内缩进槽宽（容纳一个展开箭头）。
pub(super) const TREE_INDENT: f32 = 18.;

/// 顶层实体：任务或任务组文件夹行（`group_rows` 下标）。
#[derive(Clone, Copy, Debug)]
pub(super) enum Entity {
    Task(RowId),
    Group(usize),
}

/// 组展开后的一行（目录树顺序）。
#[derive(Clone, Debug)]
enum TreeEntry {
    Member {
        id: RowId,
        depth: u8,
    },
    Dir {
        path: String,
        count: usize,
        size_bytes: u64,
    },
}

/// 一个任务组在本次可见行里的文件夹行。
pub(super) struct GroupRow {
    pub(super) id: String,
    /// 本视图范围（侧栏筛选 + 搜索）内的成员，按组内目录树顺序。
    pub(super) members: Vec<RowId>,
    /// 组行各列的数据源，也是排序 / 分桶代理（见 [`group_sort_view`]）。
    pub(super) view: DownloadTaskView,
    pub(super) aggregate: GroupAggregate,
    pub(super) expanded: bool,
    tree: Vec<TreeEntry>,
}

impl DownloadTableDelegate {
    /// 把通过筛选的行（已排序、已应用保持期）折叠成顶层实体：属于已知任务组的本地任务归入
    /// 组行；组按代理视图排序后并入任务序列，插在第一个排在它之后的任务前。
    pub(super) fn fold_groups(&mut self, rows: Vec<(RowId, &DownloadTaskView)>) -> Vec<Entity> {
        self.group_rows.clear();
        let mut top: Vec<(RowId, &DownloadTaskView)> = Vec::with_capacity(rows.len());
        let mut index: HashMap<&str, usize> = HashMap::new();
        let mut grouped: Vec<(&str, Vec<(RowId, &DownloadTaskView)>)> = Vec::new();
        for (id, task) in rows {
            if task.source != TaskSource::Local
                || task.group_id.is_empty()
                || !self.groups.contains_key(&task.group_id)
            {
                top.push((id, task));
                continue;
            }
            match index.get(task.group_id.as_str()) {
                Some(&ix) => grouped[ix].1.push((id, task)),
                None => {
                    index.insert(task.group_id.as_str(), grouped.len());
                    grouped.push((task.group_id.as_str(), vec![(id, task)]));
                }
            }
        }
        for (group_id, members) in grouped {
            let Some(meta) = self.groups.get(group_id) else {
                continue;
            };
            let input: Vec<TreeMember> = members
                .iter()
                .map(|(_, task)| TreeMember {
                    dir: member_dir(&meta.save_dir, &task.save_dir),
                    name_fold: task.name_fold.clone(),
                    size_bytes: task.size_bytes,
                })
                .collect();
            let tree: Vec<TreeEntry> = flatten(&input, |_| false)
                .into_iter()
                .filter_map(|row| match row {
                    TreeRow::Member { index, nested } => {
                        members.get(index).map(|(id, _)| TreeEntry::Member {
                            id: *id,
                            depth: if nested { 2 } else { 1 },
                        })
                    }
                    TreeRow::Dir {
                        path,
                        count,
                        size_bytes,
                        ..
                    } => Some(TreeEntry::Dir {
                        path,
                        count,
                        size_bytes,
                    }),
                })
                .collect();
            let ordered = tree
                .iter()
                .filter_map(|entry| match entry {
                    TreeEntry::Member { id, .. } => Some(*id),
                    TreeEntry::Dir { .. } => None,
                })
                .collect();
            let aggregate = GroupAggregate::of(members.iter().map(|(_, task)| *task));
            let view = group_sort_view(
                group_id,
                meta,
                &aggregate,
                members.iter().map(|(_, task)| *task),
            );
            let expanded = !self.query.is_empty() || self.expanded_groups.contains(group_id);
            self.group_rows.push(GroupRow {
                id: group_id.to_owned(),
                members: ordered,
                view,
                aggregate,
                expanded,
                tree,
            });
        }
        let prefs = &self.prefs;
        let groups = &self.group_rows;
        let mut order: Vec<usize> = (0..groups.len()).collect();
        order.sort_by(|&left, &right| prefs.compare(&groups[left].view, &groups[right].view));
        let mut entities = Vec::with_capacity(top.len() + order.len());
        let mut start = 0;
        for ix in order {
            let proxy = &groups[ix].view;
            let at = start
                + top[start..]
                    .partition_point(|(_, task)| prefs.compare(task, proxy) == Ordering::Less);
            entities.extend(top[start..at].iter().map(|(id, _)| Entity::Task(*id)));
            entities.push(Entity::Group(ix));
            start = at;
        }
        entities.extend(top[start..].iter().map(|(id, _)| Entity::Task(*id)));
        entities
    }

    /// 输出一个顶层实体的可见行：组行之后，展开时按目录树列出目录行与成员（折叠目录只留目录行）。
    pub(super) fn push_entity(&self, visible: &mut Vec<VisibleRow>, entity: Entity) {
        let ix = match entity {
            Entity::Task(id) => {
                visible.push(VisibleRow::Task(id));
                return;
            }
            Entity::Group(ix) => ix,
        };
        visible.push(VisibleRow::Group(ix));
        let Some(group) = self.group_rows.get(ix).filter(|group| group.expanded) else {
            return;
        };
        let collapsed_dirs = self.collapsed_dirs.get(&group.id);
        let mut hidden = false;
        for entry in &group.tree {
            match entry {
                TreeEntry::Dir {
                    path,
                    count,
                    size_bytes,
                } => {
                    hidden = collapsed_dirs.is_some_and(|dirs| dirs.contains(path));
                    visible.push(VisibleRow::GroupDir {
                        group: ix,
                        path: path.clone(),
                        count: *count,
                        size_bytes: *size_bytes,
                        collapsed: hidden,
                    });
                }
                TreeEntry::Member { id, depth } => {
                    if !(hidden && *depth > 1) {
                        visible.push(VisibleRow::Member {
                            id: *id,
                            depth: *depth,
                        });
                    }
                }
            }
        }
    }

    /// 顶层实体的分桶：组用代理视图，按类型分组时取成员中最多的分类。
    pub(super) fn entity_bucket(
        &self,
        entity: Entity,
        local: &[DownloadTaskView],
        remote: &[DownloadTaskView],
    ) -> Option<GroupBucket> {
        match entity {
            Entity::Task(id) => row_at(local, remote, id).map(|task| self.group_bucket(task)),
            Entity::Group(ix) => {
                let group = self.group_rows.get(ix)?;
                Some(if self.prefs.group_by == ViewGroupBy::Type {
                    self.type_bucket(&self.dominant_category(group, local, remote))
                } else {
                    self.group_bucket(&group.view)
                })
            }
        }
    }

    /// 实体计入分桶头的任务数：组行按其在本视图内的成员数计。
    pub(super) fn entity_task_count(&self, entity: Entity) -> usize {
        match entity {
            Entity::Task(_) => 1,
            Entity::Group(ix) => self
                .group_rows
                .get(ix)
                .map_or(0, |group| group.members.len()),
        }
    }

    /// 成员中数量最多的分类；并列取字节合计大者，再并列取分类规则靠前者。
    fn dominant_category(
        &self,
        group: &GroupRow,
        local: &[DownloadTaskView],
        remote: &[DownloadTaskView],
    ) -> String {
        let mut tally: HashMap<&str, (usize, u64)> = HashMap::new();
        for task in group
            .members
            .iter()
            .filter_map(|id| row_at(local, remote, *id))
        {
            let entry = tally.entry(self.categories.category_of(task)).or_default();
            entry.0 += 1;
            entry.1 += task.size_bytes;
        }
        let rank = |id: &str| {
            self.categories
                .rules()
                .iter()
                .position(|rule| rule.dto.id == id)
                .unwrap_or(usize::MAX)
        };
        tally
            .into_iter()
            .max_by(|(left_id, left), (right_id, right)| {
                left.0
                    .cmp(&right.0)
                    .then_with(|| left.1.cmp(&right.1))
                    .then_with(|| rank(right_id).cmp(&rank(left_id)))
            })
            .map(|(id, _)| id.to_owned())
            .unwrap_or_default()
    }

    /// 纯内容更新（进度 / 速度）不重算可见行：组行聚合值按成员最新内容就地刷新。
    pub(super) fn refresh_group_rows(&mut self) {
        if self.group_rows.is_empty() {
            return;
        }
        let store = Rc::clone(&self.store);
        let local = store.local();
        let remote = store.remote();
        for group in &mut self.group_rows {
            let Some(meta) = self.groups.get(&group.id) else {
                continue;
            };
            let members = group
                .members
                .iter()
                .filter_map(|id| row_at(&local, &remote, *id));
            group.aggregate = GroupAggregate::of(members.clone());
            group.view = group_sort_view(&group.id, meta, &group.aggregate, members);
        }
    }

    /// 任务所在组的显示名命中搜索词。
    pub(super) fn group_name_matches(&self, task: &DownloadTaskView) -> bool {
        !task.group_id.is_empty()
            && self
                .groups
                .get(&task.group_id)
                .is_some_and(|group| group.name_fold.contains(&self.query))
    }

    /// 全组（不受筛选影响）成员计数：组命令的可用性据此判定。
    pub(super) fn whole_group_counts(&self, group_id: &str) -> GroupCounts {
        let local = self.store.local();
        GroupAggregate::of(local.iter().filter(|task| task.group_id == group_id)).counts
    }

    fn group_ix(&self, group_id: &str) -> Option<usize> {
        self.group_rows
            .iter()
            .position(|group| group.id == group_id)
    }

    /// 组在本视图内的成员 key（排除正在删除的）。
    fn group_member_keys(&self, ix: usize) -> Vec<RowKey> {
        self.group_rows.get(ix).map_or_else(Vec::new, |group| {
            group
                .members
                .iter()
                .filter_map(|id| self.store.row(*id).map(|row| row.key.clone()))
                .filter(|key| !self.is_pending_delete(key))
                .collect()
        })
    }

    /// 组行的选中态：成员全选 / 部分选中 / 未选。
    pub(super) fn group_check_state(&self, ix: usize) -> CheckState {
        let Some(group) = self.group_rows.get(ix) else {
            return CheckState::Unchecked;
        };
        let selected = group
            .members
            .iter()
            .filter(|id| {
                self.store
                    .row(**id)
                    .is_some_and(|row| self.selected_tasks.contains(&row.key))
            })
            .count();
        if selected == 0 {
            CheckState::Unchecked
        } else if selected == group.members.len() {
            CheckState::Checked
        } else {
            CheckState::Indeterminate
        }
    }

    /// 单击组行：普通单击选中整组并聚焦（停靠面板显示组概览）；Cmd/Ctrl 单击把整组并入 /
    /// 移出选区；Shift 单击从锚点选到该组最后一个成员。
    pub(crate) fn select_group(&mut self, group_id: &str, modifiers: SelectModifiers) {
        let Some(ix) = self.group_ix(group_id) else {
            return;
        };
        let keys = self.group_member_keys(ix);
        let Some(last) = keys.last().cloned() else {
            return;
        };
        if modifiers.shift && self.selection_anchor.is_some() {
            self.select_task(last, modifiers);
            return;
        }
        if modifiers.toggle {
            if keys.iter().all(|key| self.selected_tasks.contains(key)) {
                for key in &keys {
                    self.selected_tasks.remove(key);
                }
            } else {
                self.selected_tasks.extend(keys);
            }
            self.multi_select = true;
            self.focused_group = None;
        } else {
            self.selected_tasks.clear();
            self.selected_tasks.extend(keys);
            self.multi_select = false;
            self.focused_group = Some(group_id.to_owned());
        }
        self.selection_anchor = self.selected_tasks.contains(&last).then_some(last);
    }

    /// 组行复选框：未处于多选模式时以整组开启多选，已在多选模式时整组切换。
    pub(super) fn toggle_group_multi_select(&mut self, group_id: &str) {
        let Some(ix) = self.group_ix(group_id) else {
            return;
        };
        let keys = self.group_member_keys(ix);
        let Some(last) = keys.last().cloned() else {
            return;
        };
        if !self.in_multi_select() {
            self.selected_tasks.clear();
            self.selected_tasks.extend(keys);
        } else if keys.iter().all(|key| self.selected_tasks.contains(key)) {
            for key in &keys {
                self.selected_tasks.remove(key);
            }
        } else {
            self.selected_tasks.extend(keys);
        }
        self.multi_select = true;
        self.focused_group = None;
        self.selection_anchor = self.selected_tasks.contains(&last).then_some(last);
    }

    /// 右键组行：组未被整组选中时先按普通单击选中它，菜单与停靠面板都作用于该组。
    pub(super) fn select_group_for_context_menu(&mut self, group_id: &str) {
        if let Some(ix) = self.group_ix(group_id)
            && self.group_check_state(ix) != CheckState::Checked
        {
            self.select_group(group_id, SelectModifiers::default());
        }
    }

    /// 停靠面板应显示的组：经组行单击聚焦，且选区恰好是该组在本视图内的成员。
    pub(crate) fn focused_group(&self) -> Option<&str> {
        let group_id = self.focused_group.as_deref()?;
        let group = self.group_rows.iter().find(|group| group.id == group_id)?;
        (group.members.len() == self.selected_tasks.len()
            && group.members.iter().all(|id| {
                self.store
                    .row(*id)
                    .is_some_and(|row| self.selected_tasks.contains(&row.key))
            }))
        .then_some(group_id)
    }

    pub(super) fn toggle_group_expanded(&mut self, group_id: &str) {
        if !self.expanded_groups.remove(group_id) {
            self.expanded_groups.insert(group_id.to_owned());
        }
        self.view_dirty = true;
    }

    fn toggle_group_dir(&mut self, group_id: &str, path: &str) {
        let dirs = self.collapsed_dirs.entry(group_id.to_owned()).or_default();
        if !dirs.remove(path) {
            dirs.insert(path.to_owned());
        }
        self.view_dirty = true;
    }

    /// 组行容器：与任务行同高同底色；选中外观 = 本视图内成员全部选中。单击选中整组，
    /// 双击展开 / 折叠。
    pub(super) fn render_group_tr(
        &self,
        row_ix: usize,
        ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let theme = active_theme(cx);
        let row_height = self.prefs.density.row_height(theme.density());
        let row = div().id(("download-group-folder", row_ix)).h(row_height);
        let Some(group) = self.group_rows.get(ix) else {
            return row;
        };
        let selected = self.group_check_state(ix) == CheckState::Checked;
        let group_id = group.id.clone();
        let hover = theme.extended().colors.row_hover;
        row.group(ROW_GROUP)
            .relative()
            .child(div().absolute().inset_0().bg(theme.tokens().colors.surface))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(SELECTED_INSET_X))
                    .right(px(SELECTED_INSET_X))
                    .rounded(theme.components().task_row_radius)
                    .when(selected, |this| this.bg(theme.tokens().colors.accent))
                    .when(!selected, |this| {
                        this.group_hover(ROW_GROUP, move |style| style.bg(hover))
                    }),
            )
            .on_click(cx.listener(move |table, event: &ClickEvent, _, cx| {
                let modifiers = event.modifiers();
                let delegate = table.delegate_mut();
                if event.click_count() == 2 && !modifiers.secondary(cx) && !modifiers.shift {
                    delegate.toggle_group_expanded(&group_id);
                    delegate.refresh_view();
                } else {
                    delegate.select_group(&group_id, SelectModifiers::from_event(modifiers, cx));
                }
                cx.notify();
            }))
    }

    /// 目录行容器：单击切换该目录折叠；不参与选择。
    pub(super) fn render_group_dir_tr(
        &self,
        row_ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let theme = active_theme(cx);
        let row_height = self.prefs.density.row_height(theme.density());
        let row = div().id(("download-group-dir", row_ix)).h(row_height);
        let Some(VisibleRow::GroupDir { group, path, .. }) = self.visible.get(row_ix) else {
            return row;
        };
        let Some(group_id) = self.group_rows.get(*group).map(|group| group.id.clone()) else {
            return row;
        };
        let path = path.clone();
        let hover = theme.extended().colors.row_hover;
        row.group(ROW_GROUP)
            .relative()
            .cursor_pointer()
            .child(div().absolute().inset_0().bg(theme.tokens().colors.surface))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(SELECTED_INSET_X))
                    .right(px(SELECTED_INSET_X))
                    .rounded(theme.components().task_row_radius)
                    .group_hover(ROW_GROUP, move |style| style.bg(hover)),
            )
            .on_click(cx.listener(move |table, _: &ClickEvent, _, cx| {
                let delegate = table.delegate_mut();
                delegate.toggle_group_dir(&group_id, &path);
                delegate.refresh_view();
                cx.notify();
            }))
    }

    /// 组行单元格：选择列为文件夹图标 / 复选框，文件名列为展开箭头 + 组名 + 计数行，
    /// 状态列为聚合状态 + 完成数，协议 / 队列列成员一致时才显示，其余列取聚合值。
    pub(super) fn render_group_td(
        &self,
        row_ix: usize,
        col_ix: usize,
        ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let Some(group) = self.group_rows.get(ix) else {
            return div().into_any_element();
        };
        if col_ix == 0 {
            return self.render_group_select_cell(row_ix, ix, cx);
        }
        let Some(kind) = self.shown_column(col_ix).map(|column| column.kind) else {
            return div().into_any_element();
        };
        let theme = active_theme(cx);
        let muted = theme.tokens().colors.muted_foreground;
        let tertiary = theme.extended().colors.text_tertiary;
        match kind {
            DownloadColumnKind::FileName => self.render_group_name_cell(row_ix, group, cx),
            DownloadColumnKind::Status => self.render_group_status_cell(group, cx),
            DownloadColumnKind::Protocol => {
                match self.common_member_value(group, |task| task.protocol.label().to_owned()) {
                    Some(label) => meta_cell(label, muted, false, cx),
                    None => meta_cell("—", tertiary, false, cx),
                }
            }
            DownloadColumnKind::Queue => {
                match self.common_member_value(group, |task| task.queue_id.clone()) {
                    Some(queue_id) => meta_cell(self.queue_label(&queue_id), muted, false, cx),
                    None => meta_cell("—", tertiary, false, cx),
                }
            }
            DownloadColumnKind::Progress => self.render_group_sparkline(group, cx),
            kind => self.task_cell(kind, &group.view, cx),
        }
    }

    /// 成员在某字段上全部相同则返回该值。
    fn common_member_value(
        &self,
        group: &GroupRow,
        value: impl Fn(&DownloadTaskView) -> String,
    ) -> Option<String> {
        let mut common: Option<String> = None;
        for id in &group.members {
            let row = self.store.row(*id)?;
            let current = value(&row);
            match &common {
                Some(existing) if *existing != current => return None,
                Some(_) => {}
                None => common = Some(current),
            }
        }
        common
    }

    fn render_group_select_cell(
        &self,
        row_ix: usize,
        ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let Some(group) = self.group_rows.get(ix) else {
            return div().into_any_element();
        };
        let theme = active_theme(cx);
        let hover_border = theme.tokens().colors.foreground.opacity(0.7);
        let pinned = self.in_multi_select();
        let state = if pinned {
            self.group_check_state(ix)
        } else {
            CheckState::Unchecked
        };
        let check = check_mark(state, cx);
        let group_id = group.id.clone();
        let toggle = cx.listener(move |table, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            table.delegate_mut().toggle_group_multi_select(&group_id);
            cx.notify();
        });
        if !pinned {
            let icon = group_folder_icon(
                group.expanded,
                group.members.len(),
                self.file_icon_size(cx),
                cx,
            );
            let tooltip = self.strings.select_task_tooltip.clone();
            return div()
                .id(("download-group-select-cell", row_ix))
                .group(SELECT_CELL_GROUP)
                .size_full()
                .relative()
                .cursor_pointer()
                .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                .child(
                    h_flex()
                        .absolute()
                        .inset_0()
                        .justify_center()
                        .group_hover(SELECT_CELL_GROUP, |style| style.invisible())
                        .child(icon),
                )
                .child(
                    h_flex()
                        .absolute()
                        .inset_0()
                        .justify_center()
                        .invisible()
                        .group_hover(SELECT_CELL_GROUP, |style| style.visible())
                        .child(check.group_hover(SELECT_CELL_GROUP, move |style| {
                            style.border_color(hover_border)
                        })),
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(toggle)
                .into_any_element();
        }
        div()
            .size_full()
            .relative()
            .child(
                h_flex()
                    .absolute()
                    .inset_0()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id(("download-group-multi-select", row_ix))
                            .p(px(4.))
                            .cursor_pointer()
                            .child(check.when(state != CheckState::Checked, |mark| {
                                mark.hover(move |style| style.border_color(hover_border))
                            }))
                            .on_click(toggle),
                    ),
            )
            .into_any_element()
    }

    /// 展开箭头槽（一级缩进宽）：点击只切换展开，不改选区。
    fn expand_toggle(
        &self,
        id: (&'static str, usize),
        expanded: bool,
        on_toggle: impl Fn(&mut DownloadTableDelegate) + 'static,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let theme = active_theme(cx);
        let extended = theme.extended();
        div()
            .id(id)
            .flex_none()
            .w(px(TREE_INDENT))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |table, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                let delegate = table.delegate_mut();
                on_toggle(delegate);
                delegate.refresh_view();
                cx.notify();
            }))
            .child(
                Icon::new(if expanded {
                    FluxIcon::ChevronDown
                } else {
                    FluxIcon::ChevronRight
                })
                .size(extended.icon.sm)
                .text_color(extended.colors.text_tertiary),
            )
    }

    /// 组名列：展开箭头 + 组名（中等字重）+ 计数行；紧凑密度计数行与组名同行，宽松密度
    /// 组名与计数行之间加聚合进度条（已完成省略）。
    fn render_group_name_cell(
        &self,
        row_ix: usize,
        group: &GroupRow,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let group_id = group.id.clone();
        let chevron = self.expand_toggle(
            ("download-group-expand", row_ix),
            group.expanded,
            move |delegate| delegate.toggle_group_expanded(&group_id),
            cx,
        );
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let name = div()
            .min_w_0()
            .truncate()
            .text_size(tokens.typography.sm.size)
            .line_height(tokens.typography.sm.line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(tokens.colors.foreground)
            .child(SharedString::from(group.view.name.clone()));
        let counts = group_counts_line(
            self.strings
                .group_count_segments(&group.aggregate.counts, group.aggregate.eta_seconds),
            cx,
        )
        .text_size(tokens.typography.xs.size)
        .line_height(tokens.typography.xs.line_height);
        let body = if self.prefs.density.two_line() {
            v_flex()
                .flex_1()
                .min_w_0()
                .justify_center()
                .child(name)
                .child(counts)
        } else {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(tokens.spacing.sm)
                .child(name)
                .child(counts.flex_1())
        };
        h_flex()
            .size_full()
            .min_w_0()
            .child(chevron)
            .child(body)
            .into_any_element()
    }

    /// 组状态列：聚合状态（下载中带合计速度）+ 双行密度第二行 `完成数/总数`。
    fn render_group_status_cell(&self, group: &GroupRow, cx: &App) -> AnyElement {
        let theme = active_theme(cx);
        let typography = &theme.tokens().typography;
        let state = group.aggregate.state();
        let label = if state == TaskState::Downloading && group.aggregate.speed > 0 {
            SharedString::from(format!("{}/s", format_bytes(group.aggregate.speed)))
        } else {
            self.strings.state_label(state)
        };
        let counts = &group.aggregate.counts;
        v_flex()
            .size_full()
            .min_w_0()
            .justify_center()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(typography.xs.size)
                    .line_height(typography.xs.line_height)
                    .text_color(status_color(state, cx))
                    .child(label),
            )
            .when(self.prefs.density.two_line(), |this| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(typography.xs.size)
                        .line_height(typography.xs.line_height)
                        .text_color(theme.extended().colors.text_tertiary)
                        .child(
                            self.strings
                                .format_group_done_of_total(counts.done, counts.total),
                        ),
                )
            })
            .into_any_element()
    }

    /// 组行悬停操作（挂在右侧留白列上，贴可视区右缘）：全部暂停 / 全部恢复（按全组判定）、
    /// 打开组文件夹；停靠面板未打开时加「详情」（聚焦该组并打开面板）。
    pub(super) fn render_group_actions(
        &self,
        ix: usize,
        group: &GroupRow,
        cx: &App,
    ) -> Option<AnyElement> {
        let host = self.host.as_ref()?;
        let counts = self.whole_group_counts(&group.id);
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let (muted, foreground) = (tokens.colors.muted_foreground, tokens.colors.foreground);
        let pressed = extended.colors.nav_selected;
        type GroupAction =
            fn(&mut DownloadView, String, &mut gpui::Window, &mut Context<DownloadView>);
        let mut actions: Vec<(&'static str, FluxIcon, SharedString, GroupAction)> = Vec::new();
        if counts.any_active() {
            actions.push((
                "download-group-action-pause",
                FluxIcon::Pause,
                self.strings.group_pause_all.clone(),
                |view, id, _, cx| view.group_pause_all(id, cx),
            ));
        } else if counts.any_resumable() {
            actions.push((
                "download-group-action-resume",
                FluxIcon::Play,
                self.strings.group_resume_all.clone(),
                |view, id, _, cx| view.group_resume_all(id, cx),
            ));
        }
        actions.push((
            "download-group-action-reveal",
            FluxIcon::FolderOpen,
            self.strings.group_open_folder.clone(),
            |view, id, _, cx| view.group_open_folder(id, cx),
        ));
        if !self.prefs.detail_open {
            actions.push((
                "download-group-action-detail",
                FluxIcon::PanelRight,
                self.strings.detail.clone(),
                |view, id, window, cx| view.show_group_detail(id, window, cx),
            ));
        }
        let selected = self.group_check_state(ix) == CheckState::Checked;
        let buttons = actions.into_iter().map(|(id, icon, tooltip, action)| {
            let host = host.clone();
            let group_id = group.id.clone();
            h_flex()
                .id(id)
                .flex_none()
                .size(px(ROW_ACTION_SIZE))
                .justify_center()
                .rounded(tokens.radius.sm)
                .cursor_pointer()
                .text_color(muted)
                .hover(move |style| style.bg(pressed).text_color(foreground))
                .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let group_id = group_id.clone();
                    let Ok(()) = host.update(cx, |view, cx| action(view, group_id, window, cx))
                    else {
                        // 视图已释放，结束这次回调而不再更新状态。
                        return;
                    };
                })
                .child(Icon::new(icon).size(extended.icon.md))
        });
        Some(wrap_row_action_strip(selected, buttons, cx))
    }

    /// 目录行单元格：选择列只画树轨，文件名列为展开箭头 + 文件夹图标 + 压缩路径 + 文件数与
    /// 大小（与组根成员同级），大小列为目录合计，其余列留空。
    pub(super) fn render_group_dir_td(
        &self,
        row_ix: usize,
        col_ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let Some(VisibleRow::GroupDir {
            group,
            path,
            count,
            size_bytes,
            collapsed,
        }) = self.visible.get(row_ix)
        else {
            return div().into_any_element();
        };
        let muted = active_theme(cx).tokens().colors.muted_foreground;
        let dir_icon = active_theme(cx).extended().icon.md;
        if col_ix == 0 {
            return h_flex()
                .size_full()
                .justify_center()
                .child(tree_rail(cx))
                .into_any_element();
        }
        let Some(kind) = self.shown_column(col_ix).map(|column| column.kind) else {
            return div().into_any_element();
        };
        match kind {
            DownloadColumnKind::FileName => {
                let Some(group_id) = self.group_rows.get(*group).map(|group| group.id.clone())
                else {
                    return div().into_any_element();
                };
                let toggle_path = path.clone();
                let chevron = self.expand_toggle(
                    ("download-group-dir-expand", row_ix),
                    !*collapsed,
                    move |delegate| delegate.toggle_group_dir(&group_id, &toggle_path),
                    cx,
                );
                let theme = active_theme(cx);
                let tokens = theme.tokens();
                let full_path = SharedString::from(path.clone());
                let meta = self.strings.format_group_dir_count(*count);
                h_flex()
                    .id(("download-group-dir-name", row_ix))
                    .size_full()
                    .min_w_0()
                    .child(chevron)
                    .child(
                        div().flex_none().mr(tokens.spacing.sm).child(
                            Icon::new(if *collapsed {
                                FluxIcon::Folder
                            } else {
                                FluxIcon::FolderOpen
                            })
                            .size(dir_icon)
                            .text_color(muted),
                        ),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(tokens.spacing.sm)
                            .text_size(tokens.typography.xs.size)
                            .line_height(tokens.typography.xs.line_height)
                            // 路径优先占满剩余宽度（至少保留一小段），文件数固定在右侧；深层路径
                            // 由 `compress_path` 只留末两段，完整路径见悬停提示。
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(DIR_PATH_MIN_WIDTH))
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tokens.colors.muted_foreground)
                                    .child(SharedString::from(compress_path(path))),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.extended().colors.text_tertiary)
                                    .child(meta),
                            ),
                    )
                    .tooltip(move |window, cx| tooltip_view(full_path.clone(), window, cx))
                    .into_any_element()
            }
            DownloadColumnKind::Size if *size_bytes > 0 => {
                meta_cell(format_bytes(*size_bytes), muted, true, cx)
            }
            _ => div().into_any_element(),
        }
    }
}

/// 组内成员 / 目录行选择列的树轨：一条居中的细竖线，对准组行文件夹图标的中线。
pub(super) fn tree_rail(cx: &App) -> Div {
    div()
        .w(px(1.))
        .h_full()
        .bg(active_theme(cx).extended().colors.hairline)
}

impl DownloadTableDelegate {
    /// 目录之下成员的额外缩进：宽 = 展开箭头槽 + 目录图标，细竖线对准目录行文件夹图标的中线
    /// （其后的间距由成员行的 gap 提供）。
    pub(super) fn dir_member_indent(&self, cx: &App) -> Div {
        let icon = active_theme(cx).extended().icon.md;
        div()
            .relative()
            .flex_none()
            .h_full()
            .w(px(TREE_INDENT) + icon)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(TREE_INDENT) + icon / 2.)
                    .child(tree_rail(cx)),
            )
    }

    /// 组行进度列：成员柱状图（下载中 / 排队 / 暂停 / 失败 / 完成各自着色，高度 = 成员进度）
    /// + 整组百分比；已完成也显示（全满）。柱数受列宽限制，超过时按目录树顺序等距抽样。
    fn render_group_sparkline(&self, group: &GroupRow, cx: &App) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = &theme.extended().colors;
        let compact = self.prefs.density == ViewDensity::Compact;
        let (bar, area) = if compact {
            (SPARK_BAR_COMPACT, SPARK_HEIGHT_COMPACT)
        } else {
            (SPARK_BAR, SPARK_HEIGHT)
        };
        let text_scale = f32::from(theme.text_extent(1.));
        let column_width = self
            .columns
            .iter()
            .find(|column| column.kind == DownloadColumnKind::Progress)
            .map_or(
                DownloadColumnKind::Progress.min_width() * text_scale,
                |column| column.kind.display_width(column.width, text_scale),
            );
        let label_width = theme.text_extent(PROGRESS_LABEL_WIDTH);
        let available =
            (column_width - 2. * CELL_PADDING_X - f32::from(label_width) - PROGRESS_GAP).max(0.);
        let fit = ((available + SPARK_GAP) / (bar + SPARK_GAP)).floor() as usize;
        let bars = sparkline_sample(group.members.len(), SPARKLINE_MAX_BARS.min(fit))
            .into_iter()
            .filter_map(|ix| group.members.get(ix))
            .filter_map(|id| {
                self.store
                    .row(*id)
                    .map(|row| (row.progress, row.state, row.preparing))
            })
            .map(|(progress, state, preparing)| {
                let color = match state {
                    // 状态文字色多为灰阶（完成 / 排队退为三级文字），柱状图改用语义色才看得出状态。
                    TaskState::Completed => colors.success,
                    TaskState::Failed => colors.status_failed,
                    TaskState::Paused => colors.warning,
                    TaskState::Pending if !preparing => colors.progress_track,
                    TaskState::Downloading | TaskState::Pending => colors.progress_fill,
                };
                div()
                    .flex_none()
                    .w(px(bar))
                    .h(px(area * sparkline_height(progress, state)).max(px(2.)))
                    .rounded(px(1.5))
                    .bg(color)
            });
        h_flex()
            .size_full()
            .min_w_0()
            .gap(px(PROGRESS_GAP))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h(px(area))
                    .items_end()
                    .gap(px(SPARK_GAP))
                    .overflow_hidden()
                    .children(bars),
            )
            .child(
                div()
                    .flex_none()
                    .w(label_width)
                    .text_right()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .font_features(tabular_numbers())
                    .text_color(tokens.colors.muted_foreground)
                    .child(percent_label(group.aggregate.progress)),
            )
            .into_any_element()
    }
}

/// 柱状图柱宽 / 柱间距 / 柱高区（舒适与宽松；紧凑更矮更窄），对齐旧 Flutter 组行。
const SPARK_BAR: f32 = 5.;
const SPARK_BAR_COMPACT: f32 = 4.;
const SPARK_GAP: f32 = 2.;
const SPARK_HEIGHT: f32 = 18.;
const SPARK_HEIGHT_COMPACT: f32 = 14.;
/// 目录行路径最少保留的宽度（文件名列再窄也不会被文件数挤没）。
const DIR_PATH_MIN_WIDTH: f32 = 48.;
