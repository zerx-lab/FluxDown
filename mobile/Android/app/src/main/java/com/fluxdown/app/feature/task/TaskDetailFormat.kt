package com.fluxdown.app.feature.task

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import androidx.compose.runtime.Composable
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.core.model.Task
import com.fluxdown.app.ui.TaskVisualState
import com.fluxdown.core.model.TaskProtocol
import java.net.URI
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import kotlin.math.ceil

/** 协议标记：技术名词，不随语言变化。 */
internal fun TaskProtocol.tag(): String = when (this) {
    TaskProtocol.Http -> "HTTP"
    TaskProtocol.Ftp -> "FTP"
    TaskProtocol.Bt -> "BT"
    TaskProtocol.Ed2k -> "ED2K"
    TaskProtocol.Hls -> "HLS"
    TaskProtocol.Plugin -> "PLUGIN"
}

/** 来源站点：originUrl ‖ url 的 host（去 `www.`）；取不到时 BT 为 BitTorrent，其余为协议名。 */
internal fun Task.siteLabel(): String {
    val raw = originUrl.ifEmpty { url }
    val host = runCatching { URI(raw).host }.getOrNull()?.removePrefix("www.")?.takeIf { it.isNotEmpty() }
    return host ?: if (protocol == TaskProtocol.Bt) "BitTorrent" else protocol.tag()
}

/** 状态词（详情英雄头与字段表共用）。 */
@Composable
internal fun statusWord(visual: TaskVisualState, queuePosition: Int): String = when (visual) {
    TaskVisualState.Downloading -> str(R.string.statusDownloading)
    TaskVisualState.Queued -> str(R.string.subtitleQueued, "pos" to queuePosition)
    TaskVisualState.Pending -> str(R.string.statusPending)
    TaskVisualState.Preparing -> str(R.string.statusPreparing)
    TaskVisualState.Verifying -> str(R.string.statusVerifying)
    TaskVisualState.Paused -> str(R.string.statusPaused)
    TaskVisualState.Failed -> str(R.string.statusError)
    TaskVisualState.Completed -> str(R.string.statusCompleted)
    TaskVisualState.Missing -> str(R.string.statusFileMissing)
    TaskVisualState.Seeding -> str(R.string.statusSeeding)
}

internal fun firstLine(text: String): String = text.lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() }.orEmpty()

private val DateTimeFmt: DateTimeFormatter = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm:ss")

/** Unix 秒 → `YYYY-MM-DD HH:MM:SS`（本地时区）；0 = 无。 */
internal fun formatDateTime(epochSec: Long): String =
    if (epochSec <= 0) "" else DateTimeFmt.format(Instant.ofEpochSecond(epochSec).atZone(ZoneId.systemDefault()))

/** ETA / 时长：`<60s` 秒、`<1h` 分钟、其余 `h m`；null = —。 */
@Composable
internal fun etaText(seconds: Long?): String {
    if (seconds == null) return "—"
    return when {
        seconds < 60 -> str(R.string.etaSeconds, "n" to seconds)
        seconds < 3600 -> str(R.string.etaMinutes, "n" to ceil(seconds / 60.0).toLong())
        else -> str(R.string.etaHours, "n" to seconds / 3600) + " " + str(R.string.etaMinutes, "n" to (seconds % 3600) / 60)
    }
}

/** 做种时长：`d h m` 三档，高位为 0 时省略。 */
@Composable
internal fun durationText(totalSeconds: Long): String {
    val d = totalSeconds / 86_400
    val h = totalSeconds % 86_400 / 3600
    val m = totalSeconds % 3600 / 60
    val parts = ArrayList<String>(3)
    if (d > 0) parts += "$d ${str(R.string.timeUnitDays)}"
    if (h > 0) parts += "$h ${str(R.string.timeUnitHours)}"
    if (m > 0 || parts.isEmpty()) parts += "$m ${str(R.string.timeUnitMinutes)}"
    return parts.joinToString(" ")
}

