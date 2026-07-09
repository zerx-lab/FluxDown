// 状态 Tab：全部 / 下载中 / 已完成 / 已暂停 / 错误 + 计数。
// 对齐 design/web/index.html #statusTabs；计数基于全量任务（不叠加类型/队列/搜索筛选）。

import { cn } from '../../lib/cn'
import type { I18nKey } from '../../lib/i18n'
import { useI18n } from '../../lib/i18n'
import { countByStatusTab, type StatusTab } from './filters'
import { useTasksUi } from './context'
import { useViewTasks } from './useViewTasks'

const TABS: { id: StatusTab; labelKey: I18nKey }[] = [
  { id: 'all', labelKey: 'tabs.all' },
  { id: 'downloading', labelKey: 'tabs.downloading' },
  { id: 'completed', labelKey: 'tabs.completed' },
  { id: 'paused', labelKey: 'tabs.paused' },
  { id: 'error', labelKey: 'tabs.error' },
]

export function StatusTabs() {
  const { t } = useI18n()
  const { statusTab, setStatusTab } = useTasksUi()
  const tasks = useViewTasks()
  return (
    <div className="tabs">
      {TABS.map((tab) => (
        <button key={tab.id} type="button" className={cn('tab', statusTab === tab.id && 'active')} onClick={() => setStatusTab(tab.id)}>
          {t(tab.labelKey)} <em>{countByStatusTab(tasks, tab.id)}</em>
        </button>
      ))}
    </div>
  )
}
