// 侧边栏（220px）：品牌 + 全局速度、文件类型导航、队列导航、连接徽标、反馈入口。
// 对齐 design/web/index.html .sidebar 结构与 app.js renderSideNavs()。

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import * as Dialog from '@radix-ui/react-dialog'
import { Archive, ArrowUpCircle, FileText, Image as ImageIcon, LayoutGrid, List, LogOut, Film, Monitor, Music, MessageCircle, File as FileIcon, Package2, Pause, Play, Plus, Smartphone, Trash2, X } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { api } from '../../lib/api'
import { cloudApi } from '../../lib/cloud/client'
import { cloudDeviceId, useCloudSession, useShowDeviceSync } from '../../lib/cloud/session'
import { useRemoteTasks } from '../../lib/cloud/useRemoteTasks'
import { clearCredentials, getBase } from '../../lib/auth'
import { cn } from '../../lib/cn'
import { fileType, fmtSpeed, queueDisplayName, typeLabel, TYPE_ORDER, type FileType as FT } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import { connStore, disconnectWs, useGlobalSpeed, useStore } from '../../lib/ws'
import { useUpdateCheck } from '../../lib/update'
import { confirmDialog } from '../../lib/confirm'
import { useTasksUi } from './context'
import { QueueManagerDialog } from './queue-manager-dialog'
import { useViewTasks } from './useViewTasks'

const TYPE_ICONS: Record<'all' | FT, LucideIcon> = {
  all: LayoutGrid,
  video: Film,
  audio: Music,
  document: FileText,
  image: ImageIcon,
  program: Package2,
  archive: Archive,
  other: FileIcon,
}

