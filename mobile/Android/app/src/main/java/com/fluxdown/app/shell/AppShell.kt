package com.fluxdown.app.shell

import android.content.ClipboardManager
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.EnterExitState
import androidx.compose.animation.core.VisibilityThreshold
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.MutatePriority
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.stopScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsTopHeight
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.compose.ui.res.stringResource
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.actions.TaskActions
import com.fluxdown.app.diag.AppDiagnostics
import com.fluxdown.app.feature.devices.DevicesScreen
import com.fluxdown.app.feature.devices.AddHostSheet
import com.fluxdown.app.feature.devices.AddDeviceSheet
import com.fluxdown.app.feature.devices.InboundPairingPrompt
import com.fluxdown.app.feature.downloads.ActivitySheet
import com.fluxdown.app.feature.downloads.DownloadsRailContext
import com.fluxdown.app.feature.downloads.DownloadsRailFooter
import com.fluxdown.app.feature.downloads.DownloadsScreen
import com.fluxdown.app.feature.downloads.DownloadsSelectionDock
import com.fluxdown.app.feature.downloads.HostSwitchSheet
import com.fluxdown.app.feature.downloads.ProvideDownloadsView
import com.fluxdown.app.feature.downloads.ViewOptionsSheet
import com.fluxdown.app.feature.newtask.MoveToQueueSheet
import com.fluxdown.app.feature.newtask.ImportEffects
import com.fluxdown.app.feature.newtask.NewDownloadSheet
import com.fluxdown.app.feature.rss.RssEditorSheet
import com.fluxdown.app.feature.rss.RssEditorTarget
import com.fluxdown.app.feature.rss.RssItemsScreen
import com.fluxdown.app.feature.rss.RssScreen
import com.fluxdown.app.feature.search.CommandSearch
import com.fluxdown.app.feature.selection.FileConflictHost
import com.fluxdown.app.feature.selection.SelectionRequestSheet
import com.fluxdown.app.feature.settings.ConfigEditor
import com.fluxdown.app.feature.settings.LocalConfigEditor
import com.fluxdown.app.feature.settings.LocalSettingsFocus
import com.fluxdown.app.feature.settings.SettingsFocus
import com.fluxdown.app.feature.settings.AppearanceSyncEffect
import com.fluxdown.app.feature.settings.account.AccountEffects
import com.fluxdown.app.update.UpdateEffects
import com.fluxdown.app.feature.settings.general.GeneralSettingsEffects
import com.fluxdown.app.feature.settings.SettingsPageScreen
import com.fluxdown.app.feature.settings.SettingsScreen
import com.fluxdown.app.feature.task.TaskDetailScreen
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.AppTab
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.Route
import com.fluxdown.app.nav.SheetRoute
import com.fluxdown.app.service.DownloadServiceEffect
import com.fluxdown.core.model.FileConflicts
import com.fluxdown.core.model.SelectionKind
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.protocol.preferences
import com.fluxdown.fluxui.chrome.FluxDock
import com.fluxdown.fluxui.chrome.FluxDockBadge
import com.fluxdown.fluxui.chrome.FluxDockItem
import com.fluxdown.fluxui.chrome.FluxOrb
import com.fluxdown.fluxui.chrome.FluxOrbAction
import com.fluxdown.fluxui.chrome.FluxOrbFanLayer
import com.fluxdown.fluxui.chrome.FluxRail
import com.fluxdown.fluxui.chrome.FluxRailNavItem
import com.fluxdown.fluxui.chrome.FluxOrbState
import com.fluxdown.fluxui.chrome.rememberFluxOrbState
import com.fluxdown.fluxui.feedback.FluxEmpty
import com.fluxdown.fluxui.feedback.FluxGlyph
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.FluxBackdrop
import com.fluxdown.fluxui.material.FluxCanvas
import com.fluxdown.fluxui.material.LocalFluxBackdrop
import com.fluxdown.fluxui.material.LocalFlowInEnabled
import com.fluxdown.fluxui.material.fluxBackdropSource
import com.fluxdown.fluxui.overlay.FluxOverlayHost
import com.fluxdown.fluxui.overlay.FluxPortalHost
import com.fluxdown.fluxui.overlay.FluxPortalState
import com.fluxdown.fluxui.overlay.LocalFluxPortal
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.overlay.LocalSwipeRevealCoordinator
import com.fluxdown.fluxui.overlay.rememberFluxOverlayState
import com.fluxdown.fluxui.overlay.rememberSwipeRevealCoordinator
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.FluxWindowClass

