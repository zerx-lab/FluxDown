/**
 * FluxCloud 同源代理共用小件：base 解析 + JSON 透传转发 + 带失败映射的服务端调用。
 *
 * 透传上游状态码与 JSON 错误体（前端按 code 映射文案）。转发时附带访客真实 IP
 * （X-Forwarded-For），FluxCloud 侧需将本站出口 IP 加入 FLUXCLOUD_TRUSTED_PROXIES
 * 才能按访客限频，否则全站共享一个限频桶。
 *
 * `callUpstream`：失败原样透传状态码与 `code`；上游不可达 / 未配置 / 返回畸形体统一为 502 + 稳定 code。
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

  let body: unknown;
  try {
    body = await upstream.json();
  } catch {
    body = undefined;
  }

  if (!upstream.ok) {
    const code = isRecord(body) && typeof body.code === "string" && body.code !== "" ? body.code : "upstream_error";
    const message = isRecord(body) && typeof body.message === "string" ? body.message : undefined;
    const retryAfter = upstream.headers.get("retry-after") ?? undefined;
    return failure(upstream.status, code, { message, retryAfter });
  }
  if (body === undefined && expectBody) return failure(502, "upstream_invalid");
  return { ok: true, body };
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
