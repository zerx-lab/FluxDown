// 安全与访问：local_server_* 配置组 + 令牌管理 + WS 会话状态。
import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Eye, EyeOff, RefreshCw } from 'lucide-react'
import { api } from '../../lib/api'
import { CopyButton } from '../CopyButton'
import type { ConfigMap } from '../../lib/types'
import { connStore, useStore } from '../../lib/ws'
import { alertDialog } from '../../lib/confirm'
import { SetRow, SetSwitch, TextInput } from './controls'

export function SecuritySettings({
  config,
  mutate,
}: {
  config: ConfigMap
  mutate: (entries: ConfigMap) => void
}) {
  const token = config.local_server_token ?? ''
  const [showToken, setShowToken] = useState(false)
  const takeover = (config.local_server_takeover_enabled ?? 'true') === 'true'
  const jsonrpc = (config.local_server_jsonrpc_enabled ?? 'true') === 'true'
  const conn = useStore(connStore)
  const { data: stats } = useQuery({ queryKey: ['stats'], queryFn: api.stats, refetchInterval: 5000 })

  function saveToken(next: string) {
    const v = next.trim()
    if (v === token) return
    mutate({ local_server_token: v })
    void alertDialog({ message: '访问令牌已保存，重启服务器后生效' })
  }

  function randomToken() {
    const bytes = new Uint8Array(16)
    crypto.getRandomValues(bytes)
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
    saveToken(`fxd_${hex}`)
  }

  return (
    <>
      <h2 className="set-title">安全与访问</h2>
      <p className="set-desc">对应 local_server_* 配置组 · 服务仅监听配置的地址</p>
      <div className="set-group">
        <SetRow title="访问令牌" desc="Web / 管理 API 强制鉴权（Authorization: Bearer）· 可自定义，重启服务器后生效">
          <div className="token-box">
            <TextInput value={token} onCommit={saveToken} password={!showToken} placeholder="自定义或生成令牌" />
            <button type="button" title={showToken ? '隐藏令牌' : '显示令牌'} onClick={() => setShowToken((s) => !s)}>
              {showToken ? <EyeOff /> : <Eye />}
            </button>
            <CopyButton value={token} title="复制令牌" />
            <button type="button" title="随机生成令牌" onClick={randomToken}>
              <RefreshCw />
            </button>
          </div>
        </SetRow>
      </div>
      <div className="set-group">
        <SetRow title="aria2 兼容 RPC" desc="/jsonrpc · addUri / getGlobalStat / multicall">
          <SetSwitch checked={jsonrpc} onCheckedChange={(v) => mutate({ local_server_jsonrpc_enabled: String(v) })} />
        </SetRow>
        <SetRow title="脚本接管入口" desc="/download · 油猴脚本 / 浏览器扩展">
          <SetSwitch checked={takeover} onCheckedChange={(v) => mutate({ local_server_takeover_enabled: String(v) })} />
        </SetRow>
      </div>
      <div className="set-group">
        <SetRow
          title="本机 WebSocket 连接"
          desc={conn.status === 'connected' ? `已连接 · 延迟 ${conn.rttMs ?? '—'}ms` : '未连接'}
        >
          <span className="set-value">{stats ? `服务器共 ${stats.wsClients} 个会话` : '—'}</span>
        </SetRow>
      </div>
    </>
  )
}
