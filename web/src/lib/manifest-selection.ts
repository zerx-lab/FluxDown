// 预解析清单（ResolvePreviewResponse.items）→ 建组选择弹窗的纯逻辑层。
//
// 移植自 lib/src/models/manifest_selection.dart + manifest_breadcrumb.dart（v1.6 下钻导航版，
// 已与 Flutter 参照对齐确认）。不依赖 React，可独立单测：可见性判定（扩展名筛选 + 搜索）、
// 当前层行流生成（单链目录合并 + 目录/文件排序）、返回上级跳级、目录子树 id 收集/三态勾选、
// 全局选中统计、扩展名频次 top7、面包屑分段（含 >4 段折叠）、CreateGroupRequest.items 投影。
//
// v1.6 裁决：画质规格策略（variants）/ 文件类型意图按钮组 / 树形缩进渲染均已砍除——
// resolverItem 恒为 `item.id`（不带 `@variantId`），规格选择留给插件默认档。
//
// 排序说明：目录名/文件名排序用普通字符串比较（UTF-16 code unit 序），非 locale-aware
// collation，对齐 Dart 版 `String.compareTo` 的既有约定（非行为偏离，是移植前既有实现）。

import { fileType, type FileType } from './format'
import type { GroupItemRequest, PreviewItemDto } from './types'

// =============================================================================
// 1. 可见性（扩展名筛选 ∧ 搜索词匹配）
// =============================================================================

/** 从文件名提取扩展名（不含点号，小写）；无扩展名（或以 `.` 开头的隐藏文件）返回空串。 */
export function manifestFileExtension(fileName: string): string {
  const idx = fileName.lastIndexOf('.')
  if (idx <= 0 || idx === fileName.length - 1) return ''
  return fileName.slice(idx + 1).toLowerCase()
}

/** 扩展名筛选 chip / 行内展示用：大写，无扩展名回退 `"FILE"`。 */
export function manifestExtensionLabel(fileName: string): string {
  const ext = manifestFileExtension(fileName)
  return ext === '' ? 'FILE' : ext.toUpperCase()
}

/** 文件类型分类（文件行扩展名色块 tile 着色用），复用 format.ts 的 8 类判定。 */
export function manifestItemCategory(item: PreviewItemDto): FileType {
  return fileType(item.name)
}

/** 当前搜索词是否处于「搜索态」（非空白）。搜索态下列表切换为全局扁平结果。 */
export function manifestIsSearching(search: string): boolean {
  return search.trim().length > 0
}

export interface ManifestVisibilityFilter {
  extFilter: Set<string>
  search: string
}

/** 条目是否在当前筛选（扩展名 chips）+ 搜索词下可见。 */
export function manifestItemVisible(item: PreviewItemDto, { extFilter, search }: ManifestVisibilityFilter): boolean {
  if (extFilter.size > 0 && !extFilter.has(manifestExtensionLabel(item.name))) return false
  const q = search.trim().toLowerCase()
  if (q === '') return true
  return item.name.toLowerCase().includes(q)
}

// =============================================================================
// 0. 触发条件（供 new-download.tsx 判定是否先预解析）
// =============================================================================

/** 单条链接是否值得先探测多文件清单：仅 http(s)，磁力/种子/ed2k 等协议恒不匹配（对齐
 *  Flutter new_download_dialog.dart `_isPreviewableUrl`）。多行 URL 由调用方另行判定。 */
export function manifestIsPreviewableUrl(url: string): boolean {
  const lower = url.trim().toLowerCase()
  return lower.startsWith('http://') || lower.startsWith('https://')
}

// =============================================================================
// 2. 扩展名频次 top7（工具栏 chips，恒对全量条目统计，不随当前筛选变化）
// =============================================================================

export interface ManifestExtChip {
  ext: string
  count: number
}

/** 按出现频次取前 limit 个扩展名（计数相同时按扩展名本身排序，保证结果确定性）。 */
export function manifestTopExtensions(items: PreviewItemDto[], limit = 7): ManifestExtChip[] {
  const counts = new Map<string, number>()
  for (const it of items) {
    const ext = manifestExtensionLabel(it.name)
    counts.set(ext, (counts.get(ext) ?? 0) + 1)
  }
  const chips = [...counts.entries()].map(([ext, count]) => ({ ext, count }))
  chips.sort((a, b) => b.count - a.count || (a.ext < b.ext ? -1 : a.ext > b.ext ? 1 : 0))
  return chips.length > limit ? chips.slice(0, limit) : chips
}

