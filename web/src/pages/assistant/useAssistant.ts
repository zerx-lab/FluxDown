// 助手页的数据入口：把快照（助手状态 / 登录态 / 连接）与界面语言喂给控制器，并订阅它的视图状态。

import { useEffect, useMemo, useState, useSyncExternalStore } from 'react'
import { useI18n } from '../../i18n'
import { EMPTY_ASSISTANT_STATE, rpc, useAgent, useConnection } from '../../lib/rpc'
import { AssistantController } from './controller'
import type { AssistantEnv, ViewState } from './controller'
import { pendingConfirmations } from './model'

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

/** 倒计时用的时钟：助手状态变化时立即校准；有待确认工具时每秒走一次，全部结束后停表。 */
function useNowMs(state: AssistantEnv['state']): number {
  const [now, setNow] = useState(() => Date.now())
  const ticking = pendingConfirmations(state, now) > 0
  useEffect(() => {
    setNow(Date.now())
  }, [state])
  useEffect(() => {
    if (!ticking) return
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [ticking])
  return now
}
