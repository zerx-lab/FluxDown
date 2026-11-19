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
import androidx.compose.runtime.getValue
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
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.lifecycleScope
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.actions.TaskActions
import com.fluxdown.app.data.DeviceSettings
import com.fluxdown.app.feature.newtask.NewDownloadSheet
import com.fluxdown.app.feature.newtask.PublicDownloadGuide
import com.fluxdown.app.feature.newtask.TorrentImport
import com.fluxdown.app.feature.newtask.needsPublicDownloadGuide
import com.fluxdown.app.feature.newtask.skipPublicDownloadGuide
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.AppNavigator
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.SheetRoute
import com.fluxdown.app.service.DownloadServiceController
import com.fluxdown.app.shell.FluxAppRoot
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.app.ui.defaultLocalSaveDir
import com.fluxdown.app.ui.isLocalDirWritable
import com.fluxdown.core.capture.ExternalDownload
import com.fluxdown.core.capture.ExternalIntake
import com.fluxdown.core.capture.SilentCapture
import com.fluxdown.core.capture.TorrentFile
import com.fluxdown.core.host.HostException
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.store.Connection
import com.fluxdown.core.store.HostState
import com.fluxdown.core.store.HostStore
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxOverlayHost
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.overlay.rememberFluxOverlayState
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

private const val TAG = "FluxExternal"

/** 等主机首个快照（默认目录 / 队列 / 免打扰偏好都在里面）的上限；超时按当前状态继续（弹窗会显示只读提示）。 */
private const val READY_TIMEOUT_MS = 8_000L

private suspend fun awaitReady(store: HostStore): HostState =
    withTimeoutOrNull(READY_TIMEOUT_MS) {
        store.state.first { it.connection == Connection.Live || it.connection is Connection.Failed }
    } ?: store.state.value

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
 * `fluxdown://download?…`）、系统分享、`magnet:` / `ed2k://` 链接。弹窗打开期间的后续唤起追加进同一表单。
 * 「免打扰下载」开启时不弹窗，按默认设置直接建任务后返回；算不出保存目录、本机目录不可写或建任务失败时退回弹窗。
 * 与主界面共用进程级 [AppContainer]（同一主机会话）。
 */
class ExternalDownloadActivity : ComponentActivity() {
    private val navigator = AppNavigator()

    /** 按到达顺序串行处理（解析可能较重：长 Cookie / 请求头 JSON）。 */
    private val intents = Channel<Intent>(Channel.UNLIMITED)

    /** 待导入的 `.torrent`（VIEW content / file URI）：由舞台逐个提交，完成后回调 [onTorrentDone]。 */
    private val torrents = mutableStateListOf<PendingTorrent>()
    private var closeJob: Job? = null

