import { createPrivateKey, sign, type KeyObject } from "node:crypto";

const REGISTRY_URL = "https://raw.githubusercontent.com/zerx-lab/fluxdown-plugin-index/main/registry.json";
const GITHUB_API = "https://api.github.com";
const MAX_REGISTRY_BYTES = 512 * 1024;
const UPSTREAM_TIMEOUT_MS = 5000;
const JWT_HEADER = Buffer.from('{"alg":"RS256","typ":"JWT"}').toString("base64url");
type FeedbackFetch = (url: string, init?: RequestInit) => Promise<Response>;

export function isPluginIdentity(value: unknown): value is string {
  return typeof value === "string" && value.length <= 200 && value === value.trim() && /^[a-z0-9_-]+@[a-z0-9_-]+$/.test(value);
}

export function isFeedbackRepository(value: unknown): value is string {
  return typeof value === "string" && value === value.trim() &&
    /^[a-z\d](?:[a-z\d-]{0,37}[a-z\d])?\/[a-z\d_.-]{1,100}$/i.test(value) && !/\/\.{1,2}$/.test(value);
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

export function registryRepository(registry: unknown, identity: string): string | undefined {
  const plugins = record(registry)?.plugins;
  if (!Array.isArray(plugins)) throw new Error("Invalid plugin registry");

  let repository: string | undefined;
  for (const entry of plugins) {
    const plugin = record(entry);
    if (!plugin || (plugin.id !== identity &&
        !(Array.isArray(plugin.formerIds) && plugin.formerIds.includes(identity)))) continue;
    if (repository || !isPluginIdentity(plugin.id) || !isFeedbackRepository(plugin.repo)) return undefined;
    repository = plugin.repo;
  }
  return repository;
}

/** Stream a bounded JSON payload even when Content-Length is missing or inaccurate. */
export async function readFeedbackJson(message: Request | Response, maxBytes: number): Promise<unknown> {
  if (Number(message.headers.get("Content-Length")) > maxBytes) {
    await message.body?.cancel();
    throw new RangeError("Payload too large");
  }
  if (!message.body) throw new SyntaxError("Missing body");
  const reader = message.body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let bytes = 0;
  let text = "";
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > maxBytes) throw new RangeError("Payload too large");
      text += decoder.decode(value, { stream: true });
    }
    return JSON.parse(text + decoder.decode());
  } catch (error) {
    await reader.cancel();
    throw error;
  } finally {
    reader.releaseLock();
  }
}

