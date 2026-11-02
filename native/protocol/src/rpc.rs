//! JSON-RPC 2.0 信封与本机服务握手。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ApplicationErrorCode, RpcErrorData, RpcErrorObject};

/// JSON-RPC wire 版本。
pub const JSONRPC_VERSION: &str = "2.0";
/// 当前本机服务协议版本；v4 增加托盘驻留（`ShellChanged`）与完成后关机（`PowerChanged`）事件，
/// v5 增加静默捕获建任务通知（`CaptureTasksStarted`），v6 增加局域网配对请求 / 发现事件
/// （`LinkPairingRequestsChanged` / `LinkDiscoveredChanged`），v7 增加 CloudConnectionChanged，
/// v8 增加应用内更新状态（`UpdateChanged` / `AgentSnapshot.update`），v9 增加「文件已存在」
/// 交互选择（`SelectionKind::FileExists` / `SelectionOutcome::FileExists`），v10 增加云端推送
/// （`DaemonEvent::TaskNotice`、`CloudNotifyChanged` / `AgentSnapshot.cloud_notify`），v11 增加
/// 批量建任务（`daemon.task.createMany`）与批量任务事实事件（`DaemonEvent::TasksChanged`），
/// v12 增加批量删除事实事件（`DaemonEvent::TasksDeleted`），
/// v13 增加插件可移植视图事件（`DaemonEvent::PluginViewFrame` / `PluginViewClosed` /
/// `PluginToast`），v14 增加多插件冲突选择（`SelectionKind::Plugin` /
/// `SelectionOutcome::Plugin`）与站点默认插件方法（`daemon.plugin.siteDefaults` /
/// `daemon.plugin.setSiteDefault`），v15 增加队列顺序增量（`WsServerMsg::QueueOrderChanged`、
/// `QueuePositionDto.order`）与落后订阅者的帧合并（`EventFrame.previous`，按
/// [`crate::judge_frame`] 判定），v16 增加外部恢复任务通知（`AgentEvent::CaptureTasksResumed`），
/// v17 取消全量任务单帧：快照与替换事件只带任务总数（`DaemonSnapshot.taskTotal` /
/// `queueTotal`），任务表经 `system.taskPage` 分页载入（`DaemonEvent::TablesReloading`），
/// `TaskDto` 默认值字段不再上线（反序列化须容忍缺省），v18–v24 曾承载的临时文件分享协议已整体移除，
/// v25 增加周期会员卡：`CloudPlan.billingPeriodDays`、`CloudProfile` / `AgentSessionDto.activeSubscription`、
/// `ErrorReason::SubscriptionConflict`，v26 增加 BT 防吸血：`daemon.bt.guard.*` 方法、
/// `DaemonEvent::{BtGuardChanged, BtGuardCloudRefreshWanted, BtGuardEvidenceReady}` 与 `BtPeerDto` 嫌疑字段
/// （纯增量，旧客户端忽略未知字段与方法）。
pub const PROTOCOL_VERSION: u32 = 26;
/// 旧客户端不能解析 v19 起的协议语义，必须在握手时拒绝混用。
pub const MIN_PROTOCOL_VERSION: u32 = 19;
/// 服务收到 `system.shutdown` 而退出时的 WebSocket 关闭原因：客户端据此停止重连与重拉。
pub const CLOSE_REASON_SERVICE_QUIT: &str = "service-quit";
/// headless 服务为应用更新而重启时的 WebSocket 关闭原因：浏览器客户端保持重连，
/// 重连后服务版本变化即重新加载页面（SPA 内嵌于新二进制）。桌面形态的更新重启仍以
/// [`CLOSE_REASON_SERVICE_QUIT`] 关闭，由 agent 退出后重新拉起桌面界面。
pub const CLOSE_REASON_SERVICE_RESTART: &str = "service-restart";
/// 客户端接收服务端 WebSocket 消息（含单帧）的上限。快照与整表替换都只带头部、任务表经
/// `system.taskPage` 分页（每页 ≤ [`crate::TASK_PAGE_MAX_LIMIT`] 行，约 4 MB），任务数再多单帧
/// 也不随之增长；上限留足 RSS 条目、日志等大应答的余量，同时让异常大帧尽早失败而不是吃满内存。
pub const MAX_INBOUND_RPC_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// 产品版本：发布流水线经 `FLUXDOWN_APP_VERSION` 注入 tag 版本，本地构建回退到本 crate 的
/// `CARGO_PKG_VERSION`。daemon / agent 对外自报版本、更新检查与诊断都以它为准，
/// 各 crate 自己的 `Cargo.toml` 版本不代表产品版本。
pub const APP_VERSION: &str = match option_env!("FLUXDOWN_APP_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => env!("CARGO_PKG_VERSION"),
};

