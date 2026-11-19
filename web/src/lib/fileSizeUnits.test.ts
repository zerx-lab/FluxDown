import { describe, expect, test } from 'bun:test'
import { byteUnitFor } from './fileSizeUnits'

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
