package com.fluxdown.app.feature.newtask

import android.content.ClipboardManager
import android.content.Context
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Composable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.core.capture.TorrentFile
import com.fluxdown.app.feature.settings.general.categoryDisplayName
import com.fluxdown.app.feature.settings.general.categoryIcon
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.SheetRoute
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.app.ui.fileCategory
import com.fluxdown.app.ui.label
import com.fluxdown.app.ui.launchDocumentPicker
import com.fluxdown.core.host.ImportRules
import com.fluxdown.core.host.groupImportBatches
import com.fluxdown.core.host.isError
import com.fluxdown.core.host.notice
import com.fluxdown.core.model.CategoryIndex
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.model.TaskProtocol
import com.fluxdown.core.protocol.CloudConnectionDto
import com.fluxdown.core.protocol.CloudPresence
import com.fluxdown.core.protocol.DeviceRules
import com.fluxdown.core.protocol.DispatchTarget
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.JsonValue
import com.fluxdown.core.protocol.patchPreferences
import com.fluxdown.core.protocol.PathStyle
import com.fluxdown.core.store.Connection
import com.fluxdown.fluxui.controls.ButtonSize
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxFieldAction
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxSegmented
import com.fluxdown.fluxui.controls.FluxSelect
import com.fluxdown.fluxui.controls.FluxSplitButton
import com.fluxdown.fluxui.controls.FluxStepper
import com.fluxdown.fluxui.controls.FluxSwitchRow
import com.fluxdown.fluxui.controls.FluxTag
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.SegOption
import com.fluxdown.fluxui.controls.SplitMenuAction
import com.fluxdown.fluxui.data.FileTile
import com.fluxdown.fluxui.data.FileTileSize
import com.fluxdown.fluxui.data.fileTileIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.app.i18n.fill
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogButtonStyle
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxMenuItem
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** 打开计数：每次从关闭到打开新建一份表单；退场动画期间沿用同一份。 */
private class OpenTracker {
    var wasOpen = false
    var generation = 0
    var route = SheetRoute.NewDownload()
}

/**
 * N1 新建下载 + N2 高级选项（叠在其上的第二层 Sheet）。
 * [route] 为 null 时 Sheet 退场；表单状态每次打开重建（默认值来自主机配置）。
 * [onSubmitted]：任务已全部建成、Sheet 即将关闭（外部唤起入口据此保活本机下载）。
 */
@Composable
fun NewDownloadSheet(route: SheetRoute.NewDownload?, onDismiss: () -> Unit, onSubmitted: () -> Unit = {}) {
    val tracker = remember { OpenTracker() }
    if (route != null && !tracker.wasOpen) tracker.generation++
    tracker.wasOpen = route != null
    if (route != null) tracker.route = route
    if (tracker.generation == 0) return
    key(tracker.generation) {
        NewDownloadSheetImpl(visible = route != null, route = tracker.route, onDismiss = onDismiss, onSubmitted = onSubmitted)
    }
}

