/**
 * POST /api/webhooks/github
 *
 * 接收 GitHub Webhook 的 issue_comment 事件。
 * 当开发者在带 user-feedback 标签的 Issue 上回复时，
 * 通过 SMTP 邮件通知原始反馈提交者。
 *
 * 安全：HMAC-SHA256 签名验证（X-Hub-Signature-256）。
 *
 * 环境变量:
 *   GITHUB_WEBHOOK_SECRET - Webhook 密钥
 *   SMTP_HOST             - SMTP 服务器（如 smtp.qq.com）
 *   SMTP_PORT             - 端口（如 465）
 *   SMTP_USER             - 发件邮箱
 *   SMTP_PASS             - SMTP 授权码
 */

import type { APIRoute } from "astro";
import nodemailer from "nodemailer";
import {
  GITHUB_WEBHOOK_SECRET,
  SMTP_HOST,
  SMTP_PORT,
  SMTP_USER,
  SMTP_PASS,
} from "astro:env/server";
import { bustApiCaches } from "../../../lib/api-cache";

export const prerender = false;

const WEBHOOK_SECRET = GITHUB_WEBHOOK_SECRET ?? "";
const SITE_URL = "https://fluxdown.zerx.dev";

// ── 签名验证 ──

async function verifySignature(
  payload: string,
  signature: string | null,
): Promise<boolean> {
  if (!signature || !WEBHOOK_SECRET) return false;

  // GitHub 格式: "sha256=<hex>"
  const sigHex = signature.replace("sha256=", "");

  const encoder = new TextEncoder();
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(WEBHOOK_SECRET),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );

  const mac = await crypto.subtle.sign("HMAC", key, encoder.encode(payload));
  const expected = Array.from(new Uint8Array(mac))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");

  // 常量时间比较（防时序攻击）
  if (sigHex.length !== expected.length) return false;
  let diff = 0;
  for (let i = 0; i < sigHex.length; i++) {
    diff |= sigHex.charCodeAt(i) ^ expected.charCodeAt(i);
  }
  return diff === 0;
}

// ── 从 Issue body 中提取 contact 邮箱 ──

function extractContactEmail(body: string): string | null {
  // 匹配 **Contact:** <email> 格式
  const match = body.match(/\*\*Contact:\*\*\s*([^\s\n]+)/i);
  if (!match) return null;

  const email = match[1].trim();
  // 基础邮箱格式校验
  if (/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
    return email;
  }
  return null;
}

// ── 发送邮件 ──

