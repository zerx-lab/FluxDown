// resolve 变体（画质/格式）选择对话框（对齐 hls-quality.tsx）—— 由 resolveVariantRequestStore
// （WS resolveVariantRequest）驱动开关；选择结果经 sendWs({type:'selectVariant'}) 回传引擎。服务端
// 60 秒未选会自动采用插件提供的默认变体（defaultIndex）。

import { useEffect, useState } from 'react'
import * as Dialog from '@radix-ui/react-dialog'
import { X } from 'lucide-react'
import { cn } from '../../lib/cn'
import { fmtBytes } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import { resolveVariantRequestStore, sendWs, useStore } from '../../lib/ws'
import type { ResolveVariantOption } from '../../lib/types'

/** 副文案：container/分辨率/码率/大小，逐项省略 0/空值。 */
function variantSubtitle(opt: ResolveVariantOption): string {
  const parts: string[] = []
  if (opt.container) parts.push(opt.container.toUpperCase())
  if (opt.width > 0 && opt.height > 0) parts.push(`${opt.width}x${opt.height}`)
  if (opt.bandwidth > 0) parts.push(`${(opt.bandwidth / 1e6).toFixed(1)} Mbps`)
  if (opt.totalBytes > 0) parts.push(fmtBytes(opt.totalBytes))
  return parts.join(' · ')
}

export function ResolveVariantDialog() {
  const { t } = useI18n()
  const request = useStore(resolveVariantRequestStore)
  const open = request !== null
  const [selected, setSelected] = useState<number | null>(null)

  // 每次新请求到达时，默认高亮插件提供的 defaultIndex。
  useEffect(() => {
    if (!request) return
    setSelected(request.defaultIndex)
  }, [request])

  function cancel() {
    resolveVariantRequestStore.set(null)
  }

  function confirm() {
    if (!request || selected === null) return
    sendWs({ type: 'selectVariant', taskId: request.taskId, selectedIndex: selected })
    resolveVariantRequestStore.set(null)
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
              <b>{t('resolveVariant.title')}</b>
            </Dialog.Title>
            <Dialog.Close asChild>
              <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                <X size={16} />
              </button>
            </Dialog.Close>
          </header>
          <div className="dlg-body">
            <Dialog.Description className="dlg-sub">
              {t('resolveVariant.desc', { n: request?.options.length ?? 0 })}
            </Dialog.Description>
            <div className="pick-list">
              {request?.options.map((opt) => {
                const subtitle = variantSubtitle(opt)
                return (
                  <button
                    key={opt.index}
                    type="button"
                    className={cn('pick', selected === opt.index && 'active')}
                    onClick={() => setSelected(opt.index)}
                  >
                    <b>{opt.label || t('resolveVariant.variant', { n: opt.index + 1 })}</b>
                    {subtitle && <span>{subtitle}</span>}
                    <i className="pick-dot" />
                  </button>
                )
              })}
            </div>
          </div>
          <footer className="dlg-foot">
            <Dialog.Close asChild>
              <button type="button" className="btn ghost">
                {t('common.cancel')}
              </button>
            </Dialog.Close>
            <button type="button" className="btn primary" onClick={confirm} disabled={selected === null}>
              {t('common.confirm')}
            </button>
          </footer>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
