// 助手页的数据入口：把快照（助手状态 / 登录态 / 连接）与界面语言喂给控制器，并订阅它的视图状态。

import { useEffect, useMemo, useState, useSyncExternalStore } from 'react'
import { useI18n } from '../../i18n'
import { EMPTY_ASSISTANT_STATE, rpc, useAgent, useConnection } from '../../lib/rpc'
import { AssistantController } from './controller'
import type { AssistantEnv, ViewState } from './controller'
import { agentNowMs, clockSkewMs, pendingConfirmations } from './model'

export interface AssistantHandle {
  controller: AssistantController
  view: ViewState
  /** 快照已到手（此前登录态未知，页面显示加载而非「请登录」）。 */
  ready: boolean
  /** 当前时间（毫秒）；有待确认工具时每秒刷新，用于倒计时与过期判定。 */
  now: number
}

export function useAssistant(): AssistantHandle {
  const { locale } = useI18n()
  const { phase } = useConnection()
  const state = useAgent((snapshot) => snapshot.assistant ?? EMPTY_ASSISTANT_STATE, EMPTY_ASSISTANT_STATE)
  const signedIn = useAgent((snapshot) => snapshot.session !== null, false)
  const ready = useAgent(() => true, false)
  // 助手由 agent 本身承载：只要连着 agent 就可用（与 daemon 是否在线无关）。
  const stale = phase !== 'ready'

  const env = useMemo<AssistantEnv>(() => ({ state, signedIn, stale, locale }), [state, signedIn, stale, locale])
  const [controller] = useState(() => new AssistantController(rpc.agent.assistant, env))
  useEffect(() => controller.sync(env), [controller, env])
  const view = useSyncExternalStore(controller.subscribe, controller.getSnapshot)

  const now = useNowMs(state)
  return { controller, view, ready, now }
}

/** 有多少个工具在等用户确认（面板收起时入口据此提示，避免确认窗口无声过期）。 */
export function usePendingConfirmationCount(): number {
  const state = useAgent((snapshot) => snapshot.assistant ?? EMPTY_ASSISTANT_STATE, EMPTY_ASSISTANT_STATE)
  return pendingConfirmations(state, useNowMs(state))
}

/**
 * 倒计时 / 过期判定用的时钟（agent 时间轴，与 `confirmDeadlineUnixMs` 同一时钟）：本机与 agent 主机的时钟偏差在每次
 * 收到助手状态时校准；有待确认工具时每秒走一次，全部结束后停表。
 */
function useNowMs(state: AssistantEnv['state']): number {
  const [local, setLocal] = useState(() => Date.now())
  const [calibration, setCalibration] = useState(() => ({ state, skew: clockSkewMs(state, Date.now()) }))
  if (calibration.state !== state) setCalibration({ state, skew: clockSkewMs(state, Date.now()) })
  const now = agentNowMs(local, calibration.skew)
  const ticking = pendingConfirmations(state, now) > 0
  useEffect(() => {
    setLocal(Date.now())
  }, [state])
  useEffect(() => {
    if (!ticking) return
    const timer = setInterval(() => setLocal(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [ticking])
  return now
}
