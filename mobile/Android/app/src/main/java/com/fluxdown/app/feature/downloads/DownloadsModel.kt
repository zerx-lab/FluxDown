package com.fluxdown.app.feature.downloads

import androidx.compose.runtime.Immutable
import com.fluxdown.app.ui.TaskVisualState
import com.fluxdown.core.model.Category
import com.fluxdown.core.model.HlsFragmentProgress
import com.fluxdown.core.model.Queue
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.protocol.RemoteTaskDto
import com.fluxdown.core.protocol.RemoteTaskStatus
import com.fluxdown.core.store.TaskTable
import com.fluxdown.fluxui.data.FlowSegmentUi
import com.fluxdown.fluxui.data.FlowStripState

/** 状态文件夹（ScopeTabs / Rail）；顺序即显示顺序。 */
enum class StatusFolder {
    All, Active, Completed, Seeding, Failed, Paused;

    /** 本地任务归属：做种中 = 已完成且正在 / 排队做种（仍同时属于已完成，同 qBittorrent）。 */
    fun accepts(task: Task): Boolean = when (this) {
        Seeding -> task.isSeeding
        else -> accepts(task.status)
    }

    fun accepts(status: TaskStatus): Boolean = when (this) {
        All -> true
        Active -> status == TaskStatus.Pending || status == TaskStatus.Downloading || status == TaskStatus.Preparing
        Completed -> status == TaskStatus.Completed
        Seeding -> false
        Failed -> status == TaskStatus.Failed
        Paused -> status == TaskStatus.Paused
    }

    /**
     * 其他设备上的远程任务按 GPUI `DownloadTaskView::remote` 的状态映射归入文件夹（同 iOS）：
     * 等待接单 / 已接单 / 未知 / 下载中 → 下载中，失败 / 已取消 → 失败。
     */
    fun accepts(status: RemoteTaskStatus): Boolean = when (this) {
        All -> true
        Active -> status == RemoteTaskStatus.Pending || status == RemoteTaskStatus.Accepted ||
            status == RemoteTaskStatus.Downloading || status is RemoteTaskStatus.Unknown
        Completed -> status == RemoteTaskStatus.Completed
        Seeding -> false
        Failed -> status == RemoteTaskStatus.Failed || status == RemoteTaskStatus.Canceled
        Paused -> status == RemoteTaskStatus.Paused
    }
}

/** 列表筛选：状态文件夹 · 分类（或「远程任务」）· 队列范围 · 搜索词。 */
@Immutable
data class DownloadsFilter(
    val folder: StatusFolder = StatusFolder.All,
    val categoryId: String? = null,
    /** 只看其他设备上的远程任务（分类芯片行的「远程任务」芯片，与 [categoryId] 互斥）。 */
    val remoteOnly: Boolean = false,
    /** 已规范化的队列 id（主队列 = [Queue.MAIN]）；null = 全部队列。 */
    val queueId: String? = null,
    val query: String = "",
) {
    val hasScope: Boolean get() = queueId != null || query.isNotBlank()
    val isDefault: Boolean get() = this == DownloadsFilter()
}

internal fun normQueue(id: String): String = if (id.isEmpty()) Queue.MAIN else id

/**
 * 本地任务与远程任务共用的排序 / 分区 / 分组投影：两者进同一个列表、同一个比较器（同 iOS）。
 * 状态桶 [statusBucket]：0 下载中 · 1 等待 · 2 失败 · 3 已暂停 · 4 已完成。
 */
sealed interface DownloadItem {
    val id: String

    /** 智能排序档（§3.9）：优先下载 0 → 活跃 1 → 排队 2 → 失败 3 → 暂停 4 → 完成 5。 */
    val sortTier: Int
    val createdSec: Long
    val sortName: String
    val sizeBytes: Long
    val sortProgress: Float?
    val sortSpeed: Long

    /** 「传输中」分区（其余进「历史」）。 */
    val inflight: Boolean
    val statusBucket: Int
    val category: Category?
    val site: String
}

/**
 * 一行任务的全部展示输入（在后台线程派生并复用实例：未变化的任务得到同一个对象，
 * 行组合项据此直接跳过重组）。流带 / ETA / 视觉状态都在这里算好，行只负责格式化文案。
 */
