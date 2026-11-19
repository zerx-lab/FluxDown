import { useEffect, useState } from 'react'
import { rpc, RpcError, useAgent, useConnection } from '../../../../lib/rpc'
import type { BtPortStatusDto } from '../../../../lib/rpc'
import { METHOD_NOT_FOUND_CODE } from '../../../../lib/rpc/protocol/error'
import { PORT_STATUS_POLL_MS } from './portStatus'

const selectDaemonConnected = (snapshot: { daemonConnected: boolean }) => snapshot.daemonConnected

/**
 * `daemon.bt.portStatus` 轮询：挂载（「连接」页签可见）即请求，之后在上一次请求结束 5 s
 * 后再请求；卸载或断开连接时停止读取。瞬态错误保留最后一次成功的快照。
 */
export function usePortStatus(): BtPortStatusDto | null {
  const ready = useConnection().phase === 'ready'
  const daemonConnected = useAgent(selectDaemonConnected, false)
  const connected = ready && daemonConnected
  const [status, setStatus] = useState<BtPortStatusDto | null>(null)
  useEffect(() => {
    if (!connected) {
      setStatus(null)
      return
    }
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
        .catch((error: unknown) => {
          if (alive && error instanceof RpcError && (error.code === METHOD_NOT_FOUND_CODE || error.is('unsupported'))) {
            setStatus(null)
          }
        })
        .finally(schedule)
    }
    poll()
    return () => {
      alive = false
      window.clearTimeout(timer)
    }
  }, [connected])
  return connected ? status : null
}
