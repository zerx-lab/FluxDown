//! 浏览器 Native Messaging Host 与 agent 之间的长度帧 IPC 服务。

use std::sync::Arc;

use fluxdown_api::service::LiveSpeed;
use fluxdown_protocol::{ApplicationErrorCode, DownloadRequest, PluginDto, TaskDto};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::capture::CaptureService;
use crate::daemon_client::DaemonClient;
use crate::sniff_rules::{Have, SniffRuleStore};

pub mod comspec;

const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;
const MAX_BATCH_ITEMS: usize = 1000;
const ACCEPT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);
const MAX_COMPLETED_TASKS: usize = 10;
/// 扩展 ↔ agent 的 NMH 协议版本；v2 增加应答 `data`、`capabilities`、`capture_status`、`sniff_rules`。
pub const EXTENSION_PROTOCOL_VERSION: u32 = 2;
const SUPPORTED_ACTIONS: &[&str] = &[
    "ping",
    "download",
    "batch_download",
    "tasks",
    "task_op",
    "open_file",
    "reveal_file",
    "capabilities",
    "capture_status",
    "sniff_rules",
    "page_scripts",
    "page_script_source",
    "plugin_invoke",
];
const MAX_CAPTURE_STATUS_IDS: usize = 256;

#[derive(Deserialize)]
struct PipeMessage {
    action: String,
    #[serde(default)]
    msg_id: u64,
    #[serde(flatten)]
    payload: Value,
}

#[derive(Deserialize)]
struct BatchDownloadPayload {
    items: Vec<DownloadRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskOperation {
    op: String,
    task_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskIdPayload {
    task_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptureStatusPayload {
    transaction_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SniffRulesPayload {
    #[serde(default)]
    have: Have,
    /// 用户手动同步：忽略退避并等刷新完成再应答（至多 15 s）。
    #[serde(default)]
    force: bool,
    /// 只取 plugin 层：本机聚合，不触发任何网络刷新（扩展关闭远程规则同步时使用）。
    #[serde(default)]
    plugin_only: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskBrief {
    task_id: String,
    file_name: String,
    status: i32,
    downloaded_bytes: i64,
    total_bytes: i64,
    speed: i64,
    error_message: String,
    created_at: String,
}

#[derive(Serialize)]
struct PipeResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    msg_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tasks: Option<Vec<TaskBrief>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl PipeResponse {
    fn ok(msg_id: u64, message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: Some(message.into()),
            msg_id,
            tasks: None,
            data: None,
        }
    }

    fn error(msg_id: u64, message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: Some(message.into()),
            msg_id,
            tasks: None,
            data: None,
        }
    }

    fn tasks(msg_id: u64, tasks: Vec<TaskBrief>) -> Self {
        Self {
            success: true,
            message: None,
            msg_id,
            tasks: Some(tasks),
            data: None,
        }
    }

    fn ok_data(msg_id: u64, message: impl Into<String>, data: Value) -> Self {
        Self {
            success: true,
            message: Some(message.into()),
            msg_id,
            tasks: None,
            data: Some(data),
        }
    }
}

#[derive(Clone)]
pub struct NmhService {
    daemon: Arc<DaemonClient>,
    capture: Arc<CaptureService>,
    task_events: Option<crate::task_events::TaskEventHub>,
    sniff_rules: Option<Arc<SniffRuleStore>>,
}

impl NmhService {
    #[must_use]
    pub fn new(daemon: Arc<DaemonClient>, capture: Arc<CaptureService>) -> Self {
        Self {
            daemon,
            capture,
            task_events: None,
            sniff_rules: None,
        }
    }

    /// 注入实时速率来源；未注入时 `tasks` 应答的速度恒为 0。
    #[must_use]
    pub fn with_task_events(mut self, task_events: crate::task_events::TaskEventHub) -> Self {
        self.task_events = Some(task_events);
        self
    }

    /// 注入嗅探规则包存储；未注入时 `sniff_rules` 动作按未知动作处理。
    #[must_use]
    pub fn with_sniff_rules(mut self, store: Arc<SniffRuleStore>) -> Self {
        self.sniff_rules = Some(store);
        self
    }

    /// `ready` 在 IPC 端点开始监听后触发；端点不可用时随错误一起被丢弃。
    pub async fn run(
        self,
        cancel: CancellationToken,
        ready: tokio::sync::oneshot::Sender<()>,
    ) -> Result<(), std::io::Error> {
        run_server(self, cancel, ready).await
    }

    async fn dispatch(&self, message: PipeMessage) -> PipeResponse {
        match message.action.as_str() {
            "ping" => PipeResponse::ok(message.msg_id, "pong"),
            "download" => match serde_json::from_value::<DownloadRequest>(message.payload) {
                Ok(request) => match self
                    .capture
                    .submit(request, crate::capture::CaptureOrigin::External)
                    .await
                {
                    Ok(value) => PipeResponse::ok_data(message.msg_id, "download accepted", value),
                    Err(error) => PipeResponse::error(message.msg_id, error.to_string()),
                },
                Err(error) => PipeResponse::error(message.msg_id, error.to_string()),
            },
            "batch_download" => self.batch_download(message.msg_id, message.payload).await,
            "tasks" => self.task_list(message.msg_id).await,
            "task_op" => self.task_operation(message.msg_id, message.payload).await,
            "open_file" => {
                self.platform_action(message.msg_id, message.payload, false)
                    .await
            }
            "reveal_file" => {
                self.platform_action(message.msg_id, message.payload, true)
                    .await
            }
            "capabilities" => self.capabilities(message.msg_id).await,
            "capture_status" => self.capture_status(message.msg_id, message.payload).await,
            "page_scripts" => self.page_scripts(message.msg_id).await,
            "page_script_source" => {
                self.page_script_source(message.msg_id, message.payload)
                    .await
            }
            "plugin_invoke" => self.plugin_invoke(message.msg_id, message.payload).await,
            "sniff_rules" => self.sniff_rules(message.msg_id, message.payload).await,
            other => PipeResponse::error(message.msg_id, format!("unknown action: {other}")),
        }
    }

    async fn batch_download(&self, msg_id: u64, payload: Value) -> PipeResponse {
        let batch = match serde_json::from_value::<BatchDownloadPayload>(payload) {
            Ok(batch) if !batch.items.is_empty() && batch.items.len() <= MAX_BATCH_ITEMS => batch,
            Ok(batch) => {
                return PipeResponse::error(
                    msg_id,
                    format!("invalid batch size: {}", batch.items.len()),
                );
            }
            Err(error) => return PipeResponse::error(msg_id, error.to_string()),
        };
        let count = batch.items.len();
        match self
            .capture
            .submit_many(batch.items, crate::capture::CaptureOrigin::External)
            .await
        {
            Ok(value) => {
                PipeResponse::ok_data(msg_id, format!("batch accepted ({count} items)"), value)
            }
            Err(error) => PipeResponse::error(msg_id, error.to_string()),
        }
    }

    /// plugin 层嗅探规则包；daemon 不支持（未编入插件）或查询失败时按「无插件规则」降级。
    async fn plugin_sniff_rules(&self) -> Option<fluxdown_protocol::PluginSniffRulesDto> {
        match self
            .daemon
            .call::<Value, fluxdown_protocol::PluginSniffRulesDto>(
                fluxdown_protocol::method::DAEMON_PLUGIN_SNIFF_RULES,
                None,
            )
            .await
        {
            Ok(dto) => Some(dto),
            Err(error) => {
                // Unsupported 是常态（无插件特性）；其余错误不应拖垮 capabilities / 官方规则同步。
                if !matches!(error.code, ApplicationErrorCode::Unsupported) {
                    tracing::warn!(code = ?error.code, "plugin sniff rules unavailable");
                }
                None
            }
        }
    }

    /// 页面脚本索引；daemon 不支持 / 查询失败 / epoch 为空时视为不可用（`capabilities` 两字段均为 null）。
    async fn page_scripts_index(&self) -> Option<fluxdown_protocol::PluginPageScriptsDto> {
        match self
            .daemon
            .call::<Value, fluxdown_protocol::PluginPageScriptsDto>(
                fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_SCRIPTS,
                None,
            )
            .await
        {
            Ok(dto) if !dto.epoch.is_empty() => Some(dto),
            Ok(_) => None,
            Err(error) => {
                if !matches!(error.code, ApplicationErrorCode::Unsupported) {
                    tracing::warn!(code = ?error.code, "plugin page scripts unavailable");
                }
                None
            }
        }
    }

    /// 转发 daemon 的页面脚本相关方法；失败带上 daemon 返回的消息。
    async fn forward_page_call<P: serde::Serialize>(
        &self,
        method: &str,
        params: Option<P>,
    ) -> Result<Value, String> {
        self.daemon
            .call_detailed::<P, Value>(method, params)
            .await
            .map_err(|error| error.message)
    }

    async fn page_scripts(&self, msg_id: u64) -> PipeResponse {
        match self
            .forward_page_call::<Value>(fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_SCRIPTS, None)
            .await
        {
            Ok(data) => PipeResponse::ok_data(msg_id, "ok", data),
            Err(message) => PipeResponse::error(msg_id, message),
        }
    }

    async fn page_script_source(&self, msg_id: u64, payload: Value) -> PipeResponse {
        let params = match serde_json::from_value::<fluxdown_protocol::PluginPageScriptSourceParams>(
            payload,
        ) {
            Ok(params) => params,
            Err(error) => return PipeResponse::error(msg_id, error.to_string()),
        };
        let data = match self
            .forward_page_call(
                fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_SCRIPT_SOURCE,
                Some(params),
            )
            .await
        {
            Ok(data) => data,
            Err(message) => return PipeResponse::error(msg_id, message),
        };
        match serde_json::to_vec(&data) {
            Ok(bytes) if bytes.len() > SNIFF_REPLY_BUDGET => {
                PipeResponse::error(msg_id, "page script too large")
            }
            Ok(_) => PipeResponse::ok_data(msg_id, "ok", data),
            Err(error) => PipeResponse::error(msg_id, error.to_string()),
        }
    }

    /// 页面脚本调用插件处理器（daemon 侧为慢方法，按请求并发处理，不影响其它 NMH 连接）。
    /// 扩展只收到处理器返回值；插件在这次调用里开始的任务按静默捕获规则登记提示窗口。
    async fn plugin_invoke(&self, msg_id: u64, payload: Value) -> PipeResponse {
        let params =
            match serde_json::from_value::<fluxdown_protocol::PluginPageInvokeParams>(payload) {
                Ok(params) => params,
                Err(error) => return PipeResponse::error(msg_id, error.to_string()),
            };
        match self
            .daemon
            .call_detailed::<_, fluxdown_protocol::PluginPageInvokeResultDto>(
                fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_INVOKE,
                Some(params),
            )
            .await
        {
            Ok(result) => {
                self.capture
                    .announce_page_started(&result.created_task_ids, &result.resumed_task_ids);
                PipeResponse::ok_data(msg_id, "ok", serde_json::json!({ "value": result.value }))
            }
            Err(error) => PipeResponse::error(msg_id, error.message),
        }
    }

    async fn capabilities(&self, msg_id: u64) -> PipeResponse {
        let (plugins_supported, resolvers) = match self
            .daemon
            .call::<Value, Vec<PluginDto>>(fluxdown_protocol::method::DAEMON_PLUGIN_LIST, None)
            .await
        {
            Ok(plugins) => (true, resolver_infos(plugins)),
            Err(error) if matches!(error.code, ApplicationErrorCode::Unsupported) => {
                (false, Vec::new())
            }
            Err(error) => return PipeResponse::error(msg_id, format!("{:?}", error.code)),
        };
        let (plugin_dto, page_scripts) = if plugins_supported {
            (
                self.plugin_sniff_rules().await,
                self.page_scripts_index().await,
            )
        } else {
            (None, None)
        };
        let plugin_rules_sequence = plugin_dto.as_ref().map_or(0, |dto| dto.sequence);
        let plugin_rules_epoch = plugin_dto.as_ref().and_then(plugin_epoch_value);
        PipeResponse::ok_data(
            msg_id,
            "ok",
            serde_json::json!({
                "protocol": EXTENSION_PROTOCOL_VERSION,
                "actions": SUPPORTED_ACTIONS,
                "pluginsSupported": plugins_supported,
                "resolvers": resolvers,
                "pluginRulesSequence": plugin_rules_sequence,
                "pluginRulesEpoch": plugin_rules_epoch,
                "pageScriptsSequence": page_scripts.as_ref().map(|dto| dto.sequence),
                "pageScriptsEpoch": page_scripts.map(|dto| dto.epoch),
            }),
        )
    }

    async fn capture_status(&self, msg_id: u64, payload: Value) -> PipeResponse {
        let request = match serde_json::from_value::<CaptureStatusPayload>(payload) {
            Ok(request) => request,
            Err(error) => return PipeResponse::error(msg_id, error.to_string()),
        };
        let count = request.transaction_ids.len();
        if count == 0 || count > MAX_CAPTURE_STATUS_IDS {
            return PipeResponse::error(msg_id, format!("invalid transaction count: {count}"));
        }
        let statuses = self.capture.statuses(&request.transaction_ids).await;
        PipeResponse::ok_data(msg_id, "ok", serde_json::json!({ "statuses": statuses }))
    }

    async fn sniff_rules(&self, msg_id: u64, payload: Value) -> PipeResponse {
        let Some(store) = &self.sniff_rules else {
            return PipeResponse::error(msg_id, "unknown action: sniff_rules");
        };
        let request = match serde_json::from_value::<SniffRulesPayload>(payload) {
            Ok(request) => request,
            Err(error) => return PipeResponse::error(msg_id, error.to_string()),
        };
        let mut data = if request.plugin_only {
            serde_json::json!({
                "official": null,
                "community": null,
                "revocations": null,
                "status": null,
            })
        } else {
            store.query(&request.have, request.force).await
        };
        let plugin_dto = self.plugin_sniff_rules().await;
        // daemon 未答复时序号与 epoch 都为 null：扩展据此不改动 plugin 层，不把「查不到」当作「已清空」。
        let plugin_sequence = plugin_dto
            .as_ref()
            .map_or(Value::Null, |dto| Value::from(dto.sequence));
        let plugin_epoch = plugin_dto
            .as_ref()
            .and_then(plugin_epoch_value)
            .map_or(Value::Null, Value::String);
        let plugin_pack = select_plugin_pack(
            plugin_dto,
            request.have.plugin(),
            request.have.plugin_epoch(),
        );
        if let Some(object) = data.as_object_mut() {
            object.insert("plugin".to_string(), plugin_pack);
            object.insert("pluginSequence".to_string(), plugin_sequence);
            object.insert("pluginEpoch".to_string(), plugin_epoch);
        }
        let deferred = defer_oversized_docs(&mut data);
        if let Some(object) = data.as_object_mut() {
            object.insert("deferred".to_string(), serde_json::json!(deferred));
        }
        PipeResponse::ok_data(msg_id, "ok", data)
    }

    async fn task_list(&self, msg_id: u64) -> PipeResponse {
        // 大任务表不能一次取回：分页拉取，每页只留简报需要的行（未完成的全部 + 最近完成的
        // 若干），内存与任务总数无关。
        let limit = fluxdown_protocol::TASK_PAGE_MAX_LIMIT;
        let mut kept: Vec<TaskDto> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = serde_json::json!({ "cursor": cursor, "limit": limit });
            let page = match self
                .daemon
                .call::<Value, Vec<TaskDto>>(
                    fluxdown_protocol::method::DAEMON_TASK_LIST,
                    Some(params),
                )
                .await
            {
                Ok(page) => page,
                Err(error) => return PipeResponse::error(msg_id, format!("{:?}", error.code)),
            };
            let last = page.len() < limit;
            cursor = page.last().map(|task| task.task_id.clone());
            kept.extend(page);
            kept = trim_completed(kept);
            if last || cursor.is_none() {
                break;
            }
        }
        let tasks = kept;
        let speeds = self
            .task_events
            .as_ref()
            .map(crate::task_events::TaskEventHub::live_speeds)
            .unwrap_or_default();
        PipeResponse::tasks(msg_id, select_task_briefs(tasks, &speeds))
    }

    async fn task_operation(&self, msg_id: u64, payload: Value) -> PipeResponse {
        let operation = match serde_json::from_value::<TaskOperation>(payload) {
            Ok(operation) => operation,
            Err(error) => return PipeResponse::error(msg_id, error.to_string()),
        };
        let method = match operation.op.as_str() {
            "pause" => fluxdown_protocol::method::DAEMON_TASK_PAUSE,
            "resume" => fluxdown_protocol::method::DAEMON_TASK_RESUME,
            "remove" => fluxdown_protocol::method::DAEMON_TASK_DELETE,
            other => return PipeResponse::error(msg_id, format!("unknown task op: {other}")),
        };
        let params = if operation.op == "remove" {
            serde_json::json!({"taskId": operation.task_id, "deleteFiles": false})
        } else {
            serde_json::json!({"taskId": operation.task_id})
        };
        match self.daemon.call::<Value, Value>(method, Some(params)).await {
            Ok(_) => PipeResponse::ok(msg_id, "ok"),
            Err(error) => PipeResponse::error(msg_id, format!("{:?}", error.code)),
        }
    }

    async fn platform_action(&self, msg_id: u64, payload: Value, reveal: bool) -> PipeResponse {
        let request = match serde_json::from_value::<TaskIdPayload>(payload) {
            Ok(request) => request,
            Err(error) => return PipeResponse::error(msg_id, error.to_string()),
        };
        let task = match self
            .daemon
            .call::<Value, TaskDto>(
                fluxdown_protocol::method::DAEMON_TASK_GET,
                Some(serde_json::json!({"taskId": request.task_id})),
            )
            .await
        {
            Ok(task) => task,
            Err(error) => return PipeResponse::error(msg_id, format!("{:?}", error.code)),
        };
        if task.status != 3 {
            return PipeResponse::error(msg_id, "task is not completed");
        }
        let outcome = if reveal {
            crate::platform::reveal_task(&task)
        } else {
            crate::platform::open_task(&task)
        };
        match outcome {
            Ok(()) => PipeResponse::ok(msg_id, "ok"),
            Err(error) => PipeResponse::error(msg_id, error.to_string()),
        }
    }
}

/// daemon 的 plugin 层 epoch；空串（旧 daemon 无 epoch）视为缺失。
fn plugin_epoch_value(dto: &fluxdown_protocol::PluginSniffRulesDto) -> Option<String> {
    (!dto.epoch.is_empty()).then(|| dto.epoch.clone())
}

/// plugin 层嗅探规则包的应答值：缺省 `have`、daemon 序号更新或 epoch 与扩展持有的不同
/// （数据库重置后序号重新计数，旧序号作废）时给 pack，否则 `null`。
fn select_plugin_pack(
    dto: Option<fluxdown_protocol::PluginSniffRulesDto>,
    have: Option<u64>,
    have_epoch: Option<&str>,
) -> Value {
    dto.filter(|dto| {
        let epoch_changed = !dto.epoch.is_empty() && have_epoch != Some(dto.epoch.as_str());
        epoch_changed || have.is_none_or(|known| dto.sequence > known)
    })
    .and_then(|dto| dto.pack)
    .unwrap_or(Value::Null)
}

/// 应答 data 的序列化体积预算：NMH 单帧上限 1 MiB（浏览器 → 扩展方向同口径），留出信封余量。
const SNIFF_REPLY_BUDGET: usize = 900 * 1024;

/// 文档合计超过 [`SNIFF_REPLY_BUDGET`] 时把后面的文档置 `null` 并返回其名（`deferred`），
/// 扩展处理完已收到的文档后再次查询取走剩余部分。第一份文档无论多大都保留，保证每轮有进展。
/// 顺序：吊销列表（体积小、安全相关）→ official → community → plugin。
fn defer_oversized_docs(data: &mut Value) -> Vec<&'static str> {
    let mut deferred = Vec::new();
    let Some(object) = data.as_object_mut() else {
        return deferred;
    };
    let mut used = 0_usize;
    let mut kept_any = false;
    for key in ["revocations", "official", "community", "plugin"] {
        let size = match object.get(key) {
            Some(value) if !value.is_null() => value.to_string().len(),
            _ => continue,
        };
        if kept_any && used.saturating_add(size) > SNIFF_REPLY_BUDGET {
            object.insert(key.to_string(), Value::Null);
            deferred.push(key);
        } else {
            used = used.saturating_add(size);
            kept_any = true;
        }
    }
    deferred
}

/// 只保留声明了 resolver URL 的插件；供扩展判断页面能否交给桌面端解析。
fn resolver_infos(plugins: Vec<PluginDto>) -> Vec<Value> {
    plugins
        .into_iter()
        .filter(|plugin| !plugin.resolver_urls.is_empty())
        .map(|plugin| {
            serde_json::json!({
                "identity": plugin.identity,
                "name": plugin.name,
                "enabled": plugin.enabled,
                "loaded": plugin.load_status == "Loaded",
                "urls": plugin.resolver_urls,
            })
        })
        .collect()
}

/// 只留简报需要的行：未完成的全部，加上最近完成的 `MAX_COMPLETED_TASKS` 个（未完成在前）。
fn trim_completed(tasks: Vec<TaskDto>) -> Vec<TaskDto> {
    let (mut completed, active): (Vec<_>, Vec<_>) =
        tasks.into_iter().partition(|task| task.status == 3);
    completed
        .sort_by_key(|task| std::cmp::Reverse(task.created_at.parse::<i64>().unwrap_or_default()));
    completed.truncate(MAX_COMPLETED_TASKS);
    active.into_iter().chain(completed).collect()
}

fn select_task_briefs(
    tasks: Vec<TaskDto>,
    speeds: &std::collections::HashMap<String, LiveSpeed>,
) -> Vec<TaskBrief> {
    trim_completed(tasks)
        .into_iter()
        .map(|task| TaskBrief {
            speed: speeds
                .get(&task.task_id)
                .map_or(0, |speed| speed.download_bps),
            task_id: task.task_id,
            file_name: task.file_name,
            status: task.status,
            downloaded_bytes: task.downloaded_bytes,
            total_bytes: task.total_bytes,
            error_message: task.error_message,
            created_at: task.created_at,
        })
        .collect()
}

async fn handle_stream<S>(mut stream: S, service: NmhService) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let mut length = [0_u8; 4];
        if let Err(error) = stream.read_exact(&mut length).await {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                return Ok(());
            }
            return Err(error);
        }
        let length = u32::from_le_bytes(length);
        if length == 0 || length > MAX_MESSAGE_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "native messaging frame is invalid",
            ));
        }
        let mut payload = vec![0_u8; length as usize];
        stream.read_exact(&mut payload).await?;
        let response = match serde_json::from_slice::<PipeMessage>(&payload) {
            Ok(message) => service.dispatch(message).await,
            Err(error) => PipeResponse::error(0, error.to_string()),
        };
        let bytes = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
        let length =
            u32::try_from(bytes.len()).map_err(|error| std::io::Error::other(error.to_string()))?;
        stream.write_all(&length.to_le_bytes()).await?;
        stream.write_all(&bytes).await?;
        stream.flush().await?;
    }
}

