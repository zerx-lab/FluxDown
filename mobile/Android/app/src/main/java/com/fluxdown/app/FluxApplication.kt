package com.fluxdown.app

import android.app.Application
import android.content.Context
import android.os.Build
import android.provider.Settings
import android.util.Log
import androidx.compose.runtime.snapshots.SnapshotStateSet
import androidx.datastore.preferences.preferencesDataStore
import com.fluxdown.app.data.AppearanceRepo
import com.fluxdown.app.data.AppearanceState
import com.fluxdown.app.data.DeviceSettings
import com.fluxdown.app.data.HostRepo
import com.fluxdown.app.data.SecretBox
import com.fluxdown.app.data.ViewPrefsRepo
import com.fluxdown.app.feature.devices.LinkApi
import com.fluxdown.app.feature.newtask.ImportCenter
import com.fluxdown.app.service.LocalActivity
import com.fluxdown.app.diag.AppDiagnostics
import com.fluxdown.app.service.DownloadServiceController
import com.fluxdown.bridge.FluxBridge
import com.fluxdown.core.format.Format
import com.fluxdown.core.host.CreateTaskRequest
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.host.HostSignal
import com.fluxdown.core.host.ImportBatch
import com.fluxdown.core.host.ImportJob
import com.fluxdown.core.host.ImportSummary
import com.fluxdown.core.host.ImportUpdate
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.model.SelectionOutcome
import com.fluxdown.core.store.Connection
import com.fluxdown.core.store.HostState
import com.fluxdown.core.store.HostStore
import com.fluxdown.app.update.AppUpdateController
import com.fluxdown.app.update.UpdateCheckJobService
import com.fluxdown.fluxui.theme.FluxFonts
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.plus
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import java.io.File

private val Context.prefs by preferencesDataStore(name = "fluxdown")

class FluxApplication : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        AppDiagnostics.install(this)
        DownloadServiceController.clearOrphanedTaskNotifications(this)
        container = AppContainer(this)
        // 本机出现下载活动时就尝试拉起前台服务——不限于界面在前台：RSS 自动下载、云端 / 局域网下发、
        // 队列调度都可能在后台产生任务。Android 12+ 后台启动多数会被拒（已在电池优化白名单时豁免），
        // 被拒记入诊断，回到前台由 DownloadServiceEffect 补拉。
        DownloadServiceController.keepAliveWhileBusy(this, container)
        // 周期后台检查按「自动检查」开关登记 / 取消（幂等；不持久化，每次冷启动重登）。
        // 偏好读取与 JobScheduler binder 调用放 IO，不占冷启动主线程。
        container.appScope.launch(Dispatchers.IO) {
            UpdateCheckJobService.sync(this@FluxApplication, DeviceSettings.of(this@FluxApplication).updateAutoCheck)
        }
        // 冷启动在后台解析字体文件，避免首帧在主线程加载 ~5MB 字体
        container.appScope.launch(Dispatchers.IO) { FluxFonts.preload(this@FluxApplication) }
    }
}

/**
 * 进程级依赖：主机会话、状态仓库、设备本地偏好、主机列表。
 *
 * 主机 = 本机（`:bridge` 经 UniFFI 在进程内跑 daemon + agent，冷启动即打开）或已保存的远端
 * `fluxdown-agent --server`（`/rpc` + 访问密钥，密钥由 [SecretBox] 加密落盘）。
 * UI 只依赖 [HostSession] 端口与 [HostStore]；切换主机时 store 先 detach，再关旧会话，再 attach 新会话。
 */
class AppContainer(context: Context) {
    private val appContext = context.applicationContext

    val appScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    /** 正在抓取的订阅 id（订阅列表与条目流共用：两页都显示抓取中状态）。 */
    val rssFetching = SnapshotStateSet<String>()

    /** HostStore 要求串行调度（工作副本单线程访问）。 */
    private val storeScope = CoroutineScope(SupervisorJob() + Dispatchers.Default.limitedParallelism(1))

