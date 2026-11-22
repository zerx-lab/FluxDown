// 应用内更新：UpdateStatusDto → 文案键与可用操作的纯判定（移植 GPUI `crates/settings/src/update_view.rs`）。
// 「关于」页、状态栏版本条目、移动端横幅与重启遮罩共用，保证对同一份快照给出一致的文本与按钮；状态唯一来源是 agent。

import { useSyncExternalStore } from 'react'
import type { TFunction } from '../i18n'
import { useRpcSelector } from './rpc'
import type { RpcState, UpdateFailure, UpdateInstallKind, UpdateManualReason, UpdateStatusDto } from './rpc'

const selectUpdate = (state: RpcState): UpdateStatusDto | null => state.snapshot?.update ?? null

/** 当前更新状态；快照未就绪时为 null。 */
export function useUpdateStatus(): UpdateStatusDto | null {
  return useRpcSelector(selectUpdate)
}

/** 一条待插值的文案：键 + 参数 + 可选尾注。 */
export interface UpdateLine {
  key: string
  params?: Readonly<Record<string, string | number>>
  suffix?: string
}

export function updateLineText(t: TFunction, line: UpdateLine): string {
  return t(line.key, line.params) + (line.suffix ?? '')
}

/** 手动升级原因 → 说明文案键（托管包按安装形态区分）。 */
export function manualReasonKey(reason: UpdateManualReason, kind: UpdateInstallKind): string {
  switch (reason) {
    case 'managedPackage':
      switch (kind) {
        case 'docker':
          return 'updateManualDocker'
        case 'synology':
          return 'updateManualSynology'
        case 'qnap':
          return 'updateManualQnap'
        case 'openwrt':
          return 'updateManualOpenwrt'
        case 'fnos':
          return 'updateManualFnos'
        // 归 AUR / 发行版仓库等非官方系统包所有：官方 deb / Arch 包永远可应用内更新。
        case 'linuxDeb':
        case 'linuxArch':
          return 'updateManualSystemPackage'
        default:
          return 'updateManualUnsupported'
      }
    case 'notWritable':
      return 'updateManualNotWritable'
    case 'noAsset':
      return 'updateManualNoAsset'
    case 'elevationUnavailable':
      return 'updateManualElevation'
    case 'readOnlyLocation':
      return 'updateManualReadOnly'
    case 'unofficialBuild':
      return 'updateManualUnofficial'
    case 'unsupported':
    case 'unknown':
      return 'updateManualUnsupported'
  }
}

/** 失败分类 → 文案键；缺省按未知失败。 */
export function failureKey(failure: UpdateFailure | null | undefined): string {
  switch (failure) {
    case 'network':
      return 'updateFailedNetwork'
    case 'verify':
      return 'updateFailedVerify'
    case 'storage':
      return 'updateFailedStorage'
    case 'install':
      return 'updateFailedInstall'
    case 'elevationCancelled':
      return 'updateFailedElevationCancelled'
    case 'installIncomplete':
      return 'updateFailedIncomplete'
    case 'unknown':
    case undefined:
    case null:
      return 'updateFailedUnknown'
  }
}

/** 下载进度百分比（0–100）；大小未知时为 0。 */
export function downloadPercent(status: UpdateStatusDto): number {
  if (status.assetSize <= 0) return 0
  return Math.min(100, Math.floor((Math.min(status.downloadedBytes, status.assetSize) * 100) / status.assetSize))
}

/** 当前阶段的状态行；尚未检查（idle）或无信息可说时为 null。 */
export function statusLine(status: UpdateStatusDto): UpdateLine | null {
  const latest = status.latestVersion
  switch (status.phase) {
    case 'idle':
      return null
    case 'checking':
      return { key: 'checking' }
    case 'upToDate':
      return latest === '' ? { key: 'upToDate' } : { key: 'upToDate', suffix: ` (v${latest})` }
    case 'available':
      return status.hasUpdate ? { key: 'newVersionFound', params: { v: latest } } : null
    case 'downloading':
      return { key: 'updateDownloadingProgress', params: { v: latest, percent: downloadPercent(status) } }
    case 'ready':
      return { key: 'updateReadyToast', params: { v: latest } }
    case 'installing':
      return { key: 'updateInstalling' }
    case 'failed':
      return { key: failureKey(status.failure) }
  }
}

