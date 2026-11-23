import { expect, test } from "bun:test";
import { generateKeyPairSync, verify } from "node:crypto";
import { createPluginFeedback, publicPluginIssue, readFeedbackJson } from "../src/lib/plugin-feedback";

const REGISTRY = "https://raw.githubusercontent.com/zerx-lab/fluxdown-plugin-index/main/registry.json";
const GITHUB = "https://api.github.com";
const IDENTITY = "author@plugin";
const FORMER_ID = "author@old";
const REPO = "author/Plugin";
const ISSUE = { title: "A report", body: "Something went wrong" };
const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const key = Buffer.from(privateKey.export({ format: "pem", type: "pkcs1" })).toString("base64");

function fixture() {
  const calls: { url: string; init?: RequestInit }[] = [];
  const metadata = { full_name: REPO, private: false, visibility: "public", has_issues: true, archived: false, disabled: false };
  const overrides = new Map<string, () => Response>();
  const feedback = createPluginFeedback("2142", key, async (url, init) => {
    calls.push({ url, init });
    const override = overrides.get(url);
    if (override) return override();
    switch (url) {
      case REGISTRY:
        return Response.json({ plugins: [{ id: IDENTITY, formerIds: [FORMER_ID], repo: REPO }] });
      case `${GITHUB}/repos/${REPO}/installation`:
        return Response.json({ id: 735, suspended_at: null, permissions: { issues: "write" } });
      case `${GITHUB}/app/installations/735/access_tokens`: {
        const { permissions } = JSON.parse(String(init?.body));
        return Response.json({ token: "scoped-token", permissions }, { status: 201 });
      }
      case `${GITHUB}/repos/${REPO}`:
        return Response.json(metadata);
      case `${GITHUB}/repos/${REPO}/issues`:
        return Response.json({ number: 27 }, { status: 201 });
      default:
        throw new Error(`Unexpected upstream request: ${url}`);
    }
  });
  return { calls, overrides, metadata, feedback };
}

test("GET resolves only the registry, not an arbitrary report destination, and leaves no issue", async () => {
  const { calls, feedback } = fixture();
  expect(await feedback.destination(FORMER_ID)).toBe(REPO);
  expect(calls.map((call) => call.url)).toEqual([
    REGISTRY, `${GITHUB}/repos/${REPO}/installation`, `${GITHUB}/app/installations/735/access_tokens`, `${GITHUB}/repos/${REPO}`,
  ]);
  expect(new Headers(calls[0].init?.headers).has("Authorization")).toBe(false);
  const installHeaders = new Headers(calls[1].init?.headers);
  const [header, claims, signature] = installHeaders.get("Authorization")!.slice("Bearer ".length).split(".");
  expect(JSON.parse(Buffer.from(header, "base64url").toString())).toMatchObject({ alg: "RS256" });
  expect(JSON.parse(Buffer.from(claims, "base64url").toString())).toMatchObject({ iss: "2142" });
  expect(verify("RSA-SHA256", Buffer.from(`${header}.${claims}`), publicKey, Buffer.from(signature, "base64url"))).toBe(true);
  expect(calls[2].init?.method).toBe("POST");
  expect(JSON.parse(String(calls[2].init?.body))).toEqual({ repositories: ["Plugin"], permissions: { metadata: "read" } });
  expect(new Headers(calls[3].init?.headers).get("Authorization")).toBe("Bearer scoped-token");
});

test("preview requires installation write permission but rejects a write-capable preview token", async () => {
  const { calls, overrides, feedback } = fixture();
  const installation = `${GITHUB}/repos/${REPO}/installation`;
  const token = `${GITHUB}/app/installations/735/access_tokens`;
  overrides.set(installation, () => Response.json({ id: 735, permissions: { issues: "read" } }));
  await expect(feedback.destination(IDENTITY)).rejects.toMatchObject({ status: 404 });
  expect(calls.at(-1)?.url).toBe(installation);

  overrides.delete(installation);
  overrides.set(token, () => Response.json({ token: "unsafe", permissions: { metadata: "read", issues: "write" } }));
  await expect(feedback.destination(IDENTITY)).rejects.toMatchObject({ status: 404 });
  expect(calls.at(-1)?.url).toBe(token);
});

test("POST checks the registry again after preview; the confirmed repository never routes a write", async () => {
  const { calls, overrides, feedback } = fixture();
  expect(await feedback.destination(IDENTITY)).toBe(REPO);
  calls.length = 0;
  overrides.set(REGISTRY, () => Response.json({ plugins: [{ id: IDENTITY, repo: "new-owner/new-repo" }] }));
  await expect(feedback.submit(IDENTITY, REPO, ISSUE, () => true)).rejects.toMatchObject({ status: 409 });
  expect(calls.map((call) => call.url)).toEqual([REGISTRY]);
});

