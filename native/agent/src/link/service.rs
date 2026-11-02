//! 局域网直连（L1）服务：把 `fluxdown_link::LinkManager` 装配进 agent。
//!
//! 职责：
//! - **响应端**：兼容 API 的 `/ping`（`linkFingerprint` / `linkProtocol`）、`pair/hello`、
//!   `pair/reveal`、`pair/confirm`、数据面 `link/tasks` / `link/info` / `link/unpair`（v2：请求与
//!   响应都封装）；入站配对请求（揭示被受理后）进快照 / 事件，由 `agent.link.approve` 决定；
//!   接单时按契约做目录回退（非本机风格绝对路径 → 本机默认目录；带目录建任务失败 → 去掉目录重试）。
//! - **发起端**：mDNS 发现（`LinkDiscoveredChanged`）、手动地址探测、配对（SAS 由 UI 核对）、
//!   解除配对（尽力通知在线对端）、重命名、在线探测（`LinkDeviceInfo.online` 真实值）、下发任务、
//!   经已认证链路与对端双向交换设备信息（名称 / 回连地址 / 云设备 ID / 目录 / 路径风格）。
//! - **连通性维护**：设备长时间离线时自动短时 mDNS 浏览找回新地址；本机改名 / 云登录登出 /
//!   地址变化后主动向在线对端重新交换信息；对在线设备每 15 分钟交换一次（纯网络，信息无变化
//!   不写盘、不推送）。
//!
//! 直连与云端互相隔离：本模块任何路径都不访问 FluxCloud、不写云设备名册 / 同步目录；本机的
//! 云设备 ID 只经已认证链路告知已配对对端。
//!
//! 桌面默认只监听 `127.0.0.1`：此时局域网内的其它设备连不上本机，配对码结果的 `addresses`
//! 为空、不做 mDNS 广播、配对时不自报回连地址；`--server` 模式与 `lanEnabled` 时正常工作。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fluxdown_api::routes::{API_LINK_INFO, API_LINK_TASKS, API_LINK_UNPAIR};
use fluxdown_api::service::{ApiError, LinkAuthReason, link_unsupported};
use fluxdown_link::pairing::{CODE_TTL_SECS, DECISION_WINDOW_SECS};
use fluxdown_link::{
    DiscoveredPeer, DiscoveryKind, LINK_CONTROL_VERSION, LINK_PROTOCOL_VERSION, LinkEngineEvent,
    LinkError, LinkManager, LinkOptions, LinkResult, PairConfirmOutcome, PeerAddress, PeerInfo,
    SelfInfo, WireHello, WireReveal,
};
use fluxdown_protocol::{
    AgentEvent, ApplicationErrorCode, CreateTaskRequest, DaemonCreateTaskParams, ErrorReason,
    EventFrame, LinkAddressParams, LinkApproveParams, LinkAuth, LinkCodeResponse,
    LinkControlCredentialDto, LinkDeviceInfo, LinkDeviceParams, LinkDiscoveredPeer,
    LinkDiscoveryParams, LinkDispatchParams, LinkDispatchResult, LinkPairBeginParams,
    LinkPairBeginResponse, LinkPairConfirmOutcome, LinkPairConfirmRequest, LinkPairFinishParams,
    LinkPairFinishResponse, LinkPairHelloRequest, LinkPairHelloResponse, LinkPairRevealRequest,
    LinkPairRevealResponse, LinkPairingCodeDto, LinkPairingRequestDto, LinkPingInfo,
    LinkRenameParams, LinkStopPairingParams, LinkTaskRequest, PathStyle, RpcErrorData,
    ServiceEvent, method,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, mpsc};
use tokio_util::sync::CancellationToken;

use super::storage::{AgentLinkStorage, devices_with_online, local_path_style_text};
use crate::daemon_client::DaemonClient;
use crate::event_hub::AgentEventHub;
use crate::state::{AgentState, StateStore};

/// 同时保留的待确认入站配对请求上限（超出丢最旧）。
const PAIRING_REQUEST_CAP: usize = 16;
/// 已配对设备在线状态的后台轮询间隔。
const ONLINE_POLL_INTERVAL: Duration = Duration::from_secs(45);
/// 一轮在线探测（含并发的信息交换）的整体时限：个别设备长时间不可达不拖慢整批。
const PROBE_ROUND_TIMEOUT: Duration = Duration::from_secs(8);
/// 兼容 API `link_pair_hello` 的 URL / 文件名 / 目录长度上限（同 FluxCloud 下发契约）。
const MAX_URL_LEN: usize = 8192;
const MAX_FILE_NAME_LEN: usize = 255;
const MAX_SAVE_DIR_LEN: usize = 1024;

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// 距 `expires_at_unix_ms` 的剩余毫秒（已过期 = 0）。
fn remaining_ms(expires_at_unix_ms: i64, now_ms: i64) -> u64 {
    u64::try_from(expires_at_unix_ms.saturating_sub(now_ms)).unwrap_or(0)
}

/// 按当前时刻重算入站配对请求的相对剩余时间 `ttl_ms`：客户端以「收到 DTO 的本机时刻 + `ttl_ms`」
/// 计算过期，不依赖与主机的时钟对齐。事件发布与取快照时都要重算——存着的副本只有
/// `expires_at_unix_ms` 是恒定的。
pub(crate) fn refresh_request_ttls(requests: &mut [LinkPairingRequestDto]) {
    let now = now_unix_ms();
    for request in requests {
        request.ttl_ms = remaining_ms(request.expires_at_unix_ms, now);
    }
}

/// 本机建下载任务的入口（生产实现走 daemon；测试注入记录型实现）。
#[async_trait]
pub trait LinkTaskCreator: Send + Sync {
    /// 以「无人值守」方式创建接收任务，处理可选本机分类路由并返回任务 ID。
    async fn create_task_with_route(
        &self,
        request: CreateTaskRequest,
        category_route: Option<fluxdown_protocol::CategoryRouteDto>,
    ) -> Result<String, ApiError>;
}

/// 经 daemon JSON-RPC 创建任务。
pub struct DaemonTaskCreator {
    daemon: Arc<DaemonClient>,
}

impl DaemonTaskCreator {
    #[must_use]
    pub fn new(daemon: Arc<DaemonClient>) -> Self {
        Self { daemon }
    }
}

#[async_trait]
impl LinkTaskCreator for DaemonTaskCreator {
    async fn create_task_with_route(
        &self,
        request: CreateTaskRequest,
        category_route: Option<fluxdown_protocol::CategoryRouteDto>,
    ) -> Result<String, ApiError> {
        let created: Value = self
            .daemon
            .call_detailed(
                method::DAEMON_TASK_CREATE,
                Some(DaemonCreateTaskParams {
                    request,
                    torrent_blob_id: None,
                    // 对端下发的任务没有人在本机确认选择框：全选文件直接开始。
                    unattended: true,
                    hint_file_size: None,
                    name_hint: None,
                    category_route,
                }),
            )
            .await
            .map_err(crate::api_host::object_error)?;
        created
            .get("taskId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Internal("daemon returned no taskId".to_owned()))
    }
}

/// 在线对端设备信息的周期性交换间隔（纯网络；信息无变化不写盘、不推送）。
const INFO_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// 有设备离线时自动 mDNS 浏览找回新地址的最小间隔。
const HEAL_MIN_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// 单次自动浏览的时长。
const HEAL_BROWSE_DURATION: Duration = Duration::from_secs(15);

/// 网络层选项：mDNS 开关与自报回连地址覆盖。生产环境取 [`LinkNetworkOptions::from_env`]，
/// 测试直接构造（不依赖进程环境变量）。
#[derive(Clone, Debug)]
pub struct LinkNetworkOptions {
    /// 是否允许 mDNS 广播 / 浏览（`FLUXDOWN_MDNS=off` 时为 `false`）。
    pub mdns: bool,
    /// 覆盖本机自报回连地址的原始条目（`FLUXDOWN_LINK_ADVERTISE_ADDR`：逗号分隔的 `host[:port]`
    /// 或 http(s) URL，缺省端口取网关实际端口）；空 = 自动探测。容器 / NAS / 多网卡下自动探测到
    /// 的往往是对端连不通的内部地址。
    pub advertise: Vec<String>,
}

impl LinkNetworkOptions {
    /// 从环境变量读取。
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            mdns: mdns_enabled(),
            advertise: advertise_entries_from_env(),
        }
    }
}

/// 服务装配参数。
pub struct LinkServiceParts {
    pub events: AgentEventHub,
    pub state: Arc<Mutex<AgentState>>,
    pub store: Arc<StateStore>,
    pub tasks: Arc<dyn LinkTaskCreator>,
    /// 网关实际监听地址（决定本机是否可被局域网访问，以及 mDNS 广播端口）。
    pub bound: SocketAddr,
    /// `--server` 模式（headless / NAS）。
    pub server_mode: bool,
    /// 网络层选项。
    pub network: LinkNetworkOptions,
}

