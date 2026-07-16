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

/** 品牌一句话定位(叙事:the download manager rebuilt in Rust;高意图关键词:download manager + Rust)。 */
export const TAGLINE =
  "The download manager, rebuilt in Rust — runtime dynamic segmentation, multi-protocol coverage from HTTP to BitTorrent, ED2K and HLS, and deep browser integration. Free and open source.";

export const SAME_AS = ["https://github.com/zerx-lab/FluxDown"];

/**
 * 首页三语 meta 与 hreflang 簇的单一来源(HTML <link>、sitemap xhtml:link 共用)。
 * hreflang 码遵循 Google 现行规范(ISO 639-1),x-default 指向语言选择页 "/"。
 */
export const HOME_META = {
  en: {
    title: "FluxDown — Multi-Protocol Download Manager, Rebuilt in Rust",
    description:
      "FluxDown rebuilds the download manager from the ground up: a Rust engine with runtime dynamic segmentation, HTTP/HTTPS/FTP/BitTorrent/ED2K/HLS support, and deep browser integration. Free and open source — no ads, no throttling.",
  },
  zh: {
    title: "FluxDown — Rust 重写的多协议下载管理器 | 免费开源下载工具",
    description:
      "FluxDown 用 Rust 从头重写下载管理器：运行时动态分段加速，支持 HTTP/HTTPS/FTP/BT 磁力/ED2K/HLS 流媒体，浏览器深度接管。免费开源，无广告，不限速。",
  },
  ja: {
    title: "FluxDown — Rust 製マルチプロトコル・ダウンロードマネージャー | 無料・オープンソース",
    description:
      "FluxDown は Rust でダウンロードマネージャーをゼロから再構築。実行時の動的セグメント分割、HTTP/HTTPS/FTP/BitTorrent/ED2K/HLS 対応、ブラウザとの深い連携。無料・オープンソース、広告なし、速度制限なし。",
  },
} as const;

export const HOME_ALTERNATES = [
  { lang: "en", href: `${SITE_URL}/` },
  { lang: "zh", href: `${SITE_URL}/zh/` },
  { lang: "ja", href: `${SITE_URL}/ja/` },
  { lang: "x-default", href: `${SITE_URL}/` },
];

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
    inLanguage: ["en", "zh-CN", "ja"],
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
