// GPUI `crates/assistant/src/model.rs` 单测的逐条移植（同一组场景，同一组断言）。

import { describe, expect, test } from 'bun:test'
import type {
  ApplicationErrorCode,
  AssistantConfigDto,
  AssistantIssueDto,
  AssistantMessageDto,
  AssistantRunDto,
  AssistantRunItemDto,
  AssistantRunStatus,
  AssistantSessionDto,
  AssistantStateDto,
  AssistantToolCallDto,
  JsonValue,
} from '../../lib/rpc/protocol'
import {
  ARGUMENT_SUMMARY_CHARS,
  appendSessions,
  argumentsPretty,
  argumentsSummary,
  buildTimeline,
  canSend,
  charCount,
  classifySendError,
  sessionLimitState,
  confirmView,
  currentKind,
  currentRun,
  currentSession,
  findIssue,
  finishedSessions,
  formatCountdown,
  formatDuration,
  formatToolOutput,
  gate,
  pendingConfirmations,
  pendingElsewhere,
  pendingRequestIds,
  remainingSeconds,
  sessionTitle,
  stepsOpen,
  toolCount,
  toolState,
  truncateChars,
  ACTION_FAILURE_KEY,
  SEND_FAILURE_KEY,
  agentNowMs,
  classifyConfirmError,
  clockSkewMs,
  previewActionKey,
  runErrorKey,
  toolStatusKey,
  turnTranscript,
  validTitle,
  validRatingComment,
  withoutReplaced,
  MAX_RATING_COMMENT_CHARS,
  canResend,
  filterSessions,
  retryPlan,
  awaitsHistory,
  stepCursor,
  visibleSessions,
} from './model'
import type { IssueOverrides, RatingOverrides, Turn } from './model'

const message = (id: string, run: string | null, seq: number, role: string, content: string): AssistantMessageDto => ({
  id,
  runId: run,
  seq,
  role,
  content,
  createdAt: `2026-11-27T01:00:${String(seq).padStart(2, '0')}Z`,
})

const toolMessage = (
  id: string,
  run: string,
  seq: number,
  role: string,
  callId: string,
  name: string,
  isError: boolean,
): AssistantMessageDto => ({
  ...message(id, run, seq, role, '{"a":1}'),
  toolCallId: callId,
  toolName: name,
  toolKind: 'client',
  isError,
  durationMs: role === 'tool_result' ? 420 : null,
})

const run = (session: string, runId: string | null, status: AssistantRunStatus, items: AssistantRunItemDto[]): AssistantRunDto => ({
  sessionId: session,
  runId,
  status,
  question: 'live question',
  startedAtUnixMs: 1_000,
  items,
})

const text = (value: string): AssistantRunItemDto => ({ type: 'text', text: value })

const pendingCall = (deadline: number | null): AssistantToolCallDto => ({
  callId: 'c1',
  name: 'settings_patch',
  kind: 'client',
  status: 'running',
  requestId: 'req-1',
  effect: 'write',
  confirmation: 'pending',
  confirmDeadlineUnixMs: deadline,
})

const readOnlyCall = (): AssistantToolCallDto => ({ callId: 'c0', name: 'net_measure', kind: 'client', status: 'running' })

const issue = (id: string, extra: Partial<AssistantIssueDto> = {}): AssistantIssueDto => ({
  id,
  kind: 'bug',
  title: '',
  body: '',
  status: '',
  createdAt: '',
  updatedAt: '',
  ...extra,
})

const none: IssueOverrides = new Map()
const noRatings: RatingOverrides = new Map()

const state = (runs: AssistantRunDto[], extra: Partial<AssistantStateDto> = {}): AssistantStateDto => ({ visible: true, runs, ...extra })

