// 可移植视图会话 store（模块级，React 经 useSyncExternalStore 读取）。
// - 每个 `PluginViewHost` 持有一个 ViewSession：mount → `view.open`，unmount → `view.close`；
// - `pluginViewFrame` 按 seq 应用，断档（seq != last+1）或应用失败改取 `view.snapshot`；
// - `pluginViewClosed` 标记会话结束（显示「已结束」+ 重新打开）；
// - 连接重新就绪（重连、快照整体替换）后对全部会话重取 snapshot，会话已不存在则重新 open；
// - 插件包修订号（`packageRevision`）变化（更新 / dev 重载）后，该插件全部已挂载会话自动重开，
//   含已因 `pluginUnloaded` 结束的（对应 GPUI `windows::plugin::remount_changed`）；
// - 每 30 s 对所有存活会话发 keepalive（daemon 90 s 无续期即关闭）。

import { useSyncExternalStore } from 'react'
import { RpcError, errorMessage, rpc, rpcStore, subscribeServiceEvents } from '../../../lib/rpc'
import type { DaemonEvent, EventFrame, PluginDto, ViewEventDto, ViewNode } from '../../../lib/rpc'
import { VIEW_CLOSE_REASON_REQUESTED, VIEW_ERROR_PROP, applyViewOps } from '../../../lib/rpc/protocol/pluginView'
import type { ViewEffect } from '../../../lib/rpc/protocol/pluginView'
import { toast } from '../../../ui'
import type { ToastKind } from '../../../ui'

export const KEEPALIVE_INTERVAL_MS = 30_000

export type SessionPhase = 'loading' | 'ready' | 'closed' | 'failed'

export interface ViewSessionState {
  phase: SessionPhase
  tree: ViewNode | null
  /** `failed` 时的错误文案。 */
  error: string | null
  /** 会话结束原因（`closed` / `expired` / `pluginUnloaded`）。 */
  reason: string | null
}

export interface ViewTarget {
  identity: string
  view: string
  taskId?: string
  locale: string
}

const INITIAL: ViewSessionState = { phase: 'loading', tree: null, error: null, reason: null }

/** 从事件帧取出 daemon 事件（agent 转发的与 daemon 直出的两种形状）。 */
export function daemonEventOf(frame: EventFrame): DaemonEvent | null {
  const body = frame.event
  if (body.service === 'daemon') return body.event
  return body.event.type === 'daemon' ? body.event.data : null
}

const live = new Map<string, ViewSession>()
/** 已挂载（open 过且未 dispose）的会话，含已结束的；修订号变化时据此重开。 */
const mounted = new Set<ViewSession>()
/** 每个插件最近一次见到的包修订号（只记 daemon 下发的值）。 */
const revisions = new Map<string, number>()

/**
 * open 应答之前就到达的会话动作（插件可在 `init` 里 `ctx.navigate()` / `ctx.close()`，这些事件与
 * open 应答走不同通道，可能先到）。只在有 open 进行中时缓存，登记会话时执行；全部 open 结束即清空。
 * daemon 向所有 UI 连接广播，别的客户端的会话也会落进来，故设上限。
 */
interface EarlyActions {
  effects: ViewEffect[]
  closed: string | null
}
const EARLY_SESSIONS_MAX = 32
const early = new Map<string, EarlyActions>()
let opening = 0

function stashEarly(session: string, record: (actions: EarlyActions) => void): void {
  if (opening === 0) return
  let actions = early.get(session)
  if (actions === undefined) {
    if (early.size >= EARLY_SESSIONS_MAX) {
      console.warn('plugin view early actions dropped: too many pending sessions', session)
      return
    }
    actions = { effects: [], closed: null }
    early.set(session, actions)
  }
  record(actions)
}

let pluginNavigator: ((target: string) => void) | null = null

/** 注册主界面跳转器（外壳挂一次）；`null` 注销。未注册时 navigate 动作被忽略。 */
export function setPluginNavigator(fn: ((target: string) => void) | null): void {
  pluginNavigator = fn
}

