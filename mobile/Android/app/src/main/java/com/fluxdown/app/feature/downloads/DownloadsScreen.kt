package com.fluxdown.app.feature.downloads

import androidx.compose.foundation.gestures.animateScrollBy
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.unit.coerceAtLeast
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.text.BasicText
import android.view.accessibility.AccessibilityManager
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.feature.devices.localizedName
import com.fluxdown.app.i18n.fill
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.SheetRoute
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.feature.newtask.ImportProgressBanner
import com.fluxdown.app.ui.fileCategory
import com.fluxdown.app.ui.label
import com.fluxdown.core.format.Format
import com.fluxdown.core.model.HostRef
import com.fluxdown.fluxui.chrome.FluxGlassIconButton
import com.fluxdown.fluxui.chrome.FluxHeader
import com.fluxdown.fluxui.chrome.FluxHostPill
import com.fluxdown.fluxui.chrome.FluxPill
import com.fluxdown.fluxui.chrome.FluxTopStrip
import com.fluxdown.fluxui.chrome.ScopeTab
import com.fluxdown.fluxui.chrome.ScopeTabs
import com.fluxdown.fluxui.controls.ButtonSize
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.data.GroupHeader
import com.fluxdown.fluxui.data.InstrumentStat
import com.fluxdown.fluxui.data.SpeedInstrument
import com.fluxdown.fluxui.data.SpeedReading
import com.fluxdown.fluxui.data.Waveform
import com.fluxdown.fluxui.feedback.FluxEmpty
import com.fluxdown.fluxui.feedback.FluxGlyph
import com.fluxdown.fluxui.feedback.FluxSkeletonRows
import com.fluxdown.fluxui.icons.FluxIcons
import androidx.compose.runtime.CompositionLocalProvider
import com.fluxdown.fluxui.material.FluxBackdrop
import com.fluxdown.fluxui.material.LocalFluxBackdrop
import com.fluxdown.fluxui.material.fluxBackdropSource
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.LocalSwipeRevealCoordinator
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlin.math.max
import kotlin.math.min
import kotlinx.coroutines.launch

private const val HEADER_KEY = "header"
private const val HERO_KEY = "hero"
private const val SLOT_KEY = "slot"
private const val TAIL_KEY = "tail"

/** 状态文件夹显示名（ScopeTabs / Rail / expanded 标题共用）。 */
@Composable
internal fun StatusFolder.label(): String = stringResource(
    when (this) {
        StatusFolder.All -> R.string.tabAll
        StatusFolder.Active -> R.string.tabDownloading
        StatusFolder.Completed -> R.string.tabCompleted
        StatusFolder.Seeding -> R.string.statusSeeding
        StatusFolder.Failed -> R.string.tabError
        StatusFolder.Paused -> R.string.tabPaused
    },
)

@Composable
internal fun GroupLabel.resolve(): String = when {
    res != 0 -> stringResource(res)
    category != null -> category.label()
    queue != null -> queue.label()
    else -> text
}

@Composable
internal fun HostRef.title(): String = localizedName()

@Composable
internal fun HostRef.subtitle(): String = when (this) {
    is HostRef.Remote -> endpoint
    is HostRef.Local -> ""
}

/** TalkBack 触摸探索开启：此时才为每行构建完整的自定义动作（长按菜单全集）。 */
@Composable
internal fun rememberTouchExploration(): Boolean {
    val ctx = LocalContext.current
    val am = remember(ctx) { ctx.getSystemService(AccessibilityManager::class.java) }
    var on by remember { mutableStateOf(am?.isTouchExplorationEnabled == true) }
    DisposableEffect(am) {
        val l = AccessibilityManager.TouchExplorationStateChangeListener { on = it }
        am?.addTouchExplorationStateChangeListener(l)
        onDispose { am?.removeTouchExplorationStateChangeListener(l) }
    }
    return on
}

/**
 * D1 下载列表（Tab 根页）。expanded = Rail 已承载状态 / 分类 / 队列，列表栏不显示英雄卡与 ScopeTabs。
 */
