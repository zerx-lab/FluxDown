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
    expect(classifySendError({ ...error('unavailable'), reason: 'rateLimited' })).toBe('quotaExceeded')
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
