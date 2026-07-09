// HLS 画质选择对话框（对齐 design/web #dlg-hls）—— 由 hlsRequestStore（WS hlsSelectionRequest）
// 驱动开关；选择结果经 sendWs({type:'hlsSelection'}) 回传引擎。服务端 60 秒未选会自动取最高带宽。

import { useEffect, useState } from 'react'
import * as Dialog from '@radix-ui/react-dialog'
import { X } from 'lucide-react'
import { cn } from '../../lib/cn'
import { useI18n } from '../../lib/i18n'
import { hlsRequestStore, sendWs, useStore } from '../../lib/ws'

export function HlsQualityDialog() {
  const { t } = useI18n()
  const request = useStore(hlsRequestStore)
  const open = request !== null
  const [selected, setSelected] = useState<number | null>(null)

  // 每次新请求到达时，默认高亮带宽最高的档位。
  useEffect(() => {
    if (!request) return
    let best = request.options[0] ?? null
    for (const opt of request.options) {
      if (best === null || opt.bandwidth > best.bandwidth) best = opt
    }
    setSelected(best?.index ?? null)
  }, [request])

  function cancel() {
    hlsRequestStore.set(null)
  }

  function confirm() {
    if (!request || selected === null) return
    sendWs({ type: 'hlsSelection', taskId: request.taskId, selectedIndex: selected })
    hlsRequestStore.set(null)
  }

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(o) => {
        if (!o) cancel()
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="wbackdrop show" />
        <Dialog.Content className="dialog sm show">
          <header className="dlg-head">
            <Dialog.Title asChild>
              <b>{t('hls.title')}</b>
            </Dialog.Title>
            <Dialog.Close asChild>
              <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                <X size={16} />
              </button>
            </Dialog.Close>
          </header>
          <div className="dlg-body">
            <Dialog.Description className="dlg-sub">
              {t('hls.desc', { n: request?.options.length ?? 0 })}
            </Dialog.Description>
            <div className="pick-list">
              {request?.options.map((opt) => (
                <button
                  key={opt.index}
                  type="button"
                  className={cn('pick', selected === opt.index && 'active')}
                  onClick={() => setSelected(opt.index)}
                >
                  <b>{opt.height > 0 ? `${opt.height}p` : t('hls.variant', { n: opt.index + 1 })}</b>
                  <span>{(opt.bandwidth / 1e6).toFixed(1)} Mbps</span>
                  <i className="pick-dot" />
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
            <button type="button" className="btn primary" onClick={confirm} disabled={selected === null}>
              {t('common.startDownload')}
            </button>
          </footer>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
