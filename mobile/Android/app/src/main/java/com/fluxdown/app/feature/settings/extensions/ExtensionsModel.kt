package com.fluxdown.app.feature.settings.extensions

import android.net.Uri
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.fluxdown.app.AppContainer
import com.fluxdown.app.R
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostEvent
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.protocol.ComponentKind
import com.fluxdown.core.protocol.ComponentProgressNotice
import com.fluxdown.core.protocol.ComponentResultNotice
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.HostNotice
import com.fluxdown.core.protocol.InstalledPlugin
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.MarketAction
import com.fluxdown.core.protocol.MarketEntry
import com.fluxdown.core.protocol.PluginAutoDisabledNotice
import com.fluxdown.core.protocol.PluginDto
import com.fluxdown.core.protocol.PluginSiteDefault
import com.fluxdown.core.protocol.PluginMarket
import com.fluxdown.core.protocol.PluginPackage
import com.fluxdown.core.protocol.callJson
import com.fluxdown.core.protocol.callUnit
import com.fluxdown.core.protocol.pluginIdentityParams
import com.fluxdown.core.protocol.pluginInstallParams
import com.fluxdown.core.protocol.pluginMarketInstallParams
import com.fluxdown.core.protocol.pluginSetEnabledParams
import com.fluxdown.core.protocol.pluginSetSiteDefaultParams
import com.fluxdown.core.protocol.str
import com.fluxdown.core.protocol.strOrNull
import com.fluxdown.fluxui.overlay.FluxToastKind
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

/**
 * S11 扩展页的状态与操作（Web `PluginsTab` / `MarketSection` 的逻辑层）。
 *
 * 插件列表本身来自 `daemon.plugins` 分区（事件驱动），这里只持有进行中的操作、市场目录与待确认弹窗。
 * 每个主机一个实例（[ExtensionsModels]），扩展页 / 市场页 / 详情页共用；操作在应用级作用域里运行，
 * 离开页面不会中断安装。
 */
@Stable
internal class ExtensionsModel(val container: AppContainer, val hostId: String) {
    enum class Tab { Plugins, Components }

    sealed interface InstallPhase {
        data class Uploading(val fraction: Float) : InstallPhase
        data object Installing : InstallPhase
    }

    sealed interface MarketPhase {
        data object Idle : MarketPhase
        data object Loading : MarketPhase
        data object Loaded : MarketPhase
        data class Failed(val message: String) : MarketPhase
    }

    /** 权限确认请求（新装列全部权限；更新只列新增权限）。 */
    class PermissionRequest(
        val entry: MarketEntry,
        val installed: PluginDto?,
        /** 确认期间市场里的版本变了：按最新版本重新确认。 */
        val versionChanged: Boolean = false,
    ) {
        val isUpdate: Boolean get() = installed != null
        val permissions: List<String> get() = PluginMarket.permissionsToConfirm(entry, installed)
    }

    var tab by mutableStateOf(Tab.Plugins)

    /** 进行中的插件操作（identity）。 */
    var busy by mutableStateOf(emptySet<String>())
        private set
    var installPhase by mutableStateOf<InstallPhase?>(null)
        private set

    /** 安装 / 重载成功但缺基础组件：弹窗提醒（提醒式，不阻断）。 */
    var missingComponents by mutableStateOf<List<String>?>(null)

    private var conflictNotices by mutableStateOf(emptyList<List<ResolverUrlOverlap>>())
    val conflictNotice: List<ResolverUrlOverlap>? get() = conflictNotices.firstOrNull()

    fun dismissConflictNotice() {
        conflictNotices = conflictNotices.drop(1)
    }

    // 市场
    var marketPhase by mutableStateOf<MarketPhase>(MarketPhase.Idle)
        private set
    var marketEntries by mutableStateOf<List<MarketEntry>>(emptyList())
        private set
    var marketPending by mutableStateOf(emptySet<String>())
        private set
    var permissionRequest by mutableStateOf<PermissionRequest?>(null)

    /** 市场里每个插件的最新条目。 */
    val marketLatest: List<MarketEntry> by derivedStateOf { PluginMarket.latestPerPlugin(marketEntries) }

