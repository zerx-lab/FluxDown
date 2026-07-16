---
title: 插件系统概览
description: FluxDown 插件是什么、能做什么、怎么运行、怎么安装。
section: plugins
order: 1
sourceHash: "31bae6e83229"
---

FluxDown 插件是挂进下载流程的小段 JavaScript 程序。一个插件就是一个文件夹：一份 `manifest.json` 加一两个 `.js` 文件——不需要构建工具，不需要 npm，不需要任何框架。

插件只能做两类事：

1. **解析 URL（resolver）**——在下载开始前改写任务的 URL。典型用途：把一个分享页链接（视频页、网盘页）变成真正的直链。在 manifest 的 `resolvers` 里声明，脚本导出一个全局 `resolve(ctx)` 函数。
2. **响应任务事件（hooks）**——任务开始、完成、出错、探测到元数据时收到通知。在 `hooks` 里声明，脚本导出全局的 `onStart` / `onDone` / `onError` / `onMetaProbed` 函数。

插件**不能**创建任务、不能读任意文件、不能操作界面。它和外界打交道只有 `flux.*` 这一套接口（HTTP 请求、键值存储、日志、请求重试），见 [API 参考](/docs/zh/plugins/api-reference/)。

## resolver 怎么运行

写 resolver 之前，有三条设计性质需要先知道：

- **惰性。** FluxDown 只在任务上记录你的插件 ID，从不保存解析结果。`resolve(ctx)` 在**每次**开始和恢复时都会重新执行。这是有意的：网盘直链通常会过期，恢复时重新解析才能让旧任务继续下载。
- **不在主循环里跑。** 解析在专用线程池上执行，脚本再慢、再卡也不会冻结应用。
- **失败即失败（fail-closed）。** 脚本抛异常、超时、返回值不合法，任务都会进入错误状态，错误信息带 `[插件]` 前缀。FluxDown 绝不会退回去下载原始页面 URL——那会把一个 HTML 页面当视频文件存进硬盘。用户可以在失败任务上用「忽略插件重试」显式绕过出问题的插件。

多个启用中的插件同时匹配一个 URL 时，`identity` 字典序最小的那个生效。

惰性解析有一个附带结果：带 resolver 的任务会跳过常规的元数据探测，所以同一个插件如果又订阅了 `onMetaProbed`，这个钩子对它自己的任务永远不会触发（加载时日志里会有一条警告）。

## hooks 怎么运行

hooks 是纯通知，发出后不管结果：

- 钩子抛异常或超时，只记日志然后忽略，永远影响不了任务。
- 插件运行时忙不过来时，通知直接丢弃，不排队。
- 钩子影响任务的唯一途径是 `flux.task.requestRetry({ delayMs })`，且只在 `onError` 里有效。

## 执行模型与限制

每次调用都在一个**全新的 JavaScript 上下文**（QuickJS）里执行。调用之间不保留任何变量——需要持久化就用 `flux.storage`。脚本按 classic script 方式加载（不是 ES 模块），入口函数要挂在 `globalThis` 上（顶层的 `function resolve(ctx) {...}` 声明天然就是）。

| 限制 | 值 |
|---|---|
| resolve 超时 | 默认 10 秒；manifest 的 `timeoutMs` 可以改，硬顶 30 秒 |
| resolve 内存 | 64 MB |
| hook 超时 / 内存 | 5 秒 / 32 MB |
| 熔断器 | 连续 3 次超时或内存超限 → 插件自动禁用 |

被自动禁用的插件会在应用里弹出提示，可以在**设置 → 扩展 → 插件**里手动重新启用。

## 安装插件

打开桌面应用的**设置 → 扩展 → 插件**，有三条路：

- **上传 zip**——一个 `.fxplug` 文件（就是插件文件夹打的 zip，见[打包与市场](/docs/zh/plugins/packaging/)）。
- **从目录安装**——指向一个含 `manifest.json` 的本地文件夹。打开**开发模式**开关（目录安装默认开启）时，FluxDown 只记录文件夹路径、不拷贝，且每次调用都重新读你的 `.js` 文件——改完存盘直接生效，不用重装。改 manifest 仍需重新加载（把插件关掉再打开）。
- **插件市场**——在应用内浏览并一键安装已发布的插件。

已安装的插件放在 `<数据目录>/plugins/<identity>/` 下。新装的插件默认启用。

## 接下来

- [写第一个插件](/docs/zh/plugins/your-first-plugin/)——约 40 行写出一个能跑的 resolver。
- [Manifest 参考](/docs/zh/plugins/manifest/)——`manifest.json` 每个字段和校验规则。
- [API 参考](/docs/zh/plugins/api-reference/)——钩子签名和完整的 `flux.*` 接口。
- [打包与市场](/docs/zh/plugins/packaging/)——打 `.fxplug` 包、发布到索引。
