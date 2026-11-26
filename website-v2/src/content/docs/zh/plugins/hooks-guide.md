---
title: 钩子指南
description: 响应下载生命周期事件：通知、重试、用 ffmpeg 后处理、取消时清理，以及调用 yt-dlp。
section: plugins
order: 4
sourceHash: "e0a7561ab0fe"
---

钩子（hook）是 FluxDown 在下载发生变化时运行的小脚本：任务开始、元数据探测完成、下载完成、下载失败、任务被取消。钩子不能修改任务本身，但可以做出反应——发送通知、安排重试、用 ffmpeg 转换成品文件，或者释放临时资源。

本文先讲清每个事件在什么时候触发、钩子能做什么，再完整实现五个插件。各 API 的完整签名和限制见 [API 参考](/docs/zh/plugins/api-reference/)，manifest 字段见 [Manifest 参考](/docs/zh/plugins/manifest/)。

> 钩子是一次性的短脚本。如果你需要长期保存的状态、视图或命令，请改用常驻的 `main` 脚本（见 [可移植界面指南](/docs/zh/plugins/ui-guide/)），它通过 `main.events` 同样能收到这些事件。

## 钩子如何工作

### 声明钩子

插件声明一个钩子脚本和它关心的事件：

```json
"hooks": {
  "entry": "hooks.js",
  "events": ["onDone", "onError"],
  "match": { "urls": ["*://files.example.com/*"] }
}
```

| 字段 | 必填 | 含义 |
|---|---|---|
| `entry` | 是 | 脚本文件，安全的相对路径。 |
| `events` | 是 | 非空数组，取值范围：`onStart`、`onMetaProbed`、`onDone`、`onError`、`onCancel`。 |
| `match.urls` | 否 | 用 glob 模式过滤哪些任务会触发钩子。省略表示所有任务；一旦写出就不能为空。 |

脚本为每个事件定义一个同名全局函数。经典脚本里，顶层的 `function` 声明（或者 `globalThis.onDone = …`）就够了。`const onDone = …` **不行**，因为它不会在 `globalThis` 上创建属性，而 FluxDown 正是从那里查找入口。

```js
async function onDone(ctx) {
  flux.logger.info("finished", ctx.filePath);
}
```

如果 `events` 里列了某个事件，脚本里却没有对应函数，什么都不会发生，也不会报错。

### 执行模型

- **每次都是全新上下文。** 每个事件都会在新的 QuickJS 上下文里重新执行整个脚本，顶层变量不会在事件之间保留。需要持久化的数据请用 `flux.storage`（键值）或 `flux.fs`（文件）。
- **即发即弃（fire-and-forget）。** 返回值被忽略。异常和超时只记一条警告日志（`hook onDone 执行失败（已忽略）: …`）然后吞掉。钩子对任务没有任何影响，只有两个明确的例外：`flux.task.requestRetry`（`onError`）和 `flux.task.recordArtifact`（`onDone`）。
- **不计入熔断。** 钩子超时或内存超限，不会累加用于自动禁用 resolver 的「连续三次」计数。
- **事件会投递给所有匹配的插件**，而不只是处理该任务的那个插件。请用 `hooks.match.urls` 或自己检查 `ctx.url` 来过滤。
- **`ctx.event`** 是小驼峰的事件名：`"start"`、`"metaProbed"`、`"done"`、`"error"`、`"cancel"`。
- **设置项。** 可以使用 `flux.settings`。`required: true` 对钩子**不生效**（resolver、`authenticate`、`subscribe` 才会校验），请自行检查缺失的值。没有默认值且未填写的 string/number 设置是 `null`，未填写的 boolean 是 `false`。

### 预算与背压

| | 普通钩子 | 声明了 `ffmpeg` 权限的插件的 `onDone`，以及声明了 `ytdlp` 权限的插件的所有钩子 |
|---|---|---|
| 单次调用墙钟上限 | 5 秒 | 约 30 分钟（1830 秒） |
| 内存 | 32 MB | 32 MB |
| 脚本 CPU 时间 | 不超过 5 秒上限 | 30 秒；等待 `fetch`、ffmpeg 或 yt-dlp 的时间不算 CPU 时间 |

并发是有上限的。事件到来时，如果短钩子池的所有名额都被占满，这次通知会被**丢弃**，并记录警告 `钩子并发已满，本次钩子被丢弃`。声明了 `ffmpeg`/`ytdlp` 的插件的钩子走另一个池：它们会排队（最多 64 个在等），不会立刻被丢弃；队列也满时日志会写 `外部工具钩子排队已满，本次钩子被丢弃`。高负载下不要假设钩子恰好被投递一次，请让钩子具备幂等性。

