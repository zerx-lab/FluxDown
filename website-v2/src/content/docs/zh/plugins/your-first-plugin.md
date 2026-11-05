---
title: 写第一个插件
description: 用纯 JavaScript 一步一步写出、装上并调试一个能用的 URL 解析插件。
section: plugins
order: 2
sourceHash: "bd6dc9e72156"
---

这篇教程只用一个文本编辑器，从零写一个 resolver 插件：拦截一个虚构网盘的分享链接，调用它的 API 拿到真实下载直链，再交给 FluxDown 下载。走完一遍，你就掌握了完整的「改代码 → 测试」循环。

如果你想要类型提示、多个源文件和界面，请改走 [TypeScript 项目](/docs/zh/plugins/typescript-project/)教程，两者的概念完全一致。如何选择见[概览](/docs/zh/plugins/overview/)。

## 1. 建文件夹

插件就是一个文件夹，在磁盘任意位置新建即可：

```
my-resolver/
├── manifest.json
└── resolver.js
```

## 2. 写 manifest

`manifest.json` 声明你是谁、要处理哪些 URL：

```json
{
  "identity": "yourname@my-resolver",
  "name": "Example Host Resolver",
  "version": "1.0.0",
  "description": "Resolves example-files.com share links to direct downloads.",
  "resolvers": [
    {
      "match": { "urls": ["*://example-files.com/share/*"] },
      "entry": "resolver.js"
    }
  ]
}
```

字段说明（完整列表见 [Manifest 参考](/docs/zh/plugins/manifest/)）：

- `identity` 必须是 `作者@名称` 的形式，只能用小写字母、数字、`_` 和 `-`，不能含点。它是插件的永久 ID——设置和存储都以它为键。
- `version` 是纯粹的 `主版本.次版本.修订号`。
- `match.urls` 只有 `*` 一种通配符，对整个 URL 做不区分大小写的匹配。`"*://example-files.com/share/*"` 同时匹配该路径下的 `http` 和 `https` 链接。
- `entry` 是相对插件文件夹的路径。`..`、绝对路径和盘符都会被拒绝。

## 3. 写 resolver

`resolver.js` 必须定义一个名为 `resolve` 的全局函数。顶层的 `function` 声明就够了——脚本按经典脚本加载，函数会自动落到 `globalThis` 上：

```js
async function resolve(ctx) {
  // ctx.url 是任务的原始 URL，例如 https://example-files.com/share/abc123
  const id = ctx.url.split("/share/")[1];
  if (!id) return null; // null/undefined = 放行，FluxDown 按 ctx.url 原样下载

  // 调用网盘 API 获取真实链接。
  const res = await flux.fetch({
    url: "https://example-files.com/api/file/" + id,
    headers: { "Referer": ctx.url },
  });
  if (res.status !== 200) {
    throw new Error("API returned " + res.status);
  }

  const data = JSON.parse(res.body);
  flux.logger.info("resolved", ctx.url, "->", data.directUrl);

  return {
    url: data.directUrl,        // 必填：直链
    fileName: data.name,        // 可选：覆盖文件名
    totalBytes: data.size,      // 可选：文件大小（API 提供时）
    ephemeral: true,            // 链接带签名、会过期 -> 跳过元数据探测
    rangeSupported: true,       // 站点支持 Range -> 保持多段下载
  };
}
```

各部分含义：

- `ctx` 带有 `taskId`、`url`、`cookies`、`referrer`、`userAgent`、`extraHeaders`——任务知道的一切（另有 `authRef` 和 `resolverItem`，见 [API 参考](/docs/zh/plugins/api-reference/)）。
- 返回 `null` 或 `undefined` 表示「不是我的，按原始 URL 下载」。
- 抛出错误会让任务进入错误状态（fail-closed）。这对 resolver 是正确行为：宁可明确失败，也不要把一个 HTML 页面存成 `.mp4`。
- `ephemeral: true` 告诉 FluxDown 这条链接是一次性或防盗链的，因此跳过可能把链接「烧掉」的额外 HEAD 探测。如果站点的链接很稳定，就不要加——探测能带来更好的续传完整性校验（ETag）。
- `rangeSupported: true` 承诺站点支持 HTTP Range 请求。没有探测时 FluxDown 会保守地先开单连接；有了这个承诺，就会直接规划多段下载。只在确认站点支持 Range 时才声明——错误的承诺可能浪费有配额的链接。
- `flux.*` 调用要放在 `resolve` **里面**。安装插件时，FluxDown 会在一个空白上下文（没有 `flux`）里把脚本求值一次，只为检查能否编译，所以顶层的 `flux.logger.info(...)` 会让安装失败。

## 4. 以开发模式安装

在桌面应用里：**设置 → 扩展 → 插件**，打开**开发模式**开关（默认关闭，打开后才会出现安装区），在**从目录安装（开发模式）**中选择 `my-resolver/`，点**安装**。在 Web 端，同一个输入框填的是**运行 FluxDown 的那台机器**上的路径，而不是浏览器所在机器的路径。

开发模式记录的是你的文件夹路径，而不是拷贝一份。插件行会出现「开发模式」徽标和**重新加载**按钮。这个插件没有声明任何权限，所以安装后立即启用。（声明了权限的插件一开始是**需要授权**状态：应用会弹出授权对话框，在你授权前启用开关一直不可用——见[权限](/docs/zh/plugins/permissions/)。）

之后的修改循环取决于你改了什么：

