import { ArrowDown, ArrowUp } from 'lucide-react'
import { useId, useMemo, useState } from 'react'
import { useT } from '../../../../i18n'
import { RpcError, useAgent } from '../../../../lib/rpc'
import { rpcErrorText } from '../../../../lib/rpcErrorText'
import { editSiteUserAgentRules, parseSiteUserAgentRules, SITE_USER_AGENT_RULES_KEY } from '../../../../lib/siteUserAgentRules'
import type { SiteUserAgentRule } from '../../../../lib/siteUserAgentRules'
import { Badge, Button, ConfirmFooter, Dialog, FieldError, FormField, Icon, Input, Switch, toast } from '../../../../ui'
import { optionalText, SettingsRow, setDaemon, useDaemonValue, useSettingsReadOnly } from '../../kit'
import { UserAgentPresetControl } from '../../../userAgent/UserAgentPresetControl'

const UA_KEY = 'global_user_agent'

export function UserAgentRow() {
  const t = useT()
  const current = useDaemonValue(UA_KEY)
  return <SettingsRow title={t('userAgent')} description={optionalText(t, 'userAgentDesc')}>
    <UserAgentPresetControl value={current} onChange={(value) => setDaemon(UA_KEY, value)} allowSave={false} defaultLabelKey="userAgentPresetDefault" inputPlaceholderKey="userAgentPlaceholder" />
  </SettingsRow>
}

const EMPTY_RULE = { domain: '', includeSubdomains: false, userAgent: '' }

interface RuleEditor {
  index: number | null
  baseline: readonly SiteUserAgentRule[]
  draft: SiteUserAgentRule
}

