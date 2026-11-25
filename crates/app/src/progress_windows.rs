//! 用户交互式下载的自动窗口策略。任务事件只更新状态索引和相关意图；不轮询、不为任务建定时器。
//! 所有未结束意图共用一个 oneshot 保活，完成集合接管保活后才释放本模块的等待。
//!
//! 判定交给 [`ProgressWindowTracker`]：本模块只提供窗口环境——该任务是否已有窗口、主窗口此刻
//! 是否可见 / 在前台、任务是否有待答复的选择请求。选择请求结束时重新判定仍在等待的意图，
//! 使选择窗口先于进度窗口出现，进度窗口不会抢走选择窗口的焦点。

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use fluxdown_protocol::{AgentEvent, AgentSnapshot, DaemonEvent, ServiceEvent, WsServerMsg};
use fluxdown_ui_downloads::{
    MAX_TRACKED_WINDOW_TASKS, MainWindowState, ProgressWindowEffect, ProgressWindowPrefs,
    ProgressWindowTracker, StartOrigin, UserStartIntent, WindowContext,
};
use gpui::{App, AppContext as _, Global};
use tokio::sync::oneshot;

use crate::{
    app::Desktop,
    session::{SessionSignal, agent_body},
    windows::{WindowKey, WindowRegistry},
};

/// 自动进度窗口的同时打开上限；手动打开任务详情不经过此处。
const MAX_AUTOMATIC_PROGRESS_WINDOWS: usize = 8;
/// 已确认的重启命令可能是无动作成功；等待新状态的预算不限制真正活跃的下载。
const RESTART_PROOF_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct RestartObservation {
    previous: Option<i32>,
    fresh: Option<i32>,
    confirmed: bool,
    replaces_task: bool,
    after_deletion: bool,
    proof_deadline: Option<Instant>,
    origin: StartOrigin,
}

impl RestartObservation {
    fn new(previous: Option<i32>, replaces_task: bool, origin: StartOrigin) -> Self {
        Self {
            previous,
            fresh: None,
            confirmed: false,
            replaces_task,
            after_deletion: false,
            proof_deadline: None,
            origin,
        }
    }

    fn confirm(&mut self, now: Instant) {
        self.confirmed = true;
        if self.fresh.is_none()
            && self
                .previous
                .is_some_and(|status| matches!(status, 0 | 1 | 5))
        {
            self.fresh = self.previous;
        }
        if self.fresh.is_none() && self.proof_deadline.is_none() {
            self.proof_deadline = Some(now + RESTART_PROOF_WAIT);
        }
    }

    fn observe(&mut self, status: i32) {
        // 重复的旧 completed / paused 快照不是本次开始的证据。状态确实变化后，
        // 即使命令回执晚于 completed，也能使用新状态，不依赖秒级时间戳。
        if self.previous != Some(status)
            && (self.previous.is_some() || self.after_deletion || matches!(status, 0 | 1 | 5))
        {
            self.fresh = Some(status);
        }
        self.previous = Some(status);
    }

    fn deleted(&mut self) {
        self.previous = None;
        self.fresh = None;
        self.after_deletion = true;
    }
}

#[derive(Default)]
struct ProgressWindows {
    tracker: ProgressWindowTracker,
    /// 与会话任务表同生命周期的轻量索引，避免每次开始都线性扫描完整任务表。
    statuses: HashMap<String, i32>,
    /// 只含正在执行的单任务交互命令，受 MAX_TRACKED_WINDOW_TASKS 限制。
    restarting: HashMap<String, RestartObservation>,
    automatic_windows: HashSet<String>,
    prefs: ProgressWindowPrefs,
    waiter: Option<oneshot::Sender<()>>,
    /// 所有未证明重启共用一个最早截止调度；普通下载不建立定时器。
    proof_timer: Option<gpui::Task<()>>,
}

impl Global for ProgressWindows {}

impl ProgressWindows {
    fn next_proof_deadline(&self) -> Option<Instant> {
        self.restarting
            .values()
            .filter_map(|restart| restart.proof_deadline)
            .min()
    }

