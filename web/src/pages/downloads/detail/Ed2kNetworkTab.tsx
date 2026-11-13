import { Network, Server } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { Ed2kKadNodeTable } from '../../../lib/Ed2kKadNodeTable'
import { ed2kIdCheckFailedText, ed2kIdHintKey, ed2kIdKey, ed2kUpnpFailureReason, ed2kUpnpKey } from '../../../lib/ed2kNetwork'
import { rpc } from '../../../lib/rpc'
import type { Ed2kNetworkDto, Ed2kStatusDto } from '../../../lib/rpc'
import { rpcErrorText } from '../../../lib/rpcErrorText'
import { Button, ContextMenuArea, EmptyState, Select, Switch, toast } from '../../../ui'
import { SETTINGS_ERROR_KEYS, flushSettings, setDaemonBool, useDaemonBool, useSettingsError, useSettingsReadOnly } from '../../settings/kit'
import { canChangeSegments } from '../model/taskEdit'
import type { DownloadTaskView } from '../model/task'
import type { Translate } from '../table/text'
import { copyEntry, PollError, RowCopyButton, SkeletonRows, useMeasuredRoot, useNowSecs } from './btParts'
import { lastOkText, nextAnnounceText } from './btStatus'
import { CopyButton } from './CopyButton'
import { DetailRow, Note } from './DetailRow'
import {
  isFuture,
  kadPhaseKey,
  kadView,
  mapEd2kServers,
  MAX_SOURCES_OPTIONS,
  normalizeMaxSources,
  phaseKey,
  serverStatusKey,
} from './ed2kStatus'
import type { Ed2kServerRow } from './ed2kStatus'
import { useEd2kStatus } from './useEd2kStatus'

const DASH = '—'

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-1">
      <h3 className="text-caption font-medium text-text-tertiary">{title}</h3>
      {children}
    </section>
  )
}

/** 全局开关（同步键经设置写入口）：乐观回显由写回泵覆盖层负责，失败自动回滚；写回后立即重拉状态。 */
function GlobalSwitch({ configKey, label, refresh, children }: { configKey: string; label: string; refresh: () => void; children?: ReactNode }) {
  const checked = useDaemonBool(configKey)
  const readOnly = useSettingsReadOnly()
  return (
    <span className="flex min-w-0 items-center gap-2">
      <Switch
        checked={checked}
        disabled={readOnly}
        aria-label={label}
        onCheckedChange={(next) => {
          setDaemonBool(configKey, next)
          void flushSettings().finally(refresh)
        }}
      />
      {children}
    </span>
  )
}

function NetworkStatus({ network, refresh, onChecked }: { network: Ed2kNetworkDto; refresh: () => void; onChecked: (net: Ed2kNetworkDto) => void }) {
  const t = useT()
  const [checking, setChecking] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)
  const settingsError = useSettingsError()
  const hint = ed2kIdHintKey(network.id)
  const upnpReason = ed2kUpnpFailureReason(network)

  const runCheck = async () => {
    setChecking(true)
    setFailure(null)
    try {
      const result = await rpc.daemon.ed2k.idCheck()
      onChecked(result)
      if (result.id === 'none') setFailure(ed2kIdCheckFailedText(t, result.error))
    } catch (err) {
      setFailure(ed2kIdCheckFailedText(t, rpcErrorText(err, t)))
    } finally {
      setChecking(false)
    }
  }

  return (
    <Section title={t('ed2kNetStatus')}>
      <DetailRow label={t('ed2kNetId')}>
        <div className="flex flex-col gap-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className={cn('font-medium', network.id === 'high' ? 'text-success' : network.id === 'low' ? 'text-warning' : 'text-muted-foreground')}>
              {t(ed2kIdKey(network.id))}
            </span>
            <Button variant="outline" loading={checking} disabled={checking} onClick={() => void runCheck()}>
              {t(checking ? 'ed2kIdCheckRunning' : 'ed2kIdCheckRun')}
            </Button>
          </div>
          {hint === null ? null : <Note>{t(hint)}</Note>}
          {failure === null ? null : (
            <Note tone="destructive" className="break-words">
              <span role="alert">{failure}</span>
            </Note>
          )}
        </div>
      </DetailRow>
      {network.clientId ? <DetailRow label={t('ed2kNetClientId')}>{String(network.clientId)}</DetailRow> : null}
      <DetailRow label={t('ed2kNetServer')}>
        <span title={network.server} className="block truncate font-mono text-xs">
          {network.server || DASH}
        </span>
      </DetailRow>
      <DetailRow label={t('ed2kNetListenPort')}>{network.listenPort ? String(network.listenPort) : DASH}</DetailRow>
      {network.externalIp ? (
        <DetailRow label={t('ed2kNetExternalIp')}>
          <span title={network.externalIp} className="block truncate font-mono text-xs">
            {network.externalIp}
          </span>
        </DetailRow>
      ) : null}
      <DetailRow label={t('ed2kNetUpnp')}>
        <GlobalSwitch configKey="ed2k_enable_upnp" label={t('ed2kNetUpnp')} refresh={refresh}>
          <span className="truncate text-xs text-muted-foreground">{t(ed2kUpnpKey(network.upnp))}</span>
        </GlobalSwitch>
        {upnpReason === null ? null : (
          <span className="mt-0.5 block truncate text-[11px] text-muted-foreground/80" title={upnpReason}>
            {upnpReason}
          </span>
        )}
      </DetailRow>
      <DetailRow label={t('ed2kNetKad')}>
        <GlobalSwitch configKey="ed2k_enable_kad" label={t('ed2kNetKad')} refresh={refresh} />
      </DetailRow>
      {settingsError === null ? null : (
        <Note tone="destructive">
          <span role="alert" title={settingsError.detail}>
            {t(SETTINGS_ERROR_KEYS[settingsError.kind])}
          </span>
        </Note>
      )}
      <Note>{t('ed2kNetGlobalHint')}</Note>
    </Section>
  )
}

