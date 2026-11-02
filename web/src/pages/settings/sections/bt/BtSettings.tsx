// BitTorrent：基础（DHT/UPnP/端口）、Tracker（列表 + 订阅）、做种、防吸血（设置 / 封禁名单 / 事件日志）（GPUI `sections/bt.rs`）。

import { useT } from '../../../../i18n'
import { daemonConfigField } from '../../../../lib/rpc'
import {
  DaemonEnumRow,
  DaemonNumberRow,
  DaemonSwitchRow,
  DropdownField,
  NumberField,
  SettingsPage,
  SettingsRow,
  SettingsSection,
  camel,
  setDaemon,
  setDaemonNumber,
  useDaemonBool,
  useDaemonNumber,
  useDaemonValue,
} from '../../kit'
import { ListEditorRow, SubscriptionStatusRow } from '../subscription'
import { GuardBansTab } from './guard/GuardBans'
import { GuardEventsTab } from './guard/GuardEvents'
import { GuardSettingsTab } from './guard/GuardSettings'

function BasicTab({ btEnabled }: { btEnabled: boolean }) {
  const t = useT()
  return (
    <SettingsSection title={t('settingsTabGeneral')} subtitle={t('btSettingsRestartHint')}>
      <DaemonSwitchRow configKey="bt_enabled" titleKey="btEnabled" descKey="btEnabledDesc" />
      {btEnabled ? (
        <>
          <DaemonSwitchRow configKey="bt_enable_dht" titleKey="btEnableDht" descKey="btEnableDhtDesc" />
          <DaemonSwitchRow configKey="bt_enable_upnp" titleKey="btEnableUpnp" descKey="btEnableUpnpDesc" />
          <DaemonSwitchRow configKey="bt_enable_utp" titleKey="btEnableUtp" descKey="btEnableUtpDesc" />
          <DaemonNumberRow configKey="bt_port_start" titleKey="btListenPortStart" descKey="btListenPortDesc" />
          <DaemonNumberRow configKey="bt_port_end" titleKey="btListenPortEnd" />
          <DaemonEnumRow configKey="bt_mse_mode" titleKey="btMseMode" descKey="btMseModeDesc" labelPrefix="btMseMode" />
        </>
      ) : null}
    </SettingsSection>
  )
}

function TrackerTab() {
  const t = useT()
  const subEnabled = useDaemonBool('bt_tracker_sub_enabled')
  return (
    <SettingsSection title={t('settingsTabTracker')}>
      <ListEditorRow configKey="bt_custom_trackers" titleKey="btTrackerList" descKey="btTrackerListDesc" placeholderKey="btTrackerPlaceholder" format="lines" />
      <DaemonSwitchRow configKey="bt_tracker_sub_enabled" titleKey="btTrackerSub" descKey="btTrackerSubDesc" />
      <ListEditorRow
        configKey="bt_tracker_sub_urls"
        titleKey="btTrackerSubUrls"
        descKey="btTrackerSubUrlsDesc"
        placeholderKey="btTrackerSubPlaceholder"
        format="lines"
        disabled={!subEnabled}
      />
      <SubscriptionStatusRow kind="btTrackers" />
    </SettingsSection>
  )
}

function ThenActionRow() {
  const t = useT()
  const value = useDaemonValue('bt_seed_then_action')
  const options = [
    { value: 'stop', label: t('btSeedStopSeeding') },
    { value: 'delete', label: t('btSeedDeleteTask') },
    { value: 'delete_files', label: t('btSeedDeleteTaskAndFiles') },
  ]
  const title = t('btSeedThenAction')
  return (
    <SettingsRow title={title}>
      <DropdownField value={value} options={options} aria-label={title} onValueChange={(next) => setDaemon('bt_seed_then_action', next)} />
    </SettingsRow>
  )
}

