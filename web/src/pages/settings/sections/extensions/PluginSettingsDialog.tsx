// 单个插件的设置对话框：按 widget 类型分发输入控件，提交前做
// required / number / min-max / select 前置校验，全部通过才发起 `daemon.plugin.updateSettings`
// （`pattern` 由 daemon 侧校验，失败时按 error.field 标红对应字段）。
// folder 控件在 Web 没有本机目录选择器，退化为普通路径输入（路径在服务端主机上）。

import { Copy, Eye, EyeOff } from 'lucide-react'
import { useId, useState } from 'react'
import type { ReactNode } from 'react'
import { useI18n, useT } from '../../../../i18n'
import type { TFunction } from '../../../../i18n'
import { copyText } from '../../../../lib/copy'
import { RpcError, rpc } from '../../../../lib/rpc'
import type { PluginDto, SettingFieldDto } from '../../../../lib/rpc'
import { Button, Dialog, DialogFooter, FieldError, FormField, Icon, Input, OptionGroup, OptionRow, SegmentedTabs, Select, Switch, Textarea, toast } from '../../../../ui'
import { activeSettingsViewOf } from '../../../plugins/contributions'
import { PluginViewHost } from '../../../plugins/view/PluginViewHost'
import { extensionErrorText } from './errors'
import { initialFieldValue, localizedSettingField, rangeHint, validateField } from './logic'
import type { FieldError as FieldErrorInfo } from './logic'

function fieldErrorText(t: TFunction, error: FieldErrorInfo): string {
  switch (error.kind) {
    case 'required':
      return t('pluginErrRequired')
    case 'number':
      return t('pluginErrNumber')
    case 'min':
      return t('pluginErrMin', { min: error.min })
    case 'max':
      return t('pluginErrMax', { max: error.max })
    case 'select':
      return t('pluginErrSelect')
    case 'server':
      return error.message
  }
}

function PasswordInput({ id, value, onChange, invalid, disabled }: { id: string; value: string; onChange: (value: string) => void; invalid: boolean; disabled: boolean }) {
  const t = useT()
  const [visible, setVisible] = useState(false)
  return (
    <Input
      id={id}
      type={visible ? 'text' : 'password'}
      autoComplete="off"
      value={value}
      invalid={invalid}
      disabled={disabled}
      className="coarse:pr-12"
      onChange={(event) => onChange(event.target.value)}
      trailing={
        <Button
          variant="ghost"
          iconOnly
          className="text-muted-foreground hover:text-foreground"
          aria-label={visible ? t('webHidePassword') : t('webShowPassword')}
          onClick={() => setVisible((current) => !current)}
          disabled={disabled}
        >
          <Icon icon={visible ? EyeOff : Eye} size="md" />
        </Button>
      }
    />
  )
}

interface ControlProps {
  id: string
  field: SettingFieldDto
  value: string
  invalid: boolean
  disabled: boolean
  onChange: (value: string) => void
}

/** 非开关控件（开关走 OptionRow）。 */
function FieldControl({ id, field, value, invalid, disabled, onChange }: ControlProps) {
  const t = useT()
  switch (field.widget) {
    case 'select': {
      const known = field.options.some((option) => option.value === value)
      const options = field.options.map((option) => ({ value: option.value, label: option.label }))
      // 已保存值不在选项里时照原样显示（提交前会被 select 校验拦下并提示）。
      if (!known && value !== '') options.push({ value, label: value })
      return (
        <Select
          id={id}
          value={value}
          onValueChange={onChange}
          options={options}
          placeholder={t('pluginSelectPlaceholder')}
          disabled={disabled}
        />
      )
    }
    case 'textarea':
      return <Textarea id={id} value={value} invalid={invalid} disabled={disabled} onChange={(event) => onChange(event.target.value)} />
    case 'password':
      return <PasswordInput id={id} value={value} onChange={onChange} invalid={invalid} disabled={disabled} />
    case 'folder':
      return (
        <Input
          id={id}
          value={value}
          invalid={invalid}
          disabled={disabled}
          placeholder={t('pluginFolderPickPlaceholder')}
          onChange={(event) => onChange(event.target.value)}
        />
      )
    default: {
      const placeholder = field.type === 'number' ? (rangeHint(field) ?? '') : (field.default ?? '')
      return (
        <Input
          id={id}
          value={value}
          invalid={invalid}
          disabled={disabled}
          placeholder={placeholder}
          inputMode={field.type === 'number' ? 'decimal' : undefined}
          onChange={(event) => onChange(event.target.value)}
        />
      )
    }
  }
}