/** 任务链路 wire 标签（Auto 路由、站点规则、全局 / 任务级代理与 BT 会话出口）→ 文案；未知值原样返回。 */
@Composable
internal fun routeLabel(route: String): String {
    // 整串精确匹配必须先于后缀拆分，否则 `rule:system`、`bt:all:manual` 会被误拆。
    when (route) {
        "rule:direct" -> return str(R.string.taskRouteRuleDirect)
        "rule:system" -> return str(R.string.taskRouteRuleSystem)
        "rule:manual" -> return str(R.string.taskRouteRuleManual)
        "rule:custom" -> return str(R.string.taskRouteRuleCustom)
        "global:direct" -> return str(R.string.taskRouteGlobalDirect)
        "global:bypass" -> return str(R.string.taskRouteGlobalBypass)
        "global:manual" -> return str(R.string.taskRouteGlobalManual)
        "global:system" -> return str(R.string.taskRouteGlobalSystem)
        "global:pac" -> return str(R.string.taskRouteGlobalPac)
        "task:proxy" -> return str(R.string.taskRouteTaskProxy)
        "task:direct" -> return str(R.string.taskRouteTaskDirect)
        "bt:direct" -> return str(R.string.taskRouteBtDirect)
        "bt:trackers:manual" -> return str(R.string.taskRouteBtTrackersManual)
        "bt:trackers:system" -> return str(R.string.taskRouteBtTrackersSystem)
        "bt:all:manual" -> return str(R.string.taskRouteBtAllManual)
        "bt:all:system" -> return str(R.string.taskRouteBtAllSystem)
    }
    var base = route
    var via: String? = null
    if (route.endsWith(":system")) {
        base = route.removeSuffix(":system")
        via = str(R.string.taskRouteViaSystem)
    } else if (route.endsWith(":manual")) {
        base = route.removeSuffix(":manual")
        via = str(R.string.taskRouteViaManual)
    }
    val label = when (base) {
        "direct" -> str(R.string.taskRouteDirect)
        "direct:sampled" -> str(R.string.taskRouteDirectSampled)
        "direct:pinned" -> str(R.string.taskRouteDirectPinned)
        "direct:failover" -> str(R.string.taskRouteDirectFailover)
        "proxy:cached" -> str(R.string.taskRouteProxyCached)
        "proxy:sampled" -> str(R.string.taskRouteProxySampled)
        "proxy:failover" -> str(R.string.taskRouteProxyFailover)
        else -> route
    }
    return if (via == null) label else "$label · $via"
}

internal fun Context.copyText(label: String, text: String) {
    getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText(label, text))
}

/**
 * 在文件管理器中定位文件夹：外部存储路径映射为 DocumentsProvider 目录 URI（`ACTION_VIEW`）；
 * 无法映射或没有应用可处理时返回 false，由调用方降级。
 */
internal fun Context.openFolder(path: String): Boolean {
    val documentId = path.toExternalDocumentId() ?: return false
    val uri: Uri = DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", documentId)
    val view = Intent(Intent.ACTION_VIEW)
        .setDataAndType(uri, DocumentsContract.Document.MIME_TYPE_DIR)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    return runCatching { startActivity(view) }.isSuccess
}

/** `/storage/emulated/0/Download/X` → `primary:Download/X`；`/storage/ABCD-1234/X` → `ABCD-1234:X`。 */
private fun String.toExternalDocumentId(): String? {
    val emulated = "/storage/emulated/0"
    if (this == emulated || startsWith("$emulated/")) return "primary:" + removePrefix(emulated).trimStart('/')
    val m = Regex("^/storage/([^/]+)(?:/(.*))?$").matchEntire(this) ?: return null
    val volume = m.groupValues[1]
    if (volume == "emulated" || volume == "self") return null
    return "$volume:${m.groupValues[2]}"
}
