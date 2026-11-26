/**
 * 文档搜索索引(构建期预渲染,LANGS 每种语言一份 JSON)。
 *
 * 客户端 DocsSearch 组件 fetch `/docs/search-{lang}.json` 后在浏览器内做模糊 + 全文匹配,
 * 无需服务端搜索接口(与全站静态预渲染一致)。非 en 语言缺译页沿用 en 正文(与导航/回退策略一致)。
 */
import type { APIRoute, GetStaticPaths } from "astro";
import { getCollection } from "astro:content";
import { buildDocsNav, docsEntriesOf } from "../../lib/docs-nav";
import { DEFAULT_LANG, LANGS, type Lang } from "@/i18n/config";
import { withBase } from "@/lib/base";

export const prerender = true;

export const getStaticPaths: GetStaticPaths = () => LANGS.map((lang) => ({ params: { lang } }));

/** Markdown 正文 → 可搜索纯文本(去代码块/标记/链接语法,压缩空白)。 */
function toPlainText(md: string): string {
  return md
    .replace(/```[\s\S]*?```/g, " ") // 围栏代码块
    .replace(/`[^`]*`/g, " ") // 行内代码
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1") // 图片/链接 → 文字
    .replace(/^#{1,6}\s+/gm, "") // 标题井号
    .replace(/[*_>#~|]/g, " ") // 其余标记符号
    .replace(/\s+/g, " ")
    .trim();
}

export const GET: APIRoute = async ({ params }) => {
  const lang = params.lang as Lang;
  const all = await getCollection("docs");

  // 以 en 为 slug 全集,译文优先取各语言正文,缺译回退 en(与 buildDocsNav 一致)
  const base = docsEntriesOf(all, DEFAULT_LANG);
  const translated = docsEntriesOf(all, lang);
  const { flat } = buildDocsNav(all, lang);
  const titleBySlug = new Map(flat.map((i) => [i.slug, i.title]));

  const docs = [...base].map(([slug, e]) => {
    const entry = translated.get(slug) ?? e;
    const body = entry.body ?? "";
    const headings = [...body.matchAll(/^#{2,6}\s+(.+?)\s*$/gm)].map((m) =>
      (m[1] ?? "").replace(/[*_`~]/g, "").trim(),
    );
    return {
      slug,
      href: withBase(`/docs/${lang}/${slug}/`),
      title: titleBySlug.get(slug) ?? entry.data.title,
      section: e.data.section,
      description: entry.data.description ?? "",
      headings,
      text: toPlainText(body),
    };
  });

  return new Response(JSON.stringify(docs), {
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "public, max-age=3600",
    },
  });
};
