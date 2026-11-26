package com.fluxdown.app.service

import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.toArgb
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.FileProvider
import com.fluxdown.app.AppContainer
import com.fluxdown.app.HomeActivity
import com.fluxdown.app.R
import com.fluxdown.app.data.DeviceSettings
import com.fluxdown.app.i18n.localizedTaskError
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.AppNavigator
import com.fluxdown.app.nav.AppTab
import com.fluxdown.app.nav.Route
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.model.SelectionKind
import com.fluxdown.core.model.SelectionRequest
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.protocol.preferences
import com.fluxdown.core.store.Connection
import com.fluxdown.core.store.HostState
import com.fluxdown.core.store.NotificationBatch
import com.fluxdown.core.store.NotificationEvent
import com.fluxdown.core.store.NotificationItem
import com.fluxdown.core.store.NotificationTracker
import com.fluxdown.fluxui.theme.FluxColors
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import java.io.File

/** 系统通知授权状态（Android 无「临时授权」）。 */
enum class NotifyAuthorization { Authorized, NotDetermined, Denied }

/** 所有系统通知的强调色（小图标底、进度条、动作文字）：品牌蓝，与启动图标一致。 */
internal val NotificationTint: Int = FluxColors.Brand.flux.toArgb()

/**
 * 点按通知回到应用的意图（[HomeActivity] 经 [handle] 消费）：切到通知所属主机 → 下载页 → 任务详情。
 */
object NotificationIntents {
    const val ACTION_OPEN = "com.fluxdown.app.action.OPEN_FROM_NOTIFICATION"
    const val EXTRA_TASK = "fluxdown.task"
    const val EXTRA_HOST = "fluxdown.host"

