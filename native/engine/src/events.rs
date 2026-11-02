//! 引擎向宿主（daemon、CLI、原生移动端等）上报的事件，以及事件接收端 trait。
//!
//! 宿主实现 [`EventSink`]，将事件映射为自己的协议通知或本地状态更新；
//! 引擎事件不依赖 UI、FFI 或具体传输协议。

use crate::model::{
    GroupInfo, ManifestItemInfo, QueueInfo, QueueOrderEntry, QueuePosition, SegmentDetail, TaskInfo,
};
use crate::rss::model::{RssItemInfo, RssSourceInfo};
use crate::transfer_activity::TaskRuntime;

/// 引擎运行期间产生的、宿主需要感知的事件。
///
/// `#[non_exhaustive]`:未来新增变体不算破坏性变更,强制所有 match 都带
/// `_ => {}` 兜底分支。
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Latest observation of active body transfers and file ranges.
    TaskRuntimeChanged(TaskRuntime),

    /// 仅在任务活动已成功写入持久日志之后广播。
    TaskActivityAdded(crate::task_activity::TaskActivity),
    /// 任务进度更新，下载过程中周期性发送。
    TaskProgress {
        task_id: String,
        /// 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
        status: i32,
        downloaded_bytes: i64,
        total_bytes: i64,
        /// 字节/秒
        speed: i64,
        file_name: String,
        save_dir: String,
        url: String,
        /// 无错误时为空
        error_message: String,
        /// 实时上传速率(字节/秒)。仅 BT 任务非零,其余协议恒 0。
        /// 不进 daemon 事件流,由宿主写入 aria2 兼容层的实时速率表
        /// (`ApiHost::live_speeds` 的 `upload_bps`)。
        upload_speed_bps: i64,
        /// 已上传字节数（BT 做种）。仅 BT 任务有意义，默认 0。
        uploaded_bytes: i64,
        /// Seeding status: 0=none, 1=active seeding, 2=ratio reached,
        /// 3=time reached, 4=user stopped, 5=task deleted, 6=session released,
        /// 7=inactive time reached, 8=queued for a seeding slot.
        seeding_status: i32,
        /// BT 做种状态的辅助说明（如停止原因）。无错误/未做种时为空。
        seeding_message: String,
        /// 累计做种秒数（发帧时刻；排队/暂停不计）。仅 BT 任务有意义。
        seeding_time_secs: i64,
    },

    /// BT 任务数据下载完成(piece 全部下完),但校验与 staging→save_dir
    /// 搬移尚未完成、任务未进终态。每个任务至多发送一次
    /// (`progress_reporter` 按 task_id 去重)。对应 aria2 的
    /// `onBtDownloadComplete` 通知语义,无对应客户端事件。
    BtDataFinished { task_id: String },

    /// 响应“请求全部任务”——全部持久化任务快照。
    TasksSnapshot(Vec<TaskInfo>),

    /// 完整持久化单任务事实；创建配置全部落库后、任何下载或探测帧之前发送。
    TaskChanged(Box<TaskInfo>),

    /// 一批新建任务的完整持久化事实（批量创建 `create_tasks_batch`）；语义 =
    /// 按序对每个任务应用一次 [`EngineEvent::TaskChanged`]。整批先于任何
    /// 单任务进度/元数据/启动帧发送，宿主据此一次处理整批而非逐任务广播。
    TasksChanged(Vec<TaskInfo>),

    /// 一批任务的记录已删除（批量删除 `delete_tasks_batch`）；语义 = 按序对每个任务
    /// 应用一次删除。经 progress reporter 排序发出：先于它入队的这些任务的进度帧都已发出，
    /// 宿主据此一次移除整批，而不是逐任务处理 `status=4` 删除哨兵。
    TasksDeleted(Vec<String>),

    /// Authoritative download counter baseline, ordered through the progress
    /// reporter before the corresponding progress frame. Restored bytes are
    /// not traffic; an explicit lower baseline starts a new transfer interval.
    /// Host-local accounting only: this is not a wire event.
    TaskTransferBaseline {
        task_id: String,
        downloaded_bytes: i64,
        /// Last raw counter before the new baseline. The reporter may have
        /// coalesced this sample; settle it before replacing the interval.
        previous_downloaded_bytes: Option<i64>,
    },

    /// 自上一帧以来从网络读到的 body 字节（下载器上报了
    /// `ProgressUpdate::transfer_bytes` 时才有），紧排在对应进度帧之前。
    /// 宿主据此统计流量，并忽略该任务进度帧的落盘增量，直到任务离开活动态。
    /// Host-local accounting only: this is not a wire event.
    TaskTransferred { task_id: String, bytes: i64 },

    /// 分段级进度，用于下载可视化（IDM 风格）。
    SegmentProgress {
        task_id: String,
        total_bytes: i64,
        /// 分段数量(1 = 单线程下载)
        segment_count: i32,
        segments: Vec<SegmentDetail>,
    },

    /// 队列任务探测到元数据。
    TaskMetaProbed {
        task_id: String,
        /// 空 = 无法探测
        file_name: String,
        /// 0 = 未知
        total_bytes: i64,
    },

    /// 队列位置全量替换——首次发布与用户调序（已发布键的相对顺序被破坏）时发送。
    QueuePositionsChanged(Vec<QueuePosition>),

    /// 队列顺序增量——先移除 `removed`，再按 `order` 插入 / 更新 `added`。
    /// 队首出队只携带被移除的任务，其余任务的键不变。
    QueueOrderChanged {
        removed: Vec<String>,
        added: Vec<QueueOrderEntry>,
    },

    /// 全部命名队列——启动时与任意队列变化后发送。
    QueuesChanged(Vec<QueueInfo>),
    /// 单任务队列归属变化 —— `move_task_to_queue` 成功后发送，客户端据此
    /// 原位更新该任务的 `queue_id`，避免重发整表任务快照导致 UI 闪烁。
    TaskQueueChanged { task_id: String, queue_id: String },

    /// `ProxyMode::Auto` 任务的链路决策落定/变更——启动基线（direct /
    /// proxy:cached）与运行中主导路径变化（proxy:sampled 等）都会发送，客户端
    /// 据此原位刷新详情面板「链路」行。wire 标签见 `crate::auto_proxy::route`。
    TaskRouteChanged { task_id: String, route: String },

    /// Boost 模式的优先下载任务发生变化。
    PriorityTaskChanged {
        /// 当前优先任务 ID。空字符串 = Boost 模式未激活。
        priority_task_id: String,
        /// 为释放带宽而被自动暂停的任务数量。
        auto_paused_count: i32,
    },

    /// 动态分段拆分发生通知(IDM 风格协调器),实时发送以便 UI 播放拆分动画。
    SegmentSplit {
        task_id: String,
        /// 被缩小的父分段索引。
        parent_index: i32,
        /// 拆分后父分段的新 end_byte。
        parent_new_end: i64,
        /// 新建子分段的索引。
        child_index: i32,
        /// 新子分段的起始字节(= 拆分点)。
        child_start: i64,
        /// 新子分段的结束字节(= 父分段原 end)。
        child_end: i64,
        /// 是否为主动拆分(true)还是抢救式/按需拆分(false)。
        is_proactive: bool,
        /// 拆分后的当前分段总数。
        total_segments: i32,
    },

    /// 文件跟踪：一批已完成任务的「文件已丢失」标志发生变化（true=丢失，
    /// false=恢复存在）。仅携带发生变化的任务 `(task_id, missing)`，避免重发
    /// 整表快照导致活跃下载 UI 闪烁。
    FileMissingChanged(Vec<(String, bool)>),

    /// 插件因连续超时/超内存被自动熔断禁用。宿主据此提示用户
    /// （兼容 WebSocket 事件为 `pluginAutoDisabled`）。
    /// 仅 `plugins` feature 下由 `PluginManager` 发出。
    PluginAutoDisabled {
        identity: String,
        /// 同 `DisabledReason` 的 PascalCase 惯例（熔断路径固定 `CircuitBreaker`）。
        reason: String,
    },

    /// 插件状态（列表 / 设置 / 站点默认）发生变化，携带最新完整列表。
    /// 客户端整体替换插件列表并重新拉取插件相关查询。
    /// 仅 `plugins` feature 下由 `PluginManager::publish_state` 发出。
    #[cfg(feature = "plugins")]
    PluginsChanged(Vec<crate::plugin::PluginInfo>),

    /// BT 重复添加检测：新任务的 info-hash 已被另一任务持有（下载或做种），
    /// 引擎已删除新建的占位任务行（不产生失败任务）。宿主据此提示用户并
    /// 可跳转已有任务（兼容 WebSocket 事件为 `duplicateTorrent`）。
    DuplicateTorrentDetected {
        /// 被删除的占位任务 id。
        task_id: String,
        /// 持有该种子的已有任务 id（极端情况下为 `"unknown"`）。
        existing_task_id: String,
        /// 已有任务的显示名（可能为空，例如其元数据尚未解析）。
        existing_name: String,
    },

    /// 插件钩子活动指示：带产物的任务级钩子（onDone，可能含长时 ffmpeg 转码）
    /// 开始（`running=true`）/结束（`running=false`）。**纯旁路 UI 提示，不影响
    /// 任务状态机**（通知平面 fire-and-forget 契约不变）；宿主 UI 应自设看门狗
    /// （钩子墙钟硬顶 1830s）防结束事件丢失导致指示器悬挂。
    /// 兼容 WebSocket 事件为 `pluginHookActivity`。
    /// 仅 `plugins` feature 下由 `PluginManager` 发出。
    PluginHookActivity {
        task_id: String,
        plugin_id: String,
        running: bool,
    },

    /// 常驻插件宿主渲染出一棵新的可移植视图树（未规范化的原始 JSON，由 daemon
    /// `normalize_view` + `diff_view` 后推给 UI）。仅 `plugins` feature 下由
    /// `PluginManager` 发出。
    PluginViewRendered {
        session: String,
        tree: serde_json::Value,
    },
    /// 可移植视图渲染 / 事件处理失败（会话仍保持打开，下一次渲染可恢复）。
    PluginViewFailed { session: String, message: String },
    /// 可移植视图会话已结束（`reason`：`closed` / `expired` / `pluginUnloaded` /
    /// `requested`——插件经 `ctx.close()` 请求关闭承载容器）。
    PluginViewClosed { session: String, reason: String },
    /// 插件经 `ctx.navigate()` 请求主界面跳转；`target` 为已校验的路由 id
    /// （内置 `downloads` / `rss` / `webhooks` / `share`，或 `plugin:<identity>:<view>`）。
    PluginViewNavigate { session: String, target: String },
    /// 插件经 `flux.ui.toast` 请求的提示（`level`：info/success/warning/error）。
    PluginToast {
        identity: String,
        level: String,
        text: String,
    },

    /// 全部任务组快照——组建/删除/改名/回收（GC）后发送。兼容 WebSocket
    /// 事件为 `groupsChanged`。组**进度**不在此列
    /// （仍由宿主按 `group_id` 对 `TaskProgress` 做 SUM 聚合，引擎不发组级进度）。
    GroupsChanged(Vec<GroupInfo>),

    /// 前置预解析（多文件清单）结果，只读、不建任务、不写库。`items` 为空且
    /// `error` 为空 = 插件未返回清单（宿主应回退普通单任务创建）；`error` 非空 =
    /// 预解析失败（同样回退普通创建，`error` 供 UI 提示）。仅 `plugins` feature 下由
    /// [`crate::download_manager::DownloadManager::begin_resolve_preview`] 发出
    /// （feature 关时该方法直接发出空清单，无需宿主 `cfg` 分叉）。
    ResolvePreviewReady {
        preview_id: String,
        name: String,
        source_url: String,
        items: Vec<ManifestItemInfo>,
        /// 无错误时为空。
        error: String,
    },

    /// 多 CDN 并发下载 / 多网卡聚合的节点级活动事件（任务详情「日志」Tab 的
    /// 可观测性）。兼容 WebSocket 事件为 `taskCdnEvent`。
    /// 仅多节点聚合或多网卡聚合被尝试/生效的任务发出；单节点池路径零事件。
    TaskCdnEvent {
        task_id: String,
        /// 事件类型：
        /// - `"pool"`：多节点池就绪（`nodes` = 就绪节点清单，含来源与健康度先验）；
        /// - `"kick"`：钉定节点或网卡链路被踢除（`ip`/`reason`/`count`）；
        /// - `"breaker"`：聚合熔断——被踢节点数过半，24h 内不再对该 host 聚合；
        /// - `"fallback"`：聚合已尝试但未成（存活候选不足/聚合任务异常），退单节点；
        /// - `"leases"`：节点并发分布快照（`nodes` = 参与中节点，`active` = 该节点
        ///   当前未归还的段租约数；租约借还时按 2s 节流 + 变化检测发射）；
        /// - `"summary"`：多段下载结束后的节点贡献统计（`nodes` = 各节点字节数/实测吞吐）；
        /// - `"links"`：多网卡聚合挂入额外链路（`nodes` = 各链路，`ip` = `NIC:<网卡名>`、
        ///   `origin` = 该网卡本地地址）；
        /// - `"links_off"`：多网卡聚合已开启但本任务不聚合（`reason` =
        ///   [`crate::multi_nic::off_reason`] 原因码）。
        ///
        /// 节点标签：`SYS`（任务主 client）、钉定 IP、`PROXY:manual|system`、
        /// `NIC:<网卡名>`（网卡链路）、`DIRECT`。
        kind: String,
        /// 钉定目标 host（多网卡事件为任务目标 host）。
        host: String,
        /// `pool`/`leases`/`summary`/`links` 的节点清单；其余事件为空。
        nodes: Vec<crate::model::CdnNodeInfo>,
        /// `kick`：被踢节点标签（钉定 IP 或 `NIC:<网卡名>`）；其余事件为空串。
        ip: String,
        /// `kick`：`"validator"`（内容不一致）/`"fail"`（连续失败）/`"build"`
        /// （pinned client 构建失败）；`fallback`：`"few"`（存活不足）/`"error"`
        /// （聚合任务异常）；`links_off`：原因码。其余事件为空串。
        reason: String,
        /// `pool`/`fallback`：去重候选 IP 总数；`kick`(fail)：连续失败次数。
        candidates: i32,
        /// `pool`/`fallback`：connect 预筛存活数。
        alive: i32,
        /// `pool`：本次生效的钉定节点数上限。
        cap: i32,
        /// `pool`：上限是否为自动档推导（文件大小阶梯）。
        auto_cap: bool,
    },

    /// 全部 RSS 订阅快照——增删改 / 抓取完成 / 条目状态变化后发送（含派生的
    /// 未读计数，侧边栏 badge 与订阅列表同批更新）。
    /// daemon（`event_hub.rs`）→ `DaemonEvent`/`WsServerMsg::RssSourcesChanged`。
    RssSourcesChanged(Vec<RssSourceInfo>),

    /// 某订阅的条目流快照——**只在该源确有变化或客户端显式请求时**发送
    /// （定时 tick 抓到 0 条新条目时静默，不打扰 UI）。
    /// daemon（`event_hub.rs`）→ `DaemonEvent`/`WsServerMsg::RssItemsChanged`。
    RssItemsChanged {
        source_id: String,
        /// 该源的条目流（新→旧，上限 `rss::MAX_ITEMS_PER_SOURCE`）。
        items: Vec<RssItemInfo>,
        /// 本轮自动创建任务的条目标题；订阅关闭通知或本轮无自动下载时为空。
        /// 宿主据此弹一条**合批**通知（AutoBangumi #64），引擎不做通知。
        notify_titles: Vec<String>,
    },

    /// 新建订阅向导的 feed 验证结果（只读，不落库不建任务）。
    /// daemon（`event_hub.rs`）→ `DaemonEvent`/`WsServerMsg::RssFeedValidated`。
    RssFeedValidated {
        /// 与请求一一对应，用于把结果配回发起的对话框。
        request_id: String,
        url: String,
        /// feed 标题（供回填订阅名）。
        feed_title: String,
        /// 最近条目预览（`source_id` 为空的瞬态条目）。
        items: Vec<RssItemInfo>,
        /// 无错误时为空。
        error: String,
    },

    /// 投递日志新增记录——**推给宿主，让打开着的日志面板活着更新**。
    ///
    /// 拉快照（`Engine::webhook_deliveries`）只在打开面板那一刻发生；任务真
    /// 完成时的投递、以及「模拟一次下载完成」按钮，都发生在那之后。没有这
    /// 条推送，面板就只能停在打开时的样子（按钮看着像没反应）。
    ///
    /// 载荷是**最新一小段（≤100 条，新→旧）而不是整仓**：日志落盘后能攒到
    /// `webhook::MAX_DELIVERY_LOG` 条，整仓一份实测 0.94MB，每 500ms 推一次
    /// 撑不住。宿主按 `deliveryId` 合并进自己那份列表，完整历史走「打开面板
    /// 时拉一次」。dispatcher 侧已按 500ms 节流 + 尾随补发。
    /// daemon（`event_hub.rs`）→ `DaemonEvent`/`WsServerMsg::WebhookDeliveriesChanged`。
    WebhookDeliveriesChanged(Vec<crate::webhook::WebhookDelivery>),

    /// 「等待自动重试」的任务数变化——自动重试 / 备用链路切换 / 插件 `requestRetry`
    /// 已排程、尚未回流重新派发的任务集合大小。这些任务在数据库里仍是 `status=4`，
    /// 宿主的活跃/排队计数看不到它们；据此判断「全部完成」类的联动（完成后关机、
    /// 空闲退出）时必须把它们算作仍有工作。数值不变时不发。
    RetryPendingChanged { count: u32 },

    /// 任务生命周期语义事件（与 webhook 同源同触发点）——**只给云端推送用**。
    ///
    /// 每个触发点在 `webhook.emit` 之外再发一条，携带新生成的 `deliveryId`
    /// 与毫秒时间戳；daemon 映射为 `DaemonEvent::TaskNotice`，只由 agent 消费，
    /// 不转发给任何 UI。不关心的宿主直接忽略。
    TaskNotice(crate::webhook::TaskNotice),

    /// BT 防吸血的条目 / 状态发生变化（条目增删、云名单 / 订阅应用、新的观察或处置记录）。
    /// daemon 映射为 `DaemonEvent::BtGuardChanged`，UI 据此重新拉取 `daemon.bt.guard.*`。
    /// 引擎侧已合并防抖，不会逐个对端事件各发一条。
    BtGuardChanged,

    /// BT 会话启动时与存活期间每 6 小时发出：请 agent 刷新云名单。`have_sequence` 为当前
    /// 已应用文档的序号（0 = 无）。云名单关闭（`bt_guard_cloud_enabled=false` /
    /// `bt_guard_cloud_mode=off`）或防吸血关闭时不发。daemon 映射为
    /// `DaemonEvent::BtGuardCloudRefreshWanted`，只供 agent 消费。
    BtGuardCloudRefreshWanted { have_sequence: u64 },

    /// 待上报证据队列有新增（仅 `bt_guard_report_enabled` 时累积）。`count` 为队列当前条数。
    /// daemon 映射为 `DaemonEvent::BtGuardEvidenceReady`，只供 agent 消费。
    BtGuardEvidenceReady { count: u32 },
}