describe('buildTimeline', () => {
  test('history turns group by user message and attach tool results', () => {
    // 故意乱序：时间线按 seq 排序。
    const history = [
      message('m4', 'r1', 4, 'assistant', 'about 80 Mbps'),
      message('m1', 'r1', 1, 'user', 'how fast?'),
      toolMessage('m3', 'r1', 3, 'tool_result', 'c1', 'net_measure', false),
      toolMessage('m2', 'r1', 2, 'tool_call', 'c1', 'net_measure', false),
      message('m6', 'r2', 6, 'assistant', 'you are welcome'),
      message('m5', 'r2', 5, 'user', 'thanks'),
    ]
    const turns = buildTimeline('s1', history, [], [], none, noRatings)
    expect(turns).toHaveLength(2)
    expect(turns[0]?.question).toBe('how fast?')
    expect(turns[0]?.answer).toBe('about 80 Mbps')
    expect(toolCount(turns[0] as Turn)).toBe(1)
    const step = turns[0]?.steps[0]
    expect(step?.type).toBe('tool')
    if (step?.type === 'tool') {
      expect(step.call.status).toBe('succeeded')
      expect(step.call.durationMs).toBe(420)
      expect(step.call.outputPreview).toBeTruthy()
    }
    expect(turns[1]?.answer).toBe('you are welcome')
    expect(turns.every((turn) => turn.live === null)).toBe(true)
  })

  test('tool without result in history is interrupted', () => {
    const history = [message('m1', 'r1', 1, 'user', 'q'), toolMessage('m2', 'r1', 2, 'tool_call', 'c1', 'net_measure', false)]
    const turns = buildTimeline('s1', history, [], [], none, noRatings)
    const step = turns[0]?.steps[0]
    expect(step?.type).toBe('tool')
    if (step?.type === 'tool') expect(toolState(step.call, false)).toBe('interrupted')
    expect(turns[0]?.answer).toBeNull()
  })

  test('tool_call arguments are parsed only when meaningful', () => {
    const history = [
      message('m1', 'r1', 1, 'user', 'q'),
      { ...toolMessage('m2', 'r1', 2, 'tool_call', 'c1', 'a', false), content: '{"limit":3}' },
      { ...toolMessage('m3', 'r1', 3, 'tool_call', 'c2', 'b', false), content: '{}' },
      { ...toolMessage('m4', 'r1', 4, 'tool_call', 'c3', 'c', false), content: 'not json' },
    ]
    const calls = (buildTimeline('s1', history, [], [], none, noRatings)[0]?.steps ?? []).flatMap((block) => (block.type === 'tool' ? [block.call] : []))
    expect(calls.map((call) => call.arguments)).toEqual([{ limit: 3 }, null, null])
  })

  test('tool result without a matching call becomes its own step', () => {
    const history = [message('m1', 'r1', 1, 'user', 'q'), toolMessage('m2', 'r1', 2, 'tool_result', 'c9', 'net_measure', true)]
    const step = buildTimeline('s1', history, [], [], none, noRatings)[0]?.steps[0]
    expect(step?.type).toBe('tool')
    if (step?.type === 'tool') {
      expect(step.call.status).toBe('failed')
      expect(step.call.arguments).toBeNull()
    }
  })

  test('persisted finished run is not duplicated by the overlay', () => {
    const history = [message('m1', 'r1', 1, 'user', 'q'), message('m2', 'r1', 2, 'assistant', 'persisted answer')]
    const runs = [run('s1', 'r1', 'completed', [text('overlay answer')])]
    const turns = buildTimeline('s1', history, [], runs, none, noRatings)
    expect(turns).toHaveLength(1)
    expect(turns[0]?.answer).toBe('persisted answer')
    expect(turns[0]?.live).toBeNull()
  })

  test('active run replaces its partial history in place', () => {
    const history = [
      message('m0', 'r0', 1, 'user', 'old'),
      message('m1', 'r1', 2, 'user', 'q'),
      message('m2', 'r1', 3, 'assistant', 'partial'),
    ]
    const runs = [run('s1', 'r1', 'running', [text('streaming more text')])]
    const turns = buildTimeline('s1', history, [], runs, none, noRatings)
    expect(turns).toHaveLength(2)
    expect(turns[0]?.question).toBe('old')
    expect(turns[1]?.answer).toBe('streaming more text')
    expect(turns[1]?.live?.status).toBe('running')
  })

  test('unpersisted and other session runs', () => {
    const runs = [
      run('s1', null, 'starting', []),
      run('s1', 'rx', 'failed', [text('half')]),
      run('other', 'ry', 'running', []),
    ]
    const turns = buildTimeline('s1', [], [], runs, none, noRatings)
    expect(turns).toHaveLength(2)
    expect(turns[0]?.live?.status).toBe('starting')
    expect(turns[0]?.question).toBe('live question')
    expect(turns[1]?.live?.status).toBe('failed')
    expect(turns[1]?.answer).toBe('half')
  })

  test('live items split steps, answer and issues', () => {
    const runs = [
      run('s1', 'r1', 'running', [
        text('let me check'),
        { type: 'tool', call: pendingCall(null) },
        text('final'),
        { type: 'issueDraft', issue: issue('i1', { status: 'draft' }) },
      ]),
    ]
    const turns = buildTimeline('s1', [], [], runs, none, noRatings)
    expect(turns[0]?.steps).toHaveLength(2)
    expect(turns[0]?.answer).toBe('final')
    expect(turns[0]?.issues).toHaveLength(1)
  })

  test('history issues attach to the latest turn started before them', () => {
    const history = [
      message('m1', 'r1', 1, 'user', 'q1'),
      message('m2', 'r1', 2, 'assistant', 'a1'),
      message('m3', 'r2', 10, 'user', 'q2'),
      message('m4', 'r2', 11, 'assistant', 'a2'),
    ]
    const issues = [
      issue('early', { createdAt: '2026-11-27T01:00:05.250Z' }),
      issue('late', { createdAt: '2026-11-27T01:00:12Z' }),
      issue('before-all', { createdAt: '2026-11-27T00:59:00Z' }),
    ]
    const turns = buildTimeline('s1', history, issues, [], none, noRatings)
    const ids = (turn: Turn | undefined) => turn?.issues.map((entry) => entry.id)
    expect(ids(turns[0])).toEqual(['early', 'before-all'])
    expect(ids(turns[1])).toEqual(['late'])
  })

  test('issue placement by second ignores fraction digits', () => {
    const history = [message('m1', 'r1', 1, 'user', 'q1'), message('m2', 'r2', 10, 'user', 'q2')]
    const issues = [issue('mid', { createdAt: '2026-11-27T01:00:05.250Z' })]
    const turns = buildTimeline('s1', history, issues, [], none, noRatings)
    expect(turns[0]?.issues).toHaveLength(1)
    expect(turns[1]?.issues).toHaveLength(0)
  })

  test('issues already in a live run are not repeated and overrides win', () => {
    const draft = issue('i1', { status: 'draft' })
    const runs = [run('s1', 'r1', 'completed', [{ type: 'issueDraft', issue: draft }])]
    const overrides: IssueOverrides = new Map([['i1', { ...draft, status: 'submitted' }]])
    const turns = buildTimeline('s1', [], [draft], runs, overrides, noRatings)
    expect(turns).toHaveLength(1)
    expect(turns[0]?.issues).toHaveLength(1)
    expect(turns[0]?.issues[0]?.status).toBe('submitted')
  })

  test('issues without any turn get their own entry', () => {
    const turns = buildTimeline('s1', [], [issue('i1')], [], none, noRatings)
    expect(turns).toHaveLength(1)
    expect(turns[0]?.question).toBe('')
    expect(turns[0]?.issues).toHaveLength(1)
  })

  test('each answer carries its own message id and rating; local ratings win', () => {
    const history = [
      message('m1', 'r1', 1, 'user', 'q1'),
      { ...message('m2', 'r1', 2, 'assistant', 'a1'), rating: 'up' },
      message('m3', 'r2', 3, 'user', 'q2'),
      message('m4', 'r2', 4, 'assistant', 'a2'),
      toolMessage('m5', 'r3', 5, 'tool_call', 'c1', 'net_measure', false),
    ]
    const turns = buildTimeline('s1', history, [], [], none, noRatings)
    expect(turns.map((turn) => [turn.answerId, turn.rating])).toEqual([
      ['m2', 'up'],
      ['m4', null],
      [null, null],
    ])
    const ratings: RatingOverrides = new Map([
      ['m2', null],
      ['m4', 'down'],
    ])
    const rated = buildTimeline('s1', history, [], [], none, ratings)
    expect(rated.map((turn) => [turn.answerId, turn.rating])).toEqual([
      ['m2', null],
      ['m4', 'down'],
      [null, null],
    ])
  })

  test('only the last assistant text of a turn is rated; intermediate text is a step', () => {
    const history = [
      message('m1', 'r1', 1, 'user', 'q'),
      { ...message('m2', 'r1', 2, 'assistant', 'thinking'), rating: 'down' },
      toolMessage('m3', 'r1', 3, 'tool_call', 'c1', 'net_measure', false),
      toolMessage('m4', 'r1', 4, 'tool_result', 'c1', 'net_measure', false),
      { ...message('m5', 'r1', 5, 'assistant', 'final'), rating: 'up' },
    ]
    const [turn] = buildTimeline('s1', history, [], [], none, noRatings)
    expect(turn?.answer).toBe('final')
    expect(turn?.answerId).toBe('m5')
    expect(turn?.rating).toBe('up')
  })

  test('a finished live run is rated through its message id, an unfinished answer is not', () => {
    const done = { ...run('s1', 'r1', 'completed', [text('hi')]), messageId: 'mm1' }
    const ratings: RatingOverrides = new Map([['mm1', 'up']])
    expect(buildTimeline('s1', [], [], [done], none, noRatings)[0]).toMatchObject({ answerId: 'mm1', rating: null })
    expect(buildTimeline('s1', [], [], [done], none, ratings)[0]).toMatchObject({ answerId: 'mm1', rating: 'up' })
    // 没有消息 ID（尚未落库）或没有回答文本时无法评价。
    expect(buildTimeline('s1', [], [], [run('s1', 'r1', 'completed', [text('hi')])], none, noRatings)[0]?.answerId).toBeNull()
    expect(buildTimeline('s1', [], [], [{ ...done, items: [] }], none, noRatings)[0]?.answerId).toBeNull()
  })

  test('user images come from the history message', () => {
    const history = [
      { ...message('m1', 'r1', 1, 'user', 'see [Image #1]'), images: ['data:image/png;base64,AA=='] },
      message('m2', 'r1', 2, 'assistant', 'ok'),
    ]
    expect(buildTimeline('s1', history, [], [], none, noRatings)[0]?.images).toEqual(['data:image/png;base64,AA=='])
  })
})

