---
title: 写第一个插件
description: 一步一步写出、装上并调试一个能用的 URL 解析插件。
section: plugins
order: 2
sourceHash: "2cd0573ea925"
---

这篇教程从零写一个 resolver 插件：拦截一个虚构网盘的分享链接，调它的 API 拿到真实下载直链，交给 FluxDown 下载。走完一遍，你就掌握了完整的「改代码 → 测试」循环。

## 1. 建文件夹

插件就是一个文件夹，在硬盘任意位置新建：

```
my-resolver/
├── manifest.json
└── resolver.js
```

## 2. 写 manifest

`manifest.json` 声明你是谁、要接管哪些 URL：

```json
{
  "identity": "my-resolver@yourname",
  "name": "Example Host Resolver",
  "version": "1.0.0",
  "description": "把 example-files.com 的分享链接解析成直链。",
  "resolvers": [
    {
      "match": { "urls": ["*://example-files.com/share/*"] },
      "entry": "resolver.js"
    }
  ]
}
```

字段说明（完整列表见 [Manifest 参考](/docs/zh/plugins/manifest/)）：

- `identity` 格式是 `名字@作者`，只允许小写字母、数字、`_` 和 `-`，禁止点号。它是永久 ID——设置和存储都挂在它下面。
- `version` 就是 `主.次.补丁` 三段数字。
- `match.urls` 里 `*` 是唯一的通配符，匹配不区分大小写。`"*://example-files.com/share/*"` 同时匹配 http 和 https。
- `entry` 是相对插件文件夹的路径，禁止 `..`、绝对路径和盘符。

## 3. 写 resolver

`resolver.js` 必须定义一个名为 `resolve` 的全局函数。顶层 `function` 声明就够了——脚本按 classic script 加载，顶层声明自动挂到 `globalThis`：

```js
async function resolve(ctx) {
  // ctx.url 是任务的原始 URL，例如 https://example-files.com/share/abc123
  const id = ctx.url.split("/share/")[1];
  if (!id) return null; // 返回 null/undefined = 放行，FluxDown 按原 URL 下载

  // 调网盘的 API 拿真实直链。
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
    url: data.directUrl,        // 必填：下载直链
    fileName: data.name,        // 可选：覆盖文件名
    totalBytes: data.size,      // 可选：文件大小（API 告诉你的话）
    ephemeral: true,            // 直链是签名/会过期的 -> 跳过元数据探测
    rangeSupported: true,       // 服务支持 Range -> 保留多线程分段下载
  };
}
```

逐项解释：

- `ctx` 带着 `taskId`、`url`、`cookies`、`referrer`、`userAgent`、`extraHeaders`——任务知道的信息全在这。
- 返回 `null` 或 `undefined` 表示「不归我管，按原始 URL 下载」。
- 抛异常会让任务进入错误状态（fail-closed）。对 resolver 来说这是正确行为：宁可明着失败，也别把 HTML 页面存成 `.mp4`。
- `ephemeral: true` 告诉 FluxDown 这条直链是一次性的或有防盗链，跳过额外的 HEAD 探测以免把链接「用掉」。如果你的网盘直链是稳定的就别加——探测能拿到 ETag，续传校验更可靠。
- `rangeSupported: true` 是对「服务支持 HTTP Range 请求」的担保。没有探测时 FluxDown 默认保守地从单连接起步；有了担保则直接按多线程分段规划。只在确认服务支持 Range 时声明——虚假担保会把有请求次数配额的直链白白烧掉。

## 4. 用开发模式安装

在桌面应用里：**设置 → 扩展 → 插件 → 从目录安装**，选中 `my-resolver/`，保持**开发模式**开关打开。

开发模式只记录文件夹路径、不拷贝，并且**每次调用都重新读** `resolver.js`。你的循环变成：

1. 改 `resolver.js`，存盘。
2. 在 FluxDown 里添加（或恢复）一个匹配的下载。
3. 看结果——不用重装，不用重启。

只有 `manifest.json` 的改动需要重新加载：在**设置 → 扩展 → 插件**页把插件关掉再打开。

## 5. 测试与调试

添加一个 URL 匹配你 pattern 的下载，观察结果：

- **成功**——任务从解析后的直链下载。
- **脚本抛异常 / 超时**——任务显示带插件前缀的错误信息。在失败任务上右键有「忽略插件重试」逃生舱，确认后按原始 URL 重新下载、不经过你的插件。
- **日志**——`flux.logger.*` 和 `console.log` 都写进 FluxDown 的日志文件（Windows 在应用同级的 `logs/fluxdown_YYYY-MM-DD.log`，Linux 在 `~/.local/share/fluxdown/logs/`）。每条日志截断在 4 KB。

新手常见问题：

- **完全没反应** → `match.urls` 没匹配上。pattern 是对完整 URL 比较的；`example-files.com/*` 匹配不了 `https://example-files.com/...`，因为首段是前缀锚定的——要写 `*://example-files.com/*`。
- **`flux.fetch` 拒绝了 URL** → 只允许 `http`/`https` 访问公网地址。访问 `localhost`、局域网、云元数据 IP 都会被拦截，这是有意的。
- **插件被自动禁用了** → 连续 3 次超时或内存超限会触发熔断。修好脚本后在设置里重新启用。

## 6. 加个设置项（可选）

假设网盘需要 API token，在 manifest 里声明：

```json
{
  "settings": [
    {
      "key": "apiToken",
      "title": "API token",
      "description": "在 example-files.com/account 获取。",
      "type": "string",
      "widget": "password",
      "required": true
    }
  ]
}
```

应用会自动为你的插件生成设置表单。脚本里用 `flux.settings.apiToken` 读取——已经是字符串类型（number 和 boolean 类型的设置项同样以真实 JS 类型到达）。

## 7. 发布

把文件夹打成 `.fxplug` 分享出去，或发布到插件市场——见[打包与市场](/docs/zh/plugins/packaging/)。
