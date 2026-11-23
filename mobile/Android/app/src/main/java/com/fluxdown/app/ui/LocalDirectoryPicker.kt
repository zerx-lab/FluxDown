package com.fluxdown.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import com.fluxdown.app.R
import com.fluxdown.app.i18n.fill
import com.fluxdown.app.i18n.str
import com.fluxdown.fluxui.chrome.FluxPill
import com.fluxdown.fluxui.controls.ButtonSize
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxDivider
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxIconButton
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.IconButtonSize
import com.fluxdown.fluxui.feedback.FluxEmpty
import com.fluxdown.fluxui.feedback.FluxGlyph
import com.fluxdown.fluxui.feedback.FluxSpinner
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxPortal
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import java.io.File
import java.io.IOException
import java.nio.file.InvalidPathException

/** 列目录 / 建目录的超时：卡住的 FUSE / 拔出的 SD 卡不能让界面一直转圈。 */
private const val LIST_TIMEOUT_MS = 8_000L
private const val WRITE_TIMEOUT_MS = 10_000L

private sealed interface IoOutcome<out T> {
    class Done<T>(val value: T) : IoOutcome<T>
    class Failed(val error: Exception) : IoOutcome<Nothing>
    data object TimedOut : IoOutcome<Nothing>
}

/**
 * 阻塞文件操作放到 IO 线程并限时等待：超时后界面立即恢复（阻塞线程自行结束，结果丢弃）。
 * 任何异常都在工作协程内收拢为 [IoOutcome.Failed]：不会取消外层作用域，也不会让应用崩溃。
 */
private suspend fun <T> runFileIo(scope: CoroutineScope, timeoutMs: Long, block: () -> T): IoOutcome<T> {
    val work = scope.async(Dispatchers.IO) {
        try {
            IoOutcome.Done(block())
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            IoOutcome.Failed(e)
        }
    }
    return withTimeoutOrNull(timeoutMs) { work.await() } ?: IoOutcome.TimedOut
}

private enum class DirStatus { Loading, Ready, NeedsAccess, Protected, Failed, TimedOut }

private fun normalizedOrNull(raw: String): String? {
    val file = File(raw.trim())
    if (raw.isBlank() || !file.isAbsolute) return null
    return try {
        file.toPath().normalize().toString()
    } catch (_: InvalidPathException) {
        null
    }
}

/**
 * 本机目录选择器（不经 SAF：它在 Android 11+ 禁选 Download 根，即使已有所有文件访问权限）。
 *
 * 顶层为「存储位置」（常用位置 + 已挂载存储卷），进入后以面包屑导航；返回键回上一级。
 * 需要授权的位置就地显示授权横幅，返回应用后自动重读；`Android/data|obb` 等系统保护目录直接说明不可用，
 * 不去枚举。所有文件操作在 IO 线程限时执行，失败 / 超时显示可重试的横幅，界面不会卡住。
 * 「选择此文件夹」前实际创建 / 写入 / 删除探针，确认可写才回调 [onPick]。
 */
