import { afterEach, describe, expect, test } from 'bun:test'
import { fetchPluginImage } from '../../../../lib/rpc/http'
import { acquireImage } from './content'

const PNG = Uint8Array.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3])
const ID = '123e4567-e89b-42d3-a456-426614174000'
const originalFetch = globalThis.fetch
const originalCreateObjectURL = URL.createObjectURL
const originalRevokeObjectURL = URL.revokeObjectURL
const originalSessionStorage = Object.getOwnPropertyDescriptor(globalThis, 'sessionStorage')

afterEach(() => {
  globalThis.fetch = originalFetch
  URL.createObjectURL = originalCreateObjectURL
  URL.revokeObjectURL = originalRevokeObjectURL
  if (originalSessionStorage) Object.defineProperty(globalThis, 'sessionStorage', originalSessionStorage)
  else Reflect.deleteProperty(globalThis, 'sessionStorage')
})

describe('plugin image resources', () => {
  test('fetches the active-session route with bearer auth and bounded PNG bytes', async () => {
    Object.defineProperty(globalThis, 'sessionStorage', { configurable: true, value: { getItem: () => 'web-secret' } })
    let requestedUrl = ''
    let request: RequestInit | undefined
    globalThis.fetch = async (input, init) => {
      requestedUrl = String(input)
      request = init
      return new Response(PNG, { headers: { 'Content-Type': 'image/png', 'Content-Length': String(PNG.byteLength) } })
    }

    const blob = await fetchPluginImage('active-session', ID, new AbortController().signal)
    expect(requestedUrl).toBe(`/api/web/plugin-images/active-session/${ID}`)
    expect(requestedUrl.includes('token=')).toBe(false)
    expect(new Headers(request?.headers).get('Authorization')).toBe('Bearer web-secret')
    expect(new Headers(request?.headers).get('Cache-Control')).toBe('no-store')
    expect(request?.cache).toBe('no-store')
    expect(request?.redirect).toBe('error')
    expect([...new Uint8Array(await blob.arrayBuffer())]).toEqual([...PNG])
  })

  test('deduplicates same-session resources and revokes after the last consumer releases', async () => {
    Object.defineProperty(globalThis, 'sessionStorage', { configurable: true, value: { getItem: () => 'web-secret' } })
    let fetches = 0
    const revoked: string[] = []
    globalThis.fetch = async () => {
      fetches += 1
      return new Response(PNG, { headers: { 'Content-Type': 'image/png' } })
    }

    URL.createObjectURL = () => 'blob:plugin-image'
    URL.revokeObjectURL = (url) => revoked.push(url)

    const first = acquireImage('active-session', ID)
    const second = acquireImage('active-session', ID)
    expect(await first.promise).toBe('blob:plugin-image')
    expect(await second.promise).toBe('blob:plugin-image')
    expect(fetches).toBe(1)
    first.release()
    expect(revoked).toEqual([])
    second.release()
    await Promise.resolve()
    expect(revoked).toEqual(['blob:plugin-image'])
  })
  test('cancels an oversized streamed response before buffering it', async () => {
    Object.defineProperty(globalThis, 'sessionStorage', { configurable: true, value: { getItem: () => 'web-secret' } })
    let cancelled = false
    const oversized = new Uint8Array(4 * 1024 * 1024 + 1)
    oversized.set(PNG.subarray(0, 8))
    globalThis.fetch = async () => new Response(
      new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(oversized)
        },
        cancel() {
          cancelled = true
        },
      }),
      { headers: { 'Content-Type': 'image/png' } },
    )

    const error: unknown = await fetchPluginImage('active-session', ID, new AbortController().signal)
      .then(() => null, (error: unknown) => error)
    expect(error instanceof Error ? error.message : null).toBe('plugin image exceeds size limit')
    expect(cancelled).toBe(true)
  })

  test('releases old-session object URLs even when a stale response completes later', async () => {
    Object.defineProperty(globalThis, 'sessionStorage', { configurable: true, value: { getItem: () => 'web-secret' } })
    const pending = new Map<string, (response: Response) => void>()
    const revoked: string[] = []
    let nextUrl = 0
    globalThis.fetch = (input) => {
      return new Promise<Response>((resolve) => pending.set(String(input), resolve))
    }
    URL.revokeObjectURL = (url) => revoked.push(url)
    URL.createObjectURL = () => `blob:plugin-image-${++nextUrl}`

    const oldResource = acquireImage('old-session', ID)
    oldResource.release()
    const currentResource = acquireImage('current-session', ID)
    const png = () => new Response(PNG, { headers: { 'Content-Type': 'image/png' } })
    pending.get(`/api/web/plugin-images/old-session/${ID}`)?.(png())
    pending.get(`/api/web/plugin-images/current-session/${ID}`)?.(png())
    expect(await oldResource.promise).toBe('blob:plugin-image-1')
    expect(await currentResource.promise).toBe('blob:plugin-image-2')
    await Promise.resolve()
    expect(revoked.includes('blob:plugin-image-1')).toBe(true)
    currentResource.release()
    await Promise.resolve()
    expect(revoked.includes('blob:plugin-image-2')).toBe(true)
  })
})
