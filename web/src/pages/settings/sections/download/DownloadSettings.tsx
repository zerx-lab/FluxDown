// 下载设置（GPUI crates/settings/src/sections/download.rs）。

import { useCallback, useEffect, useState } from 'react'
import { useT } from '../../../../i18n'
import { HASH_ALGORITHM_LABELS } from '../../../../lib/fileHash'
import { LATER_QUEUE_ID, MAIN_QUEUE_ID, rpc, useDaemon } from '../../../../lib/rpc'
import type { QueueDto } from '../../../../lib/rpc'
import { rpcErrorText } from '../../../../lib/rpcErrorText'
import { ThreadsRiskCallout } from '../../../../lib/ThreadsRiskCallout'
import { Button, Switch, Textarea, toast } from '../../../../ui'
import {
  DaemonEnumRow,
  DaemonNumberRow,
  DaemonSwitchRow,
  DropdownField,
  PrefSwitchRow,
  SettingsPage,
  SettingsRow,
  SettingsSection,
  TextField,
  optionalText,
  setDaemon,
  setDaemonBool,
  useDaemonNumber,
  useDaemonPending,
  useDaemonValue,
  useDaemonBool,
  usePrefBoolean,
  usePrefRaw,
  setPref,
  useSettingsReadOnly,
} from '../../kit'
import { CATEGORY_SAVE_DIR_HISTORY_PREF, SAVE_DIR_HISTORY_ENABLED_PREF, SAVE_DIR_HISTORY_PREF, readCategorySaveDirHistory, readSaveDirHistory } from '../../../../lib/saveDirHistory'
import { RateLimitRow } from './RateLimitField'
import { DirPickerDialog } from './DirPickerDialog'
import { UserAgentRow } from './UserAgentField'
import { AutoCountRow } from './AutoCountRow'
import { RetentionRow } from './RetentionRow'
import { CategoriesGroup } from './CategoriesGroup'

const NO_QUEUES: readonly QueueDto[] = []
const selectQueues = (daemon: { queues: readonly QueueDto[] }) => daemon.queues
/** Radix Select 不允许空值项：用哨兵表示「默认队列」（`default_queue_id` = ''）。 */
const DEFAULT_QUEUE = '__default__'

export function DownloadSettings() {
  const t = useT()
  const readOnly = useSettingsReadOnly()
  return (
    <SettingsPage title={t('settingsCatDownload')} description={t('settingsCatDownloadDesc')}>
      <SaveLocationSection />
      <BehaviorSection />
      <ConnectionSection />
      <RetrySection />
      <SettingsSection title={t('settingsGroupAdvanced')}>
        <UserAgentRow />
      </SettingsSection>
      <CategoriesGroup disabled={readOnly} />
    </SettingsPage>
  )
}

function SaveLocationSection() {
  const t = useT()
  const dir = useDaemonValue('default_save_dir')
  const [picking, setPicking] = useState(false)
  const title = t('defaultSaveDir')
  return (
    <SettingsSection title={t('settingsGroupSaveLocation')}>
      <SettingsRow title={title} description={optionalText(t, 'defaultSaveDirDesc')}>
        <div className="flex w-full items-center gap-2 desktop:w-[420px]">
          <TextField value={dir} aria-label={title} className="min-w-0 flex-1 desktop:w-auto" onCommit={(next) => setDaemon('default_save_dir', next)} />
          <Button variant="outline" className="shrink-0" onClick={() => setPicking(true)}>
            {t('browse')}
          </Button>
        </div>
      </SettingsRow>
      <DirPickerDialog open={picking} onOpenChange={setPicking} initialPath={dir} onPick={(path) => setDaemon('default_save_dir', path)} />
      <PrefSwitchRow prefKey="download.remember_last_save_dir" fallback={false} titleKey="rememberLastSaveDir" descKey="rememberLastSaveDirDesc" />
      <PrefSwitchRow prefKey={SAVE_DIR_HISTORY_ENABLED_PREF} fallback titleKey="saveDirHistory" descKey="saveDirHistoryDesc" />
      <ClearSaveDirHistoryRow />
    </SettingsSection>
  )
}

