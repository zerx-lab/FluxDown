---
title: 服务器部署
description: 从源码构建并运行 headless FluxDown 服务器(fluxdown-agent --server + fluxdownd),了解全部环境变量并安全地对外暴露。
section: headless-server
order: 1
sourceHash: "f2915bfc0e0e"
---

headless 服务器 = `fluxdown-agent --server` 加上同级的 `fluxdownd` 下载守护进程:没有桌面界面、托盘或文件关联。它把同一套 Rust 引擎(HTTP/HTTPS、FTP、BitTorrent、HLS、DASH)通过编译进 `fluxdown-agent` 的 Web 界面和 JSON-RPC 端点(`/rpc`,与桌面客户端同一协议)暴露出来,因此你可以把它跑在 NAS、家庭服务器或 VPS 上,在浏览器里远程管理下载。发行版是**同一目录下的两个二进制**——`fluxdown-agent`(内嵌 Web 界面)与 `fluxdownd`。请把它们放在同一目录:agent 会把 `fluxdownd` 作为子进程拉起,并在收到 `SIGTERM`/`SIGINT` 时一并关停它。

多数部署场景下，预编译 Docker 镜像是最省事的方式——见 [Docker 与 NAS](/docs/zh/headless-server/docker/)。本页介绍从工作区源码用 Cargo 构建并运行，以及对两种方式都适用的配置。

## 构建与运行

agent 在 `native/agent`(包名 `fluxdown_agent`,可执行文件 `fluxdown-agent`),daemon 在 `native/daemon`(包名 `fluxdown_daemon`,可执行文件 `fluxdownd`)。把**两者**编译到同一 target 目录,再以 server 模式启动 agent。在仓库根目录执行:

```bash
# 生产构建(Web 界面经 web-ui feature 内嵌进 agent)
cargo build --release -p fluxdown_agent --features web-ui
cargo build --release -p fluxdown_daemon
# 产物在 target/release/:fluxdown-agent 与 fluxdownd(Windows 下带 .exe)

# 运行(默认监听 [::]:17800,IPv4 + IPv6)
./target/release/fluxdown-agent --server
```

agent 会拉起同级的 `fluxdownd`,由后者打开自己的 SQLite(或 PostgreSQL)数据库并运行下载引擎;agent 则直接从可执行文件内嵌的字节托管 Web 界面——不需要额外的数据库服务、静态文件目录或反向代理就能跑起来。

## 构建 Web 前端

只有自己编译服务器时才需要这一步——官方发行二进制与 Docker 镜像已经内含界面。

