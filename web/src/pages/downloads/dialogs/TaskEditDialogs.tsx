// 重命名文件 / 更换下载链接 / 修改线程数 / BT 移动数据 / BT 任务限速：任务级小对话框，引擎稳定错误码映射为本地化文案。

import { useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../../i18n'
import { RpcError, TASK_STATUS, rpc, useTask, useTasksLoading } from '../../../lib/rpc'
import { rpcErrorText } from '../../../lib/rpcErrorText'
import type { TaskDto } from '../../../lib/rpc'
import { ThreadsRiskCallout } from '../../../lib/ThreadsRiskCallout'
import { ConfirmFooter, Dialog, FieldError, FieldHint, Form, FormField, Input, Spinner, toast } from '../../../ui'
import { PathText } from '../../../ui/PathText'
import { moveTaskStorage, resumeIfFailed } from '../model/actions'
import { limitKibText, parseTaskSegments, taskLimitsParams } from '../model/taskEdit'
import { FsPickerDialog } from './FsPickerDialog'
import { closeChangeSegments, closeChangeUrl, closeMoveStorage, closeRename, closeTaskLimits } from './store'
import { mapEngineError } from './utils'

/** 引擎稳定错误码 → 文案键（native/engine `rename_task` 契约，错误消息即错误码）。 */
const RENAME_ERRORS: Readonly<Record<string, string>> = {
  'invalid-name': 'renameErrInvalidName',
  'task-active': 'renameErrTaskActive',
  'bt-unsupported': 'renameErrBtUnsupported',
  'target-exists': 'renameErrTargetExists',
  'not-found': 'renameErrNotFound',
}

/** 同上，`change_task_url` 契约。 */
const CHANGE_URL_ERRORS: Readonly<Record<string, string>> = {
  'invalid-url': 'webChangeUrlErrInvalidUrl',
  'task-active': 'webChangeUrlErrTaskActive',
  'task-completed': 'webChangeUrlErrTaskCompleted',
  'bt-unsupported': 'webChangeUrlErrBtUnsupported',
  'protocol-unsupported': 'webChangeUrlErrProtocolUnsupported',
  'protocol-mismatch': 'webChangeUrlErrProtocolMismatch',
  'not-found': 'webChangeUrlErrNotFound',
}

interface TaskScopedProps {
  taskId: string
  onClose: () => void
  render: (task: TaskDto) => ReactNode
}

function SingleFieldDialog({
  title,
  label,
  placeholder,
  okLabel,
  initial,
  submit,
  errors,
  timeoutKey,
  onClose,
}: {
  title: string
  label: string
  placeholder: string
  okLabel: string
  initial: string
  submit: (value: string) => Promise<unknown>
  errors: Readonly<Record<string, string>>
  timeoutKey?: string
  onClose: () => void
}) {
  const t = useT()
  const [value, setValue] = useState(initial)
  const [error, setError] = useState('')
  const [pending, setPending] = useState(false)

  const confirm = async () => {
    const trimmed = value.trim()
    if (trimmed === '' || pending) return
    if (trimmed === initial) {
      onClose()
      return
    }
    setPending(true)
    setError('')
    try {
      await submit(trimmed)
      onClose()
    } catch (err) {
      const key = err instanceof RpcError && err.is('timeout') && timeoutKey ? timeoutKey : mapEngineError(err, errors)
      setError(key ? t(key) : rpcErrorText(err, t))
      setPending(false)
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      size="md"
      title={title}
      footer={<ConfirmFooter okLabel={okLabel} okDisabled={value.trim() === ''} loading={pending} onCancel={onClose} onOk={() => void confirm()} />}
    >
      <Form onSubmit={() => void confirm()}>
        <FormField label={label} htmlFor="task-edit-field">
          <Input
            id="task-edit-field"
            value={value}
            placeholder={placeholder}
            spellCheck={false}
            autoFocus
            autoComplete="off"
            onFocus={(event) => event.target.select()}
            onChange={(event) => setValue(event.target.value)}
          />
        </FormField>
        {error ? <FieldError className="break-all">{error}</FieldError> : null}
        {/* 让回车提交：隐藏的提交按钮 */}
        <button type="submit" className="hidden" tabIndex={-1} aria-hidden />
      </Form>
    </Dialog>
  )
}

export function RenameDialog({ taskId }: { taskId: string }) {
  return <TaskScoped taskId={taskId} onClose={closeRename} render={(task) => <RenameBody taskId={taskId} fileName={task.fileName} />} />
}

function RenameBody({ taskId, fileName }: { taskId: string; fileName: string }) {
  const t = useT()
  return (
    <SingleFieldDialog
      title={t('renameTaskTitle')}
      label={t('colFileName')}
      placeholder={t('renameTaskPlaceholder')}
      okLabel={t('confirm')}
      initial={fileName}
      errors={RENAME_ERRORS}
      timeoutKey="renameTaskTimeout"
      onClose={closeRename}
      submit={async (name) => {
        await rpc.daemon.task.rename({ taskId, fileName: name })
        toast.key('renameTaskSuccess', 'success')
      }}
    />
  )
}

export function ChangeUrlDialog({ taskId }: { taskId: string }) {
  return (
    <TaskScoped
      taskId={taskId}
      onClose={closeChangeUrl}
      render={(task) => <ChangeUrlBody taskId={taskId} url={task.url} failed={task.status === TASK_STATUS.error} />}
    />
  )
}

/** 换链成功后失败任务自动重试（提交时刻的状态；暂停 / 等待保持原状态）。 */
function ChangeUrlBody({ taskId, url, failed }: { taskId: string; url: string; failed: boolean }) {
  const t = useT()
  return (
    <SingleFieldDialog
      title={t('webChangeUrlTitle')}
      label={t('webChangeUrlLabel')}
      placeholder={t('webChangeUrlPlaceholder')}
      okLabel={t('webChangeUrl')}
      initial={url}
      errors={CHANGE_URL_ERRORS}
      onClose={closeChangeUrl}
      submit={async (next) => {
        await rpc.daemon.task.changeUrl({ taskId, url: next })
        await resumeIfFailed(taskId, failed)
      }}
    />
  )
}

export function ChangeSegmentsDialog({ taskId }: { taskId: string }) {
  return (
    <TaskScoped
      taskId={taskId}
      onClose={closeChangeSegments}
      render={(task) => <ChangeSegmentsBody taskId={taskId} failed={task.status === TASK_STATUS.error} />}
    />
  )
}

/** 修改线程数：已下进度保留；成功后失败任务自动重试。任务 DTO 不含当前线程数，故输入框留空。 */
function ChangeSegmentsBody({ taskId, failed }: { taskId: string; failed: boolean }) {
  const t = useT()
  const [value, setValue] = useState('')
  const [error, setError] = useState('')
  const [pending, setPending] = useState(false)
  const segments = parseTaskSegments(value)

  const confirm = async () => {
    if (pending) return
    if (segments === null) {
      setError(t('editThreadsInvalid'))
      return
    }
    setPending(true)
    setError('')
    try {
      await rpc.daemon.task.setSegments({ taskId, segments })
    } catch (err) {
      // 已完成或已不存在的任务：daemon 统一返回 not-found。
      setError(err instanceof RpcError && err.is('notFound') ? t('threadsChangeRejected') : rpcErrorText(err, t))
      setPending(false)
      return
    }
    toast.key('threadsChanged', 'success')
    closeChangeSegments()
    await resumeIfFailed(taskId, failed)
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && closeChangeSegments()}
      size="md"
      title={t('editThreadsTitle')}
      footer={<ConfirmFooter okLabel={t('confirm')} okDisabled={value.trim() === ''} loading={pending} onCancel={closeChangeSegments} onOk={() => void confirm()} />}
    >
      <Form onSubmit={() => void confirm()}>
        <FormField label={t('configuredThreads')} htmlFor="task-segments-field" hint={t('queueDefaultSegmentsHint')}>
          <Input
            id="task-segments-field"
            value={value}
            placeholder="0"
            inputMode="numeric"
            autoFocus
            autoComplete="off"
            onChange={(event) => {
              setValue(event.target.value.replace(/\D/g, ''))
              setError('')
            }}
          />
        </FormField>
        <ThreadsRiskCallout segments={segments ?? 0} />
        <FieldHint>{t('editThreadsResetHint')}</FieldHint>
        {error ? <FieldError className="break-all">{error}</FieldError> : null}
        {/* 让回车提交：隐藏的提交按钮 */}
        <button type="submit" className="hidden" tabIndex={-1} aria-hidden />
      </Form>
    </Dialog>
  )
}

export function MoveStorageDialog({ taskId }: { taskId: string }) {
  return (
    <TaskScoped
      taskId={taskId}
      onClose={closeMoveStorage}
      render={(task) => <MoveStorageBody taskId={taskId} name={task.fileName} saveDir={task.saveDir} />}
    />
  )
}

/**
 * 「移动到…」：先用新建下载同款的服务端目录选择器选目标，再调用慢方法 `daemon.task.moveStorage`；
 * 进行中显示加载态。关掉加载态不会中止移动，结果仍以 toast 反馈。
 */
function MoveStorageBody({ taskId, name, saveDir }: { taskId: string; name: string; saveDir: string }) {
  const t = useT()
  const [target, setTarget] = useState<string | null>(null)
  // 选择器「确定」先回调 onSelect 再关闭自身：选定后的那次关闭不能把对话框整体关掉。
  const chosen = useRef(false)
  // 移动完成时对话框可能已被用户关掉（甚至又为别的任务打开）：只关闭自己。
  const alive = useRef(true)
  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
    }
  }, [])

  if (target === null) {
    return (
      <FsPickerDialog
        open
        title={t('btMoveStorageTitle')}
        initialPath={saveDir}
        onOpenChange={(open) => !open && !chosen.current && closeMoveStorage()}
        onSelect={(path) => {
          chosen.current = true
          setTarget(path)
          void moveTaskStorage(taskId, path).finally(() => {
            if (alive.current) closeMoveStorage()
          })
        }}
      />
    )
  }
  return (
    <Dialog open onOpenChange={(open) => !open && closeMoveStorage()} size="md" title={t('btMoveStorageTitle')}>
      <div className="flex min-w-0 items-center gap-2 text-sm">
        <Spinner />
        <span className="min-w-0 break-all">{t('btMoveStorageWorking', { name })}</span>
      </div>
      <PathText path={target} className="mt-2 text-xs text-muted-foreground" />
    </Dialog>
  )
}

