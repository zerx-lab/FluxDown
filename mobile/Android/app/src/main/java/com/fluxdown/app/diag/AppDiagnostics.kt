package com.fluxdown.app.diag

import android.app.ActivityManager
import android.app.ApplicationExitInfo
import android.app.usage.UsageStatsManager
import android.content.Context
import android.os.Build
import android.os.PowerManager
import android.os.Process
import androidx.annotation.RequiresApi
import android.util.Log
import com.fluxdown.app.update.ApkInstaller
import java.io.File
import java.io.IOException
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.zip.ZipEntry
import java.util.zip.ZipInputStream
import java.util.zip.ZipOutputStream

private const val TAG = "FluxDiag"

/** 事件日志上限：超过即轮转为 `.1`（两份合计 ≤ 2 × 该值）。 */
private const val EVENT_LOG_MAX_BYTES = 256L * 1024L

/** 导出时附带的历史退出记录条数（系统每应用保留有限条数）。 */
private const val EXIT_REASONS_MAX = 32

/**
 * Android 宿主侧诊断：Rust 引擎日志之外，记录进程层面的事实——前台服务启停 / 被拒 / 超时、
 * 应用更新交给哪个安装程序及其返回、Kotlin 未捕获异常，以及系统给出的历史进程退出原因（`ApplicationExitInfo`，Android 11+）。
 * 用于区分「后台消失」是内存回收、冻结、崩溃、前台服务超时还是厂商系统清理，以及更新为何没装上。
 *
 * 优先写应用专属外部目录的 `logs/diagnostics/android/`，不可写则回退内部目录。
 * 外置事件只记录状态、异常类型和代码栈，不记录异常消息；不上传。
 */
object AppDiagnostics {
    private const val DIR = "diagnostics"
    private const val EVENTS = "app-events.log"

    var externalLogRoot: File? = null
        private set

    private lateinit var privateDir: File
    @Volatile
    private var dir: File? = null
    private var sharedLogs: SharedDiagnosticLogs? = null

