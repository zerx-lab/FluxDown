package com.fluxdown.app.feature.rss

import androidx.activity.compose.BackHandler
import androidx.annotation.StringRes
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.feature.settings.PageMaxWidth
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.Route
import com.fluxdown.app.nav.SheetRoute
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.format.Format
import com.fluxdown.core.model.RssSource
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.RssChipKind
import com.fluxdown.core.protocol.RssChipTone
import com.fluxdown.core.protocol.RssItemAction
import com.fluxdown.core.protocol.RssItemChip
import com.fluxdown.core.protocol.RssItemDto
import com.fluxdown.core.protocol.RssItemStatus
import com.fluxdown.core.protocol.RssItems
import com.fluxdown.core.protocol.RssLinkedTask
import com.fluxdown.core.protocol.RssReason
import com.fluxdown.core.protocol.long
import com.fluxdown.core.store.Connection
import com.fluxdown.fluxui.chrome.FluxGlassIconButton
import com.fluxdown.fluxui.chrome.FluxPageHead
import com.fluxdown.fluxui.chrome.SelectionDock
import com.fluxdown.fluxui.chrome.SelectionDockAction
import com.fluxdown.fluxui.controls.ButtonSize
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxCheckbox
import com.fluxdown.fluxui.controls.FluxDivider
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxFieldAction
import com.fluxdown.fluxui.controls.FluxTag
import com.fluxdown.fluxui.controls.Tone
import com.fluxdown.fluxui.feedback.FluxEmpty
import com.fluxdown.fluxui.feedback.FluxGlyph
import com.fluxdown.fluxui.feedback.FluxSpinner
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.FluxBackdrop
import com.fluxdown.fluxui.material.LocalFluxBackdrop
import com.fluxdown.fluxui.material.fluxBackdropSource
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxMenuItem
import com.fluxdown.fluxui.overlay.FluxSwipeAction
import com.fluxdown.fluxui.overlay.FluxSwipeTone
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.overlay.LocalSwipeRevealCoordinator
import com.fluxdown.fluxui.overlay.SwipeReveal
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.fluxPressable
import java.text.DateFormat
import java.util.Date

/** 推入页头占位高度：上 6 + 钮 44 + 下 10。 */
private val PageHeadHeight = 60.dp

/**
 * R2 条目流（订阅页点按订阅行进入）：某订阅已抓取的条目；搜索、按发布时间排序、选择模式（批量下载 / 忽略）、
 * 条目状态与关联任务状态联动（`HostState.tasks` 按 `taskId`，不另行轮询）。
 * 过滤原因文案来自引擎的稳定原因码，求值由主机完成。订阅被删除时自动返回。
 */
