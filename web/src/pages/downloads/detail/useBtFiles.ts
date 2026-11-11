import { useCallback, useEffect, useRef, useState } from 'react'
import { rpc } from '../../../lib/rpc'
import type { BtFilesDto } from '../../../lib/rpc'
import { errorMessage } from '../../../lib/rpc/error'
import { BT_FILES_POLL_MS } from './btFiles'

export interface BtFilesState {
  /** 最近一次成功的结果（含本地乐观改动）；首次响应前为 null。切换任务即清空。 */
  data: BtFilesDto | null
  /** 最近一次刷新失败的原因（数据保持上次成功值）；成功后清除。 */
  error: string | null
}

const EMPTY: BtFilesState = { data: null, error: null }

/**
 * BT 文件表轮询（`daemon.task.btFiles`），与 `useBtStatus` 同一节奏：挂载（「文件」页签可见）即请求，
 * 上一次结束 2 s 后再请求；卸载或 taskId 变化即停止并丢弃迟到的响应；页面在后台时跳过本轮。
 *
 * `update` 做乐观更新：改动之前发出的请求，其响应一律作废（避免把旧优先级 / 旧路径刷回来）。
 */
export function useBtFiles(taskId: string): BtFilesState & { update: (apply: (data: BtFilesDto) => BtFilesDto) => void } {
  const [state, setState] = useState<{ taskId: string; value: BtFilesState }>({ taskId, value: EMPTY })
  /** 本地改动代次：响应携带发请求时的代次，不一致即丢弃。 */
  const epoch = useRef(0)

  useEffect(() => {
    let alive = true
    let timer: number | undefined
    const schedule = () => {
      if (alive) timer = window.setTimeout(poll, BT_FILES_POLL_MS)
    }
    const poll = () => {
      if (document.hidden) {
        schedule()
        return
      }
      const sent = epoch.current
      rpc.daemon.task
        .btFiles({ taskId })
        .then(
          (data) => {
            if (alive && sent === epoch.current) setState({ taskId, value: { data, error: null } })
          },
          (error: unknown) => {
            if (!alive) return
            const message = errorMessage(error)
            setState((prev) => ({ taskId, value: { data: prev.taskId === taskId ? prev.value.data : null, error: message } }))
          },
        )
        .finally(schedule)
    }
    poll()
    return () => {
      alive = false
      clearTimeout(timer)
    }
  }, [taskId])

  const update = useCallback(
    (apply: (data: BtFilesDto) => BtFilesDto) => {
      epoch.current += 1
      setState((prev) => {
        if (prev.taskId !== taskId || prev.value.data === null) return prev
        return { taskId, value: { ...prev.value, data: apply(prev.value.data) } }
      })
    },
    [taskId],
  )

  const value = state.taskId === taskId ? state.value : EMPTY
  return { ...value, update }
}
