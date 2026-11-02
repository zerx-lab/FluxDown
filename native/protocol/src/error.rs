//! 稳定的本机服务应用错误契约。

use serde::{Deserialize, Serialize};

/// JSON-RPC 解析错误。
pub const PARSE_ERROR_CODE: i32 = -32700;
/// JSON-RPC 无效请求错误。
pub const INVALID_REQUEST_CODE: i32 = -32600;
/// JSON-RPC 方法不存在错误。
pub const METHOD_NOT_FOUND_CODE: i32 = -32601;
/// JSON-RPC 参数无效错误。
pub const INVALID_PARAMS_CODE: i32 = -32602;
/// JSON-RPC 内部错误。
pub const INTERNAL_ERROR_CODE: i32 = -32603;
/// FluxDown 应用错误使用的 JSON-RPC code。
pub const APPLICATION_ERROR_CODE: i32 = -32000;

/// 稳定应用错误码。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum ApplicationErrorCode {
    ProtocolIncompatible,
    Unauthorized,
    InvalidArgument,
    NotFound,
    Conflict,
    Unavailable,
    Timeout,
    Cancelled,
    Unsupported,
    Internal,
}

/// 比 [`ApplicationErrorCode`] 更细的稳定失败原因，供客户端给出可操作的本地化文案。
///
/// 同一个错误码可能对应性质完全不同的失败（如「市场镜像失效」与「本机服务断开」
/// 都属 `unavailable`），客户端优先按原因展示，缺失时再回退到按码的通用文案。
/// 未识别的原因解析为 [`ErrorReason::Unknown`]，新增变体不破坏旧客户端。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum ErrorReason {
    /// 无法连接插件市场索引源（网络 / 代理 / DNS）。
    MarketUnreachable,
    /// 插件市场索引内容无法解析或超出体积上限。
    MarketIndexInvalid,
    /// 插件市场索引序号低于本地已见高水位（可能被回滚或篡改）。
    MarketIndexRollback,
    /// 市场索引中没有该插件。
    PluginNotInMarket,
    /// 该插件版本已被发布者撤回。
    PluginYanked,
    /// 插件包的全部下载地址均不可用或内容校验不符。
    PluginDownloadFailed,
    /// 插件包超过体积上限。
    PluginPackageTooLarge,
    /// 插件包 / 插件目录未通过 manifest、脚本或版本门槛校验。
    PluginPackageInvalid,
    /// 市场安装：最新可装版本与用户确认时看到的版本不一致（需刷新目录后重新确认权限）。
    MarketVersionChanged,
    /// FluxCloud：账号或密码错误。
    InvalidCredentials,
    /// FluxCloud：邮箱验证码错误或已过期。
    InvalidVerificationCode,
    /// FluxCloud：修改密码时提供的当前密码不正确。
    WrongPassword,
    /// FluxCloud：请求过于频繁（发码 / 登录 / 校验限流）。
    RateLimited,
    /// FluxCloud：邮箱已被注册。
    EmailTaken,
    /// FluxCloud：目标 Origin ID 已被占用。
    OriginIdTaken,
    /// FluxCloud：当前权益不允许自助修改 Origin ID。
    OriginIdChangeNotAllowed,
    /// FluxCloud：Origin ID 的一次修改机会已使用。
    OriginIdAlreadyChanged,
    /// FluxCloud：账号已被停用。
    AccountDisabled,
    /// FluxCloud：服务端关闭了注册。
    RegistrationClosed,
    /// FluxCloud：账号注册未完成（需先完成邮箱验证）。
    RegistrationIncomplete,
    /// FluxCloud：服务端未配置邮件发送，无法下发验证码。
    MailNotConfigured,
    /// FluxCloud：账号设备数已达套餐上限。
    DeviceLimit,
    /// FluxCloud：账号同时在线设备数已达套餐上限（本设备保持登录，可在线列表中手动下线其他设备）。
    OnlineDeviceLimit,
    /// FluxCloud：本设备不在账号的受信任设备中（已被移除 / 被替换）。
    DeviceUntrusted,
    /// FluxCloud：登录会话已失效（被撤销 / 刷新令牌过期）。
    SessionExpired,
    /// FluxCloud：账号密码已被修改或重置，本设备会话随之失效。
    PasswordChanged,
    /// FluxCloud：无法连接云服务（网络 / DNS / TLS / 代理）。
    CloudUnreachable,
    /// 远程任务：目标设备当前离线，指令无法送达。
    TargetDeviceOffline,
    /// 远程任务：任务当前状态不允许该操作（如已结束）。
    TaskStateConflict,
    /// 远程任务：只有目标设备可以上报该任务状态。
    TaskDeviceMismatch,
    /// 保存目录在目标设备上不可用（路径风格不符 / 非绝对路径 / 无法创建）。
    SaveDirUnavailable,
    /// 局域网配对：配对码错误或已过期。
    PairingCodeInvalid,
    /// 局域网配对：配对会话不存在或已过期。
    PairingSessionExpired,
    /// 局域网配对：对端用户在核验窗口内没有表态（等待超时）。
    PairingTimeout,
    /// 局域网配对：无法连接对端地址。
    PairingPeerUnreachable,
    /// 局域网配对：对端地址不是 FluxDown 服务（如反代返回了非 FluxDown 响应、协议 / 端口不符）。
    PairingNotFluxDown,
    /// 局域网配对：失败次数过多，对端暂时拒绝配对请求。
    PairingThrottled,
    /// 局域网配对：对端拒绝了配对。
    PairingRejected,
    /// 局域网配对：对端身份签名校验失败。
    PairingSignatureInvalid,
    /// 局域网配对：不能与自身配对。
    PairingSelf,
    /// 局域网配对：对端配对协议版本不兼容（含旧版对端），两端需升级到同一版本。
    PairingVersionMismatch,
    /// 局域网互联：目标设备未配对或已解除配对。
    PeerNotPaired,
    /// 局域网互联：已配对设备当前不可达。
    PeerOffline,
    /// 局域网互联：对端 FluxDown 版本过旧，数据面链路协议不兼容（两端需升级）。
    PeerOutdated,
    /// 局域网互联：对端拒绝了本机请求，因为两台设备的系统时钟偏差超过容忍窗口。
    PeerClockSkew,
    /// 局域网互联：本机直连服务未启动（初始化失败或宿主不支持）。
    LinkUnavailable,
    /// Doctor 修复：用户取消了系统管理员授权对话框（UAC / polkit / macOS 授权），未做任何更改。
    ElevationCancelled,
    /// Doctor 修复：无法请求管理员授权（缺 `pkexec` / polkit 认证代理、无桌面会话等）。
    ElevationUnavailable,
    /// Doctor 修复：FluxDown 自身正以 root 身份运行，无法把权限还给普通用户。
    RunningElevated,
    /// Doctor 修复：修复步骤已执行，但重新探测仍未通过（noexec 挂载、安全软件、组织策略等）。
    RepairIncomplete,
    /// Doctor 修复：目标不在自动修复范围内（系统目录、家目录本身、非权限类错误），需用户手动处理。
    RepairNotApplicable,
    /// API 服务切换：请求的新监听端口已被占用，原服务与配置保持不变。
    GatewayPortInUse,
    /// API 服务切换：新服务未通过启动验证或配置无法保存，已保留原服务。
    GatewayRestartFailed,
    /// 云端推送：渠道数已达套餐上限。
    NotifyChannelLimit,
    /// 云端推送：当前套餐（含用户级覆盖）未开通云端推送。
    NotifyDisabled,
    /// 云端推送：渠道配置无效（Device Key / 主题 / 机器人地址不合法，或该种类暂不可用）。
    NotifyTargetInvalid,
    /// 提示音试听：文件缺失、格式不受支持或系统无可用播放器。
    SoundUnplayable,
    /// 套餐购买：云端未开放支付。
    PaymentDisabled,
    /// 套餐购买：支付网关建单失败（可稍后重试）。
    PaymentGatewayFailed,
    /// 套餐购买：目标套餐已下架或不可购买。
    PlanNotPurchasable,
    /// 套餐购买：已持有该套餐。
    PlanAlreadyOwned,
    /// 套餐购买：已付金额不低于目标套餐价格（买断制差价升级不成立）。
    PlanNotUpgrade,
    /// 套餐购买：已持有同档或更高档套餐（买断制只升不降）。
    PlanTierNotHigher,
    /// 套餐购买：已有另一张生效中的周期会员卡。
    SubscriptionConflict,
    /// 推介：邀请码无效（不存在 / 功能未开启）。
    ReferralCodeInvalid,
    /// 推介：不能使用自己的邀请码。
    ReferralSelfUse,
    /// 推介：本账号已使用过邀请码（每人终生一次）。
    ReferralAlreadyUsed,
    /// 推介：目标套餐不参与推介。
    ReferralPlanExcluded,
    /// 推介：自定义推荐码已被占用。
    ReferralCodeTaken,
    /// 对端发送了本端不认识的原因。
    #[serde(other)]
    Unknown,
}