## 各事件何时触发

| 事件 | 触发时机 | 额外的 `ctx` 字段 |
|---|---|---|
| `onStart` | 任务第一次真正开始下载。 | — |
| `onMetaProbed` | 后台 HEAD 探测得到了文件名和/或大小。 | `fileName`、`totalBytes` |
| `onDone` | 任务成功完成。 | `filePath`、`audioPath`、`muxed` |
| `onError` | 任务进入失败状态——**每一次**都触发，包括之后会被自动重试的失败。 | `message` |
| `onCancel` | 用户关闭了选择对话框（选择插件 / 选择变体），任务被暂停。 | `reason` |

所有事件都带有 `event`、`taskId` 和 `url`。

### `onStart`

- 任务离开队列、真正开始时触发一次，发生在插件 resolver（如果有）解析完成之后。任务被暂停后恢复，或失败后重试，**不会**再次触发。
- 对由 resolver 处理的任务，`ctx.url` 是**解析后的直链**，而不是用户添加的 URL；`hooks.match.urls` 过滤用的也是这个解析后的 URL。因此，如果你的钩子只匹配页面 URL（`*://share.example/*`），对解析到 CDN 域名的任务就永远收不到 `onStart`。其他所有事件用的都是**原始**任务 URL。
- 如果 resolver 失败，任务根本没有开始，`onStart` 不会触发。

### `onMetaProbed`

- 对任务 URL 做一次快速的后台探测，得到文件名或大小时触发。探测只针对在队列里等待、或以暂停状态创建的普通下载。以下情况会跳过：浏览器扩展已经给出大小提示、BitTorrent/磁力任务，以及**任何带 resolver 的任务**（无论是你的还是别的插件的——探测原始页面 URL 没有意义）。立即开始的任务不会走这条探测。
- 不要指望它拿到每个下载的最终文件名；那应该用 `onDone`。

### `onDone`

- 文件在磁盘上完整落盘之后触发。`filePath` 是绝对路径；在 Windows 上可能混有 `/` 和 `\`，请用 `/[\\/]/` 来分割。
- `audioPath` 和 `muxed` 描述音视频分开下载的任务：`muxed: true` 表示宿主已把它们合并成一个文件；`muxed: false` 且 `audioPath` 非空表示合并失败，音频留在单独的 sidecar 文件里；普通任务的 `audioPath` 是 `null`、`muxed` 是 `false`。
- 当用户对「文件已存在」选择了**跳过**时**不会**触发（宿主采纳了已有文件，它不是该任务的产物）。
- 这是唯一带有产物文件的钩子，因此也是 `flux.ffmpeg`、`flux.ffprobe` 和 `flux.task.recordArtifact` 唯一能用的地方。

### `onError`

- 任务每次报告失败都会触发，包括宿主马上要自动重试的那些。请用 `ctx.message` 决定怎么做。
- **resolver** 失败时不会触发（任务显示 `[插件] <identity>: <error>`，并在开始之前就停下）。
- `flux.task.requestRetry` 只在这里有效，见[示例 2](#示例-2按条件重试)。

### `onCancel`

`ctx.reason` 一定存在，取值如下：

| `reason` | 含义 |
|---|---|
| `user_cancelled` | 用户关闭了「选择插件」对话框。任务被**暂停**而不是删除，恢复时会再次询问。 |
| `variant_selection_cancelled` | 用户关闭了带 `variants` 的解析所弹出的画质/格式选择框。任务被**暂停**而不是删除，恢复时会再次询问。 |

`onCancel` 与 `onDone`、`onError` 针对同一次结果互斥：一次被取消的运行不会同时报告完成或失败。它只用于释放临时资源，不能重试。由于暂停的任务之后还能再次恢复，同一个任务可能先收到 `onCancel`，恢复后又收到 `onDone`。

任务被删除、或任务运行期间 daemon 被停止时，`onCancel` **不会**被投递——如果钩子在这些情况下也要释放远程状态，需要自己做定期清扫。

## 用 `hooks.match.urls` 过滤

- `*` 是唯一的通配符，匹配任意长度的任意字符（包括 `/`）。匹配不区分大小写，针对**完整 URL**。
- 不含 `*` 的模式必须与 URL 完全相等。第一个 `*` 之前的部分锚定在开头，最后一个 `*` 之后的部分锚定在结尾。
- `*://files.example.com/*` 同时匹配 `http` 和 `https`；`files.example.com/*` 什么都匹配不到，因为第一段是锚定的。
- 比较哪个 URL 取决于事件：`onStart` 用解析后的 URL，其余事件用原始任务 URL。
- 过滤只作用于 `hooks`。通过 `main.events` 投递给 `main` 脚本的事件不受过滤。

