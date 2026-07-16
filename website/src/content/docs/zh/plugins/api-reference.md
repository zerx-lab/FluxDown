---
title: 插件 API 参考
description: 入口函数签名、flux.* 完整接口、全部运行时限制。
section: plugins
order: 4
sourceHash: "4501af5b1d55"
---

插件脚本能看到的一切：FluxDown 会调用的五个入口函数，和注入的 `flux` 对象。跨越 JS 边界的字段名全部是 camelCase。

## 入口函数

入口就是普通的全局函数。`async` 函数和返回 Promise 都完全支持——FluxDown 会等待结果。

### `resolve(ctx)`

在协议分派之前调用，匹配任务的**每次**开始和恢复都会执行（惰性解析，见[概览](/docs/zh/plugins/overview/)）。

`ctx` 字段：

| 字段 | 类型 | 含义 |
|---|---|---|
| `taskId` | string | 任务 UUID。 |
| `url` | string | 任务的原始 URL（永远不是上次解析的结果）。 |
| `cookies` | string | 任务附带的 Cookie 值。 |
| `referrer` | string | 任务附带的 Referrer。 |
| `userAgent` | string | 生效的 User-Agent。 |
| `extraHeaders` | object | 额外请求头，字符串键值。 |

返回 `null` 或 `undefined` 表示放行（FluxDown 按 `ctx.url` 原样下载）。否则返回一个对象，除 `url` 外都可选：

| 字段 | 类型 | 含义 |
|---|---|---|
| `url` | string | 改写后的直链。空字符串表示保留原 URL。 |
| `audioUrl` | string | 独立音频流直链，用于 DASH 式音视频分离。 |
| `fileName` | string | 覆盖保存的文件名。 |
| `totalBytes` | number | 文件大小（字节），已知的话。 |
| `extraHeaders` | object | 下载解析后直链时附带的请求头。 |
| `ephemeral` | boolean | `true` = 直链是一次性的/有防盗链：跳过元数据探测（代价是续传一致性校验变弱）。默认 `false`：正常探测并保留基于 ETag 的续传校验。 |
| `rangeSupported` | boolean | `true` = 你担保解析后的服务支持 HTTP Range 请求（如 googlevideo）。与 `ephemeral` 组合时，FluxDown 依旧跳过探测，但直接按多线程分段规划下载，而不是保守的单流启动。默认 `false`：没有探测时，Range 能力只能从首个响应学习。 |
| `variants` | array | 多个画质/格式选项。存在且多于一项时，FluxDown 弹出选择对话框，用户选中后在下载前收敛为单一直链（headless 服务器或免打扰下载场景直接静默使用 `defaultVariantIndex`，与 HLS 画质选择完全一致）。每项：`{ label, url, audioUrl?, fileName?, totalBytes?, bandwidth?, width?, height?, container? }`——`label` 和 `url` 必填。`variants` 非空时顶层 `url` 允许为空。最多 50 项，每个 `label` ≤ 200 字符。 |
| `defaultVariantIndex` | number | 默认变体索引（60 秒超时 / 免打扰 / headless 时使用）。越界回退为 `0`。默认 `0`。 |

解析完成后，FluxDown 会用**解析后的** URL 重新判定协议引擎——resolver 可以返回 HLS 播放列表、磁力链接或 FTP 地址，对应引擎会自动接管。

错误行为是 fail-closed：抛异常、超时、返回值不合法、插件已卸载或被禁用，任务都进入错误状态。原始 URL 绝不会被悄悄下载。

### `onStart(ctx)` / `onDone(ctx)` / `onError(ctx)` / `onMetaProbed(ctx)`

通知钩子。都会收到 `{ event, taskId, url }`，再加各自的字段：

| 事件 | 额外字段 |
|---|---|
| `onStart` | — |
| `onError` | `message`——任务的错误文本 |
| `onDone` | `filePath`——完成文件的绝对路径；`audioPath`——轨对任务（视频+音频离散轨）mux 失败降级时独立音频文件（`<主干名>.audio.m4a`）的绝对路径，单文件产物（含 mux 成功）为 `null`；`muxed`——轨对任务是否已成功合并为单文件，非轨对任务恒 `false` |
| `onMetaProbed` | `fileName`、`totalBytes`——探测结果 |

`url` 恒为任务的原始 URL，manifest 里 `hooks.match.urls` 也是拿它过滤的。

钩子发出后不管结果：异常和超时只记日志然后吞掉，插件运行时忙不过来时通知直接丢弃。钩子做的任何事都改变不了任务——唯一例外是 `flux.task.requestRetry`，且只在 `onError` 里有效。