@Composable
fun RssItemsScreen(sourceId: String) {
    val container = LocalAppContainer.current
    val nav = LocalNavigator.current
    val actions = LocalTaskActions.current
    val overlays = LocalFluxOverlays.current
    val haptics = FluxTheme.haptics
    val appContext = LocalContext.current.applicationContext
    val controller = rememberRssController()
    val model = remember(sourceId, container, controller, actions, haptics, appContext) {
        RssItemsModel(sourceId, container.appScope, { container.session }, actions, controller, haptics, appContext)
    }

    val host = hostState()
    val source by remember(sourceId) { derivedStateOf { host.value.rssSources.firstOrNull { it.sourceId == sourceId } } }
    val live by remember { derivedStateOf { host.value.connection == Connection.Live } }
    val offline by remember { derivedStateOf { host.value.connection.let { it is Connection.Stale || it is Connection.Failed } } }
    val readOnly by remember { derivedStateOf { host.value.isReadOnly } }
    val revisionRaw by remember { derivedStateOf { host.value.sections[HostSection.daemonRssItemRevisions] } }
    val revision = remember(revisionRaw, sourceId) { Json.parseOrNull(revisionRaw).long(sourceId) }

    // 已建任务条目引用的任务的当前状态（只取被引用的任务）
    val linkedIds = remember(model.items) {
        model.items.filter { it.state == RssItemStatus.Downloaded && it.taskId.isNotEmpty() }.mapTo(HashSet()) { it.taskId }
    }
    val linked = remember(linkedIds) {
        derivedStateOf {
            if (linkedIds.isEmpty()) emptyMap() else host.value.tasks.filter { it.taskId in linkedIds }.associate { it.taskId to RssLinkedTask(it) }
        }
    }
    val visible by remember(model) { derivedStateOf { RssItems.visible(model.items, model.query, model.oldestFirst) } }

    // 重拉触发：条目流修订号 / 手动重试 / 重连恢复
    LaunchedEffect(model, revision, model.reloadTick, live) { if (live) model.load() }
    LaunchedEffect(model, container) { container.store.notices.collect { model.consumeNotice(it) } }
    // 订阅被删除：自动返回
    val missing = source == null
    LaunchedEffect(missing, live) {
        if (missing && live && nav.top == Route.RssItems(sourceId)) nav.pop()
    }
    BackHandler(enabled = model.selecting) { model.toggleSelecting() }

    val listState = rememberLazyListState()
    LaunchedEffect(model.scrollTopTick) {
        if (model.scrollTopTick > 0) listState.animateScrollToItem(0)
    }

    val margin = FluxTheme.space.screenMargin
    val statusTop = WindowInsets.statusBars.asPaddingValues().calculateTopPadding()
    val navBottom = WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()
    val scrolled by remember(listState) { derivedStateOf { listState.canScrollBackward } }
    val swipe = LocalSwipeRevealCoordinator.current
    val nowSec by rememberNowSeconds()
    val nowMin = nowSec / 60
    val current = source
    val refreshing = current != null && current.sourceId in controller.busy
    val canAct = !readOnly
    val title = current?.let { it.name.ifBlank { it.url } }.orEmpty()
    val moreRect = remember { RectBox() }

    fun openEditor() {
        if (actions.guard()) nav.openSheet(SheetRoute.RssEditor(sourceId = sourceId))
    }

    val moreMenu = buildList<FluxMenuItem> {
            add(FluxMenuItem.Header(str(R.string.rssPublishedAt)))
            add(FluxMenuItem.Action(str(R.string.rssSortNewest), { model.oldestFirst = false }, checked = !model.oldestFirst))
            add(FluxMenuItem.Action(str(R.string.rssSortOldest), { model.oldestFirst = true }, checked = model.oldestFirst))
            add(FluxMenuItem.Divider)
            add(FluxMenuItem.Action(str(R.string.rssManageTitle), ::openEditor, icon = FluxIcons.SquarePen, enabled = canAct))
            if (current != null) {
                add(
                    FluxMenuItem.Action(
                        str(R.string.rssRefreshNow),
                        { controller.refresh(current) },
                        icon = FluxIcons.RefreshCw,
                        enabled = canAct && !refreshing,
                    ),
                )
                add(
                    FluxMenuItem.Action(
                        str(R.string.rssMarkAllRead),
                        { model.markAllRead(current) },
                        icon = FluxIcons.CheckCheck,
                        enabled = canAct && !model.readBusy && current.unreadCount > 0,
                    ),
                )
                add(FluxMenuItem.Action(str(R.string.copyUrl), { controller.copyLink(current) }, icon = FluxIcons.Copy))
            }
        }
    val openMore = { overlays.showMenu(moreRect.rect, moreMenu) }
    val listBackdrop = remember { FluxBackdrop() }
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.TopCenter) {
        Box(Modifier.widthIn(max = PageMaxWidth).fillMaxWidth().fillMaxHeight()) {
            LazyColumn(
                state = listState,
                modifier = Modifier
                    .fillMaxSize()
                    // 页面级背景源：推入页里舞台级背景源取 null，选择坞要靠它采样列表做毛玻璃。
                    .fluxBackdropSource(listBackdrop)
                    .then(if (swipe != null) Modifier.nestedScroll(swipe.nestedScrollConnection) else Modifier),
                contentPadding = PaddingValues(
                    top = statusTop + PageHeadHeight + 4.dp,
                    bottom = navBottom + if (model.selecting) FluxTheme.space.dockClearance else FluxTheme.space.pageClearance,
                ),
            ) {
                item(key = "header") {
                    ItemsHeader(
                        model = model,
                        source = current,
                        offline = offline,
                        refreshing = refreshing,
                        nowMin = nowMin,
                        modifier = Modifier.padding(horizontal = margin, vertical = 8.dp),
                    )
                }
                if (visible.isEmpty()) {
                    item(key = "empty") {
                        Box(Modifier.fillMaxWidth().padding(top = 24.dp), contentAlignment = Alignment.Center) {
                            ItemsEmpty(
                                model = model,
                                source = current,
                                offline = offline,
                                readOnly = readOnly,
                                onRetry = {
                                    if (model.phase == ItemsPhase.Failed) model.reload() else current?.let(controller::refresh)
                                },
                                onCheckConfig = ::openEditor,
                            )
                        }
                    }
                } else {
                    items(visible, key = { it.guid }) { item ->
                        val task = linked.value[item.taskId]
                        val hasTask = item.state == RssItemStatus.Downloaded && task != null
                        RssItemRow(
                            item = item,
                            chip = RssItems.chip(item, task),
                            busy = item.guid in model.busy,
                            expanded = item.guid in model.expanded,
                            selecting = model.selecting,
                            selected = item.guid in model.selection,
                            canAct = canAct,
                            hasTask = hasTask,
                            onTap = {
                                when {
                                    model.selecting -> model.toggleSelected(item.guid)
                                    hasTask -> nav.push(Route.TaskDetail(item.taskId))
                                    else -> model.toggleExpanded(item.guid)
                                }
                            },
                            onSelect = { model.startSelecting(item.guid) },
                            onDownload = { model.act(listOf(item.guid), RssItemAction.Download) },
                            onIgnore = { model.act(listOf(item.guid), RssItemAction.Ignore) },
                            onCopy = { model.copyLink(item) },
                            onOpenTask = { nav.push(Route.TaskDetail(item.taskId)) },
                        )
                    }
                }
            }

            FluxPageHead(
                title = if (model.selecting) str(R.string.rssSelectedCount, "n" to model.selection.size) else title,
                modifier = Modifier.padding(top = statusTop),
                onLead = { nav.pop() },
                scrolled = scrolled,
                leftAligned = true,
                backDescription = str(R.string.back),
            ) {
                FluxGlassIconButton(
                    icon = if (model.selecting) FluxIcons.Check else FluxIcons.ListChecks,
                    contentDescription = str(if (model.selecting) R.string.mobileRssSelectDone else R.string.mobileRssSelect),
                    onClick = model::toggleSelecting,
                    on = model.selecting,
                    enabled = model.selecting || model.items.isNotEmpty(),
                )
                FluxGlassIconButton(
                    icon = FluxIcons.Ellipsis,
                    contentDescription = str(R.string.moreActions),
                    onClick = openMore,
                    modifier = Modifier.onGloballyPositioned { moreRect.rect = it.boundsInRoot() },
                )
            }

            val selectedVisible = visible.count { it.guid in model.selection }
            val ignorable = model.items.filter { it.state == RssItemStatus.New && it.guid in model.selection }.map { it.guid }
            CompositionLocalProvider(LocalFluxBackdrop provides listBackdrop) {
                SelectionDock(
                    count = model.selection.size,
                    total = visible.size,
                    onToggleAll = { model.setVisibleSelected(visible, selected = visible.isNotEmpty() && selectedVisible != visible.size) },
                    actions = listOf(
                        SelectionDockAction(
                            id = "ignore",
                            label = str(R.string.rssIgnoreSelected),
                            icon = FluxIcons.EyeOff,
                            onClick = { model.act(ignorable, RssItemAction.Ignore) },
                            enabled = canAct && ignorable.any { it !in model.busy },
                        ),
                        SelectionDockAction(
                            id = "download",
                            label = str(R.string.rssDownloadSelected),
                            icon = FluxIcons.Download,
                            onClick = { model.act(model.selection.toList(), RssItemAction.Download) },
                            enabled = canAct && model.selection.any { it !in model.busy },
                        ),
                    ),
                    visible = model.selecting,
                    modifier = Modifier
                        .align(Alignment.BottomCenter)
                        .padding(start = margin, end = margin, bottom = navBottom + 8.dp),
                    reservedEnd = 0.dp,
                    selectAllLabel = str(R.string.selectAll),
                    deselectAllLabel = str(R.string.deselectAll),
                    countDescription = { appContext.str(R.string.rssSelectedCount, "n" to it) },
                )
            }
        }
    }
}

