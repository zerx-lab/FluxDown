import { useState } from 'react'
import type { PointerEvent } from 'react'
import { useT } from '../../../i18n'
import { useFileSizeUnits, type FileSizeUnits } from '../../../lib/fileSizeUnits'
import { formatBytes, MAX_ETA_SECS } from '../model/task'
import type { DownloadTaskView } from '../model/task'
import { formatEta } from '../table/text'
import { DetailRow, Note } from './DetailRow'
import { clockText, niceCeil, pointElapsedSecs } from './speedChart'
import type { TaskSpeedHistoryDto } from '../../../lib/rpc'

const W = 400
const H = 120
/** 顶部留白，最高刻度线与描边不贴边。 */
const TOP = 4

const fmt = (v: number, units: FileSizeUnits) => `${formatBytes(v, units)}/s`

/**
 * 全程速度面积图：纵轴三刻度（0 / 一半 / 整值上限，标签画在图内保持满宽），
 * 指针悬停或触摸时显示该点的下载用时与速率。
 */
function SpeedChart({ history, label }: { history: TaskSpeedHistoryDto; label: string }) {
  const units = useFileSizeUnits()
  const t = useT()
  const [hover, setHover] = useState<number | null>(null)
  const samples = history.samples
  const n = samples.length
  const max = niceCeil(Math.max(history.peakBps, ...samples))
  const y = (v: number) => H - (Math.max(0, v) / max) * (H - TOP)
  const x = (i: number) => (i / (n - 1)) * W
  const line = samples.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(' ')
  const area = `0,${H} ${line} ${W},${H}`
  const elapsed = pointElapsedSecs(history.bucketSecs, history.activeSecs, n)
  const ticks = [max, max / 2, 0]

  const track = (e: PointerEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect()
    if (rect.width <= 0) return
    const frac = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width))
    setHover(Math.round(frac * (n - 1)))
  }

  const hx = hover === null ? 0 : (hover / (n - 1)) * 100
  const hy = hover === null ? 0 : (y(samples[hover] ?? 0) / H) * 100
  return (
    <div
      className="relative h-28 w-full touch-none select-none"
      onPointerMove={track}
      onPointerDown={track}
      onPointerLeave={() => setHover(null)}
    >
      <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className="absolute inset-0 h-full w-full text-primary" role="img" aria-label={label}>
        {ticks.map((v) => (
          <line key={v} x1={0} x2={W} y1={y(v)} y2={y(v)} className="text-hairline" stroke="currentColor" strokeDasharray="3 3" vectorEffect="non-scaling-stroke" />
        ))}
        <polygon points={area} fill="currentColor" fillOpacity={0.18} />
        <polyline points={line} fill="none" stroke="currentColor" strokeWidth={1.5} vectorEffect="non-scaling-stroke" />
      </svg>
      {ticks.map((v, i) => (
        <span
          key={v}
          className={`tabular pointer-events-none absolute left-1 text-[10px] leading-none text-text-tertiary ${i === ticks.length - 1 ? '-translate-y-full -mt-0.5' : 'mt-0.5'}`}
          style={{ top: `${(y(v) / H) * 100}%` }}
        >
          {fmt(v, units)}
        </span>
      ))}
      {hover !== null ? (
        <>
          <div className="pointer-events-none absolute inset-y-0 w-px bg-text-tertiary/50" style={{ left: `${hx}%` }} />
          <div
            className="pointer-events-none absolute size-2 -translate-x-1/2 -translate-y-1/2 rounded-full border border-surface bg-primary"
            style={{ left: `${hx}%`, top: `${hy}%` }}
          />
          <div
            className={`pointer-events-none absolute top-1 z-10 whitespace-nowrap rounded-md border border-hairline bg-surface px-2 py-1 text-xs shadow-md ${hx > 50 ? '-ml-2 -translate-x-full' : 'ml-2'}`}
            style={{ left: `${hx}%` }}
          >
            <div className="tabular text-text-tertiary">{clockText(elapsed[hover] ?? 0)}</div>
            <div className="flex items-center gap-1.5 text-foreground">
              <span className="size-1.5 rounded-full bg-primary" />
              <span>{t('infoSpeed')}</span>
              <span className="tabular ml-2 font-medium">{fmt(samples[hover] ?? 0, units)}</span>
            </div>
          </div>
        </>
      ) : null}
    </div>
  )
}

function Tile({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex min-w-0 flex-1 flex-col gap-0.5 rounded-md border border-hairline bg-chrome px-3 py-2">
      <div className="truncate text-xs text-text-tertiary">{label}</div>
      <div className="tabular truncate text-sm font-medium text-foreground">{value}</div>
    </div>
  )
}

export function SpeedTab({ view, history }: { view: DownloadTaskView; history: TaskSpeedHistoryDto | null }) {
  const units = useFileSizeUnits()
  const t = useT()
  const downloading = view.state === 'downloading'
  const current = downloading ? (view.speed ?? 0) : 0
  const samples = history?.samples ?? []
  const peak = history?.peakBps ?? 0
  const activeSecs = history?.activeSecs ?? 0
  const avg = history && activeSecs > 0 ? history.transferredBytes / activeSecs : 0
  // 少于 2 点或峰值为 0 时不画平线，给出说明。
  const hasChart = history !== null && samples.length >= 2 && peak > 0
  const duration = formatEta(t, activeSecs)

  const rt = view.runtime
  const limit = rt?.parallelismLimit
  return (
    <div className="flex flex-col gap-3">
      <div className="flex gap-2 mobile:flex-col">
        <Tile label={t('infoSpeed')} value={fmt(current, units)} />
        <Tile label={t('speedAverage')} value={fmt(avg, units)} />
        <Tile label={t('speedPeak')} value={fmt(peak, units)} />
        <Tile label={t('speedActiveTime')} value={duration} />
      </div>
      <div className="rounded-md border border-hairline bg-chrome p-2">
        {hasChart ? (
          <SpeedChart history={history} label={t('detailTabSpeed')} />
        ) : (
          <div className="flex h-28 items-center justify-center px-3 text-center">
            <Note>{t('speedChartEmpty')}</Note>
          </div>
        )}
        {hasChart ? (
          <div className="mt-1 flex justify-between text-xs text-text-tertiary">
            <span>{t('speedAxisStart')}</span>
            <span>{duration}</span>
          </div>
        ) : null}
      </div>
      <div className="flex flex-col">
        {downloading && view.etaSeconds !== null && view.etaSeconds <= MAX_ETA_SECS ? (
          <DetailRow label={t('infoRemaining')}>{formatEta(t, view.etaSeconds)}</DetailRow>
        ) : null}
        {downloading && rt && rt.activeTransfers !== null ? (
          <DetailRow label={t('taskActiveTransfers')}>
            {limit != null && limit > 0 ? `${rt.activeTransfers} / ${limit}` : String(rt.activeTransfers)}
          </DetailRow>
        ) : null}
        {view.protocol === 'bt' && rt && rt.connectedPeers !== null ? (
          <DetailRow label={t('taskConnectedPeers')}>{rt.connectedPeers}</DetailRow>
        ) : null}
      </div>
    </div>
  )
}
