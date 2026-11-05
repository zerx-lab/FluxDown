// 插件子页：已安装插件管理（启用 / 设置 / 登录 / 卸载）+ 安装区（zip 上传 / 服务端开发目录，仅开发模式显示）+ 专属插件 + 插件市场。

import { FolderOpen, Info, Package, RotateCw, ScrollText, Settings, Trash2 } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useT } from '../../../../i18n'
import { METHOD, call, rpc, uploadBlob, useDaemon } from '../../../../lib/rpc'
import type { DaemonSnapshot, InstalledPlugin, PluginArchiveRulesDto, PluginDto } from '../../../../lib/rpc'
import { Badge, Button, Card, ConfirmFooter, Dialog, EmptyState, FormField, Input, InputWithAction, Switch, confirmDialog, toast } from '../../../../ui'
import { hasPluginSettings } from '../../../plugins/contributions'
import { BlockTitle, ExtLink, IconButton, ListCard, ListRow } from './common'
import { detailFromPlugin } from './detail'
import type { PluginDetail } from './detail'
import { extensionErrorText } from './errors'
import { MarketSection } from './MarketSection'
import { PrivateSection } from './PrivateSection'
import { PluginAuditDialog } from './PluginAuditDialog'
import { PluginAuthDialog } from './PluginAuthDialog'
import { GrantConsentDialog, RiskBadge } from './PluginConsentDialog'
import { PluginDetailDialog } from './PluginDetailDialog'
import { PluginSettingsDialog } from './PluginSettingsDialog'
import { liveDialogPlugins, postInstallConsent } from './logic'
import type { ConsentSet } from './logic'

const EMPTY_PLUGINS: PluginDto[] = []
const selectPlugins = (daemon: DaemonSnapshot) => daemon.plugins

/** 安装（解包 / 校验 / 加载）可能比默认 30s 更久。 */
const INSTALL_TIMEOUT_MS = 180_000

/** 组件线名 → 标题键（依赖提醒里显示组件名）。 */
const COMPONENT_TITLE_KEYS: Record<string, string> = {
  ffmpeg: 'componentsFfmpegTitle',
  ytdlp: 'componentsYtdlpTitle',
}

type ZipPhase = { kind: 'uploading'; fraction: number } | { kind: 'installing' } | null

/** 详情对话框目标：已安装插件只存 identity（内容取最新快照），市场条目存条目详情。 */
type DetailTarget = { kind: 'installed'; identity: string } | { kind: 'market'; detail: PluginDetail }

