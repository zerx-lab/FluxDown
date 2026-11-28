/**
 * AI 助手 BFF 路由：`/api/account/ai/<path>` → FluxCloud `/api/v1/ai/*`（契约见 FluxCloud README「AI 助手」）。
 * 挂在账户 BFF 下，复用会话 Cookie（Path `/api/account`）、刷新与 CSRF 闸。
 *
 * - 会话一律以 `channel: "website"` 创建，不声明客户端工具（官网没有本机状态可查）。
 * - 会话详情只回用户 / 助手消息，用户消息带图片 id，图片经 `ai/attachments/{id}` 取。工具调用与结果不下发，唯一例外：
 *   上游 `offer_membership` 成功的工具结果（`role: "tool_result"`、`isError: false`）以官网本地合成的
 *   `role: "membership_offer"` 消息下发（`content` 置空，只带白名单字段，`runId` 即所属执行），
 *   前端据此在该轮显示开通会员卡片；失败的结果与其它工具消息一律不下发。
 * - 提问的 SSE 按 `ai-stream.ts` 白名单逐帧裁剪后转发；访客断开即中止上游，FluxCloud 随之取消本次执行。
 * - 提问可带图片（`data:image/(png|jpeg|gif|webp);base64,...`，张数 / 单张大小同 FluxCloud `ai::attachments`）；
 *   BFF 只做形状与长度预检，格式识别与解码后大小由 FluxCloud 判定。
 */
import { callUpstream, failureResponse, NOINDEX_ROBOTS, proxyNoContent } from "@/lib/cloud-proxy";
import { isRecord, pick } from "@/lib/is-record";
import { AI_ISSUE_KEYS, aiEventFilter } from "./ai-stream";
import { jsonInit, mapped, ROW_ID_RE, text, validationError, type Body, type Route } from "./route-kit";
import { callAuthed, callAuthedStream, hasSession, type AccountContext } from "./session";

/** FluxCloud `limits.maxMessageChars` 的取值上限；实际上限由 FluxCloud 按设置再校验。 */
const MAX_ASK_CHARS = 16_000;
/** FluxCloud `ai::issues`：确认时标题 4..=136 字、正文 ≤ 20000 字。 */
const MAX_ISSUE_TITLE_CHARS = 136;
const MAX_ISSUE_BODY_CHARS = 20_000;
const MAX_RATING_COMMENT_CHARS = 500;
/** 问题单确认的请求体上限（20000 字的 UTF-8 + JSON 转义余量）。 */
const LARGE_BODY_BYTES = 96 * 1024;
/** FluxCloud `ai::attachments`：每次提问最多 4 张、单张解码后 ≤ 5 MiB。 */
const MAX_IMAGES = 4;
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
/** 单张 data URL 的最大长度：base64 膨胀 4/3（含填充）+ 前缀。 */
const MAX_IMAGE_URL_CHARS = Math.ceil(MAX_IMAGE_BYTES / 3) * 4 + 64;
/** 提问请求体上限：与 FluxCloud `MAX_ASK_BODY_BYTES` 同口径（图片 + 1 MiB 给文本与字段）。 */
const ASK_BODY_BYTES = MAX_IMAGES * MAX_IMAGE_URL_CHARS + 1024 * 1024;
const IMAGE_URL_RE = /^data:image\/(?:png|jpeg|gif|webp);base64,[A-Za-z0-9+/]+={0,2}$/;
const MAX_PAGE = 100;
const LOCALE_RE = /^[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8}){0,2}$/;

const CONFIG_KEYS = [
  "available", "reason", "visible", "maxMessageChars", "dailyLimit", "usedToday", "imagesSupported", "maxImages", "maxImageBytes",
] as const;
const SESSION_KEYS = [
  "id", "channel", "kind", "title", "messageCount", "rating", "active", "createdAt", "updatedAt",
] as const;
const MESSAGE_KEYS = ["id", "runId", "seq", "role", "content", "rating", "ratingComment", "createdAt"] as const;
const SHOWN_ROLES: Record<string, true> = { user: true, assistant: true };
/** 开通会员卡片的唯一信号：FluxCloud 的服务端工具名与官网本地合成的消息角色。 */
const OFFER_MEMBERSHIP_TOOL = "offer_membership";
const MEMBERSHIP_OFFER_ROLE = "membership_offer";

