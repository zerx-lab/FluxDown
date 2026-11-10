// 标题栏：`density.titleBar`（40）高、chrome 底、hairline 下边；依次为 Logo、页面主操作插槽、
// 可自定义的前置区域（titleLeading）、页面内容插槽、后置区域（titleTrailing）与连接状态点。
// 页面用 `<TitleBarSlot>` 传送内容，`position="beforeLeading"` 可把主操作放在前置区域之前。
// 移动端顶部避开安全区；无插槽的路由显示默认页名；自定义区域只在桌面渲染。

import { createContext, useContext, useEffect, useState } from 'react'
import type { ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { cn } from '../lib/cn'

interface SlotContextValue {
  element: HTMLElement | null
  setElement: (element: HTMLElement | null) => void
  beforeLeadingElement: HTMLElement | null
  setBeforeLeadingElement: (element: HTMLElement | null) => void
  /** 当前挂载了内容的 TitleBarSlot 数量。 */
  used: number
  register: () => () => void
}

const SlotContext = createContext<SlotContextValue | null>(null)

export function TitleBarSlotProvider({ children }: { children: ReactNode }) {
  const [element, setElement] = useState<HTMLElement | null>(null)
  const [beforeLeadingElement, setBeforeLeadingElement] = useState<HTMLElement | null>(null)
  const [used, setUsed] = useState(0)
  const [value] = useState<Pick<SlotContextValue, 'register'>>(() => ({
    register: () => {
      setUsed((n) => n + 1)
      return () => setUsed((n) => n - 1)
    },
  }))
  return (
    <SlotContext.Provider value={{ element, setElement, beforeLeadingElement, setBeforeLeadingElement, used, register: value.register }}>
      {children}
    </SlotContext.Provider>
  )
}

/** 传送内容到标题栏。未处于 AppShell 内时渲染为空。 */
export function TitleBarSlot({ children, position = 'content' }: { children: ReactNode; position?: 'beforeLeading' | 'content' }) {
  const context = useContext(SlotContext)
  const register = context?.register
  useEffect(() => register?.(), [register])
  const element = position === 'beforeLeading' ? context?.beforeLeadingElement : context?.element
  if (!element) return null
  return createPortal(children, element)
}

/** AppShell 内部使用：`leading` 在主操作与页面内容插槽之间；`trailing` 在页面内容插槽之后。 */
export function TitleBar({ defaultTitle, leading, trailing }: { defaultTitle: string; leading?: ReactNode; trailing?: ReactNode }) {
  const context = useContext(SlotContext)
  return (
    <header className="flex h-title-bar shrink-0 items-center gap-2 border-b border-hairline bg-chrome box-content pt-safe pl-[max(0.75rem,env(safe-area-inset-left))] pr-[max(0.5rem,env(safe-area-inset-right))]">
      <img src="/favicon.svg" alt="" className="size-4 shrink-0 mobile:hidden" draggable={false} />
      <div ref={context?.setBeforeLeadingElement} className="flex shrink-0 items-center gap-2 empty:hidden" />
      {leading}
      {context && context.used === 0 ? (
        <div className={cn('min-w-0 truncate text-sm font-medium text-foreground desktop:sr-only')}>{defaultTitle}</div>
      ) : null}
      <div ref={context?.setElement} className="flex min-w-0 flex-1 items-center gap-2" />
      {trailing}
    </header>
  )
}