/// 互联操作的失败：互联层错误 + 服务层校验错误。
#[derive(Debug, thiserror::Error)]
pub enum LinkOpError {
    #[error(transparent)]
    Link(#[from] LinkError),
    /// 保存目录不是目标设备路径风格下的绝对路径。
    #[error("save directory is not an absolute path for the target device")]
    SaveDirUnavailable,
    /// 参数非法（字段名）。
    #[error("invalid {0}")]
    Invalid(&'static str),
}

/// RPC 错误映射的上下文：同一种网络失败在配对里是「对端不可达」，在已配对设备上是「设备离线」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorContext {
    Pairing,
    Peer,
    Local,
}

/// 互联操作错误 → `agent.link.*` 的稳定错误详情（含本地化用的 `reason`）。
#[must_use]
pub fn rpc_error(error: &LinkOpError, context: ErrorContext) -> RpcErrorData {
    use ApplicationErrorCode as Code;
    let unreachable_reason = match context {
        ErrorContext::Pairing => Some(ErrorReason::PairingPeerUnreachable),
        ErrorContext::Peer => Some(ErrorReason::PeerOffline),
        ErrorContext::Local => None,
    };
    let (code, retryable, reason) = match error {
        LinkOpError::SaveDirUnavailable => {
            return RpcErrorData {
                field: Some("saveDir".to_owned()),
                ..RpcErrorData::new(Code::InvalidArgument, false)
                    .with_reason(ErrorReason::SaveDirUnavailable)
            };
        }
        LinkOpError::Invalid(field) => {
            return RpcErrorData {
                field: Some((*field).to_owned()),
                ..RpcErrorData::new(Code::InvalidArgument, false)
            };
        }
        LinkOpError::Link(error) => match error {
            LinkError::InvalidCode => (
                Code::InvalidArgument,
                false,
                Some(ErrorReason::PairingCodeInvalid),
            ),
            LinkError::SessionExpired => (
                Code::NotFound,
                false,
                Some(ErrorReason::PairingSessionExpired),
            ),
            LinkError::PairingTimeout => (Code::Timeout, false, Some(ErrorReason::PairingTimeout)),
            LinkError::BadSignature => (
                Code::InvalidArgument,
                false,
                Some(ErrorReason::PairingSignatureInvalid),
            ),
            LinkError::SelfPairing => {
                (Code::InvalidArgument, false, Some(ErrorReason::PairingSelf))
            }
            LinkError::Throttled => (Code::Unavailable, true, Some(ErrorReason::PairingThrottled)),
            LinkError::RejectedByPeer => {
                (Code::Conflict, false, Some(ErrorReason::PairingRejected))
            }
            LinkError::NotFluxDown(_) => (
                Code::InvalidArgument,
                false,
                Some(ErrorReason::PairingNotFluxDown),
            ),
            // 配对阶段：地址上出示的身份与发现指纹不符，按身份校验失败提示；
            // 已配对设备上：拨通的不是那台设备，按离线处理。
            LinkError::IdentityMismatch(_) => {
                let reason = if context == ErrorContext::Pairing {
                    ErrorReason::PairingSignatureInvalid
                } else {
                    ErrorReason::PeerOffline
                };
                (Code::Conflict, false, Some(reason))
            }
            LinkError::CommitmentMismatch => (
                Code::InvalidArgument,
                false,
                Some(ErrorReason::PairingSignatureInvalid),
            ),
            LinkError::UnsupportedVersion => (
                Code::Conflict,
                false,
                Some(ErrorReason::PairingVersionMismatch),
            ),
            // 对端数据面协议过旧：两端都要升级，重试无用。
            LinkError::PeerOutdated => (Code::Conflict, false, Some(ErrorReason::PeerOutdated)),
            LinkError::Unauthorized => {
                (Code::Unauthorized, false, Some(ErrorReason::PeerNotPaired))
            }
            // 两台设备的系统时钟偏差过大：临时、可由用户修正，与「未配对」区分提示。
            LinkError::ClockSkew => (Code::Unauthorized, false, Some(ErrorReason::PeerClockSkew)),
            LinkError::NotPaired => (Code::NotFound, false, Some(ErrorReason::PeerNotPaired)),
            LinkError::Unreachable | LinkError::Io(_) => {
                (Code::Unavailable, true, unreachable_reason)
            }
            LinkError::BadPayload(_) => (Code::InvalidArgument, false, None),
            LinkError::Store(_) => (Code::Internal, false, None),
            // 本机直连服务没有启动（初始化失败 / 宿主不支持）：与「对端不可达」明确区分。
            LinkError::Unavailable => (Code::Unavailable, true, Some(ErrorReason::LinkUnavailable)),
        },
    };
    let data = RpcErrorData::new(code, retryable);
    match reason {
        Some(reason) => data.with_reason(reason),
        None => data,
    }
}

/// 把 `Result<T, LinkOpError>` 转成 RPC 结果值；失败时把带完整根因的原因记进日志
/// （RPC 错误对象只携带稳定的 code + reason，不带自由文本）。
pub fn rpc_value<T: serde::Serialize>(
    result: Result<T, LinkOpError>,
    context: ErrorContext,
) -> Result<Value, RpcErrorData> {
    match result {
        Ok(value) => serde_json::to_value(value)
            .map_err(|_| RpcErrorData::new(ApplicationErrorCode::Internal, false)),
        Err(error) => {
            tracing::warn!(error = %error, "agent.link request failed");
            Err(rpc_error(&error, context))
        }
    }
}

/// 互联层错误 → 兼容 API 错误（保留历史响应状态码）。
fn api_error(error: LinkError) -> ApiError {
    match error {
        LinkError::Unauthorized | LinkError::NotPaired | LinkError::ClockSkew => {
            ApiError::Unauthorized
        }
        LinkError::InvalidCode
        | LinkError::BadSignature
        | LinkError::BadPayload(_)
        | LinkError::SelfPairing
        | LinkError::SessionExpired
        | LinkError::Throttled
        | LinkError::RejectedByPeer
        | LinkError::PairingTimeout
        | LinkError::IdentityMismatch(_)
        | LinkError::CommitmentMismatch
        | LinkError::UnsupportedVersion
        | LinkError::PeerOutdated
        | LinkError::NotFluxDown(_) => ApiError::BadRequest(error.to_string()),
        LinkError::Unreachable | LinkError::Unavailable => ApiError::Unavailable,
        LinkError::Io(_) | LinkError::Store(_) => ApiError::Internal(error.to_string()),
    }
}

/// 数据面（`link/tasks` / `link/info` / `link/unpair`）的错误映射：鉴权类失败带上诊断 `reason`
/// （未认证响应，仅作排错），其余沿用 [`api_error`]。
fn data_plane_error(error: LinkError) -> ApiError {
    match error {
        LinkError::NotPaired => ApiError::LinkAuth(LinkAuthReason::NotPaired),
        LinkError::ClockSkew => ApiError::LinkAuth(LinkAuthReason::ClockSkew),
        LinkError::Unauthorized => ApiError::LinkAuth(LinkAuthReason::Unauthorized),
        other => api_error(other),
    }
}

fn op_api_error(error: LinkOpError) -> ApiError {
    match error {
        LinkOpError::Link(error) => api_error(error),
        LinkOpError::SaveDirUnavailable => ApiError::BadRequest(
            "save directory is not an absolute path for the target device".to_owned(),
        ),
        LinkOpError::Invalid(field) => ApiError::BadRequest(format!("invalid {field}")),
    }
}

fn discovered_dto(peer: DiscoveredPeer) -> LinkDiscoveredPeer {
    LinkDiscoveredPeer {
        fingerprint: peer.fingerprint,
        name: peer.name,
        platform: peer.platform,
        host: peer.host,
        port: peer.port,
        app_version: peer.app_version,
        source: match peer.kind {
            DiscoveryKind::Mdns => "mdns",
            DiscoveryKind::Manual => "manual",
        }
        .to_owned(),
    }
}

fn non_empty(text: String) -> Option<String> {
    if text.is_empty() { None } else { Some(text) }
}

fn valid_url(url: &str) -> bool {
    !url.is_empty()
        && url.len() <= MAX_URL_LEN
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// 远端（已配对设备 / 云端）下发的文件名：单一路径分量，且不是 Windows 保留设备名。
#[must_use]
pub fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_LEN
        && !name.contains(['/', '\\', '\0'])
        && name != "."
        && name != ".."
        && !is_reserved_device_name(name)
}

fn is_reserved_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '\t'])
        .to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        other => {
            let digit = other
                .strip_prefix("COM")
                .or_else(|| other.strip_prefix("LPT"));
            digit.is_some_and(|rest| {
                let mut chars = rest.chars();
                matches!(chars.next(), Some('1'..='9' | '¹' | '²' | '³')) && chars.next().is_none()
            })
        }
    }
}

