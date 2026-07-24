// FluxCloud CDN 聚合配置云拉取 + 众包遥测上报 —— Web 面板侧实现，逐条对齐桌面端
// lib/src/services/cloud/cdn_config_service.dart / cdn_report_service.dart 的语义：
//
// 配置拉取（P1 §四契约）：登录即拉一次 + 12h 周期；GET /cdn/config 走 If-None-Match
// 条件请求（ETag 按用户 id 隔离持久化，换号不误用另一账号缓存）；拉到后经宿主
// PUT /api/v1/config 落引擎 config 表（服务端 ActorCmd::ApplyConfig live-apply，
// 键集合与桌面 SaveConfig 完全相同）；失败静默保留旧值——引擎有内置 resolver
// baseline，云端不可达不影响下载功能，哪次网通了周期/事件触发即刻生效。
// 另监听 FluxCloud `GET /sync/events` SSE 的 `kind=cdn_config` 帧（管理端保存
// CDN 设置后 bump 的全局 revision），到帧立即重拉，不等 12h 周期。
//
// 遥测上报（P2 §五契约）：常开无用户开关（语义对齐匿名使用统计：仅域名/节点 IP/
// 连接耗时/吞吐，不含 URL、文件名等内容信息）。登录即上报一次 + 30min 周期 +
// 任务完成事件 10s 去抖；读取引擎 config 键 `cdn_pending_reports`（GET /api/v1/config
// 服务端已前置 telemetry flush，对齐 hub 的 RequestConfig 处理点），按 ≤64 条分批
// POST /cdn/report（批间 1.2s 让开服务端 per-user 1s 限频窗），全部批次成功后写
// 空串清空（服务端 apply_config_key 空值分支转调 clear_cdn_pending_reports），
// 任一批次失败整轮静默保留，下次周期/启动重试，绝不重复上报。
// 未登录/登出：停止定时器，不清空引擎侧缓冲（登出不代表放弃已采集样本）。

import { api } from '../api'
import { isAuthenticated } from '../auth'
import { taskCompletionListeners } from '../ws'
import { cloudApi, getCloudBaseUrl } from './client'
import { cloudDeviceId, cloudSessionStore, getCloudAccessToken } from './session'
import type { CdnConfig } from './types'

/** ETag 持久化键前缀，按登录用户 id 隔离（同桌面端 `cdn_config_etag.$uid` 惯例）。 */
const ETAG_KEY_PREFIX = 'fluxdown.cloud.cdnEtag'
/** 配置拉取周期（P1 契约 12h）。 */
const CONFIG_PERIOD_MS = 12 * 3600_000
/** 遥测上报周期（P2 契约 30min）。 */
const REPORT_PERIOD_MS = 30 * 60_000
/** 单批上传上限（服务端契约 ≤64 条/次）。 */
const BATCH_SIZE = 64
/** 批间间隔：服务端 per-user 1s 限频窗 + 0.2s 时钟偏差余量。 */
const INTER_BATCH_GAP_MS = 1200
/** 任务完成事件上报去抖：合并批量下载 + 给引擎侧段完成样本收尾留余量。 */
const COMPLETION_DEBOUNCE_MS = 10_000
/** SSE 出错后的重建延迟（EventSource 原生重连不换 token，这里整体重建）。 */
const SSE_RETRY_MS = 15_000

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms))

// ---------------------------------------------------------------------------
// 配置拉取
// ---------------------------------------------------------------------------

let configTimer: ReturnType<typeof setInterval> | null = null
let configInflight: Promise<void> | null = null

function etagKey(): string | null {
  const uid = cloudSessionStore.get().user?.id
  return uid ? `${ETAG_KEY_PREFIX}.${uid}` : null
}

async function fetchCdnConfig(): Promise<void> {
  if (cloudSessionStore.get().status !== 'authenticated' || !isAuthenticated()) return
  const key = etagKey()
  const etag = key ? localStorage.getItem(key) : null
  try {
    const result = await cloudApi.cdnConfig(etag)
    if (result.notModified || !result.config) return
    if (key && result.etag) localStorage.setItem(key, result.etag)
    await applyCdnConfig(result.config)
  } catch {
    // 失败静默：保留引擎已落库的旧值，不弹 UI 提示（断网/云端不可达是常态）。
  }
}

/** 写入宿主引擎 config 表（PUT /api/v1/config live-apply），键集合与桌面
 *  SaveConfig 完全相同；enabled=false 回退内置 baseline / 全部禁用。 */
function applyCdnConfig(config: CdnConfig): Promise<unknown> {
  if (config.enabled) {
    return api.putConfig({
      // 对象形式 `[{"url":...,"ecs":bool}]`：保留云端下发的 ECS 标志（引擎兼容旧纯字符串数组）。
      cdn_resolver_endpoints: JSON.stringify(config.resolvers.map((r) => ({ url: r.url, ecs: r.ecs }))),
      cdn_cloud_max_nodes: String(Math.min(8, Math.max(0, config.max_nodes))),
      cdn_ecs_subnets: JSON.stringify(config.ecs_subnets.map((s) => s.subnet)),
      // hints 与聚合下载同源，走当前生效的云服务地址（引擎侧仅接受 https，开发期 http 自然禁用）。
      cdn_hints_base: getCloudBaseUrl(),
    })
  }
  return api.putConfig({
    cdn_resolver_endpoints: '[]',
    cdn_cloud_max_nodes: '0',
    cdn_ecs_subnets: '[]',
    cdn_hints_base: '',
  })
}

