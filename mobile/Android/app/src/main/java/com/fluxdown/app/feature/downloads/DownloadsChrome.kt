package com.fluxdown.app.feature.downloads

import androidx.compose.foundation.layout.widthIn
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.i18n.fill
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.Route
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.app.ui.label
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.fluxui.chrome.FluxRailHeader
import com.fluxdown.fluxui.chrome.FluxRailItem
import com.fluxdown.fluxui.chrome.SelectionDock
import com.fluxdown.fluxui.chrome.SelectionDockAction
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.FluxWindowClass
import kotlinx.coroutines.launch

private fun StatusFolder.icon() = when (this) {
    StatusFolder.All -> FluxIcons.Layers
    StatusFolder.Active -> FluxIcons.Download
    StatusFolder.Completed -> FluxIcons.CheckCheck
    StatusFolder.Seeding -> FluxIcons.Upload
    StatusFolder.Failed -> FluxIcons.FileWarning
    StatusFolder.Paused -> FluxIcons.Pause
}

/**
 * expanded 档 Rail 的下载上下文：状态文件夹（带计数）· 选中文件夹下嵌套的分类子项 · 队列区（运行点 + 任务数）。
 * 选择直接更新共享视图状态。各部分的显隐由云同步偏好 `ui.show_sidebar_status|queues|category` 决定；
 * 状态区被隐藏时分类子项改为独立分区。
 */
@Composable
fun DownloadsRailContext() {
    val view = LocalDownloadsView.current
    val haptics = FluxTheme.haptics
    val facets = view.facets
    val filter = view.filter
    val visibility = view.filterVisibility

    if (visibility.status) {
        FluxRailHeader(text = stringResource(R.string.sidebarStatus))
        for (f in StatusFolder.entries) {
            val selected = filter.folder == f
            val count = facets.count(f)
            FluxRailItem(
                label = f.label(),
                onClick = {
                    haptics.tick()
                    view.setFolder(f)
                },
                icon = f.icon(),
                selected = selected,
                count = count,
                hot = f == StatusFolder.Failed && count > 0,
            )
            if (selected && visibility.categories) RailCategoryItems(view, nested = true)
        }
    } else if (visibility.categories && (facets.categories.isNotEmpty() || facets.remoteCount > 0 || filter.remoteOnly)) {
        FluxRailHeader(text = stringResource(R.string.sidebarCategory))
        RailCategoryItems(view, nested = false)
    }

    if (visibility.queues && facets.queues.isNotEmpty()) {
        FluxRailHeader(text = stringResource(R.string.sidebarQueues))
        for (qf in facets.queues) {
            FluxRailItem(
                label = qf.queue.label(),
                onClick = {
                    haptics.tick()
                    view.setQueue(qf.queue.queueId)
                },
                selected = filter.queueId == normQueue(qf.queue.queueId),
                count = qf.count,
                dot = qf.queue.isRunning,
            )
        }
    }
}

/** Rail 里的分类项：[nested] = 嵌套在选中的状态文件夹下（缩进子项）。 */
@Composable
private fun RailCategoryItems(view: DownloadsView, nested: Boolean) {
    val haptics = FluxTheme.haptics
    val filter = view.filter
    for (pill in view.facets.categories) {
        val cat = pill.category
        val catSelected = filter.categoryId == cat.id
        FluxRailItem(
            label = cat.label(),
            onClick = {
                haptics.tick()
                view.setCategory(if (catSelected) null else cat.id)
            },
            selected = catSelected,
            count = pill.count,
            sub = nested,
        )
    }
    // 「远程任务」：只看其他设备上的远程任务（与分类单选互斥）
    if (view.facets.remoteCount > 0 || filter.remoteOnly) {
        FluxRailItem(
            label = stringResource(R.string.remoteTasksGroup),
            onClick = {
                haptics.tick()
                view.setRemoteOnly(!filter.remoteOnly)
            },
            icon = FluxIcons.Cloud,
            selected = filter.remoteOnly,
            count = view.facets.remoteCount,
            sub = nested,
        )
    }
}

@Immutable
private data class SelectionCaps(val canResume: Boolean, val canPause: Boolean) {
    companion object {
        val None = SelectionCaps(false, false)
    }
}

/**
 * 多选模式的选择坞（D2）：计数 / 全选（范围 = 可见任务）· 继续 · 暂停 · 移动到队列 · 删除。
 * 选中项因删除 / 主机切换消失时自动剔除；选择归零（取消全选 / 逐个取消 / 删除后）自动退出。
 */
