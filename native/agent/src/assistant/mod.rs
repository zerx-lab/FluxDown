//! AI 助手（FluxCloud 代理的模型对话）的 agent 侧。
//!
//! - 云端调用（`/api/v1/ai/*`）在 [`cloud`]：agent 以登录账号提问，消费提问的 SSE 事件流。
//! - 本模块是状态机：把云端 `RunEvent` 映射进 `AssistantRunDto`，整体经 `AssistantChanged` 推送
//!   （`AgentSnapshot.assistant`）；云端下发的客户端工具（`clientToolCall`）在本机执行：只读工具直接
//!   执行，`network` / `write` 工具先挂起等用户经 `agent.assistant.confirm` 确认，确认回传云端
//!   （204）后才执行，执行结果再回传云端。工具实现在 [`tools`]。
//! - 一次执行 = 一个 [`RunSlot`]；同一会话同时只有一个进行中的执行。取消 / SSE 断开 / 账号登出或切换
//!   都会取消该执行的全部本机工具（工具令牌是执行令牌的子令牌）。
//! - 安全：云端是工具定义的唯一来源，但「是否需要确认」不能只信云端——`write` / `network` 类工具
//!   在本机另有最低副作用等级（[`effect_floor`]），云端把它们声明成 `read` 也照样先确认。

mod cloud;
mod tools;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fluxdown_protocol::{
    ASSISTANT_MAX_RUNS, AgentEvent, ApplicationErrorCode, AssistantAskParams, AssistantAskResult,
    AssistantClearedSessions, AssistantConfigDto, AssistantConfirmParams,
    AssistantConfirmationState, AssistantIssueDto, AssistantIssueParams, AssistantRateParams,
    AssistantRunDto, AssistantRunItemDto, AssistantRunStatus, AssistantSessionDetail,
    AssistantSessionIdParams, AssistantSessionPage, AssistantSessionsParams, AssistantStateDto,
    AssistantToolCallDto, AssistantToolEffect, AssistantToolStatus, ErrorReason,
};
use serde_json::Value;
use tokio::task::{JoinError, JoinSet};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::cloud::CloudApi;
use crate::event_hub::AgentEventHub;
use crate::gateway::GatewayService;
pub(crate) use cloud::AssistantError;
use cloud::{AiCloud, EventStream, RunEvent};
use tools::{ClientTools, ToolCall, ToolOutput};

/// 配置缓存有效期：过期后 `agent.assistant.get` 在后台刷新。
const CONFIG_TTL: Duration = Duration::from_secs(5 * 60);
/// 后台刷新配置失败后的最短重试间隔（`get` 被频繁轮询也不会反复打云端）。
const CONFIG_RETRY: Duration = Duration::from_secs(30);
/// 入口可见性的周期刷新间隔（后台开关改动最迟这么久后生效；失败按 [`CONFIG_RETRY`] 重试）。
const VISIBILITY_TTL: Duration = Duration::from_secs(30 * 60);
/// 文本增量的最长推送间隔：到达的 token 先攒着，避免每个增量都整体序列化一次状态。
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);
/// 云端未给出确认窗口时的兜底（与 FluxCloud `clientConfirmTimeoutSecs` 默认一致）。
const DEFAULT_CONFIRM_SECS: u32 = 120;
/// 工具结果摘要（`output_preview`）的字符上限。
const MAX_PREVIEW_CHARS: usize = 400;
/// 回传云端的工具输出上限（云端 512 KiB 拒收）；执行器本身已截断到 64 KiB，这里只是兜底。
const MAX_TOOL_OUTPUT_BYTES: usize = 500 * 1024;
/// 工具到期取消后，给执行器收尾的窗口。
const TOOL_CANCEL_GRACE: Duration = Duration::from_secs(5);
/// 工具结果回传的尝试次数与间隔（仅网络层可重试失败）。
const RESULT_ATTEMPTS: u32 = 3;
const RESULT_RETRY_DELAY: Duration = Duration::from_millis(500);
/// 后台任务的回收周期与关停收尾窗口。
const REAP_INTERVAL: Duration = Duration::from_secs(30);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

const KIND_CLIENT: &str = "client";

// ------------------------------------------------------------------ 工具执行器

/// 本机客户端工具执行器：生产实现是 [`ClientTools`]，测试用替身。
#[async_trait::async_trait]
pub(crate) trait ToolExecutor: Send + Sync {
    /// 本机实现的 `(name, version)`，建会话时声明给云端。
    fn declared(&self) -> Vec<(&'static str, u32)>;
    /// 执行一次调用；取消令牌触发后尽快返回。
    async fn execute(&self, call: ToolCall, cancel: CancellationToken) -> ToolOutput;
}

#[async_trait::async_trait]
impl ToolExecutor for ClientTools {
    fn declared(&self) -> Vec<(&'static str, u32)> {
        ClientTools::declared()
    }

    async fn execute(&self, call: ToolCall, cancel: CancellationToken) -> ToolOutput {
        ClientTools::execute(self, call, cancel).await
    }
}

/// 本机对工具副作用的最低判定：云端声明的等级低于它时按它处理（需要确认）。
fn effect_floor(name: &str) -> AssistantToolEffect {
    match name {
        "fd_measure_routes" => AssistantToolEffect::Network,
        "fd_create_tasks" | "fd_manage_tasks" | "fd_update_settings" => AssistantToolEffect::Write,
        _ => AssistantToolEffect::Read,
    }
}

fn effect_rank(effect: AssistantToolEffect) -> u8 {
    match effect {
        AssistantToolEffect::Read => 0,
        AssistantToolEffect::Network => 1,
        AssistantToolEffect::Write => 2,
    }
}

fn stricter(a: AssistantToolEffect, b: AssistantToolEffect) -> AssistantToolEffect {
    if effect_rank(a) >= effect_rank(b) {
        a
    } else {
        b
    }
}

// ------------------------------------------------------------------ 内部状态

type SlotId = u64;

/// 一次执行：对外的 DTO + 进行中时的控制面（结束后 `ctl` 为空）。
struct RunSlot {
    id: SlotId,
    dto: AssistantRunDto,
    ctl: Option<RunCtl>,
}

struct RunCtl {
    /// 取消这次执行：停 SSE 消费，并取消本机进行中的全部工具（工具令牌是它的子令牌）。
    cancel: CancellationToken,
    /// 钉在提问时账号会话上的云端客户端。
    cloud: AiCloud,
    /// 等待用户确认的非只读调用（`requestId` -> 调用）。
    pending: HashMap<String, PendingCall>,
}

/// 云端下发、待执行的客户端工具调用。
struct PendingCall {
    name: String,
    version: u32,
    arguments: Value,
    /// 确认后执行的时限（云端据此等待结果）。
    timeout: Duration,
    /// 确认窗口截止（收到事件起算）。
    deadline: Instant,
    /// 已有一次确认请求在途：并发的第二次确认直接拒绝。
    claimed: bool,
}

#[derive(Default)]
struct Inner {
    /// 云端开放了助手入口（`AssistantStateDto.visible`）；账号变化时保留，重新拉取后按需更新。
    visible: bool,
    config: Option<AssistantConfigDto>,
    config_fetched: Option<Instant>,
    config_attempted: Option<Instant>,
    /// 用户可见的配置拉取进行中（`AssistantStateDto.loading`）。
    loading: bool,
    last_error: Option<(Option<ErrorReason>, String)>,
    slots: Vec<RunSlot>,
}

impl Inner {
    fn to_dto(&self) -> AssistantStateDto {
        AssistantStateDto {
            visible: self.visible,
            config: self.config.clone(),
            loading: self.loading,
            last_error_reason: self.last_error.as_ref().and_then(|(reason, _)| *reason),
            last_error_message: self.last_error.as_ref().map(|(_, message)| message.clone()),
            runs: self.slots.iter().map(|slot| slot.dto.clone()).collect(),
        }
    }