## 钩子里能用什么

| 门面 | 需要的权限 | 可用位置 |
|---|---|---|
| `flux.fetch`、`flux.storage`、`flux.fs`、`flux.logger` / `console`、`flux.settings`、`flux.info` | 无 | 所有钩子 |
| `flux.task.requestRetry({ delayMs })` | 无 | 仅 `onError` |
| `flux.task.recordArtifact(name)` | 无 | 仅 `onDone` |
| `flux.ffmpeg`、`flux.ffprobe` | `ffmpeg` | 仅 `onDone`（其他钩子里对象存在，但 `run` 会被拒绝） |
| `flux.ytdlp` | `ytdlp` | 所有钩子 |
| `flux.auth` | `auth` | 所有钩子 |

`flux.tasks`、`flux.ui` 等常驻脚本的门面在钩子里**不可用**。在不合适的位置调用会记一条警告（`flux.task.requestRetry 仅 onError 钩子可用，已忽略`），或者以错误拒绝（`flux.task.recordArtifact 仅 onDone 钩子可用`）。

声明 `ffmpeg` 或 `ytdlp` 后，安装时 FluxDown 会请用户授权；同时也会提高时间预算（见上文），并把该插件的钩子移到单独的排队池。只声明 `ffmpeg` 时，长预算只对 `onDone` 生效，该插件的其他钩子仍是 5 秒。详见[权限](/docs/zh/plugins/permissions/)。

## 开发循环

1. 用**开发模式**安装插件文件夹（设置 → 扩展 → 插件 → 从目录安装）。钩子脚本在**每个事件**触发时都会重新读取，所以改完保存、触发一次下载即可，不用重装。
2. 修改 `manifest.json`（事件、`match`、设置项、权限）后，需要点开发模式插件那一行的「重新加载」按钮，它会重新读取 manifest 和全部脚本。
3. 如果 manifest 声明了权限，安装后要在弹出的对话框里授权；授权之前插件保持禁用。
4. 在插件日志里查看 `flux.logger` 的输出和被忽略的失败，见[调试与排错](/docs/zh/plugins/debugging/)。

测试下载可以用任何匹配你的 `match.urls` 的小文件；测试 `onCancel` 时，用一个返回 `variants` 的 resolver，然后关闭弹出的选择框。

## 示例 1：下载完成时发通知

一个 webhook 通知器：向用户配置的 URL 发送一条 JSON 消息，并写一行日志。它不需要任何权限，`flux.fetch` 始终可用。

`manifest.json`

```json
{
  "identity": "acme@done-notifier",
  "name": "Done Notifier",
  "version": "1.0.0",
  "description": "Posts a JSON message to a webhook when downloads finish or fail.",
  "hooks": {
    "entry": "hooks.js",
    "events": ["onDone", "onError"]
  },
  "settings": [
    {
      "key": "webhookUrl",
      "title": "Webhook URL",
      "description": "Receives a JSON POST for every finished or failed download.",
      "type": "string",
      "pattern": "^https?://\\S+$"
    },
    {
      "key": "notifyOnError",
      "title": "Also notify on failures",
      "type": "boolean",
      "default": "true"
    }
  ]
}
```

`hooks.js`

```js
function baseName(path) {
  return String(path).split(/[\\/]/).pop();
}

async function send(payload) {
  const url = flux.settings.webhookUrl;
  if (!url) {
    // "required" is not enforced for hooks, so check yourself.
    flux.logger.warn("webhookUrl is not configured; skipping notification");
    return;
  }
  const res = await flux.fetch({
    method: "POST",
    url,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (res.status >= 400) {
    flux.logger.warn("webhook answered HTTP", res.status);
  }
}

async function onDone(ctx) {
  flux.logger.info("finished", ctx.taskId, ctx.filePath);
  await send({
    text: "Downloaded " + baseName(ctx.filePath),
    taskId: ctx.taskId,
    source: ctx.url,
  });
}

async function onError(ctx) {
  if (flux.settings.notifyOnError !== true) return;
  // This fires for every failure, including ones FluxDown retries by itself.
  await send({
    text: "Download failed: " + ctx.message,
    taskId: ctx.taskId,
    source: ctx.url,
  });
}
```

需要注意：

- `flux.fetch` 只能访问公网的 `http`/`https` 地址。`localhost` 或局域网上的 webhook 会被有意拦截；请使用公网端点或中转服务。
- 默认 5 秒的钩子预算覆盖整个调用，包括 HTTP 往返。端点太慢会被中断，失败只会写进日志。
- `onError` 连会被重试的失败也会触发，所以网络不稳时可能连发多条消息。如果觉得吵，可以跳过 `ctx.message` 看起来是瞬时错误的消息，或者只通知 `onDone`。

