/**
 * 文档回退页判定 —— 单一实现,供两处共享:
 * 1. astro.config.mjs 的 sitemap({ filter })(该上下文无法使用 getCollection,故用纯 fs)
 * 2. src/pages/docs/[lang]/[...slug].astro 的 getStaticPaths
 *
 * 回退页 = en 存在而某语言(zh / zh-tw / es / ru / tr)缺失的文档:仍生成 /docs/<lang>/... 路由
 * (渲染英文内容 + 未翻译横幅),但带 noindex、不参与 hreflang、不进 sitemap(设计决策 D4)。
 * 机器翻译语言的文档目录初始为空 → 该语言全部文档都是回退页。
 */
import { readdirSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_LANG, LANGS, type Lang } from "../i18n/config";

const DOCS_DIR = fileURLToPath(new URL("../content/docs", import.meta.url));

function listSlugs(lang: Lang): string[] {
  const root = join(DOCS_DIR, lang);
  const out: string[] = [];
  const walk = (dir: string) => {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return; // 目录不存在(如某语言尚未建立)时视为空
    }
    for (const e of entries) {
      const p = join(dir, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name.endsWith(".md")) {
        out.push(relative(root, p).split(sep).join("/").replace(/\.md$/, ""));
      }
    }
  };
  walk(root);
  return out;
}

/** en 有而 `lang` 缺的 slug 集合(= 该语言的回退页);en 自身没有回退页 */
export function getFallbackSlugs(lang: Lang): Set<string> {
  if (lang === DEFAULT_LANG) return new Set();
  const translated = new Set(listSlugs(lang));
  return new Set(listSlugs(DEFAULT_LANG).filter((s) => !translated.has(s)));
}

/** 全部语言的回退页路径(不含域名、不含尾斜杠),供 sitemap filter 比对 */
export function getFallbackPathnames(): Set<string> {
  const out = new Set<string>();
  for (const lang of LANGS) {
    for (const slug of getFallbackSlugs(lang)) out.add(`/docs/${lang}/${slug}`);
  }
  return out;
}
