// 扩展页共用的几个版式小积木。

import { Search, X } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import type { ReactNode } from 'react'
import { useT } from '../../../../i18n'
import { cn } from '../../../../lib/cn'
import { Button, Card, Icon, Input, Tooltip } from '../../../../ui'
import { safeHttpUrl } from './logic'

/** 图标按钮：桌面悬浮提示，触屏靠 aria-label；命中区随指针档位放大。次要图标灰，`destructive` 悬浮变危险色。 */
export function IconButton({
  icon,
  label,
  onClick,
  disabled,
  loading,
  destructive = false,
}: {
  icon: LucideIcon
  label: string
  onClick: () => void
  disabled?: boolean
  loading?: boolean
  destructive?: boolean
}) {
  return (
    <Tooltip content={label}>
      <Button
        variant="ghost"
        iconOnly
        aria-label={label}
        onClick={onClick}
        disabled={disabled}
        loading={loading}
        className={cn('text-muted-foreground', destructive ? 'hover:text-destructive' : 'hover:text-foreground')}
      >
        {loading ? null : <Icon icon={icon} size="md" />}
      </Button>
    </Tooltip>
  )
}

/** 外链：只放行 http(s)，其余原样当文本展示；长 URL 截断而不撑破布局。 */
export function ExtLink({ href, className }: { href: string; className?: string }) {
  const safe = safeHttpUrl(href)
  if (!safe) return <span className={cn('min-w-0 truncate text-xs text-muted-foreground', className)}>{href}</span>
  return (
    <a
      href={safe}
      target="_blank"
      rel="noopener noreferrer"
      className={cn('min-w-0 max-w-full truncate text-xs text-accent-text hover:underline', className)}
    >
      {href}
    </a>
  )
}

/** 列表卡片：一张 surface 卡片内纵向排列各行，行间 hairline 分隔。 */
export function ListCard({ children }: { children: ReactNode }) {
  return <Card className="flex w-full flex-col overflow-hidden [&>*+*]:border-t [&>*+*]:border-hairline">{children}</Card>
}

/** 市场式搜索框（公开市场与专属插件区共用）：搜索图标 / 清除按钮；`onChange` 收到清除后的空串。 */
export function MarketSearchInput({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  const t = useT()
  return (
    <Input
      inputMode="search"
      className="coarse:pr-12"
      value={value}
      placeholder={t('marketSearchPlaceholder')}
      aria-label={t('marketSearchPlaceholder')}
      onChange={(event) => onChange(event.target.value)}
      trailing={
        value === '' ? (
          <span className="pr-2 text-text-tertiary">
            <Icon icon={Search} size="md" />
          </span>
        ) : (
          <Button variant="ghost" iconOnly className="text-muted-foreground hover:text-foreground" aria-label={t('componentsManualPathClear')} onClick={() => onChange('')}>
            <Icon icon={X} size="md" />
          </Button>
        )
      }
    />
  )
}

/** 列表行：信息块吃满剩余宽度，操作区在窄屏自动换到下一行。 */
export function ListRow({ info, actions }: { info: ReactNode; actions: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-x-2 gap-y-2 px-3 py-2 hover:bg-row-hover">
      <div className="flex min-w-0 flex-[1_1_14rem] flex-col gap-0.5">{info}</div>
      <div className="ml-auto flex shrink-0 items-center gap-1">{actions}</div>
    </div>
  )
}

/** 区块标题 + 可选说明（标题 sm 半粗，说明 xs 二级色）。 */
export function BlockTitle({ title, description, trailing }: { title: ReactNode; description?: ReactNode; trailing?: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-2">
      <div className="flex min-w-0 flex-col gap-0.5">
        <div className="text-sm font-medium text-foreground">{title}</div>
        {description ? <div className="text-xs text-muted-foreground">{description}</div> : null}
      </div>
      {trailing}
    </div>
  )
}
