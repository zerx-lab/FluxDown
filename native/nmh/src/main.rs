//! FluxDown Native Messaging Host (NMH) relay binary.
//!
//! Chrome/Edge/Firefox launches this process when the browser extension calls
//! `chrome.runtime.connectNative("com.fluxdown.nmh")`.
//!
//! Communication flow:
//!   Browser extension <-(stdin/stdout, 4-byte LE length + JSON)-> this process
//!   this process <-(Named Pipe, 4-byte LE length + JSON)-> FluxDown App
//!
//! Design:
//!   - Synchronous, single-threaded, no async runtime.
//!   - Pipe connection is lazy: established on first message, reconnected on error.
//!   - When the FluxDown App is not running, NMH automatically launches it and
//!     polls for the IPC endpoint at a fixed 50ms interval (up to 10s). On macOS
//!     the agent is launched through Launch Services (see [`launch_app`]) so it
//!     never inherits the browser's process-level quarantine.
//!   - The "no-launch" action set (see `NO_LAUNCH_ACTIONS`) — "ping" plus the
//!     task-panel query/control actions — only checks connectivity and never
//!     launches the App.
//!   - "warmup" messages ensure the App is running and the pipe is connected,
//!     then are answered locally (never forwarded to the App). The extension
//!     sends one at download-flow entry so App cold-start overlaps with its
//!     cookie collection instead of running after it.
//!   - Diagnostic log is written to `%TEMP%/fluxdown_nmh.log`.
//!   - Message size limit: 1 MB (Chrome NMH hard limit).

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Product version: the release pipeline injects the tag version through `FLUXDOWN_APP_VERSION`;
/// local builds fall back to this crate's version. Same rule as `fluxdown_protocol::APP_VERSION`
/// (this relay deliberately has no dependency on the protocol crate).
const APP_VERSION: &str = match option_env!("FLUXDOWN_APP_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => env!("CARGO_PKG_VERSION"),
};

/// Maximum message size: 1 MB (Chrome NMH limit).
const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;

/// Actions the NMH relay answers/forwards without auto-launching the App
/// when it isn't already running — mirrors the shared NMH contract's
/// no-launch action set. `ping` is a pure liveness check; the task-panel
/// query/control actions target an already-running App, so failing fast
/// beats a multi-second cold-start stall for a popup that has nothing to
/// show anyway. `download`/`warmup` are deliberately excluded — those are
/// the entry points that must launch the App.
const NO_LAUNCH_ACTIONS: &[&str] = &[
    "ping",
    "tasks",
    "task_op",
    "open_file",
    "reveal_file",
    "capabilities",
    "capture_status",
    "sniff_rules",
    "page_scripts",
    "page_script_source",
    "plugin_invoke",
];

/// Whether `action` belongs to [`NO_LAUNCH_ACTIONS`].
fn is_no_launch_action(action: &str) -> bool {
    NO_LAUNCH_ACTIONS.contains(&action)
}

/// Unix IPC socket: `<data dir>/ipc/fluxdown.sock`, where `ipc` is a per-user 0700 directory
/// created by the agent. The data dir is under the user's home on purpose: it is reachable
/// from both the host and Flatpak/Snap sandboxes (unlike `$XDG_RUNTIME_DIR`). The agent
/// (`native/agent/src/nmh.rs`) derives the same path; keep both in lockstep.
#[cfg(any(not(windows), test))]
fn socket_path_under(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    let data_dir = home
        .join("Library")
        .join("Application Support")
        .join("fluxdown");
    #[cfg(not(target_os = "macos"))]
    let data_dir = home.join(".local").join("share").join("fluxdown");
    data_dir.join("ipc").join("fluxdown.sock")
}

/// Windows Named Pipe for the current account: `\\.\pipe\fluxdown-<account>`. Every byte of
/// the lower-cased account name outside `[a-z0-9]` is encoded as `_xx` (the underscore is
/// itself encoded), so distinct accounts never share a pipe. The agent
/// (`native/agent/src/nmh.rs`) derives the same name; keep both in lockstep.
#[cfg(any(windows, test))]
fn pipe_name_for(user: &str) -> Option<String> {
    if user.is_empty() {
        return None;
    }
    let mut name = String::from(r"\\.\pipe\fluxdown-");
    for byte in user.to_lowercase().bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            name.push(char::from(byte));
        } else {
            name.push_str(&format!("_{byte:02x}"));
        }
    }
    Some(name)
}

/// Cold-launch candidates, in priority order, searched next to the NMH
/// binary. Only `fluxdown-agent` (GPUI stack) is launched.
#[cfg(windows)]
const APP_EXE_CANDIDATES: &[&str] = &["fluxdown-agent.exe"];
#[cfg(not(windows))]
const APP_EXE_CANDIDATES: &[&str] = &["fluxdown-agent"];

/// Maximum time (ms) to wait for the App to start and create its pipe.
const APP_LAUNCH_TIMEOUT_MS: u64 = 10_000;

