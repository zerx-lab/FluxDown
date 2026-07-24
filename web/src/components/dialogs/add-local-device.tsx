// 添加本地设备弹窗（对齐 .dlg-* 约定）—— 三步流程：
// 1) browse  发现列表（打开时 discovery start + 2s 轮询 discovered，关闭/离开时 stop）
//            叠加手动 host:port 探测的命中结果，点击任一设备进入下一步；
// 2) code    输入对端设备上显示的配对码 → POST pair/begin，拿到 token + SAS；
// 3) confirm 展示 SAS 数字供双方人工核对 → 确认/取消 → POST pair/finish → 刷新
//            ['link','devices']（无论 accept 与否都要 finish，释放对端等待的 token）。
//
// 全程若命中"宿主不支持"错误（见 lib/link.ts isLinkUnsupportedError），退化为一条提示，
// 不展示发现列表/表单，避免用户对着一个永远不会有结果的空列表干等。

import { useEffect, useMemo, useState } from 'react'
import * as Dialog from '@radix-ui/react-dialog'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronRight, Monitor, Plus, Smartphone, X } from 'lucide-react'
import { useI18n } from '../../lib/i18n'
import { friendlyLinkError, isLinkUnsupportedError, linkApi, type LinkDiscoveredPeerDto, type LinkPairBeginResponse } from '../../lib/link'

type Step = 'browse' | 'code' | 'confirm'

const DEVICES_QUERY_KEY = ['link', 'devices']
const DISCOVERED_QUERY_KEY = ['link', 'discovered']

function peerKey(peer: { host: string; port: number }): string {
  return `${peer.host}:${peer.port}`
}

