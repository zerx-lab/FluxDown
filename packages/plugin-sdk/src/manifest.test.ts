import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import {
  networkHostsOf,
  reviewOf,
  riskOf,
  validatePackage,
  validateManifest,
  validatePageScriptFiles,
  validateViewIcon,
  validateSniffPack,
  validateArchiveRulesPack,
  VALID_PERMISSIONS,
  type PluginRisk,
} from "./manifest";

interface Case {
  name: string;
  manifest: unknown;
  files: string[];
  ok: boolean;
  risk?: PluginRisk;
  networkHosts?: string[];
}

const cases: Case[] = JSON.parse(
  readFileSync(new URL("../../../native/engine/src/plugin/fixtures/manifest_cases.json", import.meta.url), "utf8"),
);

describe("manifest fixtures (shared with Rust)", () => {
  test("fixture count", () => {
    expect(cases.length).toBeGreaterThanOrEqual(30);
  });
  for (const c of cases) {
    test(c.name, () => {
      const result = validateManifest(c.manifest, new Set(c.files));
      expect(result.ok, result.errors.join("; ")).toBe(c.ok);
      expect(result.ok).toBe(result.errors.length === 0);
      if (result.ok) {
        const m = c.manifest as Parameters<typeof riskOf>[0] & Parameters<typeof networkHostsOf>[0];
        if (c.risk) expect(riskOf(m)).toBe(c.risk);
        if (c.networkHosts) expect(networkHostsOf(m)).toEqual(c.networkHosts);
      }
    });
  }
});

describe("strict mode", () => {
  const base = { identity: "a@b", name: "N", version: "1.0.0" };
  test("requires entries to exist", () => {
    const m = { ...base, hooks: { entry: "h.js", events: ["onDone"] } };
    expect(validateManifest(m, new Set()).ok).toBe(true);
    expect(validateManifest(m, new Set(), { strict: true }).ok).toBe(false);
    expect(validateManifest(m, new Set(["h.js"]), { strict: true }).ok).toBe(true);
  });
  test("version must be plain three-part", () => {
    expect(validateManifest({ ...base, version: "v1.0.0-rc.1" }, new Set()).ok).toBe(true);
    expect(validateManifest({ ...base, version: "v1.0.0-rc.1" }, new Set(), { strict: true }).ok).toBe(false);
  });
  test("omitted files skip existence checks; strict without files is a clear error", () => {
    const m = { ...base, main: { entry: "main.js" }, hooks: { entry: "h.js", events: ["onDone"] } };
    expect(validateManifest(m).ok).toBe(true);
    expect(validateManifest(m, null).ok).toBe(true);
    expect(validateManifest({ ...base, version: "x" }).ok).toBe(false);
    const strict = validateManifest(m, undefined, { strict: true });
    expect(strict.ok).toBe(false);
    expect(strict.errors).toHaveLength(1);
    expect(strict.errors[0]).toContain("files");
  });
});

describe("misc rules", () => {
  const base = { identity: "a@b", name: "N", version: "1.0.0" };
  test("legacy window key is rejected", () => {
    expect(validateManifest({ ...base, window: { entry: "w.js" } }, new Set(["w.js"])).ok).toBe(false);
  });
  test("settings matrix", () => {
    const bad = { ...base, settings: [{ key: "k", title: "K", type: "number", widget: "text" }] };
    expect(validateManifest(bad, new Set()).ok).toBe(false);
    const good = { ...base, settings: [{ key: "k", title: "K", type: "number", min: 1, max: 5, default: "3" }] };
    expect(validateManifest(good, new Set()).ok).toBe(true);
  });
  test("ten permissions", () => {
    expect(VALID_PERMISSIONS).toHaveLength(10);
  });
  test("ipv6 network hosts", () => {
    const m = (host: string) => ({
      ...base,
      permissions: ["desktop"],
      contributes: { views: [{ id: "w", title: "T", location: "window", desktop: { entry: "w.js", networkHosts: [host] } }] },
    });
    const files = new Set(["w.js"]);
    expect(validateManifest(m("::1"), files).ok).toBe(true);
    expect(validateManifest(m("2001:db8::1"), files).ok).toBe(true);
    expect(validateManifest(m("2001:DB8::1"), files).ok).toBe(false);
    expect(validateManifest(m("1.2.3.4"), files).ok).toBe(true);
    // Not an IP literal, but numeric labels are valid DNS labels (same as the Rust validator).
    expect(validateManifest(m("1.2.3.256"), files).ok).toBe(true);
  });
});