    val appearance = AppearanceRepo(context.prefs)
    /** null until DataStore loads; never publish a legacy placeholder for a saved IEC preference. */
    val appearanceState: StateFlow<AppearanceState?> = appearance.state
        .onEach { Format.setFileSizeUnits(it.fileSizeUnits) }
        .stateIn(appScope, SharingStarted.Eagerly, null)
    val viewPrefs = ViewPrefsRepo(context.prefs)

    /** 批量导入（进程级：关闭「新建下载」Sheet 后仍继续，进度显示在下载页）。 */
    val imports = ImportCenter(appScope)
    val store = HostStore(storeScope)

    /** 应用自更新（惰性创建：首次用到才加载更新器；与当前主机无关）。 */
    val updates: AppUpdateController by lazy {
        AppUpdateController(appContext, appScope, DeviceSettings.of(appContext)) {
            localActivity.value.let { it.active + it.pending + it.retryPending }
        }
    }

    private val hostRepo = HostRepo(context.prefs, SecretBox())
    private val localRef = HostRef.Local(displayName = Build.MODEL.orEmpty().ifBlank { "Android" })

    /** 本机 + 已保存的远端主机（本机恒为首项）。 */
    val hosts: StateFlow<List<HostRef>> = hostRepo.remotes
        .map { remotes -> listOf<HostRef>(localRef) + remotes }
        .stateIn(appScope, SharingStarted.Eagerly, listOf(localRef))

    private val _host = MutableStateFlow<HostRef>(localRef)

    /** 当前主机（打开期间 / 打开失败时仍是目标主机；连接状态看 [store]）。 */
    val host: StateFlow<HostRef> = _host.asStateFlow()

    /** 当前会话；本机尚未打开或打开失败时是 [UnavailableSession]（命令抛 [HostException]）。 */
    @Volatile
    var session: HostSession = UnavailableSession(HostException(HostErrorCode.Unavailable, message = "host starting"))
        private set

    private val _localState = MutableStateFlow(HostState())

    /**
     * 本机主机的状态投影（无论当前选中哪个主机）：当前就是本机时即 [store] 的状态；切到远端时来自进程内
     * 本机主机的第二个会话（独立 [HostStore]）。进度通知、前台服务保活与本机任务的完成通知都读它。
     */
    val localState: StateFlow<HostState> = _localState.asStateFlow()

    /** 本机引擎活动量：前台服务据此保活（远端主机在前台时，进程内的本机下载仍可能在跑，进程不能被回收）。 */
    val localActivity: StateFlow<LocalActivity> = _localState
        .map { LocalActivity.from(it) }
        .flowOn(Dispatchers.Default)
        .stateIn(appScope, SharingStarted.Eagerly, LocalActivity.Idle)

    private var localMonitor: Job? = null

    private val switchLock = Mutex()
    private val startupRestore = appScope.async(start = CoroutineStart.LAZY) { performHostRestore() }
    private var restoreFailureShown = false
    private var restoreSuperseded = false

    internal fun supersedeHostRestore() {
        restoreSuperseded = true
    }

    /** 界面重建只取消等待；恢复属于进程，并且失败只提示一次。 */
    internal suspend fun restoreLastHost(): String? {
        val name = startupRestore.await() ?: return null
        if (restoreSuperseded || restoreFailureShown) return null
        restoreFailureShown = true
        return name
    }

    /** 首次前台显示时尝试恢复；远端 I/O 不占用切换锁。 */
    private suspend fun performHostRestore(): String? {
        if (restoreSuperseded) return null
        val ref = try {
            hostRepo.lastRemote()
        } catch (e: java.io.IOException) {
            Log.w(TAG, "read last host failed", e)
            null
        } ?: return null
        if (restoreSuperseded) return null
        val opened = openOrFail(ref)
        val next = opened.getOrNull()
        var adopted = false
        try {
            return switchLock.withLock {
                if (restoreSuperseded) return@withLock null
                if (next == null) return@withLock ref.displayName
                adopt(ref, next)
                adopted = true
                null
            }
        } finally {
            if (!adopted) next?.close()
        }
    }