// ── 顶部（横幅 / 订阅摘要 / 搜索） ────────────────────────────────────────

@Composable
private fun ItemsHeader(
    model: RssItemsModel,
    source: RssSource?,
    offline: Boolean,
    refreshing: Boolean,
    nowMin: Long,
    modifier: Modifier = Modifier,
) {
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val feedback = model.feedback
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (offline) {
            FluxBanner(str(R.string.localServiceDisconnected), kind = FluxBannerKind.Warn, icon = FluxIcons.WifiOff, slim = true)
        }
        if (feedback != null) {
            FluxBanner(
                text = feedback.text,
                modifier = if (feedback.jumpsToTop) Modifier.fluxPressable(onClick = model::jumpToTop, role = Role.Button) else Modifier,
                kind = feedback.kind,
                onClose = model::dismissFeedback,
                slim = true,
            )
        }
        if (source != null) {
            val failed = source.failCount > 0
            val statusText = rssStatusText(source, refreshing, nowMin)
            val modeText = str(if (source.autoDownload) R.string.rssAutoDownloadOn else R.string.rssCollectMode)
            val line = listOfNotNull(
                statusText,
                rssIntervalText(source),
                modeText,
                str(R.string.mobileRssDisabled).takeIf { !source.enabled },
            ).joinToString(" · ")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.Top) {
                FluxText(
                    line,
                    modifier = Modifier.weight(1f),
                    style = t.sm,
                    color = when {
                        refreshing -> c.accentHi
                        failed -> c.coralText
                        else -> c.inkMuted
                    },
                    maxLines = 2,
                )
                if (source.unreadCount > 0) {
                    FluxText(str(R.string.rssUnreadCount, "n" to source.unreadCount), style = t.sm, color = c.accentHi, maxLines = 1)
                }
            }
            if (failed && source.lastError.isNotBlank()) {
                FluxText(source.lastError, style = t.monoS, color = c.coralText, maxLines = 2)
            }
        }
        FluxField(
            value = model.query,
            onValueChange = { model.query = it },
            placeholder = str(R.string.rssSearchHint),
            trailing = if (model.query.isNotEmpty()) {
                { FluxFieldAction(FluxIcons.X, str(R.string.close), onClick = { model.query = "" }) }
            } else {
                null
            },
        )
    }
}

