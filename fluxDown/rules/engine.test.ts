import { describe, expect, test } from "bun:test";
import cases from "./fixtures/engine-cases.json";
import {
  BUILTIN_PACK,
  BUNDLED_OFFICIAL_PACK,
  compileBundled,
  compileRules,
  createClassifier,
  effectiveOfficialPack,
  type CompiledRules,
} from "./engine";
import type { RuleOverrides, RulePack, RevocationDoc } from "./schema";
import { TIER_RANK } from "./schema";
import { validatePack } from "./validate";

interface Setup {
  packs?: RulePack[];
  overrides?: RuleOverrides;
  revocations?: RevocationDoc | null;
  options?: { sniffImages?: boolean; communityEnabled?: boolean; now?: number };
}

interface Case {
  name: string;
  setup?: Setup;
  call: Exclude<keyof CompiledRules, "listRules" | "knownExtensions" | "classifyRules">;
  args: unknown[];
  expect: unknown;
}

const DEFAULT_NOW = Date.parse("2026-01-01T00:00:00Z");

function build(setup: Setup = {}): CompiledRules {
  return compileRules({
    packs: [BUILTIN_PACK, BUNDLED_OFFICIAL_PACK, ...(setup.packs ?? [])].map((pack) => ({ tier: pack.packId, pack })),
    overrides: setup.overrides ?? {},
    revocations: setup.revocations ?? null,
    options: {
      sniffImages: setup.options?.sniffImages ?? false,
      communityEnabled: setup.options?.communityEnabled ?? true,
      now: setup.options?.now ?? DEFAULT_NOW,
    },
  });
}

describe("engine fixtures", () => {
  for (const c of cases as unknown as Case[]) {
    test(c.name, () => {
      const engine = build(c.setup);
      const fn = engine[c.call] as (...a: unknown[]) => unknown;
      const actual = fn.apply(engine, c.args);
      if (c.expect !== null && typeof c.expect === "object") {
        expect(actual).toMatchObject(c.expect as object);
      } else {
        expect(actual).toEqual(c.expect);
      }
    });
  }
});

describe("随包规则", () => {
  test("builtin 与官方包快照自身通过校验且无剔除", () => {
    for (const pack of [BUILTIN_PACK, BUNDLED_OFFICIAL_PACK]) {
      const { rejected, pack: checked } = validatePack(pack, pack.packId);
      expect(rejected).toEqual([]);
      expect(checked.rules.length).toBe(pack.rules.length);
    }
  });

  test("compileBundled 全部规则生效", () => {
    const list = compileBundled().listRules();
    expect(list.length).toBe(BUILTIN_PACK.rules.length + BUNDLED_OFFICIAL_PACK.rules.length);
    expect(list.every((e) => e.status === "active")).toBe(true);
  });

  test("knownExtensions 含 mp4 与 jpg", () => {
    const known = compileBundled().knownExtensions();
    expect(known.has("mp4")).toBe(true);
    expect(known.has("jpg")).toBe(true);
  });

  test("远程官方包只有 sequence 不低于快照时才取代快照", () => {
    const at = (sequence: number): RulePack => ({ ...BUNDLED_OFFICIAL_PACK, sequence, rules: [] });
    expect(effectiveOfficialPack(undefined)).toBe(BUNDLED_OFFICIAL_PACK);
    const older = at(BUNDLED_OFFICIAL_PACK.sequence - 1);
    expect(effectiveOfficialPack(older)).toBe(BUNDLED_OFFICIAL_PACK);
    const same = at(BUNDLED_OFFICIAL_PACK.sequence);
    expect(effectiveOfficialPack(same)).toBe(same);
    const newer = at(BUNDLED_OFFICIAL_PACK.sequence + 1);
    expect(effectiveOfficialPack(newer)).toBe(newer);
  });
});

