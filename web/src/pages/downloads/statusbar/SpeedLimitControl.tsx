import { Check } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../../../i18n'
import { byteUnit, useFileSizeUnits } from '../../../lib/fileSizeUnits'
import { cn } from '../../../lib/cn'
import { rpc, useConfigValue, usePreferences } from '../../../lib/rpc'
import { toastRpcError } from '../../../lib/rpcToast'
import { SPEED_LIMIT_RECENT_PREF, UPLOAD_LIMIT_RECENT_PREF, pushRecent, readSpeedLimitRecent } from '../../../lib/speedLimitRecent'
import { Button, Icon, Input, Popover, Tooltip } from '../../../ui'
import { setDaemonNumber } from '../../settings/kit/writeStore'
import { formatBytes } from '../model/task'
import { popoverSide, RAIL_BUTTON, resolveSide, useDaemonReady } from './widget'
import type { WidgetProps } from './widget'

/** 限速预设（MB/s），同 GPUI `SPEED_PRESETS_MB`。 */
const SPEED_PRESETS_MB = [1, 5, 10, 50]
const mbToBytes = (mb: number) => mb * 1_048_576
const kbToBytes = (kb: number) => Math.max(0, kb) * 1024

// 写入与设置页同一路径（writeStore.setDaemonNumber）：`speed_limit_bytes` 映射到云同步键
// `download.speed_limit_bytes`，经 agent.preferences.patch 落盘并参与云同步；
// 仅本机生效的 `upload_limit_bytes` 走 daemon.config.patch。

