// 助手页的纯模型：云端历史与本机实时执行的合并、工具确认状态推导、可用性判定与错误分类。
// GPUI `crates/assistant/src/model.rs`（及 `view.rs` 里的纯判定）的逐条移植，单测见 model.test.ts；
// 这里不碰 React，时间由调用方传入。

import { ASSISTANT_MAX_TITLE_CHARS, isAssistantRunActive } from '../../lib/rpc/protocol'
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

/**
 * 一轮回答里按到达顺序排列的条目。`step`：工具调用之前的中间说明文字（云端 `assistant_step`），
 * 永远属于「过程」，不会被当成最终回答。
 */
export type Block =
  | { type: 'text'; text: string; message?: AnswerMessage; step?: boolean }
  | { type: 'tool'; call: AssistantToolCallDto }
  | { type: 'issue'; issue: AssistantIssueDto }

/** 实时执行的状态（历史轮次没有）。 */
export interface Live {
  status: AssistantRunDto['status']
  error: string | null
  /** 失败原因（`AssistantRunDto.errorReason`）；界面据此本地化。 */
  errorReason: string | null
  /** 旁观执行（其他设备 / 重启前发起）：本机不能确认其中的待确认调用。 */
  observing: boolean
  /** 执行 ID；null = 提问还没被云端接受（首帧前失败 / 被取消）。 */
  runId: string | null
}

/** 一条回答消息：评价按它记录（`rating`：`up` / `down`）；`interrupted`：回答中途中断，内容是已输出的部分。 */
export interface AnswerMessage {
  id: string
  rating: string | null
  interrupted: boolean
}