/// 握手对端自报的产品版本 `running` 是否**确定**旧于本进程的 `own`（`主.次.修[-预发布]`，
/// 忽略 `+构建`）。任一方无法解析（开发版等）返回 `false`：拿不准时不替换。
///
/// 本机客户端据此替换残留的旧版服务进程（桌面 → agent、agent → daemon），只替换更旧的：
/// 更新的 daemon 可能已迁移数据库，换回旧版本等于降级。
///
/// ```
/// use fluxdown_protocol::is_older_app_version;
/// assert!(is_older_app_version("0.6.2", "0.6.3"));
/// assert!(!is_older_app_version("0.6.3", "0.6.3"));
/// assert!(!is_older_app_version("dev", "0.6.3"));
/// ```
#[must_use]
pub fn is_older_app_version(running: &str, own: &str) -> bool {
    fn parse(version: &str) -> Option<([u64; 3], Option<&str>)> {
        let version = version.trim().trim_start_matches('v');
        let version = version.split_once('+').map_or(version, |(core, _)| core);
        let (core, pre) = match version.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (version, None),
        };
        let mut parts = core.split('.');
        let mut triple = [0_u64; 3];
        for slot in &mut triple {
            *slot = parts.next()?.parse().ok()?;
        }
        parts.next().is_none().then_some((triple, pre))
    }
    let (Some((running, running_pre)), Some((own, own_pre))) = (parse(running), parse(own)) else {
        return false;
    };
    match running.cmp(&own) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        // 同一版本号：预发布旧于正式版；两个预发布之间不作判断。
        std::cmp::Ordering::Equal => running_pre.is_some() && own_pre.is_none(),
    }
}

/// 本机服务在 FluxDown 架构中的职责。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum ServiceRole {
    Daemon,
    Agent,
}

/// JSON-RPC 请求 ID。只接受字符串或有符号 64 位整数。
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Integer(i64),
}

/// 客户端首帧 `system.hello` 的版本与能力协商信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ClientHello {
    pub client_name: String,
    pub client_version: String,
    pub min_protocol_version: u32,
    pub max_protocol_version: u32,
    pub requested_role: ServiceRole,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// 服务端成功协商后返回的版本与能力信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ServiceHello {
    pub role: ServiceRole,
    pub service_name: String,
    pub service_version: String,
    pub protocol_version: u32,
    pub instance_id: String,
    pub capabilities: Vec<String>,
}

impl ServiceHello {
    /// 按当前协议基线创建握手响应。
    #[must_use]
    pub fn new(
        role: ServiceRole,
        service_name: impl Into<String>,
        service_version: impl Into<String>,
        instance_id: impl Into<String>,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            role,
            service_name: service_name.into(),
            service_version: service_version.into(),
            protocol_version: PROTOCOL_VERSION,
            instance_id: instance_id.into(),
            capabilities,
        }
    }
}

/// 协商客户端与服务端共同支持的最高协议版本。
pub fn negotiate_protocol(hello: &ClientHello) -> Result<u32, RpcErrorData> {
    let minimum = hello.min_protocol_version.max(MIN_PROTOCOL_VERSION);
    let maximum = hello.max_protocol_version.min(PROTOCOL_VERSION);
    if minimum <= maximum {
        Ok(maximum)
    } else {
        Err(RpcErrorData::new(
            ApplicationErrorCode::ProtocolIncompatible,
            false,
        ))
    }
}

/// 带 ID 的 JSON-RPC 调用请求。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl RpcRequest {
    /// 创建符合 JSON-RPC 2.0 的调用请求。
    #[must_use]
    pub fn new(id: RequestId, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            method: method.into(),
            params,
        }
    }

    /// 校验固定 JSON-RPC 版本。
    pub fn validate(&self) -> Result<(), RpcErrorData> {
        validate_jsonrpc_version(&self.jsonrpc)
    }
}

/// 校验连接首个 RPC 调用并解析 `system.hello`。
///
/// 服务端必须在读取任何其他方法前调用此函数；成功后才可进入正常分发。
pub fn validate_first_request(
    request: &RpcRequest,
    expected_role: ServiceRole,
) -> Result<ClientHello, RpcErrorData> {
    request.validate()?;
    if request.method != crate::method::SYSTEM_HELLO {
        return Err(invalid_rpc_field("method"));
    }
    let Some(params) = request.params.clone() else {
        return Err(invalid_rpc_field("params"));
    };
    let hello =
        serde_json::from_value::<ClientHello>(params).map_err(|_| invalid_rpc_field("params"))?;
    if hello.requested_role != expected_role {
        return Err(RpcErrorData::new(
            ApplicationErrorCode::ProtocolIncompatible,
            false,
        ));
    }
    negotiate_protocol(&hello)?;
    Ok(hello)
}

