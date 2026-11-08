//! 下载完成通知与活动下载期间的系统保持唤醒。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, DaemonEvent, EventFrame, ServiceEvent, SnapshotBody, TaskDto,
    WsServerMsg,
};
use tokio::sync::broadcast;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::event_hub::AgentEventHub;
use crate::failure_notice::{
    FailedTask, FailureReceiver, NOTIFY_ON_FAIL_DEFAULT, NOTIFY_ON_FAIL_PREF, failure_text,
};
use crate::notification::{
    NoticeText, Notifier, completion_path, completion_text, english_text, rss_auto_download_text,
};

/// 完成通知防抖：最后一次完成后静默这么久才合并发一条。
const NOTIFY_DEBOUNCE: Duration = Duration::from_millis(800);
/// 持续密集完成时的最长合并等待。
const NOTIFY_MAX_WAIT: Duration = Duration::from_secs(3);

pub struct BackgroundEffects {
    events: AgentEventHub,
    notifier: Arc<Notifier>,
    action_text: NoticeText,
    /// agent 自己播放提示音（桌面宿主）；为真且对应提示音已启用时，完成 / 失败通知静音。
    completion_sound_owned: bool,
    /// 下载失败通知的来源（桌面宿主）；`None` = 不发失败通知。
    failures: Option<FailureReceiver>,
    #[cfg(feature = "desktop")]
    translator: Option<fluxdown_ui_i18n::Translator>,
}

/// 防抖中尚未发出的完成通知。
#[derive(Default)]
struct PendingCompletions {
    file_names: Vec<String>,
    // 与通知正文最后一个文件绑定；不得在无效路径时回退到前一个任务。
    last_path: Option<std::path::PathBuf>,
    /// 最近一批完成时的界面语言偏好（`general.locale`；`None` = 跟随系统）。
    locale: Option<String>,
    started_at: Option<Instant>,
    flush_at: Option<Instant>,
}

/// 防抖中尚未发出的失败通知。
#[derive(Default)]
struct PendingFailures {
    tasks: Vec<FailedTask>,
    started_at: Option<Instant>,
    flush_at: Option<Instant>,
}

impl BackgroundEffects {
    /// `notifier`：与网关共用的系统通知投递器。
    #[must_use]
    pub fn new(events: AgentEventHub, notifier: Arc<Notifier>) -> Self {
        Self {
            events,
            notifier,
            action_text: NoticeText::default(),
            completion_sound_owned: false,
            failures: None,
            #[cfg(feature = "desktop")]
            translator: match fluxdown_ui_i18n::I18nCatalog::load_embedded() {
                Ok(catalog) => {
                    Some(Arc::new(catalog).translator(&fluxdown_ui_i18n::system_locale()))
                }
                Err(error) => {
                    tracing::warn!(error = %error, "notification translations unavailable");
                    None
                }
            },
        }
    }

    /// 声明本宿主会播放「下载完成」提示音（见 [`crate::sound`]）：用户启用后完成通知改为静音，避免双重声音。
    #[must_use]
    pub fn with_completion_sound(mut self, owned: bool) -> Self {
        self.completion_sound_owned = owned;
        self
    }

    /// 接入下载失败通知：`receiver` 收到的失败任务合并成一条系统通知（受
    /// `desktop.notify_on_fail` 约束）。不接入则不发失败通知（headless / 移动端）。
    #[must_use]
    pub(crate) fn with_failure_notices(mut self, receiver: FailureReceiver) -> Self {
        self.failures = Some(receiver);
        self
    }

