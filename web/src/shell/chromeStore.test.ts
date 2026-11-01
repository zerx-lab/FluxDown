import { describe, expect, test } from 'bun:test'
import { CHROME_LAYOUT_KEY, getChromeLayout, moveChromeItem, resetChromeLayout, setChromeItemHidden } from './chromeStore'

class MemoryStorage {
  private readonly data = new Map<string, string>()
  getItem(key: string): string | null {
    return this.data.get(key) ?? null
  }
  setItem(key: string, value: string): void {
    this.data.set(key, value)
  }
  removeItem(key: string): void {
    this.data.delete(key)
  }
}

// 存储在首次读取 / 写入时才访问，桩只需在首个用例前装好。
const local = new MemoryStorage()
Object.assign(globalThis, { localStorage: local })

const stored = () => JSON.parse(local.getItem(CHROME_LAYOUT_KEY) ?? 'null') as Record<string, string[] | undefined>

describe('chromeStore', () => {
  test('默认布局复现改造前的结构', () => {
    const layout = getChromeLayout()
    expect(layout.railTop).toEqual(['downloads', 'rss', 'webhooks'])
    expect(layout.railBottom).toEqual(['account', 'settings'])
    expect(layout.statusLeading).toEqual(['downloadSpeed', 'uploadSpeed', 'pauseAll', 'resumeAll'])
    expect(layout.statusTrailing).toEqual(['downloadLimit', 'uploadLimit', 'proxy', 'diskFree'])
    expect(layout.titleLeading).toEqual([])
    expect(layout.titleTrailing).toEqual([])
  })

  test('成功的移动立即写入 localStorage，且写全六个区域', () => {
    expect(moveChromeItem('proxy', 'titleTrailing', null)).toBe(true)
    expect(Object.keys(stored())).toHaveLength(6)
    expect(stored().titleTrailing).toEqual(['proxy'])
    expect(stored().statusTrailing).toEqual(['downloadLimit', 'uploadLimit', 'diskFree'])
  })

  test('被拒绝的移动不写入；重置写入默认布局', () => {
    expect(moveChromeItem('downloadSpeed', 'railTop', null)).toBe(false)
    resetChromeLayout()
    expect(stored().titleTrailing).toEqual([])
    expect(stored().statusTrailing).toEqual(['downloadLimit', 'uploadLimit', 'proxy', 'diskFree'])
  })

  test('隐藏 / 显示立即持久化，hidden 仅在非空时写入；位置不变，移动保留隐藏', () => {
    expect(stored().hidden).toBeUndefined()
    expect(setChromeItemHidden('rss', true)).toBe(true)
    expect(setChromeItemHidden('rss', true)).toBe(false)
    expect(stored().hidden).toEqual(['rss'])
    expect(stored().railTop).toEqual(['downloads', 'rss', 'webhooks'])
    expect(moveChromeItem('rss', 'titleLeading', null)).toBe(true)
    expect(stored().hidden).toEqual(['rss'])
    expect(setChromeItemHidden('rss', false)).toBe(true)
    expect(stored().hidden).toBeUndefined()
  })

  test('固定条目与未知条目不能隐藏；重置清除隐藏', () => {
    expect(setChromeItemHidden('downloads', true)).toBe(false)
    expect(setChromeItemHidden('settings', true)).toBe(false)
    expect(setChromeItemHidden('ghost', true)).toBe(false)
    expect(setChromeItemHidden('proxy', true)).toBe(true)
    expect(getChromeLayout().hidden).toEqual(['proxy'])
    resetChromeLayout()
    expect(getChromeLayout().hidden).toEqual([])
    expect(stored().hidden).toBeUndefined()
  })
})
