# FluxDown — AI 工作契约（核心）

多协议下载管理器（IDM 的免费替代），官网 <https://fluxdown.com>（旧域名 `fluxdown.zerx.dev` 长期保留）。**一套 Rust 引擎 `fluxdown_engine` + 多宿主 + 多客户端**：PC = GPUI（`fluxdown-desktop → fluxdown-agent → fluxdownd → fluxdown_engine`）；移动端 = `mobile/Android`（Compose + Flux Lumen，零 Material）与 `mobile/FluxDown`（SwiftUI + Liquid Glass），经 UniFFI（`native/mobile`）共享核心；NAS / 服务器 = `fluxdown-agent --server` + `fluxdownd`（React Web SPA 经 agent `/rpc`）。另有 CLI、WXT 浏览器扩展、Tampermonkey 用户脚本、JS 插件系统与 MCP / REST / aria2 API。旧 Flutter 工程、hub 与 `native/server` 已移除（维护边界见 `.omp/RULES.md`）。

---

## 0. 上下文分层 · 深挖索引

本文件只放**会话开头就必须在场**的东西：架构缝、跨域硬不变式、命令、单一事实源坐标、各层索引。其余内容按 omp 的加载机制放到「需要它的那一刻」才进上下文的层：

|层|载体|何时进上下文|
|---|---|---|
|粘性红线|`.omp/RULES.md`|每次请求（长会话不掉）：git 授权、分支模型、用户拍板的产品决定|
|核心契约|本文件|会话开头|
|路径规则|`.omp/rules/*.md`（`globs` + `interruptMode: never`）|**编辑命中路径时自动注入**：镜像契约 `mirror-*`（§5）、领域细则、补丁陷阱；也可随时 `read rule://<名>`|
|流式拦截|`.omp/rules/*.md`（正则 / AST 条件）|输出命中违规写法时打断：unwrap、unsafe、忽错、禁用命令、硬编码文案|
|执行前拦截|`.omp/hooks/pre/guard.ts`（判定在 `hooks/lib/check_bash.mjs`）|工具调用前硬拒：tag 推送、`--no-verify`、强推、测试 `--workspace`、子代理全量门禁|
|知识附录|`.omp/knowledge/*.md`|按需 `read`|
|Cursor 薄适配|`.cursor/**`|Cursor 会话：规则 / hooks / skills / commands 只指向 `.omp/`，**不复制正文**（约定见 `.cursor/README.md`）|

|要查什么|读哪个|
|---|---|
|架构全图、顶层目录树、版本来源|`.omp/knowledge/README.md`|
|状态码 / DB 表与字段语义、6 种协议、引擎子系统、插件系统、受管组件|`.omp/knowledge/engine.md`|
|HTTP API 路由与鉴权、agent / cli / nmh / updater、headless env 与路由、本机端口、空闲静默|`.omp/knowledge/hosts-and-api.md`|
|GPUI、原生 Android / iOS、主题兼容、云同步、扩展、用户脚本、Web SPA、官网|`.omp/knowledge/clients.md`|
|日志系统、发布 / 打包流水线、仓库维护自动化、域名与站点、设计文档实现状态|`.omp/knowledge/ops.md`|
|**「要加 X 改哪里」全表 —— 动手前先查**|`.omp/knowledge/extension-points.md`|
|按领域收拢的已验证踩坑 —— **改某领域前先读对应小节**，修复新坑同回合补一条（带坐标）|`.omp/knowledge/pitfalls.md`|

改本文件、`.omp/**` 或 `.cursor/**` 时 `rule://agent-docs-maintenance` 自动注入（分层归属、维护约定、同改矩阵）。项目知识只进上述各层（随仓分发），不依赖本机 omp memory 后端；Cursor 侧契约仍以 `.omp/` 为唯一事实源。

**单一事实源坐标**（要改契约先来这里）：