/// Polling interval (ms) while waiting for the App's IPC endpoint after
/// launching it. Fixed and short: connecting to a nonexistent local pipe or
/// socket fails in microseconds, so tight polling is essentially free —
/// exponential back-off would quantize the observed connect latency to its
/// checkpoint times (measured: endpoint ready in ~300-700ms, but back-off
/// checkpoints at 700/1500ms wasted up to ~800ms per cold start).
const PIPE_POLL_INTERVAL_MS: u64 = 50;

/// Minimum cooldown (ms) between two App launch attempts.
/// Prevents crash-loops if the App crashes on start.
const APP_LAUNCH_COOLDOWN_MS: u64 = 15_000;

/// Incoming message from the browser extension.
#[derive(Debug, Deserialize)]
struct IncomingMessage {
    #[serde(default)]
    action: String,
    #[serde(default)]
    msg_id: u64,
}

/// Response sent back to the browser extension for messages the NMH answers
/// locally: error replies and "warmup" acknowledgements.
#[derive(Debug, Serialize)]
struct HostResponse {
    success: bool,
    message: String,
    msg_id: u64,
}

/// Serialize and write a locally-generated response to stdout.
fn respond_status(success: bool, message: &str, msg_id: u64) -> io::Result<()> {
    let resp = HostResponse {
        success,
        message: message.to_string(),
        msg_id,
    };
    let json = serde_json::to_vec(&resp).map_err(io::Error::other)?;
    write_stdout_message(&json)
}

// ---------------------------------------------------------------------------
// Diagnostic logging (writes to %TEMP%/fluxdown_nmh.log)
// ---------------------------------------------------------------------------

/// Resolve the NMH log file path.
fn log_path() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("TEMP")
            .or_else(|_| std::env::var("TMP"))
            .ok()
            .map(|tmp| Path::new(&tmp).join("fluxdown_nmh.log"))
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS, Chrome launches NMH via launchd which may not set $HOME.
        // Use home_dir() which falls back to getpwuid when $HOME is absent.
        if let Some(home) = home_dir() {
            let dir = home
                .join("Library")
                .join("Application Support")
                .join("fluxdown");
            if let Err(error) = std::fs::create_dir_all(&dir) {
                eprintln!("fluxdown_nmh: cannot create log directory: {error}");
                return None;
            }
            return Some(dir.join("fluxdown_nmh.log"));
        }
        Some(Path::new("/tmp").join("fluxdown_nmh.log"))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        // Linux: use ~/.local/share/fluxdown/fluxdown_nmh.log
        // Consistent with socket_path() — avoids $XDG_RUNTIME_DIR which gets
        // remapped inside Flatpak/Snap sandboxes and may differ between the app
        // process (host) and the NMH process (launched by sandboxed browser).
        if let Some(home) = home_dir() {
            let dir = home.join(".local").join("share").join("fluxdown");
            if let Err(error) = std::fs::create_dir_all(&dir) {
                eprintln!("fluxdown_nmh: cannot create log directory: {error}");
                return None;
            }
            return Some(dir.join("fluxdown_nmh.log"));
        }
        Some(Path::new("/tmp").join("fluxdown_nmh.log"))
    }
}

/// Append a timestamped line to the NMH log file.
/// Failures go to stderr, never back into this logger or the stdout wire.
fn log(msg: &str) {
    let Some(path) = log_path() else {
        return;
    };
    let mut f = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) => {
            eprintln!("fluxdown_nmh: cannot open log file: {error}");
            return;
        }
    };
    match f.metadata() {
        Ok(meta) if meta.len() > 256 * 1024 => {
            if let Err(error) = f.set_len(0) {
                eprintln!("fluxdown_nmh: cannot truncate log file: {error}");
                return;
            }
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("fluxdown_nmh: cannot inspect log file: {error}");
            return;
        }
    }
    let now = chrono_free_timestamp();
    if let Err(error) = writeln!(f, "[{now}] {msg}") {
        eprintln!("fluxdown_nmh: cannot write log file: {error}");
    }
}

/// Simple timestamp without pulling in chrono — "YYYY-MM-DD HH:MM:SS".
fn chrono_free_timestamp() -> String {
    // Use std::time for elapsed since NMH start; not wall-clock but cheap.
    // For wall-clock we'd need `chrono` or Win32 GetLocalTime. Keep it simple.
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    // UTC is fine for diagnostics; avoids timezone complexity.
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

// ---------------------------------------------------------------------------
// Home directory resolution (macOS: std passwd fallback for launchd env)
// ---------------------------------------------------------------------------

/// Returns the current user's home directory.
///
/// On macOS, Chrome/Firefox launch the NMH process via launchd, which may
/// strip $HOME from the environment. The standard library falls back to the
/// passwd database and preserves non-UTF-8 filesystem paths.
#[cfg(target_os = "macos")]
fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| !home.as_os_str().is_empty())
}