#[cfg(unix)]
async fn run_server(
    service: NmhService,
    cancel: CancellationToken,
    ready: tokio::sync::oneshot::Sender<()>,
) -> Result<(), std::io::Error> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let path = unix_socket_path().ok_or_else(endpoint_unavailable)?;
    let dir = path.parent().ok_or_else(endpoint_unavailable)?;
    // 0700 目录：其它用户无法进入、连接或抢占 socket 路径；属主不是当前用户时 chmod 失败，
    // 端点直接不可用而不是退回共享位置。
    tokio::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .await?;
    tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).await?;
    let owner_uid = tokio::fs::metadata(dir).await?.uid();
    if tokio::fs::try_exists(&path).await? {
        match tokio::net::UnixStream::connect(&path).await {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "native messaging socket is already active",
                ));
            }
            Err(_) => tokio::fs::remove_file(&path).await?,
        }
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
    if ready.send(()).is_err() {
        tracing::debug!("NMH startup waiter closed before socket became ready");
    }
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                drop(listener);
                return match tokio::fs::remove_file(&path).await {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                };
            }
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        // 瞬时 accept 错误（如 fd 耗尽）退避重试，不让 NMH 端点连带终止 agent。
                        tracing::warn!(error = %error, "NMH socket accept failed");
                        tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                        continue;
                    }
                };
                // 0700 目录之外的纵深防御：对端 uid 必须等于 socket 目录属主（本进程用户）。
                if !stream.peer_cred().is_ok_and(|cred| cred.uid() == owner_uid) {
                    tracing::warn!("NMH socket rejected a connection from another user");
                    continue;
                }
                let service = service.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_stream(stream, service).await {
                        tracing::debug!(error = %error, "NMH socket closed");
                    }
                });
            }
        }
    }
}

/// Unix IPC socket：`<数据目录>/ipc/fluxdown.sock`，`ipc` 是仅当前用户可进入的 0700 目录。
/// 数据目录刻意放在 home 下：宿主与 Flatpak/Snap 沙箱都可达（`$XDG_RUNTIME_DIR` 在沙箱内会被
/// 重映射）。中继（`native/nmh/src/main.rs` 的 `socket_path_under`）独立推导同一路径，
/// 两侧测试用同一组字面量钉住。
#[cfg(any(unix, test))]
fn socket_path_under(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    let data_dir = home
        .join("Library")
        .join("Application Support")
        .join("fluxdown");
    #[cfg(not(target_os = "macos"))]
    let data_dir = home.join(".local").join("share").join("fluxdown");
    data_dir.join("ipc").join("fluxdown.sock")
}

/// 无 home 目录时没有端点（不回退到任何共享目录）。
#[cfg(unix)]
fn unix_socket_path() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new().map(|dirs| socket_path_under(dirs.home_dir()))
}

#[cfg(windows)]
async fn run_server(
    service: NmhService,
    cancel: CancellationToken,
    ready: tokio::sync::oneshot::Sender<()>,
) -> Result<(), std::io::Error> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_name = pipe_name().ok_or_else(endpoint_unavailable)?;
    let mut first = true;
    let mut ready = Some(ready);
    loop {
        let server = ServerOptions::new()
            .first_pipe_instance(first)
            .create(&pipe_name)?;
        first = false;
        if let Some(ready) = ready.take()
            && ready.send(()).is_err()
        {
            tracing::debug!("NMH startup waiter closed before pipe became ready");
        }
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            connected = server.connect() => {
                if let Err(error) = connected {
                    // 单个客户端的握手失败不应让整个 NMH 端点（乃至 agent）退出。
                    tracing::warn!(error = %error, "NMH pipe connect failed");
                    tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                    continue;
                }
            }
        }
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_stream(server, service).await {
                tracing::debug!(error = %error, "NMH pipe closed");
            }
        });
    }
}

/// 中继拨号的 IPC 端点（unix socket 路径或命名管道名）；无法确定当前用户时为 `unavailable`。
#[must_use]
pub fn ipc_endpoint() -> String {
    #[cfg(unix)]
    {
        unix_socket_path().map_or_else(
            || "unavailable".to_owned(),
            |path| path.display().to_string(),
        )
    }
    #[cfg(windows)]
    {
        pipe_name().unwrap_or_else(|| "unavailable".to_owned())
    }
}

/// Windows Named Pipe for the current account: `\\.\pipe\fluxdown-<account>`。账户名转小写后，
/// `[a-z0-9]` 之外的每个字节编码为 `_xx`（下划线本身也编码），不同账户不会落到同一管道。
/// 中继（`native/nmh/src/main.rs` 的 `pipe_name_for`）独立推导同一名字，两侧测试用同一组字面量钉住。
#[cfg(any(windows, test))]
fn pipe_name_for(user: &str) -> Option<String> {
    if user.is_empty() {
        return None;
    }
    let mut name = String::from(r"\\.\pipe\fluxdown-");
    for byte in user.to_lowercase().bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            name.push(char::from(byte));
        } else {
            name.push_str(&format!("_{byte:02x}"));
        }
    }
    Some(name)
}

#[cfg(windows)]
fn pipe_name() -> Option<String> {
    std::env::var("USERNAME")
        .ok()
        .and_then(|user| pipe_name_for(&user))
}

/// 以中继的长度帧协议向本进程 IPC 端点发送 `ping`，成功返回 `pong` 载荷。
pub async fn probe_ipc(timeout: std::time::Duration) -> Result<String, std::io::Error> {
    tokio::time::timeout(timeout, async {
        #[cfg(unix)]
        let stream = {
            let path = unix_socket_path().ok_or_else(endpoint_unavailable)?;
            tokio::net::UnixStream::connect(path).await?
        };
        #[cfg(windows)]
        let stream = {
            let name = pipe_name().ok_or_else(endpoint_unavailable)?;
            tokio::net::windows::named_pipe::ClientOptions::new().open(name)?
        };
        ping_stream(stream).await
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "IPC ping timed out"))?
}

fn endpoint_unavailable() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "native messaging IPC endpoint is unavailable (no home directory / account name)",
    )
}

async fn ping_stream<S>(mut stream: S) -> Result<String, std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = serde_json::to_vec(&serde_json::json!({ "action": "ping", "msg_id": 1 }))
        .map_err(std::io::Error::other)?;
    let length =
        u32::try_from(request.len()).map_err(|error| std::io::Error::other(error.to_string()))?;
    stream.write_all(&length.to_le_bytes()).await?;
    stream.write_all(&request).await?;
    stream.flush().await?;
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).await?;
    let length = u32::from_le_bytes(length);
    if length == 0 || length > MAX_MESSAGE_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "IPC ping response frame is invalid",
        ));
    }
    let mut payload = vec![0_u8; length as usize];
    stream.read_exact(&mut payload).await?;
    let response: Value = serde_json::from_slice(&payload)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let success = response.get("success").and_then(Value::as_bool) == Some(true);
    match response.get("message").and_then(Value::as_str) {
        Some("pong") if success => Ok("pong".to_owned()),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unexpected IPC ping response: {response}"),
        )),
    }
}

