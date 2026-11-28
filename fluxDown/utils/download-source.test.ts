import { describe, expect, test } from "bun:test";
import {
  ServiceWorkerScopes,
  browserOnlyReason,
  isReplayableUrl,
  observedBrowserOriginalUrl,
  scopeBelongsTo,
  type NetworkEvidence,
} from "./download-source";

const none = (): NetworkEvidence => "none";
const evidence = (map: Record<string, NetworkEvidence>) => (url: string): NetworkEvidence => map[url] ?? "none";

describe("observedBrowserOriginalUrl", () => {
  const original = "https://example.com/export?key=1";
  const final = "http://cdn.example.com/file.zip";

  test("keeps the first URL for a distinct observed HTTP(S) target", () => {
    expect(observedBrowserOriginalUrl(original, final)).toBe(original);
  });

  test("omits absent, unchanged, invalid, and non-HTTP(S) targets", () => {
    for (const target of [undefined, original, "not a URL", "ftp://example.com/file.zip"]) {
      expect(observedBrowserOriginalUrl(original, target)).toBeUndefined();
    }
    expect(observedBrowserOriginalUrl("data:,file", final)).toBeUndefined();
  });
});

describe("browserOnlyReason: local schemes", () => {
  const scopes = new ServiceWorkerScopes();
  test.each([
    "blob:https://example.com/0f1e2d3c",
    "data:application/zip;base64,UEsDBA==",
    "filesystem:https://mega.nz/temporary/big.zip",
    "chrome-extension://abcdefghijklmnop/export.zip",
    "file:///Users/me/a.zip",
    "not a url",
  ])("%s stays with the browser", (url) => {
    expect(browserOnlyReason([url], scopes, none)).toBe("local-scheme");
  });

  test("a local final URL wins over a replayable original URL", () => {
    expect(browserOnlyReason(["https://example.com/a.zip", "blob:https://example.com/x"], scopes, none)).toBe(
      "local-scheme",
    );
  });

  test("http(s) / ftp outside any SW scope are replayable without network evidence", () => {
    for (const url of ["https://example.com/a.zip", "http://example.com/a.zip", "ftp://example.com/a.zip"]) {
      expect(isReplayableUrl(url)).toBe(true);
      expect(browserOnlyReason([url, undefined, ""], scopes, none)).toBeNull();
    }
  });
});

describe("browserOnlyReason: service worker scopes", () => {
  const scopes = new ServiceWorkerScopes();
  scopes.add("https://jimmywarting.github.io/StreamSaver.js/");
  scopes.add("https://web.telegram.org/k/");

  test("SW-scoped download without any network request is synthesized in the browser", () => {
    const url = "https://jimmywarting.github.io/StreamSaver.js/392847/file.zip";
    expect(browserOnlyReason([url], scopes, none)).toBe("service-worker");
  });

  test("SW pass-through to the network keeps the download replayable", () => {
    const url = "https://web.telegram.org/k/assets/app.zip";
    expect(browserOnlyReason([url], scopes, evidence({ [url]: "network" }))).toBeNull();
  });

  test("a redirect target with network evidence makes the scoped original replayable", () => {
    const original = "https://web.telegram.org/k/get/abc";
    const final = "https://cdn.example.com/abc.zip";
    expect(browserOnlyReason([original, final], scopes, evidence({ [final]: "network" }))).toBeNull();
  });

  test("an explicitly synthetic response overrides network evidence on another URL", () => {
    const original = "https://web.telegram.org/k/d/abc";
    const final = "https://web.telegram.org/k/d/abc?x=1";
    expect(
      browserOnlyReason([original, final], scopes, evidence({ [original]: "network", [final]: "synthetic" })),
    ).toBe("service-worker");
  });

  test("scope match is a path prefix on the same origin, not a host match", () => {
    expect(browserOnlyReason(["https://web.telegram.org/a/file.zip"], scopes, none)).toBeNull();
    expect(browserOnlyReason(["https://jimmywarting.github.io/other/file.zip"], scopes, none)).toBeNull();
    expect(browserOnlyReason(["https://web.telegram.org.evil.com/k/file.zip"], scopes, none)).toBeNull();
  });
});

describe("ServiceWorkerScopes", () => {
  test("normalizes query/hash away and rejects non-http scopes", () => {
    const scopes = new ServiceWorkerScopes();
    expect(scopes.add("https://example.com/app/?v=2#x")).toBe(true);
    expect(scopes.add("https://example.com/app/")).toBe(false);
    expect(scopes.add("chrome-extension://abc/")).toBe(false);
    expect(scopes.toJSON()).toEqual(["https://example.com/app/"]);
  });

  test("evicts the oldest scope beyond capacity and survives a persistence round trip", () => {
    const scopes = new ServiceWorkerScopes(2);
    scopes.add("https://a.example/");
    scopes.add("https://b.example/");
    scopes.add("https://c.example/");
    const restored = ServiceWorkerScopes.from(JSON.parse(JSON.stringify(scopes)), 2);
    expect(restored.covers("https://a.example/x.zip")).toBe(false);
    expect(restored.covers("https://b.example/x.zip")).toBe(true);
    expect(restored.covers("https://c.example/x.zip")).toBe(true);
  });

  test("restoring ignores malformed storage", () => {
    expect(ServiceWorkerScopes.from({ bogus: true }).toJSON()).toEqual([]);
    expect(ServiceWorkerScopes.from([42, "nope", "https://ok.example/"]).toJSON()).toEqual(["https://ok.example/"]);
  });
});

describe("scopeBelongsTo", () => {
  test("accepts only scopes on the reporting frame's origin", () => {
    expect(scopeBelongsTo("https://drive.example/sw/", "https://drive.example/folder/1")).toBe(true);
    expect(scopeBelongsTo("https://github.com/", "https://evil.example/")).toBe(false);
    expect(scopeBelongsTo("https://drive.example/", "about:blank")).toBe(false);
    expect(scopeBelongsTo("blob:https://drive.example/x", "https://drive.example/")).toBe(false);
  });
});
