/**
 * BT 防吸血自查 / 申诉页的纯逻辑：上游响应解析、申诉请求校验、错误码 → 文案键、到期格式化。
 * 与 FluxCloud 的往返在 `bt-guard-proxy.ts`。
 *
 * 上游契约（FluxCloud，公开、无鉴权、只针对请求来源 IP 所在组、按访客 IP 限频）：
 * - `GET  /api/v1/bt-guard/public/self` → `{ listed, ipGroup?, entries[{ reason, category, action, expiresAt(unix 秒) }],
 *    turnstileSiteKey?, appealPending? }`
 * - `POST /api/v1/bt-guard/public/appeal { detail, contact?, turnstileToken? }` → 204；
 *   失败 `{ code, message }`：429 `rate_limited` / 403|400 `turnstile_required|turnstile_failed` / 409 `appeal_pending` / 404 `not_listed`。
 */
import { isRecord } from "./is-record";

/** Cloudflare Turnstile token 上限 2048 字符。 */
const MAX_TURNSTILE_TOKEN = 2048;

export const BT_GUARD_ENTRY_ACTIONS = ["ban", "choke", "throttle"] as const;
export type BtGuardEntryAction = (typeof BT_GUARD_ENTRY_ACTIONS)[number];

export const BT_GUARD_CATEGORIES = ["community", "consensus"] as const;
export type BtGuardCategory = (typeof BT_GUARD_CATEGORIES)[number];

export interface BtGuardSelfEntry {
  reason: string;
  category: BtGuardCategory;
  action: BtGuardEntryAction;
  /** unix 秒。 */
  expiresAt: number;
}

export interface BtGuardSelf {
  listed: boolean;
  ipGroup?: string;
  entries: BtGuardSelfEntry[];
  turnstileSiteKey?: string;
  appealPending: boolean;
}

/** 单次最多展示的条目数与理由长度（防御畸形上游）。 */
export const MAX_SELF_ENTRIES = 20;
export const MAX_SELF_REASON = 300;

export const MAX_APPEAL_DETAIL = 2000;
export const MAX_APPEAL_CONTACT = 200;
/** 申诉请求体上限：detail ≤ 2000 + contact ≤ 200 + token ≤ 2048 加 JSON 转义余量。 */
export const MAX_APPEAL_BODY = 16384;

function isOneOf<T extends string>(list: readonly T[], value: unknown): value is T {
  return typeof value === "string" && (list as readonly string[]).includes(value);
}

function parseEntry(value: unknown): BtGuardSelfEntry | null {
  if (!isRecord(value)) return null;
  const { reason, category, action, expiresAt } = value;
  if (typeof reason !== "string") return null;
  if (!isOneOf(BT_GUARD_CATEGORIES, category) || !isOneOf(BT_GUARD_ENTRY_ACTIONS, action)) return null;
  if (typeof expiresAt !== "number" || !Number.isInteger(expiresAt) || expiresAt < 0) return null;
  return { reason: reason.slice(0, MAX_SELF_REASON), category, action, expiresAt };
}

/** 校验并裁剪 `GET …/self` 的上游响应；形状不对返回 null（调用方按 502 处理）。 */
export function parseSelf(value: unknown): BtGuardSelf | null {
  if (!isRecord(value)) return null;
  const { listed, ipGroup, entries, turnstileSiteKey, appealPending } = value;
  if (typeof listed !== "boolean") return null;
  if (ipGroup !== undefined && ipGroup !== null && (typeof ipGroup !== "string" || ipGroup.length > 64)) return null;
  if (appealPending !== undefined && appealPending !== null && typeof appealPending !== "boolean") return null;
  if (turnstileSiteKey !== undefined && turnstileSiteKey !== null && typeof turnstileSiteKey !== "string") return null;

  const parsed: BtGuardSelfEntry[] = [];
  if (entries !== undefined && entries !== null) {
    if (!Array.isArray(entries)) return null;
    for (const raw of entries.slice(0, MAX_SELF_ENTRIES)) {
      const entry = parseEntry(raw);
      if (!entry) return null;
      parsed.push(entry);
    }
  }
  // 已列入必须有理由可展示；未列入不展示任何条目。
  if (listed && parsed.length === 0) return null;

  const self: BtGuardSelf = { listed, entries: listed ? parsed : [], appealPending: appealPending === true };
  if (typeof ipGroup === "string" && ipGroup !== "") self.ipGroup = ipGroup;
  if (typeof turnstileSiteKey === "string" && turnstileSiteKey !== "") self.turnstileSiteKey = turnstileSiteKey;
  return self;
}