function scheduleConfigFetch() {
  configInflight ??= fetchCdnConfig().finally(() => {
    configInflight = null
  })
}

// ---------------------------------------------------------------------------
// SSE：kind=cdn_config 全局 revision 变更 → 立即重拉
// ---------------------------------------------------------------------------

let sse: EventSource | null = null
let sseRetryTimer: ReturnType<typeof setTimeout> | null = null

function openSse() {
  closeSse()
  const token = getCloudAccessToken()
  if (!token) return
  const url = `${getCloudBaseUrl()}/api/v1/sync/events?access_token=${encodeURIComponent(token)}&deviceId=${encodeURIComponent(cloudDeviceId())}`
  const es = new EventSource(url)
  sse = es
  es.onmessage = (ev) => {
    try {
      const json = JSON.parse(ev.data) as { kind?: string }
      if (json.kind === 'cdn_config') scheduleConfigFetch()
      // 无 kind 帧是本账号 sync revision（配置同步水位线），面板暂不消费。
    } catch {
      /* 心跳/非 JSON 帧忽略 */
    }
  }
  es.onerror = () => {
    // token 过期/网络断开：整体重建（EventSource 原生重连沿用旧 URL 里的旧 token）。
    if (sse !== es) return
    es.close()
    sse = null
    sseRetryTimer = setTimeout(() => {
      if (cloudSessionStore.get().status === 'authenticated') openSse()
    }, SSE_RETRY_MS)
  }
}

function closeSse() {
  if (sseRetryTimer) {
    clearTimeout(sseRetryTimer)
    sseRetryTimer = null
  }
  sse?.close()
  sse = null
}

// ---------------------------------------------------------------------------
// 遥测上报
// ---------------------------------------------------------------------------

let reportTimer: ReturnType<typeof setInterval> | null = null
let reportInflight: Promise<void> | null = null
let completionDebounce: ReturnType<typeof setTimeout> | null = null

async function drainReports(): Promise<void> {
  if (cloudSessionStore.get().status !== 'authenticated' || !isAuthenticated()) return
  let raw: string | undefined
  try {
    raw = (await api.getConfig())['cdn_pending_reports']
  } catch {
    return // 宿主不可达：下次周期重试
  }
  if (!raw) return
  let samples: unknown[]
  try {
    const decoded = JSON.parse(raw) as unknown
    if (!Array.isArray(decoded)) throw new Error('not a JSON array')
    samples = decoded
  } catch {
    return // 形状异常：保留原值不动，交由引擎侧自愈
  }
  if (samples.length === 0) return
  try {
    for (let i = 0; i < samples.length; i += BATCH_SIZE) {
      if (i > 0) await sleep(INTER_BATCH_GAP_MS)
      await cloudApi.cdnReport(samples.slice(i, i + BATCH_SIZE))
    }
    // 全部批次成功：写空串清空（服务端 apply_config_key 空值分支转调 clear）。
    await api.putConfig({ cdn_pending_reports: '' })
  } catch {
    // 失败静默：整轮保留待下次周期/启动重试。
  }
}

function scheduleDrain() {
  reportInflight ??= drainReports().finally(() => {
    reportInflight = null
  })
}

function onTaskCompleted() {
  if (!reportTimer) return // 未登录：样本留在引擎缓冲，登录后 start 时上报
  if (completionDebounce) clearTimeout(completionDebounce)
  completionDebounce = setTimeout(scheduleDrain, COMPLETION_DEBOUNCE_MS)
}

// ---------------------------------------------------------------------------
// 生命周期接线
// ---------------------------------------------------------------------------

let attached = false

function start() {
  if (!configTimer) {
    scheduleConfigFetch()
    configTimer = setInterval(scheduleConfigFetch, CONFIG_PERIOD_MS)
  }
  if (!reportTimer) {
    scheduleDrain()
    reportTimer = setInterval(scheduleDrain, REPORT_PERIOD_MS)
  }
  if (!sse && !sseRetryTimer) openSse()
}

function stop() {
  if (configTimer) {
    clearInterval(configTimer)
    configTimer = null
  }
  if (reportTimer) {
    clearInterval(reportTimer)
    reportTimer = null
  }
  if (completionDebounce) {
    clearTimeout(completionDebounce)
    completionDebounce = null
  }
  closeSse()
}

/** 应用入口调用一次：挂登录态监听 + 任务完成钩子，已登录则立即启动。
 *  未登录时全部静默待命，登录瞬间自动开始拉取/上报。 */
export function attachCdnServices() {
  if (attached) return
  attached = true
  taskCompletionListeners.add(onTaskCompleted)
  cloudSessionStore.subscribe(() => {
    if (cloudSessionStore.get().status === 'authenticated') start()
    else stop()
  })
  if (cloudSessionStore.get().status === 'authenticated') start()
}