@Immutable
data class TaskItem(
    val task: Task,
    val speedDown: Long,
    val speedUp: Long,
    /** 引擎排序键（任务在队列期间不变）；null = 不在排队。名次不进模型：渲染可见行时按 `QueueOrder` 取。 */
    val queueOrder: Long?,
    val boosted: Boolean,
    override val category: Category?,
    val queue: Queue?,
    override val site: String,
    val visual: TaskVisualState,
    val flow: List<FlowSegmentUi>?,
    val flowState: FlowStripState,
    val progress: Float?,
    val hlsFragments: HlsFragmentProgress?,
    val eta: Long?,
) : DownloadItem {
    override val id: String get() = task.taskId
    override val sortTier: Int
        get() {
            val st = task.status
            return when {
                boosted && (st == TaskStatus.Downloading || st == TaskStatus.Pending) -> 0
                st == TaskStatus.Downloading || st == TaskStatus.Preparing -> 1
                st == TaskStatus.Pending -> 2
                st == TaskStatus.Failed -> 3
                st == TaskStatus.Paused -> 4
                else -> 5
            }
        }
    override val createdSec: Long get() = task.createdAt
    override val sortName: String get() = task.fileName
    override val sizeBytes: Long get() = task.totalBytes
    override val sortProgress: Float? get() = progress
    override val sortSpeed: Long get() = speedDown
    override val inflight: Boolean
        get() = task.status == TaskStatus.Pending || task.status == TaskStatus.Downloading || task.status == TaskStatus.Preparing
    override val statusBucket: Int
        get() = when (task.status) {
            TaskStatus.Downloading -> 0
            TaskStatus.Pending, TaskStatus.Preparing -> 1
            TaskStatus.Failed -> 2
            TaskStatus.Paused -> 3
            TaskStatus.Completed, TaskStatus.Unknown -> 4
        }
}

/**
 * 其他设备上执行的远程任务（云端下发，同 PC 下载页的远程行）。状态投影与 [StatusFolder.accepts] 一致：
 * 等待接单 / 已接单 / 未知 → 等待（传输中），失败 / 已取消 → 失败。不参与多选。
 */
@Immutable
data class RemoteItem(
    val task: RemoteTaskDto,
    /** 显示名：云端未带文件名时按 URL 推断。 */
    val name: String,
    override val category: Category?,
    override val site: String,
    /** `createdAt`（ISO-8601）解析出的 Unix 秒；解析失败 0。 */
    override val createdSec: Long,
) : DownloadItem {
    override val id: String get() = "remote:${task.id}"
    override val sortTier: Int
        get() = when (task.status) {
            RemoteTaskStatus.Downloading -> 1
            RemoteTaskStatus.Pending, RemoteTaskStatus.Accepted, is RemoteTaskStatus.Unknown -> 2
            RemoteTaskStatus.Failed, RemoteTaskStatus.Canceled -> 3
            RemoteTaskStatus.Paused -> 4
            RemoteTaskStatus.Completed -> 5
        }
    override val sortName: String get() = name
    override val sizeBytes: Long get() = task.totalBytes ?: 0L
    override val sortProgress: Float?
        get() = if (task.status is RemoteTaskStatus.Unknown) null else task.progress.toFloat().coerceIn(0f, 1f)
    override val sortSpeed: Long get() = if (task.status.isTerminal) 0L else task.speed
    override val inflight: Boolean get() = sortTier <= 2
    override val statusBucket: Int
        get() = when (task.status) {
            RemoteTaskStatus.Downloading -> 0
            RemoteTaskStatus.Pending, RemoteTaskStatus.Accepted, is RemoteTaskStatus.Unknown -> 1
            RemoteTaskStatus.Failed, RemoteTaskStatus.Canceled -> 2
            RemoteTaskStatus.Paused -> 3
            RemoteTaskStatus.Completed -> 4
        }
}

/** 行所在的容器：传输中 / 分组卡片（圆角玻璃分区）与历史（出血安静列表）。 */
enum class RowZone { Flow, Card, History }

