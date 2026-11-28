// 助手页的状态与动作（GPUI `crates/assistant/src/view.rs` 的移植）：不含 React，由 `useAssistant` 经
// `useSyncExternalStore` 订阅。异步结果用「代」（epoch）判定是否过期：会话列表 / 历史被刷新、会话切换或账号
// 变化之后，迟到的响应一律丢弃。

import { RpcError } from '../../lib/rpc/error'
import { EMPTY_ASSISTANT_STATE, isAssistantRunActive } from '../../lib/rpc/protocol'
import type {
  AssistantAskParams,
  AssistantAskResult,
  AssistantAttachmentParams,
  AssistantAttachmentResult,
  AssistantClearedSessions,
  AssistantConfirmParams,
  AssistantExportFormat,
  AssistantExportParams,
  AssistantExportResult,
  AssistantIssueDto,
  AssistantIssueParams,
  AssistantMessageDto,
  AssistantRateParams,
  AssistantRenameParams,
  AssistantRunDto,
  AssistantSessionDetail,
  AssistantSessionDto,
  AssistantSessionIdParams,
  AssistantSessionKind,
  AssistantSessionPage,
  AssistantSessionsParams,
  AssistantStateDto,
  OkResult,
  RpcErrorData,
} from '../../lib/rpc/protocol'
import {
  SESSION_PAGE_SIZE,
  appendSessions,
  classifyConfirmError,
  classifySendError,
  currentKind,
  currentRun,
  currentSession,
  finishedSessions,
  findIssue,
  gate,
  pendingRequestIds,
  validTitle,
  withoutReplaced,
} from './model'
import type { ActionFailure, SendFailure, SentImages } from './model'

/** 历史图片缓存的条数上限（每张是一份 data URL，超出按最早取用的先丢）。 */
const ATTACHMENT_CACHE_LIMIT = 64

/** 控制器依赖的 RPC 面（生产环境是 `rpc.agent.assistant`，测试注入替身）。 */
export interface AssistantPort {
  get(): Promise<AssistantStateDto>
  refresh(): Promise<AssistantStateDto>
  sessions(params?: AssistantSessionsParams): Promise<AssistantSessionPage>
  session(params: AssistantSessionIdParams): Promise<AssistantSessionDetail>
  deleteSession(params: AssistantSessionIdParams): Promise<OkResult>
  clearSessions(): Promise<AssistantClearedSessions>
  ask(params: AssistantAskParams): Promise<AssistantAskResult>
  cancel(params: AssistantSessionIdParams): Promise<OkResult>
  confirm(params: AssistantConfirmParams): Promise<OkResult>
  rate(params: AssistantRateParams): Promise<OkResult>
  renameSession(params: AssistantRenameParams): Promise<AssistantSessionDto>
  exportSession(params: AssistantExportParams): Promise<AssistantExportResult>
  attachment(params: AssistantAttachmentParams): Promise<AssistantAttachmentResult>
  confirmIssue(params: AssistantIssueParams): Promise<AssistantIssueDto>
  discardIssue(params: AssistantIssueParams): Promise<OkResult>
}

/** 页面外部的输入：快照里的助手状态、登录态、连接是否可用、界面语言。 */
export interface AssistantEnv {
  state: AssistantStateDto
  signedIn: boolean
  /** 与 agent 断开：内容保留只读，动作禁用。 */
  stale: boolean
  /** 提问时随新会话上报的界面语言（BCP 47）。 */
  locale: string
}

export type Load = 'idle' | 'loading' | 'ready' | 'failed'

/** 云端会话列表。 */
export interface SessionList {
  items: readonly AssistantSessionDto[]
  total: number
  load: Load
  moreLoading: boolean
  /** 当前搜索词（已去首尾空白；空 = 不搜索）。 */
  query: string
}

