//! 全局文件日志 — 与同一数据目录的其他日志写入端写入同一目录/文件，按日期分文件。
//!
//! - 日志目录：由 `data_dir::resolve_data_dir()` 决定，加 `/logs` 后缀
//!   - Linux: `~/.local/share/fluxdown/logs/`
//!   - macOS: `~/Library/Application Support/fluxdown/logs/`
//!   - Windows 便携版: `<exe_dir>/portable_data/logs/`
//!   - Windows 安装版: `%LOCALAPPDATA%/FluxDown/logs/`
//! - 文件名：`fluxdown_YYYY-MM-DD.log`，分卷为 `fluxdown_YYYY-MM-DD.N.log`
//! - 两端都以 append 模式写入，POSIX `O_APPEND` 保证单次 write 原子性
//! - 启动时自动清理 7 天前的日志文件
//! - `tracing` 事件统一补充级别、target、源码位置；错误带稳定 `error_id` 并立即刷盘
//! - 进程 panic hook 与 `spawn_logged` 后台任务边界保证 panic/Err 不会静默丢失
//! - `health()` 暴露初始化与持久化写入降级状态，供诊断页与 Web UI 展示
//!
//! ## 自动分割与清理
//! - 单文件超过 2MB 自动分割到 `fluxdown_YYYY-MM-DD.N.log` 分卷；
//! - 日志总大小超过上限（默认 10MB，可通过 `set_max_total_bytes` 由设置覆盖）时
//!   按（日期, 分卷序号）从最旧开始删除；
//! - 清理只做目录遍历 + metadata，不读文件内容，内存占用极小。
//!
//! ## 用法
//! ```ignore
//! // 初始化（Rust runtime 启动时调用一次）
//! crate::logger::init();
//!
//! // 普通日志
//! log_info!("[module] some message: {}", value);
//!
//! // 错误日志（立即刷盘）
//! log_error!("[module] failed: {}", err);
//! ```

use std::error::Error as StdError;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use chrono::Local;
use thiserror::Error;
use tracing::{Instrument, Metadata};
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

static LOGGER: OnceLock<Arc<AppLogger>> = OnceLock::new();
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

/// 日志保留天数
const LOG_RETENTION_DAYS: u64 = 7;

