/**
 * 单页 DOM 嗅探上报上限（settings.sniffMaxResources）。独立成无依赖模块：内容脚本与 background 共用，
 * 不把 settings.ts 的存储依赖带进每帧注入的 sensor。
 */

/** 默认值（#288：长列表页无节制扫描 / 上报会让页面卡顿）。 */
export const SNIFF_MAX_RESOURCES_DEFAULT = 200;
/** 允许设置的最大值。 */
export const SNIFF_MAX_RESOURCES_LIMIT = 99999;

/** 把任意值规整为合法上限：非有限数字回退默认值，取整后夹到 1…LIMIT。 */
export function normalizeSniffMaxResources(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return SNIFF_MAX_RESOURCES_DEFAULT;
  return Math.min(SNIFF_MAX_RESOURCES_LIMIT, Math.max(1, Math.round(value)));
}
