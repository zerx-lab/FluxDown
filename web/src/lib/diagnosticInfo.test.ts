import { describe, expect, test } from 'bun:test'
import { diagnosticText } from './diagnosticInfo'
import type { UpdateStatusDto } from './rpc'

const status = {
  phase: 'upToDate',
  currentVersion: '0.6.3',
  channel: 'frontier',
  latestVersion: '0.6.3',
  hasUpdate: false,
  installKind: 'docker',
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
} as UpdateStatusDto

describe('diagnosticText', () => {
  test('版本、渠道、协议、主机形态、客户端与语言各占一行', () => {
    expect(diagnosticText({ serviceVersion: '0.6.3', protocolVersion: 28, status, locale: 'zh', userAgent: 'Mozilla/5.0 X' })).toBe(
      'FluxDown v0.6.3 (frontier)\nProtocol: 28\nHost: docker\nClient: web · Mozilla/5.0 X\nLocale: zh',
    )
  })

  test('快照未就绪时仍能复制；已知新版本时写出', () => {
    expect(diagnosticText({ serviceVersion: '', protocolVersion: null, status: null, locale: 'en', userAgent: '' })).toBe(
      'FluxDown v?\nHost: unknown\nClient: web · unknown\nLocale: en',
    )
    const text = diagnosticText({ serviceVersion: '0.6.3', protocolVersion: 28, status: { ...status, hasUpdate: true, latestVersion: '0.7.0' }, locale: 'en', userAgent: 'UA' })
    expect(text.endsWith('\nLatest: v0.7.0')).toBe(true)
  })
})
