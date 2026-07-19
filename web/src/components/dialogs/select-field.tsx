// Radix Select 的通用值选择字段封装 —— 抽自 new-download.tsx，供 new-download.tsx /
// manifest-select.tsx 共用（避免同一 Select 外观出现两套实现）。

import * as Select from '@radix-ui/react-select'
import { Check, ChevronDown } from 'lucide-react'

/** Radix Select 不允许 Item 的 value 为空字符串，用哨兵值代表"未设置/默认"语义。 */
const EMPTY_VALUE = '__default__'

export function SelectField({
  value,
  onChange,
  options,
  ariaLabel,
}: {
  value: string
  onChange: (v: string) => void
  options: { value: string; label: string }[]
  ariaLabel: string
}) {
  return (
    <Select.Root value={value === '' ? EMPTY_VALUE : value} onValueChange={(v) => onChange(v === EMPTY_VALUE ? '' : v)}>
      <Select.Trigger className="select w-full" aria-label={ariaLabel}>
        <Select.Value className="min-w-0 flex-1 truncate text-left" />
        <Select.Icon className="shrink-0 text-text3">
          <ChevronDown size={14} />
        </Select.Icon>
      </Select.Trigger>
      <Select.Portal>
        <Select.Content position="popper" sideOffset={6} className="select-pop" style={{ width: 'var(--radix-select-trigger-width)' }}>
          <Select.Viewport className="max-h-64">
            {options.map((o) => (
              <Select.Item key={o.value || EMPTY_VALUE} value={o.value === '' ? EMPTY_VALUE : o.value} className="select-item">
                <Select.ItemText>{o.label}</Select.ItemText>
                <Select.ItemIndicator className="select-item-check">
                  <Check size={14} />
                </Select.ItemIndicator>
              </Select.Item>
            ))}
          </Select.Viewport>
        </Select.Content>
      </Select.Portal>
    </Select.Root>
  )
}
