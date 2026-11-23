//! 桌面进程（`fluxdown-desktop` / `fluxdown-agent`）共用的诊断日志文件。
//!
//! - 单文件 + 单个轮转副本：`<stem>.log` 写满 [`MAX_FILE_BYTES`] 后改名为 `<stem>.log.1`
//!   （覆盖旧副本），每个进程的日志总量恒不超过两倍上限；
//! - 每个文件开头都有会话头（进程、版本、环境），轮转后在新文件重写一遍，单个文件可独立阅读；
//! - 重复消息限流（见 [`throttle`]）：循环故障只保留前几条 + 周期摘要，不冲掉启动上下文；
//! - 逐行直接 `write` 到操作系统（无用户态缓冲），进程崩溃或被强杀时已写的行不丢；
//! - [`install_panic_hook`] 把 panic 与回溯写进同一文件，再交给原有 hook。
//!
//! 零第三方依赖：`log` / `tracing` 适配器由各宿主自己实现，本 crate 只负责落盘策略。

mod sanitize;
pub mod startup;
mod throttle;
mod time;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

pub use sanitize::{ANDROID_DISK_URL_PATTERN, SANITIZE_PATTERNS};
pub use time::utc_timestamp;

use throttle::{Decision, Summary, Throttle};

/// 单个日志文件上限；超出即轮转。
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// 单行上限：超长的调试转储截断，避免一条消息挤掉大段历史。
const MAX_LINE_BYTES: usize = 8 * 1024;
/// panic hook 等待日志锁的上限（锁被其它线程短暂持有时）。
const PANIC_LOCK_WAIT: Duration = Duration::from_millis(200);

static GLOBAL: OnceLock<LogFile> = OnceLock::new();
static PANIC_HOOK: OnceLock<()> = OnceLock::new();

/// 日志级别；与 `log` / `tracing` 的五级一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    /// 等宽级别名，日志列对齐。
    #[must_use]
    pub const fn as_padded_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// 解析 `error`/`warn`/`info`/`debug`/`trace`（大小写、首尾空白不敏感）。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

/// 一个进程的诊断日志文件。
pub struct LogFile {
    path: PathBuf,
    rotated_path: PathBuf,
    max_file_bytes: u64,
    redact: Option<fn(&str) -> String>,
    fallback_dir: Option<PathBuf>,
    fallback: OnceLock<Box<LogFile>>,
    state: Mutex<State>,
}

struct State {
    file: Option<File>,
    size: u64,
    header: String,
    throttle: Throttle,
}

impl LogFile {
    /// 在 `dir` 下打开 `<stem>.log`（必要时先轮转）并写入会话头。
    ///
    /// `header` 是宿主提供的多行环境描述（版本、路径、关键环境变量……），每次轮转后重写。
    pub fn open(dir: &Path, stem: &str, header: String) -> io::Result<Self> {
        Self::open_with_limit(dir, stem, header, MAX_FILE_BYTES, None, None)
    }

    fn open_with_limit(
        dir: &Path,
        stem: &str,
        header: String,
        max_file_bytes: u64,
        redact: Option<fn(&str) -> String>,
        fallback_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{stem}.log"));
        let rotated_path = dir.join(format!("{stem}.log.1"));
        let existing = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if existing >= max_file_bytes {
            rotate_files(&path, &rotated_path)?;
        }
        let file = open_append(&path)?;
        let size = file.metadata()?.len();
        let log = Self {
            path,
            rotated_path,
            max_file_bytes,
            redact,
            fallback_dir,
            fallback: OnceLock::new(),
            state: Mutex::new(State {
                file: Some(file),
                size,
                header: match redact {
                    Some(redact) => redact(&header),
                    None => header,
                },
                throttle: Throttle::new(),
            }),
        };
        let mut state = log.lock();
        log.write_banner(&mut state, "session started")?;
        drop(state);
        Ok(log)
    }

