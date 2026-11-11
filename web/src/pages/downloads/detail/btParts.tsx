import { Copy } from 'lucide-react'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import type { RefObject } from 'react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { copyText } from '../../../lib/copy'
import { Icon, Tooltip, toast } from '../../../ui'
import type { MenuEntry } from '../../../ui'
import { Note } from './DetailRow'

/** 监听元素内容宽度（px）；未测量前为 0。 */
function useElementWidth(ref: RefObject<HTMLElement | null>): number {
  const [width, setWidth] = useState(0)
  useEffect(() => {
    const element = ref.current
    if (!element) return
    const measure = () => setWidth(Math.floor(element.clientWidth))
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(element)
    return () => observer.disconnect()
  }, [ref])
  return width
}

/** 以容器测宽驱动响应式版式的根节点 ref + 宽度。 */
export function useMeasuredRoot(): { ref: RefObject<HTMLDivElement | null>; width: number } {
  const ref = useRef<HTMLDivElement | null>(null)
  const width = useElementWidth(ref)
  return { ref, width }
}

/** 最近的可滚动祖先（overflow-y 为 auto / scroll）。 */
function findScrollParent(el: HTMLElement): HTMLElement | null {
  for (let node = el.parentElement; node; node = node.parentElement) {
    const overflowY = getComputedStyle(node).overflowY
    if (overflowY === 'auto' || overflowY === 'scroll') return node
  }
  return null
}

/**
 * 虚拟化列表的滚动容器是祖先面板：返回该祖先与 `scrollMargin`（列表顶部相对滚动内容顶部的偏移，
 * 根节点 `rootRef` 尺寸变化时重算）。
 */
export function useAncestorScroller(
  listRef: RefObject<HTMLElement | null>,
  rootRef: RefObject<HTMLElement | null>,
): { scrollEl: HTMLElement | null; scrollMargin: number } {
  const [scrollEl, setScrollEl] = useState<HTMLElement | null>(null)
  const [scrollMargin, setScrollMargin] = useState(0)
  useLayoutEffect(() => {
    const list = listRef.current
    if (list === null) return
    const scroller = findScrollParent(list)
    setScrollEl(scroller)
    if (scroller === null) return
    const update = () => {
      const offset = list.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop
      setScrollMargin(Math.round(offset))
    }
    update()
    const observer = new ResizeObserver(update)
    if (rootRef.current) observer.observe(rootRef.current)
    return () => observer.disconnect()
  }, [listRef, rootRef])
  return { scrollEl, scrollMargin }
}

/** 每秒更新的当前 unix 秒，让相对时间在两次轮询之间自行走字。 */
export function useNowSecs(): number {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000))
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000)
    return () => clearInterval(timer)
  }, [])
  return now
}

/** 复制菜单项（右键 / 长按菜单）；`doneKey` 非空时复制后弹 toast。 */
export function copyEntry(label: string, value: string, doneKey: string | null, t: (key: string) => string): MenuEntry {
  return {
    type: 'item',
    key: 'copy',
    label,
    icon: Copy,
    onSelect: () => {
      copyText(value)
      if (doneKey !== null) toast.success(t(doneKey))
    },
  }
}

/** 行内悬停复制按钮（触屏常显，键盘聚焦可达）。 */
export function RowCopyButton({ label, value, doneKey }: { label: string; value: string; doneKey: string | null }) {
  const t = useT()
  return (
    <Tooltip content={label}>
      <button
        type="button"
        aria-label={label}
        onClick={() => {
          copyText(value)
          if (doneKey !== null) toast.success(t(doneKey))
        }}
        className={cn(
          'flex size-6 shrink-0 items-center justify-center rounded-sm text-text-tertiary outline-none',
          'opacity-0 hover:bg-nav-hover hover:text-foreground focus-visible:opacity-100 group-hover/row:opacity-100',
          'pointer-coarse:opacity-100',
        )}
      >
        <Icon icon={Copy} size="sm" />
      </button>
    </Tooltip>
  )
}

/** 刷新失败的低调内联提示（数据仍显示上次成功值）。 */
export function PollError({ message }: { message: string | null }) {
  if (message === null) return null
  return (
    <Note tone="destructive" className="truncate">
      <span title={message}>{message}</span>
    </Note>
  )
}

/** 首次响应前的骨架行。 */
export function SkeletonRows({ rows = 4 }: { rows?: number }) {
  return (
    <div aria-hidden className="flex flex-col gap-2 pt-1">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="flex animate-pulse items-center gap-3">
          <div className="h-3 w-20 rounded-sm bg-muted" />
          <div className="h-3 min-w-0 flex-1 rounded-sm bg-muted" />
          <div className="h-3 w-12 rounded-sm bg-muted" />
        </div>
      ))}
    </div>
  )
}
