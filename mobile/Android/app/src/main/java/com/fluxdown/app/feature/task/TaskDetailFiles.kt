package com.fluxdown.app.feature.task

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.core.format.Format
import com.fluxdown.core.host.HostException
import com.fluxdown.core.protocol.BtFilePriority
import com.fluxdown.core.protocol.BtFiles
import com.fluxdown.core.protocol.BtTaskFile
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.callJson
import com.fluxdown.core.protocol.jsonObject
import com.fluxdown.fluxui.controls.FluxProgressLine
import com.fluxdown.fluxui.controls.FluxTag
import com.fluxdown.fluxui.feedback.FluxEmpty
import com.fluxdown.fluxui.feedback.FluxGlyph
import com.fluxdown.fluxui.feedback.FluxSkeletonRows
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxMenuItem
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.fluxPressable
import kotlinx.coroutines.delay

/*
 * BT 详情「文件」页（移动端子集：只看与改优先级；文件重命名 / 多选批量改不在移动端）。
 * 文件表随 LazyColumn 逐行懒加载，上千个文件也只组合可见行。
 */

private const val BT_FILES_REFRESH_MS = 2_000L

/** 优先级四档的文案。 */
@StringRes
internal fun btFilePriorityLabel(priority: BtFilePriority): Int = when (priority) {
    BtFilePriority.Skip -> R.string.btFilePrioritySkip
    BtFilePriority.Normal -> R.string.btFilePriorityNormal
    BtFilePriority.High -> R.string.btFilePriorityHigh
    BtFilePriority.Maximum -> R.string.btFilePriorityMaximum
}

/** 「文件」页的轮询结果。[refresh] 自增 = 立即重拉（改优先级成功后）。 */
@Stable
internal class BtFilesState(val taskId: String) {
    var files by mutableStateOf<BtFiles?>(null)
    var loaded by mutableStateOf(false)
    var refresh by mutableIntStateOf(0)
}

/**
 * `daemon.task.btFiles` 每 2 s 轮询一次：只在 [visible]（「文件」页签选中）且页面在前台（STARTED）时运行，
 * 页签切走 / 任务变化 / 退到后台即停（同 BT 设置页端口状态轮询）。失败保留上一份数据；
 * 每次进入只 toast 一次（含旧主机不认识该方法），不在每轮轮询时刷屏。
 */
@Composable
internal fun rememberBtFilesState(taskId: String, visible: Boolean): BtFilesState {
    val container = LocalAppContainer.current
    val actions = LocalTaskActions.current
    val overlays = LocalFluxOverlays.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val state = remember(taskId) { BtFilesState(taskId) }
    LaunchedEffect(state, visible, state.refresh, lifecycleOwner) {
        if (!visible) return@LaunchedEffect
        var reported = false
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (true) {
                try {
                    val fetched = BtFiles.fromJson(
                        container.session.callJson(HostMethod.daemonTaskBtFiles, jsonObject("taskId" to state.taskId)),
                    )
                    if (fetched != null) state.files = fetched
                } catch (e: HostException) {
                    if (!reported) {
                        reported = true
                        overlays.toast(actions.errorText(e), FluxToastKind.Error)
                    }
                }
                state.loaded = true
                delay(BT_FILES_REFRESH_MS)
            }
        }
    }
    return state
}

/** 「文件」页的懒加载行：加载中骨架 / 空态 / 每个文件一行。 */
internal fun LazyListScope.btFileItems(state: BtFilesState, margin: Dp) {
    val data = state.files
    if (data == null && !state.loaded) {
        item(key = "files-loading") {
            Box(Modifier.padding(horizontal = margin).padding(top = 16.dp)) {
                FluxSkeletonRows(rows = 4, loadingLabel = str(R.string.pluginCommonLoading))
            }
        }
        return
    }
    if (data == null || data.files.isEmpty()) {
        item(key = "files-empty") {
            FluxEmpty(glyph = FluxGlyph.File, title = str(R.string.btFilesEmpty), modifier = Modifier.fillMaxWidth())
        }
        return
    }
    items(data.files, key = { "file-${it.index}" }) { file -> BtFileRow(state, data, file, margin) }
}

/**
 * 一行文件:文件名单行中间省略(保留扩展名)、所在目录、大小 · 进度、优先级标签与进度条。
 * 点按:可编辑时弹出四档优先级菜单,只读时显示完整路径;长按:完整路径。
 */
@Composable
private fun BtFileRow(state: BtFilesState, data: BtFiles, file: BtTaskFile, margin: Dp) {
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val actions = LocalTaskActions.current
    val overlays = LocalFluxOverlays.current
    var anchor by remember { mutableStateOf(Rect.Zero) }
    val labels = BtFilePriority.entries.associateWith { str(btFilePriorityLabel(it)) }
    val closeLabel = str(R.string.close)
    val skipped = file.priority == BtFilePriority.Skip
    val showPath = {
        overlays.showDialog(
            FluxDialogSpec(
                title = file.name,
                message = file.path,
                buttons = listOf(FluxDialogButton(closeLabel, onClick = {})),
            ),
        )
    }
    val openPriority = {
        overlays.showMenu(
            anchor,
            BtFilePriority.entries.map { p ->
                FluxMenuItem.Action(
                    label = labels.getValue(p),
                    onClick = {
                        actions.setBtFilePriority(state.taskId, data.files, setOf(file.index), p) { state.refresh++ }
                    },
                    checked = p == file.priority,
                    radio = true,
                )
            },
            header = file.name,
        )
    }
    Column(
        Modifier
            .fillMaxWidth()
            .onGloballyPositioned { anchor = it.boundsInRoot() }
            .fluxPressable(
                onClick = if (data.editable) openPriority else showPath,
                onLongClick = showPath,
            )
            .padding(horizontal = margin, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                FluxText(
                    file.name,
                    style = type.weight(type.body, 450),
                    color = if (skipped) c.inkFaint else c.ink,
                    maxLines = 1,
                    overflow = TextOverflow.MiddleEllipsis,
                )
                if (file.directory.isNotEmpty()) {
                    FluxText(file.directory, style = type.sm, color = c.inkFaint, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                }
                FluxText(
                    "${Format.bytes(file.size)} · ${Format.percent(file.progress)}",
                    style = type.monoS,
                    color = c.inkMuted,
                    maxLines = 1,
                )
            }
            FluxTag(labels.getValue(file.priority))
        }
        FluxProgressLine(file.progress, Modifier.fillMaxWidth())
    }
}
