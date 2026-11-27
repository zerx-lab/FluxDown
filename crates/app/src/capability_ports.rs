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

    fn image(
        &self,
        source_id: String,
        guid: String,
        index: usize,
    ) -> fluxdown_ui_rss::PortFuture<Vec<u8>> {
        self.client.rss_image(source_id, guid, index)
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

    fn feedback(
        &self,
        identity: String,
        report: Option<serde_json::Value>,
    ) -> fluxdown_ui_extensions::PortFuture<serde_json::Value> {
        self.client.plugin_feedback(identity, report)
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
    fn image(&self, session: String, id: String) -> fluxdown_ui_plugin_ui::PortFuture<Vec<u8>> {
        self.client.plugin_image(session, id)
    }
}

/// AI 助手页的 `agent.assistant.*` 调用：全部走统一的 agent 客户端。
pub struct AgentAssistantPort {
    client: Arc<AgentClient>,
}

impl AgentAssistantPort {
    pub fn new(client: Arc<AgentClient>) -> Self {
        Self { client }
    }

    /// 只关心成败的调用：结果是 `{ "ok": true }`，内容不读。
    fn call_ok<P>(&self, method: &str, params: P) -> fluxdown_ui_assistant::PortFuture<()>
    where
        P: serde::Serialize + Send + 'static,
    {
        let future = self
            .client
            .call::<P, serde_json::Value>(method, Some(params));
        Box::pin(async move { future.await.map(|_| ()) })
    }
}

impl fluxdown_ui_assistant::AssistantPort for AgentAssistantPort {
    fn get(&self) -> fluxdown_ui_assistant::PortFuture<fluxdown_protocol::AssistantStateDto> {
        self.client
            .call::<(), _>(fluxdown_protocol::method::AGENT_ASSISTANT_GET, None)
    }

    fn refresh(&self) -> fluxdown_ui_assistant::PortFuture<fluxdown_protocol::AssistantStateDto> {
        self.client
            .call::<(), _>(fluxdown_protocol::method::AGENT_ASSISTANT_REFRESH, None)
    }

    fn sessions(
        &self,
        params: fluxdown_protocol::AssistantSessionsParams,
    ) -> fluxdown_ui_assistant::PortFuture<fluxdown_protocol::AssistantSessionPage> {
        self.client.call(
            fluxdown_protocol::method::AGENT_ASSISTANT_SESSIONS,
            Some(params),
        )
    }

    fn session(
        &self,
        session_id: String,
    ) -> fluxdown_ui_assistant::PortFuture<fluxdown_protocol::AssistantSessionDetail> {
        self.client.call(
            fluxdown_protocol::method::AGENT_ASSISTANT_SESSION,
            Some(fluxdown_protocol::AssistantSessionIdParams { session_id }),
        )
    }

    fn delete_session(&self, session_id: String) -> fluxdown_ui_assistant::PortFuture<()> {
        self.call_ok(
            fluxdown_protocol::method::AGENT_ASSISTANT_DELETE_SESSION,
            fluxdown_protocol::AssistantSessionIdParams { session_id },
        )
    }

    fn clear_sessions(
        &self,
    ) -> fluxdown_ui_assistant::PortFuture<fluxdown_protocol::AssistantClearedSessions> {
        self.client.call(
            fluxdown_protocol::method::AGENT_ASSISTANT_CLEAR_SESSIONS,
            None::<()>,
        )
    }

    fn ask(
        &self,
        params: fluxdown_protocol::AssistantAskParams,
    ) -> fluxdown_ui_assistant::PortFuture<fluxdown_protocol::AssistantAskResult> {
        self.client
            .call(fluxdown_protocol::method::AGENT_ASSISTANT_ASK, Some(params))
    }

    fn cancel(&self, session_id: String) -> fluxdown_ui_assistant::PortFuture<()> {
        self.call_ok(
            fluxdown_protocol::method::AGENT_ASSISTANT_CANCEL,
            fluxdown_protocol::AssistantSessionIdParams { session_id },
        )
    }

    fn confirm(&self, request_id: String, approved: bool) -> fluxdown_ui_assistant::PortFuture<()> {
        self.call_ok(
            fluxdown_protocol::method::AGENT_ASSISTANT_CONFIRM,
            fluxdown_protocol::AssistantConfirmParams {
                request_id,
                approved,
            },
        )
    }

    fn rate(
        &self,
        params: fluxdown_protocol::AssistantRateParams,
    ) -> fluxdown_ui_assistant::PortFuture<()> {
        self.call_ok(fluxdown_protocol::method::AGENT_ASSISTANT_RATE, params)
    }

    fn confirm_issue(
        &self,
        params: fluxdown_protocol::AssistantIssueParams,
    ) -> fluxdown_ui_assistant::PortFuture<fluxdown_protocol::AssistantIssueDto> {
        self.client.call(
            fluxdown_protocol::method::AGENT_ASSISTANT_CONFIRM_ISSUE,
            Some(params),
        )
    }

    fn discard_issue(&self, issue_id: String) -> fluxdown_ui_assistant::PortFuture<()> {
        self.call_ok(
            fluxdown_protocol::method::AGENT_ASSISTANT_DISCARD_ISSUE,
            fluxdown_protocol::AssistantIssueParams {
                issue_id,
                title: None,
                body: None,
            },
        )
    }
}
