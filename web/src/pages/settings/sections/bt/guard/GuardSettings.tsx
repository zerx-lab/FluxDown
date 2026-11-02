// 设置 › BT › 防吸血：总开关与状态、今日统计、检测规则、阈值、处置、豁免、云名单、过滤文件订阅、证据上报。

import { useState } from 'react'
import { useT } from '../../../../../i18n'
import { cloudStatusKind, reportGate, reportSwitchDisabled } from '../../../../../lib/btGuard'
import type { CloudStatusKind } from '../../../../../lib/btGuard'
import type { BtGuardCloudStatusDto, BtGuardFilterSourceDto, BtGuardStateDto } from '../../../../../lib/rpc'
import { rpc } from '../../../../../lib/rpc'
import { toastRpcError } from '../../../../../lib/rpcToast'
import { formatBytes } from '../../../../downloads/model/task'
import { Button, ConfirmFooter, Dialog, Switch } from '../../../../../ui'
import { DaemonEnumRow, DaemonNumberRow, DaemonSwitchRow, SettingsCustomRow, SettingsRow, SettingsSection, setDaemonBool, useDaemonBool } from '../../../kit'
import { ListEditorRow } from '../../subscription'
import { formatUnix } from '../../subscription/listFormat'
import { useGuardState, useLoggedIn } from './useGuard'
import type { GuardStateResult } from './useGuard'

const RULES = [
  { key: 'bt_guard_rule_excessive', title: 'btGuardRuleExcessive', desc: 'btGuardRuleExcessiveDesc' },
  { key: 'bt_guard_rule_rerequest', title: 'btGuardRuleRerequest', desc: 'btGuardRuleRerequestDesc' },
  { key: 'bt_guard_rule_rewind', title: 'btGuardRuleRewind', desc: 'btGuardRuleRewindDesc' },
  { key: 'bt_guard_rule_lag', title: 'btGuardRuleLag', desc: 'btGuardRuleLagDesc' },
  { key: 'bt_guard_rule_fingerprint', title: 'btGuardRuleFingerprint', desc: 'btGuardRuleFingerprintDesc' },
] as const

function LoadFailed({ message, onReload }: { message: string; onReload: () => void }) {
  const t = useT()
  return (
    <SettingsCustomRow className="flex items-center justify-between gap-3">
      <span role="alert" className="min-w-0 flex-1 text-xs text-destructive">
        {t('btGuardLoadFailed')}: {message}
      </span>
      <Button variant="outline" onClick={onReload}>
        {t('btGuardReload')}
      </Button>
    </SettingsCustomRow>
  )
}

function GeneralSection({ guard }: { guard: GuardStateResult }) {
  const t = useT()
  const { data, error, reload } = guard
  const enabled = useDaemonBool('bt_guard_enabled')
  const status = !enabled ? t('btGuardStatusDisabled') : data ? (data.sessionActive ? t('btGuardSessionOn') : t('btGuardSessionOff')) : t('btGuardLoading')
  return (
    <SettingsSection title={t('btGuardSectionGeneral')}>
      <DaemonSwitchRow configKey="bt_guard_enabled" titleKey="btGuardEnabled" descKey="btGuardEnabledDesc" />
      <SettingsCustomRow>
        <span className="text-xs text-muted-foreground">{status}</span>
      </SettingsCustomRow>
      {error !== null && data === null ? <LoadFailed message={error} onReload={reload} /> : null}
    </SettingsSection>
  )
}

function StatCell({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <span className="tabular text-title font-semibold text-foreground">{value}</span>
      <span className="truncate text-xs text-muted-foreground">{label}</span>
    </div>
  )
}

function StatsSection({ data }: { data: BtGuardStateDto | null }) {
  const t = useT()
  if (data === null) return null
  const { stats, counts } = data
  return (
    <SettingsSection title={t('btGuardSectionStats')}>
      <SettingsCustomRow>
        <div className="grid grid-cols-2 gap-4 desktop:grid-cols-4">
          <StatCell label={t('btGuardStatObserved')} value={String(stats.todayObserved)} />
          <StatCell label={t('btGuardStatEnforced')} value={String(stats.todayEnforced)} />
          <StatCell label={t('btGuardStatBlocked')} value={String(stats.todayBlockedConnections)} />
          <StatCell label={t('btGuardStatRefusedBytes')} value={formatBytes(stats.todayRefusedBytes)} />
        </div>
      </SettingsCustomRow>
      <SettingsCustomRow>
        <span className="text-xs text-muted-foreground">{t('btGuardCountsSummary', { ...counts })}</span>
      </SettingsCustomRow>
    </SettingsSection>
  )
}

