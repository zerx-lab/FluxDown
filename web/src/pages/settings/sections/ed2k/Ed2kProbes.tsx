// eD2K 设置里的两个即时探测：HighID / LowID 检测（「常规」页签）与 Kad 引导（「Kad 节点」页签）。
// 选项与结果只存页面内存，不落配置（仿 eMule 的单选布局，对齐 GPUI `sections/ed2k.rs`）。

import { useState } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../../../i18n'
import { cn } from '../../../../lib/cn'
import { Ed2kKadNodeTable } from '../../../../lib/Ed2kKadNodeTable'
import {
  ed2kIdCheckFailedText,
  ed2kIdCheckOutcome,
  ed2kIdCheckParams,
  ed2kKadBootstrapFailedText,
  ed2kKadBootstrapOutcome,
  ed2kKadBootstrapParams,
} from '../../../../lib/ed2kNetwork'
import type { Ed2kIdCheckChoice, Ed2kIdCheckOutcome, Ed2kKadBootstrapOutcome } from '../../../../lib/ed2kNetwork'
import { rpc } from '../../../../lib/rpc'
import type { Ed2kKadBootstrapDto, Ed2kKadBootstrapSource } from '../../../../lib/rpc'
import { rpcErrorText } from '../../../../lib/rpcErrorText'
import { Button, Input, useIsMobile } from '../../../../ui'
import { SettingsRow, useDaemonNumber } from '../../kit'

function RadioOption({
  selected,
  disabled,
  label,
  onSelect,
  children,
}: {
  selected: boolean
  disabled?: boolean
  label: string
  onSelect: () => void
  children?: ReactNode
}) {
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1.5">
      <button
        type="button"
        role="radio"
        aria-checked={selected}
        disabled={disabled}
        onClick={onSelect}
        className="flex min-w-0 items-center gap-2 rounded-md py-1 text-left text-sm text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
      >
        <span className={cn('flex size-4 shrink-0 items-center justify-center rounded-full border', selected ? 'border-primary' : 'border-input')} aria-hidden>
          {selected ? <span className="size-2 rounded-full bg-primary" /> : null}
        </span>
        <span className="min-w-0 break-words">{label}</span>
      </button>
      {children}
    </div>
  )
}

/** 主机 + 端口两个输入框；只有对应单选项选中时可编辑。 */
function HostPortInputs({
  host,
  port,
  onHost,
  onPort,
  disabled,
}: {
  host: string
  port: string
  onHost: (value: string) => void
  onPort: (value: string) => void
  disabled: boolean
}) {
  const t = useT()
  return (
    <div className="flex min-w-0 flex-1 items-center gap-2">
      <Input value={host} onChange={(e) => onHost(e.target.value)} placeholder={t('ed2kHostPlaceholder')} aria-label={t('ed2kHostPlaceholder')} disabled={disabled} className="min-w-0 flex-1 sm:max-w-56" />
      <Input
        value={port}
        onChange={(e) => onPort(e.target.value)}
        placeholder={t('ed2kPortPlaceholder')}
        aria-label={t('ed2kPortPlaceholder')}
        inputMode="numeric"
        maxLength={5}
        disabled={disabled}
        className="w-20 shrink-0"
      />
    </div>
  )
}

/** 结果行：成功次要文本 + 可选警示行；失败红字。 */
function OutcomeText({ outcome }: { outcome: { ok: true; summary: string; extra: string | null } | { ok: false; message: string } }) {
  return outcome.ok ? (
    <div className="flex flex-col gap-0.5 text-xs text-muted-foreground">
      <span className="break-words">{outcome.summary}</span>
      {outcome.extra === null ? null : <span className="break-words text-warning">{outcome.extra}</span>}
    </div>
  ) : (
    <span role="alert" className="break-words text-xs text-destructive">
      {outcome.message}
    </span>
  )
}

const ID_CHECK_TARGETS: readonly { value: Ed2kIdCheckChoice; key: string }[] = [
  { value: 'server', key: 'ed2kIdCheckTargetServer' },
  { value: 'list', key: 'ed2kIdCheckTargetList' },
  { value: 'session', key: 'ed2kIdCheckTargetSession' },
]