/** 当前会话在云端的历史。 */
export interface Detail {
  load: Load
  session: AssistantSessionDto | null
  messages: readonly AssistantMessageDto[]
  issues: readonly AssistantIssueDto[]
}

export interface ViewState {
  env: AssistantEnv
  /** 配置拉取失败（本机服务 / 云端不可达）。 */
  configFailed: boolean
  configLoading: boolean
  sessions: SessionList
  /** 当前会话；null = 新对话。 */
  current: string | null
  detail: Detail
  /** 新对话的类型（已有会话沿用其类型）。 */
  kind: AssistantSessionKind
  sending: boolean
  cancelling: boolean
  sendFailure: SendFailure | null
  /** 其他动作失败的提示（确认 / 评分 / 问题单 / 会话管理）；null = 无。 */
  actionFailure: ActionFailure | null
  /** 正在导出的会话 ID（避免重复点击）。 */
  exporting: string | null
  /** 刚发出的这条提问的本地图片：执行期间提问气泡据此显示缩略图。 */
  sent: SentImages | null
  /** 已发出、等待结果的确认请求（避免重复点击）。 */
  confirming: ReadonlySet<string>
  /** 用户对「过程」折叠区的显式选择（键 = 轮次键）。 */
  stepsChoice: ReadonlyMap<string, boolean>
  /** 已展开参数 JSON 的工具调用（键 = 调用 ID）。 */
  argsOpen: ReadonlySet<string>
  /** 本地确认 / 丢弃后的问题单最新状态。 */
  issueOverrides: ReadonlyMap<string, AssistantIssueDto>
  /** 本地刚提交的回答评价（键 = 消息 ID；null = 已清除）。 */
  ratingOverrides: ReadonlyMap<string, string | null>
  issueBusy: ReadonlySet<string>
  /** 正在编辑的问题单 ID（标题 / 正文草稿在问题单卡片自己的输入框里）。 */
  issueEditing: string | null
  ratingBusy: ReadonlySet<string>
  /** 等待二次确认的删除（会话 ID）。 */
  deletePending: string | null
  /** 等待二次确认的「清空全部会话」。 */
  clearPending: boolean
  /** 「清空全部会话」已发出、等待结果。 */
  clearing: boolean
  /** 每次需要强制滚到底部（切换会话、发送、首次载入历史）时加一。 */
  scrollToken: number
}

const EMPTY_SESSIONS: SessionList = { items: [], total: 0, load: 'idle', moreLoading: false, query: '' }
const EMPTY_DETAIL: Detail = { load: 'idle', session: null, messages: [], issues: [] }

function initialView(env: AssistantEnv): ViewState {
  return {
    env,
    configFailed: false,
    configLoading: false,
    sessions: EMPTY_SESSIONS,
    current: null,
    detail: EMPTY_DETAIL,
    kind: 'ask',
    sending: false,
    cancelling: false,
    sendFailure: null,
    actionFailure: null,
    exporting: null,
    sent: null,
    confirming: new Set(),
    stepsChoice: new Map(),
    argsOpen: new Set(),
    issueOverrides: new Map(),
    ratingOverrides: new Map(),
    issueBusy: new Set(),
    issueEditing: null,
    ratingBusy: new Set(),
    deletePending: null,
    clearPending: false,
    clearing: false,
    scrollToken: 0,
  }
}

export const INITIAL_ENV: AssistantEnv = { state: EMPTY_ASSISTANT_STATE, signedIn: false, stale: true, locale: 'en' }

function errorData(error: unknown): RpcErrorData | undefined {
  return error instanceof RpcError ? error.data : undefined
}

function withMember<T>(set: ReadonlySet<T>, value: T): Set<T> {
  return new Set(set).add(value)
}

function withoutMember<T>(set: ReadonlySet<T>, value: T): Set<T> {
  const next = new Set(set)
  next.delete(value)
  return next
}

function withEntry<K, V>(map: ReadonlyMap<K, V>, key: K, value: V): Map<K, V> {
  return new Map(map).set(key, value)
}

