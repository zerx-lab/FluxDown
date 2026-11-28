//! FluxCloud AI 助手的客户端工具执行器。
//!
//! 云端是工具定义(描述、参数 Schema、副作用、时限)的唯一来源;本机只声明实现了哪些
//! `(name, version)`,按名称 + 版本分发,参数按目录 Schema 的语义解析(多余字段一律拒绝)。
//! 一次调用的收尾统一在这里:时限 / 取消 → 结果脱敏 → 截断到 [`output::MAX_OUTPUT_BYTES`]。
//! 非只读工具的用户确认在调用方的状态机里完成,执行器只负责执行与策略校验。
//!
//! 工具与本机 RPC 的对应关系见各实现模块:
//! - `read`:应用 / 设置 / 运行时 / 网络 / 插件 / 组件 / 体检 / 网卡 / 测速;
//! - `read_tasks`:任务列表与详情、BT 状态(含文件表);
//! - `read_logs`:日志;
//! - `tasks`、`task_ops`、`settings`、`admin`:写工具(任务 / 队列 / 设置 / 组件 / 插件 / 体检修复);
//! - `preview`:待确认写工具的人话预览(任务名、设置新旧值、脱敏地址、危险标记)。
//!
//! 写类工具一经用户批准就不再随取消半途丢弃:取消时已发出的 daemon 请求继续在后台跑完(结果写日志),
//! `execute` 立即返回 `interrupted = true`;只读 / 测速工具照旧直接取消。

mod admin;
mod output;
mod preview;
mod read;
mod read_logs;
mod read_tasks;
mod settings;
mod task_ops;
mod tasks;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fluxdown_api::service::LiveSpeed;
use fluxdown_protocol::{
    AgentPreferencesDto, AgentSnapshot, AssistantToolPreviewDto, DaemonConfigSnapshot,
    RpcErrorData, method,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::event_hub::AgentEventHub;
use crate::gateway::GatewayService;
use crate::task_events::TaskEventHub;

/// 只读工具的执行时限。
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// 写工具(用户确认之后起算)的执行时限。
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// 体检修复的执行时限:重新探测存储 / 组件可能要几十秒。
const REPAIR_TIMEOUT: Duration = Duration::from_secs(120);
/// 组件安装 / 更新的执行时限:要联网下载几十到上百 MB。
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
/// 测速的执行时限,对应云端目录的 `timeoutSecs`:最多 4 条路线 × 每条 60 秒,另留建连与收尾余量。
const MEASURE_TIMEOUT: Duration = Duration::from_secs(300);
/// 首次读取任务实时速率前,等待速率采样器收到第一批进度帧的时间。
const SPEED_WARMUP: Duration = Duration::from_millis(1500);

/// 云端未下发 `maxOutputChars`(旧云端)时的输出预算:FluxCloud 默认的 `maxToolOutputChars`。
const DEFAULT_OUTPUT_BYTES: usize = 24_000;
/// 输出预算的下限:再小连截断说明和 JSON 外壳都放不下。
const MIN_OUTPUT_BYTES: usize = 512;

/// `clientToolCall.maxOutputChars` → 本机输出字节预算:不超过本机上限 [`output::MAX_OUTPUT_BYTES`]。
/// 字节数不小于字符数,按同一个数取预算,云端按字符截断时就不会切到我们的输出;缺失按云端默认值。
pub(crate) fn output_budget(max_output_chars: Option<u32>) -> usize {
    match max_output_chars {
        Some(chars) => usize::try_from(chars)
            .unwrap_or(usize::MAX)
            .clamp(MIN_OUTPUT_BYTES, output::MAX_OUTPUT_BYTES),
        None => DEFAULT_OUTPUT_BYTES,
    }
}

/// 一次客户端工具调用(云端 `clientToolCall` 事件的内容)。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToolCall {
    pub name: String,
    pub version: u32,
    pub arguments: Value,
    /// 回传输出的字节预算(见 [`output_budget`]):工具按它裁剪大列表,收尾按它兜底。
    pub max_output_bytes: usize,
}

