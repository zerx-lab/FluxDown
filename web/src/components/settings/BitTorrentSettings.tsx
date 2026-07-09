// BitTorrent：librqbit 引擎参数（服务器 config 表）。
import { useI18n } from '../../lib/i18n'
import type { ConfigMap } from '../../lib/types'
import { NumberInput, SetRow, SetSwitch, TextAreaFieldRow } from './controls'

export function BitTorrentSettings({
  config,
  mutate,
}: {
  config: ConfigMap
  mutate: (entries: ConfigMap) => void
}) {
  const { t } = useI18n()
  const dht = (config.bt_enable_dht ?? 'true') === 'true'
  const upnp = (config.bt_enable_upnp ?? 'true') === 'true'
  const portStart = Number(config.bt_port_start ?? '6881')
  const portEnd = Number(config.bt_port_end ?? '6889')
  const trackers = config.bt_custom_trackers ?? ''

  return (
    <>
      <h2 className="set-title">{t('set.bt')}</h2>
      <p className="set-desc">{t('set.bt.desc')}</p>
      <div className="set-group">
        <SetRow title={t('set.bt.dht')} desc={t('set.bt.dhtDesc')}>
          <SetSwitch checked={dht} onCheckedChange={(v) => mutate({ bt_enable_dht: String(v) })} />
        </SetRow>
        <SetRow title={t('set.bt.upnp')} desc={t('set.bt.upnpDesc')}>
          <SetSwitch checked={upnp} onCheckedChange={(v) => mutate({ bt_enable_upnp: String(v) })} />
        </SetRow>
        <SetRow title={t('set.bt.ports')} desc={t('set.bt.portsDesc')}>
          <div className="flex items-center gap-2">
            <NumberInput value={portStart} min={1} className="short" onCommit={(n) => mutate({ bt_port_start: String(n) })} />
            <span className="text-text3">–</span>
            <NumberInput value={portEnd} min={1} className="short" onCommit={(n) => mutate({ bt_port_end: String(n) })} />
          </div>
        </SetRow>
      </div>
      <div className="set-group">
        <TextAreaFieldRow
          title={t('set.bt.trackers')}
          desc={t('set.bt.trackersDesc')}
          value={trackers}
          placeholder={'udp://tracker.opentrackr.org:1337/announce'}
          onCommit={(v) => mutate({ bt_custom_trackers: v })}
        />
      </div>
    </>
  )
}
