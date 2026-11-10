// 下载页顶栏：桌面主操作在 shell 的 titleLeading 控件之前，搜索 / 视图在弹性留白之后。
// 移动端沿用单行侧栏 / 新建图标 / 搜索 / 视图，不渲染桌面主操作插槽。

import { useState } from 'react'
import { PanelLeft, Plus, SlidersHorizontal } from 'lucide-react'
import { useT } from '../../../i18n'
import { TitleBarSlot } from '../../../shell'
import { Button, Popover, Tooltip, useIsMobile } from '../../../ui'
import { openNewDownload } from '../dialogs'
import { useDownloads } from '../state'
import { SearchBox } from './SearchBox'
import { ViewMenu } from './ViewMenu'
import type { ViewMenuPage } from './ViewMenu'

export function DownloadsTitleBar() {
  const t = useT()
  const mobile = useIsMobile()
  const { sidebarSelection, setSidebarOpen } = useDownloads()
  // 「视图」弹层当前分页（关闭再打开保持，与 GPUI 相同）。
  const [page, setPage] = useState<ViewMenuPage>('columns')
  const newDownloadButton = (
    <Tooltip content={t('newDownload')}>
      <Button
        variant={mobile ? 'ghost' : 'primary'}
        iconOnly={mobile}
        icon={Plus}
        aria-label={t(mobile ? 'newDownload' : 'downloadTask')}
        onClick={() =>
          // GPUI `build_new_download_context` 优先取侧栏选中的队列。
          openNewDownload(sidebarSelection.kind === 'queue' ? { queueId: sidebarSelection.queueId } : undefined)
        }
      >
        {mobile ? null : t('downloadTask')}
      </Button>
    </Tooltip>
  )

  return (
    <>
      {mobile ? null : <TitleBarSlot position="beforeLeading">{newDownloadButton}</TitleBarSlot>}
      <TitleBarSlot>
        {mobile ? (
          <>
            <Button
              variant="ghost"
              iconOnly
              icon={PanelLeft}
              title={t('webOpenSidebar')}
              aria-label={t('webOpenSidebar')}
              onClick={() => setSidebarOpen(true)}
            />
            {newDownloadButton}
          </>
        ) : null}
        <div className="min-w-0 flex-1 mobile:hidden" />
        <SearchBox />
        <Popover
          align="end"
          title={t('viewOptionsTitle')}
          className="w-[320px]"
          trigger={
            <Tooltip content={t('viewOptionsTitle')}>
              <Button variant="ghost" icon={SlidersHorizontal} iconOnly={mobile} aria-label={t('viewMenuLabel')}>
                {mobile ? null : t('viewMenuLabel')}
              </Button>
            </Tooltip>
          }
        >
          <ViewMenu page={page} onPageChange={setPage} />
        </Popover>
      </TitleBarSlot>
    </>
  )
}
