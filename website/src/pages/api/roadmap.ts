import type { APIRoute } from "astro";
import { GITHUB_TOKEN, GITHUB_REPO } from "astro:env/server";

export const prerender = false;

// ─────────────────────────────────────────────
// Author-curated roadmap backed by GitHub Issues + labels.
//
// The roadmap is NOT community-driven (that's /api/feature-vote). It is the
// author's confirmed major direction, controlled entirely by which labels an
// issue carries — edit the labels on GitHub, the site reflects it in ≤5 min.
//
//   Marker label (required)  : "roadmap"
//   Status labels (pick one) :
//     "roadmap:planned"      → 计划中  (Planned)
//     "roadmap:in-progress"  → 实施中  (In Progress)
//     "roadmap:done"         → 已完成  (Completed)
//
// Tolerant matching also accepts bare "planned" / "in progress" / "done" /
// "completed" / "shipped" and the Chinese "计划中"/"实施中"/"已完成".
// Fallback when no status label is present: CLOSED issue → done, else planned.
//
// One GitHub API call (list issues, labels included). 5-minute in-memory cache.
// ─────────────────────────────────────────────

const ROADMAP_LABEL = "roadmap";
const CACHE_TTL = 5 * 60 * 1000; // 5 min

type RoadmapStatus = "planned" | "in-progress" | "done";

/** Column render order — mirrors the author's mental model (planned → done). */
const STATUS_ORDER: RoadmapStatus[] = ["planned", "in-progress", "done"];

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

interface GHLabel {
  name: string;
  color: string;
}

interface GHIssue {
  number: number;
  title: string;
  body: string | null;
  html_url: string;
  state?: string;
  comments?: number;
  created_at: string;
  updated_at: string;
  labels?: Array<GHLabel | string>;
  pull_request?: unknown;
}

function labelObjects(issue: GHIssue): GHLabel[] {
  return (issue.labels ?? []).map((l) =>
    typeof l === "string" ? { name: l, color: "" } : { name: l.name, color: l.color },
  );
}

/** Fetch with simple retry (up to 3 attempts, back-off). */
async function fetchWithRetry(
  url: string,
  init: RequestInit,
  attempts = 3,
): Promise<Response> {
  let lastErr: unknown;
  for (let i = 0; i < attempts; i++) {
    try {
      return await fetch(url, init);
    } catch (err) {
      lastErr = err;
      if (i < attempts - 1) {
        await new Promise((r) => setTimeout(r, 500 * (i + 1)));
      }
    }
  }
  throw lastErr;
}

/** GitHub rate-limit / abuse responses (403 with remaining=0, or 429). */
function isRateLimitedResponse(res: Response): boolean {
  if (res.status === 429) return true;
  return res.status === 403 && res.headers.get("x-ratelimit-remaining") === "0";
}

/**
 * Fetch roadmap issues (label "roadmap", state=all so completed/closed items
 * still appear, max 3 pages = 300 items). Throws when the FIRST page fails so
 * callers never cache an empty list; later-page failures degrade gracefully.
 */
async function fetchRoadmapIssues(): Promise<GHIssue[]> {
  const issues: GHIssue[] = [];
  for (let page = 1; page <= 3; page++) {
    let res: Response;
    try {
      res = await fetchWithRetry(
        `https://api.github.com/repos/${GITHUB_REPO}/issues?state=all&labels=${ROADMAP_LABEL}&per_page=100&page=${page}`,
        { headers: ghHeaders() },
      );
    } catch (err) {
      if (page === 1) throw err;
      break;
    }
    if (!res.ok) {
      if (page === 1) {
        throw new Error(
          `list issues failed: ${res.status}${isRateLimitedResponse(res) ? " (rate limited)" : ""}`,
        );
      }
      break;
    }
    const batch: GHIssue[] = await res.json();
    if (!Array.isArray(batch)) break;
    for (const issue of batch) {
      if (issue.pull_request) continue;
      issues.push(issue);
    }
    if (batch.length < 100) break;
  }
  return issues;
}

// ─────────────────────────────────────────────
// Status classification
// ─────────────────────────────────────────────

const IN_PROGRESS_TOKENS = [
  "roadmap:in-progress",
  "in-progress",
  "in progress",
  "实施中",
  "进行中",
  "doing",
  "wip",
];
const DONE_TOKENS = [
  "roadmap:done",
  "done",
  "completed",
  "complete",
  "shipped",
  "已完成",
  "完成",
];
const PLANNED_TOKENS = ["roadmap:planned", "planned", "计划中", "计划"];

