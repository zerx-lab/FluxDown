// 详情面板（340px，可关闭）：常规 / 分段 / 队列 / 日志 / 高级 五个 Tab。
// 对齐 design/web/app.js renderGeneral/renderSegments/renderQueue/renderLog/renderAdvanced。

import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Download, ListOrdered, Trash2, X, Zap } from 'lucide-react'
import { api, taskFileUrl } from '../../lib/api'
import { CopyButton } from '../CopyButton'
import { cn } from '../../lib/cn'
import { fmtBytes, fmtEta, fmtSpeed, fmtTime, protoLabel } from '../../lib/format'
import { t as i18nT, translateBackendMessage, useI18n } from '../../lib/i18n'
import { segmentStore, splitStore, useStore } from '../../lib/ws'
import { confirmDialog } from '../../lib/confirm'
import type { QueueDto, SegmentDetail, TaskStatus } from '../../lib/types'
import { eventLogStore } from './eventLog'
import { useTasksUi, type DetailTab } from './context'
import { useViewTasks, type ViewTask } from './useViewTasks'

const DTABS: { id: DetailTab; labelKey: 'detail.tabGeneral' | 'detail.tabSegments' | 'detail.tabQueue' | 'detail.tabLog' | 'detail.tabAdvanced' }[] = [
  { id: 'general', labelKey: 'detail.tabGeneral' },
  { id: 'segments', labelKey: 'detail.tabSegments' },
  { id: 'queue', labelKey: 'detail.tabQueue' },
  { id: 'log', labelKey: 'detail.tabLog' },
  { id: 'advanced', labelKey: 'detail.tabAdvanced' },
]

/** 状态文案：函数而非模块级常量，避免固化在某一语言（随当前语言实时取值）。 */
function statusText(s: TaskStatus): string {
  const KEYS = {
    0: 'status.pending',
    1: 'status.downloading',
    2: 'status.paused',
    3: 'status.completed',
    4: 'status.error',
    5: 'status.preparing',
  } as const
  return i18nT(KEYS[s])
}

export function DetailPanel() {
  const { t } = useI18n()
  const { currentTaskId, detailOpen, detailTab, setDetailTab, closeDetail } = useTasksUi()
  const tasks = useViewTasks()
  const { data: queues = [] } = useQuery({ queryKey: ['queues'], queryFn: api.listQueues })
  const task = tasks.find((t) => t.taskId === currentTaskId)
  const open = detailOpen && !!task

  return (
    <aside className={cn('detail', !open && 'hidden')}>
      {task && (
        <>
          <header className="detail-head">
            <b className="ellip">{task.fileName || task.url}</b>
            <button type="button" className="icon-btn sm" title={t('detail.collapse')} onClick={closeDetail}>
              <X size={14} />
            </button>
          </header>
          <div className="detail-tabs">
            {DTABS.map((d) => (
              <button
                key={d.id}
                type="button"
                className={cn('dtab', detailTab === d.id && 'active')}
                onClick={() => setDetailTab(d.id)}
              >
                {t(d.labelKey)}
              </button>
            ))}
          </div>
          <div className="detail-body">
            {detailTab === 'general' && <GeneralTab t={task} queues={queues} />}
            {detailTab === 'segments' && <SegmentsTab t={task} />}
            {detailTab === 'queue' && <QueueTab t={task} queues={queues} />}
            {detailTab === 'log' && <LogTab t={task} />}
            {detailTab === 'advanced' && <AdvancedTab t={task} />}
          </div>
        </>
      )}
    </aside>
  )
}

/** 超过该长度的值默认折叠为 3 行，点击展开/收起（完整值始终可通过复制按钮获取）。 */
const CLAMP_THRESHOLD = 120

function DField({ label, value, copy }: { label: string; value: string; copy?: boolean }) {
  const { t } = useI18n()
  const [expanded, setExpanded] = useState(false)
  const clampable = value.length > CLAMP_THRESHOLD
  const text = (
    <p
      className={cn(clampable && 'expandable', clampable && !expanded && 'clamp')}
      title={clampable ? (expanded ? t('detail.collapseValue') : t('detail.expand')) : undefined}
      onClick={clampable ? () => setExpanded((v) => !v) : undefined}
    >
      {value}
    </p>
  )
  if (!copy) {
    return (
      <div className="d-field">
        <span>{label}</span>
        {text}
      </div>
    )
  }
  return (
    <div className="d-field">
      <span>{label}</span>
      <div className="copy-row">
        {text}
        <CopyButton value={value} />
      </div>
    </div>
  )
}

