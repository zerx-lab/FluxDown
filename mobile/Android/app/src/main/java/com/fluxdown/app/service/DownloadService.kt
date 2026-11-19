package com.fluxdown.app.service

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import com.fluxdown.app.AppContainer
import com.fluxdown.app.FluxApplication
import com.fluxdown.app.R
import com.fluxdown.app.diag.AppDiagnostics
import com.fluxdown.app.i18n.str
import com.fluxdown.core.format.Format
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.store.Transfer
import com.fluxdown.core.store.TransferSummary
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

private const val TAG = "FluxDownService"
private const val CHANNEL_ID = "fluxdown_downloads"
private const val NOTIFICATION_ID = 1001
private const val TASK_PROGRESS_CHANNEL = "fluxdown_task_progress"
private const val TASK_PROGRESS_ID = 1002
private const val ACTION_DISMISS_TASK_PROGRESS = "com.fluxdown.app.action.DISMISS_TASK_PROGRESS"

/** 通知刷新最小间隔（≤ 1 Hz）。 */
private const val THROTTLE_MS = 1_000L

/** 仅在已有刷新事件上低频对账；系统丢弃的通知不能跟随进度反复重发。 */
private const val TASK_NOTICE_CHECK_MS = 5_000L

/** 空闲后保留前台的宽限（任务切换 / 队列接力时 active 会瞬间归零），随后自行停止。 */
private const val IDLE_GRACE_MS = 3_000L

/** 本机会话 Stale（进程内重连）时按断连前的活动量保留前台的上限；超时仍未恢复视为空闲，避免通知僵死。 */
private const val STALE_KEEP_MS = 60_000L

/** 下载期间持有的 CPU 唤醒锁超时；每次刷新通知续期，进程异常时也不会无限持有。 */
private const val WAKE_LOCK_TIMEOUT_MS = 10L * 60L * 1000L

/**
 * 本机下载前台服务（`dataSync`）：本机引擎有活跃 / 排队 / 待重试任务时存在，让进程在后台继续下载——
 * 与当前选中的主机无关（远端主机在前台时，进程内的本机下载照样在跑）。
 * 通知常驻、静默，展示本机下载进度（单任务进度条 / 多任务聚合进度 + 展开明细，见 [buildNotification]），
 * 点按回到应用；本机空闲后自行停止。
 *
 * 事件驱动：观察 [AppContainer.localActivity] 和系统通知事件，无周期轮询；通知刷新按 [THROTTLE_MS] 节流，
 * 空闲宽限由新的活动量到来而取消。启动入口见 [DownloadServiceController.start]；
 * 停止由服务自己决定，因此 Activity 被销毁后也不会残留前台通知。
 */
