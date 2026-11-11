package com.fluxdown.app.feature.task

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.actions.TaskActions
import com.fluxdown.app.i18n.localizedTaskError
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.app.ui.TaskVisualState
import com.fluxdown.app.ui.fileCategory
import com.fluxdown.app.ui.tileIcon
import com.fluxdown.core.format.Format
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.model.TaskProtocol
import com.fluxdown.core.store.Connection
import com.fluxdown.fluxui.chrome.FluxGlassIconButton
import com.fluxdown.fluxui.chrome.FluxPageHead
import com.fluxdown.fluxui.chrome.PageLead
import com.fluxdown.fluxui.controls.ButtonSize
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxActionTile
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxSegmented
import com.fluxdown.fluxui.controls.SegOption
import com.fluxdown.fluxui.controls.FluxPresenceDot
import com.fluxdown.fluxui.controls.Tone
import com.fluxdown.fluxui.data.FileTile
import com.fluxdown.fluxui.data.FileTileSize
import com.fluxdown.fluxui.data.FlowStrip
import com.fluxdown.fluxui.data.FlowStripHeight
import com.fluxdown.fluxui.feedback.FluxEmpty
import com.fluxdown.fluxui.feedback.FluxGlyph
import com.fluxdown.fluxui.feedback.FluxSkeletonRows
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.FlowInGate
import com.fluxdown.fluxui.material.fluxFlowIn
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxHaptics
import com.fluxdown.fluxui.theme.FluxScaleGroup
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import java.util.Locale
import kotlin.math.floor

/** 页头高度（pageHead：上 6 + 钮 44 + 下 10），列表顶部内边距 = 状态栏 + 此值。 */
private val PageHeadHeight = 60.dp

private enum class DetailTab { General, Files, Speed, Seeding, Advanced }

/** 任务最后一次的非空模型：任务被删除后页面在退场动画期间继续显示它，而不是闪成“不存在”。 */
private class LastModel {
    var last: DetailModel? = null
    var seen = false
}

/**
 * D3 任务详情。compact：推入页（带返回钮）；medium / expanded：右侧详情栏（[inPane]，右上 ×）。
 * 任务被删除（或主机切换导致消失）→ [onClose]。
 */