    pub async fn run(mut self, cancel: CancellationToken) {
        let (mut receiver, snapshot) = self.events.subscribe_and_snapshot();
        let mut failures = self.failures.take();
        let mut awake = AwakeState::default();
        let mut pending = PendingCompletions::default();
        let mut failed = PendingFailures::default();
        let mut tracker = TaskTracker::default();
        {
            let initial = agent_snapshot(snapshot);
            // 空镜像上的整表登记不会产生完成通知。
            tracker.rebuild(&initial);
            let hold = preference_bool(&initial, KEEP_AWAKE_PREF, KEEP_AWAKE_DEFAULT)
                && tracker.has_active();
            reconcile_awake(hold, &mut awake).await;
        }
        loop {
            let flush_at = pending.flush_at;
            let flush = async move {
                match flush_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            let failure_flush_at = failed.flush_at;
            let failure_flush = async move {
                match failure_flush_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = cancel.cancelled() => return,
                () = flush => self.flush(&mut pending),
                () = failure_flush => self.flush_failures(&mut failed),
                next = recv_failure(&mut failures) => match next {
                    Some(task) => failed.push(task, Instant::now()),
                    None => failures = None,
                },
                event = receiver.recv() => {
                    let frame = match &event {
                        Ok(frame) => {
                            self.notify_rss_auto_downloads(frame);
                            Some(frame)
                        }
                        // 本接收端自己落后：丢了帧，任务表整表对账一次。
                        Err(broadcast::error::RecvError::Lagged(_)) => None,
                        Err(broadcast::error::RecvError::Closed) => return,
                    };
                    let change = frame.map_or(Change::Rebuild, |frame| change_for(frame, &tracker));
                    if matches!(change, Change::None) {
                        continue;
                    }
                    // 锁内只做与帧内任务数成正比的工作，不克隆也不扫描整份快照。
                    let observed = self
                        .events
                        .inspect(|snapshot| observe(&mut tracker, snapshot, change));
                    if !observed.completed.is_empty() {
                        pending.push(observed.completed, observed.locale, Instant::now());
                    }
                    if let Some(hold) = observed.hold {
                        reconcile_awake(hold, &mut awake).await;
                    }
                }
            }
        }
    }

    /// 订阅开启「自动下载时通知」时，引擎把本轮自动建任务的标题放进 `notify_titles`；
    /// 多 UI 连接也只在 agent 这一处发系统通知。受完成通知总开关约束。
    fn notify_rss_auto_downloads(&mut self, frame: &fluxdown_protocol::EventFrame) {
        let Some(titles) = rss_notify_titles(frame) else {
            return;
        };
        let (enabled, locale) = self
            .events
            .inspect(|snapshot| (notify_on_complete(snapshot), locale_preference(snapshot)));
        if !enabled {
            return;
        }
        let Some((title, body)) = self.rss_notice(titles, locale) else {
            return;
        };
        let notifier = Arc::clone(&self.notifier);
        tokio::task::spawn_blocking(move || notifier.show(&title, &body));
    }

    #[cfg(feature = "desktop")]
    fn rss_notice(
        &mut self,
        titles: &[String],
        locale: Option<String>,
    ) -> Option<(String, String)> {
        let Some(translator) = self.translator.as_mut() else {
            return rss_auto_download_text(titles, english_text);
        };
        translator.set_locale(&locale.unwrap_or_else(fluxdown_ui_i18n::system_locale));
        let translator = &*translator;
        rss_auto_download_text(titles, |key, count| match count {
            Some(count) => {
                let count = count.to_string();
                translator.text_with(key, &[("n", &count), ("count", &count)])
            }
            None => translator.text(key).to_owned(),
        })
    }

    #[cfg(not(feature = "desktop"))]
    #[allow(clippy::unused_self)]
    fn rss_notice(
        &mut self,
        titles: &[String],
        _locale: Option<String>,
    ) -> Option<(String, String)> {
        rss_auto_download_text(titles, english_text)
    }

    /// 把防抖中的完成合并成一条系统通知发出（发送阻塞，放进 blocking 线程）。
    fn flush(&mut self, pending: &mut PendingCompletions) {
        let batch = std::mem::take(pending);
        if !self.events.inspect(notify_on_complete) {
            return;
        }
        let silent = self.completion_sound_owned
            && self.events.inspect(|snapshot| {
                crate::sound::setting(snapshot, crate::sound::SoundKind::TaskCompleted).is_enabled()
            });
        let open_file = self.action_text.text("openFile", batch.locale.as_deref());
        let open_folder = self.action_text.text("openFolder", batch.locale.as_deref());
        let Some((title, body)) = self.completion_notice(&batch.file_names, batch.locale) else {
            return;
        };
        let notifier = Arc::clone(&self.notifier);
        tokio::task::spawn_blocking(move || {
            notifier.show_completion(
                &title,
                &body,
                batch.last_path,
                open_file,
                open_folder,
                silent,
            );
        });
    }

    #[cfg(feature = "desktop")]
    fn completion_notice(
        &mut self,
        file_names: &[String],
        locale: Option<String>,
    ) -> Option<(String, String)> {
        let Some(translator) = self.translator.as_mut() else {
            return completion_text(file_names, english_text);
        };
        translator.set_locale(&locale.unwrap_or_else(fluxdown_ui_i18n::system_locale));
        let translator = &*translator;
        completion_text(file_names, |key, count| match count {
            Some(count) => translator.text_with(key, &[("count", &count.to_string())]),
            None => translator.text(key).to_owned(),
        })
    }

    /// headless 构建不带文案目录：固定英文。
    #[cfg(not(feature = "desktop"))]
    #[allow(clippy::unused_self)]
    fn completion_notice(
        &mut self,
        file_names: &[String],
        _locale: Option<String>,
    ) -> Option<(String, String)> {
        completion_text(file_names, english_text)
    }

    /// 把防抖中的失败合并成一条系统通知发出；发送前再次检查开关（防抖期间用户可能关掉）。
    fn flush_failures(&mut self, pending: &mut PendingFailures) {
        let batch = std::mem::take(pending);
        let owned = self.completion_sound_owned;
        let (enabled, locale, silent) = self.events.inspect(|snapshot| {
            (
                notify_on_failure(snapshot),
                locale_preference(snapshot),
                owned
                    && crate::sound::setting(snapshot, crate::sound::SoundKind::TaskFailed)
                        .is_enabled(),
            )
        });
        if !enabled {
            return;
        }
        let Some((title, body)) = self.failure_notice(&batch.tasks, locale) else {
            return;
        };
        let notifier = Arc::clone(&self.notifier);
        tokio::task::spawn_blocking(move || notifier.show_failure(&title, &body, silent));
    }

    #[cfg(feature = "desktop")]
    fn failure_notice(
        &mut self,
        tasks: &[FailedTask],
        locale: Option<String>,
    ) -> Option<(String, String)> {
        let Some(translator) = self.translator.as_mut() else {
            return failure_text(tasks, english_text);
        };
        translator.set_locale(&locale.unwrap_or_else(fluxdown_ui_i18n::system_locale));
        let translator = &*translator;
        failure_text(tasks, |key, count| match count {
            Some(count) => translator.text_with(key, &[("count", &count.to_string())]),
            None => translator.text(key).to_owned(),
        })
    }

    /// headless 构建不带文案目录：固定英文。
    #[cfg(not(feature = "desktop"))]
    #[allow(clippy::unused_self)]
    fn failure_notice(
        &mut self,
        tasks: &[FailedTask],
        _locale: Option<String>,
    ) -> Option<(String, String)> {
        failure_text(tasks, english_text)
    }
}

impl PendingCompletions {
    /// 并入一批完成并重排发送时刻：静默 [`NOTIFY_DEBOUNCE`] 后发，但自本批首个完成起
    /// 最多等 [`NOTIFY_MAX_WAIT`]。
    fn push(&mut self, completed: Vec<CompletedFile>, locale: Option<String>, now: Instant) {
        if let Some(last) = completed.last() {
            self.last_path.clone_from(&last.path);
        }
        self.file_names
            .extend(completed.into_iter().map(|file| file.name));
        self.locale = locale;
        let started_at = *self.started_at.get_or_insert(now);
        self.flush_at = Some((now + NOTIFY_DEBOUNCE).min(started_at + NOTIFY_MAX_WAIT));
    }
}

impl PendingFailures {
    /// 并入一个失败任务并重排发送时刻：规则同 [`PendingCompletions::push`]。
    fn push(&mut self, task: FailedTask, now: Instant) {
        self.tasks.push(task);
        let started_at = *self.started_at.get_or_insert(now);
        self.flush_at = Some((now + NOTIFY_DEBOUNCE).min(started_at + NOTIFY_MAX_WAIT));
    }
}

/// 取下一个失败任务；未接入失败通知时永远挂起。通道关闭返回 `None`。
async fn recv_failure(receiver: &mut Option<FailureReceiver>) -> Option<FailedTask> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// 本帧若是带非空 `notify_titles` 的 RSS 条目变更，返回这些标题。
fn rss_notify_titles(frame: &fluxdown_protocol::EventFrame) -> Option<&[String]> {
    use fluxdown_protocol::{DaemonEvent, ServiceEvent, WsServerMsg};
    match &frame.event {
        ServiceEvent::Daemon(DaemonEvent::Engine(WsServerMsg::RssItemsChanged {
            notify_titles,
            ..
        })) if !notify_titles.is_empty() => Some(notify_titles),
        _ => None,
    }
}

/// 界面语言偏好（与托盘同源：`general.locale`，`system` / 缺省 = 跟随系统）。
pub(crate) fn locale_preference(snapshot: &AgentSnapshot) -> Option<String> {
    snapshot
        .preferences
        .values
        .get("general.locale")
        .and_then(serde_json::Value::as_str)
        .filter(|locale| *locale != "system")
        .map(str::to_owned)
}

fn agent_snapshot(snapshot: fluxdown_protocol::Snapshot) -> AgentSnapshot {
    match snapshot.body {
        SnapshotBody::Agent(snapshot) => *snapshot,
        SnapshotBody::Daemon(_) => AgentSnapshot::default(),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CompletedFile {
    name: String,
    path: Option<std::path::PathBuf>,
}

fn is_active(status: i32) -> bool {
    matches!(status, 1 | 5)
}

/// 一帧里受影响的任务 id。
#[derive(Clone, Copy)]
enum TaskIds<'a> {
    One(&'a str),
    Tasks(&'a [TaskDto]),
    Ids(&'a [String]),
}

impl TaskIds<'_> {
    fn for_each(self, mut visit: impl FnMut(&str)) {
        match self {
            Self::One(id) => visit(id),
            Self::Tasks(tasks) => tasks.iter().for_each(|task| visit(&task.task_id)),
            Self::Ids(ids) => ids.iter().for_each(|id| visit(id)),
        }
    }
}

/// 一帧对本模块关心的状态（任务状态集 / 保持唤醒偏好）的影响。
#[derive(Clone, Copy)]
enum Change<'a> {
    /// 无关：不读快照、不拿 hub 锁。
    None,
    /// 偏好变更（保持唤醒开关）。
    Preferences,
    /// 只触及这些任务。
    Tasks(TaskIds<'a>),
    /// 任务表被整体替换（快照替换 / 重连）或本接收端落后：整表对账一次。
    Rebuild,
}

/// 分类一帧。覆盖 `apply_daemon_event` 里所有会增删任务或改写 `status` 的事件；高频的
/// `TaskProgress` 在状态与镜像一致（或任务未知）时不需要读快照。
fn change_for<'a>(frame: &'a EventFrame, tracker: &TaskTracker) -> Change<'a> {
    let event = match &frame.event {
        ServiceEvent::Daemon(event) | ServiceEvent::Agent(AgentEvent::Daemon(event)) => event,
        ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(_)) => return Change::Rebuild,
        ServiceEvent::Agent(AgentEvent::PreferencesChanged(_)) => return Change::Preferences,
        ServiceEvent::Agent(_) => return Change::None,
    };
    match event {
        DaemonEvent::SnapshotReplaced(_)
        | DaemonEvent::TablesReloading { tasks: Some(_), .. }
        | DaemonEvent::Engine(WsServerMsg::TasksSnapshot { .. }) => Change::Rebuild,
        DaemonEvent::TaskChanged(task) => Change::Tasks(TaskIds::One(&task.task_id)),
        DaemonEvent::TasksChanged(tasks) => Change::Tasks(TaskIds::Tasks(tasks)),
        DaemonEvent::TaskDeleted { task_id } => Change::Tasks(TaskIds::One(task_id)),
        DaemonEvent::TasksDeleted { task_ids } => Change::Tasks(TaskIds::Ids(task_ids)),
        DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id,
            status,
            error_message,
            ..
        }) => {
            let deleted = *status == 4 && error_message == "deleted";
            match tracker.statuses.get(task_id) {
                Some(known) if deleted || known != status => Change::Tasks(TaskIds::One(task_id)),
                _ => Change::None,
            }
        }
        _ => Change::None,
    }
}

