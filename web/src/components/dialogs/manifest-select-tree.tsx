// manifest-select.tsx 的浏览列表部分（v1.6 下钻导航范式移植）：虚拟化行 + 单行渲染，
// 零缩进——深度已转化为面包屑（manifest-select.tsx 的工具栏下方），本组件恒渲染当前层
// （或搜索态的全局扁平结果）。1000+ 项下 DOM 仅渲染可视窗口内的若干行（禁递归组件）。

import { useEffect, useRef } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { Archive, ChevronRight, File, FileText, Film, Folder, Image as ImageIcon, Music, Package2 } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { cn } from '../../lib/cn'
import { fmtBytes } from '../../lib/format'
import type { FileType } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import {
  manifestDirRowCheckState,
  manifestExtensionLabel,
  manifestItemCategory,
  type ManifestCheckState,
  type ManifestDirRow,
  type ManifestFileRow,
  type ManifestRow,
} from '../../lib/manifest-selection'

/** 行高恒 32px（1000+ 项虚拟化后 DOM 仅渲染可视窗口）。 */
const ROW_HEIGHT = 32

const FILE_ICONS: Record<FileType, LucideIcon> = {
  video: Film,
  audio: Music,
  document: FileText,
  image: ImageIcon,
  program: Package2,
  archive: Archive,
  other: File,
}

/** 文件类型 → 色块 tile 配色。program/other 没有专属色板，回退中性色。 */
const TILE_COLORS: Record<FileType, { bg: string; fg: string }> = {
  video: { bg: 'rgba(168,85,247,.14)', fg: '#a855f7' },
  audio: { bg: 'rgba(6,182,212,.14)', fg: '#06b6d4' },
  document: { bg: 'var(--accent-weak)', fg: 'var(--accent)' },
  image: { bg: 'rgba(34,197,94,.14)', fg: '#22c55e' },
  archive: { bg: 'rgba(245,158,11,.14)', fg: '#f59e0b' },
  program: { bg: 'var(--surface2)', fg: 'var(--text2)' },
  other: { bg: 'var(--surface2)', fg: 'var(--text2)' },
}

/** 三态勾选框：`.mcheck input` 恒 `display:none`（见 design.css），可见的只有装饰用 `<i>`——
 *  必须是真正的 `<label>` 才能借浏览器原生"点击 label 转发到关联 input"语义让点击落到隐藏
 *  input 上；换成 `<span>` 会导致点击装饰框毫无反应。indeterminate 是 DOM property 而非 JSX
 *  属性，需 ref 手动同步（对齐 bt-files.tsx）。onClick 只 stopPropagation，不重复调用
 *  onToggle——转发到 input 的合成点击会触发 onChange，若 label 自身也调用 onToggle 会双重
 *  触发导致视觉上"点了没反应"（两次 toggle 抵消）。 */
function TriCheckbox({ state, onToggle }: { state: ManifestCheckState; onToggle: () => void }) {
  const ref = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = state === 'indeterminate'
  }, [state])
  return (
    <label className="mcheck mf-check" onClick={(e) => e.stopPropagation()}>
      <input type="checkbox" ref={ref} checked={state === 'checked'} onChange={onToggle} />
      <i />
    </label>
  )
}

function DirRowView({ row, onToggle, onEnter }: { row: ManifestDirRow; onToggle: () => void; onEnter: () => void }) {
  const { t } = useI18n()
  const state = manifestDirRowCheckState(row)
  return (
    <div className="mf-row" onClick={onEnter}>
      <TriCheckbox state={state} onToggle={onToggle} />
      <Folder size={14} className="mf-dir-icon" />
      <span className="mf-dir-chain">
        {row.labels.map((label, i) => (
          <span key={i} className={cn(i === row.labels.length - 1 && 'mf-dir-chain-last')}>
            {i > 0 && <span className="mf-dir-chain-sep"> / </span>}
            {label}
          </span>
        ))}
      </span>
      <span className="mf-count">
        {row.selCnt > 0 ? (
          <>
            <b>{row.selCnt}/</b>
            {t('manifest.itemsCount', { n: row.count })}
          </>
        ) : (
          t('manifest.itemsCount', { n: row.count })
        )}
      </span>
      <span className="mf-size">
        {row.size > 0 ? `${fmtBytes(row.size)}${row.unknown ? '+' : ''}` : row.unknown ? t('manifest.dirSizeUnknown') : ''}
      </span>
      <ChevronRight size={13} className="mf-enter" />
    </div>
  )
}

