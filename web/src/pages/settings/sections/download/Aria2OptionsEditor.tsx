import { useCallback, useEffect, useState } from 'react'
import { useT } from '../../../../i18n'
import { copyText } from '../../../../lib/copy'
import { rpc, rpcStore } from '../../../../lib/rpc'
import { rpcErrorText } from '../../../../lib/rpcErrorText'
import { Button, Textarea, toast } from '../../../../ui'
import { useSettingsReadOnly } from '../../kit'

export function Aria2OptionsEditor() {
  const t = useT()
  const readOnly = useSettingsReadOnly()
  const [text, setText] = useState('')
  const [revision, setRevision] = useState<number>()
  const [defaults, setDefaults] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  const reload = useCallback(async () => {
    setBusy(true)
    setError('')
    try {
      const result = await rpc.daemon.config.aria2Options()
      if (result.error) {
        setError(result.error)
        return
      }
      setText(result.text)
      setDefaults(result.defaults)
      setRevision(result.revision)
    } catch (failure) {
      setError(rpcErrorText(failure, t))
    } finally {
      setBusy(false)
    }
  }, [t])

  useEffect(() => {
    if (!readOnly && revision === undefined) void reload()
  }, [readOnly, reload, revision])

  const apply = async () => {
    if (revision === undefined || readOnly || busy) return
    setBusy(true)
    setError('')
    try {
      const preview = await rpc.daemon.config.aria2Options({ text })
      if (preview.error) {
        setError(preview.error)
        return
      }
      const snapshot = await rpc.daemon.config.patch({ expectedRevision: revision, values: preview.values })
      rpcStore.update((state) => {
        if (!state.snapshot || state.snapshot.daemon.config.revision > snapshot.revision) return state
        const daemon = { ...state.snapshot.daemon, config: snapshot }
        return { ...state, snapshot: { ...state.snapshot, daemon } }
      })
      setText(preview.text)
      setDefaults(preview.defaults)
      setRevision(snapshot.revision)
      toast.key('aria2AdvancedSaved', 'success')
    } catch (failure) {
      setError(rpcErrorText(failure, t))
    } finally {
      setBusy(false)
    }
  }

  const exportDraft = () => copyText(text)

  return (
    <div className="space-y-2 rounded-md border border-border p-3">
      <div className="text-sm font-medium">{t('aria2AdvancedTitle')}</div>
      <p className="text-xs text-muted-foreground">{t('aria2AdvancedDescription')}</p>
      <p className="text-xs text-muted-foreground">{t('aria2AdvancedPriority')}</p>
      <p className="text-xs text-muted-foreground">{t('aria2AdvancedRestart')}</p>
      <Button loading={busy} disabled={busy || readOnly} onClick={() => void reload()}>{t('aria2AdvancedReload')}</Button>
      <Textarea disabled={busy || readOnly} value={text} onChange={(event) => setText(event.target.value)} rows={12} className="font-mono text-xs" aria-label={t('aria2AdvancedTitle')} />
      <div className="flex flex-wrap gap-2">
        <Button disabled={busy || readOnly || revision === undefined} variant="primary" onClick={() => void apply()}>{t('aria2AdvancedApply')}</Button>
        <Button disabled={busy || readOnly || revision === undefined} onClick={() => setText(defaults)}>{t('aria2AdvancedDefaults')}</Button>
        <Button disabled={busy || revision === undefined} onClick={exportDraft}>{t('aria2AdvancedExport')}</Button>
      </div>
      {error ? <div role="alert" className="text-sm text-destructive">{error}</div> : null}
    </div>
  )
}
