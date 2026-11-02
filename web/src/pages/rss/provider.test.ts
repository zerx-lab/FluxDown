// 与 GPUI `crates/rss/src/provider.rs` 单测逐条对应（同一组用例）。
import { describe, expect, test } from 'bun:test'
import type { PluginDto } from '../../lib/rpc'
import { normalizeProviderId, providerChoices, validateProviderConfig } from './provider'

function plugin(identity: string, name: string, providers: string[], over: Partial<PluginDto> = {}): PluginDto {
  return {
    identity,
    name,
    version: '1.0.0',
    description: '',
    homepage: '',
    enabled: true,
    devMode: false,
    disabledReason: 'None',
    settings: [],
    settingsValues: {},
    permissions: [],
    authSupported: false,
    subscriptionProviderIds: providers,
    resolverUrls: [],
    loadStatus: 'Loaded',
    loadError: '',
    contributions: { views: [], commands: [], sniffRules: false, archiveRules: false, pageScripts: [] },
    risk: 'standard',
    consentPending: [],
    consentPendingHosts: [],
    networkHosts: [],
    ...over,
  }
}

const ids = (choices: { id: string }[]) => choices.map((choice) => choice.id)

describe('rss provider model parity with GPUI', () => {
  test('内置在前，可用插件按 identity 升序', () => {
    const plugins = [plugin('z@x', 'Zed', ['zed']), plugin('a@x', 'Alpha', ['alpha']), plugin('m@x', 'No provider', [])]
    const choices = providerChoices(plugins, 'rss')
    expect(ids(choices)).toEqual(['rss', 'alpha', 'zed'])
    expect(choices[0]?.kind).toBe('builtin')
    expect(choices[1]?.kind).toBe('plugin')
    expect(choices[1]?.name).toBe('Alpha')
  })

  test('不可用插件不出现', () => {
    const plugins = [
      plugin('a@x', 'Disabled', ['a'], { enabled: false }),
      plugin('b@x', 'Failed', ['b'], { loadStatus: 'Failed' }),
      plugin('c@x', 'Pending perms', ['c'], { consentPending: ['ffmpeg'] }),
      plugin('d@x', 'Pending hosts', ['d'], { consentPendingHosts: ['example.com'] }),
      plugin('e@x', 'Fine', ['e']),
    ]
    expect(ids(providerChoices(plugins, ''))).toEqual(['rss', 'e'])
  })

  test('同一 provider ID 取 identity 最小者', () => {
    const plugins = [plugin('b@x', 'Second', ['same']), plugin('a@x', 'First', ['same'])]
    const choices = providerChoices(plugins, 'same')
    expect(ids(choices)).toEqual(['rss', 'same'])
    expect(choices[1]?.name).toBe('First')
  })

  test('插件名为空时回落到 identity', () => {
    expect(providerChoices([plugin('a@x', '', ['a'])], 'a')[1]?.name).toBe('a@x')
  })

  test('当前 provider 不可用时保留为 missing 项', () => {
    const choices = providerChoices([plugin('a@x', 'Alpha', ['alpha'], { enabled: false })], ' alpha ')
    expect(ids(choices)).toEqual(['rss', 'alpha'])
    expect(choices[1]?.kind).toBe('missing')
  })

  test('空 provider 视为内置且不追加选项', () => {
    expect(ids(providerChoices([], '  '))).toEqual(['rss'])
    expect(normalizeProviderId('')).toBe('rss')
    expect(normalizeProviderId(' demo ')).toBe('demo')
  })

  test('provider 配置只接受留空或合法 JSON', () => {
    expect(validateProviderConfig('  ')).toBe('')
    expect(validateProviderConfig(' {"q":"1080p"} ')).toBe('{"q":"1080p"}')
    expect(validateProviderConfig('[1,2]')).toBe('[1,2]')
    expect(validateProviderConfig('{oops')).toBeNull()
    expect(validateProviderConfig('1080p')).toBeNull()
  })
})
