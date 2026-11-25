import { describe, expect, test } from 'bun:test'
import type { CloudDevice, LinkDeviceInfo } from '../../../lib/rpc'
import {
  buildRemoteTargets,
  checkRemoteSaveDir,
  dispatchRoute,
  dispatchSequentially,
  linkDispatchParams,
  remoteDispatchParams,
  resolveTargetValue,
} from './target'
import { mergeDevices } from '../model/devices'

function link(over: Partial<LinkDeviceInfo>): LinkDeviceInfo {
  return { fingerprint: 'fp', name: 'Phone', online: true, pairedAt: 0, lastSeenAt: 0, revoked: false, ...over }
}

function cloud(over: Partial<CloudDevice>): CloudDevice {
  return {
    id: 'row',
    deviceId: 'dev',
    name: 'n',
    platform: null,
    createdAt: '',
    lastSeenAt: '',
    lastIp: null,
    appVersion: null,
    isOnline: false,
    isCurrent: false,
    ...over,
  }
}

describe('远端保存目录校验', () => {
  test('空目录 = 目标默认目录，任何风格都合法', () => {
    expect(checkRemoteSaveDir('  ', 'windows')).toBe('ok')
    expect(checkRemoteSaveDir('', 'posix')).toBe('ok')
  })

  test('按目标路径风格判定绝对路径，而非本机风格', () => {
    expect(checkRemoteSaveDir('D:\\dl', 'windows')).toBe('ok')
    expect(checkRemoteSaveDir('\\\\nas\\share', 'windows')).toBe('ok')
    expect(checkRemoteSaveDir('/home/a', 'windows')).toBe('notAbsolute')
    expect(checkRemoteSaveDir('/home/a', 'posix')).toBe('ok')
    expect(checkRemoteSaveDir('D:\\dl', 'posix')).toBe('notAbsolute')
    expect(checkRemoteSaveDir('relative/dir', 'posix')).toBe('notAbsolute')
  })

  test('风格未知无法本地判定，放行给 agent', () => {
    expect(checkRemoteSaveDir('whatever', null)).toBe('ok')
    expect(checkRemoteSaveDir('whatever', 'unknown')).toBe('ok')
  })
})

describe('下发参数', () => {
  test('空目录省略 saveDir；空文件名省略 fileName', () => {
    expect(remoteDispatchParams('d', { url: 'http://x/a', fileName: ' ' }, '')).toEqual({ toDevice: 'd', url: 'http://x/a' })
    expect(remoteDispatchParams('d', { url: 'u', fileName: 'a.bin' }, ' /dl ')).toEqual({
      toDevice: 'd',
      url: 'u',
      fileName: 'a.bin',
      saveDir: '/dl',
    })
    expect(linkDispatchParams('fp', { url: 'u' }, '')).toEqual({ fingerprint: 'fp', url: 'u' })
  })
})