export function PluginsTab({ stale, active, onGoToComponents }: { stale: boolean; active: boolean; onGoToComponents: () => void }) {
  const t = useT()
  const plugins = useDaemon(selectPlugins, EMPTY_PLUGINS)
  const [archiveStatus, setArchiveStatus] = useState<PluginArchiveRulesDto | null>(null)
  const [archiveBusy, setArchiveBusy] = useState(false)
  const [archiveMessage, setArchiveMessage] = useState<'success' | 'error' | null>(null)
  const [devMode, setDevMode] = useState(false)
  /** 「插件市场」刷新按钮点击计数；递增即让专属插件目录强制重拉。 */
  const [privateRefresh, setPrivateRefresh] = useState(0)
  const [devDir, setDevDir] = useState('')
  const [installingDir, setInstallingDir] = useState(false)
  const [zipPhase, setZipPhase] = useState<ZipPhase>(null)
  const [busy, setBusy] = useState<ReadonlySet<string>>(new Set())
  const [detail, setDetail] = useState<DetailTarget | null>(null)
  const [settingsIdentity, setSettingsIdentity] = useState<string | null>(null)
  const [authIdentity, setAuthIdentity] = useState<string | null>(null)
  const [missing, setMissing] = useState<string[] | null>(null)
  const [consentIdentity, setConsentIdentity] = useState<string | null>(null)
  const [audit, setAudit] = useState<{ identity: string | null } | null>(null)
  const fileInput = useRef<HTMLInputElement>(null)
  const pluginsRef = useRef(plugins)
  pluginsRef.current = plugins

  // 对话框只存插件 identity，内容永远取自最新快照（启停 / 更新 / 授权变化即时反映）；
  // 插件被卸载则关闭，授权框在待授权集合清空（别处已授权）后关闭。
  const live = liveDialogPlugins(plugins, { detail: detail?.kind === 'installed' ? detail.identity : null, settings: settingsIdentity, auth: authIdentity, consent: consentIdentity })
  useEffect(() => {
    if (detail?.kind === 'installed' && live.detail === null) setDetail(null)
    if (settingsIdentity !== null && live.settings === null) setSettingsIdentity(null)
    if (authIdentity !== null && live.auth === null) setAuthIdentity(null)
    if (consentIdentity !== null && live.consent === null) setConsentIdentity(null)
  }, [detail, settingsIdentity, authIdentity, consentIdentity, live.detail, live.settings, live.auth, live.consent])
  const settingsPlugin = live.settings
  const authPlugin = live.auth
  const consentPlugin = live.consent
  const shownDetail: PluginDetail | null = detail?.kind === 'market' ? detail.detail : live.detail ? detailFromPlugin(live.detail) : null
  const archivePlugin = detail?.kind === 'installed' ? live.detail : null
  const archiveRequest = useRef(0)
  useEffect(() => {
    const request = ++archiveRequest.current
    let active = true
    setArchiveStatus(null)
    setArchiveMessage(null)
    if (!archivePlugin?.contributions.archiveRules || stale) {
      setArchiveBusy(false)
      return () => { active = false }
    }
    setArchiveBusy(true)
    void rpc.daemon.plugin.archiveRules({ identity: archivePlugin.identity }).then((status) => {
      if (active && archiveRequest.current === request) {
        setArchiveStatus(status)
        setArchiveBusy(false)
      }
    }).catch(() => {
      if (active && archiveRequest.current === request) {
        setArchiveMessage('error')
        setArchiveBusy(false)
      }
    })
    return () => { active = false }
  }, [archivePlugin?.identity, archivePlugin?.contributions.archiveRules, archivePlugin?.enabled, stale])
  const refreshArchiveRules = async () => {
    if (!archivePlugin || archiveBusy || !archivePlugin.enabled) return
    const request = ++archiveRequest.current
    const identity = archivePlugin.identity
    setArchiveBusy(true)
    setArchiveMessage(null)
    try {
      const status = await rpc.daemon.plugin.archiveRules({ identity, refresh: true })
      if (archiveRequest.current === request) {
        setArchiveStatus(status)
        setArchiveMessage('success')
      }
    } catch {
      if (archiveRequest.current === request) setArchiveMessage('error')
    } finally {
      if (archiveRequest.current === request) setArchiveBusy(false)
    }
  }

  /** 操作发起时已安装的插件 identity（引擎在应答前就广播新插件表，应答后再看快照已分不出是安装还是更新）。 */
  const installedIdentities = (): ReadonlySet<string> => new Set(pluginsRef.current.map((item) => item.identity))

  /**
   * 安装 / 更新成功后的统一授权流程：待授权集合与用户安装前确认的集合吻合 → 直接授权，否则弹授权对话框。
   * `confirmed` 仅市场路径提供；`updating` = 操作发起时该插件已安装（提示文案用「已更新」）。
   */
  const onInstalled = async (result: InstalledPlugin, confirmed: ConsentSet | null = null, updating = false) => {
    toast.success(t(updating ? 'pluginOpUpdateSuccess' : 'pluginOpInstallSuccess'))
    if (result.missingComponents?.length) setMissing(result.missingComponents)
    let plugin: PluginDto | undefined
    try {
      plugin = (await rpc.daemon.plugin.list()).find((item) => item.identity === result.identity)
    } catch (error) {
      toast.error(t('pluginConsentFailed', { message: extensionErrorText(t, error) }))
      return
    }
    if (plugin === undefined) return
    const action = postInstallConsent(plugin, confirmed)
    if (action === 'dialog') {
      setConsentIdentity(plugin.identity)
    } else if (action === 'grant') {
      try {
        await rpc.daemon.plugin.consent({ identity: plugin.identity, permissions: plugin.permissions, networkHosts: plugin.networkHosts })
      } catch (error) {
        toast.error(t('pluginConsentFailed', { message: extensionErrorText(t, error) }))
        setConsentIdentity(plugin.identity)
      }
    }
  }
  const onInstallFailed = (error: unknown) => {
    toast.error(t('pluginOpInstallFailed', { message: extensionErrorText(t, error) }))
  }

  const withBusy = async (identity: string, action: () => Promise<void>) => {
    if (busy.has(identity)) return
    setBusy((current) => new Set(current).add(identity))
    try {
      await action()
    } finally {
      setBusy((current) => {
        const next = new Set(current)
        next.delete(identity)
        return next
      })
    }
  }

  const setEnabled = (plugin: PluginDto, enabled: boolean) =>
    withBusy(plugin.identity, async () => {
      try {
        await rpc.daemon.plugin.setEnabled({ identity: plugin.identity, enabled })
      } catch (error) {
        toast.error(t('pluginOpEnabledFailed', { message: extensionErrorText(t, error) }))
      }
    })

  const uninstall = async (plugin: PluginDto) => {
    const ok = await confirmDialog({
      title: t('pluginUninstallTitle'),
      description: t('pluginUninstallMsg', { name: plugin.name }),
      okLabel: t('pluginUninstallTooltip'),
      intent: 'destructive',
    })
    if (!ok) return
    await withBusy(plugin.identity, async () => {
      try {
        await rpc.daemon.plugin.uninstall({ identity: plugin.identity })
        toast.success(t('pluginOpUninstallSuccess'))
      } catch (error) {
        toast.error(t('pluginOpUninstallFailed', { message: extensionErrorText(t, error) }))
      }
    })
  }

  const reload = (plugin: PluginDto) =>
    withBusy(plugin.identity, async () => {
      try {
        const result = await rpc.daemon.plugin.reloadDev({ identity: plugin.identity })
        toast.success(t('pluginOpReloadSuccess'))
        if (result.missingComponents?.length) setMissing(result.missingComponents)
      } catch (error) {
        toast.error(t('pluginOpReloadFailed', { message: extensionErrorText(t, error) }))
      }
    })

  /** 浏览器选 zip → 上传 blob → `daemon.plugin.install {blobId}`。 */
  const installZip = async (file: File) => {
    if (zipPhase) return
    setZipPhase({ kind: 'uploading', fraction: 0 })
    const installedBefore = installedIdentities()
    try {
      const blobId = await uploadBlob('plugins', file, {
        onProgress: (fraction) => setZipPhase({ kind: 'uploading', fraction }),
      })
      setZipPhase({ kind: 'installing' })
      const result = await call<InstalledPlugin>(METHOD.DAEMON_PLUGIN_INSTALL, { blobId }, { timeoutMs: INSTALL_TIMEOUT_MS })
      await onInstalled(result, null, installedBefore.has(result.identity))
    } catch (error) {
      onInstallFailed(error)
    } finally {
      setZipPhase(null)
    }
  }

  const installDir = async () => {
    const dirPath = devDir.trim()
    if (dirPath === '' || installingDir) return
    setInstallingDir(true)
    const installedBefore = installedIdentities()
    try {
      const result = await rpc.daemon.plugin.installDev({ dirPath })
      await onInstalled(result, null, installedBefore.has(result.identity))
      setDevDir('')
    } catch (error) {
      onInstallFailed(error)
    } finally {
      setInstallingDir(false)
    }
  }

  const zipLabel =
    zipPhase?.kind === 'uploading'
      ? t('webPluginUploading', { percent: Math.round(zipPhase.fraction * 100) })
      : zipPhase?.kind === 'installing'
        ? t('marketInstallingButton')
        : t('pluginInstallZipButton')

  return (
    <div className="flex w-full flex-col gap-4">
      <BlockTitle
        title={t('pluginsSectionTitle')}
        trailing={
          <div className="flex items-center gap-3">
            <Button variant="outline" icon={ScrollText} onClick={() => setAudit({ identity: null })}>
              {t('pluginAuditOpenAll')}
            </Button>
            <label className="flex items-center gap-2 text-xs text-muted-foreground">
              {t('pluginDevModeSwitch')}
              <Switch checked={devMode} onCheckedChange={setDevMode} aria-label={t('pluginDevModeSwitch')} />
            </label>
          </div>
        }
      />

      {devMode ? (
        <Card className="flex flex-col gap-4 p-3">
          <div>
            <input
              ref={fileInput}
              type="file"
              accept=".fxplug,.zip"
              hidden
              onChange={(event) => {
                const file = event.target.files?.[0]
                event.target.value = ''
                if (file) void installZip(file)
              }}
            />
            <Button variant="outline" icon={FolderOpen} loading={zipPhase !== null} disabled={stale} onClick={() => fileInput.current?.click()}>
              {zipLabel}
            </Button>
          </div>
          <FormField label={t('pluginInstallDirLabel')} htmlFor="plugin-install-dir" hint={t('webPluginInstallDirHint')}>
            <InputWithAction
              input={
                <Input
                  id="plugin-install-dir"
                  value={devDir}
                  placeholder={t('pluginInstallDirPlaceholder')}
                  disabled={installingDir}
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  onChange={(event) => setDevDir(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') void installDir()
                  }}
                />
              }
              action={
                <Button variant="primary" loading={installingDir} disabled={stale || devDir.trim() === ''} onClick={() => void installDir()}>
                  {t('pluginInstallDirButton')}
                </Button>
              }
            />
          </FormField>
        </Card>
      ) : null}

      {plugins.length === 0 ? (
        <EmptyState icon={Package} title={t('pluginsEmpty')} />
      ) : (
        <ListCard>
          {plugins.map((plugin) => (
            <PluginRow
              key={plugin.identity}
              plugin={plugin}
              disabled={stale || busy.has(plugin.identity)}
              onDetail={() => setDetail({ kind: 'installed', identity: plugin.identity })}
              onSettings={() => setSettingsIdentity(plugin.identity)}
              onAuth={() => setAuthIdentity(plugin.identity)}
              onConsent={() => setConsentIdentity(plugin.identity)}
              onUninstall={() => void uninstall(plugin)}
              onReload={() => void reload(plugin)}
              onToggle={(enabled) => void setEnabled(plugin, enabled)}
            />
          ))}
        </ListCard>
      )}

      <PrivateSection
        stale={stale}
        active={active}
        plugins={plugins}
        onInstalled={onInstalled}
        onInstallFailed={onInstallFailed}
        onShowDetail={(shown) => setDetail({ kind: 'market', detail: shown })}
        refreshNonce={privateRefresh}
      />

      <MarketSection
        stale={stale}
        active={active}
        plugins={plugins}
        onInstalled={onInstalled}
        onInstallFailed={onInstallFailed}
        onShowDetail={(shown) => setDetail({ kind: 'market', detail: shown })}
        onRefresh={() => setPrivateRefresh((count) => count + 1)}
      />

      <PluginDetailDialog
        detail={shownDetail}
        onClose={() => setDetail(null)}
        onAudit={shownDetail && plugins.some((item) => item.identity === shownDetail.identity) ? (identity) => setAudit({ identity }) : undefined}
        archiveRules={archivePlugin?.contributions.archiveRules ? archiveStatus : null}
        archiveDisabled={!archivePlugin?.enabled || archiveBusy || stale}
        archiveBusy={archiveBusy}
        archiveMessage={archiveMessage}
        onArchiveRefresh={() => void refreshArchiveRules()}
      />
      <PluginSettingsDialog plugin={settingsPlugin} onClose={() => setSettingsIdentity(null)} />
      <PluginAuthDialog plugin={authPlugin} onClose={() => setAuthIdentity(null)} />
      <GrantConsentDialog plugin={consentPlugin} onClose={() => setConsentIdentity(null)} />
      <PluginAuditDialog open={audit !== null} initialIdentity={audit?.identity ?? null} onClose={() => setAudit(null)} />
      <Dialog
        open={missing !== null}
        onOpenChange={(open) => !open && setMissing(null)}
        title={t('pluginDepsMissingTitle')}
        description={
          missing
            ? t('pluginDepsMissingBody', {
                components: missing.map((name) => (COMPONENT_TITLE_KEYS[name] ? t(COMPONENT_TITLE_KEYS[name]) : name)).join(', '),
              })
            : undefined
        }
        size="sm"
        footer={
          <ConfirmFooter
            cancelLabel={t('pluginDepsLater')}
            okLabel={t('pluginDepsGoToComponents')}
            onCancel={() => setMissing(null)}
            onOk={() => {
              setMissing(null)
              onGoToComponents()
            }}
          />
        }
      />
    </div>
  )
}

