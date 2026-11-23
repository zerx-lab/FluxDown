// 外观（crates/settings/src/sections/appearance.rs）：语言 / 明暗模式 / 内置主题卡片 / 强调色 / 文件图标 / 界面缩放。
// 只写偏好（`setPref(..., {immediate})`），ThemeProvider 与 I18nProvider 响应偏好快照即时生效；
// 主题库导入 / 导出 / 删除与「更多主题」是桌面专属，Web 省略。

import { resolveTheme } from '@gpui-theme/resolve'
import type { Hsla, ThemeDocument } from '@gpui-theme/types'
import { colorHex } from '@gpui-theme/value'
import { Check, Palette } from 'lucide-react'
import { useT } from '../../../../i18n'
import { LOCALE_PREF_KEY, useI18n } from '../../../../i18n'
import { cn } from '../../../../lib/cn'
import { setFileSizeUnits, useFileSizeUnits } from '../../../../lib/fileSizeUnits'
import { useTheme } from '../../../../theme'
import {
  ACCENT_LABEL_KEYS,
  ACCENT_SCHEMES,
  BUILTIN_THEMES,
  BUILTIN_THEME_LABEL_KEYS,
  COLOR_SCHEME_KEY,
  CUSTOM_COLOR_KEY,
  DARK_THEME_KEY,
  DEFAULT_APPEARANCE,
  LIGHT_THEME_KEY,
  THEME_MODE_KEY,
  UI_SCALE_KEY,
  accentHex,
  builtinAppearance,
  builtinExtends,
} from '../../../../theme'
import type { AccentScheme, BuiltinThemeId, ThemePreference } from '../../../../theme'
import { Icon } from '../../../../ui'
import { FontFamilyRow } from './FontFamilyRow'
import { IconPackRow } from './IconPackRow'
import { DropdownField, PrefDropdownRow, SettingsPage, SettingsRow, SettingsSection, setPref, usePrefString, useSettingsReadOnly } from '../../kit'

/** `UI_SCALE_PERCENTS`（crates/theme）。 */
const UI_SCALE_PERCENTS = [80, 90, 100, 110, 120, 130, 150] as const

interface PreviewColors {
  background: string
  surface: string
  border: string
  primary: string
  foreground: string
  mutedForeground: string
}

/** 内置主题原色（不含用户强调色，同 GPUI `BuiltinThemeId::colors()`）。 */
function previewColors(id: BuiltinThemeId): PreviewColors {
  const mode = builtinAppearance(id)
  const document: ThemeDocument = { extends: builtinExtends(id), tokens: {}, dark: {}, light: {}, extra: {} }
  const tokens = resolveTheme(document, mode).tokens
  const color = (path: string): string => {
    const value = tokens.get(path)
    return value?.type === 'color' ? colorHex(value.color satisfies Hsla) : 'transparent'
  }
  return {
    background: color('colors.background'),
    surface: color('colors.surface'),
    border: color('colors.border'),
    primary: color('colors.primary'),
    foreground: color('colors.foreground'),
    mutedForeground: color('colors.mutedForeground'),
  }
}

