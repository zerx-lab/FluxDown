package com.fluxdown.app.feature.downloads

import android.content.Context
import com.fluxdown.app.R
import com.fluxdown.app.feature.settings.account.AccountText
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.AppNavigator
import com.fluxdown.core.protocol.RemoteBatch
import com.fluxdown.core.protocol.RemoteBatchOutcome
import com.fluxdown.core.protocol.RemoteCommandAction
import com.fluxdown.core.protocol.RemoteTaskDto
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogButtonStyle
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.theme.FluxHaptics

/**
 * 多选坞里的远程任务部分：选择集合里的远程项（[RemoteItem.id]，与本地任务 id 不相交）→ 经控制矩阵与目标在线规则
 * （[RemoteBatch.eligible]）过滤 → [RemoteCommands.issueMany]（`agent.remote.commandMany`，旧主机逐条回退）。
 * 本地项仍走 `TaskActions` → 守护进程，远程 id 不会进入本机调用。
 */
internal class SelectionBatch(
    private val context: Context,
    private val view: DownloadsView,
    private val nav: AppNavigator,
    private val overlays: FluxOverlayState,
    private val haptics: FluxHaptics,
) {
    /** 选中的远程任务（含当前筛选之外仍被选中的）。 */
    fun selected(): List<RemoteTaskDto> {
        val sel = nav.selection
        if (sel.none(::isRemoteSelectionId)) return emptyList()
        return view.list.remote.filter { it.id in sel }.map { it.task }
    }

    /** 选中的远程任务中 [action] 适用的（状态矩阵 + 暂停 / 继续要求目标在线）。 */
    fun eligible(action: RemoteCommandAction): List<RemoteTaskDto> =
        RemoteBatch.eligible(selected(), action) { view.remoteTargets[it.toDevice]?.online }

    /**
     * 对选中的远程任务执行 [action]。[announceDone] = 只选了远程项：成功时给出汇总 toast
     * （含本地项时本地动作自带 toast，这里只报告失败）。
     */
    fun run(action: RemoteCommandAction, tasks: List<RemoteTaskDto>, deleteFiles: Boolean = false, announceDone: Boolean) {
        if (tasks.isEmpty()) return
        view.remoteCommands.issueMany(action, tasks, deleteFiles) { outcome ->
            if (action == RemoteCommandAction.Delete) {
                nav.selection.removeAll(outcome.succeeded.mapTo(HashSet()) { REMOTE_ID_PREFIX + it })
            }
            report(action, outcome, announceDone)
        }
    }

    private fun report(action: RemoteCommandAction, outcome: RemoteBatchOutcome, announceDone: Boolean) {
        val first = outcome.failed.firstOrNull()
        if (first != null) {
            haptics.reject()
            overlays.toast(
                context.str(
                    R.string.remoteBatchPartial,
                    "done" to outcome.succeeded.size,
                    "failed" to outcome.failed.size,
                    "reason" to AccountText.remoteError(context, first.toException()),
                ),
                FluxToastKind.Error,
            )
            return
        }
        if (!announceDone || outcome.succeeded.isEmpty()) return
        if (action == RemoteCommandAction.Delete) haptics.confirm()
        val (kind, icon) = when (action) {
            RemoteCommandAction.Resume -> FluxToastKind.Accent to FluxIcons.Play
            RemoteCommandAction.Pause -> FluxToastKind.Info to FluxIcons.Pause
            RemoteCommandAction.Cancel -> FluxToastKind.Info to FluxIcons.X
            RemoteCommandAction.Delete -> FluxToastKind.Info to FluxIcons.Trash2
        }
        overlays.toast(context.str(R.string.remoteBatchDone, "n" to outcome.succeeded.size), kind, icon)
    }

    /**
     * 含远程项的删除确认（保留文件 / 连同文件两档，与本地删除同一对话框形态）。
     * [deleteLocal] 由调用方转交 `TaskActions`（本地 id → 守护进程）。
     */
    fun confirmDelete(localCount: Int, remote: List<RemoteTaskDto>, deleteLocal: (withFiles: Boolean) -> Unit) {
        val total = localCount + remote.size
        if (total == 0) return
        fun go(withFiles: Boolean) {
            if (localCount > 0) deleteLocal(withFiles)
            run(RemoteCommandAction.Delete, remote, deleteFiles = withFiles, announceDone = localCount == 0)
        }
        val single = remote.singleOrNull()?.takeIf { localCount == 0 }
        overlays.showDialog(
            FluxDialogSpec(
                title = if (single != null) context.str(R.string.deleteTask) else context.str(R.string.mobileDeleteNTitle, "n" to total),
                message = if (single != null) {
                    single.fileName.ifEmpty { single.url }
                } else {
                    context.str(R.string.mobileDeleteNMessage)
                },
                icon = FluxIcons.Trash2,
                buttons = listOf(
                    FluxDialogButton(context.str(R.string.cancel)),
                    FluxDialogButton(context.str(R.string.deleteTaskAndFile), FluxDialogButtonStyle.Destructive) { go(true) },
                    FluxDialogButton(context.str(R.string.deleteTask), FluxDialogButtonStyle.Primary) { go(false) },
                ),
                stacked = true,
            ),
        )
    }
}