/// 工具执行结果,原样回传云端。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolOutput {
    /// 已脱敏、已截断(≤ 64 KiB)的文本;JSON 结果序列化为紧凑字符串。
    pub output: String,
    pub is_error: bool,
    /// 已批准的写类工具在执行中被停止(取消 / 超时):已发出的请求可能已生效,结果未知。
    pub interrupted: bool,
}

/// 待确认调用的可读预览(用本机数据生成)与危险标记。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ToolPreview {
    /// 生成不了(或无需预览)时为 `None`。
    pub preview: Option<AssistantToolPreviewDto>,
    /// 删除任务 / 文件、重新下载等不可撤销的动作:确认卡片用危险样式。
    pub destructive: bool,
}

impl ToolPreview {
    pub(crate) fn none() -> Self {
        Self::default()
    }
}

/// 工具执行失败的原因;文本是给模型看的英文说明。
#[derive(Debug, thiserror::Error)]
pub(super) enum ToolError {
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    /// 本机 RPC 失败(含方法名与错误码)。
    #[error("{0}")]
    Rpc(String),
    /// 黑名单 / 策略拒绝。
    #[error("refused: {0}")]
    Refused(String),
    #[error("cancelled")]
    Cancelled,
}

/// 把 daemon / agent 的应用错误转成给模型看的一行文本:`<method> failed: {"code":…}`。
fn rpc_error_message(method: &str, data: &RpcErrorData) -> String {
    match serde_json::to_string(data) {
        Ok(detail) => format!("{method} failed: {detail}"),
        Err(_) => format!("{method} failed: {:?}", data.code),
    }
}

/// 工具执行的共享上下文。
pub(super) struct ToolContext {
    pub gateway: Arc<GatewayService>,
    events: AgentEventHub,
    /// 任务实时速率只存在于进度事件流里,首次需要时才起采样泵。
    speeds: tokio::sync::OnceCell<TaskEventHub>,
}

impl ToolContext {
    fn new(gateway: Arc<GatewayService>, events: AgentEventHub) -> Self {
        Self {
            gateway,
            events,
            speeds: tokio::sync::OnceCell::new(),
        }
    }

    /// 以本机身份调 gateway 方法(`daemon.*` 经 agent 转发,`agent.*` 直达),返回 data。
    pub(super) async fn call(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        self.gateway
            .dispatch_local(method, params)
            .await
            .map_err(|data| ToolError::Rpc(rpc_error_message(method, &data)))
    }

    /// [`Self::call`] 并把 data 解析成 `T`。
    pub(super) async fn call_as<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, ToolError> {
        let value = self.call(method, params).await?;
        serde_json::from_value(value)
            .map_err(|error| ToolError::Rpc(format!("{method}: unexpected response: {error}")))
    }

    /// 当前 daemon 配置(`daemon.config.get`)。
    pub(super) async fn daemon_config(&self) -> Result<DaemonConfigSnapshot, ToolError> {
        self.call_as(method::DAEMON_CONFIG_GET, json!({})).await
    }

    /// 当前 agent 偏好(取自 agent 事件投影)。
    pub(super) async fn preferences(&self) -> Result<AgentPreferencesDto, ToolError> {
        Ok(self.events.inspect(|snapshot| snapshot.preferences.clone()))
    }

    /// 在 agent 事件投影的临界区内只读访问;闭包内不得再调用 hub。
    pub(super) fn inspect<R>(&self, read: impl FnOnce(&AgentSnapshot) -> R) -> R {
        self.events.inspect(read)
    }

    /// 各任务当前的实时速率(字节 / 秒)。
    pub(super) async fn live_speeds(&self) -> HashMap<String, LiveSpeed> {
        let hub = self
            .speeds
            .get_or_init(|| async {
                let hub = TaskEventHub::spawn(&self.events);
                tokio::time::sleep(SPEED_WARMUP).await;
                hub
            })
            .await;
        hub.live_speeds()
    }

