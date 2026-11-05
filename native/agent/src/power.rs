//! 定时关机 / 睡眠：排定时有任务（活跃 / 排队 / 待重试）就等全部完成后再倒计时，没有任务则
//! 立即倒计时；倒计时中若又出现任务则取消倒计时回到 armed（不完全解除），任务再次清空后重新
//! 计时。到点执行平台关机或睡眠命令。
//!
//! 状态机归 agent：关闭全部 UI（托盘驻留）后仍需按时执行。UI 只经 `agent.power.*`
//! 发请求、经 `AgentEvent::PowerChanged` 渲染。

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
use std::process::{Command, ExitStatus};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fluxdown_protocol::{AgentEvent, DaemonRuntimeStatsDto, PowerAction, PowerStatusDto};
use tokio_util::sync::CancellationToken;

use crate::event_hub::AgentEventHub;

/// 状态刷新 / 到点检查间隔。
const TICK: Duration = Duration::from_secs(1);

pub struct PowerService {
    state: Mutex<PowerState>,
    events: AgentEventHub,
}

impl PowerService {
    #[must_use]
    pub fn new(events: AgentEventHub) -> Self {
        Self {
            state: Mutex::new(PowerState::new()),
            events,
        }
    }

    /// 排定 `action`：有任务时全部完成后再等 `delay`，没有任务时立即开始 `delay` 倒计时。
    /// 没有任务却要求「完成后立即」（`delay = 0`）时拒绝并返回 `false`，避免误点即刻关机。
    pub fn arm(&self, delay: Duration, action: PowerAction) -> bool {
        let (active, pending) = self
            .events
            .inspect(|snapshot| work_counts(&snapshot.daemon.runtime_stats));
        let has_work = active > 0 || pending > 0;
        let accepted = self.lock().arm(delay, action, has_work, Instant::now());
        self.publish();
        accepted
    }

    /// 取消已排定的操作（无论是等待任务完成还是正在倒计时）。
    pub fn disarm(&self) {
        self.lock().disarm();
        self.publish();
    }

    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = interval.tick() => {}
            }
            let (active, pending) = self
                .events
                .inspect(|snapshot| work_counts(&snapshot.daemon.runtime_stats));
            let fired = self.lock().advance(active, pending, Instant::now());
            self.publish();
            if let Some(action) = fired {
                tokio::task::spawn_blocking(move || execute(action));
            }
        }
    }

    fn publish(&self) {
        let status = self.lock().status(Instant::now());
        if self.events.inspect(|snapshot| snapshot.power) != status {
            self.events.publish(AgentEvent::PowerChanged(status));
        }
    }

    fn lock(&self) -> MutexGuard<'_, PowerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Clone, Copy)]
struct Armed {
    delay: Duration,
    action: PowerAction,
}

/// 定时关机 / 睡眠的纯状态机：`Instant` 由调用方传入，便于单测构造固定时钟。
struct PowerState {
    armed: Option<Armed>,
    countdown_until: Option<Instant>,
}

impl PowerState {
    fn new() -> Self {
        Self {
            armed: None,
            countdown_until: None,
        }
    }

    /// 返回是否被接受；没有任务时倒计时从 `now` 起算。
    fn arm(&mut self, delay: Duration, action: PowerAction, has_work: bool, now: Instant) -> bool {
        if delay.is_zero() && !has_work {
            return false;
        }
        self.armed = Some(Armed { delay, action });
        self.countdown_until = (!has_work).then(|| now + delay);
        true
    }

    fn disarm(&mut self) {
        self.armed = None;
        self.countdown_until = None;
    }

    /// 按当前任务计数与时钟推进一次；到点时返回要执行的操作（armed 状态已复位）。
    fn advance(
        &mut self,
        active_tasks: u32,
        pending_tasks: u32,
        now: Instant,
    ) -> Option<PowerAction> {
        let armed = self.armed?;
        if active_tasks == 0 && pending_tasks == 0 {
            if self.countdown_until.is_none() {
                self.countdown_until = Some(now + armed.delay);
            }
        } else {
            // 倒计时中又出现活跃 / 等待任务：取消倒计时，回到 armed（不完全解除）。
            self.countdown_until = None;
        }
        let until = self.countdown_until?;
        if now < until {
            return None;
        }
        self.disarm();
        Some(armed.action)
    }

    fn status(&self, now: Instant) -> PowerStatusDto {
        PowerStatusDto {
            armed_delay_secs: self.armed.map(|armed| armed.delay.as_secs()),
            countdown_remaining_secs: self
                .countdown_until
                .map(|until| until.saturating_duration_since(now).as_secs()),
            action: self.armed.map(|armed| armed.action).unwrap_or_default(),
        }
    }
}

