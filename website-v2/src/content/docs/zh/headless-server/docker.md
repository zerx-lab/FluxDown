---
title: Docker 与 NAS
description: 用预编译 Docker 镜像运行 headless FluxDown 服务器，支持 Docker Compose、CasaOS/ZimaOS、Unraid 与群晖 DSM 原生套件。
section: headless-server
order: 2
sourceHash: "fb5581607fd9"
---

运行 headless 服务器最快的方式是使用预编译 Docker 镜像——无需 Cargo 构建，也无需单独构建 Web 界面。镜像内置 `fluxdown-agent`（内嵌 Web 界面）与 `fluxdownd`，由 `tini` 作为 PID 1 拉起（agent 以子进程启动 daemon，`docker stop` 时一并关停），全部通过一个端口（`17800`）暴露，并把数据库、日志和访问密钥持久化到卷。

镜像：`ghcr.io/zerx-lab/fluxdown-server`（标签：具体版本如 `0.1.54`，或 `latest`）。镜像名不变；入口现为 `tini -- fluxdown-agent --server`。

> 为了部署可复现，建议钉具体版本标签而非 `latest`。

## docker run

```bash
docker run -d \
  --name fluxdown-server \
  --restart unless-stopped \
  -p 17800:17800 \
  -v fluxdown-data:/data \
  -v /path/to/downloads:/root/Downloads \
  ghcr.io/zerx-lab/fluxdown-server:latest
```

- `/data` 存放数据库、日志和访问密钥——请放在持久化卷上。从旧镜像升级时沿用同一个卷即可：数据与访问密钥都会保留。
- `/root/Downloads` 是容器内的默认下载目录（`HOME=/root`）；绑定到你希望写入文件的宿主机路径。

首次访问 `http://<host>:17800/` 时，Web 界面会进入初始化向导，由你自行设置访问密钥（至少 8 位，须同时包含字母和数字）。用该密钥登录 Web 界面，以及为 HTTP API、MCP 端点（`Authorization: Bearer <token>`）和 `/rpc` JSON-RPC 端点鉴权。

