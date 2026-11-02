import { useState } from 'react'
import { useT } from '../../../../../i18n'
import { GUARD_DURATION_IDS, durationSecs, parseCidr } from '../../../../../lib/btGuard'
import type { GuardDurationId } from '../../../../../lib/btGuard'
import { BT_GUARD_ENTRY_ACTIONS, rpc } from '../../../../../lib/rpc'
import type { BtGuardEntryAction } from '../../../../../lib/rpc'
import { toastRpcError } from '../../../../../lib/rpcToast'
import { camel } from '../../../kit'
import { ConfirmFooter, Dialog, FormField, Input, Select, toast } from '../../../../../ui'

/** 手动添加封禁 / 限速 / 加白条目：CIDR 校验、动作、时长预设、备注。 */
export function AddEntryDialog({ open, onOpenChange, onAdded }: { open: boolean; onOpenChange: (open: boolean) => void; onAdded: () => void }) {
  const t = useT()
  const [cidr, setCidr] = useState('')
  const [action, setAction] = useState<BtGuardEntryAction>('ban')
  const [duration, setDuration] = useState<GuardDurationId>('forever')
  const [reason, setReason] = useState('')
  const [touched, setTouched] = useState(false)
  const [busy, setBusy] = useState(false)

  const check = parseCidr(cidr)
  const error = check.ok ? null : check.issue === 'empty' ? t('btGuardErrCidrEmpty') : t('btGuardErrCidrInvalid')

  const reset = () => {
    setCidr('')
    setAction('ban')
    setDuration('forever')
    setReason('')
    setTouched(false)
  }

  const submit = async () => {
    setTouched(true)
    if (!check.ok) return
    setBusy(true)
    try {
      const secs = durationSecs(duration)
      await rpc.daemon.bt.guard.entries.add({
        cidr: check.cidr,
        action,
        ...(secs === undefined ? {} : { durationSecs: secs }),
        reason: reason.trim(),
      })
      toast.key('btGuardAdded', 'success', { cidr: check.cidr })
      reset()
      onOpenChange(false)
      onAdded()
    } catch (failure) {
      toastRpcError(failure)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!busy) onOpenChange(next)
      }}
      title={t('btGuardAddTitle')}
      footer={<ConfirmFooter okLabel={t('btGuardAddConfirm')} loading={busy} onCancel={() => onOpenChange(false)} onOk={() => void submit()} />}
    >
      <form
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault()
          void submit()
        }}
      >
        <FormField label={t('btGuardAddCidr')} htmlFor="bt-guard-cidr" error={touched ? error : null}>
          <Input
            id="bt-guard-cidr"
            value={cidr}
            invalid={touched && error !== null}
            placeholder={t('btGuardAddCidrPlaceholder')}
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
            onChange={(event) => setCidr(event.target.value)}
            onBlur={() => setTouched(true)}
          />
        </FormField>
        <FormField label={t('btGuardAddAction')}>
          <Select
            value={action}
            aria-label={t('btGuardAddAction')}
            options={BT_GUARD_ENTRY_ACTIONS.map((value) => ({ value, label: t(`btGuardAction${camel(value)}`) }))}
            onValueChange={setAction}
          />
        </FormField>
        <FormField label={t('btGuardAddDuration')}>
          <Select
            value={duration}
            aria-label={t('btGuardAddDuration')}
            options={GUARD_DURATION_IDS.map((value) => ({ value, label: t(`btGuardDuration${camel(value)}`) }))}
            onValueChange={setDuration}
          />
        </FormField>
        <FormField label={t('btGuardAddReason')} htmlFor="bt-guard-reason">
          <Input id="bt-guard-reason" value={reason} maxLength={200} autoComplete="off" onChange={(event) => setReason(event.target.value)} />
        </FormField>
        <button type="submit" className="hidden" aria-hidden tabIndex={-1} />
      </form>
    </Dialog>
  )
}
