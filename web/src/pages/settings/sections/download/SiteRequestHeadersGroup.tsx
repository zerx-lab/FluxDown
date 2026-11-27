import { useMemo, useState } from 'react'
import { useT } from '../../../../i18n'
import { useConfigValue } from '../../../../lib/rpc'
import { rpcErrorText } from '../../../../lib/rpcErrorText'
import {
  SITE_REQUEST_HEADERS_KEY, parseSiteRequestHeaders, patchSiteRequestHeaders,
  siteRequestHeadersDraft, siteRequestHeadersFields,
} from '../../../../lib/siteRequestHeaders'
import type { SiteRequestHeadersRule } from '../../../../lib/siteRequestHeaders'
import { Button, ConfirmFooter, Dialog, FieldError, FormField, Input, Textarea, confirmDialog, toast } from '../../../../ui'
import { SettingsCustomRow, SettingsSection } from '../../kit'

export function SiteRequestHeadersGroup({ disabled }: { disabled: boolean }) {
  const t = useT()
  const raw = useConfigValue(SITE_REQUEST_HEADERS_KEY)
  const list = useMemo(() => parseSiteRequestHeaders(raw), [raw])
  const [editor, setEditor] = useState<{ rule: SiteRequestHeadersRule | null } | null>(null)
  const [deleting, setDeleting] = useState(false)
  if (raw === undefined) return null

  const remove = async (rule: SiteRequestHeadersRule) => {
    const confirmed = await confirmDialog({
      title: t('delete'), description: rule.origin, intent: 'destructive', okLabel: t('delete'),
    })
    if (!confirmed) return
    setDeleting(true)
    try {
      await patchSiteRequestHeaders(rule.origin, null)
    } catch (error) {
      toast.error(rpcErrorText(error, t))
    } finally {
      setDeleting(false)
    }
  }

  return (
    <SettingsSection title={t('siteRequestHeadersTitle')} subtitle={t('siteRequestHeadersDesc')}>
      <SettingsCustomRow className="flex flex-col gap-1 px-4 py-2">
        {list ? list.map((rule) => (
          <div key={rule.origin} className="flex flex-wrap items-center gap-2 py-1">
            <span className="min-w-0 flex-1 truncate font-mono text-sm" title={rule.origin}>{rule.origin}</span>
            <div className="flex shrink-0 gap-1">
              <Button variant="ghost" disabled={disabled || deleting} onClick={() => setEditor({ rule })}>
                {t('menuEdit')}
              </Button>
              <Button variant="ghost" className="text-destructive" disabled={disabled || deleting} onClick={() => void remove(rule)}>
                {t('delete')}
              </Button>
            </div>
          </div>
        )) : <FieldError>{t('siteRequestHeadersInvalid')}</FieldError>}
        <div className="flex justify-end py-1">
          <Button variant="primary" disabled={disabled || deleting || !list} onClick={() => setEditor({ rule: null })}>
            {t('siteRequestHeadersAdd')}
          </Button>
        </div>
      </SettingsCustomRow>
      {editor ? <RuleDialog rule={editor.rule} disabled={disabled || !list} onClose={() => setEditor(null)} /> : null}
    </SettingsSection>
  )
}

function RuleDialog({ rule, disabled, onClose }: {
  rule: SiteRequestHeadersRule | null
  disabled: boolean
  onClose: () => void
}) {
  const t = useT()
  const [fields, setFields] = useState(() => siteRequestHeadersFields(rule))
  const [error, setError] = useState('')
  const [saving, setSaving] = useState(false)
  const change = (key: keyof typeof fields, value: string) => {
    setFields((current) => ({ ...current, [key]: value }))
    setError('')
  }
  const save = async () => {
    if (saving || disabled) return
    const draft = siteRequestHeadersDraft(fields.origin, fields.referer, fields.extra)
    if (!draft) {
      setError(t('siteRequestHeadersInvalid'))
      return
    }
    setSaving(true)
    setError('')
    try {
      await patchSiteRequestHeaders(rule?.origin ?? null, draft)
      onClose()
    } catch (error) {
      setError(rpcErrorText(error, t))
    } finally {
      setSaving(false)
    }
  }
  return (
    <Dialog
      open
      onOpenChange={(open) => { if (!open && !saving) onClose() }}
      title={t(rule ? 'siteRequestHeadersTitle' : 'siteRequestHeadersAdd')}
      footer={<ConfirmFooter okLabel={t('confirm')} okDisabled={disabled || saving} loading={saving} onCancel={onClose} onOk={() => void save()} />}
    >
      <form className="flex flex-col gap-4" onSubmit={(event) => { event.preventDefault(); void save() }}>
        <FormField label={t('siteRequestHeadersOrigin')} htmlFor="site-request-headers-origin">
          <Input id="site-request-headers-origin" className="font-mono" inputMode="url" autoCapitalize="off" autoCorrect="off" spellCheck={false}
            placeholder="https://files.example.net" disabled={saving} value={fields.origin} onChange={(event) => change('origin', event.target.value)} />
        </FormField>
        <FormField label={t('siteRequestHeadersReferer')} htmlFor="site-request-headers-referer">
          <Input id="site-request-headers-referer" className="font-mono" inputMode="url" autoCapitalize="off" autoCorrect="off" spellCheck={false}
            placeholder="https://www.example.com/" disabled={saving} value={fields.referer} onChange={(event) => change('referer', event.target.value)} />
        </FormField>
        <FormField label={t('siteRequestHeadersExtra')} hint={t('siteRequestHeadersExtraHint')} htmlFor="site-request-headers-extra">
          <Textarea id="site-request-headers-extra" className="font-mono" rows={4} autoCapitalize="off" autoCorrect="off" spellCheck={false}
            disabled={saving} value={fields.extra} onChange={(event) => change('extra', event.target.value)} />
        </FormField>
        {error ? <FieldError>{error}</FieldError> : null}
      </form>
    </Dialog>
  )
}