export function TaskLimitsDialog({ taskId }: { taskId: string }) {
  return (
    <TaskScoped
      taskId={taskId}
      onClose={closeTaskLimits}
      render={(task) => (
        <TaskLimitsBody taskId={taskId} initial={[limitKibText(task.btDownloadLimitBps), limitKibText(task.seedUploadLimitBps)]} />
      )}
    />
  )
}

/** 任务级限速（KiB/s，0 = 不限）：初值取任务当前值，只下发改动的字段；未改直接关闭。 */
function TaskLimitsBody({ taskId, initial }: { taskId: string; initial: readonly [string, string] }) {
  const t = useT()
  // 初值只在首次渲染时取：轮询 / 事件刷新任务行时不覆盖正在编辑的输入。
  const [first] = useState(initial)
  const [download, setDownload] = useState(first[0])
  const [upload, setUpload] = useState(first[1])
  const [error, setError] = useState('')
  const [pending, setPending] = useState(false)

  const confirm = async () => {
    if (pending) return
    const built = taskLimitsParams(taskId, first, [download, upload])
    if (!built.ok) {
      setError(t('btTaskLimitHint'))
      return
    }
    if (built.params === null) {
      closeTaskLimits()
      return
    }
    setPending(true)
    setError('')
    try {
      await rpc.daemon.task.setBtLimits(built.params)
      closeTaskLimits()
    } catch (err) {
      setError(rpcErrorText(err, t))
      setPending(false)
    }
  }

  const field = (id: string, label: string, value: string, set: (next: string) => void) => (
    <FormField label={label} htmlFor={id}>
      <div className="flex items-center gap-2">
        <Input
          id={id}
          value={value}
          placeholder="0"
          inputMode="numeric"
          autoComplete="off"
          onChange={(event) => {
            set(event.target.value.replace(/\D/g, ''))
            setError('')
          }}
        />
        <span className="shrink-0 text-sm text-muted-foreground">{t('btGuardUnitKibps')}</span>
      </div>
    </FormField>
  )

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && closeTaskLimits()}
      size="sm"
      title={t('btTaskLimitsTitle')}
      footer={<ConfirmFooter okLabel={t('confirm')} loading={pending} onCancel={closeTaskLimits} onOk={() => void confirm()} />}
    >
      <Form onSubmit={() => void confirm()}>
        {field('task-limit-download', t('btTaskLimitDownload'), download, setDownload)}
        {field('task-limit-upload', t('btTaskLimitUpload'), upload, setUpload)}
        <FieldHint>{t('btTaskLimitHint')}</FieldHint>
        {error ? <FieldError className="break-all">{error}</FieldError> : null}
        {/* 让回车提交：隐藏的提交按钮 */}
        <button type="submit" className="hidden" tabIndex={-1} aria-hidden />
      </Form>
    </Dialog>
  )
}

/**
 * 任务被删除（快照里消失）时自动关闭；表单初值只在首次渲染时取。
 * 任务表重新载入期间任务暂时不在快照里，不代表被删：保持最后一次见到的任务（表单状态不丢），不关闭。
 */
function TaskScoped({ taskId, onClose, render }: TaskScopedProps) {
  const task = useTask(taskId)
  const loading = useTasksLoading()
  const lastTask = useRef(task)
  if (task) lastTask.current = task
  useEffect(() => {
    if (!task && !loading) onClose()
  }, [task, loading, onClose])
  const shown = task ?? (loading ? lastTask.current : undefined)
  return shown ? render(shown) : null
}
