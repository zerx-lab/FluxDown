//! `fluxdown-agent` 官方客户端常驻后端。
// 发布构建由开机自启直接拉起：GUI 子系统，登录时不弹控制台窗口。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

/// mimalloc 全局分配器：多线程 tokio 下吞吐与内存碎片均优于 musl/glibc 默认分配器。
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> fluxdown_agent::runtime::AgentResult {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // 桌面界面进程为前台打开文件 / 文件夹拉起的一次性进程：不初始化日志 / 运行时，执行后即退出。
    if let Some(code) = fluxdown_agent::platform::run_shell_open(&args) {
        std::process::exit(code);
    }
    #[cfg(windows)]
    if let Some(result) = fluxdown_agent::notification::handle_activation(&args) {
        fluxdown_agent::logging::init_desktop();
        let result = result.map_err(|error| -> Box<dyn std::error::Error + Send + Sync> {
            std::io::Error::other(error).into()
        });
        fluxdown_agent::logging::finish(&result);
        return result;
    }
    // 以管理员身份运行的安装器经这个一次性模式拉起桌面（见 platform/elevation.rs）。
    #[cfg(windows)]
    if let Some((flag, desktop_args)) = args.split_first()
        && flag == fluxdown_agent::platform::elevation::LAUNCH_DESKTOP_ARG
    {
        fluxdown_agent::logging::init_desktop();
        let result = fluxdown_agent::platform::elevation::launch_desktop(desktop_args).map_err(
            |error| -> Box<dyn std::error::Error + Send + Sync> {
                std::io::Error::other(error).into()
            },
        );
        fluxdown_agent::logging::finish(&result);
        return result;
    }
    // `--server`：headless 服务器形态（NAS / Docker）。不启动托盘、不接入桌面集成，
    // 监听地址、访问密钥、Web UI 等全部来自 `FLUXDOWN_*` 环境变量。
    if args.iter().any(|arg| arg == "--server") {
        let result = fluxdown_agent::server_mode::run_blocking(
            fluxdown_agent::shell::ShellHost::without_tray(
                fluxdown_protocol::TrayUnavailableReason::NotBuilt,
                false,
            ),
        );
        // 运行期（含 daemon）已完全结束：此时才能执行更新后的重启（Unix 上 exec 成功不再返回）。
        run_restart_plan();
        return result;
    }
    let autostart = args
        .iter()
        .any(|arg| arg == fluxdown_agent::platform::AUTOSTART_ARG);
    fluxdown_agent::logging::init_desktop();
    // 提权（UAC 完整令牌）的常驻 agent 建的管道浏览器中继打不开：交给未提权实例后退出。
    #[cfg(windows)]
    if fluxdown_agent::platform::elevation::hand_off_if_elevated(&args)
        == fluxdown_agent::platform::elevation::Startup::HandedOff
    {
        let result = Ok(());
        fluxdown_agent::logging::finish(&result);
        return result;
    }
    let result = {
        #[cfg(feature = "desktop")]
        {
            fluxdown_agent::shell::host::run(autostart, fluxdown_agent::runtime::run_blocking)
        }
        #[cfg(not(feature = "desktop"))]
        {
            fluxdown_agent::runtime::run_blocking(fluxdown_agent::shell::ShellHost::without_tray(
                fluxdown_protocol::TrayUnavailableReason::NotBuilt,
                autostart,
            ))
        }
    };
    // 托盘事件循环已结束且 runtime 线程已 join：daemon 已退出、`agent.lock` 已释放。
    run_restart_plan();
    fluxdown_agent::logging::finish(&result);
    result
}

/// 执行 `update::restart` 登记的重启计划（没有计划时无操作）；失败只记日志，进程照常退出。
fn run_restart_plan() {
    if let Err(error) = fluxdown_agent::update::restart::run_pending() {
        tracing::error!(%error, "could not restart FluxDown after the update");
    }
}
