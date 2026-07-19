// #screen-main —— 三栏任务界面：侧边栏 + 中央任务列表 + 详情面板。
// 对齐 design/web/index.html #screen-main 结构。
// 左右两栏支持拖拽调宽（对齐桌面 home_page：侧边栏 180–320 / 详情 240–420，
// localStorage 持久化）：宽度经 CSS 变量挂在 section 上，拖拽把手直接写 DOM
// （不走 React state，拖动零重渲染），松手才落盘。

import { useEffect, useRef, type CSSProperties, type PointerEvent as ReactPointerEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { GlobalDialogs } from '../components/dialogs'
import { DetailPanel } from '../components/tasks/DetailPanel'
import { GroupDetailPanel } from '../components/tasks/GroupDetailPanel'
import { ManageBar } from '../components/tasks/ManageBar'
import { Sidebar } from '../components/tasks/Sidebar'
import { StatusBar } from '../components/tasks/StatusBar'
import { StatusTabs } from '../components/tasks/StatusTabs'
import { TaskList } from '../components/tasks/TaskList'
import { TasksUiProvider, useTasksUi } from '../components/tasks/context'
import { TopBar } from '../components/tasks/TopBar'
import { api } from '../lib/api'
import { connectWs } from '../lib/ws'

interface PanelWidthConf {
  key: string
  def: number
  min: number
  max: number
}

const SIDEBAR_W: PanelWidthConf = { key: 'fluxdown.sidebarWidth', def: 220, min: 180, max: 320 }
const DETAIL_W: PanelWidthConf = { key: 'fluxdown.detailWidth', def: 340, min: 240, max: 420 }

function loadWidth(c: PanelWidthConf): number {
  const v = Number(localStorage.getItem(c.key))
  return Number.isFinite(v) && v >= c.min && v <= c.max ? v : c.def
}

/** 竖向拖拽分隔条：pointer capture 拖动，实时写 section 的 CSS 变量，松手持久化。
 *  `invert`：右侧面板向左拖为加宽（delta 取反）。 */
function ColResizer({ cssVar, conf, invert, className }: { cssVar: string; conf: PanelWidthConf; invert?: boolean; className?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const drag = useRef<{ startX: number; startW: number } | null>(null)
  const screenOf = () => ref.current?.closest<HTMLElement>('.wscreen') ?? null

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    const sec = screenOf()
    if (!sec || !ref.current) return
    const cur = parseFloat(getComputedStyle(sec).getPropertyValue(cssVar))
    drag.current = { startX: e.clientX, startW: Number.isFinite(cur) ? cur : conf.def }
    ref.current.setPointerCapture(e.pointerId)
    ref.current.classList.add('active')
  }
  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const sec = screenOf()
    if (!drag.current || !sec) return
    const delta = e.clientX - drag.current.startX
    const w = Math.min(conf.max, Math.max(conf.min, drag.current.startW + (invert ? -delta : delta)))
    sec.style.setProperty(cssVar, `${w}px`)
  }
  const onPointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!drag.current || !ref.current) return
    drag.current = null
    ref.current.releasePointerCapture(e.pointerId)
    ref.current.classList.remove('active')
    const sec = screenOf()
    const w = sec ? parseFloat(getComputedStyle(sec).getPropertyValue(cssVar)) : NaN
    if (Number.isFinite(w)) localStorage.setItem(conf.key, String(Math.round(w)))
  }

  return (
    <div
      ref={ref}
      className={`col-resizer ${className ?? ''}`}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
    />
  )
}

export function TasksScreen() {
  const qc = useQueryClient()
  useEffect(() => {
    connectWs(qc)
  }, [qc])

  // 预取 + 与子组件共享同一份 Query 缓存（WS 消息直接 setQueryData 到这些 key）。
  useQuery({ queryKey: ['tasks'], queryFn: api.listTasks })
  useQuery({ queryKey: ['queues'], queryFn: api.listQueues })
  useQuery({ queryKey: ['groups'], queryFn: api.listGroups })
  useQuery({ queryKey: ['stats'], queryFn: api.stats, refetchInterval: 30_000 })

  // 初始宽度只读一次（拖拽期间由把手直接写 DOM，不回流 React state）。
  const initialWidths = useRef({ sidebar: loadWidth(SIDEBAR_W), detail: loadWidth(DETAIL_W) })

  return (
    <TasksUiProvider>
      <section
        className="wscreen active"
        id="screen-main"
        style={{ '--sidebar-w': `${initialWidths.current.sidebar}px`, '--detail-w': `${initialWidths.current.detail}px` } as CSSProperties}
      >
        <Sidebar />
        <ColResizer cssVar="--sidebar-w" conf={SIDEBAR_W} />
        <SideBackdrop />
        <div className="center">
          <TopBar />
          <ManageBar />
          <StatusTabs />
          <TaskList />
          <StatusBar />
        </div>
        <ColResizer cssVar="--detail-w" conf={DETAIL_W} invert className="dresize" />
        <DetailPanel />
        <GroupDetailPanel />
      </section>
      <GlobalDialogs />
    </TasksUiProvider>
  )
}

/** 移动端抽屉侧边栏的遮罩：仅在小屏且抽屉展开时可见（CSS 控制），点击收起。 */
function SideBackdrop() {
  const { sidebarOpen, setSidebarOpen } = useTasksUi()
  if (!sidebarOpen) return null
  return <div className="side-backdrop" onClick={() => setSidebarOpen(false)} />
}