class DownloadService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var watcher: Job? = null
    private var notificationRefresh: Job? = null
    private var lastShownAtMs = 0L
    private var lastTaskNoticeCheckAtMs = -TASK_NOTICE_CHECK_MS
    private val notificationManager by lazy { checkNotNull(getSystemService(NotificationManager::class.java)) }
    private val taskProgressSelection = TaskProgressSelection()
    private val shownTasks = mutableMapOf<String, TaskProgressNotice>()
    private val notificationReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (intent.action) {
                ACTION_DISMISS_TASK_PROGRESS -> {
                    val taskId = intent.data?.lastPathSegment ?: return
                    val notice = shownTasks.remove(taskId) ?: return
                    taskProgressSelection.dismiss(taskId)
                    // A progress update can slip in between the swipe and delivery of its delete intent.
                    notificationManager.cancel(taskId, TASK_PROGRESS_ID)
                    notice.release()
                }
                NotificationManager.ACTION_NOTIFICATION_CHANNEL_BLOCK_STATE_CHANGED -> {
                    if (intent.getStringExtra(NotificationManager.EXTRA_NOTIFICATION_CHANNEL_ID) != TASK_PROGRESS_CHANNEL) return
                }
                NotificationManager.ACTION_APP_BLOCK_STATE_CHANGED -> Unit
                else -> return
            }
            // Use the same refresh budget as engine updates, including when progress is stalled.
            if (watcher?.isActive == true) requestNotificationRefresh()
        }
    }

    /** 最近一次已交付的启动请求；停止只针对它，之后在途的启动请求会让 [stopSelfResult] 返回 false。 */
    private var lastStartId = 0
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        ensureChannel(CHANNEL_ID, R.string.mobileDlNotifChannel, R.string.mobileDlNotifChannelDesc)
        ensureChannel(TASK_PROGRESS_CHANNEL, R.string.mobileDlNotifTaskChannel, R.string.mobileDlNotifTaskChannelDesc)
        ContextCompat.registerReceiver(
            this, notificationReceiver, IntentFilter(ACTION_DISMISS_TASK_PROGRESS).apply { addDataScheme("fluxdown") },
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            ContextCompat.registerReceiver(
                this, notificationReceiver, IntentFilter(NotificationManager.ACTION_NOTIFICATION_CHANNEL_BLOCK_STATE_CHANGED).apply {
                    addAction(NotificationManager.ACTION_APP_BLOCK_STATE_CHANGED)
                },
                ContextCompat.RECEIVER_NOT_EXPORTED,
            )
        }
        // 前台服务异常退出时系统可能仍保留普通通知；重新启动后按当前任务重建。
        for (entry in notificationManager.activeNotifications) {
            if (entry.id == TASK_PROGRESS_ID && entry.notification.channelId == TASK_PROGRESS_CHANNEL) {
                notificationManager.cancel(entry.tag, entry.id)
            }
        }
        AppDiagnostics.event("download service created")
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        lastStartId = startId
        notificationRefresh?.cancel()
        // startForegroundService 之后必须在数秒内 startForeground：先用当前量发出，再进入观察
        val container = (application as FluxApplication).container
        show(container.localActivity.value)
        // 完成 / 失败通知的观察在应用作用域内：下载期间即使 Activity 已被划掉也不中断
        DownloadNotifier.attach(container, applicationContext)
        // 每次启动请求都重新开始观察（空闲宽限随之重新计时）：外部唤起会在新任务的状态到达之前请求启动，
        // 旧的宽限计时不能紧接着把服务停掉。
        watcher?.cancel()
        watcher = scope.launch { watch(container) }
        return START_NOT_STICKY
    }

    private suspend fun watch(container: AppContainer) {
        container.localActivity.collectLatest { activity ->
            if (activity.busy) {
                requestNotificationRefresh()
                if (activity.stale) {
                    delay(STALE_KEEP_MS)
                    stopIfIdle("local session stale for ${STALE_KEEP_MS}ms")
                }
            } else {
                notificationRefresh?.cancel()
                taskProgressSelection.select(emptyList(), emptySet())
                clearTaskNotifications()
                delay(IDLE_GRACE_MS)
                stopIfIdle("idle")
            }
        }
    }

    private fun requestNotificationRefresh() {
        if (notificationRefresh?.isActive == true) return
        notificationRefresh = scope.launch {
            val wait = THROTTLE_MS - (SystemClock.elapsedRealtime() - lastShownAtMs)
            if (wait > 0) delay(wait)
            val activity = (application as FluxApplication).container.localActivity.value
            if (activity.busy) show(activity)
        }
    }

    /**
     * 只停止已交付的最后一次启动：若此后又有 `startForegroundService` 在途，系统返回 false 并保留服务——
     * 无条件 `stopSelf()` 会在它调用 `startForeground` 之前拆掉服务，系统随即以
     * ForegroundServiceDidNotStartInTimeException 结束整个进程。
     */
    private fun stopIfIdle(reason: String) {
        if (!stopSelfResult(lastStartId)) {
            AppDiagnostics.event("download service stop skipped ($reason): newer start pending")
            return
        }
        AppDiagnostics.event("download service stopping: $reason")
        releaseWakeLock()
        stopForeground(STOP_FOREGROUND_REMOVE)
        watcher?.cancel()
        notificationRefresh?.cancel()
    }

    /** Android 15+：dataSync 前台服务 24h 内累计 6 小时上限；系统回调后必须立即停止，回到前台时由 [DownloadServiceEffect] 重新拉起。 */
    override fun onTimeout(startId: Int, fgsType: Int) {
        AppDiagnostics.event("download service timed out (type=$fgsType); stopping until the app returns to foreground")
        watcher?.cancel()
        notificationRefresh?.cancel()
        releaseWakeLock()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        AppDiagnostics.event("download service destroyed")
        releaseWakeLock()
        scope.cancel()
        clearTaskNotifications()
        unregisterReceiver(notificationReceiver)
        super.onDestroy()
    }

    /** 重复调用 `startForeground` 即更新通知，且不受通知权限影响；服务类型参数 Android 10 以下由 ServiceCompat 忽略。 */
    private fun show(activity: LocalActivity) {
        lastShownAtMs = SystemClock.elapsedRealtime()
        val selected = if (activity.stale) emptyList() else taskProgressSelection.select(activity.transfers, shownTasks.keys)
        val visible = if (selected.isNotEmpty() && notificationManager.areNotificationsEnabled() &&
            notificationManager.getNotificationChannel(TASK_PROGRESS_CHANNEL)?.importance != NotificationManager.IMPORTANCE_NONE
        ) selected else emptyList()
        val hasIndividual = activity.transfers.size == 1 && visible.isNotEmpty()
        ServiceCompat.startForeground(
            this, NOTIFICATION_ID, buildNotification(this, activity, hasIndividual), ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
        )
        if (activity.busy) holdWakeLock()
        showTaskNotifications(visible)
    }

    private fun showTaskNotifications(transfers: List<Transfer>) {
        val posted = shownTasks.iterator()
        while (posted.hasNext()) {
            val (taskId, notice) = posted.next()
            if (transfers.any { it.taskId == taskId }) continue
            notificationManager.cancel(taskId, TASK_PROGRESS_ID)
            notice.release()
            posted.remove()
        }
        val now = SystemClock.elapsedRealtime()
        var activeIds: Set<String>? = null
        for (transfer in transfers) {
            val notice = shownTasks.getOrPut(transfer.taskId) { newTaskProgressNotice(transfer.taskId) }
            if (notice.transfer == transfer) {
                val ids = activeIds ?: if (now - lastTaskNoticeCheckAtMs >= TASK_NOTICE_CHECK_MS) {
                    lastTaskNoticeCheckAtMs = now
                    notificationManager.activeNotifications.mapNotNullTo(HashSet(shownTasks.size)) {
                        if (it.id == TASK_PROGRESS_ID && it.notification.channelId == TASK_PROGRESS_CHANNEL) it.tag else null
                    }.also { activeIds = it }
                } else continue
                if (transfer.taskId in ids) {
                    notice.missingAtLastCheck = false
                    continue
                }
                // A second observation gives the delete intent time to arrive before an unchanged notice is restored.
                if (!notice.missingAtLastCheck) {
                    notice.missingAtLastCheck = true
                    continue
                }
            }
            val builder = progressBuilder(this, TASK_PROGRESS_CHANNEL)
                .setContentIntent(notice.open)
                .setDeleteIntent(notice.dismiss)
            showTransfer(this, builder, transfer)
            notificationManager.notify(transfer.taskId, TASK_PROGRESS_ID, builder.build())
            notice.transfer = transfer
            notice.missingAtLastCheck = false
        }
    }

    private fun newTaskProgressNotice(taskId: String): TaskProgressNotice {
        // Intent extras 不参与 PendingIntent 的身份比较；用 URI 区分任务，不压成可能碰撞的整数。
        val identity = Uri.Builder().scheme("fluxdown").authority("task-progress").appendPath(taskId).build()
        val open = NotificationIntents.pending(this, HostRef.Local.ID, taskId, TASK_PROGRESS_ID, identity)
        val dismiss = PendingIntent.getBroadcast(
            this, TASK_PROGRESS_ID,
            Intent(ACTION_DISMISS_TASK_PROGRESS).setPackage(packageName).setData(identity),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_CANCEL_CURRENT,
        )
        return TaskProgressNotice(open, dismiss)
    }

    private fun clearTaskNotifications() = showTaskNotifications(emptyList())

    private class TaskProgressNotice(val open: PendingIntent, val dismiss: PendingIntent) {
        var transfer: Transfer? = null
        var missingAtLastCheck = false

        fun release() {
            open.cancel()
            dismiss.cancel()
        }
    }

    /** 渠道只创建一次（已存在则跳过）；LOW = 静默、不弹出。 */
    private fun ensureChannel(id: String, name: Int, descriptionId: Int) {
        if (notificationManager.getNotificationChannel(id) != null) return
        val channel = NotificationChannel(id, str(name), NotificationManager.IMPORTANCE_LOW).apply {
            description = str(descriptionId)
            setShowBadge(false)
        }
        notificationManager.createNotificationChannel(channel)
    }

    /** 下载期间保持 CPU 运行（前台服务本身不阻止息屏后 CPU 休眠）；非引用计数，带超时、每次刷新续期。 */
    private fun holdWakeLock() {
        val lock = wakeLock ?: getSystemService(PowerManager::class.java)
            ?.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "FluxDown:download")
            ?.also { it.setReferenceCounted(false) }
            ?.also { wakeLock = it }
            ?: return
        lock.acquire(WAKE_LOCK_TIMEOUT_MS)
    }

    private fun releaseWakeLock() {
        wakeLock?.takeIf { it.isHeld }?.release()
        wakeLock = null
    }
}

