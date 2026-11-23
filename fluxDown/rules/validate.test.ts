import { describe, expect, test } from "bun:test";
import type { RulePack, RuleTier, SniffRuleV1 } from "./schema";
import { PackRejectedError, isSafeRegex, validatePack } from "./validate";

function rule(over: Partial<SniffRuleV1> & { id: string; tier: RuleTier }): SniffRuleV1 {
  return {
    version: 1,
    stage: "response",
    match: { ext: ["abc"] },
    action: { kind: "resource", type: "other" },
    ...over,
  } as SniffRuleV1;
}

function pack(packId: RuleTier, rules: SniffRuleV1[]): RulePack {
  return {
    format: "fluxdown-sniff-pack",
    packId,
    sequence: 1,
    engine: 1,
    issuedAt: "2026-01-01T00:00:00Z",
    rules,
  };
}

describe("isSafeRegex", () => {
  test("拒绝反向引用", () => {
    expect(isSafeRegex("(a)\\1")).toBe(false);
    expect(isSafeRegex("(?<n>a)\\k<n>")).toBe(false);
  });
  test("拒绝环视", () => {
    for (const p of ["a(?=b)", "a(?!b)", "(?<=a)b", "(?<!a)b"]) expect(isSafeRegex(p)).toBe(false);
  });
  test("拒绝嵌套量词与超长 / 非法正则", () => {
    expect(isSafeRegex("(a+)+")).toBe(false);
    expect(isSafeRegex("(?:a*)*")).toBe(false);
    expect(isSafeRegex("a".repeat(257))).toBe(false);
    expect(isSafeRegex("(")).toBe(false);
  });
  test("接受普通正则", () => {
    expect(isSafeRegex("[-_](\\d{4,6})\\.m4s(?:[?#]|$)")).toBe(true);
  });
});

describe("validatePack plugin 层", () => {
  const plug = (over: Partial<SniffRuleV1> & { id: string }) =>
    rule({ tier: "plugin", match: { host: ["x.com"], ext: ["abc"] }, ...over });

  test("合法规则保留；空包允许", () => {
    const r = validatePack(pack("plugin", [plug({ id: "plugin.a.b.ok" })]), "plugin");
    expect(r.pack.rules.map((x) => x.id)).toEqual(["plugin.a.b.ok"]);
    expect(validatePack(pack("plugin", []), "plugin").pack.rules).toEqual([]);
  });

  test("缺 host 被剔除", () => {
    const r = validatePack(pack("plugin", [plug({ id: "plugin.a.nohost", match: { ext: ["abc"] } })]), "plugin");
    expect(r.pack.rules).toEqual([]);
  });

  test("preempt 被剔除", () => {
    const r = validatePack(
      pack("plugin", [
        plug({
          id: "plugin.a.pp",
          stage: "body",
          action: {
            kind: "extract",
            fields: { u: "$.u" },
            emit: { url: "{u}", mode: "preempt" },
          } as unknown as SniffRuleV1["action"],
        }),
      ]),
      "plugin",
    );
    expect(r.pack.rules).toEqual([]);
  });

  test("全局 strip-params 被剔除", () => {
    const r = validatePack(
      pack("plugin", [
        plug({ id: "plugin.a.sp", stage: "normalize", match: {}, action: { kind: "strip-params", params: ["x"] } }),
      ]),
      "plugin",
    );
    expect(r.pack.rules).toEqual([]);
  });

  test("headers 规则被剔除并给出原因", () => {
    const r = validatePack(
      pack("plugin", [
        plug({
          id: "plugin.a.hd",
          stage: "request",
          action: { kind: "headers", set: { Referer: "https://x.com/" } },
        }),
      ]),
      "plugin",
    );
    expect(r.pack.rules).toEqual([]);
    expect(r.rejected[0]?.reason).toBe("plugin 规则不得改写请求头");
  });

  test("id 超过 64 字符被剔除", () => {
    const r = validatePack(pack("plugin", [plug({ id: `plugin.${"a".repeat(60)}` })]), "plugin");
    expect(r.pack.rules).toEqual([]);
  });
});

