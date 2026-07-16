// 插件管理：已安装插件列表（启用开关 + 设置表单 + 卸载，disabledReason 徽标区分手动/熔断）
// + 安装区（zip 文件上传 / dev 模式目录路径引用）。

import { type ChangeEvent, type ReactNode, useRef, useState } from 'react'
import * as Dialog from '@radix-ui/react-dialog'
import { Check, Download, Link2, ShieldCheck, Trash2, Upload, X } from 'lucide-react'
import { cn } from '../../lib/cn'
import { confirmDialog } from '../../lib/confirm'
import { type I18nKey, translateBackendMessage, useI18n } from '../../lib/i18n'
import type { InstalledPlugin, MarketEntry, PluginDto } from '../../lib/types'
import {
  useInstallFromMarket,
  useInstallPluginDevMutation,
  useInstallPluginMutation,
  useMarketQuery,
  usePluginsQuery,
  useSetPluginEnabledMutation,
  useUninstallPluginMutation,
  useUpdatePluginSettingsMutation,
} from '../../hooks/usePlugins'
import { SetRow, SetSwitch } from './controls'
import { PluginSettingsDialog } from './PluginSettingForm'

export function PluginsSettings() {
  const { t } = useI18n()
  const { data: plugins, isLoading, isError } = usePluginsQuery()
  const installMut = useInstallPluginMutation()
  const installDevMut = useInstallPluginDevMutation()
  const [devPath, setDevPath] = useState('')
  // 最近一次安装成功后缺失的基础组件（提醒式：安装已成功，组件装好前对应能力不可用）。
  const [missingDeps, setMissingDeps] = useState<string[]>([])
  const fileRef = useRef<HTMLInputElement>(null)
  const { data: market, isLoading: marketLoading, isError: marketError } = useMarketQuery()
  const installFromMarketMut = useInstallFromMarket()
  const installedIds = new Set(plugins?.map((p) => p.identity) ?? [])

  function noteMissingDeps(res: InstalledPlugin) {
    setMissingDeps(res.missingComponents ?? [])
  }

  function onZipChosen(e: ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0]
    e.target.value = ''
    if (file) installMut.mutate(file, { onSuccess: noteMissingDeps })
  }

  function installDev() {
    const dir = devPath.trim()
    if (!dir) return
    installDevMut.mutate(dir, {
      onSuccess: (res) => {
        setDevPath('')
        noteMissingDeps(res)
      },
    })
  }

  const installError = installMut.error ?? installDevMut.error

  return (
    <div className="max-w-[640px]">
      <p className="set-desc">{t('set.plugins.desc')}</p>

      <div className="set-group">
        <SetRow title={t('plugins.installZip')} desc={t('plugins.installZipDesc')}>
          <input ref={fileRef} type="file" accept=".zip" className="hidden" onChange={onZipChosen} />
          <button
            type="button"
            className="btn ghost sm flex-shrink-0"
            onClick={() => fileRef.current?.click()}
            disabled={installMut.isPending}
          >
            <Upload size={14} />
            {installMut.isPending ? t('common.loading') : t('plugins.installZip')}
          </button>
        </SetRow>
        <SetRow title={t('plugins.installDev')} desc={t('plugins.installDevDesc')}>
          <div className="flex flex-shrink-0 items-center gap-2" style={{ width: 300 }}>
            <input
              className="text-input flex-1"
              placeholder={t('plugins.devPathPlaceholder')}
              value={devPath}
              onChange={(e) => setDevPath(e.target.value)}
            />
            <button
              type="button"
              className="btn ghost sm flex-shrink-0"
              onClick={installDev}
              disabled={installDevMut.isPending || devPath.trim() === ''}
            >
              {installDevMut.isPending ? t('common.loading') : t('plugins.installDev')}
            </button>
          </div>
        </SetRow>
        {installError && (
          <p className="px-4 pb-3 text-[12px] text-danger">
            {t('plugins.installFailed', { error: translateBackendMessage(installError.message) })}
          </p>
        )}
        {missingDeps.length > 0 && (
          <p className="px-4 pb-3 text-[12px] text-[var(--warning)]">
            {t('plugins.depsMissing', {
              components: missingDeps.map((c) => t(c === 'ytdlp' ? 'components.ytdlp' : 'components.ffmpeg')).join(', '),
            })}
          </p>
        )}
      </div>

      {isLoading ? (
        <p className="set-desc">{t('common.loading')}</p>
      ) : isError ? (
        <p className="set-desc text-danger">{t('set.loadFailed')}</p>
      ) : !plugins || plugins.length === 0 ? (
        <p className="set-desc">{t('plugins.empty')}</p>
      ) : (
        <div className="flex flex-col gap-3">
          {plugins.map((p) => (
            <PluginCard key={p.identity} plugin={p} />
          ))}
        </div>
      )}

      <h2 className="set-title mt-7">{t('market.title')}</h2>
      <p className="set-desc">{t('market.desc')}</p>
      {marketLoading ? (
        <p className="set-desc">{t('common.loading')}</p>
      ) : marketError ? (
        <p className="set-desc text-danger">{t('market.loadFailed')}</p>
      ) : !market || market.length === 0 ? (
        <p className="set-desc">{t('market.empty')}</p>
      ) : (
        <div className="flex flex-col gap-3">
          {market.map((entry) => (
            <MarketCard
              key={entry.pluginId}
              entry={entry}
              installed={installedIds.has(entry.pluginId)}
              installMut={installFromMarketMut}
              onInstalled={noteMissingDeps}
            />
          ))}
        </div>
      )}
    </div>
  )
}

