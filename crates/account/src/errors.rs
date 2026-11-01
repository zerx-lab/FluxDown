//! RPC 错误 → 本地化文案键：优先按 `RpcErrorData.reason`，缺失再按 `code` 与调用上下文回退。
//!
//! 与 Web `errorText.ts` 同一套上下文（登录 / 注册 / 验证码），另加同步与局域网配对；
//! 登录上下文下 `Unauthorized` 表示凭据错误而不是「本地服务失败」。

use fluxdown_protocol::{ApplicationErrorCode, ErrorReason, RpcErrorData};
use fluxdown_ui_i18n::Translator;
use gpui::SharedString;

/// 错误出现的位置，决定按 `code` 回退时的措辞。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ErrorContext {
    General,
    Login,
    Register,
    /// 设备验证码 / 邮箱验证码校验。
    Code,
    /// 配置同步。
    Sync,
    /// 局域网直连配对与已配对设备操作。
    Pairing,
}

/// 服务端要求先完成注册验证：登录对话框据此引导到注册验证步骤。
pub(crate) fn is_registration_incomplete(error: &RpcErrorData) -> bool {
    match error.reason {
        Some(ErrorReason::RegistrationIncomplete) => true,
        // 旧 agent 不带 reason，云端 `registration_incomplete` 折成 `conflict`。
        None => error.code == ApplicationErrorCode::Conflict,
        Some(_) => false,
    }
}

pub(crate) fn error_key(error: &RpcErrorData, context: ErrorContext) -> &'static str {
    error
        .reason
        .and_then(|reason| reason_key(reason, context))
        .unwrap_or_else(|| code_key(error, context))
}

pub(crate) fn error_text(
    translator: &Translator,
    error: &RpcErrorData,
    context: ErrorContext,
) -> SharedString {
    crate::t(translator, error_key(error, context))
}

/// 同步状态的失败原因文案；`reason` 缺失时给出通用同步失败提示。
pub(crate) fn sync_reason_key(reason: Option<ErrorReason>) -> &'static str {
    reason
        .and_then(|reason| reason_key(reason, ErrorContext::Sync))
        .unwrap_or("cloudSyncErrorGeneric")
}

/// agent 通知会话被撤销时的一次性提示文案键。
pub(crate) fn session_revoked_key(reason: ErrorReason) -> &'static str {
    match reason {
        ErrorReason::DeviceUntrusted => "accountSessionRevokedUntrusted",
        ErrorReason::PasswordChanged => "accountSessionRevokedPasswordChanged",
        ErrorReason::AccountDisabled => "accountErrorAccountDisabled",
        _ => "accountSessionRevokedExpired",
    }
}

