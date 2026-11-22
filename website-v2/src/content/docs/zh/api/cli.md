---
title: 命令行客户端
description: fluxdown 命令行客户端 —— aria2c 风格的下载客户端，从终端或脚本驱动管理 API。
section: api
order: 2
sourceHash: "fa193ab09799"
---

FluxDown 提供一个命令行客户端 `fluxdown`，对标 `aria2c`。它有两种工作模式：

- **远程模式（默认）**：一个薄的 typed HTTP 客户端，运行在[管理 API](/docs/zh/api/overview/) 之上。绝大多数命令都与运行中的 FluxDown 桌面应用或 [Headless 服务器](/docs/zh/headless-server/setup/) 通信（默认 `http://127.0.0.1:17800`，也可指向任意地址），因此它管理的正是你在应用里看到的同一批任务与队列。在这个角色下，可以把它理解为一个可脚本化的远程遥控器，与 `aria2c` 的 RPC 客户端模式相同。
- **独立模式（`add --local`）**：完全脱离运行中的实例，在 CLI 进程内**内嵌下载引擎**直接下载。这条路径不连服务器、不需要 token，适合无 GUI 的环境或一次性脚本化下载。详见 [`add`](#add) 与下方的[独立模式](#独立模式---local)一节。

此外 [`config`](#config) 子命令是纯本地文件操作，同样不连服务器。

## 获取二进制

CLI 是 workspace 里的一个 crate（`native/cli`）。从源码构建：

```bash
cargo build -p fluxdown_cli --release
# 二进制位于 target/release/fluxdown（Windows 上为 fluxdown.exe）
```

或在开发时直接经 Cargo 运行：

```bash
cargo run -p fluxdown_cli -- ping
```

## 配置

每条命令都接受以下全局选项；最关键的两个还会读取环境变量，一次设置即可长期生效：

| 选项 | 环境变量 | 默认值 | 含义 |
|---|---|---|---|
| `--url <URL>` | `FLUXDOWN_URL` | `http://127.0.0.1:17800` | FluxDown 实例的基址。 |
| `--token <TOKEN>` | `FLUXDOWN_TOKEN` | （无） | 管理 API token。除 `ping` 外每条命令都需要。 |
| `--timeout <SECS>` | — | `30` | 单请求超时（秒）。 |
| `--json` | — | 关 | 输出机器可读的 JSON，而非格式化文本。 |

`url`、`token`、`timeout` 三项的生效值按 **显式 flag > 环境变量 > 持久化配置文件 > 内置默认** 的优先级解析。持久化那一层由 [`config set`](#config)（类似 `go env -w`）写入，因此 token 可以只设一次，不必每次都 export。

token 来自你运行中的实例：桌面应用在**设置 → 本机 API 服务**下；headless 服务器的密钥是你首次打开 Web 界面时自行设置的，也可在**设置 → 安全与访问**查看/更换。详见[鉴权](/docs/zh/api/overview/#鉴权方式)。

```bash
# 方式 A：导出环境变量（仅当前 shell 会话有效）
export FLUXDOWN_TOKEN="fxd_your_token_here"
export FLUXDOWN_URL="http://127.0.0.1:17800"   # 可选；这就是默认值
fluxdown list

# 方式 B：持久化一次，之后无需再 export
fluxdown config set token fxd_your_token_here
fluxdown list
```

CLI 始终直连给定地址，绝不走系统代理，因此即便设置了 `HTTP_PROXY` 环境变量，本地实例依然可正常连接。

## 命令

| 命令 | 别名 | 作用 |
|---|---|---|
| `ping` | — | 探测实例是否存活。无需 token。 |
| `info` | — | 显示实例的应用名与版本。 |
| `add <URL...>` | `get` | 创建一个或多个下载任务。 |
| `list` | `ls` | 列出任务，可按状态过滤。 |
| `status <ID>` | `stat` | 显示单个任务的完整详情。 |
| `pause <ID>` | — | 暂停任务。 |
| `resume <ID>` | — | 恢复暂停的任务。 |
| `rm <ID>` | — | 删除任务（可选同时删文件）。 |
| `pause-all` | — | 暂停全部任务。 |
| `resume-all` | — | 恢复全部暂停的任务。 |
| `queue` | — | 列出命名队列及其配置。 |
| `watch [ID]` | — | 轮询并重绘进度，直至任务到达终态。 |
| `config <SUB>` | — | 读写持久化 CLI 配置（`set`/`unset`/`get`/`list`/`path`）。无需连服务器。 |

### add

```bash
# 单个 URL，全部自动
fluxdown add https://example.com/file.zip

# 一次多个 URL
fluxdown add https://example.com/a.zip https://example.com/b.zip

# 从文件读取 URL（每行一个；空行与 # 开头的行被忽略）
fluxdown add -i urls.txt

# 从 stdin 读取 URL
cat urls.txt | fluxdown add -i -
```

`add` 选项：

| 选项 | 含义 |
|---|---|
| `-i, --input-file <FILE>` | 从文件读取 URL，每行一个（`-` 表示 stdin）。与命令行上给出的 URL 合并。 |
| `-d, --dir <DIR>` | 保存目录（空 = 实例的默认目录）。 |
| `-o, --out <NAME>` | 输出文件名。仅在只添加单个 URL 时生效。文件名中的路径分隔符与 `..` 会被剥除，确保始终落在保存目录内。 |
| `-s, --segments <N>` | 分段/线程数（`0` = 按文件大小自动决定）。 |
| `--proxy <URL>` | 单任务代理 URL。 |
| `-U, --user-agent <UA>` | User-Agent 字符串。 |
| `--referrer <URL>` | Referrer 请求头。 |
| `--cookies <STR>` | Cookie 字符串。 |
| `--queue <ID>` | 命名队列 ID（空 = 默认队列）。 |
| `--checksum <SPEC>` | 校验和，格式 `algo=hexhash`（如 `sha256=abc123...`）。 |
| `--local` | 独立模式：不连服务器，在本进程内嵌引擎直接下载（详见[独立模式](#独立模式---local)）。仅 `add` 支持此选项。 |

成功时逐行打印新任务 ID（`added <id>`）；加 `--json` 则输出 ID 的 JSON 数组。给出多个 URL 时，每个 URL 独立尝试：某个失败会向 stderr 打印 `failed to add <url>: ...`，但不会中断其余 URL，也不会丢弃已创建的任务。仅当全部 URL 成功时退出码才为 `0`；否则在列出已创建任务后，以首个失败的退出码退出。

#### 独立模式（`--local`）

加上 `--local`，`add` 就不再连接任何运行中的实例，而是在 CLI 进程内**内嵌下载引擎**（与桌面应用/服务器同一套引擎）直接完成下载。这是让 CLI **脱机独立运行**的唯一路径——无需运行中的 App 或服务器，也不需要 token。

```bash
# 完全脱机下载，不依赖任何运行中的 FluxDown 实例
fluxdown add https://example.com/file.zip --local
```

行为要点：

- **一次性阻塞**：进程创建任务后阻塞等待，直至全部下载到达终态（完成/失败）才退出，退出码反映结果。
- **共享数据库**：与桌面应用/服务器共用同一数据目录下的 SQLite，因此用 `--local` 下的任务在 App 里同样可见（前提是二者解析到同一数据目录，即安装模式；便携模式下可能各自独立）。
- **保存目录优先级**：`-d/--dir` > 共享库里的默认保存目录配置 > 当前工作目录。
- **无人值守选择**：HLS 自动取最高码率，BitTorrent/磁力自动下载全部文件（无交互弹窗）。
- **Ctrl-C 语义**：中断会把未完成任务置为暂停并以退出码 `7` 退出；下次可经 App/服务器续传。
- 仅 `add` 支持 `--local`；其余命令始终走远程模式。

### list

```bash
fluxdown list
fluxdown list --status downloading
fluxdown --json list          # 脚本友好输出
```

`--status` 接受名称或数字码：`pending`/`0`、`downloading`/`1`、`paused`/`2`、`completed`/`3`、`error`/`4`、`preparing`/`5`。纯文本输出是一张包含 ID、状态、进度、大小、名称的表格；`--json` 输出原始 `TaskDto` 数组（camelCase 字段 —— 见 [API 概览](/docs/zh/api/overview/)）。

### status、pause、resume、rm

```bash
fluxdown status 42a14870-9276-4ea2-84ea-eb75ae497766
fluxdown pause  42a14870-9276-4ea2-84ea-eb75ae497766
fluxdown resume 42a14870-9276-4ea2-84ea-eb75ae497766

# 仅删除任务记录
fluxdown rm 42a14870-9276-4ea2-84ea-eb75ae497766
# 删除记录并删除磁盘上的文件
fluxdown rm 42a14870-9276-4ea2-84ea-eb75ae497766 --delete-files
```

### pause-all、resume-all、queue

```bash
fluxdown pause-all
fluxdown resume-all
fluxdown queue           # 列出命名队列
```

### watch

`watch` 轮询实例并重绘一张进度表（每次刷新前清屏），直至所有被监视的任务到达终态（完成 / 出错），然后退出。

```bash
fluxdown watch                 # 全部活动任务
fluxdown watch <ID>            # 单个任务
fluxdown watch --interval 2    # 每 2 秒刷新（默认 1）
```

### config

`config` 读写本地配置，让你不必每次都导出环境变量（类似 `go env -w`）。它从不连接服务器。合法的键为 `url`、`token`、`timeout`、`log_level`。

```bash
fluxdown config set token fxd_your_token_here   # 持久化 token
fluxdown config set url http://192.168.1.10:17800
fluxdown config set timeout 60

fluxdown config get token        # 打印单个值
fluxdown config list             # 列出全部键（未设置显示 "(unset)"）
fluxdown --json config list      # 机器可读输出
fluxdown config unset token      # 清除单个值
fluxdown config path             # 打印配置文件路径
```

配置文件是平台配置目录下的 `cli.toml`（Windows `%APPDATA%\zerx\fluxdown\config\`、Linux `$XDG_CONFIG_HOME/fluxdown/`、macOS `~/Library/Application Support/dev.zerx.fluxdown/`）。存储的 `token` 为明文；在 Unix 上文件以 `0600` 权限创建（仅属主可读写）。这里设置的值，在具体某次调用时会被显式 `--flag` 或对应环境变量覆盖。

`log_level` 单独保存在引擎数据目录的 `logging.conf`。文件格式错误时，`config list` 和不带键的 `config get` 在文本及 JSON 输出中将 `log_level` 显示为 `(invalid)`，其他键仍正常列出。`config get log_level` 会报告错误；运行 `config set log_level auto` 可修复文件并恢复默认值。

## 退出码

CLI 沿用 `aria2c` 的退出状态约定，方便脚本按失败类别分支处理：

| 退出码 | 含义 |
|---|---|
| `0` | 成功。 |
| `1` | 未知错误。 |
| `2` | 请求超时（或 clap 报告的未给出子命令）。 |
| `3` | 未找到（404 —— 例如任务 ID 不存在）。 |
| `5` | 网络错误（无法连接 —— FluxDown 在运行吗？）。 |
| `7` | 中断且仍有未完成下载（`--local` 模式下 Ctrl-C）。 |
| `24` | 鉴权失败（token 缺失或无效）。 |
| `32` | 参数非法（400，或非法输入，如未知的状态过滤器）。 |

## 尺寸后缀

CLI 展示尺寸时一律使用 1024 进制单位（`KiB`、`MiB`、`GiB`）。你输入的任意尺寸都接受 `K`/`M`/`G`/`T` 后缀（大小写不敏感，可带尾随 `B`），同样按 1024 进制解析：`10M` 即 `10 × 1024 × 1024` 字节。

## 与 aria2 及 API 的关系

CLI 有两种角色：远程模式驱动同一套管理 API，独立模式（`add --local`）内嵌引擎脱机下载。若你已有面向 aria2 的工具链，[aria2 兼容 RPC 端点](/docs/zh/api/overview/#curl-示例)可能更合适；面向 AI 客户端则用 [MCP](/docs/zh/api/overview/#mcpmodel-context-protocol)。远程模式与这些入口操作的是同一批任务与队列。Metalink、XML-RPC、以及 aria2 的会话保存/恢复刻意未实现 —— FluxDown 的 SQLite 存储已在重启间持久化了一切。
