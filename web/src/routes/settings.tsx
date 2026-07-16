// #screen-settings —— 左侧分类导航 + 右侧设置正文。
import { useNavigate } from '@tanstack/react-router'
import type { LucideIcon } from 'lucide-react'
import { ArrowLeft, Download, Globe, Info, Lock, Monitor, Palette, Puzzle, Shield } from 'lucide-react'
import { useState } from 'react'
import { cn } from '../lib/cn'
import { useI18n } from '../lib/i18n'
import type { I18nKey } from '../lib/i18n'
import type { ConfigMap } from '../lib/types'
import { AboutSettings } from '../components/settings/AboutSettings'
import { AppearanceSettings } from '../components/settings/AppearanceSettings'
import { BitTorrentSettings } from '../components/settings/BitTorrentSettings'
import { DownloadSettings } from '../components/settings/DownloadSettings'
import { ExtensionsSettings } from '../components/settings/ExtensionsSettings'
import { GeneralSettings } from '../components/settings/GeneralSettings'
import { ProxySettings } from '../components/settings/ProxySettings'
import { SecuritySettings } from '../components/settings/SecuritySettings'
import { useConfigMutation, useConfigQuery } from '../components/settings/useConfig'

type Category = 'general' | 'appearance' | 'download' | 'bt' | 'proxy' | 'security' | 'extensions' | 'about'

const NAV: { key: Category; labelKey: I18nKey; icon: LucideIcon }[] = [
  { key: 'general', labelKey: 'set.general', icon: Monitor },
  { key: 'appearance', labelKey: 'set.appearance', icon: Palette },
  { key: 'download', labelKey: 'set.download', icon: Download },
  { key: 'bt', labelKey: 'set.bt', icon: Globe },
  { key: 'proxy', labelKey: 'set.proxy', icon: Shield },
  { key: 'security', labelKey: 'set.security', icon: Lock },
  { key: 'extensions', labelKey: 'set.extensions', icon: Puzzle },
  { key: 'about', labelKey: 'set.about', icon: Info },
]

const CAT_KEY = 'fluxdown.settingsCat'
const CATEGORIES = new Set<Category>(NAV.map((n) => n.key))

function readStoredCat(): Category {
  const v = localStorage.getItem(CAT_KEY)
  // 旧版「插件」「组件」两个分类已合并为「扩展」，历史 localStorage 值迁移到新 key。
  if (v === 'plugins' || v === 'components') return 'extensions'
  return v && CATEGORIES.has(v as Category) ? (v as Category) : 'general'
}

export function SettingsScreen() {
  const navigate = useNavigate()
  const { t } = useI18n()
  const [cat, setCatState] = useState<Category>(readStoredCat)

  function setCat(next: Category) {
    localStorage.setItem(CAT_KEY, next)
    setCatState(next)
  }

  const { data: config, isLoading, isError } = useConfigQuery()
  const mutation = useConfigMutation()

  function mutate(entries: ConfigMap) {
    mutation.mutate(entries)
  }

  function renderBody() {
    if (cat === 'appearance') return <AppearanceSettings />
    if (cat === 'about') return <AboutSettings config={config} mutate={mutate} />
    if (cat === 'extensions') return <ExtensionsSettings />
    if (isLoading) return <p className="set-desc">{t('common.loading')}</p>
    if (isError || !config) return <p className="set-desc text-danger">{t('set.loadFailed')}</p>
    switch (cat) {
      case 'general':
        return <GeneralSettings config={config} mutate={mutate} />
      case 'download':
        return <DownloadSettings config={config} mutate={mutate} />
      case 'bt':
        return <BitTorrentSettings config={config} mutate={mutate} />
      case 'proxy':
        return <ProxySettings config={config} mutate={mutate} />
      case 'security':
        return <SecuritySettings config={config} mutate={mutate} />
      default:
        return null
    }
  }

  return (
    <section className="wscreen active" id="screen-settings">
      <aside className="settings-side">
        <button className="settings-back" type="button" onClick={() => navigate({ to: '/' })}>
          <ArrowLeft />
          {t('common.back')}
        </button>
        <p className="side-label">{t('set.title')}</p>
        <nav className="settings-nav">
          {NAV.map(({ key, labelKey, icon: Icon }) => (
            <button key={key} type="button" className={cn(cat === key && 'active')} onClick={() => setCat(key)}>
              <Icon />
              {t(labelKey)}
            </button>
          ))}
        </nav>
      </aside>
      <div className="settings-body">{renderBody()}</div>
    </section>
  )
}