/** Classify an issue's roadmap status from its labels (fallback on state). */
function classifyStatus(issue: GHIssue): RoadmapStatus {
  const names = labelObjects(issue).map((l) => l.name.trim().toLowerCase());
  const has = (tokens: string[]) => names.some((n) => tokens.includes(n));
  // In-progress wins over done/planned so a "doing" item never reads as shipped.
  if (has(IN_PROGRESS_TOKENS)) return "in-progress";
  if (has(DONE_TOKENS)) return "done";
  if (has(PLANNED_TOKENS)) return "planned";
  return issue.state === "closed" ? "done" : "planned";
}

/** Labels that drive status/marker — hidden from the card's category chips. */
const STATUS_MARKER_TOKENS = new Set(
  [ROADMAP_LABEL, ...IN_PROGRESS_TOKENS, ...DONE_TOKENS, ...PLANNED_TOKENS].map((t) =>
    t.toLowerCase(),
  ),
);

function categoryLabels(issue: GHIssue): GHLabel[] {
  return labelObjects(issue).filter(
    (l) => !STATUS_MARKER_TOKENS.has(l.name.trim().toLowerCase()),
  );
}

// ─────────────────────────────────────────────
// Title / description extraction
// ─────────────────────────────────────────────

/** Strip a leading emoji + "[Roadmap]"-style tag decoration from a title. */
function cleanTitle(raw: string): string {
  return raw.replace(/^[^[\]]{0,6}\[[^\]]*\]\s*/u, "").trim() || raw.trim();
}

/**
 * Display-friendly excerpt from an issue body: take the part before the first
 * metadata separator, drop markdown headings and legacy meta blocks.
 */
function extractDescription(body: string | null): string {
  if (!body) return "";
  const sepIdx = body.indexOf("\n---\n");
  const content = sepIdx >= 0 ? body.slice(0, sepIdx) : body;
  const cleaned = content
    .replace(/^#{1,6}\s+.*$/gm, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/```[\s\S]*?```/g, "")
    .trim();
  return cleaned.length > 240 ? `${cleaned.slice(0, 240)}…` : cleaned;
}

// ─────────────────────────────────────────────
// Response shape + cache
// ─────────────────────────────────────────────

interface RoadmapItem {
  id: number;
  title: string;
  description: string;
  status: RoadmapStatus;
  url: string;
  labels: GHLabel[];
  createdAt: string;
  updatedAt: string;
  comments: number;
}

interface RoadmapColumn {
  status: RoadmapStatus;
  items: RoadmapItem[];
}

interface RoadmapData {
  columns: RoadmapColumn[];
  counts: Record<RoadmapStatus, number>;
  total: number;
  updatedAt: string;
}

let listCache: { data: RoadmapData; timestamp: number } | null = null;

function buildData(issues: GHIssue[]): RoadmapData {
  const buckets: Record<RoadmapStatus, RoadmapItem[]> = {
    planned: [],
    "in-progress": [],
    done: [],
  };

  for (const issue of issues) {
    const status = classifyStatus(issue);
    buckets[status].push({
      id: issue.number,
      title: cleanTitle(issue.title),
      description: extractDescription(issue.body),
      status,
      url: issue.html_url,
      labels: categoryLabels(issue),
      createdAt: issue.created_at,
      updatedAt: issue.updated_at,
      comments: issue.comments ?? 0,
    });
  }

  // Within each column, most recently touched first (author bumps by editing).
  for (const status of STATUS_ORDER) {
    buckets[status].sort(
      (a, b) => new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime(),
    );
  }

  return {
    columns: STATUS_ORDER.map((status) => ({ status, items: buckets[status] })),
    counts: {
      planned: buckets.planned.length,
      "in-progress": buckets["in-progress"].length,
      done: buckets.done.length,
    },
    total: issues.length,
    updatedAt: new Date().toISOString(),
  };
}

// ─────────────────────────────────────────────
// GET /api/roadmap
// ─────────────────────────────────────────────

export const GET: APIRoute = async () => {
  if (!GITHUB_TOKEN) {
    return json({ error: "Server misconfigured" }, 500);
  }

  if (listCache && Date.now() - listCache.timestamp < CACHE_TTL) {
    return json(listCache.data, 200, {
      "Cache-Control": "public, s-maxage=300, stale-while-revalidate=600",
    });
  }

  try {
    const issues = await fetchRoadmapIssues();
    const data = buildData(issues);
    listCache = { data, timestamp: Date.now() };
    return json(data, 200, {
      "Cache-Control": "public, s-maxage=300, stale-while-revalidate=600",
    });
  } catch (err) {
    console.error("[roadmap] GET error:", err);
    // Serve the last good snapshot (even past TTL) — typical cause is a GitHub
    // rate limit, which resolves within the hour.
    if (listCache) {
      return json(listCache.data, 200, {
        "Cache-Control": "public, s-maxage=300, stale-while-revalidate=600",
      });
    }
    return json({ error: "Failed to fetch roadmap" }, 503);
  }
};

function json(
  data: unknown,
  status: number,
  extraHeaders: Record<string, string> = {},
): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json", ...extraHeaders },
  });
}
