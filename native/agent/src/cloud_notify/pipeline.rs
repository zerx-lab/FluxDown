//! 云端推送的纯逻辑：匹配过滤、隐私裁剪、有界队列与重试退避。
//!
//! 不碰网络与状态锁，便于单测覆盖契约 §5 的每条规则。

use std::collections::VecDeque;
use std::time::Duration;

use fluxdown_protocol::cloud_notify::CLOUD_NOTIFY_RECENT_DELIVERIES;
use fluxdown_protocol::{
    CloudNotifyDeliveriesPage, CloudNotifyDeliveriesParams, CloudNotifyDeliveryDto,
    CloudNotifyKindDto, CloudNotifyOverviewDto, CloudNotifyStateDto, TaskNoticeDto,
};
use serde::Serialize;

use crate::cloud::CloudError;
use crate::state::CloudNotifyPrefs;

/// 内存队列上限；满时丢最旧。
pub(super) const QUEUE_CAPACITY: usize = 256;
/// 单次 `POST /notifications/events` 的条数上限（云端限制 ≤ 50）。
pub(super) const BATCH_LIMIT: usize = 50;
/// 首条入队后攒批的等待时间。
pub(super) const BATCH_WINDOW: Duration = Duration::from_secs(1);
/// 网络 / 5xx 失败后的重试次数（不含首次）。
pub(super) const RETRY_LIMIT: u32 = 3;
/// 指数退避基数。
const RETRY_BASE: Duration = Duration::from_secs(2);

/// 上报给云端的单条事件（`POST /notifications/events` 的 `events[]` 元素）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WireEvent {
    pub delivery_id: String,
    pub event: String,
    pub timestamp_ms: i64,
    pub queue_id: String,
    pub queue_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<WireTask>,
}

/// 隐私裁剪后的任务字段：本地任务 ID 永不上报；`url` / `saveDir` 仅在对应开关打开时出现。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WireTask {
    pub file_name: String,
    pub total_bytes: i64,
    pub status: i32,
    pub error_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_dir: Option<String>,
}

/// 按本机隐私开关裁剪一条任务事件。
pub(super) fn trim_for_upload(notice: &TaskNoticeDto, prefs: CloudNotifyPrefs) -> WireEvent {
    WireEvent {
        delivery_id: notice.delivery_id.clone(),
        event: notice.event.clone(),
        timestamp_ms: notice.timestamp_ms,
        queue_id: notice.queue_id.clone(),
        queue_name: notice.queue_name.clone(),
        task: notice.task.as_ref().map(|task| WireTask {
            file_name: task.file_name.clone(),
            total_bytes: task.total_bytes,
            status: task.status,
            error_message: task.error_message.clone(),
            url: (prefs.include_url && !task.url.is_empty()).then(|| task.url.clone()),
            save_dir: (prefs.include_save_dir && !task.save_dir.is_empty())
                .then(|| task.save_dir.clone()),
        }),
    }
}

/// 事件是否值得上报：已登录 + 本设备上报开 + 套餐允许 + 至少一个渠道匹配（已启用 + 订阅该事件）。
///
/// `catalog` 为目录缓存：`Some(空)` = 管理员关闭了全部云端渠道，不上报；`None` = 尚未拉到，
/// 不据此拦截（概览里是否有匹配渠道仍然把关）。
/// `overview` 为缓存的云端概览；从未拉取成功（`None`）一律不上报。
/// FluxCloud 不认识的本机专属事件（`queue.started`）恒不上报。
pub(super) fn should_report(
    logged_in: bool,
    prefs: CloudNotifyPrefs,
    catalog: Option<&[CloudNotifyKindDto]>,
    overview: Option<&CloudNotifyOverviewDto>,
    event: &str,
) -> bool {
    if is_local_only_event(event) {
        return false;
    }
    if !logged_in || !prefs.reporting || catalog.is_some_and(<[CloudNotifyKindDto]>::is_empty) {
        return false;
    }
    let Some(overview) = overview else {
        return false;
    };
    overview.enabled
        && overview.channels.iter().any(|channel| {
            channel.enabled && channel.events.iter().any(|subscribed| subscribed == event)
        })
}

