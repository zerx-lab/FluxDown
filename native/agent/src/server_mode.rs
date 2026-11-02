//! `fluxdown-agent --server`：headless（NAS / 服务器）宿主形态。
//!
//! 与桌面形态共用同一条 daemon 监管 / UI Gateway / `/rpc` 链路，差异只在这里：
//! - 监听地址只由 `FLUXDOWN_BIND` 决定（允许非回环，不看 `lan_enabled`）；
//! - 访问密钥 = `AgentState.gateway_user_token`，策略同旧 `validate_access_key`；
//! - 浏览器鉴权：`/rpc` 子协议携带密钥、Origin 同源校验、`/api/v1/setup*` 首次初始化；
//! - 浏览器文件面 `/api/web/*`：把上传 / 下载流式转发到 daemon 的 loopback HTTP；
//! - SPA 托管（内嵌或 `FLUXDOWN_WEBROOT`）与演示模式 `/demo/file`；
//! - 桌面专属集成（NMH、开机自启迁移、托盘 / 剪贴板）关闭；`agent.platform.*` 只对
//!   本机来源连接放行打开 / 定位任务产物（[`ServerHandle::local_platform_permitted`]）。
//!
//! 环境变量语义见 [`ServerConfig::from_lookup`]；`FLUXDOWN_DATA_DIR` /
//! `FLUXDOWN_DATABASE_URL` / `FLUXDOWN_SAVE_DIR` 不在这里解析，由 daemon 从继承的环境读取。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fluxdown_api::auth::{TokenCell, constant_time_eq};
use fluxdown_protocol::AgentEvent;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::capture::{BlobError, BlobKind, DaemonBlobClient};
use crate::daemon_client::DaemonClientConfig;
use crate::event_hub::AgentEventHub;
use crate::gateway::RpcCredential;
use crate::link::LinkService;
use crate::runtime::AgentResult;
use crate::state::{AgentState, StateStore};

/// `/rpc` 子协议：客户端必须提供，服务端回显。
pub const RPC_SUBPROTOCOL: &str = "fluxdown.rpc.v1";
/// `/rpc` 子协议里携带访问密钥的前缀：`fluxdown.token.<base64url 无 padding>`。
pub const TOKEN_SUBPROTOCOL_PREFIX: &str = "fluxdown.token.";
/// 访问密钥最短长度。
pub const ACCESS_KEY_MIN_LEN: usize = 8;
/// 访问密钥最长长度：既防误粘整段文本，也保证能原样塞进 HTTP 头。
pub const ACCESS_KEY_MAX_LEN: usize = 128;
/// 默认监听地址：IPv4 + IPv6 双栈（飞牛 DDNS / FN Connect 等走 IPv6 访问）；本机不支持 IPv6
/// 时网关退回 `0.0.0.0`（`runtime::bind_gateway_socket`）。
pub const DEFAULT_BIND: &str = "[::]:17800";
/// daemon `/blobs/*` 请求体上限（与 `fluxdown_daemon::http::REQUEST_BODY_LIMIT` 一致）。
const BLOB_UPLOAD_LIMIT: usize = 4 * 1024 * 1024;
/// 初始化请求体上限：只含一个密钥。
const SETUP_BODY_LIMIT: usize = 4 * 1024;
/// 首次设置 / 状态接口等待 daemon 就绪与 legacy 迁移完成的上限（略长于 daemon 就绪等待）。
const READY_WAIT: Duration = Duration::from_secs(35);

/// server 模式配置错误。
#[derive(Debug, thiserror::Error)]
pub enum ServerConfigError {
    #[error("FLUXDOWN_BIND is invalid: {0}")]
    InvalidBind(String),
}

/// 演示模式来源。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DemoMode {
    Off,
    /// `FLUXDOWN_DEMO` 真值：指向本进程 `/demo/file`。
    Builtin,
    /// `FLUXDOWN_DEMO_URL`：仅允许下载该 URL。
    Url(String),
}

/// server 模式进程级配置（全部来自环境变量）。
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    /// 通过 [`validate_access_key`] 的预置访问密钥（`FLUXDOWN_TOKEN`）。
    pub token: Option<String>,
    /// `FLUXDOWN_TOKEN_FORCE` 为真：每次启动都用 `token` 覆盖已存密钥。
    pub token_force: bool,
    /// SPA 磁盘托管目录；`None` = 用二进制内嵌的前端。
    pub webroot: Option<PathBuf>,
    /// `/ping` 的 `language` 回退值（`en` / `zh`）。
    pub language: Option<String>,
    pub demo: DemoMode,
    /// `FLUXDOWN_ALLOW_LOCAL_PLATFORM` 为真：所有已鉴权 `/rpc` 连接都可在服务器主机上打开 /
    /// 定位任务产物（显式越权开关，默认关）。
    pub allow_local_platform: bool,
}

impl ServerConfig {
    /// 从进程环境读取。
    pub fn from_env() -> Result<Self, ServerConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// 从任意键值来源读取（测试用注入）。
    ///
    /// | 变量 | 语义 |
    /// |---|---|
    /// | `FLUXDOWN_BIND` | 监听地址，缺省 `[::]:17800`（双栈，无 IPv6 时退回 `0.0.0.0`），允许非回环 |
    /// | `FLUXDOWN_TOKEN` / `FLUXDOWN_TOKEN_FORCE` | 预置访问密钥 / 是否每次启动覆盖；不合规则忽略并告警 |
    /// | `FLUXDOWN_WEBROOT` | 磁盘托管 SPA |
    /// | `FLUXDOWN_LANG` | `/ping` 语言回退，`zh-CN` → `zh` |
    /// | `FLUXDOWN_DEMO` / `FLUXDOWN_DEMO_URL` | 演示模式 |
    /// | `FLUXDOWN_ALLOW_LOCAL_PLATFORM` | 所有已鉴权连接都可在服务器主机上打开 / 定位任务产物 |
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ServerConfigError> {
        let bind_text = get("FLUXDOWN_BIND")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_BIND.to_owned());
        let bind = bind_text
            .parse::<SocketAddr>()
            .map_err(|error| ServerConfigError::InvalidBind(format!("{bind_text}: {error}")))?;

        let token = get("FLUXDOWN_TOKEN")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .and_then(|value| match validate_access_key(&value) {
                Ok(()) => Some(value),
                Err(reason) => {
                    tracing::warn!(
                        reason,
                        "FLUXDOWN_TOKEN does not meet the access key policy; ignored"
                    );
                    None
                }
            });
        let token_force = get("FLUXDOWN_TOKEN_FORCE").is_some_and(|value| flag_truthy(&value));
        let webroot = get("FLUXDOWN_WEBROOT")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty());
        let language = get("FLUXDOWN_LANG").and_then(|raw| {
            let language = parse_lang(&raw);
            if language.is_none() && !raw.trim().is_empty() {
                tracing::warn!(value = %raw, "FLUXDOWN_LANG is not recognized (supported: en, zh); ignored");
            }
            language
        });
        let demo = match get("FLUXDOWN_DEMO_URL").as_deref().and_then(parse_demo_url) {
            Some(url) => DemoMode::Url(url),
            None if get("FLUXDOWN_DEMO").is_some_and(|value| flag_truthy(&value)) => {
                DemoMode::Builtin
            }
            None => DemoMode::Off,
        };
        let allow_local_platform =
            get("FLUXDOWN_ALLOW_LOCAL_PLATFORM").is_some_and(|value| flag_truthy(&value));
        if allow_local_platform {
            tracing::warn!(
                "FLUXDOWN_ALLOW_LOCAL_PLATFORM is on: every authenticated client may open downloaded files on this host"
            );
        }
        Ok(Self {
            bind,
            token,
            token_force,
            webroot,
            language,
            demo,
            allow_local_platform,
        })
    }

    /// 生效的演示 URL：内置演示文件指向本进程实际监听端口的回环地址。
    #[must_use]
    pub fn effective_demo_url(&self, bound: SocketAddr) -> Option<String> {
        match &self.demo {
            DemoMode::Off => None,
            DemoMode::Builtin => Some(format!(
                "http://127.0.0.1:{}{}",
                bound.port(),
                crate::demo::DEMO_FILE_PATH
            )),
            DemoMode::Url(url) => Some(url.clone()),
        }
    }
}

/// 归一化 `FLUXDOWN_LANG`：剥首尾空白与包裹引号后取 BCP 47 主语言子标签
/// （`zh-CN` / `zh_TW` → `zh`，`en-US` → `en`，忽略大小写）；无法识别视为未设置。
fn parse_lang(raw: &str) -> Option<String> {
    let primary = strip_wrapping_quotes(raw)
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(primary.as_str(), "en" | "zh").then_some(primary)
}

