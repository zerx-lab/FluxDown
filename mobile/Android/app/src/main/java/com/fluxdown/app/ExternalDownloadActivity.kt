package com.fluxdown.app

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.lifecycleScope
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.actions.TaskActions
import com.fluxdown.app.data.DeviceSettings
import com.fluxdown.app.feature.devices.hostErrorText
import com.fluxdown.app.feature.newtask.DispatchItem
import com.fluxdown.app.feature.newtask.Dispatcher
import com.fluxdown.app.feature.newtask.NewDownloadSheet
import com.fluxdown.app.feature.newtask.PublicDownloadGuide
import com.fluxdown.app.feature.newtask.TorrentImport
import com.fluxdown.app.feature.newtask.UrlEntry
import com.fluxdown.app.feature.newtask.needsPublicDownloadGuide
import com.fluxdown.app.feature.newtask.skipPublicDownloadGuide
import com.fluxdown.app.feature.settings.account.AccountText
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.AppNavigator
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.SheetRoute
import com.fluxdown.app.service.DownloadServiceController
import com.fluxdown.app.shell.FluxAppRoot
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.ui.ExternalLocalDirectory.SilentDestination
import com.fluxdown.app.ui.defaultLocalSaveDir
import com.fluxdown.app.ui.externalLocalDirectory
import com.fluxdown.app.ui.isLocalDirWritable
import com.fluxdown.core.capture.CaptureDeviceRules
import com.fluxdown.core.capture.ExternalDownload
import com.fluxdown.core.capture.ExternalIntake
import com.fluxdown.core.capture.SilentCapture
import com.fluxdown.core.capture.TorrentFile
import com.fluxdown.core.host.HostException
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.protocol.AccountErrorContext
import com.fluxdown.core.protocol.CustomCategoryDto
import com.fluxdown.core.protocol.DeviceRules
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.preferences
import com.fluxdown.core.store.Connection
import com.fluxdown.core.store.HostState
import com.fluxdown.core.store.HostStore
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxOverlayHost
import com.fluxdown.fluxui.overlay.FluxPortalHost
import com.fluxdown.fluxui.overlay.FluxPortalState
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.overlay.LocalFluxPortal
import com.fluxdown.fluxui.overlay.rememberFluxOverlayState
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

private const val TAG = "FluxExternal"

/** 等主机首个快照（默认目录 / 队列 / 免打扰偏好都在里面）的上限；超时按当前状态继续（弹窗会显示只读提示）。 */
private const val READY_TIMEOUT_MS = 8_000L

internal suspend fun Flow<HostState>.awaitHostOutcome(): HostState =
    first { !it.isReadOnly || it.connection is Connection.Failed }

/** Re-read after resuming: a writable emission can already be stale when the waiter runs. */
internal suspend fun StateFlow<HostState>.awaitTorrentWrite(
    isCurrent: () -> Boolean,
    onFailure: (HostException) -> Unit,
): HostState? {
    while (isCurrent()) {
        val latest = value
        val connection = latest.connection
        if (connection is Connection.Failed) {
            onFailure(connection.error)
            return null
        }
        if (!latest.isReadOnly) return latest
        awaitHostOutcome()
    }
    return null
}

private suspend fun awaitReady(store: HostStore): HostState {
    withTimeoutOrNull(READY_TIMEOUT_MS) { store.state.awaitHostOutcome() }
    return store.state.value
}

/** Sheet 退场动画（`FluxMotion.snap` 弹簧）走完再结束任务，避免弹窗被直接截断。 */
private const val SHEET_EXIT_MS = 250L

/** 种子建完后保留窗口的时长：让成功 / 失败 toast 先展示完再结束任务。 */
private const val TOAST_LINGER_MS = 1_800L

/**
 * 外部下载唤起入口（同 Flutter 版同名 Activity；类名与 manifest 中两个 http(s) 别名都不可改——浏览器可能按
 * 组件名保存了它们）。透明窗口 + 独立任务（`singleInstance`、`taskAffinity=""`、不进最近任务）：只在来源应用
 * （浏览器 / 文件管理器）上方弹出「新建下载」，关闭后结束整个任务、露出来源应用。
 *
 * 来源：浏览器「外部下载器」（X / Via 等的 http(s) VIEW，extra `User-Agent` / `Cookie` / `Referer`）、浏览器扩展
 * （Android 浏览器经 `intent:` URI 显式唤起 http(s) / magnet / ed2k VIEW，另带文件名与请求头 extra；桌面协议
 * `fluxdown://download?…`）、系统分享、`magnet:` / `ed2k://` 链接。需要确认的捕获按目标逐组显示。
 * 「免打扰下载」开启时按规则下发或用主机默认设置直接建任务，失败时保留原始请求供用户确认。
 * 与主界面共用进程级 [AppContainer]（同一主机会话）。
 */
