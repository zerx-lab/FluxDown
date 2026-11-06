//! 任务表 / 队列位置表的分页载入。
//!
//! 全量快照不再内嵌任务：`system.snapshot` 与替换事件只带 [`crate::DaemonSnapshot::task_total`]
//! / `queue_total`，表经 `system.taskPage` 分页拉取，任何一帧都不随任务数线性增长。
//!
//! # 一致性
//!
//! 一轮载入从某个基准帧（快照或宣告重载的事件，序号 `base`）开始：
//!
//! 1. 基准帧之后到达的所有帧先缓冲、不应用；
//! 2. 逐页拉取（游标：任务 = 上一页最后一个任务 id，队列 = 上一页最后一个 `order`；都与
//!    翻页期间的增删无关，不会像下标游标那样错位），每页带服务端当时的 `(epoch, sequence)`；
//! 3. 全部页到齐后装入表，再按 [`crate::judge_frame`] 以 `base` 为游标回放缓冲帧。
//!
//! 页读取发生在 `base` 之后的不同时刻，所以表里的行可能已含部分后续变更；回放仍然收敛：
//! 任务事件都是「整行 / 绝对值」写入，回放完全部后续帧后每个任务的状态都取自它最后一次
//! 被触及的帧，没被触及的任务取自页内容；翻页后才新增的任务由 `TaskChanged` 补齐，已被
//! 翻过又删除的任务由删除事件移除。任务按 id 唯一，页内容天然无重复。

use serde::{Deserialize, Serialize};

use crate::daemon::{QueueOrderEntryDto, TaskDto};
use crate::event::{DaemonSnapshot, EventFrame};

/// 每页默认行数（约 1.6 MB / 页，远低于 WebSocket 单帧上限）。
pub const TASK_PAGE_DEFAULT_LIMIT: usize = 2_000;
/// 每页行数硬上限：请求超过时服务端按它截断。
pub const TASK_PAGE_MAX_LIMIT: usize = 5_000;
/// 载入期间最多缓冲的事件帧数；超过即放弃本轮、整体重同步。
pub const TABLE_SYNC_BACKLOG_LIMIT: usize = 65_536;

/// 分页的表。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum TableSection {
    /// 任务表，按 `taskId` 升序，游标 = 上一页最后一个任务 id。
    Tasks,
    /// 队列位置表，按 `order` 升序，游标 = 上一页最后一个 `order` 的十进制文本。
    Queue,
}

/// `system.taskPage` 参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TablePageParams {
    pub section: TableSection,
    /// 上一页的 `nextCursor`；缺省 = 从头开始。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// 每页行数；缺省 [`TASK_PAGE_DEFAULT_LIMIT`]，超过 [`TASK_PAGE_MAX_LIMIT`] 按上限截断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// `daemon.task.list` 参数（可省略）。省略或两项都缺省 = 取全部任务，任务数超过
/// [`TASK_PAGE_MAX_LIMIT`] 时服务端以 `invalidArgument` 拒绝（避免一次返回整张大表）；给出
/// `limit` / `cursor` 则按 `taskId` 升序取 `cursor`（上一页最后一个 `taskId`）之后至多 `limit`
/// 个任务，返回数不足 `limit` 即末页。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// `system.taskPage` 应答：一页行，加读取它时服务端的事件位置。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TablePage {
    pub epoch: String,
    /// 读取本页时服务端的事件序号（只作诊断与一致性校验，回放仍以基准帧序号为准）。
    pub sequence: u64,
    pub section: TableSection,
    /// 该表此刻的总行数。
    pub total: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<QueueOrderEntryDto>,
    /// 下一页游标；缺省 = 已到末页。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// 页参数无效（游标无法解析）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidPageCursor;

