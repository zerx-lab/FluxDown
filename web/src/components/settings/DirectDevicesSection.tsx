// 直连设备（link）：无需登录任何账号，与同一局域网内、或经自建内网穿透/组网（地址可达
// 即可）的另一台 FluxDown 直接配对——与云账户（CloudAccountSettings 的云中转设备）完全
// 独立。作为账户设置页内嵌的一个 section 渲染（见 CloudAccountSettings.tsx），不是独立
// 设置分类：已配对名册（在线圆点/平台图标/移除确认）+ 显示本机配对码（POST /link/code）+
// 添加设备入口（弹窗见 add-local-device.tsx）。宿主未启用/不支持互联时整节退化为一条
// 友好提示，不渲染操作入口。

import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Monitor, Smartphone, Trash2 } from 'lucide-react'
import { cn } from '../../lib/cn'
import { confirmDialog } from '../../lib/confirm'
import { type I18nKey, useI18n } from '../../lib/i18n'
import { friendlyLinkError, isLinkUnsupportedError, linkApi, type LinkDeviceDto } from '../../lib/link'
import { AddLocalDeviceDialog } from '../dialogs/add-local-device'
import { SetRow } from './controls'

const DEVICES_QUERY_KEY = ['link', 'devices']

// 平台标签：复用云账户设备管理已有的 cloud.platform.* 翻译键（取值集合相同：
// windows|macos|linux|android|ios|web），避免重复造一套等价文案。
const PLATFORM_LABEL_KEYS: Record<string, I18nKey> = {
  windows: 'cloud.platform.windows',
  macos: 'cloud.platform.macos',
  linux: 'cloud.platform.linux',
  android: 'cloud.platform.android',
  ios: 'cloud.platform.ios',
  web: 'cloud.platform.web',
}

function platformLabel(t: (key: I18nKey) => string, platform?: string): string {
  if (!platform) return '—'
  const key = PLATFORM_LABEL_KEYS[platform]
  return key ? t(key) : platform
}

export function DirectDevicesSection() {
  const { t } = useI18n()
  const { data, isLoading, isError, error, refetch } = useQuery({
    queryKey: DEVICES_QUERY_KEY,
    queryFn: () => linkApi.devices().then((r) => r.devices),
    staleTime: 5_000,
    retry: false,
  })

  const unsupported = isError && isLinkUnsupportedError(error)

  return (
    <>
      <p className="mb-1 mt-6 text-[12.5px] font-semibold text-text2">{t('link.sectionTitle')}</p>
      <p className="set-desc" style={{ marginBottom: 10 }}>
        {t('link.sectionDesc')}
      </p>
      {unsupported ? (
        <p className="set-note">{t('link.unsupportedHost')}</p>
      ) : (
        <>
          <PairingCodeGroup />
          <p className="mb-1 mt-6 text-[12.5px] font-semibold text-text2">{t('link.pairedTitle')}</p>
          <p className="set-desc" style={{ marginBottom: 10 }}>
            {t('link.pairedDesc')}
          </p>
          <div className="set-group">
            {isLoading ? (
              <p className="p-4 text-[12px] text-text3">{t('common.loading')}</p>
            ) : isError ? (
              <div className="flex items-center justify-between p-4">
                <p className="text-[12px] text-danger">{t('link.devicesLoadFailed')}</p>
                <button type="button" className="btn ghost sm" onClick={() => void refetch()}>
                  {t('common.retry')}
                </button>
              </div>
            ) : !data || data.length === 0 ? (
              <p className="p-4 text-[12px] text-text3">{t('link.devicesEmpty')}</p>
            ) : (
              data.map((d) => <LocalDeviceItem key={d.fingerprint} device={d} />)
            )}
          </div>
          <AddLocalDeviceDialog />
        </>
      )}
    </>
  )
}

// ---------------------------------------------------------------------------
// 本机配对码：POST /link/code 生成六位一次性码 + 有效期倒计时
// ---------------------------------------------------------------------------

function PairingCodeGroup() {
  const { t } = useI18n()
  const [expiresAt, setExpiresAt] = useState<number | null>(null)
  const [remaining, setRemaining] = useState(0)

  const codeMut = useMutation({
    mutationFn: () => linkApi.generateCode(),
    onSuccess: (res) => setExpiresAt(Date.now() + res.ttlSeconds * 1000),
  })

  useEffect(() => {
    if (!expiresAt) return
    const tick = () => setRemaining(Math.max(0, Math.round((expiresAt - Date.now()) / 1000)))
    tick()
    const id = window.setInterval(tick, 1000)
    return () => window.clearInterval(id)
  }, [expiresAt])

  const code = codeMut.data?.code ?? ''

  return (
    <div className="set-group">
      <SetRow title={t('link.showCodeTitle')} desc={t('link.showCodeDesc')}>
        <button type="button" className="btn ghost sm" disabled={codeMut.isPending} onClick={() => codeMut.mutate()}>
          {codeMut.isPending ? t('common.loading') : t('link.showCode')}
        </button>
      </SetRow>
      {code && (
        <div className="set-row">
          <div className="token-box" style={{ flex: 1, justifyContent: 'center' }}>
            <b className="text-[19px] font-semibold tracking-[4px]">{code}</b>
          </div>
        </div>
      )}
      {code && (
        <p className="px-4 pb-3 text-[11px] text-text3">
          {remaining > 0 ? t('link.codeExpireIn', { s: remaining }) : t('link.codeExpired')}
        </p>
      )}
      {codeMut.isError && <p className="px-4 pb-3 text-[11.5px] text-danger">{friendlyLinkError(t, codeMut.error)}</p>}
    </div>
  )
}

// ---------------------------------------------------------------------------
// 已配对设备行：在线圆点 + 平台图标 + 移除（二次确认）
// ---------------------------------------------------------------------------

function LocalDeviceItem({ device }: { device: LinkDeviceDto }) {
  const { t } = useI18n()
  const qc = useQueryClient()
  const removeMut = useMutation({
    mutationFn: () => linkApi.removeDevice(device.fingerprint),
    onSuccess: () => void qc.invalidateQueries({ queryKey: DEVICES_QUERY_KEY }),
  })

  async function handleRemove() {
    const ok = await confirmDialog({
      title: t('link.removeDeviceTitle'),
      message: t('link.removeDeviceDesc'),
      danger: true,
    })
    if (ok) removeMut.mutate()
  }

  const PlatformIcon = device.platform === 'android' || device.platform === 'ios' ? Smartphone : Monitor

  return (
    <div className="device-item">
      <div className="set-row">
        <div className="grid h-8 w-8 flex-shrink-0 place-items-center rounded-lg bg-surface2 text-text2">
          <PlatformIcon size={15} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <i className={cn('queue-dot', device.online && 'on')} title={device.online ? t('link.online') : t('link.offline')} />
            <b className="truncate text-[13px] font-medium">{device.name || '-'}</b>
          </div>
          <p className="text-[11.5px] text-text3">{platformLabel(t, device.platform)}</p>
        </div>
        <button
          type="button"
          className="icon-btn sm text-text3 hover:text-danger"
          title={t('link.removeDeviceTitle')}
          aria-label={t('link.removeDeviceTitle')}
          disabled={removeMut.isPending}
          onClick={() => void handleRemove()}
        >
          <Trash2 size={14} />
        </button>
        {removeMut.isError && <p className="w-full px-0 text-[11.5px] text-danger">{t('link.removeDeviceFailed')}</p>}
      </div>
    </div>
  )
}

