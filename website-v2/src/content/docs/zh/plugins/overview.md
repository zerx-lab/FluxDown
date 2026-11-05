---
title: 插件系统概览
description: FluxDown 插件能做什么、如何选择开发路线（纯 JavaScript 或 TypeScript SDK）、怎么运行、怎么安装，以及各客户端的支持情况。
section: plugins
order: 1
sourceHash: "51d8d7d98214"
---

FluxDown 插件是一个文件夹（或这个文件夹打成的 `.fxplug` zip 包），里面有一份 `manifest.json` 和若干 JavaScript 文件。manifest 声明插件贡献了什么，脚本则由 FluxDown 在后台服务的沙箱里，于合适的时机调用。

插件可以：

- 在任务开始前改写它的 URL（把分享页链接变成直链）；
- 响应任务事件，对下载完成的文件做后处理；
- 登录站点，并复用登录凭据；
- 在内置 RSS 之外提供新的订阅源；
- 给应用添加页面、设置页签、任务详情页签和命令；
- 向浏览器扩展贡献嗅探规则。

## 你想做什么？

下表每一行都链接到对应能力的教程，以及列出全部字段的参考页。

| 能力 | 在 `manifest.json` 中声明 | 所需权限 | 教程 | 参考 |
|---|---|---|---|---|
| **解析 URL**——分享链接 → 直链 | `resolvers` | 无 | [写第一个插件](/docs/zh/plugins/your-first-plugin/) | [API 参考](/docs/zh/plugins/api-reference/) |
| **响应任务事件**，对完成的文件运行 ffmpeg / ffprobe / yt-dlp | `hooks` | `ffmpeg`、`ytdlp`（仅用到这些工具时） | [事件钩子指南](/docs/zh/plugins/hooks-guide/) | [API 参考](/docs/zh/plugins/api-reference/) |
| **拦截下载**——读取并修改任务的请求（URL、请求头、Cookie、请求体）、连接数、流媒体分片、下载中途的凭据以及完成后的文件；决定失败任务是否重试 | `interceptors` | `intercept` | [拦截器指南](/docs/zh/plugins/interceptors-guide/) | [API 参考](/docs/zh/plugins/api-reference/) |
| **账号登录**（扫码、验证码等）与带凭据的请求 | `auth` | `auth` | [账号登录与认证指南](/docs/zh/plugins/auth-guide/) | [API 参考](/docs/zh/plugins/api-reference/) |
| **订阅源 provider** | `subscriptions` | 无 | [订阅源指南](/docs/zh/plugins/subscription-guide/) | [API 参考](/docs/zh/plugins/api-reference/) |
| **可移植界面**——页面、设置页签、任务详情页签、命令、定时器；同一份代码在桌面端和 Web 端都能渲染 | `main`、`contributes.views`、`contributes.commands`、`settings` | 管理任务用 `tasks`，常驻用 `background` | [可移植界面指南](/docs/zh/plugins/ui-guide/) | [视图](/docs/zh/plugins/views/) |
| **桌面原生界面**——拥有完整桌面权限的 GPUI 脚本 | `contributes.views[].desktop` | `desktop` | [桌面原生界面指南](/docs/zh/plugins/desktop-ui-guide/) | [视图](/docs/zh/plugins/views/) |
| **浏览器嗅探规则** | `contributes.sniffRules` | `browserRules` | [Manifest 参考](/docs/zh/plugins/manifest/) | [Manifest 参考](/docs/zh/plugins/manifest/) |
| **浏览器页面脚本**——通过浏览器扩展在列出的网站中运行你的 JavaScript，并与插件互通 | `contributes.pageScripts` | `pageScripts` | [页面脚本](/docs/zh/plugins/page-scripts-guide/) | [Manifest 参考](/docs/zh/plugins/manifest/#pagescripts) |
| **插件设置表单** | `settings` | 无 | [写第一个插件](/docs/zh/plugins/your-first-plugin/#6-添加设置项可选) | [Manifest 参考](/docs/zh/plugins/manifest/) |

基础沙箱**不能**创建任务、不能读取任意文件，只能通过 `flux.*` 接口与外界交互（HTTP 请求、键值存储、临时文件、日志、请求重试）。超出这些的能力都是权限：由插件声明、用户批准，并记入审计日志，见[权限](/docs/zh/plugins/permissions/)。

## 选择开发路线

同一种插件格式有两种写法。两者产出的都是「`manifest.json` 加经典脚本 `.js`」的文件夹，引擎无法区分。

| | 纯 JavaScript | SDK + TypeScript |
|---|---|---|
| 工具 | 文本编辑器 | Node.js 20+ 与 `fluxdown-plugin-sdk` |
| 构建步骤 | 无 | `npm run build` 把 `src/` 打包进 `dist/` |
| `flux.*` 的类型与自动补全 | 无 | 有 |
| 多个源文件与 import | 不支持——每个入口一个脚本 | 支持——每个入口打包成一个脚本 |
| 用 JSX 写视图 | 不支持——用 `flux.ui.h` 拼节点 | 支持 |
| 安装前的 manifest 校验 | 安装时在应用内进行 | 本地 `npm run validate`，规则与应用、插件索引一致 |
| 适合 | 单个 resolver 或 hook、快速试验 | 带界面、多入口，或需要长期维护的代码 |

想不带任何额外工具弄懂插件格式，从[写第一个插件](/docs/zh/plugins/your-first-plugin/)开始；已经确定要类型、界面或多个入口，就从 [TypeScript 项目](/docs/zh/plugins/typescript-project/)开始，所有命令见 [SDK 参考](/docs/zh/plugins/sdk/)。两条路可以随时切换——manifest 是一样的。

## 插件怎么运行

FluxDown 有两种执行模型，取决于入口类型。

| | 无状态入口 | 常驻 `main` |
|---|---|---|
| 入口 | `resolvers[].entry`、`hooks.entry`、`interceptors.entry`、`auth.entry`、`subscriptions[].entry` | `main.entry` |
| 生命周期 | 每次调用都是**全新的** JavaScript 上下文（QuickJS），调用之间什么都不保留，要持久化请用 `flux.storage` | **每个插件一个长期存活的上下文**，全局变量在调用之间保留 |
| 由谁调用 | 下载（resolve、hooks、interceptors）、登录对话框、订阅刷新 | 视图、命令、定时器、`flux.events` 订阅 |
| 何时启动 | 每次调用按需启动 | 打开视图或收到事件时按需启动；没有会话、定时器和进行中调用满 5 分钟后回收，除非 `main.background` 为 `true`（需要 `background` 权限） |
| 并发 | 各次调用互相独立 | 同一插件的调用**逐个串行**：一个慢 `await` 会拖住该插件所有视图、命令和定时器 |
| 额外接口 | `flux.*` 核心接口 | 核心接口，加上 `flux.ui`、`flux.commands`、`flux.rpc`、`flux.events`、`flux.timers`，以及声明 `tasks` 权限后的 `flux.tasks` |

脚本按经典脚本（classic script）加载，不是 ES 模块。入口函数必须落在 `globalThis` 上（顶层的 `function resolve(ctx) {...}` 天然满足；用 SDK 时由 `defineResolver` 等辅助函数代劳）。

> **顶层代码不要碰 `flux.*`。** 安装或重新加载插件时，FluxDown 会在一个空白的 JavaScript 上下文里把每个 resolver、hooks、interceptors、auth、subscription 脚本求值一次，以检查它能否编译。那里没有 `flux` 和 `console`，所以顶层写 `flux.logger.info(...)` 会让安装失败，报 `脚本编译失败: …`。请把逻辑放进入口函数里。

### resolver

- **惰性。** FluxDown 只在任务上记录你的插件 ID，从不保存解析结果。`resolve(ctx)` 在**每次**开始和恢复时都会重新执行。网盘直链通常会过期，重新解析才能让旧任务继续下载。
- **不占主循环。** 解析在专用线程池上执行，脚本再慢也冻结不了应用。
- **失败即失败（fail-closed）。** 脚本抛异常、超时、返回值不合法，任务都会进入错误状态，错误信息带 `[插件] <identity>:` 前缀。FluxDown 绝不会退回去下载原始页面 URL——那会把一个 HTML 页面当视频文件存盘。用户可以在失败任务上用**忽略插件重试**显式绕过出问题的插件。

多个启用中的插件同时匹配一个 URL 时，这类任务首次开始前 FluxDown 会询问用户使用哪个插件（列出全部匹配插件，按 `identity` 排序），并可记为该站点的默认插件；用户在「设置 → 扩展 → 站点默认」中管理这些记录。无人值守的下载（RSS、远程派发、跳过二次选择的免打扰下载）不会询问：使用站点默认插件，未设置时使用 `identity` 字典序最小的插件。

带 resolver 的任务会跳过常规的元数据探测，所以同一个插件如果又订阅了 `onMetaProbed`，这个钩子对它自己的任务永远不会触发（加载时日志里会有一条警告）。

### hooks

hooks 是发出即不管的通知：

- 钩子抛异常或超时，只记一行日志（含 `执行失败（已忽略）`）然后忽略，永远影响不了任务。
- 钩子运行时忙不过来时，通知直接丢弃（日志为 `钩子并发已满，本次钩子被丢弃`），不排队。
- 钩子影响任务的唯一途径是 `flux.task.requestRetry({ delayMs })`，且只在 `onError` 里有效。

详见[事件钩子指南](/docs/zh/plugins/hooks-guide/)。

### interceptors

拦截器是一个 `intercept(ctx)` 入口，FluxDown 在 manifest 列出的阶段调用它（`prepare`、`refresh`、`probed`、`selectVariant`、`selectPeriod`、`segments`、`liveSegments`、`finalize`、`failed`）。它在 daemon 上运行，针对 URL 命中 `interceptors.match.urls` 的任务，需要 `intercept` 权限，并在每次开始、恢复和重试时重新执行。调用失败时，按 `onFailure` 的设置，要么跳过并在任务活动日志里留一条记录，要么让任务失败。详见[拦截器指南](/docs/zh/plugins/interceptors-guide/)。

### 限制

| 限制 | 值 |
|---|---|
| resolve / 订阅超时 | 默认 10 秒；入口上的 `timeoutMs` 可调整，硬顶 30 秒 |
| auth 超时 | 默认 30 秒；`auth.timeoutMs` 可调整，硬顶 30 秒 |
| 拦截器超时 | 默认 5 秒；`interceptors.timeoutMs` 可调整，硬顶 30 秒。`liveSegments` 上限 2 秒，`refresh` 上限 5 秒。带 `ffmpeg` 或 `ytdlp` 权限的插件，其 `finalize` 使用与 hooks 相同的外部工具长预算 |
| resolve、auth、订阅、拦截器内存 | 64 MB |
| hook 超时 / 内存 | 5 秒 / 32 MB；带 `ffmpeg`（在 `onDone` 中）或 `ytdlp` 权限的 hook 墙钟预算长得多，见[事件钩子指南](/docs/zh/plugins/hooks-guide/) |
| `main` 单次调用 | 墙钟 30 秒、CPU 2 秒、内存 64 MB |
| 熔断器 | **resolver、拦截器、订阅或 `main`** 的调用连续 3 次超时或内存超限 → 插件自动禁用；出现其他结果即清零。hooks 与 `authenticate` 调用不计入 |

被自动禁用的插件会在应用里弹出提示，并显示**已自动禁用**徽标。修好脚本后，在**设置 → 扩展 → 插件**中手动重新启用即可（安装新版本也会自动重新启用）。各项限制与报错详见 [API 参考](/docs/zh/plugins/api-reference/)和[调试与排错](/docs/zh/plugins/debugging/)。

## 安装插件

在桌面应用或 Web 端打开**设置 → 扩展 → 插件**，有三条路：

- **安装 .fxplug / .zip 插件**——上传一个 `.fxplug` 文件（就是插件文件夹打的 zip，见[打包与市场](/docs/zh/plugins/packaging/)）。在 Web 端，文件在浏览器里选取，再上传到宿主。仅在**开发模式**开关打开时显示（见下）。
- **从目录安装（开发模式）**——指向一个含 `manifest.json` 的文件夹。FluxDown 只记录文件夹路径，不拷贝。**开发模式**开关（默认关闭，不跨次记忆）控制整个安装区——这个输入框与**安装 .fxplug / .zip 插件**——是否显示；关闭时请从插件市场安装。开发模式插件带「开发模式」徽标和**重新加载**按钮，见下文。
- **插件市场**——在应用内浏览并安装已发布的插件。

目录安装时填写的路径，是**运行 FluxDown 后台服务的那台机器**上的路径，不是浏览器所在机器上的路径。桌面应用里两者是同一台机器；无头服务器或 Docker 容器则要填服务器或容器内的路径——例如把你的 `dist/` 目录挂载进容器，再填容器内的路径。

已安装的（非开发模式）插件放在 `<数据目录>/plugins/<identity>/` 下。新装的插件默认启用；但如果它请求了权限，则会保持禁用并显示**需要授权**，直到你批准（见[权限](/docs/zh/plugins/permissions/)）。

### 改了开发模式插件之后怎么办

| 你改了 | 操作 |
|---|---|
| resolver、hooks、interceptors、auth、订阅脚本 | 什么都不用做——开发模式插件每次调用都重新读这些文件 |
| `main` 脚本、`manifest.json`，或新增入口 | 点插件行上的**重新加载**（提示文字为「重新加载（重读 manifest 与脚本）」）——它会重读 manifest 和全部脚本，并像安装时一样重新检查；失败时保留开发模式登记并显示错误 |
| 桌面界面脚本（`desktop.entry`） | **重新加载**，然后重新打开视图 |

包含日志与报错的完整对照表见[调试与排错](/docs/zh/plugins/debugging/)。

## 客户端支持

插件逻辑始终在 FluxDown 的后台服务（daemon）里运行。各客户端的差别在于能**展示**什么：

| | 桌面应用 | Web 端 | 移动端 |
|---|---|---|---|
| resolver、hooks、interceptors、auth、订阅 | 在 daemon 中运行，对所有客户端生效 | 同左 | 同左 |
| 安装：zip、市场、目录（开发模式） | 支持 | 支持——目录路径是宿主上的路径 | 在已连接的宿主上支持 zip 与市场；不支持目录安装 |
| 可移植视图与命令（`main`） | 支持 | 支持 | 不渲染 |
| 桌面原生视图（`desktop`） | 支持 | 不显示 | 不显示 |
| `window` 视图 | 支持（插件行按钮） | 不显示 | 不显示 |
| `auth` 插件的登录对话框 | 支持 | 支持 | 支持 |

同时带 `desktop` 入口和 `"portable": true` 的视图，桌面端用原生实现，Web 端用可移植实现。详情见[视图](/docs/zh/plugins/views/#客户端支持)。

## 接下来

1. [写第一个插件](/docs/zh/plugins/your-first-plugin/)——用纯 JavaScript 写出能跑的 resolver。
2. [TypeScript 项目](/docs/zh/plugins/typescript-project/)——用 SDK 写 resolver、hook、视图和设置，直到打出 `.fxplug`。
3. 指南：[事件钩子](/docs/zh/plugins/hooks-guide/)、[拦截器](/docs/zh/plugins/interceptors-guide/)、[页面脚本](/docs/zh/plugins/page-scripts-guide/)、[账号登录与认证](/docs/zh/plugins/auth-guide/)、[订阅源](/docs/zh/plugins/subscription-guide/)、[可移植界面](/docs/zh/plugins/ui-guide/)、[桌面原生界面](/docs/zh/plugins/desktop-ui-guide/)。
4. [调试与排错](/docs/zh/plugins/debugging/)——日志、重新加载语义、报错信息与解决办法。
5. 参考：[Manifest](/docs/zh/plugins/manifest/)、[API](/docs/zh/plugins/api-reference/)、[视图](/docs/zh/plugins/views/)、[SDK 与 CLI](/docs/zh/plugins/sdk/)、[权限](/docs/zh/plugins/permissions/)、[打包与市场](/docs/zh/plugins/packaging/)。