@Composable
private fun NewDownloadSheetImpl(visible: Boolean, route: SheetRoute.NewDownload, onDismiss: () -> Unit, onSubmitted: () -> Unit) {
    val container = LocalAppContainer.current
    val overlays = LocalFluxOverlays.current
    val actions = LocalTaskActions.current
    val haptics = FluxTheme.haptics
    val context = LocalContext.current
    val form = remember { NewDownloadState.create(route.prefill, container.store.state.value, route.external, route.targetId) }
    LaunchedEffect(route) {
        // 打开期间的后续唤起追加进同一表单（批量协议唤起逐条到达）
        route.external?.let(form::addExternal)
        val extra = route.prefill.trim()
        if (extra.isNotEmpty() && form.urlText.lineSequence().none { it.trim() == extra }) {
            form.urlText = form.urlText.trimEnd().let { if (it.isEmpty()) extra else "$it\n$extra" }
        }
    }
    var advancedOpen by remember { mutableStateOf(false) }
    val dismiss by rememberUpdatedState(onDismiss)
    val submitted by rememberUpdatedState(onSubmitted)

    // 「下载到」候选：云账号其他设备 + 局域网已配对设备（快照实时投影；输入不变不重算）
    val host = hostState()
    val cloudDevices by remember { derivedStateOf { host.value.cloudDevices } }
    val linkDevices by remember { derivedStateOf { host.value.linkDevices } }
    val live by remember { derivedStateOf { host.value.connection == Connection.Live } }
    val connectionRaw by remember { derivedStateOf { host.value.sections[HostSection.agentCloudConnection] } }
    val targets = remember(cloudDevices, linkDevices, live, connectionRaw) {
        val connection = CloudConnectionDto.fromJson(Json.parseOrNull(connectionRaw))
        DeviceRules.dispatchTargets(cloudDevices, linkDevices, CloudPresence.isKnown(connection, live), live)
    }
    val target = form.target(targets)

    val s = Strings(
        discardTitle = str(R.string.mobileDiscardTitle),
        discardMessage = str(R.string.mobileDiscardMessage),
        keepEditing = str(R.string.mobileKeepEditing),
        discard = str(R.string.mobileDiscard),
        readOnly = str(R.string.localServiceDisconnected),
    )

    fun requestClose() {
        if (form.submitting) return
        if (!form.isDirty) {
            dismiss()
            return
        }
        overlays.showDialog(
            FluxDialogSpec(
                title = s.discardTitle,
                message = s.discardMessage,
                icon = FluxIcons.TriangleAlert,
                buttons = listOf(
                    FluxDialogButton(s.keepEditing),
                    FluxDialogButton(s.discard, FluxDialogButtonStyle.Destructive) { dismiss() },
                ),
            ),
        )
    }

    fun rememberMode() {
        if (target == null) {
            val session = container.session
            val mode = form.rememberedMode()
            container.appScope.launch {
                try {
                    session.patchPreferences(
                        mapOf(NewDownloadState.LAST_CATEGORY_MODE_KEY to JsonValue.of(mode)),
                        sync = false,
                    )
                } catch (e: com.fluxdown.core.host.HostException) {
                    overlays.toast(actions.errorText(e), FluxToastKind.Error)
                }
            }
        }
    }
    fun submit(startPaused: Boolean, queueId: String) {
        if (form.submitting) return
        val state = container.store.state.value
        if (state.isReadOnly) {
            haptics.reject()
            overlays.toast(s.readOnly, FluxToastKind.Error, FluxIcons.WifiOff)
            return
        }
        if (form.entries.isEmpty()) {
            haptics.reject()
            form.showEmptyError = true
            return
        }
        if (!form.saveDirValid) {
            haptics.reject()
            return
        }
        // 本机主机：目录不可写（分区存储外 / 拼错路径）在提交前拦住，避免引擎报 EPERM
        if (container.host.value is com.fluxdown.core.model.HostRef.Local && !com.fluxdown.app.ui.isLocalDirWritable(form.saveDir)) {
            haptics.reject()
            overlays.toast(context.str(R.string.mobileSaveDirNotWritable, "dir" to form.saveDir.trim()), FluxToastKind.Error)
            return
        }
        if (!checksumValid(form)) {
            haptics.reject()
            advancedOpen = true
            return
        }
        val requests = form.buildRequests(startPaused, queueId, manualProxyUrl(state.config))
        val batches = groupImportBatches(requests)
        if (requests.size > ImportRules.CHUNK_SIZE) {
            // 多块导入：交给进程级导入中心，进度 / 取消在下载页顶部，关闭本 Sheet 不影响它；汇总提示由 ImportEffects 给出
            rememberMode()
            container.imports.start(container.session, batches, startPaused, notify = true)
            haptics.confirm()
            submitted()
            dismiss()
            return
        }
        // 单块导入：Sheet 内联等待（提交中状态）；全部成功才关闭，失败 + 未提交的条目留在表单里供重试
        form.submitting = true
        container.appScope.launch {
            val summary = container.imports.start(container.session, batches, startPaused, notify = false).await()
            form.submitting = false
            val notice = summary.notice()
            val text = importNoticeText(context.resources, notice, summary.succeeded, startPaused) { actions.errorText(it) }
            if (summary.succeeded > 0) {
                rememberMode()
                submitted()
            }
            if (summary.unfinished.isEmpty()) {
                haptics.confirm()
                overlays.toast(text, FluxToastKind.Accent, if (startPaused) FluxIcons.Clock else FluxIcons.ArrowDown)
                dismiss()
            } else {
                haptics.reject()
                overlays.toast(text, FluxToastKind.Error)
                form.keepOnly(summary.unfinished.mapTo(HashSet()) { it.url })
            }
        }
    }

    /** 「下载到」其他设备：逐条下发链接 / 文件名 / 保存目录；全部成功关闭，有失败时只把失败的链接留在文本框里。 */
    fun submitRemote(target: DispatchTarget) {
        if (form.submitting) return
        if (container.store.state.value.isReadOnly) {
            haptics.reject()
            overlays.toast(s.readOnly, FluxToastKind.Error, FluxIcons.WifiOff)
            return
        }
        if (form.entries.isEmpty()) {
            haptics.reject()
            form.showEmptyError = true
            return
        }
        val saveDir = when (val check = DeviceRules.checkSaveDir(form.remoteSaveDir, target.pathStyle)) {
            DeviceRules.SaveDirCheck.Invalid -> {
                haptics.reject()
                return
            }
            DeviceRules.SaveDirCheck.UseDefault -> null
            is DeviceRules.SaveDirCheck.Explicit -> check.dir
        }
        val items = form.dispatchItems()
        val session = container.session
        form.submitting = true
        form.dispatchFailure = null
        form.dispatchProgress = 0 to items.size
        container.appScope.launch {
            val summary = Dispatcher.run(items, target, saveDir, session) { done -> form.dispatchProgress = done to items.size }
            form.dispatchProgress = null
            form.submitting = false
            val outcome = Dispatcher.outcome(context, summary, target)
            when {
                outcome.failure == null -> {
                    haptics.confirm()
                    outcome.toast?.let { overlays.toast(it, FluxToastKind.Success, FluxIcons.Send) }
                    dismiss()
                }
                else -> {
                    haptics.reject()
                    if (outcome.partial) outcome.toast?.let { overlays.toast(it, FluxToastKind.Warn) }
                    form.retain(summary.failedEntries)
                    form.dispatchFailure = outcome.failure
                }
            }
        }
    }

    FluxSheet(
        visible = visible,
        onDismissRequest = ::requestClose,
        detent = FluxSheetDetent.Full,
        title = str(R.string.newDownload),
        header = { FluxSheetHeader(title = str(R.string.newDownload), onClose = ::requestClose) },
        footer = {
            NewDownloadFooter(form, target, ::submit, onSubmitRemote = { target?.let(::submitRemote) })
        },
    ) {
        NewDownloadContent(
            form, targets, target,
            onOpenAdvanced = { advancedOpen = true },
            // 本机建了任务：记住分类模式、拉起前台服务；表单里没有待提交的链接时关闭（同 iOS `importTorrents`）
            onTorrentImported = {
                rememberMode()
                submitted()
                if (form.urlText.isBlank()) dismiss()
            },
        )
    }

    FluxSheet(
        visible = visible && advancedOpen,
        onDismissRequest = { advancedOpen = false },
        detent = FluxSheetDetent.Full,
        title = str(R.string.taskProxyAdvanced),
        header = {
            FluxSheetHeader(
                title = str(R.string.taskProxyAdvanced),
                subtitle = str(R.string.mobileAdvancedSub),
                onClose = { advancedOpen = false },
            )
        },
        footer = {
            FluxSheetFooter {
                FluxButton(str(R.string.mobileReset), { form.advanced.reset() }, variant = ButtonVariant.Ghost, modifier = Modifier.weight(1f))
                FluxButton(str(R.string.confirm), { advancedOpen = false }, variant = ButtonVariant.Primary, modifier = Modifier.weight(1f))
            }
        },
    ) {
        AdvancedContent(form)
    }
}

