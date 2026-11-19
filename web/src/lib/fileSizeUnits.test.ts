import { describe, expect, test } from 'bun:test'
import { FILE_SIZE_UNITS_PREF, byteUnitFor, getFileSizeUnits, subscribeFileSizeUnits } from './fileSizeUnits'

describe('file size unit suffixes', () => {
  test('legacy and IEC labels share binary unit indexes', () => {
    expect([1, 2, 3, 4, 5].map((index) => byteUnitFor(index, 'legacy'))).toEqual(['KB', 'MB', 'GB', 'TB', 'PB'])
    expect([1, 2, 3, 4, 5].map((index) => byteUnitFor(index, 'iec'))).toEqual(['KiB', 'MiB', 'GiB', 'TiB', 'PiB'])
  })

  test('bytes remain B and unsupported large suffixes clamp at the largest label', () => {
    expect(byteUnitFor(0, 'iec')).toBe('B')
    expect(byteUnitFor(10, 'legacy')).toBe('PB')
    expect(byteUnitFor(10, 'iec')).toBe('PiB')
  })
})

describe('cross-tab preference sync', () => {
  test('unsubscribing one view keeps storage updates for the remaining views', () => {
    const globals = globalThis as unknown as { window?: unknown; localStorage?: unknown }
    const saved = { window: globals.window, localStorage: globals.localStorage }
    const storage = { getItem: () => null, setItem: () => undefined }
    const target = new EventTarget()
    globals.window = target
    globals.localStorage = storage
    const storageEvent = (newValue: string) =>
      Object.assign(new Event('storage'), { storageArea: storage, key: FILE_SIZE_UNITS_PREF, newValue })
    try {
      let calls = 0
      const dropped = subscribeFileSizeUnits(() => undefined)
      const kept = subscribeFileSizeUnits(() => {
        calls += 1
      })
      dropped()
      target.dispatchEvent(storageEvent('iec'))
      expect(calls).toBe(1)
      expect(getFileSizeUnits()).toBe('iec')
      target.dispatchEvent(storageEvent('legacy'))
      kept()
      expect(getFileSizeUnits()).toBe('legacy')
    } finally {
      globals.window = saved.window
      globals.localStorage = saved.localStorage
    }
  })
})
