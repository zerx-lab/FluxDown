import { useAgent, usePrefBool } from '../lib/rpc/hooks'
import { ACTIVITY_ENTRIES } from './activity'
import type { ActivityEntry } from './activity'

const selectAssistantVisible = (snapshot: { assistant?: { visible?: boolean } | null }) => snapshot.assistant?.visible === true

/** 云端是否允许显示 AI 助手入口（`assistant.visible`；快照未就绪 / 旧 agent 缺字段 = 不显示）。 */
export function useAssistantVisible(): boolean {
  return useAgent(selectAssistantVisible, false)
}

/** 条目可见性判定（偏好缺省 = 显示；无偏好键的条目固定显示；助手条目另受云端 `assistant.visible` 约束）。桌面外壳无论条目放在哪个区域都按它过滤。 */
export function useActivityEntryVisible(): (entry: ActivityEntry) => boolean {
  const rss = usePrefBool('ui.show_activity_rss', true)
  const webhooks = usePrefBool('ui.show_activity_webhooks', true)
  const assistantPref = usePrefBool('ui.show_activity_assistant', true)
  const assistantCloud = useAssistantVisible()
  const account = usePrefBool('ui.show_activity_account', true)
  const visible: Record<string, boolean> = {
    'ui.show_activity_rss': rss,
    'ui.show_activity_webhooks': webhooks,
    'ui.show_activity_assistant': assistantPref && assistantCloud,
    'ui.show_activity_account': account,
  }
  return (entry) => entry.prefKey === undefined || visible[entry.prefKey] !== false
}

/** 各可选入口的可见性（移动端底部标签栏用；桌面外壳走布局区域）。 */
export function useVisibleActivityEntries(): ActivityEntry[] {
  const visible = useActivityEntryVisible()
  return ACTIVITY_ENTRIES.filter(visible)
}