/// 浏览器 Native Messaging Host 清单注册：写出指向 `fluxdown_nmh` 中继的清单，
/// 并为 Doctor 提供只读诊断快照。
///
/// 注册目标与 Flutter 时代的 hub 完全一致（Chrome/Edge/Firefox 及各 Chromium 分支），
/// 但中继二进制按 `fluxdown-agent` 的同级目录查找。
///
/// 同一台机器可能并存多份 FluxDown（Flutter / GPUI、安装版 / 开发构建），它们共用
/// 同一个注册入口（类 Unix 的启动脚本、Windows 的 HKCU 键）。启动自愈
/// [`auto_register`] 按 [`may_take_over`] 的归属规则决定是否改指向本安装，避免两份
/// 安装每次启动互相覆盖；但另一份安装的中继若实测连不到正在运行的本 agent（端点或帧
/// 协议不同），保留它只会让扩展显示未连接，此时总是接管。Doctor 的显式修复 [`register`]
/// 总是指向本安装。按路径判定的归属规则保留旧安装兼容，另以连通性实测
/// 判断旧中继是否仍可使用。
pub mod registry {
    use std::io;
    use std::path::{Path, PathBuf};

    use serde::Serialize;
    use serde_json::Value;

    const NMH_NAME: &str = "com.fluxdown.nmh";
    const NMH_DESCRIPTION: &str = "FluxDown Native Messaging Host";
    #[cfg(windows)]
    const NMH_EXE_NAME: &str = "fluxdown_nmh.exe";
    #[cfg(not(windows))]
    const NMH_EXE_NAME: &str = "fluxdown_nmh";
    /// Chrome 扩展 ID（wxt.config.ts 里通过 `key` 固定）。
    const CHROME_EXTENSION_ID: &str = "chrome-extension://meleenglfggcmcajknpeeeiobnpfmahc/";
    /// Edge 商店扩展 ID：Edge 忽略清单 `key`，必须单独放行，否则 connectNative 报 forbidden。
    const EDGE_EXTENSION_ID: &str = "chrome-extension://nglkkjbogjghekbhhcnccnpfedjbdhhd/";
    const FIREFOX_EXTENSION_ID: &str = "fluxdown@fluxdown.app";
    /// Windows 的 [`register_with`] 无条件写全部注册表键，所以未安装的浏览器也必须完好；
    /// 类 Unix 只给已安装的浏览器写清单。
    const REGISTERS_EVERY_TARGET: bool = cfg!(windows);
    /// 开发构建与临时挂载路径的特征片段（统一小写、`/` 分隔后匹配）。
    const TRANSIENT_RELAY_MARKERS: [&str; 7] = [
        "/target/debug/",
        "/target/release/",
        "/build/macos/build/products/",
        "/build/linux/",
        "/build/windows/",
        "/.mount_",
        "/apptranslocation/",
    ];

    /// 单个浏览器的 NMH 注册状态。
    #[derive(Debug, Clone)]
    pub struct NmhTarget {
        /// 展示名，如 `Chrome` / `Firefox` / `Brave (Flatpak)`。
        pub label: String,
        /// 注册位置：Windows 为注册表键，类 Unix 为清单文件绝对路径。
        pub location: String,
        /// 浏览器是否安装（配置根目录存在）；false 时 Doctor 只报 info。
        pub installed: bool,
        /// 清单完整且与当前生效的注册一致（中继归属由 [`NmhDiagnosis::relay_owner`] 单独给出）。
        pub ok: bool,
        /// `ok == false` 时的具体原因；ok 时为空串。
        pub issue: String,
    }

    /// 注册当前指向的中继相对本安装的归属。
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum RelayOwner {
        /// 未注册：类 Unix 启动脚本不存在；Windows 三个注册表键都不存在。
        #[default]
        Missing,
        /// 指向本安装的中继。
        Current,
        /// 指向另一份仍存在的 FluxDown 中继：扩展可用，浏览器冷启动会拉起那份安装。
        OtherInstall,
        /// 指向的中继不存在或无法解析（安装被移除、AppImage 挂载点失效、脚本被改坏）。
        Broken,
    }

    /// NMH 注册整体诊断快照。
    #[derive(Debug, Clone, Default)]
    pub struct NmhDiagnosis {
        /// 中继可执行文件绝对路径；空表示未找到。
        pub exe_path: String,
        /// 未找到中继时的原因；找到时为空串。
        pub exe_error: String,
        /// Chromium 清单**期望**路径（可能尚未写出）。
        pub chromium_manifest: String,
        /// Firefox 清单期望路径。
        pub firefox_manifest: String,
        /// 注册入口：类 Unix 为启动脚本路径，Windows 为提供生效中继的注册表键。
        pub relay_location: String,
        /// 注册实际指向的中继；未注册或无法解析时为空。
        pub registered_relay: String,
        /// `registered_relay` 相对本安装的归属。
        pub relay_owner: RelayOwner,
        /// 每个浏览器一条；未找到中继时为空。
        pub targets: Vec<NmhTarget>,
        /// 类 Unix：属于其他用户（通常是曾以 sudo 运行）的启动脚本 / 清单及其目录；以后的
        /// 自动注册改不了它们。
        pub foreign_owned: Vec<String>,
        /// Windows：限制原生消息主机的组织策略。
        pub policy_blocks: Vec<PolicyBlock>,
        /// Windows：Chrome 与 Edge 都开启了 `NativeHostsExecutablesLaunchDirectly`，浏览器直接
        /// 启动中继、不经 `cmd.exe`。
        pub direct_launch: bool,
        /// Windows 且仍有浏览器经 `%ComSpec% /d /c` 启动主机时：`ComSpec` 的配置值与本进程值。
        pub comspec: Option<super::comspec::ComSpecState>,
    }

    impl NmhDiagnosis {
        /// 浏览器启动原生消息主机所执行的入口（诊断详情用）：类 Unix 为启动脚本；Windows 为
        /// `<ComSpec> /d /c <中继>`，直接启动或 `ComSpec` 不可用时为中继本身。
        #[must_use]
        pub fn launch_entry(&self) -> String {
            if !cfg!(windows) {
                return self.relay_location.clone();
            }
            match self
                .comspec
                .as_ref()
                .and_then(super::comspec::ComSpecState::launcher)
            {
                Some(launcher) if !self.direct_launch => {
                    format!("{launcher} /d /c {}", self.registered_relay)
                }
                _ => self.registered_relay.clone(),
            }
        }
    }