pub(crate) fn reason_key(reason: ErrorReason, context: ErrorContext) -> Option<&'static str> {
    Some(match reason {
        ErrorReason::InvalidCredentials => "accountErrorInvalidCredentials",
        ErrorReason::WrongPassword => "accountErrorWrongPassword",
        ErrorReason::PasswordChanged => "accountSessionRevokedPasswordChanged",
        ErrorReason::InvalidVerificationCode => "accountErrorInvalidCode",
        ErrorReason::RateLimited => "accountErrorRateLimited",
        ErrorReason::EmailTaken => "accountErrorEmailTaken",
        ErrorReason::OriginIdTaken => "accountOriginIdErrorTaken",
        ErrorReason::OriginIdChangeNotAllowed => "accountOriginIdErrorNotAllowed",
        ErrorReason::OriginIdAlreadyChanged => "accountOriginIdErrorAlreadyChanged",
        ErrorReason::AccountDisabled => "accountErrorAccountDisabled",
        ErrorReason::RegistrationClosed => "accountErrorRegistrationClosed",
        ErrorReason::RegistrationIncomplete => "accountErrorRegistrationIncomplete",
        ErrorReason::MailNotConfigured => "errReasonMailNotConfigured",
        ErrorReason::DeviceLimit => "accountErrorDeviceLimit",
        ErrorReason::OnlineDeviceLimit => "cloudSyncErrorOnlineLimit",
        ErrorReason::DeviceUntrusted => {
            if context == ErrorContext::Sync {
                "cloudSyncErrorDeviceUntrusted"
            } else {
                "accountErrorDeviceUntrusted"
            }
        }
        ErrorReason::SessionExpired => "errReasonSessionExpired",
        ErrorReason::CloudUnreachable => {
            if context == ErrorContext::Sync {
                "cloudSyncErrorNetwork"
            } else {
                "accountErrorNetwork"
            }
        }
        ErrorReason::PairingCodeInvalid => "errReasonPairingCodeInvalid",
        ErrorReason::PairingSessionExpired => "errReasonPairingSessionExpired",
        ErrorReason::PairingTimeout => "errReasonPairingTimeout",
        ErrorReason::PeerClockSkew => "errReasonPeerClockSkew",
        ErrorReason::PairingPeerUnreachable => "errReasonPairingPeerUnreachable",
        ErrorReason::PairingNotFluxDown => "errReasonPairingNotFluxDown",
        ErrorReason::PairingThrottled => "errReasonPairingThrottled",
        ErrorReason::PairingRejected => "errReasonPairingRejected",
        ErrorReason::PairingSignatureInvalid => "errReasonPairingSignatureInvalid",
        ErrorReason::PairingSelf => "errReasonPairingSelf",
        ErrorReason::PairingVersionMismatch => "errReasonPairingVersionMismatch",
        ErrorReason::PeerNotPaired => "errReasonPeerNotPaired",
        ErrorReason::PeerOffline => "errReasonPeerOffline",
        ErrorReason::PeerOutdated => "errReasonPeerOutdated",
        ErrorReason::LinkUnavailable => "errReasonLinkUnavailable",
        ErrorReason::TargetDeviceOffline => "errReasonTargetDeviceOffline",
        ErrorReason::TaskStateConflict => "errReasonTaskStateConflict",
        ErrorReason::TaskDeviceMismatch => "errReasonTaskDeviceMismatch",
        ErrorReason::SaveDirUnavailable => "errReasonSaveDirUnavailable",
        ErrorReason::PaymentDisabled
        | ErrorReason::PaymentGatewayFailed
        | ErrorReason::PlanNotPurchasable
        | ErrorReason::PlanAlreadyOwned
        | ErrorReason::PlanNotUpgrade
        | ErrorReason::PlanTierNotHigher
        | ErrorReason::SubscriptionConflict
        | ErrorReason::ReferralCodeInvalid
        | ErrorReason::ReferralSelfUse
        | ErrorReason::ReferralAlreadyUsed
        | ErrorReason::ReferralPlanExcluded
        | ErrorReason::ReferralCodeTaken => return crate::billing::billing_reason_key(reason),
        // 插件市场、Doctor、API 服务切换与云端推送由各自页面展示；这里退回按 code 的通用文案。
        ErrorReason::MarketUnreachable
        | ErrorReason::MarketIndexInvalid
        | ErrorReason::MarketIndexRollback
        | ErrorReason::PluginNotInMarket
        | ErrorReason::PluginYanked
        | ErrorReason::PluginDownloadFailed
        | ErrorReason::PluginPackageTooLarge
        | ErrorReason::PluginPackageInvalid
        | ErrorReason::MarketVersionChanged
        | ErrorReason::ElevationCancelled
        | ErrorReason::ElevationUnavailable
        | ErrorReason::RunningElevated
        | ErrorReason::RepairIncomplete
        | ErrorReason::RepairNotApplicable
        | ErrorReason::GatewayPortInUse
        | ErrorReason::GatewayRestartFailed
        | ErrorReason::NotifyChannelLimit
        | ErrorReason::NotifyDisabled
        | ErrorReason::NotifyTargetInvalid
        | ErrorReason::SoundUnplayable
        | ErrorReason::Unknown => return None,
    })
}