describe("validateSniffPack", () => {
  test("accepts a valid pack", () => {
    expect(validateSniffPack('{"format":"fluxdown-sniff-pack","rules":[]}').ok).toBe(true);
  });
  test("rejects wrong format, missing rules, bad JSON", () => {
    expect(validateSniffPack('{"format":"other","rules":[]}').ok).toBe(false);
    expect(validateSniffPack('{"format":"fluxdown-sniff-pack"}').ok).toBe(false);
    expect(validateSniffPack("not json").ok).toBe(false);
  });
  test("rule count and size limits", () => {
    const many = `{"format":"fluxdown-sniff-pack","rules":[${Array(501).fill("{}").join(",")}]}`;
    expect(validateSniffPack(many).ok).toBe(false);
    expect(validateSniffPack(new Uint8Array(256 * 1024 + 1).fill(32)).ok).toBe(false);
  });
});

describe("validatePageScriptFiles", () => {
  const script = { id: "demo", js: ["a.js", "b.js"] };
  const files: Record<string, Uint8Array | string> = {
    "a.js": "var a = 1;",
    "b.js": new TextEncoder().encode("var b = 2;"),
  };
  const read = (path: string) => files[path];
  test("concatenates with \\n;\\n", () => {
    const result = validatePageScriptFiles(script, read);
    expect(result.ok).toBe(true);
    expect(result.code).toBe("var a = 1;\n;\nvar b = 2;");
  });
  test("rejects missing, oversized, non-UTF-8 and over-concatenated files", () => {
    expect(validatePageScriptFiles(script, () => undefined).ok).toBe(false);
    expect(validatePageScriptFiles(script, () => new Uint8Array(256 * 1024 + 1).fill(97)).ok).toBe(false);
    expect(validatePageScriptFiles(script, () => new Uint8Array([0xff, 0xfe])).ok).toBe(false);
    // 2 × 200 KiB passes per-file but exceeds the 384 KiB concatenation cap.
    const big = { id: "demo", js: ["a.js", "b.js"] };
    expect(validatePageScriptFiles(big, () => new Uint8Array(200 * 1024).fill(97)).ok).toBe(false);
    expect(validatePageScriptFiles({ id: "demo", js: ["a.js"] }, () => new Uint8Array(200 * 1024).fill(97)).ok).toBe(true);
  });
});

describe("validateViewIcon", () => {
  const enc = (s: string) => new TextEncoder().encode(s);
  test("accepts BOM + whitespace prefix and XML declaration", () => {
    expect(validateViewIcon("v", "\uFEFF \n<SVG viewBox='0 0 24 24'></SVG>").ok).toBe(true);
    expect(validateViewIcon("v", enc('<?xml version="1.0"?>\n<svg xmlns="x"/>')).ok).toBe(true);
  });
  test("rejects non-SVG documents", () => {
    expect(validateViewIcon("v", "<html></html>").errors).toEqual(["view 'v' icon 不是 SVG 文档"]);
    expect(validateViewIcon("v", '<?xml version="1.0"?><root/>').ok).toBe(false);
  });
  test("rejects non-UTF-8", () => {
    expect(validateViewIcon("v", new Uint8Array([0xff, 0xfe, 0x3c])).errors).toEqual(["view 'v' icon 不是合法 UTF-8"]);
  });
  test("size boundary 16384 / 16385", () => {
    const svg = "<svg>";
    const pad = (n: number) => enc(svg + " ".repeat(n - svg.length));
    expect(validateViewIcon("v", pad(16384)).ok).toBe(true);
    expect(validateViewIcon("v", pad(16385)).errors).toEqual(["view 'v' icon 超过 16384 字节"]);
  });
});