|契约|位置|
|---|---|
|设置键|`native/protocol/src/{daemon_config,settings}.rs` 的规范目录；下载设置存引擎 `db.rs` 的 `config` 表，客户端偏好由 agent 管理|
|DB schema|`native/engine/src/db.rs`：`SQLITE_SCHEMA` + `POSTGRES_SCHEMA` + `add_column_if_missing`|
|HTTP 契约|`native/api/src/types.rs`（wire，camelCase）+ `routes.rs`；规范文件 `website-v2/public/openapi.json`|
|本机服务协议 / daemon 双向鉴权|`native/protocol/src/{rpc,handshake}.rs`：角色、版本、挑战应答与会话 HTTP 凭据；daemon 保留旧 agent 静态 Bearer 兼容，新 agent 不发送长期密钥|
|本机端点 / 启动失败记录|`native/protocol/src/local_service.rs`：`gateway-endpoint.json`、`daemon-endpoint.json`（各与对应 token 同目录）、`agent-startup-failure.json`|
|慢方法调度|`fluxdown_protocol::method::SLOW_DAEMON_METHODS`：daemon 并发调度与 agent 独立通道共用，禁止另立清单|
|日志文本脱敏|`fluxdown_logfile::SANITIZE_PATTERNS`（唯一正则源）；结构化快照脱敏另见 `native/daemon/src/log_redact.rs`|
|移动 FFI|`native/mobile` 的 UniFFI 导出与 DTO；Kotlin / Swift 绑定由各原生工程脚本生成，不手改生成物|
|headless env / 访问密钥策略|`native/agent/src/server_mode.rs`（`ServerConfig::from_lookup`、`validate_access_key`）|
|i18n 基线|共享 `assets/i18n/{en,zh}.json`，GPUI / Web / Android / iOS 共同消费（`rule://i18n-baseline-zh-en`）|
|版本号|CI 按 `v*` tag 注入 `FLUXDOWN_APP_VERSION`，运行期基准 `fluxdown_protocol::APP_VERSION`（回退 `CARGO_PKG_VERSION`）|

---

## 1. 执行目录

路径与命令以仓库根 `FluxDown/` 为基准。cwd 在上级目录时：路径前置 `FluxDown/`，命令带目录限定（bash `cwd`、`--manifest-path FluxDown/…`），git 一律 `git -C FluxDown …`；bash 的 cwd 不跨调用保持。同级仓库 FluxCloud / fluxdown-plugin-index / fluxdown-sniff-rules 经 `.omp/config.yml` 的 `workspace.additionalDirectories` 自动挂载，跨仓契约以对方仓源码为准。

---

## 2. 命令速查

```bash
# ── 构建 / 静态检查 ──
cargo check -p <crate> --lib          # 验证编译按 crate（不要整 workspace）
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings   # 提交前必过；含测试目标
git config core.hooksPath .githooks   # 每个 clone 一次：pre-commit 拒收未 rustfmt 的暂存 .rs，失败就 cargo fmt 后重新 add，禁止 --no-verify
python3 scripts/lint-changed.py       # 改动行严格 lint 棘轮（字符边界切片 / panic! / dbg! 阻断）
python3 .github/scripts/check_ci_paths.py   # ci.yml 路径过滤覆盖自检
# omp 会话内 `/verify [范围]` 按改动范围串行跑全部门禁并报告（.omp/commands/verify.md）

# ── 测试（按 crate/过滤，禁止 --workspace）──
cargo nextest run -p <crate> <filter> # engine / api / agent（server 模式 --features web-ui）/ cli / mobile / daemon / protocol
cargo test -p fluxdown_engine --features plugins,components --test engine plugin_ffmpeg::   # 真实执行经 FLUXDOWN_TEST_FFMPEG / FLUXDOWN_TEST_YTDLP=<abs> 注入
cargo check -p fluxdown_engine        # 不带 feature：验证关插件时主链路零变化
PG_TEST_URL=postgres://postgres:pw@localhost/postgres cargo test -p fluxdown_engine -- --ignored pg_smoke

# ── 运行 ──
cargo build -p fluxdown_daemon && cargo run -p fluxdown_agent --features web-ui -- --server   # headless（fluxdownd 须与 agent 同目录）
cargo run -p fluxdown_cli -- add <url> --local   # 内嵌引擎独立下载
cd web && bun run dev                 # Web SPA :5173（代理到 :17800 的 agent --server）
cd website-v2 && bun run dev          # 官网 :4321
cd fluxDown && npm run dev            # 扩展（Chrome）；dev:firefox / build / zip

# ── 原生移动端 ──
cd mobile/Android && ./gradlew :core:testDebugUnitTest :bridge:testDebugUnitTest :app:assembleDebug   # JAVA_HOME = Android Studio 自带 JBR
mobile/FluxDown/scripts/build-core.sh [--release] [--flavor store|full]   # 改 Rust 后必须重跑；store = --no-default-features 剔除 BT/eD2K
cd mobile/FluxDown/FluxKit && xcodebuild test -scheme FluxKit-Package -destination 'platform=iOS Simulator,name=iPhone 18 Pro'
cd mobile/FluxDown && xcodebuild build -project FluxDown.xcodeproj -scheme FluxDown -destination 'generic/platform=iOS Simulator' && python3 scripts/check-i18n.py
```