## 示例 2：按条件重试

带签名的下载链接会过期。链接在下载中途过期时，服务器会返回 HTTP 403 或 410，FluxDown **不会**自动重试这两种状态（它只自动重试超时、连接重置和 HTTP 408/500/502/503/504）。resolver 插件是它的绝佳搭档：恢复任务会重新运行 resolver，拿到新的链接。下面这个钩子要做的正是请求这件事。

### `requestRetry` 与应用自带重试的关系

1. **宿主先决定。** 在 `onError` 里，如果宿主认为这次失败是瞬时的、并且还有自动重试额度，那么它在 `onError` 运行之前**已经**排好了自己的重试。此时为同一次失败调用 `requestRetry` 会被静默忽略（不会重复扣额度）。
2. **共用同一份额度。** 插件的重试与自动重试消耗同一个按任务计数的额度，即下载设置中的「失败重试次数」（`max_auto_retries`）。设为「关闭」（0）时，`requestRetry` 永远不会生效。额度用完后请求同样被忽略（应用日志里会记 `重试已达上限，忽略 requestRetry`）。任务成功完成后计数清零。
3. **「无限」就是无限。** 设置为「无限」时，没有任何东西能阻止插件无休止地重试，请像下面这样自己设一个上限。
4. **只对失败的任务有效。** 只有任务仍处于失败状态时请求才会被接受。
5. **重试就是恢复。** 不会再次触发 `onStart`；如果重试又失败了，会再次触发 `onError`。对 resolver 任务，resolver 会重新运行。
6. **延迟。** `delayMs` 是固定的毫秒数（默认 `0`）；宿主自己的自动重试则会随次数递增延迟。

`manifest.json`

```json
{
  "identity": "acme@expiry-retry",
  "name": "Expired Link Retry",
  "version": "1.0.0",
  "description": "Retries downloads that failed because a signed link expired.",
  "hooks": {
    "entry": "hooks.js",
    "events": ["onError"],
    "match": { "urls": ["*://files.example.com/*"] }
  },
  "settings": [
    {
      "key": "maxPluginRetries",
      "title": "Maximum plugin retries per task",
      "type": "number",
      "min": 0,
      "max": 10,
      "default": "2"
    },
    {
      "key": "delaySeconds",
      "title": "Delay before retrying (seconds)",
      "type": "number",
      "min": 0,
      "max": 600,
      "default": "3"
    }
  ]
}
```

`hooks.js`

```js
// Remember how often we retried each task. A single key keeps us far below the
// 100-key storage limit; only the 50 most recent tasks are kept.
async function bump(taskId) {
  const raw = await flux.storage.get("retries");
  const counts = raw ? JSON.parse(raw) : {};
  counts[taskId] = (counts[taskId] || 0) + 1;
  const ids = Object.keys(counts);
  if (ids.length > 50) delete counts[ids[0]];
  await flux.storage.set("retries", JSON.stringify(counts));
  return counts[taskId];
}

function looksExpired(message) {
  // Drop URLs first: a link may contain "403" by accident.
  const text = String(message).replace(/\S*:\/\/\S*/g, "");
  return /\b(403|410)\b/.test(text) || /expired/i.test(text);
}

async function onError(ctx) {
  if (!looksExpired(ctx.message)) return;

  const limit = flux.settings.maxPluginRetries ?? 2;
  const attempt = await bump(ctx.taskId);
  if (attempt > limit) {
    flux.logger.warn("giving up on", ctx.taskId, "after", limit, "retries");
    return;
  }

  const delayMs = (flux.settings.delaySeconds ?? 3) * 1000;
  flux.logger.info("retrying", ctx.taskId, "attempt", attempt, "in", delayMs, "ms");
  flux.task.requestRetry({ delayMs });
}
```

`requestRetry` 是同步的，没有返回值，所以无法得知宿主是否接受了请求。如果重试没有发生，请到应用日志里确认。

## 示例 3：用 ffmpeg 后处理

把下载完成的 `.mkv`/`.ts`/`.flv` 文件无损转封装成 `.mp4`（不重新编码），先用 `ffprobe` 看一眼再动手，最后把新文件登记为该任务的产物，这样它会随任务一起被删除。

`manifest.json`

```json
{
  "identity": "acme@mp4-remux",
  "name": "MP4 Remux",
  "version": "1.0.0",
  "description": "Remuxes finished mkv/ts/flv downloads to mp4 without re-encoding.",
  "hooks": {
    "entry": "hooks.js",
    "events": ["onDone"]
  },
  "permissions": ["ffmpeg"],
  "settings": [
    {
      "key": "extensions",
      "title": "File types to remux",
      "description": "Comma-separated extensions.",
      "type": "string",
      "default": "mkv,ts,flv"
    }
  ]
}
```