describe("validatePack 单条剔除", () => {
  test("社区规则缺 host 被剔除，带 host 保留", () => {
    const r = validatePack(
      pack("community", [
        rule({ id: "community.aa", tier: "community" }),
        rule({ id: "community.bb", tier: "community", match: { host: ["x.com"] } }),
      ]),
      "community",
    );
    expect(r.pack.rules.map((x) => x.id)).toEqual(["community.bb"]);
    expect(r.rejected.map((x) => x.id)).toEqual(["community.aa"]);
  });

  test("社区规则使用 preempt 被剔除", () => {
    const r = validatePack(
      pack("community", [
        rule({
          id: "community.pp",
          tier: "community",
          stage: "body",
          match: { host: ["x.com"] },
          action: {
            kind: "extract",
            fields: { u: "u" },
            emit: { mode: "preempt", url: "${u}" },
          },
        }),
      ]),
      "community",
    );
    expect(r.pack.rules).toEqual([]);
  });

  test("stage 与 action 不匹配被剔除", () => {
    const r = validatePack(
      pack("local", [rule({ id: "local.aa", tier: "local", stage: "normalize" })]),
      "local",
    );
    expect(r.rejected[0]?.reason).toContain("不匹配");
  });

  test("模板引用未定义字段、id 重复被剔除", () => {
    const body = (id: string, url: string): SniffRuleV1 =>
      rule({
        id,
        tier: "official",
        stage: "body",
        action: { kind: "extract", fields: { u: "u" }, emit: { mode: "resource", url } },
      });
    const r = validatePack(
      pack("official", [body("official.aa", "${nope}"), body("official.bb", "${u}"), body("official.bb", "${u}")]),
      "official",
    );
    expect(r.pack.rules.map((x) => x.id)).toEqual(["official.bb"]);
    expect(r.rejected.length).toBe(2);
  });

  test("dom 规则模板只能引用 value / text / page", () => {
    const dom = (id: string, url: string): SniffRuleV1 =>
      rule({
        id,
        tier: "plugin",
        stage: "dom",
        match: { pageHost: ["v.example.com"] },
        action: { kind: "dom", selector: "video", attr: "src", emit: { mode: "resource", url, type: "video" } },
      });
    const r = validatePack(
      pack("plugin", [dom("plugin.a.page", "${page}"), dom("plugin.a.value", "${value}#${text}"), dom("plugin.a.bad", "${u}")]),
      "plugin",
    );
    expect(r.pack.rules.map((x) => x.id)).toEqual(["plugin.a.page", "plugin.a.value"]);
    expect(r.rejected.map(({ id, reason }) => ({ id, reason }))).toEqual([
      { id: "plugin.a.bad", reason: "模板引用了未定义字段: u" },
    ]);
  });

  test("match / action 字段类型不符被剔除，不进入引擎编译", () => {
    const bad = [
      { ext: "mp4" },
      { url: "https://x/*" },
      { status: 200 },
      { minSize: -1 },
      { hasExt: "yes" },
      { header: { name: "a", regex: "b" } },
    ];
    const rules = bad.map((match, i) =>
      rule({ id: `local.bad${i}`, tier: "local", match: match as never, action: { kind: "resource", type: "video" } }),
    );
    rules.push(
      rule({ id: "local.params", tier: "local", stage: "normalize", action: { kind: "strip-params", params: [1] as never } }),
    );
    const r = validatePack(pack("local", rules), "local");
    expect(r.pack.rules).toEqual([]);
    expect(r.rejected.map((x) => x.id)).toEqual([...bad.map((_, i) => `local.bad${i}`), "local.params"]);
  });

  test("社区全局 strip-params 被剔除", () => {
    const r = validatePack(
      pack("community", [
        rule({
          id: "community.ss",
          tier: "community",
          stage: "normalize",
          match: {},
          action: { kind: "strip-params", params: ["x"] },
        }),
      ]),
      "community",
    );
    expect(r.pack.rules).toEqual([]);
  });
});

