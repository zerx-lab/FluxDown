package com.fluxdown.app.feature.downloads

import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.State
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import com.fluxdown.app.AppContainer
import com.fluxdown.app.data.ViewPrefs
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.protocol.FilterBarVisibility
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.preferences
import com.fluxdown.core.format.FileSizeUnits
import com.fluxdown.core.format.Format
import com.fluxdown.core.format.Measure
import com.fluxdown.core.model.FileConflicts
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.model.QueueOrder
import com.fluxdown.core.model.allTasksPaused
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.store.Connection
import com.fluxdown.core.store.HostState
import com.fluxdown.core.store.SpeedHistory
import com.fluxdown.fluxui.data.WaveSeries
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/** 速度波形缓冲：两个定长 FloatArray 原地复用，只有 [series] 的实例随每秒采样替换（绘制阶段读取，不触发重组）。 */
@Stable
internal class WaveBuffer {
    private val down = FloatArray(SpeedHistory.CAPACITY)
    private val up = FloatArray(SpeedHistory.CAPACITY)

    var series: WaveSeries by mutableStateOf(WaveSeries.Idle)
        private set

    fun update(h: SpeedHistory) {
        val n = h.size
        for (i in 0 until n) {
            down[i] = h.downAt(i).toFloat()
            up[i] = h.upAt(i).toFloat()
        }
        series = if (n == 0) WaveSeries.Idle else WaveSeries(down, up, n)
    }
}

/**
 * 下载页共享视图状态：筛选、视图偏好、派生列表 / 分面（后台线程计算）。
 * 由 [ProvideDownloadsView] 创建并只在前台（STARTED）期间收集主机状态。
 */
@Stable
class DownloadsView internal constructor(private val container: AppContainer) {
    var filter by mutableStateOf(DownloadsFilter())
        private set

    /** null = 偏好尚未从 DataStore 读出（此前显示骨架，避免先闪默认视图）。 */
    var prefs by mutableStateOf<ViewPrefs?>(null)
        private set

    var collapsed by mutableStateOf<Set<String>>(emptySet())
        private set

    var list by mutableStateOf(DownloadsList.Initial)
        private set

    var facets by mutableStateOf(Facets.Initial)
        private set

    /** 筛选区各部分的显隐（云同步偏好 `ui.show_sidebar_status|queues|category`，通用设置「下载页显示」）。 */
    var filterVisibility by mutableStateOf(FilterBarVisibility())
        private set

    /** 断连宽限后只读（Stale / Failed）。 */
    var readOnly by mutableStateOf(false)
        private set

    /** 显示断连横幅（Stale / Failed；首次连接中不弹）。 */
    var offline by mutableStateOf(false)
        private set

    var live by mutableStateOf(false)
        private set

    /** 首次连接中（无任务时显示骨架而非空态）。 */
    var connecting by mutableStateOf(true)
        private set

    /** 有待选 fileExists 的任务（行「待确认」角标；每个行只读自己 id 的成员关系，集合变化才重组对应行）。 */
    var conflictTaskIds by mutableStateOf<Set<String>>(emptySet())
        private set

    /** 远程任务的目标设备名 / 在线状态（按 deviceId；名册或云端连接变化才更新）。 */
    internal var remoteTargets by mutableStateOf<Map<String, RemoteTarget>>(emptyMap())
        private set

    /** 排队顺序（稳定键 → 名次）；排队行读它得到「排队 #n」，顺序变化只重组读它的可见行。 */
    internal var queueOrder by mutableStateOf(QueueOrder.Empty)
        private set

    /** 当前主机是本机：远端主机的已完成文件在主机磁盘上，行环钮改为「任务详情」（iOS `ringAction` 同规则）。 */
    var localHost by mutableStateOf(true)
        private set

    /** 远程任务命令与行内过渡态。 */
    internal val remoteCommands = RemoteCommands(container)

    internal val wave = WaveBuffer()

    /** 最近一次指针活动（动态排序键重排节流用）。 */
    @Volatile
    internal var lastInteractionMs = 0L

    private val refresh = MutableStateFlow(0)

    fun setFolder(f: StatusFolder) {
        filter = filter.copy(folder = f, categoryId = null, remoteOnly = false)
    }

    /** 选分类同时取消「远程任务」（芯片行单选）。 */
    fun setCategory(id: String?) {
        filter = filter.copy(categoryId = id, remoteOnly = if (id != null) false else filter.remoteOnly)
    }

    /** 只看远程任务；打开时清除分类（芯片行单选）。 */
    fun setRemoteOnly(on: Boolean) {
        filter = filter.copy(remoteOnly = on, categoryId = if (on) null else filter.categoryId)
    }

    /** [id] 为 null 清除队列范围；与当前相同则同样清除（Rail 再点取消）。 */
    fun setQueue(id: String?) {
        val n = id?.let(::normQueue)
        filter = filter.copy(queueId = if (n == filter.queueId) null else n)
    }

    fun setQuery(q: String) {
        filter = filter.copy(query = q)
    }

    fun clearFilters() {
        filter = DownloadsFilter()
    }

    fun toggleGroup(key: String) {
        collapsed = if (key in collapsed) collapsed - key else collapsed + key
    }

    /** 乐观更新 + 持久化（DataStore 回流为同值，不抖动）。 */
    fun updatePrefs(p: ViewPrefs) {
        prefs = p
        container.appScope.launch { container.viewPrefs.update(p) }
    }

