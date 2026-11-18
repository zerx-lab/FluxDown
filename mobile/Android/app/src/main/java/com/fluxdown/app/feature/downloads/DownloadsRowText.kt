package com.fluxdown.app.feature.downloads

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import com.fluxdown.app.R
import com.fluxdown.app.data.CardField
import com.fluxdown.app.i18n.fill
import com.fluxdown.app.i18n.str
import com.fluxdown.app.ui.TaskVisualState
import com.fluxdown.core.format.Format
import com.fluxdown.core.model.Queue
import com.fluxdown.core.model.SeedingStatus
import com.fluxdown.core.model.TaskErrorMessage
import com.fluxdown.core.model.TaskProtocol
import com.fluxdown.fluxui.data.MetaTone
import com.fluxdown.fluxui.data.buildTaskMeta
import com.fluxdown.fluxui.theme.FluxColors

/**
 * 行 / 分区内用到的全部固定文案：随语言一次性取出，行组合项不再逐个读资源。
 */
@Immutable
internal class RowText(c: Context) {
    val paused = c.str(R.string.statusPaused)
    val pending = c.str(R.string.statusPending)
    val queuedFmt = c.getString(R.string.subtitleQueued)
    val preparing = c.str(R.string.subtitlePreparing)
    val verifying = c.str(R.string.statusVerifying)
    val seeding = c.str(R.string.statusSeeding)
    val seedingQueued = c.str(R.string.seedingStatusQueued)
    val completed = c.str(R.string.statusCompleted)
    val missing = c.str(R.string.statusFileMissing)
    val failed = c.str(R.string.statusError)
    private val errorPostOnlyEndpoint = c.str(R.string.taskErrorPostOnlyEndpoint)
    private val errorPostOnlyResume = c.str(R.string.taskErrorPostOnlyResume)

    /** 引擎稳定错误文案 → 本地化；其余原样。 */
    fun taskError(raw: String): String = when (TaskErrorMessage.match(raw)) {
        TaskErrorMessage.PostOnlyEndpoint -> errorPostOnlyEndpoint
        TaskErrorMessage.PostOnlyResume -> errorPostOnlyResume
        null -> raw
    }

    private val etaSecondsFmt = c.getString(R.string.etaSeconds)
    private val etaMinutesFmt = c.getString(R.string.etaMinutes)
    private val etaHoursFmt = c.getString(R.string.etaHours)
    private val relNowText = c.str(R.string.mobileTimeJustNow)
    private val relMinutesFmt = c.getString(R.string.mobileTimeMinutesAgo)
    private val relHoursFmt = c.getString(R.string.mobileTimeHoursAgo)
    private val relDaysFmt = c.getString(R.string.mobileTimeDaysAgo)

    private val mainQueue = c.str(R.string.mainQueue)
    private val laterQueue = c.str(R.string.downloadLater)

    val ringPause = c.str(R.string.pause)
    val ringResume = c.str(R.string.resume)
    val ringRetry = c.str(R.string.mobileRetry)
    val ringRedownload = c.str(R.string.redownloadTask)
    val ringSeeding = c.str(R.string.statusSeeding)
    val ringOpen = c.str(R.string.mobileOpenFile)

    val boost = c.str(R.string.boostDownload)
    val cancelBoost = c.str(R.string.cancelBoost)
    val copyLink = c.str(R.string.mobileSwipeCopyLink)
    val delete = c.str(R.string.delete)
    val openDetails = c.str(R.string.mobileTaskDetail)
    val conflictPending = c.str(R.string.fileConflictPending)
    val conflictTooltip = c.str(R.string.fileConflictPendingTooltip)

    fun queued(pos: Int): String = queuedFmt.fill("pos" to pos)

    fun queueLabel(q: Queue): String = when (q.queueId) {
        "", Queue.MAIN -> mainQueue
        Queue.LATER -> laterQueue
        else -> q.name
    }

    /** ETA 文案（§1）：<60s 秒 · <1h 分钟（向上取整）· 其余「h 小时 m 分」。 */
    fun eta(seconds: Long): String = when {
        seconds < 60 -> etaSecondsFmt.fill("n" to seconds)
        seconds < 3600 -> etaMinutesFmt.fill("n" to ((seconds + 59) / 60))
        else -> {
            var h = seconds / 3600
            var m = (seconds % 3600 + 59) / 60
            if (m == 60L) {
                h += 1
                m = 0
            }
            if (m == 0L) etaHoursFmt.fill("n" to h) else etaHoursFmt.fill("n" to h) + " " + etaMinutesFmt.fill("n" to m)
        }
    }