function ThemeCard({ label, colors, selected, disabled, onSelect }: { label: string; colors: PreviewColors; selected: boolean; disabled: boolean; onSelect: () => void }) {
  const bar = (width: string, color: string, opacity = 1) => (
    <span className="block h-[3px] rounded-full" style={{ width, background: color, opacity }} />
  )
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      disabled={disabled}
      onClick={onSelect}
      className={cn(
        'flex w-[120px] flex-col gap-2 rounded-lg border bg-surface p-2 text-left transition-colors disabled:opacity-50',
        selected ? 'border-primary' : 'border-border hover:bg-row-hover',
        'mobile:w-[calc(50%-0.25rem)]',
      )}
    >
      <span className="flex h-[52px] w-full overflow-hidden rounded-md border" style={{ background: colors.background, borderColor: colors.border }}>
        <span className="flex h-full w-7 flex-col items-center justify-center gap-[3px]" style={{ background: colors.surface }}>
          {bar('16px', colors.primary)}
          {bar('16px', colors.mutedForeground, 0.35)}
          {bar('16px', colors.mutedForeground, 0.35)}
        </span>
        <span className="flex h-full flex-1 flex-col justify-center gap-[3px] p-1">
          {bar('100%', colors.foreground, 0.4)}
          {bar('100%', colors.mutedForeground, 0.4)}
          {bar('60%', colors.mutedForeground, 0.4)}
        </span>
      </span>
      <span className="flex items-center gap-1 text-xs text-foreground">
        <span className="min-w-0 flex-1 truncate">{label}</span>
        {selected ? <Icon icon={Check} className="text-primary" /> : null}
      </span>
    </button>
  )
}

/** 只展示可放进当前明暗槽位的内置预设（与 GPUI 主题选择器一致）。 */
function ThemeCards({ disabled }: { disabled: boolean }) {
  const t = useT()
  const { mode, prefs } = useTheme()
  const slotKey = mode === 'dark' ? DARK_THEME_KEY : LIGHT_THEME_KEY
  const selected = mode === 'dark' ? prefs.darkTheme : prefs.lightTheme
  const presets = BUILTIN_THEMES.filter((id) => builtinAppearance(id) === mode)
  return (
    <div className="flex w-full flex-col gap-1">
      <div className="text-xs text-muted-foreground">{t(mode === 'dark' ? 'themeDarkTheme' : 'themeLightTheme')}</div>
      <div role="radiogroup" className="flex flex-wrap gap-2">
        {presets.map((id) => (
          <ThemeCard
            key={id}
            label={t(BUILTIN_THEME_LABEL_KEYS[id])}
            colors={previewColors(id)}
            selected={selected === id}
            disabled={disabled}
            onSelect={() => setPref(slotKey, `builtin:${id}`, { immediate: true })}
          />
        ))}
      </div>
    </div>
  )
}

function rgbToArgb(rgb: number): number {
  return (0xff000000 | rgb) >>> 0
}

/** 4 个预设色点 + 自定义；选中自定义时展开取色器（原生 color input，触屏可用）。 */
function AccentPicker({ disabled }: { disabled: boolean }) {
  const t = useT()
  const { prefs } = useTheme()
  return (
    <div className="flex w-full flex-col gap-3">
      <div role="radiogroup" aria-label={t('themeColor')} className="flex flex-wrap gap-2">
        {ACCENT_SCHEMES.map((scheme: AccentScheme) => {
          const selected = prefs.colorScheme === scheme
          const color = accentHex({ ...DEFAULT_APPEARANCE, colorScheme: scheme, customRgb: prefs.customRgb })
          return (
            <button
              key={scheme}
              type="button"
              role="radio"
              aria-checked={selected}
              aria-label={t(ACCENT_LABEL_KEYS[scheme])}
              title={t(ACCENT_LABEL_KEYS[scheme])}
              disabled={disabled}
              onClick={() => setPref(COLOR_SCHEME_KEY, scheme, { immediate: true })}
              className={cn(
                'relative inline-flex size-7 items-center justify-center rounded-full border-2 text-white transition-colors disabled:opacity-50 coarse:size-11',
                selected ? 'border-foreground' : 'hover:border-muted-foreground',
              )}
              style={{ background: color, borderColor: selected ? undefined : color }}
            >
              {selected ? <Icon icon={Check} /> : scheme === 'custom' ? <Icon icon={Palette} /> : null}
            </button>
          )
        })}
      </div>
      {prefs.colorScheme === 'custom' ? (
        <label className={cn('flex items-center gap-3 text-xs text-muted-foreground', disabled && 'opacity-50')}>
          <input
            type="color"
            aria-label={t('colorCustom')}
            disabled={disabled}
            value={`#${prefs.customRgb.toString(16).padStart(6, '0')}`}
            onChange={(event) => {
              const rgb = Number.parseInt(event.target.value.slice(1), 16)
              if (Number.isFinite(rgb)) setPref(CUSTOM_COLOR_KEY, rgbToArgb(rgb), { immediate: true })
            }}
            className="h-9 w-14 cursor-pointer rounded-md border border-border bg-transparent p-0.5 coarse:h-11"
          />
          <span className="tabular uppercase">#{prefs.customRgb.toString(16).padStart(6, '0')}</span>
        </label>
      ) : null}
    </div>
  )
}