/// 单个日志文件大小上限，超过则自动分割到新分卷
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// 日志目录总大小默认上限（可由设置覆盖）
const DEFAULT_MAX_TOTAL_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum LoggerInitError {
    #[error("failed to resolve application data directory")]
    ResolveDataDirectory(#[source] crate::data_dir::DataDirError),
    #[error("failed to create log directory {}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to initialize log file {}", path.display())]
    InitializeFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("logger is already initialized")]
    AlreadyInitialized,
    #[error("failed to install tracing subscriber")]
    InstallSubscriber(#[source] tracing::subscriber::SetGlobalDefaultError),
}

impl LoggerInitError {
    /// 同进程二次 isolate / 热重启：subscriber 与 LOGGER 已由上次 runtime 装好。
    pub fn is_already_initialized(&self) -> bool {
        matches!(self, Self::AlreadyInitialized | Self::InstallSubscriber(_))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoggerHealth {
    pub initialized: bool,
    pub degraded: bool,
    pub failure_count: u64,
    pub last_error: Option<String>,
}

struct LogState {
    date_tag: String,
    /// 当前日期内的分卷序号（0 = 无序号的首个文件）
    part: u32,
    file: Option<File>,
    /// 当前文件实际大小（每次写入后经 `metadata()` 刷新，见 `maybe_roll_by_size`）
    size: u64,
}

struct AppLogger {
    log_dir: PathBuf,
    max_total_bytes: AtomicU64,
    degraded: AtomicBool,
    failure_count: AtomicU64,
    last_error: Mutex<Option<String>>,
    state: Mutex<LogState>,
}

impl AppLogger {
    fn new(log_dir: PathBuf) -> Result<Self, LoggerInitError> {
        fs::create_dir_all(&log_dir).map_err(|source| LoggerInitError::CreateDirectory {
            path: log_dir.clone(),
            source,
        })?;
        Ok(Self {
            log_dir,
            max_total_bytes: AtomicU64::new(DEFAULT_MAX_TOTAL_BYTES),
            degraded: AtomicBool::new(false),
            failure_count: AtomicU64::new(0),
            last_error: Mutex::new(None),
            state: Mutex::new(LogState {
                date_tag: String::new(),
                part: 0,
                file: None,
                size: 0,
            }),
        })
    }

    // ── 内部写入 ──

    /// 写入一行日志，自动按日期切换文件、按大小分割。`flush` 为 true 时立即刷盘。
    fn write_impl(&self, message: &str, flush: bool) {
        if let Err(error) = self.write_line(message, flush) {
            self.record_failure("write", &error);
        }
    }

    fn write_line(&self, message: &str, flush: bool) -> io::Result<()> {
        let now = Local::now();
        let date_tag = now.format("%Y-%m-%d").to_string();
        let ts = now.format("%H:%M:%S%.3f").to_string();
        let line = format!("{ts} {message}\n");

        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.ensure_file(&mut state, &date_tag)?;
        let file = state
            .file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "log file is not open"))?;
        file.write_all(line.as_bytes())?;
        if flush {
            file.flush()?;
        }
        self.maybe_roll_by_size(&mut state)
    }

    fn record_failure(&self, operation: &str, error: &io::Error) {
        self.degraded.store(true, Ordering::Release);
        self.failure_count.fetch_add(1, Ordering::Relaxed);
        let message = format!("{operation}: {error}");
        match self.last_error.lock() {
            Ok(mut last_error) => *last_error = Some(message.clone()),
            Err(poisoned) => *poisoned.into_inner() = Some(message.clone()),
        }
        eprintln!(
            "{} [logger] persistent logging degraded: {message}",
            Local::now().format("%H:%M:%S%.3f")
        );
    }

    /// 确保日志文件已打开且日期匹配，否则切换到新文件。
    fn ensure_file(&self, state: &mut LogState, date_tag: &str) -> io::Result<()> {
        if state.date_tag == date_tag && state.file.is_some() {
            return Ok(());
        }
        if let Some(ref mut old) = state.file {
            old.flush()?;
        }
        state.file = None;
        state.date_tag = date_tag.to_string();
        state.part = self.scan_active_part(date_tag);
        self.open_current_file(state)
    }

    /// 打开 (date_tag, part) 对应的日志文件（append 模式），并 stat 初始化大小。
    fn open_current_file(&self, state: &mut LogState) -> io::Result<()> {
        let path = self.file_path(&state.date_tag, state.part);
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        state.size = file.metadata()?.len();
        state.file = Some(file);
        Ok(())
    }

    fn file_path(&self, date_tag: &str, part: u32) -> PathBuf {
        let name = if part == 0 {
            format!("fluxdown_{date_tag}.log")
        } else {
            format!("fluxdown_{date_tag}.{part}.log")
        };
        self.log_dir.join(name)
    }

    /// 找到 `date_tag` 当天已有的最大分卷序号；若该分卷已写满则返回下一个序号。
    /// 其他写入端可能已创建更高序号的分卷，通过该扫描收敛到同一文件。
    fn scan_active_part(&self, date_tag: &str) -> u32 {
        let mut max_part: Option<u32> = None;
        if let Ok(entries) = fs::read_dir(&self.log_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_str().unwrap_or("");
                if let Some((d, part)) = parse_log_name(name)
                    && d == date_tag
                {
                    max_part = Some(max_part.map_or(part, |m| m.max(part)));
                }
            }
        }
        let Some(max_part) = max_part else {
            return 0;
        };
        let size = fs::metadata(self.file_path(date_tag, max_part))
            .map(|m| m.len())
            .unwrap_or(0);
        if size >= MAX_FILE_BYTES {
            max_part + 1
        } else {
            max_part
        }
    }

    /// 每次写入后按**真实文件长度**决定是否分割，超限则切换到新分卷并触发
    /// 总量清理。
    ///
    /// 必须每次都 stat，不能靠自身写入量累加：其他日志写入端可能写同一个
    /// 文件，自身计数会低估分卷大小，导致不同写入端的时间线被拆散。
    /// 本端写入频率低，一次 fstat 的开销可忽略。
    fn maybe_roll_by_size(&self, state: &mut LogState) -> io::Result<()> {
        if let Some(ref file) = state.file {
            state.size = file.metadata()?.len();
        }
        if state.size < MAX_FILE_BYTES || state.date_tag.is_empty() {
            return Ok(());
        }

        if let Some(ref mut old) = state.file {
            old.flush()?;
        }
        state.file = None;
        // 防御：保证分卷序号单调递增，避免重新打开已写满的文件
        let next = self.scan_active_part(&state.date_tag);
        state.part = next.max(state.part + 1);
        self.open_current_file(state)?;
        self.enforce_total_size(state)
    }

    /// 写入启动 header。
    fn write_session_header(&self) -> io::Result<()> {
        let now = Local::now();
        let header = format!(
            "\n====== Rust runtime log session started at {} ======\n  pid: {}\n  exe: {}\n\n",
            now.format("%Y-%m-%d %H:%M:%S"),
            std::process::id(),
            std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
        );

        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let date_tag = now.format("%Y-%m-%d").to_string();
        self.ensure_file(&mut state, &date_tag)?;
        let file = state
            .file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "log file is not open"))?;
        file.write_all(header.as_bytes())?;
        file.flush()?;
        self.maybe_roll_by_size(&mut state)
    }

    /// 清理超过 `max_days` 天的 `fluxdown_*.log` 文件。
    fn cleanup_old_logs(&self, max_days: u64) -> io::Result<()> {
        let cutoff = SystemTime::now() - Duration::from_secs(max_days * 86400);
        for entry in fs::read_dir(&self.log_dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if !name.starts_with("fluxdown_") || !name.ends_with(".log") {
                continue;
            }
            let metadata = fs::metadata(&path)?;
            if metadata.modified()? < cutoff {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    /// 总大小超量清理：按（日期, 分卷序号）从最旧开始删除，
    /// 直到总大小回到上限内。当前活跃文件不删除。
    fn enforce_total_size(&self, state: &LogState) -> io::Result<()> {
        let max_total = self.max_total_bytes.load(Ordering::Relaxed);
        let mut files: Vec<(String, u32, PathBuf, u64)> = Vec::new();
        let mut total = 0_u64;
        for entry in fs::read_dir(&self.log_dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let Some((date, part)) = parse_log_name(name) else {
                continue;
            };
            let size = entry.metadata()?.len();
            total = total.saturating_add(size);
            files.push((date.to_string(), part, path, size));
        }
        if total <= max_total {
            return Ok(());
        }

        files.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let active = self.file_path(&state.date_tag, state.part);
        for (_, _, path, size) in files {
            if total <= max_total {
                break;
            }
            if path == active {
                continue;
            }
            fs::remove_file(path)?;
            total = total.saturating_sub(size);
        }
        Ok(())
    }
}

#[derive(Clone)]
struct AppLogWriterFactory {
    logger: Arc<AppLogger>,
}

struct AppLogWriter {
    logger: Arc<AppLogger>,
    bytes: Vec<u8>,
    flush: bool,
}

impl Write for AppLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for AppLogWriter {
    fn drop(&mut self) {
        let message = String::from_utf8_lossy(&self.bytes);
        for line in message.lines().filter(|line| !line.is_empty()) {
            self.logger.write_impl(line, self.flush);
        }
    }
}

impl<'writer> MakeWriter<'writer> for AppLogWriterFactory {
    type Writer = AppLogWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        AppLogWriter {
            logger: self.logger.clone(),
            bytes: Vec::with_capacity(256),
            flush: false,
        }
    }

    fn make_writer_for(&'writer self, metadata: &Metadata<'_>) -> Self::Writer {
        AppLogWriter {
            logger: self.logger.clone(),
            bytes: Vec::with_capacity(256),
            flush: *metadata.level() == tracing::Level::ERROR,
        }
    }
}

/// 解析日志文件名 `fluxdown_YYYY-MM-DD.log` / `fluxdown_YYYY-MM-DD.N.log`，
/// 返回 (日期, 分卷序号)。非日志文件返回 None。
fn parse_log_name(name: &str) -> Option<(&str, u32)> {
    let rest = name.strip_prefix("fluxdown_")?.strip_suffix(".log")?;
    let (date, part) = match rest.split_once('.') {
        Some((date, part)) => (date, part.parse::<u32>().ok()?),
        None => (rest, 0),
    };
    if date.len() != 10 {
        return None;
    }
    for (index, byte) in date.as_bytes().iter().enumerate() {
        let valid = if index == 4 || index == 7 {
            *byte == b'-'
        } else {
            byte.is_ascii_digit()
        };
        if !valid {
            return None;
        }
    }
    Some((date, part))
}

fn panic_payload<'a>(info: &'a std::panic::PanicHookInfo<'a>) -> &'a str {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        message
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    }
}