@Composable
internal fun rememberLocalDirectoryPicker(
    externalDirectory: ExternalLocalDirectory? = null,
    onPick: (String) -> Unit,
): (String) -> Unit {
    val context = LocalContext.current
    val overlays = LocalFluxOverlays.current
    val haptics = FluxTheme.haptics
    val colors = FluxTheme.colors
    val scope = rememberCoroutineScope()
    val picked by rememberUpdatedState(onPick)
    val defaultDir = remember(context) { defaultLocalSaveDir(context).path }
    val downloadDir = remember { publicDownloadSaveDir().path }

    var visible by rememberSaveable { mutableStateOf(false) }
    /** 当前目录；空串 = 「存储位置」列表。 */
    var path by rememberSaveable { mutableStateOf("") }
    var layout by remember { mutableStateOf(storageLayout(context)) }
    var status by remember { mutableStateOf(DirStatus.Ready) }
    var entries by remember { mutableStateOf<List<File>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    // 只采纳最近一次导航 / 操作的结果（快速连点、关闭后迟到的结果一律丢弃）
    val ticket = remember { IntArray(1) }

    val notWritable = str(R.string.mobileSaveDirNotWritable)
    val invalidPath = str(R.string.mobileSaveDirInvalid)
    val missingMoved = str(R.string.mobileLocalFolderMissing)
    val timeoutText = str(R.string.mobileLocalFolderTimeout)
    val createFailed = str(R.string.mobileLocalFolderCreateFailed)
    val protectedText = str(R.string.mobileLocalFolderProtected)

    fun isProtected(dir: String) = isProtectedAppDataPath(dir, layout.primary, layout.volumeFiles, layout.appRoots)

    fun navigate(target: String?, announceMissing: Boolean = false) {
        val gen = ++ticket[0]
        entries = emptyList()
        if (target == null) {
            path = ""
            status = DirStatus.Ready
            return
        }
        val normalized = normalizedOrNull(target)
        if (normalized == null) {
            haptics.reject()
            overlays.toast(invalidPath, FluxToastKind.Warn)
            return
        }
        path = normalized
        status = when {
            isProtected(normalized) -> DirStatus.Protected
            canRequestStorageAccess(context, layout, normalized) -> DirStatus.NeedsAccess
            else -> DirStatus.Loading
        }
        if (status != DirStatus.Loading) return
        scope.launch {
            val outcome = runFileIo(scope, LIST_TIMEOUT_MS) {
                val existing = nearestExistingDirectory(normalized) ?: throw IOException("No existing ancestor: $normalized")
                existing.path to listLocalDirectories(existing.path)
            }
            if (gen != ticket[0]) return@launch
            when (outcome) {
                is IoOutcome.Done -> {
                    val (resolved, list) = outcome.value
                    if (resolved != normalized) {
                        path = resolved
                        if (announceMissing) overlays.toast(missingMoved, FluxToastKind.Info)
                    }
                    entries = list
                    status = DirStatus.Ready
                }
                is IoOutcome.Failed -> status = DirStatus.Failed
                IoOutcome.TimedOut -> status = DirStatus.TimedOut
            }
        }
    }

    val requestAccess = rememberStorageAccessRequest { granted ->
        if (granted && visible && status == DirStatus.NeedsAccess) navigate(path)
    }
    // 用户也可能自行去系统设置授权：回到前台时重新判定
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) {
        if (visible && status == DirStatus.NeedsAccess && !canRequestStorageAccess(context, layout, path)) navigate(path)
    }

    fun close() {
        ticket[0]++
        busy = false
        visible = false
    }

    fun checkedWritePath(path: String): String =
        if (externalDirectory == null) path
        else externalDirectory.forConfirmation(path) ?: throw IOException("Unsafe directory: $path")

    fun choose() {
        if (busy || path.isEmpty() || status != DirStatus.Ready) return
        val gen = ++ticket[0]
        val dir = path
        busy = true
        scope.launch {
            val outcome = runFileIo(scope, WRITE_TIMEOUT_MS) { prepareLocalDirectory(checkedWritePath(dir)) }
            if (gen != ticket[0]) return@launch
            busy = false
            when (outcome) {
                is IoOutcome.Done -> {
                    haptics.confirm()
                    picked(outcome.value)
                    close()
                }
                is IoOutcome.Failed -> {
                    haptics.reject()
                    overlays.toast(notWritable.fill("dir" to dir), FluxToastKind.Error)
                }
                IoOutcome.TimedOut -> {
                    haptics.reject()
                    overlays.toast(timeoutText.fill("dir" to dir), FluxToastKind.Error)
                }
            }
        }
    }

    fun createFolder(name: String) {
        val parent = path
        val gen = ++ticket[0]
        busy = true
        scope.launch {
            val outcome = runFileIo(scope, WRITE_TIMEOUT_MS) { createChildDirectory(checkedWritePath(parent), name) }
            if (gen != ticket[0]) return@launch
            busy = false
            when (outcome) {
                is IoOutcome.Done -> {
                    haptics.tick()
                    navigate(outcome.value)
                }
                is IoOutcome.Failed -> {
                    haptics.reject()
                    overlays.toast(createFailed.fill("name" to name.trim()), FluxToastKind.Error)
                }
                IoOutcome.TimedOut -> {
                    haptics.reject()
                    overlays.toast(timeoutText.fill("dir" to parent), FluxToastKind.Error)
                }
            }
        }
    }

    val rootLabel = str(R.string.mobileLocalFolderLocations)
    val crumbs = remember(path, layout, rootLabel) {
        if (path.isEmpty()) emptyList() else pathCrumbs(path, rootLabel, layout.volumes.map { it.label to File(it.path) })
    }
    fun goUp() = navigate(crumbs.getOrNull(crumbs.size - 2)?.path)

    val newFolderTitle = str(R.string.mobileLocalFolderNew)
    val namePlaceholder = str(R.string.mobileLocalFolderNamePlaceholder)
    val nameInvalid = str(R.string.mobileLocalFolderNameInvalid)
    val nameTooLong = str(R.string.mobileLocalFolderNameTooLong)
    val goToTitle = str(R.string.mobileLocalFolderGoTo)
    val goLabel = str(R.string.mobileLocalFolderGo)
    val createLabel = str(R.string.mobileLocalFolderCreate)

    fun promptNewFolder() {
        overlays.showDialog(
            FluxDialogSpec(
                title = newFolderTitle,
                icon = FluxIcons.FolderPlus,
                buttons = emptyList(),
                body = {
                    PathPrompt("", namePlaceholder, mono = false, actionLabel = createLabel, validate = { name ->
                        when (validateFolderName(name)) {
                            null -> null
                            FolderNameError.TooLong -> nameTooLong
                            FolderNameError.Blank, FolderNameError.Invalid -> nameInvalid
                        }
                    }) { name ->
                        overlays.dismissDialog()
                        createFolder(name)
                    }
                },
            ),
        )
    }

    fun promptGoTo() {
        overlays.showDialog(
            FluxDialogSpec(
                title = goToTitle,
                icon = FluxIcons.SquarePen,
                buttons = emptyList(),
                body = {
                    PathPrompt(path.ifEmpty { downloadDir }, "/storage/emulated/0/Download", mono = true, actionLabel = goLabel, validate = { raw ->
                        if (normalizedOrNull(raw) == null) invalidPath else null
                    }) { raw ->
                        overlays.dismissDialog()
                        navigate(raw, announceMissing = true)
                    }
                },
            ),
        )
    }

    val title = str(R.string.selectSaveDir)
    val windowHeightDp = with(LocalDensity.current) { LocalWindowInfo.current.containerSize.height.toDp().value.toInt() }
    val listHeight = (windowHeightDp - 340).coerceIn(240, 640).dp
    // 仅在打开时注册 portal，确保它位于发起选择的分类 / 新建下载弹层之上。
    if (visible) FluxPortal {
        FluxSheet(
            visible = true,
            onDismissRequest = ::close,
            detent = FluxSheetDetent.Full,
            title = title,
            header = {
                FluxSheetHeader(
                    title = title,
                    subtitle = path.ifEmpty { null },
                    onClose = ::close,
                    actions = {
                        FluxIconButton(
                            FluxIcons.SquarePen, goToTitle, ::promptGoTo,
                            size = IconButtonSize.Sm, enabled = !busy,
                        )
                        FluxIconButton(
                            FluxIcons.FolderPlus, newFolderTitle, ::promptNewFolder,
                            size = IconButtonSize.Sm, enabled = !busy && path.isNotEmpty() && status == DirStatus.Ready,
                        )
                    },
                )
            },
            footer = {
                FluxSheetFooter {
                    FluxButton(str(R.string.cancel), ::close, Modifier.weight(1f), variant = ButtonVariant.Secondary)
                    FluxButton(
                        str(R.string.mobileRssUseFolder),
                        onClick = ::choose,
                        modifier = Modifier.weight(1f),
                        variant = ButtonVariant.Primary,
                        enabled = path.isNotEmpty() && status == DirStatus.Ready,
                        loading = busy,
                    )
                }
            },
        ) {
            BackHandler(enabled = path.isNotEmpty() && !overlays.hasDialog) { goUp() }
            if (path.isEmpty()) {
                StorageLocations(
                    layout = layout,
                    downloadDir = downloadDir,
                    defaultDir = defaultDir,
                    onOpen = { navigate(it) },
                )
                return@FluxSheet
            }
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Breadcrumbs(crumbs, enabled = !busy) { navigate(it) }
                when (status) {
                    DirStatus.Loading -> Box(Modifier.fillMaxWidth().padding(vertical = 48.dp), Alignment.Center) {
                        FluxSpinner(label = str(R.string.mobileLoading))
                    }
                    DirStatus.NeedsAccess -> {
                        val legacy = android.os.Build.VERSION.SDK_INT < android.os.Build.VERSION_CODES.R
                        FluxBanner(
                            text = str(if (legacy) R.string.mobileStoragePermDesc else R.string.mobileAllFilesDescNative),
                            title = str(if (legacy) R.string.mobileStoragePermTitle else R.string.mobileAllFilesTitle),
                            kind = FluxBannerKind.Warn,
                            icon = FluxIcons.Lock,
                            actions = {
                                FluxButton(str(R.string.mobileGoGrant), requestAccess, variant = ButtonVariant.Primary, size = ButtonSize.Xs)
                                FluxButton(str(R.string.mobileFolderUp), ::goUp, size = ButtonSize.Xs)
                            },
                        )
                    }
                    DirStatus.Protected -> FluxBanner(
                        text = protectedText,
                        kind = FluxBannerKind.Warn,
                        icon = FluxIcons.Lock,
                        actions = { FluxButton(str(R.string.mobileFolderUp), ::goUp, size = ButtonSize.Xs) },
                    )
                    DirStatus.Failed, DirStatus.TimedOut -> FluxBanner(
                        text = if (status == DirStatus.TimedOut) timeoutText.fill("dir" to path) else str(R.string.mobileLocalFolderReadFailed),
                        kind = FluxBannerKind.Error,
                        actions = {
                            FluxButton(str(R.string.mobileRetry), { navigate(path) }, variant = ButtonVariant.Primary, size = ButtonSize.Xs)
                            FluxButton(str(R.string.mobileFolderUp), ::goUp, size = ButtonSize.Xs)
                        },
                    )
                    DirStatus.Ready -> if (entries.isEmpty()) {
                        FluxEmpty(
                            FluxGlyph.Inbox,
                            str(R.string.mobileFolderNoSubfolders),
                            Modifier.fillMaxWidth().padding(vertical = 16.dp),
                            subtitle = str(R.string.mobileLocalFolderEmptyHint),
                        )
                    } else {
                        LazyColumn(
                            Modifier
                                .fillMaxWidth()
                                .heightIn(max = listHeight)
                                .clip(FluxTheme.shapes.card)
                                .background(colors.glass1),
                        ) {
                            itemsIndexed(entries, key = { _, entry -> entry.path }) { index, entry ->
                                if (index > 0) FluxDivider(startInset = 62.dp)
                                val locked = isProtected(entry.path)
                                FluxListRow(
                                    title = entry.name,
                                    icon = if (locked) FluxIcons.Lock else FluxIcons.Folder,
                                    chevron = !locked,
                                    enabled = !busy,
                                    onClick = {
                                        if (locked) overlays.toast(protectedText, FluxToastKind.Warn) else navigate(entry.path)
                                    },
                                )
                            }
                        }
                    }
                }
            }
        }
    }
    return { startPath ->
        layout = storageLayout(context)
        busy = false
        visible = true
        navigate(startPath.ifBlank { defaultDir })
    }
}

