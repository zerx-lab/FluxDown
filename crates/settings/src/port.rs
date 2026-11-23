//! 设置能力的宿主端口：由 app 注入单一 agent 会话，本 crate 只知道方法名与 JSON。

use std::pin::Pin;

pub type PortFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, fluxdown_protocol::RpcErrorData>> + Send + 'static>>;

pub trait SettingsPort: Send + Sync {
    fn call(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> PortFuture<serde_json::Value>;

    /// 宿主需要由前台进程打开目录时返回执行 future；`None` 保留原有 agent RPC。
    fn foreground_open_directory(&self, _path: &str) -> Option<PortFuture<serde_json::Value>> {
        None
    }

    /// 宿主需要由前台进程安装浏览器扩展（拉起浏览器）时返回执行 future；`None` 保留原有 agent RPC。
    fn foreground_install_browser_extension(
        &self,
        _browser: fluxdown_protocol::browser_extension::BrowserKind,
    ) -> Option<PortFuture<serde_json::Value>> {
        None
    }
}
