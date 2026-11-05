import { describe, expect, test } from 'bun:test'
import type { MarketEntryDto, PluginDto } from '../../../../lib/rpc/protocol'
import {
  MARKET_MAX_AGE_MS,
  beginPrivateLoad,
  failedPrivateLoad,
  installedVersionYanked,
  latestPerPlugin,
  marketAction,
  marketNeedsLoad,
  permissionsToConfirm,
  INITIAL_PRIVATE_CATALOG,
  privateExpiryText,
  privateSectionView,
  replacesLocalPlugin,
  versionNewer,
} from './logic'

function entry(pluginId: string, version: string, sequence: number, yanked = 'none', permissions: string[] = []): MarketEntryDto {
  return { pluginId, version, sequence, yanked, permissions, name: pluginId, tags: [] } as unknown as MarketEntryDto
}

function plugin(identity: string, version: string, devMode = false, permissions: string[] = []): PluginDto {
  return { identity, version, devMode, permissions } as unknown as PluginDto
}

function privateEntry(pluginId: string, version: string, sequence: number, yanked = 'none'): MarketEntryDto {
  return { ...entry(pluginId, version, sequence, yanked), channel: 'private', mirrors: [] }
}

function pluginFrom(identity: string, version: string, source: string): PluginDto {
  return { ...plugin(identity, version), source }
}