打包 / 签名 / 发版见 `.omp/knowledge/ops.md` 与 `release` skill；OpenAPI 重生成见 `rule://api-openapi-sync`；图标 `bun scripts/gen_icons.ts`。

---

## 3. 架构缝

|Trait|定义位置|方向|职责|
|---|---|---|---|
|`EventSink`|`engine/src/events.rs`|引擎→宿主|进度 / 分段 / 队列 / 组变化事件推送|
|`HostSelection`|`engine/src/selection.rs`|引擎→宿主|HLS 画质 / BT 文件 / 插件 variant / 「文件已存在」询问（tristate；新方法给默认实现，无交互宿主自动回退）|
|`ApiHost`|`native/api/src/service.rs`|客户端→引擎|REST / aria2 / MCP 能力面；必需方法 + 可默认降级方法|

- 生产链路：GPUI 与 headless 共用 agent + daemon；Android / iOS 经 `native/mobile` 复用进程内 daemon + agent 或远端 `/rpc`。`fluxdown_api` 只依赖 `&dyn ApiHost`（agent 侧 `AgentApiHost` 转发 daemon RPC）；CLI 默认 HTTP，`add --local` 内嵌引擎。
- 服务边界：daemon = 纯下载核心；agent = 账户 / 云同步 / 设备协同、官方 UI Gateway 与系统外壳（托盘、关闭 UI 后驻留）；两者共享 `native/protocol` 的 JSON-RPC 语义（细则 `rule://local-service-architecture`）。
- 并发：current_thread tokio actor 串行化写；每个下载独立 task + CancellationToken；插件 resolve 永不阻塞 actor（off-actor spawn + 通道回流）。
- 客户端捕获三条前端（扩展、用户脚本、桌面）汇入 agent 同一捕获队列。架构图见 `.omp/knowledge/README.md`。

---

## 4. crate 边界与硬不变式

**crate 边界**
- `fluxdown_engine`：零 UI / FFI / axum 依赖，只经 `EventSink` / `HostSelection` 与宿主解耦。
- `fluxdown_api`：只依赖 protocol DTO 与 `&dyn ApiHost`，零引擎依赖。
- `fluxdown_protocol`：唯一传输无关 wire 层，只依赖序列化 / 纯类型。`fluxdown_engine_protocol`：引擎模型 ↔ protocol DTO 的无状态无损转换，API / agent / UI 不依赖它。
- `fluxdown_daemon`：独占 engine / 下载 DB，拥有任务、队列、组、下载设置、RSS、插件、Webhook 与选择。`fluxdown_agent`：UI Gateway 与 FluxCloud owner（Token、设备身份、同步、远程任务、捕获、兼容 API），只经 protocol RPC 调 daemon。
- `fluxdown_mobile`：移动端唯一 Rust 入口（UniFFI `FluxCore` / `HostSession`）；握手、游标、缓冲、重同步、重连退避都在这里，Kotlin / Swift 只应用已接受的信号；手写零 unsafe。`fluxdown_link`：局域网直连 L1 协议，零引擎 / DB / UI 依赖。`fluxdown_logfile`：desktop 与 agent 共用诊断日志，零第三方依赖。
- `crates/*`（GPUI）：`app` 是唯一 composition root，capability 只依赖本地端口与 protocol DTO（`rule://gpui-crate-architecture`）。
- **feature 门控**：`plugins` / `components` 默认关（desktop / server 开，mobile / CLI 关），关闭时下载主链路零行为变化（no-op `PluginManager`）。`bt` / `ed2k` 默认开；iOS 商店 flavor 以 `--no-default-features` 剔除 librqbit 与 eD2K，建此类任务返回明确拒绝。**商店包对本机与远端主机一律不含 P2P 与插件 / 组件功能**（入口、页面、选择处理、scheme / 文档类型编译期剔除；曾因「远端主机才出现的功能」被以 App Review 5.6 拒审），新增相关 iOS 界面同样门控（Swift `FULL_FLAVOR`）。

