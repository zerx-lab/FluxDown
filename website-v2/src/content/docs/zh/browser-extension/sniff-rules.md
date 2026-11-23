---
title: 嗅探规则参考
description: 嗅探规则是什么、五个规则层级，以及规则每个字段的含义——匹配条件、阶段、动作、限制与吊销。
section: browser-extension
order: 3
sourceHash: "e11c20ca7fe6"
---

嗅探规则是一小段 JSON，告诉扩展：**哪些网络请求算可下载资源**、哪些地址其实是同一个文件、怎样区分视频轨和音轨、API 响应里的直链藏在哪、下载时需要带哪些请求头。绝大多数网站靠内置规则就能正常工作；只有当某个网站识别不对、你想自己修好它——或者把修复分享给所有人——时，才需要看这一页。分享流程见[贡献嗅探规则](/docs/zh/contributing/sniff-rules/)。

打开扩展的设置页 → **嗅探规则**，可以看到规则列表（带筛选，每条规则都有状态）、**规则同步**、你自己的**本地规则**，以及**规则测试台**——它会模拟一次响应，告诉你当前生效的规则会怎么判断。

规则列表可按层级筛选，也能按标题、ID、域名、扩展名、MIME 或地址搜索；可按求值顺序或命中次数排序，并显示每条规则的优先级、求值位置和本次会话的命中次数。每一层的规则都能点「查看规则」阅读全文（发布构建中的内置规则除外），也可以「复制为本地规则」后修改；本地规则的编辑、删除、提交也在列表的操作列里。规则测试台会列出所有命中的规则（标出生效的与被遮蔽的）并说明这条响应最终会不会被收录；在本地规则编辑器里还可以直接用未保存的草稿测试，并把测试结果「加为测试用例」随提交一起带上。

## 规则层级与谁能修改

每条规则属于五个层级之一。多条规则都可能生效时，层级决定谁先判断：

