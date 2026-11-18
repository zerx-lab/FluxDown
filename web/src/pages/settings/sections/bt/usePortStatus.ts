import { useEffect, useState } from 'react'
import { rpc } from '../../../../lib/rpc'
import type { BtPortStatusDto } from '../../../../lib/rpc'
import { PORT_STATUS_POLL_MS } from './portStatus'

/**
 * `daemon.bt.portStatus` 轮询：挂载（「连接」页签可见）即请求，之后在上一次请求结束 5 s
 * 后再请求；卸载即停。浏览器页面在后台时跳过本轮。首轮在途或请求失败时没有可用快照。
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
          if (alive) setStatus(null)
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