class ExternalDownloadActivity : ComponentActivity() {
    private val navigator = AppNavigator()

    /** 按到达顺序串行处理（解析可能较重：长 Cookie / 请求头 JSON）。 */
    private val intents = Channel<Intent>(Channel.UNLIMITED)
    private val confirmations = ExternalConfirmationQueue()

    private val torrents = ExternalTorrentQueue<PendingTorrent>(
        closeNow = ::finishIfIdle,
        closeAfterToast = ::closeAfterTorrentToast,
        cancelClose = { closeJob?.cancel() },
        hasOtherSheet = { confirmations.hasOtherSheet(navigator.sheet != null) },
    )
    private var closeJob: Job? = null
    private var unwritableDir by mutableStateOf<String?>(null)
    private var outstandingIntents = 0

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        val container = (application as FluxApplication).container
        // 系统重建入口时沿用原 intent；捕获的请求上下文不会另行写入磁盘。
        enqueue(intent)
        lifecycleScope.launch {
            for (next in intents) {
                try {
                    if (withContext(Dispatchers.IO) { isTorrentView(next) }) {
                        next.data?.let { torrents.enqueue(PendingTorrent(it)) }
                    } else {
                        val requests = withContext(Dispatchers.Default) { parse(next) }
                        if (requests.isNotEmpty()) dispatch(container, requests)
                    }
                } finally {
                    outstandingIntents--
                }
                finishIfIdle()
            }
        }
        setContent {
            FluxAppRoot(container, navigator) {
                ExternalDownloadStage(
                    unwritableDir = unwritableDir,
                    onUnwritableShown = { unwritableDir = null },
                    torrents = torrents,
                    onSubmitted = { keepLocalDownloadsAlive(container) },
                    onDismiss = {
                        confirmations.beginClosing()
                        navigator.closeSheet()
                    },
                    onClosed = ::advanceConfirmation,
                )
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        enqueue(intent)
    }

    private fun enqueue(intent: Intent) {
        closeJob?.cancel()
        outstandingIntents++
        if (intents.trySend(intent).isFailure) outstandingIntents--
    }

    private fun finishIfIdle() {
        if (outstandingIntents == 0 && !confirmations.hasOtherSheet(navigator.sheet != null) &&
            torrents.pending.isEmpty() && closeJob?.isActive != true
        ) {
            finishAndRemoveTask()
        }
    }

    private fun closeAfterTorrentToast() {
        closeJob = lifecycleScope.launch {
            delay(TOAST_LINGER_MS)
            closeJob = null
            finishIfIdle()
        }
    }

    /** VIEW 一个 content / file 种子：MIME 为 `application/x-bittorrent`，或（octet-stream 兜底）文件名以 `.torrent` 结尾。 */
    private fun isTorrentView(intent: Intent): Boolean {
        if (intent.action != Intent.ACTION_VIEW) return false
        val uri = intent.data ?: return false
        if (uri.scheme != "content" && uri.scheme != "file") return false
        return intent.type == TorrentFile.MIME_TYPE || TorrentFile.isTorrentFileName(TorrentImport.displayName(this, uri))
    }

    override fun onDestroy() {
        intents.close()
        super.onDestroy()
    }

    private class ConfirmationGroup(val targetId: String?) {
        val requests = ArrayList<ExternalDownload>()
        var failure: String? = null
        var unwritableDir: String? = null
    }

    /** 设备规则用于没有显式本地目录的捕获；静默失败后保留原始请求。 */
    private suspend fun dispatch(container: AppContainer, requests: List<ExternalDownload>) {
        val state = awaitReady(container.store)
        val host = container.host.value
        val session = container.session
        val prefs = state.preferences
        val silent = prefs.bool(SilentCapture.SILENT_PREF, false)
        val rules = CaptureDeviceRules.parse(prefs.string(CaptureDeviceRules.PREFERENCE_KEY, ""))
        val targets = if (rules.isEmpty() || !silent) emptyList() else CaptureDeviceRules.targets(state)
        val groups = ArrayList<ConfirmationGroup>()
        val portableGroups = HashMap<String?, ConfirmationGroup>()
        for (request in requests) {
            val explicitDir = request.saveDir
            val targetId = if (explicitDir == null) CaptureDeviceRules.targetId(rules, request) else null
            var failure: String? = null
            var unwritableDir: String? = null
            if (targetId == null) {
                val plan = SilentCapture.plan(request, state)?.request
                val local = host is HostRef.Local
                if (plan != null && !state.isReadOnly && torrents.pending.isEmpty() &&
                    (local || explicitDir == null) &&
                    (explicitDir != null || !state.needsPublicDownloadGuide(this, host))
                ) {
                    val silentRequest = when {
                        !local -> plan
                        explicitDir == null -> plan.takeIf { withContext(Dispatchers.IO) { isLocalDirWritable(it.saveDir) } }
                        else -> when (val destination = withContext(Dispatchers.IO) {
                            externalLocalDirectory(this@ExternalDownloadActivity).forSilent(explicitDir, trustedDownloadRoots(state))
                        }) {
                            SilentDestination.Confirm -> null
                            SilentDestination.Unwritable -> {
                                unwritableDir = explicitDir
                                null
                            }
                            is SilentDestination.Ready -> if (destination.path == plan.saveDir) plan else plan.copy(saveDir = destination.path)
                        }
                    }
                    val latest = container.store.state.value
                    if (silentRequest != null && torrents.pending.isEmpty() &&
                        container.session === session && container.host.value == host && !latest.isReadOnly &&
                        (explicitDir == null || (state.config["default_save_dir"] == latest.config["default_save_dir"] &&
                            state.sections[HostSection.agentPreferences] == latest.sections[HostSection.agentPreferences]))
                    ) {
                        try {
                            session.createTask(silentRequest)
                            keepLocalDownloadsAlive(container)
                            continue
                        } catch (e: HostException) {
                            Log.w(TAG, "silent capture failed: ${e.code}")
                            failure = hostErrorText(e, host)
                        }
                    }
                }
            } else if (silent && request.autoStart != false && CaptureDeviceRules.canDispatch(request) && torrents.pending.isEmpty()) {
                val target = DeviceRules.resolveTarget(targetId, targets)
                val ready = !container.store.state.value.isReadOnly && container.session === session && container.host.value == host
                if (target != null && (!target.isLinkOnly || target.online == true) && ready) {
                    try {
                        val item = DispatchItem(UrlEntry(request.url), request.fileName.ifEmpty { null })
                        Dispatcher.dispatch(item, target, saveDir = null, session = session)
                        continue
                    } catch (e: HostException) {
                        Log.w(TAG, "silent device dispatch failed: ${e.code}")
                        val scene = if (target.routesViaLink) AccountErrorContext.Pairing else AccountErrorContext.General
                        failure = "${str(R.string.captureRoutingDispatchFailed)}\n${AccountText.error(this, e, scene)}"
                    }
                } else {
                    failure = when {
                        !ready -> str(R.string.localServiceDisconnected)
                        target?.isLinkOnly == true -> str(R.string.errReasonPeerOffline)
                        else -> str(R.string.captureRoutingTargetUnavailable)
                    }
                }
            }
            // Request context and explicit destinations belong to one capture, not a batch.
            val group = if (request.hasRequestContext || explicitDir != null || request.autoStart == true) {
                ConfirmationGroup(targetId).also(groups::add)
            } else {
                portableGroups.getOrPut(targetId) { ConfirmationGroup(targetId).also(groups::add) }
            }
            group.requests.add(request)
            if (group.failure == null) group.failure = failure
            group.unwritableDir = unwritableDir
        }
        for (group in groups) {
            val route = SheetRoute.NewDownload(
                external = group.requests, targetId = group.targetId, captureFailure = group.failure,
            )
            confirmations.offer(navigator.sheet != null, ExternalConfirmation(route, group.unwritableDir))
                ?.let(::showConfirmation)
        }
    }

    private fun showConfirmation(confirmation: ExternalConfirmation) {
        unwritableDir = confirmation.unwritableDir
        navigator.openSheet(confirmation.route)
    }

    private fun advanceConfirmation() {
        val next = confirmations.afterClosing()
        if (next != null) showConfirmation(next)
        else finishIfIdle()
    }

    /** 本机建了任务：趁窗口仍可见拉起前台服务（Android 12+ 禁止后台启动），服务在本机空闲后自行停止。 */
    private fun keepLocalDownloadsAlive(container: AppContainer) {
        if (container.host.value is HostRef.Local) DownloadServiceController.start(this, "external download")
    }

    /** 分享取文本与 ClipData 中的所有链接，VIEW 取 data 与浏览器 extra。 */
    private fun parse(intent: Intent): List<ExternalDownload> = when (intent.action) {
        Intent.ACTION_SEND, Intent.ACTION_SEND_MULTIPLE -> sharedDownloads(intent)
        Intent.ACTION_VIEW -> listOfNotNull(ExternalIntake.fromView(
            data = intent.dataString,
            userAgent = intent.getStringExtra(EXTRA_USER_AGENT),
            cookie = intent.getStringExtra(EXTRA_COOKIE),
            referer = intent.getStringExtra(EXTRA_REFERER),
            fileName = intent.getStringExtra(EXTRA_FILE_NAME),
            headersJson = intent.getStringExtra(EXTRA_HEADERS),
        ))
        else -> emptyList()
    }

    private fun sharedDownloads(intent: Intent): List<ExternalDownload> {
        val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
        val primary = ExternalIntake.fromSharedText(text)
        val clip = intent.clipData ?: return primary
        return buildList {
            addAll(primary)
            for (i in 0 until clip.itemCount) {
                addAll(ExternalIntake.fromSharedText(clip.getItemAt(i).text?.toString()))
            }
        }.distinctBy { it.url }
    }

    private companion object {
        /** X 浏览器等调用外部下载器时随 VIEW 附带的请求上下文（键名区分大小写）。 */
        const val EXTRA_USER_AGENT = "User-Agent"
        const val EXTRA_COOKIE = "Cookie"
        const val EXTRA_REFERER = "Referer"

        /** 浏览器扩展在 Android 上经 `intent:` URI 附带的建议文件名与完整请求头（JSON 对象），同 `fluxDown/utils/android-intent.ts`。 */
        const val EXTRA_FILE_NAME = "com.fluxdown.extra.FILE_NAME"
        const val EXTRA_HEADERS = "com.fluxdown.extra.HEADERS"
    }
}

/** 待导入的种子；按引用区分（同一 URI 连续唤起两次也各处理一次）。 */
private class PendingTorrent(val uri: Uri)

private fun trustedDownloadRoots(state: HostState): Sequence<String> = sequence {
    state.config["default_save_dir"]?.let { yield(it) }
    val prefs = state.preferences
    for (category in CustomCategoryDto.fromPreference(prefs[CustomCategoryDto.PREFERENCE_KEY])) {
        if (!category.isAll && category.saveDir.isNotBlank()) yield(category.saveDir)
    }
    prefs.string(SilentCapture.LAST_SAVE_DIR_PREF, "").takeIf { it.isNotBlank() }?.let { yield(it) }
}

internal data class ExternalConfirmation(val route: SheetRoute.NewDownload, val unwritableDir: String? = null)

internal class ExternalConfirmationQueue {
    private val pending = ArrayDeque<ExternalConfirmation>()
    private var closing = false

