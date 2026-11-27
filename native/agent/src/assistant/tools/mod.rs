//! FluxCloud AI 助手的客户端工具执行器。
//!
//! 云端是工具定义(描述、参数 Schema、副作用、时限)的唯一来源;本机只声明实现了哪些
//! `(name, version)`,按名称 + 版本分发,参数按目录 Schema 的语义解析(多余字段一律拒绝)。
//! 一次调用的收尾统一在这里:时限 / 取消 → 结果脱敏 → 截断到 [`output::MAX_OUTPUT_BYTES`]。
//! 非只读工具的用户确认在调用方的状态机里完成,执行器只负责执行与策略校验。
//!
//! 工具与本机 RPC 的对应关系见各实现模块:
//! - `read`:应用 / 设置 / 运行时 / 网络 / 插件 / 组件 / 体检 / 网卡 / 测速;
//! - `read_tasks`:任务列表与详情、BT 状态;
//! - `read_logs`:日志;
//! - `tasks`、`settings`:写工具。

mod output;
mod read;
mod read_logs;
mod read_tasks;
mod settings;
mod tasks;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fluxdown_api::service::LiveSpeed;
use fluxdown_protocol::{
    AgentPreferencesDto, AgentSnapshot, DaemonConfigSnapshot, RpcErrorData, method,
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
/// 测速的执行时限,对应云端目录的 `timeoutSecs`:最多 4 条路线 × 每条 60 秒,另留建连与收尾余量。
const MEASURE_TIMEOUT: Duration = Duration::from_secs(300);
/// 首次读取任务实时速率前,等待速率采样器收到第一批进度帧的时间。
const SPEED_WARMUP: Duration = Duration::from_millis(1500);

/// 一次客户端工具调用(云端 `clientToolCall` 事件的内容)。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToolCall {
    pub name: String,
    pub version: u32,
    pub arguments: Value,
}

/// 工具执行结果,原样回传云端。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolOutput {
    /// 已脱敏、已截断(≤ 64 KiB)的文本;JSON 结果序列化为紧凑字符串。
    pub output: String,
    pub is_error: bool,
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

    async fn run(&self, id: ToolId, args: Value) -> Result<Value, ToolError> {
        match id {
            ToolId::GetAppInfo => read::app_info(self, args).await,
            ToolId::GetSettings => read::settings(self, args).await,
            ToolId::ListTasks => read_tasks::list_tasks(self, args).await,
            ToolId::GetTask => read_tasks::get_task(self, args).await,
            ToolId::GetRuntimeStats => read::runtime_stats(self, args).await,
            ToolId::GetBtStatus => read_tasks::bt_status(self, args).await,
            ToolId::GetNetworkStatus => read::network_status(self, args).await,
            ToolId::ListPlugins => read::plugins(self, args).await,
            ToolId::ListComponents => read::components(self, args).await,
            ToolId::RunDiagnostics => read::diagnostics(self, args).await,
            ToolId::GetLogs => read_logs::logs(self, args).await,
            ToolId::GetNetInterfaces => read::net_interfaces(self, args).await,
            ToolId::MeasureRoutes => read::measure_routes(self, args).await,
            ToolId::CreateTasks => tasks::create_tasks(self, args).await,
            ToolId::ManageTasks => tasks::manage_tasks(self, args).await,
            ToolId::UpdateSettings => settings::update_settings(self, args).await,
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
    GetNetworkStatus,
    ListPlugins,
    ListComponents,
    RunDiagnostics,
    GetLogs,
    GetNetInterfaces,
    MeasureRoutes,
    CreateTasks,
    ManageTasks,
    UpdateSettings,
}

/// 本机实现的一个工具版本。名称与版本是与 FluxCloud `ai/catalog.rs` 的契约。
struct ToolDef {
    name: &'static str,
    version: u32,
    id: ToolId,
    timeout: Duration,
}

const fn def(name: &'static str, id: ToolId, timeout: Duration) -> ToolDef {
    ToolDef {
        name,
        version: 1,
        id,
        timeout,
    }
}

static TOOLS: [ToolDef; 16] = [
    def("fd_get_app_info", ToolId::GetAppInfo, READ_TIMEOUT),
    def("fd_get_settings", ToolId::GetSettings, READ_TIMEOUT),
    def("fd_list_tasks", ToolId::ListTasks, READ_TIMEOUT),
    def("fd_get_task", ToolId::GetTask, READ_TIMEOUT),
    def(
        "fd_get_runtime_stats",
        ToolId::GetRuntimeStats,
        READ_TIMEOUT,
    ),
    def("fd_get_bt_status", ToolId::GetBtStatus, READ_TIMEOUT),
    def(
        "fd_get_network_status",
        ToolId::GetNetworkStatus,
        READ_TIMEOUT,
    ),
    def("fd_list_plugins", ToolId::ListPlugins, READ_TIMEOUT),
    def("fd_list_components", ToolId::ListComponents, READ_TIMEOUT),
    def("fd_run_diagnostics", ToolId::RunDiagnostics, READ_TIMEOUT),
    def("fd_get_logs", ToolId::GetLogs, READ_TIMEOUT),
    def(
        "fd_get_net_interfaces",
        ToolId::GetNetInterfaces,
        READ_TIMEOUT,
    ),
    def("fd_measure_routes", ToolId::MeasureRoutes, MEASURE_TIMEOUT),
    def("fd_create_tasks", ToolId::CreateTasks, WRITE_TIMEOUT),
    def("fd_manage_tasks", ToolId::ManageTasks, WRITE_TIMEOUT),
    def("fd_update_settings", ToolId::UpdateSettings, WRITE_TIMEOUT),
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

fn error_output(message: &str) -> ToolOutput {
    ToolOutput {
        output: output::finalize_text(message),
        is_error: true,
    }
}

/// 执行流程:查表 → 带时限与取消地运行 → 收尾。`run` 把工具 id 与参数交给真正的实现,单测可替换。
async fn execute_with<F, Fut>(call: ToolCall, cancel: CancellationToken, run: F) -> ToolOutput
where
    F: FnOnce(ToolId, Value) -> Fut,
    Fut: Future<Output = Result<Value, ToolError>>,
{
    let ToolCall {
        name,
        version,
        arguments,
    } = call;
    let tool = match lookup(&name, version) {
        Ok(tool) => tool,
        Err(message) => return error_output(&message),
    };
    let work = run(tool.id, arguments);
    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ToolError::Cancelled),
        timed = tokio::time::timeout(tool.timeout, work) => match timed {
            Ok(result) => result,
            Err(_) => {
                return error_output(&format!(
                    "client tool '{name}' timed out after {} seconds",
                    tool.timeout.as_secs()
                ));
            }
        },
    };
    match outcome {
        Ok(value) => ToolOutput {
            output: output::finalize_json(value),
            is_error: false,
        },
        Err(error) => error_output(&error.to_string()),
    }
}