/**
 * 进度通知：
 * - 1 个活跃任务：有逐任务通知时仅显示任务数量与等待数；否则优先使用分片进度，再回退字节进度；
 * - 多个活跃任务：进度条汇总相同口径，混合分片与字节时为不确定态（[TransferSummary]），
 *   展开后逐行列出前 [DETAIL_LINES] 个任务的进度与速度，其余折叠为「等 N 个文件」；
 * - 只有排队 / 待重试：标题说明等待，无进度条。
 * 页眉副文本放百分比，折叠态也能一眼看到进度；点按单任务通知直达该任务详情。
 */
private fun buildNotification(context: Context, a: LocalActivity, hasIndividual: Boolean): android.app.Notification {
    val transfers = a.transfers
    val single = transfers.singleOrNull()
    val builder = progressBuilder(context, CHANNEL_ID)
        .setContentIntent(NotificationIntents.pending(context, HostRef.Local.ID, single?.taskId.orEmpty(), code = NOTIFICATION_ID))
        .setOngoing(true)
        .setSilent(true)
        .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
    val waiting = if (a.waiting > 0) context.str(R.string.mobileDlNotifWaiting, "n" to a.waiting) else null
    when {
        hasIndividual -> builder.setContentTitle(context.str(R.string.mobileDlNotifActive, "n" to 1)).setContentText(waiting)
        single != null -> showTransfer(context, builder, single, waiting)
        transfers.isNotEmpty() -> {
            val summary = TransferSummary.of(transfers)
            val headline = listOfNotNull(
                speedLabel(context, a.downBps),
                summary.etaSeconds?.let { remainingLabel(context, it) },
                waiting,
            ).joinToString(SEP)
            val lines = transfers.take(DETAIL_LINES).map { t ->
                val fragments = t.hlsFragments
                val progress = if (fragments != null) {
                    context.str(R.string.hlsFragmentProgress, "done" to fragments.written, "total" to fragments.total)
                } else {
                    t.fraction?.let(::percentLabel) ?: Format.bytes(t.downloadedBytes).toString()
                }
                listOf(t.fileName, progress, Format.speedOrZero(t.speedBps).toString()).joinToString(SEP)
            }
            val more = transfers.size - DETAIL_LINES
            val body = buildList {
                add(headline)
                addAll(lines)
                if (more > 0) add(context.str(R.string.andMoreFiles, "count" to more))
            }.joinToString("\n")
            builder.setContentTitle(context.str(R.string.mobileDlNotifActive, "n" to transfers.size))
                .setContentText(headline)
                .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            applyProgress(builder, summary.fraction)
        }
        else -> {
            val title = if (a.active > 0) {
                context.str(R.string.mobileDlNotifActive, "n" to a.active)
            } else {
                context.str(R.string.mobileDlNotifWaiting, "n" to a.waiting)
            }
            builder.setContentTitle(title)
            if (a.active > 0) builder.setContentText(listOfNotNull(speedLabel(context, a.downBps), waiting).joinToString(SEP))
        }
    }
    return builder.build()
}