private class Strings(
    val discardTitle: String,
    val discardMessage: String,
    val keepEditing: String,
    val discard: String,
    val readOnly: String,
)

private val HexLengths = setOf(32, 40, 64, 128)

private fun checksumHexValid(hex: String): Boolean {
    val h = hex.trim()
    return h.isEmpty() || (h.length in HexLengths && h.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' })
}

private fun checksumValid(form: NewDownloadState): Boolean = !form.single || checksumHexValid(form.advanced.checksumHex)

// ── N1 正文 ───────────────────────────────────────────────────────────────

@Composable
private fun NewDownloadContent(
    form: NewDownloadState,
    targets: List<DispatchTarget>,
    target: DispatchTarget?,
    onOpenAdvanced: () -> Unit,
    onTorrentImported: () -> Unit,
) {
    val container = LocalAppContainer.current
    val overlays = LocalFluxOverlays.current
    val haptics = FluxTheme.haptics
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val context = LocalContext.current
    val host = hostState()
    val hostRef by container.host.collectAsStateWithLifecycle()
    val remoteHost = hostRef is HostRef.Remote

    val queues by remember { derivedStateOf { host.value.queues } }
    val categories by remember { derivedStateOf { host.value.categories } }
    val duplicateName by remember {
        derivedStateOf {
            val first = form.entries.firstOrNull()?.url
            if (first == null) null else host.value.tasks.firstOrNull { it.url == first || it.originUrl == first }?.fileName
        }
    }

    val clipboardEmpty = str(R.string.mobileClipboardEmpty)
    val pasted = str(R.string.mobilePasted)
    val importNone = str(R.string.importTxtNoUrls)
    val noFilePicker = str(R.string.mobileNoFilePicker)
    val importFound = str(R.string.importTxtFound)
    val noEntries = form.entries.isEmpty()
    val hasText = form.urlText.isNotBlank()
    val busy = form.submitting
    // 重命名 / 首条链接变化 → 自动模式下按预测目录跟随（用户手改过目录则不覆盖）
    val predictedDir = form.predictedDir()
    LaunchedEffect(predictedDir) { form.syncAuto() }

    val importLauncher = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri: Uri? ->
        if (uri == null) return@rememberLauncherForActivityResult
        container.appScope.launch {
            val text = withContext(Dispatchers.IO) { readText(context, uri) }
            val found = text?.let { withContext(Dispatchers.Default) { parseEntries(it, loose = true).dedupe() } }.orEmpty()
            if (found.isEmpty()) {
                haptics.reject()
                overlays.toast(importNone, FluxToastKind.Warn)
            } else {
                val (merged, added) = appendEntries(form.urlText, found)
                form.urlText = merged
                overlays.toast(importFound.fill("count" to added), FluxToastKind.Success, FluxIcons.FileText)
            }
        }
    }
    var importingTorrent by remember { mutableStateOf(false) }
    val taskActions = LocalTaskActions.current
    val torrentLauncher = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris: List<Uri> ->
        if (uris.isEmpty() || importingTorrent || form.submitting) return@rememberLauncherForActivityResult
        if (!form.saveDirValid) {
            haptics.reject()
            return@rememberLauncherForActivityResult
        }
        // 本机主机：目录不可写在提交前拦住（同 `submit`）
        if (container.host.value is HostRef.Local && !com.fluxdown.app.ui.isLocalDirWritable(form.saveDir)) {
            haptics.reject()
            overlays.toast(context.str(R.string.mobileSaveDirNotWritable, "dir" to form.saveDir.trim()), FluxToastKind.Error)
            return@rememberLauncherForActivityResult
        }
        val queues = container.store.state.value.queues
        val queueId = (queues.firstOrNull { it.queueId == form.queueId } ?: queues.firstOrNull())?.queueId ?: form.queueId
        importingTorrent = true
        container.appScope.launch {
            val created = try {
                TorrentImport.submit(
                    context, container, overlays, taskActions::errorText, uris, form.saveDir, queueId, startPaused = false,
                )
            } finally {
                importingTorrent = false
            }
            if (created > 0) {
                haptics.confirm()
                onTorrentImported()
            } else {
                haptics.reject()
            }
        }
    }
    val pickDirectory = com.fluxdown.app.ui.rememberLocalDirectoryPicker { path ->
        if (container.host.value is HostRef.Local && target == null && !form.submitting) {
            form.setSaveDirManually(path)
        }
    }

    Column(Modifier.padding(top = 2.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        FluxText(str(R.string.batchDownloadDesc), style = type.sm, color = c.inkMuted)

        // 链接区
        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            FluxField(
                value = form.urlText,
                onValueChange = {
                    form.urlText = it
                    form.showEmptyError = false
                },
                label = str(R.string.downloadUrl),
                count = if (form.entries.isNotEmpty()) str(R.string.urlCount, "count" to form.entries.size) else null,
                placeholder = str(R.string.batchUrlPlaceholder),
                mono = true,
                singleLine = false,
                rows = 5,
                // 多行批量链接框不能用 KeyboardType.Uri：小米 / 搜狗 / 百度等输入法对 URI 变体固定给「前往」键，
                // 无视 TYPE_TEXT_FLAG_MULTI_LINE，回车换不了行（#1010）。Text + Default 才稳定出换行键。
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Text,
                    imeAction = ImeAction.Default,
                ),
                error = when {
                    hasText && noEntries -> str(R.string.newDownloadNoValidUrl)
                    !hasText && form.showEmptyError -> str(R.string.mobileEnterUrl)
                    else -> null
                },
                enabled = !busy,
                trailing = {
                    FluxFieldAction(FluxIcons.ClipboardPaste, str(R.string.mobilePaste), onClick = {
                        val clip = readClipboard(context)
                        if (clip.isNullOrBlank()) {
                            haptics.reject()
                            overlays.toast(clipboardEmpty, FluxToastKind.Warn)
                        } else {
                            form.appendText(clip)
                            form.showEmptyError = false
                            overlays.toast(pasted, FluxToastKind.Info, FluxIcons.ClipboardPaste)
                        }
                    })
                },
            )
            FluxButton(
                str(R.string.importTxtFile),
                { launchDocumentPicker(overlays, noFilePicker) { importLauncher.launch(arrayOf("text/*", "application/octet-stream")) } },
                size = ButtonSize.Sm,
                icon = FluxIcons.FileText,
                fullWidth = true,
                enabled = !busy,
            )
            FluxButton(
                str(R.string.openTorrentFile),
                { launchDocumentPicker(overlays, noFilePicker) { torrentLauncher.launch(arrayOf(TorrentFile.MIME_TYPE, "application/octet-stream")) } },
                size = ButtonSize.Sm,
                icon = FluxIcons.FileUp,
                fullWidth = true,
                enabled = !busy && !importingTorrent && target == null,
            )
            // 种子只能在当前主机建任务：「下载到」选了其他设备时禁用并说明（同 iOS `torrentButton(remote:)`）
            if (target != null) {
                FluxText(
                    str(R.string.downloadToTorrentLocalOnly),
                    style = type.sm,
                    color = c.inkFaint,
                    modifier = Modifier.padding(horizontal = 6.dp),
                )
            }
        }

        // 预览：前 3 条
        if (form.entries.isNotEmpty()) {
            val shown = form.entries.take(3)
            val index = remember(categories) { CategoryIndex(categories) }
            GlassSection {
                shown.forEach { e ->
                    row(hasIcon = true) {
                        val name = inferName(e)
                        val proto = protocolOf(e.url)
                        val cat = index.categoryOf(name, e.url).fileCategory()
                        FluxListRow(
                            title = name,
                            subtitle = hostOrNull(e.url) ?: proto.previewTag(),
                            leading = { FileTile(fileTileIcon(cat, proto == TaskProtocol.Bt), c.category(cat), size = FileTileSize.Sm) },
                            trailing = { FluxTag(proto.previewTag()) },
                        )
                    }
                }
            }
            if (form.entries.size > 3) {
                FluxText(
                    str(R.string.mobileNewDownloadPreviewMore, "n" to (form.entries.size - 3)),
                    style = type.monoS,
                    color = c.inkFaint,
                    modifier = Modifier.padding(horizontal = 6.dp),
                )
            }
            duplicateName?.let { name ->
                FluxBanner(
                    text = str(R.string.mobileNewDownloadDuplicateUrl, "name" to name),
                    kind = FluxBannerKind.Warn,
                    slim = true,
                )
            }
        }

        // 下载到：当前主机 / 云账号其他设备 / 局域网已配对设备
        if (targets.isNotEmpty()) {
            TargetSelect(form, targets, target, hostRef, enabled = !busy)
        }
        if (target != null) {
            // 下发只带链接 / 文件名 / 保存目录：线程、队列与高级选项只对当前主机有意义（同 iOS / GPUI）
            RemoteDestination(form, target, enabled = !busy)
            return@Column
        }

        CategorySelect(form, enabled = !busy, remoteHost = remoteHost)

        // 保存目录
        val saveDirShown = remember(form.saveDir, remoteHost) {
            if (remoteHost) form.saveDir else com.fluxdown.app.ui.friendlyLocalPath(context, form.saveDir)
        }
        FluxField(
            value = saveDirShown,
            onValueChange = { form.setSaveDirManually(it) },
            label = str(R.string.saveDir),
            mono = remoteHost,
            readOnly = !remoteHost,
            error = if (form.saveDirValid) null else str(R.string.mobileSaveDirInvalid),
            enabled = !busy,
            trailing = if (remoteHost) null else ({
                FluxFieldAction(FluxIcons.FolderOpen, str(R.string.browse), onClick = { pickDirectory(form.saveDir) })
            }),
        )

        // 重命名（仅单条）
        if (form.single) {
            FluxField(
                value = form.rename,
                onValueChange = { form.rename = it },
                label = str(R.string.renameTask),
                placeholder = str(R.string.autoDetectFilename),
                enabled = !busy,
            )
        }

        // 线程数
        if (form.threadsApplicable) {
            val threadItems = buildList {
                add(threadItem(str(R.string.auto), form.threadMode == ThreadMode.Auto) { form.threadMode = ThreadMode.Auto })
                ThreadPresets.forEach { n ->
                    add(
                        threadItem(
                            str(R.string.nThreads, "n" to n),
                            form.threadMode == ThreadMode.Preset && form.presetThreads == n,
                        ) {
                            form.threadMode = ThreadMode.Preset
                            form.presetThreads = n
                        },
                    )
                }
                add(threadItem(str(R.string.customThreads), form.threadMode == ThreadMode.Custom) { form.threadMode = ThreadMode.Custom })
            }
            val threadLabel = when (form.threadMode) {
                ThreadMode.Auto -> str(R.string.auto)
                ThreadMode.Preset -> str(R.string.nThreads, "n" to form.presetThreads)
                ThreadMode.Custom -> str(R.string.nThreads, "n" to form.customThreads)
            }
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                MenuSelect(threadLabel, str(R.string.threads), null, FluxIcons.Layers, threadItems, enabled = !busy)
                if (form.threadMode == ThreadMode.Custom) {
                    FluxStepper(
                        value = form.customThreads,
                        onValueChange = { form.customThreads = it },
                        range = 1..MaxThreads,
                        label = str(R.string.customThreads),
                        editable = true,
                    )
                    FluxText(str(R.string.customThreadsHint), style = type.sm, color = c.inkFaint, modifier = Modifier.padding(horizontal = 6.dp))
                }
            }
        }

        // 队列
        if (queues.isNotEmpty()) {
            val selected = queues.firstOrNull { it.queueId == form.queueId } ?: queues.first()
            val running = str(R.string.queueRunningBadge)
            val stopped = str(R.string.queueStoppedBadge)
            val items = queues.map { q ->
                FluxMenuItem.Action(
                    label = q.label(),
                    onClick = { form.queueId = q.queueId },
                    icon = FluxIcons.Rows3,
                    hint = if (q.isRunning) running else stopped,
                    checked = q.queueId == selected.queueId,
                )
            }
            MenuSelect(
                value = selected.label(),
                label = str(R.string.taskQueueLabel),
                hint = if (selected.isRunning) running else stopped,
                leadingIcon = FluxIcons.Rows3,
                items = items,
                enabled = !busy,
            )
        }

        // 高级选项入口
        val modified = form.advanced.modified(form.single, form.singleHttp)
        val none = str(R.string.mobileAdvancedNone)
        val headerCount = form.advanced.headers.count { h -> h.key.isNotBlank() }
        val summary = if (modified.isEmpty()) none else modified.map { advancedLabel(it, headerCount) }.joinToString(" · ")
        GlassSection {
            row(hasIcon = true) {
                FluxListRow(
                    title = str(R.string.taskProxyAdvanced),
                    subtitle = summary,
                    icon = FluxIcons.SlidersHorizontal,
                    chevron = true,
                    trailing = if (modified.isEmpty()) null else ({ Box(Modifier.size(6.dp).background(c.ink, CircleShape)) }),
                    onClick = onOpenAdvanced,
                )
            }
        }
    }
}