/// 不带 ID 的 JSON-RPC 通知。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl RpcNotification {
    /// 创建符合 JSON-RPC 2.0 的通知。
    #[must_use]
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.into(),
            params,
        }
    }

    /// 校验固定 JSON-RPC 版本。
    pub fn validate(&self) -> Result<(), RpcErrorData> {
        validate_jsonrpc_version(&self.jsonrpc)
    }
}

/// 一个已分型的 JSON-RPC 入站帧。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum RpcIncoming {
    Request(RpcRequest),
    Notification(RpcNotification),
}

/// 成功响应；wire 上永远没有 `error` 字段。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RpcSuccessResponse {
    pub jsonrpc: String,
    pub id: RequestId,
    pub result: Value,
}

/// 失败响应；wire 上永远没有 `result` 字段。`id: null` 只用于无法恢复 ID 的解析或无效请求。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RpcFailureResponse {
    pub jsonrpc: String,
    pub id: Option<RequestId>,
    pub error: RpcErrorObject,
}

/// 互斥的 JSON-RPC 成功或失败响应。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum RpcResponse {
    Success(RpcSuccessResponse),
    Failure(RpcFailureResponse),
}

impl RpcResponse {
    /// 创建成功响应。
    #[must_use]
    pub fn success(id: RequestId, result: Value) -> Self {
        Self::Success(RpcSuccessResponse {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result,
        })
    }

    /// 创建带可恢复请求 ID 的失败响应。
    #[must_use]
    pub fn failure(id: RequestId, error: RpcErrorObject) -> Self {
        Self::Failure(RpcFailureResponse {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id: Some(id),
            error,
        })
    }

    /// 创建无法恢复请求 ID 的 JSON 解析错误响应。
    #[must_use]
    pub fn parse_failure(message: impl Into<String>) -> Self {
        Self::Failure(RpcFailureResponse {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id: None,
            error: RpcErrorObject::parse_error(message),
        })
    }
}

fn validate_jsonrpc_version(version: &str) -> Result<(), RpcErrorData> {
    if version == JSONRPC_VERSION {
        Ok(())
    } else {
        Err(invalid_rpc_field("jsonrpc"))
    }
}

