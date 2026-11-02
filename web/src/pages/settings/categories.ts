// 设置分类注册表（顺序与图标对齐 GPUI `crates/settings/src/view.rs` 的 build_pages）。
// 后续「设置页」agent：为每个分类提供真实的 `Component`（替换 `SettingsCategoryStub`）。
// 路由 `/settings/$category` 的 `$category` 即这里的 `id`。

import { Code, Download, Gauge, Gift, Globe, HardDrive, Info, Magnet, Package, Palette, Settings, User } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import type { ComponentType } from 'react'
import { AboutSettings } from './sections/about'
import { AccountSettings } from './sections/account'
import { ApiSettings } from './sections/api'
import { DoctorSettings } from './sections/doctor'
import { ExtensionsSettings } from './sections/extensions'
import { AppearanceSettings } from './sections/appearance'
import { BtSettings } from './sections/bt'
import { DownloadSettings } from './sections/download'
import { Ed2kSettings } from './sections/ed2k'
import { GeneralSettings } from './sections/general'
import { ProxySettings } from './sections/proxy'
import { ReferralSettings } from './sections/referral'

export type SettingsCategoryId =
  | 'general'
  | 'account'
  | 'referral'
  | 'appearance'
  | 'download'
  | 'bt'
  | 'ed2k'
  | 'proxy'
  | 'api'
  | 'extensions'
  | 'doctor'
  | 'about'

export interface SettingsCategory {
  id: SettingsCategoryId
  labelKey: string
  descKey: string
  icon: LucideIcon
  /** 分类内容组件；由各分类的负责 agent 替换。 */
  Component: ComponentType
  /** 条件分类：仅在对应条件满足时出现在侧栏 / 可直达（见 `useVisibleCategories`）。 */
  gate?: 'referral'
}

export const SETTINGS_CATEGORIES: readonly SettingsCategory[] = [
  { id: 'general', labelKey: 'settingsCatGeneral', descKey: 'settingsCatGeneralDesc', icon: Settings, Component: GeneralSettings },
  { id: 'account', labelKey: 'settingsCatAccount', descKey: 'settingsCatAccountDesc', icon: User, Component: AccountSettings },
  { id: 'referral', labelKey: 'settingsCatReferral', descKey: 'settingsCatReferralDesc', icon: Gift, Component: ReferralSettings, gate: 'referral' },
  { id: 'appearance', labelKey: 'settingsCatAppearance', descKey: 'settingsCatAppearanceDesc', icon: Palette, Component: AppearanceSettings },
  { id: 'download', labelKey: 'settingsCatDownload', descKey: 'settingsCatDownloadDesc', icon: Download, Component: DownloadSettings },
  { id: 'bt', labelKey: 'settingsCatBt', descKey: 'settingsCatBtDesc', icon: Magnet, Component: BtSettings },
  { id: 'ed2k', labelKey: 'settingsCatEd2k', descKey: 'settingsCatEd2kDesc', icon: HardDrive, Component: Ed2kSettings },
  { id: 'proxy', labelKey: 'settingsCatProxy', descKey: 'settingsCatProxyDesc', icon: Globe, Component: ProxySettings },
  { id: 'api', labelKey: 'settingsCatApiService', descKey: 'settingsCatApiServiceDesc', icon: Code, Component: ApiSettings },
  { id: 'extensions', labelKey: 'settingsCatExtensions', descKey: 'settingsCatExtensionsDesc', icon: Package, Component: ExtensionsSettings },
  { id: 'doctor', labelKey: 'settingsCatDoctor', descKey: 'settingsCatDoctorDesc', icon: Gauge, Component: DoctorSettings },
  { id: 'about', labelKey: 'settingsCatAbout', descKey: 'settingsCatAboutDesc', icon: Info, Component: AboutSettings },
]

export function findSettingsCategory(id: string): SettingsCategory | undefined {
  return SETTINGS_CATEGORIES.find((category) => category.id === id)
}
