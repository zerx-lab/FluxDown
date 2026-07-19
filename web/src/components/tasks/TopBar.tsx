// 顶部工具栏：搜索、批量管理开关、全局暂停/恢复、限速快览、新建下载、设置入口。
// 对齐 design/web/index.html .topbar 结构；批量选择状态见 ManageBar。

import { useEffect, useRef, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { Gauge, ListChecks, Menu, Pause, Play, Plus, Search, Settings } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { openNewDownload } from '../../lib/dialogs'
import { fmtSpeed } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import { GROUP_BY_CYCLE, SORT_KEY_CYCLE, SORT_KEY_DEFAULT_DIR, updateViewPrefs } from '../../lib/view-prefs'
import { useConfigQuery } from '../settings/useConfig'
import { useTasksUi } from './context'
import { useViewTasks } from './useViewTasks'
import { ViewOptionsPanelButton } from './ViewOptionsPanel'

export function TopBar() {
  const { t } = useI18n()
  const navigate = useNavigate()
  const { search, setSearch, manageMode, setManageMode, setSidebarOpen, statusTab } = useTasksUi()
  const tasks = useViewTasks()
  const qc = useQueryClient()
  const inputRef = useRef<HTMLInputElement>(null)
  const [narrow, setNarrow] = useState(() => window.matchMedia('(max-width: 820px)').matches)

  useEffect(() => {
    const mq = window.matchMedia('(max-width: 820px)')
    const onChange = (e: MediaQueryListEvent) => setNarrow(e.matches)
    mq.addEventListener('change', onChange)
    return () => mq.removeEventListener('change', onChange)
  }, [])

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') {
        e.preventDefault()
        inputRef.current?.focus()
      }
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [])

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.ctrlKey || e.metaKey || e.altKey) return
      const active = document.activeElement
      if (active instanceof HTMLElement && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA' || active.tagName === 'SELECT' || active.isContentEditable)) return
      const key = e.key.toLowerCase()
      if (key === 'v' && !e.shiftKey) {
        e.preventDefault()
        updateViewPrefs(statusTab, (p) => ({ ...p, form: p.form === 'list' ? 'grid' : 'list' }))
      } else if (e.shiftKey && key === 'd') {
        e.preventDefault()
        updateViewPrefs(statusTab, (p) => (p.form === 'grid' ? p : { ...p, density: p.density === 'comfortable' ? 'compact' : 'comfortable' }))
      } else if (key === 'g' && !e.shiftKey) {
        e.preventDefault()
        updateViewPrefs(statusTab, (p) => {
          const i = GROUP_BY_CYCLE.indexOf(p.groupBy)
          return { ...p, groupBy: GROUP_BY_CYCLE[(i + 1) % GROUP_BY_CYCLE.length] }
        })
      } else if (key === 's' && !e.shiftKey) {
        e.preventDefault()
        updateViewPrefs(statusTab, (p) => {
          const i = SORT_KEY_CYCLE.indexOf(p.sortKey)
          const next = SORT_KEY_CYCLE[(i + 1) % SORT_KEY_CYCLE.length]
          return { ...p, sortKey: next, sortDir: SORT_KEY_DEFAULT_DIR[next] }
        })
      }
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [statusTab])

  const hasActive = tasks.some((t) => t.status === 0 || t.status === 1 || t.status === 5)
  const invalidate = () => qc.invalidateQueries({ queryKey: ['tasks'] })
  const pauseAll = useMutation({ mutationFn: api.pauseAll, onSuccess: invalidate })
  const continueAll = useMutation({ mutationFn: api.continueAll, onSuccess: invalidate })

  return (
    <header className="topbar">
      <button type="button" className="icon-btn menu-btn" title={t('common.menu')} onClick={() => setSidebarOpen(true)}>
        <Menu size={17} />
      </button>
      <div className="search">
        <Search size={14} />
        <input
          ref={inputRef}
          type="text"
          placeholder={t(narrow ? 'topbar.searchPlaceholderShort' : 'topbar.searchPlaceholder')}
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              setSearch('')
              e.currentTarget.blur()
            }
          }}
        />
      </div>
      <div className="topbar-actions">
        <button type="button" className={cn('icon-btn', manageMode && 'active')} title={t('topbar.manage')} onClick={() => setManageMode((v) => !v)}>
          <ListChecks size={17} />
        </button>
        <button
          type="button"
          className="icon-btn"
          title={t('topbar.pauseResumeAll')}
          onClick={() => (hasActive ? pauseAll.mutate() : continueAll.mutate())}
        >
          {hasActive ? <Pause size={17} /> : <Play size={17} />}
        </button>
        <LimitButton />
        <ViewOptionsPanelButton />
        <span className="vsep" />
        <button type="button" className="btn primary" onClick={openNewDownload}>
          <Plus size={15} />
          <span className="btn-label">{t('topbar.newDownload')}</span>
        </button>
        <button type="button" className="icon-btn" title={t('common.settings')} onClick={() => navigate({ to: '/settings' })}>
          <Settings size={17} />
        </button>
      </div>
    </header>
  )
}

/** 全局限速快览；点击跳转设置页调整（单一数据源，避免与设置页的编辑控件重复）。 */
function LimitButton() {
  const { t } = useI18n()
  const navigate = useNavigate()
  const { data: config } = useConfigQuery()
  const bytes = Number(config?.speed_limit_bytes ?? 0)
  const label = bytes > 0 ? t('topbar.speedLimitOn', { speed: fmtSpeed(bytes) }) : t('topbar.speedLimitOff')
  return (
    <button type="button" className="icon-btn" title={t('topbar.goSettings', { label })} onClick={() => navigate({ to: '/settings' })}>
      <Gauge size={17} />
    </button>
  )
}
