//! `fluxdownd` 常驻下载核心进程。
// 发布构建为 GUI 子系统：由 agent 在后台拉起，任何路径下都不弹控制台窗口。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use tokio_util::sync::CancellationToken;

/// mimalloc 全局分配器：多线程 tokio 下吞吐与内存碎片均优于 musl/glibc 默认分配器。
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// 主 runtime 的 worker 上限。下载数据面是 IO 等待为主（TLS 解密、分段写盘在 worker / 阻塞池
/// 之间流转），4 个 worker 已远超千兆 / 万兆链路需要；默认按核数开时每个 worker 的分配器线程
/// 堆在空闲时也常驻（10 核机实测 20 → 13 线程省约 3MB）。BT 与插件各有专用 runtime，不受此限。
const MAX_WORKER_THREADS: usize = 4;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workers = std::thread::available_parallelism()
        .map_or(2, std::num::NonZeroUsize::get)
        .clamp(2, MAX_WORKER_THREADS);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cancel = CancellationToken::new();
    let signal_cancel = cancel.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        signal_cancel.cancel();
    });
    let result = fluxdown_daemon::runtime::run(cancel).await;
    if let Err(error) = &result {
        let mut chain = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            chain.push_str(": ");
            chain.push_str(&cause.to_string());
            source = cause.source();
        }
        tracing::error!(error = %chain, "fluxdownd exited with error");
    }
    result
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let Ok(mut terminate) = signal(SignalKind::terminate()) else {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
        return;
    };
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if result.is_err() {
                terminate.recv().await;
            }
        }
        _ = terminate.recv() => {}
    }
}

/// 没有控制台时 Ctrl-C 处理器可能注册失败：失败即永不触发，而不是立刻退出。
/// 正常关停走 agent 的 `system.shutdown`。
#[cfg(not(unix))]
async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}
