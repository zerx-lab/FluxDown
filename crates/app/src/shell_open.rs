//! Windows：打开文件 / 文件夹由前台界面进程拉起一次性 agent 执行（参数与退出码契约见
//! [`fluxdown_protocol::shell_open`]）。
//!
//! Windows 的前台授权受调用进程及其启动者影响：后台常驻 agent 发起的打开可能落在其他窗口
//! 下面（#824）。打开逻辑（产物 / 临时文件 / 保存目录定位、第三方文件管理器探测、无关联
//! 文件的回退）仍只在 agent 一处实现，这里由界面进程拉起短命 agent，利用用户点击时的前台
//! 资格发起打开。调用方据结果在窗口里报错；进度 / 完成窗口不等结果，对方接管前台即关窗
//! （见 `windows/progress.rs`）。系统是否实际置前仍取决于 Windows 的焦点策略。

use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use fluxdown_protocol::shell_open::{
    EXIT_NOT_FOUND, EXIT_OK, EXIT_UNSUPPORTED, OPEN_DIR_ARG, OPEN_TASK_ARG, REVEAL_TASK_ARG,
};
use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, TaskDto, method};

use crate::agent_client::AgentClient;

/// `CREATE_NO_WINDOW`：开发构建的 agent 是控制台子系统，不能闪控制台窗口。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 打开（`reveal = false`）或在文件管理器中定位（`reveal = true`）任务产物。先取 daemon 的
/// 最新任务记录：保存目录 / 文件名可能刚被改名或换源，不能用界面快照里的旧值。
pub(crate) async fn task(
    client: &AgentClient,
    task_id: String,
    reveal: bool,
) -> Result<(), RpcErrorData> {
    let task: TaskDto = client
        .call(
            method::DAEMON_TASK_GET,
            Some(serde_json::json!({ "taskId": task_id })),
        )
        .await?;
    let flag = if reveal {
        REVEAL_TASK_ARG
    } else {
        OPEN_TASK_ARG
    };
    run([flag.to_owned(), task.save_dir, task.file_name]).await
}

/// Reveal a file at the exact path resolved by the daemon, not a BT metadata path.
pub(crate) async fn reveal_file(path: &str) -> Result<(), RpcErrorData> {
    let path = Path::new(path);
    let (Some(dir), Some(name)) = (
        path.parent().and_then(Path::to_str),
        path.file_name().and_then(|name| name.to_str()),
    ) else {
        return Err(RpcErrorData::new(ApplicationErrorCode::NotFound, false));
    };
    run([REVEAL_TASK_ARG.to_owned(), dir.to_owned(), name.to_owned()]).await
}

/// 用系统默认的目录处理程序打开已存在的目录。
pub(crate) async fn directory(path: String) -> Result<(), RpcErrorData> {
    run([OPEN_DIR_ARG.to_owned(), path]).await
}

async fn run<const N: usize>(args: [String; N]) -> Result<(), RpcErrorData> {
    let program = crate::service_bootstrap::agent_executable().map_err(|error| {
        log::warn!("could not locate fluxdown-agent for shell open: {error}");
        RpcErrorData::new(ApplicationErrorCode::Unavailable, false)
    })?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    // 等待子进程只占一个短命线程，不阻塞 UI 线程或 agent 会话运行时；结果经 oneshot 回到
    // 任意执行器上的调用方。
    let (done, exited) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("fluxdown-shell-open".to_owned())
        .spawn(move || {
            if done.send(command.status()).is_err() {
                log::debug!("shell-open caller dropped before fluxdown-agent exited");
            }
        })
        .map_err(|error| {
            log::warn!("could not start the shell-open waiter: {error}");
            RpcErrorData::new(ApplicationErrorCode::Internal, false)
        })?;
    let status = exited
        .await
        .map_err(|_| RpcErrorData::new(ApplicationErrorCode::Internal, false))?
        .map_err(|error| {
            log::warn!("could not spawn fluxdown-agent for shell open: {error}");
            RpcErrorData::new(ApplicationErrorCode::Unavailable, false)
        })?;
    outcome(status)
}

fn outcome(status: ExitStatus) -> Result<(), RpcErrorData> {
    let code = match status.code() {
        Some(EXIT_OK) => return Ok(()),
        Some(EXIT_NOT_FOUND) => ApplicationErrorCode::NotFound,
        Some(EXIT_UNSUPPORTED) => ApplicationErrorCode::Unsupported,
        _ => {
            log::warn!("fluxdown-agent shell open failed: {status}");
            ApplicationErrorCode::Internal
        }
    };
    Err(RpcErrorData::new(code, false))
}
