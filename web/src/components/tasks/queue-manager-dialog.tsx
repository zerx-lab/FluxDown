// 队列管理对话框 —— 对齐桌面端 lib/src/widgets/queue_manager_dialog.dart：
// 三个 Tab（设置 / 定时 / 任务）+ 即时启停按钮。
// - 设置：名称（内置队列锁定）/ 限速 KB/s / 最大同时下载 / 线程数量 / 默认目录 / 默认 UA；
// - 定时：每日定时启停（HH:MM，空 = 该边沿不定时）+ 生效星期位掩码 + 实时语义摘要；
// - 任务：队列内未完成任务的启动顺序，上移/下移立即持久化（reorderQueue）。
// 设置与定时经「确定」一次提交（updateQueue + setQueueSchedule）；启停即时生效。

import { useEffect, useState } from 'react'
import * as Dialog from '@radix-ui/react-dialog'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowDown, ArrowUp, CircleAlert, Info, Pause, Play, Settings2, X } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { useI18n, type I18nKey } from '../../lib/i18n'
import type { QueueDto } from '../../lib/types'
import { UA_PRESETS } from '../../lib/ua-presets'
import { SelectField } from '../dialogs/select-field'
import { FsPicker } from '../dialogs/fs-picker'
import { SetSwitch } from '../settings/controls'
import { useViewTasks, type ViewTask } from './useViewTasks'

const DAY_KEYS: I18nKey[] = ['sidebar.day0', 'sidebar.day1', 'sidebar.day2', 'sidebar.day3', 'sidebar.day4', 'sidebar.day5', 'sidebar.day6']
const SEGMENT_OPTIONS = ['0', '4', '8', '16', '32', '64']

/** 与引擎恢复顺序一致：queue_order 升序，0（未显式排序）在前按创建时间。 */
function compareQueueOrder(a: ViewTask, b: ViewTask): number {
  const ao = a.queueOrder ?? 0
  const bo = b.queueOrder ?? 0
  if (ao !== bo) return ao - bo
  const byTime = Number(a.createdAt) - Number(b.createdAt)
  return byTime !== 0 ? byTime : a.taskId.localeCompare(b.taskId)
}