@Composable
fun DownloadsScreen(expanded: Boolean) {
    val listState = rememberLazyListState()
    val topInset = WindowInsets.statusBars.asPaddingValues().calculateTopPadding()
    val topInsetPx = with(LocalDensity.current) { topInset.toPx() }
    val tracker = remember(listState, topInsetPx) { ListTracker(listState, topInsetPx) }
    var filtersHeight by remember { mutableIntStateOf(0) }
    // 页面级背景源：只录制列表本身，供浮在其上的读数条 / 吸顶筛选 / 选择坞采样模糊。
    // 舞台级背景源在 AppShell 里对页面内容提供 null，避免玻璃面落在自身采样源中形成 RenderNode 环。
    val listBackdrop = remember { FluxBackdrop() }

    Box(Modifier.fillMaxSize()) {
        Box(Modifier.fillMaxSize().fluxBackdropSource(listBackdrop)) {
            DownloadsListBody(expanded, listState, tracker, topInset) { filtersHeight }
        }
        CompositionLocalProvider(LocalFluxBackdrop provides listBackdrop) {
            if (!expanded) {
                FiltersOverlay(tracker, onHeight = { filtersHeight = it })
                TopStripHost(tracker, topInset)
            } else {
                Box(
                    Modifier
                        .align(Alignment.BottomCenter)
                        .fillMaxWidth()
                        .padding(
                            start = 16.dp,
                            end = 16.dp,
                            bottom = WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding() + 8.dp,
                        ),
                    contentAlignment = Alignment.BottomCenter,
                ) { ExpandedSelectionDock() }
            }
        }
    }
}

// ── 滚动度量 ─────────────────────────────────────────────────────────────────

/**
 * 读取 LazyColumn 布局信息的度量器：全部在 layer / draw 阶段或快照流中调用（不触发重组）。
 * 英雄卡折叠进度按 §3.3：`p = clamp((scrollTop − .35h)/(.5h), 0, 1)`。
 */
@Stable
internal class ListTracker(private val state: LazyListState, private val topInsetPx: Float) {
    private var restTop = Float.NaN
    private var headerRest = Float.NaN
    var heroHeight = 1f
        private set

    private fun atRest() = state.firstVisibleItemIndex == 0 && state.firstVisibleItemScrollOffset == 0

    private fun find(key: Any) = state.layoutInfo.visibleItemsInfo.firstOrNull { it.key == key }

    /** 滚过英雄卡自然位置的像素数（静止 = 0）。 */
    fun heroScroll(): Float {
        val hero = find(HERO_KEY)
        if (hero == null) return if (state.firstVisibleItemIndex >= 1) heroHeight * 3f else 0f
        heroHeight = hero.size.toFloat().coerceAtLeast(1f)
        if (atRest()) restTop = hero.offset.toFloat()
        return if (restTop.isNaN()) 0f else (restTop - hero.offset).coerceAtLeast(0f)
    }

    fun collapse(): Float {
        val s = heroScroll()
        val h = heroHeight
        return ((s - 0.35f * h) / (0.5f * h)).coerceIn(0f, 1f)
    }

    /** 粘性区占位项当前的屏幕 y；已滚过 → 负无穷，尚未出现 → 正无穷。 */
    fun slotY(): Float {
        if (atRest()) find(HEADER_KEY)?.let { headerRest = it.offset.toFloat() }
        val slot = find(SLOT_KEY) ?: return if (state.firstVisibleItemIndex >= 2) -1e6f else 1e6f
        val origin = if (headerRest.isNaN()) 0f else headerRest
        return slot.offset - origin + topInsetPx
    }

    /**
     * 英雄区吸附（iOS 大标题式）：滚动停在页头与英雄卡之间时，过半 → 滚到完全折叠（占位项贴顶、读数条出现），
     * 否则回到顶部。返回需滚动的像素；静止于顶部或已越过英雄卡 → null。
     */
    fun snapDelta(): Float? {
        val hero = find(HERO_KEY) ?: return null
        val s = heroScroll()
        if (restTop.isNaN()) return null
        val total = restTop + hero.size
        if (s <= 1f || s >= total - 1f) return null
        return if (s >= total / 2f) total - s else -s
    }