`hooks.js`

```js
async function onDone(ctx) {
  if (!flux.ffmpeg || !flux.ffprobe) return;

  const name = ctx.filePath.split(/[\\/]/).pop();
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return;
  const stem = name.slice(0, dot);
  const ext = name.slice(dot + 1).toLowerCase();

  const wanted = String(flux.settings.extensions || "")
    .split(",")
    .map((s) => s.trim().toLowerCase())
    .filter(Boolean);
  if (!wanted.includes(ext)) return;

  // The working directory is the folder of the finished file, so refer to files
  // by relative name. The "./" prefix keeps names that start with "-" safe.
  const input = "./" + name;
  const output = "./" + stem + ".mp4";

  const probe = await flux.ffprobe.run({
    args: [
      "-v", "error",
      "-select_streams", "v:0",
      "-show_entries", "stream=codec_name",
      "-of", "default=nw=1:nk=1",
      input,
    ],
    timeoutMs: 60000,
  });
  const codec = probe.stdout.trim();
  if (probe.code !== 0 || !["h264", "hevc"].includes(codec)) {
    flux.logger.info("skip remux, video codec is", codec || "unknown", name);
    return;
  }

  const run = await flux.ffmpeg.run({
    args: ["-i", input, "-c", "copy", "-movflags", "+faststart", "-y", output],
    timeoutMs: 20 * 60 * 1000,
  });
  if (run.code !== 0) {
    flux.logger.error("ffmpeg failed", run.timedOut ? "(timed out)" : "", (run.stderr || "").slice(-400));
    return;
  }

  // Tie the new file to the task: it is removed with "delete task and files".
  await flux.task.recordArtifact(stem + ".mp4");
  flux.logger.info("remuxed", name, "->", stem + ".mp4");
}
```

需要牢记的规则（完整表格见 [API 参考](/docs/zh/plugins/api-reference/)）：

- **权限与事件。** 只有 `permissions` 里有 `"ffmpeg"` 时才存在 `flux.ffmpeg`/`flux.ffprobe`，并且 `run` 只在 `onDone` 里能用。在别处会以 `flux.ffmpeg.run 仅在有产物文件的钩子（如 onDone）中可用` 拒绝。ffmpeg 还必须真正装好（设置 → 扩展 → 组件），否则报 `ffmpeg 未安装或不可用`；可以先用 `flux.ffmpeg.available()` 探测。ffprobe 随 ffmpeg 一起安装，缺失时报 `ffprobe 未安装或不可用（随 ffmpeg 组件一并安装）`。
- **牢笼目录。** 工作目录就是成品文件所在的文件夹，进程无法走出这个目录。请使用**相对**文件名。
- **参数筛查。** 启动前会逐个检查参数。被拒绝的有：URL 和协议前缀（`http://…`、`file:…`、`concat:…`）、绝对路径和盘符、`..` 路径段、跟在 `=` 或 `:` 后面内嵌的绝对路径（`subtitles=/etc/x`）、`tee` 复用器、从文件读取取值的选项（`-/…`、`-filter_script`、`-filter_complex_script`）、`-safe`，以及 `-protocol_whitelist`/`-protocol_blacklist`。`30000/1001`、`0:a`、`-c:v`、`scale=1280:720` 这类正常语法可以通过。被拒时的报错形如 `ffmpeg 参数 '<arg>' 被拒: <原因>`。`args` 不能为空，最多 512 项，每项最多 8 KB。
- **超时。** `timeoutMs` 默认 300 000（5 分钟），上限 1 800 000（30 分钟）；超时后进程被杀死，`timedOut` 为 `true`。整个钩子最长可以运行约 30 分钟。全局同时最多运行两个 ffmpeg/ffprobe 进程，批量处理时会排队。
- **输出。** `stdout` 截断到 256 KB，`stderr` 截断到 64 KB（见 `truncatedStdout`/`truncatedStderr`）。日志里只记 stderr 的末尾。
- **`recordArtifact(name)`** 接受一个裸文件名（不含 `/`、`\`、`:`，最长 512 个字符）。文件必须已经存在于任务所在的文件夹，且文件名必须**以任务主文件的 stem 开头**（`Show.mkv` → `Show…`），否则会被 `recordArtifact: 产物须以任务文件名主干 '…' 开头` 或 `recordArtifact: 产物文件不存在` 拒绝。

对于宿主合并失败的音视频分轨任务（`ctx.muxed === false` 且 `ctx.audioPath` 有值），也可以在 `onDone` 里自己用 `-c copy` 把 `filePath` 和 `audioPath` 合并。

## 示例 4：取消时清理

一个 resolver 先把分享文件转存到用户网盘里的临时位置，再返回网盘的下载链接。任务结束时——无论完成，还是因用户关闭选择对话框而暂停——这份临时副本都必须删除。`onCancel` 就是为此而设，成功路径则由 `onDone` 覆盖。（下面的网盘 API 是虚构的，重点在结构。）

`manifest.json`

```json
{
  "identity": "acme@drive-transfer",
  "name": "Drive Transfer",
  "version": "1.0.0",
  "description": "Downloads shared files through a temporary copy in your drive and removes it afterwards.",
  "resolvers": [
    {
      "match": { "urls": ["*://share.drive.example/*"] },
      "entry": "resolve.js"
    }
  ],
  "hooks": {
    "entry": "hooks.js",
    "events": ["onDone", "onCancel"],
    "match": { "urls": ["*://share.drive.example/*"] }
  },
  "settings": [
    {
      "key": "apiToken",
      "title": "API token",
      "type": "string",
      "widget": "password",
      "required": true
    }
  ]
}
```

`resolve.js`

```js
const API = "https://api.drive.example/v1";