function redactPublicText(value: string): string {
  return value.normalize("NFC")
    .replace(/[\p{Cc}\p{Cf}]/gu, (char) => char === "\n" ? char : char === "\t" ? " " : "")
    .replace(/[\p{L}\p{N}.!#$%&'*+/=?^_`{|}~-]{1,64}@[\p{L}\p{N}-]{1,63}(?:\.[\p{L}\p{N}-]{1,63})+/gu, "[email removed]")
    .replace(/\b(?:tel|mailto):[^\s<>()]+/gi, "[contact removed]")
    .replace(/(^|[^\p{L}\p{N}.])\+?(?:\(\d{2,4}\)|\d)[\d ().-]{6,23}\d(?![\p{L}\p{N}]|\.\d)/gu, (number, prefix: string) => {
      const digits = number.slice(prefix.length).replace(/\D/g, "");
      return digits.length >= 7 ? `${prefix}[phone removed]` : number;
    })
    .replace(/\b(?:https?:\/\/)?(?:www\.)?github\.com\/[^\s/<>]+\/[^\s/<>]+\/(?:issues|pull)\/\d+[^\s<>()]*/gi, "[GitHub issue link removed]")
    .replaceAll("@", "＠")
    .replace(/#(?=\d)/g, "＃")
    .trim();
}

function literalBlock(text: string): string {
  // A user-supplied code fence cannot escape a generated, longer fence.
  const fence = "`".repeat((text.match(/`+/g) ?? []).reduce((max, run) => Math.max(max, run.length), 2) + 1);
  return `${fence}\n${text}\n${fence}`;
}

export function publicPluginIssue(fields: {
  identity: string;
  type: string;
  title: string;
  description: string;
  appVersion?: string;
  pluginVersion?: string;
  environment?: string;
}): { title: string; body: string } | undefined {
  const title = redactPublicText(fields.title.replace(/\s+/g, " "));
  const description = redactPublicText(fields.description.replace(/\r\n?/g, "\n"));
  if (!title || (!description && fields.type !== "feature")) return undefined;

  const metadata = [
    `Plugin: ${fields.identity}`,
    `Type: ${fields.type}`,
    fields.appVersion ? `FluxDown version: ${redactPublicText(fields.appVersion)}` : null,
    fields.pluginVersion ? `Plugin version: ${redactPublicText(fields.pluginVersion)}` : null,
    fields.environment ? `Environment: ${redactPublicText(fields.environment)}` : null,
  ].filter((line) => line !== null).join("\n");
  return {
    title: `[FluxDown plugin feedback] ${title}`.slice(0, 240),
    body: ["## Plugin feedback from FluxDown", literalBlock(metadata), description ? literalBlock(description) : null]
      .filter((section) => section !== null).join("\n\n"),
  };
}

export class PluginFeedbackFailure extends Error {
  constructor(readonly status: 404 | 409 | 429, message: string) {
    super(message);
  }
}

function unavailable(): PluginFeedbackFailure {
  return new PluginFeedbackFailure(404, "Plugin feedback is unavailable");
}

type GitHubOperation = "repository installation" | "installation token" | "repository metadata" | "create issue";

async function github(fetcher: FeedbackFetch, operation: GitHubOperation, path: string, token: string, body?: unknown): Promise<unknown> {
  const res = await fetcher(`${GITHUB_API}${path}`, {
    method: body === undefined ? "GET" : "POST",
    redirect: "manual",
    signal: AbortSignal.timeout(UPSTREAM_TIMEOUT_MS),
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      ...(body === undefined ? {} : { "Content-Type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) {
    console.warn("Plugin feedback GitHub request failed", operation, res.status);
    await res.body?.cancel();
    if (res.status === 429 || (res.status === 403 &&
        (res.headers.has("Retry-After") || res.headers.get("X-RateLimit-Remaining") === "0"))) {
      throw new PluginFeedbackFailure(429, "Too many requests. Please try again later.");
    }
    // Never follow a repository redirect (rename, transfer), or reuse another credential.
    if ((res.status >= 300 && res.status < 400) || [403, 404, 410, 451].includes(res.status) ||
        (res.status === 422 && operation === "installation token")) throw unavailable();
    throw new Error(`GitHub plugin feedback request failed: ${res.status}`);
  }
  return readFeedbackJson(res, 128 * 1024);
}

function appJwt(appId: string, key: KeyObject): string {
  const now = Math.floor(Date.now() / 1000);
  const payload = Buffer.from(JSON.stringify({ iat: now - 60, exp: now + 8 * 60, iss: appId })).toString("base64url");
  const data = `${JWT_HEADER}.${payload}`;
  return `${data}.${sign("RSA-SHA256", Buffer.from(data), key).toString("base64url")}`;
}

export function createPluginFeedback(appId?: string, privateKeyBase64?: string, fetcher: FeedbackFetch = fetch) {
  let key: KeyObject | undefined;

  async function authorize(identity: string, confirmedRepository?: string) {
    if (!appId || appId !== appId.trim() || !/^\d+$/.test(appId) || !privateKeyBase64) throw unavailable();
    const registry = await fetcher(REGISTRY_URL, {
      redirect: "error",
      cache: "no-store",
      signal: AbortSignal.timeout(UPSTREAM_TIMEOUT_MS),
    });
    if (!registry.ok) {
      await registry.body?.cancel();
      throw new Error(`Plugin feedback registry request failed: ${registry.status}`);
    }
    const repository = registryRepository(await readFeedbackJson(registry, MAX_REGISTRY_BYTES), identity);
    if (!repository) throw unavailable();
    if (confirmedRepository && confirmedRepository.toLowerCase() !== repository.toLowerCase()) {
      throw new PluginFeedbackFailure(409, "Plugin feedback destination changed");
    }

    key ??= createPrivateKey(Buffer.from(privateKeyBase64, "base64"));
    const jwt = appJwt(appId, key);
    const repoPath = `/repos/${repository}`;
    const installation = record(await github(fetcher, "repository installation", `${repoPath}/installation`, jwt));
    const id = installation?.id;
    if (typeof id !== "number" || !Number.isSafeInteger(id) || id <= 0 || installation?.suspended_at ||
        record(installation?.permissions)?.issues !== "write") throw unavailable();

    const write = confirmedRepository !== undefined;
    const access = record(await github(fetcher, "installation token", `/app/installations/${id}/access_tokens`, jwt, {
      repositories: [repository.split("/")[1]],
      permissions: write ? { issues: "write" } : { metadata: "read" },
    }));
    const permissions = record(access?.permissions);
    if (typeof access?.token !== "string" || !access.token ||
        (write ? permissions?.issues !== "write" : permissions?.metadata !== "read" || permissions.issues !== undefined)) {
      throw unavailable();
    }

    const metadata = record(await github(fetcher, "repository metadata", repoPath, access.token));
    if (!metadata || !isFeedbackRepository(metadata.full_name) || metadata.full_name.toLowerCase() !== repository.toLowerCase() ||
        metadata.private !== false || metadata.visibility !== "public" || metadata.has_issues !== true ||
        metadata.archived !== false || metadata.disabled !== false) throw unavailable();
    return { repository, token: access.token };
  }

  return {
    async destination(identity: string) {
      return (await authorize(identity)).repository;
    },
    async submit(identity: string, repository: string, issue: { title: string; body: string },
      acceptWrite: (authorizedRepository: string) => boolean) {
      const destination = await authorize(identity, repository);
      if (!acceptWrite(destination.repository)) {
        throw new PluginFeedbackFailure(429, "Too many requests. Please try again later.");
      }
      const result = record(await github(fetcher, "create issue", `/repos/${destination.repository}/issues`, destination.token, issue));
      if (typeof result?.number !== "number" || !Number.isSafeInteger(result.number) || result.number <= 0) {
        throw new Error("GitHub returned an invalid plugin feedback issue number");
      }
      return result.number;
    },
  };
}