    /**
     * 短列表补白（px）：占位项之后的内容（筛选区 + 行 + 补白）至少铺满视口，任务再少也能把英雄卡滚到完全折叠。
     * 补白项不可见时以最后一个可见项的底边估计（只会偏大，且补白在视口外，校正时不跳动）；
     * 占位项已滚过（长列表）→ null，沿用上次值。
     */
    fun tailNeed(): Int? {
        val info = state.layoutInfo
        val slot = find(SLOT_KEY) ?: return null
        val end = find(TAIL_KEY)?.offset
            ?: info.visibleItemsInfo.lastOrNull()?.let { it.offset + it.size }
            ?: return null
        val room = info.viewportSize.height - info.beforeContentPadding - info.afterContentPadding
        return (room - (end - slot.offset)).coerceAtLeast(0)
    }
}

// ── 列表 ─────────────────────────────────────────────────────────────────────

@Composable
private fun DownloadsListBody(
    expanded: Boolean,
    listState: LazyListState,
    tracker: ListTracker,
    topInset: androidx.compose.ui.unit.Dp,
    filtersHeightPx: () -> Int,
) {
    val view = LocalDownloadsView.current
    val imports = LocalAppContainer.current.imports
    val nav = LocalNavigator.current
    val swipe = LocalSwipeRevealCoordinator.current
    val density = LocalDensity.current
    val margin = FluxTheme.space.screenMargin
    val bottomInset = WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()
    val clearance = bottomInset + if (expanded && !nav.selecting) {
        FluxTheme.space.pageClearance
    } else {
        FluxTheme.space.dockClearance
    }
    val list = view.list
    val prefs = view.prefs
    val style = remember(prefs?.density, prefs?.fields) { prefs?.let { RowStyle(it.density, it.fields) } }
    val gate = rememberFlowInGate()
    val a11y = rememberTouchExploration()
    val motion = FluxTheme.motion
    var tailPx by remember { mutableIntStateOf(0) }

    if (!expanded) {
        LaunchedEffect(tracker) {
            snapshotFlow { tracker.tailNeed() }.collect { need -> if (need != null) tailPx = need }
        }
        // 松手 / 惯性结束后吸附：英雄卡不停在半折叠状态。用户再次拖动会打断吸附动画（只取消该子协程）。
        LaunchedEffect(tracker, motion) {
            snapshotFlow { listState.isScrollInProgress }.collect { scrolling ->
                val delta = if (scrolling) null else tracker.snapDelta()
                if (delta != null) launch { listState.animateScrollBy(delta, motion.of(motion.page)) }
            }
        }
    }

    LazyColumn(
        state = listState,
        modifier = Modifier
            .fillMaxSize()
            .pointerInput(view) {
                awaitPointerEventScope {
                    while (true) {
                        awaitPointerEvent(PointerEventPass.Initial)
                        view.lastInteractionMs = System.currentTimeMillis()
                    }
                }
            }
            .then(if (swipe != null) Modifier.nestedScroll(swipe.nestedScrollConnection) else Modifier),
        contentPadding = PaddingValues(top = topInset, bottom = clearance),
    ) {
        item(key = HEADER_KEY, contentType = ContentType.Header) { DownloadsHeader(expanded) }

        if (view.offline) {
            item(key = "banner", contentType = ContentType.Banner) {
                FluxBanner(
                    text = str(R.string.localServiceDisconnected),
                    kind = FluxBannerKind.Warn,
                    icon = FluxIcons.WifiOff,
                    slim = true,
                    modifier = Modifier.padding(start = margin, end = margin, bottom = 14.dp),
                )
            }
        }

        // 进行中的批量导入（多于一块才有进度）：跨「新建下载」Sheet 的关闭存活，每个导入各自可取消
        val importRuns = imports.runs.filter { it.showsProgress }
        items(importRuns, key = { "import-${it.id}" }, contentType = { ContentType.Banner }) { run ->
            ImportProgressBanner(run, Modifier.padding(start = margin, end = margin, bottom = 14.dp))
        }

        if (!expanded) {
            item(key = HERO_KEY, contentType = ContentType.Hero) {
                Box(
                    Modifier
                        .padding(start = margin, end = margin, top = 4.dp, bottom = 18.dp)
                        .graphicsLayer {
                            val s = tracker.heroScroll()
                            val h = tracker.heroHeight
                            translationY = 0.18f * s
                            val k = 1f - 0.06f * min(1f, s / h)
                            scaleX = k
                            scaleY = k
                            alpha = 1f - 0.85f * min(1f, s / (0.8f * h))
                        },
                ) { HeroCard() }
            }
            item(key = SLOT_KEY, contentType = ContentType.Slot) {
                Spacer(Modifier.height(with(density) { filtersHeightPx().toDp() }))
            }
        } else if (view.filter.hasScope) {
            item(key = "scope", contentType = ContentType.Scope) {
                ScopeBadgesRow(Modifier.padding(horizontal = margin).padding(bottom = 10.dp))
            }
        }

        val st = style
        if (!list.loaded || st == null || (list.taskTotal == 0 && view.connecting)) {
            item(key = "skeleton", contentType = ContentType.Skeleton) {
                val loading = list.loading
                val label = when {
                    loading == null -> str(R.string.mobileLoading)
                    loading.total > 0 -> str(R.string.mobileTasksLoadingProgress, "n" to loading.total)
                    else -> str(R.string.mobileTasksLoading)
                }
                Column(Modifier.padding(horizontal = margin)) {
                    Box(Modifier.cardSegment(first = true, last = true)) {
                        FluxSkeletonRows(rows = 4, loadingLabel = label)
                    }
                    // 任务表分页载入期间给出可见的进度文案（列表此时为空，不能显示空态）。
                    if (loading != null) {
                        FluxText(
                            label,
                            style = FluxTheme.type.monoS,
                            color = FluxTheme.colors.inkFaint,
                            modifier = Modifier.padding(top = 12.dp),
                        )
                    }
                }
            }
        } else if (list.entries.isEmpty()) {
            item(key = "empty", contentType = ContentType.Empty) { EmptyState(noTasks = list.taskTotal == 0) }
        } else {
            itemsIndexed(list.entries, key = { _, e -> e.key }, contentType = { _, e -> e.contentType }) { index, e ->
                when (e) {
                    is FlowHeaderEntry -> FlowSectionHeader(e)
                    is HistoryHeaderEntry -> HistorySectionHeader(e)
                    is GroupHeaderEntry -> Box(Modifier.padding(horizontal = margin)) {
                        GroupHeader(
                            title = e.label.resolve(),
                            count = e.count,
                            closed = e.closed,
                            onToggle = { view.toggleGroup(e.groupKey) },
                            extra = e.extra,
                        )
                    }
                    is RowEntry -> DownloadRow(e, st, index, gate, a11y)
                    is RemoteRowEntry -> RemoteTaskRow(e, st, index, gate, a11y)
                }
            }
        }
        if (!expanded) {
            item(key = TAIL_KEY, contentType = ContentType.Tail) {
                Spacer(Modifier.fillMaxWidth().height(with(density) { tailPx.toDp() }))
            }
        }
    }
}

