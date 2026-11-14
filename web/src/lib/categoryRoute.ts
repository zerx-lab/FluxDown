// 分类目录路由（移植 native/protocol/src/agent.rs `CategoryRouteDto`）：
// from_categories / follows / base_dir。分类列表解析复用下载页的 `categoriesFromPreference`。

import { categoriesFromPreference } from '../pages/downloads/model/categories'
import { compileEngineRegex } from '../pages/rss/filter'
import type { CategoryRouteDto, CustomCategoryDto } from './rpc'

/** 由分类列表构建已授权路由；没有分类目录时也返回空规则路由，供插件归档分类使用。 */
export function routeFromCategories(categories: readonly CustomCategoryDto[]): CategoryRouteDto {
  const visible = categories.filter((category) => category.visible)
  const otherDir = visible.find((category) => category.builtinType === 'other')?.saveDir.trim() ?? ''
  const rules = visible
    .filter((category) => category.builtinType !== 'all' && category.builtinType !== 'other' && (otherDir !== '' || category.saveDir.trim() !== ''))
    .map((category) => {
      const rule: NonNullable<CategoryRouteDto['rules']>[number] = { matchMode: category.matchMode }
      if (category.extensions.length > 0) rule.extensions = category.extensions
      if (category.regexPattern !== '') rule.regexPattern = category.regexPattern
      if (category.urlPattern) rule.urlPattern = category.urlPattern
      const dir = category.saveDir.trim()
      if (dir !== '') rule.saveDir = dir
      return rule
    })
  const archiveCategories = Object.fromEntries(visible
    .filter((category) => category.builtinType && category.builtinType !== 'all' && category.saveDir.trim() !== '')
    .map((category) => [category.builtinType!, category.saveDir.trim()]))
  return {
    rules,
    ...(otherDir !== '' ? { otherDir } : {}),
    ...(Object.keys(archiveCategories).length ? { archiveCategories } : {}),
  }
}

/** 由 `custom_categories` 偏好值构建路由。 */
export function routeFromPreference(value: unknown): CategoryRouteDto {
  return routeFromCategories(categoriesFromPreference(value))
}

/** 提交目录为空、等于提供目录，或是某个分类目录时，认为用户没有手选目录。 */
export function routeFollows(route: CategoryRouteDto, submitted: string, offered: string): boolean {
  const dir = submitted.trim()
  if (dir === '') return true
  const base = offered.trim()
  if (base !== '' && dir === base) return true
  if (route.otherDir && dir === route.otherDir) return true
  return (route.rules ?? []).some((rule) => !!rule.saveDir && rule.saveDir === dir)
}

/** following 时作为 `saveDir` 下发的基准目录（无分类命中时使用）。 */
export function routeBaseDir(route: CategoryRouteDto, submitted: string, offered: string): string {
  const dir = submitted.trim()
  if (dir === '') return offered.trim()
  const isCategoryDir = (!!route.otherDir && dir === route.otherDir) || (route.rules ?? []).some((rule) => !!rule.saveDir && rule.saveDir === dir)
  return isCategoryDir ? offered.trim() : dir
}

/** 把 `%XX` 按字节解码（非法序列原样保留，非法 UTF-8 用替换符），对齐 agent `percent_decode`。 */
function percentDecode(input: string): string {
  const bytes: number[] = []
  const encoder = new TextEncoder()
  for (let index = 0; index < input.length; ) {
    const hex = input[index] === '%' ? input.slice(index + 1, index + 3) : ''
    if (/^[0-9a-fA-F]{2}$/.test(hex)) {
      bytes.push(Number.parseInt(hex, 16))
      index += 3
    } else {
      const code = input.codePointAt(index) ?? 0
      bytes.push(...encoder.encode(String.fromCodePoint(code)))
      index += code > 0xffff ? 2 : 1
    }
  }
  return new TextDecoder().decode(new Uint8Array(bytes))
}

/** URL 路径末段（百分号解码）且含 `.` 时作为文件名。 */
function fileNameFromUrl(url: string): string | null {
  let parsed: URL
  try {
    parsed = new URL(url.trim())
  } catch {
    return null
  }
  const last = parsed.pathname.split('/').pop() ?? ''
  const decoded = percentDecode(last)
  return decoded.includes('.') ? decoded : null
}

/**
 * 通配符网址匹配，语义与 agent `category_dir.rs::wildcard_matches` / 引擎
 * `category_route.rs::wildcard_matches` 一致：`*` 任意串、`?` 恰一个字符，其余字面；
 * 不分大小写；锚定完整匹配整个网址；空 / 非法模式不命中。
 */
export function compileWildcard(pattern: string): RegExp | null {
  const trimmed = pattern.trim()
  if (trimmed === '') return null
  let source = '^'
  for (const ch of trimmed) {
    if (ch === '*') source += '.*'
    else if (ch === '?') source += '.'
    else source += ch.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  }
  source += '$'
  try {
    return new RegExp(source, 'iu')
  } catch {
    return null
  }
}

export function wildcardMatches(pattern: string, url: string): boolean {
  return compileWildcard(pattern)?.test(url) ?? false
}