/// 归一化 `FLUXDOWN_DEMO_URL`：去掉首尾空白与误带的包裹引号，为空视为未开启。
fn parse_demo_url(raw: &str) -> Option<String> {
    let value = strip_wrapping_quotes(raw);
    (!value.is_empty()).then(|| value.to_owned())
}

fn strip_wrapping_quotes(raw: &str) -> &str {
    let mut value = raw.trim();
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            value = value[1..value.len() - 1].trim();
        }
    }
    value
}

fn flag_truthy(value: &str) -> bool {
    matches!(
        value
            .trim()
            .trim_matches(['"', '\''])
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// 校验用户设定的访问密钥（网关用户 token）。
///
/// 规则与 Web 端 `web/src/lib/token-policy.ts` **逐条对齐**——两侧不一致会让
/// 前端放行、后端拒收，首次运行向导直接卡死。返回的 `Err` 是稳定英文 wire 契约。
pub fn validate_access_key(key: &str) -> Result<(), &'static str> {
    if !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err("access key must not contain spaces or non-ASCII characters");
    }
    if key.len() < ACCESS_KEY_MIN_LEN {
        return Err("access key must be at least 8 characters");
    }
    if key.len() > ACCESS_KEY_MAX_LEN {
        return Err("access key must be at most 128 characters");
    }
    if !key.bytes().any(|b| b.is_ascii_alphabetic()) || !key.bytes().any(|b| b.is_ascii_digit()) {
        return Err("access key must contain both letters and digits");
    }
    Ok(())
}

/// 预置访问密钥的采纳策略。
#[derive(Clone, Debug, Default)]
pub struct TokenSeed {
    pub preset: Option<String>,
    pub force: bool,
}

/// daemon 首次就绪并完成 legacy 迁移后应用 server 模式的状态引导；返回状态是否被改动。
///
/// - `fresh`（迁移前尚无网关迁移修订）且密钥仍为空：这是全新安装，开启兼容 API 分组
///   （takeover、jsonrpc、api、mcp）。密钥为空期间 takeover / jsonrpc 由兼容 API 的
///   `require_token` 门禁一律拒绝（api / mcp 本就拒绝空 token），首次设置完成后按开关生效。
///   迁移导入了旧 server 的密钥则保持其既有开关；
/// - 预置密钥：密钥为空时采纳；`force` 时每次启动都覆盖。
#[must_use]
pub fn apply_bootstrap(state: &mut AgentState, fresh: bool, seed: &TokenSeed) -> bool {
    let mut changed = false;
    if fresh && state.gateway_user_token.trim().is_empty() {
        let gateway = &mut state.gateway;
        for flag in [
            &mut gateway.takeover_enabled,
            &mut gateway.jsonrpc_enabled,
            &mut gateway.api_enabled,
            &mut gateway.mcp_enabled,
        ] {
            if !*flag {
                *flag = true;
                changed = true;
            }
        }
    }
    if let Some(preset) = seed.preset.as_deref() {
        let empty = state.gateway_user_token.trim().is_empty();
        if empty || (seed.force && state.gateway_user_token != preset) {
            state.gateway_user_token = preset.to_owned();
            state.gateway.user_token_configured = true;
            changed = true;
        }
    }
    changed
}

/// server 模式运行期参数（传给 `runtime::run_with`）。
pub struct ServerRuntime {
    pub config: ServerConfig,
    /// 收到 SIGTERM / SIGINT 时取消：runtime 走完全退出路径（先关停 daemon）。
    pub quit: CancellationToken,
}

/// 阻塞运行 server 模式直到退出。
pub fn run_blocking(host: crate::shell::ShellHost) -> AgentResult {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let fmt_layer = tracing_subscriber::fmt::layer().with_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
    );
    let subscriber = tracing_subscriber::registry().with(fmt_layer);
    if let Err(error) = subscriber.try_init() {
        // 嵌入式宿主可能已经设置订阅者；保留它，同时让 stderr 可观察未安装的原因。
        tracing::warn!(error = %error, "server tracing subscriber was not installed");
        eprintln!("server tracing subscriber was not installed: {error}");
    }
    let config = ServerConfig::from_env()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let cancel = CancellationToken::new();
        let quit = CancellationToken::new();
        let signal_cancel = cancel.clone();
        let signal_quit = quit.clone();
        tokio::spawn(async move {
            // 第一次信号：完全退出（关停 daemon）；再来一次：立刻强制退出 agent。
            crate::runtime::shutdown_signal().await;
            tracing::info!("shutdown signal received: stopping fluxdownd and fluxdown-agent");
            signal_quit.cancel();
            crate::runtime::shutdown_signal().await;
            tracing::warn!("second shutdown signal received: forcing exit");
            signal_cancel.cancel();
        });
        crate::runtime::run_with(cancel, host, Some(ServerRuntime { config, quit })).await
    })
}

// ---------------------------------------------------------------------------
// /rpc 浏览器鉴权
// ---------------------------------------------------------------------------

/// server 模式 `/rpc` 升级鉴权：Origin 同源（带 Origin 时）→ Bearer（agent.token 或访问密钥）
/// → 子协议携带的访问密钥。失败返回升级前的 HTTP 状态码。
pub fn authorize_rpc(
    headers: &HeaderMap,
    agent_bearer: &str,
    access_key: &TokenCell,
) -> Result<(), StatusCode> {
    if !origin_allowed(headers) {
        return Err(StatusCode::FORBIDDEN);
    }
    if let Some(presented) = bearer_token(headers)
        && (constant_time_eq(presented, agent_bearer) || key_matches(access_key, presented))
    {
        return Ok(());
    }
    if let Some(presented) = subprotocol_token(headers)
        && key_matches(access_key, &presented)
    {
        return Ok(());
    }
    Err(StatusCode::UNAUTHORIZED)
}

/// 空访问密钥永远不匹配（未初始化时不能用空串登录）。
pub(crate) fn key_matches(access_key: &TokenCell, presented: &str) -> bool {
    let expected = access_key.get();
    !expected.is_empty() && !presented.is_empty() && constant_time_eq(presented, &expected)
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// 客户端提供的全部子协议（多个头 / 逗号分隔均支持）。
fn offered_subprotocols(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// 解析 `fluxdown.rpc.v1` + `fluxdown.token.<base64url>`：两者都在才返回解码后的密钥。
fn subprotocol_token(headers: &HeaderMap) -> Option<String> {
    let mut has_rpc = false;
    let mut token = None;
    for protocol in offered_subprotocols(headers) {
        if protocol == RPC_SUBPROTOCOL {
            has_rpc = true;
        } else if let Some(encoded) = protocol.strip_prefix(TOKEN_SUBPROTOCOL_PREFIX) {
            token = URL_SAFE_NO_PAD
                .decode(encoded)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
        }
    }
    if has_rpc { token } else { None }
}

/// 防 CSWSH：带 `Origin` 时其主机必须等于 `Host`（反代场景也接受 `X-Forwarded-Host` 首段）的主机
/// ——浏览器无法为 WebSocket 握手设置这些头，所以跨站页面伪造不了它们。端口：头里带端口时必须
/// 等于 `Origin` 的有效端口；不带端口时只比主机，因为常见反代配置（nginx
/// `proxy_set_header Host $host`、Nginx Proxy Manager）会丢掉对外的非默认端口。
/// 无 `Origin`（非浏览器客户端）放行。
#[must_use]
pub fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some((scheme, authority)) = origin.to_str().ok().and_then(|o| o.split_once("://")) else {
        return false;
    };
    let default_port = if scheme.eq_ignore_ascii_case("https") {
        443
    } else if scheme.eq_ignore_ascii_case("http") {
        80
    } else {
        return false;
    };
    let Ok(origin) = authority
        .trim_end_matches('/')
        .parse::<axum::http::uri::Authority>()
    else {
        return false;
    };
    let origin_port = origin.port_u16().unwrap_or(default_port);
    let forwarded = HeaderName::from_static("x-forwarded-host");
    [header::HOST, forwarded].iter().any(|name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .and_then(|host| host.trim().parse::<axum::http::uri::Authority>().ok())
            .is_some_and(|host| {
                host.host().eq_ignore_ascii_case(origin.host())
                    && host.port_u16().is_none_or(|port| port == origin_port)
            })
    })
}