/// Returns the current user's home directory on Linux.
///
/// On Linux, Chrome launches the NMH process directly (not via a session
/// manager that strips environment variables), so $HOME is reliably set.
/// This mirrors the macOS version's signature so pipe::socket_path() can
/// call super::home_dir() uniformly on both platforms.
#[cfg(target_os = "linux")]
fn home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home));
    }
    None
}

// ---------------------------------------------------------------------------
// stdin/stdout helpers (4-byte LE length-prefixed JSON, per NMH protocol)
// ---------------------------------------------------------------------------

/// Read one NMH message from stdin.
/// Returns `None` on EOF (extension disconnected).
fn read_stdin_message() -> Option<Vec<u8>> {
    let stdin = io::stdin();
    let mut handle = stdin.lock();

    let mut len_buf = [0u8; 4];
    if handle.read_exact(&mut len_buf).is_err() {
        return None;
    }
    let len = u32::from_le_bytes(len_buf);
    if len == 0 || len > MAX_MESSAGE_SIZE {
        return None;
    }

    let mut buf = vec![0u8; len as usize];
    if handle.read_exact(&mut buf).is_err() {
        return None;
    }
    Some(buf)
}

/// Write one NMH message to stdout.
fn write_stdout_message(data: &[u8]) -> io::Result<()> {
    let stdout = io::stdout();
    write_message_frame(&mut stdout.lock(), data)
}

fn write_message_frame(writer: &mut impl Write, data: &[u8]) -> io::Result<()> {
    let len = u32::try_from(data.len()).map_err(io::Error::other)?;
    if len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(data)?;
    writer.flush()
}

// ---------------------------------------------------------------------------
// Named Pipe helpers (Windows)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod pipe {
    use std::fs::OpenOptions;
    use std::io::{self, Read, Write};

    pub struct PipeHandle {
        file: std::fs::File,
    }

    impl PipeHandle {
        /// Open the App's Named Pipe. `NotFound` means the App is not running; any other error
        /// (notably `PermissionDenied` for a pipe created by an elevated App) is worth logging.
        pub fn connect(pipe_name: &str) -> io::Result<Self> {
            let file = OpenOptions::new().read(true).write(true).open(pipe_name)?;
            Ok(PipeHandle { file })
        }

        pub fn write_message(&mut self, data: &[u8]) -> io::Result<()> {
            let len = data.len() as u32;
            self.file.write_all(&len.to_le_bytes())?;
            self.file.write_all(data)?;
            // NOTE: flush() is intentionally omitted.
            // On Windows Named Pipes, File::flush() calls FlushFileBuffers(), which
            // BLOCKS until the remote end reads all data. If the Tokio async server
            // hasn't scheduled its read yet, this deadlocks for ~17 seconds until
            // Windows aborts the I/O. Named pipe writes go to the kernel buffer
            // immediately — no explicit flush is needed.
            Ok(())
        }

        pub fn read_message(&mut self) -> io::Result<Vec<u8>> {
            let mut len_buf = [0u8; 4];
            self.file.read_exact(&mut len_buf)?;
            let len = u32::from_le_bytes(len_buf);
            if len > super::MAX_MESSAGE_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "message too large",
                ));
            }
            let mut buf = vec![0u8; len as usize];
            self.file.read_exact(&mut buf)?;
            Ok(buf)
        }
    }
}

// Non-Windows: connect to FluxDown via Unix Domain Socket.
#[cfg(not(windows))]
mod pipe {
    use std::io::{self, Read, Write};
    use std::os::unix::net::UnixStream;

    /// Resolve the per-user Unix socket path the FluxDown agent listens on. There is no
    /// shared-directory fallback: without a home directory the endpoint is unavailable.
    fn socket_path() -> Option<std::path::PathBuf> {
        super::home_dir().map(|home| super::socket_path_under(&home))
    }

    pub struct PipeHandle {
        stream: UnixStream,
    }

    impl PipeHandle {
        /// Connect to the FluxDown Unix socket. `NotFound` / `ConnectionRefused` mean the app
        /// is not running.
        pub fn connect(_ignored: &str) -> io::Result<Self> {
            let path = socket_path().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "home directory is unavailable")
            })?;
            let stream = UnixStream::connect(&path)?;
            Ok(PipeHandle { stream })
        }

        pub fn write_message(&mut self, data: &[u8]) -> io::Result<()> {
            let len = data.len() as u32;
            self.stream.write_all(&len.to_le_bytes())?;
            self.stream.write_all(data)?;
            self.stream.flush()?;
            Ok(())
        }

        pub fn read_message(&mut self) -> io::Result<Vec<u8>> {
            let mut len_buf = [0u8; 4];
            self.stream.read_exact(&mut len_buf)?;
            let len = u32::from_le_bytes(len_buf);
            if len > super::MAX_MESSAGE_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "message too large",
                ));
            }
            let mut buf = vec![0u8; len as usize];
            self.stream.read_exact(&mut buf)?;
            Ok(buf)
        }
    }
}