    init {
        appScope.launch {
            switchLock.withLock {
                val opened = openOrFail(localRef)
                adopt(localRef, opened.getOrElse { UnavailableSession(it.asHostException()) })
            }
        }
        // 连接 / 任务表载入状态的每次变化记入诊断事件：界面卡在加载 / 断连时，导出的日志能看出停在哪一态。
        // 只在状态切换时写一行（StateFlow 的 10 Hz 发布经 distinctUntilChanged 折叠），不做周期写入。
        appScope.launch(Dispatchers.IO) {
            store.state.map(::connectionSummary).distinctUntilChanged().collect { AppDiagnostics.event("host $it") }
        }
    }

    /**
     * 切换主机。打开失败时保持原主机不变并返回失败（[HostException]）。
     * 切到本机以外时本机引擎继续在进程内运行（由前台服务保活）。
     */
    suspend fun switchHost(ref: HostRef, remember: Boolean = true): Result<Unit> {
        supersedeHostRestore()
        return switchLock.withLock {
            val result = if (ref.id == _host.value.id && session !is UnavailableSession) {
                Result.success(Unit)
            } else {
                openOrFail(ref).map { adopt(ref, it) }
            }
            if (remember && result.isSuccess) {
                try {
                    hostRepo.rememberRemote((ref as? HostRef.Remote)?.id)
                } catch (e: java.io.IOException) {
                    Log.w(TAG, "remember last host failed", e)
                }
            }
            result
        }
    }

    /**
     * 保存远端主机：先真实连接并拿到首个快照（鉴权 / 协议 / 可达性），通过后加密保存凭据。
     * [accessKey] 是手输的访问密钥，或局域网配对得到的设备控制凭据（此时 [linkFingerprint] = 主机指纹；
     * 已有同指纹的主机则原地更新，不新增一条）。
     */
    suspend fun addRemoteHost(
        name: String,
        endpoint: String,
        accessKey: String,
        linkFingerprint: String? = null,
    ): Result<HostRef.Remote> {
        val url = endpoint.trim()
        val key = accessKey.trim()
        return try {
            if (url.isEmpty() || key.isEmpty()) {
                throw HostException(HostErrorCode.InvalidArgument, message = "endpoint and access key are required")
            }
            val probe = FluxBridge.openRemote(url, key)
            awaitFirstSnapshot(probe)
            val saved = hostRepo.add(name.trim().ifEmpty { url.substringAfter("://") }, url, key, linkFingerprint)
            // 同一台主机重新配对（同指纹原地更新）而它正是当前主机：旧会话握着失效凭据，用新凭据重开。
            if (linkFingerprint != null) reopenIfCurrent(saved)
            Result.success(saved)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            Log.w(TAG, "add remote host failed: ${e.message}")
            Result.failure(e.asHostException())
        }
    }

    private suspend fun reopenIfCurrent(ref: HostRef.Remote) = switchLock.withLock {
        if (_host.value.id == ref.id) openOrFail(ref).onSuccess { adopt(ref, it) }
    }

    /**
     * 已配对主机被拒（设备凭据失效）后重新配对成功：先用新凭据真实连接，通过后原地替换同一台主机的地址、凭据与指纹
     * （id、名称不变），并在它就是当前主机时用新凭据重开会话（旧会话握着失效凭据）。主机不存在 → NotFound。
     */
    suspend fun repairRemoteHost(
        hostId: String,
        endpoint: String,
        credential: String,
        linkFingerprint: String,
    ): Result<HostRef.Remote> = switchLock.withLock {
        val url = endpoint.trim()
        val key = credential.trim()
        try {
            if (url.isEmpty() || key.isEmpty()) {
                throw HostException(HostErrorCode.InvalidArgument, message = "endpoint and credential are required")
            }
            awaitFirstSnapshot(FluxBridge.openRemote(url, key))
            val ref = hostRepo.replaceLinkCredential(hostId, url, key, linkFingerprint)
                ?: throw HostException(HostErrorCode.NotFound, message = "host not saved")
            if (_host.value.id == ref.id) adopt(ref, open(ref))
            Result.success(ref)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            Log.w(TAG, "repair remote host failed: ${e.message}")
            Result.failure(e.asHostException())
        }
    }