function withoutEntry<K, V>(map: ReadonlyMap<K, V>, key: K): Map<K, V> {
  const next = new Map(map)
  next.delete(key)
  return next
}

/** 提问的可选项：重新生成 / 编辑后重发（替换某条用户消息及其之后的全部消息）。 */
export interface SendOptions {
  replaceFrom?: string
  /** 沿用被替换提问的附图（此时 `images` 必须为空）。 */
  keepImages?: boolean
}

export class AssistantController {
  private view: ViewState
  private readonly listeners = new Set<() => void>()
  private readonly port: AssistantPort
  /** 账号变化时加一：之前发出的异步结果全部作废。 */
  private accountEpoch = 0
  private sessionsEpoch = 0
  private detailEpoch = 0
  /** 用户切换位置（新对话 / 选择 / 离开会话）时加一：发送在途时判断用户有没有走开。 */
  private navEpoch = 0
  private configRequested = false
  /** 历史图片（附件 ID -> data URL）；失败的请求不留，可重试。 */
  private readonly attachments = new Map<string, Promise<string>>()

  constructor(port: AssistantPort, env: AssistantEnv = INITIAL_ENV) {
    this.port = port
    this.view = initialView(env)
  }

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  readonly getSnapshot = (): ViewState => this.view

  private set(patch: Partial<ViewState>): void {
    this.view = { ...this.view, ...patch }
    for (const listener of this.listeners) listener()
  }

  private update(change: (view: ViewState) => Partial<ViewState>): void {
    this.set(change(this.view))
  }

  // ───────────────────────── 外部输入 ─────────────────────────

  /** 同步快照里的助手状态 / 登录态 / 连接状态 / 语言（每次渲染前后都可重复调用，无变化时不通知）。 */
  sync(env: AssistantEnv): void {
    const previous = this.view.env
    if (
      previous.state === env.state &&
      previous.signedIn === env.signedIn &&
      previous.stale === env.stale &&
      previous.locale === env.locale
    ) {
      return
    }
    // 登录态变化：账号相关的缓存（会话、历史、配置）一律作废。
    if (previous.signedIn !== env.signedIn) {
      this.accountEpoch += 1
      this.sessionsEpoch += 1
      this.detailEpoch += 1
      this.navEpoch += 1
      this.configRequested = false
      this.attachments.clear()
      this.view = { ...initialView(env), kind: this.view.kind, scrollToken: this.view.scrollToken + 1 }
    }

    // 整体替换助手状态后的联动（GPUI `apply_state`）：收敛确认中的请求。
    const patch: Partial<ViewState> = { env }
    const stateChanged = previous.state !== env.state
    if (stateChanged) {
      const pending = pendingRequestIds(env.state)
      const remaining = [...this.view.confirming].filter((id) => pending.has(id))
      if (remaining.length !== this.view.confirming.size) patch.confirming = new Set(remaining)
      if (env.state.config) patch.configFailed = false
    }
    this.set(patch)

    // 有执行结束：刷新云端历史与会话列表（已落库的历史取代实时覆盖层，列表排序 / 标题更新）。
    if (!stateChanged || !env.signedIn) return
    const finished = finishedSessions(previous.state, env.state)
    if (finished.length === 0) return
    this.reloadSessions()
    const current = this.view.current
    if (current !== null && finished.includes(current)) this.loadDetail(current)
  }

  // ───────────────────────── 派生 ─────────────────────────

  /** 当前会话里进行中的执行。 */
  activeRun(): AssistantRunDto | undefined {
    return currentRun(this.view.env.state, this.view.current)
  }

  // ───────────────────────── 配置 ─────────────────────────