const SSE_HEADERS = {
  "Content-Type": "text/event-stream; charset=utf-8",
  "Cache-Control": "no-cache, no-store, no-transform",
  "X-Accel-Buffering": "no",
  "X-Robots-Tag": NOINDEX_ROBOTS,
};

const sessionOf = (body: unknown) => pick(body, SESSION_KEYS);
const issueOf = (body: unknown) => pick(body, AI_ISSUE_KEYS);

/** 列表里的每一项都要合形，否则整体视为上游异常。 */
function all<T>(items: unknown, map: (item: unknown) => T | null): T[] | null {
  if (!Array.isArray(items)) return null;
  const out = items.map(map);
  return out.includes(null) ? null : (out as T[]);
}

function messageOf(message: unknown): Body | null {
  const out = pick(message, MESSAGE_KEYS);
  if (!out || !isRecord(message)) return null;
  const images = message.images === undefined ? [] : message.images;
  if (!Array.isArray(images) || !images.every((id) => typeof id === "string" && ROW_ID_RE.test(id))) return null;
  out.images = images;
  return out;
}

/** 提问图片：缺省为空；否则须是 ≤ 4 个合形的 data URL。null = 不合法。 */
function askImages(body: Body): string[] | null {
  const images = body.images === undefined ? [] : body.images;
  if (!Array.isArray(images) || images.length > MAX_IMAGES) return null;
  const ok = images.every((url) => typeof url === "string" && url.length <= MAX_IMAGE_URL_CHARS && IMAGE_URL_RE.test(url));
  return ok ? (images as string[]) : null;
}

/** 界面语言（BCP 47）：缺省为 undefined；否则须合形。null = 不合法。 */
function askLocale(body: Body): string | undefined | null {
  if (body.locale === undefined) return undefined;
  const locale = text(body, "locale", 35);
  return locale !== null && LOCALE_RE.test(locale) ? locale : null;
}

/** 上游成功的 `offer_membership` 工具结果（须带所属执行 `runId`，卡片按它挂到轮次上）。 */
function isMembershipOfferResult(message: Record<string, unknown>): boolean {
  return (
    message.role === "tool_result" &&
    message.toolName === OFFER_MEMBERSHIP_TOOL &&
    message.isError === false &&
    typeof message.runId === "string"
  );
}

/** 工具结果 → 合成的 `membership_offer` 消息：只留白名单字段，正文清空（工具输出是给模型看的）。 */
function membershipOfferOf(message: unknown): Body | null {
  const out = pick(message, MESSAGE_KEYS);
  return out && { ...out, role: MEMBERSHIP_OFFER_ROLE, content: "", images: [] };
}

function messagesOf(upstream: unknown): Body[] | null {
  if (!Array.isArray(upstream)) return null;
  const out: Body[] = [];
  for (const message of upstream) {
    if (!isRecord(message)) continue;
    const mapped = Object.hasOwn(SHOWN_ROLES, String(message.role))
      ? messageOf(message)
      : isMembershipOfferResult(message)
        ? membershipOfferOf(message)
        : undefined;
    if (mapped === null) return null;
    if (mapped !== undefined) out.push(mapped);
  }
  return out;
}

function detailOf(body: unknown): Body | null {
  if (!isRecord(body)) return null;
  const session = sessionOf(body.session);
  const messages = messagesOf(body.messages);
  const issues = all(body.issues, issueOf);
  return session && messages && issues ? { session, messages, issues } : null;
}

/** 非负整数查询参数；缺省 / 非法时用 `fallback`，超过 `max` 时截到 `max`。 */
function intParam(query: URLSearchParams, key: string, fallback: number, max: number): number {
  const raw = query.get(key);
  const value = raw !== null && /^\d{1,9}$/.test(raw) ? Number(raw) : fallback;
  return Math.min(value, max);
}