    /// `limit` = 回传输出的字节预算:会产出大列表的工具(任务 / BT 文件 / 日志)按它按条目裁剪。
    async fn run(&self, id: ToolId, args: Value, limit: usize) -> Result<Value, ToolError> {
        match id {
            ToolId::GetAppInfo => read::app_info(self, args).await,
            ToolId::GetSettings => read::settings(self, args).await,
            ToolId::ListTasks => read_tasks::list_tasks(self, args, limit).await,
            ToolId::GetTask => read_tasks::get_task(self, args).await,
            ToolId::GetRuntimeStats => read::runtime_stats(self, args).await,
            ToolId::GetBtStatus => read_tasks::bt_status(self, args, false, limit).await,
            ToolId::GetBtStatusV2 => read_tasks::bt_status(self, args, true, limit).await,
            ToolId::GetNetworkStatus => read::network_status(self, args).await,
            ToolId::ListPlugins => read::plugins(self, args).await,
            ToolId::ListComponents => read::components(self, args).await,
            ToolId::RunDiagnostics => read::diagnostics(self, args).await,
            ToolId::GetLogs => read_logs::logs(self, args, limit).await,
            ToolId::GetNetInterfaces => read::net_interfaces(self, args).await,
            ToolId::MeasureRoutes => read::measure_routes(self, args).await,
            ToolId::CreateTasks => tasks::create_tasks(self, args).await,
            ToolId::ManageTasks => tasks::manage_tasks(self, args, false).await,
            ToolId::ManageTasksV2 => tasks::manage_tasks(self, args, true).await,
            ToolId::UpdateSettings => settings::update_settings(self, args).await,
            ToolId::ManageQueues => admin::manage_queues(self, args).await,
            ToolId::InstallComponent => admin::install_component(self, args).await,
            ToolId::SetPluginEnabled => admin::set_plugin_enabled(self, args).await,
            ToolId::Repair => admin::repair(self, args).await,
        }
    }
}

/// 按目录 Schema 解析工具参数;`null` 视同空对象,多余字段由各 Args 的 `deny_unknown_fields` 拒绝。
pub(super) fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    let args = if args.is_null() {
        Value::Object(Map::new())
    } else {
        args
    };
    serde_json::from_value(args).map_err(|error| ToolError::InvalidArguments(error.to_string()))
}

/// 结果结构化为 JSON;序列化失败(不应发生)以明确的错误上抛,不吞掉。
pub(super) fn to_json<T: serde::Serialize>(what: &str, value: &T) -> Result<Value, ToolError> {
    serde_json::to_value(value)
        .map_err(|error| ToolError::Rpc(format!("cannot encode {what}: {error}")))
}

/// 无参数工具:只接受 `{}` / `null`。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