/** 「下载到」选择：当前主机（本机 / 远端主机名）+ 远端目标；下方提示随所选目标变化（同 iOS `targetSection`）。 */
@Composable
private fun TargetSelect(
    form: NewDownloadState,
    targets: List<DispatchTarget>,
    selected: DispatchTarget?,
    hostRef: HostRef,
    enabled: Boolean,
) {
    val hostLabel = when (hostRef) {
        is HostRef.Local -> str(R.string.thisDevice)
        is HostRef.Remote -> hostRef.displayName.trim().ifEmpty { str(R.string.webDownloadToServer) }
    }
    val online = str(R.string.deviceOnline)
    val offline = str(R.string.deviceOffline)
    val unknown = str(R.string.devicePresenceUnknown)
    val localTag = str(R.string.deviceLocalTag)
    val offlineCloud = str(R.string.downloadToOfflineHint)
    val offlineLink = str(R.string.errReasonPeerOffline)
    val optionsIgnored = str(R.string.downloadToRemoteOptionsIgnored)
    val hostHint = str(R.string.downloadToHint)

    fun status(t: DispatchTarget): String = when (t.online) {
        true -> online
        false -> offline
        null -> unknown
    }

    /** 同 GPUI `target_label`：云设备 `状态`，已配对设备 `局域网 · 状态`。 */
    fun detail(t: DispatchTarget): String = if (t.isCloud) status(t) else "$localTag · ${status(t)}"

    fun choose(id: String?) {
        form.targetId = id
        form.dispatchFailure = null
    }

    val items = buildList {
        add(
            FluxMenuItem.Action(
                label = hostLabel,
                onClick = { choose(null) },
                icon = if (hostRef is HostRef.Local) FluxIcons.Smartphone else FluxIcons.Server,
                checked = selected == null,
            ),
        )
        add(FluxMenuItem.Divider)
        targets.forEach { t ->
            add(
                FluxMenuItem.Action(
                    label = t.name,
                    onClick = { choose(t.id) },
                    icon = if (t.isCloud) FluxIcons.Cloud else FluxIcons.Wifi,
                    hint = detail(t),
                    checked = t.id == selected?.id,
                ),
            )
        }
    }
    // 目标非在线：云设备离线由云端排队，局域网设备离线送不到，状态未知如实说明
    val hint = if (selected == null) {
        hostHint
    } else {
        val presence = when (selected.online) {
            true -> null
            false -> if (selected.isCloud) offlineCloud else offlineLink
            null -> unknown
        }
        listOfNotNull(presence, optionsIgnored).joinToString("\n")
    }
    MenuSelect(
        value = selected?.let { "${it.name} · ${detail(it)}" } ?: hostLabel,
        label = str(R.string.downloadTo),
        hint = hint,
        leadingIcon = when {
            selected == null -> if (hostRef is HostRef.Local) FluxIcons.Smartphone else FluxIcons.Server
            selected.isCloud -> FluxIcons.Cloud
            else -> FluxIcons.Wifi
        },
        items = items,
        enabled = enabled,
    )
}

