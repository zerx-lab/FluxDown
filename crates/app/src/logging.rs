//! 界面进程诊断日志：`<agent 数据目录>/logs/desktop.log`（与 `agent.log` 同目录，Doctor
//! 导出一并打包）。落盘、轮转与重复限流见 `fluxdown_logfile`。
//!
//! 收集面按「出问题时能定位」取舍，不记常规操作：
//! - `log` 门面：GPUI 平台层（显卡选择、D3D 特性级别、DirectComposition、设备丢失与重建、
//!   draw/resize 失败）、gpui-component（经 `tracing` 的 `log` 兼容层）与本 crate 自身；
//!   第一方与 `gpui*` 按 `FLUXDOWN_LOG_LEVEL`（缺省 info），其余第三方只收 warn 以上。
//! - 会话头：版本、可执行文件、启动参数形态（不记链接内容）、平台、远程会话 / 显示协议、
//!   影响渲染的环境变量。
//! - 窗口：打开参数、GPU 规格（是否软件渲染）、首帧耗时；超时未出首帧告警。
//! - UI 线程看门狗：主线程心跳停摆超过阈值时告警并在恢复时记录停摆时长。
//! - panic（含回溯）与启动失败原因。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fluxdown_logfile::{Level, LogFile};
use gpui::{App, Window};

/// 按配置级别放行的 target 前缀：本仓库 crate 与 GPUI 全家（平台层诊断都在这里）。
const VERBOSE_TARGET_PREFIXES: &[&str] = &["fluxdown", "gpui"];
/// 新窗口在这段时间内仍未完成首帧即告警（透明 / 空白窗口的直接证据）。
const FIRST_FRAME_DEADLINE: Duration = Duration::from_secs(10);

/// 安装日志：会话头、panic hook、`log` 门面。agent 数据目录写不进去时（Windows 上目录 ACL 只授权给
/// 旧账户 SID 等）改写 `local_service::local_fallback_dir` 下的 `logs/`，启动失败仍有迹可查；都
/// 失败时静默（GUI 子系统没有 stderr 可报）。
pub(crate) fn init() {
    let agent_dir = crate::app::agent_data_dir();
    let primary_dir = agent_dir.join("logs");
    let (log, primary_error) =
        match fluxdown_logfile::init_global(&primary_dir, "desktop", session_header()) {
            Ok(log) => (log, None),
            Err(error) => {
                let Some(fallback) =
                    fluxdown_protocol::local_service::local_fallback_dir(&agent_dir)
                else {
                    return;
                };
                let Ok(log) = fluxdown_logfile::init_global(
                    &fallback.join("logs"),
                    "desktop",
                    session_header(),
                ) else {
                    return;
                };
                (log, Some(error))
            }
        };
    fluxdown_logfile::install_panic_hook();
    let verbose = std::env::var("FLUXDOWN_LOG_LEVEL")
        .ok()
        .and_then(|value| Level::parse(&value))
        .unwrap_or(Level::Info);
    let filter = TargetFilter {
        verbose: level_filter(verbose),
        others: level_filter(verbose.min(Level::Warn)),
    };
    let max = filter.verbose.max(filter.others);
    if log::set_boxed_logger(Box::new(DesktopLogger { log, filter })).is_ok() {
        log::set_max_level(max);
    }
    if let Some(error) = primary_error {
        log::warn!(
            "desktop log directory {} is not writable ({error}); logging to the fallback directory instead",
            primary_dir.display()
        );
    }
}

/// 进程退出前：记录失败原因（完整错误链）并补记限流摘要。
pub(crate) fn finish(error: Option<&dyn std::error::Error>) {
    let Some(log) = fluxdown_logfile::global() else {
        return;
    };
    if let Some(error) = error {
        log.log_unthrottled(
            Level::Error,
            &format!("desktop client exited with error: {}", error_chain(error)),
        );
    }
    log.flush_suppressed();
}

/// `a: b: c` 形式的完整错误链（等价于 anyhow 的 `{:#}`）。
pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

struct DesktopLogger {
    log: &'static LogFile,
    filter: TargetFilter,
}