    /// 当前日志文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        self.fallback
            .get()
            .map_or(self.path.as_path(), |log| log.path())
    }

    /// 写一条日志（受重复限流约束）。`text` 可含换行，续行缩进对齐。
    pub fn log(&self, level: Level, text: &str) {
        self.log_at(level, text, Instant::now());
    }

    fn log_at(&self, level: Level, text: &str, now: Instant) {
        let mut state = self.lock();
        let mut summaries = Vec::new();
        let decision = state.throttle.submit(level, text, now, &mut summaries);
        for summary in &summaries {
            self.write_summary(&mut state, summary);
        }
        match decision {
            Decision::Write {
                last_before_suppression: false,
            } => self.write_line(&mut state, level, text, None),
            Decision::Write {
                last_before_suppression: true,
            } => self.write_line(
                &mut state,
                level,
                text,
                Some("(repeated; identical messages are summarized for the next 60s)"),
            ),
            Decision::Suppress => {}
        }
    }

    /// 写一条不受限流约束的日志（panic、退出原因等一次性关键事件）。
    pub fn log_unthrottled(&self, level: Level, text: &str) {
        let mut state = self.lock();
        self.write_line(&mut state, level, text, None);
    }

    /// 补记所有尚未写出的限流摘要；进程正常退出前调用。
    pub fn flush_suppressed(&self) {
        let mut state = self.lock();
        let mut summaries = Vec::new();
        state.throttle.drain(&mut summaries);
        for summary in &summaries {
            self.write_summary(&mut state, summary);
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_banner(&self, state: &mut State, reason: &str) -> io::Result<()> {
        let banner = format!(
            "====== {} pid={} {reason} ======\n{}",
            utc_timestamp(SystemTime::now()),
            std::process::id(),
            state.header,
        );
        let mut banner = banner.trim_end().to_owned();
        banner.push('\n');
        self.append(state, banner.as_bytes())
    }

    fn write_summary(&self, state: &mut State, summary: &Summary) {
        let text = format!(
            "[suppressed {} repeats over {}s] {}",
            summary.suppressed,
            summary.span.as_secs(),
            summary.sample
        );
        self.write_line(state, summary.level, &text, None);
    }

    fn write_line(&self, state: &mut State, level: Level, text: &str, suffix: Option<&str>) {
        let sanitized;
        let text = if let Some(redact) = self.redact {
            sanitized = redact(text);
            sanitized.as_str()
        } else {
            text
        };
        let line = format_line(level, text, suffix, SystemTime::now());
        if let Err(error) = self.append_or_fallback(state, line.as_bytes()) {
            // 不调用宿主 logger：它会再次写入本文件。
            eprintln!(
                "fluxdown_logfile: writing {} failed: {error}",
                self.path.display()
            );
        }
    }

    fn append_or_fallback(&self, state: &mut State, bytes: &[u8]) -> io::Result<()> {
        if let Some(log) = self.fallback.get() {
            return log.append(&mut log.lock(), bytes);
        }
        let Some(dir) = self.fallback_dir.as_deref() else {
            return self.append(state, bytes);
        };
        if state.file.is_some() {
            match self.append(state, bytes) {
                Ok(()) => return Ok(()),
                Err(error) => eprintln!("fluxdown_logfile: external log write failed: {error}"),
            }
            state.file = None;
        }
        let stem = self
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "log file has no valid stem")
            })?;
        let log = Self::open_with_limit(
            dir,
            stem,
            state.header.clone(),
            self.max_file_bytes,
            None,
            None,
        )?;
        let log = self.fallback.get_or_init(|| Box::new(log));
        log.append(&mut log.lock(), bytes)
    }

    fn append(&self, state: &mut State, bytes: &[u8]) -> io::Result<()> {
        let Some(file) = state.file.as_mut() else {
            return Ok(());
        };
        if let Err(error) = file.write_all(bytes) {
            state.file = None;
            return Err(error);
        }
        state.size = state.size.saturating_add(bytes.len() as u64);
        if state.size >= self.max_file_bytes {
            self.rotate(state)?;
        }
        Ok(())
    }

    fn rotate(&self, state: &mut State) -> io::Result<()> {
        state.file = None;
        state.size = 0;
        let reopened = rotate_files(&self.path, &self.rotated_path)
            .and_then(|()| open_append(&self.path))
            // 改名失败（例如被其它进程占用）时退回截断当前文件，保证总量仍有界。
            .or_else(|error| {
                eprintln!(
                    "fluxdown_logfile: rotating {} failed: {error}; truncating current file",
                    self.path.display()
                );
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&self.path)
            });
        state.file = Some(reopened?);
        self.write_banner(state, "continued after rotation")
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn rotate_files(path: &Path, rotated_path: &Path) -> io::Result<()> {
    match fs::remove_file(rotated_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(path, rotated_path)
}

/// `<UTC 时间> <级别> <文本>[ <后缀>]`，多行文本的续行缩进 4 格；超长截断。
fn format_line(level: Level, text: &str, suffix: Option<&str>, time: SystemTime) -> String {
    let text = truncate(text.trim_end(), MAX_LINE_BYTES);
    let mut line = format!("{} {} ", utc_timestamp(time), level.as_padded_str());
    let mut lines = text.split('\n');
    line.push_str(lines.next().unwrap_or_default().trim_end_matches('\r'));
    for continuation in lines {
        line.push_str("\n    ");
        line.push_str(continuation.trim_end_matches('\r'));
    }
    if let Some(suffix) = suffix {
        line.push(' ');
        line.push_str(suffix);
    }
    line.push('\n');
    line
}

fn truncate(text: &str, limit: usize) -> std::borrow::Cow<'_, str> {
    if text.len() <= limit {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    std::borrow::Cow::Owned(format!(
        "{} …[truncated {} bytes]",
        &text[..end],
        text.len() - end
    ))
}

/// 初始化进程级日志文件；重复调用返回首次打开的实例。
pub fn init_global(dir: &Path, stem: &str, header: String) -> io::Result<&'static LogFile> {
    if let Some(existing) = GLOBAL.get() {
        return Ok(existing);
    }
    let log = LogFile::open(dir, stem, header)?;
    Ok(GLOBAL.get_or_init(|| log))
}

