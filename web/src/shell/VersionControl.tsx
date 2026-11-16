// 状态栏版本条目（chrome 布局 `appVersion`，GPUI `crates/settings/src/version_button.rs`）：平时显示当前版本号，
// 有新版本时就地变成更新入口——更新提示与版本号是同一个控件（桌面不再显示顶部更新横幅）。
// 单击展开菜单：当前状态对应的更新动作 / 跳过此版本、检查更新、复制诊断信息、导出日志、关于。

import { useNavigate } from '@tanstack/react-router'
import { CircleArrowDown, Copy, Download, FileDown, Info, LoaderCircle, RotateCw, TriangleAlert, X } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useState } from 'react'
import { useI18n, useT } from '../i18n'
import type { TFunction } from '../i18n'
import { cn } from '../lib/cn'
import { copyText } from '../lib/copy'
import { diagnosticText } from '../lib/diagnosticInfo'
import { exportLogs, rpc, useConnection, useRpcSelector } from '../lib/rpc'
import { rpcErrorText } from '../lib/rpcErrorText'
import { failureKey, ignoreVersion, statusLine, updateLineText, useUpdateStatus, useVersionBadge } from '../lib/update'
import type { VersionBadge } from '../lib/update'
import { popoverSide, resolveSide } from '../pages/downloads/statusbar/widget'
import type { WidgetProps } from '../pages/downloads/statusbar/widget'
import { usePrefString } from '../pages/settings/kit/writeStore'
import { Icon, Popover, Tooltip, toast } from '../ui'

function MenuRow({ icon, label, onClick, disabled }: { icon: LucideIcon; label: string; onClick: () => void; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="menuitem"
      disabled={disabled}
      onClick={onClick}
      className="flex min-h-control w-full items-center gap-2 rounded-sm px-2 py-1 text-left text-sm text-foreground hover:bg-nav-hover disabled:pointer-events-none disabled:opacity-50 coarse:min-h-touch"
    >
      <Icon icon={icon} className="text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate">{label}</span>
    </button>
  )
}

/** 有更新状态时版本号前的图标与后缀。 */
function badgeLook(t: TFunction, badge: VersionBadge): { icon: LucideIcon; suffix: string } {
  switch (badge.kind) {
    case 'available':
      return { icon: CircleArrowDown, suffix: `→ v${badge.version}` }
    case 'downloading':
      return { icon: Download, suffix: `→ v${badge.version} · ${badge.percent}%` }
    case 'installing':
      return { icon: LoaderCircle, suffix: t('updateTitleInstalling') }
    case 'ready':
      return { icon: RotateCw, suffix: t('updateTitleReady') }
    case 'failed':
      return { icon: TriangleAlert, suffix: t('updateTitleFailed') }
  }
}

