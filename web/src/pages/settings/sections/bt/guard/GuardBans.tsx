// 设置 › BT › 封禁名单：分层 / 规则筛选、搜索、剩余时长、批量解除、手动添加、加白、导出。

import { Plus } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useT } from '../../../../../i18n'
import {
  BT_GUARD_LAYERS_FILTER,
  BT_GUARD_RULE_FILTER,
  DEFAULT_ENTRY_FILTER,
  canRemoveEntry,
  canWhitelistEntry,
  RemainingClock,
  chunkIds,
  countdownIntervalMs,
  entriesQuery,
  formatRemaining,
  removableIds,
} from '../../../../../lib/btGuard'
import type { EntryFilter } from '../../../../../lib/btGuard'
import { rpc } from '../../../../../lib/rpc'
import type { BtGuardEntryAction, BtGuardEntryDto, BtGuardLayer, BtGuardRule } from '../../../../../lib/rpc'
import { toastRpcError } from '../../../../../lib/rpcToast'
import { Badge, Button, Checkbox, Icon, Input, Select, Tooltip, toast } from '../../../../../ui'
import type { BadgeTone } from '../../../../../ui'
import { SettingsCustomRow, SettingsSection, camel, optionalText } from '../../../kit'
import { AddEntryDialog } from './AddEntryDialog'
import { EvidenceFacts } from './EvidenceFacts'
import { useGuardFeed, useNow } from './useGuard'

const SEARCH_DEBOUNCE_MS = 300

const ACTION_TONE: Record<BtGuardEntryAction, BadgeTone> = { allow: 'success', ban: 'destructive', choke: 'warning', throttle: 'warning' }

function actionLabel(t: (key: string) => string, action: string): string {
  return t(`btGuardAction${camel(action)}`)
}

function ruleLabel(t: (key: string) => string, rule: string): string {
  return optionalText(t, `btGuardRule${camel(rule)}`) ?? rule
}

function EntryRow({
  entry,
  remainingSecs,
  selected,
  disabled,
  onToggle,
  onRelease,
  onAllow,
}: {
  entry: BtGuardEntryDto
  remainingSecs: number | undefined
  selected: boolean
  disabled: boolean
  onToggle: () => void
  onRelease: () => void
  onAllow: () => void
}) {
  const t = useT()
  const removable = canRemoveEntry(entry)
  const note = [entry.reason, entry.source].filter((part): part is string => part !== undefined && part.trim() !== '').join(' · ')
  return (
    <li className="flex items-start gap-3 px-3 py-2">
      <Checkbox checked={selected} disabled={!removable} aria-label={entry.cidr} onCheckedChange={onToggle} className="mt-0.5" />
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <span className="min-w-0 break-all font-mono text-xs text-foreground">{entry.cidr}</span>
          <Badge>{t(`btGuardLayer${camel(entry.layer)}`)}</Badge>
          <Badge tone={ACTION_TONE[entry.action] ?? 'neutral'}>{actionLabel(t, entry.action)}</Badge>
          {entry.rule ? <Badge tone="accent">{ruleLabel(t, entry.rule)}</Badge> : null}
          <span className="tabular text-xs text-muted-foreground">{formatRemaining(t, remainingSecs)}</span>
        </div>
        {note !== '' ? (
          <span title={note} className="truncate whitespace-pre text-xs text-muted-foreground">
            {note}
          </span>
        ) : null}
        <EvidenceFacts evidence={entry.evidence} />
      </div>
      <div className="flex shrink-0 flex-wrap items-center justify-end gap-1">
        {canWhitelistEntry(entry) ? (
          <Button variant="ghost" disabled={disabled} onClick={onAllow}>
            {t('btGuardAllowEntry')}
          </Button>
        ) : null}
        {removable ? (
          <Button variant="ghost" disabled={disabled} className="text-muted-foreground hover:text-destructive" onClick={onRelease}>
            {t('btGuardRelease')}
          </Button>
        ) : (
          <Tooltip content={t('btGuardCloudEntryHint')}>
            <span>
              <Button variant="ghost" disabled aria-label={`${t('btGuardRelease')}: ${t('btGuardCloudEntryHint')}`}>
                {t('btGuardRelease')}
              </Button>
            </span>
          </Tooltip>
        )}
      </div>
    </li>
  )
}

