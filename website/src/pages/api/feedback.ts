/**
 * POST /api/feedback
 *
 * 接收用户反馈/功能建议，代理创建 GitHub Issue 到私有仓库。
 * 服务端持有 GITHUB_TOKEN，前端无需暴露凭据。
 *
 * 请求体:
 * {
 *   type: "feature" | "bug" | "other" | "docs",
 *   title: string,
 *   description: string,
 *   contact?: string     // 可选的联系方式（邮箱等）
 *   pagePath?: string    // 可选，docs 反馈关联的页面路径（需以 /docs/ 开头且 ≤200 字符，校验失败则静默忽略）
 *   logs?: string        // 可选，客户端日志（脱敏后），独立折叠展示，上限 30000 字符（超限截断保留末尾）
 *   appVersion: string   // 应用版本号（≤50 字符）；除 docs 反馈外必填（App 端自动注入，网站表单手动填写）
 * }
 *
 * 防滥用:
 * - 基于 IP 的简易速率限制（每 IP 每分钟最多 3 次）
 * - 内容长度限制
 */

import type { APIRoute } from "astro";
import { GITHUB_TOKEN, GITHUB_REPO } from "astro:env/server";

export const prerender = false;

// ---------- Rate Limit（内存，Vercel Serverless 冷启动后重置） ----------

const rateLimitMap = new Map<string, { count: number; resetAt: number }>();
const RATE_LIMIT_WINDOW = 60_000; // 1 分钟
const RATE_LIMIT_MAX = 3; // 每窗口最多 3 次

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

// 定期清理过期条目（防内存泄漏）
setInterval(() => {
  const now = Date.now();
  for (const [ip, entry] of rateLimitMap) {
    if (now > entry.resetAt) rateLimitMap.delete(ip);
  }
}, 5 * 60_000);

// ---------- 类型映射 ----------

const TYPE_LABELS: Record<string, string> = {
  feature: "enhancement",
  bug: "bug",
  other: "feedback",
  docs: "docs",
};

const TYPE_EMOJI: Record<string, string> = {
  feature: "\u2728", // ✨
  bug: "\uD83D\uDC1B", // 🐛
  other: "\uD83D\uDCAC", // 💬
  docs: "\uD83D\uDCD6", // 📖
};

// ---------- Handler ----------