/// 任务状态镜像：只在受影响的任务上增量更新，并维护活跃（status 1|5）任务计数——
/// 每帧都全表扫描会让持锁时间随任务数线性增长（几万任务导入时拖垮整条事件链）。
#[derive(Default)]
struct TaskTracker {
    statuses: HashMap<String, i32>,
    active: usize,
}

impl TaskTracker {
    fn has_active(&self) -> bool {
        self.active > 0
    }

    /// 用整张任务表重建镜像，返回相对旧镜像新转为完成（status 3）且需要通知的文件。
    /// 首次出现的任务只登记不通知；已不在表中的任务被剔除。
    fn rebuild(&mut self, snapshot: &AgentSnapshot) -> Vec<CompletedFile> {
        let enabled = notify_on_complete(snapshot);
        let tasks = &snapshot.daemon.tasks;
        let mut statuses = HashMap::with_capacity(tasks.len());
        let mut completed = Vec::new();
        for task in tasks {
            if enabled
                && task.status == 3
                && self
                    .statuses
                    .get(&task.task_id)
                    .is_some_and(|previous| *previous != 3)
            {
                completed.push(completed_file(task));
            }
            statuses.insert(task.task_id.clone(), task.status);
        }
        self.active = statuses
            .values()
            .filter(|status| is_active(**status))
            .count();
        self.statuses = statuses;
        completed
    }

