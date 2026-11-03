// 新建下载：字段、顺序与提交语义对齐 GPUI `pages/new_download.rs`（纯规则见 `model.ts`）。
//
// - 链接文本框（一行一条，`out=` / `checksum=` 选项行；默认收起约 3 行，标签行右侧可展开，批量输入自动展开）、打开 .torrent（浏览器选文件 → blob 上传）、导入 TXT（浏览器端读取）；
// - 保存目录 + 服务端目录选择器；文件名、线程数各占一行；「高级」折叠区（HTTP 认证 / 代理 / UA / Cookie / 校验 / 请求头 / 忽略 TLS）；
// - 底栏：取消、「稍后下载」分体按钮（队列菜单）、主按钮「开始下载」分体按钮（队列菜单）；
// - 单条 http(s) 链接先 `daemon.group.resolvePreview` 探测插件多文件清单，命中则进入清单选择建任务组；
// - 外部捕获（`agent.capture.*`）作为链接行并入表单，提交时经 `agent.capture.resolve` 确认，关闭时未确认的一并忽略。

import { ChevronDown, ChevronRight, FileText, FolderOpen, Plus, Sprout, X } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import type { ChangeEvent } from 'react'
import { useT } from '../../../i18n'
import { CUSTOM_CATEGORIES_PREF_KEY, LATER_QUEUE_ID, MAIN_QUEUE_ID, METHOD, RpcError, call, rpc, rpcStore, useAgent, useConfigValues, useConnection, useDaemon } from '../../../lib/rpc'
import { autoFollowDir, eligibleRoute, matchedCategory, modeDir, pickableCategories, routeBaseDir, routeFollows, routeFromCategories, routeSaveDir } from '../../../lib/categoryRoute'
import type { CategoryPickMode } from '../../../lib/categoryRoute'
import { categoriesFromPreference } from '../model/categories'
import { cloudPresenceKnown } from '../../../lib/cloud-presence'
import { describeUploadError } from '../../../lib/rpcErrorText'
import { ThreadsRiskCallout } from '../../../lib/ThreadsRiskCallout'
import type { AgentSnapshot, CloudDevice, JsonValue, LinkDeviceInfo, PendingCaptureDto, QueueDto, ResolvePreviewResponse } from '../../../lib/rpc'
import { Button, Dialog, DialogFooter, FieldError, FieldHint, FieldLabel, Form, FormField, FormRow, Icon, Input, InputWithAction, OptionGroup, OptionRow, Select, Spinner, Switch, Textarea, toast } from '../../../ui'
import type { MenuEntry } from '../../../ui'
import { FsPickerDialog } from './FsPickerDialog'
import { ManifestSelectDialog } from './ManifestSelectDialog'
import type { ManifestBaseOptions } from './ManifestSelectDialog'
import { DEFAULT_HASH_ALGORITHM, HASH_ALGORITHMS, MAX_THREADS, PROXY_CHOICES, THREAD_PRESETS, UA_PRESET_CUSTOM, UA_PRESET_DEFAULT, UA_PRESET_KEYS } from './model'
import type { DraftOptions, ProxyChoice, ThreadChoice, UrlEntry } from './model'
import { appendEntries, buildBatch, buildRequests, captureEntry, checksumSpec, customSegments, detectUaPreset, entryToText, isHttpUrl, isMagnetUrl, manualProxyUrl, parseEntries, parseInput, proxyWire, threadChoiceFromSegments, uaPresetValue } from './model'
import { applyHints, emptyHints, emptyTracking, hintsEqual } from './aria2c'
import type { FillTracking, RequestHints } from './aria2c'
import { IMPORT_CHUNK_SIZE, importNotice, runImport } from './importRunner'
import type { ImportProgress } from './importRunner'
import { SplitButton } from './SplitButton'
import { capturesShown, closeNewDownload, openSeedLocal } from './store'
import type { NewDownloadSession } from './store'
import { isTorrentFile, submitTorrentFiles } from './torrents'
import { SaveDirCombo } from './SaveDirCombo'
import { CategoryPicker } from './CategoryPicker'
import {
  CATEGORY_SAVE_DIR_HISTORY_PREF,
  SAVE_DIR_HISTORY_ENABLED_PREF,
  SAVE_DIR_HISTORY_PREF,
  readCategorySaveDirHistory,
  readSaveDirHistory,
  recordCategorySaveDir,
  recordSaveDir,
  saveDirChoices,
} from '../../../lib/saveDirHistory'
import { accountErrorKey } from '../../settings/sections/account/errorText'
import { buildRemoteTargets, checkRemoteSaveDir, dispatchRoute, LOCAL_TARGET, linkDispatchParams, pathExample, remoteDispatchParams, resolveTargetValue, summarizeDispatch } from './target'
import type { RemoteTarget } from './target'
import { queueLabel } from './utils'

/** resolvePreview 探测超时（对齐桌面端 90s）：超时 / 异常 / 空清单 / error 均静默回退直接建任务。 */
const RESOLVE_PREVIEW_TIMEOUT_MS = 90_000
const SITE_AUTH_LOOKUP_DEBOUNCE_MS = 250
/** 失败原因最多渲染的行数（超出的只显示计数，避免上万行 DOM）。 */
const MAX_FAILURE_ROWS = 50
/** 链接输入框随内容自增高的上限（行）；超出后框内滚动。与 GPUI `URLS_MAX_ROWS` 一致。 */
const URLS_MAX_ROWS = 10

const EMPTY_CAPTURES: readonly PendingCaptureDto[] = []
const EMPTY_QUEUES: readonly QueueDto[] = []
const EMPTY_CLOUD: readonly CloudDevice[] = []
const EMPTY_LINKED: readonly LinkDeviceInfo[] = []

interface FormContext {
  saveDir: string
  queueId: string
  segments: number
}

/** 开启「记住上次目录」且有记录时沿用；队列优先入参、其次配置 `default_queue_id`、最后主队列。 */
function buildContext(snapshot: AgentSnapshot | null, wantedQueue: string | undefined): FormContext {
  const config = snapshot?.daemon.config.values ?? {}
  const prefs = (snapshot?.preferences.values ?? {}) as Record<string, unknown>
  const cfg = (key: string) => (config[key] ?? '').trim()
  const remember = prefs['download.remember_last_save_dir'] === true
  const last = typeof prefs['download.last_save_dir'] === 'string' ? (prefs['download.last_save_dir'] as string) : ''
  const saveDir = remember && last !== '' ? last : cfg('default_save_dir') || snapshot?.daemon.runtimeStats.saveDir || ''
  const queueId = wantedQueue ?? (cfg('default_queue_id') || MAIN_QUEUE_ID)
  const queueSegments = snapshot?.daemon.queues.find((queue) => queue.queueId === queueId)?.defaultSegments ?? 0
  const segments = queueSegments > 0 ? queueSegments : Number.parseInt(cfg('default_segments'), 10) || 0
  return { saveDir, queueId, segments }
}