describe('market logic', () => {
  test('latestPerPlugin：取最新可安装版本，撤回版本让位，保持首次出现顺序', () => {
    const list = latestPerPlugin([
      entry('a', '1.0.0', 1),
      entry('b', '1.0.0', 1),
      entry('a', '1.1.0', 2),
      entry('a', '1.2.0', 3, 'malicious'),
    ])
    expect(list.map((e) => `${e.pluginId}@${e.version}`)).toEqual(['a@1.1.0', 'b@1.0.0'])
  })

  test('latestPerPlugin：全部撤回（下架）的插件不出现', () => {
    const list = latestPerPlugin([entry('a', '1.0.0', 1, 'deprecated'), entry('a', '1.1.0', 2, 'vulnerable'), entry('b', '1.0.0', 3)])
    expect(list.map((e) => e.pluginId)).toEqual(['b'])
  })

  test('versionNewer：忽略前导 v 与预发布后缀，无法解析视为不更新', () => {
    expect(versionNewer('v1.10.0', '1.9.9')).toBe(true)
    expect(versionNewer('1.0.0-rc.1', '1.0.0')).toBe(false)
    expect(versionNewer('abc', '1.0.0')).toBe(false)
  })

  test('marketAction', () => {
    expect(marketAction(entry('a', '1.0.0', 1), undefined)).toBe('install')
    expect(marketAction(entry('a', '1.0.0', 1, 'deprecated'), undefined)).toBe('unavailable')
    expect(marketAction(entry('a', '1.1.0', 2), plugin('a', '1.0.0'))).toBe('update')
    expect(marketAction(entry('a', '1.1.0', 2), plugin('a', '1.0.0', true))).toBe('installed')
    expect(marketAction(entry('a', '1.1.0', 2, 'malicious'), plugin('a', '1.0.0'))).toBe('installed')
  })

  test('permissionsToConfirm：更新只取新增权限', () => {
    const e = entry('a', '2.0.0', 2, 'none', ['ffmpeg', 'auth'])
    expect(permissionsToConfirm(e, undefined)).toEqual(['ffmpeg', 'auth'])
    expect(permissionsToConfirm(e, plugin('a', '1.0.0', false, ['ffmpeg']))).toEqual(['auth'])
  })

  test('installedVersionYanked', () => {
    const entries = [entry('a', '1.0.0', 1, 'vulnerable'), entry('b', '1.0.0', 1)]
    expect(installedVersionYanked(entries, plugin('a', '1.0.0'))).toBe('vulnerable')
    expect(installedVersionYanked(entries, plugin('a', '1.0.0', true))).toBeNull()
    expect(installedVersionYanked(entries, plugin('b', '1.0.0'))).toBeNull()
  })

  test('marketNeedsLoad：从未请求 / 断连重试 / 过期才加载，失败态不自动重试', () => {
    const loaded = { requested: true, retryOnReconnect: false, failed: false, loadedAt: 1_000 }
    expect(marketNeedsLoad({ requested: false, retryOnReconnect: false, failed: false, loadedAt: null }, 1_000)).toBe(true)
    expect(marketNeedsLoad({ ...loaded, retryOnReconnect: true, failed: true }, 1_000)).toBe(true)
    expect(marketNeedsLoad(loaded, 1_000 + MARKET_MAX_AGE_MS - 1)).toBe(false)
    expect(marketNeedsLoad(loaded, 1_000 + MARKET_MAX_AGE_MS)).toBe(true)
    expect(marketNeedsLoad({ ...loaded, failed: true }, 1_000 + MARKET_MAX_AGE_MS)).toBe(false)
    expect(marketNeedsLoad({ ...loaded, loadedAt: null, failed: true }, 5_000)).toBe(false)
  })

  test('私有隔离：marketAction 对 source=private 的已装插件不给公开 Update，私有条目照常', () => {
    const installedPrivate = pluginFrom('a', '1.0.0', 'private')
    // 公开行（无 channel）：不给 Update / Install，一律按已安装
    expect(marketAction(entry('a', '2.0.0', 2), installedPrivate)).toBe('installed')
    expect(marketAction(entry('a', '2.0.0', 2, 'deprecated'), installedPrivate)).toBe('installed')
    // 私有行：照常 install / update / installed / unavailable
    expect(marketAction(privateEntry('a', '2.0.0', 2), installedPrivate)).toBe('update')
    expect(marketAction(privateEntry('a', '1.0.0', 1), installedPrivate)).toBe('installed')
    expect(marketAction(privateEntry('a', '2.0.0', 2), undefined)).toBe('install')
    expect(marketAction(privateEntry('a', '2.0.0', 2, 'deprecated'), undefined)).toBe('unavailable')
    // 非私有来源的已装插件：公开行照常更新（空 source 与缺省同义）
    expect(marketAction(entry('a', '2.0.0', 2), pluginFrom('a', '1.0.0', ''))).toBe('update')
    expect(marketAction(entry('a', '2.0.0', 2), plugin('a', '1.0.0'))).toBe('update')
    // 开发模式的私有插件不被覆盖
    expect(marketAction(privateEntry('a', '2.0.0', 2), { ...installedPrivate, devMode: true })).toBe('installed')
  })

  test('私有隔离：installedVersionYanked 私有来源只认私有条目，其余只认公开条目', () => {
    const publicYanked = entry('a', '1.0.0', 1, 'malicious')
    const privateYanked = privateEntry('a', '1.0.0', 1, 'vulnerable')
    const installedPrivate = pluginFrom('a', '1.0.0', 'private')
    expect(installedVersionYanked([publicYanked], installedPrivate)).toBeNull()
    expect(installedVersionYanked([publicYanked, privateYanked], installedPrivate)).toBe('vulnerable')
    expect(installedVersionYanked([privateYanked], plugin('a', '1.0.0'))).toBeNull()
    expect(installedVersionYanked([privateYanked, publicYanked], plugin('a', '1.0.0'))).toBe('malicious')
    expect(installedVersionYanked([privateYanked], { ...installedPrivate, devMode: true })).toBeNull()
  })

  test('privateExpiryText：缺省 / null / 空串 = 永久，无法解析原样返回，成功交给格式化函数', () => {
    const format = (date: Date) => `D${date.getUTCFullYear()}`
    expect(privateExpiryText(undefined, format)).toBeNull()
    expect(privateExpiryText(null, format)).toBeNull()
    expect(privateExpiryText('  ', format)).toBeNull()
    expect(privateExpiryText('not a date', format)).toBe('not a date')
    expect(privateExpiryText('2027-03-01T00:00:00Z', format)).toBe('D2027')
  })

  test('privateSectionView：未请求隐藏 / 加载 / 失败 / 未登录 / 已登录无条目整区隐藏 / 有条目', () => {
    const base = { loading: false, failed: false, signedIn: null, entries: [] as MarketEntryDto[] }
    expect(privateSectionView(base)).toBe('hidden')
    expect(privateSectionView({ ...base, loading: true })).toBe('loading')
    expect(privateSectionView({ ...base, failed: true })).toBe('error')
    expect(privateSectionView({ ...base, failed: true, signedIn: true, entries: [privateEntry('a', '1.0.0', 1)] })).toBe('error')
    expect(privateSectionView({ ...base, signedIn: false })).toBe('signIn')
    expect(privateSectionView({ ...base, signedIn: true })).toBe('hidden')
    // 全部撤回（下架）经 latestPerPlugin 后为空 → 同样整区隐藏
    expect(privateSectionView({ ...base, signedIn: true, entries: [privateEntry('a', '1.0.0', 1, 'malicious')] })).toBe('hidden')
    expect(privateSectionView({ ...base, signedIn: true, entries: [privateEntry('a', '1.0.0', 1)] })).toBe('list')
    // 已有目录时静默刷新不闪回加载态
    expect(privateSectionView({ ...base, loading: true, signedIn: true, entries: [privateEntry('a', '1.0.0', 1)] })).toBe('list')
  })

  test('beginPrivateLoad / failedPrivateLoad：刷新保留已加载目录不闪空，失败态无条目回到加载态', () => {
    const loaded = { loading: false, failed: false, signedIn: true, entries: [privateEntry('a', '1.0.0', 1)] }

    // 刷新：已加载的条目继续显示，等新结果替换。
    const refreshing = beginPrivateLoad(loaded, false)
    expect(refreshing).toEqual({ ...loaded, loading: true })
    expect(privateSectionView(refreshing)).toBe('list')

    // 重试 / 登录态变化（reset）：丢弃目录回到加载态。
    const reset = beginPrivateLoad(loaded, true)
    expect(reset).toEqual({ loading: true, failed: false, signedIn: null, entries: [] })
    expect(privateSectionView(reset)).toBe('loading')

    // 失败 → 错误条；再刷新：回到加载态而不是残留旧条目。
    const failed = failedPrivateLoad(loaded)
    expect(privateSectionView(failed)).toBe('error')
    const retrying = beginPrivateLoad(failed, false)
    expect(retrying).toEqual({ loading: true, failed: false, signedIn: null, entries: [] })
    expect(privateSectionView(retrying)).toBe('loading')

    // 未登录 / 已登录无授权刷新期间保持原呈现；授权出现后整区立即出现。
    const signedOut = { ...loaded, signedIn: false, entries: [] as MarketEntryDto[] }
    expect(privateSectionView(beginPrivateLoad(signedOut, false))).toBe('signIn')
    const empty = { ...loaded, entries: [] as MarketEntryDto[] }
    expect(privateSectionView(beginPrivateLoad(empty, false))).toBe('hidden')
    const granted = { loading: false, failed: false, signedIn: true, entries: [privateEntry('b', '1.0.0', 1)] }
    expect(privateSectionView(granted)).toBe('list')

    // 初始态：尚未请求 → 整区不展示。
    expect(privateSectionView(INITIAL_PRIVATE_CATALOG)).toBe('hidden')
  })

  test('replacesLocalPlugin：私有安装覆盖本机非私有同 id 插件才警示（镜像 Rust replaces_local_plugin）', () => {
    const priv = privateEntry('a', '2.0.0', 2)
    expect(replacesLocalPlugin(priv, plugin('a', '1.0.0'))).toBe(true)
    expect(replacesLocalPlugin(priv, undefined)).toBe(false)
    expect(replacesLocalPlugin(priv, pluginFrom('a', '1.0.0', 'private'))).toBe(false)
    expect(replacesLocalPlugin(priv, plugin('a', '1.0.0', true))).toBe(false)
    expect(replacesLocalPlugin(entry('a', '2.0.0', 2), plugin('a', '1.0.0'))).toBe(false)
  })
})
