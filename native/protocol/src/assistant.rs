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
    /// 单个会话最多提问次数（套餐能力，重新生成 / 编辑重发也计）；`0` = 不限。
    #[serde(default)]
    pub session_limit: u32,
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
    /// 确认截止时刻（agent 主机时钟）。跨机 UI 用 [`AssistantStateDto::now_unix_ms`] 换算时钟偏差。
    #[serde(default)]
    pub confirm_deadline_unix_ms: Option<i64>,
    /// 本机执行结果摘要（客户端工具，截断后的文本）。
    #[serde(default)]
    pub output_preview: Option<String>,
    /// 待确认调用的可读预览（agent 用本机数据生成：任务名、设置旧值 → 新值等）；无法生成时为 `None`，
    /// UI 回退显示原始参数。
    #[serde(default)]
    pub preview: Option<AssistantToolPreviewDto>,
    /// 不可撤销的破坏性动作（删除任务 / 删除文件等）：确认卡片用危险样式。
    #[serde(default)]
    pub destructive: bool,
    /// 已批准的写操作在执行中被用户停止：本机请求可能已生效，结果未知（`status` 为 `Failed`）。
    #[serde(default)]
    pub interrupted: bool,
}

/// 确认卡片的可读预览。各字段按需出现，UI 按 `action` 本地化动作名、逐项渲染其余字段。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantToolPreviewDto {
    /// 动作标识（camelCase，与工具参数里的 `action` 同名，如 `delete` / `pause` / `create` /
    /// `updateSettings` / `changeUrl`）；UI 映射到 `assistantPreviewAction<Action>` 文案，未知值回退原文。
    pub action: String,
    /// 受影响任务的显示名（至多 [`ASSISTANT_PREVIEW_MAX_ITEMS`] 条）。
    #[serde(default)]
    pub tasks: Vec<String>,
    /// 超出 `tasks` 上限、未列出的任务数。
    #[serde(default)]
    pub more_tasks: u32,
    /// 同时删除已下载的文件。
    #[serde(default)]
    pub delete_files: bool,
    /// 设置变更（旧值 → 新值）。
    #[serde(default)]
    pub settings: Vec<AssistantSettingChangeDto>,
    /// 将要访问 / 新建的地址（至多 [`ASSISTANT_PREVIEW_MAX_ITEMS`] 条，已脱敏）。
    #[serde(default)]
    pub urls: Vec<String>,
    /// 其余原样显示的补充文本（如目标队列名、保存目录）。
    #[serde(default)]
    pub details: Vec<String>,
}

/// 预览里一项设置的变更。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantSettingChangeDto {
    /// 规范设置键（UI 有本地化名时显示名称，否则显示键）。
    pub key: String,
    #[serde(default)]
    pub from: Option<String>,
    pub to: String,
}

/// 预览列表（任务名 / 地址）的条数上限。
pub const ASSISTANT_PREVIEW_MAX_ITEMS: usize = 20;

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
    /// 提问附带的图片张数（执行中提问气泡据此显示占位 / 本地缩略图）。
    #[serde(default)]
    pub image_count: u32,
    /// 沿用的旧图（`keepImages` 重新生成 / 编辑重发）的附件 ID，经 `agent.assistant.attachment` 取图；
    /// 与 `image_count`（本次新附的张数）分开计。
    #[serde(default)]
    pub image_ids: Vec<String>,
    /// 旁观执行：由其他设备或本机重启前发起、本机只接回事件流显示进度。本机不执行它的客户端工具，
    /// 也不能确认 / 拒绝其中的待确认调用；UI 显示 `assistantRunObserving` 提示并隐藏确认按钮。
    #[serde(default)]
    pub observing: bool,
    pub started_at_unix_ms: i64,
    #[serde(default)]
    pub items: Vec<AssistantRunItemDto>,
    #[serde(default)]
    pub message_id: Option<String>,
    /// 失败原文（详情用）；UI 优先按 `error_reason` 本地化。
    #[serde(default)]
    pub error: Option<String>,
    /// 失败原因（camelCase）。云端：`timeout` / `maxTurns` / `noAnswer` / `canceled` /
    /// `contextOverflow` / `imageUnsupported` / `unavailable` / `error`；本机：`streamLost`（事件流
    /// 断开且续读失败）/ `network`。未知值按 `error` 处理。
    #[serde(default)]
    pub error_reason: Option<String>,
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
    /// 生成本投影时 agent 主机的时钟（Unix 毫秒）。UI 以「本地收到时刻 − 该值」作时钟偏差，换算
    /// `confirm_deadline_unix_ms`；旧 agent 缺省为 `0`，UI 视为无偏差。
    #[serde(default)]
    pub now_unix_ms: i64,
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
    /// 云端进行中执行的 ID（`active` 时；旧云端不带）。agent 用它接回本机未跟踪的执行（旁观）。
    #[serde(default)]
    pub active_run_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 云端会话中的一条消息（`role`：`user` / `assistant` / `assistant_step` / `tool_call` / `tool_result`）。
/// `assistant_step` 是工具调用之前的中间说明文字，UI 归入「过程」，不可评价。被「重新生成 / 编辑重发」
/// 替换的消息云端不再返回。
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
    /// 回答未完成就中断（取消 / 超时 / 失败），内容是中断前已输出的部分；只有 `assistant` 消息可能为真。
    #[serde(default)]
    pub interrupted: bool,
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
    /// 本会话已计入 [`AssistantConfigDto::session_limit`] 的提问次数（含已被替换的提问）。
    #[serde(default)]
    pub questions_used: u64,
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
    /// 按标题 / 消息内容搜索（不区分大小写）；空白视为不搜索。
    #[serde(default)]
    pub query: Option<String>,
}

/// `agent.assistant.renameSession` 参数。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantRenameParams {
    pub session_id: String,
    /// 新标题（去首尾空白后 1–[`ASSISTANT_MAX_TITLE_CHARS`] 字符）。
    pub title: String,
}

/// 会话标题的字符数上限（与 FluxCloud 一致）。
pub const ASSISTANT_MAX_TITLE_CHARS: usize = 80;

/// 会话导出格式。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum AssistantExportFormat {
    #[default]
    Markdown,
    Json,
}

/// `agent.assistant.exportSession` 参数。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantExportParams {
    pub session_id: String,
    #[serde(default)]
    pub format: AssistantExportFormat,
}

/// `agent.assistant.exportSession` 的结果：由云端生成，UI 只负责保存 / 下载。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantExportResult {
    /// 建议文件名（已去除路径分隔符等不安全字符，含扩展名）。
    pub file_name: String,
    /// `text/markdown` / `application/json`。
    pub mime: String,
    pub content: String,
}

/// `agent.assistant.attachment` 参数：[`AssistantMessageDto::images`] 里的附件 ID。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantAttachmentParams {
    pub attachment_id: String,
}

/// `agent.assistant.attachment` 的结果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AssistantAttachmentResult {
    /// `data:image/(png|jpeg|gif|webp);base64,...`。
    pub data_url: String,
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
    /// 重新生成 / 编辑后重发：该会话里一条 `user` 消息的 ID。云端把它及之后的消息标为已替换（对用户与
    /// 模型都不再可见），再以本次 `text` 作答。须同时带 `session_id`。
    #[serde(default)]
    pub replace_from: Option<String>,
    /// 与 `replace_from` 连用：沿用被替换提问的附图（此时 `images` 必须为空；`text` 里的占位符
    /// 仍按原附图编号）。
    #[serde(default)]
    pub keep_images: bool,
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
