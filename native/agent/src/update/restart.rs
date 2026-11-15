//! 重启计划：登记后，agent 运行期**完全结束**（daemon 已退出、`agent.lock` 已释放）再由
//! `main` 执行，然后进程退出。
//!
//! 计划只描述「怎么把程序重新拉起来」，不做任何文件替换（替换在 [`super::install`] 里完成）：
//! - 更新后的桌面各形态：分离启动桌面程序并带 `--after-update`（它等旧实例退出后正常启动，
//!   并提示已更新）；
//! - 托盘「重启」（[`desktop_relaunch`]）：同样分离启动桌面程序，但带 `--relaunch`
//!   （等旧实例退出后正常启动，不提示已更新）；
//! - Windows 安装版：静默运行安装器，安装器结束时自行带 `--after-update` 拉起桌面；
//! - headless 服务器：Unix `exec` 同一路径与原 argv（PID 不变，systemd / tini / 终端照常工作），
//!   Windows 分离启动同一路径。

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

/// 桌面程序在更新后重启时带的参数（只有桌面平台有桌面程序）。
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(crate) const AFTER_UPDATE_ARG: &str = "--after-update";

/// 托盘「重启」后桌面程序带的参数：与 [`AFTER_UPDATE_ARG`] 一样等旧实例释放单实例锁，
/// 但没有更新发生，不提示已更新。
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(crate) const RELAUNCH_ARG: &str = "--relaunch";

/// Agent-only relaunch after an opted-in idle update; never opens a desktop window.
pub const BACKGROUND_UPDATE_ARG: &str = "--background-update";

/// macOS：经 LaunchServices 拉起整个 bundle（签名与权限归属正确，与更新重启一致）。
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
const OPEN: &str = "/usr/bin/open";

static PENDING: Mutex<Option<RestartPlan>> = Mutex::new(None);

/// 一次待执行的重启。
#[derive(Debug)]
pub struct RestartPlan {
    action: Action,
    /// 是否由 headless 服务器自身重启（决定 UI 连接以 `service-restart` 关闭）。
    server: bool,
}

#[derive(Debug)]
enum Action {
    /// 分离启动一个进程（不等待）：桌面平台的桌面重启，及非 Unix 的服务器重启。
    #[cfg(any(not(unix), target_os = "linux", target_os = "macos"))]
    Spawn {
        program: PathBuf,
        args: Vec<OsString>,
    },
    /// 以同一进程原地替换为新程序（Unix 服务器）。
    #[cfg(unix)]
    Exec {
        program: PathBuf,
        args: Vec<OsString>,
    },
    /// 静默运行 Inno Setup 安装器（Windows 安装版）。
    #[cfg(windows)]
    Installer {
        installer: PathBuf,
        log: PathBuf,
        background: bool,
    },
}

impl RestartPlan {
    /// 桌面形态：分离启动 `program`。
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    pub(crate) fn desktop(program: PathBuf, args: Vec<OsString>) -> Self {
        Self {
            action: Action::Spawn { program, args },
            server: false,
        }
    }

    /// headless 服务器：Unix `exec` 原路径与原参数，Windows 分离启动。
    pub(crate) fn server(program: PathBuf, args: Vec<OsString>) -> Self {
        #[cfg(unix)]
        let action = Action::Exec { program, args };
        #[cfg(not(unix))]
        let action = Action::Spawn { program, args };
        Self {
            action,
            server: true,
        }
    }

    /// Windows 安装版：退出后静默运行 `installer`，日志写到 `log`。
    #[cfg(windows)]
    pub(crate) fn installer(installer: PathBuf, log: PathBuf) -> Self {
        Self {
            action: Action::Installer {
                installer,
                log,
                background: false,
            },
            server: false,
        }
    }

    pub(super) fn in_background(self, background_agent: Self) -> Self {
        match self.action {
            #[cfg(windows)]
            Action::Installer { installer, log, .. } => Self {
                action: Action::Installer {
                    installer,
                    log,
                    background: true,
                },
                server: false,
            },
            #[cfg(any(not(unix), target_os = "linux", target_os = "macos"))]
            Action::Spawn { .. } => background_agent,
            #[cfg(unix)]
            Action::Exec { .. } => background_agent,
        }
    }

    fn run(self) -> std::io::Result<()> {
        match self.action {
            #[cfg(any(not(unix), target_os = "linux", target_os = "macos"))]
            Action::Spawn { program, args } => {
                tracing::info!(program = %program.display(), "starting program after exit");
                spawn_detached(&program, &args)
            }
            #[cfg(unix)]
            Action::Exec { program, args } => {
                use std::os::unix::process::CommandExt;

                tracing::info!(program = %program.display(), "re-executing agent after update");
                // `exec` 成功时不返回；只会带着失败原因回来。
                Err(std::process::Command::new(&program).args(&args).exec())
            }
            #[cfg(windows)]
            Action::Installer {
                installer,
                log,
                background,
            } => run_installer(&installer, &log, background),
        }
    }
}

