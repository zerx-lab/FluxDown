# YouTube 下载插件（fluxdown@youtube-down）

在插件内自研解析 YouTube，把直链交回引擎做多段下载、断点续传与音视频合并；同时向浏览器扩展贡献 YouTube 嗅探规则，让播放器上直接出现下载浮钮。不依赖 yt-dlp、ffmpeg 或任何外部 JavaScript 运行时。

## 依赖

- App 版本需支持 manifest `resolvers[0].memoryLimitMb`（本插件声明 384 MiB，见下文「内存」）。旧版 App 会以「manifest 非法」拒绝安装。
- 与索引仓的 `fluxdown@ytdlp` 插件同时启用时两者都匹配 YouTube 链接，只保留一个。

## 安装

设置 → 扩展 → 插件 → 「安装开发目录」，选择本目录 `packages/plugin-sdk/examples/youtube-down`，授权 `browserRules`。

## 支持的链接

|链接|处理|
|---|---|
|`youtube.com/watch?v=…`（含 `m.` / `music.`）、`youtu.be/…`、`/shorts/…`、`/live/…`、`/embed/…`、`youtube-nocookie.com/embed/…`|单个视频。带 `list=` 的播放页只下当前视频|
|`youtube.com/playlist?list=…`|播放列表：返回清单，按条目裂变子任务（条数上限见设置）|

直播进行中与预告不可下载；直播结束、YouTube 处理完成后可下载回放。

## 解析流程

客户端选择、请求格式与格式取舍对齐 yt-dlp（`extractor/youtube/_base.py`、`_video.py`，2026-11 版）：