    /**
     * 已保存远端主机的访问密钥（解密失败 / 不存在 → null）。供需要直连主机 HTTP 面的功能使用
     * （插件包上传 `/api/web/blobs/<kind>`、日志导出 `/api/web/logs/export`），同 iOS `HostRepo.accessKey`。
     */
    suspend fun remoteAccessKey(hostId: String): String? = hostRepo.accessKey(hostId)

    /**
     * 当前远端主机的访问密钥已在主机侧更换：原地保存新密钥并用它重开同一主机（旧会话仍持旧密钥，
     * 重连会鉴权失败）。主机 id 不变。本机主机无访问密钥 → InvalidArgument；重开失败保持原会话并返回失败。
     */
    suspend fun updateRemoteAccessKey(accessKey: String): Result<Unit> = switchLock.withLock {
        val ref = _host.value as? HostRef.Remote
            ?: return@withLock Result.failure(HostException(HostErrorCode.InvalidArgument, message = "current host is local"))
        // 局域网配对的主机用设备专属控制凭据：主机改访问密钥不影响它，这里不能把它覆盖成访问密钥。
        if (ref.linkFingerprint != null) return@withLock Result.success(Unit)
        val saved = try {
            hostRepo.updateAccessKey(ref.id, accessKey.trim())
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            // Keystore 加密 / DataStore 写入失败：调用方据此提示，不能让异常逃出协程使 App 崩溃。
            Log.w(TAG, "update access key for ${ref.id} failed: ${e.message}")
            return@withLock Result.failure(e.asHostException())
        }
        if (!saved) {
            return@withLock Result.failure(HostException(HostErrorCode.NotFound, message = "host not saved"))
        }
        openOrFail(ref).map { adopt(ref, it) }
    }

    /**
     * 删除已保存的远端主机；正在使用时先回到本机。本机不可删除。
     * 局域网配对的主机同时对本机主机调 `agent.link.remove`（尽力而为，失败只记日志），让主机侧撤销该设备的控制权限。
     */
    suspend fun removeHost(id: String) {
        if (id == HostRef.Local.ID) return
        supersedeHostRestore()
        val fingerprint = (hosts.value.firstOrNull { it.id == id } as? HostRef.Remote)?.linkFingerprint
        switchLock.withLock {
            if (_host.value.id == id) {
                adopt(localRef, openOrFail(localRef).getOrElse { UnavailableSession(it.asHostException()) })
            }
            hostRepo.remove(id)
        }
        if (fingerprint != null) unpairLinkedHost(fingerprint)
    }

    private suspend fun unpairLinkedHost(fingerprint: String) {
        try {
            withLocalSession { LinkApi(it).remove(fingerprint) }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            Log.w(TAG, "unpair linked host failed: ${e.message}")
        }
    }

    private suspend fun openOrFail(ref: HostRef): Result<HostSession> = try {
        Result.success(open(ref))
    } catch (e: CancellationException) {
        throw e
    } catch (e: Exception) {
        Log.w(TAG, "open host ${ref.id} failed: ${e.message}")
        val error = e.asHostException()
        AppDiagnostics.event("host open failed (${hostKind(ref)}): ${error.code}")
        Result.failure(error)
    }

    private suspend fun open(ref: HostRef): HostSession = when (ref) {
        is HostRef.Local -> openLocalSession()
        is HostRef.Remote -> {
            val key = hostRepo.accessKey(ref.id)
                ?: throw HostException(HostErrorCode.Unauthorized, message = "access key unavailable")
            FluxBridge.openRemote(ref.endpoint, key)
        }
    }

    /** 停本机监视 → store.detach → 关旧会话 → 绑定新会话 → 重选本机状态来源。 */
    private fun adopt(ref: HostRef, next: HostSession) {
        // 先停监视：当前是本机时它直接投影 store，store 换绑后的远端状态不能流进 localState
        localMonitor?.cancel()
        store.detach()
        val old = session
        session = next
        _host.value = ref
        AppDiagnostics.event("host session attached (${hostKind(ref)})")
        old.close()
        store.attach(next)
        restartLocalMonitor(ref)
    }

