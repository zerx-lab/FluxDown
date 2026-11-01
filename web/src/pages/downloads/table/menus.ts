// 任务行 / 分组头右键菜单（移植 task_table.rs 的 `context_menu_items` 与 `group_context_menu`）。
// 「打开文件 / 在文件夹中显示」仅在 agent 下发 `agent.openTaskFiles`（同机）时出现，「复制文件」仅在下发
// `agent.copyTaskFiles`（同机桌面会话）时出现；远程 / NAS 连接都用浏览器下载已完成文件。

import {
  AppWindow,
  CircleAlert,
  CircleCheck,
  Copy,
  Download,
  FileDown,
  Files,
  Layers,
  Link2,
  Magnet,
  FolderOpen,
  Pause,
  PanelRight,
  Pen,
  Play,
  SquareCheck,
  RotateCw,
  Rows3,
  ShieldCheck,
  Trash2,
  Zap,
} from 'lucide-react'
import type { GroupDto, QueueDto } from '../../../lib/rpc'
import { runPluginCommand } from '../../plugins/commands'
import type { PluginCommandRef } from '../../plugins/contributions'
import type { MenuEntry } from '../../../ui'
import {
  canOpenGroupFolder,
  canCopyTaskFiles,
  canOpenTaskFile,
  canRedownload,
  canRevealTaskFile,
  confirmDeleteGroupWithFiles,
  confirmDeleteWithFiles,
  confirmIgnorePluginRetry,
  copySourceLink,
  copyUrls,
  copyMagnet,
  exportTorrent,
  canShareTorrent,
  copyTaskFiles,
  deleteGroup,
  deleteViews,
  downloadViewsFiles,
  isDownloadable,
  isPluginRetryError,
  moveViewsToQueue,
  openTaskFile,
  openGroupFolder,
  pauseGroup,
  pauseViews,
  redownloadViews,
  resumeGroup,
  resumeViews,
  retryFailedInGroup,
  revealTaskFile,
  toggleBoost,
  verifyTaskIntegrity,
} from '../model/actions'
import { remoteCan } from '../model/batchPlan'
import { canChangeSegments, canChangeUrl, canRename, canVerifyIntegrity } from '../model/taskEdit'
import { canPause, canResumeSeeding } from '../model/task'
import type { DownloadTaskView } from '../model/task'
import { anyActive, anyResumable } from '../model/groupTree'
import type { GroupCounts } from '../model/groupTree'
import { openChangeSegments, openChangeUrl, openGroupDetail, openRename } from '../dialogs'

type Translate = (key: string, params?: Record<string, string | number>) => string

const IS_MAC = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/i.test(navigator.platform || navigator.userAgent)

/** 多选模式的右键入口（桌面表格专用；卡片视图有自己的点选方式，不传）。 */
interface SelectionEntries {
  /** 已处于显式多选模式时不再显示「选择 / 全选」。 */
  multiSelect: boolean
  enterMultiSelect: () => void
  selectAll: () => void
}

interface TaskMenuContext {
  t: Translate
  views: readonly DownloadTaskView[]
  queues: readonly QueueDto[]
  queueName: (queueId: string) => string
  showDetail: (taskId: string) => void
  selection?: SelectionEntries
  /** 插件贡献的任务右键命令（`taskMenu: true`），位于选择组与删除组之间。 */
  pluginCommands?: readonly PluginCommandRef[]
}

/** 任务行右键菜单：多选时每一项都要求对全部选中任务成立（交集语义），resume/pause 例外。 */
/** 插件命令分组（含前置分隔线）；无命令返回空数组。 */
export function pluginTaskMenuEntries(t: Translate, commands: readonly PluginCommandRef[], taskIds: readonly string[]): MenuEntry[] {
  if (commands.length === 0) return []
  return [
    { type: 'separator', key: 'sep-plugin' },
    ...commands.map(
      (ref): MenuEntry => ({
        type: 'item',
        key: `plugin:${ref.identity}:${ref.command.id}`,
        label: `${ref.pluginName}: ${ref.command.title}`,
        onSelect: () => void runPluginCommand(t, ref.identity, ref.command.id, [...taskIds]),
      }),
    ),
  ]
}