/// 请求是否来自字面本机：TCP 对端是环回；`X-Forwarded-For` / `X-Real-IP` / `Forwarded: for=`
/// 的每一跳都是环回 IP；`Host` / `Origin` / `X-Forwarded-Host` / `Forwarded: host=` 的每个
/// authority 都是 `localhost` 或环回 IP，且至少出现一个。
///
/// 同名头的所有实例都参与判定，任何无法解析的值都判否（fail closed）：同机反代虽从环回连入，
/// 但浏览器访问的是公网域名（`Origin` / `Host` / `X-Forwarded-Host` 暴露出来），不会被当成本机。
fn literal_local_origin(peer: IpAddr, headers: &HeaderMap) -> bool {
    if !is_loopback_ip(peer) {
        return false;
    }
    for name in ["x-forwarded-for", "x-real-ip"] {
        for value in headers.get_all(name) {
            let Ok(value) = value.to_str() else {
                return false;
            };
            if !value.split(',').all(|hop| parse_loopback_ip(hop.trim())) {
                return false;
            }
        }
    }
    let mut saw_authority = false;
    for value in headers.get_all(header::FORWARDED) {
        let Ok(value) = value.to_str() else {
            return false;
        };
        for pair in value.split([',', ';']) {
            let Some((key, raw)) = pair.split_once('=') else {
                continue;
            };
            let raw = raw.trim().trim_matches('"');
            let key = key.trim();
            if key.eq_ignore_ascii_case("for") {
                if !loopback_forwarded_node(raw) {
                    return false;
                }
            } else if key.eq_ignore_ascii_case("host") {
                saw_authority = true;
                if !loopback_authority(raw) {
                    return false;
                }
            }
        }
    }
    for (name, is_origin) in [
        (header::HOST, false),
        (header::ORIGIN, true),
        (HeaderName::from_static("x-forwarded-host"), false),
    ] {
        for value in headers.get_all(name) {
            let Ok(value) = value.to_str() else {
                return false;
            };
            for item in value.split(',') {
                saw_authority = true;
                let item = item.trim();
                let authority = if is_origin {
                    match item.split_once("://") {
                        Some((_, authority)) => authority.trim_end_matches('/'),
                        None => return false,
                    }
                } else {
                    item
                };
                if !loopback_authority(authority) {
                    return false;
                }
            }
        }
    }
    saw_authority
}

fn is_loopback_ip(ip: IpAddr) -> bool {
    ip.to_canonical().is_loopback()
}

fn parse_loopback_ip(text: &str) -> bool {
    text.parse::<IpAddr>().is_ok_and(is_loopback_ip)
}

/// RFC 7239 节点：`IPv4[:port]` 或 `"[IPv6][:port]"`；`unknown` / 混淆标识判否。
fn loopback_forwarded_node(node: &str) -> bool {
    let host = match node.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map(|(ip, _)| ip),
        None => Some(node.split_once(':').map_or(node, |(ip, _)| ip)),
    };
    host.is_some_and(parse_loopback_ip)
}

/// `host[:port]` 的主机是否为 `localhost`（允许 FQDN 末尾点）或环回 IP。
fn loopback_authority(value: &str) -> bool {
    value
        .parse::<axum::http::uri::Authority>()
        .is_ok_and(|authority| {
            let host = authority
                .host()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim_end_matches('.');
            host.eq_ignore_ascii_case("localhost") || parse_loopback_ip(host)
        })
}

/// 按来源地址的访问密钥失败节流：窗口内失败达到上限即锁定到窗口结束。
/// 回环来源不受限（本机反代会让所有外部请求都呈现为回环，限速只会变成全站锁定）。
#[derive(Default)]
struct AuthThrottle {
    entries: std::sync::Mutex<HashMap<IpAddr, ThrottleEntry>>,
}

struct ThrottleEntry {
    failures: u32,
    window_start: std::time::Instant,
}

const THROTTLE_MAX_FAILURES: u32 = 10;
const THROTTLE_WINDOW: Duration = Duration::from_secs(60);
const THROTTLE_MAX_TRACKED: usize = 4096;

impl AuthThrottle {
    /// IPv6 按 /64 归并，避免攻击者靠同一前缀内的地址轮换绕过。
    fn bucket(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V6(v6) => {
                let mut octets = v6.octets();
                octets[8..].fill(0);
                IpAddr::V6(Ipv6Addr::from(octets))
            }
            other => other,
        }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, ThrottleEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn is_locked(&self, peer: IpAddr, now: std::time::Instant) -> bool {
        if peer.is_loopback() {
            return false;
        }
        let mut entries = self.entries();
        let bucket = Self::bucket(peer);
        match entries.get(&bucket) {
            Some(entry) if now.duration_since(entry.window_start) >= THROTTLE_WINDOW => {
                entries.remove(&bucket);
                false
            }
            Some(entry) => entry.failures >= THROTTLE_MAX_FAILURES,
            None => false,
        }
    }

    fn record_failure(&self, peer: IpAddr, now: std::time::Instant) {
        if peer.is_loopback() {
            return;
        }
        let mut entries = self.entries();
        if entries.len() >= THROTTLE_MAX_TRACKED {
            entries.retain(|_, entry| now.duration_since(entry.window_start) < THROTTLE_WINDOW);
        }
        if entries.len() >= THROTTLE_MAX_TRACKED {
            return;
        }
        let entry = entries.entry(Self::bucket(peer)).or_insert(ThrottleEntry {
            failures: 0,
            window_start: now,
        });
        if now.duration_since(entry.window_start) >= THROTTLE_WINDOW {
            entry.failures = 0;
            entry.window_start = now;
        }
        entry.failures = entry.failures.saturating_add(1);
    }

    fn record_success(&self, peer: IpAddr) {
        self.entries().remove(&Self::bucket(peer));
    }
}

// ---------------------------------------------------------------------------
// 运行期句柄与 HTTP 路由
// ---------------------------------------------------------------------------

/// [`ServerHandle::new`] 的依赖。
pub struct ServerHandleParts {
    pub token: TokenCell,
    pub state: Arc<tokio::sync::Mutex<AgentState>>,
    pub store: Arc<StateStore>,
    pub events: AgentEventHub,
    /// daemon 就绪且迁移 / 引导完成后置 `true`。
    pub ready: tokio::sync::watch::Receiver<bool>,
    pub blobs: Arc<DaemonBlobClient>,
    pub daemon: DaemonClientConfig,
    pub webroot: Option<PathBuf>,
    pub demo: bool,
    /// [`ServerConfig::allow_local_platform`]。
    pub allow_local_platform: bool,
    /// 互联服务：接受局域网配对签发的设备控制凭据（`/rpc` 与 `/api/web/*`）。
    pub link: Option<Arc<LinkService>>,
    pub diagnostics: Arc<crate::diagnostics::DiagnosticsService>,
}

/// server 模式在 Gateway 中的共享句柄。
pub struct ServerHandle {
    token: TokenCell,
    state: Arc<tokio::sync::Mutex<AgentState>>,
    store: Arc<StateStore>,
    events: AgentEventHub,
    ready: tokio::sync::watch::Receiver<bool>,
    diagnostics: Arc<crate::diagnostics::DiagnosticsService>,
    blobs: Arc<DaemonBlobClient>,
    daemon_http: DaemonHttp,
    webroot: Option<PathBuf>,
    demo: bool,
    allow_local_platform: bool,
    link: Option<Arc<LinkService>>,
    throttle: AuthThrottle,
}

/// 首次设置结果。
#[derive(Debug, PartialEq, Eq)]
pub enum SetupOutcome {
    Done,
    Invalid(&'static str),
    AlreadyCompleted,
    Failed,
}

impl ServerHandle {
    pub fn new(parts: ServerHandleParts) -> Result<Self, BlobError> {
        Ok(Self {
            daemon_http: DaemonHttp::new(&parts.daemon)?,
            token: parts.token,
            state: parts.state,
            store: parts.store,
            events: parts.events,
            ready: parts.ready,
            diagnostics: parts.diagnostics,
            blobs: parts.blobs,
            webroot: parts.webroot,
            demo: parts.demo,
            allow_local_platform: parts.allow_local_platform,
            link: parts.link,
            throttle: AuthThrottle::default(),
        })
    }

    /// 访问密钥（与兼容 API 共享同一份可热替换状态）。
    #[must_use]
    pub fn access_key(&self) -> &TokenCell {
        &self.token
    }

    /// 本连接能否在服务器主机上打开 / 定位任务产物（`agent.platform.openTask` /
    /// `revealTask`）。运维显式开启 `FLUXDOWN_ALLOW_LOCAL_PLATFORM` 时所有已鉴权连接都放行；
    /// 否则只放行字面本机来源（见 [`literal_local_origin`]）。建连时判定一次，整条连接沿用。
    #[must_use]
    pub fn local_platform_permitted(&self, peer: IpAddr, headers: &HeaderMap) -> bool {
        self.allow_local_platform || literal_local_origin(peer, headers)
    }

