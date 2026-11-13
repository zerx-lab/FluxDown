---
title: 权限、授权与审计
description: 各插件权限授予什么、风险等级与授权如何运作（界面与无界面两种方式）、市场如何校验插件包、哪些操作会被审计，以及「完全信任」的含义。
section: plugins
order: 14
sourceHash: "770bfb63ac01"
---

插件并非被沙箱限制到毫无用处：它可以申请很强的能力。FluxDown 的规则是：每项能力都必须在 manifest 里**声明**，在生效之前**展示给你并由你批准**，并**记入**审计日志。

本页讲面向用户的模型，以及插件作者需要了解的规则。manifest 字段本身见 [Manifest 参考](/docs/zh/plugins/manifest/#permissions)，各权限背后的接口见 [API 参考](/docs/zh/plugins/api-reference/)。

## 权限

| 权限 | 授予 |
|---|---|
| *（无）* | 基础沙箱：`flux.fetch`、`flux.storage`、临时文件、日志、视图 / 命令贡献。 |
| `ffmpeg` | 对下载完成的文件使用 `flux.ffmpeg` / `flux.ffprobe`。 |
| `ytdlp` | `flux.ytdlp`。 |
| `auth` | 平台登录流程，以及 `flux.fetch` / `flux.auth` 使用的已存凭据。 |
| `tasks` | `flux.tasks.*`：列出、创建、暂停、继续、删除下载任务。 |
| `browserRules` | 向浏览器扩展贡献规则（`contributes.sniffRules`）。 |
| `background` | 让 `main` 脚本随 daemon 常驻。 |
| `intercept` | 拦截器：读取并修改匹配任务的链接、请求头、Cookie 与请求体，控制连接数，跳过探测，在下载中途换发凭据，改写或过滤流媒体分片，选择画质或 DASH Period，在完成前处理下载文件，并在任务失败后请求重试。见[拦截器指南](/docs/zh/plugins/interceptors-guide/)。 |
| `desktop` | 以**完全信任**运行桌面原生脚本（见下文）。 |
| `pageScripts` | 通过 FluxDown 浏览器扩展，在 `contributes.pageScripts` 列出的网站中运行插件脚本，风险**完全**（见[浏览器中的页面脚本](#浏览器中的页面脚本)）。 |
| `archive` | 允许 `contributes.archive` 使用已知文件名与可选来源元数据选择安全子目录；`contributes.archiveRules` 提供发布者签名分类规则。不会暴露 URL、路径或凭据；规则是数据而非可执行代码。`archiveRules.updateUrl` 主机名会纳入网络授权。 |

订阅 provider、hooks、resolver 与可移植视图本身不需要额外权限。

## 风险等级

风险等级由权限推导，不是插件自己声明的。

| 等级 | 条件 | 显示为 |
|---|---|---|
| **标准** | 没有任何权限。 | — |
| **较高** | 含 `ffmpeg`、`ytdlp`、`auth`、`tasks`、`browserRules`、`background`、`intercept`、`archive` 任一项。 | 警告徽标 |
| **完全** | 含 `desktop` 或 `pageScripts`。 | 危险徽标 |

徽标显示在已安装的插件和市场列表上。

## 授权

插件只会以你已批准的能力运行。批准结果按插件保存，内容是你看到的那一组权限与网络主机的精确集合，以及对应的版本。

- **安装或更新。** 安装插件（市场、`.fxplug`、目录或开发模式安装）或更新插件时，如果它的权限或所请求的网络主机未被你之前的批准覆盖，插件会装上但保持**停用**，原因显示为*需要授权*，直到你批准。桌面端会弹出授权对话框，列出风险徽标、每项权限的含义和网络主机列表，插件所在行也会出现**授权**按钮。
- **完全信任。** 对**完全**等级的插件，对话框会额外给出明确警告，只有勾选*我已了解*之后确认按钮才可用。
- **市场安装。** 市场安装前的确认就是同一个对话框；在那里确认即授予同一组能力，不会再问第二次。对话框会记录它展示的版本；如果此时市场上的最新版本已经变化，安装会被拒绝，让你先查看新版本的权限。
- **更新只为新增项再次询问。** 新版本增加了权限或网络主机时需要你批准；没有增加则什么都不变。
- **升级 FluxDown。** 授权机制出现之前安装的插件继续可用：它们当时拥有的权限会被记为已批准（在审计日志里标注为迁移）。之后它们再申请的新内容需要批准，而此后你新装的插件绝不会被预先批准。
- **开发模式安装也不例外。** 声明了权限的开发模式插件，同样要先获得批准才会运行。

批准是精确匹配的：已批准的集合必须与插件当前请求的集合完全一致，不能多也不能少。

### 没有界面时授权（无界面服务器与脚本）

在无界面服务器上，或任何没有桌面对话框的环境里，通过服务 RPC 授权。在 agent/daemon 的 `/rpc` 端点（见 [API 概览](/docs/zh/api/overview/)）调用 `daemon.plugin.consent`，传入插件 identity 以及它当前请求的**完整**列表：

```json
{
  "identity": "acme@demo",
  "permissions": ["background", "tasks"],
  "networkHosts": []
}
```

- `permissions` 是插件完整的 `permissions` 数组；`networkHosts` 是 `main.imageHosts`、所有视图 `desktop.networkHosts` 以及已声明的 `archiveRules.updateUrl` 主机名的并集。顺序和重复项无关；主机按字面精确比较（区分大小写）。
- 任一列表与 manifest 当前请求不一致，调用都会失败，错误为 `consent does not match requested capabilities`。可以先从 `daemon.plugin.list` 读出插件当前的待授权内容（`consentPending`、`consentPendingHosts`）。
- 授权没有完成之前，启用该插件（`daemon.plugin.setEnabled` 传 `true`，或 `PUT /api/v1/plugins/{identity}/enabled`）会失败，错误为 `consent required`。

调用成功后插件会正常加载并启用。这次授权会以 `consent.granted` 写入审计日志。

## 市场如何校验插件包

插件索引的条目带有权限与网络主机，市场界面在安装前就是把这些展示给你。FluxDown 在安装前会把下载到的包与该条目再核对一遍：

- 包内 manifest 的 `identity` 与 `version` 必须与索引条目**完全相等**；
- 包内 manifest 的每一项权限、每一个网络主机都必须**包含在**索引条目的列表之内——包申请的比条目公布的更多时会被拒绝。（是「包含」而不是「相等」：这是市场的防越权检查。上面的授权检查更严格，要求精确匹配。）

索引本身在每次发布时都会自动检查。**插件的首次登记由维护者人工审核；已登记插件的后续版本不再人工审核**——它们只需通过或未通过自动准入检查。安装前，你始终能看到插件的权限。见[打包与插件市场](/docs/zh/plugins/packaging/#发布到插件索引)。

## 完全信任：桌面脚本

视图的 `desktop` 脚本运行在桌面应用进程内，是 GPUI Shell 下的 ES module。就你的机器而言它**不受沙箱约束**：它以你的用户权限读写文件、启动进程、使用剪贴板。

网络访问是例外。`networkHosts` 是**强制生效的白名单**：脚本只能连接列出的这些主机名（不支持通配符），这份清单也会显示在授权对话框里；未列出的主机会被拒绝。文件系统、进程与剪贴板不受限制，`process.exit` 永远不会被授予。

只有当你像信任任何下载后运行的程序那样信任作者时，才安装带 `desktop` 的插件。作者一侧——怎么写这类脚本、可以使用哪些模块——见[桌面原生界面指南](/docs/zh/plugins/desktop-ui-guide/)。

**桌面脚本所做的原生文件、进程、网络操作不会被逐条记录。** 审计日志只记录桌面脚本被加载、卸载，以及它发起的每一次 `fluxdown.call` / `fluxdown.invoke`——也就是它接触 FluxDown 的那条粗粒度边界——至于它自己对操作系统做了什么，则不留记录。

桌面脚本不会在 daemon、Web 或移动端运行。

## 浏览器中的页面脚本

声明了 `pageScripts` 的插件，会让 FluxDown 浏览器扩展在其 manifest 列出的网站里运行插件的 JavaScript。这样的脚本可以读取并修改页面显示的一切内容，以及已登录用户在该站点能做的一切操作，所以这项权限是**完全**风险，桌面端与 Web 端的授权对话框会列出每条脚本的标题和它匹配的站点。

每个插件需要确认两次，且各只需一次：

1. **在 FluxDown 中**：在上文所述的授权对话框里批准该权限（完全风险的插件需要勾选「我已了解」）。
2. **在浏览器扩展中**：第一次在扩展选项页启用某个插件的页面脚本时，会弹出对话框说明风险并请你确认。

此后，同一插件新增脚本、更新代码或修改匹配站点都会自动生效，不再询问。停用或卸载插件，或有新申请的权限尚未批准时，它的脚本会从浏览器中移除。脚本代码始终从 FluxDown 桌面端获取，扩展自己不保存副本。桥接口、限制与安全注意事项见[页面脚本](/docs/zh/plugins/page-scripts-guide/)。

## 失败熔断

插件的调用连续三次超时或超出内存限制，就会被自动停用（审计日志记为 `breaker`）。安装该插件的新版本会清除熔断状态。如何把熔断与其他停用状态区分开，见[调试与排错](/docs/zh/plugins/debugging/)。

## 审计日志

打开**设置 → 扩展 → 插件 → 审计日志**，或在某个插件详情里点**审计记录**只看该插件。每行包含时间、来源（`daemon` 或 `desktop`）、动作、目标、结果（`ok`、`denied`、`error`）和详情。可以按插件筛选、加载更早的记录、把日志复制为 JSON，以及清空（需确认）。

同样的数据也可以通过 RPC 读取：`daemon.plugin.audit.list`（分页，可按 identity 过滤）与 `daemon.plugin.audit.clear`。

| 动作 | 何时记录 |
|---|---|
| `install`、`update`、`uninstall`、`enable`、`disable` | 生命周期变化。 |
| `consent.granted`、`consent.migrated` | 你批准了能力；或升级时记录了既有能力。 |
| `revoked` | 市场撤回了该插件。 |
| `breaker` | 插件被失败熔断器停用。 |
| `fetch` | `flux.fetch`；目标为 `METHOD scheme://host/path`——查询字符串和凭据会被去掉——详情为状态码。 |
| `auth.save`、`auth.remove` | 凭据变更。 |
| `fs.write`、`fs.remove` | 写入插件的临时工作区。 |
| `ffmpeg.run`、`ffprobe.run`、`ytdlp.run` | 工具运行；目标为（缩短后的）参数列表。 |
| `task.retry`、`task.artifact` | hooks 发起的任务请求。 |
| `intercept.<阶段>` | 该阶段的一次拦截器调用（`prepare`、`segments`、`finalize` 等）；目标为任务 id，详情为空或错误文本。 |
| `tasks.create`、`tasks.control` | `main` 里的 `flux.tasks` 调用。创建任务有频率限制；被拒绝的调用会以结果 `denied` 记录。 |
| `settings.update` | `flux.settings.update`。 |
| `desktop.load`、`desktop.unload` | 桌面脚本启动 / 停止；详情列出它运行时的授权。 |
| `desktop.call`、`desktop.invoke` | 桌面脚本发起的 RPC。 |
| `audit.clear` | 日志被清空。 |

细节：

- 每行的目标最多保留 512 字节，详情最多 2048 字节，超出部分被截断。
- `desktop.*` 记录由桌面客户端通过 `daemon.plugin.audit.record` 上报，来源一律被强制为 `desktop`，每次调用至多 100 条。
- 读取、`flux.storage` 与日志不会被记录。超过 90 天的记录会被清除，日志最多保留最新的 20,000 行。插件空闲时不会写入任何内容。