// ── 页头 ─────────────────────────────────────────────────────────────────────

@Composable
private fun DownloadsHeader(expanded: Boolean) {
    val view = LocalDownloadsView.current
    val nav = LocalNavigator.current
    val container = LocalAppContainer.current
    val margin = FluxTheme.space.screenMargin
    val host by container.host.collectAsState()
    val hostName = host.title()
    val customized = view.prefs?.customized == true || view.filter.queueId != null

    FluxHeader(
        // 多选时标题换成计数（同 iOS 选择模式的导航栏标题），选择坞里只放动作
        title = when {
            nav.selecting -> str(R.string.selectedCount, "n" to nav.selection.size)
            expanded -> view.filter.folder.label()
            else -> str(R.string.mobileNavDownloads)
        },
        subtitle = if (expanded) str(R.string.nTasks, "n" to view.facets.matching) else null,
        modifier = Modifier.padding(horizontal = margin),
        hostPill = {
            FluxHostPill(
                name = hostName,
                online = view.live,
                onClick = { nav.openSheet(SheetRoute.HostSwitch) },
                description = str(R.string.mobileHostSwitchDescription, "name" to hostName),
            )
        },
    ) {
        FluxGlassIconButton(
            icon = FluxIcons.Search,
            contentDescription = str(R.string.mobileSearchHint),
            onClick = { nav.openSearch() },
        )
        FluxGlassIconButton(
            icon = FluxIcons.SlidersHorizontal,
            contentDescription = str(R.string.viewMenuLabel),
            onClick = { nav.openSheet(SheetRoute.ViewOptions) },
            on = customized,
            onDescription = str(R.string.mobileViewCustomized),
        )
    }
}

