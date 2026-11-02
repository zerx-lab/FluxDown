---
title: Manifest 参考
description: manifest.json 全部字段及校验规则、设置项 widget 矩阵、URL pattern 规则，以及安装与加载时的检查方式。
section: plugins
order: 10
sourceHash: "8a6af81a734f"
---

`manifest.json` 位于插件包根部，声明插件是什么、带有哪些入口脚本、向界面贡献什么，以及需要哪些能力。所有键名使用 camelCase。**未知键会被拒绝**——写错字段名会让整份 manifest 校验失败，而不是被静默忽略。

本页是逐字段的参考。如果想按步骤上手，请看[写第一个插件](/docs/zh/plugins/your-first-plugin/)（手写 JavaScript）或 [TypeScript 项目](/docs/zh/plugins/typescript-project/)（SDK、构建与打包）。权限与授权见[权限、授权与审计](/docs/zh/plugins/permissions/)，打包与发布见[打包与插件市场](/docs/zh/plugins/packaging/)。

## 完整示例

```json
{
  "identity": "acme@demo",
  "name": "Acme Demo",
  "version": "1.0.0",
  "description": "解析 Acme 分享链接，并整理下载完成的文件。",
  "homepage": "https://example.com/acme-demo",
  "icon": "icon.png",
  "minAppVersion": "0.9.0",
  "resolvers": [
    { "match": { "urls": ["*://acme.example/share/*"] }, "entry": "resolve.js", "timeoutMs": 15000, "multi": true }
  ],
  "hooks": {
    "entry": "hooks.js",
    "events": ["onDone", "onError", "onCancel"],
    "match": { "urls": ["*://acme.example/*"] }
  },
  "main": { "entry": "main.js", "events": ["onDone"], "background": false },
  "contributes": {
    "views": [{ "id": "dashboard", "title": "看板", "location": "activity" }],
    "commands": [{ "id": "refresh", "title": "刷新", "palette": true }],
    "archive": "archive.js"
  },
  "settings": [
    { "key": "quality", "title": "偏好画质", "type": "string", "widget": "select",
      "options": [ { "value": "hd", "label": "高清" }, { "value": "sd", "label": "标清" } ], "default": "hd" }
  ],
  "permissions": ["archive"]
}
```

插件可以任意组合 `resolvers`、`subscriptions`、`hooks`、`interceptors`、`auth`、`main` 与 `contributes`。一项都不声明的 manifest 合法，只是什么也不做。

## 顶层字段

