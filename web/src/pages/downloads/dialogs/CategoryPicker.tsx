// 新建下载「保存目录」行左侧的分类下拉：自动 / 不分类 / 各分类目录（语义见 `lib/categoryRoute.ts`）。
// 与同行输入框同高同圆角（`h-control`、`rounded-md`、`border-input`），菜单复用 `ActionMenu`。

import { Ban, ChevronDown, Sparkles } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useT } from '../../../i18n'
import { categoryIconByKey } from '../../../lib/category-icons'
import { matchedCategory } from '../../../lib/categoryRoute'
import type { CategoryPickMode } from '../../../lib/categoryRoute'
import type { CustomCategoryDto } from '../../../lib/rpc'
import { ActionMenu, Icon } from '../../../ui'
import type { MenuEntry } from '../../../ui'
import { categoryLabel } from '../state'

export function CategoryPicker({
  mode,
  options,
  predicted,
  onChange,
}: {
  mode: CategoryPickMode
  /** 可选分类（`pickableCategories`）。 */
  options: readonly CustomCategoryDto[]
  /** Auto 模式的预测目录。 */
  predicted: string | null
  onChange: (mode: CategoryPickMode) => void
}) {
  const t = useT()

  let triggerIcon: LucideIcon = Sparkles
  let triggerLabel = t('saveDirCategoryAuto')
  if (mode.kind === 'auto') {
    const hit = matchedCategory(options, predicted)
    if (hit) {
      triggerIcon = categoryIconByKey(hit.icon)
      triggerLabel = t('saveDirCategoryAutoMatched', { name: categoryLabel(t, hit) })
    }
  } else if (mode.kind === 'none') {
    triggerIcon = Ban
    triggerLabel = t('saveDirCategoryNone')
  } else {
    const picked = options.find((category) => category.id === mode.id)
    if (picked) {
      triggerIcon = categoryIconByKey(picked.icon)
      triggerLabel = categoryLabel(t, picked)
    }
  }

  const entries: MenuEntry[] = [
    {
      type: 'item',
      key: 'auto',
      label: t('saveDirCategoryAuto'),
      description: t('saveDirCategoryAutoDesc'),
      icon: Sparkles,
      checked: mode.kind === 'auto',
      onSelect: () => onChange({ kind: 'auto' }),
    },
    {
      type: 'item',
      key: 'none',
      label: t('saveDirCategoryNone'),
      description: t('saveDirCategoryNoneDesc'),
      icon: Ban,
      checked: mode.kind === 'none',
      onSelect: () => onChange({ kind: 'none' }),
    },
    ...(options.length > 0 ? [{ type: 'separator' as const, key: 'sep' }] : []),
    ...options.map(
      (category): MenuEntry => ({
        type: 'item',
        key: category.id,
        label: categoryLabel(t, category),
        description: category.saveDir.trim(),
        descriptionIsPath: true,
        icon: categoryIconByKey(category.icon),
        checked: mode.kind === 'category' && mode.id === category.id,
        onSelect: () => onChange({ kind: 'category', id: category.id }),
      }),
    ),
  ]

  const description = mode.kind === 'auto' ? t('saveDirCategoryAutoDesc') : mode.kind === 'none' ? t('saveDirCategoryNoneDesc') : ''

  return (
    <ActionMenu
      align="start"
      title={t('saveDirCategory')}
      entries={entries}
      trigger={
        <button
          type="button"
          aria-label={t('saveDirCategory')}
          title={description || triggerLabel}
          className="inline-flex h-control w-40 shrink-0 cursor-pointer items-center gap-2 rounded-md border border-input bg-transparent px-2.5 text-sm text-foreground transition-colors hover:bg-row-hover focus-visible:border-ring coarse:min-h-touch"
        >
          <Icon icon={triggerIcon} className="shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1 truncate text-left">{triggerLabel}</span>
          <Icon icon={ChevronDown} className="shrink-0 text-muted-foreground" />
        </button>
      }
    />
  )
}
