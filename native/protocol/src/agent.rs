//! 官方 agent 暴露给 UI 的无令牌 DTO。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 云端任务连接生命周期，独立于配置同步连接。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum CloudConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

/// 仅内存中的任务连接状态；connected 要求 SSE、心跳及其后的设备名册刷新均成功。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudConnectionDto {
    pub state: CloudConnectionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_reason: Option<crate::ErrorReason>,
}

/// FluxCloud 用户状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum CloudUserStatus {
    #[default]
    Active,
    Disabled,
    Pending,
    /// 云端新增、本端不认识的状态。
    #[serde(other)]
    Unknown,
}

/// FluxCloud 用户公开资料。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudUser {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub status: CloudUserStatus,
    #[serde(default)]
    pub created_at: String,
    pub last_login_at: Option<String>,
    pub origin_id: Option<i64>,
    #[serde(default)]
    pub origin_id_changed: bool,
    pub membership_ordinal: Option<i64>,
    /// 账号是否设置了登录密码；旧版云端不下发时为 `None`（视为未知）。
    #[serde(default)]
    pub has_password: Option<bool>,
}

/// 前向兼容的套餐权益集合。未知字段必须原样保留。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(transparent)]
pub struct Entitlements(pub BTreeMap<String, Value>);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudPlanCampaignStage {
    pub label: String,
    pub price_minor: i64,
    pub quota: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudPlanCampaign {
    pub name: String,
    pub end_at: Option<String>,
    pub stages: Vec<CloudPlanCampaignStage>,
    pub sold_total: i64,
    pub stage_sold: Vec<i64>,
    pub current_stage_index: i64,
    pub effective_price_minor: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudPlan {
    pub code: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub badge: Option<String>,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub color: String,
    #[serde(default = "default_badge_style")]
    pub badge_style: String,
    #[serde(default)]
    pub badge_color: String,
    #[serde(default)]
    pub badge_numbered: bool,
    #[serde(default = "default_badge_number_digits")]
    pub badge_number_digits: i64,
    #[serde(default)]
    pub price_minor: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default)]
    pub highlights: Vec<String>,
    #[serde(default, rename = "entitlements")]
    pub entitlements_raw: BTreeMap<String, Value>,
    #[serde(default)]
    pub sort: i64,
    pub campaign: Option<CloudPlanCampaign>,
    #[serde(default = "default_true")]
    pub purchasable: bool,
    /// 周期会员卡的周期天数；`None` = 永久套餐。
    #[serde(default)]
    pub billing_period_days: Option<u32>,
}

fn default_badge_style() -> String {
    "outline".to_owned()
}

fn default_badge_number_digits() -> i64 {
    4
}

fn default_currency() -> String {
    "CNY".to_owned()
}

fn default_true() -> bool {
    true
}

/// 当前账户资料与套餐快照（`GET /me`）。FluxCloud 把用户字段平铺在顶层，
/// `entitlements` / `currentPlan` / `purchaseCreditMinor` 为同级字段，因此 `user` 用 `flatten` 映射。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudProfile {
    #[serde(flatten)]
    pub user: CloudUser,
    #[serde(default)]
    pub entitlements: Entitlements,
    pub current_plan: Option<CloudPlan>,
    #[serde(default)]
    pub purchase_credit_minor: i64,
    /// 当前生效的周期会员卡；没有为 `None`。
    #[serde(default)]
    pub active_subscription: Option<CloudSubscription>,
}

/// 生效中的周期会员卡（手动续费；到期回到永久套餐权益）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct CloudSubscription {
    pub plan_code: String,
    pub plan_name: String,
    /// 开始时间（RFC 3339）。
    pub starts_at: String,
    /// 到期时间（RFC 3339）。
    pub ends_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct OriginIdCheckResult {
    pub available: bool,
    pub reason: Option<String>,
}

/// 受信任设备公开投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudDevice {
    pub id: String,
    pub device_id: String,
    #[serde(default)]
    pub name: String,
    pub platform: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub last_seen_at: String,
    pub last_ip: Option<String>,
    pub app_version: Option<String>,
    #[serde(default)]
    pub is_online: bool,
    #[serde(default)]
    pub is_current: bool,
    /// 设备自报的默认下载目录（目标设备本地路径；远程下发不填保存目录时使用它）。
    #[serde(default)]
    pub default_save_dir: Option<String>,
    /// 设备自报的本地路径风格；`None` = 旧版客户端未上报（可按 `platform` 推断）。
    #[serde(default)]
    pub path_style: Option<PathStyle>,
}

/// 设备本地文件路径的书写风格，决定远程下发时保存目录的合法形态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum PathStyle {
    /// `C:\dir` / `\\server\share`。
    Windows,
    /// `/dir`。
    Posix,
    /// 对端发送了本端不认识的风格。
    #[serde(other)]
    Unknown,
}

impl PathStyle {
    /// 本进程所在平台的路径风格。
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }

    /// 按设备平台名（`windows` / `macos` / `linux` / `android` / `ios` / `web` …）推断。
    #[must_use]
    pub fn from_platform(platform: &str) -> Option<Self> {
        match platform.trim().to_ascii_lowercase().as_str() {
            "windows" | "win32" => Some(Self::Windows),
            "macos" | "darwin" | "linux" | "android" | "ios" | "freebsd" | "openbsd" | "netbsd" => {
                Some(Self::Posix)
            }
            _ => None,
        }
    }

    /// `path` 是否为该风格下的绝对路径。
    #[must_use]
    pub fn is_absolute(self, path: &str) -> bool {
        let path = path.trim();
        match self {
            Self::Windows => {
                let bytes = path.as_bytes();
                let drive = bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'\\' | b'/');
                drive || path.starts_with("\\\\")
            }
            Self::Posix => path.starts_with('/'),
            Self::Unknown => false,
        }
    }
}

impl CloudDevice {
    /// 设备自报的路径风格；旧版客户端未上报时按平台推断。
    #[must_use]
    pub fn effective_path_style(&self) -> Option<PathStyle> {
        self.path_style
            .filter(|style| *style != PathStyle::Unknown)
            .or_else(|| self.platform.as_deref().and_then(PathStyle::from_platform))
    }
}

/// 无 access/refresh token 的本地会话视图。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionDto {
    pub user: CloudUser,
    #[serde(default)]
    pub entitlements: Entitlements,
    pub current_plan: Option<CloudPlan>,
    pub device: CloudDevice,
    /// 当前生效的周期会员卡；没有为 `None`。
    #[serde(default)]
    pub active_subscription: Option<CloudSubscription>,
}

/// 邮箱或设备验证步骤的无令牌元数据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AuthVerificationDto {
    pub ttl_seconds: u64,
    #[serde(default)]
    pub will_replace_devices: bool,
}

/// 登录的本地安全结果。成功结果只包含无令牌会话。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AgentLoginResult {
    Ok {
        session: Box<AgentSessionDto>,
    },
    DeviceVerificationRequired {
        ttl_seconds: u64,
        #[serde(default)]
        will_replace_devices: bool,
    },
}

