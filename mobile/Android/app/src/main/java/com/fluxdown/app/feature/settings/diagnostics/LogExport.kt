package com.fluxdown.app.feature.settings.diagnostics

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Context
import android.content.Intent
import com.fluxdown.core.format.Format
import android.util.Log
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.fluxdown.app.AppContainer
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.diag.AppDiagnostics
import com.fluxdown.app.feature.settings.SettingRowBox
import com.fluxdown.app.feature.settings.SettingsCtx
import com.fluxdown.app.feature.settings.SettingsEntry
import com.fluxdown.app.feature.settings.SettingsSearchContext
import com.fluxdown.app.feature.settings.settingNumber
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.LogExportHttp
import com.fluxdown.core.protocol.LogExportParams
import com.fluxdown.core.protocol.LogExportResult
import com.fluxdown.core.protocol.LogPathsDto
import com.fluxdown.core.protocol.callJson
import com.fluxdown.core.store.Connection
import com.fluxdown.fluxui.controls.FluxActionRow
import com.fluxdown.fluxui.controls.FluxKeyValue
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.UUID

private const val TAG = "FluxLogExport"

/** 导出临时文件的根目录（`cache/log-export/<uuid>/fluxdown-logs.zip`；与 `res/xml/file_paths.xml` 的 `log_export` 对应）。 */
private const val EXPORT_ROOT = "log-export"
private const val ARCHIVE_NAME = "fluxdown-logs.zip"

/** 分享给其它应用的压缩包可能在系统分享面板关闭后才被读取：只清理这么久之前的旧导出。 */
private const val STALE_AFTER_MS = 60L * 60L * 1000L

private const val CONNECT_TIMEOUT_MS = 15_000

/** 主机现打包日志，大目录需要时间。 */
private const val READ_TIMEOUT_MS = 180_000

/** 设置搜索 itemKey：日志分组在诊断页 / 关于页都用这个 key。 */
internal const val LOGS_ITEM_KEY = "logs"

/** 远端 `--server` 主机的日志下载（`GET /api/web/logs/export`，Bearer 访问密钥）。 */
private object LogExportDownloader {
    /**
     * 下载到 [target]；失败映射为 [HostException]（401 / 403 = 访问密钥被拒）。
     * 协程被取消时断开连接以立刻解除阻塞的读。
     */
    suspend fun download(url: String, accessKey: String, target: File) = coroutineScope {
        val connection = try {
            URL(url).openConnection() as HttpURLConnection
        } catch (e: IOException) {
            throw HostException(HostErrorCode.InvalidArgument, message = "invalid host address", cause = e)
        }
        val work = async(Dispatchers.IO) { transfer(connection, accessKey, target) }
        try {
            work.await()
        } catch (e: CancellationException) {
            connection.disconnect()
            throw e
        }
    }

    private fun transfer(connection: HttpURLConnection, accessKey: String, target: File) {
        try {
            connection.requestMethod = "GET"
            connection.setRequestProperty("Authorization", "Bearer $accessKey")
            connection.connectTimeout = CONNECT_TIMEOUT_MS
            connection.readTimeout = READ_TIMEOUT_MS
            val status = connection.responseCode
            when {
                status in 200..299 -> Unit
                status == 401 || status == 403 ->
                    throw HostException(HostErrorCode.Unauthorized, message = "HTTP $status")
                else -> throw HostException(HostErrorCode.Unavailable, retryable = status >= 500, message = "HTTP $status")
            }
            connection.inputStream.use { input -> target.outputStream().use { out -> input.copyTo(out) } }
        } catch (e: IOException) {
            throw HostException(HostErrorCode.Unavailable, retryable = true, message = e.message ?: "I/O error", cause = e)
        } finally {
            connection.disconnect()
        }
    }
}

/** 日志占用 / 导出的状态（诊断页与关于页各持一份）。 */
@Stable
internal class LogExportModel(private val context: Context, private val container: AppContainer) {
    var isExporting by mutableStateOf(false)
        private set

    /** 最近一次导出的失败说明（行内显示）。 */
    var failure by mutableStateOf<String?>(null)
        private set

    /** 本机日志总大小（字节）；远端主机 / 目录不在本机 → null。 */
    var sizeBytes by mutableStateOf<Long?>(null)
        private set

    /** 刷新当前日志大小：只在本机主机上统计（日志目录路径来自 `agent.diagnostics.logPaths`）。 */
    suspend fun refreshSize() {
        val live = container.store.state.value.connection == Connection.Live
        if (container.host.value !is HostRef.Local || !live) {
            sizeBytes = null
            return
        }
        sizeBytes = try {
            val paths = LogPathsDto.fromJson(container.session.callJson(HostMethod.agentDiagnosticsLogPaths))
            withContext(Dispatchers.IO) { totalSize(listOf(paths.agentLogDir, paths.daemonLogDir)) }
        } catch (e: CancellationException) {
            throw e
        } catch (e: HostException) {
            Log.i(TAG, "log paths unavailable: ${e.code} ${e.message}")
            null
        }
    }