pub(super) fn expect_no_args(args: Value) -> Result<(), ToolError> {
    parse_args::<NoArgs>(args).map(|NoArgs {}| ())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolId {
    GetAppInfo,
    GetSettings,
    ListTasks,
    GetTask,
    GetRuntimeStats,
    GetBtStatus,
    GetBtStatusV2,
    GetNetworkStatus,
    ListPlugins,
    ListComponents,
    RunDiagnostics,
    GetLogs,
    GetNetInterfaces,
    MeasureRoutes,
    CreateTasks,
    ManageTasks,
    ManageTasksV2,
    UpdateSettings,
    ManageQueues,
    InstallComponent,
    SetPluginEnabled,
    Repair,
}

/// 本机实现的一个工具版本。名称与版本是与 FluxCloud `ai/catalog.rs` 的契约。
struct ToolDef {
    name: &'static str,
    version: u32,
    id: ToolId,
    timeout: Duration,
    /// 会改动设置 / 任务 / 组件等本机状态:批准后不随取消丢弃,见 [`execute_write`]。
    writes: bool,
}

const fn read_tool(name: &'static str, version: u32, id: ToolId, timeout: Duration) -> ToolDef {
    ToolDef {
        name,
        version,
        id,
        timeout,
        writes: false,
    }
}

const fn write_tool(name: &'static str, version: u32, id: ToolId, timeout: Duration) -> ToolDef {
    ToolDef {
        name,
        version,
        id,
        timeout,
        writes: true,
    }
}

/// 同名工具可并存多个版本:云端按客户端声明的版本取目录里最高的,旧云端仍只会用版本 1。
static TOOLS: &[ToolDef] = &[
    read_tool("fd_get_app_info", 1, ToolId::GetAppInfo, READ_TIMEOUT),
    read_tool("fd_get_settings", 1, ToolId::GetSettings, READ_TIMEOUT),
    read_tool("fd_list_tasks", 1, ToolId::ListTasks, READ_TIMEOUT),
    read_tool("fd_get_task", 1, ToolId::GetTask, READ_TIMEOUT),
    read_tool(
        "fd_get_runtime_stats",
        1,
        ToolId::GetRuntimeStats,
        READ_TIMEOUT,
    ),
    read_tool("fd_get_bt_status", 1, ToolId::GetBtStatus, READ_TIMEOUT),
    read_tool("fd_get_bt_status", 2, ToolId::GetBtStatusV2, READ_TIMEOUT),
    read_tool(
        "fd_get_network_status",
        1,
        ToolId::GetNetworkStatus,
        READ_TIMEOUT,
    ),
    read_tool("fd_list_plugins", 1, ToolId::ListPlugins, READ_TIMEOUT),
    read_tool(
        "fd_list_components",
        1,
        ToolId::ListComponents,
        READ_TIMEOUT,
    ),
    read_tool(
        "fd_run_diagnostics",
        1,
        ToolId::RunDiagnostics,
        READ_TIMEOUT,
    ),
    read_tool("fd_get_logs", 1, ToolId::GetLogs, READ_TIMEOUT),
    read_tool(
        "fd_get_net_interfaces",
        1,
        ToolId::GetNetInterfaces,
        READ_TIMEOUT,
    ),
    read_tool(
        "fd_measure_routes",
        1,
        ToolId::MeasureRoutes,
        MEASURE_TIMEOUT,
    ),
    write_tool("fd_create_tasks", 1, ToolId::CreateTasks, WRITE_TIMEOUT),
    write_tool("fd_manage_tasks", 1, ToolId::ManageTasks, WRITE_TIMEOUT),
    write_tool("fd_manage_tasks", 2, ToolId::ManageTasksV2, WRITE_TIMEOUT),
    write_tool(
        "fd_update_settings",
        1,
        ToolId::UpdateSettings,
        WRITE_TIMEOUT,
    ),
    write_tool("fd_manage_queues", 1, ToolId::ManageQueues, WRITE_TIMEOUT),
    write_tool(
        "fd_install_component",
        1,
        ToolId::InstallComponent,
        INSTALL_TIMEOUT,
    ),
    write_tool(
        "fd_set_plugin_enabled",
        1,
        ToolId::SetPluginEnabled,
        WRITE_TIMEOUT,
    ),
    write_tool("fd_repair", 1, ToolId::Repair, REPAIR_TIMEOUT),
];

/// 按名称 + 版本找实现;失败时给出面向模型的原因。
fn lookup(name: &str, version: u32) -> Result<&'static ToolDef, String> {
    if let Some(found) = TOOLS
        .iter()
        .find(|tool| tool.name == name && tool.version == version)
    {
        return Ok(found);
    }
    let versions: Vec<String> = TOOLS
        .iter()
        .filter(|tool| tool.name == name)
        .map(|tool| tool.version.to_string())
        .collect();
    if versions.is_empty() {
        Err(format!("unknown client tool '{name}'"))
    } else {
        Err(format!(
            "client tool '{name}' has no version {version}; implemented versions: {}",
            versions.join(", ")
        ))
    }
}

fn error_output(message: &str, limit: usize) -> ToolOutput {
    ToolOutput {
        output: output::finalize_text(message, limit),
        is_error: true,
        interrupted: false,
    }
}

/// 写类工具被中途停止(取消 / 超时 / 任务崩溃):已发出的请求可能已生效,结果未知。
fn interrupted_output(message: &str, limit: usize) -> ToolOutput {
    ToolOutput {
        interrupted: true,
        ..error_output(message, limit)
    }
}

