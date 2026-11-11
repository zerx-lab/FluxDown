// 下载页（对齐 GPUI crates/downloads/src/pages/downloads.rs）：
// 标题栏插槽（新建 / 搜索 / 视图）→ 侧栏 | 任务表 | 详情面板 →（仅移动端）状态栏；移动端侧栏与详情改为 Sheet，表格改为卡片行。

import { useEffect, useRef, useState } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import { Sheet, toast, useIsMobile } from '../../ui'
import { SETTINGS_ERROR_KEYS, clearSettingsError, useSettingsError } from '../settings/kit/writeStore'
import { DownloadDialogsHost, openNewDownload } from './dialogs'
import { DetailPanel } from './detail'
import { confirmDeleteWithFiles, deleteViews } from './model/actions'
import { DETAIL_SIZE_RANGE, SIDEBAR_WIDTH_RANGE } from './model/viewPrefs'
import { SelectionBar } from './selection'
import { Sidebar } from './sidebar'
import { DownloadsProvider, useDownloads } from './state'
import { StatusBar } from './statusbar'
import { TaskCards } from './table/TaskCards'
import { TaskTable } from './table/TaskTable'
import { DownloadsTitleBar } from './titlebar'

const MAX_DROP_TEXT_BYTES = 1_048_576
const SUPPORTED_URL = /^(https?:\/\/|ftps?:\/\/|magnet:|ed2k:\/\/)/i

/** 从拖入文本按行提取受支持协议的下载链接。 */
function parseDropUrls(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => SUPPORTED_URL.test(line))
}

function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  return target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)
}

/** 拖放导入：`.torrent` 交给新建下载；`.txt/.url/.list`（≤1MB）与拖入的链接文本按行提取后预填。 */
async function importDropped(data: DataTransfer, unsupportedHint: string): Promise<void> {
  const torrentFiles: File[] = []
  const urls: string[] = []
  let unsupported = false
  for (const file of Array.from(data.files)) {
    const ext = file.name.split('.').pop()?.toLowerCase()
    if (ext === 'torrent') torrentFiles.push(file)
    else if ((ext === 'txt' || ext === 'url' || ext === 'list') && file.size <= MAX_DROP_TEXT_BYTES) {
      urls.push(...parseDropUrls(await file.text()))
    } else unsupported = true
  }
  if (data.files.length === 0) {
    const text = data.getData('text/uri-list') || data.getData('text/plain')
    urls.push(...parseDropUrls(text))
    if (urls.length === 0 && text.trim() !== '') unsupported = true
  }
  if (torrentFiles.length > 0 || urls.length > 0) {
    openNewDownload({ ...(urls.length > 0 ? { urls } : {}), ...(torrentFiles.length > 0 ? { torrentFiles } : {}) })
  }
  if (unsupported) toast.info(unsupportedHint)
}

function ResizeHandle({
  orientation,
  onDrag,
}: {
  orientation: 'vertical' | 'horizontal'
  /** 拖拽时回传相对起点的位移（px）。 */
  onDrag: (delta: number, start: boolean) => void
}) {
  const start = (event: ReactPointerEvent) => {
    event.preventDefault()
    const origin = orientation === 'vertical' ? event.clientX : event.clientY
    onDrag(0, true)
    const move = (moveEvent: PointerEvent) => {
      onDrag((orientation === 'vertical' ? moveEvent.clientX : moveEvent.clientY) - origin, false)
    }
    const up = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
  }
  return (
    <div
      role="separator"
      aria-orientation={orientation}
      onPointerDown={start}
      className={cn(
        'group relative z-10 shrink-0 bg-hairline',
        orientation === 'vertical' ? 'w-px cursor-col-resize' : 'h-px cursor-row-resize',
      )}
    >
      <div
        className={cn(
          'absolute bg-transparent transition-colors group-hover:bg-drag-border/40 group-active:bg-drag-border/60',
          orientation === 'vertical' ? 'inset-y-0 -left-1 w-2' : 'inset-x-0 -top-1 h-2',
        )}
      />
    </div>
  )
}