export function SpeedLimitControl({
  icon,
  titleKey,
  configKey,
  orientation = 'horizontal',
  side,
}: {
  icon: LucideIcon
  titleKey: 'speedLimitTitle' | 'uploadLimit'
  configKey: 'speed_limit_bytes' | 'upload_limit_bytes'
} & WidgetProps) {
  const units = useFileSizeUnits()
  const t = useT()
  const disabled = !useDaemonReady()
  const raw = useConfigValue(configKey)
  const parsed = Number.parseInt(raw ?? '', 10)
  const value = Number.isFinite(parsed) ? parsed : 0
  const [draft, setDraft] = useState<string | null>(null)
  const recentPref = configKey === 'upload_limit_bytes' ? UPLOAD_LIMIT_RECENT_PREF : SPEED_LIMIT_RECENT_PREF
  const recent = readSpeedLimitRecent(usePreferences()[recentPref])
  const title = t(titleKey)
  const off = t('statusSpeedLimitOff')
  const label = value <= 0 ? off : `${formatBytes(value, units)}/s`
  const presetHit = value <= 0 || SPEED_PRESETS_MB.some((mb) => mbToBytes(mb) === value)
  const recentHit = recent.some((kbps) => kbToBytes(kbps) === value)
  // 自定义输入框默认回显当前自定义值（KB/s）；用户编辑后以草稿为准。
  const custom = draft ?? (presetHit ? '' : String(Math.max(1, Math.round(value / 1024))))
  const customLabel = presetHit ? t('speedLimitCustom') : t('speedLimitCustomWithValue', { value: `${formatBytes(value, units)}/s` })
  const desc = t(configKey === 'upload_limit_bytes' ? 'uploadLimitDesc' : 'speedLimitDesc')

  const row = (key: string, text: string, checked: boolean, bytes: number, close: () => void, recordKbps?: number) => (
    <button
      key={key}
      type="button"
      disabled={disabled}
      onClick={() => {
        close()
        setDaemonNumber(configKey, bytes)
        if (recordKbps !== undefined) remember(recordKbps)
      }}
      className="flex h-control w-full items-center gap-2 rounded-sm px-2 text-left text-sm hover:bg-nav-hover disabled:opacity-50 coarse:min-h-touch"
    >
      <span className="inline-flex w-3.5 justify-center">{checked ? <Icon icon={Check} /> : null}</span>
      {text}
    </button>
  )

  /** 把自定义值记入最近列表（设备本地偏好，尽力而为）。 */
  const remember = (kbps: number) => {
    const next = pushRecent(recent, kbps)
    if (next) void rpc.agent.preferences.patch({ values: { [recentPref]: next }, sync: false }).catch(toastRpcError)
  }

  const clearRecent = (close: () => void) => {
    close()
    void rpc.agent.preferences.patch({ values: { [recentPref]: null }, sync: false }).catch(toastRpcError)
  }

  const apply = (close: () => void) => {
    const kb = Number.parseInt(custom, 10)
    if (!Number.isFinite(kb)) return
    close()
    setDraft(null)
    setDaemonNumber(configKey, kbToBytes(kb))
    remember(Math.max(0, kb))
  }

  const rail = orientation === 'rail'
  return (
    <Popover
      title={title}
      side={popoverSide(orientation)}
      align="end"
      trigger={
        <Tooltip content={rail ? `${title}: ${label}` : title} side={resolveSide(orientation, side)}>
          <button
            type="button"
            disabled={disabled}
            aria-label={`${title} ${label}`}
            className={cn(
              rail
                ? RAIL_BUTTON
                : 'inline-flex h-status-control shrink-0 items-center justify-center gap-1 whitespace-nowrap rounded-sm px-1.5 text-caption tabular-nums hover:bg-nav-hover disabled:opacity-50 coarse:min-h-touch coarse:min-w-touch',
              value > 0 ? 'text-accent-text' : 'text-muted-foreground',
            )}
          >
            <Icon icon={icon} size={rail ? 'xl' : 'lg'} />
            {/* 窄屏且未限速时只留图标，避免状态栏横向溢出；活动栏恒为纯图标（数值在气泡里）。 */}
            {rail ? null : <span className={cn(value <= 0 && 'narrow:hidden')}>{label}</span>}
          </button>
        </Tooltip>
      }
    >
      {(close) => (
        <div className="flex min-w-56 flex-col p-1">
          {row('off', off, value <= 0, 0, close)}
          {SPEED_PRESETS_MB.map((mb) => row(String(mb), `${mb} ${byteUnit(2, units)}/s`, mbToBytes(mb) === value, mbToBytes(mb), close))}
          {recent.map((kbps) => row(`recent-${kbps}`, `${formatBytes(kbToBytes(kbps), units)}/s`, kbToBytes(kbps) === value, kbToBytes(kbps), close, kbps))}
          <div className="my-1 h-px bg-hairline" />
          <div className="flex flex-col gap-1.5 px-2 py-1">
            <span className="flex items-center gap-2 text-sm">
              <span className="inline-flex w-3.5 justify-center">{!presetHit && !recentHit ? <Icon icon={Check} /> : null}</span>
              {customLabel}
            </span>
            <div className="flex items-center gap-2">
              <Input
                type="number"
                inputMode="numeric"
                min={0}
                value={custom}
                placeholder="0"
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') apply(close)
                }}
                disabled={disabled}
                className="flex-1"
              />
              <span className="text-xs text-muted-foreground">{byteUnit(1, units)}/s</span>
              <Button variant="primary" disabled={disabled || custom.trim() === ''} onClick={() => apply(close)}>
                {t('confirm')}
              </Button>
            </div>
            <span className="text-caption text-text-tertiary">{desc}</span>
          </div>
          {recent.length > 0 ? (
            <>
              <div className="my-1 h-px bg-hairline" />
              <button
                type="button"
                disabled={disabled}
                onClick={() => clearRecent(close)}
                className="flex h-control w-full items-center gap-2 rounded-sm px-2 text-left text-sm text-muted-foreground hover:bg-nav-hover disabled:opacity-50 coarse:min-h-touch"
              >
                <span className="inline-flex w-3.5" />
                {t('speedLimitClearRecent')}
              </button>
            </>
          ) : null}
        </div>
      )}
    </Popover>
  )
}