    /**
     * 导出并经系统分享面板分享。成功时临时文件保留到分享方读完（下次导出 / 打开诊断页时按年龄清理）；
     * 失败时立即清掉本次的临时目录。
     */
    suspend fun export(errorText: (HostException) -> String, onShared: () -> Unit) {
        if (isExporting) return
        isExporting = true
        failure = null
        var dir: File? = null
        var shared = false
        try {
            withContext(Dispatchers.IO) { cleanStaleExports(context) }
            val created = File(File(context.cacheDir, EXPORT_ROOT), UUID.randomUUID().toString())
            dir = created
            val target = withContext(Dispatchers.IO) {
                if (!created.mkdirs()) throw HostException(HostErrorCode.Internal, message = "cannot create export directory")
                File(created, ARCHIVE_NAME)
            }
            val produced = when (val host = container.host.value) {
                is HostRef.Local -> exportLocally(target)
                is HostRef.Remote -> {
                    exportRemotely(host, target)
                    target
                }
            }
            // 手机侧进程诊断（退出原因 / 前台服务事件 / 崩溃）与当前主机无关，一并附上
            withContext(Dispatchers.IO) {
                try {
                    AppDiagnostics.appendTo(context, produced)
                } catch (e: IOException) {
                    throw HostException(HostErrorCode.Internal, message = e.message ?: "append diagnostics failed", cause = e)
                }
            }
            if (share(produced)) {
                shared = true
                onShared()
            } else {
                failure = context.str(R.string.logExportFailed)
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: HostException) {
            failure = errorTextFor(e, errorText)
        } finally {
            if (!shared) dir?.let { withContext(kotlinx.coroutines.NonCancellable + Dispatchers.IO) { it.deleteRecursively() } }
            isExporting = false
        }
    }

    private fun errorTextFor(e: HostException, errorText: (HostException) -> String): String =
        if (e.code == HostErrorCode.Unauthorized) context.str(R.string.webLoginInvalidKey) else errorText(e)

    /** 本机：`agent.diagnostics.exportLogs` 把 zip 写到应用缓存目录。 */
    private suspend fun exportLocally(target: File): File {
        val result = LogExportResult.fromJson(
            container.session.callJson(HostMethod.agentDiagnosticsExportLogs, LogExportParams(target.path).toJson()),
        ) ?: throw HostException(HostErrorCode.Internal, message = "invalid export result")
        return withContext(Dispatchers.IO) {
            val reported = File(result.path)
            when {
                reported.isFile && reported.canonicalPath == target.canonicalPath -> target
                reported.isFile -> {
                    // 引擎写到了别处：拷进缓存导出目录，FileProvider 只暴露该目录。
                    try {
                        reported.copyTo(target, overwrite = true)
                    } catch (e: IOException) {
                        throw HostException(HostErrorCode.Internal, message = e.message ?: "copy failed", cause = e)
                    }
                }
                target.isFile -> target
                else -> throw HostException(HostErrorCode.Internal, message = "exported archive is missing")
            }
        }
    }

    /** 远端 `--server` 主机：网关拒绝 `exportLogs`（会写主机上的任意路径），只走 HTTP 下载。 */
    private suspend fun exportRemotely(host: HostRef.Remote, target: File) {
        val url = LogExportHttp.exportUrl(host.endpoint)
            ?: throw HostException(HostErrorCode.InvalidArgument, message = "invalid host address")
        val key = container.remoteAccessKey(host.id)
            ?: throw HostException(HostErrorCode.Unauthorized, message = "access key unavailable")
        LogExportDownloader.download(url, key, target)
    }

    /** 系统分享面板分享 zip（FileProvider URI）；没有可处理的应用 / URI 无法授予 → false。 */
    private fun share(file: File): Boolean {
        val uri = try {
            FileProvider.getUriForFile(context, "${context.packageName}.files", file)
        } catch (e: IllegalArgumentException) {
            Log.w(TAG, "file is outside the FileProvider roots: ${e.message}")
            return false
        }
        val send = Intent(Intent.ACTION_SEND)
            .setType("application/zip")
            .putExtra(Intent.EXTRA_STREAM, uri)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        send.clipData = ClipData.newRawUri(file.name, uri)
        val chooser = Intent.createChooser(send, context.str(R.string.logExportButton))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_GRANT_READ_URI_PERMISSION)
        return try {
            context.startActivity(chooser)
            true
        } catch (e: ActivityNotFoundException) {
            Log.w(TAG, "no share target: ${e.message}")
            false
        }
    }
}

