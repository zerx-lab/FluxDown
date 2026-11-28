//! AI 助手 capability：与 FluxCloud AI 助手对话的窗口右侧面板。
//!
//! 数据全部来自本机 agent：提问 / 取消 / 确认等动作经 [`AssistantPort`] 调 `agent.assistant.*`，
//! 执行过程（流式回答、工具调用、待确认的工具）经 `AgentEvent::AssistantChanged` 整体替换推送，
//! 初始状态在 `AgentSnapshot.assistant`。面板内容是对话流（或切到云端会话列表的历史视图），
//! 顶栏插槽 [`AssistantPanelHeader`] 放标题、用量、新对话与历史切换；改动本机或消耗流量的
//! 工具调用以内嵌确认卡片等待用户允许。

pub mod actions;
mod ask;
mod attachment;
mod dates;
mod draft;
mod header;
mod history;
mod image_viewer;
mod images;
mod markdown;
mod model;
mod preview;
mod render;
mod render_composer;
mod render_history;
mod render_issue;
mod render_steps;
mod view;
mod widgets;

use std::{future::Future, pin::Pin};

use fluxdown_protocol::{
    AssistantAskParams, AssistantAskResult, AssistantAttachmentResult, AssistantClearedSessions,
    AssistantExportFormat, AssistantExportResult, AssistantIssueDto, AssistantIssueParams,
    AssistantRateParams, AssistantSessionDetail, AssistantSessionDto, AssistantSessionPage,
    AssistantSessionsParams, AssistantStateDto, RpcErrorData,
};

pub use header::AssistantPanelHeader;
pub use view::{AssistantHost, AssistantView, WindowAction};

pub type PortFuture<T> = Pin<Box<dyn Future<Output = Result<T, RpcErrorData>> + Send + 'static>>;

/// 助手页对本机 agent 的全部调用面（`agent.assistant.*`）；由 app 用统一的 agent 客户端实现。
pub trait AssistantPort: Send + Sync {
    /// `agent.assistant.get`：读本机缓存状态（配置过期时 agent 在后台刷新并推送）。
    fn get(&self) -> PortFuture<AssistantStateDto>;
    /// `agent.assistant.refresh`：立即向 FluxCloud 拉取配置。
    fn refresh(&self) -> PortFuture<AssistantStateDto>;
    /// `agent.assistant.sessions`：云端会话，最近更新在前。
    fn sessions(&self, params: AssistantSessionsParams) -> PortFuture<AssistantSessionPage>;
    /// `agent.assistant.session`：会话详情（历史消息与问题单）。
    fn session(&self, session_id: String) -> PortFuture<AssistantSessionDetail>;
    /// `agent.assistant.deleteSession`：只在用户侧隐藏，云端保留。
    fn delete_session(&self, session_id: String) -> PortFuture<()>;
    /// `agent.assistant.clearSessions`：清空本人全部会话（同样只在用户侧隐藏）。
    fn clear_sessions(&self) -> PortFuture<AssistantClearedSessions>;
    /// `agent.assistant.ask`：提问；执行随后经 `AssistantChanged` 推送。
    fn ask(&self, params: AssistantAskParams) -> PortFuture<AssistantAskResult>;
    /// `agent.assistant.cancel`：取消会话中进行中的执行。
    fn cancel(&self, session_id: String) -> PortFuture<()>;
    /// `agent.assistant.confirm`：批准 / 拒绝一次待确认的客户端工具。
    fn confirm(&self, request_id: String, approved: bool) -> PortFuture<()>;
    /// `agent.assistant.rate`。
    fn rate(&self, params: AssistantRateParams) -> PortFuture<()>;
    /// `agent.assistant.confirmIssue`：确认提交云端起草的问题单（可带修改后的标题 / 正文）。
    fn confirm_issue(&self, params: AssistantIssueParams) -> PortFuture<AssistantIssueDto>;
    /// `agent.assistant.discardIssue`。
    fn discard_issue(&self, issue_id: String) -> PortFuture<()>;
    /// `agent.assistant.renameSession`：重命名会话（标题 trim 后 1–80 字符）。
    fn rename_session(&self, session_id: String, title: String) -> PortFuture<AssistantSessionDto>;
    /// `agent.assistant.exportSession`：导出会话为 Markdown / JSON（云端生成，UI 负责保存）。
    fn export_session(
        &self,
        session_id: String,
        format: AssistantExportFormat,
    ) -> PortFuture<AssistantExportResult>;
    /// `agent.assistant.attachment`：按附件 ID 取历史提问里的一张图（data URL）。
    fn attachment(&self, attachment_id: String) -> PortFuture<AssistantAttachmentResult>;
}