/// Open the process log with a redactor applied before file writes, including panic records.
pub fn init_global_redacted(
    dir: &Path,
    stem: &str,
    header: String,
    redact: fn(&str) -> String,
    fallback_dir: Option<&Path>,
) -> io::Result<&'static LogFile> {
    if let Some(existing) = GLOBAL.get() {
        return Ok(existing);
    }
    let log = LogFile::open_with_limit(
        dir,
        stem,
        header,
        MAX_FILE_BYTES,
        Some(redact),
        fallback_dir.map(Path::to_path_buf),
    )?;
    Ok(GLOBAL.get_or_init(|| log))
}

/// 进程级日志文件（未初始化时为 `None`）。
#[must_use]
pub fn global() -> Option<&'static LogFile> {
    GLOBAL.get()
}

/// 安装 panic hook：把线程名、位置、消息与回溯写进进程级日志文件，再调用原 hook。
///
/// 重复调用只安装一次。日志锁在 [`PANIC_LOCK_WAIT`] 内拿不到时放弃写文件（避免 panic
/// 发生在持锁路径上时自锁），原 hook 照常执行。
pub fn install_panic_hook() {
    PANIC_HOOK.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(log) = global() {
                let payload = info
                    .payload()
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("non-string panic payload");
                let location = info
                    .location()
                    .map(|location| format!("{}:{}", location.file(), location.line()))
                    .unwrap_or_else(|| "unknown location".to_owned());
                let thread = std::thread::current();
                let text = format!(
                    "panic in thread '{}' at {location}: {payload}\n{}",
                    thread.name().unwrap_or("<unnamed>"),
                    std::backtrace::Backtrace::force_capture(),
                );
                log.write_panic(&text);
            }
            previous(info);
        }));
    });
}

impl LogFile {
    fn write_panic(&self, text: &str) {
        let deadline = Instant::now() + PANIC_LOCK_WAIT;
        loop {
            match self.state.try_lock() {
                Ok(mut state) => {
                    self.write_line(&mut state, Level::Error, text, None);
                    return;
                }
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    let mut state = poisoned.into_inner();
                    self.write_line(&mut state, Level::Error, text, None);
                    return;
                }
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(std::sync::TryLockError::WouldBlock) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_logfile_{name}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove stale test directory: {error}"),
        }
        dir
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read test log: {error}"))
    }

    #[test]
    fn redacted_sink_persists_headers_events_and_panics_without_the_secret() -> io::Result<()> {
        let dir = temp_dir("redacted");
        let result = (|| {
            let log = LogFile::open_with_limit(
                &dir,
                "agent",
                "header secret".to_owned(),
                2048,
                Some(|text| text.replace("secret", "[REDACTED]")),
                None,
            )?;
            log.log(Level::Info, "event secret");
            log.write_panic("panic secret");
            fs::read_to_string(log.path())
        })();
        fs::remove_dir_all(&dir)?;
        let text = result?;
        for line in ["header [REDACTED]", "event [REDACTED]", "panic [REDACTED]"] {
            assert!(text.contains(line), "{text}");
        }
        assert!(!text.contains("secret"), "{text}");
        Ok(())
    }

