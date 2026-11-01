/**
 * POST /api/cloud/bt-guard/appeal — BT 防吸血申诉（同源代理）。
 *
 * 请求体 `{ detail, contact?, turnstileToken? }`：detail 必填 ≤ 2000，contact 为邮箱 ≤ 200，
 * turnstileToken 来自页面上的 Turnstile 小部件（自查响应带 turnstileSiteKey 时）。校验后转发 FluxCloud
 * `POST /api/v1/bt-guard/public/appeal`（只能为访客来源 IP 所在组申诉），成功 204；失败透传状态码与
 * `{ code, message }`（404 not_listed / 409 appeal_pending / 403|400 turnstile_* / 429）。
 * 响应 no-store + `X-Robots-Tag: noindex`。
 */

import type { APIRoute } from "astro";
import { MAX_APPEAL_BODY, parseAppealRequest, readBodyCapped } from "@/lib/bt-guard";
import { submitAppeal } from "@/lib/bt-guard-proxy";
import { getClientIp } from "@/lib/client-ip";
import { failureResponse, proxyJson, proxyNoContent } from "@/lib/cloud-proxy";

export const prerender = false;

export const POST: APIRoute = async ({ request, clientAddress }) => {
  // 不信任 content-length：chunked / 谎报长度的请求同样在超过上限时中止读取。
  const text = await readBodyCapped(request, MAX_APPEAL_BODY);
  if (text === null) return proxyJson({ code: "payload_too_large" }, 413);
  const parsed = parseAppealRequest(text);
  if (!parsed.ok) return proxyJson({ code: "validation_error" }, 422);

  const result = await submitAppeal(parsed, getClientIp(request, clientAddress));
  return result.ok ? proxyNoContent() : failureResponse(result);
};