function LanguageRow() {
  const t = useT()
  const { languages } = useI18n()
  const value = usePrefString(LOCALE_PREF_KEY, 'system')
  const options = [{ value: 'system', label: t('languageSystem') }, ...languages.map((language) => ({ value: language.code, label: language.name }))]
  return (
    <SettingsRow title={t('language')} description={t('languageDesc')}>
      <DropdownField value={value} options={options} aria-label={t('language')} onValueChange={(next) => setPref(LOCALE_PREF_KEY, next, { immediate: true })} />
    </SettingsRow>
  )
}

function UiScaleRow() {
  const t = useT()
  const { prefs } = useTheme()
  const options = UI_SCALE_PERCENTS.map((percent) => ({ value: String(percent), label: `${percent}%` }))
  return (
    <SettingsRow title={t('uiScale')} description={t('uiScaleDesc')}>
      <DropdownField
        value={String(prefs.uiScalePercent)}
        options={options}
        aria-label={t('uiScale')}
        onValueChange={(next) => setPref(UI_SCALE_KEY, Number(next) / 100, { immediate: true })}
      />
    </SettingsRow>
  )
}
function FileSizeUnitsRow({ disabled }: { disabled: boolean }) {
  const t = useT()
  const units = useFileSizeUnits()
  const options = [
    { value: 'legacy', label: t('fileSizeUnitsLegacy') },
    { value: 'iec', label: t('fileSizeUnitsIec') },
  ]
  return (
    <SettingsRow title={t('fileSizeUnits')} description={t('fileSizeUnitsDesc')} disabled={disabled}>
      <DropdownField
        value={units}
        options={options}
        aria-label={t('fileSizeUnits')}
        onValueChange={(next) => setFileSizeUnits(next === 'iec' ? 'iec' : 'legacy')}
      />
    </SettingsRow>
  )
}


export function AppearanceSettings() {
  const t = useT()
  const disabled = useSettingsReadOnly()
  const modeOptions: { value: ThemePreference; label: string }[] = [
    { value: 'system', label: t('themeModeSystem') },
    { value: 'light', label: t('themeModeLight') },
    { value: 'dark', label: t('themeModeDark') },
  ]
  return (
    <SettingsPage title={t('settingsCatAppearance')} description={t('settingsCatAppearanceDesc')}>
      <SettingsSection>
        <LanguageRow />
      </SettingsSection>
      <SettingsSection title={t('settingsGroupTheme')}>
        <PrefDropdownRow prefKey={THEME_MODE_KEY} fallback="system" titleKey="themeMode" descKey="themeModeDesc" options={modeOptions} immediate />
        <SettingsRow title={t('themeSelection')} description={t('themeSelectionDesc')} vertical>
          <ThemeCards disabled={disabled} />
        </SettingsRow>
        <SettingsRow title={t('themeColor')} description={t('themeColorDesc')} vertical>
          <AccentPicker disabled={disabled} />
        </SettingsRow>
        <IconPackRow />
      </SettingsSection>
      <SettingsSection title={t('settingsGroupInterface')}>
        <FontFamilyRow />
        <FileSizeUnitsRow disabled={disabled} />
        <UiScaleRow />
      </SettingsSection>
    </SettingsPage>
  )
}
