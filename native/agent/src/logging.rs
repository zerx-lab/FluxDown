//! 桌面（非 `--server`）与移动端嵌入模式的诊断日志：把 agent 的 `tracing` 事件与 panic 落盘到
//! `<agent_data_dir>/logs/agent.log`（有界轮转、去重限流，见 `fluxdown_logfile`）。
//!
//! GUI 子系统 / 移动 App 没有可读的 stderr，不装订阅者时所有事件都会丢失。

use std::fmt::Write as _;
use std::io;
use std::path::Path;

use fluxdown_logfile::Level;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::runtime::AgentResult;

/// 一等公民 crate：默认按用户级别输出；第三方（hyper / reqwest / tungstenite / axum）只保留 warn+。
/// 嵌入模式下 daemon 与 agent 同进程，daemon 事件也写进这份日志；移动端 UniFFI 会话驱动
/// （`fluxdown_mobile`，只在重连时记一行原因）同样写进来，否则界面卡在加载 / 断连时无从定位。
const FIRST_PARTY_TARGETS: [&str; 6] = [
    "fluxdown_agent",
    "fluxdown_daemon",
    "fluxdown_link",
    "fluxdown_api",
    "fluxdown_protocol",
    "fluxdown_mobile",
];

/// 初始化桌面模式日志；初始化失败时向现有订阅者与 stderr 报告，不替换已有订阅者。
pub fn init_desktop() {
    let agent_data_dir = match crate::runtime::resolve_agent_data_dir() {
        Ok(dir) => dir,
        Err(error) => {
            tracing::error!(error = %error, "could not resolve agent log directory");
            eprintln!("could not resolve agent log directory: {error}");
            return;
        }
    };
    init_at(&agent_data_dir, &crate::runtime::engine_data_dir());
}

/// 初始化移动端嵌入模式日志（进程内 daemon + agent）。必须在启动 daemon 之前调用：daemon
/// 只在没有全局订阅者时才装自己的 stderr 订阅者。进程内只生效一次，重复调用沿用首次的目录。
pub fn init_embedded(agent_data_dir: &Path, engine_data_dir: &Path) {
    init_at(agent_data_dir, engine_data_dir);
}

fn init_at(agent_data_dir: &Path, engine_data_dir: &Path) {
    let dir = crate::log_export::agent_log_dir(agent_data_dir);
    let header = build_header(agent_data_dir, engine_data_dir);
    let log = match fluxdown_logfile::init_global(&dir, "agent", header) {
        Ok(log) => log,
        Err(error) => {
            tracing::error!(path = %dir.display(), error = %error, "could not initialize agent log file");
            eprintln!(
                "could not initialize agent log file at {}: {error}",
                dir.display()
            );
            return;
        }
    };
    fluxdown_logfile::install_panic_hook();
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_level(false)
        .with_target(true)
        .with_writer(LogFileWriter)
        .with_filter(build_filter());
    let subscriber = tracing_subscriber::registry().with(fmt_layer);
    if let Err(error) = tracing::subscriber::set_global_default(subscriber) {
        log.log_unthrottled(
            Level::Warn,
            &format!(
                "agent tracing subscriber was not installed; keeping existing subscriber: {error}"
            ),
        );
        tracing::warn!(error = %error, "keeping existing tracing subscriber");
    }
}

/// 进程结束前调用：失败时把完整错误链写入日志，并输出被限流吞掉的重复条目摘要。
/// 日志未初始化时不做任何事。
pub fn finish(result: &AgentResult) {
    let Some(log) = fluxdown_logfile::global() else {
        return;
    };
    if let Err(error) = result {
        log.log_unthrottled(
            Level::Error,
            &format!("agent exited with error: {}", error_chain(error.as_ref())),
        );
    }
    log.flush_suppressed();
}

pub(crate) fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = String::new();
    let mut source = Some(error);
    while let Some(cause) = source {
        let end = text.len();
        if end > 0 {
            text.push_str(": ");
        }
        if write!(text, "{cause}").is_err() {
            text.truncate(end);
            if end > 0 {
                text.push_str(": ");
            }
            text.push_str("<error display failed>");
            break;
        }
        source = cause.source();
    }
    text
}

