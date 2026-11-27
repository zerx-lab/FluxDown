/**
 * FluxCloud 同源代理共用小件：base 解析 + JSON 透传转发 + 带失败映射的服务端调用。
 *
 * 透传上游状态码与 JSON 错误体（前端按 code 映射文案）。转发时附带访客真实 IP
 * （X-Forwarded-For），FluxCloud 侧需将本站出口 IP 加入 FLUXCLOUD_TRUSTED_PROXIES
 * 才能按访客限频，否则全站共享一个限频桶。
 *
 * `callUpstream`：失败原样透传状态码与 `code`；上游不可达 / 未配置 / 返回畸形体统一为 502 + 稳定 code。
 * `openUpstreamStream`：SSE 流式调用，失败映射同 `callUpstream`，响应头到达后正文不设总时限。
 */

import { FLUXCLOUD_API_BASE } from "astro:env/server";
import { isRecord } from "./is-record";

export function cloudBase(): string {
  return (FLUXCLOUD_API_BASE ?? "").replace(/\/+$/, "");
}

export async function forwardJson(
  upstreamUrl: string,
  init: RequestInit,
): Promise<Response> {
  let upstream: Response;
  try {
    upstream = await fetch(upstreamUrl, {
      ...init,
      signal: AbortSignal.timeout(20_000),
    });
  } catch {
    return new Response(JSON.stringify({ code: "upstream_unreachable" }), {
      status: 502,
      headers: { "Content-Type": "application/json; charset=utf-8" },
    });
  }
  return new Response(upstream.body, {
    status: upstream.status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "no-store",
    },
  });
}

const UPSTREAM_TIMEOUT_MS = 10_000;

/** 不让搜索引擎收录代理响应。 */
export const NOINDEX_ROBOTS = "noindex, nofollow";

export interface ProxyFailure {
  ok: false;
  status: number;
  code: string;
  message?: string;
  retryAfter?: string;
}

export function failure(status: number, code: string, extra: Partial<ProxyFailure> = {}): ProxyFailure {
  return { ok: false, status, code, ...extra };
}

/** 非 2xx 上游响应 → 失败：透传状态码、`code`、`message` 与 `Retry-After`。 */
function upstreamFailure(upstream: Response, body: unknown): ProxyFailure {
  const code = isRecord(body) && typeof body.code === "string" && body.code !== "" ? body.code : "upstream_error";
  const message = isRecord(body) && typeof body.message === "string" ? body.message : undefined;
  const retryAfter = upstream.headers.get("retry-after") ?? undefined;
  return failure(upstream.status, code, { message, retryAfter });
}

async function jsonBody(upstream: Response): Promise<unknown> {
  try {
    return await upstream.json();
  } catch {
    return undefined;
  }
}

export async function callUpstream(
  path: string,
  init: { method: string; headers?: Record<string, string>; body?: string },
  ip: string,
  expectBody = true,
): Promise<{ ok: true; body: unknown } | ProxyFailure> {
  const base = cloudBase();
  if (!base) return failure(502, "not_configured");

  let upstream: Response;
  try {
    upstream = await fetch(`${base}${path}`, {
      ...init,
      headers: { Accept: "application/json", "X-Forwarded-For": ip, ...init.headers },
      signal: AbortSignal.timeout(UPSTREAM_TIMEOUT_MS),
    });
  } catch {
    return failure(502, "upstream_unreachable");
  }

  const body = await jsonBody(upstream);
  if (!upstream.ok) return upstreamFailure(upstream, body);
  if (body === undefined && expectBody) return failure(502, "upstream_invalid");
  return { ok: true, body };
}

/** 流式上游响应的期望形状：`sse` = 提问事件流，`image` = 提问图片（只认四种位图类型，排除 SVG 等可执行内容）。 */
export type UpstreamStreamKind = "sse" | "image";

const STREAM_EXPECT: Record<UpstreamStreamKind, { accept: string; type: RegExp }> = {
  sse: { accept: "text/event-stream", type: /^text\/event-stream(?:;|$)/ },
  image: { accept: "image/png, image/jpeg, image/gif, image/webp", type: /^image\/(?:png|jpeg|gif|webp)$/ },
};

/**
 * 流式调用（SSE / 图片）：响应头须在 `UPSTREAM_TIMEOUT_MS` 内到达，之后正文不设总时限（SSE 由上游的执行时限与保活兜底）。
 * `signal` 中止（访客断开）时一并中止上游请求。非 2xx 的映射同 `callUpstream`；
 * 2xx 但 Content-Type 不合 `kind` 为 502 `upstream_invalid`。成功时带回规范化（小写、去参数）的 Content-Type。
 */
export async function openUpstreamStream(
  path: string,
  init: { method: string; headers?: Record<string, string>; body?: string },
  ip: string,
  signal?: AbortSignal,
  kind: UpstreamStreamKind = "sse",
): Promise<{ ok: true; body: ReadableStream<Uint8Array>; contentType: string } | ProxyFailure> {
  const base = cloudBase();
  if (!base) return failure(502, "not_configured");
  const expect = STREAM_EXPECT[kind];

  const connect = new AbortController();
  const timer = setTimeout(() => connect.abort(), UPSTREAM_TIMEOUT_MS);
  let upstream: Response;
  try {
    upstream = await fetch(`${base}${path}`, {
      ...init,
      headers: { Accept: expect.accept, "X-Forwarded-For": ip, ...init.headers },
      signal: signal ? AbortSignal.any([connect.signal, signal]) : connect.signal,
    });
  } catch {
    return failure(502, "upstream_unreachable");
  } finally {
    clearTimeout(timer);
  }

  if (!upstream.ok) return upstreamFailure(upstream, await jsonBody(upstream));
  const raw = upstream.headers.get("content-type")?.toLowerCase() ?? "";
  if (!upstream.body || !expect.type.test(kind === "sse" ? raw : raw.split(";")[0].trim())) {
    await upstream.body?.cancel().catch(() => undefined);
    return failure(502, "upstream_invalid");
  }
  return { ok: true, body: upstream.body, contentType: raw.split(";")[0].trim() };
}

const JSON_HEADERS = {
  "Content-Type": "application/json; charset=utf-8",
  "Cache-Control": "no-store",
  "X-Robots-Tag": NOINDEX_ROBOTS,
};

export function proxyJson(body: unknown, status = 200, extra: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), { status, headers: { ...JSON_HEADERS, ...extra } });
}

/** 成功且无正文（204）。 */
export function proxyNoContent(): Response {
  return new Response(null, { status: 204, headers: { "Cache-Control": "no-store", "X-Robots-Tag": NOINDEX_ROBOTS } });
}

/** 失败 → `{ code, message? }` + 同一状态码（429 保留 Retry-After）。 */
export function failureResponse(fail: ProxyFailure): Response {
  const body = fail.message === undefined ? { code: fail.code } : { code: fail.code, message: fail.message };
  return proxyJson(body, fail.status, fail.retryAfter ? { "Retry-After": fail.retryAfter } : {});
}
