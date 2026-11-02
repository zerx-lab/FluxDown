// 通用（crates/settings/src/sections/general.rs）：Web 只保留可用部分——
// 启动与托盘 / 剪贴板监听 / 文件与协议关联 / 保持唤醒是桌面专属，不出现在 Web。

import { useT } from '../../../../i18n'
import { useAgent } from '../../../../lib/rpc'
import { Switch } from '../../../../ui'
import { mergeDevices } from '../../../downloads/model/devices'
import { PrefSwitchRow, SettingsPage, SettingsRow, SettingsSection, setPref, usePrefRaw } from '../../kit'

/** `ui.show_sidebar_devices` 三态：未设置 = 登录或有其他设备（云 + 独立直连）时自动显示；开关显示有效值。 */
function SidebarDevicesRow() {
  const t = useT()
  const raw = usePrefRaw('ui.show_sidebar_devices')
  const loggedIn = useAgent((snapshot) => snapshot.session !== null && snapshot.session !== undefined, false)
  const hasOtherDevices = useAgent(
    (snapshot) => mergeDevices(snapshot.cloudDevices, snapshot.linkedDevices, { cloud: true, local: true }).length > 0,
    false,
  )
  const checked = typeof raw === 'boolean' ? raw : loggedIn || hasOtherDevices
  return (
    <SettingsRow title={t('showSidebarDevice')} description={t('showSidebarDeviceDesc')} compact>
      <Switch checked={checked} onCheckedChange={(next) => setPref('ui.show_sidebar_devices', next)} aria-label={t('showSidebarDevice')} />
    </SettingsRow>
  )
}

export function GeneralSettings() {
  const t = useT()
  return (
    <SettingsPage title={t('settingsCatGeneral')} description={t('settingsCatGeneralDesc')}>
      <SettingsSection title={t('settingsGroupSystem')}>
        <PrefSwitchRow prefKey="analytics_enabled" fallback titleKey="analyticsEnabled" descKey="analyticsEnabledDesc" />
      </SettingsSection>
      <SettingsSection title={t('sidebarVisibility')} subtitle={t('sidebarVisibilityDesc')}>
        <PrefSwitchRow prefKey="ui.show_sidebar_status" fallback titleKey="showSidebarStatus" descKey="showSidebarStatusDesc" />
        <PrefSwitchRow prefKey="ui.show_sidebar_queues" fallback titleKey="showSidebarQueues" descKey="showSidebarQueuesDesc" />
        <PrefSwitchRow prefKey="ui.show_sidebar_category" fallback titleKey="showSidebarCategory" descKey="showSidebarCategoryNestedDesc" />
        <SidebarDevicesRow />
      </SettingsSection>
      <SettingsSection title={t('activityBarSection')} subtitle={t('activityBarSectionDesc')}>
        <PrefSwitchRow prefKey="ui.show_activity_rss" fallback titleKey="showActivityRss" descKey="showActivityRssDesc" />
        <PrefSwitchRow prefKey="ui.show_activity_webhooks" fallback titleKey="showActivityWebhooks" descKey="showActivityWebhooksDesc" />
        <PrefSwitchRow prefKey="ui.show_activity_account" fallback titleKey="showActivityAccount" descKey="showActivityAccountDesc" />
      </SettingsSection>
    </SettingsPage>
  )
}