function RulesSection({ disabled }: { disabled: boolean }) {
  const t = useT()
  return (
    <SettingsSection title={t('btGuardSectionRules')} subtitle={t('btGuardSectionRulesDesc')}>
      {RULES.map((rule) => (
        <DaemonEnumRow key={rule.key} configKey={rule.key} titleKey={rule.title} descKey={rule.desc} labelPrefix="btGuardMode" disabled={disabled} />
      ))}
    </SettingsSection>
  )
}

function ThresholdsSection({ disabled }: { disabled: boolean }) {
  const t = useT()
  return (
    <SettingsSection title={t('btGuardSectionThresholds')}>
      <DaemonNumberRow configKey="bt_guard_excess_ratio_pct" titleKey="btGuardExcessRatio" descKey="btGuardExcessRatioDesc" unit={t('btGuardUnitPercent')} disabled={disabled} />
      <DaemonNumberRow configKey="bt_guard_min_torrent_mib" titleKey="btGuardMinTorrent" descKey="btGuardMinTorrentDesc" unit={t('btGuardUnitMib')} disabled={disabled} />
      <DaemonNumberRow configKey="bt_guard_rewind_tolerance_pct" titleKey="btGuardRewindTolerance" descKey="btGuardRewindToleranceDesc" unit={t('btGuardUnitPercent')} disabled={disabled} />
      <DaemonNumberRow configKey="bt_guard_ipv6_prefix" titleKey="btGuardIpv6Prefix" descKey="btGuardIpv6PrefixDesc" disabled={disabled} />
    </SettingsSection>
  )
}

function ActionsSection({ disabled }: { disabled: boolean }) {
  const t = useT()
  return (
    <SettingsSection title={t('btGuardSectionActions')}>
      <DaemonNumberRow configKey="bt_guard_throttle_kib" titleKey="btGuardThrottleKib" descKey="btGuardThrottleKibDesc" unit={t('btGuardUnitKibps')} disabled={disabled} />
      <DaemonNumberRow configKey="bt_guard_ban_base_hours" titleKey="btGuardBanBaseHours" descKey="btGuardBanBaseHoursDesc" unit={t('timeUnitHours')} disabled={disabled} />
      <DaemonNumberRow configKey="bt_guard_ban_max_days" titleKey="btGuardBanMaxDays" unit={t('timeUnitDays')} disabled={disabled} />
      <DaemonEnumRow
        configKey="bt_guard_downloading_max_action"
        titleKey="btGuardDownloadingMaxAction"
        descKey="btGuardDownloadingMaxActionDesc"
        labelPrefix="btGuardDownloadingAction"
        disabled={disabled}
      />
    </SettingsSection>
  )
}

function ExemptionsSection({ disabled }: { disabled: boolean }) {
  const t = useT()
  return (
    <SettingsSection title={t('btGuardSectionExemptions')}>
      <DaemonSwitchRow configKey="bt_guard_skip_private" titleKey="btGuardSkipPrivate" descKey="btGuardSkipPrivateDesc" disabled={disabled} />
      <DaemonSwitchRow configKey="bt_guard_skip_lan" titleKey="btGuardSkipLan" descKey="btGuardSkipLanDesc" disabled={disabled} />
    </SettingsSection>
  )
}

const CLOUD_STATUS_TEXT: Record<Exclude<CloudStatusKind, 'ok'>, string> = {
  disabled: 'btGuardCloudDisabled',
  none: 'btGuardCloudNone',
  stale: 'btGuardCloudStale',
  rejected: 'btGuardCloudRejected',
  error: 'btGuardCloudLastError',
}

