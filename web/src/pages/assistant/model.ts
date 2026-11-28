// 助手页的纯模型：云端历史与本机实时执行的合并、工具确认状态推导、可用性判定与错误分类。
// GPUI `crates/assistant/src/model.rs`（及 `view.rs` 里的纯判定）的逐条移植，单测见 model.test.ts；
// 这里不碰 React，时间由调用方传入。

import { isAssistantRunActive } from '../../lib/rpc/protocol'
import type {
  AssistantConfigDto,
  AssistantIssueDto,
  AssistantMessageDto,
  AssistantRunDto,
  AssistantSessionDto,
  AssistantSessionKind,
  AssistantStateDto,
  AssistantToolCallDto,
  JsonValue,
  RpcErrorData,
} from '../../lib/rpc/protocol'

/** 会话列表一页的条数。 */
export const SESSION_PAGE_SIZE = 30
/** 工具参数摘要的最大字符数。 */
export const ARGUMENT_SUMMARY_CHARS = 120
/** 历史里客户端工具结果预览的最大字符数。 */
const OUTPUT_PREVIEW_CHARS = 400
/** 时间戳比较只取到秒（`YYYY-MM-DDTHH:MM:SS`），避免小数位数不同导致的字典序误判。 */
const TIMESTAMP_SECOND_CHARS = 19
/** 会话日期取 `updatedAt` 的前 10 位（`YYYY-MM-DD`）。 */
export const SESSION_DATE_CHARS = 10
/** 「开通会员」卡片的唯一信号：服务端工具 `offer_membership`（与 FluxCloud 约定）。 */
export const MEMBERSHIP_OFFER_TOOL = 'offer_membership'

/** 本地确认 / 丢弃后的问题单最新状态（键 = 问题单 ID）。 */
export type IssueOverrides = ReadonlyMap<string, AssistantIssueDto>

/** 本地刚提交的回答评价（键 = 消息 ID；null = 已清除）。 */
export type RatingOverrides = ReadonlyMap<string, string | null>

// ───────────────────────── 时间线 ─────────────────────────

/** 一轮回答里按到达顺序排列的条目。 */
export type Block =
  | { type: 'text'; text: string; message?: AnswerMessage }
  | { type: 'tool'; call: AssistantToolCallDto }
  | { type: 'issue'; issue: AssistantIssueDto }

/** 实时执行的状态（历史轮次没有）。 */
export interface Live {
  status: AssistantRunDto['status']
  error: string | null
}

/** 一条回答消息：评价按它记录（`rating`：`up` / `down`）。 */
export interface AnswerMessage {
  id: string
  rating: string | null
}

/** 一问一答：用户提问、中间过程、最终回答与问题单草稿。 */
export interface Turn {
  /** 折叠状态等界面状态的键：执行 ID 优先，其次消息 / 起始时间。 */
  key: string
  question: string
  /** 用户消息附带的图片（`data:` URL，仅云端历史轮次有）。 */
  images: readonly string[]
  /** 最终回答之前的全部条目：中间文本与工具调用（可折叠的「过程」）。 */
  steps: Block[]
  /** 最终回答（Markdown）；尚未产出时为 null。 */
  answer: string | null
  /** 最终回答对应的 `assistant` 消息 ID（评价按它记录）；尚未落库 / 未知时为 null。 */
  answerId: string | null
  /** 最终回答的评价（`up` / `down`）；未评价为 null。 */
  rating: string | null
  issues: AssistantIssueDto[]
  /** 本轮是否显示「开通会员」卡片：存在成功的 `offer_membership` 工具调用（该工具条目不进 `steps`）。 */
  membershipOffer: boolean
  live: Live | null
  /** 云端历史轮次的起始时间（RFC 3339），用于把问题单挂到对应轮次；实时轮次为 null。 */
  startedAt: string | null
}

export function runActive(turn: Turn): boolean {
  return turn.live !== null && isAssistantRunActive(turn.live.status)
}

/** 本轮是否有等待用户确认的工具调用（未过期）。 */
export function hasPendingConfirmation(turn: Turn, nowMs: number): boolean {
  const active = runActive(turn)
  return turn.steps.some((block) => block.type === 'tool' && confirmView(block.call, active, nowMs)?.kind === 'pending')
}

export function toolCount(turn: Turn): number {
  return turn.steps.filter((block) => block.type === 'tool').length
}

