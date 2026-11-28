/**
 * `/api/account/*` — 官网账户中心 BFF（同源代理 FluxCloud 账户接口，令牌只存 HttpOnly Cookie）。
 * 路由白名单、CSRF 闸与字段裁剪见 `src/lib/account/routes.ts`，会话与刷新见 `src/lib/account/session.ts`。
 */
import type { APIRoute } from "astro";
import { handleAccountRequest } from "@/lib/account/routes";
import { getClientIp } from "@/lib/client-ip";

export const prerender = false;

export const ALL: APIRoute = ({ request, params, cookies, clientAddress }) =>
  handleAccountRequest(request, (params.path ?? "").replace(/\/+$/, ""), cookies, getClientIp(request, clientAddress));