export function QueueManagerDialog({ queue, queueName }: { queue: QueueDto; queueName: string }) {
  const { t } = useI18n()
  const qc = useQueryClient()
  const tasks = useViewTasks()
  const [open, setOpen] = useState(false)
  const [tab, setTab] = useState<0 | 1 | 2>(0)
  const builtin = queue.queueId === 'main' || queue.queueId === 'later'

  // ── 设置 ──
  const [name, setName] = useState(queue.name)
  const [speed, setSpeed] = useState('')
  const [concurrent, setConcurrent] = useState('')
  const [segments, setSegments] = useState('0')
  const [saveDir, setSaveDir] = useState('')
  const [ua, setUa] = useState('')

  // ── 定时 ──
  const [schedEnabled, setSchedEnabled] = useState(queue.scheduleEnabled)
  const [start, setStart] = useState(queue.scheduleStart)
  const [stop, setStop] = useState(queue.scheduleStop)
  const [days, setDays] = useState(queue.scheduleDays)
  const [schedError, setSchedError] = useState('')

  // 每次打开都从队列当前快照重置表单。
  useEffect(() => {
    if (!open) return
    setTab(0)
    setName(queue.name)
    setSpeed(queue.speedLimitKbps > 0 ? String(queue.speedLimitKbps) : '')
    setConcurrent(queue.maxConcurrent > 0 ? String(queue.maxConcurrent) : '')
    setSegments(queue.defaultSegments > 0 ? String(queue.defaultSegments) : '0')
    setSaveDir(queue.defaultSaveDir)
    setUa(queue.defaultUserAgent)
    setSchedEnabled(queue.scheduleEnabled)
    setStart(queue.scheduleStart)
    setStop(queue.scheduleStop)
    setDays((queue.scheduleDays & 0x7f) || 0x7f)
    setSchedError('')
  }, [open, queue])

  const invalidateQueues = () => void qc.invalidateQueries({ queryKey: ['queues'] })
  const toggleRun = useMutation({
    mutationFn: () => (queue.isRunning ? api.stopQueue(queue.queueId) : api.startQueue(queue.queueId)),
    onSuccess: invalidateQueues,
  })
  const save = useMutation({
    mutationFn: async () => {
      await api.updateQueue(queue.queueId, {
        // 内置队列名称固定（引擎侧同样拒绝改名），提交存量名即可。
        name: builtin ? queue.name : name.trim(),
        speedLimitKbps: Math.max(0, Number(speed.trim()) || 0),
        maxConcurrent: Math.min(100, Math.max(0, Number(concurrent.trim()) || 0)),
        defaultSaveDir: saveDir.trim(),
        defaultSegments: Number(segments) || 0,
        defaultUserAgent: ua.trim(),
      })
      await api.setQueueSchedule(queue.queueId, {
        enabled: schedEnabled,
        startTime: start.trim(),
        stopTime: stop.trim(),
        days: (days & 0x7f) || 0x7f,
      })
    },
    onSuccess: () => {
      invalidateQueues()
      setOpen(false)
    },
  })
  const reorder = useMutation({
    mutationFn: (taskIds: string[]) => api.reorderQueue(queue.queueId, taskIds),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ['tasks'] }),
  })

  function submit() {
    // 启用定时但两个时刻都空 = 无任何可执行动作，拦截并提示。
    if (schedEnabled && !start.trim() && !stop.trim()) {
      setSchedError(t('queue.scheduleNeedOneTime'))
      setTab(1)
      return
    }
    if (!builtin && !name.trim()) {
      setTab(0)
      return
    }
    save.mutate()
  }

  // ── 任务顺序 ──
  const pending = tasks.filter((tk) => tk.queueId === queue.queueId && tk.status !== 3).sort(compareQueueOrder)
  function moveTask(index: number, delta: number) {
    const target = index + delta
    if (target < 0 || target >= pending.length) return
    const ids = pending.map((tk) => tk.taskId)
    const [moved] = ids.splice(index, 1)
    ids.splice(target, 0, moved)
    reorder.mutate(ids)
  }

  const uaPresetOptions = [
    { value: '', label: t('queue.uaInherit') },
    ...UA_PRESETS.map((p) => ({ value: p.value, label: p.label })),
    { value: 'custom', label: t('common.custom') },
  ]
  const uaSelectValue = ua === '' ? '' : (UA_PRESETS.some((p) => p.value === ua) ? ua : 'custom')

  const startOk = schedEnabled && !!start.trim()
  const stopOk = schedEnabled && !!stop.trim()
  const summary =
    startOk && stopOk
      ? t('queue.scheduleSummaryBoth', { start, stop })
      : startOk
        ? t('queue.scheduleSummaryStartOnly', { start })
        : stopOk
          ? t('queue.scheduleSummaryStopOnly', { stop })
          : t('queue.scheduleNeedOneTime')
  const summaryWarn = schedEnabled && !startOk && !stopOk

  const TAB_KEYS: I18nKey[] = ['queue.tabSettings', 'queue.tabSchedule', 'queue.tabTasks']

  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button type="button" className="icon-btn sm" title={t('sidebar.queueManage')} onClick={(e) => e.stopPropagation()}>
          <Settings2 size={13} />
        </button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="wbackdrop show" />
        <Dialog.Content asChild onClick={(e) => e.stopPropagation()} onPointerDownOutside={(e) => e.preventDefault()}>
          <div className="dialog show" style={{ width: 480 }}>
            <header className="dlg-head">
              <Dialog.Title asChild>
                <b className="flex items-center gap-2">
                  {queueName}
                  <span className={cn('run-badge', queue.isRunning && 'on')}>
                    <i className={cn('queue-dot', queue.isRunning && 'on')} />
                    {queue.isRunning ? t('sidebar.queueRunning') : t('sidebar.queueStopped')}
                  </span>
                </b>
              </Dialog.Title>
              <Dialog.Close asChild>
                <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                  <X size={16} />
                </button>
              </Dialog.Close>
            </header>
            <Dialog.Description className="sr-only">{t('sidebar.queueManage')}</Dialog.Description>
            <div className="dlg-body">
              <div className="qtab-bar">
                {TAB_KEYS.map((key, i) => (
                  <button key={key} type="button" className={cn('qtab', tab === i && 'active')} onClick={() => setTab(i as 0 | 1 | 2)}>
                    {t(key)}
                  </button>
                ))}
              </div>

              {tab === 0 && (
                <>
                  <label className="field-label" htmlFor="qm-name">{t('queue.nameLabel')}</label>
                  <input
                    id="qm-name"
                    className="text-input"
                    value={name}
                    disabled={builtin}
                    placeholder={builtin ? queueName : t('queue.nameHint')}
                    onChange={(e) => setName(e.target.value)}
                  />
                  {builtin && <p className="field-hint">{t('queue.builtinRenameHint')}</p>}
                  <div className="grid3" style={{ marginTop: 10 }}>
                    <div>
                      <label className="field-label" htmlFor="qm-speed">{t('queue.speedLimit')}</label>
                      <input
                        id="qm-speed"
                        className="text-input"
                        inputMode="numeric"
                        value={speed}
                        placeholder={t('queue.speedLimitHint')}
                        onChange={(e) => setSpeed(e.target.value)}
                      />
                    </div>
                    <div>
                      <label className="field-label" htmlFor="qm-concurrent">{t('queue.maxConcurrent')}</label>
                      <input
                        id="qm-concurrent"
                        className="text-input"
                        inputMode="numeric"
                        value={concurrent}
                        placeholder={t('queue.maxConcurrentHint')}
                        onChange={(e) => setConcurrent(e.target.value)}
                      />
                    </div>
                    <div>
                      <label className="field-label">{t('queue.defaultSegments')}</label>
                      <SelectField
                        value={segments}
                        onChange={setSegments}
                        options={SEGMENT_OPTIONS.map((opt) => ({ value: opt, label: opt === '0' ? t('queue.segmentsAuto') : opt }))}
                        ariaLabel={t('queue.defaultSegments')}
                      />
                    </div>
                  </div>
                  <label className="field-label" htmlFor="qm-dir" style={{ marginTop: 10 }}>{t('queue.saveDir')}</label>
                  <div className="dir-row">
                    <input
                      id="qm-dir"
                      className="text-input"
                      spellCheck={false}
                      value={saveDir}
                      placeholder={t('queue.dirInheritHint')}
                      onChange={(e) => setSaveDir(e.target.value)}
                    />
                    <FsPicker value={saveDir} onChange={setSaveDir} />
                  </div>
                  <label className="field-label" style={{ marginTop: 10 }}>{t('queue.defaultUa')}</label>
                  <div className="dir-row">
                    <div style={{ width: 140, flexShrink: 0 }}>
                      <SelectField
                        value={uaSelectValue}
                        onChange={(v) => { if (v !== 'custom') setUa(v) }}
                        options={uaPresetOptions}
                        ariaLabel={t('queue.defaultUa')}
                      />
                    </div>
                    <input
                      className="text-input"
                      spellCheck={false}
                      value={ua}
                      placeholder={t('queue.uaHint')}
                      onChange={(e) => setUa(e.target.value)}
                    />
                  </div>
                </>
              )}

              {tab === 1 && (
                <>
                  <div className="set-row" style={{ padding: '4px 0' }}>
                    <div className="set-info">
                      <b>{t('sidebar.scheduleEnable')}</b>
                      <span>{t('queue.scheduleDesc')}</span>
                    </div>
                    <SetSwitch checked={schedEnabled} onCheckedChange={(v) => { setSchedEnabled(v); setSchedError('') }} />
                  </div>
                  <div className="time-row" style={{ marginTop: 10 }}>
                    <div>
                      <label className="field-label" htmlFor="qm-sched-start">{t('sidebar.scheduleStart')}</label>
                      <input
                        id="qm-sched-start"
                        className="text-input"
                        type="time"
                        value={start}
                        disabled={!schedEnabled}
                        onChange={(e) => { setStart(e.target.value); setSchedError('') }}
                      />
                    </div>
                    <div>
                      <label className="field-label" htmlFor="qm-sched-stop">{t('sidebar.scheduleStop')}</label>
                      <input
                        id="qm-sched-stop"
                        className="text-input"
                        type="time"
                        value={stop}
                        disabled={!schedEnabled}
                        onChange={(e) => { setStop(e.target.value); setSchedError('') }}
                      />
                    </div>
                  </div>
                  <label className="field-label" style={{ marginTop: 10 }}>{t('sidebar.scheduleDays')}</label>
                  <div className="day-row">
                    {DAY_KEYS.map((key, bit) => (
                      <button
                        key={key}
                        type="button"
                        className={cn('day-chip', (days & (1 << bit)) !== 0 && 'active')}
                        disabled={!schedEnabled}
                        onClick={() => setDays((d) => d ^ (1 << bit))}
                      >
                        {t(key)}
                      </button>
                    ))}
                  </div>
                  {schedEnabled && (
                    <p className={cn('sched-summary', summaryWarn && 'warn')}>
                      {summaryWarn ? <CircleAlert size={13} /> : <Info size={13} />}
                      <span>{summary}</span>
                    </p>
                  )}
                  {schedError && <p className="sched-summary warn"><CircleAlert size={13} /><span>{schedError}</span></p>}
                </>
              )}

              {tab === 2 &&
                (pending.length === 0 ? (
                  <p className="qorder-empty">{t('queue.noPendingTasks')}</p>
                ) : (
                  <>
                    <p className="field-hint" style={{ marginTop: 0 }}>{t('queue.tasksOrderHint')}</p>
                    <div className="qorder-list">
                      {pending.map((tk, i) => (
                        <div key={tk.taskId} className="qorder-row">
                          <em>{i + 1}</em>
                          <span className="qorder-name" title={tk.fileName}>{tk.fileName}</span>
                          <button type="button" className="icon-btn sm" disabled={i === 0 || reorder.isPending} onClick={() => moveTask(i, -1)}>
                            <ArrowUp size={13} />
                          </button>
                          <button
                            type="button"
                            className="icon-btn sm"
                            disabled={i === pending.length - 1 || reorder.isPending}
                            onClick={() => moveTask(i, 1)}
                          >
                            <ArrowDown size={13} />
                          </button>
                        </div>
                      ))}
                    </div>
                  </>
                ))}
            </div>
            <footer className="dlg-foot">
              <button type="button" className="btn ghost" disabled={toggleRun.isPending} onClick={() => toggleRun.mutate()}>
                {queue.isRunning ? <Pause size={13} /> : <Play size={13} />}
                {queue.isRunning ? t('sidebar.stopQueue') : t('sidebar.startQueue')}
              </button>
              <span className="flex1" />
              <Dialog.Close asChild>
                <button type="button" className="btn ghost">{t('common.cancel')}</button>
              </Dialog.Close>
              <button type="button" className="btn primary" disabled={save.isPending} onClick={submit}>
                {save.isPending ? t('common.loading') : t('common.confirm')}
              </button>
            </footer>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
