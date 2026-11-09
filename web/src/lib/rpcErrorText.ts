// RpcError → 可展示文案：按稳定应用错误码取 `localService*` 键，
// 不把传输层英文诊断串（如 `request timed out: daemon.task.pause`）直接上屏。

import type { TFunction } from '../i18n'
import { RpcError, UploadError } from './rpc'

export type RpcErrorKind = 'disconnected' | 'conflict' | 'invalidArgument' | 'failed'

/** 对应 assets/i18n 的既有键（同 GPUI `SettingsErrorKind::i18n_key`）。 */
export const RPC_ERROR_KEYS: Readonly<Record<RpcErrorKind, string>> = {
  disconnected: 'localServiceDisconnected',
  conflict: 'localServiceConflict',
  invalidArgument: 'localServiceInvalidArgument',
  failed: 'localServiceActionFailed',
}

export function rpcErrorKind(error: RpcError): RpcErrorKind {
  switch (error.appCode) {
    case 'unavailable':
    case 'timeout':
      return 'disconnected'
    case 'conflict':
      return 'conflict'
    case 'invalidArgument':
      return 'invalidArgument'
    default:
      return 'failed'
  }
}

/**
 * 动作失败文案：`localService*` 通用键；invalidArgument 追加服务端细节。
 * 非 RpcError（如上传/网络异常）取其 message。
 */
export function rpcErrorText(error: unknown, t: TFunction): string {
  if (!(error instanceof RpcError)) return describeUploadError(error, t)
  switch (error.data?.reason) {
    case 'gatewayPortInUse': return t('apiServicePortInUse')
    case 'gatewayRestartFailed': return t('apiServiceRestartFailed')
    case 'taskFilesInUse': return t('errReasonTaskFilesInUse')
    case 'taskFilesNotDeleted': return t('errReasonTaskFilesNotDeleted')
  }
  const kind = rpcErrorKind(error)
  const base = t(RPC_ERROR_KEYS[kind])
  return kind === 'invalidArgument' && error.message ? `${base}: ${error.message}` : base
}

/**
 * 上传 / 通用失败 → 本地化文案，不展示 `UploadError.message` 等原始诊断文本：
 * RpcError 走 `rpcErrorText`；网络错误、中止、HTTP 413 各有专用键，其余回退通用失败文案。
 */
export function describeUploadError(error: unknown, t: TFunction): string {
  if (error instanceof RpcError) return rpcErrorText(error, t)
  if (error instanceof UploadError) {
    if (error.kind === 'network') return t('accountErrorNetwork')
    if (error.kind === 'aborted') return t('uploadAborted')
    if (error.status === 413) return t('uploadTooLarge')
  }
  return t('localServiceActionFailed')
}
