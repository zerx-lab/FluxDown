package com.fluxdown.app.feature.settings.diagnostics

import android.app.ActivityManager
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.os.StatFs
import com.fluxdown.core.format.Format
import android.util.Log
import com.fluxdown.app.AppContainer
import com.fluxdown.app.ui.defaultLocalSaveDir
import com.fluxdown.core.host.HostException
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.protocol.EngineState
import com.fluxdown.core.protocol.LinkHandler
import com.fluxdown.core.protocol.LinkProbe
import com.fluxdown.core.protocol.MobileCheck
import com.fluxdown.core.protocol.MobileChecks
import com.fluxdown.core.protocol.MobileNetworkSnapshot
import com.fluxdown.core.protocol.NotificationState
import com.fluxdown.core.store.Connection
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.IOException
import java.net.Inet4Address
import java.net.Inet6Address
import java.util.UUID

private const val TAG = "FluxDiagnostics"

/** 收集全部「此设备」检查（探测在 IO 线程，系统服务读取都是同步快速调用）。 */
internal object MobileCheckRunner {
    suspend fun run(context: Context, container: AppContainer, errorText: (HostException) -> String): List<MobileCheck> {
        val app = context.applicationContext
        val state = container.store.state.value
        val isLocal = container.host.value is HostRef.Local
        return withContext(Dispatchers.IO) {
            val dir = storageDir(app, state.config["default_save_dir"].orEmpty(), isLocal)
            val writable = isWritable(dir)
            // 本机引擎自己量的剩余空间优先；远端主机的 `diskFreeBytes` 是服务器磁盘，不能算作这台手机的。
            val free = (if (isLocal) state.stats.diskFreeBytes else null) ?: freeBytes(dir)
            val connection = state.connection
            listOf(
                MobileChecks.notifications(notificationState(app)),
                background(app),
                MobileChecks.storage(writable, free) { Format.bytes(it).toString() },
                links(app),
                MobileChecks.network(networkSnapshot(app)),
                MobileChecks.engine(
                    state = when (connection) {
                        Connection.Live -> EngineState.Live
                        Connection.Connecting -> EngineState.Connecting
                        Connection.Stale -> EngineState.Stale
                        is Connection.Failed -> EngineState.Failed
                    },
                    version = state.info?.serviceVersion?.takeIf { it.isNotEmpty() },
                    protocol = state.info?.protocolVersion,
                    failureText = (connection as? Connection.Failed)?.let { errorText(it.error) },
                ),
            )
        }
    }

    // ───────────────────────────── 通知 / 后台 ─────────────────────────────

    private fun notificationState(context: Context): NotificationState {
        val nm = context.getSystemService(NotificationManager::class.java)
        return when {
            !nm.areNotificationsEnabled() -> NotificationState.Disabled
            nm.notificationChannels.any { it.importance == NotificationManager.IMPORTANCE_NONE } -> NotificationState.ChannelsBlocked
            else -> NotificationState.Enabled
        }
    }

    private fun background(context: Context): MobileCheck {
        val power = context.getSystemService(PowerManager::class.java)
        val activity = context.getSystemService(ActivityManager::class.java)
        return MobileChecks.background(
            // Android 9 以下没有「后台限制」开关，视为未限制
            backgroundRestricted = Build.VERSION.SDK_INT >= Build.VERSION_CODES.P && activity.isBackgroundRestricted,
            batteryUnrestricted = power.isIgnoringBatteryOptimizations(context.packageName),
            powerSave = power.isPowerSaveMode,
        )
    }

    // ───────────────────────────── 存储 ─────────────────────────────

    /** 本机主机检查实际配置的保存目录；远端主机的目录在服务器上，改查手机上的默认保存目录。 */
    private fun storageDir(context: Context, configured: String, isLocal: Boolean): File {
        val path = configured.trim()
        return if (isLocal && path.isNotEmpty() && File(path).isAbsolute) File(path) else defaultLocalSaveDir(context)
    }

    /** 真实写入探测：在目录里建一个临时文件再删除。 */
    private fun isWritable(dir: File): Boolean {
        if (!dir.isDirectory && !dir.mkdirs()) return false
        val probe = File(dir, ".fluxdown-probe-${UUID.randomUUID()}")
        try {
            probe.writeText("ok")
        } catch (e: IOException) {
            Log.i(TAG, "write probe failed: ${e.message}")
            return false
        }
        if (!probe.delete()) Log.i(TAG, "write probe cleanup failed: ${probe.path}")
        return true
    }

    /** 目录（或其最近的已存在祖先）所在卷的可用字节；取不到 → null。 */
    private fun freeBytes(dir: File): Long? {
        var probe: File? = dir
        while (probe != null && !probe.exists()) probe = probe.parentFile
        val target = probe ?: return null
        return try {
            StatFs(target.path).availableBytes
        } catch (e: IllegalArgumentException) {
            Log.i(TAG, "free space query failed: ${e.message}")
            null
        }
    }

