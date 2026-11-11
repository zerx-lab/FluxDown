// 任务详情「文件」页（BT）的纯逻辑：优先级档位映射、批量改优先级的差量、
// 「至少保留一个文件」守卫、多选（单击 / 切换 / 连选）与路径展示拆分。
// 镜像 crates/downloads/src/model/bt_files.rs（同名函数同语义）。

import { BT_FILE_PRIORITY_HIGH, BT_FILE_PRIORITY_MAXIMUM, BT_FILE_PRIORITY_NORMAL, BT_FILE_PRIORITY_SKIP } from '../../../lib/rpc'
import type { BtFilePriorityDto, BtTaskFileDto } from '../../../lib/rpc'

/** 轮询间隔（与 BT 实况一致）。 */
export const BT_FILES_POLL_MS = 2000

/** 界面上的四档优先级。 */
export type PriorityLevel = 'skip' | 'normal' | 'high' | 'maximum'

export const PRIORITY_LEVELS: readonly PriorityLevel[] = ['skip', 'normal', 'high', 'maximum']

const RAW: Record<PriorityLevel, number> = {
  skip: BT_FILE_PRIORITY_SKIP,
  normal: BT_FILE_PRIORITY_NORMAL,
  high: BT_FILE_PRIORITY_HIGH,
  maximum: BT_FILE_PRIORITY_MAXIMUM,
}

const LABEL_KEY: Record<PriorityLevel, string> = {
  skip: 'btFilePrioritySkip',
  normal: 'btFilePriorityNormal',
  high: 'btFilePriorityHigh',
  maximum: 'btFilePriorityMaximum',
}

/** 线上原始值 → 档位：四档以外的值（`2..=5` 及越界值）按距离最近的档位显示。 */
export function priorityLevel(raw: number): PriorityLevel {
  if (raw === BT_FILE_PRIORITY_SKIP) return 'skip'
  if (raw >= 1 && raw <= 3) return 'normal'
  if (raw >= 4 && raw <= 6) return 'high'
  // 负数不会出现在线上（u8），按最高档兜底与 Rust 一致。
  return 'maximum'
}

export const priorityRaw = (level: PriorityLevel): number => RAW[level]

export const priorityLabelKey = (level: PriorityLevel): string => LABEL_KEY[level]

/** 一组目标文件的优先级概况：`null` = 没有目标文件；`'mixed'` = 档位不一。 */
export type TargetLevel = PriorityLevel | 'mixed' | null

/** 目标文件的档位是否一致（按显示档位比较，`2` 与 `1` 同算「普通」）。 */
export function targetLevel(files: readonly BtTaskFileDto[], targets: ReadonlySet<number>): TargetLevel {
  let found: PriorityLevel | null = null
  for (const file of files) {
    if (!targets.has(file.index)) continue
    const level = priorityLevel(file.priority)
    if (found === null) found = level
    else if (found !== level) return 'mixed'
  }
  return found
}

/** 把目标文件设为 `level` 需要下发的条目：只含原始值确有变化的文件。 */
export function priorityChanges(files: readonly BtTaskFileDto[], targets: ReadonlySet<number>, level: PriorityLevel): BtFilePriorityDto[] {
  const priority = RAW[level]
  return files.filter((file) => targets.has(file.index) && file.priority !== priority).map((file) => ({ index: file.index, priority }))
}

/** 应用 `changes` 后是否所有文件都是「不下载」（种子至少要保留一个文件）。 */
export function leavesNothingToDownload(files: readonly BtTaskFileDto[], changes: readonly BtFilePriorityDto[]): boolean {
  if (files.length === 0) return false
  const next = new Map(changes.map((change) => [change.index, change.priority]))
  return files.every((file) => (next.get(file.index) ?? file.priority) === BT_FILE_PRIORITY_SKIP)
}

/** 乐观更新：返回写入 `changes` 后的新行（不改原数组）。 */
export function applyChanges(files: readonly BtTaskFileDto[], changes: readonly BtFilePriorityDto[]): BtTaskFileDto[] {
  const next = new Map(changes.map((change) => [change.index, change.priority]))
  return files.map((file) => {
    const priority = next.get(file.index)
    return priority === undefined ? file : { ...file, priority }
  })
}

/** 已下载比例 `0..=1`；大小为 0 的文件视为已完成。 */
export function progressFraction(downloaded: number, size: number): number {
  if (!(size > 0)) return 1
  return Math.min(Math.max(downloaded, 0), size) / size
}

/** 百分比文本，向下取整到 0.1%（未完成不会显示成 100.0%）。 */
export function progressPercentText(fraction: number): string {
  const tenths = Math.floor(Math.min(Math.max(fraction, 0), 1) * 1000)
  return `${(tenths / 10).toFixed(1)}%`
}

