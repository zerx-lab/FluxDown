/**
 * 文档中心共享常量:源码仓库路径、hreflang 簇、分区图标。
 * 文档源文件在 GitHub 上位于 `website-v2/src/content/docs/<lang>/<slug>.md`。
 */
import {
  Braces,
  Globe,
  HeartHandshake,
  Layers,
  Plug,
  Rocket,
  Server,
  type LucideIcon,
} from "lucide-react";
import { HREFLANG, type Lang } from "@/i18n/config";
import { SITE_URL } from "@/lib/seo";
import { GITHUB_URL } from "@/lib/site-nav";
import type { SectionId } from "@/lib/docs-nav";

const DOCS_SOURCE = "website-v2/src/content/docs";

export const SECTION_ICONS: Record<SectionId, LucideIcon> = {
  "getting-started": Rocket,
  protocols: Layers,
  "browser-extension": Globe,
  "headless-server": Server,
  api: Braces,
  plugins: Plug,
  contributing: HeartHandshake,
};

export function docsRepoTree(): string {
  return `${GITHUB_URL}/tree/main/${DOCS_SOURCE}`;
}

/**
 * 某语言某页的 GitHub 编辑 / 历史 / 新建译文链接。
 * `translateTo` 仅在页面缺译(回退页)时传入,指向该语言译文目录的新建链接。
 */
export function docsSourceLinks(srcLang: Lang, slug: string, translateTo?: Lang) {
  const file = `${DOCS_SOURCE}/${srcLang}/${slug}.md`;
  return {
    edit: `${GITHUB_URL}/edit/main/${file}`,
    history: `${GITHUB_URL}/commits/main/${file}`,
    // GitHub /new?filename= 会忽略 URL 路径末级目录,目录必须拼进 filename
    translate: translateTo
      ? `${GITHUB_URL}/new/main/${DOCS_SOURCE}/${translateTo}?filename=${encodeURIComponent(`${slug}.md`)}`
      : undefined,
  };
}

/**
 * docs 的 hreflang 簇:`/docs/en/x/` ↔ `/docs/zh/x/` ↔ `/docs/zh-tw/x/` …,x-default = en。
 * `langs` 为真实存在译文的语言(回退页传空数组,不输出任何 alternate)。
 */
export function docsAlternates(path: string, langs: readonly Lang[]) {
  if (langs.length === 0) return [];
  const url = (l: Lang) => `${SITE_URL}/docs/${l}/${path}`;
  return [
    ...langs.map((l) => ({ lang: HREFLANG[l], href: url(l) })),
    { lang: "x-default", href: url("en") },
  ];
}