**编译期陷阱**
- GPUI = crates.io gpui-fast + gpui-kit 同一发布版（各包开 `gpui-fast` feature）、engine 的 `quickjs-jit` 与 gpui-shell 同版本、根 `[patch.crates-io]` 的 librqbit 系补丁 / `third_party/librqbit-dualstack-sockets` 与 `profile.release` 不设 `panic="abort"`：改 `Cargo.toml` / `Cargo.lock` / `third_party/` 前读 `rule://workspace-patches`。
- **gpui-fast 保留模式**：render 读的状态必须在 entity / global / `ListState` / `ScrollHandle` 内，否则改了要 notify 读者 view；父 view notify 不再重建子 view；渲染期不得无条件写 entity / global。
- **headless Web UI 编译期内嵌**（`web-ui` feature，`FLUXDOWN_EMBED_WEBROOT` 缺省 `web/dist`）：改前端必须先 `cd web && bun run build` 再重编 agent；Vite 别名引用 `assets/i18n` 与 `website-v2/src/lib/gpui-theme`，打包上下文须包含两处。

**运行期不变式**
- 持有引擎的宿主 actor **必须** drain `resolve_rx` 与 `plugin_retry_rx`，否则命中 resolver 的下载永久挂起。
- pg 字节列必须 `BIGINT`（`INTEGER` >2GB 静默截断）；新表同时进 `SQLITE_SCHEMA` + `POSTGRES_SCHEMA` + 迁移。
- 每个 console 子进程 spawn 都包 `proc::no_console_window`（Windows 闪黑窗）。
- **anyhow 错误一律 `{e:#}`**：`{e}` 吞掉根因（BT `dht.json` 撞 Windows 端口排除区间致全部 BT 任务失败就是这么被吞的）。
- **BT 判定只认 `magnet:` 与 `torrent-file://` 哨兵**；HTTP `.torrent` 直链会下回种子文件，要真下载须抓字节后以 `NewTaskSpec::torrent_file_bytes` 建任务。「复制链接」类 UI 读 `origin_url`，空则回退 `url`。
- **RSS 无人值守**：任何「需要用户点一下才能继续」都是 bug。建任务即全选 + `unattended=1`；引擎自发建任务也须由宿主发布完整任务事实；手动「重新下载」对任何状态放行。
- **一个 data_dir 只有一个引擎写入者**：`Db::open_exclusive` / `connect_exclusive` 持 `<data_dir>/engine.lock`（PG 另加 advisory lock），第二个打开者得 `DbError::WriterLeaseHeld`；CLI `--local` 报「App 正在运行」退出；`fluxdown_nmh` 冷启动只拉 `fluxdown-agent`，不得回退旧 Flutter 可执行。
- **本机端口默认宽松、显式严格**：默认端口（agent 17800 / daemon 17801）不可用时回退系统分配并原子发布端点文件，读取方每次连接前重读；显式地址绑定失败即启动失败。新客户端不得写死端口，本机连接与握手必须有超时。
- **远程控制凭据**：远端 `/rpc` 只认 `agent.token`、访问密钥、配对且用户批准控制（`grantsControl`）的设备凭据；任何凭据失效必须断开已建立会话，新凭据种类同样接入重新校验。
- **空闲静默（NAS 硬盘休眠）**：无活动任务 / 做种 / 到期 RSS / 客户端请求时，daemon / agent 不得周期性读写 save_dir 或 data_dir；新周期任务须事件驱动、受空闲门控，或长周期且仅内容变化时写。
- **宿主自身下载只走统一出口**（更新、受管组件、插件包、规则包、tracker / server.met 订阅等）：daemon / engine 用 `fluxdown_engine::egress`，agent 经 `daemon.egress.plan` + `native/agent/src/egress.rs`，禁止裸 `reqwest::Client::builder()`；仅有独立来源哈希 / 签名的载荷可交插件改写，失败回退原地址。
- 引擎学习 / 遥测类 config 键（`cdn_node_health`、`auto_route_health`、`cdn_pending_reports`、`domain_conn_caps`）UI 不读写。
- **遥测只有两条匿名部署事件**（`app_installed` 一次 + `app_active` 每日，`analytics_enabled` 门控），绝不采集下载 / 任务信息，不新增其他遥测点。**唯一例外：BT 防吸血证据**（用户 2026-10-31 决定）——只报命中检测规则的对端证据（IP 组 IPv4 /32、IPv6 /64，信号类型，送达量 / 种子体积等计量，时间段，客户端特征），不含文件名 / 路径 / 种子名 / 其他任务；infohash 只在 FluxCloud 以服务端密钥加盐散列后存储；私有种子不报；独立开关、默认关、须 UI 显式同意，与 `analytics_enabled` 无关（口径见 `ops.md` 防吸血条，上线时同改官网隐私政策）。**云端推送不是遥测**：本设备上报默认关、须 UI 显式同意、与 `analytics_enabled` 无关；默认只发文件名 / 大小 / 状态 / 错误 / 队列名，下载地址与保存目录各自单独开关（默认关）；`TaskNotice` 只由 agent 消费。
- **插件页面脚本只经浏览器 `userScripts` API 执行**：扩展任何位置禁止 `eval` / `new Function` / 远程或拼接的 `<script>`；每个插件首次启用须用户在选项页显式确认；扩展不保存脚本源码副本；`fluxdown` 桥能力窄且固定。
- 以上各条的落点、阻碍项与背景见 `hosts-and-api.md`（端口 / 凭据 / 空闲静默）、`ops.md`（云端推送）、`engine.md`（egress / naming / 做种）、`clients.md`（userScripts / iOS flavor）、`pitfalls.md`。
- 命名歧义：`tracker_subscription.rs` / `ed2k/server_subscription.rs` 是 tracker / `server.met` 订阅，与 `rss/` 无关；官网 `api/webhooks/github` 是 GitHub 接收器，与 `engine/src/webhook.rs` 无关。

