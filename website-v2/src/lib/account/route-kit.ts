/**
 * 账户 BFF 路由的共用小件：路由形状、请求体字段校验与上游响应映射（路由表见 `routes.ts` / `ai-routes.ts`）。
 */
import { failure, failureResponse, proxyJson } from "@/lib/cloud-proxy";
import type { AccountContext, UpstreamResult } from "./session";

export type Body = Record<string, unknown>;

export interface Route {
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  path: RegExp;
  /** 是否要求 JSON 请求体。 */
  json: boolean;
  /** JSON 请求体上限（字节），缺省 `DEFAULT_MAX_BODY_BYTES`。 */
  maxBody?: number;
  /**
   * 允许不带自定义头的 GET（`<img src>` 这类浏览器直接发起、无法加头的只读请求）。仍要求 `Sec-Fetch-Site` 同源；
   * 会话 Cookie 为 SameSite=Strict，跨站请求本就不带凭据。只用于无副作用的读取。
   */
  simpleGet?: boolean;
  handle(ctx: AccountContext, body: Body, params: string[], query: URLSearchParams): Promise<Response>;
}

export const DEFAULT_MAX_BODY_BYTES = 16 * 1024;

/** 路径里的行 id（设备、AI 会话、问题单）：只接受 FluxCloud 生成的 id 字符集。 */
export const ROW_ID_RE = /^[A-Za-z0-9-]{1,64}$/;

export const invalidUpstream = () => failureResponse(failure(502, "upstream_invalid"));
export const validationError = () => failureResponse(failure(400, "validation_error"));

/** 字符串字段：trim 后非空且不超过 `max` 字符；否则 null。密码不 trim。 */
export function text(body: Body, key: string, max: number, trim = true): string | null {
  const raw = body[key];
  if (typeof raw !== "string") return null;
  const value = trim ? raw.trim() : raw;
  return value !== "" && [...value].length <= max ? value : null;
}

export const jsonInit = (method: string, payload?: Body) => ({
  method,
  headers: payload === undefined ? undefined : { "Content-Type": "application/json" },
  body: payload === undefined ? undefined : JSON.stringify(payload),
});

/** 成功时按 `map` 裁剪响应体（返回 null = 上游形状不对 → 502）；失败透传。 */
export function mapped(res: UpstreamResult, map: (body: unknown) => unknown): Response {
  if (!res.ok) return failureResponse(res);
  const out = map(res.body);
  return out === null ? invalidUpstream() : proxyJson(out);
}