/**
 * 「分类」选择：自动（按文件名跟随）/ 不分类 / 各已配置目录的分类；窄屏单独一行，样式同表单内其它选择行。
 * 本机目录按存储卷友好显示（`/storage/emulated/0/…` → 「内部存储 › …」，同保存目录行），远端主机的路径
 * （如 Windows `D:\下载\…`）原样显示；均单行中间省略。
 */
@Composable
private fun CategorySelect(form: NewDownloadState, enabled: Boolean, remoteHost: Boolean) {
    val context = LocalContext.current
    val shownDirs = remember(form.categoryOptions, remoteHost) {
        form.categoryOptions.associate { option ->
            option.id to if (remoteHost) option.saveDir else com.fluxdown.app.ui.friendlyLocalPath(context, option.saveDir)
        }
    }
    val mode = form.mode
    val matched = form.autoMatched()
    val auto = str(R.string.saveDirCategoryAuto)
    val none = str(R.string.saveDirCategoryNone)
    val items = buildList {
        add(
            FluxMenuItem.Action(
                label = auto,
                onClick = { form.selectMode(SaveDirMode.Auto) },
                icon = FluxIcons.Folders,
                hint = str(R.string.saveDirCategoryAutoDesc),
                checked = mode == SaveDirMode.Auto,
            ),
        )
        add(
            FluxMenuItem.Action(
                label = none,
                onClick = { form.selectMode(SaveDirMode.None) },
                icon = FluxIcons.FolderOpen,
                hint = str(R.string.saveDirCategoryNoneDesc),
                checked = mode == SaveDirMode.None,
            ),
        )
        if (form.categoryOptions.isNotEmpty()) add(FluxMenuItem.Divider)
        form.categoryOptions.forEach { option ->
            add(
                FluxMenuItem.Action(
                    label = categoryDisplayName(context, option),
                    onClick = { form.selectMode(SaveDirMode.Category(option.id)) },
                    icon = categoryIcon(option.icon),
                    hint = shownDirs[option.id] ?: option.saveDir,
                    hintIsPath = true,
                    checked = mode == SaveDirMode.Category(option.id),
                ),
            )
        }
    }
    val selected = (mode as? SaveDirMode.Category)?.let { m -> form.categoryOptions.firstOrNull { it.id == m.id } }
    val value = when (mode) {
        SaveDirMode.Auto -> matched?.let { str(R.string.saveDirCategoryAutoMatched, "name" to categoryDisplayName(context, it)) } ?: auto
        SaveDirMode.None -> none
        is SaveDirMode.Category -> selected?.let { categoryDisplayName(context, it) } ?: auto
    }
    val icon = when (mode) {
        SaveDirMode.Auto -> matched?.let { categoryIcon(it.icon) } ?: FluxIcons.Folders
        SaveDirMode.None -> FluxIcons.FolderOpen
        is SaveDirMode.Category -> selected?.let { categoryIcon(it.icon) } ?: FluxIcons.Folders
    }
    MenuSelect(
        value = value,
        label = str(R.string.saveDirCategory),
        hint = when (mode) {
            SaveDirMode.Auto -> str(R.string.saveDirCategoryAutoDesc)
            SaveDirMode.None -> str(R.string.saveDirCategoryNoneDesc)
            // 所选分类的目录已显示在下方「保存目录」行
            is SaveDirMode.Category -> null
        },
        leadingIcon = icon,
        items = items,
        enabled = enabled,
    )
}

