import { describe, expect, test } from 'bun:test'
import type { CustomCategoryDto } from './rpc'
import { autoFollowDir, dirUntouched, matchedCategory, modeDir, pickableCategories, routeBaseDir, routeFollows, routeFromCategories, routeFromPreference, routeSaveDir, wildcardMatches } from './categoryRoute'
import type { CategoryRouteDto } from './rpc'

declare const Bun: { file(path: URL): { json(): Promise<unknown> } }
const routeCases = (await Bun.file(new URL('../../../native/engine/src/fixtures/category_route_cases.json', import.meta.url)).json()) as {
  name: string
  route: CategoryRouteDto
  fileName: string
  url: string
  expected: string | null
}[]

const cat = (id: string, patch: Partial<CustomCategoryDto> = {}): CustomCategoryDto => ({
  id,
  name: id,
  icon: '',
  matchMode: 'extension',
  extensions: [],
  regexPattern: '',
  urlPattern: '',
  position: 0,
  visible: true,
  isBuiltin: false,
  builtinType: null,
  saveDir: '',
  ...patch,
})

describe('routeFromCategories', () => {
  test('没有任何目录 → null', () => {
    expect(routeFromCategories([cat('all', { builtinType: 'all' }), cat('v', { extensions: ['mp4'] })])).toBeNull()
  })

  test('other 无目录：无目录的普通分类省略，有目录的保留', () => {
    const route = routeFromCategories([
      cat('all', { builtinType: 'all', saveDir: '/ignored' }),
      cat('a', { extensions: ['zip'] }),
      cat('b', { extensions: ['mp4'], saveDir: ' /v ' }),
      cat('other', { builtinType: 'other' }),
    ])
    expect(route).toEqual({ rules: [{ matchMode: 'extension', extensions: ['mp4'], saveDir: '/v' }] })
  })

  test('other 有目录：无目录的普通分类保留以阻止兜底；隐藏分类忽略', () => {
    const route = routeFromCategories([
      cat('a', { matchMode: 'regex', regexPattern: '^x' }),
      cat('hidden', { saveDir: '/h', visible: false }),
      cat('other', { builtinType: 'other', saveDir: '/o' }),
    ])
    expect(route).toEqual({ rules: [{ matchMode: 'regex', regexPattern: '^x' }], otherDir: '/o' })
  })

  test('偏好值解析（数组 / JSON 字符串）', () => {
    const list = [cat('b', { extensions: ['mp4'], saveDir: '/v' })]
    const expected = { rules: [{ matchMode: 'extension', extensions: ['mp4'], saveDir: '/v' }] }
    expect(routeFromPreference(list)).toEqual(expected)
    expect(routeFromPreference(JSON.stringify(list))).toEqual(expected)
  })
})

describe('follows / baseDir', () => {
  const route = { rules: [{ matchMode: 'extension', saveDir: '/v' }, { matchMode: 'extension' }], otherDir: '/o' }

  test('空、等于提供目录、分类目录均视为跟随', () => {
    expect(routeFollows(route, '  ', '/d')).toBe(true)
    expect(routeFollows(route, ' /d ', '/d')).toBe(true)
    expect(routeFollows(route, '/v', '/d')).toBe(true)
    expect(routeFollows(route, '/o', '')).toBe(true)
    expect(routeFollows(route, '/custom', '/d')).toBe(false)
    expect(routeFollows(route, '/d', '')).toBe(false)
  })

  test('baseDir：空 / 分类目录 → offered，否则提交值', () => {
    expect(routeBaseDir(route, '', ' /d ')).toBe('/d')
    expect(routeBaseDir(route, '/v', '/d')).toBe('/d')
    expect(routeBaseDir(route, '/o', '/d')).toBe('/d')
    expect(routeBaseDir(route, ' /d ', '/d')).toBe('/d')
  })
})

describe('routeSaveDir (shared fixtures)', () => {
  for (const item of routeCases) {
    test(item.name, () => {
      expect(routeSaveDir(item.route, item.fileName, item.url)).toBe(item.expected)
    })
  }
})

describe('wildcardMatches / url 规则', () => {
  test('通配符语义（* 任意串、? 单字符、不分大小写、锚定）', () => {
    expect(wildcardMatches('https://example.com/*', 'https://example.com/a/b.zip?token=1')).toBe(true)
    expect(wildcardMatches('HTTPS://X.TEST/file?.zip', 'https://x.test/fileA.zip')).toBe(true)
    expect(wildcardMatches('https://x.test/file?.zip', 'https://x.test/file12.zip')).toBe(false)
    expect(wildcardMatches('x.test/*', 'https://x.test/a')).toBe(false)
    expect(wildcardMatches('https://*.example.com/*', 'https://example.com/a')).toBe(false)
    expect(wildcardMatches('', 'https://x.test/a')).toBe(false)
    expect(wildcardMatches('   ', 'https://x.test/a')).toBe(false)
  })

  test('url 模式规则随 routeFromCategories 传递并参与落点路由', () => {
    const route = routeFromCategories([cat('site', { matchMode: 'url', urlPattern: 'https://mirror.test/*', saveDir: '/mirror' })])
    expect(route).toEqual({ rules: [{ matchMode: 'url', urlPattern: 'https://mirror.test/*', saveDir: '/mirror' }] })
    expect(routeSaveDir(route!, 'a.zip', 'https://mirror.test/a.zip')).toBe('/mirror')
  })
})

describe('auto follow / mode', () => {
  test('空、等于程序最近写入值才跟随；从未写入时等于 offered', () => {
    expect(autoFollowDir('', '/d', null, '/v')).toBe('/v')
    expect(autoFollowDir(' /d ', '/d', null, '/v')).toBe('/v')
    expect(autoFollowDir('/v', '/d', '/v', null)).toBe('/d')
    expect(autoFollowDir('/custom', '/d', '/v', '/p')).toBeNull()
  })

  test('回归：自动命中分类目录后用户改回 offered，不被覆盖且提交不路由', () => {
    // 自动写入 /v 后，用户手改回默认目录 /d：lastWritten=/v，不再视为未手改。
    expect(dirUntouched('/d', '/d', '/v')).toBe(false)
    expect(autoFollowDir('/d', '/d', '/v', '/p')).toBeNull()
    // 用户改成另一个分类目录同理。
    expect(dirUntouched('/p', '/d', '/v')).toBe(false)
    // 未手改：等于 lastWritten 或为空。
    expect(dirUntouched(' /v ', '/d', '/v')).toBe(true)
    expect(dirUntouched('  ', '/d', '/v')).toBe(true)
    expect(dirUntouched('/d', ' /d ', null)).toBe(true)
  })

  test('可选分类过滤与命中', () => {
    const list = [
      cat('all', { builtinType: 'all', saveDir: '/a' }),
      cat('v', { saveDir: '/v' }),
      cat('h', { saveDir: '/h', visible: false }),
      cat('n'),
      cat('other', { builtinType: 'other', saveDir: '/o' }),
    ]
    const options = pickableCategories(list)
    expect(options.map((c) => c.id)).toEqual(['v', 'other'])
    expect(matchedCategory(options, '/o')?.id).toBe('other')
    expect(matchedCategory(options, '/x')).toBeNull()
    expect(modeDir({ kind: 'category', id: 'v' }, options, '/d', null)).toBe('/v')
    expect(modeDir({ kind: 'none' }, options, '/d', '/v')).toBe('/d')
    expect(modeDir({ kind: 'auto' }, options, '/d', null)).toBe('/d')
  })
})