    /// 只按 `ids` 以快照里的最新状态刷新镜像：存在则登记 / 更新（跃迁到完成时通知），
    /// 不存在则剔除。耗时与 `ids` 个数成正比，与任务总数无关。
    fn refresh(&mut self, snapshot: &AgentSnapshot, ids: TaskIds<'_>) -> Vec<CompletedFile> {
        let enabled = notify_on_complete(snapshot);
        let mut completed = Vec::new();
        ids.for_each(|id| match snapshot.daemon.task(id) {
            Some(task) => match self.statuses.get_mut(id) {
                Some(previous) => {
                    if enabled && task.status == 3 && *previous != 3 {
                        completed.push(completed_file(task));
                    }
                    match (is_active(*previous), is_active(task.status)) {
                        (false, true) => self.active += 1,
                        (true, false) => self.active = self.active.saturating_sub(1),
                        _ => {}
                    }
                    *previous = task.status;
                }
                None => {
                    self.statuses.insert(task.task_id.clone(), task.status);
                    if is_active(task.status) {
                        self.active += 1;
                    }
                }
            },
            None => {
                if let Some(previous) = self.statuses.remove(id)
                    && is_active(previous)
                {
                    self.active = self.active.saturating_sub(1);
                }
            }
        });
        completed
    }
}