test("POST writes only to the still-authorized repository, with a repository-scoped token", async () => {
  const { calls, feedback } = fixture();
  expect(await feedback.submit(IDENTITY, REPO.toLowerCase(), ISSUE, (repository) => repository === REPO)).toBe(27);
  const mint = calls.find((call) => call.url.endsWith("/access_tokens"))!;
  expect(JSON.parse(String(mint.init?.body))).toEqual({ repositories: ["Plugin"], permissions: { issues: "write" } });
  const write = calls.at(-1)!;
  expect(write.url).toBe(`${GITHUB}/repos/${REPO}/issues`);
  expect(write.init?.method).toBe("POST");
  expect(write.init?.redirect).toBe("manual");
  expect(new Headers(write.init?.headers).get("Authorization")).toBe("Bearer scoped-token");
  expect(JSON.parse(String(write.init?.body))).toEqual(ISSUE);
});

test("ambiguous identities and unsafe repositories fail closed before GitHub authorization", async () => {
  for (const plugins of [
    [{ id: IDENTITY, repo: REPO }, { id: FORMER_ID, repo: "other/repo", formerIds: [IDENTITY] }],
    [{ id: IDENTITY, repo: "author/.." }],
    [{ id: IDENTITY, repo: "author/repo?target=other" }],
    [{ id: IDENTITY, repo: "author/repo\n" }],
  ]) {
    const { calls, overrides, feedback } = fixture();
    overrides.set(REGISTRY, () => Response.json({ plugins }));
    await expect(feedback.destination(IDENTITY)).rejects.toMatchObject({ status: 404 });
    expect(calls.map((call) => call.url)).toEqual([REGISTRY]);
  }
});

test("uninstalled, redirected, private, or issues-disabled destinations cannot receive issues", async () => {
  for (const [url, reply] of [
    [`${GITHUB}/repos/${REPO}/installation`, () => new Response(null, { status: 404 })],
    [`${GITHUB}/app/installations/735/access_tokens`, () => new Response(null, { status: 422 })],
    [`${GITHUB}/repos/${REPO}`, () => Response.redirect("https://github.com/other/repo", 301)],
  ] as const) {
    const { calls, overrides, feedback } = fixture();
    overrides.set(url, reply);
    await expect(feedback.submit(IDENTITY, REPO, ISSUE, () => true)).rejects.toMatchObject({ status: 404 });
    expect(calls.at(-1)?.url).toBe(url);
  }
  for (const metadataOverride of [{ private: true }, { has_issues: false }, { full_name: "other/repo" }]) {
    const { calls, metadata, feedback } = fixture();
    Object.assign(metadata, metadataOverride);
    await expect(feedback.submit(IDENTITY, REPO, ISSUE, () => true)).rejects.toMatchObject({ status: 404 });
    expect(calls.at(-1)?.url).toBe(`${GITHUB}/repos/${REPO}`);
  }
});

test("write quota failures never reach the GitHub issue API", async () => {
  const { calls, feedback } = fixture();
  await expect(feedback.submit(IDENTITY, REPO, ISSUE, () => false)).rejects.toMatchObject({ status: 429 });
  expect(calls.at(-1)?.url).toBe(`${GITHUB}/repos/${REPO}`);
});

test("public text is literal with contact details and notification syntax neutralized", () => {
  const fields = {
    identity: IDENTITY,
    type: "bug",
    title: "Bug for @team/member\n#19",
    description: "```\nEmail user@example.test, call +1 (415) 555-0199. Fixes other/repo#11 and https://github.com/other/repo/issues/20\n```",
    appVersion: "0.7.0",
    pluginVersion: "1.2.3",
    environment: "Host platform: linux",
    contact: "never-public-contact",
    logs: "never-public-log",
  };
  const issue = publicPluginIssue(fields)!;
  expect(issue.title).not.toMatch(/@team|\n|#19/);
  expect(issue.body).toContain(`Plugin: ${IDENTITY}`);
  expect(issue.body).toContain("FluxDown version: 0.7.0");
  expect(issue.body).toContain("````\n```\nEmail [email removed], call [phone removed].");
  expect(issue.body).not.toMatch(/user@example|555-0199|repo#11|github\.com|never-public/);
});

test("body limits count bytes even with absent or inaccurate Content-Length", async () => {
  const text = '"微信"';
  await expect(readFeedbackJson(new Response(text), 4)).rejects.toBeInstanceOf(RangeError);
  await expect(readFeedbackJson(new Response(text, { headers: { "Content-Length": "1" } }), 4)).rejects.toBeInstanceOf(RangeError);
  expect(await readFeedbackJson(new Response(text), 8)).toBe("微信");
});