describe("listRules 状态", () => {
  test("engine 过新的包整包标记 engine-too-new", () => {
    const pack = {
      ...BUILTIN_PACK,
      packId: "official",
      engine: 99,
      rules: [{ ...BUILTIN_PACK.rules[0], id: "official.future", tier: "official" }],
    } as unknown as RulePack;
    const list = compileRules({
      packs: [{ tier: "official", pack }],
      overrides: {},
      revocations: null,
      options: { sniffImages: false, communityEnabled: true, now: DEFAULT_NOW },
    }).listRules();
    expect(list.map((e) => e.status)).toEqual(["engine-too-new"]);
  });

  const localPack = (rules: unknown[]): RulePack =>
    ({ format: "fluxdown-sniff-pack", packId: "local", sequence: 0, engine: 1, issuedAt: "2026-01-01T00:00:00Z", rules }) as RulePack;
  const good = { id: "local.ok", version: 1, tier: "local", stage: "response", match: { ext: ["abc"] }, action: { kind: "resource", type: "other" } };

  test("单条畸形规则列为 rejected 并带原因，其余规则照常生效", () => {
    const bad = { ...good, id: "local.bad", match: { ext: "abc" } };
    const engine = build({ packs: [localPack([good, bad, 42])] });
    const local = engine.listRules().filter((e) => e.rule.tier === "local");
    expect(local.map((e) => [e.rule.id, e.status])).toEqual([
      ["local.bad", "rejected"],
      ["local.#3", "rejected"],
      ["local.ok", "active"],
    ]);
    expect(local[0].detail).toBe("ext 须为字符串数组");
    expect(engine.classifyResponse({ url: "https://x.com/a.abc" }).decision).toBe("resource");
  });

  test("整包被拒（超过数量上限）时逐条列为 rejected，不静默消失", () => {
    const rules = Array.from({ length: 2001 }, (_, i) => ({ ...good, id: `local.r${i}` }));
    const local = build({ packs: [localPack(rules)] }).listRules().filter((e) => e.rule.tier === "local");
    expect(local.length).toBe(2001);
    expect(local.every((e) => e.status === "rejected" && e.detail === "rules 非法或超过上限")).toBe(true);
  });

  test("社区规则关闭为 paused，吊销列表缺失为 expired + revocations-stale", () => {
    const community = {
      format: "fluxdown-sniff-pack", packId: "community", sequence: 1, engine: 1, issuedAt: "2026-01-01T00:00:00Z",
      rules: [{ ...good, id: "community.xy", tier: "community", match: { host: ["x.com"], ext: ["abc"] } }],
    } as unknown as RulePack;
    const off = build({ packs: [community], options: { communityEnabled: false } }).listRules();
    expect(off.find((e) => e.rule.id === "community.xy")).toMatchObject({ status: "paused" });
    const stale = build({ packs: [community] }).listRules();
    expect(stale.find((e) => e.rule.id === "community.xy")).toMatchObject({ status: "expired", reason: "revocations-stale" });
  });

  test("吊销条目缺少 versions 时不命中且不抛错", () => {
    const revocations = {
      format: "fluxdown-sniff-revocations", sequence: 1, issuedAt: "2026-01-01T00:00:00Z", expiresAt: "2027-01-01T00:00:00Z",
      entries: [{ ruleId: "local.ok", action: "hard", reason: "x" }],
    } as unknown as RevocationDoc;
    const list = build({ packs: [localPack([good])], revocations }).listRules();
    expect(list.find((e) => e.rule.id === "local.ok")?.status).toBe("active");
  });
});

describe("plugin 层级", () => {
  test("优先级：local < official < plugin < community < builtin", () => {
    expect(TIER_RANK.local).toBeLessThan(TIER_RANK.official);
    expect(TIER_RANK.official).toBeLessThan(TIER_RANK.plugin);
    expect(TIER_RANK.plugin).toBeLessThan(TIER_RANK.community);
    expect(TIER_RANK.community).toBeLessThan(TIER_RANK.builtin);
  });

  test("plugin 包规则可编译并生效", () => {
    const pack = {
      format: "fluxdown-sniff-pack",
      packId: "plugin",
      sequence: 1,
      engine: 1,
      issuedAt: "2026-01-01T00:00:00Z",
      rules: [
        { id: "plugin.a.b.r1", version: 1, tier: "plugin", stage: "response", match: { host: ["x.com"], ext: ["abc"] }, action: { kind: "resource", type: "other" } },
      ],
    } as unknown as RulePack;
    const list = build({ packs: [pack] }).listRules();
    const entry = list.find((e) => e.rule.id === "plugin.a.b.r1");
    expect(entry?.status).toBe("active");
  });
});