fn code_key(error: &RpcErrorData, context: ErrorContext) -> &'static str {
    use ApplicationErrorCode as Code;
    let network = error.code == Code::Unavailable && error.retryable;
    match context {
        ErrorContext::Login => match error.code {
            Code::Unauthorized => return "accountErrorInvalidCredentials",
            Code::InvalidArgument => return "accountErrorValidation",
            Code::Conflict => return "accountErrorRegistrationIncomplete",
            _ if network => return "accountErrorNetwork",
            _ => {}
        },
        ErrorContext::Register => match error.code {
            Code::Conflict => return "accountErrorEmailTaken",
            Code::InvalidArgument => return "accountErrorValidation",
            Code::Unsupported => return "accountErrorRegistrationClosed",
            _ if network => return "accountErrorNetwork",
            _ => {}
        },
        ErrorContext::Code => match error.code {
            Code::Unauthorized | Code::InvalidArgument | Code::NotFound => {
                return "accountErrorInvalidCode";
            }
            _ if network => return "accountErrorNetwork",
            _ => {}
        },
        ErrorContext::Sync => {
            if network {
                return "cloudSyncErrorNetwork";
            }
        }
        ErrorContext::Pairing => {
            // 不带 reason 的 Unavailable 是本机服务 / 远端主机断开：落到通用的「服务已断开」，
            // 真正的对端不可达由 `PairingPeerUnreachable` reason 表达。
            if error.code == Code::InvalidArgument {
                return "localPairingAddressInvalid";
            }
        }
        ErrorContext::General => {}
    }
    generic_key(error.code)
}