/// 购买订单。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudOrder {
    pub order_no: String,
    pub plan_code: String,
    #[serde(default)]
    pub plan_name: String,
    #[serde(default = "default_order_status")]
    pub status: String,
    #[serde(default)]
    pub amount_minor: i64,
    #[serde(default)]
    pub list_price_minor: i64,
    #[serde(default)]
    pub credit_minor: i64,
    pub upgrade_from_plan: Option<String>,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub campaign_name: Option<String>,
    pub stage_label: Option<String>,
    pub referral_code: Option<String>,
    #[serde(default)]
    pub referral_discount_minor: i64,
    pub code_url: Option<String>,
    #[serde(default)]
    pub created_at: String,
    pub paid_at: Option<String>,
    #[serde(default)]
    pub expires_at: String,
}

fn default_order_status() -> String {
    "pending".to_owned()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudReferralRule {
    pub plan_code: String,
    pub plan_name: String,
    pub price_minor: i64,
    pub discount_minor: i64,
    pub reward_percent: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudReferralSummary {
    pub enabled: bool,
    pub description: String,
    pub reward_enabled: bool,
    pub contact: String,
    pub invited_count: i64,
    pub pending_reward_minor: i64,
    pub paid_reward_minor: i64,
    pub total_reward_minor: i64,
    pub rules: Vec<CloudReferralRule>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudReferralCode {
    pub id: String,
    pub code: String,
    pub paid_order_count: i64,
    pub reward_minor: i64,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudReferralCodesResult {
    pub total: i64,
    pub items: Vec<CloudReferralCode>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudReferralRecord {
    pub id: String,
    pub buyer_label: String,
    pub order_amount_minor: i64,
    pub reward_minor: i64,
    pub reward_percent: i64,
    pub status: String,
    pub created_at: String,
    pub paid_at: Option<String>,
    pub referral_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudReferralRecordsResult {
    pub total: i64,
    pub items: Vec<CloudReferralRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudReferralValidateResult {
    pub valid: bool,
    pub discount_minor: i64,
    pub reason: Option<String>,
}

/// 插件市场收藏概览：全站计数 + 本人收藏。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginFavoritesDto {
    /// 仅含收藏数大于 0 的插件。
    pub counts: std::collections::BTreeMap<String, u64>,
    /// 本人收藏的插件 id（最近收藏在前）；未登录为空。
    pub mine: Vec<String>,
    pub signed_in: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginFavoriteSetRequest {
    pub plugin_id: String,
    pub favorited: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginFavoriteResultDto {
    pub plugin_id: String,
    pub favorited: bool,
    pub count: u64,
}

/// `agent.privatePlugin.list`：FluxCloud 私有商店中当前账号被授权的条目。
///
/// 未登录时不发任何云请求，返回 `signedIn:false` 与空列表；已登录但无授权同样是空列表。
/// 条目复用 [`MarketEntryDto`]（`channel == "private"`，`mirrors` 恒空，`expiresAt` 为授权到期时间）。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PrivatePluginListDto {
    pub signed_in: bool,
    pub entries: Vec<crate::daemon::MarketEntryDto>,
}

/// `agent.privatePlugin.install`：`version` = 用户确认权限时看到的版本；云端当前可装版本不一致时
/// 以 `marketVersionChanged` 拒绝，由客户端刷新目录后重新确认。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PrivatePluginInstallRequest {
    pub plugin_id: String,
    pub version: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum RemoteTaskStatus {
    Accepted,
    Downloading,
    Paused,
    Completed,
    Failed,
    Canceled,
    #[default]
    Pending,
    /// 云端新增、本端不认识的状态；不参与接单与控制。
    #[serde(other)]
    Unknown,
}

/// 跨设备任务的 UI 投影。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskDto {
    pub id: String,
    #[serde(default)]
    pub from_device: String,
    #[serde(default)]
    pub to_device: String,
    #[serde(default)]
    pub url: String,
    pub save_dir: Option<String>,
    /// 云端对未指定文件名的任务回 `null`，按空串处理。
    #[serde(default, deserialize_with = "null_as_default")]
    pub file_name: String,
    #[serde(default)]
    pub status: RemoteTaskStatus,
    pub total_bytes: Option<i64>,
    #[serde(default)]
    pub downloaded_bytes: i64,
    #[serde(default)]
    pub speed: i64,
    #[serde(default)]
    pub progress: f64,
    pub error: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// `agent.remote.dispatch` 参数：经 FluxCloud 把下载下发到本账号另一台受信任设备。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteDispatchParams {
    /// 目标设备的 `CloudDevice::device_id`。
    pub to_device: String,
    pub url: String,
    /// 空 / 省略 = 由目标设备按 URL 推断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// 目标设备上的保存目录；省略 = 目标设备的默认下载目录。必须符合目标设备的路径风格。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_dir: Option<String>,
}

/// `agent.remote.dispatch` 结果。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteDispatchResult {
    pub task: RemoteTaskDto,
}

/// 远程任务控制动作。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum RemoteCommandAction {
    Pause,
    Resume,
    /// 取消任务：云端直接置 `canceled`（不依赖目标在线），目标设备删除其本地任务、保留已下载文件。
    Cancel,
    /// 删除任务：云端直接删除记录（任何状态，不依赖目标在线）；目标设备删除其本地任务，
    /// 在线收到指令时按 `delete_files` 决定是否同时删文件，离线期间被删则保留文件。
    Delete,
}

/// `agent.remote.command` 参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteCommandParams {
    pub task_id: String,
    pub action: RemoteCommandAction,
    /// 幂等键；省略时 agent 生成唯一值（同一动作可重复下发）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    /// 仅 `Delete`：目标设备同时删除已下载文件。
    #[serde(default)]
    pub delete_files: bool,
}

/// UI Gateway 运行状态；永远不携带 token 文本。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatusDto {
    pub takeover_enabled: bool,
    pub jsonrpc_enabled: bool,
    pub api_enabled: bool,
    pub mcp_enabled: bool,
    pub cors_enabled: bool,
    pub user_token_configured: bool,
    /// 当前已验证可用的实际监听端口；修改失败时保持原值。
    #[serde(default = "default_gateway_port")]
    pub port: u16,
    /// server 模式或环境固定监听地址时为 false。
    #[serde(default = "default_true")]
    pub port_editable: bool,
    /// 是否对局域网开放兼容 API（持久化配置）；修改后下次 agent 启动生效。
    #[serde(default)]
    pub lan_enabled: bool,
    /// 本次运行的网关是否实际监听非回环地址（局域网 / 组网可达）。`lan_enabled` 已开但
    /// 尚未重启时为 `false`，界面据此提示「重启后生效」；server 模式恒为 `true`。
    #[serde(default)]
    pub lan_active: bool,
}

impl Default for GatewayStatusDto {
    fn default() -> Self {
        Self {
            takeover_enabled: false,
            jsonrpc_enabled: false,
            api_enabled: false,
            mcp_enabled: false,
            cors_enabled: false,
            user_token_configured: false,
            port: default_gateway_port(),
            port_editable: true,
            lan_enabled: false,
            lan_active: false,
        }
    }
}

fn default_gateway_port() -> u16 {
    17800
}

/// 原子修改 agent 托管的兼容网关开关与用户 token。
///
/// 管理 API / MCP 端点强制鉴权：`api_enabled` 或 `mcp_enabled` 由关转开且（应用本次
/// `user_token` 后）用户 token 仍为空时，agent 自动生成随机 token；反之显式把 token
/// 清空（且本次未开启上述开关）时，agent 同时关闭 `api_enabled` 与 `mcp_enabled`。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct GatewayPatchParams {
    pub takeover_enabled: Option<bool>,
    pub jsonrpc_enabled: Option<bool>,
    pub api_enabled: Option<bool>,
    pub mcp_enabled: Option<bool>,
    pub cors_enabled: Option<bool>,
    pub lan_enabled: Option<bool>,
    /// 1024..=65535；验证新 API/RPC 服务可用后立即切换，固定监听模式拒绝修改。
    pub port: Option<u16>,
    /// `Some("")` 清除用户 token；省略则保持。
    pub user_token: Option<String>,
    /// `true` 生成新的随机用户 token（优先于 `user_token`）。
    #[serde(default)]
    pub regenerate_user_token: bool,
}

/// agent 托盘不可用的原因。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum TrayUnavailableReason {
    /// agent 未编译托盘支持（headless 构建）：关闭全部 UI 后后台恒驻留。
    NotBuilt,
    /// 没有图形会话（无 `DISPLAY` / `WAYLAND_DISPLAY`）。
    NoDisplay,
    /// 桌面环境没有 StatusNotifier 托盘宿主（如未启用 AppIndicator 扩展的 GNOME）。
    NoHost,
    /// 平台托盘初始化失败（如缺少 appindicator 运行库）。
    InitFailed,
}

