// 「本机做种」表单 → `daemon.task.seedLocal` 参数：去空白、拆分 tracker、必填校验。纯函数，便于单测。

import type { DaemonSeedLocalParams, SeedTorrentVersion } from '../../../lib/rpc'

/** 生成种子的格式选项（顺序即下拉顺序）。 */
export const SEED_TORRENT_VERSIONS: readonly SeedTorrentVersion[] = ['v1', 'v2', 'hybrid']

/** 分片大小选项：0 = 自动，其余为 16 KiB … 16 MiB 的 2 的幂。 */
export const SEED_PIECE_LENGTHS: readonly number[] = [0, ...Array.from({ length: 11 }, (_, i) => 16_384 * 2 ** i)]

/** 分片大小选项的显示文字（0 由调用方换成「自动」）：KiB / MiB 整数。 */
export function pieceLengthLabel(bytes: number): string {
  return bytes >= 1_048_576 ? `${bytes / 1_048_576} MiB` : `${bytes / 1024} KiB`
}

export interface SeedLocalForm {
  path: string
  torrentPath: string
  /** 每行一个 announce 地址；空行分隔层级（BEP 12，同 qBittorrent）。 */
  trackers: string
  unlimited: boolean
  /** 以下为「种子选项」：只在现场生成种子（`torrentPath` 为空）时生效。 */
  version: SeedTorrentVersion
  /** 字节；0 = 自动。 */
  pieceLength: number
  private: boolean
  /** 每行一个 HTTP 源地址。 */
  webSeeds: string
  comment: string
  source: string
  /** 同时写出 .torrent 的路径（守护进程主机）。 */
  torrentOutput: string
  /** 关闭 = 只生成种子文件不建任务（`createOnly`），此时必须给 `torrentOutput`。 */
  startSeeding: boolean
  superSeeding: boolean
  /** 只在给了现成 .torrent 时生效：信任数据完整，跳过哈希校验。 */
  skipCheck: boolean
}

export const SEED_LOCAL_FORM_DEFAULTS: SeedLocalForm = {
  path: '',
  torrentPath: '',
  trackers: '',
  unlimited: false,
  version: 'v1',
  pieceLength: 0,
  private: false,
  webSeeds: '',
  comment: '',
  source: '',
  torrentOutput: '',
  startSeeding: true,
  superSeeding: false,
  skipCheck: false,
}

export type SeedLocalBuild = { ok: true; params: DaemonSeedLocalParams } | { ok: false; error: 'pathRequired' | 'outputRequired' }

function lines(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line !== '')
}

/** tracker 文本 → 参数：每行一个地址，空行分隔层级（上线为空串）；首尾与连续空行只算一个分隔。 */
export function trackerTierLines(text: string): string[] {
  const out: string[] = []
  for (const line of text.split(/\r?\n/).map((line) => line.trim())) {
    if (line !== '') out.push(line)
    else if (out.length > 0 && out[out.length - 1] !== '') out.push('')
  }
  if (out[out.length - 1] === '') out.pop()
  return out
}

/**
 * 指定了现成 .torrent 时忽略 trackers 与种子选项（种子自带），只带 `superSeeding` / `skipCheck`；
 * 现场生成时带上非默认的种子选项（默认值不上线，旧 daemon 收到的仍是旧形状）。
 * 空路径 → `pathRequired`；不立即做种却没给种子文件路径 → `outputRequired`。
 */
export function buildSeedLocalParams(form: SeedLocalForm): SeedLocalBuild {
  const path = form.path.trim()
  if (path === '') return { ok: false, error: 'pathRequired' }
  const torrentPath = form.torrentPath.trim()
  const params: DaemonSeedLocalParams = { path, unlimited: form.unlimited }
  if (torrentPath !== '') {
    params.torrentPath = torrentPath
    if (form.superSeeding) params.superSeeding = true
    if (form.skipCheck) params.skipCheck = true
    return { ok: true, params }
  }
  const torrentOutput = form.torrentOutput.trim()
  if (!form.startSeeding && torrentOutput === '') return { ok: false, error: 'outputRequired' }
  const trackers = trackerTierLines(form.trackers)
  if (trackers.length > 0) params.trackers = trackers
  if (form.version !== 'v1') params.version = form.version
  if (form.private) params.private = true
  if (form.pieceLength > 0) params.pieceLength = form.pieceLength
  const webSeeds = lines(form.webSeeds)
  if (webSeeds.length > 0) params.webSeeds = webSeeds
  const comment = form.comment.trim()
  if (comment !== '') params.comment = comment
  const source = form.source.trim()
  if (source !== '') params.source = source
  if (form.superSeeding && form.startSeeding) params.superSeeding = true
  if (torrentOutput !== '') params.torrentOutput = torrentOutput
  if (!form.startSeeding) params.createOnly = true
  return { ok: true, params }
}

export type SeedLocalFeedback =
  | { key: 'seedLocalStarted'; level: 'success' }
  | { key: 'btCreateSaved'; level: 'success'; params: { path: string } }
  | { key: 'duplicateTorrentToast'; level: 'warning'; params: { name: string } }
  | { key: 'duplicateTorrentToastUnnamed'; level: 'warning' }

/**
 * 结果 → 提示：只生成种子文件（`createOnly`）给「已保存到」（路径取结果，旧 daemon 缺省时回退到请求值）；
 * 重复（未新建任务）给警告，否则成功。
 */
export function seedLocalFeedback(
  result: { duplicate?: boolean; existingName?: string; torrentPath?: string },
  request: Pick<DaemonSeedLocalParams, 'createOnly' | 'torrentOutput'> = {},
): SeedLocalFeedback {
  if (request.createOnly) {
    const saved = result.torrentPath ?? ''
    return { key: 'btCreateSaved', level: 'success', params: { path: saved !== '' ? saved : (request.torrentOutput ?? '') } }
  }
  if (!result.duplicate) return { key: 'seedLocalStarted', level: 'success' }
  const name = result.existingName ?? ''
  if (name === '') return { key: 'duplicateTorrentToastUnnamed', level: 'warning' }
  return { key: 'duplicateTorrentToast', level: 'warning', params: { name } }
}
