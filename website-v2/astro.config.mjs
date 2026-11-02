// @ts-check
import { defineConfig, envField } from "astro/config";

import react from "@astrojs/react";
import sitemap, { ChangeFreqEnum } from "@astrojs/sitemap";
import tailwindcss from "@tailwindcss/vite";
import node from "@astrojs/node";
import { getFallbackPathnames } from "./src/lib/docs-fallback.ts";

// 文档回退页(en 有 zh 缺):noindex,不进 sitemap(设计决策 D4,单一判定源 docs-fallback.ts)
const docsFallbackPathnames = getFallbackPathnames();

// 纯社群入口页(内容单薄、与外部邀请链接重复)与短片舞台:noindex 且不进 sitemap,
// 避免与主内容页争夺抓取预算(与页面 noindex prop 保持一致,en/zh 双路径)。
const noindexPathnames = new Set([
  "/qq-group",
  "/telegram-group",
  "/zh/qq-group",
  "/zh/telegram-group",
  // 产品短片录制舞台
  "/film",
  "/zh/film",
]);

// 构建时刻(ISO8601),用作 sitemap lastmod —— 每次部署刷新站点 freshness 信号。
const BUILD_TIME = new Date().toISOString();

// 站点语言路由:en 在根路径,zh 在 /zh 前缀(与 src/i18n/routing.ts 同契约;
// config 无法 import TS 模块,双处以注释互指)。docs 自带 /docs/<lang>/ 结构,不走此映射。
const SITE = "https://fluxdown.com";

// 站点挂载前缀:根部署留空;预览部署到子路径时构建期注入(如 SITE_BASE=/v2,见 Dockerfile)。
// 运行期代码经 import.meta.env.BASE_URL 读取(src/lib/base.ts),与这里同源。
const BASE = (process.env.SITE_BASE ?? "").replace(/\/+$/, "");

/**
 * 去掉挂载前缀,得到站点路径(与 src/lib/base.ts 的 stripBase 同契约)。
 * @param {string} pathname
 */
function stripBase(pathname) {
  if (!BASE) return pathname;
  if (pathname === BASE) return "/";
  return pathname.startsWith(`${BASE}/`) ? pathname.slice(BASE.length) : pathname;
}

/**
 * Markdown 正文里的站内绝对链接(`/docs/...`、`/docs/img.png`)补挂载前缀。
 * 只在子路径部署时启用;根部署零改动。
 */
function rehypeBaseLinks() {
  /** @param {any} node */
  const visit = (node) => {
    if (node.type === "element" && node.properties) {
      for (const attr of ["href", "src"]) {
        const value = node.properties[attr];
        if (typeof value === "string" && value.startsWith("/") && !value.startsWith("//")) {
          node.properties[attr] = `${BASE}${value}`;
        }
      }
    }
    if (Array.isArray(node.children)) node.children.forEach(visit);
  };
  return visit;
}

/**
 * 页面路径 → [enPath, zhPath];docs/api 返回 null(不生成 hreflang 簇)。
 * @param {string} pathname
 */
function langPair(pathname) {
  if (pathname.startsWith("/docs/") || pathname.startsWith("/api/")) return null;
  const bare = pathname === "/zh/" || pathname === "/zh" ? "/" : pathname.replace(/^\/zh(?=\/)/, "");
  const zh = bare === "/" ? "/zh/" : `/zh${bare}`;
  return [bare, zh];
}