Web 界面是 `web/` 目录下独立的 SPA(React 19 + TanStack,用 [Bun](https://bun.sh) 构建)。它的构建产物在**编译期**被嵌入 `fluxdown-agent`,所以必须在编译 agent **之前**就存在:

```bash
cd web
bun install
bun run build      # 输出到 web/dist

cd ..
cargo build --release -p fluxdown_agent --features web-ui   # 把 web/dist 嵌进二进制
```

前端每次改动后都要重新编译 agent——正在运行的二进制永远只提供它编译时那份字节(也可设置 `FLUXDOWN_WEBROOT` 从磁盘目录实时托管)。两个编译期开关:

- `FLUXDOWN_EMBED_WEBROOT`——嵌入其它目录而不是 `web/dist`(CI 就用它,因为 SPA 在单独的 job 里构建)。
- 目录缺失或为空不会让编译失败,只打一条 warning;此时服务器对浏览器请求返回 `503` 说明页,`/rpc` 与 HTTP API 照常工作。

<!-- TODO(screenshot): 浏览器里首次运行「初始化 FluxDown Server」向导的截图 -->

## 环境变量

全部配置在启动时从环境变量一次性读取,没有配置文件。

| 变量 | 默认值 | 说明 |
|---|---|---|
| `FLUXDOWN_BIND` | `[::]:17800` | HTTP/WebSocket 服务监听的 TCP 地址。`[::]` 同时接收 IPv4 与 IPv6（IPv6 DDNS、飞牛 FN Connect 需要），系统不支持 IPv6 时自动退回 `0.0.0.0`；显式写 `0.0.0.0:<端口>` 则只监听 IPv4。 |
| `FLUXDOWN_DATA_DIR` | 平台自动探测(见下表) | 数据根目录。daemon/引擎数据(数据库、日志)直接放在其中,agent 自己的状态放在 `<根目录>/agent`。 |
| `FLUXDOWN_SAVE_DIR` | 未设置——平台下载目录 | 首次启动时播种的默认保存目录(仅在尚无 `default_save_dir` 时由 daemon 写入)。之后在设置页选过的目录永远优先。群晖套件用它指向安装向导里选定的共享文件夹。 |
| `FLUXDOWN_DATABASE_URL` | 未设置——使用数据目录下的 SQLite 文件 | 显式连接串:`sqlite:/path/to/file.db` 或 `postgres://user:pass@host/db`。 |
| `FLUXDOWN_WEBROOT` | 未设置——托管内嵌的 Web 界面 | 可选覆盖:改从该目录托管 SPA,而不用内嵌那份(自定义前端,或热替换 `bun run build` 产物)。**不再**隐式探测可执行文件同级的 `./web`。 |
| `FLUXDOWN_TOKEN` | 未设置——走 Web 首次运行向导 | 可选的预置管理访问密钥(即 Web 界面与 API 使用的 gateway 用户密钥)。仅当尚未设置密钥时采纳(会 trim 首尾空白;须满足下文密钥规则,否则忽略并打警告)。用于 docker-compose / k8s / CI 等无人值守部署跳过向导。若要覆盖已存在的密钥,见下方 `FLUXDOWN_TOKEN_FORCE`。 |
| `FLUXDOWN_TOKEN_FORCE` | 未设置——`FLUXDOWN_TOKEN` 仅播种空密钥 | 真值(`1`/`true`/`yes`/`on`)时,`FLUXDOWN_TOKEN` 每次启动都覆盖库中已存的密钥,而不仅是库中还没有密钥时才生效。适合把密钥完全交给编排系统(Kubernetes Secret、docker-compose env)管理、不希望 Web 界面改的密钥跨重启保留的场景。 |
| `FLUXDOWN_ALLOW_LOCAL_PLATFORM` | 未设置(关闭) | 允许所有已鉴权 WebSocket 客户端在服务器系统上执行「打开文件」/「在文件夹中显示」。浏览器直接连接字面 `localhost` 或环回地址时自动获得该能力,公网反代来源不会。**可远程访问的服务器请保持未设置**:开启后远程用户可触发系统打开已下载文件。 |
| `FLUXDOWN_DEMO` | 未设置(关闭) | 真值(`1`/`true`/`yes`/`on`)开启演示模式:仅允许下载内置生成的 64 MiB 演示文件,适合公开演示。 |
| `FLUXDOWN_DEMO_URL` | 未设置(关闭) | 用指定 URL 覆盖演示模式的内置生成文件,仅该 URL 可下载。 |
| `FLUXDOWN_LANG` | 未设置(回退浏览器语言) | `/ping` 返回的回退语言(`en`/`zh`),供 Web 界面首次加载使用。在浏览器里选过语言的用户始终以本人选择为准。 |
| `FLUXDOWN_MDNS` | 开 | 设备互联 mDNS 广播开关。设为假值可停止在局域网内广播本服务器。 |
| `FLUXDOWN_LINK_NAME` | `FluxDown Server` | 设备互联开启时,其它 FluxDown 客户端看到的本机名称。 |
| `FLUXDOWN_LINK_ADVERTISE_ADDR` | 未设置(自动探测) | 覆盖本服务器在配对与设备信息交换时向其它 FluxDown 设备自报的可回连地址。逗号分隔的 `host[:port]` 或 `http(s)://` URL(缺省端口取实际监听端口)。在 Docker / NAT 后 / 多网卡主机上自动探测到的往往是对端连不上的内部地址,此时用它指定对外可达地址,例如 `192.168.1.5:8080`。 |
| `FLUXDOWN_ANALYTICS` | 开 | 匿名统计开关(仅在带 App-Key 的构建中生效,官方构建带有)。设为假值可关闭。 |
| `FLUXDOWN_LOG_LEVEL` | 未设置——`info` | 未设置 `RUST_LOG` 时的默认 `tracing` 日志级别(`error`/`warn`/`info`/`debug`/`trace`,大小写不敏感)。`RUST_LOG` 存在时始终优先——简单场景用 `FLUXDOWN_LOG_LEVEL` 一键调级,需要按模块精细控制再用 `RUST_LOG`。启动时读取一次,修改需重启生效。 |

未设置 `FLUXDOWN_DATA_DIR` 时,数据目录探测规则与桌面客户端一致:

| 平台 | 目录 |
|---|---|
| Windows(便携版) | 可执行文件同级目录 |
| Windows(安装版) | `%LOCALAPPDATA%\FluxDown\` |
| Linux | `$XDG_DATA_HOME/fluxdown/` |
| macOS | `~/Library/Application Support/fluxdown/` |

headless 部署几乎总是应该显式设置 `FLUXDOWN_DATA_DIR` 为一个固定、有备份的路径,而不是依赖自动探测。

```bash
FLUXDOWN_BIND=0.0.0.0:8080 \
FLUXDOWN_DATA_DIR=/srv/fluxdown/data \
./fluxdown-agent --server
```

## 首次运行:在 Web 界面设置访问密钥

全新安装默认开启兼容 API 分组（接管、aria2、管理 API、MCP），CORS 默认关闭，监听地址只由 `FLUXDOWN_BIND` 决定。访问密钥未设置时，`/download`、`/download/batch` 与 `/jsonrpc`（POST/WS 升级）返回 HTTP 403（`setup required: set the access key first`），管理 API 与 MCP 同样拒绝空密钥；因此首次设置前不能使用 aria2 或脚本接管。Web 页面与首次设置接口仍可访问，`GET /api/v1/setup/status` 报告 `setupRequired`（启动就绪前可能返回 503）。headless 不会套用桌面 LAN/CORS 的自动补 token 策略。

打开 `http://<server-ip>:17800/`。登录页会变成**初始化 FluxDown Server**向导(不是普通登录框):填写访问密钥并确认,可点按钮随机生成,可勾选「记住此设备」,保存后立即登录进主界面——无需重启服务器。

密钥规则(前后端一致):

- 仅 ASCII 可见字符(无空格、无非 ASCII)
- 长度 8–128
- 必须同时包含字母和数字

保存后密钥存放在 agent 状态(`<数据目录>/agent`)中,只要数据目录还在,重启后依然有效。用它来:

- 登录 Web 界面(见[Web 界面](/docs/zh/headless-server/web-ui/))。
- 用 `Authorization: Bearer <token>` 鉴权管理 API 调用(见 [API 总览](/docs/zh/api/overview/))。

采用这一流程(而不是「服务器生成 token 并只打印一次到 stderr」)是因为 NAS(群晖、QNAP、Unraid 等)用户往往看不到容器/套件的 stderr,一次性打印的密钥等于把人锁在门外。

### 无人值守部署

若要跳过向导(docker-compose、Kubernetes、CI),用 `FLUXDOWN_TOKEN` 预置密钥。仅当库中还没有密钥时才会采纳:

```bash
FLUXDOWN_TOKEN='replace-with-strong-key-2026' ./fluxdown-agent --server
```

若要让环境变量始终生效——即使有人在 Web 界面改过密钥——再加上 `FLUXDOWN_TOKEN_FORCE=1`:

```bash
FLUXDOWN_TOKEN='replace-with-strong-key-2026' FLUXDOWN_TOKEN_FORCE=1 ./fluxdown-agent --server
```

### 安全提示

初始化窗口是「谁先访问谁落定」的一次性窗口。在把服务器暴露到不可信网络之前,应先完成初始化,或用 `FLUXDOWN_TOKEN` 预置。

### 重置访问密钥

如果密钥丢失或怀疑已泄露,登录状态下可在 Web 界面(**设置 → 安全与访问**)修改。若已无法登录,用 `FLUXDOWN_TOKEN=<新密钥>` 加 `FLUXDOWN_TOKEN_FORCE=1` 重启一次服务器:环境变量的值会在启动时覆盖库中已存的密钥(须满足上面的密钥规则,否则忽略并打警告)。若希望之后 Web 界面的修改能保留,请随后去掉 `FLUXDOWN_TOKEN_FORCE`。

新密钥立即生效,旧密钥同刻失效。

## 数据库:SQLite 默认,PostgreSQL 可选

默认情况下服务器会在数据目录里打开一个 SQLite 文件,无需任何设置。如果部署多实例或对吞吐量有更高要求,可以改用 PostgreSQL:

```bash
FLUXDOWN_DATABASE_URL=postgres://fluxdown:password@localhost/fluxdown \
./fluxdown-agent --server
```

连接串的 scheme(`sqlite:` 还是 `postgres:`)决定后端,两者共用同一套 schema 与迁移逻辑。服务器自己的日志会掩掉 `FLUXDOWN_DATABASE_URL` 里的凭证段,但这个环境变量本身仍要当作敏感信息对待(避免留在 shell 历史或明文提交到进程管理器配置里)。

## 安全地对外暴露(反向代理与 TLS)

`FLUXDOWN_BIND` 默认是 `[::]:17800`，在所有网络接口上同时监听 IPv4 与 IPv6；桌面则默认只绑回环，但可显式开启 LAN。headless 不受桌面 LAN 开关控制，**网络边界由部署者负责**：

- 管理访问密钥是互联网与"完全远程控制你的服务器"(创建/删除下载、取回任意已完成文件)之间唯一的屏障。把它当 root 密码对待:不要分享、不要打进日志,一旦怀疑泄露就重新生成。
- 如果服务器需要在可信局域网之外访问,把它放在反向代理(nginx、Caddy、Traefik)之后终结 TLS,只对外暴露 HTTPS。Web 界面会把密钥放在 WebSocket 子协议头里(文件下载时放在查询字符串里),明文 HTTP 下会被网络路径上的任何人看到。
- WebSocket 端点(`/rpc`,Web 界面使用)需要代理转发 `Upgrade`/`Connection` 头。最简 nginx 片段:

  ```nginx
  location / {
      proxy_pass http://127.0.0.1:17800;
      proxy_http_version 1.1;
      proxy_set_header Upgrade $http_upgrade;
      proxy_set_header Connection "upgrade";
      proxy_set_header Host $http_host;
  }
  ```

  `/rpc` 握手会校验浏览器的 `Origin`:代理转发的 `Host`(或 `X-Forwarded-Host`)必须是浏览器访问用的主机名,不能改写成内网 IP 或 `127.0.0.1`,否则握手被拒(403),Web 界面只显示"无法连接到 FluxDown 服务"。`$http_host` 原样保留浏览器的主机名与端口;群晖 DSM 反向代理需在「自定义标题」里加上 WebSocket,Nginx Proxy Manager 需勾选 Websockets Support。

- 相比直接把端口暴露给公网(即使配了 TLS),更推荐绑定到私有接口(`FLUXDOWN_BIND=127.0.0.1:17800`,由反向代理挡在前面)或 VPN/Tailscale 地址。

## 作为 systemd 服务运行

Linux 部署的最小 unit 文件示例(按需调整路径与用户):

```ini
[Unit]
Description=FluxDown headless download server (agent + daemon)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=fluxdown
Group=fluxdown
WorkingDirectory=/opt/fluxdown
Environment=FLUXDOWN_BIND=[::]:17800
Environment=FLUXDOWN_DATA_DIR=/var/lib/fluxdown
ExecStart=/opt/fluxdown/fluxdown-agent --server
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

把 release 压缩包解压到 `/opt/fluxdown`,让 `fluxdown-agent` 与 `fluxdownd` 并排放置(Web 界面就在 agent 里面,没有别的文件要装;`systemctl stop` 发送 `SIGTERM`,agent 会连带关停 `fluxdownd`),创建 `fluxdown` 系统用户与 `/var/lib/fluxdown` 目录,然后:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now fluxdown-server   # unit 名取决于你保存的文件名
sudo journalctl -u fluxdown-server -f
```

随后在浏览器打开 `http://<host>:17800/` 完成「初始化 FluxDown Server」向导(无人值守部署可在 unit 里预置 `FLUXDOWN_TOKEN`)。

## 从旧版 `fluxdown-server` 升级

旧版发行物是单个 `fluxdown-server` 二进制。现在的服务器发行物(`fluxdown-server` Docker 镜像、`FluxDown-Server-*` 压缩包/NAS 套件,名称均不变;现随常规 `vX.Y.Z` GitHub release 发布,更早版本在 `server-v*` release)改为 `fluxdown-agent` + `fluxdownd`。

- **数据与设置不变**:沿用同一数据卷 / `FLUXDOWN_DATA_DIR`;`FLUXDOWN_BIND`、`FLUXDOWN_DATA_DIR`、`FLUXDOWN_SAVE_DIR`、`FLUXDOWN_DATABASE_URL`、`FLUXDOWN_TOKEN`、`FLUXDOWN_TOKEN_FORCE`、`FLUXDOWN_WEBROOT`、`FLUXDOWN_LANG`、`FLUXDOWN_DEMO*` 名称与含义都不变,已有访问密钥会沿用。
- **修改启动命令**:`fluxdown-server` → `fluxdown-agent --server`(两个二进制放同一目录)。Docker 镜像与群晖 / QNAP / OpenWrt 套件已代为处理,原地升级即可。
- **已移除的扩展 REST 端点**(只存在于旧服务器):`/api/v1/config`、队列增删改与 start/stop/schedule/order、`/api/v1/stats`、`/api/v1/fs/list`、组件、webhook、日志、`/api/v1/token/regenerate`。请改用内置 Web 界面(现经 `/rpc` 走 JSON-RPC)管理,或直接调用 JSON-RPC 协议。核心 API——`/ping`、`/download`、`/jsonrpc`(aria2)、`/mcp`、`/api/v1/info`、任务、队列列表——保留。
- `FLUXDOWN_SERVER_VERSION` 已移除;上报版本即 agent 的 crate 版本。

## 下一步

- [Web 界面](/docs/zh/headless-server/web-ui/)——在浏览器里登录并管理下载。
- [API 总览](/docs/zh/api/overview/)——用脚本或其它工具自动化操作服务器。