| 字段 | 必填 | 类型 | 规则 |
|---|---|---|---|
| `identity` | 是 | string | `^[a-z0-9_-]+@[a-z0-9_-]+$`——小写作者、`@`、名称。禁止点号。永久 ID：设置、存储、授权记录与安装目录都以它为键。 |
| `name` | 是 | string | 显示名，不能为空，也不能只有空白字符。 |
| `version` | 是 | string | `主.次.补丁` 三段整数。解析的严格程度见[版本号写法](#版本号写法)。 |
| `description` | 否 | string | 显示在插件列表里。 |
| `homepage` | 否 | string | 项目或作者主页。 |
| `icon` | 否 | string | 包内路径。禁止 `..`、绝对路径、盘符、以 `/` 或 `\` 开头，以及空路径段。 |
| `minAppVersion` | 否 | string | 要求的最低 FluxDown 版本，解析规则同 `version`。见 [`minAppVersion` 的实际行为](#minappversion-的实际行为)。 |
| `resolvers` | 否 | array | **至多一项**。见 [`resolvers[0]`](#resolvers0)。 |
| `subscriptions` | 否 | array | **至多一项**，订阅源 provider。见 [`subscriptions[0]`](#subscriptions0)。 |
| `hooks` | 否 | object | 下载生命周期回调。见 [`hooks`](#hooks)。 |
| `interceptors` | 否 | object | 任务拦截器（请求改写、分片过滤、后处理）。见 [`interceptors`](#interceptors)。 |
| `auth` | 否 | object | 平台登录入口。见 [`auth`](#auth)。 |
| `main` | 否 | object | 常驻脚本，承载可移植视图、命令与后台逻辑。见 [`main`](#main)。 |
| `contributes` | 否 | object | 视图、命令、归档分类、浏览器嗅探规则与页面脚本。见 [`contributes`](#contributes)。 |
| `permissions` | 否 | array | 能力授权，未知值一律拒绝。见 [`permissions`](#permissions)。 |
| `settings` | 否 | array | 声明式设置项。见 [`settings[]`](#settings)。 |

### 版本号写法

`version` 与 `minAppVersion` 都按三个整数比较；第三段数字之后的内容，客户端会忽略。

| 场景 | 接受的写法 |
|---|---|
| FluxDown 客户端（安装与加载） | `MAJOR.MINOR.PATCH`，可带前导 `v`，可带 `-pre` 或 `+build` 后缀（比较前会被截掉）。`1.2.3`、`v1.2.3`、`1.2.3-rc.1` 都解析为 1.2.3。必须恰好三段。 |
| 插件索引与 `fluxdown-plugin validate` | 严格模式：只能是 `MAJOR.MINOR.PATCH`，仅数字、不允许前导零、不允许 `v` 前缀、不允许后缀。 |

请始终写严格形式。客户端能容忍的版本号，发布到市场时仍可能被拒绝。

### `minAppVersion` 的实际行为

运行中的 FluxDown 比 `minAppVersion` 旧（或任一侧无法解析）时，插件**不会加载**。它会出现在插件列表里，加载状态为*失败*，原因是「需要 App ≥ X，当前 Y」，并不是被静默跳过。安装这样的包会被拒绝，提示「安装后未能加载插件（校验失败或版本门槛不满足）」，并回滚。

当你开始使用旧版本不认识的字段、事件或权限时，请同步抬高 `minAppVersion`：旧客户端遇到未知键或未知权限会直接判 manifest 非法，版本门槛则能让用户看到清晰的提示。

## `resolvers[0]`

resolver 把一个页面 URL 解析成一个或多个可下载文件。

```json
{
  "resolvers": [
    {
      "match": { "urls": ["*://host.com/share/*", "*://cdn.host.com/*"] },
      "entry": "resolver.js",
      "timeoutMs": 15000,
      "multi": false
    }
  ]
}
```

| 字段 | 必填 | 规则 |
|---|---|---|
| `match.urls` | 是 | 非空的 URL pattern 列表（规则见 [URL pattern 规则](#url-pattern-规则)）。 |
| `entry` | 是 | 脚本文件，安全相对路径。必须定义 `globalThis.resolve`。 |
| `timeoutMs` | 否 | 单次调用超时（毫秒），不能是 `0`。它替换默认的 10 秒，但无论写多大都被 30 秒硬顶封住。 |
| `memoryLimitMb` | 否 | 单次调用的 QuickJS 堆上限（MiB），不能是 `0`。它替换默认的 64 MiB，超过 512 MiB 按 512 MiB 封顶。只给确实要在脚本里处理大体量数据的 resolver 用（例如把几 MB 的站点脚本解析成语法树）；上限只是天花板，不会预先占用内存。 |
| `multi` | 否 | 布尔值，默认 `false`。`true` 声明该 resolver 可能返回多文件清单，并参与**前置预解析**（创建任务之前先列出文件清单的预览）。`false` 时，单文件 resolver 不会为了预览被白白提前执行一次；如果它在任务开始时返回了多文件清单，引擎仍会把它拆成多个任务，所以 `multi` 只是前置预解析的声明，不是返回清单的前提。 |

`resolve(ctx)` 的契约与返回结构见 [API 参考](/docs/zh/plugins/api-reference/)。

## `subscriptions[0]`

订阅 provider 为订阅功能提供 RSS 之外的内容来源，例如某个站点用 API 列出的新剧集。

```json
{
  "subscriptions": [
    { "providerId": "acme-feed", "entry": "subscribe.js", "timeoutMs": 20000 }
  ]
}
```

| 字段 | 必填 | 规则 |
|---|---|---|
| `providerId` | 是 | `^[a-z0-9_-]{1,64}$`。必须稳定：它会写入订阅记录。`rss` 是内置 provider 的保留字，会被拒绝。 |
| `entry` | 是 | 脚本文件，安全相对路径。必须定义 `globalThis.subscribe`，函数名固定。 |
| `timeoutMs` | 否 | 单次调用超时（毫秒），不能是 `0`。默认 10 秒，可设置到 30 秒硬顶。 |

每个插件至多一个 provider。`subscribe(ctx)` 的契约、条目结构以及对返回条目的校验见[订阅源指南](/docs/zh/plugins/subscription-guide/)。

## `hooks`

hooks 在下载生命周期事件发生时运行你的代码。

```json
{
  "hooks": {
    "entry": "hooks.js",
    "events": ["onStart", "onDone", "onError", "onCancel"],
    "match": { "urls": ["*://host.com/*"] }
  }
}
```

| 字段 | 必填 | 规则 |
|---|---|---|
| `entry` | 是 | 脚本文件，安全相对路径。须为每个订阅的事件各定义一个同名全局函数。 |
| `events` | 是 | 非空列表，每项只能取 `onStart`、`onError`、`onDone`、`onMetaProbed`、`onCancel`，其余一律拒绝。 |
| `match` | 否 | URL 过滤器，pattern 规则同上；存在时 `urls` 不能为空。省略 = 所有任务都触发。 |

注意：同一插件如果还声明了 resolver，`onMetaProbed` 对它的任务永远不会触发——带 resolver 的任务直接跳过元数据探测。若你仍然订阅了它，FluxDown 会记一条警告日志。

事件参数、`requestRetry`、ffmpeg/ffprobe 与 yt-dlp 后处理见[事件钩子指南](/docs/zh/plugins/hooks-guide/)。

## `interceptors`

拦截器让插件在引擎运行任务时读取并修改任务：请求身份、并发、流媒体分片、后处理与重试决策。每个插件至多声明一个 `interceptors` 对象，并且需要 `intercept` 权限（较高风险，见[权限](/docs/zh/plugins/permissions/)）。

```json
{
  "permissions": ["intercept"],
  "interceptors": {
    "entry": "intercept.js",
    "stages": ["prepare", "segments"],
    "match": { "urls": ["https://example.com/*"] },
    "timeoutMs": 3000,
    "onFailure": "continue"
  }
}
```

| 字段 | 必填 | 规则 |
|---|---|---|
| `entry` | 是 | 脚本文件，安全相对路径。必须定义唯一的全局函数 `intercept(ctx)`，由它按 `ctx.stage` 分派。 |
| `stages` | 是 | 非空且不重复，每项只能取 `prepare`、`probed`、`refresh`、`selectVariant`、`selectPeriod`、`segments`、`liveSegments`、`finalize`、`failed`。未知阶段会被拒绝并列出合法值。 |
| `match` | 是 | URL 过滤器，pattern 规则同上，`urls` 不能为空。任务的原始 URL 或解析后的 URL 命中任一 pattern 即被拦截。 |
| `timeoutMs` | 否 | 单次调用的墙钟超时，不能是 `0`。默认 5 秒，受硬顶限制；`liveSegments` 另被限制在 2 秒内，`refresh` 另被限制在 5 秒内。 |
| `onFailure` | 否 | `continue`（默认）或 `fail`。`continue` 时该插件的报错或非法返回值会记入任务活动并被忽略；`fail` 时任务失败，错误为 `[插件] <identity>（<阶段>）：<消息>`。 |

校验在 `auth` 之后进行，顺序为：`entry` 路径 → `stages` 非空 → 每个阶段都已知 → 阶段不重复 → `match.urls` 非空 → `timeoutMs ≠ 0` → `permissions` 含 `intercept`。入口文件必须在包内存在，并与 `hooks.entry` 一样在加载时读取。各阶段的上下文、返回值、`RequestPatch` 规则与 `defineInterceptors` 见[拦截器指南](/docs/zh/plugins/interceptors-guide/)。

## `auth`

```json
{
  "auth": { "entry": "auth.js", "timeoutMs": 20000 }
}
```

| 字段 | 必填 | 规则 |
|---|---|---|
| `entry` | 是 | 脚本文件，安全相对路径。必须定义 `globalThis.authenticate`。 |
| `timeoutMs` | 否 | 单次 `authenticate` 调用超时（毫秒），不能是 `0`。它替换默认的 30 秒，但同样被 30 秒硬顶封住；与 `resolvers[0].timeoutMs` 是相互独立的预算。 |

声明 `auth.entry` 时 `permissions` 必须包含 `"auth"`，否则校验失败。五个登录 action 与 `flux.auth` 的完整接口见[账号登录指南](/docs/zh/plugins/auth-guide/)与 [API 参考](/docs/zh/plugins/api-reference/#authenticatectx)。

## `main`

```json
{ "main": { "entry": "main.js", "events": ["onDone"], "background": false } }
```

| 字段 | 必填 | 规则 |
|---|---|---|
| `entry` | 是 | classic 脚本（不是 ES module），包内安全相对路径，扩展名 `.js`，文件必须存在。 |
| `events` | 否 | 投递给脚本的事件，每项须是 `onStart`、`onError`、`onDone`、`onMetaProbed`、`onCancel` 之一。 |
| `background` | 否 | 布尔值。`true` 表示脚本随 daemon 常驻，需要 `background` 权限。 |

`main` 是插件的常驻部分：可移植视图、命令与后台逻辑都在 daemon 内的这个脚本里运行。见[可移植界面指南](/docs/zh/plugins/ui-guide/)。

## `contributes`

```json
{
  "contributes": {
    "views": [{ "id": "dashboard", "title": "看板", "location": "activity",
                "desktop": { "entry": "ui/dashboard.js", "networkHosts": ["api.example.com"] },
                "portable": true, "icon": "icons/dashboard.svg" }],
    "commands": [{ "id": "refresh", "title": "刷新", "palette": true, "taskMenu": false }],
    "sniffRules": "rules.json",
    "pageScripts": [{ "id": "hello", "title": "你好", "matches": ["https://*.example.com/*"], "js": ["page/hello.js"] }],
    "archive": "archive.js"
  }
}
```

`views` / `commands` 为空数组等同于不声明。`archive` 会在文件名已知后为跟随分类路由的任务选择一个子文件夹；它不会改文件名，也不会接管保存根目录。

### `archive`

`contributes.archive` 是指向包内 `.js` 文件的包内相对路径，且必须声明 `permissions: ["archive"]`（风险**较高**）。规则：

- 路径必须是包内相对路径：不能含 `..`，不能是绝对路径或带盘符。
- 扩展名必须是 `.js`；`.mjs` 会被拒绝。
- 文件必须存在于包内。
- 安装时会像其他入口一样做编译检查：脚本会在一个没有 `flux` 和 `console` 的空白上下文里求值一次，所以顶层代码不能调用它们。

支持该键和权限之前的旧版宿主会把它们当作无效 manifest 拒绝（未知键、未知权限），所以请把 `minAppVersion` 设为下一个版本或更高。

入口是 `globalThis.archive = (ctx) => result`，其中 `ctx = { fileName }`。它返回 `{ folderName: string }`，或用 `null` / `undefined` 表示不使用子文件夹，可以是同步或异步函数。仅当任务使用分类路由、且路由选中了某个分类目录（命中且带目录的分类规则，或“其他”兜底目录）时才会调用；选定的文件夹之后会被复用，文件名永远不会被改变。脚本里只有 `flux.settings`、`flux.logger` 和 `console`。完整行为（调用时机、多个插件、`folderName` 规则、失败处理、限制）见 [API 参考](/docs/zh/plugins/api-reference/#archivectx)。

### `views[]`

至多 16 项。

| 字段 | 必填 | 规则 |
|---|---|---|
| `id` | 是 | 匹配 `^[a-z][a-z0-9-]{0,31}$`，在视图间唯一。 |
| `title` | 是 | 1–40 个字符。 |
| `location` | 是 | `activity`、`settings`、`taskDetail`、`window` 或 `auth`；`settings` 至多一个视图使用，`auth` 也至多一个。`auth` 视图要求顶层声明 `auth`（`auth.entry`，后者又要求 `auth` 权限），否则 manifest 报错 `location 为 auth 的 view 要求声明 auth`。它会替换桌面端（GPUI）与 Web 端插件**登录**对话框的内容（Web 端只显示可移植实现）；移动端不渲染它，仍用默认登录流程。见[自定义登录界面](/docs/zh/plugins/auth-guide/#自定义登录界面)。不认识该 location 的旧版本应用会拒绝加载此插件，请把 `minAppVersion` 设为下一个版本或更高。 |
| `desktop` | 否 | 桌面原生实现，见下。**没有** `desktop` 的视图是可移植视图，要求声明 `main`。 |
| `portable` | 否 | 布尔值，默认 `false`。仅允许与 `desktop` 同时声明。`true` 表示 `main` 也通过 `flux.ui.view` 实现了该视图，让不能运行桌面脚本的客户端也能显示；要求声明 `main`。 |
| `icon` | 否 | `.svg` 文件的包内相对路径（任何 `location` 都允许，目前仅活动栏 `activity` 使用）。至多 16 KiB（16384 字节），合法 UTF-8，且是 SVG 文档（以 `<svg` 或 `<?xml` 开头并包含 `<svg`）。宿主把它当作**单色蒙版**按主题色着色，只使用 alpha 通道，因此建议 24×24 的 `viewBox`，并使用 `stroke="currentColor"` 或 `fill="currentColor"`（lucide 风格）；彩色会被压成单色。不声明则使用默认图标。 |

视图在哪里显示，取决于它的声明方式：

| 声明方式 | 桌面（GPUI） | Web | 移动端 |
|---|---|---|---|
| 无 `desktop`（可移植） | 由 `main` 渲染 | 由 `main` 渲染 | 不渲染任何内容 |
| `desktop` + `portable: true` | 运行桌面原生脚本；若本机读不到该插件的脚本包，则回退到 `main` 的实现 | 由 `main` 渲染 | 不渲染任何内容 |
| 仅 `desktop` | 运行桌面原生脚本；若本机读不到该插件的脚本包（例如 daemon 在远端主机上），GPUI 客户端显示「仅桌面端可用」提示 | 完全不显示——Web 只列出可移植视图，不会出现「仅桌面端可用」卡片 | 不渲染任何内容 |

想让同一个视图在 Web 上也可用，就声明 `portable: true`，把桌面脚本当作增强。节点树与事件的细节见[视图参考](/docs/zh/plugins/views/)与[可移植界面指南](/docs/zh/plugins/ui-guide/)；原生脚本见[桌面原生界面指南](/docs/zh/plugins/desktop-ui-guide/)。

### `views[].desktop`

| 字段 | 必填 | 规则 |
|---|---|---|
| `entry` | 是 | 界面脚本，包内安全相对路径，扩展名只能是 `.js` 或 `.mjs`，文件必须存在于包内。 |
| `width` / `height` | 否 | 初始窗口尺寸（逻辑像素），仅当视图的 `location` 为 `window` 时允许。有限数，范围 `[240, 4096]`。 |
| `networkHosts` | 否 | 界面脚本可连接的网络主机，每个视图至多 64 项；每项须是小写 DNS 主机名（RFC 1123 标签，总长不超过 253 个字符）或 IP 字面量，不带 scheme、端口、路径或通配符，且在视图内不得重复。 |

任何带 `desktop` 的视图都要求声明 `desktop` 权限。`desktop.entry` 是由 GPUI 桌面客户端内置的 GPUI Shell 执行的 ES module（可 `import` 自 `"gpui-kit"`、`"gpui-base"`、`"gpui-component"`）。引擎和 daemon **从不**编译或执行它，只校验路径并确认文件存在。Web 与移动端不会运行它。

`networkHosts` 是强制生效的白名单：脚本的网络访问被限制在这些主机名之内。文件系统、进程与剪贴板访问则**不受**限制——脚本在桌面进程内以完全信任运行，请只安装你信任的插件，见[权限](/docs/zh/plugins/permissions/#完全信任桌面脚本)。所有视图 `networkHosts` 的并集会显示在授权对话框里，是你批准内容的一部分。

### `commands[]`

至多 32 项，要求声明 `main`。

| 字段 | 必填 | 规则 |
|---|---|---|
| `id` | 是 | 规则同视图 id，在命令间唯一。 |
| `title` | 是 | 1–40 个字符。 |
| `palette` | 否 | 布尔值。是否出现在命令面板。 |
| `taskMenu` | 否 | 布尔值。是否出现在任务右键菜单。 |

### `sniffRules`

浏览器扩展嗅探规则包的 `.json` 安全相对路径，需要 `browserRules` 权限。

- 文件必须存在于包内、不超过 256 KiB、能解析为 JSON，且含 `"format": "fluxdown-sniff-pack"` 与一个至多 500 条的 `rules` 数组。
- 每条规则的格式见[嗅探规则参考](/docs/zh/browser-extension/sniff-rules/)。

最小可用的 `rules.json`：

```json
{
  "format": "fluxdown-sniff-pack",
  "packId": "acme-demo",
  "sequence": 1,
  "engine": 1,
  "issuedAt": "2026-10-17T00:00:00Z",
  "rules": [
    {
      "id": "video",
      "version": 1,
      "tier": "plugin",
      "stage": "response",
      "match": { "host": ["acme.example"], "ext": ["xyz"] },
      "action": { "kind": "resource", "type": "video" }
    }
  ]
}
```

规则如何落到浏览器扩展里：

- 只有**已启用、已加载且已授权**的插件，其规则才会提供给扩展。停用插件，或有新增权限尚未批准时，它的规则会随之消失。
- 每条规则的 `id` 会被改写为 `plugin.<identity 中的 @ 换成 .>.<你写的 id>`——上面的规则会变成 `plugin.acme.demo.video`。改写后超过 64 个字符的规则会被丢弃（并记日志）。
- 无论你写什么，`tier` 都会被强制为 `plugin`。不是对象或缺少字符串 `id` 的规则会被丢弃。
- 插件规则的优先级低于 `local` 和 `official`，高于 `community` 与 `built-in`。

### `pageScripts[]`

由浏览器扩展在网页中运行的脚本。至多 16 条；需要 `pageScripts` 权限（**完全**风险——脚本可读取并修改每个匹配站点的页面内容与用户在该站点的登录状态）。使用指南见[页面脚本](/docs/zh/plugins/page-scripts-guide/)。

```json
{
  "permissions": ["pageScripts"],
  "contributes": {
    "pageScripts": [{
      "id": "bili-button",
      "title": "下载按钮",
      "matches": ["https://*.bilibili.com/*"],
      "excludeMatches": ["https://www.bilibili.com/blackboard/*"],
      "js": ["page/bili.js"],
      "runAt": "document_idle",
      "world": "isolated",
      "allFrames": false
    }]
  }
}
```

| 字段 | 必填 | 规则 |
|---|---|---|
| `id` | 是 | 匹配 `^[a-z0-9][a-z0-9_-]{0,31}$`；在该插件的页面脚本中唯一。 |
| `title` | 是 | 1–40 个字符。显示在授权对话框与插件详情里。 |
| `matches` | 是 | 1–32 条匹配模式（见下）。 |
| `excludeMatches` | 否 | 0–32 条匹配模式，规则相同；默认 `[]`。 |
| `js` | 是 | 1–8 个文件；包内的安全相对路径，扩展名 `.js`，不得重复。 |
| `runAt` | 否 | `document_start`、`document_end` 或 `document_idle`（默认）。 |
| `world` | 否 | `isolated`（默认）：运行在扩展的用户脚本 world，带有 `fluxdown` 桥。`main`：运行在页面自己的 JavaScript world，**没有**桥。 |
| `allFrames` | 否 | 布尔值，默认 `false`。 |

匹配模式形如 `<scheme>://<host><path>`：

- `scheme` 为 `http`、`https` 或 `*`。
- `host` 为域名或 `*.域名`。域名只含 `a-z`、`0-9`、`.`、`-`，必须含 `.`，且不得以 `.` 或 `-` 开头或结尾。允许 IPv4 字面量（同样含点）。
- `path` 必须以 `/` 开头。
- 拒绝：`<all_urls>`、单独的 `*` 主机、端口以及任何其他特殊形式。

文件内容规则在安装或加载包时检查：每个 `js` 文件不超过 256 KiB 且为合法 UTF-8，同一条脚本的文件拼接后不超过 384 KiB。文件按 `js` 顺序以 `"\n;\n"` 连接；拼接结果 UTF-8 字节的 SHA-256 用来标识该脚本的版本。

## `permissions`

插件主动申请的额外宿主能力。为空或省略 = 基础沙箱（`flux.fetch` 网络、`flux.storage`、日志、视图与命令贡献）。识别以下值：

| 值 | 授予 |
|---|---|
| `ffmpeg` | `flux.ffmpeg` / `flux.ffprobe` 接口——对下载完成的文件运行解析到的 ffmpeg。 |
| `ytdlp` | `flux.ytdlp` 接口——在 `resolve` 或任意 hook 里运行解析到的 yt-dlp。 |
| `auth` | 声明 `auth.entry` 平台登录入口，并允许 `flux.fetch` / `flux.auth` 使用认证凭据。 |
| `tasks` | `flux.tasks.*`——允许在 `main` 中操作下载任务。 |
| `browserRules` | 允许通过 `contributes.sniffRules` 贡献浏览器扩展嗅探规则。 |
| `pageScripts` | 允许扩展在 `contributes.pageScripts` 列出的网页中运行插件脚本（完全风险）。 |
| `desktop` | 允许桌面脚本（`contributes.views[].desktop`）在桌面进程内以完全信任运行。 |
| `background` | 允许 `main` 随 daemon 常驻（`main.background`）。 |
| `intercept` | 允许声明 `interceptors`，在任务运行时读取并修改其请求、分片、画质与完成后的文件。 |
| `archive` | 声明 `contributes.archive`：根据文件名在任务的分类文件夹内选择一个子文件夹。它可以读取插件设置并写入插件日志，不能访问文件、网络、任务或进程（风险较高）。 |

未知值会让整份 manifest 失败。风险等级、授权与审计见[权限、授权与审计](/docs/zh/plugins/permissions/)；各权限背后的接口见 [API 参考](/docs/zh/plugins/api-reference/)。

某些声明要求同时具备对应权限：

| 声明 | 要求 |
|---|---|
| `auth.entry` | `auth` |
| `main.background: true` | `background` |
| 任一 `views[].desktop` | `desktop` |
| `contributes.sniffRules` | `browserRules` |
| `contributes.pageScripts`（非空） | `pageScripts` |
| `contributes.archive` | `archive` |

（`tasks`、`ffmpeg`、`ytdlp` 在调用接口时才检查，manifest 校验不要求。）

## `settings[]`

每一项描述一个字段，应用据此在桌面与 Web 生成表单。key 必须唯一且非空。

```json
{
  "settings": [
    { "key": "apiToken", "title": "API token", "type": "string", "widget": "password", "required": true },
    { "key": "quality",  "title": "偏好画质", "type": "string", "widget": "select",
      "options": [ { "value": "hd", "label": "高清" }, { "value": "sd", "label": "标清" } ],
      "default": "hd" },
    { "key": "maxRetries", "title": "最大重试次数", "type": "number", "min": 0, "max": 10, "default": "3" },
    { "key": "verbose", "title": "详细日志", "type": "boolean", "default": "false" }
  ]
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `key` | 是 | 插件内唯一，不能为空。脚本里用 `flux.settings.<key>` 读回。 |
| `title` | 是 | 表单标签。 |
| `description` | 否 | 字段下方的说明文字。 |
| `type` | 是 | `string`、`number` 或 `boolean`。 |
| `widget` | 否 | 缺省按 type 推导：string→`text`，number→`number`，boolean→`toggle`。 |
| `options` | 仅 select | 非空的 `{ "value", "label" }` 列表。 |
| `default` | 否 | **一律写成字符串**，number 也是（`"3"`），boolean 只能是 `"true"` / `"false"`。select 的 default 必须是某个 option 的 value；number 的 default 必须能解析为有限数且落在 `min` / `max` 区间内。 |
| `required` | 否 | 布尔值。表单拒绝保存空值。 |
| `min` / `max` | 仅 number | 有限数、闭区间、`min ≤ max`。其他类型写了会被拒绝。 |
| `pattern` | 仅 string | **JavaScript RegExp** 语法（不是 Rust regex），保存时对值做校验。安装时会编译一次，不合法的 pattern 会导致安装被拒绝。 |
| `helperScript` | 仅 string | 一段脚本，宿主会在该字段旁渲染一个**复制**按钮。宿主只把脚本文本复制到剪贴板，**绝不执行**。用户把它粘贴到目标站点的浏览器开发者工具 Console 中运行（典型用途：读取 `document.cookie` 并把登录 Cookie 复制回该字段）。须非空、不超过 4 KB。 |
| `helperLabel` | 与 `helperScript` 同时出现 | 按钮文案，不超过 60 个字符。只能与 `helperScript` 一起使用；省略时用通用文案「复制获取脚本」。 |

### widget × type 合法矩阵

只有这些组合合法，其余组合校验失败：

| widget | 允许的 type |
|---|---|
| `text`、`password`、`textarea`、`folder`、`select` | `string` |
| `toggle` | `boolean` |
| `number` | `number` |

## URL pattern 规则

pattern 不是正则，也不是浏览器扩展的 match pattern。完整规则：

- `*` 是**唯一**通配符，匹配任意长度的字符（包括 `/` 和 `:`）。
- 各个 `*` 之间的文本必须按顺序作为子串出现。
- pattern 不以 `*` 开头时，首段必须是 URL 的**前缀**。
- 不以 `*` 结尾时，末段必须是**后缀**。
- 匹配不区分大小写（两侧整体转小写后比较，路径也一样）。
- 单独一个 `*` 匹配一切。

以 `https://www.youtube.com/watch?v=abc` 为例：

| pattern | 结果 |
|---|---|
| `*://www.youtube.com/watch*` | 匹配 |
| `https://x.com/a`（无通配符） | 只做精确匹配 |
| `*://x.com/*` 对 `https://y.com/a` | 不匹配——找不到 `://x.com/` |
| `youtube.com/*` | 不匹配——首段前缀锚定，而 URL 以 `https://` 开头 |

## 校验

manifest 在安装时和每次加载时都会校验。校验失败的插件不会运行：它在列表里显示为失败并带有原因，原因同时写入日志（见[调试与排错](/docs/zh/plugins/debugging/)）。

### 检查顺序

identity 格式 → `name` 非空 → `version` 格式 → `minAppVersion` 格式 → `icon` 路径安全 → resolver 至多一个 → resolver 的 `entry` 路径、`match.urls` 非空、`timeoutMs ≠ 0`、`memoryLimitMb ≠ 0` → subscription 至多一个 → `providerId` 格式且不是 `rss`、`entry` 路径、`timeoutMs ≠ 0` → hooks 的 `entry` 路径、`events` 非空且合法、`match.urls`（如有）非空 → auth 的 `entry` 路径、含 `auth` 权限、`timeoutMs ≠ 0` → `main`（路径、`.js`、`events` 合法、`background` 权限）→ `contributes`（视图与命令的数量上限和 id、`settings` 位置至多一个、`portable` 只能与 `desktop` 同时声明、可移植视图与命令要求 `main`、desktop 的入口 / 尺寸 / 主机规则、`desktop` 权限、视图 `icon` 的路径 / `.svg` 扩展名、`sniffRules` 的路径 / 扩展名 / `browserRules` 权限、`archive` 的包内相对路径 / `.js` 扩展名 / `archive` 权限，然后是 `pageScripts`：至多 16 条 → `pageScripts` 权限 → 逐条脚本：`id` 格式与唯一性、`title` 长度、`matches` 数量与模式、`excludeMatches` 数量与模式、`js` 数量、安全的 `.js` 路径、不重复）→ `settings`（key 唯一、逐字段规则与 widget × type 矩阵）→ `permissions` 全为已知值。

### 必须存在的文件

安装与加载时，包内必须有每个已声明的入口脚本（`resolvers[0].entry`、`hooks.entry`、`auth.entry`、`subscriptions[0].entry`、`main.entry`、`contributes.archive`）、每个 `desktop.entry`、每个视图 `icon`（还要通过上文的图标内容检查）、`sniffRules` 指向的文件（后者还要通过上文的内容检查），以及页面脚本 `js` 列出的每个文件（每个不超过 256 KiB 且为合法 UTF-8，同一条脚本拼接后不超过 384 KiB）。缺失时的错误是「包内缺少文件 '<路径>'」；入口脚本存在但无法按 UTF-8 文本读取时，错误是「读取入口文件 '<路径>' 失败: …」。开发模式插件同样适用：插件会被列为**加载失败**，而不是等到第一次调用时才出错。

### 安装时的编译检查

语法错误会在安装时当场拒绝，而不是等到第一次下载才发现。编译检查覆盖 **resolver、hooks、interceptors、auth、订阅和 archive** 入口脚本（按声明的种类，最多六个）。每个脚本会在一个没有 `flux` 和 `console` 的空白上下文里求值一次，所以顶层代码不能调用它们。**不**包括 `main`（按需编译）和桌面脚本（引擎从不编译）。每个 `settings[].pattern` 也会在安装时按 JavaScript RegExp 编译一次，不合法则拒绝安装。

安装失败不会留下任何改动：首次安装不会留下目录，更新则恢复到旧版本。

### 发布前自检

`fluxdown-plugin validate` 会运行同样的规则，并加上更严格的上架检查（严格版本号格式、所有入口与图标必须在包内、条目与体积上限）。见 [SDK 参考](/docs/zh/plugins/sdk/)；发布前手动校验的做法见[打包](/docs/zh/plugins/packaging/#发布前本地自检)。
