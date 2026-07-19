// 顶栏「显示选项」入口 —— 图标按钮 + 挂载的下拉面板（View Options Panel），
// 移植自 lib/src/widgets/view_options_panel.dart 的六节结构（形态/密度/分组/排序/显示/列），
// UI 细节按 web 现有风格适配（Radix DropdownMenu，非受控 Item，面板常开、改动即时生效）。
// 组件名为 ViewOptionsPanelButton（而非 ViewOptionsPanel）：它自身就是「按钮+面板」整体，
// TopBar 侧一行 `<ViewOptionsPanelButton />` 即可挂载。

import { type ReactNode } from 'react'
import * as DropdownMenu from '@radix-ui/react-dropdown-menu'
import { ArrowDown, ArrowUp, SlidersHorizontal } from 'lucide-react'
import { cn } from '../../lib/cn'
import { useI18n } from '../../lib/i18n'
import { columnLabel, COLUMN_CANONICAL_ORDER } from '../../lib/task-columns'
import {
  describeViewState,
  GROUP_BY_CYCLE,
  isDefaultViewPrefs,
  resetViewPrefs,
  SORT_KEY_CYCLE,
  SORT_KEY_DEFAULT_DIR,
  updateViewPrefs,
  useViewPrefs,
  viewDensityLabel,
  viewFormLabel,
  viewGroupByLabel,
  viewSortKeyLabel,
  type TaskColumnId,
  type ViewGroupBy,
  type ViewPrefs,
  type ViewSortKey,
} from '../../lib/view-prefs'
import { useTasksUi } from './context'

/** 小节外壳：标题行（可选快捷键提示）+ 控件。 */
function ViewSection({ label, hint, children }: { label: string; hint?: string | null; children: ReactNode }) {
  return (
    <div className="view-section">
      <div className="view-section-head">
        <span>{label}</span>
        {hint && <em>{hint}</em>}
      </div>
      {children}
    </div>
  )
}

export function ViewOptionsPanelButton() {
  const { t } = useI18n()
  const { statusTab } = useTasksUi()
  const prefs = useViewPrefs(statusTab)
  const apply = (updater: (current: ViewPrefs) => ViewPrefs) => updateViewPrefs(statusTab, updater)

  const toggleColumn = (id: TaskColumnId) => {
    const next = new Set(prefs.columns)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    apply((p) => ({ ...p, columns: next }))
  }

  const toggleSort = (key: ViewSortKey) => {
    if (prefs.sortKey === key && key !== 'smart') {
      apply((p) => ({ ...p, sortDir: p.sortDir === 'asc' ? 'desc' : 'asc' }))
    } else {
      apply((p) => ({ ...p, sortKey: key, sortDir: SORT_KEY_DEFAULT_DIR[key] }))
    }
  }

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          className={cn('icon-btn', 'view-btn', !isDefaultViewPrefs(prefs) && 'active')}
          title={t('view.entryTooltip', { state: describeViewState(prefs) })}
        >
          <SlidersHorizontal size={17} />
          {!isDefaultViewPrefs(prefs) && <span className="view-dot" />}
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="view-panel show" sideOffset={8} align="end">
          <b>{t('view.optionsTitle')}</b>

          <ViewSection label={t('view.sectionForm')} hint="V">
            <div className="view-seg">
              <button type="button" className={cn(prefs.form === 'list' && 'active')} onClick={() => apply((p) => ({ ...p, form: 'list' }))}>
                {viewFormLabel('list')}
              </button>
              <button type="button" className={cn(prefs.form === 'grid' && 'active')} onClick={() => apply((p) => ({ ...p, form: 'grid' }))}>
                {viewFormLabel('grid')}
              </button>
            </div>
          </ViewSection>

          <ViewSection label={t('view.sectionDensity')} hint={prefs.form === 'grid' ? t('view.densityGridDisabledHint') : 'Shift+D'}>
            <div className={cn('view-seg', prefs.form === 'grid' && 'disabled')}>
              <button
                type="button"
                disabled={prefs.form === 'grid'}
                className={cn(prefs.density === 'comfortable' && 'active')}
                onClick={() => apply((p) => ({ ...p, density: 'comfortable' }))}
              >
                {viewDensityLabel('comfortable')}
              </button>
              <button
                type="button"
                disabled={prefs.form === 'grid'}
                className={cn(prefs.density === 'compact' && 'active')}
                onClick={() => apply((p) => ({ ...p, density: 'compact' }))}
              >
                {viewDensityLabel('compact')}
              </button>
            </div>
          </ViewSection>

          <ViewSection label={t('view.sectionGroupBy')} hint="G">
            <div className="view-chips">
              {GROUP_BY_CYCLE.map((g: ViewGroupBy) => (
                <button
                  key={g}
                  type="button"
                  className={cn('view-chip', prefs.groupBy === g && 'active')}
                  onClick={() => apply((p) => ({ ...p, groupBy: g }))}
                >
                  {viewGroupByLabel(g)}
                </button>
              ))}
            </div>
          </ViewSection>

          <ViewSection label={t('view.sectionSort')} hint="S">
            <div className="view-chips">
              {SORT_KEY_CYCLE.map((key: ViewSortKey) => {
                const selected = prefs.sortKey === key
                return (
                  <button key={key} type="button" className={cn('view-chip', selected && 'active')} onClick={() => toggleSort(key)}>
                    {viewSortKeyLabel(key)}
                    {selected && key !== 'smart' && (prefs.sortDir === 'asc' ? <ArrowUp size={11} /> : <ArrowDown size={11} />)}
                  </button>
                )
              })}
            </div>
          </ViewSection>

          <ViewSection label={t('view.sectionDisplay')} hint={null}>
            <div className="view-chips">
              <button
                type="button"
                className={cn('view-chip', prefs.showCompleted && 'active')}
                onClick={() => apply((p) => ({ ...p, showCompleted: !p.showCompleted }))}
              >
                {t('view.showCompleted')}
              </button>
              <button
                type="button"
                className={cn('view-chip', prefs.protocolBadges && 'active')}
                onClick={() => apply((p) => ({ ...p, protocolBadges: !p.protocolBadges }))}
              >
                {t('view.protocolBadges')}
              </button>
            </div>
          </ViewSection>

          {prefs.form !== 'grid' && (
            <ViewSection label={t('view.sectionColumns')} hint={null}>
              <div className="view-chips">
                {COLUMN_CANONICAL_ORDER.map((id) => (
                  <button
                    key={id}
                    type="button"
                    className={cn('view-chip', prefs.columns.has(id) && 'active')}
                    onClick={() => toggleColumn(id)}
                  >
                    {columnLabel(id)}
                  </button>
                ))}
              </div>
            </ViewSection>
          )}

          <div className="view-divider" />
          <button type="button" className="view-reset" onClick={() => resetViewPrefs(statusTab)}>
            {t('view.resetDefault')}
          </button>
          <p className="view-reset-hint">{t('view.resetHint')}</p>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  )
}