  /** 页面可见且已登录时：首次拉取配置；门禁开放后拉取会话列表（幂等）。 */
  ensureLoaded(): void {
    const { env } = this.view
    if (!env.signedIn || env.stale) return
    if (!this.configRequested) {
      this.configRequested = true
      this.fetchConfig(false)
    }
    const view = this.view
    if (gate(env.signedIn, env.state, view.configFailed).kind === 'open' && view.sessions.load === 'idle') {
      this.reloadSessions()
    }
  }

  fetchConfig(refresh: boolean): void {
    if (this.view.configLoading) return
    const account = this.accountEpoch
    this.set({ configLoading: true })
    const request = refresh ? this.port.refresh() : this.port.get()
    request.then(
      () => {
        // 状态本体经 `assistantChanged` 推送进入快照；响应只用来判断成败。
        if (account !== this.accountEpoch) return
        this.set({ configLoading: false, configFailed: false })
      },
      () => {
        if (account !== this.accountEpoch) return
        this.set({ configLoading: false, configFailed: true })
      },
    )
  }

  // ───────────────────────── 会话列表 ─────────────────────────

  /** 重新读取会话列表第一页（已加载更多页时保持原有条数）。 */
  reloadSessions(): void {
    this.loadSessions(Math.max(SESSION_PAGE_SIZE, this.view.sessions.items.length))
  }

  /** 搜索会话（标题 / 消息内容）；空白清除搜索。结果从第一页重新开始。 */
  setQuery(query: string): void {
    const next = query.trim()
    if (this.view.sessions.query === next) return
    this.update(() => ({ sessions: { items: [], total: 0, load: 'loading', moreLoading: false, query: next } }))
    if (this.view.env.signedIn) this.loadSessions(SESSION_PAGE_SIZE)
  }

  private loadSessions(limit: number): void {
    if (!this.view.env.signedIn) return
    this.sessionsEpoch += 1
    const epoch = this.sessionsEpoch
    const account = this.accountEpoch
    const query = this.view.sessions.query
    this.update((view) => ({ sessions: { ...view.sessions, load: 'loading' } }))
    this.port.sessions({ limit, offset: 0, query: query === '' ? null : query }).then(
      (page) => {
        if (account !== this.accountEpoch || epoch !== this.sessionsEpoch) return
        this.update((view) => ({ sessions: { items: page.items, total: page.total, load: 'ready', moreLoading: false, query: view.sessions.query } }))
      },
      () => {
        if (account !== this.accountEpoch || epoch !== this.sessionsEpoch) return
        this.update((view) => ({ sessions: { ...view.sessions, load: 'failed', moreLoading: false } }))
      },
    )
  }

  loadMoreSessions(): void {
    const { sessions } = this.view
    if (sessions.moreLoading || sessions.items.length >= sessions.total) return
    const epoch = this.sessionsEpoch
    const account = this.accountEpoch
    this.update((view) => ({ sessions: { ...view.sessions, moreLoading: true } }))
    this.port.sessions({ limit: SESSION_PAGE_SIZE, offset: sessions.items.length, query: sessions.query === '' ? null : sessions.query }).then(
      (page) => {
        if (account !== this.accountEpoch) return
        // 期间列表被整体刷新：这一页的偏移已失效。
        if (epoch !== this.sessionsEpoch) {
          this.update((view) => ({ sessions: { ...view.sessions, moreLoading: false } }))
          return
        }
        this.update((view) => ({
          sessions: { ...view.sessions, items: appendSessions(view.sessions.items, page.items), total: page.total, moreLoading: false },
        }))
      },
      () => {
        if (account !== this.accountEpoch) return
        this.update((view) => ({ sessions: { ...view.sessions, moreLoading: false }, actionFailure: 'failed' }))
      },
    )
  }

  // ───────────────────────── 当前会话 ─────────────────────────

  newChat(): void {
    this.detailEpoch += 1
    this.navEpoch += 1
    this.set({
      current: null,
      detail: EMPTY_DETAIL,
      sent: null,
      sendFailure: null,
      actionFailure: null,
      deletePending: null,
      clearPending: false,
      issueEditing: null,
    })
  }

