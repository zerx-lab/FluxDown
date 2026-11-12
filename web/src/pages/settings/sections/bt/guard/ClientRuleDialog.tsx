// 防吸血客户端名字规则的新增 / 编辑对话框（设置页与 BT 连接列表「封禁此客户端…」共用）。

import { useState } from 'react'
import { useT } from '../../../../../i18n'
import {
  CLIENT_RULE_ACTIONS,
  CLIENT_RULE_FIELDS,
  CLIENT_RULE_OPS,
  CLIENT_RULE_WHENS,
  DEFAULT_BAN_MINUTES,
  MAX_BAN_MINUTES,
  MAX_REASON_CHARS,
  MAX_VALUE_CHARS,
  upsertClientRule,
} from '../../../../../lib/btGuardClientRules'
import type { ClientRule, ClientRuleAction, ClientRuleField, ClientRuleOp, ClientRuleWhen } from '../../../../../lib/btGuardClientRules'
import { toastRpcError } from '../../../../../lib/rpcToast'
import { ConfirmFooter, Dialog, FormField, Input, Select } from '../../../../../ui'

export const FIELD_LABEL: Record<ClientRuleField, string> = {
  peerIdPrefix: 'btGuardClientRuleFieldPeerId',
  clientName: 'btGuardClientRuleFieldClientName',
}
export const OP_LABEL: Record<ClientRuleOp, string> = {
  prefix: 'btGuardClientRuleOpPrefix',
  contains: 'btGuardClientRuleOpContains',
}
export const ACTION_LABEL: Record<ClientRuleAction, string> = {
  allow: 'btGuardClientRuleActionAllow',
  throttle: 'btGuardClientRuleActionThrottle',
  choke: 'btGuardClientRuleActionChoke',
  ban: 'btGuardClientRuleActionBan',
}
export const WHEN_LABEL: Record<ClientRuleWhen, string> = {
  always: 'btGuardClientRuleWhenAlways',
  seeding: 'btGuardClientRuleWhenSeeding',
  downloading: 'btGuardClientRuleWhenDownloading',
}

/** `isNew` 为真时按新增保存（已有同匹配条件则不重复写入）。 */
export function ClientRuleDialog({ rule, isNew, onClose }: { rule: ClientRule; isNew: boolean; onClose: () => void }) {
  const t = useT()
  const [field, setField] = useState<ClientRuleField>(rule.field)
  const [op, setOp] = useState<ClientRuleOp>(rule.op)
  const [value, setValue] = useState(rule.value)
  const [action, setAction] = useState<ClientRuleAction>(rule.action)
  const [when, setWhen] = useState<ClientRuleWhen>(rule.when)
  const [banMinutes, setBanMinutes] = useState(String(rule.banMinutes))
  const [reason, setReason] = useState(rule.reason)
  const [touched, setTouched] = useState(false)
  const [busy, setBusy] = useState(false)

  const trimmed = value.trim()
  const valueError = trimmed === '' ? t('btGuardClientRuleValueRequired') : null

  const submit = async () => {
    setTouched(true)
    if (valueError !== null) return
    const minutes = Number.parseInt(banMinutes, 10)
    const draft: ClientRule = {
      id: rule.id,
      field,
      op,
      value: Array.from(trimmed).slice(0, MAX_VALUE_CHARS).join(''),
      action,
      when,
      banMinutes: Number.isFinite(minutes) ? Math.min(Math.max(minutes, 1), MAX_BAN_MINUTES) : DEFAULT_BAN_MINUTES,
      reason: Array.from(reason.trim()).slice(0, MAX_REASON_CHARS).join(''),
    }
    setBusy(true)
    try {
      await upsertClientRule(draft, isNew)
      onClose()
    } catch (failure) {
      toastRpcError(failure)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next && !busy) onClose()
      }}
      title={t(isNew ? 'btGuardClientRuleAdd' : 'btGuardClientRuleEdit')}
      modalLocked={busy}
      footer={<ConfirmFooter okLabel={t('confirm')} loading={busy} onCancel={onClose} onOk={() => void submit()} />}
    >
      <form
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault()
          void submit()
        }}
      >
        <div className="grid grid-cols-2 gap-3">
          <Select value={field} aria-label={t('btGuardClientRuleFieldClientName')} options={CLIENT_RULE_FIELDS.map((v) => ({ value: v, label: t(FIELD_LABEL[v]) }))} onValueChange={setField} />
          <Select value={op} aria-label={t('btGuardClientRuleOpContains')} options={CLIENT_RULE_OPS.map((v) => ({ value: v, label: t(OP_LABEL[v]) }))} onValueChange={setOp} />
        </div>
        <FormField label={t('btGuardClientRuleValue')} htmlFor="bt-guard-client-value" error={touched ? valueError : null}>
          <Input
            id="bt-guard-client-value"
            value={value}
            maxLength={MAX_VALUE_CHARS}
            invalid={touched && valueError !== null}
            placeholder={t('btGuardClientRuleValuePlaceholder')}
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
            autoFocus
            onChange={(event) => setValue(event.target.value)}
            onBlur={() => setTouched(true)}
          />
        </FormField>
        <FormField label={t('btGuardClientRuleAction')}>
          <Select value={action} aria-label={t('btGuardClientRuleAction')} options={CLIENT_RULE_ACTIONS.map((v) => ({ value: v, label: t(ACTION_LABEL[v]) }))} onValueChange={setAction} />
        </FormField>
        <FormField label={t('btGuardClientRuleWhen')}>
          <Select value={when} aria-label={t('btGuardClientRuleWhen')} options={CLIENT_RULE_WHENS.map((v) => ({ value: v, label: t(WHEN_LABEL[v]) }))} onValueChange={setWhen} />
        </FormField>
        {action === 'ban' ? (
          <FormField label={t('btGuardClientRuleBanMinutes')} htmlFor="bt-guard-client-ban">
            <Input
              id="bt-guard-client-ban"
              type="number"
              inputMode="numeric"
              min={1}
              max={MAX_BAN_MINUTES}
              value={banMinutes}
              onChange={(event) => setBanMinutes(event.target.value)}
            />
          </FormField>
        ) : null}
        <FormField label={t('btGuardClientRuleReason')} htmlFor="bt-guard-client-reason">
          <Input id="bt-guard-client-reason" value={reason} maxLength={MAX_REASON_CHARS} autoComplete="off" onChange={(event) => setReason(event.target.value)} />
        </FormField>
        <button type="submit" className="hidden" aria-hidden tabIndex={-1} />
      </form>
    </Dialog>
  )
}
