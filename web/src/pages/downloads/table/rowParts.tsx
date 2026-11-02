// 任务表行的小件组件：数值元信息单元格、树轨与行悬停操作浮层。

import type { ReactNode } from 'react'
import { cn } from '../../../lib/cn'
import { CELL_PADDING_X, TREE_CHEVRON, TREE_INDENT } from './rowMetrics'

export function Meta({
  children,
  numeric,
  tertiary,
  success,
}: {
  children: string
  numeric: boolean
  tertiary?: boolean
  success?: boolean
}) {
  return (
    <div
      className={cn(
        'min-w-0 truncate text-xs',
        numeric && 'tabular text-right',
        success ? 'text-success' : tertiary ? 'text-text-tertiary' : 'text-muted-foreground',
      )}
    >
      {children}
    </div>
  )
}

/** 树轨：每级缩进的 chevron 中线处一条细竖线（贯穿整行高，需放在 `relative` 且拉伸到行高的单元格内）。 */
export function TreeRails({ depth }: { depth: number }) {
  return Array.from({ length: depth }, (_, level) => (
    <span
      key={level}
      aria-hidden
      className="pointer-events-none absolute inset-y-0 w-px bg-hairline"
      style={{ left: CELL_PADDING_X + level * TREE_INDENT + TREE_CHEVRON / 2 }}
    />
  ))
}

/**
 * 行悬停操作浮层（放在行末）：零宽 sticky 锚点贴住表格滚动区右缘，横向滚动时按钮不随末列移出视口
 * （与 GPUI 挂在右侧留白列上的浮层一致）；未溢出时锚点就在行末，位置与行内右缘相同。
 *
 * 底色 = surface 上叠选中 / 悬停色（与行一致），左侧 16px 渐隐，避免半透明盖住「创建时间」等末列文字透出。
 */
export function RowActionsBar({ selected, children }: { selected: boolean; children: ReactNode }) {
  const fadeMask = {
    maskImage: 'linear-gradient(to right, transparent, #000 16px)',
    WebkitMaskImage: 'linear-gradient(to right, transparent, #000 16px)',
  } as const
  return (
    <div className="sticky right-1 w-0 shrink-0 self-stretch">
      <div className="absolute inset-y-0 right-0 hidden items-center gap-0.5 rounded-r-[var(--fx-components-task-row-radius)] pl-1 pr-1 group-hover/row:flex">
        <div
          aria-hidden
          className="pointer-events-none absolute inset-y-0 -left-4 right-0 bg-surface"
          style={fadeMask}
        />
        <div
          aria-hidden
          className={cn(
            'pointer-events-none absolute inset-y-0 -left-4 right-0',
            selected ? 'bg-accent' : 'bg-row-hover',
          )}
          style={fadeMask}
        />
        <div data-no-sweep className="relative flex items-center gap-0.5" onPointerDown={(event) => event.stopPropagation()}>
          {children}
        </div>
      </div>
    </div>
  )
}