/// agent 系统外壳状态：托盘可用性，以及关闭全部官方 UI 后后台是否继续驻留。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ShellStatusDto {
    /// 托盘图标可用（`close_to_tray` 偏好可以生效）。
    pub tray_available: bool,
    pub tray_unavailable_reason: Option<TrayUnavailableReason>,
    /// `true`：关闭全部 UI 只退出界面，agent + daemon 继续运行；`false`：随界面一起退出。
    pub resident: bool,
    /// agent 全局快捷键后端与逐项注册状态（旧 agent 缺省 = 不可用）。
    #[serde(default)]
    pub global_shortcuts: crate::shortcuts::GlobalShortcutsStatusDto,
}

/// 定时电源操作：到点关机还是睡眠（挂起到内存）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum PowerAction {
    #[default]
    Shutdown,
    Sleep,
}

/// 定时关机 / 睡眠的调度状态（agent 拥有状态机与执行）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PowerStatusDto {
    /// 已排定的延迟（秒）；`None` = 未启用。
    pub armed_delay_secs: Option<u64>,
    /// 全部任务结束（或排定时本就没有任务）、真正开始倒计时后的剩余秒数；等待任务完成阶段为 `None`。
    pub countdown_remaining_secs: Option<u64>,
    /// 已排定的操作；未启用时为默认值（旧 agent 缺省 = 关机）。
    #[serde(default)]
    pub action: PowerAction,
}

/// `agent.power.arm` 参数：没有任务时立即开始倒计时，否则全部任务完成后再等待 `delay_secs`
/// 秒执行 `action`。`delay_secs = 0`（完成后立即）只在有任务时接受。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PowerArmParams {
    pub delay_secs: u64,
    /// 缺省 = 关机（旧客户端）。
    #[serde(default)]
    pub action: PowerAction,
}

/// agent 配置同步状态投影。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SyncStatusDto {
    pub enabled: bool,
    pub revision: u64,
    pub dirty_keys: Vec<String>,
    /// 诊断用原始错误文本（UI 优先按 `last_error_reason` 展示本地化文案）。
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_reason: Option<crate::ErrorReason>,
    /// 同步事件流当前已连通。
    #[serde(default)]
    pub connected: bool,
    /// 因不可自动恢复的错误（设备超限 / 设备未受信任）暂停自动重试，需用户处理后重新启用。
    #[serde(default)]
    pub halted: bool,
    /// 最近一次成功完成拉取 + 推送的时间。
    #[serde(default)]
    pub last_synced_at_unix_ms: Option<i64>,
    /// 本设备不参与云同步的同步目录键（设备本地，不上云）。
    #[serde(default)]
    pub local_only_keys: Vec<String>,
}

/// `agent.sync.setLocalOnly` 参数：把一组同步目录键设为本设备专属（不推送、不接收云端值）或恢复同步。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SyncLocalOnlyParams {
    pub keys: Vec<String>,
    pub local_only: bool,
}

/// 等待本机确认的入站局域网配对请求（对端已输入本机配对码）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairingRequestDto {
    pub session_id: String,
    pub peer_name: String,
    pub peer_fingerprint: String,
    #[serde(default)]
    pub peer_platform: Option<String>,
    /// 双方肉眼核对的短认证串。
    pub sas: String,
    pub expires_at_unix_ms: i64,
    /// 生成本 DTO 时刻的剩余有效毫秒数：客户端以「收到 DTO 的本机时刻 + `ttl_ms`」计算过期，
    /// 不依赖与主机的时钟对齐。旧主机没有此字段（`0`），回退 `expires_at_unix_ms`。
    #[serde(default)]
    pub ttl_ms: u64,
    /// 对端在配对中请求「完全控制」本机：批准即授权它经 `/rpc` 操作本机（含访问密钥、下载任务、
    /// 设置）。UI 必须明确警示。
    #[serde(default)]
    pub requests_control: bool,
}

/// 本机当前展示的局域网配对码（供对端输入）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairingCodeDto {
    pub code: String,
    pub expires_at_unix_ms: i64,
    /// 生成本 DTO 时刻的剩余有效毫秒数（语义同 [`LinkPairingRequestDto::ttl_ms`]）。
    #[serde(default)]
    pub ttl_ms: u64,
    /// 本机可被对端直连的地址（`http(s)://host:port`），供对端手动输入。
    pub addresses: Vec<String>,
    pub fingerprint: String,
    pub device_name: String,
}

/// `agent.link.stopPairing` 参数。带 `code` 时只有它仍是当前展示的配对码才作废（多个客户端
/// 互不干扰：某个客户端关闭自己的面板，不会作废另一个客户端刚刷新出来的新码）；省略则无条件
/// 作废当前配对码。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkStopPairingParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// `agent.link.discovery.set` 参数。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkDiscoveryParams {
    pub enabled: bool,
}

