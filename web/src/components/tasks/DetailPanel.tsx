// 详情面板（340px，可关闭）：常规 / 分段 / 队列 / 日志 / 高级 五个 Tab。
// 对齐 design/web/app.js renderGeneral/renderSegments/renderQueue/renderLog/renderAdvanced。

import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Download, ListOrdered, Trash2, X, Zap } from 'lucide-react'
import { api, taskFileUrl } from '../../lib/api'
import { CopyButton } from '../CopyButton'
import { cn } from '../../lib/cn'
import { fmtBytes, fmtEta, fmtSpeed, fmtTime, protoLabel } from '../../lib/format'
import { segmentStore, splitStore, useStore } from '../../lib/ws'
import { confirmDialog } from '../../lib/confirm'
import type { QueueDto, SegmentDetail } from '../../lib/types'
import { eventLogStore } from './eventLog'
import { useTasksUi, type DetailTab } from './context'
import { useViewTasks, type ViewTask } from './useViewTasks'

const DTABS: { id: DetailTab; label: string }[] = [
  { id: 'general', label: '常规' },
  { id: 'segments', label: '分段' },
  { id: 'queue', label: '队列' },
  { id: 'log', label: '日志' },
  { id: 'advanced', label: '高级' },
]

const STATUS_TEXT: Record<number, string> = {
  0: '排队中',
  1: '下载中',
  2: '已暂停',
  3: '已完成',
  4: '错误',
  5: '正在准备',
}

export function DetailPanel() {
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
            <button type="button" className="icon-btn sm" title="收起面板" onClick={closeDetail}>
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
                {d.label}
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
  const [expanded, setExpanded] = useState(false)
  const clampable = value.length > CLAMP_THRESHOLD
  const text = (
    <p
      className={cn(clampable && 'expandable', clampable && !expanded && 'clamp')}
      title={clampable ? (expanded ? '点击收起' : '点击展开完整内容') : undefined}
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
  const qc = useQueryClient()
  const invalidate = () => qc.invalidateQueries({ queryKey: ['tasks'] })
  const boostMut = useMutation({ mutationFn: () => api.boostTask(t.taskId), onSuccess: invalidate })
  const deleteMut = useMutation({ mutationFn: (deleteFiles: boolean) => api.deleteTask(t.taskId, deleteFiles), onSuccess: invalidate })
  const seg = useStore(segmentStore)[t.taskId]
  const pct = t.totalBytes > 0 ? Math.round((t.downloadedBytes / t.totalBytes) * 100) : 0
  const queueName = queues.find((q) => q.queueId === t.queueId)?.name ?? '默认队列'

  return (
    <>
      <div className="d-progress">
        <div className="d-progress-num">
          <b>{pct}%</b>
          <span>
            {STATUS_TEXT[t.status] ?? '—'}
            {t.status === 1 ? ` · 剩余 ${fmtEta(t.totalBytes - t.downloadedBytes, t.speed)}` : ''}
          </span>
        </div>
        <div className="d-bar">
          <i style={{ width: `${pct}%` }} />
        </div>
      </div>
      <div className="d-stats">
        <div className="d-stat">
          <span>已下载</span>
          <b>{fmtBytes(t.downloadedBytes)}</b>
        </div>
        <div className="d-stat">
          <span>总大小</span>
          <b>{fmtBytes(t.totalBytes)}</b>
        </div>
        <div className="d-stat">
          <span>速度</span>
          <b className="accent">{t.speed > 0 ? fmtSpeed(t.speed) : '—'}</b>
        </div>
        <div className="d-stat">
          <span>线程</span>
          <b>{seg?.segmentCount ?? '—'}</b>
        </div>
      </div>
      <DField label="下载链接" value={t.url || '—'} copy />
      <DField label="保存位置（服务器）" value={`${t.saveDir}/${t.fileName}`} />
      <DField label="协议 / 队列" value={`${protoLabel(t.url)} · ${queueName}`} />
      <DField label="创建时间" value={fmtTime(t.createdAt)} />
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
            保存到本地
          </button>
        ) : (
          <button type="button" className="btn ghost sm" onClick={() => boostMut.mutate()}>
            <Zap size={15} />
            Boost 优先
          </button>
        )}
        <button
          type="button"
          className="btn danger sm"
          onClick={async () => {
            if (await confirmDialog({ title: '删除任务', message: '删除该任务？', danger: true })) deleteMut.mutate(false)
          }}
        >
          <Trash2 size={15} />
          删除
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
    return <p className="seg-note">任务已完成，分段信息已从 task_segments 表清理。</p>
  }
  if (!seg || seg.segments.length === 0) {
    return <p className="seg-note">暂无分段数据（等待引擎探测完成或分配分段）。</p>
  }

  const total = seg.totalBytes || t.totalBytes
  const cells = computeSegCells(seg.segments, total, t.status === 1)

  return (
    <>
      <div className="seg-summary">
        <span>
          共 <b>{seg.segmentCount}</b> 个分段 · segment_advisor 动态决定
        </span>
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
      <p className="seg-note">慢速分段由 segment_coordinator 主动拆分 / 抢救；拆分事件（SegmentSplit）经 WebSocket 实时推送并触发列表动画。</p>
    </>
  )
}

function QueueTab({ t, queues }: { t: ViewTask; queues: QueueDto[] }) {
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
        <span>当前：{current?.name ?? '默认队列'}</span>
      </div>
      <p className="q-move-label">移动到其它队列</p>
      {others.map((q) => (
        <button key={q.queueId} type="button" className="q-item" onClick={() => moveMut.mutate(q.queueId)}>
          <ListOrdered size={14} />
          <span>{q.name}</span>
          <em>
            {q.speedLimitKbps > 0 ? `${q.speedLimitKbps} KB/s` : '不限速'} · 并发 {q.maxConcurrent}
          </em>
        </button>
      ))}
      <p className="seg-note">每个命名队列拥有独立限速 / 并发 / 默认目录 / 默认线程配置。</p>
    </>
  )
}

function fmtClock(ms: number): string {
  const d = new Date(ms)
  const p = (n: number, l = 2) => String(n).padStart(l, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`
}

function LogTab({ t }: { t: ViewTask }) {
  const entries = useStore(eventLogStore).filter((e) => e.taskId === t.taskId)
  return (
    <>
      <div className="log-line">
        <span className="log-time">{fmtClock(Date.now())}</span>
        <span className="log-msg">当前状态：{STATUS_TEXT[t.status] ?? t.status}</span>
      </div>
      {entries.map((e) => (
        <div key={e.id} className="log-line">
          <span className="log-time">{fmtClock(e.at)}</span>
          <span className={cn('log-msg', e.isError && 'err')}>{e.message}</span>
        </div>
      ))}
      {entries.length === 0 && (
        <p className="seg-note">本面板仅记录本次会话内观测到的状态迁移与分段拆分事件；完整审计日志见服务器端 logs/ 目录。</p>
      )}
    </>
  )
}

function AdvancedTab({ t }: { t: ViewTask }) {
  return (
    <>
      <DField label="Checksum 校验" value={t.checksum || '未设置（下载完成后跳过校验）'} />
      <DField label="单任务代理" value={t.proxyUrl || '跟随全局（设置 → 代理）'} />
      <DField label="下载链接 URL" value={t.url} copy />
      <DField label="保存目录" value={t.saveDir} />
      <p className="seg-note">Checksum 格式 algo=hexhash（如 sha256=…），任务完成后由引擎在服务器端校验。</p>
    </>
  )
}
