// 应用内更新的全局呈现：移动端内容区顶部横幅（可按版本忽略；桌面由状态栏版本条目 `VersionControl` 承担）
// + 安装 / 重启中的全屏遮罩。
// 状态唯一来源是 `snapshot.update`；重启期间的 `restarting` 来自 rpc 连接层（关闭原因 service-restart），
// 新版本握手成功后由连接层刷新页面（见 lib/rpc/reload.ts）。

import { CircleArrowUp, X } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../i18n'
import { rpc, useConnection } from '../lib/rpc'
import { rpcErrorText } from '../lib/rpcErrorText'
import {
  bannerVisible,
  canCancel,
  canInstall,
  ignoreVersion,
  manualLine,
  manualUrl,
  statusLine,
  updateLineText,
  useIgnoredVersion,
  useUpdateStatus,
} from '../lib/update'
import { Button, Icon, Spinner, toast } from '../ui'

/** 有新版本时的移动端横幅；Ready / Downloading / 手动升级三种形态。 */
export function UpdateBanner() {
  const t = useT()
  const status = useUpdateStatus()
  const connection = useConnection()
  const dismissed = useIgnoredVersion()
  const [busy, setBusy] = useState(false)
  if (!status || !bannerVisible(status, dismissed)) return null

  const disabled = connection.phase !== 'ready'
  const line = statusLine(status)
  const manual = manualLine(status)
  const url = manualUrl(status)
  const text = manual ?? line
  if (!text) return null

  const run = async (action: () => Promise<unknown>) => {
    if (busy) return
    setBusy(true)
    try {
      await action()
    } catch (error) {
      toast.error(rpcErrorText(error, t))
    } finally {
      setBusy(false)
    }
  }
  const dismiss = () => ignoreVersion(status.latestVersion)

  return (
    <div role="status" className="flex shrink-0 items-center gap-2 border-b border-hairline bg-accent/10 px-3 py-1.5 text-xs text-foreground">
      <Icon icon={CircleArrowUp} size="md" />
      <span className="min-w-0 flex-1 truncate">{updateLineText(t, text)}</span>
      {canCancel(status) ? (
        <Button disabled={disabled || busy} onClick={() => void run(() => rpc.agent.update.cancel())}>
          {t('cancel')}
        </Button>
      ) : null}
      {canInstall(status) ? (
        <Button variant="primary" loading={busy} disabled={disabled} onClick={() => void run(() => rpc.agent.update.install())}>
          {t('updateRestartNow')}
        </Button>
      ) : null}
      {url ? (
        <Button variant="primary" onClick={() => window.open(url, '_blank', 'noopener,noreferrer')}>
          {t('updateFailedOpenSite')}
        </Button>
      ) : null}
      <Button variant="ghost" iconOnly icon={X} aria-label={t('close')} onClick={dismiss} />
    </div>
  )
}

/** 安装中 / 服务重启中的全屏遮罩；页面在新版本就绪后自动刷新。 */
export function UpdateRestartOverlay() {
  const t = useT()
  const status = useUpdateStatus()
  const connection = useConnection()
  if (status?.phase !== 'installing' && !connection.restarting) return null
  return (
    <div
      role="alertdialog"
      aria-modal="true"
      aria-live="polite"
      className="fixed inset-0 z-[80] flex flex-col items-center justify-center gap-4 bg-background/95 px-6 text-center backdrop-blur-sm"
    >
      <Spinner />
      <h2 className="text-title font-semibold text-foreground">{t('updateRestartingTitle')}</h2>
      <p className="max-w-md text-sm text-muted-foreground">{t('updateRestartingBody')}</p>
    </div>
  )
}
