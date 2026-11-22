//! Windows portable installation runs from staging, not from an image it will replace.

use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[path = "../../../portable_process.rs"]
mod process;

use super::install::{self, InstallError};
use super::restart::{self, RestartPlan};
use super::{PENDING_FILE, PendingRecord, PortableFailure};

const WORKER_ARG: &str = "--apply-portable-update";
const AGENT: &str = "fluxdown-agent.exe";
const DESKTOP: &str = "fluxdown-desktop.exe";
const DAEMON: &str = "fluxdownd.exe";
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const SIGNAL: [u8; 1] = [1];

#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum CommitMode {
    Desktop = 1,
    Background = 2,
}

impl CommitMode {
    fn read(input: &mut impl Read) -> io::Result<Self> {
        let mut byte = [0];
        input.read_exact(&mut byte)?;
        match byte[0] {
            1 => Ok(Self::Desktop),
            2 => Ok(Self::Background),
            _ => Err(io::Error::other("invalid portable update commit mode")),
        }
    }

    fn relaunch(self, desktop_arg: &str) -> (&str, &str) {
        match self {
            Self::Desktop => (DESKTOP, desktop_arg),
            Self::Background => (AGENT, restart::BACKGROUND_UPDATE_ARG),
        }
    }
}

/// Register a running Windows desktop-stack process with the portable installer.
/// Returns false during installation. Installed builds have no lease.
///
/// # Errors
/// Returns an I/O error if the portable process lease cannot be acquired.
pub fn admit() -> io::Result<bool> {
    process::admit()
}

/// Handle the dedicated one-shot worker launch before starting the normal agent runtime.
///
/// # Errors
/// Returns the worker's I/O error for logging, or `None` for a normal launch.
pub fn run_worker() -> Option<io::Result<()>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(WORKER_ARG)) {
        return None;
    }
    let result = match (args.next(), args.next(), args.next(), args.next()) {
        (Some(dest), Some(root), Some(pending), None) => {
            apply(Path::new(&dest), Path::new(&root), Path::new(&pending))
        }
        _ => Err(io::Error::other(
            "malformed portable update worker invocation",
        )),
    };
    if let Err(error) = &result {
        eprintln!("portable update worker: {error:#}");
    }
    Some(result)
}

pub(super) fn prepare(package: &Path, work: &Path) -> Result<RestartPlan, InstallError> {
    fn storage(source: io::Error) -> InstallError {
        InstallError::storage("prepare Windows portable update worker")(source)
    }
    let agent = std::env::current_exe().map_err(storage)?;
    let dest = parent(&agent)
        .map_err(storage)?
        .canonicalize()
        .map_err(storage)?;
    let work = work.canonicalize().map_err(storage)?;
    if work
        .strip_prefix(&dest)
        .is_ok_and(|relative| !relative.starts_with(process::DATA_DIR))
    {
        return Err(storage(io::Error::other(
            "update staging overlaps the portable installation",
        )));
    }
    let root = install::extract_portable(package, &work)?;
    for name in [AGENT, DESKTOP, DAEMON] {
        if !root.join(name).is_file() {
            return Err(InstallError::InvalidPackage(format!(
                "package does not contain {name}"
            )));
        }
    }
    let worker_dir = work.join("worker");
    fs::create_dir(&worker_dir).map_err(storage)?;
    let worker = worker_dir.join(AGENT);
    fs::copy(&agent, &worker).map_err(storage)?;
    copy_runtime(&dest, &worker_dir).map_err(storage)?;
    let pending = parent(parent(&work).map_err(storage)?)
        .map_err(storage)?
        .join(PENDING_FILE);
    start_worker(&worker, &dest, &root, &pending, &work).map_err(storage)
}

fn parent(path: &Path) -> io::Result<&Path> {
    path.parent()
        .ok_or_else(|| io::Error::other("missing parent directory"))
}

// Release builds ship the VC runtime next to the executable; the isolated copy needs it too.
fn copy_runtime(dest: &Path, worker: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dest)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| {
            name.ends_with(".dll")
                && (name.starts_with("vcruntime140") || name.starts_with("msvcp140"))
        }) {
            fs::copy(entry.path(), worker.join(name))?;
        }
    }
    Ok(())
}

