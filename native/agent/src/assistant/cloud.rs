//! FluxCloud AI 助手 API（`/api/v1/ai/*`）客户端：REST 调用、提问 SSE 读取与云端错误映射。
//!
//! 契约以 FluxCloud `routes/client_ai.rs` 与 `ai/runner.rs` 为准。鉴权、令牌刷新与会话 epoch 全部
//! 复用 [`CloudApi`]（本模块不建 HTTP client、不碰令牌）；`RunEvent` 解析只认本机用得上的字段，
//! 未知事件类型与多余字段一律忽略，云端加事件不会破坏旧客户端。

use std::collections::VecDeque;
use std::time::Duration;

use fluxdown_protocol::{
    ASSISTANT_CLIENT_TOOL_PROTOCOL, ApplicationErrorCode, AssistantClearedSessions,
    AssistantConfigDto, AssistantIssueDto, AssistantSessionDetail, AssistantSessionDto,
    AssistantSessionPage, AssistantToolEffect, ErrorReason, RpcErrorData,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cloud::{CloudApi, CloudError, RequestEpoch, encode_path_segment};

/// SSE 单行上限；超过说明流已损坏。
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
/// 单个事件（多行 `data:` 合并后）的上限。
const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;
/// 提问流空闲上限：云端每 15 秒发一次保活帧，长工具调用 / 等待确认期间也不会静默。
const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// 提问响应里云端执行 ID 的头。
const RUN_ID_HEADER: &str = "x-ai-run-id";

// ------------------------------------------------------------------ 错误

/// 助手操作失败：`data` 是给客户端的稳定错误详情，`message` 是可读说明（云端文案或本机英文原因），
/// 进入日志与 `AssistantStateDto.last_error_message`。
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct AssistantError {
    pub(crate) data: RpcErrorData,
    pub(crate) message: String,
}

impl AssistantError {
    fn with(code: ApplicationErrorCode, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            data: RpcErrorData::new(code, retryable),
            message: message.into(),
        }
    }

    pub(crate) fn invalid_field(field: &str, message: impl Into<String>) -> Self {
        let mut error = Self::with(ApplicationErrorCode::InvalidArgument, false, message);
        error.data.field = Some(field.to_owned());
        error
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::with(ApplicationErrorCode::NotFound, false, message)
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::with(ApplicationErrorCode::Conflict, false, message)
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::with(ApplicationErrorCode::Unavailable, false, message)
    }

    pub(crate) fn reason(&self) -> Option<ErrorReason> {
        self.data.reason
    }
}

impl From<CloudError> for AssistantError {
    fn from(error: CloudError) -> Self {
        Self {
            data: rpc_error(&error),
            message: error.message,
        }
    }
}

/// 云端错误 → RPC 错误详情。
///
/// AI 专属错误码按语义落到现有 `ApplicationErrorCode`（协议没有 AI 专属 `ErrorReason`）：
/// - `ai_unavailable`（503，未开放 / 当日额度耗尽）：`Unavailable`，立即重试无意义；
/// - `ai_disabled`（403，套餐不含 AI 助手）：`Unsupported`；
/// - `ai_quota_exceeded`（403，当日提问次数用完）：`Conflict`（与设备数 / 渠道数上限同类）；
/// - `rate_limited`（429）与 401 等沿用通用映射（带 `reason`，`RateLimited` 可重试）；
/// - 其余未识别的 409（如该会话已有进行中的执行）：`Conflict`。
pub(crate) fn rpc_error(error: &CloudError) -> RpcErrorData {
    let base = error.to_rpc_error();
    match (error.status, error.code.as_deref()) {
        (_, Some("ai_unavailable")) => RpcErrorData::new(ApplicationErrorCode::Unavailable, false),
        (_, Some("ai_disabled")) => RpcErrorData::new(ApplicationErrorCode::Unsupported, false),
        (_, Some("ai_quota_exceeded")) => RpcErrorData::new(ApplicationErrorCode::Conflict, false),
        (Some(409), _) if base.reason.is_none() => {
            RpcErrorData::new(ApplicationErrorCode::Conflict, false)
        }
        _ => base,
    }
}

// ------------------------------------------------------------------ wire 类型

/// `GET /ai/config` 的响应（只取本机用得上的字段）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigBody {
    available: bool,
    #[serde(default)]
    reason: Option<String>,
    /// 本账号可见 AI 助手入口（与套餐无关）。
    #[serde(default)]
    visible: bool,
    #[serde(default)]
    client_tool_protocol: u32,
    #[serde(default)]
    client_tools: Vec<CloudToolRef>,
    #[serde(default)]
    max_message_chars: u32,
    #[serde(default)]
    images_supported: bool,
    #[serde(default)]
    max_images: u32,
    #[serde(default)]
    max_image_bytes: u64,
    #[serde(default)]
    daily_limit: u32,
    #[serde(default)]
    used_today: u64,
}

