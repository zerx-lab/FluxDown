//! 共享任务行存储：controller 增量写、表格 / 详情按下标读，不逐事件全量克隆。

use std::{
    cell::{Cell, Ref, RefCell},
    collections::HashMap,
    rc::Rc,
};

use super::{
    CategoryIndex, DownloadTaskView, RowKey, TaskState, counts::SidebarCounts,
    view_prefs::ViewSortKey,
};

/// 变更日志容量：超过即整体作废（读者回退到整表重算），日志不随事件数无界增长。
const CHANGE_LOG_LIMIT: usize = 4096;

/// 一次行变更对排序 / 筛选视图的影响；读者按 [`TaskStore::changes_since`] 增量维护排序结果，
/// 而不是每次变更都整表重排。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowChange {
    /// 行内容变化：`view` = 筛选 / 搜索 / 分组读取的字段变了；`sort` 是 `ViewSortKey::CYCLE`
    /// 下标位掩码，置位的排序键读取的值变了。
    Updated { ix: usize, view: bool, sort: u16 },
    /// 末尾追加了一行（下标 = 追加时的长度）。
    Appended(usize),
    /// `swap_remove(ix)`：原末位 `last` 的行（`last != ix` 时）搬到了 `ix`。
    Removed { ix: usize, last: usize },
}

/// 增量维护中的侧栏计数及其对应的分类索引（索引换了就整体重算）。
#[derive(Default)]
struct BoundCounts {
    categories: Option<Rc<CategoryIndex>>,
    counts: SidebarCounts,
}

/// 行在存储中的位置（随删除会变；跨帧身份用 [`RowKey`]）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum RowId {
    Local(usize),
    Remote(usize),
}

#[derive(Default)]
pub(crate) struct TaskStore {
    local: RefCell<Vec<DownloadTaskView>>,
    remote: RefCell<Vec<DownloadTaskView>>,
    /// 本地 task_id → `local` 下标。
    index: RefCell<HashMap<String, usize>>,
    generation: Cell<u64>,
    /// 行集合 / 下标布局变化计数（增删、整表换成不同的行）。只改行内容时不变，
    /// 表格据此判断 [`RowId`] 是否仍指向同一任务、能否沿用上次的行顺序。
    structure: Cell<u64>,
    /// 影响侧栏计数的变化（状态、队列、设备、文件名 / 分类、行增删）；进度等不影响计数的
    /// 内容更新不动它，侧栏计数据此缓存。
    counts: Cell<u64>,
    /// 引擎待启动队列的排序键（升序）。行只存自己的键，展示序号 = 键在此集合里的名次，
    /// 队首出队只改这里一处，不重写其余排队行。
    queue_keys: RefCell<Vec<i64>>,
    /// 任务表 / 队列位置表仍在分页载入：值为服务端报告的任务总数。载入期间 `local` 只是占位，
    /// 不是完整列表——读者不得据它显示「暂无任务」、统计计数或推断「任务已被删除」。
    loading: Cell<Option<usize>>,
    /// 行变更日志（见 [`RowChange`]）；`changes[0]` 的位置是 `changes_base`。整表替换 / 重排 /
    /// 超限时作废：`changes_base` 越过当时的末尾，旧位置的读者得到 `None`。
    changes: RefCell<Vec<RowChange>>,
    changes_base: Cell<u64>,
    /// 侧栏计数：随行增删 / 状态变化增量维护，不随事件整表重扫。
    bound_counts: RefCell<BoundCounts>,
    /// 本地「全部暂停」作用对象数（等待 / 下载中 / 准备中）与各队列里的暂停任务数，随行变更
    /// 增量维护。
    pausable: Cell<usize>,
    resumable_seeds: Cell<usize>,
    paused_by_queue: RefCell<HashMap<String, usize>>,
}

impl TaskStore {
    /// 载入态（见字段说明）；`None` = 表完整。
    pub(crate) fn loading(&self) -> Option<usize> {
        self.loading.get()
    }

