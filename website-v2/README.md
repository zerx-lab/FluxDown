# FluxDown 官网 v2

面向 GPUI 桌面客户端的新版官网。Astro 5（SSR，`@astrojs/node` standalone，自托管）+ React 19 islands + Tailwind CSS v4。

## 运行

```bash
bun install
bun run dev        # http://localhost:4321
bun run build      # astro check + astro build → dist/
node dist/server/entry.mjs
```

环境变量见 `.env.example`（`GITHUB_TOKEN` 等仅 `/api/*` 路由在运行期使用）。

## 插件作者反馈

- 创建一个专供插件反馈的**公开 GitHub App**，关闭 Webhook，不订阅事件，不用 OAuth；仓库权限只开 **Issues: Read and write** 与默认的 **Metadata: Read-only**。把数字 App ID 和生成的私钥 PEM 的单行 Base64 写入运行期 `.env` 的 `PLUGIN_FEEDBACK_APP_ID`、`PLUGIN_FEEDBACK_PRIVATE_KEY_BASE64`，安全保管原始私钥和编码值（Base64 不是加密），不要放进镜像或前端。修改运行期变量后重新创建容器（仅重启不会更新注入的环境）。不配两项即关闭插件入口，不影响原网站反馈的 `GITHUB_TOKEN` / `GITHUB_REPO`。
- 插件作者在 GitHub 安装该 App，**仅选择愿意接收反馈的仓库**，并开启 Issues。只有官方插件索引 [`registry.json`](https://github.com/zerx-lab/fluxdown-plugin-index/blob/main/registry.json) 当前登记的公开、未归档、启用了 Issues 的仓库可用，旧 identity 只通过登记的 `formerIds` 解析。移除该仓库的安装授权、关闭 Issues 或从登记移除即可取消。安装不需要配置站点密钥；允许到 `raw.githubusercontent.com` 与 `api.github.com` 的站点出站连接。
- `GET /api/feedback?pluginIdentity=…` 确认可用后返回 `{ repository }`，使用单仓库 `metadata: read` 令牌，不写 Issue；`POST` 带 `pluginIdentity` 和用户确认的 `repository`（可选 `pluginVersion`），重新解析登记与安装授权，并用单仓库 `issues: write` 令牌提交，成功返回 201 `{ issueNumber }`。404 表示不可用，409 表示确认后的目的地已改变，不静默转发到通用入口。插件请求失败时，GitHub 状态码与操作名可用于排查，日志不包含原始响应或令牌。
- 通用反馈仍提交到既有私有仓库；同源或无 `Origin` 的旧客户端即使未标记 `application/json` 也可以提交 JSON，通用请求体上限 256 KiB。自托管 Web SPA 的无凭据 CORS 只在带 `?pluginIdentity=…` 的插件入口开放，POST 必须发送 JSON，query 与 JSON identity 必须一致；反代须转发该路由的 OPTIONS / GET / POST，不缓存其响应。
- 公开 Issue 不包含联系方式字段、日志或访客 IP；用户提供的内容以纯文本展示，过滤常见邮箱、电话号码和 GitHub 通知引用。内容仍公开，勿在描述里填写秘密或其他个人信息。限流按进程计算：反馈 POST 同 IP 每分钟 3 次，插件探测及授权同 IP 每分钟 10 次、进程每分钟 60 次，插件写入每小时同 IP 4 次、同仓库 6 次、同 IP 对同仓库 2 次、进程 40 次。多副本部署要在入口使用可信来源 IP 叠加共享限流；反代必须覆盖 `X-Client-IP`，不要直接暴露容器端口。
- Web 不运行完整诊断来推断插件宿主平台；用户开启诊断后，四端只附带可预览的固定英文加载状态和可用审计计数。网络错误、5xx 或无效回执可能发生在已创建 Issue 之后；提交结果无法确认时，客户端提示先检查目标仓库再重试。

## 下载策略

- 官网下载页与更新日志使用 `/api/download/<filename>?source=auto&tag=<release>`：先由 GitHub 发布元数据验证非草稿 release 与精确资产再 302。中国大陆访客由阿里云 ESA 按访客真实 IP 判定（IPv4 / IPv6 均可）：反代只采信带对鉴别头（`X-Fluxdown-Edge-Key`）的 ESA 回源请求里托管转换注入的 `Ali-Ip-Country`（旧域名经 Cloudflare 时为 `CF-IPCountry`），以 `X-Client-Country` 覆盖写给官网（`src/lib/client-ip.ts::getClientCountry`），`CN` 为大陆，港澳台是 HK / MO / TW。大陆访客先 HEAD 私有 OSS 确认资产存在且大小一致（结果缓存 1h），再 302 到下载镜像 `CN_CDN_BASE`（`dl-cn.fluxdown.com`，URL 鉴权 A，3 小时有效）；其余访客与 OSS 缺资产时 302 到 GitHub。镜像为阿里云 ESA（中国内地）边缘函数 `cdn/esa-dl-cn.js`（校验签名 + 私有 OSS 回源，边缘缓存 30 天；控制台粘贴代码发布，部署说明见文件头）。
- 123 云盘（`src/lib/pan123.ts` + `src/lib/download-pan123.ts`）优先于 dl-cn，**只用于 ≥ 0.6.0 的应用内更新**，不分地区；官网浏览器下载（含大陆访客）与不带 User-Agent 的旧版更新器不走 123。在下载准入之外还须同时满足（`src/lib/download-gate.ts`）：User-Agent 严格为 `fluxdown-agent/<版本>` 或 `fluxdown-android/<版本>` 且版本 ≥ 0.6.0（`isPan123Updater`）；资产在 `/api/release` 当前清单（stable / frontier）的桌面、服务器或 Android 资产里（`recordUpdateAssets` / `isUpdateAsset`，历史版本、扩展、CLI、iOS 侧载包不给）；同一 IP 24 小时内签发不超过 10 条（`hasPan123Quota`）。通过后先 WebDAV `PROPFIND` 确认 `<OSS_RELEASE_PREFIX>/<版本>/<组件>/<文件>` 存在且大小一致（缓存：存在 1h、缺失 60s），再本地签发直链空间链接 `https://<UID>.cdn.123clouddisk.cn/<UID>/fd-release/<对象键>?auth_key=<过期>-<rand>-<UID>-<md5>`（123「URL 鉴权」，签名用解码后的路径，**有效期 10 分钟**：更新器拿到 302 立即单连接下载，续传 / 重试都会重新请求 `/api/download`），`X-Download-Source: pan123`。直链不绑 IP；WebDAV GET 的 302 链接只认生成它的出口 IP（东京服务器生成后用户 403 / 50002），不能用。123 侧另设流量上限：全站每日 200GB、单 IP 每日 5GB（「直链流量包 → 流量设置」）。任一条件不满足或 123 不可用时大陆更新器回落 dl-cn，非大陆直接 GitHub。资产在 release 发布成功后由 `pan123-upload.yml` 经东京服务器中转上传（GitHub runner 直传 123 极慢），上传期间（约半小时）大陆更新器仍走 dl-cn。
- 准入（`src/lib/download-gate.ts`）：只有同一访客 IP 在 24 小时内读过 `/api/release` 或 `/api/changelog`（官网下载页、首页、更新日志页与应用内更新都会先读清单）、且 User-Agent 不是常见脚本 / HTTP 库时才发 CDN 链接；直接批量请求 `/api/download` 的脚本一律 302 GitHub。记录在进程内存，重启后清空。
- 浏览器验证码通行证（`src/lib/download-pass.ts` + `src/pages/api/download/pass.ts` + 前端 `src/lib/download-captcha.ts`）：配置 `DOWNLOAD_PASS_SECRET` 后，UA 非空且不是 `fluxdown-<产品>/<版本>` 更新器的请求（浏览器等）须带 `fd_dl_pass` Cookie（HMAC 绑访客 IP，30 分钟）才拿镜像，否则 302 GitHub；FluxDown 更新器与不带 UA 的旧版更新器不受影响。Cookie 由 `POST /api/download/pass` 签发：ESA AI 验证码规则 `download-pass` 在边缘验签（拦截空 Token），官网另要求反代写入的 `X-Fluxdown-Edge: 1`（经 ESA）与精确路径。`GET /api/download/pass` 返回 `{ required }` 供页面决定是否加载验证码。部署顺序与反代配置见 `.omp/knowledge/ops.md`「浏览器下载验证码」。
- 显式 `source=github` 只用 GitHub；`source=oss` 对可走镜像的大陆访客只用已验证的镜像（更新器 123 云盘或 dl-cn，浏览器仅 dl-cn），都不可用为 503，其余访客同 `auto`。缺省/未知 source 与 `auto` 相同。GitHub 仅有过期缓存时只允许 GitHub 直链，不签发任何镜像链接。
- 阿里云 OSS 默认域名禁止分发 `.apk` / `.ipa`：桶内存为 `<name>.bin`（Content-Type 须为 octet-stream），镜像回源时换成 `.bin` 并覆盖 APK 的 Content-Type / Content-Disposition；规则见 `src/lib/oss-storage.ts`，上传侧在 `.github/actions/oss-upload`。
- 本地回归：`bun test tests/download-backend.test.ts tests/download-route.test.ts tests/download-pass.test.ts tests/download-source.test.ts`，完整静态与构建检查：`bun run build`。这些测试替换外部 fetch 与反代头，不代表真实 CDN 回源演练。

## BT 防吸血自查 / 申诉

- BT 防吸血自查 / 申诉：`/bt-guard`（en）与 `/zh/bt-guard`（zh）是静态外壳（可索引、进 sitemap，不含个人信息），`src/components/btguard/BtGuardSelfCheck.tsx` 在浏览器里经同源代理 `GET /api/cloud/bt-guard/self` 现取访客自己的结果（FluxCloud `GET /api/v1/bt-guard/public/self`，只回答请求来源 IP 所在组，经 `X-Forwarded-For` 透传访客 IP）：`{ listed, ipGroup?, entries[{ reason, category, action, expiresAt(unix 秒) }], turnstileSiteKey?, appealPending? }`。已列入且无待处理申诉时给申诉表单，经 `POST /api/cloud/bt-guard/appeal`（`{ detail ≤ 2000, contact? 邮箱 ≤ 200, turnstileToken? }` → FluxCloud `POST /api/v1/bt-guard/public/appeal`，成功 204，失败透传 404 `not_listed` / 409 `appeal_pending` / 403|400 `turnstile_*` / 429）。代理响应 no-store + `X-Robots-Tag: noindex`；逻辑在 `src/lib/bt-guard.ts`（解析 / 校验）与 `src/lib/bt-guard-proxy.ts`（上游调用，复用 `share-proxy.ts::callUpstream`）。隐私政策 `#bt-guard` 一节说明证据上报。回归：`bun test tests/bt-guard-proxy.test.ts`。
- 代理传输、失败映射与 IP 透传在 `src/lib/cloud-proxy.ts`（`callUpstream` / `failureResponse`），环境变量只用 `FLUXCLOUD_API_BASE`（FluxCloud 根地址，无 `/api` 前缀，缺省 `http://127.0.0.1:8720`）。代理响应一律 `Cache-Control: no-store`、`X-Robots-Tag: noindex, nofollow`。
- 回归：`bun test tests/bt-guard-proxy.test.ts`（替换 FluxCloud fetch，不代表真实上游与 Turnstile 演练）。

## 账户中心

- 页面：`/account/`（账户中心：概览 / 会员 / 设备 / 云端推送 / 账户与安全）与 `/account/login/`、`/account/register/`、`/account/forgot/`（zh 加 `/zh` 前缀），静态外壳 + React island，全部 `noindex` 且不进 sitemap（`astro.config.mjs::noindexPathnames`）。
- BFF：浏览器只调同源 `/api/account/*`（`src/pages/api/account/[...path].ts` → `src/lib/account/routes.ts` 白名单路由），服务端再调 FluxCloud `/api/v1/*`。请求体只取已知字段重建，响应只回 `src/lib/account/types.ts` 的字段，**任何响应都不含令牌**；失败透传 FluxCloud 状态码与 `code`（文案 `src/i18n/messages/accountErrors.ts`）。
- 会话（`src/lib/account/session.ts`）：刷新令牌 `fd_acct_rt`（90 天滑动）与访问令牌 `fd_acct_at`（15 分钟，提前 60 秒过期）为 HttpOnly + SameSite=Strict + 生产 Secure，Path `/api/account`；`fd_acct_dev` 是本浏览器的 Web 会话 deviceId（随机 UUID）；`fd_signed_in=1` 是给静态页顶栏用的非 HttpOnly 提示，不作鉴权。访问令牌过期或上游 401 `unauthorized` 时刷新一次再重放；同一刷新令牌并发只刷新一次，结果复用 30 秒（单进程内，多副本依赖 FluxCloud 的轮换重放宽限）。
- CSRF：`security.checkOrigin` 全站关闭，账户接口自行把关——每个请求必须带 `X-Fluxdown-Account: 1`（跨站带自定义头需 CORS 预检，本站不应答），浏览器给出的 `Sec-Fetch-Site` 必须是 `same-origin`。
- Web 会话依赖 FluxCloud 把 `devicePlatform: "web"` 的设备与 App 设备隔离（不占 `maxDevices`、不进 `GET /devices`、不能当派发目标、每账号至多 5 个），**部署顺序：先升级 FluxCloud 再上线官网**，否则官网登录会占用 App 设备名额并出现在各端设备列表里。新浏览器首次密码登录照常需要邮箱验证码；登出会删除该 Web 会话的设备信任。
- 登录后购买复用 `/api/cloud/order*` 公开网页下单端点（`account` = 本人 Origin ID 或邮箱），不新增订单接口；定价页在有登录提示时先取 `/api/account/me` 跳过账号确认步。官网仍不展示 USDT。
- 环境变量只用既有的 `FLUXCLOUD_API_BASE`；反代须转发 `/api/account/*` 的 Cookie 与 `Set-Cookie`，不缓存其响应。
- 回归：`bun test tests/account-bff.test.ts`（替换 FluxCloud fetch 与 Cookie，不代表真实上游演练）。

## AI 助手

- 页面：`/assistant/`（zh 加 `/zh` 前缀，顶栏「AI 助手」），静态外壳 + React island（`src/components/assistant/`），`noindex` 且不进 sitemap。必须登录：`GET /api/account/ai/config` 返回 401 即跳登录页并带回本页；FluxCloud 未开放（`off` / `internal_only`）或套餐不含 `aiAssistant` 时给说明页。
- 全站悬浮入口：`src/components/assistant/widget/`，`Layout.astro` 在有外壳的页面挂载（助手整页与登录 / 注册 / 找回密码页除外）。右下角按钮展开悬浮窗，拖标题栏可自由移动（夹在视口内，双击标题栏复位到右下角）；宽屏（≥1024px）可停靠到右侧，拖左缘调宽（320 ~ min(960, 视口 − 560) px，方向键微调，双击复位 420），`<html data-ai-dock>` + `--ai-dock-w` 让 body 让出同样宽度。未登录给登录 / 注册引导（带回当前页），已登录即在面板内对话、看历史、在完整页面打开（`/assistant/?session=<id>`）。形态、位置、停靠宽度存 localStorage，展开状态与当前会话存 sessionStorage（本标签页跨页面保留）；对话部分首次展开才懒加载。原 LangBot 客服组件已移除。
- BFF 挂在账户 BFF 下（`src/lib/account/ai-routes.ts`，`/api/account/ai/*` → FluxCloud `/api/v1/ai/*`），复用会话 Cookie、刷新与 CSRF 闸：`config`、`sessions`（列表 / 新建 / 详情 / 删除 / 评价 / 提问 / 取消）、`issues/{id}/confirm|discard`、`attachments/{id}`。会话固定 `channel: "website"`、不声明客户端工具；详情只回用户 / 助手消息（用户消息带图片 id）。来自桌面 / Web 的会话只读。
- 带图提问：`ai/config` 的 `imagesSupported` 为真时输入框可粘贴、拖入或选择图片（PNG / JPEG / GIF / WebP，张数与单张上限取 `maxImages` / `maxImageBytes`）；超限的静态图在浏览器里缩放重编码（`src/components/assistant/attachments.ts`），GIF 超限直接拒绝。提问请求体上限与 FluxCloud `MAX_ASK_BODY_BYTES` 同口径（约 28 MiB），**反代须放开 `/api/account/ai/sessions/*/messages` 的请求体大小**（如 nginx `client_max_body_size 32m`）。已存图片经 `GET /api/account/ai/attachments/{id}` 取：路由标 `simpleGet`，`<img src>` 不带自定义头也放行（仍要求 `Sec-Fetch-Site` 同源，会话 Cookie 为 SameSite=Strict），只转发四种位图类型并带 `nosniff` 与 `sandbox` CSP。
- 提问是 SSE：`callAuthedStream` → `openUpstreamStream`（响应头 10 秒内到达，正文不设总时限，访客断开即中止上游，FluxCloud 随之取消执行），逐帧经 `ai-stream.ts` 白名单裁剪（丢弃分诊 / 路由 / 思考过程 / 客户端工具调用 / token 用量）。反代须对 `/api/account/ai/sessions/*/messages` 关闭响应缓冲（BFF 已回 `X-Accel-Buffering: no`）。
- 回答用 `src/components/assistant/md-parse.ts` 解析成结构化节点再由 `Markdown.tsx` 以 React 元素渲染（不注入 HTML；链接只放行 http(s) / mailto / 站内路径）。
- 部署顺序：FluxCloud AI 模块先上线并在后台开放 `mode`，官网才有可用的入口。
- 回归：`bun test tests/assistant-bff.test.ts`（替换 FluxCloud fetch，不代表真实模型与上游演练）。

## 结构

| 位置 | 内容 |
|---|---|
| `src/styles/global.css` | 设计系统：OKLCH token（`html[data-theme]` 明暗）、导轨框架 `.frame/.rule/.section`、`.eyebrow/.btn/.kbd/.fig/.cells/.prose` 等组件类、视图过渡与滚动揭示 |
| `src/layouts/Layout.astro` | SEO 头、hreflang、主题预绘制、Header / Footer / ⌘K 命令面板 / 公告条 |
| `src/scripts/motion.ts` | `data-reveal` / `data-scramble` / `data-count`、`.rule` 扫描揭示、`.spot` / `.spot-edge` 聚光与边缘光、`data-magnetic` 按钮磁吸、主题圆形揭示切换、`D` 与 `⌘K` 快捷键 |
| `src/scripts/particle-stage.ts` | 粒子舞台：canvas 尺寸 / DPR、离屏停帧、`prefers-reduced-motion` 静帧、设计 token 取色（随主题切换）、宿主指针与按下事件、`StrokeBatch` 合批绘制；场景只实现 `Scene` |
| `src/scripts/idle.ts` | `whenIdle()`：装饰动效（粒子舞台、复刻窗口演示、分段可视化等）统一延后到首帧绘制后的空闲时段，并以动态 `import()` 拆出首屏关键路径；首屏 UI 全部 SSR 直出，首屏（Hero）不挂 `data-reveal`，不等任何脚本 |
| `src/i18n/` | 语言契约与路由（`config.ts`、`routing.ts`）、`defineMessages` 类型化文案、`messages/<命名空间>.ts`；机翻语言的生成模块 `locales/<lang>/<ns>.ts` 与译文库 `locales/<lang>.json` |
| `src/pages/[...lang]/` | 全部页面：一个文件同时产出 `/x/`（en）与 `/<lang>/x/`（其余语言） |
| `src/pages/docs/` | 文档（`/docs/<lang>/…`，内容在 `src/content/docs/<lang>`） |
| `src/pages/api/` | 后端接口（release / download / plugins / pay …，桌面客户端依赖其中部分，契约不随 UI 变动） |
| `src/components/home/` | 首页分区；`AppReplica.astro` + `app-demo.ts` 是 GPUI 客户端的 DOM 复刻与脚本化演示，`segment-sim.ts` 是动态分段模拟；`flux-field.ts`（首屏数据流粒子，几何与线稿共用 `hero-geometry.ts`）与 `neural-sphere.ts`（收尾 CTA 神经点云球）是两个粒子场景 |

## 语言

语言集见 `src/i18n/config.ts::LANGS`：en + zh 手写，zh-tw / es / ru / tr 由 qwen-mt 机翻（`.github/workflows/i18n-mt.yml`，本机 `task i18n:mt`）。en 在根路径，其余语言在 `/<lang>` 前缀，服务端按 URL 直出。新增文案：在 `src/i18n/messages/` 用 `defineMessages("<ns>", { en, zh })` 声明，zh 与 en 结构不一致会在 `astro check` 报错；改了 en 结构后跑 `bun run i18n:generate` 刷新机翻语言模块（缺译回退 en），不要手改 `src/i18n/locales/`。站内链接一律经 `href(path, lang)` 生成。

## 产品短片

`/film/`（noindex）是 1920×1080 的短片舞台：片头、GPUI 复刻界面演示、镜头推拉、字幕与片尾都由同一个时钟逐帧计算，可精确复现。`public/film/` 中的视频由它生成：

1. `bun run build && node dist/server/entry.mjs`
2. 用无头 Chromium 以 1920×1080 视口打开 `/film/?hold=1`（zh 用 `/zh/film/?hold=1`），对 `i = 0 … duration×60` 依次执行 `window.__film.seek(i / 60)` 并截图为 `%05d.jpg`
3. 合成：
   ```bash
   ffmpeg -framerate 60 -i %05d.jpg -c:v libx264 -preset slow -crf 24 -pix_fmt yuv420p -movflags +faststart -tune animation fluxdown-film-<lang>.mp4
   ```
4. 海报取第 975 帧（缩放到 1600 宽）存为 `fluxdown-film-<lang>.jpg`；`public/og.png` 是 1200×630 视口下 `/film/?t=19.8` 的截图。
