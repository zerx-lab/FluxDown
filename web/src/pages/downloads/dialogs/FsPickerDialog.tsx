// 服务端目录选择器：浏览 daemon 所在主机的目录（`daemon.fs.list`，仅子目录）。
// 桌面 GPUI 用系统目录对话框；Web 里目录在服务器上，浏览器没有等价能力。

import { ArrowUp, Folder, FolderOpen } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { t as translate, useT } from '../../../i18n'
import { rpc } from '../../../lib/rpc'
import { rpcErrorText } from '../../../lib/rpcErrorText'
import type { FsListResponse } from '../../../lib/rpc'
import { Button, ConfirmFooter, Dialog, EmptyState, FieldError, Icon, Input, Spinner } from '../../../ui'

export function FsPickerDialog({
  open,
  onOpenChange,
  initialPath,
  onSelect,
  title,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** 起始目录；空 = 服务端默认保存目录。 */
  initialPath: string
  onSelect: (path: string) => void
  /** 对话框标题；缺省「选择目录」。 */
  title?: string
}) {
  const t = useT()
  const [listing, setListing] = useState<FsListResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const [pathInput, setPathInput] = useState('')
  const requestId = useRef(0)

  const load = useCallback(async (path: string | undefined) => {
    const id = ++requestId.current
    setLoading(true)
    setError('')
    try {
      const result = await rpc.daemon.fs.list(path === undefined || path === '' ? undefined : { path })
      if (id !== requestId.current) return
      setListing(result)
      setPathInput(result.path)
    } catch (err) {
      if (id !== requestId.current) return
      // 失败后旧列表不再对应输入框里的路径，必须作废，否则「确定」会提交上一次成功的目录。
      setListing(null)
      setError(rpcErrorText(err, translate))
    } finally {
      if (id === requestId.current) setLoading(false)
    }
  }, [])

  useEffect(() => {
    if (!open) return
    setListing(null)
    void load(initialPath.trim())
  }, [open, initialPath, load])

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      size="md"
      title={title ?? t('webFsPickTitle')}
      footer={
        <ConfirmFooter
          okLabel={t('confirm')}
          okDisabled={listing === null || listing.denied}
          onCancel={() => onOpenChange(false)}
          onOk={() => {
            if (!listing) return
            onSelect(listing.path)
            onOpenChange(false)
          }}
        />
      }
    >
      <div className="flex flex-col gap-3">
        <form
          className="flex items-center gap-2"
          onSubmit={(event) => {
            event.preventDefault()
            void load(pathInput.trim())
          }}
        >
          <Input value={pathInput} onChange={(event) => setPathInput(event.target.value)} spellCheck={false} aria-label={t('saveDir')} />
          <Button
            variant="outline"
            iconOnly
            title={t('webFsParent')}
            aria-label={t('webFsParent')}
            disabled={loading || listing === null || listing.parent === null}
            onClick={() => listing?.parent != null && void load(listing.parent)}
          >
            <Icon icon={ArrowUp} />
          </Button>
        </form>

        {error ? <FieldError>{error}</FieldError> : null}

        <div className="min-h-[200px] rounded-md border border-hairline">
          {loading && listing === null ? (
            <div className="flex h-[200px] items-center justify-center">
              <Spinner />
            </div>
          ) : listing?.denied ? (
            <EmptyState icon={FolderOpen} title={t('webFsDenied')} />
          ) : listing && listing.dirs.length === 0 ? (
            <EmptyState icon={FolderOpen} title={t('webFsEmpty')} />
          ) : (
            <ul className="max-h-[320px] overflow-y-auto p-1">
              {listing?.dirs.map((dir) => (
                <li key={dir.path}>
                  <button
                    type="button"
                    className="flex min-h-control w-full items-center gap-2 rounded-sm px-2 text-left text-sm hover:bg-nav-hover coarse:min-h-touch"
                    onClick={() => void load(dir.path)}
                  >
                    <Icon icon={Folder} className="text-muted-foreground" />
                    <span className="min-w-0 flex-1 truncate">{dir.name}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </Dialog>
  )
}