/**
 * 「过程」折叠区是否展开：有待确认的工具时强制展开；否则取用户选择，缺省为
 * 「执行中且最终回答尚未产出」时展开，回答出现后收起。
 */
export function stepsOpen(turn: Turn, userChoice: boolean | undefined, nowMs: number): boolean {
  if (hasPendingConfirmation(turn, nowMs)) return true
  return userChoice ?? (runActive(turn) && turn.answer === null)
}

function splitBlocks(blocks: readonly Block[]): {
  steps: Block[]
  answer: string | null
  answerMessage: AnswerMessage | null
  issues: AssistantIssueDto[]
  membershipOffer: boolean
} {
  const issues: AssistantIssueDto[] = []
  const steps: Block[] = []
  let membershipOffer = false
  for (const block of blocks) {
    if (block.type === 'issue') issues.push(block.issue)
    else if (block.type === 'tool' && block.call.name === MEMBERSHIP_OFFER_TOOL) {
      // 不论状态都不进过程；仅成功的调用触发卡片，同轮多次只算一张。
      if (block.call.status === 'succeeded') membershipOffer = true
    } else if (block.type !== 'text' || block.text.trim() !== '') steps.push(block)
  }
  const last = steps[steps.length - 1]
  if (last?.type === 'text') {
    steps.pop()
    return { steps, answer: last.text, answerMessage: last.message ?? null, issues, membershipOffer }
  }
  return { steps, answer: null, answerMessage: null, issues, membershipOffer }
}

/** 问题单在本地确认 / 丢弃后的最新状态优先于推送与历史里的旧副本。 */
function resolveIssue(issue: AssistantIssueDto, overrides: IssueOverrides): AssistantIssueDto {
  return overrides.get(issue.id) ?? issue
}

/** 本地刚提交的评价优先于历史消息自带的评价。 */
function resolveRating(messageId: string, fallback: string | null, ratings: RatingOverrides): string | null {
  return ratings.has(messageId) ? (ratings.get(messageId) ?? null) : fallback
}

function liveTurn(run: AssistantRunDto, overrides: IssueOverrides, ratings: RatingOverrides): Turn {
  const blocks = (run.items ?? []).map((item): Block => {
    switch (item.type) {
      case 'text':
        return { type: 'text', text: item.text }
      case 'tool':
        return { type: 'tool', call: item.call }
      case 'issueDraft':
        return { type: 'issue', issue: resolveIssue(item.issue, overrides) }
    }
  })
  const { steps, answer, issues, membershipOffer } = splitBlocks(blocks)
  const answerId = answer !== null && run.messageId ? run.messageId : null
  return {
    key: run.runId ? `run:${run.runId}` : `live:${run.startedAtUnixMs}`,
    question: run.question,
    images: [],
    steps,
    answer,
    answerId,
    rating: answerId === null ? null : resolveRating(answerId, null, ratings),
    issues,
    membershipOffer,
    live: { status: run.status, error: run.error ? run.error : null },
    startedAt: null,
  }
}

interface HistoryTurn {
  runId: string | null
  key: string
  startedAt: string
  question: string
  images: readonly string[]
  blocks: Block[]
}

function newHistoryTurn(message: AssistantMessageDto): HistoryTurn {
  const runId = message.runId ?? null
  return {
    runId,
    key: runId === null ? `msg:${message.id}` : `run:${runId}`,
    startedAt: message.createdAt,
    question: '',
    images: [],
    blocks: [],
  }
}

function parseJson(text: string): JsonValue | undefined {
  try {
    return JSON.parse(text) as JsonValue
  } catch {
    return undefined
  }
}

function toolFromCall(message: AssistantMessageDto): AssistantToolCallDto {
  const parsed = message.role === 'tool_call' ? parseJson(message.content) : undefined
  return {
    callId: message.toolCallId ?? '',
    name: message.toolName ?? '',
    kind: message.toolKind ?? 'server',
    status: 'running',
    durationMs: message.durationMs ?? null,
    arguments: parsed !== undefined && argumentsSummary(parsed) !== null ? parsed : null,
  }
}

