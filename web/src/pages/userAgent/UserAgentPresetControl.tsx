import { useMemo, useRef, useState } from 'react'
import { useT } from '../../i18n'
import { rpc, useAgent, useConnection } from '../../lib/rpc'
import { Button, Dialog, Input, Select, toast } from '../../ui'
import { UA_PRESETS } from '../../lib/ua-presets'

const USER_AGENT_PRESETS_PREF = 'download.user_agent_presets'
type UserAgentPreset = { name: string; value: string }
interface DraftPreset extends UserAgentPreset { id: number }
function hasControlCharacters(value: string): boolean {
  for (let i = 0; i < value.length; i++) {
    const code = value.charCodeAt(i)
    if ((code < 32 && code !== 9) || code === 127) return true
  }
  return false
}
const builtInValues = new Set(UA_PRESETS.map(({ value }) => value))

function parseUserAgentPresets(raw: unknown): UserAgentPreset[] {
  if (!Array.isArray(raw)) return []
  const result: UserAgentPreset[] = []
  const seen = new Set<string>()
  for (const item of raw) {
    if (!item || typeof item !== 'object') continue
    const name = typeof item.name === 'string' ? item.name.trim() : ''
    const value = typeof item.value === 'string' ? item.value.trim() : ''
    if (!name || !value || hasControlCharacters(value) || builtInValues.has(value) || seen.has(value)) continue
    seen.add(value)
    result.push({ name, value })
  }
  return result
}