describe("page / request / mirror / manifest 规则", () => {
  const local = (rules: unknown[]): RulePack =>
    ({ format: "fluxdown-sniff-pack", packId: "local", sequence: 1, engine: 1, issuedAt: "2026-01-01T00:00:00Z", rules }) as RulePack;
  const engine = build({
    packs: [local([
      { id: "local.title", version: 1, tier: "local", stage: "page", match: { pageHost: ["v.example.com"] }, action: { kind: "title", selector: ".t", attr: "data-name" } },
      { id: "local.ad", version: 1, tier: "local", stage: "page", match: { pageHost: ["v.example.com"] }, action: { kind: "player", selector: ".ad", float: "hide" } },
      { id: "local.mini", version: 1, tier: "local", stage: "page", match: { pageHost: ["v.example.com"] }, action: { kind: "player", selector: ".mini", float: "inset", top: 52 } },
      { id: "local.ref", version: 1, tier: "local", stage: "request", match: { host: ["cdn.example.com"] }, action: { kind: "headers", set: { Referer: "https://v.example.com/" } } },
      { id: "local.mirror", version: 1, tier: "local", stage: "normalize", match: {}, action: { kind: "mirror", hosts: ["cdn.example.com", "bak.example.net"] } },
      { id: "local.api", version: 1, tier: "local", stage: "body", match: { url: ["*/api/play*"] }, action: { kind: "manifest", path: "data.streams" } },
    ])],
  });

  test("pageHints 只在匹配的页面生效", () => {
    expect(engine.pageHints("https://v.example.com/watch/1")).toEqual({
      titles: [{ selector: ".t", attr: "data-name" }],
      players: [{ selector: ".ad" }],
      insets: [{ selector: ".mini", top: 52 }],
    });
    expect(engine.pageHints("https://other.example.org/")).toEqual({ titles: [], players: [], insets: [] });
  });

  test("requestHeaders 按 URL host 匹配，名字小写", () => {
    expect(engine.requestHeaders("https://cdn.example.com/a.mp4", "https://v.example.com/")).toEqual({ referer: "https://v.example.com/" });
    expect(engine.requestHeaders("https://elsewhere.com/a.mp4", "https://v.example.com/")).toEqual({});
  });

  test("mirrorGroups 与 manifestsFromBody", () => {
    expect(engine.mirrorGroups()).toEqual([["cdn.example.com", "bak.example.net"]]);
    const body = { data: { streams: { video: [{ baseUrl: "https://cdn.example.com/1.mp4" }] } } };
    expect(engine.manifestsFromBody("https://v.example.com/api/play?id=1", "https://v.example.com/", body)).toEqual([
      body.data.streams,
    ]);
    expect(engine.bodyWatchers("https://v.example.com/").some((w) => w.rule.id === "local.api")).toBe(true);
  });
});

describe("社区规则的站点范围", () => {
  const community = (rules: unknown[]): RulePack =>
    ({ format: "fluxdown-sniff-pack", packId: "community", sequence: 1, engine: 1, issuedAt: "2026-01-01T00:00:00Z", expiresAt: "2027-01-01T00:00:00Z", rules }) as RulePack;
  const revocations = { format: "fluxdown-sniff-revocations", sequence: 1, issuedAt: "2026-01-01T00:00:00Z", expiresAt: "2027-01-01T00:00:00Z", entries: [] } as RevocationDoc;
  const engine = build({
    revocations,
    packs: [community([
      { id: "community.a.api", version: 1, tier: "community", stage: "body", match: { host: ["example.com", "example-cdn.net"] }, action: { kind: "extract", fields: { u: "u" }, emit: { mode: "resource", url: "${u}" } } },
      { id: "community.a.link", version: 1, tier: "community", stage: "dom", match: { pageHost: ["example.com"] }, action: { kind: "dom", selector: "a", attr: "href", emit: { mode: "resource", url: "${value}" } } },
    ])],
  });

  test("extract 只产出作用站点内的地址", () => {
    const urls = (u: string) =>
      engine.extractFromBody("https://example.com/api", "https://example.com/", { u }).flatMap((x) => x.emits.map((e) => e.url));
    expect(urls("https://dl.example-cdn.net/a.zip")).toEqual(["https://dl.example-cdn.net/a.zip"]);
    expect(urls("https://evil.test/a.zip")).toEqual([]);
  });

  test("dom 规则同样受限", () => {
    const rule = engine.domRules("https://example.com/p")[0];
    const render = (value: string) => engine.renderDomEmit(rule.rule, rule.emit, { value, text: "", page: "https://example.com/p" });
    expect(render("https://www.example.com/a.zip")?.url).toBe("https://www.example.com/a.zip");
    expect(render("https://evil.test/a.zip")).toBeNull();
  });
});

