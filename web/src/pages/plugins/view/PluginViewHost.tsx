// 可移植插件视图宿主：打开 daemon 会话并把节点树渲染为 web/src/ui 组件。
// 挂载即 `view.open`，卸载即 `view.close`；会话结束（插件卸载 / 过期）显示「已结束」并可重新打开。

import { useEffect, useMemo, useRef, useState } from 'react'
import { useI18n } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { Button, Spinner } from '../../../ui'
import { ViewRenderContext } from './nodes/context'
import { ViewNodeView } from './nodes'
import { ViewSession, useViewSession, viewError } from './viewStore'

export interface PluginViewHostProps {
  identity: string
  view: string
  /** 任务详情 tab 传入的任务 id。 */
  taskId?: string
  className?: string
  /** 承载容器的关闭处理（弹窗等）；插件 `ctx.close()` 时调用。不传表示无可关闭容器。 */
  onClose?: () => void
}

function Notice({ text, action }: { text: string; action?: { label: string; run: () => void } }) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 px-6 py-10 text-center text-sm text-muted-foreground">
      <div>{text}</div>
      {action ? (
        <Button variant="outline" onClick={action.run}>
          {action.label}
        </Button>
      ) : null}
    </div>
  )
}

function SessionBody({ session }: { session: ViewSession }) {
  const { t } = useI18n()
  const state = useViewSession(session)
  const ctx = useMemo(() => ({ send: session.send.bind(session) }), [session])
  const reopen = { label: t('pluginViewReopen'), run: () => void session.open() }

  if (state.phase === 'failed') return <Notice text={t('pluginViewError', { message: state.error ?? '' })} action={reopen} />
  if (state.phase === 'closed') return <Notice text={t('pluginViewClosed')} action={reopen} />
  if (state.tree === null) {
    return (
      <div className="flex items-center justify-center gap-2 px-6 py-10 text-sm text-muted-foreground">
        <Spinner size="md" />
        {t('pluginViewLoading')}
      </div>
    )
  }
  const error = viewError(state.tree)
  return (
    <ViewRenderContext.Provider value={ctx}>
      {error ? (
        <div role="alert" className="mb-3 rounded-md bg-destructive/10 px-3 py-2 text-xs text-destructive">
          {t('pluginViewError', { message: error })}
        </div>
      ) : null}
      <ViewNodeView node={state.tree} />
    </ViewRenderContext.Provider>
  )
}

export function PluginViewHost({ identity, view, taskId, className, onClose }: PluginViewHostProps) {
  const { locale } = useI18n()
  const [session, setSession] = useState<ViewSession | null>(null)
  // 语言切换不重开会话（locale 只在打开时传给插件）。
  const localeRef = useRef(locale)
  localeRef.current = locale
  const onCloseRef = useRef(onClose)
  onCloseRef.current = onClose
  const closable = onClose !== undefined

  useEffect(() => {
    const next = new ViewSession({ identity, view, taskId, locale: localeRef.current })
    if (closable) next.onRequestClose = () => onCloseRef.current?.()
    setSession(next)
    void next.open()
    return () => next.dispose()
  }, [identity, view, taskId, closable])

  return <div className={cn('flex min-h-0 min-w-0 flex-col', className)}>{session ? <SessionBody session={session} /> : null}</div>
}