type BadgeTone = 'accent' | 'neutral' | 'danger'

function Badge({ tone, children }: { tone: BadgeTone; children: ReactNode }) {
  return (
    <span
      className={cn(
        'rounded-full px-2 py-0.5 text-[11px] font-medium',
        tone === 'accent' && 'bg-accent-weak text-accent',
        tone === 'neutral' && 'bg-surface2 text-text3',
        tone === 'danger' && 'bg-danger/10 text-danger',
      )}
    >
      {children}
    </span>
  )
}

function DisabledBadge({ reason }: { reason: PluginDto['disabledReason'] }) {
  const { t } = useI18n()
  if (reason === 'None') return null
  const manual = reason === 'Manual'
  return <Badge tone={manual ? 'neutral' : 'danger'}>{manual ? t('plugins.disabledManual') : t('plugins.disabledCircuitBreaker')}</Badge>
}

// 权限徽章：manifest 声明的能力权限（后续新增权限补 PERMISSION_KEYS 即可；
// 未知权限降级展示原始名）。
const PERMISSION_KEYS: Record<string, { label: I18nKey; desc: I18nKey }> = {
  ffmpeg: { label: 'plugins.permFfmpeg', desc: 'plugins.permFfmpegDesc' },
  ytdlp: { label: 'plugins.permYtdlp', desc: 'plugins.permYtdlpDesc' },
}

function PermissionBadges({ permissions }: { permissions?: string[] }) {
  const { t } = useI18n()
  if (!permissions || permissions.length === 0) return null
  return (
    <>
      {permissions.map((perm) => {
        const keys = PERMISSION_KEYS[perm]
        return (
          <span
            key={perm}
            className="inline-flex items-center gap-1 rounded-md bg-accent-weak px-1.5 py-0.5 text-[10px] font-semibold text-accent"
            title={keys ? t(keys.desc) : perm}
          >
            <ShieldCheck size={10} />
            {keys ? t(keys.label) : perm}
          </span>
        )
      })}
    </>
  )
}