describe("validatePack 整包", () => {
  test("packId 不符 / engine 过新 / format 错误抛错", () => {
    expect(() => validatePack(pack("local", []), "official")).toThrow(PackRejectedError);
    expect(() => validatePack({ ...pack("local", []), engine: 2 }, "local")).toThrow(PackRejectedError);
    expect(() => validatePack({ ...pack("local", []), format: "x" } as never, "local")).toThrow(PackRejectedError);
  });
});

describe("page / request / mirror / manifest 规则校验", () => {
  const check = (r: SniffRuleV1) => validatePack(pack(r.tier, [r]), r.tier).rejected;

  test("headers 不允许设置凭据类请求头，且必须限定 host", () => {
    expect(check(rule({ id: "official.h1", tier: "official", stage: "request", match: { host: ["a.com"] }, action: { kind: "headers", set: { Cookie: "x" } } }))).toHaveLength(1);
    expect(check(rule({ id: "official.h2", tier: "official", stage: "request", match: {}, action: { kind: "headers", set: { Referer: "https://a.com/" } } }))).toHaveLength(1);
    expect(check(rule({ id: "official.h3", tier: "official", stage: "request", match: { host: ["a.com"] }, action: { kind: "headers", set: { Referer: "https://a.com/" } } }))).toHaveLength(0);
  });

  test("page 规则：非本地规则必须限定页面；player.float 只接受 hide / inset，inset 的 top 须为 1–200 的整数", () => {
    expect(check(rule({ id: "community.t1", tier: "community", stage: "page", match: {}, action: { kind: "title", selector: ".t" } }))).toHaveLength(1);
    expect(check(rule({ id: "official.p1", tier: "official", stage: "page", match: { pageHost: ["a.com"] }, action: { kind: "player", selector: ".ad", float: "show" as "hide" } }))).toHaveLength(1);
    expect(check(rule({ id: "official.p2", tier: "official", stage: "page", match: { pageHost: ["a.com"] }, action: { kind: "player", selector: ".ad", float: "hide" } }))).toHaveLength(0);
    const inset = (top: unknown) =>
      check(rule({ id: "official.p3", tier: "official", stage: "page", match: { pageHost: ["a.com"] }, action: { kind: "player", selector: ".mini", float: "inset", top: top as number } }));
    expect(inset(52)).toHaveLength(0);
    expect(inset(200)).toHaveLength(0);
    for (const bad of [undefined, 0, 201, 12.5, "52"]) expect(inset(bad).map((r) => r.code)).toEqual(["player_top_invalid"]);
  });

  test("mirror 至少两个合法 host；manifest 的 path 必须是字符串", () => {
    expect(check(rule({ id: "official.m1", tier: "official", stage: "normalize", match: {}, action: { kind: "mirror", hosts: ["a.com"] } }))).toHaveLength(1);
    expect(check(rule({ id: "official.m2", tier: "official", stage: "normalize", match: {}, action: { kind: "mirror", hosts: ["a.com", "b.net"] } }))).toHaveLength(0);
    expect(check(rule({ id: "official.b1", tier: "official", stage: "body", match: { url: ["*/api*"] }, action: { kind: "manifest", path: 3 as unknown as string } }))).toHaveLength(1);
  });

  test("社区规则的 mirror.hosts 必须在 match 声明的站点内", () => {
    const mirror = (hosts: string[]) =>
      check(rule({ id: "community.m1", tier: "community", stage: "normalize", match: { host: ["example.com", "example-cdn.net"] }, action: { kind: "mirror", hosts } }));
    expect(mirror(["a.example.com", "b.example-cdn.net"])).toHaveLength(0);
    expect(mirror(["a.example.com", "evil.test"])).toHaveLength(1);
  });
});

