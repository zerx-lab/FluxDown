import { expect, test } from 'bun:test'
import { HTML_RESPONSE_CHANGED_MESSAGE, POST_ONLY_ENDPOINT_MESSAGE, POST_ONLY_RESUME_MESSAGE, UNEXPECTED_HTML_PREFIX, localizeTaskError, taskErrorKey } from './taskError'

test('引擎稳定错误消息映射到 i18n key，其余原样', () => {
  expect(taskErrorKey(POST_ONLY_ENDPOINT_MESSAGE)).toBe('taskErrorPostOnlyEndpoint')
  expect(taskErrorKey(`  ${POST_ONLY_RESUME_MESSAGE}\n`)).toBe('taskErrorPostOnlyResume')
  expect(taskErrorKey('connection reset')).toBeNull()
  expect(taskErrorKey(`${POST_ONLY_ENDPOINT_MESSAGE} extra`)).toBeNull()
  expect(localizeTaskError((key) => `L:${key}`, POST_ONLY_ENDPOINT_MESSAGE)).toBe('L:taskErrorPostOnlyEndpoint')
  expect(localizeTaskError((key) => `L:${key}`, 'connection reset')).toBe('connection reset')
})

test('网页响应失败文案（前缀 / 整串）映射到 i18n key', () => {
  expect(taskErrorKey(`${UNEXPECTED_HTML_PREFIX}text/html) instead of the expected file — x`)).toBe('taskErrorUnexpectedHtml')
  expect(taskErrorKey(HTML_RESPONSE_CHANGED_MESSAGE)).toBe('taskErrorHtmlChanged')
  expect(taskErrorKey(`${HTML_RESPONSE_CHANGED_MESSAGE} extra`)).toBeNull()
})
