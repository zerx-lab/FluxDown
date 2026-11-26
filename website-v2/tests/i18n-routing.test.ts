import { describe, expect, test } from "bun:test";
import { LANGS, matchLangTag } from "../src/i18n/config";
import { parseAcceptLanguage } from "../src/lib/locale-detect";
import { getFallbackPathnames, getFallbackSlugs } from "../src/lib/docs-fallback";

// 例外于「静态 import」:base.ts 在模块加载时读 Astro 注入的 import.meta.env.BASE_URL,
// 静态 import 会被提升到赋值之前;bun 下必须先给根部署值再动态加载依赖它的模块。
process.env.BASE_URL = "/";
const { href, langFromPath, langStaticPaths, resolveLang, stripLang, switchLang } = await import("../src/i18n/routing");
const { docsAlternates } = await import("../src/components/docs/meta");

describe("语言路由", () => {
  test("langStaticPaths 为每种语言产出一条路径,en 无前缀", () => {
    const paths = langStaticPaths();
    expect(paths.length).toBe(LANGS.length);
    expect(paths.find((p) => p.params.lang === undefined)).toBeDefined();
    for (const lang of LANGS) {
      if (lang !== "en") expect(paths.some((p) => p.params.lang === lang)).toBe(true);
    }
  });

  test("resolveLang 未知值回退 en", () => {
    expect(resolveLang(undefined)).toBe("en");
    expect(resolveLang("fr")).toBe("en");
    expect(resolveLang("zh-tw")).toBe("zh-tw");
  });

  test("href 给非默认语言加前缀,zh 与 zh-tw 互不吞并", () => {
    expect(href("/download", "en")).toBe("/download/");
    expect(href("/download", "zh")).toBe("/zh/download/");
    expect(href("/download", "zh-tw")).toBe("/zh-tw/download/");
    expect(href("/", "es")).toBe("/es/");
    expect(href("/#features", "ru")).toBe("/ru/#features");
    expect(href("/docs/tr/", "tr")).toBe("/docs/tr/");
    expect(href("/api/release", "tr")).toBe("/api/release");
  });

  test("stripLang / langFromPath 识别任意语言前缀", () => {
    expect(stripLang("/zh-tw/download/")).toBe("/download/");
    expect(stripLang("/zh/")).toBe("/");
    expect(stripLang("/es")).toBe("/");
    expect(stripLang("/download/")).toBe("/download/");
    expect(stripLang("/zhx/download/")).toBe("/zhx/download/");
    expect(langFromPath("/zh-tw/download/")).toBe("zh-tw");
    expect(langFromPath("/zh/download/")).toBe("zh");
    expect(langFromPath("/tr")).toBe("tr");
    expect(langFromPath("/download/")).toBe("en");
  });

  test("switchLang 在任意两种语言间互转,docs 走 /docs/<lang>/", () => {
    for (const from of LANGS) {
      for (const to of LANGS) {
        const switched = switchLang(href("/pricing", from), to);
        expect(stripLang(switched)).toBe("/pricing/");
        expect(langFromPath(switched)).toBe(to);
      }
    }
    expect(switchLang("/docs/en/protocols/http/", "zh-tw")).toBe("/docs/zh-tw/protocols/http/");
    expect(switchLang("/docs/zh-tw/protocols/http/", "en")).toBe("/docs/en/protocols/http/");
    expect(switchLang("/docs/ru/", "es")).toBe("/docs/es/");
  });
});

describe("浏览器语言判定", () => {
  test("matchLangTag 区分简繁并按主语言码映射", () => {
    expect(matchLangTag("zh-Hant")).toBe("zh-tw");
    expect(matchLangTag("zh-TW")).toBe("zh-tw");
    expect(matchLangTag("zh-HK")).toBe("zh-tw");
    expect(matchLangTag("zh-CN")).toBe("zh");
    expect(matchLangTag("zh-Hans-CN")).toBe("zh");
    expect(matchLangTag("zh")).toBe("zh");
    expect(matchLangTag("es-MX")).toBe("es");
    expect(matchLangTag("ru")).toBe("ru");
    expect(matchLangTag("tr-TR")).toBe("tr");
    expect(matchLangTag("en-GB")).toBe("en");
    expect(matchLangTag("fr-FR")).toBeNull();
  });

  test("parseAcceptLanguage 按 q 值取第一个受支持语言,否则回退 en", () => {
    expect(parseAcceptLanguage(null)).toBe("en");
    expect(parseAcceptLanguage("zh-TW,zh;q=0.9,en;q=0.8")).toBe("zh-tw");
    expect(parseAcceptLanguage("fr;q=0.9,es;q=0.8")).toBe("es");
    expect(parseAcceptLanguage("en;q=0.5,ru;q=0.9")).toBe("ru");
    expect(parseAcceptLanguage("ja,de")).toBe("en");
  });
});

describe("docs 回退与 hreflang", () => {
  test("getFallbackSlugs:en 自身无回退,其余语言 = en 有而本语言缺", () => {
    expect(getFallbackSlugs("en").size).toBe(0);
    // 目录尚未建立的语言 = 全部文档都是回退页
    expect(getFallbackSlugs("tr").size).toBeGreaterThanOrEqual(getFallbackSlugs("zh").size);
  });

  test("getFallbackPathnames 以 /docs/<lang>/ 开头且不含 en", () => {
    for (const pathname of getFallbackPathnames()) {
      expect(pathname.startsWith("/docs/")).toBe(true);
      expect(pathname.startsWith("/docs/en/")).toBe(false);
      expect(pathname.endsWith("/")).toBe(false);
    }
  });

  test("docsAlternates 列出传入语言 + x-default(en)", () => {
    const alts = docsAlternates("protocols/http/", ["en", "zh", "zh-tw"]);
    expect(alts.map((a) => a.lang)).toEqual(["en", "zh", "zh-TW", "x-default"]);
    expect(alts.at(-1)?.href.endsWith("/docs/en/protocols/http/")).toBe(true);
    expect(docsAlternates("x/", [])).toEqual([]);
  });
});