async function loadTransfers() {
  const raw = await flux.storage.get("transfers");
  return raw ? JSON.parse(raw) : {};
}

async function saveTransfers(map) {
  const ids = Object.keys(map);
  while (ids.length > 50) delete map[ids.shift()];
  await flux.storage.set("transfers", JSON.stringify(map));
}

async function resolve(ctx) {
  const headers = { Authorization: "Bearer " + flux.settings.apiToken };
  const map = await loadTransfers();

  // A resume resolves again: drop the previous temporary copy first.
  if (map[ctx.taskId]) {
    await flux.fetch({ method: "DELETE", url: API + "/transfers/" + map[ctx.taskId], headers }).catch(() => {});
  }

  const res = await flux.fetch({
    method: "POST",
    url: API + "/transfers",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({ source: ctx.url }),
  });
  if (res.status !== 200) throw new Error("transfer failed: HTTP " + res.status);

  const transfer = JSON.parse(res.body);
  map[ctx.taskId] = transfer.id;
  await saveTransfers(map);
  return { url: transfer.downloadUrl, fileName: transfer.name, ephemeral: true };
}
```

`hooks.js`

```js
const API = "https://api.drive.example/v1";

async function release(taskId, why) {
  const raw = await flux.storage.get("transfers");
  const map = raw ? JSON.parse(raw) : {};
  const id = map[taskId];
  if (!id) return;

  const token = flux.settings.apiToken;
  if (!token) return;
  const res = await flux.fetch({
    method: "DELETE",
    url: API + "/transfers/" + id,
    headers: { Authorization: "Bearer " + token },
  });
  if (res.status < 300 || res.status === 404) {
    delete map[taskId];
    await flux.storage.set("transfers", JSON.stringify(map));
    flux.logger.info("released temporary copy", id, "after", why);
  } else {
    flux.logger.warn("could not release", id, "HTTP", res.status);
  }
}

async function onDone(ctx) {
  await release(ctx.taskId, "done");
}