    private var marketRequested = false

    private val session get() = container.session

    // ───────────── 组件 ─────────────

    private val controllers = HashMap<String, ComponentController>()

    fun controller(kind: ComponentKind): ComponentController =
        controllers.getOrPut(kind.wire) { ComponentController(kind, container) }

    /** 组件进度 / 结果通知常驻分发（也包含其他客户端发起的安装），模型被换掉时取消。 */
    private val noticeJob = container.appScope.launch {
        container.store.notices.collect { dispatch(it) }
    }

    fun close() {
        noticeJob.cancel()
    }

    private fun dispatch(notice: HostEvent.Notice) {
        when (notice.name) {
            HostNotice.componentProgress ->
                ComponentProgressNotice.fromJson(Json.parseOrNull(notice.json))?.let { controller(it.component).onProgress(it) }
            HostNotice.componentResult ->
                ComponentResultNotice.fromJson(Json.parseOrNull(notice.json))?.let { controller(it.component).onResult(it) }
        }
    }

    // ───────────── 已安装插件 ─────────────

    private fun launchBusy(identity: String, block: suspend () -> Unit) {
        if (identity in busy) return
        busy = busy + identity
        container.appScope.launch {
            try {
                block()
            } finally {
                busy = busy - identity
            }
        }
    }

    fun setEnabled(env: ExtensionsEnv, plugin: PluginDto, enabled: Boolean) = launchBusy(plugin.identity) {
        val target = session
        try {
            target.callUnit(HostMethod.daemonPluginSetEnabled, pluginSetEnabledParams(plugin.identity, enabled))
            if (enabled) noteResolverConflicts(target, plugin.identity)
        } catch (e: HostException) {
            env.toast(env.text(R.string.pluginOpEnabledFailed, "message" to env.error(e)), FluxToastKind.Error)
        }
    }

    fun uninstall(env: ExtensionsEnv, plugin: PluginDto, onDone: () -> Unit = {}) = launchBusy(plugin.identity) {
        try {
            session.callUnit(HostMethod.daemonPluginUninstall, pluginIdentityParams(plugin.identity))
            env.toast(env.text(R.string.pluginOpUninstallSuccess), FluxToastKind.Success)
            onDone()
        } catch (e: HostException) {
            env.toast(env.text(R.string.pluginOpUninstallFailed, "message" to env.error(e)), FluxToastKind.Error)
        }
    }

    fun reload(env: ExtensionsEnv, plugin: PluginDto) = launchBusy(plugin.identity) {
        try {
            val result = session.callJson(HostMethod.daemonPluginReloadDev, pluginIdentityParams(plugin.identity))
            env.toast(env.text(R.string.pluginOpReloadSuccess), FluxToastKind.Success)
            noteMissing(InstalledPlugin.fromJson(result))
        } catch (e: HostException) {
            env.showPluginFailure(R.string.pluginOpReloadFailed, e)
        }
    }

    private fun noteMissing(result: InstalledPlugin?) {
        val missing = result?.missingComponents.orEmpty()
        if (missing.isNotEmpty()) missingComponents = missing
    }

    private suspend fun noteResolverConflicts(target: HostSession, identity: String) {
        if (identity.isEmpty() || target !== session || container.host.value.id != hostId) return
        // The PluginsChanged projection may still be behind the mutation response.
        val plugins = try {
            PluginDto.listFromJson(target.callJson(HostMethod.daemonPluginList))
        } catch (_: HostException) {
            // A failed read does not turn a successful mutation into a failure.
            return
        }
        if (target !== session || container.host.value.id != hostId) return
        val conflicts = resolverUrlConflicts(plugins, setOf(identity))
        if (conflicts.isNotEmpty()) conflictNotices = conflictNotices + listOf(conflicts)
    }

    // ───────────── 站点默认插件 ─────────────

    /** `daemon.plugin.siteDefaults`（按 site 升序）；非空才渲染「站点默认插件」区块。 */
    var siteDefaults by mutableStateOf<List<PluginSiteDefault>>(emptyList())
        private set
    var siteDefaultBusy by mutableStateOf(emptySet<String>())
        private set

