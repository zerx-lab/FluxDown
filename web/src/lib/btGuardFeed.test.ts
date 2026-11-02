import { describe, expect, test } from 'bun:test'
import { emptyFeed, FeedSeq, feedFor, feedLoading, feedReducer } from './btGuardFeed'
import type { FeedState } from './btGuardFeed'

type Item = { id: number }
const items = (...ids: number[]): Item[] => ids.map((id) => ({ id }))
const loaded = (key: string, ids: number[], total = ids.length): FeedState<Item> => feedReducer(emptyFeed<Item>(key), { type: 'refreshed', key, items: items(...ids), total })

describe('feedReducer', () => {
  test('refresh replaces data and clears errors / spinner', () => {
    const busy = feedReducer(feedReducer(loaded('a', [1, 2], 5), { type: 'moreStarted', key: 'a' }), { type: 'moreFailed', key: 'a', error: 'x' })
    const next = feedReducer(busy, { type: 'refreshed', key: 'a', items: items(3), total: 1 })
    expect(next).toMatchObject({ items: [{ id: 3 }], total: 1, error: null, loaded: true, loadingMore: false })
  })

  test('append dedupes by id and updates the total', () => {
    const next = feedReducer(feedReducer(loaded('a', [1, 2], 4), { type: 'moreStarted', key: 'a' }), { type: 'moreLoaded', key: 'a', items: items(2, 3), total: 3 })
    expect(next.items.map((i) => i.id)).toEqual([1, 2, 3])
    expect(next.total).toBe(3)
    expect(next.loadingMore).toBe(false)
  })

  test('first failure under a new filter drops the old data and shows the error', () => {
    const old = loaded('a', [1, 2])
    expect(feedFor(old, 'b').items).toEqual([])
    const failed = feedReducer(old, { type: 'failed', key: 'b', error: 'boom' })
    expect(failed).toMatchObject({ key: 'b', items: [], total: 0, error: 'boom', loaded: false })
    expect(feedLoading(failed)).toBe(false)
    expect(feedLoading(emptyFeed('b'))).toBe(true)
  })

  test('a refresh failure after a successful load keeps the content', () => {
    const failed = feedReducer(loaded('a', [1]), { type: 'failed', key: 'a', error: 'boom' })
    expect(failed.items).toEqual(items(1))
    expect(failed.error).toBe('boom')
  })

  test('cancelling a discarded append resets the spinner', () => {
    const started = feedReducer(loaded('a', [1], 9), { type: 'moreStarted', key: 'a' })
    expect(started.loadingMore).toBe(true)
    expect(feedReducer(started, { type: 'moreCancelled', key: 'a' }).loadingMore).toBe(false)
  })
})

describe('FeedSeq', () => {
  test('a refresh supersedes an in-flight append and reports it', () => {
    const seq = new FeedSeq()
    const first = seq.beginRefresh()
    expect(first.cancelledMore).toBe(false)
    const more = seq.beginMore()
    expect(seq.loadingMore).toBe(true)
    const second = seq.beginRefresh()
    expect(second.cancelledMore).toBe(true)
    expect(seq.loadingMore).toBe(false)
    expect(seq.isCurrentRefresh(first.ticket)).toBe(false)
    expect(seq.isCurrentRefresh(second.ticket)).toBe(true)
    expect(seq.finishMore(more)).toBe(false)
  })

  test('a live append finishes exactly once; a newer append invalidates the older one', () => {
    const seq = new FeedSeq()
    seq.beginRefresh()
    const a = seq.beginMore()
    expect(seq.finishMore(a)).toBe(true)
    expect(seq.loadingMore).toBe(false)
    const b = seq.beginMore()
    const c = seq.beginMore()
    expect(seq.finishMore(b)).toBe(false)
    expect(seq.finishMore(c)).toBe(true)
  })
})