    async fn wait_ready(&self) -> bool {
        let mut ready = self.ready.clone();
        tokio::time::timeout(READY_WAIT, ready.wait_for(|ready| *ready))
            .await
            .is_ok_and(|result| result.is_ok())
    }

    /// 首次设置：仅密钥为空时接受；落盘 → 热更新兼容 API 令牌 → 广播 `GatewayChanged`。
    pub async fn setup(&self, token: &str) -> SetupOutcome {
        let mut state = self.state.lock().await;
        if !state.gateway_user_token.trim().is_empty() {
            return SetupOutcome::AlreadyCompleted;
        }
        if let Err(reason) = validate_access_key(token) {
            return SetupOutcome::Invalid(reason);
        }
        state.gateway_user_token = token.to_owned();
        state.gateway.user_token_configured = true;
        if let Err(error) = self.store.save(&state).await {
            tracing::error!(error = %error, "could not persist the access key");
            state.gateway_user_token.clear();
            state.gateway.user_token_configured = false;
            return SetupOutcome::Failed;
        }
        let gateway = state.gateway.clone();
        drop(state);
        // 首次设置只发生在密钥为空时，而空密钥永不匹配：此刻没有以访问密钥认证的 `/rpc` 会话，
        // 不需要撤销任何会话（之后的改密钥走 `agent.gateway.patch`，会断开旧密钥的会话）。
        self.token.set(token);
        self.events.publish(AgentEvent::GatewayChanged(gateway));
        SetupOutcome::Done
    }

    /// 浏览器文件面（`/api/web/*`）鉴权：`Authorization: Bearer` 携带访问密钥或设备控制凭据，
    /// 或 `?token=<访问密钥>`（设备凭据只认 Bearer）；空密钥永远拒绝。三者共用同一份按来源节流：
    /// 同一来源连续失败过多后进入锁定（429），锁定期内连正确凭据也不受理。
    async fn browser_authorized(
        &self,
        peer: IpAddr,
        headers: &HeaderMap,
        query_token: Option<&str>,
    ) -> Result<(), StatusCode> {
        let bearer = bearer_token(headers);
        self.authorize(
            peer,
            bearer,
            bearer.is_some() || query_token.is_some(),
            |key| {
                [bearer, query_token]
                    .into_iter()
                    .flatten()
                    .find(|presented| key_matches(key, presented))
                    .map(|presented| RpcCredential::AccessKey(presented.to_owned()))
            },
        )
        .await
        .map(|_| ())
    }

    /// `/rpc` 升级鉴权（带按来源失败节流）；网关应在有对端地址时用它取代 [`authorize_rpc`]。
    ///
    /// 接受的凭据：`agent.token`、访问密钥（`Authorization: Bearer` 或子协议携带），以及互联服务存在时
    /// 的设备控制凭据（仅 `Authorization: Bearer`，由局域网配对签发，见
    /// [`LinkService::verify_control_credential`]）。三者失败都计入同一份按来源节流。返回通过的
    /// 凭据种类，会话据此在撤销事件之后重新校验。
    pub(crate) async fn authorize_rpc_from(
        &self,
        peer: IpAddr,
        headers: &HeaderMap,
        agent_bearer: &str,
    ) -> Result<RpcCredential, StatusCode> {
        if !origin_allowed(headers) {
            return Err(StatusCode::FORBIDDEN);
        }
        let bearer = bearer_token(headers);
        let presented = bearer
            .map(str::to_owned)
            .or_else(|| subprotocol_token(headers));
        self.authorize(peer, bearer, presented.is_some(), |key| {
            let presented = presented.as_deref()?;
            if constant_time_eq(presented, agent_bearer) {
                Some(RpcCredential::Agent)
            } else if key_matches(key, presented) {
                Some(RpcCredential::AccessKey(presented.to_owned()))
            } else {
                None
            }
        })
        .await
    }

    /// 节流 + 凭据判定的共同核心：先看来源是否被锁定，再依次试静态凭据（`agent.token` / 访问
    /// 密钥，由 `static_credential` 判定）与设备控制凭据（`device_bearer`）；成功清空该来源的失败
    /// 记录，失败且确实出示过凭据才计一次失败。
    async fn authorize(
        &self,
        peer: IpAddr,
        device_bearer: Option<&str>,
        credential_presented: bool,
        static_credential: impl FnOnce(&TokenCell) -> Option<RpcCredential>,
    ) -> Result<RpcCredential, StatusCode> {
        let now = std::time::Instant::now();
        if self.throttle.is_locked(peer, now) {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        let credential = match static_credential(&self.token) {
            Some(credential) => Some(credential),
            None => self.device_credential(device_bearer).await,
        };
        match credential {
            Some(credential) => {
                self.throttle.record_success(peer);
                Ok(credential)
            }
            None => {
                if credential_presented {
                    self.throttle.record_failure(peer, now);
                }
                Err(StatusCode::UNAUTHORIZED)
            }
        }
    }

    /// 设备控制凭据：互联服务存在且按名册校验通过（授权控制本机、未撤销、HMAC 一致）。
    async fn device_credential(&self, bearer: Option<&str>) -> Option<RpcCredential> {
        let token = bearer?;
        let fingerprint = self.link.as_ref()?.verify_control_credential(token).await?;
        Some(RpcCredential::Device {
            token: token.to_owned(),
            fingerprint,
        })
    }
}

/// daemon loopback HTTP 的流式转发客户端（无整体超时：大文件下载可以很久）。
/// 鉴权与目标地址取自已认证 WebSocket 会话，见 `DaemonClientConfig::http_session`。
struct DaemonHttp {
    session: crate::daemon_client::HttpSession,
    client: reqwest::Client,
}

impl DaemonHttp {
    fn new(config: &DaemonClientConfig) -> Result<Self, BlobError> {
        // 只连本机回环 daemon：跳过系统根证书加载（见 `DaemonBlobClient::new`）。
        let client = reqwest::Client::builder()
            .no_proxy()
            .tls_built_in_root_certs(false)
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            session: config.http_session(),
            client,
        })
    }

    /// 转发 `GET <daemon>/<segments...>`，响应体流式回写；上游非 2xx 映射为对应错误状态。
    /// `Content-Type` / `Content-Length` / `Content-Disposition` 默认取上游，`forced_headers`
    /// 中列出的头以调用方的值为准。
    async fn forward(
        &self,
        segments: &[&str],
        forced_headers: &[(HeaderName, &'static str)],
    ) -> Response {
        if segments
            .iter()
            .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
        {
            return StatusCode::NOT_FOUND.into_response();
        }
        // 没有已认证的 daemon 会话时 daemon 视为不可用，绝不退回长期 token。
        let Some(access) = self.session.access() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let mut url = access.base_url;
        match url.path_segments_mut() {
            Ok(mut path) => {
                path.clear().extend(segments);
            }
            Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
        let upstream = match self
            .client
            .get(url)
            .bearer_auth(access.credential)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(error = %error, "daemon file proxy request failed");
                return StatusCode::BAD_GATEWAY.into_response();
            }
        };
        let status = upstream.status();
        if !status.is_success() {
            return match status.as_u16() {
                404 => StatusCode::NOT_FOUND,
                409 => StatusCode::CONFLICT,
                _ => StatusCode::BAD_GATEWAY,
            }
            .into_response();
        }
        let mut builder = Response::builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "no-store")
            .header(header::REFERRER_POLICY, "no-referrer")
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
        let forced = |name: &HeaderName| {
            forced_headers
                .iter()
                .find(|(forced, _)| forced == name)
                .map(|(_, value)| HeaderValue::from_static(value))
        };
        for name in [
            header::CONTENT_TYPE,
            header::CONTENT_LENGTH,
            header::CONTENT_DISPOSITION,
        ] {
            if let Some(value) = forced(&name).or_else(|| upstream.headers().get(&name).cloned()) {
                builder = builder.header(name, value);
            }
        }
        builder
            .body(Body::from_stream(upstream.bytes_stream()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// server 模式全部 HTTP 路由（含 SPA fallback）；合并进 Gateway 的 API / `/rpc` 路由。
pub fn router(handle: Arc<ServerHandle>) -> Router {
    let webroot = handle.webroot.clone();
    let demo = handle.demo;
    let mut router = Router::new()
        .route("/api/v1/setup/status", get(setup_status))
        .route(
            "/api/v1/setup",
            post(setup_submit).layer(DefaultBodyLimit::max(SETUP_BODY_LIMIT)),
        )
        .route("/api/web/blobs/torrents", post(upload_torrent))
        .route("/api/web/blobs/plugins", post(upload_plugin))
        .route("/api/web/files/tasks/{task_id}", get(download_task_file))
        .route("/api/web/exports/{export_id}", get(download_export))
        .route("/api/web/logs/export", get(download_logs))
        .with_state(handle);
    if demo {
        router = router.merge(crate::demo::demo_router());
    }
    router.fallback(move |method: Method, uri: Uri, headers: HeaderMap| {
        let webroot = webroot.clone();
        async move {
            let response = match webroot {
                Some(root) => crate::web_assets::disk_handler(root, method, uri).await,
                None => crate::web_assets::handler(method, uri, headers).await,
            };
            crate::web_assets::with_security_headers(response)
        }
    })
}

fn json_response(status: StatusCode, value: serde_json::Value) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        axum::Json(value),
    )
        .into_response()
}

fn starting_response() -> Response {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({ "error": "fluxdown-agent is still starting" }),
    )
}

async fn setup_status(State(handle): State<Arc<ServerHandle>>) -> Response {
    if !handle.wait_ready().await {
        return starting_response();
    }
    json_response(
        StatusCode::OK,
        serde_json::json!({
            "setupRequired": handle.token.is_empty(),
            "minLength": ACCESS_KEY_MIN_LEN,
        }),
    )
}

#[derive(Deserialize)]
struct SetupBody {
    token: String,
}

async fn setup_submit(
    State(handle): State<Arc<ServerHandle>>,
    body: axum::body::Bytes,
) -> Response {
    if !handle.wait_ready().await {
        return starting_response();
    }
    // 已完成初始化的判定先于请求体校验：不向匿名调用者泄露多余信息。
    if !handle.token.is_empty() {
        return json_response(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": "setup already completed" }),
        );
    }
    let Ok(body) = serde_json::from_slice::<SetupBody>(&body) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "invalid request body" }),
        );
    };
    match handle.setup(&body.token).await {
        SetupOutcome::Done => json_response(StatusCode::OK, serde_json::json!({ "ok": true })),
        SetupOutcome::Invalid(reason) => json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": reason }),
        ),
        SetupOutcome::AlreadyCompleted => json_response(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": "setup already completed" }),
        ),
        SetupOutcome::Failed => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": "could not persist the access key" }),
        ),
    }
}

