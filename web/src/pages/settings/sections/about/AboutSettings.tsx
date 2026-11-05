// 关于（GPUI `crates/settings/src/sections/about.rs`）：版本、软件更新、日志导出、浏览器扩展与捐赠链接。
// 桌面专属的「打开日志目录」不在 Web 出现；更新由 agent 驱动（后台检查 / 下载 / 校验），
// 这里按 `snapshot.update` 展示状态，并提供「更新并重启」、取消下载、手动升级入口（Docker / NAS 为页内说明）。

import { useState } from 'react'
import { useI18n, useT } from '../../../../i18n'
import { exportLogs, rpc, useRpcSelector } from '../../../../lib/rpc'
import type { ReleaseNoteDto } from '../../../../lib/rpc'
import { Button, toast } from '../../../../ui'
import { rpcErrorText } from '../../../../lib/rpcErrorText'
import { localizedReleaseBody, releaseDate, releaseNoteBlocks, stripVersionHeading } from '../../../../lib/releaseNotes'
import { canCancel, canInstall, manualLine, manualUrl, statusLine, updateLineText, useUpdateStatus } from '../../../../lib/update'
import { DaemonNumberRow, PrefDropdownRow, PrefSwitchRow, SettingsCustomRow, SettingsPage, SettingsRow, SettingsSection, useSettingsReadOnly, usePrefString } from '../../kit'

const CHROME_STORE = 'https://chromewebstore.google.com/search/FluxDown'
const FIREFOX_STORE = 'https://addons.mozilla.org/firefox/addon/fluxdown/'
const EDGE_STORE = 'https://microsoftedge.microsoft.com/addons/search/FluxDown'
const DONATE = 'https://fluxdown.com/sponsor'
const WEBSITE = 'https://fluxdown.com'
/** 服务端资产随统一的 `vX.Y.Z` release 发布（更早版本在 `server-v*` release）。 */
const SERVER_RELEASES = 'https://github.com/zerx-lab/FluxDown/releases'
/** 对端国家：DB-IP lite 地理库（CC BY 4.0 署名）与国旗图标（flag-icons，MIT）。 */
const DB_IP = 'https://db-ip.com'
const FLAG_ICONS = 'https://github.com/lipis/flag-icons'

function openUrl(url: string) {
  window.open(url, '_blank', 'noopener,noreferrer')
}

function LinkButtons({ links }: { links: readonly { label: string; url: string }[] }) {
  return (
    <div className="flex flex-wrap gap-2">
      {links.map((link) => (
        <Button key={link.url} onClick={() => openUrl(link.url)}>
          {link.label}
        </Button>
      ))}
    </div>
  )
}

/** 一个版本的更新说明：标题统一为「v版本 + 发布日期」，正文按界面语言取段并去掉重复的版本标题。 */
function ReleaseNote({ note, locale, divided }: { note: ReleaseNoteDto; locale: string; divided: boolean }) {
  const date = releaseDate(note.publishedAt)
  const blocks = releaseNoteBlocks(stripVersionHeading(localizedReleaseBody(note.body, locale), note.version))
  return (
    <div className={divided ? 'flex flex-col gap-2 border-t border-border pt-4' : 'flex flex-col gap-2'}>
      <div className="flex items-baseline gap-2">
        <span className="text-sm font-semibold text-foreground">v{note.version}</span>
        {date ? <span className="text-xs tabular text-muted-foreground">{date}</span> : null}
      </div>
      <div className="flex flex-col gap-2 break-words text-xs leading-relaxed text-foreground">
        {blocks.map((block, index) => {
          switch (block.kind) {
            case 'heading':
              return (
                <div key={index} className="pt-1 font-medium">
                  {block.text}
                </div>
              )
            case 'list':
              return (
                <ul key={index} className="flex list-disc flex-col gap-1 pl-5 marker:text-muted-foreground">
                  {block.items.map((item, itemIndex) => (
                    <li key={itemIndex} style={item.depth > 0 ? { marginLeft: `${item.depth * 1.25}rem` } : undefined}>
                      {item.text}
                    </li>
                  ))}
                </ul>
              )
            case 'paragraph':
              return (
                <p key={index} className="whitespace-pre-line">
                  {block.lines.join('\n')}
                </p>
              )
          }
        })}
      </div>
    </div>
  )
}