/** 远端目标的保存目录（目标设备上的路径，按其路径风格校验；留空 = 目标设备默认目录）+ 重命名 + 下发进度 / 失败说明。 */
@Composable
private fun RemoteDestination(form: NewDownloadState, target: DispatchTarget, enabled: Boolean) {
    val invalid = DeviceRules.checkSaveDir(form.remoteSaveDir, target.pathStyle) == DeviceRules.SaveDirCheck.Invalid
    FluxField(
        value = form.remoteSaveDir,
        onValueChange = { form.remoteSaveDir = it },
        label = str(R.string.saveDir),
        placeholder = target.defaultSaveDir?.let { str(R.string.downloadToRemoteDirDefault, "dir" to it) }
            ?: str(R.string.downloadToRemoteDirUseDefault),
        hint = str(R.string.downloadToRemoteDirHint),
        error = if (invalid) str(R.string.downloadToPathInvalid, "example" to PathStyle.example(target.pathStyle)) else null,
        mono = true,
        keyboardOptions = KeyboardOptions(
            capitalization = KeyboardCapitalization.None,
            autoCorrectEnabled = false,
            keyboardType = KeyboardType.Uri,
        ),
        enabled = enabled,
    )
    if (form.single) {
        FluxField(
            value = form.rename,
            onValueChange = { form.rename = it },
            label = str(R.string.renameTask),
            placeholder = str(R.string.autoDetectFilename),
            enabled = enabled,
        )
    }
    val progress = form.dispatchProgress
    val failure = form.dispatchFailure
    when {
        progress != null -> FluxBanner(
            text = str(R.string.mobileDispatchSending, "done" to progress.first, "total" to progress.second),
            kind = FluxBannerKind.Info,
            slim = true,
        )
        failure != null -> FluxBanner(text = failure, kind = FluxBannerKind.Error, slim = true)
    }
}

private fun threadItem(label: String, checked: Boolean, onClick: () -> Unit) =
    FluxMenuItem.Action(label = label, onClick = onClick, checked = checked)

private fun TaskProtocol.previewTag(): String = when (this) {
    TaskProtocol.Http -> "HTTP"
    TaskProtocol.Ftp -> "FTP"
    TaskProtocol.Bt -> "BT"
    TaskProtocol.Ed2k -> "ED2K"
    TaskProtocol.Hls -> "HLS"
    TaskProtocol.Plugin -> "PLUGIN"
}

@Composable
private fun advancedLabel(item: AdvancedItem, headerCount: Int): String = when (item) {
    AdvancedItem.Auth -> str(R.string.taskHttpAuth)
    AdvancedItem.Proxy -> str(R.string.taskProxy)
    AdvancedItem.UserAgent -> str(R.string.userAgent)
    AdvancedItem.Cookie -> str(R.string.taskCookie)
    AdvancedItem.Referrer -> str(R.string.mobileReferrer)
    AdvancedItem.Checksum -> str(R.string.taskChecksum)
    AdvancedItem.Headers -> str(R.string.taskHeaders) + " ×$headerCount"
    AdvancedItem.Tls -> "TLS"
}

/** FluxSelect + 浮层菜单：点按时以字段包围盒为锚点弹出 [items]。 */
@Composable
private fun MenuSelect(
    value: String,
    label: String,
    hint: String?,
    leadingIcon: ImageVector?,
    items: List<FluxMenuItem>,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    val overlays = LocalFluxOverlays.current
    var bounds by remember { mutableStateOf(Rect.Zero) }
    FluxSelect(
        value = value,
        onClick = { overlays.showMenu(bounds, items) },
        modifier = modifier.onGloballyPositioned { bounds = it.boundsInRoot() },
        label = label,
        hint = hint,
        enabled = enabled,
        leadingIcon = leadingIcon,
    )
}

