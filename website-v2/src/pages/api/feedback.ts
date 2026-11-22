/**
 * GET /api/feedback?pluginIdentity=... resolves an authorized public plugin destination.
 * POST /api/feedback submits either generic private feedback or public plugin feedback.
 * The public plugin route never uses the private repository's token.
 *
 * 请求体:
 * {
 *   type: "feature" | "bug" | "other" | "docs",
 *   title: string,
 *   description?: string // 详细描述；feature 类型可选，其余类型必填
 *   contact?: string     // 可选的联系方式（邮箱等）
 *   pagePath?: string    // 可选，docs 反馈关联的页面路径（需以 /docs/ 开头且 ≤200 字符，校验失败则静默忽略）
 *   logs?: string        // 可选，客户端日志（脱敏后），独立折叠展示，上限 30000 字符（超限截断保留末尾）
 *   appVersion?: string  // 应用版本号（≤50 字符）；bug/other 必填，feature/docs 可选（App 端自动注入，网站表单手动填写）
 *   environment?: string // 可选，客户端环境摘要（OS/浏览器/语言等，客户端自动检测并在表单中明示；单行 ≤300 字符）
 *   pluginIdentity?: string // optional, official registry identity or its former ID
 *   pluginVersion?: string  // optional, public plugin feedback only
 *   repository?: string     // required with pluginIdentity, confirmed destination, never used for routing
 * }
 *
 * 防滥用:
 * - Private feedback: 3 requests per IP/minute.
 * - Plugin feedback: separate lookup limits and per-destination/global write quotas.
 */

import type { APIRoute } from "astro";
import { GITHUB_TOKEN, GITHUB_REPO, PLUGIN_FEEDBACK_APP_ID, PLUGIN_FEEDBACK_PRIVATE_KEY_BASE64 } from "astro:env/server";
import { getClientIp } from "@/lib/client-ip";
import {
  createPluginFeedback, isFeedbackRepository, isPluginIdentity, PluginFeedbackFailure,
  publicPluginIssue, readFeedbackJson,
} from "@/lib/plugin-feedback";

export const prerender = false;

const pluginFeedback = createPluginFeedback(PLUGIN_FEEDBACK_APP_ID, PLUGIN_FEEDBACK_PRIVATE_KEY_BASE64);
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const PRIVATE_BODY_MAX = 128 * 1024; // 30,000 characters of optional private logs plus metadata
const PLUGIN_BODY_MAX = 32 * 1024;

interface Quota {
  start: number;
  total: number;
  keys: Map<string, number>;
}

const feedbackPosts: Quota = { start: 0, total: 0, keys: new Map() };
const pluginLookups: Quota = { start: 0, total: 0, keys: new Map() };
const pluginWrites: Quota = { start: 0, total: 0, keys: new Map() };

function takeQuota(quota: Quota, key: string, duration: number, perKey: number, overall: number): boolean {
  const start = Math.floor(Date.now() / duration) * duration;
  if (quota.start !== start) {
    quota.start = start;
    quota.total = 0;
    quota.keys.clear();
  }
  const count = quota.keys.get(key) ?? 0;
  if (quota.total >= overall || count >= perKey) return false;
  quota.total++;
  quota.keys.set(key, count + 1);
  return true;
}

const PLUGIN_CORS = { "Access-Control-Allow-Origin": "*" };

function feedbackError(status: number, error: string, pluginCors = false): Response {
  return Response.json({ error }, {
    status,
    headers: { "Cache-Control": "no-store", ...(pluginCors ? PLUGIN_CORS : {}) },
  });
}

function pluginFailure(error: unknown, cors: boolean): Response {
  if (error instanceof PluginFeedbackFailure) return feedbackError(error.status, error.message, cors);
  // Upstream errors may carry secrets or submitted material; never log raw responses or exceptions.
  console.error("Plugin feedback upstream request failed");
  return feedbackError(502, "Failed to submit or resolve plugin feedback", cors);
}

export const GET: APIRoute = async ({ url, request, clientAddress }) => {
  const identities = url.searchParams.getAll("pluginIdentity");
  if (!identities.length) return feedbackError(404, "Plugin feedback is unavailable");
  if (identities.length !== 1 || !isPluginIdentity(identities[0])) {
    return feedbackError(400, "Invalid plugin identity", true);
  }
  if (!takeQuota(pluginLookups, getClientIp(request, clientAddress), MINUTE, 10, 60)) {
    return feedbackError(429, "Too many requests. Please try again later.", true);
  }
  try {
    return Response.json({ repository: await pluginFeedback.destination(identities[0]) }, {
      headers: { "Cache-Control": "no-store", ...PLUGIN_CORS },
    });
  } catch (error) {
    return pluginFailure(error, true);
  }
};

export const OPTIONS: APIRoute = ({ url, request }) => {
  const identities = url.searchParams.getAll("pluginIdentity");
  const method = request.headers.get("Access-Control-Request-Method");
  const headers = request.headers.get("Access-Control-Request-Headers");
  if (identities.length !== 1 || !isPluginIdentity(identities[0]) ||
      !request.headers.has("Origin") || (method !== "GET" && method !== "POST") ||
      (headers !== null && headers.trim().toLowerCase() !== "content-type")) {
    return new Response(null, { status: 404 });
  }
  return new Response(null, {
    status: 204,
    headers: {
      ...PLUGIN_CORS,
      "Access-Control-Allow-Methods": "GET, POST",
      "Access-Control-Allow-Headers": "Content-Type",
      "Access-Control-Max-Age": "600",
    },
  });
};

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

