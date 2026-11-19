// 受管组件子页：每个组件（ffmpeg / yt-dlp）一张卡片 —— 生效状态、系统 PATH、
// 手动路径、托管安装（版本列表 / 安装 / 更新 / 卸载 / 下载进度）。

import { CircleAlert, CircleCheck, Info, SquareTerminal, X } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../../../i18n'
import { METHOD, RpcError, call, rpc, rpcStore, useConfigValue, useDaemon, useServiceEvents } from '../../../../lib/rpc'
import type { ComponentKind, ComponentStatus, ComponentStatusDto, EventFrame } from '../../../../lib/rpc'
import { Badge, Button, Card, Icon, Input, InputWithAction, ProgressBar, Select, Spinner, confirmDialog, toast } from '../../../../ui'
import { extensionErrorText } from './errors'
import { formatBytes } from './logic'

/** 固定展示顺序（与 GPUI 组件页一致）。 */
const COMPONENT_KINDS: readonly ComponentKind[] = ['ffmpeg', 'ytdlp']

/** 手动指定可执行文件路径的 daemon 配置键（与引擎 `CONFIG_*_PATH` 一致）。 */
const MANUAL_PATH_KEYS: Record<ComponentKind, string> = {
  ffmpeg: 'component.ffmpeg.path',
  ytdlp: 'component.ytdlp.path',
}

const TITLE_KEYS: Record<ComponentKind, string> = { ffmpeg: 'componentsFfmpegTitle', ytdlp: 'componentsYtdlpTitle' }
const DESC_KEYS: Record<ComponentKind, string> = { ffmpeg: 'componentsFfmpegDesc', ytdlp: 'componentsYtdlpDesc' }
const RELEASE_URLS: Record<ComponentKind, string> = {
  ffmpeg: 'https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases',
  ytdlp: 'https://github.com/yt-dlp/yt-dlp/releases',
}
const MIRROR_KEY = 'component_mirror_base'
const PATH_HINT_KEYS: Record<ComponentKind, string> = {
  ffmpeg: 'componentsManualPathHintFfmpeg',
  ytdlp: 'componentsManualPathHintYtdlp',
}
const INSTALL_DESC_KEYS: Record<ComponentKind, string> = {
  ffmpeg: 'componentsInstallSectionDescFfmpeg',
  ytdlp: 'componentsInstallSectionDescYtdlp',
}

const SOURCE_KEYS: Record<string, string> = {
  manual: 'componentsSourceManual',
  managed: 'componentsSourceManaged',
}

/** 安装是长耗时 RPC（下载 + 解压），进度走引擎事件；超时放宽到 30 分钟。 */
const INSTALL_TIMEOUT_MS = 30 * 60_000
const MAX_CONFLICT_RETRIES = 3

const EMPTY_COMPONENTS: ComponentStatusDto[] = []
const selectComponents = (daemon: { components: ComponentStatusDto[] }) => daemon.components

/** `daemon.config.patch` 乐观并发：冲突时用服务端回带的最新 revision 重试。 */
async function patchConfigValue(key: string, value: string): Promise<void> {
  let revision = rpcStore.peek().snapshot?.daemon.config.revision ?? 0
  for (let attempt = 0; ; attempt += 1) {
    try {
      await rpc.daemon.config.patch({ expectedRevision: revision, values: { [key]: value } })
      return
    } catch (error) {
      if (attempt < MAX_CONFLICT_RETRIES && error instanceof RpcError && error.is('conflict') && error.revision !== undefined) {
        revision = error.revision
        continue
      }
      throw error
    }
  }
}

function StatusLine({ icon, iconClass, children }: { icon: LucideIcon; iconClass: string; children: ReactNode }) {
  return (
    <div className="flex items-start gap-2 text-xs text-muted-foreground">
      <span className="flex h-[1.25em] flex-none items-center">
        <Icon icon={icon} size="md" className={iconClass} />
      </span>
      <div className="min-w-0 flex-1 break-words">{children}</div>
    </div>
  )
}

function Divider() {
  return <div className="h-px w-full flex-none bg-hairline" />
}

interface InstallProgress {
  installing: boolean
  downloaded: number
  total: number
}