## `flux` 对象

### `flux.fetch(opts)` → `Promise<response>`

HTTP 客户端。`opts`：

| 字段 | 默认 | 说明 |
|---|---|---|
| `method` | `"GET"` | |
| `url` | — | 必填。 |
| `headers` | `{}` | 字符串键值。 |
| `body` | 无 | 请求体，仅文本。 |

resolve 出 `{ status, headers, body, truncated }`——`status` 是数字状态码，`body` 是文本（v1 不支持二进制响应），`truncated` 为 `true` 时表示响应体触顶被截断。网络失败和守卫拦截都会 reject。

安全护栏，全部在宿主侧强制：

| 规则 | 值 |
|---|---|
| 协议 | 仅 `http` / `https` |
| 目标地址 | 仅允许公网可路由地址。环回、局域网、link-local、云元数据 IP 全部拦截——对 URL 字面量、DNS 解析结果、每一跳重定向都检查。 |
| 响应体上限 | 8 MB，超出截断 |
| 单请求超时 | 10 秒 |
| 并发请求数 | 8，全部插件共享 |
| 最大重定向 | 30 跳 |

### `flux.storage`

插件私有的持久化键值存储，应用重启后仍在（存在 FluxDown 数据库里）。

- `flux.storage.get(key)` → `Promise<string | null>`
- `flux.storage.set(key, value)` → `Promise<void>`——单个值超过 **64 KB**，或插件键数将超过 **100 个**时 reject。

值只能是字符串；结构化数据自己 JSON 序列化。

### `flux.fs`

始终可用——无需在 manifest 里声明权限。插件自己的临时文件工作区，和 `flux.storage` 的键值存储是两码事：需要给受管工具一个磁盘上的真实文件（而不是内存里的字符串）时用它。这个工作区就是 `flux.ytdlp` 运行时的**同一个目录**（其 cwd）——写在这里的文件，yt-dlp（或任何工具调用）都能用普通相对文件名读到。

- `flux.fs.writeFile(name, content)` → `Promise<void>`——写入（或覆盖）一个文本文件；文件名非法或触发限额时 reject（抛异常）。
- `flux.fs.readFile(name)` → `Promise<string | null>`——读回文本文件；不存在则为 `null`。
- `flux.fs.remove(name)` → `Promise<void>`——删除文件；文件不存在不算错误（幂等）。
- `flux.fs.list()` → `Promise<string[]>`——工作区顶层的文件名列表（不含子目录，也不含 yt-dlp 自己的 `.cache`）。

`name` 必须是扁平的安全文件名：非空、不含 `/`、`\`、`:`，不是 `.` 或 `..`，且不超过 255 个字符——否则抛异常。限额：单文件 **8 MB**，单插件工作区总量 **64 MB**，文件数至多 **100** 个。内容仅支持文本（不支持二进制）。Unix 下写入会尽力设为 `0600`。

典型用法：为受管工具物化输入文件——cookie、配置、字幕——写入后以相对名喂给工具调用，用完删除。例子——给 `flux.ytdlp` 喂一份 cookie：

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

只读对象，装着 manifest 里声明的设置项，类型已经转好：`string` 项是字符串，`number` 是数字，`boolean` 是布尔。用户没填的项带着 `default` 值。

### `flux.info`

`{ identity, version, appVersion }`——插件自己的 ID 和版本，以及承载它的 FluxDown 版本。

### `flux.logger` 与 `console`

`flux.logger.info/warn/error(...)` 写入 FluxDown 日志文件。`console.log/info/warn/error/debug` 映射到同一处（`debug` 按 info 级别记）。多个参数用空格连接，非字符串会 JSON 序列化。每条日志截断在 4 KB。

### `flux.task.requestRetry(opts)`

`flux.task.requestRetry({ delayMs: 5000 })`——请求 FluxDown 在延迟后重试失败的任务。只在 `onError` 里有意义；其他地方调用只记一条警告，什么也不做。重试消耗任务自己的自动重试额度，插件无法无限重试。

### `flux.ffmpeg`

**仅当** manifest 声明 `permissions: ["ffmpeg"]` 时可用——否则 `flux.ffmpeg` 为 `undefined`，请用 `if (flux.ffmpeg)` 判断。它运行 FluxDown 解析到的 ffmpeg（用户手动指定的路径 → 托管安装 → 系统 `PATH`），因此还要求 ffmpeg 确实存在（可在应用「设置 → 扩展 → 组件」页安装）。

- `flux.ffmpeg.available()` → `Promise<{ available, version, source }>`——探测生效的 ffmpeg。`source` 取 `"manual"` / `"managed"` / `"system"` / `"none"`。
- `flux.ffmpeg.run(spec)` → `Promise<outcome>`——运行 ffmpeg。`spec`：

| 字段 | 默认 | 说明 |
|---|---|---|
| `args` | — | 必填，非空。ffmpeg 参数数组（不含程序名；`-nostdin` 会自动前置）。 |
| `subdir` | 无 | 沙箱根目录下的工作子目录；安全相对路径，不得逃逸。 |
| `timeoutMs` | 300000 | 单次调用超时，上限 1800000（30 分钟）。 |

resolve 出 `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }`——`code` 是退出码（被杀死/无码时为 `-1`），`stdout`/`stderr` 会截断（256 KB / 64 KB），`timedOut` 为 `true` 表示被超时杀掉。

**沙箱隔离。** `flux.ffmpeg` 只在 `onDone`（唯一有产物文件的钩子）里可用；`resolve` 和其他事件里调用会 reject。工作目录就是完成文件所在的目录，该目录即沙箱边界——文件一律用**相对**名（basename）引用，名字可能以 `-` 开头时前缀 `./`。

参数会被审查，出现以下 token 时拒绝启动：

| 拦截 | 例子 |
|---|---|
| URL scheme / 协议 | `http://…`、`file:…`、`concat:…`、`crypto:…` |
| 绝对路径 / 盘符 | `/etc/x`、`C:\x`、`\\host\share` |
| 上级穿越 | `../x`、`a/../b` |
| 内嵌绝对路径 | `subtitles=/etc/x` |

