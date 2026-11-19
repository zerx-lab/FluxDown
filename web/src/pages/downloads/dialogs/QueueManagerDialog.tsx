import { byteUnit, useFileSizeUnits } from '../../../lib/fileSizeUnits'
// 队列管理：左列队列列表，右侧表单编辑（新建 / 更新 / 定时 / 启停 / 删除 / 待处理任务排序）。
// 与 GPUI `pages/queue_manager.rs` 对齐；移动端（<=820px）为「列表 → 详情」两级导航。

import { ArrowDown, ArrowLeft, ArrowUp, ChevronRight, Plus } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useT } from '../../../i18n'
import { LATER_QUEUE_ID, MAIN_QUEUE_ID, TASK_STATUS, rpc, useAgent, useDaemon, useTasks, useTasksLoading } from '../../../lib/rpc'
import type { QueueDto, TaskDto } from '../../../lib/rpc'
import { Button, Checkbox, Dialog, DialogFooter, FieldError, FieldHint, Form, FormField, FormRow, Icon, Input, InputWithAction, OptionGroup, OptionRow, SectionHeader, Select, Switch, confirmDialog, useIsMobile } from '../../../ui'
import { toastRpcError } from '../../../lib/rpcToast'
import { cn } from '../../../lib/cn'
import { ThreadsRiskCallout } from '../../../lib/ThreadsRiskCallout'
import { FsPickerDialog } from './FsPickerDialog'
import { closeQueueManager } from './store'
import { queueLabel } from './utils'

const EMPTY_QUEUES: readonly QueueDto[] = []
const MINUTE_STEP = 5

/** `HH:MM` → 当日分钟数；空串或非法值 = 不定时。 */
function parseTime(text: string): number | null {
  const match = /^(\d{1,2}):(\d{1,2})$/.exec(text.trim())
  if (!match) return null
  const hours = Number(match[1])
  const minutes = Number(match[2])
  return hours < 24 && minutes < 60 ? hours * 60 + minutes : null
}

function formatTime(minutes: number | null): string {
  if (minutes === null) return ''
  return `${String(Math.floor(minutes / 60)).padStart(2, '0')}:${String(minutes % 60).padStart(2, '0')}`
}

/** 分钟候选：按步长取整点，外加当前值（旧数据可能不是步长整数倍），升序去重。 */
function minuteChoices(current: number): number[] {
  const choices = new Set<number>()
  for (let minute = 0; minute < 60; minute += MINUTE_STEP) choices.add(minute)
  choices.add(current)
  return [...choices].sort((a, b) => a - b)
}

const pad2 = (value: number) => String(value).padStart(2, '0')

function intOrZero(text: string): number {
  const value = Number.parseInt(text.trim(), 10)
  return Number.isFinite(value) && value > 0 ? value : 0
}

function isBuiltin(queueId: string | null): boolean {
  return queueId === MAIN_QUEUE_ID || queueId === LATER_QUEUE_ID
}

export function QueueManagerDialog({ initialQueueId }: { initialQueueId?: string }) {
  const t = useT()
  const mobile = useIsMobile()
  const queues = useDaemon((daemon) => [...daemon.queues].sort((a, b) => a.position - b.position), EMPTY_QUEUES, shallowQueues)
  const connected = useAgent((snapshot) => snapshot.daemonConnected, true)
  /** null = 新建；string = 已有队列；undefined = 尚未选择（移动端停在列表）。 */
  const [selection, setSelection] = useState<string | null | undefined>(() => initialQueueId ?? undefined)
  const [mobileDetail, setMobileDetail] = useState(initialQueueId !== undefined)

  // 桌面端默认选中主队列（GPUI ensure_form）。已选队列被删除则清除选择。
  useEffect(() => {
    if (queues.length === 0) return
    if (selection === undefined && !mobile) {
      setSelection((queues.find((queue) => queue.queueId === MAIN_QUEUE_ID) ?? queues[0]).queueId)
    } else if (typeof selection === 'string' && !queues.some((queue) => queue.queueId === selection)) {
      setSelection(undefined)
      setMobileDetail(false)
    }
  }, [queues, selection, mobile])

  const selectedQueue = typeof selection === 'string' ? queues.find((queue) => queue.queueId === selection) : undefined
  const showList = !mobile || !mobileDetail
  const showDetail = !mobile || mobileDetail

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && closeQueueManager()}
      size="xl"
      title={t('manageQueueAction')}
      className="desktop:h-[min(620px,85dvh)]"
    >
      {!connected ? <FieldHint className="pb-2">{t('localServiceDisconnected')}</FieldHint> : null}
      <div className="grid gap-4 desktop:grid-cols-[184px_minmax(0,1fr)]">
        {showList ? (
          <QueueList
            queues={queues}
            selection={selection}
            onSelect={(queueId) => {
              setSelection(queueId)
              setMobileDetail(true)
            }}
            onCreate={() => {
              setSelection(null)
              setMobileDetail(true)
            }}
          />
        ) : null}
        {showDetail ? (
          selection === null ? (
            <QueueEditor key="new" queue={null} onBack={mobile ? () => setMobileDetail(false) : undefined} onSaved={(queueId) => setSelection(queueId ?? undefined)} onDeleted={() => setSelection(undefined)} />
          ) : selectedQueue ? (
            <QueueEditor
              key={selectedQueue.queueId}
              queue={selectedQueue}
              onBack={mobile ? () => setMobileDetail(false) : undefined}
              onSaved={() => undefined}
              onDeleted={() => {
                setSelection(undefined)
                setMobileDetail(false)
              }}
            />
          ) : null
        ) : null}
      </div>
    </Dialog>
  )
}