export function UserAgentPresetControl({ value, onChange, allowSave = true, showSelector = true, showInput = true, defaultLabelKey = 'queueUaInheritGlobal', inputPlaceholderKey = 'userAgentTaskPlaceholder', forceDisabled = false }: { value: string; onChange: (value: string) => void; allowSave?: boolean; showSelector?: boolean; showInput?: boolean; defaultLabelKey?: string; inputPlaceholderKey?: string; forceDisabled?: boolean }) {
  const t = useT()
  const connectionPhase = useConnection().phase
  const daemonConnected = useAgent((snapshot) => snapshot.daemonConnected, false)
  const connected = connectionPhase === 'ready' && daemonConnected
  const pref = useAgent((snapshot) => snapshot.preferences.values[USER_AGENT_PRESETS_PREF], undefined)
  const presets = useMemo(() => parseUserAgentPresets(pref), [pref])
  const [manage, setManage] = useState(false)
  const [saveName, setSaveName] = useState('')
  const [draft, setDraft] = useState<DraftPreset[]>([])
  const [pending, setPending] = useState(false)
  const nextDraftId = useRef(0)
  const builtins = UA_PRESETS.map((preset, index) => ({ value: `builtin:${index}`, label: preset.label, userAgent: preset.value }))
  const options = [
    { value: '__default', label: t(defaultLabelKey), userAgent: '' },
    ...builtins,
    ...presets.map((preset, index) => ({ value: `saved:${index}`, label: preset.name, userAgent: preset.value })),
    { value: '__custom', label: t('userAgentPresetCustom'), userAgent: null },
  ]
  const isPreset = (ua: string) => builtins.some((preset) => preset.userAgent === ua) || presets.some((preset) => preset.value === ua)
  const [customMode, setCustomMode] = useState(false)
  const selected = customMode ? '__custom' : options.find((option) => option.userAgent === value)?.value ?? '__custom'

  async function persist(next: UserAgentPreset[]) {
    await rpc.agent.preferences.patch({ values: { [USER_AGENT_PRESETS_PREF]: next }, sync: false })
  }
  function beginManage() {
    nextDraftId.current = 0
    setDraft(presets.map((preset) => ({ ...preset, id: nextDraftId.current++ })))
    setManage(true)
  }
  function addDraft() {
    setDraft((items) => [...items, { id: nextDraftId.current++, name: '', value: '' }])
  }
  async function saveCurrent() {
    const name = saveName.trim()
    const ua = value.trim()
    if (!name || !ua || hasControlCharacters(ua)) { toast.key('userAgentPresetInvalid', 'error'); return }
    if (isPreset(ua)) { toast.key('userAgentPresetExists', 'error'); return }
    setPending(true)
    try {
      await persist([...presets, { name, value: ua }])
      onChange(ua)
      setCustomMode(false)
      setSaveName('')
    } catch (error) {
      toast.error(error)
    } finally {
      setPending(false)
    }
  }
  async function saveDraft() {
    const next = draft.map(({ name, value }) => ({ name: name.trim(), value: value.trim() }))
    if (next.some(({ name, value }) => !name || !value || hasControlCharacters(value))) { toast.key('userAgentPresetInvalid', 'error'); return }
    const seen = new Set<string>()
    for (const preset of next) {
      if (builtInValues.has(preset.value) || seen.has(preset.value)) { toast.key('userAgentPresetExists', 'error'); return }
      seen.add(preset.value)
    }
    setPending(true)
    try {
      await persist(next)
      setManage(false)
    } catch (error) {
      toast.error(error)
    } finally {
      setPending(false)
    }
  }
  const disabled = forceDisabled || !connected || pending
  return <div className="flex flex-col gap-2">
    {showSelector ? <div className="flex gap-2"><Select value={selected} options={options} disabled={disabled} aria-label={t('userAgent')} onValueChange={(next) => {
      const option = options.find((item) => item.value === next)
      if (!option) return
      if (option.value === '__custom') { setCustomMode(true); return }
      setCustomMode(false)
      onChange(option.userAgent ?? '')
    }} /><Button type="button" disabled={disabled} onClick={beginManage}>{t('userAgentManagePresets')}</Button></div> : <Button type="button" disabled={disabled} onClick={beginManage}>{t('userAgentManagePresets')}</Button>}
    {showInput && (customMode || (value !== '' && !isPreset(value))) ? <Input value={value} disabled={disabled} placeholder={t(inputPlaceholderKey)} spellCheck={false} aria-label={t('userAgent')} onChange={(event) => { setCustomMode(true); onChange(event.target.value) }} /> : null}
    {allowSave && value.trim() && !isPreset(value) ? <div className="flex gap-2"><Input value={saveName} disabled={disabled} onChange={(event) => setSaveName(event.target.value)} placeholder={t('userAgentPresetName')} aria-label={t('userAgentPresetName')} /><Button type="button" disabled={disabled || !saveName.trim()} onClick={() => void saveCurrent()}>{t('userAgentSavePreset')}</Button></div> : null}
    {manage ? <Dialog open onOpenChange={(open) => !open && !pending && setManage(false)} title={t('userAgentManagePresets')}>
      <div className="flex max-h-[60vh] flex-col gap-3 overflow-y-auto">
        {builtins.map((preset) => <div key={preset.value} className="grid grid-cols-[minmax(0,1fr)_minmax(0,2fr)] gap-2"><Input value={preset.label} disabled /><Input value={preset.value} disabled /></div>)}
        {draft.map((preset, index) => <div key={preset.id} className="grid grid-cols-[minmax(0,1fr)_minmax(0,2fr)_auto] gap-2"><Input value={preset.name} disabled={disabled} aria-label={t('userAgentPresetName')} onChange={(event) => setDraft((items) => items.map((item, i) => i === index ? { ...item, name: event.target.value } : item))} /><Input value={preset.value} disabled={disabled} aria-label={t('userAgentPresetValue')} onChange={(event) => setDraft((items) => items.map((item, i) => i === index ? { ...item, value: event.target.value } : item))} /><Button type="button" disabled={disabled} onClick={() => setDraft((items) => items.filter((_, i) => i !== index))}>{t('delete')}</Button></div>)}
        <Button type="button" aria-label={t('userAgentAddPreset')} disabled={disabled} onClick={addDraft}>+ {t('userAgentAddPreset')}</Button>
        <Button type="button" disabled={disabled} onClick={() => void saveDraft()}>{t('confirm')}</Button>
      </div>
    </Dialog> : null}
  </div>
}