1. **会话与播放器版本**：带登录 Cookie 时先取嵌入页 `ytcfg`（会话序号、账号 id、visitorData、播放器地址，与播放页 ytcfg 同源同字段，但约 150 KB 而播放页约 1.5 MB；同一份 ytcfg 也供 `web_embedded` 客户端使用）。有 24 小时内缓存过的播放器时直接沿用，否则未登录从 `iframe_api`（约 1 KB）取当前播放器 id。
2. **InnerTube `/youtubei/v1/player`**：未登录依次请求 `tv_samsung`、`visionos`，带登录 Cookie 时请求 `tv_samsung`、`web_embedded`。`tv_samsung` / `visionos` 的 HTTPS 直链不需要 PO Token；`web_embedded` 自 2026-11 起被 yt-dlp 标为需要 GVS PO Token（登录态直链会 403），所以排在后面，只补前者没有的格式。web / mweb / android / ios 的直链没有 PO Token 会 403，yt-dlp 同样丢弃它们的格式。年龄限制或「为儿童打造」的视频追加 `web_embedded` 回退。请求带播放器的 `signatureTimestamp`，登录态另带 SAPISIDHASH 授权头。
3. **签名与 n 挑战**：直接复用 yt-dlp 官方求解器 [yt-dlp-ejs](https://github.com/yt-dlp/ejs) 0.8.0（`vendor/yt-dlp-ejs/`，打包时原样置于 `resolve.js` 顶部，在插件自己的 QuickJS 里运行）。求解用 TV 变体播放器 `tv-player-ias.vflset/tv-player-ias.js`（yt-dlp 默认 `player_js_variant=tv`）：同一播放器 id 的各变体签名 / n 算法相同，而网页主变体 `player_ias.vflset/en_US/base.js` 自 2026-11 起换成了 ejs 0.8.0 解析不了的构建（报 `addOnDisposeCallback is not a function`）。所有格式的挑战一次批量求解；缺签名或 n 结果的格式直接丢弃，不会交出必定被限速或 403 的直链。
4. **格式**：跳过 DRM、直播分片、OTF、没有地址的 SABR-only 格式、动态范围压缩音轨、疑似损坏（时长不足一半）与全景投影格式；按 itag + 音轨 + DRC 去重，先请求的客户端优先。

### 播放器缓存与内存

ejs 首次处理某个播放器时要把约 3 MB 的播放器 JS 解析成 AST：QuickJS 里约 2.5 s（Apple Silicon；ARM NAS 等弱 CPU 上可慢一个数量级）、峰值约 180 MiB 堆，所以 manifest 声明 `memoryLimitMb: 384`（引擎默认 64 MiB）与 `timeoutMs: 120000`（resolve 硬顶；0.6.8 及更早版本的 App 按 30 s 截断，弱 CPU 设备上首次冷启动可能超时）。处理结果（预处理播放器，约 4 MB）缓存进插件工作区（`flux.fs`，最多 3 份，索引在 `flux.storage` 的 `ejs.players`，按 ejs 版本与播放器变体区分），之后同一播放器只需约 0.2 s、约 60 MiB。

YouTube 同时灰度多个播放器构建。24 小时内缓存过的播放器直接沿用（会话 ytcfg 指明的当前播放器有缓存时优先用它），不必每次确认当前构建；这与 yt-dlp `player_js_version=STS@ID` 强制旧播放器同理：`signatureTimestamp` 告诉服务端按哪个版本加密。缓存过期才重新确定当前播放器并冷启动。缓存的播放器求解失败时丢弃该缓存，换当前播放器冷启动重试一次。播放列表建任务的那次解析会顺手预热缓存，避免裂变出的子任务各自冷启动。每次解析的分段耗时（`session` / `player` / `api` / `solve`，毫秒）写在插件日志里。

## 清晰度与格式

- 每个清晰度挑一个最佳视频轨，配同容器的音频轨：MP4 视频配 M4A（AAC），WebM 视频配 WebM（Opus）；容器对不上时落成 MKV。分离轨优先于音画合一格式，原画优先于 AI 超分，多音轨视频优先原声。
- 「编码偏好」：兼容优先时同一清晰度先选 H.264，画质优先时先选 AV1 / VP9。
- 「清晰度」设为「每次询问」时，新建任务弹框列出各清晰度与「仅音频」，60 秒未选按最高画质；其余选项直接下载不超过该高度的最高档。播放列表条目从不弹框，规格取清单条目的首个规格（即设置里的清晰度，「每次询问」视为最高画质）。
- 交给引擎的大小只用 `contentLength`（准确值）；音画合一格式没有准确大小时报 0，引擎边下边确认长度。

## Cookie

从浏览器扩展发起的下载会带上 youtube.com 的 Cookie。带登录 Cookie（`LOGIN_INFO` + `SAPISID` 系）时，插件按登录态请求，可通过「确认你不是机器人」校验，并能下载会员 / 年龄限制视频。在 App 里粘贴链接时任务没有 Cookie，可在设置「备用 Cookie」里填 Cookie 头或 Netscape cookies.txt。

未登录请求是否被要求「确认你不是机器人」主要取决于出口 IP（数据中心 / 代理 IP 很常见）。这时 yt-dlp 自己也会被拒，只能带登录 Cookie。

## 浏览器扩展规则（`rules.json`）

插件启用且已授权时，规则随 plugin 层同步到扩展（id 改写为 `plugin.fluxdown.youtube-down.*`）。plugin 层只经本机 NMH 下发：扩展以远程模式连接 NAS，或在 Android 上运行时收不到这些规则，所以前两条另有同义的官方规则（规则仓 `rules/official/youtube.json` 的 `official.youtube.{ignore-googlevideo,watch-page}`，随扩展官方包快照分发）：

|规则|作用|
|---|---|
|`ignore-googlevideo`|YouTube（含 `youtube-nocookie.com` 嵌入）页面上 `googlevideo.com` 的播放分片不再作为下载资源（它们是 UMP / 带 Range 的流式片段，单独下载只会得到几十字节的坏文件；引擎也会以 `application/vnd.yt-ump` 拒收）|
|`watch-page`|播放页 / Shorts / 直播页（含手机版 `m.youtube.com`）上，主播放器的 `<video>` 一挂上媒体源，就把当前页面 URL 作为该播放器的视频资源（`${page}` 模板字段），并归属这个 `<video>`：浮钮与「下载正在播放的媒体」快捷键直接可用，下载请求带着浏览器里的 YouTube Cookie 交给桌面端 / NAS，由本插件解析|
|`watch-title`|候选名取播放页标题 `ytd-watch-metadata h1`|

## 开发

`resolve.js` 是生成文件（引擎只加载 manifest 指向的单个 classic script），不要手改：

|路径|内容|
|---|---|
|`src/`|插件源码（ES 模块）：`main.js` 入口，`innertube.js` 会话与 API，`clients.js` 客户端定义，`player.js` 播放器缓存与求解，`formats.js` 格式取舍，`playlist.js` 播放列表，`cookies.js` / `sha1.js` 登录态|
|`vendor/yt-dlp-ejs/`|yt-dlp-ejs 发布物原文（含 meriyah / astring 许可证头）与 Unlicense|
|`build.ts`|校验 vendor 的 SHA3-512，拼装 `resolve.js`|
|`test/`|`bun test` 单测（假 YouTube + 假求解器）|

在 `packages/plugin-sdk` 下：`bun examples/youtube-down/build.ts` 重新生成，`bun run gen:check` 校验入库的 `resolve.js` 没有漂移（CI 同跑），`bun test examples/youtube-down` 跑单测。

升级 yt-dlp-ejs：从 `https://github.com/yt-dlp/ejs/releases/download/<版本>/` 下载 `yt.solver.lib.min.js` 与 `yt.solver.core.min.js` 替换 `vendor/yt-dlp-ejs/`，把 `build.ts` 里的哈希换成 yt-dlp `extractor/youtube/jsc/_builtin/vendor/_info.py` 中同版本的值，并同步 `src/player.js` 的 `EJS_VERSION`（旧版本的播放器缓存随之失效）。

## 验收清单

- [ ] 安装后授权对话框只列出 `browserRules` 权限，授权后插件已加载、已启用。
- [ ] App 新建下载粘贴 `https://youtu.be/<id>`：创建任务后弹清晰度框（含 2160p…144p 与「仅音频」），选择后下载、合并为单个文件，文件名为视频标题。
- [ ] 设置「清晰度」= ≤ 720p：同一链接不再弹框，直接下 720p；「仅音频」下出 M4A。
- [ ] 首次解析后插件工作区出现 `ejs-player-<id>.js`；之后的解析不再下载播放器 JS。
- [ ] 粘贴 `https://www.youtube.com/playlist?list=<id>`：新建框预览出条目清单，每条可选规格；确认后按条目建子任务并下载。
- [ ] 浏览器扩展选项页 → 规则：plugin 层出现上面三条规则；禁用插件后再同步，规则清空。
- [ ] 打开 YouTube 播放页：浮钮出现在播放器上（暂停时也在），点击下载交给桌面端；面板 / 弹窗里只有这一个视频候选，没有 `googlevideo.com` 分片。
- [ ] 站内点击另一个视频（SPA 换页）：候选换成新视频，旧视频不残留。
- [ ] Shorts 页上下滑动：浮钮对应当前 Short。
- [ ] 未登录 / 被风控时任务报错文案提示从浏览器发起下载或填写备用 Cookie；从已登录 YouTube 的浏览器发起的下载可以通过。