    fn expire_unproven_restarts(&mut self, now: Instant) {
        self.restarting.retain(|task_id, restart| {
            let expired = restart
                .proof_deadline
                .is_some_and(|deadline| deadline <= now);
            if expired {
                log::debug!(
                    "no new state after confirmed task restart; releasing intent {task_id}"
                );
            }
            !expired
        });
    }

    /// 只有已见过任务的权威快照缺失才能证明删除。命令响应先于首个任务事件时，
    /// 紧接着到达的快照仍可能是创建前的旧表，不能撤销尚未见过的开始意图。
    fn reconcile_snapshot(&mut self, statuses: &HashMap<String, i32>) {
        self.tracker.retain_existing(|task_id| {
            !self.statuses.contains_key(task_id) || statuses.contains_key(task_id)
        });
        self.restarting.retain(|task_id, restart| {
            if statuses.contains_key(task_id) || !self.statuses.contains_key(task_id) {
                return true;
            }
            restart.deleted();
            restart.replaces_task
        });
    }

    /// 只在从零个有效意图变为非零时创建通道；整个突发共用一份接收任务。
    fn update_waiter(&mut self) -> Option<oneshot::Receiver<()>> {
        if self.restarting.is_empty() {
            self.proof_timer.take();
        }
        if !self.tracker.needs_keep_alive(self.prefs) && self.restarting.is_empty() {
            release_waiter(self);
            return None;
        }
        if self.waiter.is_some() {
            return None;
        }
        let (resolved, wait) = oneshot::channel();
        self.waiter = Some(resolved);
        Some(wait)
    }
}

/// 安装一次；不依赖主窗口存在。
pub fn install(cx: &mut App) {
    crate::completion_windows::install(cx);
    cx.set_global(ProgressWindows {
        prefs: prefs(cx),
        ..ProgressWindows::default()
    });
    let session = Desktop::global(cx).session.clone();
    cx.subscribe(&session, |_, signal, cx| match signal {
        SessionSignal::Snapshot(snapshot) => {
            if let Some(body) = agent_body(snapshot) {
                observe_all(body, cx);
            }
        }
        SessionSignal::Event(frame) => on_event(&frame.event, cx),
        SessionSignal::Fatal(_) | SessionSignal::ServiceStopped => clear(cx),
        SessionSignal::Stale | SessionSignal::ServiceFailed(_) => {}
    })
    .detach();
    let store = Desktop::global(cx).settings_store.clone();
    cx.observe(&store, |_, cx| {
        let prefs = prefs(cx);
        let state = cx.global_mut::<ProgressWindows>();
        if state.prefs == prefs {
            return;
        }
        state.prefs = prefs;
        state.tracker.update_prefs(prefs);
        if !prefs.progress && !prefs.completion {
            state.restarting.clear();
        }
        sync_keep_alive(cx);
    })
    .detach();
    cx.on_app_quit(|cx| {
        clear(cx);
        async {}
    })
    .detach();
    if let Some(statuses) = session
        .read(cx)
        .agent_snapshot()
        .and_then(snapshot_statuses)
    {
        cx.global_mut::<ProgressWindows>().statuses = statuses;
    }
}

/// 应用内入口（主窗口、新建下载表单、任务详情）的开始意图。
pub fn user_started(intent: UserStartIntent, cx: &mut App) {
    start(intent, StartOrigin::App, cx);
}