    /** 打开扩展页与每次修改后重新拉取；失败（含主机不支持）保持上次列表，不打扰用户。 */
    fun refreshSiteDefaults() {
        container.appScope.launch { fetchSiteDefaults() }
    }

    private suspend fun fetchSiteDefaults() {
        try {
            siteDefaults = PluginSiteDefault.listFromJson(session.callJson(HostMethod.daemonPluginSiteDefaults))
        } catch (e: HostException) {
            // 不支持 / 暂不可达：保留现状
        }
    }

    /** [identity] = null → 删除条目；非 null → 确认 / 更换默认插件。 */
    fun setSiteDefault(env: ExtensionsEnv, site: String, identity: String?) {
        if (site in siteDefaultBusy) return
        siteDefaultBusy = siteDefaultBusy + site
        container.appScope.launch {
            try {
                session.callUnit(HostMethod.daemonPluginSetSiteDefault, pluginSetSiteDefaultParams(site, identity))
            } catch (e: HostException) {
                env.toast(env.text(R.string.pluginSiteDefaultSaveFailed, "error" to env.error(e)), FluxToastKind.Error)
            } finally {
                fetchSiteDefaults()
                siteDefaultBusy = siteDefaultBusy - site
            }
        }
    }

    // ───────────── 文件安装（远端主机） ─────────────

    /**
     * 选中的 `.fxplug` / `.zip`（仅远端主机）：读入内存（上限 [PluginPackage.MAX_BYTES]）→ 上传 blob
     * （`POST /api/web/blobs/plugins`，Bearer 访问密钥）→ `daemon.plugin.install`。
     */
    fun installFile(env: ExtensionsEnv, uri: Uri) {
        if (installPhase != null) return
        installPhase = InstallPhase.Installing
        container.appScope.launch {
            val target = session
            try {
                val data = readPackage(env, uri)
                val result = installRemote(target, data)
                env.toast(env.text(R.string.pluginOpInstallSuccess), FluxToastKind.Success)
                noteResolverConflicts(target, result.identity)
                noteMissing(result)
            } catch (e: HostException) {
                env.showPluginFailure(R.string.pluginOpInstallFailed, e)
            } finally {
                installPhase = null
            }
        }
    }

    private suspend fun readPackage(env: ExtensionsEnv, uri: Uri): ByteArray = withContext(Dispatchers.IO) {
        try {
            val input = env.context.contentResolver.openInputStream(uri)
                ?: throw HostException(HostErrorCode.InvalidArgument, message = "cannot open file")
            input.use { stream ->
                val out = ByteArrayOutputStream()
                val buffer = ByteArray(64 * 1024)
                while (true) {
                    ensureActive()
                    val n = stream.read(buffer)
                    if (n < 0) break
                    out.write(buffer, 0, n)
                    if (out.size() > PluginPackage.MAX_BYTES) {
                        throw HostException(HostErrorCode.InvalidArgument, reason = "pluginPackageTooLarge", message = "package exceeds 10 MiB")
                    }
                }
                out.toByteArray()
            }
        } catch (e: IOException) {
            throw HostException(HostErrorCode.InvalidArgument, message = e.message, cause = e)
        } catch (e: SecurityException) {
            throw HostException(HostErrorCode.InvalidArgument, message = e.message, cause = e)
        }
    }

    private suspend fun installRemote(target: HostSession, data: ByteArray): InstalledPlugin {
        if (target !== session || container.host.value.id != hostId) {
            throw HostException(HostErrorCode.Unavailable, message = "host changed")
        }
        val ref = container.host.value as? HostRef.Remote
            ?: throw HostException(HostErrorCode.InvalidArgument, message = "not a remote host")
        val key = container.remoteAccessKey(ref.id)
            ?: throw HostException(HostErrorCode.Unauthorized, message = "access key unavailable")
        val url = PluginPackage.uploadUrl(ref.endpoint)
            ?: throw HostException(HostErrorCode.InvalidArgument, message = "invalid host address")
        installPhase = InstallPhase.Uploading(0f)
        val blobId = PluginBlobUploader.upload(data, url, key) { fraction ->
            if (installPhase is InstallPhase.Uploading) installPhase = InstallPhase.Uploading(fraction)
        }
        installPhase = InstallPhase.Installing
        val result = target.callJson(HostMethod.daemonPluginInstall, pluginInstallParams(blobId))
        return InstalledPlugin.fromJson(result) ?: InstalledPlugin("")
    }