@Composable
private fun StorageLocations(layout: StorageLayout, downloadDir: String, defaultDir: String, onOpen: (String) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
        GlassSection(Modifier.fillMaxWidth(), title = str(R.string.mobileLocalFolderQuick)) {
            row(hasIcon = true) {
                FluxListRow(
                    title = str(R.string.mobileLocalFolderDownload),
                    subtitle = downloadDir,
                    icon = FluxIcons.Download,
                    chevron = true,
                    onClick = { onOpen(downloadDir) },
                )
            }
            row(hasIcon = true) {
                FluxListRow(
                    title = str(R.string.mobileLocalFolderAppDir),
                    subtitle = str(R.string.mobileLocalFolderAppDirDesc),
                    icon = FluxIcons.Folder,
                    chevron = true,
                    onClick = { onOpen(defaultDir) },
                )
            }
        }
        GlassSection(Modifier.fillMaxWidth(), title = str(R.string.mobileLocalFolderDevices)) {
            for (volume in layout.volumes) {
                row(hasIcon = true) {
                    FluxListRow(
                        title = volume.label,
                        subtitle = volume.path,
                        icon = if (volume.removable) FluxIcons.HardDrive else FluxIcons.Smartphone,
                        chevron = true,
                        onClick = { onOpen(volume.path) },
                    )
                }
            }
        }
    }
}