    /** 打开 [hostId] 的下载页；[taskId] 非空时再推入该任务详情。[identity] 区分共用 [code] 的通知。 */
    fun pending(context: Context, hostId: String, taskId: String, code: Int, identity: Uri? = null): PendingIntent {
        val intent = Intent(context, HomeActivity::class.java)
            .setAction(ACTION_OPEN)
            .setData(identity)
            .putExtra(EXTRA_HOST, hostId)
            .putExtra(EXTRA_TASK, taskId)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        return PendingIntent.getActivity(context, code, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    /** @return true = 这是通知意图（已处理）。 */
    fun handle(container: AppContainer, nav: AppNavigator, intent: Intent?): Boolean {
        if (intent?.action != ACTION_OPEN) return false
        val hostId = intent.getStringExtra(EXTRA_HOST).orEmpty()
        container.hostSelection.prioritizeNotification(hostId)
        val taskId = intent.getStringExtra(EXTRA_TASK).orEmpty()
        container.appScope.launch {
            if (!ensureHost(container, hostId)) return@launch
            nav.selectTab(AppTab.Downloads)
            if (taskId.isNotEmpty()) nav.push(Route.TaskDetail(taskId))
        }
        return true
    }

    /**
     * 通知来自另一台主机：先切换过去（主机已被删除则放弃），并最多等 5 秒让新会话的首个快照到达。
     * 冷启动时已保存的远端主机列表来自 DataStore，首次发射前只有本机：先等目标出现，超时再放弃。
     */
    private suspend fun ensureHost(container: AppContainer, hostId: String): Boolean {
        if (hostId.isEmpty() || container.host.value.id == hostId) return true
        val ref = withTimeoutOrNull(HOSTS_WAIT_MS) {
            container.hosts.first { list -> list.any { it.id == hostId } }.first { it.id == hostId }
        } ?: return false
        if (container.hostSelection.switchHost(ref, remember = false).isFailure) return false
        withTimeoutOrNull(LIVE_WAIT_MS) { container.store.state.first { it.connection == Connection.Live } }
        return true
    }

    private const val HOSTS_WAIT_MS = 3_000L
    private const val LIVE_WAIT_MS = 5_000L
}

/**
 * 应用在前台时的完成提示（代替系统通知，由壳层以 toast 呈现）。
 * [count] == 1 时 [taskId] / [fileName] 为该任务；合批多条时二者为空。
 */
data class InAppCompletion(val hostId: String, val taskId: String, val fileName: String, val count: Int)

/**
 * 本机通知服务：观察任务状态，在任务完成 / 失败 / 引擎请求选择时发系统通知（对应 iOS `NotificationService`）。
 *
 * 规则（逻辑在 `:core` 的 [NotificationTracker] / [NotificationBatch]，有单测）：
 * - 观察**当前主机**与**本机**：正在查看远端主机时，本机引擎的下载仍在跑（前台服务的进度通知也在显示它们），
 *   它们的完成 / 失败 / 选择请求同样通知；两路各自一个跟踪器，互不干扰；
 * - 每个来源的首个已连上快照只播种；窗口内同类事件合批，≥ 3 条合并为一条汇总；
 * - 偏好实时读取：`download.notify_on_complete`（所属主机的 agent 偏好，默认开）、[DeviceSettings.notifyOnFailure] /
 *   [DeviceSettings.notifyActions]（设备本地）；系统通知被关闭 / 未授权时一律不发；
 * - 前台时完成不发系统通知，改经 [completions] 交给壳层弹应用内提示；失败照常发；
 * - 选择请求：前台时壳层只为当前主机自动弹出请求 Sheet，因此只有「当前主机且在前台」不发通知。
 *
 * 授权**懒申请**：这里从不弹系统授权框，由通知设置页 / 首次打开通知开关时申请。
 * 观察在应用作用域内运行，由 [DownloadService]（本机下载期间，Activity 已被划掉也不中断）与
 * [GeneralSettingsEffects][com.fluxdown.app.feature.settings.general.GeneralSettingsEffects] 拉起，幂等。
 */
object DownloadNotifier {
    private const val CHANNEL_COMPLETED = "fluxdown_completed"
    private const val CHANNEL_FAILED = "fluxdown_failed"
    private const val CHANNEL_SELECTION = "fluxdown_selection"
    private const val NOTIFICATION_ID = 2001

    /** 系统通知授权状态（Compose 状态：设置页 / 首页读数据此重组）。 */
    var authorization by mutableStateOf(NotifyAuthorization.NotDetermined)
        private set

    /** 应用界面是否在前台（STARTED）。 */
    @Volatile
    var foreground: Boolean = false

    private val _completions = MutableSharedFlow<InAppCompletion>(
        extraBufferCapacity = 8,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    /** 前台时的完成提示；无订阅者时丢弃（提示不是状态，错过即无意义）。 */
    val completions: SharedFlow<InAppCompletion> = _completions.asSharedFlow()

    /** 单线程：两路观察、合批与发送都串行在这里。 */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default.limitedParallelism(1))
    private val tracker = NotificationTracker()
    private val localTracker = NotificationTracker()
    private val batches = LinkedHashMap<String, ArrayList<NotificationEvent>>()
    private var flushJob: Job? = null
    private var attached = false

    /** 接入应用容器（幂等）。 */
    @Synchronized
    fun attach(container: AppContainer, context: Context) {
        val app = context.applicationContext
        ensureChannels(app)
        refreshAuthorization(app)
        if (attached) return
        attached = true
        scope.launch {
            combine(container.store.state, container.host) { state, host -> state to host.id }
                .collect { (state, hostId) -> process(container, app, tracker, state, hostId) }
        }
        scope.launch {
            // 当前就是本机时由上一路覆盖；切到远端时这一路接管本机任务
            combine(container.localState, container.host) { state, host -> if (host is HostRef.Local) null else state }
                .collect { state ->
                    if (state == null) {
                        localTracker.reset()
                    } else {
                        process(container, app, localTracker, state, HostRef.Local.ID)
                    }
                }
        }
    }

    /** 重新读取系统授权（回到前台 / 授权结果返回后调用）。 */
    fun refreshAuthorization(context: Context) {
        authorization = currentAuthorization(context.applicationContext)
    }

    private fun currentAuthorization(context: Context): NotifyAuthorization = when {
        NotificationManagerCompat.from(context).areNotificationsEnabled() -> NotifyAuthorization.Authorized
        DownloadServiceController.shouldRequestNotificationPermission(context) -> NotifyAuthorization.NotDetermined
        else -> NotifyAuthorization.Denied
    }

    /** 发送一条测试通知（通知设置页）；返回是否已提交给系统（未授权 / 被关闭为 false）。 */
    fun sendTest(context: Context): Boolean {
        val app = context.applicationContext
        ensureChannels(app)
        refreshAuthorization(app)
        if (authorization != NotifyAuthorization.Authorized) return false
        val notification = NotificationCompat.Builder(app, CHANNEL_COMPLETED)
            .setSmallIcon(R.drawable.ic_stat_download)
            .setColor(NotificationTint)
            .setContentTitle(app.str(R.string.doctorTestNotificationTitle))
            .setContentText(app.str(R.string.doctorTestNotificationBody))
            .setContentIntent(NotificationIntents.pending(app, hostId = "", taskId = "", code = "test".hashCode()))
            .setAutoCancel(true)
            .build()
        notify(app, "fluxdown.test.${System.currentTimeMillis()}", notification)
        return true
    }

    // ── 观察 ────────────────────────────────────────────────────────────────

    private fun process(container: AppContainer, context: Context, tracker: NotificationTracker, state: HostState, hostId: String) {
        val delta = tracker.ingest(
            hostId = hostId,
            // 任务表载入中视同未就绪：丢弃基线，等完整表到达再静默播种（否则历史完成项会被当作新事件）。
            isLive = state.connection == Connection.Live && !state.tasksLoading,
            tasks = state.tasks,
            selections = state.selections,
            now = System.currentTimeMillis() / 1000,
        )
        if (delta.isEmpty) return

        for (id in delta.resolvedSelections) cancel(context, selectionTag(hostId, id))

        if (delta.events.isNotEmpty()) {
            val wantsComplete = state.preferences.bool("download.notify_on_complete", true)
            val wantsFailure = DeviceSettings.of(context).notifyOnFailure
            enqueue(container, context, delta.events.filter { if (it.kind == NotificationEvent.Kind.Completed) wantsComplete else wantsFailure }, hostId)
        }

        val sheetInApp = foreground && hostId == container.host.value.id
        if (delta.newSelections.isNotEmpty() && !sheetInApp) {
            for (request in state.selections) {
                if (request.requestId in delta.newSelections) postSelection(context, request, state.task(request.taskId), hostId)
            }
        }
    }

    // ── 去抖与发送 ──────────────────────────────────────────────────────────

    private fun enqueue(container: AppContainer, context: Context, events: List<NotificationEvent>, hostId: String) {
        if (events.isEmpty()) return
        batches.getOrPut(hostId) { ArrayList() } += events
        if (flushJob != null) return
        flushJob = scope.launch {
            delay(NotificationBatch.WINDOW_MS)
            flush(container, context)
        }
    }

    private fun flush(container: AppContainer, context: Context) {
        flushJob = null
        val pending = batches.entries.map { it.key to it.value.toList() }
        batches.clear()
        val current = container.host.value.id
        val withActions = DeviceSettings.of(context).notifyActions
        for ((hostId, events) in pending) {
            // 窗口内切走了主机：只保留仍在观察的来源（当前主机 + 本机）
            if (hostId != current && hostId != HostRef.Local.ID) continue
            val state = if (hostId == current) container.store.state.value else container.localState.value
            if (foreground) announceInApp(hostId, events)
            for (item in NotificationBatch.plan(events)) {
                when (item) {
                    is NotificationItem.Single -> postSingle(context, item.event, hostId, state, withActions)
                    is NotificationItem.Summary -> postSummary(context, item, hostId)
                }
            }
        }
    }

    /** 前台：完成改为应用内提示（一条批次一次，多条合并为计数）。 */
    private fun announceInApp(hostId: String, events: List<NotificationEvent>) {
        val done = events.filter { it.kind == NotificationEvent.Kind.Completed }
        val notice = when (done.size) {
            0 -> return
            1 -> InAppCompletion(hostId, done[0].taskId, done[0].fileName, 1)
            else -> InAppCompletion(hostId, taskId = "", fileName = "", count = done.size)
        }
        _completions.tryEmit(notice)
    }

    private fun postSingle(context: Context, event: NotificationEvent, hostId: String, state: HostState, withActions: Boolean) {
        val code = "${event.kind}.$hostId.${event.taskId}".hashCode()
        val contentIntent = NotificationIntents.pending(context, hostId, event.taskId, code)
        val notification = when (event.kind) {
            NotificationEvent.Kind.Completed -> {
                // 前台时已由应用内提示告知（announceInApp），不重复打扰
                if (foreground) return
                val builder = NotificationCompat.Builder(context, CHANNEL_COMPLETED)
                    .setSmallIcon(R.drawable.ic_stat_download)
                    .setColor(NotificationTint)
                    .setContentTitle(context.str(R.string.downloadCompleted))
                    .setContentText(event.fileName)
                    .setContentIntent(contentIntent)
                    .setAutoCancel(true)
                    .setCategory(NotificationCompat.CATEGORY_STATUS)
                if (withActions) addFileActions(builder, context, hostId, state, event.taskId, code)
                builder.build()
            }
            NotificationEvent.Kind.Failed -> {
                val reason = context.localizedTaskError(event.errorMessage.trim())
                val body = if (reason.isEmpty()) event.fileName else "${event.fileName}\n$reason"
                NotificationCompat.Builder(context, CHANNEL_FAILED)
                    .setSmallIcon(R.drawable.ic_stat_download)
                    .setColor(NotificationTint)
                    .setContentTitle(context.str(R.string.subtitleError))
                    .setContentText(event.fileName)
                    .setStyle(NotificationCompat.BigTextStyle().bigText(body))
                    .setContentIntent(contentIntent)
                    .setAutoCancel(true)
                    .setPriority(NotificationCompat.PRIORITY_HIGH)
                    .setCategory(NotificationCompat.CATEGORY_ERROR)
                    .build()
            }
        }
        val prefix = if (event.kind == NotificationEvent.Kind.Completed) "done" else "fail"
        notify(context, "$prefix.$hostId.${event.taskId}", notification)
    }

    private fun postSummary(context: Context, item: NotificationItem.Summary, hostId: String) {
        val completed = item.kind == NotificationEvent.Kind.Completed
        if (completed && foreground) return
        val tail = if (item.count > item.names.size) "…" else ""
        val body = item.names.joinToString(", ") + tail
        val title = context.str(if (completed) R.string.mobileNotifCompletedSummary else R.string.mobileNotifFailedSummary, "n" to item.count)
        val stamp = System.currentTimeMillis()
        val builder = NotificationCompat.Builder(context, if (completed) CHANNEL_COMPLETED else CHANNEL_FAILED)
            .setSmallIcon(R.drawable.ic_stat_download)
            .setColor(NotificationTint)
            .setContentTitle(title)
            .setContentText(body)
            .setContentIntent(NotificationIntents.pending(context, hostId, taskId = "", code = "summary.${item.kind}.$hostId".hashCode()))
            .setAutoCancel(true)
        if (!completed) builder.setPriority(NotificationCompat.PRIORITY_HIGH)
        notify(context, "${if (completed) "done" else "fail"}.$hostId.summary.$stamp", builder.build())
    }

    private fun postSelection(context: Context, request: SelectionRequest, task: Task?, hostId: String) {
        val conflict = request.kind as? SelectionKind.FileExists
        val name = conflict?.fileName ?: task?.fileName.orEmpty()
        val (title, body) = when {
            conflict != null -> context.str(R.string.fileConflictTitle) to context.str(R.string.mobileNotifFileConflictBody, "name" to name)
            name.isEmpty() -> context.str(R.string.mobileNotifSelectionTitle) to context.str(R.string.mobileNotifSelectionBodyGeneric)
            else -> context.str(R.string.mobileNotifSelectionTitle) to context.str(R.string.mobileNotifSelectionBody, "name" to name)
        }
        val notification = NotificationCompat.Builder(context, CHANNEL_SELECTION)
            .setSmallIcon(R.drawable.ic_stat_download)
            .setColor(NotificationTint)
            .setContentTitle(title)
            .setContentText(body)
            .setContentIntent(NotificationIntents.pending(context, hostId, taskId = "", code = selectionTag(hostId, request.requestId).hashCode()))
            .setAutoCancel(true)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_REMINDER)
            .build()
        notify(context, selectionTag(hostId, request.requestId), notification)
    }

    private fun selectionTag(hostId: String, requestId: String) = "sel.$hostId.$requestId"

    /**
     * 完成通知的「打开」「分享」：任务属于本机主机且是普通文件时才提供（目录 / 远端文件无法打开或分享）；
     * 文件不在 FileProvider 声明的根目录内时同样不提供。
     */
    private fun addFileActions(builder: NotificationCompat.Builder, context: Context, hostId: String, state: HostState, taskId: String, code: Int) {
        if (hostId != HostRef.Local.ID) return
        val task = state.task(taskId) ?: return
        if (task.status != TaskStatus.Completed || task.fileName.isEmpty()) return
        val file = File(task.saveDir, task.fileName)
        if (!file.isFile) return
        val uri: Uri = try {
            FileProvider.getUriForFile(context, "${context.packageName}.files", file)
        } catch (e: IllegalArgumentException) {
            return
        }
        val mime = com.fluxdown.app.actions.mimeForFile(file.name)
        fun viewIntent(type: String) = Intent(Intent.ACTION_VIEW).setDataAndType(uri, type)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        // PendingIntent 无法事后重试：精确类型无人处理时退回 */*（保持以前至少能弹选择器）
        val view = viewIntent(mime).takeIf { it.resolveActivity(context.packageManager) != null } ?: viewIntent("*/*")
        val send = Intent(Intent.ACTION_SEND).setType(mime).putExtra(Intent.EXTRA_STREAM, uri)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        val chooser = Intent.createChooser(send, task.fileName).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        builder.addAction(0, context.str(R.string.mobileNotifActionOpen), PendingIntent.getActivity(context, code + 1, view, flags))
        builder.addAction(0, context.str(R.string.mobileNotifActionShare), PendingIntent.getActivity(context, code + 2, chooser, flags))
    }

    // ── 底层 ────────────────────────────────────────────────────────────────

    /** 系统通知被关闭 / 未授权时不发（[NotificationManagerCompat.areNotificationsEnabled] 已含 Android 13+ 运行时权限）。 */
    @SuppressLint("MissingPermission")
    private fun notify(context: Context, tag: String, notification: android.app.Notification) {
        val manager = NotificationManagerCompat.from(context)
        if (!manager.areNotificationsEnabled()) return
        manager.notify(tag, NOTIFICATION_ID, notification)
    }

    private fun cancel(context: Context, tag: String) {
        NotificationManagerCompat.from(context).cancel(tag, NOTIFICATION_ID)
    }

    private fun ensureChannels(context: Context) {
        val manager = NotificationManagerCompat.from(context)
        fun channel(id: String, importance: Int, name: Int) = NotificationChannelCompat.Builder(id, importance)
            .setName(context.str(name))
            .build()
        manager.createNotificationChannel(channel(CHANNEL_COMPLETED, NotificationManagerCompat.IMPORTANCE_DEFAULT, R.string.mobileNotifChannelCompleted))
        manager.createNotificationChannel(channel(CHANNEL_FAILED, NotificationManagerCompat.IMPORTANCE_HIGH, R.string.mobileNotifChannelFailed))
        manager.createNotificationChannel(channel(CHANNEL_SELECTION, NotificationManagerCompat.IMPORTANCE_HIGH, R.string.mobileNotifChannelSelection))
    }
}
