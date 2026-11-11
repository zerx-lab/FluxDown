import { describe, expect, test } from 'bun:test'
import type { BtTaskFileDto } from '../../../lib/rpc'
import {
  applyChanges,
  clickSelection,
  EMPTY_FILE_SELECTION,
  leavesNothingToDownload,
  priorityChanges,
  priorityLabelKey,
  priorityLevel,
  priorityRaw,
  progressFraction,
  progressPercentText,
  pathEllipsisParts,
  pruneSelection,
  renameTarget,
  splitFilePath,
  targetLevel,
  targetsFor,
} from './btFiles'

// 镜像 crates/downloads/src/model/bt_files.rs 的测试。
const file = (index: number, priority: number, patch: Partial<BtTaskFileDto> = {}): BtTaskFileDto => ({
  index,
  path: `f${index}`,
  size: 10,
  downloaded: 0,
  priority,
  ...patch,
})

describe('优先级档位', () => {
  test('四档往返与就近映射', () => {
    expect(['skip', 'normal', 'high', 'maximum'].map((level) => priorityRaw(level as 'skip'))).toEqual([0, 1, 6, 7])
    expect([0, 1, 2, 3, 4, 5, 6, 7, 200].map(priorityLevel)).toEqual(['skip', 'normal', 'normal', 'normal', 'high', 'high', 'high', 'maximum', 'maximum'])
    expect(priorityLabelKey('high')).toBe('btFilePriorityHigh')
  })

  test('目标档位：空 / 一致 / 不一（2 与 1 同算普通）', () => {
    const files = [file(0, 1), file(1, 2), file(2, 7)]
    expect(targetLevel(files, new Set())).toBeNull()
    expect(targetLevel(files, new Set([9]))).toBeNull()
    expect(targetLevel(files, new Set([0, 1]))).toBe('normal')
    expect(targetLevel(files, new Set([0, 2]))).toBe('mixed')
  })
})

describe('批量改优先级', () => {
  const files = [file(0, 1), file(1, 6), file(2, 2), file(3, 0)]

  test('只下发原始值确有变化的文件', () => {
    expect(priorityChanges(files, new Set([0, 1, 2]), 'normal')).toEqual([
      { index: 1, priority: 1 },
      { index: 2, priority: 1 },
    ])
    expect(priorityChanges(files, new Set([3]), 'skip')).toEqual([])
    expect(priorityChanges(files, new Set([99]), 'high')).toEqual([])
  })

  test('至少保留一个文件', () => {
    const all = priorityChanges(files, new Set([0, 1, 2]), 'skip')
    expect(leavesNothingToDownload(files, all)).toBe(true)
    expect(leavesNothingToDownload(files, all.slice(1))).toBe(false)
    expect(leavesNothingToDownload([], [])).toBe(false)
  })

  test('乐观更新不改原数组', () => {
    const next = applyChanges(files, [{ index: 1, priority: 0 }])
    expect(next.map((f) => f.priority)).toEqual([1, 0, 2, 0])
    expect(files[1]?.priority).toBe(6)
    expect(next[0]).toBe(files[0])
  })
})

describe('进度', () => {
  test('大小为 0 视为完成；超大值', () => {
    expect(progressFraction(0, 0)).toBe(1)
    expect(progressFraction(20, 10)).toBe(1)
    expect(progressFraction(4_294_967_296, 8_589_934_592)).toBe(0.5)
  })

  test('向下取整到 0.1%，未完成不显示 100.0%', () => {
    expect(progressPercentText(progressFraction(9_999, 10_000))).toBe('99.9%')
    expect(progressPercentText(progressFraction(5_368_709_119, 5_368_709_120))).toBe('99.9%')
    expect(progressPercentText(1)).toBe('100.0%')
    expect(progressPercentText(0)).toBe('0.0%')
    expect(progressPercentText(-1)).toBe('0.0%')
  })
})

