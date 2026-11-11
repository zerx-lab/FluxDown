package com.fluxdown.app.actions

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.text.input.KeyboardType
import androidx.core.content.FileProvider
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.AppNavigator
import com.fluxdown.app.nav.SheetRoute
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskProtocol
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.protocol.BtFilePriority
import com.fluxdown.core.protocol.BtFilePriorityChange
import com.fluxdown.core.protocol.BtTaskFile
import com.fluxdown.core.protocol.BtTaskLimits
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.btFilePriorityChange
import com.fluxdown.core.protocol.callUnit
import com.fluxdown.core.protocol.jsonObject
import com.fluxdown.core.protocol.jsonObjectOmitNulls
import com.fluxdown.core.store.HostStore
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogButtonStyle
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxMenuItem
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.theme.FluxHaptics
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import java.io.File

/**
 * 任务动作的唯一分发点：行环钮、滑动、长按菜单、选择坞、详情页都经此调用，
 * 保证只读拦截、错误文案、toast 与触感一致（PC `MenuEntry` 同序）。
 */
class TaskActions(
    private val context: Context,
    private val scope: CoroutineScope,
    private val session: () -> HostSession,
    private val store: HostStore,
    private val overlays: FluxOverlayState,
    private val nav: AppNavigator,
    private val haptics: FluxHaptics,
    /** 当前主机是否为本机：远端主机的文件在主机磁盘上，本机无法打开。 */
    private val isLocalHost: () -> Boolean,
) {
    private fun s(id: Int, vararg args: Pair<String, Any?>) = context.str(id, *args)

    /** 只读（断连宽限后）拒绝写操作：toast(error) + REJECT。 */
    fun guard(): Boolean {
        if (!store.state.value.isReadOnly) return true
        haptics.reject()
        overlays.toast(s(R.string.localServiceDisconnected), FluxToastKind.Error, FluxIcons.WifiOff)
        return false
    }

    private fun run(onSuccess: (() -> Unit)? = null, block: suspend HostSession.() -> Unit) {
        if (!guard()) return
        scope.launch {
            try {
                session().block()
                onSuccess?.invoke()
            } catch (e: HostException) {
                haptics.reject()
                overlays.toast(errorText(e), FluxToastKind.Error)
            }
        }
    }

    /** 先按 reason（`ErrorReason` wire 名）本地化，再按 code 回退。 */
    fun errorText(e: HostException): String = when (e.reason) {
        "taskFilesInUse" -> s(R.string.errReasonTaskFilesInUse)
        "taskFilesNotDeleted" -> s(R.string.errReasonTaskFilesNotDeleted)
        else -> when (e.code) {
            HostErrorCode.Conflict -> s(R.string.localServiceConflict)
            HostErrorCode.InvalidArgument -> s(R.string.localServiceInvalidArgument)
            HostErrorCode.Unavailable, HostErrorCode.Timeout -> s(R.string.localServiceDisconnected)
            else -> s(R.string.localServiceActionFailed)
        }
    }

    // ── 单任务 / 批量 ──────────────────────────────────────────────────────

    fun pause(ids: List<String>) = run({
        if (ids.size > 1) overlays.toast(s(R.string.mobileToastPausedN, "n" to ids.size), FluxToastKind.Info, FluxIcons.Pause)
    }) { if (ids.size == 1) pause(ids[0]) else pauseMany(ids) }

    fun resume(ids: List<String>) = run({
        if (ids.size > 1) overlays.toast(s(R.string.mobileToastResumedN, "n" to ids.size), FluxToastKind.Accent, FluxIcons.Play)
    }) { if (ids.size == 1) resume(ids[0]) else resumeMany(ids) }

    fun pauseAll() = run({ overlays.toast(s(R.string.mobilePausedAllToast), FluxToastKind.Info, FluxIcons.Pause) }) { pauseAll() }
    fun resumeAll() = run({ overlays.toast(s(R.string.mobileResumedAllToast), FluxToastKind.Accent, FluxIcons.Play) }) { resumeAll() }

    fun boost(task: Task, boosted: Boolean) = run({
        overlays.toast(s(if (boosted) R.string.mobileBoostOff else R.string.mobileBoostOn), FluxToastKind.Accent, FluxIcons.Zap)
    }) { boost(task.taskId) }

    fun moveToQueue(ids: List<String>) {
        if (guard()) nav.openSheet(SheetRoute.MoveToQueue(ids))
    }

    fun moveToQueueNow(ids: List<String>, queueId: String, queueName: String) = run({
        overlays.toast(s(R.string.mobileMovedToQueueNamed, "name" to queueName), FluxToastKind.Success)
    }) { ids.forEach { moveToQueue(it, queueId) } }

    /** 删除确认（保留文件 / 连同文件），PC 同两档。 */
    fun confirmDelete(tasks: List<Task>, onDone: () -> Unit = {}) {
        if (tasks.isEmpty() || !guard()) return
        val ids = tasks.map { it.taskId }
        val one = tasks.singleOrNull()
        overlays.showDialog(
            FluxDialogSpec(
                title = if (one != null) s(R.string.deleteTask) else s(R.string.mobileDeleteNTitle, "n" to tasks.size),
                message = if (one != null) s(R.string.deleteConfirmDescKeepFile, "fileName" to one.fileName) else s(R.string.mobileDeleteNMessage),
                icon = FluxIcons.Trash2,
                buttons = listOf(
                    FluxDialogButton(s(R.string.cancel)),
                    FluxDialogButton(s(R.string.deleteTaskAndFile), FluxDialogButtonStyle.Destructive) { delete(ids, true, onDone) },
                    FluxDialogButton(s(R.string.deleteTask), FluxDialogButtonStyle.Primary) { delete(ids, false, onDone) },
                ),
                stacked = true,
            ),
        )
    }

    private fun delete(ids: List<String>, withFiles: Boolean, onDone: () -> Unit) = run({
        haptics.confirm()
        val msg = when {
            ids.size > 1 -> s(R.string.mobileToastDeletedN, "n" to ids.size)
            withFiles -> s(R.string.mobileTaskFileDeleted)
            else -> s(R.string.mobileTaskDeleted)
        }
        overlays.toast(msg, FluxToastKind.Info, FluxIcons.Trash2)
        onDone()
    }) { if (ids.size == 1) delete(ids[0], withFiles) else deleteMany(ids, withFiles) }

    /** 可重新下载：本地任务且 url 不是 `torrent-file://` 哨兵（与 GPUI `redownload_commands` 同规则）。 */
    fun canRedownload(task: Task): Boolean =
        (task.status == TaskStatus.Completed || task.status == TaskStatus.Failed) && !task.url.startsWith("torrent-file://")

    /** 重新下载需确认：删除任务与已有文件后按原参数重建（GPUI `DownloadsCommand::Redownload` 同语义）。 */
    fun confirmRedownload(task: Task) {
        if (!guard()) return
        if (!canRedownload(task)) {
            haptics.reject()
            overlays.toast(s(R.string.mobileFileNotFound), FluxToastKind.Warn, FluxIcons.FileWarning)
            return
        }
        overlays.showDialog(
            FluxDialogSpec(
                title = s(R.string.redownloadTask),
                message = s(R.string.mobileRedownloadMessage, "fileName" to task.fileName),
                icon = FluxIcons.RotateCw,
                buttons = listOf(
                    FluxDialogButton(s(R.string.cancel)),
                    FluxDialogButton(s(R.string.redownloadTask), FluxDialogButtonStyle.Primary) { redownload(task) },
                ),
            ),
        )
    }

    private fun redownload(task: Task) = run({
        overlays.toast(s(R.string.mobileDownloadStarted), FluxToastKind.Accent, FluxIcons.RotateCw)
    }) {
        delete(task.taskId, true)
        createTask(
            com.fluxdown.core.host.CreateTaskRequest(
                url = task.url,
                fileName = task.fileName,
                saveDir = task.saveDir,
                queueId = task.queueId,
            ),
        )
    }

    fun rename(task: Task) {
        if (!guard()) return
        overlays.showDialog(
            FluxDialogSpec(
                title = s(R.string.renameTaskTitle),
                icon = FluxIcons.Pen,
                buttons = emptyList(),
                body = { TextPrompt(task.fileName, s(R.string.renameTaskPlaceholder), mono = false) { name ->
                    run({ overlays.toast(s(R.string.renameTaskSuccess), FluxToastKind.Success) }) { rename(task.taskId, name) }
                } },
            ),
        )
    }

    fun changeUrl(task: Task) {
        if (!guard()) return
        overlays.showDialog(
            FluxDialogSpec(
                title = s(R.string.mobileChangeUrl),
                icon = FluxIcons.Link,
                buttons = emptyList(),
                body = { TextPrompt(task.shareUrl, s(R.string.urlPlaceholder), mono = true) { url ->
                    run({ overlays.toast(s(R.string.mobileChangeUrlDone), FluxToastKind.Success) }) { changeUrl(task.taskId, url) }
                } },
            ),
        )
    }

    /** 对话框内输入：确认后关闭对话框并提交。 */
    @Composable
    private fun ColumnScope.TextPrompt(initial: String, placeholder: String, mono: Boolean, onSubmit: (String) -> Unit) {
        var value by remember { mutableStateOf(initial) }
        FluxField(value = value, onValueChange = { value = it }, placeholder = placeholder, mono = mono, singleLine = !mono, rows = if (mono) 3 else 1)
        com.fluxdown.fluxui.controls.FluxButton(
            text = str(R.string.confirm),
            onClick = {
                val v = value.trim()
                if (v.isEmpty()) haptics.reject() else {
                    overlays.dismissDialog()
                    if (v != initial) onSubmit(v)
                }
            },
            variant = com.fluxdown.fluxui.controls.ButtonVariant.Primary,
            fullWidth = true,
        )
    }

    // ── 本地动作（不经主机） ───────────────────────────────────────────────

    fun copyLink(task: Task) {
        val cm = context.getSystemService(ClipboardManager::class.java)
        cm.setPrimaryClip(ClipData.newPlainText(task.fileName, task.shareUrl))
        overlays.toast(s(R.string.urlCopied), FluxToastKind.Success, FluxIcons.Copy)
    }

    fun shareLink(task: Task) {
        val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, task.shareUrl)
        context.startActivity(Intent.createChooser(send, task.fileName).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }

    /** 本机已完成且文件仍在（iOS `hasLocalFile` 同条件）。远端主机的文件不在本机，不提供打开。 */
    fun hasLocalFile(task: Task): Boolean =
        isLocalHost() && task.status == TaskStatus.Completed && !task.fileMissing

    /** 打开当前主机列表里的已完成文件：远端主机 / 不存在 → 警告；存在 → ACTION_VIEW（FileProvider）。 */
    fun open(task: Task) {
        if (!hasLocalFile(task)) {
            haptics.reject()
            overlays.toast(s(R.string.mobileFileNotFound), FluxToastKind.Warn, FluxIcons.FileWarning)
            return
        }
        openLocalFile(task)
    }

    /**
     * 打开本机引擎的任务文件，不看当前查看的是哪台主机：完成提示里的本机任务（含 PC 下发到本机的）
     * 在用户正查看远端主机时同样要能打开。调用方须保证 [task] 来自本机主机状态。
     */
    fun openLocalFile(task: Task) {
        val file = File(task.saveDir, task.fileName)
        if (task.fileMissing || !file.exists()) {
            haptics.reject()
            overlays.toast(s(R.string.mobileFileNotFound), FluxToastKind.Warn, FluxIcons.FileWarning)
            return
        }
        val uri = try {
            FileProvider.getUriForFile(context, "${context.packageName}.files", file)
        } catch (e: IllegalArgumentException) {
            // 文件不在 FileProvider 任何已声明根目录内（异常存储布局）：提示而不是崩溃
            android.util.Log.w("FluxDown", "open: ${file.path} is outside the FileProvider roots: ${e.message}")
            haptics.reject()
            overlays.toast(s(R.string.mobileOpenFileFailed), FluxToastKind.Error, FluxIcons.FileWarning)
            return
        }
        val mime = mimeForFile(file.name)
        fun view(type: String) = Intent(Intent.ACTION_VIEW).setDataAndType(uri, type)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            context.startActivity(view(mime))
        } catch (e: android.content.ActivityNotFoundException) {
            android.util.Log.i("FluxDown", "no viewer for $mime: ${e.message}")
            // 指定类型无人处理时退回 */*，保持此前能打开的文件仍可打开
            try {
                if (mime == "*/*") throw e
                context.startActivity(view("*/*"))
            } catch (e2: android.content.ActivityNotFoundException) {
                overlays.toast(s(R.string.mobileNoAppToOpen), FluxToastKind.Warn)
            }
        }
    }

    /** 行环钮的主动作（按状态）。 */
    fun primary(task: Task) = when {
        task.status == TaskStatus.Completed && task.fileMissing -> confirmRedownload(task)
        task.status == TaskStatus.Completed && isLocalHost() -> open(task)
        task.status == TaskStatus.Completed -> nav.push(com.fluxdown.app.nav.Route.TaskDetail(task.taskId))
        task.status == TaskStatus.Paused || task.status == TaskStatus.Failed -> resume(listOf(task.taskId))
        else -> pause(listOf(task.taskId))
    }

    /**
     * 长按菜单 / 详情“更多”菜单。选片分组紧跟单任务编辑项（重命名 / 更换下载源），
     * 位于重新下载与复制链接之前。顺序是单选（最稀有优先 / 按顺序下载，混选都不点亮）；
     * 首尾文件块是独立复选框（全部开启才勾上，点击设为反值）。
     */
    fun menuItems(task: Task, boosted: Boolean, onSelect: (() -> Unit)? = null): List<FluxMenuItem> = buildList {
        val st = task.status
        if (hasLocalFile(task)) add(FluxMenuItem.Action(s(R.string.openFile), { open(task) }, FluxIcons.ExternalLink))
        if (task.canResume) add(FluxMenuItem.Action(s(R.string.resume), { resume(listOf(task.taskId)) }, FluxIcons.Play))
        if (task.canPause) add(FluxMenuItem.Action(s(R.string.pause), { pause(listOf(task.taskId)) }, FluxIcons.Pause))
        // 只有未结束的任务能优先下载（主机对已完成 / 失败任务拒绝 Boost）。
        if (st != TaskStatus.Completed && st != TaskStatus.Failed) {
            add(FluxMenuItem.Action(s(if (boosted) R.string.cancelBoost else R.string.boostDownload), { boost(task, boosted) }, FluxIcons.Zap, checked = boosted))
        }
        add(FluxMenuItem.Divider)
        val beforeEdit = size
        if (!st.isActive && task.protocol != TaskProtocol.Bt) add(FluxMenuItem.Action(s(R.string.renameTask), { rename(task) }, FluxIcons.Pen))
        if (st == TaskStatus.Failed || st == TaskStatus.Paused) add(FluxMenuItem.Action(s(R.string.mobileChangeUrl), { changeUrl(task) }, FluxIcons.Link))
        piecePickingState(listOf(task))?.let { picking ->
            if (size > beforeEdit) add(FluxMenuItem.Divider)
            addAll(piecePickingItems(listOf(task), picking))
        }
        btTaskMenu(listOf(task))?.let { bt ->
            val items = btTaskItems(listOf(task), bt)
            if (items.isNotEmpty() && size > beforeEdit) add(FluxMenuItem.Divider)
            addAll(items)
        }
        if (canRedownload(task)) add(FluxMenuItem.Action(s(R.string.redownloadTask), { confirmRedownload(task) }, FluxIcons.RotateCw))
        add(FluxMenuItem.Action(s(R.string.copyUrl), { copyLink(task) }, FluxIcons.Copy))
        add(FluxMenuItem.Action(s(R.string.mobileShareLink), { shareLink(task) }, FluxIcons.Share2))
        if (st != TaskStatus.Completed) add(FluxMenuItem.Action(s(R.string.moveToQueueAction), { moveToQueue(listOf(task.taskId)) }, FluxIcons.Rows3))
        if (onSelect != null) add(FluxMenuItem.Action(s(R.string.mobileMenuSelect), onSelect, FluxIcons.CheckCheck))
        add(FluxMenuItem.Divider)
        add(FluxMenuItem.Action(s(R.string.delete), { confirmDelete(listOf(task)) }, FluxIcons.Trash2, destructive = true))
    }

    /** 全部所选任务都是未完成 BT 时非 null（含正在控制的远端 PC / 服务器主机）。 */
    fun piecePickingState(tasks: List<Task>): PiecePickingMenu? = piecePickingMenu(tasks)

    /**
     * 对每个任务各发一次 RPC（[block]）：单任务失败不中断其余任务；有失败时最后走既有 RPC toast 一次，
     * 全部成功才回调 [onAllOk]。
     */
    private fun eachTask(tasks: List<Task>, onAllOk: (() -> Unit)? = null, block: suspend HostSession.(Task) -> Unit) {
        if (tasks.isEmpty() || !guard()) return
        scope.launch {
            var first: HostException? = null
            val host = session()
            for (task in tasks) {
                try {
                    host.block(task)
                } catch (e: HostException) {
                    if (first == null) first = e
                }
            }
            val failed = first
            if (failed == null) {
                onAllOk?.invoke()
                return@launch
            }
            haptics.reject()
            overlays.toast(errorText(failed), FluxToastKind.Error)
        }
    }

    /** 每个任务一次 `daemon.task.setPiecePicking`，只带被切换的字段。 */
    fun setPiecePicking(tasks: List<Task>, sequential: Boolean? = null, firstLastPiecesFirst: Boolean? = null) {
        if (sequential == null && firstLastPiecesFirst == null) return
        eachTask(tasks) { task ->
            callUnit(
                HostMethod.daemonTaskSetPiecePicking,
                jsonObjectOmitNulls(
                    "taskId" to task.taskId,
                    "sequential" to sequential,
                    "firstLastPiecesFirst" to firstLastPiecesFirst,
                ),
            )
        }
    }

    // ── BT 任务级（移动端子集：强制重新校验 / 任务限速 / 超级做种 / 文件优先级） ─────────
    // 刻意不在移动端（PC / Web 有）：Tracker 页的层级 / 做种数 / 下载数列与「HTTP 源」（web seeds）列表、
    // 移动存储（daemon.task.moveStorage）、文件重命名（daemon.task.renameBtFile）、文件多选批量改优先级、
    // 制作种子的高级选项（版本 / 分块大小 / 私有 / web seeds / 注释 / 来源 / 输出路径）。
    // 设置页的 BT 连接 / 汇报策略 / 代理 BT 分组与 PC 同键同序，不在此列。

    /** `daemon.task.recheck`：每个未完成 BT 任务一次；单任务成功后提示「正在重新校验」。 */
    fun recheck(tasks: List<Task>) {
        val one = tasks.singleOrNull()
        eachTask(
            tasks,
            onAllOk = one?.let { t ->
                { overlays.toast(s(R.string.btForceRecheckStarted, "name" to t.fileName), FluxToastKind.Info, FluxIcons.ShieldCheck) }
            },
        ) { task -> callUnit(HostMethod.daemonTaskRecheck, jsonObject("taskId" to task.taskId)) }
    }

    /** `daemon.task.setSuperSeeding`：只对状态不同于 [enabled] 的任务下发。 */
    fun setSuperSeeding(tasks: List<Task>, enabled: Boolean) {
        eachTask(superSeedingChange(tasks, enabled)) { task ->
            callUnit(HostMethod.daemonTaskSetSuperSeeding, jsonObject("taskId" to task.taskId, "enabled" to enabled))
        }
    }

    /** 任务限速对话框：KiB/s，0 = 不限；只发改动的字段（`daemon.task.setBtLimits`）。 */
    fun taskLimits(task: Task) {
        if (!guard()) return
        overlays.showDialog(
            FluxDialogSpec(
                title = s(R.string.btTaskLimitsTitle),
                message = s(R.string.btTaskLimitHint),
                icon = FluxIcons.Gauge,
                buttons = emptyList(),
                body = { TaskLimitsPrompt(task) },
            ),
        )
    }

    @Composable
    private fun ColumnScope.TaskLimitsPrompt(task: Task) {
        var down by remember { mutableStateOf(BtTaskLimits.kibText(task.btDownloadLimitBps)) }
        var up by remember { mutableStateOf(BtTaskLimits.kibText(task.seedUploadLimitBps)) }
        val unit = str(R.string.btGuardUnitKibps)
        val numeric = KeyboardOptions(keyboardType = KeyboardType.Number)
        FluxField(value = down, onValueChange = { down = limitDigits(it) }, label = str(R.string.btTaskLimitDownload), suffix = unit, mono = true, keyboardOptions = numeric)
        FluxField(value = up, onValueChange = { up = limitDigits(it) }, label = str(R.string.btTaskLimitUpload), suffix = unit, mono = true, keyboardOptions = numeric)
        com.fluxdown.fluxui.controls.FluxButton(
            text = str(R.string.confirm),
            onClick = {
                when (val change = BtTaskLimits.change(task.taskId, task.btDownloadLimitBps, task.seedUploadLimitBps, down, up)) {
                    BtTaskLimits.Result.Invalid -> haptics.reject()
                    BtTaskLimits.Result.NoChange -> overlays.dismissDialog()
                    is BtTaskLimits.Result.Send -> {
                        overlays.dismissDialog()
                        this@TaskActions.run(null) { callUnit(HostMethod.daemonTaskSetBtLimits, change.params) }
                    }
                }
            },
            variant = com.fluxdown.fluxui.controls.ButtonVariant.Primary,
            fullWidth = true,
        )
    }

    /**
     * 把 [indices] 这些文件设为 [priority]（`daemon.task.setFilePriorities`，部分更新）。
     * 会让全部文件都不下载时本地拦下并提示 `btFilesKeepOne`；[onSent] 在主机确认后回调（调用方据此刷新列表）。
     */
    fun setBtFilePriority(taskId: String, files: List<BtTaskFile>, indices: Set<Int>, priority: BtFilePriority, onSent: () -> Unit) {
        when (val change = btFilePriorityChange(taskId, files, indices, priority)) {
            BtFilePriorityChange.NoChange -> Unit
            BtFilePriorityChange.KeepOne -> {
                haptics.reject()
                overlays.toast(s(R.string.btFilesKeepOne), FluxToastKind.Warn)
            }
            is BtFilePriorityChange.Send -> run(onSent) { callUnit(HostMethod.daemonTaskSetFilePriorities, change.params) }
        }
    }

    /** 多选长按：自己读当前主机任务表。「选片方式」分组 + BT 批量项（重新校验 / 超级做种）；条件不满足时为空。 */
    fun btBatchMenuItems(taskIds: Set<String>): List<FluxMenuItem> {
        if (taskIds.size < 2) return emptyList()
        val tasks = store.state.value.tasks.filter { it.taskId in taskIds }
        if (tasks.size < 2) return emptyList()
        return buildList {
            piecePickingMenu(tasks)?.let { addAll(piecePickingItems(tasks, it)) }
            btTaskMenu(tasks)?.let { bt ->
                val items = btTaskItems(tasks, bt)
                if (items.isNotEmpty() && isNotEmpty()) add(FluxMenuItem.Divider)
                addAll(items)
            }
        }
    }

    private fun btTaskItems(tasks: List<Task>, bt: BtTaskMenu): List<FluxMenuItem> = buildList {
        if (bt.recheck) add(FluxMenuItem.Action(s(R.string.btForceRecheck), { recheck(tasks) }, FluxIcons.ShieldCheck))
        if (bt.limits) add(FluxMenuItem.Action(s(R.string.btTaskLimits), { taskLimits(tasks.single()) }, FluxIcons.Gauge))
        add(
            FluxMenuItem.Action(
                s(R.string.btSuperSeeding),
                { setSuperSeeding(tasks, bt.superSeedingTarget) },
                if (bt.superSeedingOn) FluxIcons.SquareCheck else FluxIcons.Square,
                checked = bt.superSeedingOn,
                showTrailingCheck = false,
            ),
        )
    }

    private fun piecePickingItems(tasks: List<Task>, picking: PiecePickingMenu): List<FluxMenuItem> {
        fun order(label: String, sequential: Boolean, selected: Boolean) = FluxMenuItem.Action(
            label,
            {
                val change = piecePickingOrderChange(tasks, sequential) ?: return@Action
                setPiecePicking(change.tasks, sequential = change.sequential)
            },
            if (selected) FluxIcons.CircleDot else FluxIcons.Circle,
            checked = selected,
            showTrailingCheck = false,
            radio = true,
        )
        return listOf(
            FluxMenuItem.Header(s(R.string.btPiecePicking)),
            order(s(R.string.btRarestFirst), sequential = false, selected = picking.order == PiecePickingOrder.Rarest),
            order(s(R.string.btSequentialDownload), sequential = true, selected = picking.order == PiecePickingOrder.Sequential),
            FluxMenuItem.Action(
                s(R.string.btFirstLastPiecesFirst),
                { setPiecePicking(tasks, firstLastPiecesFirst = picking.firstLastTarget) },
                if (picking.firstLastOn) FluxIcons.SquareCheck else FluxIcons.Square,
                checked = picking.firstLastOn,
                showTrailingCheck = false,
            ),
        )
    }
}