/// 第一方与 `gpui*` 用 `verbose`，其余第三方用 `others`。
#[derive(Clone, Copy)]
struct TargetFilter {
    verbose: log::LevelFilter,
    others: log::LevelFilter,
}

impl TargetFilter {
    fn allows(&self, level: log::Level, target: &str) -> bool {
        let filter = if VERBOSE_TARGET_PREFIXES
            .iter()
            .any(|prefix| target.starts_with(prefix))
        {
            self.verbose
        } else {
            self.others
        };
        level <= filter
    }
}

impl log::Log for DesktopLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.filter.allows(metadata.level(), metadata.target())
    }

    fn log(&self, record: &log::Record<'_>) {
        // `gpui_util::log_err` 绕过 `enabled` 直接提交且 target 可能为空：这里统一再判一次。
        if !self.filter.allows(record.level(), record.target()) {
            return;
        }
        self.log
            .log(to_level(record.level()), &format_record(record));
    }

    fn flush(&self) {}
}

/// `target: message`；警告 / 错误附源码位置（`log_err` 的 target 为空，位置是唯一线索）。
fn format_record(record: &log::Record<'_>) -> String {
    let location = record.file().map(|file| {
        let line = record.line().unwrap_or_default();
        format!("{}:{line}", short_path(file))
    });
    let target = match (record.target(), &location) {
        ("", Some(location)) => location.as_str(),
        ("", None) => "?",
        (target, _) => target,
    };
    match &location {
        Some(location) if record.level() <= log::Level::Warn && !record.target().is_empty() => {
            format!("{target}: {} ({location})", record.args())
        }
        _ => format!("{target}: {}", record.args()),
    }
}

/// 源码路径只保留最后三段（`<crate-dir>/src/<file>.rs`），去掉构建机绝对路径。
fn short_path(file: &str) -> String {
    let normalized = file.replace('\\', "/");
    let parts: Vec<&str> = normalized.rsplit('/').take(3).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

const fn level_filter(level: Level) -> log::LevelFilter {
    match level {
        Level::Error => log::LevelFilter::Error,
        Level::Warn => log::LevelFilter::Warn,
        Level::Info => log::LevelFilter::Info,
        Level::Debug => log::LevelFilter::Debug,
        Level::Trace => log::LevelFilter::Trace,
    }
}

const fn to_level(level: log::Level) -> Level {
    match level {
        log::Level::Error => Level::Error,
        log::Level::Warn => Level::Warn,
        log::Level::Info => Level::Info,
        log::Level::Debug => Level::Debug,
        log::Level::Trace => Level::Trace,
    }
}

fn session_header() -> String {
    let mut lines = vec![
        format!("  version: {}", fluxdown_protocol::APP_VERSION),
        format!(
            "  exe: {}",
            std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|error| format!("<unavailable: {error}>"))
        ),
        format!("  args: {}", describe_args(std::env::args().skip(1))),
        format!(
            "  platform: {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        format!("  data dir: {}", crate::app::agent_data_dir().display()),
    ];
    #[cfg(windows)]
    {
        let portable = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("portable").exists()))
            .unwrap_or(false);
        lines.push(format!("  portable marker: {portable}"));
    }
    // 远程桌面 / 显示协议：Windows `SESSIONNAME`（Console 或 RDP-Tcp#N），Linux 会话类型。
    for name in [
        "SESSIONNAME",
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "WAYLAND_DISPLAY",
        "DISPLAY",
    ] {
        if let Ok(value) = std::env::var(name) {
            lines.push(format!("  {name}: {value}"));
        }
    }
    lines.extend(env_overrides(std::env::vars()));
    lines.join("\n")
}

/// 启动参数只记开关与位置参数个数：位置参数是用户交来的链接 / 种子路径，不进日志。
fn describe_args(args: impl Iterator<Item = String>) -> String {
    let mut flags = Vec::new();
    let mut positional = 0_usize;
    for arg in args {
        if arg.starts_with("--") {
            flags.push(arg.split('=').next().unwrap_or_default().to_owned());
        } else {
            positional += 1;
        }
    }
    let mut text = if flags.is_empty() {
        "(no flags)".to_owned()
    } else {
        flags.join(" ")
    };
    if positional > 0 {
        text.push_str(&format!(" (+{positional} positional)"));
    }
    text
}