/** HighID / LowID 检测：选目标 + 按钮。 */
export function IdCheckRow() {
  const t = useT()
  const [target, setTarget] = useState<Ed2kIdCheckChoice>('list')
  const [host, setHost] = useState('')
  const [port, setPort] = useState('')
  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<Ed2kIdCheckOutcome | null>(null)
  const [needServer, setNeedServer] = useState(false)

  const run = async () => {
    const params = ed2kIdCheckParams(target, host, port)
    setNeedServer(params === null)
    if (params === null) return
    setBusy(true)
    try {
      setOutcome(ed2kIdCheckOutcome(t, await rpc.daemon.ed2k.idCheck(params), target))
    } catch (err) {
      setOutcome({ ok: false, message: ed2kIdCheckFailedText(t, rpcErrorText(err, t)) })
    } finally {
      setBusy(false)
    }
  }

  const footer = (
    <div className="flex flex-col gap-2">
      <div role="radiogroup" aria-label={t('ed2kIdCheck')} className="flex flex-col gap-1">
        {ID_CHECK_TARGETS.map((option) => (
          <RadioOption key={option.value} selected={target === option.value} disabled={busy} label={t(option.key)} onSelect={() => setTarget(option.value)}>
            {option.value === 'server' ? (
              <HostPortInputs host={host} port={port} onHost={setHost} onPort={setPort} disabled={busy || target !== 'server'} />
            ) : null}
          </RadioOption>
        ))}
      </div>
      {needServer ? (
        <span role="alert" className="text-xs text-destructive">
          {t('ed2kIdCheckNeedServer')}
        </span>
      ) : outcome === null ? null : (
        <OutcomeText outcome={outcome.ok ? { ok: true, summary: outcome.summary, extra: outcome.hint } : outcome} />
      )}
    </div>
  )

  return (
    <SettingsRow title={t('ed2kIdCheck')} description={t('ed2kIdCheckDesc')} footer={footer}>
      <Button variant="outline" loading={busy} disabled={busy} onClick={() => void run()} className="shrink-0">
        {t(busy ? 'ed2kIdCheckRunning' : 'ed2kIdCheckRun')}
      </Button>
    </SettingsRow>
  )
}

/** Kad 引导：选来源 + 按钮；结果下方列出本次联系 / 发现的节点。 */
export function KadBootstrapRow({ disabled }: { disabled: boolean }) {
  const t = useT()
  const narrow = useIsMobile()
  const knownCount = useDaemonNumber('ed2k_nodes_dat_count')
  const [source, setSource] = useState<Ed2kKadBootstrapSource>('known')
  const [host, setHost] = useState('')
  const [port, setPort] = useState('')
  const [url, setUrl] = useState('')
  const [busy, setBusy] = useState(false)
  const [needNode, setNeedNode] = useState(false)
  const [result, setResult] = useState<{ outcome: Ed2kKadBootstrapOutcome; dto: Ed2kKadBootstrapDto | null } | null>(null)
  const locked = disabled || busy

  const run = async () => {
    const params = ed2kKadBootstrapParams(source, host, port, url)
    setNeedNode(params === null)
    if (params === null) return
    setBusy(true)
    try {
      const dto = await rpc.daemon.ed2k.kadBootstrap(params)
      setResult({ outcome: ed2kKadBootstrapOutcome(t, dto), dto })
    } catch (err) {
      setResult({ outcome: { ok: false, message: ed2kKadBootstrapFailedText(t, rpcErrorText(err, t)) }, dto: null })
    } finally {
      setBusy(false)
    }
  }

  const outcome = result?.outcome
  const nodes = result?.outcome.ok && result.dto ? result.dto.nodes ?? [] : []
  const footer = (
    <div className="flex flex-col gap-2">
      <div role="radiogroup" aria-label={t('ed2kKadBootstrap')} className="flex flex-col gap-1">
        <RadioOption selected={source === 'node'} disabled={locked} label={t('ed2kKadBootstrapFromNode')} onSelect={() => setSource('node')}>
          <HostPortInputs host={host} port={port} onHost={setHost} onPort={setPort} disabled={locked || source !== 'node'} />
        </RadioOption>
        <RadioOption selected={source === 'url'} disabled={locked} label={t('ed2kKadBootstrapFromUrl')} onSelect={() => setSource('url')}>
          <Input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder={t('ed2kKadBootstrapUrlPlaceholder')}
            aria-label={t('ed2kKadBootstrapFromUrl')}
            disabled={locked || source !== 'url'}
            className="min-w-0 flex-1 basis-56"
          />
        </RadioOption>
        <RadioOption selected={source === 'known'} disabled={locked} label={t('ed2kKadBootstrapFromKnown', { n: knownCount })} onSelect={() => setSource('known')} />
      </div>
      {needNode ? (
        <span role="alert" className="text-xs text-destructive">
          {t('ed2kKadBootstrapNeedNode')}
        </span>
      ) : outcome === undefined ? null : (
        <OutcomeText outcome={outcome.ok ? { ok: true, summary: outcome.summary, extra: outcome.noReply } : outcome} />
      )}
      {needNode || nodes.length === 0 ? null : <Ed2kKadNodeTable nodes={nodes} narrow={narrow} maxHeightClass="max-h-72" />}
    </div>
  )

  return (
    <SettingsRow title={t('ed2kKadBootstrap')} description={t('ed2kKadBootstrapDesc')} disabled={disabled} footer={footer}>
      <Button variant="outline" loading={busy} disabled={locked} onClick={() => void run()} className="shrink-0">
        {t(busy ? 'ed2kKadBootstrapRunning' : 'ed2kKadBootstrapRun')}
      </Button>
    </SettingsRow>
  )
}
