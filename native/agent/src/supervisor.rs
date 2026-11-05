//! agent 对同级 `fluxdownd` 的单飞启动与异步回收。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Mutex;

/// daemon 启动错误。
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("could not locate sibling fluxdownd: {0}")]
    Locate(#[from] std::io::Error),
    #[error("failed to spawn fluxdownd: {0}")]
    Spawn(String),
}

/// mimalloc 回收策略：释放的内存立即归还系统（默认 1s 延迟，且只在之后还有分配器
/// 活动时才真正归还；空闲静默的 daemon 在一次大块收尾后会长期占着峰值 RSS）。
/// mimalloc 只在进程启动时读环境变量，所以由拉起方注入；父进程已设置时沿用其值。
const ALLOCATOR_ENV: [(&str, &str); 1] = [("MIMALLOC_PURGE_DELAY", "0")];

/// 父环境里尚未设置的分配器变量。
fn allocator_env(
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> impl Iterator<Item = (&'static str, &'static str)> {
    ALLOCATOR_ENV
        .into_iter()
        .filter(move |(key, _)| lookup(key).is_none())
}

#[derive(Default)]
struct SupervisorState {
    generation: u64,
    running: bool,
    reapers: Vec<tokio::task::JoinHandle<()>>,
}

/// 只在连接拒绝路径调用的 daemon 单飞启动器。
pub struct DaemonSupervisor {
    state: Arc<Mutex<SupervisorState>>,
    /// 显式 daemon 地址（`FLUXDOWN_DAEMON_URL`）：经 `FLUXDOWN_DAEMON_BIND` 交给 daemon，严格绑定。
    /// 为 `None` 时不干预该变量：daemon 使用默认端口，不可用时回退系统分配端口并发布端点文件。
    bind_addr: Option<SocketAddr>,
    /// 完全退出流程中置位：此后连接拒绝不再拉起 daemon。
    stopped: AtomicBool,
    /// 追加给 daemon 子进程的环境变量（如 server 模式生效的演示 URL）。
    extra_env: Vec<(String, String)>,
    /// daemon 子进程 stderr 的落盘文件；未设置或打开失败时丢弃。
    stderr_log: Option<PathBuf>,
    /// stderr 日志打开失败只告警一次。
    stderr_log_warned: AtomicBool,
    /// 连续快速异常退出次数（崩溃循环判定）。
    crash_streak: Arc<std::sync::atomic::AtomicU32>,
}

impl DaemonSupervisor {
    #[must_use]
    pub fn new(bind_addr: Option<SocketAddr>) -> Self {
        Self {
            state: Arc::new(Mutex::new(SupervisorState::default())),
            bind_addr,
            stopped: AtomicBool::new(false),
            extra_env: Vec::new(),
            stderr_log: None,
            stderr_log_warned: AtomicBool::new(false),
            crash_streak: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// 给拉起的 daemon 追加环境变量（覆盖继承的同名变量）。
    #[must_use]
    pub fn with_extra_env(mut self, env: Vec<(String, String)>) -> Self {
        self.extra_env = env;
        self
    }

    /// 把 daemon 的 stderr 追加到该文件（启动失败、panic 的唯一证据）；已超过 1 MiB 则截断重写。
    #[must_use]
    pub fn with_stderr_log(mut self, path: PathBuf) -> Self {
        self.stderr_log = Some(path);
        self
    }

    /// 永久停止监管（不可恢复）：随后的 [`Self::ensure_running`] 均为空操作。
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    /// 启动同级 daemon；短时间内并发/重复调用只产生一个子进程。
    ///
    /// 返回本进程所监管、仍存活的 daemon 子进程代际（刚拉起或早先拉起）；已停止监管时为
    /// `None`。代际让调用方区分「同一个子进程仍在初始化」与「子进程已退出又被重新拉起」。
    pub async fn ensure_running(&self) -> Result<Option<u64>, SupervisorError> {
        let mut state = self.state.lock().await;
        state.reapers.retain(|task| !task.is_finished());
        if self.stopped.load(Ordering::Acquire) {
            return Ok(None);
        }
        if state.running {
            return Ok(Some(state.generation));
        }
        let executable = daemon_executable()?;
        let mut command = std::process::Command::new(&executable);
        if let Some(bind_addr) = self.bind_addr {
            command.env("FLUXDOWN_DAEMON_BIND", bind_addr.to_string());
        }
        command
            .envs(allocator_env(|key| std::env::var_os(key)))
            .envs(self.extra_env.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(self.open_stderr_log());
        detach_background_process(&mut command);
        let mut child = tokio::process::Command::from(command)
            .spawn()
            .map_err(|error| SupervisorError::Spawn(format!("{error:#}")))?;
        let spawned_at = std::time::Instant::now();
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        tracing::info!(
            executable = %executable.display(),
            generation,
            pid = child.id(),
            "spawned fluxdownd"
        );
        state.running = true;
        let supervisor_state = self.state.clone();
        let streak = self.crash_streak.clone();
        state.reapers.push(tokio::spawn(async move {
            let mut success = false;
            match child.wait().await {
                Ok(status) if status.success() => {
                    success = true;
                    tracing::info!(%status, generation, "supervised fluxdownd exited");
                }
                Ok(status) => tracing::warn!(
                    %status,
                    generation,
                    uptime_secs = spawned_at.elapsed().as_secs_f64(),
                    "supervised fluxdownd exited abnormally; see fluxdownd.stderr.log"
                ),
                Err(error) => tracing::warn!(error = %error, "failed to reap fluxdownd"),
            }
            let previous = streak.load(Ordering::Acquire);
            streak.store(
                next_crash_streak(previous, success, spawned_at.elapsed()),
                Ordering::Release,
            );
            let mut state = supervisor_state.lock().await;
            if state.generation == generation {
                state.running = false;
            }
        }));
        Ok(Some(generation))
    }

    /// 本进程拉起的 daemon 子进程是否仍在运行（尚未被回收）。
    pub async fn child_alive(&self) -> bool {
        self.state.lock().await.running
    }

    /// 子进程是否处于「启动即崩溃」循环（连续快速异常退出）；调用方据此拉长重拉间隔。
    #[must_use]
    pub fn in_crash_loop(&self) -> bool {
        self.crash_streak() >= CRASH_LOOP_THRESHOLD
    }

    /// 当前连续快速异常退出次数（成功连上 daemon 后清零）。
    #[must_use]
    pub fn crash_streak(&self) -> u32 {
        self.crash_streak.load(Ordering::Acquire)
    }

    /// 已成功连上 daemon：之前的快速失败不再算数。
    pub fn clear_crash_streak(&self) {
        self.crash_streak.store(0, Ordering::Release);
    }
}

/// 连续快速异常退出达到该次数即视为崩溃循环。
const CRASH_LOOP_THRESHOLD: u32 = 3;
/// 存活不足该时长的异常退出计入崩溃循环。
const CRASH_FAST_EXIT: std::time::Duration = std::time::Duration::from_secs(30);

fn next_crash_streak(previous: u32, success: bool, uptime: std::time::Duration) -> u32 {
    if success || uptime >= CRASH_FAST_EXIT {
        0
    } else {
        previous.saturating_add(1)
    }
}

impl DaemonSupervisor {
    fn open_stderr_log(&self) -> Stdio {
        let Some(path) = &self.stderr_log else {
            return Stdio::null();
        };
        let mut options = std::fs::OpenOptions::new();
        options.create(true);
        let oversized = std::fs::metadata(path).is_ok_and(|meta| meta.len() > STDERR_LOG_MAX_BYTES);
        if oversized {
            options.write(true).truncate(true);
        } else {
            options.append(true);
        }
        if let Some(dir) = path.parent()
            && let Err(error) = std::fs::create_dir_all(dir)
        {
            if !self.stderr_log_warned.swap(true, Ordering::AcqRel) {
                tracing::warn!(path = %dir.display(), %error, "cannot create fluxdownd stderr log directory; discarding daemon stderr");
            }
            return Stdio::null();
        }
        match options.open(path) {
            Ok(file) => Stdio::from(file),
            Err(error) => {
                if !self.stderr_log_warned.swap(true, Ordering::AcqRel) {
                    tracing::warn!(path = %path.display(), error = %error, "cannot open fluxdownd stderr log; discarding daemon stderr");
                }
                Stdio::null()
            }
        }
    }
}

/// daemon stderr 日志超过该大小时在下次拉起前截断。
const STDERR_LOG_MAX_BYTES: u64 = 1024 * 1024;

fn daemon_executable() -> Result<PathBuf, std::io::Error> {
    if let Some(path) = std::env::var_os("FLUXDOWN_DAEMON_BIN") {
        return Ok(PathBuf::from(path));
    }
    let current = std::env::current_exe()?;
    let name = if cfg!(windows) {
        "fluxdownd.exe"
    } else {
        "fluxdownd"
    };
    Ok(current.with_file_name(name))
}

/// 后台服务与拉起者解耦：Windows 不弹控制台窗；Unix 进入独立进程组，终端里对拉起者的
/// Ctrl-C（SIGINT 发给前台进程组）不会连带终止常驻 daemon。
#[cfg(windows)]
fn detach_background_process(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(unix)]
fn detach_background_process(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(any(windows, unix)))]
fn detach_background_process(_command: &mut std::process::Command) {}

/// 新 agent 遇到旧版 daemon 占着端口（既不能与它握手，也绝不能向它发送 token）时的善后：
/// 结束当前用户名下的旧 `fluxdownd`，随后由 [`DaemonSupervisor::ensure_running`] 拉起同版本 daemon。
impl DaemonSupervisor {
    /// 结束占着 `port` 的旧版 `fluxdownd`；返回被发出终止信号的进程数。
    /// 已停止监管或认不出目标进程时为 0。
    pub async fn terminate_stale_daemon(&self, port: u16) -> usize {
        if self.stopped.load(Ordering::Acquire) {
            return 0;
        }
        terminate_stale_listener(port).await
    }
}

const DAEMON_PROCESS_NAME: &str = "fluxdownd";
/// 单次调用系统进程工具（lsof / ps / kill / pkill / taskkill）的时限。
const PROCESS_TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 进程名（Unix `ps -o comm=` 可能带目录，Windows 带 `.exe`）是否为 daemon。
#[cfg(any(unix, test))]
fn is_daemon_process(comm: &str) -> bool {
    let name = comm.trim().rsplit(['/', '\\']).next().unwrap_or_default();
    name.strip_suffix(".exe").unwrap_or(name) == DAEMON_PROCESS_NAME
}

/// 解析 `lsof -t` 的输出（每行一个 PID）。
#[cfg(any(unix, test))]
fn parse_listener_pids(output: &str) -> Vec<u32> {
    let mut pids: Vec<u32> = output
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// `taskkill` 参数：只结束当前用户（`DOMAIN\user`）名下的 `fluxdownd.exe`；用户名未知时
/// 不给参数，宁可不结束也不误伤别的会话。
#[cfg(any(windows, test))]
fn taskkill_args(domain: Option<&str>, user: Option<&str>) -> Option<Vec<String>> {
    let user = user.filter(|user| !user.is_empty())?;
    let account = match domain.filter(|domain| !domain.is_empty()) {
        Some(domain) => format!("{domain}\\{user}"),
        None => user.to_owned(),
    };
    Some(vec![
        "/F".to_owned(),
        "/IM".to_owned(),
        format!("{DAEMON_PROCESS_NAME}.exe"),
        "/FI".to_owned(),
        format!("USERNAME eq {account}"),
    ])
}

fn tool_command(program: &str, args: &[&str]) -> tokio::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    no_console_window(&mut command);
    tokio::process::Command::from(command)
}

#[cfg(unix)]
async fn tool_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(PROCESS_TOOL_TIMEOUT, tool_command(program, args).output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn tool_succeeds(program: &str, args: &[&str]) -> bool {
    let mut command = tool_command(program, args);
    command.stdout(Stdio::null());
    matches!(
        tokio::time::timeout(PROCESS_TOOL_TIMEOUT, command.status()).await,
        Ok(Ok(status)) if status.success()
    )
}

/// 按监听端口定位 PID 并确认进程名是 `fluxdownd` 后发 SIGTERM（旧 daemon 会优雅收尾）；
/// 没有 lsof 或认不出监听者时，退回按名字结束当前用户有权结束的同名进程。
#[cfg(unix)]
async fn terminate_stale_listener(port: u16) -> usize {
    let port_filter = format!("-iTCP:{port}");
    let mut listeners = Vec::new();
    if let Some(listing) = tool_stdout("lsof", &["-nP", &port_filter, "-sTCP:LISTEN", "-t"]).await {
        listeners = parse_listener_pids(&listing);
    }
    if listeners.is_empty() {
        return usize::from(tool_succeeds("pkill", &["-TERM", "-x", DAEMON_PROCESS_NAME]).await);
    }
    let mut signalled = 0;
    for pid in listeners {
        let pid = pid.to_string();
        let named_daemon = tool_stdout("ps", &["-p", &pid, "-o", "comm="])
            .await
            .is_some_and(|name| is_daemon_process(&name));
        // 监听者不是 fluxdownd：不是该善后的对象，不碰。
        if named_daemon && tool_succeeds("kill", &["-TERM", &pid]).await {
            signalled += 1;
        }
    }
    signalled
}

#[cfg(windows)]
async fn terminate_stale_listener(_port: u16) -> usize {
    let domain = std::env::var("USERDOMAIN").ok();
    let user = std::env::var("USERNAME").ok();
    let Some(args) = taskkill_args(domain.as_deref(), user.as_deref()) else {
        return 0;
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    usize::from(tool_succeeds("taskkill", &args).await)
}

#[cfg(not(any(unix, windows)))]
async fn terminate_stale_listener(_port: u16) -> usize {
    0
}

#[cfg(windows)]
fn no_console_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn no_console_window(_command: &mut std::process::Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn fast_failures_accumulate_and_healthy_run_resets() {
        let mut streak = 0;
        for _ in 0..CRASH_LOOP_THRESHOLD {
            streak = next_crash_streak(streak, false, Duration::from_secs(1));
        }
        assert_eq!(streak, CRASH_LOOP_THRESHOLD);
        assert_eq!(next_crash_streak(streak, false, Duration::from_secs(60)), 0);
        assert_eq!(next_crash_streak(streak, true, Duration::from_secs(1)), 0);
    }

    #[test]
    fn daemon_process_names_are_recognised_across_platform_spellings() {
        for name in [
            "fluxdownd",
            " fluxdownd\n",
            "/Applications/FluxDown.app/Contents/MacOS/fluxdownd",
            "/opt/Flux Down/fluxdownd",
            "fluxdownd.exe",
            "C:\\Program Files\\FluxDown\\fluxdownd.exe",
        ] {
            assert!(is_daemon_process(name), "{name:?}");
        }
        for name in [
            "",
            "fluxdown-agent",
            "fluxdownd-helper",
            "sleep",
            "/usr/bin/fluxdown",
        ] {
            assert!(!is_daemon_process(name), "{name:?}");
        }
    }

    #[test]
    fn listener_pids_are_parsed_and_deduplicated() {
        assert_eq!(parse_listener_pids("123\n456\n123\n"), vec![123, 456]);
        assert_eq!(parse_listener_pids("\nnot-a-pid\n 789 \n"), vec![789]);
        assert!(parse_listener_pids("").is_empty());
    }

    #[test]
    fn taskkill_is_scoped_to_the_current_user_or_refused() {
        assert_eq!(
            taskkill_args(Some("WORKGROUP"), Some("zero")),
            Some(vec![
                "/F".to_owned(),
                "/IM".to_owned(),
                "fluxdownd.exe".to_owned(),
                "/FI".to_owned(),
                "USERNAME eq WORKGROUP\\zero".to_owned(),
            ])
        );
        assert_eq!(
            taskkill_args(None, Some("zero")).map(|args| args[4].clone()),
            Some("USERNAME eq zero".to_owned())
        );
        // 用户名未知：宁可不结束也不能不带用户过滤。
        assert_eq!(taskkill_args(Some("WORKGROUP"), None), None);
        assert_eq!(taskkill_args(Some("WORKGROUP"), Some("")), None);
    }

    #[test]
    fn allocator_env_respects_values_already_set_by_the_parent() {
        let injected: Vec<_> = allocator_env(|_| None).collect();
        assert_eq!(injected, vec![("MIMALLOC_PURGE_DELAY", "0")]);
        let overridden = allocator_env(|key| {
            (key == "MIMALLOC_PURGE_DELAY").then(|| std::ffi::OsString::from("250"))
        });
        assert_eq!(overridden.count(), 0);
    }
}