// =============================================================================
// 3. 下钻导航：当前层行流（单链合并 + 目录/文件排序，零缩进）
// =============================================================================

export type ManifestSortKey = 'name' | 'size'

/** 一条当前层的目录行：单链合并后的展示（labels = 折叠链各段名，UI 拼接为 `a / b / c`，
 *  末段加粗）；path = 链末端真实目录的完整路径（导航目标 / 选中集合 key）。 */
export interface ManifestDirRow {
  path: string
  labels: string[]
  count: number
  size: number
  selCnt: number
  unknown: boolean
}

/** 一条文件行；showPath 仅搜索模式（全局扁平结果）为 true。 */
export interface ManifestFileRow {
  item: PreviewItemDto
  showPath: boolean
}

export type ManifestRow = { kind: 'dir'; row: ManifestDirRow } | { kind: 'file'; row: ManifestFileRow }

/** manifestRowsAt 的结果：cwd 是筛选后落地的实际当前目录（请求的层被筛空时回退根 `""`，
 *  调用方应据此同步 cwd）。 */
export interface ManifestRowsResult {
  cwd: string
  rows: ManifestRow[]
}

/** 目录构建的内部节点（仅本文件内使用）：按名分组的子目录 + 该层直属文件。 */
class DirBuilder {
  dirs = new Map<string, DirBuilder>()
  files: PreviewItemDto[] = []
}

/** 由可见 items 构建完整目录树（按 path 以 `/` 拆分目录段）。 */
function buildVisibleDirTree(items: PreviewItemDto[], filter: ManifestVisibilityFilter): DirBuilder {
  const root = new DirBuilder()
  for (const item of items) {
    if (!manifestItemVisible(item, filter)) continue
    let node = root
    for (const seg of item.path.split('/').filter((s) => s !== '')) {
      let next = node.dirs.get(seg)
      if (!next) {
        next = new DirBuilder()
        node.dirs.set(seg, next)
      }
      node = next
    }
    node.files.push(item)
  }
  return root
}

/** 按 `/` 拆分的路径在树中定位节点；筛选后该层不存在时返回 null（调用方回退根）。 */
function nodeAt(root: DirBuilder, path: string): DirBuilder | null {
  if (path === '') return root
  let node = root
  for (const seg of path.split('/')) {
    const next = node.dirs.get(seg)
    if (!next) return null
    node = next
  }
  return node
}

interface DirAggregate {
  count: number
  size: number
  selCnt: number
  unknown: boolean
}

/** 子树内全部文件的聚合统计：计数 / Σsize（size===0 视为未知，不计入总和但置 unknown）/
 *  已选计数 / 是否含未知大小项。 */
function dirStat(node: DirBuilder, selectedItemIds: Set<string>): DirAggregate {
  let count = 0
  let size = 0
  let selCnt = 0
  let unknown = false
  const walk = (n: DirBuilder) => {
    for (const f of n.files) {
      count++
      if (f.size === 0) unknown = true
      else size += f.size
      if (selectedItemIds.has(f.id)) selCnt++
    }
    for (const d of n.dirs.values()) walk(d)
  }
  walk(node)
  return { count, size, selCnt, unknown }
}

/** 文件排序键：size 时未知大小（size===0）视为 -1，排到末尾。 */
function fileSizeRank(item: PreviewItemDto): number {
  return item.size === 0 ? -1 : item.size
}

function sortFiles(files: PreviewItemDto[], key: ManifestSortKey): PreviewItemDto[] {
  const list = [...files]
  if (key === 'size') list.sort((a, b) => fileSizeRank(b) - fileSizeRank(a))
  else list.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
  return list
}

export interface ManifestRowsAtParams extends ManifestVisibilityFilter {
  items: PreviewItemDto[]
  cwd: string
  selectedItemIds: Set<string>
  sortKey: ManifestSortKey
}

/** 当前层行流：搜索态返回全局扁平结果（文件行 showPath=true）；否则返回 cwd 直属子目录
 *  （单链合并）+ 直属文件（目录恒在前，各自再按 sortKey 排序——目录恒按名排序，只有文件
 *  排序受 sortKey 影响）。cwd 被筛空时结果的 cwd 回退为 `""`。 */
