//! Kad 找源探针：读本地 nodes.dat，对 eMule0.50a.zip 跑 find_sources_kad。
//! run: cargo run -p fluxdown_engine --example kad_probe -- <nodes.dat path>

use std::time::Duration;

use fluxdown_engine::ed2k::kad::node::find_sources_kad;
use tokio_util::sync::CancellationToken;

const FILE_HASH: [u8; 16] = [
    0xE8, 0xC6, 0x36, 0xD0, 0xC0, 0x48, 0x63, 0x78, 0xBF, 0x61, 0xE6, 0xA3, 0x00, 0x0D, 0x0F, 0xB7,
];
const FILE_SIZE: u64 = 2907254;

#[tokio::main]
async fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| format!("{}\\nodes.dat", std::env::var("TEMP").unwrap_or_default()));
    let nodes = match std::fs::read(&path) {
        Ok(v) => v,
        Err(e) => {
            println!("read {path} failed: {e}");
            return;
        }
    };
    println!("nodes.dat: {} bytes", nodes.len());
    let cancel = CancellationToken::new();
    let start = std::time::Instant::now();
    match find_sources_kad(
        &FILE_HASH,
        FILE_SIZE,
        0,
        4661,
        &nodes,
        Duration::from_secs(90),
        &cancel,
        None,
    )
    .await
    {
        Ok(sources) => {
            println!(
                "kad found {} sources in {:.1}s",
                sources.len(),
                start.elapsed().as_secs_f32()
            );
            for s in sources.iter().take(20) {
                println!("  {s}");
            }
        }
        Err(e) => println!("kad failed: {e}"),
    }
}