describe("评估顺序 / inert / explainResponse", () => {
  const local = (rules: unknown[]): RulePack =>
    ({ format: "fluxdown-sniff-pack", packId: "local", sequence: 1, engine: 1, issuedAt: "2026-01-01T00:00:00Z", rules }) as RulePack;
  const res = (id: string, extra: Record<string, unknown>) => ({
    id, version: 1, tier: "local", stage: "response", match: { ext: ["zzz"] }, action: { kind: "resource", type: "archive" }, ...extra,
  });
  const input = { url: "https://x.com/a.zzz", pageUrl: "", size: 0, attachment: false };

  test("evalOrder 按 tier → priority → 包内顺序编号，仅 active 规则带", () => {
    const engine = build({
      overrides: { "local.off": { enabled: false } },
      packs: [local([res("local.low", { priority: 1 }), res("local.high", { priority: 9 }), res("local.off", {})])],
    });
    const list = engine.listRules();
    const byId = (id: string) => list.find((e) => e.rule.id === id);
    expect(byId("local.high")?.evalOrder).toBe(0);
    expect(byId("local.low")?.evalOrder).toBe(1);
    expect(byId("local.off")?.evalOrder).toBeUndefined();
    const active = list.filter((e) => e.status === "active").map((e) => e.evalOrder);
    expect(new Set(active).size).toBe(active.length);
    expect((byId("builtin.ext.video")?.evalOrder ?? 0) > (byId("local.low")?.evalOrder ?? 99)).toBe(true);
  });

  test("inert：classifyOnly 与未开启 sniffImages 的 requires 规则", () => {
    const engine = build({
      packs: [local([res("local.co", { classifyOnly: true }), res("local.rq", { requires: "sniffImages" }), res("local.ok", {})])],
    });
    const inert = (id: string) => engine.listRules().find((e) => e.rule.id === id)?.inert;
    expect(inert("local.co")).toBe("classify-only");
    expect(inert("local.rq")).toBe("requires-setting");
    expect(inert("local.ok")).toBeUndefined();
    const withImages = build({ options: { sniffImages: true }, packs: [local([res("local.rq", { requires: "sniffImages" })])] });
    expect(withImages.listRules().find((e) => e.rule.id === "local.rq")?.inert).toBeUndefined();
  });

  test("explainResponse 列出全部命中规则，第一条是 winner 且与 classifyResponse 一致", () => {
    const engine = build({
      packs: [local([
        res("local.first", { priority: 5 }),
        res("local.second", { action: { kind: "ignore" } }),
        res("local.inert", { classifyOnly: true }),
      ])],
    });
    const explained = engine.explainResponse(input);
    expect(explained.map((e) => [e.ref.id, e.decision, e.winner])).toEqual([
      ["local.first", "resource", true],
      ["local.second", "ignore", false],
    ]);
    expect(engine.classifyResponse(input)).toMatchObject({ decision: "resource", rule: { id: "local.first" } });
    expect(engine.explainResponse({ ...input, url: "https://x.com/a.nomatch" })).toEqual([]);
  });

  test("被拒与编译失败条目带稳定 code / param", () => {
    const engine = build({ packs: [local([res("local.bad", { match: { ext: "x" } }), res("local.re", { match: { pathRegex: "(a+)+" } })])] });
    const rejected = engine.listRules().filter((e) => e.status === "rejected");
    expect(rejected.map((e) => [e.rule.id, e.code, e.param])).toEqual([
      ["local.bad", "string_array_required", "ext"],
      ["local.re", "unsafe_regex", "pathRegex"],
    ]);
  });
});

