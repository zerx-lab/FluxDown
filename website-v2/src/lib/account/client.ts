/**
 * 账户中心浏览器侧小件：调用同源 BFF `/api/account/*`、错误码 → 文案、登录态提示 Cookie、页面跳转地址。
 *
 * 会话令牌只在 HttpOnly Cookie 里（`src/lib/account/session.ts`），脚本读不到；
 * `fd_signed_in=1` 只是给顶栏等静态页判断「显示登录还是账户」的提示，不代表会话一定有效——
 * 真正的登录态以 `GET /api/account/me` 为准（401 = 未登录 / 已失效）。
 */
import type { Lang } from "@/i18n/config";
import { accountErrors } from "@/i18n/messages/accountErrors";
import { href } from "@/i18n/routing";
import { withBase } from "@/lib/base";
import { isSafeLocalPath } from "@/lib/return-to";

/** 非 HttpOnly 的登录态提示 Cookie 名（与 session.ts 同值）。 */
export const SIGNED_IN_COOKIE = "fd_signed_in";

/** 每个 BFF 请求都带的自定义头：跨站请求带它必须先过 CORS 预检，BFF 不应答预检，据此挡 CSRF（与 routes.ts 同值）。 */
export const ACCOUNT_REQUEST_HEADER = "X-Fluxdown-Account";

export type AccountResult<T> =
  | { ok: true; data: T }
  | { ok: false; status: number; code: string; retryAfter: number | null };

/**
 * 调用 `/api/account/<path>`。有 `body` 时以 JSON 发送；204 的 `data` 为 `undefined`。
 * 网络失败返回 `status: 0, code: "network"`；非 2xx 返回 BFF / FluxCloud 的 `code`。
 */
export async function accountApi<T = undefined>(
  path: string,
  init: { method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE"; body?: unknown } = {},
): Promise<AccountResult<T>> {
  const method = init.method ?? (init.body === undefined ? "GET" : "POST");
  const headers: Record<string, string> = { Accept: "application/json", [ACCOUNT_REQUEST_HEADER]: "1" };
  if (init.body !== undefined) headers["Content-Type"] = "application/json";
  let res: Response;
  try {
    res = await fetch(withBase(`/api/account/${path}`), {
      method,
      headers,
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
      credentials: "same-origin",
    });
  } catch {
    return { ok: false, status: 0, code: "network", retryAfter: null };
  }
  if (res.ok) {
    if (res.status === 204) return { ok: true, data: undefined as T };
    try {
      return { ok: true, data: (await res.json()) as T };
    } catch {
      return { ok: false, status: res.status, code: "upstream_invalid", retryAfter: null };
    }
  }
  let code = "fallback";
  try {
    const body = (await res.json()) as { code?: unknown };
    if (typeof body.code === "string" && body.code !== "") code = body.code;
  } catch {
    // 非 JSON 错误体：按通用错误展示
  }
  const retry = Number(res.headers.get("retry-after"));
  return { ok: false, status: res.status, code, retryAfter: Number.isFinite(retry) && retry > 0 ? retry : null };
}

/** 错误码 → 当前语言文案；未登记的 code 用通用文案。 */
export function accountErrorText(lang: Lang, code: string): string {
  const t = accountErrors[lang];
  return Object.hasOwn(t, code) ? t[code as keyof typeof t] : t.fallback;
}

/** 浏览器里是否带着登录态提示（只用于决定显示「登录」还是「账户」，不作鉴权依据）。 */
export function hasSignedInHint(): boolean {
  if (typeof document === "undefined") return false;
  return document.cookie.split(";").some((part) => part.trim() === `${SIGNED_IN_COOKIE}=1`);
}

export type AccountPage = "home" | "login" | "register" | "forgot";

const PAGE_PATH: Record<AccountPage, string> = {
  home: "/account",
  login: "/account/login",
  register: "/account/register",
  forgot: "/account/forgot",
};

/**
 * 账户页面地址；`next` 是登录后要回到的站内路径（已带挂载前缀，如 `location.pathname + location.search`），
 * 只保留通过 `isSafeLocalPath` 的值。
 */
export function accountHref(lang: Lang, page: AccountPage, next?: string | null): string {
  const base = href(PAGE_PATH[page], lang);
  return next && isSafeLocalPath(next) ? `${base}?next=${encodeURIComponent(next)}` : base;
}

/** 读取当前地址的 `next` 参数：安全的站内路径原样返回，否则回到账户中心首页。 */
export function nextAfterSignIn(lang: Lang): string {
  if (typeof location === "undefined") return accountHref(lang, "home");
  const next = new URLSearchParams(location.search).get("next");
  return isSafeLocalPath(next) ? next : accountHref(lang, "home");
}

/** 当前页面地址（含查询串），作为跳去登录页时的 `next`。 */
export function currentPath(): string {
  return typeof location === "undefined" ? "/" : `${location.pathname}${location.search}${location.hash}`;
}