    // ───────────── 市场 ─────────────

    /** 首次进入且连接就绪时拉取一次（GPUI `ensure_market_loaded`）。 */
    fun ensureMarketLoaded(env: ExtensionsEnv) {
        if (marketRequested || container.store.state.value.isReadOnly) return
        loadMarket(env)
    }

    fun loadMarket(env: ExtensionsEnv) {
        marketRequested = true
        if (marketPhase == MarketPhase.Loading) return
        marketPhase = MarketPhase.Loading
        container.appScope.launch { fetchMarket(env) }
    }

    private suspend fun fetchMarket(env: ExtensionsEnv) {
        try {
            marketEntries = MarketEntry.listFromJson(session.callJson(HostMethod.daemonPluginMarketList))
            marketPhase = MarketPhase.Loaded
        } catch (e: HostException) {
            val message = env.error(e)
            marketPhase = MarketPhase.Failed(message)
            // 已有目录时覆盖层不显示失败态：用 Toast 告知刷新失败。
            if (marketEntries.isNotEmpty()) {
                env.toast(env.text(R.string.marketLoadFailed, "message" to message), FluxToastKind.Error)
            }
        }
    }

    /** 已安装插件的可用更新（市场目录已加载时）。 */
    fun updateFor(plugin: PluginDto): MarketEntry? {
        val entry = marketLatest.firstOrNull { it.pluginId == plugin.identity } ?: return null
        return entry.takeIf { PluginMarket.action(it, plugin) == MarketAction.Update }
    }

    fun requestInstall(env: ExtensionsEnv, entry: MarketEntry, installed: PluginDto?) {
        if (entry.pluginId in marketPending) return
        val request = PermissionRequest(entry, installed)
        if (request.permissions.isEmpty()) {
            performInstall(env, request)
        } else {
            permissionRequest = request
        }
    }

    fun confirmInstall(env: ExtensionsEnv, request: PermissionRequest) {
        permissionRequest = null
        performInstall(env, request)
    }

    private fun performInstall(env: ExtensionsEnv, request: PermissionRequest) {
        val entry = request.entry
        if (entry.pluginId in marketPending) return
        marketPending = marketPending + entry.pluginId
        container.appScope.launch {
            val target = session
            try {
                val result = target.callJson(
                    HostMethod.daemonPluginMarketInstall,
                    pluginMarketInstallParams(entry.pluginId, entry.version),
                )
                env.toast(
                    env.text(if (request.isUpdate) R.string.pluginOpUpdateSuccess else R.string.pluginOpInstallSuccess),
                    FluxToastKind.Success,
                )
                val installed = InstalledPlugin.fromJson(result)
                noteResolverConflicts(target, installed?.identity ?: entry.pluginId)
                noteMissing(installed)
            } catch (e: HostException) {
                // 用户确认的版本已不是最新：刷新目录，按新版本重新确认权限。
                if (e.reason == "marketVersionChanged") {
                    marketPhase = MarketPhase.Loading
                    fetchMarket(env)
                    val fresh = marketLatest.firstOrNull { it.pluginId == entry.pluginId }
                    if (fresh != null && fresh.installable) {
                        val next = PermissionRequest(fresh, request.installed, versionChanged = true)
                        if (next.permissions.isNotEmpty()) {
                            permissionRequest = next
                            return@launch
                        }
                    }
                }
                val res = if (request.isUpdate) R.string.pluginOpUpdateFailed else R.string.pluginOpInstallFailed
                env.showPluginFailure(res, e)
            } finally {
                marketPending = marketPending - entry.pluginId
            }
        }
    }
}

/** 每个主机一个 [ExtensionsModel]（切换主机即换新实例，市场目录 / 进行中状态不串主机）。 */
internal object ExtensionsModels {
    private var cached: ExtensionsModel? = null