describe("validatePackage", () => {
  const base = { identity: "a@b", name: "N", version: "1.0.0", main: { entry: "main.js" } };
  const files = (entries: Record<string, string>) => new Map(Object.entries(entries));

  test("checks the contents of every referenced file, not just their presence", () => {
    const manifest = {
      ...base,
      permissions: ["browserRules", "pageScripts"],
      contributes: {
        views: [{ id: "v", title: "V", location: "activity", icon: "v.svg" }],
        sniffRules: "rules.json",
        pageScripts: [{ id: "p", title: "P", matches: ["https://*.example.com/*"], js: ["p.js"] }],
      },
    };
    const good = files({ "main.js": "", "v.svg": "<svg/>", "rules.json": '{"format":"fluxdown-sniff-pack","rules":[]}', "p.js": "1" });
    expect(validatePackage(manifest, good)).toEqual({ ok: true, errors: [] });
    const bad = files({ "main.js": "", "v.svg": "<html/>", "rules.json": "{}", "p.js": "1" });
    expect(validatePackage(manifest, bad).errors).toEqual(["sniffRules format 须为 fluxdown-sniff-pack", "view 'v' icon 不是 SVG 文档"]);
  });

  test("checks signed archive rules envelope syntax and package contents", () => {
    const payload = JSON.stringify({
      format: "fluxdown-archive-rules",
      schemaVersion: 1,
      sequence: 1,
      rules: [{ id: "series", folderName: "Series", match: { extensions: ["mkv"] } }],
    });
    const seed = JSON.stringify({ payload, signature: "a".repeat(128) });
    expect(validateArchiveRulesPack(seed).ok).toBe(true);
    const manifest = {
      ...base,
      permissions: ["archive"],
      contributes: {
        archiveRules: {
          entry: "rules.json",
          publicKey: "a".repeat(64),
        },
      },
    };
    expect(validatePackage(manifest, files({ "main.js": "", "rules.json": seed }))).toEqual({ ok: true, errors: [] });
    expect(validatePackage(manifest, files({ "main.js": "", "rules.json": "{}" })).ok).toBe(false);
  });

  test("backslash paths resolve to the forward-slash package entry", () => {
    const manifest = { ...base, contributes: { views: [{ id: "v", title: "V", location: "activity", icon: "i\\v.svg" }] } };
    expect(validatePackage(manifest, files({ "main.js": "", "i/v.svg": "<html/>" })).errors).toEqual(["view 'v' icon 不是 SVG 文档"]);
  });

  test("enforces the client's entry-count limit", () => {
    const many = files(Object.fromEntries([["main.js", ""], ...Array.from({ length: 200 }, (_, i) => [`f${i}`, ""])]));
    expect(validatePackage(base, many).errors).toEqual(["包内文件数 201 超过上限 200"]);
  });
});

describe("reviewOf", () => {
  test("collects every URL scope a reviewer approves, sorted and de-duplicated", () => {
    const review = reviewOf({
      permissions: ["intercept", "pageScripts", "archive"],
      main: { imageHosts: ["img.example.com", "same.example.com"] },
      resolvers: [{ match: { urls: ["*://b.example/*"] } }],
      hooks: { match: { urls: ["*://a.example/*", "*://b.example/*"] } },
      interceptors: { match: { urls: ["*://c.example/*"] } },
      contributes: {
        pageScripts: [{ matches: ["https://z.example/*"] }, { matches: ["https://y.example/*"] }],
        views: [{ desktop: { networkHosts: ["same.example.com", "ui.example.com"] } }],
        archive: "handlers/archive.js",
      },
    });
    expect(review).toEqual({
      permissions: ["intercept", "pageScripts", "archive"],
      risk: "full",
      archiveEntry: "handlers/archive.js",
      networkHosts: ["img.example.com", "same.example.com", "ui.example.com"],
      downloadUrls: ["*://a.example/*", "*://b.example/*", "*://c.example/*"],
      pageScriptMatches: ["https://y.example/*", "https://z.example/*"],
    });
  });
});