/** 分组标题：i18n 资源 / 原文 / 分类 / 队列（组合期解析为文案）。 */
@Immutable
data class GroupLabel(
    val res: Int = 0,
    val text: String = "",
    val category: Category? = null,
    val queue: Queue? = null,
)

/** 列表条目（LazyColumn 的扁平化模型）。 */
@Immutable
sealed interface ListEntry {
    val key: String
    val contentType: Int
}

internal object ContentType {
    const val Header = 0
    const val Banner = 1
    const val Hero = 2
    const val Slot = 3
    const val FlowHeader = 4
    const val HistoryHeader = 5
    const val GroupHeader = 6
    const val RowFlow = 7
    const val RowCard = 8
    const val RowHistory = 9
    const val Empty = 10
    const val Skeleton = 11
    const val Scope = 12
    const val RowRemote = 13
    const val Tail = 14
}

/** 「传输中」分区头：右侧实时汇总下行速度 + 任务数。 */
@Immutable
data class FlowHeaderEntry(val count: Int, val downSpeed: Long) : ListEntry {
    override val key: String get() = "h:flow"
    override val contentType: Int get() = ContentType.FlowHeader
}

/** 「历史」小标题；选了已完成 / 失败 / 已暂停文件夹时换成该文件夹名（[folder] 非 null）。 */
@Immutable
data class HistoryHeaderEntry(val count: Int, val folder: StatusFolder?) : ListEntry {
    override val key: String get() = "h:history"
    override val contentType: Int get() = ContentType.HistoryHeader
}

@Immutable
data class GroupHeaderEntry(
    val groupKey: String,
    val label: GroupLabel,
    val count: Int,
    val extra: String?,
    val closed: Boolean,
) : ListEntry {
    override val key: String get() = "g:$groupKey"
    override val contentType: Int get() = ContentType.GroupHeader
}

@Immutable
data class RowEntry(
    val item: TaskItem,
    val zone: RowZone,
    val first: Boolean,
    val last: Boolean,
) : ListEntry {
    override val key: String get() = "t:${item.id}"
    override val contentType: Int
        get() = when (zone) {
            RowZone.Flow -> ContentType.RowFlow
            RowZone.Card -> ContentType.RowCard
            RowZone.History -> ContentType.RowHistory
        }
}

/** 远程任务行：与本地行同一列表、同一分区样式（[zone]）；不参与多选。 */
@Immutable
data class RemoteRowEntry(
    val item: RemoteItem,
    val zone: RowZone,
    val first: Boolean,
    val last: Boolean,
) : ListEntry {
    override val key: String get() = "r:${item.task.id}"
    override val contentType: Int get() = ContentType.RowRemote
}

/**
 * 派生出的列表：条目 + 可见任务 id（全选范围）+ 总任务数（区分「无任务」与「筛选后为空」）。
 * [loading] 非空 = 主机的任务表正在分页载入：[entries] 为空不代表“没有任务”，页面只能显示加载态。
 */
@Immutable
data class DownloadsList(
    val entries: List<ListEntry>,
    val visibleIds: List<String>,
    val taskTotal: Int,
    val loaded: Boolean,
    val loading: TaskTable.Loading? = null,
) {
    companion object {
        val Initial = DownloadsList(emptyList(), emptyList(), 0, loaded = false)
    }
}

@Immutable
data class CategoryPill(val category: Category, val count: Int)

@Immutable
data class QueueFacet(val queue: Queue, val count: Int)

/** 分面：文件夹计数（范围内）· 当前文件夹内的分类计数 · 各队列任务数。只在数值变化时才发布。 */
@Immutable
data class Facets(
    /** 按 [StatusFolder.ordinal] 排列。 */
    val folderCounts: List<Int>,
    val categories: List<CategoryPill>,
    val queues: List<QueueFacet>,
    /** 当前筛选结果行数。 */
    val matching: Int,
    /** 范围内、当前文件夹内的远程任务数（分类筛选之前）：「远程任务」芯片的计数与显隐。 */
    val remoteCount: Int,
) {
    fun count(folder: StatusFolder): Int = folderCounts.getOrElse(folder.ordinal) { 0 }

    companion object {
        val Initial = Facets(List(StatusFolder.entries.size) { 0 }, emptyList(), emptyList(), 0, 0)
    }
}