fn generic_key(code: ApplicationErrorCode) -> &'static str {
    match code {
        ApplicationErrorCode::Unavailable | ApplicationErrorCode::Timeout => {
            "localServiceDisconnected"
        }
        ApplicationErrorCode::InvalidArgument | ApplicationErrorCode::NotFound => {
            "localServiceInvalidArgument"
        }
        ApplicationErrorCode::Conflict => "localServiceConflict",
        ApplicationErrorCode::Unsupported => "settingsUnsupportedOnPlatform",
        ApplicationErrorCode::ProtocolIncompatible
        | ApplicationErrorCode::Unauthorized
        | ApplicationErrorCode::Cancelled
        | ApplicationErrorCode::Internal => "localServiceActionFailed",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use fluxdown_ui_i18n::I18nCatalog;

    use super::*;

    fn error(code: ApplicationErrorCode) -> RpcErrorData {
        RpcErrorData::new(code, false)
    }

    #[test]
    fn reason_takes_precedence_over_code() {
        let rate_limited = RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
            .with_reason(ErrorReason::RateLimited);
        assert_eq!(
            error_key(&rate_limited, ErrorContext::Login),
            "accountErrorRateLimited"
        );
        let limit = error(ApplicationErrorCode::Unauthorized).with_reason(ErrorReason::DeviceLimit);
        assert_eq!(
            error_key(&limit, ErrorContext::Login),
            "accountErrorDeviceLimit"
        );
    }

    #[test]
    fn unauthorized_in_login_context_is_wrong_credentials_not_service_failure() {
        let unauthorized = error(ApplicationErrorCode::Unauthorized);
        assert_eq!(
            error_key(&unauthorized, ErrorContext::Login),
            "accountErrorInvalidCredentials"
        );
        assert_eq!(
            error_key(&unauthorized, ErrorContext::General),
            "localServiceActionFailed"
        );
        assert_eq!(
            error_key(&unauthorized, ErrorContext::Code),
            "accountErrorInvalidCode"
        );
    }

    #[test]
    fn unknown_reason_falls_back_to_code() {
        let unknown = error(ApplicationErrorCode::Conflict).with_reason(ErrorReason::Unknown);
        assert_eq!(
            error_key(&unknown, ErrorContext::General),
            "localServiceConflict"
        );
        // 插件市场原因不属于账户页：同样按 code 回退。
        let market =
            error(ApplicationErrorCode::Conflict).with_reason(ErrorReason::MarketUnreachable);
        assert_eq!(
            error_key(&market, ErrorContext::General),
            "localServiceConflict"
        );
    }

    #[test]
    fn billing_reasons_use_plan_and_referral_keys() {
        let taken =
            error(ApplicationErrorCode::Conflict).with_reason(ErrorReason::ReferralCodeTaken);
        assert_eq!(
            error_key(&taken, ErrorContext::General),
            "accountReferralCodeTaken"
        );
        let gateway =
            error(ApplicationErrorCode::Unavailable).with_reason(ErrorReason::PaymentGatewayFailed);
        assert_eq!(
            error_key(&gateway, ErrorContext::General),
            "accountPlanErrorGateway"
        );
    }

    #[test]
    fn retryable_unavailable_is_network_in_auth_contexts() {
        let offline = RpcErrorData::new(ApplicationErrorCode::Unavailable, true);
        assert_eq!(
            error_key(&offline, ErrorContext::Register),
            "accountErrorNetwork"
        );
        assert_eq!(
            error_key(&offline, ErrorContext::Sync),
            "cloudSyncErrorNetwork"
        );
        assert_eq!(
            error_key(&offline, ErrorContext::General),
            "localServiceDisconnected"
        );
    }

    #[test]
    fn device_untrusted_wording_depends_on_context() {
        let untrusted =
            error(ApplicationErrorCode::Unauthorized).with_reason(ErrorReason::DeviceUntrusted);
        assert_eq!(
            error_key(&untrusted, ErrorContext::Sync),
            "cloudSyncErrorDeviceUntrusted"
        );
        assert_eq!(
            error_key(&untrusted, ErrorContext::General),
            "accountErrorDeviceUntrusted"
        );
    }

    #[test]
    fn registration_incomplete_is_detected_from_reason_or_legacy_conflict() {
        let modern =
            error(ApplicationErrorCode::Conflict).with_reason(ErrorReason::RegistrationIncomplete);
        assert!(is_registration_incomplete(&modern));
        assert!(is_registration_incomplete(&error(
            ApplicationErrorCode::Conflict
        )));
        let taken = error(ApplicationErrorCode::Conflict).with_reason(ErrorReason::EmailTaken);
        assert!(!is_registration_incomplete(&taken));
        assert!(!is_registration_incomplete(&error(
            ApplicationErrorCode::Unauthorized
        )));
    }

    #[test]
    fn sync_reason_without_mapping_uses_generic_sync_failure() {
        assert_eq!(sync_reason_key(None), "cloudSyncErrorGeneric");
        assert_eq!(
            sync_reason_key(Some(ErrorReason::OnlineDeviceLimit)),
            "cloudSyncErrorOnlineLimit"
        );
        assert_eq!(
            sync_reason_key(Some(ErrorReason::Unknown)),
            "cloudSyncErrorGeneric"
        );
    }

    #[test]
    fn link_unavailable_is_not_reported_as_peer_unreachable_in_pairing() {
        let unavailable = RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
            .with_reason(ErrorReason::LinkUnavailable);
        assert_eq!(
            error_key(&unavailable, ErrorContext::Pairing),
            "errReasonLinkUnavailable"
        );
        let outdated =
            error(ApplicationErrorCode::Unavailable).with_reason(ErrorReason::PeerOutdated);
        assert_eq!(
            error_key(&outdated, ErrorContext::General),
            "errReasonPeerOutdated"
        );
    }

    #[test]
    fn reasonless_unavailable_in_pairing_is_a_local_disconnect_not_peer_unreachable() {
        // 不带 reason 的 Unavailable 是本机服务 / 远端主机断开，不能说成「无法连接对端地址」。
        let offline = RpcErrorData::new(ApplicationErrorCode::Unavailable, true);
        assert_eq!(
            error_key(&offline, ErrorContext::Pairing),
            "localServiceDisconnected"
        );
        let unreachable = RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
            .with_reason(ErrorReason::PairingPeerUnreachable);
        assert_eq!(
            error_key(&unreachable, ErrorContext::Pairing),
            "errReasonPairingPeerUnreachable"
        );
    }

    #[test]
    fn revoked_reasons_pick_distinct_prompts_with_expiry_as_fallback() {
        assert_eq!(
            session_revoked_key(ErrorReason::DeviceUntrusted),
            "accountSessionRevokedUntrusted"
        );
        assert_eq!(
            session_revoked_key(ErrorReason::AccountDisabled),
            "accountErrorAccountDisabled"
        );
        assert_eq!(
            session_revoked_key(ErrorReason::SessionExpired),
            "accountSessionRevokedExpired"
        );
        assert_eq!(
            session_revoked_key(ErrorReason::Unknown),
            "accountSessionRevokedExpired"
        );
        assert_eq!(
            session_revoked_key(ErrorReason::PasswordChanged),
            "accountSessionRevokedPasswordChanged"
        );
    }

    #[test]
    fn every_mapped_key_exists_in_both_locales() {
        let catalog = Arc::new(I18nCatalog::load_embedded().expect("embedded catalog"));
        let reasons = [
            ErrorReason::InvalidCredentials,
            ErrorReason::InvalidVerificationCode,
            ErrorReason::WrongPassword,
            ErrorReason::RateLimited,
            ErrorReason::EmailTaken,
            ErrorReason::OriginIdTaken,
            ErrorReason::OriginIdChangeNotAllowed,
            ErrorReason::OriginIdAlreadyChanged,
            ErrorReason::AccountDisabled,
            ErrorReason::RegistrationClosed,
            ErrorReason::RegistrationIncomplete,
            ErrorReason::MailNotConfigured,
            ErrorReason::DeviceLimit,
            ErrorReason::OnlineDeviceLimit,
            ErrorReason::DeviceUntrusted,
            ErrorReason::SessionExpired,
            ErrorReason::CloudUnreachable,
            ErrorReason::PairingCodeInvalid,
            ErrorReason::PairingSessionExpired,
            ErrorReason::PairingTimeout,
            ErrorReason::PeerClockSkew,
            ErrorReason::PairingPeerUnreachable,
            ErrorReason::PairingNotFluxDown,
            ErrorReason::PairingThrottled,
            ErrorReason::PairingRejected,
            ErrorReason::PairingSignatureInvalid,
            ErrorReason::PairingSelf,
            ErrorReason::PairingVersionMismatch,
            ErrorReason::PeerNotPaired,
            ErrorReason::PeerOffline,
            ErrorReason::PeerOutdated,
            ErrorReason::LinkUnavailable,
            ErrorReason::TargetDeviceOffline,
            ErrorReason::TaskStateConflict,
            ErrorReason::TaskDeviceMismatch,
            ErrorReason::SaveDirUnavailable,
        ];
        let contexts = [
            ErrorContext::General,
            ErrorContext::Login,
            ErrorContext::Register,
            ErrorContext::Code,
            ErrorContext::Sync,
            ErrorContext::Pairing,
        ];
        let codes = [
            ApplicationErrorCode::Unauthorized,
            ApplicationErrorCode::InvalidArgument,
            ApplicationErrorCode::Conflict,
            ApplicationErrorCode::Unavailable,
            ApplicationErrorCode::Internal,
        ];
        let mut keys = Vec::new();
        for context in contexts {
            for reason in reasons {
                keys.push(error_key(
                    &error(ApplicationErrorCode::Internal).with_reason(reason),
                    context,
                ));
            }
            for code in codes {
                keys.push(error_key(&error(code), context));
                keys.push(error_key(&RpcErrorData::new(code, true), context));
            }
        }
        keys.push(sync_reason_key(None));
        for reason in [
            ErrorReason::DeviceUntrusted,
            ErrorReason::AccountDisabled,
            ErrorReason::SessionExpired,
            ErrorReason::PasswordChanged,
        ] {
            keys.push(session_revoked_key(reason));
        }
        for locale in ["en", "zh"] {
            let translator = catalog.translator(locale);
            for key in &keys {
                assert_ne!(
                    translator.text(key),
                    *key,
                    "missing i18n key {key} in {locale}"
                );
            }
        }
    }
}
