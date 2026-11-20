import { afterEach, beforeEach, expect, test } from 'bun:test'
import { fetchRssImage } from './http'

const PNG = Uint8Array.of(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3)
const originalFetch = globalThis.fetch
const originalSessionStorage = Object.getOwnPropertyDescriptor(globalThis, 'sessionStorage')

beforeEach(() => {
  Object.defineProperty(globalThis, 'sessionStorage', { configurable: true, value: { getItem: () => 'web-secret' } })
})

afterEach(() => {
  globalThis.fetch = originalFetch
  if (originalSessionStorage) Object.defineProperty(globalThis, 'sessionStorage', originalSessionStorage)
  else Reflect.deleteProperty(globalThis, 'sessionStorage')
})

function pngResponse() {
  return new Response(PNG, { headers: { 'Content-Type': 'image/png' } })
}

async function rejectionMessage(pending: Promise<unknown>): Promise<string> {
  const [result] = await Promise.allSettled([pending])
  if (result?.status !== 'rejected' || !(result.reason instanceof Error)) {
    throw new Error('Expected the image request to reject with an Error')
  }
  return result.reason.message
}

test('RSS image uses same-origin bearer fetch and preserves the opaque item key', async () => {
  const sourceId = '源/& +?'
  const guid = 'https://例子.example/a?b=1+二&x=%23#🙂'
  globalThis.fetch = async (input, init) => {
    const url = new URL(String(input), 'https://fluxdown.example')
    expect(url.origin).toBe('https://fluxdown.example')
    expect(url.pathname).toBe('/api/web/rss-images')
    expect(url.searchParams.size).toBe(3)
    expect(url.searchParams.get('sourceId')).toBe(sourceId)
    expect(url.searchParams.get('guid')).toBe(guid)
    expect(url.searchParams.get('index')).toBe('0')
    expect(url.hash).toBe('')
    expect(new Headers(init?.headers).get('Authorization')).toBe('Bearer web-secret')
    expect(init?.cache).toBe('no-store')
    expect(init?.redirect).toBe('error')
    return pngResponse()
  }

  const blob = await fetchRssImage(sourceId, guid, 0, new AbortController().signal)
  expect(blob.type).toBe('image/png')
  expect([...new Uint8Array(await blob.arrayBuffer())]).toEqual([...PNG])
})

test('one unavailable RSS image does not prevent another image from loading', async () => {
  globalThis.fetch = async (input) => {
    const url = new URL(String(input), 'https://fluxdown.example')
    return url.searchParams.get('index') === '0' ? new Response(null, { status: 404 }) : pngResponse()
  }

  const controller = new AbortController()
  const results = await Promise.allSettled([
    fetchRssImage('source', 'item', 0, controller.signal),
    fetchRssImage('source', 'item', 1, controller.signal),
  ])
  expect(results.map((result) => result.status)).toEqual(['rejected', 'fulfilled'])
  expect(controller.signal.aborted).toBe(false)
})

test('RSS rejects content with a PNG media type but a non-PNG signature', async () => {
  globalThis.fetch = async () => new Response('<html>not an image</html>', { headers: { 'Content-Type': 'image/png' } })

  expect(await rejectionMessage(fetchRssImage('source', 'item', 0, new AbortController().signal))).toBe('RSS image is not PNG')
})

test('RSS rejects an oversized PNG even with no Content-Length', async () => {
  const oversized = new Uint8Array(512 * 1024 + 1)
  oversized.set(PNG)
  let cancelled = false
  globalThis.fetch = async () => new Response(new ReadableStream<Uint8Array>({
    start(controller) { controller.enqueue(oversized) },
    cancel() { cancelled = true },
  }), { headers: { 'Content-Type': 'image/png' } })

  expect(await rejectionMessage(fetchRssImage('source', 'item', 0, new AbortController().signal))).toBe('RSS image exceeds size limit')
  expect(cancelled).toBe(true)
})

test('RSS image abort forwards the caller reason to an in-flight request', async () => {
  globalThis.fetch = async (_input, init) => {
    const signal = init?.signal
    if (!signal) throw new Error('missing request signal')
    const body = new ReadableStream({
      start(controller) {
        signal.addEventListener('abort', () => controller.error(signal.reason), { once: true })
      },
    })
    return new Response(body, { headers: { 'Content-Type': 'image/png' } })
  }

  const controller = new AbortController()
  const pending = fetchRssImage('source', 'item', 0, controller.signal)
  controller.abort(new Error('closed'))
  expect(await rejectionMessage(pending)).toBe('closed')
})