// https://astro.com/docs/en/guides/environment-variables/
export default defineConfig({
  site: SITE,
  base: BASE || "/",
  adapter: node({ mode: "standalone" }),
  integrations: [
    react(),
    sitemap({
      filter: (page) => {
        const path = stripBase(new URL(page).pathname).replace(/\/$/, "");
        return !docsFallbackPathnames.has(path) && !noindexPathnames.has(path);
      },
      // Bing/IndexNow 依赖 ISO8601 lastmod 做 freshness 判定。SSG 页无天然 mtime,
      // 用构建时刻统一标注(每次部署刷新,反映站点最近更新)。每个页面输出 en/zh
      // hreflang 簇(xhtml:link);首页 changefreq/priority 最高。
      serialize: (item) => {
        item.lastmod = BUILD_TIME;
        const pathname = stripBase(new URL(item.url).pathname);
        const pair = langPair(pathname);
        if (pair) {
          const [en, zh] = pair;
          item.links = [
            { url: `${SITE}${BASE}${en}`, lang: "en" },
            { url: `${SITE}${BASE}${zh}`, lang: "zh" },
            { url: `${SITE}${BASE}${en}`, lang: "x-default" },
          ];
        }
        if (pathname === "/" || pathname === "/zh/") {
          item.changefreq = ChangeFreqEnum.WEEKLY;
          item.priority = pathname === "/" ? 1.0 : 0.9;
        }
        return item;
      },
    }),
  ],

  markdown: {
    rehypePlugins: BASE ? [rehypeBaseLinks] : [],
    shikiConfig: {
      // 双主题输出 --shiki-light/--shiki-dark CSS 变量,
      // 由 global.css 中锚定 html.light 的桥接规则决定实际展示(站内主题机制,非 prefers-color-scheme)
      themes: { light: "github-light", dark: "github-dark" },
      defaultColor: false,
    },
  },

  // 关闭 CSRF 保护，允许前端 fetch 调用 API 端点
  security: {
    checkOrigin: false,
  },

  // 日文站点已下线(v2 仅 en + zh);旧链接永久跳转到英文首页。
  // macOS 构建已签名 + 公证,「已损坏」指南下线;旧链接跳下载页。
  redirects: {
    "/ja": { status: 301, destination: `${BASE}/` },
    "/macos-gatekeeper": { status: 301, destination: `${BASE}/download/` },
    "/zh/macos-gatekeeper": { status: 301, destination: `${BASE}/zh/download/` },
  },

  prefetch: { prefetchAll: false, defaultStrategy: "hover" },

  vite: {
    plugins: [tailwindcss()],
  },

  env: {
    schema: {
      // ── 必填：GitHub 私有仓库访问凭证 ──
      GITHUB_TOKEN: envField.string({
        context: "server",
        access: "secret",
      }),
      GITHUB_REPO: envField.string({
        context: "server",
        access: "secret",
        default: "user/x_down",
      }),

      // ── 可选：GitHub Projects 专用 Token（需要 read:project scope）──
      // Classic token，在 https://github.com/settings/tokens 创建
      // 勾选 read:project scope 即可，用于读取 Projects v2 看板数据
      GITHUB_PROJECT_TOKEN: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      // GitHub Projects 看板编号（URL 末尾的数字，如 /projects/4 则填 4）
      GITHUB_PROJECT_NUMBER: envField.number({
        context: "server",
        access: "secret",
        default: 4,
        optional: true,
      }),
      // Projects 所属账号（用户名或组织名，如 zerx-lab）
      GITHUB_PROJECT_OWNER: envField.string({
        context: "server",
        access: "secret",
        default: "zerx-lab",
        optional: true,
      }),

      // ── 可选：Webhook 签名校验 ──
      GITHUB_WEBHOOK_SECRET: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),

      // ── 可选：GitHub OAuth 登录（定价页投票/讨论需真实 GitHub 用户）──
      // OAuth App 在 https://github.com/settings/developers 创建，
      // 回调 URL 必须是 `<站点根>/api/auth/github/callback`（生产/本地各建一个 App）
      GITHUB_OAUTH_CLIENT_ID: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      GITHUB_OAUTH_CLIENT_SECRET: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),

      // ── 可选：阿里云 OSS 发布资产 + dl-cn 下载镜像（/api/download，见 src/lib/oss.ts）──
      // 对象布局 `<OSS_RELEASE_PREFIX>/<tag>/<file>`，与 .github/actions/oss-upload 一致。
      // bucket 私有：官网用 AK/SK 只做 HEAD 存在性探测；中国大陆访客（ESA 国家码，client-ip.ts）与 source=oss
      // 302 到 CN_CDN_BASE（URL 鉴权 A），其余访客走 GitHub。AK/SK 或 CN_CDN_AUTH_KEY 缺失时全部走 GitHub。
      OSS_ACCESS_KEY_ID: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      OSS_ACCESS_KEY_SECRET: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      OSS_BUCKET: envField.string({
        context: "server",
        access: "secret",
        default: "zerx-lab",
      }),
      OSS_ENDPOINT: envField.string({
        context: "server",
        access: "secret",
        default: "oss-cn-beijing.aliyuncs.com",
      }),
      OSS_RELEASE_PREFIX: envField.string({
        context: "server",
        access: "secret",
        default: "FluxDownRelease",
      }),
      // dl-cn 下载镜像：阿里云 ESA 边缘函数（website-v2/cdn/esa-dl-cn.js，私有 OSS 回源），
      // 按「URL 鉴权 A 方式」校验 CN_CDN_AUTH_KEY，缺省则镜像关闭。
      CN_CDN_BASE: envField.string({
        context: "server",
        access: "secret",
        default: "https://dl-cn.fluxdown.com",
      }),
      CN_CDN_AUTH_KEY: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      // 官网浏览器下载的验证码通行证（src/lib/download-pass.ts）：HMAC 密钥，随机 32 字节以上。
      // 配置后浏览器（非 FluxDown 更新器 UA）须先过 ESA AI 验证码才拿 dl-cn 镜像，否则 GitHub；留空则不要求。
      // 先在两个站点的反代写好 X-Fluxdown-Edge（见 .omp/knowledge/ops.md「下载准入」）再配置，否则浏览器全部回落 GitHub。
      DOWNLOAD_PASS_SECRET: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      // 123 云盘下载源（src/lib/pan123.ts）：.github/scripts/pan123_mirror.sh 经 WebDAV 上传到同一对象键；
      // 仅 ≥ 0.6.0 的应用内更新（规则见 download-gate.ts::isPan123Updater）优先 302 到 123 直链（10 分钟有效），失败再走 dl-cn / GitHub；官网浏览器下载不走 123。
      // WebDAV 账号 / 应用密码（只授权发布目录）用于 PROPFIND 确认资产；UID + 鉴权密钥用于签直链。缺任一项则关闭。
      PAN123_WEBDAV_URL: envField.string({
        context: "server",
        access: "secret",
        default: "https://webdav.123pan.cn/webdav",
      }),
      PAN123_WEBDAV_USER: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      PAN123_WEBDAV_PASSWORD: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      PAN123_UID: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      // 123「直链管理 → URL 鉴权」密钥（1–16 位字母数字）
      PAN123_AUTH_KEY: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      // 空 = https://<PAN123_UID>.cdn.123clouddisk.cn（备用域名 123clouddisk.com）
      PAN123_DIRECT_BASE: envField.string({
        context: "server",
        access: "secret",
        default: "",
      }),
      // 已启用直链空间的根目录文件夹（即 WebDAV 授权目录）
      PAN123_DIRECT_FOLDER: envField.string({
        context: "server",
        access: "secret",
        default: "fd-release",
      }),

      // ── 赞助名录（Sponsor Wall）──
      // 支付成功后自动把赞助者名称/留言评论到公开仓库的置顶 issue
      SPONSOR_WALL_REPO: envField.string({
        context: "server",
        access: "secret",
        default: "zerx-lab/FluxDown",
      }),
      SPONSOR_WALL_ISSUE: envField.number({
        context: "server",
        access: "secret",
        default: 3,
      }),

      // ── 可选：自由付款支付网关（zerx pay）──
      PAY_GATEWAY_URL: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      PAY_APP_ID: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      PAY_APP_SECRET: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),

      // ── 可选：SMTP 邮件配置 ──
      SMTP_HOST: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      SMTP_PORT: envField.number({
        context: "server",
        access: "secret",
        default: 465,
        optional: true,
      }),
      SMTP_USER: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      SMTP_PASS: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),

      // ── 可选：FluxCloud 云端服务根地址（无 /api 前缀）──
      // /api/cloud/plans 经服务端中转拉取公开套餐目录 GET /api/v1/plans/catalog
      FLUXCLOUD_API_BASE: envField.string({
        context: "server",
        access: "secret",
        default: "http://127.0.0.1:8720",
        optional: true,
      }),
    },
  },
});