export class ViewSession {
  private state: ViewSessionState = INITIAL
  private id: string | null = null
  private seq = 0
  private disposed = false
  /** 每次 open / reopen 递增，用来丢弃过期的异步结果。 */
  private epoch = 0
  private resyncing = false
  private appliedLocale: string | null = null
  private localeSync: Promise<void> | null = null
  private readonly listeners = new Set<() => void>()

  /** 容器关闭处理（登录 / 设置弹窗等）；未设置则 requested 关闭显示「已结束」。 */
  onRequestClose?: () => void

  constructor(readonly target: ViewTarget) {}

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  readonly getState = (): ViewSessionState => this.state

  get sessionId(): string | null {
    return this.id
  }

  private set(next: Partial<ViewSessionState>): void {
    this.state = { ...this.state, ...next }
    for (const listener of this.listeners) listener()
  }

  private register(id: string): void {
    this.id = id
    live.set(id, this)
    ensureRunning()
  }

  private unregister(): void {
    if (this.id !== null && live.get(this.id) === this) live.delete(this.id)
    this.id = null
    stopIfIdle()
  }

  /** Update the existing daemon session without reopening or re-running plugin initialization. */
  setLocale(locale: string): void {
    if (this.target.locale === locale) return
    this.target.locale = locale
    this.syncLocale()
  }

  private syncLocale(): void {
    const id = this.id
    if (id === null || this.localeSync !== null || this.disposed) return
    const epoch = this.epoch
    this.localeSync = (async () => {
      while (this.id === id && this.epoch === epoch && this.appliedLocale !== this.target.locale) {
        const locale = this.target.locale
        try {
          await rpc.daemon.plugin.viewSetLocale({ session: id, locale })
          this.appliedLocale = locale
        } catch (error) {
          toast.error(error)
          if (this.target.locale === locale) return
        }
      }
    })().finally(() => {
      this.localeSync = null
      if (this.id !== id || this.epoch !== epoch) this.syncLocale()
    })
  }

  /** 打开（或重新打开）会话。 */
  async open(): Promise<void> {
    mount(this)
    const epoch = ++this.epoch
    const openedLocale = this.target.locale
    this.appliedLocale = null
    this.unregister()
    this.set({ phase: 'loading', error: null, reason: null })
    // open 进行中就订阅事件并缓存未知会话的动作：插件 init 里发出的动作可能先于应答到达。
    opening += 1
    ensureRunning()
    try {
      const opened = await rpc.daemon.plugin.viewOpen({
        identity: this.target.identity,
        view: this.target.view,
        taskId: this.target.taskId,
        locale: openedLocale,
      })
      if (this.disposed || epoch !== this.epoch) {
        early.delete(opened.session)
        void rpc.daemon.plugin.viewClose({ session: opened.session }).catch(() => undefined)
        return
      }
      this.seq = opened.seq
      this.register(opened.session)
      this.appliedLocale = openedLocale
      this.set({ phase: 'ready', tree: opened.tree })
      this.syncLocale()
      const pending = early.get(opened.session)
      if (pending !== undefined) {
        early.delete(opened.session)
        // 与 daemon 发出的先后一致：先执行动作，再处理关闭。
        for (const effect of pending.effects) runEffect(effect)
        if (pending.closed !== null) this.handleClosed(pending.closed)
      }
    } catch (error) {
      if (this.disposed || epoch !== this.epoch) return
      this.set({ phase: 'failed', error: errorMessage(error) })
    } finally {
      opening -= 1
      if (opening === 0) early.clear()
      stopIfIdle()
    }
  }

  /** 卸载：关闭 daemon 会话。 */
  dispose(): void {
    this.disposed = true
    this.epoch += 1
    mounted.delete(this)
    const id = this.id
    this.unregister()
    this.listeners.clear()
    if (id !== null) void rpc.daemon.plugin.viewClose({ session: id }).catch(() => undefined)
  }

  /** 插件包被替换后原地重开：先关掉仍存活的旧 daemon 会话，再 open。 */
  async reopen(): Promise<void> {
    if (this.disposed) return
    const id = this.id
    if (id !== null) void rpc.daemon.plugin.viewClose({ session: id }).catch(() => undefined)
    await this.open()
  }