    /** 进程启动时调用一次：定位目录、记录启动、安装未捕获异常记录（之后交回原处理器，崩溃语义不变）。 */
    fun install(context: Context) {
        // Separate from older private event logs, which could contain exception messages.
        privateDir = File(context.noBackupFilesDir, "$DIR/redacted")
        externalLogRoot = try {
            context.getExternalFilesDir(null)?.let { File(it, "logs/diagnostics") }
        } catch (e: SecurityException) {
            Log.w(TAG, "external diagnostics storage unavailable", e)
            null
        }
        dir = externalLogRoot?.let { File(it, "android") } ?: privateDir
        val shared = SharedDiagnosticLogs(
            context.applicationContext, externalLogRoot, File(context.filesDir, "fluxdown/diagnostic-logs"),
        )
        sharedLogs = shared
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, error ->
            try {
                event(crashSummary(error))
                publishSharedLogs()
            } finally {
                previous?.uncaughtException(thread, error)
            }
        }
        event("process start pid=${Process.myPid()} sdk=${Build.VERSION.SDK_INT}")
        shared.watch()
    }

    /** Called after directory authorization on an IO thread, and synchronously on crash. */
    internal fun publishSharedLogs() {
        val shared = sharedLogs ?: return
        dir?.let(shared::publishHost)
        shared.publishNative()
    }

    /** 仅传入受控状态和计量，不传请求、用户数据或异常消息。 */
    fun event(message: String) {
        Log.i(TAG, message)
        synchronized(this) {
            val root = dir ?: return
            try {
                appendEvent(root, message)
                sharedLogs?.publishHostLater(root)
            } catch (e: IOException) {
                Log.w(TAG, "event log write failed", e)
                if (root == privateDir) return
                dir = privateDir
                try {
                    appendEvent(privateDir, message)
                    sharedLogs?.publishHostLater(privateDir)
                } catch (fallback: IOException) {
                    Log.w(TAG, "private event log write failed", fallback)
                }
            }
        }
    }

    private fun appendEvent(root: File, message: String) = try {
        if (!root.mkdirs() && !root.isDirectory) throw IOException("cannot create diagnostics directory")
        val file = File(root, EVENTS)
        if (file.length() > EVENT_LOG_MAX_BYTES) {
            val rotated = File(root, "$EVENTS.1")
            if (rotated.exists() && !rotated.delete()) throw IOException("cannot remove previous event log")
            if (!file.renameTo(rotated)) throw IOException("cannot rotate event log")
        }
        file.appendText("${timestamp(System.currentTimeMillis())} $message\n")
    } catch (e: SecurityException) {
        throw IOException("diagnostics storage access denied", e)
    }

    /**
     * 把 Android 侧诊断追加进已有 zip（引擎导出的日志包）：`android/diagnostics.txt`（设备 / 后台策略 /
     * 历史退出原因）与 `android/app-events.log*`。在 IO 线程调用。
     */
    fun appendTo(context: Context, zip: File) {
        val merged = File(zip.parentFile, "${zip.name}.tmp")
        ZipOutputStream(merged.outputStream().buffered()).use { out ->
            ZipInputStream(zip.inputStream().buffered()).use { input ->
                while (true) {
                    val entry = input.nextEntry ?: break
                    out.putNextEntry(ZipEntry(entry.name))
                    input.copyTo(out)
                    out.closeEntry()
                }
            }
            out.putNextEntry(ZipEntry("android/diagnostics.txt"))
            out.write(report(context).toByteArray())
            out.closeEntry()
            val root = dir
            if (root != null) {
                synchronized(this) {
                    for (name in listOf("$EVENTS.1", EVENTS)) {
                        val file = File(root, name)
                        if (!file.isFile) continue
                        out.putNextEntry(ZipEntry("android/$name"))
                        file.inputStream().use { it.copyTo(out) }
                        out.closeEntry()
                    }
                }
            }
        }
        if (!merged.renameTo(zip)) {
            merged.delete()
            throw IOException("cannot replace ${zip.path}")
        }
    }

    /** 纯文本报告：设备、后台限制状态、历史退出原因。 */
    fun report(context: Context): String = buildString {
        val pkg = context.packageName
        val version = try {
            context.packageManager.getPackageInfo(pkg, 0).versionName
        } catch (e: android.content.pm.PackageManager.NameNotFoundException) {
            "unknown (${e.message})"
        }
        appendLine("app: $pkg $version")
        appendLine("device: ${Build.MANUFACTURER} ${Build.MODEL} (${Build.DEVICE})")
        appendLine("rom: ${Build.DISPLAY} / ${Build.FINGERPRINT}")
        appendLine("android: ${Build.VERSION.RELEASE} (sdk ${Build.VERSION.SDK_INT})")
        // 应用更新交给「打开 APK」的默认处理者：区分系统安装器 / 用户设定的第三方安装程序 / 未设默认（选择器）
        appendLine("apkInstaller: ${ApkInstaller.defaultHandler(context)}")
        val am = context.getSystemService(ActivityManager::class.java)
        val power = context.getSystemService(PowerManager::class.java)
        appendLine("batteryOptimizationIgnored: ${power?.isIgnoringBatteryOptimizations(pkg)}")
        appendLine("powerSaveMode: ${power?.isPowerSaveMode}")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            appendLine("backgroundRestricted: ${am?.isBackgroundRestricted}")
            val bucket = context.getSystemService(UsageStatsManager::class.java)?.appStandbyBucket
            appendLine("standbyBucket: $bucket")
        }
        appendLine("lowRamDevice: ${am?.isLowRamDevice} memoryClass: ${am?.memoryClass}MB")
        appendLine()
        appendLine("== process exit reasons (newest first) ==")
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R || am == null) {
            appendLine("unavailable below Android 11")
            return@buildString
        }
        val exits = try {
            am.getHistoricalProcessExitReasons(pkg, 0, EXIT_REASONS_MAX)
        } catch (e: SecurityException) {
            appendLine("unavailable: ${e.message}")
            return@buildString
        }
        if (exits.isEmpty()) appendLine("(none recorded)")
        for (exit in exits) appendLine(describe(exit))
    }

    @RequiresApi(Build.VERSION_CODES.R)
    private fun describe(exit: ApplicationExitInfo): String {
        val reason = when (exit.reason) {
            ApplicationExitInfo.REASON_ANR -> "ANR"
            ApplicationExitInfo.REASON_CRASH -> "CRASH"
            ApplicationExitInfo.REASON_CRASH_NATIVE -> "CRASH_NATIVE"
            ApplicationExitInfo.REASON_DEPENDENCY_DIED -> "DEPENDENCY_DIED"
            ApplicationExitInfo.REASON_EXCESSIVE_RESOURCE_USAGE -> "EXCESSIVE_RESOURCE_USAGE"
            ApplicationExitInfo.REASON_EXIT_SELF -> "EXIT_SELF"
            ApplicationExitInfo.REASON_FREEZER -> "FREEZER"
            ApplicationExitInfo.REASON_INITIALIZATION_FAILURE -> "INITIALIZATION_FAILURE"
            ApplicationExitInfo.REASON_LOW_MEMORY -> "LOW_MEMORY"
            ApplicationExitInfo.REASON_OTHER -> "OTHER"
            ApplicationExitInfo.REASON_PACKAGE_STATE_CHANGE -> "PACKAGE_STATE_CHANGE"
            ApplicationExitInfo.REASON_PACKAGE_UPDATED -> "PACKAGE_UPDATED"
            ApplicationExitInfo.REASON_PERMISSION_CHANGE -> "PERMISSION_CHANGE"
            ApplicationExitInfo.REASON_SIGNALED -> "SIGNALED"
            ApplicationExitInfo.REASON_USER_REQUESTED -> "USER_REQUESTED"
            ApplicationExitInfo.REASON_USER_STOPPED -> "USER_STOPPED"
            else -> "UNKNOWN(${exit.reason})"
        }
        return "${timestamp(exit.timestamp)} pid=${exit.pid} $reason status=${exit.status} " +
            "importance=${exit.importance} pss=${exit.pss}KB rss=${exit.rss}KB desc=${exit.description.orEmpty()}"
    }

    private fun timestamp(ms: Long): String =
        SimpleDateFormat("yyyy-MM-dd HH:mm:ss.SSS Z", Locale.ROOT).format(Date(ms))

    private fun crashSummary(error: Throwable): String = buildString {
        append("uncaught exception")
        var cause: Throwable? = error
        repeat(8) {
            val current = cause ?: return@buildString
            append("\n  ").append(current.javaClass.name)
            for (frame in current.stackTrace.take(32)) {
                append("\n    at ").append(frame.className).append('.').append(frame.methodName)
                    .append(':').append(frame.lineNumber)
            }
            cause = current.cause
        }
    }
}
