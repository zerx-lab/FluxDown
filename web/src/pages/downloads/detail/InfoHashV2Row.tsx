import { useEffect, useState } from 'react'
import { useT } from '../../../i18n'
import { rpc } from '../../../lib/rpc'
import { DetailRow } from './DetailRow'
import { CopyButton } from './CopyButton'
import { BT_STATUS_POLL_MS, infoHashV2, splitMiddle } from './btStatus'

/**
 * 常规页 v2 info hash 行（GPUI `info_hash_v2`）：v1 种子 / 旧 daemon / 未取得元数据时不显示。
 * hash 只在元数据到手时出现、此后不变，因此按「任务 + 是否已有元数据」取一次，不随常规页轮询。
 * `hasMetadata` 须在元数据真正到手时翻转：磁力带 `dn=` 时名称先于元数据写入，只看名称会过早取一次
 * （此时没有 hash）且再也不重取，所以调用方要同时看总大小。取失败时该行保持隐藏，并按 BT 实况的
 * 轮询间隔重试到成功为止（成功但无 v2 hash 则不再取）；切换任务 / 卸载即停止并丢弃迟到响应。
 */
export function InfoHashV2Row({ taskId, hasMetadata }: { taskId: string; hasMetadata: boolean }) {
  const t = useT()
  const [state, setState] = useState<{ taskId: string; hash: string | null }>({ taskId, hash: null })

  useEffect(() => {
    if (!hasMetadata) return
    let alive = true
    let timer: number | undefined
    const load = () => {
      rpc.daemon.task.btStatus({ taskId }).then(
        (status) => {
          if (alive) setState({ taskId, hash: infoHashV2(status) })
        },
        () => {
          if (!alive) return
          setState({ taskId, hash: null })
          timer = window.setTimeout(load, BT_STATUS_POLL_MS)
        },
      )
    }
    load()
    return () => {
      alive = false
      window.clearTimeout(timer)
    }
  }, [taskId, hasMetadata])

  const hash = state.taskId === taskId ? state.hash : null
  if (hash === null) return null
  const { head, tail } = splitMiddle(hash, 12)
  return (
    <DetailRow label={t('btInfoHashV2')}>
      <div className="flex min-w-0 items-center gap-1">
        <span title={hash} className="flex min-w-0 flex-1 font-mono text-xs">
          <span className="min-w-0 truncate whitespace-pre">{head}</span>
          {tail === '' ? null : <span className="shrink-0 whitespace-pre">{tail}</span>}
        </span>
        <CopyButton text={hash} label={t('webCopy')} copiedLabel={t('webCopied')} />
      </div>
    </DetailRow>
  )
}