const offerCall = (status: AssistantToolCallDto['status'], callId = 'offer-r1'): AssistantRunItemDto => ({
  type: 'tool',
  call: { callId, name: 'offer_membership', kind: 'server', status },
})

const offerMessage = (id: string, seq: number, role: string, callId: string, isError: boolean): AssistantMessageDto => ({
  ...toolMessage(id, 'r1', seq, role, callId, 'offer_membership', isError),
  toolKind: 'server',
})

describe('membership offer', () => {
  test('live: only a succeeded offer_membership tool shows the card; the tool never enters steps', () => {
    for (const [status, expected] of [
      ['succeeded', true],
      ['running', false],
      ['failed', false],
    ] as const) {
      const [turn] = buildTimeline('s1', [], [], [run('s1', 'r1', 'running', [offerCall(status)])], none, noRatings)
      expect(turn?.membershipOffer).toBe(expected)
      expect(turn?.steps).toEqual([])
      expect(toolCount(turn as Turn)).toBe(0)
    }
  })

  test('live: answer is judged after the offer tool is removed, other tools stay counted', () => {
    const items: AssistantRunItemDto[] = [
      { type: 'tool', call: readOnlyCall() },
      text('thinking'),
      offerCall('succeeded'),
      text('final'),
      offerCall('succeeded', 'offer-r1b'),
    ]
    const [turn] = buildTimeline('s1', [], [], [run('s1', 'r1', 'completed', items)], none, noRatings)
    // 末尾的 offer 工具被移除后，最后一个文本块才是回答。
    expect(turn?.answer).toBe('final')
    expect(turn?.membershipOffer).toBe(true)
    expect(turn?.steps).toHaveLength(2)
    expect(toolCount(turn as Turn)).toBe(1)
  })

  test('live: one succeeded offer among several calls still yields a single card flag', () => {
    const items: AssistantRunItemDto[] = [offerCall('failed', 'o1'), offerCall('succeeded', 'o2'), offerCall('succeeded', 'o3'), text('ok')]
    const [turn] = buildTimeline('s1', [], [], [run('s1', 'r1', 'completed', items)], none, noRatings)
    expect(turn?.membershipOffer).toBe(true)
    expect(turn?.steps).toEqual([])
    expect(turn?.answer).toBe('ok')
  })

  test('history: tool_call + tool_result success shows the card, error does not', () => {
    const build = (isError: boolean) =>
      buildTimeline(
        's1',
        [
          message('m1', 'r1', 1, 'user', 'upgrade?'),
          offerMessage('m2', 2, 'tool_call', 'offer-r1', false),
          offerMessage('m3', 3, 'tool_result', 'offer-r1', isError),
          message('m4', 'r1', 4, 'assistant', 'here you go'),
        ],
        [],
        [],
        none,
        noRatings,
      )[0]
    const ok = build(false)
    expect(ok?.membershipOffer).toBe(true)
    expect(ok?.steps).toEqual([])
    expect(ok?.answer).toBe('here you go')
    expect(ok?.answerId).toBe('m4')
    expect(toolCount(ok as Turn)).toBe(0)
    const failed = build(true)
    expect(failed?.membershipOffer).toBe(false)
    expect(failed?.steps).toEqual([])
    expect(failed?.answer).toBe('here you go')
  })

  test('history: a tool_call without result and other turns stay unaffected', () => {
    const history = [
      message('m1', 'r1', 1, 'user', 'a'),
      offerMessage('m2', 2, 'tool_call', 'offer-r1', false),
      message('m3', 'r1', 3, 'assistant', 'x'),
      message('m4', 'r2', 4, 'user', 'b'),
      message('m5', 'r2', 5, 'assistant', 'y'),
    ]
    const turns = buildTimeline('s1', history, [], [], none, noRatings)
    expect(turns.map((turn) => turn.membershipOffer)).toEqual([false, false])
  })

  test('an issue-only placeholder turn has no membership offer', () => {
    const turns = buildTimeline('s1', [], [issue('i1')], [], none, noRatings)
    expect(turns[0]?.membershipOffer).toBe(false)
  })
})

describe('confirmView', () => {
  test('confirmation states', () => {
    const now = 10_000
    const view = (call: AssistantToolCallDto, active: boolean) => confirmView(call, active, now)
    // 无确认字段 = 只读工具。
    expect(view(readOnlyCall(), true)).toBeNull()
    // 待确认：剩余时间 = 截止 - 现在。
    expect(view(pendingCall(now + 28_500), true)).toEqual({ kind: 'pending', remainingMs: 28_500 })
    expect(view(pendingCall(null), true)).toEqual({ kind: 'pending', remainingMs: null })
    // 截止时间已过（推送晚于本机时钟）→ 过期；恰好到点也算过期。
    expect(view(pendingCall(now), true)).toEqual({ kind: 'expired' })
    expect(view(pendingCall(now - 1), true)).toEqual({ kind: 'expired' })
    // 执行已结束仍待确认 → 不可再操作。
    expect(view(pendingCall(now + 5_000), false)).toEqual({ kind: 'expired' })
    // 已处理的状态原样返回，与截止时间无关。
    for (const [confirmation, kind] of [
      ['approved', 'approved'],
      ['declined', 'declined'],
      ['expired', 'expired'],
    ] as const) {
      const call: AssistantToolCallDto = { ...pendingCall(null), confirmation, confirmDeadlineUnixMs: now + 99_000 }
      expect(view(call, false)).toEqual({ kind })
    }
  })

  test('null confirmation is the same as a missing one', () => {
    expect(confirmView({ ...readOnlyCall(), confirmation: null }, true, 0)).toBeNull()
  })

  test('tool state falls back to interrupted once the run ended', () => {
    expect(toolState(readOnlyCall(), true)).toBe('running')
    expect(toolState(readOnlyCall(), false)).toBe('interrupted')
    expect(toolState({ ...readOnlyCall(), status: 'succeeded' }, false)).toBe('succeeded')
    expect(toolState({ ...readOnlyCall(), status: 'failed' }, true)).toBe('failed')
  })
})

describe('countdown', () => {
  test('rounds up', () => {
    expect(remainingSeconds(1)).toBe(1)
    expect(remainingSeconds(1_000)).toBe(1)
    expect(remainingSeconds(1_001)).toBe(2)
    expect(remainingSeconds(29_500)).toBe(30)
    expect(formatCountdown(0)).toBe('0 s')
    expect(formatCountdown(59)).toBe('59 s')
    expect(formatCountdown(60)).toBe('1:00')
    expect(formatCountdown(125)).toBe('2:05')
    expect(formatCountdown(-3)).toBe('0 s')
  })
})

