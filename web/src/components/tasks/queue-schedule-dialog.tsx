// 队列每日定时启停对话框 —— 队列行 hover 时的时钟图标触发；对齐 design.css .dialog.sm/.set-row。

import { useEffect, useState } from 'react'
import * as Dialog from '@radix-ui/react-dialog'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Clock, X } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { useI18n, type I18nKey } from '../../lib/i18n'
import type { QueueDto } from '../../lib/types'
import { SetSwitch } from '../settings/controls'

const DAY_KEYS: I18nKey[] = ['sidebar.day0', 'sidebar.day1', 'sidebar.day2', 'sidebar.day3', 'sidebar.day4', 'sidebar.day5', 'sidebar.day6']

export function QueueScheduleDialog({ queue, queueName }: { queue: QueueDto; queueName: string }) {
  const { t } = useI18n()
  const qc = useQueryClient()
  const [open, setOpen] = useState(false)
  const [enabled, setEnabled] = useState(queue.scheduleEnabled)
  const [start, setStart] = useState(queue.scheduleStart)
  const [stop, setStop] = useState(queue.scheduleStop)
  const [days, setDays] = useState(queue.scheduleDays)

  // 每次打开都从队列当前已保存的定时配置重置表单。
  useEffect(() => {
    if (open) {
      setEnabled(queue.scheduleEnabled)
      setStart(queue.scheduleStart)
      setStop(queue.scheduleStop)
      setDays(queue.scheduleDays)
    }
  }, [open, queue])

  const save = useMutation({
    mutationFn: () => api.setQueueSchedule(queue.queueId, { enabled, startTime: start, stopTime: stop, days }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['queues'] })
      setOpen(false)
    },
  })

  function toggleDay(bit: number) {
    setDays((d) => d ^ (1 << bit))
  }

  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button
          type="button"
          className="icon-btn sm"
          title={t('sidebar.queueSchedule')}
          onClick={(e) => e.stopPropagation()}
        >
          <Clock size={13} />
        </button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="wbackdrop show" />
        <Dialog.Content
          asChild
          onClick={(e) => e.stopPropagation()}
          onPointerDownOutside={(e) => e.preventDefault()}
        >
          <div className="dialog sm show">
            <header className="dlg-head">
              <Dialog.Title asChild>
                <b>{t('sidebar.scheduleTitle', { name: queueName })}</b>
              </Dialog.Title>
              <Dialog.Close asChild>
                <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                  <X size={16} />
                </button>
              </Dialog.Close>
            </header>
            <Dialog.Description className="sr-only">{t('sidebar.queueSchedule')}</Dialog.Description>
            <div className="dlg-body">
              <div className="set-row" style={{ padding: '4px 0' }}>
                <div className="set-info">
                  <b>{t('sidebar.scheduleEnable')}</b>
                </div>
                <SetSwitch checked={enabled} onCheckedChange={setEnabled} />
              </div>
              <div className="time-row" style={{ marginTop: 10 }}>
                <div>
                  <label className="field-label" htmlFor="q-sched-start">
                    {t('sidebar.scheduleStart')}
                  </label>
                  <input
                    id="q-sched-start"
                    className="text-input"
                    type="time"
                    value={start}
                    disabled={!enabled}
                    onChange={(e) => setStart(e.target.value)}
                  />
                </div>
                <div>
                  <label className="field-label" htmlFor="q-sched-stop">
                    {t('sidebar.scheduleStop')}
                  </label>
                  <input
                    id="q-sched-stop"
                    className="text-input"
                    type="time"
                    value={stop}
                    disabled={!enabled}
                    onChange={(e) => setStop(e.target.value)}
                  />
                </div>
              </div>
              <label className="field-label" style={{ marginTop: 10 }}>
                {t('sidebar.scheduleDays')}
              </label>
              <div className="day-row">
                {DAY_KEYS.map((key, bit) => (
                  <button
                    key={key}
                    type="button"
                    className={cn('day-chip', (days & (1 << bit)) !== 0 && 'active')}
                    disabled={!enabled}
                    onClick={() => toggleDay(bit)}
                  >
                    {t(key)}
                  </button>
                ))}
              </div>
            </div>
            <footer className="dlg-foot">
              <Dialog.Close asChild>
                <button type="button" className="btn ghost">
                  {t('common.cancel')}
                </button>
              </Dialog.Close>
              <button type="button" className="btn primary" disabled={save.isPending} onClick={() => save.mutate()}>
                {save.isPending ? t('common.loading') : t('sidebar.scheduleSave')}
              </button>
            </footer>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
