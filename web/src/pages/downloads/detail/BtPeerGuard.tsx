// BT 详情 › 连接：防吸血列单元、右键处置与证据对话框。

import { useEffect, useState } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../../i18n'
import { evidenceQueries, peerFlagged } from '../../../lib/btGuard'
import type { PeerGuardView } from '../../../lib/btGuard'
import { rpc } from '../../../lib/rpc'
import type { BtGuardEntryDto, BtGuardEventDto } from '../../../lib/rpc'
import { errorMessage } from '../../../lib/rpc/error'
import { Badge, Button, Dialog, DialogFooter, Tooltip } from '../../../ui'
import type { BadgeTone } from '../../../ui'
import { EvidenceFacts } from '../../settings/sections/bt/guard/EvidenceFacts'
import { camel, optionalText } from '../../settings/kit'
import { formatBytes, formatDateTime } from '../model/task'
import type { PeerRow } from './btStatus'
import { actionName, peerActionText, ruleName } from './peerGuardActions'

const DASH = '—'
const SUSPICION_TONE: Record<PeerGuardView['suspicion'], BadgeTone> = { none: 'neutral', weak: 'neutral', medium: 'warning', strong: 'destructive' }
const EVIDENCE_EVENT_ROWS = 20

/** 悬停证据摘要：IP 组、累计送达、命中规则、处置。 */
function PeerTip({ view }: { view: PeerGuardView }) {
  const t = useT()
  const action = peerActionText(t, view)
  return (
    <div className="flex flex-col gap-0.5">
      <span className="font-mono break-all">
        {t('btGuardPeerGroup')}: {view.ipGroup}
      </span>
      <span>
        {t('btGuardPeerUploaded')}: {formatBytes(view.groupDeliveredBytes)}
      </span>
      <span>
        {t('btGuardPeerSuspicion')}: {t(`btGuardSuspicion${camel(view.suspicion)}`)}
      </span>
      {view.rules.length > 0 ? (
        <span>
          {t('btGuardPeerRules')}: {view.rules.map((rule) => ruleName(t, rule)).join(', ')}
        </span>
      ) : null}
      {action !== null ? (
        <span>
          {t('btGuardPeerAction')}: {action}
        </span>
      ) : null}
    </div>
  )
}

/** 带悬停摘要的包裹；没有 IP 组 / 无标记时原样返回。 */
function WithTip({ view, children }: { view: PeerGuardView; children: ReactNode }) {
  if (view.ipGroup === '' || !peerFlagged(view)) return <>{children}</>
  return (
    <Tooltip content={<PeerTip view={view} />}>
      <span className="min-w-0">{children}</span>
    </Tooltip>
  )
}

/** 嫌疑列：等级徽标 + 命中规则名。 */
export function SuspicionCell({ view }: { view: PeerGuardView }) {
  const t = useT()
  if (view.suspicion === 'none' && view.rules.length === 0) return <span className="text-xs text-text-tertiary">{DASH}</span>
  const rules = view.rules.map((rule) => ruleName(t, rule)).join(', ')
  return (
    <WithTip view={view}>
      <span className="flex min-w-0 items-center gap-1.5">
        {view.suspicion === 'none' ? null : <Badge tone={SUSPICION_TONE[view.suspicion]}>{t(`btGuardSuspicion${camel(view.suspicion)}`)}</Badge>}
        {rules === '' ? null : (
          <span title={rules} className="truncate text-xs text-muted-foreground">
            {rules}
          </span>
        )}
      </span>
    </WithTip>
  )
}

/** 处置列：放行为「—」；观察态弱化显示。 */
export function PeerActionCell({ view }: { view: PeerGuardView }) {
  const t = useT()
  const text = peerActionText(t, view)
  if (text === null) return <span className="text-xs text-text-tertiary">{DASH}</span>
  return (
    <WithTip view={view}>
      <span title={text} className={view.observeOnly ? 'truncate text-xs text-muted-foreground italic' : 'truncate text-xs text-destructive'}>
        {text}
      </span>
    </WithTip>
  )
}

/** 卡片 / 紧凑版式里的一行摘要：嫌疑、规则、处置；无标记返回 null。 */
export function PeerGuardSummary({ view, showAction = true }: { view: PeerGuardView; showAction?: boolean }) {
  if (!peerFlagged(view)) return null
  return (
    <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5">
      <SuspicionCell view={view} />
      {showAction ? <PeerActionCell view={view} /> : null}
    </span>
  )
}

