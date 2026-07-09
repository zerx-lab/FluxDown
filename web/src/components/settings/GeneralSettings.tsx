// 通用：并发/分段/重试参数（服务器 config 表）。
import { useI18n } from '../../lib/i18n'
import type { ConfigMap } from '../../lib/types'
import { NumberFieldRow } from './controls'

export function GeneralSettings({
  config,
  mutate,
}: {
  config: ConfigMap
  mutate: (entries: ConfigMap) => void
}) {
  const { t } = useI18n()
  const maxConcurrent = Number(config.max_concurrent_tasks ?? '5')
  const defaultSegments = Number(config.default_segments ?? '0')
  const maxRetries = Number(config.max_auto_retries ?? '3')
  const retryDelay = Number(config.auto_retry_delay_secs ?? '5')

  return (
    <>
      <h2 className="set-title">{t('set.general')}</h2>
      <p className="set-desc">{t('set.general.desc')}</p>
      <div className="set-group">
        <NumberFieldRow
          title={t('set.general.maxConcurrent')}
          desc={t('set.general.maxConcurrentDesc')}
          value={maxConcurrent}
          min={1}
          onCommit={(n) => mutate({ max_concurrent_tasks: String(n) })}
        />
        <NumberFieldRow
          title={t('set.general.segments')}
          desc={t('set.general.segmentsDesc')}
          value={defaultSegments}
          min={0}
          onCommit={(n) => mutate({ default_segments: String(n) })}
        />
        <NumberFieldRow
          title={t('set.general.retries')}
          desc={t('set.general.retriesDesc')}
          value={maxRetries}
          min={0}
          onCommit={(n) => mutate({ max_auto_retries: String(n) })}
        />
        <NumberFieldRow
          title={t('set.general.retryDelay')}
          desc={t('set.general.retryDelayDesc')}
          value={retryDelay}
          min={0}
          onCommit={(n) => mutate({ auto_retry_delay_secs: String(n) })}
        />
      </div>
    </>
  )
}