function ComponentCard({ kind, stale }: { kind: ComponentKind; stale: boolean }) {
  const t = useT()
  const title = t(TITLE_KEYS[kind])
  const components = useDaemon(selectComponents, EMPTY_COMPONENTS)
  const snapshotStatus = components.find((entry) => entry.component === kind)
  // `daemon.component.get` 回流：仅在快照里的该条目仍是当时那个对象时覆盖，快照一更新即让位。
  const [override, setOverride] = useState<{ base: ComponentStatusDto | undefined; status: ComponentStatusDto } | null>(null)
  const dto = override && override.base === snapshotStatus ? override.status : snapshotStatus
  const status: ComponentStatus | undefined = dto?.status

  // ── 手动路径 ──
  const configPath = useConfigValue(MANUAL_PATH_KEYS[kind])?.trim() ?? ''
  const [pathValue, setPathValue] = useState(configPath)
  const pathSynced = useRef(configPath)
  const pathInput = useRef<HTMLInputElement>(null)
  const [savingPath, setSavingPath] = useState(false)

  // 配置变更且输入框未聚焦时才覆盖用户输入；聚焦期间的变更在失焦后补同步。
  const syncFromConfig = useCallback(() => {
    if (pathSynced.current === configPath) return
    if (document.activeElement === pathInput.current) return
    pathSynced.current = configPath
    setPathValue(configPath)
  }, [configPath])
  useEffect(syncFromConfig, [syncFromConfig])

  const savePath = async (raw: string) => {
    const path = raw.trim()
    if (savingPath || path === configPath) return
    setSavingPath(true)
    pathSynced.current = path
    try {
      await patchConfigValue(MANUAL_PATH_KEYS[kind], path)
      // 重新探测生效状态（快照的 componentsChanged 事件也会随后到达）。
      const base = snapshotStatus
      void rpc.daemon.component
        .get({ component: kind })
        .then((status) => setOverride({ base, status }))
        .catch(() => undefined)
    } catch (error) {
      toast.error(extensionErrorText(t, error))
    } finally {
      setSavingPath(false)
    }
  }

  // ── 版本列表 ──
  const [versions, setVersions] = useState<string[]>([])
  const [versionsLoading, setVersionsLoading] = useState(false)
  const [versionsError, setVersionsError] = useState<string | null>(null)
  const [versionsRequested, setVersionsRequested] = useState(false)
  const [selected, setSelected] = useState<string | null>(null)

  const fetchVersions = async () => {
    if (versionsLoading) return
    setVersionsRequested(true)
    setVersionsLoading(true)
    setVersionsError(null)
    try {
      const result = await rpc.daemon.component.listVersions({ component: kind })
      setVersions(result.versions)
      setSelected((current) => {
        if (current !== null && result.versions.includes(current)) return current
        return result.latestStable === '' ? (result.versions[0] ?? null) : result.latestStable
      })
    } catch (error) {
      setVersionsError(extensionErrorText(t, error))
    } finally {
      setVersionsLoading(false)
    }
  }

  const managedSupported = status?.managedSupported ?? true
  const canFetch = status?.managedSupported === true
  const fetchVersionsRef = useRef(fetchVersions)
  fetchVersionsRef.current = fetchVersions
  // 版本列表只在「受支持且连接就绪」首次成立时懒拉一次，之后靠刷新按钮。
  useEffect(() => {
    if (canFetch && !versionsRequested && !stale) void fetchVersionsRef.current()
  }, [canFetch, versionsRequested, stale])

  // ── 安装 / 卸载 / 进度 ──
  const [progress, setProgress] = useState<InstallProgress>({ installing: false, downloaded: 0, total: 0 })
  const [uninstalling, setUninstalling] = useState(false)
  const installPending = useRef(false)
  const lastResult = useRef<{ ok: boolean; message: string } | null>(null)

  const onEvent = useCallback(
    (frame: EventFrame) => {
      const outer = frame.event
      if (outer.service !== 'agent' || outer.event.type !== 'daemon' || outer.event.data.type !== 'engine') return
      const message = outer.event.data.data
      if (message.type === 'componentProgress' && message.component === kind) {
        setProgress({ installing: true, downloaded: message.downloadedBytes, total: message.totalBytes })
      } else if (message.type === 'componentResult' && message.component === kind) {
        lastResult.current = { ok: message.ok, message: message.message }
        // 非本页发起的安装（如另一个客户端）：结果到达即结束进度展示。
        if (!installPending.current) setProgress((current) => ({ ...current, installing: false }))
      }
    },
    [kind],
  )
  useServiceEvents(onEvent)

  const install = async () => {
    if (installPending.current || uninstalling) return
    installPending.current = true
    lastResult.current = null
    setProgress({ installing: true, downloaded: 0, total: 0 })
    try {
      await call(METHOD.DAEMON_COMPONENT_INSTALL, { component: kind, version: selected ? selected : null }, { timeoutMs: INSTALL_TIMEOUT_MS })
      toast.success(t('componentsInstallSuccess', { name: title }))
    } catch (error) {
      // 引擎推送的结果携带真实错误说明；RPC 错误只有错误码。
      const pushed = lastResult.current as { ok: boolean; message: string } | null // 事件回调在 await 期间写入，TS 看不到
      const detail = pushed && !pushed.ok && pushed.message !== '' ? pushed.message : extensionErrorText(t, error)
      toast.error(t('componentsInstallFailed', { message: detail }))
    } finally {
      installPending.current = false
      setProgress((current) => ({ ...current, installing: false }))
    }
  }

  const uninstall = async () => {
    const ok = await confirmDialog({
      title: t('componentsUninstallConfirmTitle', { name: title }),
      description: t('componentsUninstallConfirmMsg', { name: title }),
      okLabel: t('componentsUninstallButton'),
      intent: 'destructive',
    })
    if (!ok || uninstalling || installPending.current) return
    setUninstalling(true)
    try {
      await rpc.daemon.component.uninstall({ component: kind })
      toast.success(t('componentsUninstallSuccess', { name: title }))
    } catch (error) {
      toast.error(t('componentsUninstallFailed', { message: extensionErrorText(t, error) }))
    } finally {
      setUninstalling(false)
    }
  }

  // ── 渲染 ──
  const hasManaged = status !== undefined && status.managedVersion !== ''
  const busy = stale || progress.installing || uninstalling
  const pathBusy = stale || savingPath
  const fraction = progress.total > 0 ? Math.min(1, Math.max(0, progress.downloaded / progress.total)) : null

  let statusRow: ReactNode
  if (!status) {
    // 快照已到但没有该组件：daemon 未编译组件支持（或当前平台不可用）。
    statusRow = stale ? (
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <Spinner size="md" />
        {t('componentsStatusLoading')}
      </div>
    ) : (
      <StatusLine icon={CircleAlert} iconClass="text-warning">
        {t('settingsUnsupportedOnPlatform')}
      </StatusLine>
    )
  } else if (status.source === 'none') {
    statusRow = (
      <StatusLine icon={CircleAlert} iconClass="text-warning">
        {t(status.managedSupported ? 'componentsStatusNotFound' : 'componentsStatusNotFoundUnsupported', { name: title })}
      </StatusLine>
    )
  } else {
    statusRow = (
      <StatusLine icon={CircleCheck} iconClass="text-success">
        <div className="flex flex-col gap-1">
          <div className="flex flex-wrap items-center gap-2">
            <Badge>{t(SOURCE_KEYS[status.source] ?? 'componentsSourceSystem')}</Badge>
            {status.version ? <span className="tabular">v{status.version}</span> : null}
          </div>
          <div className="break-all">{status.path}</div>
        </div>
      </StatusLine>
    )
  }

  return (
    <Card className="flex w-full flex-col gap-3 p-4">
      <div className="flex flex-col gap-0.5">
        <div className="text-sm font-medium text-foreground">{title}</div>
        <div className="text-xs text-muted-foreground">{t(DESC_KEYS[kind])}</div>
      </div>
      {statusRow}
      {status ? (
        <StatusLine icon={SquareTerminal} iconClass="text-muted-foreground">
          <span className="mr-1">{t('componentsSystemPathLabel')}</span>
          <span className="break-all">{status.systemPath === '' ? t('componentsSystemPathNotFound') : status.systemPath}</span>
        </StatusLine>
      ) : null}

      <Divider />

      <div className="flex w-full flex-col gap-2">
        <div className="flex flex-col gap-0.5">
          <div className="text-sm font-medium text-foreground">{t('componentsManualPathLabel')}</div>
          <div className="text-xs text-muted-foreground">{t('componentsManualPathDesc', { name: title })}</div>
        </div>
        <a
          className="self-start text-sm text-primary underline"
          href={RELEASE_URLS[kind]}
          target="_blank"
          rel="noopener noreferrer"
        >
          {t('componentsOfficialReleases')}
        </a>
        <InputWithAction
          input={
            <Input
              ref={pathInput}
              value={pathValue}
              disabled={pathBusy}
              placeholder={t(PATH_HINT_KEYS[kind])}
              aria-label={t('componentsManualPathLabel')}
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              onChange={(event) => setPathValue(event.target.value)}
              onBlur={syncFromConfig}
              onKeyDown={(event) => {
                if (event.key === 'Enter') void savePath(pathValue)
              }}
            />
          }
          action={
            <div className="flex items-center gap-2">
              <Button variant="outline" loading={savingPath} disabled={pathBusy} onClick={() => void savePath(pathValue)}>
                {t('componentsManualPathSave')}
              </Button>
              <Button
                variant="ghost"
                iconOnly
                className="text-muted-foreground hover:text-foreground"
                aria-label={t('componentsManualPathClear')}
                title={t('componentsManualPathClear')}
                disabled={pathBusy}
                onClick={() => {
                  setPathValue('')
                  void savePath('')
                }}
              >
                <Icon icon={X} size="md" />
              </Button>
            </div>
          }
        />
      </div>

      <Divider />

      {!managedSupported ? (
        <StatusLine icon={Info} iconClass="text-muted-foreground">
          {t('componentsManagedUnsupported', { name: title })}
        </StatusLine>
      ) : (
        <div className="flex w-full flex-col gap-2">
          <div className="flex items-center justify-between gap-2">
            <div className="min-w-0 text-sm font-medium text-foreground">{t('componentsInstallSectionTitle')}</div>
            <Button variant="ghost" loading={versionsLoading} disabled={versionsLoading || stale} onClick={() => void fetchVersions()}>
              {t('componentsFetchVersionsButton')}
            </Button>
          </div>
          <div className="text-xs text-muted-foreground">{t(INSTALL_DESC_KEYS[kind])}</div>
          {status && hasManaged ? <div className="text-xs text-muted-foreground">{t('componentsManagedVersionLabel', { version: status.managedVersion })}</div> : null}
          {versionsError ? (
            <div className="flex flex-wrap items-center gap-2">
              <div className="min-w-0 flex-[1_1_12rem]">
                <StatusLine icon={CircleAlert} iconClass="text-destructive">
                  {t('componentsVersionsLoadFailed', { message: versionsError })}
                </StatusLine>
              </div>
              <Button variant="outline" loading={versionsLoading} disabled={versionsLoading || stale} onClick={() => void fetchVersions()}>
                {t('componentsRetryVersions')}
              </Button>
            </div>
          ) : null}
          <div className="flex flex-wrap items-center gap-2">
            <div className="min-w-[10rem] flex-1">
              <Select
                value={selected ?? ''}
                onValueChange={setSelected}
                options={versions.map((version) => ({ value: version, label: version }))}
                placeholder={t(versionsLoading ? 'componentsVersionsLoading' : 'componentsVersionSelectPlaceholder')}
                disabled={versions.length === 0 || busy}
                aria-label={t('componentsInstallSectionTitle')}
                className="w-full"
              />
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <Button variant="primary" loading={progress.installing} disabled={busy} onClick={() => void install()}>
                {t(progress.installing ? 'componentsInstalling' : hasManaged ? 'componentsReinstallButton' : 'componentsInstallButton')}
              </Button>
              {hasManaged ? (
                <Button variant="outline" loading={uninstalling} disabled={busy} onClick={() => void uninstall()}>
                  {t('componentsUninstallButton')}
                </Button>
              ) : null}
            </div>
          </div>
          {progress.installing ? (
            <div className="flex w-full flex-col gap-1">
              <ProgressBar value={fraction} />
              <div className="tabular text-caption text-muted-foreground">
                {fraction === null
                  ? `${formatBytes(progress.downloaded)} · ${t('componentsInstallUnknownSize')}`
                  : `${(fraction * 100).toFixed(1)}%  ${formatBytes(progress.downloaded)} / ${formatBytes(progress.total)}`}
              </div>
            </div>
          ) : null}
        </div>
      )}
    </Card>
  )
}

