//! agent 为待确认交互（外部捕获 / 引擎选择）与下载提示窗口拉起官方 UI 的记账。
//!
//! 确认拉起是电平触发的：外壳控制循环每一轮（事件、1s 节拍、UI 连接变化、拉起的进程退出）都按
//! 快照重新判定「有待确认交互且没有 UI 连接」，而不是只在队列由空变非空的那一刻尝试一次。界面
//! 进程正在退出时到达的捕获、因静默期暂缓的拉起，都会在条件仍成立时补上。约束：
//!
//! - 拉起的桌面进程退出前不再为确认另起进程：它要么成为主实例连上来，要么等正在退出的旧实例
//!   释放单实例锁后接替，要么把请求交给仍在运行的主实例后退出——退出时重新判定；
//! - 拉起后与最后一个 UI 断开后各留 [`HOLD`] 静默期：重连抖动不额外拉起进程；
//! - 待确认交互没有变化期间最多为确认拉起 [`MAX_PROMPT_ATTEMPTS`] 次：桌面程序启动即崩溃时
//!   不无限重拉；出现新的待确认交互或已有的被处理后重新计数。
//! - 下载提示与确认共享单飞 / 静默期；同一批未交付任务最多尝试 [`MAX_PROMPT_ATTEMPTS`]
//!   次，新任务追加不会重置失败次数。交付或关闭两个提示开关后，下一批重新计数；
//! - 只暂存最近 [`MAX_PENDING_TASKS`] 个单任务捕获意图。超限淘汰最旧意图并累计告警，
//!   不影响下载本身；仅本 agent 生命周期内保留，不从历史任务 / 恢复快照生成提醒。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 拉起后 / 最后一个 UI 断开后不为确认拉起界面的静默期。
pub(super) const HOLD: Duration = Duration::from_secs(1);
/// 待确认交互不变时为确认拉起界面的次数上限。
pub(super) const MAX_PROMPT_ATTEMPTS: u32 = 3;
/// 等待官方 UI 首个快照完成的单任务意图上限；不会为每项启动进程或定时器。
pub(super) const MAX_PENDING_TASKS: usize = 1024;

#[derive(Debug, Default)]
pub(super) struct UiLaunches {
    /// 本 agent 拉起、尚未退出的桌面进程数。
    running: usize,
    /// 静默期截止时刻。
    hold_until: Option<Instant>,
    /// 自待确认交互上次变化以来为确认拉起的次数。
    prompt_attempts: u32,
    task_attempts: u32,
    pending_tasks: VecDeque<String>,
    dropped_tasks: usize,
}

impl UiLaunches {
    /// 有待确认交互且没有 UI 连接时，现在是否应为确认拉起界面。
    pub(super) fn prompt_due(&self, now: Instant) -> bool {
        self.running == 0
            && self.prompt_attempts < MAX_PROMPT_ATTEMPTS
            && self.hold_until.is_none_or(|until| now >= until)
    }

    /// 为确认发起一次拉起（在 spawn 之前登记，并发判定不会重复拉起）。
    pub(super) fn begin_prompt(&mut self, now: Instant) {
        self.prompt_attempts = self.prompt_attempts.saturating_add(1);
        self.begin(now);
    }

    /// 记录尚无就绪 UI 接收的单任务意图；下载请求已逐个建成独立 ID。
    pub(super) fn queue_task(&mut self, task_id: &str) {
        if self.pending_tasks.is_empty() {
            self.task_attempts = 0;
        }
        if self.pending_tasks.len() == MAX_PENDING_TASKS {
            drop(self.pending_tasks.pop_front());
            self.dropped_tasks = self.dropped_tasks.saturating_add(1);
            if self.dropped_tasks == 1 {
                tracing::warn!(
                    capacity = MAX_PENDING_TASKS,
                    "pending task-window intents full; retaining newest task IDs"
                );
            }
        }
        self.pending_tasks.push_back(task_id.to_owned());
    }

    /// 确认与下载提示共享运行中进程与重连静默期，但分别限制失败尝试次数。
    pub(super) fn task_due(&self, now: Instant) -> bool {
        !self.pending_tasks.is_empty()
            && self.running == 0
            && self.task_attempts < MAX_PROMPT_ATTEMPTS
            && self.hold_until.is_none_or(|until| now >= until)
    }

    /// 仅首个 ID 通过命令行传递以保活；其余 ID 在首个快照后由事件重放交付。
    pub(super) fn begin_task(&mut self, now: Instant) -> Option<String> {
        if !self.task_due(now) {
            return None;
        }
        let task_id = self.pending_tasks.front()?.clone();
        self.task_attempts = self.task_attempts.saturating_add(1);
        self.begin(now);
        Some(task_id)
    }

    pub(super) fn take_tasks(&mut self) -> VecDeque<String> {
        self.report_dropped_tasks();
        self.task_attempts = 0;
        std::mem::take(&mut self.pending_tasks)
    }

    pub(super) fn clear_tasks(&mut self) {
        self.report_dropped_tasks();
        self.pending_tasks.clear();
        self.task_attempts = 0;
    }

    fn report_dropped_tasks(&mut self) {
        if self.dropped_tasks > 0 {
            tracing::warn!(
                count = self.dropped_tasks,
                "old task-window intents omitted while desktop was unavailable"
            );
            self.dropped_tasks = 0;
        }
    }

    fn begin(&mut self, now: Instant) {
        self.running = self.running.saturating_add(1);
        self.hold(now);
    }

    /// 拉起的进程已退出，或根本没能启动。
    pub(super) fn finished(&mut self) {
        self.running = self.running.saturating_sub(1);
    }

