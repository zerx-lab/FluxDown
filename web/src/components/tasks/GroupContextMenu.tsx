// 任务组右键菜单：全部暂停/恢复 + 复制来源链接 + 删除(+删除文件)。
// 对齐 lib/src/widgets/task_group_card.dart showGroupContextMenu，但去掉桌面独有的
// 「打开文件夹」（web 无本地文件系统概念）与「重试失败」（REST 无独立端点——
// 全部恢复已在引擎侧 resume_group 同时重启 paused/error 成员，见
// native/engine/src/download_manager.rs）。

import * as ContextMenu from '@radix-ui/react-context-menu'
import { Copy, Pause, Play, Trash2 } from 'lucide-react'
import type { ReactNode } from 'react'
import { confirmDialog } from '../../lib/confirm'
import { useI18n } from '../../lib/i18n'
import { groupDisplayName } from '../../lib/task-group'
import type { GroupDto } from '../../lib/types'

export function GroupContextMenu({
  group,
  hasActive,
  onPauseAll,
  onResumeAll,
  onDelete,
  children,
}: {
  group: GroupDto
  hasActive: boolean
  onPauseAll: () => void
  onResumeAll: () => void
  onDelete: (deleteFiles: boolean) => void
  children: ReactNode
}) {
  const { t } = useI18n()
  const name = groupDisplayName(group)
  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger asChild>{children}</ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content className="ctxmenu show">
          {hasActive ? (
            <ContextMenu.Item className="ctx-item" onSelect={onPauseAll}>
              <Pause size={14} />
              {t('group.pauseAll')}
            </ContextMenu.Item>
          ) : (
            <ContextMenu.Item className="ctx-item" onSelect={onResumeAll}>
              <Play size={14} />
              {t('group.resumeAll')}
            </ContextMenu.Item>
          )}
          <ContextMenu.Item className="ctx-item" onSelect={() => void navigator.clipboard.writeText(group.sourceUrl)}>
            <Copy size={14} />
            {t('group.copySourceLink')}
          </ContextMenu.Item>
          <ContextMenu.Separator className="ctx-sep" />
          <ContextMenu.Item
            className="ctx-item danger"
            onSelect={async () => {
              if (await confirmDialog({ title: t('group.deleteTitle'), message: t('group.deleteMsg', { name }), danger: true })) onDelete(false)
            }}
          >
            <Trash2 size={14} />
            {t('group.delete')}
          </ContextMenu.Item>
          <ContextMenu.Item
            className="ctx-item danger"
            onSelect={async () => {
              if (await confirmDialog({ title: t('group.deleteTitle'), message: t('group.deleteWithFilesMsg', { name }), danger: true }))
                onDelete(true)
            }}
          >
            <Trash2 size={14} />
            {t('group.deleteWithFiles')}
          </ContextMenu.Item>
        </ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  )
}