    fn is_blank(&self) -> bool {
        self.config.is_none() && !self.loading && self.last_error.is_none() && self.slots.is_empty()
    }

    fn config_due(&self, now: Instant) -> bool {
        self.config_fetched
            .is_none_or(|fetched| now.duration_since(fetched) >= CONFIG_TTL)
            && self
                .config_attempted
                .is_none_or(|attempted| now.duration_since(attempted) >= CONFIG_RETRY)
    }

    /// 新执行入列前，裁掉最旧的已结束执行，保证不超过 [`ASSISTANT_MAX_RUNS`]；进行中的永不被裁。
    fn make_room(&mut self) {
        while self.slots.len() >= ASSISTANT_MAX_RUNS {
            match self.slots.iter().position(|slot| slot.ctl.is_none()) {
                Some(index) => {
                    self.slots.remove(index);
                }
                None => break,
            }
        }
    }
}

impl RunSlot {
    fn tool_mut(&mut self, request_id: &str) -> Option<&mut AssistantToolCallDto> {
        self.dto.items.iter_mut().find_map(|item| match item {
            AssistantRunItemDto::Tool { call }
                if call.request_id.as_deref() == Some(request_id) =>
            {
                Some(call)
            }
            _ => None,
        })
    }

    /// 确认窗口到期：调用未执行，云端按「未执行」交还模型。已不在等待返回 `false`。
    fn expire(&mut self, request_id: &str) -> bool {
        let removed = self
            .ctl
            .as_mut()
            .is_some_and(|ctl| ctl.pending.remove(request_id).is_some());
        if let Some(call) = self.tool_mut(request_id)
            && call.confirmation == Some(AssistantConfirmationState::Pending)
        {
            call.confirmation = Some(AssistantConfirmationState::Expired);
            call.status = AssistantToolStatus::Failed;
        }
        removed
    }

    /// 结束这次执行：写入终态，收尾未完成的工具 / 确认，取消本机进行中的工具。
    fn finish(&mut self, status: AssistantRunStatus, error: Option<String>) {
        self.dto.status = status;
        if error.is_some() {
            self.dto.error = error;
        }
        let leftover = if matches!(
            status,
            AssistantRunStatus::Completed | AssistantRunStatus::Replied
        ) {
            AssistantToolStatus::Succeeded
        } else {
            AssistantToolStatus::Failed
        };
        for item in &mut self.dto.items {
            if let AssistantRunItemDto::Tool { call } = item {
                if call.confirmation == Some(AssistantConfirmationState::Pending) {
                    call.confirmation = Some(AssistantConfirmationState::Expired);
                }
                if call.status == AssistantToolStatus::Running {
                    call.status = leftover;
                }
            }
        }
        if let Some(ctl) = self.ctl.take() {
            ctl.cancel.cancel();
        }
    }