  selectSession(sessionId: string): void {
    if (this.view.current === sessionId) return
    this.detailEpoch += 1
    this.navEpoch += 1
    this.update((view) => ({
      current: sessionId,
      detail: EMPTY_DETAIL,
      sent: view.sent?.sessionId === sessionId ? view.sent : null,
      sendFailure: null,
      actionFailure: null,
      deletePending: null,
      clearPending: false,
      issueEditing: null,
      issueOverrides: new Map(),
      scrollToken: view.scrollToken + 1,
    }))
    this.loadDetail(sessionId)
  }

  loadDetail(sessionId: string): void {
    this.detailEpoch += 1
    const epoch = this.detailEpoch
    const account = this.accountEpoch
    this.update((view) => ({ detail: { ...view.detail, load: 'loading' } }))
    this.port.session({ sessionId }).then(
      (detail) => {
        if (account !== this.accountEpoch || epoch !== this.detailEpoch || this.view.current !== sessionId) return
        const first = this.view.detail.messages.length === 0
        this.update((view) => ({
          detail: { load: 'ready', session: detail.session, messages: detail.messages, issues: detail.issues ?? [] },
          // 新读到的历史里问题单状态以云端为准。
          issueOverrides: new Map(),
          scrollToken: first ? view.scrollToken + 1 : view.scrollToken,
        }))
      },
      () => {
        if (account !== this.accountEpoch || epoch !== this.detailEpoch || this.view.current !== sessionId) return
        this.update((view) => ({ detail: { ...view.detail, load: 'failed' } }))
      },
    )
  }

  requestDelete(sessionId: string): void {
    this.set({ deletePending: sessionId, clearPending: false })
  }

  /** 只在待确认的正是该会话时取消（鼠标离开别的行不影响当前待确认项）。 */
  cancelDelete(sessionId: string): void {
    if (this.view.deletePending === sessionId) this.set({ deletePending: null })
  }

  deleteSession(sessionId: string): void {
    if (this.view.env.stale) return
    const account = this.accountEpoch
    this.set({ deletePending: null })
    this.port.deleteSession({ sessionId }).then(
      () => {
        if (account !== this.accountEpoch) return
        const view = this.view
        const items = view.sessions.items.filter((session) => session.id !== sessionId)
        const removed = items.length !== view.sessions.items.length
        const sessions = { ...view.sessions, items, total: removed ? Math.max(0, view.sessions.total - 1) : view.sessions.total }
        if (view.current !== sessionId) {
          this.set({ sessions })
          return
        }
        this.set({ sessions, ...this.leaveCurrent() })
      },
      () => this.failAction(account),
    )
  }

  /** 修改会话标题（去首尾空白后 1–80 字符）；成功返回 true，列表与当前会话详情同步更新。 */
  async renameSession(sessionId: string, title: string): Promise<boolean> {
    const valid = validTitle(title)
    if (valid === null || this.view.env.stale) return false
    const account = this.accountEpoch
    this.set({ actionFailure: null })
    try {
      const updated = await this.port.renameSession({ sessionId, title: valid })
      if (account !== this.accountEpoch) return false
      this.update((view) => ({
        sessions: { ...view.sessions, items: view.sessions.items.map((session) => (session.id === sessionId ? updated : session)) },
        detail: view.detail.session?.id === sessionId ? { ...view.detail, session: updated } : view.detail,
      }))
      return true
    } catch {
      this.failAction(account)
      return false
    }
  }

  /** 导出会话（云端生成文件内容）；成功返回结果交给调用方保存，失败返回 null 并提示。 */
  async exportSession(sessionId: string, format: AssistantExportFormat): Promise<AssistantExportResult | null> {
    if (this.view.env.stale || this.view.exporting !== null) return null
    const account = this.accountEpoch
    this.set({ exporting: sessionId, actionFailure: null })
    try {
      const result = await this.port.exportSession({ sessionId, format })
      if (account !== this.accountEpoch) return null
      this.set({ exporting: null })
      return result
    } catch {
      if (account !== this.accountEpoch) return null
      this.set({ exporting: null, actionFailure: 'failed' })
      return null
    }
  }