/// `(活跃任务数, 仍在等待的任务数)`：等待 = 排队 + 已排程自动重试、尚未重新派发的任务，
/// 后者在任务表里是失败状态，不计入就会在重试间隙被误判为「全部完成」。
fn work_counts(stats: &DaemonRuntimeStatsDto) -> (u32, u32) {
    (
        stats.active_tasks,
        stats
            .pending_tasks
            .saturating_add(stats.retry_pending_tasks),
    )
}

/// 执行平台电源操作；`FLUXDOWN_SHUTDOWN_DRY_RUN` 设置时只记日志，便于 QA 验证不真的关机 / 睡眠。
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn execute(action: PowerAction) {
    if std::env::var_os("FLUXDOWN_SHUTDOWN_DRY_RUN").is_some() {
        tracing::warn!(?action, "power dry-run: would run the scheduled action now");
        return;
    }
    tracing::info!(?action, "scheduled power action is due");
    let result = match action {
        PowerAction::Shutdown => power_off(),
        PowerAction::Sleep => suspend(),
    };
    match result {
        Ok(status) if status.success() => tracing::info!(?action, "power request accepted"),
        Ok(status) => tracing::error!(%status, ?action, "power request failed"),
        Err(error) => tracing::error!(%error, ?action, "could not run power command"),
    }
}

#[cfg(target_os = "windows")]
fn power_off() -> std::io::Result<ExitStatus> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("shutdown")
        .args(["/s", "/f", "/t", "0"])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
}

/// .NET `Application.SetSuspendState(Suspend)`（即 `SetSuspendState(FALSE, …)`，不经 FFI）。
/// 传统 S3 机器进入睡眠；Modern Standby（S0 低功耗空闲）机器上系统不支持该调用进入
/// 待机：启用了休眠时会改为休眠，否则返回失败（脚本以退出码 1 报告，记为失败日志）。
#[cfg(target_os = "windows")]
fn suspend() -> std::io::Result<ExitStatus> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = "Add-Type -AssemblyName System.Windows.Forms; \
        if (-not [System.Windows.Forms.Application]::SetSuspendState(\
        [System.Windows.Forms.PowerState]::Suspend, $false, $false)) { exit 1 }";
    let powershell = crate::permission::Tools::locate(crate::permission::Os::Windows)
        .powershell
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "powershell.exe"))?;
    Command::new(powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
}

#[cfg(target_os = "macos")]
fn power_off() -> std::io::Result<ExitStatus> {
    Command::new("osascript")
        .args(["-e", "tell app \"System Events\" to shut down"])
        .status()
}

/// `pmset sleepnow` 无需 root，也不经 System Events（不触发自动化授权）。
#[cfg(target_os = "macos")]
fn suspend() -> std::io::Result<ExitStatus> {
    Command::new("/usr/bin/pmset").arg("sleepnow").status()
}

#[cfg(target_os = "linux")]
fn power_off() -> std::io::Result<ExitStatus> {
    logind("poweroff")
}

#[cfg(target_os = "linux")]
fn suspend() -> std::io::Result<ExitStatus> {
    logind("suspend")
}

/// systemd-logind（`systemctl` 优先，失败再试 `loginctl`，后者也覆盖 elogind）。
/// logind 不让调用者自己持有的 block 抑制锁（下载中保持唤醒）拦住本用户的请求。
#[cfg(target_os = "linux")]
fn logind(verb: &str) -> std::io::Result<ExitStatus> {
    match Command::new("systemctl").arg(verb).status() {
        Ok(status) if status.success() => return Ok(status),
        Ok(status) => tracing::warn!(%status, verb, "systemctl failed; trying loginctl"),
        Err(error) => tracing::warn!(%error, verb, "could not run systemctl; trying loginctl"),
    }
    Command::new("loginctl").arg(verb).status()
}