/// `agent.link.probe` 参数。`address` 接受 `host`、`host:port`、`http(s)://host[:port][/base]`；
/// 未写协议时按 `http`，未写端口时 `http` 用 17800、`https` 用 443。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkAddressParams {
    pub address: String,
}

/// `agent.link.pairBegin` 参数（地址规则同 [`LinkAddressParams`]）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairBeginParams {
    pub address: String,
    pub code: String,
    /// 同时请求「完全控制」对端（对端用户批准后，本机可用 `agent.link.controlCredential` 取得
    /// 对端 `/rpc` 的设备控制凭据）。对端必须声明支持（`/ping` 的 `linkControl >= 1`），否则
    /// 返回 `pairingVersionMismatch`、不发出 hello。
    #[serde(default)]
    pub request_control: bool,
}

/// `agent.link.pairFinish` 参数：发起端核对 SAS 后确认 / 放弃。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairFinishParams {
    pub token: String,
    pub accept: bool,
}

/// `agent.link.approve` 参数：响应端对入站配对请求的决定。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkApproveParams {
    pub session_id: String,
    pub accept: bool,
}

/// `agent.link.remove` 的设备定位参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkDeviceParams {
    pub fingerprint: String,
}

/// `agent.link.controlCredential` 结果：本机可用来连接已配对主机 `/rpc` 的设备控制凭据
/// （`fdl1.<发起方指纹>.<base64url(HMAC)>`，仅 `Authorization: Bearer` 使用）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkControlCredentialDto {
    pub token: String,
}

/// `agent.link.rename` 参数：给已配对设备设置仅本机可见的备注名。`name` 省略、为空或全空白
/// = 清除备注名，恢复显示对端自报名；非空时去首尾空白后须为 1–64 个字符且不含控制字符。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkRenameParams {
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `agent.link.dispatch` 参数：把下载直接下发到已配对的局域网设备。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkDispatchParams {
    pub fingerprint: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// 目标设备上的保存目录；省略 = 目标设备默认下载目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_dir: Option<String>,
}

/// `agent.link.dispatch` 结果：目标设备上新建任务的 ID。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkDispatchResult {
    pub task_id: String,
}

/// FluxCloud 服务地址；`editable=false`（正式构建）时 `base_url` 恒等于 `default_base_url`。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudEndpointDto {
    pub base_url: String,
    pub default_base_url: String,
    pub editable: bool,
}

/// `agent.cloud.endpointSet` 参数；`base_url` 为空即恢复默认地址。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CloudEndpointSetParams {
    #[serde(default)]
    pub base_url: String,
}

/// agent 自有偏好设置及其原子版本。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentPreferencesDto {
    pub revision: u64,
    pub values: BTreeMap<String, Value>,
}

/// `agent.preferences.patch` 的结果。
///
/// `revision` 是本次写入落定后的偏好版本：此后携带 `revision` 不低于它的
/// `PreferencesChanged` / 快照必然已包含本次写入。响应与事件在同一连接上不保证先后，
/// 客户端据此判断在途写入何时被确认，而不是在收到响应时立即放手。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPreferencesPatchResult {
    pub ok: bool,
    pub revision: u64,
}

/// 等待官方 UI 确认的外部捕获请求。
///
/// 不含 cookie / header / 请求体原文（这些只留在 agent 的捕获事务里，确认时由 agent
/// 合并进建任务参数），只给出携带摘要供 UI 提示。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PendingCaptureDto {
    pub transaction_id: String,
    pub url: String,
    #[serde(default)]
    pub file_name: String,
    pub created_at_unix_ms: i64,
    /// 捕获方声明的文件大小（字节，0 = 未知）。
    #[serde(default)]
    pub file_size: i64,
    /// 来源页面。
    #[serde(default)]
    pub referrer: String,
    /// 捕获方指定的保存目录（空 = 未指定）。
    #[serde(default)]
    pub save_dir: String,
    /// 是否携带浏览器 Cookie。
    #[serde(default)]
    pub has_cookies: bool,
    /// 携带的请求头名（不含值）。
    #[serde(default)]
    pub header_names: Vec<String>,
}

impl PendingCaptureDto {
    /// 浏览器请求已带 `Authorization` 头（HTTP 认证沿用浏览器原值）。
    #[must_use]
    pub fn has_authorization(&self) -> bool {
        self.header_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("authorization"))
    }
}

/// `agent.capture.resolve` 参数。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CaptureResolveParams {
    pub transaction_id: String,
    pub accepted: bool,
    /// 即使目录等于捕获表单的预填分类目录，也显式禁用分类路由。
    #[serde(default, skip_serializing_if = "crate::daemon::is_false")]
    pub disable_category_route: bool,
    /// 确认时官方 UI 表单产出的建任务参数（`None` = 按捕获原请求建任务）。
    ///
    /// agent 以捕获原请求为底合并：`url` / `method` / `body` / `audioUrl` 恒取捕获值；
    /// `fileName` / `saveDir` / `cookies` / `referrer` 为空时取捕获值；`headers` 以捕获头
    /// 为底、同名（忽略大小写）以表单为准；`userAgent` 非空时替换捕获的 `User-Agent` 头。
    #[serde(default)]
    pub request: Option<crate::daemon::CreateTaskRequest>,
}

/// `agent.capture.preview` 参数。只读预解析，不消费捕获事务。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CapturePreviewParams {
    pub transaction_id: String,
    /// 表单上下文；捕获原 URL / method / body / audioUrl 不可被覆盖。
    pub request: crate::daemon::CreateTaskRequest,
}

/// `agent.capture.createGroup` 参数。成功建组后单次消费捕获事务。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CaptureCreateGroupParams {
    pub transaction_id: String,
    /// 最终清单选择与组级选项；sourceUrl 恒取捕获原 URL。
    pub request: crate::daemon::CreateGroupRequest,
    /// 原表单 HTTP Basic 凭据；非空用户名覆盖浏览器 Authorization。
    pub context: crate::daemon::CreateTaskRequest,
}

/// `agent.capture.submitTorrentFile` 参数。
///
/// `silent=true`（系统打开 / 关联启动）全选文件直接建任务；`silent=false`（用户主动选择）
/// 由 daemon 发 BT 文件选择请求。`saveDir` / `queueId` / `startPaused` 缺省维持旧行为
/// （daemon 默认目录 / 默认队列 / 立即开始），新建下载表单入口携带表单值。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CaptureSubmitTorrentFileParams {
    /// 本机 `.torrent` 路径。
    pub path: String,
    #[serde(default)]
    pub silent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_paused: Option<bool>,
}

/// 桌面系统集成状态（开机自启、`.torrent` 关联、URL scheme 注册）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformIntegrationDto {
    pub autostart_supported: bool,
    pub autostart_enabled: bool,
    pub file_association_supported: bool,
    pub torrent_associated: bool,
    pub url_protocol_supported: bool,
    /// scheme → 是否已注册到 FluxDown（`magnet` / `ed2k` / `fluxdown`）。
    pub url_protocols: BTreeMap<String, bool>,
    /// 注册目标可执行文件；空表示未找到桌面程序。
    pub desktop_executable: String,
}