function completeTool(turn: HistoryTurn, message: AssistantMessageDto): void {
  const matches = (call: AssistantToolCallDto): boolean =>
    call.status === 'running' && (message.toolCallId != null ? call.callId === message.toolCallId : call.name === message.toolName)
  let position = -1
  for (let index = turn.blocks.length - 1; index >= 0; index -= 1) {
    const block = turn.blocks[index]
    if (block?.type === 'tool' && matches(block.call)) {
      position = index
      break
    }
  }
  const preview = message.toolKind === 'client' ? truncateChars(message.content.trim(), OUTPUT_PREVIEW_CHARS) : ''
  const status = message.isError ? 'failed' : 'succeeded'
  const outputPreview = preview === '' ? null : preview
  const found = turn.blocks[position]
  if (found?.type === 'tool') {
    found.call = { ...found.call, status, durationMs: message.durationMs ?? null, outputPreview }
    return
  }
  turn.blocks.push({ type: 'tool', call: { ...toolFromCall(message), status, outputPreview } })
}

function pushMessage(turn: HistoryTurn, message: AssistantMessageDto): void {
  switch (message.role) {
    case 'user':
      turn.question = message.content
      turn.images = message.images ?? []
      break
    case 'assistant':
      if (message.content.trim() !== '') {
        turn.blocks.push({ type: 'text', text: message.content, message: { id: message.id, rating: message.rating ?? null } })
      }
      break
    case 'tool_call':
      turn.blocks.push({ type: 'tool', call: toolFromCall(message) })
      break
    case 'tool_result':
      completeTool(turn, message)
      break
    default:
      break
  }
}

function intoTurn(turn: HistoryTurn, ratings: RatingOverrides): Turn {
  const { steps, answer, answerMessage, issues, membershipOffer } = splitBlocks(turn.blocks)
  return {
    key: turn.key,
    question: turn.question,
    images: turn.images,
    steps,
    answer,
    answerId: answerMessage?.id ?? null,
    rating: answerMessage ? resolveRating(answerMessage.id, answerMessage.rating, ratings) : null,
    issues,
    membershipOffer,
    live: null,
    startedAt: turn.startedAt,
  }
}

/**
 * 合并云端历史与本机实时执行。
 *
 * - 历史消息按 `seq` 排序，每条用户消息（或执行 ID 变化）开启新一轮；
 * - 进行中的执行永远用实时覆盖层：同一 `runId` 的历史消息被它取代（原位替换，避免半截历史
 *   与流式内容并存）；
 * - 已结束的执行若已落库（历史里出现同一 `runId`）以历史为准，不再重复显示；未落库
 *   （如首帧前失败、被取消）的保留覆盖层；
 * - 云端问题单按创建时间挂到最近的历史轮次；实时执行里已带的问题单不重复；
 * - 每轮回答的评价取本地刚提交的结果（`ratings`），否则取历史消息自带的评价。
 */
export function buildTimeline(
  sessionId: string,
  messages: readonly AssistantMessageDto[],
  issues: readonly AssistantIssueDto[],
  runs: readonly AssistantRunDto[],
  overrides: IssueOverrides,
  ratings: RatingOverrides,
): Turn[] {
  const live = runs.filter((run) => run.sessionId === sessionId)
  const ordered = [...messages].sort((a, b) => a.seq - b.seq)

  const groups: HistoryTurn[] = []
  for (const message of ordered) {
    const current = groups[groups.length - 1]
    if (current === undefined || message.role === 'user' || current.runId !== (message.runId ?? null)) {
      const group = newHistoryTurn(message)
      groups.push(group)
      pushMessage(group, message)
    } else {
      pushMessage(current, message)
    }
  }

  const consumed = live.map(() => false)
  const turns: Turn[] = []
  for (const group of groups) {
    const index = group.runId === null ? -1 : live.findIndex((run) => run.runId === group.runId)
    const run = live[index]
    if (run === undefined) {
      turns.push(intoTurn(group, ratings))
      continue
    }
    const already = consumed[index] === true
    consumed[index] = true
    if (!isAssistantRunActive(run.status)) turns.push(intoTurn(group, ratings))
    else if (!already) turns.push(liveTurn(run, overrides, ratings))
  }
  live.forEach((run, index) => {
    if (!consumed[index]) turns.push(liveTurn(run, overrides, ratings))
  })
  placeIssues(turns, issues, overrides)
  return turns
}

function secondKey(timestamp: string): string {
  return timestamp.slice(0, TIMESTAMP_SECOND_CHARS)
}