interface EvidenceData {
  entries: BtGuardEntryDto[]
  events: BtGuardEventDto[]
}

/** 证据对话框：当前状态 + 该组的封禁条目 + 最近事件。 */
export function PeerEvidenceDialog({ row, onClose }: { row: PeerRow | null; onClose: () => void }) {
  const t = useT()
  const group = row?.guard.ipGroup ?? ''
  const [data, setData] = useState<{ group: string; value: EvidenceData | null; error: string | null }>({ group: '', value: null, error: null })

  useEffect(() => {
    if (group === '') return
    let alive = true
    const { entries: entriesQuery, events: eventsQuery } = evidenceQueries(group, EVIDENCE_EVENT_ROWS)
    Promise.all([rpc.daemon.bt.guard.entries.list(entriesQuery), rpc.daemon.bt.guard.events.list(eventsQuery)]).then(
      ([entries, events]) => {
        if (!alive) return
        setData({ group, error: null, value: { entries: entries.items, events: events.items } })
      },
      (failure: unknown) => {
        if (alive) setData({ group, value: null, error: errorMessage(failure) })
      },
    )
    return () => {
      alive = false
    }
  }, [group])

  const current = data.group === group ? data : { group, value: null, error: null }
  const view = row?.guard
  const empty = current.value !== null && current.value.entries.length === 0 && current.value.events.length === 0
  return (
    <Dialog
      open={row !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
      title={t('btGuardEvidenceTitle', { group })}
      size="lg"
      footer={
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('close')}
          </Button>
        </DialogFooter>
      }
    >
      {view ? (
        <div className="flex flex-col gap-4 pb-2">
          <section className="flex flex-col gap-1">
            <h3 className="text-caption font-medium text-text-tertiary">{t('btGuardEvidenceSummary')}</h3>
            <PeerTip view={view} />
          </section>
          {current.error !== null ? (
            <span role="alert" className="text-xs text-destructive">
              {t('btGuardEvidenceFailed')}: {current.error}
            </span>
          ) : current.value === null ? (
            <span className="text-xs text-muted-foreground">{t('btGuardLoading')}</span>
          ) : empty ? (
            <span className="text-xs text-muted-foreground">{t('btGuardEvidenceNone')}</span>
          ) : (
            <>
              {current.value.entries.length > 0 ? (
                <section className="flex flex-col gap-2">
                  <h3 className="text-caption font-medium text-text-tertiary">{t('btGuardEvidenceEntries')}</h3>
                  {current.value.entries.map((entry) => (
                    <div key={entry.id} className="flex flex-col gap-1 rounded-md border border-hairline p-2">
                      <div className="flex flex-wrap items-center gap-2 text-xs">
                        <span className="font-mono break-all text-foreground">{entry.cidr}</span>
                        <Badge>{t(`btGuardLayer${camel(entry.layer)}`)}</Badge>
                        <Badge tone={entry.action === 'allow' ? 'success' : entry.action === 'ban' ? 'destructive' : 'warning'}>{actionName(t, entry.action)}</Badge>
                        {entry.rule ? <span className="text-muted-foreground">{ruleName(t, entry.rule)}</span> : null}
                      </div>
                      {entry.reason ? (
                        <span title={entry.reason} className="truncate whitespace-pre text-xs text-muted-foreground">
                          {entry.reason}
                        </span>
                      ) : null}
                      <EvidenceFacts evidence={entry.evidence} />
                    </div>
                  ))}
                </section>
              ) : null}
              {current.value.events.length > 0 ? (
                <section className="flex flex-col gap-2">
                  <h3 className="text-caption font-medium text-text-tertiary">{t('btGuardEvidenceEvents')}</h3>
                  {current.value.events.map((event) => (
                    <div key={event.id} className="flex flex-col gap-1 rounded-md border border-hairline p-2">
                      <div className="flex flex-wrap items-center gap-2 text-xs">
                        <span className="tabular text-muted-foreground">{formatDateTime(event.at)}</span>
                        <Badge tone="accent">{optionalText(t, `btGuardEventKind${camel(event.kind)}`) ?? event.kind}</Badge>
                        {event.rule ? <span className="text-muted-foreground">{ruleName(t, event.rule)}</span> : null}
                      </div>
                      <EvidenceFacts evidence={event.detail} />
                    </div>
                  ))}
                </section>
              ) : null}
            </>
          )}
        </div>
      ) : null}
    </Dialog>
  )
}