| 层级 | `tier` 取值 | 来源 | 谁能修改 |
| --- | --- | --- | --- |
| 本地 | `local` | 你在本机创建的规则（设置 → 嗅探规则 → 本地规则） | 你自己：编辑、导入、导出、删除 |
| 官方 | `official` | 由公开的[规则仓库](https://github.com/zerx-lab/fluxdown-sniff-rules)维护，扩展随包附带一份快照 | 维护者；你只能开关某条规则 |
| 插件 | `plugin` | FluxDown 插件声明的规则，经桌面端下发 | 插件作者；你可以开关规则 |
| 社区 | `community` | 作者在自己登记的规则仓库里维护（id 为 `community.<命名空间>.<名字>`），或一次性提交（`community.misc.*`），见[贡献嗅探规则](/docs/zh/contributing/sniff-rules/) | 作者通过自己的仓库或经审核更新；你只能开关某条规则 |
| 内置 | `builtin` | 随扩展一起发布 | 随扩展更新；你只能开关某条规则 |

**优先级：本地 > 官方 > 插件 > 社区 > 内置。** 同一层级内，`priority` 数值大的先判断；`priority` 相同则按规则在包内的先后顺序。对于只需要一个结论的判断（例如「是资源还是忽略」），**第一条命中的规则说了算**。

各层级怎么保持最新：

- **内置**规则和**官方**规则快照随扩展附带。官方包可以通过桌面端热更新：当桌面端提供的规则包 `sequence` 比随扩展附带的快照更高时，就用更新的那份，无需等扩展发版。
- **社区**规则依赖桌面端：桌面端从规则仓库拉取规则包（规则仓每小时收录登记仓库的新 Release）并验证签名，扩展再从桌面端同步（每 6 小时一次，或点**立即同步**）。社区包有效期 30 天；一旦社区包已过期，或吊销列表缺失、已过期，**整个社区包会暂停**（规则状态显示「已过期」），直到拿到新的为止。
- **启用社区规则**开关可以一次性停用整个社区包。
- 如果某个规则包需要比当前扩展更新的引擎，它会被跳过，规则状态显示「需更新扩展」。

## 一条规则长什么样

```json
{
  "id": "community.example.video",
  "version": 1,
  "tier": "community",
  "title": "Example：把 .xyz 文件识别为视频",
  "stage": "response",
  "match": { "host": ["example.com"], "ext": ["xyz"] },
  "exclude": [{ "url": ["*/preview/*"] }],
  "action": { "kind": "resource", "type": "video", "confidence": "high" }
}
```

| 字段 | 必填 | 含义 |
| --- | --- | --- |
| `id` | 是 | 唯一名称：层级、一个点，再加 2–63 个字符（小写字母、数字、`.`、`_`、`-`，且以字母或数字开头），例如 `local.my-site.video`。前缀必须与 `tier` 一致。插件规则的 id 总长不超过 64 个字符。 |
| `version` | 是 | 正整数。修改已发布的规则时请加一——吊销是按版本生效的。 |
| `tier` | 是 | `builtin`、`official`、`plugin`、`community` 或 `local`，必须与规则所在的规则包一致。 |
| `title` | 否 | 在规则列表中显示的名称。 |
| `enabled` | 否 | 默认 `true`。你在规则列表里切换开关会覆盖它。 |
| `priority` | 否 | 数字，默认 `0`。同一层级内越大越先判断。 |
| `stage` | 是 | 规则在流程的哪个环节运行，同时决定允许哪些 `action`。见[阶段与动作](#阶段与动作)。 |
| `requires` | 否 | 只能是 `"sniffImages"`：仅当设置里开启了图片嗅探时，这条规则才参与响应识别。 |
| `classifyOnly` | 否 | `true` 表示规则只用于「这个地址 / MIME 是什么类型」的查询，不参与实时响应的判定。 |
| `match` | 是 | 请求要满足的条件。见[匹配条件](#匹配条件)。 |
| `exclude` | 否 | 一组额外的 `match` 对象。**任意一个**命中，就视为本条规则不匹配。 |
| `action` | 是 | 要做什么。`kind` 必须是该规则 `stage` 允许的动作。 |

同一个规则包里 `id` 不能重复。校验不通过的规则会被剔除（本地规则编辑器和规则仓库都会告诉你原因）。

## 匹配条件

`match` 是一个对象，所有字段都可选。**写了多个字段时，必须全部满足。** 列表类字段（`host`、`url`、`ext`、`mime`、`resourceType`）里**任意一项**命中即可。`exclude` 是一组这样的对象，任意一个命中，本条规则就不生效。

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| `host` | 字符串列表 | **请求地址**的域名。`"example.com"` 匹配 `example.com` 及其所有子域（如 `cdn.example.com`）；`"=example.com"` 只匹配这一个域名。**不支持**通配写法 `"*.example.com"`。 |
| `pageHost` | 字符串列表 | 写法同上，作用于用户所在**顶层页面**的域名。 |
| `url` | 通配串列表 | 对整个 URL（含查询串）做通配匹配。`*` 是唯一的通配符；第一个 `*` 之前的部分必须是前缀，最后一个 `*` 之后的部分必须是后缀；不区分大小写。不含 `*` 时必须完全相同。 |
| `pathRegex` | 字符串 | 只对 URL 的**路径**（不含查询串）做正则匹配，不区分大小写。必须符合[正则安全子集](#正则安全子集)。 |
| `ext` | 字符串列表 | URL 路径里的文件扩展名，小写、不带点（`"mp4"`）。没有扩展名的 URL 永远不匹配。 |
| `hasExt` | 布尔 | `true` 表示 URL 路径带扩展名，`false` 表示不带。 |
| `mime` | 字符串列表 | 响应的 `Content-Type`，去掉参数并转小写后比较。以 `*` 结尾的是前缀写法（`"video/*"`），否则必须完全一致（`"application/x-mpegurl"`）。没有 MIME 类型的响应永远不匹配。 |
| `resourceType` | 字符串列表 | 浏览器的请求类型（`"media"`、`"xmlhttprequest"`、`"image"` 等）。从页面内部观察到的请求记作 `"xmlhttprequest"`。 |
| `status` | `[下限, 上限]` | HTTP 状态码落在闭区间内，如 `[200, 206]`。需要已知状态码。 |
| `minSize` / `maxSize` | 数字（字节） | 大小上下限。**大小未知**时，该条件视为满足。 |
| `attachment` | 布尔 | 响应是否带 `Content-Disposition: attachment`。 |
| `header` | `{ "name", "regex" }` 列表 | 响应头；列出的**每一个**头都必须存在且匹配其正则（不区分大小写）。`name` 按小写比较。 |

示例：

```json
{ "host": ["example.com"], "ext": ["m3u8", "mpd"] }
{ "pageHost": ["=www.example.com"], "mime": ["video/*"], "minSize": 1048576 }
{ "host": ["cdn.example.com"], "pathRegex": "^/v/[0-9a-f]{8}/" }
{ "url": ["https://example.com/download/*"], "attachment": true }
{ "host": ["example.com"], "header": [{ "name": "x-content-kind", "regex": "^video$" }] }
```

哪些字段有值取决于阶段。`response` 阶段能看到全部信息；其它阶段只知道请求地址（以及页面地址），所以 `mime`、`status`、大小、`attachment`、`header`、`resourceType` 这些条件只适合 `response` 阶段（`track` 阶段也能看到 MIME 类型）。在 `body` 阶段，`host` / `url` / `pathRegex` 描述的是被检查响应的 **API 地址**，`pageHost` 描述的是发出请求的页面。

### 正则安全子集

规则里所有正则（`pathRegex`、`header[].regex`，以及 `track` 动作的 `pattern`）都必须：

- 长度不超过 **256 个字符**，并且能被编译为 JavaScript 正则（匹配时不区分大小写）；
- 不使用反向引用（`\1`、`\k<name>`）；
- 不使用先行 / 后行断言（`(?=`、`(?!`、`(?<=`、`(?<!`）；
- 不使用嵌套量词，例如 `(a+)+`。

CSS 选择器（`dom`、`title`、`player`）不能为空、不超过 **512 个字符**，并且必须合法。

## 阶段与动作

规则的 `stage` 决定它在什么时候运行，也限定了能用哪些 `action.kind`：

| 阶段 | 含义（界面上的名称） | 允许的动作 |
| --- | --- | --- |
| `response` | 判断一个网络响应是什么（响应识别） | `resource`、`ignore` |
| `normalize` | 判定地址是否同一文件、找扩展名、归并镜像域名（地址归一化） | `strip-params`、`filename`、`mirror` |
| `track` | 区分音轨与视频轨（音视频分轨） | `track` |
| `body` | 从 API 的 JSON 响应里取出直链（响应体提取） | `extract`、`manifest` |
| `dom` | 从页面元素里取链接（页面元素） | `dom` |
| `page` | 页面级提示（页面提示） | `title`、`player` |
| `request` | 下载时补充请求头（下载请求） | `headers` |

### `resource`：识别为资源（`response` 阶段）

把命中的响应标记为可下载资源。

| 字段 | 含义 |
| --- | --- |
| `type` | 取值之一：`video`、`audio`、`document`、`archive`、`image`、`executable`、`torrent`、`stream`、`subtitle`、`magnet`、`other`。 |
| `confidence` | 可选：`high`、`medium` 或 `low`。 |

```json
{
  "id": "local.example.xyz",
  "version": 1,
  "tier": "local",
  "title": "Example：.xyz 是视频",
  "stage": "response",
  "match": { "host": ["example.com"], "ext": ["xyz"] },
  "action": { "kind": "resource", "type": "video" }
}
```

响应规则按层级和 `priority` 的顺序检查，**第一条命中的规则说了算**——所以排在 `ignore` 规则前面的 `resource` 规则会赢，反之亦然。带 `requires: "sniffImages"` 的规则在图片嗅探关闭时会被跳过。当扩展只需要知道某个地址的*类型*时，会查询 `resource` 规则：先看带 `mime` 条件的，再看只有 `ext` 的，都不命中就是 `other`。所有生效的 `resource` 规则的 `ext` 合在一起，就是 `filename` 动作使用的「已知扩展名」集合。

### `ignore`：不显示（`response` 阶段）

除了 `kind` 没有别的字段。命中的响应不会出现在资源列表里。

```json
{
  "id": "local.example.no-thumbs",
  "version": 1,
  "tier": "local",
  "stage": "response",
  "match": { "host": ["img.example.com"], "pathRegex": "/thumb/" },
  "action": { "kind": "ignore" }
}
```

### `strip-params`：比较地址时去掉查询参数（`normalize` 阶段）

`params` 是查询参数名列表（不区分大小写）。扩展判断两个地址是不是同一个文件时，会去掉这些参数、丢掉片段（`#…`），并把剩下的参数排序。`match` 为空（没有任何条件，也没有 `exclude`）的规则对所有地址生效——只有内置、官方和本地规则允许这样写。带范围的规则只对命中的地址生效。

```json
{
  "id": "local.example.strip-token",
  "version": 1,
  "tier": "local",
  "stage": "normalize",
  "match": { "host": ["cdn.example.com"] },
  "action": { "kind": "strip-params", "params": ["token", "expires", "sign"] }
}
```

### `filename`：从查询串里找扩展名（`normalize` 阶段）

用于路径里没有扩展名的地址（`/download?file=a.zip`），改从查询参数里取扩展名。`extFromQuery` 是参数名列表，按顺序尝试；参数值是已知扩展名时采用（见上文 `resource`）。如果都没有结果，就找任何以 `.扩展名` 结尾、且扩展名已知的查询参数值。路径自带的扩展名始终优先。

```json
{
  "id": "local.example.dl-ext",
  "version": 1,
  "tier": "local",
  "stage": "normalize",
  "match": { "host": ["example.com"], "pathRegex": "^/download$" },
  "action": { "kind": "filename", "extFromQuery": ["format", "file"] }
}
```

### `mirror`：镜像域名组（`normalize` 阶段）

`hosts` 至少列出两个域名模式（写法同 `host`）。这些域名上路径相同的文件会被视为同一个文件（用于合并清单和识别正在播放的流）。镜像组的判定不使用规则的 `match`，但社区和插件规则仍然必须写上 `host` 或 `pageHost`。

```json
{
  "id": "community.example.mirrors",
  "version": 1,
  "tier": "community",
  "stage": "normalize",
  "match": { "pageHost": ["example.com"] },
  "action": { "kind": "mirror", "hosts": ["cdn1.example.net", "cdn2.example.net", "cdn3.example.net"] }
}
```

### `track`：音轨还是视频轨（`track` 阶段）

很多网站把音频和视频分成两个文件提供。这个动作从文件名（或 URL）里读出一个编号，据此判断文件是哪种轨道。

| 字段 | 含义 |
| --- | --- |
| `source` | 固定为 `"filenameOrUrl"`：对文件名做匹配，没有文件名时对 URL 匹配。 |
| `pattern` | 正则（安全子集）。 |
| `group` | 编号在第几个捕获组（非负整数；`0` 表示整个匹配）。 |
| `audio` | 表示**音频**的编号列表。 |
| `video` | 可选：表示视频的编号列表。省略时，不在 `audio` 里的编号都算视频。 |
| `fallback` | `"video"`、`"audio"` 或 `"none"`：正则或编号对不上时的结论。 |

MIME 类型以 `audio/` 开头的响应一律是音频。第一条给出结论的命中规则生效。

下面是 bilibili CDN 的官方规则（实际文件里还有一条用 `pageHost` 的孪生规则）：

```json
{
  "id": "official.bilibili.track",
  "version": 1,
  "tier": "official",
  "title": "哔哩哔哩：按 m4s 分片编号区分音轨 / 视频轨（CDN 域名）",
  "stage": "track",
  "match": { "host": ["bilibili.com", "bilivideo.com", "bilivideo.cn", "acgvideo.com"] },
  "action": {
    "kind": "track",
    "source": "filenameOrUrl",
    "pattern": "[-_](\\d{4,6})\\.m4s(?:[?#]|$)",
    "group": 1,
    "audio": ["30216", "30232", "30280", "30250", "30251"],
    "fallback": "video"
  }
}
```

对于 `…/x/xxx-1-30280.m4s`，捕获组是 `30280`，在 `audio` 里，所以判为音轨；`…-30080.m4s` 不在 `audio` 里，落到 `fallback`，算视频。

### `extract`：从 API 响应里取直链（`body` 阶段）

有些网站不会把真实文件地址写在页面里，而是通过一次 API 调用返回 JSON。`extract` 读取这段 JSON 并产出资源。

| 字段 | 含义 |
| --- | --- |
| `when` | 可选，`{ "path", "equals" }` 条件列表。**全部**成立规则才会执行。`equals` 是字符串、数字或布尔值，严格比较（`1` 不等于 `"1"`）。 |
| `each` | 可选，指向 JSON 中某个**数组**的点路径；规则会对数组的每个元素各产出一个资源。不写 `each` 时，整个 JSON 就是唯一的元素。如果 `each` 指向的不是数组，则什么都不产出。 |
| `fields` | 「字段名 → 点路径」的映射，从每个元素里读取。值必须是字符串、数字或布尔（会转成字符串），其它类型会让该字段处于未定义状态。 |
| `emit` | 如何用这些字段构造资源。见[用 emit 构造资源](#用-emit-构造资源)。 |

**点路径**用 `.` 逐层往下走：对象按键名、数组按下标——`data.list.0.url` 表示 `data.list` 第一项的 `url`。键名按字面匹配（含点号的键名无法被指定）。

蓝奏云的官方规则，把 `ajaxm.php` 的响应变成直链：

```json
{
  "id": "official.lanzou.extract",
  "version": 1,
  "tier": "official",
  "title": "蓝奏云：从 ajaxm.php 响应提取直链",
  "stage": "body",
  "match": { "url": ["*/ajaxm.php*"] },
  "action": {
    "kind": "extract",
    "when": [{ "path": "zt", "equals": 1 }],
    "fields": { "dom": "dom", "url": "url", "name": "inf" },
    "emit": {
      "mode": "preempt",
      "url": { "resolve": { "base": "${dom}", "ref": "${url}", "bareQueryPrefix": "/file/" } },
      "filename": "${name}"
    }
  }
}
```

对于 `{ "zt": 1, "dom": "https://dl.cdn.com", "url": "?token", "inf": "a.zip" }` 这样的响应，会得到名为 `a.zip` 的 `https://dl.cdn.com/file/?token`；`zt` 不等于 1，或者缺少字段时，什么都不产出。（社区规则不能用 `mode: "preempt"`，请用 `"resource"`。）

### `manifest`：声明 DASH 清单（`body` 阶段）

JSON 响应里包含 DASH 结构（带 `baseUrl` 的 `video[]` 与 `audio[]`），扩展会把它当作流媒体清单处理。`path` 是可选的点路径，指向包含该结构的子树（默认是整个 JSON）。

```json
{
  "id": "community.example.dash-json",
  "version": 1,
  "tier": "community",
  "stage": "body",
  "match": { "host": ["api.example.com"], "pathRegex": "^/play/url$", "pageHost": ["example.com"] },
  "action": { "kind": "manifest", "path": "data.dash" }
}
```

### `dom`：从页面元素里取链接（`dom` 阶段）

| 字段 | 含义 |
| --- | --- |
| `selector` | 要读取的元素的 CSS 选择器（不超过 512 个字符）。 |
| `attr` | 要读取的属性，如 `"data-src"`。对于 URL 型属性（`href`、`src`、`data`、`poster`、`action`、`currentSrc`），取浏览器解析后的绝对地址。取值为空的元素会被跳过。 |
| `emit` | 如何构造资源。可用的模板字段：`value`（属性值）、`text`（元素文本，去除首尾空白并截取前 200 个字符）、`page`（规则匹配所用的页面地址）。 |

如果该元素是页面上的媒体元素，或包含媒体元素，产出的资源会归属到这个元素（这样悬浮下载按钮才认得它）。

```json
{
  "id": "community.example.dom-video",
  "version": 1,
  "tier": "community",
  "title": "Example：从 data-video-url 取视频地址",
  "stage": "dom",
  "match": { "pageHost": ["example.com"], "pathRegex": "^/watch/" },
  "action": {
    "kind": "dom",
    "selector": "div.player[data-video-url]",
    "attr": "data-video-url",
    "emit": { "mode": "resource", "url": "${value}", "filename": "${text}", "type": "video" }
  }
}
```

### `title`：文件名从哪里来（`page` 阶段）

告诉扩展哪个元素里写着正在播放的视频或音频的标题。离播放元素最近的标题会成为候选名称和默认文件名。

| 字段 | 含义 |
| --- | --- |
| `selector` | CSS 选择器。使用**离媒体元素最近**的匹配节点。 |
| `attr` | 可选：读取该属性，而不是元素文本。 |

这是抖音的官方规则：

```json
{
  "id": "official.douyin.title",
  "version": 1,
  "tier": "official",
  "title": "抖音：视频描述作为文件名",
  "stage": "page",
  "match": { "pageHost": ["douyin.com"] },
  "action": { "kind": "title", "selector": "[data-e2e=\"video-desc\"]" }
}
```

### `player`：悬浮按钮的显示方式（`page` 阶段）

作用于匹配 `selector`（或位于匹配元素内部）的媒体元素，`float` 取下面两种值之一：

| `float` | 效果 |
| --- | --- |
| `"hide"` | 不显示悬浮下载按钮——适合背景视频、广告和预览视频。 |
| `"inset"` | 贴在顶部的悬浮按钮离媒体元素顶部至少 `top` 像素（`top` 为 1–200 的整数），避开播放器自带的顶部控件条（例如迷你播放器的展开 / 关闭按钮）；元素剩余高度放不下按钮时隐藏按钮。按钮设在底部时不受影响。较早版本的扩展不认识 `inset`，会跳过这条规则。 |

```json
{
  "id": "community.example.hide-bg",
  "version": 1,
  "tier": "community",
  "stage": "page",
  "match": { "pageHost": ["example.com"] },
  "action": { "kind": "player", "selector": ".hero-bg video, .ad-slot video", "float": "hide" }
}
```

```json
{
  "id": "community.example.mini-player",
  "version": 1,
  "tier": "community",
  "stage": "page",
  "match": { "pageHost": ["example.com"] },
  "action": { "kind": "player", "selector": ".mini-player", "float": "inset", "top": 52 }
}
```

### `headers`：补充请求头（`request` 阶段）

为规则命中的地址在下载请求里补充请求头（典型的是为防盗链加上 `Referer` 或 `Origin`）。

| 字段 | 含义 |
| --- | --- |
| `set` | 「请求头名 → 值」的映射，1 到 16 个。名称：1–64 个字母、数字或 `-`；值：不超过 1024 个字符，不能含换行。 |

下面这些头是**禁止设置**的，因为它们由浏览器或下载引擎负责：`Cookie`、`Authorization`、`Proxy-Authorization`、`Host`、`Content-Length`、`Range`、`Connection`。多条规则设置同一个头时，先命中的规则优先。

```json
{
  "id": "community.example.referer",
  "version": 1,
  "tier": "community",
  "title": "Example：CDN 要求带 Referer",
  "stage": "request",
  "match": { "host": ["cdn.example.net"] },
  "action": { "kind": "headers", "set": { "Referer": "https://example.com/" } }
}
```

## 用 emit 构造资源

`extract` 和 `dom` 通过 `emit` 对象构造资源：

| 字段 | 含义 |
| --- | --- |
| `mode` | `"resource"` 把链接加入资源列表。`"preempt"` 用于一次性的下载地址（例如中转页）：该链接**不会**列出来，扩展会把它记住 30 秒，避免浏览器自己对这个地址发起的下载被再次交给 FluxDown。**只有内置和官方规则可以使用 `preempt`。** |
| `url` | 链接：一个模板字符串，或一个 `resolve` 对象（见下）。 |
| `filename` | 可选，文件名模板；渲染结果为空时忽略。 |
| `type` | 可选，资源类型（取值同 `resource`）。省略时由 `resource` 规则根据 URL 推断类型。 |
| `quality` | 可选，清晰度标签（如 `1080p`）的模板；渲染结果为空时忽略。 |

**模板**只支持 `${名称}` 替换，不支持表达式。在 `extract` 里，`名称` 必须是 `fields` 的某个键；在 `dom` 里，必须是 `value`、`text`、`page` 之一。引用其它名称会让规则校验不通过。如果规则运行时某个字段**缺失**（比如 JSON 里没有这个值），模板就无法渲染，**不会产出任何资源**——只有 `filename` 或 `quality` 缺字段时，只是少了这个属性。

**`url` 写成对象**，适用于 API 返回相对引用的情况：

```json
"url": { "resolve": { "base": "${dom}", "ref": "${url}", "bareQueryPrefix": "/file/" } }
```

`base` 和 `ref` 都是模板。结果取决于 `ref` 的形态：

1. `ref` 以 `http://` 或 `https://` 开头——原样使用。
2. `ref` 以 `/` 开头——接在 `base` 的协议和域名之后（`https://dl.cdn.com` + `/file/?abc` → `https://dl.cdn.com/file/?abc`）。
3. 其它情况（例如只有查询串 `?token`）——如果写了 `bareQueryPrefix`，结果是 `base` + `bareQueryPrefix` + `ref`（`https://dl.cdn.com/file/?token`）；没写则不产出。

最终地址必须以 `http` 开头。

## 各层级的限制

| 限制 | builtin | official | plugin | community | local |
| --- | --- | --- | --- | --- | --- |
| `match` 必须包含 `host` 或 `pageHost` | 否 | 否 | **是** | **是** | 否 |
| `emit.mode: "preempt"` | 可以 | 可以 | 不可以 | 不可以 | 不可以 |
| 全局 `strip-params`（没有任何匹配条件） | 可以 | 可以 | 不可以 | 不可以 | 可以 |
| `title` / `player` 规则必须写 `host` 或 `pageHost` | **是** | **是** | **是** | **是** | 否 |
| `headers` 规则 | 须写 `host` 或 `pageHost` | 须写 `host` 或 `pageHost` | 不允许 | 须写 `host` 或 `pageHost` | 须写 `host` 或 `pageHost` |
| `extract` / `dom` 产出的下载地址，域名必须落在规则自身 `host` / `pageHost` 范围内（否则丢弃） | 否 | 否 | 否 | **是** | 否 |
| `mirror.hosts` 必须落在规则的 `host` / `pageHost` 范围内 | 否 | 否 | 否 | **是** | 否 |

所有层级都适用的上限：

- 每个规则包最多 **2000 条**规则。
- 正则最长 **256 个字符**且在安全子集内；CSS 选择器最长 **512 个字符**。
- 每条 `headers` 规则最多 **16 个**请求头，且不含禁止设置的头。
- `mirror` 至少两个域名；`id` 必须唯一，前缀必须与 `tier` 一致。
- 域名模式必须是合法域名；`"*.example.com"` 会被拒绝。

## 吊销

如果某条规则有害或已失效，维护者可以在**吊销列表**里为它发布一条吊销。任何层级都可以被吊销，包括内置规则。

| 字段 | 含义 |
| --- | --- |
| `ruleId` | 要吊销的规则。 |
| `versions` | `"*"` 表示所有版本，`{ "max": N }` 表示不超过 `N` 的版本。修复后版本号更高的规则不受影响。 |
| `action` | `"soft"` 或 `"hard"`。 |
| `reason` | 原因——会显示在规则列表里。 |
| `ref` | 可选，指向讨论的链接。 |

- **soft（软吊销）**：规则被关闭，显示为「已撤回」，但你读完原因后可以点**仍然启用**把它重新打开。这个确认是针对当前吊销列表的，吊销列表更新后可能需要再确认一次。
- **hard（硬吊销）**：规则被关闭，显示为「已撤回（不可操作）」，不能再手动启用。

吊销列表本身带签名并会过期；它缺失或已过期期间，社区包保持暂停。

## 更多示例

**把某个网站的 `.m3u8` / `.mpd` 识别为流媒体，同时忽略它的广告域名**——两条规则。`ignore` 规则的 `priority` 更高，所以先判断，第一条命中的说了算：

```json
{
  "id": "local.example.ads",
  "version": 1,
  "tier": "local",
  "priority": 10,
  "stage": "response",
  "match": { "pageHost": ["example.com"], "host": ["ads.example-cdn.com"] },
  "action": { "kind": "ignore" }
}
```

```json
{
  "id": "local.example.stream",
  "version": 1,
  "tier": "local",
  "stage": "response",
  "match": { "pageHost": ["example.com"], "ext": ["m3u8", "mpd"] },
  "action": { "kind": "resource", "type": "stream", "confidence": "high" }
}
```

**只把较大的 `application/octet-stream` 下载识别为压缩包**——用 `minSize` 加一个 `exclude`：

```json
{
  "id": "local.example.octet",
  "version": 1,
  "tier": "local",
  "stage": "response",
  "match": { "host": ["files.example.com"], "mime": ["application/octet-stream"], "minSize": 5242880 },
  "exclude": [{ "pathRegex": "/(thumb|preview)/" }],
  "action": { "kind": "resource", "type": "archive" }
}
```

上文的三条官方规则（bilibili 的 `track`、蓝奏云的 `extract`、抖音的 `title`）都是完整的真实示例，完整文件见[规则仓库](https://github.com/zerx-lab/fluxdown-sniff-rules/tree/main/rules/official)。想试一条规则，可以先在**本地规则**里新建，再用**规则测试台**验证；确认有效后欢迎分享，见[贡献嗅探规则](/docs/zh/contributing/sniff-rules/)。