describe("isSafeRegex：ReDoS", () => {
  test("拒绝嵌套括号里的嵌套量词", () => {
    for (const p of ["((a+))+", "((a+)b?)*", "(?:(?:a*)b)+", "(a+){2,}", "((a|b)+)*"]) {
      expect([p, isSafeRegex(p)]).toEqual([p, false]);
    }
  });

  test("拒绝被重复的重叠选择支", () => {
    for (const p of ["(a|aa)+", "(\\w|\\d)+", "(?:a|a)*", "(x|xy|y)+", "(a|)+"]) {
      expect([p, isSafeRegex(p)]).toEqual([p, false]);
    }
  });

  test("拒绝相邻重叠的无界量词", () => {
    for (const p of ["a*a*a*", "a*a*", "\\w+\\d+", ".*.*", "[a-z]+[a-c]*x"]) {
      expect([p, isSafeRegex(p)]).toEqual([p, false]);
    }
  });

  test("拒绝无界量词过多", () => {
    expect(isSafeRegex("a+b+c+d+e+")).toBe(false);
  });

  test("接受常见的无歧义写法", () => {
    for (const p of [
      "\\.(?:mp4|mkv)(?:[?#]|$)",
      "(?:ab|cd)+",
      "^/api/v\\d+/stream/[^/]+$",
      "\\d+\\.\\d+",
      "a+b+",
      ".*\\.m3u8",
      "/(?:log|track|collect)(?:/|$)",
      "(\\d{1,3}\\.){3}\\d{1,3}",
      "x{2,5}y*",
    ]) {
      expect([p, isSafeRegex(p)]).toEqual([p, true]);
    }
  });

  test("字面量大括号与转义不被误判为量词", () => {
    expect(isSafeRegex("a{b}")).toBe(true);
    expect(isSafeRegex("\\(a+\\)+")).toBe(true);
    expect(isSafeRegex("[(a+)]+")).toBe(true);
  });
});