/// 新建任务可接受当前 completed；已有任务必须以命令前后的状态转移证明本轮完成。
fn start(intent: UserStartIntent, origin: StartOrigin, cx: &mut App) {
    if Desktop::global(cx).quitting {
        return;
    }
    match intent {
        UserStartIntent::Created(task_id) => {
            let state = cx.global_mut::<ProgressWindows>();
            if state.tracker.arm_created(&task_id, origin) {
                let status = state.statuses.get(&task_id).copied();
                state.tracker.update_prefs(state.prefs);
                if let Some(status) = status {
                    observe(&task_id, status, cx);
                }
            } else if state.tracker.tracked_count() >= MAX_TRACKED_WINDOW_TASKS {
                log::warn!("automatic task-window tracking limit reached; skipping {task_id}");
            }
        }
        UserStartIntent::RestartRequested {
            task_id,
            replaces_task,
        } => {
            let state = cx.global_mut::<ProgressWindows>();
            if !state.prefs.progress && !state.prefs.completion {
                return;
            }
            if state.restarting.contains_key(&task_id) {
                return;
            }
            if state.restarting.len() + state.tracker.tracked_count() >= MAX_TRACKED_WINDOW_TASKS {
                log::warn!(
                    "automatic task-window tracking limit reached; skipping restart {task_id}"
                );
                return;
            }
            let previous = state.statuses.get(&task_id).copied();
            state.restarting.insert(
                task_id,
                RestartObservation::new(previous, replaces_task, origin),
            );
        }
        UserStartIntent::RestartSucceeded(task_id) => {
            if let Some(restart) = cx
                .global_mut::<ProgressWindows>()
                .restarting
                .get_mut(&task_id)
            {
                restart.confirm(Instant::now());
                resolve_restart(&task_id, cx);
                schedule_proof_timeout(cx);
            }
        }
        UserStartIntent::RestartFailed(task_id) => {
            cx.global_mut::<ProgressWindows>()
                .restarting
                .remove(&task_id);
        }
    }
    sync_keep_alive(cx);
}

fn resolve_restart(task_id: &str, cx: &mut App) {
    let state = cx.global_mut::<ProgressWindows>();
    let Some((status, origin)) = state
        .restarting
        .get(task_id)
        .filter(|restart| restart.confirmed)
        .and_then(|restart| restart.fresh.map(|status| (status, restart.origin)))
    else {
        return;
    };
    state.restarting.remove(task_id);
    if state.tracker.arm_restart(task_id, origin) {
        crate::completion_windows::forget(task_id, cx);
        observe(task_id, status, cx);
    }
}

/// CLI / 单实例唤起与 CaptureTasksStarted 都是外部入口的新建任务证明；相同 ID 的重复启动
/// 提示幂等。
pub fn user_started_on_launch(task_id: String, cx: &mut App) {
    start(UserStartIntent::Created(task_id), StartOrigin::External, cx);
}

/// 外部入口（插件页面脚本）恢复的已有任务：命令已在 daemon 成功，仍须见到命令后的新状态才
/// 算本轮开始，与主窗口「继续」同一套证明。
fn external_resumed(task_id: String, cx: &mut App) {
    start(
        UserStartIntent::RestartRequested {
            task_id: task_id.clone(),
            replaces_task: false,
        },
        StartOrigin::External,
        cx,
    );
    start(
        UserStartIntent::RestartSucceeded(task_id),
        StartOrigin::External,
        cx,
    );
}

fn clear(cx: &mut App) {
    let state = cx.global_mut::<ProgressWindows>();
    state.tracker.clear();
    state.statuses.clear();
    state.restarting.clear();
    state.automatic_windows.clear();
    state.proof_timer.take();
    release_waiter(state);
}

fn release_waiter(state: &mut ProgressWindows) {
    if let Some(waiter) = state.waiter.take()
        && waiter.send(()).is_err()
    {
        log::trace!("task-window keepalive has already ended");
    }
}

fn sync_keep_alive(cx: &mut App) {
    let Some(wait) = cx.global_mut::<ProgressWindows>().update_waiter() else {
        return;
    };
    let task = cx.background_spawn(async move {
        if wait.await.is_err() {
            log::debug!("task-window keepalive owner released");
        }
    });
    crate::lifecycle::keep_alive(cx, task).detach();
}

