---
title: 插件 API 参考
description: 全部入口函数、flux.* 接口、JavaScript 运行环境、运行时限制，以及插件会遇到的错误信息。
section: plugins
order: 11
sourceHash: "426a6413a002"
---

本页列出插件脚本能看到的一切：FluxDown 会调用的入口函数、注入的 `flux` 对象、JavaScript 运行环境、运行时限制，以及开发中会遇到的错误信息。这是一份参考手册；分步教程请看[你的第一个插件](/docs/zh/plugins/your-first-plugin/)、[TypeScript 项目](/docs/zh/plugins/typescript-project/)、[事件钩子指南](/docs/zh/plugins/hooks-guide/)、[账号登录指南](/docs/zh/plugins/auth-guide/)、[订阅源指南](/docs/zh/plugins/subscription-guide/)、[可移植界面指南](/docs/zh/plugins/ui-guide/)和[桌面原生界面指南](/docs/zh/plugins/desktop-ui-guide/)。

约定：

- 跨越 JS 边界的字段名全部是 camelCase。
- 入口就是普通的全局函数。`async` 函数和返回 Promise 都完全支持——FluxDown 会等待结果。
- 引擎与宿主产生的错误文案是**中文原文**（与界面语言无关），下文逐字引用，便于你在日志中搜索；中文以外的部分只是说明。详见[你会遇到的错误](#你会遇到的错误)。
- 本页的限制都由宿主强制执行，插件无法调高。

## 入口函数

| 入口 | 何时调用 | 由谁声明 | 脚本文件 | 返回值 |
|---|---|---|---|---|
| [`resolve(ctx)`](#resolvectx) | 匹配的任务开始或恢复时（以及新建下载窗口的预解析）。 | `resolvers[0]` | `resolvers[0].entry` | 改写结果，或 `null` / `undefined` |
| [`onStart`](#钩子onstartondoneonerroroncancelonmetaprobed) | 任务首次开始。 | `hooks.events` | `hooks.entry` | 忽略 |
| `onMetaProbed` | 元数据探测得到文件名或大小。 | `hooks.events` | `hooks.entry` | 忽略 |
| `onDone` | 任务成功完成。 | `hooks.events` | `hooks.entry` | 忽略 |
| `onError` | 任务进入失败状态。 | `hooks.events` | `hooks.entry` | 忽略 |
| `onCancel` | 用户关闭了选择对话框（选择插件或选择变体），任务因此被暂停。 | `hooks.events` | `hooks.entry` | 忽略 |
| [`authenticate(ctx)`](#authenticatectx) | 宿主驱动一次登录动作（`begin` / `poll` / `cancel` / `logout` / `status`）。 | `auth.entry` + 权限 `auth` | `auth.entry` | [`AuthResult`](#authenticatectx) 对象 |
| [`subscribe(ctx)`](#subscribectx) | 属于你的 provider 的订阅源被刷新。 | `subscriptions[0]` | `subscriptions[0].entry` | 规范化的订阅内容对象 |
| [`main`](#main-常驻脚本) | daemon（或某个视图）激活你的常驻脚本。 | `main` | `main.entry` | —（用于注册处理器） |
| [`archive(ctx)`](#archivectx) | 授权默认保存目录的任务进入路由时调用，不要求命中分类规则。 | `contributes.archive` + 权限 `archive` | `contributes.archive` 指定的 `.js` 文件 | `{ directories }`、旧格式 `{ folderName }` 或 `null` |

### `archive(ctx)`

这是一个纯本地分类回调，可使用已知文件名与可选来源元数据；不解析 URL，也不访问文件或网络。宿主会对使用授权默认保存目录的任务调用它，不依赖普通分类规则是否命中。用户明确指定保存目录时跳过；BitTorrent 与 eD2K 任务不会移动。每次调用使用新的 QuickJS 运行时，可读取 `flux.settings`，但没有 `flux.fetch`、文件系统、任务或进程接口。

`ctx` 形如 `{ fileName, metadata?, isGroup }`。`fileName` 是当前已知文件名；`metadata` 可选，字段为 `platform`、`mediaKind`（`""`、`"series"` 或 `"post"`）、`creatorId`、`creatorName`、`seriesId`、`seriesTitle`、`collectionId`、`collectionTitle`。每个标签最多 180 UTF-8 字节，且不得含控制字符。这些是来源元数据，不代表读取文件语义内容；宿主不会通过此元数据暴露 URL、本地路径或凭据。

对于任务组，`isGroup` 为 true，宿主只在组根路由一次，成员路径与文件名保持不变。返回 `null` / `undefined` 表示不改变目录；也可返回 `{ directories: ["视频", "哔哩哔哩", "创作者", "合集"] }`，每项是一个安全路径分量，最多 8 层，且相对任务获准使用的默认保存目录，而非分类目录。已有插件仍可返回 `{ folderName: "The Expanse" }`，其原有的分类子目录语义保持不变。空值或不安全输出会被忽略（fail-open），不会使任务失败。多个插件按 identity 排序，首个有效结果生效；目录决定会持久化供恢复和完成期复用，原文件名不变。
拼接后的相对目录最长 180 UTF-8 字节；绝对路径、路径穿越、不安全分量及逃出根目录的链接均会被拒绝。

常驻 `main` 脚本可调用 `flux.archiveRules.status()` 查询当前已验证规则包状态，或调用 `flux.archiveRules.update()` 显式请求更新；两者均要求 `archive` 权限及 `contributes.archiveRules` 声明。规则是由固定发布者公钥验证的有界签名 JSON。启用自动更新后每日检查；获取或验证失败会保留最后一份已验证规则，新规则不会重新分类历史文件。

脚本如何运行：

- 每次调用 `resolve`、钩子、`authenticate` 或 `subscribe` 都会创建一个**全新的 QuickJS 运行时**。脚本文件按经典脚本（classic script）从头到尾执行一遍，然后在 `globalThis` 上查找入口函数。调用之间不保留任何全局状态——需要持久化请用 [`flux.storage`](#fluxstorage) 或 [`flux.fs`](#fluxfs)。
- 顶层 `function` 声明和 `globalThis.name = …` 都能定义入口。脚本文件里的 `export` / `import` 语法无效（[SDK](/docs/zh/plugins/sdk/) 会替你打包成经典脚本）。
- 文件顶层代码在**每次**调用时、入口之前都会执行。顶层抛出异常会让这次调用以 `脚本编译失败: …` 失败。
- `resolve`、`authenticate`、`subscribe` 未定义时，调用失败，错误信息含 `入口 resolve 未定义`（`authenticate` / `subscribe` 同理）。**钩子**未定义则被静默跳过。
- `resolve`、`authenticate`、`subscribe` 的返回值会被 `JSON.stringify` 后交给宿主解析：请直接返回普通对象，不要返回已经序列化的 JSON 字符串（会被二次编码，校验失败）。`undefined`、函数、symbol 会被 `JSON.stringify` 丢弃。
- 类型化的 `flux.settings`、`flux.info` 和入口参数会在脚本运行前注入。`archive` 入口只会得到 `flux.settings`、`flux.logger`、`console` 和它的参数（见 [`archive(ctx)`](#archivectx)）。

### `resolve(ctx)`

在协议分派之前调用，任务每次**开始和恢复**时都会调用（前提是匹配 `resolvers[0].match.urls`；解析是惰性的，见[概览](/docs/zh/plugins/overview/)）。清单声明了 `multi: true` 的 resolver，还会被新建下载窗口调用一次做预解析（见[清单结果](#清单结果manifest)）。

如果某个必填设置既没有用户值也没有 `default`，调用会在你的脚本运行**之前**以 `缺少必填设置项` 失败。

`ctx` 字段：

| 字段 | 类型 | 含义 |
|---|---|---|
| `taskId` | string | 任务 UUID。 |
| `url` | string | 任务的原始 URL（绝不会是之前解析出来的链接）。 |
| `authRef` | string | 「插件 + 站点」的默认凭据引用。只有清单声明了权限 `auth` **且** URL 能推出 http/https 站点时才有值，否则为 `""`。通常直接调用 `flux.fetch` 即可，宿主会按该引用自动查找凭据。 |
| `cookies` | string | 任务附带的 Cookie 头值，没有则为 `""`。 |
| `referrer` | string | 任务附带的 Referrer，没有则为 `""`。 |
| `userAgent` | string | 生效的 User-Agent。 |
| `extraHeaders` | object | 额外请求头，字符串键值（没有则为 `{}`）。 |
| `resolverItem` | string | 普通调用时为 `""`。当它是被选中的[清单](#清单结果manifest)子项时：为插件自己的 `item.id`；用户明确选了该条目的某个规格时为 `<itemId>@<variantId>`。 |

`cookies`、`referrer`、`userAgent`、`extraHeaders` 是创建任务时存进任务的内容（例如浏览器扩展捕获、新建下载表单）。没有提供时为空字符串 / 空对象。

#### 返回值

返回 `null` 或 `undefined` 表示放行：FluxDown 原样下载 `ctx.url`。否则返回一个对象：

| 字段 | 类型 | 含义 |
|---|---|---|
| `url` | string | 改写后的直链。除非 `variants` 非空，否则**必须是合法链接**（见校验表）。空字符串**不**表示「保留原 URL」：既无 `variants` 又无 `manifest` 时会被拒绝（`url scheme 不允许: `）。想保留 URL 只改别的（比如 `fileName`），请返回 `url: ctx.url`。 |
| `audioUrl` | string | 独立的音频流链接，用于 DASH 风格的音视频分离。 |
| `fileName` | string | 覆盖保存的文件名；为空时忽略。 |
| `totalBytes` | integer | 文件大小（字节）。**仅当 `ephemeral` 为 `true` 时**才被采用，作为跳过探测后的大小提示（未知或 `0` 视为「未知」）。其他情况下被忽略：FluxDown 会自己探测真实大小。被选中变体的 `totalBytes` 会先覆盖这个字段。 |
| `extraHeaders` | object | 下载解析后链接时要发送的请求头，会替换任务原本捕获的请求头。 |
| `request` | object | 应用于任务本次运行的 [`RequestPatch`](/docs/zh/plugins/interceptors-guide/#requestpatch)，规则与 `prepare` 阶段相同（同主机 `url`、请求头、Cookie、User-Agent、`segments`，以及用于拒绝大 `Range` 区间的直链的 `maxRangeBytes` 等）。非法补丁会让任务失败，错误为 `[插件] <identity>: <消息>`。每次运行都会应用，不会存库。同时返回 `extraHeaders` 与 `request` 时，先由 `extraHeaders` 替换请求头表，再叠加 `request`。 |
| `ephemeral` | boolean | `true` 表示链接是一次性 / 防盗链的：跳过元数据探测（代价是续传完整性校验变弱）。默认 `false`：正常探测并保留基于 ETag 的续传校验。 |
| `rangeSupported` | boolean | `true` 表示你保证解析后的主机支持 HTTP Range 请求（例如 googlevideo）。与 `ephemeral` 同时使用时，FluxDown 仍然跳过探测，但会立即规划完整的多段下载，而不是保守的单流起步。默认 `false`：没有探测时，Range 能力由首个响应判断。 |
| `variants` | array | 多个画质 / 格式选项，见[变体](#变体variants)。 |
| `defaultVariantIndex` | integer | 默认变体（选择弹窗 60 秒超时、免打扰、无界面运行时使用）。越界回退到 `0`。默认 `0`。 |
| `manifest` | object | 多文件结果，见[清单结果](#清单结果manifest)，包括其可选根字段 `archiveMetadata`。与 `url`、`variants`、`audioUrl` 互斥。 |

普通 resolve 结果也可在顶层返回 `archiveMetadata`。结果为 manifest 且 resolve 结果与 manifest 根都提供元数据时，以 manifest 的 `archiveMetadata` 为准。

解析之后，FluxDown 会重新检查*解析后*的 URL 来选择协议引擎——resolver 可以返回 HLS 播放列表、磁力链接或 FTP URL，由对应的引擎接手。

数值字段（`totalBytes`、`size`、`bandwidth`、`width`、`height`、`defaultVariantIndex`）必须是整数；带小数会让整个返回值无效（`插件输出非法: resolve 返回值非法: …`）。

#### 输出校验

宿主会校验结果，任何一条违规都会整体拒绝（fail-closed）：

| 字段 | 规则 |
|---|---|
| `url`、`audioUrl`、变体的 `url` / `audioUrl` | scheme 只能是 `http`、`https`、`ftp`、`magnet`、`ed2k`（不区分大小写），长度不超过 8 KB。违规信息：`url scheme 不允许: <scheme>`、`url 超过 8KB`。空 `audioUrl` 允许（被忽略）。 |
| 顶层 `url` | 非空时校验；`variants` 为空时也校验。只有 `variants` 有条目时才可以为 `""`。 |
| `fileName`（也包括变体的 `fileName`、清单条目的 `name`） | 不能含 `/`、`\`、连续的 `..`（任意位置，所以 `a..b.mp4` 也会被拒）或控制字符。违规信息：`file_name 非法: …`。 |
| `variants` | 至多 50 项（`variants 过多: N > 50`）。每个 `label` 非空且不超过 200 个字符；每个 `url` 合法。 |
| `manifest` | 见[清单结果](#清单结果manifest)。 |

出错行为是 fail-closed：异常、超时、非法返回值、插件已卸载 / 被禁用，都会让任务进入错误状态，错误文本为 `[插件] <identity>: <message>`。绝不会悄悄下载原始 URL。（用户可以在应用里选择「忽略插件重试」来下载原始 URL。）

#### 变体（variants）

`variants` 多于一项时，FluxDown 会弹出选择对话框，用户的选择在下载前收敛为一条链接。在无界面服务器或免打扰下载中，会静默使用 `defaultVariantIndex`，与 HLS 画质选择一致。如果用户关闭了选择框，任务会被暂停，并以 `reason: "variant_selection_cancelled"` 触发 `onCancel`。

每项形如 `{ label, url, audioUrl?, fileName?, totalBytes?, bandwidth?, width?, height?, container? }`。

| 字段 | 含义 |
|---|---|
| `label` | 必填，在选择框中显示。 |
| `url` | 必填：该变体的主链接。 |
| `audioUrl`、`fileName`、`totalBytes` | 可选。存在且非空时，对被选中的变体覆盖顶层同名值。 |
| `bandwidth`、`width`、`height` | 整数，未知为 `0`。仅用于展示 / 排序。 |
| `container` | 容器格式，如 `"mp4"`，可为空。 |

清单条目处于第二段解析（`ctx.resolverItem` 非空）时，变体会静默收敛为默认项——不会为每个子任务弹一次选择框。

#### 清单结果（manifest）

清单在一次结果里描述多个文件（分享链接、网盘文件夹）。形状：

```text
{ name?, items: [{ id, name, path?, size?, kind?, variants?: [{ id, label, size? }] }] }
```
`manifest.archiveMetadata` 是可选的根级来源元数据。resolve 结果与 manifest 根同时提供元数据时，以 manifest 值为准。它仅是来源元数据，不会提取文件内容。

| 字段 | 规则 |
|---|---|
| `items` | 1 到 1000 项（`manifest items 数量非法`）。 |
| `name` | 组 / 文件夹名。为空时由宿主用母任务文件名兜底。 |
| `items[].id` | 插件自定义的选择标识，非空，不超过 200 个字符，会以 `ctx.resolverItem` 回传。 |
| `items[].name` | 文件名，非空，规则同 `fileName`。 |
| `items[].path` | 相对子目录（`""` 表示组根目录）。必须是安全的相对路径（不能是绝对路径、不能含 `..`），最多 **8** 级，且 `path + "/" + name` 不超过 **180** 个字符。 |
| `items[].size` | 已知大小（字节）；缺省或为负数表示未知。 |
| `items[].kind` | `""` 或 `"file"`，其他值一律拒绝。 |
| `items[].variants` | 可选的单文件规格，至多 50 项，每项的 `id` 和 `label` 都非空。 |

规则：

- `manifest` 与 `url`、`variants`、`audioUrl` **互斥**：同时返回会以 `manifest 与 url/variants/audioUrl 互斥` 失败。
- 只有第一段解析（`ctx.resolverItem === ""`）可以返回清单。第二段返回清单会以 `二段解析（resolverItem 非空）不可返回 manifest：防递归裂变` 失败。用户（或引擎的自动展开）选中子项后，宿主会带着 `ctx.resolverItem` 再次调用 `resolve`，此时插件应返回普通直链结果。
- 任何一个条目违规，整份清单都会被拒绝。
- 宿主不会从名字推断文件之间的关系或画质。

##### 交互式多文件清单

GPUI 新建下载窗口在创建任务前，会对单个本地 GET 链接做预解析（仅限声明了 `multi: true` 的 resolver）。非空清单会在同一窗口中打开文件选择；返回表单或取消预览都不会创建任务。确认后用所选文件创建任务组。创建任务组出错时，选择器保持打开以便重试。

每个清单条目默认全部选中。既有插件返回的扁平清单保持扁平，沿用原来的 ID。只有提供了 `item.variants` 才会出现逐文件画质 / 格式选择：保持选中**插件默认**时传递 `item.id`；明确选择时在 `ctx.resolverItem` 中传递 `<itemId>@<variantId>`。插件必须把该标识解析为所选资源。这些逐文件的元数据变体，与上面顶层直链 `variants` 是两回事。

浏览器捕获的预览会把私有请求上下文保留在 agent 中，只有清单元数据会到达界面。只有任务组创建成功后，捕获事务才会被消费。多链接提交、远程派发、非 GET / 带请求体 / 带音频的请求以及无人值守下载，仍沿用原有路径。

没有声明 `multi` 却返回清单的 resolver，引擎仍会在任务开始时自动展开，所以 `multi` 只是交互预览的优化，不是硬性要求。

### 钩子：`onStart`、`onDone`、`onError`、`onCancel`、`onMetaProbed`

通知型钩子，在 `hooks.events` 中声明，在 `hooks.entry` 文件中实现。完整教程见[事件钩子指南](/docs/zh/plugins/hooks-guide/)。所有钩子都会收到 `{ event, taskId, url }` 以及各事件专属字段；`event` 的取值分别是 `"start"`、`"error"`、`"done"`、`"metaProbed"`、`"cancel"`。

| 钩子 | `event` | 额外字段 | 触发时机 |
|---|---|---|---|
| `onStart` | `start` | — | 任务首次开始时。已暂停的任务之后恢复不会再次触发。 |
| `onMetaProbed` | `metaProbed` | `fileName`、`totalBytes`——探测结果 | 元数据探测之后，仅当探测得到了文件名或大小。 |
| `onDone` | `done` | `filePath`——完成文件的绝对路径；`audioPath`——轨对任务（视频 + 音频分离的流）合并失败时，独立音频文件的绝对路径（`<stem>.audio.m4a`），单文件结果（包括合并成功）为 `null`；`muxed`——轨对任务是否已合并为单个文件，非轨对任务恒为 `false` | 任务成功完成时。FluxDown 采用了已存在的文件而没有下载时**不**触发。 |
| `onError` | `error` | `message`——任务的错误文本 | 任务每次进入失败状态时——包括 FluxDown 之后会自动重试的失败。 |
| `onCancel` | `cancel` | `reason`——见下 | 用户关闭「选择插件」对话框或变体选择框时：任务被暂停，而不是删除。同一任务上与 `onDone` / `onError` 互斥。用于释放临时远程资源，不能用来重试。删除任务或停止 daemon **不会**触发它。 |

`onCancel` 的 `reason` 一定存在，取值为 `"user_cancelled"`（关闭了「选择插件」对话框）或 `"variant_selection_cancelled"`（关闭了变体选择框）。

关于 `url`：

- `onStart`：任务经 resolver 处理过时，`url` 是**解析后**的直链（实际正在下载的链接）；没有 resolver 的任务则是任务 URL。
- `onDone`、`onError`、`onCancel`、`onMetaProbed`：是任务保存的 URL（resolver 任务即原始链接）。
- `hooks.match.urls` 过滤用的就是这个 `url`。没有 `match` 时，钩子会收到所有任务。

投递与失败行为：

- 钩子是即发即忘：异常和超时只记录日志（`hook onDone 执行失败（已忽略）: …`）并吞掉，钩子里做的任何事都不会改变任务——下面两个例外除外。
- 钩子池饱和时，通知会被**丢弃**并记一条警告（`钩子并发已满，本次钩子被丢弃`）。拥有 `ffmpeg` / `ytdlp` 权限的钩子走独立的池，并有有界队列，见[并发与过载](#并发与过载)。
- 钩子的失败从不计入熔断。

两个例外：[`flux.task.requestRetry`](#fluxtaskrequestretryopts)（在 `onError` 中）和 [`flux.task.recordArtifact`](#fluxtaskrecordartifactname)（在 `onDone` 中）。

`onDone` 是唯一有产物文件的钩子：`flux.ffmpeg` / `flux.ffprobe` 可以在其中运行，被限制在完成文件所在的文件夹里。

### `intercept(ctx)`

任务拦截器入口，仅当清单声明了 `interceptors.entry`（且 `permissions` 含 `"intercept"`）时才会被调用。同一个全局函数处理所有已订阅的阶段，按 `ctx.stage` 分派（`prepare`、`probed`、`refresh`、`selectVariant`、`selectPeriod`、`segments`、`liveSegments`、`finalize`、`failed`）。返回 `null` / `undefined` 表示不做改动。上下文、返回值与 `RequestPatch` 规则见[拦截器指南](/docs/zh/plugins/interceptors-guide/)；TypeScript 中使用 `fluxdown-plugin-sdk/runtime` 的 `defineInterceptors`。调用与 resolve 共用并发池和熔断；声明了 `ffmpeg` 或 `ytdlp` 权限的插件在 `finalize` 阶段的调用使用更长的后处理预算。

### `authenticate(ctx)`

平台登录入口，仅当清单声明了 `auth.entry`（且 `permissions` 含 `"auth"`）时才会被调用。宿主通过 `daemon.plugin.auth` RPC 带上 `action` 驱动它：用户在设置里点「登录」时为 `begin`，之后按需轮询为 `poll`，用户取消时为 `cancel`，用户退出登录时为 `logout`；`status` 让宿主在无需用户交互的情况下探测当前登录状态。完整示例见[账号登录指南](/docs/zh/plugins/auth-guide/)。

`ctx` 字段：

| 字段 | 类型 | 含义 |
|---|---|---|
| `action` | string | `begin` / `poll` / `cancel` / `logout` / `status` 之一。其他值会在脚本运行前被宿主拒绝。 |
| `site` | string | 用户输入的站点（没有提供则为空字符串）。 |
| `authRef` | string | 已知的认证引用。宿主收到空的 `authRef` 同时带有 `site` 时，会替你填成 `pluginId::<规范化后的站点>`；`poll` / `cancel` / `logout` 通常会带回 `begin` / `status` 返回的值。 |
| `sessionId` | string | 插件自定义的会话标识，从 `begin` 一路传到 `poll`。 |
| `input` | string | 用户在交互表单里输入的值（例如验证码），格式由插件决定。 |

返回一个对象（不是 JSON 字符串）：

| 字段 | 类型 | 含义 |
|---|---|---|
| `status` | string | 必填，`pending` / `success` / `error` 之一（其他值：`authenticate.status 必须是 pending/success/error`）。 |
| `sessionId` | string | 回显或延续会话标识。 |
| `challenge` | string | 可选；二维码内容、data URL 或其他要展示给用户的挑战数据。 |
| `challengeType` | string | 可选；`challenge` 的类型（例如 `"qrcode"`），由插件与宿主界面约定。 |
| `message` | string | 可选；要展示给用户的状态文字。 |
| `authRef` | string | 可选；成功时应指向你刚保存的凭据。`success` 时省略，宿主会回退为请求里的 `authRef`。 |

`status` 为 `pending` 时，`poll` 的回复如果省略 `challenge` / `challengeType` 或返回 `null`，会保留上一次的挑战。终态回复会清除省略的挑战字段；显式传空的挑战会清空其内容。
对 `challengeType: "qrcode"`，GPUI 与 Web 会把普通文本（包括登录 URL）渲染为本地生成的、不透明白底的二维码图片，绝不会把 URL 当图片去请求。受支持的 base64 图片 data URL 则直接使用其自带图片。二维码编码失败时，界面退回文本并保留复制按钮以复制完整原始值。`pending` / `success` 的消息只是提示，只有 `error` 消息使用错误样式。

登录成功后，插件调用 [`flux.auth.save`](#fluxauth) 持久化凭据；`authenticate` 本身不负责持久化。

值得了解的宿主行为：

- **`logout` 由宿主兜底完成。** 请求带有 `authRef` 时，脚本返回后宿主会删除该凭据，即使脚本没有调用 `flux.auth.remove`（甚至抛了异常）。
- **被禁用的插件仍可收到 `logout`。** 这种情况下宿主根本不运行你的脚本，直接删除凭据。被禁用插件的其他 action 会以 `插件 <id> 未启用` 失败。
- 必填设置会在脚本运行前检查（`缺少必填设置项`）。
- `authenticate` 有独立的时间预算和并发池，**从不**计入熔断。这里**没有** `flux.ytdlp`。

### `subscribe(ctx)`

订阅源 provider 入口。当 `providerId` 与 `subscriptions[0].providerId` 相同的订阅源被刷新时调用；函数名固定为 `subscribe`，脚本文件是 `subscriptions[0].entry`。教程见[订阅源指南](/docs/zh/plugins/subscription-guide/)。

`ctx` 字段：

| 字段 | 类型 | 含义 |
|---|---|---|
| `providerId` | string | 你的 provider ID。 |
| `sourceId` | string | 订阅记录的 ID。 |
| `url` | string | 订阅记录中保存的地址。 |
| `providerConfig` | string | provider 专属的不透明配置，**是一个 JSON 字符串**（为空时是 `"{}"`），请自行 `JSON.parse`。 |
| `cookies` | string | 订阅级 Cookie 头（可为空）。 |
| `userAgent` | string | 已解析的 User-Agent。 |

返回一个对象：

```text
{ title, link, items: [{ guid, title, link, enclosureUrl, resolverItem, enclosureLength, pubDate }] }
```

| 字段 | 规则 |
|---|---|
| `items` | 至多 1000 项（`订阅条目数量超过 1000`）。 |
| `items[].guid` | 必填，非空，不超过 2048 字节。用于去重的稳定标识。 |
| `items[].title` | 不超过 1024 字节。 |
| `items[].link`、`items[].enclosureUrl` | 至少一个非空。每个非空值都必须通过与 `resolve` 相同的 URL 规则（scheme 为 `http` / `https` / `ftp` / `magnet` / `ed2k`，不超过 8 KB）。 |
| `items[].resolverItem` | 可选，不超过 2048 字节。设置后，条目的 `link` 会连同这个二段解析标识交给你的 resolver，因此此时 `link` **必填**。 |
| `items[].enclosureLength` | 整数 ≥ 0（字节），未知为 `0`。 |
| `items[].pubDate` | 整数，Unix 时间，含义由你的 provider 定义（未知为 `0`）。 |

非法条目会被**跳过**并记日志，不会致命，所以上游一条脏数据不会拖垮整个订阅源。只有列表非空且**每一条**都非法时，调用才会失败：`订阅条目全部非法（N 条），首条原因: …`。返回 `null`、非对象或畸形 JSON 会以 `订阅返回值非法` 失败。

时间预算默认等于 resolve 预算（10 秒），可通过 `subscriptions[0].timeoutMs` 设置（上限 30 秒）。`subscribe` 与 resolve 共用并发池，并计入熔断。可用 `flux.ytdlp`，声明了权限 `auth` 时还可用 `flux.auth`。

### `main` 常驻脚本

`main` 与上面所有入口都不同：它是**常驻**脚本，宿主启动时只执行**一次**，并通过*注册*处理器来工作，而不是导出入口函数。注册信息保留在 JavaScript 全局状态里，之后的每次调用（渲染视图、执行命令、处理事件或定时器）都会逐个派发到同一个运行时中。

```js
flux.ui.view("dashboard", {
  render: (state, ctx) => flux.ui.h("text", { text: "Hello" }),
});
flux.commands.register("refresh", async ({ taskIds }) => { /* … */ });
flux.rpc.handle("ping", async (args) => ({ pong: args }));
flux.events.on("onDone", async (ev) => flux.ui.toast("success", ev.url));
flux.timers.setInterval(() => flux.logger.info("tick"), 60000);
```

这些注册接口（`flux.ui`、`flux.commands`、`flux.rpc`、`flux.events`、`flux.timers`、`flux.tasks`、`flux.settings.update`）在[仅 main 可用的接口](#仅-main-可用的接口)中概述；节点词汇和事件载荷见[视图](/docs/zh/plugins/views/)，教程见[可移植界面指南](/docs/zh/plugins/ui-guide/)。`main` 有自己的限制（见[运行时限制](#运行时限制)）：超时或内存溢出会丢弃整个运行时及其所有状态。

## 各入口里有哪些 `flux` 接口

注入的 `flux` 对象在所有入口中都一样，除非下表另有说明。`archive` 入口是例外：它运行在最小沙箱里，只有 `flux.settings`、`flux.logger` 和 `console`；下表其余各行在那里都不存在（`flux.info`、`flux.storage`、`flux.fetch`、`flux.fs`、`flux.task`、`flux.ffmpeg`、`flux.ffprobe`、`flux.ytdlp`、`flux.auth`、`flux.ui` / `rpc` 及其余接口，以及任何文件、网络、任务或进程接口）。「权限」指清单的 `permissions` 里必须声明对应项；没声明时该属性是 `undefined`（用 `if (flux.ytdlp)` 判断）。

| 接口 | `resolve` | 钩子（`onDone` 以外） | `onDone` | `authenticate` | `subscribe` | `main` | `archive` |
|---|---|---|---|---|---|---|---|
| `flux.fetch` | 有 | 有 | 有 | 有 | 有 | 有 | **`undefined`** |
| `flux.storage`、`flux.fs` | 有 | 有 | 有 | 有 | 有 | 有 | **`undefined`** |
| `flux.settings`、`flux.logger`、`console` | 有 | 有 | 有 | 有 | 有 | 有 | 有 |
| `flux.info` | 有 | 有 | 有 | 有 | 有 | 有 | **`undefined`** |
| `flux.auth` | 权限 `auth` | 权限 `auth` | 权限 `auth` | 始终有（入口本身要求 `auth`） | 权限 `auth` | 权限 `auth` | **`undefined`** |
| `flux.task.requestRetry` | 无效果（警告） | 仅 `onError` | 无效果 | 无效果 | 无效果 | 无效果 | **`undefined`** |
| `flux.task.recordArtifact` | reject | reject | 可用 | reject | reject | reject | **`undefined`** |
| `flux.ffmpeg`、`flux.ffprobe` | `undefined` | 权限 `ffmpeg`：对象存在，`run` 会 reject，`ffmpeg.available()` 可用 | 权限 `ffmpeg`，`run` 可用 | `undefined` | `undefined` | 权限 `ffmpeg`：对象存在，`run` 会 reject | `undefined` |
| `flux.ytdlp` | 权限 `ytdlp` | 权限 `ytdlp` | 权限 `ytdlp` | **`undefined`** | 权限 `ytdlp` | 权限 `ytdlp` | **`undefined`** |
| `flux.ui`、`commands`、`rpc`、`events`、`timers`、`settings.update` | — | — | — | — | — | 有 | **`undefined`** |
| `flux.tasks` | — | — | — | — | — | 权限 `tasks` | **`undefined`** |

`flux.auth.*` 和 `flux.fetch` 的 `authRef` 选项都需要在 `permissions` 里声明 `auth`；没有该权限却显式传入 `authRef`，`flux.fetch` 会 reject（见[错误](#你会遇到的错误)）。

## `flux` 对象

### `flux.fetch(opts)` → `Promise<response>`

HTTP 客户端。`opts`：

| 字段 | 默认值 | 说明 |
|---|---|---|
| `method` | `"GET"` | 任何合法的 HTTP 方法。 |
| `url` | — | 必填。 |
| `headers` | `{}` | 字符串键值。名称或值不是合法 HTTP 头的会被静默丢弃。 |
| `body` | 无 | 请求体，仅限文本。 |
| `authRef` | 无 | 显式的认证引用（见 [`flux.auth`](#fluxauth)）。省略或为空，且插件声明了 `permissions: ["auth"]` 时，宿主会按「插件 ID + 请求站点」派生默认引用并自动查找。没有 `auth` 权限的插件永远不会得到任何凭据注入，也不能传 `authRef`。 |

`opts` 会被序列化为 JSON。如果它无法按上面的形状读取（例如请求头的值是数字），会被当作空对象处理，调用以 `URL 非法` 错误失败。

返回 `{ status, headers, body, truncated }`：

- `status` 是数字 HTTP 状态码。非 2xx 状态也会**正常 resolve**；只有网络和守卫失败才会 reject。
- `headers` 是对象，名称**全部小写**（`set-cookie`、`content-type` 等）。**重复的响应头**（最常见的是多个 `Set-Cookie`）会用换行符（`\n`）拼接成一个字符串，而不是只保留最后一个——从登录响应里解析 Cookie 时请按 `\n` 切分。值不是合法文本的响应头会被省略。
- `body` 是文本：字节按 UTF-8 解码，非法序列替换为替换字符（不支持二进制响应）。
- `truncated` 为 `true` 表示响应体触达了大小上限。

全部由宿主侧强制执行的防护：

| 规则 | 值 |
|---|---|
| 协议 | 仅 `http` / `https` |
| 目标地址 | 仅限公网可路由地址。环回、局域网、链路本地、CGNAT、文档 / 基准测试地址段和云元数据 IP 都被拦截——对字面量 URL、DNS 解析结果（因此能防 DNS rebinding）以及每一跳重定向都会检查。用户在应用里配置了代理时，DNS 由代理解析，只有字面量 IP 与重定向检查生效。例外：DNS 应答全部落在 198.18.0.0/15（Clash / sing-box 等 TUN 代理的 fake-ip）时按 TUN 隧道放行，由代理软件解析真实目标，此时与配置代理相同，只有字面量 IP 与重定向检查生效。 |
| 开发模式例外 | 通过「从目录安装（开发模式）」加载的插件不受上述公网限制，可访问 `localhost`、局域网等地址，便于连接本机服务联调。它与其他插件一样遵循代理设置与站点规则，只是环回、局域网私网段与本地域名（`.local`、`.lan`、`.home.arpa`、`.internal`）恒直连；正式安装的插件行为不变。 |
| 响应体上限 | 8 MB，超出截断 |
| 单请求超时 | 10 秒 |
| 并发请求 | 8，所有插件共享 |
| 最大重定向 | 30 |
| 代理 | 跟随应用当前的代理设置（修改后下一个请求即生效）。请求 URL 命中站点代理规则时走该规则；其余请求走本次调用的出口：任务自带代理（此时不看站点规则），否则为任务链接命中的规则，再否则为全局模式。**Auto** 模式下调用先走直连（或该站点近期走得通的代理）；若因请求无法连通网络而失败，宿主会经每个候选代理各重跑一次整次调用，随后的下载沿用解析成功时的同一出口。 |

凭据与重定向：真正带上了已存凭据（显式或隐式 `authRef`）的请求，会走更严格的客户端，它**不会跟随到另一个 origin 的重定向**（scheme、主机或端口任一不同）。你会拿到 `3xx` 响应本身（`status`、`headers.location`），由你自己决定是否跟随，从而保证凭据不会跨源泄漏。不带凭据的请求则正常跟随重定向。

### `flux.auth`

**仅当**清单声明了 `permissions: ["auth"]` 时才可用——否则 `flux.auth` 是 `undefined`。它管理由宿主持久化的插件凭据（Cookie / Bearer 令牌 / HTTP Basic / 自定义请求头），供 `flux.fetch` 自动复用。凭据不存放在插件自己的 `flux.storage` 里——由宿主统一实现持久化和过期，插件不必各自重复造轮子。

- `flux.auth.save(profile)` → `Promise<string>`——写入或替换一份凭据，返回规范化后的 `authRef`。`profile.site` 必填，可以是完整 URL 或裸 `host[:port]`（裸主机默认规范化为 `https`——要注册明确允许明文请求的站点，需要自己传 `"http://host"`）。省略 `authRef` 时会生成为 `pluginId::规范化站点`；显式传入时必须与该规范化结果一致，否则调用 reject。序列化后的档案不得超过 **64 KB**。
- `flux.auth.get(authRef)` → `Promise<profile | null>`——读回一份凭据；`authRef` 不属于当前插件时 reject。
- `flux.auth.remove(authRef)` → `Promise<true>`——删除凭据；`authRef` 必须属于当前插件（以 `pluginId::` 为前缀）。删除不存在的凭据不算错误。

`profile` 字段（`save` / `get` 共用）：

| 字段 | 说明 |
|---|---|
| `site` | 站点键，如上所述。**带协议**——同一主机的 `http` 与 `https` 是两个不同的站点；通过 `https` 建立的凭据绝不会自动用于 `http` 请求（避免被动中间人在明文链路上窃取凭据）。 |
| `kind` | `basic` / `cookie` / `bearer` / `headers` / `session`——仅作描述，无论取值如何，宿主都按下面的规则注入。 |
| `cookies` | 作为 `Cookie` 请求头注入（请求已有该头则跳过）。 |
| `accessToken` | 作为 `Authorization: Bearer <accessToken>` 注入（已有 `Authorization` 则跳过）。 |
| `username` / `password` | `kind === "basic"` 时作为 `Authorization: Basic <base64>` 注入（已有 `Authorization` 则跳过）。 |
| `headers` | 额外请求头，逐个写入（优先级最高——会覆盖上面字段生成的同名请求头）。 |
| `account` / `refreshToken` / `expiresAt` / `refreshAt` / `metadata` | 平台相关的元数据；宿主原样保存、不解读，唯一例外是 `expiresAt`（Unix 秒）——超过该时间凭据即过期：隐式引用（没有显式 `authRef` 的 `flux.fetch`）会静默跳过注入，显式 `authRef` 则以 `authentication_required` reject。 |

`flux.fetch` 的 `authRef` 解析：显式传入时，凭据缺失或过期会 reject，错误信息**包含** `authentication_required`（宿主会在前面加 `插件运行时错误: ` 前缀，所以请用 `String(e.message).includes('authentication_required')` 判断，不要用 `startsWith`）；省略（或为空）时，宿主按插件 + 站点派生默认引用，未命中或过期则静默跳过注入——请求照常发出，只是不带凭据。凭据保存的站点与请求站点不一致、或属于其他插件时同样 reject。

### `flux.storage`

持久化键值存储，归你的插件私有，应用重启后仍然保留（由 FluxDown 数据库支撑）。

- `flux.storage.get(key)` → `Promise<string | null>`——键未设置时为 `null`。
- `flux.storage.set(key, value)` → `Promise<void>`——单个值超过 **64 KB**，或插件键数将超过 **100** 时 reject（覆盖已有键不算新键）。

键和值都会经 `String(…)` 转换；结构化数据请自行 JSON 编码。没有删除接口；需要清除时用空字符串覆盖。

### `flux.fs`

始终可用——不需要在 `permissions` 里声明。这是插件自己工作区里的临时文件存储，与 `flux.storage` 的键值存储不同：当受管工具需要磁盘上的真实文件、而不是内存里的字符串时使用。该工作区与 `flux.ytdlp` 运行时的目录（它的 cwd）是**同一个目录**——写在这里的文件，yt-dlp（或任何工具调用）都可以用普通相对文件名访问。它不是已下载文件所在的文件夹；那个文件夹请看 `onDone` 里的 `flux.ffmpeg`。

- `flux.fs.writeFile(name, content)` → `Promise<void>`——写入（或覆盖）文本文件；文件名非法或超出限制时 reject。
- `flux.fs.readFile(name)` → `Promise<string | null>`——读回文本文件；文件不存在**或文件名非法**时为 `null`。
- `flux.fs.remove(name)` → `Promise<void>`——删除文件；文件不存在不算错误（幂等）；文件名非法则 reject。
- `flux.fs.list()` → `Promise<string[]>`——工作区顶层文件名（不含子目录和 yt-dlp 自己的 `.cache`）。

`name` 必须是扁平且安全的文件名：非空，不含 `/`、`\`、`:` 或 NUL，不是 `.` 或 `..`，且不超过 255 字节——否则 reject。限制：单文件 **8 MB**，每个插件工作区总共 **64 MB**，最多 **100 个文件**（覆盖已有文件不算新文件）。内容只能是文本（不支持二进制）。在 Unix 上文件以 `0600` 权限创建。卸载插件时工作区会被删除。

典型用法：为受管工具物化输入文件——Cookie、配置、字幕——写入，在工具调用里用相对名引用，然后删除。示例——把 Cookie 文件喂给 `flux.ytdlp`：

```js
await flux.fs.writeFile('cookies.txt', netscapeCookieText);
try {
  const r = await flux.ytdlp.run({ args: ['--cookies', 'cookies.txt', '-J', ctx.url] });
  // ... 使用 r.stdout
} finally {
  await flux.fs.remove('cookies.txt');
}
```

### `flux.settings`

一个普通对象，包含清单中声明的设置，已按 `settings[].type` 转成对应类型：

| 类型 | JS 值 |
|---|---|
| `string` | 字符串；用户没设置**且**没有 `default` 时为 `null` |
| `number` | 数字；未设置且没有 `default`（或保存的文本不是数字）时为 `null` |
| `boolean` | 仅当值恰好是 `"true"` 时为 `true`，否则为 `false`（未设置的布尔值是 `false`） |

未设置但有 `default` 的字段带着默认值。对 `string` 和 `number` 请始终防范 `null`。标记为 `required` 且既无用户值也无 `default` 的字段，会让 `resolve`、`authenticate`、`subscribe` 在你的脚本运行**之前**就失败（`缺少必填设置项: 插件 <id> 需先配置「<title>」`），所以在这些入口里必填设置不会是 `null`。

一次性入口里该对象是调用时生成的快照。`main` 里它是一个活对象，用户修改设置时宿主会就地更新它，并且存在 `flux.settings.update`（见[仅 main 可用的接口](#仅-main-可用的接口)）。

### `flux.info`

`{ identity, version, appVersion }`——你的插件 ID 和版本，以及宿主 FluxDown 的版本。

### `flux.logger` 与 `console`

`flux.logger.info/warn/error(...)` 写入 FluxDown 的日志文件，带 `[plugin:<id>]` 前缀。`console.log/info/warn/error/debug` 映射到同一处（`log`、`info`、`debug` 按 info 级别记录）。多个参数以空格连接；非字符串会被 JSON 序列化。每行截断到 4 KB。注意 **`warn` 与 `info` 写入同一级别**，只有 `error` 才记为错误。调用立即返回 `undefined`。日志文件位置和搜索方法见[调试与排错](/docs/zh/plugins/debugging/)。

### `flux.task.requestRetry(opts)`

`flux.task.requestRetry({ delayMs: 5000 })`——请求 FluxDown 在 `delayMs` 毫秒后重试失败的任务（非负整数，其他值按 `0` 处理）。调用是同步的，没有返回值。只有在 `onError` 里才有意义；在其他地方调用会记录 `flux.task.requestRetry 仅 onError 钩子可用，已忽略`，什么都不做。重试与任务的自动重试额度（`max_auto_retries`）共用，因此插件不可能无限重试。由于 `onError` 在 FluxDown 本来就会自动重试的失败上也会触发，请避免为应用自己会重试的错误再请求重试。

### `flux.task.recordArtifact(name)`

`await flux.task.recordArtifact('video.srt')`——把你的 `onDone` 钩子在下载文件旁边创建的文件登记为该任务的**衍生产物**：用户删除任务**并删除文件**时，该产物会一并删除。仅限 `onDone`；其他位置调用会以 `flux.task.recordArtifact 仅 onDone 钩子可用` reject。

`name` 必须是任务保存目录下的裸文件名：

- 非空，不超过 512 字节，不含 `/`、`\`、`:`，也不是 `.` 或 `..`；
- 以任务主文件的主干（不含扩展名的部分）开头——无关文件无法被登记；
- 该文件必须已存在于保存目录。

违规会以 `recordArtifact: …` reject。请先写出文件（通常用 `flux.ffmpeg.run`），再登记。

### `flux.ffmpeg`

**仅当**清单声明了 `permissions: ["ffmpeg"]` 时才可用——否则 `flux.ffmpeg` 是 `undefined`，请用 `if (flux.ffmpeg)` 判断。它运行 FluxDown 解析出的 ffmpeg（用户设置的路径 → 托管安装 → 系统 `PATH`），所以 ffmpeg 还必须真实存在（可在应用的 设置 → 扩展 → 组件 页安装）；否则 `run` 会以 `ffmpeg 未安装或不可用` reject。

- `flux.ffmpeg.available()` → `Promise<{ available, version, source }>`——探测当前生效的 ffmpeg。`source` 为 `"manual"` / `"managed"` / `"system"` / `"none"`。
- `flux.ffmpeg.run(spec)` → `Promise<outcome>`——运行 ffmpeg。`spec`：

| 字段 | 默认值 | 说明 |
|---|---|---|
| `args` | — | 必填，非空，至多 512 项，每项不超过 8 KB 且不含 NUL。ffmpeg 参数数组（不含程序名；`-nostdin` 会自动前置）。 |
| `subdir` | 无 | 牢笼根下的工作子目录；安全的相对路径，不得逃逸。不存在时会创建。 |
| `timeoutMs` | 300000 | 单次调用超时，上限 1800000（30 分钟）。 |

返回 `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }`——`code` 是退出码（被杀死 / 无退出码时为 `-1`），`stdout` / `stderr` 会被截断（256 KB / 64 KB），超时杀死时 `timedOut` 为 `true`（此时 `stdout` / `stderr` 为空）。非零退出码也会 **resolve**，请检查 `code`。

**牢笼。** `flux.ffmpeg.run` 只在 `onDone`（唯一有产物文件的钩子）里可用；其他任何上下文调用都会以 `flux.ffmpeg.run 仅在有产物文件的钩子（如 onDone）中可用` reject（`available()` 仍可用）。工作目录就是完成文件所在的文件夹，这个文件夹就是牢笼——请用**相对**文件名（basename）引用文件，并加上 `./` 前缀，以防文件名以 `-` 开头。

参数会被筛查；任何一个 token 命中以下情况，都会拒绝启动（`ffmpeg 参数 '<arg>' 被拒: <reason>`）：

| 被拦截 | 示例 |
|---|---|
| URL scheme / 协议 | `http://…`、`file:…`、`concat:…`、`crypto:…` |
| 绝对路径 / 盘符 | `/etc/x`、`C:\x`、`\\host\share` |
| 父目录穿越 | `../x`、`a/../b` |
| 内嵌的绝对路径 | `subtitles=/etc/x` |
| 滤镜 / 多输出表达式中以 `/` 或盘符开头的片段 | `a.mkv\|/abs`、`[v]/abs` |
| `tee` 复用器 | `tee` |
| 从文件读取选项值或滤镜图、或覆盖协议安全设置的选项 | `-/…`、`-safe`、`-protocol_whitelist`、`-protocol_blacklist`、`-filter_script`、`-filter_complex_script` |

路径检查之前，token 里的引号和反斜杠会先被去掉，所以加引号也藏不住路径。普通的 ffmpeg 语法不受影响——除法（`30000/1001`）、流说明符（`0:a`、`-c:v`）、滤镜（`scale=1280:720`）都能通过。既没有可达的 URL 也没有可达的绝对路径，ffmpeg 就只能接触牢笼里的文件，因此也没有网络通路。所有插件合计最多同时运行 2 个 ffmpeg / ffprobe 进程，超时或取消时每个子进程都会被杀死。

示例——在 `onDone` 里把非 MP4 结果转成 MP4：

```js
globalThis.onDone = async (ctx) => {
  if (!flux.ffmpeg) return;
  const name = ctx.filePath.split(/[\\/]/).pop();
  if (/\.mp4$/i.test(name)) return;
  const out = name.replace(/\.[^.]+$/, '') + '.mp4';
  const r = await flux.ffmpeg.run({
    args: ['-i', './' + name, '-c:v', 'libx264', '-c:a', 'aac',
           '-movflags', '+faststart', '-y', './' + out],
  });
  if (r.code !== 0) {
    flux.logger.error('convert failed', (r.stderr || '').slice(-400));
    return;
  }
  await flux.task.recordArtifact(out); // 与任务文件一并删除
};
```

### `flux.ffprobe`

与 `flux.ffmpeg` 由同一个 `permissions: ["ffmpeg"]` 声明控制——没有单独的权限。它共用完全相同的牢笼（`run` **只**在 `onDone` 里可用，其他位置以 `flux.ffprobe.run 仅在有产物文件的钩子（如 onDone）中可用` reject）、相同的路径解析顺序（用户设置 → 托管安装 → 系统 `PATH`）和相同的参数筛查。ffprobe 随托管的 ffmpeg 一起安装——在 设置 → 扩展 → 组件 页安装 ffmpeg 时也会把 ffprobe 放进 `<data_dir>/bin`，无需单独安装。若缺失，`run` 会以 `ffprobe 未安装或不可用（随 ffmpeg 组件一并安装）` reject。

- `flux.ffprobe.run(spec)` → `Promise<outcome>`——运行 ffprobe。`spec` 与返回的 `outcome` 形状和 `flux.ffmpeg.run` 完全一致（`args` / `subdir` / `timeoutMs`，返回 `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }`）。与 ffmpeg 不同，不会前置 `-nostdin`。没有 `available()`。

用它做结构化探测，而不是去抓 ffmpeg 的 stderr：

```js
const out = await flux.ffprobe.run({
  args: ['-v', 'quiet', '-print_format', 'json', '-show_format', '-show_streams', './in.mp4'],
});
const info = JSON.parse(out.stdout);
```

### `flux.ytdlp`

**仅当**清单声明了 `permissions: ["ytdlp"]` 时才可用——否则 `flux.ytdlp` 是 `undefined`，请用 `if (flux.ytdlp)` 判断。它运行 FluxDown 解析出的 yt-dlp（用户设置的路径 → 托管安装 → 系统 `PATH`），所以 yt-dlp 还必须真实存在（可在应用的 设置 → 扩展 → 组件 页安装）；否则 `run` 会以 `yt-dlp 未安装或不可用` reject。

- `flux.ytdlp.available()` → `Promise<{ available, version, source }>`——探测当前生效的 yt-dlp。`source` 为 `"manual"` / `"managed"` / `"system"` / `"none"`。用 `run({ args: ['--version'] })` 并检查 `code === 0` 也是更轻量的存活探测。
- `flux.ytdlp.run(spec)` → `Promise<outcome>`——运行 yt-dlp。`spec`：

| 字段 | 默认值 | 说明 |
|---|---|---|
| `args` | — | 必填，非空，至多 512 项，每项不超过 8 KB 且不含 NUL。yt-dlp 参数数组（不含程序名）。 |
| `subdir` | 无 | 牢笼根下的工作子目录；安全的相对路径，不得逃逸。不存在时会创建。 |
| `timeoutMs` | 300000 | 单次调用超时，上限 3600000（60 分钟）。 |

返回 `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }`——`code` 是退出码（被杀死 / 无退出码时为 `-1`），`stdout` / `stderr` 会被截断（默认 16 MB / 256 KB），超时杀死时 `timedOut` 为 `true`（此时 `stdout` / `stderr` 为空）。把 `stdout` 当 JSON 解析之前请先检查 `truncatedStdout`，给出明确的错误，而不是让 `JSON.parse` 在被截断的尾部失败。

stdout 上限默认 16 MB，并被限制在 1–64 MB 之间。无人值守部署的管理员可以通过配置键 `plugin.<id>.ytdlp.stdout_cap`（单个插件，优先级最高）、`component.ytdlp.stdout_cap`（全局）或环境变量 `FLUXDOWN_YTDLP_STDOUT_CAP`（默认值）修改。取值是字节数，或带 `k` / `m` / `g` 后缀（例如 `32m`）。

**牢笼。** 与 `flux.ffmpeg` 不同，`flux.ytdlp` 不依赖产物文件，所以在所有暴露它的入口里都能用（见[矩阵](#各入口里有哪些-flux-接口)）——但 `authenticate` 里**没有**。牢笼不是任务的输出文件夹，而是桥接层为每个插件维护的一个临时目录（在 FluxDown 数据目录下惰性创建），跨调用复用。它是该次调用的工作目录，`subdir` 在其内再划出一个子文件夹。读写的文件都请用**相对**文件名引用。这与 `flux.fs` 读写的是同一个工作区——给 yt-dlp 喂 Cookie 文件、配置文件或字幕就靠它：先 `flux.fs.writeFile('cookies.txt', …)`，在 `args` 里用相对名引用，调用返回后再 `flux.fs.remove('cookies.txt')`。

#### 参数白名单

yt-dlp 是网络工具，所以允许 URL 参数和出站网络访问。参数按一份**精确白名单**检查：下面没有列出的一律拒绝，包括缩写的长选项（`--no-warn`）和与取值粘在一起的短选项（`-fbest`、`-Jq`）。每个短选项请单独成一个参数（`['-f', 'best']`）。长选项可以写成 `--name value` 或 `--name=value`。

- **位置参数**必须是 `http://` 或 `https://` URL。单独的 `--` 之后，剩下的参数都按位置参数处理。
- **开关**（不带值）——`-J` `-j` `-q` `-v` `-s` `-x` `-F` `-i` `-4` `-6` `-k` `--version` `--dump-json` `--dump-single-json` `--simulate` `--skip-download` `--quiet` `--verbose` `--no-warnings` `--no-progress` `--ignore-errors` `--no-abort-on-error` `--no-playlist` `--yes-playlist` `--flat-playlist` `--no-flat-playlist` `--lazy-playlist` `--playlist-reverse` `--force-ipv4` `--force-ipv6` `--geo-bypass` `--no-geo-bypass` `--no-check-certificates` `--check-formats` `--list-formats` `--live-from-start` `--no-live-from-start` `--no-mtime` `--restrict-filenames` `--windows-filenames` `--no-cache-dir` `--extract-audio` `--keep-video` `--no-keep-video` `--write-info-json` `--write-description` `--write-thumbnail` `--write-subs` `--write-auto-subs` `--no-write-subs` `--embed-subs` `--embed-thumbnail` `--embed-metadata` `--embed-chapters` `--no-part` `--no-continue` `--no-js-runtimes` `--mark-watched` `--no-mark-watched` `--hls-prefer-native` `--hls-use-mpegts` `--no-hls-use-mpegts`。开关不能带 `=值`。
- **带一个值的选项**——`-f` `-S` `-N` `-r` `-R` `--format` `--format-sort` `--extractor-args` `--playlist-items` `--playlist-start` `--playlist-end` `--max-downloads` `--proxy` `--socket-timeout` `--source-address` `--geo-verification-proxy` `--geo-bypass-country` `--geo-bypass-ip-block` `--xff` `--user-agent` `--referer` `--add-header` `--sleep-interval` `--min-sleep-interval` `--max-sleep-interval` `--sleep-requests` `--sleep-subtitles` `--retries` `--extractor-retries` `--fragment-retries` `--file-access-retries` `--retry-sleep` `--concurrent-fragments` `--limit-rate` `--throttled-rate` `--http-chunk-size` `--buffer-size` `--min-filesize` `--max-filesize` `--date` `--datebefore` `--dateafter` `--match-filters` `--match-filter` `--age-limit` `--download-sections` `--merge-output-format` `--remux-video` `--recode-video` `--audio-format` `--audio-quality` `--sub-langs` `--sub-format` `--convert-subs` `--convert-thumbnails` `--impersonate` `--print` `--parse-metadata` `--compat-options` `--video-password` `--username` `--password` `--twofactor` `--encoding` `--output-na-placeholder`。
- **`--js-runtimes`**——一个值，只能由字母、数字、`,`、`_`、`-` 组成（运行时名称，绝不能是路径）。
- **路径选项**——`--cookies`、`--cache-dir`、`--download-archive`：一个值，必须是**工作区内**的（相对）路径。
- **输出选项**——`-o` / `--output` 与 `-P` / `--paths`：一个值，可带输出类型前缀（`home:`、`temp:`、`default:`、`subtitle:`、`thumbnail:`、`description:`、`annotation:`、`infojson:`、`link:`、`pl_thumbnail:`、`pl_description:`、`pl_infojson:`、`chapter:`、`pl_video:`、`pl_subtitle:`、`pl_chapter:`）；路径部分按路径选项检查。`%(title)s.%(ext)s` 这样的输出模板没问题。
- **`--print-to-file TEMPLATE FILE`**——两个独立的参数；`FILE` 按路径选项检查（`--print-to-file=…` 会被拒绝）。

路径值在以下情况会被拒绝：以 `~` 开头、含 `$` 或 `%NAME%` 形式的环境变量引用、是绝对路径（`/…`、`\…`、`C:…`）、含 `..` 段，或含 `:/` / `:\`。

其余一律拒绝，尤其是：`--exec` 与 `--exec-before-download`、`--downloader` / `--external-downloader` 及其 `-args` 变体、`--use-postprocessor`、`--alias`、`--config-location(s)`、`--plugin-dirs`、`--ffmpeg-location`、`--batch-file` / `-a`、`--load-info` / `--load-info-json`、`--netrc*`、`--cookies-from-browser`。被拒绝的调用会以下列之一 reject：`yt-dlp 参数被拒: 开关 '--x' 不在允许列表内`、`…开关 '--x' 不接受值`、`…开关 '--x' 缺少值`、`…开关 '--x' 的值被拒: <reason>`、`…位置参数 '<arg>': 位置参数只允许 http/https URL`（均带 `插件输出非法: ` 前缀）。

FluxDown 替你添加的内容：

- 始终前置 `--ignore-config`，所以 yt-dlp 自己的任何配置文件都不会被读取。
- ffmpeg 已安装时，会注入指向解析出的托管 / 系统 ffmpeg 的 `--ffmpeg-location`，使合并（bestvideo+bestaudio）、`-x` 抽取音频、remux 和转码都能工作。插件自己传的 `--ffmpeg-location` 仍会被拒绝；只信任宿主自己的路径。
- `--cache-dir <工作区>/.cache` 把 yt-dlp 的缓存收进工作区；你自己传的 `--cache-dir`（相对路径）会覆盖它。
- `--proxy`：应用为本次调用决定了出口时（配置了代理、命中站点规则、任务自带代理、Auto 改走代理），宿主会把它的 `--proxy` 追加在你的参数**之后**，所以应用的选择优先；规则或任务决定的直连会传 `--proxy ""`。未配置代理（或 Auto 正在尝试直连）时，你自己的 `--proxy` 生效。站点规则按第一个 URL 参数匹配。
- 子进程会获得完整的 `PATH`（Windows 上包括用户级条目），使 yt-dlp 能找到 Node、Deno 等 JavaScript 运行时。

所有插件合计最多同时运行 2 个 yt-dlp 进程；等待空闲名额的时间不计入脚本的时间预算。超时或取消时每个子进程都会被杀死。

示例——让 yt-dlp 输出页面的元数据 JSON，从中挑出直链来解析页面：

```js
globalThis.resolve = async (ctx) => {
  if (!flux.ytdlp) return null;
  const r = await flux.ytdlp.run({ args: ['-J', '--no-warnings', ctx.url] });
  if (r.code !== 0) throw new Error('yt-dlp failed: ' + (r.stderr || '').slice(-400));
  if (r.truncatedStdout) throw new Error('yt-dlp output was truncated');
  const info = JSON.parse(r.stdout);
  const direct = info.url || info.formats?.[info.formats.length - 1]?.url;
  if (!direct) return null;
  return { url: direct, fileName: info.title ? `${info.title}.${info.ext || 'mp4'}` : undefined };
};
```

在 `resolve` 里，脚本整体的时间预算（默认 10 秒，至多 30 秒）同样约束这次调用，所以超过它的 `timeoutMs` 在那里没有意义。

### 仅 main 可用的接口

这些接口只存在于常驻的 `main` 脚本中。详细行为见[视图](/docs/zh/plugins/views/)和[可移植界面指南](/docs/zh/plugins/ui-guide/)。

| 接口 | 概述 |
|---|---|
| `flux.ui.view(id, { init, render, event, dispose })` | 注册可移植视图；`id` 必须与已声明的视图一致。 |
| `flux.ui.h(kind, props, ...children)` / `flux.ui.Fragment` | 构建 `{k, p, c}` 节点（子节点会被展平，`null` / `false` / `true` 被丢弃）。通过 [SDK](/docs/zh/plugins/sdk/#jsx) 使用 JSX 会产生同样的形状。 |
| `flux.ui.toast(level, text)` | `info`、`success`、`warning` 或 `error`（其他值按 `info` 显示）。 |
| `flux.commands.register(id, handler)` | 处理器收到 `{ taskIds }`。 |
| `flux.rpc.handle(name, handler, opts?)` | `handler(args, ctx)`。你自己的桌面视图可通过 `fluxdown.invoke(name, args)` 调用，此时 `ctx` 为 `{ caller: "desktop" }`。把 `{ page: true }` 作为 `opts` 传入，还会向你的[页面脚本](/docs/zh/plugins/page-scripts-guide/)暴露该处理函数：页面脚本用 `fluxdown.invoke(name, args)` 调用，`ctx` 为 `{ caller: "page", pageUrl }`。没有带 `page: true` 注册的处理函数会在页面调用时被拒绝，错误为 `rpc not exposed to page: <name>`。`pageUrl` 与 `args` 都来自网页，请当作不可信输入。只接收 `args` 的旧处理函数照常工作。 |
| `flux.events.on(name, handler)` | `name` 是钩子名（`onStart`、`onDone`、`onError`、`onMetaProbed`、`onCancel`），必须列在 `main.events` 中。这类投递**不受** `hooks.match.urls` 过滤。处理器抛异常会被记录并跳过。之后已打开的视图会重新渲染。 |
| `flux.timers.setTimeout(fn, ms)` / `setInterval(fn, ms)` / `clear(id)` | 最多 64 个存活的定时器（`too many timers (max 64)`）。有视图会话打开时，间隔被限制为 ≥ 1 秒，否则 ≥ 60 秒。它们**不是**全局的 `setTimeout` / `setInterval`。 |
| `flux.tasks.list()` / `get(id)` / `create(params)` / `pause(id)` / `resume(id)` / `remove(id, deleteFiles?)` | 需要权限 `tasks`。它们调用 daemon 的 `daemon.task.list` / `get` / `create` / `pause` / `resume` / `delete`，参数和结果与 [RPC API](/docs/zh/api/overview/) 相同（`create` 接受 `{ request: { url, … } }`；`get` / `pause` / `resume` / `remove` 接受任务 id 字符串）。`list()` 无参返回全部任务，任务数超过 5000 时会被拒绝；此时传 `list({ cursor, limit })` 按 `taskId` 升序分页（`cursor` = 上一页最后一个 `taskId`，`limit` ≤ 5000，返回数不足 `limit` 即末页）。只能调用这六个方法。`create` 限制为**每个插件每 60 秒 20 次**（`tasks.create rate limited`）。每次调用都会写入审计日志。 |
| `flux.settings.update({ key: "value" })` | 写入你自己插件的设置。**值必须是字符串**；会按 `manifest.settings` 校验，键或值不合法则 reject。 |
| `flux.ffmpeg`、`flux.ffprobe`、`flux.ytdlp`、`flux.auth` | 见[矩阵](#各入口里有哪些-flux-接口)。`flux.ffmpeg.run` 在 `main` 里总是 reject（没有产物文件）。 |

## JavaScript 运行环境

脚本运行在 FluxDown 内置的 QuickJS 引擎中（基于较新 QuickJS 的分支，已对照锁定的源码核实）。每次调用得到一个标准上下文，包含以下全局对象：

| 可用 | 说明 |
|---|---|
| ECMAScript 内建对象 | `Object`、`Array`、`String`、`Number`、`Boolean`、`Symbol`、`Math`、`JSON`、`Date`、`RegExp`、`Promise`、`Map`、`Set`、`WeakMap`、`WeakSet`、`WeakRef`、`Proxy`、`Reflect`、`BigInt`、类型化数组 / `ArrayBuffer`、`eval`、`globalThis`、`parseInt`、`parseFloat`、`encodeURIComponent` / `decodeURIComponent`、`escape` / `unescape`。 |
| `atob`、`btoa` | 二进制字符串的 Base64，内置。 |
| `queueMicrotask` | 标准行为。 |
| `performance.now()` | 自上下文创建以来的毫秒数；适合计时，不是挂钟时间。 |
| `console` | 被替换为 FluxDown 的日志器（见上）。 |
| `flux` | 注入的 API。 |

**不可用**：`setTimeout`、`setInterval`、`clearTimeout`（所以在 `resolve` 或钩子里没有办法休眠；`main` 里请用 `flux.timers`）、`fetch`（请用 `flux.fetch`）、`URL`、`URLSearchParams`、`TextEncoder`、`TextDecoder`、`crypto`、`structuredClone`、`Intl`、`require`、`import`、`process`、`window` / DOM、`localStorage`。站点需要时请自带实现（例如 MD5 或 HMAC 函数）。以 `__flux_` 开头的名字和 `__fluxHost` 是内部实现，可能随时变化。

异步代码照常工作（`async` / `await`）；入口返回时仍处于 pending 的 Promise 不会被等待——凡是你启动的异步操作，都要 `await`。

## 运行时限制

预算是按次调用计算的。「CPU 预算」是你的 JavaScript 可以执行的时间；等待宿主调用（`flux.fetch`、`flux.ffmpeg.run`、`flux.ytdlp.run`、`flux.settings.update`、`flux.tasks.*`）的时间会被补回，所以等网络不会耗尽它。「墙钟」是含等待在内的总时间。

| 入口 | 墙钟 | CPU 预算 | 内存 |
|---|---|---|---|
| `resolve` | 默认 10 秒；`resolvers[0].timeoutMs` 会替换它，**硬上限 30 秒** | 同一数值，≤ 30 秒 | 默认 64 MB；`resolvers[0].memoryLimitMb` 会替换它，上限 512 MB |
| `subscribe` | 默认 10 秒；`subscriptions[0].timeoutMs` 会替换它，上限 30 秒 | 相同 | 64 MB |
| `authenticate` | 默认 30 秒；`auth.timeoutMs` 会替换它，上限 30 秒 | 相同 | 64 MB |
| 钩子 | 5 秒 | 5 秒 | 32 MB |
| `archive` | 5 秒 | 5 秒 | 与 `resolve` 默认值相同（64 MB） |
| 拥有 `ytdlp` 权限（任何钩子）或拥有 `ffmpeg` 权限（在 `onDone` 中）的插件的钩子 | 约 30 分钟（1830 秒） | 30 秒 | 32 MB |
| `main`（每次调用：渲染、命令、事件、定时器、rpc） | 30 秒 | 2 秒 | 整个运行时 64 MB |

`timeoutMs` 必须大于 `0`。

其他固定限制：

| 项目 | 限制 |
|---|---|
| 日志行 | 4 KB |
| `flux.fetch` | 单请求 10 秒，响应体 8 MB，并发 8（全局），30 次重定向 |
| `flux.storage` | 单值 64 KB，100 个键 |
| `flux.fs` | 单文件 8 MB，每个工作区 64 MB 与 100 个文件，文件名 255 字节 |
| `flux.auth.save` | 每份档案 64 KB |
| `flux.ffmpeg` / `flux.ffprobe` | 默认 300 秒、单次最长 1800 秒；stdout 256 KB，stderr 64 KB；并发 2（全局） |
| `flux.ytdlp` | 默认 300 秒、单次最长 3600 秒；stdout 16 MB（可配置，1–64 MB），stderr 256 KB；并发 2（全局） |
| `flux.tasks.create` | 每个插件每 60 秒 20 次 |
| `main` 定时器 | 每个插件 64 个 |
| `main` 空闲 | 没有 `background` 权限的宿主，在没有打开的视图、没有定时器、没有进行中的调用持续 5 分钟后会被停止；视图会话 90 秒没有 keep-alive 即被关闭。 |

### 并发与过载

- `resolve` 和 `subscribe` 共用一个池。其大小跟随应用的并发下载上限（不低于运行时的 worker 数，worker 至多 4 个）。调用最多等待 **3 秒**获取空闲名额，超时则以 `插件运行时过载（并发已满）` 失败（fail-closed，任务进入错误状态）。
- `authenticate` 有自己的池，容量 **2**，等待同样是 3 秒，所以登录轮询永远不会饿死 resolve。
- `archive` 在整个应用内有自己的池，容量 **2**。调用最多等待 **3 秒**获取名额，超时则作为过载被跳过（不使用子文件夹，下载不受影响）。
- 钩子使用大小等于 worker 数（至多 4）的池。池满时，后续的钩子通知会被**丢弃**（记为 `钩子并发已满，本次钩子被丢弃`），不会排队。
- 拥有 `ffmpeg` 或 `ytdlp` 权限的插件，其钩子使用同样大小的独立池。池忙时，最多有 **64** 个通知排队等待；超出则丢弃（`外部工具钩子排队已满，本次钩子被丢弃`）。
- `main` 每个插件一次只处理一个调用；调用按顺序排队。

### 熔断

`resolve`、`subscribe`、`archive` 或 `main` 的调用中，**连续** 3 次超时（`脚本执行超时`）或超内存（`脚本超出内存上限`）会自动禁用该插件。应用会弹出通知，插件保持关闭直到被手动重新启用。任何其他结果——成功，或其他类型的错误（如非法返回值、网络失败）——都会把计数清零。

不计入的：钩子（它们的超时和内存错误只记录日志）、`authenticate` 和过载拒绝。`main` 里的超时或内存溢出还会销毁运行时；下次调用会启动一个全新的运行时，**内存中的所有状态都会丢失**。

## 你会遇到的错误

错误以 `Error` 对象到达你的代码，`message` 由宿主生成。来自 `flux.*` 调用的信息带有类别前缀：**`插件运行时错误: `**（运行时 / 网络 / 权限失败）或 **`插件输出非法: `**（参数不合法或输出非法）。请匹配稳定的子串，而不是整段文本。

### 让一次调用失败的错误（任务错误文本或 RPC 错误）

对 `resolve`，任务上显示 `[插件] <identity>: <message>`。

| 信息 | 含义与处理 |
|---|---|
| `脚本编译失败: …` | 语法错误，或脚本顶层抛出了异常。 |
| `插件运行时错误: …` | 你的代码抛了异常，或某个被 reject 的 `await` 没有捕获。冒号后面是你的错误文本。函数不存在时包含 `入口 resolve 未定义`。 |
| `脚本执行超时` | 超出墙钟或 CPU 预算。计入熔断（对 `resolve` / `subscribe` / `archive` / `main`）。 |
| `脚本超出内存上限` | 超出内存限制。计入熔断。 |
| `插件运行时过载（并发已满）` | 3 秒内没有空闲名额。见[并发与过载](#并发与过载)。 |
| `缺少必填设置项: 插件 <id> 需先配置「<title>」` | `required` 设置没有值。请让用户填写。 |
| `插件 <id> 当前版本未提供 resolver（或源码读取失败）；…` | 已安装的版本没有声明 resolver，或 resolver 文件在加载之后消失了。仍绑定该插件的任务会 fail-closed，而不是去下载网页 URL。加载时就缺失的 resolver 文件会更早作为加载失败报出——见[调试](/docs/zh/plugins/debugging/)。 |
| `插件输出非法: resolve 返回值非法: …` | 返回的对象无法读取（例如 `totalBytes` 不是整数）。 |
| `插件输出非法: url scheme 不允许: <scheme>` | 链接不是以 `http` / `https` / `ftp` / `magnet` / `ed2k` 开头（空 `url` 会落到这里）。 |
| `插件输出非法: url 超过 8KB` | 链接超过 8 KB。 |
| `插件输出非法: file_name 非法: <name>` | `fileName` 含 `/`、`\`、`..` 或控制字符。 |
| `插件输出非法: variants 过多: N > 50` / `variant label 须非空且 ≤200 字符` | 变体限制。 |
| `插件输出非法: manifest 与 url/variants/audioUrl 互斥` | `manifest` 与其他结果字段同时返回。 |
| `插件输出非法: 二段解析（resolverItem 非空）不可返回 manifest：防递归裂变` | 第二段解析返回了清单。 |
| `插件输出非法: manifest items 数量非法: N（须 1..=1000）` | 条目数量不对。 |
| `插件输出非法: manifest item id 非法` / `name 不可为空` / `path 非法` / `path 深度超限: N > 8` / `落盘路径过长: N > 180` / `kind 非法` / `variants 过多` / `variant id/label 不可为空` | 违反了对应的清单条目规则。 |
| `插件输出非法: authenticate 返回值非法: …` | `authenticate` 返回的不是预期的对象（例如返回了字符串）。 |
| `插件输出非法: 订阅返回值非法: …` | `subscribe` 返回了 `null` 或畸形值。见 [`subscribe`](#subscribectx)。 |
| `插件 <id> 不存在` / `未启用` / `未声明 auth 权限` / `未提供 auth 入口` | 对一个不存在、被禁用、缺少权限或没有 `auth.entry` 的插件调用了 `authenticate`。 |
| `订阅 provider <id> 未安装或已禁用` | 订阅 provider 查找失败。 |

### `flux.*` 的 reject

| 信息（子串） | 来源 | 含义 |
|---|---|---|
| `flux.fetch 仅支持 http/https` | `fetch` | 使用了其他协议。 |
| `blocked: non-routable IP` | `fetch` | 字面量 IP 是环回 / 私网 / 链路本地等地址。 |
| `fetch 失败: …` | `fetch` | 网络错误、TLS 错误、超时、DNS 失败（包括所有地址都被过滤）、`too many redirects`、`blocked: non-routable redirect target`。后面跟着详细原因。 |
| `URL 非法: …` / `HTTP method 非法: …` | `fetch` | URL 或方法无法解析（`opts` 畸形时也会产生）。 |
| `读取响应体失败: …` | `fetch` | 读取响应体时连接中断。 |
| `插件未声明 auth 权限，不能使用认证凭据` | `fetch` | 没有权限 `auth` 却显式传了 `authRef`。 |
| `authentication_required: 未找到认证凭据 <ref>` / `认证凭据已过期 <ref>` | `fetch` | 显式 `authRef`：没有凭据，或已过期。 |
| `认证凭据不属于当前插件` / `认证凭据不属于当前请求站点` | `fetch`、`auth.get` | 凭据的所有者或站点不匹配。 |
| `认证档案必须提供 site` / `认证档案的 site 必须是有效的 URL 或 host` / `authRef 必须等于当前插件和规范化站点组成的引用` / `认证档案超过 65536 字节上限` | `auth.save` | 档案问题。 |
| `authRef 必须属于当前插件` | `auth.remove` | 该引用不是以 `<你的 id>::` 开头。 |
| `storage 值超过 65536 字节上限` / `storage 键数超过 100 上限` | `storage.set` | 限制。 |
| `flux.fs 文件名 '<name>' 非法: <reason>` | `fs.*` | 文件名不合法。 |
| `flux.fs 文件超过 8388608 字节上限` / `工作区总量超过 67108864 字节上限` / `文件数超过 100 上限` | `fs.writeFile` | 限制。 |
| `flux.task.recordArtifact 仅 onDone 钩子可用` / `recordArtifact: …` | `task.recordArtifact` | 钩子不对、名字非法、文件不以任务文件主干开头，或文件不存在。 |
| `flux.ffmpeg.run 仅在有产物文件的钩子（如 onDone）中可用` / `flux.ffprobe.run …` | `ffmpeg.run` / `ffprobe.run` | 在 `onDone` 之外调用。 |
| `ffmpeg 未安装或不可用` / `ffprobe 未安装或不可用（随 ffmpeg 组件一并安装）` / `yt-dlp 未安装或不可用` | `run` | 工具没有安装。 |
| `ffmpeg args 不可为空` / `yt-dlp args 不可为空` | `run` | `args` 为空（`spec` 畸形时也会产生）。 |
| `ffmpeg 参数过多（>512）` / `ffmpeg 参数过长` / `ffmpeg 参数含 NUL`（`yt-dlp` 同理） | `run` | 参数数量 / 大小限制。 |
| `ffmpeg 参数 '<arg>' 被拒: <reason>` | `ffmpeg.run`、`ffprobe.run` | 参数被拦截。 |
| `yt-dlp 参数被拒: …` | `ytdlp.run` | 参数不在白名单内。 |
| `ffmpeg subdir '<x>' 非法` / `… subdir 逃逸牢笼`（`yt-dlp` 同理） | `run` | `subdir` 不安全。 |
| `启动 ffmpeg 失败: …` / `ffmpeg 执行失败: …`（`yt-dlp` 同理） | `run` | 进程无法启动，或在宿主侧崩溃。正常的非零退出码会 **resolve**，不在此列。 |
| `tasks.create rate limited` | `tasks.create` | 60 秒内创建超过 20 次。 |
| `method not allowed for plugins: …` | `tasks.*` | 内部防护；只能调用那六个任务方法。 |
| `任务参数非法: …` / `settings.update 参数非法: …` | `tasks.*`、`settings.update` | 参数不是合法的可序列化数据 / 不是字符串映射。 |

### 只写入日志的信息

| 日志文本 | 含义 |
|---|---|
| `hook <name> 执行失败（已忽略）: <error>` | 某个钩子抛异常或超时。 |
| `钩子并发已满，本次钩子被丢弃` | 钩子池已满，通知被丢弃。 |
| `外部工具钩子排队已满，本次钩子被丢弃` | 外部工具钩子队列（64）已满。 |
| `flux.task.requestRetry 仅 onError 钩子可用，已忽略` | 在 `onError` 之外调用了 `requestRetry`。 |
| `[plugin] subscription output: skipped N of M items, first reason: …` | 部分订阅条目非法。 |
| `setInterval interval clamped to …ms` | `main` 的间隔被提升到最小值。 |

## 接下来看什么

- 声明入口和权限：[清单](/docs/zh/plugins/manifest/)与[权限](/docs/zh/plugins/permissions/)。
- 视图和节点词汇：[视图](/docs/zh/plugins/views/)。
- 工具链与类型化入口：[SDK](/docs/zh/plugins/sdk/)。
- 排查故障：[调试与排错](/docs/zh/plugins/debugging/)。
- 可运行的示例：[feature-demo](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/feature-demo) 与 [youtube-down](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/youtube-down) 插件。