/// 影响渲染 / 日志的环境变量原样记录；其余 `FLUXDOWN_*` 只记名字（可能是访问密钥等机密）。
fn env_overrides(vars: impl Iterator<Item = (String, String)>) -> Vec<String> {
    let mut lines: Vec<String> = vars
        .filter_map(|(name, value)| {
            if name.starts_with("GPUI_")
                || name.starts_with("ZED_")
                || name == "RUST_LOG"
                || name == "FLUXDOWN_LOG_LEVEL"
            {
                Some(format!("  env {name}={value}"))
            } else if name.starts_with("FLUXDOWN_") {
                Some(format!("  env {name}=<set>"))
            } else {
                None
            }
        })
        .collect();
    lines.sort();
    lines
}

/// 新窗口诊断：打开参数、首个窗口的 GPU 规格、首帧耗时与首帧超时告警。
///
/// 在 `open_window` 的构建闭包里调用（平台窗口与渲染器已创建，根视图尚未构建）。
pub(crate) fn observe_new_window(
    label: String,
    opened_at: Instant,
    window: &mut Window,
    cx: &mut App,
) {
    static GPU_LOGGED: AtomicBool = AtomicBool::new(false);
    let bounds = window.bounds();
    log::info!(
        "window {label} opened: {}x{} at ({}, {}), scale {}",
        bounds.size.width.as_f32(),
        bounds.size.height.as_f32(),
        bounds.origin.x.as_f32(),
        bounds.origin.y.as_f32(),
        window.scale_factor(),
    );
    if !GPU_LOGGED.swap(true, Ordering::Relaxed) {
        match window.gpu_specs() {
            Some(gpu) if gpu.is_software_emulated => log::warn!(
                "GPU is software emulated (expect high CPU usage): {} | {} | {}",
                gpu.device_name,
                gpu.driver_name,
                gpu.driver_info
            ),
            Some(gpu) => log::info!(
                "GPU: {} | {} | {}",
                gpu.device_name,
                gpu.driver_name,
                gpu.driver_info
            ),
            None => log::warn!("GPU specs unavailable for window {label}"),
        }
    }
    let rendered = Arc::new(AtomicBool::new(false));
    let first_frame = Arc::clone(&rendered);
    let frame_label = label.clone();
    window.on_next_frame(move |_, _| {
        first_frame.store(true, Ordering::Release);
        log::info!(
            "window {frame_label} first frame after {}ms",
            opened_at.elapsed().as_millis()
        );
    });
    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_FRAME_DEADLINE).await;
        if !rendered.load(Ordering::Acquire) {
            log::warn!(
                "window {label} has not completed a first frame {}s after opening",
                FIRST_FRAME_DEADLINE.as_secs()
            );
        }
    })
    .detach();
}

/// UI 线程停摆多久开始告警；之后在这些时长再各记一次，恢复时记总时长。
const STALL_REPORT_SECS: [u64; 3] = [10, 60, 300];
/// 心跳 / 看门狗周期。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
/// 看门狗自身睡眠超出预期这么多：视为系统休眠 / 进程被挂起，重置基线不误报。
const SUSPEND_GAP: Duration = Duration::from_secs(5);

/// UI 线程看门狗：主线程上的前台任务每秒写心跳，独立线程检查心跳是否停摆。
///
/// 前台任务只有主线程空闲时才会被调度，所以停摆 = 主线程被阻塞或忙于长任务（界面「未响应」、
/// 首帧前卡死）。macOS 不启用：App Nap 会把后台应用的定时器合并延后数十秒，心跳式检测在
/// 那里只会产生误报。
pub(crate) fn install_ui_watchdog(cx: &mut App) {
    if cfg!(target_os = "macos") || fluxdown_logfile::global().is_none() {
        return;
    }
    let epoch = Instant::now();
    let beat = Arc::new(AtomicU64::new(0));
    let foreground_beat = Arc::clone(&beat);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(HEARTBEAT_INTERVAL).await;
            foreground_beat.store(elapsed_ms(epoch), Ordering::Release);
        }
    })
    .detach();
    let spawned = std::thread::Builder::new()
        .name("fluxdown-ui-watchdog".to_owned())
        .spawn(move || watchdog_loop(epoch, &beat));
    if let Err(error) = spawned {
        log::warn!("UI watchdog thread could not start: {error}");
    }
}

