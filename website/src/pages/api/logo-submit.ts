import type { APIRoute } from "astro";
import {
  GITHUB_TOKEN,
  GITHUB_REPO,
  CF_R2_ACCESS_KEY_ID,
  CF_R2_SECRET_ACCESS_KEY,
  CF_R2_ENDPOINT,
  CF_R2_BUCKET,
  CF_R2_PUBLIC_URL,
} from "astro:env/server";
import { S3Client, PutObjectCommand } from "@aws-sdk/client-s3";

export const prerender = false;

// ── Rate limiting ──────────────────────────────────────────────────────────────
const submitRateLimitMap = new Map<
  string,
  { count: number; resetAt: number }
>();
const SUBMIT_RATE_LIMIT_WINDOW = 24 * 60 * 60_000; // 24 hours
const SUBMIT_RATE_LIMIT_MAX = 3;

function isSubmitRateLimited(ip: string): boolean {
  const now = Date.now();
  const entry = submitRateLimitMap.get(ip);
  if (!entry || now > entry.resetAt) {
    submitRateLimitMap.set(ip, {
      count: 1,
      resetAt: now + SUBMIT_RATE_LIMIT_WINDOW,
    });
    return false;
  }
  entry.count += 1;
  return entry.count > SUBMIT_RATE_LIMIT_MAX;
}

setInterval(() => {
  const now = Date.now();
  for (const [ip, entry] of submitRateLimitMap) {
    if (now > entry.resetAt) submitRateLimitMap.delete(ip);
  }
}, 60 * 60_000);

// ── Constants ──────────────────────────────────────────────────────────────────
const LOGO_ISSUE_TITLE_PREFIX = "[Logo]";
const MAX_FILE_SIZE_BYTES = 10 * 1024 * 1024; // 10 MB
const ALLOWED_MIME_TYPES = new Set([
  "image/png",
  "image/jpeg",
  "image/svg+xml",
  "image/webp",
]);
const ALLOWED_EXTENSIONS = new Set([".png", ".jpg", ".jpeg", ".svg", ".webp"]);
const MAX_SUBMITTER_NAME_LENGTH = 50;
const MAX_DESCRIPTION_LENGTH = 200;

// R2 key prefix for logo uploads
const LOGO_KEY_PREFIX = "logos";

// ── Helpers ────────────────────────────────────────────────────────────────────
function ghHeaders(): Record<string, string> {
  return {
    Authorization: `Bearer ${GITHUB_TOKEN}`,
    Accept: "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28",
    "Content-Type": "application/json",
  };
}

function getFileExtension(filename: string): string {
  const lastDot = filename.lastIndexOf(".");
  if (lastDot === -1) return "";
  return filename.slice(lastDot).toLowerCase();
}

/** Keep only URL-safe characters, truncate to 80 chars. */
function sanitizeFilename(original: string): string {
  const ext = getFileExtension(original);
  const base = original
    .slice(0, original.length - ext.length)
    .replace(/[^a-zA-Z0-9._-]/g, "_")
    .slice(0, 80 - ext.length);
  return base + ext;
}

// ── R2 client (lazy singleton) ────────────────────────────────────────────────
let _r2Client: S3Client | null = null;

function getR2Client(): S3Client {
  if (!_r2Client) {
    _r2Client = new S3Client({
      region: "auto",
      endpoint: CF_R2_ENDPOINT,
      credentials: {
        accessKeyId: CF_R2_ACCESS_KEY_ID!,
        secretAccessKey: CF_R2_SECRET_ACCESS_KEY!,
      },
    });
  }
  return _r2Client;
}

/**
 * Upload a file to Cloudflare R2.
 * Returns the permanent public URL, or throws on failure.
 */
async function uploadFileToR2(params: {
  key: string; // object key, e.g. "logos/timestamp_filename.png"
  body: Uint8Array;
  contentType: string;
}): Promise<string> {
  await getR2Client().send(
    new PutObjectCommand({
      Bucket: CF_R2_BUCKET,
      Key: params.key,
      Body: params.body,
      ContentType: params.contentType,
    }),
  );
  const publicBase = CF_R2_PUBLIC_URL!.replace(/\/$/, "");
  return `${publicBase}/${params.key}`;
}

/** Build GitHub Issue body — only metadata + image URL, no binary data. */
function buildIssueBody(params: {
  filename: string;
  mimeType: string;
  r2Key: string;
  imageUrl: string;
  submitterName: string;
  description: string;
  uploadedAt: string;
}): string {
  const {
    filename,
    mimeType,
    r2Key,
    imageUrl,
    submitterName,
    description,
    uploadedAt,
  } = params;

  const metaJson = JSON.stringify(
    {
      filename,
      mimeType,
      r2Key,
      imageUrl,
      submitterName,
      description,
      uploadedAt,
    },
    null,
    2,
  );

  return [
    "### Logo Submission",
    "",
    `**Submitter:** ${submitterName || "Anonymous"}`,
    `**Description:** ${description || "(none)"}`,
    `**Uploaded At:** ${uploadedAt}`,
    `**File:** [${filename}](${imageUrl})`,
    "",
    `![preview](${imageUrl})`,
    "",
    "<!-- logo-data-start -->",
    "```json",
    metaJson,
    "```",
    "<!-- logo-data-end -->",
  ].join("\n");
}