private fun progressBuilder(context: Context, channelId: String) =
    NotificationCompat.Builder(context, channelId)
        .setSmallIcon(R.drawable.ic_stat_download)
        .setColor(NotificationTint)
        .setOnlyAlertOnce(true)
        .setShowWhen(false)
        .setCategory(NotificationCompat.CATEGORY_PROGRESS)

private fun showTransfer(context: Context, builder: NotificationCompat.Builder, transfer: Transfer, waiting: String? = null) {
    val fragments = transfer.hlsFragments
    builder.setContentTitle(transfer.fileName.ifEmpty { context.str(R.string.mobileDlNotifActive, "n" to 1) })
        .setContentText(
            listOfNotNull(
                speedLabel(context, transfer.speedBps),
                if (fragments != null) {
                    context.str(R.string.hlsFragmentProgress, "done" to fragments.written, "total" to fragments.total)
                } else sizeLabel(transfer.downloadedBytes, transfer.totalBytes),
                if (fragments == null) transfer.etaSeconds?.let { remainingLabel(context, it) } else null,
                waiting,
            ).joinToString(SEP),
        )
    applyProgress(builder, transfer.fraction)
}

private const val SEP = " · "

/** 多任务通知展开后最多列出的任务行数。 */
private const val DETAIL_LINES = 5

