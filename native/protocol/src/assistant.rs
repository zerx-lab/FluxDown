//! AI 助手（FluxCloud 代理的模型对话）的 wire 契约。
//!
//! 数据流：UI 经 `agent.assistant.*` 提问；agent 以账号身份调 FluxCloud `/api/v1/ai/*`，消费提问的
//! SSE 事件流，在本机执行云端下发的客户端工具（`clientToolCall`）并回传结果。非只读工具
//! （[`AssistantToolEffect::Network`] / [`AssistantToolEffect::Write`]）先挂起等待用户在任一官方 UI
//! 确认（`agent.assistant.confirm`），确认后才执行。进行中与最近结束的执行投影为
//! `AgentSnapshot.assistant`，经 `AssistantChanged` 整体替换推送；历史消息按需经
//! `agent.assistant.session` 从云端读取（消息带 `runId`，UI 据此去掉已落库执行的实时覆盖层）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 本机实现的客户端工具信封协议版本（与 FluxCloud `CLIENT_TOOL_PROTOCOL` 一致才声明工具）。
pub const ASSISTANT_CLIENT_TOOL_PROTOCOL: u32 = 3;
/// `AssistantStateDto.runs` 保留的执行数上限（进行中的执行永不被裁掉）。
pub const ASSISTANT_MAX_RUNS: usize = 8;

/// 发起提问的界面渠道（云端据此做路由与统计）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum AssistantChannel {
    /// 桌面 GPUI。
    #[default]
    Desktop,
    /// headless Web 管理界面。
    Web,
}

impl AssistantChannel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Web => "web",
        }
    }
}

/// 会话类型：普通提问或反馈（反馈场景偏向起草问题单）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum AssistantSessionKind {
    #[default]
    Ask,
    Feedback,
}

impl AssistantSessionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Feedback => "feedback",
        }
    }
}

/// 客户端工具在本机的副作用（与 FluxCloud `ToolEffect` 同义）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum AssistantToolEffect {
    #[default]
    Read,
    /// 产生真实网络流量（测速）。
    Network,
    /// 改动设置或任务。
    Write,
}

impl AssistantToolEffect {
    #[must_use]
    pub const fn needs_confirmation(self) -> bool {
        !matches!(self, Self::Read)
    }
}

/// 云端 `GET /api/v1/ai/config` 的本机投影。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantConfigDto {
    pub available: bool,
    /// 不可用原因：`off` / `internal_only` / `plan_disabled`。
    #[serde(default)]
    pub reason: Option<String>,
    pub max_message_chars: u32,
    pub images_supported: bool,
    pub max_images: u32,
    pub max_image_bytes: u64,
    /// `0` = 不限。
    pub daily_limit: u32,
    pub used_today: u64,
    /// 云端工具协议与本机一致、本机工具会被声明。
    pub tools_enabled: bool,
}

/// 一次执行的状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum AssistantRunStatus {
    /// 已发出提问、尚未收到云端首帧。
    #[default]
    Starting,
    Running,
    /// 模型正常作答。
    Completed,
    /// 路由为固定回复。
    Replied,
    Failed,
    Canceled,
}

impl AssistantRunStatus {
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}

/// 工具调用状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum AssistantToolStatus {
    #[default]
    Running,
    Succeeded,
    Failed,
}

/// 非只读客户端工具的确认状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum AssistantConfirmationState {
    /// 等待用户确认（截止 `confirm_deadline_unix_ms`）。
    Pending,
    Approved,
    Declined,
    /// 超时未确认（云端按「未执行」交还模型）。
    Expired,
}

/// 一次工具调用。服务端 / MCP 工具只有名称、状态与耗时；客户端工具另带本机执行信息。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantToolCallDto {
    pub call_id: String,
    pub name: String,
    /// `client` / `server` / `mcp`。
    pub kind: String,
    pub status: AssistantToolStatus,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    /// 客户端工具的请求 ID（`clientToolCall.requestId`）。
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub effect: AssistantToolEffect,
    /// 客户端工具的调用参数（确认卡片据此说明将要做什么）。
    #[serde(default)]
    pub arguments: Option<Value>,
    #[serde(default)]
    pub confirmation: Option<AssistantConfirmationState>,
    #[serde(default)]
    pub confirm_deadline_unix_ms: Option<i64>,
    /// 本机执行结果摘要（客户端工具，截断后的文本）。
    #[serde(default)]
    pub output_preview: Option<String>,
}

/// 云端起草、待用户确认提交的问题单。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantIssueDto {
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub labels: Vec<String>,
    /// `draft` / `pending_review` / `submitted` / `failed` / `rejected` / `discarded`。
    pub status: String,
    #[serde(default)]
    pub github_number: Option<i64>,
    #[serde(default)]
    pub github_url: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub review_note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 执行中按到达顺序排列的条目。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AssistantRunItemDto {
    /// 一段回答文本（工具调用把回答切成多段）。
    Text {
        text: String,
    },
    Tool {
        call: AssistantToolCallDto,
    },
    IssueDraft {
        issue: AssistantIssueDto,
    },
}