// ── 空 / 加载 / 失败 ────────────────────────────────────────────────────

@Composable
private fun ItemsEmpty(
    model: RssItemsModel,
    source: RssSource?,
    offline: Boolean,
    readOnly: Boolean,
    onRetry: () -> Unit,
    onCheckConfig: () -> Unit,
) {
    val trimmed = model.query.trim()
    when {
        model.phase == ItemsPhase.Loading ->
            if (offline) FluxEmpty(FluxGlyph.Wifi, str(R.string.localServiceDisconnected)) else Fetching()
        trimmed.isNotEmpty() && model.items.isNotEmpty() ->
            FluxEmpty(FluxGlyph.Search, str(R.string.rssNoMatch, "query" to trimmed.lowercase()))
        model.phase == ItemsPhase.Failed || source?.lastError?.isNotEmpty() == true ->
            FluxEmpty(
                glyph = FluxGlyph.Inbox,
                title = str(R.string.rssEmptyError),
                subtitle = str(R.string.rssEmptyErrorHint),
                action = {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                        FluxButton(str(R.string.rssEmptyRetry), onClick = onRetry, variant = ButtonVariant.Primary, enabled = !readOnly)
                        FluxButton(str(R.string.rssCheckConfig), onClick = onCheckConfig, enabled = !readOnly)
                    }
                },
            )
        source != null && source.enabled && source.lastSuccessAt == 0L -> Fetching()
        else -> FluxEmpty(FluxGlyph.Rss, str(R.string.rssEmptyTitle), subtitle = str(R.string.rssEmptyDesc))
    }
}