/** 面包屑：横向滚动，自动停在末端；末项为当前目录（选中态）。 */
@Composable
private fun Breadcrumbs(crumbs: List<PathCrumb>, enabled: Boolean, onOpen: (String?) -> Unit) {
    val scroll = rememberScrollState()
    LaunchedEffect(crumbs.size, scroll.maxValue) { scroll.scrollTo(scroll.maxValue) }
    Row(
        Modifier.fillMaxWidth().horizontalScroll(scroll),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        crumbs.forEachIndexed { index, crumb ->
            if (index > 0) FluxIcon(FluxIcons.ChevronRight, null, size = 14.dp, tint = FluxTheme.colors.inkFaint)
            val last = index == crumbs.lastIndex
            FluxPill(
                text = crumb.label,
                selected = last,
                onClick = { if (enabled && !last) onOpen(crumb.path) },
                small = true,
                toggleable = false,
                icon = if (index == 0) FluxIcons.House else null,
            )
        }
    }
}

/** 对话框内单行输入 + 主按钮；[validate] 返回错误文案则就地显示，不关闭对话框。 */
@Composable
private fun ColumnScope.PathPrompt(
    initial: String,
    placeholder: String,
    mono: Boolean,
    actionLabel: String,
    validate: (String) -> String?,
    onSubmit: (String) -> Unit,
) {
    val haptics = FluxTheme.haptics
    var value by remember { mutableStateOf(initial) }
    var error by remember { mutableStateOf<String?>(null) }
    fun submit() {
        val problem = validate(value)
        if (problem != null) {
            error = problem
            haptics.reject()
        } else {
            onSubmit(value.trim())
        }
    }
    FluxField(
        value = value,
        onValueChange = {
            value = it
            error = null
        },
        placeholder = placeholder,
        mono = mono,
        error = error,
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false, imeAction = ImeAction.Done),
        keyboardActions = KeyboardActions(onDone = { submit() }),
    )
    FluxButton(actionLabel, ::submit, variant = ButtonVariant.Primary, fullWidth = true)
}
