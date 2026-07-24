// 通用：并发/分段/重试参数（服务器 config 表）。
import { useI18n } from '../../lib/i18n'
import type { ConfigMap } from '../../lib/types'
import { NumberFieldRow, SetRow, SetSwitch } from './controls'

/** 解析引擎持久化的域名连接上限数据（首行 v1 版本标记），返回未过期条数。 */
function parseConnPolicyCount(raw: string): number {
  const lines = raw.split('\n')
  if (lines[0]?.trim() !== 'v1') return 0
  const nowSecs = Math.floor(Date.now() / 1000)
  const ttlSecs = 24 * 3600
  let count = 0
  for (const line of lines.slice(1)) {
    const parts = line.split('\t')
    if (parts.length !== 3 || !parts[0]) continue
    const cap = Number(parts[1])
    const ts = Number(parts[2])
    if (!Number.isFinite(cap) || cap < 1 || !Number.isFinite(ts)) continue
    if (nowSecs - ts < ttlSecs) count++
  }
  return count
}

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
  const autoMaxConnections = Number(config.auto_max_connections ?? '16')
  const connPolicyCount = parseConnPolicyCount(config.domain_conn_caps ?? '')
  const maxRetries = Number(config.max_auto_retries ?? '3')
  const retryDelay = Number(config.auto_retry_delay_secs ?? '5')
  const analyticsEnabled = (config.analytics_enabled ?? 'true') === 'true'

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
        {defaultSegments === 0 && (
          <NumberFieldRow
            title={t('set.general.autoMaxConn')}
            desc={t('set.general.autoMaxConnDesc')}
            value={autoMaxConnections}
            min={1}
            onCommit={(n) => mutate({ auto_max_connections: String(n) })}
          />
        )}
        <SetRow title={t('set.general.connPolicy')} desc={t('set.general.connPolicyDesc')}>
          <div className="flex items-center gap-3">
            <span className="text-xs opacity-60">
              {connPolicyCount > 0
                ? t('set.general.connPolicyCount', { count: String(connPolicyCount) })
                : t('set.general.connPolicyEmpty')}
            </span>
            <button
              type="button"
              className="btn ghost sm"
              disabled={connPolicyCount === 0}
              onClick={() => mutate({ domain_conn_caps: '' })}
            >
              {t('set.general.connPolicyClear')}
            </button>
          </div>
        </SetRow>
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
        <SetRow title={t('set.general.analytics')} desc={t('set.general.analyticsDesc')}>
          <SetSwitch
            checked={analyticsEnabled}
            onCheckedChange={(v) => mutate({ analytics_enabled: String(v) })}
          />
        </SetRow>
      </div>
    </>
  )
}
