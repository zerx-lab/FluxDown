/**
 * 账户中心 BFF 路由表：`/api/account/<path>` → FluxCloud `/api/v1/*`（入口 `src/pages/api/account/[...path].ts`）。
 *
 * - 只开放白名单里的方法 + 路径；请求体只取已知字段重建后转发（设备三元组由服务端注入，浏览器无法伪造）。
 * - 响应按 `src/lib/account/types.ts` 只回已知字段，任何响应都不含令牌；失败透传 FluxCloud 的状态码与 `code`。
 * - CSRF：每个请求必须带 `X-Fluxdown-Account: 1`（跨站带自定义头必须过 CORS 预检，本站不应答预检），
 *   浏览器给出的 `Sec-Fetch-Site` 必须是 `same-origin`；会话 Cookie 另有 SameSite=Strict。
 */
import { failure, failureResponse, proxyJson, proxyNoContent, callUpstream } from "@/lib/cloud-proxy";
import { isRecord, pick } from "@/lib/is-record";
import { AI_ROUTES } from "./ai-routes";
import {
  DEFAULT_MAX_BODY_BYTES, ROW_ID_RE, invalidUpstream, jsonInit, mapped, text, validationError, type Body, type Route,
} from "./route-kit";
import {
  callAuthed, clearSession, parseTokens, webDevice, writeSession, REFRESH_COOKIE,
  type AccountContext, type CookieJar, type UpstreamResult,
} from "./session";

/** 与 `src/lib/account/client.ts::ACCOUNT_REQUEST_HEADER` 同值（HTTP 头名大小写不敏感）。 */
export const ACCOUNT_REQUEST_HEADER = "x-fluxdown-account";

const ME_KEYS = [
  "id", "email", "nickname", "plan", "status", "originId", "originIdChanged", "membershipOrdinal", "hasPassword",
  "createdAt", "lastLoginAt", "entitlements", "currentPlan", "purchaseCreditMinor", "activeSubscription",
] as const;
const DEVICE_KEYS = ["id", "name", "platform", "lastIp", "appVersion", "createdAt", "lastSeenAt", "isOnline"] as const;
const ORDER_KEYS = [
  "orderNo", "planCode", "planName", "status", "amountMinor", "listPriceMinor", "creditMinor", "referralCode",
  "referralDiscountMinor", "upgradeFromPlan", "currency", "campaignName", "stageLabel", "createdAt", "paidAt",
  "expiresAt", "payMethod", "payAmountMicros",
] as const;
const CHANNEL_KEYS = [
  "id", "kind", "name", "enabled", "events", "target", "status", "lastError", "lastDeliveredAt", "createdAt",
] as const;

/** 登录类成功：FluxCloud `AuthResponse` → 写会话 Cookie，只回 `{status:"ok"}`。 */
function signIn(cookies: CookieJar, auth: unknown): Response {
  const tokens = parseTokens(auth);
  if (!tokens) return invalidUpstream();
  writeSession(cookies, tokens);
  return proxyJson({ status: "ok" });
}

const codeSent = (body: unknown) => (isRecord(body) && typeof body.ttlSeconds === "number" ? { ttlSeconds: body.ttlSeconds } : null);
const meOf = (body: unknown) => pick(body, ME_KEYS);

async function publicPost(ctx: AccountContext, path: string, payload: Body, expectBody = true): Promise<UpstreamResult> {
  return callUpstream(path, jsonInit("POST", payload), ctx.ip, expectBody);
}