/// 仅在本机 webhook / 通知面使用、FluxCloud 不认识的事件；上传前一律丢弃。
fn is_local_only_event(event: &str) -> bool {
    event == "queue.started"
}

/// 有界 FIFO：满时丢最旧。
#[derive(Debug)]
pub(super) struct EventQueue<T> {
    items: VecDeque<T>,
    capacity: usize,
}

impl<T> EventQueue<T> {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity,
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.items.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.items.clear();
    }

    /// 入队；返回被挤掉的最旧条数（0 或 1）。
    pub(super) fn push(&mut self, item: T) -> usize {
        let mut dropped = 0;
        while self.items.len() >= self.capacity {
            self.items.pop_front();
            dropped += 1;
        }
        self.items.push_back(item);
        dropped
    }

    /// 取出最老的至多 `max` 条。
    pub(super) fn take_batch(&mut self, max: usize) -> Vec<T> {
        let count = max.min(self.items.len());
        self.items.drain(..count).collect()
    }
}

/// 第 `attempt`（从 0 起）次重试前的等待：2s、4s、8s。
pub(super) fn retry_delay(attempt: u32) -> Duration {
    RETRY_BASE.saturating_mul(1_u32 << attempt.min(8))
}

/// 投递记录第一页的请求参数（页大小 = `recent_deliveries` 上限）。
pub(super) fn first_page_params() -> CloudNotifyDeliveriesParams {
    CloudNotifyDeliveriesParams {
        limit: u32::try_from(CLOUD_NOTIFY_RECENT_DELIVERIES).ok(),
        before: None,
    }
}

/// 新的在前：`createdAt`（RFC 3339，云端格式固定）降序，同刻按 `id` 降序，保证顺序确定。
fn sort_deliveries(list: &mut [CloudNotifyDeliveryDto]) {
    list.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
}

/// 把 SSE `notify.delivery` 的增量按 `id` upsert 进列表，降序后截断到上限。
/// 返回是否因截断丢掉了记录（此时第一页游标已不再指向窗口末尾，需要重拉第一页校正）。
pub(super) fn merge_deliveries(
    list: &mut Vec<CloudNotifyDeliveryDto>,
    items: Vec<CloudNotifyDeliveryDto>,
) -> bool {
    for item in items {
        match list.iter_mut().find(|existing| existing.id == item.id) {
            Some(existing) => *existing = item,
            None => list.push(item),
        }
    }
    sort_deliveries(list);
    let truncated = list.len() > CLOUD_NOTIFY_RECENT_DELIVERIES;
    list.truncate(CLOUD_NOTIFY_RECENT_DELIVERIES);
    truncated
}

/// 第一页整页覆盖：记录与翻页游标都以云端为准（重连 / 刷新后纠正离线期间的变化）。
pub(super) fn set_first_page(state: &mut CloudNotifyStateDto, page: CloudNotifyDeliveriesPage) {
    let mut items = page.items;
    sort_deliveries(&mut items);
    items.truncate(CLOUD_NOTIFY_RECENT_DELIVERIES);
    state.recent_deliveries = items;
    state.recent_next_cursor = page.next_cursor;
}