describe('路径拆分', () => {
  test('按 / 拆；反斜杠属于文件名', () => {
    expect(splitFilePath('Season 1/Sub/E01.mkv')).toEqual({ dir: 'Season 1/Sub', name: 'E01.mkv' })
    expect(splitFilePath('a\\b.txt')).toEqual({ dir: '', name: 'a\\b.txt' })
    expect(splitFilePath('README')).toEqual({ dir: '', name: 'README' })
  })

  test('中间省略：尾部按字素簇保留文件名末尾（含扩展名）', () => {
    expect(pathEllipsisParts('Season 1/E01.mkv')).toEqual({ head: 'Season 1/', tail: 'E01.mkv' })
    const long = `深/${'很长的名字'.repeat(6)}😀🎬.tar.gz`
    const parts = pathEllipsisParts(long)
    expect(parts.head + parts.tail).toBe(long)
    expect(Array.from(parts.tail)).toHaveLength(16)
    expect(parts.tail.endsWith('😀🎬.tar.gz')).toBe(true)
    expect(pathEllipsisParts('README')).toEqual({ head: '', tail: 'README' })
  })

  test('不切开 ZWJ emoji / 国旗 / 肤色修饰 / NFD 字符', () => {
    const family = '👨‍👩‍👧‍👦'
    for (const cluster of [family, '🇯🇵', '👍🏽', 'e\u0301', '\u1112\u1161\u11AB']) {
      // 簇正好跨在第 16 个字素的边界上：尾部要么完整包含它，要么完全不含。
      const name = `${cluster}${'x'.repeat(15)}.mkv`
      const parts = pathEllipsisParts(`dir/${'a'.repeat(30)}${name}`)
      expect(parts.head + parts.tail).toBe(`dir/${'a'.repeat(30)}${name}`)
      expect(parts.tail).toBe(`${'x'.repeat(12)}.mkv`)
      const edge = pathEllipsisParts(`dir/${cluster}${'y'.repeat(11)}.mkv`)
      expect(edge.tail).toBe(`${cluster}${'y'.repeat(11)}.mkv`)
      expect(edge.head).toBe('dir/')
    }
  })

  test('含 RTL 文字时不拆，整体交给 CSS 截断', () => {
    for (const path of ['فيلم/الحلقة الأولى من المسلسل الطويل.mkv', 'סרטים/פרק ראשון של הסדרה הארוכה.mkv']) {
      expect(pathEllipsisParts(path)).toEqual({ head: path, tail: '' })
    }
  })
})

describe('多选', () => {
  const order = [5, 3, 8, 1]

  test('单击 / 切换 / 连选', () => {
    let sel = clickSelection(EMPTY_FILE_SELECTION, order, 3, false, false)
    expect([...sel.selected]).toEqual([3])
    sel = clickSelection(sel, order, 1, true, false)
    expect([...sel.selected].sort((a, b) => a - b)).toEqual([1, 3, 8])
    expect(sel.anchor).toBe(3)
    sel = clickSelection(sel, order, 5, false, true)
    expect([...sel.selected].sort((a, b) => a - b)).toEqual([1, 3, 5, 8])
    sel = clickSelection(sel, order, 5, false, true)
    expect(sel.selected.has(5)).toBe(false)
    expect(sel.anchor).toBe(5)
  })

  test('无锚点时 shift 退化为单击', () => {
    const sel = clickSelection(EMPTY_FILE_SELECTION, order, 8, true, false)
    expect([...sel.selected]).toEqual([8])
  })

  test('剪除消失的下标；无变化原样返回', () => {
    const sel = clickSelection(clickSelection(EMPTY_FILE_SELECTION, order, 3, false, false), order, 8, false, true)
    expect(pruneSelection(sel, order)).toBe(sel)
    const pruned = pruneSelection(sel, [5, 3, 1])
    expect([...pruned.selected]).toEqual([3])
    expect(pruned.anchor).toBeNull()
  })

  test('右键目标：在选区内取整个选区，否则只有这一行', () => {
    const sel = clickSelection(EMPTY_FILE_SELECTION, order, 3, false, false)
    expect(targetsFor(sel, 3)).toBe(sel.selected)
    expect([...targetsFor(sel, 8)]).toEqual([8])
  })
})

describe('重命名', () => {
  test('空白不提交、相同直接关闭、其余原样下发', () => {
    expect(renameTarget('  ', 'a.mkv')).toEqual({ kind: 'invalid' })
    expect(renameTarget('a.mkv', 'a.mkv')).toEqual({ kind: 'unchanged' })
    expect(renameTarget('Subs/Episode 1.srt', 'a.srt')).toEqual({ kind: 'rename', path: 'Subs/Episode 1.srt' })
  })
})
