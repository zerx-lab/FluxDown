//! ED2K 端到端生产路径验证：走真实 `Engine`（`create_task` → `run_ed2k_download`
//! → 服务器/Kad 找源 → peer 拉块 → MD4 终验 → rename），把 eMule0.50a.zip
//! 完整下载到临时目录并校验 root hash。
//!
//! 运行（可选参数 1 = nodes.dat 路径，默认 `%TEMP%\nodes.dat`）：
//! ```text
//! cargo run -p fluxdown_engine --example ed2k_e2e
//! ```

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use fluxdown_engine::bt_downloader::BtConfig;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

const ED2K_LINK: &str = "ed2k://|file|eMule0.50a.zip|2907254|E8C636D0C0486378BF61E6A3000D0FB7|h=S5ZFHA4PBCMYAWGVAPPPK4ISNXVHUUGY|/";

const SERVERS: &str = "45.82.80.155:5687,176.123.5.89:4725,91.208.162.182:4232,\
213.141.198.207:4232,91.208.162.87:4232,77.42.68.79:4232,85.121.5.137:4232,176.123.2.239:4232";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let nodes_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| format!("{}\\nodes.dat", std::env::var("TEMP").unwrap_or_default()));
    let nodes_dat =
        std::fs::read(&nodes_path).map_err(|e| format!("read nodes.dat at {nodes_path}: {e}"))?;
    println!("nodes.dat: {} bytes", nodes_dat.len());

    let work_dir = std::env::temp_dir().join(format!("fluxdown-ed2k-e2e-{}", std::process::id()));
    tokio::fs::create_dir_all(&work_dir).await?;

    let mut engine = Engine::new(
        EngineConfig {
            max_concurrent: 4,
            speed_limit_bps: 0,
            default_save_dir: work_dir.to_string_lossy().into_owned(),
            app_data_dir: work_dir.to_string_lossy().into_owned(),
            bt_config: BtConfig::default(),
            proxy_config: ProxyConfig::default(),
            user_agent: String::new(),
            data_dir_override: Some(work_dir.clone()),
            database_url: None,
        },
        Arc::new(NoopSink),
        Arc::new(NoopSelection),
    )
    .await?;

    // 注入 ed2k 运行配置（生产中由 hub 写入 DB config）。
    engine.db.set_config("ed2k_server_list", SERVERS).await?;
    engine.db.set_config("ed2k_listen_port", "0").await?;
    engine.db.set_config("ed2k_enable_upnp", "false").await?;
    engine.db.set_config("ed2k_enable_kad", "true").await?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&nodes_dat);
    engine.db.set_config("ed2k_nodes_dat_cache", &b64).await?;

    let mut done_rx = engine
        .manager
        .take_done_rx()
        .ok_or("take_done_rx should return Some on first call")?;

    engine
        .manager
        .create_task(
            ED2K_LINK.to_string(),
            work_dir.to_string_lossy().into_owned(),
            String::new(),
            0,
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

    let started = std::time::Instant::now();
    let done = tokio::time::timeout(Duration::from_secs(600), done_rx.recv())
        .await
        .map_err(|_| "timed out (600s) waiting for ed2k download")?
        .ok_or("done channel closed unexpectedly")?;
    engine.manager.on_task_done(&done).await;

    let dest = work_dir.join("eMule0.50a.zip");
    let bytes = tokio::fs::read(&dest).await?;
    assert_eq!(bytes.len(), 2_907_254, "size mismatch");
    println!(
        "OK: downloaded {} bytes in {:.0}s to {} (engine already verified block MD4 + root hash)",
        bytes.len(),
        started.elapsed().as_secs_f32(),
        dest.display()
    );

    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    Ok(())
}