val LocalTaskActions = staticCompositionLocalOf<TaskActions> { error("TaskActions missing") }

/**
 * BT 选片菜单事实：每个任务都是未完成的 BT（`magnet:` / `torrent-file://`，大小写不敏感）时显示。
 * 不看「文件是否在这台手机上」：正在控制的远端主机同样走 `daemon.task.setPiecePicking`。
 *
 * 下载顺序是单选：全部 `btSequential=false` 为最稀有优先（引擎默认），全部为 true 为按顺序下载，
 * 混选两项都不点亮。点选只对状态不同的任务发 `sequential`。首尾文件块仍是独立复选框：
 * 勾选 = 全部开启，点击目标是勾选态的反值，且只发 `firstLastPiecesFirst`。
 */
enum class PiecePickingOrder {
    Rarest,
    Sequential,
    Mixed,
}

data class PiecePickingMenu(
    val order: PiecePickingOrder,
    val firstLastOn: Boolean,
) {
    val firstLastTarget: Boolean get() = !firstLastOn
}

/**
 * 把下载顺序设为 [sequential] 时要下发的任务。已是该状态的任务跳过；全部已是目标时返回 null（不发 RPC）。
 * [PiecePickingOrderChange.fields] 只有 `sequential`。
 */
data class PiecePickingOrderChange(
    val tasks: List<Task>,
    val sequential: Boolean,
) {
    val fields: Map<String, Boolean>
        get() = mapOf("sequential" to sequential)
}