fn build_filter() -> EnvFilter {
    if let Ok(raw) = std::env::var("RUST_LOG")
        && !raw.trim().is_empty()
    {
        return EnvFilter::new(raw);
    }
    let level = std::env::var("FLUXDOWN_LOG_LEVEL")
        .ok()
        .and_then(|value| Level::parse(&value))
        .unwrap_or(Level::Info);
    EnvFilter::new(default_directives(level))
}

fn default_directives(level: Level) -> String {
    let name = match level {
        Level::Error => return "error".to_owned(),
        Level::Warn => "warn",
        Level::Info => "info",
        Level::Debug => "debug",
        Level::Trace => "trace",
    };
    let mut directives = String::from("warn");
    for target in FIRST_PARTY_TARGETS {
        directives.push(',');
        directives.push_str(target);
        directives.push('=');
        directives.push_str(name);
    }
    directives
}

fn build_header(agent_data_dir: &Path, engine_data_dir: &Path) -> String {
    let mut header = String::new();
    let mut line = |key: &str, value: String| {
        header.push_str("  ");
        header.push_str(key);
        header.push_str(": ");
        header.push_str(&value);
        header.push('\n');
    };
    line("version", fluxdown_protocol::APP_VERSION.to_owned());
    let exe = std::env::current_exe();
    line(
        "exe",
        exe.as_ref().map_or_else(
            |error| format!("<unknown: {error}>"),
            |p| p.display().to_string(),
        ),
    );
    line(
        "args",
        std::env::args().skip(1).collect::<Vec<_>>().join(" "),
    );
    line(
        "os/arch",
        format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
    );
    line("agent data dir", agent_data_dir.display().to_string());
    line("engine data dir", engine_data_dir.display().to_string());
    #[cfg(windows)]
    {
        let portable = exe
            .as_ref()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("portable").exists()))
            .unwrap_or(false);
        line("portable marker", portable.to_string());
        line(
            "SESSIONNAME",
            std::env::var("SESSIONNAME").unwrap_or_else(|_| "<unset>".to_owned()),
        );
    }
    for key in ["RUST_LOG", "FLUXDOWN_LOG_LEVEL"] {
        line(
            &format!("env {key}"),
            std::env::var(key).unwrap_or_else(|_| "<unset>".to_owned()),
        );
    }
    // 其余 FLUXDOWN_* 只记名字：值可能是访问密钥等机密。
    let mut names: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| name.starts_with("FLUXDOWN_") && name != "FLUXDOWN_LOG_LEVEL")
        .collect();
    names.sort();
    for name in names {
        line(&format!("env {name}"), "<set>".to_owned());
    }
    header
}

/// `tracing` → `LogFile` 适配：每个事件一个 writer，Drop 时整条转发。
#[derive(Clone, Copy)]
struct LogFileWriter;

struct EventWriter {
    level: Level,
    buf: Vec<u8>,
}

impl<'a> MakeWriter<'a> for LogFileWriter {
    type Writer = EventWriter;

    fn make_writer(&'a self) -> Self::Writer {
        EventWriter {
            level: Level::Info,
            buf: Vec::new(),
        }
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
        let level = match *meta.level() {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            tracing::Level::DEBUG => Level::Debug,
            tracing::Level::TRACE => Level::Trace,
        };
        EventWriter {
            level,
            buf: Vec::new(),
        }
    }
}

impl io::Write for EventWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.buf);
        let text = text.trim_end();
        if text.is_empty() {
            return;
        }
        if let Some(log) = fluxdown_logfile::global() {
            log.log(self.level, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_filter_keeps_third_party_quiet() {
        assert_eq!(
            default_directives(Level::Info),
            "warn,fluxdown_agent=info,fluxdown_daemon=info,fluxdown_link=info,fluxdown_api=info,fluxdown_protocol=info,fluxdown_mobile=info"
        );
        assert_eq!(default_directives(Level::Error), "error");
    }

    #[test]
    fn error_chain_walks_sources() {
        #[derive(Debug)]
        struct Outer(io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("outer")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let error = Outer(io::Error::other("inner"));
        assert_eq!(error_chain(&error), "outer: inner");
    }
}
