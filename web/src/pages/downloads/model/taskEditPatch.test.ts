import { describe, expect, test } from 'bun:test'
import { SECRET_HEADER_PLACEHOLDER, type TaskEditSnapshot } from '../../../lib/rpc'
import { hasTaskEditChanges, taskEditPatch, taskEditValues } from './taskEditPatch'

const snapshot: TaskEditSnapshot = {
  taskId: 't', url: 'https://example.test/file', revision: 'r1', saveDir: '/d', fileName: 'f', proxyUrl: '', segments: 8,
  properties: { downloadLimitBps: 1500, userAgent: '', authRef: '', category: '', tags: [], scheduledAt: 0 },
  headers: { Authorization: SECRET_HEADER_PLACEHOLDER, 'X-Test': 'yes' }, cookiesConfigured: true,
  capabilities: { request: true, proxy: true, storage: true, rename: true, segments: true, schedule: true },
}
describe('unified task editor patch', () => {
  test('unchanged values produce no network patch', () => {
    const patch = taskEditPatch(snapshot, taskEditValues(snapshot))
    expect(hasTaskEditChanges(patch)).toBe(false)
  })
  test('secret header placeholder is preserved and cookies are write-only', () => {
    const values = taskEditValues(snapshot)
    values.userAgent = 'agent'
    const patch = taskEditPatch(snapshot, values)
    expect(patch.userAgent).toBe('agent')
    expect(patch.headers).toBeUndefined()
    expect(patch.cookies).toBeUndefined()
    values.cookiesEdited = true
    values.cookies = ''
    expect(taskEditPatch(snapshot, values).cookies).toBe('')
  })
  test('changed header emits full map with secret placeholder for backend preservation', () => {
    const values = taskEditValues(snapshot)
    values.headers['X-Test'] = 'new'
    expect(taskEditPatch(snapshot, values).headers).toEqual(values.headers)
  })
  test('removing a redacted secret value is an intentional header edit', () => {
    const values = taskEditValues(snapshot)
    values.headers.Authorization = ''
    expect(taskEditPatch(snapshot, values).headers?.Authorization).toBe('')
  })
})