/** 清空新建下载的保存目录历史与分类目录记忆（设备本地偏好，写 null 移除）；两者都没有记录时按钮置灰。 */
function ClearSaveDirHistoryRow() {
  const t = useT()
  const readOnly = useSettingsReadOnly()
  const historyEmpty = readSaveDirHistory(usePrefRaw(SAVE_DIR_HISTORY_PREF)).length === 0
  const categoryEmpty = Object.keys(readCategorySaveDirHistory(usePrefRaw(CATEGORY_SAVE_DIR_HISTORY_PREF))).length === 0
  const clear = () => {
    setPref(SAVE_DIR_HISTORY_PREF, null)
    setPref(CATEGORY_SAVE_DIR_HISTORY_PREF, null)
  }
  return (
    <SettingsRow title={t('clearSaveDirHistory')} description={optionalText(t, 'clearSaveDirHistoryDesc')}>
      <Button variant="outline" className="shrink-0" disabled={readOnly || (historyEmpty && categoryEmpty)} onClick={clear}>
        {t('clearSaveDirHistoryAction')}
      </Button>
    </SettingsRow>
  )
}

function queueLabel(t: ReturnType<typeof useT>, queue: QueueDto): string {
  if (queue.queueId === MAIN_QUEUE_ID) return t('mainQueue')
  if (queue.queueId === LATER_QUEUE_ID) return t('laterQueue')
  return queue.name !== '' ? queue.name : queue.queueId
}

function DefaultQueueRow() {
  const t = useT()
  const queues = useDaemon(selectQueues, NO_QUEUES)
  const current = useDaemonValue('default_queue_id')
  const title = t('defaultQueueSetting')
  const options = [
    { value: DEFAULT_QUEUE, label: t('defaultQueue') },
    ...queues.map((queue) => ({ value: queue.queueId, label: queueLabel(t, queue) })),
  ]
  return (
    <SettingsRow title={title} description={optionalText(t, 'defaultQueueSettingDesc')}>
      <DropdownField
        value={current === '' ? DEFAULT_QUEUE : current}
        options={options}
        aria-label={title}
        onValueChange={(next) => setDaemon('default_queue_id', next === DEFAULT_QUEUE ? '' : next)}
      />
    </SettingsRow>
  )
}

function BehaviorSection() {
  const t = useT()
  const silent = usePrefBoolean('download.silent_download', false)
  return (
    <SettingsSection title={t('settingsGroupBehavior')}>
      <PrefSwitchRow prefKey="download.silent_download" fallback={false} titleKey="silentDownload" descKey="silentDownloadDesc" />
      {silent ? (
        <>
          <PrefSwitchRow prefKey="download.silent_auto_category" fallback={true} titleKey="silentAutoCategory" descKey="silentAutoCategoryDesc" />
          <PrefSwitchRow prefKey="download.silent_skip_selection" fallback={false} titleKey="silentSkipSelection" descKey="silentSkipSelectionDesc" />
        </>
      ) : null}
      <DaemonSwitchRow configKey="use_server_time" titleKey="useServerTime" descKey="useServerTimeDesc" />
      <DaemonEnumRow configKey="file_exists_behavior" titleKey="fileExistsBehavior" descKey="fileExistsBehaviorDesc" labelPrefix="fileExists" />
      <DaemonEnumRow configKey="file_missing_action" titleKey="fileMissingAction" descKey="fileMissingActionDesc" labelPrefix="fileMissing" />
      <RetentionRow />
      <DaemonSwitchRow configKey="idle_file_scan" titleKey="idleFileScan" descKey="idleFileScanDesc" />
      <DaemonSwitchRow configKey="hash_after_download" titleKey="hashAfterDownload" descKey="hashAfterDownloadDesc" />
      <DaemonEnumRow configKey="hash_algorithm" titleKey="hashAlgorithm" descKey="hashAlgorithmDesc" labels={HASH_ALGORITHM_LABELS} />
      <DefaultQueueRow />
    </SettingsSection>
  )
}

