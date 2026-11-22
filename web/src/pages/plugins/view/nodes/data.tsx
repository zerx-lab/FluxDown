// table / list：@tanstack/react-virtual 虚拟滚动（行高固定，上限 10000 行也只渲染可见行）。

import { useVirtualizer } from '@tanstack/react-virtual'
import { useRef } from 'react'
import type { CSSProperties } from 'react'
import type { JsonValue, ViewNode } from '../../../../lib/rpc'
import { Button, ProgressBar } from '../../../../ui'
import { useFileSizeUnits } from '../../../../lib/fileSizeUnits'
import { formatCell } from '../cells'
import { toneColor } from '../tones'
import { ToneBadge } from './content'
import { useViewRender } from './context'
import { num, optStr, record, records, str } from './props'

const ROW_HEIGHT = 36
const LIST_ROW_HEIGHT = 52
const MAX_VIEWPORT = 420

function viewportHeight(count: number, rowHeight: number): number {
  return Math.min(MAX_VIEWPORT, Math.max(1, count) * rowHeight)
}

function columnTemplate(columns: readonly { width?: number }[], extra: boolean): string {
  const parts = columns.map((column) => (column.width !== undefined && column.width > 0 ? `${column.width}px` : 'minmax(0,1fr)'))
  if (extra) parts.push('max-content')
  return parts.join(' ')
}

export function TableNode({ node }: { node: ViewNode }) {
  const units = useFileSizeUnits()
  const { send } = useViewRender()
  const scrollRef = useRef<HTMLDivElement>(null)
  const columns = records(node.p, 'columns').map((column) => ({
    key: str(column, 'key'),
    title: str(column, 'title'),
    kind: str(column, 'kind', 'text'),
    width: num(column, 'width'),
  }))
  const rows = Array.isArray(node.p.rows) ? node.p.rows.flatMap((row) => (record(row) ? [record(row) as Record<string, JsonValue>] : [])) : []
  const actions = records(node.p, 'rowActions')
  const template = columnTemplate(columns, actions.length > 0)
  const virtualizer = useVirtualizer({ count: rows.length, getScrollElement: () => scrollRef.current, estimateSize: () => ROW_HEIGHT, overscan: 8 })
  const grid: CSSProperties = { display: 'grid', gridTemplateColumns: template, alignItems: 'center', columnGap: 12 }

  return (
    <div className="flex min-w-0 flex-col rounded-md border border-hairline">
      <div className="px-3 py-1.5 text-xs font-medium text-muted-foreground" style={grid}>
        {columns.map((column) => (
          <div key={column.key} className="truncate">
            {column.title}
          </div>
        ))}
        {actions.length > 0 ? <div /> : null}
      </div>
      {rows.length === 0 ? (
        <div className="border-t border-hairline px-3 py-4 text-center text-xs text-muted-foreground">
          {optStr(node.p, 'emptyText') ?? ''}
        </div>
      ) : (
        <div ref={scrollRef} className="overflow-auto border-t border-hairline" style={{ height: viewportHeight(rows.length, ROW_HEIGHT) }}>
          <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
            {virtualizer.getVirtualItems().map((item) => {
              const row = rows[item.index]
              if (!row) return null
              const key = str(row, 'key')
              return (
                <div
                  key={key}
                  className="absolute inset-x-0 cursor-default px-3 text-sm hover:bg-row-hover"
                  style={{ ...grid, height: ROW_HEIGHT, transform: `translateY(${item.start}px)` }}
                  onClick={() => send(node.id, { type: 'select', key })}
                >
                  {columns.map((column) => {
                    const cell = formatCell(column.kind, row[column.key], units)
                    return (
                      <div key={column.key} className="min-w-0 truncate tabular">
                        {column.kind === 'progress' && cell.fraction !== undefined ? (
                          <div className="flex items-center gap-2">
                            <ProgressBar value={cell.fraction} className="flex-1" />
                            <span className="text-xs text-muted-foreground">{cell.text}</span>
                          </div>
                        ) : column.kind === 'badge' ? (
                          cell.text ? <ToneBadge text={cell.text} tone={cell.tone} /> : null
                        ) : (
                          cell.text
                        )}
                      </div>
                    )
                  })}
                  {actions.length > 0 ? (
                    <div className="flex items-center gap-1" onClick={(event) => event.stopPropagation()}>
                      {actions.map((action) => (
                        <Button
                          key={str(action, 'id')}
                          variant="ghost"
                          className="h-7 px-2 text-xs"
                          style={action.tone ? { color: toneColor(action.tone) } : undefined}
                          onClick={() => send(node.id, { type: 'rowAction', key, action: str(action, 'id') })}
                        >
                          {str(action, 'label')}
                        </Button>
                      ))}
                    </div>
                  ) : null}
                </div>
              )
            })}
          </div>
        </div>
      )}
    </div>
  )
}

export function ListNode({ node }: { node: ViewNode }) {
  const { send } = useViewRender()
  const scrollRef = useRef<HTMLDivElement>(null)
  const items = records(node.p, 'items')
  const virtualizer = useVirtualizer({ count: items.length, getScrollElement: () => scrollRef.current, estimateSize: () => LIST_ROW_HEIGHT, overscan: 8 })
  if (items.length === 0) return null
  return (
    <div ref={scrollRef} className="overflow-auto rounded-md border border-hairline" style={{ height: viewportHeight(items.length, LIST_ROW_HEIGHT) }}>
      <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((entry) => {
          const item = items[entry.index]
          if (!item) return null
          const key = str(item, 'key')
          const badge = optStr(item, 'badge')
          const description = optStr(item, 'description')
          return (
            <div
              key={key}
              className="absolute inset-x-0 flex cursor-default items-center gap-3 px-3 hover:bg-row-hover"
              style={{ height: LIST_ROW_HEIGHT, transform: `translateY(${entry.start}px)` }}
              onClick={() => send(node.id, { type: 'select', key })}
            >
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm text-foreground">{str(item, 'title')}</div>
                {description ? <div className="truncate text-xs text-muted-foreground">{description}</div> : null}
              </div>
              {badge ? <ToneBadge text={badge} tone={str(item, 'tone', 'neutral')} /> : null}
            </div>
          )
        })}
      </div>
    </div>
  )
}