export function Sidebar() {
  const { t } = useI18n()
  const tasks = useViewTasks()
  const { data: queues = [] } = useQuery({ queryKey: ['queues'], queryFn: api.listQueues })
  const { typeFilter, setTypeFilter, queueFilter, setQueueFilter, deviceFilter, setDeviceFilter, sidebarOpen, setSidebarOpen } = useTasksUi()
  const speed = useGlobalSpeed()
  const conn = useStore(connStore)
  const update = useUpdateCheck()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [logoutOpen, setLogoutOpen] = useState(false)
  const session = useCloudSession()
  const showDeviceOverride = useShowDeviceSync()
  const myDeviceId = cloudDeviceId()
  const { data: cloudDevices = [] } = useQuery({
    queryKey: ['cloud', 'devices'],
    queryFn: () => cloudApi.devices().then((r) => r.devices),
    enabled: session.status === 'authenticated',
    staleTime: 10_000,
  })
  const remoteDevices = cloudDevices.filter((d) => d.deviceId !== myDeviceId)
  const { remoteTasks } = useRemoteTasks()
  // 渐进披露：已登录 + 至少一台远程设备才显示设备区；设置页「显示设备同步」开关可强制显示
  // （即使仅本机，便于提前熟悉入口，见 mdc §4「或本地开关」）。
  const showDeviceSection = session.status === 'authenticated' && (remoteDevices.length > 0 || showDeviceOverride)

  function logout() {
    setLogoutOpen(false)
    disconnectWs()
    clearCredentials()
    qc.clear()
    navigate({ to: '/login' })
  }

  const createQueue = useMutation({
    mutationFn: (name: string) => api.createQueue({ name }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['queues'] }),
  })
  const deleteQueue = useMutation({
    mutationFn: (id: string) => api.deleteQueue(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['queues'] }),
  })
  const startQueue = useMutation({
    mutationFn: (id: string) => api.startQueue(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['queues'] }),
  })
  const stopQueue = useMutation({
    mutationFn: (id: string) => api.stopQueue(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['queues'] }),
  })

  function addQueue() {
    const name = window.prompt(t('sidebar.newQueuePrompt'))
    if (name?.trim()) createQueue.mutate(name.trim())
  }

  const host = (() => {
    const base = getBase()
    if (!base) return location.host
    try {
      return new URL(base).host
    } catch {
      return base
    }
  })()
  const connText =
    conn.status === 'connected'
      ? conn.rttMs != null
        ? t('sidebar.connectedRtt', { rtt: conn.rttMs })
        : t('sidebar.connected')
      : conn.status === 'connecting'
        ? t('sidebar.connecting')
        : t('sidebar.disconnected')

  return (
    <aside className={cn('sidebar', sidebarOpen && 'open')}>
      <div className="side-brand">
        <span className="side-logo">
          <svg viewBox="30 30 452 452" role="img" xmlns="http://www.w3.org/2000/svg">
            <rect x="56" y="56" width="400" height="400" rx="88" fill="#3B82F6" />
            <path
              d="M 226 131 Q 226 119 238 119 L 274 119 Q 286 119 286 131 L 286 296 L 331 251 Q 340 242 349 251 L 363 265 Q 372 274 363 283 L 265 381 Q 256 390 247 381 L 149 283 Q 140 274 149 265 L 163 251 Q 172 242 181 251 L 226 296 Z"
              fill="#F2F4F8"
            />
          </svg>
        </span>
        <div className="side-brand-text">
          <b>FluxDown</b>
          <span>↓ {speed > 0 ? fmtSpeed(speed) : t('sidebar.idle')}</span>
        </div>
      </div>

      <div className="side-scroll">
        <p className="side-label">{t('sidebar.fileTypes')}</p>
        <nav className="side-nav">
          {TYPE_ORDER.map((k) => {
            const Icon = TYPE_ICONS[k]
            const count = k === 'all' ? tasks.length : tasks.filter((t) => fileType(t.fileName, t.url) === k).length
            return (
              <button key={k} type="button" className={cn('side-item', typeFilter === k && 'active')} onClick={() => { setTypeFilter(k); setSidebarOpen(false) }}>
                <Icon size={15} />
                <span>{typeLabel(k)}</span>
                <em>{count || ''}</em>
              </button>
            )
          })}
        </nav>

        {showDeviceSection && (
          <>
            <p className="side-label">设备</p>
            <nav className="side-nav">
              <button
                type="button"
                className={cn('side-item', deviceFilter === null && 'active')}
                onClick={() => { setDeviceFilter(null); setSidebarOpen(false) }}
              >
                <Monitor size={15} />
                <span>全部设备</span>
              </button>
              <button
                type="button"
                className={cn('side-item', deviceFilter === myDeviceId && 'active')}
                onClick={() => { setDeviceFilter(myDeviceId); setSidebarOpen(false) }}
              >
                <Monitor size={15} />
                <i className="queue-dot on" title="在线" />
                <span>{t('cloud.deviceCurrent')}</span>
                <em>{tasks.length || ''}</em>
              </button>
              {remoteDevices.map((d) => {
                const Icon = d.platform === 'android' || d.platform === 'ios' ? Smartphone : Monitor
                const count = remoteTasks.filter((rt) => rt.toDevice === d.deviceId).length
                return (
                  <button
                    key={d.id}
                    type="button"
                    className={cn('side-item', deviceFilter === d.deviceId && 'active')}
                    onClick={() => { setDeviceFilter(d.deviceId); setSidebarOpen(false) }}
                  >
                    <Icon size={15} />
                    <i className={cn('queue-dot', d.isOnline && 'on')} title={d.isOnline ? '在线' : '离线'} />
                    <span>{d.name || '-'}</span>
                    <em>{count || ''}</em>
                  </button>
                )
              })}
            </nav>
          </>
        )}

        <p className="side-label row">
          {t('sidebar.queues')}
          <button type="button" className="side-add" title={t('sidebar.newQueue')} onClick={addQueue}>
            <Plus size={13} />
          </button>
        </p>
        <nav className="side-nav">
          {queues.map((q) => {
            const count = tasks.filter((t) => t.queueId === q.queueId).length
            const builtin = q.queueId === 'main' || q.queueId === 'later'
            const displayName = queueDisplayName(q)
            return (
              <div key={q.queueId} className="queue-row">
                <button
                  type="button"
                  className={cn('side-item', queueFilter === q.queueId && 'active')}
                  onClick={() => { setQueueFilter((f) => (f === q.queueId ? 'all' : q.queueId)); setSidebarOpen(false) }}
                >
                  <List size={15} />
                  <i
                    className={cn('queue-dot', q.isRunning && 'on')}
                    title={q.isRunning ? t('sidebar.queueRunning') : t('sidebar.queueStopped')}
                  />
                  <span>{displayName}</span>
                  <em>{count || ''}</em>
                </button>
                <div className="queue-actions">
                  <button
                    type="button"
                    className="icon-btn sm"
                    title={q.isRunning ? t('sidebar.stopQueue') : t('sidebar.startQueue')}
                    onClick={(e) => {
                      e.stopPropagation()
                      if (q.isRunning) stopQueue.mutate(q.queueId)
                      else startQueue.mutate(q.queueId)
                    }}
                  >
                    {q.isRunning ? <Pause size={13} /> : <Play size={13} />}
                  </button>
                  <QueueManagerDialog queue={q} queueName={displayName} />
                  {!builtin && (
                    <button
                      type="button"
                      className="icon-btn sm"
                      title={t('sidebar.deleteQueue')}
                      onClick={async (e) => {
                        e.stopPropagation()
                        if (await confirmDialog({ title: t('sidebar.deleteQueue'), message: t('sidebar.deleteQueueMsg', { name: displayName }), danger: true }))
                          deleteQueue.mutate(q.queueId)
                      }}
                    >
                      <Trash2 size={13} />
                    </button>
                  )}
                </div>
              </div>
            )
          })}
        </nav>
      </div>

      <div className="side-bottom">
        <div className="conn-badge" title={host}>
          <i className="dot" style={{ background: conn.status === 'connected' ? 'var(--success)' : 'var(--text3)' }} />
          <div className="conn-text">
            <b>{host}</b>
            <span>{connText}</span>
          </div>
          <button type="button" className="icon-btn sm ml-auto shrink-0" title={t('sidebar.logoutTitle')} onClick={() => setLogoutOpen(true)}>
            <LogOut size={13} />
          </button>
        </div>
        <a className="side-feedback" href="https://github.com/zerx-lab/FluxDown/issues" target="_blank" rel="noreferrer">
          <MessageCircle size={14} />
          {t('sidebar.feedback')}
        </a>
        {update.hasUpdate && update.releaseUrl ? (
          <a className="side-feedback" style={{ color: 'var(--accent)' }} href={update.releaseUrl} target="_blank" rel="noreferrer">
            <ArrowUpCircle size={14} />
            {t('sidebar.newVersion', { version: `v${update.latest}` })}
          </a>
        ) : update.current ? (
          <span className="side-feedback" style={{ cursor: 'default' }}>
            {t('sidebar.version', { version: `v${update.current}` })}
          </span>
        ) : null}
      </div>

      <Dialog.Root open={logoutOpen} onOpenChange={setLogoutOpen}>
        <Dialog.Portal>
          <Dialog.Overlay className="wbackdrop show" />
          <Dialog.Content className="dialog sm show">
            <header className="dlg-head">
              <Dialog.Title asChild>
                <b>{t('sidebar.logoutTitle')}</b>
              </Dialog.Title>
              <Dialog.Close asChild>
                <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                  <X size={16} />
                </button>
              </Dialog.Close>
            </header>
            <div className="dlg-body">
              <Dialog.Description className="dlg-sub">{t('sidebar.logoutMsg')}</Dialog.Description>
            </div>
            <footer className="dlg-foot">
              <Dialog.Close asChild>
                <button type="button" className="btn ghost">
                  {t('common.cancel')}
                </button>
              </Dialog.Close>
              <button type="button" className="btn danger" onClick={logout}>
                {t('sidebar.logoutTitle')}
              </button>
            </footer>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </aside>
  )
}