// ---------------------------------------------------------------------------
// App auto-launch
// ---------------------------------------------------------------------------

/// Find the FluxDown App executable.
///
/// Search order:
/// 1. Same directory as the NMH binary, following [`APP_EXE_CANDIDATES`]
/// 2. Cargo output for `fluxdown-agent` (development fallback)
fn find_app_exe() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
        && let Some(found) = first_existing(dir, APP_EXE_CANDIDATES)
    {
        return Some(found);
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir)
        .parent()
        .and_then(|path| path.parent())?;

    ["debug", "release"].iter().find_map(|profile| {
        let dir = workspace_root.join("target").join(profile);
        first_existing(&dir, APP_EXE_CANDIDATES)
    })
}

/// First `names` entry that exists inside `dir`.
fn first_existing(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|name| dir.join(name)).find(|p| p.exists())
}

/// Launch the FluxDown App as a detached process.
#[cfg(windows)]
fn launch_app(app_exe: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    // Firefox 把 NMH 放进会在 NMH 退出时终止整个 Job 的 Job object；
    // 不脱离则冷启动的 agent / daemon 会随 NMH 一起被杀。
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;
    const ERROR_ACCESS_DENIED: i32 = 5;

    let spawn = |flags: u32| {
        std::process::Command::new(app_exe)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    let base = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    match spawn(base | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(_) => true,
        // 所在 Job 不允许脱离（Chrome / 企业策略 / 沙箱）：退化为随浏览器生命周期。
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
            log("launch: job does not allow breakaway; app will follow the browser's job lifetime");
            spawn(base).is_ok()
        }
        Err(_) => false,
    }
}

/// macOS: launch the agent's helper bundle through Launch Services (`open -g`, as the
/// desktop's service bootstrap does) so the agent is a child of launchd, not of the browser.
///
/// A direct spawn inherits the browser's process-level quarantine (browsers set
/// `LSFileQuarantineEnabled`): every file the agent and its children write gets
/// `com.apple.quarantine` — the update package and the bundle its updater installs included —
/// and a quarantined bundle is App-Translocated on every Launch Services open (lost Launchpad
/// entry, self-update refused). Development layouts and a failing `open` fall back to a direct
/// spawn.
#[cfg(target_os = "macos")]
fn launch_app(app_exe: &Path) -> bool {
    if let Some(helper_app) = bundled_helper_app(app_exe) {
        if let Some(bundle) = outer_app_bundle(&helper_app) {
            release_installed_bundle(&bundle);
        }
        if open_with_launch_services(&helper_app) {
            return true;
        }
    }
    spawn_detached(app_exe)
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn launch_app(app_exe: &Path) -> bool {
    spawn_detached(app_exe)
}

#[cfg(not(windows))]
fn spawn_detached(app_exe: &Path) -> bool {
    std::process::Command::new(app_exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// How long to wait for `open` to hand the launch over to Launch Services.
#[cfg(target_os = "macos")]
const OPEN_TIMEOUT_MS: u64 = 5_000;

/// Run `open -g <helper>`; `false` only when `open` could not run or reported failure.
#[cfg(target_os = "macos")]
fn open_with_launch_services(helper_app: &Path) -> bool {
    let mut child = match std::process::Command::new("/usr/bin/open")
        .arg("-g")
        .arg(helper_app)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            log(&format!(
                "launch: cannot run open ({e}); spawning the agent directly"
            ));
            return false;
        }
    };
    let deadline = Instant::now() + std::time::Duration::from_millis(OPEN_TIMEOUT_MS);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return true,
            Ok(Some(status)) => {
                log(&format!(
                    "launch: open exited with {status}; spawning the agent directly"
                ));
                return false;
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(PIPE_POLL_INTERVAL_MS));
            }
            Ok(None) => {
                // Launch Services is slow but may still start the agent: let the endpoint poll
                // decide, and reap `open` off this thread.
                log("launch: open is still running; waiting for the App endpoint");
                std::thread::spawn(move || {
                    if let Err(e) = child.wait() {
                        log(&format!("launch: waiting for open failed ({e})"));
                    }
                });
                return true;
            }
            Err(e) => {
                log(&format!(
                    "launch: cannot poll open ({e}); waiting for the App endpoint"
                ));
                return true;
            }
        }
    }
}

/// `<Helper>.app/Contents/MacOS/fluxdown-agent` → `<Helper>.app`; development layouts → `None`.
#[cfg(any(target_os = "macos", test))]
fn bundled_helper_app(agent_exe: &Path) -> Option<PathBuf> {
    let macos_dir = agent_exe.parent()?;
    if !macos_dir.ends_with("Contents/MacOS") {
        return None;
    }
    let helper_app = macos_dir.parent()?.parent()?;
    helper_app
        .extension()
        .is_some_and(|ext| ext == "app")
        .then(|| helper_app.to_path_buf())
}