impl TablePage {
    /// 从投影 `snapshot`（位于 `(epoch, sequence)`）取一页。
    ///
    /// # Errors
    ///
    /// 队列游标不是十进制整数时返回 [`InvalidPageCursor`]。
    pub fn build(
        snapshot: &DaemonSnapshot,
        epoch: &str,
        sequence: u64,
        params: &TablePageParams,
    ) -> Result<Self, InvalidPageCursor> {
        let limit = params
            .limit
            .map_or(TASK_PAGE_DEFAULT_LIMIT, |limit| {
                usize::try_from(limit).unwrap_or(TASK_PAGE_MAX_LIMIT)
            })
            .clamp(1, TASK_PAGE_MAX_LIMIT);
        let mut page = Self {
            epoch: epoch.to_owned(),
            sequence,
            section: params.section,
            total: 0,
            tasks: Vec::new(),
            queue: Vec::new(),
            next_cursor: None,
        };
        match params.section {
            TableSection::Tasks => {
                page.total = u64::try_from(snapshot.tasks.len()).unwrap_or(u64::MAX);
                (page.tasks, page.next_cursor) =
                    snapshot.task_page(params.cursor.as_deref(), limit);
            }
            TableSection::Queue => {
                page.total = u64::try_from(snapshot.queue_positions.len()).unwrap_or(u64::MAX);
                let cursor = params
                    .cursor
                    .as_deref()
                    .map(str::parse::<i64>)
                    .transpose()
                    .map_err(|_| InvalidPageCursor)?;
                (page.queue, page.next_cursor) = snapshot.queue_page(cursor, limit);
            }
        }
        Ok(page)
    }
}

/// 载入过程的失败：调用方一律放弃本轮并整体重同步（重连 / 重新拉快照）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableSyncError {
    /// 页所属 epoch 与基准帧不同：服务端已重启。
    EpochChanged,
    /// 页的分区与请求不符。
    UnexpectedSection,
    /// 服务端返回了与请求相同的游标：翻页不前进。
    Stalled,
    /// 缓冲帧超过 [`TABLE_SYNC_BACKLOG_LIMIT`]。
    BacklogOverflow,
}

#[derive(Debug)]
struct Section<T> {
    wanted: bool,
    done: bool,
    cursor: Option<String>,
    rows: Vec<T>,
}

impl<T> Section<T> {
    fn new(total: Option<u64>) -> Self {
        Self {
            wanted: total.is_some(),
            done: total.is_none(),
            cursor: None,
            rows: Vec::new(),
        }
    }

    fn accept(
        &mut self,
        rows: Vec<T>,
        requested: Option<&str>,
        next: Option<String>,
    ) -> Result<(), TableSyncError> {
        if next.is_some() && next.as_deref() == requested {
            return Err(TableSyncError::Stalled);
        }
        self.rows.extend(rows);
        self.done = next.is_none();
        self.cursor = next;
        Ok(())
    }
}

/// 载入完成后的产物（[`TableSync::into_parts`]）。
#[derive(Debug, Default)]
pub struct LoadedTables {
    /// `Some` = 任务表已载入（按 id 升序）。
    pub tasks: Option<Vec<TaskDto>>,
    /// `Some` = 队列位置表已载入（按 `order` 升序）。
    pub queue: Option<Vec<QueueOrderEntryDto>>,
    /// 载入期间缓冲的帧（含基准帧之前已覆盖的，回放时由 `judge_frame` 过滤）。
    pub backlog: Vec<EventFrame>,
}

/// 一轮分页载入的状态机；传输无关，调用方负责收发请求与帧。
#[derive(Debug)]
pub struct TableSync {
    epoch: String,
    base_sequence: u64,
    tasks: Section<TaskDto>,
    queue: Section<QueueOrderEntryDto>,
    requested: Option<(TableSection, Option<String>)>,
    backlog: Vec<EventFrame>,
}

impl TableSync {
    /// 基准帧位于 `(epoch, base_sequence)`；两个总数都为 `None` 时无需载入，返回 `None`。
    #[must_use]
    pub fn new(
        epoch: impl Into<String>,
        base_sequence: u64,
        tasks_total: Option<u64>,
        queue_total: Option<u64>,
    ) -> Option<Self> {
        if tasks_total.is_none() && queue_total.is_none() {
            return None;
        }
        Some(Self {
            epoch: epoch.into(),
            base_sequence,
            tasks: Section::new(tasks_total),
            queue: Section::new(queue_total),
            requested: None,
            backlog: Vec::new(),
        })
    }

    /// 按投影里的待载入标记建立载入；无标记返回 `None`。
    #[must_use]
    pub fn for_snapshot(
        epoch: impl Into<String>,
        base_sequence: u64,
        snapshot: &DaemonSnapshot,
    ) -> Option<Self> {
        Self::new(
            epoch,
            base_sequence,
            snapshot.task_total,
            snapshot.queue_total,
        )
    }

