// manifest 前置选择弹窗（多文件清单建组，v1.6 下钻导航范式移植，对齐
// lib/src/widgets/manifest_select_dialog.dart + manifest_dialog_chrome.dart + manifest_advanced_panel.dart，
// UI 细节按 web 现有风格适配，不逐像素照搬）。
//
// 由 manifestSelectStore（lib/dialogs.ts）驱动开关：new-download.tsx 单条 http(s) URL 提交时先
// resolvePreview 探测，命中多文件清单后调用 openManifestSelect() 打开本弹窗——本弹窗以嵌套模态
// 盖在新建下载表单上层（表单保持打开不动，对齐 Flutter 头注释）。取消 → 回到表单；确认 →
// createGroup 成功后一并关闭底层表单（newDownloadOpenStore.set(false)）。
//
// resolverItem 恒为 item.id（v1.6 裁决已砍除画质规格/variant 拼接，见 manifest-selection.ts 头注）。

import { useEffect, useRef, useState } from 'react'
import * as Dialog from '@radix-ui/react-dialog'
import * as DropdownMenu from '@radix-ui/react-dropdown-menu'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, ChevronRight, Folder, FolderOpen, Link as LinkIcon, Plus, X } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { manifestSelectStore, newDownloadOpenStore } from '../../lib/dialogs'
import { fmtBytes, queueDisplayName } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import {
  buildManifestBreadcrumb,
  buildManifestGroupItems,
  manifestAdvancedOptionsDirty,
  manifestDefaultGroupName,
  manifestEffectiveHeaders,
  manifestInvertVisibleSelection,
  manifestIsSearching,
  manifestRowsAt,
  manifestSelectAllVisible,
  manifestSelectionStat,
  manifestSourceHost,
  manifestToggleDirSubtree,
  manifestTopExtensions,
  manifestTotalSize,
  manifestUpPath,
  type ManifestCrumbSegment,
  type ManifestSortKey,
} from '../../lib/manifest-selection'
import { UA_PRESETS } from '../../lib/ua-presets'
import { useStore } from '../../lib/ws'
import { SetSwitch } from '../settings/controls'
import { FsPicker } from './fs-picker'
import { ManifestSelectTree } from './manifest-select-tree'
import { SelectField } from './select-field'

interface HeaderRow {
  id: number
  key: string
  value: string
}

/** 面包屑 `⋯` 折叠段：点击展开被隐藏的中间层级菜单。 */
function BreadcrumbOverflowMenu({ overflow, onNavigate }: { overflow: ManifestCrumbSegment[]; onNavigate: (path: string) => void }) {
  const { t } = useI18n()
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button type="button" className="mf-crumb-more" title={t('manifest.breadcrumbMoreTooltip')}>
          ⋯
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="ctxmenu show" sideOffset={4} align="start">
          {overflow.map((seg) => (
            <DropdownMenu.Item key={seg.path} className="ctx-item" onSelect={() => onNavigate(seg.path)}>
              <Folder size={14} />
              {seg.label}
            </DropdownMenu.Item>
          ))}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  )
}

