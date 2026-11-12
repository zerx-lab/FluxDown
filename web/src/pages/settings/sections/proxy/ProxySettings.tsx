// 代理（crates/settings/src/sections/proxy.rs）：模式 / 系统代理只读信息 / 手动服务器（任何模式可编辑）/ 连通性测试 /
// BitTorrent / 站点规则 / 站点凭据。

import { useEffect, useState } from 'react'
import { useT } from '../../../../i18n'
import { RpcError, errorMessage, rpc } from '../../../../lib/rpc'
import type { ProxyTestRequest, SystemProxyDto } from '../../../../lib/rpc'
import { enabledRuleCount } from '../../../../lib/proxyRules'
import { Button } from '../../../../ui'
import {
  DaemonEnumRow,
  DaemonSwitchRow,
  DaemonTextRow,
  DropdownField,
  SettingsPage,
  SettingsRow,
  SettingsSection,
  TextField,
  setDaemon,
  useDaemonBool,
  useDaemonValue,
} from '../../kit'
import { InfoLine } from './InfoLine'
import { btProxyHint } from './btProxy'
import { ProxyRulesGroup } from './ProxyRulesGroup'
import { SiteAuthGroup } from './SiteAuthGroup'

const MODES = ['none', 'system', 'manual', 'auto'] as const
type ProxyMode = (typeof MODES)[number]

const TYPES = [
  { value: 'http', label: 'HTTP' },
  { value: 'https', label: 'HTTPS' },
  { value: 'socks4', label: 'SOCKS4' },
  { value: 'socks5', label: 'SOCKS5' },
]

/** 只读值展示行（不可编辑）。 */
function ReadonlyRow({ titleKey, value }: { titleKey: string; value: string }) {
  const t = useT()
  return (
    <SettingsRow title={t(titleKey)}>
      <span className="break-all text-sm text-muted-foreground">{value}</span>
    </SettingsRow>
  )
}

/** 端口号：过滤非数字并夹取到 1..=65535。 */
function PortRow() {
  const t = useT()
  const value = useDaemonValue('proxy_port')
  return (
    <SettingsRow title={t('proxyPort')} description={t('proxyPortPlaceholder')}>
      <TextField
        value={value}
        aria-label={t('proxyPort')}
        onCommit={(raw) => {
          const digits = raw.replace(/\D/g, '')
          const parsed = digits === '' ? Number.NaN : Number.parseInt(digits, 10)
          setDaemon('proxy_port', Number.isFinite(parsed) ? String(Math.min(65535, Math.max(1, parsed))) : '')
        }}
      />
    </SettingsRow>
  )
}

/** 连通性测试：结果文本在按钮左侧。 */
function TestConnectionRow({ source, system }: { source: 'manual' | 'system'; system: SystemProxyDto | null }) {
  const t = useT()
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState('')
  const [failed, setFailed] = useState(false)
  const type = useDaemonValue('proxy_type')
  const host = useDaemonValue('proxy_host')
  const port = useDaemonValue('proxy_port')
  const username = useDaemonValue('proxy_username')
  const password = useDaemonValue('proxy_password')

  const run = async () => {
    const params: ProxyTestRequest =
      source === 'manual'
        ? { proxyType: type, host, port, username, password }
        : { proxyType: system?.proxyType ?? '', host: system?.host ?? '', port: String(system?.port ?? 0), username: '', password: '' }
    setBusy(true)
    try {
      const response = await rpc.daemon.config.proxyTest(params)
      setFailed(false)
      setResult(t('proxyTestSuccess', { ms: response.latencyMs }))
    } catch (error) {
      setFailed(true)
      const detail = error instanceof RpcError && /did not accept a TLS handshake/i.test(error.message) ? t('proxyTestTlsEndpointHint') : error instanceof RpcError ? (error.appCode ?? error.message) : errorMessage(error)
      setResult(t('proxyTestFailed', { error: detail }))
    } finally {
      setBusy(false)
    }
  }

  return (
    <SettingsRow title={t('proxyTestConnection')}>
      <div className="flex flex-wrap items-center gap-2 desktop:justify-end">
        {result ? <span className={failed ? 'min-w-0 text-xs text-destructive' : 'min-w-0 text-xs text-muted-foreground'}>{result}</span> : null}
        <Button variant="outline" loading={busy} onClick={() => void run()}>
          {t(busy ? 'proxyTesting' : 'proxyTestConnection')}
        </Button>
      </div>
    </SettingsRow>
  )
}

/** `system` 模式：检测状态 + 只读系统代理详情。 */
function SystemSection() {
  const t = useT()
  const [detected, setDetected] = useState<SystemProxyDto | null>(null)
  const [busy, setBusy] = useState(true)
  useEffect(() => {
    let cancelled = false
    setBusy(true)
    // 切到系统代理模式时立即重新检测（系统设置可能在两次切换之间变化）。
    rpc.daemon.config
      .systemProxy()
      .then((dto) => {
        if (!cancelled) setDetected(dto)
      })
      .catch(() => undefined)
      .finally(() => {
        if (!cancelled) setBusy(false)
      })
    return () => {
      cancelled = true
    }
  }, [])
  const status = busy || detected === null ? t('proxySystemDetecting') : detected.detected ? t('proxySystemDetected') : t('proxySystemNotConfigured')
  return (
    <SettingsSection title={t('proxyModeSystem')} subtitle={t('proxyModeSystemDesc')}>
      <InfoLine text={status} />
      {detected?.detected && !busy ? (
        <>
          <ReadonlyRow titleKey="proxyType" value={detected.proxyType.toUpperCase()} />
          <ReadonlyRow titleKey="proxyHost" value={detected.host} />
          <ReadonlyRow titleKey="proxyPort" value={String(detected.port)} />
          {detected.noList ? <ReadonlyRow titleKey="proxyNoList" value={detected.noList} /> : null}
          <TestConnectionRow source="system" system={detected} />
        </>
      ) : null}
    </SettingsSection>
  )
}

