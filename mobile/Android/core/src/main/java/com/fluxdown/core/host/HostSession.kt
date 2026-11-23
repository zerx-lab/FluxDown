package com.fluxdown.core.host

import com.fluxdown.core.model.Category
import com.fluxdown.core.model.CloudDevice
import com.fluxdown.core.model.HostInfo
import com.fluxdown.core.model.LinkDevice
import com.fluxdown.core.model.Queue
import com.fluxdown.core.model.QueueOrderEntry
import com.fluxdown.core.model.RssSource
import com.fluxdown.core.model.RuntimeStats
import com.fluxdown.core.model.SelectionOutcome
import com.fluxdown.core.model.SelectionRequest
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskGroup
import com.fluxdown.core.model.TaskRuntime
import kotlinx.coroutines.flow.Flow

/**
 * 一台主机的会话端口：方法与 `fluxdown_protocol` 的 `daemon.* / agent.*` 一一对应。
 *
 * 实现方：
 * - UniFFI `native/mobile` 绑定（`:bridge` 的 `RustHostSession`）：本机进程内 daemon + agent / 远端 `--server` 的 `/rpc`。
 *   Rust 侧负责握手、epoch/sequence 游标、缓冲、断档重同步、重连退避与 800ms 离线宽限，
 *   只向这里推送“已被接受”的信号，Kotlin 不重复实现游标（避免镜像从 3 份变 4 份）。
 *
 * 所有命令失败抛 [HostException]；慢方法（目录浏览、预解析等）由调用方显示加载态。
 */
interface HostSession {
    /** 信号流：首个信号必为 [HostSignal.Snapshot]；之后 Event / Stale / Snapshot（重同步）交替。 */
    val signals: Flow<HostSignal>

    fun close()

    // daemon.task.*
    suspend fun createTask(request: CreateTaskRequest): String
    suspend fun pause(taskId: String)
    suspend fun resume(taskId: String)
    suspend fun delete(taskId: String, deleteFiles: Boolean)
    suspend fun pauseMany(taskIds: List<String>)
    suspend fun resumeMany(taskIds: List<String>)
    suspend fun deleteMany(taskIds: List<String>, deleteFiles: Boolean)
    suspend fun pauseAll()
    suspend fun resumeAll()
    suspend fun rename(taskId: String, fileName: String)
    suspend fun changeUrl(taskId: String, url: String)
    suspend fun rescan()

    /**
     * 批量导入：分块 `daemon.task.createMany`（本机进程内 / 远端 `/rpc` 同一路径），立即返回；
     * 进度与汇总经 [ImportJob.updates]。会话关闭时在途导入随之失败并放弃剩余分块。
     */
    fun startImport(batches: List<ImportBatch>): ImportJob

    // daemon.queue.*
    suspend fun moveToQueue(taskId: String, queueId: String)
    suspend fun boost(taskId: String)

    // daemon.selection.*
    suspend fun resolveSelection(requestId: String, outcome: SelectionOutcome)

    // daemon.config.*：乐观并发，revision 不符抛 Conflict（携带最新 revision）
    suspend fun patchConfig(expectedRevision: Long, values: Map<String, String>)

    // daemon.rss.*
    suspend fun refreshRssSource(sourceId: String)
    suspend fun setRssSourceEnabled(sourceId: String, enabled: Boolean)

    // 通用通道：任意 `daemon.*` / `agent.*` 方法（其它前缀抛 InvalidArgument）。
    /** [paramsJson] 与返回值都是协议 serde wire 的 JSON 文本（camelCase）；无结果时返回 `"null"`。 */
    suspend fun call(method: String, paramsJson: String? = null): String
}

/** `CreateTaskRequest` 的移动端子集（N1 / N2）；空串 = 由引擎推断 / 跟随全局。 */
data class CreateTaskRequest(
    val url: String,
    val fileName: String = "",
    val saveDir: String = "",
    /** 0 = 自动。 */
    val segments: Int = 0,
    val queueId: String = "",
    val startPaused: Boolean = false,
    val cookies: String = "",
    val referrer: String = "",
    val userAgent: String = "",
    val proxyUrl: String = "",
    val checksum: String = "",
    val ignoreTlsErrors: Boolean = false,
    val headers: Map<String, String> = emptyMap(),
    val httpUser: String = "",
    val httpPassword: String = "",
    val saveSiteAuth: Boolean = false,
    /** null = 不按分类路由；非 null = 新建表单 / 外部唤起给出的默认目录，Rust 按分类规则决定是否路由。 */
    val offeredSaveDir: String? = null,
    /** 免打扰外部捕获：跳过 BT 文件 / 画质 / 变体二次选择（同 `daemon.task.create.unattended`）。 */
    val unattended: Boolean = false,
)

/** 主机推送给 UI 仓库的信号（对应 UniFFI `HostSignal`）。 */
sealed interface HostSignal {
    /** 全量快照（连接 / 重同步）。 */
    data class Snapshot(val snapshot: HostSnapshot) : HostSignal

    /** 已按游标规则接受的增量事件。 */
    data class Event(val event: HostEvent) : HostSignal

    /** 离线宽限（800ms）已过：数据只读、清空运行时。 */
    data object Stale : HostSignal

    /** 不可恢复错误（协议不兼容、鉴权失败等）。 */
    data class Fatal(val error: HostException) : HostSignal
}