describe('目标列表', () => {
  test('云设备去掉本机，路径风格未上报时按平台推断', () => {
    const linked: LinkDeviceInfo[] = [
      link({ platform: 'android', defaultSaveDir: '/sdcard' }),
    ]
    const targets = buildRemoteTargets(
      [
        cloud({ deviceId: 'me', isCurrent: true }),
        cloud({ deviceId: 'pc', name: 'PC', platform: 'windows', isOnline: true, defaultSaveDir: 'C:\\Dl' }),
        cloud({ deviceId: 'x', platform: 'weird', pathStyle: 'posix' }),
      ],
      linked,
      { cloud: true, local: true },
    )
    expect(targets.map((target) => target.value)).toEqual(['cloud:pc', 'cloud:x', 'link:fp'])
    expect([targets[0]?.pathStyle, targets[0]?.defaultSaveDir, targets[0]?.online]).toEqual(['windows', 'C:\\Dl', true])
    expect(targets[1]?.pathStyle).toBe('posix')
    expect([targets[2]?.pathStyle, targets[2]?.defaultSaveDir]).toEqual(['posix', '/sdcard'])
  })

  test('cloud disconnection hides stale presence without hiding dispatch targets or LAN presence', () => {
    const linked: LinkDeviceInfo[] = [link({})]
    const cloudDevices = [cloud({ isOnline: true })]
    const targets = buildRemoteTargets(cloudDevices, linked, { cloud: false, local: true })
    expect(targets.map((target) => target.online)).toEqual([null, true])
    expect(targets.map((target) => target.value)).toEqual(['cloud:dev', 'link:fp'])
    expect(buildRemoteTargets(cloudDevices, linked, { cloud: false, local: false }).map((target) => target.online))
      .toEqual([null, null])
  })

  const presence = { cloud: true, local: true }

  test('cloudDeviceId 命中云条目 → 挂接，不单独成条；显示云名，云优先目录', () => {
    const cloudDevices = [cloud({ deviceId: 'pc', name: 'PC', isOnline: false })]
    const linked = [link({ cloudDeviceId: 'pc', online: true, defaultSaveDir: '/x' })]
    const entries = mergeDevices(cloudDevices, linked, presence)
    expect(entries).toHaveLength(1)
    expect(entries[0]).toMatchObject({ id: 'pc', kind: 'cloud', name: 'PC', online: true, linkFingerprint: 'fp', linkOnline: true, defaultSaveDir: '/x' })
  })

  test('挂接在线组合：任一在线 → 在线；否则任一未知 → 未知；否则离线', () => {
    const cloudDevices = [cloud({ deviceId: 'pc', name: 'PC', isOnline: false })]
    const linked = [link({ cloudDeviceId: 'pc', online: false })]
    expect(mergeDevices(cloudDevices, linked, presence)[0]?.online).toBe(false)
    expect(mergeDevices(cloudDevices, linked, { cloud: false, local: true })[0]?.online).toBeNull()
    expect(mergeDevices([cloud({ deviceId: 'pc', name: 'PC', isOnline: true })], linked, { cloud: true, local: false })[0]?.online).toBe(true)
  })

  test('cloudDeviceId 无对应云条目 / 云条目已挂接 → 独立直连；revoked 排除', () => {
    const cloudDevices = [cloud({ deviceId: 'pc', name: 'PC' })]
    const linked = [
      link({ fingerprint: 'a', name: 'A', cloudDeviceId: 'pc' }),
      link({ fingerprint: 'b', name: 'B', cloudDeviceId: 'pc' }),
      link({ fingerprint: 'c', name: 'C', cloudDeviceId: 'zzz' }),
      link({ fingerprint: 'd', name: 'D', revoked: true }),
    ]
    const targets = buildRemoteTargets(cloudDevices, linked, presence)
    expect(targets.map((target) => target.value)).toEqual(['cloud:pc', 'link:b', 'link:c'])
    expect(targets[0]?.linkFingerprint).toBe('a')
  })

  test('合并后整表同名消歧：云 id / 指纹短码', () => {
    const cloudDevices = [cloud({ deviceId: 'dev-AAAA1111', name: 'PC' })]
    const linked = [link({ fingerprint: 'fp-bbbb2222', name: ' pc ' }), link({ fingerprint: 'fp-cc', name: '' })]
    expect(mergeDevices(cloudDevices, linked, presence).map((entry) => entry.name)).toEqual(['PC · 1111', 'pc · 2222', 'fpcc'])
  })

  test('下发路由：独立直连 / 挂接且直连在线走 link，否则走云', () => {
    const cloudDevices = [cloud({ deviceId: 'pc', name: 'PC' })]
    const attachedOnline = buildRemoteTargets(cloudDevices, [link({ cloudDeviceId: 'pc', online: true })], presence)[0]
    const attachedOffline = buildRemoteTargets(cloudDevices, [link({ cloudDeviceId: 'pc', online: false })], presence)[0]
    const standalone = buildRemoteTargets([], [link({})], presence)[0]
    expect(attachedOnline && dispatchRoute(attachedOnline)).toEqual({ via: 'link', fingerprint: 'fp' })
    expect(attachedOffline && dispatchRoute(attachedOffline)).toEqual({ via: 'cloud' })
    expect(standalone && dispatchRoute(standalone)).toEqual({ via: 'link', fingerprint: 'fp' })
  })

  test('持久化 link:<fp> 已挂接 → 解析为云条目', () => {
    const targets = buildRemoteTargets([cloud({ deviceId: 'pc', name: 'PC' })], [link({ cloudDeviceId: 'pc' })], presence)
    expect(resolveTargetValue('link:fp', targets)?.value).toBe('cloud:pc')
    expect(resolveTargetValue('link:nope', targets)).toBeNull()
  })

  test('其他设备 = 云条目（含挂接）在前 + 独立直连在后；独立直连保留 kind=link 与指纹 id', () => {
    const entries = mergeDevices([cloud({ deviceId: 'pc', name: 'PC' })], [link({ cloudDeviceId: 'pc' }), link({ fingerprint: 'solo', name: 'Solo' })], presence)
    expect(entries.map((entry) => [entry.id, entry.kind])).toEqual([['pc', 'cloud'], ['solo', 'link']])
    expect(entries[1]).toMatchObject({ linkFingerprint: 'solo', linkOnline: true })
  })

  test('仅有独立直连、无云设备时也计入其他设备总数；revoked 不计', () => {
    expect(mergeDevices([], [link({ fingerprint: 'solo' })], presence)).toHaveLength(1)
    expect(mergeDevices([], [link({ revoked: true })], presence)).toHaveLength(0)
    expect(mergeDevices([cloud({ deviceId: 'me', isCurrent: true })], [], presence)).toHaveLength(0)
  })

  test('dispatches a large range in order without queueing against an in-flight request', async () => {
    const urls = Array.from({ length: 600 }, (_, index) => `https://example.com/${index}.zip`)
    const seen: string[] = []
    const failure = new Error('device rejected this URL')
    let busy = false
    const result = await dispatchSequentially(urls, async (url) => {
      if (busy) throw new Error('gateway lane full')
      busy = true
      await Promise.resolve()
      busy = false
      seen.push(url)
      if (url === urls[127]) throw failure
    }, new AbortController().signal, () => {})
    expect(seen).toEqual(urls)
    expect(result).toEqual({ ok: 599, failed: [{ item: urls[127], reason: failure }], unsent: [] })
  })

  test('stops before the next RPC and reports the result of the in-flight request', async () => {
    const controller = new AbortController()
    const urls = ['first', 'second', 'third']
    const progress: number[] = []
    const sent: string[] = []
    const result = await dispatchSequentially(urls, async (url) => {
      sent.push(url)
      await Promise.resolve()
      if (url === 'second') controller.abort()
    }, controller.signal, (done) => progress.push(done))
    expect(sent).toEqual(['first', 'second'])
    expect(progress).toEqual([1, 2])
    expect(result).toEqual({ ok: 2, failed: [], unsent: ['third'] })
  })
})
