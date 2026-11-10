// 防吸血数据钩子：状态轮询 + `btGuardChanged` 事件刷新；条目 / 事件分页追加。

import { useCallback, useEffect, useReducer, useRef, useState } from 'react'
import { rpc, useAgent, useServiceEvents } from '../../../../../lib/rpc'
import type { BtGuardStateDto, EventFrame } from '../../../../../lib/rpc'
import { errorMessage } from '../../../../../lib/rpc/error'
import { GUARD_PAGE_SIZE, isGuardChangedFrame } from '../../../../../lib/btGuard'
import { emptyFeed, FeedSeq, feedFor, feedLoading, feedReducer } from '../../../../../lib/btGuardFeed'
import type { FeedState } from '../../../../../lib/btGuardFeed'

/** 状态（含今日统计）没有变化事件，可见时按此间隔刷新。 */
const STATE_POLL_MS = 5000
/** 事件风暴合并窗口。 */
const CHANGE_DEBOUNCE_MS = 400
/** 单页上限（协议 `BT_GUARD_LIST_MAX_LIMIT`）。 */
const MAX_LIMIT = 500

/** 已登录（有云账号会话）。 */
export function useLoggedIn(): boolean {
  return useAgent((snapshot) => snapshot.session !== null && snapshot.session !== undefined, false)
}

/** 防吸血条目 / 状态变化事件（去抖后回调）；daemon 直达与经 agent 转发两种帧都认。 */
export function useGuardChanged(onChange: () => void): void {
  const latest = useRef(onChange)
  latest.current = onChange
  const timer = useRef<number | undefined>(undefined)
  useEffect(() => () => window.clearTimeout(timer.current), [])
  useServiceEvents(
    useCallback((frame: EventFrame) => {
      if (!isGuardChangedFrame(frame)) return
      window.clearTimeout(timer.current)
      timer.current = window.setTimeout(() => latest.current(), CHANGE_DEBOUNCE_MS)
    }, []),
  )
}

/** 本地时钟：按 `intervalMs` 刷新（`null` = 不需要）；页面不可见时不刷新，重新可见时立即对时。 */
export function useNow(intervalMs: number | null): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (intervalMs === null) return
    const tick = () => {
      if (!document.hidden) setNow(Date.now())
    }
    const timer = window.setInterval(tick, intervalMs)
    document.addEventListener('visibilitychange', tick)
    return () => {
      window.clearInterval(timer)
      document.removeEventListener('visibilitychange', tick)
    }
  }, [intervalMs])
  return now
}

export interface GuardStateResult {
  data: BtGuardStateDto | null
  error: string | null
  reload: () => void
  /** 直接写入新状态（如过滤订阅刷新结果）。 */
  patch: (update: (prev: BtGuardStateDto) => BtGuardStateDto) => void
}

export function useGuardState(): GuardStateResult {
  const [data, setData] = useState<BtGuardStateDto | null>(null)
  const [error, setError] = useState<string | null>(null)
  const seq = useRef(0)

  const load = useCallback(() => {
    const mine = ++seq.current
    rpc.daemon.bt.guard.state().then(
      (next) => {
        if (mine !== seq.current) return
        setData(next)
        setError(null)
      },
      (failure: unknown) => {
        if (mine === seq.current) setError(errorMessage(failure))
      },
    )
  }, [])

  useEffect(() => {
    load()
    const timer = window.setInterval(() => {
      if (!document.hidden) load()
    }, STATE_POLL_MS)
    return () => {
      window.clearInterval(timer)
      seq.current += 1
    }
  }, [load])
  useGuardChanged(load)

  const patch = useCallback((update: (prev: BtGuardStateDto) => BtGuardStateDto) => setData((prev) => (prev ? update(prev) : prev)), [])
  return { data, error, reload: load, patch }
}

export interface GuardFeed<T> {
  items: T[]
  total: number
  /** 首次载入中。 */
  loading: boolean
  loadingMore: boolean
  error: string | null
  hasMore: boolean
  loadMore: () => void
  reload: () => void
}

/**
 * 分页追加列表。`fetchPage` 随 `key` 变化（筛选条件）重新载入首页；变化事件 / `reload` 以当前已载入条数
 * （封顶 500）重取首页，保证不丢已展开的内容。首页刷新与「加载更多」各用独立序号（`FeedSeq`），迟到的响应按序号
 * 丢弃，被刷新取代的追加会复位转圈；换筛选后首次失败不保留旧数据（`feedReducer`）。
 */
export function useGuardFeed<T extends { id: number }>(
  fetchPage: (offset: number, limit: number) => Promise<{ total: number; items: T[] }>,
  key: string,
): GuardFeed<T> {
  const [state, dispatch] = useReducer(feedReducer<T>, key, emptyFeed<T>)
  const visible: FeedState<T> = feedFor(state, key)
  const visibleRef = useRef(visible)
  visibleRef.current = visible
  const fetchRef = useRef(fetchPage)
  fetchRef.current = fetchPage
  const seq = useRef(new FeedSeq())

  const refresh = useCallback((forKey: string, limit: number) => {
    const { ticket, cancelledMore } = seq.current.beginRefresh()
    if (cancelledMore) dispatch({ type: 'moreCancelled', key: forKey })
    fetchRef.current(0, limit).then(
      (page) => {
        if (seq.current.isCurrentRefresh(ticket)) dispatch({ type: 'refreshed', key: forKey, items: page.items, total: page.total })
      },
      (failure: unknown) => {
        if (seq.current.isCurrentRefresh(ticket)) dispatch({ type: 'failed', key: forKey, error: errorMessage(failure) })
      },
    )
  }, [])

  useEffect(() => {
    const current = seq.current
    refresh(key, GUARD_PAGE_SIZE)
    return () => {
      current.beginRefresh()
    }
  }, [key, refresh])

  const reload = useCallback(() => refresh(key, Math.min(MAX_LIMIT, Math.max(GUARD_PAGE_SIZE, visibleRef.current.items.length))), [key, refresh])
  useGuardChanged(reload)

  const loadMore = useCallback(() => {
    if (seq.current.loadingMore) return
    const ticket = seq.current.beginMore()
    dispatch({ type: 'moreStarted', key })
    fetchRef.current(visibleRef.current.items.length, GUARD_PAGE_SIZE).then(
      (page) => {
        if (seq.current.finishMore(ticket)) dispatch({ type: 'moreLoaded', key, items: page.items, total: page.total })
      },
      (failure: unknown) => {
        if (seq.current.finishMore(ticket)) dispatch({ type: 'moreFailed', key, error: errorMessage(failure) })
      },
    )
  }, [key])

  return {
    items: visible.items,
    total: visible.total,
    loading: feedLoading(visible),
    loadingMore: visible.loadingMore,
    error: visible.error,
    hasMore: visible.items.length < visible.total,
    loadMore,
    reload,
  }
}