function PluginRow({
  plugin,
  disabled,
  onDetail,
  onSettings,
  onAuth,
  onConsent,
  onUninstall,
  onReload,
  onToggle,
}: {
  plugin: PluginDto
  disabled: boolean
  onDetail: () => void
  onSettings: () => void
  onAuth: () => void
  onConsent: () => void
  onUninstall: () => void
  onReload: () => void
  onToggle: (enabled: boolean) => void
}) {
  const t = useT()
  const loadFailed = plugin.loadStatus === 'Failed'
  return (
    <ListRow
      info={
        <>
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className="min-w-0 break-words text-sm font-medium text-foreground">{plugin.name}</span>
            <span className="tabular text-xs text-muted-foreground">v{plugin.version}</span>
            {plugin.devMode ? <Badge>{t('pluginDevModeBadge')}</Badge> : null}
            {/* 颜色是信号：只有加载失败 / 熔断停用着 destructive，其余中性。 */}
            {loadFailed ? <Badge tone="destructive">{t('pluginLoadStatusFailed')}</Badge> : <Badge>{t('pluginLoadStatusLoaded')}</Badge>}
            <RiskBadge risk={plugin.risk} />
            {plugin.disabledReason === 'Manual' ? <Badge>{t('pluginDisabledManual')}</Badge> : null}
            {plugin.disabledReason === 'CircuitBreaker' ? <Badge tone="destructive">{t('pluginDisabledCircuitBreaker')}</Badge> : null}
            {plugin.disabledReason === 'Revoked' ? <Badge tone="destructive">{t('pluginDisabledRevoked')}</Badge> : null}
            {plugin.disabledReason === 'ConsentRequired' ? <Badge tone="warning">{t('pluginDisabledConsentRequired')}</Badge> : null}
          </div>
          {plugin.homepage ? <ExtLink href={plugin.homepage} /> : null}
          {plugin.description ? <div className="line-clamp-2 break-words text-xs text-muted-foreground">{plugin.description}</div> : null}
          {loadFailed && plugin.loadError ? <div className="break-words text-xs text-destructive">{plugin.loadError}</div> : null}
        </>
      }
      actions={
        <>
          {plugin.devMode ? <IconButton icon={RotateCw} label={t('pluginReloadTooltip')} onClick={onReload} disabled={disabled} /> : null}
          <IconButton icon={Info} label={t('pluginDetailDescription')} onClick={onDetail} />
          {plugin.disabledReason === 'ConsentRequired' ? (
            <Button variant="outline" onClick={onConsent} disabled={disabled}>
              {t('pluginConsentButton')}
            </Button>
          ) : null}
          {!loadFailed && hasPluginSettings(plugin) ? (
            <IconButton icon={Settings} label={t('pluginSettingsTooltip')} onClick={onSettings} disabled={disabled} />
          ) : null}
          {!loadFailed && plugin.authSupported ? (
            <Button variant="outline" onClick={onAuth} disabled={disabled}>
              {t('pluginAuthButton')}
            </Button>
          ) : null}
          <IconButton icon={Trash2} label={t('pluginUninstallTooltip')} onClick={onUninstall} disabled={disabled} destructive />
          <Switch
            checked={plugin.enabled && !loadFailed}
            disabled={disabled || loadFailed}
            aria-label={plugin.name}
            onCheckedChange={onToggle}
            className="mx-1"
          />
        </>
      }
    />
  )
}