fn install_panic_hook() {
    PANIC_HOOK_INSTALLED.get_or_init(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let panic_message = single_line(panic_payload(info).to_string());
            let error_id = Uuid::new_v4();
            if let Some(location) = info.location() {
                tracing::error!(
                    %error_id,
                    panic_message,
                    panic_file = location.file(),
                    panic_line = location.line(),
                    panic_column = location.column(),
                    "panic"
                );
            } else {
                tracing::error!(%error_id, panic_message, "panic");
            }
            previous_hook(info);
        }));
    });
}

pub(crate) fn panic_hook_installed() -> bool {
    PANIC_HOOK_INSTALLED.get().is_some()
}

// ══════════════════════════════════════════════════
//  公开 API
// ══════════════════════════════════════════════════

/// 初始化全局日志与 `tracing` subscriber。必须在启动任何后台任务前调用。
///
/// 同进程再次初始化也会成功：subscriber 与文件 logger 是进程级一次性
/// 资源，已装好则直接返回。
pub fn init() -> Result<(), LoggerInitError> {
    let data_dir =
        crate::data_dir::resolve_data_dir(None).map_err(LoggerInitError::ResolveDataDirectory)?;
    init_at(data_dir.join("logs"))
}

/// 用显式数据目录初始化全局日志：日志写入 `<data_dir>/logs`。
///
/// 供 headless server 使用——它按 `FLUXDOWN_DATA_DIR` 解析数据目录，日志须
/// 随之落到同一（可能是挂载卷的）目录，而非平台默认的 HOME 路径。Docker
/// 部署（`FLUXDOWN_DATA_DIR=/data`）下日志因此持久化到 `/data/logs`。
pub fn init_with_dir(data_dir: &Path) -> Result<(), LoggerInitError> {
    init_at(data_dir.join("logs"))
}

/// 默认日志级别决定优先级（474#1）：
/// 1. `RUST_LOG` 环境变量——由 [`build_env_filter`] 处理，
///    非空时始终优先于本函数返回的默认指令；
/// 2. `FLUXDOWN_LOG_LEVEL` 环境变量（`error`/`warn`/`info`/`debug`/`trace`，
///    大小写不敏感）——未设置 `RUST_LOG` 时的默认级别，供 headless
///    server/CLI 无需改代码即可临时调高日志粒度排查问题；
/// 3. 以上均未设置 → `INFO`（桌面端 UI 侧的等级选择留待后续迭代，当前只做
///    环境变量这一层，见 474 讨论）。
fn resolve_default_level_filter() -> tracing_subscriber::filter::LevelFilter {
    parse_level_filter(std::env::var("FLUXDOWN_LOG_LEVEL").ok().as_deref())
}