/** 相对路径拆成 `{ dir, name }`；线上路径以 `/` 分隔，反斜杠属于文件名本身。 */
export function splitFilePath(path: string): { dir: string; name: string } {
  const cut = path.lastIndexOf('/')
  return cut < 0 ? { dir: '', name: path } : { dir: path.slice(0, cut), name: path.slice(cut + 1) }
}

/** 中间省略时尾部固定保留的字符数（按字素簇，含扩展名）。 */
export const NAME_TAIL_CHARS = 16

const GRAPHEMES = new Intl.Segmenter(undefined, { granularity: 'grapheme' })

/** 强 RTL 文字（希伯来 / 阿拉伯 / 叙利亚 / 它拿 / 西非书面字母等 BMP 区段与 SMP 的 RTL 平面区段）。 */
const STRONG_RTL = /[\u0590-\u08FF\uFB1D-\uFDFF\uFE70-\uFEFC\u{10800}-\u{10FFF}\u{1E800}-\u{1EFFF}]/u

/**
 * 单行中间省略的拆分：`tail` 固定显示（文件名末尾 `NAME_TAIL_CHARS` 个字素簇，保住扩展名），
 * `head` 是其余部分（目录 + 文件名开头），宽度不够时由 CSS 截断。按字素簇切，不会切开 ZWJ emoji、
 * 国旗、肤色修饰或分解形式（NFD）的字符。
 * 含强 RTL 文字时不拆（`tail` 为空，整体由 CSS 尾部截断）：两段分属两个盒子，按从左到右排列会打乱双向文本。
 */
export function pathEllipsisParts(path: string): { head: string; tail: string } {
  if (STRONG_RTL.test(path)) return { head: path, tail: '' }
  const { name } = splitFilePath(path)
  const clusters = Array.from(GRAPHEMES.segment(name), (part) => part.segment)
  const keep = Math.min(clusters.length, NAME_TAIL_CHARS)
  const tail = clusters.slice(clusters.length - keep).join('')
  return { head: path.slice(0, path.length - tail.length), tail }
}

/** 文件页多选：按文件 `index` 记录，轮询刷新后保留，下标消失时剪除。不可变值，每次操作返回新对象。 */
export interface FileSelection {
  selected: ReadonlySet<number>
  /** 连选锚点（最近一次单击 / 切换的文件）。 */
  anchor: number | null
}

export const EMPTY_FILE_SELECTION: FileSelection = Object.freeze({ selected: new Set<number>(), anchor: null })

/**
 * 单击一行。`order` 为当前显示顺序的文件下标；`shift` 从锚点连选，`toggle`（Cmd / Ctrl）
 * 切换单行或把连选范围并入现有选区。
 */
export function clickSelection(current: FileSelection, order: readonly number[], index: number, shift: boolean, toggle: boolean): FileSelection {
  const anchorPos = current.anchor === null ? -1 : order.indexOf(current.anchor)
  const indexPos = order.indexOf(index)
  if (shift && anchorPos >= 0 && indexPos >= 0) {
    const [start, end] = anchorPos <= indexPos ? [anchorPos, indexPos] : [indexPos, anchorPos]
    const selected = new Set(toggle ? current.selected : [])
    for (const item of order.slice(start, end + 1)) selected.add(item)
    return { selected, anchor: current.anchor }
  }
  if (toggle) {
    const selected = new Set(current.selected)
    if (!selected.delete(index)) selected.add(index)
    return { selected, anchor: index }
  }
  return { selected: new Set([index]), anchor: index }
}

/** 剪掉已不存在的下标；没有变化时原样返回（避免无谓重渲染）。 */
export function pruneSelection(current: FileSelection, present: readonly number[]): FileSelection {
  const live = new Set(present)
  const stale = [...current.selected].some((index) => !live.has(index))
  const anchorGone = current.anchor !== null && !live.has(current.anchor)
  if (!stale && !anchorGone) return current
  return {
    selected: new Set([...current.selected].filter((index) => live.has(index))),
    anchor: anchorGone ? null : current.anchor,
  }
}

/** 右键菜单的作用对象：右键行在选区内 → 整个选区；否则只有这一行。 */
export function targetsFor(current: FileSelection, index: number): ReadonlySet<number> {
  return current.selected.has(index) ? current.selected : new Set([index])
}

/** 重命名提交：空白 → `invalid`（不提交）；与原路径相同 → `unchanged`（直接关闭）；否则下发原样输入。 */
export function renameTarget(input: string, current: string): { kind: 'invalid' } | { kind: 'unchanged' } | { kind: 'rename'; path: string } {
  if (input.trim() === '') return { kind: 'invalid' }
  if (input === current) return { kind: 'unchanged' }
  return { kind: 'rename', path: input }
}