function PluginCard({ plugin }: { plugin: PluginDto }) {
  const { t } = useI18n()
  const enabledMut = useSetPluginEnabledMutation()
  const settingsMut = useUpdatePluginSettingsMutation()
  const uninstallMut = useUninstallPluginMutation()
  const [detailOpen, setDetailOpen] = useState(false)

  async function uninstall() {
    const ok = await confirmDialog({
      title: t('plugins.uninstallTitle'),
      message: t('plugins.uninstallMsg', { name: plugin.name }),
      danger: true,
    })
    if (ok) uninstallMut.mutate(plugin.identity)
  }

  return (
    <>
      <div
        role="button"
        tabIndex={0}
        className="cursor-pointer rounded-xl border border-line bg-surface p-4 transition-colors hover:border-accent/50"
        onClick={() => setDetailOpen(true)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault()
            setDetailOpen(true)
          }
        }}
      >
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <b className="text-[13px] font-semibold">{plugin.name}</b>
              <span className="text-[11px] tabular-nums text-text3">v{plugin.version}</span>
              {plugin.devMode && <Badge tone="accent">{t('plugins.devMode')}</Badge>}
              <DisabledBadge reason={plugin.disabledReason} />
              <PermissionBadges permissions={plugin.permissions} />
            </div>
            {plugin.description && (
              <p className="mt-1.5 line-clamp-3 text-[12px] leading-relaxed text-text2">{plugin.description}</p>
            )}
            {plugin.homepage && (
              <a
                className="mt-1.5 inline-flex items-center gap-1 text-[11.5px] text-accent hover:underline"
                href={plugin.homepage}
                target="_blank"
                rel="noreferrer"
                onClick={(e) => e.stopPropagation()}
              >
                <Link2 size={11} />
                {plugin.homepage}
              </a>
            )}
          </div>
          <div className="flex flex-shrink-0 items-center gap-2" onClick={(e) => e.stopPropagation()}>
            {plugin.settings.length > 0 && (
              <PluginSettingsDialog
                plugin={plugin}
                saving={settingsMut.isPending}
                onSave={(entries, onDone) => settingsMut.mutate({ identity: plugin.identity, entries }, { onSuccess: onDone })}
              />
            )}
            <SetSwitch
              checked={plugin.enabled}
              onCheckedChange={(v) => enabledMut.mutate({ identity: plugin.identity, enabled: v })}
            />
            <button
              type="button"
              className="icon-btn sm text-text3 hover:text-danger"
              title={t('plugins.uninstallTitle')}
              aria-label={t('plugins.uninstallTitle')}
              onClick={() => void uninstall()}
              disabled={uninstallMut.isPending}
            >
              <Trash2 size={14} />
            </button>
          </div>
        </div>
      </div>
      <PluginDetailDialog
        open={detailOpen}
        onOpenChange={setDetailOpen}
        detail={{
          name: plugin.name,
          version: plugin.version,
          identity: plugin.identity,
          description: plugin.description,
          homepage: plugin.homepage,
          settingsCount: plugin.settings.length,
          permissions: plugin.permissions,
        }}
      />
    </>
  )
}

const YANKED_KEYS: Record<string, I18nKey> = {
  deprecated: 'market.yanked.deprecated',
  vulnerable: 'market.yanked.vulnerable',
  malicious: 'market.yanked.malicious',
}

function YankedBadge({ yanked }: { yanked: string }) {
  const { t } = useI18n()
  if (yanked === '' || yanked === 'none') return null
  const key = YANKED_KEYS[yanked]
  return <Badge tone="danger">{key ? t(key) : yanked}</Badge>
}

