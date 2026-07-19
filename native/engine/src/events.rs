//! 引擎向宿主(hub/CLI/Server/...)上报的事件,以及事件接收端 trait。
//!
//! 每个 [`EngineEvent`] 变体的字段与 `hub::signals` 中对应的现有信号结构体
//! 逐字段对应(字段顺序以 `hub/src/signals/mod.rs` 当前定义为准;引擎侧
//! 可多出信号不需要的字段,如 `TaskProgress::upload_speed_bps`,由宿主
//! 自行消费)。`hub` 侧的 `RinfEventSink` 实现把每个变体 match 回具体
//! 信号类型并调用 `.send_signal_to_dart()`。

use crate::model::{
    GroupInfo, ManifestItemInfo, QueueInfo, QueuePosition, SegmentDetail, TaskInfo,
};

/// 引擎运行期间产生的、宿主需要感知的事件。
///
/// `#[non_exhaustive]`:未来新增变体不算破坏性变更,强制所有 match 都带
/// `_ => {}` 兜底分支。
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// 任务进度更新,下载过程中周期性发送。对应 `hub::signals::TaskProgress`。
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
        /// 不进 Dart 信号,由宿主写入 aria2 兼容层的实时速率表
        /// (`ApiHost::live_speeds` 的 `upload_bps`)。
        upload_speed_bps: i64,
    },

    /// BT 任务数据下载完成(piece 全部下完),但校验与 staging→save_dir
    /// 搬移尚未完成、任务未进终态。每个任务至多发送一次
    /// (`progress_reporter` 按 task_id 去重)。对应 aria2 的
    /// `onBtDownloadComplete` 通知语义,无对应 Dart 信号。
    BtDataFinished { task_id: String },

    /// 响应"请求全部任务" — 全部持久化任务快照。对应 `hub::signals::AllTasks`。
    TasksSnapshot(Vec<TaskInfo>),

    /// 分段级进度,用于下载可视化(IDM 风格)。对应 `hub::signals::SegmentProgress`。
    SegmentProgress {
        task_id: String,
        total_bytes: i64,
        /// 分段数量(1 = 单线程下载)
        segment_count: i32,
        segments: Vec<SegmentDetail>,
    },

    /// 队列任务探测到元数据。对应 `hub::signals::TaskMetaProbed`。
    TaskMetaProbed {
        task_id: String,
        /// 空 = 无法探测
        file_name: String,
        /// 0 = 未知
        total_bytes: i64,
    },

    /// 队列位置批量更新 — 每次队列变化时广播。对应 `hub::signals::QueuePositionsUpdate`。
    QueuePositionsChanged(Vec<QueuePosition>),

    /// 全部命名队列 — 启动时与任意队列变化后发送。对应 `hub::signals::AllQueues`。
    QueuesChanged(Vec<QueueInfo>),
    /// 单任务队列归属变化 —— `move_task_to_queue` 成功后发送，客户端据此
    /// 原位更新该任务的 `queue_id`，避免重发整表任务快照导致 UI 闪烁。
    /// hub → `TaskQueueChanged` 信号；server → WS `taskQueueChanged`。
    TaskQueueChanged { task_id: String, queue_id: String },

    /// Boost 模式的优先下载任务发生变化。对应 `hub::signals::PriorityTaskChanged`。
    PriorityTaskChanged {
        /// 当前优先任务 ID。空字符串 = Boost 模式未激活。
        priority_task_id: String,
        /// 为释放带宽而被自动暂停的任务数量。
        auto_paused_count: i32,
    },

    /// 动态分段拆分发生通知(IDM 风格协调器),实时发送以便 UI 播放拆分动画。
    /// 对应 `hub::signals::SegmentSplitEvent`。
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
    /// 整表快照导致活跃下载 UI 闪烁。对应 `hub::signals::FileMissingChanged`。
    FileMissingChanged(Vec<(String, bool)>),

    /// 插件因连续超时/超内存被自动熔断禁用。宿主据此提示用户
    /// （hub → `PluginAutoDisabledNotice` 信号；server → WS `pluginAutoDisabled`）。
    /// 仅 `plugins` feature 下由 `PluginManager` 发出。
    PluginAutoDisabled {
        identity: String,
        /// 同 `DisabledReason` 的 PascalCase 惯例（熔断路径固定 `CircuitBreaker`）。
        reason: String,
    },

    /// 插件钩子活动指示：带产物的任务级钩子（onDone，可能含长时 ffmpeg 转码）
    /// 开始（`running=true`）/结束（`running=false`）。**纯旁路 UI 提示，不影响
    /// 任务状态机**（通知平面 fire-and-forget 契约不变）；宿主 UI 应自设看门狗
    /// （钩子墙钟硬顶 1830s）防结束事件丢失导致指示器悬挂。
    /// hub → `PluginHookActivityEvent` 信号；server → WS `pluginHookActivity`。
    /// 仅 `plugins` feature 下由 `PluginManager` 发出。
    PluginHookActivity {
        task_id: String,
        plugin_id: String,
        running: bool,
    },

    /// 全部任务组快照——组建/删除/改名/回收(GC)后发送。对应
    /// `hub::signals::AllGroups`；server → WS `groupsChanged`。组**进度**不在此列
    /// （仍由宿主按 `group_id` 对 `TaskProgress` 做 SUM 聚合，引擎不发组级进度）。
    GroupsChanged(Vec<GroupInfo>),

    /// 前置预解析（多文件清单）结果，只读、不建任务、不写库。`items` 为空且
    /// `error` 为空 = 插件未返回清单（宿主应回退普通单任务创建）；`error` 非空 =
    /// 预解析失败（同样回退普通创建，`error` 供 UI 提示）。对应
    /// `hub::signals::ResolvePreviewResult`。仅 `plugins` feature 下由
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
/// 自行 `spawn`,不得让调用方等待 —— 因为 `hub` 侧的调用方运行在单线程
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
