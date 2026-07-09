import type { APIRoute } from "astro";

export const prerender = false;

/**
 * POST /api/logo-submit
 *
 * Logo 征集投票已结束，端点仅保留兼容响应（前端 LogoVotePage 仍会调用）。
 * 原 R2 图片上传 + GitHub Issue 创建逻辑已随 R2 存储下线一并移除。
 */
export const POST: APIRoute = async () => {
  return new Response(
    JSON.stringify({
      error: "Logo voting has ended. Submissions are no longer accepted.",
    }),
    { status: 403, headers: { "Content-Type": "application/json" } },
  );
};