    @Synchronized
    fun of(container: AppContainer, hostId: String): ExtensionsModel {
        cached?.takeIf { it.hostId == hostId && it.container === container }?.let { return it }
        cached?.close()
        return ExtensionsModel(container, hostId).also { cached = it }
    }
}

@Composable
internal fun rememberExtensionsModel(): ExtensionsModel {
    val container = LocalAppContainer.current
    val host by container.host.collectAsStateWithLifecycle()
    return remember(container, host.id) { ExtensionsModels.of(container, host.id) }
}

// ───────────────────────────── 远端 blob 上传 ─────────────────────────────

/** `POST /api/web/blobs/plugins`：远端 `--server` 主机的文件面（Bearer 访问密钥，原始字节）→ `blobId`。 */
internal object PluginBlobUploader {
    private const val CHUNK = 32 * 1024
    private const val CONNECT_TIMEOUT_MS = 15_000
    private const val READ_TIMEOUT_MS = 120_000

    suspend fun upload(data: ByteArray, url: String, accessKey: String, onProgress: (Float) -> Unit): String =
        withContext(Dispatchers.IO) {
            val conn = try {
                URL(url).openConnection() as HttpURLConnection
            } catch (e: IOException) {
                throw HostException(HostErrorCode.InvalidArgument, message = "invalid host address", cause = e)
            }
            try {
                conn.requestMethod = "POST"
                conn.doOutput = true
                conn.connectTimeout = CONNECT_TIMEOUT_MS
                conn.readTimeout = READ_TIMEOUT_MS
                conn.setFixedLengthStreamingMode(data.size)
                conn.setRequestProperty("Authorization", "Bearer $accessKey")
                conn.setRequestProperty("Content-Type", "application/octet-stream")
                conn.outputStream.use { out ->
                    var offset = 0
                    while (offset < data.size) {
                        ensureActive()
                        val n = minOf(CHUNK, data.size - offset)
                        out.write(data, offset, n)
                        offset += n
                        onProgress(offset.toFloat() / data.size)
                    }
                }
                val code = conn.responseCode
                val stream = if (code in 200..299) conn.inputStream else conn.errorStream
                val body = stream?.use { it.readBytes().toString(Charsets.UTF_8) }
                val reply = Json.parseOrNull(body)
                val blobId = reply.strOrNull("blobId")
                if (code in 200..299 && blobId != null) return@withContext blobId
                val message = reply.str("error").ifEmpty { "HTTP $code" }
                throw when (code) {
                    401, 403 -> HostException(HostErrorCode.Unauthorized, message = message)
                    413 -> HostException(HostErrorCode.InvalidArgument, reason = "pluginPackageTooLarge", message = message)
                    in 400..499 -> HostException(HostErrorCode.InvalidArgument, message = message)
                    else -> HostException(HostErrorCode.Unavailable, retryable = true, message = message)
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: IOException) {
                throw HostException(HostErrorCode.Unavailable, retryable = true, message = e.message, cause = e)
            } finally {
                conn.disconnect()
            }
        }
}

// ───────────────────────────── 熔断自动禁用提示 ─────────────────────────────

/**
 * `PluginAutoDisabled` 一次性通知 → toast（`pluginAutoDisabledToast`）。
 * 只处理挂载之后到达的通知（SharedFlow 不重放历史）；挂在扩展相关页面上即得到提示。
 */
@Composable
internal fun PluginAutoDisabledEffect(env: ExtensionsEnv) {
    val container = LocalAppContainer.current
    val plugins by rememberPlugins()
    val latest by androidx.compose.runtime.rememberUpdatedState(plugins)
    LaunchedEffect(container) {
        container.store.notices.collect { notice ->
            if (notice.name != HostNotice.pluginAutoDisabled) return@collect
            val payload = PluginAutoDisabledNotice.fromJson(Json.parseOrNull(notice.json)) ?: return@collect
            val name = latest.firstOrNull { it.identity == payload.identity }?.name ?: payload.identity
            val toastKey = if (payload.reason == "Revoked") R.string.pluginRevokedToast else R.string.pluginAutoDisabledToast
            env.toast(env.text(toastKey, "name" to name), FluxToastKind.Warn)
        }
    }
}