#[derive(Debug, Deserialize)]
struct CloudToolRef {
    name: String,
    version: u32,
}

/// `GET /ai/status`（免鉴权）的响应：未登录时的入口可见性。
#[derive(Debug, Deserialize)]
pub(crate) struct StatusBody {
    pub(crate) visible: bool,
}

impl ConfigBody {
    pub(crate) const fn visible(&self) -> bool {
        self.visible
    }

    /// `declared` 是本机实现的 `(name, version)`；云端工具协议一致且至少一个本机工具在云端目录里，
    /// 才算 `tools_enabled`。
    pub(crate) fn into_dto(self, declared: &[(&'static str, u32)]) -> AssistantConfigDto {
        let tools_enabled = self.available
            && self.client_tool_protocol == ASSISTANT_CLIENT_TOOL_PROTOCOL
            && declared.iter().any(|(name, version)| {
                self.client_tools
                    .iter()
                    .any(|tool| tool.name == *name && tool.version == *version)
            });
        AssistantConfigDto {
            available: self.available,
            reason: self.reason,
            max_message_chars: self.max_message_chars,
            images_supported: self.images_supported,
            max_images: self.max_images,
            max_image_bytes: self.max_image_bytes,
            daily_limit: self.daily_limit,
            used_today: self.used_today,
            tools_enabled,
        }
    }
}

/// 提问 SSE 的一帧（云端 `RunEvent`，`type` 区分）。
#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum RunEvent {
    Run {
        run_id: String,
    },
    Text {
        delta: String,
    },
    /// 本轮回答被重试：丢弃本轮已显示的文本。
    TurnRetried,
    ToolStarted {
        call_id: String,
        name: String,
        kind: String,
    },
    ClientToolCall {
        request_id: String,
        name: String,
        version: u32,
        arguments: Value,
        effect: AssistantToolEffect,
        #[serde(default)]
        confirm_timeout_secs: Option<u32>,
        timeout_secs: u32,
    },
    ToolFinished {
        call_id: String,
        name: String,
        kind: String,
        is_error: bool,
        #[serde(default)]
        duration_ms: Option<i64>,
    },
    IssueDraft {
        issue: Box<AssistantIssueDto>,
    },
    Done {
        status: String,
        #[serde(default)]
        message_id: Option<String>,
        #[serde(default)]
        text: String,
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
    },
    Error {
        status: String,
        message: String,
    },
    /// `triage` / `routed` / `reasoning`（只推给后台测试）与云端后续新增的事件：本机不展示。
    #[serde(other)]
    Other,
}

pub(crate) fn parse_event(payload: &[u8]) -> Result<RunEvent, serde_json::Error> {
    serde_json::from_slice(payload)
}

// ------------------------------------------------------------------ SSE

/// SSE 读取失败。
#[derive(Debug, thiserror::Error)]
pub(crate) enum StreamError {
    #[error("FluxCloud event stream was silent for too long")]
    Idle,
    #[error("FluxCloud event stream read failed: {0}")]
    Read(String),
    #[error("FluxCloud event stream line exceeds the size limit")]
    TooLong,
}

/// 增量 SSE 解析器：按行累积，空行分发一个事件，只认 `data:` 行（多行以 `\n` 合并），
/// 注释 / `event:` / `id:` / `retry:` 一律忽略。单行与单事件都有上限。
#[derive(Default)]
pub(crate) struct SseParser {
    line: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
    ready: VecDeque<Vec<u8>>,
}

impl SseParser {
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Result<(), StreamError> {
        // 只拷贝一行，不拷贝整个传输块的剩余部分。
        for part in chunk.split_inclusive(|byte| *byte == b'\n') {
            if self.line.len().saturating_add(part.len()) > MAX_SSE_LINE_BYTES {
                return Err(StreamError::TooLong);
            }
            self.line.extend_from_slice(part);
            if !self.line.ends_with(b"\n") {
                continue;
            }
            let line = self.line.strip_suffix(b"\n").unwrap_or(&self.line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                if self.has_data {
                    self.ready.push_back(std::mem::take(&mut self.data));
                    self.has_data = false;
                }
            } else if let Some(rest) = line.strip_prefix(b"data:") {
                let rest = rest.strip_prefix(b" ").unwrap_or(rest);
                let separator = usize::from(self.has_data);
                if self
                    .data
                    .len()
                    .saturating_add(separator)
                    .saturating_add(rest.len())
                    > MAX_SSE_EVENT_BYTES
                {
                    return Err(StreamError::TooLong);
                }
                if self.has_data {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(rest);
                self.has_data = true;
            }
            self.line.clear();
        }
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Option<Vec<u8>> {
        self.ready.pop_front()
    }
}

/// 提问响应的事件流：读 body、喂解析器、带空闲看门狗。
pub(crate) struct EventStream {
    response: reqwest::Response,
    parser: SseParser,
}

impl EventStream {
    fn new(response: reqwest::Response) -> Self {
        Self {
            response,
            parser: SseParser::default(),
        }
    }

    /// 下一个事件的 JSON 载荷；流被云端正常关闭返回 `Ok(None)`。可在 `select!` 中取消重试：
    /// 已读入的半行留在解析器里，不丢数据。
    pub(crate) async fn next_data(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
        loop {
            if let Some(data) = self.parser.pop() {
                return Ok(Some(data));
            }
            let chunk = tokio::time::timeout(SSE_IDLE_TIMEOUT, self.response.chunk())
                .await
                .map_err(|_| StreamError::Idle)?
                .map_err(|error| StreamError::Read(error_chain(&error)))?;
            match chunk {
                Some(chunk) => self.parser.feed(&chunk)?,
                None => return Ok(None),
            }
        }
    }
}

fn error_chain(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// 提问成功后的响应：云端执行 ID（响应头）与事件流。
pub(crate) struct AskStream {
    pub(crate) run_id: Option<String>,
    pub(crate) events: EventStream,
}

// ------------------------------------------------------------------ 请求体

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionBody<'a> {
    channel: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_id: Option<&'a str>,
    client: ClientInfo<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientInfo<'a> {
    app_version: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    locale: Option<&'a str>,
    client_tools: Vec<ToolRef<'a>>,
    client_tool_protocol: u32,
}

#[derive(Serialize)]
struct ToolRef<'a> {
    name: &'a str,
    version: u32,
}

#[derive(Serialize)]
struct AskBody<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    locale: Option<&'a str>,
}

#[derive(Serialize)]
struct RatingBody<'a> {
    rating: Option<&'a str>,
    comment: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultBody<'a> {
    request_id: &'a str,
    output: &'a str,
    is_error: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolConfirmationBody<'a> {
    request_id: &'a str,
    approved: bool,
}