function ComponentMirrorCard({ stale }: { stale: boolean }) {
  const t = useT()
  const configured = useConfigValue(MIRROR_KEY) ?? ''
  const [draft, setDraft] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const value = draft ?? configured
  const busy = stale || saving

  const save = async () => {
    if (busy || value.trim() === configured) return
    setSaving(true)
    try {
      await patchConfigValue(MIRROR_KEY, value.trim())
      setDraft(null)
    } catch (error) {
      toast.error(extensionErrorText(t, error))
    } finally {
      setSaving(false)
    }
  }

  return (
    <Card className="flex w-full flex-col gap-2 p-4">
      <label className="text-sm font-medium text-foreground" htmlFor="component-mirror">
        {t('mobileComponentMirrorBase')}
      </label>
      <p id="component-mirror-help" className="text-xs text-muted-foreground">
        {t('mobileComponentMirrorBaseDesc')}
      </p>
      <InputWithAction
        input={
          <Input
            id="component-mirror"
            aria-describedby="component-mirror-help"
            value={value}
            disabled={busy}
            placeholder={t('mobileComponentMirrorBasePlaceholder')}
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') void save()
            }}
          />
        }
        action={
          <Button variant="outline" loading={saving} disabled={busy} onClick={() => void save()}>
            {t('componentsManualPathSave')}
          </Button>
        }
      />
    </Card>
  )
}

export function ComponentsTab({ stale }: { stale: boolean }) {
  return (
    <div className="flex w-full flex-col gap-4">
      <ComponentMirrorCard stale={stale} />
      {COMPONENT_KINDS.map((kind) => (
        <ComponentCard key={kind} kind={kind} stale={stale} />
      ))}
    </div>
  )
}
