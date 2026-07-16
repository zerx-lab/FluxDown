// @ts-check
import { defineConfig, envField } from "astro/config";

import react from "@astrojs/react";
import sitemap, { ChangeFreqEnum } from "@astrojs/sitemap";
import tailwindcss from "@tailwindcss/vite";
import node from "@astrojs/node";
import { getFallbackPathnames } from "./src/lib/docs-fallback.ts";

// 文档回退页(en 有 zh 缺):noindex,不进 sitemap(设计决策 D4,单一判定源 docs-fallback.ts)
const docsFallbackPathnames = getFallbackPathnames();

// 纯社群入口页(内容单薄、与外部邀请链接重复):noindex 且不进 sitemap,
// 避免与主内容页争夺抓取预算(与 Layout noindex prop 保持一致)。
const noindexPathnames = new Set(["/qq-group", "/telegram-group"]);

// 构建时刻(ISO8601),用作 sitemap lastmod —— 每次部署刷新站点 freshness 信号。
const BUILD_TIME = new Date().toISOString();

// 首页语言变体簇(与 src/lib/seo.ts HOME_ALTERNATES 一致;config 无法 import TS 常量,
// 双处以注释互指)。sitemap xhtml:link 是 Google 认可的 hreflang 三种载体之一。
const SITE = "https://fluxdown.zerx.dev";
const HOME_VARIANT_PATHS = new Set(["/", "/zh/", "/ja/"]);
const HOME_SITEMAP_LINKS = [
  { url: `${SITE}/`, lang: "en" },
  { url: `${SITE}/zh/`, lang: "zh" },
  { url: `${SITE}/ja/`, lang: "ja" },
  { url: `${SITE}/`, lang: "x-default" },
];

// https://astro.com/docs/en/guides/environment-variables/
export default defineConfig({
  site: "https://fluxdown.zerx.dev",
  adapter: node({ mode: "standalone" }),
  integrations: [
    react(),
    sitemap({
      filter: (page) => {
        const path = new URL(page).pathname.replace(/\/$/, "");
        return !docsFallbackPathnames.has(path) && !noindexPathnames.has(path);
      },
      // Bing/IndexNow 依赖 ISO8601 lastmod 做 freshness 判定。SSG 页无天然 mtime,
      // 用构建时刻统一标注(每次部署刷新,反映站点最近更新)。changefreq/priority
      // 作为辅助信号:首页最高,其余默认。
      serialize: (item) => {
        item.lastmod = BUILD_TIME;
        const pathname = new URL(item.url).pathname;
        if (HOME_VARIANT_PATHS.has(pathname)) {
          item.links = HOME_SITEMAP_LINKS;
          item.changefreq = ChangeFreqEnum.WEEKLY;
          item.priority = pathname === "/" ? 1.0 : 0.9;
        }
        return item;
      },
    }),
  ],

  markdown: {
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

  vite: {
    plugins: [tailwindcss()],
    ssr: {
      noExternal: ["@primer/react", "styled-components"],
    },
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

      // ── 可选：中国大陆 GitHub 下载加速镜像列表（逗号分隔，覆盖内置默认值）──
      // 例: https://ghproxy.net（默认，hunshcn/gh-proxy 公共实例，CF 加速）
      // 注意：入选前核查 Google Safe Browsing 状态（ghfast.top 曾被拉黑，
      // Chrome 弹全屏警告）；"地址发布页"如 ghproxy.link 不是代理，不可填
      // 镜像不可用时下载路由自动降级到 GitHub 直连
      DOWNLOAD_MIRRORS: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
    },
  },
});