    /// 进入 / 退出载入态。变化时使计数与整体视图失效，读者据此重绘。
    pub(crate) fn set_loading(&self, loading: Option<usize>) {
        if self.loading.replace(loading) != loading {
            self.counts.set(self.counts.get().wrapping_add(1));
            self.bump();
        }
    }

    pub(crate) fn local(&self) -> Ref<'_, [DownloadTaskView]> {
        Ref::map(self.local.borrow(), Vec::as_slice)
    }

    pub(crate) fn remote(&self) -> Ref<'_, [DownloadTaskView]> {
        Ref::map(self.remote.borrow(), Vec::as_slice)
    }

    pub(crate) fn row(&self, id: RowId) -> Option<Ref<'_, DownloadTaskView>> {
        match id {
            RowId::Local(ix) => Ref::filter_map(self.local.borrow(), |rows| rows.get(ix)).ok(),
            RowId::Remote(ix) => Ref::filter_map(self.remote.borrow(), |rows| rows.get(ix)).ok(),
        }
    }

    pub(crate) fn row_id(&self, key: &RowKey) -> Option<RowId> {
        match key {
            RowKey::Local(id) => self.find_local(id).map(RowId::Local),
            RowKey::Remote(id) => self
                .remote
                .borrow()
                .iter()
                .position(|row| row.key.task_id() == id)
                .map(RowId::Remote),
        }
    }

    pub(crate) fn get(&self, key: &RowKey) -> Option<Ref<'_, DownloadTaskView>> {
        self.row_id(key).and_then(|id| self.row(id))
    }

    pub(crate) fn find_local(&self, task_id: &str) -> Option<usize> {
        self.index.borrow().get(task_id).copied()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.get()
    }

    pub(crate) fn structure_generation(&self) -> u64 {
        self.structure.get()
    }

    /// 侧栏计数的失效计数（行增删、状态 / 队列 / 设备 / 分类相关字段变化）；计数本身已由
    /// [`Self::sidebar_counts`] 增量维护，这里只留给测试断言哪些更新不触及计数。
    #[cfg(test)]
    pub(crate) fn counts_generation(&self) -> u64 {
        self.counts.get()
    }

    /// 排队序号（1 起）：行排序键在有序键集合中的名次，二分求得；不在队列中为 0。
    pub(crate) fn queue_rank(&self, row: &DownloadTaskView) -> u32 {
        let Some(key) = row.queue_key else {
            return 0;
        };
        let rank = self
            .queue_keys
            .borrow()
            .partition_point(|probe| *probe < key)
            + 1;
        u32::try_from(rank).unwrap_or(u32::MAX)
    }

    /// 整表替换排队键（入参无需有序）。名次变化不动 `generation`：行内容没变，调用方
    /// 负责通知读者重绘。
    pub(crate) fn replace_queue_keys(&self, mut keys: Vec<i64>) {
        keys.sort_unstable();
        *self.queue_keys.borrow_mut() = keys;
    }

    /// 排队键增量：先移除 `removed` 再插入 `added`（均无需有序）。少量变化二分定位后做有序
    /// 数组移动，O(Δ·log P) 加数组移动；批量变化改为一趟归并，避免 Δ 次整段搬移。
    pub(crate) fn apply_queue_keys(&self, removed: &mut [i64], added: &mut [i64]) {
        const MERGE_THRESHOLD: usize = 64;
        let mut keys = self.queue_keys.borrow_mut();
        if removed.len() + added.len() <= MERGE_THRESHOLD {
            for key in removed.iter() {
                if let Ok(ix) = keys.binary_search(key) {
                    keys.remove(ix);
                }
            }
            for key in added.iter() {
                let ix = keys.partition_point(|probe| probe < key);
                keys.insert(ix, *key);
            }
            return;
        }
        removed.sort_unstable();
        added.sort_unstable();
        let mut removed = removed.iter().copied().peekable();
        let mut added = added.iter().copied().peekable();
        let mut merged = Vec::with_capacity(keys.len() + added.len());
        for &key in keys.iter() {
            // 不在集合里的待移除键直接跳过；相等的键每次只抵消一个。
            while removed.next_if(|probe| *probe < key).is_some() {}
            if removed.next_if_eq(&key).is_some() {
                continue;
            }
            while let Some(next) = added.next_if(|probe| *probe < key) {
                merged.push(next);
            }
            merged.push(key);
        }
        merged.extend(added);
        *keys = merged;
    }

    /// 变更日志当前末尾位置；读者记下它，之后用 [`Self::changes_since`] 取增量。
    pub(crate) fn change_log_end(&self) -> u64 {
        self.changes_base.get() + self.changes.borrow().len() as u64
    }

    /// `position` 之后的行变更；日志已在该位置之后作废（整表替换 / 重排 / 超限）返回 `None`，
    /// 读者须整表重算。
    pub(crate) fn changes_since(&self, position: u64) -> Option<Ref<'_, [RowChange]>> {
        let start = usize::try_from(position.checked_sub(self.changes_base.get())?).ok()?;
        Ref::filter_map(self.changes.borrow(), |entries| entries.get(start..)).ok()
    }

    fn log_change(&self, change: RowChange) {
        let mut log = self.changes.borrow_mut();
        if log.len() >= CHANGE_LOG_LIMIT {
            // 只丢最旧的一半：紧跟着刷新的读者位置仍在日志内，不会被迫整表重排；落后太多
            // （位置已被丢弃）的读者回退到整表重算。
            let dropped = log.len() / 2;
            log.drain(..dropped);
            self.changes_base
                .set(self.changes_base.get() + dropped as u64);
        }
        log.push(change);
    }

    /// 作废日志：现有位置之后的读者必须整表重算。
    fn reset_changes(&self) {
        let mut log = self.changes.borrow_mut();
        self.changes_base
            .set(self.changes_base.get() + log.len() as u64 + 1);
        log.clear();
    }

    /// 本地「全部暂停」的作用对象数。
    pub(crate) fn pausable_count(&self) -> usize {
        self.pausable.get()
    }

    /// 本地「全部开始」会重新启动的用户停止做种任务数。
    pub(crate) fn resumable_seed_count(&self) -> usize {
        self.resumable_seeds.get()
    }

    /// 各队列里的暂停任务数（只含非零项）。
    pub(crate) fn paused_by_queue(&self) -> Ref<'_, HashMap<String, usize>> {
        self.paused_by_queue.borrow()
    }

    /// 侧栏计数；与 `categories` 绑定，之后随行变更增量维护。分类索引换了（或整表替换后）
    /// 才整表重算一次。
    pub(crate) fn sidebar_counts(&self, categories: &Rc<CategoryIndex>) -> Ref<'_, SidebarCounts> {
        let bound = self
            .bound_counts
            .borrow()
            .categories
            .as_ref()
            .is_some_and(|bound| Rc::ptr_eq(bound, categories));
        if !bound {
            let counts = SidebarCounts::compute(
                self.local
                    .borrow()
                    .iter()
                    .chain(self.remote.borrow().iter()),
                categories,
            );
            *self.bound_counts.borrow_mut() = BoundCounts {
                categories: Some(Rc::clone(categories)),
                counts,
            };
        }
        Ref::map(self.bound_counts.borrow(), |bound| &bound.counts)
    }

    fn adjust(cell: &Cell<usize>, add: bool) {
        let count = cell.get();
        cell.set(if add {
            count + 1
        } else {
            count.saturating_sub(1)
        });
    }

    /// 一行进出存储时更新增量聚合（暂停可作用数、队列暂停数、已绑定的侧栏计数）。
    fn account(&self, row: &DownloadTaskView, add: bool) {
        match row.state {
            TaskState::Pending | TaskState::Downloading => Self::adjust(&self.pausable, add),
            TaskState::Paused => {
                let mut paused = self.paused_by_queue.borrow_mut();
                if add {
                    *paused.entry(row.queue_id.clone()).or_default() += 1;
                } else if let Some(count) = paused.get_mut(&row.queue_id) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        paused.remove(&row.queue_id);
                    }
                }
            }
            TaskState::Completed => {
                if row.is_seeding() {
                    Self::adjust(&self.pausable, add);
                } else if row.is_resume_all_seed() {
                    Self::adjust(&self.resumable_seeds, add);
                }
            }
            TaskState::Failed => {}
        }
        let mut bound = self.bound_counts.borrow_mut();
        if let Some(categories) = bound.categories.clone() {
            if add {
                bound.counts.add(row, &categories);
            } else {
                bound.counts.remove(row, &categories);
            }
        }
    }

    /// 整表替换后重建本地聚合；侧栏计数解绑，下次读取时按新表重算。
    fn rebuild_aggregates(&self, rows: &[DownloadTaskView]) {
        self.pausable.set(0);
        self.resumable_seeds.set(0);
        self.paused_by_queue.borrow_mut().clear();
        *self.bound_counts.borrow_mut() = BoundCounts::default();
        for row in rows {
            self.account(row, true);
        }
    }

    fn invalidate_view_fields(&self) {
        // 筛选 / 分组字段是计数所读字段的超集：保守地一并失效。
        self.counts.set(self.counts.get().wrapping_add(1));
    }

    fn bump(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    fn bump_structure(&self) {
        self.structure.set(self.structure.get().wrapping_add(1));
        self.counts.set(self.counts.get().wrapping_add(1));
        self.bump();
    }

    /// 整表替换；行 key 与顺序不变时（快照 / 重连重建）只算内容变化。
    pub(crate) fn replace_local(&self, rows: Vec<DownloadTaskView>) {
        self.invalidate_view_fields();
        let same_rows = same_row_keys(&self.local.borrow(), &rows);
        if !same_rows {
            let index = rows
                .iter()
                .enumerate()
                .map(|(ix, row)| (row.key.task_id().to_owned(), ix))
                .collect();
            *self.index.borrow_mut() = index;
        }
        self.reset_changes();
        self.rebuild_aggregates(&rows);
        *self.local.borrow_mut() = rows;
        if same_rows {
            self.bump();
        } else {
            self.bump_structure();
        }
    }

    pub(crate) fn replace_remote(&self, rows: Vec<DownloadTaskView>) {
        self.invalidate_view_fields();
        let same_rows = same_row_keys(&self.remote.borrow(), &rows);
        self.reset_changes();
        {
            // 远程行很少：只调整它们对已绑定计数的贡献，不触发本地大表重算。
            let mut bound = self.bound_counts.borrow_mut();
            if let Some(categories) = bound.categories.clone() {
                for old in self.remote.borrow().iter() {
                    bound.counts.remove(old, &categories);
                }
                for new in &rows {
                    bound.counts.add(new, &categories);
                }
            }
        }
        *self.remote.borrow_mut() = rows;
        if same_rows {
            self.bump();
        } else {
            self.bump_structure();
        }
    }

    /// 覆盖单行（下标必须已存在）。
    pub(crate) fn set_local(&self, ix: usize, row: DownloadTaskView) {
        if let Some(slot) = self.local.borrow_mut().get_mut(ix) {
            let view = !same_view_fields(slot, &row);
            if view {
                self.invalidate_view_fields();
                self.account(slot, false);
                self.account(&row, true);
            }
            let mut sort = 0_u16;
            for key in ViewSortKey::CYCLE {
                if !key.same_value(slot, &row) {
                    sort |= key.change_bit();
                }
            }
            if view || sort != 0 {
                self.log_change(RowChange::Updated { ix, view, sort });
            }
            *slot = row;
            self.bump();
        }
    }

    /// 只改一行的排队键（进出队列的任务）：不重建整行，只在排队键参与排序时使排序失效。
    pub(crate) fn set_queue_key(&self, ix: usize, key: Option<i64>) {
        let mut rows = self.local.borrow_mut();
        let Some(row) = rows.get_mut(ix) else {
            return;
        };
        if row.queue_key == key {
            return;
        }
        row.queue_key = key;
        let mut sort = 0_u16;
        for sort_key in ViewSortKey::CYCLE {
            if sort_key.orders_by_queue_key(row) {
                sort |= sort_key.change_bit();
            }
        }
        if sort != 0 {
            self.log_change(RowChange::Updated {
                ix,
                view: false,
                sort,
            });
        }
        self.bump();
    }

    pub(crate) fn push_local(&self, row: DownloadTaskView) -> usize {
        let ix = self.append_local(row);
        self.bump_structure();
        ix
    }

    /// 追加一行但不记结构变化：批量追加后由调用方 [`Self::commit_appends`] 一次登记。
    pub(crate) fn append_local(&self, row: DownloadTaskView) -> usize {
        let mut rows = self.local.borrow_mut();
        let ix = rows.len();
        self.index
            .borrow_mut()
            .insert(row.key.task_id().to_owned(), ix);
        self.account(&row, true);
        rows.push(row);
        self.log_change(RowChange::Appended(ix));
        ix
    }

    /// 登记一批 [`Self::append_local`] 的结构变化（整批一次）。
    pub(crate) fn commit_appends(&self) {
        self.bump_structure();
    }

    /// Preserve authoritative snapshot order without replacing unchanged views.
    pub(crate) fn swap_local(&self, left: usize, right: usize) {
        let mut rows = self.local.borrow_mut();
        rows.swap(left, right);
        self.reset_changes();
        let mut index = self.index.borrow_mut();
        for position in [left, right] {
            let task_id = rows[position].key.task_id();
            if let Some(indexed) = index.get_mut(task_id) {
                *indexed = position;
            } else {
                eprintln!("repairing missing local task index during snapshot reorder");
                index.insert(task_id.to_owned(), position);
            }
        }
        self.bump_structure();
    }

    /// `swap_remove` 并修正被换入行的索引。
    pub(crate) fn swap_remove_local(&self, ix: usize) {
        if self.swap_remove_local_quiet(ix) {
            self.bump_structure();
        }
    }

    /// 同 [`Self::swap_remove_local`] 但不记结构变化：批量删除后由调用方
    /// [`Self::commit_removals`] 一次登记。返回是否真的移除了一行。
    pub(crate) fn swap_remove_local_quiet(&self, ix: usize) -> bool {
        let mut rows = self.local.borrow_mut();
        if ix >= rows.len() {
            return false;
        }
        let removed = rows.swap_remove(ix);
        self.account(&removed, false);
        let mut index = self.index.borrow_mut();
        index.remove(removed.key.task_id());
        if let Some(moved) = rows.get(ix) {
            index.insert(moved.key.task_id().to_owned(), ix);
        }
        self.log_change(RowChange::Removed {
            ix,
            last: rows.len(),
        });
        true
    }

    /// 登记一批 [`Self::swap_remove_local_quiet`] 的结构变化（整批一次）。
    pub(crate) fn commit_removals(&self) {
        self.bump_structure();
    }
}