// ── 英雄卡 / 顶部读数条 ───────────────────────────────────────────────────────

@Composable
private fun HeroCard() {
    val view = LocalDownloadsView.current
    val nav = LocalNavigator.current
    val actions = LocalTaskActions.current
    val stats by rememberLiveStats()
    val activeLabel = str(R.string.mobileInstrumentActive)
    val freeLabel = str(R.string.mobileInstrumentFree)
    val wave = remember(view) { { view.wave.series } }
    val list = remember(stats, activeLabel, freeLabel) {
        buildList {
            add(InstrumentStat("↑", stats.upSpeed.toString()))
            add(InstrumentStat(activeLabel, stats.active.toString()))
            stats.diskFree?.let { add(InstrumentStat(freeLabel, Format.bytes(it).toString())) }
        }
    }
    SpeedInstrument(
        speed = SpeedReading(stats.speed.value, stats.speed.unit),
        live = stats.down > 0,
        waveform = wave,
        stats = list,
        pausedAll = stats.allPaused,
        onToggleAll = { if (stats.allPaused) actions.resumeAll() else actions.pauseAll() },
        onClick = { nav.openSheet(SheetRoute.Activity) },
        title = str(R.string.mobileInstrumentLabel),
        pauseAllLabel = str(R.string.pauseAll),
        resumeAllLabel = str(R.string.resumeAll),
        // 空闲（无活跃 / 排队任务、无传输）时收起波形，首页默认更紧凑；开始下载再展开
        showWaveform = stats.active > 0 || stats.pending > 0 || stats.down > 0 || stats.up > 0,
    )
}

/** 紧凑变体（Rail 底部）：供壳层放入 `FluxRail(footer = …)`。 */
@Composable
fun DownloadsRailFooter() {
    val view = LocalDownloadsView.current
    val nav = LocalNavigator.current
    val actions = LocalTaskActions.current
    val stats by rememberLiveStats()
    val activeLabel = str(R.string.mobileInstrumentActive)
    val wave = remember(view) { { view.wave.series } }
    val list = remember(stats, activeLabel) {
        listOf(InstrumentStat("↑", stats.upSpeed.toString()), InstrumentStat(activeLabel, stats.active.toString()))
    }
    SpeedInstrument(
        speed = SpeedReading(stats.speed.value, stats.speed.unit),
        live = stats.down > 0,
        waveform = wave,
        stats = list,
        pausedAll = stats.allPaused,
        onToggleAll = { if (stats.allPaused) actions.resumeAll() else actions.pauseAll() },
        onClick = { nav.openSheet(SheetRoute.Activity) },
        title = str(R.string.mobileInstrumentLabel),
        pauseAllLabel = str(R.string.pauseAll),
        resumeAllLabel = str(R.string.resumeAll),
        compact = true,
    )
}

@Composable
private fun BoxScope.TopStripHost(tracker: ListTracker, topInset: androidx.compose.ui.unit.Dp) {
    val view = LocalDownloadsView.current
    val nav = LocalNavigator.current
    val actions = LocalTaskActions.current
    val stats by rememberLiveStats()
    val progress = remember(tracker) { { tracker.collapse() } }
    val wave = remember(view) { { view.wave.series } }
    FluxTopStrip(
        progress = progress,
        speedValue = stats.speed.value,
        speedUnit = stats.speed.unit,
        meta = "↑ ${stats.upSpeed} · ${stats.active}",
        allPaused = stats.allPaused,
        onTogglePause = { if (stats.allPaused) actions.resumeAll() else actions.pauseAll() },
        onClick = { nav.openSheet(SheetRoute.Activity) },
        modifier = Modifier
            .align(Alignment.TopCenter)
            .padding(start = 12.dp, end = 12.dp, top = (topInset - 4.dp).coerceAtLeast(0.dp)),
        speedDescription = str(R.string.mobileLiveSpeedDescription, "speed" to stats.speed.toString()),
        pauseDescription = str(R.string.pauseAll),
        resumeDescription = str(R.string.resumeAll),
        waveform = { Waveform(samples = wave, mini = true) },
        label = if (nav.selecting) str(R.string.selectedCount, "n" to nav.selection.size) else null,
    )
}

