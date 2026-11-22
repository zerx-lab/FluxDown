// 速率限制单位换算（镜像 GPUI rate_limit.rs）：落库为字节/秒，1024 进制，0 = 不限。

export type RateUnit = 'kb' | 'mb' | 'gb'

export const RATE_UNITS: readonly RateUnit[] = ['kb', 'mb', 'gb']

export const RATE_UNIT_FACTOR: Record<RateUnit, number> = { kb: 1024, mb: 1024 ** 2, gb: 1024 ** 3 }

export const RATE_UNIT_STEP: Record<RateUnit, number> = { kb: 64, mb: 1, gb: 1 }

const round2 = (value: number): number => Math.round(value * 100) / 100

/** 字节/秒 → 该单位下的显示值（两位小数）。 */
export function toDisplay(bytes: number, unit: RateUnit): number {
  return round2(bytes / RATE_UNIT_FACTOR[unit])
}

/** 显示值 → 字节/秒：先截到显示精度；负数归零。 */
export function toBytes(value: number, unit: RateUnit): number {
  return Math.max(0, Math.round(round2(value) * RATE_UNIT_FACTOR[unit]))
}

/** 该单位的两位小数显示能否无损还原为 `bytes`。 */
function exact(bytes: number, unit: RateUnit): boolean {
  return toBytes(toDisplay(bytes, unit), unit) === bytes
}

/** 能精确表示 `bytes` 的最大单位；0（不限）默认 MB/s。 */
export function bestUnit(bytes: number): RateUnit {
  if (bytes <= 0) return 'mb'
  for (const unit of [...RATE_UNITS].reverse()) {
    if (bytes >= RATE_UNIT_FACTOR[unit] && exact(bytes, unit)) return unit
  }
  return 'kb'
}

/** 用户选的单位能精确表示当前值时沿用，否则回退 `bestUnit`。 */
export function effectiveUnit(bytes: number, chosen: RateUnit | null): RateUnit {
  return chosen !== null && (bytes <= 0 || exact(bytes, chosen)) ? chosen : bestUnit(bytes)
}

/** 切换单位的新字节值：保留当前显示的数字，换算到新单位。 */
export function switchTarget(bytes: number, from: RateUnit, to: RateUnit): number {
  return toBytes(toDisplay(bytes, from), to)
}