export function AddLocalDeviceDialog() {
  const { t } = useI18n()
  const qc = useQueryClient()
  const [open, setOpen] = useState(false)
  const [step, setStep] = useState<Step>('browse')

  const [manualHost, setManualHost] = useState('')
  const [manualPort, setManualPort] = useState('')
  const [manualError, setManualError] = useState('')
  const [manualPeer, setManualPeer] = useState<LinkDiscoveredPeerDto | null>(null)

  const [selected, setSelected] = useState<LinkDiscoveredPeerDto | null>(null)
  const [peerCode, setPeerCode] = useState('')
  const [beginResult, setBeginResult] = useState<LinkPairBeginResponse | null>(null)
  const [stepError, setStepError] = useState('')

  const [discoveryError, setDiscoveryError] = useState<unknown>(null)

  // 打开对话框即启动发现；关闭（或对话框卸载）时停止。start/stop 均幂等，失败静默捕获——
  // 未支持的宿主会立即报错，记下来驱动下方的"宿主不支持"提示。
  useEffect(() => {
    if (!open) return
    setDiscoveryError(null)
    linkApi.discovery('start').catch((err) => setDiscoveryError(err))
    return () => {
      linkApi.discovery('stop').catch(() => {})
    }
  }, [open])

  const { data: discoveredData } = useQuery({
    queryKey: DISCOVERED_QUERY_KEY,
    queryFn: () => linkApi.discovered().then((r) => r.peers),
    enabled: open && step === 'browse' && discoveryError === null,
    refetchInterval: open && step === 'browse' && discoveryError === null ? 2000 : false,
    retry: false,
  })

  const unsupported = discoveryError !== null && isLinkUnsupportedError(discoveryError)

  const candidates = useMemo(() => {
    const list = discoveredData ?? []
    if (manualPeer && !list.some((p) => peerKey(p) === peerKey(manualPeer))) return [...list, manualPeer]
    return list
  }, [discoveredData, manualPeer])

  const probeMut = useMutation({
    mutationFn: () => linkApi.probe(manualHost.trim(), Number(manualPort)),
    onSuccess: (peer) => {
      setManualPeer(peer)
      setManualError('')
    },
    onError: (err) => setManualError(friendlyLinkError(t, err)),
  })

  function submitManualProbe() {
    const host = manualHost.trim()
    const port = Number(manualPort)
    if (!host || !Number.isInteger(port) || port <= 0 || port > 65535) {
      setManualError(t('link.manualInvalid'))
      return
    }
    setManualError('')
    probeMut.mutate()
  }

  function selectPeer(peer: LinkDiscoveredPeerDto) {
    setSelected(peer)
    setPeerCode('')
    setStepError('')
    setStep('code')
  }

  const beginMut = useMutation({
    mutationFn: () => linkApi.pairBegin(selected!.host, selected!.port, peerCode.trim()),
    onSuccess: (res) => {
      setBeginResult(res)
      setStepError('')
      setStep('confirm')
    },
    onError: (err) => setStepError(friendlyLinkError(t, err)),
  })

  function submitBegin() {
    if (!selected || !peerCode.trim() || beginMut.isPending) return
    beginMut.mutate()
  }

  const finishMut = useMutation({
    mutationFn: (accept: boolean) => linkApi.pairFinish(beginResult!.token, accept),
    onSuccess: (res, accept) => {
      void qc.invalidateQueries({ queryKey: DEVICES_QUERY_KEY })
      if (accept && !res.paired) {
        setStepError(t('link.pairRejected'))
        return
      }
      close()
    },
    onError: (err) => setStepError(friendlyLinkError(t, err)),
  })

  function reset() {
    setStep('browse')
    setManualHost('')
    setManualPort('')
    setManualError('')
    setManualPeer(null)
    setSelected(null)
    setPeerCode('')
    setBeginResult(null)
    setStepError('')
    setDiscoveryError(null)
  }

  function close() {
    setOpen(false)
    reset()
  }

  const title = step === 'browse' ? t('link.addDeviceTitle') : step === 'code' ? t('link.enterPeerCodeTitle') : t('link.sasTitle')

  return (
    <Dialog.Root open={open} onOpenChange={(o) => (o ? setOpen(true) : close())}>
      <Dialog.Trigger asChild>
        <button type="button" className="btn ghost">
          <Plus size={14} />
          {t('link.addDevice')}
        </button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="wbackdrop show" />
        <Dialog.Content className="dialog show" onPointerDownOutside={(e) => e.preventDefault()}>
          <header className="dlg-head">
            <Dialog.Title asChild>
              <b>{title}</b>
            </Dialog.Title>
            <Dialog.Close asChild>
              <button type="button" className="icon-btn sm" aria-label={t('common.close')}>
                <X size={16} />
              </button>
            </Dialog.Close>
          </header>

          {step === 'browse' && (
            <div className="dlg-body">
              <Dialog.Description className="dlg-sub">{t('link.addDeviceDesc')}</Dialog.Description>
              {unsupported ? (
                <p className="set-note">{t('link.unsupportedHost')}</p>
              ) : (
                <>
                  <div className="set-group">
                    {candidates.length === 0 ? (
                      <p className="device-list-empty">{t('link.discoveredEmpty')}</p>
                    ) : (
                      candidates.map((peer) => {
                        const PeerIcon = peer.platform === 'android' || peer.platform === 'ios' ? Smartphone : Monitor
                        return (
                          <div key={peerKey(peer)} className="device-item">
                            <div className="set-row">
                              <button type="button" className="device-row-main" onClick={() => selectPeer(peer)}>
                                <div className="grid h-8 w-8 flex-shrink-0 place-items-center rounded-lg bg-surface2 text-text2">
                                  <PeerIcon size={15} />
                                </div>
                                <div className="min-w-0 flex-1 text-left">
                                  <b className="truncate text-[13px] font-medium">{peer.name || peer.host}</b>
                                  <p className="text-[11.5px] text-text3">
                                    {peer.host}:{peer.port} · {peer.source === 'manual' ? t('link.sourceManual') : t('link.sourceMdns')}
                                  </p>
                                </div>
                                <ChevronRight size={13} className="flex-shrink-0 text-text3" />
                              </button>
                            </div>
                          </div>
                        )
                      })
                    )}
                  </div>
                  <label className="field-label" style={{ marginTop: 0 }}>
                    {t('link.manualAddTitle')}
                  </label>
                  <div className="dir-row">
                    <input
                      className="text-input"
                      style={{ flex: 2 }}
                      placeholder={t('link.manualHostPlaceholder')}
                      spellCheck={false}
                      value={manualHost}
                      onChange={(e) => setManualHost(e.target.value)}
                    />
                    <input
                      className="text-input"
                      style={{ flex: 1 }}
                      placeholder={t('link.manualPortPlaceholder')}
                      inputMode="numeric"
                      value={manualPort}
                      onChange={(e) => setManualPort(e.target.value.replace(/\D/g, ''))}
                    />
                    <button type="button" className="btn ghost sm" disabled={probeMut.isPending} onClick={submitManualProbe}>
                      {probeMut.isPending ? t('common.loading') : t('link.probe')}
                    </button>
                  </div>
                  {manualError && <p className="mt-1.5 text-[11.5px] text-danger">{manualError}</p>}
                  {manualPeer && !manualError && (
                    <p className="mt-1.5 text-[11.5px] text-success">{t('link.probeSuccess', { name: manualPeer.name || manualPeer.host })}</p>
                  )}
                </>
              )}
            </div>
          )}

          {step === 'code' && (
            <>
              <div className="dlg-body">
                <Dialog.Description className="dlg-sub">
                  {t('link.pairWithDesc', { name: selected?.name || selected?.host || '' })}
                </Dialog.Description>
                <label className="field-label" style={{ marginTop: 0 }}>
                  {t('link.peerCodeLabel')}
                </label>
                <input
                  className="text-input"
                  style={{ letterSpacing: 4, textAlign: 'center', fontSize: 18 }}
                  maxLength={6}
                  placeholder={t('link.peerCodePlaceholder')}
                  value={peerCode}
                  autoFocus
                  onChange={(e) => setPeerCode(e.target.value.trim())}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') submitBegin()
                  }}
                />
                {stepError && <p className="mt-2 text-[11.5px] text-danger">{stepError}</p>}
              </div>
              <footer className="dlg-foot">
                <button
                  type="button"
                  className="btn ghost"
                  onClick={() => {
                    setStep('browse')
                    setStepError('')
                  }}
                >
                  {t('common.back')}
                </button>
                <button type="button" className="btn primary" disabled={!peerCode.trim() || beginMut.isPending} onClick={submitBegin}>
                  {beginMut.isPending ? t('common.loading') : t('link.pairBegin')}
                </button>
              </footer>
            </>
          )}

          {step === 'confirm' && (
            <>
              <div className="dlg-body">
                <Dialog.Description className="dlg-sub">{t('link.sasHint')}</Dialog.Description>
                <div className="token-box" style={{ justifyContent: 'center' }}>
                  <b className="text-[22px] font-semibold tracking-[5px]">{beginResult?.sas}</b>
                </div>
                <p className="mt-2 text-center text-[12px] text-text3">{t('link.sasPeerName', { name: beginResult?.peerName || '' })}</p>
                {stepError && <p className="mt-2 text-center text-[11.5px] text-danger">{stepError}</p>}
              </div>
              <footer className="dlg-foot">
                <button type="button" className="btn ghost" disabled={finishMut.isPending} onClick={() => finishMut.mutate(false)}>
                  {t('common.cancel')}
                </button>
                <button type="button" className="btn primary" disabled={finishMut.isPending} onClick={() => finishMut.mutate(true)}>
                  {finishMut.isPending ? t('common.loading') : t('link.confirmPair')}
                </button>
              </footer>
            </>
          )}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