describe('pending confirmations', () => {
  test('counts cover active runs only', () => {
    const now = 1_000
    const pending = (): AssistantRunItemDto => ({ type: 'tool', call: pendingCall(now + 5_000) })
    const snapshot = state([
      run('a', 'r1', 'running', [pending()]),
      run('b', 'r2', 'canceled', [pending()]),
      run('c', 'r3', 'running', [pending()]),
    ])
    expect(pendingConfirmations(snapshot, now)).toBe(2)
    expect(pendingElsewhere(snapshot, 'a', now)).toBe('c')
    expect(pendingElsewhere(snapshot, 'c', now)).toBe('a')
    expect(pendingElsewhere(snapshot, null, now)).toBe('a')
    expect(pendingConfirmations(snapshot, now + 6_000)).toBe(0)
    expect(pendingElsewhere(snapshot, 'a', now + 6_000)).toBeNull()
  })

  test('request ids ignore the deadline but only keep pending calls', () => {
    const snapshot = state([
      run('a', 'r1', 'running', [
        { type: 'tool', call: pendingCall(1) },
        { type: 'tool', call: { ...pendingCall(null), requestId: 'req-2', confirmation: 'approved' } },
        { type: 'tool', call: { ...pendingCall(null), requestId: null } },
      ]),
    ])
    expect([...pendingRequestIds(snapshot)]).toEqual(['req-1'])
  })
})

describe('stepsOpen', () => {
  test('defaults and overrides', () => {
    const now = 0
    const thinking = buildTimeline('s1', [], [], [run('s1', 'r1', 'running', [text('plan'), { type: 'tool', call: readOnlyCall() }])], none, noRatings)
    expect(stepsOpen(thinking[0] as Turn, undefined, now)).toBe(true)
    expect(stepsOpen(thinking[0] as Turn, false, now)).toBe(false)

    const answered = buildTimeline('s1', [], [], [run('s1', 'r1', 'running', [{ type: 'tool', call: readOnlyCall() }, text('answer')])], none, noRatings)
    expect(stepsOpen(answered[0] as Turn, undefined, now)).toBe(false)
    expect(stepsOpen(answered[0] as Turn, true, now)).toBe(true)

    // 待确认时强制展开，用户收起也无效。
    const confirming = buildTimeline('s1', [], [], [run('s1', 'r1', 'running', [{ type: 'tool', call: pendingCall(60_000) }, text('x')])], none, noRatings)
    expect(stepsOpen(confirming[0] as Turn, false, now)).toBe(true)
    // 过期后不再强制。
    expect(stepsOpen(confirming[0] as Turn, false, 61_000)).toBe(false)
  })
})

describe('gate', () => {
  const open: AssistantConfigDto = {
    available: true,
    maxMessageChars: 4000,
    imagesSupported: false,
    maxImages: 0,
    maxImageBytes: 0,
    dailyLimit: 5,
    usedToday: 4,
    toolsEnabled: true,
  }
  const withConfig = (config: AssistantConfigDto | null): AssistantStateDto => ({ config, visible: true, runs: [] })

  test('reflects account, plan, quota and errors', () => {
    expect(gate(false, withConfig(open), false)).toEqual({ kind: 'signedOut' })
    expect(gate(true, withConfig(null), false)).toEqual({ kind: 'loading' })
    expect(gate(true, withConfig(null), true)).toEqual({ kind: 'serviceError' })
    expect(gate(true, { visible: true, runs: [], lastErrorReason: 'cloudUnreachable' }, false)).toEqual({ kind: 'serviceError' })
    expect(gate(true, { visible: true, runs: [], lastErrorMessage: 'x' }, false)).toEqual({ kind: 'serviceError' })
    // 拉取进行中的上一次错误不算失败。
    expect(gate(true, { visible: true, runs: [], loading: true, lastErrorReason: 'cloudUnreachable' }, false)).toEqual({ kind: 'loading' })
    expect(gate(true, withConfig(open), false)).toEqual({ kind: 'open', quotaExhausted: false })
    expect(gate(true, withConfig({ ...open, usedToday: 5 }), false)).toEqual({ kind: 'open', quotaExhausted: true })
    // 0 = 不限额。
    expect(gate(true, withConfig({ ...open, dailyLimit: 0, usedToday: 999 }), false)).toEqual({ kind: 'open', quotaExhausted: false })
    expect(gate(true, withConfig({ ...open, available: false, reason: 'plan_disabled' }), false)).toEqual({ kind: 'planDisabled' })
    for (const reason of ['off', 'internal_only', null, undefined]) {
      expect(gate(true, withConfig({ ...open, available: false, reason }), false)).toEqual({ kind: 'notOpen' })
    }
  })

  test('a missing state field is the same as the empty state', () => {
    expect(gate(true, { visible: false, runs: [] }, false)).toEqual({ kind: 'loading' })
  })

  test('canSend follows the gate, connection, running state and length limit', () => {
    const base = {
      gate: { kind: 'open', quotaExhausted: false } as const,
      stale: false,
      sending: false,
      hasActiveRun: false,
      draft: 'hello',
      maxMessageChars: 5,
    }
    expect(canSend(base)).toBe(true)
    expect(canSend({ ...base, draft: '' })).toBe(false)
    expect(canSend({ ...base, draft: 'hello!' })).toBe(false)
    expect(canSend({ ...base, maxMessageChars: 0, draft: 'x'.repeat(10_000) })).toBe(true)
    // 超长按字符（码点）而不是 UTF-16 单元计。
    expect(canSend({ ...base, draft: '😀😀😀😀😀' })).toBe(true)
    expect(canSend({ ...base, stale: true })).toBe(false)
    expect(canSend({ ...base, sending: true })).toBe(false)
    expect(canSend({ ...base, hasActiveRun: true })).toBe(false)
    expect(canSend({ ...base, gate: { kind: 'open', quotaExhausted: true } })).toBe(false)
    expect(canSend({ ...base, gate: { kind: 'loading' } })).toBe(false)
  })
})

describe('sessionLimitState', () => {
  const limited = { sessionLimit: 10 }

  test('limit 0 or missing means unlimited', () => {
    expect(sessionLimitState({ sessionLimit: 0 }, 's', 999, false)).toEqual({ kind: 'none' })
    expect(sessionLimitState({}, 's', 999, false)).toEqual({ kind: 'none' })
    expect(sessionLimitState(undefined, 's', 999, false)).toEqual({ kind: 'none' })
    expect(sessionLimitState(null, 's', 999, false)).toEqual({ kind: 'none' })
  })

  test('a new chat is never limited', () => {
    expect(sessionLimitState(limited, null, 10, false)).toEqual({ kind: 'none' })
  })

  test('remaining hint shows for 3, 2 and 1 left only', () => {
    expect(sessionLimitState(limited, 's', 0, false)).toEqual({ kind: 'none' })
    expect(sessionLimitState(limited, 's', 6, false)).toEqual({ kind: 'none' })
    expect(sessionLimitState(limited, 's', 7, false)).toEqual({ kind: 'remaining', count: 3 })
    expect(sessionLimitState(limited, 's', 8, false)).toEqual({ kind: 'remaining', count: 2 })
    expect(sessionLimitState(limited, 's', 9, false)).toEqual({ kind: 'remaining', count: 1 })
    expect(sessionLimitState(limited, 's', 9, true)).toEqual({ kind: 'remaining', count: 1 })
  })

  test('reached at or above the limit unless a run is active', () => {
    expect(sessionLimitState(limited, 's', 10, false)).toEqual({ kind: 'reached', limit: 10 })
    expect(sessionLimitState(limited, 's', 12, false)).toEqual({ kind: 'reached', limit: 10 })
    expect(sessionLimitState(limited, 's', 10, true)).toEqual({ kind: 'none' })
  })
})