async function onCancel(ctx) {
  // ctx.reason: user_cancelled | variant_selection_cancelled
  await release(ctx.taskId, ctx.reason || "cancel");
}
```

说明：

- `resolve.js` 和 `hooks.js` 是两个独立的脚本，各自重复了自己需要的小工具函数。
- **不要**在 `onError` 里释放：任务可能会被重试，重试时还需要同一份副本。
- `variant_selection_cancelled` 只是暂停任务；此时释放副本仍然正确，因为下次恢复会重新解析——也就是重新转存。
- 如果任务被删除，或任务运行期间 daemon 被停止，`onCancel` 不会被投递。如果泄漏的副本代价很高，请再加一个定期清扫。

## 示例 5：使用 yt-dlp

`flux.ytdlp` 运行的是 FluxDown 管理的 yt-dlp（依次查找用户设置的路径、托管安装、系统 `PATH`）。它需要 `permissions` 里有 `"ytdlp"`，并且已在 设置 → 扩展 → 组件 里装好 yt-dlp。没有该权限时 `flux.ytdlp` 是 `undefined`；yt-dlp 缺失时调用会以 `yt-dlp 未安装或不可用` 拒绝。

可用范围：`resolve`、**所有**钩子、`subscribe` 和 `main` 脚本——`authenticate` 里不可用。它的工作目录是每个插件独立的临时目录（与 `flux.fs` 共用），**不是**下载目录。实际使用中，yt-dlp 对插件来说是个*获取元数据*的工具：让它找到直链，再交给 FluxDown 去下载。钩子不能用 yt-dlp 把文件放到下载目录旁边。

### 允许哪些参数

规则是一份**白名单**，没列出的都会被拒绝，包括缩写（`--no-warn`）和连写的短选项（`-fbest`、`-Jq`）。被拒时的报错形如 `yt-dlp 参数被拒: 开关 '--x' 不在允许列表内`。

- **位置参数**必须是 `http://` 或 `https://` 的 URL（`位置参数只允许 http/https URL`）。裸的 `--` 之后的所有参数都按位置参数处理。
- **长选项**使用完整的规范名，取值可以放在下一个参数，也可以写成 `--name=value`。**短选项**只接受独立出现的参数，并且只能是下面列出的那些；需要取值的开关缺少取值时报 `开关 '…' 缺少值`。
- **不带值的开关：** `-J -j -q -v -s -x -F -i -4 -6 -k`，`--version --dump-json --dump-single-json --simulate --skip-download --quiet --verbose --no-warnings --no-progress --ignore-errors --no-abort-on-error --no-playlist --yes-playlist --flat-playlist --no-flat-playlist --lazy-playlist --playlist-reverse --force-ipv4 --force-ipv6 --geo-bypass --no-geo-bypass --no-check-certificates --check-formats --list-formats --live-from-start --no-live-from-start --no-mtime --restrict-filenames --windows-filenames --no-cache-dir --extract-audio --keep-video --no-keep-video --write-info-json --write-description --write-thumbnail --write-subs --write-auto-subs --no-write-subs --embed-subs --embed-thumbnail --embed-metadata --embed-chapters --no-part --no-continue --no-js-runtimes --mark-watched --no-mark-watched --hls-prefer-native --hls-use-mpegts --no-hls-use-mpegts`。
- **带一个值的开关：** `-f -S -N -r -R`，`--format --format-sort --extractor-args --playlist-items --playlist-start --playlist-end --max-downloads --proxy --socket-timeout --source-address --geo-verification-proxy --geo-bypass-country --geo-bypass-ip-block --xff --user-agent --referer --add-header --sleep-interval --min-sleep-interval --max-sleep-interval --sleep-requests --sleep-subtitles --retries --extractor-retries --fragment-retries --file-access-retries --retry-sleep --concurrent-fragments --limit-rate --throttled-rate --http-chunk-size --buffer-size --min-filesize --max-filesize --date --datebefore --dateafter --match-filters --match-filter --age-limit --download-sections --merge-output-format --remux-video --recode-video --audio-format --audio-quality --sub-langs --sub-format --convert-subs --convert-thumbnails --impersonate --print --parse-metadata --compat-options --video-password --username --password --twofactor --encoding --output-na-placeholder`。
- **运行时选择：** `--js-runtimes <name>` 只接受不带路径的运行时名（字母、数字、`,`、`_`、`-`）。
- **文件路径类开关**——`--cookies`、`--cache-dir`、`--download-archive`、`-o`/`--output`、`-P`/`--paths`，以及 `--print-to-file <模板> <文件>` 的第二个参数——只接受临时目录内的相对路径。绝对路径、盘符、`..`、`~`、`$VAR` 和 `%VAR%` 都会被拒绝（`开关 '…' 的值被拒: …`）。`-o`/`-P` 可以带 `home:`、`temp:` 这样的类型前缀。
- **一律不允许：** 任何会运行程序、加载配置或插件、读取其他凭据的开关，例如 `--exec`、`--exec-before-download`、`--downloader`、`--external-downloader`、`--config-locations`、`--plugin-dirs`、`--ffmpeg-location`、`--batch-file`、`--load-info-json`、`--netrc`、`--cookies-from-browser`、`--use-postprocessor`、`--alias`。
- 其他限制：最多 512 个参数，每个最长 8 KB，不能含 NUL 字节；`args` 不能为空。

宿主会在你的参数之前自动加上 `--ignore-config`、`--ffmpeg-location`（指向它自己的 ffmpeg）和 `--cache-dir <临时目录>/.cache`（你自己传的 `--cache-dir` 会覆盖它，但仍限制在该目录内）；如果用户在 FluxDown 里配置了代理，还会在你的参数之后追加 `--proxy`，此时它优先于你自己写的。`timeoutMs` 默认 300 000，上限 3 600 000；调用还受脚本自身墙钟限制——resolver 里是 resolver 自己的预算（默认 10 秒，最多 120 秒），钩子里约 30 分钟。全局同时最多运行两个 yt-dlp 进程。`stdout` 默认截断到 16 MB（用 `truncatedStdout` 判断），`stderr` 截断到 256 KB。完整细节见 [API 参考](/docs/zh/plugins/api-reference/)。