fn valid_save_dir(dir: &str) -> bool {
    dir.len() <= MAX_SAVE_DIR_LEN && !dir.contains('\0')
}

/// 目录不含 `..` 段，且不是 UNC / 设备命名空间路径（`\\host\share`、`\\?\`、`\\.\`）：
/// 后者在 Windows 上创建目录会主动触发对外 SMB 认证。
fn is_plain_local_dir(dir: &str) -> bool {
    if dir.contains('\0') || dir.split(['/', '\\']).any(|segment| segment == "..") {
        return false;
    }
    let mut chars = dir.chars();
    let is_separator = |c: char| c == '/' || c == '\\';
    !(chars.next().is_some_and(is_separator) && chars.next().is_some_and(is_separator))
}

/// 接单目录：对端给的保存目录必须是**本机路径风格**的绝对路径且通过 [`is_plain_local_dir`]
/// 才采用，否则（含空串）返回 `None`，由 daemon 使用本机默认目录（例如 Windows 设备下发
/// `C:\Downloads` 给 macOS）。云端接单与局域网互联共用这一规则。
#[must_use]
pub fn resolve_receive_dir(save_dir: &str) -> Option<String> {
    let dir = save_dir.trim();
    (!dir.is_empty() && PathStyle::current().is_absolute(dir) && is_plain_local_dir(dir))
        .then(|| dir.to_owned())
}

fn build_task_request(
    url: &str,
    file_name: &str,
    save_dir: &str,
) -> Result<CreateTaskRequest, ApiError> {
    serde_json::from_value(json!({ "url": url, "fileName": file_name, "saveDir": save_dir }))
        .map_err(|error| ApiError::Internal(format!("could not build task request: {error}")))
}

