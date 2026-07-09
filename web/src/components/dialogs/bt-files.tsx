// BT 文件选择对话框（对齐 design/web #dlg-bt）—— 由 btRequestStore（WS btSelectionRequest）
// 驱动开关；选择结果经 sendWs({type:'btSelection'}) 回传引擎，全选时按协议发空数组（=全部下载）。

import { useEffect, useRef, useState } from 'react'
import * as Dialog from '@radix-ui/react-dialog'
import { Archive, File, FileText, Film, Image as ImageIcon, Music, X } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { cn } from '../../lib/cn'
import { fileType, fmtBytes, type FileType } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import { btRequestStore, sendWs, useStore } from '../../lib/ws'

const FILE_ICONS: Record<FileType, LucideIcon> = {
  video: Film,
  audio: Music,
  document: FileText,
  image: ImageIcon,
  archive: Archive,
  other: File,
}

export function BtFilesDialog() {
  const { t } = useI18n()
  const request = useStore(btRequestStore)
  const open = request !== null
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const selectAllRef = useRef<HTMLInputElement>(null)

  // 每次新请求到达时，默认全选（对齐原型 BT_FILES 大部分 on:true）。
  useEffect(() => {
    if (request) setSelected(new Set(request.files.map((f) => f.index)))
  }, [request])

  const files = request?.files ?? []
  const totalBytes = files.reduce((s, f) => s + f.size, 0)
  const selectedBytes = files.filter((f) => selected.has(f.index)).reduce((s, f) => s + f.size, 0)
  const allSelected = files.length > 0 && selected.size === files.length

  // 原生 checkbox 的 indeterminate 不是可设置的 JSX 属性，需手动同步 DOM 属性。
  useEffect(() => {
    if (selectAllRef.current) selectAllRef.current.indeterminate = selected.size > 0 && !allSelected
  }, [selected, allSelected])

  function cancel() {
    btRequestStore.set(null)
  }

  function toggle(index: number) {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(index)) next.delete(index)
      else next.add(index)
      return next
    })
  }

  function toggleAll() {
    setSelected((prev) => (prev.size === files.length ? new Set() : new Set(files.map((f) => f.index))))
  }

  function confirm() {
    if (!request) return
    sendWs({
      type: 'btSelection',
      taskId: request.taskId,
      selectedIndices: allSelected ? [] : [...selected].sort((a, b) => a - b),
    })
    btRequestStore.set(null)
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
        <Dialog.Content className="dialog show">
          <header className="dlg-head">
            <Dialog.Title asChild>
              <b>{t('bt.title')}</b>
            </Dialog.Title>
            <Dialog.Close asChild>
              <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                <X size={16} />
              </button>
            </Dialog.Close>
          </header>
          <div className="dlg-body">
            <Dialog.Description className="dlg-sub">
              {t('bt.summary', { n: files.length, size: fmtBytes(totalBytes) })}
            </Dialog.Description>
            <label className="mcheck mb-2">
              <input type="checkbox" ref={selectAllRef} checked={allSelected} onChange={toggleAll} />
              <i />
              {t('common.selectAll')}
            </label>
            <div className="bt-tree">
              {files.map((f) => {
                const Icon = FILE_ICONS[fileType(f.path)]
                const on = selected.has(f.index)
                return (
                  <label key={f.index} className={cn('bt-file', !on && 'off')}>
                    <span className="mcheck">
                      <input type="checkbox" checked={on} onChange={() => toggle(f.index)} />
                      <i />
                    </span>
                    <Icon className="ficon" />
                    <span className="bt-name">{f.path}</span>
                    <span className="bt-size">{fmtBytes(f.size)}</span>
                  </label>
                )
              })}
            </div>
          </div>
          <footer className="dlg-foot">
            <span className="bt-sel">
              {t('bt.selected', { n: selected.size, size: fmtBytes(selectedBytes) })}
            </span>
            <span className="flex1" />
            <Dialog.Close asChild>
              <button type="button" className="btn ghost">
                {t('common.cancel')}
              </button>
            </Dialog.Close>
            <button type="button" className="btn primary" onClick={confirm} disabled={selected.size === 0}>
              {t('common.startDownload')}
            </button>
          </footer>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
