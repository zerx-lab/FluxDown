/**
 * seo.ts — 全站 SEO 实体与结构化数据单一来源。
 *
 * 2026 Google/Bing 规则要点(已核实):
 *  - 结构化数据从"展示富结果"转向"内容理解 + AI Overview 引用信号",权重上升。
 *  - author/publisher 必须以 `@id` 引用实体,不能内联裸 name;实体命名需全站一致。
 *  - 通过 `@graph` 建立 Organization / WebSite / SoftwareApplication 互引实体图,
 *    让搜索引擎与 LLM 把散落的页面归并到同一品牌实体。
 *
 * 用法:`buildGraph()` 生成首页/全站实体图;`buildSoftwareOffer()` 复用软件报价片段。
 */

export const SITE_URL = "https://fluxdown.zerx.dev";
export const SITE_NAME = "FluxDown";

/** 稳定的实体 @id 锚点(URI fragment 形式,全站唯一且不随页面变化)。 */
export const ORG_ID = `${SITE_URL}/#organization`;
export const WEBSITE_ID = `${SITE_URL}/#website`;
export const SOFTWARE_ID = `${SITE_URL}/#software`;

/** 1200×630 社交/AI 卡片大图。 */
export const OG_IMAGE_URL = `${SITE_URL}/og.png`;
export const OG_IMAGE_WIDTH = 1200;
export const OG_IMAGE_HEIGHT = 630;

/** 品牌一句话定位(高意图关键词:免费 IDM 替代 + Rust)。 */
export const TAGLINE =
  "Free, open-source IDM alternative — a blazing-fast multi-protocol download manager powered by Rust.";

export const SAME_AS = ["https://github.com/zerx-lab/FluxDown"];

type JsonLdNode = Record<string, unknown>;

/** Organization 实体节点。 */
export function orgNode(): JsonLdNode {
  return {
    "@type": "Organization",
    "@id": ORG_ID,
    name: SITE_NAME,
    url: `${SITE_URL}/`,
    logo: {
      "@type": "ImageObject",
      url: `${SITE_URL}/logo.png`,
      width: 512,
      height: 512,
    },
    sameAs: SAME_AS,
  };
}

/** WebSite 实体节点,含站内检索 SearchAction。 */
export function websiteNode(): JsonLdNode {
  return {
    "@type": "WebSite",
    "@id": WEBSITE_ID,
    name: SITE_NAME,
    url: `${SITE_URL}/`,
    publisher: { "@id": ORG_ID },
    inLanguage: ["en", "zh-Hans"],
    potentialAction: {
      "@type": "SearchAction",
      target: {
        "@type": "EntryPoint",
        urlTemplate: `${SITE_URL}/docs/?q={search_term_string}`,
      },
      "query-input": "required name=search_term_string",
    },
  };
}

/** SoftwareApplication 实体节点(引用 Organization 作为发行方)。 */
export function softwareNode(): JsonLdNode {
  return {
    "@type": "SoftwareApplication",
    "@id": SOFTWARE_ID,
    name: SITE_NAME,
    alternateName: "FluxDown Download Manager",
    applicationCategory: "UtilitiesApplication",
    applicationSubCategory: "Download Manager",
    operatingSystem: "Windows 10+, Linux",
    description: TAGLINE,
    url: `${SITE_URL}/`,
    image: OG_IMAGE_URL,
    publisher: { "@id": ORG_ID },
    isAccessibleForFree: true,
    offers: {
      "@type": "Offer",
      price: "0",
      priceCurrency: "USD",
    },
    softwareVersion: "latest",
    license: "https://opensource.org/licenses/MIT",
    featureList: [
      "Multi-threaded download acceleration",
      "HTTP / HTTPS / FTP / BitTorrent / HLS / ed2k protocol support",
      "IDM-style smart dynamic segmentation",
      "Breakpoint resume via SQLite",
      "Chrome / Firefox browser extension integration",
      "Token-bucket global speed limiter",
      "Zero ads, zero tracking, no account required",
    ],
  };
}

/**
 * 组装全站实体图。`extra` 追加页面级节点(如 FAQPage / BreadcrumbList)。
 * 返回可直接 `JSON.stringify` 的对象。
 */
export function buildGraph(extra: JsonLdNode[] = []): JsonLdNode {
  return {
    "@context": "https://schema.org",
    "@graph": [orgNode(), websiteNode(), softwareNode(), ...extra],
  };
}
