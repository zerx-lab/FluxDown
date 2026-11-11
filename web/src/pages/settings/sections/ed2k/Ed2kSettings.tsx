// eD2K：基础（Kad/UPnP/端口/ID 检测）、服务器（列表 + server.met 订阅）、Kad 节点（nodes.dat 订阅）（GPUI `sections/ed2k.rs`）。

import { useT } from '../../../../i18n'
import { DaemonNumberRow, DaemonSwitchRow, SettingsPage, SettingsSection, useDaemonBool } from '../../kit'
import { ListEditorRow, SubscriptionStatusRow } from '../subscription'
import { IdCheckRow, KadBootstrapRow } from './Ed2kProbes'

function BasicTab() {
  const t = useT()
  return (
    <SettingsSection title={t('settingsTabGeneral')}>
      <DaemonSwitchRow configKey="ed2k_enable_kad" titleKey="ed2kEnableKad" descKey="ed2kEnableKadDesc" />
      <DaemonSwitchRow configKey="ed2k_enable_upnp" titleKey="ed2kEnableUpnp" descKey="ed2kEnableUpnpDesc" />
      <DaemonNumberRow configKey="ed2k_listen_port" titleKey="ed2kListenPort" descKey="ed2kListenPortDesc" />
      <IdCheckRow />
    </SettingsSection>
  )
}

function ServersTab() {
  const t = useT()
  const subEnabled = useDaemonBool('ed2k_server_sub_enabled')
  return (
    <SettingsSection title={t('settingsTabServers')}>
      <ListEditorRow configKey="ed2k_server_list" titleKey="ed2kServerList" descKey="ed2kServerListDesc" placeholderKey="ed2kServerPlaceholder" format="comma" />
      <DaemonSwitchRow configKey="ed2k_server_sub_enabled" titleKey="ed2kServerSub" descKey="ed2kServerSubDesc" />
      <ListEditorRow
        configKey="ed2k_server_sub_urls"
        titleKey="ed2kServerSubUrls"
        descKey="ed2kServerSubUrlsDesc"
        placeholderKey="ed2kServerSubPlaceholder"
        format="lines"
        disabled={!subEnabled}
      />
      <SubscriptionStatusRow kind="ed2kServers" />
    </SettingsSection>
  )
}

function KadNodesTab() {
  const t = useT()
  const kadEnabled = useDaemonBool('ed2k_enable_kad')
  return (
    <SettingsSection title={t('settingsTabKadNodes')}>
      <ListEditorRow
        configKey="ed2k_nodes_dat_url"
        titleKey="ed2kNodesSubUrls"
        descKey="ed2kNodesSubUrlsDesc"
        placeholderKey="ed2kNodesSubPlaceholder"
        format="lines"
        disabled={!kadEnabled}
      />
      <SubscriptionStatusRow kind="ed2kNodes" />
      <KadBootstrapRow disabled={!kadEnabled} />
    </SettingsSection>
  )
}

export function Ed2kSettings() {
  const t = useT()
  return (
    <SettingsPage
      title={t('settingsCatEd2k')}
      description={t('settingsCatEd2kDesc')}
      tabs={[
        { id: 'basic', label: t('settingsTabGeneral'), content: <BasicTab /> },
        { id: 'servers', label: t('settingsTabServers'), content: <ServersTab /> },
        { id: 'kad', label: t('settingsTabKadNodes'), content: <KadNodesTab /> },
      ]}
    />
  )
}