function download(text: string, name: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: 'text/plain;charset=utf-8' }))
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = name
  anchor.setAttribute('data-fluxdown-skip', '1')
  document.body.append(anchor)
  anchor.click()
  anchor.remove()
  setTimeout(() => URL.revokeObjectURL(url), 10_000)
}

export function GuardBansTab() {
  const t = useT()
  const [filter, setFilter] = useState<EntryFilter>(DEFAULT_ENTRY_FILTER)
  const [queryDraft, setQueryDraft] = useState('')
  const [selected, setSelected] = useState<ReadonlySet<number>>(new Set())
  const [adding, setAdding] = useState(false)
  const [busy, setBusy] = useState(false)
  const busyRef = useRef(false)

  useEffect(() => {
    const timer = window.setTimeout(() => setFilter((prev) => (prev.query === queryDraft ? prev : { ...prev, query: queryDraft })), SEARCH_DEBOUNCE_MS)
    return () => window.clearTimeout(timer)
  }, [queryDraft])

  const key = `${filter.layer}|${filter.rule}|${filter.query.trim()}`
  const feed = useGuardFeed<BtGuardEntryDto>((offset, limit) => rpc.daemon.bt.guard.entries.list({ ...entriesQuery(filter, 0, limit), offset }), key)
  const { items } = feed
  const clock = useRef(new RemainingClock())
  clock.current.observe(items, Date.now())
  const remaining = items.map((item) => clock.current.secs(item, Date.now()))
  useNow(countdownIntervalMs(remaining))

  useEffect(() => {
    setSelected((prev) => {
      const present = new Set(items.map((item) => item.id))
      const next = new Set([...prev].filter((id) => present.has(id)))
      return next.size === prev.size ? prev : next
    })
  }, [items])

  const filtered = filter.layer !== 'all' || filter.rule !== 'all' || filter.query.trim() !== ''
  const ids = removableIds(items, selected)
  const selectable = items.filter(canRemoveEntry)
  const allSelected = selectable.length > 0 && selectable.every((item) => selected.has(item.id))

  const toggle = (id: number) =>
    setSelected((prev) => {
      const next = new Set(prev)
      if (!next.delete(id)) next.add(id)
      return next
    })

  /** 同一时刻只允许一个变更操作（ref 守卫，避免两次点击落在同一次渲染内）。 */
  const run = async (work: () => Promise<void>) => {
    if (busyRef.current) return
    busyRef.current = true
    setBusy(true)
    try {
      await work()
    } finally {
      busyRef.current = false
      setBusy(false)
    }
  }

  const release = (targets: readonly number[]) =>
    run(async () => {
      if (targets.length === 0) return
      let removed = 0
      try {
        for (const chunk of chunkIds(targets)) removed += (await rpc.daemon.bt.guard.entries.remove({ ids: chunk })).removed
        toast.key('btGuardReleased', 'success', { n: removed })
      } catch (failure) {
        if (removed > 0) toast.key('btGuardReleased', 'success', { n: removed })
        toastRpcError(failure)
      } finally {
        setSelected(new Set())
        feed.reload()
      }
    })

  const allow = (entry: BtGuardEntryDto) =>
    run(async () => {
      try {
        await rpc.daemon.bt.guard.entries.add({ cidr: entry.cidr, action: 'allow', reason: '' })
        toast.key('btGuardAllowed', 'success', { cidr: entry.cidr })
        feed.reload()
      } catch (failure) {
        toastRpcError(failure)
      }
    })

  const exportList = () =>
    run(async () => {
      try {
        const result = await rpc.daemon.bt.guard.export(filter.layer === 'all' ? {} : { layer: filter.layer })
        download(result.text, `fluxdown-bt-guard-${filter.layer}.txt`)
        toast.key('btGuardExportDone', 'success', { n: result.lines })
      } catch (failure) {
        toastRpcError(failure)
      }
    })

  const layerOptions = BT_GUARD_LAYERS_FILTER.map((value) => ({ value, label: t(`btGuardLayer${camel(value)}`) }))
  const ruleOptions = BT_GUARD_RULE_FILTER.map((value) => ({ value, label: value === 'all' ? t('btGuardRuleAll') : ruleLabel(t, value) }))

  return (
    <>
      <SettingsSection>
        <SettingsCustomRow className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <Select<BtGuardLayer | 'all'>
              value={filter.layer}
              aria-label={t('btGuardColLayer')}
              options={layerOptions}
              className="w-auto min-w-[9rem] mobile:flex-1"
              onValueChange={(layer) => setFilter((prev) => ({ ...prev, layer }))}
            />
            <Select<BtGuardRule | 'all'>
              value={filter.rule}
              aria-label={t('btGuardColRule')}
              options={ruleOptions}
              className="w-auto min-w-[9rem] mobile:flex-1"
              onValueChange={(rule) => setFilter((prev) => ({ ...prev, rule }))}
            />
            <Input
              value={queryDraft}
              aria-label={t('btGuardSearchPlaceholder')}
              placeholder={t('btGuardSearchPlaceholder')}
              autoComplete="off"
              spellCheck={false}
              className="min-w-[10rem] flex-1"
              onChange={(event) => setQueryDraft(event.target.value)}
            />
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <span className="tabular mr-auto text-xs text-muted-foreground">
              {t('btGuardEntriesTotal', { total: feed.total })}
              {ids.length > 0 ? ` · ${t('btGuardSelectedCount', { n: ids.length })}` : ''}
            </span>
            {ids.length > 0 ? (
              <Button variant="danger" disabled={busy} onClick={() => void release(ids)}>
                {t('btGuardReleaseSelected')}
              </Button>
            ) : null}
            <Button variant="outline" disabled={busy} onClick={() => void exportList()}>
              {t('btGuardExport')}
            </Button>
            <Button variant="primary" onClick={() => setAdding(true)}>
              <Icon icon={Plus} size="md" />
              {t('btGuardAddEntry')}
            </Button>
          </div>
        </SettingsCustomRow>
      </SettingsSection>

      <SettingsSection>
        {feed.error !== null && items.length === 0 ? (
          <SettingsCustomRow className="flex items-center justify-between gap-3">
            <span role="alert" className="min-w-0 flex-1 text-xs text-destructive">
              {t('btGuardLoadFailed')}: {feed.error}
            </span>
            <Button variant="outline" onClick={feed.reload}>
              {t('btGuardReload')}
            </Button>
          </SettingsCustomRow>
        ) : feed.loading ? (
          <SettingsCustomRow>
            <span className="text-xs text-muted-foreground">{t('btGuardLoading')}</span>
          </SettingsCustomRow>
        ) : items.length === 0 ? (
          <SettingsCustomRow>
            <span className="text-xs text-muted-foreground">{filtered ? t('btGuardEntriesFilterEmpty') : t('btGuardEntriesEmpty')}</span>
          </SettingsCustomRow>
        ) : (
          <>
            <SettingsCustomRow className="flex items-center gap-3 py-2">
              <Checkbox
                checked={allSelected}
                disabled={selectable.length === 0}
                aria-label={t('btGuardSelectedCount', { n: selectable.length })}
                onCheckedChange={(next) => setSelected(next ? new Set(selectable.map((item) => item.id)) : new Set())}
              />
              <span className="text-xs text-muted-foreground">{t('btGuardSelectedCount', { n: ids.length })}</span>
            </SettingsCustomRow>
            <ul className="flex flex-col divide-y divide-hairline">
              {items.map((entry, index) => (
                <EntryRow
                  key={entry.id}
                  entry={entry}
                  remainingSecs={remaining[index]}
                  disabled={busy}
                  selected={selected.has(entry.id)}
                  onToggle={() => toggle(entry.id)}
                  onRelease={() => void release([entry.id])}
                  onAllow={() => void allow(entry)}
                />
              ))}
            </ul>
            {feed.hasMore ? (
              <SettingsCustomRow className="flex justify-center">
                <Button variant="outline" loading={feed.loadingMore} onClick={feed.loadMore}>
                  {t('btGuardLoadMore', { shown: items.length, total: feed.total })}
                </Button>
              </SettingsCustomRow>
            ) : null}
          </>
        )}
      </SettingsSection>

      <AddEntryDialog open={adding} onOpenChange={setAdding} onAdded={feed.reload} />
    </>
  )
}
