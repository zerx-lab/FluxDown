/**
 * 账户中心会话（BFF）：FluxCloud 令牌只存在 HttpOnly Cookie，浏览器脚本拿不到。
 *
 * - `fd_acct_rt` 刷新令牌（滑动 90 天，每次刷新轮换）、`fd_acct_at` 访问令牌（15 分钟，提前 60 秒过期）：
 *   HttpOnly + SameSite=Strict + 生产环境 Secure，Path 限定在 `/api/account`，只随 BFF 请求发送。
 * - `fd_acct_dev` 本浏览器的 Web 会话 deviceId（随机 UUID，400 天）：FluxCloud 按它做新设备邮箱验证，
 *   `devicePlatform: "web"` 的会话不占 App 设备名额、不出现在 App 设备列表、不能被派发任务。
 * - `fd_signed_in=1` 非 HttpOnly 的登录态提示（Path `/`），只供静态页决定显示「登录」还是「账户」。
 *
 * 刷新单飞：同一刷新令牌同时只发一次 `/auth/refresh`，结果再缓存 30 秒给晚到的并发请求，
 * 避免并发请求各自轮换、互相把对方的新令牌顶掉（单进程内；多副本部署时 FluxCloud 的轮换重放宽限兜底）。
 */
import { createHash, randomUUID } from "node:crypto";
import { withBase } from "@/lib/base";
import { callUpstream, failure, type ProxyFailure } from "@/lib/cloud-proxy";
import { isRecord } from "@/lib/is-record";

export const REFRESH_COOKIE = "fd_acct_rt";
export const ACCESS_COOKIE = "fd_acct_at";
export const DEVICE_COOKIE = "fd_acct_dev";
/** 与 `src/lib/account/client.ts::SIGNED_IN_COOKIE` 同值。 */
export const SIGNED_IN_COOKIE = "fd_signed_in";

/** FluxCloud 识别 Web 会话的设备平台值（`auth.rs::WEB_DEVICE_PLATFORM`）。 */
export const WEB_DEVICE_PLATFORM = "web";

const REFRESH_TTL_S = 90 * 24 * 3600;
const DEVICE_TTL_S = 400 * 24 * 3600;
const ACCESS_SKEW_S = 60;
const REFRESH_REUSE_MS = 30_000;
const DEVICE_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/** AstroCookies 的最小子集（便于测试替身）。 */
export interface CookieJar {
  get(name: string): { value: string } | undefined;
  set(name: string, value: string, options: CookieOptions): void;
  delete(name: string, options: { path: string }): void;
}

export interface CookieOptions {
  httpOnly: boolean;
  sameSite: "strict" | "lax";
  secure: boolean;
  path: string;
  maxAge: number;
}

export interface SessionTokens {
  accessToken: string;
  refreshToken: string;
  /** 访问令牌剩余秒数。 */
  expiresIn: number;
}

export interface AccountContext {
  cookies: CookieJar;
  /** 访客真实 IP（透传给 FluxCloud 限频）。 */
  ip: string;
}

export type UpstreamResult = { ok: true; body: unknown } | ProxyFailure;

const secure = (): boolean => Boolean(import.meta.env.PROD);
const tokenPath = (): string => withBase("/api/account");
const hintPath = (): string => withBase("/");

function tokenCookie(maxAge: number): CookieOptions {
  return { httpOnly: true, sameSite: "strict", secure: secure(), path: tokenPath(), maxAge };
}

/** FluxCloud `AuthResponse` → 令牌三件套；形状不对返回 null。 */
export function parseTokens(body: unknown): SessionTokens | null {
  if (!isRecord(body)) return null;
  const { accessToken, refreshToken, expiresIn } = body;
  if (typeof accessToken !== "string" || accessToken === "") return null;
  if (typeof refreshToken !== "string" || refreshToken === "") return null;
  if (typeof expiresIn !== "number" || !Number.isFinite(expiresIn)) return null;
  return { accessToken, refreshToken, expiresIn };
}

export function writeSession(cookies: CookieJar, tokens: SessionTokens): void {
  cookies.set(REFRESH_COOKIE, tokens.refreshToken, tokenCookie(REFRESH_TTL_S));
  const accessAge = Math.floor(tokens.expiresIn) - ACCESS_SKEW_S;
  if (accessAge > 0) cookies.set(ACCESS_COOKIE, tokens.accessToken, tokenCookie(accessAge));
  else cookies.delete(ACCESS_COOKIE, { path: tokenPath() });
  cookies.set(SIGNED_IN_COOKIE, "1", { httpOnly: false, sameSite: "lax", secure: secure(), path: hintPath(), maxAge: REFRESH_TTL_S });
}

/** 清掉会话令牌与登录态提示；设备 Cookie 保留（同一浏览器再次登录沿用同一 Web 会话身份）。 */
export function clearSession(cookies: CookieJar): void {
  cookies.delete(REFRESH_COOKIE, { path: tokenPath() });
  cookies.delete(ACCESS_COOKIE, { path: tokenPath() });
  cookies.delete(SIGNED_IN_COOKIE, { path: hintPath() });
}