/// 客户端工具执行器。
pub(crate) struct ClientTools {
    ctx: ToolContext,
}

impl ClientTools {
    /// `gateway` 是本机 RPC 入口;`events` 是同一进程的 agent 事件投影(任务表、实时速率、偏好)。
    pub(crate) fn new(gateway: Arc<GatewayService>, events: AgentEventHub) -> Self {
        read::mark_started();
        Self {
            ctx: ToolContext::new(gateway, events),
        }
    }

    /// 本机实现的 `(name, version)`,建会话时声明给云端(`clientTools`)。
    pub(crate) fn declared() -> Vec<(&'static str, u32)> {
        TOOLS.iter().map(|tool| (tool.name, tool.version)).collect()
    }

    /// 执行一次调用;取消即尽快返回。未知工具 / 版本、参数错误、超时都以 `is_error = true` 返回。
    pub(crate) async fn execute(&self, call: ToolCall, cancel: CancellationToken) -> ToolOutput {
        execute_with(call, cancel, |id, args| self.ctx.run(id, args)).await
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
        }
    }

    #[test]
    fn declares_the_sixteen_catalog_tools_once() {
        let declared = ClientTools::declared();
        assert_eq!(declared.len(), 16);
        let names: HashSet<&str> = declared.iter().map(|(name, _)| *name).collect();
        assert_eq!(names.len(), 16);
        assert!(declared.iter().all(|(_, version)| *version == 1));
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
        ] {
            assert!(names.contains(name), "{name} not declared");
        }
    }

    #[test]
    fn timeouts_follow_the_tool_effect() {
        let timeout = |name: &str| lookup(name, 1).map(|tool| tool.timeout);
        assert_eq!(timeout("fd_get_logs"), Ok(Duration::from_secs(30)));
        assert_eq!(timeout("fd_measure_routes"), Ok(Duration::from_secs(300)));
        assert_eq!(timeout("fd_manage_tasks"), Ok(Duration::from_secs(60)));
        assert_eq!(timeout("fd_update_settings"), Ok(Duration::from_secs(60)));
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
    async fn oversized_multibyte_results_are_truncated_on_a_char_boundary() {
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
        assert!(
            out.output.contains("[output truncated"),
            "tail: {}",
            &out.output[out.output.len() - 80..]
        );
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