/// Android / iOS 的应用不能关机或睡眠：宿主不应暴露该功能，误调用时只记录。
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn execute(action: PowerAction) {
    tracing::warn!(
        ?action,
        "scheduled power actions are not supported on this platform"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHUTDOWN: PowerAction = PowerAction::Shutdown;

    #[test]
    fn immediate_needs_work_but_timer_does_not() {
        let mut state = PowerState::new();
        let now = Instant::now();
        assert!(!state.arm(Duration::ZERO, SHUTDOWN, false, now));
        assert_eq!(state.status(now), PowerStatusDto::default());

        assert!(state.arm(Duration::from_secs(60), PowerAction::Sleep, false, now));
        let status = state.status(now);
        assert_eq!(status.armed_delay_secs, Some(60));
        assert_eq!(status.countdown_remaining_secs, Some(60));
        assert_eq!(status.action, PowerAction::Sleep);
    }

    #[test]
    fn idle_timer_fires_its_action_when_due() {
        let mut state = PowerState::new();
        let now = Instant::now();
        state.arm(Duration::from_secs(60), PowerAction::Sleep, false, now);
        assert_eq!(state.advance(0, 0, now + Duration::from_secs(59)), None);
        assert_eq!(
            state.advance(0, 0, now + Duration::from_secs(60)),
            Some(PowerAction::Sleep)
        );
        assert_eq!(state.status(now), PowerStatusDto::default());
    }

    #[test]
    fn countdown_starts_only_after_all_tasks_finish() {
        let mut state = PowerState::new();
        let now = Instant::now();
        assert!(state.arm(Duration::from_secs(60), SHUTDOWN, true, now));
        assert_eq!(state.advance(1, 0, now), None);
        assert_eq!(state.status(now).countdown_remaining_secs, None);
        assert_eq!(state.advance(0, 1, now), None);
        assert_eq!(state.status(now).countdown_remaining_secs, None);
        assert_eq!(state.advance(0, 0, now), None);
        assert_eq!(state.status(now).countdown_remaining_secs, Some(60));
    }

    #[test]
    fn new_task_pauses_an_idle_timer_until_it_finishes() {
        let mut state = PowerState::new();
        let now = Instant::now();
        state.arm(Duration::from_secs(60), SHUTDOWN, false, now);
        let later = now + Duration::from_secs(30);
        assert_eq!(state.advance(1, 0, later), None);
        let status = state.status(later);
        assert_eq!(status.countdown_remaining_secs, None);
        assert_eq!(status.armed_delay_secs, Some(60));
        // 任务结束后按完整延迟重新计时，不沿用被打断前的剩余时间。
        assert_eq!(state.advance(0, 0, later), None);
        assert_eq!(state.status(later).countdown_remaining_secs, Some(60));
    }

    #[test]
    fn countdown_fires_once_when_delay_elapses() {
        let mut state = PowerState::new();
        let now = Instant::now();
        state.arm(Duration::from_secs(5), SHUTDOWN, true, now);
        state.advance(0, 0, now);
        assert_eq!(state.advance(0, 0, now + Duration::from_secs(4)), None);
        assert_eq!(
            state.advance(0, 0, now + Duration::from_secs(5)),
            Some(SHUTDOWN)
        );
        assert_eq!(state.advance(0, 0, now + Duration::from_secs(6)), None);
        assert_eq!(
            state.status(now + Duration::from_secs(6)),
            PowerStatusDto::default()
        );
    }

    #[test]
    fn zero_delay_fires_on_first_idle_tick() {
        let mut state = PowerState::new();
        let now = Instant::now();
        state.arm(Duration::ZERO, PowerAction::Sleep, true, now);
        assert_eq!(state.advance(0, 0, now), Some(PowerAction::Sleep));
    }

    #[test]
    fn auto_retry_gap_keeps_the_countdown_from_starting() {
        let stats = |retry_pending_tasks| DaemonRuntimeStatsDto {
            retry_pending_tasks,
            ..DaemonRuntimeStatsDto::default()
        };
        let mut state = PowerState::new();
        let now = Instant::now();
        state.arm(Duration::ZERO, SHUTDOWN, true, now);
        // 最后一个任务在重试间隙：活跃 / 排队都是 0，但仍有重试待定，不得触发关机。
        let (active, waiting) = work_counts(&stats(1));
        assert_eq!(state.advance(active, waiting, now), None);
        assert_eq!(state.status(now).countdown_remaining_secs, None);
        // 重试重新派发后任务活跃，仍不关机；全部结束后才到点。
        assert_eq!(state.advance(1, 0, now), None);
        let (active, waiting) = work_counts(&stats(0));
        assert_eq!(state.advance(active, waiting, now), Some(SHUTDOWN));
    }

    #[test]
    fn disarm_clears_countdown() {
        let mut state = PowerState::new();
        let now = Instant::now();
        state.arm(Duration::from_secs(60), SHUTDOWN, false, now);
        state.disarm();
        assert_eq!(state.status(now), PowerStatusDto::default());
        assert_eq!(state.advance(0, 0, now + Duration::from_secs(120)), None);
    }
}