function shallowQueues(a: readonly QueueDto[], b: readonly QueueDto[]): boolean {
  return a.length === b.length && a.every((item, index) => item === b[index])
}

function StatusDot({ running }: { running: boolean }) {
  return <span className={cn('size-1.5 shrink-0 rounded-full', running ? 'bg-success' : 'bg-text-tertiary')} aria-hidden />
}

function QueueList({
  queues,
  selection,
  onSelect,
  onCreate,
}: {
  queues: readonly QueueDto[]
  selection: string | null | undefined
  onSelect: (queueId: string) => void
  onCreate: () => void
}) {
  const t = useT()
  return (
    <div className="flex min-w-0 flex-col gap-0.5 desktop:sticky desktop:top-0 desktop:self-start">
      <SectionHeader
        trailing={
          <Button variant="ghost" iconOnly title={t('createQueueAction')} aria-label={t('createQueueAction')} onClick={onCreate}>
            <Icon icon={Plus} />
          </Button>
        }
      >
        {t('sidebarQueues')}
      </SectionHeader>
      {queues.map((queue) => (
        <button
          key={queue.queueId}
          type="button"
          onClick={() => onSelect(queue.queueId)}
          className={cn(
            'flex min-h-nav-row w-full items-center gap-2 rounded-md px-2 text-left text-sm coarse:min-h-touch',
            selection === queue.queueId ? 'bg-nav-selected' : 'hover:bg-nav-hover',
          )}
        >
          <StatusDot running={queue.isRunning} />
          <span className="min-w-0 flex-1 truncate">{queueLabel(t, queue)}</span>
          <Icon icon={ChevronRight} size="md" className="text-text-tertiary desktop:hidden" />
        </button>
      ))}
      {selection === null ? (
        <div className="flex min-h-nav-row items-center gap-2 rounded-md bg-nav-selected px-2 text-sm coarse:min-h-touch">
          <Icon icon={Plus} size="md" />
          <span className="truncate">{t('createQueueAction')}</span>
        </div>
      ) : null}
    </div>
  )
}

// ── 编辑表单 ──