/// Outermost `.app` containing `path` (the agent helper is nested inside `FluxDown.app`).
#[cfg(any(target_os = "macos", test))]
fn outer_app_bundle(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .filter(|ancestor| ancestor.extension().is_some_and(|ext| ext == "app"))
        .last()
        .map(Path::to_path_buf)
}

#[cfg(target_os = "macos")]
const XATTR: &str = "/usr/bin/xattr";
#[cfg(target_os = "macos")]
const QUARANTINE_XATTR: &str = "com.apple.quarantine";
#[cfg(target_os = "macos")]
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// Self-heal an installed bundle that still carries `com.apple.quarantine` before launching
/// from it: older updaters running in the browser's process tree installed such bundles, and
/// Launch Services would App-Translocate them (the `open` that follows included). The relay
/// always runs from the real install path, unlike an agent already running translocated.
/// Same rule as the agent's `update/install/macos.rs::heal_installed_quarantine`; keep both in
/// lockstep.
#[cfg(target_os = "macos")]
fn release_installed_bundle(bundle: &Path) {
    if !is_healable_install(bundle) {
        return;
    }
    let quiet = |command: &mut std::process::Command| {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    };
    let quarantined = quiet(
        std::process::Command::new(XATTR)
            .args(["-p", QUARANTINE_XATTR])
            .arg(bundle),
    )
    .is_ok_and(|status| status.success());
    if !quarantined {
        return;
    }
    // `-s` acts on symlinks themselves (dangling links do not fail the run).
    match quiet(
        std::process::Command::new(XATTR)
            .args(["-s", "-d", "-r", QUARANTINE_XATTR])
            .arg(bundle),
    ) {
        Ok(status) if status.success() => {
            log(&format!("released {} from quarantine", bundle.display()));
        }
        Ok(status) => {
            log(&format!(
                "could not release {} from quarantine: xattr exited with {status}",
                bundle.display()
            ));
            return;
        }
        Err(e) => {
            log(&format!(
                "could not release {} from quarantine: {e}",
                bundle.display()
            ));
            return;
        }
    }
    match quiet(std::process::Command::new(LSREGISTER).arg("-f").arg(bundle)) {
        Ok(status) if status.success() => {}
        Ok(status) => log(&format!("lsregister exited with {status}")),
        Err(e) => log(&format!("could not run lsregister ({e})")),
    }
}

/// Only writable bundles inside an `Applications` folder (any depth, `~/Applications` too):
/// translocated copies and disk images are read-only, and an App run from Downloads keeps the
/// system's quarantine / translocation policy.
#[cfg(any(target_os = "macos", test))]
fn is_healable_install(bundle: &Path) -> bool {
    let text = bundle.to_string_lossy();
    !text.contains("/AppTranslocation/")
        && !text.starts_with("/Volumes/")
        && bundle.parent().is_some_and(|parent| {
            parent
                .ancestors()
                .any(|dir| dir.file_name().is_some_and(|name| name == "Applications"))
        })
}

/// Returns the IPC address string for `pipe::PipeHandle::connect()`.
/// On Windows this is the per-user Named Pipe path (empty when the account name is
/// unavailable, which makes every connect fail); on non-Windows the argument is ignored
/// and the Unix socket path is resolved inside the `pipe` module.
fn ipc_address() -> String {
    #[cfg(windows)]
    {
        std::env::var("USERNAME")
            .ok()
            .and_then(|user| pipe_name_for(&user))
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        String::new()
    }
}