fn watchdog_loop(epoch: Instant, beat: &AtomicU64) {
    let mut reported = 0_usize;
    loop {
        let before = Instant::now();
        std::thread::sleep(HEARTBEAT_INTERVAL);
        let overslept = before.elapsed().saturating_sub(HEARTBEAT_INTERVAL);
        if overslept >= SUSPEND_GAP {
            log::info!(
                "system suspend or process freeze detected (watchdog slept {}s longer than expected)",
                overslept.as_secs()
            );
            beat.store(elapsed_ms(epoch), Ordering::Release);
            reported = 0;
            continue;
        }
        let stalled =
            Duration::from_millis(elapsed_ms(epoch).saturating_sub(beat.load(Ordering::Acquire)));
        if stalled < HEARTBEAT_INTERVAL * 3 {
            if reported > 0 {
                log::info!("UI thread responsive again");
                reported = 0;
            }
            continue;
        }
        if let Some(threshold) = STALL_REPORT_SECS.get(reported)
            && stalled.as_secs() >= *threshold
        {
            log::warn!(
                "UI thread unresponsive: no heartbeat for {}s (main thread blocked or busy)",
                stalled.as_secs()
            );
            reported += 1;
        }
    }
}

fn elapsed_ms(epoch: Instant) -> u64 {
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_are_reduced_to_flags_and_positional_count() {
        let args = [
            "--capture",
            "https://example.com/a?token=x",
            "--progress=abc",
            "C:\\a.torrent",
        ]
        .into_iter()
        .map(str::to_owned);
        assert_eq!(describe_args(args), "--capture --progress (+2 positional)");
        assert_eq!(describe_args(std::iter::empty()), "(no flags)");
    }

    #[test]
    fn fluxdown_env_values_are_redacted_but_render_overrides_kept() {
        let vars = [
            ("FLUXDOWN_ACCESS_KEY", "secret"),
            ("GPUI_DISABLE_DIRECT_COMPOSITION", "1"),
            ("FLUXDOWN_LOG_LEVEL", "debug"),
            ("PATH", "/bin"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()));
        assert_eq!(
            env_overrides(vars),
            vec![
                "  env FLUXDOWN_ACCESS_KEY=<set>",
                "  env FLUXDOWN_LOG_LEVEL=debug",
                "  env GPUI_DISABLE_DIRECT_COMPOSITION=1",
            ]
        );
    }

    #[test]
    fn third_party_targets_only_pass_warnings_by_default() {
        let filter = TargetFilter {
            verbose: log::LevelFilter::Info,
            others: log::LevelFilter::Warn,
        };
        assert!(filter.allows(log::Level::Info, "gpui_windows::directx_devices"));
        assert!(filter.allows(log::Level::Info, "fluxdown_ui_app::agent_client"));
        assert!(!filter.allows(log::Level::Info, "naga::front"));
        assert!(filter.allows(log::Level::Error, ""));
        assert!(!filter.allows(log::Level::Debug, "gpui"));
    }

    #[test]
    fn error_records_without_target_fall_back_to_their_source_location() {
        let args = format_args!("draw failed");
        let record = log::Record::builder()
            .level(log::Level::Error)
            .target("")
            .file(Some(
                "C:\\Users\\ci\\.cargo\\registry\\src\\index\\gpui-pre-windows-0.3.7\\src\\window.rs",
            ))
            .line(Some(1034))
            .args(args)
            .build();
        assert_eq!(
            format_record(&record),
            "gpui-pre-windows-0.3.7/src/window.rs:1034: draw failed"
        );
    }
}
