import type { APIRoute } from "astro";
import { GITHUB_TOKEN, GITHUB_REPO } from "astro:env/server";

export const prerender = false;

// ─────────────────────────────────────────────
// Built-in logos
// Add files to public/logos/ and register them here.
// ─────────────────────────────────────────────
interface BuiltinLogo {
  id: string; // stable identifier, e.g. "builtin-01"
  filename: string; // served under /logos/
  description: string;
}

const BUILTIN_LOGOS: BuiltinLogo[] = [
  {
    id: "builtin-0F900664495063A1DD0634DFBF4E68CE",
    filename: "0F900664495063A1DD0634DFBF4E68CE.jpg",
    description: "Community logo design #1",
  },
  {
    id: "builtin-103F353FD6CEEC59C3BD23574597F898",
    filename: "103F353FD6CEEC59C3BD23574597F898.png",
    description: "Community logo design #2",
  },
  {
    id: "builtin-59EA0193A911CF1FB3C3DA6A7CD10579",
    filename: "59EA0193A911CF1FB3C3DA6A7CD10579.png",
    description: "Community logo design #4",
  },
  {
    id: "builtin-968086859FFFDE34B0B28FFABCBB462C",
    filename: "968086859FFFDE34B0B28FFABCBB462C.png",
    description: "Community logo design #5",
  },
  {
    id: "builtin-A8814129540721CBB8C3CD6D4445C038",
    filename: "A8814129540721CBB8C3CD6D4445C038.jpg",
    description: "Community logo design #6",
  },
  {
    id: "builtin-C27B37A4C1FD00E427B1F26EBE6F17FB",
    filename: "C27B37A4C1FD00E427B1F26EBE6F17FB.png",
    description: "Community logo design #7",
  },
  {
    id: "builtin-CC60A08FF2E29164435B379AF91CCBBF",
    filename: "CC60A08FF2E29164435B379AF91CCBBF.jpg",
    description: "Community logo design #8",
  },
  {
    id: "builtin-F5CD102E02D47F1570AA70D804EDBFCA",
    filename: "F5CD102E02D47F1570AA70D804EDBFCA.jpg",
    description: "Community logo design #9",
  },
];

// ─────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────

// All votes (builtin + submission) are stored as comments in ONE issue.
// This means GET only needs 2 GitHub API calls: list issues + list comments.
const VOTES_ISSUE_TITLE = "[FluxDown] Logo Vote Records";
const SUBMISSION_TITLE_PREFIX = "[Logo]";

const BUILTIN_UPLOAD_DATE = "2025-01-01T00:00:00.000Z";

// Cache
const CACHE_TTL = 30_000; // 30 s

// Rate limit (POST)
const VOTE_RATE_WINDOW = 60_000; // 1 min
const VOTE_RATE_MAX = 20;

// ─────────────────────────────────────────────
// GitHub helpers
// ─────────────────────────────────────────────

function ghHeaders(): Record<string, string> {
  return {
    Authorization: `Bearer ${GITHUB_TOKEN}`,
    Accept: "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28",
    "Content-Type": "application/json",
  };
}

interface GHIssue {
  number: number;
  title: string;
  body: string;
  created_at: string;
  labels: { name: string }[];
}

interface GHComment {
  id: number;
  body: string;
}

/** Fetch with simple retry (up to 3 attempts, 500 ms back-off). */
async function fetchWithRetry(
  url: string,
  init: RequestInit,
  attempts = 3,
): Promise<Response> {
  let lastErr: unknown;
  for (let i = 0; i < attempts; i++) {
    try {
      const res = await fetch(url, init);
      return res;
    } catch (err) {
      lastErr = err;
      if (i < attempts - 1) {
        await new Promise((r) => setTimeout(r, 500 * (i + 1)));
      }
    }
  }
  throw lastErr;
}

/**
 * Fetch ALL comments for an issue (handles pagination), with retry.
 * Returns [] on any error so callers can degrade gracefully.
 */
async function fetchAllComments(issueNumber: number): Promise<GHComment[]> {
  const all: GHComment[] = [];
  let page = 1;
  while (true) {
    let res: Response;
    try {
      res = await fetchWithRetry(
        `https://api.github.com/repos/${GITHUB_REPO}/issues/${issueNumber}/comments?per_page=100&page=${page}`,
        { headers: ghHeaders() },
      );
    } catch {
      break; // network error — return what we have
    }
    if (!res.ok) break;
    const batch: GHComment[] = await res.json();
    if (!Array.isArray(batch) || batch.length === 0) break;
    all.push(...batch);
    if (batch.length < 100) break;
    page++;
  }
  return all;
}

