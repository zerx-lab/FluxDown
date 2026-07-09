// 任务行右键菜单。纯展示 + 回调 props，不持有任何 mutation（由 TaskRow 统一持有并下发），
// 对齐 design/web/app.js ctxItems()。

import * as ContextMenu from '@radix-ui/react-context-menu'
import { ChevronRight, Copy, Download, Link2, ListOrdered, Pause, Play, RotateCcw, Trash2, Zap } from 'lucide-react'
import type { ReactNode } from 'react'
import { taskFileUrl } from '../../lib/api'
import { confirmDialog } from '../../lib/confirm'
import type { QueueDto } from '../../lib/types'
import type { ViewTask } from './useViewTasks'

export function TaskContextMenu({
  task: t,
  queues,
  onSelect,
  onPause,
  onContinue,
  onBoost,
  onDelete,
  onMove,
  children,
}: {
  task: ViewTask
  queues: QueueDto[]
  onSelect: () => void
  onPause: () => void
  onContinue: () => void
  onBoost: () => void
  onDelete: (deleteFiles: boolean) => void
  onMove: (queueId: string) => void
  children: ReactNode
}) {
  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger asChild onContextMenu={onSelect}>
        {children}
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content className="ctxmenu show">
          {(t.status === 1 || t.status === 5) && (
            <ContextMenu.Item className="ctx-item" onSelect={onPause}>
              <Pause size={14} />
              暂停
            </ContextMenu.Item>
          )}
          {(t.status === 2 || t.status === 0) && (
            <ContextMenu.Item className="ctx-item" onSelect={onContinue}>
              <Play size={14} />
              继续
            </ContextMenu.Item>
          )}
          {t.status === 4 && (
            <ContextMenu.Item className="ctx-item" onSelect={onContinue}>
              <RotateCcw size={14} />
              重试
            </ContextMenu.Item>
          )}
          {t.status !== 3 && (
            <ContextMenu.Item className="ctx-item" onSelect={onBoost}>
              <Zap size={14} />
              Boost 优先下载
            </ContextMenu.Item>
          )}
          {t.status === 3 && (
            <ContextMenu.Item
              className="ctx-item"
              onSelect={() => {
                location.href = taskFileUrl(t.taskId)
              }}
            >
              <Download size={14} />
              保存到本地
            </ContextMenu.Item>
          )}
          <ContextMenu.Item className="ctx-item" onSelect={() => void navigator.clipboard.writeText(t.url)}>
            <Copy size={14} />
            复制下载链接
          </ContextMenu.Item>
          <ContextMenu.Item className="ctx-item" onSelect={() => void navigator.clipboard.writeText(`${t.saveDir}/${t.fileName}`)}>
            <Link2 size={14} />
            复制文件服务器路径
          </ContextMenu.Item>
          {queues.filter((q) => q.queueId !== t.queueId).length > 0 && (
            <ContextMenu.Sub>
              <ContextMenu.SubTrigger className="ctx-item">
                <ListOrdered size={14} />
                移动到队列…
                <ChevronRight size={13} style={{ marginLeft: 'auto' }} />
              </ContextMenu.SubTrigger>
              <ContextMenu.Portal>
                <ContextMenu.SubContent className="ctxmenu show">
                  {queues
                    .filter((q) => q.queueId !== t.queueId)
                    .map((q) => (
                      <ContextMenu.Item key={q.queueId} className="ctx-item" onSelect={() => onMove(q.queueId)}>
                        {q.name}
                      </ContextMenu.Item>
                    ))}
                </ContextMenu.SubContent>
              </ContextMenu.Portal>
            </ContextMenu.Sub>
          )}
          <ContextMenu.Separator className="ctx-sep" />
          <ContextMenu.Item className="ctx-item danger" onSelect={() => onDelete(false)}>
            <Trash2 size={14} />
            删除
          </ContextMenu.Item>
          <ContextMenu.Item
            className="ctx-item danger"
            onSelect={async () => {
              if (await confirmDialog({ title: '删除任务', message: '删除任务并删除本地文件？此操作不可撤销。', danger: true })) onDelete(true)
            }}
          >
            <Trash2 size={14} />
            删除并删文件
          </ContextMenu.Item>
        </ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  )
}
