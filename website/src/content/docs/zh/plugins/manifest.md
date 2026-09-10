---
title: Manifest 参考
description: manifest.json 全部字段、设置项 widget 矩阵、URL pattern 规则。
section: plugins
order: 3
sourceHash: "b8cae49fcfee"
---

`manifest.json` 放在插件文件夹根部。所有键名用 camelCase。**未知键会被拒绝**——写错字段名会让整份 manifest 校验失败，而不是被静默忽略。

## 顶层字段

| 字段 | 必填 | 类型 | 规则 |
|---|---|---|---|
| `identity` | 是 | string | `^[a-z0-9_-]+@[a-z0-9_-]+$`——小写名字、`@`、作者。禁止点号。永久 ID：设置、存储、安装目录都以它为键。 |
| `name` | 是 | string | 显示名，不能为空。 |
| `version` | 是 | string | `主.次.补丁` 三段整数。 |
| `description` | 否 | string | 显示在插件列表里。 |
| `homepage` | 否 | string | 项目或作者主页。 |
| `icon` | 否 | string | 相对插件文件夹的路径。禁止 `..`、绝对路径、盘符、以 `/` 或 `\` 开头。 |
| `minAppVersion` | 否 | string | 三段版本号。运行中的 FluxDown 比它旧时，插件在加载阶段被跳过（记日志，不算错误）。 |
| `resolvers` | 否 | array | v1 **至多一个**。见下。 |
| `hooks` | 否 | object | 见下。 |
| `permissions` | 否 | array | 能力授权。v1 接受 `"ffmpeg"` 与 `"ytdlp"`，未知值一律拒绝。 |
| `settings` | 否 | array | 声明式设置项。见下。 |

插件可以只声明 resolver、只声明 hooks，或两者都有。两者都没有的 manifest 合法，只是什么也不做。

## `resolvers[0]`

```json
{
  "resolvers": [
    {
      "match": { "urls": ["*://host.com/share/*", "*://cdn.host.com/*"] },
      "entry": "resolver.js",
      "timeoutMs": 15000
    }
  ]
}
```

| 字段 | 必填 | 规则 |
|---|---|---|
| `match.urls` | 是 | 非空的 URL pattern 列表（规则见下）。 |
| `entry` | 是 | 脚本文件，安全相对路径。必须定义 `globalThis.resolve`。 |
| `timeoutMs` | 否 | 单次调用超时（毫秒），不能是 `0`。它替换默认的 10 秒，但无论写多大都被 30 秒硬顶封住。 |

## `hooks`

```json
{
  "hooks": {
    "entry": "hooks.js",
    "events": ["onStart", "onDone", "onError"],
    "match": { "urls": ["*://host.com/*"] }
  }
}
```

| 字段 | 必填 | 规则 |
|---|---|---|
| `entry` | 是 | 脚本文件，安全相对路径。按订阅的事件各定义一个全局函数。 |
| `events` | 是 | 非空，只能取 `onStart`、`onError`、`onDone`、`onMetaProbed`、`onCancel`，其余一律拒绝。 |
| `match` | 否 | 可选的 URL 过滤器，pattern 规则同上；存在时 `urls` 不能为空。省略 = 所有任务都触发。 |

注意：同一插件如果还声明了 resolver，`onMetaProbed` 对它的任务永远不触发——带 resolver 的任务直接跳过元数据探测。你照订阅的话，FluxDown 会记一条警告日志。

## `permissions`

插件主动申请的额外宿主能力。为空或省略 = 基础沙箱（`flux.fetch` 网络、`flux.storage`、日志）。v1 识别两个值：

| 值 | 授予 |
|---|---|
| `ffmpeg` | `flux.ffmpeg` 接口——对下载完成的文件运行解析到的 ffmpeg（见 [API 参考](/docs/zh/plugins/api-reference/)）。 |
| `ytdlp` | `flux.ytdlp` 接口——在 `resolve` 或任意 hook 里运行解析到的 yt-dlp（见 [API 参考](/docs/zh/plugins/api-reference/)）。 |

```json
{ "permissions": ["ffmpeg", "ytdlp"] }
```

未知值会让整份 manifest 失败——这样不认识某权限的旧版 FluxDown 会拒绝该插件，而不是悄悄忽略；新增权限时请一并抬高 `minAppVersion`。

## `settings[]`

每一项描述一个字段，应用据此生成表单。key 必须唯一且非空。

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
| `key` | 是 | 插件内唯一。脚本里用 `flux.settings.<key>` 读回。 |
| `title` | 是 | 表单标签。 |
| `description` | 否 | 字段下方的说明文字。 |
| `type` | 是 | `string`、`number` 或 `boolean`。 |
| `widget` | 否 | 缺省按 type 推导：string→`text`，number→`number`，boolean→`toggle`。 |
| `options` | 仅 select | 非空的 `{value, label}` 列表。 |
| `default` | 否 | **一律写成字符串**，number 也是（`"3"`），boolean 只能是 `"true"`/`"false"`。select 的 default 必须是某个 option 的 value；number 的 default 必须能解析且落在 `min`/`max` 区间内。 |
| `required` | 否 | 表单拒绝保存空值。 |
| `min` / `max` | 仅 number | 有限数、闭区间、`min ≤ max`。 |
| `pattern` | 仅 string | **JavaScript RegExp** 语法（不是 Rust regex），保存时对值做校验。 |
| `helperScript` | 仅 string | 一段脚本，宿主会在该字段旁渲染一个**复制**按钮。宿主只把脚本文本复制到剪贴板，**绝不执行**。用户把它粘贴到目标站点的浏览器开发者工具 Console 中运行（典型用途：读取 `document.cookie` 并把登录 Cookie 复制回该字段）。须非空、≤ 4 KB。 |
| `helperLabel` | 与 `helperScript` 同时出现 | 按钮文案（≤ 60 字符）。只能与 `helperScript` 一起使用；省略时用通用文案「复制获取脚本」。 |

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

例子：

| pattern | 对 `https://www.youtube.com/watch?v=abc` |
|---|---|
| `*://www.youtube.com/watch*` | 匹配 |
| `https://x.com/a`（无通配符） | 只做精确匹配 |
| `*://x.com/*` 对 `https://y.com/a` | 不匹配——找不到 `://x.com/` |
| `youtube.com/*` | 不匹配——首段前缀锚定，URL 以 `https://` 开头 |

## 校验总结

manifest 在安装时和每次加载时都会校验；失败则跳过该插件，原因写进日志。检查顺序：identity 格式 → name 非空 → version 格式 → `minAppVersion` 格式 → icon 路径安全 → resolver 至多一个 → resolver 的 entry 路径 / `match.urls` 非空 / `timeoutMs ≠ 0` → hooks 的 entry 路径 / `events` 非空且合法 / `match.urls`（如有）非空 → settings 的 key 唯一性和上面的逐字段规则 → `permissions` 全为已知值。

两个入口脚本在安装时还会做编译检查——语法错误当场拒绝，而不是等到第一次下载才发现。