/** 详情栏宽度（§13.2）：medium 344 / expanded 460；Rail 272。 */
private val DetailPaneMedium = 344.dp
private val DetailPaneExpanded = 460.dp

/**
 * 全局舞台（取代 Material Scaffold）：纯色画布 + 颗粒 + 页面栈 → 坞 / 球（或 Rail）→
 * Sheet 层 → 命令搜索 → 浮层宿主（菜单 / 对话框 / toast）→ 新建球扇形层。
 * 玻璃面都在 [fluxBackdropSource] 之后绘制，以采样同一份模糊副本。
 */
@Composable
fun AppShell() {
    val container = LocalAppContainer.current
    val nav = LocalNavigator.current
    val context = LocalContext.current
    val backdrop = remember { FluxBackdrop() }
    val overlays = rememberFluxOverlayState()
    LaunchedEffect(container, overlays) {
        container.restoreLastHost()?.let { name ->
            val boundaries = android.icu.text.BreakIterator.getCharacterInstance()
            boundaries.setText(name)
            val end = boundaries.next(20)
            val label = if (end in 0 until name.length) name.substring(0, end) + "…" else name
            overlays.toast(context.str(R.string.hostRestoreFailed, "name" to label))
        }
    }
    val swipe = rememberSwipeRevealCoordinator()
    val scope = rememberCoroutineScope()
    val haptics = FluxTheme.haptics
    val actions = remember(overlays, haptics) {
        TaskActions(context.applicationContext, scope, { container.session }, container.store, overlays, nav, haptics) {
            container.host.value is com.fluxdown.core.model.HostRef.Local
        }
    }
    val disconnectedText = stringResource(R.string.localServiceDisconnected)
    val invalidText = stringResource(R.string.localServiceInvalidArgument)
    val configEditor = remember(container, overlays, haptics, actions, disconnectedText, invalidText) {
        ConfigEditor(container, overlays, haptics, actions::errorText, disconnectedText, invalidText)
    }
    val settingsFocus = remember { SettingsFocus() }
    // 根节点只读派生量：避免 10 Hz 的主机状态发布让整棵树重组
    val host = hostState()
    val failed by remember { derivedStateOf { host.value.tasks.any { it.status == TaskStatus.Failed } } }
    // X1–X3 单请求 Sheet 只看非 fileExists 的请求；fileExists 聚合成一个对话框（FileConflictHost）
    val selection by remember { derivedStateOf { host.value.selections.firstOrNull { it.kind !is SelectionKind.FileExists } } }
    val fileConflicts by remember { derivedStateOf { FileConflicts.pending(host.value.selections) } }
    val windowClass = FluxTheme.windowClass
    val orb = rememberFluxOrbState()
    // 回到前台：文件跟踪重扫（10s 节流，对齐 RescanThrottle；空闲静默期间不轮询）
    LifecycleEventEffect(Lifecycle.Event.ON_START) {
        container.rescanOnForeground()
        // 应用更新：重试挂起的安装、6h 节流检查、条件允许时自动下载（永不自动安装）
        container.updates.onForeground()
        // 仍在等待的 fileExists 询问：回到前台重新弹出（含点通知进入）
        nav.reopenFileConflicts()
    }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { AppDiagnostics.refreshSharedLogStatus() }
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) { AppDiagnostics.publishSharedLogs() }
    // 本机有活跃 / 排队任务 → 前台服务（dataSync）；首次下载时请求通知权限
    DownloadServiceEffect()
    val portal = remember { FluxPortalState() }
    val pagingBlocked = orb.fanOpen || overlays.hasMenu || overlays.hasDialog ||
        selection != null || FileConflicts.shouldShow(fileConflicts, nav.deferredFileConflicts)

    CompositionLocalProvider(
        LocalFluxBackdrop provides backdrop,
        LocalFluxOverlays provides overlays,
        LocalSwipeRevealCoordinator provides swipe,
        LocalTaskActions provides actions,
        LocalConfigEditor provides configEditor,
        LocalSettingsFocus provides settingsFocus,
        LocalFluxPortal provides portal,
    ) {
        // 设置的应用侧副作用（各分类自管）：完成通知 / 保持唤醒等、外观与云同步偏好互通、会话吊销提示。
        GeneralSettingsEffects()
        AppearanceSyncEffect()
        AccountEffects()
        UpdateEffects()
        ImportEffects()
        ProvideDownloadsView {
            BackHandler(enabled = nav.searchOpen || nav.sheet != null || nav.selecting || nav.stack.isNotEmpty() || nav.tab != AppTab.Downloads) {
                nav.back()
            }
            Box(Modifier.fillMaxSize()) {
                Box(Modifier.fillMaxSize().fluxBackdropSource(backdrop)) {
                    // 背景源内部不得采样自身（RenderNode 环）：页面内的玻璃面取 null → 平玻璃 / 实色；
                    // 需要真模糊的页内浮层（读数条等）由页面自建局部背景源，页内 Sheet 经 FluxPortal 传送到浮层层。
                    CompositionLocalProvider(LocalFluxBackdrop provides null) {
                        FluxCanvas(modifier = Modifier.fillMaxSize()) {
                            when (windowClass) {
                                FluxWindowClass.Expanded -> ExpandedStage(pagingBlocked)
                                else -> CompactStage(paned = windowClass == FluxWindowClass.Medium, pagingBlocked = pagingBlocked)
                            }
                        }
                    }
                    StatusScrim()
                }

                if (windowClass != FluxWindowClass.Expanded) BottomChrome(failed = failed, orb = orb)

                Sheets()
                FluxPortalHost(portal)
                SelectionRequestSheet(request = selection)
                InboundPairingPrompt()
                FileConflictHost(requests = fileConflicts, blocked = selection != null)
                CommandSearch(visible = nav.searchOpen, onDismiss = nav::closeSearch)
                FluxOverlayHost(overlays)
                if (windowClass != FluxWindowClass.Expanded) FluxOrbFanLayer(orb)
            }
        }
    }
}