---

## 5. 镜像契约：改一处必须同步另一处

逐条事实源 ↔ 镜像端清单在路径规则里，**编辑任一侧文件时自动注入**；规划跨端改动前先 `read` 对应规则。新增镜像对时加进对应规则并覆盖两侧 `globs`。

|规则|覆盖|
|---|---|
|`rule://mirror-wire-protocol`|帧判定 `judge_frame` / 合帧、队列增量、任务表分页载入、protocol DTO / 方法 / 事件 ↔ agent、GPUI、移动端、`web/src/lib/rpc`|
|`rule://mirror-mobile-bridge`|UniFFI DTO / 通用分区 / 方法名 / 设置目录、reducer、内置分类基线 ↔ Android / iOS|
|`rule://mirror-downloads`|下载页纯模型（新建解析 / aria2c / 菜单事实 / 远程命令 / 做种 / 排序 / 任务组 / 来源构成 / 设备 / 重扫节流 / 历史与限速记忆）↔ Web / 移动端；扩展 → Android App 外部唤起载荷（Intent extra / 协议参数）|
|`rule://mirror-category-route`|分类目录路由、新建下载分类选择器、一键分类目录名推导（四端）|
|`rule://mirror-engine-logic`|RSS 过滤、Content-Disposition / 字符集、Webhook 事件与端点、线程数风险档、代理直连名单归一化（engine ↔ agent）、站点代理规则宽松解析与路由标签（engine ↔ 四端）|
|`rule://mirror-theme-shell`|主题 token 注册表、文件图标包、chrome 布局|
|`rule://mirror-settings-account`|访问密钥策略（含 LuCI）、开机自启、应用更新 / 更新说明、云端推送、套餐购买|
|`rule://mirror-plugins`|插件 manifest、视图协议、贡献点授权、市场排序、嗅探规则层、页面脚本、扩展规则（含外部仓）|
|`rule://api-openapi-sync` · `rule://i18n-baseline-zh-en` · `rule://dual-client-parity`|`native/api` ↔ OpenAPI；UI 文案 en + zh 基线；Web 设置归属以 GPUI 为准|

---

## 6. git · 分支 · 发布

