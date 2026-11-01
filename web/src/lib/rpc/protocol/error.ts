// 稳定应用错误契约（native/protocol/src/error.rs）。

export const PARSE_ERROR_CODE = -32700;
export const INVALID_REQUEST_CODE = -32600;
export const METHOD_NOT_FOUND_CODE = -32601;
export const INVALID_PARAMS_CODE = -32602;
export const INTERNAL_ERROR_CODE = -32603;
/** FluxDown 应用错误使用的 JSON-RPC code；细节在 `error.data`（RpcErrorData）。 */
export const APPLICATION_ERROR_CODE = -32000;

/** 稳定应用错误码（`ApplicationErrorCode`，camelCase）。 */
export const APPLICATION_ERROR_CODES = {
  protocolIncompatible: 'protocolIncompatible',
  unauthorized: 'unauthorized',
  invalidArgument: 'invalidArgument',
  notFound: 'notFound',
  conflict: 'conflict',
  unavailable: 'unavailable',
  timeout: 'timeout',
  cancelled: 'cancelled',
  unsupported: 'unsupported',
  internal: 'internal',
} as const;
export type ApplicationErrorCode =
  (typeof APPLICATION_ERROR_CODES)[keyof typeof APPLICATION_ERROR_CODES];

/**
 * 比错误码更细的失败原因（`ErrorReason`）。对端发来本端不认识的原因时 Rust 端解析为 `unknown`，
 * 前端同样应对未列出的字符串回退到按码的通用文案。
 */
export type ErrorReason =
  | 'marketUnreachable'
  | 'marketIndexInvalid'
  | 'marketIndexRollback'
  | 'pluginNotInMarket'
  | 'pluginYanked'
  | 'pluginDownloadFailed'
  | 'pluginPackageTooLarge'
  | 'pluginPackageInvalid'
  | 'marketVersionChanged'
  | 'invalidCredentials'
  | 'invalidVerificationCode'
  | 'wrongPassword'
  | 'rateLimited'
  | 'emailTaken'
  | 'originIdTaken'
  | 'originIdChangeNotAllowed'
  | 'originIdAlreadyChanged'
  | 'accountDisabled'
  | 'registrationClosed'
  | 'registrationIncomplete'
  | 'mailNotConfigured'
  | 'deviceLimit'
  | 'onlineDeviceLimit'
  | 'deviceUntrusted'
  | 'sessionExpired'
  | 'passwordChanged'
  | 'cloudUnreachable'
  | 'targetDeviceOffline'
  | 'taskStateConflict'
  | 'taskDeviceMismatch'
  | 'saveDirUnavailable'
  | 'pairingCodeInvalid'
  | 'pairingSessionExpired'
  | 'pairingTimeout'
  | 'peerClockSkew'
  | 'pairingPeerUnreachable'
  | 'pairingNotFluxDown'
  | 'pairingThrottled'
  | 'pairingRejected'
  | 'pairingSignatureInvalid'
  | 'pairingSelf'
  | 'pairingVersionMismatch'
  | 'peerNotPaired'
  | 'peerOffline'
  | 'peerOutdated'
  | 'linkUnavailable'
  | 'elevationCancelled'
  | 'elevationUnavailable'
  | 'runningElevated'
  | 'repairIncomplete'
  | 'repairNotApplicable'
  | 'gatewayPortInUse'
  | 'gatewayRestartFailed'
  | 'notifyChannelLimit'
  | 'notifyDisabled'
  | 'notifyTargetInvalid'
  | 'soundUnplayable'
  | 'paymentDisabled'
  | 'paymentGatewayFailed'
  | 'planNotPurchasable'
  | 'planAlreadyOwned'
  | 'planNotUpgrade'
  | 'planTierNotHigher'
  | 'referralCodeInvalid'
  | 'referralSelfUse'
  | 'referralAlreadyUsed'
  | 'referralPlanExcluded'
  | 'referralCodeTaken'
  | 'subscriptionConflict'
  | 'unknown';

/** `error.data`：应用错误的机器可读详情。 */
export interface RpcErrorData {
  code: ApplicationErrorCode;
  retryable: boolean;
  /** 出错的参数字段名。 */
  field?: string;
  /** 乐观并发冲突时服务端当前版本（如 daemon.config.patch）。 */
  revision?: number;
  reason?: ErrorReason;
}

/** JSON-RPC 2.0 错误对象。`data` 仅应用错误（code = -32000）携带。 */
export interface RpcErrorObject {
  code: number;
  message: string;
  data?: RpcErrorData;
}
