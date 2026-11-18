package com.fluxdown.app.feature.task

import androidx.compose.runtime.Immutable
import com.fluxdown.app.ui.TaskVisualState
import com.fluxdown.app.ui.visualState
import com.fluxdown.core.model.Category
import com.fluxdown.core.model.CategoryIndex
import com.fluxdown.core.model.Queue
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskGroup
import com.fluxdown.core.model.TaskRuntime
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.store.HostState
import com.fluxdown.fluxui.data.FlowSegmentUi
import com.fluxdown.fluxui.data.FlowStripState

/**
 * 详情页一次渲染所需的只读派生量。数据类的结构相等让 `derivedStateOf` 在字段没变化时不触发重组
 * （主机状态约 10 Hz 发布，但单个任务的字段变化频率远低于此）。
 */
@Immutable
internal data class DetailModel(
    val task: Task,
    val visual: TaskVisualState,
    /** 排队序号（1 起）；0 = 不在排队。 */
    val queuePosition: Int,
    val queue: Queue?,
    val group: TaskGroup?,
    val category: Category?,
    val speedDown: Long,
    val speedUp: Long,
    val runtime: TaskRuntime?,
    val boosted: Boolean,
) {
    val isTransferring: Boolean get() = task.status == TaskStatus.Downloading
    val progress: Float? get() = runtime?.hlsFragments?.fraction ?: task.progress

    /** HLS 点播按分片计数，其他任务按真实字节区间或总字节进度。 */
    val flowSegments: List<FlowSegmentUi>
        get() {
            val done = visual == TaskVisualState.Completed || visual == TaskVisualState.Seeding
            if (done) return FULL
            runtime?.hlsFragments?.fraction?.let { return listOf(FlowSegmentUi(1f, it)) }
            val segs = runtime?.segments.orEmpty()
            if (segs.isEmpty() || task.totalBytes <= 0) {
                return listOf(FlowSegmentUi(1f, task.progress ?: 0f))
            }
            val live = isTransferring
            return segs.map { s ->
                val len = (s.endByte - s.startByte + 1).coerceAtLeast(1)
                FlowSegmentUi(
                    fraction = len.toFloat(),
                    filled = (s.downloadedBytes.toFloat() / len).coerceIn(0f, 1f),
                    active = live && s.active == true,
                )
            }
        }

    val flowState: FlowStripState
        get() = when (visual) {
            TaskVisualState.Downloading -> FlowStripState.Downloading
            TaskVisualState.Queued -> FlowStripState.Queued
            TaskVisualState.Pending -> FlowStripState.Pending
            TaskVisualState.Preparing, TaskVisualState.Verifying -> FlowStripState.Preparing
            TaskVisualState.Paused -> FlowStripState.Paused
            TaskVisualState.Failed -> FlowStripState.Failed
            TaskVisualState.Seeding -> FlowStripState.Seeding
            TaskVisualState.Missing -> FlowStripState.Missing
            TaskVisualState.Completed -> FlowStripState.Completed
        }

    private companion object {
        val FULL = listOf(FlowSegmentUi(1f, 1f))
    }
}

/** 分类索引缓存：规则含正则编译，只在分类列表（按引用）变化时重建。 */
internal class CategoryIndexCache {
    private var source: List<Category>? = null
    private var index: CategoryIndex? = null

    fun of(categories: List<Category>): CategoryIndex {
        val cached = index
        if (cached != null && source === categories) return cached
        return CategoryIndex(categories).also { source = categories; index = it }
    }
}

internal fun buildDetailModel(state: HostState, taskId: String, categories: CategoryIndexCache): DetailModel? {
    val task = state.task(taskId) ?: return null
    val pos = state.queueOrder.rank(taskId)
    val speed = state.speeds[taskId]
    return DetailModel(
        task = task,
        visual = task.visualState(pos > 0),
        queuePosition = pos,
        queue = state.queues.firstOrNull { it.queueId == task.queueId || (task.queueId.isEmpty() && it.queueId == Queue.MAIN) },
        group = task.groupId.takeIf { it.isNotEmpty() }?.let { id -> state.groups.firstOrNull { it.groupId == id } },
        category = categories.of(state.categories).categoryOf(task.fileName, task.shareUrl),
        speedDown = speed?.down ?: 0L,
        speedUp = speed?.up ?: 0L,
        runtime = state.runtime[taskId],
        boosted = state.priorityTaskId == taskId,
    )
}