const ACCOUNT_ROUTES: Route[] = [
  {
    method: "POST",
    path: /^login$/,
    json: true,
    async handle(ctx, body) {
      const account = text(body, "account", 254);
      const password = text(body, "password", 256, false);
      if (!account || !password) return validationError();
      const res = await publicPost(ctx, "/api/v1/auth/login", { account, password, ...webDevice(ctx.cookies) });
      if (!res.ok) return failureResponse(res);
      if (!isRecord(res.body)) return invalidUpstream();
      if (res.body.status === "ok") return signIn(ctx.cookies, res.body.auth);
      if (res.body.status === "deviceVerificationRequired" && typeof res.body.ttlSeconds === "number") {
        return proxyJson({ status: "verify", ttlSeconds: res.body.ttlSeconds });
      }
      return invalidUpstream();
    },
  },
  {
    method: "POST",
    path: /^login\/verify$/,
    json: true,
    async handle(ctx, body) {
      const account = text(body, "account", 254);
      const password = text(body, "password", 256, false);
      const code = text(body, "code", 16);
      if (!account || !password || !code) return validationError();
      const res = await publicPost(ctx, "/api/v1/auth/login/verify", { account, password, code, ...webDevice(ctx.cookies) });
      return res.ok ? signIn(ctx.cookies, res.body) : failureResponse(res);
    },
  },
  {
    method: "POST",
    path: /^code\/send$/,
    json: true,
    async handle(ctx, body) {
      const email = text(body, "email", 254);
      if (!email) return validationError();
      return mapped(await publicPost(ctx, "/api/v1/auth/code/send", { email }), codeSent);
    },
  },
  {
    method: "POST",
    path: /^code\/login$/,
    json: true,
    async handle(ctx, body) {
      const email = text(body, "email", 254);
      const code = text(body, "code", 16);
      if (!email || !code) return validationError();
      const res = await publicPost(ctx, "/api/v1/auth/code/verify", { email, code, ...webDevice(ctx.cookies) });
      return res.ok ? signIn(ctx.cookies, res.body) : failureResponse(res);
    },
  },
  {
    method: "POST",
    path: /^register$/,
    json: true,
    async handle(ctx, body) {
      const email = text(body, "email", 254);
      const password = text(body, "password", 256, false);
      if (!email || !password) return validationError();
      // 昵称选填：缺省 / 空白交给 FluxCloud 生成随机昵称；给了就按长度校验。
      const hasNickname = typeof body.nickname === "string" && body.nickname.trim() !== "";
      const nickname = hasNickname ? text(body, "nickname", 64) : null;
      if (hasNickname && nickname === null) return validationError();
      const payload: Body = nickname === null ? { email, password } : { email, password, nickname };
      return mapped(await publicPost(ctx, "/api/v1/auth/register", payload), codeSent);
    },
  },
  {
    method: "POST",
    path: /^register\/verify$/,
    json: true,
    async handle(ctx, body) {
      const email = text(body, "email", 254);
      const code = text(body, "code", 16);
      if (!email || !code) return validationError();
      const res = await publicPost(ctx, "/api/v1/auth/register/verify", { email, code, ...webDevice(ctx.cookies) });
      return res.ok ? signIn(ctx.cookies, res.body) : failureResponse(res);
    },
  },
  {
    method: "POST",
    path: /^password\/code$/,
    json: true,
    async handle(ctx, body) {
      const email = text(body, "email", 254);
      if (!email) return validationError();
      return mapped(await publicPost(ctx, "/api/v1/auth/password/code", { email }), codeSent);
    },
  },
  {
    method: "POST",
    path: /^password\/reset$/,
    json: true,
    async handle(ctx, body) {
      const email = text(body, "email", 254);
      const code = text(body, "code", 16);
      const newPassword = text(body, "newPassword", 256, false);
      if (!email || !code || !newPassword) return validationError();
      const res = await publicPost(ctx, "/api/v1/auth/password/reset", { email, code, newPassword }, false);
      if (!res.ok) return failureResponse(res);
      // 重置吊销该账户全部会话：本浏览器若正登录着同一账户，令牌也已失效。
      clearSession(ctx.cookies);
      return proxyNoContent();
    },
  },
  {
    method: "POST",
    path: /^logout$/,
    json: false,
    async handle(ctx) {
      const refreshToken = ctx.cookies.get(REFRESH_COOKIE)?.value;
      if (refreshToken) {
        const res = await publicPost(ctx, "/api/v1/auth/logout", { refreshToken }, false);
        // 上游暂时不可用时保留会话，让用户重试；令牌已失效（401）同样算登出成功。
        if (!res.ok && res.status !== 401) return failureResponse(res);
      }
      clearSession(ctx.cookies);
      return proxyNoContent();
    },
  },
  {
    method: "GET",
    path: /^me$/,
    json: false,
    async handle(ctx) {
      return mapped(await callAuthed(ctx, "/api/v1/me", { method: "GET" }), meOf);
    },
  },
  {
    method: "PUT",
    path: /^me\/nickname$/,
    json: true,
    async handle(ctx, body) {
      const nickname = text(body, "nickname", 64);
      if (!nickname) return validationError();
      return mapped(await callAuthed(ctx, "/api/v1/me/nickname", jsonInit("PUT", { nickname })), meOf);
    },
  },
  {
    method: "POST",
    path: /^me\/password\/code$/,
    json: false,
    async handle(ctx) {
      return mapped(await callAuthed(ctx, "/api/v1/me/password/code", { method: "POST" }), codeSent);
    },
  },
  {
    method: "POST",
    path: /^me\/password$/,
    json: true,
    async handle(ctx, body) {
      const newPassword = text(body, "newPassword", 256, false);
      const currentPassword = body.currentPassword === undefined ? null : text(body, "currentPassword", 256, false);
      const code = body.code === undefined ? null : text(body, "code", 16);
      // 恰好一种验证方式（与 FluxCloud `PasswordChangeRequest` 同约束）。
      if (!newPassword || (currentPassword === null) === (code === null)) return validationError();
      const payload: Body = currentPassword === null ? { newPassword, code } : { newPassword, currentPassword };
      return mapped(await callAuthed(ctx, "/api/v1/me/password", jsonInit("POST", payload)), meOf);
    },
  },
  {
    method: "GET",
    path: /^devices$/,
    json: false,
    async handle(ctx) {
      return mapped(await callAuthed(ctx, "/api/v1/devices", { method: "GET" }), (body) => {
        if (!isRecord(body) || !Array.isArray(body.devices)) return null;
        const devices = body.devices.map((device) => pick(device, DEVICE_KEYS));
        return devices.includes(null) ? null : { devices };
      });
    },
  },
  {
    method: "PATCH",
    path: /^devices\/([^/]+)$/,
    json: true,
    async handle(ctx, body, [id]) {
      const name = text(body, "name", 64);
      if (!name || !id || !ROW_ID_RE.test(id)) return validationError();
      const res = await callAuthed(ctx, `/api/v1/devices/${encodeURIComponent(id)}`, jsonInit("PATCH", { name }));
      return mapped(res, (device) => pick(device, DEVICE_KEYS));
    },
  },
  {
    method: "DELETE",
    path: /^devices\/([^/]+)$/,
    json: false,
    async handle(ctx, _body, [id]) {
      if (!id || !ROW_ID_RE.test(id)) return validationError();
      const res = await callAuthed(ctx, `/api/v1/devices/${encodeURIComponent(id)}`, { method: "DELETE" }, false);
      return res.ok ? proxyNoContent() : failureResponse(res);
    },
  },
  {
    method: "GET",
    path: /^orders$/,
    json: false,
    async handle(ctx) {
      return mapped(await callAuthed(ctx, "/api/v1/orders", { method: "GET" }), (body) => {
        if (!Array.isArray(body)) return null;
        const orders = body.map((order) => pick(order, ORDER_KEYS));
        return orders.includes(null) ? null : orders;
      });
    },
  },
  {
    method: "GET",
    path: /^notifications$/,
    json: false,
    async handle(ctx) {
      return mapped(await callAuthed(ctx, "/api/v1/notifications/overview", { method: "GET" }), (body) => {
        if (!isRecord(body) || !Array.isArray(body.channels)) return null;
        const channels = body.channels.map((channel) => pick(channel, CHANNEL_KEYS));
        if (channels.includes(null)) return null;
        return { enabled: body.enabled, usage: body.usage, maxChannels: body.maxChannels, channels };
      });
    },
  },
];

