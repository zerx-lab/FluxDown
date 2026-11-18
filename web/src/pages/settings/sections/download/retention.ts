// 已完成记录保留天数的下拉模式逻辑（镜像 crates/settings/src/sections/retention.rs）。
// 两端共用用例 crates/settings/tests/fixtures/retention_cases.json（retention.test.ts）。

/** 预设（-1 = 立即，0 = 从不），与下拉项一一对应。 */
export const RETENTION_PRESETS: readonly number[] = [-1, 0, 1, 7, 30]
/** 切到「自定义」且没有可复用的自定义值时的起始天数（刻意不等于任何预设）。 */
export const RETENTION_CUSTOM_START = 14
export const RETENTION_CUSTOM = 'custom'

/** 下拉当前项：预设天数的字符串，或 {@link RETENTION_CUSTOM}。 */
export function retentionModeId(days: number, customFlag: boolean): string {
  if ((customFlag && days > 0) || !RETENTION_PRESETS.includes(days)) return RETENTION_CUSTOM
  return String(days)
}

/** 切到「自定义」时写入的天数：当前值已是非预设值则沿用，否则用起始值。 */
export function retentionCustomTarget(current: number, max: number): number {
  const base = current > 0 && !RETENTION_PRESETS.includes(current) ? current : RETENTION_CUSTOM_START
  return Math.min(max, Math.max(1, base))
}
