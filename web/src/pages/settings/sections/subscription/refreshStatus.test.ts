import { expect, test } from 'bun:test'
import { classifyRefresh } from './refreshStatus'

test('success with failed sources is partial and keeps the detail', () => {
  expect(classifyRefresh({ success: true, error: ' https://x/nodes.dat: not a server.met ' })).toEqual({
    kind: 'partial',
    detail: 'https://x/nodes.dat: not a server.met',
  })
})

test('success without error is ok; no success is failed regardless of error text', () => {
  expect(classifyRefresh({ success: true, error: '' })).toEqual({ kind: 'ok' })
  expect(classifyRefresh({ success: false, error: 'a: timeout' })).toEqual({ kind: 'failed' })
})