export function VersionControl({ orientation = 'horizontal', side }: WidgetProps) {
  const t = useT()
  const { locale } = useI18n()
  const navigate = useNavigate()
  const hello = useRpcSelector((state) => state.hello)
  const status = useUpdateStatus()
  const badge = useVersionBadge()
  const channel = usePrefString('general.update_channel', 'stable')
  const connected = useConnection().phase === 'ready'
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)

  const version = hello?.serviceVersion || status?.currentVersion || ''
  const versionLabel = version ? `v${version}` : 'v—'
  const checking = busy || status?.phase === 'checking'
  const look = badge ? badgeLook(t, badge) : null
  const line = status ? statusLine(status) : null
  const title = `FluxDown ${versionLabel}`
  const tooltip = badge?.kind === 'failed'
    ? `${title} · ${t(failureKey(badge.failure))}`
    : line ? `${title} · ${updateLineText(t, line)}` : title
  const channelLabel = t(channel === 'frontier' ? 'updateChannelFrontier' : 'updateChannelStable')
  const subtitle = line ? `${channelLabel} · ${updateLineText(t, line)}` : channelLabel

  const run = async (action: () => Promise<unknown>) => {
    if (busy) return
    setBusy(true)
    try {
      await action()
    } catch (error) {
      toast.error(rpcErrorText(error, t))
    } finally {
      setBusy(false)
    }
  }
  const openAbout = () => void navigate({ to: '/settings/$category', params: { category: 'about' } })
  const copyDiagnostics = () => {
    copyText(diagnosticText({ serviceVersion: hello?.serviceVersion ?? '', protocolVersion: hello?.protocolVersion ?? null, status, locale, userAgent: navigator.userAgent }))
    toast.success(t('diagnosticInfoCopied'))
  }

  // 有更新用强调色，失败用警示色；只显示版本号时与其它状态栏读数同为次要文字色。
  const tone = !badge ? 'text-muted-foreground hover:text-foreground' : badge.kind === 'failed' ? 'bg-warning/10 text-warning' : 'bg-accent-text/12 text-accent-text'
  const leading = look?.icon ?? (checking ? LoaderCircle : null)

  return (
    <Popover
      title={title}
      side={popoverSide(orientation)}
      align="end"
      open={open}
      onOpenChange={setOpen}
      trigger={
        <Tooltip content={tooltip} side={resolveSide(orientation, side)}>
          <button
            type="button"
            aria-label={tooltip}
            aria-haspopup="menu"
            aria-expanded={open}
            className={cn(
              'inline-flex h-status-control min-w-status-control max-w-64 shrink-0 items-center justify-center gap-1 whitespace-nowrap rounded-sm px-1.5 text-caption tabular-nums hover:bg-nav-hover coarse:min-h-touch',
              tone,
            )}
          >
            {leading ? <Icon icon={leading} className={leading === LoaderCircle ? 'animate-spin' : undefined} /> : null}
            <span>{versionLabel}</span>
            {look ? <span className="min-w-0 truncate">{look.suffix}</span> : null}
          </button>
        </Tooltip>
      }
    >
      {(close) => {
        const act = (then: () => void) => () => {
          close()
          then()
        }
        return (
          <div role="menu" aria-label={title} className="flex w-64 max-w-full flex-col p-1">
            <div className="px-2 pb-1.5 pt-1">
              <div className="truncate text-sm tabular-nums text-foreground">{title}</div>
              <div className="truncate text-caption text-muted-foreground" title={subtitle}>
                {subtitle}
              </div>
            </div>
            {badge && badge.kind !== 'downloading' && badge.kind !== 'installing' ? (
              <div className="flex flex-col border-t border-hairline pt-1">
                {badge.kind === 'available' && !badge.manual ? (
                  <MenuRow icon={Download} label={t('downloadUpdateVersion', { v: badge.version })} disabled={!connected} onClick={act(() => void run(() => rpc.agent.update.download()))} />
                ) : badge.kind === 'ready' ? (
                  <MenuRow icon={RotateCw} label={t('updateTitleReady')} disabled={!connected} onClick={act(() => void run(() => rpc.agent.update.install()))} />
                ) : (
                  <MenuRow icon={CircleArrowDown} label={t('updateViewDetails')} onClick={act(openAbout)} />
                )}
                {badge.kind === 'available' ? <MenuRow icon={X} label={t('skipThisVersion')} onClick={act(() => ignoreVersion(badge.version))} /> : null}
              </div>
            ) : null}
            <div className="mt-1 flex flex-col border-t border-hairline pt-1">
              <MenuRow
                icon={RotateCw}
                label={t('checkUpdate')}
                disabled={!connected || checking}
                onClick={act(() => void run(() => rpc.agent.update.check({ channel: channel === 'frontier' ? 'frontier' : 'stable' })))}
              />
              <MenuRow icon={Copy} label={t('copyDiagnosticInfo')} onClick={act(copyDiagnostics)} />
              <MenuRow icon={FileDown} label={t('logExportButton')} disabled={!connected} onClick={act(() => void run(exportLogs))} />
              <MenuRow icon={Info} label={t('menuAbout')} onClick={act(openAbout)} />
            </div>
          </div>
        )
      }}
    </Popover>
  )
}
