import { AlertCircle, CheckCircle2, Info, TriangleAlert, X } from 'lucide-react'
import { useSyncExternalStore } from 'react'
import { t } from '../i18n'
import { cn } from '../lib/cn'
import { Icon } from './Icon'
import { dismissToast, readToasts, subscribeToasts } from './toast'
import type { ToastKind } from './toast'

const ICONS = { info: Info, success: CheckCircle2, warning: TriangleAlert, error: AlertCircle } as const
const ICON_TONE: Record<ToastKind, string> = {
  info: 'text-nav-selected-icon',
  success: 'text-success',
  warning: 'text-warning',
  error: 'text-destructive',
}

export function ToastHost() {
  const list = useSyncExternalStore(subscribeToasts, readToasts, readToasts)
  return (
    <div
      aria-live="polite"
      className="pointer-events-none fixed inset-x-0 z-[70] flex flex-col items-center gap-2 px-3"
      style={{ bottom: 'calc(var(--fx-bottom-bar, 0px) + env(safe-area-inset-bottom, 0px) + 12px)' }}
    >
      {list.map((item) => (
        <div
          key={item.id}
          role={item.kind === 'error' ? 'alert' : 'status'}
          className={cn(
            'animate-fx-pop pointer-events-auto flex max-w-[min(480px,100%)] items-start gap-2 rounded-lg border border-hairline bg-surface px-3 py-2 text-sm text-foreground shadow-md',
          )}
        >
          <Icon icon={ICONS[item.kind]} className={cn('mt-px', ICON_TONE[item.kind])} />
          <span className="max-h-[50dvh] min-w-0 flex-1 overflow-auto whitespace-pre-wrap [overflow-wrap:anywhere]">{item.text}</span>
          <button
            type="button"
            aria-label={t('close')}
            className="-mr-1 inline-flex size-5 shrink-0 items-center justify-center rounded-sm text-text-tertiary hover:text-foreground coarse:size-8"
            onClick={() => dismissToast(item.id)}
          >
            <Icon icon={X} size="md" />
          </button>
        </div>
      ))}
    </div>
  )
}