/**
 * Find the single votes-tracking issue by title.
 * Returns null if not found (caller decides whether to create).
 */
async function findVotesIssue(): Promise<number | null> {
  // Search open issues page by page (max 2 pages = 200 issues)
  for (let page = 1; page <= 2; page++) {
    let res: Response;
    try {
      res = await fetchWithRetry(
        `https://api.github.com/repos/${GITHUB_REPO}/issues?state=open&per_page=100&page=${page}`,
        { headers: ghHeaders() },
      );
    } catch {
      return null;
    }
    if (!res.ok) return null;
    const issues: GHIssue[] = await res.json();
    if (!Array.isArray(issues)) return null;
    const found = issues.find((i) => i.title === VOTES_ISSUE_TITLE);
    if (found) return found.number;
    if (issues.length < 100) break;
  }
  return null;
}

/** Find or lazily create the single votes issue. */
async function findOrCreateVotesIssue(): Promise<number> {
  const existing = await findVotesIssue();
  if (existing !== null) return existing;

  const res = await fetchWithRetry(
    `https://api.github.com/repos/${GITHUB_REPO}/issues`,
    {
      method: "POST",
      headers: ghHeaders(),
      body: JSON.stringify({
        title: VOTES_ISSUE_TITLE,
        body: [
          "## FluxDown Logo Vote Records",
          "",
          "This issue stores all logo vote comments.",
          "Each comment is a JSON record: `{ logoId, ip, action, date }`.",
          "**Do not close or rename this issue.**",
        ].join("\n"),
        // No labels — avoids 422 when label doesn't exist in repo
      }),
    },
  );

  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Failed to create votes issue: ${res.status} ${text}`);
  }

  const created: GHIssue = await res.json();
  return created.number;
}

/**
 * Find submission issues by title prefix.
 * Only fetches page 1 (100 issues) — enough for now, extend if needed.
 */
async function fetchSubmissionIssues(): Promise<GHIssue[]> {
  const results: GHIssue[] = [];
  for (let page = 1; page <= 5; page++) {
    let res: Response;
    try {
      res = await fetchWithRetry(
        `https://api.github.com/repos/${GITHUB_REPO}/issues?state=open&per_page=100&page=${page}`,
        { headers: ghHeaders() },
      );
    } catch {
      break;
    }
    if (!res.ok) break;
    const issues: GHIssue[] = await res.json();
    if (!Array.isArray(issues) || issues.length === 0) break;
    const matched = issues.filter((i) =>
      i.title.startsWith(SUBMISSION_TITLE_PREFIX),
    );
    results.push(...matched);
    if (issues.length < 100) break;
  }
  return results;
}

// ─────────────────────────────────────────────
// Vote record helpers
// ─────────────────────────────────────────────

interface VoteRecord {
  logoId: string;
  ip: string;
  action: "vote" | "unvote";
  date: string;
}

function parseVoteComment(body: string): VoteRecord | null {
  const m = body.match(/```json\s*([\s\S]*?)```/);
  if (!m) return null;
  try {
    const d = JSON.parse(m[1]);
    if (
      typeof d.logoId === "string" &&
      typeof d.ip === "string" &&
      (d.action === "vote" || d.action === "unvote")
    ) {
      return d as VoteRecord;
    }
  } catch {
    // malformed
  }
  return null;
}

function buildVoteCommentBody(record: VoteRecord): string {
  return [
    "### Logo Vote Record",
    "",
    "```json",
    JSON.stringify(record, null, 2),
    "```",
    "",
    `- **Logo:** ${record.logoId}`,
    `- **Action:** ${record.action}`,
    `- **Date:** ${record.date}`,
  ].join("\n");
}

/**
 * Count net votes for a logoId.
 * Per-IP replay: last action wins.
 */
function countVotes(records: VoteRecord[], logoId: string): number {
  const perIp = new Map<string, "vote" | "unvote">();
  for (const r of records) {
    if (r.logoId === logoId) perIp.set(r.ip, r.action);
  }
  let count = 0;
  for (const a of perIp.values()) {
    if (a === "vote") count++;
  }
  return count;
}

// ─────────────────────────────────────────────
// Submission issue metadata
// ─────────────────────────────────────────────

interface SubmissionMeta {
  filename: string;
  mimeType: string;
  submitterName: string;
  description: string;
  uploadedAt: string;
  imageUrl: string;
}