fn finish(outcome: Result<Value, ToolError>, limit: usize) -> ToolOutput {
    match outcome {
        Ok(value) => ToolOutput {
            output: output::finalize_json(value, limit),
            is_error: false,
            interrupted: false,
        },
        Err(error) => error_output(&error.to_string(), limit),
    }
}

/// 执行流程:查表 → 带时限与取消地运行 → 收尾。`run` 把工具 id 与参数交给真正的实现,单测可替换。
/// 写类工具走 [`execute_write`](取消不丢弃在途请求),其余在取消 / 超时时直接丢弃。
async fn execute_with<F, Fut>(call: ToolCall, cancel: CancellationToken, run: F) -> ToolOutput
where
    F: FnOnce(ToolId, Value) -> Fut,
    Fut: Future<Output = Result<Value, ToolError>> + Send + 'static,
{
    let ToolCall {
        name,
        version,
        arguments,
        max_output_bytes: limit,
    } = call;
    let tool = match lookup(&name, version) {
        Ok(tool) => tool,
        Err(message) => return error_output(&message, limit),
    };
    let work = run(tool.id, arguments);
    if tool.writes {
        return execute_write(tool, cancel, work, limit).await;
    }
    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ToolError::Cancelled),
        timed = tokio::time::timeout(tool.timeout, work) => match timed {
            Ok(result) => result,
            Err(_) => {
                return error_output(
                    &format!(
                        "client tool '{name}' timed out after {} seconds",
                        tool.timeout.as_secs()
                    ),
                    limit,
                );
            }
        },
    };
    finish(outcome, limit)
}

