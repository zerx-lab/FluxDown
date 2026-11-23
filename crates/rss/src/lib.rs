//! RSS 订阅、条目、验证与动作 capability。

mod assets;
mod controller;
mod editor;
mod images;
mod provider;
mod view;

pub use assets::{RSS_ICON_PATH, RssAssets};
pub use controller::RssController;
pub use view::RssView;

use std::{future::Future, pin::Pin};

pub type PortFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, fluxdown_protocol::RpcErrorData>> + Send + 'static>>;

pub trait RssPort: Send + Sync {
    fn call(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> PortFuture<serde_json::Value>;

    /// Fetch one zero-based PNG preview for a persisted RSS item from the agent.
    fn image(&self, source_id: String, guid: String, index: usize) -> PortFuture<Vec<u8>>;
}