/** 一问一答：用户提问、中间过程、最终回答与问题单草稿。 */
export interface Turn {
  /** 折叠状态等界面状态的键：执行 ID 优先，其次消息 / 起始时间。 */
  key: string
  /** 用户消息 ID（重新生成 / 编辑重发的 `replaceFrom`）；实时轮次为 null。 */
  questionId: string | null
  question: string
  /** 用户消息附带图片的附件 ID（云端历史轮次；经 `agent.assistant.attachment` 取图）。 */
  images: readonly string[]
  /** 本轮提问的附图张数：历史轮次 = `images.length`，实时轮次取执行上报的张数。 */
  imageCount: number
  /** 刚发出的这条提问的本地缩略图（`data:` URL）；只有最后一轮、文本一致时才有。 */
  localImages: readonly string[]
  /** 最终回答之前的全部条目：中间文本与工具调用（可折叠的「过程」）。 */
  steps: Block[]
  /** 最终回答（Markdown）；尚未产出时为 null。 */
  answer: string | null
  /** 最终回答对应的 `assistant` 消息 ID（评价按它记录）；尚未落库 / 未知时为 null。 */
  answerId: string | null
  /** 回答中途中断（取消 / 超时 / 失败）：`answer` 是中断前已输出的部分。 */
  answerInterrupted: boolean
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
  // 中间说明文字（`assistant_step`）永远属于过程，不会被当成最终回答。
  if (last?.type === 'text' && last.step !== true) {
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

function liveStatus(run: AssistantRunDto): Live {
  return {
    status: run.status,
    error: run.error ? run.error : null,
    errorReason: run.errorReason ?? null,
    observing: run.observing === true,
    runId: run.runId ? run.runId : null,
  }
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
  // 沿用的旧图（keepImages）以附件 ID 给出，与本次新附的张数分开计。
  const images = run.imageIds ?? []
  return {
    key: run.runId ? `run:${run.runId}` : `live:${run.startedAtUnixMs}`,
    questionId: null,
    question: run.question,
    images,
    imageCount: (run.imageCount ?? 0) + images.length,
    localImages: [],
    steps,
    answer,
    answerId,
    answerInterrupted: answer !== null && (run.status === 'failed' || run.status === 'canceled'),
    rating: answerId === null ? null : resolveRating(answerId, null, ratings),
    issues,
    membershipOffer,
    live: liveStatus(run),
    startedAt: null,
  }
}

function hasRunningTool(turn: Turn): boolean {
  return turn.steps.some((block) => block.type === 'tool' && block.call.status === 'running')
}

/**
 * 已结束且已落库的执行：以历史为准，但历史若是执行中途读到的（还没有最终回答、工具仍在运行）而实时覆盖层更完整，
 * 就继续用覆盖层（带上历史里的用户消息 ID / 起始时间），直到结束后那次重读成功。失败 / 停止的状态与原因随轮次保留，
 * 失败重试与失败说明据此显示。
 */
function finishedTurn(group: HistoryTurn, run: AssistantRunDto, overrides: IssueOverrides, ratings: RatingOverrides): Turn {
  const history = intoTurn(group, ratings)
  const overlay = liveTurn(run, overrides, ratings)
  if ((history.answer === null && overlay.answer !== null) || hasRunningTool(history)) {
    return {
      ...overlay,
      questionId: history.questionId,
      images: history.images.length > 0 ? history.images : overlay.images,
      imageCount: Math.max(history.imageCount, overlay.imageCount),
      startedAt: history.startedAt,
    }
  }
  return run.status === 'failed' || run.status === 'canceled' ? { ...history, live: liveStatus(run) } : history
}

interface HistoryTurn {
  runId: string | null
  key: string
  startedAt: string
  questionId: string | null
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
    questionId: null,
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
    // 没有调用 ID 的历史条目用消息 ID 兜底，保证列表键与展开状态不重复。
    callId: message.toolCallId || `msg:${message.id}`,
    name: message.toolName ?? '',
    kind: message.toolKind ?? 'server',
    status: 'running',
    durationMs: message.durationMs ?? null,
    arguments: parsed !== undefined && argumentsSummary(parsed) !== null ? parsed : null,
  }
}

function completeTool(turn: HistoryTurn, message: AssistantMessageDto): void {
  const matches = (call: AssistantToolCallDto): boolean =>
    call.status === 'running' && (message.toolCallId ? call.callId === message.toolCallId : call.name === message.toolName)
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
      turn.questionId = message.id
      turn.question = message.content
      turn.images = message.images ?? []
      break
    case 'assistant':
      if (message.content.trim() !== '') {
        turn.blocks.push({
          type: 'text',
          text: message.content,
          message: { id: message.id, rating: message.rating ?? null, interrupted: message.interrupted === true },
        })
      }
      break
    case 'assistant_step':
      // 工具调用之前的中间说明文字：属于过程，不是最终回答，也不可评价。
      if (message.content.trim() !== '') turn.blocks.push({ type: 'text', text: message.content, step: true })
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
    questionId: turn.questionId,
    question: turn.question,
    images: turn.images,
    imageCount: turn.images.length,
    localImages: [],
    steps,
    answer,
    answerId: answerMessage?.id ?? null,
    answerInterrupted: answerMessage?.interrupted ?? false,
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
  sent: SentImages | null = null,
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
    if (!isAssistantRunActive(run.status)) turns.push(finishedTurn(group, run, overrides, ratings))
    else if (!already) turns.push(liveTurn(run, overrides, ratings))
  }
  live.forEach((run, index) => {
    if (!consumed[index]) turns.push(liveTurn(run, overrides, ratings))
  })
  placeIssues(turns, issues, overrides)
  attachSentImages(turns, sessionId, sent)
  return turns
}

/** 刚发出的提问的本地图片（`data:` URL）：执行期间提问气泡据此显示缩略图，不必等历史重载。 */
export interface SentImages {
  sessionId: string
  text: string
  images: readonly string[]
}

/** 只认刚发出的那条（最后一轮、文本一致、历史轮次的张数对得上）。 */
function attachSentImages(turns: Turn[], sessionId: string, sent: SentImages | null): void {
  const last = turns[turns.length - 1]
  if (sent === null || last === undefined || sent.images.length === 0) return
  if (sent.sessionId !== sessionId || sent.text !== last.question) return
  if (last.live === null && last.imageCount !== sent.images.length) return
  last.localImages = sent.images
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
      questionId: null,
      question: '',
      images: [],
      imageCount: 0,
      localImages: [],
      steps: [],
      answer: null,
      answerId: null,
      answerInterrupted: false,
      rating: null,
      issues: [issue],
      membershipOffer: false,
      live: null,
      startedAt: null,
    })
  }
}

// ───────────────────────── 工具调用 ─────────────────────────

/**
 * 工具调用在界面上的呈现状态。`interrupted`：所属执行已结束，工具却没有结果（连接中断等）；
 * `unknown`：已批准的写操作在执行中被停止，本机请求可能已生效，结果未知。
 */
export type ToolState = 'running' | 'succeeded' | 'failed' | 'interrupted' | 'unknown'