// ── 粘性区：ScopeTabs + 分类胶囊 + 范围徽章 ─────────────────────────────────────

/**
 * 粘性筛选区。作为列表外的覆盖层按「占位项屏幕 y 与吸顶线取大」定位（draw 阶段读取，滚动零重组）；
 * 吸顶线 = 状态栏 + 54dp × 读数条折叠进度，吸顶后出现 canvas 底。
 */
@Composable
private fun BoxScope.FiltersOverlay(tracker: ListTracker, onHeight: (Int) -> Unit) {
    val c = FluxTheme.colors
    val density = LocalDensity.current
    val inset54 = with(density) { 54.dp.toPx() }
    val fadePx = with(density) { 8.dp.toPx() }
    val topPx = with(density) { WindowInsets.statusBars.getTop(this).toFloat() }

    Column(
        Modifier
            .align(Alignment.TopStart)
            .fillMaxWidth()
            .onSizeChanged { onHeight(it.height) }
            .graphicsLayer {
                val pin = topPx + inset54 * tracker.collapse()
                translationY = max(tracker.slotY(), pin)
            }
            .drawBehind {
                val pin = topPx + inset54 * tracker.collapse()
                val stuck = ((pin - tracker.slotY()) / fadePx).coerceIn(0f, 1f)
                if (stuck > 0f) {
                    val fill = c.canvas.copy(alpha = 0.88f)
                    drawRect(
                        fill,
                        topLeft = Offset(0f, -topPx),
                        size = androidx.compose.ui.geometry.Size(size.width, size.height + topPx),
                        alpha = stuck,
                    )
                }
            },
    ) {
        FiltersBlock()
    }
}

@Composable
private fun FiltersBlock() {
    val view = LocalDownloadsView.current
    val haptics = FluxTheme.haptics
    val c = FluxTheme.colors
    val margin = FluxTheme.space.screenMargin
    val facets = view.facets
    val filter = view.filter
    val tasksFmt = stringResource(R.string.nTasks)

    val tabs = StatusFolder.entries.map { f ->
        ScopeTab(f, f.label(), facets.count(f), hot = f == StatusFolder.Failed && facets.count(f) > 0)
    }
    val visibility = view.filterVisibility
    val hasCategoryChips = facets.categories.isNotEmpty() || facets.remoteCount > 0 || filter.remoteOnly
    val showCategories = visibility.categories && hasCategoryChips
    val showChips = showCategories || filter.hasScope
    // 状态条与芯片行都被隐藏时整块不占高度
    val blockEmpty = visibility.isEmpty(hasCategories = hasCategoryChips, hasScopeChip = filter.hasScope)
    Column(Modifier.fillMaxWidth().padding(bottom = if (blockEmpty) 0.dp else 10.dp)) {
        if (visibility.status) {
            ScopeTabs(
                tabs = tabs,
                selected = filter.folder,
                onSelect = view::setFolder,
                modifier = Modifier.padding(horizontal = margin),
                countDescription = { tasksFmt.fill("n" to it) },
            )
        }
        if (showChips) {
            Row(
                Modifier
                    .padding(top = if (visibility.status) 12.dp else 0.dp)
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = margin),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                ScopeBadges()
                if (visibility.categories) {
                    for (pill in facets.categories) {
                        val cat = pill.category
                        val selected = filter.categoryId == cat.id
                        FluxPill(
                            text = cat.label(),
                            selected = selected,
                            onClick = {
                                haptics.tick()
                                view.setCategory(if (selected) null else cat.id)
                            },
                            dot = c.category(cat.fileCategory()),
                            count = pill.count,
                        )
                    }
                    // 「远程任务」：只看其他设备上的远程任务（与分类单选互斥）
                    if (facets.remoteCount > 0 || filter.remoteOnly) {
                        FluxPill(
                            text = str(R.string.remoteTasksGroup),
                            selected = filter.remoteOnly,
                            onClick = {
                                haptics.tick()
                                view.setRemoteOnly(!filter.remoteOnly)
                            },
                            icon = FluxIcons.Cloud,
                            count = facets.remoteCount,
                        )
                    }
                }
            }
        }
    }
}