/// `agent.platform.setAutostart` 参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformToggleParams {
    pub enabled: bool,
}

/// `agent.platform.setUrlProtocol` 参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformUrlProtocolParams {
    pub scheme: String,
    pub enabled: bool,
}

/// `agent.platform.openPath` 参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformOpenPathParams {
    pub path: String,
    /// `true` 在文件管理器中定位而非直接打开。
    #[serde(default)]
    pub reveal: bool,
}

/// `agent.platform.copyTaskFiles` 参数：要写入系统剪贴板的任务（产物必须已在磁盘上）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformCopyTaskFilesParams {
    /// 非空；重复 id 只计一次，上限 [`PLATFORM_COPY_TASK_FILES_MAX`]。
    pub task_ids: Vec<String>,
}

/// 一次 `agent.platform.copyTaskFiles` 最多写入的任务数（防止整表误选拖垮 daemon 逐个查询）。
pub const PLATFORM_COPY_TASK_FILES_MAX: usize = 1000;

/// 按文件（而非扩展名）取图标的扩展名（小写）：图标内嵌在文件自身里。其余类型的图标只由
/// 扩展名关联决定，调用方按扩展名缓存即可。`.lnk` / `.url` 故意不在内：它们的图标路径可以
/// 指向远程共享，按文件解析会向外发起认证。
pub const FILE_ICON_PER_FILE_EXTENSIONS: &[&str] = &["exe", "ico"];

/// `agent.platform.fileIcon` 参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformFileIconParams {
    /// 不带点的扩展名（大小写不敏感）；空 = 无扩展名的普通文件。
    #[serde(default)]
    pub extension: String,
    /// 本机文件绝对路径。只在扩展名属于 [`FILE_ICON_PER_FILE_EXTENSIONS`] 且文件存在时按文件
    /// 取图标，否则按扩展名取。
    #[serde(default)]
    pub path: Option<String>,
    /// 目标边长（物理像素），agent 限制在 16..=256。
    pub size: u32,
}

/// `agent.platform.fileIcon` 结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformFileIconDto {
    /// `size`×`size` 的 PNG，base64 编码。
    pub png: String,
}

/// Doctor 检查项级别。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticLevel {
    #[default]
    Info,
    Ok,
    Warn,
    Error,
}

/// 单条 Doctor 检查结果。`id` 稳定，UI 据此选择标题文案与修复动作。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCheckDto {
    pub id: String,
    /// 同一 `id` 下的子目标（如浏览器名、scheme）；无则为空。
    #[serde(default)]
    pub target: String,
    pub level: DiagnosticLevel,
    pub detail: String,
    /// 既有 hint code（如 `HINT_CHECK_DISK`）；无则为空。
    #[serde(default)]
    pub hint: String,
    /// 可用的就地修复动作（`agent.diagnostics.repair` 的 `action`）；无则为 None。
    #[serde(default)]
    pub repair: Option<DiagnosticRepairParams>,
}

/// `agent.diagnostics.repair` 参数。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRepairParams {
    pub action: String,
    #[serde(default)]
    pub target: String,
}

/// `agent.diagnostics.run` 结果。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReportDto {
    pub generated_at_unix_ms: i64,
    pub app_version: String,
    pub platform: String,
    pub agent_data_dir: String,
    pub daemon_connected: bool,
    pub checks: Vec<DiagnosticCheckDto>,
}

/// `agent.diagnostics.exportLogs` 参数：目标文件路径（`.zip`）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LogExportParams {
    pub target_path: String,
}

/// `agent.diagnostics.exportLogs` 结果。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LogExportResult {
    pub path: String,
    pub bytes: u64,
}

/// 日志目录信息（`agent.diagnostics.logPaths`）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LogPathsDto {
    pub agent_log_dir: String,
    pub daemon_log_dir: String,
}

/// `agent.update.check` 参数。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckParams {
    /// `stable` | `frontier`；省略取偏好 `general.update_channel`。
    #[serde(default)]
    pub channel: Option<String>,
}

/// 单个版本的更新说明。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ReleaseNoteDto {
    pub version: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub body: String,
}

/// 更新流程阶段（`AgentSnapshot.update.phase`）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    /// 本次运行尚未检查。
    #[default]
    Idle,
    Checking,
    /// 已是渠道最新版本。
    UpToDate,
    /// 有新版本；`manualReason` 为空时可一键更新，否则只能按说明手动升级。
    Available,
    /// 后台下载更新包，进度见 `downloadedBytes` / `assetSize`。
    Downloading,
    /// 更新包已下载并校验，等待安装（重启）。
    Ready,
    /// 正在替换程序文件；成功后服务随即重启。
    Installing,
    /// 最近一次检查 / 下载 / 安装失败，原因见 `failure`。
    Failed,
}

/// 本机安装形态：决定更新资产与安装方式。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum UpdateInstallKind {
    /// Windows 安装版（Inno Setup，每用户）。
    WindowsSetup,
    /// Windows 便携版（目录内 `portable` 标记）。
    WindowsPortable,
    /// macOS `FluxDown.app`。
    MacosApp,
    LinuxAppImage,
    LinuxDeb,
    LinuxArch,
    /// Linux 便携 tar.gz。
    LinuxPortable,
    /// headless 服务器的二进制包（tar.gz / zip，`fluxdown-agent` + `fluxdownd`）。
    ServerBinary,
    /// Docker 镜像：容器内替换文件会随重建丢失，只能拉取新镜像。
    Docker,
    Synology,
    Qnap,
    Openwrt,
    /// 飞牛 fnOS 应用中心（.fpk）。
    Fnos,
    /// Android APK（官网分发）：由 App 交给安装程序（用户默认的 APK 安装程序）安装。
    AndroidApk,
    #[default]
    #[serde(other)]
    Unknown,
}

/// 不能应用内一键更新的原因；客户端据此给出手动升级说明。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum UpdateManualReason {
    /// 由包管理器 / 容器镜像管理（Docker、群晖、威联通、OpenWrt，或归 AUR / 发行版仓库等
    /// 非官方系统包所有的 `linuxDeb` / `linuxArch`），具体见 `installKind`。
    ManagedPackage,
    /// 程序所在目录对当前用户不可写（如放在受保护目录、只读卷）。
    NotWritable,
    /// 发布中缺少本平台 / 架构 / 安装形态的资产或其校验和。
    NoAsset,
    /// 需要管理员授权安装（deb / Arch），但没有可用的图形授权（缺 `pkexec`、无桌面会话）。
    ElevationUnavailable,
    /// macOS 应用在只读位置运行（App Translocation、直接从 DMG 运行），需先拖入「应用程序」。
    ReadOnlyLocation,
    /// 当前构建不是正式签名发行版（开发构建、ad-hoc 签名），不自动替换。
    UnofficialBuild,
    /// 平台不支持应用内安装。
    Unsupported,
    #[serde(other)]
    Unknown,
}