async function sendNotificationEmail(
  to: string,
  issueTitle: string,
  issueNumber: number,
  commentBody: string,
  commentAuthor: string,
): Promise<void> {
  const transporter = nodemailer.createTransport({
    host: SMTP_HOST,
    port: SMTP_PORT,
    secure: SMTP_PORT === 465,
    auth: {
      user: SMTP_USER,
      pass: SMTP_PASS,
    },
  });

  // 截取评论预览（去 markdown 格式，限 500 字符）
  const preview = commentBody
    .replace(/[#*`>_~\[\]()]/g, "")
    .replace(/\n{2,}/g, "\n")
    .trim()
    .slice(0, 500);

  const cleanTitle = issueTitle
    .replace(
      /^[\u{1F300}-\u{1FAF6}\u{2600}-\u{27BF}\u{FE00}-\u{FE0F}\u{200D}\u{20E3}\u{E0020}-\u{E007F}]+\s*/u,
      "",
    )
    .replace(/^\[Website Feedback\]\s*/i, "");

  const feedbackUrl = `${SITE_URL}/feedback`;

  await transporter.sendMail({
    from: `"FluxDown" <${SMTP_USER}>`,
    to,
    subject: `Your feedback "${cleanTitle}" has a new reply`,
    html: `
<!DOCTYPE html>
<html>
<head><meta charset="utf-8"></head>
<body style="margin:0;padding:0;background:#0a0a0f;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;">
  <div style="max-width:560px;margin:0 auto;padding:32px 20px;">
    <!-- Header -->
    <div style="text-align:center;margin-bottom:28px;">
      <h1 style="color:#e4e4e7;font-size:20px;font-weight:700;margin:0;">FluxDown</h1>
      <p style="color:#71717a;font-size:12px;margin:4px 0 0;">Feedback Notification</p>
    </div>

    <!-- Card -->
    <div style="background:#18181b;border:1px solid #27272a;border-radius:12px;overflow:hidden;">
      <!-- Title bar -->
      <div style="padding:16px 20px;border-bottom:1px solid #27272a;">
        <p style="color:#a1a1aa;font-size:12px;margin:0 0 4px;">Your feedback #${issueNumber}</p>
        <h2 style="color:#e4e4e7;font-size:16px;font-weight:600;margin:0;line-height:1.4;">${cleanTitle}</h2>
      </div>

      <!-- Reply content -->
      <div style="padding:20px;">
        <div style="display:flex;align-items:center;gap:8px;margin-bottom:12px;">
          <span style="display:inline-block;width:24px;height:24px;border-radius:50%;background:#3b82f6;color:#fff;text-align:center;line-height:24px;font-size:12px;font-weight:600;">${commentAuthor.charAt(0).toUpperCase()}</span>
          <span style="color:#93c5fd;font-size:13px;font-weight:500;">Developer</span>
        </div>
        <div style="color:#d4d4d8;font-size:14px;line-height:1.6;white-space:pre-wrap;">${preview}</div>
      </div>

      <!-- CTA -->
      <div style="padding:0 20px 20px;text-align:center;">
        <a href="${feedbackUrl}" style="display:inline-block;padding:10px 24px;background:#3b82f6;color:#fff;text-decoration:none;border-radius:8px;font-size:14px;font-weight:500;">View on FluxDown</a>
      </div>
    </div>

    <!-- Footer -->
    <p style="color:#52525b;font-size:11px;text-align:center;margin:20px 0 0;line-height:1.5;">
      You received this email because you submitted feedback on FluxDown.<br>
      If you didn't submit any feedback, please ignore this email.
    </p>
  </div>
</body>
</html>
    `.trim(),
  });
}

// ── 类型 ──

interface WebhookPayload {
  action: string;
  issue: {
    number: number;
    title: string;
    body: string | null;
    labels: { name: string }[];
    state: string;
  };
  comment: {
    id: number;
    body: string;
    user: {
      login: string;
    };
  };
}

// ── Handler ──

export const POST: APIRoute = async ({ request }) => {
  // 1. 读取原始 body
  const rawBody = await request.text();

  // 2. 验证签名
  const signature = request.headers.get("x-hub-signature-256");
  const valid = await verifySignature(rawBody, signature);
  if (!valid) {
    console.error("[webhook] Invalid signature");
    return new Response(JSON.stringify({ error: "Invalid signature" }), {
      status: 401,
      headers: { "Content-Type": "application/json" },
    });
  }

  // 3. 检查事件类型
  const event = request.headers.get("x-github-event");

  // release 事件：发版后立即清除 /api/release 与 /api/changelog 的进程内缓存，
  // 保证官网下载地址即时指向新版本（配合 GitHub Actions 发版流程）
  if (event === "release") {
    let action = "";
    try {
      action = (JSON.parse(rawBody) as { action?: string }).action ?? "";
    } catch {
      return new Response(JSON.stringify({ error: "Invalid JSON" }), {
        status: 400,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (action === "published" || action === "edited" || action === "deleted") {
      bustApiCaches();
      console.log(`[webhook] Release ${action}, API caches busted`);
      return new Response(JSON.stringify({ ok: true, busted: true }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response(JSON.stringify({ ok: true, skipped: action }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }

  if (event !== "issue_comment") {
    // 非评论事件，静默接受
    return new Response(
      JSON.stringify({ ok: true, skipped: "not issue_comment" }),
      {
        status: 200,
        headers: { "Content-Type": "application/json" },
      },
    );
  }

  // 4. 解析 payload
  let payload: WebhookPayload;
  try {
    payload = JSON.parse(rawBody);
  } catch {
    return new Response(JSON.stringify({ error: "Invalid JSON" }), {
      status: 400,
      headers: { "Content-Type": "application/json" },
    });
  }

  // 5. 只处理新建的评论
  if (payload.action !== "created") {
    return new Response(JSON.stringify({ ok: true, skipped: "not created" }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }

  // 6. 检查 issue 是否有 user-feedback 标签
  const hasFeedbackLabel = payload.issue.labels.some(
    (l) => l.name === "user-feedback",
  );
  if (!hasFeedbackLabel) {
    return new Response(
      JSON.stringify({ ok: true, skipped: "no user-feedback label" }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );
  }

  // 7. 排除网站访客自己的回复（由 bot token 发出、带 "Website visitor reply" 标记）
  if (payload.comment.body.includes("Website visitor reply")) {
    return new Response(
      JSON.stringify({ ok: true, skipped: "visitor reply" }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );
  }

  // 8. 从 issue body 中提取联系邮箱
  const contactEmail = extractContactEmail(payload.issue.body || "");
  if (!contactEmail) {
    console.log(
      `[webhook] Issue #${payload.issue.number} has no contact email, skipping`,
    );
    return new Response(
      JSON.stringify({ ok: true, skipped: "no contact email" }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );
  }

  // 9. 检查 SMTP 配置
  if (!SMTP_HOST || !SMTP_USER || !SMTP_PASS) {
    console.error("[webhook] SMTP not configured");
    return new Response(JSON.stringify({ error: "SMTP not configured" }), {
      status: 500,
      headers: { "Content-Type": "application/json" },
    });
  }

  // 10. 发送邮件
  try {
    await sendNotificationEmail(
      contactEmail,
      payload.issue.title,
      payload.issue.number,
      payload.comment.body,
      payload.comment.user.login,
    );

    console.log(
      `[webhook] Email sent to ${contactEmail} for issue #${payload.issue.number}`,
    );

    return new Response(JSON.stringify({ ok: true, sent: true }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  } catch (err) {
    console.error("[webhook] Failed to send email:", err);
    return new Response(JSON.stringify({ error: "Failed to send email" }), {
      status: 500,
      headers: { "Content-Type": "application/json" },
    });
  }
};