fn start_worker(
    worker: &Path,
    dest: &Path,
    root: &Path,
    pending: &Path,
    work: &Path,
) -> io::Result<RestartPlan> {
    let mut command = Command::new(worker);
    command
        .arg(WORKER_ARG)
        .args([dest, root, pending])
        .current_dir(parent(worker)?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(File::create(work.join("portable-update.log"))?);
    restart::detach(&mut command);
    let mut child = command.spawn()?;
    if let Err(error) = wait_ready(&mut child) {
        if child.try_wait()?.is_none() {
            child.kill()?;
            child.wait()?;
        }
        return Err(error);
    }
    let commit = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing worker commit pipe"))?;
    Ok(RestartPlan::portable(commit, dest.join(DESKTOP)))
}

fn wait_ready(child: &mut Child) -> io::Result<()> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing worker ready pipe"))?;
    let (tx, rx) = mpsc::sync_channel(1);
    thread::Builder::new().spawn(move || {
        let result = read_signal(&mut stdout);
        if tx.send(result).is_err() {
            tracing::debug!("portable update readiness receiver already closed");
        }
    })?;
    rx.recv_timeout(READY_TIMEOUT).map_err(io::Error::other)?
}

fn read_signal(input: &mut impl Read) -> io::Result<()> {
    let mut byte = [0];
    input.read_exact(&mut byte)?;
    if byte == SIGNAL {
        Ok(())
    } else {
        Err(io::Error::other("invalid portable update handshake"))
    }
}

fn apply(dest: &Path, root: &Path, pending: &Path) -> io::Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let work = parent(parent(&exe)?)?;
    if exe != work.join("worker").join(AGENT)
        || !root.starts_with(work.join("extract"))
        || pending != parent(parent(work)?)?.join(PENDING_FILE)
        || !dest.join("portable").is_file()
    {
        return Err(io::Error::other(
            "portable update worker started outside its staging layout",
        ));
    }
    let data = dest.join(process::DATA_DIR);
    let intent = process::lease(&data, process::INTENT)?;
    wait_for_lock(&intent, Duration::from_secs(3)).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("acquire portable update startup gate: {error}"),
        )
    })?;
    let running = process::lease(&data, process::RUNNING)?;
    let mut record: PendingRecord = serde_json::from_slice(&fs::read(pending)?)?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&SIGNAL)?;
    stdout.flush()?;
    drop(stdout);
    // EOF, including a crashed parent, is not authorization to touch the installation.
    let mode = CommitMode::read(&mut io::stdin().lock())?;

    let result = match wait_for_exit(&running) {
        Ok(()) => {
            // Preserve backups if the worker dies after the first rename but before rollback.
            record.portable_failure = Some(PortableFailure {
                detail: "Portable update was interrupted during replacement; keep the backups and restore the installation manually.".to_owned(),
                rollback_incomplete: true,
            });
            write_record(pending, &record)?;
            install::replace_portable(root, dest, &[AGENT, DESKTOP, DAEMON])
        }
        Err(error) => Err(error),
    };
    let relaunch = match result {
        Ok(()) => {
            record.portable_failure = None;
            if let Err(error) = write_record(pending, &record) {
                eprintln!(
                    "portable files replaced, but the recovery record could not be cleared: {error}"
                );
            }
            restart::AFTER_UPDATE_ARG
        }
        Err(error) => {
            let rollback_incomplete = matches!(
                error,
                InstallError::Replace {
                    rollback_incomplete: true,
                    ..
                }
            );
            let detail = error.to_string();
            eprintln!("portable update failed: {detail}");
            record.portable_failure = Some(PortableFailure {
                detail,
                rollback_incomplete,
            });
            if let Err(error) = write_record(pending, &record) {
                eprintln!("could not record portable update failure: {error}");
            }
            if rollback_incomplete {
                // The old application may no longer be usable. Keep its backups and do not start it.
                return Err(io::Error::other(error));
            }
            restart::RELAUNCH_ARG
        }
    };
    drop(running);
    drop(intent);
    let (program, arg) = mode.relaunch(relaunch);
    let mut command = Command::new(dest.join(program));
    command
        .arg(arg)
        .current_dir(dest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    restart::detach(&mut command);
    command.spawn().map(drop)
}

fn write_record(pending: &Path, record: &PendingRecord) -> io::Result<()> {
    let temporary = pending.with_extension("worker.tmp");
    fs::write(&temporary, serde_json::to_vec(record)?)?;
    fs::rename(temporary, pending)
}

fn wait_for_exit(running: &File) -> Result<(), InstallError> {
    wait_for_lock(running, DRAIN_TIMEOUT).map_err(|error| {
        InstallError::Task(format!(
            "wait for FluxDown desktop, agent or daemon to exit: {error}"
        ))
    })
}

fn wait_for_lock(lease: &File, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match lease.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "lock is still held",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}