function parseSubmissionMeta(body: string): SubmissionMeta | null {
  const m = body.match(
    /<!-- logo-data-start -->\s*```json\s*([\s\S]*?)```\s*<!-- logo-data-end -->/,
  );
  if (!m) return null;
  try {
    const d = JSON.parse(m[1]);
    if (d.filename && d.uploadedAt) {
      return {
        filename: d.filename,
        mimeType: d.mimeType ?? "image/png",
        submitterName: d.submitterName ?? "",
        description: d.description ?? "",
        uploadedAt: d.uploadedAt,
        imageUrl: d.imageUrl ?? "",
      };
    }
  } catch {
    // malformed
  }
  return null;
}

// ─────────────────────────────────────────────
// Response types & sorting
// ─────────────────────────────────────────────

interface LogoEntry {
  id: string;
  filename: string;
  submitterName: string;
  description: string;
  uploadedAt: string;
  votes: number;
  isBuiltin: boolean;
  imageUrl?: string;
}

function sortLogos(logos: LogoEntry[]): LogoEntry[] {
  const sorted = [...logos].sort((a, b) => {
    if (b.votes !== a.votes) return b.votes - a.votes;
    // tie-break: older upload first (stable order for top-10)
    return new Date(a.uploadedAt).getTime() - new Date(b.uploadedAt).getTime();
  });
  const top10 = sorted.slice(0, 10);
  const rest = sorted
    .slice(10)
    .sort(
      (a, b) =>
        new Date(b.uploadedAt).getTime() - new Date(a.uploadedAt).getTime(),
    );
  return [...top10, ...rest];
}

// ─────────────────────────────────────────────
// In-memory cache
// ─────────────────────────────────────────────

interface ListCache {
  data: { logos: LogoEntry[] };
  timestamp: number;
}

let listCache: ListCache | null = null;

// ─────────────────────────────────────────────
// Rate limiting
// ─────────────────────────────────────────────

const voteRateMap = new Map<string, { count: number; resetAt: number }>();

setInterval(() => {
  const now = Date.now();
  for (const [ip, e] of voteRateMap) {
    if (now > e.resetAt) voteRateMap.delete(ip);
  }
}, 5 * 60_000);

function isRateLimited(ip: string): boolean {
  const now = Date.now();
  const e = voteRateMap.get(ip);
  if (!e || now > e.resetAt) {
    voteRateMap.set(ip, { count: 1, resetAt: now + VOTE_RATE_WINDOW });
    return false;
  }
  e.count++;
  return e.count > VOTE_RATE_MAX;
}

// ─────────────────────────────────────────────
// GET /api/logo-vote
// Only 2 GitHub API calls (+ pagination if needed):
//   1. findVotesIssue  — scans open issues for our tracking issue
//   2. fetchAllComments — pulls all vote records from that single issue
// Submission issue list is fetched in parallel with step 1.
// No per-submission comment fetching needed.
// ─────────────────────────────────────────────
export const GET: APIRoute = async () => {
  if (!GITHUB_TOKEN) {
    return json({ error: "Server misconfigured" }, 500);
  }

  if (listCache && Date.now() - listCache.timestamp < CACHE_TTL) {
    return json(listCache.data, 200, {
      "Cache-Control": "public, s-maxage=30, stale-while-revalidate=60",
    });
  }

  try {
    // Run both lookups in parallel to minimize wall-clock time
    const [votesIssueNumber, submissionIssues] = await Promise.all([
      findVotesIssue().catch(() => null),
      fetchSubmissionIssues().catch(() => [] as GHIssue[]),
    ]);

    // Pull all vote comments from the single tracking issue (or empty if not created yet)
    let allVoteRecords: VoteRecord[] = [];
    if (votesIssueNumber !== null) {
      const comments = await fetchAllComments(votesIssueNumber);
      allVoteRecords = comments
        .map((c) => parseVoteComment(c.body))
        .filter((r): r is VoteRecord => r !== null);
    }

    // Build builtin entries
    const builtinEntries: LogoEntry[] = BUILTIN_LOGOS.map((bl) => ({
      id: bl.id,
      filename: bl.filename,
      submitterName: "",
      description: bl.description,
      uploadedAt: BUILTIN_UPLOAD_DATE,
      votes: countVotes(allVoteRecords, bl.id),
      isBuiltin: true,
    }));

    // Build submission entries (metadata already in issue body — no extra API call)
    const submissionEntries: LogoEntry[] = submissionIssues
      .map((issue): LogoEntry | null => {
        const meta = parseSubmissionMeta(issue.body ?? "");
        if (!meta) return null;
        const entry: LogoEntry = {
          id: String(issue.number),
          filename: meta.filename,
          submitterName: meta.submitterName,
          description: meta.description,
          uploadedAt: meta.uploadedAt,
          votes: countVotes(allVoteRecords, String(issue.number)),
          isBuiltin: false,
        };
        if (meta.imageUrl) entry.imageUrl = meta.imageUrl;
        return entry;
      })
      .filter((e): e is LogoEntry => e !== null);

    const data = {
      logos: sortLogos([...builtinEntries, ...submissionEntries]),
    };
    listCache = { data, timestamp: Date.now() };

    return json(data, 200, {
      "Cache-Control": "public, s-maxage=30, stale-while-revalidate=60",
    });
  } catch (err) {
    console.error("[logo-vote] GET error:", err);
    return json({ error: "Failed to fetch logo list" }, 500);
  }
};

