export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 只保留已知字段（缺失的字段不补）；不是对象时返回 null。 */
export function pick(src: unknown, keys: readonly string[]): Record<string, unknown> | null {
  if (!isRecord(src)) return null;
  const out: Record<string, unknown> = {};
  for (const key of keys) if (key in src) out[key] = src[key];
  return out;
}
