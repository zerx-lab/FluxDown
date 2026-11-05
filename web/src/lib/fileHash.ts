// 文件哈希展示与校验和比对（与 GPUI `file_hash.rs` 同一规则）。

import { HASH_ALGORITHMS } from './rpc/protocol/config'

type HashAlgorithm = (typeof HASH_ALGORITHMS)[number]

const LABELS: Readonly<Record<string, string>> = {
  md5: 'MD5',
  sha1: 'SHA-1',
  sha256: 'SHA-256',
  sha512: 'SHA-512',
}

/** 设置页枚举行的显式文案（技术名，不走 i18n）。 */
export const HASH_ALGORITHM_LABELS = LABELS

const ALGORITHMS = new Set<string>(HASH_ALGORITHMS)

/** 已知算法给出规范名；其余把输入原样大写（不归一化）。 */
export function hashAlgorithmLabel(algo: string): string {
  return LABELS[algo] ?? algo.toUpperCase()
}

function normalizeAlgo(raw: string): HashAlgorithm | null {
  const algo = raw.trim().toLowerCase().replaceAll('-', '')
  return ALGORITHMS.has(algo) ? (algo as HashAlgorithm) : null
}

/** `algo=hex`。`stripPrefix` 只用于校验和：去掉与算法相同的 `<algo>:` 前缀。 */
function parseSpec(spec: string, stripPrefix: boolean): { algo: HashAlgorithm; hex: string } | null {
  const trimmed = spec.trim()
  if (trimmed === '') return null
  const eq = trimmed.indexOf('=')
  if (eq <= 0) return null
  const algo = normalizeAlgo(trimmed.slice(0, eq))
  if (algo === null) return null
  let hex = trimmed.slice(eq + 1).trim()
  if (stripPrefix) {
    const colon = hex.indexOf(':')
    if (colon > 0 && normalizeAlgo(hex.slice(0, colon)) === algo) hex = hex.slice(colon + 1).trim()
  }
  if (hex === '') return null
  return { algo, hex }
}

/**
 * 落盘哈希是否等于用户填写的校验和。
 * 任一侧为空 / 无法解析，或算法不同，返回 null。
 */
export function fileHashMatchesChecksum(fileHash: string, checksum: string): boolean | null {
  const left = parseSpec(fileHash, false)
  const right = parseSpec(checksum, true)
  if (left === null || right === null || left.algo !== right.algo) return null
  return left.hex.toLowerCase() === right.hex.toLowerCase()
}

/** 详情行展示：已知算法用规范名，未知算法把去空白后的前缀大写。无法拆分时返回 null。 */
export function fileHashDisplay(fileHash: string): { algo: HashAlgorithm | null; label: string; hex: string } | null {
  const eq = fileHash.indexOf('=')
  if (eq <= 0) return null
  const algo = fileHash.slice(0, eq).trim()
  const hex = fileHash.slice(eq + 1).trim()
  if (algo === '' || hex === '') return null
  const canonical = normalizeAlgo(algo)
  return { algo: canonical, label: hashAlgorithmLabel(canonical ?? algo), hex }
}