export function manifestRowsAt({ items, cwd, selectedItemIds, extFilter, search, sortKey }: ManifestRowsAtParams): ManifestRowsResult {
  if (manifestIsSearching(search)) {
    const visible = items.filter((it) => manifestItemVisible(it, { extFilter, search }))
    const sorted = sortFiles(visible, sortKey)
    return {
      cwd,
      rows: sorted.map((it): ManifestRow => ({ kind: 'file', row: { item: it, showPath: true } })),
    }
  }

  const root = buildVisibleDirTree(items, { extFilter, search })
  let node = nodeAt(root, cwd)
  let effectiveCwd = cwd
  if (!node) {
    node = root
    effectiveCwd = ''
  }

  const rows: ManifestRow[] = []
  const dirNames = [...node.dirs.keys()].sort()
  for (const name of dirNames) {
    let childPath = effectiveCwd === '' ? name : `${effectiveCwd}/${name}`
    let child = node.dirs.get(name)!
    const labels = [name]
    // 单链合并：仅单个子目录且无直属文件的链持续下潜合并，直到遇到分叉（>1 子项）或该级
    // 出现文件为止。
    while (child.files.length === 0 && child.dirs.size === 1) {
      const nextName = [...child.dirs.keys()][0]
      labels.push(nextName)
      childPath = `${childPath}/${nextName}`
      child = child.dirs.get(nextName)!
    }
    const stat = dirStat(child, selectedItemIds)
    rows.push({
      kind: 'dir',
      row: { path: childPath, labels, count: stat.count, size: stat.size, selCnt: stat.selCnt, unknown: stat.unknown },
    })
  }
  for (const it of sortFiles(node.files, sortKey)) {
    rows.push({ kind: 'file', row: { item: it, showPath: false } })
  }
  return { cwd: effectiveCwd, rows }
}

// =============================================================================
// 4. 目录三态勾选 / 子树选择
// =============================================================================

export type ManifestCheckState = 'checked' | 'unchecked' | 'indeterminate'

/** 由行内已计算好的 selCnt/count 推导三态（无需重新遍历）。 */
export function manifestDirRowCheckState(row: ManifestDirRow): ManifestCheckState {
  if (row.selCnt === 0) return 'unchecked'
  if (row.selCnt === row.count) return 'checked'
  return 'indeterminate'
}

export interface ManifestDirFileIdsParams extends ManifestVisibilityFilter {
  items: PreviewItemDto[]
  dirPath: string
}

/** 目录子树（dirPath 或其任意下级路径）下全部可见文件 id（用于目录行勾选框的整树选择/取消）。 */
export function manifestDirFileIds({ items, dirPath, extFilter, search }: ManifestDirFileIdsParams): Set<string> {
  const result = new Set<string>()
  for (const it of items) {
    if (!manifestItemVisible(it, { extFilter, search })) continue
    if (it.path === dirPath || it.path.startsWith(`${dirPath}/`)) result.add(it.id)
  }
  return result
}

export interface ManifestToggleDirSubtreeParams extends ManifestDirFileIdsParams {
  selectedItemIds: Set<string>
}

/** 切换目录子树选择：子树内文件全部已选则整体取消，否则整体选中。 */
export function manifestToggleDirSubtree({ items, dirPath, selectedItemIds, extFilter, search }: ManifestToggleDirSubtreeParams): Set<string> {
  const ids = manifestDirFileIds({ items, dirPath, extFilter, search })
  const allSelected = [...ids].every((id) => selectedItemIds.has(id))
  const next = new Set(selectedItemIds)
  if (allSelected) {
    for (const id of ids) next.delete(id)
  } else {
    for (const id of ids) next.add(id)
  }
  return next
}

// =============================================================================
// 5. 返回上级（跳过纯过渡层）
// =============================================================================

/** 返回上级：逐段 pop 直到该层有直属文件、或有 >1 个子目录、或到根——纯过渡层（单链合并链
 *  的中间段）不是独立可停留的层级，去程回程都不经过。搜索态不调用本函数。 */
export function manifestUpPath({ items, cwd, extFilter }: { items: PreviewItemDto[]; cwd: string; extFilter: Set<string> }): string {
  if (cwd === '') return ''
  const root = buildVisibleDirTree(items, { extFilter, search: '' })
  const segs = cwd.split('/')
  do {
    segs.pop()
    const node = nodeAt(root, segs.join('/'))
    if (segs.length === 0 || (node !== null && (node.files.length > 0 || node.dirs.size > 1))) break
  } while (segs.length > 0)
  return segs.join('/')
}