describe('classifySendError', () => {
  test('errors are classified', () => {
    const error = (code: ApplicationErrorCode) => ({ code })
    expect(classifySendError(error('unauthorized'))).toBe('signedOut')
    expect(classifySendError(error('unavailable'))).toBe('unavailable')
    expect(classifySendError(error('timeout'))).toBe('unavailable')
    expect(classifySendError(error('conflict'))).toBe('busy')
    expect(classifySendError(error('unsupported'))).toBe('notOpen')
    expect(classifySendError(error('invalidArgument'))).toBe('rejected')
    expect(classifySendError(error('internal'))).toBe('failed')
    expect(classifySendError({ ...error('unavailable'), reason: 'assistantQuotaExceeded' })).toBe('quotaExceeded')
    // 会话上限是独立类别：不归到额度 / 忙 / 通用失败。
    expect(classifySendError({ ...error('conflict'), reason: 'assistantSessionLimit' })).toBe('sessionLimit')
    expect(classifySendError({ ...error('internal'), reason: 'assistantSessionLimit' })).toBe('sessionLimit')
    expect(classifySendError({ ...error('unavailable'), reason: 'rateLimited' })).toBe('rateLimited')
    expect(classifySendError({ ...error('internal'), reason: 'cloudUnreachable' })).toBe('unavailable')
    expect(classifySendError({ ...error('internal'), reason: 'sessionExpired' })).toBe('signedOut')
    // 传输层之外的异常（没有错误数据）。
    expect(classifySendError(undefined)).toBe('failed')
  })
})

describe('argument summaries', () => {
  test('summary and pretty JSON', () => {
    expect(argumentsSummary(undefined)).toBeNull()
    expect(argumentsSummary(null)).toBeNull()
    expect(argumentsSummary({})).toBeNull()
    expect(argumentsSummary([])).toBeNull()
    expect(argumentsSummary('  ')).toBeNull()
    expect(argumentsSummary({ limit: 3 })).toBe('{"limit":3}')
    expect(argumentsSummary(0)).toBe('0')
    expect(argumentsSummary(false)).toBe('false')
    const long: JsonValue = { text: '界'.repeat(500) }
    const summary = argumentsSummary(long)
    expect(summary).not.toBeNull()
    expect(charCount(summary ?? '')).toBe(ARGUMENT_SUMMARY_CHARS + 1)
    expect(summary?.endsWith('…')).toBe(true)
    expect(argumentsPretty({ a: [1] }).includes('\n  "a"')).toBe(true)
  })

  test('tool output is prettified only when it is valid JSON', () => {
    expect(formatToolOutput('{"a":[1,2]}')).toBe('{\n  "a": [\n    1,\n    2\n  ]\n}')
    expect(formatToolOutput('  \n{"a":1}\n ')).toBe('{\n  "a": 1\n}')
    // 预览可能被截断成非法 JSON：原样（仅修剪）。
    expect(formatToolOutput('{"a":[1,2…')).toBe('{"a":[1,2…')
    expect(formatToolOutput('plain text\nline 2  ')).toBe('plain text\nline 2')
    expect(formatToolOutput('  \n ')).toBe('')
  })

  test('truncation counts characters, never splits a surrogate pair', () => {
    expect(truncateChars('abc', 3)).toBe('abc')
    expect(truncateChars('abcd', 3)).toBe('abc…')
    expect(truncateChars('😀😀😀', 2)).toBe('😀😀…')
    expect(truncateChars('', 0)).toBe('')
  })
})

describe('formatDuration', () => {
  test('labels', () => {
    expect(formatDuration(0)).toBe('0 ms')
    expect(formatDuration(850)).toBe('850 ms')
    expect(formatDuration(1_000)).toBe('1.0 s')
    expect(formatDuration(12_345)).toBe('12.3 s')
  })
})

describe('session helpers', () => {
  const session = (id: string, extra: Partial<AssistantSessionDto> = {}): AssistantSessionDto => ({
    id,
    channel: 'web',
    kind: 'ask',
    title: id,
    messageCount: 0,
    createdAt: '',
    updatedAt: '',
    ...extra,
  })

  test('current run is the latest active run of the session', () => {
    const snapshot = state([
      run('s1', 'r1', 'running', []),
      run('s1', 'r2', 'completed', []),
      { ...run('s1', 'r3', 'running', []), startedAtUnixMs: 3 },
      run('s2', 'r4', 'running', []),
    ])
    expect(currentRun(snapshot, 's1')?.runId).toBe('r3')
    expect(currentRun(snapshot, 's9')).toBeUndefined()
    expect(currentRun(snapshot, null)).toBeUndefined()
  })

  test('current session prefers the loaded detail and falls back to the list', () => {
    const items = [session('a', { title: 'from list' }), session('b')]
    expect(currentSession(null, null, items)).toBeUndefined()
    expect(currentSession('a', null, items)?.title).toBe('from list')
    expect(currentSession('a', session('a', { title: 'from detail' }), items)?.title).toBe('from detail')
    // 详情属于另一个会话时不采用。
    expect(currentSession('a', session('b', { title: 'other' }), items)?.title).toBe('from list')
    expect(currentSession('zzz', null, items)).toBeUndefined()
  })

  test('kind: new chats use the pending kind, saved sessions their own', () => {
    expect(currentKind(null, 'feedback', undefined)).toBe('feedback')
    expect(currentKind('a', 'feedback', session('a', { kind: 'ask' }))).toBe('ask')
    expect(currentKind('a', 'ask', session('a', { kind: 'feedback' }))).toBe('feedback')
    expect(currentKind('a', 'feedback', undefined)).toBe('ask')
  })

  test('title is trimmed and empty means untitled', () => {
    expect(sessionTitle('  hello  ')).toBe('hello')
    expect(sessionTitle('   ')).toBeNull()
    expect(sessionTitle('')).toBeNull()
  })

  test('finished sessions are runs that stopped being active', () => {
    const before = state([run('a', 'r1', 'running', []), run('b', 'r2', 'starting', []), run('c', 'r3', 'completed', [])])
    const after = state([run('a', 'r1', 'running', []), run('b', 'r2', 'completed', [])])
    expect(finishedSessions(before, after)).toEqual(['b'])
    // 被裁掉的进行中执行同样算结束。
    expect(finishedSessions(before, state([]))).toEqual(['a', 'b'])
    expect(finishedSessions(after, after)).toEqual([])
  })

  test('appending a page keeps order and drops duplicates', () => {
    const merged = appendSessions([session('a'), session('b')], [session('b', { title: 'dup' }), session('c')])
    expect(merged.map((entry) => entry.id)).toEqual(['a', 'b', 'c'])
    expect(merged[1]?.title).toBe('b')
  })

  test('issue lookup order: local override, live run, loaded history', () => {
    const live = issue('i1', { status: 'draft', title: 'live' })
    const snapshot = state([run('s1', 'r1', 'running', [{ type: 'issueDraft', issue: live }])])
    const history = [issue('i2', { title: 'history' })]
    const overrides: IssueOverrides = new Map([['i1', issue('i1', { title: 'local' })]])
    expect(findIssue('i1', overrides, snapshot, history)?.title).toBe('local')
    expect(findIssue('i1', none, snapshot, history)?.title).toBe('live')
    expect(findIssue('i2', none, snapshot, history)?.title).toBe('history')
    expect(findIssue('missing', none, snapshot, history)).toBeUndefined()
  })
})