  /** 历史消息里的一张图片（附件 ID -> `data:` URL）：按 ID 缓存，失败不缓存（可重试）。 */
  attachment(attachmentId: string): Promise<string> {
    const cached = this.attachments.get(attachmentId)
    if (cached !== undefined) return cached
    const request = this.port.attachment({ attachmentId }).then((result) => result.dataUrl)
    this.attachments.set(attachmentId, request)
    if (this.attachments.size > ATTACHMENT_CACHE_LIMIT) {
      const oldest = this.attachments.keys().next()
      if (!oldest.done) this.attachments.delete(oldest.value)
    }
    request.catch(() => {
      if (this.attachments.get(attachmentId) === request) this.attachments.delete(attachmentId)
    })
    return request
  }

  requestClear(): void {
    this.set({ clearPending: true, deletePending: null })
  }

  cancelClear(): void {
    if (this.view.clearPending) this.set({ clearPending: false })
  }

  /** 清空本人全部会话（云端只在用户侧隐藏）：成功后列表清空、回到新对话；在途的列表请求结果作废。 */
  clearSessions(): void {
    if (this.view.env.stale || this.view.clearing) return
    const account = this.accountEpoch
    this.set({ clearPending: false, deletePending: null, clearing: true, actionFailure: null })
    this.port.clearSessions().then(
      () => {
        if (account !== this.accountEpoch) return
        this.sessionsEpoch += 1
        const view = this.view
        this.set({
          clearing: false,
          sessions: { items: [], total: 0, load: 'ready', moreLoading: false, query: view.sessions.query },
          ...(view.current !== null ? this.leaveCurrent() : {}),
        })
      },
      () => {
        if (account !== this.accountEpoch) return
        this.set({ clearing: false, actionFailure: 'failed' })
      },
    )
  }

  /** 当前会话已不存在（被删 / 清空）：回到新对话。 */
  private leaveCurrent(): Partial<ViewState> {
    this.detailEpoch += 1
    this.navEpoch += 1
    return { current: null, detail: EMPTY_DETAIL, sent: null, issueEditing: null }
  }

  private failAction(account: number, failure: ActionFailure = 'failed'): void {
    if (account !== this.accountEpoch) return
    this.set({ actionFailure: failure })
  }

  /** 关掉提问 / 动作失败的提示（切换视图时调用）。 */
  clearFailure(): void {
    if (this.view.sendFailure !== null || this.view.actionFailure !== null) this.set({ sendFailure: null, actionFailure: null })
  }

  // ───────────────────────── 提问 / 停止 ─────────────────────────

