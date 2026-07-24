// 下载：默认保存目录 / 全局限速 / 全局 User-Agent / 多 CDN 并发（服务器 config 表）。
import { useState } from 'react'
import { confirmDialog } from '../../lib/confirm'
import { useI18n } from '../../lib/i18n'
import type { ConfigMap } from '../../lib/types'
import { FsPicker } from '../dialogs/fs-picker'
import { UA_PRESETS } from '../../lib/ua-presets'
import { NumberFieldRow, SetRow, SetSelect, SetSwitch, TextInput } from './controls'

const MB = 1024 * 1024

const CUSTOM = '__custom__'

export function DownloadSettings({
  config,
  mutate,
}: {
  config: ConfigMap
  mutate: (entries: ConfigMap) => void
}) {
  const { t } = useI18n()
  const saveDir = config.default_save_dir ?? ''
  const speedBytes = Number(config.speed_limit_bytes ?? '0')
  const speedMB = speedBytes > 0 ? speedBytes / MB : 0
  const ua = config.global_user_agent ?? ''
  const useServerTime = (config.use_server_time ?? 'false') === 'true'
  const cdnMultiEnabled = (config.cdn_multi_enabled ?? '0') === '1'
  const cdnMaxNodes = Number(config.cdn_max_nodes ?? '0')
  const proxyMode = config.proxy_mode ?? 'none'

  /** 开启多 CDN 并发时与代理互斥（对齐桌面端 _onCdnMultiChanged）：代理已启用则
   *  弹确认框——确认「关闭代理并开启」一次写入两个键，取消则不改任何状态。 */
  async function onCdnMultiChange(v: boolean) {
    if (!v || proxyMode === 'none') {
      mutate({ cdn_multi_enabled: v ? '1' : '0' })
      return
    }
    const ok = await confirmDialog({
      title: t('set.download.cdnMultiProxyConfirmTitle'),
      message:
        proxyMode === 'system'
          ? t('set.download.cdnMultiProxyConfirmDescSystem')
          : t('set.download.cdnMultiProxyConfirmDescManual'),
      confirmLabel: t('set.download.cdnMultiProxyConfirmDisable'),
    })
    if (ok) mutate({ proxy_mode: 'none', cdn_multi_enabled: '1' })
  }

  // 自定义模式：用户在下拉里选了"自定义"，或当前值不匹配任何预设。
  const isPreset = ua === '' || UA_PRESETS.some((p) => p.value === ua)
  const [customMode, setCustomMode] = useState(!isPreset)
  const customActive = customMode || !isPreset

  // Radix Select 把 value="" 视为"未选择"，触发器会显示空白 —— 默认项用哨兵值。
  const DEFAULT = '__default__'
  const uaOptions = [
    { label: t('set.download.uaDefault'), value: DEFAULT },
    ...UA_PRESETS,
    { label: t('common.custom'), value: CUSTOM },
  ]
  const selectValue = customActive ? CUSTOM : ua === '' ? DEFAULT : ua

  return (
    <>
      <h2 className="set-title">{t('set.download')}</h2>
      <p className="set-desc">{t('set.download.desc')}</p>
      <div className="set-group">
        <SetRow title={t('set.download.saveDir')} desc={t('set.download.saveDirDesc')}>
          <div className="dir-row" style={{ width: 300, flexShrink: 0 }}>
            <TextInput value={saveDir} onCommit={(v) => mutate({ default_save_dir: v })} />
            <FsPicker value={saveDir} onChange={(p) => mutate({ default_save_dir: p })} />
          </div>
        </SetRow>
        <NumberFieldRow
          title={t('set.download.speedLimit')}
          desc={t('set.download.speedLimitDesc')}
          value={speedMB}
          min={0}
          onCommit={(n) => mutate({ speed_limit_bytes: String(Math.max(0, Math.round(n * MB))) })}
        />
        <SetRow title={t('set.download.ua')} desc={t('set.download.uaDesc')}>
          <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexShrink: 0 }}>
            {customActive && (
              <div style={{ width: 220 }}>
                <TextInput
                  value={ua}
                  placeholder={t('set.download.uaCustomPlaceholder')}
                  onCommit={(v) => mutate({ global_user_agent: v.trim() })}
                />
              </div>
            )}
            <SetSelect
              width={customActive ? 130 : 220}
              value={selectValue}
              onValueChange={(v) => {
                if (v === CUSTOM) {
                  setCustomMode(true)
                } else {
                  setCustomMode(false)
                  mutate({ global_user_agent: v === DEFAULT ? '' : v })
                }
              }}
              options={uaOptions}
            />
          </div>
        </SetRow>
        <SetRow title={t('set.download.serverTime')} desc={t('set.download.serverTimeDesc')}>
          <SetSwitch
            checked={useServerTime}
            onCheckedChange={(v) => mutate({ use_server_time: String(v) })}
          />
        </SetRow>
        <SetRow title={t('set.download.cdnMulti')} desc={t('set.download.cdnMultiDesc')}>
          <SetSwitch checked={cdnMultiEnabled} onCheckedChange={(v) => void onCdnMultiChange(v)} />
        </SetRow>
        {cdnMultiEnabled && (
          <NumberFieldRow
            title={t('set.download.cdnMaxNodes')}
            desc={t('set.download.cdnMaxNodesDesc')}
            value={cdnMaxNodes}
            min={0}
            max={8}
            onCommit={(n) => mutate({ cdn_max_nodes: String(Math.min(8, Math.max(0, n))) })}
          />
        )}
      </div>
    </>
  )
}