首次设置完成前 `/download`、`/download/batch` 与 `/jsonrpc`（POST/WS 升级）返回 HTTP 403（`setup required: set the access key first`）；aria2 客户端和脚本接管不可用。Web 初始化页面与 setup 接口仍可访问，保存合规密钥后无需重启即可按开关使用兼容 API。
在 docker-compose 或其它编排场景中，可用 `FLUXDOWN_TOKEN` 预置密钥并跳过向导。仅在实例尚未设置过密钥时生效；若还设置了 `FLUXDOWN_TOKEN_FORCE=1`，则每次重启都会用它覆盖库中已存的密钥（见[环境变量](/docs/zh/headless-server/setup/#环境变量)）：

```bash
docker run -d \
  --name fluxdown-server \
  --restart unless-stopped \
  -p 17800:17800 \
  -e FLUXDOWN_TOKEN=replace-with-strong-key-2026 \
  -v fluxdown-data:/data \
  -v /path/to/downloads:/root/Downloads \
  ghcr.io/zerx-lab/fluxdown-server:latest
```

## Docker Compose

```yaml
services:
  fluxdown-server:
    image: ghcr.io/zerx-lab/fluxdown-server:latest
    container_name: fluxdown-server
    restart: unless-stopped
    ports:
      - "17800:17800"
    volumes:
      - fluxdown-data:/data
      - ./downloads:/root/Downloads
    # environment:
    #   FLUXDOWN_TOKEN: replace-with-strong-key-2026   # 可选：预置访问密钥，跳过初始化向导
    #   FLUXDOWN_LANG: zh
    #   FLUXDOWN_DATABASE_URL: postgres://user:pass@host:5432/fluxdown

volumes:
  fluxdown-data:
```

```bash
docker compose up -d
```

[服务器部署](/docs/zh/headless-server/setup/)中的全部环境变量在此同样适用——最常用的是 `FLUXDOWN_LANG`（Web 界面默认语言，`en`/`zh`）和 `FLUXDOWN_DATABASE_URL`（指向外部 PostgreSQL 而非内置 SQLite）。

## CasaOS / ZimaOS

FluxDown 已发布为第三方 CasaOS / ZimaOS 应用商店，可一键安装。

在 CasaOS / ZimaOS 中：**应用商店 → 来源 → 添加**，填入：

```
https://cdn.jsdelivr.net/gh/zerx-lab/casaos-appstore@gh-pages
```

然后从商店安装 **FluxDown**。商店源：[zerx-lab/casaos-appstore](https://github.com/zerx-lab/casaos-appstore)。

## Unraid

Unraid Community Applications 模板见 [zerx-lab/unraid-templates](https://github.com/zerx-lab/unraid-templates)。Web 界面地址为 `http://[服务器IP]:17800/`。

## 群晖 NAS（原生 .spk 套件）

每个 Server release 都附带 DSM 原生套件——无需 Docker。四个包覆盖两代 DSM 与两种 CPU 架构：

| 套件 | DSM 版本 | CPU |
|---|---|---|
| `FluxDown-Server-<ver>-synology-dsm7-x64.spk` | DSM 7.0 及以上 | Intel / AMD（x86_64） |
| `FluxDown-Server-<ver>-synology-dsm7-arm64.spk` | DSM 7.0 及以上 | ARM64（rtd1296、rtd1619b、armada37xx 等） |
| `FluxDown-Server-<ver>-synology-dsm6-x64.spk` | DSM 6.0 – 6.2 | Intel / AMD（x86_64） |
| `FluxDown-Server-<ver>-synology-dsm6-arm64.spk` | DSM 6.0 – 6.2 | ARM64 |

不确定机型架构？在[群晖官方 CPU 列表](https://kb.synology.cn/zh-cn/DSM/tutorial/What_kind_of_CPU_does_my_NAS_have)里查你机型的「Package Arch」列：`x86_64` 家族选 x64 包，`armv8` 家族选 arm64 包。更老的 `armv7`/`i686` 机型不支持。

### 安装

1. 打开**套件中心 → 设置 → 常规**，把**信任层级**设为**任何发行者**。这一步是必需的：套件未经签名——DSM 7 已彻底移除第三方套件签名机制，只有通过群晖官方套件中心分发的套件才带「已验证」状态。
2. **套件中心 → 手动安装**，选择 `.spk`，按向导完成。DSM 7 上向导会询问**用于下载的共享文件夹**（默认 `FluxDown`）：不存在则自动创建，并在每次启动时把读写权限授予套件用户，装完即可直接作为保存目录使用。
3. 启动套件后，在套件中心点**打开**——直达端口 `17800` 的 Web 界面（`http://<NAS-IP>:17800`）。

### 首次运行访问密钥

首次打开 Web 界面（`http://<NAS-IP>:17800`）时，初始化向导会要求你自行设置访问密钥（至少 8 位，须同时包含字母和数字）。密钥持久化在套件自己的数据库里，重启与升级后依然有效。之后可在**设置 → 安全与访问**查看或更换。

### 权限与数据位置

- **DSM 7** 上服务以专属低权限套件用户 `sc-fluxdown` 运行（DSM 7 平台强制要求——套件不允许再以 root 运行）；**DSM 6** 上以 root 运行。
- 数据库、日志与访问密钥位于 `/var/packages/FluxDown/var`。DSM 7 上下载默认落到安装向导里选定的共享文件夹；DSM 6 上默认为平台下载目录。
- 套件用户默认**对其他任何共享文件夹都没有权限**。如果保存目录选择器进入 `/volume1/<共享文件夹>` 时提示「服务进程无权读取此目录」（或看起来是空的），先授权：**控制面板 → 共享文件夹 → 编辑 → 权限**，把用户下拉切到**系统内部用户**，给 **sc-fluxdown** 读写权限。DSM 6 以 root 运行，无需授权。

### 升级与卸载

升级即手动安装更新版本的 `.spk` 覆盖安装——`var` 里的数据库、访问密钥与设置全部保留。套件现在包含 `fluxdown-agent` + `fluxdownd`（启动脚本运行 `fluxdown-agent --server`，日志文件仍为 `var/fluxdown-server.log`）。在套件中心卸载会停止服务并移除套件。

## 飞牛 fnOS（原生 .fpk 应用）

每个 Server release 都附带飞牛 fnOS 原生应用包——无需 Docker：

| 应用包 | CPU |
|---|---|
| `FluxDown-Server-<ver>-fnos-x64.fpk` | Intel / AMD（x86） |
| `FluxDown-Server-<ver>-fnos-arm64.fpk` | ARM64 |

### 安装

1. 打开**应用中心 → 手动安装**，选择对应架构的 `.fpk`，按提示完成安装（手动安装的第三方应用未经飞牛应用中心审核）。
2. 安装后启动应用，点击桌面上的 **FluxDown** 图标——在新标签页打开端口 `17800` 的 Web 界面（`http://<NAS-IP>:17800`）。
3. 首次打开时设置访问密钥（至少 8 位，须同时包含字母和数字），之后可在**设置 → 安全与访问**更换。

### 权限与数据位置

- 服务以专用应用用户 `fluxdown` 运行（非 root）。
- 安装时系统会自动创建共享文件夹 `fluxdown` 并授予应用读写权限，它是默认保存目录。
- 要保存到其他共享文件夹，在**应用设置 → 授权目录**里为 FluxDown 添加该目录；未授权的目录在保存目录选择器中会提示「服务进程无权读取此目录」。
- 数据库、访问密钥与日志（`fluxdown-server.log`）位于应用数据目录 `/var/apps/fluxdown/var`。

### 升级

应用内更新只会提示新版本，不会替换二进制：在应用中心手动安装更新版本的 `.fpk` 覆盖安装即可，数据库、访问密钥与设置保留。

### 远程访问（DDNS / FN Connect）

FluxDown 默认在 `17800` 端口同时监听 IPv4 与 IPv6，飞牛 DDNS（常解析到 IPv6）直连与 FN Connect 转发都能访问 `http://<域名>:17800`，无需额外配置反向代理或 WebSocket 透传。若公网打不开：

- 在 NAS 上执行 `ss -ltn | grep 17800`，应显示 `*:17800` 或 `[::]:17800`；只显示 `0.0.0.0:17800` 说明仍是旧版本，请升级 `.fpk`。
- DDNS 解析到 IPv4 时，需要在路由器把 `17800` 端口转发到 NAS，且宽带要有公网 IPv4。
- 检查光猫 / 路由器的 IPv6 入站防火墙与飞牛防火墙是否放行 `17800`。

## 安全地对外暴露

镜像在容器内绑定 `[::]:17800`（IPv4 + IPv6，容器无 IPv6 时自动退回 `0.0.0.0`），映射到宿主机。与任何 headless 部署一样，访问密钥是守护完整远程控制权的唯一屏障——在把它暴露到可信局域网之外前，请先阅读[反向代理与 TLS 指引](/docs/zh/headless-server/setup/)。

## 下一步

- [Web 界面](/docs/zh/headless-server/web-ui/)——在浏览器里登录并管理下载。
- [API 概览](/docs/zh/api/overview/)——用脚本或其他工具自动化服务器。