    /**
     * 本机状态来源（仅在 [switchLock] 内调用）：
     * - 当前主机就是本机：直接投影主 [store]，不再开第二个会话；
     * - 当前主机是远端：对进程内同一本机主机再开一个会话，由独立的 [HostStore] 归约。
     * 全程事件驱动，无轮询；失联（Stale、Fatal）由 [LocalActivity.from] 视为空闲，避免前台服务僵死。
     */
    private fun restartLocalMonitor(ref: HostRef) {
        localMonitor?.cancel()
        localMonitor = appScope.launch(Dispatchers.Default) {
            when (ref) {
                is HostRef.Local -> store.state.collect { _localState.value = it }
                is HostRef.Remote -> monitorLocalSession()
            }
        }
    }

    private suspend fun monitorLocalSession() {
        val monitor = try {
            openLocalSession()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            Log.w(TAG, "local monitor unavailable: ${e.message}")
            _localState.value = HostState()
            return
        }
        try {
            coroutineScope {
                // 与主 store 相同的串行约束；随本协程取消（切主机）一并停止
                val local = HostStore(this + Dispatchers.Default.limitedParallelism(1))
                local.attach(monitor)
                local.state.collect { _localState.value = it }
            }
        } finally {
            monitor.close()
        }
    }

    private suspend fun awaitFirstSnapshot(probe: HostSession) {
        try {
            withTimeout(CONNECT_TIMEOUT_MS) {
                probe.signals.first { signal ->
                    when (signal) {
                        is HostSignal.Snapshot -> true
                        is HostSignal.Fatal -> throw signal.error
                        else -> false
                    }
                }
            }
        } catch (e: TimeoutCancellationException) {
            throw HostException(HostErrorCode.Timeout, retryable = true, message = "no response from host", cause = e)
        } finally {
            probe.close()
        }
    }

    private fun Throwable.asHostException(): HostException =
        this as? HostException ?: HostException(HostErrorCode.Internal, message = message ?: javaClass.simpleName, cause = this)

    private var lastRescanMs = 0L

    /**
     * 回到前台时请求主机重扫文件跟踪（`daemon.task.rescan`），10s 冷却。
     * 前后台均不周期轮询（空闲静默，AGENTS.md §4）。
     */
    fun rescanOnForeground() {
        val now = android.os.SystemClock.elapsedRealtime()
        if (now - lastRescanMs < RESCAN_COOLDOWN_MS) return
        lastRescanMs = now
        appScope.launch {
            try {
                session.rescan()
            } catch (e: com.fluxdown.core.host.HostException) {
                android.util.Log.w("FluxDown", "rescan failed: ${e.code} ${e.message}")
            }
        }
    }

    /**
     * 对进程内本机主机新开一个独立会话（本机 agent 的 `agent.link.*` 等）；每次返回新会话，调用方负责 [HostSession.close]，
     * 与当前主机选择无关（当前是远端主机时也照常可用）。
     */
    suspend fun openLocalSession(): HostSession {
        val (dataDir, saveDir) = withContext(Dispatchers.IO) {
            val data = File(appContext.filesDir, "fluxdown").also { it.mkdirs() }
            val save = com.fluxdown.app.ui.defaultLocalSaveDir(appContext)
            save.mkdirs()
            data.path to save.path
        }
        return FluxBridge.openLocal(
            dataDir = dataDir, saveDir = saveDir, platform = PLATFORM, deviceName = systemDeviceName(),
            externalLogDir = AppDiagnostics.externalLogRoot?.path,
        )
    }

    /** 开一个本机独立会话跑 [block]，结束（含失败 / 取消）即关闭。 */
    suspend fun <T> withLocalSession(block: suspend (HostSession) -> T): T {
        val local = openLocalSession()
        try {
            return block(local)
        } finally {
            local.close()
        }
    }