    fn mark_running(&mut self) {
        if self.dto.status == AssistantRunStatus::Starting {
            self.dto.status = AssistantRunStatus::Running;
        }
    }
}

/// 一个事件应用到状态后，消费循环要做的后续动作。
enum Action {
    /// 执行已不存在（登出 / 会话切换 / 被删除）。
    Gone,
    Unchanged,
    /// 状态已改；`urgent = false`（文本增量）时推送可以稍后合并。
    Changed {
        urgent: bool,
    },
    /// 只读工具：立即在本机执行。
    Execute {
        request_id: String,
        call: PendingCall,
    },
    /// 非只读工具已挂起等用户确认：启动到期计时。
    AwaitConfirm {
        request_id: String,
        window: Duration,
    },
    /// 执行已到终态。
    Finished,
}

/// 把一个云端事件映射进执行状态。
fn apply_event(slot: &mut RunSlot, event: RunEvent, now: Instant) -> Action {
    if slot.ctl.is_none() {
        return Action::Unchanged;
    }
    match event {
        RunEvent::Run { run_id } => {
            slot.dto.run_id = Some(run_id);
            slot.mark_running();
            Action::Changed { urgent: true }
        }
        RunEvent::Text { delta } => {
            if delta.is_empty() {
                return Action::Unchanged;
            }
            slot.mark_running();
            match slot.dto.items.last_mut() {
                Some(AssistantRunItemDto::Text { text }) => text.push_str(&delta),
                _ => slot
                    .dto
                    .items
                    .push(AssistantRunItemDto::Text { text: delta }),
            }
            Action::Changed { urgent: false }
        }
        RunEvent::TurnRetried => {
            // 丢弃本轮已显示的文本（本轮 = 最后一次工具调用之后的那段）。
            if matches!(
                slot.dto.items.last(),
                Some(AssistantRunItemDto::Text { .. })
            ) {
                slot.dto.items.pop();
            }
            Action::Changed { urgent: true }
        }
        RunEvent::ToolStarted {
            call_id,
            name,
            kind,
        } => {
            slot.mark_running();
            ensure_tool(&mut slot.dto.items, &call_id, &name, &kind);
            Action::Changed { urgent: true }
        }
        RunEvent::ToolFinished {
            call_id,
            name,
            kind,
            is_error,
            duration_ms,
        } => {
            let mut request_id = None;
            if let Some(call) = ensure_tool(&mut slot.dto.items, &call_id, &name, &kind) {
                call.status = if is_error {
                    AssistantToolStatus::Failed
                } else {
                    AssistantToolStatus::Succeeded
                };
                call.duration_ms = duration_ms;
                if call.confirmation == Some(AssistantConfirmationState::Pending) {
                    call.confirmation = Some(AssistantConfirmationState::Expired);
                }
                request_id.clone_from(&call.request_id);
            }
            // 云端已结束这次调用（含确认超时）：不再等用户确认。
            if let (Some(request_id), Some(ctl)) = (request_id, slot.ctl.as_mut()) {
                ctl.pending.remove(&request_id);
            }
            Action::Changed { urgent: true }
        }
        RunEvent::ClientToolCall {
            request_id,
            name,
            version,
            arguments,
            effect,
            confirm_timeout_secs,
            timeout_secs,
        } => apply_client_call(
            slot,
            now,
            request_id,
            name,
            version,
            arguments,
            effect,
            confirm_timeout_secs,
            timeout_secs,
        ),
        RunEvent::IssueDraft { issue } => {
            let issue = *issue;
            let existing = slot.dto.items.iter_mut().find(|item| {
                matches!(item, AssistantRunItemDto::IssueDraft { issue: known } if known.id == issue.id)
            });
            match existing {
                Some(item) => *item = AssistantRunItemDto::IssueDraft { issue },
                None => slot
                    .dto
                    .items
                    .push(AssistantRunItemDto::IssueDraft { issue }),
            }
            Action::Changed { urgent: true }
        }
        RunEvent::Done {
            status,
            message_id,
            text,
            input_tokens,
            output_tokens,
        } => {
            // 固定回复等路径可能不推文本增量：用最终文本补齐。
            let has_text = slot
                .dto
                .items
                .iter()
                .any(|item| matches!(item, AssistantRunItemDto::Text { .. }));
            if !has_text && !text.is_empty() {
                slot.dto.items.push(AssistantRunItemDto::Text { text });
            }
            slot.dto.message_id = message_id;
            slot.dto.input_tokens = input_tokens;
            slot.dto.output_tokens = output_tokens;
            let status = if status == "replied" {
                AssistantRunStatus::Replied
            } else {
                AssistantRunStatus::Completed
            };
            slot.finish(status, None);
            Action::Finished
        }
        RunEvent::Error { status, message } => {
            let status = if status == "canceled" {
                AssistantRunStatus::Canceled
            } else {
                AssistantRunStatus::Failed
            };
            slot.finish(status, Some(message));
            Action::Finished
        }
        RunEvent::Other => Action::Unchanged,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "一个云端事件的全部字段，拆成结构体只是搬运"
)]
fn apply_client_call(
    slot: &mut RunSlot,
    now: Instant,
    request_id: String,
    name: String,
    version: u32,
    arguments: Value,
    effect: AssistantToolEffect,
    confirm_timeout_secs: Option<u32>,
    timeout_secs: u32,
) -> Action {
    slot.mark_running();
    let effect = stricter(effect, effect_floor(&name));
    // 非只读（云端声明或本机判定）或云端给了确认窗口：都必须先确认。
    let window = (effect.needs_confirmation() || confirm_timeout_secs.is_some()).then(|| {
        Duration::from_secs(u64::from(
            confirm_timeout_secs.unwrap_or(DEFAULT_CONFIRM_SECS),
        ))
    });
    let items = &mut slot.dto.items;
    // 云端先发 `toolStarted`（展示）再发 `clientToolCall`（请求）；乱序时也要对得上。
    let index = items
        .iter()
        .position(|item| {
            matches!(item, AssistantRunItemDto::Tool { call }
                if call.kind == KIND_CLIENT
                    && call.name == name
                    && call.request_id.is_none()
                    && call.status == AssistantToolStatus::Running)
        })
        .unwrap_or_else(|| {
            items.push(AssistantRunItemDto::Tool {
                call: AssistantToolCallDto {
                    // 占位：`toolStarted` 随后到达时认领并换成真实 callId。
                    call_id: request_id.clone(),
                    name: name.clone(),
                    kind: KIND_CLIENT.to_owned(),
                    ..AssistantToolCallDto::default()
                },
            });
            items.len() - 1
        });
    let Some(AssistantRunItemDto::Tool { call }) = items.get_mut(index) else {
        return Action::Unchanged;
    };
    call.request_id = Some(request_id.clone());
    call.effect = effect;
    call.arguments = Some(arguments.clone());
    call.confirmation = window.map(|_| AssistantConfirmationState::Pending);
    call.confirm_deadline_unix_ms = window.map(|window| {
        now_unix_ms().saturating_add(i64::try_from(window.as_millis()).unwrap_or(i64::MAX))
    });
    let pending = PendingCall {
        name,
        version,
        arguments,
        timeout: Duration::from_secs(u64::from(timeout_secs.max(1))),
        deadline: now + window.unwrap_or_default(),
        claimed: false,
    };
    match (window, slot.ctl.as_mut()) {
        (Some(window), Some(ctl)) => {
            ctl.pending.insert(request_id.clone(), pending);
            Action::AwaitConfirm { request_id, window }
        }
        (None, Some(_)) => Action::Execute {
            request_id,
            call: pending,
        },
        (_, None) => Action::Unchanged,
    }
}

/// 按 `callId` 找工具条目，找不到则认领客户端工具的占位条目（`clientToolCall` 先到的乱序），
/// 再找不到就新增一条运行中的条目。
fn ensure_tool<'a>(
    items: &'a mut Vec<AssistantRunItemDto>,
    call_id: &str,
    name: &str,
    kind: &str,
) -> Option<&'a mut AssistantToolCallDto> {
    let mut index = items.iter().position(
        |item| matches!(item, AssistantRunItemDto::Tool { call } if call.call_id == call_id),
    );
    if index.is_none() && kind == KIND_CLIENT {
        index = items.iter().position(|item| {
            matches!(item, AssistantRunItemDto::Tool { call }
                if call.kind == KIND_CLIENT
                    && call.name == name
                    && call.request_id.as_deref() == Some(call.call_id.as_str()))
        });
        if let Some(AssistantRunItemDto::Tool { call }) = index.and_then(|i| items.get_mut(i)) {
            call.call_id = call_id.to_owned();
        }
    }
    let index = index.unwrap_or_else(|| {
        items.push(AssistantRunItemDto::Tool {
            call: AssistantToolCallDto {
                call_id: call_id.to_owned(),
                name: name.to_owned(),
                kind: kind.to_owned(),
                ..AssistantToolCallDto::default()
            },
        });
        items.len() - 1
    });
    match items.get_mut(index) {
        Some(AssistantRunItemDto::Tool { call }) => Some(call),
        _ => None,
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// 工具输出过长时在字符边界处截断（云端 512 KiB 拒收）。
fn clamp_output(mut output: String) -> String {
    if output.len() > MAX_TOOL_OUTPUT_BYTES {
        let mut end = MAX_TOOL_OUTPUT_BYTES;
        while !output.is_char_boundary(end) {
            end -= 1;
        }
        output.truncate(end);
        output.push_str("\n[output truncated]");
    }
    output
}

fn preview_of(output: &str) -> String {
    let mut preview: String = output.chars().take(MAX_PREVIEW_CHARS).collect();
    if output.chars().nth(MAX_PREVIEW_CHARS).is_some() {
        preview.push('…');
    }
    preview
}

/// 需要在本机执行的一次工具调用。
struct ToolRun {
    slot_id: SlotId,
    request_id: String,
    run_id: Option<String>,
    cloud: AiCloud,
    /// 所属执行的令牌：它被取消 = 云端已不再等这次结果。
    run_cancel: CancellationToken,
    call: PendingCall,
}

// ------------------------------------------------------------------ 服务

/// AI 助手服务：配置缓存、执行状态机、客户端工具执行与确认。
pub(crate) struct AssistantService {
    cloud: AiCloud,
    events: AgentEventHub,
    /// 整个 agent 的关停令牌；每次执行的令牌是它的子令牌。
    cancel: CancellationToken,
    tools: Mutex<Option<Arc<dyn ToolExecutor>>>,
    inner: Mutex<Inner>,
    /// 配置拉取串行化。
    refresh_gate: tokio::sync::Mutex<()>,
    /// 后台任务（SSE 消费、工具执行、到期计时、配置刷新）：关停时统一收回。
    tasks: Mutex<JoinSet<()>>,
    next_slot: AtomicU64,
}

impl AssistantService {
    #[must_use]
    pub(crate) fn new(cloud: CloudApi, events: AgentEventHub, cancel: CancellationToken) -> Self {
        Self {
            cloud: AiCloud::new(cloud),
            events,
            cancel,
            tools: Mutex::new(None),
            inner: Mutex::new(Inner::default()),
            refresh_gate: tokio::sync::Mutex::new(()),
            tasks: Mutex::new(JoinSet::new()),
            next_slot: AtomicU64::new(1),
        }
    }

    /// 装配本机工具执行器（需要网关：工具经网关调用 daemon / agent 的本机 RPC）。
    /// 网关与服务互相持有，循环由 [`Self::run`] 退出时断开。
    pub(crate) fn attach_tools(&self, gateway: Arc<GatewayService>) {
        self.set_executor(Arc::new(ClientTools::new(gateway, self.events.clone())));
    }

    fn set_executor(&self, executor: Arc<dyn ToolExecutor>) {
        *lock(&self.tools) = Some(executor);
    }

    fn executor(&self) -> Option<Arc<dyn ToolExecutor>> {
        lock(&self.tools).clone()
    }

    fn declared(&self) -> Vec<(&'static str, u32)> {
        self.executor()
            .map(|executor| executor.declared())
            .unwrap_or_default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// 整体推送当前状态（在持锁时调用，保证推送顺序与状态变更顺序一致）。
    fn publish(&self, inner: &Inner) {
        self.events
            .publish(AgentEvent::AssistantChanged(inner.to_dto()));
    }

    fn flush(&self) {
        let inner = self.lock();
        self.publish(&inner);
    }

    fn state(&self) -> AssistantStateDto {
        self.lock().to_dto()
    }

    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        if self.cancel.is_cancelled() {
            return;
        }
        lock(&self.tasks).spawn(future);
    }

    // ---------------------------------------------------------------- 后台循环

    /// 后台循环：响应账号会话变化（登出 / 切换 = 取消全部执行并清空状态）、回收已结束的任务、
    /// 刷新入口可见性（启动即拉、账号变化后重拉、之后按周期）；关停时收回全部任务并断开与网关的循环引用。
    pub(crate) async fn run(self: Arc<Self>) {
        let mut session = self.cloud.session_changes();
        session.borrow_and_update();
        let mut reap = tokio::time::interval(REAP_INTERVAL);
        let mut visibility_due = Instant::now();
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => break,
                changed = session.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    session.borrow_and_update();
                    self.reset();
                    visibility_due = Instant::now();
                }
                () = tokio::time::sleep_until(visibility_due) => {
                    let fresh = tokio::select! {
                        () = self.cancel.cancelled() => break,
                        fresh = self.refresh_visibility() => fresh,
                    };
                    visibility_due =
                        Instant::now() + if fresh { VISIBILITY_TTL } else { CONFIG_RETRY };
                }
                _ = reap.tick() => self.reap(),
            }
        }
        self.shutdown().await;
    }

    /// 取消全部执行并清空状态（账号登出 / 切换）。
    fn reset(&self) {
        let mut inner = self.lock();
        let was_blank = inner.is_blank();
        for slot in inner.slots.drain(..) {
            if let Some(ctl) = slot.ctl {
                ctl.cancel.cancel();
            }
        }
        *inner = Inner {
            visible: inner.visible,
            ..Inner::default()
        };
        if !was_blank {
            self.publish(&inner);
        }
    }

    fn reap(&self) {
        let finished = {
            let mut tasks = lock(&self.tasks);
            let mut finished = Vec::new();
            while let Some(result) = tasks.try_join_next() {
                finished.push(result);
            }
            finished
        };
        for result in finished {
            log_join(result);
        }
    }

    async fn shutdown(&self) {
        self.reset();
        let mut tasks = std::mem::take(&mut *lock(&self.tasks));
        let drained = tokio::time::timeout(SHUTDOWN_GRACE, async {
            while let Some(result) = tasks.join_next().await {
                log_join(result);
            }
        })
        .await;
        if drained.is_err() {
            tasks.shutdown().await;
        }
        *lock(&self.tools) = None;
    }

    /// 刷新入口可见性，返回是否拿到了云端结果。已登录走 `ai/config`（顺带刷新配置缓存），
    /// 未登录走免鉴权 `ai/status`；失败保留上次值。只在值变化时推送。
    async fn refresh_visibility(&self) -> bool {
        if self.cloud.is_authenticated().await {
            return match self.fetch_config(false).await {
                Ok(()) => true,
                Err(error) => {
                    tracing::debug!(error = %error.message, "assistant visibility refresh failed");
                    false
                }
            };
        }
        let epoch = self.cloud.request_epoch();
        let status = match self.cloud.status().await {
            Ok(status) => status,
            Err(error) => {
                tracing::debug!(error = %error.message, "assistant status refresh failed");
                return false;
            }
        };
        let mut inner = self.lock();
        if self.cloud.request_epoch() != epoch {
            // 拉取期间登录了：匿名结果作废，账号变化会触发重新拉取。
            return false;
        }
        if inner.visible != status.visible {
            inner.visible = status.visible;
            self.publish(&inner);
        }
        true
    }

    // ---------------------------------------------------------------- 配置

    /// 当前状态（读缓存）；配置缺失 / 过期且已登录时在后台刷新并经 `AssistantChanged` 推送。
    pub(crate) async fn get(self: &Arc<Self>) -> AssistantStateDto {
        let (state, due) = {
            let inner = self.lock();
            (inner.to_dto(), inner.config_due(Instant::now()))
        };
        if due && self.cloud.is_authenticated().await {
            let service = Arc::clone(self);
            self.spawn(async move {
                if let Err(error) = service.fetch_config(false).await {
                    tracing::debug!(error = %error.message, "assistant config refresh failed");
                }
            });
        }
        state
    }

    /// 立即向 FluxCloud 拉取配置。
    pub(crate) async fn refresh(&self) -> Result<AssistantStateDto, AssistantError> {
        self.fetch_config(true).await?;
        Ok(self.state())
    }

    /// 拉取配置并更新投影。`visible` 时置 `loading` 并等待其他拉取结束；后台刷新遇到拉取在途直接跳过。
    async fn fetch_config(&self, visible: bool) -> Result<(), AssistantError> {
        let _gate = if visible {
            self.refresh_gate.lock().await
        } else {
            let Ok(gate) = self.refresh_gate.try_lock() else {
                return Ok(());
            };
            gate
        };
        let epoch = self.cloud.request_epoch();
        {
            let mut inner = self.lock();
            inner.config_attempted = Some(Instant::now());
            if visible {
                inner.loading = true;
                self.publish(&inner);
            }
        }
        let result = self.cloud.at_epoch(epoch).config().await;
        let declared = self.declared();
        let mut inner = self.lock();
        if self.cloud.request_epoch() != epoch {
            // 拉取期间账号变了：结果属于已结束的会话，丢弃（状态已被重置）。
            inner.loading = false;
            return Err(AssistantError::conflict(
                "The account changed while the assistant configuration was loading.",
            ));
        }
        inner.loading = false;
        let outcome = match result {
            Ok(body) => {
                inner.visible = body.visible();
                inner.config = Some(body.into_dto(&declared));
                inner.config_fetched = Some(Instant::now());
                inner.last_error = None;
                Ok(())
            }
            Err(error) => {
                let error = AssistantError::from(error);
                if error.data.code == ApplicationErrorCode::Unauthorized {
                    inner.config = None;
                }
                inner.last_error = Some((error.reason(), error.message.clone()));
                Err(error)
            }
        };
        self.publish(&inner);
        outcome
    }

    /// 记录一次提问 / 建会话失败（`last_error_*`），原样返回错误。
    fn record_error(&self, error: AssistantError) -> AssistantError {
        let mut inner = self.lock();
        inner.last_error = Some((error.reason(), error.message.clone()));
        self.publish(&inner);
        error
    }

    // ---------------------------------------------------------------- 云端会话

    pub(crate) async fn sessions(
        &self,
        params: AssistantSessionsParams,
    ) -> Result<AssistantSessionPage, AssistantError> {
        Ok(self.cloud.sessions(params.limit, params.offset).await?)
    }

    pub(crate) async fn session(
        &self,
        params: AssistantSessionIdParams,
    ) -> Result<AssistantSessionDetail, AssistantError> {
        let id = require_id("sessionId", &params.session_id)?;
        Ok(self.cloud.session(id).await?)
    }

    /// 删除会话（云端只在用户侧隐藏）：本机该会话进行中的执行先取消，删除成功后本机相关执行一并清掉。
    pub(crate) async fn delete_session(
        &self,
        params: AssistantSessionIdParams,
    ) -> Result<(), AssistantError> {
        let id = require_id("sessionId", &params.session_id)?;
        self.cancel_local(id);
        self.cloud.delete_session(id).await?;
        let mut inner = self.lock();
        inner.slots.retain(|slot| slot.dto.session_id != id);
        self.publish(&inner);
        Ok(())
    }

    /// 清空本人全部会话（云端只在用户侧隐藏）：本机进行中的执行全部先取消，成功后本机执行记录清空。
    pub(crate) async fn clear_sessions(&self) -> Result<AssistantClearedSessions, AssistantError> {
        {
            let mut inner = self.lock();
            for slot in inner.slots.iter_mut().filter(|slot| slot.ctl.is_some()) {
                slot.finish(AssistantRunStatus::Canceled, None);
            }
            self.publish(&inner);
        }
        let cleared = self.cloud.clear_sessions().await?;
        let mut inner = self.lock();
        inner.slots.clear();
        self.publish(&inner);
        Ok(cleared)
    }

    pub(crate) async fn rate(&self, params: AssistantRateParams) -> Result<(), AssistantError> {
        let session_id = require_id("sessionId", &params.session_id)?;
        let message_id = require_id("messageId", &params.message_id)?;
        Ok(self
            .cloud
            .rate(
                session_id,
                message_id,
                params.rating.as_deref(),
                params.comment.as_deref(),
            )
            .await?)
    }

    pub(crate) async fn confirm_issue(
        &self,
        params: AssistantIssueParams,
    ) -> Result<AssistantIssueDto, AssistantError> {
        let id = require_id("issueId", &params.issue_id)?;
        let issue = self
            .cloud
            .confirm_issue(id, params.title.as_deref(), params.body.as_deref())
            .await?;
        self.sync_issue(&issue);
        Ok(issue)
    }

    pub(crate) async fn discard_issue(
        &self,
        params: AssistantIssueParams,
    ) -> Result<(), AssistantError> {
        let id = require_id("issueId", &params.issue_id)?;
        let issue = self.cloud.discard_issue(id).await?;
        self.sync_issue(&issue);
        Ok(())
    }

    /// 问题单被确认 / 放弃后，把执行里的草稿卡片同步成云端返回的最新状态。
    fn sync_issue(&self, issue: &AssistantIssueDto) {
        let mut inner = self.lock();
        let mut changed = false;
        for slot in &mut inner.slots {
            for item in &mut slot.dto.items {
                if let AssistantRunItemDto::IssueDraft { issue: known } = item
                    && known.id == issue.id
                {
                    known.clone_from(issue);
                    changed = true;
                }
            }
        }
        if changed {
            self.publish(&inner);
        }
    }

    // ---------------------------------------------------------------- 提问

    /// 提问：无 `sessionId` 先建会话；云端准入（额度 / 限频 / 会话忙…）失败同步返回，成功后执行
    /// 在后台消费 SSE，状态经 `AssistantChanged` 推送。
    pub(crate) async fn ask(
        self: &Arc<Self>,
        params: AssistantAskParams,
    ) -> Result<AssistantAskResult, AssistantError> {
        let text = params.text.trim();
        if text.is_empty() {
            return Err(AssistantError::invalid_field(
                "text",
                "The question is empty.",
            ));
        }
        let Some(executor) = self.executor() else {
            return Err(AssistantError::unavailable(
                "The assistant's local tools are not ready yet.",
            ));
        };
        let locale = params
            .locale
            .as_deref()
            .map(str::trim)
            .filter(|locale| !locale.is_empty());
        // 整次执行（建会话 / 提问 / 确认 / 回传结果）钉在提问时的账号会话上。
        let cloud = self.cloud.at_epoch(self.cloud.request_epoch());
        let session_id = match params
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            Some(id) => id.to_owned(),
            None => {
                let created = cloud
                    .create_session(
                        params.channel.as_str(),
                        params.kind.as_str(),
                        locale,
                        &executor.declared(),
                    )
                    .await
                    .map_err(|error| self.record_error(error.into()))?;
                created.id
            }
        };
        let (slot_id, cancel) = self.begin_run(&session_id, text, cloud.clone())?;
        let stream = match cloud.ask(&session_id, text, &params.images, locale).await {
            Ok(stream) => stream,
            Err(error) => {
                self.discard_run(slot_id);
                return Err(self.record_error(error.into()));
            }
        };
        if let Some(run_id) = &stream.run_id {
            self.with_slot(slot_id, |slot| slot.dto.run_id = Some(run_id.clone()));
        }
        if cancel.is_cancelled() {
            // 提问在途时被取消：状态已由取消方写好；丢弃响应流，云端据断开取消执行。
            tracing::debug!(%session_id, "assistant run cancelled while the question was in flight");
            return Ok(AssistantAskResult { session_id });
        }
        let service = Arc::clone(self);
        self.spawn(service.consume(slot_id, stream.events, cloud, cancel, stream.run_id));
        Ok(AssistantAskResult { session_id })
    }

    /// 登记一次新的执行（`Starting`）；同一会话已有进行中的执行则冲突。
    fn begin_run(
        &self,
        session_id: &str,
        question: &str,
        cloud: AiCloud,
    ) -> Result<(SlotId, CancellationToken), AssistantError> {
        let mut inner = self.lock();
        if inner
            .slots
            .iter()
            .any(|slot| slot.ctl.is_some() && slot.dto.session_id == session_id)
        {
            return Err(AssistantError::conflict(
                "This conversation is already answering a question.",
            ));
        }
        let id = self.next_slot.fetch_add(1, Ordering::Relaxed);
        let cancel = self.cancel.child_token();
        inner.make_room();
        inner.slots.push(RunSlot {
            id,
            dto: AssistantRunDto {
                session_id: session_id.to_owned(),
                status: AssistantRunStatus::Starting,
                question: question.to_owned(),
                started_at_unix_ms: now_unix_ms(),
                ..AssistantRunDto::default()
            },
            ctl: Some(RunCtl {
                cancel: cancel.clone(),
                cloud,
                pending: HashMap::new(),
            }),
        });
        inner.last_error = None;
        self.publish(&inner);
        Ok((id, cancel))
    }

    /// 提问被云端拒绝：撤掉刚登记的执行。
    fn discard_run(&self, slot_id: SlotId) {
        let mut inner = self.lock();
        inner.slots.retain(|slot| slot.id != slot_id);
        self.publish(&inner);
    }

    /// 修改一次执行并推送；执行已不存在返回 `false`。
    fn with_slot(&self, slot_id: SlotId, change: impl FnOnce(&mut RunSlot)) -> bool {
        let mut inner = self.lock();
        let Some(slot) = inner.slots.iter_mut().find(|slot| slot.id == slot_id) else {
            return false;
        };
        change(slot);
        self.publish(&inner);
        true
    }

    /// 应用一个云端事件；除「稍后合并推送」的文本增量外，状态变化立即推送。
    fn apply(&self, slot_id: SlotId, event: RunEvent) -> Action {
        let mut inner = self.lock();
        let Some(slot) = inner.slots.iter_mut().find(|slot| slot.id == slot_id) else {
            return Action::Gone;
        };
        let action = apply_event(slot, event, Instant::now());
        if !matches!(
            action,
            Action::Unchanged | Action::Changed { urgent: false }
        ) {
            self.publish(&inner);
        }
        action
    }

    /// 以失败 / 取消 / 完成终态收尾；已结束的执行返回 `false`。
    fn finish(&self, slot_id: SlotId, status: AssistantRunStatus, error: Option<String>) -> bool {
        let mut inner = self.lock();
        let Some(slot) = inner
            .slots
            .iter_mut()
            .find(|slot| slot.id == slot_id && slot.ctl.is_some())
        else {
            return false;
        };
        slot.finish(status, error);
        self.publish(&inner);
        true
    }

    /// 消费一次提问的 SSE，直到终态、断开或被取消。
    async fn consume(
        self: Arc<Self>,
        slot_id: SlotId,
        mut events: EventStream,
        cloud: AiCloud,
        cancel: CancellationToken,
        mut run_id: Option<String>,
    ) {
        let mut flush_at: Option<Instant> = None;
        let refresh = loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                () = tokio::time::sleep_until(flush_at.unwrap_or_else(Instant::now)),
                    if flush_at.is_some() =>
                {
                    flush_at = None;
                    self.flush();
                    continue;
                }
                next = events.next_data() => next,
            };
            let payload = match next {
                Ok(Some(payload)) => payload,
                Ok(None) => {
                    self.finish(
                        slot_id,
                        AssistantRunStatus::Failed,
                        Some(
                            "FluxCloud closed the event stream before the run finished.".to_owned(),
                        ),
                    );
                    break true;
                }
                Err(error) => {
                    self.finish(slot_id, AssistantRunStatus::Failed, Some(error.to_string()));
                    break true;
                }
            };
            let event = match cloud::parse_event(&payload) {
                Ok(event) => event,
                Err(error) => {
                    tracing::debug!(error = %error, "assistant ignored an unreadable event frame");
                    continue;
                }
            };
            if let RunEvent::Run { run_id: id } = &event {
                run_id = Some(id.clone());
            }
            match self.apply(slot_id, event) {
                Action::Gone => break false,
                Action::Finished => break true,
                Action::Unchanged => {}
                Action::Changed { urgent: true } => flush_at = None,
                Action::Changed { urgent: false } => {
                    flush_at.get_or_insert_with(|| Instant::now() + FLUSH_INTERVAL);
                }
                Action::Execute { request_id, call } => {
                    flush_at = None;
                    self.spawn_tool(ToolRun {
                        slot_id,
                        request_id,
                        run_id: run_id.clone(),
                        cloud: cloud.clone(),
                        run_cancel: cancel.clone(),
                        call,
                    });
                }
                Action::AwaitConfirm { request_id, window } => {
                    flush_at = None;
                    self.spawn_expiry(slot_id, request_id, window, cancel.clone());
                }
            }
        };
        if refresh {
            // 本次提问计入当日额度：静默刷新配置，让 `usedToday` 跟上。
            let service = Arc::clone(&self);
            self.spawn(async move {
                if let Err(error) = service.fetch_config(false).await {
                    tracing::debug!(error = %error.message, "assistant config refresh failed");
                }
            });
        }
    }

    // ---------------------------------------------------------------- 取消

    /// 取消会话中进行中的执行：本机立即收尾（停 SSE、取消进行中的工具），并通知云端。
    pub(crate) async fn cancel(
        &self,
        params: AssistantSessionIdParams,
    ) -> Result<(), AssistantError> {
        let id = require_id("sessionId", &params.session_id)?;
        let cloud = self.cancel_local(id).unwrap_or_else(|| self.cloud.clone());
        cloud.cancel(id).await?;
        Ok(())
    }

    /// 本机侧取消：会话里进行中的执行标为已取消并取消其令牌。返回该执行的云端客户端。
    fn cancel_local(&self, session_id: &str) -> Option<AiCloud> {
        let mut inner = self.lock();
        let slot = inner
            .slots
            .iter_mut()
            .find(|slot| slot.ctl.is_some() && slot.dto.session_id == session_id)?;
        let cloud = slot.ctl.as_ref().map(|ctl| ctl.cloud.clone());
        slot.finish(AssistantRunStatus::Canceled, None);
        self.publish(&inner);
        cloud
    }

    // ---------------------------------------------------------------- 确认与工具执行

    /// 用户批准 / 拒绝一次非只读客户端工具调用：先把决定回传云端（204 = 云端仍在等），批准才执行。
    pub(crate) async fn confirm(
        self: &Arc<Self>,
        params: AssistantConfirmParams,
    ) -> Result<(), AssistantError> {
        let request_id = require_id("requestId", &params.request_id)?;
        let approved = params.approved;
        let (slot_id, run_id, cloud) = self.claim_confirmation(request_id)?;
        let decision = cloud.tool_confirmation(&run_id, request_id, approved).await;
        let mut inner = self.lock();
        let Some(slot) = inner.slots.iter_mut().find(|slot| slot.id == slot_id) else {
            // 执行已不存在（取消 / 登出）：没有可执行的东西。
            return decision.map_err(AssistantError::from);
        };
        match decision {
            Ok(()) => {
                let pending = slot
                    .ctl
                    .as_mut()
                    .and_then(|ctl| ctl.pending.remove(request_id));
                let Some(pending) = pending else {
                    // 确认在途时执行已结束 / 云端已结束这次调用：不执行。
                    return Ok(());
                };
                if let Some(call) = slot.tool_mut(request_id) {
                    if approved {
                        call.confirmation = Some(AssistantConfirmationState::Approved);
                    } else {
                        call.confirmation = Some(AssistantConfirmationState::Declined);
                        call.status = AssistantToolStatus::Failed;
                        call.output_preview = Some("Declined by the user.".to_owned());
                    }
                }
                let run = slot.ctl.as_ref().map(|ctl| ToolRun {
                    slot_id,
                    request_id: request_id.to_owned(),
                    run_id: Some(run_id),
                    cloud: ctl.cloud.clone(),
                    run_cancel: ctl.cancel.clone(),
                    call: pending,
                });
                self.publish(&inner);
                drop(inner);
                if approved && let Some(run) = run {
                    self.spawn_tool(run);
                }
                Ok(())
            }
            Err(error) if error.status == Some(404) => {
                // 云端已不再等这次确认（超时 / 已结束）：绝不执行。
                slot.expire(request_id);
                self.publish(&inner);
                Err(AssistantError::conflict(
                    "The confirmation window has expired; the action was not performed.",
                ))
            }
            Err(error) => {
                // 决定没送达：恢复待确认，用户可以重试。
                if let Some(pending) = slot
                    .ctl
                    .as_mut()
                    .and_then(|ctl| ctl.pending.get_mut(request_id))
                {
                    pending.claimed = false;
                }
                Err(error.into())
            }
        }
    }

    /// 认领一个待确认调用（并发的第二次确认拒绝；窗口已过则就地过期）。
    fn claim_confirmation(
        &self,
        request_id: &str,
    ) -> Result<(SlotId, String, AiCloud), AssistantError> {
        let mut inner = self.lock();
        let Some(slot) = inner.slots.iter_mut().find(|slot| {
            slot.ctl
                .as_ref()
                .is_some_and(|ctl| ctl.pending.contains_key(request_id))
        }) else {
            return Err(AssistantError::not_found(
                "No tool call is waiting for confirmation under this request id.",
            ));
        };
        let slot_id = slot.id;
        let run_id = slot.dto.run_id.clone();
        let Some(ctl) = slot.ctl.as_mut() else {
            return Err(AssistantError::not_found("The run has already ended."));
        };
        let cloud = ctl.cloud.clone();
        let Some(pending) = ctl.pending.get_mut(request_id) else {
            return Err(AssistantError::not_found(
                "The tool call is no longer pending.",
            ));
        };
        if pending.claimed {
            return Err(AssistantError::conflict(
                "A decision for this tool call is already being delivered.",
            ));
        }
        if Instant::now() >= pending.deadline {
            slot.expire(request_id);
            self.publish(&inner);
            return Err(AssistantError::conflict(
                "The confirmation window has expired; the action was not performed.",
            ));
        }
        pending.claimed = true;
        let Some(run_id) = run_id else {
            pending.claimed = false;
            return Err(AssistantError::conflict("The run has not started yet."));
        };
        Ok((slot_id, run_id, cloud))
    }

    /// 本机到期：确认窗口过了仍无决定（也没有决定在途）则标为过期；云端按同一窗口自行判定。
    fn expire(&self, slot_id: SlotId, request_id: &str) {
        let mut inner = self.lock();
        let Some(slot) = inner.slots.iter_mut().find(|slot| slot.id == slot_id) else {
            return;
        };
        let in_flight = slot
            .ctl
            .as_ref()
            .and_then(|ctl| ctl.pending.get(request_id))
            .is_none_or(|pending| pending.claimed);
        if in_flight {
            return;
        }
        if slot.expire(request_id) {
            self.publish(&inner);
        }
    }

    fn spawn_expiry(
        self: &Arc<Self>,
        slot_id: SlotId,
        request_id: String,
        window: Duration,
        cancel: CancellationToken,
    ) {
        let service = Arc::clone(self);
        self.spawn(async move {
            tokio::select! {
                () = cancel.cancelled() => {}
                () = tokio::time::sleep(window) => service.expire(slot_id, &request_id),
            }
        });
    }

    fn spawn_tool(self: &Arc<Self>, run: ToolRun) {
        let service = Arc::clone(self);
        self.spawn(service.run_tool(run));
    }

    /// 在本机执行一次工具并把结果回传云端。
    async fn run_tool(self: Arc<Self>, run: ToolRun) {
        let token = run.run_cancel.child_token();
        let ToolRun {
            slot_id,
            request_id,
            run_id,
            cloud,
            run_cancel,
            call,
        } = run;
        let output = match self.executor() {
            Some(executor) => {
                let tool_call = ToolCall {
                    name: call.name,
                    version: call.version,
                    arguments: call.arguments,
                };
                execute_with_deadline(executor.as_ref(), tool_call, &token, call.timeout).await
            }
            None => ToolOutput {
                output: "The local tool executor is not available.".to_owned(),
                is_error: true,
            },
        };
        if run_cancel.is_cancelled() {
            // 执行已被取消（用户取消 / 断线 / 登出）：云端不再等结果，状态由取消方写好。
            tracing::debug!(%request_id, "assistant tool stopped with its run; result not reported");
            return;
        }
        let output_text = clamp_output(output.output);
        let is_error = output.is_error;
        let preview = preview_of(&output_text);
        self.with_slot(slot_id, |slot| {
            if let Some(call) = slot.tool_mut(&request_id) {
                call.status = if is_error {
                    AssistantToolStatus::Failed
                } else {
                    AssistantToolStatus::Succeeded
                };
                call.output_preview = Some(preview);
            }
        });
        let Some(run_id) = run_id else {
            tracing::warn!(%request_id, "assistant tool finished but the run id is unknown; result not reported");
            return;
        };
        report_result(
            &cloud,
            &run_cancel,
            &run_id,
            &request_id,
            &output_text,
            is_error,
        )
        .await;
    }
}

