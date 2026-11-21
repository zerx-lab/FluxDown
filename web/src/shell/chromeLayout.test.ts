import { describe, expect, test } from 'bun:test'
import { CHROME_ZONES, defaultLayout, isHidden, moveItem, normalizeLayout, serializeLayout, setHidden, zoneFamily } from './chromeLayout'
import type { ChromeFamily, ChromeItemSpec, StoredChromeLayout } from './chromeLayout'
import type { ChromeZone } from './chromeLayout'

interface Fixture {
  catalog: ChromeItemSpec[]
  normalize: { name: string; stored: unknown; expected: StoredChromeLayout }[]
  moves: {
    name: string
    stored: unknown
    move: { id: string; zone: ChromeZone; target: string | null }
    changed: boolean
    expected: StoredChromeLayout
  }[]
  visibility: {
    name: string
    stored: unknown
    set: { id: string; hidden: boolean }
    changed: boolean
    expected: StoredChromeLayout
  }[]
  defaultHidden: { catalog: ChromeItemSpec[]; cases: { name: string; stored: unknown; expected: StoredChromeLayout }[] }
}

// 与 crates/shell/tests/fixtures 共用同一份用例（运行期读取：Docker 构建上下文不含 crates/）。
// tsconfig 未引入 Bun 类型，仅声明测试用到的最小接口。
declare const Bun: { file(path: URL): { json(): Promise<unknown> } }
const fixture = (await Bun.file(new URL('../../../crates/shell/tests/fixtures/chrome_layout_cases.json', import.meta.url)).json()) as Fixture

describe('chrome layout normalize (shared fixtures)', () => {
  for (const item of fixture.normalize) {
    test(item.name, () => {
      expect(serializeLayout(normalizeLayout(item.stored, fixture.catalog))).toEqual(item.expected)
    })
  }
})

describe('chrome layout default hidden (shared fixtures)', () => {
  for (const item of fixture.defaultHidden.cases) {
    test(item.name, () => {
      expect(serializeLayout(normalizeLayout(item.stored, fixture.defaultHidden.catalog))).toEqual(item.expected)
    })
  }
})

describe('chrome layout move (shared fixtures)', () => {
  for (const item of fixture.moves) {
    test(item.name, () => {
      const layout = normalizeLayout(item.stored, fixture.catalog)
      const result = moveItem(layout, fixture.catalog, item.move.id, item.move.zone, item.move.target)
      expect(result.changed).toBe(item.changed)
      expect(serializeLayout(result.layout)).toEqual(item.expected)
    })
  }
})

describe('chrome layout visibility (shared fixtures)', () => {
  for (const item of fixture.visibility) {
    test(item.name, () => {
      const layout = normalizeLayout(item.stored, fixture.catalog)
      const result = setHidden(layout, fixture.catalog, item.set.id, item.set.hidden)
      expect(result.changed).toBe(item.changed)
      expect(serializeLayout(result.layout)).toEqual(item.expected)
      if (!item.changed) expect(result.layout).toBe(layout)
    })
  }
})

describe('chrome layout helpers', () => {
  test('默认布局等于 normalize(null)，且区域键齐全', () => {
    const layout = defaultLayout(fixture.catalog)
    expect(layout).toEqual(normalizeLayout(null, fixture.catalog))
    expect(Object.keys(serializeLayout(layout))).toEqual([...CHROME_ZONES])
  })

  test('区域族由前缀决定', () => {
    const families = CHROME_ZONES.map((zone): ChromeFamily => zoneFamily(zone))
    expect(families).toEqual(['title', 'title', 'rail', 'rail', 'status', 'status'])
  })

  test('未变化的移动返回同一个布局对象', () => {
    const layout = defaultLayout(fixture.catalog)
    expect(moveItem(layout, fixture.catalog, 'feed', 'railTop', null).layout).toBe(layout)
  })

  test('默认布局不含隐藏项；isHidden 反映 hidden 列表', () => {
    expect(defaultLayout(fixture.catalog).hidden).toEqual([])
    const layout = normalizeLayout({ hidden: ['feed', 'home'] }, fixture.catalog)
    expect(isHidden(layout, 'feed')).toBe(true)
    expect(isHidden(layout, 'home')).toBe(false)
  })
})