    fun relative(unixSec: Long, nowSec: Long): String {
        val d = (nowSec - unixSec).coerceAtLeast(0)
        return when {
            d < 60 -> relNowText
            d < 3600 -> relMinutesFmt.fill("n" to d / 60)
            d < 86_400 -> relHoursFmt.fill("n" to d / 3600)
            else -> relDaysFmt.fill("n" to d / 86_400)
        }
    }
}

internal val LocalRowText = staticCompositionLocalOf<RowText> { error("RowText missing") }

@Composable
internal fun rememberRowText(): RowText {
    val ctx = LocalContext.current
    val cfg = LocalConfiguration.current
    return remember(cfg) { RowText(ctx) }
}

/** 协议标识（技术名词，随 PC 同写法，不做翻译）。 */
internal fun protocolLabel(p: TaskProtocol): String = when (p) {
    TaskProtocol.Http -> "HTTP"
    TaskProtocol.Ftp -> "FTP"
    TaskProtocol.Bt -> "BT"
    TaskProtocol.Ed2k -> "ED2K"
    TaskProtocol.Hls -> "HLS"
    TaskProtocol.Plugin -> "Plugin"
}

private fun firstLine(s: String): String? = s.lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() }

/**
 * 元信息行（§3.7 taskMeta）：按状态拼接，失败行整行 coralText 且不追加尾部字段。
 * 字段可选项来自视图偏好 [fields]，尾部顺序固定：协议 · 来源 · 队列 · 创建时间。
 * [queuePosition] = 排队名次（1 起），只对排队行有意义：由渲染方按可见行取，不随任务行模型缓存。
 */
internal fun buildRowMeta(
    colors: FluxColors,
    t: RowText,
    item: TaskItem,
    fields: Set<CardField>,
    nowSec: Long,
    queuePosition: Int,
): AnnotatedString = colors.buildTaskMeta {
    val task = item.task
    val showSize = CardField.Size in fields
    val showSpeed = CardField.Speed in fields
    val showEta = CardField.Eta in fields
    val total = task.totalBytes
    val pct = item.progress?.let(Format::percent).orEmpty()
    val doneOfTotal = if (total > 0) "${Format.bytes(task.downloadedBytes)}/${Format.bytes(total)}" else Format.bytes(task.downloadedBytes).toString()
    val totalSize = if (total > 0) Format.bytes(total).toString() else ""
    val upText = if (item.speedUp > 0) "↑ ${Format.speed(item.speedUp)}" else ""

    when (item.visual) {
        TaskVisualState.Downloading -> {
            if (showSpeed && item.speedDown > 0) part(Format.speed(item.speedDown).toString(), MetaTone.Accent)
            if (showEta) part(item.eta?.let(t::eta) ?: "—")
            part(pct)
            if (showSize) part(doneOfTotal)
            if (showSpeed) part(upText)
        }
        TaskVisualState.Paused -> {
            part(t.paused)
            part(pct)
            if (showSize && total > 0) part(doneOfTotal)
        }
        TaskVisualState.Queued -> {
            part(t.queued(queuePosition))
            if (showSize) part(totalSize)
        }
        TaskVisualState.Pending -> {
            part(t.pending)
            if (showSize) part(totalSize)
        }
        TaskVisualState.Preparing -> part(t.preparing)
        TaskVisualState.Verifying -> part(t.verifying)
        TaskVisualState.Failed -> part(firstLine(t.taskError(task.errorMessage)) ?: t.failed, MetaTone.Coral)
        TaskVisualState.Missing -> {
            part(t.missing, MetaTone.Amber)
            if (showSize) part(totalSize)
        }
        TaskVisualState.Seeding -> {
            if (task.seedingStatus == SeedingStatus.QueuedForSlot) {
                part(t.seedingQueued, MetaTone.Mint)
            } else {
                part(t.seeding, MetaTone.Mint)
                part(upText)
            }
            if (showSize) part(totalSize)
        }
        TaskVisualState.Completed -> {
            var any = false
            if (showSize && total > 0) {
                part(totalSize)
                any = true
            }
            if (CardField.Created !in fields && task.completedAt > 0) {
                part(t.relative(task.completedAt, nowSec))
                any = true
            }
            if (!any) part(t.completed)
        }
    }

    if (item.visual != TaskVisualState.Failed) {
        trailing(
            if (CardField.Protocol in fields) protocolLabel(task.protocol) else null,
            if (CardField.Site in fields) item.site.ifEmpty { null } else null,
            if (CardField.Queue in fields) item.queue?.let(t::queueLabel) else null,
            if (CardField.Created in fields && task.createdAt > 0) t.relative(task.createdAt, nowSec) else null,
        )
    }
}