describe('timeline: v31 additions', () => {
  test('assistant_step text stays in the process and never becomes the answer', () => {
    const history = [
      message('m1', 'r1', 1, 'user', 'q'),
      message('m2', 'r1', 2, 'assistant_step', 'let me check'),
      toolMessage('m3', 'r1', 3, 'tool_call', 'c1', 'net_measure', false),
      toolMessage('m4', 'r1', 4, 'tool_result', 'c1', 'net_measure', false),
      message('m5', 'r1', 5, 'assistant', 'done'),
    ]
    const [turn] = buildTimeline('s1', history, [], [], none, noRatings)
    expect(turn?.answer).toBe('done')
    expect(turn?.steps.map((block) => block.type)).toEqual(['text', 'tool'])
    expect(turn?.answerId).toBe('m5')

    const [onlyStep] = buildTimeline('s1', history.slice(0, 2), [], [], none, noRatings)
    expect(onlyStep?.answer).toBeNull()
    expect(onlyStep?.steps).toHaveLength(1)
  })

  test('history images are attachment ids and the question id is kept for resend', () => {
    const user = { ...message('m1', 'r1', 1, 'user', 'see'), images: ['att-1', 'att-2'] }
    const [turn] = buildTimeline('s1', [user, message('m2', 'r1', 2, 'assistant', 'ok')], [], [], none, noRatings)
    expect(turn?.questionId).toBe('m1')
    expect(turn?.images).toEqual(['att-1', 'att-2'])
    expect(turn?.imageCount).toBe(2)
  })

  test('an interrupted history answer is flagged', () => {
    const partial = { ...message('m2', 'r1', 2, 'assistant', 'partial'), interrupted: true }
    const [turn] = buildTimeline('s1', [message('m1', 'r1', 1, 'user', 'q'), partial], [], [], none, noRatings)
    expect(turn?.answer).toBe('partial')
    expect(turn?.answerInterrupted).toBe(true)
  })

  test('a live answer of a failed or canceled run is interrupted; a completed one is not', () => {
    const flags = (status: AssistantRunStatus) =>
      buildTimeline('s1', [], [], [run('s1', 'r1', status, [text('part')])], none, noRatings)[0]?.answerInterrupted
    expect(flags('failed')).toBe(true)
    expect(flags('canceled')).toBe(true)
    expect(flags('completed')).toBe(false)
    expect(flags('running')).toBe(false)
  })

  test('live turns carry the attached image count and the local thumbnails of the matching question', () => {
    const live = { ...run('s1', 'r1', 'running', []), imageCount: 2 }
    const sent = { sessionId: 's1', text: 'live question', images: ['data:image/png;base64,AA==', 'data:image/png;base64,BB=='] }
    const [turn] = buildTimeline('s1', [], [], [live], none, noRatings, sent)
    expect(turn?.imageCount).toBe(2)
    expect(turn?.localImages).toEqual(sent.images)
    // 文本不一致（不是刚发出的那条）时不挂本地图。
    const [other] = buildTimeline('s1', [], [], [live], none, noRatings, { ...sent, text: 'something else' })
    expect(other?.localImages).toEqual([])
  })

  test('a live persisted tool call without arguments is tolerated', () => {
    const call: AssistantToolCallDto = { callId: 'c9', name: 'net_measure', kind: 'client', status: 'succeeded', arguments: null }
    const [turn] = buildTimeline('s1', [], [], [run('s1', 'r1', 'completed', [{ type: 'tool', call }, text('ok')])], none, noRatings)
    expect(toolCount(turn as Turn)).toBe(1)
    expect(argumentsSummary(call.arguments)).toBeNull()
  })
})

describe('tool state: unknown result', () => {
  test('an approved write that was stopped mid-run has an unknown result', () => {
    const stopped: AssistantToolCallDto = { ...pendingCall(null), confirmation: 'approved', status: 'failed', interrupted: true }
    expect(toolState(stopped, false)).toBe('unknown')
    expect(toolState(stopped, true)).toBe('unknown')
    expect(toolStatusKey(confirmView(stopped, false, 0), toolState(stopped, false))).toBe('assistantToolResultUnknown')
  })

  test('status keys: pending / skipped win over the call state', () => {
    expect(toolStatusKey({ kind: 'pending', remainingMs: 5 }, 'running')).toBe('assistantToolWaiting')
    expect(toolStatusKey({ kind: 'declined' }, 'running')).toBe('assistantToolSkipped')
    expect(toolStatusKey({ kind: 'expired' }, 'interrupted')).toBe('assistantToolSkipped')
    expect(toolStatusKey(null, 'running')).toBe('assistantToolRunning')
    expect(toolStatusKey({ kind: 'approved' }, 'succeeded')).toBe('assistantToolDone')
    expect(toolStatusKey(null, 'failed')).toBe('assistantToolFailed')
    expect(toolStatusKey(null, 'interrupted')).toBe('assistantToolInterrupted')
  })
})

describe('error classification and keys', () => {
  test('confirm errors: an expired window is separate', () => {
    expect(classifyConfirmError({ reason: 'assistantConfirmExpired' })).toBe('confirmExpired')
    expect(classifyConfirmError({ reason: 'rateLimited' })).toBe('failed')
    expect(classifyConfirmError(undefined)).toBe('failed')
  })

  test('run error reasons map to their own keys; unknown and canceled fall back to error', () => {
    const reasons: [string, string][] = [
      ['timeout', 'assistantRunErrTimeout'],
      ['maxTurns', 'assistantRunErrMaxTurns'],
      ['noAnswer', 'assistantRunErrNoAnswer'],
      ['contextOverflow', 'assistantRunErrContextOverflow'],
      ['imageUnsupported', 'assistantRunErrImageUnsupported'],
      ['unavailable', 'assistantRunErrUnavailable'],
      ['streamLost', 'assistantRunErrStreamLost'],
      ['network', 'assistantRunErrNetwork'],
      ['error', 'assistantRunErrError'],
      ['canceled', 'assistantRunErrError'],
      ['somethingNew', 'assistantRunErrError'],
    ]
    for (const [reason, key] of reasons) expect(runErrorKey(reason)).toBe(key)
    expect(runErrorKey(null)).toBe('assistantRunErrError')
    expect(runErrorKey(undefined)).toBe('assistantRunErrError')
  })

  test('quota and rate limit have separate texts', () => {
    expect(SEND_FAILURE_KEY.quotaExceeded).toBe('assistantErrQuota')
    expect(SEND_FAILURE_KEY.sessionLimit).toBe('assistantErrSessionLimit')
    expect(SEND_FAILURE_KEY.rateLimited).toBe('assistantErrRateLimited')
    expect(ACTION_FAILURE_KEY.confirmExpired).toBe('assistantErrConfirmExpired')
    expect(ACTION_FAILURE_KEY.failed).toBe('assistantActionFailed')
  })

  test('preview actions: protocol names map to keys, anything else shows verbatim', () => {
    expect(previewActionKey('create')).toBe('assistantPreviewActionCreate')
    expect(previewActionKey('moveToQueue')).toBe('assistantPreviewActionMoveToQueue')
    expect(previewActionKey('setSeedLimits')).toBe('assistantPreviewActionSetSeedLimits')
    expect(previewActionKey('bogus')).toBeNull()
    expect(previewActionKey('toString')).toBeNull()
    expect(previewActionKey('')).toBeNull()
  })
})

