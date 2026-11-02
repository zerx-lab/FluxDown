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
    expect(normalizeLayout(stored, getChromeCatalog()).titleLeading).toEqual(['plugin:a@x:dash'])
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
    expect(layout.hidden).toEqual(['rss', 'plugin:a@x:dash'])
    setPluginChromeSpecs([])
  })
})