@Composable
fun DownloadsSelectionDock(modifier: Modifier) {
    SelectionDockContent(modifier, reservedEnd = FluxTheme.space.dockHeight + FluxTheme.space.dockOrbGap)
}

/** expanded 档：由列表栏自己浮在底部居中（壳层在该档不放坞与球）。 */
@Composable
internal fun ExpandedSelectionDock(modifier: Modifier = Modifier) {
    SelectionDockContent(modifier.widthIn(max = 560.dp), reservedEnd = 0.dp)
}

@Composable
private fun SelectionDockContent(modifier: Modifier, reservedEnd: Dp) {
    val nav = LocalNavigator.current
    val view = LocalDownloadsView.current
    val container = LocalAppContainer.current
    val actions = LocalTaskActions.current
    val haptics = FluxTheme.haptics
    val host = hostState()

    val selecting = nav.selecting
    val count = nav.selection.size
    val total by remember(view) { derivedStateOf { view.list.visibleIds.size } }
    val caps by remember(host, nav) {
        derivedStateOf {
            if (!nav.selecting || nav.selection.isEmpty()) {
                SelectionCaps.None
            } else {
                var resume = false
                var pause = false
                val sel = nav.selection
                for (t in host.value.tasks) {
                    if (t.taskId !in sel) continue
                    if (t.canResume) resume = true
                    if (t.canPause) pause = true
                }
                SelectionCaps(resume, pause)
            }
        }
    }

    LaunchedEffect(selecting) {
        if (!selecting) return@LaunchedEffect
        var had = nav.selection.isNotEmpty()
        launch {
            snapshotFlow { nav.selection.size }.collect { n ->
                if (n > 0) had = true else if (had) nav.exitSelection()
            }
        }
        container.store.state.collect { s ->
            // 任务表载入中列表是空的，不能据此判定“已选任务都被删除了”。
            if (nav.selection.isEmpty() || s.tasksLoading) return@collect
            val ids = if (nav.selection.size > 8) s.tasks.mapTo(HashSet()) { it.taskId } else null
            val gone = nav.selection.filter { id -> if (ids != null) id !in ids else s.tasks.none { it.taskId == id } }
            if (gone.isNotEmpty()) nav.selection.removeAll(gone.toSet())
        }
    }

    fun selectedTasks(): List<Task> {
        val sel = nav.selection
        return container.store.state.value.tasks.filter { it.taskId in sel }
    }

    val resumeLabel = stringResource(R.string.resume)
    val pauseLabel = stringResource(R.string.pause)
    val moveLabel = stringResource(R.string.mobileDockMove)
    val deleteLabel = stringResource(R.string.delete)
    val selectedFmt = stringResource(R.string.selectedCount)

    val dockActions = remember(caps, actions, resumeLabel, pauseLabel, moveLabel, deleteLabel) {
        buildList {
            if (caps.canResume) {
                add(SelectionDockAction("resume", resumeLabel, FluxIcons.Play, { actions.resume(selectedTasks().map { it.taskId }) }))
            }
            if (caps.canPause) {
                add(SelectionDockAction("pause", pauseLabel, FluxIcons.Pause, { actions.pause(selectedTasks().map { it.taskId }) }))
            }
            add(SelectionDockAction("move", moveLabel, FluxIcons.Rows3, { actions.moveToQueue(selectedTasks().map { it.taskId }) }))
            add(
                SelectionDockAction(
                    "delete", deleteLabel, FluxIcons.Trash2,
                    { actions.confirmDelete(selectedTasks()) { nav.exitSelection() } },
                    danger = true,
                ),
            )
        }
    }

    // expanded 档且只选 1 项、右栏已开：不显示选择坞
    val paneOpen = FluxTheme.windowClass == FluxWindowClass.Expanded && nav.top is Route.TaskDetail

    SelectionDock(
        count = count,
        total = total,
        onToggleAll = {
            haptics.tick()
            val visible = view.list.visibleIds
            val all = visible.isNotEmpty() && visible.all { it in nav.selection }
            if (all) nav.exitSelection() else nav.selection.addAll(visible)
        },
        actions = dockActions,
        visible = selecting && !(paneOpen && count <= 1),
        modifier = modifier,
        reservedEnd = reservedEnd,
        selectAllLabel = stringResource(R.string.selectAll),
        deselectAllLabel = stringResource(R.string.deselectAll),
        countDescription = { selectedFmt.fill("n" to it) },
        allSelectedDescription = stringResource(R.string.mobileSelectionAllOn),
        notAllSelectedDescription = stringResource(R.string.mobileSelectionAllOff),
        contentDescription = stringResource(R.string.mobileSelectionToolbar),
    )
}
