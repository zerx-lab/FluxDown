// BT 防吸血本机名字规则：daemon 配置键 `bt_guard_client_rules`（JSON 数组字符串，顺序即优先级）。
// 解析与 engine `bt_guard/cloud.rs::{ClientRule::compile, parse_local_client_rules}` 逐条一致；
// 匹配只在 engine 里做。

import { createConfigListPatcher } from './configList'
import type { BtGuardClientRuleDto } from './rpc'

export const CLIENT_RULES_KEY = 'bt_guard_client_rules'

export type ClientRule = Required<BtGuardClientRuleDto>
export type ClientRuleField = ClientRule['field']
export type ClientRuleOp = ClientRule['op']
export type ClientRuleAction = ClientRule['action']
export type ClientRuleWhen = ClientRule['when']

export const CLIENT_RULE_FIELDS: readonly ClientRuleField[] = ['peerIdPrefix', 'clientName']
export const CLIENT_RULE_OPS: readonly ClientRuleOp[] = ['prefix', 'contains']
export const CLIENT_RULE_ACTIONS: readonly ClientRuleAction[] = ['allow', 'throttle', 'choke', 'ban']
export const CLIENT_RULE_WHENS: readonly ClientRuleWhen[] = ['always', 'seeding', 'downloading']

export const DEFAULT_BAN_MINUTES = 4320
export const MAX_BAN_MINUTES = 525_600
export const MAX_CLIENT_RULES = 200
export const MAX_VALUE_CHARS = 64
export const MAX_REASON_CHARS = 200

const ID_PATTERN = /^[a-z0-9_-]{1,32}$/

const oneOf = <T extends string>(values: readonly T[], value: string): T | null =>
  (values as readonly string[]).includes(value) ? (value as T) : null

/** serde 形状不符（类型错误）时抛出，整份视为非法。 */
class Malformed extends Error {}

function strField(item: Record<string, unknown>, key: string): string {
  const value = item[key]
  if (value === undefined) return ''
  if (typeof value !== 'string') throw new Malformed(key)
  return value
}

function optStrField(item: Record<string, unknown>, key: string): string | null {
  const value = item[key]
  if (value === undefined || value === null) return null
  if (typeof value !== 'string') throw new Malformed(key)
  return value
}

function compile(raw: unknown): ClientRule | null {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) throw new Malformed('item')
  const item = raw as Record<string, unknown>
  const id = strField(item, 'id')
  const fieldRaw = strField(item, 'field')
  const opRaw = strField(item, 'op')
  const value = strField(item, 'value')
  const actionRaw = strField(item, 'action')
  const reason = strField(item, 'reason')
  const verdict = optStrField(item, 'verdict')
  const whenRaw = optStrField(item, 'when')
  const seedingOnlyRaw = item.seedingOnly
  if (seedingOnlyRaw !== undefined && typeof seedingOnlyRaw !== 'boolean') throw new Malformed('seedingOnly')
  const banRaw = item.banMinutes
  if (banRaw !== undefined && banRaw !== null && !(typeof banRaw === 'number' && Number.isInteger(banRaw) && banRaw >= 0 && banRaw <= 0xffff_ffff)) {
    throw new Malformed('banMinutes')
  }

  const field = oneOf(CLIENT_RULE_FIELDS, fieldRaw)
  const op = oneOf(CLIENT_RULE_OPS, opRaw)
  const action = oneOf(CLIENT_RULE_ACTIONS, verdict ?? actionRaw)
  const when: ClientRuleWhen | null = whenRaw !== null ? oneOf(CLIENT_RULE_WHENS, whenRaw) : seedingOnlyRaw === true ? 'seeding' : 'always'
  if (!field || !op || !action || !when || value === '' || id === '') return null
  const banMinutes = typeof banRaw === 'number' ? Math.min(Math.max(banRaw, 1), MAX_BAN_MINUTES) : DEFAULT_BAN_MINUTES
  return { id, field, op, value, action, when, banMinutes, reason }
}

/** 空串 / 非法 JSON / 非数组 / 元素形状不符 → 空列表；单条取值非法丢该条；最多 200 条。 */
export function parseClientRules(raw: string | undefined): ClientRule[] {
  if (!raw || raw.trim() === '') return []
  try {
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    return parsed
      .map(compile)
      .filter((rule): rule is ClientRule => rule !== null)
      .slice(0, MAX_CLIENT_RULES)
  } catch {
    return []
  }
}

/** 总是写出全部 8 个字段，不写旧字段。 */
export function encodeClientRules(list: readonly ClientRule[]): string {
  return JSON.stringify(
    list.map((rule) => ({
      id: rule.id,
      field: rule.field,
      op: rule.op,
      value: rule.value,
      action: rule.action,
      when: rule.when,
      banMinutes: rule.banMinutes,
      reason: rule.reason,
    })),
  )
}