function GeneralTab({ t, queues }: { t: ViewTask; queues: QueueDto[] }) {
  const { t: tr } = useI18n()
  const qc = useQueryClient()
  const invalidate = () => qc.invalidateQueries({ queryKey: ['tasks'] })
  const boostMut = useMutation({ mutationFn: () => api.boostTask(t.taskId), onSuccess: invalidate })
  const deleteMut = useMutation({ mutationFn: (deleteFiles: boolean) => api.deleteTask(t.taskId, deleteFiles), onSuccess: invalidate })
  const seg = useStore(segmentStore)[t.taskId]
  const pct = t.totalBytes > 0 ? Math.round((t.downloadedBytes / t.totalBytes) * 100) : 0
  const queueName = queues.find((q) => q.queueId === t.queueId)?.name ?? tr('detail.defaultQueue')

  return (
    <>
      <div className="d-progress">
        <div className="d-progress-num">
          <b>{pct}%</b>
          <span>
            {statusText(t.status)}
            {t.status === 1 ? ` · ${tr('detail.remaining', { eta: fmtEta(t.totalBytes - t.downloadedBytes, t.speed) })}` : ''}
          </span>
        </div>
        <div className="d-bar">
          <i style={{ width: `${pct}%` }} />
        </div>
      </div>
      <div className="d-stats">
        <div className="d-stat">
          <span>{tr('detail.downloaded')}</span>
          <b>{fmtBytes(t.downloadedBytes)}</b>
        </div>
        <div className="d-stat">
          <span>{tr('detail.totalSize')}</span>
          <b>{fmtBytes(t.totalBytes)}</b>
        </div>
        <div className="d-stat">
          <span>{tr('detail.speed')}</span>
          <b className="accent">{t.speed > 0 ? fmtSpeed(t.speed) : '—'}</b>
        </div>
        <div className="d-stat">
          <span>{tr('detail.threads')}</span>
          <b>{seg?.segmentCount ?? '—'}</b>
        </div>
      </div>
      <DField label={tr('detail.url')} value={t.url || '—'} copy />
      <DField label={tr('detail.savePath')} value={`${t.saveDir}/${t.fileName}`} />
      <DField label={tr('detail.protoQueue')} value={`${protoLabel(t.url)} · ${queueName}`} />
      <DField label={tr('detail.createdAt')} value={fmtTime(t.createdAt)} />
      {t.status === 4 && t.errorMessage ? (
        <DField label={tr('detail.error')} value={translateBackendMessage(t.errorMessage)} copy />
      ) : null}
      <div className="d-actions">
        {t.status === 3 ? (
          <button
            type="button"
            className="btn primary sm"
            onClick={() => {
              location.href = taskFileUrl(t.taskId)
            }}
          >
            <Download size={15} />
            {tr('task.saveToLocal')}
          </button>
        ) : (
          <button type="button" className="btn ghost sm" onClick={() => boostMut.mutate()}>
            <Zap size={15} />
            {tr('task.boost')}
          </button>
        )}
        <button
          type="button"
          className="btn danger sm"
          onClick={async () => {
            if (await confirmDialog({ title: tr('task.deleteTitle'), message: tr('task.deleteMsg'), danger: true })) deleteMut.mutate(false)
          }}
        >
          <Trash2 size={15} />
          {tr('task.delete')}
        </button>
      </div>
    </>
  )
}

/** 100 格分段图：按各分段的字节范围与已下载量填充 done/active，不做任何合成/随机数据。 */
function computeSegCells(segments: SegmentDetail[], totalBytes: number, active: boolean): string[] {
  const N = 100
  const total = totalBytes || 1
  const cells = new Array<string>(N).fill('')
  for (const s of segments) {
    const doneUpTo = s.startByte + s.downloadedBytes
    const startCell = Math.max(0, Math.floor((s.startByte / total) * N))
    const endCell = Math.min(N, Math.ceil((s.endByte / total) * N))
    const doneCell = Math.floor((doneUpTo / total) * N)
    for (let i = startCell; i < endCell; i++) {
      if (i < doneCell) cells[i] = 'done'
      else if (i === doneCell && active) cells[i] = 'active'
    }
  }
  return cells
}