// ─────────────────────────────────────────────
// POST /api/logo-vote  — vote or unvote
// ─────────────────────────────────────────────
export const POST: APIRoute = async ({ request, clientAddress }) => {
  const ip = clientAddress || "unknown";

  if (!GITHUB_TOKEN) {
    return json({ error: "Server misconfigured" }, 500);
  }

  if (isRateLimited(ip)) {
    return json({ error: "Too many requests" }, 429);
  }

  let body: { logoId?: string; action?: string };
  try {
    body = await request.json();
  } catch {
    return json({ error: "Invalid JSON body" }, 400);
  }

  const { logoId, action } = body;

  if (!logoId || typeof logoId !== "string") {
    return json({ error: "logoId is required" }, 400);
  }
  if (action !== "vote" && action !== "unvote") {
    return json({ error: "action must be 'vote' or 'unvote'" }, 400);
  }

  // Validate logoId: must be a known builtin or an existing submission issue
  const isBuiltin = BUILTIN_LOGOS.some((bl) => bl.id === logoId);
  if (!isBuiltin) {
    const issueNum = parseInt(logoId, 10);
    if (isNaN(issueNum)) {
      return json({ error: "Invalid logoId" }, 400);
    }
    // Quick existence check
    let checkRes: Response;
    try {
      checkRes = await fetchWithRetry(
        `https://api.github.com/repos/${GITHUB_REPO}/issues/${issueNum}`,
        { headers: ghHeaders() },
      );
    } catch {
      return json({ error: "Failed to validate logo" }, 502);
    }
    if (!checkRes.ok) {
      return json({ error: "Logo not found" }, 404);
    }
    const issue: GHIssue = await checkRes.json();
    if (!issue.title.startsWith(SUBMISSION_TITLE_PREFIX)) {
      return json({ error: "Logo not found" }, 404);
    }
  }

  try {
    // Ensure the single votes issue exists
    const votesIssueNumber = await findOrCreateVotesIssue();

    // Read existing comments to determine current state for this IP + logoId
    const comments = await fetchAllComments(votesIssueNumber);
    const relevantRecords = comments
      .map((c) => parseVoteComment(c.body))
      .filter(
        (r): r is VoteRecord =>
          r !== null && r.logoId === logoId && r.ip === ip,
      );

    const lastAction =
      relevantRecords.length > 0
        ? relevantRecords[relevantRecords.length - 1].action
        : null;

    // Idempotent checks
    if (action === "vote" && lastAction === "vote") {
      return json({ success: true, message: "already_voted" }, 200);
    }
    if (action === "unvote" && lastAction !== "vote") {
      return json({ success: true, message: "not_voted" }, 200);
    }

    // Append new vote comment
    const record: VoteRecord = {
      logoId,
      ip,
      action,
      date: new Date().toISOString(),
    };

    const commentRes = await fetchWithRetry(
      `https://api.github.com/repos/${GITHUB_REPO}/issues/${votesIssueNumber}/comments`,
      {
        method: "POST",
        headers: ghHeaders(),
        body: JSON.stringify({ body: buildVoteCommentBody(record) }),
      },
    );

    if (!commentRes.ok) {
      const text = await commentRes.text();
      console.error(
        `[logo-vote] Failed to post vote comment: ${commentRes.status}`,
        text,
      );
      return json({ error: "Failed to record vote" }, 502);
    }

    // Invalidate cache
    listCache = null;

    return json(
      { success: true, message: action === "vote" ? "voted" : "unvoted" },
      201,
    );
  } catch (err) {
    console.error("[logo-vote] POST error:", err);
    return json({ error: "Internal server error" }, 500);
  }
};

// ─────────────────────────────────────────────
// Helper
// ─────────────────────────────────────────────
function json(
  data: unknown,
  status: number,
  extraHeaders: Record<string, string> = {},
): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: {
      "Content-Type": "application/json",
      ...extraHeaders,
    },
  });
}