const sessionPath = (id: string, suffix = "") => `/api/v1/ai/sessions/${encodeURIComponent(id)}${suffix}`;
const issuePath = (id: string, action: string) => `/api/v1/ai/issues/${encodeURIComponent(id)}/${action}`;
const validId = (id: string | undefined): id is string => id !== undefined && ROW_ID_RE.test(id);

async function noContent(ctx: AccountContext, path: string, init: { method: string; headers?: Record<string, string>; body?: string }) {
  const res = await callAuthed(ctx, path, init, false);
  return res.ok ? proxyNoContent() : failureResponse(res);
}

/** 入口可见性：上游 `ai/config` 与公开 `ai/status` 都带布尔 `visible`，只取这一个字段。 */
const visibleOf = (body: unknown): Body | null =>
  isRecord(body) && typeof body.visible === "boolean" ? { visible: body.visible } : null;

export const AI_ROUTES: Route[] = [
  {
    method: "GET",
    path: /^ai\/config$/,
    json: false,
    async handle(ctx) {
      return mapped(await callAuthed(ctx, "/api/v1/ai/config", { method: "GET" }), (body) => pick(body, CONFIG_KEYS));
    },
  },
  {
    // 各端 AI 入口是否显示（FluxCloud 判定，官网不复刻规则）。静态页拿不到登录态，所以允许匿名：
    // 有会话按本人身份取 `ai/config.visible`（`internal` 模式只对内部账号显示）；无会话、或会话已失效（401，
    // `callAuthed` 已清会话）退回免鉴权的 `ai/status.visible`。其它上游失败原样返回，客户端据此保留上次已知值。
    method: "GET",
    path: /^ai\/status$/,
    json: false,
    async handle(ctx) {
      if (hasSession(ctx.cookies)) {
        const res = await callAuthed(ctx, "/api/v1/ai/config", { method: "GET" });
        if (res.ok || res.status !== 401) return mapped(res, visibleOf);
      }
      return mapped(await callUpstream("/api/v1/ai/status", { method: "GET" }, ctx.ip), visibleOf);
    },
  },
  {
    method: "GET",
    path: /^ai\/sessions$/,
    json: false,
    async handle(ctx, _body, _params, query) {
      const limit = Math.max(1, intParam(query, "limit", 30, MAX_PAGE));
      const offset = intParam(query, "offset", 0, Number.MAX_SAFE_INTEGER);
      const res = await callAuthed(ctx, `/api/v1/ai/sessions?limit=${limit}&offset=${offset}`, { method: "GET" });
      return mapped(res, (body) => {
        if (!isRecord(body) || typeof body.total !== "number") return null;
        const items = all(body.items, sessionOf);
        return items && { total: body.total, items };
      });
    },
  },
  {
    method: "POST",
    path: /^ai\/sessions$/,
    json: true,
    async handle(ctx, body) {
      const kind = body.kind === undefined ? "ask" : body.kind;
      if (kind !== "ask" && kind !== "feedback") return validationError();
      const locale = askLocale(body);
      if (locale === null) return validationError();
      const client: Body = { clientTools: [] };
      if (locale !== undefined) client.locale = locale;
      const res = await callAuthed(ctx, "/api/v1/ai/sessions", jsonInit("POST", { channel: "website", kind, client }));
      return mapped(res, sessionOf);
    },
  },
  {
    method: "GET",
    path: /^ai\/sessions\/([^/]+)$/,
    json: false,
    async handle(ctx, _body, [id]) {
      if (!validId(id)) return validationError();
      return mapped(await callAuthed(ctx, sessionPath(id), { method: "GET" }), detailOf);
    },
  },
  {
    method: "DELETE",
    path: /^ai\/sessions\/([^/]+)$/,
    json: false,
    async handle(ctx, _body, [id]) {
      if (!validId(id)) return validationError();
      return noContent(ctx, sessionPath(id), { method: "DELETE" });
    },
  },
  {
    method: "PUT",
    path: /^ai\/sessions\/([^/]+)\/messages\/([^/]+)\/rating$/,
    json: true,
    async handle(ctx, body, [id, messageId]) {
      const rating = body.rating;
      if (!validId(id) || !validId(messageId) || (rating !== null && rating !== "up" && rating !== "down")) return validationError();
      const hasComment = typeof body.comment === "string" && body.comment.trim() !== "";
      const comment = hasComment ? text(body, "comment", MAX_RATING_COMMENT_CHARS) : null;
      if (hasComment && comment === null) return validationError();
      const payload: Body = comment === null || rating === null ? { rating } : { rating, comment };
      return noContent(ctx, sessionPath(id, `/messages/${encodeURIComponent(messageId)}/rating`), jsonInit("PUT", payload));
    },
  },
  {
    method: "POST",
    path: /^ai\/sessions\/([^/]+)\/messages$/,
    json: true,
    maxBody: ASK_BODY_BYTES,
    async handle(ctx, body, [id]) {
      const question = text(body, "text", MAX_ASK_CHARS);
      const images = askImages(body);
      const locale = askLocale(body);
      if (!validId(id) || question === null || images === null || locale === null) return validationError();
      const payload: Body = images.length > 0 ? { text: question, images } : { text: question };
      if (locale !== undefined) payload.locale = locale;
      const res = await callAuthedStream(ctx, sessionPath(id, "/messages"), jsonInit("POST", payload));
      if (!res.ok) return failureResponse(res);
      return new Response(res.body.pipeThrough(aiEventFilter()), { status: 200, headers: SSE_HEADERS });
    },
  },
  {
    method: "POST",
    path: /^ai\/sessions\/([^/]+)\/cancel$/,
    json: false,
    async handle(ctx, _body, [id]) {
      if (!validId(id)) return validationError();
      return noContent(ctx, sessionPath(id, "/cancel"), { method: "POST" });
    },
  },
  {
    method: "POST",
    path: /^ai\/issues\/([^/]+)\/confirm$/,
    json: true,
    maxBody: LARGE_BODY_BYTES,
    async handle(ctx, body, [id]) {
      if (!validId(id)) return validationError();
      const payload: Body = {};
      if (body.title !== undefined) {
        const title = text(body, "title", MAX_ISSUE_TITLE_CHARS);
        if (title === null) return validationError();
        payload.title = title;
      }
      if (body.body !== undefined) {
        const issueBody = text(body, "body", MAX_ISSUE_BODY_CHARS, false);
        if (issueBody === null) return validationError();
        payload.body = issueBody;
      }
      return mapped(await callAuthed(ctx, issuePath(id, "confirm"), jsonInit("POST", payload)), issueOf);
    },
  },
  {
    method: "POST",
    path: /^ai\/issues\/([^/]+)\/discard$/,
    json: false,
    async handle(ctx, _body, [id]) {
      if (!validId(id)) return validationError();
      return mapped(await callAuthed(ctx, issuePath(id, "discard"), { method: "POST" }), issueOf);
    },
  },
  {
    method: "GET",
    path: /^ai\/attachments\/([^/]+)$/,
    json: false,
    simpleGet: true,
    async handle(ctx, _body, [id]) {
      if (!validId(id)) return validationError();
      const res = await callAuthedStream(ctx, `/api/v1/ai/attachments/${encodeURIComponent(id)}`, { method: "GET" }, "image");
      if (!res.ok) return failureResponse(res);
      return new Response(res.body, {
        status: 200,
        headers: {
          "Content-Type": res.contentType,
          // 图片按 id 不可变；只给本人浏览器缓存
          "Cache-Control": "private, max-age=86400, immutable",
          "X-Content-Type-Options": "nosniff",
          "Content-Security-Policy": "default-src 'none'; sandbox",
          "X-Robots-Tag": NOINDEX_ROBOTS,
        },
      });
    },
  },
];
