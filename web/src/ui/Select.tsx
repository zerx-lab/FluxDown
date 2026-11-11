import * as RadixSelect from '@radix-ui/react-select'
import { Check, ChevronDown } from 'lucide-react'
import { cn } from '../lib/cn'
import { Icon } from './Icon'

export interface SelectOption<V extends string = string> {
  value: V
  label: string
  disabled?: boolean
}

/**
 * 下拉选择 = outline 外观 + 下拉箭头，与输入框等高（GPUI 约定）。
 * Radix 不允许空字符串作选项 value：需要「未选」请传 `value=""`，Root 保持受控并显示 placeholder
 * （Radix 2.x 把 `''` 视为占位态；不能映射成 `undefined`，否则 Root 转为非受控，会记住上次的选择）。
 */
export function Select<V extends string>({
  value,
  onValueChange,
  options,
  placeholder,
  disabled,
  id,
  className,
  'aria-label': ariaLabel,
}: {
  value: V | ''
  onValueChange: (value: V) => void
  options: readonly SelectOption<V>[]
  placeholder?: string
  disabled?: boolean
  id?: string
  className?: string
  'aria-label'?: string
}) {
  return (
    <RadixSelect.Root value={value} onValueChange={(next) => onValueChange(next as V)} disabled={disabled}>
      <RadixSelect.Trigger
        id={id}
        aria-label={ariaLabel}
        className={cn(
          'inline-flex h-control w-full min-w-0 items-center justify-between gap-2 rounded-md border border-input bg-transparent px-2.5 text-sm text-foreground transition-colors coarse:min-h-touch',
          'hover:bg-row-hover focus-visible:border-ring data-[placeholder]:text-text-tertiary disabled:cursor-not-allowed disabled:opacity-50',
          className,
        )}
      >
        <span className="min-w-0 flex-1 truncate text-left">
          <RadixSelect.Value placeholder={placeholder} />
        </span>
        <RadixSelect.Icon>
          <Icon icon={ChevronDown} className="text-muted-foreground" />
        </RadixSelect.Icon>
      </RadixSelect.Trigger>
      <RadixSelect.Portal>
        <RadixSelect.Content
          position="popper"
          sideOffset={4}
          collisionPadding={8}
          className="animate-fx-pop z-50 max-h-[min(320px,var(--radix-select-content-available-height))] min-w-[var(--radix-select-trigger-width)] overflow-hidden rounded-md border border-hairline bg-surface shadow-md"
        >
          <RadixSelect.Viewport className="p-1">
            {options.map((option) => (
              <RadixSelect.Item
                key={option.value}
                value={option.value}
                disabled={option.disabled}
                className="relative flex min-h-control cursor-pointer select-none items-center rounded-sm py-1 pr-2 pl-7 text-sm outline-none data-[disabled]:pointer-events-none data-[disabled]:opacity-50 data-[highlighted]:bg-nav-hover coarse:min-h-touch"
              >
                <RadixSelect.ItemIndicator className="absolute left-2">
                  <Icon icon={Check} size="md" />
                </RadixSelect.ItemIndicator>
                <RadixSelect.ItemText>{option.label}</RadixSelect.ItemText>
              </RadixSelect.Item>
            ))}
          </RadixSelect.Viewport>
        </RadixSelect.Content>
      </RadixSelect.Portal>
    </RadixSelect.Root>
  )
}
