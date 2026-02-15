/**
 * POST /api/issues/:number/comments
 *
 * 代理用户回复到 GitHub Issue 评论。
 * 服务端持有 GITHUB_TOKEN，前端无需暴露凭据。
 *
 * 请求体:
 * {
 *   body: string        // 回复内容（必填，1–2000 字符）
 * }
 *
 * 防滥用:
 * - 基于 IP 的速率限制（每 IP 每 2 分钟最多 3 次评论）
 * - 内容长度限制
 */

import type { APIRoute } from "astro";

export const prerender = false;

const GITHUB_REPO = import.meta.env.GITHUB_REPO || "user/x_down";
const GITHUB_TOKEN = import.meta.env.GITHUB_TOKEN || "";

// ── Rate Limit ──

const rateLimitMap = new Map<string, { count: number; resetAt: number }>();
const RATE_LIMIT_WINDOW = 120_000; // 2 分钟
const RATE_LIMIT_MAX = 3;

function isRateLimited(ip: string): boolean {
  const now = Date.now();
  const entry = rateLimitMap.get(ip);

  if (!entry || now > entry.resetAt) {
    rateLimitMap.set(ip, { count: 1, resetAt: now + RATE_LIMIT_WINDOW });
    return false;
  }

  entry.count += 1;
  return entry.count > RATE_LIMIT_MAX;
}

// 定期清理过期条目
setInterval(() => {
  const now = Date.now();
  for (const [ip, entry] of rateLimitMap) {
    if (now > entry.resetAt) rateLimitMap.delete(ip);
  }
}, 5 * 60_000);

// ── Handler ──

export const POST: APIRoute = async ({ params, request, clientAddress }) => {
  const ip = clientAddress || "unknown";

  // 速率限制
  if (isRateLimited(ip)) {
    return new Response(
      JSON.stringify({ error: "Too many requests. Please try again later." }),
      { status: 429, headers: { "Content-Type": "application/json" } },
    );
  }

  // Token 检查
  if (!GITHUB_TOKEN) {
    return new Response(
      JSON.stringify({ error: "Server misconfigured: missing GITHUB_TOKEN" }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }

  // 验证 issue number
  const numberStr = params.number;
  if (!numberStr) {
    return new Response(
      JSON.stringify({ error: "Missing issue number" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  const issueNumber = parseInt(numberStr, 10);
  if (isNaN(issueNumber) || issueNumber <= 0) {
    return new Response(
      JSON.stringify({ error: "Invalid issue number" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  // 解析请求体
  let payload: { body?: string };

  try {
    payload = await request.json();
  } catch {
    return new Response(
      JSON.stringify({ error: "Invalid JSON body" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  const { body } = payload;

  // 验证内容
  if (!body || typeof body !== "string" || !body.trim()) {
    return new Response(
      JSON.stringify({ error: "Missing required field: body" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  const trimmedBody = body.trim();

  if (trimmedBody.length > 2000) {
    return new Response(
      JSON.stringify({ error: "Reply too long (max 2000 characters)" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  // 构造评论内容（带元数据标记，方便在 GitHub 侧识别来源）
  const commentBody = [
    "> \uD83D\uDCAC Website visitor reply",
    "",
    trimmedBody,
    "",
    "---",
    "",
    `**Source:** Website reply`,
    `**Time:** ${new Date().toISOString()}`,
  ].join("\n");

  try {
    const res = await fetch(
      `https://api.github.com/repos/${GITHUB_REPO}/issues/${issueNumber}/comments`,
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${GITHUB_TOKEN}`,
          Accept: "application/vnd.github+json",
          "X-GitHub-Api-Version": "2022-11-28",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ body: commentBody }),
      },
    );

    if (res.status === 404) {
      return new Response(
        JSON.stringify({ error: "Issue not found" }),
        { status: 404, headers: { "Content-Type": "application/json" } },
      );
    }

    if (!res.ok) {
      const text = await res.text();
      console.error(`GitHub API error: ${res.status}`, text);
      return new Response(
        JSON.stringify({ error: "Failed to submit reply" }),
        { status: 502, headers: { "Content-Type": "application/json" } },
      );
    }

    const comment = await res.json();

    return new Response(
      JSON.stringify({
        success: true,
        message: "Reply submitted successfully",
        commentId: comment.id,
      }),
      {
        status: 201,
        headers: { "Content-Type": "application/json" },
      },
    );
  } catch (err) {
    console.error("Failed to create comment:", err);
    return new Response(
      JSON.stringify({ error: "Internal server error" }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }
};