### 一个用 yt-dlp 的 resolver，加一个负责重试的钩子

`manifest.json`

```json
{
  "identity": "acme@ytdlp-resolver",
  "name": "yt-dlp Resolver",
  "version": "1.0.0",
  "description": "Asks yt-dlp for a direct link and lets FluxDown download it.",
  "resolvers": [
    {
      "match": { "urls": ["*://www.youtube.com/watch*", "*://youtu.be/*"] },
      "entry": "resolve.js",
      "timeoutMs": 30000
    }
  ],
  "hooks": {
    "entry": "hooks.js",
    "events": ["onError"],
    "match": { "urls": ["*://www.youtube.com/watch*", "*://youtu.be/*"] }
  },
  "permissions": ["ytdlp"],
  "settings": [
    {
      "key": "format",
      "title": "yt-dlp format selector",
      "type": "string",
      "default": "best[ext=mp4]/best"
    }
  ]
}
```

resolver 预算默认是 10 秒；`timeoutMs: 30000` 把它提高到 30 秒（上限是 120 秒；FluxDown 0.6.8 及更早版本钳在 30 秒），这样给 yt-dlp 的 25 秒（见下）才有空间。

`resolve.js`

```js
function safeName(title, ext) {
  const clean = String(title || "video")
    .replace(/[\\/:*?"<>|\u0000-\u001f]/g, "_")
    .replace(/\.{2,}/g, ".")
    .trim()
    .slice(0, 150);
  return clean + "." + (ext || "mp4");
}

async function resolve(ctx) {
  if (!flux.ytdlp) throw new Error("the ytdlp permission is not granted");

  const run = await flux.ytdlp.run({
    args: ["-J", "--no-playlist", "--no-warnings", "-f", flux.settings.format || "best", ctx.url],
    timeoutMs: 25000,
  });
  if (run.timedOut) throw new Error("yt-dlp timed out");
  if (run.code !== 0) throw new Error("yt-dlp failed: " + (run.stderr || "").slice(-300));
  if (run.truncatedStdout) throw new Error("yt-dlp output was truncated");

  const info = JSON.parse(run.stdout);
  const headers = info.http_headers || {};

  // Separate video + audio streams (e.g. "bv*+ba"): hand both to FluxDown.
  const [video, audio] = info.requested_formats || [];
  if (video && audio) {
    return {
      url: video.url,
      audioUrl: audio.url,
      fileName: safeName(info.title, info.ext),
      extraHeaders: headers,
      ephemeral: true,
    };
  }

  if (!info.url) throw new Error("yt-dlp returned no direct URL");
  return {
    url: info.url,
    fileName: safeName(info.title, info.ext),
    extraHeaders: headers,
    ephemeral: true,
  };
}
```

`hooks.js`

```js
// Signed links expire; FluxDown does not retry HTTP 403/410 on its own, but
// resuming runs the resolver again and fetches a fresh link (see Example 2).
async function onError(ctx) {
  const text = String(ctx.message).replace(/\S*:\/\/\S*/g, "");
  if (/\b(403|410)\b/.test(text)) {
    flux.task.requestRetry({ delayMs: 2000 });
  }
}
```

## 排错

| 现象 | 可能的原因 |
|---|---|
| 钩子根本没有运行 | 事件没有写进 `hooks.events`；`match.urls` 没匹配上（别忘了 `onStart` 看到的是解析后的 URL）；插件被禁用或在等待授权；函数不存在，或者是用 `const` 声明的。 |
| `onMetaProbed` 从不触发 | 任务带 resolver、立即开始、是 BitTorrent，或者带有大小提示。 |
| `requestRetry` 没效果 | 在 `onError` 之外调用；「失败重试次数」是关闭或已用完；宿主已经为这次失败排好了自己的重试。 |
| `ffmpeg` 是 `undefined` | 缺少 `ffmpeg` 权限，或者权限没有被授予。 |
| ffmpeg 报 `…被拒…` | 某个参数是绝对路径、URL、`..` 或其他被筛查的语法——请使用带 `./` 的相对文件名。 |
| 日志里出现警告 `钩子并发已满，本次钩子被丢弃` | 同时运行的短钩子太多；让钩子更轻量，或者让它走带权限的池。 |

更多内容见[调试与排错](/docs/zh/plugins/debugging/)。如果想用带长期状态的方式响应事件，或者把事件显示在视图里，请继续阅读[可移植界面指南](/docs/zh/plugins/ui-guide/)；登录相关见[认证指南](/docs/zh/plugins/auth-guide/)。仓库里有更完整的示例：[feature-demo](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/feature-demo) 和 [youtube-down](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/youtube-down)。
