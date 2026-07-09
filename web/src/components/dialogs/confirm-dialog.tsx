// 全局确认/提示对话框 —— 订阅 confirmStore，替代原生 window.confirm / window.alert。
// 由 confirmDialog(...) / alertDialog(...) 命令式驱动开关，结果经 store 的 resolve 回传。
// 危险操作（danger）用红色警告图标 + 实心红底确认按钮强调；提示用蓝色信息图标。

import * as Dialog from '@radix-ui/react-dialog'
import { AlertTriangle, Info } from 'lucide-react'
import { cn } from '../../lib/cn'
import { confirmStore } from '../../lib/confirm'
import { useStore } from '../../lib/ws'

export function ConfirmDialog() {
  const state = useStore(confirmStore)
  const open = state !== null
  const danger = state?.danger ?? false

  function close(ok: boolean) {
    const cur = confirmStore.get()
    if (!cur) return
    confirmStore.set(null)
    cur.resolve(ok)
  }

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(o) => {
        if (!o) close(false)
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="wbackdrop show" />
        <Dialog.Content className="dialog confirm-dlg show">
          <div className="dlg-body">
            <div className="confirm-row">
              <div className={cn('confirm-icon', danger ? 'danger' : 'info')}>
                {danger ? <AlertTriangle size={20} /> : <Info size={20} />}
              </div>
              <div className="confirm-text">
                <Dialog.Title asChild>
                  <b>{state?.title}</b>
                </Dialog.Title>
                <Dialog.Description asChild>
                  <p>{state?.message}</p>
                </Dialog.Description>
              </div>
            </div>
          </div>
          <footer className="dlg-foot">
            {state?.kind === 'confirm' && (
              <button type="button" className="btn ghost" onClick={() => close(false)}>
                {state?.cancelLabel}
              </button>
            )}
            <button
              type="button"
              className={cn('btn', danger ? 'danger solid' : 'primary')}
              onClick={() => close(true)}
              autoFocus
            >
              {state?.confirmLabel}
            </button>
          </footer>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
