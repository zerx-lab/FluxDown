// 设置控件与「绑定到配置键」的行（GPUI `SectionContext::daemon_* / pref_*`）。

import { useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { daemonConfigField } from '../../../lib/rpc'
import { Input, Select, Switch } from '../../../ui'
import type { SelectOption } from '../../../ui'
import { SettingsRow } from './layout'
import { WIDTH_DROPDOWN, WIDTH_INPUT, WIDTH_INPUT_WIDE, WIDTH_NUMBER, camel, formatNumber, optionalText } from './util'
import {
  setDaemon,
  setDaemonBool,
  setDaemonNumber,
  setPref,
  useDaemonBool,
  useDaemonNumber,
  useDaemonValue,
  usePrefBoolean,
  usePrefString,
} from './writeStore'

// ── 基础控件 ──

/**
 * 数字输入：草稿文本本地维护，解析成功即提交（夹到 min..max），失焦时回显夹取后的值。
 * `unit` 固定单位后缀。
 */
export function NumberField({
  value,
  min,
  max,
  step = 1,
  unit,
  onCommit,
  disabled,
  className,
  float = false,
  'aria-label': ariaLabel,
}: {
  value: number
  min?: number
  max?: number
  step?: number
  unit?: string
  onCommit: (value: number) => void
  disabled?: boolean
  className?: string
  float?: boolean
  'aria-label'?: string
}) {
  const [draft, setDraft] = useState<string | null>(null)
  const clamp = (n: number) => Math.min(max ?? Number.POSITIVE_INFINITY, Math.max(min ?? Number.NEGATIVE_INFINITY, n))
  const shown = draft ?? formatNumber(value)
  return (
    <div className={cn('inline-flex items-center gap-2', unit ? 'mobile:w-full' : WIDTH_NUMBER, className)}>
      <Input
        type="number"
        inputMode={float ? 'decimal' : 'numeric'}
        aria-label={ariaLabel}
        value={shown}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        className={cn('min-w-0 tabular', unit ? 'flex-1 desktop:w-[104px] desktop:flex-none' : 'flex-1')}
        onFocus={() => setDraft(formatNumber(value))}
        onChange={(event) => {
          const text = event.target.value
          setDraft(text)
          // 输入中只提交范围内的值；删空、输到一半、越界的草稿留待失焦再钳制。
          const parsed = Number(text)
          if (text.trim() === '' || !Number.isFinite(parsed)) return
          const next = float ? parsed : Math.round(parsed)
          if (next === clamp(next) && next !== value) onCommit(next)
        }}
        onBlur={() => {
          const text = draft ?? ''
          const parsed = Number(text)
          setDraft(null)
          if (text.trim() === '' || !Number.isFinite(parsed)) return
          const next = float ? clamp(parsed) : clamp(Math.round(parsed))
          if (next !== value) onCommit(next)
        }}
      />
      {unit ? <span className="shrink-0 text-xs text-muted-foreground">{unit}</span> : null}
    </div>
  )
}

/** 文本输入：草稿本地维护，逐字提交；外部值变化且未聚焦时回显。 */
export function TextField({
  value,
  onCommit,
  placeholder,
  disabled,
  className,
  type = 'text',
  invalid,
  trailing,
  'aria-label': ariaLabel,
}: {
  value: string
  onCommit: (value: string) => void
  placeholder?: string
  disabled?: boolean
  className?: string
  type?: 'text' | 'password' | 'url'
  invalid?: boolean
  trailing?: ReactNode
  'aria-label'?: string
}) {
  const [draft, setDraft] = useState(value)
  const focused = useRef(false)
  useEffect(() => {
    if (!focused.current) setDraft(value)
  }, [value])
  return (
    <Input
      type={type}
      aria-label={ariaLabel}
      value={draft}
      placeholder={placeholder}
      disabled={disabled}
      invalid={invalid}
      trailing={trailing}
      autoComplete="off"
      spellCheck={false}
      className={cn(WIDTH_INPUT, className)}
      onFocus={() => {
        focused.current = true
      }}
      onChange={(event) => {
        setDraft(event.target.value)
        onCommit(event.target.value)
      }}
      onBlur={() => {
        focused.current = false
        setDraft(value)
      }}
    />
  )
}

/** 下拉控件（outline + caret）。 */
export function DropdownField<V extends string>({
  value,
  options,
  onValueChange,
  disabled,
  className,
  'aria-label': ariaLabel,
}: {
  value: V | ''
  options: readonly SelectOption<V>[]
  onValueChange: (value: V) => void
  disabled?: boolean
  className?: string
  'aria-label'?: string
}) {
  return <Select value={value} options={options} onValueChange={onValueChange} disabled={disabled} aria-label={ariaLabel} className={cn(WIDTH_DROPDOWN, className)} />
}

// ── 绑定到 daemon 配置键的行 ──

interface RowText {
  titleKey: string
  descKey?: string
}

function useRowText({ titleKey, descKey }: RowText) {
  const t = useT()
  return { t, title: t(titleKey), description: optionalText(t, descKey) }
}

export function DaemonSwitchRow({ configKey, titleKey, descKey, disabled }: RowText & { configKey: string; disabled?: boolean }) {
  const { title, description } = useRowText({ titleKey, descKey })
  const checked = useDaemonBool(configKey)
  return (
    <SettingsRow title={title} description={description} compact disabled={disabled}>
      <Switch checked={checked} onCheckedChange={(next) => setDaemonBool(configKey, next)} aria-label={title} />
    </SettingsRow>
  )
}

/** 整数/浮点配置：范围取自 `DAEMON_CONFIG_FIELDS`。 */
export function DaemonNumberRow({
  configKey,
  titleKey,
  descKey,
  unit,
  step,
  disabled,
  footer,
}: RowText & { configKey: string; unit?: string; step?: number; disabled?: boolean; footer?: (value: number) => ReactNode }) {
  const { title, description } = useRowText({ titleKey, descKey })
  const value = useDaemonNumber(configKey)
  const field = daemonConfigField(configKey)
  const float = field?.kind === 'float'
  return (
    <SettingsRow title={title} description={description} disabled={disabled} {...(footer ? { footer: footer(value) } : {})}>
      <NumberField
        value={value}
        {...(field?.min !== undefined ? { min: field.min } : {})}
        {...(field?.max !== undefined ? { max: field.max } : {})}
        step={step ?? 1}
        float={float}
        {...(unit ? { unit } : {})}
        aria-label={title}
        onCommit={(next) => setDaemonNumber(configKey, next)}
      />
    </SettingsRow>
  )
}

export function DaemonTextRow({
  configKey,
  titleKey,
  descKey,
  placeholder,
  vertical,
  wide,
  disabled,
}: RowText & { configKey: string; placeholder?: string; vertical?: boolean; wide?: boolean; disabled?: boolean }) {
  const { title, description } = useRowText({ titleKey, descKey })
  const value = useDaemonValue(configKey)
  return (
    <SettingsRow title={title} description={description} vertical={vertical ?? false} disabled={disabled}>
      <TextField value={value} placeholder={placeholder} aria-label={title} className={cn(vertical && 'w-full desktop:w-full', wide && WIDTH_INPUT_WIDE)} onCommit={(next) => setDaemon(configKey, next)} />
    </SettingsRow>
  )
}

/** 枚举下拉：选项取自协议目录。文案优先 `labels`，否则 `{labelPrefix}{CamelValue}`。 */
export function DaemonEnumRow({
  configKey,
  titleKey,
  descKey,
  labelPrefix,
  labels,
  disabled,
}: RowText & {
  configKey: string
  /** i18n 键前缀；与 `labels` 二选一。 */
  labelPrefix?: string
  /** 显式选项文案（技术名等不走 i18n）。缺省项回退 `labelPrefix` 或原值。 */
  labels?: Readonly<Record<string, string>>
  disabled?: boolean
}) {
  const { t, title, description } = useRowText({ titleKey, descKey })
  const value = useDaemonValue(configKey)
  const options = (daemonConfigField(configKey)?.options ?? []).map((option) => ({
    value: option,
    label: labels?.[option] ?? (labelPrefix !== undefined ? t(`${labelPrefix}${camel(option)}`) : option),
  }))
  return (
    <SettingsRow title={title} description={description} disabled={disabled}>
      <DropdownField value={value} options={options} aria-label={title} onValueChange={(next) => setDaemon(configKey, next)} />
    </SettingsRow>
  )
}

// ── 绑定到 agent 偏好的行 ──

export function PrefSwitchRow({ prefKey, fallback, titleKey, descKey, disabled, immediate }: RowText & { prefKey: string; fallback: boolean; disabled?: boolean; immediate?: boolean }) {
  const { title, description } = useRowText({ titleKey, descKey })
  const checked = usePrefBoolean(prefKey, fallback)
  return (
    <SettingsRow title={title} description={description} compact disabled={disabled}>
      <Switch checked={checked} onCheckedChange={(next) => setPref(prefKey, next, { immediate: immediate === true })} aria-label={title} />
    </SettingsRow>
  )
}

export function PrefDropdownRow<V extends string>({
  prefKey,
  fallback,
  titleKey,
  descKey,
  options,
  immediate,
}: RowText & { prefKey: string; fallback: V; options: readonly SelectOption<V>[]; immediate?: boolean }) {
  const { title, description } = useRowText({ titleKey, descKey })
  const value = usePrefString(prefKey, fallback)
  return (
    <SettingsRow title={title} description={description}>
      <DropdownField value={value as V} options={options} aria-label={title} onValueChange={(next) => setPref(prefKey, next, { immediate: immediate === true })} />
    </SettingsRow>
  )
}
