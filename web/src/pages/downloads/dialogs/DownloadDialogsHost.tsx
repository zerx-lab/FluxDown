// 下载页专属对话框，以及进入新建下载的外部捕获入口。

import { useEffect } from 'react'
import { useAgent } from '../../../lib/rpc'
import type { PendingCaptureDto } from '../../../lib/rpc'
import { GroupDetailDialog } from './GroupDetailDialog'
import { QueueManagerDialog } from './QueueManagerDialog'
import { capturesShown, openNewDownload, useDialogsState } from './store'
import { AddWebSeedDialog, ChangeSegmentsDialog, ChangeUrlDialog, MoveStorageDialog, RenameDialog, TaskLimitsDialog } from './TaskEditDialogs'
import { TaskPropertiesDialog } from './TaskPropertiesDialog'

const EMPTY_CAPTURES: readonly PendingCaptureDto[] = []

export function DownloadDialogsHost() {
  const state = useDialogsState()
  const captures = useAgent((snapshot) => snapshot.pendingCaptures, EMPTY_CAPTURES)

  // 外部捕获（浏览器扩展 / 协议关联）到达：拉起新建下载表单并把捕获并入链接行。
  const hasFreshCapture = captures.some((capture) => !capturesShown.has(capture.transactionId))
  useEffect(() => {
    if (hasFreshCapture && state.newDownload === null) openNewDownload()
  }, [hasFreshCapture, state.newDownload])

  return (
    <>
      {state.queueManager ? <QueueManagerDialog key={state.queueManager.nonce} initialQueueId={state.queueManager.queueId} /> : null}
      {state.groupDetail ? <GroupDetailDialog key={state.groupDetail} groupId={state.groupDetail} /> : null}
      {state.rename ? <RenameDialog key={state.rename} taskId={state.rename} /> : null}
      {state.changeUrl ? <ChangeUrlDialog key={state.changeUrl} taskId={state.changeUrl} /> : null}
      {state.taskEdit ? <TaskPropertiesDialog key={state.taskEdit} taskId={state.taskEdit} /> : null}
      {state.changeSegments ? <ChangeSegmentsDialog key={state.changeSegments} taskId={state.changeSegments} /> : null}
      {state.moveStorage ? <MoveStorageDialog key={state.moveStorage} taskId={state.moveStorage} /> : null}
      {state.taskLimits ? <TaskLimitsDialog key={state.taskLimits} taskId={state.taskLimits} /> : null}
      {state.addWebSeed ? <AddWebSeedDialog key={state.addWebSeed.session} taskId={state.addWebSeed.taskId} session={state.addWebSeed.session} /> : null}
    </>
  )
}