function QueueEditor({
  queue,
  onBack,
  onSaved,
  onDeleted,
}: {
  queue: QueueDto | null
  onBack?: () => void
  onSaved: (createdQueueId: string | null) => void
  onDeleted: () => void
}) {
  const fileSizeUnits = useFileSizeUnits()
  const speedUnit = `${byteUnit(1, fileSizeUnits)}/s`
  const t = useT()
  const [name, setName] = useState(queue?.name ?? '')
  const [maxConcurrent, setMaxConcurrent] = useState(String(queue?.maxConcurrent ?? 0))
  const [speedLimit, setSpeedLimit] = useState(String(queue?.speedLimitKbps ?? 0))
  const [uploadLimit, setUploadLimit] = useState(String(queue?.uploadLimitKbps ?? 0))
  const [saveDir, setSaveDir] = useState(queue?.defaultSaveDir ?? '')
  const [segments, setSegments] = useState(String(queue?.defaultSegments ?? 0))
  const [userAgent, setUserAgent] = useState(queue?.defaultUserAgent ?? '')
  const [scheduleEnabled, setScheduleEnabled] = useState(queue?.scheduleEnabled ?? false)
  const [scheduleStart, setScheduleStart] = useState<number | null>(() => parseTime(queue?.scheduleStart ?? ''))
  const [scheduleStop, setScheduleStop] = useState<number | null>(() => parseTime(queue?.scheduleStop ?? ''))
  const [days, setDays] = useState(queue?.scheduleDays ?? 127)
  const [error, setError] = useState('')
  const [pending, setPending] = useState(false)
  const [pickerOpen, setPickerOpen] = useState(false)

  const queueId = queue?.queueId ?? null
  const builtin = isBuiltin(queueId)
  const running = queue?.isRunning ?? true
  const isMain = queueId === MAIN_QUEUE_ID
  const title = queue === null ? t('createQueueAction') : queueLabel(t, queue)

  /** 成功返回 true；失败已弹 toast 并返回 false，调用方据此决定是否继续后续状态迁移。 */
  const run = async (action: () => Promise<unknown>): Promise<boolean> => {
    try {
      await action()
      return true
    } catch (err) {
      toastRpcError(err)
      return false
    }
  }

  const save = async () => {
    const trimmed = name.trim()
    if (!builtin && trimmed === '') {
      setError(t('queueNameRequired'))
      return
    }
    setError('')
    setPending(true)
    const fields = {
      name: builtin && queue ? queue.name : trimmed,
      speedLimitKbps: intOrZero(speedLimit),
      uploadLimitKbps: intOrZero(uploadLimit),
      maxConcurrent: intOrZero(maxConcurrent),
      defaultSaveDir: saveDir.trim(),
      defaultSegments: intOrZero(segments),
      defaultUserAgent: userAgent.trim(),
    }
    const schedule = { enabled: scheduleEnabled, startTime: formatTime(scheduleStart), stopTime: formatTime(scheduleStop), days }
    try {
      if (queue) {
        await rpc.daemon.queue.update({ queueId: queue.queueId, ...fields })
        if (queue.queueId !== MAIN_QUEUE_ID) await rpc.daemon.queue.schedule({ queueId: queue.queueId, ...schedule })
        onSaved(null)
      } else {
        await rpc.daemon.queue.create(fields)
        // create 不返回 id：按名称取最新创建的队列再补定时。
        let createdId: string | null = null
        if (scheduleEnabled || scheduleStart !== null || scheduleStop !== null) {
          const list = await rpc.daemon.queue.list()
          const created = list.filter((item) => item.name === fields.name).sort((a, b) => b.position - a.position)[0]
          if (created) {
            createdId = created.queueId
            await rpc.daemon.queue.schedule({ queueId: created.queueId, ...schedule })
          }
        }
        onSaved(createdId)
      }
    } catch (err) {
      toastRpcError(err)
    } finally {
      setPending(false)
    }
  }

  const remove = async () => {
    if (!queue) return
    const ok = await confirmDialog({
      title: t('deleteQueueAction'),
      description: t('queueDeleteConfirmDesc', { name: queue.name }),
      okLabel: t('deleteQueueAction'),
      intent: 'destructive',
    })
    if (!ok) return
    if (await run(() => rpc.daemon.queue.delete({ queueId: queue.queueId }))) onDeleted()
  }

  const weekdays = t('weekdaysShort').split(',')

  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div className="flex items-center gap-2">
        {onBack ? (
          <Button variant="ghost" iconOnly title={t('back')} aria-label={t('back')} onClick={onBack}>
            <Icon icon={ArrowLeft} />
          </Button>
        ) : null}
        <h3 className="min-w-0 flex-1 truncate text-title font-semibold">{title}</h3>
        {queue ? (
          <span className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
            <StatusDot running={running} />
            {running ? t('queueRunningBadge') : t('queueStoppedBadge')}
          </span>
        ) : null}
      </div>

      <Form onSubmit={() => void save()}>
        {builtin ? <FieldHint>{t('builtinQueueRenameHint')}</FieldHint> : (
          <FormField label={t('queueNameLabel')} htmlFor="queue-name" error={error}>
            <Input id="queue-name" value={name} onChange={(event) => setName(event.target.value)} invalid={error !== ''} spellCheck={false} />
          </FormField>
        )}
        <FormRow>
          <NumberField id="queue-speed" label={t('queueDownloadLimitWithUnit', { unit: speedUnit })} hint={t('queueSpeedLimitHint')} value={speedLimit} onChange={setSpeedLimit} />
          <NumberField id="queue-upload" label={t('queueUploadLimitWithUnit', { unit: speedUnit })} hint={t('queueUploadLimitDesc')} value={uploadLimit} onChange={setUploadLimit} />
        </FormRow>
        <div className="flex flex-col">
          <FormRow>
            <NumberField id="queue-concurrent" label={t('queueMaxConcurrent')} hint={t('queueMaxConcurrentHint')} value={maxConcurrent} onChange={setMaxConcurrent} />
            <NumberField id="queue-segments" label={t('queueDefaultSegments')} hint={t('queueDefaultSegmentsHint')} value={segments} onChange={setSegments} />
          </FormRow>
          <ThreadsRiskCallout segments={intOrZero(segments)} />
        </div>
        <FormField label={t('queueSaveDir')} htmlFor="queue-save-dir" hint={t('queueDirInheritHint')}>
          <InputWithAction
            input={<Input id="queue-save-dir" value={saveDir} onChange={(event) => setSaveDir(event.target.value)} spellCheck={false} />}
            action={<Button onClick={() => setPickerOpen(true)}>{t('browse')}</Button>}
          />
        </FormField>
        <FormField label={t('queueDefaultUserAgent')} htmlFor="queue-ua" hint={t('queueUaHint')}>
          <Input id="queue-ua" value={userAgent} onChange={(event) => setUserAgent(event.target.value)} spellCheck={false} />
        </FormField>

        {isMain ? (
          <FieldHint>{t('mainQueueAlwaysRunningHint')}</FieldHint>
        ) : (
        <OptionGroup>
          <OptionRow
            title={t('queueScheduleEnable')}
            description={t('queueScheduleDesc')}
            control={<Switch checked={scheduleEnabled} onCheckedChange={setScheduleEnabled} aria-label={t('queueScheduleEnable')} />}
          />
          {scheduleEnabled ? (
            <div className="flex flex-col gap-3 p-3">
              <FormRow>
                <TimeField label={t('queueScheduleStartLabel')} value={scheduleStart} onChange={setScheduleStart} />
                <TimeField label={t('queueScheduleStopLabel')} value={scheduleStop} onChange={setScheduleStop} />
              </FormRow>
              <FieldHint>{t('queueScheduleTimePickHint')}</FieldHint>
              <FormField label={t('queueScheduleDays')}>
                <div className="flex flex-wrap gap-x-4 gap-y-1">
                  {weekdays.map((label, index) => {
                    const bit = 1 << index
                    return (
                      <label key={label} className="flex min-h-control cursor-pointer items-center gap-2 text-sm coarse:min-h-touch">
                        <Checkbox checked={(days & bit) !== 0} onCheckedChange={(checked) => setDays(checked ? days | bit : days & ~bit)} />
                        {label}
                      </label>
                    )
                  })}
                </div>
              </FormField>
            </div>
          ) : null}
        </OptionGroup>
        )}

        {queue ? <PendingOrder queue={queue} /> : null}

        {error && builtin ? <FieldError>{error}</FieldError> : null}
        <button type="submit" className="hidden" tabIndex={-1} aria-hidden />
      </Form>

      <DialogFooter className="sticky bottom-0 z-10 -mx-4 flex-wrap justify-between border-t border-hairline bg-surface px-4 py-3">
        {queue && !builtin ? (
          <Button variant="ghost" className="text-destructive" onClick={() => void remove()}>
            {t('deleteQueueAction')}
          </Button>
        ) : (
          <span />
        )}
        <div className="flex flex-wrap items-center gap-2">
          {queue && !(isMain && running) ? (
            <Button
              variant="outline"
              onClick={() => void run(() => (running ? rpc.daemon.queue.stop({ queueId: queue.queueId }) : rpc.daemon.queue.start({ queueId: queue.queueId })))}
            >
              {running ? t('stopQueueAction') : t('startQueueAction')}
            </Button>
          ) : null}
          <Button variant="primary" loading={pending} onClick={() => void save()}>
            {t('queueSaveAction')}
          </Button>
        </div>
      </DialogFooter>
      <FsPickerDialog open={pickerOpen} onOpenChange={setPickerOpen} initialPath={saveDir} onSelect={setSaveDir} />
    </div>
  )
}