  /** 应用一帧；seq 非 last+1 或应用失败时改取 snapshot。 */
  applyFrame(seq: number, ops: Parameters<typeof applyViewOps>[1]): void {
    const tree = this.state.tree
    if (this.state.phase !== 'ready' || tree === null) return
    if (seq <= this.seq) return
    if (seq !== this.seq + 1) {
      void this.resync()
      return
    }
    try {
      const next = applyViewOps(tree, ops)
      this.seq = seq
      this.set({ tree: next })
    } catch {
      void this.resync()
    }
  }

  /** `pluginViewClosed`：插件主动 `ctx.close()` 且有可关闭容器 → 注销并交给容器；否则进入「已结束」。 */
  handleClosed(reason: string): void {
    if (reason === VIEW_CLOSE_REASON_REQUESTED && this.onRequestClose !== undefined) {
      const close = this.onRequestClose
      this.unregister()
      close()
      return
    }
    this.markClosed(reason)
  }

  markClosed(reason: string): void {
    this.unregister()
    this.set({ phase: 'closed', reason })
  }

  /** 重取 snapshot；会话已不存在则重新 open。 */
  async resync(): Promise<void> {
    const id = this.id
    if (id === null || this.resyncing || this.disposed) return
    this.resyncing = true
    const epoch = this.epoch
    try {
      const snapshot = await rpc.daemon.plugin.viewSnapshot({ session: id })
      if (this.disposed || epoch !== this.epoch || this.id !== id) return
      this.seq = snapshot.seq
      this.set({ phase: 'ready', tree: snapshot.tree })
    } catch (error) {
      if (this.disposed || epoch !== this.epoch || this.id !== id) return
      // 传输层失败（断线 / 超时）等连接就绪后再取；会话确实不存在才重新 open。
      if (error instanceof RpcError && (error.appCode === 'unavailable' || error.appCode === 'timeout')) return
      this.resyncing = false
      await this.open()
    } finally {
      this.resyncing = false
    }
  }

  /** 投递视图事件（入队即返回；结果经帧送回）。 */
  send(node: string, event: ViewEventDto): void {
    const id = this.id
    if (id === null || this.state.phase !== 'ready') return
    void rpc.daemon.plugin.viewEvent({ session: id, node, event }).catch((error: unknown) => {
      toast.error(error)
    })
  }
}

// ── 全局：事件订阅 / 重连重取 / keepalive ──

let unsubscribeEvents: (() => void) | null = null
let unsubscribeStore: (() => void) | null = null
let keepaliveTimer: ReturnType<typeof setInterval> | null = null
let wasReady = rpcStore.peek().connection.phase === 'ready'
let lastPlugins: readonly PluginRevision[] | undefined

function mount(session: ViewSession): void {
  if (mounted.has(session)) return
  mounted.add(session)
  const { identity } = session.target
  const revision = rpcStore.peek().snapshot?.daemon.plugins.find((plugin) => plugin.identity === identity)?.packageRevision
  if (revision !== undefined) revisions.set(identity, revision)
}

type PluginRevision = Pick<PluginDto, 'identity' | 'packageRevision'>

/**
 * 插件表更新：某插件的包修订号相对上次见到的值变化，就重开它的全部已挂载会话；返回的 Promise 在
 * 全部重开结束后落定。插件暂时不可见 / 旧 daemon 不下发修订号时保留旧视图与旧基准，不触发重开。
 */
export async function reopenOnRevisionChange(plugins: readonly PluginRevision[]): Promise<void> {
  const reopening: Promise<void>[] = []
  for (const plugin of plugins) {
    const next = plugin.packageRevision
    if (next === undefined) continue
    const previous = revisions.get(plugin.identity)
    revisions.set(plugin.identity, next)
    if (previous === undefined || previous === next) continue
    for (const session of mounted) {
      if (session.target.identity === plugin.identity) reopening.push(session.reopen())
    }
  }
  await Promise.all(reopening)
}

