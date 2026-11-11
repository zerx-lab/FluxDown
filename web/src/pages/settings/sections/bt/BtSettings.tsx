// BitTorrent：常规（开关 + 新任务默认）、连接（监听 / 传输 / 来源 / 上限 / I2P）、Tracker（列表 + 订阅 + 汇报策略）、
// 做种、防吸血（设置 / 封禁名单 / 事件日志）（GPUI `sections/bt.rs`）。

import { useT } from '../../../../i18n'
import { Tooltip } from '../../../../ui'
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
  DaemonTextRow,
  camel,
  setDaemon,
  setDaemonNumber,
  useDaemonBool,
  useDaemonNumber,
  useDaemonValue,
} from '../../kit'
import { ListEditorRow, SubscriptionStatusRow } from '../subscription'
import { portMappingSegments, portSegmentText } from './portStatus'
import { usePortStatus } from './usePortStatus'
import { GuardBansTab } from './guard/GuardBans'
import { GuardEventsTab } from './guard/GuardEvents'
import { GuardSettingsTab } from './guard/GuardSettings'

function BasicTab({ btEnabled }: { btEnabled: boolean }) {
  const t = useT()
  return (
    <>
      <SettingsSection title={t('settingsTabGeneral')}>
        <DaemonSwitchRow configKey="bt_enabled" titleKey="btEnabled" descKey="btEnabledDesc" />
      </SettingsSection>
      {btEnabled ? (
        <SettingsSection title={t('btNewTasksSection')}>
          <DaemonEnumRow configKey="bt_piece_order" titleKey="btPieceOrder" descKey="btPieceOrderDesc" labelPrefix="btPieceOrder" />
          <DaemonSwitchRow
            configKey="bt_first_last_pieces_first"
            titleKey="btFirstLastPiecesFirstDefault"
            descKey="btFirstLastPiecesFirstDefaultDesc"
          />
          <DaemonSwitchRow
            configKey="bt_selection_auto_default"
            titleKey="btSelectionAutoDefault"
            descKey="btSelectionAutoDefaultDesc"
          />
        </SettingsSection>
      ) : null}
    </>
  )
}

/** 端口映射状态行：快照由本行挂载期间（「连接」页签可见且 UPnP 开）轮询；尚无快照时行内留空。 */
function PortMappingRow() {
  const t = useT()
  const status = usePortStatus()
  const segments = status ? portMappingSegments(status) : []
  return (
    <SettingsRow title={t('btPortMappingStatus')} vertical>
      <div className="flex min-w-0 flex-wrap gap-x-3 gap-y-0.5">
        {segments.map((segment, index) => {
          const { text, full } = portSegmentText(t, segment)
          const label = <span className="block max-w-[320px] min-w-0 truncate text-xs text-muted-foreground">{text}</span>
          return full === null ? (
            <span key={index} className="min-w-0">{label}</span>
          ) : (
            <Tooltip key={index} content={<span className="whitespace-pre-line break-words">{full}</span>}>
              <span className="min-w-0" title={full}>{label}</span>
            </Tooltip>
          )
        })}
      </div>
    </SettingsRow>
  )
}

/** 连接：监听、传输、来源、连接上限、I2P。除「连接上限」外的改动都会重建 BT 会话，提示挂在第一组。 */
function ConnectionTab() {
  const t = useT()
  const upnp = useDaemonBool('bt_enable_upnp')
  const utp = useDaemonBool('bt_enable_utp')
  const i2p = useDaemonBool('bt_i2p_enabled')
  return (
    <>
      <SettingsSection title={t('btSectionListen')} subtitle={t('btSettingsRestartHint')}>
        <DaemonNumberRow configKey="bt_port_start" titleKey="btListenPortStart" descKey="btListenPortDesc" />
        <DaemonNumberRow configKey="bt_port_end" titleKey="btListenPortEnd" />
        <DaemonSwitchRow configKey="bt_enable_upnp" titleKey="btEnableUpnp" descKey="btEnableUpnpDesc" />
        {upnp ? <PortMappingRow /> : null}
      </SettingsSection>
      <SettingsSection title={t('btSectionTransport')}>
        <DaemonSwitchRow configKey="bt_enable_utp" titleKey="btEnableUtp" descKey="btEnableUtpDesc" />
        {utp ? (
          <DaemonEnumRow configKey="bt_utp_congestion" titleKey="btUtpCongestion" descKey="btUtpCongestionDesc" labelPrefix="btUtpCongestion" />
        ) : null}
        <DaemonEnumRow configKey="bt_mse_mode" titleKey="btMseMode" descKey="btMseModeDesc" labelPrefix="btMseMode" />
        <DaemonNumberRow configKey="bt_upload_slots" titleKey="btUploadSlots" descKey="btUploadSlotsDesc" />
      </SettingsSection>
      <SettingsSection title={t('btSectionSources')}>
        <DaemonSwitchRow configKey="bt_enable_dht" titleKey="btEnableDht" descKey="btEnableDhtDesc" />
        <DaemonSwitchRow configKey="bt_enable_lsd" titleKey="btEnableLsd" descKey="btEnableLsdDesc" />
        <DaemonSwitchRow configKey="bt_enable_webseed" titleKey="btEnableWebseed" descKey="btEnableWebseedDesc" />
      </SettingsSection>
      <SettingsSection title={t('btSectionLimits')}>
        <DaemonNumberRow configKey="bt_max_connections" titleKey="btMaxConnections" descKey="btMaxConnectionsDesc" />
        <DaemonNumberRow
          configKey="bt_max_connections_per_torrent"
          titleKey="btMaxConnectionsPerTorrent"
          descKey="btMaxConnectionsPerTorrentDesc"
        />
      </SettingsSection>
      <SettingsSection title={t('btSectionI2p')}>
        <DaemonSwitchRow configKey="bt_i2p_enabled" titleKey="btI2pEnabled" descKey="btI2pEnabledDesc" />
        {i2p ? (
          <>
            <DaemonTextRow configKey="bt_i2p_sam_host" titleKey="btI2pSamHost" />
            <DaemonNumberRow configKey="bt_i2p_sam_port" titleKey="btI2pSamPort" />
            <DaemonSwitchRow configKey="bt_i2p_mixed" titleKey="btI2pMixed" descKey="btI2pMixedDesc" />
          </>
        ) : null}
      </SettingsSection>
    </>
  )
}

function TrackerTab() {
  const t = useT()
  const subEnabled = useDaemonBool('bt_tracker_sub_enabled')
  return (
    <>
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
      <SettingsSection title={t('btSectionAnnounce')}>
        <DaemonSwitchRow configKey="bt_announce_all_tiers" titleKey="btAnnounceAllTiers" descKey="btAnnounceAllTiersDesc" />
        <DaemonSwitchRow configKey="bt_announce_all_trackers" titleKey="btAnnounceAllTrackers" descKey="btAnnounceAllTrackersDesc" />
      </SettingsSection>
    </>
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
      { id: 'connection', label: t('settingsTabConnection'), content: <ConnectionTab /> },
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