function NumberField({ id, label, hint, value, onChange }: { id: string; label: string; hint: string; value: string; onChange: (value: string) => void }) {
  return (
    <FormField label={label} htmlFor={id} hint={hint}>
      <Input id={id} inputMode="numeric" value={value} onChange={(event) => onChange(event.target.value.replace(/[^0-9]/g, ''))} />
    </FormField>
  )
}

/** 时间选择：「时」含「不定时」项，「分」按步长；未定时时「分」禁用（GPUI render_time_field）。 */
function TimeField({ label, value, onChange }: { label: string; value: number | null; onChange: (value: number | null) => void }) {
  const t = useT()
  const hour = value === null ? null : Math.floor(value / 60)
  const minute = value === null ? 0 : value % 60
  const hourOptions = useMemo(
    () => [{ value: 'off', label: t('queueScheduleTimeUnset') }, ...Array.from({ length: 24 }, (_, h) => ({ value: String(h), label: pad2(h) }))],
    [t],
  )
  const minuteOptions = useMemo(() => minuteChoices(minute).map((m) => ({ value: String(m), label: pad2(m) })), [minute])
  return (
    <FormField label={label}>
      <div className="flex items-center gap-1">
        <Select
          value={hour === null ? 'off' : String(hour)}
          options={hourOptions}
          aria-label={label}
          onValueChange={(next) => onChange(next === 'off' ? null : Number(next) * 60 + minute)}
        />
        <span aria-hidden>:</span>
        <Select
          value={String(minute)}
          options={minuteOptions}
          disabled={hour === null}
          aria-label={label}
          onValueChange={(next) => hour !== null && onChange(hour * 60 + Number(next))}
        />
      </div>
    </FormField>
  )
}