/// 引擎事件的接收端,由宿主实现并注入 [`crate::Engine`]。
///
/// # 契约(实现者必须遵守)
///
/// `emit` 是**同步**方法,fire-and-forget 语义 —— 依据是现有全部
/// `.send_signal_to_dart()` 调用点均为无 `.await` 的同步调用惯例,做成
/// async trait 会强迫所有调用点新增 `.await` 却无对应行为收益。
///
/// 实现**不得**执行阻塞操作或长时间持锁;任何异步/耗时工作必须由实现
/// 自行 `spawn`,不得让调用方等待 —— 因为宿主 actor 的调用方运行在单线程
/// `current_thread` runtime 上,`emit` 内部阻塞会 stall 整个 runtime 上的
/// 所有任务。
pub trait EventSink: Send + Sync {
    /// 上报一个引擎事件。必须立即返回,不得阻塞或长时间持锁(见 trait 文档)。
    ///
    /// # Examples
    ///
    /// ```
    /// use fluxdown_engine::events::{EngineEvent, EventSink};
    ///
    /// struct PrintSink;
    /// impl EventSink for PrintSink {
    ///     fn emit(&self, event: EngineEvent) {
    ///         println!("{event:?}");
    ///     }
    /// }
    ///
    /// let sink = PrintSink;
    /// sink.emit(EngineEvent::TaskMetaProbed {
    ///     task_id: "abc".to_string(),
    ///     file_name: "video.mp4".to_string(),
    ///     total_bytes: 1024,
    /// });
    /// ```
    fn emit(&self, event: EngineEvent);
}