describe("槽位层级与随包规则", () => {
  test("按存储槽位校验层级：自报 official 的包放进 community 槽位整包被拒", () => {
    const forged = {
      format: "fluxdown-sniff-pack", packId: "official", sequence: 1, engine: 1, issuedAt: "2026-01-01T00:00:00Z",
      rules: [{ id: "official.forged", version: 1, tier: "official", stage: "response", match: { ext: ["zzz"] }, action: { kind: "resource", type: "other" } }],
    } as unknown as RulePack;
    const engine = compileRules({
      packs: [{ tier: "community", pack: forged }],
      overrides: {},
      revocations: null,
      options: { sniffImages: false, communityEnabled: true, now: DEFAULT_NOW },
    });
    expect(engine.listRules().map((e) => [e.status, e.code])).toEqual([["rejected", "pack_tier_mismatch"]]);
    expect(engine.classifyResponse({ url: "https://x.com/a.zzz", pageUrl: "", size: 0, attachment: false }).decision).toBe("none");
  });

  test("compileBundled 按 sniffImages 缓存单例；被改动的随包副本仍会校验", () => {
    expect(compileBundled()).toBe(compileBundled());
    expect(compileBundled({ sniffImages: true })).not.toBe(compileBundled());
    const tampered = { ...BUILTIN_PACK, rules: [{ ...BUILTIN_PACK.rules[0], id: "BAD" }] } as RulePack;
    const list = compileRules({
      packs: [{ tier: "builtin", pack: tampered }],
      overrides: {},
      revocations: null,
      options: { sniffImages: false, communityEnabled: true, now: DEFAULT_NOW },
    }).listRules();
    expect(list[0]).toMatchObject({ status: "rejected", code: "id_invalid" });
  });

  test("classifyRules 序列化后还原的 classifier 与引擎 classify 一致，并反映用户覆盖", () => {
    const samples: [string, string | undefined][] = [
      ["https://x/a.mp4", "audio/mpeg"], ["https://x/a.m4s", "video/mp4"], ["https://x/noext", "video/mp4"],
      ["https://x/noext", "application/pdf"], ["https://x/a.zip", "application/octet-stream"], ["https://x/a.png", undefined],
      ["https://x/a.unknownext", "text/html"], ["", "application/vnd.apple.mpegurl"],
    ];
    const engine = build();
    const restored = createClassifier(JSON.parse(JSON.stringify(engine.classifyRules())));
    for (const [url, mime] of samples) expect([url, mime, restored(url, mime)]).toEqual([url, mime, engine.classify(url, mime)]);

    const off = build({ overrides: { "builtin.ext.video": { enabled: false } } });
    expect(off.classify("https://x/a.mp4")).toBe("other");
    expect(createClassifier(off.classifyRules())("https://x/a.mp4")).toBe("other");
  });
});

describe("classify 与 classifyResponse 求值顺序一致", () => {
  test("同一 (url, mime) 下 resource 结论相同（sniffImages 开启以覆盖图片规则）", () => {
    const engine = build({ options: { sniffImages: true } });
    const urls = ["https://x/a.mp4", "https://x/a.m4s", "https://x/a.zip", "https://x/a.png", "https://x/noext", "https://x/a.bin", "https://x/i.m3u8"];
    const mimes = [undefined, "video/mp4", "audio/mpeg", "application/octet-stream", "application/vnd.apple.mpegurl", "image/png", "application/pdf"];
    for (const url of urls) {
      for (const mime of mimes) {
        const d = engine.classifyResponse({ url, pageUrl: "", mime, size: 0, attachment: false });
        if (d.decision === "resource") expect([url, mime, d.type]).toEqual([url, mime, engine.classify(url, mime)]);
      }
    }
  });

  test("扩展名规则先于 MIME 规则：a.m4s + video/mp4 → stream", () => {
    const engine = build();
    expect(engine.classify("https://x/a.m4s", "video/mp4")).toBe("stream");
    expect(engine.classifyResponse({ url: "https://x/a.m4s", pageUrl: "", mime: "video/mp4", size: 0, attachment: false })).toMatchObject({ type: "stream" });
  });
});

