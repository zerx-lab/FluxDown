// @ts-check
import { defineConfig, envField } from "astro/config";

import react from "@astrojs/react";
import sitemap from "@astrojs/sitemap";
import tailwindcss from "@tailwindcss/vite";
import node from "@astrojs/node";

// https://astro.com/docs/en/guides/environment-variables/
export default defineConfig({
  site: "https://fluxdown.zerx.dev",
  adapter: node({ mode: "standalone" }),
  integrations: [react(), sitemap()],

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

      // ── 可选：Webhook 签名校验 ──
      GITHUB_WEBHOOK_SECRET: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),

      // ── 可选：爱发电 Open API 凭证 ──
      AFDIAN_USER_ID: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      AFDIAN_TOKEN: envField.string({
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

      // ── 可选：Cloudflare R2 对象存储（下载加速，改善中国大陆速度）──
      CF_R2_ACCESS_KEY_ID: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      CF_R2_SECRET_ACCESS_KEY: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      CF_R2_ENDPOINT: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      CF_R2_BUCKET: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
      CF_R2_PUBLIC_URL: envField.string({
        context: "server",
        access: "secret",
        optional: true,
      }),
    },
  },
});
