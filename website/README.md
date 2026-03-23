# FluxDown 官网

基于 Astro + React + Tailwind CSS 构建，部署到 Vercel。

## 环境变量配置

复制 `.env.example` 为 `.env`，填写以下必要变量：

| 变量 | 说明 | 必填 |
|------|------|------|
| `GITHUB_REPO` | 仓库地址（owner/repo） | 是 |
| `GITHUB_TOKEN` | GitHub PAT，需要 `repo` 权限 | 是 |
| `GITHUB_WEBHOOK_SECRET` | Webhook 签名密钥 | 否 |
| `SMTP_HOST/PORT/USER/PASS` | SMTP 邮件配置 | 否 |
| `AFDIAN_USER_ID/TOKEN` | 爱发电 API | 否 |
| `CF_R2_ACCESS_KEY_ID` | R2 API 令牌访问密钥 ID | 否 |
| `CF_R2_SECRET_ACCESS_KEY` | R2 API 令牌机密访问密钥 | 否 |
| `CF_R2_ENDPOINT` | R2 S3 终结点 URL | 否 |
| `CF_R2_BUCKET` | R2 存储桶名称 | 否 |
| `CF_R2_PUBLIC_URL` | R2 公开访问域名（如 `https://dl.fluxdown.app`） | 否 |

### Cloudflare R2 配置说明

R2 用于将 Release 文件镜像到 Cloudflare 网络，改善中国大陆用户的下载速度。

1. 在 [Cloudflare Dashboard](https://dash.cloudflare.com/) → R2 → 创建存储桶
2. 进入存储桶 → 设置 → 公开访问，绑定自定义域（如 `dl.fluxdown.app`）
3. R2 → 管理 R2 API 令牌 → 创建 Account API 令牌
   - 权限：**对象读和写**
   - 指定存储桶：仅限 fluxdown 存储桶
4. 将凭据填入 `.env`，并同步到 Vercel 环境变量和 GitHub Actions Secrets

GitHub Actions Secrets 需要配置（用于发版时自动同步文件到 R2）：

| Secret 名称 | 对应值 |
|-------------|--------|
| `CF_R2_ACCESS_KEY_ID` | 访问密钥 ID |
| `CF_R2_SECRET_ACCESS_KEY` | 机密访问密钥 |
| `CF_R2_ENDPOINT` | S3 终结点 URL |
| `CF_R2_BUCKET` | 存储桶名称 |

## 🚀 Project Structure

Inside of your Astro project, you'll see the following folders and files:

```text
/
├── public/
├── src/
│   └── pages/
│       └── index.astro
└── package.json
```

Astro looks for `.astro` or `.md` files in the `src/pages/` directory. Each page is exposed as a route based on its file name.

There's nothing special about `src/components/`, but that's where we like to put any Astro/React/Vue/Svelte/Preact components.

Any static assets, like images, can be placed in the `public/` directory.

## 🧞 Commands

All commands are run from the root of the project, from a terminal:

| Command                   | Action                                           |
| :------------------------ | :----------------------------------------------- |
| `npm install`             | Installs dependencies                            |
| `npm run dev`             | Starts local dev server at `localhost:4321`      |
| `npm run build`           | Build your production site to `./dist/`          |
| `npm run preview`         | Preview your build locally, before deploying     |
| `npm run astro ...`       | Run CLI commands like `astro add`, `astro check` |
| `npm run astro -- --help` | Get help using the Astro CLI                     |

## 👀 Want to learn more?

Feel free to check [our documentation](https://docs.astro.build) or jump into our [Discord server](https://astro.build/chat).