/// [`resolve_default_level_filter`] 的纯实现，值显式传入以便测试
/// （不依赖/不修改进程级 `std::env` 状态）。
fn parse_level_filter(value: Option<&str>) -> tracing_subscriber::filter::LevelFilter {
    use tracing_subscriber::filter::LevelFilter;
    let Some(raw) = value else {
        return LevelFilter::INFO;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "error" => LevelFilter::ERROR,
        "warn" => LevelFilter::WARN,
        "info" => LevelFilter::INFO,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        other => {
            if !other.is_empty() {
                eprintln!(
                    "[logger] 忽略未知 FLUXDOWN_LOG_LEVEL='{other}'（合法值：error/warn/info/debug/trace），回退 info"
                );
            }
            LevelFilter::INFO
        }
    }
}

/// librqbit 按单个 peer / 单次 announce 打 INFO/WARN（MSE 握手成败、tracker
/// announce 成功）。实测 18 个种子约 40 行/秒，几分钟就把 2MB 的轮转日志冲掉，
/// 真正有用的诊断随之被挤出保留窗口。默认把这些目标压到更高等级。
#[cfg(feature = "bt")]
const NOISY_TARGET_DIRECTIVES: &[&str] = &[
    "librqbit::peer_connection=error",
    "librqbit::mse=warn",
    "librqbit_tracker_comms=warn",
];

/// 无 `bt` feature 时不链接 librqbit，没有需要压制的目标。
#[cfg(not(feature = "bt"))]
const NOISY_TARGET_DIRECTIVES: &[&str] = &[];

/// 组装全局过滤器。`rust_log` 为 `RUST_LOG` 的值：非空时完全由它决定，不叠加
/// 任何内置指令；否则以 `level` 为默认级别，且仅在 INFO/WARN 下压制
/// [`NOISY_TARGET_DIRECTIVES`]——显式调到 debug/trace 排查时保留全部输出，
/// ERROR 下这些指令反而会抬高目标等级，故跳过。
fn build_env_filter(
    rust_log: Option<&str>,
    level: tracing_subscriber::filter::LevelFilter,
) -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::filter::LevelFilter;
    let builder = tracing_subscriber::EnvFilter::builder().with_default_directive(level.into());
    if let Some(directives) = rust_log.filter(|value| !value.trim().is_empty()) {
        return builder.parse_lossy(directives);
    }
    let mut filter = builder.parse_lossy("");
    if level == LevelFilter::INFO || level == LevelFilter::WARN {
        for directive in NOISY_TARGET_DIRECTIVES {
            if let Ok(directive) = directive.parse() {
                filter = filter.add_directive(directive);
            }
        }
    }
    filter
}

fn init_at(log_dir: PathBuf) -> Result<(), LoggerInitError> {
    if let Some(existing) = LOGGER.get() {
        existing.write_impl("[logger] init skipped (already initialized)", false);
        return Ok(());
    }

    let logger = Arc::new(AppLogger::new(log_dir.clone())?);
    logger
        .write_session_header()
        .map_err(|source| LoggerInitError::InitializeFile {
            path: log_dir.clone(),
            source,
        })?;
    if let Err(error) = logger.cleanup_old_logs(LOG_RETENTION_DAYS) {
        logger.record_failure("cleanup old logs", &error);
    }
    {
        let state = match logger.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(error) = logger.enforce_total_size(&state) {
            logger.record_failure("enforce total log size", &error);
        }
    }

    let filter = build_env_filter(
        std::env::var("RUST_LOG").ok().as_deref(),
        resolve_default_level_filter(),
    );
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(AppLogWriterFactory {
            logger: logger.clone(),
        })
        .with_ansi(false)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .without_time()
        .compact()
        .with_filter(filter);
    let subscriber = tracing_subscriber::registry().with(fmt_layer);
    if let Err(error) = tracing::subscriber::set_global_default(subscriber) {
        // subscriber 已被上次 runtime 装上。把本次 logger 挂上（或接受
        // 竞态下别人先挂上的），不要当成致命错误掐死 actor。
        if LOGGER.set(logger.clone()).is_ok() {
            install_panic_hook();
            logger.write_impl(
                "[logger] tracing subscriber already installed; reused existing",
                false,
            );
            return Ok(());
        }
        if LOGGER.get().is_some() {
            return Ok(());
        }
        logger.write_impl(
            &format!("[logger] failed to install tracing subscriber: {error}"),
            true,
        );
        return Err(LoggerInitError::InstallSubscriber(error));
    }
    if LOGGER.set(logger).is_err() {
        return Ok(());
    }
    install_panic_hook();
    tracing::info!("[logger] tracing subscriber initialized");
    Ok(())
}