function placeIssues(turns: Turn[], issues: readonly AssistantIssueDto[], overrides: IssueOverrides): void {
  for (const raw of issues) {
    if (turns.some((turn) => turn.issues.some((known) => known.id === raw.id))) continue
    const issue = resolveIssue(raw, overrides)
    const created = secondKey(issue.createdAt)
    let target = turns.findLastIndex((turn) => turn.startedAt !== null && secondKey(turn.startedAt) <= created)
    if (target < 0) target = turns.findIndex((turn) => turn.startedAt !== null)
    if (target < 0) target = turns.length - 1
    const turn = turns[target]
    if (turn) {
      turn.issues.push(issue)
      continue
    }
    turns.push({
      key: `issue:${issue.id}`,
      question: '',
      images: [],
      steps: [],
      answer: null,
      answerId: null,
      rating: null,
      issues: [issue],
      membershipOffer: false,
      live: null,
      startedAt: null,
    })
  }
}

// ───────────────────────── 工具调用 ─────────────────────────

/** 工具调用在界面上的呈现状态。`interrupted`：所属执行已结束，工具却没有结果（连接中断等）。 */
export type ToolState = 'running' | 'succeeded' | 'failed' | 'interrupted'

export function toolState(call: AssistantToolCallDto, runIsActive: boolean): ToolState {
  if (call.status === 'running') return runIsActive ? 'running' : 'interrupted'
  return call.status
}

/** 确认卡片的呈现状态；`remainingMs` 为到期前剩余毫秒（无期限时为 null）。 */
export type ConfirmView =
  | { kind: 'pending'; remainingMs: number | null }
  | { kind: 'approved' }
  | { kind: 'declined' }
  | { kind: 'expired' }

/**
 * 推导确认卡片状态。null = 该工具不需要确认（只读）。
 *
 * 云端 / agent 的「过期」推送可能晚于本机时钟：待确认超过截止时间即视为过期；所属执行
 * 已结束时仍「待确认」的调用同样不再可操作。
 */
export function confirmView(call: AssistantToolCallDto, runIsActive: boolean, nowMs: number): ConfirmView | null {
  switch (call.confirmation) {
    case undefined:
    case null:
      return null
    case 'approved':
      return { kind: 'approved' }
    case 'declined':
      return { kind: 'declined' }
    case 'expired':
      return { kind: 'expired' }
    case 'pending': {
      if (!runIsActive) return { kind: 'expired' }
      const deadline = call.confirmDeadlineUnixMs
      if (deadline === undefined || deadline === null) return { kind: 'pending', remainingMs: null }
      if (deadline <= nowMs) return { kind: 'expired' }
      return { kind: 'pending', remainingMs: deadline - nowMs }
    }
  }
}

/** 剩余毫秒 → 向上取整的秒数（倒计时显示）。 */
export function remainingSeconds(remainingMs: number): number {
  return Math.trunc((remainingMs + 999) / 1000)
}

/** 倒计时文案：一分钟以内 `N s`，否则 `M:SS`。 */
export function formatCountdown(totalSeconds: number): string {
  const seconds = Math.max(0, totalSeconds)
  if (seconds < 60) return `${seconds} s`
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
}

function pendingToolCalls(state: AssistantStateDto, nowMs: number): { sessionId: string }[] {
  const found: { sessionId: string }[] = []
  for (const run of state.runs) {
    if (!isAssistantRunActive(run.status)) continue
    for (const item of run.items ?? []) {
      if (item.type === 'tool' && confirmView(item.call, true, nowMs)?.kind === 'pending') found.push({ sessionId: run.sessionId })
    }
  }
  return found
}

/** 所有进行中执行里等待确认的调用数。 */
export function pendingConfirmations(state: AssistantStateDto, nowMs: number): number {
  return pendingToolCalls(state, nowMs).length
}

/** 当前会话之外、有待确认工具的会话（头部提示据此给出跳转）。 */
export function pendingElsewhere(state: AssistantStateDto, current: string | null, nowMs: number): string | null {
  return pendingToolCalls(state, nowMs).find((call) => call.sessionId !== current)?.sessionId ?? null
}

