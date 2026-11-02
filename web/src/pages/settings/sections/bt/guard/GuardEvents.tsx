// 设置 › BT › 事件日志：按规则 / 类型筛选，分页追加。

import { useState } from 'react'
import { useT } from '../../../../../i18n'
import { BT_GUARD_EVENT_KIND_FILTER, BT_GUARD_RULE_FILTER, DEFAULT_EVENT_FILTER, eventsQuery, evidenceFacts } from '../../../../../lib/btGuard'
import type { EventFilter } from '../../../../../lib/btGuard'
import { rpc } from '../../../../../lib/rpc'
import type { BtGuardEventDto, BtGuardRule } from '../../../../../lib/rpc'
import { Badge, Button, Select } from '../../../../../ui'
import { formatBytes, formatDateTime } from '../../../../downloads/model/task'
import { SettingsCustomRow, SettingsSection, camel, optionalText } from '../../../kit'
import { EvidenceFacts } from './EvidenceFacts'
import { useGuardFeed } from './useGuard'

type Translate = (key: string) => string

/** 事件 kind → 文案；未知 kind 显示原文。 */
function kindLabel(t: Translate, kind: string): string {
  return optionalText(t, `btGuardEventKind${camel(kind)}`) ?? kind
}

function EventRow({ event }: { event: BtGuardEventDto }) {
  const t = useT()
  const hasFacts = evidenceFacts(event.detail, formatBytes).length > 0
  const rule = event.rule ? (optionalText(t, `btGuardRule${camel(event.rule)}`) ?? event.rule) : null
  const action = event.action ? (optionalText(t, `btGuardAction${camel(event.action)}`) ?? event.action) : null
  const mode = event.mode ? (optionalText(t, `btGuardMode${camel(event.mode)}`) ?? event.mode) : null
  return (
    <li className="flex flex-col gap-1 px-3 py-2">
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <span className="tabular text-xs text-muted-foreground">{formatDateTime(event.at)}</span>
        <Badge tone={event.kind === 'enforced' ? 'destructive' : event.kind === 'observed' ? 'neutral' : 'accent'}>{kindLabel(t, event.kind)}</Badge>
        {event.ipGroup !== '' ? <span className="min-w-0 break-all font-mono text-xs text-foreground">{event.ipGroup}</span> : null}
        {rule !== null ? <span className="text-xs text-muted-foreground">{rule}</span> : null}
        {mode !== null ? <Badge>{mode}</Badge> : null}
        {action !== null ? <Badge>{action}</Badge> : null}
      </div>
      {hasFacts ? (
        <EvidenceFacts evidence={event.detail} />
      ) : event.detail ? (
        <span title={event.detail} className="truncate whitespace-pre text-xs text-muted-foreground">
          {event.detail}
        </span>
      ) : null}
    </li>
  )
}

export function GuardEventsTab() {
  const t = useT()
  const [filter, setFilter] = useState<EventFilter>(DEFAULT_EVENT_FILTER)
  const key = `${filter.rule}|${filter.kind}`
  const feed = useGuardFeed<BtGuardEventDto>((offset, limit) => rpc.daemon.bt.guard.events.list({ ...eventsQuery(filter, 0, limit), offset }), key)
  const filtered = filter.rule !== 'all' || filter.kind !== 'all'
  const { items } = feed

  return (
    <>
      <SettingsSection>
        <SettingsCustomRow className="flex flex-wrap items-center gap-2">
          <Select<string>
            value={filter.kind}
            aria-label={t('btGuardColKind')}
            options={BT_GUARD_EVENT_KIND_FILTER.map((value) => ({ value, label: value === 'all' ? t('btGuardEventKindAll') : kindLabel(t, value) }))}
            className="w-auto min-w-[10rem] mobile:flex-1"
            onValueChange={(kind) => setFilter((prev) => ({ ...prev, kind }))}
          />
          <Select<BtGuardRule | 'all'>
            value={filter.rule}
            aria-label={t('btGuardColRule')}
            options={BT_GUARD_RULE_FILTER.map((value) => ({ value, label: value === 'all' ? t('btGuardRuleAll') : t(`btGuardRule${camel(value)}`) }))}
            className="w-auto min-w-[10rem] mobile:flex-1"
            onValueChange={(rule) => setFilter((prev) => ({ ...prev, rule }))}
          />
          <span className="tabular ml-auto text-xs text-muted-foreground">{t('btGuardEntriesTotal', { total: feed.total })}</span>
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
            <span className="text-xs text-muted-foreground">{filtered ? t('btGuardEventsFilterEmpty') : t('btGuardEventsEmpty')}</span>
          </SettingsCustomRow>
        ) : (
          <>
            <ul className="flex flex-col divide-y divide-hairline">
              {items.map((event) => (
                <EventRow key={event.id} event={event} />
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
    </>
  )
}