    /** 免打扰建任务失败的原因：交给组合层 toast 后清空。 */
    private var silentFailure by mutableStateOf<HostException?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        val container = (application as FluxApplication).container
        // 进程被回收后恢复：弹窗状态已丢失，按原 intent 重新处理（建成即结束，不会重复建任务）。
        intents.trySend(intent)
        lifecycleScope.launch {
            for (next in intents) {
                if (withContext(Dispatchers.IO) { isTorrentView(next) }) {
                    closeJob?.cancel()
                    next.data?.let { torrents.add(PendingTorrent(it)) }
                    continue
                }
                val request = withContext(Dispatchers.Default) { parse(next) }
                when {
                    request != null -> dispatch(container, request)
                    navigator.sheet == null && torrents.isEmpty() -> finishAndRemoveTask()
                }
            }
        }
        setContent {
            FluxAppRoot(container, navigator) {
                ExternalDownloadStage(
                    failure = silentFailure,
                    torrents = torrents,
                    onFailureShown = { silentFailure = null },
                    onSubmitted = { keepLocalDownloadsAlive(container) },
                    onTorrentDone = ::onTorrentDone,
                    onTorrentCancelled = ::cancelTorrentImport,
                    onClosed = ::finishAndRemoveTask,
                )
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        intents.trySend(intent)
    }

    /** 种子建完：窗口只为它而开时，留出 toast 展示时间再结束；其间又来新的唤起则取消。 */
    private fun onTorrentDone(done: PendingTorrent) {
        torrents.remove(done)
        closeJob?.cancel()
        closeJob = lifecycleScope.launch {
            delay(TOAST_LINGER_MS)
            if (navigator.sheet == null && torrents.isEmpty()) finishAndRemoveTask()
        }
    }

    private fun cancelTorrentImport() {
        torrents.clear()
        closeJob?.cancel()
        if (navigator.sheet == null) finishAndRemoveTask()
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

    /** 免打扰优先；否则打开弹窗，已打开时追加进当前表单。 */
    private suspend fun dispatch(container: AppContainer, request: ExternalDownload) {
        val state = awaitReady(container.store)
        val plan = SilentCapture.plan(request, state)
        if (plan != null && torrents.isEmpty() &&
            !state.needsPublicDownloadGuide(this, container.host.value) && silentDirUsable(container, plan)
        ) {
            try {
                container.session.createTask(plan.request)
                keepLocalDownloadsAlive(container)
                if (navigator.sheet == null) finishAndRemoveTask()
                return
            } catch (e: HostException) {
                Log.w(TAG, "silent download failed, falling back to the sheet: ${e.code} ${e.message}")
                silentFailure = e
            }
        }
        navigator.openSheet(SheetRoute.NewDownload(external = request))
    }

    /** 本机主机：目录不可写（分区存储外 / 拼错路径）时退回弹窗，避免静默建出必然失败的任务。 */
    private suspend fun silentDirUsable(container: AppContainer, plan: SilentCapture): Boolean =
        container.host.value !is HostRef.Local || withContext(Dispatchers.IO) { isLocalDirWritable(plan.request.saveDir) }

    /** 本机建了任务：趁窗口仍可见拉起前台服务（Android 12+ 禁止后台启动），服务在本机空闲后自行停止。 */
    private fun keepLocalDownloadsAlive(container: AppContainer) {
        if (container.host.value is HostRef.Local) DownloadServiceController.start(this, "external download")
    }

    /** 分享取文本，VIEW 取 data 与浏览器 extra。 */
    private fun parse(intent: Intent?): ExternalDownload? = when (intent?.action) {
        Intent.ACTION_SEND, Intent.ACTION_SEND_MULTIPLE ->
            ExternalIntake.fromSharedText(intent.getStringExtra(Intent.EXTRA_TEXT)?.takeIf { it.isNotBlank() } ?: clipText(intent))
        Intent.ACTION_VIEW -> ExternalIntake.fromView(
            data = intent.dataString,
            userAgent = intent.getStringExtra(EXTRA_USER_AGENT),
            cookie = intent.getStringExtra(EXTRA_COOKIE),
            referer = intent.getStringExtra(EXTRA_REFERER),
            fileName = intent.getStringExtra(EXTRA_FILE_NAME),
            headersJson = intent.getStringExtra(EXTRA_HEADERS),
        )
        else -> null
    }

    /** ClipData 中首个非空文本（分享缺 `EXTRA_TEXT` 时的兜底）。 */
    private fun clipText(intent: Intent): String? {
        val clip = intent.clipData ?: return null
        for (i in 0 until clip.itemCount) {
            val text = clip.getItemAt(i).text?.toString()
            if (!text.isNullOrBlank()) return text
        }
        return null
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
        submit: suspend (String) -> Unit,
    ) {
        val saveDir = snapshotFlow {
            val state = stateOf()
            val host = hostOf()
            when {
                saving || state.isReadOnly -> null
                selectedDirectory != null && host is HostRef.Local -> selectedDirectory
                !resolved && state.needsPublicDownloadGuide(host, done = false, appDir = appDir) -> null
                else -> ""
            }
        }.filterNotNull().first()
        submit(saveDir)
    }
}

@Composable
private fun ExternalTorrentGuideSheet(
    guide: ExternalTorrentGuide,
    state: HostState,
    host: HostRef,
    onSkip: () -> Unit,
    onCancel: () -> Unit,
) {
    val resolved = guide.resolved
    FluxSheet(
        visible = !resolved || state.isReadOnly,
        onDismissRequest = if (resolved) onCancel else onSkip,
        dismissible = !guide.saving,
        title = str(if (resolved) R.string.mobileTorrentImportPending else R.string.defaultSaveDir),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (resolved) {
                if (state.isReadOnly) {
                    FluxBanner(str(R.string.mobileTorrentImportWaitingForHost), kind = FluxBannerKind.Warn)
                    FluxButton(str(R.string.cancel), onClick = onCancel, enabled = !guide.saving)
                }
            } else {
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

/** 透明舞台：只有「新建下载」Sheet 与浮层（对话框 / toast）；Sheet 关闭后结束窗口。 */
@Composable
private fun ExternalDownloadStage(
    failure: HostException?,
    torrents: List<PendingTorrent>,
    onFailureShown: () -> Unit,
    onSubmitted: () -> Unit,
    onTorrentDone: (PendingTorrent) -> Unit,
    onTorrentCancelled: () -> Unit,
    onClosed: () -> Unit,
) {
    val container = LocalAppContainer.current
    val nav = LocalNavigator.current
    val context = LocalContext.current
    val overlays = rememberFluxOverlayState()
    val scope = rememberCoroutineScope()
    val haptics = FluxTheme.haptics
    val actions = remember(overlays, haptics) {
        TaskActions(context.applicationContext, scope, { container.session }, container.store, overlays, nav, haptics) {
            container.host.value is HostRef.Local
        }
    }
    val close by rememberUpdatedState(onClosed)
    val head = torrents.firstOrNull()
    val state = hostState()
    val host = container.host.collectAsStateWithLifecycle()
    val settings = remember(context) { DeviceSettings.of(context) }
    val appDir = remember(context) { defaultLocalSaveDir(context).path }
    var guide by remember(head) { mutableStateOf<ExternalTorrentGuide?>(null) }
    CompositionLocalProvider(LocalFluxOverlays provides overlays, LocalTaskActions provides actions) {
        Box(Modifier.fillMaxSize()) {
            NewDownloadSheet(route = nav.sheet as? SheetRoute.NewDownload, onDismiss = nav::closeSheet, onSubmitted = onSubmitted)
            guide?.let { activeGuide ->
                ExternalTorrentGuideSheet(
                    guide = activeGuide,
                    state = state.value,
                    host = host.value,
                    onSkip = {
                        if (guide === activeGuide) {
                            activeGuide.dismiss(container.store.state.value, container.host.value)
                        }
                    },
                    onCancel = {
                        if (guide === activeGuide && !activeGuide.saving) {
                            guide = null
                            onTorrentCancelled()
                        }
                    },
                )
            }
            FluxOverlayHost(overlays)
        }
    }
    LaunchedEffect(failure) {
        if (failure != null) {
            overlays.toast(actions.errorText(failure), FluxToastKind.Error)
            onFailureShown()
        }
    }
    var opened by remember { mutableStateOf(false) }
    val sheet = nav.sheet
    LaunchedEffect(sheet) {
        if (sheet != null) {
            opened = true
        } else if (opened) {
            delay(SHEET_EXIT_MS)
            if (torrents.isEmpty()) close()
        }
    }
    // 种子逐个提交；本机首次使用应用默认目录时等用户选定目录或明确跳过。
    LaunchedEffect(head) {
        if (head == null) return@LaunchedEffect
        val ready = awaitReady(container.store)
        suspend fun submit(saveDir: String) {
            if (torrents.firstOrNull() !== head) return
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
            ).let { if (it > 0) onSubmitted() }
            onTorrentDone(head)
        }
        if (ready.needsPublicDownloadGuide(container.host.value, settings.downloadFolderGuideDone, appDir)) {
            val activeGuide = ExternalTorrentGuide(appDir, { settings.downloadFolderGuideDone }, settings::finishDownloadFolderGuide)
            guide = activeGuide
            activeGuide.submitWhenReady({ state.value }, { host.value }, ::submit)
        } else {
            submit("")
        }
    }
}