/** 不可一键更新时的手动升级说明（仅在确有新版本时）。 */
export function manualLine(status: UpdateStatusDto): UpdateLine | null {
  if (!status.hasUpdate || !status.manualReason) return null
  return { key: manualReasonKey(status.manualReason, status.installKind) }
}

/** 是否展示「更新并重启」：有新版本、可一键更新，且当前阶段允许发起安装。 */
export function canInstall(status: UpdateStatusDto): boolean {
  if (!status.hasUpdate || status.manualReason) return false
  switch (status.phase) {
    case 'available':
    case 'ready':
    case 'failed':
      return true
    case 'downloading':
      return !status.installPending
    default:
      return false
  }
}

/** 下载中可取消。 */
export function canCancel(status: UpdateStatusDto): boolean {
  return status.phase === 'downloading'
}

/** 手动升级的打开地址：优先资产直链，其次发布页；无新版本或非手动时为 null。 */
export function manualUrl(status: UpdateStatusDto): string | null {
  if (!status.hasUpdate || !status.manualReason) return null
  return [status.downloadUrl, status.releasePageUrl].find((url) => url !== '') ?? null
}

/** 全局横幅（移动端；桌面由状态栏版本条目承担）：有新版本且未在安装时展示（安装中由全屏遮罩接管）。 */
export function bannerVisible(status: UpdateStatusDto, dismissedVersion: string | null): boolean {
  return status.hasUpdate && status.phase !== 'installing' && status.latestVersion !== '' && dismissedVersion !== status.latestVersion
}

/** 状态栏版本条目的更新形态（镜像 GPUI `crates/settings/src/version_button.rs::update_badge`）；null = 只显示版本号。 */
export type VersionBadge =
  | { kind: 'available'; version: string; manual: boolean }
  | { kind: 'downloading'; version: string; percent: number }
  | { kind: 'installing'; version: string }
  | { kind: 'ready'; version: string }
  | { kind: 'failed'; version: string; failure: UpdateFailure | null }

/**
 * 更新状态 → 版本条目形态。没有新版本、后台检查失败（`hasUpdate` 为假）时为 null；
 * 被忽略的版本只在仍未下载时不提示——已下载 / 安装中照常显示进度。周期复查（checking）沿用上一轮结果，不闪烁。
 */
export function versionBadge(status: UpdateStatusDto, ignoredVersion: string | null): VersionBadge | null {
  if (!status.hasUpdate || status.latestVersion === '') return null
  const version = status.latestVersion
  if (status.phase === 'installing' || (status.phase === 'downloading' && status.installPending)) return { kind: 'installing', version }
  switch (status.phase) {
    case 'available':
    case 'checking':
      return ignoredVersion === version ? null : { kind: 'available', version, manual: status.manualReason != null }
    case 'downloading':
      return { kind: 'downloading', version, percent: downloadPercent(status) }
    case 'ready':
      return { kind: 'ready', version }
    case 'failed':
      return { kind: 'failed', version, failure: status.failure ?? null }
    default:
      return null
  }
}

/** 记录「已忽略的版本」的 localStorage 键（按版本忽略，新版本发布后重新提示；移动端横幅与桌面版本条目共用）。 */
export const UPDATE_BANNER_DISMISS_KEY = 'fluxdown.updateBannerDismissed'

function readIgnoredVersion(): string | null {
  try {
    return localStorage.getItem(UPDATE_BANNER_DISMISS_KEY)
  } catch {
    return null
  }
}

let ignoredVersion: string | null = readIgnoredVersion()
const ignoredListeners = new Set<() => void>()

function subscribeIgnored(listener: () => void): () => void {
  ignoredListeners.add(listener)
  return () => {
    ignoredListeners.delete(listener)
  }
}

/** 已忽略的版本（横幅 ×、版本条目「忽略此版本」共用，各处同步刷新）。 */
export function useIgnoredVersion(): string | null {
  return useSyncExternalStore(subscribeIgnored, () => ignoredVersion)
}

export function ignoreVersion(version: string): void {
  ignoredVersion = version
  try {
    localStorage.setItem(UPDATE_BANNER_DISMISS_KEY, version)
  } catch {
    // 存储不可用（隐私模式等）：本次会话内仍按内存状态忽略。
  }
  for (const listener of ignoredListeners) listener()
}

/** 当前版本条目形态（快照未就绪时 null）。 */
export function useVersionBadge(): VersionBadge | null {
  const status = useUpdateStatus()
  const ignored = useIgnoredVersion()
  return status ? versionBadge(status, ignored) : null
}