/// 更新失败分类。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum UpdateFailure {
    /// 无法访问更新服务或下载源。
    Network,
    /// 下载内容的大小或 SHA-256 与发布校验和不符。
    Verify,
    /// 写入更新包失败（磁盘空间 / 权限）。
    Storage,
    /// 替换程序文件失败；原程序保持不变。
    Install,
    /// 用户取消了管理员授权。
    ElevationCancelled,
    /// 上次安装没有完成：重启后的版本仍是旧版本。
    InstallIncomplete,
    #[serde(other)]
    Unknown,
}

/// agent 自有的更新状态（`AgentSnapshot.update`，变化经 `AgentEvent::UpdateChanged` 推送），
/// 也是 `agent.update.*` 方法的返回值。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatusDto {
    pub phase: UpdatePhase,
    pub current_version: String,
    /// 最近一次检查使用的渠道（`stable` | `frontier`）。
    #[serde(default)]
    pub channel: String,
    /// 渠道最新版本；未检查时为空串。
    #[serde(default)]
    pub latest_version: String,
    /// `latestVersion` 严格高于 `currentVersion`。
    #[serde(default)]
    pub has_update: bool,
    #[serde(default)]
    pub install_kind: UpdateInstallKind,
    /// 为空表示可一键「更新并重启」。
    #[serde(default)]
    pub manual_reason: Option<UpdateManualReason>,
    /// 选中的发布资产文件名与大小（字节）。
    #[serde(default)]
    pub asset_name: String,
    #[serde(default)]
    pub asset_size: u64,
    #[serde(default)]
    pub downloaded_bytes: u64,
    /// 下载完成后立即安装（用户已点「更新并重启」，包仍在下载）。
    #[serde(default)]
    pub install_pending: bool,
    /// 手动升级用的资产直链（官方分发域）；无可用资产时为空串。
    #[serde(default)]
    pub download_url: String,
    #[serde(default)]
    pub release_page_url: String,
    /// 比当前版本新的更新说明（最新在前）。
    #[serde(default)]
    pub notes: Vec<ReleaseNoteDto>,
    #[serde(default)]
    pub failure: Option<UpdateFailure>,
    /// 失败的技术细节（英文，供诊断展示），无失败时为空串。
    #[serde(default)]
    pub error_detail: String,
    /// 最近一次成功检查的 Unix 毫秒时间戳；0 = 本次运行尚未检查成功。
    #[serde(default)]
    pub checked_at_ms: u64,
}

/// 偏好键：自定义分类列表（JSON 字符串或数组，与 Flutter `custom_categories` 同形）。
pub const CUSTOM_CATEGORIES_PREF_KEY: &str = "custom_categories";

/// 自定义分类（保留旧客户端持久化的 JSON 形状）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CustomCategoryDto {
    pub id: String,
    pub name: String,
    #[serde(default = "default_category_icon")]
    pub icon: String,
    /// `extension` | `regex` | `url`。
    #[serde(default = "default_category_match_mode")]
    pub match_mode: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub regex_pattern: String,
    /// `url` 模式的通配符模式（`*` 任意串、`?` 单字符、不分大小写、完整匹配整个网址）。
    #[serde(default)]
    pub url_pattern: String,
    #[serde(default)]
    pub position: i64,
    #[serde(default = "default_category_visible")]
    pub visible: bool,
    #[serde(default)]
    pub is_builtin: bool,
    #[serde(default)]
    pub builtin_type: Option<String>,
    #[serde(default)]
    pub save_dir: String,
}

fn default_category_icon() -> String {
    "file".to_owned()
}
fn default_category_match_mode() -> String {
    "extension".to_owned()
}
fn default_category_visible() -> bool {
    true
}

impl CustomCategoryDto {
    /// 内置分类基线（原生 Android `Category.BUILTIN` / iOS `TaskCategory.builtin` 同序同扩展名镜像）。
    #[must_use]
    pub fn builtin_defaults() -> Vec<Self> {
        let make = |id: &str, icon: &str, exts: &[&str], position: i64| Self {
            id: format!("builtin_{id}"),
            name: String::new(),
            icon: icon.to_owned(),
            match_mode: "extension".to_owned(),
            extensions: exts.iter().map(|ext| (*ext).to_owned()).collect(),
            regex_pattern: String::new(),
            url_pattern: String::new(),
            position,
            visible: true,
            is_builtin: true,
            builtin_type: Some(id.to_owned()),
            save_dir: String::new(),
        };
        vec![
            make("all", "folders", &[], 0),
            make(
                "video",
                "film",
                &[
                    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "m3u8",
                ],
                1,
            ),
            make(
                "audio",
                "music",
                &["mp3", "flac", "wav", "aac", "ogg", "m4a", "wma", "opus"],
                2,
            ),
            make(
                "document",
                "fileText",
                &[
                    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "epub", "md",
                ],
                3,
            ),
            make(
                "image",
                "image",
                &[
                    "jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "heic", "avif",
                ],
                4,
            ),
            make(
                "program",
                "cpu",
                &["exe", "msi", "dmg", "pkg", "deb", "rpm", "apk", "appimage"],
                5,
            ),
            make(
                "archive",
                "archive",
                &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso"],
                6,
            ),
            make("other", "file", &[], 7),
        ]
    }

    /// 从偏好值解析分类列表（JSON 字符串或数组）；空 / 损坏时回退内置基线，按 `position` 排序。
    #[must_use]
    pub fn from_preference(value: Option<&serde_json::Value>) -> Vec<Self> {
        let parsed = match value {
            Some(serde_json::Value::String(text)) => serde_json::from_str::<Vec<Self>>(text).ok(),
            Some(value) => serde_json::from_value::<Vec<Self>>(value.clone()).ok(),
            None => None,
        };
        let mut list = parsed
            .filter(|list| !list.is_empty())
            .unwrap_or_else(Self::builtin_defaults);
        list.sort_by_key(|entry| entry.position);
        list
    }
}

/// 分类保存目录路由：外部捕获未指定保存目录时，agent 把 `custom_categories` 中与落点
/// 有关的规则随 `daemon.task.create` 交给 daemon；引擎建任务时按初始名定初始目录，
/// 下载完成、最终文件名确定后（插件改名、按响应推断的名字、HLS 转封装后的 `.mp4`）
/// 再按同一规则复核并把产物移入命中分类的目录。任务组成员与 BT / eD2K 任务不参与。
///
/// 匹配语义（唯一事实源为 agent `category_dir.rs` 与引擎 `category_route.rs`，两端共用
/// 夹具 `native/engine/src/fixtures/category_route_cases.json`）：网址规则固定优先于
/// 扩展名 / 正则分类（不看列表位置），网址规则之间按列表顺序取首个配置了目录且命中的；
/// 未被网址规则认领的任务再按文件名规则取首个配置了目录且命中的普通分类；都不命中时
/// 取「其他」的目录。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CategoryRouteDto {
    /// 可见普通分类（按 `position`），只保留会影响落点的条目。
    #[serde(default)]
    pub rules: Vec<CategoryRuleDto>,
    /// 「其他」分类的目录；空 = 不兜底。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub other_dir: String,
    /// Visible builtin category identifiers mapped to configured save directories at submission.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub archive_categories: std::collections::BTreeMap<String, String>,
}

