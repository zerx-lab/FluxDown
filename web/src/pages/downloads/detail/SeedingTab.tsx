import { useState } from 'react'
import { byteUnit, useFileSizeUnits } from '../../../lib/fileSizeUnits'
import { useT } from '../../../i18n'
import { rpc } from '../../../lib/rpc'
import { Button, FormField, Input, Select, toast } from '../../../ui'
import { toastRpcError } from '../../../lib/rpcToast'
import { formatBytes } from '../model/task'
import type { DownloadTaskView } from '../model/task'
import { DetailRow } from './DetailRow'
import type { Translate } from '../table/text'
import { isFieldValid, loadSeedLimit, seedLimitToSend } from './seedLimits'
import type { SeedLimitField, SeedLimitKind, SeedMode } from './seedLimits'
import { isUploadValid, uploadLimitText, uploadLimitToSend } from './seedUpload'

const STATUS_KEY: Record<number, string> = {
  1: 'seedingStatusSeeding',
  2: 'seedingStatusRatioReached',
  3: 'seedingStatusTimeReached',
  4: 'seedingStatusUserStopped',
  5: 'seedingStatusDeleted',
  6: 'seedingStatusSessionReleased',
  7: 'seedingStatusInactiveReached',
  8: 'seedingStatusQueued',
}

function formatDuration(t: Translate, totalSeconds: number): string {
  const minutes = Math.floor(Math.max(0, totalSeconds) / 60)
  if (minutes < 60) return `${minutes} ${t('timeUnitMinutes')}`
  return `${Math.floor(minutes / 60)} ${t('timeUnitHours')} ${minutes % 60} ${t('timeUnitMinutes')}`
}

/** 模式下拉 + 数值框：数值框仅自定义模式可编辑，非自定义显示 qBit 默认值。 */
function LimitField({
  t,
  label,
  kind,
  value,
  onChange,
}: {
  t: Translate
  label: string
  kind: SeedLimitKind
  value: SeedLimitField
  onChange: (next: SeedLimitField) => void
}) {
  const options: { value: SeedMode; label: string }[] = [
    { value: 'global', label: t('btSeedLimitsModeGlobal') },
    { value: 'unlimited', label: t('btSeedLimitsModeUnlimited') },
    { value: 'custom', label: t('btSeedLimitsModeCustom') },
  ]
  return (
    <FormField label={label}>
      <div className="flex items-center gap-2">
        <div className="w-32 shrink-0">
          <Select<SeedMode> aria-label={label} value={value.mode} options={options} onValueChange={(mode) => onChange({ ...value, mode })} />
        </div>
        <Input
          inputMode="decimal"
          className="min-w-0 flex-1"
          value={value.text}
          disabled={value.mode !== 'custom'}
          invalid={!isFieldValid(kind, value)}
          onChange={(e) => onChange({ ...value, text: e.target.value })}
        />
        <span className="shrink-0 text-xs text-muted-foreground">{kind === 'minutes' ? t('timeUnitMinutes') : t('btSeedRatioUnit')}</span>
      </div>
    </FormField>
  )
}

export function SeedingTab({ view }: { view: DownloadTaskView }) {
  const t = useT()
  const fileSizeUnits = useFileSizeUnits()
  const dto = view.dto
  const [initial] = useState(() => ({
    ratio: loadSeedLimit('ratio', dto?.seedRatioLimitMilli),
    postRatio: loadSeedLimit('ratio', dto?.seedPostRatioLimitMilli),
    seedTime: loadSeedLimit('minutes', dto?.seedTimeLimitMinutes),
    inactive: loadSeedLimit('minutes', dto?.seedInactiveTimeLimitMinutes),
    upload: uploadLimitText(dto?.seedUploadLimitBps),
  }))
  const [ratio, setRatio] = useState(initial.ratio)
  const [postRatio, setPostRatio] = useState(initial.postRatio)
  const [seedTime, setSeedTime] = useState(initial.seedTime)
  const [inactive, setInactive] = useState(initial.inactive)
  const [upload, setUpload] = useState(initial.upload)
  const [saving, setSaving] = useState(false)

  const uploaded = Math.max(0, view.uploadedBytes)
  const ratioLabel = view.sizeBytes > 0 ? (uploaded / view.sizeBytes).toFixed(2) : '—'
  const statusKey = STATUS_KEY[view.seedingStatus] ?? 'seedingStatusNone'
  const valid =
    isFieldValid('ratio', ratio) &&
    isFieldValid('ratio', postRatio) &&
    isFieldValid('minutes', seedTime) &&
    isFieldValid('minutes', inactive) &&
    isUploadValid(upload)

  const save = () => {
    const ratioLimitMilli = seedLimitToSend('ratio', ratio, initial.ratio, dto?.seedRatioLimitMilli)
    const postRatioLimitMilli = seedLimitToSend('ratio', postRatio, initial.postRatio, dto?.seedPostRatioLimitMilli)
    const seedTimeLimitMinutes = seedLimitToSend('minutes', seedTime, initial.seedTime, dto?.seedTimeLimitMinutes)
    const inactiveTimeLimitMinutes = seedLimitToSend('minutes', inactive, initial.inactive, dto?.seedInactiveTimeLimitMinutes)
    if (ratioLimitMilli === null || postRatioLimitMilli === null || seedTimeLimitMinutes === null || inactiveTimeLimitMinutes === null) return
    setSaving(true)
    rpc.daemon.task
      .setSeedLimits({
        taskId: view.taskId,
        ratioLimitMilli,
        postRatioLimitMilli,
        seedTimeLimitMinutes,
        inactiveTimeLimitMinutes,
        uploadLimitBps: uploadLimitToSend(upload, initial.upload, dto?.seedUploadLimitBps),
      })
      .then(() => toast.key('btSeedLimitsSaved', 'success'))
      .catch(toastRpcError)
      .finally(() => setSaving(false))
  }

  return (
    <div className="flex flex-col">
      <DetailRow label={t('seedingStatus')}>{t(statusKey)}</DetailRow>
      <DetailRow label={t('uploadedTotal')}>{formatBytes(uploaded, fileSizeUnits)}</DetailRow>
      <DetailRow label={t('seedRatio')}>{ratioLabel}</DetailRow>
      <DetailRow label={t('seedTime')}>{formatDuration(t, dto?.seedingTimeSecs ?? 0)}</DetailRow>
      <div className="my-3 h-px bg-hairline" />
      <div className="pb-3 text-caption font-medium text-text-tertiary">{t('btSeedLimitsTitle')}</div>
      <div className="flex flex-col gap-3">
        <LimitField t={t} label={t('btSeedRatioLimit')} kind="ratio" value={ratio} onChange={setRatio} />
        <LimitField t={t} label={t('btSeedPostRatioLimit')} kind="ratio" value={postRatio} onChange={setPostRatio} />
        <LimitField t={t} label={t('btSeedTimeLimit')} kind="minutes" value={seedTime} onChange={setSeedTime} />
        <LimitField t={t} label={t('btSeedInactiveTimeLimit')} kind="minutes" value={inactive} onChange={setInactive} />
        <FormField label={`${t('btSeedUploadLimit')} (${byteUnit(1, fileSizeUnits)}/s)`} hint={t('btSeedUploadLimitHint')}>
          <Input inputMode="numeric" value={upload} invalid={!isUploadValid(upload)} onChange={(e) => setUpload(e.target.value)} />
        </FormField>
        <div className="flex justify-end">
          <Button variant="primary" loading={saving} disabled={!valid} onClick={save}>
            {t('confirm')}
          </Button>
        </div>
      </div>
    </div>
  )
}