function MaxSourcesControl({ taskId, status, refresh }: { taskId: string; status: Ed2kStatusDto; refresh: () => void }) {
  const t = useT()
  const current = normalizeMaxSources(status.segments)
  const [picked, setPicked] = useState<number | null>(null)
  // 状态追上本次写入后撤掉乐观值。
  useEffect(() => {
    if (picked !== null && current === picked) setPicked(null)
  }, [current, picked])
  const value = picked ?? current
  const options = MAX_SOURCES_OPTIONS.map((n) => ({ value: String(n), label: n === 0 ? t('ed2kNetMaxSourcesAuto') : String(n) }))

  const change = async (next: number) => {
    setPicked(next)
    try {
      await rpc.daemon.task.setSegments({ taskId, segments: next })
      refresh()
    } catch (err) {
      setPicked(null)
      toast.error(rpcErrorText(err, t))
    }
  }

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-2">
        <Select value={String(value)} options={options} onValueChange={(v) => void change(Number(v))} aria-label={t('ed2kNetMaxSources')} className="w-28" />
        {value === 0 && status.maxSources > 0 ? <span className="tabular text-xs text-muted-foreground">{status.maxSources}</span> : null}
      </div>
      <Note>{t('ed2kNetMaxSourcesDesc')}</Note>
    </div>
  )
}

function TaskSection({ view, status, now, refresh }: { view: DownloadTaskView; status: Ed2kStatusDto; now: number; refresh: () => void }) {
  const t = useT()
  const editable = canChangeSegments(view)
  return (
    <Section title={t('ed2kNetTask')}>
      <DetailRow label={t('ed2kNetPhase')}>{t(phaseKey(status.phase))}</DetailRow>
      {isFuture(status.nextSearchAt, now) ? <DetailRow label={t('ed2kNetNextSearch')}>{nextAnnounceText(t, status.nextSearchAt, now)}</DetailRow> : null}
      <DetailRow label={t('ed2kNetParts')}>{`${status.partsDone} / ${status.partsTotal}`}</DetailRow>
      <DetailRow label={t('ed2kNetFileHash')}>
        {status.fileHash === '' ? (
          DASH
        ) : (
          <div className="flex min-w-0 items-center gap-1">
            <span title={status.fileHash} className="min-w-0 truncate font-mono text-xs">
              {status.fileHash}
            </span>
            <CopyButton text={status.fileHash} label={t('webCopy')} copiedLabel={t('fileHashCopied')} />
          </div>
        )}
      </DetailRow>
      {editable ? (
        <DetailRow label={t('ed2kNetMaxSources')}>
          <MaxSourcesControl taskId={view.taskId} status={status} refresh={refresh} />
        </DetailRow>
      ) : null}
    </Section>
  )
}

const SERVER_GRID = 'grid-cols-[minmax(0,1.4fr)_minmax(6rem,1fr)_3.5rem_6.5rem]'
const SERVER_TEXT: Record<Ed2kServerRow['status'], string> = {
  querying: 'text-muted-foreground',
  working: 'text-success',
  notWorking: 'text-destructive',
}