    // ───────────────────────────── 链接 ─────────────────────────────

    private fun links(context: Context): MobileCheck {
        val pm = context.packageManager
        val own = context.packageName
        val magnet = Intent(Intent.ACTION_VIEW, Uri.parse("magnet:?xt=urn:btih:0000000000000000000000000000000000000000"))
        val ed2k = Intent(Intent.ACTION_VIEW, Uri.parse("ed2k://|file|probe|0|00000000000000000000000000000000|/"))
        val torrent = Intent(Intent.ACTION_VIEW).setDataAndType(Uri.parse("content://fluxdown.probe/probe.torrent"), "application/x-bittorrent")
        return MobileChecks.links(probeLink(pm, own, magnet), probeLink(pm, own, ed2k), probeLink(pm, own, torrent))
    }

    /**
     * 谁处理这个 Intent：先看本应用有没有声明；再看系统默认处理者（`resolveActivity`）：
     * 是本应用 → [LinkHandler.This]；是系统选择器（多个候选且未设默认）→ [LinkHandler.Ask]；是别的应用 → [LinkHandler.Other]。
     * 其它应用的可见性依赖 manifest 的 `<queries>`（magnet / ed2k / torrent）。
     */
    private fun probeLink(pm: PackageManager, own: String, intent: Intent): LinkProbe {
        val candidates = queryActivities(pm, intent)
        if (candidates.none { it.activityInfo.packageName == own }) return LinkProbe(LinkHandler.Missing)
        val resolved = resolveActivity(pm, intent)?.activityInfo ?: return LinkProbe(LinkHandler.Ask)
        return when {
            resolved.packageName == own -> LinkProbe(LinkHandler.This)
            isSystemResolver(resolved.packageName, resolved.name) -> LinkProbe(LinkHandler.Ask)
            else -> LinkProbe(LinkHandler.Other, resolved.packageName)
        }
    }

    private fun isSystemResolver(pkg: String, cls: String): Boolean =
        pkg == "android" || pkg == "com.android.intentresolver" || cls.endsWith("ResolverActivity") || cls.endsWith("ChooserActivity")

    private fun queryActivities(pm: PackageManager, intent: Intent) =
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            pm.queryIntentActivities(intent, PackageManager.ResolveInfoFlags.of(PackageManager.MATCH_DEFAULT_ONLY.toLong()))
        } else {
            @Suppress("DEPRECATION")
            pm.queryIntentActivities(intent, PackageManager.MATCH_DEFAULT_ONLY)
        }

    private fun resolveActivity(pm: PackageManager, intent: Intent) =
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            pm.resolveActivity(intent, PackageManager.ResolveInfoFlags.of(PackageManager.MATCH_DEFAULT_ONLY.toLong()))
        } else {
            @Suppress("DEPRECATION")
            pm.resolveActivity(intent, PackageManager.MATCH_DEFAULT_ONLY)
        }

    // ───────────────────────────── 网络 ─────────────────────────────

    private fun networkSnapshot(context: Context): MobileNetworkSnapshot? {
        val cm = context.getSystemService(ConnectivityManager::class.java) ?: return null
        val dataSaver = cm.restrictBackgroundStatus == ConnectivityManager.RESTRICT_BACKGROUND_STATUS_ENABLED
        val network = cm.activeNetwork
        val caps = network?.let { cm.getNetworkCapabilities(it) }
        if (network == null || caps == null || !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)) {
            return MobileNetworkSnapshot(
                MobileNetworkSnapshot.Reachability.Unsatisfied, MobileNetworkSnapshot.Transport.None, dataSaver = dataSaver,
            )
        }
        val reachability = if (caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)) {
            MobileNetworkSnapshot.Reachability.Satisfied
        } else {
            MobileNetworkSnapshot.Reachability.Unvalidated
        }
        val transport = when {
            caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> MobileNetworkSnapshot.Transport.Wifi
            caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> MobileNetworkSnapshot.Transport.Cellular
            caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> MobileNetworkSnapshot.Transport.Wired
            else -> MobileNetworkSnapshot.Transport.Other
        }
        val addresses = cm.getLinkProperties(network)?.linkAddresses.orEmpty().map { it.address }
        return MobileNetworkSnapshot(
            reachability = reachability,
            transport = transport,
            isMetered = !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED),
            dataSaver = dataSaver,
            supportsIPv4 = addresses.any { it is Inet4Address },
            supportsIPv6 = addresses.any { it is Inet6Address && !it.isLinkLocalAddress && !it.isLoopbackAddress },
        )
    }
}
