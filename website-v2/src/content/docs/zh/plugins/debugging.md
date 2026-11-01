---
title: 调试与排错
description: 插件日志在哪、怎么搜；每种改动之后该做什么；以及你会遇到的报错信息、原因与解决办法对照表。
section: plugins
order: 9
sourceHash: "158dc2a39164"
---

这一页写给「出问题了」的时刻：去哪里看、插件的各种状态是什么意思、某个改动需要什么操作，以及最有用的——**报错信息**与它们的原因。

> **关于信息的语言。** FluxDown 插件引擎抛出的错误大多是中文，你在应用和日志里看到的正是这些原文。本页**逐字**引用它们，方便你直接搜索。SDK 命令行的信息是英文。

## 快速检查清单

插件「毫无反应」时，按顺序检查：

1. **加载了吗？** 在**设置 → 扩展 → 插件**里，这一行必须显示**已加载**。**加载失败**表示 FluxDown 读不了它；点开错误（「插件加载错误」，带复制按钮），见[安装与校验报错](#安装与校验报错)。
2. **启用了吗？** 看名称旁边有没有**已禁用**、**已自动禁用**、**已被市场撤回**或**需要授权**，见[插件状态](#插件状态)。
3. **重新加载了吗？** manifest 和 `main` 的改动要点**重新加载**才生效，见[开发循环](#开发循环)。
4. **URL 匹配吗？** resolver、hook 和拦截器的规则是对完整 URL 匹配的，见 [resolver 问题](#resolver-问题)和[拦截器问题](#拦截器问题)里的检查项。
5. **日志怎么说？** 搜索 `[plugin:<你的 identity>]`，见[日志](#日志)。

## 日志

### 哪些内容会进日志

| 你写的 | 记录为 |
|---|---|
| `flux.logger.info(...)`、`console.log`、`console.info`、`console.debug` | 一行 INFO |
| `flux.logger.warn(...)`、`console.warn` | 一行 INFO——在日志文件里**区分不出 warn 和 info** |
| `flux.logger.error(...)`、`console.error` | 一行 ERROR（立即刷盘） |

每一行都带前缀 `[plugin:<identity>]`，后面是你的参数：字符串原样输出，其余值输出为 JSON，参数之间用一个空格连接。一行日志大致如下：

```
14:02:31.512 INFO … [plugin:me@mirror-demo] rewriting https://mirror-demo.invalid/files/1Mb.dat -> https://proof.ovh.net/files/1Mb.dat
```

（中间的部分是日志器附加的源码位置信息。）每条消息超过 4 KB 会被截断。

实际使用中要记住：

- **按 identity 过滤，而不是按级别：** 搜索 `[plugin:me@mirror-demo]`。搜「warn」或「error」会漏掉你自己写的警告。
- `Error` 对象会被记成 `{}`（它没有可枚举属性）。请改记 `String(error)` 或 `error.message`。
- 引擎自己会写以 `[plugin]`（冒号后面没有 identity）开头的行，记录加载、启停和熔断事件，例如 `[plugin] 跳过 …`（有插件加载不了）、`[plugin] <identity> 连续超时，已自动熔断禁用`。搜索 `[plugin` 两种都能看到。
- `main` 里的 `console.log` 和其他所有插件输出一样进 **daemon 日志**，不会出现在浏览器控制台里——即使视图是在 Web 端显示的。

### 文件在哪

插件代码运行在 FluxDown 的后台服务（daemon）里，所以插件输出进的是 **daemon 的**日志目录：daemon 数据目录下的 `logs/`。文件名形如 `fluxdown_YYYY-MM-DD.log`（滚动后为 `fluxdown_YYYY-MM-DD.N.log`）；单个文件到 2 MB 就滚动，超过 7 天的日志会被删除，目录总大小默认上限 10 MB（可在**设置 → 关于 → 日志占用上限**修改）。

| 系统 | daemon 日志目录 |
|---|---|
| Linux | `$XDG_DATA_HOME/fluxdown/logs/`（默认 `~/.local/share/fluxdown/logs/`） |
| macOS | `~/Library/Application Support/fluxdown/logs/` |
| Windows 安装版 | `%LOCALAPPDATA%\FluxDown\logs\` |
| Windows 便携版（exe 旁有 `portable` 标记文件） | `<程序目录>\portable_data\logs\` |
| 设置了 `FLUXDOWN_DATA_DIR` 的**任何**系统——无头服务器、Docker | `$FLUXDOWN_DATA_DIR/logs/`（Docker 镜像里是 `/data/logs/`） |

桌面应用和 agent 会把**它们自己的**诊断日志（`desktop.log`、`agent.log`）写进应用数据根目录下另一个 `agent/logs/` 文件夹。插件输出**不在**那里。

### 在应用内打开与导出日志

| 位置 | 作用 |
|---|---|
| **设置 → 关于 → 导出日志**（桌面端和 Web 端） | 保存一个 zip，包含 agent 日志和 daemon 日志（在 `daemon/logs/` 里），令牌、Cookie 等敏感信息已被遮盖。这是在任何部署（包括远程主机）下拿到插件输出最可靠的办法。 |
| **设置 → 关于 → 打开目录**（桌面端）和**打开日志目录**菜单 / 命令面板项 | 打开桌面应用的 **agent** 日志目录（`desktop.log`、`agent.log`）——适合排查应用自身的崩溃，但里面**没有**你插件的日志行。请改去打开上表中的 daemon 目录。 |

Web 端没有可打开的文件夹，请用**导出日志**。（旧版服务端的 `/api/v1/logs` REST 接口已经不存在。）

### 想要更详细的日志

日志级别默认是 INFO，而你的 `flux.logger` 输出本来就是 INFO，所以通常不需要调整。要提高 daemon 自身的详细程度，请在启动 daemon 的进程环境里设置 `FLUXDOWN_LOG_LEVEL`（`error`、`warn`、`info`、`debug`、`trace`）或 `RUST_LOG`，然后重启。

## 开发循环

用**从目录安装（开发模式）**安装插件，才能获得最快的循环。开发模式插件记录你的文件夹路径并直接读取它。它所在行上的**重新加载**按钮（提示文字：「重新加载（重读 manifest 与脚本）」）会重读 manifest 和全部脚本，并重新执行安装时的检查。如果失败，会显示错误，并且开发模式登记**会保留**，你可以改好文件再次重新加载。

| 你改了什么 | 该怎么做 |
|---|---|
| resolver、hooks、interceptors、auth 或订阅脚本 | 什么都不用做——开发模式插件**每次调用**都会读这些文件。SDK 项目要重建（`npm run dev` 会在保存时自动重建）。 |
| `main` 脚本 | **重新加载。** `main` 是一个长期存活的上下文，只在启动时读取；打开着的视图会话会结束（显示「视图会话已结束」和**重新打开**按钮），重新打开后运行的是新代码。 |
| `manifest.json`（任何内容：匹配规则、入口、设置项、视图、命令、权限、版本） | **重新加载。** manifest 只在加载插件时解析。 |
| 新增了权限或网络主机的 manifest 改动 | **重新加载**，然后在对话框里批准（见[插件状态](#插件状态)）。 |
| 桌面脚本（`desktop.entry`） | 关掉视图再重新打开；如果同时改了 manifest，还要**重新加载**。 |
| 某个插件设置项的值 | 什么都不用做——下次调用时就会传入（并会推送给运行中的 `main`）。 |
| SDK 的配置文件（`fluxdown-plugin.config.json`） | 重启 `npm run dev`，然后**重新加载**。 |
| 由 `.fxplug` 或市场安装的插件 | 重新安装：非开发模式插件在加载时只读入内存一次。 |

请记住：`npm run build`、`npm run pack` 和 `npm run dev` 会先删除 `dist/`。如果 `dist/` 就是你装的开发模式文件夹，请等构建结束再测试；之后若显示**加载失败**，点**重新加载**。

把启用开关关掉再打开，附带效果也会重新加载所有插件，但只有**重新加载**按钮会把错误反馈给你。

开发模式登记优先于同一 identity 的普通安装副本。要测试打好的 `.fxplug`，请先卸载开发模式那一份。

## 插件状态

| 行上显示 | 含义 | 怎么办 |
|---|---|---|
| **已加载** | manifest 解析成功，插件已在内存里。 | — |
| **加载失败** | manifest 或某个必需文件有问题，或者应用版本太旧。点开可看「插件加载错误」，**复制错误**可复制。 | 修好后**重新加载**（开发模式）或重新安装。 |
| **需要授权** | manifest 请求了你还没批准的权限或网络主机。插件处于禁用状态，启用开关被锁住，并显示**授权**按钮。用其他方式启用会失败，信息里含 `consent required`。 | 点**授权**并批准。新版本请求*更多*权限时需要再次批准；请求更少则不用。 |
| **已禁用** | 你手动关掉了它。 | 重新打开。 |
| **已自动禁用** | 触发了熔断，见下文。 | 修好脚本，再把它打开（安装更新也会自动重新启用）。 |
| **已被市场撤回** | 插件市场撤回了这个版本。启用时报 `该插件版本已被插件市场撤回，请升级后再启用`。 | 更新插件。 |
| 「开发模式」徽标 | 通过目录安装。 | — |

### 熔断器

resolver、拦截器、订阅 provider 或 `main` 调用中，**连续** 3 次超时或内存超限，会让插件被自动禁用（出现其他结果即清零；hooks 和 `authenticate` 不计入）。你会收到提示（「插件「…」已因连续失败被自动禁用，可在「设置 → 扩展 → 插件」中重新启用」），日志里有 `[plugin] <identity> 连续超时，已自动熔断禁用`，需要这个插件的任务会失败，报 `插件 <identity> 已被熔断自动禁用；重新启用插件，或选择「忽略插件重试」按原始链接下载`。常见原因：`flux.fetch` 在等一个永远不回应的主机、死循环、把巨大的响应读进内存。可以在 manifest 里调低 `timeoutMs` 来比默认值（resolver 10 秒，拦截器 5 秒）更快失败，给 `flux.fetch` 加上错误处理，然后重新启用。

## 安装与校验报错

这些错误出现在安装、重新加载或运行 `npm run validate` 的时候。安装对话框会显示 `安装失败：…` / `插件安装失败：…`，后面跟着信息；同样的文字也会写进日志。

### 引擎报错（与 `npm run validate`）

引擎报告这些错误时会带前缀 `manifest 非法: `。`npm run validate` 和 SDK 校验器使用同一套规则、几乎相同的措辞，但没有这个前缀，并且会一次列出全部问题，而不是遇到第一个就停。措辞不同的地方，下表给出的是引擎的原文。

| 信息 | 原因 | 解决 |
|---|---|---|
| `JSON 解析失败: …` | `manifest.json` 不是合法 JSON、某个值的类型不对，或含有引擎不认识的字段（此时详情里是 `unknown field …`）。 | 修正语法；删掉未知字段——拼写和嵌套层级都会被检查，见 [Manifest 参考](/docs/zh/plugins/manifest/)。用 SDK 校验时得到的是 `manifest: 未知字段 <key>`。 |
| `identity '…' 非法：须匹配 ^[a-z0-9_-]+@[a-z0-9_-]+$` | identity 不是由小写字母、数字、`_`、`-` 组成的 `作者@名称`。 | 改名，例如 `me@my-plugin`。不能有点，不能有大写。 |
| `name 不可为空` | `name` 为空。 | 填上。 |
| `version '…' 非法：须为 MAJOR.MINOR.PATCH` | 不是用点分隔的三个数字。 | 用 `1.0.0`。SDK 和插件索引更严格：不能有 `v` 前缀和后缀。 |
| `v1 每插件至多一个 resolver` / `v1 每插件至多一个 subscription provider` | `resolvers` / `subscriptions` 里有多个条目。 | 只保留一个。 |
| `resolver entry 路径 '…' 非法`（`hooks entry`、`auth entry`、`subscription entry`、`main entry`、`icon`、`sniffRules` 同理） | 绝对路径、盘符、`..`、空段或控制字符。 | 使用包内的相对路径，例如 `resolve.js`。 |
| `resolver match.urls 不可为空` / `hooks match.urls 不可为空` | 规则列表为空。 | 添加一条规则，或者干脆不写 `hooks.match`。 |
| `interceptors entry 路径 '…' 非法` | 路径规则与其他入口相同。 | 使用包内的相对路径，例如 `intercept.js`。 |
| `interceptors.stages 不可为空` | `interceptors.stages` 是空列表。 | 至少列出一个阶段。 |
| `未知拦截阶段 '…'，合法：[…]` | 阶段名不存在。 | 使用信息里列出的名称之一（`prepare`、`refresh`、`probed` 等）。 |
| `interceptors.stages 重复：'…'` | 同一个阶段列了两次。 | 删掉重复项。 |
| `interceptors match.urls 不可为空` | 规则列表为空。 | 至少添加一条规则；`interceptors.match` 必填。 |
| `interceptors timeoutMs 不可为 0` | `timeoutMs` 为 `0`。 | 用正整数，实际生效最多 30000。 |
| `声明 interceptors 时 permissions 必须包含 intercept` | 声明了 `interceptors` 却没有 `intercept` 权限。 | 把 `"intercept"` 加进 `permissions`。 |
| `resolver timeoutMs 不可为 0` | `timeoutMs` 为 `0`（`auth` 与 `subscription` 同样会检查）。 | 用正整数，实际生效最多 30000。 |
| `hooks.events 不可为空` | `hooks` 没有事件。 | 至少列出一个事件。 |
| `未知事件 '…'，合法：[…]` | 不是 `onStart`、`onError`、`onDone`、`onMetaProbed`、`onCancel` 之一。 | 改正名称（`main.events` 同样适用，其信息为 `main.events 未知事件 '…'，合法：[…]`）。 |
| `未知权限 '…'，合法：[…]` | 权限名不存在。 | 使用 `ffmpeg`、`ytdlp`、`auth`、`tasks`、`browserRules`、`desktop`、`background`、`intercept`、`pageScripts` 之一。 |
| `声明 auth.entry 时 permissions 必须包含 auth` | 声明了 `auth` 却没有 `auth` 权限。 | 把 `"auth"` 加进 `permissions`。 |
| `声明 sniffRules 时 permissions 必须包含 browserRules` | 有嗅探规则却没有对应权限。 | 加上 `"browserRules"`。 |
| `main.background 为 true 时 permissions 必须包含 background` | `main.background` 为 `true` 却没有 `background` 权限。 | 加上 `"background"`。 |
| `声明 pageScripts 时 permissions 必须包含 pageScripts` | 有 `contributes.pageScripts` 却没有对应权限。 | 把 `"pageScripts"` 加进 `permissions`。 |
| `pageScript '<id>' …`（id、title、matches、js 相关错误） | 某条页面脚本违反了 [Manifest 参考](/docs/zh/plugins/manifest/#pagescripts)中的规则。 | 修正消息里指出的字段。 |
| `view '<id>' 有可移植实现，必须声明 main` | 没有 `desktop`（或带 `portable: true`）的视图需要 `main` 脚本。 | 声明 `main`。 |
| `声明 commands 时必须声明 main` | 命令需要 `main`。 | 声明 `main`。 |
| `view id '…' 非法：…` / `command id '…' 非法：须匹配 ^[a-z][a-z0-9-]{0,31}$` | id 不是以小写字母开头、由 `a-z0-9-` 组成、至多 32 个字符。 | 改名。 |
| `view id '…' 重复` / `command id '…' 重复` / `setting key '…' 重复` | id 或 key 重复。 | 改成唯一的。 |
| `contributes.views 至多 16 项` / `contributes.commands 至多 32 项` | 贡献项太多。 | 减少。 |
| `view '<id>' title 须为 1–40 个字符` / `command '<id>' title 须为 1–40 个字符` | 标题为空或超过 40 个字符。 | 缩短。 |
| `view '…' networkHosts '…' 非法` / `…重复` | 网络主机不是小写 DNS 名或 IP 字面量（不能带协议、端口、路径或通配符），或者重复。 | 写成 `api.example.com`。 |
| `setting key 不可为空` | 设置项的 key 为空。 | 填上。 |
| `setting '<key>': …` | 某个设置项有问题——widget 与类型不匹配、`select` 没有 `options`、`default` 不在选项里、`min` > `max`、数字或布尔的默认值不对。 | 见 [Manifest 参考](/docs/zh/plugins/manifest/)。 |
| `setting '<key>': pattern 非法（JS RegExp 编译失败）` | `pattern` 不是合法的 JavaScript 正则表达式。 | 修正正则。 |
| `包内缺少文件 '…'` | manifest 声明的某个文件不在文件夹或 zip 里：`main`、`resolvers`、`hooks`、`interceptors`、`auth` 或 `subscriptions` 的入口，某个 `desktop` 入口，或 `sniffRules`。信息里的路径与 `manifest.json` 中的写法一致。`dist/` 被清空或从未构建的开发模式插件，在安装和**重新加载**时就会这样报错（该行显示**加载失败**），而不是等到任务调用 resolver 时才发现。SDK 校验还会检查 `icon`。 | 创建该文件，或改对路径。SDK 项目要检查：脚本是否列在 `fluxdown-plugin.config.json` 的 `entries` 里，输出名是否一致。重新构建后点**重新加载**。 |
| `读取入口文件 '…' 失败: …` | 入口文件存在但读不出来（没有权限、路径是个目录，或文件不是合法的 UTF-8）；冒号之后是系统错误。出现的场合同上。 | 修好权限或文件（UTF-8 文本），然后**重新加载**。 |
| `main entry '…' 扩展名须为 .js` | `main` 不是 `.js` 文件。 | 改名。 |
| `sniffRules '…' 扩展名须为 .json` / `sniffRules format 须为 fluxdown-sniff-pack` / `sniffRules 至多 500 条规则` / `sniffRules 文件超过 262144 字节` | 嗅探规则包有问题。 | 见 [Manifest 参考](/docs/zh/plugins/manifest/)。 |
| `view '<id>' icon 路径 '…' 非法` / `view '<id>' icon '…' 扩展名须为 .svg` | 视图 `icon` 不是安全的包内相对路径，或不是 `.svg` 文件。 | 使用包内的相对 `.svg` 路径。 |
| `view '<id>' icon 超过 16384 字节` / `view '<id>' icon 不是合法 UTF-8` | 图标文件超过 16 KiB，或不是 UTF-8 文本。 | 精简 SVG；以 UTF-8 保存。 |
| `view '<id>' icon 不是 SVG 文档` | 去掉可选的 BOM 与空白后，文件不以 `<svg` 或 `<?xml` 开头，或不含 `<svg`。 | 提供真正的 SVG 文件。 |
| `需要 App ≥ X，当前 Y` | `minAppVersion` 比你的应用新。 | 升级 FluxDown，或降低 `minAppVersion`。 |
| `脚本编译失败: …` | resolver、hooks、interceptors、auth 或订阅脚本有语法错误——**或者在顶层就抛了异常**。检查时会在一个没有 `flux` 和 `console` 的空白上下文里把脚本求值一次。 | 修正语法；把所有 `flux.*` / `console.*` 调用挪进入口函数。TypeScript 项目请运行 `npm run typecheck`，并确认产物已重新构建。 |
| `安装后未能加载插件（校验失败或版本门槛不满足）` | 上面这些检查在安装过程中失败了；如果有旧版本，已经被恢复。 | 看日志里更早的几行，找到真正的原因。 |
| `读取 manifest 失败: …` / `读取 manifest.json 失败: …` | 你选的文件夹里没有 `manifest.json`。 | 选**直接包含** `manifest.json` 的那个文件夹——SDK 项目是 `dist/`，不是项目根目录。 |
| `zip 内未找到根 manifest.json` | zip 根目录（或它唯一的顶层文件夹）里没有 `manifest.json`。 | 压缩 `dist/` 的*内容*；`npm run pack` 会正确处理。 |
| `zip 条目数 N 超过上限 200` / `解压总量超过 … 字节上限（疑压缩炸弹）` | 文件超过 200 个，或解压后超过 50 MB。 | 精简插件包。 |
| `zip 含非法路径（zip-slip）` | 压缩包含有 `..` 或绝对路径。 | 用正常的压缩工具重新打包。 |
| `插件加载失败: …` | 对开发模式插件**重新加载**时发现了加载失败；冒号后面是原因（上面某一条信息）。 | 修好后再重新加载。 |
| `不是开发模式插件: …` | 对已安装（非开发模式）的插件请求了**重新加载**。 | 改为重新安装。 |

### SDK 命令行的报错

| 信息 | 原因 |
|---|---|
| ``dist/ not found; run `fluxdown-plugin build` first`` | 没有先构建就运行了 `validate`。 |
| `dist/manifest.json missing` | 构建产物不完整。 |
| `package has N files; the limit is 200`、`package is N bytes; the limit is 52428800` | 包的大小限制。 |
| `fluxdown-plugin.config.json: unknown key '…'` / `…: entries must be an object of string → string` / `…: entries key '…' is not a safe relative path` | 配置文件写错了。 |
| `<path> is not empty` | 往非空文件夹里执行了 `create`。 |
| `<flag> needs a value` | `--选项` 后面没有值。 |
| 带 `文件:行:列` 的 esbuild 错误块 | 某个被打包的文件里有 TypeScript/JSX 错误；到该位置修正。 |

## resolver 问题

resolver 失败时，任务会进入**下载失败**，信息以 `[插件] <identity>: ` 开头，后面是错误文本。FluxDown **绝不会**自己退回去使用原始页面 URL（fail-closed），因为那可能把一个 HTML 页面存成视频。

### 忽略插件重试

在失败的任务上右键，选择**忽略插件重试**（有一个确认对话框说明含义）。FluxDown 会清除这个任务的插件绑定，并用**原始链接**重试——插件坏了、被卸载或被禁用，而你想要普通下载时就用它。卸载插件时，会对它的所有任务应用同样的逃生机制。

### 任务运行时的信息

这些信息都以 `[插件] <identity>: <信息>` 的形式出现。来自宿主的信息会被包装成 `插件运行时错误: …`、`插件输出非法: …` 等。

| 信息 | 原因 | 解决 |
|---|---|---|
| `插件运行时错误: <你的错误信息>` | 你的脚本抛了异常（或者某个被拒绝的 `await` 没有捕获）。`<你的错误信息>` 就是 `error.message`。 | 看它前面的日志行；想自己处理失败，就给 `flux.fetch` 包上 `try/catch`。 |
| `插件运行时错误: 入口 resolve 未定义` | 脚本跑了，但没有在 `globalThis.resolve` 上放函数——漏了 `defineResolver(...)`、函数名写错，或者用了 ES 的 `export` 而不是全局函数。 | 定义 `resolve`。hook 的名字是 `onStart`/`onError`/`onDone`/`onMetaProbed`/`onCancel`；auth 是 `authenticate`；订阅是 `subscribe`。 |
| `脚本编译失败: …` | 脚本有语法错误。 | 修正。 |
| `脚本执行超时` | 调用没有在预算内完成（默认 10 秒，最多 30 秒）。计入熔断。 | 提速或减少请求数；确实有慢站点时把 `timeoutMs` 调到最多 30000。 |
| `脚本超出内存上限` | 使用超过内存预算（默认 64 MB）。计入熔断。 | 不要持有大响应；谨慎对待被 `truncated` 截断的响应体。确实需要更多内存的 resolver 可声明 `memoryLimitMb`（最多 512）。 |
| `插件运行时过载（并发已满）` | 同时的解析太多；等待空闲名额超过 3 秒。 | 重试任务。 |
| `插件输出非法: resolve 返回值非法: …` | 返回的对象字段类型不对，比如 `totalBytes` 不是数字。 | 按 [API 参考](/docs/zh/plugins/api-reference/)返回正确的结构。 |
| `插件输出非法: url scheme 不允许: …`、`url 超过 8KB`、`file_name 非法: …`、`variants 过多: N > 50`、`manifest 与 url/variants/audioUrl 互斥` | 返回结果违反了某条规则：不支持的 URL 协议、URL 过长、不安全的文件名、变体太多，或同时返回 `manifest` 和 `url`。 | 修正返回字段。 |
| `缺少必填设置项: 插件 <identity> 需先配置「<标题>」` | 某个 `required: true` 的设置项既没有值也没有默认值。脚本根本不会被调用。 | 在插件设置（齿轮按钮）里填上，或给它一个 `default`。 |
| `插件运行时错误: 插件 <identity> 已卸载或加载失败；可在任务菜单选择「忽略插件重试」按原始链接下载` | 任务绑定的插件已被卸载或加载失败。 | 重装或修好插件，或者**忽略插件重试**。 |
| `插件运行时错误: 插件 <identity> 已被禁用；重新启用插件，或选择「忽略插件重试」按原始链接下载` | 插件被禁用了（熔断后这句话里是 `已被熔断自动禁用`，被撤回则是 `已被插件市场撤回`）。 | 启用它，或者**忽略插件重试**。 |
| `插件运行时错误: 插件 <identity> 当前版本未提供 resolver（或源码读取失败）…` | 已安装的版本没有声明 resolver，或者 resolver 文件在插件加载之后消失了（例如插件运行期间 `dist/` 被清空）。加载插件时就已经缺失的 resolver 文件，会更早作为**加载失败**报出：`包内缺少文件 '…'`（见上）。 | 检查 `entry` 文件是否存在；重新构建并**重新加载**。 |

**完全没有反应**（原始 URL 被原样下载）？说明 resolver 根本没被选中：

- 规则没有匹配。规则是对完整 URL 不区分大小写地匹配；`*` 是唯一的通配符；第一个 `*` 之前的文字必须是 URL 的前缀，最后一个 `*` 之后的文字必须是后缀。`example.com/*` **匹配不了** `https://example.com/x`——应写 `*://example.com/*`。
- 插件被禁用、仍处于**需要授权**，或者没有加载。
- 选中了别的插件。有多个插件匹配时，FluxDown 会问一次并可能记住选择；请检查**设置 → 扩展 → 站点默认**。无人值守的下载使用站点默认插件，或 identity 最小的插件。
- 添加 URL 的方式绕过了插件——用常规的**新建下载**对话框添加链接来对照检查。

另见 [resolver 教程](/docs/zh/plugins/your-first-plugin/#5-测试与调试)。

### `flux.fetch` 与被拦截的地址

`flux.fetch` 只能访问**公网**的 HTTP(S) 主机。回环、局域网私有地址、链路本地、云元数据以及其他不可路由的地址都被有意拦截，包括经由域名解析和重定向到达的情况。

| 信息（出现在 `插件输出非法:` / `插件运行时错误:` 之后） | 原因 |
|---|---|
| `URL 非法: …` | `url` 无法解析。 |
| `flux.fetch 仅支持 http/https` | 用了 `file:`、`ftp:` 等 URL。 |
| `blocked: non-routable IP` | URL 是字面量 IP，如 `127.0.0.1`、`192.168.x.x`、`169.254.x.x`。 |
| `blocked: non-routable redirect target` | 服务器重定向到了这类地址。 |
| `fetch 失败: …` 加详情 | 连接错误、TLS 错误、超时（每个请求 10 秒），或域名只解析到非公网地址。 |
| `插件未声明 auth 权限，不能使用认证凭据` | 没有 `auth` 权限却传了 `authRef`。 |

**例外：开发模式插件。**通过「设置 → 扩展 → 插件 → 从目录安装（开发模式）」加载的插件，`flux.fetch` 不受公网地址限制，可访问 `localhost` 或局域网服务，便于连接本机服务联调。它与其他插件一样遵循应用的代理设置与站点规则，只是环回、局域网私网段与本地域名恒直连；正式安装的插件行为不变。下面的替代做法适用于非开发模式安装。

开发时**怎样对着自己的服务器测试**：`flux.fetch` 访问不了 `localhost` 或你的局域网。可选做法：把测试环境放在公网主机上（HTTPS，端口不限）、使用公共的 mock 服务，或者在一个设置项后面放一份写死的数据，让插件跳过请求直接返回。对于只改写 URL 的 resolver，根本不需要服务器——resolver 可以直接返回固定的 `url`（见 [TypeScript 项目](/docs/zh/plugins/typescript-project/#4-添加-resolver)里 `.invalid` 主机的做法）。响应体只支持文本（超过 8 MB 时 `truncated: true`）。

## hook 问题

hook 没法让任务失败，所以它们的错误只出现在日志里，形如带 `[plugin:<identity>]` 的行：

| 日志文本 | 含义 |
|---|---|
| `hook onDone 执行失败（已忽略）: <错误>` | hook 抛了异常、超时，或入口缺失（`入口 onDone 未定义`）。hook 名就是对应的事件处理函数名。 |
| `钩子并发已满，本次钩子被丢弃` | 同时运行的 hook 太多，这次通知被丢弃，而不是排队。 |
| `外部工具钩子排队已满，本次钩子被丢弃` | 使用 ffmpeg / yt-dlp 的 hook 的同类情况。 |

请记住：同一个插件里的 resolver 与 `onMetaProbed` 不能共存——带 resolver 的任务会跳过元数据探测，加载日志会警告 `同时声明 resolver 与 onMetaProbed；带 resolver 的任务跳过探测，该钩子不会触发`。另外，hook 只对**已启用**、且 `hooks.match`（如果有）匹配任务 URL 的插件运行。ffmpeg 与重试见[事件钩子指南](/docs/zh/plugins/hooks-guide/)。

## 拦截器问题

拦截器能改动任务，所以它的影响体现在三个地方：任务的活动日志、任务的错误信息和审计日志。

### 活动记录

打开任务详情的**日志**页：类型为**插件**的条目由拦截器写入。条目只写字段名、阶段和文件名——绝不写请求头的值、Cookie 或请求体。

| 条目 | 含义 |
|---|---|
| `插件 <identity> 改写了请求：<字段>` | 应用了 `prepare`（或 resolver `request`）补丁；列出被改的字段，例如 `请求头`、`Cookie`、`线程数`。 |
| `插件 <identity> 的文件名改写已忽略（任务已开始下载）` | `fileName` 只在任务首次运行时生效；恢复和重试会忽略它。 |
| `插件 <identity> 调整了并发：…` | `probed` 的结果改变了 `segments` 或 `maxConnections`。 |
| `插件 <identity> 刷新了凭据（HTTP <状态码>）：<字段>` | `refresh` 在请求被 401 / 403 / 410 拒绝后换发了新凭据。 |
| `插件 <identity> 选择了第 N 个画质` / `…个Period` | `selectVariant` / `selectPeriod` 选中了某一项（从 1 开始计数）。 |
| `插件 <identity> 过滤了 N 个视频分片` / `…音频分片` | `segments` 或 `liveSegments` 丢弃了分片。 |
| `插件 <identity> 改写了 N 个…分片地址` | `segments` 的 `rewrite` 替换了分片 URI。 |
| `插件 <identity> 请求在 N 毫秒后重试` | `failed` 调用请求了重试。 |
| `插件 <identity> 的 <阶段> 阶段出错，已忽略：<信息>` | 调用抛异常、超时、返回了非法内容或非法补丁，且 `onFailure` 为 `continue`。任务不带这项改动继续进行。 |
| `正在执行插件下载后处理` / `插件后处理已跳过：找不到下载文件` / `插件后处理已跳过：读取任务信息失败` | `finalize` 正在执行，或因读不到文件或任务记录而被跳过。 |
| `插件后处理已取消，任务按原文件完成` | `finalize` 期间任务被暂停或取消。 |
| `插件 <identity> 将下载文件替换为 <文件名>` | `finalize` 返回了 `replaceWith`。 |

### 失败信息

`onFailure` 为 `fail`，或某个阶段本身就是无条件中止（`probed` 与 `finalize` 的 `fail`、被拒绝的 resolver 补丁）时，任务进入**下载失败**，信息为

```
[插件] <identity>（<stage>）：<message>
```

resolver 补丁被拒绝时则是 `[插件] <identity>: <message>`。`<message>` 是插件自己的 `fail` 文本，或引擎给出的原因，例如 `请求头 '<name>' 由引擎管理，不可设置`、`仅 HTTP 任务可声明 ephemeral`、`url 只能在同一主机内改写（跨站改写请使用 resolver）`、`url 不能改变下载协议类别`。补丁是全有或全无的：只要有一个字段不合法，整个补丁都不会生效。

### 审计事件

每次调用都会写一条审计记录 `intercept.<stage>`（如 `intercept.prepare`），目标为任务 id，结果为 `ok` 或 `error`，详情里是错误文本。见[权限](/docs/zh/plugins/permissions/#审计日志)。

### 常见误区

- **根本没有被调用。** 插件没有已批准的 `intercept` 权限（该行显示**需要授权**）、被禁用，或者 `interceptors.match.urls` 既没匹配任务的原始 URL，也没匹配 resolver 产出的 URL。规则写法见 [resolver 问题](#resolver-问题)：用 `*://example.com/*`，而不是 `example.com/*`。BitTorrent 与 eD2K 任务从不被拦截。
- **阶段没列出来。** 只有 `interceptors.stages` 里列出的阶段才会被调用。
- **`fileName` 没有效果。** 它只在任务首次运行时生效；找一找 `文件名改写已忽略` 这条记录。
- **`ephemeral` 被拒绝。** 只有 HTTP 任务能跳过探测；`totalBytes` 与 `rangeSupported` 只能和 `ephemeral: true` 一起使用。
- **某个请求头被拒绝。** 引擎自己管理的请求头（如 `Range`）不可设置，错误信息会指出是哪一个。
- **重启后改动消失。** 补丁从不落库，每次运行都会重新应用，所以插件每次都必须重新返回它们。

## 视图、命令与 `main`

`main` 和视图代码运行在 daemon 里的常驻上下文中；它们的错误会写进日志，视图错误还会显示在视图里。

| 现象 | 原因与解决 |
|---|---|
| 视图没有出现在活动栏 / Web 端 | 插件没加载或没启用；或者视图只有 `desktop` 实现——Web 端只显示可移植的视图（没有 `desktop` 入口，或带 `"portable": true`）。移动端不渲染插件视图。 |
| 视图顶部出现错误条，上一次的内容仍可见 | `render` 或 `event` 抛了异常，或返回了非法的树。错误条显示信息；下一次成功渲染后它会消失。来自 `main` 的信息有：`render() returned no node`（render 没有返回节点）、`view not registered: <id>`（`flux.ui.view` 里的 id 与 `contributes.views[].id` 不一致）、`command not registered: <id>`（manifest 里的命令没有对应的 `flux.commands.register`）、`unknown session: …`。 |
| `invalid view node root/…: …` | 节点树违反了某条规则；路径就是节点 id。典型情况：`a node must be an object {k, p, c}`（**字符串**子节点——把文字包进 `<text text="…"/>`）、`<kind> unknown prop "…"; allowed props: …`、`<kind> prop "…": expected string, got a number`（数字要用 `String()` 转换）、`<kind> missing required prop "…"`、`duplicate sibling id: …`（两个同级节点的 `key` 相同）、`the tree has more than 5000 nodes`、`nesting is deeper than the limit of 64 levels`。见[视图](/docs/zh/plugins/views/)。 |
| 第一次打开就失败，视图显示加载失败和**重新打开** | 第一次渲染就非法或抛了异常。修好，**重新加载**，点**重新打开**。 |
| 「视图会话已结束」和**重新打开** | 插件被禁用、被重新加载，或者它的 `main` 在失败（超时或内存超限）后被回收。重新打开即可。 |
| `flux.tasks` / `flux.auth` / `flux.ffmpeg` / `flux.ytdlp` 上出现 `TypeError` | 这些对象只有在声明了对应权限**并且已被批准**时才存在。声明权限、**重新加载**、**授权**。 |
| `tasks.create rate limited` | `flux.tasks.create` 每个插件每分钟最多调用 20 次。 |
| 点按钮没反应 | `ev.node` 里的节点 id 写错了——它是 `root/<key>/…`；先把 `ev.node` 打到日志里看一次真实的 id。 |
| 在输入框里打字会跳动或丢失 | 输入框必须是受控的：把文字存进状态，再作为 `value` 传回去。见[可移植界面指南](/docs/zh/plugins/ui-guide/)。 |
| 命令没有出现 | 它需要 `palette: true`（命令面板）或 `taskMenu: true`（任务右键菜单）、一个 `main` 脚本，以及与之对应的 `flux.commands.register`。 |
| Web 端提示仅桌面可用，或什么都没显示 | 原生 `desktop` 视图只在桌面应用里存在。 |

`main` 的调用是逐个串行的，每次调用的预算是墙钟 30 秒、CPU 2 秒、内存 64 MB；超出预算会结束该插件所有视图会话，并计入熔断。

## 无头服务器与 Docker

- 插件逻辑和它的日志都在**服务器**上。插件输出在 `$FLUXDOWN_DATA_DIR/logs/fluxdown_YYYY-MM-DD.log`（Docker 镜像里是 `/data/logs/`，所以挂载的目录或命名卷能把它保留下来）。在 Web 端用**设置 → 关于 → 导出日志**下载它们。
- Web 端的**从目录安装**填的是**服务器 / 容器内**的路径。把你的构建产物挂载进去，例如 `-v "$PWD/dist:/plugins-dev/mirror-demo:ro"`，然后填 `/plugins-dev/mirror-demo`。从浏览器上传 `.fxplug` 则不需要挂载。
- 开发循环是一样的：在 Web 端也可以点**重新加载**。
- 没有桌面会话时，`desktop` 视图无处显示；Web 端只显示可移植视图。
- `flux.fetch` 在服务器上执行，所以它用的是服务器的网络、代理设置和 DNS——而不是你浏览器的。

## 常见问题

**我改了 `resolver.js`，没有任何效果。** 是开发模式插件还是已安装的副本？已安装（zip / 市场）的插件在加载时就被缓存了：请重新安装。如果是指向 `dist/` 的开发模式插件，构建完成了吗？`dist/resolve.js` 里有你的改动吗？

**我改了 `manifest.json`，什么都没发生。** 点**重新加载**。用 SDK 时还要确认 `npm run dev` 或 `npm run build` 已经把它复制进了 `dist/`。

**重新加载后提示「重新加载失败：…」。** 后面的文字就是加载失败的原因（见[安装与校验报错](#安装与校验报错)）。开发模式登记仍然保留，改好后再次重新加载。

**重新加载后插件行显示需要授权。** manifest 现在请求了你尚未批准的权限或主机。批准它们；在此之前什么都不会运行。

**我的 `console.log` 在浏览器里看不到。** 插件代码运行在 daemon 里，不是在浏览器里。请看 daemon 日志或导出日志，见[日志](#日志)。

**`flux.settings.x` 是 `undefined`。** 这个 key 没有在 `manifest.settings` 里声明，或者该设置项既没有值也没有 `default`（只有 `required` 的设置项才会阻止调用）。

**提示 `setTimeout` / `URL` / `TextEncoder` 未定义。** 插件环境是没有浏览器 API 的 QuickJS。额外可用的只有 `atob`、`btoa`、`queueMicrotask` 和 `performance.now()`；在 `main` 里要用 `flux.timers`。TypeScript 项目能拦下大部分这类问题，因为 `lib` 不含 DOM。

**去哪里看插件做过什么？** 审计日志（**设置 → 扩展 → 插件 → 审计日志**，或在某个插件上点**审计记录**）列出授权、安装、请求、任务操作等——见[权限](/docs/zh/plugins/permissions/)。

**还是解决不了？** 导出日志（**设置 → 关于 → 导出日志**），反馈问题时附上插件的 identity 与版本、上表中的原始错误文本，以及它前后的 `[plugin:…]` 日志行。