export function SiteUserAgentRulesRow() {
  const t = useT()
  const raw = useDaemonValue(SITE_USER_AGENT_RULES_KEY)
  const rules = useMemo(() => parseSiteUserAgentRules(raw), [raw])
  const readOnly = useSettingsReadOnly()
  const daemonConnected = useAgent((snapshot) => snapshot.daemonConnected, false)
  const [editor, setEditor] = useState<RuleEditor | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const domainId = useId()
  const subdomainsId = useId()
  const disabled = readOnly || !daemonConnected || busy

  const write = async (baseline: readonly SiteUserAgentRule[], edit: (list: SiteUserAgentRule[]) => SiteUserAgentRule[]) => {
    setBusy(true)
    try {
      return await editSiteUserAgentRules(baseline, edit) ? '' : t('localServiceConflict')
    } catch (failure) {
      return failure instanceof RpcError && failure.is('invalidArgument')
        ? t('siteUserAgentInvalid')
        : rpcErrorText(failure, t)
    } finally {
      setBusy(false)
    }
  }

  const openEditor = (index: number | null) => {
    if (!rules) return
    setError('')
    setEditor({ index, baseline: rules, draft: index === null ? EMPTY_RULE : rules[index] })
  }
  const changeDraft = (change: Partial<SiteUserAgentRule>) => {
    setError('')
    setEditor((current) => current && { ...current, draft: { ...current.draft, ...change } })
  }
  const save = async () => {
    if (!editor || disabled) return
    setError('')
    const { baseline, index, draft } = editor
    const failure = await write(baseline, (list) => {
      if (index === null) list.push(draft)
      else list[index] = draft
      return list
    })
    if (failure) setError(failure)
    else setEditor(null)
  }
  const changeList = async (edit: (list: SiteUserAgentRule[]) => SiteUserAgentRule[]) => {
    if (!rules || disabled) return
    const failure = await write(rules, edit)
    if (failure) toast.error(failure)
  }
  const move = (index: number, delta: -1 | 1) => changeList((list) => {
    const [rule] = list.splice(index, 1)
    list.splice(index + delta, 0, rule)
    return list
  })
  const remove = (index: number) => changeList((list) => {
    list.splice(index, 1)
    return list
  })

  const rowKeys = new Map<string, number>()
  const closeEditor = () => { if (!busy) setEditor(null) }
  return (
    <SettingsRow title={t('siteUserAgentRules')} description={t('siteUserAgentRulesDesc')} vertical>
      <div className="flex flex-col gap-2">
        {!rules ? <FieldError>{t('siteUserAgentInvalid')}</FieldError> : null}
        {rules?.length ? (
          <ol className="flex flex-col gap-1">
            {rules.map((rule, index) => {
              const identity = JSON.stringify(rule)
              const occurrence = rowKeys.get(identity) ?? 0
              rowKeys.set(identity, occurrence + 1)
              return (
                <li key={`${identity}:${occurrence}`} className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-md border border-hairline px-2 py-1.5">
                  <Badge className="shrink-0 font-mono tabular-nums">{index + 1}</Badge>
                  <div className="min-w-0 flex-1 basis-32">
                    <div className="truncate text-sm" title={rule.domain}>{rule.domain}</div>
                    <div className="truncate whitespace-pre font-mono text-xs text-muted-foreground" title={rule.userAgent}>{rule.userAgent}</div>
                    {rule.includeSubdomains ? <div className="text-xs text-muted-foreground">{t('siteUserAgentSubdomains')}</div> : null}
                  </div>
                  <div className="flex flex-wrap items-center gap-1">
                    <Button variant="ghost" iconOnly aria-label={t('siteUserAgentMoveUp')} title={t('siteUserAgentMoveUp')} disabled={disabled || index === 0} onClick={() => void move(index, -1)}>
                      <Icon icon={ArrowUp} size="md" />
                    </Button>
                    <Button variant="ghost" iconOnly aria-label={t('siteUserAgentMoveDown')} title={t('siteUserAgentMoveDown')} disabled={disabled || index === rules.length - 1} onClick={() => void move(index, 1)}>
                      <Icon icon={ArrowDown} size="md" />
                    </Button>
                    <Button variant="outline" disabled={disabled} onClick={() => openEditor(index)}>{t('siteUserAgentEdit')}</Button>
                    <Button variant="ghost" className="text-destructive" disabled={disabled} onClick={() => void remove(index)}>{t('delete')}</Button>
                  </div>
                </li>
              )
            })}
          </ol>
        ) : null}
        <div className="flex justify-end">
          <Button variant="outline" disabled={disabled || !rules} onClick={() => openEditor(null)}>{t('siteUserAgentAdd')}</Button>
        </div>
      </div>
      {editor ? (
        <Dialog
          open
          persistent
          modalLocked={busy}
          onOpenChange={(open) => { if (!open) closeEditor() }}
          title={t(editor.index === null ? 'siteUserAgentAdd' : 'siteUserAgentEdit')}
          footer={<ConfirmFooter okLabel={t('proxyRuleSave')} okDisabled={disabled || !editor.draft.domain.trim() || !editor.draft.userAgent.trim()} loading={busy} onCancel={closeEditor} onOk={() => void save()} />}
        >
          <div className="flex flex-col gap-4">
            <FormField label={t('siteUserAgentDomain')} htmlFor={domainId}>
              <Input id={domainId} value={editor.draft.domain} autoCapitalize="off" autoCorrect="off" spellCheck={false} disabled={disabled} onChange={(event) => changeDraft({ domain: event.target.value })} />
            </FormField>
            <label htmlFor={subdomainsId} className="flex items-center gap-2 text-sm">
              <Switch id={subdomainsId} checked={editor.draft.includeSubdomains} disabled={disabled} onCheckedChange={(includeSubdomains) => changeDraft({ includeSubdomains })} />
              {t('siteUserAgentSubdomains')}
            </label>
            <FormField label={t('userAgent')}>
              <UserAgentPresetControl value={editor.draft.userAgent} onChange={(userAgent) => changeDraft({ userAgent })} allowDefault={false} allowSave={false} forceDisabled={disabled} inputPlaceholderKey="userAgentPlaceholder" />
            </FormField>
            {error ? <FieldError>{error}</FieldError> : null}
          </div>
        </Dialog>
      ) : null}
    </SettingsRow>
  )
}