fn same_row_keys(current: &[DownloadTaskView], next: &[DownloadTaskView]) -> bool {
    current.len() == next.len() && current.iter().zip(next).all(|(a, b)| a.key == b.key)
}

/// 覆盖当前所有筛选、搜索与分组读取的字段；不确定当前视图是否受影响时保守地失效。
fn same_view_fields(left: &DownloadTaskView, right: &DownloadTaskView) -> bool {
    left.key == right.key
        && left.state == right.state
        && left.source == right.source
        && left.queue_id == right.queue_id
        && left.name == right.name
        && left.name_fold == right.name_fold
        && left.file_extension == right.file_extension
        && left.url_fold == right.url_fold
        && left.site_fold == right.site_fold
        && left.site == right.site
        && left.referrer == right.referrer
        && left.protocol == right.protocol
        && left.created_at_secs == right.created_at_secs
        && left.group_id == right.group_id
        && left.to_device == right.to_device
        && left.seeding_status == right.seeding_status
        && left.file_missing == right.file_missing
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use fluxdown_protocol::{CustomCategoryDto, TaskDto};

    use super::{RowChange, TaskStore};
    use crate::model::{
        CategoryIndex, DownloadFilter, DownloadStatusFilter, DownloadTaskView, TaskState,
        counts::SidebarCounts,
    };

    fn row(id: &str, status: i32, queue: &str, name: &str) -> DownloadTaskView {
        let task: TaskDto = serde_json::from_value(serde_json::json!({
            "taskId": id, "url": "https://example.com/f", "fileName": name,
            "saveDir": "/tmp", "status": status, "downloadedBytes": 0, "totalBytes": 10,
            "errorMessage": "", "createdAt": "1", "proxyUrl": "", "queueId": queue,
            "checksum": ""
        }))
        .expect("task");
        let mut view = DownloadTaskView::local(&task, None, false);
        // 由 id + 名称确定性地派生做种状态 / 文件丢失，覆盖 0..=8 各取值。
        let hash = id.bytes().chain(name.bytes()).map(i32::from).sum::<i32>();
        view.seeding_status = hash % 9;
        view.file_missing = hash % 4 == 0;
        view
    }

    fn assert_counts_match_rescan(store: &TaskStore, categories: &Rc<CategoryIndex>) {
        let expected = SidebarCounts::compute(
            store.local().iter().chain(store.remote().iter()),
            categories,
        );
        let actual = store.sidebar_counts(categories);
        let mut category_ids: Vec<Option<String>> = vec![None, Some("missing".to_owned())];
        category_ids.extend(
            CustomCategoryDto::builtin_defaults()
                .into_iter()
                .map(|dto| Some(dto.id)),
        );
        for status in DownloadStatusFilter::ALL {
            for category in &category_ids {
                let filter = DownloadFilter {
                    status,
                    category: category.clone(),
                };
                assert_eq!(
                    actual.filter(&filter),
                    expected.filter(&filter),
                    "{filter:?}"
                );
            }
        }
        for queue in ["q0", "q1", "q2", "gone"] {
            assert_eq!(actual.queue(queue), expected.queue(queue), "{queue}");
        }
        let paused: usize = store.paused_by_queue().values().sum();
        let rows = store.local();
        assert_eq!(
            paused,
            rows.iter()
                .filter(|row| row.state == TaskState::Paused)
                .count()
        );
        assert_eq!(
            store.pausable_count(),
            rows.iter()
                .filter(|row| row.is_seeding()
                    || matches!(row.state, TaskState::Pending | TaskState::Downloading))
                .count()
        );
        assert_eq!(
            store.resumable_seed_count(),
            rows.iter().filter(|row| row.is_resume_all_seed()).count()
        );
    }

    #[test]
    fn incremental_counts_and_aggregates_match_a_rescan_through_row_churn() {
        let categories = Rc::new(CategoryIndex::from_dtos(
            CustomCategoryDto::builtin_defaults(),
        ));
        let store = TaskStore::default();
        let names = ["a.zip", "b.mp4", "c.pdf", "d.exe", "e"];
        store.replace_local(
            (0..40)
                .map(|ix| {
                    row(
                        &format!("t{ix}"),
                        ix % 6,
                        &format!("q{}", ix % 3),
                        names[ix as usize % names.len()],
                    )
                })
                .collect(),
        );
        // 绑定后才开始增量维护。
        assert_counts_match_rescan(&store, &categories);
        let mut random = 0x9e37_79b9_7f4a_7c15_u64;
        for step in 0..400 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let count = store.local().len();
            let ix = (random >> 32) as usize % count.max(1);
            match random % 5 {
                0 | 1 if count > 0 => {
                    let id = store.local()[ix].key.task_id().to_owned();
                    store.set_local(
                        ix,
                        row(
                            &id,
                            (random >> 8) as i32 % 6,
                            &format!("q{}", (random >> 16) % 3),
                            names[(random >> 24) as usize % names.len()],
                        ),
                    );
                }
                2 if count > 5 => store.swap_remove_local(ix),
                3 => {
                    store.push_local(row(&format!("n{step}"), 2, "q1", "x.zip"));
                }
                _ => {
                    // 只改进度字段：不影响任何聚合。
                    if count > 0 {
                        let mut changed = store.local()[ix].clone();
                        changed.downloaded_bytes += 1;
                        store.set_local(ix, changed);
                    }
                }
            }
            assert_counts_match_rescan(&store, &categories);
        }
    }

    #[test]
    fn replacing_remote_rows_adjusts_bound_counts_without_a_local_rescan() {
        let categories = Rc::new(CategoryIndex::from_dtos(
            CustomCategoryDto::builtin_defaults(),
        ));
        let store = TaskStore::default();
        store.replace_local(vec![row("a", 1, "q0", "a.zip"), row("b", 3, "q1", "b.zip")]);
        assert_counts_match_rescan(&store, &categories);
        let mut remote = row("r", 2, "", "r.zip");
        remote.key = crate::model::RowKey::Remote("r".into());
        remote.source = crate::model::TaskSource::Remote;
        remote.to_device = "device".into();
        store.replace_remote(vec![remote.clone()]);
        assert_counts_match_rescan(&store, &categories);
        assert_eq!(store.sidebar_counts(&categories).device("device"), 1);
        store.replace_remote(Vec::new());
        assert_counts_match_rescan(&store, &categories);
        assert_eq!(store.sidebar_counts(&categories).device("device"), 0);
    }

    #[test]
    fn change_log_records_relevant_updates_and_invalidates_on_replacement() {
        let store = TaskStore::default();
        store.replace_local(vec![row("a", 1, "q0", "a"), row("b", 2, "q0", "b")]);
        let start = store.change_log_end();
        assert!(store.changes_since(start).is_some_and(|log| log.is_empty()));

        // 只改进度：不是视图 / 排序变化，不入日志。
        let mut progressed = store.local()[0].clone();
        progressed.downloaded_bytes = 5;
        store.set_local(0, progressed);
        assert!(store.changes_since(start).is_some_and(|log| log.is_empty()));

        let mut done = store.local()[0].clone();
        done.state = TaskState::Completed;
        store.set_local(0, done);
        store.push_local(row("c", 0, "q0", "c"));
        store.swap_remove_local(0);
        let log = store.changes_since(start).expect("log");
        assert!(matches!(
            log[0],
            RowChange::Updated {
                ix: 0,
                view: true,
                ..
            }
        ));
        assert_eq!(log[1], RowChange::Appended(2));
        assert_eq!(log[2], RowChange::Removed { ix: 0, last: 2 });
        drop(log);

        // 整表替换作废此前的所有位置（包括恰好等于旧末尾的位置）。
        let end = store.change_log_end();
        store.replace_local(vec![row("z", 0, "q0", "z")]);
        assert!(store.changes_since(start).is_none());
        assert!(store.changes_since(end).is_none());
        assert!(store.changes_since(store.change_log_end()).is_some());
    }

    #[test]
    fn change_log_overflow_keeps_recent_positions_and_drops_stale_ones() {
        let store = TaskStore::default();
        store.replace_local(vec![row("a", 1, "q0", "a")]);
        let stale = store.change_log_end();
        for step in 0..(super::CHANGE_LOG_LIMIT * 2) {
            let mut flipped = store.local()[0].clone();
            flipped.state = if step % 2 == 0 {
                TaskState::Paused
            } else {
                TaskState::Downloading
            };
            if step == super::CHANGE_LOG_LIMIT * 2 - 8 {
                let recent = store.change_log_end();
                store.set_local(0, flipped);
                assert!(store.changes_since(stale).is_none());
                assert!(
                    store
                        .changes_since(recent)
                        .is_some_and(|log| log.len() == 1)
                );
                return;
            }
            store.set_local(0, flipped);
        }
    }
}
