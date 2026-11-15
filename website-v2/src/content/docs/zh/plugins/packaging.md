---
title: 打包与插件市场
description: 构建 .fxplug、本地自检、安装/更新/卸载语义、发布到插件索引的完整流程，以及通过 REST/RPC 管理插件。
section: plugins
order: 15
sourceHash: "e362e9be2b99"
---

本页带着一个写好的插件，从源码文件夹走到用户可安装的插件包，再走到官方插件市场。想先学怎么写插件，请从[写第一个插件](/docs/zh/plugins/your-first-plugin/)或 [TypeScript 项目](/docs/zh/plugins/typescript-project/)开始；manifest 的全部规则见 [Manifest 参考](/docs/zh/plugins/manifest/)。

## `.fxplug` 格式

`.fxplug` 文件就是**插件文件夹打的 zip**，只是换了扩展名。两种结构都可以：

```
my-plugin.fxplug          my-plugin.fxplug
├── manifest.json         └── my-plugin/          ← 单层包裹目录
└── resolver.js               ├── manifest.json      会被自动剥掉
                               └── resolver.js
```

`manifest.json` 必须在 zip 根部，**或者**整个 zip 顶层有且只有一个文件夹（旁边没有任何其他内容），`manifest.json` 就在这个文件夹里。其他情况一律被拒绝，错误是「zip 内未找到根 manifest.json」。

> **macOS 的坑。** 访达的「压缩」会在你的文件夹旁边加上一个 `__MACOSX/` 目录（通常还有 `.DS_Store`）。顶层同时出现 `my-plugin/` 和 `__MACOSX/` 的 zip 有两个顶层目录，不会被当作单层包裹目录，于是报上面的错误；文件夹旁多出一个零散文件也一样。请改用命令行或 SDK 来创建压缩包。

### 构建插件包

**推荐：用 SDK。** 在用 `fluxdown-plugin create` 创建的项目里：

```sh
npm run pack        # 或：bun run pack
```

`pack` 会把入口打包到 `dist/`，用严格规则校验结果，并生成 `<identity>-<version>.fxplug`（例如 `acme@demo-1.0.0.fxplug`），manifest 位于 zip 根部。见 [SDK 参考](/docs/zh/plugins/sdk/)和完整的 [TypeScript 项目教程](/docs/zh/plugins/typescript-project/)。

**手动打包**，适用于手写的插件文件夹。请打包文件夹的*内容*，而不是文件夹本身：

```sh
cd my-plugin
zip -r -X -D ../acme-demo-1.0.0.fxplug . -x "*.DS_Store" -x "__MACOSX/*"
```

- `.` 让 `manifest.json` 位于 zip 根部。
- `-D` 不写入目录条目，`-X` 不写入额外文件属性，两个 `-x` 排除 macOS 的元数据。
- 只放 manifest 引用到的内容（入口脚本、图标、`rules.json`、资源文件）；不要把 `node_modules`、`.git` 和用不到的 source map 打进去。

在 Windows 上，先选中文件夹**里面**的文件（不要选文件夹本身），再用「发送到 → 压缩(zipped)文件夹」，最后把结果改名为 `.fxplug`。

### 安装时强制的限制

无论 zip 从哪里来，安装器都应用这些检查：