/// Resolve a UI-free restart before the running files are renamed by installation.
pub(super) fn background_agent() -> std::io::Result<RestartPlan> {
    let agent = std::env::current_exe()?;
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("APPIMAGE").filter(|path| !path.is_empty()) {
        return Ok(RestartPlan::desktop(
            PathBuf::from(appimage),
            vec![
                crate::platform::AUTOSTART_ARG.into(),
                BACKGROUND_UPDATE_ARG.into(),
            ],
        ));
    }
    #[cfg(target_os = "macos")]
    if let Some(helper) = agent
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .filter(|path| path.extension().is_some_and(|ext| ext == "app"))
    {
        return Ok(RestartPlan::desktop(
            OPEN.into(),
            vec![
                "-g".into(),
                "-n".into(),
                helper.as_os_str().to_owned(),
                "--args".into(),
                BACKGROUND_UPDATE_ARG.into(),
            ],
        ));
    }
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    return Ok(RestartPlan::desktop(
        agent,
        vec![BACKGROUND_UPDATE_ARG.into()],
    ));
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    Ok(RestartPlan::server(
        agent,
        vec![BACKGROUND_UPDATE_ARG.into()],
    ))
}

/// 登记重启计划；agent 运行期完全结束后由 [`run_pending`] 执行。后登记的覆盖先登记的。
pub(crate) fn schedule(plan: RestartPlan) {
    *PENDING.lock().unwrap_or_else(PoisonError::into_inner) = Some(plan);
}

/// 已登记的计划是否是 headless 服务器自身重启。
pub(crate) fn scheduled_server_restart() -> bool {
    PENDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|plan| plan.server)
}