@Composable
private fun Fetching() {
    FluxEmpty(
        glyph = FluxGlyph.Rss,
        title = str(R.string.rssEmptyFetching),
        subtitle = str(R.string.rssEmptyFetchingHint),
        action = { FluxSpinner() },
    )
}

// ── 条目行 ──────────────────────────────────────────────────────────────

/**
 * 条目行：标题（新条目加粗）/ 元信息 / 状态 chip + 过滤原因；点按展开完整标题与链接（已建任务且任务仍在 = 打开任务详情）。
 * chip 形状 + 颜色双通道；下载中带 14dp 进度环。左滑 = 忽略（仅新条目），右滑 = 下载，长按 = 菜单。
 */
@Composable
private fun RssItemRow(
    item: RssItemDto,
    chip: RssItemChip,
    busy: Boolean,
    expanded: Boolean,
    selecting: Boolean,
    selected: Boolean,
    canAct: Boolean,
    hasTask: Boolean,
    onTap: () -> Unit,
    onSelect: () -> Unit,
    onDownload: () -> Unit,
    onIgnore: () -> Unit,
    onCopy: () -> Unit,
    onOpenTask: () -> Unit,
) {
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val overlays = LocalFluxOverlays.current
    val margin = FluxTheme.space.screenMargin

    val published = str(R.string.rssPublishedAt)
    val meta = remember(item.pubDate, item.enclosureLength, published) { metaText(item, published) }
    val chipText = if (busy) str(R.string.rssActionPreparing) else str(chipTitle(chip.kind))
    val reasonText = reasonRes(item.reasonCode)?.let { str(it) }
    val downloadLabel = str(
        when {
            busy -> R.string.rssActionPreparing
            item.state == RssItemStatus.Downloaded -> R.string.rssActionRedownload
            else -> R.string.rssActionDownload
        },
    )
    val ignorable = item.state == RssItemStatus.New
    val description = listOfNotNull(item.title, chipText, meta.ifEmpty { null }, reasonText).joinToString(", ")

    val rectBox = remember { RectBox() }
    val menu = buildList<FluxMenuItem> {
            if (hasTask) add(FluxMenuItem.Action(str(R.string.mobileRssOpenTask), onOpenTask, icon = FluxIcons.FolderOpen))
            add(FluxMenuItem.Action(downloadLabel, onDownload, icon = FluxIcons.Download, enabled = canAct && !busy))
            if (ignorable) add(FluxMenuItem.Action(str(R.string.rssActionIgnore), onIgnore, icon = FluxIcons.EyeOff, enabled = canAct && !busy))
            if (item.effectiveLink.isNotEmpty()) add(FluxMenuItem.Action(str(R.string.copyUrl), onCopy, icon = FluxIcons.Copy))
            add(FluxMenuItem.Action(str(R.string.mobileRssSelect), onSelect, icon = FluxIcons.ListChecks))
        }
    val openMenu = { overlays.showMenu(rectBox.rect, menu, header = item.title) }

    val startActions = if (canAct && !selecting) {
        listOf(FluxSwipeAction(downloadLabel, FluxIcons.Download, FluxSwipeTone.Accent, onClick = onDownload))
    } else {
        emptyList()
    }
    val endActions = if (canAct && !selecting && ignorable) {
        listOf(FluxSwipeAction(str(R.string.rssActionIgnore), FluxIcons.EyeOff, FluxSwipeTone.Warn, onClick = onIgnore))
    } else {
        emptyList()
    }
    val titleStyle = remember(t, item.state) { t.weight(t.body, if (item.state == RssItemStatus.New) 600 else 400) }

    Column {
        SwipeReveal(startActions = startActions, endActions = endActions, enabled = !selecting) {
            Row(
                Modifier
                    .fillMaxWidth()
                    .onGloballyPositioned { rectBox.rect = it.boundsInRoot() }
                    .fluxPressable(onClick = onTap, onLongClick = openMenu, role = if (selecting) Role.Checkbox else Role.Button)
                    .semantics(mergeDescendants = !expanded) {
                        if (!expanded) contentDescription = description
                        if (selecting) this.selected = selected
                    }
                    .heightIn(min = 60.dp)
                    .padding(horizontal = margin, vertical = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                verticalAlignment = Alignment.Top,
            ) {
                if (selecting) {
                    FluxCheckbox(checked = selected, onCheckedChange = null, modifier = Modifier.clearAndSetSemantics { })
                }
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    FluxText(
                        item.title,
                        style = titleStyle,
                        color = c.ink,
                        maxLines = if (expanded) Int.MAX_VALUE else 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (meta.isNotEmpty()) FluxText(meta, style = t.sm, color = c.inkMuted, maxLines = 2)
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                        ItemChip(chip, chipText)
                        if (reasonText != null) {
                            FluxText(reasonText, modifier = Modifier.weight(1f, fill = false), style = t.sm, color = c.amberText, maxLines = 2)
                        }
                    }
                    if (expanded) {
                        if (item.effectiveLink.isNotEmpty()) {
                            FluxText(item.effectiveLink, style = t.monoS, color = c.inkMuted, maxLines = 5)
                        }
                        Row(
                            Modifier.horizontalScroll(rememberScrollState()),
                            horizontalArrangement = Arrangement.spacedBy(4.dp),
                        ) {
                            if (item.effectiveLink.isNotEmpty()) {
                                DetailAction(str(R.string.copyUrl), FluxIcons.Copy, onCopy)
                            }
                            if (hasTask) DetailAction(str(R.string.mobileRssOpenTask), FluxIcons.FolderOpen, onOpenTask)
                            DetailAction(downloadLabel, FluxIcons.Download, onDownload, enabled = canAct && !busy)
                            if (ignorable) DetailAction(str(R.string.rssActionIgnore), FluxIcons.EyeOff, onIgnore, enabled = canAct && !busy)
                        }
                    }
                }
            }
        }
        FluxDivider(startInset = margin)
    }
}

