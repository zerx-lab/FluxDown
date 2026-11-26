// 新建下载「下载到」目标：本服务器 / 云账号其他设备 / 已配对局域网设备的纯规则（校验、下发参数、汇总）。

import { isAbsolutePath } from '../../../lib/rpc'
import type {
  CloudDevice,
  LinkDeviceInfo,
  LinkDispatchParams,
  PathStyle,
  RemoteDispatchParams,
} from '../../../lib/rpc'
import { mergeDevices } from '../model/devices'

export const LOCAL_TARGET = 'local'

/**
 * 下拉里的一个远端目标（合并后的设备条目）。`value`：云条目（含挂接直连）= `cloud:<deviceId>`，
 * 独立直连 = `link:<fingerprint>`。
 */
export interface RemoteTarget {
  value: string
  /** `cloud` = 云条目（可能挂接直连）；`link` = 仅直连。 */
  kind: 'cloud' | 'link'
  /** cloud = deviceId；link = 指纹。 */
  id: string
  name: string
  online: boolean | null
  /** 目标自报的默认下载目录；未上报为 null。 */
  defaultSaveDir: string | null
  /** 目标路径风格；无法判定为 null（此时只能交给 agent 校验）。 */
  pathStyle: PathStyle | null
  /** 挂接 / 独立的直连指纹；无直连为 null。 */
  linkFingerprint: string | null
  linkOnline: boolean | null
}

export function buildRemoteTargets(
  cloudDevices: readonly CloudDevice[],
  linkedDevices: readonly LinkDeviceInfo[],
  presence: { cloud: boolean; local: boolean },
): RemoteTarget[] {
  return mergeDevices(cloudDevices, linkedDevices, presence).map((entry) => ({
    ...entry,
    value: `${entry.kind}:${entry.id}`,
  }))
}

/** 持久化 / 外部传入的目标值解析：`link:<fp>` 已挂接到云条目时解析为该云条目。 */
export function resolveTargetValue(value: string, targets: readonly RemoteTarget[]): RemoteTarget | null {
  const exact = targets.find((item) => item.value === value)
  if (exact) return exact
  if (!value.startsWith('link:')) return null
  const fingerprint = value.slice('link:'.length)
  return targets.find((item) => item.kind === 'cloud' && item.linkFingerprint === fingerprint) ?? null
}

/** 下发路由：独立直连或挂接且直连在线 → 直连一次性投递；否则走云。 */
export function dispatchRoute(target: RemoteTarget): { via: 'link'; fingerprint: string } | { via: 'cloud' } {
  if (target.linkFingerprint !== null && (target.kind === 'link' || target.linkOnline === true)) {
    return { via: 'link', fingerprint: target.linkFingerprint }
  }
  return { via: 'cloud' }
}

export type SaveDirCheck = 'ok' | 'notAbsolute'

/** 空 = 使用目标默认目录；风格未知无法本地判定，放行交给 agent；否则必须是目标风格的绝对路径。 */
export function checkRemoteSaveDir(saveDir: string, style: PathStyle | null): SaveDirCheck {
  const dir = saveDir.trim()
  if (dir === '' || style === null || style === 'unknown') return 'ok'
  return isAbsolutePath(style, dir) ? 'ok' : 'notAbsolute'
}

/** 校验失败提示里的路径示例。 */
export function pathExample(style: PathStyle | null): string {
  return style === 'windows' ? 'D:\\Downloads' : '/home/user/Downloads'
}

interface DispatchInput {
  url: string
  fileName?: string
}

function optionalFields(input: DispatchInput, saveDir: string): { fileName?: string; saveDir?: string } {
  const fields: { fileName?: string; saveDir?: string } = {}
  const name = input.fileName?.trim()
  if (name) fields.fileName = name
  const dir = saveDir.trim()
  if (dir !== '') fields.saveDir = dir
  return fields
}

/** 目录为空则不带 `saveDir`（= 目标默认目录）。 */
export function remoteDispatchParams(toDevice: string, input: DispatchInput, saveDir: string): RemoteDispatchParams {
  return { toDevice, url: input.url, ...optionalFields(input, saveDir) }
}

export function linkDispatchParams(fingerprint: string, input: DispatchInput, saveDir: string): LinkDispatchParams {
  return { fingerprint, url: input.url, ...optionalFields(input, saveDir) }
}

/** Each dispatch uses the same serial gateway lane. Await its reply before sending the next. */
export async function dispatchSequentially<T>(
  items: readonly T[],
  send: (item: T) => Promise<unknown>,
  signal: AbortSignal,
  onProgress: (done: number, failed: number) => void,
) {
  const failed: { item: T; reason: unknown }[] = []
  let done = 0
  for (const item of items) {
    if (signal.aborted) break
    try {
      await send(item)
    } catch (reason) {
      failed.push({ item, reason })
    }
    onProgress(++done, failed.length)
  }
  return { ok: done - failed.length, failed, unsent: items.slice(done) }
}