/// 上报失败是否值得重试：网络失败与 5xx / 429；其余 4xx 重试没有意义。
pub(super) fn is_retryable(error: &CloudError) -> bool {
    error.unreachable || error.retryable
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{
        CloudNotifyChannelDto, CloudNotifyOverviewDto, TaskNoticeDto, TaskNoticeTaskDto,
    };

    use super::*;

    fn notice(event: &str) -> TaskNoticeDto {
        TaskNoticeDto {
            delivery_id: "d-1".to_owned(),
            event: event.to_owned(),
            timestamp_ms: 1_700_000_000_000,
            queue_id: "default".to_owned(),
            queue_name: "默认队列".to_owned(),
            task: Some(TaskNoticeTaskDto {
                id: "local-task-id".to_owned(),
                file_name: "ubuntu.iso".to_owned(),
                url: "https://example.com/secret/ubuntu.iso?token=abc".to_owned(),
                save_dir: "/home/me/Downloads".to_owned(),
                total_bytes: 42,
                status: 3,
                error_message: String::new(),
            }),
        }
    }

    fn channel(enabled: bool, events: &[&str]) -> CloudNotifyChannelDto {
        CloudNotifyChannelDto {
            id: "c1".to_owned(),
            kind: "email".to_owned(),
            enabled,
            events: events.iter().map(|value| (*value).to_owned()).collect(),
            ..CloudNotifyChannelDto::default()
        }
    }

    fn overview(enabled: bool, channels: Vec<CloudNotifyChannelDto>) -> CloudNotifyOverviewDto {
        CloudNotifyOverviewDto {
            enabled,
            channels,
            ..CloudNotifyOverviewDto::default()
        }
    }

    fn on() -> CloudNotifyPrefs {
        CloudNotifyPrefs {
            reporting: true,
            ..CloudNotifyPrefs::default()
        }
    }

    #[test]
    fn privacy_defaults_drop_url_and_save_dir_and_local_id() -> Result<(), serde_json::Error> {
        let wire = trim_for_upload(&notice("task.completed"), on());
        let task = wire.task.as_ref();
        assert_eq!(task.map(|t| t.file_name.as_str()), Some("ubuntu.iso"));
        assert_eq!(task.and_then(|t| t.url.as_deref()), None);
        assert_eq!(task.and_then(|t| t.save_dir.as_deref()), None);
        let json = serde_json::to_value(&wire)?;
        let task = &json["task"];
        assert!(task.get("url").is_none());
        assert!(task.get("saveDir").is_none());
        assert!(task.get("id").is_none(), "本地任务 ID 不得上报");
        assert_eq!(task["fileName"], "ubuntu.iso");
        assert_eq!(task["totalBytes"], 42);
        assert_eq!(task["status"], 3);
        assert_eq!(json["deliveryId"], "d-1");
        assert_eq!(json["queueName"], "默认队列");
        Ok(())
    }

    #[test]
    fn privacy_switches_are_independent() {
        let url_only = trim_for_upload(
            &notice("task.completed"),
            CloudNotifyPrefs {
                reporting: true,
                include_url: true,
                include_save_dir: false,
            },
        );
        let task = url_only.task.as_ref();
        assert!(task.and_then(|t| t.url.as_deref()).is_some());
        assert_eq!(task.and_then(|t| t.save_dir.as_deref()), None);

        let dir_only = trim_for_upload(
            &notice("task.completed"),
            CloudNotifyPrefs {
                reporting: true,
                include_url: false,
                include_save_dir: true,
            },
        );
        let task = dir_only.task.as_ref();
        assert_eq!(task.and_then(|t| t.url.as_deref()), None);
        assert_eq!(
            task.and_then(|t| t.save_dir.as_deref()),
            Some("/home/me/Downloads")
        );
    }

    #[test]
    fn queue_drained_has_no_task_payload() {
        let mut drained = notice("queue.drained");
        drained.task = None;
        let wire = trim_for_upload(&drained, on());
        assert!(wire.task.is_none());
    }

    /// FluxCloud 不认识 `queue.started`：即使渠道订阅里出现了这个名字也不上报，
    /// 同一份概览下已知事件照常上报。
    #[test]
    fn queue_started_is_never_reported_to_cloud() {
        let subscribed = overview(
            true,
            vec![channel(true, &["queue.started", "queue.drained"])],
        );
        assert!(!should_report(
            true,
            on(),
            None,
            Some(&subscribed),
            "queue.started"
        ));
        assert!(should_report(
            true,
            on(),
            None,
            Some(&subscribed),
            "queue.drained"
        ));
    }

    #[test]
    fn report_requires_login_switch_plan_and_matching_channel() {
        let matching = overview(true, vec![channel(true, &["task.completed"])]);
        let report = |logged_in, prefs, overview: Option<&CloudNotifyOverviewDto>, event| {
            should_report(logged_in, prefs, None, overview, event)
        };
        assert!(report(true, on(), Some(&matching), "task.completed"));

        // 未登录
        assert!(!report(false, on(), Some(&matching), "task.completed"));
        // 开关关（默认）
        assert!(!report(
            true,
            CloudNotifyPrefs::default(),
            Some(&matching),
            "task.completed"
        ));
        // 概览未拉取
        assert!(!report(true, on(), None, "task.completed"));
        // 套餐未开通
        let disabled = overview(false, vec![channel(true, &["task.completed"])]);
        assert!(!report(true, on(), Some(&disabled), "task.completed"));
        // 无渠道
        assert!(!report(
            true,
            on(),
            Some(&overview(true, Vec::new())),
            "task.completed"
        ));
        // 渠道已暂停
        let paused = overview(true, vec![channel(false, &["task.completed"])]);
        assert!(!report(true, on(), Some(&paused), "task.completed"));
        // 事件未订阅
        assert!(!report(true, on(), Some(&matching), "task.failed"));
    }

    #[test]
    fn empty_catalog_blocks_reporting_but_unknown_catalog_does_not() {
        let matching = overview(true, vec![channel(true, &["task.completed"])]);
        let kinds = [CloudNotifyKindDto {
            kind: "email".to_owned(),
            available: true,
        }];
        let report = |catalog: Option<&[CloudNotifyKindDto]>| {
            should_report(true, on(), catalog, Some(&matching), "task.completed")
        };
        assert!(report(None), "目录未知不拦截");
        assert!(report(Some(&kinds)));
        assert!(!report(Some(&[])), "目录为空 = 云端渠道全部关闭，不上报");
    }

    #[test]
    fn full_queue_drops_oldest() {
        let mut queue = EventQueue::new(QUEUE_CAPACITY);
        let mut dropped = 0;
        for index in 0..(QUEUE_CAPACITY + 10) {
            dropped += queue.push(index);
        }
        assert_eq!(dropped, 10);
        assert_eq!(queue.len(), QUEUE_CAPACITY);
        let batch = queue.take_batch(BATCH_LIMIT);
        assert_eq!(batch.len(), BATCH_LIMIT);
        assert_eq!(batch.first(), Some(&10), "最旧的 10 条已被丢弃");
        assert_eq!(batch.last(), Some(&(10 + BATCH_LIMIT - 1)));
        assert_eq!(queue.len(), QUEUE_CAPACITY - BATCH_LIMIT);
        queue.clear();
        assert!(queue.is_empty());
    }

    #[test]
    fn retry_backoff_doubles_from_two_seconds() {
        assert_eq!(retry_delay(0), Duration::from_secs(2));
        assert_eq!(retry_delay(1), Duration::from_secs(4));
        assert_eq!(retry_delay(2), Duration::from_secs(8));
    }

    #[test]
    fn only_transient_failures_are_retried() {
        assert!(is_retryable(&CloudError::network("reset".to_owned())));
        let server = CloudError {
            status: Some(503),
            code: None,
            message: String::new(),
            retryable: true,
            unreachable: false,
        };
        assert!(is_retryable(&server));
        let rejected = CloudError {
            status: Some(422),
            code: Some("notify_target_invalid".to_owned()),
            message: String::new(),
            retryable: false,
            unreachable: false,
        };
        assert!(!is_retryable(&rejected));
    }
}