export type ClientRuleIssue = 'id' | 'valueRequired' | 'valueTooLong' | 'banMinutes' | 'reasonTooLong'

const charCount = (text: string) => Array.from(text).length

/** 编辑器校验；合法返回 null。 */
export function validateClientRule(rule: ClientRule): ClientRuleIssue | null {
  if (!ID_PATTERN.test(rule.id)) return 'id'
  if (rule.value === '') return 'valueRequired'
  if (charCount(rule.value) > MAX_VALUE_CHARS) return 'valueTooLong'
  if (!Number.isInteger(rule.banMinutes) || rule.banMinutes < 1 || rule.banMinutes > MAX_BAN_MINUTES) return 'banMinutes'
  if (charCount(rule.reason) > MAX_REASON_CHARS) return 'reasonTooLong'
  return null
}

/** 列表内未占用的 `local-<n>`（n 从 1 起取最小空位）。 */
export function nextClientRuleId(list: readonly ClientRule[]): string {
  const taken = new Set(list.map((rule) => rule.id))
  for (let n = 1; ; n += 1) {
    const id = `local-${n}`
    if (!taken.has(id)) return id
  }
}

export function newClientRule(id: string): ClientRule {
  return { id, field: 'clientName', op: 'contains', value: '', action: 'ban', when: 'always', banMinutes: DEFAULT_BAN_MINUTES, reason: '' }
}

/** 按 id 覆盖，不存在则追加；新增超过 200 条返回 null。 */
export function upsertedClientRule(list: readonly ClientRule[], draft: ClientRule): ClientRule[] | null {
  const index = list.findIndex((rule) => rule.id === draft.id)
  if (index >= 0) return list.map((rule, i) => (i === index ? draft : rule))
  if (list.length >= MAX_CLIENT_RULES) return null
  return [...list, draft]
}

/** 删除；id 不存在返回 null。 */
export function removedClientRule(list: readonly ClientRule[], id: string): ClientRule[] | null {
  if (!list.some((rule) => rule.id === id)) return null
  return list.filter((rule) => rule.id !== id)
}

/** 上移 / 下移一位；id 缺失或越界返回 null。 */
export function movedClientRule(list: readonly ClientRule[], id: string, delta: -1 | 1): ClientRule[] | null {
  const from = list.findIndex((rule) => rule.id === id)
  const to = from + delta
  if (from < 0 || to < 0 || to >= list.length) return null
  const next = [...list]
  const [item] = next.splice(from, 1)
  if (!item) return null
  next.splice(to, 0, item)
  return next
}

/** 已有同 field / op / value（忽略 ASCII 大小写）的规则。 */
export function hasSameMatcher(list: readonly ClientRule[], field: ClientRuleField, op: ClientRuleOp, value: string): boolean {
  const lower = value.toLowerCase()
  return list.some((rule) => rule.field === field && rule.op === op && rule.value.toLowerCase() === lower)
}

/** 从对端客户端名生成「包含 · 封禁 · 始终」规则；名字为空返回 null。 */
export function banClientDraft(list: readonly ClientRule[], client: string): ClientRule | null {
  const value = Array.from(client.trim()).slice(0, MAX_VALUE_CHARS).join('')
  if (value === '') return null
  return { ...newClientRule(nextClientRuleId(list)), value }
}

/** 追加封禁客户端规则；已有同 field / op / value 时不写。 */
export function appendedBanClient(list: readonly ClientRule[], client: string): ClientRule[] | null {
  const draft = banClientDraft(list, client)
  if (!draft || hasSameMatcher(list, draft.field, draft.op, draft.value)) return null
  return upsertedClientRule(list, draft)
}

export const patchClientRules = createConfigListPatcher(CLIENT_RULES_KEY, parseClientRules, encodeClientRules)

/** 编辑保存：新增时 id 若已被并发占用则改用最新列表的空位；新增且已有同 field / op / value 时不写（首条命中，重复无效）。 */
export const upsertClientRule = (draft: ClientRule, isNew: boolean) =>
  patchClientRules((list) => {
    if (!isNew) return upsertedClientRule(list, draft)
    if (hasSameMatcher(list, draft.field, draft.op, draft.value)) return null
    return upsertedClientRule(list, list.some((r) => r.id === draft.id) ? { ...draft, id: nextClientRuleId(list) } : draft)
  })

export const removeClientRule = (id: string) => patchClientRules((list) => removedClientRule(list, id))

export const moveClientRule = (id: string, delta: -1 | 1) => patchClientRules((list) => movedClientRule(list, id, delta))