/** 模型给出的参数的一行摘要；没有有意义的参数时为 null。 */
export function argumentsSummary(value: JsonValue | null | undefined): string | null {
  if (value === undefined || value === null) return null
  if (typeof value === 'string' && value.trim() === '') return null
  if (typeof value === 'object' && Object.keys(value).length === 0) return null
  return truncateChars(JSON.stringify(value), ARGUMENT_SUMMARY_CHARS)
}

/** 参数的完整缩进 JSON（展开查看）。 */
export function argumentsPretty(value: JsonValue): string {
  return JSON.stringify(value, null, 2)
}

/** 工具输出预览的展示文本：去首尾空白后能解析为 JSON 则缩进美化，否则（含被截断的非法 JSON）原样返回修剪后的文本。 */
export function formatToolOutput(text: string): string {
  const trimmed = text.trim()
  if (trimmed === '') return trimmed
  try {
    return JSON.stringify(JSON.parse(trimmed) as JsonValue, null, 2)
  } catch {
    return trimmed
  }
}

/** 按字符（Unicode 码点）截断，超长时追加 `…`；不会把代理对切成两半。 */
export function truncateChars(text: string, max: number): string {
  let count = 0
  let end = 0
  for (const char of text) {
    if (count === max) return `${text.slice(0, end)}…`
    count += 1
    end += char.length
  }
  return text
}

/** Unicode 码点数（对应 Rust `chars().count()`）。 */
export function charCount(text: string): number {
  let count = 0
  for (const _char of text) count += 1
  return count
}

/** 工具耗时：不足 1 秒用毫秒，否则一位小数的秒。 */
export function formatDuration(durationMs: number): string {
  if (durationMs < 1000) return `${Math.max(0, durationMs)} ms`
  return `${Math.floor(durationMs / 1000)}.${Math.floor((durationMs % 1000) / 100)} s`
}

// ───────────────────────── 可用性与错误 ─────────────────────────

/** 页面级门禁：决定显示对话还是说明页。 */
export type Gate =
  | { kind: 'signedOut' }
  /** 已登录、配置尚未取得。 */
  | { kind: 'loading' }
  /** 服务端未开放（`off` / `internal_only` / 未知原因）。 */
  | { kind: 'notOpen' }
  /** 当前套餐不含助手。 */
  | { kind: 'planDisabled' }
  /** 配置取不到且本机服务 / 云端报错。 */
  | { kind: 'serviceError' }
  /** 可对话；额度用尽时仍可浏览历史，但不能提问。 */
  | { kind: 'open'; quotaExhausted: boolean }

export function quotaExhausted(config: AssistantConfigDto): boolean {
  return config.dailyLimit > 0 && config.usedToday >= config.dailyLimit
}

export function gate(signedIn: boolean, state: AssistantStateDto, fetchFailed: boolean): Gate {
  if (!signedIn) return { kind: 'signedOut' }
  const config = state.config
  if (config === undefined || config === null) {
    const hasError = (state.lastErrorReason ?? null) !== null || (state.lastErrorMessage ?? null) !== null
    return { kind: fetchFailed || (!state.loading && hasError) ? 'serviceError' : 'loading' }
  }
  if (!config.available) return { kind: config.reason === 'plan_disabled' ? 'planDisabled' : 'notOpen' }
  return { kind: 'open', quotaExhausted: quotaExhausted(config) }
}

/** 提问失败的归类（决定提示文案与是否需要重新取配置）。 */
export type SendFailure =
  | 'signedOut'
  | 'notOpen'
  | 'quotaExceeded'
  /** 该会话已有执行在进行。 */
  | 'busy'
  | 'unavailable'
  /** 参数被拒（过长等）。 */
  | 'rejected'
  | 'failed'

export function classifySendError(error: Pick<RpcErrorData, 'code' | 'reason'> | undefined): SendFailure {
  if (error?.reason === 'rateLimited') return 'quotaExceeded'
  if (error?.reason === 'sessionExpired') return 'signedOut'
  if (error?.reason === 'cloudUnreachable') return 'unavailable'
  switch (error?.code) {
    case 'unauthorized':
      return 'signedOut'
    case 'unsupported':
      return 'notOpen'
    case 'conflict':
      return 'busy'
    case 'unavailable':
    case 'timeout':
      return 'unavailable'
    case 'invalidArgument':
      return 'rejected'
    default:
      return 'failed'
  }
}