/** 状态栏纯色罩：内容之上、系统图标之下（§4.4）。 */
@Composable
private fun BoxScope.StatusScrim() {
    val c = FluxTheme.colors
    Box(
        Modifier
            .align(Alignment.TopCenter)
            .fillMaxWidth()
            .windowInsetsTopHeight(WindowInsets.statusBars)
            .padding(bottom = 0.dp)
            .background(c.scrimTopFrom),
    )
}

/** 页面栈只包含推入深度与路由；根标签由同一个 Pager 保持，不重建页面栈。 */
private data class PageKey(val depth: Int, val route: Route?)

/** compact / medium：顶层页 + 推入页；medium 档任务详情进右栏（不做推入）。 */
@Composable
private fun CompactStage(paned: Boolean, pagingBlocked: Boolean) {
    val nav = LocalNavigator.current
    val top = nav.top
    val paneTask = (top as? Route.TaskDetail)?.takeIf { paned && nav.tab == AppTab.Downloads }
    Row(Modifier.fillMaxSize()) {
        PageStack(Modifier.weight(1f).fillMaxHeight(), if (paneTask != null) null else top, expanded = false, pagingBlocked = pagingBlocked)
        if (paned && nav.tab == AppTab.Downloads) DetailPane(paneTask?.taskId, DetailPaneMedium)
    }
}

/** expanded：Rail 272 | 列表 | 详情 460（PC 三栏心智）。 */
@Composable
private fun ExpandedStage(pagingBlocked: Boolean) {
    val nav = LocalNavigator.current
    val top = nav.top
    val paneTask = (top as? Route.TaskDetail)?.takeIf { nav.tab == AppTab.Downloads }
    Row(Modifier.fillMaxSize()) {
        AppRail()
        PageStack(Modifier.weight(1f).fillMaxHeight(), if (paneTask != null) null else top, expanded = true, pagingBlocked = pagingBlocked)
        if (nav.tab == AppTab.Downloads) DetailPane(paneTask?.taskId, DetailPaneExpanded)
    }
}

/**
 * 页面过渡（对齐 iOS 导航）：推入 = 新页整页不透明地自右滑入、盖在上层，下层页向左视差 30%；返回反向，
 * 离场页保持在上层滑出。`page` 弹簧临界阻尼、无回弹；全程不缩放、不模糊、不淡化文字。
 * 根页之间由 Pager 跟手切换，落定后才更新导航。入场期间关闭流入（[LocalFlowInEnabled]）。
 * Reduce motion → 瞬时。
 */