    /// 基准帧序号：缓冲帧回放时的初始游标。
    #[must_use]
    pub fn base_sequence(&self) -> u64 {
        self.base_sequence
    }

    #[must_use]
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// 下一个要发的页请求；全部载入完毕返回 `None`。任务表先于队列位置表。
    #[must_use]
    pub fn next_request(&mut self) -> Option<TablePageParams> {
        let (section, cursor) = if self.tasks.wanted && !self.tasks.done {
            (TableSection::Tasks, self.tasks.cursor.clone())
        } else if self.queue.wanted && !self.queue.done {
            (TableSection::Queue, self.queue.cursor.clone())
        } else {
            self.requested = None;
            return None;
        };
        self.requested = Some((section, cursor.clone()));
        Some(TablePageParams {
            section,
            cursor,
            limit: None,
        })
    }

    /// 吸收最近一次 [`Self::next_request`] 对应的页。
    ///
    /// # Errors
    ///
    /// epoch 变化、分区不符或游标不前进时返回 [`TableSyncError`]。
    pub fn accept(&mut self, page: TablePage) -> Result<(), TableSyncError> {
        if page.epoch != self.epoch {
            return Err(TableSyncError::EpochChanged);
        }
        let Some((section, cursor)) = self.requested.take() else {
            return Err(TableSyncError::UnexpectedSection);
        };
        if page.section != section {
            return Err(TableSyncError::UnexpectedSection);
        }
        match section {
            TableSection::Tasks => {
                self.tasks
                    .accept(page.tasks, cursor.as_deref(), page.next_cursor)
            }
            TableSection::Queue => {
                self.queue
                    .accept(page.queue, cursor.as_deref(), page.next_cursor)
            }
        }
    }

    /// 缓冲载入期间到达的帧。
    ///
    /// # Errors
    ///
    /// 超过 [`TABLE_SYNC_BACKLOG_LIMIT`] 返回 [`TableSyncError::BacklogOverflow`]。
    pub fn push_frame(&mut self, frame: EventFrame) -> Result<(), TableSyncError> {
        if self.backlog.len() >= TABLE_SYNC_BACKLOG_LIMIT {
            return Err(TableSyncError::BacklogOverflow);
        }
        self.backlog.push(frame);
        Ok(())
    }

    /// 全部页到齐后取出产物。
    #[must_use]
    pub fn into_parts(self) -> LoadedTables {
        LoadedTables {
            tasks: self.tasks.wanted.then_some(self.tasks.rows),
            queue: self.queue.wanted.then_some(self.queue.rows),
            backlog: self.backlog,
        }
    }

