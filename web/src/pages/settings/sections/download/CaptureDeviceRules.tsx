import { useMemo, useState } from 'react'
import { useT } from '../../../../i18n'
import { useAgent } from '../../../../lib/rpc'
import type { CloudDevice, LinkDeviceInfo } from '../../../../lib/rpc'
import { Button } from '../../../../ui'
import { buildRemoteTargets, resolveTargetValue } from '../../../downloads/dialogs/target'
import { DropdownField, SettingsRow, TextField, setPref, usePrefRaw, useSettingsReadOnly } from '../../kit'

const PREF = 'download.capture_device_rules'
const NO_TARGET = '__none__'
const NO_CLOUD: readonly CloudDevice[] = []
const NO_LINKED: readonly LinkDeviceInfo[] = []

export function CaptureDeviceRules() {
  const t = useT()
  const raw = usePrefRaw(PREF)
  const readOnly = useSettingsReadOnly()
  const cloud = useAgent((snapshot) => snapshot.cloudDevices, NO_CLOUD)
  const linked = useAgent((snapshot) => snapshot.linkedDevices, NO_LINKED)
  const targets = useMemo(() => buildRemoteTargets(cloud, linked, { cloud: false, local: false }), [cloud, linked])
  const [editing, setEditing] = useState<number | null>(null)
  const [extension, setExtension] = useState('')
  const [target, setTarget] = useState(NO_TARGET)
  const rules = typeof raw === 'string' ? raw.split('\n').flatMap((line) => {
    const match = /^\s*\.?([A-Za-z0-9]+)\s*=\s*((?:cloud|link):[^\s:=\p{Cc}]+)\s*$/u.exec(line)
    return match ? [{ extension: match[1].toLowerCase(), target: match[2] }] : []
  }) : []
  const normalized = extension.trim().replace(/^\./, '').toLowerCase()
  const valid = /^[a-z0-9]+$/.test(normalized)
  const selected = resolveTargetValue(target, targets)
  const write = (next: typeof rules) => setPref(PREF, next.map((rule) => `${rule.extension}=${rule.target}`).join('\n'), { immediate: true })
  const begin = (index: number) => {
    setEditing(index)
    setExtension(rules[index]?.extension ?? '')
    setTarget(rules[index]?.target ?? NO_TARGET)
  }
  const save = () => {
    if (editing === null || !valid || !selected || readOnly) return
    const next = rules.slice()
    next.splice(editing, editing < rules.length ? 1 : 0, { extension: normalized, target: selected.value })
    write(next)
    setEditing(null)
  }
  return (
    <>
      <SettingsRow title={t('captureDeviceRules')} description={t('captureDeviceRulesDesc')}>
        <Button variant="outline" onClick={() => begin(rules.length)} disabled={readOnly || targets.length === 0}>{t('captureDeviceRuleAdd')}</Button>
      </SettingsRow>
      {targets.length === 0 ? <p className="text-sm text-muted-foreground">{t('captureDeviceRuleNoDevices')}</p> : null}
      {rules.map((rule, index) => (
        <SettingsRow key={index} title={`.${rule.extension}`} description={resolveTargetValue(rule.target, targets)?.name ?? t('captureRoutingTargetUnavailable')}>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" disabled={readOnly || targets.length === 0} onClick={() => begin(index)}>{t('captureDeviceRuleEdit')}</Button>
            <Button variant="outline" disabled={readOnly} onClick={() => { write(rules.filter((_, i) => i !== index)); setEditing(null) }}>{t('delete')}</Button>
          </div>
        </SettingsRow>
      ))}
      {editing !== null ? (
        <>
          <SettingsRow title={t('captureDeviceRuleExtension')} description={extension && !valid ? t('captureDeviceRuleExtensionInvalid') : undefined}>
            <TextField key={editing} value={extension} aria-label={t('captureDeviceRuleExtension')} onCommit={setExtension} />
          </SettingsRow>
          <SettingsRow title={t('captureDeviceRuleTarget')}>
            <DropdownField value={selected?.value ?? NO_TARGET} options={[{ value: NO_TARGET, label: t('captureDeviceRuleTarget') }, ...targets.map((item) => ({ value: item.value, label: item.name }))]} onValueChange={setTarget} aria-label={t('captureDeviceRuleTarget')} />
          </SettingsRow>
          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={() => setEditing(null)}>{t('cancel')}</Button>
            <Button disabled={readOnly || !valid || selected === null} onClick={save}>{t('save')}</Button>
          </div>
        </>
      ) : null}
    </>
  )
}