/// 本机网关可被局域网设备直连的基址（`http://ip:port`）：只监听回环 → 空；绑定了具体
/// 非回环地址 → 就是它；绑定通配地址 → 经 UDP 出站网卡探测得到各网段本机 IP。
#[must_use]
pub fn lan_base_urls(bound: SocketAddr) -> Vec<String> {
    let ip = bound.ip();
    if ip.is_loopback() {
        return Vec::new();
    }
    if !ip.is_unspecified() {
        return vec![format!("http://{bound}")];
    }
    let mut urls: Vec<String> = Vec::new();
    for target in ["8.8.8.8", "192.168.0.1", "10.0.0.1", "172.16.0.1"] {
        for candidate in fluxdown_link::discovery::local_direct_addrs(target, bound.port()) {
            let Ok(addr) = candidate.parse::<SocketAddr>() else {
                continue;
            };
            let unusable = match addr.ip() {
                IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified() || v4.is_link_local(),
                IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
            };
            let url = format!("http://{addr}");
            if !unusable && !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    urls
}

fn mdns_enabled() -> bool {
    std::env::var("FLUXDOWN_MDNS").map_or(true, |value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

fn advertise_entries_from_env() -> Vec<String> {
    std::env::var("FLUXDOWN_LINK_ADVERTISE_ADDR")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// 自报地址覆盖条目补全端口：完整 URL 与已带端口的 `host:port` / `[v6]:port` 原样保留，
/// 缺端口的主机（含裸 IPv6 字面量）用网关实际端口。
fn with_default_port(entry: &str, port: u16) -> String {
    if entry.contains("://") {
        return entry.to_owned();
    }
    if entry.starts_with('[') {
        return if entry.contains("]:") {
            entry.to_owned()
        } else {
            format!("{entry}:{port}")
        };
    }
    match entry.matches(':').count() {
        0 => format!("{entry}:{port}"),
        1 => entry.to_owned(),
        _ => format!("[{entry}]:{port}"),
    }
}

/// 覆盖条目按网关实际监听地址补全端口。
fn resolved_advertise_entries(entries: &[String], bound: SocketAddr) -> Vec<String> {
    entries
        .iter()
        .map(|entry| with_default_port(entry, bound.port()))
        .collect()
}

fn trusted_proxies_from_env() -> Vec<IpAddr> {
    std::env::var("FLUXDOWN_TRUSTED_PROXIES")
        .map(|value| {
            value
                .split(',')
                .filter_map(|entry| entry.trim().parse::<IpAddr>().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// 事件是否可能改变本机自报身份：云账号登录 / 登出（`SessionChanged`）与云设备改名
/// （`CloudDevicesChanged`）。
fn affects_link_identity(frame: &EventFrame) -> bool {
    matches!(
        &frame.event,
        ServiceEvent::Agent(AgentEvent::SessionChanged(_) | AgentEvent::CloudDevicesChanged(_))
    )
}

/// 用于判断设备列表是否真的变化的指纹：整条设备视图的 Debug 文本，但忽略在线期间每轮都会
/// 刷新的 `last_seen_at`——其余字段（名称、在线、目录、版本、云设备 ID、撤销标记……）任何
/// 一个变化都算变化。
fn roster_signature(devices: &[LinkDeviceInfo]) -> Vec<String> {
    devices
        .iter()
        .map(|device| {
            let mut stable = device.clone();
            stable.last_seen_at = 0;
            format!("{stable:?}")
        })
        .collect()
}

/// agent 的局域网直连服务。
pub struct LinkService {
    events: AgentEventHub,
    state: Arc<Mutex<AgentState>>,
    store: Arc<StateStore>,
    tasks: Arc<dyn LinkTaskCreator>,
    bound: StdMutex<SocketAddr>,
    server_mode: bool,
    network: LinkNetworkOptions,
    trusted_proxies: Vec<IpAddr>,
    manager: OnceLock<Arc<LinkManager>>,
    started: Notify,
    online: StdMutex<HashMap<String, bool>>,
    requests: StdMutex<Vec<LinkPairingRequestDto>>,
    code_generation: AtomicU64,
    /// 当前展示的配对码（`agent.link.stopPairing {code}` 只在它仍是当前码时才作废）。
    current_code: StdMutex<Option<String>>,
    /// 名册里设备凭据相关的记录发生变化（配对 / 解除 / 被对端撤销）时递增：已建立的 `/rpc` 设备
    /// 会话据此重新校验自己的凭据，失效即断开（见 [`Self::subscribe_roster`]）。
    roster_epoch: tokio::sync::watch::Sender<u64>,
    /// 每台设备最近一次成功交换设备信息的时刻：决定「设备刚上线 / 久未交换」时是否要再交换。
    last_info_exchange: StdMutex<HashMap<String, Instant>>,
    /// 最近一次自动 mDNS 浏览的启动时刻（限频）。
    last_heal: StdMutex<Option<Instant>>,
    /// 自身的弱引用：`&self` 方法里需要 spawn 后台任务时升级成 `Arc`。
    weak: Weak<LinkService>,
}

fn lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl LinkService {
    #[must_use]
    pub fn new(parts: LinkServiceParts) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            events: parts.events,
            state: parts.state,
            store: parts.store,
            tasks: parts.tasks,
            bound: StdMutex::new(parts.bound),
            server_mode: parts.server_mode,
            network: parts.network,
            trusted_proxies: trusted_proxies_from_env(),
            manager: OnceLock::new(),
            started: Notify::new(),
            online: StdMutex::new(HashMap::new()),
            requests: StdMutex::new(Vec::new()),
            code_generation: AtomicU64::new(0),
            current_code: StdMutex::new(None),
            roster_epoch: tokio::sync::watch::Sender::new(0),
            last_info_exchange: StdMutex::new(HashMap::new()),
            last_heal: StdMutex::new(None),
            weak: weak.clone(),
        })
    }

    /// 刷新已验证成功的网关监听端口；保留 IP、身份、配对与可达范围。
    ///
    /// 启动前只记录最新地址，不等待 daemon 或互联初始化。广告更新失败时保留旧地址；
    /// 传入旧地址可用于网关事务回滚。端口真的变了时，对端手里的回连地址已经过期，主动向在线
    /// 对端重新交换设备信息。
    pub async fn update_bound(&self, bound: SocketAddr) -> Result<(), LinkOpError> {
        let changed = {
            let mut current = lock(&self.bound);
            let mut expected = *current;
            expected.set_port(bound.port());
            if expected != bound {
                return Err(LinkOpError::Invalid("bound"));
            }
            if let Some(manager) = self.manager.get() {
                manager.update_api_port(bound.port())?;
                manager.set_advertise_addrs(&resolved_advertise_entries(
                    &self.network.advertise,
                    bound,
                ));
            }
            let changed = *current != bound;
            *current = bound;
            changed
        };
        if changed {
            self.spawn_exchange_all();
        }
        Ok(())
    }

    /// 本机展示名：`FLUXDOWN_LINK_NAME` 优先；server 模式固定名；否则云端 / 系统设备名。
    fn link_name(&self, device_name: &str) -> String {
        std::env::var("FLUXDOWN_LINK_NAME")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                if self.server_mode {
                    "FluxDown Server".to_owned()
                } else if device_name.trim().is_empty() {
                    "FluxDown".to_owned()
                } else {
                    device_name.to_owned()
                }
            })
    }

    /// 本机自报身份的输入：设备名，以及（已登录云账号时）云设备 ID。云设备 ID 只用于经已认证
    /// 链路告知已配对对端，本模块不会用它访问 FluxCloud。
    async fn identity_inputs(&self) -> (String, Option<String>) {
        let state = self.state.lock().await;
        let cloud_device_id = (state.credentials.is_some() && !state.device_id.is_empty())
            .then(|| state.device_id.clone());
        (state.device_name.clone(), cloud_device_id)
    }

    /// 把本机设备名 / 云设备 ID 同步给互联层（改名、云登录 / 登出后调用）。有变化时立即向在线
    /// 对端重新交换设备信息，对端的名册随之更新。
    pub async fn sync_self_identity(&self) {
        let Some(manager) = self.manager.get() else {
            return;
        };
        let (device_name, cloud_device_id) = self.identity_inputs().await;
        let name = self.link_name(&device_name);
        match manager.update_self(&name, cloud_device_id.as_deref()) {
            Ok(true) => self.spawn_exchange_all(),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(error = %error, "could not refresh the local link identity")
            }
        }
    }

    /// 本机网关可被局域网设备直连的基址（配对码结果的 `addresses`）：只监听回环 → 空；
    /// 配置了自报地址覆盖则取覆盖值，否则按监听地址自动探测。
    fn advertised_base_urls(&self) -> Vec<String> {
        let bound = *lock(&self.bound);
        // 配置了自报地址覆盖（反代 / 端口映射 / 容器）时，即使监听回环也对外可达。
        if bound.ip().is_loopback() && self.network.advertise.is_empty() {
            return Vec::new();
        }
        let overridden: Vec<String> = resolved_advertise_entries(&self.network.advertise, bound)
            .iter()
            .filter_map(|entry| PeerAddress::parse(entry).ok())
            .map(|address| address.base_url())
            .collect();
        if overridden.is_empty() {
            lan_base_urls(bound)
        } else {
            overridden
        }
    }

    /// 加载（或首次生成）本机身份并启动互联。必须在 legacy 迁移完成后调用：迁移可能
    /// 写入 daemon 时代的身份与名册，提前生成身份会被随后的迁移覆盖。
    pub async fn start(self: &Arc<Self>) -> LinkResult<()> {
        if self.manager.get().is_some() {
            return Ok(());
        }
        let (device_name, cloud_device_id) = self.identity_inputs().await;
        let self_info = SelfInfo {
            name: self.link_name(&device_name),
            platform: Some(if self.server_mode {
                "server".to_owned()
            } else {
                std::env::consts::OS.to_owned()
            }),
            app_version: Some(fluxdown_protocol::APP_VERSION.to_owned()),
            cloud_device_id,
        };
        let bound = *lock(&self.bound);
        // 只监听回环但配置了自报地址覆盖：外部经反代 / 端口映射到达本机，视为可达。
        let reachable = !bound.ip().is_loopback() || !self.network.advertise.is_empty();
        let options = LinkOptions {
            api_port: bound.port(),
            reachable,
            advertise: self.network.mdns,
            advertise_addrs: resolved_advertise_entries(&self.network.advertise, bound),
        };
        let (tx, rx) = mpsc::channel::<LinkEngineEvent>(64);
        let storage = Arc::new(AgentLinkStorage::new(
            self.state.clone(),
            self.store.clone(),
        ));
        let manager = LinkManager::load(storage, self_info, options, tx).await?;
        tracing::info!(
            fingerprint = manager.fingerprint(),
            lan_reachable = reachable,
            "device link ready"
        );
        {
            // load 会等待状态存储；初始化期间重绑不能被它挡住。发布前在同一短锁下追上
            // 最新端口，避免 update_bound 看见空 OnceLock 后初始化又发布旧端口。
            let bound = lock(&self.bound);
            manager.update_api_port(bound.port())?;
            manager
                .set_advertise_addrs(&resolved_advertise_entries(&self.network.advertise, *bound));
            if self.manager.set(manager).is_err() {
                return Ok(());
            }
        }
        tokio::spawn(Self::pump(Arc::downgrade(self), rx));
        self.started.notify_one();
        self.publish_devices().await;
        Ok(())
    }

    /// 已配对设备在线状态的后台轮询 + 本机身份（改名 / 云登录登出）变化的跟踪；`start` 完成后
    /// 才开始。启动后的首轮探测立即执行（并对在线设备交换设备信息）。
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = self.started.notified() => {}
        }
        let (mut events, _snapshot) = self.events.subscribe_and_snapshot();
        // 订阅之后补一次同步：覆盖订阅之前已经发生的登录 / 改名。
        self.sync_self_identity().await;
        let mut tick = tokio::time::interval(ONLINE_POLL_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {
                    if let Err(error) = self.probe_all(false).await {
                        tracing::debug!(error = %error, "linked device probe round failed");
                    }
                }
                frame = events.recv() => match frame {
                    Ok(frame) => {
                        if affects_link_identity(&frame) {
                            self.sync_self_identity().await;
                        }
                    }
                    // 落后丢了事件：无从知道丢的是什么，直接按当前状态重新同步一次。
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.sync_self_identity().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    }

    pub(super) fn manager(&self) -> Result<&Arc<LinkManager>, LinkError> {
        self.manager.get().ok_or(LinkError::Unavailable)
    }

    /// 兼容 API 面：未启动 = 本宿主不支持设备互联。
    fn api_manager(&self) -> Result<&Arc<LinkManager>, ApiError> {
        self.manager.get().ok_or_else(link_unsupported)
    }

    /// 转发 [`LinkManager`] 事件到快照 / 事件流。只持 `Weak`：转发任务自己不能成为服务存活的理由——
    /// 服务拥有 `LinkManager`（持有事件发送端），任务若再攥一份 `Arc<Self>`，两者互相拽住，宿主
    /// 关停后状态目录的锁永远不释放（嵌入式移动宿主要在同一进程里重启）。
    async fn pump(weak: Weak<Self>, mut rx: mpsc::Receiver<LinkEngineEvent>) {
        while let Some(event) = rx.recv().await {
            let Some(service) = weak.upgrade() else {
                break;
            };
            service.handle_event(event).await;
        }
    }

    async fn handle_event(self: &Arc<Self>, event: LinkEngineEvent) {
        {
            match event {
                LinkEngineEvent::Discovered(_) | LinkEngineEvent::DiscoveredLost => {
                    self.publish_discovered();
                }
                LinkEngineEvent::Paired(record) => {
                    tracing::info!(
                        name = %record.name,
                        fingerprint = %record.short_fingerprint(),
                        grants_control = record.grants_control,
                        controllable = record.controllable,
                        "device paired"
                    );
                    self.drop_requests(|request| request.peer_fingerprint == record.fingerprint);
                    // 刚配对完成：对端此刻必然在线（响应方这一侧没有走 `pair_finish` 的在线探测）。
                    lock(&self.online).insert(record.fingerprint.clone(), true);
                    self.bump_roster();
                    self.publish_devices().await;
                    // 双方都拿到对端的名称 / 地址 / 云设备 ID / 默认目录 / 路径风格；失败（旧版
                    // 对端、暂时不可达）静默。
                    let service = self.clone();
                    let fingerprint = record.fingerprint;
                    tokio::spawn(async move {
                        if service.exchange_info(&fingerprint).await == Some(true) {
                            service.publish_devices().await;
                        }
                    });
                }
                LinkEngineEvent::Unpaired(fingerprint) => {
                    lock(&self.online).remove(&fingerprint);
                    lock(&self.last_info_exchange).remove(&fingerprint);
                    self.bump_roster();
                    self.publish_devices().await;
                }
                LinkEngineEvent::Revoked(fingerprint) => {
                    tracing::info!(
                        fingerprint = %fingerprint.chars().take(12).collect::<String>(),
                        "linked device revoked the pairing"
                    );
                    lock(&self.online).insert(fingerprint, false);
                    self.bump_roster();
                    self.publish_devices().await;
                }
                LinkEngineEvent::IncomingCancelled { session_id } => {
                    self.drop_requests(|request| request.session_id == session_id);
                }
                LinkEngineEvent::IncomingPairing {
                    session_id,
                    sas,
                    peer_name,
                    peer_platform,
                    peer_fingerprint,
                    requests_control,
                } => self.add_request(LinkPairingRequestDto {
                    session_id,
                    peer_name,
                    peer_fingerprint,
                    peer_platform,
                    sas,
                    expires_at_unix_ms: now_unix_ms().saturating_add(
                        i64::try_from(DECISION_WINDOW_SECS * 1000).unwrap_or(60_000),
                    ),
                    ttl_ms: DECISION_WINDOW_SECS * 1000,
                    requests_control,
                }),
                LinkEngineEvent::Error(message) => {
                    tracing::warn!(error = %message, "device link subsystem error");
                }
            }
        }
    }

    // ── 快照 / 事件 ────────────────────────────────────────────────────────

    async fn devices(&self) -> Vec<LinkDeviceInfo> {
        let online = lock(&self.online).clone();
        let state = self.state.lock().await;
        devices_with_online(&state, &online)
    }

    /// 订阅名册变化：每当配对完成、解除配对或被对端撤销时值递增。`/rpc` 设备会话借此重新校验
    /// 凭据（设备记录被删除 / revoked / 重新配对导致 `link_secret` 变化 / `grants_control`
    /// 变化都会让旧凭据失效）。
    #[must_use]
    pub fn subscribe_roster(&self) -> tokio::sync::watch::Receiver<u64> {
        self.roster_epoch.subscribe()
    }

    fn bump_roster(&self) {
        self.roster_epoch.send_modify(|epoch| *epoch += 1);
    }

    /// 校验 `/rpc` 设备控制凭据（被控主机侧）：通过返回设备指纹。互联服务未启动 → `None`。
    pub async fn verify_control_credential(&self, token: &str) -> Option<String> {
        self.manager.get()?.verify_control_credential(token).await
    }

    /// `agent.link.controlCredential`：为「本机可控制」的已配对主机签发 `/rpc` 设备控制凭据。
    pub async fn control_credential(
        &self,
        fingerprint: &str,
    ) -> Result<LinkControlCredentialDto, LinkOpError> {
        let token = self.manager()?.control_credential(fingerprint).await?;
        Ok(LinkControlCredentialDto { token })
    }

    async fn publish_devices(&self) {
        let devices = self.devices().await;
        self.events
            .publish(AgentEvent::LinkedDevicesChanged(devices));
    }

    fn publish_discovered(&self) {
        let peers = self
            .manager
            .get()
            .map(|manager| {
                manager
                    .discovered_peers()
                    .into_iter()
                    .map(discovered_dto)
                    .collect()
            })
            .unwrap_or_default();
        self.events
            .publish(AgentEvent::LinkDiscoveredChanged(peers));
    }

    fn publish_requests(&self) {
        let mut requests = lock(&self.requests).clone();
        // 事件里的 `ttl_ms` 是发布这一刻的剩余时间（存着的副本只保证 `expires_at_unix_ms` 准确）。
        refresh_request_ttls(&mut requests);
        self.events
            .publish(AgentEvent::LinkPairingRequestsChanged(requests));
    }

    fn add_request(self: &Arc<Self>, request: LinkPairingRequestDto) {
        let session_id = request.session_id.clone();
        let expires_in = Duration::from_secs(DECISION_WINDOW_SECS);
        {
            let mut requests = lock(&self.requests);
            requests.retain(|existing| existing.session_id != session_id);
            requests.push(request);
            while requests.len() > PAIRING_REQUEST_CAP {
                requests.remove(0);
            }
        }
        self.publish_requests();
        // 核验窗口过后请求自然作废：从快照里移除，UI 不再显示一个已无法批准的弹窗。定时任务只持
        // `Weak`：服务释放后不必等窗口走完。
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(expires_in).await;
            if let Some(service) = weak.upgrade() {
                service.drop_requests(|request| request.session_id == session_id);
            }
        });
    }

    fn drop_requests(&self, matches: impl Fn(&LinkPairingRequestDto) -> bool) {
        let removed = {
            let mut requests = lock(&self.requests);
            let before = requests.len();
            requests.retain(|request| !matches(request));
            requests.len() != before
        };
        if removed {
            self.publish_requests();
        }
    }

    // ── 本机信息 ───────────────────────────────────────────────────────────

    /// 本机默认下载目录 / 路径风格（经已认证链路自报给已配对对端）。
    fn local_peer_info(&self) -> PeerInfo {
        let configured = self.events.inspect(|snapshot| {
            snapshot
                .daemon
                .config
                .values
                .get("default_save_dir")
                .map(|dir| dir.trim().to_owned())
        });
        let default_save_dir = configured.filter(|dir| !dir.is_empty()).or_else(|| {
            directories::UserDirs::new().and_then(|dirs| {
                dirs.download_dir()
                    .map(|dir| dir.to_string_lossy().into_owned())
            })
        });
        PeerInfo {
            default_save_dir,
            path_style: Some(local_path_style_text().to_owned()),
            ..PeerInfo::default()
        }
    }

    /// 与对端交换设备信息。成功返回 `Some(名册记录是否变化)`；失败（不可达、对端版本过旧、
    /// 对端已解除配对……）返回 `None`。
    pub(super) async fn exchange_info(&self, fingerprint: &str) -> Option<bool> {
        let manager = self.manager().ok()?;
        match manager
            .exchange_peer_info(fingerprint, &self.local_peer_info())
            .await
        {
            Ok(changed) => {
                lock(&self.last_info_exchange).insert(fingerprint.to_owned(), Instant::now());
                Some(changed)
            }
            Err(error) => {
                tracing::debug!(fingerprint, error = %error, "peer info exchange failed");
                None
            }
        }
    }

    /// 距上次成功交换已超过 [`INFO_REFRESH_INTERVAL`]（或从未交换）。
    pub(super) fn info_exchange_due(&self, fingerprint: &str) -> bool {
        lock(&self.last_info_exchange)
            .get(fingerprint)
            .is_none_or(|at| at.elapsed() >= INFO_REFRESH_INTERVAL)
    }

    /// 后台向全部未被解除的设备重新交换设备信息（本机改名 / 云登录登出 / 地址变化之后）。
    fn spawn_exchange_all(&self) {
        let Some(service) = self.weak.upgrade() else {
            return;
        };
        tokio::spawn(async move {
            service.exchange_all().await;
        });
    }

    async fn exchange_all(&self) {
        let Some(manager) = self.manager.get() else {
            return;
        };
        let records = match manager.list_devices().await {
            Ok(records) => records,
            Err(error) => {
                tracing::debug!(error = %error, "cannot list linked devices for an info exchange");
                return;
            }
        };
        let exchanges = records
            .iter()
            .filter(|record| !record.revoked)
            .map(|record| async move {
                tokio::time::timeout(PROBE_ROUND_TIMEOUT, self.exchange_info(&record.fingerprint))
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or(false)
            });
        let changed = futures_util::future::join_all(exchanges)
            .await
            .into_iter()
            .any(|changed| changed);
        if changed {
            self.publish_devices().await;
        }
    }

    /// 有设备离线时自动短时 mDNS 浏览找回新地址（限频：每 [`HEAL_MIN_INTERVAL`] 至多一次；
    /// mDNS 被禁用时不做）。用户界面已开着发现时复用同一个浏览器。线索只影响之后的拨号；
    /// 浏览结束后立即补一轮探测，不必等下一个轮询周期。
    fn maybe_heal(&self) {
        if !self.network.mdns {
            return;
        }
        {
            let mut last = lock(&self.last_heal);
            if last.is_some_and(|at| at.elapsed() < HEAL_MIN_INTERVAL) {
                return;
            }
            *last = Some(Instant::now());
        }
        let Some(service) = self.weak.upgrade() else {
            return;
        };
        tokio::spawn(async move {
            let Ok(manager) = service.manager() else {
                return;
            };
            if let Err(error) = manager.browse_for(HEAL_BROWSE_DURATION).await {
                tracing::debug!(error = %error, "self-heal mDNS browse failed");
                return;
            }
            if let Err(error) = service.probe_all(false).await {
                tracing::debug!(error = %error, "post-browse probe round failed");
            }
        });
    }

    /// 兼容 API 是否应采信转发头（见 `ApiHost::link_trusted_proxy`）。
    #[must_use]
    pub fn trusted_proxy(&self, peer: IpAddr) -> bool {
        peer.is_loopback() || self.trusted_proxies.contains(&peer)
    }

    // ── agent.link.* ───────────────────────────────────────────────────────

    /// 配对码过期后收起 mDNS 广播（除非之后又生成了更新的码 / 用户已主动停止）。
    fn arm_advertising_timeout(self: &Arc<Self>, generation: u64) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(CODE_TTL_SECS)).await;
            if let Some(service) = weak.upgrade()
                && service.code_generation.load(Ordering::Acquire) == generation
                && let Ok(manager) = service.manager()
            {
                manager.stop_advertising();
            }
        });
    }

    /// 生成新配对码并安排过期后收起广播。**先**递增广播代数再生成码：代数在码存在之前就已前进，
    /// 之前码的过期任务不可能在新码刚生成的间隙里误关新码的广播。
    fn issue_code(self: &Arc<Self>, manager: &LinkManager) -> String {
        let generation = self.code_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let code = manager.generate_code();
        *lock(&self.current_code) = Some(code.clone());
        self.arm_advertising_timeout(generation);
        code
    }

    /// `agent.link.pairingCode`：生成配对码并（局域网可达时）开始 mDNS 广播。
    pub async fn pairing_code(self: &Arc<Self>) -> Result<LinkPairingCodeDto, LinkOpError> {
        let manager = self.manager()?;
        let code = self.issue_code(manager);
        Ok(LinkPairingCodeDto {
            code,
            expires_at_unix_ms: now_unix_ms()
                .saturating_add(i64::try_from(CODE_TTL_SECS * 1000).unwrap_or(120_000)),
            ttl_ms: CODE_TTL_SECS * 1000,
            addresses: self.advertised_base_urls(),
            fingerprint: manager.fingerprint().to_owned(),
            device_name: manager.self_name().to_owned(),
        })
    }

    /// `agent.link.stopPairing`：作废配对码并停止广播。带 `code` 时只有它仍是当前展示的码才作废
    /// （多个客户端互不干扰：一个客户端关闭自己的面板，不会作废另一个客户端刚刷新出来的新码）；
    /// 不带则无条件作废。
    pub fn stop_pairing(&self, code: Option<&str>) -> Result<(), LinkOpError> {
        let manager = self.manager()?;
        {
            let mut current = lock(&self.current_code);
            if let Some(code) = code
                && current.as_deref() != Some(code)
            {
                return Ok(());
            }
            *current = None;
        }
        self.code_generation.fetch_add(1, Ordering::AcqRel);
        manager.stop_pairing();
        Ok(())
    }

    /// `agent.link.discovery.set`。
    pub fn set_discovery(&self, enabled: bool) -> Result<(), LinkOpError> {
        let manager = self.manager()?;
        if enabled {
            manager.start_discovery()?;
        } else {
            manager.stop_discovery();
        }
        // 开启会清空发现快照、停止后也清空：UI 据此重置列表。
        self.publish_discovered();
        Ok(())
    }

    /// `agent.link.probe`。
    pub async fn probe(&self, address: &str) -> Result<LinkDiscoveredPeer, LinkOpError> {
        let address = PeerAddress::parse(address).map_err(|_| LinkOpError::Invalid("address"))?;
        Ok(discovered_dto(self.manager()?.probe(&address).await?))
    }

    /// `agent.link.pairBegin`。
    pub async fn pair_begin(
        &self,
        address: &str,
        code: &str,
        request_control: bool,
    ) -> Result<LinkPairBeginResponse, LinkOpError> {
        let address = PeerAddress::parse(address).map_err(|_| LinkOpError::Invalid("address"))?;
        let code = code.trim();
        if code.is_empty() {
            return Err(LinkOpError::Invalid("code"));
        }
        let result = self
            .manager()?
            .begin_pairing(&address, code, request_control)
            .await?;
        Ok(LinkPairBeginResponse {
            token: result.token,
            sas: result.sas,
            peer_name: result.peer_name,
            peer_fingerprint: result.peer_fingerprint,
        })
    }

    /// `agent.link.pairFinish`：发起端核对 SAS 后确认 / 放弃。放弃或对端拒绝 → `None`。
    pub async fn pair_finish(
        &self,
        token: &str,
        accept: bool,
    ) -> Result<Option<LinkDeviceInfo>, LinkOpError> {
        let manager = self.manager()?;
        let Some(record) = manager.confirm_pairing(token, accept).await? else {
            return Ok(None);
        };
        let online = manager.is_online(&record.fingerprint).await;
        lock(&self.online).insert(record.fingerprint.clone(), online);
        self.publish_devices().await;
        Ok(Some(super::storage::device_info(&record, online)))
    }

    /// `agent.link.approve`：响应端对入站配对请求的决定。
    pub fn approve(&self, session_id: &str, accept: bool) -> Result<(), LinkOpError> {
        let result = self.manager()?.approve_incoming(session_id, accept);
        // 无论决定是否成功送达（会话可能已过期），这条请求都不再需要 UI 处理。
        self.drop_requests(|request| request.session_id == session_id);
        Ok(result?)
    }

    /// `agent.link.remove`：对在线对端尽力通知（≤3s，失败不阻断）后本地删除。
    pub async fn remove(&self, fingerprint: &str) -> Result<(), LinkOpError> {
        let manager = self.manager()?;
        if !manager.remove_device(fingerprint).await? {
            return Err(LinkError::NotPaired.into());
        }
        lock(&self.online).remove(fingerprint);
        lock(&self.last_info_exchange).remove(fingerprint);
        self.publish_devices().await;
        Ok(())
    }

    /// `agent.link.rename`：设置 / 清除已配对设备的本机备注名。省略或空白 = 清除；非空须为
    /// 1–64 个字符且不含控制字符（否则 `InvalidArgument`，field=`name`）。
    pub async fn rename(
        &self,
        fingerprint: &str,
        name: Option<&str>,
    ) -> Result<LinkDeviceInfo, LinkOpError> {
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty())
            && (name.chars().count() > 64 || name.chars().any(char::is_control))
        {
            return Err(LinkOpError::Invalid("name"));
        }
        let record = self.manager()?.rename_device(fingerprint, name).await?;
        let online = lock(&self.online)
            .get(fingerprint)
            .copied()
            .unwrap_or(false);
        self.publish_devices().await;
        Ok(super::storage::device_info(&record, online))
    }

    /// `agent.link.refresh`：探测全部已配对设备在线状态并刷新对端信息，同时推送 `LinkedDevicesChanged`。
    pub async fn refresh(&self) -> Result<Vec<LinkDeviceInfo>, LinkOpError> {
        Ok(self.probe_all(true).await?)
    }

    /// 并发探测名册里全部（未被对端解除的）设备，结果**合并**进在线表而不是整表覆盖——本轮探测
    /// 期间刚完成配对的设备不会被抹成离线。刚从离线变回在线、久未交换（≥15 分钟）或
    /// `force_info` 时对在线设备交换设备信息。名册或在线状态变化才推送事件（`force_info` 时总是
    /// 推送）；有设备离线时触发自动 mDNS 浏览找回新地址（限频）。
    async fn probe_all(&self, force_info: bool) -> LinkResult<Vec<LinkDeviceInfo>> {
        let manager = self.manager()?;
        let records = manager.list_devices().await?;
        let before = self.devices().await;
        let previously_online = lock(&self.online).clone();
        let probes = records
            .iter()
            .filter(|record| !record.revoked)
            .map(|record| {
                let was_online = previously_online
                    .get(&record.fingerprint)
                    .copied()
                    .unwrap_or(false);
                async move {
                    // 超时只判定这一台设备离线，不连累同轮里已经探测成功的其他设备。
                    let probe = async {
                        let online = manager.is_online(&record.fingerprint).await;
                        if online
                            && (force_info
                                || !was_online
                                || self.info_exchange_due(&record.fingerprint))
                        {
                            self.exchange_info(&record.fingerprint).await;
                        }
                        online
                    };
                    let online = tokio::time::timeout(PROBE_ROUND_TIMEOUT, probe)
                        .await
                        .unwrap_or(false);
                    (record.fingerprint.clone(), online)
                }
            });
        let results = futures_util::future::join_all(probes).await;
        {
            let mut online = lock(&self.online);
            for (fingerprint, value) in &results {
                online.insert(fingerprint.clone(), *value);
            }
            for record in records.iter().filter(|record| record.revoked) {
                online.insert(record.fingerprint.clone(), false);
            }
        }
        let after = self.devices().await;
        if force_info || roster_signature(&before) != roster_signature(&after) {
            self.events
                .publish(AgentEvent::LinkedDevicesChanged(after.clone()));
        }
        if results.iter().any(|(_, online)| !online) {
            self.maybe_heal();
        }
        Ok(after)
    }

    /// `agent.link.dispatch`：把下载下发到已配对设备，返回对端新任务 ID。
    pub async fn dispatch(
        &self,
        fingerprint: &str,
        url: &str,
        file_name: Option<&str>,
        save_dir: Option<&str>,
    ) -> Result<String, LinkOpError> {
        let url = url.trim();
        if !valid_url(url) {
            return Err(LinkOpError::Invalid("url"));
        }
        let file_name = file_name.map(str::trim).filter(|name| !name.is_empty());
        if file_name.is_some_and(|name| !valid_file_name(name)) {
            return Err(LinkOpError::Invalid("fileName"));
        }
        let save_dir = save_dir.map(str::trim).filter(|dir| !dir.is_empty());
        if save_dir.is_some_and(|dir| !valid_save_dir(dir)) {
            return Err(LinkOpError::Invalid("saveDir"));
        }
        let manager = self.manager()?;
        let record = manager
            .get_device(fingerprint)
            .await?
            .ok_or(LinkError::NotPaired)?;
        // 对端已解除配对：不发请求，直接告知未配对。
        if record.revoked {
            return Err(LinkError::NotPaired.into());
        }
        if let Some(dir) = save_dir {
            // 目标路径风格未知（旧版对端）时不阻拦：由接收端按契约回退到它的默认目录。
            let style = record
                .path_style
                .as_deref()
                .and_then(|style| match style {
                    "windows" => Some(PathStyle::Windows),
                    "posix" => Some(PathStyle::Posix),
                    _ => None,
                })
                .or_else(|| {
                    record
                        .platform
                        .as_deref()
                        .and_then(PathStyle::from_platform)
                });
            if style.is_some_and(|style| !style.is_absolute(dir)) {
                return Err(LinkOpError::SaveDirUnavailable);
            }
        }
        Ok(manager
            .dispatch(fingerprint, url, save_dir, file_name)
            .await?)
    }

    /// `agent.link.*` RPC 分发：解析参数、调用对应操作、按稳定 `reason` 映射错误。
    pub async fn rpc(
        self: &Arc<Self>,
        method_name: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcErrorData> {
        fn parse<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, RpcErrorData> {
            serde_json::from_value(params.unwrap_or_else(|| json!({})))
                .map_err(|_| RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false))
        }
        let ok = |result: Result<(), LinkOpError>, context| {
            rpc_value(result.map(|()| json!({ "ok": true })), context)
        };
        match method_name {
            method::AGENT_LINK_PAIRING_CODE => {
                rpc_value(self.pairing_code().await, ErrorContext::Local)
            }
            method::AGENT_LINK_STOP_PAIRING => {
                let params: LinkStopPairingParams = parse(params)?;
                ok(
                    self.stop_pairing(params.code.as_deref()),
                    ErrorContext::Local,
                )
            }
            method::AGENT_LINK_DISCOVERY_SET => {
                let params: LinkDiscoveryParams = parse(params)?;
                ok(self.set_discovery(params.enabled), ErrorContext::Local)
            }
            method::AGENT_LINK_PROBE => {
                let params: LinkAddressParams = parse(params)?;
                rpc_value(self.probe(&params.address).await, ErrorContext::Pairing)
            }
            method::AGENT_LINK_PAIR_BEGIN => {
                let params: LinkPairBeginParams = parse(params)?;
                rpc_value(
                    self.pair_begin(&params.address, &params.code, params.request_control)
                        .await,
                    ErrorContext::Pairing,
                )
            }
            method::AGENT_LINK_PAIR_FINISH => {
                let params: LinkPairFinishParams = parse(params)?;
                rpc_value(
                    self.pair_finish(&params.token, params.accept)
                        .await
                        .map(|device| LinkPairFinishResponse {
                            paired: device.is_some(),
                            device,
                        }),
                    ErrorContext::Pairing,
                )
            }
            method::AGENT_LINK_APPROVE => {
                let params: LinkApproveParams = parse(params)?;
                ok(
                    self.approve(&params.session_id, params.accept),
                    ErrorContext::Pairing,
                )
            }
            method::AGENT_LINK_REMOVE => {
                let params: LinkDeviceParams = parse(params)?;
                ok(self.remove(&params.fingerprint).await, ErrorContext::Peer)
            }
            method::AGENT_LINK_RENAME => {
                let params: LinkRenameParams = parse(params)?;
                rpc_value(
                    self.rename(&params.fingerprint, params.name.as_deref())
                        .await,
                    ErrorContext::Peer,
                )
            }
            method::AGENT_LINK_REFRESH => rpc_value(self.refresh().await, ErrorContext::Local),
            method::AGENT_LINK_CONTROL_CREDENTIAL => {
                let params: LinkDeviceParams = parse(params)?;
                rpc_value(
                    self.control_credential(&params.fingerprint).await,
                    ErrorContext::Peer,
                )
            }
            method::AGENT_LINK_DISPATCH => {
                let params: LinkDispatchParams = parse(params)?;
                rpc_value(
                    self.dispatch(
                        &params.fingerprint,
                        &params.url,
                        params.file_name.as_deref(),
                        params.save_dir.as_deref(),
                    )
                    .await
                    .map(|task_id| LinkDispatchResult { task_id }),
                    ErrorContext::Peer,
                )
            }
            _ => Err(RpcErrorData::new(ApplicationErrorCode::Unsupported, false)),
        }
    }

    // ── 兼容 API（Flutter / CLI / 其它设备的 `/api/v1/link/*` 与 `/ping`）──────

    #[must_use]
    pub fn api_ping_info(&self) -> Option<LinkPingInfo> {
        let manager = self.manager.get()?;
        Some(LinkPingInfo {
            fingerprint: manager.fingerprint().to_owned(),
            name: manager.self_name(),
            platform: manager.self_platform().unwrap_or_default(),
            link_protocol: LINK_PROTOCOL_VERSION,
            link_control: LINK_CONTROL_VERSION,
        })
    }

    pub async fn api_pair_hello(
        &self,
        req: LinkPairHelloRequest,
        source: Option<IpAddr>,
    ) -> Result<LinkPairHelloResponse, ApiError> {
        let manager = self.api_manager()?;
        let wire = WireHello {
            protocol_version: req.protocol_version,
            code: req.code,
            initiator_id_pub: req.initiator_id_pub,
            initiator_commit: req.initiator_commit,
            initiator_sig: req.initiator_sig,
            name: req.name,
            platform: non_empty(req.platform),
            app_version: non_empty(req.app_version),
            initiator_addrs: req.initiator_addrs,
            request_control: req.request_control,
        };
        let resp = manager.pair_hello_wire(wire, source).map_err(api_error)?;
        Ok(LinkPairHelloResponse {
            protocol_version: resp.protocol_version,
            session_id: resp.session_id,
            responder_eph_pub: resp.responder_eph_pub,
            responder_nonce: resp.responder_nonce,
            responder_id_pub: resp.responder_id_pub,
            name: resp.name,
            platform: resp.platform.unwrap_or_default(),
            app_version: resp.app_version.unwrap_or_default(),
        })
    }

    pub async fn api_pair_reveal(
        &self,
        req: LinkPairRevealRequest,
    ) -> Result<LinkPairRevealResponse, ApiError> {
        let manager = self.api_manager()?;
        let resp = manager
            .pair_reveal_wire(WireReveal {
                session_id: req.session_id,
                initiator_eph_pub: req.initiator_eph_pub,
                initiator_nonce: req.initiator_nonce,
            })
            .map_err(api_error)?;
        Ok(LinkPairRevealResponse {
            responder_sig: resp.responder_sig,
        })
    }

    pub async fn api_pair_confirm(
        &self,
        req: LinkPairConfirmRequest,
    ) -> Result<LinkPairConfirmOutcome, ApiError> {
        let manager = self.api_manager()?;
        let outcome = manager
            .pair_confirm(&req.session_id, req.confirm)
            .await
            .map_err(api_error)?;
        // 终局（批准 / 拒绝 / 超时 / 发起方放弃）之后这条入站请求都不必再展示。
        self.drop_requests(|request| request.session_id == req.session_id);
        Ok(match outcome {
            PairConfirmOutcome::Paired => LinkPairConfirmOutcome::Paired,
            PairConfirmOutcome::Declined => LinkPairConfirmOutcome::Declined,
            PairConfirmOutcome::Rejected => LinkPairConfirmOutcome::Rejected,
            PairConfirmOutcome::TimedOut => LinkPairConfirmOutcome::TimedOut,
        })
    }

    pub fn api_approve(&self, session_id: &str, accept: bool) -> Result<(), ApiError> {
        self.approve(session_id, accept).map_err(op_api_error)
    }

    /// 校验一次入站数据面请求（v2）并返回已解密的明文与封装响应所需的上下文。
    async fn authorize_link(
        &self,
        path: &str,
        auth: &LinkAuth,
        body: &[u8],
    ) -> Result<fluxdown_link::LinkRequest, ApiError> {
        self.api_manager()?
            .authorize(
                "POST",
                path,
                &auth.device,
                auth.ts,
                &auth.nonce,
                body,
                &auth.tag,
                &auth.enc,
            )
            .await
            .map_err(data_plane_error)
    }

    /// 已配对设备下发下载任务（数据面 v2，链路 MAC 鉴权 + AEAD 解密）。成功返回封装好的
    /// `{"taskId":…}`——发起方只在响应解封成功后才认为对端确实是那台已配对设备。
    pub async fn api_create_task(
        &self,
        auth: LinkAuth,
        body: Vec<u8>,
    ) -> Result<Vec<u8>, ApiError> {
        let request = self.authorize_link(API_LINK_TASKS, &auth, &body).await?;
        let task: LinkTaskRequest = serde_json::from_slice(&request.body)
            .map_err(|error| ApiError::BadRequest(format!("invalid link task payload: {error}")))?;
        let url = task.url.trim();
        if !valid_url(url) {
            return Err(ApiError::BadRequest("invalid url".to_owned()));
        }
        let file_name = task.file_name.trim();
        if !file_name.is_empty() && !valid_file_name(file_name) {
            return Err(ApiError::BadRequest("invalid fileName".to_owned()));
        }
        if !valid_save_dir(&task.save_dir) {
            return Err(ApiError::BadRequest("invalid saveDir".to_owned()));
        }
        let task_id = self
            .create_local_task(url, file_name, resolve_receive_dir(&task.save_dir))
            .await?;
        Ok(request.seal_response(json!({ "taskId": task_id }).to_string().as_bytes()))
    }

    /// 接单建任务：带目录失败 → 去掉目录（使用本机默认目录）重试一次。
    async fn create_local_task(
        &self,
        url: &str,
        file_name: &str,
        save_dir: Option<String>,
    ) -> Result<String, ApiError> {
        let route = if save_dir.is_none() {
            self.events.inspect(|snapshot| {
                crate::category_dir::category_route(&snapshot.preferences.values)
            })
        } else {
            None
        };
        let first = build_task_request(url, file_name, save_dir.as_deref().unwrap_or_default())?;
        match self.tasks.create_task_with_route(first, route).await {
            Ok(task_id) => Ok(task_id),
            Err(error) if save_dir.is_some() => {
                tracing::warn!(
                    error = %error,
                    "link task rejected with the requested directory; retrying with the default directory"
                );
                let route = self.events.inspect(|snapshot| {
                    crate::category_dir::category_route(&snapshot.preferences.values)
                });
                self.tasks
                    .create_task_with_route(build_task_request(url, file_name, "")?, route)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    /// `POST /api/v1/link/info`：双向交换设备信息（数据面 v2，请求 / 响应均为密文）。请求方的
    /// 自报信息让名册变化时才推送给界面；信息没变化的周期性交换不产生任何事件。
    pub async fn api_peer_info(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        let request = self.authorize_link(API_LINK_INFO, &auth, &body).await?;
        let answer = self
            .api_manager()?
            .answer_peer_info(&request, &self.local_peer_info())
            .await
            .map_err(data_plane_error)?;
        if answer.changed {
            self.publish_devices().await;
        }
        Ok(answer.sealed)
    }

    /// `POST /api/v1/link/unpair`：对端通知「我已解除与你的配对」。本机把它标记为 `revoked`
    /// （事件泵随后推送 `LinkedDevicesChanged`），返回封装好的确认。
    pub async fn api_unpair(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        let request = self.authorize_link(API_LINK_UNPAIR, &auth, &body).await?;
        self.api_manager()?
            .answer_unpair(&request)
            .await
            .map_err(data_plane_error)
    }

    pub fn api_generate_code(self: &Arc<Self>) -> Result<LinkCodeResponse, ApiError> {
        let manager = self.api_manager()?;
        let code = self.issue_code(manager);
        Ok(LinkCodeResponse {
            code,
            ttl_seconds: i64::try_from(CODE_TTL_SECS).unwrap_or(120),
        })
    }

    pub fn api_stop_advertising(&self) -> Result<(), ApiError> {
        self.api_manager()?.stop_advertising();
        Ok(())
    }

    pub fn api_discovery(&self, start: bool) -> Result<(), ApiError> {
        self.api_manager()?;
        self.set_discovery(start).map_err(op_api_error)
    }

    pub fn api_discovered(&self) -> Result<Vec<LinkDiscoveredPeer>, ApiError> {
        Ok(self
            .api_manager()?
            .discovered_peers()
            .into_iter()
            .map(discovered_dto)
            .collect())
    }

    pub async fn api_probe(&self, host: &str, port: u16) -> Result<LinkDiscoveredPeer, ApiError> {
        let address = PeerAddress::from_host_port(host, port).map_err(api_error)?;
        self.api_manager()?
            .probe(&address)
            .await
            .map(discovered_dto)
            .map_err(api_error)
    }

    pub async fn api_pair_begin(
        &self,
        host: &str,
        port: u16,
        code: &str,
    ) -> Result<LinkPairBeginResponse, ApiError> {
        let address = PeerAddress::from_host_port(host, port).map_err(api_error)?;
        self.api_manager()?;
        // 管理面 REST 的配对不请求设备控制：控制授权只经 `agent.link.pairBegin` 显式发起。
        self.pair_begin(&address.to_candidate(), code, false)
            .await
            .map_err(op_api_error)
    }

    pub async fn api_pair_finish(
        &self,
        token: &str,
        accept: bool,
    ) -> Result<Option<LinkDeviceInfo>, ApiError> {
        self.api_manager()?;
        self.pair_finish(token, accept).await.map_err(op_api_error)
    }

    pub async fn api_devices(&self) -> Result<Vec<LinkDeviceInfo>, ApiError> {
        self.api_manager()?;
        self.refresh().await.map_err(op_api_error)
    }

    pub async fn api_remove_device(&self, fingerprint: &str) -> Result<bool, ApiError> {
        self.api_manager()?;
        match self.remove(fingerprint).await {
            Ok(()) => Ok(true),
            Err(LinkOpError::Link(LinkError::NotPaired)) => Ok(false),
            Err(error) => Err(op_api_error(error)),
        }
    }

    pub async fn api_dispatch(
        &self,
        fingerprint: &str,
        url: &str,
        save_dir: Option<&str>,
        file_name: Option<&str>,
    ) -> Result<String, ApiError> {
        self.api_manager()?;
        self.dispatch(fingerprint, url, file_name, save_dir)
            .await
            .map_err(op_api_error)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use fluxdown_protocol::{AgentSessionDto, CloudDevice};

    use super::*;

    #[test]
    fn advertise_override_entries_get_the_gateway_port_only_when_they_lack_one() {
        assert_eq!(with_default_port("192.168.1.5", 17800), "192.168.1.5:17800");
        assert_eq!(with_default_port("nas.local", 17800), "nas.local:17800");
        assert_eq!(
            with_default_port("192.168.1.5:8080", 17800),
            "192.168.1.5:8080"
        );
        assert_eq!(with_default_port("[fd00::5]:8080", 17800), "[fd00::5]:8080");
        assert_eq!(with_default_port("[fd00::5]", 17800), "[fd00::5]:17800");
        assert_eq!(with_default_port("fd00::5", 17800), "[fd00::5]:17800");
        assert_eq!(
            with_default_port("https://nas.example.com/fluxdown", 17800),
            "https://nas.example.com/fluxdown"
        );
        let bound: SocketAddr = "0.0.0.0:18000".parse().unwrap();
        assert_eq!(
            resolved_advertise_entries(&["10.0.0.9".to_owned(), "10.0.0.8:9".to_owned()], bound),
            vec!["10.0.0.9:18000".to_owned(), "10.0.0.8:9".to_owned()]
        );
    }

    #[test]
    fn only_session_and_cloud_device_events_can_change_the_link_identity() {
        let frame = |event: AgentEvent| EventFrame {
            epoch: "e".to_owned(),
            sequence: 1,
            previous: None,
            event: ServiceEvent::Agent(event),
        };
        assert!(affects_link_identity(&frame(AgentEvent::SessionChanged(
            Box::new(None::<AgentSessionDto>)
        ))));
        assert!(affects_link_identity(&frame(
            AgentEvent::CloudDevicesChanged(Vec::<CloudDevice>::new())
        )));
        assert!(!affects_link_identity(&frame(
            AgentEvent::LinkedDevicesChanged(Vec::new())
        )));
    }
}