// ── POST /api/logo-submit ──────────────────────────────────────────────────────
/** Voting has ended — reject all new submissions */
const VOTE_ENDED = true;

export const POST: APIRoute = async ({ request, clientAddress }) => {
  if (VOTE_ENDED) {
    return json(
      { error: "Logo voting has ended. Submissions are no longer accepted." },
      403,
    );
  }

  const ip = clientAddress || "unknown";

  if (!GITHUB_TOKEN) {
    return json({ error: "Server misconfigured" }, 500);
  }

  if (
    !CF_R2_ACCESS_KEY_ID ||
    !CF_R2_SECRET_ACCESS_KEY ||
    !CF_R2_ENDPOINT ||
    !CF_R2_BUCKET ||
    !CF_R2_PUBLIC_URL
  ) {
    return json({ error: "Server misconfigured: storage unavailable" }, 500);
  }

  if (isSubmitRateLimited(ip)) {
    return json(
      {
        error:
          "Too many submissions. You can submit at most 3 logos per 24 hours.",
      },
      429,
    );
  }

  // Must be multipart/form-data
  const contentType = request.headers.get("content-type") || "";
  if (!contentType.includes("multipart/form-data")) {
    return json({ error: "Expected multipart/form-data" }, 415);
  }

  let formData: FormData;
  try {
    formData = await request.formData();
  } catch {
    return json({ error: "Failed to parse form data" }, 400);
  }

  // ── Validate file ──────────────────────────────────────────────────────────
  const fileEntry = formData.get("file");
  if (!fileEntry || !(fileEntry instanceof File)) {
    return json({ error: "Missing required field: file" }, 400);
  }

  const file = fileEntry as File;

  if (file.size === 0) {
    return json({ error: "File is empty." }, 400);
  }
  if (file.size > MAX_FILE_SIZE_BYTES) {
    return json(
      { error: "File too large. Maximum allowed size is 10 MB." },
      413,
    );
  }

  const reportedMime = file.type;
  if (!ALLOWED_MIME_TYPES.has(reportedMime)) {
    return json(
      {
        error: `Unsupported file type "${reportedMime}". Allowed: png, jpg, jpeg, svg, webp.`,
      },
      400,
    );
  }

  const ext = getFileExtension(file.name);
  if (!ALLOWED_EXTENSIONS.has(ext)) {
    return json(
      {
        error: `Unsupported file extension "${ext}". Allowed: .png, .jpg, .jpeg, .svg, .webp.`,
      },
      400,
    );
  }

  // ── Validate optional text fields ─────────────────────────────────────────
  const rawSubmitterName =
    (formData.get("submitterName") as string | null) ?? "";
  const rawDescription = (formData.get("description") as string | null) ?? "";

  const submitterName = rawSubmitterName
    .trim()
    .slice(0, MAX_SUBMITTER_NAME_LENGTH);
  const description = rawDescription.trim().slice(0, MAX_DESCRIPTION_LENGTH);

  // ── Read file bytes ────────────────────────────────────────────────────────
  let fileBytes: Uint8Array;
  try {
    fileBytes = new Uint8Array(await file.arrayBuffer());
  } catch (err) {
    console.error("[logo-submit] Failed to read file bytes:", err);
    return json({ error: "Failed to process uploaded file." }, 500);
  }

  // ── Upload image to Cloudflare R2 ──────────────────────────────────────────
  const uploadedAt = new Date().toISOString();
  const timestamp = Date.now();
  const safeFilename = sanitizeFilename(file.name);
  const r2Filename = `${timestamp}_${safeFilename}`;
  const r2Key = `${LOGO_KEY_PREFIX}/${r2Filename}`;

  let imageUrl: string;
  try {
    imageUrl = await uploadFileToR2({
      key: r2Key,
      body: fileBytes,
      contentType: reportedMime,
    });
  } catch (err) {
    console.error("[logo-submit] Failed to upload image to R2:", err);
    return json(
      { error: "Failed to upload image. Please try again later." },
      502,
    );
  }

  // ── Create GitHub Issue (metadata only) ───────────────────────────────────
  const issueBody = buildIssueBody({
    filename: r2Filename,
    mimeType: reportedMime,
    r2Key,
    imageUrl,
    submitterName: submitterName || "Anonymous",
    description,
    uploadedAt,
  });

  const issueTitle = `${LOGO_ISSUE_TITLE_PREFIX} ${r2Filename} — ${submitterName || "Anonymous"}`;

  try {
    const createRes = await fetch(
      `https://api.github.com/repos/${GITHUB_REPO}/issues`,
      {
        method: "POST",
        headers: ghHeaders(),
        body: JSON.stringify({ title: issueTitle, body: issueBody }),
      },
    );

    if (!createRes.ok) {
      const text = await createRes.text();
      console.error(
        `[logo-submit] Failed to create GitHub issue: ${createRes.status}`,
        text,
      );
      return json(
        { error: "Failed to submit logo. Please try again later." },
        502,
      );
    }

    const created = await createRes.json();
    return json(
      {
        success: true,
        logoId: created.number,
        imageUrl,
        message: "Logo submitted successfully!",
      },
      201,
    );
  } catch (err) {
    console.error("[logo-submit] Unexpected error:", err);
    return json({ error: "Internal server error" }, 500);
  }
};

// ── Tiny helper ───────────────────────────────────────────────────────────────
function json(data: unknown, status: number): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
