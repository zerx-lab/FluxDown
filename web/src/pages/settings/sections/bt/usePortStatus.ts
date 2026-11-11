import { useEffect, useState } from 'react'
import { rpc } from '../../../../lib/rpc'
import type { BtPortStatusDto } from '../../../../lib/rpc'
import { PORT_STATUS_POLL_MS } from './portStatus'

/**
 * `daemon.bt.portStatus` 轮询：挂载（「连接」页签可见且 UPnP 开）即请求，之后在上一次请求结束 5 s
 * 后再请求；卸载即停。浏览器页面在后台时跳过本轮。失败（含旧 daemon 无此方法）保留上次快照，
 * 首轮之前 / 一直失败时为 null（行内留空，对齐 GPUI）。
 */
export function usePortStatus(): BtPortStatusDto | null {
  const [status, setStatus] = useState<BtPortStatusDto | null>(null)
  useEffect(() => {
    let alive = true
    let timer: number | undefined
    const schedule = () => {
      if (alive) timer = window.setTimeout(poll, PORT_STATUS_POLL_MS)
    }
    const poll = () => {
      if (document.hidden) {
        schedule()
        return
      }
      rpc.daemon.bt
        .portStatus()
        .then((data) => {
          if (alive) setStatus(data)
        })
        .catch(() => {
          // 保留上次快照；旧 daemon 无此方法时行内一直留空。
        })
        .finally(schedule)
    }
    poll()
    return () => {
      alive = false
      window.clearTimeout(timer)
    }
  }, [])
  return status
}
