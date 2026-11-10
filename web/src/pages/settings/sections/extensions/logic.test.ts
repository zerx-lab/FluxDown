import { describe, expect, test } from 'bun:test'
import type { MarketEntryDto, PluginDto, SettingFieldDto } from '../../../../lib/rpc/protocol'
import {
  CHALLENGE_TEXT_LIMIT,
  MARKET_PAGE_SIZE,
  PRIVATE_SEARCH_MIN_ENTRIES,
  dataImageChallengeSrc,
  filterMarket,
  filterPrivate,
  formatBytes,
  liveDialogPlugins,
  pageOf,
  privateSearchVisible,
  safeHttpUrl,
  truncateChallengeText,
  validateField,
} from './logic'

function field(type: string, widget: string, patch: Partial<SettingFieldDto> = {}): SettingFieldDto {
  return {
    key: 'k',
    title: 'K',
    description: '',
    type,
    widget,
    options: [],
    default: null,
    required: false,
    min: null,
    max: null,
    pattern: null,
    helperScript: null,
    helperLabel: null,
    ...patch,
  }
}

function entry(id: string, name: string, description: string, author: string, tags: string[]): MarketEntryDto {
  return {
    pluginId: id,
    version: '1.0.0',
    sequence: 1,
    contentHash: '',
    minAppVersion: '',
    name,
    description,
    author,
    homepage: '',
    mirrors: [],
    publishTime: '',
    yanked: '',
    tags,
    permissions: [],
    networkHosts: [],
  }
}

describe('validateField', () => {
  test('required 只拒绝空白', () => {
    const required = field('string', 'text', { required: true })
    expect(validateField(required, '  ')).toEqual({ kind: 'required' })
    expect(validateField(required, 'x')).toBeNull()
    expect(validateField(field('string', 'text'), '')).toBeNull()
  })

  test('number 范围与格式', () => {
    const number = field('number', 'number', { min: 1, max: 10.5 })
    expect(validateField(number, 'abc')).toEqual({ kind: 'number' })
    expect(validateField(number, '0x10')).toEqual({ kind: 'number' })
    expect(validateField(number, 'Infinity')).toEqual({ kind: 'number' })
    expect(validateField(number, '0')).toEqual({ kind: 'min', min: '1' })
    expect(validateField(number, '11')).toEqual({ kind: 'max', max: '10.5' })
    expect(validateField(number, ' 5 ')).toBeNull()
    expect(validateField(number, '1e1')).toBeNull()
  })

  test('select 必须是已知选项', () => {
    const select = field('string', 'select', { options: [{ value: 'a', label: 'A' }] })
    expect(validateField(select, 'b')).toEqual({ kind: 'select' })
    expect(validateField(select, 'a')).toBeNull()
  })
})

describe('filterMarket', () => {
  const entries = [
    entry('video.dl', 'Video', 'grabs videos', 'Ann', ['media']),
    entry('other', 'Other', 'misc', 'Bob', ['tools']),
  ]
  const ids = (query: string) => filterMarket(entries, query).map((item) => item.pluginId)

  test('空查询保留全部', () => {
    expect(ids('   ')).toEqual(['video.dl', 'other'])
  })

  test('名称/id/描述/作者/标签任一命中且大小写不敏感', () => {
    expect(ids('VIDEO')).toEqual(['video.dl'])
    expect(ids('bob')).toEqual(['other'])
    expect(ids('media')).toEqual(['video.dl'])
    expect(ids('grabs')).toEqual(['video.dl'])
    expect(ids('nothing')).toEqual([])
  })
})

describe('pageOf', () => {
  const items = Array.from({ length: MARKET_PAGE_SIZE * 2 + 20 }, (_, index) => index)
  const at = (length: number, limit: number) => {
    const { visible, remaining } = pageOf(items.slice(0, length), limit)
    return [visible.length, remaining]
  }

  test('0 条没有可见项也没有剩余', () => {
    expect(at(0, MARKET_PAGE_SIZE)).toEqual([0, 0])
  })

  test('恰好一页：全部可见，剩余 0（按钮隐藏）', () => {
    expect(at(MARKET_PAGE_SIZE, MARKET_PAGE_SIZE)).toEqual([MARKET_PAGE_SIZE, 0])
  })

  test('多一条：剩余 1；展开两页后到达最后一条', () => {
    expect(at(MARKET_PAGE_SIZE + 1, MARKET_PAGE_SIZE)).toEqual([MARKET_PAGE_SIZE, 1])
    expect(at(MARKET_PAGE_SIZE * 2 + 20, MARKET_PAGE_SIZE * 2)).toEqual([MARKET_PAGE_SIZE * 2, 20])
    expect(at(MARKET_PAGE_SIZE * 2 + 20, MARKET_PAGE_SIZE * 3)).toEqual([MARKET_PAGE_SIZE * 2 + 20, 0])
  })

  test('limit 大于总数（目录刷新后变短）不越界；可见的是原顺序前缀', () => {
    expect(at(3, MARKET_PAGE_SIZE * 4)).toEqual([3, 0])
    expect(pageOf(items.slice(0, MARKET_PAGE_SIZE + 1), MARKET_PAGE_SIZE).visible.at(-1)).toBe(MARKET_PAGE_SIZE - 1)
  })
})