/// [`CategoryRouteDto`] 的单条普通分类规则。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CategoryRuleDto {
    /// `extension` | `regex` | `url`。
    #[serde(default = "default_category_match_mode")]
    pub match_mode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub regex_pattern: String,
    /// `url` 模式的通配符模式；语义与 [`CustomCategoryDto::url_pattern`] 一致。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url_pattern: String,
    /// 空 = 该分类不改变落点，只用于阻止「其他」兜底。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub save_dir: String,
}

impl CategoryRouteDto {
    /// 由分类列表（已按 `position` 排序）构建已授权路由；即使没有分类规则也返回空规则路由，
    /// 以便宿主在默认保存目录任务上运行插件归档分类。
    #[must_use]
    pub fn from_categories(categories: &[CustomCategoryDto]) -> Option<Self> {
        let visible = || categories.iter().filter(|category| category.visible);
        let other_dir = visible()
            .find(|category| category.builtin_type.as_deref() == Some("other"))
            .map(|other| other.save_dir.trim().to_owned())
            .unwrap_or_default();
        let rules = visible()
            .filter(|category| {
                !matches!(category.builtin_type.as_deref(), Some("all" | "other"))
                    && (!other_dir.is_empty() || !category.save_dir.trim().is_empty())
            })
            .map(|category| CategoryRuleDto {
                match_mode: category.match_mode.clone(),
                extensions: category.extensions.clone(),
                regex_pattern: category.regex_pattern.clone(),
                url_pattern: category.url_pattern.clone(),
                save_dir: category.save_dir.trim().to_owned(),
            })
            .collect::<Vec<_>>();
        let archive_categories = visible()
            .filter_map(|category| {
                let kind = category.builtin_type.as_deref()?;
                let dir = category.save_dir.trim();
                (kind != "all" && !dir.is_empty()).then(|| (kind.to_owned(), dir.to_owned()))
            })
            .collect();
        Some(Self {
            rules,
            other_dir,
            archive_categories,
        })
    }

    /// 由 `custom_categories` 偏好值构建路由；缺失或无法解析时也返回空规则路由，
    /// 以授权默认保存目录任务使用插件归档分类。
    #[must_use]
    pub fn from_preference(value: Option<&serde_json::Value>) -> Option<Self> {
        Self::from_categories(&CustomCategoryDto::from_preference(value))
    }

    /// 表单提交的保存目录 `submitted` 是否仍是系统给出的、应继续按分类路由：为空、等于
    /// 非空的表单初始目录 `offered`（预填分类之前显示的目录），或等于「其他」 / 任一分类
    /// 规则的目录。用户改选了别的目录即返回 `false`，以用户选择为准。认任一分类目录而不只
    /// 是命中的那个：多条链接并入同一张表单时目录框只预填第一条命中的分类目录。
    ///
    /// 两端都先去首尾空白再比较。
    #[must_use]
    pub fn follows(&self, submitted: &str, offered: &str) -> bool {
        let submitted = submitted.trim();
        if submitted.is_empty() {
            return true;
        }
        let offered = offered.trim();
        (!offered.is_empty() && submitted == offered) || self.is_category_dir(submitted)
    }

    /// 跟随路由时作为 `request.saveDir` 发送的基准目录（没有分类命中时的落点）：提交目录
    /// 为空或本身是分类目录时取表单初始目录 `offered`，否则（等于 `offered` 或用户
    /// 填的其他目录）取提交目录。结果已去首尾空白。
    #[must_use]
    pub fn base_dir(&self, submitted: &str, offered: &str) -> String {
        let submitted = submitted.trim();
        if submitted.is_empty() || self.is_category_dir(submitted) {
            offered.trim().to_owned()
        } else {
            submitted.to_owned()
        }
    }

    /// `dir`（已去空白）是否是「其他」或某条规则配置的分类目录；空串恒为 `false`。
    #[must_use]
    pub fn is_category_dir(&self, dir: &str) -> bool {
        !dir.is_empty()
            && (self.other_dir == dir || self.rules.iter().any(|rule| rule.save_dir == dir))
    }
}

#[cfg(test)]
mod category_route_helper_tests {
    use serde_json::json;

    use super::{CategoryRouteDto, CategoryRuleDto, CustomCategoryDto};