describe("命中语义", () => {
  const pack = (tier: "local" | "plugin", rules: unknown[]): RulePack =>
    ({ format: "fluxdown-sniff-pack", packId: tier, sequence: 1, engine: 1, issuedAt: "2026-01-01T00:00:00Z", rules }) as RulePack;

  test("非官方正则只作用于 ≤2048 的输入，超长视为不匹配", () => {
    const rule = { id: "local.rx", version: 1, tier: "local", stage: "response", match: { pathRegex: "zzz$" }, action: { kind: "resource", type: "archive" } };
    const engine = build({ packs: [pack("local", [rule])] });
    const at = (n: number) => engine.classifyResponse({ url: `https://x.com/${"a".repeat(n)}zzz`, pageUrl: "", size: 0, attachment: false }).decision;
    expect(at(2000)).toBe("resource");
    expect(at(2100)).toBe("none");
  });

  test("plugin 规则产出的地址同样限制在声明站点内", () => {
    const plugin = pack("plugin", [
      { id: "plugin.a.api", version: 1, tier: "plugin", stage: "body", match: { host: ["example.com"] }, action: { kind: "extract", fields: { u: "u" }, emit: { mode: "resource", url: "${u}" } } },
      { id: "plugin.a.link", version: 1, tier: "plugin", stage: "dom", match: { pageHost: ["example.com"] }, action: { kind: "dom", selector: "a", attr: "href", emit: { mode: "resource", url: "${value}" } } },
    ]);
    const engine = build({ packs: [plugin] });
    const urls = (u: string) => engine.extractFromBody("https://example.com/api", "https://example.com/", { u }).flatMap((x) => x.emits.map((e) => e.url));
    expect(urls("https://cdn.example.com/a.zip")).toEqual(["https://cdn.example.com/a.zip"]);
    expect(urls("https://evil.test/a.zip")).toEqual([]);
    const dom = engine.domRules("https://example.com/p")[0];
    const render = (value: string) => engine.renderDomEmit(dom.rule, dom.emit, { value, text: "", page: "https://example.com/p" });
    expect(render("https://evil.test/a.zip")).toBeNull();
    expect(render("https://example.com/a.zip")?.url).toBe("https://example.com/a.zip");
  });

  test("host 带末尾点与路径参数的 URL 仍按规则命中", () => {
    const engine = build();
    const ignored = engine.classifyResponse({ url: "https://www.google-analytics.com./collect", pageUrl: "", size: 0, attachment: false });
    expect(ignored.decision).toBe("ignore");
    expect(engine.classifyResponse({ url: "https://x.com/a.mp4;jsessionid=ABC?x=1", pageUrl: "", size: 0, attachment: false })).toMatchObject({ decision: "resource", type: "video" });
  });

  test("fileExtension：前一条 filename 规则无结果时继续尝试后面的", () => {
    const first = { id: "local.fe1", version: 1, tier: "local", stage: "normalize", match: { host: ["x.com"] }, action: { kind: "filename", extFromQuery: ["nothing"] } };
    const engine = build({ packs: [pack("local", [first])] });
    expect(engine.fileExtension("https://x.com/dl?format=jpg", "")).toBe("jpg");
  });

  test("trackKind：文件名取不到编号时回看 URL；URL 线索只认 mime= 查询参数", () => {
    const engine = build();
    const input = { pageUrl: "https://www.bilibili.com/video/BV1" };
    expect(engine.trackKind({ ...input, url: "https://upos.bilivideo.com/x/100-30280.m4s", filename: "renamed.m4s" })).toBe("audio");
    expect(engine.trackKind({ ...input, url: "https://other.example/audio/x.bin", pageUrl: "" })).toBeNull();
    expect(engine.trackKind({ pageUrl: "", url: "https://other.example/video/foo.mp3" })).toBeNull();
    expect(engine.trackKind({ pageUrl: "", url: "https://r.googlevideo.com/p?mime=audio%2Fmp4" })).toBe("audio");
    expect(engine.trackKind({ pageUrl: "", url: "https://r.googlevideo.com/p?mime=video%2Fmp4" })).toBe("video");
  });

  test("dedupeKey：作用域 strip 只对命中的 URL 生效，不污染全局集合", () => {
    const scoped = { id: "local.sp", version: 1, tier: "local", stage: "normalize", match: { host: ["cdn.example.com"] }, action: { kind: "strip-params", params: ["sig"] } };
    const engine = build({ packs: [pack("local", [scoped])] });
    expect(engine.dedupeKey("https://cdn.example.com/a?sig=1&q=2", "")).toBe("https://cdn.example.com/a?q=2");
    expect(engine.dedupeKey("https://other.example.com/a?sig=1&q=2", "")).toBe("https://other.example.com/a?q=2&sig=1");
    expect(engine.dedupeKey("https://other.example.com/a?utm_source=x&q=2", "")).toBe("https://other.example.com/a?q=2");
  });
});