/// 一次执行（一问一答，含其间的工具调用）。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantRunDto {
    pub session_id: String,
    /// 云端执行 ID（收到首帧前为 `None`）。
    #[serde(default)]
    pub run_id: Option<String>,
    pub status: AssistantRunStatus,
    pub question: String,
    pub started_at_unix_ms: i64,
    #[serde(default)]
    pub items: Vec<AssistantRunItemDto>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// AI 助手在本机的完整状态投影（`AgentSnapshot.assistant`）。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantStateDto {
    /// 云端开放了 AI 助手入口（FluxCloud 判定：`mode = enabled`，或 `internal` 且为内部账号；与套餐无关）。
    /// 已登录取 `GET /ai/config` 的 `visible`，未登录取免鉴权 `GET /ai/status`；拉取失败保留上次值，
    /// 初始为 `false`。为 `false` 时各 UI 隐藏全部助手入口。
    #[serde(default)]
    pub visible: bool,
    /// 最近一次成功拉取的云端配置；未登录或从未拉取成功为 `None`。
    #[serde(default)]
    pub config: Option<AssistantConfigDto>,
    #[serde(default)]
    pub loading: bool,
    #[serde(default)]
    pub last_error_reason: Option<crate::ErrorReason>,
    #[serde(default)]
    pub last_error_message: Option<String>,
    /// 进行中与最近结束的执行（新的在后，至多 [`ASSISTANT_MAX_RUNS`] 条）。
    #[serde(default)]
    pub runs: Vec<AssistantRunDto>,
}

/// 云端会话摘要。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantSessionDto {
    pub id: String,
    pub channel: String,
    pub kind: String,
    pub title: String,
    pub message_count: i64,
    /// 会话汇总：该会话最近一次的回答评价（按条评价见 [`AssistantMessageDto::rating`]）。
    #[serde(default)]
    pub rating: Option<String>,
    /// 云端正在回答中。
    #[serde(default)]
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 云端会话中的一条消息（`role`：`user` / `assistant` / `tool_call` / `tool_result`）。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageDto {
    pub id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    pub seq: i64,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub tool_kind: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub images: Vec<String>,
    /// 用户对该条回答的评价（`up` / `down`）；只有 `assistant` 消息可能非空。
    #[serde(default)]
    pub rating: Option<String>,
    #[serde(default)]
    pub rating_comment: Option<String>,
    pub created_at: String,
}

/// `agent.assistant.sessions` 的结果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantSessionPage {
    pub total: u64,
    pub items: Vec<AssistantSessionDto>,
}

/// `agent.assistant.session` 的结果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantSessionDetail {
    pub session: AssistantSessionDto,
    pub messages: Vec<AssistantMessageDto>,
    #[serde(default)]
    pub issues: Vec<AssistantIssueDto>,
}

/// `agent.assistant.clearSessions` 的结果。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantClearedSessions {
    /// 本次清空的会话数。
    pub cleared: u64,
}

/// `agent.assistant.sessions` 参数。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantSessionsParams {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

/// 只带会话 ID 的参数（`session` / `deleteSession` / `cancel`）。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantSessionIdParams {
    pub session_id: String,
}

/// `agent.assistant.ask` 参数。`session_id` 为空时先新建会话。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantAskParams {
    #[serde(default)]
    pub session_id: Option<String>,
    /// 仅新建会话时使用。
    #[serde(default)]
    pub kind: AssistantSessionKind,
    #[serde(default)]
    pub channel: AssistantChannel,
    /// 提问文本。用户插图的位置写 [`assistant_image_placeholder`]（`[Image #N]`，N 从 1 起对应
    /// `images[N-1]`）：云端把图片紧跟在占位符后发给模型，模型据此知道每张图指问题里的哪一处。
    pub text: String,
    /// `data:image/(png|jpeg|gif|webp);base64,...`。
    #[serde(default)]
    pub images: Vec<String>,
    /// 界面语言（BCP 47），新建会话时随客户端信息上报。
    #[serde(default)]
    pub locale: Option<String>,
}

/// `agent.assistant.ask` 的结果：执行随后经 `AssistantChanged` 推送。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantAskResult {
    pub session_id: String,
}

/// `agent.assistant.confirm` 参数。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantConfirmParams {
    pub request_id: String,
    pub approved: bool,
}

/// `agent.assistant.rate` 参数：评价一条回答（`message_id` 为 `assistant` 消息 ID，即执行的
/// [`AssistantRunDto::message_id`]）；`rating`：`up` / `down`，`None` 清除。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantRateParams {
    pub session_id: String,
    pub message_id: String,
    #[serde(default)]
    pub rating: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
}

/// 提问文本里第 `number` 张附图（从 1 起）的占位符：`[Image #N]`，与 FluxCloud 的解析一致。
#[must_use]
pub fn assistant_image_placeholder(number: usize) -> String {
    format!("[Image #{number}]")
}

/// `agent.assistant.confirmIssue` / `discardIssue` 参数；确认时可改标题与正文。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantIssueParams {
    pub issue_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}