private fun readClipboard(context: Context): String? {
    val cm = context.getSystemService(ClipboardManager::class.java) ?: return null
    val clip = cm.primaryClip ?: return null
    if (clip.itemCount == 0) return null
    return clip.getItemAt(0).coerceToText(context)?.toString()
}

/** 读取文本文件（上限 2 MiB，防止误选大文件）。 */
private fun readText(context: Context, uri: Uri): String? = runCatching {
    context.contentResolver.openInputStream(uri)?.use { input ->
        val limit = 2 * 1024 * 1024
        val out = java.io.ByteArrayOutputStream()
        val buf = ByteArray(16 * 1024)
        while (out.size() < limit) {
            val n = input.read(buf, 0, minOf(buf.size, limit - out.size()))
            if (n < 0) break
            out.write(buf, 0, n)
        }
        String(out.toByteArray(), Charsets.UTF_8)
    }
}.getOrNull()

// ── 页脚 ──────────────────────────────────────────────────────────────────

@Composable
private fun NewDownloadFooter(
    form: NewDownloadState,
    target: DispatchTarget?,
    submit: (startPaused: Boolean, queueId: String) -> Unit,
    onSubmitRemote: () -> Unit,
) {
    val overlays = LocalFluxOverlays.current
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val host = hostState()
    val queues by remember { derivedStateOf { host.value.queues } }
    val count = form.entries.size
    val selected = queues.firstOrNull { it.queueId == form.queueId } ?: queues.firstOrNull()
    val startLabel = if (count > 1) str(R.string.startBatchDownload, "count" to count) else str(R.string.startDownload)
    if (target != null) {
        // 下发只有「立即开始」：没有队列，也没有稍后下载（同 iOS / GPUI）
        FluxSheetFooter {
            FluxButton(
                startLabel,
                onSubmitRemote,
                modifier = Modifier.weight(1f),
                variant = ButtonVariant.Primary,
                icon = FluxIcons.ArrowDown,
                enabled = !form.submitting,
                loading = form.submitting,
            )
        }
        return
    }
    val later = str(R.string.downloadLater)
    val caption = selected?.let { str(R.string.laterIntoQueueTooltip, "name" to it.label()) }

    val startItems = queues.map { q ->
        FluxMenuItem.Action(
            label = str(R.string.mobileStartIntoQueue, "name" to q.label()),
            onClick = { submit(false, q.queueId) },
            icon = FluxIcons.ArrowDown,
            checked = q.queueId == selected?.queueId,
        )
    }
    val laterItems = queues.map { q ->
        FluxMenuItem.Action(
            label = str(R.string.mobileLaterIntoQueue, "name" to q.label()),
            onClick = { submit(true, q.queueId) },
            icon = FluxIcons.Clock,
        )
    }
    val a11y = (startItems + laterItems).map { SplitMenuAction(it.label, it.onClick) }
    val moreLabel = str(R.string.moreActions)
    val queueId = selected?.queueId ?: form.queueId

    FluxSheetFooter {
        Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                FluxButton(
                    later,
                    { submit(true, queueId) },
                    modifier = Modifier.weight(1f),
                    icon = FluxIcons.Clock,
                    enabled = !form.submitting,
                )
                FluxSplitButton(
                    label = startLabel,
                    onClick = { submit(false, queueId) },
                    onMenuClick = { anchor ->
                        overlays.showMenu(anchor, startItems + FluxMenuItem.Divider + laterItems)
                    },
                    modifier = Modifier.weight(1.4f),
                    icon = FluxIcons.ArrowDown,
                    enabled = !form.submitting,
                    menuLabel = moreLabel,
                    menuActions = a11y,
                )
            }
            if (caption != null) {
                FluxText(caption, style = type.micro, color = c.inkFaint, maxLines = 2)
            }
        }
    }
}

// ── N2 高级 ───────────────────────────────────────────────────────────────