describe('titles, resend history and clock skew', () => {
  test('a title needs 1-80 characters after trimming', () => {
    expect(validTitle('  hello  ')).toBe('hello')
    expect(validTitle('   ')).toBeNull()
    expect(validTitle('界'.repeat(80))).toBe('界'.repeat(80))
    expect(validTitle('界'.repeat(81))).toBeNull()
    // 按码点计数，不按 UTF-16 单元。
    expect(validTitle('😀'.repeat(80))).toBe('😀'.repeat(80))
  })

  test('a replaced message and everything after it leave the history', () => {
    const messages = [message('m1', 'r1', 1, 'user', 'a'), message('m2', 'r1', 2, 'assistant', 'b'), message('m3', 'r2', 3, 'user', 'c')]
    expect(withoutReplaced(messages, 'm3').map((item) => item.id)).toEqual(['m1', 'm2'])
    expect(withoutReplaced(messages, 'm1')).toEqual([])
    expect(withoutReplaced(messages, 'missing')).toBe(messages)
  })

  test('skew is local minus agent; an old agent or an idle projection means none', () => {
    expect(clockSkewMs(state([], { nowUnixMs: 10_000 }), 12_500)).toBe(2_500)
    expect(clockSkewMs(state([], { nowUnixMs: 10_000 }), 7_000)).toBe(-3_000)
    expect(clockSkewMs(state([], { nowUnixMs: 0 }), 12_500)).toBe(0)
    expect(clockSkewMs(state([]), 12_500)).toBe(0)
    expect(agentNowMs(12_600, 2_500)).toBe(10_100)
    // 本机时钟偏快 2.5 秒：agent 时间轴上的截止时间仍按 agent 时钟倒数。
    const call = pendingCall(20_000)
    expect(confirmView(call, true, agentNowMs(12_600, 2_500))).toEqual({ kind: 'pending', remainingMs: 9_900 })
  })
})

describe('turnTranscript', () => {
  const t = (key: string) => `[${key}]`

  test('contains the question, the whole process, the answer and the failure notes', () => {
    const call: AssistantToolCallDto = {
      ...readOnlyCall(),
      status: 'succeeded',
      durationMs: 1_500,
      arguments: { a: 1 },
      outputPreview: 'plain ``` result',
    }
    const failed = { ...run('s1', 'r1', 'failed', [text('thinking'), { type: 'tool', call }, text('partial')]), errorReason: 'timeout', error: 'boom' }
    const [turn] = buildTimeline('s1', [], [], [failed], none, noRatings)
    const out = turnTranscript(turn as Turn, 0, t)
    const has = (part: string) => out.includes(part)
    expect(out.startsWith('## [assistantTranscriptUser]\n\nlive question\n\n## [assistantTranscriptAssistant]\n')).toBe(true)
    expect(has('thinking')).toBe(true)
    expect(has('### [assistantTranscriptTool] `net_measure` · [assistantToolDone] · [assistantToolLocal] · 1.5 s')).toBe(true)
    expect(has('```json\n{\n  "a": 1\n}\n```')).toBe(true)
    // 内容里有 ``` 时围栏加长，不会提前闭合。
    expect(has('````\nplain ``` result\n````')).toBe(true)
    expect(has('partial')).toBe(true)
    expect(has('[assistantAnswerInterrupted]')).toBe(true)
    expect(has('[assistantRunFailed]')).toBe(true)
    expect(has('[assistantRunErrTimeout]')).toBe(true)
    expect(has('boom')).toBe(true)
  })

  test('a canceled run says it stopped; a clean answer has no notes', () => {
    const [canceled] = buildTimeline('s1', [], [], [run('s1', 'r1', 'canceled', [])], none, noRatings)
    expect(turnTranscript(canceled as Turn, 0, t).includes('[assistantStopped]')).toBe(true)
    const history = [message('m1', 'r1', 1, 'user', 'q'), message('m2', 'r1', 2, 'assistant', 'answer')]
    const [done] = buildTimeline('s1', history, [], [], none, noRatings)
    const out = turnTranscript(done as Turn, 0, t)
    expect(out.includes('answer')).toBe(true)
    expect(out.includes('[assistantRunFailed]')).toBe(false)
    expect(out.includes('[assistantAnswerInterrupted]')).toBe(false)
  })
})

describe('observing runs and kept images', () => {
  const observing = (extra: Partial<AssistantRunDto> = {}): AssistantRunDto => ({ ...run('s1', 'r1', 'running', []), observing: true, ...extra })

  test('imageIds and observing flow into the live turn; kept images are counted with new ones', () => {
    const [turn] = buildTimeline('s1', [], [], [observing({ imageIds: ['a1', 'a2'], imageCount: 1 })], none, noRatings)
    expect(turn?.live?.observing).toBe(true)
    expect(turn?.images).toEqual(['a1', 'a2'])
    expect(turn?.imageCount).toBe(3)
    const [plain] = buildTimeline('s1', [], [], [run('s1', 'r1', 'running', [])], none, noRatings)
    expect(plain?.live?.observing).toBe(false)
    expect(plain?.images).toEqual([])
  })

  test('a pending confirmation of an observed run is not counted for the badge or the jump hint', () => {
    const now = 1_000
    const pending = (): AssistantRunItemDto => ({ type: 'tool', call: pendingCall(now + 5_000) })
    const snapshot = state([observing({ items: [pending()] }), run('b', 'r2', 'running', [pending()])])
    expect(pendingConfirmations(snapshot, now)).toBe(1)
    // 倒计时仍要走表：算上旁观执行。
    expect(pendingConfirmations(snapshot, now, true)).toBe(2)
    expect(pendingElsewhere(state([observing({ items: [pending()] })]), null, now)).toBeNull()
    expect(pendingElsewhere(snapshot, 's1', now)).toBe('b')
  })
})