@Composable
private fun PageStack(modifier: Modifier, route: Route?, expanded: Boolean, pagingBlocked: Boolean) {
    val nav = LocalNavigator.current
    val motion = FluxTheme.motion
    val host = hostState()
    val tabs = visibleTabs()
    val pager = rememberPagerState(initialPage = tabs.indexOf(nav.tab)) { tabs.size }
    val roots = rememberSaveableStateHolder()
    val pagingEnabled = !pagingBlocked && nav.stack.isEmpty() &&
        !nav.selecting && !nav.searchOpen && nav.sheet == null
    val latestEnabled by rememberUpdatedState(pagingEnabled)
    val revision = nav.revision
    val selectedTab = nav.tab
    var syncedRevision by remember { mutableLongStateOf(-1L) }
    // 不接收子级滚动容器的剩余横向位移 / fling：chips、滑块、行内侧滑由子级拥有。
    val childScrollBoundary = remember { object : NestedScrollConnection {} }
    LaunchedEffect(revision, tabs, pagingEnabled) {
        if (nav.revision != revision) return@LaunchedEffect
        // 导航是事实源；恢复、点选、隐藏 RSS 与弹出浮层都会撤销旧手势并对齐到当前标签。
        pager.stopScroll(MutatePriority.PreventUserInput)
        pager.scrollToPage(tabs.indexOf(selectedTab))
        syncedRevision = revision
        if (!pagingEnabled) return@LaunchedEffect
        var scrolling = false
        snapshotFlow { pager.isScrollInProgress }.collect { inProgress ->
            if (inProgress) {
                scrolling = true
            } else if (scrolling) {
                scrolling = false
                val currentTabs = rootTabs(host.value.preferences.bool("ui.show_activity_rss", true), nav.tab)
                if (latestEnabled && currentTabs == tabs) {
                    tabs.getOrNull(pager.settledPage)?.let { tab ->
                        nav.selectTabFromSwipe(tab, revision)
                    }
                }
            }
        }
    }
    AnimatedContent(
        targetState = PageKey(if (route == null) 0 else nav.stack.size, route),
        modifier = modifier,
        contentKey = { it.route ?: "root" },
        transitionSpec = {
            val slide = motion.of<IntOffset>(motion.page, IntOffset.VisibilityThreshold)
            when {
                targetState.depth >= initialState.depth ->
                    (slideInHorizontally(slide) { w -> w } togetherWith slideOutHorizontally(slide) { w -> -w * 3 / 10 })
                        .apply { targetContentZIndex = 1f }
                else ->
                    (slideInHorizontally(slide) { w -> -w * 3 / 10 } togetherWith slideOutHorizontally(slide) { w -> w })
                        .apply { targetContentZIndex = -1f }
            }
        },
        label = "page",
    ) { key ->
        val r = key.route
        val entering = transition.targetState == EnterExitState.Visible && transition.currentState != EnterExitState.Visible
        // 推入页铺实色画布，根页的保存状态按标签隔离，离屏 / 推入后仍保留列表位置。
        CompositionLocalProvider(LocalFlowInEnabled provides (!entering && !pager.isScrollInProgress)) {
            Box(if (r == null) Modifier.fillMaxSize() else Modifier.fillMaxSize().background(FluxTheme.colors.canvas)) {
                if (r == null) {
                    HorizontalPager(
                        state = pager,
                        modifier = Modifier.fillMaxSize(),
                        key = { tabs[it].name },
                        userScrollEnabled = pagingEnabled && syncedRevision == nav.revision,
                        pageNestedScrollConnection = childScrollBoundary,
                    ) { page ->
                        val tab = tabs[page]
                        roots.SaveableStateProvider(tab.name) {
                            Box(
                                if (tab == AppTab.Downloads) Modifier.fillMaxSize()
                                else Modifier.fillMaxSize().background(FluxTheme.colors.canvas),
                            ) {
                                TabRoot(tab, expanded)
                            }
                        }
                    }
                } else {
                    RouteContent(r, inPane = false)
                }
            }
        }
    }
}

