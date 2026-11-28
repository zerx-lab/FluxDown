// 会话级动作：改名（行内输入，Enter 保存 / Esc 取消）与导出（Markdown / JSON，云端生成、浏览器保存）。
// 会话列表的每一行与面板顶栏（当前会话）共用。

import { Download } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import type { KeyboardEvent } from 'react'
import { useT } from '../../i18n'
import { ASSISTANT_MAX_TITLE_CHARS } from '../../lib/rpc'
import type { AssistantExportFormat } from '../../lib/rpc'
import { ActionMenu, Button, Icon, Input, Spinner, toast } from '../../ui'
import type { MenuEntry } from '../../ui'
import type { AssistantController, ViewState } from './controller'
import { saveExport } from './exportFile'
import { validTitle } from './model'

/** 行内改名：Enter 保存（标题去首尾空白后 1–80 字符）、Esc / 失焦取消；保存中禁用输入。 */
export function RenameField({
  sessionId,
  initial,
  controller,
  stale,
  onDone,
}: {
  sessionId: string
  initial: string
  controller: AssistantController
  stale: boolean
  /** 保存成功或取消后退出编辑。 */
  onDone: () => void
}) {
  const t = useT()
  const [value, setValue] = useState(initial)
  const [saving, setSaving] = useState(false)
  const ref = useRef<HTMLInputElement>(null)
  const alive = useRef(true)

  useEffect(() => {
    alive.current = true
    ref.current?.focus()
    ref.current?.select()
    return () => {
      alive.current = false
    }
  }, [])

  const valid = validTitle(value)
  const save = () => {
    if (valid === null || saving || stale) return
    if (valid === initial.trim()) {
      onDone()
      return
    }
    setSaving(true)
    void controller.renameSession(sessionId, valid).then((ok) => {
      if (!alive.current) return
      setSaving(false)
      if (ok) onDone()
      else ref.current?.focus()
    })
  }
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Escape') {
      event.preventDefault()
      // 面板（移动端对话框）自己也在听 Esc：这里只取消改名。
      event.stopPropagation()
      onDone()
      return
    }
    if (event.key !== 'Enter' || event.nativeEvent.isComposing || event.keyCode === 229) return
    event.preventDefault()
    save()
  }

  return (
    <Input
      ref={ref}
      value={value}
      invalid={valid === null}
      disabled={saving}
      maxLength={ASSISTANT_MAX_TITLE_CHARS * 2}
      placeholder={t('assistantRenamePlaceholder')}
      aria-label={t('assistantRename')}
      className="h-7 min-w-0 flex-1 text-sm"
      onChange={(event) => setValue(event.target.value)}
      onKeyDown={onKeyDown}
      onBlur={() => {
        if (!saving) onDone()
      }}
    />
  )
}

/** 导出菜单：Markdown / JSON；云端生成内容，成功后保存为本地文件并提示文件名。 */
export function ExportMenu({
  sessionId,
  view,
  controller,
  className,
  iconSize = 'sm',
}: {
  sessionId: string
  view: ViewState
  controller: AssistantController
  className?: string
  iconSize?: 'sm' | 'md'
}) {
  const t = useT()
  const busy = view.exporting === sessionId
  const disabled = view.env.stale || view.exporting !== null
  const run = (format: AssistantExportFormat) => {
    void controller.exportSession(sessionId, format).then((result) => {
      if (result === null) return
      saveExport(result)
      toast.key('assistantExported', 'success', { name: result.fileName })
    })
  }
  const entries: MenuEntry[] = [
    { type: 'item', key: 'markdown', label: t('assistantExportMarkdown'), onSelect: () => run('markdown') },
    { type: 'item', key: 'json', label: t('assistantExportJson'), onSelect: () => run('json') },
  ]
  return (
    <ActionMenu
      title={t('assistantExport')}
      entries={entries}
      trigger={
        <Button variant="ghost" iconOnly className={className} aria-label={t('assistantExport')} title={t('assistantExport')} disabled={disabled}>
          {busy ? <Spinner size={iconSize} /> : <Icon icon={Download} size={iconSize} />}
        </Button>
      }
    />
  )
}