@Composable
private fun DetailAction(text: String, icon: ImageVector, onClick: () -> Unit, enabled: Boolean = true) {
    FluxButton(text, onClick = onClick, variant = ButtonVariant.Ghost, size = ButtonSize.Xs, icon = icon, enabled = enabled)
}

/** 状态 chip：下载中 = 进度环 + 文案，其余 = 图标 + 文案。 */
@Composable
private fun ItemChip(chip: RssItemChip, text: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
        chip.progress?.let { ProgressRing(it) }
        FluxTag(
            text = text,
            tone = when (chip.tone) {
                RssChipTone.Neutral -> Tone.Neutral
                RssChipTone.Accent -> Tone.Accent
                RssChipTone.Success -> Tone.Mint
                RssChipTone.Warning -> Tone.Amber
                RssChipTone.Failure -> Tone.Coral
            },
            icon = if (chip.progress == null) chipIcon(chip.kind) else null,
        )
    }
}

/** 14dp 进度环（下载中的关联任务）；轨道用弱墨色，进度用强调色。 */
@Composable
private fun ProgressRing(fraction: Double) {
    val c = FluxTheme.colors
    val track = c.ink.copy(alpha = 0.18f)
    val arc = c.accentHi
    Canvas(Modifier.size(14.dp).clearAndSetSemantics { }) {
        val stroke = 2.dp.toPx()
        val topLeft = Offset(stroke / 2, stroke / 2)
        val arcSize = Size(size.width - stroke, size.height - stroke)
        drawArc(track, 0f, 360f, false, topLeft, arcSize, style = Stroke(stroke))
        drawArc(arc, -90f, (360.0 * fraction).toFloat(), false, topLeft, arcSize, style = Stroke(stroke, cap = StrokeCap.Round))
    }
}

