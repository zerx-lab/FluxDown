package com.fluxdown.app.feature.downloads

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.unit.dp
import com.fluxdown.app.AppContainer
import com.fluxdown.app.R
import com.fluxdown.app.data.Density
import com.fluxdown.app.feature.settings.account.AccountText
import com.fluxdown.app.feature.task.copyText
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.ui.fileCategory
import com.fluxdown.app.ui.tileIcon
import com.fluxdown.core.format.Format
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.model.CloudDevice
import com.fluxdown.core.model.TaskErrorMessage
import com.fluxdown.core.protocol.CloudConnectionDto
import com.fluxdown.core.protocol.CloudPresence
import com.fluxdown.core.protocol.HostCapability
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.RemoteBatch
import com.fluxdown.core.protocol.RemoteBatchOutcome
import com.fluxdown.core.protocol.RemoteCommandAction
import com.fluxdown.core.protocol.RemoteCommandManyResult
import com.fluxdown.core.protocol.RemoteCommandParams
import com.fluxdown.core.protocol.RemoteTaskDto
import com.fluxdown.core.protocol.RemoteTaskRules
import com.fluxdown.core.protocol.RemoteTaskStatus
import com.fluxdown.core.protocol.callJson
import com.fluxdown.core.protocol.callUnit
import com.fluxdown.core.protocol.has
import com.fluxdown.fluxui.data.FlowSegmentUi
import com.fluxdown.fluxui.data.FlowStripState
import com.fluxdown.fluxui.data.MetaTone
import com.fluxdown.fluxui.data.RingKind
import com.fluxdown.fluxui.data.TaskRow
import com.fluxdown.fluxui.data.TaskRowDensity
import com.fluxdown.fluxui.data.TaskRowSelection
import com.fluxdown.fluxui.data.buildTaskMeta
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogButtonStyle
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxMenuItem
import com.fluxdown.fluxui.overlay.FluxSwipeAction
import com.fluxdown.fluxui.overlay.FluxSwipeTone
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.overlay.SwipeReveal
import com.fluxdown.fluxui.material.FlowInGate
import com.fluxdown.fluxui.material.fluxFlowIn
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** 远程任务的目标设备：名称 + 在线状态（null = 云端 presence 不可信，不据此禁用暂停 / 继续）。 */
@Immutable
internal data class RemoteTarget(val name: String, val online: Boolean?) {
    companion object {
        /** 云端名册按 deviceId 索引（同 iOS `DevicesModel.target`）。 */
        fun index(devices: List<CloudDevice>, connectionRaw: String?, localReady: Boolean): Map<String, RemoteTarget> {
            val known = CloudPresence.isKnown(CloudConnectionDto.fromJson(Json.parseOrNull(connectionRaw)), localReady)
            return devices.associate { it.deviceId to RemoteTarget(it.name, if (known) it.isOnline else null) }
        }
    }
}

/**
 * 远程任务命令（`agent.remote.command`）与行内过渡态（同 iOS `DevicesModel.issue` / `isBusy`）：
 * 命令在途，或已被接受但任务的状态 / `updatedAt` 还没回写（最长等 [SETTLE_MS]）时行显示进行中。
 */
@Stable
internal class RemoteCommands(private val container: AppContainer) {
    private class Settling(val status: RemoteTaskStatus, val updatedAt: String)

    private var inFlight by mutableStateOf<Set<String>>(emptySet())
    private var settling by mutableStateOf<Map<String, Settling>>(emptyMap())

    fun isBusy(task: RemoteTaskDto): Boolean {
        if (task.id in inFlight) return true
        val pending = settling[task.id] ?: return false
        return pending.status == task.status && pending.updatedAt == task.updatedAt
    }

