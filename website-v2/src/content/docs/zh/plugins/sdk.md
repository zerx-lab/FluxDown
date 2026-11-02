---
title: 插件 SDK 与 CLI
description: fluxdown-plugin-sdk 参考——create/build/validate/pack/dev 命令行、项目配置、运行时辅助函数、JSX 运行时、随包类型与 manifest 校验器 API。
section: plugins
order: 13
sourceHash: "05badbca13f5"
---

`fluxdown-plugin-sdk` 是用 TypeScript 编写 FluxDown 插件的工具链：带类型的 `flux.*`、用于[可移植视图](/docs/zh/plugins/views/)的 JSX 运行时、与引擎和插件索引同一套的 manifest 校验器，以及负责打包、校验、封装 `.fxplug` 的 CLI。

本页是参考手册。想从空文件夹一步步走到打好包的插件，请读 [TypeScript 项目](/docs/zh/plugins/typescript-project/)。你也完全可以不用 SDK——只含 `.js` 文件的文件夹同样可用（见[写第一个插件](/docs/zh/plugins/your-first-plugin/)）。

| | |
|---|---|
| npm 包名 | `fluxdown-plugin-sdk` |
| CLI 可执行文件 | `fluxdown-plugin` |
| 运行要求 | Node.js 20 或更高（`engines.node` 为 `>=20`）；也可以用 `bun` 作为包管理器 |
| 许可证 | AGPL-3.0-or-later |

`npx fluxdown-plugin-sdk create …` 无需安装即可运行该包的 CLI。在生成的项目里，CLI 以 `fluxdown-plugin` 的名字出现在 `npm run` 脚本的路径上。

## CLI

所有命令的调用格式一致：`fluxdown-plugin <命令> [dir] [--选项 值]…`。`dir` 缺省为当前目录。每个 `--选项` 必须恰好带一个值（`--sdk` 后面没有值会报 `--sdk needs a value`）。不带命令、`help` 或 `--help` 时打印用法；未知命令打印用法并以退出码 1 结束。任何失败都会打印错误信息并以退出码 1 退出。