@Composable
private fun DetailPane(taskId: String?, width: Dp) {
    val nav = LocalNavigator.current
    val c = FluxTheme.colors
    Box(Modifier.width(width).fillMaxHeight()) {
        Box(Modifier.fillMaxHeight().width(0.5.dp).background(c.hairline))
        if (taskId == null) {
            FluxEmpty(
                glyph = FluxGlyph.File,
                title = str(R.string.selectTaskHint),
                subtitle = str(R.string.mobileSelectTaskHint),
                modifier = Modifier.align(Alignment.Center),
            )
        } else {
            TaskDetailScreen(taskId = taskId, inPane = true, onClose = { nav.pop() })
        }
    }
}

@Composable
private fun TabRoot(tab: AppTab, expanded: Boolean) {
    when (tab) {
        AppTab.Downloads -> DownloadsScreen(expanded = expanded)
        AppTab.Rss -> RssScreen()
        AppTab.Devices -> DevicesScreen()
        AppTab.Settings -> SettingsScreen()
    }
}

@Composable
private fun RouteContent(route: Route, inPane: Boolean) {
    val nav = LocalNavigator.current
    when (route) {
        is Route.TaskDetail -> TaskDetailScreen(taskId = route.taskId, inPane = inPane, onClose = { nav.pop() })
        is Route.RssItems -> RssItemsScreen(sourceId = route.sourceId)
        is Route.Settings -> SettingsPageScreen(route.page, route.arg)
    }
}

@Composable
private fun Sheets() {
    val nav = LocalNavigator.current
    val sheet = nav.sheet
    NewDownloadSheet(route = sheet as? SheetRoute.NewDownload, onDismiss = nav::closeSheet)
    MoveToQueueSheet(route = sheet as? SheetRoute.MoveToQueue, onDismiss = nav::closeSheet)
    ViewOptionsSheet(visible = sheet == SheetRoute.ViewOptions, onDismiss = nav::closeSheet)
    HostSwitchSheet(visible = sheet == SheetRoute.HostSwitch, onDismiss = nav::closeSheet)
    AddHostSheet(route = sheet as? SheetRoute.AddHost, onDismiss = nav::closeSheet)
    AddDeviceSheet(visible = sheet == SheetRoute.AddDevice, onDismiss = nav::closeSheet)
    ActivitySheet(visible = sheet == SheetRoute.Activity, onDismiss = nav::closeSheet)
    val rss = sheet as? SheetRoute.RssEditor
    RssEditorSheet(
        target = rss?.let { r -> r.sourceId?.let { RssEditorTarget.Edit(it) } ?: RssEditorTarget.Create(r.prefillUrl) },
        onDismiss = nav::closeSheet,
    )
}

/**
 * 坞 / Rail 的目的地：订阅标签由云同步偏好 `ui.show_activity_rss` 隐藏；正停留在订阅页时仍保留，
 * 命令搜索的「前往订阅」等入口照常可进入。
 */
@Composable
private fun visibleTabs(): List<AppTab> {
    val nav = LocalNavigator.current
    val host = hostState()
    val showRss by remember { derivedStateOf { host.value.preferences.bool("ui.show_activity_rss", true) } }
    return rootTabs(showRss, nav.tab)
}

internal fun rootTabs(showRss: Boolean, selected: AppTab): List<AppTab> =
    if (showRss || selected == AppTab.Rss) AppTab.entries else AppTab.entries - AppTab.Rss

@Composable
private fun dockItems(failed: Boolean, tabs: List<AppTab>): List<FluxDockItem> {
    val host = hostState()
    val unread by remember { derivedStateOf { host.value.rssSources.sumOf { it.unreadCount } } }
    return listOf(
        FluxDockItem(
            FluxIcons.ArrowDown, str(R.string.mobileNavDownloads),
            badge = if (failed) FluxDockBadge.Dot else null,
            badgeDescription = if (failed) str(R.string.mobileFailedTasksDot) else null,
        ),
        FluxDockItem(
            FluxIcons.Rss, str(R.string.mobileNavRss),
            badge = if (unread > 0) FluxDockBadge.Count(unread) else null,
            badgeDescription = if (unread > 0) str(R.string.mobileUnreadCount, "n" to unread) else null,
        ),
        FluxDockItem(FluxIcons.Smartphone, str(R.string.mobileNavDevices)),
        FluxDockItem(FluxIcons.Settings, str(R.string.mobileNavSettings)),
    ).filterIndexed { index, _ -> AppTab.entries[index] in tabs }
}