    /** 适用性由调用方经 [RemoteTaskRules.canIssue] 保证；失败交给 [onError]。 */
    fun issue(action: RemoteCommandAction, task: RemoteTaskDto, deleteFiles: Boolean, onError: (HostException) -> Unit) {
        val id = task.id
        if (id in inFlight) return
        inFlight = inFlight + id
        settling = settling - id
        val before = Settling(task.status, task.updatedAt)
        val session = container.session
        container.appScope.launch {
            try {
                session.callUnit(HostMethod.agentRemoteCommand, RemoteCommandParams(id, action, deleteFiles = deleteFiles).toJson())
            } catch (e: HostException) {
                inFlight = inFlight - id
                // 旧主机对「云端已无此记录」只回裸 notFound 且不移除该行：重新同步一次（失败忽略）。
                if (e.reason == null && e.code == HostErrorCode.NotFound) resync(session)
                onError(e)
                return@launch
            }
            inFlight = inFlight - id
            settling = settling + (id to before)
            delay(SETTLE_MS)
            if (settling[id] === before) settling = settling - id
        }
    }

    /**
     * 批量命令：对 [tasks] 逐一应用 [action]（适用性由调用方经 [RemoteTaskRules.canIssue] 过滤；在途的任务跳过）。
     * 主机声明 `agent.remoteBatch` 时走 `agent.remote.commandMany`（按 1000 分块），否则逐条回退（并发 4）。
     * 成功的任务进入与单条命令相同的过渡态；[onDone] 收到完整结果（含每个失败的原因）。
     */
    fun issueMany(
        action: RemoteCommandAction,
        tasks: List<RemoteTaskDto>,
        deleteFiles: Boolean,
        onDone: (RemoteBatchOutcome) -> Unit,
    ) {
        val todo = tasks.filter { it.id !in inFlight }.distinctBy { it.id }
        if (todo.isEmpty()) return
        val ids = todo.map { it.id }
        val before = todo.associate { it.id to Settling(it.status, it.updatedAt) }
        inFlight = inFlight + ids
        settling = settling - ids.toSet()
        val session = container.session
        val batch = container.store.state.value.has(HostCapability.agentRemoteBatch)
        container.appScope.launch {
            val outcome = RemoteBatch.run(
                ids, action, deleteFiles, batch,
                many = { p -> RemoteCommandManyResult.fromJson(session.callJson(HostMethod.agentRemoteCommandMany, p.toJson())) },
                single = { p -> session.callUnit(HostMethod.agentRemoteCommand, p.toJson()) },
            )
            inFlight = inFlight - ids.toSet()
            val settled = outcome.succeeded.mapNotNull { id -> before[id]?.let { id to it } }.toMap()
            settling = settling + settled
            if (outcome.legacyNotFound || outcome.failed.any { it.isLegacyNotFound }) resync(session)
            onDone(outcome)
            delay(SETTLE_MS)
            settled.forEach { (id, pending) -> if (settling[id] === pending) settling = settling - id }
        }
    }

    /** 请求一次 `agent.remote.list` 重新同步分区（fire-and-forget，失败忽略）。 */
    private fun resync(session: HostSession) {
        container.appScope.launch {
            try {
                session.callUnit(HostMethod.agentRemoteList)
            } catch (_: HostException) {
            }
        }
    }

    private companion object {
        /** 命令落地后等待状态事件回写的最长时间。 */
        const val SETTLE_MS = 6_000L
    }
}

/**
 * 其他设备上执行的远程任务行（同 iOS `RemoteTaskRow` / PC 下载页远程行）：与本地行同一列表、同一分区样式；
 * `→ 目标设备` + 状态 + 进度 / 字节 / 速度；环 = 暂停 / 继续（命令在途旋转；没有主操作时打开动作菜单），
 * 滑动取消 / 删除，长按含全部可用动作。控制矩阵只走 [RemoteTaskRules.canIssue]；未知状态没有任何控制。
 * 参与多选（点击 / 长按切换勾选），没有详情页。
 */