function CloudStatusCard({ cloud, enabled }: { cloud: BtGuardCloudStatusDto; enabled: boolean }) {
  const t = useT()
  const kind = cloudStatusKind(cloud, enabled)
  const hasDoc = kind !== 'disabled' && cloud.sequence > 0
  const problem =
    kind === 'ok'
      ? null
      : t(CLOUD_STATUS_TEXT[kind], { reason: cloud.rejectedReason ?? '', error: cloud.lastError ?? '' })
  const quiet = kind === 'none' || kind === 'disabled'
  const attribution = cloud.attribution ?? []
  return (
    <SettingsCustomRow className="flex flex-col gap-2">
      {hasDoc ? (
        <div className="flex flex-col gap-0.5 text-xs text-muted-foreground">
          <span className="text-sm text-foreground">{t('btGuardCloudSequence', { n: cloud.sequence })}</span>
          <span>{t('btGuardCloudEntries', { entries: cloud.entries, rules: cloud.clientRules })}</span>
          {cloud.updatedAt > 0 ? <span>{t('btGuardCloudUpdatedAt', { time: formatUnix(cloud.updatedAt) })}</span> : null}
          {cloud.expiresAt > 0 ? <span>{t('btGuardCloudExpiresAt', { time: formatUnix(cloud.expiresAt) })}</span> : null}
        </div>
      ) : null}
      {problem !== null ? (
        <span
          role={quiet ? undefined : 'alert'}
          title={problem}
          className={quiet ? 'line-clamp-4 break-words text-xs text-muted-foreground' : 'line-clamp-4 break-words text-xs text-destructive'}
        >
          {problem}
        </span>
      ) : null}
      {attribution.length > 0 ? (
        <div className="flex flex-col gap-0.5 border-t border-hairline pt-2 text-caption text-text-tertiary">
          <span>{t('btGuardCloudAttribution')}</span>
          {attribution.map((line) => (
            <span key={line} className="break-words">
              {line}
            </span>
          ))}
        </div>
      ) : null}
    </SettingsCustomRow>
  )
}

function CloudSection({ data, disabled }: { data: BtGuardStateDto | null; disabled: boolean }) {
  const t = useT()
  const cloudEnabled = useDaemonBool('bt_guard_cloud_enabled')
  return (
    <SettingsSection title={t('btGuardSectionCloud')}>
      <DaemonSwitchRow configKey="bt_guard_cloud_enabled" titleKey="btGuardCloudEnabled" descKey="btGuardCloudEnabledDesc" disabled={disabled} />
      <DaemonEnumRow configKey="bt_guard_cloud_mode" titleKey="btGuardCloudMode" labelPrefix="btGuardMode" disabled={disabled || !cloudEnabled} />
      {data ? <CloudStatusCard cloud={data.cloud} enabled={cloudEnabled} /> : null}
    </SettingsSection>
  )
}

function FilterSourceLine({ source }: { source: BtGuardFilterSourceDto }) {
  const t = useT()
  const detail = source.ok
    ? `${t('btGuardFilterEntries', { n: source.entries })} · ${source.updatedAt > 0 ? t('btGuardFilterUpdatedAt', { time: formatUnix(source.updatedAt) }) : t('btGuardFilterNeverUpdated')}`
    : t('btGuardFilterFailed', { error: source.error ?? '' })
  return (
    <li className="flex min-w-0 flex-col gap-0.5 py-1.5">
      <span title={source.url} className="truncate font-mono text-xs text-foreground">
        {source.url}
      </span>
      <span title={detail} className={source.ok ? 'line-clamp-3 break-words text-xs text-muted-foreground' : 'line-clamp-3 break-words text-xs text-destructive'}>
        {detail}
      </span>
    </li>
  )
}

