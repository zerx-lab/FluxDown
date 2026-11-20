import { useByteFormatter } from '../../../lib/fileSizeUnits'
import type { CSSProperties } from 'react'
import { useT } from '../../../i18n'
import { formatBytes } from '../model/task'
import type { DownloadTaskView } from '../model/task'
import { composeSources, formatPercent } from '../model/sourceComposition'
import type { SourceKind } from '../model/sourceComposition'
import { Note } from './DetailRow'

/** 与 GPUI 常规页同色：多网卡取 info 色相 +60°（蓝→紫），与源站的主色蓝区分。 */
const COLOR: Record<SourceKind, { className?: string; style?: CSSProperties }> = {
  origin: { className: 'text-primary' },
  cdn: { className: 'text-success' },
  proxy: { className: 'text-warning' },
  nic: { style: { color: 'hsl(from var(--color-info) calc(h + 60) s l)' } },
  p2p: { className: 'text-primary' },
}
const LABEL_KEY = {
  origin: 'sourceOrigin',
  cdn: 'sourceCdn',
  proxy: 'sourceProxy',
  nic: 'sourceNic',
  p2p: 'sourceP2p',
} as const

const R = 42
const C = 2 * Math.PI * R

/** 常规页「来源构成」区块（环形图 + 图例 + 摘要）；尚无已下载字节时不渲染。 */
export function SourcesSection({ view }: { view: DownloadTaskView }) {
  const fmtBytes = useByteFormatter(formatBytes)
  const t = useT()
  const bytes = view.runtime?.sourceBytes ?? view.dto?.sourceBytes ?? null
  const comp = composeSources(view.protocol, view.downloadedBytes, bytes)
  if (comp.empty) return null

  let offset = 0
  const arcs = comp.slices
    .filter((s) => s.bytes > 0)
    .map((s) => {
      const len = s.fraction * C
      const arc = { kind: s.kind, len, offset }
      offset += len
      return arc
    })
  const summary = comp.p2p
    ? t('sourcesP2pHint')
    : comp.acceleratedShare > 0
      ? t('sourcesAccelShare', { percent: formatPercent(comp.acceleratedShare) })
      : t('sourcesNoAccel')

  return (
    <section className="flex w-[22.5rem] max-w-full shrink-0 flex-col gap-2">
      <div className="text-xs font-medium text-text-tertiary">{t('detailSourcesTitle')}</div>
      <div className="flex flex-wrap items-center gap-3">
        <div className="relative size-[8.75rem] shrink-0">
          <svg viewBox="0 0 100 100" className="size-full -rotate-90" role="img" aria-label={t('detailSourcesTitle')}>
            <circle cx={50} cy={50} r={R} fill="none" strokeWidth={12} className="stroke-progress-track" />
            {arcs.map((a) => (
              <circle
                key={a.kind}
                cx={50}
                cy={50}
                r={R}
                fill="none"
                strokeWidth={12}
                stroke="currentColor"
                className={COLOR[a.kind].className}
                style={COLOR[a.kind].style}
                strokeDasharray={`${a.len} ${C - a.len}`}
                strokeDashoffset={-a.offset}
              />
            ))}
          </svg>
          <div className="absolute inset-0 flex flex-col items-center justify-center">
            <div className="text-xs text-text-tertiary">{t('infoDownloaded')}</div>
            <div className="tabular text-sm font-medium text-foreground">{fmtBytes(comp.downloaded)}</div>
          </div>
        </div>
        <div className="flex min-w-[11.25rem] flex-1 flex-col">
          {comp.slices.map((s) => (
            <div key={s.kind} className="flex min-h-control items-center gap-2 text-xs">
              <span className={`size-2 shrink-0 rounded-full bg-current ${COLOR[s.kind].className ?? ''}`} style={COLOR[s.kind].style} />
              <span className="min-w-0 flex-1 truncate text-foreground">{t(LABEL_KEY[s.kind])}</span>
              <span className="tabular shrink-0 text-foreground">{formatPercent(s.fraction)}</span>
              <span className="tabular w-16 shrink-0 text-right text-text-tertiary">{fmtBytes(s.bytes)}</span>
            </div>
          ))}
        </div>
      </div>
      <Note>{summary}</Note>
    </section>
  )
}