@Composable
fun TaskDetailScreen(taskId: String, inPane: Boolean, onClose: () -> Unit) {
    val host = hostState()
    val categories = remember { CategoryIndexCache() }
    val holder = remember(taskId) { LastModel() }
    val live by remember(taskId) { derivedStateOf { buildDetailModel(host.value, taskId, categories) } }
    // 任务表分页载入期间（含重连后的重载）任务暂时不在列表里：不是“任务不存在”，显示加载态且不关闭详情。
    val connecting by remember {
        derivedStateOf { host.value.tasksLoading || (host.value.connection == Connection.Connecting && host.value.tasks.isEmpty()) }
    }
    val closeNow by rememberUpdatedState(onClose)
    val modelOf: () -> DetailModel? = remember(taskId) { { live ?: holder.last } }

    LaunchedEffect(taskId) {
        snapshotFlow { live to host.value.tasksLoading }.collect { (model, loading) ->
            if (model != null) {
                holder.last = model
                holder.seen = true
            } else if (holder.seen && !loading) {
                closeNow()
            }
        }
    }

    val listState = rememberLazyListState()
    val scrolled by remember { derivedStateOf { listState.firstVisibleItemIndex > 0 || listState.firstVisibleItemScrollOffset > 12 } }
    val hasModel by remember { derivedStateOf { live != null || holder.last != null } }

    Box(Modifier.fillMaxSize()) {
        when {
            hasModel -> DetailBody(taskId, modelOf, listState)
            connecting -> Box(
                Modifier
                    .fillMaxSize()
                    .statusBarsPadding()
                    .padding(top = PageHeadHeight + 24.dp, start = FluxTheme.space.screenMargin, end = FluxTheme.space.screenMargin),
            ) { FluxSkeletonRows(rows = 4, loadingLabel = str(R.string.pluginCommonLoading)) }
            else -> FluxEmpty(
                glyph = FluxGlyph.File,
                title = str(R.string.mobileTaskGone),
                subtitle = str(R.string.mobileTaskGoneSub),
                modifier = Modifier.fillMaxSize().statusBarsPadding().padding(top = PageHeadHeight + 80.dp),
            )
        }
        FluxPageHead(
            title = str(R.string.detail),
            modifier = Modifier.statusBarsPadding(),
            lead = if (inPane) PageLead.None else PageLead.Back,
            onLead = onClose,
            scrolled = scrolled,
            backDescription = str(R.string.back),
            closeDescription = str(R.string.close),
            actions = {
                if (inPane) FluxGlassIconButton(FluxIcons.X, str(R.string.close), onClose)
            },
        )
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun DetailBody(taskId: String, modelOf: () -> DetailModel?, listState: LazyListState) {
    val margin = FluxTheme.space.screenMargin
    val c = FluxTheme.colors
    val haptics = FluxTheme.haptics
    val isBt by remember(taskId) { derivedStateOf { modelOf()?.task?.protocol == TaskProtocol.Bt } }
    val stuck by remember { derivedStateOf { listState.firstVisibleItemIndex >= 1 } }
    val tabs = remember(isBt) {
        buildList {
            add(DetailTab.General)
            if (isBt) add(DetailTab.Files)
            add(DetailTab.Speed)
            if (isBt) add(DetailTab.Seeding)
            add(DetailTab.Advanced)
        }
    }
    var tab by rememberSaveable(taskId) { mutableStateOf(DetailTab.General) }
    if (tab !in tabs) tab = DetailTab.General
    val options = tabs.map { SegOption(it, tabLabel(it)) }
    val gate = rememberFlowInGate()
    val files = rememberBtFilesState(taskId, visible = isBt && tab == DetailTab.Files)

    // 切换分页：列表滚回到标签条（英雄头保持滚出）；新页内容各自重播流入
    LaunchedEffect(tab) {
        if (listState.firstVisibleItemIndex > 1 || (listState.firstVisibleItemIndex == 1 && listState.firstVisibleItemScrollOffset > 0)) {
            listState.scrollToItem(1)
        }
    }

    val swipePx = with(LocalDensity.current) { 72.dp.toPx() }
    val top = WindowInsets.statusBars.asPaddingValues().calculateTopPadding() + PageHeadHeight
    val tabBg by animateFloatAsState(if (stuck) 1f else 0f, label = "tabsBg")

    LazyColumn(
        modifier = Modifier
            .fillMaxSize()
            .pointerInput(tabs, tab) {
                var acc = 0f
                detectHorizontalDragGestures(
                    onDragStart = { acc = 0f },
                    onDragEnd = {
                        val i = tabs.indexOf(tab)
                        val next = when {
                            acc <= -swipePx -> tabs.getOrNull(i + 1)
                            acc >= swipePx -> tabs.getOrNull(i - 1)
                            else -> null
                        }
                        if (next != null) {
                            haptics.tick()
                            tab = next
                        }
                        acc = 0f
                    },
                    onDragCancel = { acc = 0f },
                    onHorizontalDrag = { _, dx -> acc += dx },
                )
            },
        state = listState,
        contentPadding = PaddingValues(top = top, bottom = FluxTheme.space.pageClearance),
    ) {
        item(key = "hero") {
            Box(Modifier.padding(horizontal = margin)) { DetailHero(modelOf, gate) }
        }
        stickyHeader(key = "tabs") {
            Box(
                Modifier
                    .fillMaxWidth()
                    .background(c.canvas.copy(alpha = 0.89f * tabBg))
                    .padding(horizontal = margin)
                    .padding(top = 6.dp, bottom = 4.dp),
            ) {
                FluxSegmented(options = options, selected = tab, onSelect = { tab = it })
            }
        }
        if (tab == DetailTab.Files) {
            btFileItems(files, margin)
        } else {
            item(key = "tab-$tab") {
                Column(
                    Modifier
                        .fillMaxWidth()
                        .padding(horizontal = margin)
                        .padding(top = 16.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    when (tab) {
                        DetailTab.General -> GeneralTab(modelOf)
                        DetailTab.Files -> Unit
                        DetailTab.Speed -> SpeedTab(taskId, modelOf)
                        DetailTab.Seeding -> SeedingTab(taskId, modelOf)
                        DetailTab.Advanced -> AdvancedTab(modelOf)
                    }
                }
            }
        }
    }
}

@Composable
private fun tabLabel(tab: DetailTab): String = when (tab) {
    DetailTab.General -> str(R.string.detailTabGeneral)
    DetailTab.Files -> str(R.string.btTabFiles)
    DetailTab.Speed -> str(R.string.detailTabSpeed)
    DetailTab.Seeding -> str(R.string.tabSeeding)
    DetailTab.Advanced -> str(R.string.detailTabAdvanced)
}

// ── 英雄头 ────────────────────────────────────────────────────────────────

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun DetailHero(modelOf: () -> DetailModel?, gate: FlowInGate) {
    val model = modelOf() ?: return
    val task = model.task
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val context = LocalContext.current
    val actions = LocalTaskActions.current
    val overlays = LocalFluxOverlays.current
    val haptics = FluxTheme.haptics
    val hostRef by LocalAppContainer.current.host.collectAsStateWithLifecycle()
    val remoteHost = hostRef is HostRef.Remote
    val category = model.category

    Column(Modifier.fluxFlowIn(0, gate, "hero:${task.taskId}")) {
        // 第 1 行：图块 + 文件名（可选择复制）
        Row(Modifier.padding(horizontal = 2.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            FileTile(
                icon = category.tileIcon(task),
                categoryColor = c.category(category.fileCategory()),
                size = FileTileSize.Lg,
            )
            Spacer(Modifier.width(14.dp))
            SelectionContainer(Modifier.weight(1f)) {
                FluxText(
                    task.fileName,
                    modifier = Modifier.semantics { heading() },
                    style = type.h1.copy(letterSpacing = (-0.01).em),
                    color = c.ink,
                    maxLines = 2,
                )
            }
        }
        Spacer(Modifier.height(12.dp))
        StatusLine(model)
        Spacer(Modifier.height(18.dp))
        FlowStrip(
            segments = model.flowSegments,
            state = model.flowState,
            height = FlowStripHeight.Lg,
            semanticsLabel = task.progress?.let { Format.percent(it) },
        )
        Spacer(Modifier.height(16.dp))
        ReadoutRow(model)

        val chips = heroChips(model)
        if (chips.isNotEmpty()) {
            Spacer(Modifier.height(14.dp))
            val labelStyle = remember(type) { type.monoS.copy(fontFamily = type.sm.fontFamily) }
            val valueStyle = remember(type) { type.weight(type.monoS, 600, mono = true) }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                chips.forEach { (label, value) ->
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        FluxText(label, style = labelStyle, color = c.inkFaint, maxLines = 1)
                        FluxText(value, style = valueStyle, color = c.ink, maxLines = 1)
                    }
                }
            }
        }

        Spacer(Modifier.height(18.dp))
        ActionRow(model, remoteHost, context, actions, overlays, haptics)

        when (model.visual) {
            TaskVisualState.Failed -> {
                Spacer(Modifier.height(16.dp))
                FluxBanner(
                    text = firstLine(localizedTaskError(task.errorMessage)).ifEmpty { str(R.string.subtitleError) },
                    kind = FluxBannerKind.Error,
                    title = str(R.string.subtitleError),
                    actions = {
                        FluxButton(str(R.string.mobileRetry), { actions.resume(listOf(task.taskId)) }, variant = ButtonVariant.Primary, size = ButtonSize.Xs, icon = FluxIcons.RotateCw)
                        if (task.protocol != TaskProtocol.Bt) {
                            FluxButton(str(R.string.mobileChangeUrl), { actions.changeUrl(task) }, size = ButtonSize.Xs, icon = FluxIcons.Link)
                        }
                    },
                )
            }
            TaskVisualState.Missing -> {
                Spacer(Modifier.height(16.dp))
                FluxBanner(
                    text = "${task.saveDir.trimEnd('/')}/${task.fileName}",
                    kind = FluxBannerKind.Warn,
                    title = str(R.string.statusFileMissing),
                    actions = {
                        FluxButton(str(R.string.redownloadTask), { actions.confirmRedownload(task) }, variant = ButtonVariant.Primary, size = ButtonSize.Xs, icon = FluxIcons.RotateCw)
                    },
                )
            }
            else -> Unit
        }
    }
}

@Composable
private fun StatusLine(model: DetailModel) {
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val task = model.task
    val visual = model.visual
    val word = statusWord(visual, model.queuePosition)
    val wordColor = when (visual) {
        TaskVisualState.Downloading -> c.accentHi
        TaskVisualState.Failed -> c.coralText
        TaskVisualState.Missing -> c.amberText
        TaskVisualState.Seeding -> c.mintText
        else -> c.ink
    }
    val protocol = task.protocol.tag()
    val size = task.totalBytes.takeIf { it > 0 }?.let { Format.bytes(it).toString() }
    val upload = "↑ " + (Format.speed(model.speedUp)?.toString() ?: "—")
    val tail = when (visual) {
        TaskVisualState.Failed -> firstLine(localizedTaskError(task.errorMessage))
        TaskVisualState.Completed, TaskVisualState.Missing -> listOfNotNull(size, protocol, task.siteLabel()).joinToString(" · ")
        TaskVisualState.Seeding -> "$protocol · $upload"
        else -> "$protocol · ${task.siteLabel()}"
    }
    val tailColor = if (visual == TaskVisualState.Failed) c.coralText else c.inkFaint
    val tone = when (visual) {
        TaskVisualState.Downloading -> Tone.Accent
        TaskVisualState.Failed -> Tone.Coral
        TaskVisualState.Missing -> Tone.Amber
        TaskVisualState.Seeding -> Tone.Mint
        else -> Tone.Neutral
    }
    val pulse: State<Float>? = if (visual == TaskVisualState.Downloading && !FluxTheme.motion.reduce) {
        rememberInfiniteTransition(label = "dot").animateFloat(
            initialValue = 1f,
            targetValue = 0.35f,
            animationSpec = infiniteRepeatable(tween(1000), RepeatMode.Reverse),
            label = "dotAlpha",
        )
    } else {
        null
    }
    val text = remember(word, tail, wordColor, tailColor) {
        buildAnnotatedString {
            withStyle(SpanStyle(color = wordColor, fontWeight = FontWeight.Medium)) { append(word) }
            if (tail.isNotEmpty()) withStyle(SpanStyle(color = tailColor)) { append(" · $tail") }
        }
    }
    Row(Modifier.padding(horizontal = 2.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        FluxPresenceDot(
            if (pulse != null) Modifier.graphicsLayer { alpha = pulse.value } else Modifier,
            tone = tone,
            glow = tone == Tone.Accent || tone == Tone.Mint,
        )
        BasicText(text, modifier = Modifier.weight(1f), style = type.sm, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

@Composable
private fun ReadoutRow(model: DetailModel) {
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val task = model.task
    val done = model.visual == TaskVisualState.Completed || model.visual == TaskVisualState.Seeding || model.visual == TaskVisualState.Missing
    val progress = task.progress
    val pct = when {
        done -> "100.0"
        progress == null -> "—"
        else -> String.format(Locale.ROOT, "%.1f", floor(progress * 1000.0) / 10.0)
    }
    val pctStyle = remember(type) { type.sized(type.weight(type.mono, 600, mono = true), 34f, FluxScaleGroup.Kl).copy(letterSpacing = (-0.03).em) }
    val pctUnit = remember(type) { type.sized(type.weight(type.mono, 500, mono = true), 12f, FluxScaleGroup.Ks) }
    val speedStyle = remember(type) { type.sized(type.weight(type.mono, 600, mono = true), 15f, FluxScaleGroup.Km) }
    Row(Modifier.padding(horizontal = 2.dp), horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.Bottom) {
        Row(verticalAlignment = Alignment.Bottom) {
            FluxText(pct, style = pctStyle, color = c.ink, maxLines = 1)
            if (pct != "—") FluxText("%", modifier = Modifier.padding(start = 2.dp, bottom = 4.dp), style = pctUnit, color = c.inkMuted, maxLines = 1)
        }
        Spacer(Modifier.weight(1f))
        Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            val totalText = if (task.totalBytes > 0) Format.bytes(task.totalBytes).toString() else str(R.string.unknownSize)
            val sizeLine = remember(task.downloadedBytes, totalText, c) {
                buildAnnotatedString {
                    withStyle(SpanStyle(color = c.ink, fontWeight = FontWeight.SemiBold)) { append(Format.bytes(task.downloadedBytes).toString()) }
                    withStyle(SpanStyle(color = c.inkMuted)) { append(" / $totalText") }
                }
            }
            BasicText(sizeLine, style = type.mono, maxLines = 1, overflow = TextOverflow.Ellipsis)
            val speed = when {
                model.visual == TaskVisualState.Downloading && model.speedDown > 0 -> "↓ ${Format.speed(model.speedDown)}" to c.accentHi
                model.visual == TaskVisualState.Seeding && model.speedUp > 0 -> "↑ ${Format.speed(model.speedUp)}" to c.mintText
                else -> null
            }
            // 无实时速度（已完成 / 暂停 / 排队）时不占一行占位符「—」
            speed?.let { (text, color) -> FluxText(text, style = speedStyle, color = color, maxLines = 1) }
        }
    }
}

/** 芯片行：剩余 / 活跃连接 / BT 节点 / 上传；全不满足时为空（整行隐藏）。 */
@Composable
private fun heroChips(model: DetailModel): List<Pair<String, String>> {
    val task = model.task
    val out = ArrayList<Pair<String, String>>(4)
    if (model.isTransferring) {
        val eta = Format.etaSeconds(task.downloadedBytes, task.totalBytes, model.speedDown)
        out += str(R.string.infoRemaining) to etaText(eta)
        model.runtime?.activeTransfers?.let { out += str(R.string.detailActiveTransfers) to it.toString() }
    }
    if (task.protocol == TaskProtocol.Bt) {
        if (model.isTransferring) model.runtime?.connectedPeers?.let { out += str(R.string.detailConnectedPeers) to it.toString() }
        if (model.speedUp > 0 && model.visual != TaskVisualState.Seeding) {
            out += str(R.string.mobileUploadChip) to (Format.speed(model.speedUp)?.toString() ?: "—")
        }
    }
    return out
}

@Composable
private fun ActionRow(
    model: DetailModel,
    remoteHost: Boolean,
    context: android.content.Context,
    actions: TaskActions,
    overlays: FluxOverlayState,
    haptics: FluxHaptics,
) {
    val task = model.task
    val primaryLabel: String
    val primaryIcon: ImageVector
    when (model.visual) {
        TaskVisualState.Completed, TaskVisualState.Seeding, TaskVisualState.Missing -> {
            primaryLabel = str(R.string.openFile)
            primaryIcon = FluxIcons.ExternalLink
        }
        TaskVisualState.Paused -> {
            primaryLabel = str(R.string.resume)
            primaryIcon = FluxIcons.Play
        }
        TaskVisualState.Failed -> {
            primaryLabel = str(R.string.mobileRetry)
            primaryIcon = FluxIcons.RotateCw
        }
        else -> {
            primaryLabel = str(R.string.pause)
            primaryIcon = FluxIcons.Pause
        }
    }
    val missingToast = str(R.string.mobileFileMissingRedownload)
    val shareLabel = str(R.string.mobileShareLink)
    val moreLabel = str(R.string.moreActions)
    val folderLabel = str(R.string.detailActionFolder)
    val shareShort = str(R.string.detailActionShare)
    val moreShort = str(R.string.detailActionMore)
    var moreBounds by remember { mutableStateOf(Rect.Zero) }
    val onPrimary: () -> Unit = {
        if (model.visual == TaskVisualState.Missing) {
            haptics.reject()
            overlays.toast(missingToast, FluxToastKind.Warn, FluxIcons.FileWarning)
        } else {
            actions.primary(task)
        }
    }
    val onFolder: () -> Unit = { if (!context.openFolder(task.saveDir)) actions.shareLink(task) }
    val onMore: () -> Unit = { overlays.showMenu(moreBounds, actions.menuItems(task, model.boosted)) }

    // 等宽动作块：图标在上、短标签在下（对齐 iOS 地图 / 通讯录卡片）。每块只放一个短词，
    // 窄屏与大字号下不会再出现“图标 + 折成两行的文字”挤在胶囊里；主动作为强调色实心块。
    Row(
        Modifier.fillMaxWidth().height(IntrinsicSize.Min),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        val tile = Modifier.weight(1f).fillMaxHeight()
        // 远端主机的已完成文件在主机磁盘上，本机无法打开：不给「打开文件」主动作（iOS 同规则）
        val remoteFile = remoteHost && (model.visual == TaskVisualState.Completed || model.visual == TaskVisualState.Seeding)
        if (!remoteFile) FluxActionTile(primaryLabel, primaryIcon, onPrimary, tile, prominent = true)
        if (!remoteHost) FluxActionTile(folderLabel, FluxIcons.FolderOpen, onFolder, tile)
        FluxActionTile(shareShort, FluxIcons.Share2, { actions.shareLink(task) }, tile, contentDescription = shareLabel)
        FluxActionTile(
            moreShort,
            FluxIcons.Ellipsis,
            onMore,
            tile.onGloballyPositioned { moreBounds = it.boundsInRoot() },
            contentDescription = moreLabel,
        )
    }
}
