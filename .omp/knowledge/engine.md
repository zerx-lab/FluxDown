# FluxDown internals · 数据模型 · 下载引擎 · 插件系统

> 本文件是 `FluxDown/AGENTS.md` 的深挖附录：只放**枚举性 / 可从源码复原**的细节，硬不变式与红线在 AGENTS.md。
> 路径以 `FluxDown/` 为根（cwd=工作区根时前置 `FluxDown/`）。事实层以源码为准，文档给坐标。

---

## 状态与数据模型

- **任务状态码**: 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing（+ Dart 端 resuming）。
- **文件类型分类**: all/video/audio/document/image/program/archive/other（扩展名表见 `models/download_task.dart`）；用户可自定义分类（`models/custom_category.dart`，27 图标 + 匹配模式，驱动按类别落盘）。
- **时间分组**: today/yesterday/thisWeek/thisMonth/older。
- **任务组**: 多文件下载的纯逻辑聚合壳（N 独立子任务 + `task_groups` 行）；组进度由前端 SUM 聚合；空组自动回收（`gc_empty_groups`）。

### 数据库（`native/engine/src/db.rs`，sqlx `Any` 池）

**双后端**：URL scheme 选后端（`sqlite:`/`postgres:`）；两份 DDL 常量（`SQLITE_SCHEMA`/`POSTGRES_SCHEMA`）处理二进制、字节计数与自增主键的后端差异；运行时 SQL 统一 `$N` 占位符；`add_column_if_missing` 幂等迁移（新库建表即全列，旧桌面库经 ALTER 升级）。SQLite 侧 WAL + 外键 + busy_timeout=5000。