- 授权门槛、分支模型、tag 规则是粘性红线，见 `.omp/RULES.md`（每次请求都在）；`guard.ts` 在执行前硬拦截。
- 主干门禁 = `.github/workflows/ci.yml`：按变更触发 fmt、workspace clippy、按 crate 分组 nextest、Web / 官网 / 扩展构建与「发布对齐」检查；只在发布才走的路径必须在这里有 check 级覆盖。路径过滤由 `ci-paths` job 对照源码自检。
- 发布 = 组件变更检测 + 统一 release；稳定版先跑全量 CI + `rehearsal` 演练，推 tag 时晋升同一提交的演练产物（不重新编译）。细节见 `.omp/knowledge/ops.md` 与 `release` skill。

---

## 7. 代码风格与强制规则

**Rust**
- Edition 2024；Clippy deny `unwrap_used` / `expect_used` / `wildcard_imports`（根 `clippy.toml` 允许测试 unwrap / expect），非测试代码用 `?` + `thiserror`。
- 禁 `todo!` / `unimplemented!` 与静默忽略错误（workspace lints deny，所有 crate `[lints] workspace = true`）；禁止 `allow` / `drop(Result)` / 空错误分支绕过（`rule://no-ignored-errors-in-rust`）。
- 默认安全：unsafe 仅限无语义等价安全替代的平台 FFI 边界，最小范围 + SAFETY + 健全接口（`rule://no-unsafe-in-rust`）；workspace deny `unsafe_op_in_unsafe_fn` 与 `clippy::undocumented_unsafe_blocks`。`crates/*` 零 unsafe，唯一例外是 `crates/app/src/app_icon.rs` 的 macOS `setApplicationIconImage(Some)`。
- 公开 API `///` + doctest；异步优先，阻塞走 `spawn_blocking`；重试指数退避（MAX=3，base=2s）；task panic 用 `AssertUnwindSafe` + `catch_unwind`。
- 日志：`use crate::logger::log_info; log_info!("[mod] ...")`（每文件显式 use），格式 `HH:MM:SS.mmm [Tag] message`。
- 解析不可信输入（文件名 / 头部 / 种子元数据 / 协议帧）：字符串用 `get(a..b)` / `split_at_checked` / 先 `is_char_boundary`，下标用 `get`，长度转换用 `try_from`。

**客户端**
- 原生移动端：共享状态与 wire 由 `native/mobile` / protocol 承担，Kotlin / Swift 不复制游标、重连或云状态机；文案走共享 i18n。分层与平台约束见 `clients.md`。
- 四端渲染路径 / 文件名 / URL / 设备名等用户数据时按真实形态自查边界（`rule://ui-data-edge-cases`）；无法目视的平台在交付说明里写检查样例。

**通用门槛**
- **禁止新增 dependency**（先说明理由等确认）；禁止手编 `Cargo.toml` 版本号，用 cargo 命令。
- 改动前 `cargo check -p <crate> --lib`；提交前 fmt + workspace clippy；测试 `cargo nextest run -p <crate> <filter>`，**禁 `--workspace`**。
- 复用已有 trait / error 类型；单文件 >600 行考虑拆分，单函数 >80 行需说明。
- **一个 crate 只有一个集成测试二进制**：`tests/<名>/main.rs` + 按领域分模块，不新建 `tests/*.rs`；feature 门控模块文件头写 `#![cfg(feature = "…")]`。
- **并行子代理**：子代理只跑自己改动 crate 的精准命令（`cargo check|clippy|nextest run -p <crate>`）；全量 clippy、无 `-p` 的构建 / 测试、gradlew / xcodebuild / `build-core.sh` 由主会话或 `build-verifier` 串行跑一次（`guard.ts` 强制），子代理在报告里写明需要跑的命令。
- **高风险路径（主会话实现，不派 `task` 任务）**：`native/engine/src/{download_manager,bt_downloader,downloader,db}.rs`、`native/{protocol,daemon}/**`、`native/agent/src/{server_mode,egress}.rs` 与鉴权 / 凭据代码、`installer/**`。
- 查文档：`cargo path <crate>` 本地源码 > docs.rs > web 搜索。新增 / 改 public API、trait、error 类型、unsafe / FFI / 性能关键路径、新增 crate、写 doc comment 前先读 `rust-router` skill。