async fn upload_torrent(
    State(handle): State<Arc<ServerHandle>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    upload_blob(
        &handle,
        BlobKind::Torrent,
        addr.ip(),
        &headers,
        query.token.as_deref(),
        body,
    )
    .await
}

async fn upload_plugin(
    State(handle): State<Arc<ServerHandle>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    upload_blob(
        &handle,
        BlobKind::Plugin,
        addr.ip(),
        &headers,
        query.token.as_deref(),
        body,
    )
    .await
}

async fn upload_blob(
    handle: &ServerHandle,
    kind: BlobKind,
    peer: IpAddr,
    headers: &HeaderMap,
    query_token: Option<&str>,
    body: Body,
) -> Response {
    // 先鉴权再读体：未授权请求不会让服务器缓冲 4 MiB。
    if let Err(status) = handle.browser_authorized(peer, headers, query_token).await {
        return status.into_response();
    }
    let Ok(bytes) = axum::body::to_bytes(body, BLOB_UPLOAD_LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    match handle.blobs.upload(kind, bytes.to_vec()).await {
        Ok(blob_id) => json_response(StatusCode::OK, serde_json::json!({ "blobId": blob_id })),
        Err(BlobError::Status(413)) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(error) => {
            tracing::warn!(error = %error, "daemon blob upload failed");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

async fn download_task_file(
    State(handle): State<Arc<ServerHandle>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    AxumPath(task_id): AxumPath<String>,
) -> Response {
    if let Err(status) = handle
        .browser_authorized(addr.ip(), &headers, query.token.as_deref())
        .await
    {
        return status.into_response();
    }
    // 文件名 / 长度 / 类型都由 daemon 给出（含 RFC 5987 `filename*`），原样透传。
    handle
        .daemon_http
        .forward(&["files", "tasks", &task_id], &[])
        .await
}

async fn download_export(
    State(handle): State<Arc<ServerHandle>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    AxumPath(export_id): AxumPath<String>,
) -> Response {
    if let Err(status) = handle
        .browser_authorized(addr.ip(), &headers, query.token.as_deref())
        .await
    {
        return status.into_response();
    }
    // daemon 的日志导出是其诊断快照（JSON），响应头不带文件名：这里补上下载语义。
    handle
        .daemon_http
        .forward(
            &["exports", &export_id],
            &[
                (header::CONTENT_TYPE, "application/json"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"fluxdown-diagnostics.json\"",
                ),
            ],
        )
        .await
}

/// agent + daemon 日志 zip：复用 `agent.diagnostics.exportLogs` 的打包逻辑，
/// 写到数据目录下的临时文件，读出后立即删除。
async fn download_logs(
    State(handle): State<Arc<ServerHandle>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = handle
        .browser_authorized(addr.ip(), &headers, query.token.as_deref())
        .await
    {
        return status.into_response();
    }
    let temp = handle
        .store
        .data_dir()
        .join(format!("log-export-{}.zip", uuid::Uuid::new_v4()));
    let params = fluxdown_protocol::LogExportParams {
        target_path: temp.display().to_string(),
    };
    let exported = handle.diagnostics.export_logs(&params).await;
    let bytes = match exported {
        Ok(_) => tokio::fs::read(&temp).await,
        Err(error) => {
            tracing::warn!(error = %error, "log export failed");
            // 导出可能在创建归档前失败，NotFound 是预期清理结果。
            if let Err(error) = tokio::fs::remove_file(&temp).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %temp.display(), error = %error, "could not remove failed log export archive");
            }
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if let Err(error) = tokio::fs::remove_file(&temp).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        // 返回已读出的归档仍然有效，删除只负责 best-effort 临时文件清理。
        tracing::warn!(path = %temp.display(), error = %error, "could not remove log export archive");
    }
    match bytes {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/zip"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"fluxdown_logs.zip\"",
                ),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(error = %error, "could not read exported log archive");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// 启动日志：Web UI 的来源（磁盘覆盖 / 内嵌 / 未嵌入）。
pub fn log_web_ui(config: &ServerConfig) {
    if let Some(root) = &config.webroot {
        tracing::info!(webroot = %root.display(), "serving Web UI from FLUXDOWN_WEBROOT");
    } else if crate::web_assets::is_embedded() {
        let (files, bytes) = crate::web_assets::stats();
        tracing::info!(files, bytes, "serving embedded Web UI");
    } else {
        tracing::warn!(
            "Web UI is not embedded (build with --features web-ui after `bun run build` in web/, or set FLUXDOWN_WEBROOT); frontend requests answer 503"
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashMap;

    use fluxdown_protocol::AgentSnapshot;

    use super::*;

    fn config_from(pairs: &[(&str, &str)]) -> Result<ServerConfig, ServerConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        ServerConfig::from_lookup(|key| map.get(key).cloned())
    }

    #[test]
    fn access_key_policy_matches_web_token_policy() {
        for key in ["flux2026", "fxd_1a2b3c4d", "Aa1!@#$%^&*()"] {
            assert_eq!(validate_access_key(key), Ok(()), "{key:?}");
        }
        assert_eq!(
            validate_access_key("abc123"),
            Err("access key must be at least 8 characters")
        );
        assert_eq!(
            validate_access_key(""),
            Err("access key must be at least 8 characters")
        );
        let long = format!("a1{}", "x".repeat(ACCESS_KEY_MAX_LEN));
        assert_eq!(
            validate_access_key(&long),
            Err("access key must be at most 128 characters")
        );
        let mixed = Err("access key must contain both letters and digits");
        assert_eq!(validate_access_key("allletters"), mixed);
        assert_eq!(validate_access_key("1234567890"), mixed);
        assert_eq!(validate_access_key("!@#$%^&*"), mixed);
        let graphic = Err("access key must not contain spaces or non-ASCII characters");
        assert_eq!(validate_access_key("flux 2026"), graphic);
        assert_eq!(validate_access_key("flux2026\n"), graphic);
        assert_eq!(validate_access_key("密钥12345678"), graphic);
    }

    #[test]
    fn bind_defaults_to_all_interfaces_and_accepts_non_loopback() {
        assert_eq!(config_from(&[]).unwrap().bind.to_string(), "[::]:17800");
        assert_eq!(
            config_from(&[("FLUXDOWN_BIND", " 192.168.1.10:9000 ")])
                .unwrap()
                .bind
                .to_string(),
            "192.168.1.10:9000"
        );
        assert_eq!(
            config_from(&[("FLUXDOWN_BIND", "[::]:17800")])
                .unwrap()
                .bind
                .port(),
            17800
        );
        assert!(config_from(&[("FLUXDOWN_BIND", "not-an-address")]).is_err());
        assert!(config_from(&[("FLUXDOWN_BIND", "0.0.0.0")]).is_err());
    }

    #[test]
    fn preset_token_must_pass_policy_and_force_is_truthy_flag() {
        let ok = config_from(&[
            ("FLUXDOWN_TOKEN", " flux2026 "),
            ("FLUXDOWN_TOKEN_FORCE", "TRUE"),
        ])
        .unwrap();
        assert_eq!(ok.token.as_deref(), Some("flux2026"));
        assert!(ok.token_force);
        let weak = config_from(&[("FLUXDOWN_TOKEN", "short")]).unwrap();
        assert_eq!(weak.token, None);
        assert!(!weak.token_force);
    }

    #[test]
    fn language_demo_and_webroot_are_normalized() {
        let config = config_from(&[
            ("FLUXDOWN_LANG", " \"zh-CN\" "),
            ("FLUXDOWN_DEMO_URL", "'https://example.com/demo.bin' "),
            ("FLUXDOWN_WEBROOT", "/srv/web"),
        ])
        .unwrap();
        assert_eq!(config.language.as_deref(), Some("zh"));
        assert_eq!(
            config.demo,
            DemoMode::Url("https://example.com/demo.bin".to_owned())
        );
        assert_eq!(config.webroot, Some(PathBuf::from("/srv/web")));

        let defaults = config_from(&[("FLUXDOWN_LANG", "fr")]).unwrap();
        assert_eq!(defaults.language, None);
        assert_eq!(defaults.demo, DemoMode::Off);
    }

    #[test]
    fn allow_local_platform_is_an_explicit_truthy_flag() {
        assert!(!config_from(&[]).unwrap().allow_local_platform);
        assert!(
            !config_from(&[("FLUXDOWN_ALLOW_LOCAL_PLATFORM", "0")])
                .unwrap()
                .allow_local_platform
        );
        assert!(
            config_from(&[("FLUXDOWN_ALLOW_LOCAL_PLATFORM", " On ")])
                .unwrap()
                .allow_local_platform
        );
    }

    fn header_map(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn literal_local_origin_accepts_only_loopback_peers_hops_and_authorities() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let local = |pairs: &[(&'static str, &'static str)]| {
            literal_local_origin(loopback, &header_map(pairs))
        };

        assert!(local(&[
            ("host", "localhost:17800"),
            ("origin", "http://localhost:17800"),
        ]));
        assert!(literal_local_origin(
            "::1".parse().unwrap(),
            &header_map(&[("host", "[::1]:17800")])
        ));
        // 双栈监听下本机 IPv4 连接呈现为 IPv4 映射地址。
        assert!(literal_local_origin(
            "::ffff:127.0.0.1".parse().unwrap(),
            &header_map(&[("host", "[::ffff:127.0.0.1]:17800")])
        ));
        assert!(local(&[("host", "localhost.:17800")]));
        assert!(local(&[
            ("host", "localhost:17800"),
            ("x-forwarded-for", "127.0.0.1, ::1"),
        ]));
        assert!(local(&[
            ("host", "localhost:17800"),
            (
                "forwarded",
                "for=\"[::1]:1234\", for=127.0.0.1:80;proto=http"
            ),
        ]));
        // 只有 `by=` / `proto=` 的 Forwarded 不提供来源地址，结果由 Host 决定。
        assert!(local(&[
            ("host", "localhost:17800"),
            ("forwarded", "by=127.0.0.1;proto=http"),
        ]));
        assert!(!local(&[
            ("host", "downloads.example.com"),
            ("forwarded", "by=127.0.0.1"),
        ]));

        assert!(!literal_local_origin(
            "192.0.2.10".parse().unwrap(),
            &header_map(&[("host", "localhost:17800")])
        ));
        assert!(!local(&[]), "an authority must be present");
        assert!(!local(&[
            ("host", "127.0.0.1:17800"),
            ("x-forwarded-for", "192.0.2.10"),
        ]));
        assert!(!local(&[
            ("host", "127.0.0.1:17800"),
            ("x-real-ip", "unknown")
        ]));
        // 同机反代对外提供公网域名：Origin / X-Forwarded-Host 暴露真实访问地址。
        assert!(!local(&[
            ("host", "127.0.0.1:17800"),
            ("origin", "https://downloads.example.com"),
            ("x-forwarded-host", "downloads.example.com"),
        ]));
        assert!(!local(&[("host", "localhost:17800"), ("origin", "null")]));
        // 同名头的每个实例都参与判定。
        assert!(!local(&[
            ("host", "localhost:17800"),
            ("host", "downloads.example.com"),
        ]));
        assert!(!local(&[
            ("host", "localhost:17800"),
            ("x-forwarded-for", "127.0.0.1"),
            ("x-forwarded-for", "192.0.2.10"),
        ]));
        for forwarded in [
            "for=192.0.2.10",
            "for=\"[2001:db8::1]\"",
            "for=_gazonk",
            "for=unknown",
            "for=127.0.0.1;host=downloads.example.com",
        ] {
            assert!(
                !local(&[("host", "localhost:17800"), ("forwarded", forwarded)]),
                "{forwarded}"
            );
        }
    }

    #[test]
    fn builtin_demo_url_follows_the_bound_port() {
        let config = config_from(&[("FLUXDOWN_DEMO", "1")]).unwrap();
        assert_eq!(config.demo, DemoMode::Builtin);
        let bound: SocketAddr = "0.0.0.0:9000".parse().unwrap();
        assert_eq!(
            config.effective_demo_url(bound).as_deref(),
            Some("http://127.0.0.1:9000/demo/file")
        );
        assert_eq!(config_from(&[]).unwrap().effective_demo_url(bound), None);
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn token_protocol(token: &str) -> String {
        format!(
            "{TOKEN_SUBPROTOCOL_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(token)
        )
    }

    #[test]
    fn subprotocol_token_needs_rpc_marker_and_valid_base64url() {
        let good = format!("{RPC_SUBPROTOCOL}, {}", token_protocol("Fl/ux+2026=="));
        assert_eq!(
            subprotocol_token(&headers(&[("sec-websocket-protocol", &good)])).as_deref(),
            Some("Fl/ux+2026==")
        );
        // 多个头行与逗号分隔等价。
        let split = headers(&[
            ("sec-websocket-protocol", RPC_SUBPROTOCOL),
            ("sec-websocket-protocol", &token_protocol("flux2026")),
        ]);
        assert_eq!(subprotocol_token(&split).as_deref(), Some("flux2026"));
        // 缺 rpc 标记、缺 token、非法 base64 一律无效。
        let no_rpc = token_protocol("flux2026");
        assert_eq!(
            subprotocol_token(&headers(&[("sec-websocket-protocol", &no_rpc)])),
            None
        );
        assert_eq!(
            subprotocol_token(&headers(&[("sec-websocket-protocol", RPC_SUBPROTOCOL)])),
            None
        );
        let bad = format!("{RPC_SUBPROTOCOL}, {TOKEN_SUBPROTOCOL_PREFIX}!!!");
        assert_eq!(
            subprotocol_token(&headers(&[("sec-websocket-protocol", &bad)])),
            None
        );
    }

    #[test]
    fn rpc_auth_accepts_agent_bearer_access_key_bearer_and_subprotocol_only() {
        let key = TokenCell::new("flux2026");
        let agent = "agent-secret";
        let bearer = |value: &str| headers(&[("authorization", &format!("Bearer {value}"))]);
        assert_eq!(authorize_rpc(&bearer(agent), agent, &key), Ok(()));
        assert_eq!(authorize_rpc(&bearer("flux2026"), agent, &key), Ok(()));
        assert_eq!(
            authorize_rpc(&bearer("wrong"), agent, &key),
            Err(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            authorize_rpc(&HeaderMap::new(), agent, &key),
            Err(StatusCode::UNAUTHORIZED)
        );
        let protocols = format!("{RPC_SUBPROTOCOL}, {}", token_protocol("flux2026"));
        assert_eq!(
            authorize_rpc(
                &headers(&[("sec-websocket-protocol", &protocols)]),
                agent,
                &key
            ),
            Ok(())
        );
        let wrong = format!("{RPC_SUBPROTOCOL}, {}", token_protocol("flux2027"));
        assert_eq!(
            authorize_rpc(&headers(&[("sec-websocket-protocol", &wrong)]), agent, &key),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn empty_access_key_never_authenticates() {
        let key = TokenCell::new("");
        let empty = format!("{RPC_SUBPROTOCOL}, {TOKEN_SUBPROTOCOL_PREFIX}");
        assert_eq!(
            authorize_rpc(
                &headers(&[("sec-websocket-protocol", &empty)]),
                "agent",
                &key
            ),
            Err(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            authorize_rpc(&headers(&[("authorization", "Bearer ")]), "agent", &key),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn origin_must_match_host_when_present() {
        assert!(origin_allowed(&HeaderMap::new()));
        let same = headers(&[
            ("origin", "http://nas.local:17800"),
            ("host", "nas.local:17800"),
        ]);
        assert!(origin_allowed(&same));
        let case = headers(&[("origin", "https://NAS.local"), ("host", "nas.LOCAL")]);
        assert!(origin_allowed(&case));
        let cross = headers(&[
            ("origin", "https://evil.example"),
            ("host", "nas.local:17800"),
        ]);
        assert!(!origin_allowed(&cross));
        let port = headers(&[
            ("origin", "http://nas.local:1"),
            ("host", "nas.local:17800"),
        ]);
        assert!(!origin_allowed(&port));
        let null = headers(&[("origin", "null"), ("host", "nas.local")]);
        assert!(!origin_allowed(&null));
        let no_host = headers(&[("origin", "http://nas.local")]);
        assert!(!origin_allowed(&no_host));
        let proxied = headers(&[
            ("origin", "https://nas.example:8443"),
            ("host", "127.0.0.1:17800"),
            ("x-forwarded-host", "nas.example:8443"),
        ]);
        assert!(origin_allowed(&proxied));
        // nginx `Host $host` / Nginx Proxy Manager 丢掉对外的非默认端口：只比主机。
        let stripped = headers(&[
            ("origin", "https://nas.example:18800"),
            ("host", "nas.example"),
        ]);
        assert!(origin_allowed(&stripped));
        let stripped_forwarded = headers(&[
            ("origin", "https://nas.example:18800"),
            ("host", "127.0.0.1:17800"),
            ("x-forwarded-host", "nas.example"),
        ]);
        assert!(origin_allowed(&stripped_forwarded));
        // 头里显式带端口时仍按 Origin 的有效端口（缺省 = 协议默认端口）比较。
        let explicit_mismatch = headers(&[
            ("origin", "https://nas.example"),
            ("host", "nas.example:17800"),
        ]);
        assert!(!origin_allowed(&explicit_mismatch));
        let default_port = headers(&[
            ("origin", "https://nas.example"),
            ("host", "nas.example:443"),
        ]);
        assert!(origin_allowed(&default_port));
        let stripped_cross = headers(&[
            ("origin", "https://evil.example:18800"),
            ("host", "nas.example"),
        ]);
        assert!(!origin_allowed(&stripped_cross));
        let ipv6 = headers(&[("origin", "http://[::1]:17800"), ("host", "[::1]:17800")]);
        assert!(origin_allowed(&ipv6));
        let extension = headers(&[("origin", "chrome-extension://abcdef"), ("host", "abcdef")]);
        assert!(!origin_allowed(&extension));
    }

    #[test]
    fn origin_mismatch_is_forbidden_even_with_valid_credentials() {
        let key = TokenCell::new("flux2026");
        let request = headers(&[
            ("origin", "https://evil.example"),
            ("host", "nas.local"),
            ("authorization", "Bearer flux2026"),
        ]);
        assert_eq!(
            authorize_rpc(&request, "agent", &key),
            Err(StatusCode::FORBIDDEN)
        );
    }

    #[test]
    fn bootstrap_enables_groups_only_on_fresh_install_and_seeds_token() {
        let seed = TokenSeed::default();
        let mut fresh = AgentState::default();
        assert!(apply_bootstrap(&mut fresh, true, &seed));
        assert!(fresh.gateway.takeover_enabled);
        assert!(fresh.gateway.jsonrpc_enabled);
        assert!(fresh.gateway.api_enabled);
        assert!(fresh.gateway.mcp_enabled);
        assert!(!apply_bootstrap(&mut fresh, true, &seed));

        // 迁移导入了旧 server 的密钥：保持其既有开关。
        let mut legacy = AgentState {
            gateway_user_token: "legacy2026".to_owned(),
            ..AgentState::default()
        };
        assert!(!apply_bootstrap(&mut legacy, true, &seed));
        assert!(!legacy.gateway.api_enabled);

        let preset = TokenSeed {
            preset: Some("preset2026".to_owned()),
            force: false,
        };
        let mut empty = AgentState::default();
        assert!(apply_bootstrap(&mut empty, false, &preset));
        assert_eq!(empty.gateway_user_token, "preset2026");
        assert!(empty.gateway.user_token_configured);
        // 非 force：已有密钥不被覆盖；force：覆盖。
        assert!(!apply_bootstrap(&mut legacy, false, &preset));
        assert_eq!(legacy.gateway_user_token, "legacy2026");
        let forced = TokenSeed {
            preset: Some("preset2026".to_owned()),
            force: true,
        };
        assert!(apply_bootstrap(&mut legacy, false, &forced));
        assert_eq!(legacy.gateway_user_token, "preset2026");
        assert!(!apply_bootstrap(&mut legacy, false, &forced));
    }

    struct Harness {
        handle: Arc<ServerHandle>,
        state: Arc<tokio::sync::Mutex<AgentState>>,
        store: Arc<StateStore>,
        dir: PathBuf,
        ready: tokio::sync::watch::Sender<bool>,
        shutdown: CancellationToken,
        server: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    }

    impl Harness {
        async fn new(label: &str, token: &str) -> Self {
            Self::with_local_platform(label, token, false).await
        }

        async fn with_local_platform(label: &str, token: &str, allow_local_platform: bool) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fluxdown_agent_{label}_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let store = Arc::new(StateStore::open(dir.clone()).await.unwrap());
            let state = Arc::new(tokio::sync::Mutex::new(AgentState {
                gateway_user_token: token.to_owned(),
                ..AgentState::default()
            }));
            let (ready, ready_rx) = tokio::sync::watch::channel(true);
            let daemon = DaemonClientConfig::new("ws://127.0.0.1:9/rpc", "daemon-token");
            let handle = Arc::new(
                ServerHandle::new(ServerHandleParts {
                    token: TokenCell::new(token),
                    state: state.clone(),
                    store: store.clone(),
                    events: AgentEventHub::new(AgentSnapshot::default()),
                    ready: ready_rx,
                    blobs: Arc::new(DaemonBlobClient::new(&daemon).unwrap()),
                    diagnostics: Arc::new(crate::diagnostics::DiagnosticsService::new(
                        Arc::new(crate::daemon_client::DaemonClient::disconnected()),
                        DaemonClientConfig::new("ws://127.0.0.1:9/rpc", String::new()),
                        AgentEventHub::new(AgentSnapshot::default()),
                        state.clone(),
                        store.clone(),
                        Arc::new(fluxdown_api::server::ApiRuntimeSwitches::new(
                            false, false, false, false, false,
                        )),
                        TokenCell::new(""),
                    )),
                    daemon,
                    webroot: None,
                    demo: false,
                    allow_local_platform,
                    link: None,
                })
                .unwrap(),
            );
            Self {
                handle,
                state,
                store,
                dir,
                ready,
                shutdown: CancellationToken::new(),
                server: None,
            }
        }

        async fn serve(&mut self) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let app = router(self.handle.clone())
                .into_make_service_with_connect_info::<std::net::SocketAddr>();
            let shutdown = self.shutdown.clone();
            self.server = Some(tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown.cancelled_owned())
                    .await
            }));
            base
        }

        async fn finish(self) {
            let Self {
                handle,
                state,
                store,
                dir,
                ready: _,
                shutdown,
                server,
            } = self;
            shutdown.cancel();
            if let Some(server) = server {
                server
                    .await
                    .expect("join headless agent test server")
                    .expect("serve headless agent test");
            }
            drop(handle);
            drop(state);
            drop(store);
            if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), error = %error, "remove server test directory");
            }
        }
    }

    #[tokio::test]
    async fn explicit_flag_extends_task_file_actions_to_remote_connections() {
        let remote: IpAddr = "192.0.2.10".parse().unwrap();
        let public = header_map(&[
            ("host", "downloads.example.com"),
            ("origin", "https://downloads.example.com"),
        ]);
        let local = header_map(&[("host", "localhost:17800")]);
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();

        let default = Harness::new("local_platform_default", "flux2026").await;
        assert!(!default.handle.local_platform_permitted(remote, &public));
        assert!(default.handle.local_platform_permitted(loopback, &local));
        default.finish().await;

        let explicit =
            Harness::with_local_platform("local_platform_explicit", "flux2026", true).await;
        assert!(explicit.handle.local_platform_permitted(remote, &public));
        explicit.finish().await;
    }

    #[tokio::test]
    async fn setup_endpoint_moves_empty_to_set_to_conflict() {
        let mut harness = Harness::new("setup", "").await;
        let base = harness.serve().await;
        let client = reqwest::Client::new();

        let status: serde_json::Value = client
            .get(format!("{base}/api/v1/setup/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            status,
            serde_json::json!({ "setupRequired": true, "minLength": 8 })
        );

        let post = |body: &str| {
            client
                .post(format!("{base}/api/v1/setup"))
                .header("content-type", "application/json")
                .body(body.to_owned())
                .send()
        };
        let weak = post(r#"{"token":"short"}"#).await.unwrap();
        assert_eq!(weak.status(), 400);
        let weak: serde_json::Value = weak.json().await.unwrap();
        assert_eq!(
            weak,
            serde_json::json!({ "error": "access key must be at least 8 characters" })
        );
        assert_eq!(post("not json").await.unwrap().status(), 400);
        assert!(harness.handle.access_key().is_empty());

        let done = post(r#"{"token":"flux2026"}"#).await.unwrap();
        assert_eq!(done.status(), 200);
        let done: serde_json::Value = done.json().await.unwrap();
        assert_eq!(done, serde_json::json!({ "ok": true }));
        assert_eq!(&*harness.handle.access_key().get(), "flux2026");
        assert_eq!(harness.state.lock().await.gateway_user_token, "flux2026");
        assert!(harness.state.lock().await.gateway.user_token_configured);
        let persisted = harness.store.load().await.unwrap();
        assert_eq!(persisted.gateway_user_token, "flux2026");

        let again = post(r#"{"token":"other2026"}"#).await.unwrap();
        assert_eq!(again.status(), 409);
        let again: serde_json::Value = again.json().await.unwrap();
        assert_eq!(
            again,
            serde_json::json!({ "error": "setup already completed" })
        );
        assert_eq!(&*harness.handle.access_key().get(), "flux2026");

        let status: serde_json::Value = client
            .get(format!("{base}/api/v1/setup/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["setupRequired"], false);
        harness.finish().await;
    }

    #[tokio::test]
    async fn browser_file_routes_reject_missing_wrong_and_empty_credentials() {
        let mut harness = Harness::new("files", "flux2026").await;
        let base = harness.serve().await;
        let client = reqwest::Client::new();
        for path in ["/api/web/files/tasks/t1", "/api/web/exports/e1"] {
            let anonymous = client.get(format!("{base}{path}")).send().await.unwrap();
            assert_eq!(anonymous.status(), 401, "{path}");
            let wrong = client
                .get(format!("{base}{path}?token=flux2027"))
                .send()
                .await
                .unwrap();
            assert_eq!(wrong.status(), 401, "{path}");
            let empty = client
                .get(format!("{base}{path}?token="))
                .send()
                .await
                .unwrap();
            assert_eq!(empty.status(), 401, "{path}");
            // 凭据正确但 daemon 未连接：返回 503，而不是放行或挂起。
            let bearer = client
                .get(format!("{base}{path}"))
                .bearer_auth("flux2026")
                .send()
                .await
                .unwrap();
            assert_eq!(bearer.status(), 503, "{path}");
        }
        let logs = format!("{base}/api/web/logs/export");
        assert_eq!(client.get(&logs).send().await.unwrap().status(), 401);
        for query in ["?token=flux2027", "?token="] {
            let denied = client.get(format!("{logs}{query}")).send().await.unwrap();
            assert_eq!(denied.status(), 401, "{query}");
        }
        let ok = client
            .get(format!("{logs}?token=flux2026"))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.headers()[header::CONTENT_TYPE], "application/zip");
        assert!(ok.bytes().await.unwrap().starts_with(b"PK"));
        let upload = client
            .post(format!("{base}/api/web/blobs/torrents"))
            .body(vec![1_u8, 2, 3])
            .send()
            .await
            .unwrap();
        assert_eq!(upload.status(), 401);
        harness.finish().await;
    }

    #[tokio::test]
    async fn setup_waits_for_daemon_readiness_signal() {
        let mut harness = Harness::new("ready", "").await;
        harness.ready.send(false).unwrap();
        let base = harness.serve().await;
        let client = reqwest::Client::new();
        let pending = tokio::spawn({
            let url = format!("{base}/api/v1/setup/status");
            async move { reqwest::get(url).await.unwrap().status() }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!pending.is_finished(), "status must wait for readiness");
        harness.ready.send(true).unwrap();
        assert_eq!(pending.await.unwrap(), 200);
        drop(client);
        harness.finish().await;
    }

    #[tokio::test]
    async fn failed_device_credentials_feed_the_same_throttle_as_every_other_credential() {
        let harness = Harness::new("device_throttle", "flux2026").await;
        let remote: IpAddr = "203.0.113.9".parse().unwrap();
        let bearer = |value: &str| headers(&[("authorization", &format!("Bearer {value}"))]);
        // 形状正确但没有任何名册背书的设备凭据（本机没有互联服务 / 名册里没有该设备）。
        let forged = format!("fdl1.{}.{}", "a".repeat(64), "b".repeat(43));
        for _ in 0..THROTTLE_MAX_FAILURES {
            assert_eq!(
                harness
                    .handle
                    .authorize_rpc_from(remote, &bearer(&forged), "agent")
                    .await,
                Err(StatusCode::UNAUTHORIZED)
            );
        }
        // 锁定：`/rpc` 与浏览器文件面都拒绝，连正确的访问密钥也不受理；本机来源不受节流。
        assert_eq!(
            harness
                .handle
                .authorize_rpc_from(remote, &bearer("flux2026"), "agent")
                .await,
            Err(StatusCode::TOO_MANY_REQUESTS)
        );
        assert_eq!(
            harness
                .handle
                .browser_authorized(remote, &bearer("flux2026"), None)
                .await,
            Err(StatusCode::TOO_MANY_REQUESTS)
        );
        let local: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            harness
                .handle
                .authorize_rpc_from(local, &bearer("flux2026"), "agent")
                .await,
            Ok(RpcCredential::AccessKey("flux2026".to_owned()))
        );
        // 设备凭据不走子协议：子协议里的同一串只会被当成（错误的）访问密钥。
        let subprotocol = format!("{RPC_SUBPROTOCOL}, {}", token_protocol(&forged));
        assert_eq!(
            harness
                .handle
                .authorize_rpc_from(
                    local,
                    &headers(&[("sec-websocket-protocol", &subprotocol)]),
                    "agent"
                )
                .await,
            Err(StatusCode::UNAUTHORIZED)
        );
        harness.finish().await;
    }
}

#[cfg(test)]
mod throttle_tests {
    use std::net::IpAddr;
    use std::time::{Duration, Instant};

    use super::{AuthThrottle, THROTTLE_MAX_FAILURES, THROTTLE_WINDOW};

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))
    }

    #[test]
    fn repeated_failures_lock_the_source_until_the_window_ends() {
        let throttle = AuthThrottle::default();
        let peer = ip("203.0.113.7");
        let start = Instant::now();
        for _ in 0..THROTTLE_MAX_FAILURES {
            assert!(!throttle.is_locked(peer, start));
            throttle.record_failure(peer, start);
        }
        assert!(throttle.is_locked(peer, start + Duration::from_secs(1)));
        assert!(!throttle.is_locked(ip("203.0.113.8"), start));
        assert!(!throttle.is_locked(peer, start + THROTTLE_WINDOW));
    }

    #[test]
    fn success_clears_failures_and_loopback_is_never_limited() {
        let throttle = AuthThrottle::default();
        let peer = ip("198.51.100.1");
        let now = Instant::now();
        for _ in 0..THROTTLE_MAX_FAILURES - 1 {
            throttle.record_failure(peer, now);
        }
        throttle.record_success(peer);
        throttle.record_failure(peer, now);
        assert!(!throttle.is_locked(peer, now));

        let local = ip("127.0.0.1");
        for _ in 0..THROTTLE_MAX_FAILURES * 2 {
            throttle.record_failure(local, now);
        }
        assert!(!throttle.is_locked(local, now));
    }

    #[test]
    fn ipv6_addresses_share_a_64_bit_bucket() {
        let throttle = AuthThrottle::default();
        let now = Instant::now();
        for last in 0..THROTTLE_MAX_FAILURES {
            throttle.record_failure(ip(&format!("2001:db8:1:2::{last:x}")), now);
        }
        assert!(throttle.is_locked(ip("2001:db8:1:2:ffff::1"), now));
    }
}
