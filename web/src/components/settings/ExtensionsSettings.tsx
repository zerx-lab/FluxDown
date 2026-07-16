// 扩展：插件与组件的统一入口，内部用 Tab 切换（插件 / 组件），默认展示插件 Tab。
import { useState } from 'react'
import { cn } from '../../lib/cn'
import { useI18n } from '../../lib/i18n'
import { ComponentsSettings } from './ComponentsSettings'
import { PluginsSettings } from './PluginsSettings'

type ExtensionTab = 'plugins' | 'components'

export function ExtensionsSettings() {
  const { t } = useI18n()
  const [tab, setTab] = useState<ExtensionTab>('plugins')

  return (
    <div className="max-w-[640px]">
      <h2 className="set-title">{t('set.extensions')}</h2>
      <p className="set-desc">{t('set.extensions.desc')}</p>

      <div className="mb-5 flex gap-1 border-b border-line">
        <button
          type="button"
          className={cn(
            'border-b-2 px-3 pb-2.5 pt-1 text-[13px] transition-colors',
            tab === 'plugins' ? 'border-accent font-semibold text-accent' : 'border-transparent text-text2 hover:text-text',
          )}
          onClick={() => setTab('plugins')}
        >
          {t('set.plugins')}
        </button>
        <button
          type="button"
          className={cn(
            'border-b-2 px-3 pb-2.5 pt-1 text-[13px] transition-colors',
            tab === 'components' ? 'border-accent font-semibold text-accent' : 'border-transparent text-text2 hover:text-text',
          )}
          onClick={() => setTab('components')}
        >
          {t('set.components')}
        </button>
      </div>

      {tab === 'plugins' ? <PluginsSettings /> : <ComponentsSettings />}
    </div>
  )
}