function UpdateRow() {
  const t = useT()
  const { locale } = useI18n()
  const disabled = useSettingsReadOnly()
  const channel = usePrefString('general.update_channel', 'stable')
  const status = useUpdateStatus()
  const [busy, setBusy] = useState<'check' | 'install' | 'cancel' | null>(null)

  const run = async (kind: 'check' | 'install' | 'cancel', action: () => Promise<unknown>) => {
    if (busy) return
    setBusy(kind)
    try {
      // 结果经 `updateChanged` 事件进入快照，这里只负责报错。
      await action()
    } catch (error) {
      toast.error(rpcErrorText(error, t))
    } finally {
      setBusy(null)
    }
  }

  const checking = busy === 'check' || status?.phase === 'checking'
  // 「检查中」由检查按钮的 loading 承担，状态行不再重复显示。
  const lines = status ? [checking ? null : statusLine(status), manualLine(status)].flatMap((line) => (line ? [updateLineText(t, line)] : [])) : []
  const url = status ? manualUrl(status) : null

  return (
    <>
      <SettingsRow title={t('checkUpdate')} description={t('checkUpdateDesc')}>
        <div className="flex flex-col gap-1.5 desktop:items-end">
          {lines.map((line) => (
            <span key={line} className="text-xs text-muted-foreground desktop:text-right">
              {line}
            </span>
          ))}
          <div className="flex flex-wrap items-center gap-2 desktop:justify-end">
            {status && canCancel(status) ? (
              <Button loading={busy === 'cancel'} disabled={disabled || busy !== null} onClick={() => void run('cancel', () => rpc.agent.update.cancel())}>
                {t('cancel')}
              </Button>
            ) : null}
            {status && canInstall(status) ? (
              <Button variant="primary" loading={busy === 'install'} disabled={disabled || busy !== null} onClick={() => void run('install', () => rpc.agent.update.install())}>
                {t('updateRestartNow')}
              </Button>
            ) : null}
            {url ? (
              <Button variant="primary" onClick={() => openUrl(url)}>
                {t('updateFailedOpenSite')}
              </Button>
            ) : null}
            <Button
              loading={checking}
              disabled={disabled || checking}
              onClick={() => void run('check', () => rpc.agent.update.check({ channel: channel === 'frontier' ? 'frontier' : 'stable' }))}
            >
              {t('checkUpdate')}
            </Button>
          </div>
        </div>
      </SettingsRow>
      {status && status.notes.length > 0 ? (
        <SettingsCustomRow className="flex flex-col gap-4">
          {status.notes.slice(0, 10).map((note, index) => (
            <ReleaseNote key={note.version} note={note} locale={locale} divided={index > 0} />
          ))}
        </SettingsCustomRow>
      ) : null}
    </>
  )
}

function ExportRow() {
  const t = useT()
  const disabled = useSettingsReadOnly()
  const [busy, setBusy] = useState(false)
  const exportNow = async () => {
    if (busy) return
    setBusy(true)
    try {
      await exportLogs()
    } catch (error) {
      toast.error(rpcErrorText(error, t), t('logExportFailed'))
    } finally {
      setBusy(false)
    }
  }
  return (
    <SettingsRow title={t('logExportButton')}>
      <div className="flex desktop:justify-end">
        <Button variant="primary" loading={busy} disabled={disabled} onClick={() => void exportNow()}>
          {t('logExportButton')}
        </Button>
      </div>
    </SettingsRow>
  )
}

export function AboutSettings() {
  const t = useT()
  const version = useRpcSelector((state) => state.hello?.serviceVersion ?? '')

  return (
    <SettingsPage title={t('settingsCatAbout')} description={t('settingsCatAboutDesc')}>
      <SettingsSection title="FluxDown">
        <SettingsRow title={t('currentVersion')}>
          <span className="text-sm tabular text-foreground">{version ? `v${version}` : '—'}</span>
        </SettingsRow>
      </SettingsSection>
      <SettingsSection title={t('softwareUpdate')}>
        <PrefDropdownRow
          prefKey="general.update_channel"
          fallback="stable"
          titleKey="updateChannel"
          descKey="updateChannelDesc"
          options={[
            { value: 'stable', label: t('updateChannelStable') },
            { value: 'frontier', label: t('updateChannelFrontier') },
          ]}
        />
        <PrefSwitchRow prefKey="general.auto_check_update" fallback titleKey="autoCheckUpdate" descKey="autoCheckUpdateBackgroundDesc" />
        <UpdateRow />
        <SettingsRow title={t('webServerReleases')}>
          <LinkButtons links={[{ label: 'GitHub', url: SERVER_RELEASES }]} />
        </SettingsRow>
      </SettingsSection>
      <SettingsSection title={t('logExport')} subtitle={t('logExportDesc')}>
        <DaemonNumberRow configKey="log_max_size_mb" titleKey="logMaxSize" descKey="logMaxSizeDesc" unit="MB" />
        <ExportRow />
      </SettingsSection>
      <SettingsSection title={t('extensionCardTitle')} subtitle={t('extensionCardDesc')}>
        <SettingsRow title={t('extensionCardTitle')}>
          <LinkButtons
            links={[
              { label: 'Chrome', url: CHROME_STORE },
              { label: 'Firefox', url: FIREFOX_STORE },
              { label: 'Edge', url: EDGE_STORE },
            ]}
          />
        </SettingsRow>
        <SettingsRow title={t('donateTitle')}>
          <LinkButtons
            links={[
              { label: t('donateButton'), url: DONATE },
              { label: t('officialWebsite'), url: WEBSITE },
            ]}
          />
        </SettingsRow>
      </SettingsSection>
      <SettingsSection title={t('mobileOpenSource')}>
        <SettingsRow title="IP Geolocation by DB-IP">
          <LinkButtons links={[{ label: 'db-ip.com', url: DB_IP }]} />
        </SettingsRow>
        <SettingsRow title="flag-icons (MIT)">
          <LinkButtons links={[{ label: 'GitHub', url: FLAG_ICONS }]} />
        </SettingsRow>
      </SettingsSection>
    </SettingsPage>
  )
}