    /**
     * 云端设备列表里的默认名：系统「设置 › 关于手机 › 设备名称」（厂商出厂即填，如「Galaxy S24 Ultra」），
     * 读不到再用厂商 + 机型（「Google Pixel 9 Pro」）。沙盒里的内核主机名恒为 `localhost`，不可用。
     */
    private fun systemDeviceName(): String? {
        Settings.Global.getString(appContext.contentResolver, Settings.Global.DEVICE_NAME)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }
            ?.let { return it }
        val model = Build.MODEL.orEmpty().trim()
        val manufacturer = Build.MANUFACTURER.orEmpty().trim().replaceFirstChar { it.titlecase() }
        return when {
            model.isEmpty() -> manufacturer.takeIf { it.isNotEmpty() }
            manufacturer.isEmpty() || model.startsWith(manufacturer, ignoreCase = true) -> model
            else -> "$manufacturer $model"
        }
    }

    private companion object {
        const val TAG = "FluxDown"
        const val PLATFORM = "android"
        const val RESCAN_COOLDOWN_MS = 10_000L
        const val CONNECT_TIMEOUT_MS = 15_000L
    }
}

/** 诊断事件里的连接摘要：连接态 + 任务表是否在分页载入（不含任务数据）。 */
private fun connectionSummary(state: HostState): String {
    val connection = when (val c = state.connection) {
        Connection.Connecting -> "connecting"
        Connection.Live -> "live"
        Connection.Stale -> "stale"
        is Connection.Failed -> "failed ${c.error.code}"
    }
    return if (state.tasksLoading) "$connection, tasks loading (${state.taskTotal})" else connection
}

/** 诊断事件只记主机类别，不记远端地址。 */
private fun hostKind(ref: HostRef): String = if (ref is HostRef.Local) "local" else "remote"

/**
 * 没有可用连接时的占位会话：信号流只发 [HostSignal.Fatal]（store 显示失败态），命令一律抛同一个 [HostException]。
 * 用于本机引擎启动前 / 启动失败（如数据目录被占用）。
 */
private class UnavailableSession(private val error: HostException) : HostSession {
    override val signals: Flow<HostSignal> = flowOf(HostSignal.Fatal(error))

    override fun close() = Unit

    override suspend fun createTask(request: CreateTaskRequest): String = throw error
    override suspend fun pause(taskId: String): Unit = throw error
    override suspend fun resume(taskId: String): Unit = throw error
    override suspend fun delete(taskId: String, deleteFiles: Boolean): Unit = throw error
    override suspend fun pauseMany(taskIds: List<String>): Unit = throw error
    override suspend fun resumeMany(taskIds: List<String>): Unit = throw error
    override suspend fun deleteMany(taskIds: List<String>, deleteFiles: Boolean): Unit = throw error
    override suspend fun pauseAll(): Unit = throw error
    override suspend fun resumeAll(): Unit = throw error
    override suspend fun rename(taskId: String, fileName: String): Unit = throw error
    override suspend fun changeUrl(taskId: String, url: String): Unit = throw error
    override suspend fun rescan(): Unit = throw error

    override fun startImport(batches: List<ImportBatch>): ImportJob {
        val total = batches.sumOf { it.items.size }
        return object : ImportJob {
            // 没有连接：整批失败（原因即会话不可用的原因），不产生进度；条目全部留给重试。
            override val updates: Flow<ImportUpdate> =
                flowOf(ImportUpdate.Finished(ImportSummary(total, 0, total, 0, false, error, batches.flatMap { it.items })))

            override fun cancel() = Unit
        }
    }

    override suspend fun moveToQueue(taskId: String, queueId: String): Unit = throw error
    override suspend fun boost(taskId: String): Unit = throw error
    override suspend fun resolveSelection(requestId: String, outcome: SelectionOutcome): Unit = throw error
    override suspend fun patchConfig(expectedRevision: Long, values: Map<String, String>): Unit = throw error
    override suspend fun refreshRssSource(sourceId: String): Unit = throw error
    override suspend fun setRssSourceEnabled(sourceId: String, enabled: Boolean): Unit = throw error
    override suspend fun call(method: String, paramsJson: String?): String = throw error
}