fn completed_file(task: &TaskDto) -> CompletedFile {
    CompletedFile {
        name: task.file_name.clone(),
        path: completion_path(&task.save_dir, &task.file_name),
    }
}

/// 一次观察的结果。
struct Observation {
    completed: Vec<CompletedFile>,
    /// 有完成通知时的界面语言偏好。
    locale: Option<String>,
    /// 需要重新协调「保持唤醒」时的目标状态；`None` = 与上次相比没有变化可能。
    hold: Option<bool>,
}

/// 在 hub 锁内按帧影响更新镜像，工作量与帧内任务数成正比。
fn observe(tracker: &mut TaskTracker, snapshot: &AgentSnapshot, change: Change<'_>) -> Observation {
    let had_active = tracker.has_active();
    let completed = match change {
        Change::None | Change::Preferences => Vec::new(),
        Change::Tasks(ids) => tracker.refresh(snapshot, ids),
        Change::Rebuild => tracker.rebuild(snapshot),
    };
    let reevaluate = matches!(change, Change::Preferences | Change::Rebuild)
        || had_active != tracker.has_active();
    let locale = if completed.is_empty() {
        None
    } else {
        locale_preference(snapshot)
    };
    Observation {
        completed,
        locale,
        hold: reevaluate.then(|| {
            preference_bool(snapshot, KEEP_AWAKE_PREF, KEEP_AWAKE_DEFAULT) && tracker.has_active()
        }),
    }
}

/// 保持唤醒状态：`failed` 记住本次「应持有」区间内的获取失败（无 logind 等环境），
/// 在不再需要持有之前不重试，避免每个进度帧都新建连接并刷警告。
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[derive(Default)]
struct AwakeState {
    guard: Option<keepawake::KeepAwake>,
    failed: bool,
}

/// 移动平台没有进程内的休眠抑制 API（由宿主应用的前台服务 / 后台任务保活）。
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
#[derive(Default)]
struct AwakeState;

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
async fn reconcile_awake(_should_hold: bool, _awake: &mut AwakeState) {}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
async fn reconcile_awake(should_hold: bool, awake: &mut AwakeState) {
    if should_hold && awake.guard.is_none() && !awake.failed {
        match tokio::task::spawn_blocking(|| {
            keepawake::Builder::default()
                .idle(true)
                .sleep(true)
                .reason("FluxDown active download")
                .app_name("FluxDown")
                .app_reverse_domain("dev.zerx.fluxdown")
                .create()
        })
        .await
        {
            Ok(Ok(guard)) => awake.guard = Some(guard),
            Ok(Err(error)) => {
                awake.failed = true;
                tracing::warn!(error = %error, "could not inhibit sleep");
            }
            Err(error) => {
                awake.failed = true;
                tracing::warn!(error = %error, "keep-awake worker failed");
            }
        }
    } else if !should_hold {
        awake.guard = None;
        awake.failed = false;
    }
}

/// 下载完成通知开关；未写过偏好时默认开启——与设置页开关
/// （`crates/settings/src/sections/notify.rs`）及 Web / 原生移动端设置目录的默认值一致，
/// 否则开关显示为开、实际却不通知。
const NOTIFY_ON_COMPLETE_PREF: &str = "download.notify_on_complete";
const NOTIFY_ON_COMPLETE_DEFAULT: bool = true;
/// 下载期间保持唤醒；默认关闭（与设置页开关一致）。
const KEEP_AWAKE_PREF: &str = "download.keep_awake";
const KEEP_AWAKE_DEFAULT: bool = false;

fn preference_bool(snapshot: &AgentSnapshot, key: &str, default: bool) -> bool {
    snapshot
        .preferences
        .values
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default)
}

