import { describe, expect, test } from 'bun:test'
import {
  appendedBanClient,
  encodeClientRules,
  movedClientRule,
  newClientRule,
  nextClientRuleId,
  parseClientRules,
  removedClientRule,
  upsertedClientRule,
  validateClientRule,
} from './btGuardClientRules'

const rule = (extra: Record<string, unknown>) => ({ id: 'a', field: 'clientName', op: 'contains', value: 'Xunlei', action: 'ban', ...extra })

describe('parseClientRules', () => {
  test('空串 / 非法 JSON / 非数组 → 空', () => {
    expect(parseClientRules('')).toEqual([])
    expect(parseClientRules(undefined)).toEqual([])
    expect(parseClientRules('{')).toEqual([])
    expect(parseClientRules('{"id":"a"}')).toEqual([])
  })

  test('形状不符（类型错误）整份为空', () => {
    expect(parseClientRules(JSON.stringify([rule({}), rule({ id: 1 })]))).toEqual([])
    expect(parseClientRules(JSON.stringify([rule({ banMinutes: -1 })]))).toEqual([])
    expect(parseClientRules(JSON.stringify([rule({}), 'x']))).toEqual([])
  })

  test('单条取值非法只丢该条', () => {
    const list = parseClientRules(
      JSON.stringify([rule({ field: 'ip' }), rule({ op: 'regex' }), rule({ action: 'kick' }), rule({ when: 'never' }), rule({ id: '' }), rule({ value: '' }), rule({ id: 'ok' })]),
    )
    expect(list.map((r) => r.id)).toEqual(['ok'])
  })

  test('缺省值与 verdict / seedingOnly 兼容', () => {
    const [plain, legacySeed, verdict, whenWins] = parseClientRules(
      JSON.stringify([
        rule({}),
        rule({ id: 'b', action: 'throttle', seedingOnly: true }),
        rule({ id: 'c', action: 'throttle', verdict: 'allow' }),
        rule({ id: 'd', seedingOnly: true, when: 'downloading' }),
      ]),
    )
    expect(plain).toEqual({ id: 'a', field: 'clientName', op: 'contains', value: 'Xunlei', action: 'ban', when: 'always', banMinutes: 4320, reason: '' })
    expect(legacySeed?.when).toBe('seeding')
    expect(legacySeed?.action).toBe('throttle')
    expect(verdict?.action).toBe('allow')
    expect(whenWins?.when).toBe('downloading')
  })

  test('verdict 非法丢条（即使 action 合法）', () => {
    expect(parseClientRules(JSON.stringify([rule({ verdict: 'kick' })]))).toEqual([])
  })

  test('banMinutes 钳到 1..=525600', () => {
    const [low, high] = parseClientRules(JSON.stringify([rule({ banMinutes: 0 }), rule({ id: 'b', banMinutes: 9_999_999 })]))
    expect(low?.banMinutes).toBe(1)
    expect(high?.banMinutes).toBe(525_600)
  })

  test('最多 200 条', () => {
    const many = Array.from({ length: 250 }, (_, i) => rule({ id: `r${i}` }))
    const list = parseClientRules(JSON.stringify(many))
    expect(list.length).toBe(200)
    expect(list[199]?.id).toBe('r199')
  })
})

describe('encode / edit', () => {
  test('编码写全 8 个字段且不写旧字段', () => {
    const [parsed] = parseClientRules(JSON.stringify([rule({ seedingOnly: true, verdict: 'choke', reason: 'r' })]))
    const encoded = JSON.parse(encodeClientRules(parsed ? [parsed] : [])) as Record<string, unknown>[]
    expect(Object.keys(encoded[0] ?? {})).toEqual(['id', 'field', 'op', 'value', 'action', 'when', 'banMinutes', 'reason'])
    expect(encoded[0]).toEqual({ id: 'a', field: 'clientName', op: 'contains', value: 'Xunlei', action: 'choke', when: 'seeding', banMinutes: 4320, reason: 'r' })
  })

  test('唯一 id / upsert / 删除 / 调序', () => {
    const a = { ...newClientRule('local-1'), value: 'x' }
    const c = { ...newClientRule('local-3'), value: 'z' }
    expect(nextClientRuleId([a, c])).toBe('local-2')
    const list = upsertedClientRule([a, c], { ...newClientRule('local-2'), value: 'y' }) ?? []
    expect(list.map((r) => r.value)).toEqual(['x', 'z', 'y'])
    expect(upsertedClientRule(list, { ...a, value: 'X2' })?.[0]?.value).toBe('X2')
    expect(movedClientRule(list, 'local-2', -1)?.map((r) => r.id)).toEqual(['local-1', 'local-2', 'local-3'])
    expect(movedClientRule(list, 'local-1', -1)).toBeNull()
    expect(removedClientRule(list, 'local-3')?.length).toBe(2)
    expect(removedClientRule(list, 'nope')).toBeNull()
    const full = Array.from({ length: 200 }, (_, i) => ({ ...newClientRule(`r${i}`), value: 'v' }))
    expect(upsertedClientRule(full, { ...newClientRule('new'), value: 'v' })).toBeNull()
  })

  test('校验', () => {
    const ok = { ...newClientRule('local-1'), value: 'x' }
    expect(validateClientRule(ok)).toBeNull()
    expect(validateClientRule({ ...ok, id: 'Bad' })).toBe('id')
    expect(validateClientRule({ ...ok, value: '' })).toBe('valueRequired')
    expect(validateClientRule({ ...ok, value: 'x'.repeat(65) })).toBe('valueTooLong')
    expect(validateClientRule({ ...ok, banMinutes: 0 })).toBe('banMinutes')
    expect(validateClientRule({ ...ok, reason: 'x'.repeat(201) })).toBe('reasonTooLong')
  })

  test('封禁此客户端：截断 64、忽略大小写去重', () => {
    const list = appendedBanClient([], `  ${'q'.repeat(70)} `) ?? []
    expect(list[0]).toEqual({ id: 'local-1', field: 'clientName', op: 'contains', value: 'q'.repeat(64), action: 'ban', when: 'always', banMinutes: 4320, reason: '' })
    const xl = appendedBanClient(list, 'Xunlei 0.0.1') ?? []
    expect(xl.length).toBe(2)
    expect(appendedBanClient(xl, 'XUNLEI 0.0.1')).toBeNull()
    expect(appendedBanClient(xl, '   ')).toBeNull()
  })
})