/** 文件行：整行即勾选（无"进入"语义），根元素本身就是 `<label>`——点击 tile/文件名/大小
 *  任意区域都天然落到内部 input 上触发一次 onChange，无需（也不应该）额外挂 onClick，否则
 *  会与点击装饰框触发的 onChange 重复计数、双重 toggle 抵消（同 TriCheckbox 头注）。 */
function FileRowView({ row, selected, onToggle }: { row: ManifestFileRow; selected: boolean; onToggle: () => void }) {
  const { t } = useI18n()
  const item = row.item
  const category = manifestItemCategory(item)
  const Icon = FILE_ICONS[category]
  const tile = TILE_COLORS[category]
  const ext = manifestExtensionLabel(item.name)
  const pathPrefix = row.showPath && item.path !== '' ? `${item.path.split('/').at(-1)}/` : ''
  return (
    <label className={cn('mf-row', selected && 'mf-row-selected')}>
      <span className="mcheck mf-check">
        <input type="checkbox" checked={selected} onChange={onToggle} />
        <i />
      </span>
      <span className="mf-file-tile" style={{ background: tile.bg, color: tile.fg }} title={ext}>
        <Icon size={11} />
      </span>
      <span className="mf-file-name" title={row.showPath ? item.path : undefined}>
        {pathPrefix && <span className="mf-file-path-prefix">{pathPrefix}</span>}
        {item.name}
      </span>
      <span className="mf-count" aria-hidden />
      <span className="mf-size mf-size-file">{item.size === 0 ? t('manifest.fileSizeUnknown') : fmtBytes(item.size)}</span>
      <span className="mf-enter" aria-hidden />
    </label>
  )
}

interface ManifestSelectTreeProps {
  rows: ManifestRow[]
  selectedItemIds: Set<string>
  onToggleDirSubtree: (dirPath: string) => void
  onEnterDir: (dirPath: string) => void
  onToggleFile: (itemId: string) => void
  height?: number
}

/** 虚拟化文件列表：外部传入当前层（或搜索态）已算好的行流（manifestRowsAt 输出），本组件
 *  只负责渲染 + 交互回调，不持有导航/选择/筛选状态。 */
export function ManifestSelectTree({ rows, selectedItemIds, onToggleDirSubtree, onEnterDir, onToggleFile, height = 300 }: ManifestSelectTreeProps) {
  const { t } = useI18n()
  const parentRef = useRef<HTMLDivElement>(null)
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  })

  if (rows.length === 0) {
    return (
      <div className="mf-tree-empty" style={{ height }}>
        {t('manifest.treeEmpty')}
      </div>
    )
  }

  return (
    <div className="mf-tree" ref={parentRef} style={{ height }}>
      <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
        {virtualizer.getVirtualItems().map((vi) => {
          const row = rows[vi.index]
          return (
            <div
              key={vi.key}
              style={{ position: 'absolute', top: 0, left: 0, right: 0, height: ROW_HEIGHT, transform: `translateY(${vi.start}px)` }}
            >
              {row.kind === 'dir' ? (
                <DirRowView row={row.row} onToggle={() => onToggleDirSubtree(row.row.path)} onEnter={() => onEnterDir(row.row.path)} />
              ) : (
                <FileRowView row={row.row} selected={selectedItemIds.has(row.row.item.id)} onToggle={() => onToggleFile(row.row.item.id)} />
              )}
            </div>
          )
        })}
      </div>
    </div>
  )
}
