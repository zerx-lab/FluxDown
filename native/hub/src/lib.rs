mod actors;
mod api_host;
mod file_association;
mod logger;
mod native_messaging;
mod nmh_registry;
mod protocol_registry;
mod reveal_file;
pub mod rinf_selection;
mod rinf_sink;
mod signal_bridge;
mod signals;
mod updater;

use actors::create_actors;
use rinf::{dart_shutdown, write_interface};
use tokio::spawn;

write_interface!();

// RUNTIME CONSTRAINT: This binary uses a single-threaded (`current_thread`) Tokio runtime.
// All tasks share the same OS thread, so blocking operations (blocking I/O, `std::thread::sleep`,
// `Mutex::lock` held across `.await`, etc.) will stall every other task on the runtime.
//
// Rules for contributors:
//   • Never call blocking APIs directly in `async fn` — wrap them in `tokio::task::spawn_blocking`.
//   • Never use `mpsc::Sender::blocking_send` inside a `tokio::spawn(async { … })` block;
//     use `.send(…).await` instead. `blocking_send` is only safe inside `spawn_blocking` closures.
//   • Never park the thread with `std::thread::sleep` or synchronous `Mutex` contention in async code.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    logger::init();
    spawn(create_actors());
    dart_shutdown().await;
}