function ServerRow({ row, t, now }: { row: Ed2kServerRow; t: Translate; now: number }) {
  return (
    <ContextMenuArea entries={[copyEntry(t('webCopy'), row.addr, null, t)]} title={row.addr} className="group/row border-b border-hairline hover:bg-row-hover">
      <div className={cn('grid items-center gap-x-3 px-1 py-1.5', SERVER_GRID)}>
        <div className="flex min-w-0 items-center gap-1.5">
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 items-center gap-1.5">
              <span title={row.addr} className="min-w-0 truncate font-mono text-xs text-foreground">
                {row.addr}
              </span>
              {row.connected ? <span className="shrink-0 rounded-sm bg-muted px-1.5 py-px text-caption text-success">{t('ed2kServerConnected')}</span> : null}
            </div>
            <span className="block truncate text-caption text-text-tertiary">{t(row.source === 'manual' ? 'ed2kServerSourceManual' : 'ed2kServerSourceSubscription')}</span>
          </div>
          <RowCopyButton label={t('webCopy')} value={row.addr} doneKey={null} />
        </div>
        <span className="flex min-w-0 flex-col">
          <span title={row.message || undefined} className={cn('truncate text-xs font-medium', SERVER_TEXT[row.status])}>
            {t(serverStatusKey(row.status))}
          </span>
          {row.message === '' ? null : (
            <span title={row.message} className="truncate text-caption text-text-tertiary">
              {row.message}
            </span>
          )}
        </span>
        <span className="tabular text-right text-xs text-muted-foreground">{row.sources === null ? DASH : String(row.sources)}</span>
        <span className="tabular truncate text-xs text-muted-foreground">{row.lastQueryAt > 0 ? lastOkText(t, row.lastQueryAt, now) : DASH}</span>
      </div>
    </ContextMenuArea>
  )
}

function ServersSection({ status, now }: { status: Ed2kStatusDto; now: number }) {
  const t = useT()
  const rows = mapEd2kServers(status.servers)
  return (
    <Section title={t('ed2kNetServers')}>
      <div className="tabular text-xs text-muted-foreground">{t('ed2kServersSummary', { queried: status.servers.length, total: status.network.serverCount })}</div>
      {rows.length === 0 ? (
        <EmptyState icon={Server} title={t('ed2kServersEmpty')} className="py-4" />
      ) : (
        <div className="flex flex-col overflow-x-auto">
          <div className={cn('grid gap-x-3 border-b border-hairline px-1 pb-1 text-xs text-text-tertiary', SERVER_GRID)}>
            <span className="truncate">{t('ed2kServerAddr')}</span>
            <span className="truncate">{t('ed2kServerStatus')}</span>
            <span className="truncate text-right">{t('ed2kServerSources')}</span>
            <span className="truncate">{t('ed2kServerLastQuery')}</span>
          </div>
          {rows.map((row) => (
            <ServerRow key={row.key} row={row} t={t} now={now} />
          ))}
        </div>
      )}
    </Section>
  )
}

function KadSection({ status, narrow }: { status: Ed2kStatusDto; narrow: boolean }) {
  const t = useT()
  const mode = kadView(status.network)
  const kad = status.kad
  return (
    <Section title={t('ed2kNetKadNodes')}>
      {mode === 'off' ? (
        <Note>{t('ed2kKadOff')}</Note>
      ) : mode === 'noBootstrap' ? (
        <Note tone="warning">{t('ed2kKadNoBootstrap')}</Note>
      ) : (
        <>
          <div className="tabular text-xs text-muted-foreground">
            {t('ed2kKadSummary', {
              phase: t(kadPhaseKey(kad.phase)),
              bootstrap: kad.bootstrapNodes,
              responded: kad.responded,
              discovered: kad.discovered,
              sources: kad.sources,
            })}
          </div>
          {kad.nodes.length === 0 ? (
            <EmptyState icon={Network} title={t('ed2kKadNodesEmpty')} className="py-4" />
          ) : (
            <Ed2kKadNodeTable nodes={kad.nodes} narrow={narrow} />
          )}
        </>
      )}
    </Section>
  )
}

export function Ed2kNetworkTab({ view }: { view: DownloadTaskView }) {
  const t = useT()
  const { data, error, refresh } = useEd2kStatus(view.taskId)
  const now = useNowSecs()
  const { ref, width } = useMeasuredRoot()
  // 「检测」结果覆盖本段显示，直到下一次轮询结果到达。
  const [checked, setChecked] = useState<{ base: Ed2kStatusDto; network: Ed2kNetworkDto } | null>(null)
  const latest = useRef(data)
  latest.current = data
  const network = data !== null && checked !== null && checked.base === data ? checked.network : data?.network

  return (
    <div ref={ref} className="flex flex-col gap-4">
      <PollError message={error === null ? null : `${t('ed2kStatusLoadFailed')}: ${error}`} />
      {data === null || network === undefined ? (
        error === null ? <SkeletonRows /> : null
      ) : (
        <>
          <NetworkStatus network={network} refresh={refresh} onChecked={(net) => latest.current && setChecked({ base: latest.current, network: net })} />
          <TaskSection view={view} status={data} now={now} refresh={refresh} />
          <ServersSection status={data} now={now} />
          <KadSection status={data} narrow={width > 0 && width < 480} />
        </>
      )}
    </div>
  )
}
