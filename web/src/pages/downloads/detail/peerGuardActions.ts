// 防吸血 Peer 处置：文案与右键菜单项（纯数据，组件见 BtPeerGuard.tsx）。

import { Ban, FileSearch, Gauge, ShieldCheck } from 'lucide-react'
import { peerCommandParams } from '../../../lib/btGuard'
import type { PeerGuardCommand, PeerGuardView } from '../../../lib/btGuard'
import { rpc } from '../../../lib/rpc'
import { toastRpcError } from '../../../lib/rpcToast'
import { toast } from '../../../ui'
import type { MenuEntry } from '../../../ui'
import { camel, optionalText } from '../../settings/kit'
import type { Translate } from '../table/text'
import type { PeerRow } from './btStatus'

export function ruleName(t: Translate, rule: string): string {
  return optionalText(t, `btGuardRule${camel(rule)}`) ?? rule
}

export function actionName(t: Translate, action: string): string {
  return t(`btGuardAction${camel(action)}`)
}

/** 处置文案：观察态显示「本应…（仅观察）」。 */
export function peerActionText(t: Translate, view: PeerGuardView): string | null {
  if (view.action === 'allow') return null
  const label = actionName(t, view.action)
  return view.observeOnly ? t('btGuardPeerObserved', { action: label }) : label
}

async function runCommand(t: Translate, group: string, command: PeerGuardCommand): Promise<void> {
  try {
    await rpc.daemon.bt.guard.entries.add(peerCommandParams(group, command))
    const key = command.kind === 'ban' ? 'btGuardPeerBanned' : command.kind === 'throttle' ? 'btGuardPeerThrottled' : 'btGuardPeerAllowed'
    toast.success(t(key, { group }))
  } catch (failure) {
    toastRpcError(failure)
  }
}

/** 右键菜单的防吸血项：封禁（1 小时 / 1 天 / 7 天 / 永久）、限速、加白、查看证据；没有 IP 组时为空。 */
export function peerGuardMenu(row: PeerRow, t: Translate, onEvidence: (row: PeerRow) => void): MenuEntry[] {
  const group = row.guard.ipGroup
  if (group === '') return []
  const ban = (key: string, label: string, duration: 'forever' | '1h' | '1d' | '7d'): MenuEntry => ({
    type: 'item',
    key,
    label,
    icon: Ban,
    destructive: true,
    onSelect: () => void runCommand(t, group, { kind: 'ban', duration }),
  })
  return [
    { type: 'separator', key: 'guard-sep' },
    ban('guard-ban-1h', t('btGuardPeerBan1h'), '1h'),
    ban('guard-ban-1d', t('btGuardPeerBan1d'), '1d'),
    ban('guard-ban-7d', t('btGuardPeerBan7d'), '7d'),
    ban('guard-ban-forever', t('btGuardPeerBanForever'), 'forever'),
    { type: 'item', key: 'guard-throttle', label: t('btGuardPeerThrottle'), icon: Gauge, onSelect: () => void runCommand(t, group, { kind: 'throttle' }) },
    { type: 'item', key: 'guard-allow', label: t('btGuardPeerAllow'), icon: ShieldCheck, onSelect: () => void runCommand(t, group, { kind: 'allow' }) },
    { type: 'item', key: 'guard-evidence', label: t('btGuardPeerViewEvidence'), icon: FileSearch, onSelect: () => onEvidence(row) },
  ]
}