function FilterSection({ guard, disabled }: { guard: GuardStateResult; disabled: boolean }) {
  const t = useT()
  const [busy, setBusy] = useState(false)
  const sources = guard.data?.filterSources ?? []
  const refresh = async () => {
    setBusy(true)
    try {
      const result = await rpc.daemon.bt.guard.filterSourcesRefresh()
      guard.patch((prev) => ({ ...prev, filterSources: result.sources }))
      guard.reload()
    } catch (error) {
      toastRpcError(error)
    } finally {
      setBusy(false)
    }
  }
  return (
    <SettingsSection title={t('btGuardSectionFilter')}>
      <ListEditorRow
        configKey="bt_guard_filter_urls"
        titleKey="btGuardFilterUrls"
        descKey="btGuardFilterUrlsDesc"
        placeholderKey="btGuardFilterUrlsPlaceholder"
        format="lines"
        disabled={disabled}
      />
      <SettingsCustomRow className="flex flex-col gap-2">
        <div className="flex items-center justify-between gap-3">
          {sources.length === 0 ? <span className="text-xs text-muted-foreground">{t('btGuardFilterEmpty')}</span> : <span />}
          <Button variant="outline" loading={busy} disabled={busy || disabled} onClick={() => void refresh()} className="shrink-0">
            {busy ? t('btGuardFilterRefreshing') : t('btGuardFilterRefresh')}
          </Button>
        </div>
        {sources.length > 0 ? <ul className="flex flex-col divide-y divide-hairline">{sources.map((source) => <FilterSourceLine key={source.url} source={source} />)}</ul> : null}
      </SettingsCustomRow>
    </SettingsSection>
  )
}

function ReportSection({ disabled }: { disabled: boolean }) {
  const t = useT()
  const loggedIn = useLoggedIn()
  const enabled = useDaemonBool('bt_guard_report_enabled')
  const [consent, setConsent] = useState(false)
  const needLogin = reportGate(loggedIn) === 'needLogin'
  // 「关闭」永远可用：总开关关闭 / 未登录只阻止新开启，已开启的上报仍可关掉。
  const switchDisabled = reportSwitchDisabled({ enabled, loggedIn, guardDisabled: disabled })
  const title = t('btGuardReportEnabled')
  return (
    <SettingsSection title={t('btGuardSectionReport')}>
      <SettingsRow title={title} description={t('btGuardReportEnabledDesc')} compact disabled={switchDisabled}>
        <Switch
          checked={enabled}
          disabled={switchDisabled}
          aria-label={title}
          onCheckedChange={(next) => {
            if (next) setConsent(true)
            else setDaemonBool('bt_guard_report_enabled', false)
          }}
        />
      </SettingsRow>
      {needLogin ? (
        <SettingsCustomRow>
          <span role="status" className="text-xs text-muted-foreground">
            {t('btGuardReportLoginRequired')}
          </span>
        </SettingsCustomRow>
      ) : null}
      <SettingsCustomRow className="flex flex-col gap-1">
        <span className="text-xs font-medium text-foreground">{t('btGuardReportContentsTitle')}</span>
        <span className="whitespace-pre-line text-xs text-muted-foreground">{t('btGuardReportContents')}</span>
      </SettingsCustomRow>
      <Dialog
        open={consent}
        onOpenChange={setConsent}
        title={t('btGuardReportConsentTitle')}
        footer={
          <ConfirmFooter
            okLabel={t('btGuardReportConsentConfirm')}
            onCancel={() => setConsent(false)}
            onOk={() => {
              setDaemonBool('bt_guard_report_enabled', true)
              setConsent(false)
            }}
          />
        }
      >
        <div className="flex flex-col gap-3 text-sm text-foreground">
          <p className="whitespace-pre-line">{t('btGuardReportConsentBody')}</p>
          <div className="flex flex-col gap-1 rounded-md border border-hairline p-3">
            <span className="text-xs font-medium">{t('btGuardReportContentsTitle')}</span>
            <span className="whitespace-pre-line text-xs text-muted-foreground">{t('btGuardReportContents')}</span>
          </div>
        </div>
      </Dialog>
    </SettingsSection>
  )
}

export function GuardSettingsTab() {
  const guard = useGuardState()
  const enabled = useDaemonBool('bt_guard_enabled')
  const off = !enabled
  return (
    <>
      <GeneralSection guard={guard} />
      <StatsSection data={guard.data} />
      <RulesSection disabled={off} />
      <ThresholdsSection disabled={off} />
      <ActionsSection disabled={off} />
      <ExemptionsSection disabled={off} />
      <CloudSection data={guard.data} disabled={off} />
      <FilterSection guard={guard} disabled={off} />
      <ReportSection disabled={off} />
    </>
  )
}