| 命令 | 作用 |
|---|---|
| `fluxdown-plugin create <dir> [--identity a@b] [--name "Name"] [--sdk <path>]` | 创建项目骨架，见 [`create`](#create)。 |
| `fluxdown-plugin build [dir]` | 把所有入口打包到 `dist/`，复制 manifest 与资源，然后校验，见 [`build`](#build)。 |
| `fluxdown-plugin validate [dir]` | 用引擎规则校验已有的 `dist/`，输出风险等级与网络主机。**不会**构建。 |
| `fluxdown-plugin pack [dir]` | 先执行 `build`，再把 `dist/` 压缩为项目根目录下的 `<identity>-<version>.fxplug`。 |
| `fluxdown-plugin dev [dir]` | 监听源码并重建 `dist/`。 |

### `create`

目标目录已存在且非空时，`fluxdown-plugin create <dir>` 会报 `<绝对路径> is not empty`；目录不存在则自动创建。缺少 `<dir>` 参数会报 `create needs a target directory`。

| 选项 | 默认值 | 作用 |
|---|---|---|
| `--identity` | `me@` 加目录名的 slug | 写入 `manifest.json` 的 `identity`。slug 规则：转小写，把所有 `a-z0-9_-` 之外的连续字符替换为 `-`，去掉首尾的 `-`；结果为空则用 `plugin`。该选项的值不做校验——非法的 identity 会在 `build` 时被发现。 |
| `--name` | 目录名 | 写入 `manifest.json` 的 `name`。生成的视图标题取它的前 40 个字符（标题长度上限）。 |
| `--sdk <path>` | 已发布的版本 | 依赖写成 `file:<绝对路径>`（本地 SDK 仓库），而不是 `^<SDK 版本>`。用于 SDK 自身的开发。 |

生成的文件：

| 文件 | 内容 |
|---|---|
| `manifest.json` | `version` 为 `0.1.0`；description 为 `A FluxDown plugin built with fluxdown-plugin-sdk.`；`main: { "entry": "main.js" }`；一个 `activity` 视图 `dashboard`，标题为插件名；一个命令面板命令 `hello`，标题 `Say hello`。没有 `permissions`。 |
| `fluxdown-plugin.config.json` | `{ "entries": { "main.js": "src/main.tsx" }, "desktop": {}, "assets": [] }` |
| `package.json` | `name` 为目录名的 slug，`private: true`，`version` 为 `0.1.0`，`type: "module"`；依赖 `fluxdown-plugin-sdk`；开发依赖 `typescript` `^5.7.0`；脚本见下 |
| `tsconfig.json` | 见 [tsconfig 与 env.d.ts](#tsconfig-与-envdts) |
| `.gitignore` | `node_modules/`、`dist/`、`*.fxplug` |
| `src/env.d.ts` | `/// <reference types="fluxdown-plugin-sdk" />` |
| `src/main.tsx` | 一个 JSX 计数器视图 `dashboard`，外加 `hello` 命令的处理函数 |

`package.json` 的脚本：`build` = `fluxdown-plugin build`，`validate` = `fluxdown-plugin validate`，`pack` = `fluxdown-plugin pack`，`dev` = `fluxdown-plugin dev`，`typecheck` = `tsc --noEmit`。

项目一开始**没有 resolver、hook 和设置项**——这些要靠修改 `manifest.json`、配置文件和 `src/` 来添加，做法见 [TypeScript 项目](/docs/zh/plugins/typescript-project/)。这样扩展出来的项目大致如下：

```
my-plugin/
  manifest.json                    你的 manifest（唯一的事实来源，会复制进 dist/）
  fluxdown-plugin.config.json      要打包什么
  package.json
  tsconfig.json
  src/
    env.d.ts                       把全局 `flux` 引入作用域
    main.tsx                       常驻 main 脚本（视图、命令）
    resolve.ts                     resolver 入口        ← 由你添加
    hooks.ts                       hooks 入口           ← 由你添加
  dist/                            构建产物（已被 git 忽略）：实际被安装、被压缩的内容
```

### `build`

1. 读取 `manifest.json`（必须存在且是 JSON 对象）和 `fluxdown-plugin.config.json`。
2. **删除 `dist/`。**
3. 用 esbuild 打包 `entries` 与 `desktop` 的每一项（每个入口单独构建；遇到第一个错误就中止，并输出格式化后的 esbuild 信息）。
4. 复制 `manifest.json`、每个 `assets` 文件，以及 manifest 里点名但不是构建产物的文件（见[配置文件](#fluxdown-pluginconfigjson)）。
5. 以严格模式校验 `dist/`（见[限制与校验](#限制与校验)）。失败时打印 `validation failed:`，随后每个问题一行 `  - <错误>`，并以退出码 1 结束；成功时打印 `built dist`。

由于第 2 步会删掉整个目录，以 `dist/` 登记的开发模式插件在重建期间会暂时丢失文件。构建之后请点一次**重新加载**，让 FluxDown 重新读取全部内容（见[调试与排错](/docs/zh/plugins/debugging/#开发循环)）。

### `validate`

没有 `dist/` 时报 ``dist/ not found; run `fluxdown-plugin build` first``。否则对 `dist/` 执行[限制与校验](#限制与校验)里的检查，成功时打印 `valid (risk: <standard|elevated|full>)`；视图声明了 `desktop.networkHosts` 时，后面追加 `, network hosts: a, b`。

### `pack`

先执行 `build`，再把 `dist/` 里的每个文件写入 `<项目>/<identity>-<version>.fxplug`，例如 `me@my-plugin-0.1.0.fxplug`。manifest 位于压缩包根目录。压缩结果是确定性的：文件按排序顺序加入，修改时间固定为 2020-01-01（UTC），deflate 级别 6，所以相同的源码总是得到相同的字节。完成后打印 `packed <绝对路径>`。

### `dev`

启动时删除一次 `dist/`，然后为每个入口保持 esbuild 监听。每次重建成功后（防抖 100 毫秒）重新复制 `manifest.json`、资源和被引用文件，并再次校验，打印 `rebuilt, valid` 或 `validation failed:` 清单；打包出错时打印 esbuild 的信息，并保留上一次的有效产物。修改 `manifest.json`、`assets` 和被引用文件也会触发同样的步骤。配置文件只在 `dev` 启动时读取一次——改了 `fluxdown-plugin.config.json` 要重启它。按 Ctrl+C 停止。

`dev` 只负责重建文件，不会通知 FluxDown。开发模式插件的 resolver、hooks、auth 和订阅脚本每次调用都会重新读取，所以重建就够了；改了 manifest 或 `main` 则需要在插件行上点**重新加载**。

## `fluxdown-plugin.config.json`

```json
{
  "entries": { "main.js": "src/main.tsx", "resolve.js": "src/resolve.ts" },
  "desktop": { "ui/dashboard.js": "src/desktop/dashboard.ts" },
  "assets": ["rules.json"]
}
```

三个键都是可选的（文件不存在时等同于全部为空，此时什么都不会构建，只复制 manifest）。出现其他键会报 `fluxdown-plugin.config.json: unknown key '<key>'`；文件不是对象会报 `fluxdown-plugin.config.json: must be a JSON object`。`entries` 和 `desktop` 必须是「输出路径 → 源文件路径」的对象；每个输出路径必须是安全的相对路径（不含 `..`、绝对路径、盘符或空段）；`assets` 必须是安全相对路径组成的数组。

| 键 | 含义 |
|---|---|
| `entries` | 经典脚本，`dist 内的输出路径 → 源文件`。用 esbuild 打成单个 **IIFE**：`bundle: true`、平台 `browser`、目标 ES2022、自动 JSX 转换，`jsxImportSource` 为 `fluxdown-plugin-sdk/ui`。产物里不会残留任何 `import` / `export`，因为引擎把这些文件当经典脚本求值。用于 `main`、resolver、hooks、`auth` 和订阅。输出路径必须与 manifest 里的 `entry` 一致。 |
| `desktop` | 桌面界面脚本（`contributes.views[].desktop.entry`），打包为 **ES 模块**。宿主模块 `gpui-kit`、`gpui`、`gpui-base`、`gpui-component`、`gpui-shell`、`gpui-fps`、`fluxdown`，桌面运行时提供的 Node 风格内置模块（`buffer`、`console`、`crypto`、`fs/promises`、`net`、`os`、`path`、`process`、`url`、`websocket`、`zlib`），以及 `node:*` 形式的说明符，都保留为裸 import。见[桌面原生界面指南](/docs/zh/plugins/desktop-ui-guide/)。 |
| `assets` | 额外的**文件**，按相同的相对路径原样复制进 `dist/`（例如嗅探规则包或图标）。 |

除了 `assets`，`build` 还会自动从项目根目录复制 manifest 里点名、且不是构建入口产物的文件：resolver、hooks、auth、订阅、`main` 与 `desktop` 的入口、`icon` 以及 `contributes.sniffRules`。因此项目里可以把手写的 `.js` 文件和打包产物混用。被点名但不存在的文件不会被复制，随后 `validate` 会报告它缺失。

### tsconfig 与 `env.d.ts`

生成的 `tsconfig.json`：

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "lib": ["ES2022"],
    "types": [],
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true,
    "jsx": "react-jsx",
    "jsxImportSource": "fluxdown-plugin-sdk/ui"
  },
  "include": ["src"]
}
```

`src/env.d.ts`（`/// <reference types="fluxdown-plugin-sdk" />`）把全局 `flux` 和 `console` 引入作用域。不要启用 DOM lib：QuickJS 宿主没有 DOM，也没有 `URL`、`setTimeout`——不开 DOM lib，类型检查就能挡住运行时会失败的代码。`tsc` 只做类型检查（`noEmit`），产物由 esbuild 生成。

## 限制与校验

`build`、`pack` 和 `validate` 都会对 `dist/` 执行同样的检查：

| 检查 | 失败信息 |
|---|---|
| 存在 `dist/manifest.json` | `dist/manifest.json missing` |
| manifest 通过 `validateManifest(…, { strict: true })` | 每个问题一行，与引擎一样是中文（见[调试与排错](/docs/zh/plugins/debugging/#安装与校验报错)） |
| `dist/` 中最多 **200 个文件** | `package has N files; the limit is 200` |
| 总大小最多 **50 MB**（52,428,800 字节） | `package is N bytes; the limit is 52428800` |
| 声明的 `contributes.sniffRules` 文件是合法规则包（≤ 256 KiB，`format` 为 `fluxdown-sniff-pack`，≤ 500 条规则） | `validateSniffPack` 的信息 |

引擎安装 zip 时同样限制 200 个条目和 50 MB（解压后）。插件市场另外把单个包限制在 10 MB 以内，见[打包与市场](/docs/zh/plugins/packaging/)。

严格模式（CLI 里始终开启）比应用本身多两条要求，插件索引也同样强制：`version` 与 `minAppVersion` 必须是纯粹的三段数字 `主版本.次版本.修订号`（不带 `v` 前缀，不带预发布或构建后缀，不含前导零）；以及每个 resolver、hooks、auth、订阅入口和 `icon` 都必须存在于包内。（应用在安装时只要求 `main` 入口、`desktop` 入口、视图图标、`sniffRules` 文件、页面脚本文件和 `contributes.archive` 文件存在。）

## 入口辅助函数

引擎在求值脚本之后会读取固定的全局名：`resolve`、`onStart` / `onError` / `onDone` / `onMetaProbed` / `onCancel`、`authenticate`、`subscribe`、`intercept` 和 `archive`。`fluxdown-plugin-sdk/runtime` 里的辅助函数会（用 `Reflect.set(globalThis, …)`）把你的函数赋给这些全局名，所以每个入口文件都可以写成普通模块。每个辅助函数都会原样返回你传入的函数。

| 辅助函数 | 安装的全局名 | 使引擎调用它的 manifest 字段 |
|---|---|---|
| `defineResolver(fn: ResolveFn)` | `globalThis.resolve` | `resolvers[].entry` |
| `defineHooks(handlers: HookHandlers)` | `handlers` 中每个值为函数的属性，以同名全局安装：`onStart`、`onError`、`onDone`、`onMetaProbed`、`onCancel` | `hooks.entry`；触发哪些事件由 `hooks.events` 决定 |
| `defineInterceptors(handlers: InterceptHandlers)` | `globalThis.intercept`，按 `ctx.stage` 分派到 `handlers` 中同名阶段的处理函数（`prepare`、`probed`、`refresh`、`selectVariant`、`selectPeriod`、`segments`、`liveSegments`、`finalize`、`failed`）；没有对应处理函数时返回 `undefined` | `interceptors.entry`（需要 `intercept` 权限；订阅哪些阶段由 `interceptors.stages` 决定） |
| `defineAuth(fn: AuthFn)` | `globalThis.authenticate` | `auth.entry`（需要 `auth` 权限） |
| `defineSubscription(fn: SubscribeFn)` | `globalThis.subscribe` | `subscriptions[].entry` |
| `defineArchive(fn: ArchiveFn)` | `globalThis.archive`；`fn` 接收 `ctx: ArchiveContext`（`{ fileName }`），同步或异步返回 `ArchiveResult`（`{ folderName }`）、`null` 或 `undefined` | `contributes.archive`（需要 `archive` 权限）；脚本里只有 `flux.settings`、`flux.logger` 和 `console` |
| `defineMain(setup: (flux: MainFlux) => void)` | 不安装任何全局名——它**立即同步地**用 `main` 脚本的门面（`MainFlux`）调用 `setup(flux)`，因此视图、命令、rpc 处理器、事件和定时器都在脚本求值期间完成注册 | `main.entry` |

```ts
import { defineResolver, defineHooks } from "fluxdown-plugin-sdk/runtime";

defineResolver(async (ctx) => {
  const res = await flux.fetch({ url: ctx.url });
  return { url: JSON.parse(res.body).direct };
});

defineHooks({
  onDone: (ev) => flux.logger.info("finished", ev.filePath),
});
```

注意：

- 只在脚本顶层调用辅助函数，顶层的其他代码也不要碰 `flux`（安装时的检查会在空白上下文里求值 resolver、hooks、auth、订阅脚本）。一个插件最多一个 resolver 和一个订阅 provider；对同一个辅助函数调用两次，后者直接覆盖全局名。
- `defineMain` 是在常驻 `main` 脚本里做带类型注册的方式，生成的模板就是这么写的。`ui`、`commands`、`rpc`、`events`、`timers` 和 `tasks` 只存在于 `main` 中，所以由所有入口脚本共用的全局 `flux` 类型不声明它们，`MainFlux` 才声明（`tasks` 可选：需要 `tasks` 权限）。不用 `defineMain` 的话，就得自己把全局 `flux` 强转成 `MainFlux`。
- 运行时的值请从 `fluxdown-plugin-sdk/runtime` 导入，包根只导入**类型**（`import type { ViewEvent } from "fluxdown-plugin-sdk"`，构建时会被擦除）。包根模块同时重新导出了 manifest 校验器和运行时辅助函数；值的导入都走专用子路径，产物更小也更可预期。

## JSX

按上文设置 `jsxImportSource` 后，每个小写标签都是一个[视图节点](/docs/zh/plugins/views/#节点参考)，类型取自 daemon 校验所用的同一份 schema：

```tsx
import { defineMain } from "fluxdown-plugin-sdk/runtime";

defineMain((flux) => {
  flux.ui.view("dashboard", {
    render: () => (
      <stack gap="md" padding="md">
        <heading text="Hello" />
        <button key="go" text="Go" variant="primary" />
      </stack>
    ),
    event: (_state, ev) => { if (ev.node === "root/go") flux.ui.toast("info", "Clicked"); },
  });
});
```

| 规则 | 说明 |
|---|---|
| 输出 | 恰好是 `flux.ui.h` 返回的 `{k, p, c}` 结构 |
| `key` | 成为 `p.key`（它不是普通 prop），并决定节点 id |
| 被丢弃的子节点 | `null`、`false`、`undefined`；嵌套数组会被展平 |
| 值为 `undefined` 的 prop | 不写入 `p` |
| **不允许字符串或数字子节点** | 子节点类型是 `ViewNode`，所以 `<stack>hello</stack>` 过不了类型检查；运行时字符串子节点会被视图校验器拒绝，报 `a node must be an object {k, p, c}`。文字请放进节点：`<text text="hello" />`。 |
| Fragment | `<>…</>` 把子节点展开到父节点里。它不能作为视图的**根**：`render` 必须返回一个真正的节点。 |
| 函数组件 | `function Row(props) { return <stack>…</stack>; }` 可用：它接收 props（含 `children` 和 `key`），必须返回一个节点。 |
| 另外导出 | `jsxs`（与 `jsx` 相同）和 `jsx-dev-runtime`（用于 `"jsx": "react-jsxdev"`） |

## 随包类型

- `flux` 及其子接口（`Flux`、`FluxFetchOptions` 等）、入口上下文与返回值（`ResolveContext`、`ResolveResult`、`AuthContext`、`AuthResult`、`SubscribeContext`、`ArchiveContext`、`ArchiveResult`、`HookHandlers` 等）、`ViewEvent`、`ViewContext`、`ViewDefinition`、`FluxEvent`，以及全部视图类型与 JSX 内置元素——来自 `fluxdown-plugin-sdk`。
- `ThemeTokenPath`——`fluxdown.theme().tokens` 能解析的每一个主题 token 路径，由主题注册表生成。
- `fluxdown-plugin-sdk/types/desktop`——桌面界面脚本（`gpui-kit` 等）的类型声明，由桌面运行时生成。

## 包导出

| 导入路径 | 内容 |
|---|---|
| `fluxdown-plugin-sdk` | 类型（`flux`、视图节点、入口上下文、主题 token），以及对 manifest API 和运行时辅助函数的重新导出——插件代码里只从这里导入**类型**。导入它会把全局 `flux` / `console` 的声明带进作用域。 |
| `fluxdown-plugin-sdk/manifest` | 零依赖的校验器，见下文。 |
| `fluxdown-plugin-sdk/runtime` | `defineResolver`、`defineHooks`、`defineAuth`、`defineSubscription`、`defineMain`。 |
| `fluxdown-plugin-sdk/ui/jsx-runtime`、`…/ui/jsx-dev-runtime` | JSX 运行时；设置 `"jsxImportSource": "fluxdown-plugin-sdk/ui"`。 |
| `fluxdown-plugin-sdk/types/desktop` | 仅类型：桌面界面脚本的声明。 |

## 校验器 API

manifest 校验器是一个零依赖模块（插件索引引入的是同一个文件）：

```ts
import { validateManifest, riskOf, networkHostsOf, validateSniffPack } from "fluxdown-plugin-sdk/manifest";

const manifest = JSON.parse(await readFile("manifest.json", "utf8"));
const files = new Set(["manifest.json", "main.js", "resolve.js"]); // 包内的相对路径，用正斜杠

const { ok, errors } = validateManifest(manifest, files, { strict: true });
riskOf(manifest);          // "standard" | "elevated" | "full"
networkHostsOf(manifest);  // 所有视图的 desktop.networkHosts 的去重并集，已排序
```

| 导出 | 签名 / 取值 |
|---|---|
| `validateManifest(manifest, files, options?)` | `manifest: unknown`，`files?: Set<string>`（包内相对路径，用 `/`；省略时跳过文件存在性检查），`options.strict?: boolean`（strict 模式需要 `files`；缺省时只返回一条要求提供 `files` 的错误，不会抛异常）。返回 `{ ok: boolean; errors: string[] }`，包含发现的**全部**错误。信息为中文，与引擎一致。 |
| `validateSniffPack(content)` | `content: string \| Uint8Array`；返回 `{ ok, errors }`。 |
| `riskOf(manifest)` | `permissions` 含 `desktop` 或 `pageScripts` 时为 `"full"`；含 `ffmpeg`、`ytdlp`、`auth`、`tasks`、`browserRules`、`background`、`intercept`、`archive` 中任意一个时为 `"elevated"`；否则为 `"standard"`。 |
| `networkHostsOf(manifest)` | `contributes.views[].desktop.networkHosts` 的去重并集，已排序。 |
| `packageFilesOf(manifest)` | 安装时必须存在的文件：`main.entry`、每个 `desktop.entry`、每个视图 `icon`、`sniffRules`、每个 `pageScripts[].js` 文件以及 `contributes.archive`。 |
| `isSafeRelativePath`、`isValidIdentity`、`isValidNetworkHost` | 校验器内部使用的判定函数。 |
| `VALID_PERMISSIONS`、`VALID_EVENTS`、`VIEW_LOCATIONS` | 封闭的取值列表。 |
| `WINDOW_SIZE_MIN`/`MAX`（240 / 4096）、`NETWORK_HOSTS_MAX`（64）、`VIEWS_MAX`（16）、`COMMANDS_MAX`（32）、`TITLE_MAX_CHARS`（40）、`SNIFF_PACK_MAX_BYTES`（256 KiB）、`SNIFF_PACK_MAX_RULES`（500）、`SNIFF_PACK_FORMAT` | 校验所用的上限常量。 |

校验器与引擎遵循同一套规则，并用共享的测试用例互相核对。措辞上有一处差别：manifest 出现未知字段时，SDK 报 `manifest: 未知字段 <key>`，而引擎在解析 JSON 时就拒绝未知字段，报的是 `JSON 解析失败: unknown field …`。