fn invalid_rpc_field(field: &str) -> RpcErrorData {
    RpcErrorData {
        code: ApplicationErrorCode::InvalidArgument,
        retryable: false,
        field: Some(field.to_owned()),
        revision: None,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        ClientHello, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, RequestId, RpcIncoming, RpcRequest,
        RpcResponse, ServiceHello, ServiceRole, negotiate_protocol, validate_first_request,
    };
    use crate::error::{ApplicationErrorCode, RpcErrorData, RpcErrorObject};

    #[test]
    fn negotiates_current_protocol_and_stable_hello_shape() -> Result<(), serde_json::Error> {
        let client = ClientHello {
            client_name: "fluxdown-desktop".to_owned(),
            client_version: "1.0.0".to_owned(),
            min_protocol_version: MIN_PROTOCOL_VERSION,
            max_protocol_version: PROTOCOL_VERSION,
            requested_role: ServiceRole::Agent,
            capabilities: vec!["client.selections".to_owned()],
        };
        assert_eq!(negotiate_protocol(&client), Ok(PROTOCOL_VERSION));

        let service = ServiceHello::new(
            ServiceRole::Agent,
            "fluxdown-agent",
            "1.0.0",
            "instance-1",
            vec!["agent.gateway".to_owned()],
        );
        let wire = serde_json::to_value(service)?;
        assert_eq!(
            wire,
            json!({
                "role": "agent",
                "serviceName": "fluxdown-agent",
                "serviceVersion": "1.0.0",
                "protocolVersion": PROTOCOL_VERSION,
                "instanceId": "instance-1",
                "capabilities": ["agent.gateway"]
            })
        );
        Ok(())
    }

    #[test]
    fn rejects_non_intersecting_protocol_range() {
        let client = ClientHello {
            client_name: "old-client".to_owned(),
            client_version: "0.1.0".to_owned(),
            min_protocol_version: 3,
            max_protocol_version: 3,
            requested_role: ServiceRole::Daemon,
            capabilities: Vec::new(),
        };

        let error = match negotiate_protocol(&client) {
            Ok(version) => panic!("v3 unexpectedly negotiated protocol {version}"),
            Err(error) => error,
        };
        assert_eq!(error.code, ApplicationErrorCode::ProtocolIncompatible);
        assert!(!error.retryable);
    }

    #[test]
    fn deserializes_requested_role_and_capabilities() -> Result<(), serde_json::Error> {
        let wire = json!({
            "clientName": "web",
            "clientVersion": "1",
            "minProtocolVersion": 2,
            "maxProtocolVersion": 2,
            "requestedRole": "daemon",
            "capabilities": []
        });
        let hello = serde_json::from_value::<ClientHello>(wire)?;
        assert_eq!(hello.requested_role, ServiceRole::Daemon);
        assert_eq!(hello.capabilities, Vec::<String>::new());
        assert_eq!(
            serde_json::to_value(hello)?["maxProtocolVersion"],
            Value::from(2)
        );
        Ok(())
    }

    #[test]
    fn requires_system_hello_as_first_request() {
        let request = RpcRequest::new(RequestId::Integer(-7), "system.ping", None);
        let error = match validate_first_request(&request, ServiceRole::Agent) {
            Ok(_) => panic!("non-hello first request unexpectedly passed"),
            Err(error) => error,
        };
        assert_eq!(error.code, ApplicationErrorCode::InvalidArgument);
        assert_eq!(error.field.as_deref(), Some("method"));
    }

    #[test]
    fn validates_hello_role_and_protocol() {
        let request = RpcRequest::new(
            RequestId::String("hello".to_owned()),
            crate::method::SYSTEM_HELLO,
            Some(json!({
                "clientName": "desktop",
                "clientVersion": "1",
                "minProtocolVersion": MIN_PROTOCOL_VERSION,
                "maxProtocolVersion": PROTOCOL_VERSION,
                "requestedRole": "agent",
                "capabilities": ["client.selections"]
            })),
        );
        let hello = match validate_first_request(&request, ServiceRole::Agent) {
            Ok(hello) => hello,
            Err(error) => panic!("valid hello rejected: {:?}", error.code),
        };
        assert_eq!(hello.client_name, "desktop");
    }

    #[test]
    fn rejects_fractional_out_of_range_and_notification_ids() {
        assert!(
            serde_json::from_value::<RpcIncoming>(json!({
                "jsonrpc": "2.0", "id": 1.5, "method": "system.ping"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RpcIncoming>(json!({
                "jsonrpc": "2.0", "id": 9223372036854775808_u64, "method": "system.ping"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RpcIncoming>(json!({
                "jsonrpc": "2.0", "id": 1, "method": "service.event", "params": {}
            }))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<super::RpcNotification>(json!({
                "jsonrpc": "2.0", "id": 1, "method": "service.event", "params": {}
            }))
            .is_err()
        );
    }

    #[test]
    fn response_variants_are_mutually_exclusive_and_null_id_is_scoped()
    -> Result<(), serde_json::Error> {
        let success = serde_json::to_value(RpcResponse::success(
            RequestId::Integer(-9),
            json!({"ok": true}),
        ))?;
        assert_eq!(success["id"], -9);
        assert!(success.get("error").is_none());

        let failure = serde_json::to_value(RpcResponse::failure(
            RequestId::String("x".to_owned()),
            RpcErrorObject::application(
                "conflict",
                RpcErrorData::new(ApplicationErrorCode::Conflict, false),
            ),
        ))?;
        assert!(failure.get("result").is_none());
        assert_eq!(failure["error"]["data"]["code"], "conflict");

        let parse = serde_json::to_value(RpcResponse::parse_failure("invalid json"))?;
        assert!(parse["id"].is_null());
        assert_eq!(parse["error"]["code"], -32700);
        Ok(())
    }

    /// 只认定确定更旧的版本：更新的服务可能已迁移数据库，开发版等无法解析时不替换。
    #[test]
    fn only_a_strictly_older_app_version_counts_as_older() {
        use super::is_older_app_version;
        assert!(is_older_app_version("0.6.2", "0.6.3"));
        assert!(is_older_app_version("v0.5.9", "0.6.3"));
        assert!(is_older_app_version("0.6.3-rc.2", "0.6.3"));
        assert!(!is_older_app_version("0.6.3", "0.6.3"));
        assert!(!is_older_app_version("0.6.4", "0.6.3"));
        assert!(!is_older_app_version("0.6.3", "0.6.3-rc.1"));
        assert!(!is_older_app_version("0.6.3-rc.1", "0.6.3-rc.2"));
        assert!(!is_older_app_version("0.6.3+build.1", "0.6.3"));
        assert!(!is_older_app_version("dev", "0.6.3"));
        assert!(!is_older_app_version("0.6.2", "dev"));
        assert!(!is_older_app_version("0.6", "0.6.3"));
    }
}
