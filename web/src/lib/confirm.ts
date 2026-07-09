// 命令式确认/提示对话框的全局 store —— 替代 window.confirm / window.alert。
// 调用 confirmDialog(...) 返回 Promise<boolean>（确定=true，取消/关闭=false）；
// alertDialog(...) 返回 Promise<void>（关闭即 resolve）。由
// components/dialogs/confirm-dialog.tsx 订阅渲染，全局挂载于 ThemeProvider 内。

import { Store } from './ws'

export interface ConfirmState {
  id: number
  kind: 'confirm' | 'alert'
  title: string
  message: string
  // 确认按钮文案（confirm 默认「确定」，alert 默认「知道了」）。
  confirmLabel: string
  // 取消按钮文案（仅 confirm 显示）。
  cancelLabel: string
  // 确认按钮是否使用危险样式（删除类操作）。
  danger: boolean
  resolve: (ok: boolean) => void
}

export const confirmStore = new Store<ConfirmState | null>(null)

let seq = 0

export interface ConfirmOptions {
  title?: string
  message: string
  confirmLabel?: string
  cancelLabel?: string
  danger?: boolean
}

// 弹出确认框，返回用户选择。同一时刻仅保留最后一次请求。
export function confirmDialog(opts: ConfirmOptions): Promise<boolean> {
  const prev = confirmStore.get()
  if (prev) prev.resolve(false)
  return new Promise<boolean>((resolve) => {
    confirmStore.set({
      id: ++seq,
      kind: 'confirm',
      title: opts.title ?? '确认操作',
      message: opts.message,
      confirmLabel: opts.confirmLabel ?? '确定',
      cancelLabel: opts.cancelLabel ?? '取消',
      danger: opts.danger ?? false,
      resolve,
    })
  })
}

// 弹出提示框（单按钮），关闭后 resolve。
export function alertDialog(opts: Omit<ConfirmOptions, 'cancelLabel' | 'danger'>): Promise<void> {
  const prev = confirmStore.get()
  if (prev) prev.resolve(false)
  return new Promise<void>((resolve) => {
    confirmStore.set({
      id: ++seq,
      kind: 'alert',
      title: opts.title ?? '提示',
      message: opts.message,
      confirmLabel: opts.confirmLabel ?? '知道了',
      cancelLabel: '',
      danger: false,
      resolve: () => resolve(),
    })
  })
}
