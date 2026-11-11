// 下载页对话框宿主：由 DownloadsPage 挂载一次，渲染 store 里所有对话框，
// 并根据快照自动弹出：daemon 的交互选择请求（HLS / BT / 变体 / 文件已存在聚合）与 agent 的待确认外部捕获。

import { useEffect } from 'react'
import { useAgent, useDaemon } from '../../../lib/rpc'
import type { PendingCaptureDto, SelectionRequestDto } from '../../../lib/rpc'
import { FileConflictDialog } from './FileConflictDialog'
import { GroupDetailDialog } from './GroupDetailDialog'
import { NewDownloadDialog } from './NewDownloadDialog'
import { QueueManagerDialog } from './QueueManagerDialog'
import { SeedLocalDialog } from './SeedLocalDialog'
import { SelectionHost } from './SelectionDialogs'
import { capturesShown, openNewDownload, useDialogsState } from './store'
import { ChangeSegmentsDialog, ChangeUrlDialog, MoveStorageDialog, RenameDialog, TaskLimitsDialog } from './TaskEditDialogs'

const EMPTY_SELECTIONS: readonly SelectionRequestDto[] = []
const EMPTY_CAPTURES: readonly PendingCaptureDto[] = []

export function DownloadDialogsHost() {
  const state = useDialogsState()
  const selections = useDaemon((daemon) => daemon.pendingSelections, EMPTY_SELECTIONS)
  const captures = useAgent((snapshot) => snapshot.pendingCaptures, EMPTY_CAPTURES)

  // 外部捕获（浏览器扩展 / 协议关联）到达：拉起新建下载表单并把捕获并入链接行。
  const hasFreshCapture = captures.some((capture) => !capturesShown.has(capture.transactionId))
  useEffect(() => {
    if (hasFreshCapture && state.newDownload === null) openNewDownload()
  }, [hasFreshCapture, state.newDownload])

  return (
    <>
      {state.newDownload ? <NewDownloadDialog key={state.newDownload.session} session={state.newDownload} /> : null}
      {state.queueManager ? <QueueManagerDialog key={state.queueManager.nonce} initialQueueId={state.queueManager.queueId} /> : null}
      {state.groupDetail ? <GroupDetailDialog key={state.groupDetail} groupId={state.groupDetail} /> : null}
      {state.rename ? <RenameDialog key={state.rename} taskId={state.rename} /> : null}
      {state.changeUrl ? <ChangeUrlDialog key={state.changeUrl} taskId={state.changeUrl} /> : null}
      {state.changeSegments ? <ChangeSegmentsDialog key={state.changeSegments} taskId={state.changeSegments} /> : null}
      {state.moveStorage ? <MoveStorageDialog key={state.moveStorage} taskId={state.moveStorage} /> : null}
      {state.taskLimits ? <TaskLimitsDialog key={state.taskLimits} taskId={state.taskLimits} /> : null}
      {state.seedLocal ? <SeedLocalDialog /> : null}
      <SelectionHost requests={selections} />
      <FileConflictDialog />
    </>
  )
}
