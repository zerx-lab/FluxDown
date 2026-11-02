---
title: API 总览
description: FluxDown 的 HTTP API——五组路由、鉴权方式,以及它与 headless 服务器的关系。
section: api
order: 1
sourceHash: "5361a8e27802"
---

FluxDown 的 HTTP API 供扩展、用户脚本、aria2 客户端与自动化工具使用；不要把 aria2 的 `/jsonrpc` 与官方客户端的 `/rpc` 混为一谈。

- **桌面客户端**默认监听 `127.0.0.1:17800`。GPUI 的本地服务设置可开启局域网访问（`local_server_lan_enabled`），改为 `0.0.0.0`，下次启动生效；并非永远只监听回环。管理 API 与 MCP 默认关闭，脚本接管与 aria2 默认开启。
- **[headless 服务器](/docs/zh/headless-server/setup/)**由同目录的 `fluxdown-agent --server` 与 `fluxdownd` 提供，监听地址只由 `FLUXDOWN_BIND` 决定（默认 `[::]:17800`，IPv4 + IPv6）。全新安装默认开启兼容分组，但**未设置访问密钥不等于匿名开放**；Web 界面经 agent 的 `/rpc` 管理下载。

## 五组路由

| 分组 | 端点 | 开关 | 鉴权 |
|---|---|---|---|
| 探活 | `GET /ping` | 总开关 | 无 |
| 脚本接管 | `POST /download`、`POST /download/batch` | `local_server_takeover_enabled`（默认开） | 非空 `X-FluxDown-Client` 头；已配置 token 时必须携带 |
| aria2 兼容 RPC | `POST /jsonrpc`、`GET /jsonrpc`（WebSocket） | `local_server_jsonrpc_enabled`（默认开） | 已配置 token 时逐调用校验（名单方法除外） |
| 管理 API | `/api/v1/*`（任务、队列等） | `local_server_api_enabled`（桌面默认关，全新 headless 默认开） | **强制** token |
| MCP | `POST /mcp` | `local_server_mcp_enabled`（桌面默认关，全新 headless 默认开） | **强制** token（与管理 API 共用） |

headless 首次设置完成前，`/download`、`/download/batch` 与 `/jsonrpc`（POST 和 WS 升级）返回 **HTTP 403**，消息为 `setup required: set the access key first`，名单方法也不能绕过；管理 API 与 MCP 同样拒绝空密钥。Web 页面、探活与首次设置接口仍可访问（启动就绪前 setup 接口可能返回 503）。完成初始化或用合规的 `FLUXDOWN_TOKEN` 预置密钥后，兼容入口才按各自开关工作。

管理分组开启时可取 `GET /api/v1/openapi.json`（免鉴权的纯接口描述）。旧 `fluxdown-server` 的扩展 REST（`/api/v1/config`、队列增删改、stats、fs/list、components、webhooks、logs、`/api/v1/ws`、`/api/v1/token/regenerate`）已不在新宿主提供；请使用 Web 界面或 `/rpc`。免鉴权的 `GET /api/v1/setup/status` 与 `POST /api/v1/setup` 仅负责首次初始化，后者只在密钥尚未设置时接受。

