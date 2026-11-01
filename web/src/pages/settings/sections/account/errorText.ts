// RPC 错误 → 可展示的 i18n 键。优先按 `error.reason`（稳定、细粒度），缺失时按应用错误码回退
// （通用映射与 GPUI `crates/account/src/lib.rs::error_text` 一致）。登录/注册/验证码场景不再猜测语义：
// agent 已把云端错误码归一化为 reason，`unauthorized` / `unsupported` 等不再承载特定含义。

import { RpcError } from '../../../../lib/rpc'
import type { ErrorReason } from '../../../../lib/rpc'

export type AccountErrorContext = 'generic' | 'login' | 'register' | 'code' | 'pairing'

/** reason → 文案键；未列出的 reason（含 `unknown` 与插件市场类）回退按码。 */
export const REASON_KEYS: Partial<Record<ErrorReason, string>> = {
  invalidCredentials: 'accountErrorInvalidCredentials',
  wrongPassword: 'accountErrorWrongPassword',
  invalidVerificationCode: 'accountErrorInvalidCode',
  rateLimited: 'accountErrorRateLimited',
  emailTaken: 'accountErrorEmailTaken',
  originIdTaken: 'accountOriginIdErrorTaken',
  originIdChangeNotAllowed: 'accountOriginIdErrorNotAllowed',
  originIdAlreadyChanged: 'accountOriginIdErrorAlreadyChanged',
  accountDisabled: 'accountErrorAccountDisabled',
  registrationClosed: 'accountErrorRegistrationClosed',
  registrationIncomplete: 'accountErrorRegistrationIncomplete',
  mailNotConfigured: 'errReasonMailNotConfigured',
  deviceLimit: 'accountErrorDeviceLimit',
  onlineDeviceLimit: 'cloudSyncErrorOnlineLimit',
  deviceUntrusted: 'cloudSyncErrorDeviceUntrusted',
  sessionExpired: 'errReasonSessionExpired',
  cloudUnreachable: 'accountErrorNetwork',
  targetDeviceOffline: 'errReasonTargetDeviceOffline',
  taskStateConflict: 'errReasonTaskStateConflict',
  taskDeviceMismatch: 'errReasonTaskDeviceMismatch',
  saveDirUnavailable: 'errReasonSaveDirUnavailable',
  pairingCodeInvalid: 'errReasonPairingCodeInvalid',
  pairingSessionExpired: 'errReasonPairingSessionExpired',
  pairingTimeout: 'errReasonPairingTimeout',
  peerClockSkew: 'errReasonPeerClockSkew',
  pairingPeerUnreachable: 'errReasonPairingPeerUnreachable',
  pairingNotFluxDown: 'errReasonPairingNotFluxDown',
  pairingThrottled: 'errReasonPairingThrottled',
  pairingRejected: 'errReasonPairingRejected',
  pairingSignatureInvalid: 'errReasonPairingSignatureInvalid',
  pairingSelf: 'errReasonPairingSelf',
  pairingVersionMismatch: 'errReasonPairingVersionMismatch',
  peerNotPaired: 'errReasonPeerNotPaired',
  peerOffline: 'errReasonPeerOffline',
  peerOutdated: 'errReasonPeerOutdated',
  linkUnavailable: 'errReasonLinkUnavailable',
  subscriptionConflict: 'errReasonSubscriptionConflict',
  paymentDisabled: 'accountPlanErrorPaymentDisabled',
  paymentGatewayFailed: 'accountPlanErrorGateway',
  planNotPurchasable: 'accountPlanErrorNotPurchasable',
  planAlreadyOwned: 'accountPlanErrorAlreadyOwned',
  planNotUpgrade: 'accountPlanErrorNotUpgrade',
  planTierNotHigher: 'accountPlanErrorTierNotHigher',
  referralCodeInvalid: 'accountPlanErrorReferralInvalid',
  referralSelfUse: 'accountPlanErrorReferralSelfUse',
  referralAlreadyUsed: 'accountPlanErrorReferralAlreadyUsed',
  referralPlanExcluded: 'accountPlanErrorReferralPlanExcluded',
  referralCodeTaken: 'accountReferralCodeTaken',
}

function codeKey(error: RpcError): string {
  switch (error.appCode) {
    case 'unavailable':
    case 'timeout':
      return 'localServiceDisconnected'
    case 'invalidArgument':
    case 'notFound':
      return 'localServiceInvalidArgument'
    case 'conflict':
      return 'localServiceConflict'
    case 'unsupported':
      return 'settingsUnsupportedOnPlatform'
    default:
      return 'localServiceActionFailed'
  }
}

/**
 * `context` 只在没有 reason（旧 agent / 非云端错误）时按码细化：
 * 登录 / 验证码场景的参数类错误对应「校验失败 / 验证码无效」。
 */
export function accountErrorKey(error: unknown, context: AccountErrorContext = 'generic'): string {
  if (!(error instanceof RpcError)) return 'accountErrorUnknown'
  const byReason = error.reason ? REASON_KEYS[error.reason] : undefined
  if (byReason) return byReason
  const code = error.appCode
  if (context === 'code' && (code === 'invalidArgument' || code === 'notFound')) return 'accountErrorInvalidCode'
  if (context === 'pairing' && code === 'invalidArgument') return 'localPairingAddressInvalid'
  if ((context === 'login' || context === 'register') && code === 'invalidArgument') return 'accountErrorValidation'
  return codeKey(error)
}