export function buildTaskMenu({ t, views, queues, queueName, showDetail, selection, pluginCommands = [] }: TaskMenuContext): MenuEntry[] {
  if (views.length === 0) return []
  const entries: MenuEntry[] = []
  const only = views.length === 1 ? views[0] : undefined
  const allLocal = views.every((view) => view.source === 'local')

  if (views.some((view) => (view.state !== 'completed' ? view.state !== 'downloading' : canResumeSeeding(view)) && remoteCan(view, 'resume'))) {
    entries.push({ type: 'item', key: 'resume', label: t('resume'), icon: Play, onSelect: () => void resumeViews(views) })
  }
  if (views.some((view) => canPause(view) && remoteCan(view, 'pause'))) {
    entries.push({ type: 'item', key: 'pause', label: t('pause'), icon: Pause, onSelect: () => void pauseViews(views) })
  }
  if (only) {
    if (isPluginRetryError(only)) {
      entries.push({
        type: 'item',
        key: 'ignore-plugin-retry',
        label: t('taskIgnorePluginRetry'),
        icon: CircleAlert,
        onSelect: () => void confirmIgnorePluginRetry(only.taskId),
      })
    }
    if (only.source === 'local') {
      // 已完成 / 失败的任务不会处于 Boost；残留标记也只显示「优先下载」，点击给出提示。
      const ended = only.state === 'completed' || only.state === 'failed'
      entries.push({
        type: 'item',
        key: 'boost',
        label: only.boosted && !ended ? t('cancelBoost') : t('boostDownload'),
        icon: Zap,
        onSelect: () => void toggleBoost(only),
      })
    }
  }
  if (views.every(isDownloadable)) {
    entries.push({
      type: 'item',
      key: 'download-file',
      label: t('webDownloadFile'),
      icon: Download,
      onSelect: () => downloadViewsFiles(views),
    })
  }
  if (only && canOpenTaskFile(only)) {
    entries.push({
      type: 'item',
      key: 'open-file',
      label: t('openFile'),
      icon: AppWindow,
      onSelect: () => void openTaskFile(only.taskId),
    })
  }
  if (only && canRevealTaskFile(only)) {
    entries.push({
      type: 'item',
      key: 'reveal-file',
      label: t('openFolder'),
      icon: FolderOpen,
      onSelect: () => void revealTaskFile(only.taskId),
    })
  }
  if (canCopyTaskFiles(views)) {
    entries.push({
      type: 'item',
      key: 'copy-files',
      label: t('copyFileAction'),
      icon: Files,
      onSelect: () => void copyTaskFiles(views),
    })
  }
  if (only && canRename(only)) {
    entries.push({ type: 'item', key: 'rename', label: t('renameTask'), icon: Pen, onSelect: () => openRename(only.taskId) })
  }
  if (only && canChangeUrl(only)) {
    entries.push({ type: 'item', key: 'change-url', label: t('webChangeUrl'), icon: Link2, onSelect: () => openChangeUrl(only.taskId) })
  }
  if (only && canChangeSegments(only)) {
    entries.push({
      type: 'item',
      key: 'change-segments',
      label: t('editThreads'),
      icon: Layers,
      onSelect: () => openChangeSegments(only.taskId),
    })
  }
  if (only && canVerifyIntegrity(only)) {
    entries.push({
      type: 'item',
      key: 'verify-integrity',
      label: t('verifyTaskIntegrity'),
      icon: ShieldCheck,
      onSelect: () => void verifyTaskIntegrity(only),
    })
  }
  if (views.every(canRedownload)) {
    entries.push({
      type: 'item',
      key: 'redownload',
      label: t('redownloadTask'),
      icon: RotateCw,
      onSelect: () => void redownloadViews(views),
    })
  }
  entries.push({ type: 'item', key: 'copy-url', label: t('copyUrl'), icon: Copy, onSelect: () => copyUrls(views) })
  if (only && canShareTorrent(views)) {
    entries.push({ type: 'item', key: 'copy-magnet', label: t('btCopyMagnet'), icon: Magnet, onSelect: () => void copyMagnet(only) })
    entries.push({ type: 'item', key: 'export-torrent', label: t('btExportTorrent'), icon: FileDown, onSelect: () => void exportTorrent(only) })
  }
  if (allLocal && queues.length > 0) {
    entries.push({
      type: 'sub',
      key: 'move-to-queue',
      label: t('moveToQueueAction'),
      icon: Rows3,
      entries: queues.map((queue) => ({
        type: 'item' as const,
        key: `queue:${queue.queueId}`,
        label: queueName(queue.queueId),
        onSelect: () => void moveViewsToQueue(views, queue.queueId),
      })),
    })
  }
  // 与 GPUI `task_menu_entries` 对齐：未处于多选模式时在删除组前插入「选择 / 全选」。
  if (selection && !selection.multiSelect) {
    entries.push({ type: 'separator', key: 'sep-select' })
    entries.push({ type: 'item', key: 'select', label: t('selectTasks'), icon: CircleCheck, onSelect: selection.enterMultiSelect })
    entries.push({
      type: 'item',
      key: 'select-all',
      label: t('selectAll'),
      icon: SquareCheck,
      shortcut: IS_MAC ? '⌘A' : 'Ctrl+A',
      onSelect: selection.selectAll,
    })
  }
  const localTaskIds = views.filter((view) => view.source === 'local').map((view) => view.taskId)
  if (localTaskIds.length > 0) entries.push(...pluginTaskMenuEntries(t, pluginCommands, localTaskIds))
  entries.push({ type: 'separator', key: 'sep-delete' })
  entries.push({
    type: 'item',
    key: 'delete',
    label: t('deleteTask'),
    icon: Trash2,
    destructive: true,
    onSelect: () => void deleteViews(views, false),
  })
  entries.push({
    type: 'item',
    key: 'delete-with-files',
    label: t('deleteTaskAndFile'),
    icon: Trash2,
    destructive: true,
    onSelect: () => void confirmDeleteWithFiles(views),
  })
  const firstLocal = views.find((view) => view.source === 'local')
  if (firstLocal) {
    entries.push({
      type: 'item',
      key: 'detail',
      label: t('detail'),
      icon: PanelRight,
      onSelect: () => showDetail(firstLocal.taskId),
    })
  }
  return entries
}