    internal suspend fun run() = coroutineScope {
        launch { container.viewPrefs.state.collect { prefs = it } }
        launch {
            container.store.state
                .map { Flags(it.connection) }
                .distinctUntilChanged()
                .collect {
                    readOnly = it.readOnly
                    offline = it.offline
                    live = it.live
                    connecting = it.connecting
                }
        }
        launch {
            // 被隐藏的部分对应的筛选必须复位，否则列表会被一个看不见的筛选卡住。
            container.store.state.map { FilterBarVisibility.of(it.preferences) }.distinctUntilChanged().collect { v ->
                filterVisibility = v
                if (!v.status && filter.folder != StatusFolder.All) setFolder(StatusFolder.All)
                if (!v.queues && filter.queueId != null) setQueue(null)
                if (!v.categories && filter.categoryId != null) setCategory(null)
                if (!v.categories && filter.remoteOnly) setRemoteOnly(false)
            }
        }
        launch {
            container.store.state.map { FileConflicts.taskIds(it.selections) }.distinctUntilChanged().collect { conflictTaskIds = it }
        }
        launch {
            container.store.state
                .map { Triple(it.cloudDevices, it.sections[HostSection.agentCloudConnection], it.connection == Connection.Live) }
                .distinctUntilChanged()
                .collect { (devices, connection, live) -> remoteTargets = RemoteTarget.index(devices, connection, live) }
        }
        launch {
            container.store.state.map { it.speedHistory }.distinctUntilChanged().collect { wave.update(it) }
        }
        launch {
            // 名次只在渲染可见排队行时按键二分取：顺序变化（队首出队等）只重组读它的可见行，不重建任务行模型。
            container.store.state.map { it.queueOrder }.distinctUntilChanged().collect { queueOrder = it }
        }
        launch {
            container.host.map { it is HostRef.Local }.distinctUntilChanged().collect { localHost = it }
        }
        launch {
            val cache = DeriveCache()
            var retry: Job? = null
            combine(
                container.store.state,
                snapshotFlow { prefs }.filterNotNull(),
                snapshotFlow { filter },
                snapshotFlow { collapsed },
                refresh,
            ) { s, p, f, c, _ -> DeriveInput(s, p, f, c) }
                .map { derive(it, cache, System.currentTimeMillis(), lastInteractionMs) }
                .flowOn(Dispatchers.Default)
                .conflate()
                .collect { r ->
                    if (r.list != list) list = r.list
                    if (r.facets != facets) facets = r.facets
                    retry?.cancel()
                    if (r.retryAtMs > 0) {
                        retry = launch {
                            delay((r.retryAtMs - System.currentTimeMillis()).coerceAtLeast(50L))
                            refresh.value++
                        }
                    }
                }
        }
    }

    private class Flags(c: Connection) {
        val readOnly = c != Connection.Live
        val offline = c is Connection.Stale || c is Connection.Failed
        val live = c == Connection.Live
        val connecting = c == Connection.Connecting

        override fun equals(other: Any?) = other is Flags && other.readOnly == readOnly && other.offline == offline &&
            other.live == live && other.connecting == connecting

        override fun hashCode() = (if (readOnly) 1 else 0) + (if (offline) 2 else 0) + (if (live) 4 else 0) + (if (connecting) 8 else 0)
    }
}

val LocalDownloadsView = staticCompositionLocalOf<DownloadsView> { error("DownloadsView missing") }
internal val LocalDownloadSizeUnits = compositionLocalOf<FileSizeUnits?> { null }

/** 创建并提供下载页共享视图状态（Tab 之间、Rail、选择坞、Sheet 共用一份）。 */
@Composable
fun ProvideDownloadsView(content: @Composable () -> Unit) {
    val container = LocalAppContainer.current
    val view = remember(container) { DownloadsView(container) }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(view, lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { view.run() }
    }
    val rowText = rememberRowText()
    val appearance by container.appearanceState.collectAsStateWithLifecycle()
    CompositionLocalProvider(
        LocalDownloadsView provides view,
        LocalRowText provides rowText,
        LocalDownloadSizeUnits provides appearance?.fileSizeUnits,
    ) {
        content()
    }
}

// ── 实时汇总（叶子组合项读取，避免整屏随 10 Hz 发布重组） ───────────────────────

@Immutable
internal data class LiveStats(
    val down: Long,
    val up: Long,
    val speed: Measure,
    val upSpeed: Measure,
    val active: Int,
    val pending: Int,
    val retry: Int,
    val diskFree: Long?,
    /** 全部任务都已暂停：按钮显示「全部恢复」。 */
    val allPaused: Boolean,
)

internal fun HostState.toLiveStats(): LiveStats {
    val st = stats
    val allPaused = allTasksPaused(tasks, st.activeTasks, st.pendingTasks)
    return LiveStats(
        down = st.totalDownloadBps,
        up = st.totalUploadBps,
        speed = Format.speedOrZero(st.totalDownloadBps),
        upSpeed = Format.speedOrZero(st.totalUploadBps),
        active = st.activeTasks,
        pending = st.pendingTasks,
        retry = st.retryPendingTasks,
        diskFree = st.diskFreeBytes,
        allPaused = allPaused,
    )
}

@Composable
internal fun rememberLiveStats(): State<LiveStats> {
    val host = hostState()
    return remember(host) { derivedStateOf { host.value.toLiveStats() } }
}