    /// 让浏览器拉不起 FluxDown 原生消息主机的组织策略（Windows 组策略 / 注册表）。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolicyBlock {
        /// `NativeMessagingUserLevelHosts = 0`：浏览器忽略 HKCU 下的注册（FluxDown 只注册用户级）。
        UserLevelHostsDisabled { browser: String },
        /// `NativeMessagingBlocklist` 命中（`*` 或本主机名）且 `Allowlist` 未放行。
        Blocklisted { browser: String },
        /// `DisableCMD = 1`：Chrome / Edge 经 `cmd.exe` 启动原生消息主机，命令提示符被禁用即无法启动。
        CommandPromptDisabled,
    }

    /// [`register`] 遇到的写入被拒位置。
    #[derive(Debug, Default, PartialEq, Eq)]
    pub struct RegisterIssues {
        /// 需要还给当前用户的目录（只改目录本身：写入走「临时文件 + rename」，目录可写即可替换
        /// 其中属于其他用户的旧文件；浏览器清单目录还放着别家原生消息主机的清单，不能递归）。
        /// 目录不存在时取最近的已存在上级。
        pub denied: Vec<PathBuf>,
    }

    impl RegisterIssues {
        fn deny(&mut self, dir: &Path) {
            let target = dir
                .ancestors()
                .find(|candidate| !candidate.as_os_str().is_empty() && candidate.exists())
                .unwrap_or(dir)
                .to_path_buf();
            if !self.denied.contains(&target) {
                self.denied.push(target);
            }
        }
    }

    /// 浏览器式拉起中继并经它 `ping` 的失败原因。
    #[derive(Debug, thiserror::Error)]
    pub enum RelayLaunchError {
        /// 无法启动：缺执行权限、noexec 挂载、文件不存在、被安全软件拦截。
        #[error("cannot start: {0}")]
        Spawn(#[source] io::Error),
        /// 进程以失败状态退出且没有回 `pong`（126 = 不可执行、127 = 找不到程序、命令提示符被禁用等）。
        #[error("exited without replying ({0})")]
        Exited(String),
        /// 时限内没有回 `pong`（中继与 agent 端点 / 帧协议不一致，或 agent 端点无应答）。
        #[error("no reply: {0}")]
        NoReply(String),
    }

    impl RelayLaunchError {
        /// 启动被系统以权限理由拒绝。
        #[must_use]
        pub fn permission_denied(&self) -> bool {
            matches!(self, Self::Spawn(error) if error.kind() == io::ErrorKind::PermissionDenied)
        }
    }

    /// [`auto_register`] 的结果。
    #[derive(Debug, PartialEq, Eq)]
    pub enum AutoRegisterOutcome {
        /// 注册完整，未改动任何文件。
        UpToDate,
        /// 已重写注册，指向给定中继（可能是保留下来的另一份安装）。
        Registered(PathBuf),
    }

    /// Chromium 系清单（`allowed_origins`）。
    #[derive(Serialize)]
    struct NmhManifestChromium {
        name: String,
        description: String,
        path: String,
        #[serde(rename = "type")]
        host_type: String,
        allowed_origins: Vec<String>,
    }

    /// Firefox 清单：只能有 `allowed_extensions`，多出 `allowed_origins` 会被 schema 校验拒绝。
    #[derive(Serialize)]
    struct NmhManifestFirefox {
        name: String,
        description: String,
        path: String,
        #[serde(rename = "type")]
        host_type: String,
        allowed_extensions: Vec<String>,
    }

    fn chromium_manifest_json(path: &str) -> Result<String, io::Error> {
        serde_json::to_string_pretty(&NmhManifestChromium {
            name: NMH_NAME.to_owned(),
            description: NMH_DESCRIPTION.to_owned(),
            path: path.to_owned(),
            host_type: "stdio".to_owned(),
            allowed_origins: vec![CHROME_EXTENSION_ID.to_owned(), EDGE_EXTENSION_ID.to_owned()],
        })
        .map_err(io::Error::other)
    }

    fn firefox_manifest_json(path: &str) -> Result<String, io::Error> {
        serde_json::to_string_pretty(&NmhManifestFirefox {
            name: NMH_NAME.to_owned(),
            description: NMH_DESCRIPTION.to_owned(),
            path: path.to_owned(),
            host_type: "stdio".to_owned(),
            allowed_extensions: vec![FIREFOX_EXTENSION_ID.to_owned()],
        })
        .map_err(io::Error::other)
    }

    /// 清单声明的 `path`。
    fn manifest_relay(manifest: &Value) -> Option<&str> {
        manifest.get("path").and_then(Value::as_str)
    }

    /// Chromium 清单是否放行 Edge 商店扩展。
    fn manifest_allows_edge(manifest: &Value) -> bool {
        manifest
            .get("allowed_origins")
            .and_then(Value::as_array)
            .is_some_and(|origins| {
                origins
                    .iter()
                    .any(|origin| origin.as_str() == Some(EDGE_EXTENSION_ID))
            })
    }

    /// 浏览器启动原生消息主机的 `cmd.exe` 参数行（`raw_arg` 原样追加）：外层引号让 `cmd /c`
    /// 剥掉首尾引号后仍保留中继路径的引号，路径含括号（`Program Files (x86)`）也不被拆开。
    #[cfg(any(windows, test))]
    fn cmd_launch_line(relay: &str, origin: &str) -> String {
        format!("/d /c \"\"{relay}\" {origin}\"")
    }

    /// 单个 Chromium 系浏览器的原生消息策略评估（Chrome 文档语义：`Allowlist` 可覆盖
    /// `Blocklist` 的 `*`，但不能恢复被 `NativeMessagingUserLevelHosts = 0` 禁用的用户级注册）。
    #[cfg(any(windows, test))]
    fn evaluate_browser_policy(
        browser: &str,
        user_level_hosts: Option<u32>,
        blocklist: &[String],
        allowlist: &[String],
    ) -> Vec<PolicyBlock> {
        let mut blocks = Vec::new();
        if user_level_hosts == Some(0) {
            blocks.push(PolicyBlock::UserLevelHostsDisabled {
                browser: browser.to_owned(),
            });
        }
        let listed = |list: &[String], wildcard: bool| {
            list.iter()
                .map(|entry| entry.trim())
                .any(|entry| entry == NMH_NAME || (wildcard && entry == "*"))
        };
        if listed(blocklist, true) && !listed(allowlist, false) {
            blocks.push(PolicyBlock::Blocklisted {
                browser: browser.to_owned(),
            });
        }
        blocks
    }

    /// `DisableCMD = 1` 连同脚本一起禁用命令提示符（`2` 只禁交互式窗口，`cmd /c` 仍可用）；
    /// 只有仍经 `cmd.exe` 启动主机的浏览器（未开 `NativeHostsExecutablesLaunchDirectly`）受影响。
    #[cfg(any(windows, test))]
    fn cmd_blocks_launch(disable_cmd: Option<u32>, launches_directly: &[bool]) -> bool {
        disable_cmd == Some(1) && launches_directly.iter().any(|direct| !direct)
    }

    /// 开发构建（cargo / Flutter 产物）或临时挂载（AppImage、macOS App Translocation）里的中继。
    fn is_transient_relay(path: &Path) -> bool {
        let normalized = path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        TRANSIENT_RELAY_MARKERS
            .iter()
            .any(|marker| normalized.contains(marker))
    }

    /// 两个中继路径是否指向同一文件（Windows 不区分大小写；再以 canonicalize 兜底符号链接）。
    fn same_relay(a: &Path, b: &Path) -> bool {
        let literal = if cfg!(windows) {
            a.to_string_lossy()
                .eq_ignore_ascii_case(&b.to_string_lossy())
        } else {
            a == b
        };
        literal
            || matches!(
                (std::fs::canonicalize(a), std::fs::canonicalize(b)),
                (Ok(a), Ok(b)) if a == b
            )
    }

    fn classify_relay(registered: &Path, current: &Path) -> RelayOwner {
        if same_relay(registered, current) {
            RelayOwner::Current
        } else if registered.is_file() {
            RelayOwner::OtherInstall
        } else {
            RelayOwner::Broken
        }
    }

    /// 启动自愈能否把注册改指向本安装：未注册、失效或已是本安装时总可以。另一份安装的中继
    /// 实测连不到正在运行的本 agent（`reaches_agent == Some(false)`）时必须接管；能连通或
    /// 无从实测（`None`）时，只在它是开发构建/临时路径、而本安装不是时才接管，其余保持先到者。
    fn may_take_over(
        owner: RelayOwner,
        registered: &Path,
        current: &Path,
        reaches_agent: Option<bool>,
    ) -> bool {
        match owner {
            RelayOwner::Missing | RelayOwner::Broken | RelayOwner::Current => true,
            RelayOwner::OtherInstall => {
                reaches_agent == Some(false)
                    || (is_transient_relay(registered) && !is_transient_relay(current))
            }
        }
    }

    /// 中继只认 agent 同级目录里的那一份：它与本 agent 出自同一次构建，端点与帧协议必然一致。
    /// 不回退到其它构建目录（例如另一 profile 的 `target/`）：那里的中继可能停在旧端点，
    /// 注册它等于让扩展连不上。
    fn find_nmh_exe() -> Result<PathBuf, io::Error> {
        let exe = std::env::current_exe()?;
        let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
        canonical
            .parent()
            .map(|dir| dir.join(NMH_EXE_NAME))
            .filter(|candidate| candidate.is_file())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "{NMH_EXE_NAME} not found next to {}. Build it with: cargo build -p fluxdown_nmh",
                        canonical.display()
                    ),
                )
            })
    }

    /// 实测中继的上限；免拉起的 `ping` 在本机往返只需毫秒级，余量留给安全软件的首次扫描。
    const RELAY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
    /// `ping` 失败后等子进程退出以取得退出码的上限。
    const RELAY_EXIT_GRACE: std::time::Duration = std::time::Duration::from_millis(500);
    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// 拉起 `command` 并经其标准输入输出发一条 `ping`：拿到本进程 IPC 端点回的 `pong` 才说明
    /// 这条拉起路径、中继与正在运行的 agent 端点、帧协议都一致。子进程随句柄丢弃被结束。
    async fn launch_and_ping(mut command: tokio::process::Command) -> Result<(), RelayLaunchError> {
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        let mut child = command.spawn().map_err(RelayLaunchError::Spawn)?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(RelayLaunchError::NoReply("stdio unavailable".to_owned()));
        };
        let reply = tokio::time::timeout(
            RELAY_PROBE_TIMEOUT,
            super::ping_stream(tokio::io::join(stdout, stdin)),
        )
        .await;
        let detail = match reply {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(error)) => error.to_string(),
            Err(_) => format!("timed out after {}s", RELAY_PROBE_TIMEOUT.as_secs()),
        };
        // 标准输入已随 ping 流关闭：正常的中继会以 0 退出，失败退出码才说明启动本身出了问题。
        match tokio::time::timeout(RELAY_EXIT_GRACE, child.wait()).await {
            Ok(Ok(status)) if !status.success() => {
                Err(RelayLaunchError::Exited(status.to_string()))
            }
            _ => Err(RelayLaunchError::NoReply(detail)),
        }
    }

    /// 直接拉起另一份安装的中继（参数为扩展 origin），实测它能否连到本 agent。
    async fn relay_reaches_agent(relay: &Path) -> bool {
        let mut command = tokio::process::Command::new(relay);
        command.arg(CHROME_EXTENSION_ID);
        launch_and_ping(command).await.is_ok()
    }

    /// 像浏览器那样拉起当前生效的注册并经它 `ping`：类 Unix 执行清单指向的启动脚本，Windows
    /// 与 Chrome / Edge 一样经 `%ComSpec% /d /c` 启动中继（浏览器都开了
    /// `NativeHostsExecutablesLaunchDirectly` 时直接启动）。`ComSpec` 取注册表配置值——即重启后
    /// 的浏览器会拿到的值；本进程环境可能还是修改前继承的旧值。注册缺失 / 失效、`ComSpec` 不可用
    /// （由 `nmh_comspec` 检查报告），或本进程 IPC 端点不在线（由监听检查报告，重新注册修不好）
    /// 时返回 `None`。
    pub async fn probe_browser_launch(
        diagnosis: &NmhDiagnosis,
    ) -> Option<Result<(), RelayLaunchError>> {
        if !matches!(
            diagnosis.relay_owner,
            RelayOwner::Current | RelayOwner::OtherInstall
        ) {
            return None;
        }
        super::probe_ipc(RELAY_PROBE_TIMEOUT).await.ok()?;
        #[cfg(unix)]
        let command = {
            let mut command = tokio::process::Command::new(&diagnosis.relay_location);
            command.arg(CHROME_EXTENSION_ID);
            command
        };
        #[cfg(windows)]
        let command = if diagnosis.direct_launch {
            let mut command = tokio::process::Command::new(&diagnosis.registered_relay);
            command.arg(CHROME_EXTENSION_ID);
            command
        } else {
            let launcher = diagnosis
                .comspec
                .as_ref()
                .and_then(super::comspec::ComSpecState::launcher)?;
            let mut command = tokio::process::Command::new(launcher);
            command.raw_arg(cmd_launch_line(
                &diagnosis.registered_relay,
                CHROME_EXTENSION_ID,
            ));
            command
        };
        Some(launch_and_ping(command).await)
    }

    /// 显式修复：把全部注册改指向本安装的中继。写入被拒的位置随结果返回，由调用方决定是否
    /// 请求管理员授权释放后重试；其它错误直接失败。
    pub fn register() -> Result<RegisterIssues, io::Error> {
        register_with(&find_nmh_exe()?)
    }

    /// 本安装的中继属于当前用户却缺执行位时补上，返回是否改动；不属于当前用户的不碰
    /// （安装包内的文件由重装恢复）。
    #[cfg(unix)]
    pub fn ensure_relay_executable() -> Result<bool, io::Error> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let relay = find_nmh_exe()?;
        let metadata = std::fs::metadata(&relay)?;
        let mode = metadata.permissions().mode() & 0o7777;
        if mode & 0o111 == 0o111 || metadata.uid() != crate::permission::effective_uid()? {
            return Ok(false);
        }
        std::fs::set_permissions(&relay, std::fs::Permissions::from_mode(mode | 0o511))?;
        tracing::info!(relay = %relay.display(), "restored execute permission of the NMH relay");
        Ok(true)
    }

    /// 启动自愈：注册缺失、失效、不完整，或指向连不到本 agent 的另一份中继时按归属规则重写，
    /// 完好时不碰任何文件。`endpoint_live` 表示本进程 IPC 端点已在监听：只有这时另一份安装的
    /// 中继才能实测连通性，否则只按路径规则判定。写入被拒（属于其他用户的旧注册）时报错，
    /// 由 Doctor 的「重新注册」请求授权修复。
    pub async fn auto_register(endpoint_live: bool) -> Result<AutoRegisterOutcome, io::Error> {
        let diagnosis = tokio::task::spawn_blocking(diagnose)
            .await
            .map_err(io::Error::other)?;
        if diagnosis.exe_path.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, diagnosis.exe_error));
        }
        let current = PathBuf::from(&diagnosis.exe_path);
        let registered = PathBuf::from(&diagnosis.registered_relay);
        let reaches_agent = if endpoint_live && diagnosis.relay_owner == RelayOwner::OtherInstall {
            let reachable = relay_reaches_agent(&registered).await;
            if !reachable {
                tracing::info!(
                    relay = %registered.display(),
                    "registered NMH relay cannot reach this agent; registering this installation's relay"
                );
            }
            Some(reachable)
        } else {
            None
        };
        let relay = if may_take_over(diagnosis.relay_owner, &registered, &current, reaches_agent) {
            current
        } else {
            registered.clone()
        };
        let complete = matches!(
            diagnosis.relay_owner,
            RelayOwner::Current | RelayOwner::OtherInstall
        ) && same_relay(&relay, &registered)
            && diagnosis
                .targets
                .iter()
                .all(|target| target.ok || !(target.installed || REGISTERS_EVERY_TARGET));
        if complete {
            return Ok(AutoRegisterOutcome::UpToDate);
        }
        let target = relay.clone();
        let issues = tokio::task::spawn_blocking(move || register_with(&target))
            .await
            .map_err(io::Error::other)??;
        if !issues.denied.is_empty() {
            let paths = issues
                .denied
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("NMH registration blocked by permissions: {paths}"),
            ));
        }
        Ok(AutoRegisterOutcome::Registered(relay))
    }

    #[cfg(unix)]
    pub use unix::diagnose;
    #[cfg(unix)]
    use unix::register_with;
    #[cfg(windows)]
    pub use windows::diagnose;
    #[cfg(windows)]
    use windows::register_with;

    #[cfg(test)]
    mod tests {
        use std::path::Path;

        use super::{
            PolicyBlock, RegisterIssues, RelayOwner, classify_relay, cmd_blocks_launch,
            cmd_launch_line, evaluate_browser_policy, is_transient_relay, may_take_over,
        };

        const INSTALLED: &str = "/Applications/FluxDown.app/Contents/MacOS/fluxdown_nmh";
        const DEV: &str = "/Users/dev/FluxDown/target/release/fluxdown_nmh";

        #[test]
        fn transient_relays_cover_dev_builds_and_temporary_mounts() {
            for path in [
                DEV,
                "/Users/dev/FluxDown/build/macos/Build/Products/Debug/FluxDown.app/Contents/MacOS/fluxdown_nmh",
                "/tmp/.mount_FluxDoAbc123/usr/bin/fluxdown_nmh",
                "/private/var/folders/x/AppTranslocation/1234/d/FluxDown.app/Contents/MacOS/fluxdown_nmh",
                r"D:\code\FluxDown\target\debug\fluxdown_nmh.exe",
            ] {
                assert!(is_transient_relay(Path::new(path)), "{path}");
            }
            for path in [
                INSTALLED,
                "/opt/fluxdown/fluxdown_nmh",
                r"C:\Program Files\FluxDown\fluxdown_nmh.exe",
            ] {
                assert!(!is_transient_relay(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn other_install_is_kept_only_while_it_reaches_this_agent() {
            let other_installed = Path::new("/opt/fluxdown/fluxdown_nmh");
            // 能连通或无从实测时保持先到者，只有开发构建让位给安装版。
            for reaches_agent in [Some(true), None] {
                assert!(!may_take_over(
                    RelayOwner::OtherInstall,
                    other_installed,
                    Path::new(INSTALLED),
                    reaches_agent
                ));
                assert!(!may_take_over(
                    RelayOwner::OtherInstall,
                    other_installed,
                    Path::new(DEV),
                    reaches_agent
                ));
                assert!(may_take_over(
                    RelayOwner::OtherInstall,
                    Path::new(DEV),
                    Path::new(INSTALLED),
                    reaches_agent
                ));
            }
            // 连不到本 agent 的中继（旧端点 / 旧帧协议）对本次运行无用：开发构建也要接管。
            for current in [INSTALLED, DEV] {
                assert!(may_take_over(
                    RelayOwner::OtherInstall,
                    other_installed,
                    Path::new(current),
                    Some(false)
                ));
            }
            for owner in [RelayOwner::Missing, RelayOwner::Broken, RelayOwner::Current] {
                assert!(may_take_over(owner, other_installed, Path::new(DEV), None));
            }
        }

        #[test]
        fn classify_distinguishes_current_other_and_missing_relays() {
            let dir = std::env::temp_dir().join(format!(
                "fluxdown_nmh_owner_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).expect("create relay test directory");
            let current = dir.join("current_nmh");
            let other = dir.join("other_nmh");
            std::fs::write(&current, b"").expect("create current relay");
            std::fs::write(&other, b"").expect("create other relay");
            assert_eq!(classify_relay(&current, &current), RelayOwner::Current);
            assert_eq!(classify_relay(&other, &current), RelayOwner::OtherInstall);
            assert_eq!(
                classify_relay(&dir.join("removed_nmh"), &current),
                RelayOwner::Broken
            );
            if let Err(error) = std::fs::remove_dir_all(&dir) {
                tracing::warn!(path = %dir.display(), %error, "relay ownership test cleanup failed");
            }
        }

        fn list(items: &[&str]) -> Vec<String> {
            items.iter().map(|item| (*item).to_owned()).collect()
        }

        #[test]
        fn browser_policy_follows_chrome_precedence() {
            assert!(evaluate_browser_policy("Chrome", None, &[], &[]).is_empty());
            assert!(evaluate_browser_policy("Chrome", Some(1), &list(&["other"]), &[]).is_empty());
            assert_eq!(
                evaluate_browser_policy("Edge", None, &list(&["*"]), &[]),
                [PolicyBlock::Blocklisted {
                    browser: "Edge".to_owned()
                }]
            );
            assert!(
                evaluate_browser_policy(
                    "Edge",
                    None,
                    &list(&["*"]),
                    &list(&[" com.fluxdown.nmh "])
                )
                .is_empty()
            );
            // `Allowlist` 里的 `*` 不放行具体主机；用户级主机被禁用时 Allowlist 也救不回来。
            assert_eq!(
                evaluate_browser_policy(
                    "Chrome",
                    Some(0),
                    &list(&["com.fluxdown.nmh"]),
                    &list(&["*"])
                ),
                [
                    PolicyBlock::UserLevelHostsDisabled {
                        browser: "Chrome".to_owned()
                    },
                    PolicyBlock::Blocklisted {
                        browser: "Chrome".to_owned()
                    }
                ]
            );
            assert!(cmd_blocks_launch(Some(1), &[false, true]));
            assert!(!cmd_blocks_launch(Some(1), &[true, true]));
            assert!(!cmd_blocks_launch(Some(2), &[false, false]));
            assert!(!cmd_blocks_launch(None, &[false]));
        }

        #[test]
        fn cmd_launch_line_keeps_relay_quoted_inside_outer_quotes() {
            assert_eq!(
                cmd_launch_line(
                    r"C:\Program Files (x86)\FluxDown\fluxdown_nmh.exe",
                    "chrome-extension://abc/"
                ),
                r#"/d /c ""C:\Program Files (x86)\FluxDown\fluxdown_nmh.exe" chrome-extension://abc/""#
            );
        }

        #[test]
        fn denied_dirs_are_released_alone_and_missing_ones_at_the_parent()
        -> Result<(), std::io::Error> {
            let dir = std::env::temp_dir().join(format!(
                "fluxdown_nmh_deny_{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(dir.join("existing"))?;
            let mut issues = RegisterIssues::default();
            issues.deny(&dir.join("existing"));
            issues.deny(&dir.join("missing").join("deeper"));
            issues.deny(&dir.join("existing"));
            assert_eq!(issues.denied, [dir.join("existing"), dir.clone()]);
            std::fs::remove_dir_all(&dir)
        }

        /// 真实拉起：启动失败、失败退出与正常退出但不回 `pong` 三种结果分得开。
        #[cfg(unix)]
        #[tokio::test]
        async fn launch_failures_are_classified() -> Result<(), std::io::Error> {
            use std::os::unix::fs::PermissionsExt;

            use super::RelayLaunchError;

            let dir = std::env::temp_dir().join(format!(
                "fluxdown_nmh_launch_{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir)?;
            let script = |name: &str, body: &str, mode: u32| -> Result<_, std::io::Error> {
                let path = dir.join(name);
                std::fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
                Ok(path)
            };
            let launch = |path: std::path::PathBuf| {
                super::launch_and_ping(tokio::process::Command::new(path))
            };

            let not_executable = launch(script("plain.sh", "exit 0", 0o644)?).await;
            assert!(matches!(&not_executable, Err(error) if error.permission_denied()));
            let missing = launch(dir.join("missing.sh")).await;
            assert!(matches!(missing, Err(RelayLaunchError::Spawn(_))));
            let exited = launch(script("fail.sh", "exit 126", 0o755)?).await;
            assert!(
                matches!(&exited, Err(RelayLaunchError::Exited(status)) if status.contains("126"))
            );
            let silent = launch(script("silent.sh", "exit 0", 0o755)?).await;
            assert!(matches!(silent, Err(RelayLaunchError::NoReply(_))));
            std::fs::remove_dir_all(&dir)
        }
    }

    /// 类 Unix：清单指向 shell 包装脚本，脚本再 `exec` 真实中继。
    ///
    /// macOS 上 Hardened Runtime 的浏览器只允许拉起系统签名的 `/bin/sh`；
    /// Linux 上 AppImage 挂载点每次启动都变，包装脚本给清单一个稳定路径。
    #[cfg(unix)]
    mod unix {
        use std::io;
        use std::path::{Path, PathBuf};

        use serde_json::Value;

        use super::{NmhDiagnosis, NmhTarget, RegisterIssues, RelayOwner};

        const MANIFEST_FILENAME: &str = "com.fluxdown.nmh.json";
        const NMH_WRAPPER_NAME: &str = "fluxdown_nmh.sh";

        fn home_dir() -> Option<PathBuf> {
            directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
        }

        /// 浏览器已安装的代理判定：清单目录的父目录（profile 根）存在。
        fn browser_installed(nmh_dir: &Path) -> bool {
            nmh_dir.parent().is_some_and(Path::is_dir)
        }

        #[cfg(target_os = "macos")]
        fn wrapper_path() -> Option<PathBuf> {
            home_dir().map(|home| {
                home.join("Library")
                    .join("Application Support")
                    .join("fluxdown")
                    .join(NMH_WRAPPER_NAME)
            })
        }

        #[cfg(not(target_os = "macos"))]
        fn wrapper_path() -> Option<PathBuf> {
            home_dir().map(|home| {
                home.join(".local")
                    .join("share")
                    .join("fluxdown")
                    .join(NMH_WRAPPER_NAME)
            })
        }

        #[cfg(target_os = "macos")]
        fn chromium_nmh_dirs() -> Vec<PathBuf> {
            let Some(home) = home_dir() else {
                return Vec::new();
            };
            let lib = home.join("Library").join("Application Support");
            vec![
                lib.join("Google")
                    .join("Chrome")
                    .join("NativeMessagingHosts"),
                lib.join("Google")
                    .join("Chrome Beta")
                    .join("NativeMessagingHosts"),
                lib.join("Google")
                    .join("Chrome Dev")
                    .join("NativeMessagingHosts"),
                lib.join("Google")
                    .join("Chrome Canary")
                    .join("NativeMessagingHosts"),
                lib.join("Chromium").join("NativeMessagingHosts"),
                lib.join("Microsoft Edge").join("NativeMessagingHosts"),
                lib.join("Microsoft Edge Beta").join("NativeMessagingHosts"),
                lib.join("Microsoft Edge Dev").join("NativeMessagingHosts"),
                lib.join("Microsoft Edge Canary")
                    .join("NativeMessagingHosts"),
                lib.join("Arc")
                    .join("User Data")
                    .join("NativeMessagingHosts"),
                lib.join("BraveSoftware")
                    .join("Brave-Browser")
                    .join("NativeMessagingHosts"),
                lib.join("Vivaldi").join("NativeMessagingHosts"),
                // Thorium (Chromium fork; user-reported, #360)
                lib.join("Thorium").join("NativeMessagingHosts"),
                // Norton Neo (Chromium-based AI browser; user-reported, #623/#653).
                lib.join("Neo").join("NativeMessagingHosts"),
            ]
        }

        /// Firefox 清单目录及其“已安装”判定。macOS 上 Firefox 的 profile 根是
        /// `Firefox/` 而清单目录在 `Mozilla/`，两者任一存在都算已安装。
        #[cfg(target_os = "macos")]
        fn firefox_targets() -> Vec<(PathBuf, bool)> {
            let Some(home) = home_dir() else {
                return Vec::new();
            };
            let lib = home.join("Library").join("Application Support");
            let installed = lib.join("Firefox").is_dir() || lib.join("Mozilla").is_dir();
            vec![(lib.join("Mozilla").join("NativeMessagingHosts"), installed)]
        }

        #[cfg(target_os = "macos")]
        fn label_for_dir(dir: &Path) -> String {
            fn dir_name(path: Option<&Path>) -> &str {
                path.and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
            }
            let parent = dir.parent();
            let mut root = dir_name(parent);
            if root == "User Data" {
                root = dir_name(parent.and_then(Path::parent));
            }
            match root {
                "Microsoft Edge" => "Edge",
                "Microsoft Edge Beta" => "Edge Beta",
                "Microsoft Edge Dev" => "Edge Dev",
                "Microsoft Edge Canary" => "Edge Canary",
                "Brave-Browser" => "Brave",
                "Mozilla" => "Firefox",
                "Thorium" => "Thorium",
                "Neo" => "Neo",
                "" => "Unknown browser",
                other => other,
            }
            .to_owned()
        }

        #[cfg(not(target_os = "macos"))]
        fn chromium_nmh_dirs() -> Vec<PathBuf> {
            let Some(home) = home_dir() else {
                return Vec::new();
            };
            let config = home.join(".config");
            let var_app = home.join(".var").join("app");
            let snap = home.join("snap");
            vec![
                config.join("google-chrome").join("NativeMessagingHosts"),
                config
                    .join("google-chrome-beta")
                    .join("NativeMessagingHosts"),
                config
                    .join("google-chrome-unstable")
                    .join("NativeMessagingHosts"),
                config.join("chromium").join("NativeMessagingHosts"),
                config.join("microsoft-edge").join("NativeMessagingHosts"),
                config
                    .join("microsoft-edge-beta")
                    .join("NativeMessagingHosts"),
                config
                    .join("microsoft-edge-dev")
                    .join("NativeMessagingHosts"),
                config
                    .join("BraveSoftware")
                    .join("Brave-Browser")
                    .join("NativeMessagingHosts"),
                config.join("vivaldi").join("NativeMessagingHosts"),
                config.join("thorium").join("NativeMessagingHosts"),
                // Norton Neo (Chromium-based AI browser; user-reported, #623/#653).
                config.join("neo").join("NativeMessagingHosts"),
                var_app
                    .join("com.google.Chrome")
                    .join("config")
                    .join("google-chrome")
                    .join("NativeMessagingHosts"),
                var_app
                    .join("org.chromium.Chromium")
                    .join("config")
                    .join("chromium")
                    .join("NativeMessagingHosts"),
                var_app
                    .join("com.microsoft.Edge")
                    .join("config")
                    .join("microsoft-edge")
                    .join("NativeMessagingHosts"),
                var_app
                    .join("com.brave.Browser")
                    .join("config")
                    .join("BraveSoftware")
                    .join("Brave-Browser")
                    .join("NativeMessagingHosts"),
                snap.join("chromium")
                    .join("common")
                    .join(".config")
                    .join("chromium")
                    .join("NativeMessagingHosts"),
            ]
        }

        /// True when any Firefox-family browser's profile root exists on disk.
        ///
        /// `~/.mozilla/native-messaging-hosts` is a compat dir some Firefox-fork
        /// builds (Zen, LibreWolf) read NMH manifests from instead of their own
        /// profile root, so it must count as "installed" whenever any fork is
        /// present, not only Firefox itself (#360).
        #[cfg(not(target_os = "macos"))]
        fn firefox_family_present(home: &Path) -> bool {
            let var_app = home.join(".var").join("app");
            home.join(".mozilla").is_dir()
                || home.join(".zen").is_dir()
                || home.join(".librewolf").is_dir()
                || var_app
                    .join("org.mozilla.firefox")
                    .join(".mozilla")
                    .is_dir()
                || var_app
                    .join("io.gitlab.librewolf-community")
                    .join(".librewolf")
                    .is_dir()
        }

        #[cfg(not(target_os = "macos"))]
        fn firefox_targets() -> Vec<(PathBuf, bool)> {
            let Some(home) = home_dir() else {
                return Vec::new();
            };
            let var_app = home.join(".var").join("app");
            let mozilla_compat = home.join(".mozilla").join("native-messaging-hosts");
            [
                mozilla_compat.clone(),
                var_app
                    .join("org.mozilla.firefox")
                    .join(".mozilla")
                    .join("native-messaging-hosts"),
                home.join(".librewolf").join("native-messaging-hosts"),
                home.join(".zen").join("native-messaging-hosts"),
                var_app
                    .join("io.gitlab.librewolf-community")
                    .join(".librewolf")
                    .join("native-messaging-hosts"),
            ]
            .into_iter()
            .map(|dir| {
                // `.mozilla` 是多个 Firefox 分支共用的兼容目录；Zen / LibreWolf
                // 部分构建从这里而非各自 profile 根读取 NMH 清单，任一分支已装
                // 都算安装（#360）。
                let installed = if dir == mozilla_compat {
                    browser_installed(&dir) || firefox_family_present(&home)
                } else {
                    browser_installed(&dir)
                };
                (dir, installed)
            })
            .collect()
        }

        #[cfg(not(target_os = "macos"))]
        fn label_for_dir(dir: &Path) -> String {
            let flatpak = dir
                .components()
                .any(|component| component.as_os_str().to_str() == Some(".var"));
            let snap = !flatpak
                && dir
                    .components()
                    .any(|component| component.as_os_str().to_str() == Some("snap"));
            let root = dir
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let base = match root {
                "google-chrome" => "Chrome",
                "google-chrome-beta" => "Chrome Beta",
                "google-chrome-unstable" => "Chrome Dev",
                "chromium" => "Chromium",
                "microsoft-edge" => "Edge",
                "microsoft-edge-beta" => "Edge Beta",
                "microsoft-edge-dev" => "Edge Dev",
                "Brave-Browser" => "Brave",
                "vivaldi" => "Vivaldi",
                "thorium" => "Thorium",
                "neo" => "Neo",
                ".mozilla" => "Firefox",
                ".librewolf" => "LibreWolf",
                ".zen" => "Zen Browser",
                "" => "Unknown browser",
                other => other,
            };
            if flatpak {
                format!("{base} (Flatpak)")
            } else if snap {
                format!("{base} (Snap)")
            } else {
                base.to_owned()
            }
        }

        /// POSIX 单引号转义：`'` → `'\''`。
        fn shell_quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', r"'\''"))
        }

        /// 启动脚本 `exec '<relay>' "$@"` 里的中继路径（兼容 `'\''` 转义）。
        fn parse_wrapper_relay(script: &str) -> Option<PathBuf> {
            let rest = script
                .lines()
                .find_map(|line| line.trim_start().strip_prefix("exec "))?;
            let mut relay = String::new();
            let mut chars = rest.trim_start().chars();
            loop {
                match chars.next() {
                    Some('\'') => loop {
                        match chars.next()? {
                            '\'' => break,
                            other => relay.push(other),
                        }
                    },
                    Some('\\') => relay.push(chars.next()?),
                    _ => break,
                }
            }
            (!relay.is_empty()).then(|| PathBuf::from(relay))
        }

        /// 同目录写临时文件后原子替换：目标即使属于其他用户（曾以 sudo 运行），只要目录属于
        /// 当前用户就能换成当前用户的文件；权限位在替换前设置，浏览器不会读到半截内容。
        fn write_replacing(path: &Path, contents: &str, mode: u32) -> Result<(), io::Error> {
            use std::os::unix::fs::PermissionsExt;

            let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("not a file path: {}", path.display()),
                ));
            };
            let staged = dir.join(format!(
                ".{}.{}.tmp",
                name.to_string_lossy(),
                std::process::id()
            ));
            // 上次中断留下的同名临时文件（可能属于其他用户，删不掉时下面的写入会如实报错）。
            crate::permission::remove_file_quietly(&staged);
            let result = std::fs::write(&staged, contents)
                .and_then(|()| {
                    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(mode))
                })
                .and_then(|()| std::fs::rename(&staged, path));
            if result.is_err() {
                crate::permission::remove_file_quietly(&staged);
            }
            result
        }

        fn write_wrapper_script(wrapper: &Path, relay: &Path) -> Result<(), io::Error> {
            if let Some(parent) = wrapper.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let script = format!(
                "#!/bin/sh\nexec {} \"$@\"\n",
                shell_quote(&relay.to_string_lossy())
            );
            write_replacing(wrapper, &script, 0o755)
        }

        fn write_manifest(dir: &Path, json: &str) -> Result<PathBuf, io::Error> {
            std::fs::create_dir_all(dir)?;
            let path = dir.join(MANIFEST_FILENAME);
            write_replacing(&path, json, 0o644)?;
            Ok(path)
        }

        /// 已存在却不属于本进程有效用户的注册文件与目录（通常是曾以 sudo 运行 FluxDown 留下的）。
        fn foreign_owned(paths: impl IntoIterator<Item = PathBuf>) -> Vec<String> {
            let Ok(uid) = crate::permission::effective_uid() else {
                return Vec::new();
            };
            paths
                .into_iter()
                .filter(|path| crate::permission::owner_uid(path).is_some_and(|owner| owner != uid))
                .map(|path| path.display().to_string())
                .collect()
        }

        /// 启动脚本的归属与其指向的中继；不可执行的脚本浏览器拉不起来，按失效处理。
        fn diagnose_wrapper(wrapper: &Path, current: &Path) -> (RelayOwner, String) {
            use std::os::unix::fs::PermissionsExt;

            let script = match std::fs::read_to_string(wrapper) {
                Ok(script) => script,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return (RelayOwner::Missing, String::new());
                }
                Err(_) => return (RelayOwner::Broken, String::new()),
            };
            let Some(relay) = parse_wrapper_relay(&script) else {
                return (RelayOwner::Broken, String::new());
            };
            let executable = std::fs::metadata(wrapper)
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0);
            let owner = if executable {
                super::classify_relay(&relay, current)
            } else {
                RelayOwner::Broken
            };
            (owner, relay.display().to_string())
        }

        /// 只读检查单个浏览器清单：必须是指向启动脚本的合法 JSON；中继归属由启动脚本单独判定。
        fn diagnose_dir(
            dir: &Path,
            installed: bool,
            wrapper_str: &str,
            require_edge_origin: bool,
        ) -> NmhTarget {
            let manifest = dir.join(MANIFEST_FILENAME);
            let location = manifest.display().to_string();
            let issue = match std::fs::read_to_string(&manifest) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    format!("manifest file missing: {location}")
                }
                Err(error) => format!("manifest unreadable: {location}: {error}"),
                Ok(content) => match serde_json::from_str::<Value>(&content) {
                    Err(error) => format!("manifest is not valid JSON: {location}: {error}"),
                    Ok(json) if super::manifest_relay(&json) != Some(wrapper_str) => {
                        format!("manifest does not point to the launcher script: {location}")
                    }
                    Ok(json) if require_edge_origin && !super::manifest_allows_edge(&json) => {
                        format!("missing Edge origin in manifest: {location}")
                    }
                    Ok(_) => String::new(),
                },
            };
            NmhTarget {
                label: label_for_dir(dir),
                location,
                installed,
                ok: issue.is_empty(),
                issue,
            }
        }

        /// 选择用于诊断的清单目录：已有清单文件 > 已安装浏览器 > 首个候选。
        pub(crate) fn select_preferred_manifest_dir(
            dirs: &[PathBuf],
            is_installed: impl Fn(&Path) -> bool,
        ) -> Option<&PathBuf> {
            dirs.iter()
                .find(|dir| dir.join(MANIFEST_FILENAME).is_file())
                .or_else(|| dirs.iter().find(|dir| is_installed(dir)))
                .or_else(|| dirs.first())
        }

        /// 只读注册快照；从不写清单、包装脚本或目录。
        #[must_use]
        pub fn diagnose() -> NmhDiagnosis {
            let mut diagnosis = NmhDiagnosis::default();
            let chromium_dirs = chromium_nmh_dirs();
            let firefox_dirs = firefox_targets();
            if let Some(preferred) =
                select_preferred_manifest_dir(&chromium_dirs, browser_installed)
            {
                diagnosis.chromium_manifest =
                    preferred.join(MANIFEST_FILENAME).display().to_string();
            }
            let ff_paths: Vec<PathBuf> = firefox_dirs.iter().map(|(dir, _)| dir.clone()).collect();
            if let Some(preferred) = select_preferred_manifest_dir(&ff_paths, |dir| {
                firefox_dirs
                    .iter()
                    .any(|(d, installed)| d == dir && *installed)
            }) {
                diagnosis.firefox_manifest =
                    preferred.join(MANIFEST_FILENAME).display().to_string();
            }
            let nmh_exe = match super::find_nmh_exe() {
                Ok(path) => path,
                Err(error) => {
                    diagnosis.exe_error = error.to_string();
                    return diagnosis;
                }
            };
            diagnosis.exe_path = nmh_exe.display().to_string();
            let Some(wrapper) = wrapper_path() else {
                return diagnosis;
            };
            let wrapper_str = wrapper.display().to_string();
            let (owner, relay) = diagnose_wrapper(&wrapper, &nmh_exe);
            diagnosis.relay_owner = owner;
            diagnosis.registered_relay = relay;
            diagnosis.relay_location = wrapper_str.clone();
            let mut owned = vec![wrapper.clone()];
            owned.extend(wrapper.parent().map(Path::to_path_buf));
            for dir in &chromium_dirs {
                let installed = browser_installed(dir);
                if installed {
                    owned.push(dir.clone());
                    owned.push(dir.join(MANIFEST_FILENAME));
                }
                diagnosis
                    .targets
                    .push(diagnose_dir(dir, installed, &wrapper_str, true));
            }
            for (dir, installed) in &firefox_dirs {
                if *installed {
                    owned.push(dir.clone());
                    owned.push(dir.join(MANIFEST_FILENAME));
                }
                diagnosis
                    .targets
                    .push(diagnose_dir(dir, *installed, &wrapper_str, false));
            }
            diagnosis.foreign_owned = foreign_owned(owned);
            diagnosis
        }

        /// 启动脚本改指向 `relay`，并为所有已安装浏览器写出清单；未安装的浏览器不凭空创建
        /// profile 目录。写入被拒的目录记入结果（启动脚本被拒时清单仍照写，二者互不依赖）。
        pub(super) fn register_with(relay: &Path) -> Result<RegisterIssues, io::Error> {
            let Some(wrapper) = wrapper_path() else {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "cannot determine home directory for wrapper script",
                ));
            };
            let mut issues = RegisterIssues::default();
            match write_wrapper_script(&wrapper, relay) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                    tracing::warn!(wrapper = %wrapper.display(), error = %error, "NMH launcher script write denied");
                    if let Some(parent) = wrapper.parent() {
                        issues.deny(parent);
                    }
                }
                Err(error) => return Err(error),
            }
            let wrapper_str = wrapper.display().to_string();
            let chromium = super::chromium_manifest_json(&wrapper_str)?;
            let firefox = super::firefox_manifest_json(&wrapper_str)?;
            let chromium_targets = chromium_nmh_dirs()
                .into_iter()
                .filter(|dir| browser_installed(dir))
                .map(|dir| (dir, chromium.as_str()));
            let firefox_targets = firefox_targets()
                .into_iter()
                .filter(|(_, installed)| *installed)
                .map(|(dir, _)| (dir, firefox.as_str()));
            for (dir, json) in chromium_targets.chain(firefox_targets) {
                match write_manifest(&dir, json) {
                    Ok(path) => tracing::info!(path = %path.display(), "NMH manifest written"),
                    Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                        tracing::warn!(dir = %dir.display(), error = %error, "NMH manifest write denied");
                        issues.deny(&dir);
                    }
                    Err(error) => {
                        tracing::warn!(dir = %dir.display(), error = %error, "NMH manifest write failed")
                    }
                }
            }
            tracing::info!(relay = %relay.display(), wrapper = %wrapper.display(), denied = issues.denied.len(), "NMH registered");
            Ok(issues)
        }

        #[cfg(test)]
        mod tests {
            use std::path::Path;

            use super::{parse_wrapper_relay, shell_quote, write_replacing};

            #[test]
            fn wrapper_relay_round_trips_through_shell_quoting() {
                for relay in [
                    "/Applications/FluxDown.app/Contents/MacOS/fluxdown_nmh",
                    "/home/o'brien/My Apps/fluxdown_nmh",
                ] {
                    let script = format!("#!/bin/sh\nexec {} \"$@\"\n", shell_quote(relay));
                    assert_eq!(
                        parse_wrapper_relay(&script).as_deref(),
                        Some(Path::new(relay))
                    );
                }
            }

            #[test]
            fn wrapper_without_exec_line_has_no_relay() {
                assert_eq!(parse_wrapper_relay("#!/bin/sh\necho hi\n"), None);
                assert_eq!(
                    parse_wrapper_relay("#!/bin/sh\nexec '/unterminated\n"),
                    None
                );
            }

            /// 旧文件不可写（模拟曾以 sudo 写出的文件）但目录可写时，原子替换仍成功且权限位正确；
            /// 目录不可写时报 `PermissionDenied`，不留临时文件。
            #[test]
            fn replacing_write_survives_unwritable_files_but_not_unwritable_dirs()
            -> Result<(), std::io::Error> {
                use std::os::unix::fs::PermissionsExt;

                let dir = std::env::temp_dir().join(format!(
                    "fluxdown_nmh_replace_{}",
                    uuid::Uuid::new_v4().simple()
                ));
                std::fs::create_dir_all(&dir)?;
                let file = dir.join("fluxdown_nmh.sh");
                std::fs::write(&file, "old")?;
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444))?;
                write_replacing(&file, "new", 0o755)?;
                assert_eq!(std::fs::read_to_string(&file)?, "new");
                assert_eq!(
                    std::fs::metadata(&file)?.permissions().mode() & 0o777,
                    0o755
                );

                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555))?;
                let denied = write_replacing(&file, "newer", 0o755);
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
                // root 无视权限位：此时写入照常成功，无从验证拒绝路径。
                if let Err(error) = denied {
                    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                    assert_eq!(std::fs::read_to_string(&file)?, "new");
                }
                assert_eq!(std::fs::read_dir(&dir)?.count(), 1);
                std::fs::remove_dir_all(&dir)
            }

            #[test]
            fn chromium_nmh_dirs_and_labels_cover_dev_channels() {
                #[cfg(target_os = "macos")]
                {
                    use super::{chromium_nmh_dirs, label_for_dir};
                    let dirs = chromium_nmh_dirs();
                    assert!(
                        dirs.iter()
                            .any(|d| d.to_string_lossy().contains("Chrome Dev")),
                        "missing Chrome Dev in chromium_nmh_dirs: {dirs:?}"
                    );
                    assert!(
                        dirs.iter()
                            .any(|d| d.to_string_lossy().contains("Microsoft Edge Dev")),
                        "missing Microsoft Edge Dev in chromium_nmh_dirs: {dirs:?}"
                    );
                    assert!(
                        dirs.iter()
                            .any(|d| d.to_string_lossy().contains("Microsoft Edge Canary")),
                        "missing Microsoft Edge Canary in chromium_nmh_dirs: {dirs:?}"
                    );

                    assert_eq!(
                        label_for_dir(Path::new(
                            "/Users/u/Library/Application Support/Google/Chrome Dev/NativeMessagingHosts"
                        )),
                        "Chrome Dev"
                    );
                    assert_eq!(
                        label_for_dir(Path::new(
                            "/Users/u/Library/Application Support/Microsoft Edge Dev/NativeMessagingHosts"
                        )),
                        "Edge Dev"
                    );
                    assert_eq!(
                        label_for_dir(Path::new(
                            "/Users/u/Library/Application Support/Microsoft Edge Canary/NativeMessagingHosts"
                        )),
                        "Edge Canary"
                    );
                }

                #[cfg(not(target_os = "macos"))]
                {
                    use super::{chromium_nmh_dirs, label_for_dir};
                    let dirs = chromium_nmh_dirs();
                    assert!(
                        dirs.iter()
                            .any(|d| d.to_string_lossy().contains("google-chrome-unstable")),
                        "missing google-chrome-unstable in chromium_nmh_dirs: {dirs:?}"
                    );
                    assert!(
                        dirs.iter()
                            .any(|d| d.to_string_lossy().contains("google-chrome-beta")),
                        "missing google-chrome-beta in chromium_nmh_dirs: {dirs:?}"
                    );
                    assert!(
                        dirs.iter()
                            .any(|d| d.to_string_lossy().contains("microsoft-edge-dev")),
                        "missing microsoft-edge-dev in chromium_nmh_dirs: {dirs:?}"
                    );
                    assert!(
                        dirs.iter()
                            .any(|d| d.to_string_lossy().contains("microsoft-edge-beta")),
                        "missing microsoft-edge-beta in chromium_nmh_dirs: {dirs:?}"
                    );
                    assert_eq!(
                        label_for_dir(Path::new(
                            "/home/u/.config/google-chrome-unstable/NativeMessagingHosts"
                        )),
                        "Chrome Dev"
                    );
                    assert_eq!(
                        label_for_dir(Path::new(
                            "/home/u/.config/google-chrome-beta/NativeMessagingHosts"
                        )),
                        "Chrome Beta"
                    );
                    assert_eq!(
                        label_for_dir(Path::new(
                            "/home/u/.config/microsoft-edge-dev/NativeMessagingHosts"
                        )),
                        "Edge Dev"
                    );
                    assert_eq!(
                        label_for_dir(Path::new(
                            "/home/u/.config/microsoft-edge-beta/NativeMessagingHosts"
                        )),
                        "Edge Beta"
                    );
                }
            }

            #[test]
            fn select_preferred_manifest_dir_prioritizes_existing_then_installed_then_first() {
                use super::{MANIFEST_FILENAME, select_preferred_manifest_dir};
                let root = std::env::temp_dir().join(format!(
                    "fluxdown_nmh_select_pref_{}",
                    uuid::Uuid::new_v4().simple()
                ));
                let dir_a = root.join("browser_a");
                let dir_b = root.join("browser_b");
                let dir_c = root.join("browser_c");
                let dirs = vec![dir_a.clone(), dir_b.clone(), dir_c.clone()];

                // 1. 没有任何清单或安装：退回首个目录
                let chosen = select_preferred_manifest_dir(&dirs, |_| false);
                assert_eq!(chosen, Some(&dir_a));

                // 2. 无清单，但 dir_b 标记为已安装：优先已安装
                let chosen = select_preferred_manifest_dir(&dirs, |p| p == dir_b);
                assert_eq!(chosen, Some(&dir_b));

                // 3. dir_c 磁盘上存在实际清单文件：优先已有清单（即使 dir_b 也是已安装）
                std::fs::create_dir_all(&dir_c).unwrap();
                std::fs::write(dir_c.join(MANIFEST_FILENAME), "{}").unwrap();
                let chosen = select_preferred_manifest_dir(&dirs, |p| p == dir_b);
                assert_eq!(chosen, Some(&dir_c));

                drop(std::fs::remove_dir_all(&root));
            }
        }
    }

    /// Windows：HKCU 注册表键指向 `<data_dir>\nmh\` 下的两份清单（Chromium / Firefox 各一份）。
    #[cfg(windows)]
    mod windows {
        use std::io;
        use std::path::{Path, PathBuf};

        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};

        use serde_json::Value;

        use super::{NMH_NAME, NmhDiagnosis, NmhTarget, PolicyBlock, RegisterIssues, RelayOwner};

        /// Chrome / Edge 的策略根（HKLM 优先于 HKCU）。
        const BROWSER_POLICY_PATHS: [(&str, &str); 2] = [
            (r"SOFTWARE\Policies\Google\Chrome", "Chrome"),
            (r"SOFTWARE\Policies\Microsoft\Edge", "Edge"),
        ];
        /// 「阻止访问命令提示符」组策略（`DisableCMD`）。
        const SYSTEM_POLICY_PATH: &str = r"SOFTWARE\Policies\Microsoft\Windows\System";

        /// 首个存在该 DWORD 值的根（HKLM 优先，与浏览器策略优先级一致）。
        fn read_dword_policy(path: &str, name: &str) -> Option<u32> {
            [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
                .into_iter()
                .find_map(|root| {
                    RegKey::predef(root)
                        .open_subkey_with_flags(path, KEY_READ)
                        .and_then(|key| key.get_value::<u32, _>(name))
                        .ok()
                })
        }

        /// 列表策略存成 `<path>\<name>` 子键下的编号字符串值；HKLM 的列表存在时覆盖 HKCU。
        fn read_list_policy(path: &str, name: &str) -> Vec<String> {
            [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
                .into_iter()
                .find_map(|root| {
                    RegKey::predef(root)
                        .open_subkey_with_flags(format!("{path}\\{name}"), KEY_READ)
                        .ok()
                })
                .map(|key| {
                    key.enum_values()
                        .filter_map(Result::ok)
                        .filter_map(|(value_name, _)| key.get_value::<String, _>(&value_name).ok())
                        .collect()
                })
                .unwrap_or_default()
        }

        /// `(策略阻断, Chrome 与 Edge 是否都直接启动主机)`。
        fn read_policies() -> (Vec<PolicyBlock>, bool) {
            let mut blocks = Vec::new();
            let mut launches_directly = Vec::with_capacity(BROWSER_POLICY_PATHS.len());
            for (path, browser) in BROWSER_POLICY_PATHS {
                blocks.extend(super::evaluate_browser_policy(
                    browser,
                    read_dword_policy(path, "NativeMessagingUserLevelHosts"),
                    &read_list_policy(path, "NativeMessagingBlocklist"),
                    &read_list_policy(path, "NativeMessagingAllowlist"),
                ));
                launches_directly.push(
                    read_dword_policy(path, "NativeHostsExecutablesLaunchDirectly") == Some(1),
                );
            }
            if super::cmd_blocks_launch(
                read_dword_policy(SYSTEM_POLICY_PATH, "DisableCMD"),
                &launches_directly,
            ) {
                blocks.push(PolicyBlock::CommandPromptDisabled);
            }
            (blocks, launches_directly.iter().all(|direct| *direct))
        }

        /// 写清单；旧文件带只读属性时先以普通权限去掉（只读属性不是 ACL，授权解决不了）。
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "Windows only: clears the read-only attribute, it does not widen any ACL"
        )]
        fn write_manifest_file(path: &Path, contents: &str) -> Result<(), io::Error> {
            if let Ok(metadata) = std::fs::metadata(path) {
                let mut permissions = metadata.permissions();
                if permissions.readonly() {
                    permissions.set_readonly(false);
                    // 去不掉时随后的写入会如实报错，这里只记日志。
                    if let Err(error) = std::fs::set_permissions(path, permissions) {
                        tracing::warn!(path = %path.display(), error = %error, "could not clear the read-only attribute of an NMH manifest");
                    }
                }
            }
            std::fs::write(path, contents)
        }

        const MANIFEST_FILENAME_CHROMIUM: &str = "com.fluxdown.nmh.json";
        const MANIFEST_FILENAME_FIREFOX: &str = "com.fluxdown.nmh.firefox.json";
        /// Brave/Vivaldi/Opera 等分支在自身键缺失时回退读 Chrome 键，只需 Chrome 与 Edge。
        const CHROMIUM_REG_PATHS: [(&str, &str); 2] = [
            (r"Software\Google\Chrome\NativeMessagingHosts", "Chrome"),
            (r"Software\Microsoft\Edge\NativeMessagingHosts", "Edge"),
        ];
        const FIREFOX_REG_PATH: &str = r"Software\Mozilla\NativeMessagingHosts";
        /// `(注册表路径, 展示名, 是否 Chromium 清单)`；首个能解析出中继的键决定生效注册。
        const REG_TARGETS: [(&str, &str, bool); 3] = [
            (CHROMIUM_REG_PATHS[0].0, CHROMIUM_REG_PATHS[0].1, true),
            (CHROMIUM_REG_PATHS[1].0, CHROMIUM_REG_PATHS[1].1, true),
            (FIREFOX_REG_PATH, "Firefox", false),
        ];

        /// 一个注册表键的读取结果。
        enum Registration {
            KeyMissing(String),
            Invalid(String),
            Valid { manifest: String, json: Value },
        }

        fn strip_unc_prefix(path: &str) -> String {
            path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
        }

        fn env_dir_exists(var: &str, rest: &[&str]) -> bool {
            let Ok(base) = std::env::var(var) else {
                return false;
            };
            let mut path = PathBuf::from(base);
            for segment in rest {
                path.push(segment);
            }
            path.is_dir()
        }

        /// 每用户可写的清单目录：`<engine data dir>\nmh`（安装版 `%LOCALAPPDATA%\FluxDown\nmh`，
        /// 便携版 `<exe_dir>\portable_data\nmh`）。不能写在中继旁边：全局安装落在
        /// `Program Files`，未提权进程写入直接 access denied，注册表键永远写不出来。
        fn manifest_dir() -> PathBuf {
            crate::runtime::engine_data_dir().join("nmh")
        }

        /// 期望的（去 UNC 前缀）清单路径 `(chromium, firefox)`。
        fn expected_manifest_paths() -> (String, String) {
            let dir = manifest_dir();
            (
                strip_unc_prefix(&dir.join(MANIFEST_FILENAME_CHROMIUM).to_string_lossy()),
                strip_unc_prefix(&dir.join(MANIFEST_FILENAME_FIREFOX).to_string_lossy()),
            )
        }

        fn read_registration(hkcu: &RegKey, reg_path: &str) -> Registration {
            let full_path = format!("{reg_path}\\{NMH_NAME}");
            let Ok(key) = hkcu.open_subkey_with_flags(&full_path, KEY_READ) else {
                return Registration::KeyMissing(format!(
                    "registry key missing: HKCU\\{full_path}"
                ));
            };
            let Ok(manifest) = key.get_value::<String, _>("") else {
                return Registration::Invalid(format!(
                    "registry default value unreadable: HKCU\\{full_path}"
                ));
            };
            let content = match std::fs::read_to_string(&manifest) {
                Ok(content) => content,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Registration::Invalid(format!("manifest file missing: {manifest}"));
                }
                Err(error) => {
                    return Registration::Invalid(format!(
                        "manifest unreadable: {manifest}: {error}"
                    ));
                }
            };
            match serde_json::from_str(&content) {
                Ok(json) => Registration::Valid { manifest, json },
                Err(error) => Registration::Invalid(format!(
                    "manifest is not valid JSON: {manifest}: {error}"
                )),
            }
        }

        /// 单个键相对生效注册的问题；完好时为空串。`owner_is_current` 时还要求清单落在本安装
        /// 的数据目录（迁移旧版写在 exe 旁的清单）。
        fn target_issue(
            registration: &Registration,
            registered: &Path,
            owner_is_current: bool,
            expected_manifest: &str,
            require_edge_origin: bool,
        ) -> String {
            let (manifest, json) = match registration {
                Registration::KeyMissing(issue) | Registration::Invalid(issue) => {
                    return issue.clone();
                }
                Registration::Valid { manifest, json } => (manifest, json),
            };
            let Some(relay) = super::manifest_relay(json) else {
                return format!("manifest has no relay path: {manifest}");
            };
            if !super::same_relay(Path::new(&strip_unc_prefix(relay)), registered) {
                return format!(
                    "manifest points to a different relay than the active registration: {manifest}"
                );
            }
            if owner_is_current && !manifest.eq_ignore_ascii_case(expected_manifest) {
                return format!("registry points to unexpected manifest: {manifest}");
            }
            if require_edge_origin && !super::manifest_allows_edge(json) {
                return format!("missing Edge origin in manifest: {manifest}");
            }
            String::new()
        }

        /// 只读注册快照；从不写注册表、清单或目录。
        #[must_use]
        pub fn diagnose() -> NmhDiagnosis {
            let mut diagnosis = NmhDiagnosis::default();
            let nmh_exe = match super::find_nmh_exe() {
                Ok(path) => path,
                Err(error) => {
                    diagnosis.exe_error = error.to_string();
                    return diagnosis;
                }
            };
            diagnosis.exe_path = strip_unc_prefix(&nmh_exe.to_string_lossy());
            let (chromium_manifest, firefox_manifest) = expected_manifest_paths();
            diagnosis.chromium_manifest = chromium_manifest;
            diagnosis.firefox_manifest = firefox_manifest;
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let registrations =
                REG_TARGETS.map(|(reg_path, _, _)| read_registration(&hkcu, reg_path));
            let active = REG_TARGETS.iter().zip(&registrations).find_map(
                |((reg_path, _, _), registration)| match registration {
                    Registration::Valid { json, .. } => super::manifest_relay(json)
                        .map(|relay| (*reg_path, strip_unc_prefix(relay))),
                    _ => None,
                },
            );
            match active {
                Some((reg_path, relay)) => {
                    diagnosis.relay_location = format!("HKCU\\{reg_path}\\{NMH_NAME}");
                    diagnosis.relay_owner =
                        super::classify_relay(Path::new(&relay), Path::new(&diagnosis.exe_path));
                    diagnosis.registered_relay = relay;
                }
                None => {
                    diagnosis.relay_location =
                        format!("HKCU\\{}\\{NMH_NAME}", CHROMIUM_REG_PATHS[0].0);
                    diagnosis.relay_owner = if registrations
                        .iter()
                        .all(|registration| matches!(registration, Registration::KeyMissing(_)))
                    {
                        RelayOwner::Missing
                    } else {
                        RelayOwner::Broken
                    };
                }
            }
            let registered = PathBuf::from(&diagnosis.registered_relay);
            let owner_is_current = diagnosis.relay_owner == RelayOwner::Current;
            let installed = [
                env_dir_exists("LOCALAPPDATA", &["Google", "Chrome", "User Data"]),
                env_dir_exists("LOCALAPPDATA", &["Microsoft", "Edge", "User Data"]),
                env_dir_exists("APPDATA", &["Mozilla", "Firefox"]),
            ];
            for (((reg_path, label, chromium), registration), installed) in
                REG_TARGETS.iter().zip(&registrations).zip(installed)
            {
                let expected_manifest = if *chromium {
                    &diagnosis.chromium_manifest
                } else {
                    &diagnosis.firefox_manifest
                };
                let issue = target_issue(
                    registration,
                    &registered,
                    owner_is_current,
                    expected_manifest,
                    *chromium,
                );
                diagnosis.targets.push(NmhTarget {
                    label: (*label).to_owned(),
                    location: format!("HKCU\\{reg_path}\\{NMH_NAME}"),
                    installed,
                    ok: issue.is_empty(),
                    issue,
                });
            }
            (diagnosis.policy_blocks, diagnosis.direct_launch) = read_policies();
            // 只要还有浏览器经 cmd.exe 启动主机，ComSpec 就决定扩展能否连上。
            if !diagnosis.direct_launch {
                diagnosis.comspec = Some(crate::nmh::comspec::inspect());
            }
            diagnosis
        }

        /// 写出指向 `relay` 的两份清单并注册 Chrome / Edge / Firefox 键；幂等。清单目录写入被拒
        /// 时记入结果（注册表键照写，指向的清单路径不变）；注册表写入失败直接报错。
        pub(super) fn register_with(relay: &Path) -> Result<RegisterIssues, io::Error> {
            let nmh_path = strip_unc_prefix(&relay.to_string_lossy());
            let dir = manifest_dir();
            let chromium_path = dir.join(MANIFEST_FILENAME_CHROMIUM);
            let firefox_path = dir.join(MANIFEST_FILENAME_FIREFOX);
            let chromium_json = super::chromium_manifest_json(&nmh_path)?;
            let firefox_json = super::firefox_manifest_json(&nmh_path)?;
            let mut issues = RegisterIssues::default();
            let written = std::fs::create_dir_all(&dir)
                .and_then(|()| write_manifest_file(&chromium_path, &chromium_json))
                .and_then(|()| write_manifest_file(&firefox_path, &firefox_json));
            match written {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                    tracing::warn!(dir = %dir.display(), error = %error, "NMH manifest write denied");
                    issues.deny(&dir);
                }
                Err(error) => return Err(error),
            }
            let chromium_str = strip_unc_prefix(&chromium_path.to_string_lossy());
            let firefox_str = strip_unc_prefix(&firefox_path.to_string_lossy());
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            for (reg_path, _) in CHROMIUM_REG_PATHS {
                let (key, _) =
                    hkcu.create_subkey_with_flags(format!("{reg_path}\\{NMH_NAME}"), KEY_WRITE)?;
                key.set_value("", &chromium_str)?;
            }
            let (key, _) = hkcu
                .create_subkey_with_flags(format!("{FIREFOX_REG_PATH}\\{NMH_NAME}"), KEY_WRITE)?;
            key.set_value("", &firefox_str)?;
            tracing::info!(relay = %nmh_path, chromium = %chromium_str, firefox = %firefox_str, denied = issues.denied.len(), "NMH registered");
            Ok(issues)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::{
        MAX_BATCH_ITEMS, NmhService, PipeMessage, SNIFF_REPLY_BUDGET, handle_stream, ping_stream,
        pipe_name_for, select_task_briefs, socket_path_under,
    };

    /// 与 `native/nmh/src/main.rs` 的 `unix_socket_lives_in_a_private_per_user_ipc_dir`
    /// 使用同一组字面量：agent 与中继各自推导端点，必须算出同一路径。
    #[test]
    fn unix_socket_lives_in_a_private_per_user_ipc_dir() {
        let (home, expected) = if cfg!(target_os = "macos") {
            (
                "/Users/alice",
                "/Users/alice/Library/Application Support/fluxdown/ipc/fluxdown.sock",
            )
        } else {
            (
                "/home/alice",
                "/home/alice/.local/share/fluxdown/ipc/fluxdown.sock",
            )
        };
        assert_eq!(
            socket_path_under(std::path::Path::new(home)),
            std::path::PathBuf::from(expected)
        );
    }

    /// 与 `native/nmh/src/main.rs` 的 `pipe_name_is_per_user_and_injective` 使用同一组字面量。
    #[test]
    fn pipe_name_is_per_user_and_injective() {
        assert_eq!(
            pipe_name_for("Alice Smith").as_deref(),
            Some(r"\\.\pipe\fluxdown-alice_20smith")
        );
        assert_eq!(
            pipe_name_for("a_b"),
            Some(r"\\.\pipe\fluxdown-a_5fb".to_owned())
        );
        assert_eq!(
            pipe_name_for("a b"),
            Some(r"\\.\pipe\fluxdown-a_20b".to_owned())
        );
        assert_eq!(
            pipe_name_for("张三").as_deref(),
            Some(r"\\.\pipe\fluxdown-_e5_bc_a0_e4_b8_89")
        );
        assert_eq!(pipe_name_for(""), None);
        assert_ne!(pipe_name_for("alice"), pipe_name_for("bob"));
        // Windows 账户名不区分大小写。
        assert_eq!(pipe_name_for("ALICE"), pipe_name_for("alice"));
    }
    #[test]
    fn task_panel_reports_live_download_speed() {
        let task = |id: &str| -> fluxdown_protocol::TaskDto {
            serde_json::from_value(json!({
                "taskId": id, "url": "https://example.com/a", "fileName": "a",
                "saveDir": "/tmp", "status": 1, "downloadedBytes": 1, "totalBytes": 2,
                "errorMessage": "", "createdAt": "1", "proxyUrl": "", "queueId": "main",
                "checksum": ""
            }))
            .expect("task")
        };
        let mut speeds = std::collections::HashMap::new();
        speeds.insert(
            "fast".to_owned(),
            fluxdown_api::service::LiveSpeed {
                download_bps: 4096,
                upload_bps: 1,
            },
        );
        let selected = select_task_briefs(vec![task("fast"), task("idle")], &speeds);
        assert_eq!(selected[0].speed, 4096);
        assert_eq!(selected[1].speed, 0);
    }

    #[tokio::test]
    async fn ipc_ping_round_trips_through_frame_protocol() {
        let daemon = Arc::new(crate::daemon_client::DaemonClient::disconnected());
        let events =
            crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default());
        let shell = crate::shell::ShellState::new(
            crate::shell::TrayAvailability::Unavailable(
                fluxdown_protocol::TrayUnavailableReason::NotBuilt,
            ),
            daemon.clone(),
            events.clone(),
        );
        let capture = Arc::new(crate::capture::CaptureService::new(
            daemon.clone(),
            events,
            shell,
        ));
        let service = NmhService::new(daemon, capture);
        let (client, server) = tokio::io::duplex(4096);
        let server_task = tokio::spawn(handle_stream(server, service));
        let reply = ping_stream(client).await.expect("pong");
        assert_eq!(reply, "pong");
        server_task
            .await
            .expect("join NMH stream handler")
            .expect("clean client EOF closes NMH stream");
    }

    #[test]
    fn task_panel_keeps_active_and_ten_recent_completions() {
        let tasks = (0..15)
            .map(|index| {
                serde_json::from_value(json!({
                    "taskId": format!("task-{index}"),
                    "url": "https://example.com/a",
                    "fileName": format!("{index}.bin"),
                    "saveDir": "/tmp",
                    "status": if index == 0 { 1 } else { 3 },
                    "downloadedBytes": 1,
                    "totalBytes": 1,
                    "errorMessage": "",
                    "createdAt": index.to_string(),
                    "proxyUrl": "",
                    "queueId": "main",
                    "checksum": ""
                }))
                .expect("task")
            })
            .collect();
        let selected = select_task_briefs(tasks, &std::collections::HashMap::new());
        assert_eq!(selected.len(), 11);
        assert_eq!(selected[0].task_id, "task-0");
        assert_eq!(selected[1].task_id, "task-14");
    }

    #[test]
    fn flattened_native_message_preserves_download_payload() {
        let message = serde_json::from_value::<PipeMessage>(json!({
            "action": "download",
            "msg_id": 7,
            "url": "https://example.com/a"
        }))
        .expect("native message");
        assert_eq!(message.msg_id, 7);
        assert_eq!(message.payload["url"], "https://example.com/a");
        assert_eq!(MAX_BATCH_ITEMS, 1000);
    }

    fn service_with(
        daemon: Arc<crate::daemon_client::DaemonClient>,
    ) -> (NmhService, Arc<crate::capture::CaptureService>) {
        let events =
            crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default());
        let shell = crate::shell::ShellState::new(
            crate::shell::TrayAvailability::Unavailable(
                fluxdown_protocol::TrayUnavailableReason::NotBuilt,
            ),
            daemon.clone(),
            events.clone(),
        );
        let capture = Arc::new(crate::capture::CaptureService::new(
            daemon.clone(),
            events,
            shell,
        ));
        (NmhService::new(daemon, capture.clone()), capture)
    }

    /// 经长度帧协议发一问一答。
    async fn exchange(
        stream: &mut tokio::io::DuplexStream,
        request: serde_json::Value,
    ) -> serde_json::Value {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let bytes = serde_json::to_vec(&request).expect("request json");
        let length = u32::try_from(bytes.len()).expect("frame length");
        stream
            .write_all(&length.to_le_bytes())
            .await
            .expect("write length");
        stream.write_all(&bytes).await.expect("write body");
        let mut length = [0_u8; 4];
        stream.read_exact(&mut length).await.expect("read length");
        let mut body = vec![0_u8; u32::from_le_bytes(length) as usize];
        stream.read_exact(&mut body).await.expect("read body");
        serde_json::from_slice(&body).expect("response json")
    }

    #[tokio::test]
    async fn download_reply_carries_transaction_ids_and_capture_status_follows_resolution() {
        let daemon = Arc::new(crate::daemon_client::DaemonClient::disconnected());
        let (service, capture) = service_with(daemon);
        let (mut client, server) = tokio::io::duplex(65536);
        let server_task = tokio::spawn(handle_stream(server, service));

        let reply = exchange(
            &mut client,
            json!({"action": "download", "msg_id": 2, "url": "https://example.com/a.zip"}),
        )
        .await;
        assert_eq!(reply["success"], true);
        assert_eq!(reply["message"], "download accepted");
        let ids = reply["data"]["transactionIds"].as_array().expect("ids");
        assert_eq!(ids.len(), 1);
        let id = ids[0].as_str().expect("id").to_owned();

        let status = exchange(
            &mut client,
            json!({"action": "capture_status", "msg_id": 3, "transactionIds": [id, "missing"]}),
        )
        .await;
        assert_eq!(status["data"]["statuses"][&id], "pending");
        assert_eq!(status["data"]["statuses"]["missing"], "unknown");

        capture
            .resolve(&id, false, false, None)
            .await
            .expect("reject");
        let status = exchange(
            &mut client,
            json!({"action": "capture_status", "msg_id": 4, "transactionIds": [id]}),
        )
        .await;
        assert_eq!(status["data"]["statuses"][&id], "rejected");

        let invalid = exchange(
            &mut client,
            json!({"action": "capture_status", "msg_id": 5, "transactionIds": []}),
        )
        .await;
        assert_eq!(invalid["success"], false);
        assert_eq!(invalid["message"], "invalid transaction count: 0");

        drop(client);
        server_task
            .await
            .expect("join")
            .expect("clean client EOF closes NMH stream");
    }

    #[tokio::test]
    async fn capabilities_reports_protocol_and_plugin_support() {
        use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, RpcErrorObject};

        let supported = Arc::new(crate::daemon_client::DaemonClient::answering(|_| {
            Ok(json!([]))
        }));
        let (service, _) = service_with(supported);
        let (mut client, server) = tokio::io::duplex(65536);
        tokio::spawn(handle_stream(server, service));
        let reply = exchange(&mut client, json!({"action": "capabilities", "msg_id": 1})).await;
        assert_eq!(reply["data"]["protocol"], 2);
        assert_eq!(reply["data"]["pluginsSupported"], true);
        assert_eq!(reply["data"]["resolvers"], json!([]));
        assert!(
            reply["data"]["actions"]
                .as_array()
                .expect("actions")
                .contains(&json!("sniff_rules"))
        );

        let unsupported = Arc::new(crate::daemon_client::DaemonClient::answering(|_| {
            Err(RpcErrorObject::application(
                "daemon was built without plugin support",
                RpcErrorData::new(ApplicationErrorCode::Unsupported, false),
            ))
        }));
        let (service, _) = service_with(unsupported);
        let (mut client, server) = tokio::io::duplex(65536);
        tokio::spawn(handle_stream(server, service));
        let reply = exchange(&mut client, json!({"action": "capabilities", "msg_id": 1})).await;
        assert_eq!(reply["success"], true);
        assert_eq!(reply["data"]["pluginsSupported"], false);
        assert_eq!(reply["data"]["resolvers"], json!([]));
    }

    #[tokio::test]
    async fn capabilities_reports_plugin_rules_sequence_and_epoch() {
        let daemon = Arc::new(crate::daemon_client::DaemonClient::answering(|method| {
            if method == fluxdown_protocol::method::DAEMON_PLUGIN_SNIFF_RULES {
                Ok(json!({"sequence": 7, "epoch": "e1", "pack": {"rules": []}}))
            } else {
                Ok(json!([]))
            }
        }));
        let (service, _) = service_with(daemon);
        let (mut client, server) = tokio::io::duplex(65536);
        tokio::spawn(handle_stream(server, service));
        let reply = exchange(&mut client, json!({"action": "capabilities", "msg_id": 1})).await;
        assert_eq!(reply["data"]["pluginRulesSequence"], 7);
        assert_eq!(reply["data"]["pluginRulesEpoch"], "e1");
    }

    #[tokio::test]
    async fn capabilities_reports_page_scripts_sequence_and_epoch() {
        use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, RpcErrorObject};

        let daemon = Arc::new(crate::daemon_client::DaemonClient::answering(|method| {
            if method == fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_SCRIPTS {
                Ok(json!({"sequence": 5, "epoch": "p1", "scripts": []}))
            } else {
                Ok(json!([]))
            }
        }));
        let (service, _) = service_with(daemon);
        let (mut client, server) = tokio::io::duplex(65536);
        tokio::spawn(handle_stream(server, service));
        let reply = exchange(&mut client, json!({"action": "capabilities", "msg_id": 1})).await;
        assert_eq!(reply["data"]["pageScriptsSequence"], 5);
        assert_eq!(reply["data"]["pageScriptsEpoch"], "p1");
        let actions = reply["data"]["actions"].as_array().expect("actions");
        for action in ["page_scripts", "page_script_source", "plugin_invoke"] {
            assert!(actions.contains(&json!(action)), "{action}");
        }

        // daemon 不支持该方法：两字段均为 null，capabilities 仍成功。
        let unsupported = Arc::new(crate::daemon_client::DaemonClient::answering(|method| {
            if method == fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_SCRIPTS {
                Err(RpcErrorObject::application(
                    "unsupported",
                    RpcErrorData::new(ApplicationErrorCode::Unsupported, false),
                ))
            } else {
                Ok(json!([]))
            }
        }));
        let (service, _) = service_with(unsupported);
        let (mut client, server) = tokio::io::duplex(65536);
        tokio::spawn(handle_stream(server, service));
        let reply = exchange(&mut client, json!({"action": "capabilities", "msg_id": 1})).await;
        assert_eq!(reply["success"], true);
        assert!(reply["data"]["pageScriptsSequence"].is_null());
        assert!(reply["data"]["pageScriptsEpoch"].is_null());

        // epoch 为空：视为不可用。
        let empty = Arc::new(crate::daemon_client::DaemonClient::answering(|method| {
            if method == fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_SCRIPTS {
                Ok(json!({"sequence": 0, "epoch": "", "scripts": []}))
            } else {
                Ok(json!([]))
            }
        }));
        let (service, _) = service_with(empty);
        let (mut client, server) = tokio::io::duplex(65536);
        tokio::spawn(handle_stream(server, service));
        let reply = exchange(&mut client, json!({"action": "capabilities", "msg_id": 1})).await;
        assert!(reply["data"]["pageScriptsSequence"].is_null());
        assert!(reply["data"]["pageScriptsEpoch"].is_null());
    }

    #[tokio::test]
    async fn page_script_actions_forward_daemon_data() {
        let index = json!({"sequence": 3, "epoch": "e", "scripts": [{"id": "a"}]});
        let expected = index.clone();
        let daemon = Arc::new(crate::daemon_client::DaemonClient::answering(
            move |method| match method {
                fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_SCRIPTS => Ok(expected.clone()),
                fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_SCRIPT_SOURCE => {
                    Ok(json!({"identity": "a@b", "id": "a", "sha256": "h", "code": "x"}))
                }
                fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_INVOKE => Ok(json!({
                    "value": {"n": 1}, "createdTaskIds": [], "resumedTaskIds": [],
                })),
                _ => Ok(json!([])),
            },
        ));
        let (service, _) = service_with(daemon);
        let (mut client, server) = tokio::io::duplex(65536);
        tokio::spawn(handle_stream(server, service));

        let reply = exchange(&mut client, json!({"action": "page_scripts", "msg_id": 1})).await;
        assert_eq!(reply["success"], true);
        assert_eq!(reply["data"], index);

        let reply = exchange(
            &mut client,
            json!({"action": "page_script_source", "msg_id": 2, "identity": "a@b", "id": "a"}),
        )
        .await;
        assert_eq!(reply["success"], true);
        assert_eq!(reply["data"]["code"], "x");

        let reply = exchange(
            &mut client,
            json!({"action": "page_script_source", "msg_id": 3, "identity": "a@b"}),
        )
        .await;
        assert_eq!(reply["success"], false);

        let reply = exchange(
            &mut client,
            json!({
                "action": "plugin_invoke", "msg_id": 4, "identity": "a@b",
                "name": "run", "args": {"q": 1}, "pageUrl": "https://example.com/",
            }),
        )
        .await;
        assert_eq!(reply["success"], true);
        assert_eq!(reply["data"], json!({"value": {"n": 1}}));

        let reply = exchange(
            &mut client,
            json!({"action": "plugin_invoke", "msg_id": 5, "identity": "a@b", "name": "run"}),
        )
        .await;
        assert_eq!(reply["success"], false);
    }

    #[tokio::test]
    async fn page_script_source_over_budget_and_daemon_errors_fail() {
        use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, RpcErrorObject};

        let daemon = Arc::new(crate::daemon_client::DaemonClient::answering(
            |method| match method {
                fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_SCRIPT_SOURCE => Ok(json!({
                    "identity": "a@b", "id": "a", "sha256": "h",
                    "code": "x".repeat(SNIFF_REPLY_BUDGET + 1),
                })),
                fluxdown_protocol::method::DAEMON_PLUGIN_PAGE_INVOKE => {
                    Err(RpcErrorObject::application(
                        "rpc not exposed to page: run",
                        RpcErrorData::new(ApplicationErrorCode::Unsupported, false),
                    ))
                }
                _ => Ok(json!([])),
            },
        ));
        let (service, _) = service_with(daemon);
        let (mut client, server) = tokio::io::duplex(65536);
        tokio::spawn(handle_stream(server, service));

        let reply = exchange(
            &mut client,
            json!({"action": "page_script_source", "msg_id": 1, "identity": "a@b", "id": "a"}),
        )
        .await;
        assert_eq!(reply["success"], false);
        assert_eq!(reply["message"], "page script too large");

        let reply = exchange(
            &mut client,
            json!({
                "action": "plugin_invoke", "msg_id": 2, "identity": "a@b",
                "name": "run", "pageUrl": "https://example.com/",
            }),
        )
        .await;
        assert_eq!(reply["success"], false);
        assert_eq!(reply["message"], "rpc not exposed to page: run");
    }

    #[tokio::test]
    async fn plugin_only_sniff_rules_skips_remote_docs_and_resets_on_epoch_change() {
        let daemon = Arc::new(crate::daemon_client::DaemonClient::answering(|method| {
            if method == fluxdown_protocol::method::DAEMON_PLUGIN_SNIFF_RULES {
                Ok(json!({"sequence": 2, "epoch": "e2", "pack": {"packId": "plugin"}}))
            } else {
                Ok(json!([]))
            }
        }));
        // 指向永远连不上的数据目录：plugin_only 若触碰远程文档存储会留下可观察的错误。
        let dir = std::env::temp_dir().join(format!("fluxdown-nmh-sniff-{}", uuid::Uuid::new_v4()));
        let (service, _) = service_with(daemon);
        let service = service.with_sniff_rules(Arc::new(crate::sniff_rules::SniffRuleStore::new(
            dir.clone(),
            crate::egress::Egress::system_default(),
        )));
        let (mut client, server) = tokio::io::duplex(65536);
        tokio::spawn(handle_stream(server, service));

        let same_epoch = exchange(
            &mut client,
            json!({
                "action": "sniff_rules", "msg_id": 1, "pluginOnly": true,
                "have": {"plugin": 2, "pluginEpoch": "e2"},
            }),
        )
        .await;
        assert_eq!(same_epoch["success"], true);
        assert!(same_epoch["data"]["plugin"].is_null());
        assert_eq!(same_epoch["data"]["pluginSequence"], 2);
        assert_eq!(same_epoch["data"]["pluginEpoch"], "e2");
        assert!(same_epoch["data"]["status"].is_null());
        assert_eq!(same_epoch["data"]["deferred"], json!([]));

        // 序号没有更新，但 epoch 变了（数据库重置）：必须重发 plugin 包。
        let reset = exchange(
            &mut client,
            json!({
                "action": "sniff_rules", "msg_id": 2, "pluginOnly": true,
                "have": {"plugin": 9, "pluginEpoch": "old"},
            }),
        )
        .await;
        assert_eq!(reset["data"]["plugin"], json!({"packId": "plugin"}));
        assert_eq!(reset["data"]["pluginEpoch"], "e2");
        assert!(
            !dir.exists(),
            "plugin-only must not touch the remote doc store"
        );
    }

    #[test]
    fn plugin_pack_is_returned_only_when_newer_than_have_or_epoch_changed() {
        use super::select_plugin_pack;
        use serde_json::Value;

        let dto = |sequence, pack| fluxdown_protocol::PluginSniffRulesDto {
            sequence,
            epoch: "e1".to_owned(),
            pack,
        };
        let pack = json!({"packId": "plugin"});
        assert_eq!(
            select_plugin_pack(Some(dto(3, Some(pack.clone()))), None, Some("e1")),
            pack
        );
        assert_eq!(
            select_plugin_pack(Some(dto(3, Some(pack.clone()))), Some(2), Some("e1")),
            pack
        );
        assert_eq!(
            select_plugin_pack(Some(dto(3, Some(pack.clone()))), Some(3), Some("e1")),
            Value::Null
        );
        // epoch 不同：即使序号更小 / 相等也重发。
        assert_eq!(
            select_plugin_pack(Some(dto(1, Some(pack.clone()))), Some(9), Some("old")),
            pack
        );
        assert_eq!(
            select_plugin_pack(Some(dto(3, Some(pack.clone()))), Some(3), None),
            pack
        );
        assert_eq!(
            select_plugin_pack(Some(dto(0, None)), None, Some("e1")),
            Value::Null
        );
        assert_eq!(select_plugin_pack(None, Some(1), Some("e1")), Value::Null);
    }

    #[test]
    fn oversized_reply_defers_later_documents_but_always_keeps_the_first() {
        use super::{SNIFF_REPLY_BUDGET, defer_oversized_docs};
        use serde_json::Value;

        let big = |fill: usize| Value::String("x".repeat(fill));
        let mut data = json!({
            "revocations": {"sequence": 1},
            "official": big(SNIFF_REPLY_BUDGET / 2),
            "community": big(SNIFF_REPLY_BUDGET / 2),
            "plugin": {"packId": "plugin"},
        });
        let deferred = defer_oversized_docs(&mut data);
        assert_eq!(deferred, ["community"]);
        assert!(data["community"].is_null());
        assert!(!data["official"].is_null());
        assert!(!data["revocations"].is_null());
        // 后面体积小、仍装得下的文档照常发送，不被前面的超限连坐。
        assert!(!data["plugin"].is_null());

        // 第一份文档本身超预算也要保留，否则永远没有进展。
        let mut oversized = json!({"revocations": null, "official": big(SNIFF_REPLY_BUDGET + 1)});
        assert!(defer_oversized_docs(&mut oversized).is_empty());
        assert!(!oversized["official"].is_null());

        let mut small = json!({"official": {"sequence": 1}, "community": null});
        assert!(defer_oversized_docs(&mut small).is_empty());
    }
}