const ROUTES: Route[] = [...ACCOUNT_ROUTES, ...AI_ROUTES];

/** CSRF 闸：自定义头（`simpleGet` 路由的 GET 免）+ `Sec-Fetch-Site`（浏览器给出时必须同源）。 */
function crossSite(request: Request, route: Route): boolean {
  const headerless = route.simpleGet === true && request.method === "GET";
  if (!headerless && request.headers.get(ACCOUNT_REQUEST_HEADER) !== "1") return true;
  const site = request.headers.get("sec-fetch-site");
  return site !== null && site !== "same-origin";
}

async function readJsonBody(request: Request, maxBytes: number): Promise<Body | null> {
  const type = request.headers.get("content-type")?.toLowerCase() ?? "";
  if (!type.startsWith("application/json")) return null;
  // 声明的长度已超限就不读正文（带图提问的上限有几十 MB，没必要先整个收下来）
  if (Number(request.headers.get("content-length") ?? 0) > maxBytes) return null;
  const raw = await request.text();
  if (Buffer.byteLength(raw) > maxBytes) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    return isRecord(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

/**
 * 处理一次 `/api/account/<path>` 请求。
 * 404 `not_found` = 无此路径；405 `method_not_allowed`；403 `forbidden` = CSRF 闸；400 `bad_request` = 请求体不是
 * JSON 对象或超过路由的上限（缺省 16 KiB）。
 */
export async function handleAccountRequest(request: Request, path: string, cookies: CookieJar, ip: string): Promise<Response> {
  const candidates = ROUTES.filter((route) => route.path.test(path));
  if (candidates.length === 0) return failureResponse(failure(404, "not_found"));
  const route = candidates.find((r) => r.method === request.method);
  if (!route) return failureResponse(failure(405, "method_not_allowed"));
  if (crossSite(request, route)) return failureResponse(failure(403, "forbidden"));

  let body: Body = {};
  if (route.json) {
    const parsed = await readJsonBody(request, route.maxBody ?? DEFAULT_MAX_BODY_BYTES);
    if (!parsed) return failureResponse(failure(400, "bad_request"));
    body = parsed;
  }
  const params = route.path.exec(path)?.slice(1) ?? [];
  const query = new URL(request.url).searchParams;
  return route.handle({ cookies, ip, signal: request.signal }, body, params, query);
}