fun piecePickingOrderChange(tasks: List<Task>, sequential: Boolean): PiecePickingOrderChange? {
    val changed = tasks.filter { it.btSequential != sequential }
    if (changed.isEmpty()) return null
    return PiecePickingOrderChange(changed, sequential)
}

/** BT 任务：`magnet:` / `torrent-file://`（大小写不敏感）。含正在控制的远端 PC / 服务器主机上的任务。 */
fun isBtTask(task: Task): Boolean {
    val url = task.url.lowercase()
    return url.startsWith("magnet:") || url.startsWith("torrent-file://")
}

fun piecePickingMenu(tasks: List<Task>): PiecePickingMenu? {
    if (tasks.isEmpty()) return null
    if (tasks.any { !isBtTask(it) || it.status == TaskStatus.Completed }) return null
    val order = when {
        tasks.all { !it.btSequential } -> PiecePickingOrder.Rarest
        tasks.all { it.btSequential } -> PiecePickingOrder.Sequential
        else -> PiecePickingOrder.Mixed
    }
    return PiecePickingMenu(
        order = order,
        firstLastOn = tasks.all { it.btFirstLastPiecesFirst },
    )
}

/**
 * BT 任务级菜单事实（全部所选任务都是 BT 时非 null）：
 * - [recheck]「强制重新校验」：全部未完成（已完成 / 做种中的任务用「校验」，主机对其返回 `taskStateConflict`）；
 * - [limits]「任务限速…」：仅单选；
 * - 「超级做种」复选：勾选 = 全部 `btSuperSeeding` 为真，点击目标是勾选态的反值（只发给状态不同的任务）。
 */
data class BtTaskMenu(
    val recheck: Boolean,
    val limits: Boolean,
    val superSeedingOn: Boolean,
) {
    val superSeedingTarget: Boolean get() = !superSeedingOn
}

fun btTaskMenu(tasks: List<Task>): BtTaskMenu? {
    if (tasks.isEmpty() || tasks.any { !isBtTask(it) }) return null
    return BtTaskMenu(
        recheck = tasks.none { it.status == TaskStatus.Completed },
        limits = tasks.size == 1,
        superSeedingOn = tasks.all { it.btSuperSeeding },
    )
}

/** 超级做种要下发的任务：已是 [enabled] 的跳过。 */
fun superSeedingChange(tasks: List<Task>, enabled: Boolean): List<Task> = tasks.filter { it.btSuperSeeding != enabled }

/** 限速输入只收数字，最多 15 位（始终在 [BtTaskLimits.MAX_KIB] 内）。 */
internal fun limitDigits(text: String): String = text.filter { it in '0'..'9' }.take(15)