// =============================================================================
// 6. 全局选择操作（作用域 = 全部可见文件，跨层级）
// =============================================================================

/** 全选：整体替换为当前可见文件集合（筛选范围外此前已选的条目会被丢弃，作用域纪律的直接
 *  结果，非增量并集）。 */
export function manifestSelectAllVisible(items: PreviewItemDto[], { extFilter, search }: ManifestVisibilityFilter): Set<string> {
  const result = new Set<string>()
  for (const it of items) if (manifestItemVisible(it, { extFilter, search })) result.add(it.id)
  return result
}

/** 反选：整体替换为「当前可见且此前未选中」的集合（同上，非增量）。 */
export function manifestInvertVisibleSelection(
  items: PreviewItemDto[],
  selectedItemIds: Set<string>,
  { extFilter, search }: ManifestVisibilityFilter,
): Set<string> {
  const result = new Set<string>()
  for (const it of items) {
    if (!manifestItemVisible(it, { extFilter, search })) continue
    if (!selectedItemIds.has(it.id)) result.add(it.id)
  }
  return result
}

// =============================================================================
// 7. 选中统计 / 清单汇总
// =============================================================================

export interface ManifestSelectionStat {
  count: number
  size: number
  unknownCount: number
}

/** 全局已选统计：计数 / Σsize（size===0 视为未知，计入 unknownCount 但不计入 size）。 */
export function manifestSelectionStat(items: PreviewItemDto[], selectedItemIds: Set<string>): ManifestSelectionStat {
  let size = 0
  let unknown = 0
  for (const it of items) {
    if (!selectedItemIds.has(it.id)) continue
    if (it.size === 0) unknown++
    else size += it.size
  }
  return { count: selectedItemIds.size, size, unknownCount: unknown }
}

/** 清单总大小（摘要区「N 项 · 总大小」，不做未知标注）。 */
export function manifestTotalSize(items: PreviewItemDto[]): number {
  return items.reduce((sum, i) => sum + i.size, 0)
}

// =============================================================================
// 8. 组名默认值 / 来源站点
// =============================================================================

/** 组名默认值：优先用清单自带的 name；为空时退化用来源 URL 最后一段（去查询串/末尾斜杠）；
 *  全部拿不到时返回空串——交调用方套用本地化占位符。 */
export function manifestDefaultGroupName(manifestName: string, sourceUrl: string): string {
  const trimmed = manifestName.trim()
  if (trimmed !== '') return trimmed
  try {
    const url = new URL(sourceUrl)
    const segments = url.pathname.split('/').filter((s) => s !== '')
    if (segments.length > 0) return decodeURIComponent(segments[segments.length - 1])
  } catch {
    // 解析失败：忽略，落到下方空串兜底。
  }
  return ''
}

/** 摘要区副标题「来源站点」：URL host；解析失败返回空串（调用方隐藏该片段）。 */
export function manifestSourceHost(sourceUrl: string): string {
  try {
    return new URL(sourceUrl).host
  } catch {
    return ''
  }
}

// =============================================================================
// 9. 高级选项（组级，随 CreateGroupRequest 下发全部子任务）
// =============================================================================

export interface ManifestHeaderEntry {
  key: string
  value: string
}

/** 组级高级选项快照（纯数据，供 dirty 判定用；文本字段在 UI 层用 React state 承载）。 */
export interface ManifestAdvancedOptions {
  proxyUrl: string
  ignoreTlsErrors: boolean
  /** true = 继承全局 UA（发送时 userAgent 应发空串）；false = 自定义。 */
  uaInherit: boolean
  userAgent: string
  cookies: string
  /** 每子任务线程数；0 = 自动。 */
  segments: number
  headers: ManifestHeaderEntry[]
}

/** 高级选项是否偏离默认（折叠条圆点用）。 */
export function manifestAdvancedOptionsDirty(options: ManifestAdvancedOptions): boolean {
  return (
    options.proxyUrl.trim() !== '' ||
    options.ignoreTlsErrors ||
    (!options.uaInherit && options.userAgent.trim() !== '') ||
    options.cookies.trim() !== '' ||
    options.segments !== 0 ||
    options.headers.some((h) => h.key.trim() !== '' || h.value.trim() !== '')
  )
}