  /**
   * 提问；成功返回 true（调用方清空输入）。`text` 已去首尾空白，`images` 为 `data:` URL。
   * 可提问性由调用方按 `canSend` 判定，这里只防重入与断线。`options.replaceFrom` 为重新生成 / 编辑后重发。
   * 发送期间用户可能切到别的会话 / 新对话：回包时只有仍停在发送时的位置才跟进（新会话才接管当前会话，
   * 重发才截掉被替换的历史），否则只刷新会话列表，不覆盖用户的切换。
   */
  async send(text: string, images: readonly string[], options: SendOptions = {}): Promise<boolean> {
    const view = this.view
    if (view.env.stale || view.sending || currentRun(view.env.state, view.current) !== undefined) return false
    const account = this.accountEpoch
    const origin = view.current
    const navigation = this.navEpoch
    const kind = currentKind(origin, view.kind, currentSession(origin, view.detail.session, view.sessions.items))
    this.set({ sending: true, sendFailure: null, actionFailure: null })
    try {
      const answer = await this.port.ask({
        sessionId: origin,
        kind,
        channel: 'web',
        text,
        images: [...images],
        locale: view.env.locale,
        ...(options.replaceFrom ? { replaceFrom: options.replaceFrom, keepImages: options.keepImages === true } : {}),
      })
      if (account !== this.accountEpoch) return false
      const stayed = navigation === this.navEpoch
      const created = stayed && origin === null
      if (created) {
        // 新会话在云端还没有任何已落库的历史：直接以空历史起步，提问内容由推送的实时执行显示。
        this.detailEpoch += 1
      }
      this.update((now) => ({
        sending: false,
        sent: images.length > 0 ? { sessionId: answer.sessionId, text, images: [...images] } : null,
        scrollToken: stayed ? now.scrollToken + 1 : now.scrollToken,
        ...(created ? { current: answer.sessionId, detail: { ...EMPTY_DETAIL, load: 'ready' as const } } : {}),
        ...(stayed && !created && options.replaceFrom
          ? { detail: { ...now.detail, messages: withoutReplaced(now.detail.messages, options.replaceFrom) } }
          : {}),
      }))
      this.reloadSessions()
      return true
    } catch (error) {
      if (account !== this.accountEpoch) return false
      const failure = classifySendError(errorData(error))
      this.set({ sending: false, sendFailure: failure })
      // 额度 / 开放状态可能已变：重新取配置让页面门禁跟上（限频只是稍后再试，不用）。
      if (failure === 'quotaExceeded' || failure === 'notOpen') this.fetchConfig(true)
      return false
    }
  }

  /** 重新生成：用原提问文本替换这条用户消息及其之后的回答；原提问有附图时沿用。 */
  regenerate(questionId: string, text: string, keepImages: boolean): Promise<boolean> {
    return this.send(text, [], { replaceFrom: questionId, keepImages })
  }

  stop(): void {
    const run = this.activeRun()
    if (run === undefined || this.view.cancelling) return
    const account = this.accountEpoch
    this.set({ cancelling: true })
    this.port.cancel({ sessionId: run.sessionId }).then(
      () => {
        if (account !== this.accountEpoch) return
        this.set({ cancelling: false })
      },
      () => {
        if (account !== this.accountEpoch) return
        this.set({ cancelling: false, actionFailure: 'failed' })
      },
    )
  }

  /** 新对话选择提问 / 反馈（已有会话沿用其类型）。 */
  setKind(kind: AssistantSessionKind): void {
    if (this.view.current === null && this.view.kind !== kind) this.set({ kind })
  }

  // ───────────────────────── 工具确认 ─────────────────────────

  confirmTool(requestId: string, approved: boolean): void {
    const view = this.view
    if (view.env.stale || view.confirming.has(requestId)) return
    const account = this.accountEpoch
    this.set({ confirming: withMember(view.confirming, requestId), actionFailure: null })
    this.port.confirm({ requestId, approved }).then(
      () => undefined,
      (error) => {
        if (account !== this.accountEpoch) return
        // 成功后状态由推送改写；失败则放开按钮让用户重试（确认窗口已过期时单独提示：工具没有执行）。
        this.update((now) => ({ confirming: withoutMember(now.confirming, requestId), actionFailure: classifyConfirmError(errorData(error)) }))
      },
    )
  }

  toggleSteps(key: string, currentlyOpen: boolean): void {
    this.update((view) => ({ stepsChoice: withEntry(view.stepsChoice, key, !currentlyOpen) }))
  }

  toggleArguments(key: string): void {
    this.update((view) => ({ argsOpen: view.argsOpen.has(key) ? withoutMember(view.argsOpen, key) : withMember(view.argsOpen, key) }))
  }

  // ───────────────────────── 评分 ─────────────────────────