| 你改了 | 怎么做 |
|---|---|
| `resolver.js`（以及 hooks、auth、订阅脚本） | 保存，然后新建或恢复一个匹配的下载即可。这些文件**每次调用都会重新读取**——不用重装，也不用重新加载。 |
| `manifest.json`（匹配规则、版本、设置项、权限、入口路径） | 点插件行上的**重新加载**。它会重读 manifest 和全部脚本，重新执行安装时的检查，出错时显示错误信息——开发模式登记会保留，你修好后可以再次重新加载。 |
| 常驻的 `main` 脚本（视图、命令，见[可移植界面指南](/docs/zh/plugins/ui-guide/)） | 点**重新加载**。与 resolver 不同，`main` 在调用之间持续运行，不会每次调用都重新读取。 |

## 5. 测试与调试

新建一个 URL 能匹配你规则的下载，观察结果：

- **成功**——任务从解析后的直链下载，任务详情里显示解析后的状态。
- **脚本抛异常或超时**——任务显示形如 `[插件] yourname@my-resolver: 插件运行时错误: API returned 404` 的错误（`[插件]` 是插件标记）。在失败的任务上右键选择**忽略插件重试**，即可绕过你的插件，按原始 URL 重新下载。
- **日志**——`flux.logger.*` 和 `console.log` 都写进 FluxDown 后台服务的日志，每行带 `[plugin:yourname@my-resolver]` 标记，单行超过 4 KB 会被截断。在日志文件里搜索 `[plugin:` 即可。日志位于 FluxDown 数据目录下的 `logs/`：

  | 系统 | 日志目录 |
  |---|---|
  | Linux | `$XDG_DATA_HOME/fluxdown/logs/`（默认 `~/.local/share/fluxdown/logs/`） |
  | macOS | `~/Library/Application Support/fluxdown/logs/` |
  | Windows 安装版 | `%LOCALAPPDATA%\FluxDown\logs\` |
  | Windows 便携版 | `<程序目录>\portable_data\logs\` |
  | 设置了 `FLUXDOWN_DATA_DIR` 的任何系统（服务器、Docker） | `$FLUXDOWN_DATA_DIR/logs/` |

  文件名形如 `fluxdown_YYYY-MM-DD.log`。最省事的办法是**设置 → 关于 → 导出日志**，它会把 daemon 日志打成 zip。注意菜单里的**打开日志目录**打开的是桌面应用自己的诊断日志，不是插件输出。更多见[调试与排错](/docs/zh/plugins/debugging/)。

第一次运行常见的错误：

- **完全没有反应** → `match.urls` 没有匹配上。规则是对完整 URL 匹配的，且首段与尾段都有锚定：`example-files.com/*` 匹配不了 `https://example-files.com/...`，因为第一个 `*` 之前的部分必须是 URL 的前缀——应写成 `*://example-files.com/*`。
- **`flux.fetch` 拒绝了这个 URL** → 只允许访问公网可路由地址的 `http`/`https`。对 `localhost`、局域网或云元数据 IP 的请求会按设计失败，报 `插件输出非法: blocked: non-routable IP`。
- **安装时报 `脚本编译失败: …`** → 有语法错误，或者顶层代码用到了 `flux`。改好脚本后重新安装；如果插件已经装上，则点**重新加载**。
- **插件被自动禁用** → resolver 连续 3 次超时或内存超限会触发熔断。修好脚本，再到设置里重新启用。

在真实站点还没准备好时，想先验证整条链路，可以临时让 resolver 返回一个固定链接，然后新建一个 `https://example-files.com/share/test` 的下载：

```js
async function resolve(ctx) {
  flux.logger.info("resolve called for", ctx.url);
  return { url: "https://proof.ovh.net/files/1Mb.dat", fileName: "test.dat" };
}
```

日志里会出现 `[plugin:yourname@my-resolver] resolve called for https://example-files.com/share/test`，任务会下载那个测试文件。（原站点不需要真实存在：resolver 在连接原站点之前就已运行。）

## 6. 添加设置项（可选）

假设站点需要 API 令牌。在 manifest 里声明它：

```json
{
  "settings": [
    {
      "key": "apiToken",
      "title": "API token",
      "description": "Get one at example-files.com/account.",
      "type": "string",
      "widget": "password",
      "required": true
    }
  ]
}
```

点**重新加载**后，应用会自动为你的插件渲染一个设置表单（插件行上的齿轮按钮）。在脚本里用 `flux.settings.apiToken` 读取——已是字符串类型（数字和布尔值也会以真实的 JS 类型传入）：

```js
const res = await flux.fetch({
  url: "https://example-files.com/api/file/" + id,
  headers: { "Authorization": "Bearer " + flux.settings.apiToken },
});
```

由于该字段是 `required`，在令牌填好之前开始的下载不会调用你的脚本，而是直接失败，报 `缺少必填设置项: 插件 yourname@my-resolver 需先配置「API token」`（意为：插件需要先配置「API token」）。

## 7. 发布

把文件夹打成 `.fxplug` 分享给别人，或发布到插件市场——见[打包与市场](/docs/zh/plugins/packaging/)。

## 下一步

- [TypeScript 项目](/docs/zh/plugins/typescript-project/)——同样的插件，用 SDK 写成带类型的版本，并加上 hook、视图和设置。
- [事件钩子指南](/docs/zh/plugins/hooks-guide/)——响应任务事件，对完成的文件做后处理。
- [可移植界面指南](/docs/zh/plugins/ui-guide/)——用常驻 `main` 脚本实现页面、命令与设置页签。
- [调试与排错](/docs/zh/plugins/debugging/)——日志、重新加载规则与报错信息对照表。