/// FluxDown 应用错误的机器可读详情。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RpcErrorData {
    pub code: ApplicationErrorCode,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ErrorReason>,
}

impl RpcErrorData {
    /// 创建不带字段或版本上下文的错误详情。
    #[must_use]
    pub const fn new(code: ApplicationErrorCode, retryable: bool) -> Self {
        Self {
            code,
            retryable,
            field: None,
            revision: None,
            reason: None,
        }
    }

    /// 附加细分失败原因。
    #[must_use]
    pub const fn with_reason(mut self, reason: ErrorReason) -> Self {
        self.reason = Some(reason);
        self
    }
}

/// JSON-RPC 2.0 错误对象。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RpcErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<RpcErrorData>,
}

impl RpcErrorObject {
    /// 创建带稳定 FluxDown 应用错误详情的错误对象。
    #[must_use]
    pub fn application(message: impl Into<String>, data: RpcErrorData) -> Self {
        Self {
            code: APPLICATION_ERROR_CODE,
            message: message.into(),
            data: Some(data),
        }
    }

    /// 创建不携带应用数据的 JSON 解析错误。
    #[must_use]
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self {
            code: PARSE_ERROR_CODE,
            message: message.into(),
            data: None,
        }
    }

    /// 创建不携带应用数据的无效请求错误。
    #[must_use]
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: INVALID_REQUEST_CODE,
            message: message.into(),
            data: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ApplicationErrorCode, ErrorReason, RpcErrorData};

    #[test]
    fn reason_is_optional_and_tolerates_unknown_values() {
        let legacy: RpcErrorData =
            serde_json::from_value(json!({ "code": "unavailable", "retryable": true }))
                .expect("error data without reason stays valid");
        assert_eq!(legacy.reason, None);

        let future: RpcErrorData = serde_json::from_value(json!({
            "code": "unavailable",
            "retryable": true,
            "reason": "someFutureReason"
        }))
        .expect("unknown reason must not reject the whole error");
        assert_eq!(future.reason, Some(ErrorReason::Unknown));

        let data = RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
            .with_reason(ErrorReason::PluginDownloadFailed);
        assert_eq!(
            serde_json::to_value(&data).expect("serialize"),
            json!({ "code": "unavailable", "retryable": true, "reason": "pluginDownloadFailed" })
        );
    }
}
