/**
 * Astro Content Collections 配置
 *
 * docs 集合:产品文档,多语言目录结构 src/content/docs/<lang>/<section>/<page>.md(lang 取 `LANGS`,en 为全集)
 * schema 刻意保持最小(除 title 外全部可选/带默认),
 * 降低社区 PR 因 frontmatter 错误拖垮全站构建的概率(CI 见 .github/workflows/website-ci.yml)。
 */
import { defineCollection, z } from "astro:content";
import { glob } from "astro/loaders";

const docs = defineCollection({
  loader: glob({ pattern: "**/*.md", base: "./src/content/docs" }),
  schema: z.object({
    title: z.string(),
    description: z.string().optional(),
    section: z.enum([
      "getting-started",
      "protocols",
      "browser-extension",
      "headless-server",
      "api",
      "plugins",
      "contributing",
    ]),
    order: z.number().default(999),
    /** 仅译文(非 en)文件使用:对应 en 文件正文的 sha256 前 12 位(见 npm run docs:hash) */
    sourceHash: z.string().optional(),
  }),
});

export const collections = { docs };
