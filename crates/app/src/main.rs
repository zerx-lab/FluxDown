//! `fluxdown-desktop` 的薄入口；应用装配集中在本 crate。
// 发布构建为 GUI 子系统：双击 / 由 agent 拉起时不弹控制台窗口。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account_host;
mod account_port;
mod actions;
mod activity;
mod agent_client;
mod agent_endpoint;
mod app;
mod app_icon;
mod assets;
mod capability_ports;
mod chrome_layout;
mod command_palette;
mod completion_windows;
mod downloads_port;
mod instance_ipc;
mod launch;
mod legacy_themes;
mod lifecycle;
mod logging;
mod menus;
mod plugin_contributions;
mod plugin_notices;
mod power;
mod preference_writes;
mod progress_windows;
mod service_bootstrap;
mod service_failure;
mod service_notices;
mod session;
mod settings_port;
#[cfg(windows)]
mod shell_open;
mod shortcuts;
mod update_notices;
mod windows;

use std::process::ExitCode;

/// mimalloc 全局分配器：GPUI 每帧大量小对象分配，吞吐与碎片均优于系统默认分配器。
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    logging::init();
    let result = app::run();
    logging::finish(
        result
            .as_ref()
            .err()
            .map(|error| error as &dyn std::error::Error),
    );
    exit_code(result)
}

fn exit_code(result: Result<app::RunOutcome, app::AppError>) -> ExitCode {
    match result {
        Ok(app::RunOutcome::Completed) => ExitCode::SUCCESS,
        Ok(app::RunOutcome::NoPrimary) => ExitCode::from(3),
        Err(error) => {
            eprintln!("failed to start FluxDown desktop client: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_existing_without_a_primary_uses_exit_code_three() {
        assert_eq!(exit_code(Ok(app::RunOutcome::NoPrimary)), ExitCode::from(3));
    }
}