function DownloadsBody() {
  const t = useT()
  const mobile = useIsMobile()
  const ctx = useDownloads()
  const { prefs, updatePrefs, sidebarOpen, setSidebarOpen, detailOpen, closeDetail, connected } = ctx
  const settingsError = useSettingsError()
  const settingsErrorText = settingsError
    ? `${t(SETTINGS_ERROR_KEYS[settingsError.kind])}${settingsError.kind === 'invalidArgument' && settingsError.detail ? `: ${settingsError.detail}` : ''}`
    : null
  const [dragging, setDragging] = useState(false)
  const dragDepth = useRef(0)
  const dragOrigin = useRef(0)

  // 键盘：Esc 清空选择、Ctrl/Cmd+A 全选、Delete 删除选中（输入框聚焦时不拦截）。
  const ctxRef = useRef(ctx)
  ctxRef.current = ctx
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (isEditable(event.target) || document.querySelector('[role="dialog"]')) return
      const current = ctxRef.current
      const secondary = event.ctrlKey || event.metaKey
      if (event.key === 'Escape' && current.selected.size > 0) {
        current.clearSelection()
      } else if (secondary && event.key.toLowerCase() === 'a') {
        event.preventDefault()
        current.selectAll()
      } else if (event.key === 'Delete' && current.selectedViews.length > 0) {
        event.preventDefault()
        if (event.shiftKey) void confirmDeleteWithFiles(current.selectedViews)
        else void deleteViews(current.selectedViews, false)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [])

  const hasFiles = (event: React.DragEvent) =>
    Array.from(event.dataTransfer.types).some((type) => type === 'Files' || type === 'text/uri-list' || type === 'text/plain')

  const table = mobile ? <TaskCards /> : <TaskTable />
  const main = (
    <div className="relative min-h-0 min-w-0 flex-1">
      {table}
      {mobile ? <SelectionBar /> : null}
    </div>
  )

  const showDocked = !mobile && detailOpen
  const [minSize, maxSize] = DETAIL_SIZE_RANGE[prefs.detail_placement]
  const detailSize = Math.min(maxSize, Math.max(minSize, prefs.detail_size))
  const docked = showDocked ? (
    <>
      <ResizeHandle
        orientation={prefs.detail_placement === 'bottom' ? 'horizontal' : 'vertical'}
        onDrag={(delta, start) => {
          if (start) dragOrigin.current = detailSize
          const size = dragOrigin.current - delta
          updatePrefs((current) => ({ ...current, detail_size: Math.round(Math.min(maxSize, Math.max(minSize, size))) }))
        }}
      />
      <div
        className="min-h-0 min-w-0 shrink-0"
        style={prefs.detail_placement === 'bottom' ? { height: detailSize } : { width: detailSize }}
      >
        <DetailPanel mode="docked" />
      </div>
    </>
  ) : null

  return (
    <div
      className="relative flex h-full min-h-0 flex-col bg-surface"
      onDragEnter={(event) => {
        if (!hasFiles(event)) return
        dragDepth.current += 1
        setDragging(true)
      }}
      onDragLeave={(event) => {
        if (!hasFiles(event)) return
        dragDepth.current = Math.max(0, dragDepth.current - 1)
        if (dragDepth.current === 0) setDragging(false)
      }}
      onDragOver={(event) => {
        if (hasFiles(event)) event.preventDefault()
      }}
      onDrop={(event) => {
        dragDepth.current = 0
        setDragging(false)
        // 表头列换位用 HTML5 拖拽（无文件），不当作导入。
        if (!hasFiles(event)) return
        event.preventDefault()
        void importDropped(event.dataTransfer, t('unsupportedDropHint'))
      }}
    >
      <div className="flex min-h-0 flex-1">
        {mobile ? null : (
          <>
            <aside
              className="min-h-0 shrink-0 overflow-hidden"
              style={{
                width: Math.min(SIDEBAR_WIDTH_RANGE[1], Math.max(SIDEBAR_WIDTH_RANGE[0], prefs.sidebar_width)),
              }}
            >
              <Sidebar />
            </aside>
            <ResizeHandle
              orientation="vertical"
              onDrag={(delta, start) => {
                if (start) dragOrigin.current = prefs.sidebar_width
                const width = dragOrigin.current + delta
                updatePrefs((current) => ({
                  ...current,
                  sidebar_width: Math.round(Math.min(SIDEBAR_WIDTH_RANGE[1], Math.max(SIDEBAR_WIDTH_RANGE[0], width))),
                }))
              }}
            />
          </>
        )}
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          {settingsErrorText ? (
            <div role="alert" className="flex shrink-0 items-start gap-2 border-b border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
              <span className="min-w-0 flex-1">{settingsErrorText}</span>
              <button type="button" className="shrink-0 underline coarse:min-h-touch" onClick={clearSettingsError}>
                {t('close')}
              </button>
            </div>
          ) : null}
          {connected ? null : (
            <div className="shrink-0 truncate px-3 py-1 text-xs text-destructive">{t('localServiceDisconnected')}</div>
          )}
          <div className={cn('flex min-h-0 min-w-0 flex-1', prefs.detail_placement === 'right' ? 'flex-row' : 'flex-col')}>
            {main}
            {docked}
          </div>
        </div>
      </div>
      {/* 桌面端状态栏属于外壳（所有路由可见、可自定义布局）；移动端仍是下载页自带的固定状态栏。 */}
      {mobile ? <StatusBar /> : null}
      {dragging ? <div className="pointer-events-none absolute inset-0 z-30 bg-drop-target" /> : null}

      {mobile ? (
        <>
          <Sheet open={sidebarOpen} onOpenChange={setSidebarOpen} side="left" title={t('sidebarStatus')} hideHeader bodyClassName="p-0">
            <Sidebar onNavigate={() => setSidebarOpen(false)} />
          </Sheet>
          <Sheet
            open={detailOpen}
            onOpenChange={(open) => {
              if (!open) closeDetail()
            }}
            side="right"
            title={t('detail')}
            bodyClassName="p-0"
          >
            <DetailPanel mode="sheet" />
          </Sheet>
        </>
      ) : null}
    </div>
  )
}

export function DownloadsPage() {
  return (
    <DownloadsProvider>
      <DownloadsTitleBar />
      <DownloadsBody />
      <DownloadDialogsHost />
    </DownloadsProvider>
  )
}