export function hasSession(cookies: CookieJar): boolean {
  return Boolean(cookies.get(REFRESH_COOKIE)?.value || cookies.get(ACCESS_COOKIE)?.value);
}

/** 签发令牌类请求要 flatten 进请求体的设备三元组；本浏览器还没有 deviceId 时生成并写 Cookie。 */
export function webDevice(cookies: CookieJar): { deviceId: string; devicePlatform: string } {
  const existing = cookies.get(DEVICE_COOKIE)?.value;
  const deviceId = existing && DEVICE_ID_RE.test(existing) ? existing : randomUUID();
  // 每次都续期，长期不用的浏览器 400 天后才换新身份。
  cookies.set(DEVICE_COOKIE, deviceId, tokenCookie(DEVICE_TTL_S));
  return { deviceId, devicePlatform: WEB_DEVICE_PLATFORM };
}

type RefreshOutcome = { ok: true; tokens: SessionTokens } | ProxyFailure;

const inflight = new Map<string, Promise<RefreshOutcome>>();
const recent = new Map<string, { tokens: SessionTokens; at: number }>();

function rememberRefresh(key: string, tokens: SessionTokens, now: number): void {
  for (const [k, entry] of recent) if (now - entry.at >= REFRESH_REUSE_MS) recent.delete(k);
  recent.set(key, { tokens, at: now });
}

/** 测试用：清空刷新单飞与复用缓存。 */
export function resetRefreshCache(): void {
  inflight.clear();
  recent.clear();
}

async function refreshOnce(refreshToken: string, ip: string): Promise<RefreshOutcome> {
  const key = createHash("sha256").update(refreshToken).digest("hex");
  const now = Date.now();
  const reused = recent.get(key);
  if (reused && now - reused.at < REFRESH_REUSE_MS) return { ok: true, tokens: reused.tokens };
  const pending = inflight.get(key);
  if (pending) return pending;
  const task = (async (): Promise<RefreshOutcome> => {
    const res = await callUpstream(
      "/api/v1/auth/refresh",
      { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ refreshToken }) },
      ip,
    );
    if (!res.ok) return res;
    const tokens = parseTokens(res.body);
    if (!tokens) return failure(502, "upstream_invalid");
    rememberRefresh(key, tokens, Date.now());
    return { ok: true, tokens };
  })().finally(() => inflight.delete(key));
  inflight.set(key, task);
  return task;
}

/** 刷新并写回 Cookie；会话失效（401）或账户停用（403）时清会话。 */
async function renew(ctx: AccountContext, refreshToken: string): Promise<{ ok: true; accessToken: string } | ProxyFailure> {
  const out = await refreshOnce(refreshToken, ctx.ip);
  if (!out.ok) {
    if (out.status === 401) {
      clearSession(ctx.cookies);
      return failure(401, "unauthorized");
    }
    if (out.status === 403) clearSession(ctx.cookies);
    return out;
  }
  writeSession(ctx.cookies, out.tokens);
  return { ok: true, accessToken: out.tokens.accessToken };
}

function withBearer(init: { method: string; headers?: Record<string, string>; body?: string }, accessToken: string) {
  return { ...init, headers: { ...init.headers, Authorization: `Bearer ${accessToken}` } };
}

const sessionExpired = (res: UpstreamResult): boolean => !res.ok && res.status === 401 && res.code === "unauthorized";

/**
 * 以当前会话调用 FluxCloud：访问令牌缺失先刷新；上游 401 `unauthorized` 刷新后重放一次。
 * 只有 `unauthorized` 视为会话失效——`invalid_code` 等同为 401 的业务错误原样返回。
 */
export async function callAuthed(
  ctx: AccountContext,
  path: string,
  init: { method: string; headers?: Record<string, string>; body?: string },
  expectBody = true,
): Promise<UpstreamResult> {
  const refreshToken = ctx.cookies.get(REFRESH_COOKIE)?.value;
  let accessToken = ctx.cookies.get(ACCESS_COOKIE)?.value;
  if (!accessToken) {
    if (!refreshToken) {
      clearSession(ctx.cookies);
      return failure(401, "unauthorized");
    }
    const renewed = await renew(ctx, refreshToken);
    if (!renewed.ok) return renewed;
    accessToken = renewed.accessToken;
  }

  let res = await callUpstream(path, withBearer(init, accessToken), ctx.ip, expectBody);
  if (sessionExpired(res) && refreshToken) {
    const renewed = await renew(ctx, refreshToken);
    if (!renewed.ok) return renewed;
    res = await callUpstream(path, withBearer(init, renewed.accessToken), ctx.ip, expectBody);
  }
  if (sessionExpired(res)) clearSession(ctx.cookies);
  else if (!res.ok && res.status === 403 && res.code === "account_disabled") clearSession(ctx.cookies);
  return res;
}
