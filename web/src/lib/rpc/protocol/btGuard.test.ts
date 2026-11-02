import { describe, expect, test } from 'bun:test'
import { DAEMON_CONFIG_FIELDS, daemonConfigField } from './config'
import { METHOD } from './method'

// tsconfig 未引入 Bun 类型，仅声明测试用到的最小接口。
declare const Bun: { file(path: URL): { text(): Promise<string> } }

const read = (relative: string) => Bun.file(new URL(`../../../../../native/protocol/src/${relative}`, import.meta.url)).text()

describe('防吸血镜像与 Rust 对齐', () => {
  test('bt_guard_* 配置键的类型 / 默认值 / 范围与 daemon_config.rs 一致', async () => {
    const source = await read('daemon_config.rs')
    const rust = [...source.matchAll(/field\(\s*"(bt_guard_\w+)",\s*([\s\S]*?),\s*"([^"]*)",?\s*\)/g)]
    expect(rust.length > 0).toBe(true)
    for (const [, key, kind, dflt] of rust) {
      const field = daemonConfigField(key as string)
      expect(field === undefined).toBe(false)
      expect(field?.default).toBe(dflt as string)
      const text = kind as string
      if (text.includes('Bool')) expect(field?.kind).toBe('bool')
      else if (text.includes('ReadOnly')) expect(field?.kind).toBe('readOnly')
      else if (text.includes('Text')) expect(field?.kind).toBe('text')
      else if (text.includes('Enum')) expect(field?.kind).toBe('enum')
      else {
        expect(field?.kind).toBe('integer')
        const min = /min:\s*([\d_]+)/.exec(text)?.[1]
        const max = /max:\s*([\d_]+)/.exec(text)?.[1]
        expect(field?.min).toBe(Number((min ?? '').replaceAll('_', '')))
        expect(field?.max).toBe(Number((max ?? '').replaceAll('_', '')))
      }
    }
    expect(DAEMON_CONFIG_FIELDS.filter((field) => field.key.startsWith('bt_guard_')).length).toBe(rust.length)
  })

  test('DAEMON_BT_GUARD_* 方法常量与 method.rs 一致', async () => {
    const source = await read('method.rs')
    const rust = [...source.matchAll(/pub const (DAEMON_BT_GUARD_\w+): &str = "([^"]+)"/g)]
    expect(rust.length).toBe(9)
    for (const [, name, wire] of rust) expect((METHOD as Record<string, string>)[name as string]).toBe(wire as string)
  })
})