@Composable
private fun AdvancedContent(form: NewDownloadState) {
    val adv = form.advanced
    val container = LocalAppContainer.current
    val manualProxy = remember { manualProxyUrl(container.store.state.value.config) }
    val single = form.single

    Column(Modifier.padding(top = 2.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (form.entries.size > 1) {
            FluxBanner(text = str(R.string.mobileAdvancedBatchHint), kind = FluxBannerKind.Info, slim = true)
        }

        // HTTP 认证（仅单条 http(s)）
        if (single && form.singleHttp) {
            GlassSection(title = str(R.string.taskHttpAuth), footer = str(R.string.taskHttpAuthDesc)) {
                custom(padded = true) {
                    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        FluxField(
                            value = adv.httpUser,
                            onValueChange = { adv.httpUser = it },
                            label = str(R.string.taskHttpAuthUser),
                            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                        )
                        PasswordInput(adv.httpPassword, { adv.httpPassword = it }, str(R.string.taskHttpAuthPassword))
                    }
                }
                row {
                    FluxSwitchRow(
                        title = str(R.string.taskHttpAuthSaveForSite),
                        checked = adv.saveSiteAuth,
                        onCheckedChange = { adv.saveSiteAuth = it },
                    )
                }
            }
        }

        // 任务代理
        GlassSection(title = str(R.string.taskProxy), footer = str(R.string.taskProxyDesc)) {
            custom(padded = true) {
                val notConfigured = str(R.string.proxyNotConfigured)
                val items = ProxyChoice.entries.map { choice ->
                    FluxMenuItem.Action(
                        label = proxyLabel(choice),
                        onClick = { adv.proxyChoice = choice },
                        hint = if (choice == ProxyChoice.GlobalManual) manualProxy.ifEmpty { notConfigured } else null,
                        checked = adv.proxyChoice == choice,
                        enabled = choice != ProxyChoice.GlobalManual || manualProxy.isNotEmpty(),
                    )
                }
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    MenuSelect(proxyLabel(adv.proxyChoice), str(R.string.taskProxy), null, FluxIcons.Network, items)
                    if (adv.proxyChoice == ProxyChoice.Custom) {
                        FluxField(
                            value = adv.proxyCustom,
                            onValueChange = { adv.proxyCustom = it },
                            placeholder = str(R.string.taskProxyPlaceholder),
                            mono = true,
                            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false, keyboardType = KeyboardType.Uri),
                        )
                    }
                }
            }
        }

        // User-Agent
        GlassSection(title = str(R.string.userAgent)) {
            custom(padded = true) {
                val default = str(R.string.userAgentPresetDefault)
                val custom = str(R.string.userAgentPresetCustom)
                val names = mapOf(
                    UaDefault to str(R.string.queueUaInheritGlobal),
                    "chrome" to str(R.string.userAgentPresetChrome),
                    "firefox" to str(R.string.userAgentPresetFirefox),
                    "edge" to str(R.string.userAgentPresetEdge),
                    "safari" to str(R.string.userAgentPresetSafari),
                    UaCustom to custom,
                )
                val items = names.map { (key, name) ->
                    FluxMenuItem.Action(
                        label = name,
                        onClick = {
                            adv.uaPreset = key
                            if (key != UaCustom) adv.userAgent = uaPresetValue(key)
                        },
                        checked = adv.uaPreset == key,
                    )
                }
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    MenuSelect(names[adv.uaPreset] ?: default, str(R.string.userAgent), null, FluxIcons.Globe, items)
                    FluxField(
                        value = adv.userAgent,
                        onValueChange = {
                            adv.userAgent = it
                            adv.uaPreset = detectUaPreset(it)
                        },
                        placeholder = str(R.string.userAgentTaskPlaceholder),
                        mono = true,
                        singleLine = false,
                        rows = 2,
                        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                    )
                }
            }
        }

        // Cookie + 来源页
        GlassSection(title = str(R.string.taskCookie), footer = if (form.entries.size > 1) str(R.string.taskCookieBatchDesc) else str(R.string.taskCookieDesc)) {
            custom(padded = true) {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    FluxField(
                        value = adv.cookie,
                        onValueChange = { adv.cookie = it },
                        placeholder = str(R.string.taskCookiePlaceholder),
                        mono = true,
                        singleLine = false,
                        rows = 3,
                        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                    )
                    FluxField(
                        value = adv.referrer,
                        onValueChange = { adv.referrer = it },
                        label = str(R.string.mobileReferrer),
                        placeholder = "https://",
                        mono = true,
                        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false, keyboardType = KeyboardType.Uri),
                    )
                }
            }
        }

        // 哈希校验（仅单条）
        if (single) {
            GlassSection(title = str(R.string.taskChecksum), footer = str(R.string.taskChecksumDesc)) {
                custom(padded = true) {
                    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        FluxSegmented(
                            options = HashAlgorithms.map { SegOption(it, it) },
                            selected = adv.checksumAlgo,
                            onSelect = { adv.checksumAlgo = it },
                            small = true,
                        )
                        FluxField(
                            value = adv.checksumHex,
                            onValueChange = { adv.checksumHex = it.trim() },
                            placeholder = str(R.string.taskChecksumPlaceholder),
                            mono = true,
                            error = if (checksumHexValid(adv.checksumHex)) null else str(R.string.mobileChecksumInvalid),
                            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                        )
                    }
                }
            }
        }

        // 自定义请求头
        GlassSection(title = str(R.string.taskHeaders), footer = str(R.string.taskHeadersDesc)) {
            custom(padded = true) {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    val removeLabel = str(R.string.delete)
                    adv.headers.forEach { h ->
                        key(h.id) {
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.Top) {
                                FluxField(
                                    value = h.key,
                                    onValueChange = { h.key = it },
                                    modifier = Modifier.weight(1f),
                                    placeholder = str(R.string.taskHeadersKeyPlaceholder),
                                    mono = true,
                                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                                )
                                FluxField(
                                    value = h.value,
                                    onValueChange = { h.value = it },
                                    modifier = Modifier.weight(1f),
                                    placeholder = str(R.string.taskHeadersValuePlaceholder),
                                    mono = true,
                                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                                    trailing = { FluxFieldAction(FluxIcons.X, removeLabel, onClick = { adv.headers.remove(h) }) },
                                )
                            }
                        }
                    }
                    FluxButton(str(R.string.taskHeadersAdd), { adv.addHeader() }, size = ButtonSize.Sm, icon = FluxIcons.Plus)
                }
            }
        }

        // 忽略证书错误
        GlassSection(footer = null) {
            row {
                FluxSwitchRow(
                    title = str(R.string.taskIgnoreTlsErrors),
                    subtitle = str(R.string.taskIgnoreTlsErrorsDesc),
                    checked = adv.ignoreTls,
                    onCheckedChange = { adv.ignoreTls = it },
                )
            }
        }
    }
}

@Composable
private fun proxyLabel(choice: ProxyChoice): String = when (choice) {
    ProxyChoice.Follow -> str(R.string.taskProxyChoiceFollow)
    ProxyChoice.Direct -> str(R.string.taskProxyChoiceDirect)
    ProxyChoice.System -> str(R.string.taskProxyChoiceSystem)
    ProxyChoice.GlobalManual -> str(R.string.taskProxyChoiceGlobalManual)
    ProxyChoice.Custom -> str(R.string.taskProxyChoiceCustom)
}

@Composable
private fun PasswordInput(value: String, onValueChange: (String) -> Unit, label: String) {
    var visible by remember { mutableStateOf(false) }
    val show = str(R.string.mobilePasswordShow)
    val hide = str(R.string.mobilePasswordHide)
    FluxField(
        value = value,
        onValueChange = onValueChange,
        label = label,
        mono = visible,
        visualTransformation = if (visible) VisualTransformation.None else PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false, keyboardType = KeyboardType.Password),
        trailing = {
            FluxFieldAction(
                icon = if (visible) FluxIcons.EyeOff else FluxIcons.Eye,
                contentDescription = if (visible) hide else show,
                onClick = { visible = !visible },
            )
        },
    )
}