正常的 ffmpeg 语法不受影响——除法（`30000/1001`）、流选择器（`0:a`、`-c:v`）、滤镜（`scale=1280:720`）都放行。既无 URL 也够不到绝对路径，ffmpeg 只能访问沙箱内的文件，因此也没有网络出口。全部插件合计同时至多跑 2 个 ffmpeg 进程，每个子进程在超时或取消时被杀。

例子——在 `onDone` 里把非 MP4 产物转为 MP4：

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
  if (r.code !== 0) flux.logger.error('转码失败', (r.stderr || '').slice(-400));
};
```

### `flux.ffprobe`

和 `flux.ffmpeg` 由同一个 `permissions: ["ffmpeg"]` 声明门控——不存在独立权限。它共享完全相同的沙箱（**仅**在 `onDone` 里可用，其他事件里调用会 reject）、相同的路径解析顺序（用户手动指定 → 托管安装 → 系统 `PATH`），以及相同的参数审查（不许 URL scheme、不许绝对路径、不许上级穿越、不许内嵌绝对路径）。ffprobe 随托管 ffmpeg 一并安装——在「设置 → 扩展 → 组件」页安装 ffmpeg 时会同时把 ffprobe 放进 `<data_dir>/bin`，无需单独安装。

- `flux.ffprobe.run(spec)` → `Promise<outcome>`——运行 ffprobe。`spec` 与 resolve 出的 `outcome` 形状与 `flux.ffmpeg.run` 完全一致（`args` / `subdir` / `timeoutMs`，resolve 出 `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }`）。

用它做结构化探测，而不是去抠 ffmpeg 的 stderr：

```js
const out = await flux.ffprobe.run({
  args: ['-v', 'quiet', '-print_format', 'json', '-show_format', '-show_streams', './in.mp4'],
});
const info = JSON.parse(out.stdout);
```

### `flux.ytdlp`

**仅当** manifest 声明 `permissions: ["ytdlp"]` 时可用——否则 `flux.ytdlp` 为 `undefined`，请用 `if (flux.ytdlp)` 判断。它运行 FluxDown 解析到的 yt-dlp（用户手动指定的路径 → 托管安装 → 系统 `PATH`），因此还要求 yt-dlp 确实存在（可在应用「设置 → 扩展 → 组件」页安装）。

- `flux.ytdlp.available()` → `Promise<{ available, version, source }>`——探测生效的 yt-dlp。`source` 取 `"manual"` / `"managed"` / `"system"` / `"none"`。想轻量探测也可以直接 `run({ args: ['--version'] })` 看 `code === 0`。
- `flux.ytdlp.run(spec)` → `Promise<outcome>`——运行 yt-dlp。`spec`：

| 字段 | 默认 | 说明 |
|---|---|---|
| `args` | — | 必填，非空。yt-dlp 参数数组（不含程序名；`--ignore-config` 会自动前置）。 |
| `subdir` | 无 | 沙箱根目录下的工作子目录；安全相对路径，不得逃逸。 |
| `timeoutMs` | 300000 | 单次调用超时，上限 3600000（60 分钟）。 |

resolve 出 `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }`——`code` 是退出码（被杀死/无码时为 `-1`），`stdout`/`stderr` 会截断（256 KB / 64 KB），`timedOut` 为 `true` 表示被超时杀掉。

**沙箱隔离。** 和 `flux.ffmpeg` 不同，`flux.ytdlp` 在**所有**上下文里都可用——`resolve` 和每个 hook——因为它不依赖产物文件。沙箱根目录不是任务的产物目录，而是 bridge 自持的每插件 scratch 目录（懒创建于 FluxDown 数据目录下），跨调用复用。它就是本次调用的工作目录，`subdir` 在其中划出子目录。读写的文件一律用**相对**名引用。这正是 `flux.fs` 读写的同一个工作区——喂给 yt-dlp cookie、配置或字幕文件的方式就是：调用前 `flux.fs.writeFile('cookies.txt', …)`，在 `args` 里以相对名引用该文件，调用结束后 `flux.fs.remove('cookies.txt')`。

yt-dlp 是网络工具，和 ffmpeg 的沙箱不同，URL 参数与出站网络访问都放行——从远程 URL 提取正是它的本职。会被拦的是那些能跳出 yt-dlp 本身或逃出沙箱的东西：

| 拦截 | 例子 |
|---|---|
| 绝对路径 / 盘符 | `/etc/x`、`C:\x`、`\\host\share` |
| 上级穿越 | `../x`、`a/../b` |
| 内嵌绝对路径 | `--paths home:/etc/x` |
| `file:` 本地方案 | `file:///etc/passwd` |
| 会执行外部程序 / 加载任意配置或插件 / 读浏览器凭据的开关 | `--exec`、`--exec-before-download`、`--downloader`、`--external-downloader`、`--config-location`/`--config-locations`、`--plugin-dirs`、`--ffmpeg-location`、`--batch-file`、`-a`、`--load-info`/`--load-info-json`、`--cookies-from-browser` |

