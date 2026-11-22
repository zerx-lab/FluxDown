import { useEffect, useRef, useState } from 'react'
import { useI18n } from '../../../../i18n'
import { rpc, useRpcSelector } from '../../../../lib/rpc'
import { Button, CheckRow, ConfirmFooter, Dialog, FormField, Input, Textarea } from '../../../../ui'

const WEBSITE = 'https://fluxdown.com'
const TITLE_LIMIT = 200
const DESCRIPTION_LIMIT = 5000
const AUDIT_LIMIT = 20
const REQUEST_TIMEOUT_MS = 30_000

export interface FeedbackPlugin {
  identity: string
  version: string
  localOnly: boolean
  enabled: boolean
  loadStatus: 'Loaded' | 'Failed' | null
}

type Route = { kind: 'loading' | 'unavailable' | 'failed' | 'limited' } | { kind: 'ready'; repository: string }
type SendStatus = 'idle' | 'sending' | 'failed' | 'limited' | 'changed' | { issueNumber: number }
type AuditSummary = { error: number; denied: number } | 'loading' | 'unavailable'

function parseRepository(value: unknown): string | null {
  return typeof value === 'string' && value === value.trim() &&
    /^[a-z\d](?:[a-z\d-]{0,37}[a-z\d])?\/[a-z\d_.-]{1,100}$/i.test(value) && !/\/\.{1,2}$/.test(value) ? value : null
}

function version(value: string, limit: number): string | undefined {
  const normalized = value.trim().replace(/\s+/g, ' ')
  return normalized.length > 0 && normalized.length <= limit ? normalized : undefined
}

function Metadata({ label, value }: { label: string; value: string | undefined }) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,2fr)] gap-2">
      <dt className="break-words text-muted-foreground">{label}</dt>
      <dd className="min-w-0 whitespace-pre-wrap [overflow-wrap:anywhere]" dir="auto">{value || '—'}</dd>
    </div>
  )
}