| 检查 | 限制 |
|---|---|
| 包体积 | 从市场或索引下载时 ≤ 10 MiB；REST 上传端点接受的请求体至多 4 MB |
| zip 条目数 | ≤ 200 |
| 解压总大小 | ≤ 50 MB（按实际解压出的字节数统计，而不是 zip 里声明的大小） |
| 条目路径 | 必须落在目标目录内——`..` 和绝对路径被拒绝（防 zip-slip） |
| manifest | 完整解析并校验（见 [Manifest 参考](/docs/zh/plugins/manifest/#校验)） |
| 文件 | `main.entry`、每个 `desktop.entry` 与 `sniffRules` 文件必须存在 |
| 脚本 | resolver、hooks、auth、subscription 入口会做编译检查，语法错误中止安装 |
| 设置项 | 每个 `pattern` 都必须能按 JavaScript RegExp 编译 |
| 加载 | 安装后插件必须真正加载成功——其中包含 `minAppVersion` 门槛 |

插件解压到 `<数据目录>/plugins/<identity>/`。目录安装（开发工具使用）不会剥包裹目录：你给的路径下必须直接有 `manifest.json`。

## 发布前本地自检

按顺序做这几步；每一步都能提前发现后一步要等发布之后才会暴露的问题。

1. **校验 manifest 与插件包。** 用 SDK：`fluxdown-plugin validate`（`pack` 也会运行它）。它应用引擎规则，再加上市场更严格的要求：严格的 `MAJOR.MINOR.PATCH` 版本号与 `minAppVersion`、所有 resolver / hooks / auth / subscription 入口与图标都必须在包内、200 条目与 50 MB 限制，以及嗅探规则包的内容检查。
2. **真正装一次。** 先把文件夹作为开发模式插件加载，再在干净的配置下通过*设置 → 扩展 → 插件*（或 `POST /api/v1/plugins/install`，见下文）安装打好的 `.fxplug`，证明可用的是插件包而不只是文件夹。走一遍授权，把插件的功能各用一次。出错时见[调试与排错](/docs/zh/plugins/debugging/)。
3. **演练索引准入**（可选，推荐）。索引仓库自带机器人所运行的同一套准入代码。克隆后使用 Node 20 或更高版本，无需任何依赖：

   ```sh
   git clone https://github.com/zerx-lab/fluxdown-plugin-index
   cd fluxdown-plugin-index
   # 先把你的条目写进 registry.json（见「发布到插件索引」），然后：
   GITHUB_TOKEN=<token> node scripts/check-registry.mjs --head registry.json
   ```

   `check-registry.mjs` 会下载你仓库的**最新 Release** 并对它运行完整准入，因此必须已经存在发布好的 GitHub Release。`node scripts/validate.mjs` 与 `node --test scripts/index.test.mjs scripts/stage.test.mjs` 检查的是索引仓库本身（分片、哈希、序号），主要面向自建索引的人。

## 安装、更新与卸载

对同一个 identity 再次**安装**会原地替换旧版本。你的数据会怎样：

| | 更新时 | 卸载时 |
|---|---|---|
| 安装目录 | 被替换；更新失败会恢复旧版本 | 删除（开发模式插件的源码文件夹永远不会被删，只删除其登记） |
| 设置、`flux.storage` 的值、启用/停用状态 | **保留** | 删除（插件 `plugin.<identity>.` 配置前缀下存放的全部内容） |
| 授权 | 保留；只有新增的权限或主机需要批准 | 随其他内容一起删除 |
| 失败熔断 | 重置——被熔断停用的插件，安装新版本后会重新启用 | — |
| 用户手动停用 | 保持停用 | — |
| 市场撤回状态 | 已安装版本与被撤回的版本不同时清除 | — |
| 已保存的凭据 | 保留 | 删除 |
| 任务绑定 | 保留 | 曾绑定到该插件的任务会切换为「忽略插件、按原始链接」，仍可继续 |
| 临时文件（`flux.fs`）与记住的站点默认插件 | 保留 | 删除 |

安装是「全有或全无」：任何一项检查失败，磁盘上都不会有任何改动。新版本何时会再次要求授权，见[权限](/docs/zh/plugins/permissions/#授权)。

## 插件市场

FluxDown 的市场是**一份数据格式，不是一个服务**——一份应用直接读取的索引，无后端、无账号。组成部分：

- **索引**——一份 Git 版本化的 JSON 目录，列出插件、版本和下载镜像。默认索引在 [`zerx-lab/fluxdown-plugin-index`](https://github.com/zerx-lab/fluxdown-plugin-index)，从两个默认来源拉取（GitHub raw 文件与 jsDelivr 镜像，先成功者胜出）。任何人都可以 fork 一份自己维护，用户也可以通过 `market_index_sources` 设置（逗号分隔的 URL）让应用使用其他来源。
- **作者维护自己的仓库**——每个插件放在作者自己的 GitHub 仓库里。索引只登记「哪个仓库发布哪个插件 identity」，并自动收录该仓库的**最新 Release**。
- **内容寻址**——每个发布版本都记录 `contentHash = sha256(整个 .fxplug 文件)`。从任何镜像下载后，FluxDown 重新计算哈希，不一致就拒绝。被攻破的镜像换不掉内容，已发布版本的字节也永不改变。
- **manifest 一致**——安装前，FluxDown 核对包内 `identity` 与 `version` 与索引条目相等，且包的权限与网络主机都包含在条目之内，因此包拿到的永远不会比市场展示给你的更多（见[权限](/docs/zh/plugins/permissions/#市场如何校验插件包)）。
- **防回滚**——每份索引带一个单调递增的 `sequence`；应用记住每个索引见过的最大值，拒绝更旧的。

v1 没有作者级密码学签名——完整性依靠内容寻址 + TLS + Git 历史。索引 schema 预留了签名字段，将来加上不会破坏现有客户端。

## 发布到插件索引

从一个做好的插件包到出现在市场里的完整路径。插件只需**登记一次**；之后每个版本都是一次 GitHub Release。

### 开始之前

- 一个**公开**的 GitHub 仓库来存放插件。**一个仓库只发布一个插件。**
- 一份 `identity` 已定稿的 manifest（`author@name`），它是永久 ID，之后不能再改。
- 一个通过 `fluxdown-plugin validate`、并能在干净配置下安装的插件包。

### 1. 发布 GitHub Release

1. 把 `manifest.json` 的 `version` 写成严格的 `MAJOR.MINOR.PATCH`（只含数字、没有前导零、没有 `v` 前缀、没有后缀）。第一个版本可以随意，例如 `1.0.0`；之后每个版本都必须**严格高于索引里已有的所有版本**。
2. 创建 GitHub Release——不是草稿，也不是预发布——并附上**恰好一个** `.fxplug` 文件。tag 由你定，但它必须是仓库的 *Latest* release（GitHub 标记为「Latest」的那个）。
3. 建议：开启 GitHub 的[不可变 Release](https://github.blog/changelog/2025-10-28-immutable-releases-are-now-generally-available)，让已发布的附件之后无法被替换。

### 2. 提 PR 登记一次

1. Fork [`zerx-lab/fluxdown-plugin-index`](https://github.com/zerx-lab/fluxdown-plugin-index)。
2. 只修改 [`registry.json`](https://github.com/zerx-lab/fluxdown-plugin-index/blob/main/registry.json)，在 `plugins` 数组里追加一项：

   ```json
   { "id": "author@name", "repo": "owner/repo", "tags": ["resolver", "video"] }
   ```

   | 字段 | 规则 |
   |---|---|
   | `id` | 必须等于 manifest 的 `identity`。 |
   | `repo` | 你的公开仓库，写成 `owner/repo`。一个仓库只发布一个插件。 |
   | `tags` | 可选；至多 10 个小写标签，用于市场展示。 |

3. 提交 PR。**registry-check** 机器人会下载你的最新 Release，运行完整准入，并在 PR 里评论结果、你声明的权限和插件拦截的 URL 规则。
4. 检查失败时，先修好 Release（发布新的一个，或在尚未被收录的 Release 上替换附件），再向 PR 推送任意提交重新触发检查，也可以手动运行 `registry-check` workflow 并填入 PR 号。
5. 维护者审核并合并 PR。**首次登记由人工审核**，这是唯一的人工环节。随后 Stage 任务会在几分钟内发布你的插件。

### 3. 准入检查的内容

PR 检查与每小时一次的 Stage 任务使用同一套规则：

| 检查 | 规则 |
|---|---|
| Release | 登记仓库的 Latest Release，恰好一个 `.fxplug` 附件，下载地址属于该仓库自己的 `releases/download` |
| 字节 | ≤ 10 MiB；GitHub 提供附件 digest 时，sha256 必须一致 |
| zip | ≤ 200 个条目、解压 ≤ 50 MiB；禁止 `..`、绝对路径与反斜杠路径，禁止符号链接、加密条目、ZIP64、重复条目；只支持 *stored* 或 *deflate* 压缩；校验 CRC；`manifest.json` 在根目录或唯一的包裹目录内 |
| manifest | 与客户端同一套 schema；此外每个 `entry` 与 `icon` 文件都必须存在于包内 |
| 设置国际化（仅市场） | 新上架及更新时，每项设置的标题、全部选项标签和自定义说明／辅助按钮文案，均须有完整、非空的 `localizations.en` 与 `localizations.zh` |
| 身份 | manifest 的 `identity` 等于登记的 `id` |
| 版本 | 严格的 `MAJOR.MINOR.PATCH`，严格高于全部已发布版本；字节完全相同时，只把该 Release 追加为这个版本的镜像 |
| 不可变 | 已被收录的 Release 附件如果 digest 发生变化，会被上报；已发布版本保持原有字节 |

这是发布准入策略，**不是客户端安装或加载要求**。市场已有版本、原包字节相同的补镜像不追溯要求翻译。历史豁免以可信发布基线中的 identity/version/hash 为准，作者不能自行声明“旧插件”来绕过。自动 Stage 与镜像 CI 中的新增分片检查都会执行，历史复核不执行。没有 manifest 设置项的插件无需翻译设置标签；自定义脚本视图仍需自行实现按语言渲染。


### 4. 发布更新

发布一个 `version` 更高的新 GitHub Release 即可，无需 PR。Stage 任务**每小时**运行一次（`registry.json` 变化时也会运行），收录仓库此刻的 *Latest* release：计算哈希、解包、校验、追加分片、重新发布 `index.json`。它只看 Latest Release，所以如果一小时内连发了几个版本，不要指望中间版本会被收录。

某个 Release 无法收录时（identity 不符、版本没有提高、manifest 有误、入口文件缺失……），索引仓库里会开一个带 `stage-fail` 标签的 issue；之后一次运行成功时它会被自动关闭。所以请修好问题并发布修正后的 Release——不要复用旧版本号。

更新是全自动的，不做人工审核。安装前用户始终能看到权限；新增权限的版本会让用户再次授权。

### 维护者操作

这些操作通过 PR 或由索引维护者完成：

- **撤回某个版本。** 维护者在该版本的分片里把 `yanked` 设为 `deprecated`、`vulnerable` 或 `malicious`，并重新生成索引。被撤回的版本会被「安装最新版」跳过；`vulnerable` 与 `malicious` 还会停用已安装的副本。被停用的副本会一直保持停用，直到用户安装一个不同于被撤回版本的版本，所以请发布一个版本号更高的修复版。
- **下架插件。** 从 `registry.json` 删除它的条目。Stage 停止收录并关闭它的 `stage-fail` issue；需要从安装中消失的版本要另外撤回。
- **更换维护者。** 提一个 PR，把已有 `id` 的 `repo` 指向新仓库。机器人会标注，原维护者必须在 PR 中确认；新仓库的版本号仍须高于已发布的版本。

## 发布检查清单

1. `identity` 已定稿，`version` 是严格的三段式，并高于上一次发布。
2. 如果用了旧版本不认识的字段、事件或权限，已经抬高 `minAppVersion`。
3. `permissions` 只列代码实际用到的，不多列；每一项 `networkHosts` 都确有必要。
4. `fluxdown-plugin validate` 通过；包不超过 10 MiB、200 个条目、解压 50 MB。
5. `.fxplug` 的 `manifest.json` 在根部（没有 `__MACOSX/`，没有零散文件）。
6. 你已在干净配置下安装过这个包、完成授权，并把每项功能都用了一遍。
7. GitHub Release 不是草稿或预发布，被标为 Latest，且恰好一个 `.fxplug` 附件。
8. 合并之后（首次）或发布之后（更新），一小时内检查市场，并留意是否出现 `stage-fail` issue。

## 管理接口

除了设置页，也可以用接口以编程方式管理插件。REST 路由属于管理 API 组，需要管理 token——见 [API 概览](/docs/zh/api/overview/)。同样的操作也以 RPC 方法的形式存在于 agent/daemon 的 `/rpc` 端点上。

| REST | RPC 方法 | 用途 |
|---|---|---|
| `GET /api/v1/plugins` | `daemon.plugin.list` | 列出已安装插件及其加载状态与授权状态。 |
| `POST /api/v1/plugins/install`（body 为 `.fxplug` 字节，请求体 ≤ 4 MB） | `daemon.plugin.install` | 从插件包安装。 |
| `POST /api/v1/plugins/install-dev`，body 为 `{ "dirPath": "<daemon 所在主机上的绝对路径>" }` | `daemon.plugin.installDev` | 把一个文件夹登记为开发模式插件（不复制）。 |
| — | `daemon.plugin.reloadDev` | 重新读取开发模式插件的 manifest 与源码并重跑安装检查；失败时保留登记。 |
| `PUT /api/v1/plugins/{identity}/enabled`，body 为 `{ "enabled": true }` | `daemon.plugin.setEnabled` | 启用或停用；仍有待批准项时启用会失败，错误为 `consent required`。 |
| `PUT /api/v1/plugins/{identity}/settings` | `daemon.plugin.updateSettings` | 更新设置值。 |
| `POST /api/v1/plugins/{identity}/auth` | `daemon.plugin.auth` | 驱动登录流程（`begin` / `poll` / `cancel` / `logout` / `status`）。 |
| `DELETE /api/v1/plugins/{identity}` | `daemon.plugin.uninstall` | 卸载。 |
| `GET /api/v1/market` | `daemon.plugin.marketList` | 合并后的市场索引。 |
| `POST /api/v1/market/install`，body 为 `{ "pluginId": "author@name" }` | `daemon.plugin.marketInstall` | 安装最新的未撤回版本；RPC 还接受 `version`，用来钉住用户确认过的版本（最新版本与之不同时拒绝安装）。 |
| — | `daemon.plugin.consent` | 授权插件请求的权限与主机（见[权限](/docs/zh/plugins/permissions/#没有界面时授权无界面服务器与脚本)）。 |
| — | `daemon.plugin.audit.list`、`.clear`、`.record` | 读取、清空、上报审计记录。 |
| — | `agent.plugin.installFile` | agent 读取本机插件包文件，并通过 daemon 安装。 |
