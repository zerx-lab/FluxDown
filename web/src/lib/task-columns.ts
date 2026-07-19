// 任务列表视图系统 —— 「列」注册表。
//
// 桌面端 task_columns.dart 是一套独立的 9 列定宽表格渲染（进度150/大小80/创建104/
// 协议64/来源148/队列88/速度90/剩余80/状态60 + 宽度预算护栏 + 紧凑档自动裁列）。
// web 任务行是卡片式（图标+名称+状态相关的一行 meta 文案+进度条），不是表格布局，
// 逐列复刻会与既有视觉语言冲突，故按「web 现有风格适配」简化为：
//   - progress/speed/eta/status 四列在桌面出厂默认即可见，在 web 卡片行里本就是结构性
//     常驻信息（进度条+百分比始终显示；下载中行的速度/剩余时间、其它状态的对应文案见
//     TaskRow.tsx TaskMeta），不纳入可关闭的列系统——切换这四项在 web 语境下无实际意义。
//   - size/created/protocol/source/queue 五列是桌面「可选」列，web 侧保留为真正可切换的
//     「附加信息」：勾选后作为额外 meta 片段追加显示在任务行（TaskRow.tsx），不区分密度。
// 因此本文件只覆盖这 5 个可选列；宽度预算/自动裁列机制不适用（flex 卡片行按内容自然
// 截断，无需手动护栏）。桌面「至少保留一列」护栏不移植：web 的列是可选附加段，
// 0 项合法（时间等显示完全由勾选决定，取消即消失）。

import { t } from './i18n'
import { COLUMN_CANONICAL_ORDER, type TaskColumnId } from './view-prefs'

export { COLUMN_CANONICAL_ORDER }

/** 列标签（面板 chips / 任务行 meta 片段前缀用）。 */
export function columnLabel(id: TaskColumnId): string {
  const KEYS = {
    size: 'view.colSize',
    created: 'view.colCreated',
    protocol: 'view.colProtocol',
    source: 'view.colSource',
    queue: 'view.colQueue',
  } as const
  return t(KEYS[id])
}