/// 回传工具结果：404 = 云端已不再等（记 debug 日志即可）；网络层失败重试几次。
async fn report_result(
    cloud: &AiCloud,
    cancel: &CancellationToken,
    run_id: &str,
    request_id: &str,
    output: &str,
    is_error: bool,
) {
    for attempt in 1..=RESULT_ATTEMPTS {
        match cloud
            .tool_result(run_id, request_id, output, is_error)
            .await
        {
            Ok(()) => return,
            Err(error) if error.status == Some(404) => {
                tracing::debug!(%request_id, "assistant tool result not accepted: the cloud no longer waits for it");
                return;
            }
            Err(error) if error.retryable && attempt < RESULT_ATTEMPTS => {
                tracing::debug!(%request_id, attempt, error = %error, "assistant tool result delivery failed; retrying");
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(RESULT_RETRY_DELAY) => {}
                }
            }
            Err(error) => {
                tracing::warn!(%request_id, error = %error, "assistant tool result could not be delivered");
                return;
            }
        }
    }
}

/// 执行工具并施加时限：到期先取消令牌、给执行器一点收尾时间，再返回超时结果。
async fn execute_with_deadline(
    executor: &dyn ToolExecutor,
    call: ToolCall,
    token: &CancellationToken,
    timeout: Duration,
) -> ToolOutput {
    let execution = executor.execute(call, token.clone());
    tokio::pin!(execution);
    tokio::select! {
        output = &mut execution => output,
        () = tokio::time::sleep(timeout) => {
            token.cancel();
            if tokio::time::timeout(TOOL_CANCEL_GRACE, &mut execution).await.is_err() {
                tracing::warn!("assistant tool did not stop after its deadline");
            }
            ToolOutput {
                output: format!(
                    "The tool did not finish within {} seconds and was cancelled.",
                    timeout.as_secs()
                ),
                is_error: true,
            }
        }
    }
}

fn require_id<'a>(field: &str, value: &'a str) -> Result<&'a str, AssistantError> {
    let value = value.trim();
    if value.is_empty() {
        Err(AssistantError::invalid_field(
            field,
            format!("{field} is empty."),
        ))
    } else {
        Ok(value)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn log_join(result: Result<(), JoinError>) {
    match result {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => tracing::debug!("assistant task cancelled"),
        Err(error) => tracing::error!(error = %error, "assistant task panicked"),
    }
}