function runEffect(effect: ViewEffect): void {
  if (effect.kind !== 'navigate' || !('target' in effect) || typeof effect.target !== 'string') return
  if (pluginNavigator === null) {
    console.warn('plugin navigate ignored: no navigator registered', effect.target)
    return
  }
  pluginNavigator(effect.target)
}

/** 仅测试用：直接注入事件帧。 */
export const onPluginViewEventForTest = (frame: EventFrame): void => onFrame(frame)

function onFrame(frame: EventFrame): void {
  // agent 重连 daemon 后整体替换快照：断线期间可能漏帧。
  if (frame.event.service === 'agent' && frame.event.event.type === 'daemonSnapshotReplaced') {
    resyncAll()
    return
  }
  const event = daemonEventOf(frame)
  if (event === null) return
  if (event.type === 'pluginViewFrame') {
    const session = live.get(event.data.session)
    const effects = event.data.effects ?? []
    if (session === undefined) {
      if (effects.length > 0) stashEarly(event.data.session, (actions) => actions.effects.push(...effects))
      return
    }
    // 动作帧：先执行 effects（不受 seq 影响），再按 seq 规则处理 ops。
    for (const effect of effects) runEffect(effect)
    session.applyFrame(event.data.seq, event.data.ops)
  } else if (event.type === 'pluginViewClosed') {
    const session = live.get(event.data.session)
    const { reason } = event.data
    if (session === undefined) stashEarly(event.data.session, (actions) => (actions.closed = reason))
    else session.handleClosed(reason)
  }
}

function resyncAll(): void {
  for (const session of [...live.values()]) void session.resync()
}

function onStore(): void {
  const state = rpcStore.peek()
  const ready = state.connection.phase === 'ready'
  // 重新进入就绪（重连 / 重新同步）：断线期间可能漏帧，全部重取。
  if (ready && !wasReady) resyncAll()
  wasReady = ready
  const plugins = state.snapshot?.daemon.plugins
  if (plugins !== undefined && plugins !== lastPlugins) {
    lastPlugins = plugins
    void reopenOnRevisionChange(plugins)
  }
}

function keepalive(): void {
  const sessions = [...live.keys()]
  if (sessions.length === 0) return
  void rpc.daemon.plugin.viewKeepalive({ sessions }).catch(() => undefined)
}

function ensureRunning(): void {
  unsubscribeEvents ??= subscribeServiceEvents(onFrame)
  unsubscribeStore ??= rpcStore.subscribe(onStore)
  keepaliveTimer ??= setInterval(keepalive, KEEPALIVE_INTERVAL_MS)
}

function stopIfIdle(): void {
  if (live.size > 0 || opening > 0 || mounted.size > 0) return
  unsubscribeEvents?.()
  unsubscribeEvents = null
  unsubscribeStore?.()
  unsubscribeStore = null
  if (keepaliveTimer !== null) clearInterval(keepaliveTimer)
  keepaliveTimer = null
}

export function useViewSession(session: ViewSession): ViewSessionState {
  return useSyncExternalStore(session.subscribe, session.getState, session.getState)
}

/** 根节点上的渲染错误（`p.__error`）。 */
export function viewError(tree: ViewNode | null): string | null {
  const value = tree?.p[VIEW_ERROR_PROP]
  return typeof value === 'string' && value !== '' ? value : null
}

// ── toast ──

const TOAST_KINDS: readonly ToastKind[] = ['info', 'success', 'warning', 'error']

export function toastKind(level: string): ToastKind {
  return (TOAST_KINDS as readonly string[]).includes(level) ? (level as ToastKind) : 'info'
}

/** 订阅 `pluginToast` 事件并转为全局 toast（挂在外壳上，返回取消函数）。 */
export function startPluginToasts(): () => void {
  return subscribeServiceEvents((frame) => {
    const event = daemonEventOf(frame)
    if (event?.type !== 'pluginToast') return
    const { level, text } = event.data
    const kind = toastKind(level)
    if (kind === 'error') toast.error(text)
    else toast[kind](text)
  })
}
