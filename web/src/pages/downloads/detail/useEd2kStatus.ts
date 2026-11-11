import { useCallback, useEffect, useRef, useState } from 'react'
import { rpc } from '../../../lib/rpc'
import type { Ed2kStatusDto } from '../../../lib/rpc'
import { errorMessage } from '../../../lib/rpc/error'
import { BT_STATUS_POLL_MS } from './btStatus'
import { normalizeEd2kStatus } from './ed2kStatus'

export interface Ed2kStatusState {
  /** 最近一次成功的结果（已归一缺省字段）；首次响应前为 null。切换任务即清空。 */
  data: Ed2kStatusDto | null
  /** 最近一次刷新失败的原因（数据保持上次成功值）；成功后清除。 */
  error: string | null
  /** 立即重拉（取消等待中的下一轮；在途请求结束后照常续排）。 */
  refresh: () => void
}

/**
 * eD2K 实况轮询（同 `useBtStatus`）：页签可见即请求，上一次结束 2 s 后再请求（不重叠）；
 * 卸载或 taskId 变化即停止并丢弃迟到响应；失败保留旧数据。页面在后台时跳过本轮。
 */
export function useEd2kStatus(taskId: string): Ed2kStatusState {
  const [state, setState] = useState<{ taskId: string; data: Ed2kStatusDto | null; error: string | null }>({ taskId, data: null, error: null })
  const kick = useRef<() => void>(() => undefined)

  useEffect(() => {
    let alive = true
    let timer: number | undefined
    let inflight = false
    let again = false
    const schedule = () => {
      if (alive) timer = window.setTimeout(poll, BT_STATUS_POLL_MS)
    }
    const poll = () => {
      if (document.hidden) {
        schedule()
        return
      }
      inflight = true
      rpc.daemon.task
        .ed2kStatus({ taskId })
        .then(
          (dto) => {
            if (alive) setState({ taskId, data: normalizeEd2kStatus(dto), error: null })
          },
          (error: unknown) => {
            if (!alive) return
            const message = errorMessage(error)
            setState((prev) => ({ taskId, data: prev.taskId === taskId ? prev.data : null, error: message }))
          },
        )
        .finally(() => {
          inflight = false
          if (again) {
            again = false
            poll()
          } else schedule()
        })
    }
    kick.current = () => {
      if (!alive) return
      if (inflight) {
        again = true
        return
      }
      clearTimeout(timer)
      poll()
    }
    poll()
    return () => {
      alive = false
      clearTimeout(timer)
    }
  }, [taskId])

  const refresh = useCallback(() => kick.current(), [])
  return state.taskId === taskId ? { data: state.data, error: state.error, refresh } : { data: null, error: null, refresh }
}
