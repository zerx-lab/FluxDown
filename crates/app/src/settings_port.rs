use std::sync::Arc;

use fluxdown_ui_settings::{PortFuture, SettingsPort};

use crate::agent_client::AgentClient;

/// 设置能力的 agent 适配器：方法名与 JSON 直通单一会话。
pub struct AgentSettingsPort {
    client: Arc<AgentClient>,
}

impl AgentSettingsPort {
    #[must_use]
    pub fn new(client: Arc<AgentClient>) -> Self {
        Self { client }
    }
}

impl SettingsPort for AgentSettingsPort {
    fn call(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> PortFuture<serde_json::Value> {
        self.client.call(method, Some(params))
    }

    #[cfg(windows)]
    fn foreground_open_directory(&self, path: &str) -> Option<PortFuture<serde_json::Value>> {
        let path = path.to_owned();
        Some(Box::pin(async move {
            crate::shell_open::directory(path).await?;
            Ok(serde_json::json!({ "ok": true }))
        }))
    }

    #[cfg(windows)]
    fn foreground_install_browser_extension(
        &self,
        browser: fluxdown_protocol::browser_extension::BrowserKind,
    ) -> Option<PortFuture<serde_json::Value>> {
        Some(Box::pin(crate::shell_open::browser_extension(browser)))
    }
}