interface HeaderRow {
  id: number
  key: string
  value: string
}

export function NewDownloadDialog({ session }: { session: NewDownloadSession }) {
  const t = useT()
  const context = useMemo(() => buildContext(rpcStore.peek().snapshot, session.queueId), [session.queueId])
  const config = useConfigValues()
  const manualProxy = useMemo(() => manualProxyUrl(config), [config])
  const queues = useDaemon((daemon) => [...daemon.queues].sort((a, b) => a.position - b.position), EMPTY_QUEUES, (a, b) => a.length === b.length && a.every((item, index) => item === b[index]))
  const pendingCaptures = useAgent((snapshot) => snapshot.pendingCaptures, EMPTY_CAPTURES)
  const cloudDevices = useAgent((snapshot) => snapshot.cloudDevices, EMPTY_CLOUD)
  const linkedDevices = useAgent((snapshot) => snapshot.linkedDevices, EMPTY_LINKED)
  const cloudConnection = useAgent((snapshot) => snapshot.cloudConnection, undefined)
  const localReady = useConnection().phase === 'ready'
  const cloudReady = cloudPresenceKnown(cloudConnection, localReady)
  const remoteTargets = useMemo(
    () => buildRemoteTargets(cloudDevices, linkedDevices, { cloud: cloudReady, local: localReady }),
    [cloudDevices, linkedDevices, cloudReady, localReady],
  )
  const [targetValue, setTargetValue] = useState(session.target ?? LOCAL_TARGET)
  // 目标设备消失（登出 / 解除配对）时回落到本服务器。
  const target: RemoteTarget | null = resolveTargetValue(targetValue, remoteTargets)
  const [remoteSaveDir, setRemoteSaveDir] = useState('')
  const historyEnabled = useAgent((snapshot) => snapshot.preferences.values[SAVE_DIR_HISTORY_ENABLED_PREF] !== false, true)
  const historyValue = useAgent((snapshot) => snapshot.preferences.values[SAVE_DIR_HISTORY_PREF], undefined)
  const saveDirHistory = useMemo(() => (historyEnabled ? readSaveDirHistory(historyValue) : null), [historyEnabled, historyValue])
  const categoryHistoryValue = useAgent((snapshot) => snapshot.preferences.values[CATEGORY_SAVE_DIR_HISTORY_PREF], undefined)
  const categoryHistory = useMemo(() => readCategorySaveDirHistory(categoryHistoryValue), [categoryHistoryValue])

  const [text, setText] = useState(() => session.initialUrls.join('\n'))
  const [captures, setCaptures] = useState<PendingCaptureDto[]>([])
  const [saveDir, setSaveDir] = useState(context.saveDir)
  const [threads, setThreads] = useState<ThreadChoice>(() => threadChoiceFromSegments(context.segments))
  const [customThreads, setCustomThreads] = useState(context.segments > 0 ? String(context.segments) : '')
  const [rename, setRename] = useState('')
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const [httpUser, setHttpUser] = useState('')
  const [httpPassword, setHttpPassword] = useState('')
  const [saveSiteAuth, setSaveSiteAuth] = useState(false)
  const [proxyChoice, setProxyChoice] = useState<ProxyChoice>('follow')
  const [customProxy, setCustomProxy] = useState('')
  const [ignoreTls, setIgnoreTls] = useState(false)
  const [uaPreset, setUaPreset] = useState(UA_PRESET_DEFAULT)
  const [userAgent, setUserAgent] = useState('')
  const [cookie, setCookie] = useState('')
  const [hashAlgorithm, setHashAlgorithm] = useState(DEFAULT_HASH_ALGORITHM)
  const [checksum, setChecksum] = useState('')
  const [headers, setHeaders] = useState<HeaderRow[]>([])
  const headerSeq = useRef(0)
  const [phase, setPhase] = useState<'idle' | 'resolving' | 'submitting'>('idle')
  const [failures, setFailures] = useState<{ url: string; message: string }[]>([])
  const [failureOverflow, setFailureOverflow] = useState(0)
  const [notice, setNotice] = useState<string | null>(null)
  const [progress, setProgress] = useState<ImportProgress | null>(null)
  const [cancelling, setCancelling] = useState(false)
  const importAbort = useRef<AbortController | null>(null)
  const [manifest, setManifest] = useState<{ preview: ResolvePreviewResponse; sourceUrl: string; base: ManifestBaseOptions } | null>(null)
  const [pickerOpen, setPickerOpen] = useState(false)
  const [busyFiles, setBusyFiles] = useState(false)
  const torrentInput = useRef<HTMLInputElement>(null)
  const txtInput = useRef<HTMLInputElement>(null)
  const resolveToken = useRef(0)

  const parsed = useMemo(() => parseInput(text, false), [text])
  const entries = parsed.entries

  // aria2c 命令带出的 Cookie / UA / 请求头回填到高级字段（规则同桌面端，见 `aria2c.ts`）：
  // 与上次处理的 hints 相同则不处理；用户编辑过的字段 / 行不再覆盖。
  const lastHints = useRef<RequestHints>(emptyHints())
  const fillTracking = useRef<FillTracking>(emptyTracking())
  useEffect(() => {
    if (hintsEqual(parsed.hints, lastHints.current)) return
    lastHints.current = parsed.hints
    const result = applyHints({ cookie, userAgent, rows: headers }, fillTracking.current, parsed.hints, () => {
      headerSeq.current += 1
      return headerSeq.current
    })
    fillTracking.current = result.tracking
    if (result.cookie !== cookie) setCookie(result.cookie)
    if (result.userAgent !== userAgent) {
      setUserAgent(result.userAgent)
      setUaPreset(detectUaPreset(result.userAgent))
    }
    if (result.rows.length !== headers.length || result.rows.some((row, index) => row !== headers[index])) setHeaders(result.rows)
    if (result.filled) setAdvancedOpen(true)
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 仅在命令头部信息变化时回填，表单当前值取渲染时快照
  }, [parsed.hints])
  const count = entries.length
  const batch = count > 1
  const allMagnet = count > 0 && entries.every((entry) => isMagnetUrl(entry.url))
  const busy = phase !== 'idle' || busyFiles
  const remoteDirCheck = target ? checkRemoteSaveDir(remoteSaveDir, target.pathStyle) : 'ok'
  const canSubmit = !busy && count > 0 && (target ? remoteDirCheck === 'ok' : saveDir.trim() !== '')

  // ── 分类下拉：Auto 模式下目录框跟随识别出的文件名（规则见 `lib/categoryRoute.ts`）──
  const categoriesPref = useAgent((snapshot) => snapshot.preferences.values[CUSTOM_CATEGORIES_PREF_KEY], undefined)
  const categories = useMemo(() => categoriesFromPreference(categoriesPref), [categoriesPref])
  const categoryRoute = useMemo(() => routeFromCategories(categories), [categories])
  const categoryOptions = useMemo(() => pickableCategories(categories), [categories])
  const [categoryMode, setCategoryMode] = useState<CategoryPickMode>({ kind: 'auto' })
  /** 程序最近一次写入目录框的值（自动跟随 / 切换模式 / 捕获预填）；目录框与之不等即视为用户手改。 */
  const lastWrittenDir = useRef<string | null>(null)
  /** 用户直接编辑/浏览选择后明确优先于自动路由，包括目录恰为默认或分类目录。 */
  const saveDirManuallyChosen = useRef(false)
  const categoryShown = !target
  const predictedName = rename.trim() !== '' ? rename.trim() : (entries[0]?.fileName ?? '')
  const predictedUrl = entries[0]?.url ?? ''
  const predictedDir = categoryRoute ? routeSaveDir(categoryRoute, predictedName, predictedUrl) : null
  useEffect(() => {
    if (!categoryShown || categoryMode.kind !== 'auto') return
    const next = autoFollowDir(saveDir, context.saveDir, lastWrittenDir.current, predictedDir, saveDirManuallyChosen.current)
    if (next === null) return
    lastWrittenDir.current = next
    if (next !== saveDir) setSaveDir(next)
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 仅在预测输入 / 模式变化时跟随，目录框当前值取渲染时快照
  }, [categoryShown, categoryMode.kind, predictedDir])
  const pickCategory = (mode: CategoryPickMode) => {
    saveDirManuallyChosen.current = false
    setCategoryMode(mode)
    const next = modeDir(mode, categoryOptions, context.saveDir, predictedDir)
    lastWrittenDir.current = next
    setSaveDir(next)
  }
  const chooseSaveDir = (dir: string) => {
    saveDirManuallyChosen.current = true
    setSaveDir(dir)
  }

  // ── 外部捕获并入表单 ──
  const capturesRef = useRef<PendingCaptureDto[]>([])
  capturesRef.current = captures
  const injectionsDone = useRef(0)
  const textRef = useRef(text)
  textRef.current = text

  useEffect(() => {
    const fresh = pendingCaptures.filter((capture) => !capturesShown.has(capture.transactionId))
    if (fresh.length > 0) {
      for (const capture of fresh) capturesShown.add(capture.transactionId)
      const current = textRef.current
      if (current.trim() === '') {
        const dir = fresh.map((capture) => capture.saveDir.trim()).find((value) => value !== '')
        if (dir) {
          // 非默认 / 分类目录的捕获目录是显式选择；不得再被 Auto 路由覆盖。
          const followsRoute = categoryRoute !== null && routeFollows(categoryRoute, dir, context.saveDir)
          saveDirManuallyChosen.current = !followsRoute && dir !== context.saveDir
          if (followsRoute || dir === context.saveDir) lastWrittenDir.current = dir
          setSaveDir(dir)
        }
      }
      setText(appendEntries(current, fresh.map((capture) => captureEntry(capture.url, capture.fileName))))
      setCaptures((prev) => [...prev, ...fresh])
    }
    // 只保留 agent 仍在等待确认的捕获（其余已在别处处理 / agent 重启丢失）；对应链接行保留为普通链接。
    setCaptures((prev) => {
      const kept = prev.filter((capture) => pendingCaptures.some((pending) => pending.transactionId === capture.transactionId))
      return kept.length === prev.length ? prev : kept
    })
  }, [pendingCaptures, categoryRoute, context.saveDir])

  // 关闭 / 取消时，仍未确认的捕获一并忽略（agent 不留悬挂事务）。
  useEffect(
    () => () => {
      for (const capture of capturesRef.current) {
        void rpc.agent.capture.resolve({ transactionId: capture.transactionId, accepted: false }).catch(() => undefined)
      }
    },
    [],
  )

  // ── 会话期间追加的链接 / 种子 ──
  useEffect(() => {
    for (const injection of session.injections) {
      if (injection.id <= injectionsDone.current) continue
      injectionsDone.current = injection.id
      if (injection.urls.length > 0) {
        setText((current) => {
          const existing = new Set(current.split('\n').map((line) => line.trim()))
          const add = injection.urls.filter((url) => !existing.has(url))
          return add.length === 0 ? current : `${current.replace(/\s+$/, '')}${current.trim() === '' ? '' : '\n'}${add.join('\n')}`
        })
      }
      if (injection.torrentFiles.length > 0) {
        void submitTorrentFiles(injection.torrentFiles, { saveDir: saveDir.trim(), queueId: context.queueId })
      }
    }
  }, [session.injections, saveDir, context.queueId])

  // ── 站点凭据自动回填 ──
  const authDirty = useRef(false)
  const authFilled = useRef(false)
  const usesBrowserAuth =
    count === 1 && captures.some((capture) => capture.url === entries[0].url && capture.headerNames.some((name) => name.toLowerCase() === 'authorization'))
  const siteTarget = count === 1 && isHttpUrl(entries[0].url) && !usesBrowserAuth ? entries[0].url : null

  useEffect(() => {
    if (authDirty.current) return
    if (siteTarget === null) {
      if (authFilled.current) {
        setHttpUser('')
        setHttpPassword('')
        authFilled.current = false
      }
      return
    }
    let cancelled = false
    const timer = setTimeout(() => {
      rpc.daemon.siteAuth
        .match({ url: siteTarget })
        .then((credential) => {
          if (cancelled || authDirty.current) return
          if (credential) {
            setHttpUser(credential.user)
            setHttpPassword(credential.pass)
            authFilled.current = true
          } else if (authFilled.current) {
            setHttpUser('')
            setHttpPassword('')
            authFilled.current = false
          }
        })
        .catch(() => undefined)
    }, SITE_AUTH_LOOKUP_DEBOUNCE_MS)
    return () => {
      cancelled = true
      clearTimeout(timer)
    }
  }, [siteTarget])

  // ── 表单动作 ──
  const changeThreads = (next: ThreadChoice) => {
    if (next === 'custom' && threads !== 'custom') {
      // 进入自定义：若当前是数字预设则预填，便于快速编辑。
      setCustomThreads(threads === 'auto' ? '' : threads)
    }
    setThreads(next)
  }

  const changeUaPreset = (key: string) => {
    setUaPreset(key)
    if (key !== UA_PRESET_CUSTOM) setUserAgent(uaPresetValue(key))
  }

  const editUserAgent = (value: string) => {
    setUserAgent(value)
    setUaPreset(detectUaPreset(value))
  }

  const segments = threads === 'auto' ? 0 : threads === 'custom' ? customSegments(customThreads) : Math.min(Number(threads), MAX_THREADS)

  const headerMap = (): Record<string, string> => {
    const map: Record<string, string> = {}
    for (const row of headers) {
      const key = row.key.trim()
      if (key !== '') map[key] = row.value.trim()
    }
    return map
  }

  // 分类记忆键：指定分类 = 该分类；自动 = 命中分类；不分类 / 无分类选择器 = 无。
  const memoryCategoryId = !categoryShown
    ? null
    : categoryMode.kind === 'category'
      ? categoryMode.id
      : categoryMode.kind === 'auto'
        ? (matchedCategory(categoryOptions, predictedDir)?.id ?? null)
        : null
  // 保存目录历史（偏好关闭时为空）：该分类记忆在前、全局历史在后，排除当前输入，交给组合框下拉。
  const saveDirOptions = saveDirHistory ? saveDirChoices(memoryCategoryId ? (categoryHistory[memoryCategoryId] ?? []) : [], saveDirHistory, saveDir) : []

  const draftOptions = (later: boolean, queueOverride: string | undefined): DraftOptions => ({
    saveDir: saveDir.trim(),
    segments,
    cookies: cookie.trim(),
    proxyUrl: proxyWire(proxyChoice, manualProxy, customProxy),
    userAgent: userAgent.trim(),
    queueId: queueOverride ?? (later ? LATER_QUEUE_ID : context.queueId),
    checksum: checksumSpec(hashAlgorithm, checksum),
    ignoreTlsErrors: ignoreTls,
    headers: headerMap(),
    startPaused: later,
    rename: rename.trim(),
    httpUser: httpUser.trim(),
    httpPassword,
    saveSiteAuth,
  })

  const rememberSaveDir = (dir: string) => {
    // 尽力而为：失败不影响建任务；无条件记录，「记住上次目录」开关开启后立即生效。
    const values: Record<string, JsonValue> = { 'download.last_save_dir': dir }
    // 历史（偏好关闭时不记）按提交时的最新快照计算，与 GPUI `record_save_dir_history` 同规则。
    const prefs = rpcStore.peek().snapshot?.preferences.values ?? {}
    if (prefs[SAVE_DIR_HISTORY_ENABLED_PREF] !== false) {
      const next = recordSaveDir(readSaveDirHistory(prefs[SAVE_DIR_HISTORY_PREF]), dir)
      if (next) values[SAVE_DIR_HISTORY_PREF] = next
      // 分类记忆：有分类键且目录框被手改时，记到该分类名下。
      if (memoryCategoryId && saveDirManuallyChosen.current) {
        const nextByCategory = recordCategorySaveDir(readCategorySaveDirHistory(prefs[CATEGORY_SAVE_DIR_HISTORY_PREF]), memoryCategoryId, dir)
        if (nextByCategory) values[CATEGORY_SAVE_DIR_HISTORY_PREF] = nextByCategory
      }
    }
    void rpc.agent.preferences.patch({ values, sync: false }).catch(() => undefined)
  }

  /** 逐条下发到云账号设备 / 已配对设备；成功的从文本框移除，失败的保留并逐条显示原因。 */
  const submitRemote = async (remote: RemoteTarget) => {
    setFailures([])
    setPhase('submitting')
    const renamed = entries.length === 1 ? rename.trim() : ''
    const route = dispatchRoute(remote)
    const results = await Promise.allSettled(
      entries.map((entry) => {
        const input = { url: entry.url, fileName: renamed !== '' ? renamed : entry.fileName }
        return route.via === 'cloud'
          ? rpc.agent.remote.dispatch(remoteDispatchParams(remote.id, input, remoteSaveDir))
          : rpc.agent.link.dispatch(linkDispatchParams(route.fingerprint, input, remoteSaveDir))
      }),
    )
    const summary = summarizeDispatch(results)
    if (summary.failed === 0) {
      if (route.via === 'link') toast.key('linkDispatchedToDevice', 'success', { device: remote.name })
      else toast.key(remote.online === false ? 'downloadToDispatchedOffline' : 'downloadToDispatched', 'success', { count: summary.ok, device: remote.name })
      closeNewDownload()
      return
    }
    const failed: { entry: UrlEntry; key: string }[] = []
    results.forEach((result, index) => {
      if (result.status === 'rejected') failed.push({ entry: entries[index], key: accountErrorKey(result.reason) })
    })
    if (summary.ok > 0) toast.key('downloadToPartial', 'warning', { ok: summary.ok, failed: summary.failed, device: remote.name })
    setText(failed.map((item) => entryToText(item.entry)).join('\n'))
    setFailures(failed.map((item) => ({ url: item.entry.url, message: t(item.key) })))
    setPhase('idle')
  }

  const submit = async (later: boolean, queueOverride?: string) => {
    if (!canSubmit) return
    if (target) {
      await submitRemote(target)
      return
    }
    setFailures([])
    setFailureOverflow(0)
    setNotice(null)
    const options = draftOptions(later, queueOverride)
    const requests = buildRequests(entries, options)

    // 单条 http(s) 链接（非外部捕获）：先探测是否为多文件清单。
    const single = requests.length === 1 ? requests[0] : null
    if (single && isHttpUrl(single.url) && !captures.some((capture) => capture.url === single.url)) {
      const token = ++resolveToken.current
      setPhase('resolving')
      try {
        const preview = await call<ResolvePreviewResponse>(
          METHOD.DAEMON_GROUP_RESOLVE_PREVIEW,
          {
            url: single.url,
            cookies: options.cookies || undefined,
            userAgent: options.userAgent || undefined,
            extraHeaders: Object.keys(options.headers).length > 0 ? options.headers : undefined,
          },
          { timeoutMs: RESOLVE_PREVIEW_TIMEOUT_MS },
        )
        if (token !== resolveToken.current) return // 用户取消了探测
        if (!preview.error && preview.items.length > 0) {
          const followed = eligibleRoute(categoryRoute, categoryMode, saveDirManuallyChosen.current, options.saveDir, context.saveDir)
          // Group creation accepts categoryRoute; retain the form's base directory and routing policy.
          setManifest({
            preview,
            sourceUrl: single.url,
            base: {
              saveDir: followed ? routeBaseDir(followed, options.saveDir, context.saveDir) : options.saveDir,
              offeredSaveDir: context.saveDir,
              // 「稍后下载」触发的清单：主按钮仍是「开始下载」，必须落到开始队列而非稍后队列。
              queueId: later ? context.queueId : options.queueId,
              segments: options.segments,
              cookies: options.cookies,
              userAgent: options.userAgent,
              proxyUrl: options.proxyUrl,
              headers: options.headers,
              ignoreTlsErrors: options.ignoreTlsErrors,
              categoryRoute: followed ?? undefined,
            },
          })
          setPhase('idle')
          return
        }
      } catch {
        if (token !== resolveToken.current) return
        // 超时 / 异常：与预解析前完全一致，直接建任务。
      }
    }

    setPhase('submitting')
    rememberSaveDir(options.saveDir)
    // 保留在文本框里的条目（失败 / 未提交）及其错误；外部捕获逐条确认，其余走分块 createMany。
    const retained: { index: number; error: unknown }[] = []
    const consumed = new Set<string>()
    const plain: number[] = []
    for (const [index, request] of requests.entries()) {
      const capture = captures.find((item) => item.url === request.url)
      if (!capture) {
        plain.push(index)
        continue
      }
      try {
        await rpc.agent.capture.resolve({
          transactionId: capture.transactionId,
          accepted: true,
          disableCategoryRoute: eligibleRoute(categoryRoute, categoryMode, saveDirManuallyChosen.current, options.saveDir, context.saveDir) === null,
          request,
        })
        consumed.add(capture.transactionId)
      } catch (error) {
        retained.push({ index, error })
      }
    }
    if (consumed.size > 0) {
      capturesRef.current = capturesRef.current.filter((capture) => !consumed.has(capture.transactionId))
      setCaptures(capturesRef.current)
    }

    let noticeText: string | null = null
    if (plain.length > 0) {
      const batch = buildBatch(
        plain.map((index) => entries[index]),
        options,
        entries.length === 1,
      )
      const { items } = batch
      let { common } = batch
      // 分类路由：只对本机提交生效；目录仍是表单初始值 / 分类目录时交给引擎按文件名定落点；
      // 分类下拉选了「不分类」/ 指定分类时用户已显式定目录，不路由。
      const followed = eligibleRoute(categoryRoute, categoryMode, saveDirManuallyChosen.current, options.saveDir, context.saveDir)
      if (followed) common = { ...common, saveDir: routeBaseDir(followed, options.saveDir, context.saveDir) }
      const controller = new AbortController()
      importAbort.current = controller
      setCancelling(false)
      const chunked = items.length > IMPORT_CHUNK_SIZE
      setProgress(chunked ? { total: items.length, done: 0, failed: 0, retry: null } : null)
      const report = await runImport({
        common,
        items,
        categoryRoute: followed,
        call: rpc.daemon.task.createMany,
        sleep: (ms) => new Promise<void>((resolve) => setTimeout(resolve, ms)),
        signal: controller.signal,
        onProgress: chunked ? setProgress : undefined,
      })
      importAbort.current = null
      setProgress(null)
      report.outcomes.forEach((outcome, position) => {
        if (outcome.status === 'created') return
        // 失败 → 带原因；未提交（取消 / 重发用尽）→ 只放回文本框。
        const error = outcome.status === 'failed' ? new RpcError({ code: -32000, message: '', data: outcome.error }) : null
        retained.push({ index: plain[position], error })
      })
      const summary = importNotice(report)
      const added = summary.added.toLocaleString()
      const total = summary.total.toLocaleString()
      const reason = summary.error ? describeUploadError(new RpcError({ code: -32000, message: '', data: summary.error }), t) : null
      if (summary.kind === 'cancelled') {
        const head = t('importCancelled', { added, total })
        noticeText = reason ? `${head}: ${reason}` : head
        if (retained.length === 0) toast.key('importCancelled', 'success', { added, total })
      } else if (summary.kind === 'partial') {
        const head = t('importPartial', { added, total, failed: summary.failed.toLocaleString() })
        noticeText = reason ? `${head}: ${reason}` : head
      } else if (summary.kind === 'failed') {
        noticeText = reason
      }
    }

    if (retained.length === 0) {
      closeNewDownload()
      return
    }
    // 保留未成功的行（含选项行），成功的从文本框移除；失败原因只渲染前若干条。
    retained.sort((a, b) => a.index - b.index)
    setText(retained.map((item) => entryToText(entries[item.index])).join('\n'))
    const withReason = retained.filter((item) => item.error !== null)
    setFailures(withReason.slice(0, MAX_FAILURE_ROWS).map((item) => ({ url: entries[item.index].url, message: describeUploadError(item.error, t) })))
    setFailureOverflow(Math.max(0, withReason.length - MAX_FAILURE_ROWS))
    setNotice(noticeText)
    setPhase('idle')
  }

  const pickTorrents = async (event: ChangeEvent<HTMLInputElement>) => {
    const files = [...(event.target.files ?? [])].filter(isTorrentFile)
    event.target.value = ''
    if (files.length === 0) return
    setBusyFiles(true)
    try {
      const created = await submitTorrentFiles(files, { saveDir: saveDir.trim(), queueId: context.queueId })
      if (created > 0) closeNewDownload()
    } catch (error) {
      toast.error(describeUploadError(error, t))
    } finally {
      setBusyFiles(false)
    }
  }

  const importTxt = async (event: ChangeEvent<HTMLInputElement>) => {
    const files = [...(event.target.files ?? [])].filter((file) => /\.te?xt$/i.test(file.name))
    event.target.value = ''
    if (files.length === 0) return
    setBusyFiles(true)
    try {
      const imported: UrlEntry[] = []
      for (const file of files) {
        // 单个文件读取失败跳过，继续处理其他文件。
        try {
          imported.push(...parseEntries(await file.text(), true))
        } catch {
          continue
        }
      }
      if (imported.length === 0) {
        toast.key('importTxtNoUrls', 'warning')
        return
      }
      setText((current) => appendEntries(current, imported))
      toast.key('importTxtFound', 'success', { count: imported.length })
    } finally {
      setBusyFiles(false)
    }
  }

  const queueMenu = (later: boolean): MenuEntry[] =>
    queues.map((queue) => ({ type: 'item', key: queue.queueId, label: queueLabel(t, queue), onSelect: () => void submit(later, queue.queueId) }))

  const startQueue = queues.find((queue) => queue.queueId === context.queueId)
  const laterQueue = queues.find((queue) => queue.queueId === LATER_QUEUE_ID)

  const proxyOptions = PROXY_CHOICES.map((choice) => {
    const labels: Record<ProxyChoice, string> = {
      follow: t('taskProxyChoiceFollow'),
      direct: t('taskProxyChoiceDirect'),
      system: t('taskProxyChoiceSystem'),
      globalManual: t('taskProxyChoiceGlobalManual'),
      custom: t('taskProxyChoiceCustom'),
    }
    if (choice === 'globalManual') {
      return manualProxy === ''
        ? { value: choice, label: `${labels[choice]}（${t('proxyNotConfigured')}）`, disabled: true }
        : { value: choice, label: `${labels[choice]} · ${manualProxy}` }
    }
    return { value: choice, label: labels[choice] }
  })

  const threadOptions = [
    { value: 'auto' as ThreadChoice, label: t('auto') },
    ...THREAD_PRESETS.map((value) => ({ value: `${value}` as ThreadChoice, label: String(value) })),
    { value: 'custom' as ThreadChoice, label: t('customThreads') },
  ]

  const uaLabel = (key: string): string => {
    switch (key) {
      case 'chrome':
        return t('userAgentPresetChrome')
      case 'firefox':
        return t('userAgentPresetFirefox')
      case 'edge':
        return t('userAgentPresetEdge')
      case 'safari':
        return t('userAgentPresetSafari')
      case UA_PRESET_CUSTOM:
        return t('userAgentPresetCustom')
      default:
        return t('queueUaInheritGlobal')
    }
  }

  const showAuth = !batch && !allMagnet
  const startLabel = count > 1 ? t('startBatchDownload', { count }) : t('startDownload')

  const cancelImport = () => {
    setCancelling(true)
    importAbort.current?.abort()
  }
  const progressLabel = !progress
    ? ''
    : cancelling
      ? t('importCancelling')
      : progress.retry
        ? t('importRetrying', { seconds: progress.retry.delaySeconds, attempt: progress.retry.attempt, max: progress.retry.max })
        : t('importProgress', { done: progress.done.toLocaleString(), total: progress.total.toLocaleString() })

  return (
    <>
      <Dialog
        open
        onOpenChange={(open) => !open && !busy && closeNewDownload()}
        size="lg"
        modalLocked={phase === 'submitting'}
        title={t('newDownload')}
        description={t('batchDownloadDesc')}
        footer={
          phase === 'resolving' ? (
            <div className="flex items-center justify-between gap-3">
              <span className="flex min-w-0 items-center gap-2 text-sm text-muted-foreground">
                <Spinner />
                <span className="truncate">{t('manifestResolvingLabel')}</span>
              </span>
              <Button
                variant="outline"
                onClick={() => {
                  resolveToken.current += 1
                  setPhase('idle')
                }}
              >
                {t('manifestResolvingCancel')}
              </Button>
            </div>
          ) : progress ? (
            <div className="flex items-center justify-between gap-3">
              <div className="flex min-w-0 flex-1 flex-col gap-1">
                <span className="truncate text-sm text-muted-foreground tabular">{progressLabel}</span>
                <div
                  role="progressbar"
                  aria-valuemin={0}
                  aria-valuemax={progress.total}
                  aria-valuenow={progress.done}
                  className="h-1.5 w-full overflow-hidden rounded-full bg-hairline"
                >
                  <div className="h-full bg-primary transition-[width]" style={{ width: `${progress.total > 0 ? (progress.done / progress.total) * 100 : 0}%` }} />
                </div>
              </div>
              <Button variant="outline" disabled={cancelling} onClick={cancelImport}>
                {t('cancel')}
              </Button>
            </div>
          ) : (
            <DialogFooter className="flex-wrap">
              <Button variant="outline" className="mobile:hidden" disabled={phase === 'submitting'} onClick={closeNewDownload}>
                {t('cancel')}
              </Button>
              {target ? (
                <Button variant="primary" disabled={!canSubmit} loading={phase === 'submitting'} onClick={() => void submit(false)}>
                  {startLabel}
                </Button>
              ) : null}
              {target ? null : <SplitButton
                label={t('downloadLater')}
                tooltip={laterQueue ? t('laterIntoQueueTooltip', { name: queueLabel(t, laterQueue) }) : undefined}
                disabled={!canSubmit}
                menu={queueMenu(true)}
                menuTitle={t('downloadLater')}
                menuLabel={t('downloadLater')}
                onClick={() => void submit(true)}
              />}
              {target ? null : <SplitButton
                variant="primary"
                label={startLabel}
                tooltip={startQueue ? t('startIntoQueueTooltip', { name: queueLabel(t, startQueue) }) : undefined}
                disabled={!canSubmit}
                loading={phase === 'submitting'}
                menu={queueMenu(false)}
                menuTitle={t('startDownload')}
                menuLabel={t('startDownload')}
                onClick={() => void submit(false)}
              />}
            </DialogFooter>
          )
        }
      >
        <Form onSubmit={() => void submit(false)} className="pb-2">
          {/* 链接 */}
          <div className="flex w-full flex-col gap-1.5">
            <div className="flex items-center justify-between gap-2">
              <FieldLabel htmlFor="new-download-urls">{t('downloadUrl')}</FieldLabel>
              {count > 0 ? <FieldHint>{t('urlCount', { count })}</FieldHint> : null}
            </div>
            <Textarea
              id="new-download-urls"
              maxAutoRows={URLS_MAX_ROWS}
              value={text}
              spellCheck={false}
              autoFocus
              placeholder={t('newDownloadUrlPlaceholder')}
              onChange={(event) => setText(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
                  event.preventDefault()
                  void submit(false)
                }
              }}
              className="font-mono text-xs"
            />
            {text.trim() !== '' && count === 0 ? <FieldError>{t('newDownloadNoValidUrl')}</FieldError> : null}
            {captures.length > 0 ? <FieldHint>{t('newDownloadCaptureContextHint')}</FieldHint> : null}
            {notice ? <FieldError className="break-all">{notice}</FieldError> : null}
            {failures.map((failure, index) => (
              <FieldError key={`${index}:${failure.url}`} className="break-all">
                {failure.url} — {failure.message}
              </FieldError>
            ))}
            {failureOverflow > 0 ? <FieldHint>… +{failureOverflow.toLocaleString()}</FieldHint> : null}
            <div className="flex flex-wrap gap-2">
              <Button variant="ghost" icon={FolderOpen} disabled={busy || target !== null} onClick={() => torrentInput.current?.click()}>
                {t('openTorrentFile')}
              </Button>
              <Button
                variant="ghost"
                icon={Sprout}
                disabled={busy || target !== null}
                onClick={() => {
                  closeNewDownload()
                  openSeedLocal()
                }}
              >
                {t('seedLocalTitle')}
              </Button>
              <Button variant="ghost" icon={FileText} disabled={busy} onClick={() => txtInput.current?.click()}>
                {t('importTxtFile')}
              </Button>
              <input ref={torrentInput} type="file" accept=".torrent,application/x-bittorrent" multiple hidden onChange={(event) => void pickTorrents(event)} />
              <input ref={txtInput} type="file" accept=".txt,.text,text/plain" multiple hidden onChange={(event) => void importTxt(event)} />
            </div>
          </div>

          {/* 下载到：本服务器 / 云账号其他设备 / 已配对设备 */}
          {remoteTargets.length > 0 ? (
            <FormField
              label={t('downloadTo')}
              htmlFor="new-download-target"
              hint={target
                ? [target.online === null ? t('devicePresenceUnknown') : target.online ? '' : t('downloadToOfflineHint'), t('downloadToRemoteOptionsIgnored')].filter(Boolean).join(' ')
                : t('downloadToHint')}
            >
              <Select
                id="new-download-target"
                value={target ? target.value : LOCAL_TARGET}
                options={[
                  { value: LOCAL_TARGET, label: t('webDownloadToServer') },
                  ...remoteTargets.map((item) => ({
                    value: item.value,
                    label: `${item.name} · ${item.kind === 'link' ? `${t('deviceLocalTag')} · ` : ''}${t(item.online === null ? 'devicePresenceUnknown' : item.online ? 'deviceOnline' : 'deviceOffline')}`,
                  })),
                ]}
                aria-label={t('downloadTo')}
                onValueChange={setTargetValue}
              />
            </FormField>
          ) : null}

          {/* 保存目录：本服务器用服务端目录选择器；远端为目标设备路径（空 = 目标默认目录） */}
          {target ? (
            <FormField
              label={t('saveDir')}
              htmlFor="new-download-save-dir"
              hint={t('downloadToRemoteDirHint')}
              {...(remoteDirCheck === 'notAbsolute' ? { error: t('downloadToPathInvalid', { example: pathExample(target.pathStyle) }) } : {})}
            >
              <InputWithAction
                input={
                  <Input
                    id="new-download-save-dir"
                    value={remoteSaveDir}
                    placeholder={target.defaultSaveDir ? t('downloadToRemoteDirDefault', { dir: target.defaultSaveDir }) : t('downloadToRemoteDirUseDefault')}
                    spellCheck={false}
                    invalid={remoteDirCheck !== 'ok'}
                    onChange={(event) => setRemoteSaveDir(event.target.value)}
                  />
                }
                action={
                  <Button icon={FolderOpen} disabled title={t('downloadToRemoteBrowseDisabled')}>
                    {t('browse')}
                  </Button>
                }
              />
            </FormField>
          ) : (
            <FormField label={t('saveDir')} htmlFor="new-download-save-dir">
              <InputWithAction
                input={
                  <div className="flex flex-wrap items-center gap-2">
                    {categoryOptions.length > 0 ? <CategoryPicker mode={categoryMode} options={categoryOptions} predicted={predictedDir} onChange={pickCategory} /> : null}
                    <div className="min-w-[14rem] flex-1">
                      <SaveDirCombo
                        id="new-download-save-dir"
                        value={saveDir}
                        placeholder={t('selectSaveDir')}
                        choices={saveDirOptions}
                        label={t('saveDirRecent')}
                        onChange={chooseSaveDir}
                      />
                    </div>
                  </div>
                }
                action={
                  <Button icon={FolderOpen} onClick={() => setPickerOpen(true)}>
                    {t('browse')}
                  </Button>
                }
              />
            </FormField>
          )}

          {/* 文件名：独占一行（与保存目录同宽），批量隐藏 */}
          {!batch ? (
            <FormField label={t('renameOptional')} htmlFor="new-download-rename">
              <Input id="new-download-rename" value={rename} placeholder={t('autoDetectFilename')} spellCheck={false} onChange={(event) => setRename(event.target.value)} />
            </FormField>
          ) : null}

          {/* 线程数：独占一行，全磁力隐藏；过高时下方风险提示 */}
          {!allMagnet ? (
            <div className="flex flex-col">
              <FormField label={t('threads')}>
                <div className="flex items-center gap-2">
                  <div className={threads === 'custom' ? 'w-26 shrink-0' : 'min-w-0 flex-1'}>
                    <Select value={threads} options={threadOptions} aria-label={t('threads')} onValueChange={changeThreads} />
                  </div>
                  {threads === 'custom' ? (
                    <Input
                      className="min-w-0 flex-1"
                      inputMode="numeric"
                      value={customThreads}
                      placeholder={t('customRangeHint', { min: 1, max: MAX_THREADS })}
                      aria-label={t('customThreads')}
                      onChange={(event) => setCustomThreads(event.target.value.replace(/[^0-9]/g, ''))}
                    />
                  ) : null}
                </div>
              </FormField>
              <ThreadsRiskCallout segments={segments} />
            </div>
          ) : null}

          {/* 高级 */}
          <div className="flex w-full flex-col gap-4">
            <button
              type="button"
              className="flex min-h-control w-full items-center gap-1 text-left coarse:min-h-touch"
              aria-expanded={advancedOpen}
              onClick={() => setAdvancedOpen((open) => !open)}
            >
              <Icon icon={advancedOpen ? ChevronDown : ChevronRight} size="md" className="text-text-tertiary" />
              <span className="text-sm font-medium">{t('taskProxyAdvanced')}</span>
              <span className="ml-2 h-px flex-1 bg-hairline" />
            </button>
            {advancedOpen ? (
              <>
                {showAuth ? (
                  <FormField
                    label={t('taskHttpAuth')}
                    hint={usesBrowserAuth ? t('newDownloadCaptureAuthHint') : t('taskHttpAuthDesc')}
                  >
                    <FormRow>
                      <Input
                        value={httpUser}
                        placeholder={t('taskHttpAuthUser')}
                        autoComplete="off"
                        spellCheck={false}
                        aria-label={t('taskHttpAuthUser')}
                        onChange={(event) => {
                          authDirty.current = true
                          authFilled.current = false
                          setHttpUser(event.target.value)
                        }}
                      />
                      <Input
                        type="password"
                        value={httpPassword}
                        placeholder={t('taskHttpAuthPassword')}
                        autoComplete="new-password"
                        aria-label={t('taskHttpAuthPassword')}
                        onChange={(event) => {
                          authDirty.current = true
                          authFilled.current = false
                          setHttpPassword(event.target.value)
                        }}
                      />
                    </FormRow>
                  </FormField>
                ) : null}

                <FormField label={t('taskProxy')} hint={t('taskProxyDesc')}>
                  <div className="flex flex-col gap-2">
                    <Select value={proxyChoice} options={proxyOptions} aria-label={t('taskProxy')} onValueChange={setProxyChoice} />
                    {proxyChoice === 'custom' ? (
                      <Input value={customProxy} placeholder={t('taskProxyPlaceholder')} spellCheck={false} aria-label={t('taskProxy')} onChange={(event) => setCustomProxy(event.target.value)} />
                    ) : null}
                  </div>
                </FormField>

                <FormField label={t('userAgent')} hint={t('userAgentTaskPlaceholder')}>
                  <div className="flex flex-col gap-2 min-[560px]:flex-row">
                    <div className="min-[560px]:w-[140px] min-[560px]:shrink-0">
                      <Select value={uaPreset} options={UA_PRESET_KEYS.map((key) => ({ value: key, label: uaLabel(key) }))} aria-label={t('userAgent')} onValueChange={changeUaPreset} />
                    </div>
                    <Input value={userAgent} placeholder={t('userAgentTaskPlaceholder')} spellCheck={false} aria-label={t('userAgent')} onChange={(event) => editUserAgent(event.target.value)} />
                  </div>
                </FormField>

                <FormField label={t('taskCookie')} htmlFor="new-download-cookie" hint={t('taskCookieDesc')}>
                  <Textarea id="new-download-cookie" rows={3} value={cookie} spellCheck={false} placeholder={t('taskCookiePlaceholder')} onChange={(event) => setCookie(event.target.value)} />
                </FormField>

                <FormField label={t('taskChecksum')} hint={t('taskChecksumDesc')}>
                  <div className="flex flex-col gap-2 min-[560px]:flex-row">
                    <div className="min-[560px]:w-[140px] min-[560px]:shrink-0">
                      <Select value={hashAlgorithm} options={HASH_ALGORITHMS.map((algo) => ({ value: algo, label: algo }))} aria-label={t('taskChecksum')} onValueChange={setHashAlgorithm} />
                    </div>
                    <Input value={checksum} placeholder={t('taskChecksumPlaceholder')} spellCheck={false} aria-label={t('taskChecksum')} onChange={(event) => setChecksum(event.target.value)} />
                  </div>
                </FormField>

                <FormField label={t('taskHeaders')} hint={t('taskHeadersDesc')}>
                  <div className="flex flex-col gap-2">
                    {headers.map((row) => (
                      <div key={row.id} className="flex items-center gap-2">
                        <Input
                          className="w-2/5 min-w-0 shrink-0 min-[560px]:w-[168px]"
                          value={row.key}
                          placeholder={t('taskHeadersKeyPlaceholder')}
                          spellCheck={false}
                          aria-label={t('taskHeadersKeyPlaceholder')}
                          onChange={(event) => setHeaders((rows) => rows.map((item) => (item.id === row.id ? { ...item, key: event.target.value } : item)))}
                        />
                        <Input
                          value={row.value}
                          placeholder={t('taskHeadersValuePlaceholder')}
                          spellCheck={false}
                          aria-label={t('taskHeadersValuePlaceholder')}
                          onChange={(event) => setHeaders((rows) => rows.map((item) => (item.id === row.id ? { ...item, value: event.target.value } : item)))}
                        />
                        <Button variant="ghost" iconOnly title={t('delete')} aria-label={t('delete')} onClick={() => setHeaders((rows) => rows.filter((item) => item.id !== row.id))}>
                          <Icon icon={X} />
                        </Button>
                      </div>
                    ))}
                    <div>
                      <Button
                        icon={Plus}
                        onClick={() => {
                          headerSeq.current += 1
                          setHeaders((rows) => [...rows, { id: headerSeq.current, key: '', value: '' }])
                        }}
                      >
                        {t('taskHeadersAdd')}
                      </Button>
                    </div>
                  </div>
                </FormField>

                <OptionGroup>
                  {showAuth ? <OptionRow title={t('taskHttpAuthSaveForSite')} control={<Switch checked={saveSiteAuth} onCheckedChange={setSaveSiteAuth} aria-label={t('taskHttpAuthSaveForSite')} />} /> : null}
                  <OptionRow
                    title={t('taskIgnoreTlsErrors')}
                    description={t('taskIgnoreTlsErrorsDesc')}
                    control={<Switch checked={ignoreTls} onCheckedChange={setIgnoreTls} aria-label={t('taskIgnoreTlsErrors')} />}
                  />
                </OptionGroup>
              </>
            ) : null}
          </div>
          <button type="submit" className="hidden" tabIndex={-1} aria-hidden />
        </Form>
      </Dialog>

      {manifest ? (
        <ManifestSelectDialog
          preview={manifest.preview}
          sourceUrl={manifest.sourceUrl}
          base={manifest.base}
          queues={queues}
          onCancel={() => setManifest(null)}
          onCreated={() => {
            rememberSaveDir(manifest.base.saveDir)
            closeNewDownload()
          }}
        />
      ) : null}
      <FsPickerDialog open={pickerOpen} onOpenChange={setPickerOpen} initialPath={saveDir} onSelect={chooseSaveDir} />
    </>
  )
}
