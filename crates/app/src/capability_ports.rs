use std::sync::Arc;

use crate::agent_client::AgentClient;

pub struct AgentRssPort {
    client: Arc<AgentClient>,
}

impl AgentRssPort {
    pub fn new(client: Arc<AgentClient>) -> Self {
        Self { client }
    }
}

impl fluxdown_ui_rss::RssPort for AgentRssPort {
    fn call(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> fluxdown_ui_rss::PortFuture<serde_json::Value> {
        self.client.call(method, Some(params))
    }
}

pub struct AgentExtensionsPort {
    client: Arc<AgentClient>,
}

impl AgentExtensionsPort {
    pub fn new(client: Arc<AgentClient>) -> Self {
        Self { client }
    }
}

impl fluxdown_ui_extensions::ExtensionsPort for AgentExtensionsPort {
    fn call(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> fluxdown_ui_extensions::PortFuture<serde_json::Value> {
        self.client.call(method, Some(params))
    }
}

/// 插件界面脚本的 `fluxdown.call`：方法名来自脚本（运行期字符串），白名单在 capability 内校验。
pub struct AgentPluginUiPort {
    client: Arc<AgentClient>,
}

impl AgentPluginUiPort {
    pub fn new(client: Arc<AgentClient>) -> Self {
        Self { client }
    }
}

impl fluxdown_ui_plugin_ui::PluginUiPort for AgentPluginUiPort {
    fn call(
        &self,
        method: String,
        params: serde_json::Value,
    ) -> fluxdown_ui_plugin_ui::PortFuture<serde_json::Value> {
        self.client.call(&method, Some(params))
    }
}