#[cfg(test)]
mod delivery_tests {
    use fluxdown_protocol::{
        CloudNotifyDeliveriesPage, CloudNotifyDeliveryDto, CloudNotifyStateDto,
    };

    use super::*;

    fn delivery(id: &str, created_at: &str, status: &str) -> CloudNotifyDeliveryDto {
        CloudNotifyDeliveryDto {
            id: id.to_owned(),
            created_at: created_at.to_owned(),
            status: status.to_owned(),
            ..CloudNotifyDeliveryDto::default()
        }
    }

    fn ids(list: &[CloudNotifyDeliveryDto]) -> Vec<&str> {
        list.iter().map(|item| item.id.as_str()).collect()
    }

    #[test]
    fn upsert_replaces_by_id_and_keeps_newest_first() {
        let mut list = vec![
            delivery("b", "2026-10-09T12:00:02.000Z", "pending"),
            delivery("a", "2026-10-09T12:00:01.000Z", "sent"),
        ];
        // 状态变化：同 id 覆盖，位置不变。
        let truncated = merge_deliveries(
            &mut list,
            vec![delivery("b", "2026-10-09T12:00:02.000Z", "sent")],
        );
        assert!(!truncated);
        assert_eq!(ids(&list), ["b", "a"]);
        assert_eq!(list[0].status, "sent");
        // 新记录插到最前；乱序到达的旧记录排到后面。
        merge_deliveries(
            &mut list,
            vec![
                delivery("old", "2026-10-09T11:00:00.000Z", "failed"),
                delivery("c", "2026-10-09T12:00:03.000Z", "pending"),
            ],
        );
        assert_eq!(ids(&list), ["c", "b", "a", "old"]);
    }