private const val PROGRESS_MAX = 1000

/** 进度条 + 页眉百分比；无可用比例则显示不确定态（不显示百分比）。 */
private fun applyProgress(builder: NotificationCompat.Builder, fraction: Float?) {
    if (fraction == null) {
        builder.setProgress(0, 0, true)
    } else {
        builder.setProgress(PROGRESS_MAX, (fraction * PROGRESS_MAX).toInt(), false)
            .setSubText(percentLabel(fraction))
    }
}

private fun percentLabel(fraction: Float): String = "${(fraction * 100).toInt()}%"

private fun speedLabel(context: Context, bps: Long): String =
    context.str(R.string.mobileDlNotifSpeed, "speed" to Format.speedOrZero(bps).toString())

/** `已下 / 总量`；大小未知只给已下载量。 */
private fun sizeLabel(downloaded: Long, total: Long): String =
    if (total > 0) "${Format.bytes(downloaded)} / ${Format.bytes(total)}" else Format.bytes(downloaded).toString()

/** 剩余时间：`<60s` 秒、`<1h` 分钟（向上取整）、其余 `h m`（与任务详情同一组文案）。 */
private fun remainingLabel(context: Context, seconds: Long): String {
    val time = when {
        seconds < 60 -> context.str(R.string.etaSeconds, "n" to seconds)
        seconds < 3600 -> context.str(R.string.etaMinutes, "n" to (seconds + 59) / 60)
        else -> context.str(R.string.etaHours, "n" to seconds / 3600) + " " + context.str(R.string.etaMinutes, "n" to seconds % 3600 / 60)
    }
    return context.str(R.string.mobileDlNotifRemaining, "time" to time)
}

/**
 * 前台服务启动入口与通知权限（Android 13+）。
 * 只在应用处于前台时调用（Android 12+ 禁止后台启动前台服务）。
 */
object DownloadServiceController {
    private const val PREFS = "fluxdown_service"
    private const val KEY_NOTIF_ASKED = "notif_permission_asked"

    /** 拉起（或刷新）前台服务。后台启动被系统拒绝时记入诊断，服务若已在运行则不受影响。 */
    fun start(context: Context, reason: String) {
        try {
            ContextCompat.startForegroundService(context, Intent(context, DownloadService::class.java))
        } catch (e: IllegalStateException) {
            // 含 ForegroundServiceStartNotAllowedException（后台启动受限 / dataSync 时长耗尽）
            AppDiagnostics.event("download service start refused ($reason): ${e.javaClass.simpleName}: ${e.message}")
        } catch (e: SecurityException) {
            AppDiagnostics.event("download service start denied ($reason): ${e.message}")
        }
    }

    /**
     * 进程级观察：本机由空闲变为有活动时就尝试拉起前台服务（界面不在前台时也尝试）。后台启动在 Android 12+
     * 多数会被拒——电池优化白名单内的应用豁免；被拒时回到前台由 [DownloadServiceEffect] 补拉。
     */
    fun keepAliveWhileBusy(context: Context, container: AppContainer) {
        val app = context.applicationContext
        container.appScope.launch {
            container.localActivity
                .map { it.busy }
                .distinctUntilChanged()
                .collect { busy -> if (busy) start(app, "local activity") }
        }
    }

    /** 需要请求通知权限：Android 13+、尚未授予、且从未请求过（避免每次冷启动打扰；拒绝后服务照常运行）。 */
    fun shouldRequestNotificationPermission(context: Context): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return false
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) return false
        return !context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY_NOTIF_ASKED, false)
    }

    /** 记录已请求过（无论用户同意与否）。 */
    fun markNotificationPermissionAsked(context: Context) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(KEY_NOTIF_ASKED, true).apply()
    }
}
