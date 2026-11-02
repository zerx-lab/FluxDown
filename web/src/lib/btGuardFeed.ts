// 防吸血分页列表的状态机（`useGuardFeed` 的纯逻辑）：按筛选键隔离数据、首页载入 / 追加 / 失败互不串扰。

export interface FeedState<T> {
  /** 数据所属的筛选键；与当前键不同 = 旧数据，视同空。 */
  key: string
  items: T[]
  total: number
  error: string | null
  /** 该键的首页已成功载入过。 */
  loaded: boolean
  loadingMore: boolean
}

export type FeedAction<T> =
  | { type: 'refreshed'; key: string; items: T[]; total: number }
  | { type: 'failed'; key: string; error: string }
  | { type: 'moreStarted'; key: string }
  | { type: 'moreLoaded'; key: string; items: T[]; total: number }
  | { type: 'moreFailed'; key: string; error: string }
  /** 在途的追加被刷新 / 换键取代并丢弃：复位转圈。 */
  | { type: 'moreCancelled'; key: string }

export function emptyFeed<T>(key: string): FeedState<T> {
  return { key, items: [], total: 0, error: null, loaded: false, loadingMore: false }
}

/** 当前键下可见的状态：旧键数据（含错误）一律不可见。 */
export function feedFor<T>(state: FeedState<T>, key: string): FeedState<T> {
  return state.key === key ? state : emptyFeed(key)
}

export function feedReducer<T extends { id: number }>(state: FeedState<T>, action: FeedAction<T>): FeedState<T> {
  const base = feedFor(state, action.key)
  switch (action.type) {
    case 'refreshed':
      return { key: action.key, items: action.items, total: action.total, error: null, loaded: true, loadingMore: false }
    case 'failed':
      // 首次载入失败（该键从未成功）：不保留任何旧数据，只显示错误；已载入后的刷新失败保留已有内容。
      return { ...base, error: action.error, loadingMore: false }
    case 'moreStarted':
      return { ...base, loadingMore: true }
    case 'moreLoaded': {
      const seen = new Set(base.items.map((item) => item.id))
      return { ...base, items: [...base.items, ...action.items.filter((item) => !seen.has(item.id))], total: action.total, error: null, loadingMore: false }
    }
    case 'moreFailed':
      return { ...base, error: action.error, loadingMore: false }
    case 'moreCancelled':
      return { ...base, loadingMore: false }
  }
}

/** 首次载入中：当前键尚无成功首页且没有错误。 */
export function feedLoading<T>(state: FeedState<T>): boolean {
  return !state.loaded && state.error === null
}

export interface MoreTicket {
  refresh: number
  more: number
}

/**
 * 请求序号：首页刷新与「加载更多」各用一个，刷新同时作废在途追加（二者共用一个序号会让被丢弃的追加
 * 永远无法复位转圈）。`beginRefresh` 返回是否有在途追加被取代，调用方据此派发 `moreCancelled`。
 */
export class FeedSeq {
  private refresh = 0
  private more = 0
  private moreInflight = false

  beginRefresh(): { ticket: number; cancelledMore: boolean } {
    const cancelledMore = this.moreInflight
    this.moreInflight = false
    this.more += 1
    this.refresh += 1
    return { ticket: this.refresh, cancelledMore }
  }

  isCurrentRefresh(ticket: number): boolean {
    return ticket === this.refresh
  }

  beginMore(): MoreTicket {
    this.moreInflight = true
    this.more += 1
    return { refresh: this.refresh, more: this.more }
  }

  /** 追加响应是否仍有效；有效则同时结束在途状态。 */
  finishMore(ticket: MoreTicket): boolean {
    if (ticket.refresh !== this.refresh || ticket.more !== this.more) return false
    this.moreInflight = false
    return true
  }

  get loadingMore(): boolean {
    return this.moreInflight
  }
}