describe('retry plan for a failed turn', () => {
  const failed = (extra: Partial<AssistantRunDto> = {}): AssistantRunDto => ({ ...run('s1', 'r1', 'failed', []), errorReason: 'timeout', ...extra })

  test('a persisted question is replaced and keeps its images', () => {
    const history = [message('m1', 'r1', 1, 'user', 'how fast?')]
    const [turn] = buildTimeline('s1', history, [], [failed()], none, noRatings)
    expect(retryPlan(turn as Turn, true)).toEqual({ kind: 'replace', questionId: 'm1', text: 'how fast?', keepImages: false })
    const withImages = [{ ...message('m1', 'r1', 1, 'user', 'see'), images: ['att-1'] }]
    const [pictured] = buildTimeline('s1', withImages, [], [failed()], none, noRatings)
    expect(retryPlan(pictured as Turn, true)).toEqual({ kind: 'replace', questionId: 'm1', text: 'see', keepImages: true })
  })

  test('an unsaved question that was never accepted is resent as typed, with its local images when it had any', () => {
    const [plain] = buildTimeline('s1', [], [], [failed({ runId: null, question: '  hello  ' })], none, noRatings)
    expect(plain?.live?.runId).toBeNull()
    expect(awaitsHistory(plain as Turn)).toBe(false)
    expect(retryPlan(plain as Turn, true)).toEqual({ kind: 'resend', text: 'hello', images: [] })
    const sent = { sessionId: 's1', text: 'live question', images: ['data:image/png;base64,AA'] }
    const [pictured] = buildTimeline('s1', [], [], [failed({ runId: null, imageCount: 1 })], none, noRatings, sent)
    expect(retryPlan(pictured as Turn, true)).toEqual({ kind: 'resend', text: 'live question', images: ['data:image/png;base64,AA'] })
  })

  test('an unsaved question whose images are gone cannot be resent', () => {
    const [turn] = buildTimeline('s1', [], [], [failed({ runId: null, imageCount: 2 })], none, noRatings)
    expect(retryPlan(turn as Turn, true)).toBeNull()
  })

  test('an accepted run whose question is not in the history yet is never resent', () => {
    // 云端先保存用户消息再返回执行流：有执行 ID 说明提问已落库，只是历史还没读到；重发会重复提问并多占一次额度。
    const [turn] = buildTimeline('s1', [], [], [failed({ question: 'hello' })], none, noRatings)
    expect(turn?.questionId).toBeNull()
    expect(awaitsHistory(turn as Turn)).toBe(true)
    expect(retryPlan(turn as Turn, true)).toBeNull()
    // 带本地缩略图的刚发出的提问同样不重发。
    const sent = { sessionId: 's1', text: 'hello', images: ['data:image/png;base64,AA'] }
    const [pictured] = buildTimeline('s1', [], [], [failed({ question: 'hello', imageCount: 1 })], none, noRatings, sent)
    expect(retryPlan(pictured as Turn, true)).toBeNull()
    // 历史读到后才有替换方案。
    const history = [message('m1', 'r1', 1, 'user', 'hello')]
    const [loaded] = buildTimeline('s1', history, [], [failed({ question: 'hello' })], none, noRatings)
    expect(awaitsHistory(loaded as Turn)).toBe(false)
    expect(retryPlan(loaded as Turn, true)).toEqual({ kind: 'replace', questionId: 'm1', text: 'hello', keepImages: false })
    // 只有失败的轮次才算在等历史。
    const [running] = buildTimeline('s1', [], [], [run('s1', 'r1', 'running', [])], none, noRatings)
    expect(awaitsHistory(running as Turn)).toBe(false)
  })

  test('only the last failed turn offers a retry', () => {
    const history = [message('m1', 'r1', 1, 'user', 'q')]
    const [failedTurn] = buildTimeline('s1', history, [], [failed()], none, noRatings)
    expect(retryPlan(failedTurn as Turn, false)).toBeNull()
    const [canceled] = buildTimeline('s1', history, [], [{ ...failed(), status: 'canceled' }], none, noRatings)
    expect(retryPlan(canceled as Turn, true)).toBeNull()
    const [clean] = buildTimeline('s1', [...history, message('m2', 'r1', 2, 'assistant', 'ok')], [], [], none, noRatings)
    expect(retryPlan(clean as Turn, true)).toBeNull()
  })
})

describe('resend gating', () => {
  const open = { kind: 'open', quotaExhausted: false } as const
  const base = { gate: open, stale: false, sending: false, cooldown: false, hasActiveRun: false, hasSession: true, limitReached: false }

  test('regenerate, edit and retry need an open gate, a session and no blocker', () => {
    expect(canResend(base)).toBe(true)
    expect(canResend({ ...base, gate: { kind: 'open', quotaExhausted: true } })).toBe(false)
    expect(canResend({ ...base, gate: { kind: 'planDisabled' } })).toBe(false)
    for (const blocker of [{ stale: true }, { sending: true }, { cooldown: true }, { hasActiveRun: true }, { hasSession: false }, { limitReached: true }]) {
      expect(canResend({ ...base, ...blocker })).toBe(false)
    }
  })

  test('a cooldown also blocks a plain send', () => {
    const check = { gate: open, stale: false, sending: false, hasActiveRun: false, draft: 'hi', maxMessageChars: 10 }
    expect(canSend(check)).toBe(true)
    expect(canSend({ ...check, cooldown: true })).toBe(false)
  })
})

describe('rating comment', () => {
  test('1-500 characters after trimming, counted by code points', () => {
    expect(validRatingComment('  too slow  ')).toBe('too slow')
    expect(validRatingComment('   ')).toBeNull()
    expect(validRatingComment('界'.repeat(MAX_RATING_COMMENT_CHARS))).toBe('界'.repeat(MAX_RATING_COMMENT_CHARS))
    expect(validRatingComment('界'.repeat(MAX_RATING_COMMENT_CHARS + 1))).toBeNull()
    expect(validRatingComment('😀'.repeat(MAX_RATING_COMMENT_CHARS))).not.toBeNull()
  })
})

describe('history search while typing', () => {
  const sess = (id: string, title: string): AssistantSessionDto => ({
    id,
    channel: 'web',
    kind: 'ask',
    title,
    messageCount: 1,
    createdAt: '',
    updatedAt: '',
  })
  const items = [sess('a', 'Proxy setup'), sess('b', 'Speed limit'), sess('c', '代理设置')]

  test('the local filter matches titles case-insensitively and ignores blank queries', () => {
    expect(filterSessions(items, 'PROXY').map((item) => item.id)).toEqual(['a'])
    expect(filterSessions(items, '  设置 ').map((item) => item.id)).toEqual(['c'])
    expect(filterSessions(items, '   ')).toHaveLength(3)
    expect(filterSessions(items, 'nothing')).toEqual([])
  })

  test('the server result is shown whole once the typed query was applied', () => {
    // 命中可能来自消息内容，标题里没有：已应用的结果不再按标题过滤。
    expect(visibleSessions(items, 'speed', 'speed ')).toHaveLength(3)
    expect(visibleSessions(items, '', '')).toHaveLength(3)
  })

  test('while the typed query is still debouncing the list is filtered by title', () => {
    expect(visibleSessions(items, '', 'proxy').map((item) => item.id)).toEqual(['a'])
    expect(visibleSessions(items, 'proxy', 'proxy s').map((item) => item.id)).toEqual(['a'])
  })
})

describe('keyboard cursor', () => {
  test('moves one step without wrapping', () => {
    expect(stepCursor(null, 3, true)).toBe(0)
    expect(stepCursor(null, 3, false)).toBe(2)
    expect(stepCursor(0, 3, true)).toBe(1)
    expect(stepCursor(2, 3, true)).toBe(2)
    expect(stepCursor(2, 3, false)).toBe(1)
    expect(stepCursor(0, 3, false)).toBe(0)
  })

  test('an empty list has no cursor and a stale one is clamped', () => {
    expect(stepCursor(null, 0, true)).toBeNull()
    expect(stepCursor(4, 0, false)).toBeNull()
    expect(stepCursor(7, 3, false)).toBe(1)
    expect(stepCursor(7, 3, true)).toBe(2)
  })
})