#[derive(Serialize)]
struct IssueEditBody<'a> {
    title: Option<&'a str>,
    body: Option<&'a str>,
}

// ------------------------------------------------------------------ 客户端

/// `/api/v1/ai/*` 调用面。`Clone` 很便宜（共享同一个 [`CloudApi`]）；一次执行内的全部请求用
/// [`Self::at_epoch`] 钉在提问时的账号会话上，账号切换后请求直接失败，不会落到另一个账号。
#[derive(Clone)]
pub(crate) struct AiCloud {
    api: CloudApi,
}

impl AiCloud {
    pub(crate) fn new(api: CloudApi) -> Self {
        Self { api }
    }

    pub(crate) fn request_epoch(&self) -> RequestEpoch {
        self.api.request_epoch()
    }

    pub(crate) fn at_epoch(&self, epoch: RequestEpoch) -> Self {
        Self {
            api: self.api.at_epoch(epoch),
        }
    }

    pub(crate) fn session_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.api.session_changes()
    }

    pub(crate) async fn is_authenticated(&self) -> bool {
        self.api.is_authenticated().await
    }

    pub(crate) async fn config(&self) -> Result<ConfigBody, CloudError> {
        self.api
            .ai_call(Method::GET, "/config", None::<&Value>)
            .await
    }

    /// 免鉴权的入口可见性（未登录时用）。
    pub(crate) async fn status(&self) -> Result<StatusBody, CloudError> {
        self.api.ai_status().await
    }

    /// 建会话并声明本机实现的客户端工具（云端只对声明了且协议一致的会话下发工具）。
    pub(crate) async fn create_session(
        &self,
        channel: &str,
        kind: &str,
        locale: Option<&str>,
        tools: &[(&'static str, u32)],
    ) -> Result<AssistantSessionDto, CloudError> {
        let (device_id, platform) = self.api.ai_device().await;
        let body = CreateSessionBody {
            channel,
            kind,
            device_id: Some(device_id.as_str()).filter(|id| !id.is_empty()),
            client: ClientInfo {
                app_version: fluxdown_protocol::APP_VERSION,
                platform: Some(platform.as_str()).filter(|platform| !platform.is_empty()),
                locale,
                client_tools: tools
                    .iter()
                    .map(|(name, version)| ToolRef {
                        name,
                        version: *version,
                    })
                    .collect(),
                client_tool_protocol: ASSISTANT_CLIENT_TOOL_PROTOCOL,
            },
        };
        self.api
            .ai_call(Method::POST, "/sessions", Some(&body))
            .await
    }

    pub(crate) async fn sessions(
        &self,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<AssistantSessionPage, CloudError> {
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        if let Some(offset) = offset {
            query.push(format!("offset={offset}"));
        }
        let suffix = if query.is_empty() {
            "/sessions".to_owned()
        } else {
            format!("/sessions?{}", query.join("&"))
        };
        self.api.ai_call(Method::GET, &suffix, None::<&Value>).await
    }

    pub(crate) async fn session(&self, id: &str) -> Result<AssistantSessionDetail, CloudError> {
        self.api
            .ai_call(
                Method::GET,
                &format!("/sessions/{}", encode_path_segment(id)),
                None::<&Value>,
            )
            .await
    }

    pub(crate) async fn delete_session(&self, id: &str) -> Result<(), CloudError> {
        self.api
            .ai_call(
                Method::DELETE,
                &format!("/sessions/{}", encode_path_segment(id)),
                None::<&Value>,
            )
            .await
    }

    /// 清空本人全部会话（云端只在用户侧隐藏）。
    pub(crate) async fn clear_sessions(&self) -> Result<AssistantClearedSessions, CloudError> {
        self.api
            .ai_call(Method::DELETE, "/sessions", None::<&Value>)
            .await
    }

    /// 评价一条回答（`message_id` 为 `assistant` 消息）。
    pub(crate) async fn rate(
        &self,
        session_id: &str,
        message_id: &str,
        rating: Option<&str>,
        comment: Option<&str>,
    ) -> Result<(), CloudError> {
        self.api
            .ai_call(
                Method::PUT,
                &format!(
                    "/sessions/{}/messages/{}/rating",
                    encode_path_segment(session_id),
                    encode_path_segment(message_id)
                ),
                Some(&RatingBody { rating, comment }),
            )
            .await
    }

    /// 提问：成功时响应头已到，事件流由调用方读取；准入失败（额度 / 限频 / 409 …）在此返回。
    pub(crate) async fn ask(
        &self,
        session_id: &str,
        text: &str,
        images: &[String],
        locale: Option<&str>,
    ) -> Result<AskStream, CloudError> {
        let body = AskBody {
            text,
            images: (!images.is_empty()).then_some(images),
            locale,
        };
        let response = self
            .api
            .ai_stream(
                &format!("/sessions/{}/messages", encode_path_segment(session_id)),
                &body,
            )
            .await?;
        let run_id = response
            .headers()
            .get(RUN_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(AskStream {
            run_id,
            events: EventStream::new(response),
        })
    }

    pub(crate) async fn cancel(&self, session_id: &str) -> Result<(), CloudError> {
        self.api
            .ai_call(
                Method::POST,
                &format!("/sessions/{}/cancel", encode_path_segment(session_id)),
                None::<&Value>,
            )
            .await
    }

    /// 回传用户对非只读客户端工具的确认。`Ok` = 云端仍在等（通过时随后执行并回传结果）；
    /// 404 = 已超时 / 已确认 / 不属于本人，客户端不得执行。
    pub(crate) async fn tool_confirmation(
        &self,
        run_id: &str,
        request_id: &str,
        approved: bool,
    ) -> Result<(), CloudError> {
        self.api
            .ai_call(
                Method::POST,
                &format!("/runs/{}/tool-confirmations", encode_path_segment(run_id)),
                Some(&ToolConfirmationBody {
                    request_id,
                    approved,
                }),
            )
            .await
    }

    /// 回传客户端工具结果（`output` 已脱敏）。404 = 云端已不再等待该结果。
    pub(crate) async fn tool_result(
        &self,
        run_id: &str,
        request_id: &str,
        output: &str,
        is_error: bool,
    ) -> Result<(), CloudError> {
        self.api
            .ai_call(
                Method::POST,
                &format!("/runs/{}/tool-results", encode_path_segment(run_id)),
                Some(&ToolResultBody {
                    request_id,
                    output,
                    is_error,
                }),
            )
            .await
    }

    pub(crate) async fn confirm_issue(
        &self,
        id: &str,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<AssistantIssueDto, CloudError> {
        self.api
            .ai_call(
                Method::POST,
                &format!("/issues/{}/confirm", encode_path_segment(id)),
                Some(&IssueEditBody { title, body }),
            )
            .await
    }

    pub(crate) async fn discard_issue(&self, id: &str) -> Result<AssistantIssueDto, CloudError> {
        self.api
            .ai_call(
                Method::POST,
                &format!("/issues/{}/discard", encode_path_segment(id)),
                None::<&Value>,
            )
            .await
    }
}