@Composable
internal fun RemoteTaskRow(entry: RemoteRowEntry, style: RowStyle, flowIndex: Int, gate: FlowInGate, a11y: Boolean) {
    val item = entry.item
    val task = item.task
    val view = LocalDownloadsView.current
    val nav = LocalNavigator.current
    val overlays = LocalFluxOverlays.current
    val haptics = FluxTheme.haptics
    val colors = FluxTheme.colors
    val context = LocalContext.current
    val margin = FluxTheme.space.screenMargin
    val compact = style.density == Density.Compact

    val target = view.remoteTargets[task.toDevice]
    val targetName = target?.name?.ifBlank { null } ?: task.toDevice
    val online = target?.online
    val busy = view.remoteCommands.isBusy(task)
    val readOnly = view.readOnly
    val latest by rememberUpdatedState(task)

    val id = item.id
    val selecting = nav.selecting
    val selected by remember(id, nav) { derivedStateOf { id in nav.selection } }
    val selection = when {
        !selecting -> TaskRowSelection.None
        selected -> TaskRowSelection.Selected
        else -> TaskRowSelection.Unselected
    }

    val s = RemoteRowStrings(
        pause = str(R.string.pause),
        resume = str(R.string.resume),
        cancel = str(R.string.cancel),
        delete = str(R.string.delete),
        copyLink = str(R.string.mobileSwipeCopyLink),
        more = str(R.string.moreActions),
        select = str(R.string.mobileMenuSelect),
        offline = str(R.string.errReasonTargetDeviceOffline),
        urlCopied = str(R.string.urlCopied),
        deleteTask = str(R.string.deleteTask),
        deleteTaskAndFile = str(R.string.deleteTaskAndFile),
        statusError = str(R.string.statusError),
        errorPostOnlyEndpoint = str(R.string.taskErrorPostOnlyEndpoint),
        errorPostOnlyResume = str(R.string.taskErrorPostOnlyResume),
        errorUnexpectedHtml = str(R.string.taskErrorUnexpectedHtml),
        errorHtmlChanged = str(R.string.taskErrorHtmlChanged),
        errorMagnetMetadataTimeout = str(R.string.taskErrorMagnetMetadataTimeout),
        statusCompleted = str(R.string.statusCompleted),
        statusDownloading = str(R.string.statusDownloading),
        statusPending = str(R.string.statusPending),
        statusPaused = str(R.string.statusPaused),
        statusCanceled = str(R.string.statusCanceled),
        statusUnknown = str(R.string.mobileRemoteStatusUnknown),
    )

    fun allows(action: RemoteCommandAction): Boolean =
        !readOnly && !busy && RemoteTaskRules.canIssue(action, latest, online)

    fun issue(action: RemoteCommandAction, deleteFiles: Boolean = false) {
        haptics.tick()
        view.remoteCommands.issue(action, latest, deleteFiles) { e ->
            haptics.reject()
            overlays.toast(AccountText.remoteError(context, e), FluxToastKind.Error)
        }
    }

    fun copyLink() {
        context.copyText(item.name, latest.url)
        overlays.toast(s.urlCopied, FluxToastKind.Success, FluxIcons.Copy)
    }

    fun confirmDelete() {
        overlays.showDialog(
            FluxDialogSpec(
                title = s.deleteTask,
                message = item.name,
                icon = FluxIcons.Trash2,
                buttons = buildList {
                    add(FluxDialogButton(s.cancel))
                    // 目标离线时无法让它删除文件，只能删记录：不提供「同时删除文件」
                    if (online != false) {
                        add(
                            FluxDialogButton(s.deleteTaskAndFile, FluxDialogButtonStyle.Destructive) {
                                issue(RemoteCommandAction.Delete, deleteFiles = true)
                            },
                        )
                    }
                    add(
                        FluxDialogButton(s.deleteTask, FluxDialogButtonStyle.Primary) {
                            issue(RemoteCommandAction.Delete)
                        },
                    )
                },
                stacked = true,
            ),
        )
    }

    val primary = RemoteTaskRules.primaryAction(task)
    val primaryLabel = when (primary) {
        RemoteCommandAction.Pause -> s.pause
        RemoteCommandAction.Resume -> s.resume
        else -> s.more
    }

    fun menuItems(): List<FluxMenuItem> = buildList {
        if (primary != null && allows(primary)) {
            add(
                FluxMenuItem.Action(
                    primaryLabel,
                    { issue(primary) },
                    icon = if (primary == RemoteCommandAction.Pause) FluxIcons.Pause else FluxIcons.Play,
                ),
            )
        }
        if (allows(RemoteCommandAction.Cancel)) add(FluxMenuItem.Action(s.cancel, { issue(RemoteCommandAction.Cancel) }, icon = FluxIcons.X))
        if (latest.url.isNotEmpty()) add(FluxMenuItem.Action(s.copyLink, ::copyLink, icon = FluxIcons.Copy))
        add(FluxMenuItem.Action(s.select, { nav.enterSelection(id) }, icon = FluxIcons.CheckCheck))
        if (allows(RemoteCommandAction.Delete)) {
            add(FluxMenuItem.Divider)
            add(FluxMenuItem.Action(s.delete, ::confirmDelete, icon = FluxIcons.Trash2, destructive = true))
        }
    }

    val coords = remember { arrayOfNulls<LayoutCoordinates>(1) }
    fun openMenu() {
        val c = coords[0] ?: return
        if (!c.isAttached) return
        val items = menuItems()
        if (items.isNotEmpty()) overlays.showMenu(c.boundsInRoot(), items)
    }

    // 进行中 / 已暂停的任务因目标离线而不能暂停 / 继续
    val offlineBlocksPrimary = primary != null && !RemoteTaskRules.canIssue(primary, task, online)
    val meta = remember(task, targetName, offlineBlocksPrimary, colors, s) {
        colors.buildTaskMeta {
            part("→ $targetName", MetaTone.Ink)
            val status = task.status
            when (status) {
                RemoteTaskStatus.Failed -> part(firstLine(s.taskError(task.error)) ?: s.statusError, MetaTone.Coral)
                RemoteTaskStatus.Completed -> part(s.statusCompleted, MetaTone.Mint)
                RemoteTaskStatus.Downloading -> Format.speed(task.speed)?.let { part(it.toString(), MetaTone.Accent) }
                    ?: part(s.statusDownloading)
                RemoteTaskStatus.Pending, RemoteTaskStatus.Accepted -> part(s.statusPending)
                RemoteTaskStatus.Paused -> part(s.statusPaused)
                RemoteTaskStatus.Canceled -> part(s.statusCanceled)
                is RemoteTaskStatus.Unknown -> part(s.statusUnknown)
            }
            if (status !is RemoteTaskStatus.Unknown && status != RemoteTaskStatus.Failed) {
                part(Format.percent(task.progress.toFloat().coerceIn(0f, 1f)))
                val total = task.totalBytes
                part(
                    if (total != null && total > 0) {
                        "${Format.bytes(task.downloadedBytes)}/${Format.bytes(total)}"
                    } else {
                        Format.bytes(task.downloadedBytes).toString()
                    },
                )
            }
            if (offlineBlocksPrimary) part(s.offline, MetaTone.Amber)
        }
    }

    val progress = task.progress.toFloat().coerceIn(0f, 1f)
    val flowState = when (task.status) {
        RemoteTaskStatus.Downloading -> FlowStripState.Downloading
        RemoteTaskStatus.Pending, RemoteTaskStatus.Accepted -> FlowStripState.Pending
        RemoteTaskStatus.Paused, RemoteTaskStatus.Canceled -> FlowStripState.Paused
        RemoteTaskStatus.Failed -> FlowStripState.Failed
        RemoteTaskStatus.Completed, is RemoteTaskStatus.Unknown -> FlowStripState.Completed
    }
    val showFlow = task.status != RemoteTaskStatus.Completed && task.status !is RemoteTaskStatus.Unknown
    val ringKind = when {
        busy -> RingKind.Spinning
        primary == RemoteCommandAction.Pause -> RingKind.Pause
        primary == RemoteCommandAction.Resume -> RingKind.Play
        task.status == RemoteTaskStatus.Completed -> RingKind.Seeding
        else -> RingKind.More
    }

    fun toggleSelected() {
        nav.toggleSelected(id)
        haptics.tick()
    }

    fun onRing() {
        when {
            selecting -> toggleSelected()
            busy || readOnly -> Unit
            primary != null && allows(primary) -> issue(primary)
            primary != null -> {
                haptics.reject()
                overlays.toast(s.offline, FluxToastKind.Warn, FluxIcons.WifiOff)
            }
            else -> openMenu()
        }
    }

    val startActions = if (primary != null && allows(primary)) {
        listOf(
            FluxSwipeAction(primaryLabel, if (primary == RemoteCommandAction.Pause) FluxIcons.Pause else FluxIcons.Play, FluxSwipeTone.Accent) {
                issue(primary)
            },
        )
    } else {
        emptyList()
    }
    val endActions = buildList {
        if (allows(RemoteCommandAction.Cancel)) add(FluxSwipeAction(s.cancel, FluxIcons.X, FluxSwipeTone.Warn) { issue(RemoteCommandAction.Cancel) })
        if (allows(RemoteCommandAction.Delete)) {
            add(FluxSwipeAction(s.delete, FluxIcons.Trash2, FluxSwipeTone.Danger, showLabel = false, onClick = ::confirmDelete))
        }
    }
    val customActions = if (a11y) {
        menuItems().filterIsInstance<FluxMenuItem.Action>().map { a -> CustomAccessibilityAction(a.label) { a.onClick(); true } }
    } else {
        emptyList()
    }

    val history = entry.zone == RowZone.History
    val inset = if (compact) 60.dp else 68.dp
    val noPane = remember { mutableStateOf(false) }
    Box(
        Modifier
            .fillMaxWidth()
            .then(if (flowIndex in 0..11) Modifier.fluxFlowIn(flowIndex, gate, item.id) else Modifier)
            .then(if (history) Modifier else Modifier.padding(horizontal = margin)),
    ) {
        Box(
            Modifier
                .fillMaxWidth()
                .then(if (history) Modifier else Modifier.cardSegment(entry.first, entry.last))
                .rowDecor(entry.zone, entry.first, entry.last, inset, noPane)
                .onGloballyPositioned { coords[0] = it },
        ) {
            SwipeReveal(startActions = startActions, endActions = endActions, enabled = !selecting && !readOnly) {
                TaskRow(
                    fileName = item.name,
                    meta = meta,
                    tileIcon = item.category.tileIcon(bt = task.url.startsWith("magnet:")),
                    categoryColor = colors.category(item.category.fileCategory()),
                    ringKind = ringKind,
                    ringProgress = if (showFlow || busy) progress else null,
                    ringContentDescription = primaryLabel,
                    onClick = { if (selecting) toggleSelected() else openMenu() },
                    onLongClick = { if (selecting) nav.toggleSelected(id) else openMenu() },
                    onRingClick = ::onRing,
                    onTileClick = {
                        if (selecting) {
                            toggleSelected()
                        } else {
                            haptics.longPress()
                            nav.enterSelection(id)
                        }
                    },
                    flowSegments = if (showFlow) listOf(FlowSegmentUi(1f, progress, task.status == RemoteTaskStatus.Downloading)) else null,
                    flowState = flowState,
                    density = if (compact) TaskRowDensity.Compact else TaskRowDensity.Comfortable,
                    selection = selection,
                    horizontalPadding = if (history) 20.dp else 18.dp,
                    onClickLabel = s.more,
                    customActions = customActions,
                )
            }
        }
    }
}