function MarketCard({
  entry,
  installed,
  installMut,
  onInstalled,
}: {
  entry: MarketEntry
  installed: boolean
  installMut: ReturnType<typeof useInstallFromMarket>
  onInstalled: (res: InstalledPlugin) => void
}) {
  const { t } = useI18n()
  const pending = installMut.isPending && installMut.variables === entry.pluginId
  const label = entry.name || entry.pluginId
  const initial = label.trim().charAt(0).toUpperCase() || '?'
  const [detailOpen, setDetailOpen] = useState(false)
  const yankedLabel = entry.yanked === '' || entry.yanked === 'none' ? null : t((YANKED_KEYS[entry.yanked] ?? 'market.yanked.deprecated'))

  return (
    <>
      <div
        role="button"
        tabIndex={0}
        className="cursor-pointer rounded-xl border border-line bg-surface p-4 transition-colors hover:border-accent/50"
        onClick={() => setDetailOpen(true)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault()
            setDetailOpen(true)
          }
        }}
      >
        <div className="flex items-start gap-3">
          <div className="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-lg bg-accent-weak text-[13px] font-semibold text-accent">
            {initial}
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <b className="text-[13px] font-semibold">{label}</b>
              <span className="text-[11px] tabular-nums text-text3">v{entry.version}</span>
              {entry.author && (
                <span className="text-[11px] text-text3">
                  <span className="opacity-50">· </span>
                  {entry.author}
                </span>
              )}
              <YankedBadge yanked={entry.yanked} />
              <PermissionBadges permissions={entry.permissions} />
            </div>
            {entry.description && (
              <p className="mt-1.5 line-clamp-3 text-[12px] leading-relaxed text-text2">{entry.description}</p>
            )}
            {entry.tags.length > 0 && (
              <div className="mt-1.5 flex flex-wrap gap-1.5">
                {entry.tags.map((tag) => (
                  <Badge key={tag} tone="neutral">
                    {tag}
                  </Badge>
                ))}
              </div>
            )}
            {entry.homepage && (
              <a
                className="mt-1.5 inline-flex items-center gap-1 text-[11.5px] text-accent hover:underline"
                href={entry.homepage}
                target="_blank"
                rel="noreferrer"
                onClick={(e) => e.stopPropagation()}
              >
                <Link2 size={11} />
                {entry.homepage}
              </a>
            )}
            {installMut.isError && installMut.variables === entry.pluginId && (
              <p className="mt-1.5 text-[12px] text-danger">
                {t('market.installFailed', { error: translateBackendMessage(installMut.error.message) })}
              </p>
            )}
          </div>
          <button
            type="button"
            className={cn('btn sm flex-shrink-0', installed ? 'ghost' : 'primary')}
            onClick={(e) => {
              e.stopPropagation()
              installMut.mutate(entry.pluginId, { onSuccess: onInstalled })
            }}
            disabled={installed || pending}
          >
            {installed ? (
              <>
                <Check size={14} />
                {t('market.installed')}
              </>
            ) : pending ? (
              t('common.loading')
            ) : (
              <>
                <Download size={14} />
                {t('market.install')}
              </>
            )}
          </button>
        </div>
      </div>
      <PluginDetailDialog
        open={detailOpen}
        onOpenChange={setDetailOpen}
        detail={{
          name: label,
          version: entry.version,
          identity: entry.pluginId,
          description: entry.description,
          homepage: entry.homepage,
          author: entry.author,
          tags: entry.tags,
          publishTime: entry.publishTime,
          minAppVersion: entry.minAppVersion,
          permissions: entry.permissions,
          yankedLabel,
        }}
      />
    </>
  )
}

interface PluginDetail {
  name: string
  version: string
  identity: string
  description: string
  homepage: string
  author?: string
  tags?: string[]
  publishTime?: string
  minAppVersion?: string
  settingsCount?: number
  permissions?: string[]
  yankedLabel?: string | null
}