    fn route() -> CategoryRouteDto {
        CategoryRouteDto {
            rules: vec![CategoryRuleDto {
                match_mode: "extension".to_owned(),
                extensions: vec!["jpg".to_owned()],
                regex_pattern: String::new(),
                url_pattern: String::new(),
                save_dir: "/pictures".to_owned(),
            }],
            other_dir: "/other".to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn category_follows_accepts_system_given_dirs_only() {
        let route = route();
        assert!(route.follows("", "/default"));
        assert!(route.follows("  ", ""));
        assert!(route.follows(" /default ", " /default "));
        assert!(route.follows("/pictures", "/default"));
        assert!(route.follows("/other", ""));
        assert!(!route.follows("/elsewhere", "/default"));
        // 空的 offered 不等同于空提交之外的任何目录。
        assert!(!route.follows("/default", ""));
    }

    #[test]
    fn category_base_dir_falls_back_to_offered_unless_user_chose() {
        let route = route();
        assert_eq!(route.base_dir("", " /default "), "/default");
        assert_eq!(route.base_dir("/pictures", "/default"), "/default");
        assert_eq!(route.base_dir("/other", "/default"), "/default");
        assert_eq!(route.base_dir(" /default ", "/default"), "/default");
        assert_eq!(route.base_dir("/elsewhere", "/default"), "/elsewhere");
        assert_eq!(route.base_dir("", ""), "");
    }

    #[test]
    fn category_from_preference_reads_custom_categories_value() {
        assert_eq!(
            CategoryRouteDto::from_preference(None),
            Some(CategoryRouteDto::default())
        );
        let mut categories = CustomCategoryDto::builtin_defaults();
        for category in &mut categories {
            if category.builtin_type.as_deref() == Some("other") {
                category.save_dir = "/other".to_owned();
            }
        }
        let value = serde_json::to_value(&categories).expect("serialize categories");
        let route = CategoryRouteDto::from_preference(Some(&value)).expect("route");
        assert_eq!(route.other_dir, "/other");
        let as_text = json!(value.to_string());
        assert_eq!(
            CategoryRouteDto::from_preference(Some(&as_text)),
            Some(route)
        );
    }
    #[test]
    fn archive_category_routes_follow_stable_type_not_display_name() {
        let mut categories = CustomCategoryDto::builtin_defaults();
        let image = categories
            .iter_mut()
            .find(|category| category.builtin_type.as_deref() == Some("image"))
            .expect("image baseline");
        image.name = "摄影收藏".to_owned();
        image.save_dir = " /photos ".to_owned();
        let route = CategoryRouteDto::from_categories(&categories).expect("route");
        assert_eq!(
            route.archive_categories.get("image").map(String::as_str),
            Some("/photos")
        );
        let image = categories
            .iter_mut()
            .find(|category| category.builtin_type.as_deref() == Some("image"))
            .expect("image baseline");
        image.save_dir = "/new-photos".to_owned();
        let updated = CategoryRouteDto::from_categories(&categories).expect("updated route");
        assert_eq!(
            updated.archive_categories.get("image").map(String::as_str),
            Some("/new-photos")
        );
        assert_eq!(
            route.archive_categories.get("image").map(String::as_str),
            Some("/photos")
        );
        categories
            .iter_mut()
            .for_each(|category| category.visible = false);
        assert!(
            CategoryRouteDto::from_categories(&categories)
                .expect("hidden route")
                .archive_categories
                .is_empty()
        );
    }
}

#[cfg(test)]
mod capture_dto_tests {
    use serde_json::json;

    use super::{CaptureResolveParams, CaptureSubmitTorrentFileParams, PendingCaptureDto};

    #[test]
    fn resolve_params_without_request_field_deserializes() {
        let params: CaptureResolveParams = serde_json::from_value(json!({
            "transactionId": "tx-1",
            "accepted": true,
        }))
        .expect("deserialize without request");
        assert_eq!(params.transaction_id, "tx-1");
        assert!(params.accepted);
        assert!(!params.disable_category_route);
        assert!(params.request.is_none());
    }

    #[test]
    fn torrent_file_params_keep_legacy_shape_and_carry_form_options() {
        let legacy: CaptureSubmitTorrentFileParams = serde_json::from_value(json!({
            "path": "/tmp/a.torrent",
            "silent": true,
            "association": "torrent",
        }))
        .expect("legacy caller without form options");
        assert!(legacy.silent);
        assert_eq!(legacy.save_dir, None);
        assert_eq!(legacy.queue_id, None);
        assert_eq!(legacy.start_paused, None);
        assert_eq!(
            serde_json::to_value(&legacy).expect("serialize"),
            json!({ "path": "/tmp/a.torrent", "silent": true })
        );

        let form: CaptureSubmitTorrentFileParams = serde_json::from_value(json!({
            "path": "/tmp/a.torrent",
            "silent": false,
            "saveDir": "/data",
            "queueId": "q1",
            "startPaused": true,
        }))
        .expect("form caller");
        assert!(!form.silent);
        assert_eq!(form.save_dir.as_deref(), Some("/data"));
        assert_eq!(form.queue_id.as_deref(), Some("q1"));
        assert_eq!(form.start_paused, Some(true));
    }

    #[test]
    fn pending_capture_without_optional_fields_deserializes_with_defaults() {
        let capture: PendingCaptureDto = serde_json::from_value(json!({
            "transactionId": "tx-2",
            "url": "https://example.com/a.bin",
            "fileName": "a.bin",
            "createdAtUnixMs": 1_700_000_000_000_i64,
        }))
        .expect("deserialize legacy payload without optional fields");
        assert_eq!(capture.transaction_id, "tx-2");
        assert_eq!(capture.file_size, 0);
        assert_eq!(capture.referrer, "");
        assert!(!capture.has_cookies);
        assert!(!capture.has_authorization());
    }

    #[test]
    fn authorization_header_name_matches_case_insensitively() {
        let capture: PendingCaptureDto = serde_json::from_value(json!({
            "transactionId": "tx-3",
            "url": "https://example.com/a.bin",
            "createdAtUnixMs": 0,
            "headerNames": ["accept", "AUTHORIZATION"],
        }))
        .expect("deserialize capture with header names");
        assert!(capture.has_authorization());
    }
}

#[cfg(test)]
mod cloud_profile_tests {
    use serde_json::json;

    use super::CloudProfile;

    /// FluxCloud `GET /me` 把用户字段平铺在顶层。
    #[test]
    fn profile_parses_flat_me_payload() {
        let profile: CloudProfile = serde_json::from_value(json!({
            "id": "u1",
            "email": "user@example.com",
            "nickname": "User",
            "plan": "founder",
            "originId": 88888888,
            "membershipOrdinal": 18,
            "entitlements": { "originIdEdit": true },
            "currentPlan": { "code": "founder", "name": "创始会员", "badge": "创始会员" },
            "purchaseCreditMinor": 0
        }))
        .expect("flat /me payload");
        assert_eq!(profile.user.email, "user@example.com");
        assert_eq!(profile.user.origin_id, Some(88888888));
        assert_eq!(
            profile.current_plan.as_ref().map(|plan| plan.code.as_str()),
            Some("founder")
        );
    }
}

#[cfg(test)]
mod remote_dto_tests {
    use serde_json::json;

    use super::{PathStyle, RemoteTaskDto, RemoteTaskStatus};

    /// FluxCloud 对未指定文件名的任务回 `"fileName": null`；解析失败会让整批远程任务同步停摆。
    #[test]
    fn remote_task_accepts_null_file_name_and_unknown_status() {
        let task: RemoteTaskDto = serde_json::from_value(json!({
            "id": "t1",
            "fromDevice": "a",
            "toDevice": "b",
            "url": "https://example.com/a.bin",
            "saveDir": null,
            "fileName": null,
            "status": "archived",
            "totalBytes": null,
            "downloadedBytes": 0,
            "speed": 0,
            "progress": 0.0,
            "error": null,
            "createdAt": "2026-09-29T00:00:00Z",
            "updatedAt": "2026-09-29T00:00:00Z"
        }))
        .expect("null fileName must parse");
        assert_eq!(task.file_name, "");
        assert_eq!(task.status, RemoteTaskStatus::Unknown);
    }

    #[test]
    fn path_style_absolute_rules() {
        assert!(PathStyle::Windows.is_absolute(r"C:\Downloads"));
        assert!(PathStyle::Windows.is_absolute("d:/data"));
        assert!(PathStyle::Windows.is_absolute(r"\\nas\share"));
        assert!(!PathStyle::Windows.is_absolute("/mnt/data"));
        assert!(!PathStyle::Windows.is_absolute("Downloads"));
        assert!(PathStyle::Posix.is_absolute("/mnt/Download"));
        assert!(!PathStyle::Posix.is_absolute(r"E:\download"));
        assert_eq!(PathStyle::from_platform("macos"), Some(PathStyle::Posix));
        assert_eq!(
            PathStyle::from_platform("windows"),
            Some(PathStyle::Windows)
        );
        assert_eq!(PathStyle::from_platform("web"), None);
    }
}
