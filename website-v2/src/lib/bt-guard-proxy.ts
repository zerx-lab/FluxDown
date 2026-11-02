/**
 * BT 防吸血自查 / 申诉的 FluxCloud 服务端代理核心（`/api/cloud/bt-guard/*` 共用，浏览器只连本站）。
 * 上游契约见 `bt-guard.ts`；传输、IP 透传（X-Forwarded-For）与失败映射沿用 `cloud-proxy.ts::callUpstream`。
 */
import { type AppealRequest, type BtGuardSelf, parseSelf } from "./bt-guard";
import { callUpstream, failure, type ProxyFailure } from "./cloud-proxy";

export type BtGuardSelfResult = { ok: true; self: BtGuardSelf } | ProxyFailure;

/** 查询访客来源 IP 所在组是否在已发布名单。 */
export async function lookupSelf(ip: string): Promise<BtGuardSelfResult> {
  const result = await callUpstream("/api/v1/bt-guard/public/self", { method: "GET" }, ip);
  if (!result.ok) return result;
  const self = parseSelf(result.body);
  return self ? { ok: true, self } : failure(502, "upstream_invalid");
}

/** 提交申诉；上游成功为 204 无正文。入参由调用方先经 `parseAppealRequest` 校验。 */
export async function submitAppeal(
  appeal: Extract<AppealRequest, { ok: true }>,
  ip: string,
): Promise<{ ok: true } | ProxyFailure> {
  const { detail, contact, turnstileToken } = appeal;
  const result = await callUpstream(
    "/api/v1/bt-guard/public/appeal",
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ detail, contact, turnstileToken }),
    },
    ip,
    false,
  );
  return result.ok ? { ok: true } : result;
}