    /// 把载入的表装入 `snapshot`（清除待载入标记），返回需按 `judge_frame` 回放的缓冲帧。
    #[must_use]
    pub fn finish(self, snapshot: &mut DaemonSnapshot) -> Vec<EventFrame> {
        let loaded = self.into_parts();
        if let Some(tasks) = loaded.tasks {
            snapshot.install_tasks(tasks);
        }
        if let Some(queue) = loaded.queue {
            snapshot.install_queue(queue);
        }
        loaded.backlog
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{QueuePositionDto, WsServerMsg};
    use crate::event::{DaemonEvent, FrameVerdict, ServiceEvent, apply_daemon_event, judge_frame};

    fn task(id: &str, status: i32) -> TaskDto {
        TaskDto {
            task_id: id.to_owned(),
            url: format!("https://example.com/{id}.bin"),
            file_name: format!("{id}.bin"),
            save_dir: "/downloads".to_owned(),
            status,
            downloaded_bytes: 0,
            total_bytes: 2048,
            error_message: String::new(),
            created_at: "1700000000".to_owned(),
            proxy_url: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            file_missing: false,
            completed_at: String::new(),
            file_hash: String::new(),
            referrer: String::new(),
            group_id: String::new(),
            rss_source_id: String::new(),
            origin_url: String::new(),
            auto_route: String::new(),
            source_bytes: crate::TaskSourceBytesDto::default(),
            queue_order: 0,
            uploaded_bytes: 0,
            uploaded_at_completion: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
            seed_ratio_limit_milli: -2,
            seed_post_ratio_limit_milli: -2,
            seed_time_limit_minutes: -2,
            seed_inactive_time_limit_minutes: -2,
            seed_upload_limit_bps: 0,
            bt_sequential: false,
            bt_first_last_pieces_first: false,
        }
    }

    /// 内存里的「服务端」：一个投影加一个事件序号。
    struct Server {
        snapshot: DaemonSnapshot,
        sequence: u64,
    }

    impl Server {
        fn new(ids: impl IntoIterator<Item = String>) -> Self {
            let tasks: Vec<TaskDto> = ids.into_iter().map(|id| task(&id, 2)).collect();
            Self {
                snapshot: DaemonSnapshot {
                    tasks,
                    ..DaemonSnapshot::default()
                },
                sequence: 10,
            }
        }

        /// 应用内部事件并返回对应的线上帧。
        fn publish(&mut self, event: DaemonEvent) -> EventFrame {
            apply_daemon_event(&mut self.snapshot, &event);
            self.sequence += 1;
            EventFrame {
                epoch: "e".to_owned(),
                sequence: self.sequence,
                previous: None,
                event: ServiceEvent::Daemon(event.into_wire()),
            }
        }

        fn header(&self) -> (u64, DaemonSnapshot) {
            (self.sequence, self.snapshot.paged_header())
        }

        fn page(&self, params: &TablePageParams) -> TablePage {
            TablePage::build(&self.snapshot, "e", self.sequence, params).expect("page")
        }
    }

    /// 客户端：头 + 分页载入 + 回放。`between_pages` 在每页取完后让服务端继续变化，返回这
    /// 期间到达的帧。
    fn load(
        server: &mut Server,
        limit: u32,
        mut between_pages: impl FnMut(&mut Server, usize) -> Vec<EventFrame>,
    ) -> DaemonSnapshot {
        let (base, mut snapshot) = server.header();
        let mut sync = TableSync::for_snapshot("e", base, &snapshot).expect("tables to load");
        let mut index = 0;
        while let Some(mut params) = sync.next_request() {
            params.limit = Some(limit);
            let page = server.page(&params);
            sync.accept(page).expect("accept");
            for frame in between_pages(server, index) {
                sync.push_frame(frame).expect("buffer");
            }
            index += 1;
        }
        let mut cursor = sync.base_sequence();
        for frame in sync.finish(&mut snapshot) {
            match judge_frame("e", cursor, &frame) {
                FrameVerdict::Accept => {
                    cursor = frame.sequence;
                    let ServiceEvent::Daemon(event) = &frame.event else {
                        panic!("daemon frame");
                    };
                    apply_daemon_event(&mut snapshot, event);
                }
                FrameVerdict::Skip => {}
                FrameVerdict::Resync => panic!("unexpected gap"),
            }
        }
        snapshot
    }

    fn sorted_ids(snapshot: &DaemonSnapshot) -> Vec<String> {
        let mut ids: Vec<String> = snapshot.tasks.iter().map(|t| t.task_id.clone()).collect();
        ids.sort();
        ids
    }

    #[test]
    fn header_carries_totals_instead_of_rows() {
        let mut server = Server::new((0..7).map(|i| format!("t{i}")));
        server.snapshot.queue_positions = vec![QueuePositionDto {
            task_id: "t0".to_owned(),
            position: 1,
            order: 5,
        }];
        let (_, header) = server.header();
        assert!(header.tasks.is_empty() && header.queue_positions.is_empty());
        assert_eq!((header.task_total, header.queue_total), (Some(7), Some(1)));
        assert!(header.tables_loading());
        let json = serde_json::to_value(&header).expect("json");
        assert!(json["tasks"].as_array().is_some_and(Vec::is_empty));
        assert!(json["queuePositions"].as_array().is_some_and(Vec::is_empty));
        assert_eq!(json["taskTotal"], 7);
    }

    #[test]
    fn paging_without_concurrent_changes_returns_every_task_once() {
        let mut server = Server::new((0..103).map(|i| format!("t{i:03}")));
        let snapshot = load(&mut server, 10, |_, _| Vec::new());
        assert_eq!(snapshot.tasks.len(), 103);
        assert_eq!(snapshot.task_total, None);
        assert_eq!(sorted_ids(&snapshot), sorted_ids(&server.snapshot));
    }

    #[test]
    fn changes_arriving_between_pages_are_neither_lost_nor_duplicated() {
        let mut server = Server::new((0..100).map(|i| format!("t{i:03}")));
        let snapshot = load(&mut server, 10, |server, page| match page {
            // 已翻过的任务被删：靠回放删除事件移除，翻页位置不错位。
            0 => vec![server.publish(DaemonEvent::TasksDeleted {
                task_ids: vec!["t000".to_owned(), "t005".to_owned()],
            })],
            // 排在游标之前的新任务：页里不会再出现，只能靠 TaskChanged 补齐。
            1 => vec![server.publish(DaemonEvent::TaskChanged(task("a-before", 0)))],
            // 游标之后的新任务：页和事件各来一次，不得重复。
            2 => vec![server.publish(DaemonEvent::TaskChanged(task("z-after", 0)))],
            // 未翻到的任务被改 / 被删。
            3 => vec![
                server.publish(DaemonEvent::TaskChanged(task("t090", 3))),
                server.publish(DaemonEvent::Engine(WsServerMsg::TaskProgress {
                    task_id: "t095".to_owned(),
                    status: 4,
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    file_name: String::new(),
                    save_dir: String::new(),
                    url: String::new(),
                    error_message: "deleted".to_owned(),
                    uploaded_bytes: 0,
                    seeding_status: 0,
                    seeding_message: String::new(),
                    seeding_time_secs: 0,
                    speed: 0,
                    upload_speed: 0,
                })),
            ],
            // 已翻过的任务状态变化。
            4 => vec![server.publish(DaemonEvent::TaskChanged(task("t012", 1)))],
            _ => Vec::new(),
        });
        assert_eq!(sorted_ids(&snapshot), sorted_ids(&server.snapshot));
        let mut seen = std::collections::HashSet::new();
        assert!(
            snapshot
                .tasks
                .iter()
                .all(|t| seen.insert(t.task_id.as_str()))
        );
        for task_id in ["t000", "t005", "t095"] {
            assert!(snapshot.task(task_id).is_none(), "{task_id} must be gone");
        }
        assert_eq!(snapshot.task("t090").map(|t| t.status), Some(3));
        assert_eq!(snapshot.task("t012").map(|t| t.status), Some(1));
        assert!(snapshot.task("a-before").is_some() && snapshot.task("z-after").is_some());
        assert_eq!(snapshot.tasks.len(), server.snapshot.tasks.len());
    }

    #[test]
    fn queue_positions_page_by_order_and_survive_concurrent_reorders() {
        let mut server = Server::new((0..30).map(|i| format!("t{i:02}")));
        server.snapshot.queue_positions = (0..30)
            .map(|i| QueuePositionDto {
                task_id: format!("t{i:02}"),
                position: i + 1,
                order: i64::from(i) * 10,
            })
            .collect();
        let snapshot = load(&mut server, 7, |server, page| {
            if page != 3 {
                return Vec::new();
            }
            vec![
                server.publish(DaemonEvent::Engine(WsServerMsg::QueueOrderChanged {
                    removed: vec!["t00".to_owned(), "t20".to_owned()],
                    added: vec![
                        crate::QueueOrderEntryDto {
                            task_id: "t29".to_owned(),
                            order: 5,
                        },
                        crate::QueueOrderEntryDto {
                            task_id: "t05".to_owned(),
                            order: 1_000,
                        },
                    ],
                })),
            ]
        });
        assert_eq!(snapshot.queue_total, None);
        assert_eq!(snapshot.queue_positions, server.snapshot.queue_positions);
    }

    #[test]
    fn queue_page_never_splits_equal_orders() {
        let snapshot = DaemonSnapshot {
            queue_positions: ["a", "b", "c", "d", "e"]
                .iter()
                .enumerate()
                .map(|(index, id)| QueuePositionDto {
                    task_id: (*id).to_owned(),
                    position: i32::try_from(index + 1).expect("position"),
                    order: if index < 3 { 10 } else { 20 },
                })
                .collect(),
            ..DaemonSnapshot::default()
        };
        let (first, next) = snapshot.queue_page(None, 1);
        assert_eq!(first.len(), 3, "the tie group stays on one page");
        let (second, end) = snapshot.queue_page(next.as_deref().and_then(|c| c.parse().ok()), 1);
        assert_eq!(second.len(), 2);
        assert_eq!(end, None);
    }

    #[test]
    fn epoch_change_and_stalled_cursor_abort_the_round() {
        let server = Server::new((0..5).map(|i| format!("t{i}")));
        let (base, snapshot) = server.header();
        let mut sync = TableSync::for_snapshot("e", base, &snapshot).expect("sync");
        let mut params = sync.next_request().expect("request");
        params.limit = Some(2);
        let mut page = server.page(&params);
        page.epoch = "other".to_owned();
        assert_eq!(sync.accept(page), Err(TableSyncError::EpochChanged));

        let mut sync = TableSync::for_snapshot("e", base, &snapshot).expect("sync");
        let mut params = sync.next_request().expect("request");
        params.limit = Some(2);
        let first = server.page(&params);
        sync.accept(first.clone()).expect("first page");
        let _ = sync.next_request().expect("second request");
        let mut stalled = server.page(&TablePageParams {
            cursor: None,
            ..params
        });
        stalled.next_cursor = first.next_cursor.clone();
        assert_eq!(sync.accept(stalled), Err(TableSyncError::Stalled));
    }

    #[test]
    fn oversized_tables_are_announced_instead_of_inlined() {
        let tasks = (0..=super::super::event::TABLE_INLINE_LIMIT)
            .map(|i| task(&format!("t{i}"), 0))
            .collect();
        let event = DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks }).into_wire();
        assert!(matches!(
            event,
            DaemonEvent::TablesReloading {
                tasks: Some(total),
                queue: None
            } if total == 2_001
        ));
        let small = DaemonEvent::Engine(WsServerMsg::TasksSnapshot {
            tasks: vec![task("a", 0)],
        })
        .into_wire();
        assert!(matches!(
            small,
            DaemonEvent::Engine(WsServerMsg::TasksSnapshot { .. })
        ));

