// AI 助手协议镜像：方法名、版本号与 Rust 源码逐字一致；新增 ErrorReason 与 `error.rs` 同名；JSON 形状往返。
import { describe, expect, test } from 'bun:test'
import { METHOD } from './method'
import { ASSISTANT_MAX_TITLE_CHARS, ASSISTANT_PREVIEW_MAX_ITEMS, EMPTY_ASSISTANT_STATE } from './assistant'
import type { AssistantAskParams, AssistantExportResult, AssistantRunDto, AssistantSessionDto, AssistantToolCallDto } from './assistant'
import type { ErrorReason } from './error'
import { PROTOCOL_VERSION } from './rpc'

// tsconfig 未引入 Bun 类型，仅声明测试用到的最小接口。
declare const Bun: { file(path: URL): { text(): Promise<string> } }

const read = (relative: string) => Bun.file(new URL(`../../../../../native/protocol/src/${relative}`, import.meta.url)).text()

const ASSISTANT_METHODS = [
  'AGENT_ASSISTANT_GET',
  'AGENT_ASSISTANT_REFRESH',
  'AGENT_ASSISTANT_SESSIONS',
  'AGENT_ASSISTANT_SESSION',
  'AGENT_ASSISTANT_DELETE_SESSION',
  'AGENT_ASSISTANT_CLEAR_SESSIONS',
  'AGENT_ASSISTANT_ASK',
  'AGENT_ASSISTANT_CANCEL',
  'AGENT_ASSISTANT_CONFIRM',
  'AGENT_ASSISTANT_RATE',
  'AGENT_ASSISTANT_CONFIRM_ISSUE',
  'AGENT_ASSISTANT_DISCARD_ISSUE',
  'AGENT_ASSISTANT_RENAME_SESSION',
  'AGENT_ASSISTANT_EXPORT_SESSION',
  'AGENT_ASSISTANT_ATTACHMENT',
] as const

describe('AI 助手协议镜像与 Rust 对齐', () => {
  test('方法常量与 method.rs 逐字一致，且都在 ALL_METHODS 里', async () => {
    const source = await read('method.rs')
    const all = /pub const ALL_METHODS: &\[&str\] = &\[([\s\S]*?)\];/.exec(source)?.[1] ?? ''
    for (const name of ASSISTANT_METHODS) {
      const wire = new RegExp(`pub const ${name}: &str = "([^"]+)"`).exec(source)?.[1]
      expect(wire === undefined).toBe(false)
      expect(METHOD[name]).toBe(wire as string)
      expect(all.includes(name)).toBe(true)
    }
  })

  test('协议版本与 rpc.rs 一致', async () => {
    const source = await read('rpc.rs')
    const version = /pub const PROTOCOL_VERSION: u32 = (\d+);/.exec(source)?.[1]
    expect(Number(version)).toBe(PROTOCOL_VERSION)
  })

  test('新增 ErrorReason 与 error.rs 同名', async () => {
    const source = await read('error.rs')
    expect(source.includes('AssistantQuotaExceeded')).toBe(true)
    expect(source.includes('AssistantConfirmExpired')).toBe(true)
    expect(source.includes('AssistantSessionLimit')).toBe(true)
    const reasons: ErrorReason[] = ['assistantQuotaExceeded', 'assistantConfirmExpired', 'assistantSessionLimit']
    expect(reasons).toHaveLength(3)
  })

  test('常量与 assistant.rs 一致', async () => {
    const source = await read('assistant.rs')
    expect(Number(/pub const ASSISTANT_PREVIEW_MAX_ITEMS: usize = (\d+);/.exec(source)?.[1])).toBe(ASSISTANT_PREVIEW_MAX_ITEMS)
    expect(Number(/pub const ASSISTANT_MAX_TITLE_CHARS: usize = (\d+);/.exec(source)?.[1])).toBe(ASSISTANT_MAX_TITLE_CHARS)
  })

  test('空状态带 nowUnixMs = 0（旧 agent 缺省按无偏差）', () => {
    expect(EMPTY_ASSISTANT_STATE.nowUnixMs).toBe(0)
  })

  test('v31 字段的 JSON 形状往返不变', () => {
    const call: AssistantToolCallDto = {
      callId: 'c1',
      name: 'fd_tasks',
      kind: 'client',
      status: 'running',
      confirmation: 'pending',
      preview: {
        action: 'updateSettings',
        tasks: ['a.iso'],
        moreTasks: 3,
        deleteFiles: true,
        settings: [{ key: 'max_concurrent', from: '3', to: '5' }],
        urls: ['https://example.com/a'],
        details: ['Queue: default'],
      },
      destructive: true,
      interrupted: false,
    }
    const run: AssistantRunDto = {
      sessionId: 's1',
      status: 'failed',
      question: 'q',
      imageCount: 2,
      startedAtUnixMs: 1,
      items: [{ type: 'tool', call }],
      errorReason: 'streamLost',
    }
    const ask: AssistantAskParams = { sessionId: 's1', text: 'again', replaceFrom: 'm1', keepImages: true }
    const exported: AssistantExportResult = { fileName: 'chat.md', mime: 'text/markdown', content: '# x' }
    for (const value of [call, run, ask, exported]) expect(JSON.parse(JSON.stringify(value))).toEqual(value)
  })

  test('旁观执行与沿用旧图的字段在 Rust 里同名，JSON 形状往返不变', async () => {
    const source = await read('assistant.rs')
    for (const field of ['image_ids', 'observing', 'active_run_id']) expect(source.includes(`pub ${field}:`)).toBe(true)
    const run: AssistantRunDto = {
      sessionId: 's1',
      status: 'running',
      question: 'q',
      imageCount: 1,
      imageIds: ['att-1', 'att-2'],
      observing: true,
      startedAtUnixMs: 1,
      items: [],
    }
    const session: AssistantSessionDto = {
      id: 's1',
      channel: 'web',
      kind: 'ask',
      title: 't',
      messageCount: 2,
      active: true,
      activeRunId: 'r1',
      createdAt: '',
      updatedAt: '',
    }
    for (const value of [run, session]) expect(JSON.parse(JSON.stringify(value))).toEqual(value)
  })
})