  /**
   * 评价一条回答：再次点击同一个评价则清除。`current` 为该回答当前显示的评价。
   * 乐观更新：先显示新评价，失败时回滚到点击前的状态。
   */
  rate(messageId: string, current: string | null, rating: 'up' | 'down'): void {
    const view = this.view
    const sessionId = view.current
    if (sessionId === null || view.env.stale || view.ratingBusy.has(messageId)) return
    const next = current === rating ? null : rating
    const account = this.accountEpoch
    const hadOverride = view.ratingOverrides.has(messageId)
    const previous = view.ratingOverrides.get(messageId) ?? null
    this.set({
      ratingBusy: withMember(view.ratingBusy, messageId),
      ratingOverrides: withEntry(view.ratingOverrides, messageId, next),
      actionFailure: null,
    })
    this.port.rate({ sessionId, messageId, rating: next, comment: null }).then(
      () => {
        if (account !== this.accountEpoch) return
        this.update((now) => ({ ratingBusy: withoutMember(now.ratingBusy, messageId) }))
      },
      () => {
        if (account !== this.accountEpoch) return
        this.update((now) => ({
          ratingBusy: withoutMember(now.ratingBusy, messageId),
          ratingOverrides: hadOverride ? withEntry(now.ratingOverrides, messageId, previous) : withoutEntry(now.ratingOverrides, messageId),
          actionFailure: 'failed',
        }))
      },
    )
  }

  // ───────────────────────── 问题单 ─────────────────────────

  /** 在本地覆盖、实时执行与云端历史里找问题单。 */
  issue(issueId: string): AssistantIssueDto | undefined {
    return findIssue(issueId, this.view.issueOverrides, this.view.env.state, this.view.detail.issues)
  }

  editIssue(issueId: string): void {
    if (this.issue(issueId) !== undefined) this.set({ issueEditing: issueId })
  }

  cancelIssueEdit(): void {
    this.set({ issueEditing: null })
  }

  /** 确认提交问题单；`edited` 为修改后的标题 / 正文。 */
  confirmIssue(issueId: string, edited?: { title: string; body: string }): void {
    const view = this.view
    if (view.env.stale || view.issueBusy.has(issueId)) return
    const account = this.accountEpoch
    this.set({ issueBusy: withMember(view.issueBusy, issueId), actionFailure: null })
    this.port.confirmIssue({ issueId, title: edited?.title ?? null, body: edited?.body ?? null }).then(
      (issue) => {
        if (account !== this.accountEpoch) return
        this.update((now) => ({
          issueBusy: withoutMember(now.issueBusy, issueId),
          issueOverrides: withEntry(now.issueOverrides, issueId, issue),
          issueEditing: null,
        }))
      },
      () => {
        if (account !== this.accountEpoch) return
        this.update((now) => ({ issueBusy: withoutMember(now.issueBusy, issueId), actionFailure: 'failed' }))
      },
    )
  }

  discardIssue(issueId: string): void {
    const view = this.view
    if (view.env.stale || view.issueBusy.has(issueId)) return
    const account = this.accountEpoch
    const snapshot = this.issue(issueId)
    this.set({ issueBusy: withMember(view.issueBusy, issueId), actionFailure: null })
    this.port.discardIssue({ issueId }).then(
      () => {
        if (account !== this.accountEpoch) return
        this.update((now) => ({
          issueBusy: withoutMember(now.issueBusy, issueId),
          issueOverrides: snapshot ? withEntry(now.issueOverrides, issueId, { ...snapshot, status: 'discarded' }) : now.issueOverrides,
          issueEditing: null,
        }))
      },
      () => {
        if (account !== this.accountEpoch) return
        this.update((now) => ({ issueBusy: withoutMember(now.issueBusy, issueId), actionFailure: 'failed' }))
      },
    )
  }
}

/** 还有进行中的执行（任一会话）。 */
export function hasActiveRun(state: AssistantStateDto): boolean {
  return state.runs.some((run) => isAssistantRunActive(run.status))
}