    #[test]
    fn external_write_failure_preserves_redaction_and_switches_active_path() -> io::Result<()> {
        let dir = temp_dir("fallback");
        let private = dir.join("private");
        let result = (|| {
            let log = LogFile::open_with_limit(
                &dir,
                "agent",
                String::new(),
                2048,
                Some(|text| text.replace("secret", "[REDACTED]")),
                Some(private.clone()),
            )?;
            log.lock().file = Some(File::open(log.path())?);
            log.write_panic("panic secret");
            log.log(Level::Info, "continued secret");
            Ok::<_, io::Error>((log.path().to_path_buf(), fs::read_to_string(log.path())?))
        })();
        fs::remove_dir_all(&dir)?;
        let (active, text) = result?;
        assert_eq!(active, private.join("agent.log"));
        assert!(text.contains("panic [REDACTED]"), "{text}");
        assert!(text.contains("continued [REDACTED]"), "{text}");
        assert!(!text.contains("secret"), "{text}");
        Ok(())
    }

    #[test]
    fn rotation_keeps_one_previous_file_and_rewrites_the_header() {
        let dir = temp_dir("rotate");
        let log = LogFile::open_with_limit(
            &dir,
            "desktop",
            "  version: 1.2.3\n".to_owned(),
            2048,
            None,
            None,
        )
        .unwrap_or_else(|error| panic!("open: {error}"));
        for index in 0..200 {
            log.log_unthrottled(Level::Info, &format!("line {index} {}", "x".repeat(40)));
        }
        let current = read(&dir.join("desktop.log"));
        let rotated = read(&dir.join("desktop.log.1"));
        assert!(
            current.len() < 2048 + 256,
            "current file stays near the limit"
        );
        assert!(rotated.len() >= 2048 - 256);
        assert!(current.contains("continued after rotation"));
        assert!(
            current.contains("version: 1.2.3"),
            "header repeated after rotation"
        );
        assert!(!dir.join("desktop.log.2").exists(), "only one rotated copy");
        drop(log);
        if let Err(error) = fs::remove_dir_all(&dir) {
            eprintln!("clean test directory failed: {error}");
        }
    }

    #[test]
    fn oversized_file_is_rotated_on_open() {
        let dir = temp_dir("reopen");
        fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("create test directory: {error}"));
        fs::write(dir.join("agent.log"), vec![b'a'; 4096])
            .unwrap_or_else(|error| panic!("write test log: {error}"));
        let log = LogFile::open_with_limit(&dir, "agent", String::new(), 1024, None, None)
            .unwrap_or_else(|error| panic!("open: {error}"));
        log.log(Level::Warn, "fresh");
        assert_eq!(
            fs::metadata(dir.join("agent.log.1"))
                .unwrap_or_else(|error| panic!("read rotated log metadata: {error}"))
                .len(),
            4096
        );
        assert!(read(&dir.join("agent.log")).contains("WARN  fresh"));
        drop(log);
        if let Err(error) = fs::remove_dir_all(&dir) {
            eprintln!("clean test directory failed: {error}");
        }
    }

    #[test]
    fn repeated_messages_are_summarized_in_the_file() {
        let dir = temp_dir("throttle");
        let log = LogFile::open(&dir, "desktop", String::new())
            .unwrap_or_else(|error| panic!("open: {error}"));
        let start = Instant::now();
        for _ in 0..25 {
            log.log_at(
                Level::Error,
                "DirectX device lost detected: 0x887A0005",
                start,
            );
        }
        log.flush_suppressed();
        let text = read(log.path());
        assert_eq!(text.matches("ERROR DirectX device lost").count(), 10);
        assert!(text.contains("[suppressed 15 repeats over 0s] DirectX device lost detected"));
        drop(log);
        if let Err(error) = fs::remove_dir_all(&dir) {
            eprintln!("clean test directory failed: {error}");
        }
    }

    #[test]
    fn multi_line_and_oversized_text_is_normalized() {
        let time = SystemTime::UNIX_EPOCH;
        let line = format_line(Level::Warn, "first\r\nsecond\n", Some("(tail)"), time);
        assert_eq!(
            line,
            "1970-01-01T00:00:00.000Z WARN  first\n    second (tail)\n"
        );
        let long = "é".repeat(MAX_LINE_BYTES);
        let line = format_line(Level::Info, &long, None, time);
        assert!(line.len() < MAX_LINE_BYTES + 128);
        assert!(line.contains("…[truncated"));
    }

    #[test]
    fn level_parsing_accepts_common_spellings() {
        assert_eq!(Level::parse(" WARN "), Some(Level::Warn));
        assert_eq!(Level::parse("warning"), Some(Level::Warn));
        assert_eq!(Level::parse("verbose"), None);
    }
}