describe('filterPrivate', () => {
  const make = (count: number) =>
    Array.from({ length: count }, (_, index) => entry(`p${index}@acme`, `Plugin ${index}`, '', '', []))

  test('搜索框出现的阈值', () => {
    expect(privateSearchVisible(0)).toBe(false)
    expect(privateSearchVisible(PRIVATE_SEARCH_MIN_ENTRIES - 1)).toBe(false)
    expect(privateSearchVisible(PRIVATE_SEARCH_MIN_ENTRIES)).toBe(true)
  })

  test('搜索框不可见时忽略残留关键字', () => {
    const few = make(PRIVATE_SEARCH_MIN_ENTRIES - 1)
    expect(filterPrivate(few, 'nothing-matches')).toHaveLength(few.length)
  })

  test('可见时同公开市场规则：空查询保留全部、全部被滤掉得到空、长 / CJK 名称可命中', () => {
    const many = make(120)
    expect(filterPrivate(many, '')).toHaveLength(120)
    expect(filterPrivate(many, '  PLUGIN 119').map((item) => item.pluginId)).toEqual(['p119@acme'])
    expect(filterPrivate(many, 'nothing-matches')).toEqual([])
    const long = entry('long@acme', `${'很长的名字'.repeat(40)}专属插件`, '', '', [])
    expect(filterPrivate([...make(PRIVATE_SEARCH_MIN_ENTRIES), long], '专属')).toEqual([long])
  })
})

describe('dataImageChallengeSrc', () => {
  test('接受已知 mime 的 base64 data URL', () => {
    expect(dataImageChallengeSrc('data:image/png;base64,aGVsbG8=')).toBe('data:image/png;base64,aGVsbG8=')
    expect(dataImageChallengeSrc('data:IMAGE/PNG;charset=utf-8;base64,aGVs bG8=')).toBe('data:image/png;base64,aGVsbG8=')
  })

  test('拒绝非图片、未知格式、非 base64、非法载荷与超限', () => {
    expect(dataImageChallengeSrc('plain-text-challenge')).toBeNull()
    expect(dataImageChallengeSrc('https://example.com/login')).toBeNull()
    expect(dataImageChallengeSrc('data:text/html;base64,aGVsbG8=')).toBeNull()
    expect(dataImageChallengeSrc('data:image/unknown;base64,aGVsbG8=')).toBeNull()
    expect(dataImageChallengeSrc('data:image/png,aGVsbG8=')).toBeNull()
    expect(dataImageChallengeSrc('data:image/png;base64,not base64!')).toBeNull()
    expect(dataImageChallengeSrc(`data:image/png;base64,${'A'.repeat(256 * 1024)}`)).toBeNull()
  })
})

describe('challenge text', () => {
  test('短文本原样，长文本按码点截断并加省略号', () => {
    expect(truncateChallengeText('short')).toBe('short')
    const truncated = truncateChallengeText('码'.repeat(600))
    expect(Array.from(truncated).length).toBe(CHALLENGE_TEXT_LIMIT + 1)
    expect(truncated.endsWith('…')).toBe(true)
  })

  test('只放行 http(s) 链接', () => {
    expect(safeHttpUrl('https://example.com/a')).toBe('https://example.com/a')
    expect(safeHttpUrl('javascript:alert(1)')).toBeNull()
    expect(safeHttpUrl('not a url')).toBeNull()
  })
})

describe('formatBytes', () => {
  test('按 1024 进制换算', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(-5)).toBe('0 B')
    expect(formatBytes(1023)).toBe('1023 B')
    expect(formatBytes(1536)).toBe('1.5 KB')
    expect(formatBytes(12 * 1024 * 1024 + 300 * 1024)).toBe('12.3 MB')
    expect(formatBytes(1024 ** 3)).toBe('1.0 GB')
  })
})

describe('liveDialogPlugins', () => {
  const plugin = (identity: string, pending: string[] = []) => ({ identity, consentPending: pending, consentPendingHosts: [] }) as unknown as PluginDto
  const none = { detail: null, settings: null, auth: null, consent: null }

  test('对话框内容取最新快照，卸载后为 null', () => {
    const v1 = plugin('a@x')
    const open = { ...none, detail: 'a@x', settings: 'a@x', auth: 'b@x' }
    expect(liveDialogPlugins([v1, plugin('b@x')], open)).toMatchObject({ detail: v1, settings: v1 })
    const v2 = plugin('a@x')
    const next = liveDialogPlugins([v2], open)
    expect(next.detail).toBe(v2)
    expect(next.settings).toBe(v2)
    expect(next.auth).toBeNull()
  })

  test('授权框在待授权集合清空后为 null，仍有待授权则取最新快照', () => {
    const open = { ...none, consent: 'a@x' }
    const pending = plugin('a@x', ['ffmpeg'])
    expect(liveDialogPlugins([pending], open).consent).toBe(pending)
    expect(liveDialogPlugins([plugin('a@x')], open).consent).toBeNull()
    expect(liveDialogPlugins([], open).consent).toBeNull()
  })

  test('未打开的对话框恒为 null', () => {
    expect(liveDialogPlugins([plugin('a@x')], none)).toEqual(none)
  })
})