BT 防吸血路由位于同一管理分组的 `/api/v1/bt/guard/{state,entries,events}`,见 [BT 防吸血](/docs/zh/protocols/bt-guard/#rest-api)。

## 鉴权方式

兼容 API 与 Web 共用用户访问密钥，agent 持久化在 `gateway_user_token`；从旧宿主迁移的 `local_server_token` 会导入它。这与内部的 `agent.token`、`daemon.token` 不是同一凭据。

| 路由组 | 接受的形式 |
|---|---|
| 脚本接管 | `X-FluxDown-Token`；始终还需非空 `X-FluxDown-Client`。仅未对外暴露且未配置 token 的桌面场景可匿名使用，headless 空密钥一律拒绝。 |
| aria2 兼容 RPC | POST 可用 `X-FluxDown-Token` 或 `params[0]="token:xxx"`；WS 每个调用只能在 params 中携带 token。`system.listMethods` / `system.listNotifications` 免逐调用 token 校验，但仍受来源与首次设置门禁。 |
| 管理 API / MCP | `Authorization: Bearer <token>` 或 `X-FluxDown-Token`；空密钥返回 403。 |
| agent `/rpc`（WebSocket） | Bearer；浏览器用 `Sec-WebSocket-Protocol: fluxdown.rpc.v1, fluxdown.token.<base64url(token)>`（无 padding）。带 Origin 的浏览器连接必须与服务同源。 |
| headless `/api/web/files/tasks/{id}`、`/api/web/exports/{id}` | Bearer 或 `?token=<token>`，用于不能自定义请求头的浏览器文件下载。 |

桌面开启 LAN 或 CORS、同时开启脚本接管或 aria2 时，agent 会为**空密钥自动生成并持久化随机 token**；已有密钥不覆盖，暴露面仍在时清空也会补回。启动与旧配置迁移后同样补齐。外部工具需同步配置该 token。headless 不自动补密钥，以免跳过首次设置向导。

### 跨域与 Host 校验

默认不返回 `Access-Control-Allow-Origin`。接管所需的自定义头会触发 CORS 预检；`/jsonrpc` 的简单 POST 与 WS 不触发预检，因此服务端还主动校验 Origin：`/jsonrpc`（POST/WS）和 `/download*` 只放行扩展源（`chrome-extension://`、`moz-extension://`、`safari-web-extension://`）或与 Host 同源的请求，其他浏览器跨源请求返回 403。不带 Origin 的 CLI、aria2 客户端和 `GM_xmlhttpRequest` 不受此来源门禁影响，仍须满足 token 要求。

桌面仅回环监听时，核心 API 的 Host 还必须为 `127.0.0.1`、`localhost` 或 `[::1]`（可带端口），防 DNS 重绑定；LAN/headless 模式不强制回环 Host。此限制不会因打开 CORS 而取消。

**允许任意网页跨域访问（CORS）**（`local_server_cors_allow_all`，默认关）会发出 `Access-Control-Allow-Origin: *`，预检额外允许私有网络访问并回显请求头，同时放开兼容入口的 Origin 门禁。代价是任意网页可探测本机服务，并在获得 token 后调用兼容入口；token 校验不会取消。aria2 与管理 API 直接建任务、不弹确认框，不能把桌面脚本接管的确认框视作普遍保护。

## 接管与直接建任务

`/download*` 进入外部下载流程：桌面可以弹确认框，也可按免打扰设置静默创建；headless 没有确认窗口，鉴权通过后直接创建。`aria2.addUri` / `aria2.addTorrent` 与管理 API `POST /api/v1/tasks` 始终直接建任务，不经过确认通道。**首次设置前 headless 接管和 aria2 都不可用**。

## 批量 RPC 与慢方法

aria2 `/jsonrpc` 支持顶层 JSON 数组，按顺序执行并返回对应的响应数组；`system.multicall` 支持子调用集合。每个需鉴权的子调用都要在自己的 params 头部放 `token:xxx`（POST 也可共用有效 token 头），只在 multicall 外层放 token 不够；不允许嵌套 multicall。

agent/daemon 的 `/rpc` 一帧只接受一个请求对象，不接受上述顶层数组；可发送多个不同 ID 的请求并按 ID 匹配响应。慢 daemon 方法由 `native/protocol/src/method.rs::SLOW_DAEMON_METHODS` 共享清单统一识别：daemon 有界并发处理，agent 放到独立慢通道，避免组件安装、网络探测等堵住暂停/恢复与设置写入；普通 daemon 命令仍按通道顺序执行。

大量本地任务的批量操作应优先用单个 `daemon.task.pauseMany {taskIds}`、`daemon.task.resumeMany {taskIds}` 或 `daemon.task.deleteMany {taskIds, deleteFiles}`，而不是逐项并发提交单任务请求。daemon 去重并忽略不存在的 ID；空列表为空操作。整批处理后统一推任务快照，避免批量 UI 操作产生逐任务快照洪峰。
## 内部 daemon ↔ agent 认证

这不是外部 API 的登录方式。新版 agent 在不带凭据的 WS 升级后，以 `system.auth.challenge` / `system.auth.prove` 双向挑战应答先验证 daemon，再证明自己持有长期密钥，随后才发 `system.hello`；长期 `daemon.token` 不上线。blob、文件与导出 HTTP 请求只使用两端各自派生的会话 Bearer，连接断开即撤销；没有已认证会话不回退长期 token。

daemon 仍接受旧常驻 agent 的有效静态 Bearer（WS 升级与 HTTP），用于二进制已升级但旧进程尚未退出的兼容窗口；新版 agent **不会**向旧 daemon 降级发送长期 token。此兼容不等于配对协议允许降级。

## 局域网配对 v2

配对使用一次性 6 位码（有效 120 秒）及双端 SAS 核对，不是分享用户 API token。握手必须依次经过 `/api/v1/link/pair/hello` → `/api/v1/link/pair/reveal` → `/api/v1/link/pair/confirm`：

1. hello 带 `protocolVersion: 2` 与临时公钥/随机数的 SHA-256 **承诺**，不直接揭示发起方临时值；响应方先返回本次会话的临时值。
2. reveal 揭示承诺中的值。核对承诺并验证完整转录的身份签名后，两端分别计算并展示 6 位 SAS；SAS 不作为字段在网络上传递。承诺不符或揭示超过 30 秒会作废会话，不能重放旧 reveal 或跳过此步 confirm。
3. 双方用户肉眼核对 SAS 并批准，响应方才放行登记；配对码本身不提供抗中间人保证，不能略过 SAS 核对。

两端协议版本必须严格相等，缺版本按 0 拒绝；**不兼容旧版无承诺握手，不做降级**。配对的 hello/reveal/confirm 不走管理用户 token 校验，而由一次性码、承诺、会话与本机批准保护；配对失败提示版本不兼容时应升级两端。

## curl 示例

直接创建任务(管理 API):

```bash
curl -X POST http://<host>:17800/api/v1/tasks \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com/file.zip","segments":8}'
# -> {"taskId":"..."}
```

查询任务列表:

```bash
curl http://<host>:17800/api/v1/tasks \
  -H "Authorization: Bearer <token>"
```

用 aria2 兼容 RPC 添加下载(现成的面向 aria2 的油猴脚本/客户端可直接工作):

```bash
curl -X POST http://<host>:17800/jsonrpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": "1",
    "method": "aria2.addUri",
    "params": ["token:<token>", ["https://example.com/file.zip"]]
  }'
```

`CreateTaskRequest` 接受必填的 `url`,以及可选的 `fileName`、`saveDir`、`segments`、`cookies`、`referrer`、`proxyUrl`、`userAgent`、`queueId`、`checksum`(`algo=hexhash`)与 `headers`——JSON body 全部为 camelCase。传入的 `fileName` 会被清洗(剥除路径分隔符与 `..`),确保下载始终落在其保存目录内。完整字段定义见下方的 OpenAPI 文档。

## MCP(Model Context Protocol)

FluxDown 支持通过 HTTP 提供 [MCP](https://modelcontextprotocol.io) 服务,让 AI 客户端(Claude Desktop、Cursor、Cline 及任何支持 MCP 的智能体)用自然语言驱动下载。它是单个端点 `POST /mcp`,由与管理 API 相同的 token 保护。

MCP 是"JSON-RPC 2.0 over 单 HTTP 端点"(不是 REST)——每个操作都是一次 POST 到 `/mcp`,靠请求体里的 `method` 区分,采用 Streamable HTTP 传输的无状态子集:请求返回 `application/json`,通知返回 `202 Accepted`,不跟踪会话 id。用 `Authorization: Bearer <token>`(或 `X-FluxDown-Token`)鉴权;规范允许内部部署用静态 bearer token 代替 OAuth 2.1。

### 工具列表

客户端先调 `tools/list` 在运行时发现这些工具(每个都自带完整的参数 JSON Schema),再用 `tools/call` 调用其中之一:

| 工具 | 作用 | 参数 |
|---|---|---|
| `download_add` | 新建下载任务(HTTP/HTTPS/FTP/磁力/BitTorrent)。返回新任务 id。 | `url`(必填);可选 `fileName`、`saveDir`、`segments`、`proxyUrl`、`cookies`、`referrer`、`userAgent`、`queueId`、`checksum` |
| `download_list` | 列出任务,可按状态过滤。 | `status`(可选:`all`/`pending`/`downloading`/`paused`/`completed`/`error`/`preparing`) |
| `download_get` | 按 id 查询单个任务的完整详情。 | `taskId`(必填) |
| `download_pause` | 暂停指定任务。 | `taskId`(必填) |
| `download_resume` | 恢复指定的已暂停任务。 | `taskId`(必填) |
| `download_pause_all` | 暂停全部活跃任务(pending / downloading / preparing)。 | 无 |
| `download_resume_all` | 恢复全部已暂停任务。 | 无 |
| `download_remove` | 删除任务,可选同时删除磁盘文件。 | `taskId`(必填);可选 `deleteFiles`(布尔) |
| `queue_list` | 列出全部命名队列及其配置。 | 无 |

这九个工具全部直接映射到管理 API 的宿主能力,所以 MCP 客户端与 REST 客户端看到的是完全相同的任务与队列。

### 接入客户端

把 MCP 客户端指向该端点并带上 bearer token,例如在 `mcp.json` 里:

```json
{
  "mcpServers": {
    "fluxdown": {
      "url": "http://<host>:17800/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

或直接用 curl 试调用——先 initialize,再调一个工具:

```bash
curl -X POST http://<host>:17800/mcp \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call",
       "params":{"name":"download_add",
                 "arguments":{"url":"https://example.com/file.zip","segments":8}}}'
```

## fluxdown:// URL 协议

在 HTTP API 之外,FluxDown 还注册了一个自定义 URL 协议,任何网页、脚本或第三方应用都可以用它转交下载——不需要发起本机 HTTP 调用:

```text
fluxdown://download?url=<percent 编码的 URL>&filename=<可选文件名>
```

- `url`——必填。要下载的地址,需 percent 编码(`http`/`https`/`ftp` 直链或 `magnet:` 链接)。缺少或为空 `url` 参数的 `fluxdown://` URL 会被静默忽略。
- `filename`——可选。建议文件名,会预填给用户保留或修改。当真实文件名只存在于接收方永远看不到的 `Content-Disposition` 响应头里时特别有用。

由谁响应取决于平台:

- **桌面端(Windows、macOS、Linux)**——客户端注册系统协议处理器(Windows 每次启动写注册表;macOS 经 `CFBundleURLTypes` 声明;Linux 经 `.desktop` 文件的 `x-scheme-handler` 条目)。打开 `fluxdown://` URL 会启动客户端(或转发给已在运行的实例),并把请求路由进与浏览器扩展请求相同的外部下载流程:默认弹快速下载确认框,用户开启免打扰下载后则静默建任务。在 Android 以及受限的桌面环境中,浏览器扩展本身也可以经此协议投递——见 [fluxdown:// 协议模式](/docs/zh/browser-extension/usage/)。
- **Android**——应用为该 scheme 声明了 VIEW intent-filter。打开 URL 会唤起应用并显示新建下载弹层,`url` 与 `filename` 已预填;用户确认后才开始下载。弹层打开期间陆续到达的协议 URL 会作为新行合入其中(浏览器扩展在 Android 上就是这样投递批量下载的)。

一个普通的 HTML 链接就能完成集成:

```html
<a href="fluxdown://download?url=https%3A%2F%2Fexample.com%2Ffile.zip&filename=file.zip">
  用 FluxDown 下载
</a>
```

注意该协议不携带任何 Cookie、请求头或凭据——接收方会从零发起对该 URL 的请求。需要认证的下载请改用上面的脚本接管或管理 API 端点,它们的请求体接受 `cookies` 与 `headers`。

## 交互式文档

- 本站的 [`/api-docs`](/api-docs) 渲染完整的 OpenAPI 3.1 规范(由真实路由 handler 生成),带在线试调用界面,覆盖两种宿主共有的路由。
- 运行中的服务器还会自己提供实时规范:`/api/v1/openapi.json`(原始 JSON)——始终与你正在运行的那个版本保持一致。
