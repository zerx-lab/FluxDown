// 速率限制控件：数字 + 单位下拉（KB/s / MB/s / GB/s），落库为字节/秒。

import { useState } from 'react'
import { byteUnit, useFileSizeUnits } from '../../../../lib/fileSizeUnits'
import { DropdownField, NumberField, SettingsRow, setDaemonNumber, useDaemonNumber } from '../../kit'
import { RATE_UNITS, RATE_UNIT_FACTOR, RATE_UNIT_STEP, effectiveUnit, switchTarget, toBytes, toDisplay } from './rate'
import type { RateUnit } from './rate'

export function RateLimitRow({ configKey, title, description }: { configKey: string; title: string; description?: string | undefined }) {
  const fileSizeUnits = useFileSizeUnits()
  const unitOptions = RATE_UNITS.map((rateUnit, index) => ({
    value: rateUnit,
    label: `${byteUnit(index + 1, fileSizeUnits)}/s`,
  }))
  const bytes = useDaemonNumber(configKey)
  // 所选单位仅会话内记住（GPUI transient）；外部改值后无法精确表示时自动回退。
  const [chosen, setChosen] = useState<RateUnit | null>(null)
  const unit = effectiveUnit(bytes, chosen)
  return (
    <SettingsRow title={title} description={description}>
      <div className="flex w-full items-center gap-2 desktop:w-auto">
        <NumberField
          key={unit}
          float
          min={0}
          max={Math.floor(Number.MAX_SAFE_INTEGER / RATE_UNIT_FACTOR[unit])}
          step={RATE_UNIT_STEP[unit]}
          value={toDisplay(bytes, unit)}
          aria-label={title}
          className="min-w-0 flex-1 mobile:w-auto"
          onCommit={(next) => setDaemonNumber(configKey, toBytes(next, unit))}
        />
        <DropdownField
          value={unit}
          options={unitOptions}
          aria-label={title}
          className="w-28 shrink-0 mobile:w-28 desktop:w-28 desktop:min-w-0"
          onValueChange={(next) => {
            if (next === unit) return
            setDaemonNumber(configKey, switchTarget(bytes, unit, next))
            setChosen(next)
          }}
        />
      </div>
    </SettingsRow>
  )
}
