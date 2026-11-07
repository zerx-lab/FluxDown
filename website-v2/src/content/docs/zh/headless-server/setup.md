---
title: 服务器部署
description: 从源码构建并运行 headless FluxDown 服务器(fluxdown-agent --server + fluxdownd),了解全部环境变量并安全地对外暴露。
section: headless-server
order: 1
sourceHash: "d8748e5cddf6"
---

headless 服务器 = `fluxdown-agent --server` 加上同级的 `fluxdownd` 下载守护进程:没有桌面界面、托盘或文件关联。它把同一套 Rust 引擎(HTTP/HTTPS、FTP、BitTorrent、HLS、DASH)通过编译进 `fluxdown-agent` 的 Web 界面和 JSON-RPC 端点(`/rpc`,与桌面客户端同一协议)暴露出来,因此你可以把它跑在 NAS、家庭服务器或 VPS 上,在浏览器里远程管理下载。发行版是**同一目录下的两个二进制**——`fluxdown-agent`(内嵌 Web 界面)与 `fluxdownd`。请把它们放在同一目录:agent 会把 `fluxdownd` 作为子进程拉起,并在收到 `SIGTERM`/`SIGINT` 时一并关停它。

多数部署场景下，预编译 Docker 镜像是最省事的方式——见 [Docker 与 NAS](/docs/zh/headless-server/docker/)。Windows 直接用预编译 zip，无需编译——见下一节。本页其余部分介绍从工作区源码用 Cargo 构建并运行，以及对所有方式都适用的配置。

## Windows:直接运行预编译包

FluxDown 本身不需要编译,也不需要安装,但需要一个压缩包里没有的运行库:请先安装 **Microsoft Visual C++ 2015–2022 Redistributable**(x64 主机装 x64,ARM64 主机装 ARM64),到 Microsoft Learn 的 [Latest supported Visual C++ Redistributable downloads](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist) 页面下载。干净的 Windows 上没有它,缺了它两个可执行文件都会立即退出。然后:

1. 从[下载页](/zh/download/)或 [GitHub Releases](https://github.com/zerx-lab/FluxDown/releases) 下载 `FluxDown-Server-<版本>-windows-x64.zip`(ARM 设备用 `windows-arm64`)。压缩包里是一个 `fluxdown-server-<版本>-<平台>\` 文件夹,内含 `fluxdown-agent.exe` 与 `fluxdownd.exe`。把这两个文件移到一个**不带版本号**的固定目录,例如 `D:\FluxDown\server\`,**两者必须留在同一目录**(路径固定,下文的启动脚本和防火墙规则才不会随升级失效)。
2. 在 PowerShell 里进入该目录,先设置环境变量再启动(变量含义见下文[环境变量](#环境变量),都可省略):

   ```powershell
   cd D:\FluxDown\server
   $env:FLUXDOWN_DATA_DIR = "D:\FluxDown\data"      # 数据目录:建议固定、可备份
   $env:FLUXDOWN_SAVE_DIR = "D:\Downloads"          # 首次启动的默认保存目录
   .\fluxdown-agent.exe --server
   ```

3. 浏览器打开 `http://localhost:17800/`,在「欢迎使用 FluxDown」页面设置访问密钥(见[首次运行](#首次运行在-web-界面设置访问密钥))。

几点和 Linux 不同:

- **先决条件:Microsoft Visual C++ 运行库。**压缩包不带 VC++ 运行库。如果进程启动后马上消失,或 Windows 报找不到 `VCRUNTIME140.dll` / 退出码 `0xC0000135`,就是缺它:按上文安装 Redistributable。(下文的计划任务部署里这个故障是无声的:启动脚本只会每 10 秒重试一次。)
- Windows 发布版的 `fluxdown-agent.exe` 是 GUI 子系统程序:**不弹控制台窗口,也没有控制台日志输出**,PowerShell 里执行完命令会立即返回,服务在后台运行。能否访问以浏览器是否打开得了 `http://localhost:17800/` 为准,进程可在任务管理器或 `Get-Process fluxdown-agent, fluxdownd` 里看到。
- 默认监听 `[::]:17800`(所有网卡)。要让局域网内其它设备访问,需放行 Windows 防火墙的入站规则(管理员 PowerShell;`-Profile Private` 让公用网络下仍保持关闭):

  ```powershell
  New-NetFirewallRule -DisplayName "FluxDown Server" -Direction Inbound -Program "D:\FluxDown\server\fluxdown-agent.exe" -Action Allow -Profile Private
  ```

  Windows 经常把新网络归为**公用**,此时这条规则不生效,局域网访问会无声失败。先用 `Get-NetConnectionProfile` 确认(看 `NetworkCategory`);若显示 `Public` 且你信任该网络,用 `Set-NetConnectionProfile -InterfaceAlias "<列表里的名称>" -NetworkCategory Private` 改成专用。

  只在本机使用时设 `$env:FLUXDOWN_BIND = "127.0.0.1:17800"`。对外暴露前务必先完成访问密钥初始化,见下文[安全地对外暴露](#安全地对外暴露反向代理与-tls)。若同一台机器上还装了 FluxDown 桌面版,请给服务器换一个端口(例如 `$env:FLUXDOWN_BIND = "[::]:17900"`),避免两者在 17800 上冲突。
- 未设置 `FLUXDOWN_DATA_DIR` 时,数据位置**随运行它的账户而定**,并且分在两处:引擎数据在 `%LOCALAPPDATA%\FluxDown\`,agent 状态(含访问密钥与设备身份)在 `%APPDATA%\zerx\FluxDown\data\agent`。请始终显式设置 `FLUXDOWN_DATA_DIR`(引擎数据在其根目录,agent 状态在 `<根目录>\agent`),这样备份、迁移才不会漏掉东西。打算之后改用下文的计划任务时,请让这次试跑使用另一个数据目录,或按下文「已经手动运行过」的步骤处理:首次启动会把数据目录锁给运行它的账户。
- Windows 没有 `SIGTERM`,而这个程序没有控制台,无法用 Ctrl+C 退出;**停止就是强制结束进程**。先在 Web 界面暂停任务,再在 PowerShell 里执行(按路径过滤,不会误伤本机安装的 FluxDown 桌面版;服务以其它账户运行时请用管理员 PowerShell):

  ```powershell
  Get-Process fluxdown-agent, fluxdownd -ErrorAction SilentlyContinue |
    Where-Object Path -like 'D:\FluxDown\server\*' | Stop-Process
  ```

### 开机自启 / 常驻

FluxDown Server 本身**不是原生 Windows 服务**(没有 `sc create` 能用的服务入口)。需要开机自动运行时,用系统自带的「任务计划程序」。

> **不要用 `SYSTEM` 或管理员身份运行。**访问密钥等同于整台主机的完全控制权:Web 界面可以把保存目录设到任意路径,服务端没有保存目录沙箱,拿到密钥(或服务出现漏洞)的人就能往 `System32` 或全局启动目录写文件,并以服务的权限运行。请使用专用的标准(非管理员)本地账户——与 Linux systemd 示例里专用 `fluxdown` 用户是同一个最小权限思路。

**1. 创建专用账户并授予目录权限**(管理员 PowerShell,只需一次):

```powershell
$cred = Get-Credential -UserName fluxdown -Message "为 FluxDown 专用账户设置密码"
New-LocalUser -Name fluxdown -Password $cred.Password -PasswordNeverExpires -UserMayNotChangePassword
Add-LocalGroupMember -Group Users -Member fluxdown

New-Item -ItemType Directory -Force D:\FluxDown\data, D:\Downloads | Out-Null
icacls D:\FluxDown\data /setowner fluxdown
icacls D:\FluxDown\data /grant "fluxdown:(OI)(CI)F"    # 数据目录:完全控制
icacls D:\Downloads     /grant "fluxdown:(OI)(CI)M"    # 保存目录:修改权限

# 服务器目录:只有 SYSTEM 和 Administrators 保留完全控制,fluxdown 账户只有读取和执行
icacls D:\FluxDown\server /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "fluxdown:(OI)(CI)RX"
```

这个账户需要对数据目录有**完全控制**权限,对每个下载目录有写权限,对 `D:\FluxDown\server\` 只需读取和执行。数据目录的完全控制不能省:`fluxdownd` 首次启动时会把数据目录收紧为只有运行它的账户可访问(`icacls … /inheritance:r`),这需要「修改权限」这一项,而「修改」权限里没有它;同时把目录所有者交给该账户,则与目录的创建方式无关。缺了它,daemon 起不来,启动脚本每 10 秒重启一次,却没有任何提示。因此启动之后**只有 `fluxdown` 账户能访问数据目录**,备份请在管理员 shell 里进行(被拒绝时先执行 `takeown /f D:\FluxDown\data /r /a`)。服务器目录那条 `icacls` 去掉了该目录的权限继承:新建的数据盘根目录通常给 *Authenticated Users* 「修改」权限并向下继承,不收紧的话专用账户(以及任何本地用户)都能改写程序文件。请不要省略。

**如果已经用这个数据目录手动运行过**(上一节),那次运行已经把 `D:\FluxDown\data` 及其 `agent\` 子目录锁给了试跑用的账户,向导里设置的访问密钥也存放在那里,`fluxdown` 读不到。请先停止进程(用上文的命令),再在管理员 shell 里恢复继承,然后重新执行上面两条 `icacls D:\FluxDown\data`。或者让试跑使用另一个数据目录。

```powershell
takeown /f D:\FluxDown\data /r /a    # 把所有者改为 Administrators;下面的重置不报拒绝访问时可跳过
icacls D:\FluxDown\data /reset /T /C
```

计划任务以「批处理作业」方式登录,这项权限默认只有 Administrators、Backup Operators 和 Performance Log Users 拥有。如果注册或启动任务时报登录错误(`0x80070569`),请把这项权限授予该账户:

- **Windows 专业版 / 企业版 / 教育版:**`secpol.msc` → 本地策略 → 用户权限分配 →「作为批处理作业登录」→ 添加 `fluxdown`。
- **Windows 家庭版**没有 `secpol.msc`,改用 `secedit` 把该账户的 SID 加进策略(管理员 PowerShell):

  ```powershell
  $sid = (Get-LocalUser fluxdown).SID.Value
  $inf = "$env:TEMP\rights.inf"
  secedit /export /cfg $inf /areas USER_RIGHTS
  (Get-Content $inf) -replace '^(SeBatchLogonRight\s*=.*)$', ('$1,*' + $sid) | Set-Content $inf -Encoding Unicode
  secedit /configure /db "$env:TEMP\rights.sdb" /cfg $inf /areas USER_RIGHTS
  Remove-Item $inf, "$env:TEMP\rights.sdb"
  ```

**2. 创建启动脚本** `D:\FluxDown\server\start-server.cmd`(环境变量写在这里,计划任务本身带不了环境变量;放在已收紧权限的服务器目录里,其它本地用户就改不了专用账户要执行的脚本)。请用管理员身份创建,例如以管理员身份启动记事本。循环会在服务退出 10 秒后重新拉起它,这就是崩溃自愈:

```bat
@echo off
set FLUXDOWN_DATA_DIR=D:\FluxDown\data
set FLUXDOWN_SAVE_DIR=D:\Downloads
cd /d D:\FluxDown\server
:loop
start "" /wait fluxdown-agent.exe --server
ping -n 11 127.0.0.1 >nul
goto loop
```

`start /wait` 让脚本一直等到服务退出,计划任务因此能把它当作「仍在运行」;`ping -n 11` 相当于暂停 10 秒,与 `timeout` 不同,没有控制台时也能工作。

**3. 注册任务**(管理员 PowerShell,**与步骤 1 在同一个会话里执行**,因为用到 `$cred`;若是新开的会话,先执行 `$cred = Get-Credential -UserName fluxdown -Message "输入 FluxDown 专用账户的密码"`。开机即启动、不依赖登录、以专用账户的标准权限运行、不限制运行时长、接电池也照常运行、用正常 CPU 优先级而不是计划任务默认的 7(低于正常)):

```powershell
$action   = New-ScheduledTaskAction -Execute "D:\FluxDown\server\start-server.cmd"
$trigger  = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds 0) -StartWhenAvailable `
  -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -Priority 5
Register-ScheduledTask -TaskName "FluxDown Server" -Action $action -Trigger $trigger -Settings $settings `
  -User "$env:COMPUTERNAME\fluxdown" -Password $cred.GetNetworkCredential().Password -RunLevel Limited

Start-ScheduledTask -TaskName "FluxDown Server"   # 立即启动,不必重启
```

任务计划程序自带的 `-RestartCount` / `-RestartInterval` 只在任务**启动失败**时才重试;动作一旦成功启动,之后无论以什么方式退出都算这次运行已完成,因此恢复不了崩溃的服务。所以循环放在脚本里。

说明:

- **下载目录在网络共享上:**映射盘符(如 `Z:\`)只存在于交互会话里,开机任务看不到。请改用 UNC 路径(`\\nas\share\downloads`)作为保存目录,并确保共享端给 `fluxdown` 账户授权(例如在 NAS 上建一个同名同密码的账户)。不要为此改用 `-LogonType S4U`:Microsoft 文档明确说明 S4U 登录无法访问网络和加密文件。如果只希望在你登录期间运行,可以改用 `-AtLogOn` 触发器配合你自己的账户(`-LogonType Interactive`)。
- **停止或取消:**先执行 `Stop-ScheduledTask -TaskName "FluxDown Server"`(否则循环会把服务重新拉起),再用上文的 `Get-Process … | Stop-Process` 结束残留进程(结束计划任务不一定带走后台进程);`Unregister-ScheduledTask -TaskName "FluxDown Server" -Confirm:$false` 删除任务。
- **升级:**因为服务器目录对 `fluxdown` 账户是只读的(步骤 1),这种部署下**不要使用 Web 界面的应用内更新**:账户没有写权限,即使有,它自动重启服务也会与循环脚本互相抢占。请手动升级:`Stop-ScheduledTask`、结束进程,用新压缩包里的 `fluxdown-agent.exe` 与 `fluxdownd.exe`(在管理员 shell 里)覆盖 `D:\FluxDown\server\` 里的同名文件,再 `Start-ScheduledTask`。数据目录不变。
- 若需要原生 Windows 服务,可用第三方服务包装器,但这不是官方支持的部署方式。

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