/**
 * 手动服务器在任何模式下都可编辑：「自动」把它当备选路径，站点规则与任务的「手动代理」出口也读它。
 * 非手动模式默认收起为一行摘要 + 展开按钮（GPUI `proxy.rs::manual_section`）；离开本页即恢复收起。
 */
function ManualSection({ active }: { active: boolean }) {
  const t = useT()
  const type = useDaemonValue('proxy_type')
  const host = useDaemonValue('proxy_host').trim()
  const port = useDaemonValue('proxy_port').trim()
  const [expanded, setExpanded] = useState(false)
  const open = active || expanded
  const summary = host === '' ? t('proxyManualNotSet') : `${type.toUpperCase()} · ${port === '' ? host : `${host}:${port}`}`
  return (
    <SettingsSection title={t('proxyModeManual')} subtitle={t(active ? 'proxyModeManualDesc' : 'proxyManualKeptDesc')}>
      {active ? null : (
        <SettingsRow title={t('proxyManualServer')} description={summary}>
          <Button variant="outline" aria-expanded={expanded} onClick={() => setExpanded(!expanded)}>
            {t(expanded ? 'proxyManualCollapse' : 'proxyManualExpand')}
          </Button>
        </SettingsRow>
      )}
      {open ? (
        <>
          <SettingsRow title={t('proxyType')}>
            <DropdownField value={type} options={TYPES} aria-label={t('proxyType')} onValueChange={(next) => setDaemon('proxy_type', next)} />
          </SettingsRow>
          <DaemonTextRow configKey="proxy_host" titleKey="proxyHost" descKey="proxyHostPlaceholder" />
          <PortRow />
          <DaemonTextRow configKey="proxy_username" titleKey="proxyUsername" descKey="proxyUsernamePlaceholder" />
          <PasswordRow />
          <DaemonTextRow configKey="proxy_no_list" titleKey="proxyNoList" descKey="proxyNoListDesc" />
          <TestConnectionRow source="manual" system={null} />
        </>
      ) : null}
    </SettingsSection>
  )
}

/** 密码不在页面上明文回显（GPUI 为普通输入；Web 页面可被旁观，改用密码框）。 */
function PasswordRow() {
  const t = useT()
  const value = useDaemonValue('proxy_password')
  return (
    <SettingsRow title={t('proxyPassword')} description={t('proxyPasswordPlaceholder')}>
      <TextField type="password" value={value} aria-label={t('proxyPassword')} onCommit={(next) => setDaemon('proxy_password', next)} />
    </SettingsRow>
  )
}

/** BitTorrent 走代理：总开关 + 代理来源 + 节点连接是否也走代理 + 一条提示。 */
function BtProxySection() {
  const t = useT()
  const enabled = useDaemonBool('bt_proxy_enabled')
  const hint = btProxyHint(
    useDaemonValue('bt_proxy_source'),
    useDaemonValue('proxy_type'),
    useDaemonValue('proxy_host'),
    useDaemonValue('proxy_port'),
  )
  return (
    <SettingsSection title={t('proxyBtSection')}>
      <DaemonSwitchRow configKey="bt_proxy_enabled" titleKey="btProxyEnabled" descKey="btProxyEnabledDesc" />
      {enabled ? (
        <>
          <DaemonEnumRow configKey="bt_proxy_source" titleKey="btProxySource" descKey="btProxySourceDesc" labelPrefix="btProxySource" />
          <DaemonSwitchRow configKey="bt_proxy_peers" titleKey="btProxyPeers" descKey="btProxyPeersDesc" />
        </>
      ) : null}
      {enabled && hint !== null ? <InfoLine text={t(hint)} /> : null}
    </SettingsSection>
  )
}

export function ProxySettings() {
  const t = useT()
  const mode = useDaemonValue('proxy_mode')
  const ruleCount = enabledRuleCount(useDaemonValue('proxy_rules'))
  // 快照未就绪前 useDaemonValue 回落到目录默认值（none），无需额外判空。
  const current = (MODES as readonly string[]).includes(mode) ? (mode as ProxyMode) : 'none'
  return (
    <SettingsPage title={t('settingsCatProxy')} description={t('settingsCatProxyDesc')}>
      <SettingsSection title={t('proxySettings')} subtitle={t('proxyBtNote')}>
        <SettingsRow title={t('proxySettings')} description={t('proxySettingsDesc')}>
          <DropdownField
            value={current}
            aria-label={t('proxySettings')}
            options={[
              { value: 'none', label: t('proxyModeNone') },
              { value: 'system', label: t('proxyModeSystem') },
              { value: 'manual', label: t('proxyModeManual') },
              { value: 'auto', label: t('proxyModeAuto') },
            ]}
            onValueChange={(next) => setDaemon('proxy_mode', next)}
          />
        </SettingsRow>
        {ruleCount > 0 ? <InfoLine text={t('proxyRulesActiveHint', { count: ruleCount })} /> : null}
      </SettingsSection>
      {current === 'auto' ? (
        <SettingsSection>
          <InfoLine text={t('proxyModeAutoDesc')} />
        </SettingsSection>
      ) : null}
      {current === 'system' ? <SystemSection /> : null}
      <ManualSection active={current === 'manual'} />
      <BtProxySection />
      <ProxyRulesGroup />
      <SiteAuthGroup />
    </SettingsPage>
  )
}