/** 范围徽章：队列 / 搜索（on 态小胶囊，点按清除）。 */
@Composable
private fun ScopeBadges() {
    val view = LocalDownloadsView.current
    val filter = view.filter
    val qid = filter.queueId
    if (qid != null) {
        val q = view.facets.queues.firstOrNull { normQueue(it.queue.queueId) == qid }?.queue
        val name = q?.label() ?: qid
        val label = str(R.string.mobileScopeQueue, "name" to name)
        FluxPill(
            text = label,
            selected = true,
            onClick = { view.setQueue(null) },
            small = true,
            toggleable = false,
            icon = FluxIcons.Rows3,
            trailingIcon = FluxIcons.X,
            contentDescription = str(R.string.mobileScopeClear, "name" to label),
        )
    }
    if (filter.query.isNotBlank()) {
        val label = str(R.string.mobileScopeSearch, "q" to filter.query.trim())
        FluxPill(
            text = label,
            selected = true,
            onClick = { view.setQuery("") },
            small = true,
            toggleable = false,
            icon = FluxIcons.Search,
            trailingIcon = FluxIcons.X,
            contentDescription = str(R.string.mobileScopeClear, "name" to label),
        )
    }
}

/** expanded 档：没有粘性筛选区，范围徽章单独成行。 */
@Composable
private fun ScopeBadgesRow(modifier: Modifier) {
    Row(
        modifier.horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) { ScopeBadges() }
}

// ── 分区头 / 空态 ─────────────────────────────────────────────────────────────

@Composable
private fun FlowSectionHeader(e: FlowHeaderEntry) {
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val margin = FluxTheme.space.screenMargin
    val countText = str(R.string.nTasks, "n" to e.count)
    val right = remember(e.downSpeed, countText, c) {
        buildAnnotatedString {
            withStyle(SpanStyle(color = c.accentHi)) { append("↓ ${Format.speedOrZero(e.downSpeed)}") }
            withStyle(SpanStyle(color = c.inkFaint)) { append(" · $countText") }
        }
    }
    Box(Modifier.padding(horizontal = margin)) {
        Row(
            Modifier
                .fillMaxWidth()
                .cardSegment(first = true, last = false)
                .padding(start = 18.dp, end = 18.dp, top = 18.dp, bottom = 4.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            FluxText(str(R.string.mobileSectionInFlight), style = t.micro, color = c.inkMuted, maxLines = 1)
            BasicText(right, style = t.monoS, maxLines = 1)
        }
    }
}

@Composable
private fun HistorySectionHeader(e: HistoryHeaderEntry) {
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val margin = FluxTheme.space.screenMargin
    Row(
        Modifier
            .fillMaxWidth()
            .padding(start = margin + 6.dp, end = margin + 6.dp, top = 26.dp, bottom = 6.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        FluxText(
            e.folder?.label() ?: str(R.string.mobileSectionHistory),
            style = t.micro,
            color = c.inkMuted,
            maxLines = 1,
        )
        FluxText(str(R.string.nTasks, "n" to e.count), style = t.monoS, color = c.inkFaint, maxLines = 1)
    }
}

@Composable
private fun EmptyState(noTasks: Boolean) {
    val view = LocalDownloadsView.current
    val nav = LocalNavigator.current
    FluxEmpty(
        glyph = if (noTasks) FluxGlyph.Download else FluxGlyph.Search,
        title = if (noTasks) str(R.string.emptyTitle) else str(R.string.mobileFilterEmptyTitle),
        subtitle = if (noTasks) str(R.string.mobileEmptyDownloadsSub) else str(R.string.mobileFilterEmptySub),
        modifier = Modifier.fillMaxWidth().padding(top = 40.dp),
        action = {
            if (noTasks) {
                FluxButton(
                    text = str(R.string.newDownload),
                    onClick = { nav.openSheet(SheetRoute.NewDownload()) },
                    variant = ButtonVariant.Primary,
                    size = ButtonSize.Sm,
                    icon = FluxIcons.Plus,
                )
            } else {
                FluxButton(
                    text = str(R.string.mobileResetFilter),
                    onClick = { view.clearFilters() },
                    variant = ButtonVariant.Secondary,
                    size = ButtonSize.Sm,
                )
            }
        },
    )
}
