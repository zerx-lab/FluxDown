import { describe, expect, test } from 'bun:test'
import { CHROME_CATALOG, getChromeCatalog, setPluginChromeSpecs, subscribeChromeCatalog } from './chromeCatalog'
import { normalizeLayout } from './chromeLayout'

describe('dynamic chrome catalog', () => {
  test('plugin specs append to railTop by default and keep stored positions', () => {
    let notified = 0
    const off = subscribeChromeCatalog(() => notified++)
    const spec = { id: 'plugin:a@x:dash', defaultZone: 'railTop' as const, families: ['rail', 'title'] as const }
    setPluginChromeSpecs([spec])
    setPluginChromeSpecs([spec])
    expect(notified).toBe(1)
    expect(getChromeCatalog().length).toBe(CHROME_CATALOG.length + 1)
    expect(normalizeLayout(null, getChromeCatalog()).railTop).toEqual(['downloads', 'rss', 'webhooks', 'plugin:a@x:dash'])
    // 存储里把插件项放到标题栏，目录带着它时被找回
    const stored = { titleLeading: ['plugin:a@x:dash'] }
    expect(normalizeLayout(stored, getChromeCatalog()).titleLeading.includes('plugin:a@x:dash')).toBe(true)
    // 不能放进状态栏
    expect(normalizeLayout({ statusLeading: ['plugin:a@x:dash'] }, getChromeCatalog()).statusLeading.includes('plugin:a@x:dash')).toBe(false)
    setPluginChromeSpecs([])
    expect(getChromeCatalog().length).toBe(CHROME_CATALOG.length)
    off()
  })

  test('only downloads and settings are fixed; plugin specs stay hideable', () => {
    expect(CHROME_CATALOG.filter((spec) => spec.fixed).map((spec) => spec.id)).toEqual(['downloads', 'settings'])
    const spec = { id: 'plugin:a@x:dash', defaultZone: 'railTop' as const, families: ['rail', 'title'] as const }
    setPluginChromeSpecs([spec])
    const layout = normalizeLayout({ hidden: ['plugin:a@x:dash', 'downloads', 'settings', 'rss'] }, getChromeCatalog())
    // 存储值没摆放 diskFree：默认隐藏条目按目录顺序加入。
    expect(layout.hidden).toEqual(['rss', 'diskFree', 'plugin:a@x:dash'])
    setPluginChromeSpecs([])
  })
})

describe('static chrome catalog', () => {
  test('assistantPanel 默认落在状态栏右侧、紧挨 appVersion 之前，且没有旧 assistant 路由条目', () => {
    const ids = CHROME_CATALOG.map((spec) => spec.id)
    expect(ids.includes('assistant')).toBe(false)
    expect(ids.indexOf('assistantPanel')).toBe(ids.indexOf('appVersion') - 1)
    const spec = CHROME_CATALOG.find((item) => item.id === 'assistantPanel')
    expect(spec?.defaultZone).toBe('statusTrailing')
    expect(spec?.families).toEqual(['rail', 'title', 'status'])
    expect(spec?.fixed).toBeUndefined()
    const layout = normalizeLayout(null, CHROME_CATALOG)
    expect(layout.statusTrailing.slice(-2)).toEqual(['assistantPanel', 'appVersion'])
  })

  test('旧布局里存的 assistant 被丢弃，assistantPanel 落到默认位置', () => {
    const layout = normalizeLayout({ railTop: ['downloads', 'assistant', 'rss'] }, CHROME_CATALOG)
    expect(layout.railTop.includes('assistant')).toBe(false)
    expect(layout.statusTrailing.includes('assistantPanel')).toBe(true)
  })
})
