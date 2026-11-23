---
description: FluxDown 下一代 daemon/agent/protocol 本机服务边界、状态所有权、JSON-RPC 契约与旧宿主迁移规则
condition: native/**
interruptMode: never
---

你正在修改 FluxDown Rust 本机核心或宿主。GPUI 桌面与 headless 运行于 `native/{protocol,daemon,agent}`；原生 Android/iOS 经 `native/mobile` 复用进程内 daemon + agent 或连接远端 `/rpc`。

## 固定分层

```text
GPUI / WASM Web / CLI
          |
          v
fluxdown-agent  ---- HTTPS/SSE ----> FluxCloud
          |
          | JSON-RPC
          v
      fluxdownd
          |
          v
 fluxdown_engine
```

- `native/engine`：下载领域实现；不知道 UI、FluxCloud 账户和进程传输。
- `native/daemon`（`fluxdown_daemon`，目标二进制 `fluxdownd`）：aria2c 式纯下载核心；拥有下载任务、下载设置、RSS、插件、Webhook、下载 DB 与下载事件。
- `native/agent`（`fluxdown_agent`，目标二进制 `fluxdown-agent`）：官方客户端常驻后端；拥有 FluxCloud Token、设备身份、配置同步、Entitlements、远程任务、UI Gateway，以及系统外壳（托盘、关闭 UI 后的驻留策略、剪贴板监听、完成后关机、开机自启入口）。
- `native/protocol`（`fluxdown_protocol`）：传输无关 wire；只放 DTO、版本、方法名、事件和稳定错误码，不放网络运行时、业务实现、数据库或 UI。
- headless/NAS 由 `fluxdown-agent --server` + `fluxdownd` 承担，Web SPA 走 agent `/rpc`；旧 headless 宿主 `native/server`（Flutter 时代产物）已移除，不得恢复。

## 状态单一所有者

- 下载任务、队列、分组、下载配置的事实源只能是 daemon；agent/UI 不直接读写下载 DB。
- FluxCloud Access/Refresh Token、同步 revision、设备信任与远程任务状态机的事实源只能是 agent；Token 不进入 daemon、GPUI、WASM LocalStorage 或 URL。
- `fluxdown_engine` 只执行下载领域行为；agent 不直接依赖或调用 engine，必须经 daemon 协议。
- UI 只管理表单草稿、选择、路由、滚动、窗口等展示状态；关闭全部 UI 后 daemon 与 agent 的后台状态机仍可继续（是否继续由 agent 的驻留策略决定，UI 不自建托盘或常驻逻辑）。
- 进程退出分两种且不得混用：完全退出走 `system.shutdown`（agent 先关停 daemon，连接以 `service-quit` 关闭，客户端不得重拉）；SIGTERM 只退出收到信号的进程。
- 下载设置云同步由 agent 协调，但最终值必须先由 daemon 校验、持久化和应用；禁止 agent 与 daemon 各存一份可独立修改的下载设置。

## 协议与传输

- 控制面统一 JSON-RPC 2.0 语义；事件用带单调 `sequence` 的 JSON-RPC notification，断线后靠全量 snapshot 恢复。
- Web 使用 HTTP + WebSocket；桌面界面首选每用户 Named Pipe / Unix Domain Socket（`native/agent/src/gateway/ipc.rs`，同一 router、同一鉴权），回环 TCP 保留作回退与兼容 API。传输变化不得分叉方法、DTO、错误码或 UI 业务逻辑。
- 二进制上传/下载、静态资源可走专用 HTTP endpoint；禁止为追求“全 JSON-RPC”把大文件普遍 Base64 化。
- 新增 wire 类型先检查 `native/protocol` 与 `native/api/src/types.rs`，禁止创建同义 DTO；旧 API 迁移时必须一次性迁移调用方并删除旧路径。
- daemon ↔ agent 先按 `native/protocol/src/handshake.rs` 双向挑战应答认证，再做版本与 capability 握手；新版 agent 校验服务端证明后才发客户端证明，不发送长期 token，专用 HTTP 使用会话派生凭据。daemon 保留旧 agent 静态 Bearer 校验兼容；协议不兼容必须显式失败。
- 慢方法只维护 `fluxdown_protocol::method::SLOW_DAEMON_METHODS`，daemon 并发调度与 agent 独立通道共同消费，禁止另立清单。

## 依赖方向

```text
engine <- daemon <- agent
protocol <- daemon / agent / clients
```

- 上图的 `daemon <- agent` 表示运行期 RPC，不表示 agent 可依赖 daemon 实现 crate。
- daemon 不依赖 agent、server 或 `crates/*`。
- agent 不依赖 engine、server 或 GPUI capability crate。
- protocol 不依赖 engine、daemon、agent、api、Tokio/Axum、数据库或 GPUI。
- 官方 UI 默认只连 agent；纯下载第三方客户端可以直连 daemon。

## 迁移纪律

- 原生移动端复用 `EventSink`、`HostSelection`、`ApiHost` 与既有错误类型；禁止复制出第二套引擎 actor 语义。
- 一个 data dir 同时只允许一个 Engine/daemon 写入；禁止 daemon 与其它宿主或 CLI `--local` 共享 DB 并发运行。
- 云状态机只归 agent；不保留双 Token、双 SSE、双 revision 实现。已被当前客户端消费的旧数据迁移与主题兼容必须保留。
- 只为真实迁移切片创建模块；禁止预建空目录、空 trait、占位 handler、假成功响应或无运行行为的二进制。

## 修改前检查

1. UI 全退后仍需继续吗？需要则归 daemon 或 agent，不归 UI。
2. 功能是否需要知道 FluxCloud 账户？需要则归 agent，不归 daemon。
3. 功能是否直接改变下载事实？需要则由 daemon 统一入口调用 engine。
4. 这是 wire 语义还是传输实现？前者归 protocol，后者留在服务端/客户端适配器。
5. 是否恢复了旧 headless 宿主或建立第二套 DTO/状态机？若是，停止并收敛到新边界（Web SPA 同样只消费 `native/protocol` 的 wire 形状，TS 镜像在 `web/src/lib/rpc/protocol/`）。
