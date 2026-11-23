// 外观 → 文件图标（crates/settings/src/sections/icon_pack.rs）：图标包卡片 + 样例文件预览，只写偏好。
// Web 取不到系统图标：「系统」卡片仍可选（与桌面同步同一偏好），本端经回退链显示其后的包。

import { Check, HardDrive } from 'lucide-react'
import { useMemo } from 'react'
import { useT } from '../../../../i18n'
import { cn } from '../../../../lib/cn'
import { BUILTIN_PACK_IDS, DEFAULT_PACK, FILE_ICON_PACK_KEY, PackIconGlyph, SYSTEM_PACK_ID, fileKindOf, iconPackState, resolveFileIcon, useIconPackState } from '../../../../lib/icon-pack'
import { Icon } from '../../../../ui'
import { SettingsRow, setPref, useSettingsReadOnly } from '../../kit'

const LABEL_KEYS: Record<string, string> = {
  [SYSTEM_PACK_ID]: 'fileIconPackSystem',
  lucide: 'fileIconPackLucide',
  material: 'fileIconPackMaterial',
  catppuccin: 'fileIconPackCatppuccin',
  thunder: 'fileIconPackThunder',
}

/** 预览样例：视频、压缩包、文档、程序（同 GPUI `PREVIEW_FILES`）。 */
const PREVIEW_FILES = ['video.mp4', 'archive.zip', 'report.pdf', 'setup.exe']
const PREVIEW_SIZE = '20px'

function PackPreview({ selection }: { selection: string }) {
  const state = useMemo(() => iconPackState(selection), [selection])
  if (selection === DEFAULT_PACK) return <Icon icon={HardDrive} className="text-muted-foreground" />
  return PREVIEW_FILES.map((name) => {
    const icon = resolveFileIcon(state, name, fileKindOf(name))
    return icon ? <PackIconGlyph key={name} icon={icon} size={PREVIEW_SIZE} className="text-muted-foreground" /> : null
  })
}

export function IconPackRow() {
  const t = useT()
  const disabled = useSettingsReadOnly()
  const current = useIconPackState().selection
  const cards = BUILTIN_PACK_IDS.map((id) => `builtin:${id}`)
  const selected = cards.includes(current) ? current : DEFAULT_PACK
  return (
    <SettingsRow title={t('fileIconPack')} description={t('fileIconPackDesc')} vertical>
      <div role="radiogroup" aria-label={t('fileIconPack')} className="flex flex-wrap gap-2">
        {BUILTIN_PACK_IDS.map((id) => {
          const selection = `builtin:${id}`
          const active = selection === selected
          return (
            <button
              key={id}
              type="button"
              role="radio"
              aria-checked={active}
              disabled={disabled}
              onClick={() => setPref(FILE_ICON_PACK_KEY, selection, { immediate: true })}
              className={cn(
                'flex w-[120px] flex-col gap-2 rounded-lg border bg-surface p-2 text-left transition-colors disabled:opacity-50',
                active ? 'border-primary' : 'border-border hover:bg-row-hover',
                'mobile:w-[calc(50%-0.25rem)]',
              )}
            >
              <span className="flex h-10 w-full items-center justify-center gap-1 rounded-md bg-background">
                <PackPreview selection={selection} />
              </span>
              <span className="flex items-center gap-1 text-xs text-foreground">
                <span className="min-w-0 flex-1 truncate">{t(LABEL_KEYS[id] ?? id)}</span>
                {active ? <Icon icon={Check} className="text-primary" /> : null}
              </span>
            </button>
          )
        })}
      </div>
    </SettingsRow>
  )
}
