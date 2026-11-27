// 列与列之间的拖拽分隔条（配合 `useColumnWidth`）：指针拖拽 / 方向键调宽。

import { COLUMN_MAX, COLUMN_MIN } from '../lib/useColumnWidth'
import type { ColumnWidthHandleProps } from '../lib/useColumnWidth'

export function ColumnResizer({ width, handleProps }: { width: number; handleProps: ColumnWidthHandleProps }) {
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-valuemin={COLUMN_MIN}
      aria-valuemax={COLUMN_MAX}
      aria-valuenow={width}
      tabIndex={0}
      className="group relative z-10 -mx-0.5 w-1 shrink-0 cursor-col-resize touch-none outline-none"
      {...handleProps}
    >
      <div className="mx-auto h-full w-px bg-hairline transition-colors group-hover:bg-primary/60 group-focus-visible:bg-primary group-active:bg-primary" />
    </div>
  )
}