/** 提问失败的提示文案键。 */
export const SEND_FAILURE_KEY: Readonly<Record<SendFailure, string>> = {
  signedOut: 'assistantErrSignedOut',
  notOpen: 'assistantErrNotOpen',
  quotaExceeded: 'assistantErrQuota',
  busy: 'assistantErrBusy',
  unavailable: 'assistantErrUnavailable',
  rejected: 'assistantErrRejected',
  failed: 'assistantErrFailed',
}

/** 提问输入的校验入参（`draft` 已去首尾空白）。 */
export interface SendCheck {
  gate: Gate
  stale: boolean
  sending: boolean
  hasActiveRun: boolean
  draft: string
  maxMessageChars: number
}

/** 与 GPUI `AssistantView::can_send` 同判定：门禁开放且未用尽额度、未断线、无进行中、非空、未超长。 */
export function canSend(check: SendCheck): boolean {
  if (check.gate.kind !== 'open' || check.gate.quotaExhausted) return false
  if (check.stale || check.sending || check.hasActiveRun) return false
  if (check.draft === '') return false
  return check.maxMessageChars === 0 || charCount(check.draft) <= check.maxMessageChars
}

// ───────────────────────── 会话与状态 ─────────────────────────

/** 当前会话里进行中的执行（取最新的一条）。 */
export function currentRun(state: AssistantStateDto, current: string | null): AssistantRunDto | undefined {
  if (current === null) return undefined
  return state.runs.findLast((run) => run.sessionId === current && isAssistantRunActive(run.status))
}

/** 当前会话的摘要：详情里的优先，其次会话列表里的。 */
export function currentSession(
  current: string | null,
  detailSession: AssistantSessionDto | null,
  items: readonly AssistantSessionDto[],
): AssistantSessionDto | undefined {
  if (current === null) return undefined
  return detailSession?.id === current ? detailSession : items.find((session) => session.id === current)
}

/** 当前会话的类型（新对话取待发送的类型）。 */
export function currentKind(current: string | null, pending: AssistantSessionKind, session: AssistantSessionDto | undefined): AssistantSessionKind {
  if (current === null) return pending
  return session?.kind === 'feedback' ? 'feedback' : 'ask'
}

/** 会话标题（去首尾空白）；空标题为 null（界面显示「新会话」）。 */
export function sessionTitle(title: string): string | null {
  const trimmed = title.trim()
  return trimmed === '' ? null : trimmed
}

/** 进行中的执行：`(起始时间, 会话 ID)` 的稳定键。 */
function activeRunKey(run: AssistantRunDto): string {
  return `${run.startedAtUnixMs}:${run.sessionId}`
}

/** `before` 里进行中、`after` 里不再进行（结束或被裁掉）的执行所在会话。 */
export function finishedSessions(before: AssistantStateDto, after: AssistantStateDto): string[] {
  const still = new Set(after.runs.filter((run) => isAssistantRunActive(run.status)).map(activeRunKey))
  return before.runs.filter((run) => isAssistantRunActive(run.status) && !still.has(activeRunKey(run))).map((run) => run.sessionId)
}

/** 仍待确认的请求 ID（不看截止时间）。 */
export function pendingRequestIds(state: AssistantStateDto): Set<string> {
  const ids = new Set<string>()
  for (const run of state.runs) {
    for (const item of run.items ?? []) {
      if (item.type === 'tool' && item.call.confirmation === 'pending' && item.call.requestId) ids.add(item.call.requestId)
    }
  }
  return ids
}

/** 在本地覆盖、实时执行与云端历史里找问题单。 */
export function findIssue(
  issueId: string,
  overrides: IssueOverrides,
  state: AssistantStateDto,
  detailIssues: readonly AssistantIssueDto[],
): AssistantIssueDto | undefined {
  const overridden = overrides.get(issueId)
  if (overridden) return overridden
  for (const run of state.runs) {
    for (const item of run.items ?? []) {
      if (item.type === 'issueDraft' && item.issue.id === issueId) return item.issue
    }
  }
  return detailIssues.find((issue) => issue.id === issueId)
}

/** 会话列表追加一页：按 ID 去重，保持原有顺序。 */
export function appendSessions(items: readonly AssistantSessionDto[], page: readonly AssistantSessionDto[]): AssistantSessionDto[] {
  const known = new Set(items.map((session) => session.id))
  return [...items, ...page.filter((session) => !known.has(session.id))]
}
