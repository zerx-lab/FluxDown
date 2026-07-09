//! CLI 式同进程直接调用 `fluxdown_engine` 的最小可执行证明。
//!
//! 不依赖 `hub`/`rinf` 的任何符号,仅 `use fluxdown_engine::*`,构造
//! `Engine::new(config, Arc::new(NoopSink), Arc::new(NoopSelection))`,对一个
//! 本地 HTTP 服务器提供的小文件发起一次完整的"创建任务 → 下载 → 完成"流程。
//!
//! 运行:
//! ```text
//! cargo run --example headless_download -p fluxdown_engine
//! ```
//!
//! 期望:进程退出码为 0,目标文件被创建且内容与源文件字节相同。
//!
//! **范围声明**:这只是"CLI 式同进程直接调用"路径可行的具体可执行证明
//! (原始需求条款 1 的部分验证)。Server 的多并发客户端复用与 Phone 的跨
//! FFI/uniffi 绑定复用不在本示例验证范围内,仍依赖架构判断,留待对应产品
//! 立项时验证,不得理解为"三端复用均已证明"。

use std::io::Write as _;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use fluxdown_engine::bt_downloader::BtConfig;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

/// 固定内容,便于校验下载结果字节完全一致。
const FILE_BODY: &[u8] = b"fluxdown-engine headless download example payload\n";

/// 启动一个极简本地 HTTP/1.1 服务器:对每个请求返回固定 body(HEAD 请求
/// 省略 body)。probe 阶段(HEAD/GET Range:0-0)与真正下载各自开一条新
/// 连接(`Connection: close`),故需持续 accept,不能一次性退出。
fn spawn_local_file_server() -> std::io::Result<(u16, std::thread::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            // 读取(并丢弃)请求头,直到空行,记录 method。
            let mut buf = [0u8; 4096];
            let mut header_text = String::new();
            loop {
                match std::io::Read::read(&mut stream, &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        header_text.push_str(&String::from_utf8_lossy(&buf[..n]));
                        if header_text.contains("\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let is_head = header_text.starts_with("HEAD ");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                FILE_BODY.len()
            );
            let _ = stream.write_all(response.as_bytes());
            if !is_head {
                let _ = stream.write_all(FILE_BODY);
            }
            let _ = stream.flush();
        }
    });
    Ok((port, handle))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let work_dir = std::env::temp_dir().join(format!(
        "fluxdown-engine-headless-example-{}",
        std::process::id()
    ));
    tokio::fs::create_dir_all(&work_dir).await?;

    let (port, _server_handle) = spawn_local_file_server()?;
    let url = format!("http://127.0.0.1:{port}/payload.bin");

    let mut engine = Engine::new(
        EngineConfig {
            max_concurrent: 4,
            speed_limit_bps: 0,
            default_save_dir: work_dir.to_string_lossy().into_owned(),
            app_data_dir: work_dir.to_string_lossy().into_owned(),
            bt_config: BtConfig::default(),
            proxy_config: ProxyConfig::default(),
            user_agent: String::new(),
            // 显式指定数据目录,避免示例污染真实用户数据目录。
            data_dir_override: Some(work_dir.clone()),
            database_url: None,
        },
        Arc::new(NoopSink),
        Arc::new(NoopSelection),
    )
    .await?;

    let mut done_rx = engine
        .manager
        .take_done_rx()
        .expect("take_done_rx should return Some on first call");

    engine
        .manager
        .create_task(
            url,
            work_dir.to_string_lossy().into_owned(),
            "payload.bin".to_string(),
            1, // 单分段——本地一次性服务器不支持并发多连接
            String::new(),
            String::new(),
            0,
            Vec::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            std::collections::HashMap::new(),
            Vec::new(),
            None,
            None,
            None,
        )
        .await;

    // 等待任务完成通知(带超时,避免示例在 CI 中意外挂起)。
    let done = tokio::time::timeout(Duration::from_secs(10), done_rx.recv())
        .await
        .map_err(|_| "timed out waiting for download to complete")?
        .ok_or("done channel closed unexpectedly")?;
    engine.manager.on_task_done(&done).await;

    let dest = work_dir.join("payload.bin");
    let downloaded = tokio::fs::read(&dest).await?;
    assert_eq!(
        downloaded, FILE_BODY,
        "downloaded content must match the source file byte-for-byte"
    );

    println!(
        "OK: downloaded {} bytes to {} (byte-for-byte match)",
        downloaded.len(),
        dest.display()
    );

    // 清理工作目录(best-effort)。
    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    Ok(())
}