/** 自定义请求头行 → 生效 Map：丢弃 key 或 value 为空的行，同名 key 后者覆盖前者。 */
export function manifestEffectiveHeaders(headers: ManifestHeaderEntry[]): Record<string, string> {
  const result: Record<string, string> = {}
  for (const h of headers) {
    const key = h.key.trim()
    const value = h.value.trim()
    if (key === '' || value === '') continue
    result[key] = value
  }
  return result
}

// =============================================================================
// 10. CreateGroupRequest.items 投影
// =============================================================================

/** 由选中集合构建 CreateGroupRequest.items：resolverItem 恒为 item.id（v1.6 裁决——规格/
 *  变体选择不在本弹窗，留给插件默认档）。 */
export function buildManifestGroupItems(items: PreviewItemDto[], selectedItemIds: Set<string>): GroupItemRequest[] {
  const result: GroupItemRequest[] = []
  for (const item of items) {
    if (!selectedItemIds.has(item.id)) continue
    result.push({ resolverItem: item.id, fileName: item.name, relPath: item.path, size: item.size })
  }
  return result
}

// =============================================================================
// 11. 面包屑分段模型——深度的唯一去处
// =============================================================================
//
// 非搜索态由 cwd 路径段推导；>4 段折叠为 首段 / ⋯ / 末两段，中间段进 overflowSegments
// （点击 ⋯ 的隐藏层级菜单数据源）；搜索态整条替换为「搜索结果 · N 项」。

export type ManifestCrumbKind = 'home' | 'segment' | 'ellipsis'

export interface ManifestCrumbSegment {
  kind: ManifestCrumbKind
  label: string
  path: string
  isLast: boolean
}

export interface ManifestBreadcrumbModel {
  /** true = 搜索态：整条替换为「搜索结果 · N 项」，segments/overflowSegments 均为空。 */
  searching: boolean
  searchResultCount: number
  /** 是否显示「返回上级」按钮（非根且非搜索态）。 */
  showUp: boolean
  /** 展示用分段（含 home，超过 4 段时已折叠、含 ellipsis 标记）。 */
  segments: ManifestCrumbSegment[]
  /** 折叠时被 ⋯ 隐藏的中间段，未折叠时为空。 */
  overflowSegments: ManifestCrumbSegment[]
}

export interface ManifestBreadcrumbParams extends ManifestVisibilityFilter {
  items: PreviewItemDto[]
  cwd: string
}

/** 构建面包屑模型。items/extFilter/search 仅用于搜索态下的结果计数；非搜索态下面包屑纯由
 *  cwd 路径段推导。 */
export function buildManifestBreadcrumb({ items, cwd, extFilter, search }: ManifestBreadcrumbParams): ManifestBreadcrumbModel {
  if (manifestIsSearching(search)) {
    const count = items.filter((it) => manifestItemVisible(it, { extFilter, search })).length
    return { searching: true, searchResultCount: count, showUp: false, segments: [], overflowSegments: [] }
  }

  const segs = cwd === '' ? [] : cwd.split('/')
  const paths: string[] = []
  let acc = ''
  for (const s of segs) {
    acc = acc === '' ? s : `${acc}/${s}`
    paths.push(acc)
  }

  const segments: ManifestCrumbSegment[] = [{ kind: 'home', label: '', path: '', isLast: segs.length === 0 }]
  const overflow: ManifestCrumbSegment[] = []

  if (segs.length <= 4) {
    for (let i = 0; i < segs.length; i++) {
      segments.push({ kind: 'segment', label: segs[i], path: paths[i], isLast: i === segs.length - 1 })
    }
  } else {
    segments.push({ kind: 'segment', label: segs[0], path: paths[0], isLast: false })
    segments.push({ kind: 'ellipsis', label: '⋯', path: '', isLast: false })
    for (let i = segs.length - 2; i < segs.length; i++) {
      segments.push({ kind: 'segment', label: segs[i], path: paths[i], isLast: i === segs.length - 1 })
    }
    for (let i = 1; i < segs.length - 2; i++) {
      overflow.push({ kind: 'segment', label: segs[i], path: paths[i], isLast: false })
    }
  }

  return { searching: false, searchResultCount: 0, showUp: segs.length > 0, segments, overflowSegments: overflow }
}


