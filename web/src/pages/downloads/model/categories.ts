// 分类匹配索引（移植 crates/downloads/src/model/categories.rs）：
// 由偏好 `custom_categories` 构建，按扩展名 / 正则 / 网址通配符把任务归类。

import type { CustomCategoryDto } from '../../../lib/rpc'
import { compileWildcard } from '../../../lib/categoryRoute'
import { compileEngineRegex } from '../../rss/filter'
import { extensionOf, shareUrl } from './task'
import type { DownloadTaskView } from './task'

type Matcher =
  | { type: 'all' }
  | { type: 'other' }
  | { type: 'extensions'; set: ReadonlySet<string> }
  | { type: 'regex'; regex: RegExp | null }
  | { type: 'url'; regex: RegExp | null }

export interface CategoryRule {
  dto: CustomCategoryDto
  matcher: Matcher
}

export const BUILTIN_ALL_ID = 'builtin_all'
export const BUILTIN_OTHER_ID = 'builtin_other'

/** 内置分类基线：固定显示顺序与默认扩展名。 */
export function builtinDefaults(): CustomCategoryDto[] {
  const make = (id: string, icon: string, extensions: string[], position: number): CustomCategoryDto => ({
    id: `builtin_${id}`,
    name: '',
    icon,
    matchMode: 'extension',
    extensions,
    regexPattern: '',
    urlPattern: '',
    position,
    visible: true,
    isBuiltin: true,
    builtinType: id,
    saveDir: '',
  })
  return [
    make('all', 'folders', [], 0),
    make('video', 'film', ['mp4', 'mkv', 'avi', 'mov', 'wmv', 'flv', 'webm', 'm4v', 'ts', 'm3u8'], 1),
    make('audio', 'music', ['mp3', 'flac', 'wav', 'aac', 'ogg', 'm4a', 'wma', 'opus'], 2),
    make('document', 'fileText', ['pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'txt', 'epub', 'md'], 3),
    make('image', 'image', ['jpg', 'jpeg', 'png', 'gif', 'webp', 'bmp', 'svg', 'heic', 'avif'], 4),
    make('program', 'cpu', ['exe', 'msi', 'dmg', 'pkg', 'deb', 'rpm', 'apk', 'appimage'], 5),
    make('archive', 'archive', ['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz', 'iso'], 6),
    make('other', 'file', [], 7),
  ]
}

function coerceDto(raw: unknown): CustomCategoryDto | null {
  if (typeof raw !== 'object' || raw === null) return null
  const item = raw as Record<string, unknown>
  if (typeof item.id !== 'string') return null
  return {
    id: item.id,
    name: typeof item.name === 'string' ? item.name : '',
    icon: typeof item.icon === 'string' ? item.icon : 'file',
    matchMode: typeof item.matchMode === 'string' ? item.matchMode : 'extension',
    extensions: Array.isArray(item.extensions) ? item.extensions.filter((ext): ext is string => typeof ext === 'string') : [],
    regexPattern: typeof item.regexPattern === 'string' ? item.regexPattern : '',
    urlPattern: typeof item.urlPattern === 'string' ? item.urlPattern : '',
    position: typeof item.position === 'number' ? item.position : 0,
    visible: item.visible !== false,
    isBuiltin: item.isBuiltin === true,
    builtinType: typeof item.builtinType === 'string' ? item.builtinType : null,
    saveDir: typeof item.saveDir === 'string' ? item.saveDir : '',
  }
}

/** 从偏好值解析分类列表（JSON 字符串或数组）；空 / 损坏时回退内置基线，按 position 排序。 */
export function categoriesFromPreference(value: unknown): CustomCategoryDto[] {
  let raw: unknown = value
  if (typeof raw === 'string') {
    try {
      raw = JSON.parse(raw)
    } catch {
      raw = null
    }
  }
  const parsed = Array.isArray(raw) ? raw.map(coerceDto).filter((dto): dto is CustomCategoryDto => dto !== null) : []
  const list = parsed.length > 0 ? parsed : builtinDefaults()
  return list.slice().sort((a, b) => a.position - b.position)
}

export class CategoryIndex {
  readonly rules: readonly CategoryRule[]
  /** 分类 id → 首条同 id 规则的下标（重复 id 只认首条）。 */
  private readonly firstIndexById = new Map<string, number>()
  /** 具体分类规则（非 all / other），保持规则顺序。 */
  private readonly specific: readonly CategoryRule[]

  constructor(dtos: readonly CustomCategoryDto[]) {
    this.rules = dtos.map((dto): CategoryRule => {
      if (dto.builtinType === 'all') return { dto, matcher: { type: 'all' } }
      if (dto.builtinType === 'other') return { dto, matcher: { type: 'other' } }
      if (dto.matchMode === 'regex') {
        // 与 agent（Rust regex）同语义；Rust 不支持的写法（环视、反向引用）编译失败 → 不匹配。
        const regex = dto.regexPattern === '' ? null : compileEngineRegex(dto.regexPattern)
        return { dto, matcher: { type: 'regex', regex } }
      }
      if (dto.matchMode === 'url') {
        return { dto, matcher: { type: 'url', regex: compileWildcard(dto.urlPattern) } }
      }
      return {
        dto,
        matcher: {
          type: 'extensions',
          set: new Set(dto.extensions.map((ext) => ext.replace(/^\.+/, '').toLowerCase())),
        },
      }
    })
    this.rules.forEach((rule, index) => {
      if (!this.firstIndexById.has(rule.dto.id)) this.firstIndexById.set(rule.dto.id, index)
    })
    this.specific = this.rules.filter((rule) => rule.matcher.type !== 'all' && rule.matcher.type !== 'other')
  }

  /** 可见规则（`visible=false` 的隐藏）。 */
  visible(): CategoryRule[] {
    return this.rules.filter((rule) => rule.dto.visible)
  }

  /** 分类 id 在规则列表中的位置（重复 id 取首条）；未知返回 -1。 */
  indexOf(id: string): number {
    return this.firstIndexById.get(id) ?? -1
  }

  /** `ext` 为任务扩展名；扫描多条规则时由调用方算一次传入，省略则现算。 */
  private static ruleMatches(rule: CategoryRule, view: DownloadTaskView, ext: string | null = extensionOf(view.name)): boolean {
    const matcher = rule.matcher
    switch (matcher.type) {
      case 'all':
        return true
      case 'other':
        return false
      case 'extensions':
        return ext !== null && matcher.set.has(ext)
      case 'regex':
        return matcher.regex !== null && matcher.regex.test(view.name)
      case 'url':
        return matcher.regex !== null && matcher.regex.test(shareUrl(view))
    }
  }

  matches(id: string, view: DownloadTaskView): boolean {
    const override = view.dto?.properties?.category ?? ''
    if (override !== '') return id === BUILTIN_ALL_ID || id === override
    const index = this.firstIndexById.get(id)
    if (index === undefined) return false
    const rule = this.rules[index] as CategoryRule
    if (rule.matcher.type === 'all') return true
    if (rule.matcher.type === 'other') return !this.specific.some((other) => CategoryIndex.ruleMatches(other, view))
    return CategoryIndex.ruleMatches(rule, view)
  }

  categoryOf(view: DownloadTaskView): string {
    const override = view.dto?.properties?.category ?? ''
    if (override !== '') return override
    const isUrl = (rule: CategoryRule): boolean => rule.matcher.type === 'url'
    const hit =
      this.specific.find((rule) => isUrl(rule) && CategoryIndex.ruleMatches(rule, view)) ??
      this.specific.find((rule) => !isUrl(rule) && CategoryIndex.ruleMatches(rule, view))
    return hit ? hit.dto.id : BUILTIN_OTHER_ID
  }

  /**
   * 对任务命中的每个分类 id 调用一次 `emit`：与对每个不同 id 逐个调用 {@link matches} 等价
   * （重复 id 只认首条规则；`other` 仅在没有任何具体分类命中时命中），但每条规则只对任务求值一次，
   * 供一次扫描得出全部分类计数（对应 Rust `CategoryIndex::for_each_match`）。
   */
  forEachMatch(view: DownloadTaskView, emit: (id: string) => void): void {
    const override = view.dto?.properties?.category ?? ''
    if (override !== '') {
      emit(BUILTIN_ALL_ID)
      if (this.firstIndexById.has(override)) emit(override)
      return
    }
    let specificHit = false
    let otherIds: string[] | null = null
    const ext = extensionOf(view.name)
    for (let index = 0; index < this.rules.length; index += 1) {
      const rule = this.rules[index] as CategoryRule
      const type = rule.matcher.type
      const hit = type !== 'other' && CategoryIndex.ruleMatches(rule, view, ext)
      if (hit && type !== 'all') specificHit = true
      if (this.firstIndexById.get(rule.dto.id) !== index) continue
      if (type === 'other') (otherIds ??= []).push(rule.dto.id)
      else if (hit) emit(rule.dto.id)
    }
    if (!specificHit && otherIds !== null) {
      for (const id of otherIds) emit(id)
    }
  }
}
