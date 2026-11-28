import { clsx, type ClassValue } from 'clsx'
import { extendTailwindMerge } from 'tailwind-merge'

// 自定义字号（GPUI typography.caption/title/md）必须登记为 font-size，
// 否则 twMerge 会把 `text-caption` 当颜色，与 `text-muted-foreground` 互相吞掉。
// 密度工具类（index.css 的 `@utility h-control` 等）同理必须登记进尺寸分组，
// 否则 `h-control` 与调用方追加的 `h-6` 会同时保留，谁生效取决于 CSS 输出顺序。
const twMerge = extendTailwindMerge({
  extend: {
    classGroups: {
      'font-size': [{ text: ['caption', 'title', 'md'] }],
      h: [
        { h: ['control', 'nav-row', 'title-bar', 'status-bar', 'status-control', 'section-header', 'task-row', 'task-row-compact', 'toolbar-button'] },
      ],
      'min-h': [{ 'min-h': ['control', 'nav-row', 'section-header', 'touch'] }],
      w: [{ w: ['control', 'rail'] }],
      'min-w': [{ 'min-w': ['control', 'status-control', 'touch'] }],
      size: [{ size: ['control', 'touch'] }],
    },
  },
})

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