function SegmentsTab({ t }: { t: ViewTask }) {
  const { t: tr } = useI18n()
  const seg = useStore(segmentStore)[t.taskId]
  const split = useStore(splitStore)
  const [, setTick] = useState(0)

  const recentSplit = split && split.taskId === t.taskId && Date.now() - split.at < 3000 ? split : null

  useEffect(() => {
    if (!recentSplit) return
    const remaining = Math.max(0, 3000 - (Date.now() - recentSplit.at))
    const timer = setTimeout(() => setTick((n) => n + 1), remaining)
    return () => clearTimeout(timer)
  }, [recentSplit])

  if (t.status === 3) {
    return <p className="seg-note">{tr('detail.segCleared')}</p>
  }
  if (!seg || seg.segments.length === 0) {
    return <p className="seg-note">{tr('detail.noSegmentsHint')}</p>
  }

  const total = seg.totalBytes || t.totalBytes
  const cells = computeSegCells(seg.segments, total, t.status === 1)

  return (
    <>
      <div className="seg-summary">
        <span>{tr('detail.segAdvisorSummary', { n: seg.segmentCount })}</span>
        <span>{protoLabel(t.url)}</span>
      </div>
      <div className="seg-map">
        {cells.map((c, i) => (
          <i key={`cell-${i}`} className={cn('seg-cell', c)} />
        ))}
      </div>
      {seg.segments.map((s) => {
        const segPct = Math.round((s.downloadedBytes / Math.max(1, s.endByte - s.startByte)) * 100)
        const isSplit = !!recentSplit && (recentSplit.parentIndex === s.index || recentSplit.childIndex === s.index)
        return (
          <div key={s.index} className={cn('seg-row', isSplit && 'split')}>
            <span className="seg-idx">#{s.index + 1}</span>
            <span className="seg-range">
              {fmtBytes(s.startByte)} – {fmtBytes(s.endByte)}
            </span>
            <span className="seg-pct">{Math.max(0, Math.min(100, segPct))}%</span>
          </div>
        )
      })}
      <p className="seg-note">{tr('detail.segFooterNote')}</p>
    </>
  )
}

function QueueTab({ t, queues }: { t: ViewTask; queues: QueueDto[] }) {
  const { t: tr } = useI18n()
  const qc = useQueryClient()
  const moveMut = useMutation({
    mutationFn: (queueId: string) => api.moveTaskToQueue(t.taskId, queueId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['tasks'] }),
  })
  const current = queues.find((q) => q.queueId === t.queueId)
  const others = queues.filter((q) => q.queueId !== t.queueId)

  return (
    <>
      <div className="q-current">
        <ListOrdered size={15} />
        <span>{tr('detail.currentQueueValue', { name: current?.name ?? tr('detail.defaultQueue') })}</span>
      </div>
      <p className="q-move-label">{tr('detail.moveToOther')}</p>
      {others.map((q) => (
        <button key={q.queueId} type="button" className="q-item" onClick={() => moveMut.mutate(q.queueId)}>
          <ListOrdered size={14} />
          <span>{q.name}</span>
          <em>
            {q.speedLimitKbps > 0 ? `${q.speedLimitKbps} KB/s` : tr('detail.noLimit')} · {tr('detail.concurrency', { n: q.maxConcurrent })}
          </em>
        </button>
      ))}
      <p className="seg-note">{tr('detail.queueFooterNote')}</p>
    </>
  )
}

function fmtClock(ms: number): string {
  const d = new Date(ms)
  const p = (n: number, l = 2) => String(n).padStart(l, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`
}

function LogTab({ t }: { t: ViewTask }) {
  const { t: tr } = useI18n()
  const entries = useStore(eventLogStore).filter((e) => e.taskId === t.taskId)
  return (
    <>
      <div className="log-line">
        <span className="log-time">{fmtClock(Date.now())}</span>
        <span className="log-msg">{tr('detail.currentStatus', { status: statusText(t.status) })}</span>
      </div>
      {entries.map((e) => (
        <div key={e.id} className="log-line">
          <span className="log-time">{fmtClock(e.at)}</span>
          <span className={cn('log-msg', e.isError && 'err')}>{e.message}</span>
        </div>
      ))}
      {entries.length === 0 && <p className="seg-note">{tr('detail.logEmptyNote')}</p>}
    </>
  )
}

function AdvancedTab({ t }: { t: ViewTask }) {
  const { t: tr } = useI18n()
  return (
    <>
      <DField label={tr('detail.checksum')} value={t.checksum || tr('detail.checksumNotSet')} />
      <DField label={tr('detail.proxy')} value={t.proxyUrl || tr('detail.proxyNotSet')} />
      <DField label={tr('detail.url')} value={t.url} copy />
      <DField label={tr('detail.savePath')} value={t.saveDir} />
      <p className="seg-note">{tr('detail.checksumFooterNote')}</p>
    </>
  )
}