export const POST: APIRoute = async ({ request, clientAddress }) => {
  // Astro SSR 的 clientAddress 由适配器（Vercel）从底层正确解析
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

  // 解析请求体
  let body: {
    type?: string;
    title?: string;
    description?: string;
    contact?: string;
    pagePath?: string;
    logs?: string;
    source?: string; // "website"（默认）| "app"，决定来源标签与 body 模板措辞
    appVersion?: string; // 应用版本号，除 docs 类型外必填
  };

  try {
    body = await request.json();
  } catch {
    return new Response(JSON.stringify({ error: "Invalid JSON body" }), {
      status: 400,
      headers: { "Content-Type": "application/json" },
    });
  }

  const { type, title, description, contact, pagePath, logs, source, appVersion } = body;

  // 验证必填字段
  if (!type || !title || !description) {
    return new Response(
      JSON.stringify({
        error: "Missing required fields: type, title, description",
      }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  // 验证 type 取值
  if (!["feature", "bug", "other", "docs"].includes(type)) {
    return new Response(
      JSON.stringify({
        error: "Invalid type. Must be: feature, bug, other, or docs",
      }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  // 内容长度限制
  if (title.length > 200) {
    return new Response(
      JSON.stringify({ error: "Title too long (max 200 characters)" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  if (description.length > 5000) {
    return new Response(
      JSON.stringify({ error: "Description too long (max 5000 characters)" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  // 版本号：除 docs 反馈外必填（App 端构建时注入自动上报，网站表单由用户填写）。
  const safeAppVersion =
    typeof appVersion === "string" && appVersion.trim().length > 0
      ? appVersion.trim().slice(0, 50)
      : undefined;

  if (type !== "docs" && !safeAppVersion) {
    return new Response(
      JSON.stringify({ error: "Missing required field: appVersion" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  // 可选字段：pagePath（docs 反馈关联的页面路径）。校验失败静默忽略而非 400，
  // 因为它只是补充上下文，不应阻塞用户提交反馈本身。
  const safePagePath =
    typeof pagePath === "string" &&
    pagePath.startsWith("/docs/") &&
    pagePath.length <= 200
      ? pagePath
      : undefined;

  // 可选字段：logs（客户端日志）。独立字段而非塞进 description，避免挤占
  // 用户描述配额；上限 30000 字符（GitHub Issue body 上限 65536，留足余量），
  // 超限则截断保留末尾（最新日志），校验失败静默忽略不阻塞提交。
  const safeLogs =
    typeof logs === "string" && logs.trim().length > 0
      ? logs.trim().slice(-30000)
      : undefined;

  // 构造 Issue 内容。按 type 生成区分 bug / feature 的结构化标题，按 source
  // 区分来源（Website 表单 vs 桌面应用内反馈），metadata 尾部保留
  // **Type:** / **Source:** / **Submitted:** 字面量以兼容 issues API 的 parseFeedbackBody。
  const emoji = TYPE_EMOJI[type] || "\uD83D\uDCAC";
  const label = TYPE_LABELS[type] || "feedback";
  const isApp = source === "app";

  // 来源标记：标题前缀 + metadata 的 **Source:** 值（后者必须保留供 parser 门禁识别）。
  const sourceTag = isApp ? "App" : "Website";
  const versionSuffix = safeAppVersion ? ` (${safeAppVersion})` : "";
  const sourceMeta = isApp
    ? `Desktop App${versionSuffix}`
    : `Website feedback form${versionSuffix}`;

  // 正文标题：bug / feature / docs / other 各自不同措辞。
  const heading =
    type === "feature"
      ? "Feature Request"
      : type === "bug"
        ? "Bug Report"
        : type === "docs"
          ? "Documentation Feedback"
          : "Feedback";

  const issueTitle = `${emoji} [${sourceTag} Feedback] ${title}`;
  const issueBody = [
    `## ${heading}`,
    "",
    // bug 与 feature 各自补一个语义化子标题，让内容在详情页分区展示。
    type === "bug"
      ? "### 问题描述 / Description"
      : type === "feature"
        ? "### 建议内容 / Proposal"
        : null,
    type === "bug" || type === "feature" ? "" : null,
    description,
    "",
    "---",
    "",
    `**Type:** ${type}`,
    safePagePath ? `**页面**: ${safePagePath}` : null,
    contact ? `**Contact:** ${contact}` : null,
    `**Source:** ${sourceMeta}`,
    `**Submitted:** ${new Date().toISOString()}`,
    `**IP:** \`${ip}\``,
    safeLogs
      ? `\n<details>\n<summary>Client Logs</summary>\n\n\`\`\`log\n${safeLogs}\n\`\`\`\n</details>`
      : null,
  ]
    .filter((line) => line !== null)
    .join("\n");

  try {
    const res = await fetch(
      `https://api.github.com/repos/${GITHUB_REPO}/issues`,
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${GITHUB_TOKEN}`,
          Accept: "application/vnd.github+json",
          "X-GitHub-Api-Version": "2022-11-28",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          title: issueTitle,
          body: issueBody,
          labels: ["user-feedback", label],
        }),
      },
    );

    if (!res.ok) {
      const text = await res.text();
      console.error(`GitHub API error: ${res.status}`, text);
      return new Response(
        JSON.stringify({ error: "Failed to submit feedback" }),
        { status: 502, headers: { "Content-Type": "application/json" } },
      );
    }

    const issue = await res.json();

    return new Response(
      JSON.stringify({
        success: true,
        message: "Feedback submitted successfully",
        issueNumber: issue.number,
      }),
      {
        status: 201,
        headers: { "Content-Type": "application/json" },
      },
    );
  } catch (err) {
    console.error("Failed to create GitHub issue:", err);
    return new Response(JSON.stringify({ error: "Internal server error" }), {
      status: 500,
      headers: { "Content-Type": "application/json" },
    });
  }
};
