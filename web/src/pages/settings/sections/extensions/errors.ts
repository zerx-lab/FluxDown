// 扩展页的错误文案映射（与 GPUI `error_text` 一致）。

import type { TFunction } from '../../../../i18n'
import { RpcError } from '../../../../lib/rpc'
import { describeUploadError } from '../../../../lib/rpcErrorText'
import type { ErrorReason } from '../../../../lib/rpc'

const REASON_KEYS: Partial<Record<ErrorReason, string>> = {
  marketUnreachable: 'pluginErrorMarketUnreachable',
  marketIndexInvalid: 'pluginErrorMarketIndexInvalid',
  marketIndexRollback: 'pluginErrorMarketIndexRollback',
  pluginNotInMarket: 'pluginErrorNotInMarket',
  pluginYanked: 'pluginErrorYanked',
  pluginDownloadFailed: 'pluginErrorDownloadFailed',
  pluginPackageTooLarge: 'pluginErrorPackageTooLarge',
  pluginPackageInvalid: 'pluginErrorPackageInvalid',
  marketVersionChanged: 'pluginErrorMarketVersionChanged',
}

/**
 * agent 端口不透传服务端 message：有细分原因时按原因给出可操作文案，否则按码映射通用文案。
 * 非 RPC 错误（上传失败等）直接展示其消息。
 */
export function extensionErrorText(t: TFunction, error: unknown): string {
  if (!(error instanceof RpcError)) return describeUploadError(error, t)
  const reason = error.reason
  const reasonKey = reason ? REASON_KEYS[reason] : undefined
  if (reasonKey) return t(reasonKey)
  // unavailable / timeout 与其余码同走通用文案（`localServiceDisconnected` 带倒计时占位符，不适合行内错误）。
  switch (error.appCode) {
    case 'invalidArgument':
    case 'notFound':
      return t('localServiceInvalidArgument')
    case 'conflict':
      return t('localServiceConflict')
    case 'unsupported':
      return t('settingsUnsupportedOnPlatform')
    default:
      return t('localServiceActionFailed')
  }
}

/** Inline/copyable plugin diagnostics; do not expose generic JS exceptions or daemon messages. */
export function pluginFailureText(t: TFunction, error: unknown): string {
  if (!(error instanceof RpcError)) return t('localServiceActionFailed')
  const detail = error.data?.pluginDetail
  switch (detail?.kind) {
    case 'manifestUnreadable':
      return t('pluginDiagnosticManifestUnreadable')
    case 'manifestUnknownField':
      return t('pluginDiagnosticManifestUnknownField')
    case 'manifestJsonInvalid':
      return t('pluginDiagnosticManifestJsonInvalid')
    case 'manifestIdentityInvalid':
      return t('pluginDiagnosticManifestIdentityInvalid')
    case 'manifestNameMissing':
      return t('pluginDiagnosticManifestNameMissing')
    case 'manifestVersionInvalid':
      return t('pluginDiagnosticManifestVersionInvalid')
    case 'manifestMinAppVersionInvalid':
      return t('pluginDiagnosticManifestMinAppVersionInvalid')
    case 'manifestUnknownPermission':
      return t('pluginDiagnosticManifestUnknownPermission')
    case 'manifestAuthPermissionMissing':
      return t('pluginDiagnosticManifestAuthPermissionMissing')
    case 'scriptCompileFailed':
      return t('pluginDiagnosticScriptCompileFailed')
    case 'scriptCompileFailedAt':
      return t('pluginDiagnosticScriptCompileFailedAt', { line: detail.line, column: detail.column })
    case 'missingFile':
      return t('pluginDiagnosticMissingFile', { path: detail.path })
    case 'loadFailed':
      return t('pluginDiagnosticLoadFailed')
    case 'notDevPlugin':
      return t('pluginDiagnosticNotDevPlugin')
    default:
      return extensionErrorText(t, error)
  }
}
