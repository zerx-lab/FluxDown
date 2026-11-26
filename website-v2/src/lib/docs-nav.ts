/**
 * 文档导航树构建:分区元数据 + 从 docs 集合生成分组导航与扁平序列(供上一页/下一页)。
 * 非 en 语言的导航以 en 集合为全集——缺译页仍出现在导航中(点击进入回退页)。
 */
import type { CollectionEntry } from "astro:content";
import { DEFAULT_LANG, type Lang } from "@/i18n/config";
import { docs } from "@/i18n/messages/docs";

/** 文档分区(顺序即侧栏顺序);各语言标题见 `docs[lang].sectionLabels`。 */
export const SECTIONS = [
  "getting-started",
  "protocols",
  "browser-extension",
  "headless-server",
  "api",
  "plugins",
  "contributing",
] as const;

export type SectionId = (typeof SECTIONS)[number];

export type DocsEntry = CollectionEntry<"docs">;

/** 某语言目录下的文档条目(slug → 条目,保持集合顺序);集合 id 形如 `<lang>/<slug>`。 */
export function docsEntriesOf(all: DocsEntry[], lang: Lang): Map<string, DocsEntry> {
  const prefix = `${lang}/`;
  return new Map(all.filter((e) => e.id.startsWith(prefix)).map((e) => [e.id.slice(prefix.length), e]));
}

export interface DocsNavItem {
  slug: string;
  title: string;
  href: string;
  /** 当前语言缺译(非 en 导航中指向回退页) */
  fallback: boolean;
  section: SectionId;
}

export interface DocsNavGroup {
  id: SectionId;
  label: string;
  items: DocsNavItem[];
}

export function buildDocsNav(all: DocsEntry[], lang: Lang): { groups: DocsNavGroup[]; flat: DocsNavItem[] } {
  const base = docsEntriesOf(all, DEFAULT_LANG);
  const translated = lang === DEFAULT_LANG ? base : docsEntriesOf(all, lang);

  const items = [...base].map(([slug, e]) => {
    const tr = translated.get(slug);
    const use = tr ?? e;
    return {
      slug,
      title: use.data.title,
      href: `/docs/${lang}/${slug}/`,
      fallback: !tr,
      section: e.data.section,
      order: use.data.order,
    };
  });

  const groups: DocsNavGroup[] = [];
  for (const id of SECTIONS) {
    const groupItems = items
      .filter((i) => i.section === id)
      .sort((a, b) => a.order - b.order || a.title.localeCompare(b.title));
    if (groupItems.length > 0) {
      groups.push({ id, label: docs[lang].sectionLabels[id], items: groupItems });
    }
  }
  return { groups, flat: groups.flatMap((g) => g.items) };
}