fn schedule_proof_timeout(cx: &mut App) {
    let state = cx.global::<ProgressWindows>();
    if state.proof_timer.is_some() {
        return;
    }
    let Some(mut deadline) = state.next_proof_deadline() else {
        return;
    };
    let task = cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(deadline.saturating_duration_since(Instant::now()))
                .await;
            let next = cx.update(|cx| {
                let state = cx.global_mut::<ProgressWindows>();
                state.expire_unproven_restarts(Instant::now());
                let next = state.next_proof_deadline();
                if next.is_none() {
                    state.proof_timer.take();
                }
                sync_keep_alive(cx);
                next
            });
            match next {
                Some(next) => deadline = next,
                None => break,
            }
        }
    });
    cx.global_mut::<ProgressWindows>().proof_timer = Some(task);
}

fn prefs(cx: &App) -> ProgressWindowPrefs {
    ProgressWindowPrefs::from_preferences(Desktop::preferences(cx))
}

fn on_event(event: &ServiceEvent, cx: &mut App) {
    let ServiceEvent::Agent(event) = event else {
        return;
    };
    match event {
        AgentEvent::Daemon(DaemonEvent::TaskChanged(task)) => {
            record_status(&task.task_id, task.status, cx)
        }
        AgentEvent::Daemon(DaemonEvent::TasksChanged(tasks)) => {
            for task in tasks {
                record_status(&task.task_id, task.status, cx);
            }
        }
        AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id,
            status,
            ..
        })) => {
            record_status(task_id, *status, cx);
        }
        AgentEvent::CaptureTasksStarted(task_ids) => {
            if let [task_id] = task_ids.as_slice() {
                user_started_on_launch(task_id.clone(), cx);
            }
        }
        AgentEvent::CaptureTasksResumed(task_ids) => {
            if let [task_id] = task_ids.as_slice() {
                external_resumed(task_id.clone(), cx);
            }
        }
        // 会话已先移除该请求；选择窗口的订阅更早注册，此时已关窗。
        AgentEvent::Daemon(DaemonEvent::SelectionResolved { .. }) => reobserve_armed(cx),
        AgentEvent::Daemon(DaemonEvent::TaskDeleted { task_id }) => {
            let state = cx.global_mut::<ProgressWindows>();
            state.statuses.remove(task_id);
            state.tracker.forget(task_id);
            if let Some(restart) = state.restarting.get_mut(task_id) {
                restart.deleted();
                if !restart.replaces_task {
                    state.restarting.remove(task_id);
                }
            }
            sync_keep_alive(cx);
        }
        AgentEvent::Daemon(DaemonEvent::TasksDeleted { task_ids }) => {
            let state = cx.global_mut::<ProgressWindows>();
            for task_id in task_ids {
                state.statuses.remove(task_id);
                state.tracker.forget(task_id);
                if let Some(restart) = state.restarting.get_mut(task_id) {
                    restart.deleted();
                    if !restart.replaces_task {
                        state.restarting.remove(task_id);
                    }
                }
            }
            sync_keep_alive(cx);
        }
        AgentEvent::DaemonSnapshotReplaced(_)
        | AgentEvent::Daemon(
            DaemonEvent::SnapshotReplaced(_)
            | DaemonEvent::Engine(WsServerMsg::TasksSnapshot { .. }),
        ) => {
            let session = Desktop::global(cx).session.clone();
            let statuses = session
                .read(cx)
                .agent_snapshot()
                .and_then(snapshot_statuses);
            if let Some(statuses) = statuses {
                replace_statuses(statuses, cx);
            }
        }
        _ => {}
    }
}

fn observe_all(body: &AgentSnapshot, cx: &mut App) {
    if let Some(statuses) = snapshot_statuses(body) {
        replace_statuses(statuses, cx);
    }
}

/// 任务状态表；任务表仍在分页载入（`tasks` 只是占位）时返回 `None`：用占位表对账会把所有任务
/// 当成已删除，并让载入后「状态变化」的通知判定失去对照。
fn snapshot_statuses(body: &AgentSnapshot) -> Option<HashMap<String, i32>> {
    (body.daemon_connected && !body.daemon.tables_loading()).then(|| {
        body.daemon
            .tasks
            .iter()
            .map(|task| (task.task_id.clone(), task.status))
            .collect()
    })
}