/// 设置日志目录总大小上限（字节），由设置项 `log_max_size_mb` 驱动。
/// 立即执行一次超量清理。低于 1MB 的值会被忽略。
pub fn set_max_total_bytes(bytes: u64) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    if bytes < 1024 * 1024 {
        return;
    }
    logger.max_total_bytes.store(bytes, Ordering::Relaxed);
    let state = match logger.state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Err(error) = logger.enforce_total_size(&state) {
        logger.record_failure("enforce total log size", &error);
    }
}

pub fn health() -> LoggerHealth {
    let Some(logger) = LOGGER.get() else {
        return LoggerHealth::default();
    };
    let last_error = match logger.last_error.lock() {
        Ok(last_error) => last_error.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    LoggerHealth {
        initialized: true,
        degraded: logger.degraded.load(Ordering::Acquire),
        failure_count: logger.failure_count.load(Ordering::Relaxed),
        last_error,
    }
}

/// 直接写入一条普通日志。新代码优先使用 [`log_info!`] 或 `tracing`。
pub fn write(message: &str) {
    if let Some(logger) = LOGGER.get() {
        logger.write_impl(message, false);
    } else {
        eprintln!(
            "{} [logger-uninitialized] {message}",
            Local::now().format("%H:%M:%S%.3f")
        );
    }
}

#[doc(hidden)]
pub fn trace_info(arguments: fmt::Arguments<'_>) {
    tracing::info!("{arguments}");
}

#[doc(hidden)]
pub fn trace_warn(arguments: fmt::Arguments<'_>) {
    tracing::warn!("{arguments}");
}
#[doc(hidden)]
pub fn trace_error(arguments: fmt::Arguments<'_>) {
    tracing::error!("{arguments}");
}

pub fn format_error_chain(error: &(dyn StdError + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    message
}
fn single_line(message: String) -> String {
    if message.contains(['\r', '\n']) {
        message.replace('\r', "\\r").replace('\n', "\\n")
    } else {
        message
    }
}

pub fn report_error(
    component: &'static str,
    operation: &'static str,
    error: &(dyn StdError + 'static),
) {
    let error_id = Uuid::new_v4();
    let error_chain = single_line(format_error_chain(error));
    tracing::error!(%error_id, component, operation, %error_chain, "operation failed");
}

pub fn report_warning(
    component: &'static str,
    operation: &'static str,
    error: &(dyn StdError + 'static),
) {
    let error_id = Uuid::new_v4();
    let error_chain = single_line(format_error_chain(error));
    tracing::warn!(%error_id, component, operation, %error_chain, "operation degraded");
}

pub fn report_anyhow(component: &'static str, operation: &'static str, error: &anyhow::Error) {
    let error_id = Uuid::new_v4();
    let error_chain = single_line(format!("{error:#}"));
    tracing::error!(
        %error_id,
        component,
        operation,
        %error_chain,
        "operation failed"
    );
}

pub fn spawn_logged<F, E>(
    component: &'static str,
    operation: &'static str,
    future: F,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: Into<anyhow::Error> + Send + 'static,
{
    tokio::spawn(async move {
        let span = tracing::error_span!("background_task", component, operation);
        match tokio::spawn(future.instrument(span)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => report_anyhow(component, operation, &error.into()),
            Err(error) if error.is_panic() && PANIC_HOOK_INSTALLED.get().is_some() => {
                // The process-wide panic hook already recorded the panic while
                // this task's span was active.
            }
            Err(error) => report_error(component, operation, &error),
        }
    })
}

/// 单个日志文件的元信息（列举与导出用）。
pub struct LogFileMeta {
    /// 文件名（`fluxdown_YYYY-MM-DD.log` / `fluxdown_YYYY-MM-DD.N.log`）。
    pub name: String,
    /// 文件字节大小。
    pub size: u64,
}

/// 当前日志目录的绝对路径。初始化后返回真实目录，否则回退平台解析。
pub fn log_dir() -> PathBuf {
    LOGGER
        .get()
        .map(|l| l.log_dir.clone())
        .unwrap_or_else(resolve_log_dir)
}

/// 列举日志目录下全部日志文件，按文件名升序（即日期 + 分卷序）。
///
/// 只识别 `fluxdown_YYYY-MM-DD[.N].log` 命名的文件，忽略目录内其它内容。
pub fn list_log_files() -> Vec<LogFileMeta> {
    list_log_files_in(&log_dir())
}

/// [`list_log_files`] 的纯实现，目录显式传入以便测试。
fn list_log_files_in(dir: &std::path::Path) -> Vec<LogFileMeta> {
    let mut files: Vec<LogFileMeta> = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if parse_log_name(&name).is_none() {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        files.push(LogFileMeta { name, size });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

static SANITIZE_RULES: std::sync::LazyLock<Vec<(regex::Regex, &'static str)>> =
    std::sync::LazyLock::new(|| {
        fluxdown_logfile::SANITIZE_PATTERNS
            .iter()
            .map(|&(pattern, replacement)| {
                let regex = regex::Regex::new(pattern)
                    .unwrap_or_else(|error| panic!("invalid log sanitization regex: {error}"));
                (regex, replacement)
            })
            .collect()
    });

/// 对日志文本脱敏（URL userinfo、令牌路径/query、Cookie、Authorization、用户目录）。
pub fn sanitize_log_str(text: &str) -> String {
    let mut content = text.to_owned();
    for (regex, replacement) in SANITIZE_RULES.iter() {
        content = regex.replace_all(&content, *replacement).into_owned();
    }
    content
}

/// [`sanitize_log_str`] 的字节版本（非 UTF-8 按 lossy 处理），供导出日志复用。
pub fn sanitize_log_bytes(data: &[u8]) -> Vec<u8> {
    sanitize_log_str(&String::from_utf8_lossy(data)).into_bytes()
}

/// 将日志目录下全部日志文件脱敏后打包为 zip 字节（deflate 压缩），供
/// headless server 的「导出日志」下载端点使用。脱敏规则与 agent 导出共用
/// `fluxdown_logfile::SANITIZE_PATTERNS`。
///
/// 需 `components` 或 `plugins` feature（`zip` 依赖随之启用）；导出瞬间被
/// 清理的单个文件会被跳过，不使整个导出失败。
#[cfg(any(feature = "components", feature = "plugins"))]
pub fn export_logs_zip() -> Result<Vec<u8>, String> {
    export_logs_zip_from(&log_dir())
}

/// [`export_logs_zip`] 的纯实现，目录显式传入以便测试。
#[cfg(any(feature = "components", feature = "plugins"))]
fn export_logs_zip_from(dir: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Cursor;

    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for meta in list_log_files_in(dir) {
        let Ok(data) = fs::read(dir.join(&meta.name)) else {
            continue;
        };
        zw.start_file(meta.name.as_str(), opts)
            .map_err(|e| e.to_string())?;
        zw.write_all(&sanitize_log_bytes(&data))
            .map_err(|e| e.to_string())?;
    }
    let cursor = zw.finish().map_err(|e| e.to_string())?;
    Ok(cursor.into_inner())
}

// ══════════════════════════════════════════════════
//  路径解析 — 委托 data_dir 模块
// ══════════════════════════════════════════════════

fn resolve_log_dir() -> PathBuf {
    match crate::data_dir::resolve_data_dir(None) {
        Ok(data_dir) => data_dir.join("logs"),
        Err(error) => {
            eprintln!(
                "{} [logger] failed to resolve log directory: {error}",
                Local::now().format("%H:%M:%S%.3f")
            );
            PathBuf::from(".").join("logs")
        }
    }
}

// ══════════════════════════════════════════════════
//  日志宏
//
//  `#[macro_export]` 把宏放到 crate 根路径(`fluxdown_engine::log_info!`),
//  下方 `pub use` 把它们重新导出回 `logger` 模块路径，支持
//  `fluxdown_engine::logger::log_info!` 的模块限定调用。
//  宏体内必须用 `$crate` 而非 `crate`——`crate::` 在 `macro_rules!` 里按
//  *调用点* 所在 crate 解析,只有 `$crate` 才会不论调用点在哪个 crate,
//  始终指回定义宏的 `fluxdown_engine`。
// ══════════════════════════════════════════════════

/// 记录普通日志，格式同 `format!()`。
///
/// ```ignore
/// log_info!("[actor] task created: id={}", id);
/// ```
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logger::trace_info(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::logger::trace_warn(format_args!($($arg)*))
    };
}

/// 记录错误日志并由 tracing writer 立即刷盘，格式同 `format!()`。
///
/// ```ignore
/// log_error!("[actor] database open failed: {e:#}");
/// ```
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logger::trace_error(format_args!($($arg)*))
    };
}

#[allow(unused_imports)]
pub use crate::log_error;
#[allow(unused_imports)]
pub use crate::log_info;
#[allow(unused_imports)]
pub use crate::log_warn;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use thiserror::Error;

    use super::sanitize_log_str;

    #[test]
    fn sanitize_redacts_bot_token_and_secret_query_params() {
        let s = sanitize_log_str(
            "error sending request for url (https://api.telegram.org/bot123456:AAE-xyz_1/sendMessage?chat_id=9)",
        );
        assert!(!s.contains("AAE-xyz_1"), "{s}");
        let s = sanitize_log_str("GET https://h/x?a=1&token=abc&apiKey=def&n=2");
        assert!(!s.contains("abc") && !s.contains("def"), "{s}");
        assert!(s.contains("a=1") && s.contains("n=2"), "{s}");
        let s = sanitize_log_str("ftp://user:pass@host/f");
        assert!(!s.contains("pass"), "{s}");
    }

    #[cfg_attr(not(feature = "bt"), allow(unused_imports))]
    use super::{
        AppLogWriterFactory, AppLogger, LoggerInitError, build_env_filter, format_error_chain,
        install_panic_hook, list_log_files_in, parse_level_filter, parse_log_name, report_error,
        spawn_logged,
    };

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("fluxdown_logtest_{tag}_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // 474#1：FLUXDOWN_LOG_LEVEL 决定未设置 RUST_LOG 时的默认级别。
    #[test]
    fn parse_level_filter_recognizes_all_levels() {
        use tracing_subscriber::filter::LevelFilter;
        assert_eq!(parse_level_filter(Some("error")), LevelFilter::ERROR);
        assert_eq!(parse_level_filter(Some("WARN")), LevelFilter::WARN);
        assert_eq!(parse_level_filter(Some(" info ")), LevelFilter::INFO);
        assert_eq!(parse_level_filter(Some("Debug")), LevelFilter::DEBUG);
        assert_eq!(parse_level_filter(Some("trace")), LevelFilter::TRACE);
    }

    #[test]
    fn parse_level_filter_falls_back_to_info_when_unset_or_invalid() {
        use tracing_subscriber::filter::LevelFilter;
        assert_eq!(parse_level_filter(None), LevelFilter::INFO);
        assert_eq!(parse_level_filter(Some("")), LevelFilter::INFO);
        assert_eq!(parse_level_filter(Some("verbose")), LevelFilter::INFO);
    }

    #[cfg(feature = "bt")]
    #[test]
    fn default_filter_silences_per_peer_bt_noise_but_yields_to_explicit_config() {
        use tracing::Level;
        use tracing_subscriber::filter::LevelFilter;
        use tracing_subscriber::layer::SubscriberExt;

        // (MSE 逐 peer 警告, tracker announce 成功, 引擎自身 INFO)
        let probe = |rust_log: Option<&str>, level: LevelFilter| {
            let subscriber = tracing_subscriber::registry().with(build_env_filter(rust_log, level));
            tracing::subscriber::with_default(subscriber, || {
                (
                    tracing::event_enabled!(target: "librqbit::peer_connection", Level::WARN),
                    tracing::event_enabled!(target: "librqbit_tracker_comms::tracker_comms", Level::INFO),
                    tracing::event_enabled!(target: "fluxdown_engine::logger", Level::INFO),
                )
            })
        };

        assert_eq!(probe(None, LevelFilter::INFO), (false, false, true));
        assert_eq!(probe(Some("  "), LevelFilter::INFO), (false, false, true));
        // 显式调到 debug 排查：不压制。
        assert_eq!(probe(None, LevelFilter::DEBUG), (true, true, true));
        // RUST_LOG 完全接管，内置压制指令不叠加。
        assert_eq!(probe(Some("info"), LevelFilter::INFO), (true, true, true));
        // ERROR 下不得因目标指令反而放出 WARN。
        assert_eq!(probe(None, LevelFilter::ERROR), (false, false, false));
    }

    #[test]
    fn parse_plain_daily_file() {
        assert_eq!(
            parse_log_name("fluxdown_2026-06-10.log"),
            Some(("2026-06-10", 0))
        );
    }

    #[test]
    fn parse_part_file() {
        assert_eq!(
            parse_log_name("fluxdown_2026-06-10.3.log"),
            Some(("2026-06-10", 3))
        );
    }

    #[test]
    fn reject_non_log_names() {
        assert_eq!(parse_log_name("fluxdown_logs.zip"), None);
        assert_eq!(parse_log_name("fluxdown_backup.log"), None);
        assert_eq!(parse_log_name("fluxdown_2026-06-10.abc.log"), None);
        assert_eq!(parse_log_name("other_2026-06-10.log"), None);
    }

    #[test]
    fn list_log_files_filters_non_logs_and_sorts_ascending() {
        let dir = temp_dir("list");
        std::fs::write(dir.join("fluxdown_2026-01-02.log"), b"b").unwrap();
        std::fs::write(dir.join("fluxdown_2026-01-01.log"), b"aa").unwrap();
        std::fs::write(dir.join("fluxdown_2026-01-01.1.log"), b"ccc").unwrap();
        std::fs::write(dir.join("readme.txt"), b"ignore me").unwrap();

        let files = list_log_files_in(&dir);
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        // 非日志文件被过滤；其余按文件名升序（日期 + 分卷序）。
        assert_eq!(
            names,
            [
                "fluxdown_2026-01-01.1.log",
                "fluxdown_2026-01-01.log",
                "fluxdown_2026-01-02.log",
            ]
        );
        // 大小如实反映内容字节数。
        assert_eq!(files[0].size, 3);
        assert_eq!(files[1].size, 2);
        assert_eq!(files[2].size, 1);

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    #[test]
    #[cfg(any(feature = "components", feature = "plugins"))]
    fn export_logs_zip_packs_only_log_files_with_content() {
        use std::io::{Cursor, Read};

        let dir = temp_dir("zip");
        std::fs::write(
            dir.join("fluxdown_2026-01-01.log"),
            concat!(
                "Authorization: Bearer top-secret\n",
                "https://cdn.example/file?abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ\n",
                "C:\\Users\\zero\\Downloads /home/zero/downloads\n",
            ),
        )
        .unwrap();
        std::fs::write(dir.join("fluxdown_2026-01-02.log"), b"world!!").unwrap();
        std::fs::write(dir.join("notes.md"), b"skip").unwrap();

        let bytes = super::export_logs_zip_from(&dir).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        // 仅两个日志文件入包，非日志被排除。
        assert_eq!(zip.len(), 2);

        let mut got = std::collections::BTreeMap::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let mut content = String::new();
            entry.read_to_string(&mut content).unwrap();
            got.insert(entry.name().to_string(), content);
        }
        let sanitized = got
            .get("fluxdown_2026-01-01.log")
            .map(String::as_str)
            .unwrap();
        assert!(sanitized.contains("Authorization: [REDACTED]"));
        assert!(sanitized.contains("?[QUERY_REDACTED]"));
        assert!(sanitized.contains(r"C:\Users\***\Downloads"));
        assert!(sanitized.contains("/home/***/downloads"));
        assert!(!sanitized.contains("top-secret"));
        assert_eq!(
            got.get("fluxdown_2026-01-02.log").map(String::as_str),
            Some("world!!")
        );
        assert!(!got.contains_key("notes.md"));

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    #[derive(Debug, Error)]
    #[error("outer failure")]
    struct OuterError {
        #[source]
        source: std::io::Error,
    }

    #[test]
    fn formats_complete_error_chain() {
        let error = OuterError {
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "root failure"),
        };
        assert_eq!(format_error_chain(&error), "outer failure: root failure");
    }

    #[test]
    fn already_initialized_is_reentry_safe() {
        assert!(LoggerInitError::AlreadyInitialized.is_already_initialized());
        let fatal = LoggerInitError::CreateDirectory {
            path: std::path::PathBuf::from("/tmp"),
            source: std::io::Error::other("nope"),
        };
        assert!(!fatal.is_already_initialized());
    }

    #[test]
    fn tracing_error_is_persisted_once_with_context() {
        let dir = temp_dir("tracing");
        let logger = Arc::new(AppLogger::new(dir.clone()).unwrap());
        let subscriber = tracing_subscriber::fmt()
            .with_writer(AppLogWriterFactory { logger })
            .with_ansi(false)
            .with_target(false)
            .without_time()
            .compact()
            .finish();
        let error = OuterError {
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "root failure\nsecond line",
            ),
        };
        tracing::subscriber::with_default(subscriber, || {
            report_error("test-component", "test-operation", &error);
        });

        let files = list_log_files_in(&dir);
        assert_eq!(files.len(), 1);
        let content = std::fs::read_to_string(dir.join(&files[0].name)).unwrap();
        assert_eq!(content.matches("operation failed").count(), 1);
        assert!(content.contains("test-component"));
        assert!(content.contains("test-operation"));
        assert!(content.contains(r"outer failure: root failure\nsecond line"));
        assert!(!content.contains("root failure\nsecond line"));

        if let Err(error) = std::fs::remove_dir_all(dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_error_is_persisted_once_with_complete_chain() {
        let dir = temp_dir("background_error");
        let logger = Arc::new(AppLogger::new(dir.clone()).unwrap());
        let subscriber = tracing_subscriber::fmt()
            .with_writer(AppLogWriterFactory { logger })
            .with_ansi(false)
            .with_target(false)
            .without_time()
            .compact()
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let _default = tracing::dispatcher::set_default(&dispatch);

        spawn_logged("task-component", "task-operation", async {
            Err(anyhow::Error::new(OuterError {
                source: std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "background root failure",
                ),
            }))
        })
        .await
        .unwrap();

        let files = list_log_files_in(&dir);
        assert_eq!(files.len(), 1);
        let content = std::fs::read_to_string(dir.join(&files[0].name)).unwrap();
        assert_eq!(content.matches("operation failed").count(), 1);
        assert!(content.contains("task-component"));
        assert!(content.contains("task-operation"));
        assert!(content.contains("outer failure: background root failure"));

        if let Err(error) = std::fs::remove_dir_all(dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    #[allow(clippy::panic)]
    fn panic_result() -> anyhow::Result<()> {
        panic!("background exploded")
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::panic)]
    async fn background_panic_is_persisted_once_with_task_context() {
        let dir = temp_dir("panic");
        let logger = Arc::new(AppLogger::new(dir.clone()).unwrap());
        let subscriber = tracing_subscriber::fmt()
            .with_writer(AppLogWriterFactory { logger })
            .with_ansi(false)
            .with_target(false)
            .without_time()
            .compact()
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let _default = tracing::dispatcher::set_default(&dispatch);
        install_panic_hook();

        spawn_logged("panic-component", "panic-operation", async {
            panic_result()
        })
        .await
        .unwrap();

        let files = list_log_files_in(&dir);
        assert_eq!(files.len(), 1);
        let content = std::fs::read_to_string(dir.join(&files[0].name)).unwrap();
        assert_eq!(content.matches("background exploded").count(), 1);
        assert!(content.contains("panic-component"));
        assert!(content.contains("panic-operation"));

        if let Err(error) = std::fs::remove_dir_all(dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    #[test]
    fn persistent_write_failure_sets_degraded_health() {
        let dir = temp_dir("degraded");
        let logger = AppLogger::new(dir.clone()).unwrap();
        logger.write_impl("first", true);
        {
            let mut state = logger.state.lock().unwrap();
            state.file = None;
        }
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();

        logger.write_impl("second", true);

        assert!(logger.degraded.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            logger
                .failure_count
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(
            logger
                .last_error
                .lock()
                .unwrap()
                .as_deref()
                .is_some_and(|error| error.starts_with("write:"))
        );

        if let Err(error) = std::fs::remove_file(dir) {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "clean test path: {error}"
            );
        }
    }
}
