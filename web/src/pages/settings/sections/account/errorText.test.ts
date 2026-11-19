import { describe, expect, test } from 'bun:test'
import { RpcError } from '../../../../lib/rpc'
import type { ApplicationErrorCode, ErrorReason } from '../../../../lib/rpc'
import { accountErrorKey, REASON_KEYS } from './errorText'

// tsconfig 未引入 Bun 类型，仅声明测试用到的最小接口。
declare const Bun: { file(path: URL): { text(): Promise<string> } }

const err = (code: ApplicationErrorCode, reason?: ErrorReason, retryable = false) =>
  new RpcError({ code: -32000, message: 'x', data: { code, retryable, ...(reason ? { reason } : {}) } })

describe('错误 → 文案键', () => {
  test('reason 优先于 code', () => {
    expect(accountErrorKey(err('unauthorized', 'invalidCredentials'), 'login')).toBe('accountErrorInvalidCredentials')
    expect(accountErrorKey(err('conflict', 'deviceLimit'))).toBe('accountErrorDeviceLimit')
    expect(accountErrorKey(err('invalidArgument', 'wrongPassword'))).toBe('accountErrorWrongPassword')
    expect(accountErrorKey(err('unavailable', 'cloudUnreachable', true))).toBe('accountErrorNetwork')
    // 同一 code 因 reason 不同得到不同文案
    expect(accountErrorKey(err('conflict', 'emailTaken'))).not.toBe(accountErrorKey(err('conflict', 'taskStateConflict')))
  })

  test('remoteTaskGone 映射到 errReasonRemoteTaskGone（notFound 码不再回退通用文案）', () => {
    expect(accountErrorKey(err('notFound', 'remoteTaskGone'))).toBe('errReasonRemoteTaskGone')
  })

  test('无 reason 或 reason 未映射时按 code 回退，且不再猜测语义', () => {
    expect(accountErrorKey(err('unauthorized'), 'login')).toBe('localServiceActionFailed')
    expect(accountErrorKey(err('unsupported'), 'register')).toBe('settingsUnsupportedOnPlatform')
    expect(accountErrorKey(err('conflict', 'unknown'))).toBe('localServiceConflict')
    expect(accountErrorKey(err('notFound'))).toBe('localServiceInvalidArgument')
    expect(accountErrorKey(err('timeout'))).toBe('localServiceDisconnected')
  })

  test('无 reason 时参数错误按场景细化；非 RpcError 为未知错误', () => {
    expect(accountErrorKey(err('invalidArgument'), 'code')).toBe('accountErrorInvalidCode')
    expect(accountErrorKey(err('invalidArgument'), 'login')).toBe('accountErrorValidation')
    expect(accountErrorKey(new Error('boom'))).toBe('accountErrorUnknown')
  })

  test('所有已映射 reason 的文案键都存在于 en/zh 目录', async () => {
    for (const lang of ['en', 'zh']) {
      const text = await Bun.file(new URL(`../../../../../../assets/i18n/${lang}.json`, import.meta.url)).text()
      const catalog = JSON.parse(text) as Record<string, string>
      for (const key of Object.values(REASON_KEYS)) expect(key && key in catalog).toBe(true)
    }
  })
})
