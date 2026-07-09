// 侧边栏（220px）：品牌 + 全局速度、文件类型导航、队列导航、连接徽标、反馈入口。
// 对齐 design/web/index.html .sidebar 结构与 app.js renderSideNavs()。

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import * as Dialog from '@radix-ui/react-dialog'
import { Archive, FileText, Image as ImageIcon, LayoutGrid, List, LogOut, Film, Music, MessageCircle, File as FileIcon, Plus, Trash2, X } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { api } from '../../lib/api'
import { clearCredentials, getBase } from '../../lib/auth'
import { cn } from '../../lib/cn'
import { fileType, fmtSpeed, TYPE_LABELS, type FileType as FT } from '../../lib/format'
import { connStore, disconnectWs, useGlobalSpeed, useStore } from '../../lib/ws'
import { confirmDialog } from '../../lib/confirm'
import { useTasksUi } from './context'
import { useViewTasks } from './useViewTasks'

const TYPE_ICONS: Record<'all' | FT, LucideIcon> = {
  all: LayoutGrid,
  video: Film,
  audio: Music,
  document: FileText,
  image: ImageIcon,
  archive: Archive,
  other: FileIcon,
}

export function Sidebar() {
  const tasks = useViewTasks()
  const { data: queues = [] } = useQuery({ queryKey: ['queues'], queryFn: api.listQueues })
  const { typeFilter, setTypeFilter, queueFilter, setQueueFilter } = useTasksUi()
  const speed = useGlobalSpeed()
  const conn = useStore(connStore)
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [logoutOpen, setLogoutOpen] = useState(false)

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

  function addQueue() {
    const name = window.prompt('新队列名称')
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
      ? `已连接${conn.rttMs != null ? ` · 延迟 ${conn.rttMs}ms` : ''}`
      : conn.status === 'connecting'
        ? '连接中…'
        : '已断开'

  return (
    <aside className="sidebar">
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
          <span>↓ {speed > 0 ? fmtSpeed(speed) : '空闲'}</span>
        </div>
      </div>

      <div className="side-scroll">
        <p className="side-label">文件类型</p>
        <nav className="side-nav">
          {(Object.keys(TYPE_LABELS) as ('all' | FT)[]).map((k) => {
            const Icon = TYPE_ICONS[k]
            const count = k === 'all' ? tasks.length : tasks.filter((t) => fileType(t.fileName, t.url) === k).length
            return (
              <button key={k} type="button" className={cn('side-item', typeFilter === k && 'active')} onClick={() => setTypeFilter(k)}>
                <Icon size={15} />
                <span>{TYPE_LABELS[k]}</span>
                <em>{count || ''}</em>
              </button>
            )
          })}
        </nav>

        <p className="side-label row">
          队列
          <button type="button" className="side-add" title="新建队列" onClick={addQueue}>
            <Plus size={13} />
          </button>
        </p>
        <nav className="side-nav">
          <button type="button" className={cn('side-item', queueFilter === 'all' && 'active')} onClick={() => setQueueFilter('all')}>
            <List size={15} />
            <span>全部任务</span>
            <em>{tasks.length || ''}</em>
          </button>
          {queues.map((q) => {
            const count = tasks.filter((t) => t.queueId === q.queueId).length
            return (
              <div key={q.queueId} className="group relative">
                <button
                  type="button"
                  className={cn('side-item', queueFilter === q.queueId && 'active')}
                  onClick={() => setQueueFilter(q.queueId)}
                >
                  <List size={15} />
                  <span>{q.name}</span>
                  <em className="group-hover:opacity-0">{count || ''}</em>
                </button>
                <button
                  type="button"
                  className="icon-btn sm absolute top-1/2 right-1 hidden -translate-y-1/2 group-hover:grid"
                  title="删除队列"
                  onClick={async (e) => {
                    e.stopPropagation()
                    if (await confirmDialog({ title: '删除队列', message: `删除队列「${q.name}」？其中的任务会移动到默认队列。`, danger: true })) deleteQueue.mutate(q.queueId)
                  }}
                >
                  <Trash2 size={13} />
                </button>
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
          <button type="button" className="icon-btn sm ml-auto shrink-0" title="退出登录" onClick={() => setLogoutOpen(true)}>
            <LogOut size={13} />
          </button>
        </div>
        <a className="side-feedback" href="https://github.com/zerx-lab/FluxDown/issues" target="_blank" rel="noreferrer">
          <MessageCircle size={14} />
          反馈
        </a>
      </div>

      <Dialog.Root open={logoutOpen} onOpenChange={setLogoutOpen}>
        <Dialog.Portal>
          <Dialog.Overlay className="wbackdrop show" />
          <Dialog.Content className="dialog sm show">
            <header className="dlg-head">
              <Dialog.Title asChild>
                <b>退出登录</b>
              </Dialog.Title>
              <Dialog.Close asChild>
                <button type="button" className="icon-btn sm" aria-label="关闭">
                  <X size={16} />
                </button>
              </Dialog.Close>
            </header>
            <div className="dlg-body">
              <Dialog.Description className="dlg-sub">
                将断开与服务器的连接并清除本设备上保存的令牌，下次访问需重新登录。
              </Dialog.Description>
            </div>
            <footer className="dlg-foot">
              <Dialog.Close asChild>
                <button type="button" className="btn ghost">
                  取消
                </button>
              </Dialog.Close>
              <button type="button" className="btn danger" onClick={logout}>
                退出登录
              </button>
            </footer>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </aside>
  )
}