export function toolState(call: AssistantToolCallDto, runIsActive: boolean): ToolState {
  if (call.interrupted === true) return 'unknown'
  if (call.status === 'running') return runIsActive ? 'running' : 'interrupted'
  return call.status
}

/** 工具调用状态的文案键：待确认 / 未执行优先于调用状态本身。 */
export function toolStatusKey(confirmation: ConfirmView | null, state: ToolState): string {
  if (confirmation?.kind === 'pending') return 'assistantToolWaiting'
  if (confirmation?.kind === 'declined' || confirmation?.kind === 'expired') return 'assistantToolSkipped'
  switch (state) {
    case 'running':
      return 'assistantToolRunning'
    case 'succeeded':
      return 'assistantToolDone'
    case 'failed':
      return 'assistantToolFailed'
    case 'interrupted':
      return 'assistantToolInterrupted'
    case 'unknown':
      return 'assistantToolResultUnknown'
  }
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

/** 进行中执行里的待确认调用（按会话）。旁观执行的待确认在本机不可操作，默认不计。 */
function pendingToolCalls(state: AssistantStateDto, nowMs: number, includeObserving: boolean): { sessionId: string }[] {
  const found: { sessionId: string }[] = []
  for (const run of state.runs) {
    if (!isAssistantRunActive(run.status) || (run.observing === true && !includeObserving)) continue
    for (const item of run.items ?? []) {
      if (item.type === 'tool' && confirmView(item.call, true, nowMs)?.kind === 'pending') found.push({ sessionId: run.sessionId })
    }
  }
  return found
}

/**
 * 进行中执行里等待用户确认的调用数（入口角标据此提示）。`includeObserving` 为真时连旁观执行的待确认一起算
 * （倒计时是否需要走表用它判断）；角标与跳转提示不含旁观执行。
 */
export function pendingConfirmations(state: AssistantStateDto, nowMs: number, includeObserving = false): number {
  return pendingToolCalls(state, nowMs, includeObserving).length
}

/** 当前会话之外、有待确认工具的会话（头部提示据此给出跳转；旁观执行不计）。 */
export function pendingElsewhere(state: AssistantStateDto, current: string | null, nowMs: number): string | null {
  return pendingToolCalls(state, nowMs, false).find((call) => call.sessionId !== current)?.sessionId ?? null
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

/** 剩余提问次数 ≤ 此值时在输入区附近提示。 */
export const SESSION_REMAINING_HINT_MAX = 3

/** 单会话提问上限的界面状态。 */
export type SessionLimitState =
  /** 不限 / 新对话 / 剩余充足：界面零变化。 */
  | { kind: 'none' }
  /** 剩余 1..=3 次：显示弱化提示。 */
  | { kind: 'remaining'; count: number }
  /** 已达上限且没有进行中的执行：换提示卡，隐藏重新生成 / 编辑重发。 */
  | { kind: 'reached'; limit: number }

/**
 * 单会话提问上限状态。`limit` 为 `config.sessionLimit`（0 / 缺失 = 不限），`used` 为当前会话详情的 `questionsUsed`。
 * 只针对已存在的会话（`current` 非空）；有进行中的执行时不判定为已达上限（执行结束后详情重载再判）。
 */
export function sessionLimitState(
  config: Pick<AssistantConfigDto, 'sessionLimit'> | null | undefined,
  current: string | null,
  used: number,
  hasActiveRun: boolean,
): SessionLimitState {
  const limit = config?.sessionLimit ?? 0
  if (current === null || limit <= 0) return { kind: 'none' }
  const remaining = limit - used
  if (remaining <= 0) return hasActiveRun ? { kind: 'none' } : { kind: 'reached', limit }
  return remaining <= SESSION_REMAINING_HINT_MAX ? { kind: 'remaining', count: remaining } : { kind: 'none' }
}

/** 提问失败的归类（决定提示文案与是否需要重新取配置）。 */
export type SendFailure =
  | 'signedOut'
  | 'notOpen'
  /** 当日额度用完（`assistantQuotaExceeded`）。 */
  | 'quotaExceeded'
  /** 本会话提问数已达上限（`assistantSessionLimit`）。 */
  | 'sessionLimit'
  /** 每分钟限频（`rateLimited`），稍后可再试。 */
  | 'rateLimited'
  /** 该会话已有执行在进行。 */
  | 'busy'
  | 'unavailable'
  /** 参数被拒（过长等）。 */
  | 'rejected'
  | 'failed'

export function classifySendError(error: Pick<RpcErrorData, 'code' | 'reason'> | undefined): SendFailure {
  if (error?.reason === 'assistantQuotaExceeded') return 'quotaExceeded'
  if (error?.reason === 'assistantSessionLimit') return 'sessionLimit'
  if (error?.reason === 'rateLimited') return 'rateLimited'
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
  sessionLimit: 'assistantErrSessionLimit',
  rateLimited: 'assistantErrRateLimited',
  busy: 'assistantErrBusy',
  unavailable: 'assistantErrUnavailable',
  rejected: 'assistantErrRejected',
  failed: 'assistantErrFailed',
}

/** 其他动作（确认 / 评分 / 问题单 / 会话管理）失败的归类。 */
export type ActionFailure = 'failed' | 'confirmExpired'

/** 工具确认失败：确认窗口已过期（工具未执行）与其他失败分开提示。 */
export function classifyConfirmError(error: Pick<RpcErrorData, 'reason'> | undefined): ActionFailure {
  return error?.reason === 'assistantConfirmExpired' ? 'confirmExpired' : 'failed'
}

export const ACTION_FAILURE_KEY: Readonly<Record<ActionFailure, string>> = {
  failed: 'assistantActionFailed',
  confirmExpired: 'assistantErrConfirmExpired',
}

/** 执行失败原因（`AssistantRunDto.errorReason`）→ 文案键；缺省 / 未知 / `canceled` 一律按 `error`。 */
export function runErrorKey(reason: string | null | undefined): string {
  switch (reason) {
    case 'timeout':
      return 'assistantRunErrTimeout'
    case 'maxTurns':
      return 'assistantRunErrMaxTurns'
    case 'noAnswer':
      return 'assistantRunErrNoAnswer'
    case 'contextOverflow':
      return 'assistantRunErrContextOverflow'
    case 'imageUnsupported':
      return 'assistantRunErrImageUnsupported'
    case 'unavailable':
      return 'assistantRunErrUnavailable'
    case 'streamLost':
      return 'assistantRunErrStreamLost'
    case 'network':
      return 'assistantRunErrNetwork'
    default:
      return 'assistantRunErrError'
  }
}

/** 协议固定的预览动作（`assistantPreviewAction` + 首字母大写的 action 是文案键）。 */
const PREVIEW_ACTIONS: Readonly<Record<string, true | undefined>> = {
  create: true,
  pause: true,
  resume: true,
  delete: true,
  moveToQueue: true,
  edit: true,
  restart: true,
  changeUrl: true,
  recheck: true,
  reannounce: true,
  setSeedLimits: true,
  setFilePriorities: true,
  updateSettings: true,
  queueStart: true,
  queueStop: true,
  queueReorder: true,
  installComponent: true,
  setPluginEnabled: true,
  repair: true,
  measureRoutes: true,
}

/** 预览动作 → 文案键；协议里没有的动作返回 null，界面显示原文。 */
export function previewActionKey(action: string): string | null {
  return PREVIEW_ACTIONS[action] === true ? `assistantPreviewAction${action.charAt(0).toUpperCase()}${action.slice(1)}` : null
}

/** 提问输入的校验入参（`draft` 已去首尾空白）。 */
export interface SendCheck {
  gate: Gate
  stale: boolean
  sending: boolean
  hasActiveRun: boolean
  draft: string
  maxMessageChars: number
  /** 刚被限频：冷却期内暂不能提问。 */
  cooldown?: boolean
}

/** 与 GPUI `AssistantView::can_send` 同判定：门禁开放且未用尽额度、未断线、无进行中、不在冷却、非空、未超长。 */
export function canSend(check: SendCheck): boolean {
  if (check.gate.kind !== 'open' || check.gate.quotaExhausted) return false
  if (check.stale || check.sending || check.hasActiveRun || check.cooldown === true) return false
  if (check.draft === '') return false
  return check.maxMessageChars === 0 || charCount(check.draft) <= check.maxMessageChars
}

/** 重新生成 / 编辑重发 / 失败重试的入参（这些动作都会再提一问，受额度 / 上限 / 冷却门控）。 */
export interface ResendCheck {
  gate: Gate
  stale: boolean
  sending: boolean
  cooldown: boolean
  hasActiveRun: boolean
  /** 已有云端会话（新对话没有可重写的历史）。 */
  hasSession: boolean
  /** 本会话提问数已达上限。 */
  limitReached: boolean
}

/** 与 GPUI `AssistantView::can_resend` 同判定：门禁开放且额度未用尽、已连接、已有会话、无进行中、不在冷却、未达会话上限。 */
export function canResend(check: ResendCheck): boolean {
  if (check.gate.kind !== 'open' || check.gate.quotaExhausted) return false
  return !check.stale && !check.sending && !check.cooldown && !check.hasActiveRun && check.hasSession && !check.limitReached
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

/** 新标题去首尾空白后须有 1–`ASSISTANT_MAX_TITLE_CHARS` 个字符；合法返回修剪后的标题，否则 null。 */
export function validTitle(title: string): string | null {
  const trimmed = title.trim()
  const length = charCount(trimmed)
  return length >= 1 && length <= ASSISTANT_MAX_TITLE_CHARS ? trimmed : null
}

/** 重新生成 / 编辑重发成功后：历史里去掉被替换的用户消息及其之后的全部消息（云端不再返回它们）。 */
export function withoutReplaced(messages: readonly AssistantMessageDto[], fromId: string): readonly AssistantMessageDto[] {
  const from = messages.find((message) => message.id === fromId)
  return from === undefined ? messages : messages.filter((message) => message.seq < from.seq)
}

/** 失败轮次的一键重试方案。 */
export type RetryPlan =
  /** 已落库：替换这条提问重答（有附图时沿用原图）。 */
  | { kind: 'replace'; questionId: string; text: string; keepImages: boolean }
  /** 提问从未被云端接受（没有用户消息 ID、也没有执行 ID）：按原问题在同一会话里重发。 */
  | { kind: 'resend'; text: string; images: readonly string[] }

/**
 * 失败的轮次已拿到执行 ID、但历史里还没有它的用户消息（云端在返回执行流之前就保存了提问，只是历史还没读到，
 * 网络失败后常见）：提问其实已落库，不能原样重发（会重复提问、多占一次额度），要等历史读到后再走替换。
 */
export function awaitsHistory(turn: Turn): boolean {
  return turn.questionId === null && turn.live?.status === 'failed' && turn.live.runId !== null
}

/**
 * 失败轮次能否一键重试。只对最后一轮、执行失败的轮次给出方案：已落库的替换重答；执行从未被接受（没有执行 ID）
 * 才按原问题重发。已被接受却还没读到历史（`awaitsHistory`）、或未落库而附图又不在本地（沿用旧图的执行、页面刷新后）
 * 无法安全重发，返回 null。
 */
export function retryPlan(turn: Turn, isLast: boolean): RetryPlan | null {
  if (!isLast || turn.live?.status !== 'failed') return null
  const text = turn.question.trim()
  if (text === '') return null
  if (turn.questionId !== null) return { kind: 'replace', questionId: turn.questionId, text, keepImages: turn.imageCount > 0 }
  if (awaitsHistory(turn)) return null
  if (turn.imageCount === 0) return { kind: 'resend', text, images: [] }
  return turn.localImages.length === turn.imageCount ? { kind: 'resend', text, images: turn.localImages } : null
}

/** 点踩后可选说明的字符数上限（与云端 `MAX_RATING_COMMENT_CHARS` 一致）。 */
export const MAX_RATING_COMMENT_CHARS = 500

/** 评价说明去首尾空白后须有 1–`MAX_RATING_COMMENT_CHARS` 个字符；合法返回修剪后的文本，否则 null。 */
export function validRatingComment(comment: string): string | null {
  const trimmed = comment.trim()
  const length = charCount(trimmed)
  return length >= 1 && length <= MAX_RATING_COMMENT_CHARS ? trimmed : null
}

/** 本地标题过滤：大小写不敏感的子串匹配；空白搜索词不过滤。 */
export function filterSessions(items: readonly AssistantSessionDto[], query: string): AssistantSessionDto[] {
  const needle = query.trim().toLowerCase()
  return needle === '' ? [...items] : items.filter((session) => session.title.toLowerCase().includes(needle))
}

/**
 * 列表里显示哪些会话：搜索词已发给服务端并取回结果时按服务端结果全显示（命中可能来自消息内容，标题里没有）；
 * 输入还在防抖中（`typed` 与已应用的 `applied` 不同）则先按标题本地过滤，即时反馈。
 */
export function visibleSessions(items: readonly AssistantSessionDto[], applied: string, typed: string): AssistantSessionDto[] {
  return typed.trim() === applied.trim() ? [...items] : filterSessions(items, typed)
}

/** 键盘光标移动一步（不循环）；`current` 为当前下标，`length` 为可见条数；没有可见条目时为 null。 */
export function stepCursor(current: number | null, length: number, forward: boolean): number | null {
  if (length === 0) return null
  const last = length - 1
  if (current === null) return forward ? 0 : last
  return forward ? Math.min(current + 1, last) : Math.max(0, Math.min(current, last) - 1)
}

// ───────────────────────── 时钟偏差 ─────────────────────────

/**
 * agent 主机与本地的时钟偏差（本地 − agent，毫秒）：以收到投影的本地时刻对 `nowUnixMs`；
 * 旧 agent 缺省为 0，视为无偏差。
 */
export function clockSkewMs(state: AssistantStateDto, receivedAtMs: number): number {
  const agentNow = state.nowUnixMs ?? 0
  return agentNow > 0 ? receivedAtMs - agentNow : 0
}

/** 本地时刻换算到 agent 时间轴（与 `confirmDeadlineUnixMs` 同一时钟）。 */
export function agentNowMs(localNowMs: number, skewMs: number): number {
  return localNowMs - skewMs
}

// ───────────────────────── 复制对话 ─────────────────────────

/** Markdown 代码块：围栏比内容里最长的连续反引号多一个（至少三个），内容里的 ``` 不会提前闭合。 */
function fenced(body: string, lang: string): string {
  const longest = [...body.matchAll(/`+/g)].reduce((max, match) => Math.max(max, match[0].length), 0)
  const fence = '`'.repeat(Math.max(3, longest + 1))
  return `${fence}${lang}\n${body}\n${fence}`
}

function toolTranscript(call: AssistantToolCallDto, active: boolean, nowMs: number, t: (key: string) => string): string[] {
  const status = toolStatusKey(confirmView(call, active, nowMs), toolState(call, active))
  let head = `### ${t('assistantTranscriptTool')} \`${call.name}\` · ${t(status)}`
  if (call.kind === 'client') head += ` · ${t('assistantToolLocal')}`
  if (call.durationMs !== undefined && call.durationMs !== null) head += ` · ${formatDuration(call.durationMs)}`
  const parts = [head]
  if (call.arguments !== undefined && call.arguments !== null && argumentsSummary(call.arguments) !== null) {
    parts.push(`${t('assistantToolArguments')}:`, fenced(argumentsPretty(call.arguments), 'json'))
  }
  if (call.outputPreview && call.outputPreview.trim() !== '') {
    parts.push(`${t('assistantToolOutput')}:`, fenced(formatToolOutput(call.outputPreview), ''))
  }
  return parts
}

/**
 * 一轮对话的 Markdown 副本：提问、全部过程（中间文本、每次工具调用的状态 / 耗时 / 完整参数与结果）、
 * 最终回答、失败 / 停止说明与问题单草稿（GPUI `turn_transcript`）。`t` 按文案键取界面语言的标签。
 */
export function turnTranscript(turn: Turn, nowMs: number, t: (key: string) => string): string {
  const head = `## ${t('assistantTranscriptUser')}\n\n${turn.question.trim()}\n\n## ${t('assistantTranscriptAssistant')}\n`
  const parts: string[] = []
  const active = runActive(turn)
  for (const block of turn.steps) {
    if (block.type === 'text') parts.push(block.text.trim())
    else if (block.type === 'tool') parts.push(...toolTranscript(block.call, active, nowMs, t))
  }
  if (turn.answer !== null) {
    parts.push(turn.answer.trim())
    if (turn.answerInterrupted) parts.push(t('assistantAnswerInterrupted'))
  }
  if (turn.membershipOffer) parts.push(`### ${t('assistantMembershipTitle')}`, t('assistantMembershipBody'))
  if (turn.live?.status === 'failed') {
    parts.push(t('assistantRunFailed'), t(runErrorKey(turn.live.errorReason)))
    if (turn.live.error) parts.push(turn.live.error.trim())
  } else if (turn.live?.status === 'canceled') {
    parts.push(t('assistantStopped'))
  }
  for (const issue of turn.issues) parts.push(`### ${issue.title.trim()}`, issue.body.trim())
  return head + parts.filter((part) => part !== '').map((part) => `\n${part}\n`).join('')
}