/** 浮动导航坞 + 新建球（多选时坞变形为选择坞，球变为“退出选择”）。 */
@Composable
private fun BoxScope.BottomChrome(failed: Boolean, orb: FluxOrbState) {
    val nav = LocalNavigator.current
    val tabs = visibleTabs()
    val context = LocalContext.current
    val density = LocalDensity.current
    val navInset = with(density) { WindowInsets.navigationBars.getBottom(this).toDp() }
    val imeVisible = WindowInsets.ime.getBottom(density) > 0
    val medium = FluxTheme.windowClass == FluxWindowClass.Medium
    val bottom = navInset + 8.dp
    val pushed = nav.stack.isNotEmpty() && !(medium && nav.top is Route.TaskDetail && nav.tab == AppTab.Downloads)
    val hidden = imeVisible || pushed || nav.searchOpen
    val pasteLabel = str(R.string.mobileOrbPaste)

    Box(
        Modifier
            .align(Alignment.BottomCenter)
            .fillMaxWidth()
            .padding(start = 16.dp, end = 16.dp, bottom = bottom)
            .height(FluxTheme.space.dockHeight),
    ) {
        FluxDock(
            items = dockItems(failed, tabs),
            selected = tabs.indexOf(nav.tab),
            onSelect = { nav.selectTab(tabs[it]) },
            hidden = hidden,
            selectionMode = nav.selecting,
            contentDescription = str(R.string.mobileMainNav),
        )
        DownloadsSelectionDock(Modifier.fillMaxWidth())
        if (!hidden) {
            val rssTab = nav.tab == AppTab.Rss
            FluxOrb(
                state = orb,
                // 订阅页上新建 = 添加订阅；其余页 = 新建下载
                onClick = {
                    when {
                        nav.selecting -> nav.exitSelection()
                        rssTab -> nav.openSheet(SheetRoute.RssEditor())
                        else -> nav.openSheet(SheetRoute.NewDownload())
                    }
                },
                actions = listOf(FluxOrbAction(FluxIcons.ClipboardPaste, pasteLabel)),
                onAction = {
                    val clip = context.getSystemService(ClipboardManager::class.java)?.primaryClip
                    val text = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(context)?.toString().orEmpty()
                    if (rssTab) {
                        nav.openSheet(SheetRoute.RssEditor(prefillUrl = text.trim()))
                    } else {
                        nav.openSheet(SheetRoute.NewDownload(prefill = text.trim()))
                    }
                },
                modifier = Modifier.align(Alignment.CenterEnd),
                selectionMode = nav.selecting,
                contentDescription = str(if (rssTab) R.string.rssAddSource else R.string.newDownload),
                exitDescription = str(R.string.mobileExitSelection),
            )
        }
    }
}

/** expanded 档：Rail 取代坞与球；下载页二级上下文（状态文件夹 / 分类 / 队列）由下载特性提供。 */
@Composable
private fun AppRail() {
    val nav = LocalNavigator.current
    val tabs = visibleTabs()
    val items = listOf(
        FluxRailNavItem(AppTab.Downloads.name, str(R.string.mobileNavDownloads), FluxIcons.ArrowDown),
        FluxRailNavItem(AppTab.Rss.name, str(R.string.mobileNavRss), FluxIcons.Rss),
        FluxRailNavItem(AppTab.Devices.name, str(R.string.mobileNavDevices), FluxIcons.Smartphone),
        FluxRailNavItem(AppTab.Settings.name, str(R.string.mobileNavSettings), FluxIcons.Settings),
    ).filter { AppTab.valueOf(it.id) in tabs }
    FluxRail(
        nav = items,
        selected = nav.tab.name,
        onSelect = { nav.selectTab(AppTab.valueOf(it)) },
        newLabel = str(if (nav.tab == AppTab.Rss) R.string.rssAddSource else R.string.newDownload),
        onNew = { nav.openSheet(if (nav.tab == AppTab.Rss) SheetRoute.RssEditor() else SheetRoute.NewDownload()) },
        extra = if (nav.tab == AppTab.Downloads) ({ DownloadsRailContext() }) else null,
        footer = { DownloadsRailFooter() },
    )
}