// 插件详情对话框：已安装插件与市场条目共用（点击卡片弹出，逻辑对齐桌面客户端）。
// 展示 manifest 级基础信息 + 完整描述（不截断）+ 权限说明 + 通用使用须知。
function PluginDetailDialog({
  open,
  onOpenChange,
  detail,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  detail: PluginDetail
}) {
  const { t } = useI18n()
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="wbackdrop show" />
        <Dialog.Content asChild>
          <div className="dialog show">
            <header className="dlg-head">
              <Dialog.Title asChild>
                <div className="flex min-w-0 flex-wrap items-center gap-2">
                  <b className="truncate">{detail.name || detail.identity}</b>
                  <span className="text-[12px] font-normal text-text3">v{detail.version}</span>
                  {detail.yankedLabel && <Badge tone="danger">{detail.yankedLabel}</Badge>}
                </div>
              </Dialog.Title>
              <Dialog.Close asChild>
                <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                  <X size={16} />
                </button>
              </Dialog.Close>
            </header>
            <Dialog.Description className="sr-only">{detail.description || detail.name}</Dialog.Description>
            <div className="dlg-body">
              <div className="flex flex-col gap-2 text-[12.5px]">
                <InfoRow label={t('plugins.detailIdentity')} value={detail.identity} />
                {detail.author && <InfoRow label={t('plugins.detailAuthor')} value={detail.author} />}
                {detail.publishTime && <InfoRow label={t('plugins.detailPublishTime')} value={detail.publishTime} />}
                {detail.minAppVersion && <InfoRow label={t('plugins.detailMinAppVersion')} value={detail.minAppVersion} />}
                {detail.settingsCount != null && detail.settingsCount > 0 && (
                  <InfoRow
                    label={t('plugins.detailSettings')}
                    value={t('plugins.detailSettingsCount', { count: detail.settingsCount })}
                  />
                )}
                {detail.homepage && <InfoRow label={t('plugins.detailHomepage')} value={detail.homepage} link />}
              </div>
              {detail.tags && detail.tags.length > 0 && (
                <div className="mt-3 flex flex-wrap gap-1.5">
                  {detail.tags.map((tag) => (
                    <Badge key={tag} tone="neutral">
                      {tag}
                    </Badge>
                  ))}
                </div>
              )}
              {detail.permissions && detail.permissions.length > 0 && (
                <div className="mt-4">
                  <p className="mb-1.5 text-[11px] font-semibold uppercase tracking-wide text-text3">
                    {t('plugins.detailPermissions')}
                  </p>
                  <div className="flex flex-col gap-1.5">
                    {detail.permissions.map((perm) => (
                      <PermissionRow key={perm} perm={perm} />
                    ))}
                  </div>
                </div>
              )}
              {detail.description && (
                <div className="mt-4">
                  <p className="mb-1.5 text-[11px] font-semibold uppercase tracking-wide text-text3">
                    {t('plugins.detailDescription')}
                  </p>
                  <p className="whitespace-pre-wrap text-[12.5px] leading-relaxed text-text2">{detail.description}</p>
                </div>
              )}
              <div className="mt-4">
                <p className="mb-1.5 text-[11px] font-semibold uppercase tracking-wide text-text3">
                  {t('plugins.detailUsage')}
                </p>
                <p className="text-[12.5px] leading-relaxed text-text2">{t('plugins.detailUsageBody')}</p>
              </div>
            </div>
            <footer className="dlg-foot">
              <Dialog.Close asChild>
                <button type="button" className="btn ghost">
                  {t('common.close')}
                </button>
              </Dialog.Close>
            </footer>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}

function InfoRow({ label, value, link }: { label: string; value: string; link?: boolean }) {
  return (
    <div className="flex items-start gap-3">
      <span className="w-24 flex-shrink-0 text-text3">{label}</span>
      {link ? (
        <a
          className="min-w-0 break-all text-accent hover:underline"
          href={value}
          target="_blank"
          rel="noreferrer"
        >
          {value}
        </a>
      ) : (
        <span className="min-w-0 break-all text-text2">{value}</span>
      )}
    </div>
  )
}

function PermissionRow({ perm }: { perm: string }) {
  const { t } = useI18n()
  const keys = PERMISSION_KEYS[perm]
  return (
    <div className="flex items-start gap-2">
      <ShieldCheck size={13} className="mt-0.5 flex-shrink-0 text-accent" />
      <div className="min-w-0">
        <span className="text-[12.5px] font-medium text-text">{keys ? t(keys.label) : perm}</span>
        <p className="text-[11.5px] leading-relaxed text-text3">
          {keys ? t(keys.desc) : t('plugins.permUnknownDesc')}
        </p>
      </div>
    </div>
  )
}
