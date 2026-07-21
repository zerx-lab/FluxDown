// 任务主界面的纯 UI 状态（筛选 / 搜索 / 选中 / 折叠 / 详情面板），与服务端数据（React Query）分离。
// react-compiler 已启用，不手写 useMemo/useCallback。

import { createContext, useContext, useState, type Dispatch, type ReactNode, type SetStateAction } from 'react'
import { dirKey } from '../../lib/task-group'
import type { FileType } from '../../lib/format'
import type { StatusTab } from './filters'

export type DetailTab = 'general' | 'segments' | 'queue' | 'log' | 'advanced'

const DEVICE_FILTER_KEY = 'fluxdown.tasks.deviceFilter'

interface TasksUiState {
  typeFilter: 'all' | FileType
  setTypeFilter: Dispatch<SetStateAction<'all' | FileType>>
  queueFilter: string
  setQueueFilter: Dispatch<SetStateAction<string>>
  /** 设备筛选：null=全部设备；本机=cloudDeviceId()；远程设备=其 deviceId（见 Sidebar 设备区）。 */
  deviceFilter: string | null
  setDeviceFilter: Dispatch<SetStateAction<string | null>>
  statusTab: StatusTab
  setStatusTab: Dispatch<SetStateAction<StatusTab>>
  search: string
  setSearch: Dispatch<SetStateAction<string>>
  manageMode: boolean
  setManageMode: Dispatch<SetStateAction<boolean>>
  selected: Set<string>
  setSelected: Dispatch<SetStateAction<Set<string>>>
  foldedSections: Set<string>
  toggleSectionFold: (key: string) => void
  expandedGroups: Set<string>
  toggleGroupExpand: (id: string) => void
  scrollTarget: string | null
  clearScrollTarget: () => void
  /** 失败直达：展开目标组（并展开成员所在目录，若已折叠）+ 请求 TaskList 滚动到该成员行。 */
  jumpToGroupMember: (groupId: string, taskId: string, dirPath?: string) => void
  collapsedDirs: Set<string>
  toggleDirCollapsed: (groupId: string, path: string) => void
  /** 当前选中的任务组（组详情面板；与 currentTaskId 互斥，见 selectGroup/selectTask）。 */
  selectedGroupId: string | null
  groupDetailOpen: boolean
  selectGroup: (id: string) => void
  closeGroupDetail: () => void
  currentTaskId: string | null
  detailOpen: boolean
  sidebarOpen: boolean
  setSidebarOpen: Dispatch<SetStateAction<boolean>>
  detailTab: DetailTab
  setDetailTab: Dispatch<SetStateAction<DetailTab>>
  selectTask: (id: string) => void
  closeDetail: () => void
}

const Ctx = createContext<TasksUiState | null>(null)

export function TasksUiProvider({ children }: { children: ReactNode }) {
  const [typeFilter, setTypeFilter] = useState<'all' | FileType>('all')
  const [queueFilter, setQueueFilter] = useState('all')
  const [deviceFilter, setDeviceFilterState] = useState<string | null>(() => localStorage.getItem(DEVICE_FILTER_KEY))
  const [statusTab, setStatusTab] = useState<StatusTab>('all')
  const [search, setSearch] = useState('')
  const [manageMode, setManageModeState] = useState(false)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [foldedSections, setFoldedSections] = useState<Set<string>>(new Set())
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set())
  const [scrollTarget, setScrollTarget] = useState<string | null>(null)
  const [collapsedDirs, setCollapsedDirs] = useState<Set<string>>(new Set())
  const [selectedGroupId, setSelectedGroupId] = useState<string | null>(null)
  const [groupDetailOpen, setGroupDetailOpen] = useState(false)
  const [currentTaskId, setCurrentTaskId] = useState<string | null>(null)
  const [detailOpen, setDetailOpen] = useState(false)
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const [detailTab, setDetailTab] = useState<DetailTab>('general')

  function setManageMode(v: SetStateAction<boolean>) {
    setManageModeState(v)
    setSelected(new Set())
  }
  function setDeviceFilter(v: SetStateAction<string | null>) {
    setDeviceFilterState((prev) => {
      const next = typeof v === 'function' ? (v as (p: string | null) => string | null)(prev) : v
      if (next === null) localStorage.removeItem(DEVICE_FILTER_KEY)
      else localStorage.setItem(DEVICE_FILTER_KEY, next)
      return next
    })
  }
  function toggleSectionFold(key: string) {
    setFoldedSections((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }
  function selectTask(id: string) {
    setCurrentTaskId(id)
    setDetailOpen(true)
    setSelectedGroupId(null)
    setGroupDetailOpen(false)
  }
  function selectGroup(id: string) {
    setSelectedGroupId(id)
    setGroupDetailOpen(true)
    setCurrentTaskId(null)
    setDetailOpen(false)
  }
  function closeGroupDetail() {
    setGroupDetailOpen(false)
  }
  function toggleGroupExpand(id: string) {
    setExpandedGroups((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }
  function jumpToGroupMember(groupId: string, taskId: string, dirPath?: string) {
    setExpandedGroups((prev) => (prev.has(groupId) ? prev : new Set(prev).add(groupId)))
    if (dirPath) {
      const key = dirKey(groupId, dirPath)
      setCollapsedDirs((prev) => {
        if (!prev.has(key)) return prev
        const next = new Set(prev)
        next.delete(key)
        return next
      })
    }
    setScrollTarget(taskId)
  }
  function toggleDirCollapsed(groupId: string, path: string) {
    const key = dirKey(groupId, path)
    setCollapsedDirs((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }
  function clearScrollTarget() {
    setScrollTarget(null)
  }
  function closeDetail() {
    setDetailOpen(false)
  }

  return (
    <Ctx.Provider
      value={{
        typeFilter,
        setTypeFilter,
        queueFilter,
        setQueueFilter,
        deviceFilter,
        setDeviceFilter,
        statusTab,
        setStatusTab,
        search,
        setSearch,
        manageMode,
        setManageMode,
        selected,
        setSelected,
        foldedSections,
        toggleSectionFold,
        expandedGroups,
        toggleGroupExpand,
        scrollTarget,
        clearScrollTarget,
        jumpToGroupMember,
        collapsedDirs,
        toggleDirCollapsed,
        selectedGroupId,
        groupDetailOpen,
        selectGroup,
        closeGroupDetail,
        currentTaskId,
        detailOpen,
        sidebarOpen,
        setSidebarOpen,
        detailTab,
        setDetailTab,
        selectTask,
        closeDetail,
      }}
    >
      {children}
    </Ctx.Provider>
  )
}

export function useTasksUi(): TasksUiState {
  const ctx = useContext(Ctx)
  if (!ctx) throw new Error('useTasksUi must be used within TasksUiProvider')
  return ctx
}
