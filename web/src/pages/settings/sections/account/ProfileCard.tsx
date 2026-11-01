// 已登录资料卡（GPUI profile.rs）：头像 + 昵称/套餐徽标 + Origin ID 胶囊 + 刷新 + 退出登录；
// 昵称 / Origin ID 修改遵守云端权益与一次性约束，走 agent.profile.*。

import { Check, Copy, Pencil, RefreshCw } from 'lucide-react'
import { useEffect, useRef, useState, useSyncExternalStore } from 'react'
import { useT } from '../../../../i18n'
import { copyText } from '../../../../lib/copy'
import { rpc } from '../../../../lib/rpc'
import type { AgentSessionDto } from '../../../../lib/rpc'
import { Button, Card, CheckRow, ConfirmFooter, Dialog, FieldError, FieldHint, Form, FormField, Icon, Input, Switch, Tooltip, toast } from '../../../../ui'
import { setPref, usePrefBoolean } from '../../kit'
import { SHOW_REFERRAL_PREF } from '../referral/pref'
import { localDate } from './billing'
import { accountErrorKey } from './errorText'
import { PlanBadge } from './PlanBadge'
import { PlanDialog } from './PlanDialog'
import { canEditOriginId, OriginIdEditor, trimmedNickname, validNickname } from './profileEditing'
function displayName(session: AgentSessionDto): string {
  const nickname = session.user.nickname.trim()
  if (nickname) return nickname
  return session.user.email.split('@')[0] ?? session.user.email
}

export function NicknameDialog({ current, onClose }: { current: string; onClose: () => void }) {
  const t = useT()
  const [value, setValue] = useState(current)
  const [busy, setBusy] = useState(false)
  const [errorKey, setErrorKey] = useState<string | null>(null)
  const pending = useRef(false)
  const active = useRef(true)
  useEffect(() => {
    active.current = true
    return () => { active.current = false }
  }, [])
  const trimmed = trimmedNickname(value)
  const invalid = !validNickname(value)
  const unchanged = trimmed === trimmedNickname(current)
  const close = () => { if (!pending.current) onClose() }

  const submit = async () => {
    if (pending.current || invalid || unchanged || !active.current) return
    pending.current = true
    setBusy(true)
    setErrorKey(null)
    try {
      await rpc.agent.profile.changeNickname({ nickname: trimmed })
      if (!active.current) return
      toast.key('accountNicknameEditSuccess', 'success')
      onClose()
    } catch (error) {
      if (!active.current) return
      pending.current = false
      setErrorKey(accountErrorKey(error))
      setBusy(false)
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && close()}
      title={t('accountNicknameEditTitle')}
      modalLocked={busy}
      footer={<ConfirmFooter okLabel={t('confirm')} cancelLabel={busy ? null : t('cancel')} onCancel={close} onOk={() => void submit()} okDisabled={invalid || unchanged} loading={busy} />}
    >
      <Form onSubmit={() => void submit()}>
        <FormField label={t('accountNicknameEditTitle')} htmlFor="account-nickname" {...(invalid && value !== '' ? { error: t('accountNicknameEditInvalid') } : {})}>
          <Input id="account-nickname" value={value} disabled={busy} onChange={(event) => { setValue(event.target.value); setErrorKey(null) }} invalid={invalid && value !== ''} autoFocus />
        </FormField>
        {errorKey ? <FieldError>{t(errorKey)}</FieldError> : null}
        <button type="submit" className="hidden" />
      </Form>
    </Dialog>
  )
}

