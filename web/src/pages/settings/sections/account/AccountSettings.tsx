// 账户页（GPUI `crates/account`）：未登录 hero → 登录/注册；已登录 profile → 账号与安全 → 设备；始终有云功能。
// 调试用的「服务器地址」卡片不在 Web 出现。数据全部来自快照 `session / cloudDevices / sync`，无本地副本。

import { CircleUser } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../../../../i18n'
import { useAgent, useConnection } from '../../../../lib/rpc'
import { maxOnlineDevices } from '../../../../lib/cloud-presence'
import type { CloudDevice, LinkDeviceInfo, SyncStatusDto } from '../../../../lib/rpc'
import { Button, Card } from '../../../../ui'
import { SettingsPage } from '../../kit'
import { CloudFeaturesCard } from './CloudFeaturesCard'
import { DevicesCard } from './DevicesCard'
import { PairedDevicesCard } from './PairedDevicesCard'
import { LoginDialog, RegisterDialog } from './AuthDialogs'
import { ProfileCard } from './ProfileCard'
import { SecurityCard } from './SecurityCard'

const NO_DEVICES: readonly CloudDevice[] = []
const NO_LINKED: readonly LinkDeviceInfo[] = []
const NO_SYNC: SyncStatusDto = { enabled: false, revision: 0, dirtyKeys: [], lastError: null }

function HeroCard({ disabled }: { disabled: boolean }) {
  const t = useT()
  const [dialog, setDialog] = useState<'login' | 'register' | null>(null)
  return (
    <Card className="flex w-full flex-col items-center p-6 text-center">
      <div className="flex size-14 items-center justify-center rounded-full bg-accent text-accent-text">
        <CircleUser strokeWidth={1.5} className="size-8" />
      </div>
      <div className="mt-4 text-title font-semibold text-foreground">{t('accountLoginDialogTitle')}</div>
      <div className="mt-1 max-w-md text-xs text-muted-foreground">{t('accountHeroSubtitle')}</div>
      <div className="mt-4 flex gap-2">
        <Button variant="primary" disabled={disabled} onClick={() => setDialog('login')}>
          {t('accountLogin')}
        </Button>
        <Button disabled={disabled} onClick={() => setDialog('register')}>
          {t('accountRegister')}
        </Button>
      </div>
      {dialog === 'login' ? <LoginDialog onClose={() => setDialog(null)} /> : null}
      {dialog === 'register' ? <RegisterDialog onClose={() => setDialog(null)} /> : null}
    </Card>
  )
}

export function AccountSettings() {
  const t = useT()
  const session = useAgent((snapshot) => snapshot.session, null)
  const devices = useAgent((snapshot) => snapshot.cloudDevices, NO_DEVICES)
  const linked = useAgent((snapshot) => snapshot.linkedDevices, NO_LINKED)
  const sync = useAgent((snapshot) => snapshot.sync, NO_SYNC)
  const cloudConnection = useAgent((snapshot) => snapshot.cloudConnection, undefined)
  const disabled = useConnection().phase !== 'ready'

  return (
    <SettingsPage title={t('settingsCatAccount')} description={t('settingsCatAccountDesc')}>
      {session ? <ProfileCard session={session} disabled={disabled} /> : <HeroCard disabled={disabled} />}
      {session ? <SecurityCard session={session} disabled={disabled} /> : null}
      {session ? <DevicesCard devices={devices} disabled={disabled} connection={cloudConnection} maxOnline={maxOnlineDevices(session.entitlements)} /> : null}
      <PairedDevicesCard devices={linked} disabled={disabled} />
      <CloudFeaturesCard loggedIn={session !== null} sync={sync} devices={devices} entitlements={session?.entitlements} disabled={disabled} connection={cloudConnection} />
    </SettingsPage>
  )
}
