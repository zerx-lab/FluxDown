import { describe, expect, test } from 'bun:test'
import type { UpdateStatusDto } from './rpc'
import { bannerVisible, canCancel, canInstall, downloadPercent, manualLine, manualUrl, statusLine, versionBadge } from './update'

function status(patch: Partial<UpdateStatusDto>): UpdateStatusDto {
  return {
    phase: 'idle',
    currentVersion: '1.0.0',
    channel: 'stable',
    latestVersion: '2.0.0',
    hasUpdate: true,
    installKind: 'unknown',
    manualReason: null,
    assetName: '',
    assetSize: 0,
    downloadedBytes: 0,
    installPending: false,
    downloadUrl: '',
    releasePageUrl: '',
    notes: [],
    failure: null,
    errorDetail: '',
    checkedAtMs: 0,
    ...patch,
  }
}

describe('update view rules', () => {
  test('状态行随阶段变化', () => {
    expect(statusLine(status({ phase: 'idle' }))).toBeNull()
    expect(statusLine(status({ phase: 'checking' }))?.key).toBe('checking')
    expect(statusLine(status({ phase: 'upToDate' }))?.suffix).toBe(' (v2.0.0)')
    expect(statusLine(status({ phase: 'available', hasUpdate: false }))).toBeNull()
    expect(statusLine(status({ phase: 'ready' }))?.key).toBe('updateReadyToast')
    expect(statusLine(status({ phase: 'failed', failure: 'verify' }))?.key).toBe('updateFailedVerify')
    expect(statusLine(status({ phase: 'failed' }))?.key).toBe('updateFailedUnknown')
  })

  test('下载进度', () => {
    expect(downloadPercent(status({ assetSize: 0, downloadedBytes: 5 }))).toBe(0)
    expect(downloadPercent(status({ assetSize: 200, downloadedBytes: 50 }))).toBe(25)
    expect(downloadPercent(status({ assetSize: 200, downloadedBytes: 999 }))).toBe(100)
    expect(statusLine(status({ phase: 'downloading', assetSize: 200, downloadedBytes: 50 }))?.params).toEqual({
      v: '2.0.0',
      percent: 25,
    })
  })

  test('按钮规则', () => {
    expect(canInstall(status({ phase: 'available' }))).toBe(true)
    expect(canInstall(status({ phase: 'downloading' }))).toBe(true)
    expect(canInstall(status({ phase: 'downloading', installPending: true }))).toBe(false)
    expect(canInstall(status({ phase: 'installing' }))).toBe(false)
    expect(canInstall(status({ phase: 'available', manualReason: 'noAsset' }))).toBe(false)
    expect(canInstall(status({ phase: 'available', hasUpdate: false }))).toBe(false)
    expect(canCancel(status({ phase: 'downloading' }))).toBe(true)
    expect(canCancel(status({ phase: 'ready' }))).toBe(false)
  })

  test('手动升级文案与地址', () => {
    const docker = status({ phase: 'available', manualReason: 'managedPackage', installKind: 'docker', releasePageUrl: 'https://r' })
    expect(manualLine(docker)?.key).toBe('updateManualDocker')
    expect(manualUrl(docker)).toBe('https://r')
    expect(manualUrl({ ...docker, downloadUrl: 'https://d' })).toBe('https://d')
    expect(manualUrl(status({ phase: 'available' }))).toBeNull()
    expect(manualLine(status({ manualReason: 'managedPackage', installKind: 'qnap' }))?.key).toBe('updateManualQnap')
    expect(manualLine(status({ manualReason: 'managedPackage', installKind: 'unknown' }))?.key).toBe('updateManualUnsupported')
    expect(manualLine(status({ manualReason: 'managedPackage', installKind: 'linuxArch' }))?.key).toBe('updateManualSystemPackage')
    expect(manualLine(status({ manualReason: 'managedPackage', installKind: 'linuxDeb' }))?.key).toBe('updateManualSystemPackage')
  })

  test('横幅按版本忽略', () => {
    expect(bannerVisible(status({ phase: 'ready' }), null)).toBe(true)
    expect(bannerVisible(status({ phase: 'ready' }), '2.0.0')).toBe(false)
    expect(bannerVisible(status({ phase: 'ready' }), '1.9.0')).toBe(true)
    expect(bannerVisible(status({ phase: 'installing' }), null)).toBe(false)
    expect(bannerVisible(status({ hasUpdate: false, phase: 'upToDate' }), null)).toBe(false)
  })

  test('版本条目形态与 GPUI update_badge 同组用例', () => {
    expect(versionBadge(status({ phase: 'upToDate' }), null)).toBeNull()
    expect(versionBadge(status({ phase: 'failed', hasUpdate: false }), null)).toBeNull()
    expect(versionBadge(status({ phase: 'downloading', assetSize: 200, downloadedBytes: 90 }), null)).toEqual({ kind: 'downloading', version: '2.0.0', percent: 45 })
    expect(versionBadge(status({ phase: 'ready' }), null)).toEqual({ kind: 'ready', version: '2.0.0' })
    expect(versionBadge(status({ phase: 'downloading', installPending: true }), null)).toEqual({ kind: 'installing', version: '2.0.0' })
    expect(versionBadge(status({ phase: 'installing' }), null)?.kind).toBe('installing')
    expect(versionBadge(status({ phase: 'checking' }), null)).toEqual({ kind: 'available', version: '2.0.0', manual: false })
    expect(versionBadge(status({ phase: 'available', manualReason: 'notWritable' }), null)).toEqual({ kind: 'available', version: '2.0.0', manual: true })
    expect(versionBadge(status({ phase: 'failed', failure: 'network' }), null)).toEqual({ kind: 'failed', version: '2.0.0', failure: 'network' })
    // 忽略的版本只在仍未下载时不提示。
    expect(versionBadge(status({ phase: 'available' }), '2.0.0')).toBeNull()
    expect(versionBadge(status({ phase: 'ready' }), '2.0.0')).not.toBeNull()
    expect(versionBadge(status({ phase: 'available', latestVersion: '2.0.1' }), '2.0.0')).not.toBeNull()
  })
})