/** 时长限制：数字 + 单位下拉同一行（GPUI `duration_control`）。 */
function DurationRow({ valueKey, unitKey, titleKey }: { valueKey: string; unitKey: string; titleKey: string }) {
  const t = useT()
  const title = t(titleKey)
  const value = useDaemonNumber(valueKey)
  const unit = useDaemonValue(unitKey)
  const field = daemonConfigField(valueKey)
  const unitOptions = (daemonConfigField(unitKey)?.options ?? []).map((option) => ({ value: option, label: t(`timeUnit${camel(option)}`) }))
  return (
    <SettingsRow title={title}>
      <div className="flex items-center gap-2 mobile:w-full">
        <NumberField
          value={value}
          {...(field?.min !== undefined ? { min: field.min } : {})}
          {...(field?.max !== undefined ? { max: field.max } : {})}
          aria-label={title}
          onCommit={(next) => setDaemonNumber(valueKey, next)}
        />
        <DropdownField value={unit} options={unitOptions} aria-label={title} className="desktop:min-w-[88px] mobile:w-auto" onValueChange={(next) => setDaemon(unitKey, next)} />
      </div>
    </SettingsRow>
  )
}

function SeedingTab() {
  const t = useT()
  const seedEnabled = useDaemonBool('bt_seed_enabled')
  const operatorOptions = [
    { value: 'or', label: t('btSeedOperatorOr') },
    { value: 'and', label: t('btSeedOperatorAnd') },
  ]
  const operator = useDaemonValue('bt_seed_limit_operator')
  return (
    <SettingsSection title={t('settingsTabSeeding')}>
      <DaemonSwitchRow configKey="bt_seed_enabled" titleKey="btSeedEnabled" descKey="btSeedEnabledDesc" />
      {seedEnabled ? (
        <>
          <DaemonNumberRow configKey="bt_seed_max_active" titleKey="btSeedMaxActive" descKey="btSeedMaxActiveDesc" />
          <DaemonSwitchRow configKey="bt_auto_reseed" titleKey="btAutoReseed" descKey="btAutoReseedDesc" />
          <DaemonNumberRow configKey="bt_seed_ratio_limit" titleKey="btSeedRatioLimit" step={0.1} unit={t('btSeedRatioUnit')} />
          <DaemonNumberRow configKey="bt_seed_post_ratio_limit" titleKey="btSeedPostRatioLimit" step={0.1} unit={t('btSeedRatioUnit')} />
          <DurationRow valueKey="bt_seed_time_limit_minutes" unitKey="bt_seed_time_limit_unit" titleKey="btSeedTimeLimit" />
          <DurationRow valueKey="bt_seed_inactive_time_limit_minutes" unitKey="bt_seed_inactive_time_limit_unit" titleKey="btSeedInactiveTimeLimit" />
          <SettingsRow title={t('btSeedConditionsOperator')}>
            <DropdownField
              value={operator}
              options={operatorOptions}
              aria-label={t('btSeedConditionsOperator')}
              onValueChange={(next) => setDaemon('bt_seed_limit_operator', next)}
            />
          </SettingsRow>
          <ThenActionRow />
        </>
      ) : null}
    </SettingsSection>
  )
}

export function BtSettings() {
  const t = useT()
  const btEnabled = useDaemonBool('bt_enabled')
  const tabs = [
    { id: 'basic', label: t('settingsTabGeneral'), content: <BasicTab btEnabled={btEnabled} /> },
  ]
  if (btEnabled) {
    tabs.push(
      { id: 'tracker', label: t('settingsTabTracker'), content: <TrackerTab /> },
      { id: 'seeding', label: t('settingsTabSeeding'), content: <SeedingTab /> },
      { id: 'guard', label: t('btGuardTab'), content: <GuardSettingsTab /> },
      { id: 'guardBans', label: t('btGuardTabBans'), content: <GuardBansTab /> },
      { id: 'guardEvents', label: t('btGuardTabEvents'), content: <GuardEventsTab /> },
    )
  }
  return (
    <SettingsPage
      title={t('settingsCatBt')}
      description={t('settingsCatBtDesc')}
      tabs={tabs}
    />
  )
}
