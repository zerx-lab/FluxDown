mod actors;
mod db;
mod download_manager;
mod downloader;
mod ftp_downloader;
mod native_messaging;
mod segment_advisor;
mod signals;
mod speed_limiter;
mod updater;

use actors::create_actors;
use rinf::{dart_shutdown, write_interface};
use tokio::spawn;

write_interface!();

#[tokio::main(flavor = "current_thread")]
async fn main() {
    spawn(create_actors());
    dart_shutdown().await;
}