function ruleMatches(rule: NonNullable<CategoryRouteDto['rules']>[number], name: string, url: string): boolean {
  if (rule.matchMode === 'url') return wildcardMatches(rule.urlPattern ?? '', url)
  if (rule.matchMode === 'regex') {
    const pattern = rule.regexPattern ?? ''
    if (pattern === '') return false
    const regex = compileEngineRegex(pattern)
    return regex !== null && new RegExp(regex.source, regex.flags.replace(/[gy]/g, '')).test(name)
  }
  const dot = name.lastIndexOf('.')
  if (dot < 0) return false
  const extension = name.slice(dot + 1).toLowerCase()
  if (extension === '') return false
  return (rule.extensions ?? []).some((candidate) => candidate.replace(/^\.+/, '').toLowerCase() === extension)
}

/**
 * 命中的分类保存目录；无命中 / 命中分类未配置目录时为 null。
 * 网址规则固定优先（不看列表位置）：先按列表顺序取首个命中且配置了目录的 url 规则，
 * 命中即定落点；扩展名 / 正则分类只处理未被网址规则认领的任务。
 * 语义同 agent `category_dir.rs::route_save_dir`（夹具 `native/engine/src/fixtures/category_route_cases.json`）。
 */
export function routeSaveDir(route: CategoryRouteDto, fileName: string, url: string): string | null {
  const rules = route.rules ?? []
  const urlHit = rules.find(
    (rule) => rule.matchMode === 'url' && !!rule.saveDir && wildcardMatches(rule.urlPattern ?? '', url),
  )
  if (urlHit?.saveDir) return urlHit.saveDir
  const name = fileName.includes('.') ? fileName : (fileNameFromUrl(url) ?? fileName)
  // 文件名未知时只有网址规则参与（上面的网址优先层已返回）。
  if (name === '') return null
  const hit = rules.find((rule) => !!rule.saveDir && ruleMatches(rule, name, url))
  if (hit?.saveDir) return hit.saveDir
  if (route.otherDir && !rules.some((rule) => ruleMatches(rule, name, url))) return route.otherDir
  return null
}

/** 分类下拉的模式：自动 / 不分类 / 指定分类 id。 */
export type CategoryPickMode = { kind: 'auto' } | { kind: 'none' } | { kind: 'category'; id: string }

/** 设备本地偏好：新建下载分类模式记忆（`auto` / `none` / `category:<id>`，不进云同步）。 */
export const LAST_CATEGORY_MODE_PREF = 'download.last_category_mode'

/** Device-local remembered picker mode; malformed or stale category ids fall back to Auto. */
export function categoryModeFromPreference(value: unknown, categories: readonly CustomCategoryDto[]): CategoryPickMode {
  if (value === 'none') return { kind: 'none' }
  if (typeof value !== 'string' || !value.startsWith('category:')) return { kind: 'auto' }
  const id = value.slice('category:'.length)
  return pickableCategories(categories).some((category) => category.id === id) ? { kind: 'category', id } : { kind: 'auto' }
}

export function categoryModePreference(mode: CategoryPickMode): string {
  return mode.kind === 'category' ? `category:${mode.id}` : mode.kind
}

/** 可选分类：visible、非 all、目录非空，按 position 排序（`categories` 已排序）。 */
export function pickableCategories(categories: readonly CustomCategoryDto[]): CustomCategoryDto[] {
  return categories.filter((category) => category.visible && category.builtinType !== 'all' && category.saveDir.trim() !== '')
}

/** 预测目录对应的分类：可选列表中第一个目录等于预测目录的项。 */
export function matchedCategory(options: readonly CustomCategoryDto[], predicted: string | null): CustomCategoryDto | null {
  if (predicted === null) return null
  return options.find((category) => category.saveDir.trim() === predicted) ?? null
}

/**
 * 目录框是否仍是「程序给的」（未被用户手改）：空、等于程序最近一次写入值；
 * 表单尚未写过目录框（lastWritten 为空）时等于 offered。
 */
export function dirUntouched(current: string, offered: string, lastWritten: string | null): boolean {
  const dir = current.trim()
  if (dir === '') return true
  if (lastWritten !== null && lastWritten !== '') return dir === lastWritten
  return dir === offered.trim()
}

/**
 * Auto 模式下预测输入变化时目录框应改写成的值；用户手改过（非 `dirUntouched`）返回 null。
 */
export function autoFollowDir(current: string, offered: string, lastWritten: string | null, predicted: string | null, manuallyChosen = false): string | null {
  if (manuallyChosen || !dirUntouched(current, offered, lastWritten)) return null
  return predicted ?? offered.trim()
}

/** 模式对应的目录框值（Auto 取预测或 offered）。 */
export function modeDir(mode: CategoryPickMode, categories: readonly CustomCategoryDto[], offered: string, predicted: string | null): string {
  if (mode.kind === 'auto') return predicted ?? offered
  if (mode.kind === 'none') return offered
  const category = categories.find((item) => item.id === mode.id)
  return category ? category.saveDir.trim() : offered
}
/** Route is submitted only while Auto is active and the directory still follows the offered/category route. */
export function eligibleRoute(
  route: CategoryRouteDto | null,
  mode: CategoryPickMode,
  manuallyChosen: boolean,
  submitted: string,
  offered: string,
): CategoryRouteDto | null {
  return route !== null && mode.kind === 'auto' && !manuallyChosen && routeFollows(route, submitted, offered) ? route : null
}