/** 目录（递归）里普通文件的总字节数（不含隐藏文件）；目录不在本机 → null。去重后累加（agent / daemon 可能共用目录）。 */
private fun totalSize(directories: List<String>): Long? {
    val seen = HashSet<String>()
    var sum: Long? = null
    for (path in directories) {
        if (path.isEmpty()) continue
        val dir = File(path)
        if (!dir.isDirectory || !seen.add(dir.canonicalPath)) continue
        val size = dir.walkTopDown()
            .onEnter { it == dir || !it.name.startsWith('.') }
            .filter { it.isFile && !it.name.startsWith('.') }
            .sumOf { it.length() }
        sum = (sum ?: 0L) + size
    }
    return sum
}

/** 删除超过 [STALE_AFTER_MS] 的旧导出目录（IO 线程调用）。 */
internal fun cleanStaleExports(context: Context) {
    val root = File(context.cacheDir, EXPORT_ROOT)
    val children = root.listFiles() ?: return
    val cutoff = System.currentTimeMillis() - STALE_AFTER_MS
    for (child in children) {
        if (child.lastModified() < cutoff && !child.deleteRecursively()) Log.i(TAG, "stale export not removed: ${child.path}")
    }
}

@Composable
internal fun rememberLogExportModel(): LogExportModel {
    val context = LocalContext.current
    val container = LocalAppContainer.current
    return remember(context, container) { LogExportModel(context, container) }
}

/**
 * 日志分组：占用上限（`log_max_size_mb`，MB）、当前日志大小（仅本机）、导出日志（分享面板）。
 * 诊断页与关于页共用（同 iOS `LogsSection`）。[scope] 为页面级作用域：导出可能持续数分钟，
 * 日志区所在的 lazy item 滚出视口被销毁时不能随之取消。
 */
@Composable
internal fun LogsSection(ctx: SettingsCtx, model: LogExportModel, scope: CoroutineScope) {
    val container = LocalAppContainer.current
    val overlays = LocalFluxOverlays.current
    val actions = LocalTaskActions.current
    val appearance by container.appearanceState.collectAsStateWithLifecycle()
    val hostRef by container.host.collectAsStateWithLifecycle()
    val state = hostState()
    val live by remember { derivedStateOf { state.value.connection == Connection.Live } }
    val exportedText = str(R.string.logExportSuccessNotice)

    LaunchedEffect(hostRef, live) { model.refreshSize() }

    val exportDesc = str(R.string.logExportDesc)
    val sizeUnitHint = appearance?.let {
        str(R.string.mobileLogSizeUnitLabel, "unit" to it.fileSizeUnits.byteLabel(2))
    }.orEmpty()
    val footer = if (ctx.form.has("log_max_size_mb") && ctx.synced("log_max_size_mb")) {
        exportDesc + "\n" + str(R.string.settingsSyncLegend)
    } else {
        exportDesc
    }
    GlassSection(title = str(R.string.mobileLogsTitle), footer = footer) {
        settingNumber(
            ctx, "log_max_size_mb", R.string.logMaxSize, R.string.logMaxSizeDesc,
            range = 1L..1024L, default = 10L,
            unitLabel = sizeUnitHint,
            id = "logs.maxSize",
        )
        appearance?.let { currentAppearance ->
            model.sizeBytes?.let { bytes ->
                row {
                    SettingRowBox(ctx, "logs.currentSize", null) {
                        FluxKeyValue(
                            key = str(R.string.mobileLogCurrentSize),
                            value = Format.bytes(bytes, currentAppearance.fileSizeUnits).toString(),
                        )
                    }
                }
            }
        }
        row {
            SettingRowBox(ctx, "logs.export", null) {
                Column(Modifier.fillMaxWidth()) {
                    FluxActionRow(
                        title = str(if (model.isExporting) R.string.mobileLogExporting else R.string.logExportButton),
                        onClick = {
                            scope.launch {
                                model.export(
                                    errorText = { actions.errorText(it) },
                                    onShared = { overlays.toast(exportedText, FluxToastKind.Success) },
                                )
                            }
                        },
                        icon = FluxIcons.Share2,
                        loading = model.isExporting,
                        enabled = !ctx.readOnly,
                    )
                    model.failure?.let { text ->
                        FluxText(
                            text,
                            style = FluxTheme.type.sm,
                            color = c.coralText,
                            modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 10.dp),
                        )
                    }
                }
            }
        }
    }
}

/** 搜索索引（日志行）：诊断页并入；与 [LogsSection] 共用可见性判定（`log_max_size_mb` 配置未加载时无该行）。 */
internal fun logSearchEntries(ctx: SettingsSearchContext, page: SettingsPage, breadcrumb: String): List<SettingsEntry> = buildList {
    if (ctx.form.has("log_max_size_mb")) {
        add(ctx.entry("logs.maxSize", page, LOGS_ITEM_KEY, R.string.logMaxSize, R.string.logMaxSizeDesc, breadcrumb, FluxIcons.FileText))
    }
    add(ctx.entry("logs.export", page, LOGS_ITEM_KEY, R.string.logExportButton, R.string.logExportDesc, breadcrumb, FluxIcons.Share2))
}