export function ManifestSelectDialog() {
  const { t } = useI18n()
  const queryClient = useQueryClient()
  const payload = useStore(manifestSelectStore)
  const open = payload !== null
  const items = payload?.manifest.items ?? []

  const [cwd, setCwd] = useState('')
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [extFilter, setExtFilter] = useState<Set<string>>(new Set())
  const [search, setSearch] = useState('')
  const [sortKey, setSortKey] = useState<ManifestSortKey>('name')
  const [groupName, setGroupName] = useState('')
  const [saveDir, setSaveDir] = useState('')
  const [queueId, setQueueId] = useState('')
  const [segments, setSegments] = useState('0')
  const [cookies, setCookies] = useState('')
  const [userAgent, setUserAgent] = useState('')
  const [proxyUrl, setProxyUrl] = useState('')
  const [headerRows, setHeaderRows] = useState<HeaderRow[]>([])
  const [ignoreTlsErrors, setIgnoreTlsErrors] = useState(false)
  const [advOpen, setAdvOpen] = useState(false)
  const [submitting, setSubmitting] = useState(false)
  const headerSeq = useRef(0)

  const { data: queues = [] } = useQuery({ queryKey: ['queues'], queryFn: api.listQueues, enabled: open })

  // 每次新请求到达时，重置为该清单的初始态（表单字段沿用触发方 new-download.tsx 已填值）。
  useEffect(() => {
    if (!payload) return
    setCwd('')
    setSelected(new Set())
    setExtFilter(new Set())
    setSearch('')
    setSortKey('name')
    setGroupName(manifestDefaultGroupName(payload.manifest.name, payload.sourceUrl))
    setSaveDir(payload.saveDir)
    setQueueId(payload.queueId)
    setSegments(String(payload.segments || 0))
    setCookies(payload.cookies)
    setUserAgent(payload.userAgent)
    setProxyUrl(payload.proxyUrl)
    setHeaderRows(Object.entries(payload.extraHeaders).map(([key, value]) => ({ id: ++headerSeq.current, key, value })))
    setIgnoreTlsErrors(false)
    setAdvOpen(false)
    setSubmitting(false)
  }, [payload])

  const segmentOptions = [
    { value: '0', label: t('newDl.segmentsAuto') },
    { value: '1', label: t('newDl.segmentsN', { n: 1 }) },
    { value: '4', label: t('newDl.segmentsN', { n: 4 }) },
    { value: '8', label: t('newDl.segmentsN', { n: 8 }) },
    { value: '16', label: t('newDl.segmentsN', { n: 16 }) },
    { value: '32', label: t('newDl.segmentsN', { n: 32 }) },
  ]
  const userAgentOptions = [{ value: '', label: t('newDl.globalDefault') }, ...UA_PRESETS.map((p) => ({ value: p.value, label: p.label }))]
  const queueOptions = [
    { value: '', label: t('newDl.defaultQueue') },
    ...queues.map((q) => ({
      value: q.queueId,
      label: queueDisplayName(q),
    })),
  ]

  const rowsResult = manifestRowsAt({ items, cwd, selectedItemIds: selected, extFilter, search, sortKey })
  const breadcrumb = buildManifestBreadcrumb({ items, cwd, extFilter, search })
  const topExtensions = manifestTopExtensions(items)
  const selStat = manifestSelectionStat(items, selected)
  const totalSize = manifestTotalSize(items)
  const sourceHost = payload ? manifestSourceHost(payload.sourceUrl) : ''
  const advDirty = manifestAdvancedOptionsDirty({
    proxyUrl,
    ignoreTlsErrors,
    uaInherit: userAgent === '',
    userAgent,
    cookies,
    segments: Number(segments),
    headers: headerRows,
  })

  function syncCwd(nextCwd: string, nextExtFilter: Set<string> = extFilter) {
    const result = manifestRowsAt({ items, cwd: nextCwd, selectedItemIds: selected, extFilter: nextExtFilter, search: '', sortKey })
    setCwd(result.cwd)
  }

  function navigateUp() {
    if (cwd === '' || manifestIsSearching(search)) return
    syncCwd(manifestUpPath({ items, cwd, extFilter }))
  }

  function toggleDirSubtree(dirPath: string) {
    setSelected(manifestToggleDirSubtree({ items, dirPath, selectedItemIds: selected, extFilter, search }))
  }

  function toggleFile(id: string) {
    setSelected((prev) => {
      const next = new Set(prev)
      if (!next.delete(id)) next.add(id)
      return next
    })
  }

  function toggleExt(ext: string) {
    const next = new Set(extFilter)
    if (!next.delete(ext)) next.add(ext)
    setExtFilter(next)
    syncCwd(cwd, next)
  }

  function onSearchChange(value: string) {
    setSearch(value)
    if (!manifestIsSearching(value)) syncCwd(cwd)
  }

  function addHeaderRow() {
    headerSeq.current += 1
    setHeaderRows((rows) => [...rows, { id: headerSeq.current, key: '', value: '' }])
  }
  function removeHeaderRow(id: number) {
    setHeaderRows((rows) => rows.filter((r) => r.id !== id))
  }
  function updateHeaderRow(id: number, field: 'key' | 'value', value: string) {
    setHeaderRows((rows) => rows.map((r) => (r.id === id ? { ...r, [field]: value } : r)))
  }

  function cancel() {
    manifestSelectStore.set(null)
  }

  async function submit(targetQueueId: string, startPaused: boolean) {
    if (!payload || submitting || selStat.count === 0) return
    setSubmitting(true)
    const eff = manifestEffectiveHeaders(headerRows)
    try {
      await api.createGroup({
        sourceUrl: payload.sourceUrl,
        groupName: groupName.trim() || payload.manifest.name || undefined,
        saveDir: saveDir.trim() || undefined,
        queueId: targetQueueId || undefined,
        segments: Number(segments),
        cookies: cookies.trim() || undefined,
        userAgent: userAgent || undefined,
        proxyUrl: proxyUrl.trim() || undefined,
        extraHeaders: Object.keys(eff).length > 0 ? eff : undefined,
        ignoreTlsErrors,
        startPaused,
        items: buildManifestGroupItems(items, selected),
      })
      await Promise.all([queryClient.invalidateQueries({ queryKey: ['groups'] }), queryClient.invalidateQueries({ queryKey: ['tasks'] })])
      manifestSelectStore.set(null)
      newDownloadOpenStore.set(false)
    } catch (err) {
      console.warn('[createGroup] failed', err)
      setSubmitting(false)
    }
  }

  const enabled = selStat.count > 0 && !submitting
  const summaryText =
    selStat.count === 0
      ? t('manifest.noSelection')
      : selStat.unknownCount > 0
        ? `${t('manifest.selectedSummary', { n: selStat.count, size: fmtBytes(selStat.size) })} ${t('manifest.unknownSizeNote', { n: selStat.unknownCount })}`
        : t('manifest.selectedSummary', { n: selStat.count, size: fmtBytes(selStat.size) })

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(o) => {
        if (!o) cancel()
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="wbackdrop mf-backdrop show" />
        <Dialog.Content className="dialog mf-dialog show" onPointerDownOutside={(e) => e.preventDefault()}>
          <header className="dlg-head mf-head">
            <div className="mf-head-icon">
              <FolderOpen size={15} />
            </div>
            <div className="mf-head-main">
              <Dialog.Title asChild>
                <input
                  className="mf-name-input"
                  value={groupName}
                  onChange={(e) => setGroupName(e.target.value)}
                  placeholder={t('manifest.groupNamePlaceholder')}
                  spellCheck={false}
                />
              </Dialog.Title>
              <div className="mf-head-meta">
                <span>{t('manifest.summary', { n: items.length, size: fmtBytes(totalSize) })}</span>
                {sourceHost !== '' && (
                  <span className="mf-head-site" title={sourceHost}>
                    · <LinkIcon size={10} /> {sourceHost}
                  </span>
                )}
                <span className="mf-badge">{t('manifest.pluginBadge')}</span>
              </div>
            </div>
            <Dialog.Close asChild>
              <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                <X size={16} />
              </button>
            </Dialog.Close>
          </header>
          <Dialog.Description className="sr-only">{t('manifest.desc')}</Dialog.Description>

          <div className="dlg-body mf-body">
            <div className="mf-toolbar">
              <input
                className="text-input mf-search"
                placeholder={t('manifest.searchPlaceholder')}
                value={search}
                spellCheck={false}
                onChange={(e) => onSearchChange(e.target.value)}
              />
              <div className="mf-chips">
                {topExtensions.map((chip) => (
                  <button
                    key={chip.ext}
                    type="button"
                    className={cn('mf-chip', extFilter.has(chip.ext) && 'active')}
                    onClick={() => toggleExt(chip.ext)}
                  >
                    {chip.ext} <em>{chip.count}</em>
                  </button>
                ))}
              </div>
              <div className="mf-toolbar-actions">
                <button type="button" className="mf-mini-btn" onClick={() => setSelected(manifestSelectAllVisible(items, { extFilter, search }))}>
                  {t('manifest.selectAll')}
                </button>
                <button
                  type="button"
                  className="mf-mini-btn"
                  onClick={() => setSelected(manifestInvertVisibleSelection(items, selected, { extFilter, search }))}
                >
                  {t('manifest.invertSelection')}
                </button>
                <button type="button" className="mf-mini-btn" onClick={() => setSelected(new Set())}>
                  {t('manifest.clearSelection')}
                </button>
                <button
                  type="button"
                  className={cn('mf-mini-btn', sortKey === 'size' && 'active')}
                  onClick={() => setSortKey((k) => (k === 'name' ? 'size' : 'name'))}
                >
                  {sortKey === 'size' ? t('manifest.sortBySizeDesc') : t('manifest.sortByName')}
                </button>
              </div>
            </div>

            <div className="mf-breadcrumb">
              {breadcrumb.searching ? (
                <span>{t('manifest.searchResultCount', { n: breadcrumb.searchResultCount })}</span>
              ) : (
                <>
                  {breadcrumb.showUp && (
                    <button type="button" className="mf-crumb-up" onClick={navigateUp} title={t('manifest.breadcrumbUpTooltip')}>
                      <ArrowLeft size={13} />
                    </button>
                  )}
                  {breadcrumb.segments.map((seg, i) => (
                    <span key={`${seg.kind}-${seg.path}-${i}`} className="mf-crumb-item">
                      {i > 0 && <span className="mf-crumb-sep"> / </span>}
                      {seg.kind === 'ellipsis' ? (
                        <BreadcrumbOverflowMenu overflow={breadcrumb.overflowSegments} onNavigate={syncCwd} />
                      ) : (
                        <button
                          type="button"
                          className={cn('mf-crumb', seg.isLast && 'cur')}
                          disabled={seg.isLast}
                          onClick={() => syncCwd(seg.path)}
                        >
                          {seg.kind === 'home' ? (
                            <>
                              <Folder size={12} /> {t('manifest.categoryAll')}
                            </>
                          ) : (
                            seg.label
                          )}
                        </button>
                      )}
                    </span>
                  ))}
                </>
              )}
            </div>

            <ManifestSelectTree
              rows={rowsResult.rows}
              selectedItemIds={selected}
              onToggleDirSubtree={toggleDirSubtree}
              onEnterDir={syncCwd}
              onToggleFile={toggleFile}
              height={300}
            />

            <button type="button" className={cn('adv-toggle', advOpen && 'open')} onClick={() => setAdvOpen((o) => !o)}>
              <ChevronRight size={13} />
              {t('manifest.advancedToggle')}
              {advDirty && <span className="mf-adv-dot" />}
            </button>
            <div className={cn('adv-panel', advOpen && 'open')}>
              <div className="grid2">
                <div>
                  <label className="field-label">{t('newDl.proxy')}</label>
                  <input className="text-input" type="text" placeholder="socks5://127.0.0.1:1080" value={proxyUrl} onChange={(e) => setProxyUrl(e.target.value)} />
                </div>
                <div>
                  <label className="field-label">{t('newDl.segments')}</label>
                  <SelectField value={segments} onChange={setSegments} options={segmentOptions} ariaLabel={t('newDl.segments')} />
                </div>
              </div>
              <div className="flex items-center justify-between gap-3 mt-3">
                <label className="field-label !mt-0">{t('manifest.ignoreTlsErrors')}</label>
                <SetSwitch checked={ignoreTlsErrors} onCheckedChange={setIgnoreTlsErrors} />
              </div>
              <label className="field-label">{t('newDl.userAgent')}</label>
              <SelectField value={userAgent} onChange={setUserAgent} options={userAgentOptions} ariaLabel={t('newDl.userAgent')} />
              <label className="field-label" htmlFor="mf-cookies">
                {t('newDl.cookies')}
              </label>
              <input
                id="mf-cookies"
                className="text-input"
                type="text"
                placeholder="key=value; key2=value2"
                value={cookies}
                onChange={(e) => setCookies(e.target.value)}
              />
              <label className="field-label">{t('newDl.headers')}</label>
              <div className="flex flex-col gap-2">
                {headerRows.map((h) => (
                  <div key={h.id} className="flex items-center gap-2">
                    <input
                      className="text-input flex-1"
                      type="text"
                      spellCheck={false}
                      placeholder={t('newDl.headerName')}
                      value={h.key}
                      onChange={(e) => updateHeaderRow(h.id, 'key', e.target.value)}
                    />
                    <input
                      className="text-input flex-1"
                      type="text"
                      spellCheck={false}
                      placeholder={t('newDl.headerValue')}
                      value={h.value}
                      onChange={(e) => updateHeaderRow(h.id, 'value', e.target.value)}
                    />
                    <button type="button" className="icon-btn sm shrink-0" aria-label={t('common.delete')} onClick={() => removeHeaderRow(h.id)}>
                      <X size={14} />
                    </button>
                  </div>
                ))}
                <button type="button" className="btn ghost sm self-start" onClick={addHeaderRow}>
                  <Plus size={13} />
                  {t('newDl.headersAdd')}
                </button>
              </div>
            </div>
          </div>

          <footer className="dlg-foot mf-foot">
            <div className="mf-savedir">
              <label className="field-label">{t('newDl.saveDir')}</label>
              <div className="dir-row">
                <input className="text-input" type="text" spellCheck={false} value={saveDir} onChange={(e) => setSaveDir(e.target.value)} />
                <FsPicker value={saveDir} onChange={setSaveDir} />
              </div>
            </div>
            <div className="mf-queue">
              <label className="field-label">{t('newDl.queue')}</label>
              <SelectField value={queueId} onChange={setQueueId} options={queueOptions} ariaLabel={t('newDl.queue')} />
            </div>
            <span className="mf-sel-summary">{summaryText}</span>
            <span className="flex1" />
            <button type="button" className="btn ghost" onClick={cancel}>
              {t('common.cancel')}
            </button>
            <button type="button" className="btn ghost" disabled={!enabled} onClick={() => void submit('later', true)}>
              {t('newDl.later')}
            </button>
            <button type="button" className="btn primary" disabled={!enabled} onClick={() => void submit(queueId, false)}>
              {submitting ? t('newDl.creating') : selStat.count > 0 ? t('manifest.startDownloadWithCount', { n: selStat.count }) : t('common.startDownload')}
            </button>
          </footer>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