**当前表（列以 db.rs 为准，此处仅索引）**：
- `tasks`(id PK, url, file_name, save_dir, status, total/downloaded_bytes, segments, created_at, error_message, proxy_url, queue_id, checksum, ignore_tls_errors, bt_selected_files, bt_custom_name, orig_etag, orig_last_modified, audio_url, file_missing, `range_verified`（配额端点续传验证）, queue_order；迁移列：cookies, referrer, extra_headers, resolver_plugin_id, segments_epoch, completed_at, group_id, resolver_item, rss_source_id（RSS 溯源，空=非 RSS 来源）, `origin_url`（展示用真实来源；`.torrent` 任务的 `url` 是 `torrent-file://local` 哨兵，「复制链接」类 UI **一律**读它并空则回退 `url`——Dart `DownloadTask.shareUrl` / web `taskShareUrl()`）, `auto_route`（`ProxyMode::Auto` 的任务级最终链路，wire 标签见 `auto_proxy::route`；空=非 Auto）, `unattended`（无人值守创建标记，`NewTaskSpec::unattended_selection` 置位：RSS / 外部接管命中「免打扰跳过二次选择」config `silent_skip_selection`；start/resume 读它让 HLS/DASH 画质与插件变体静默取默认；BT 不读此列——建任务时已按「全选」写 bt_selected_files））
- `task_segments`(复合 PK task_id+segment_index；旧库遗留 id AUTOINCREMENT 不再读)
- `task_groups`(id PK, name, source_url, save_dir, created_at)
- `config`(key PK, value)——**所有设置键**都存这里
- `torrent_files`(task_id PK, file_bytes BLOB)
- `queues`(id PK, name, speed_limit_kbps, upload_limit_kbps, max_concurrent, default_save_dir, position, default_segments, default_user_agent, is_running, schedule_enabled/start/stop, schedule_days 位掩码)
- `ed2k_blocks`(复合 PK task_id+block_index, state, downloaded_bytes, retry_count)
- `ed2k_hashset`(task_id PK, hashes BLOB)
- `task_artifacts`(复合 PK task_id+file_name；追踪 sidecar/产物文件供清理)
- `rss_sources`(id PK, `provider_id`/`provider_config`（订阅 provider 适配器与其不透明配置，旧数据默认 `rss`/空）, url, name, enabled, auto_download, start_paused, queue_id, save_dir, interval_minutes, include/exclude_pattern, use_regex, smart_episode, size_min/max_bytes, send_referer, notify_on_download, max_per_fetch, cookies, user_agent, proxy_url, last_fetch_at, last_success_at, last_error, fail_count, `seeded`（首轮是否已完成）, position)
- `rss_items`(复合 PK source_id+guid, title, link, enclosure_url/length, `resolver_item`（插件二段解析标识，空 = 普通直链）, pub_date, fetched_at, status 0..5, task_id 回链, episode_key, reason 原因码；`ON DELETE CASCADE` 于 rss_sources)

**内置队列**: `main`（主）/`later`（稍后下载），播种于 `Engine::new`，不可删/改名；存量 `queue_id=''` 迁入 `main`。删除命名队列后，任务归属、排队位置与持久排序都要同步：`TaskQueueChanged` 只带归属，`QueuePositionsChanged` 只带待排位置，`TasksSnapshot` 才是 `queue_order` 归零后的权威来源。

**任务活动与实时采样**：`task_activity.rs` 拥有源端活动代理，`db.rs` 的活动表/保留标记在 SQLite 与 PostgreSQL 同步维护。状态边沿、完整错误、实际重试、分段拆分及 CDN 语义事件先持久化再广播；高频进度不逐帧入库。ID 在同一数据库内跨重启单调，任务删除级联清理；保留约束及分页上限以 `db.rs` 常量为准，清理过的历史有逐任务截断标记。所有宿主的 `progress_reporter` 必须使用 `Engine::activity_sink`，否则绕过统一记录；关闭时冲刷有超时并报告真实失败。`transfer_activity.rs` 的 RAII 计数只覆盖真实传输生命周期，采样携带源端单调序号；分段几何、活跃传输、配置上限与 BT peers 是不同事实，不能相互推算。

---

## 下载引擎（`native/engine`）

### 6 种协议（分发 = `download_manager::do_start_task`/`do_resume_task` 内单条 if/else 链，每臂 `catch_unwind`）

| 协议 | 判定谓词 | 入口 | 文件 |
|---|---|---|---|
| **HTTP/HTTPS**（默认兜底） | fallthrough | `segment_coordinator`（IDM worker pool） | `downloader.rs` / `segment_coordinator.rs` / `segment_advisor.rs` |
| **FTP** | `is_ftp_url` | `ftp_downloader::run_ftp_download` | `ftp_downloader.rs`（suppaftp 同步 + spawn_blocking） |
| **BitTorrent** | `is_bt_url`（magnet 或 .torrent 哨兵） | librqbit `SharedBtSession` | `bt_downloader.rs` / `tracker_subscription.rs` |
| **HLS** | `hls_downloader::is_hls_url` | `run_hls_download` | `hls_downloader.rs`（M3U8/多码率/AES-128） |
| **DASH / 音视频轨合并** | `is_dash_url` 或有 `audio_url` | `run_dash_download` | `dash_downloader.rs` |
| **ED2K（仅下载）** | `ed2k::link::is_ed2k_url` | `ed2k::run_ed2k_download` | `ed2k/`（mod,link,proto,hash,server,peer,client,server_subscription,upnp,kad/） |

- BT 任务绕过 pending 队列，且**不计入** http/ftp 并发计数（`max_concurrent`）。
- **BT 判定只认 `magnet:` 与 `torrent-file://` 哨兵**（`is_bt_url`）。HTTP 的
  `.torrent` **直链不会走 BT**——会被当普通文件下回来一个种子文件。要让直链
  变成真下载，必须先把字节抓下来再以 `NewTaskSpec::torrent_file_bytes` 建任务
  （RSS 订阅就是这么做的，见 `rss/` 的两段式）。
- **DHT 持久化是纯缓存，`SharedBtSession::new` 对它三级兜底**：带 `dht.json`
  起 → 失败则删掉它重试 → 再失败则关 DHT 起（tracker + PEX 仍可用）。起因是
  一份钉着 `addr: 0.0.0.0:58686` 的 `dht.json` 撞上 Windows 动态端口排除区间
  （`netsh interface ipv4 show excludedportrange protocol=udp`；`netstat` 看不到
  占用但 bind 返回 `WSAEACCES` 10013），导致**所有** BT 任务永久 status=4 而
  用户无从自救。**anyhow 错误一律用 `{e:#}` 打印**——`{e}` 只输出最外层
  context，会把这类根因整个吞掉。
- **BT 会话保活与完成校验**：`maybe_release_bt_session` 在存在「已暂停的未完成
  torrent（句柄在册）」时**不拆**共享会话——resume 走 `unpause`（Paused→Live
  零校验秒恢复）；全部 BT 任务终态化（完成/删除且无做种）才释放。暂停撞上
  librqbit 初检（只能从 Live 暂停）时经世代号「延迟暂停」兜底，防幽灵下载。
  Windows 上 staging 文件经 `bt_sparse` 打 FSCTL_SET_SPARSE（免整体簇预留 +
  免 VDL 零填充写放大）。完成期全量重哈希只在 fastresume 污点时执行（add 时
  存在既有 `.bitv` / 经缓存句柄跨暂停恢复 / 完成重试）；无污点任务的 have-bits
  全部有磁盘依据（全量初检读盘 / Live 写盘后读回校验），完成即时。
- **ED2K**：eDonkey2000 纯 leech。源发现 = 服务器 `GETSOURCES`（手动 `ed2k_server_list` + 订阅 `server.met` 缓存）+ Kad DHT 兜底 + UPnP-IGD 争 HighID + LowID 回调中继。逐块 MD4 + hashset 自校验（违规拉黑 peer）；分块 MD4 root hash（PART_SIZE=9.28MB，幻影尾处理）。进程级共享 `Ed2kClient` 持久服务器会话。

### 引擎子系统（一句话职责）
- `download_manager.rs`（~7300 行）：任务生命周期、并发、队列（内置 + 命名，启停/每日定时边沿触发/顺序）、任务组、自动重试、协议分发、off-actor 插件解析插桩、速度平滑（EMA α=0.4，1s 采样窗）、WAL checkpoint。
- `downloader.rs`：共享原语（`DownloadError` 含 Ed2k/Ed2kIntegrity/Cancelled、`RequestSpec`、文件名/编码工具）。
- `segment_advisor.rs`：按文件大小 + CPU 推荐连接上限（HTTP 是上限，coordinator 逐步爬升）。
- `segment_coordinator.rs`（~5300 行）：IDM 式动态分段（按需分配、对半拆最大在传分段救慢速、连接复用、per-domain 连接策略学习——负面上限 + 正面起步提示双观察面、`fallocate` 预分配）。
- `speed_limiter.rs`：全局 token bucket（Arc 可克隆，limit==0=不限）。
- `meta_prober.rs`：队列任务后台探测文件名/大小（8s；HTTP HEAD / FTP SIZE / magnet dn= / torrent 跳过）。
- `proxy_config.rs`：无/系统（Windows 注册表）/手动/**自动**（`ProxyMode::Auto`）；HTTP/HTTPS/SOCKS4/5；`test_proxy_connection` 测延迟。**运行期跟随**：`DownloadManager::tick_system_proxy`（宿主 20s 与 `tick_queue_schedules` 同 tick 搭车）+ `refresh_system_proxy_if_changed`（挂 `create_task` 新任务与 `resume_task` 暂停→开始入口）仅在 Auto 模式比对系统代理快照，变化即清 `DecisionCache` + failover 状态，**新任务/手动恢复的任务**随当前系统代理重评（运行中任务不热切；route_health 先验靠指纹 epoch 自愈；System 模式代理地址烤在共享 client 里，仅设置变更时重建）。
- `auto_proxy.rs`：`ProxyMode::Auto` 决策机器——任务直连无阻塞启动（保留 CDN 聚合资格），越过 6s 爬升期且剩余 ≥4MiB 时，对全部可用候选（手动字段 + 系统代理；相同端点去重）并行各采 256KiB；按相同的单连接量纲比较（不让多分段总速掩盖慢连接），取最快且吞吐 ≥2× 的代理，经 `NodePool::switch_to_client` 在分段边界热切换。host 级决策缓存两层：内存租约 `DecisionCache` 记录胜出来源（重启清零）+ `route_health.rs` 持久化先验（config `auto_route_health`，网络指纹 epoch——换网整表丢弃、**离线=unknown 不清表**）；持久层未记录代理来源，故手动与系统候选并存时只缩短复评等待、不盲采纳旧 Proxy 先验。Cooldown 指数退避 / NoSwitch 完整性门禁；局部续传永不直接采纳 host 代理租约，必须重新采样 validator。failover **独立于通用重试配额**：手动代理、系统代理、本地直连在一个自动恢复周期内各尝试至多一次，当前链路传输失败就切到尚未尝试的候选；三路均失败后只服从通用重试，杜绝 ping-pong。任务级最终链路落 `tasks.auto_route` + `EngineEvent::TaskRouteChanged`（含 `direct:failover` / `proxy:failover:{manual,system}`），双端详情面板可追溯。代理设置变更同点清 `DecisionCache` + `route_health` + `domain_conn_caps` + failover 状态。
- `disk_space.rs`：跨平台余量查询（HLS remux/DASH mux ENOSPC 预检）。
- `proc.rs`：`no_console_window` —— **每个 console 子进程 spawn 都必须包裹**（ffmpeg/ffprobe/yt-dlp/tar/探版），防 Windows 闪窗。
- `data_dir.rs`：数据目录解析（Windows 便携 `<exe>/portable_data` via `portable` 标记 vs 安装 `%LOCALAPPDATA%`；Linux XDG；macOS App Support；Android files dir）+ 旧版迁移。**Dart 侧 `services/platform_utils.dart` 的 KNOWN_ITEMS 必须与此同步。**
- `logger.rs`：全局文件日志宏 `log_info!`/`log_error!`（`#[macro_export]`，`$crate` 前缀跨 crate 安全；每文件顶显式 `use`）。与 Dart `LogService` 写同一文件。
- `model.rs`/`events.rs`/`selection.rs`：领域类型 / `EngineEvent`（`#[non_exhaustive]`）+ `EventSink` / `HostSelection`。
- `rss/`（`model`/`parser`/`filter`/`mod`）：订阅自动下载。`RssManager` 挂在 `DownloadManager.rss` 上；宿主只提供 60s 节拍（`tick_rss_sources()`）与回流 drain（`on_rss_event()`），抓取 off-actor，建任务仍收敛到 `create_task`。`subscription::SubscriptionProvider` 是 RSS 与插件订阅共用的 provider 分支：内置 `rss` provider 保持原流程；插件在 manifest 的 `subscriptions` 声明 `providerId` + `entry`，由 `PluginManager` 动态路由到 `globalThis.subscribe(ctx)`，返回规范化 `ParsedFeed`，并可通过 `flux.fetch` 请求平台接口。provider 不负责调度、退避、去重、过滤、落库或建任务。三层去重：guid → 单轮上限（超额留 `New` 下轮从旧到新续派）→ 智能剧集去重（识别失败即放行）。失败指数退避封顶 6h，**不自动停用订阅**。`filter.rs` 是纯函数单测主战场，**Dart/TS 各有一份逐条对齐的镜像**（`lib/src/models/rss_filter.dart`、`web/src/lib/rss-filter.ts`）供规则预览用——改任一侧必须同步三处。

  **「无人值守」是 RSS 的核心不变式**——订阅可能半夜抓到 5 集,任何需要用户点一下才能继续的东西都是 bug:① BT 条目建任务时 `NewTaskSpec.unattended_selection=true`,**在启动前**把「已确认全部文件」落库(`save_bt_selected_files(id, &[], true)`)并落 `tasks.unattended=1`(HLS/变体选择也静默),否则 `do_start_task` 会走 `HostSelection` 弹 5 次文件选择框,而用户点「取消」后条目已被标记「已下载」,状态就撒谎了;② `create_task` 内部自发建任务不经过 Dart 的建任务路径,**必须显式补发** `load_and_send_all_tasks()`——`TaskProgress` 信号不带 `queue_id`,不补发的话新任务在 UI 里不属于任何队列;③ 手动「重新下载」对**任何**状态(含已下载)都放行,挡住重下没有任何好处,只会逼用户去别处找种子。

  **删除任务不触发自动重下**：单删/批删在同一事务清空 RSS 条目的任务回链，将原 `Downloaded` 改为 `Ignored`（已读），其余处置状态保留；不能退回 `New`，否则下一轮自动抓取会重新派发。提交后仅向受影响订阅广播条目快照及源计数，手动下载仍对任何状态开放。

### 受管组件子系统（`components/`，`components` feature）
外部二进制 **ffmpeg + yt-dlp** 的按需安装器/解析器（**不打包**，合规边界——用户在设置「组件」页触发下载）。解析优先级 `manual`（config path）→ `managed`（`<data_dir>/bin/`）→ `system` PATH，wire 为 `ComponentSource{Manual,Managed,System,None}`。ffmpeg = BtbN 静态归档（取单文件，macOS 不支持受管）；yt-dlp = 单平台二进制（全平台）。版本列表经官方镜像 `fluxdown.zerx.dev/api/components` + GitHub 兜底。**被两处消费**：插件 `flux.ffmpeg`/`flux.ytdlp` 能力面 + 设置「组件」UI。

---

## 插件系统（`native/engine/src/plugin`，`plugins` feature）

**可选、可失败的下载任务中间层**，JS 编写（rquickjs 沙箱），声明式设置项（双端自动生成表单）。两个正交能力平面 + 门控工具面：

- **订阅平面**：manifest `subscriptions:[{providerId,entry,timeoutMs}]` 声明 provider，脚本导出 `globalThis.subscribe(ctx)`，通过 `flux.fetch` 自主完成平台请求/解析，返回 `{title,link,items:[{guid,title,link,enclosureUrl,resolverItem,enclosureLength,pubDate}]}`（`providerId` 不得占用内置 `rss`；单条非法跳过并记日志，全部非法才算失败；`ctx.providerConfig` 空值归一为 `{}`）；宿主自动把它挂到公共订阅调度，插件安装/启停后由动态路由读取最新快照。
- **Resolver 平面**：`resolve(url,ctx)→{url}|{manifest}|null`。协议判定**之前**惰性执行、**off-actor**（防冻结 actor），命中后 fail-closed（失败进 status=4，绝不把 HTML 当视频存）。惰性 = 每次 start/resume 重跑，天然防直链过期。支持两段式：初段返 manifest 清单 → 引擎裂变为任务组；二段（`ctx.resolverItem`）返直链。`multi:true` 触发新建对话框前置预解析（`begin_resolve_preview` 只读）。
- **通知平面**：onStart/onDone/onError/onMetaProbed，全 fire-and-forget（失败仅记日志/超时/`try_acquire`，绝不影响任务状态）；仅 onError 内可 `flux.task.requestRetry`。
- **门控工具面**（manifest `permissions` 声明才注入）：
  - `flux.auth`（`permissions:["auth"]`）：宿主持久化插件认证档案；`save/get/remove` 管理
    Cookie、Bearer、Basic 或自定义 Header。站点键**含 scheme**（`{scheme}://host[:port]`，
    `auth::site_key`）——同 host 的 http/https 是两个不同站点，https 登录态绝不隐式复用到
    http 请求（H-3，防明文 MITM 窃取）；裸 host 输入默认按 https 规范化，插件要登记明确
    允许明文的站点必须自己传 `"http://host"`。`flux.fetch` 按显式 `authRef`（空串视同未传，
    M-5）或插件+站点默认引用自动注入（仅对声明 `auth` 权限的插件推导默认引用），认证档案
    绑定插件和目标站点，过期时隐式引用跳过注入、显式 `authRef` 返回 `authentication_required`。
    档案按 `plugin.<identity>.auth.<site>` 独立配置键保存；旧版整表键 `plugin_auth_profiles`
    仅作迁移兼容，解析失败时记日志丢弃而非 fail-closed 卡死 save/remove/purge（M-1）。
    `flux.fetch` 返回的同名多值响应头以换行符（`\n`）拼接。auth 平面独立信号量
    （容量 2）+独立预算（`auth.timeoutMs` 可选，默认 30s，30s 硬顶），不与 resolve 共享、
    不计入 resolve 熔断（M-2）。凭据清理只挂用户主动 `uninstall`，安装失败的回滚
    `purge`（含其内部 `plugin.<id>.` 前缀清理）不删凭据（M-4）。
  - 登录入口（manifest `auth.entry`）：宿主 RPC `daemon.plugin.auth` 以
    `begin`/`poll`/`cancel`/`logout`/`status` 调用 `globalThis.authenticate(ctx)`；插件返回二维码挑战，
    成功后用 `flux.auth.save` 提交凭据。`logout` 在插件被禁用时也放行——不跑插件 JS，
    直接走宿主兜底删除凭据（M-3）；`poll` 回包省略 `challenge`/`challengeType` 时客户端
    保留上一帧。
  - `flux.ffmpeg`/`flux.ffprobe`（`permissions:["ffmpeg"]`）：近乎全量 argv，**封网 + 封越牢路径**（拒 URL scheme/绝对路径/`..`），牢笼 = 产物目录（仅 onDone 类有产物钩子可用），sema=2，300s/1800s 超时。
  - `flux.ytdlp`（`permissions:["ytdlp"]`）：**放行 URL/网络**（本职抓站），封危险开关（`--exec`/`--config-location`/`--plugin-dirs`/`--ffmpeg-location`/`--batch-file`…），bridge 自持 per-plugin scratch 牢笼，宿主注入 `--ffmpeg-location`（受管 ffmpeg 不在 PATH）+ `--cache-dir` 收进牢笼。resolve + 全 hook 可用。
  - `flux.fs`：per-plugin 通用临时文件读写（扁平安全名 + 单文件 8MB/总量 64MB/文件数 100 上限 + unix 0600），取代"每种输入给工具加类型化字段"的反模式。

**模块**：`auth`（受控认证档案存储、站点绑定、过期判断和请求注入）、`manifest`（校验器 + subscription/provider 声明 + `permissions`⊆{auth,ffmpeg,ytdlp} + `auth.entry`）、`semver`、`runtime`（**无 rquickjs 类型**——可换 deno_core；含 Spec/Outcome 跨界结构 + `HostContext`）、`quickjs`（v1 唯一 impl，rquickjs 限在此文件；memory_limit + interrupt + timeout 三重兜底 + 连续 3 次熔断）、`bridge`（网络出口 SSRF 守卫 + flux.* 面）、`manager`（`RwLock<Arc<Vec>>` 整表原子替换，含 `authenticate` 登录入口 + 动态订阅路由）、`dependencies`（权限→组件依赖：ffmpeg→[ffmpeg]，ytdlp→[ytdlp,ffmpeg]，**提醒式非阻断**）、`install`（.fxplug zip：zip-slip + 压缩炸弹防护 + 单层剥壳）、`market`（去中心化市场：Git 版本化联邦索引 `zerx-lab/fluxdown-plugin-index`、内容寻址 `contentHash=sha256(zip)`、多源 failover、per-index sequence 防回滚；v1 无作者签名，schema 预留）。

**off-actor 惰性 resolve 接线**：`create_task` 命中 `match_resolver` → 落 `tasks.resolver_plugin_id`（仅存 ID）+ 跳过 meta_prober。`do_start/resume_task` 体首守卫：resolver 非空且未解析 → 占位 active_tasks + off-actor spawn → return。worker 经 `resolve_rx` 回流，actor `select!` 分支 `on_resolve_ready`（复查生命周期 → 用解析后 url 重算五路协议分派）。**宿主 actor 必须接线 `resolve_rx` + `plugin_retry_rx`**。

**config 命名空间**：`plugin.<identity>.enabled`/`.disabled_reason`/`.setting.<key>`/`.kv.<key>`；`plugin.dev.<identity>`（devMode 路径）；`market.<index_id>.sequence`。identity 格式 `^[a-z0-9_-]+@[a-z0-9_-]+$`。