export function PluginFeedbackDialog({ plugin, onClose }: { plugin: FeedbackPlugin; onClose: () => void }) {
  const { t, locale } = useI18n()
  const serviceVersion = useRpcSelector((state) => state.hello?.serviceVersion ?? '')
  const appVersion = version(serviceVersion, 50)
  const pluginVersion = version(plugin.version, 64)
  const canLookUp = !plugin.localOnly && pluginVersion !== undefined
  const endpoint = `${WEBSITE}/api/feedback?pluginIdentity=${encodeURIComponent(plugin.identity)}`
  const [route, setRoute] = useState<Route>({ kind: canLookUp ? 'loading' : 'unavailable' })
  const [lookupAttempt, setLookupAttempt] = useState(0)
  const [platform, setPlatform] = useState<string | null>(null)
  const [title, setTitle] = useState('')
  const [description, setDescription] = useState('')
  const [includeDiagnostics, setIncludeDiagnostics] = useState(false)
  const [audit, setAudit] = useState<AuditSummary>('loading')
  const [status, setStatus] = useState<SendStatus>('idle')
  const sending = useRef(false)
  const sent = typeof status === 'object' ? status.issueNumber : null

  useEffect(() => {
    if (!canLookUp) return
    const controller = new AbortController()
    let active = true
    const timer = window.setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS)
    void (async () => {
      try {
        const res = await fetch(endpoint, {
          signal: controller.signal, credentials: 'omit', referrerPolicy: 'no-referrer', redirect: 'error', cache: 'no-store',
        })
        if (!active) return
        if (res.status === 404) setRoute({ kind: 'unavailable' })
        else if (res.status === 429) setRoute({ kind: 'limited' })
        else if (res.status === 200) {
          const body = (await res.json()) as { repository?: unknown } | null
          const repository = parseRepository(body?.repository)
          if (active) setRoute(repository ? { kind: 'ready', repository } : { kind: 'failed' })
        } else setRoute({ kind: 'failed' })
      } catch {
        if (active) setRoute({ kind: 'failed' })
      } finally {
        window.clearTimeout(timer)
      }
    })()
    return () => {
      active = false
      window.clearTimeout(timer)
      controller.abort()
    }
  }, [canLookUp, endpoint, lookupAttempt])

  useEffect(() => {
    if (!canLookUp) return
    let active = true
    // The browser is not the plugin host. The agent report uses its compiled OS/arch; /ping reports "server".
    void rpc.agent.diagnostics.run().then((report) => {
      const value = report.platform
      if (active) {
        setPlatform(/^(linux|windows|macos|freebsd|openbsd|netbsd|dragonfly|android|ios) (x86_64|x86|aarch64|arm|riscv64|loongarch64)$/.test(value) ? value : '')
      }
    }).catch(() => {
      if (active) setPlatform('')
    })
    return () => { active = false }
  }, [canLookUp])

  useEffect(() => {
    if (!includeDiagnostics) return
    let active = true
    void rpc.daemon.plugin.auditList({ identity: plugin.identity, limit: AUDIT_LIMIT }).then(({ entries }) => {
      if (!active) return
      const counts = { error: 0, denied: 0 }
      for (const { outcome } of entries.slice(0, AUDIT_LIMIT)) {
        if (outcome === 'error' || outcome === 'denied') counts[outcome]++
      }
      setAudit(counts)
    }).catch(() => {
      if (active) setAudit('unavailable')
    })
    return () => { active = false }
  }, [includeDiagnostics, plugin.identity])

  // Attach only fixed status values and aggregate counts, never runtime logs, errors, settings, or paths.
  const diagnostics = includeDiagnostics ? [
    t(plugin.enabled ? 'pluginArchiveRulesEnabled' : 'pluginDisabledManual'),
    plugin.loadStatus === 'Loaded' ? t('pluginLoadStatusLoaded') : plugin.loadStatus === 'Failed' ? t('pluginLoadStatusFailed') : '',
    typeof audit === 'object' ? t('plugFeedbackAuditSummary', audit) : '',
  ].filter(Boolean).join(' · ') : ''
  const environment = [platform, diagnostics].filter(Boolean).join(' · ') || undefined
  const reportTitle = title.trim()
  const reportDescription = description.trim()
  const canSend = route.kind === 'ready' && platform !== null && (!includeDiagnostics || audit !== 'loading') &&
    reportTitle.length > 0 && reportTitle.length <= TITLE_LIMIT &&
    reportDescription.length > 0 && reportDescription.length <= DESCRIPTION_LIMIT
  const send = async () => {
    if (route.kind !== 'ready' || !canSend || sending.current || sent !== null) return
    sending.current = true
    setStatus('sending')
    const controller = new AbortController()
    const timer = window.setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS)
    try {
      const res = await fetch(endpoint, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'omit',
        referrerPolicy: 'no-referrer',
        redirect: 'error',
        signal: controller.signal,
        body: JSON.stringify({
          source: 'app', type: 'bug', title: reportTitle, description: reportDescription,
          pluginIdentity: plugin.identity, pluginVersion, appVersion, environment, repository: route.repository,
        }),
      })
      if (res.status === 404) {
        setRoute({ kind: 'unavailable' })
        setStatus('idle')
      } else if (res.status === 409) {
        setRoute({ kind: 'loading' })
        setStatus('changed')
        setLookupAttempt((attempt) => attempt + 1)
      } else if (res.status === 429) setStatus('limited')
      else if (res.status === 201) {
        const body = (await res.json()) as { issueNumber?: unknown } | null
        const issueNumber = body?.issueNumber
        setStatus(typeof issueNumber === 'number' && Number.isSafeInteger(issueNumber) && issueNumber > 0 ? { issueNumber } : 'failed')
      } else setStatus('failed')
    } catch {
      setStatus('failed')
    } finally {
      window.clearTimeout(timer)
      sending.current = false
    }
  }

  const locked = status === 'sending'
  const closeOnly = sent !== null || route.kind === 'unavailable'
  return (
    <Dialog
      open
      persistent
      modalLocked={locked}
      size="lg"
      onOpenChange={(open) => { if (!open && !sending.current) onClose() }}
      title={t('plugFeedbackTitle')}
      description={t('plugFeedbackDescription')}
      footer={(
        <ConfirmFooter
          cancelLabel={closeOnly || locked ? null : undefined}
          okLabel={t(closeOnly ? 'close' : 'plugFeedbackSend')}
          okDisabled={!closeOnly && !canSend}
          loading={locked}
          onCancel={onClose}
          onOk={closeOnly ? onClose : () => void send()}
        />
      )}
    >
      <div className="flex min-w-0 flex-col gap-4 pb-2">
        <dl className="grid gap-2 rounded-md border border-hairline px-3 py-2 text-sm">
          <Metadata label={t('plugFeedbackDestination')} value={route.kind === 'ready' ? route.repository : route.kind === 'loading' ? t('pluginCommonLoading') : undefined} />
          <Metadata label={t('pluginDetailIdentity')} value={plugin.identity} />
          <Metadata label={t('feedbackVersionLabel')} value={appVersion} />
          <Metadata label={t('plugFeedbackPluginVersion')} value={pluginVersion} />
          {canLookUp ? <Metadata label={t('feedbackSysInfoSystem')} value={platform === null ? t('pluginCommonLoading') : platform} /> : null}
          {diagnostics ? <Metadata label={t('plugFeedbackDiagnostics')} value={diagnostics} /> : null}
        </dl>
        {sent !== null ? <p role="status" className="text-sm">{t('plugFeedbackSent', { number: sent })}</p> : (
          <>
            {canLookUp && (route.kind !== 'unavailable' || title !== '' || description !== '') ? (
              <>
                <p className="text-sm text-muted-foreground">{t('plugFeedbackPublic')}</p>
                <FormField label={t('feedbackTitleLabel')} htmlFor="plugin-feedback-title">
                  <Input id="plugin-feedback-title" value={title} maxLength={TITLE_LIMIT} disabled={locked} onChange={(event) => setTitle(event.target.value)} />
                </FormField>
                <FormField label={t('feedbackDescLabel')} htmlFor="plugin-feedback-description">
                  <Textarea id="plugin-feedback-description" value={description} rows={5} maxLength={DESCRIPTION_LIMIT} disabled={locked} onChange={(event) => setDescription(event.target.value)} />
                </FormField>
                <CheckRow checked={includeDiagnostics} onCheckedChange={(checked) => {
                  if (checked) setAudit('loading')
                  setIncludeDiagnostics(checked)
                }} disabled={locked || route.kind !== 'ready'}>
                  {t('plugFeedbackDiagnostics')}
                </CheckRow>
                {includeDiagnostics && audit === 'loading' ? <p role="status" className="text-xs text-muted-foreground">{t('pluginCommonLoading')}</p> : null}
                {includeDiagnostics && audit === 'unavailable' ? <p role="status" className="text-xs text-muted-foreground">{t('plugFeedbackAuditUnavailable')}</p> : null}
              </>
            ) : null}
            {status === 'changed' ? <p role="status" className="text-sm">{t('plugFeedbackRouteChanged')}</p> : null}
            {route.kind === 'unavailable' ? <p role="status" className="text-sm">{t('plugFeedbackUnavailable')}</p> : null}
            {route.kind === 'limited' || status === 'limited' ? <p role="alert" className="text-sm text-destructive">{t('feedbackRateLimited')}</p> : null}
            {route.kind === 'failed' || status === 'failed' ? <p role="alert" className="text-sm text-destructive">{t('plugFeedbackFailed')}</p> : null}
            {route.kind === 'failed' || route.kind === 'limited' ? (
              <Button variant="outline" className="self-start" onClick={() => {
                setRoute({ kind: 'loading' })
                setLookupAttempt((attempt) => attempt + 1)
              }}>{t('webRetry')}</Button>
            ) : null}
            {route.kind === 'unavailable' || route.kind === 'failed' || status === 'failed' ? (
              <a className="self-start text-sm text-accent-text underline" href={`${WEBSITE}${locale.startsWith('zh') ? '/zh' : ''}/feedback`} target="_blank" rel="noopener noreferrer">
                {t('plugFeedbackFallback')}
              </a>
            ) : null}
          </>
        )}
      </div>
    </Dialog>
  )
}