/// 写类工具:工作放到独立任务里跑(自带时限)。执行期间被取消时不丢弃它——已发出的 daemon 请求
/// 继续在后台跑完、结果写日志,本函数立即返回 `interrupted`(结果未知,须先核对当前状态再重试)。
async fn execute_write<Fut>(
    tool: &'static ToolDef,
    cancel: CancellationToken,
    work: Fut,
    output_limit: usize,
) -> ToolOutput
where
    Fut: Future<Output = Result<Value, ToolError>> + Send + 'static,
{
    // 还没开始执行就已取消:什么都没发出,不是「结果未知」。
    if cancel.is_cancelled() {
        return error_output(&ToolError::Cancelled.to_string(), output_limit);
    }
    let name = tool.name;
    let limit = tool.timeout;
    let mut handle = tokio::spawn(async move { tokio::time::timeout(limit, work).await });
    // 两者同时就绪时以执行结果为准。
    let joined = tokio::select! {
        biased;
        joined = &mut handle => Some(joined),
        () = cancel.cancelled() => None,
    };
    match joined {
        Some(Ok(Ok(result))) => finish(result, output_limit),
        Some(Ok(Err(_))) => interrupted_output(
            &format!(
                "client tool '{name}' timed out after {} seconds; the operation may or may not have completed, check the current state before retrying",
                limit.as_secs()
            ),
            output_limit,
        ),
        Some(Err(error)) => {
            tracing::error!(tool = name, %error, "assistant write tool task failed");
            interrupted_output(
                &format!(
                    "client tool '{name}' ended unexpectedly; the operation may or may not have completed, check the current state before retrying"
                ),
                output_limit,
            )
        }
        None => {
            tokio::spawn(async move {
                match handle.await {
                    Ok(Ok(Ok(_))) => {
                        tracing::info!(
                            tool = name,
                            "assistant write tool finished after its run stopped"
                        );
                    }
                    Ok(Ok(Err(error))) => {
                        tracing::warn!(tool = name, %error, "assistant write tool failed after its run stopped");
                    }
                    Ok(Err(_)) => {
                        tracing::warn!(
                            tool = name,
                            "assistant write tool timed out after its run stopped"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(tool = name, %error, "assistant write tool task failed after its run stopped");
                    }
                }
            });
            interrupted_output(
                &format!(
                    "the run was stopped while client tool '{name}' was executing; it may still complete in the background, check the current state before retrying"
                ),
                output_limit,
            )
        }
    }
}

/// 客户端工具执行器。
pub(crate) struct ClientTools {
    ctx: Arc<ToolContext>,
}

impl ClientTools {
    /// `gateway` 是本机 RPC 入口;`events` 是同一进程的 agent 事件投影(任务表、实时速率、偏好)。
    pub(crate) fn new(gateway: Arc<GatewayService>, events: AgentEventHub) -> Self {
        read::mark_started();
        Self {
            ctx: Arc::new(ToolContext::new(gateway, events)),
        }
    }

    /// 本机实现的 `(name, version)`,建会话时声明给云端(`clientTools`)。
    pub(crate) fn declared() -> Vec<(&'static str, u32)> {
        TOOLS.iter().map(|tool| (tool.name, tool.version)).collect()
    }

    /// 执行一次调用;取消即尽快返回(写类工具见 [`execute_write`])。未知工具 / 版本、参数错误、超时都以
    /// `is_error = true` 返回。
    pub(crate) async fn execute(&self, call: ToolCall, cancel: CancellationToken) -> ToolOutput {
        let ctx = Arc::clone(&self.ctx);
        let limit = call.max_output_bytes;
        execute_with(call, cancel, move |id, args| async move {
            ctx.run(id, args, limit).await
        })
        .await
    }

    /// 为待确认的调用生成人能读懂的预览;生成不了返回 `preview: None`。只用本机数据与快速 RPC。
    pub(crate) async fn preview(&self, call: &ToolCall) -> ToolPreview {
        preview::build(self.ctx.as_ref(), call).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn call(name: &str, version: u32, arguments: Value) -> ToolCall {
        ToolCall {
            name: name.to_owned(),
            version,
            arguments,
            max_output_bytes: output::MAX_OUTPUT_BYTES,
        }
    }

    #[test]
    fn declares_every_catalog_tool_version_once() {
        let declared = ClientTools::declared();
        let unique: HashSet<(&str, u32)> = declared.iter().copied().collect();
        assert_eq!(unique.len(), declared.len(), "duplicate (name, version)");
        let names: HashSet<&str> = declared.iter().map(|(name, _)| *name).collect();
        assert_eq!(names.len(), 20);
        for name in [
            "fd_get_app_info",
            "fd_get_settings",
            "fd_list_tasks",
            "fd_get_task",
            "fd_get_runtime_stats",
            "fd_get_bt_status",
            "fd_get_network_status",
            "fd_list_plugins",
            "fd_list_components",
            "fd_run_diagnostics",
            "fd_get_logs",
            "fd_get_net_interfaces",
            "fd_measure_routes",
            "fd_create_tasks",
            "fd_manage_tasks",
            "fd_update_settings",
            "fd_manage_queues",
            "fd_install_component",
            "fd_set_plugin_enabled",
            "fd_repair",
        ] {
            assert!(names.contains(name), "{name} not declared");
        }
        for (name, version) in [
            ("fd_get_bt_status", 2),
            ("fd_manage_tasks", 1),
            ("fd_manage_tasks", 2),
        ] {
            assert!(
                unique.contains(&(name, version)),
                "{name} v{version} missing"
            );
        }
    }

    #[test]
    fn only_state_changing_tools_are_write_tools() {
        for tool in TOOLS {
            let expected = matches!(
                tool.name,
                "fd_create_tasks"
                    | "fd_manage_tasks"
                    | "fd_update_settings"
                    | "fd_manage_queues"
                    | "fd_install_component"
                    | "fd_set_plugin_enabled"
                    | "fd_repair"
            );
            assert_eq!(tool.writes, expected, "{} v{}", tool.name, tool.version);
        }
    }

    #[test]
    fn timeouts_follow_the_tool_effect() {
        let timeout = |name: &str| lookup(name, 1).map(|tool| tool.timeout);
        assert_eq!(timeout("fd_get_logs"), Ok(Duration::from_secs(30)));
        assert_eq!(timeout("fd_measure_routes"), Ok(Duration::from_secs(300)));
        assert_eq!(timeout("fd_manage_tasks"), Ok(Duration::from_secs(60)));
        assert_eq!(timeout("fd_update_settings"), Ok(Duration::from_secs(60)));
        assert_eq!(
            timeout("fd_install_component"),
            Ok(Duration::from_secs(600))
        );
        assert_eq!(timeout("fd_repair"), Ok(Duration::from_secs(120)));
    }

    #[tokio::test]
    async fn unknown_tool_and_unknown_version_are_errors_that_never_run() {
        let runner = |_: ToolId, _: Value| async {
            panic!("an unknown tool must not reach the implementation");
            #[allow(unreachable_code)]
            Ok::<Value, ToolError>(Value::Null)
        };
        let unknown = execute_with(
            call("fd_nope", 1, json!({})),
            CancellationToken::new(),
            runner,
        )
        .await;
        assert!(unknown.is_error);
        assert!(
            unknown.output.contains("unknown client tool 'fd_nope'"),
            "{}",
            unknown.output
        );

        let version = execute_with(
            call("fd_get_task", 2, json!({})),
            CancellationToken::new(),
            runner,
        )
        .await;
        assert!(version.is_error);
        assert!(
            version.output.contains("no version 2"),
            "{}",
            version.output
        );
        assert!(
            version.output.contains("implemented versions: 1"),
            "{}",
            version.output
        );
    }

    #[tokio::test]
    async fn successful_results_are_sanitized_and_compact() {
        let out = execute_with(
            call("fd_get_task", 1, json!({})),
            CancellationToken::new(),
            |id, _| async move {
                assert_eq!(id, ToolId::GetTask);
                Ok(json!({ "url": "https://u:p@h/x?token=abc", "ok": true }))
            },
        )
        .await;
        assert!(!out.is_error);
        for leaked in ["u:p", "abc"] {
            assert!(
                !out.output.contains(leaked),
                "{leaked} leaked: {}",
                out.output
            );
        }
        assert!(!out.output.contains('\n'), "{}", out.output);
        assert!(
            serde_json::from_str::<Value>(&out.output).is_ok(),
            "{}",
            out.output
        );
    }

    #[tokio::test]
    async fn oversized_multibyte_results_become_a_valid_json_envelope_within_the_limit() {
        let out = execute_with(
            call("fd_get_logs", 1, json!({})),
            CancellationToken::new(),
            |_, _| async { Ok(json!({ "text": "错".repeat(output::MAX_OUTPUT_BYTES) })) },
        )
        .await;
        assert!(!out.is_error);
        assert!(
            out.output.len() <= output::MAX_OUTPUT_BYTES,
            "{}",
            out.output.len()
        );
        let parsed: Value = serde_json::from_str(&out.output).unwrap_or(Value::Null);
        assert_eq!(parsed["truncated"], json!(true), "len {}", out.output.len());
    }

    #[tokio::test]
    async fn the_output_budget_follows_the_call_limit() {
        let mut small = call("fd_get_logs", 1, json!({}));
        small.max_output_bytes = output_budget(Some(2_000));
        let out = execute_with(small, CancellationToken::new(), |_, _| async {
            Ok(json!({ "text": "x".repeat(10_000) }))
        })
        .await;
        assert!(out.output.len() <= 2_000, "{}", out.output.len());
        assert!(serde_json::from_str::<Value>(&out.output).is_ok());
    }

    #[test]
    fn output_budget_clamps_and_defaults() {
        assert_eq!(output_budget(None), 24_000);
        assert_eq!(output_budget(Some(24_000)), 24_000);
        assert_eq!(output_budget(Some(200_000)), output::MAX_OUTPUT_BYTES);
        assert_eq!(output_budget(Some(0)), MIN_OUTPUT_BYTES);
    }

    #[tokio::test]
    async fn tool_errors_become_error_outputs() {
        let out = execute_with(
            call("fd_list_tasks", 1, json!({})),
            CancellationToken::new(),
            |_, _| async {
                Err(ToolError::InvalidArguments(
                    "limit must be at most 100".to_owned(),
                ))
            },
        )
        .await;
        assert!(out.is_error);
        assert_eq!(out.output, "invalid arguments: limit must be at most 100");

        let out = execute_with(
            call("fd_manage_tasks", 1, json!({})),
            CancellationToken::new(),
            |_, _| async {
                Err(ToolError::Refused(
                    "deleting files needs deleteFiles=true".to_owned(),
                ))
            },
        )
        .await;
        assert!(out.is_error);
        assert!(out.output.starts_with("refused: "), "{}", out.output);
    }

    #[tokio::test]
    async fn cancellation_returns_without_waiting_for_the_tool() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = execute_with(call("fd_measure_routes", 1, json!({})), cancel, |_, _| {
            std::future::pending::<Result<Value, ToolError>>()
        })
        .await;
        assert!(out.is_error);
        assert_eq!(out.output, "cancelled");
    }

    #[tokio::test]
    async fn cancelled_before_a_write_tool_starts_sends_nothing() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let out = execute_with(
            call("fd_manage_tasks", 1, json!({})),
            cancel,
            move |_, _| async move {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(json!({ "ok": true }))
            },
        )
        .await;
        assert!(out.is_error);
        assert!(
            !out.interrupted,
            "nothing was sent, so the result is not unknown"
        );
        assert_eq!(out.output, "cancelled");
        tokio::task::yield_now().await;
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancelling_a_running_write_tool_detaches_it_and_reports_interrupted() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let cancel = CancellationToken::new();
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            stopper.cancel();
        });
        let started = std::time::Instant::now();
        let out = execute_with(
            call("fd_manage_tasks", 1, json!({})),
            cancel,
            move |_, _| async move {
                tokio::time::sleep(Duration::from_millis(400)).await;
                flag.store(true, Ordering::SeqCst);
                Ok(json!({ "ok": true }))
            },
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "must not wait for the tool"
        );
        assert!(out.is_error && out.interrupted, "{out:?}");
        assert!(
            out.output.contains("check the current state"),
            "{}",
            out.output
        );
        assert!(
            !finished.load(Ordering::SeqCst),
            "still running right after the cancel"
        );
        // 已发出的请求继续在后台跑完,而不是被丢弃。
        for _ in 0..100 {
            if finished.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            finished.load(Ordering::SeqCst),
            "the detached write must run to completion"
        );
    }

    #[tokio::test]
    async fn cancelling_a_read_tool_drops_it_without_interrupted() {
        let cancel = CancellationToken::new();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            stopper.cancel();
        });
        let out = execute_with(call("fd_get_task", 1, json!({})), cancel, |_, _| {
            std::future::pending::<Result<Value, ToolError>>()
        })
        .await;
        assert!(out.is_error);
        assert!(!out.interrupted);
        assert_eq!(out.output, "cancelled");
    }

    #[tokio::test]
    async fn a_finished_write_tool_reports_its_real_result_not_interrupted() {
        let out = execute_with(
            call("fd_manage_tasks", 2, json!({})),
            CancellationToken::new(),
            |id, _| async move {
                assert_eq!(id, ToolId::ManageTasksV2);
                Ok(json!({ "applied": [] }))
            },
        )
        .await;
        assert!(!out.is_error && !out.interrupted, "{out:?}");
    }

    #[test]
    fn extra_fields_are_rejected_for_argument_less_tools() {
        assert!(expect_no_args(json!({})).is_ok());
        assert!(expect_no_args(Value::Null).is_ok());
        let error = expect_no_args(json!({ "verbose": true }))
            .err()
            .map(|error| error.to_string());
        assert!(
            error
                .as_deref()
                .is_some_and(|text| text.contains("unknown field")),
            "{error:?}"
        );
    }

    #[test]
    fn rpc_errors_carry_the_method_and_machine_readable_code() {
        let mut data = RpcErrorData::new(fluxdown_protocol::ApplicationErrorCode::NotFound, false);
        data.field = Some("taskId".to_owned());
        let message = rpc_error_message("daemon.task.get", &data);
        assert!(message.starts_with("daemon.task.get failed: "), "{message}");
        assert!(message.contains("notFound"), "{message}");
        assert!(message.contains("taskId"), "{message}");
    }
}
