import { describe, expect, test } from 'bun:test'
import { hasSelectionDeadline } from './selection'

describe('选择请求截止', () => {
  test('wire 上的 i64::MAX 解析失真后仍判为无截止', () => {
    const parsed = JSON.parse('{"deadlineUnixMs":9223372036854775807}') as { deadlineUnixMs: number }
    expect(hasSelectionDeadline(parsed)).toBe(false)
  })

  test('真实时刻（含已过期）有截止', () => {
    expect(hasSelectionDeadline({ deadlineUnixMs: 1_762_500_000_000 })).toBe(true)
    expect(hasSelectionDeadline({ deadlineUnixMs: 0 })).toBe(true)
  })
})