function ConnPolicyRow() {
  const t = useT()
  const readOnly = useSettingsReadOnly()
  const [count, setCount] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    let cancelled = false
    rpc.daemon.config
      .connPolicy()
      .then((summary) => {
        if (!cancelled) setCount(summary.domainCount)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [])
  const clear = useCallback(async () => {
    setBusy(true)
    try {
      const summary = await rpc.daemon.config.clearConnPolicy()
      setCount(summary.domainCount)
    } catch (err) {
      toast.error(rpcErrorText(err, t))
    } finally {
      setBusy(false)
    }
  }, [t])
  const shown = count ?? 0
  return (
    <SettingsRow title={t('connPolicyCache')} description={optionalText(t, 'connPolicyCacheDesc')}>
      <div className="flex w-full items-center gap-3 mobile:justify-between desktop:w-auto">
        <span className="tabular text-xs text-muted-foreground">{shown === 0 ? t('connPolicyCacheEmpty') : String(shown)}</span>
        <Button variant="outline" loading={busy} disabled={busy || shown === 0 || readOnly} onClick={() => void clear()}>
          {t('connPolicyCacheClear')}
        </Button>
      </div>
    </SettingsRow>
  )
}

function ConnectionRulesRow() {
  const t = useT()
  const value = useDaemonValue('connection_rules')
  const pending = useDaemonPending('connection_rules')
  const readOnly = useSettingsReadOnly()
  const [draft, setDraft] = useState<string | null>(null)
  const [submitted, setSubmitted] = useState<string | null>(null)
  useEffect(() => {
    if (submitted === null || pending || readOnly) return
    if (value === submitted) setDraft(null)
    setSubmitted(null)
  }, [pending, readOnly, submitted, value])
  const disabled = readOnly || pending
  const save = () => {
    if (draft === null || disabled) return
    setSubmitted(draft.trim())
    setDaemon('connection_rules', draft)
  }
  return (
    <SettingsRow title={t('connectionRules')} description={t('connectionRulesDesc')} vertical>
      <div className="flex w-full min-w-0 flex-col gap-2">
        <Textarea
          aria-label={t('connectionRules')}
          value={draft ?? value}
          placeholder={t('connectionRulesPlaceholder')}
          rows={5}
          spellCheck={false}
          autoComplete="off"
          disabled={disabled}
          className="w-full font-mono"
          onChange={(event) => setDraft(event.target.value)}
        />
        <div className="flex justify-end gap-2">
          <Button variant="outline" disabled={disabled || draft === null} onClick={() => setDraft(null)}>
            {t('cancel')}
          </Button>
          <Button loading={pending} disabled={disabled || draft === null || draft.trim() === value} onClick={save}>
            {t('proxyRuleSave')}
          </Button>
        </div>
      </div>
    </SettingsRow>
  )
}

function ConnectionSection() {
  const t = useT()
  const autoSegments = useDaemonNumber('default_segments') === 0
  const cdnMulti = useDaemonBool('cdn_multi_enabled')
  return (
    <SettingsSection title={t('settingsGroupConnection')}>
      <AutoCountRow
        configKey="default_segments"
        customDefault={16}
        titleKey="defaultThreads"
        descKey="defaultThreadsDesc"
        footer={(segments) => <ThreadsRiskCallout segments={segments} contentClassName="pb-3" />}
      />
      {autoSegments ? <DaemonNumberRow configKey="auto_max_connections" titleKey="autoMaxConnections" descKey="autoMaxConnectionsDesc" /> : null}
      <DaemonSwitchRow configKey="cdn_multi_enabled" titleKey="cdnMultiEnabled" descKey="cdnMultiEnabledDesc" />
      {cdnMulti ? <AutoCountRow configKey="cdn_max_nodes" customDefault={2} titleKey="cdnMaxNodes" descKey="cdnMaxNodesDesc" /> : null}
      <MultiNicRow />
      <ConnectionRulesRow />
      <ConnPolicyRow />
      <DaemonNumberRow configKey="max_concurrent_tasks" titleKey="maxConcurrent" descKey="maxConcurrentDesc" />
      <RateLimitRow configKey="speed_limit_bytes" title={t('speedLimit')} description={optionalText(t, 'speedLimitDesc')} />
      <RateLimitRow configKey="upload_limit_bytes" title={t('uploadLimit')} description={optionalText(t, 'uploadLimitDesc')} />
    </SettingsSection>
  )
}

function MultiNicRow() {
  const t = useT()
  const title = t('multiNicEnabled')
  const checked = useDaemonBool('multi_nic_enabled')
  return (
    <SettingsRow title={title} description={optionalText(t, 'multiNicEnabledDesc')} help={`${t('multiNicHelpTitle')}\n${t('multiNicHelp')}`} compact>
      <Switch checked={checked} onCheckedChange={(next) => setDaemonBool('multi_nic_enabled', next)} aria-label={title} />
    </SettingsRow>
  )
}

function RetrySection() {
  const t = useT()
  return (
    <SettingsSection title={t('settingsGroupRetry')}>
      <DaemonNumberRow configKey="max_auto_retries" titleKey="autoRetryCount" descKey="autoRetryCountDesc" />
      <DaemonNumberRow configKey="auto_retry_delay_secs" titleKey="autoRetryDelay" descKey="autoRetryDelayDesc" />
      <DaemonSwitchRow configKey="auto_resume_on_start" titleKey="autoResumeOnStart" descKey="autoResumeOnStartDesc" />
    </SettingsSection>
  )
}