fn replace_statuses(statuses: HashMap<String, i32>, cx: &mut App) {
    cx.global_mut::<ProgressWindows>()
        .reconcile_snapshot(&statuses);
    // 仅快照替换扫描全量表；正常进度帧只是一次哈希查询。
    for (task_id, status) in &statuses {
        let changed = cx.global::<ProgressWindows>().statuses.get(task_id) != Some(status);
        observe_recorded_status(task_id, *status, changed, cx);
    }
    cx.global_mut::<ProgressWindows>().statuses = statuses;
    sync_keep_alive(cx);
}

fn record_status(task_id: &str, status: i32, cx: &mut App) {
    let state = cx.global_mut::<ProgressWindows>();
    let changed = match state.statuses.get_mut(task_id) {
        Some(previous) => {
            let changed = *previous != status;
            *previous = status;
            changed
        }
        None => {
            state.statuses.insert(task_id.to_owned(), status);
            true
        }
    };
    observe_recorded_status(task_id, status, changed, cx);
}

fn observe_recorded_status(task_id: &str, status: i32, changed: bool, cx: &mut App) {
    let state = cx.global_mut::<ProgressWindows>();
    if let Some(restart) = state.restarting.get_mut(task_id) {
        restart.observe(status);
        resolve_restart(task_id, cx);
        return;
    }
    if changed || state.tracker.is_armed(task_id) {
        observe(task_id, status, cx);
    }
}

/// 选择请求结束：仍在等的意图按当前状态补判（答复后的新状态可能早于本事件到达）。
fn reobserve_armed(cx: &mut App) {
    let state = cx.global::<ProgressWindows>();
    let waiting: Vec<(String, i32)> = state
        .tracker
        .armed_ids()
        .filter_map(|task_id| {
            state
                .statuses
                .get(task_id)
                .map(|status| (task_id.to_owned(), *status))
        })
        .collect();
    for (task_id, status) in waiting {
        observe(&task_id, status, cx);
    }
}

/// 主窗口此刻的可见 / 前台状态。句柄暂不可更新只发生在主窗口自己的 update 栈内，即用户
/// 正在主窗口里操作，按在场处理。
fn main_window_state(cx: &mut App) -> MainWindowState {
    let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Main) else {
        return MainWindowState::default();
    };
    handle
        .update(cx, |_, window, _| MainWindowState {
            visible: window.is_visible(),
            active: window.is_window_active(),
        })
        .unwrap_or_else(|error| {
            log::debug!("main window busy while judging task windows: {error:#}");
            MainWindowState {
                visible: true,
                active: true,
            }
        })
}

fn selection_pending(task_id: &str, cx: &App) -> bool {
    Desktop::global(cx)
        .session
        .read(cx)
        .agent_snapshot()
        .is_some_and(|body| {
            body.daemon
                .pending_selections
                .iter()
                .any(|request| request.task_id == task_id)
        })
}

fn observe(task_id: &str, status: i32, cx: &mut App) {
    let key = WindowKey::Progress(task_id.to_owned());
    let context = WindowContext {
        window_open: WindowRegistry::is_open(cx, &key),
        main: main_window_state(cx),
        selection_pending: selection_pending(task_id, cx),
    };
    let state = cx.global_mut::<ProgressWindows>();
    let effect = state.tracker.observe(task_id, status, context, state.prefs);
    match effect {
        Some(ProgressWindowEffect::OpenProgress) => {
            let mut open =
                std::mem::take(&mut cx.global_mut::<ProgressWindows>().automatic_windows);
            open.retain(|id| WindowRegistry::is_open(cx, &WindowKey::Progress(id.clone())));
            let can_open = open.len() < MAX_AUTOMATIC_PROGRESS_WINDOWS;
            cx.global_mut::<ProgressWindows>().automatic_windows = open;
            if can_open {
                crate::windows::progress::open_automatic(cx, task_id.to_owned());
                if WindowRegistry::is_open(cx, &key) {
                    cx.global_mut::<ProgressWindows>()
                        .automatic_windows
                        .insert(task_id.to_owned());
                }
            }
        }
        Some(ProgressWindowEffect::OpenCompletion) => {
            // 必须先让完成集合接管保活，再释放最后一个任务意图，避免无窗口退出竞态。
            crate::completion_windows::enqueue(task_id.to_owned(), cx);
        }
        Some(ProgressWindowEffect::Close) => WindowRegistry::close(cx, &key),
        None => {}
    }
    sync_keep_alive(cx);
}

