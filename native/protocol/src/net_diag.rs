//! 网络诊断的 wire 契约：本机网卡事实（`daemon.net.interfaces`）与按真实下载出口的
//! 吞吐测量（`daemon.net.measureRoutes`）。
//!
//! 两者都在下载服务所在主机（daemon）上执行。测量只把数据读进内存后丢弃：不建任务、
//! 不写下载目录、不写数据库，也不改变任何设置。结果不含 IP 地址与凭据（地址只给范围分类）。

use serde::{Deserialize, Serialize};

/// 单次测量的路线数上限。
pub const NET_MEASURE_MAX_ROUTES: usize = 4;
/// 并行连接数上限。
pub const NET_MEASURE_MAX_CONNECTIONS: u32 = 32;
/// 每条路线读取量上限（MiB）。
pub const NET_MEASURE_MAX_MEGABYTES: u32 = 512;
/// 每条路线默认读取量（MiB）。
pub const NET_MEASURE_DEFAULT_MEGABYTES: u32 = 32;
/// 每条路线传输时长上下限与默认值（秒）。
pub const NET_MEASURE_MIN_SECONDS: u32 = 3;
pub const NET_MEASURE_MAX_SECONDS: u32 = 60;
pub const NET_MEASURE_DEFAULT_SECONDS: u32 = 15;

/// 地址范围分类（不暴露地址本身）。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum NetAddressScope {
    Loopback,
    LinkLocal,
    /// RFC 1918 / IPv6 ULA。
    Private,
    /// 运营商级 NAT（100.64.0.0/10）。
    SharedCgnat,
    Public,
    Other,
}

/// 网卡类型（按系统报告与名称推断）。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum NetInterfaceKind {
    Ethernet,
    Wifi,
    Cellular,
    Vpn,
    Loopback,
    Other,
}

/// 一块网卡。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct NetInterfaceDto {
    pub name: String,
    pub kind: NetInterfaceKind,
    pub up: bool,
    /// 协商链路速率（Mbps；Wi-Fi 为当前 PHY 速率）。系统不报告时为 `None`。
    #[serde(default)]
    pub link_speed_mbps: Option<u64>,
    #[serde(default)]
    pub mtu: Option<u32>,
    /// 已配置的 IPv4 地址范围（每个地址一项）。
    #[serde(default)]
    pub ipv4: Vec<NetAddressScope>,
    #[serde(default)]
    pub ipv6: Vec<NetAddressScope>,
    /// 承载 IPv4 / IPv6 默认路由。
    #[serde(default)]
    pub default_route_v4: bool,
    #[serde(default)]
    pub default_route_v6: bool,
}

/// 系统正在使用的一个 DNS 服务器：公网地址原样给出（公共解析服务可辨识），其余只给范围。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct NetDnsServerDto {
    pub scope: NetAddressScope,
    #[serde(default)]
    pub address: Option<String>,
}

/// `daemon.net.interfaces` 的结果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct NetInterfacesDto {
    pub interfaces: Vec<NetInterfaceDto>,
    #[serde(default)]
    pub dns_servers: Vec<NetDnsServerDto>,
    /// 采集过程中无法取得的事实（例如系统工具不可用），供解读时说明缺口。
    #[serde(default)]
    pub notes: Vec<String>,
}

/// `daemon.net.measureRoutes` 参数。`task_id` 与 `url` 恰好给一个。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct NetMeasureParams {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// 依次测量的路线：`current` / `direct` / `system` / `manual` 或不含凭据的代理 URL。
    /// 空 = `["current", "direct"]`。
    #[serde(default)]
    pub routes: Vec<String>,
    #[serde(default)]
    pub connections: Option<u32>,
    #[serde(default)]
    pub max_megabytes: Option<u32>,
    #[serde(default)]
    pub max_seconds: Option<u32>,
}

/// 被测来源的事实（不含完整 URL）。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct NetMeasureSourceDto {
    /// `http` / `https` / `ftp`。
    pub scheme: String,
    pub host: String,
    /// 被测任务 ID（按任务测量时）。
    #[serde(default)]
    pub task_id: Option<String>,
    /// 计划的并行连接数。
    pub connections: u32,
    pub max_bytes: u64,
    pub max_seconds: u32,
}

/// 一条路线的测量结果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct NetRouteMeasureDto {
    /// 请求的路线（原样，代理 URL 去掉凭据）。
    pub route: String,
    /// 实际出口（`direct`、`http://host:port`、`socks5h://host:port`、`system` …，不含凭据）。
    pub egress: String,
    /// 出口标签：命中的站点规则（`rule:*`）、Auto 决策或静态标签；可能为空。
    #[serde(default)]
    pub route_label: String,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// 首个响应的 HTTP 状态码（FTP 为 `None`）。
    #[serde(default)]
    pub http_status: Option<u16>,
    /// 计时的目标：`origin`（直连源站）或 `proxy`（代理服务器）。
    #[serde(default)]
    pub timing_target: String,
    #[serde(default)]
    pub dns_ms: Option<u64>,
    #[serde(default)]
    pub connect_ms: Option<u64>,
    #[serde(default)]
    pub tls_ms: Option<u64>,
    /// 发出首个请求到收到响应头（含建连、代理握手与重定向）。
    #[serde(default)]
    pub first_byte_ms: Option<u64>,
    /// 服务器是否接受 Range（多连接前提）。
    #[serde(default)]
    pub range_supported: Option<bool>,
    /// 来源总大小（字节）。
    #[serde(default)]
    pub total_bytes: Option<u64>,
    pub bytes: u64,
    /// 传输阶段时长（秒）。
    pub seconds: f64,
    pub avg_bytes_per_sec: u64,
    /// 1 秒滑动窗口内的峰值。
    pub peak_bytes_per_sec: u64,
    /// 实际建立的数据连接数。
    pub connections_used: u32,
    /// 每条连接的平均速率（字节 / 秒）。
    #[serde(default)]
    pub per_connection_bytes_per_sec: Vec<u64>,
    /// 停止原因：`bytes`（达到读取量）、`time`（达到时长）、`eof`（来源读完）、`error`。
    #[serde(default)]
    pub stopped_by: String,
}

/// `daemon.net.measureRoutes` 的结果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct NetMeasureResultDto {
    pub source: NetMeasureSourceDto,
    pub routes: Vec<NetRouteMeasureDto>,
}