/** `AgentSnapshot` 中移动端渲染的子集。 */
data class HostSnapshot(
    val info: HostInfo,
    val daemonConnected: Boolean,
    val tasks: List<Task>,
    val runtime: Map<String, TaskRuntime>,
    val queues: List<Queue>,
    /** 排队任务的排序键，按键升序（名次 = 下标 + 1 即「排队 #n」）。 */
    val queueOrder: List<QueueOrderEntry>,
    val groups: List<TaskGroup>,
    val stats: RuntimeStats,
    /** 0 或 1 个优先任务。 */
    val priorityTaskId: String?,
    val pendingSelections: List<SelectionRequest>,
    /** `DaemonConfigSnapshot`：全部主机配置键（字符串值）。 */
    val config: Map<String, String>,
    val configRevision: Long,
    val rssSources: List<RssSource>,
    val cloudDevices: List<CloudDevice>,
    val linkDevices: List<LinkDevice>,
    /** 偏好 `custom_categories` 解析结果（空 / 损坏已由 Rust 回退内置基线）。 */
    val categories: List<Category> = Category.BUILTIN,
    /** 其余 `AgentSnapshot` / `DaemonSnapshot` 分区：键见 `sections.rs`，值为协议 serde wire 的 JSON 字符串。 */
    val sections: Map<String, String> = emptyMap(),
    /**
     * 任务表尚在分页载入（协议 v17 `taskTotal` 存在）：[tasks] 此时为空，不是“没有任务”。
     * 载入完成后主机会再推送一份 `tasksLoading == false` 的完整快照。
     */
    val tasksLoading: Boolean = false,
    /** 主机上的任务总数；仅 [tasksLoading] 为 true 时有意义（进度文案用）。 */
    val taskTotal: Long = 0,
)

/** 移动端订阅的事件子集（`DaemonEvent` / `WsServerMsg` 经 Rust 侧归一）。 */
sealed interface HostEvent {
    data class TaskChanged(val task: Task) : HostEvent
    data class TaskDeleted(val taskId: String) : HostEvent

    /** `WsServerMsg::TaskProgress`：速度只在这里出现。 */
    data class TaskProgress(
        val taskId: String,
        val status: Int,
        val downloadedBytes: Long,
        val totalBytes: Long,
        val speed: Long,
        val uploadSpeed: Long,
        val fileName: String,
        val errorMessage: String,
        val uploadedBytes: Long,
        val seedingStatus: Int,
    ) : HostEvent

    data class TaskRuntimeChanged(val runtime: TaskRuntime) : HostEvent
    data class QueuesChanged(val queues: List<Queue>) : HostEvent

    /** 队列顺序整表替换（首次 / 用户重排）：[order] 按键升序。 */
    data class QueuePositionsChanged(val order: List<QueueOrderEntry>) : HostEvent

    /** 队列顺序增量：先移除 [removed]，再按键插入 / 更新 [added]。 */
    data class QueueOrderChanged(val removed: List<String>, val added: List<QueueOrderEntry>) : HostEvent
    data class GroupsChanged(val groups: List<TaskGroup>) : HostEvent
    data class FileMissingChanged(val updates: Map<String, Boolean>) : HostEvent
    data class PriorityTaskChanged(val taskId: String?) : HostEvent
    data class RuntimeStatsChanged(val stats: RuntimeStats) : HostEvent
    data class DaemonConnectionChanged(val connected: Boolean) : HostEvent
    data class SelectionPending(val request: SelectionRequest) : HostEvent
    data class SelectionResolved(val requestId: String) : HostEvent
    data class ConfigChanged(val values: Map<String, String>, val revision: Long) : HostEvent
    data class RssSourcesChanged(val sources: List<RssSource>) : HostEvent
    data class CloudDevicesChanged(val devices: List<CloudDevice>) : HostEvent
    data class LinkedDevicesChanged(val devices: List<LinkDevice>) : HostEvent
    data class CategoriesChanged(val categories: List<Category>) : HostEvent

    /** 通用分区变化（键见 Rust `native/mobile/src/sections.rs`）：[json] 是该分区的完整新值（协议 serde wire）。 */
    data class SectionChanged(val name: String, val json: String) : HostEvent

    /** 一次性通知（不进快照）：[name] 为 serde 变体名，[json] 为载荷。 */
    data class Notice(val name: String, val json: String) : HostEvent
}

/** `ApplicationErrorCode`。 */
enum class HostErrorCode {
    ProtocolIncompatible, Unauthorized, InvalidArgument, NotFound, Conflict,
    Unavailable, Timeout, Cancelled, Unsupported, Internal,
}

/** Structured plugin failure arguments, separate from the daemon's error message. */
data class PluginDiagnostic(
    val kind: String,
    val path: String?,
    val line: UInt?,
    val column: UInt?,
)

/**
 * `RpcErrorData`：UI 先按 [reason]（`ErrorReason` 的 wire 名）本地化，再按 [code] 回退。
 */
class HostException(
    val code: HostErrorCode,
    val reason: String? = null,
    val retryable: Boolean = false,
    message: String? = null,
    cause: Throwable? = null,
    val pluginDiagnostic: PluginDiagnostic? = null,
) : Exception(message ?: reason ?: code.name, cause)
