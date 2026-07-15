// 单个插件的设置表单 —— 按 widget 分发既有 controls.tsx 行组件；提交前做
// required/pattern/min-max/select 成员前置校验（全部通过才发起 PUT，避免 all-or-nothing 请求半路失败）。

import { Fragment, useEffect, useRef, useState } from 'react'
import { Check, ClipboardCopy } from 'lucide-react'
import type { I18nKey } from '../../lib/i18n'
import { useI18n } from '../../lib/i18n'
import type { PluginDto, SettingFieldDto } from '../../lib/types'
import { NumberFieldRow, SetRow, SetSelect, SetSwitch, TextAreaFieldRow, TextFieldRow } from './controls'

export interface PluginSettingFormProps {
  plugin: PluginDto
  saving?: boolean
  onSave: (entries: Record<string, string>) => void
}

/** 单条设置项前置校验：required → number/min/max → pattern → select 成员。首个失败项即返回。 */
function validateField(field: SettingFieldDto, raw: string): I18nKey | null {
  const value = raw.trim()
  if (field.required && value === '') return 'plugins.err.required'
  if (value === '') return null
  if (field.type === 'number') {
    const n = Number(value)
    if (!Number.isFinite(n)) return 'plugins.err.number'
    if (field.min !== null && n < field.min) return 'plugins.err.min'
    if (field.max !== null && n > field.max) return 'plugins.err.max'
  }
  if (field.pattern) {
    let ok = true
    try {
      ok = new RegExp(field.pattern).test(value)
    } catch {
      ok = true // 插件提供的正则非法：不阻塞提交
    }
    if (!ok) return 'plugins.err.pattern'
  }
  if (field.widget === 'select' && field.options.length > 0 && !field.options.some((o) => o.value === value)) {
    return 'plugins.err.select'
  }
  return null
}

export function PluginSettingForm({ plugin, saving, onSave }: PluginSettingFormProps) {
  const { t } = useI18n()
  const [values, setValues] = useState<Record<string, string>>(() => ({ ...plugin.settingsValues }))
  const [errors, setErrors] = useState<Partial<Record<string, I18nKey>>>({})

  function valueOf(field: SettingFieldDto): string {
    return values[field.key] ?? field.default ?? ''
  }

  function setValue(key: string, v: string) {
    setValues((prev) => ({ ...prev, [key]: v }))
    setErrors((prev) => {
      if (!(key in prev)) return prev
      const next = { ...prev }
      delete next[key]
      return next
    })
  }

  function submit() {
    const nextErrors: Partial<Record<string, I18nKey>> = {}
    for (const field of plugin.settings) {
      const err = validateField(field, valueOf(field))
      if (err) nextErrors[field.key] = err
    }
    setErrors(nextErrors)
    if (Object.keys(nextErrors).length > 0) return
    onSave(values)
  }

  if (plugin.settings.length === 0) return null

  return (
    <div className="overflow-hidden rounded-lg border border-line bg-surface2">
      {plugin.settings.map((field) => (
        <Fragment key={field.key}>
          <SettingFieldRow field={field} value={valueOf(field)} onChange={(v) => setValue(field.key, v)} />
          {field.helperScript && <HelperScriptButton field={field} />}
          {errors[field.key] && <p className="px-4 pb-2 text-[11px] text-danger">{t(errors[field.key]!)}</p>}
        </Fragment>
      ))}
      <div className="flex justify-end border-t border-line p-3">
        <button type="button" className="btn primary sm" disabled={saving} onClick={submit}>
          {saving ? t('common.loading') : t('plugins.saveSettings')}
        </button>
      </div>
    </div>
  )
}

function SettingFieldRow({
  field,
  value,
  onChange,
}: {
  field: SettingFieldDto
  value: string
  onChange: (v: string) => void
}) {
  const title = (
    <>
      {field.title || field.key}
      {field.required && <span className="ml-0.5 text-danger">*</span>}
    </>
  )
  const desc = field.description || undefined
  switch (field.widget) {
    case 'password':
      return <TextFieldRow title={title} desc={desc} value={value} onCommit={onChange} password />
    case 'textarea':
      return <TextAreaFieldRow title={title} desc={desc} value={value} onCommit={onChange} />
    case 'number':
      return (
        <NumberFieldRow
          title={title}
          desc={desc}
          value={Number(value || '0')}
          onCommit={(n) => onChange(String(n))}
          min={field.min ?? undefined}
          max={field.max ?? undefined}
        />
      )
    case 'toggle':
      return (
        <SetRow title={title} desc={desc}>
          <SetSwitch checked={value === 'true'} onCheckedChange={(v) => onChange(v ? 'true' : 'false')} />
        </SetRow>
      )
    case 'select':
      return (
        <SetRow title={title} desc={desc}>
          <SetSelect value={value} onValueChange={onChange} options={field.options} />
        </SetRow>
      )
    case 'folder':
      return (
        <SetRow title={title} desc={desc}>
          <input className="text-input" readOnly value={value} />
        </SetRow>
      )
    case 'text':
    default:
      return <TextFieldRow title={title} desc={desc} value={value} onCommit={onChange} />
  }
}
/** 字段级辅助脚本复制按钮：仅复制文本到剪贴板（绝不执行），供用户粘贴到目标
 *  网站的开发者工具 Console 运行（典型用途：提取 cookie）。 */
function HelperScriptButton({ field }: { field: SettingFieldDto }) {
  const { t } = useI18n()
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | undefined>(undefined)
  useEffect(() => () => window.clearTimeout(timer.current), [])
  return (
    <div className="px-4 pb-2">
      <button
        type="button"
        className="btn sm"
        onClick={() => {
          void navigator.clipboard.writeText(field.helperScript ?? '')
          setCopied(true)
          window.clearTimeout(timer.current)
          timer.current = window.setTimeout(() => setCopied(false), 2500)
        }}
      >
        {copied ? <Check size={13} /> : <ClipboardCopy size={13} />}
        {copied ? t('plugins.helperCopied') : field.helperLabel || t('plugins.copyHelper')}
      </button>
    </div>
  )
}