// ── 文案 / 映射 ─────────────────────────────────────────────────────────

/** 条目元信息：`发布时间 · 日期 · 大小`；两者都未知为空串。 */
private fun metaText(item: RssItemDto, published: String): String {
    val date = if (item.pubDate > 0) {
        DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(item.pubDate * 1000))
    } else {
        ""
    }
    val size = if (item.enclosureLength > 0) Format.bytes(item.enclosureLength).toString() else ""
    return listOf(if (date.isEmpty()) "" else "$published · $date", size).filter { it.isNotEmpty() }.joinToString(" · ")
}

@StringRes
private fun chipTitle(kind: RssChipKind): Int = when (kind) {
    RssChipKind.TaskMissing -> R.string.rssTaskMissing
    RssChipKind.Pending -> R.string.statusPending
    RssChipKind.Downloading -> R.string.statusDownloading
    RssChipKind.Paused -> R.string.statusPaused
    RssChipKind.Incomplete -> R.string.statusIncomplete
    RssChipKind.Completed -> R.string.statusCompleted
    RssChipKind.Error -> R.string.statusError
    RssChipKind.Preparing -> R.string.statusPreparing
    RssChipKind.TaskCreated -> R.string.rssTaskCreated
    RssChipKind.Ignored -> R.string.rssStatusIgnored
    RssChipKind.Filtered -> R.string.rssStatusFiltered
    RssChipKind.Duplicate -> R.string.rssStatusDuplicate
    RssChipKind.History -> R.string.rssStatusHistory
    RssChipKind.New -> R.string.rssStatusNew
}

private fun chipIcon(kind: RssChipKind): ImageVector = when (kind) {
    RssChipKind.TaskMissing -> FluxIcons.CircleAlert
    RssChipKind.Pending -> FluxIcons.Clock
    RssChipKind.Downloading -> FluxIcons.CircleArrowDown
    RssChipKind.Paused -> FluxIcons.CirclePause
    RssChipKind.Incomplete -> FluxIcons.TriangleAlert
    RssChipKind.Completed -> FluxIcons.CircleCheck
    RssChipKind.Error -> FluxIcons.CircleX
    RssChipKind.Preparing -> FluxIcons.Hourglass
    RssChipKind.TaskCreated -> FluxIcons.Inbox
    RssChipKind.Ignored -> FluxIcons.EyeOff
    RssChipKind.Filtered -> FluxIcons.ListFilter
    RssChipKind.Duplicate -> FluxIcons.Copy
    RssChipKind.History -> FluxIcons.History
    RssChipKind.New -> FluxIcons.Circle
}

/** 原因码 → 文案；无文案（空 / `seed_skipped` / 未知码）为 null。 */
@StringRes
private fun reasonRes(reason: RssReason): Int? = when (reason) {
    RssReason.NotIncluded -> R.string.rssReasonNotIncluded
    RssReason.Excluded -> R.string.rssReasonExcluded
    RssReason.TooSmall -> R.string.rssReasonTooSmall
    RssReason.TooLarge -> R.string.rssReasonTooLarge
    RssReason.DupEpisode -> R.string.rssReasonDupEpisode
    RssReason.TorrentFetchFailed -> R.string.rssReasonTorrentFetchFailed
    RssReason.SeedSkipped, RssReason.None, RssReason.Unknown -> null
}