function SettingsBody({ plugin, onClose }: { plugin: PluginDto; onClose: () => void }) {
  const { locale } = useI18n()
  const t = useT()
  const baseId = useId()
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(plugin.settings.map((field) => [field.key, initialFieldValue(field, plugin.settingsValues)])),
  )
  const [errors, setErrors] = useState<Record<string, FieldErrorInfo>>({})
  const [serverError, setServerError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  // 插件的设置视图（可移植实现）：有则以分段标签与「基础设置」表单并列。
  const settingsView = activeSettingsViewOf(plugin)
  const hasBasic = plugin.settings.length > 0
  const [tab, setTab] = useState<'view' | 'basic'>(settingsView ? 'view' : 'basic')
  const activeTab = settingsView && hasBasic ? tab : settingsView ? 'view' : 'basic'

  const setValue = (key: string, value: string) => {
    setValues((current) => ({ ...current, [key]: value }))
    setErrors((current) => {
      if (!(key in current)) return current
      const { [key]: _removed, ...rest } = current
      return rest
    })
  }

  const submit = async () => {
    if (saving) return
    const nextErrors: Record<string, FieldErrorInfo> = {}
    for (const field of plugin.settings) {
      const error = validateField(field, values[field.key] ?? '')
      if (error) nextErrors[field.key] = error
    }
    setServerError(null)
    setErrors(nextErrors)
    if (Object.keys(nextErrors).length > 0) return
    setSaving(true)
    try {
      await rpc.daemon.plugin.updateSettings({ identity: plugin.identity, entries: values })
      onClose()
    } catch (error) {
      const message = extensionErrorText(t, error)
      setServerError(t('pluginSettingsSaveFailed', { message }))
      const field = error instanceof RpcError ? error.field : undefined
      if (field && plugin.settings.some((known) => known.key === field)) {
        setErrors({ [field]: { kind: 'server', message } })
      }
      setSaving(false)
    }
  }

  const copyHelper = (script: string) => {
    copyText(script)
    toast.success(t('pluginHelperScriptCopied'))
  }

  // 连续的开关字段合并进同一个 OptionGroup；其余字段逐个 FormField。
  const blocks: ReactNode[] = []
  let toggles: ReactNode[] = []
  const flushToggles = () => {
    if (toggles.length === 0) return
    blocks.push(<OptionGroup key={`toggles-${blocks.length}`}>{toggles}</OptionGroup>)
    toggles = []
  }
  for (const field of plugin.settings) {
    const display = localizedSettingField(field, locale)
    const id = `${baseId}-${field.key}`
    const title = display.title === '' ? field.key : display.title
    const value = values[field.key] ?? ''
    const error = errors[field.key]
    const errorNode = error ? <FieldError>{fieldErrorText(t, error)}</FieldError> : null
    const helper = field.helperScript ? (
      <div>
        <Button variant="outline" icon={Copy} onClick={() => copyHelper(field.helperScript ?? '')}>
          {display.helperLabel ? display.helperLabel : t('pluginCopyHelperScript')}
        </Button>
      </div>
    ) : null
    if (field.widget === 'toggle') {
      toggles.push(
        <OptionRow
          key={field.key}
          title={title}
          description={display.description || undefined}
          control={<Switch checked={value === 'true'} disabled={saving} aria-label={title} onCheckedChange={(checked) => setValue(field.key, checked ? 'true' : 'false')} />}
        />,
      )
      if (!helper && !errorNode) continue
      blocks.push(
        <div key={field.key} className="flex w-full flex-col gap-1.5">
          <OptionGroup>{toggles}</OptionGroup>
          {helper}
          {errorNode}
        </div>,
      )
      toggles = []
      continue
    }
    flushToggles()
    blocks.push(
      <div key={field.key} className="flex w-full flex-col gap-1.5">
        <FormField label={title} htmlFor={id} hint={display.description || undefined}>
          <div className="flex w-full min-w-0 flex-col gap-1.5">
            <FieldControl id={id} field={display} value={value} invalid={error !== undefined} disabled={saving} onChange={(next) => setValue(field.key, next)} />
            {helper}
          </div>
        </FormField>
        {errorNode}
      </div>,
    )
  }
  flushToggles()

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && !saving && onClose()}
      modalLocked={saving}
      title={t('pluginSettingsDialogTitle', { name: plugin.name })}
      size={activeTab === 'view' ? 'lg' : undefined}
      footer={
        activeTab === 'view' ? (
          <DialogFooter>
            <Button variant="outline" onClick={onClose}>
              {t('close')}
            </Button>
          </DialogFooter>
        ) : (
          <DialogFooter>
            <Button variant="outline" onClick={onClose} disabled={saving}>
              {t('cancel')}
            </Button>
            <Button variant="primary" onClick={() => void submit()} loading={saving}>
              {saving ? t('pluginSettingsSaving') : t('pluginSettingsSaveButton')}
            </Button>
          </DialogFooter>
        )
      }
    >
      {settingsView && hasBasic ? (
        <div className="flex pb-2">
          <SegmentedTabs
            items={[
              { value: 'view' as const, label: t('pluginSettingsViewTab') },
              { value: 'basic' as const, label: t('pluginSettingsBasicTab') },
            ]}
            value={activeTab}
            onValueChange={setTab}
          />
        </div>
      ) : null}
      {settingsView && activeTab === 'view' ? <PluginViewHost identity={plugin.identity} view={settingsView.id} onClose={onClose} /> : null}
      <div className={activeTab === 'basic' ? 'flex w-full flex-col gap-4 py-1' : 'hidden'}>
        {serverError ? (
          <div role="alert" className="rounded-md bg-destructive/10 px-3 py-2 text-xs text-destructive">
            {serverError}
          </div>
        ) : null}
        {blocks}
      </div>
    </Dialog>
  )
}

/**
 * `plugin` 取最新快照，但表单只在挂载时按当时的设置值建立（GPUI 同样在打开瞬间建表单，之后快照更新不覆盖用户
 * 输入）。插件包被替换（更新 / dev 重载，`packageRevision` 变化）后设置声明可能已变，按新声明重建表单。
 */
export function PluginSettingsDialog({ plugin, onClose }: { plugin: PluginDto | null; onClose: () => void }) {
  return plugin ? <SettingsBody key={`${plugin.identity}#${plugin.packageRevision ?? 0}`} plugin={plugin} onClose={onClose} /> : null
}