@Immutable
private data class RemoteRowStrings(
    val pause: String,
    val resume: String,
    val cancel: String,
    val delete: String,
    val copyLink: String,
    val more: String,
    val select: String,
    val offline: String,
    val urlCopied: String,
    val deleteTask: String,
    val deleteTaskAndFile: String,
    val statusError: String,
    val statusCompleted: String,
    val statusDownloading: String,
    val statusPending: String,
    val statusPaused: String,
    val statusCanceled: String,
    val statusUnknown: String,
    val errorPostOnlyEndpoint: String,
    val errorPostOnlyResume: String,
    val errorUnexpectedHtml: String,
    val errorHtmlChanged: String,
    val errorMagnetMetadataTimeout: String,
) {
    fun taskError(raw: String?): String? = raw?.let {
        when (TaskErrorMessage.match(it)) {
            TaskErrorMessage.PostOnlyEndpoint -> errorPostOnlyEndpoint
            TaskErrorMessage.PostOnlyResume -> errorPostOnlyResume
            TaskErrorMessage.UnexpectedHtml -> errorUnexpectedHtml
            TaskErrorMessage.HtmlChanged -> errorHtmlChanged
            TaskErrorMessage.MagnetMetadataTimeout -> errorMagnetMetadataTimeout
            null -> it
        }
    }
}

private fun firstLine(s: String?): String? = s?.lineSequence()?.map { it.trim() }?.firstOrNull { it.isNotEmpty() }
