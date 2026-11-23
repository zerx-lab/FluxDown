import { useState } from 'react'
import { useT } from '../../../../i18n'
import { daemonConfigField } from '../../../../lib/rpc'
import {
  DropdownField,
  NumberField,
  SettingsRow,
  optionalText,
  setDaemonNumber,
  useDaemonNumber,
  useSettingsReadOnly,
} from '../../kit'
import { RETENTION_CUSTOM, retentionCustomTarget, retentionModeId } from './retention'

const KEY = 'completed_record_retention_days'

/** 已完成记录保留天数：预设下拉 + 「自定义」天数输入（GPUI sections/retention.rs）。 */
export function RetentionRow() {
  const t = useT()
  const value = useDaemonNumber(KEY)
  const readOnly = useSettingsReadOnly()
  // 会话内「自定义模式」标记：值恰好等于预设时仍留在自定义态，避免输入中途控件切换。
  const [customFlag, setCustomFlag] = useState(false)
  const max = daemonConfigField(KEY)?.max ?? 3650
  const title = t('completedRecordRetention')
  const mode = retentionModeId(value, customFlag)
  return (
    <SettingsRow title={title} description={optionalText(t, 'completedRecordRetentionDesc')}>
      <div className="flex w-full flex-wrap items-center gap-2 desktop:w-auto desktop:justify-end">
        <DropdownField
          value={mode}
          options={[
            { value: '0', label: t('retentionNever') },
            { value: '-1', label: t('retentionImmediate') },
            { value: '1', label: t('retentionDay1') },
            { value: '7', label: t('retentionDay7') },
            { value: '30', label: t('retentionDay30') },
            { value: RETENTION_CUSTOM, label: t('retentionCustom') },
          ]}
          aria-label={title}
          disabled={readOnly}
          className="mobile:flex-1"
          onValueChange={(next) => {
            if (readOnly || next === mode) return
            if (next === RETENTION_CUSTOM) {
              setCustomFlag(true)
              setDaemonNumber(KEY, retentionCustomTarget(value, max))
            } else {
              setCustomFlag(false)
              setDaemonNumber(KEY, Number(next))
            }
          }}
        />
        {mode === RETENTION_CUSTOM ? (
          <>
            <NumberField
              value={value}
              min={1}
              max={max}
              aria-label={`${title} · ${t('retentionCustom')}`}
              disabled={readOnly}
              className="mobile:flex-1"
              onCommit={(next) => {
                if (!readOnly) setDaemonNumber(KEY, next)
              }}
            />
            <span className="text-sm text-muted-foreground">{t('retentionDaysUnit')}</span>
          </>
        ) : null}
      </div>
    </SettingsRow>
  )
}