function OriginIdDialog({ session, onClose }: { session: AgentSessionDto; onClose: () => void }) {
  const t = useT()
  const [editor] = useState(() => new OriginIdEditor(session.user.originId, rpc.agent.profile))
  const { value, check, busy, error, confirmed } = useSyncExternalStore(editor.subscribe, editor.snapshot)
  const allowed = editor.acceptsSession(session)
  useEffect(() => () => editor.invalidate(), [editor])
  useEffect(() => {
    if (!allowed) { editor.invalidate(); onClose() }
  }, [allowed, editor, onClose])
  useEffect(() => {
    if (busy) return
    const timer = window.setTimeout(() => void editor.check(), 400)
    return () => window.clearTimeout(timer)
  }, [editor, value, busy])

  const close = () => { if (!editor.snapshot().busy) onClose() }
  const submit = async () => {
    if (await editor.submit()) {
      toast.key('accountOriginIdEditSuccess', 'success')
      onClose()
    }
  }
  const fieldError = check === 'invalid' ? t('accountOriginIdInvalid') : check === 'taken' ? t('accountOriginIdErrorTaken') : undefined
  if (!allowed) return null
  return (
    <Dialog
      open
      onOpenChange={(open) => !open && close()}
      title={t('accountOriginIdEditTitle')}
      description={t('accountOriginIdEditDesc')}
      modalLocked={busy}
      footer={
        <ConfirmFooter
          okLabel={t('accountOriginIdEditConfirm')}
          cancelLabel={busy ? null : t('cancel')}
          onCancel={close}
          onOk={() => void submit()}
          okDisabled={!editor.canSubmit()}
          loading={busy}
        />
      }
    >
      <Form onSubmit={() => void submit()}>
        <FormField label={t('accountOriginIdEditPlaceholder')} htmlFor="account-origin-id" {...(fieldError ? { error: fieldError } : {})}>
          <div className="flex items-center gap-2">
            <Input
              id="account-origin-id"
              value={value}
              inputMode="numeric"
              disabled={busy}
              onChange={(event) => editor.setValue(event.target.value)}
              invalid={fieldError !== undefined}
              autoFocus
            />
            <Button onClick={() => void editor.roll()} disabled={busy}>
              {t('accountOriginIdEditRoll')}
            </Button>
          </div>
        </FormField>
        {check === 'available' ? <FieldHint className="text-success">#{Number(value)}</FieldHint> : null}
        <CheckRow checked={confirmed} onCheckedChange={(next) => editor.setConfirmed(next)} disabled={busy} className="text-warning">{t('accountOriginIdEditWarning')}</CheckRow>
        {error !== null ? <FieldError>{t(accountErrorKey(error))}</FieldError> : null}
        <button type="submit" className="hidden" />
      </Form>
    </Dialog>
  )
}

export function ProfileCard({ session, disabled }: { session: AgentSessionDto; disabled: boolean }) {
  const t = useT()
  const [copied, setCopied] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [errorKey, setErrorKey] = useState<string | null>(null)
  const [editing, setEditing] = useState<{ kind: 'nickname' | 'originId'; userId: string } | null>(null)
  const [planOpen, setPlanOpen] = useState(false)
  const showReferral = usePrefBoolean(SHOW_REFERRAL_PREF, false)
  const originEditable = canEditOriginId(session)
  const activeEditing = !disabled && editing?.userId === session.user.id ? editing.kind : null
  useEffect(() => {
    if (disabled || editing?.userId !== session.user.id || (editing.kind === 'originId' && session.entitlements.originIdEdit !== true)) setEditing(null)
  }, [disabled, session.user.id, session.entitlements.originIdEdit, editing])

  useEffect(() => {
    if (!copied) return
    const timer = window.setTimeout(() => setCopied(false), 2000)
    return () => window.clearTimeout(timer)
  }, [copied])


  const name = displayName(session)
  const initial = [...name.trim()][0]?.toUpperCase()
  const originId = session.user.originId

  const refresh = async () => {
    if (refreshing) return
    setRefreshing(true)
    setErrorKey(null)
    try {
      await rpc.agent.auth.refreshProfile()
      await rpc.agent.device.list()
      toast.key('accountCloudRefreshDone', 'success')
    } catch (error) {
      setErrorKey(accountErrorKey(error))
      toast.key(accountErrorKey(error), 'error')
    } finally {
      setRefreshing(false)
    }
  }

  const logout = async () => {
    setErrorKey(null)
    try {
      await rpc.agent.auth.logout()
    } catch (error) {
      setErrorKey(accountErrorKey(error))
    }
  }

  return (
    <Card className="flex w-full flex-col gap-3 p-4">
      <div className="flex flex-wrap items-center gap-3">
        <div className="flex size-12 shrink-0 items-center justify-center rounded-full bg-accent-text/12 text-title font-semibold text-accent-text">
          {initial ?? '·'}
        </div>
        <div className="flex min-w-0 flex-1 basis-48 flex-col items-start gap-1">
          <div className="flex max-w-full items-center gap-2">
            <div className="group/nickname flex min-w-0 items-center gap-0 hover:gap-0.5 focus-within:gap-0.5 coarse:gap-0.5">
              <span className="min-w-0 truncate text-title font-semibold text-foreground">{name}</span>
              <Tooltip content={t('accountNicknameEditTooltip')}>
                <Button variant="ghost" iconOnly className="pointer-events-none w-0 overflow-hidden opacity-0 text-muted-foreground hover:text-foreground group-hover/nickname:pointer-events-auto group-hover/nickname:w-control group-hover/nickname:opacity-100 group-focus-within/nickname:pointer-events-auto group-focus-within/nickname:w-control group-focus-within/nickname:opacity-100 coarse:pointer-events-auto coarse:w-control coarse:opacity-100" aria-label={t('accountNicknameEditTooltip')} disabled={disabled} onClick={() => setEditing({ kind: 'nickname', userId: session.user.id })}>
                  <Icon icon={Pencil} />
                </Button>
              </Tooltip>
            </div>
            {session.currentPlan ? <PlanBadge plan={session.currentPlan} ordinal={session.user.membershipOrdinal} /> : null}
          </div>
          <div className="group/origin flex items-center gap-0.5">
            {originId !== null ? (
              <button
                type="button"
                className="inline-flex items-center gap-1 rounded-full bg-accent-text/12 px-2 py-0.5 text-xs font-medium text-accent-text tabular coarse:min-h-touch"
                onClick={() => {
                  copyText(String(originId))
                  setCopied(true)
                }}
              >
                {copied ? t('accountOriginIdCopied') : `#${originId}`}
                <Icon icon={copied ? Check : Copy} />
              </button>
            ) : (
              <span className="inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground">#—</span>
            )}
            {originEditable ? (
              <Tooltip content={t('accountOriginIdEditTooltip')}>
                <Button variant="ghost" iconOnly className="pointer-events-none opacity-0 text-muted-foreground hover:text-foreground group-hover/origin:pointer-events-auto group-hover/origin:opacity-100 group-focus-within/origin:pointer-events-auto group-focus-within/origin:opacity-100 coarse:pointer-events-auto coarse:opacity-100" aria-label={t('accountOriginIdEditTooltip')} disabled={disabled} onClick={() => setEditing({ kind: 'originId', userId: session.user.id })}>
                  <Icon icon={Pencil} />
                </Button>
              </Tooltip>
            ) : null}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Tooltip content={t('accountCloudRefresh')}>
            <Button variant="ghost" iconOnly className="text-muted-foreground hover:text-foreground" aria-label={t('accountCloudRefresh')} disabled={disabled} loading={refreshing} onClick={() => void refresh()}>
              <Icon icon={RefreshCw} />
            </Button>
          </Tooltip>
          <Button disabled={disabled} onClick={() => void logout()}>
            {t('accountLogout')}
          </Button>
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t border-hairline pt-3">
        <span className="text-xs text-muted-foreground">{t('accountPlanLabel')}</span>
        <span className="min-w-0 truncate text-sm font-medium text-foreground">{session.currentPlan ? session.currentPlan.name || session.currentPlan.code : '—'}</span>
        {session.currentPlan ? <PlanBadge plan={session.currentPlan} ordinal={session.user.membershipOrdinal} /> : null}
        {session.activeSubscription ? (
          <span className="min-w-0 truncate text-xs text-muted-foreground tabular">
            {t('accountSubscriptionUntil', { date: localDate(session.activeSubscription.endsAt) ?? '' })}
          </span>
        ) : null}
        <Button variant="primary" className="ml-auto" disabled={disabled} onClick={() => setPlanOpen(true)}>
          {t('accountPlanUpgrade')}
        </Button>
      </div>
      <div className="flex items-center gap-3 border-t border-hairline pt-3">
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <div className="text-sm font-medium text-foreground">{t('accountFeatureReferral')}</div>
          <div className="text-xs text-muted-foreground">{t('accountFeatureReferralDesc')}</div>
        </div>
        <Switch checked={showReferral} disabled={disabled} onCheckedChange={(next) => setPref(SHOW_REFERRAL_PREF, next)} aria-label={t('accountFeatureReferral')} />
      </div>
      {errorKey ? <FieldError>{t(errorKey)}</FieldError> : null}
      {planOpen && !disabled ? <PlanDialog session={session} onClose={() => setPlanOpen(false)} /> : null}
      {activeEditing === 'nickname' ? <NicknameDialog key={session.user.id} current={session.user.nickname} onClose={() => setEditing(null)} /> : null}
      {activeEditing === 'originId' && session.entitlements.originIdEdit === true ? <OriginIdDialog key={session.user.id} session={session} onClose={() => setEditing(null)} /> : null}
    </Card>
  )
}