export type AppealRequest =
  | { ok: true; detail: string; contact?: string; turnstileToken?: string }
  | { ok: false };

const EMAIL_RE = /^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/;

/** 可选文本：缺省 / null / 空白 → 省略（null）；非字符串或超长 → 非法（undefined）。 */
function optionalText(value: unknown, max: number): string | null | undefined {
  if (value === undefined || value === null) return null;
  if (typeof value !== "string") return undefined;
  const text = value.trim();
  if (text === "") return null;
  return text.length > max ? undefined : text;
}

/** 校验 `POST …/appeal` 请求体：detail 必填 ≤ 2000，contact 可选 ≤ 200 且须为邮箱，turnstileToken ≤ 2048。 */
export function parseAppealRequest(text: string): AppealRequest {
  if (text.length > MAX_APPEAL_BODY) return { ok: false };
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    return { ok: false };
  }
  if (!isRecord(body)) return { ok: false };
  if (typeof body.detail !== "string") return { ok: false };
  const detail = body.detail.trim();
  if (detail === "" || detail.length > MAX_APPEAL_DETAIL) return { ok: false };
  const contact = optionalText(body.contact, MAX_APPEAL_CONTACT);
  const token = optionalText(body.turnstileToken, MAX_TURNSTILE_TOKEN);
  if (contact === undefined || token === undefined) return { ok: false };
  if (contact !== null && !EMAIL_RE.test(contact)) return { ok: false };
  return {
    ok: true,
    detail,
    ...(contact !== null && { contact }),
    ...(token !== null && { turnstileToken: token }),
  };
}

/**
 * 流式读取请求体，累计字节超过 `max` 立即取消并返回 null（不依赖 content-length，chunked 也受限）。
 * 无请求体返回空串。
 */
export async function readBodyCapped(request: Request, max: number): Promise<string | null> {
  if (!request.body) return "";
  const reader = request.body.getReader();
  const decoder = new TextDecoder();
  let total = 0;
  let text = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > max) {
      await reader.cancel().catch(() => undefined);
      return null;
    }
    text += decoder.decode(value, { stream: true });
  }
  return text + decoder.decode();
}

export type BtGuardErrorKey =
  | "notListed"
  | "appealPending"
  | "turnstileRequired"
  | "turnstileFailed"
  | "rateLimited"
  | "unavailable"
  | "generic";

/** 自查 / 申诉失败 → 文案键：先看 FluxCloud 错误码，再看状态码。 */
export function btGuardErrorKey(status: number, code: string | undefined): BtGuardErrorKey {
  switch (code) {
    case "not_listed":
      return "notListed";
    case "appeal_pending":
      return "appealPending";
    case "turnstile_required":
      return "turnstileRequired";
    case "turnstile_failed":
      return "turnstileFailed";
    case "rate_limited":
      return "rateLimited";
  }
  if (status === 404) return "notListed";
  if (status === 409) return "appealPending";
  if (status === 429) return "rateLimited";
  if (status === 503) return "unavailable";
  return "generic";
}

/** unix 秒 → 本地化日期时间；非法值返回 "—"。 */
export function formatExpiry(unixSecs: number, lang: "en" | "zh"): string {
  const date = new Date(unixSecs * 1000);
  if (!Number.isFinite(date.getTime())) return "—";
  return new Intl.DateTimeFormat(lang === "zh" ? "zh-CN" : "en-US", {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}