#[cfg(test)]
mod tests {
    use fluxdown_ui_downloads::{StartOrigin, WindowContext};

    use super::{ProgressWindowPrefs, ProgressWindows, RestartObservation, snapshot_statuses};

    const APP: StartOrigin = StartOrigin::App;

    #[test]
    fn old_completed_snapshot_does_not_complete_a_restart() {
        let mut restart = RestartObservation::new(Some(3), true, APP);
        restart.observe(3);
        assert_eq!(restart.fresh, None);
        restart.observe(5);
        restart.observe(3);
        assert_eq!(restart.fresh, Some(3));
    }

    /// 任务表分页载入期间不产出状态表：用占位空表对账会把所有任务当成已删除。
    #[test]
    fn statuses_are_withheld_while_the_task_table_is_loading() {
        let mut body = fluxdown_protocol::AgentSnapshot {
            daemon_connected: true,
            ..fluxdown_protocol::AgentSnapshot::default()
        };
        assert_eq!(
            snapshot_statuses(&body).map(|statuses| statuses.len()),
            Some(0)
        );
        body.daemon.task_total = Some(230_000);
        assert!(snapshot_statuses(&body).is_none());
        body.daemon.task_total = None;
        body.daemon.queue_total = Some(3);
        assert!(snapshot_statuses(&body).is_none());
    }

    #[test]
    fn fast_resume_completion_before_command_reply_is_fresh() {
        let mut restart = RestartObservation::new(Some(2), false, APP);
        restart.observe(3);
        assert_eq!(restart.fresh, Some(3));
    }

    #[test]
    fn deletion_proves_recreated_completed_task_is_new() {
        let mut restart = RestartObservation::new(Some(3), true, APP);
        restart.deleted();
        restart.observe(3);
        assert_eq!(restart.fresh, Some(3));
    }

    #[test]
    fn missing_restart_baseline_does_not_accept_old_completed() {
        let mut restart = RestartObservation::new(None, true, APP);
        restart.observe(3);
        assert_eq!(restart.fresh, None);
    }

