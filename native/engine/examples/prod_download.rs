//! 端到端生产路径验证：调**真实引擎函数** find_sources + download_block_from_peer，
//! 把 eMule0.50a.zip 的第 0 块真正下到临时文件。证明 UI 里能下载。
//! run: cargo run -p fluxdown_engine --example prod_download

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use fluxdown_engine::ed2k::client::{ClientConfig, Source, shared_client};
use fluxdown_engine::ed2k::hash::PART_SIZE;
use fluxdown_engine::ed2k::peer::download_block_from_peer;
use fluxdown_engine::speed_limiter::SpeedLimiter;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

const FILE_HASH: [u8; 16] = [
    0xE8, 0xC6, 0x36, 0xD0, 0xC0, 0x48, 0x63, 0x78, 0xBF, 0x61, 0xE6, 0xA3, 0x00, 0x0D, 0x0F, 0xB7,
];
const FILE_SIZE: u64 = 2907254;

#[tokio::main]
async fn main() {
    let servers: Vec<String> = [
        "45.82.80.155:5687",
        "176.123.5.89:4725",
        "91.208.162.182:4232",
        "213.141.198.207:4232",
        "91.208.162.87:4232",
        "77.42.68.79:4232",
        "85.121.5.137:4232",
        "176.123.2.239:4232",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let client = shared_client();
    client.configure(ClientConfig {
        servers,
        ..ClientConfig::default()
    });

    // 1. 真实 find_sources（多服务器并发聚合）。
    println!("calling find_sources...");
    let sources = match client.find_sources(&FILE_HASH, FILE_SIZE, false).await {
        Ok(query) => query.sources,
        Err(e) => {
            println!("find_sources failed: {e}");
            return;
        }
    };
    let high: Vec<Source> = sources
        .iter()
        .filter(|s| matches!(s, Source::HighId(_)))
        .copied()
        .collect();
    println!("found {} sources ({} HighID)", sources.len(), high.len());
    if high.is_empty() {
        println!("no HighID sources this run (churn) — retry or file cold");
        return;
    }

    // 2. 真实 download_block_from_peer 逐个 HighID 源尝试，直到某个成功下块 0。
    let dest = std::env::temp_dir().join("fluxdown_prod_probe.part");
    // 预分配目标文件到全长（peer.rs 用 open+write，需文件存在）。
    if let Err(error) = tokio::fs::write(&dest, vec![0u8; FILE_SIZE as usize]).await {
        eprintln!(
            "prepare peer download destination {}: {error}",
            dest.display()
        );
        return;
    }
    let cancel = CancellationToken::new();
    let limiter = SpeedLimiter::new(0);

    for (i, src) in high.iter().enumerate() {
        let Source::HighId(peer) = src else { continue };
        println!("\n[{}/{}] trying block 0 from {}", i + 1, high.len(), peer);
        let hc: Arc<OnceCell<Vec<[u8; 16]>>> = Arc::new(OnceCell::new());
        let pg: Arc<StdMutex<HashMap<u64, i64>>> = Arc::new(StdMutex::new(HashMap::new()));
        let r = tokio::time::timeout(
            Duration::from_secs(40),
            download_block_from_peer(
                *peer,
                &FILE_HASH,
                0,
                FILE_SIZE,
                PART_SIZE,
                false,
                &dest,
                &cancel,
                &limiter,
                hc,
                Arc::clone(&pg),
            ),
        )
        .await;
        match r {
            Ok(Ok((p, md4))) => {
                let got = pg.lock().ok().and_then(|m| m.get(&0).copied()).unwrap_or(0);
                println!(
                    "  *** SUCCESS: block 0 downloaded from {} ({} bytes), md4={} ***",
                    p,
                    got,
                    md4.iter().map(|b| format!("{b:02x}")).collect::<String>()
                );
                println!(
                    "\n=== PRODUCTION PATH WORKS: real bytes downloaded to {} ===",
                    dest.display()
                );
                return;
            }
            Ok(Err(e)) => println!("  failed: {}", e.source),
            Err(_) => println!("  timed out (40s)"),
        }
    }
    println!("\n=== all HighID sources failed this run ===");
}