// ── 待处理任务顺序（daemon.queue.reorder） ──

function pendingIn(tasks: readonly TaskDto[], queueId: string): TaskDto[] {
  return tasks
    .filter((task) => (task.queueId || MAIN_QUEUE_ID) === queueId && task.status === TASK_STATUS.pending)
    .sort((a, b) => (a.queueOrder || Number.MAX_SAFE_INTEGER) - (b.queueOrder || Number.MAX_SAFE_INTEGER) || Number(a.createdAt) - Number(b.createdAt))
}

function PendingOrder({ queue }: { queue: QueueDto }) {
  const t = useT()
  const tasks = useTasks()
  const loading = useTasksLoading()
  const pending = useMemo(() => pendingIn(tasks, queue.queueId), [tasks, queue.queueId])
  const move = async (index: number, delta: -1 | 1) => {
    const order = pending.map((task) => task.taskId)
    const target = index + delta
    if (target < 0 || target >= order.length) return
    ;[order[index], order[target]] = [order[target], order[index]]
    try {
      await rpc.daemon.queue.reorder({ queueId: queue.queueId, taskIds: order })
    } catch (err) {
      toastRpcError(err)
    }
  }
  return (
    <div className="flex flex-col gap-1.5">
      <SectionHeader>{t('queueTabTasks')}</SectionHeader>
      <FieldHint>{t('queueTasksOrderHint')}</FieldHint>
      {loading ? (
        <FieldHint>{t('tasksLoadingTitle')}</FieldHint>
      ) : pending.length === 0 ? (
        <FieldHint>{t('queueNoPendingTasks')}</FieldHint>
      ) : (
        <ul className="rounded-md border border-hairline">
          {pending.map((task, index) => (
            <li key={task.taskId} className="flex min-h-control items-center gap-1 border-b border-hairline pl-3 last:border-b-0 coarse:min-h-touch">
              <span className="min-w-0 flex-1 truncate text-sm">{task.fileName || task.url}</span>
              <Button variant="ghost" iconOnly disabled={index === 0} title={t('webMoveUp')} aria-label={t('webMoveUp')} onClick={() => void move(index, -1)}>
                <Icon icon={ArrowUp} />
              </Button>
              <Button variant="ghost" iconOnly disabled={index === pending.length - 1} title={t('webMoveDown')} aria-label={t('webMoveDown')} onClick={() => void move(index, 1)}>
                <Icon icon={ArrowDown} />
              </Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