/// Connect errors that only mean "the App is not running" (no endpoint, or a stale Unix socket).
fn app_not_running(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// Log text for a failed connect. `PermissionDenied` means the endpoint exists but this relay may
/// not open it: on Windows the App was started elevated (its pipe then belongs to Administrators).
fn describe_connect_error(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::PermissionDenied {
        format!(
            "{error}; the App is probably running as administrator, quit it from the tray and start it normally"
        )
    } else {
        error.to_string()
    }
}

thread_local! {
    /// Last connect failure written to the log, so a polling popup does not repeat it.
    static LOGGED_CONNECT_ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn log_connect_error(error: &io::Error) {
    let message = format!("ipc connect failed ({})", describe_connect_error(error));
    LOGGED_CONNECT_ERROR.with(|logged| {
        let mut logged = logged.borrow_mut();
        if logged.as_deref() != Some(message.as_str()) {
            log(&message);
            *logged = Some(message);
        }
    });
}

/// One direct connect attempt (never launches the App). Unexpected failures are logged once.
fn connect_direct(addr: &str) -> Result<pipe::PipeHandle, io::Error> {
    let result = pipe::PipeHandle::connect(addr);
    match &result {
        Ok(_) => LOGGED_CONNECT_ERROR.with(|logged| *logged.borrow_mut() = None),
        Err(error) if !app_not_running(error) => log_connect_error(error),
        Err(_) => {}
    }
    result
}

/// Try to connect to the IPC endpoint. If unavailable, launch the App
/// (subject to cooldown) and poll at a fixed 50ms interval until the
/// endpoint appears or the timeout is reached.
///
/// On Windows an endpoint that exists but refuses this relay (`PermissionDenied`) is
/// not "App not running": a named pipe vanishes with its server, so the refusing
/// server is alive (elevated) and launching another App instance cannot help (it
/// finds the running one and exits); fail fast instead of waiting out the launch
/// timeout. On Unix the same error can be a stale root-owned socket that a freshly
/// launched agent replaces, so the launch path stays.
fn connect_with_auto_launch(last_launch: &mut Option<Instant>) -> Option<pipe::PipeHandle> {
    let addr = ipc_address();

    // Fast path: App is already running.
    match connect_direct(&addr) {
        Ok(p) => {
            log("ipc connected (fast path)");
            return Some(p);
        }
        Err(error) if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied => {
            return None;
        }
        Err(_) => {}
    }

    // Cooldown: don't re-launch too quickly (prevents crash-loop).
    if let Some(prev) = last_launch
        && prev.elapsed().as_millis() < APP_LAUNCH_COOLDOWN_MS as u128
    {
        log("launch skipped: cooldown active");
        return None;
    }

    // Find and launch the App.
    let app_exe = match find_app_exe() {
        Some(p) => p,
        None => {
            log("App exe not found");
            return None;
        }
    };

    log(&format!("launching App: {}", app_exe.display()));
    if !launch_app(&app_exe) {
        log("App launch failed (spawn error)");
        return None;
    }
    *last_launch = Some(Instant::now());

    // Poll at a fixed short interval, attempting to connect immediately:
    // a failed connect to a local pipe/socket costs microseconds, while any
    // sleep-first back-off adds its full interval to every cold start.
    let deadline = Instant::now() + std::time::Duration::from_millis(APP_LAUNCH_TIMEOUT_MS);

    let last_error = loop {
        match pipe::PipeHandle::connect(&addr) {
            Ok(p) => {
                let elapsed = last_launch.map_or(0, |t| t.elapsed().as_millis() as u64);
                log(&format!("ipc connected after {}ms", elapsed));
                return Some(p);
            }
            Err(error) if Instant::now() >= deadline => break error,
            Err(_) => {}
        }

        std::thread::sleep(std::time::Duration::from_millis(PIPE_POLL_INTERVAL_MS));
    };

    log(&format!(
        "ipc connect timed out after launch (last error: {})",
        describe_connect_error(&last_error)
    ));
    None
}

/// Reconnect after a write failure and resend the frame once.
///
/// A write failure means the kernel never accepted the frame, so the App
/// cannot have processed it — resending is duplicate-safe. The common cause
/// is a stale pipe handle after the App restarted; without this, the
/// extension pays a full port-teardown → new-NMH → ping → resend round-trip
/// (~0.5-1s) for the first download after every App restart.
///
/// No-launch actions (see [`NO_LAUNCH_ACTIONS`]) reconnect without launching
/// the App (liveness/query checks must not have side effects); everything
/// else goes through auto-launch.
fn reconnect_and_resend(
    raw: &[u8],
    is_no_launch: bool,
    last_launch: &mut Option<Instant>,
) -> Option<pipe::PipeHandle> {
    let mut p = if is_no_launch {
        connect_direct(&ipc_address()).ok()?
    } else {
        connect_with_auto_launch(last_launch)?
    };
    match p.write_message(raw) {
        Ok(()) => {
            log("reconnected and resent after write failure");
            Some(p)
        }
        Err(e) => {
            log(&format!("resend after reconnect failed ({})", e));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

/// Answer for a direct command-line invocation (bare argv or
/// `--help`/`--version`); `None` when launched by a browser.
#[derive(Debug, PartialEq, Eq)]
enum CliAction {
    Version,
    Help,
}

/// Classify a direct command-line invocation (`args` = argv without the
/// program name).
///
/// Package-manager validation pipelines (winget's installer validation runs
/// every installed exe with `--help`/`--version`) and curious humans expect a
/// prompt exit; the relay loop would otherwise block forever reading console
/// stdin. A browser launch always passes at least one positional argument
/// (Chrome/Edge: the extension origin; Firefox: manifest path + extension
/// id; the Linux/macOS wrapper script forwards `"$@"`), so an empty argv is
/// also treated as a human invocation.
fn classify_cli_invocation<S: AsRef<str>>(args: &[S]) -> Option<CliAction> {
    if args
        .iter()
        .any(|a| matches!(a.as_ref(), "--version" | "-V"))
    {
        return Some(CliAction::Version);
    }
    if args.is_empty() || args.iter().any(|a| matches!(a.as_ref(), "--help" | "-h")) {
        return Some(CliAction::Help);
    }
    None
}

fn main() {
    let cli_args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(action) = classify_cli_invocation(&cli_args) {
        match action {
            CliAction::Version => println!("fluxdown_nmh {APP_VERSION}"),
            CliAction::Help => println!(
                "fluxdown_nmh {} — FluxDown Native Messaging Host\n\
                 \n\
                 Relays messages between a browser extension (stdin/stdout)\n\
                 and the FluxDown app (named pipe / unix socket). Launched\n\
                 by the browser via Native Messaging; not meant to be run\n\
                 directly.\n\
                 \n\
                 Options:\n\
                 \x20 -h, --help       Show this help and exit\n\
                 \x20 -V, --version    Show version and exit",
                APP_VERSION
            ),
        }
        return;
    }

    if let Err(error) = relay() {
        log(&format!("NMH stdout failed; stopping relay: {error}"));
        eprintln!("fluxdown_nmh: relay failed: {error}");
        std::process::exit(1);
    }
}

fn relay() -> io::Result<()> {
    log("NMH started");
    let mut pipe: Option<pipe::PipeHandle> = None;
    let mut last_launch: Option<Instant> = None;

    while let Some(raw) = read_stdin_message() {
        let parsed = serde_json::from_slice::<IncomingMessage>(&raw);
        let msg_id = parsed.as_ref().map_or(0, |m| m.msg_id);
        let action = parsed.as_ref().map_or("", |m| m.action.as_str());
        let is_no_launch = is_no_launch_action(action);

        // Ensure IPC connection.
        // No-launch actions only do a direct connect (no App launch for
        // status/query checks).
        if pipe.is_none() {
            pipe = if is_no_launch {
                connect_direct(&ipc_address()).ok()
            } else {
                connect_with_auto_launch(&mut last_launch)
            };
        }

        // "warmup" is answered locally — its only job is to get the App
        // launched and the pipe connected as early as possible. Never
        // forwarded to the App, so the App-side protocol is untouched.
        // NOTE: a cached handle may be stale (App restarted); warmup does
        // not probe it. The next real message self-heals via
        // reconnect_and_resend, so an optimistic "warmed" costs nothing.
        if action == "warmup" {
            if pipe.is_some() {
                respond_status(true, "warmed", msg_id)?;
            } else {
                respond_status(false, "app_not_running", msg_id)?;
            }
            continue;
        }

        // Take the handle out; it is put back only if this message's
        // write/read round-trip proves the connection healthy.
        let mut p = match pipe.take() {
            Some(p) => p,
            None => {
                respond_status(false, "app_not_running", msg_id)?;
                continue;
            }
        };

        // Forward message to App. On write failure, reconnect and resend
        // once in-process instead of bouncing the error to the extension.
        if let Err(e) = p.write_message(&raw) {
            log(&format!("pipe write failed ({}), reconnecting", e));
            drop(p);
            match reconnect_and_resend(&raw, is_no_launch, &mut last_launch) {
                Some(fresh) => p = fresh,
                None => {
                    respond_status(false, "app_not_running", msg_id)?;
                    continue;
                }
            }
        }

        // Read response from App.
        match p.read_message() {
            Ok(response_data) => {
                write_stdout_message(&response_data)?;
                pipe = Some(p);
            }
            Err(e) => {
                log(&format!("pipe read failed ({}), dropping connection", e));
                respond_status(false, "app_not_running", msg_id)?;
            }
        }
    }

    log("NMH exiting (stdin closed)");
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn no_launch_set_covers_ping_and_task_panel_actions() {
        for action in [
            "ping",
            "tasks",
            "task_op",
            "open_file",
            "reveal_file",
            "capabilities",
            "capture_status",
            "sniff_rules",
            "page_scripts",
            "page_script_source",
            "plugin_invoke",
        ] {
            assert!(is_no_launch_action(action), "{action} should be no-launch");
        }
    }

    #[test]
    fn no_launch_set_excludes_launch_triggering_actions() {
        for action in ["download", "warmup", "unknown", ""] {
            assert!(
                !is_no_launch_action(action),
                "{action} must still auto-launch the App"
            );
        }
    }

    /// Must stay in lockstep with `native/agent/src/nmh.rs` (`socket_path_under`): the agent
    /// and this relay derive the endpoint independently, both tests pin the same literals.
    #[test]
    fn unix_socket_lives_in_a_private_per_user_ipc_dir() {
        #[cfg(target_os = "macos")]
        let expected = "/Users/alice/Library/Application Support/fluxdown/ipc/fluxdown.sock";
        #[cfg(not(target_os = "macos"))]
        let expected = "/home/alice/.local/share/fluxdown/ipc/fluxdown.sock";
        let home = if cfg!(target_os = "macos") {
            "/Users/alice"
        } else {
            "/home/alice"
        };
        assert_eq!(
            socket_path_under(Path::new(home)),
            Path::new(expected).to_path_buf()
        );
    }

    /// Same literals as `native/agent/src/nmh.rs` (`pipe_name_for`).
    #[test]
    fn pipe_name_is_per_user_and_injective() {
        assert_eq!(
            pipe_name_for("Alice Smith").as_deref(),
            Some(r"\\.\pipe\fluxdown-alice_20smith")
        );
        assert_eq!(
            pipe_name_for("a_b"),
            Some(r"\\.\pipe\fluxdown-a_5fb".to_owned())
        );
        assert_eq!(
            pipe_name_for("a b"),
            Some(r"\\.\pipe\fluxdown-a_20b".to_owned())
        );
        assert_eq!(
            pipe_name_for("张三").as_deref(),
            Some(r"\\.\pipe\fluxdown-_e5_bc_a0_e4_b8_89")
        );
        assert_eq!(pipe_name_for(""), None);
        assert_ne!(pipe_name_for("alice"), pipe_name_for("bob"));
        // Windows account names are case-insensitive.
        assert_eq!(pipe_name_for("ALICE"), pipe_name_for("alice"));
    }

    #[test]
    fn cli_invocation_exits_for_bare_help_and_version() {
        assert_eq!(classify_cli_invocation::<&str>(&[]), Some(CliAction::Help));
        assert_eq!(classify_cli_invocation(&["--help"]), Some(CliAction::Help));
        assert_eq!(classify_cli_invocation(&["-h"]), Some(CliAction::Help));
        assert_eq!(
            classify_cli_invocation(&["--version"]),
            Some(CliAction::Version)
        );
        assert_eq!(classify_cli_invocation(&["-V"]), Some(CliAction::Version));
    }

    #[test]
    fn cli_invocation_falls_through_for_browser_launch_args() {
        // Chrome/Edge pass the extension origin (plus --parent-window on
        // Windows); Firefox passes the manifest path and extension id. All
        // must reach the relay loop.
        assert_eq!(
            classify_cli_invocation(&["chrome-extension://meleenglfggcmcajknpeeeiobnpfmahc/"]),
            None
        );
        assert_eq!(
            classify_cli_invocation(&[
                "chrome-extension://meleenglfggcmcajknpeeeiobnpfmahc/",
                "--parent-window=12345"
            ]),
            None
        );
        assert_eq!(
            classify_cli_invocation(&["/path/to/com.fluxdown.nmh.json", "fluxdown@zerx.dev"]),
            None
        );
    }

    #[test]
    fn macos_launch_targets_the_helper_bundle_of_a_packaged_agent() {
        let agent = Path::new(
            "/Applications/FluxDown.app/Contents/Helpers/FluxDownAgent.app/Contents/MacOS/fluxdown-agent",
        );
        let helper = bundled_helper_app(agent);
        assert_eq!(
            helper.as_deref(),
            Some(Path::new(
                "/Applications/FluxDown.app/Contents/Helpers/FluxDownAgent.app"
            ))
        );
        assert_eq!(
            helper.as_deref().and_then(outer_app_bundle).as_deref(),
            Some(Path::new("/Applications/FluxDown.app"))
        );
        // Development layouts have no helper bundle: the relay spawns the agent directly.
        assert_eq!(
            bundled_helper_app(Path::new("/src/FluxDown/target/debug/fluxdown-agent")),
            None
        );
        assert_eq!(
            bundled_helper_app(Path::new("/opt/Contents/MacOS/fluxdown-agent")),
            None
        );
    }

    /// Mirrors the agent's `healable_install_location` rule.
    #[test]
    fn quarantine_heal_only_touches_writable_applications_installs() {
        for healable in [
            "/Applications/FluxDown.app",
            "/Applications/Utilities/FluxDown.app",
            "/Users/me/Applications/FluxDown.app",
        ] {
            assert!(is_healable_install(Path::new(healable)), "{healable}");
        }
        for kept in [
            "/Users/me/Downloads/FluxDown.app",
            "/Users/me/Applications.backup/FluxDown.app",
            "/private/var/folders/xx/T/AppTranslocation/ABC/d/FluxDown.app",
            "/Volumes/FluxDown 1.0.0/FluxDown.app",
            "/Volumes/External/Applications/FluxDown.app",
        ] {
            assert!(!is_healable_install(Path::new(kept)), "{kept}");
        }
    }
    #[cfg(test)]
    mod frame_failure_tests {
        use super::*;

        struct FailingWriter {
            fail_at: usize,
            operations: usize,
            bytes: Vec<u8>,
        }

        impl Write for FailingWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.operations += 1;
                if self.operations == self.fail_at {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.operations += 1;
                if self.operations == self.fail_at {
                    Err(io::ErrorKind::BrokenPipe.into())
                } else {
                    Ok(())
                }
            }
        }

        #[test]
        fn frame_stops_at_header_payload_or_flush_failure() {
            for fail_at in 1..=3 {
                let mut writer = FailingWriter {
                    fail_at,
                    operations: 0,
                    bytes: Vec::new(),
                };
                let error = write_message_frame(&mut writer, b"{}").expect_err("frame must fail");
                assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
                assert_eq!(writer.operations, fail_at, "no operation after failure");
                let expected: &[u8] = match fail_at {
                    1 => b"",
                    2 => &[2, 0, 0, 0],
                    _ => &[2, 0, 0, 0, b'{', b'}'],
                };
                assert_eq!(writer.bytes, expected);
            }
        }
    }
}