export const POST: APIRoute = async ({ request, url, clientAddress }) => {
  const ip = getClientIp(request, clientAddress);
  const identities = url.searchParams.getAll("pluginIdentity");
  const pluginCors = identities.length > 0;
  const queryIdentity = identities[0];
  if (pluginCors && (identities.length !== 1 || !isPluginIdentity(queryIdentity))) {
    return feedbackError(400, "Invalid plugin identity", true);
  }
  // Only the explicit public route gets CORS. Requiring JSON also prevents a browser
  // from writing to the private route with a cross-origin simple POST.
  if (request.headers.get("Content-Type")?.split(";", 1)[0].trim().toLowerCase() !== "application/json") {
    return feedbackError(415, "Expected application/json", pluginCors);
  }
  if (!takeQuota(feedbackPosts, ip, MINUTE, 3, 10_000)) {
    return feedbackError(429, "Too many requests. Please try again later.", pluginCors);
  }

  let body: unknown;
  try {
    body = await readFeedbackJson(request, pluginCors ? PLUGIN_BODY_MAX : PRIVATE_BODY_MAX);
  } catch (error) {
    return error instanceof RangeError
      ? feedbackError(413, "Feedback body too large", pluginCors)
      : feedbackError(400, "Invalid JSON body", pluginCors);
  }
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    return feedbackError(400, "Invalid JSON body", pluginCors);
  }
  const { type, title, description, contact, pagePath, logs, source, appVersion, environment,
    pluginIdentity, pluginVersion, repository } = body as Record<string, unknown>;
  const isPlugin = pluginCors || pluginIdentity !== undefined || pluginVersion !== undefined || repository !== undefined;

  if (typeof type !== "string" || !["feature", "bug", "other", "docs"].includes(type) ||
      typeof title !== "string" || !title.trim() ||
      (description !== undefined && typeof description !== "string") ||
      (type !== "feature" && !description?.trim())) {
    return feedbackError(400, "Invalid feedback type, title or description", pluginCors);
  }

  if (isPlugin) {
    if (!isPluginIdentity(pluginIdentity) || !isFeedbackRepository(repository) ||
        (pluginCors && queryIdentity !== pluginIdentity)) {
      return feedbackError(400, "Expected a plugin identity and confirmed repository", pluginCors);
    }
    if (title.length > 200 || (description?.length ?? 0) > 5000 ||
        (appVersion !== undefined && (typeof appVersion !== "string" || appVersion.length > 50)) ||
        (pluginVersion !== undefined && (typeof pluginVersion !== "string" || pluginVersion.length > 64)) ||
        (environment !== undefined && (typeof environment !== "string" || environment.length > 300))) {
      return feedbackError(400, "Invalid plugin feedback fields", pluginCors);
    }
    const issue = publicPluginIssue({
      identity: pluginIdentity, type, title, description: description ?? "",
      appVersion: appVersion?.trim().replace(/\s+/g, " "),
      pluginVersion: pluginVersion?.trim().replace(/\s+/g, " "),
      environment: environment?.trim().replace(/\s+/g, " "),
    });
    if (!issue) return feedbackError(400, "Empty public plugin feedback", pluginCors);
    if (!takeQuota(pluginLookups, ip, MINUTE, 10, 60)) {
      return feedbackError(429, "Too many requests. Please try again later.", pluginCors);
    }
    try {
      const issueNumber = await pluginFeedback.submit(pluginIdentity, repository, issue, (authorizedRepository) =>
        takeQuota(pluginWrites, authorizedRepository.toLowerCase(), HOUR, 6, 40));
      return Response.json({ issueNumber }, {
        status: 201, headers: { "Cache-Control": "no-store", ...(pluginCors ? PLUGIN_CORS : {}) },
      });
    } catch (error) {
      return pluginFailure(error, pluginCors);
    }
  }

  if (!GITHUB_TOKEN) return feedbackError(500, "Server misconfigured: missing GITHUB_TOKEN");

  // 内容长度限制
  if (title.length > 200) {
    return new Response(
      JSON.stringify({ error: "Title too long (max 200 characters)" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  const safeDescription =
    typeof description === "string" ? description.trim() : "";

  if (safeDescription.length > 5000) {
    return new Response(
      JSON.stringify({ error: "Description too long (max 5000 characters)" }),
      { status: 400, headers: { "Content-Type": "application/json" } },
    );
  }

  // 版本号：bug / other 必填（App 端构建时注入自动上报，网站表单由用户填写）；
  // feature 与 docs 反馈可选 —— 功能建议与具体版本通常无关。
  const safeAppVersion =
    typeof appVersion === "string" && appVersion.trim().length > 0
      ? appVersion.trim().slice(0, 50)
      : undefined;

  if (type !== "docs" && type !== "feature" && !safeAppVersion) {
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

  // 可选字段：environment（客户端自动检测的系统/浏览器/语言摘要）。
  // 压成单行并限长，防止注入 metadata 区破坏 parseFeedbackBody；校验失败静默忽略。
  const safeEnvironment =
    typeof environment === "string" && environment.trim().length > 0
      ? environment.trim().replace(/\s+/g, " ").slice(0, 300)
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
    safeDescription || "_(no description)_",
    "",
    "---",
    "",
    `**Type:** ${type}`,
    safePagePath ? `**页面**: ${safePagePath}` : null,
    typeof contact === "string" && contact ? `**Contact:** ${contact}` : null,
    safeEnvironment ? `**Environment:** ${safeEnvironment}` : null,
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