    #[test]
    fn same_instant_orders_by_id_descending() {
        let mut list = Vec::new();
        merge_deliveries(
            &mut list,
            vec![
                delivery("a", "2026-10-09T12:00:00.000Z", "pending"),
                delivery("c", "2026-10-09T12:00:00.000Z", "pending"),
                delivery("b", "2026-10-09T12:00:00.000Z", "pending"),
            ],
        );
        assert_eq!(ids(&list), ["c", "b", "a"]);
    }

    #[test]
    fn merge_truncates_to_the_limit_and_reports_it() {
        let mut list: Vec<CloudNotifyDeliveryDto> = (0..CLOUD_NOTIFY_RECENT_DELIVERIES)
            .map(|index| {
                delivery(
                    &format!("id{index:03}"),
                    &format!("2026-10-09T10:{index:02}:00.000Z"),
                    "sent",
                )
            })
            .collect();
        sort_deliveries(&mut list);
        let newest = delivery("fresh", "2026-10-09T13:00:00.000Z", "pending");
        assert!(
            merge_deliveries(&mut list, vec![newest]),
            "超限必须报告截断"
        );
        assert_eq!(list.len(), CLOUD_NOTIFY_RECENT_DELIVERIES);
        assert_eq!(list[0].id, "fresh");
        assert!(
            !list.iter().any(|item| item.id == "id000"),
            "最旧的一条被挤出窗口"
        );
        // upsert 已有记录不改变长度、不算截断。
        assert!(!merge_deliveries(
            &mut list,
            vec![delivery("fresh", "2026-10-09T13:00:00.000Z", "sent")]
        ));
    }

    #[test]
    fn first_page_overwrites_records_and_cursor() {
        let mut state = CloudNotifyStateDto {
            recent_deliveries: vec![delivery("stale", "2026-10-09T09:00:00.000Z", "pending")],
            recent_next_cursor: Some("old-cursor".to_owned()),
            ..CloudNotifyStateDto::default()
        };
        set_first_page(
            &mut state,
            CloudNotifyDeliveriesPage {
                items: vec![
                    delivery("x", "2026-10-09T10:00:00.000Z", "sent"),
                    delivery("y", "2026-10-09T11:00:00.000Z", "failed"),
                ],
                next_cursor: Some("next".to_owned()),
            },
        );
        assert_eq!(
            ids(&state.recent_deliveries),
            ["y", "x"],
            "整页覆盖并排序，旧记录不残留"
        );
        assert_eq!(state.recent_next_cursor.as_deref(), Some("next"));
        set_first_page(&mut state, CloudNotifyDeliveriesPage::default());
        assert!(state.recent_deliveries.is_empty());
        assert_eq!(state.recent_next_cursor, None);
    }

    #[test]
    fn first_page_request_matches_the_recent_window() {
        let params = first_page_params();
        assert_eq!(params.limit, Some(50));
        assert_eq!(params.before, None);
    }
}