/// 完成通知总开关（RSS 自动下载通知同受其约束；Doctor 据此判断是否需要检查通知权限）。
pub(crate) fn notify_on_complete(snapshot: &AgentSnapshot) -> bool {
    preference_bool(
        snapshot,
        NOTIFY_ON_COMPLETE_PREF,
        NOTIFY_ON_COMPLETE_DEFAULT,
    )
}

/// 下载失败通知开关（设备本地偏好，缺省开启；与完成通知互不影响）。
pub(crate) fn notify_on_failure(snapshot: &AgentSnapshot) -> bool {
    preference_bool(snapshot, NOTIFY_ON_FAIL_PREF, NOTIFY_ON_FAIL_DEFAULT)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use fluxdown_protocol::{AgentPreferencesDto, AgentSnapshot, TaskDto};
    use serde_json::json;

    use fluxdown_protocol::{AgentEvent, DaemonEvent, EventFrame, ServiceEvent, WsServerMsg};

    use super::{
        Change, CompletedFile, FailedTask, KEEP_AWAKE_DEFAULT, KEEP_AWAKE_PREF, NOTIFY_DEBOUNCE,
        NOTIFY_MAX_WAIT, PendingCompletions, PendingFailures, TaskTracker, change_for,
        locale_preference, notify_on_complete, notify_on_failure, observe, preference_bool,
    };

    #[test]
    fn completion_batches_debounce_but_never_wait_past_the_cap() {
        let start = tokio::time::Instant::now();
        let mut pending = PendingCompletions::default();
        pending.push(vec![completed("a.bin")], None, start);
        assert_eq!(pending.flush_at, Some(start + NOTIFY_DEBOUNCE));

        // 连续完成不断顺延，但封顶于首个完成后 NOTIFY_MAX_WAIT。
        let late = start + NOTIFY_MAX_WAIT - Duration::from_millis(100);
        pending.push(vec![completed("b.bin")], Some("zh".to_owned()), late);
        assert_eq!(pending.flush_at, Some(start + NOTIFY_MAX_WAIT));
        assert_eq!(pending.file_names, ["a.bin", "b.bin"]);
        assert_eq!(pending.last_path, completed("b.bin").path);
        assert_eq!(pending.locale.as_deref(), Some("zh"));
    }

    fn completed(name: &str) -> CompletedFile {
        CompletedFile {
            name: name.to_owned(),
            path: Some(std::env::temp_dir().join(name)),
        }
    }

    #[test]
    fn batch_without_final_path_never_targets_an_earlier_file() {
        let mut pending = PendingCompletions::default();
        let now = tokio::time::Instant::now();
        pending.push(vec![completed("a.bin")], None, now);
        pending.push(
            vec![CompletedFile {
                name: "b.bin".into(),
                path: None,
            }],
            None,
            now,
        );
        assert_eq!(pending.last_path, None);
        assert_eq!(pending.file_names, ["a.bin", "b.bin"]);
    }

    #[test]
    fn locale_preference_treats_system_as_follow_os() {
        let mut snapshot = AgentSnapshot::default();
        assert_eq!(locale_preference(&snapshot), None);
        snapshot
            .preferences
            .values
            .insert("general.locale".to_owned(), json!("system"));
        assert_eq!(locale_preference(&snapshot), None);
        snapshot
            .preferences
            .values
            .insert("general.locale".to_owned(), json!("zh"));
        assert_eq!(locale_preference(&snapshot).as_deref(), Some("zh"));
    }

    fn task(task_id: &str, status: i32) -> Result<TaskDto, serde_json::Error> {
        serde_json::from_value(json!({
            "taskId": task_id,
            "url": "https://example.com/file",
            "fileName": format!("{task_id}.bin"),
            "saveDir": std::env::temp_dir(),
            "status": status,
            "downloadedBytes": 0,
            "totalBytes": 100,
            "errorMessage": "",
            "createdAt": "1",
            "proxyUrl": "",
            "queueId": "main",
            "checksum": ""
        }))
    }

    #[test]
    fn rebuild_notifies_new_completions_once_and_forgets_deleted_tasks()
    -> Result<(), serde_json::Error> {
        let mut snapshot = AgentSnapshot::default();
        snapshot
            .preferences
            .values
            .insert("download.notify_on_complete".to_owned(), json!(true));
        let mut tracker = TaskTracker::default();
        let mut rebuild = |snapshot: &AgentSnapshot| tracker.rebuild(snapshot);

        // 首次出现的任务（含已完成的）只登记，不通知。
        snapshot.daemon.tasks = vec![task("a", 1)?, task("b", 3)?];
        assert!(rebuild(&snapshot).is_empty());

        // a 从下载中转为完成：恰好通知一次，重复对账不再通知。
        snapshot.daemon.tasks = vec![task("a", 3)?, task("b", 3)?];
        assert_eq!(rebuild(&snapshot), [completed("a.bin")]);
        assert!(rebuild(&snapshot).is_empty());

        // b 被删除后以同 id 重新出现并直接是完成态：视为新任务，不通知。
        snapshot.daemon.tasks = vec![task("a", 3)?];
        assert!(rebuild(&snapshot).is_empty());
        snapshot.daemon.tasks = vec![task("a", 3)?, task("b", 3)?];
        assert!(rebuild(&snapshot).is_empty());

        // 关闭通知开关后，跃迁仍被记录但不产生通知。
        snapshot
            .preferences
            .values
            .insert("download.notify_on_complete".to_owned(), json!(false));
        snapshot.daemon.tasks = vec![task("a", 1)?, task("b", 3)?];
        assert!(rebuild(&snapshot).is_empty());
        snapshot.daemon.tasks = vec![task("a", 3)?, task("b", 3)?];
        assert!(rebuild(&snapshot).is_empty());
        assert_eq!(tracker.statuses.get("a"), Some(&3));
        Ok(())
    }

    fn progress_frame(task_id: &str, status: i32, error_message: &str) -> EventFrame {
        EventFrame {
            epoch: "epoch".to_owned(),
            sequence: 1,
            previous: None,
            event: ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::Engine(
                WsServerMsg::TaskProgress {
                    task_id: task_id.to_owned(),
                    status,
                    downloaded_bytes: 0,
                    total_bytes: 100,
                    speed: 0,
                    file_name: String::new(),
                    save_dir: String::new(),
                    upload_speed: 0,
                    url: String::new(),
                    error_message: error_message.to_owned(),
                    uploaded_bytes: 0,
                    seeding_status: 0,
                    seeding_message: String::new(),
                    seeding_time_secs: 0,
                },
            ))),
        }
    }

    fn daemon_frame(event: DaemonEvent) -> EventFrame {
        EventFrame {
            epoch: "epoch".to_owned(),
            sequence: 1,
            previous: None,
            event: ServiceEvent::Agent(AgentEvent::Daemon(event)),
        }
    }

    /// 按帧增量维护的镜像，在任何帧序列之后都与整表对账（旧实现的逐帧全表扫描）等价：
    /// 完成通知、任务集与「是否有 status 1|5 的任务」一致。
    #[test]
    fn incremental_updates_match_a_full_scan_after_every_frame() -> Result<(), serde_json::Error> {
        let mut snapshot = AgentSnapshot::default();
        snapshot.daemon.tasks = vec![task("a", 0)?, task("b", 1)?, task("c", 3)?];
        let mut tracker = TaskTracker::default();
        assert!(tracker.rebuild(&snapshot).is_empty());

        let frames = [
            // 任务 a 开始下载，b 完成（通知），已知任务重复进度（不碰快照）。
            (progress_frame("a", 1, ""), vec![]),
            (progress_frame("b", 3, ""), vec!["b.bin"]),
            (progress_frame("b", 3, ""), vec![]),
            // 未知任务的进度不进入任务集。
            (progress_frame("ghost", 3, ""), vec![]),
            // 批量新增：新任务只登记；已存在的 a 完成（通知）。
            (
                daemon_frame(DaemonEvent::TasksChanged(vec![
                    task("a", 3)?,
                    task("d", 5)?,
                    task("e", 3)?,
                ])),
                vec!["a.bin"],
            ),
            // 删除：进度帧里的 deleted 标记与 TaskDeleted / TasksDeleted。
            (progress_frame("d", 4, "deleted"), vec![]),
            (
                daemon_frame(DaemonEvent::TaskDeleted {
                    task_id: "e".to_owned(),
                }),
                vec![],
            ),
            (
                daemon_frame(DaemonEvent::TasksDeleted {
                    task_ids: vec!["a".to_owned(), "nope".to_owned()],
                }),
                vec![],
            ),
            // 删除后同 id 以完成态重新出现：视为新任务，不通知。
            (
                daemon_frame(DaemonEvent::TaskChanged(task("a", 3)?)),
                vec![],
            ),
            (
                daemon_frame(DaemonEvent::TaskChanged(task("a", 1)?)),
                vec![],
            ),
            (progress_frame("a", 3, ""), vec!["a.bin"]),
        ];
        for (frame, expected) in frames {
            let change = change_for(&frame, &tracker);
            fluxdown_protocol::apply_agent_event(
                &mut snapshot,
                match &frame.event {
                    ServiceEvent::Agent(event) => event,
                    ServiceEvent::Daemon(_) => unreachable!("agent frames only"),
                },
            );
            let observed = observe(&mut tracker, &snapshot, change);
            let names: Vec<_> = observed
                .completed
                .iter()
                .map(|file| file.name.as_str())
                .collect();
            assert_eq!(names, expected);

            let table: HashMap<_, _> = snapshot
                .daemon
                .tasks
                .iter()
                .map(|task| (task.task_id.clone(), task.status))
                .collect();
            assert_eq!(tracker.statuses, table);
            assert_eq!(
                tracker.has_active(),
                snapshot
                    .daemon
                    .tasks
                    .iter()
                    .any(|task| matches!(task.status, 1 | 5))
            );
        }
        Ok(())
    }

    #[test]
    fn keep_awake_is_only_reevaluated_on_relevant_changes() -> Result<(), serde_json::Error> {
        let mut snapshot = AgentSnapshot::default();
        snapshot
            .preferences
            .values
            .insert("download.keep_awake".to_owned(), json!(true));
        snapshot.daemon.tasks = vec![task("a", 0)?];
        let mut tracker = TaskTracker::default();
        let initial = observe(&mut tracker, &snapshot, Change::Rebuild);
        assert_eq!(initial.hold, Some(false));

        // 状态不变的进度帧不需要读快照，也不触发重新评估。
        let idle = progress_frame("a", 0, "");
        assert!(matches!(change_for(&idle, &tracker), Change::None));

        snapshot.daemon.tasks = vec![task("a", 1)?];
        let started = progress_frame("a", 1, "");
        let change = change_for(&started, &tracker);
        assert_eq!(observe(&mut tracker, &snapshot, change).hold, Some(true));

        // 活跃数没有变化的任务变更不重新评估；偏好变更总是重新评估。
        snapshot.daemon.tasks = vec![task("a", 5)?];
        let still_active = progress_frame("a", 5, "");
        let change = change_for(&still_active, &tracker);
        assert_eq!(observe(&mut tracker, &snapshot, change).hold, None);
        assert_eq!(
            observe(&mut tracker, &snapshot, Change::Preferences).hold,
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn unset_preferences_fall_back_to_settings_page_defaults() {
        let mut snapshot = AgentSnapshot {
            preferences: AgentPreferencesDto::default(),
            ..AgentSnapshot::default()
        };
        assert!(notify_on_complete(&snapshot));
        assert!(!should_keep_awake_pref(&snapshot));
        snapshot
            .preferences
            .values
            .insert("download.notify_on_complete".to_owned(), json!(false));
        snapshot
            .preferences
            .values
            .insert("download.keep_awake".to_owned(), json!(true));
        assert!(!notify_on_complete(&snapshot));
        assert!(should_keep_awake_pref(&snapshot));
    }

    #[test]
    fn failure_notification_defaults_on_and_is_independent_of_completion() {
        let mut snapshot = AgentSnapshot::default();
        assert!(notify_on_failure(&snapshot));
        snapshot
            .preferences
            .values
            .insert("desktop.notify_on_fail".to_owned(), json!(false));
        assert!(!notify_on_failure(&snapshot));
        assert!(notify_on_complete(&snapshot));
        snapshot
            .preferences
            .values
            .insert("download.notify_on_complete".to_owned(), json!(false));
        snapshot
            .preferences
            .values
            .insert("desktop.notify_on_fail".to_owned(), json!(true));
        assert!(notify_on_failure(&snapshot));
    }

    #[test]
    fn failure_batches_debounce_but_never_wait_past_the_cap() {
        let failed = || FailedTask {
            name: "a.bin".to_owned(),
            reason: String::new(),
        };
        let start = tokio::time::Instant::now();
        let mut pending = PendingFailures::default();
        pending.push(failed(), start);
        assert_eq!(pending.flush_at, Some(start + NOTIFY_DEBOUNCE));
        // 持续有失败：每次顺延，但不超过首个失败起算的上限。
        let mut now = start;
        while now < start + NOTIFY_MAX_WAIT {
            now += Duration::from_millis(500);
            pending.push(failed(), now);
        }
        assert_eq!(pending.flush_at, Some(start + NOTIFY_MAX_WAIT));
        assert_eq!(pending.tasks.len(), 7);
    }

    fn should_keep_awake_pref(snapshot: &AgentSnapshot) -> bool {
        preference_bool(snapshot, KEEP_AWAKE_PREF, KEEP_AWAKE_DEFAULT)
    }
}