/// 托盘「重启」的计划：桌面程序会再拉起 agent 与 daemon。找不到桌面程序时退而只重启 agent
/// 自身（原参数、同一路径），保证用户点了「重启」后仍然回到运行状态；连 agent 自己的路径都
/// 取不到则返回 `None`，调用方保持当前运行不动。
#[must_use]
pub(crate) fn desktop_relaunch() -> Option<RestartPlan> {
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    {
        let (program, args) = relaunch_command(RelaunchFacts::gather())?;
        Some(RestartPlan::desktop(program, args))
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// 选择重启命令所依据的进程事实。
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
struct RelaunchFacts {
    /// Linux：`$APPIMAGE` 指向的存在的文件（运行期的挂载点在进程退出后失效，必须从文件本身重启）。
    appimage: Option<PathBuf>,
    /// macOS：agent 所在的外层 `<Host>.app`。
    host_bundle: Option<PathBuf>,
    /// 官方桌面程序（存在才有值）。
    desktop: Option<PathBuf>,
    /// agent 自身的可执行文件与启动参数（桌面程序缺失时的兜底）。
    agent: Option<PathBuf>,
    agent_args: Vec<OsString>,
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
impl RelaunchFacts {
    fn gather() -> Self {
        #[cfg(target_os = "linux")]
        let appimage = std::env::var_os("APPIMAGE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_file());
        #[cfg(not(target_os = "linux"))]
        let appimage = None;
        #[cfg(target_os = "macos")]
        let host_bundle = crate::platform::host_app_bundle();
        #[cfg(not(target_os = "macos"))]
        let host_bundle = None;
        Self {
            appimage,
            host_bundle,
            desktop: crate::platform::desktop_executable(),
            agent: std::env::current_exe().ok(),
            agent_args: std::env::args_os().skip(1).collect(),
        }
    }
}

/// 重启命令的选择顺序：AppImage 文件 → macOS 外层 bundle（`open -n`）→ 同级桌面程序 →
/// agent 自身。
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn relaunch_command(facts: RelaunchFacts) -> Option<(PathBuf, Vec<OsString>)> {
    if let Some(appimage) = facts.appimage {
        return Some((appimage, vec![RELAUNCH_ARG.into()]));
    }
    if let Some(bundle) = facts.host_bundle
        && facts.desktop.is_some()
    {
        return Some((
            PathBuf::from(OPEN),
            vec![
                "-n".into(),
                bundle.into_os_string(),
                "--args".into(),
                RELAUNCH_ARG.into(),
            ],
        ));
    }
    if let Some(desktop) = facts.desktop {
        return Some((desktop, vec![RELAUNCH_ARG.into()]));
    }
    tracing::warn!("fluxdown-desktop not found; restarting the agent on its own");
    facts.agent.map(|agent| (agent, facts.agent_args))
}

/// `main` / 宿主在 agent 运行期结束、进程退出前调用；没有计划时 `Ok(())`。
///
/// Unix 服务器计划成功时不会返回（进程已被新程序替换）。
///
/// # Errors
/// 启动 / `exec` 失败的 I/O 错误；调用方记录日志即可，进程随后照常退出。
pub fn run_pending() -> std::io::Result<()> {
    let plan = PENDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    match plan {
        Some(plan) => plan.run(),
        None => Ok(()),
    }
}

/// 分离启动：标准流置空，Unix 进入独立进程组，Windows 不带控制台窗口、独立进程组。
#[cfg(any(not(unix), target_os = "linux", target_os = "macos"))]
fn spawn_detached(program: &std::path::Path, args: &[OsString]) -> std::io::Result<()> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);
    // 子进程由系统接管：本进程随后退出，不等待。
    command.spawn().map(drop)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn detach(_command: &mut std::process::Command) {}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// `ERROR_ELEVATION_REQUIRED`：安装器清单要求提权，而当前进程未提权。
#[cfg(windows)]
const ERROR_ELEVATION_REQUIRED: i32 = 740;

/// 静默运行安装器。安装器清单要求提权（os error 740）时用 `__COMPAT_LAYER=RunAsInvoker`
/// 重试：安装器是每用户安装，不需要管理员权限。
#[cfg(windows)]
fn run_installer(
    installer: &std::path::Path,
    log: &std::path::Path,
    background: bool,
) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;

    let launch = |run_as_invoker: bool| {
        let mut command = std::process::Command::new(installer);
        command
            .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"])
            // 路径含空格时 Inno Setup 要求 `/LOG="<path>"` 的形式。
            .raw_arg(format!("/LOG=\"{}\"", log.display()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        if background {
            command.args([
                "/BACKGROUNDUPDATE=1",
                "/NOCLOSEAPPLICATIONS",
                "/NORESTARTAPPLICATIONS",
            ]);
        }
        if run_as_invoker {
            command.env("__COMPAT_LAYER", "RunAsInvoker");
        }
        command.spawn().map(drop)
    };
    tracing::info!(installer = %installer.display(), "running installer after update");
    match launch(false) {
        Err(error) if error.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
            tracing::info!("installer requested elevation; retrying with RunAsInvoker");
            launch(true)
        }
        other => other,
    }
}

#[cfg(all(test, any(windows, target_os = "linux", target_os = "macos")))]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{
        OPEN, RELAUNCH_ARG, RelaunchFacts, RestartPlan, relaunch_command, run_pending, schedule,
        scheduled_server_restart,
    };

    fn facts() -> RelaunchFacts {
        RelaunchFacts {
            appimage: None,
            host_bundle: None,
            desktop: None,
            agent: Some(PathBuf::from("/opt/fluxdown/fluxdown-agent")),
            agent_args: vec![OsString::from("--autostart")],
        }
    }

    #[test]
    fn relaunch_prefers_appimage_then_bundle_then_desktop_then_agent() {
        let relaunch = vec![OsString::from(RELAUNCH_ARG)];
        let desktop = PathBuf::from("/opt/fluxdown/fluxdown-desktop");

        let (program, args) = relaunch_command(RelaunchFacts {
            appimage: Some(PathBuf::from("/home/u/FluxDown.AppImage")),
            host_bundle: Some(PathBuf::from("/Applications/FluxDown.app")),
            desktop: Some(desktop.clone()),
            ..facts()
        })
        .expect("appimage plan");
        assert_eq!(program, PathBuf::from("/home/u/FluxDown.AppImage"));
        assert_eq!(args, relaunch);

        let (program, args) = relaunch_command(RelaunchFacts {
            host_bundle: Some(PathBuf::from("/Applications/FluxDown.app")),
            desktop: Some(desktop.clone()),
            ..facts()
        })
        .expect("bundle plan");
        assert_eq!(program, PathBuf::from(OPEN));
        assert_eq!(
            args,
            ["-n", "/Applications/FluxDown.app", "--args", RELAUNCH_ARG].map(OsString::from)
        );

        let (program, args) = relaunch_command(RelaunchFacts {
            desktop: Some(desktop.clone()),
            ..facts()
        })
        .expect("desktop plan");
        assert_eq!(program, desktop);
        assert_eq!(args, relaunch);

        // 没有桌面程序：只重启 agent，沿用原参数，且不带 `--relaunch`。
        let (program, args) = relaunch_command(facts()).expect("agent plan");
        assert_eq!(program, PathBuf::from("/opt/fluxdown/fluxdown-agent"));
        assert_eq!(args, [OsString::from("--autostart")]);

        // 连 agent 路径都未知：不登记计划。
        assert!(
            relaunch_command(RelaunchFacts {
                agent: None,
                ..facts()
            })
            .is_none()
        );
    }

    /// 全局槽位被多个断言共享，合并成一个测试避免并行互相覆盖。
    #[test]
    fn pending_plan_is_taken_once_and_reports_its_scope() {
        assert!(run_pending().is_ok(), "no plan is a no-op");
        assert!(!scheduled_server_restart());

        let missing = std::env::temp_dir().join(format!(
            "fluxdown-restart-missing-{}",
            uuid::Uuid::new_v4().simple()
        ));
        schedule(RestartPlan::desktop(missing.clone(), Vec::new()));
        assert!(!scheduled_server_restart());
        schedule(RestartPlan::server(
            PathBuf::from("/definitely/not/here"),
            vec![OsString::from("--server")],
        ));
        assert!(
            scheduled_server_restart(),
            "later plans replace earlier ones"
        );

        // 启动失败如实返回，且计划已被取走。
        schedule(RestartPlan::desktop(missing, Vec::new()));
        let error = run_pending().expect_err("missing program must fail to start");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(run_pending().is_ok());
        assert!(!scheduled_server_restart());
    }
}