`--ignore-config` 恒被前置注入，这样 yt-dlp 自己的配置文件（本可能夹带危险开关）也读不到。全部插件合计同时至多跑 2 个 yt-dlp 进程，每个子进程在超时或取消时被杀。

FluxDown 会自动注入 `--ffmpeg-location`（指向解析到的托管/系统 ffmpeg），使合并（bestvideo+bestaudio）、`-x` 抽音、remux、recode 都能正常工作——插件自带的 `--ffmpeg-location` 仍会被拒（见上表），只信任宿主自己注入的路径。它还会自动注入 `--cache-dir <jail>/.cache`，把 yt-dlp 的缓存收在沙箱内，不外泄。

例子——向 yt-dlp 要元数据 JSON，从中挑一条直链来 resolve：

```js
globalThis.resolve = async (ctx) => {
  if (!flux.ytdlp) return null;
  const r = await flux.ytdlp.run({ args: ['-J', '--no-warnings', ctx.url] });
  if (r.code !== 0) throw new Error('yt-dlp 失败: ' + (r.stderr || '').slice(-400));
  const info = JSON.parse(r.stdout);
  const direct = info.url || info.formats?.[info.formats.length - 1]?.url;
  if (!direct) return null;
  return { url: direct, fileName: info.title ? `${info.title}.${info.ext || 'mp4'}` : undefined };
};
```

## 运行时限制

每次调用都在全新的 QuickJS 上下文里跑：调用之间没有任何全局变量残留，没有定时器和 DOM API，脚本按 classic script 加载（顶层 `function` 声明自动成为全局函数；`export` 语法不能用）。

| 预算 | resolve | hooks |
|---|---|---|
| 超时 | 10 秒（manifest `timeoutMs` 可改，30 秒硬顶） | 5 秒 |
| 内存 | 64 MB | 32 MB |

连续 3 次超时或内存超限会触发熔断：插件被自动禁用，应用弹出提示，直到手动重新启用为止。

获得 `permissions: ["ffmpeg"]`（且命中产物钩子，即 `onDone`）或 `permissions: ["ytdlp"]`（任意 hook）的插件会拿到抬高的墙钟预算（约 30 分钟），好让长时外部工具跑完；30 秒 CPU 顶仍约束 JavaScript 本身——等待子进程的时间不计入。`resolve` 始终用自己的预算（默认 10 秒，`timeoutMs` 可改，30 秒硬顶），即便 `flux.ytdlp` 在那里也能用——长任务请按此规划 `run()` 调用。