    #[test]
    fn completion_only_burst_uses_one_waiter_until_every_task_finishes() {
        let mut state = ProgressWindows {
            prefs: ProgressWindowPrefs {
                progress: false,
                completion: true,
            },
            ..ProgressWindows::default()
        };
        let mut wait = None;
        for index in 0..1000 {
            let task_id = index.to_string();
            state.tracker.arm_created(&task_id, APP);
            let next = state.update_waiter();
            if index == 0 {
                wait = next;
            } else {
                assert!(next.is_none(), "同一突发不得产生每任务等待");
            }
            state
                .tracker
                .observe(&task_id, 1, WindowContext::default(), state.prefs);
            assert!(state.update_waiter().is_none());
            assert!(state.waiter.is_some(), "进入活跃态不能释放仅完成模式的保活");
        }
        let mut wait = wait.expect("首个任务创建全局等待");
        assert_eq!(
            wait.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        for index in 0..1000 {
            state
                .tracker
                .observe(&index.to_string(), 3, WindowContext::default(), state.prefs);
            assert!(state.update_waiter().is_none());
        }
        assert!(state.waiter.is_none());
        assert_eq!(wait.try_recv(), Ok(()));
    }

    #[test]
    fn disabling_both_and_service_cleanup_release_the_global_waiter() {
        let mut state = ProgressWindows::default();
        state.tracker.arm_created("t", APP);
        let mut wait = state.update_waiter().expect("开始意图保活");
        state.prefs = ProgressWindowPrefs {
            progress: false,
            completion: false,
        };
        state.tracker.update_prefs(state.prefs);
        assert!(state.update_waiter().is_none());
        assert_eq!(wait.try_recv(), Ok(()));

        state.prefs = ProgressWindowPrefs::default();
        state.tracker.arm_created("next", APP);
        let mut wait = state.update_waiter().expect("下一次开始独立等待");
        state.tracker.clear();
        state.restarting.clear();
        assert!(state.update_waiter().is_none());
        assert_eq!(wait.try_recv(), Ok(()));
    }

    #[test]
    fn disconnected_initial_snapshot_cannot_discard_a_launch_intent() {
        let mut state = ProgressWindows::default();
        state.tracker.arm_created("launched", APP);
        let snapshot = fluxdown_protocol::AgentSnapshot::default();
        assert!(snapshot_statuses(&snapshot).is_none());
        assert!(state.tracker.needs_keep_alive(state.prefs));
        let ready = fluxdown_protocol::AgentSnapshot {
            daemon_connected: true,
            ..fluxdown_protocol::AgentSnapshot::default()
        };
        assert!(snapshot_statuses(&ready).is_some());
    }

    #[test]
    fn snapshot_before_first_task_event_preserves_intent_but_known_deletion_releases_it() {
        let mut state = ProgressWindows::default();
        state.tracker.arm_created("new", APP);
        state.reconcile_snapshot(&std::collections::HashMap::new());
        assert!(state.tracker.is_armed("new"));
        state.statuses.insert("new".to_owned(), 0);
        state.reconcile_snapshot(&std::collections::HashMap::new());
        assert!(!state.tracker.needs_keep_alive(state.prefs));
    }

    #[test]
    fn shared_restart_proof_deadline_expires_only_unproven_commands() {
        let now = std::time::Instant::now();
        let mut state = ProgressWindows::default();
        for index in 0..1000 {
            let mut restart = RestartObservation::new(Some(2), false, APP);
            restart.confirm(now);
            state.restarting.insert(index.to_string(), restart);
        }
        let deadline = now + super::RESTART_PROOF_WAIT;
        assert_eq!(state.next_proof_deadline(), Some(deadline));
        let mut wait = state.update_waiter().expect("等待恢复状态时仍然保活");
        state.expire_unproven_restarts(deadline - std::time::Duration::from_secs(1));
        assert_eq!(state.restarting.len(), 1000);
        let later_now = now + std::time::Duration::from_secs(1);
        let mut later = RestartObservation::new(Some(3), true, APP);
        later.confirm(later_now);
        state.restarting.insert("later".to_owned(), later);
        state.expire_unproven_restarts(deadline);
        assert_eq!(state.restarting.len(), 1);
        assert_eq!(
            state.next_proof_deadline(),
            Some(later_now + super::RESTART_PROOF_WAIT)
        );
        assert!(state.update_waiter().is_none());
        assert_eq!(
            wait.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        state.expire_unproven_restarts(later_now + super::RESTART_PROOF_WAIT);
        assert!(state.restarting.is_empty());
        assert!(state.update_waiter().is_none());
        assert_eq!(wait.try_recv(), Ok(()));

        state.tracker.arm_created("really-active", APP);
        state
            .tracker
            .observe("really-active", 1, WindowContext::default(), state.prefs);
        state.expire_unproven_restarts(deadline + super::RESTART_PROOF_WAIT);
        assert!(state.tracker.needs_keep_alive(state.prefs));
    }

    #[test]
    fn confirmed_running_or_pending_restart_needs_no_proof_timer() {
        for status in [0, 1, 5] {
            let mut restart = RestartObservation::new(Some(status), false, APP);
            restart.confirm(std::time::Instant::now());
            assert_eq!(restart.fresh, Some(status));
            assert!(restart.proof_deadline.is_none());
        }
        let mut completed = RestartObservation::new(Some(3), true, APP);
        completed.confirm(std::time::Instant::now());
        assert_eq!(completed.fresh, None);
        assert!(completed.proof_deadline.is_some());
    }
}