    /// 最后一个 UI 断开：静默期内留给重连，不拉起新进程。
    pub(super) fn ui_left(&mut self, now: Instant) {
        self.hold(now);
    }

    /// 待确认交互发生变化（新增、被处理、快照替换或事件断档）：重新计数。
    pub(super) fn prompts_changed(&mut self) {
        self.prompt_attempts = 0;
    }

    fn hold(&mut self, now: Instant) {
        let until = now + HOLD;
        self.hold_until = Some(self.hold_until.map_or(until, |current| current.max(until)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_launch_blocks_relaunch_until_it_exits() {
        let start = Instant::now();
        let mut launches = UiLaunches::default();
        assert!(launches.prompt_due(start));
        launches.begin_prompt(start);
        // 进程仍在启动 / 等旧实例释放锁：无论过多久都不重复拉起。
        assert!(!launches.prompt_due(start + HOLD * 30));
        launches.finished();
        // 进程退出后（例如把请求交给了正在退出的旧实例）条件仍成立就补拉。
        assert!(launches.prompt_due(start + HOLD));
    }

    #[test]
    fn task_launch_counts_as_running_without_spending_prompt_attempts() {
        let start = Instant::now();
        let mut launches = UiLaunches::default();
        launches.queue_task("task-1");
        assert_eq!(launches.begin_task(start).as_deref(), Some("task-1"));
        assert!(!launches.prompt_due(start + HOLD));
        launches.finished();
        for attempt in 0..MAX_PROMPT_ATTEMPTS {
            let now = start + HOLD * (attempt + 1);
            assert!(
                launches.prompt_due(now),
                "attempt {attempt} must be allowed"
            );
            launches.begin_prompt(now);
            launches.finished();
        }
    }

    #[test]
    fn ui_disconnect_holds_launch_for_reconnect_then_allows_it() {
        let start = Instant::now();
        let mut launches = UiLaunches::default();
        launches.ui_left(start);
        assert!(!launches.prompt_due(start));
        assert!(!launches.prompt_due(start + HOLD - Duration::from_millis(1)));
        assert!(launches.prompt_due(start + HOLD));
    }

    #[test]
    fn hold_never_shrinks() {
        let start = Instant::now();
        let mut launches = UiLaunches::default();
        launches.ui_left(start + HOLD);
        launches.ui_left(start);
        assert!(!launches.prompt_due(start + HOLD));
        assert!(launches.prompt_due(start + HOLD * 2));
    }

    #[test]
    fn unanswered_launches_stop_until_prompts_change() {
        let start = Instant::now();
        let mut launches = UiLaunches::default();
        let mut now = start;
        for _ in 0..MAX_PROMPT_ATTEMPTS {
            assert!(launches.prompt_due(now));
            launches.begin_prompt(now);
            launches.finished();
            now += HOLD;
        }
        // 桌面程序反复启动即退出：不再无限重拉。
        assert!(!launches.prompt_due(now + HOLD * 10));
        // 新捕获到达 / 已有交互被处理：重新允许拉起。
        launches.prompts_changed();
        assert!(launches.prompt_due(now));
    }

    #[test]
    fn thousand_single_captures_launch_once_and_retain_every_intent() {
        let start = Instant::now();
        let mut launches = UiLaunches::default();
        let mut spawned = 0;
        for index in 0..1000 {
            launches.queue_task(&format!("task-{index}"));
            if launches.begin_task(start).is_some() {
                spawned += 1;
            }
        }
        assert_eq!(spawned, 1);
        assert!(!launches.task_due(start + HOLD * 30));
        let tasks = launches.take_tasks();
        assert_eq!(tasks.len(), 1000);
        assert_eq!(tasks.front().map(String::as_str), Some("task-0"));
        assert_eq!(tasks.back().map(String::as_str), Some("task-999"));
        assert!(
            launches.take_tasks().is_empty(),
            "reconnect must not replay old IDs"
        );
    }

    #[test]
    fn task_intents_survive_exit_hold_and_bounded_retry_failures() {
        let start = Instant::now();
        let mut launches = UiLaunches::default();
        launches.queue_task("first");
        for attempt in 0..MAX_PROMPT_ATTEMPTS {
            let now = start + HOLD * attempt;
            assert_eq!(launches.begin_task(now).as_deref(), Some("first"));
            launches.finished();
            launches.queue_task(&format!("during-attempt-{attempt}"));
            assert!(!launches.task_due(now));
        }
        assert!(!launches.task_due(start + HOLD * 30));
        assert_eq!(launches.take_tasks().len(), 4);
        launches.queue_task("new-cohort");
        launches.ui_left(start + HOLD * 30);
        assert!(!launches.task_due(start + HOLD * 30));
        assert_eq!(
            launches.begin_task(start + HOLD * 31).as_deref(),
            Some("new-cohort")
        );
    }

    #[test]
    fn pending_task_limit_keeps_recent_ids_and_clear_discards_intents() {
        let mut launches = UiLaunches::default();
        for index in 0..MAX_PENDING_TASKS + 5 {
            launches.queue_task(&format!("task-{index}"));
        }
        assert_eq!(launches.pending_tasks.len(), MAX_PENDING_TASKS);
        assert_eq!(launches.dropped_tasks, 5);
        let tasks = launches.take_tasks();
        assert_eq!(tasks.front().map(String::as_str), Some("task-5"));
        assert_eq!(tasks.len(), MAX_PENDING_TASKS);
        assert_eq!(launches.dropped_tasks, 0);
        launches.queue_task("disabled");
        launches.clear_tasks();
        assert!(launches.take_tasks().is_empty());
        assert!(!launches.task_due(Instant::now()));
    }
}