describe("validatePack 限制与稳定代码", () => {
  const codeOf = (r: SniffRuleV1) => {
    const out = validatePack(pack(r.tier, [r]), r.tier).rejected[0];
    return out ? { code: out.code, param: out.param } : null;
  };
  const local = (id: string, match: SniffRuleV1["match"], extra: Partial<SniffRuleV1> = {}) =>
    rule({ id, tier: "local", match, ...extra });
  const many = (n: number, f: (i: number) => string) => Array.from({ length: n }, (_, i) => f(i));

  test("数组与规则体积上限", () => {
    expect(codeOf(local("local.uu", { url: many(65, (i) => `*a${i}*`) }))).toEqual({ code: "too_many", param: "url" });
    expect(codeOf(local("local.u2", { url: many(64, (i) => `*a${i}*`) }))).toBeNull();
    expect(codeOf(local("local.ee", { ext: many(257, (i) => `e${i}`) }))).toEqual({ code: "too_many", param: "ext" });
    expect(codeOf(local("local.mm", { mime: many(65, (i) => `a/${i}`) }))).toEqual({ code: "too_many", param: "mime" });
    expect(codeOf(local("local.hh", { header: many(9, (i) => `h${i}`).map((name) => ({ name, regex: "x" })) }))).toEqual({
      code: "too_many",
      param: "header",
    });
    expect(codeOf(local("local.big", { ext: ["abc"] }, { title: "x".repeat(17 * 1024) }))).toEqual({
      code: "rule_too_large",
      param: undefined,
    });
  });

  test("mirror.hosts ≤16、extract.fields ≤32", () => {
    const mirror = (n: number) =>
      local("local.mi", {}, {
        stage: "normalize",
        action: { kind: "mirror", hosts: many(n, (i) => `h${i}.example.com`) },
      });
    expect(codeOf(mirror(17))).toEqual({ code: "too_many", param: "mirror.hosts" });
    expect(codeOf(mirror(16))).toBeNull();
    const extract = (n: number) =>
      local("local.ex", { url: ["*/api*"] }, {
        stage: "body",
        action: {
          kind: "extract",
          fields: Object.fromEntries(many(n, (i) => `f${i}`).map((name) => [name, `p.${name}`])),
          emit: { mode: "resource", url: "${f0}" },
        },
      });
    expect(codeOf(extract(33))).toEqual({ code: "too_many", param: "fields" });
    expect(codeOf(extract(32))).toBeNull();
  });

  test("社区 / 插件规则的单标签 host 被拒（local 不受限）", () => {
    expect(codeOf(rule({ id: "community.tld", tier: "community", match: { host: ["com"] } }))).toEqual({
      code: "host_too_broad",
      param: "com",
    });
    expect(codeOf(rule({ id: "plugin.a.tld", tier: "plugin", match: { pageHost: ["localhost"] } }))?.code).toBe("host_too_broad");
    expect(codeOf(rule({ id: "community.ok", tier: "community", match: { host: ["example.com"] } }))).toBeNull();
    expect(codeOf(local("local.tld", { host: ["com"] }))).toBeNull();
    // exclude 只会收窄命中范围，不受限
    expect(codeOf(rule({ id: "community.ex", tier: "community", match: { host: ["example.com"] }, exclude: [{ host: ["com"] }] }))).toBeNull();
  });

  test("headers：必须有 match.host，名字走白名单", () => {
    const hdr = (id: string, match: SniffRuleV1["match"], set: Record<string, string>) =>
      rule({ id, tier: "official", stage: "request", match, action: { kind: "headers", set } });
    expect(codeOf(hdr("official.h1", { pageHost: ["a.com"] }, { Referer: "https://a.com/" }))?.code).toBe("headers_host_required");
    expect(codeOf(hdr("official.h2", { host: ["a.com"] }, { Referer: "https://a.com/", Origin: "https://a.com", "User-Agent": "x" }))).toBeNull();
    for (const name of ["Range", "Cookie", "Transfer-Encoding", "Sec-Fetch-Mode", "If-None-Match", "X-Http-Method-Override", "Proxy-Foo"]) {
      expect(codeOf(hdr("official.h3", { host: ["a.com"] }, { [name]: "x" }))).toEqual({ code: "header_not_allowed", param: name });
    }
    expect(codeOf(hdr("official.h4", { host: ["a.com"] }, { Referer: "a", referer: "b" }))?.code).toBe("header_name_invalid");
    expect(codeOf(hdr("official.h5", { host: ["a.com"] }, { Referer: "a\r\nX: y" }))?.code).toBe("header_value_invalid");
  });

  test("被拒条目带稳定代码与参数；exclude 内的问题保留内层代码", () => {
    expect(codeOf(local("local.re", { pathRegex: "(a+)+" }))).toEqual({ code: "unsafe_regex", param: "pathRegex" });
    expect(codeOf(local("local.ty", { ext: "mp4" as never }))).toEqual({ code: "string_array_required", param: "ext" });
    expect(codeOf(local("local.st", { status: 5 as never }))).toEqual({ code: "status_invalid", param: "status" });
    expect(codeOf(local("local.hp", { host: ["a b"] }))?.code).toBe("host_invalid");
    const withExclude = validatePack(pack("local", [local("local.exc", { ext: ["a"] }, { exclude: [{ ext: "x" as never }] })]), "local");
    expect(withExclude.rejected[0]).toMatchObject({ code: "string_array_required", param: "ext", reason: "exclude: ext 须为字符串数组" });
    const dup = validatePack(pack("local", [local("local.dd", { ext: ["a"] }), local("local.dd", { ext: ["a"] })]), "local");
    expect(dup.rejected[0]).toMatchObject({ code: "id_duplicate" });
    expect(codeOf(rule({ id: "BAD", tier: "local" }))?.code).toBe("id_invalid");
    expect(codeOf(rule({ id: "official.x1", tier: "local" }))?.code).toBe("id_prefix_mismatch");
  });

  test("整包拒绝带 code", () => {
    try {
      validatePack(pack("local", []), "official");
      throw new Error("应抛出");
    } catch (e) {
      expect(e).toBeInstanceOf(PackRejectedError);
      expect((e as PackRejectedError).code).toBe("pack_tier_mismatch");
    }
  });
});
