/**
 * AI 助手 BFF 路由：`/api/account/ai/<path>` → FluxCloud `/api/v1/ai/*`（契约见 FluxCloud README「AI 助手」）。
 * 挂在账户 BFF 下，复用会话 Cookie（Path `/api/account`）、刷新与 CSRF 闸。
 *
 * - 会话一律以 `channel: "website"` 创建，不声明客户端工具（官网没有本机状态可查）。
 * - 会话详情只回用户 / 助手消息（工具调用与结果不下发），用户消息带图片 id，图片经 `ai/attachments/{id}` 取。
 * - 提问的 SSE 按 `ai-stream.ts` 白名单逐帧裁剪后转发；访客断开即中止上游，FluxCloud 随之取消本次执行。
 * - 提问可带图片（`data:image/(png|jpeg|gif|webp);base64,...`，张数 / 单张大小同 FluxCloud `ai::attachments`）；
 *   BFF 只做形状与长度预检，格式识别与解码后大小由 FluxCloud 判定。
 */
import { failureResponse, NOINDEX_ROBOTS, proxyNoContent } from "@/lib/cloud-proxy";
import { isRecord, pick } from "@/lib/is-record";
import { AI_ISSUE_KEYS, aiEventFilter } from "./ai-stream";
import { jsonInit, mapped, ROW_ID_RE, text, validationError, type Body, type Route } from "./route-kit";
import { callAuthed, callAuthedStream, type AccountContext } from "./session";

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
  "available", "reason", "maxMessageChars", "dailyLimit", "usedToday", "imagesSupported", "maxImages", "maxImageBytes",
] as const;
const SESSION_KEYS = [
  "id", "channel", "kind", "title", "messageCount", "rating", "active", "createdAt", "updatedAt",
] as const;
const MESSAGE_KEYS = ["id", "runId", "seq", "role", "content", "createdAt"] as const;
const SHOWN_ROLES: Record<string, true> = { user: true, assistant: true };

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

function detailOf(body: unknown): Body | null {
  if (!isRecord(body)) return null;
  const session = sessionOf(body.session);
  const shown = Array.isArray(body.messages)
    ? body.messages.filter((m) => isRecord(m) && typeof m.role === "string" && Object.hasOwn(SHOWN_ROLES, m.role))
    : null;
  const messages = all(shown, messageOf);
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
      const locale = body.locale === undefined ? null : text(body, "locale", 35);
      if (body.locale !== undefined && (locale === null || !LOCALE_RE.test(locale))) return validationError();
      const client: Body = { clientTools: [] };
      if (locale !== null) client.locale = locale;
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
    path: /^ai\/sessions\/([^/]+)\/rating$/,
    json: true,
    async handle(ctx, body, [id]) {
      const rating = body.rating;
      if (!validId(id) || (rating !== null && rating !== "up" && rating !== "down")) return validationError();
      const hasComment = typeof body.comment === "string" && body.comment.trim() !== "";
      const comment = hasComment ? text(body, "comment", MAX_RATING_COMMENT_CHARS) : null;
      if (hasComment && comment === null) return validationError();
      const payload: Body = comment === null || rating === null ? { rating } : { rating, comment };
      return noContent(ctx, sessionPath(id, "/rating"), jsonInit("PUT", payload));
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
      if (!validId(id) || question === null || images === null) return validationError();
      const payload: Body = images.length > 0 ? { text: question, images } : { text: question };
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