    fun hasOtherSheet(sheetOpen: Boolean): Boolean = sheetOpen || closing

    fun offer(sheetOpen: Boolean, incoming: ExternalConfirmation): ExternalConfirmation? {
        if (hasOtherSheet(sheetOpen) || pending.isNotEmpty()) {
            pending.addLast(incoming)
            return null
        }
        return incoming
    }

    fun beginClosing() {
        closing = true
    }

    fun afterClosing(): ExternalConfirmation? {
        closing = false
        return pending.removeFirstOrNull()
    }
}

internal class ExternalTorrentQueue<T : Any>(
    private val closeNow: () -> Unit,
    private val closeAfterToast: () -> Unit,
    private val cancelClose: () -> Unit,
    private val hasOtherSheet: () -> Boolean,
) {
    private val queue = mutableStateListOf<T>()
    val pending: List<T> get() = queue

    fun enqueue(item: T) {
        cancelClose()
        queue.add(item)
    }

    private fun removeHead(head: T): Boolean {
        if (queue.firstOrNull() !== head) return false
        queue.removeAt(0)
        return true
    }

    fun cancelWaiting(head: T, state: HostState) {
        if (!state.isReadOnly || state.connection is Connection.Failed) return
        if (!removeHead(head)) return
        cancelClose()
        closeIfIdle()
    }

    fun complete(head: T) {
        if (removeHead(head)) lingerIfIdle()
    }

    fun fail(head: T, report: () -> Unit) {
        if (queue.firstOrNull() !== head) return
        queue.clear()
        report()
        lingerIfIdle()
    }

    private fun lingerIfIdle() {
        if (queue.isEmpty()) {
            cancelClose()
            closeAfterToast()
        }
    }

    fun closeIfIdle() {
        if (queue.isEmpty() && !hasOtherSheet()) closeNow()
    }
}

/** 只为已确认符合引导条件的种子创建；断连不取消已经展示的引导。 */
internal class ExternalTorrentGuide(
    private val appDir: String,
    private val guideDone: () -> Boolean,
    private val finishGuide: () -> Unit,
) {
    var saving by mutableStateOf(false)
    var selectedDirectory by mutableStateOf<String?>(null)
    private var dismissed by mutableStateOf(false)
    val resolved: Boolean get() = dismissed || guideDone()

    private sealed interface Outcome {
        data class Submit(val saveDir: String) : Outcome
        data class Fail(val error: HostException) : Outcome
    }

    fun canChooseDirectory(state: HostState, host: HostRef): Boolean =
        !saving && !resolved && state.needsPublicDownloadGuide(host, done = false, appDir = appDir)

    fun dismiss(state: HostState, host: HostRef) {
        if (saving || dismissed) return
        if (state.isReadOnly && host is HostRef.Local) {
            // 已向用户展示本机引导；跳过只写设备偏好，断连也无需修改主机配置。
            if (!guideDone()) finishGuide()
        } else {
            state.skipPublicDownloadGuide(host, guideDone(), appDir, finishGuide)
        }
        dismissed = true
    }

    /** 空字符串交给主机使用当前默认目录；一次等待只交接一次导入。 */
    suspend fun submitWhenReady(
        stateOf: () -> HostState,
        hostOf: () -> HostRef,
        onFailure: (HostException) -> Unit,
        submit: suspend (String) -> Unit,
    ) {
        val outcome = snapshotFlow<Outcome?> {
            val state = stateOf()
            val connection = state.connection
            if (connection is Connection.Failed) return@snapshotFlow Outcome.Fail(connection.error)
            if (saving || state.isReadOnly) return@snapshotFlow null
            val host = hostOf()
            val directory = selectedDirectory
            when {
                directory != null && host is HostRef.Local -> Outcome.Submit(directory)
                !resolved && state.needsPublicDownloadGuide(host, done = false, appDir = appDir) -> null
                else -> Outcome.Submit("")
            }
        }.filterNotNull().first()
        when (outcome) {
            is Outcome.Submit -> submit(outcome.saveDir)
            is Outcome.Fail -> onFailure(outcome.error)
        }
    }
}

@Composable
private fun ExternalTorrentGuideSheet(
    guide: ExternalTorrentGuide?,
    state: HostState,
    host: HostRef,
    onSkip: () -> Unit,
    onCancel: () -> Unit,
) {
    val resolved = guide?.resolved != false
    FluxSheet(
        visible = !resolved || state.isReadOnly,
        onDismissRequest = if (resolved) onCancel else onSkip,
        dismissible = guide?.saving != true,
        title = str(if (resolved) R.string.mobileTorrentImportPending else R.string.defaultSaveDir),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (resolved) {
                if (state.isReadOnly) {
                    FluxBanner(str(R.string.mobileTorrentImportWaitingForHost), kind = FluxBannerKind.Warn)
                    FluxButton(str(R.string.cancel), onClick = onCancel, enabled = guide?.saving != true)
                }
            } else if (guide != null) {
                if (state.isReadOnly) {
                    FluxBanner(str(R.string.mobileDownloadGuideDisconnected), kind = FluxBannerKind.Warn)
                }
                PublicDownloadGuide(
                    enabled = guide.canChooseDirectory(state, host),
                    onSavingChange = { guide.saving = it },
                    onSaved = { guide.selectedDirectory = it },
                    onSkip = onSkip,
                    skipEnabled = !guide.saving,
                )
            }
        }
    }
}

