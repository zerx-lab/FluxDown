// 服务器目录选择器 —— 独立的模态对话框（嵌套 Radix Dialog，z 层级压过父弹窗），
// 经 api.fsList 浏览服务器文件系统：面包屑逐级跳转 + 子目录列表点击进入，
// 「选择此目录」把当前解析路径回传给父表单（保存目录输入框）。

import { useEffect, useRef, useState } from 'react'
import * as Dialog from '@radix-ui/react-dialog'
import { useQuery } from '@tanstack/react-query'
import { ChevronLeft, ChevronRight, Folder, FolderOpen, FolderX, X } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { useI18n } from '../../lib/i18n'

interface FsPickerProps {
  value: string
  onChange: (path: string) => void
}

interface Crumb {
  label: string
  path: string
}

/** 把绝对路径拆成可点击的面包屑（兼容 Windows 盘符与 Unix 根）。 */
function crumbsOf(path: string): Crumb[] {
  const norm = path.replaceAll('\\', '/')
  const parts = norm.split('/').filter(Boolean)
  const crumbs: Crumb[] = []
  if (norm.startsWith('/')) {
    crumbs.push({ label: '/', path: '/' })
    let acc = ''
    for (const part of parts) {
      acc += `/${part}`
      crumbs.push({ label: part, path: acc })
    }
  } else {
    let acc = ''
    for (const part of parts) {
      // Windows 盘符段（"C:"）单独指向盘根 "C:\"。
      acc = acc === '' ? `${part}\\` : acc.endsWith('\\') ? `${acc}${part}` : `${acc}\\${part}`
      crumbs.push({ label: part, path: acc })
    }
  }
  return crumbs
}

export function FsPicker({ value, onChange }: FsPickerProps) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [path, setPath] = useState(value)
  const crumbsRef = useRef<HTMLDivElement>(null)

  // 每次打开都从表单当前值重新开始浏览。
  useEffect(() => {
    if (open) setPath(value)
  }, [open, value])

  const { data, isLoading, isError } = useQuery({
    queryKey: ['fsList', path],
    queryFn: () => api.fsList(path),
    enabled: open,
  })

  const resolved = data?.path ?? path
  const crumbs = crumbsOf(resolved)

  // 路径变深时面包屑自动滚到最右，保证当前层级始终可见。
  useEffect(() => {
    const el = crumbsRef.current
    if (el) el.scrollLeft = el.scrollWidth
  }, [resolved])

  function choose() {
    onChange(resolved)
    setOpen(false)
  }

  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button type="button" className="btn ghost">
          <Folder size={15} />
          {t('fs.browse')}
        </button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="wbackdrop fs-backdrop show" />
        <Dialog.Content className="dialog fs-dialog show">
          <header className="dlg-head">
            <Dialog.Title asChild>
              <b>{t('fs.title')}</b>
            </Dialog.Title>
            <Dialog.Close asChild>
              <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                <X size={16} />
              </button>
            </Dialog.Close>
          </header>
          <Dialog.Description className="sr-only">{t('fs.desc')}</Dialog.Description>
          <div className="dlg-body">
            <div className="fs-bar">
              <button
                type="button"
                className="icon-btn sm shrink-0"
                disabled={data?.parent == null}
                onClick={() => {
                  if (data?.parent != null) setPath(data.parent)
                }}
                title={t('fs.up')}
                aria-label={t('fs.up')}
              >
                <ChevronLeft size={14} />
              </button>
              <div ref={crumbsRef} className="fs-crumbs" title={resolved}>
                {crumbs.map((c, i) => {
                  const isLast = i === crumbs.length - 1
                  return (
                    <span key={c.path} className="contents">
                      {i > 0 && (
                        <i className="fs-crumb-sep" aria-hidden>
                          <ChevronRight />
                        </i>
                      )}
                      <button
                        type="button"
                        className={cn('fs-crumb', isLast && 'cur')}
                        disabled={isLast}
                        onClick={() => setPath(c.path)}
                      >
                        {c.label}
                      </button>
                    </span>
                  )
                })}
              </div>
            </div>
            <div className="fs-list">
              {isLoading && <div className="fs-empty">{t('common.loading')}</div>}
              {isError && (
                <div className="fs-empty">
                  <FolderX />
                  {t('fs.loadFailed')}
                </div>
              )}
              {!isLoading && !isError && data?.dirs.length === 0 && (
                <div className="fs-empty">
                  <FolderOpen />
                  {t('fs.emptyDir')}
                </div>
              )}
              {data?.dirs.map((d) => (
                <button key={d.path} type="button" className="fs-row" onClick={() => setPath(d.path)}>
                  <Folder />
                  <span className="fs-name">{d.name}</span>
                  <ChevronRight size={13} className="fs-go" />
                </button>
              ))}
            </div>
          </div>
          <footer className="dlg-foot">
            <span className="fs-foot-path" title={resolved}>
              {resolved}
            </span>
            <Dialog.Close asChild>
              <button type="button" className="btn ghost">
                {t('common.cancel')}
              </button>
            </Dialog.Close>
            <button type="button" className="btn primary" onClick={choose}>
              {t('fs.choose')}
            </button>
          </footer>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