/**
 * 任务组「文件夹行」右键菜单：用全组成员（不受侧栏筛选 / 搜索影响）判定可用项。
 * `counts` 为全组计数；`allViews` 在「打开组文件夹」「重试失败项」点击时才取。
 */
export function buildGroupMenu(
  t: Translate,
  group: GroupDto,
  name: string,
  counts: GroupCounts | undefined,
  allViews: () => readonly DownloadTaskView[],
): MenuEntry[] {
  const groupId = group.groupId
  const entries: MenuEntry[] = []
  if (counts !== undefined && anyActive(counts)) {
    entries.push({ type: 'item', key: 'pause-all', label: t('groupPauseAll'), icon: Pause, onSelect: () => void pauseGroup(groupId) })
  }
  if (counts !== undefined && anyResumable(counts)) {
    entries.push({ type: 'item', key: 'resume-all', label: t('groupResumeAll'), icon: Play, onSelect: () => void resumeGroup(groupId) })
  }
  if (counts !== undefined && counts.failed > 0) {
    entries.push({
      type: 'item',
      key: 'retry-failed',
      label: t('groupRetryFailed'),
      icon: RotateCw,
      onSelect: () => void retryFailedInGroup(allViews(), groupId),
    })
  }
  if (canOpenGroupFolder()) {
    entries.push({
      type: 'item',
      key: 'open-folder',
      label: t('groupOpenFolder'),
      icon: FolderOpen,
      onSelect: () => void openGroupFolder(allViews(), groupId, group.saveDir),
    })
  }
  if (group.sourceUrl !== '') {
    entries.push({
      type: 'item',
      key: 'copy-source',
      label: t('groupCopySourceLink'),
      icon: Copy,
      onSelect: () => copySourceLink(group.sourceUrl),
    })
  }
  entries.push({ type: 'separator', key: 'sep-delete' })
  entries.push({
    type: 'item',
    key: 'delete',
    label: t('groupDelete'),
    icon: Trash2,
    destructive: true,
    onSelect: () => void deleteGroup(groupId, false),
  })
  entries.push({
    type: 'item',
    key: 'delete-with-files',
    label: t('groupDeleteWithFiles'),
    icon: Trash2,
    destructive: true,
    onSelect: () => void confirmDeleteGroupWithFiles(groupId, name),
  })
  entries.push({
    type: 'item',
    key: 'open-group',
    label: t('openGroupInWindowAction'),
    icon: AppWindow,
    onSelect: () => openGroupDetail(groupId),
  })
  return entries
}