/** 透明舞台：展示新建下载、种子引导及其浮层。 */
@Composable
private fun ExternalDownloadStage(
    unwritableDir: String?,
    onUnwritableShown: () -> Unit,
    torrents: ExternalTorrentQueue<PendingTorrent>,
    onSubmitted: () -> Unit,
    onDismiss: () -> Unit,
    onClosed: () -> Unit,
) {
    val container = LocalAppContainer.current
    val nav = LocalNavigator.current
    val context = LocalContext.current
    val overlays = rememberFluxOverlayState()
    val portal = remember { FluxPortalState() }
    val scope = rememberCoroutineScope()
    val haptics = FluxTheme.haptics
    val actions = remember(overlays, haptics) {
        TaskActions(context.applicationContext, scope, { container.session }, container.store, overlays, nav, haptics) {
            container.host.value is HostRef.Local
        }
    }
    val close by rememberUpdatedState(onClosed)
    val head = torrents.pending.firstOrNull()
    val state = container.store.state.collectAsState()
    val host = container.host.collectAsState()
    val settings = remember(context) { DeviceSettings.of(context) }
    val appDir = remember(context) { defaultLocalSaveDir(context).path }
    var guide by remember(head) { mutableStateOf<ExternalTorrentGuide?>(null) }
    var creating by remember(head) { mutableStateOf(false) }
    CompositionLocalProvider(
        LocalFluxOverlays provides overlays,
        LocalTaskActions provides actions,
        LocalFluxPortal provides portal,
    ) {
        Box(Modifier.fillMaxSize()) {
            NewDownloadSheet(route = nav.sheet as? SheetRoute.NewDownload, onDismiss = onDismiss, onSubmitted = onSubmitted)
            val activeGuide = guide
            if (head != null && !creating && state.value.connection !is Connection.Failed) key(head) {
                ExternalTorrentGuideSheet(
                    guide = activeGuide,
                    state = state.value,
                    host = host.value,
                    onSkip = {
                        if (activeGuide != null && guide === activeGuide) {
                            activeGuide.dismiss(container.store.state.value, container.host.value)
                        }
                    },
                    onCancel = {
                        if (!creating && guide === activeGuide && activeGuide?.saving != true) {
                            torrents.cancelWaiting(head, container.store.state.value)
                        }
                    },
                )
            }
            FluxPortalHost(portal)
            FluxOverlayHost(overlays)
        }
    }
    LaunchedEffect(unwritableDir) {
        if (unwritableDir != null) {
            overlays.toast(context.str(R.string.mobileSaveDirNotWritable, "dir" to unwritableDir), FluxToastKind.Error)
            onUnwritableShown()
        }
    }
    var opened by remember { mutableStateOf(false) }
    val sheet = nav.sheet
    LaunchedEffect(sheet) {
        if (sheet != null) {
            opened = true
        } else if (opened) {
            delay(SHEET_EXIT_MS)
            opened = false
            close()
        }
    }
    // 种子逐个提交；本机首次使用应用默认目录时等用户选定目录或明确跳过。
    LaunchedEffect(head) {
        if (head == null) return@LaunchedEffect
        fun fail(error: HostException) {
            torrents.fail(head) {
                guide = null
                overlays.toast(context.hostErrorText(error, container.host.value), FluxToastKind.Error)
            }
        }
        suspend fun awaitWritable(): HostState? = container.store.state.awaitTorrentWrite(
            isCurrent = { torrents.pending.firstOrNull() === head },
            onFailure = ::fail,
        )
        val ready = awaitWritable() ?: return@LaunchedEffect
        suspend fun submit(saveDir: String) {
            if (awaitWritable() == null) return
            guide = null
            TorrentImport.submit(
                context = context,
                container = container,
                overlays = overlays,
                errorText = actions::errorText,
                uris = listOf(head.uri),
                saveDir = saveDir,
                queueId = "",
                startPaused = false,
                beforeCreate = {
                    if (awaitWritable() == null) false
                    else {
                        creating = true
                        true
                    }
                },
            ).let { if (it > 0) onSubmitted() }
            torrents.complete(head)
        }
        if (ready.needsPublicDownloadGuide(container.host.value, settings.downloadFolderGuideDone, appDir)) {
            val activeGuide = ExternalTorrentGuide(appDir, { settings.downloadFolderGuideDone }, settings::finishDownloadFolderGuide)
            guide = activeGuide
            activeGuide.submitWhenReady({ state.value }, { host.value }, ::fail, ::submit)
        } else {
            submit("")
        }
    }
}
