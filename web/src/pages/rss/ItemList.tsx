// 虚拟化条目列表：固定行高（桌面 64 / 移动 76），只渲染可视窗口 + overscan。

import { useVirtualizer } from '@tanstack/react-virtual'
import { useRef } from 'react'
import type { RssItemDto } from '../../lib/rpc'
import { ItemRow, ROW_HEIGHT_DESKTOP, ROW_HEIGHT_MOBILE } from './ItemRow'
import { itemStatusKey } from './format'
import type { LinkedTaskStates } from './format'
import type { ItemAction } from './useRssItems'

export function ItemList({
  rows,
  selected,
  busy,
  taskStates,
  stale,
  mobile,
  onToggle,
  onAct,
  onViewImages,
}: {
  rows: readonly RssItemDto[]
  selected: ReadonlySet<string>
  busy: ReadonlySet<string>
  taskStates: LinkedTaskStates
  stale: boolean
  mobile: boolean
  onToggle: (guid: string) => void
  onAct: (guids: readonly string[], action: ItemAction) => void
  onViewImages: (item: RssItemDto) => void
}) {
  const parentRef = useRef<HTMLDivElement>(null)
  const rowHeight = mobile ? ROW_HEIGHT_MOBILE : ROW_HEIGHT_DESKTOP
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => rowHeight,
    overscan: 8,
    getItemKey: (index) => rows[index]?.guid ?? index,
  })
  return (
    <div ref={parentRef} className="min-h-0 w-full flex-1 overflow-y-auto overscroll-contain">
      <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((virtual) => {
          const item = rows[virtual.index]
          if (!item) return null
          const isBusy = busy.has(item.guid)
          return (
            <div
              key={virtual.key}
              className="absolute top-0 left-0 w-full"
              style={{ height: rowHeight, transform: `translateY(${virtual.start}px)` }}
            >
              <ItemRow
                item={item}
                statusKey={itemStatusKey(item, taskStates)}
                selected={selected.has(item.guid)}
                busy={isBusy}
                canAct={!stale && !isBusy}
                stale={stale}
                mobile={mobile}
                onToggle={onToggle}
                onAct={onAct}
                onViewImages={onViewImages}
              />
            </div>
          )
        })}
      </div>
    </div>
  )
}