        // 客户端投影收到宣告后进入加载态，空表不被当成完整列表。
        let mut client = DaemonSnapshot {
            tasks: vec![task("old", 0)],
            ..DaemonSnapshot::default()
        };
        apply_daemon_event(&mut client, &event);
        assert!(client.tables_loading() && client.tasks.is_empty());
        assert_eq!(client.task_count(), 2_001);
    }

    /// 二十万任务：快照头与每一页（含硬上限页）的序列化大小都远低于 16 MiB。
    #[test]
    fn two_hundred_thousand_tasks_never_need_a_frame_over_16_mib() {
        const LIMIT: usize = 16 * 1024 * 1024;
        let mut server =
            Server::new((0..200_000).map(|i| format!("{i:08x}-0000-4000-8000-{i:012x}")));
        server.snapshot.queue_positions = (0..200_000)
            .map(|i: i64| QueuePositionDto {
                task_id: format!("{i:08x}-0000-4000-8000-{i:012x}"),
                position: i32::try_from(i + 1).expect("position"),
                order: 1_700_000_000_000 + i,
            })
            .collect();
        let (_, header) = server.header();
        let header_bytes = serde_json::to_vec(&header).expect("header").len();
        assert!(header_bytes < LIMIT, "header {header_bytes}");

        let reload = DaemonEvent::Engine(WsServerMsg::QueuePositionsChanged {
            positions: server.snapshot.queue_positions.clone(),
        })
        .into_wire();
        assert!(serde_json::to_vec(&reload).expect("reload").len() < 1024);

        let mut tasks_seen = 0;
        let mut worst = 0;
        let mut cursor = None;
        loop {
            let page = server.page(&TablePageParams {
                section: TableSection::Tasks,
                cursor: cursor.take(),
                limit: Some(u32::try_from(TASK_PAGE_MAX_LIMIT).expect("limit")),
            });
            worst = worst.max(serde_json::to_vec(&page).expect("page").len());
            tasks_seen += page.tasks.len();
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(tasks_seen, 200_000);
        let mut queue_seen = 0;
        let mut cursor = None;
        loop {
            let page = server.page(&TablePageParams {
                section: TableSection::Queue,
                cursor: cursor.take(),
                limit: Some(u32::try_from(TASK_PAGE_MAX_LIMIT).expect("limit")),
            });
            worst = worst.max(serde_json::to_vec(&page).expect("page").len());
            queue_seen += page.queue.len();
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(queue_seen, 200_000);
        assert!(worst < LIMIT, "largest page {worst} bytes");
    }
}
