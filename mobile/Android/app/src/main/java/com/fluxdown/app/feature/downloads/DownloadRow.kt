package com.fluxdown.app.feature.downloads

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.data.CardField
import com.fluxdown.app.data.Density
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.Route
import com.fluxdown.app.ui.TaskVisualState
import com.fluxdown.app.ui.fileCategory
import com.fluxdown.app.ui.ringKind
import com.fluxdown.app.ui.tileIcon
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.fluxui.data.TaskRow
import com.fluxdown.fluxui.data.TaskRowDensity
import com.fluxdown.fluxui.data.TaskRowSelection
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.FlowInGate
import com.fluxdown.fluxui.material.fluxFlowIn
import com.fluxdown.fluxui.overlay.FluxMenuItem
import com.fluxdown.fluxui.overlay.FluxSwipeAction
import com.fluxdown.fluxui.overlay.FluxSwipeTone
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.overlay.SwipeReveal
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.FluxWindowClass

/** 行样式（视图偏好）。独立不可变类，供行组合项稳定跳过。 */
@Immutable
internal data class RowStyle(val density: Density, val fields: Set<CardField>)

/** 玻璃分区的一段（r24）：整组由首 / 中 / 尾段拼成一张卡，段间不重叠（避免半透明叠加）。 */
@Composable
internal fun Modifier.cardSegment(first: Boolean, last: Boolean): Modifier {
    val c = FluxTheme.colors
    return drawBehind { drawCardSegment(c.glass1, c.hairline, first, last) }
}

private fun DrawScope.drawCardSegment(fill: Color, line: Color, first: Boolean, last: Boolean) {
    val r = 24.dp.toPx()
    val hw = 0.5.dp.toPx()
    val top = if (first) 0f else -2f * r
    val bottom = if (last) size.height else size.height + 2f * r
    clipRect(0f, 0f, size.width, size.height) {
        val corner = CornerRadius(r)
        drawRoundRect(fill, Offset(0f, top), Size(size.width, bottom - top), corner)
        drawRoundRect(
            line,
            Offset(hw / 2f, top + hw / 2f),
            Size(size.width - hw, bottom - top - hw),
            corner,
            style = Stroke(hw),
        )
    }
}

/** [localHost] = false 时已完成行的主动作是进入任务详情（远端文件不在本机）。 */
private fun ringLabelOf(t: RowText, v: TaskVisualState, localHost: Boolean): String = when (v) {
    TaskVisualState.Downloading, TaskVisualState.Pending, TaskVisualState.Queued,
    TaskVisualState.Preparing, TaskVisualState.Verifying,
    -> t.ringPause
    TaskVisualState.Paused -> t.ringResume
    TaskVisualState.Failed -> t.ringRetry
    TaskVisualState.Missing -> t.ringRedownload
    TaskVisualState.Seeding -> t.ringSeeding
    TaskVisualState.Completed -> if (localHost) t.ringOpen else t.openDetails
}

private fun primaryIconOf(v: TaskVisualState): ImageVector = when (v) {
    TaskVisualState.Downloading, TaskVisualState.Pending, TaskVisualState.Queued,
    TaskVisualState.Preparing, TaskVisualState.Verifying,
    -> FluxIcons.Pause
    TaskVisualState.Paused -> FluxIcons.Play
    TaskVisualState.Failed, TaskVisualState.Missing -> FluxIcons.RotateCw
    TaskVisualState.Seeding, TaskVisualState.Completed -> FluxIcons.ExternalLink
}

/**
 * 任务行：TaskRow + 滑动动作 + 分区背景。只在自己的 [entry]（任务 / 运行时 / 速度）或
 * 选择状态变化时重组；选择成员关系经 derivedStateOf 读取，批量勾选不会重组其他行。
 */
@Composable
internal fun DownloadRow(
    entry: RowEntry,
    style: RowStyle,
    flowIndex: Int,
    gate: FlowInGate,
    a11y: Boolean,
) {
    val item = entry.item
    val id = item.id
    val nav = LocalNavigator.current
    val actions = LocalTaskActions.current

    val overlays = LocalFluxOverlays.current
    val view = LocalDownloadsView.current
    val haptics = FluxTheme.haptics
    val colors = FluxTheme.colors
    val text = LocalRowText.current
    val margin = FluxTheme.space.screenMargin
    val compact = style.density == Density.Compact
    val latest by rememberUpdatedState(item)

    // 排队名次在组合里按稳定键二分取（只读 queueOrder 的可见排队行随顺序变化重组）；非排队行不读它。
    val queuePosition = if (item.visual == TaskVisualState.Queued) view.queueOrder.rank(id) else 0
    val meta = remember(item, style.fields, colors, text, queuePosition) {
        buildRowMeta(colors, text, item, style.fields, System.currentTimeMillis() / 1000, queuePosition)
    }

    val selecting = nav.selecting
    val selected by remember(id, nav) { derivedStateOf { id in nav.selection } }
    val selection = when {
        !selecting -> TaskRowSelection.None
        selected -> TaskRowSelection.Selected
        else -> TaskRowSelection.Unselected
    }

    // 待选 fileExists：只读自己 id 的成员关系，集合变化时仅对应行重组
    val conflict by remember(id, view) { derivedStateOf { id in view.conflictTaskIds } }
    val openConflicts = remember(nav, haptics) {
        {
            haptics.tick()
            nav.reopenFileConflicts()
        }
    }

    val paned = FluxTheme.windowClass != FluxWindowClass.Compact
    val inPane = remember(id, nav, paned) { derivedStateOf { paned && (nav.top as? Route.TaskDetail)?.taskId == id } }

    val coords = remember { arrayOfNulls<LayoutCoordinates>(1) }

    val localHost = view.localHost
    val startActions = remember(item.visual, item.boosted, text, actions, localHost) {
        buildList {
            add(
                FluxSwipeAction(ringLabelOf(text, item.visual, localHost), primaryIconOf(item.visual), FluxSwipeTone.Accent) {
                    actions.primary(latest.task)
                },
            )
            val st = item.task.status
            if (st == TaskStatus.Downloading || st == TaskStatus.Pending) {
                add(
                    FluxSwipeAction(if (item.boosted) text.cancelBoost else text.boost, FluxIcons.Zap, FluxSwipeTone.Warn) {
                        actions.boost(latest.task, latest.boosted)
                    },
                )
            }
        }
    }
    val endActions = remember(text, actions) {
        listOf(
            FluxSwipeAction(text.copyLink, FluxIcons.Copy, FluxSwipeTone.Neutral, showLabel = false) { actions.copyLink(latest.task) },
            FluxSwipeAction(text.delete, FluxIcons.Trash2, FluxSwipeTone.Danger, showLabel = false) { actions.confirmDelete(listOf(latest.task)) },
        )
    }

    val customActions = if (a11y) {
        remember(item, text, actions, conflict) {
            val menu = actions.menuItems(item.task, item.boosted) { nav.enterSelection(id) }
                .filterIsInstance<FluxMenuItem.Action>()
                .map { a -> CustomAccessibilityAction(a.label) { a.onClick(); true } }
            if (conflict) menu + CustomAccessibilityAction(text.conflictTooltip) { nav.reopenFileConflicts(); true } else menu
        }
    } else {
        emptyList()
    }

    val hPad: Dp = if (entry.zone == RowZone.History) 20.dp else 18.dp
    val inset = if (compact) 60.dp else 68.dp
    val frame = if (entry.zone == RowZone.History) Modifier else Modifier.cardSegment(entry.first, entry.last)

    Box(
        Modifier
            .fillMaxWidth()
            .then(if (flowIndex in 0..11) Modifier.fluxFlowIn(flowIndex, gate, id) else Modifier)
            .then(if (entry.zone == RowZone.History) Modifier else Modifier.padding(horizontal = margin)),
    ) {
        Box(
            Modifier
                .fillMaxWidth()
                .then(frame)
                .rowDecor(entry.zone, entry.first, entry.last, inset, inPane)
                .onGloballyPositioned { coords[0] = it },
        ) {
            SwipeReveal(
                startActions = startActions,
                endActions = endActions,
                enabled = !selecting && !view.readOnly,
            ) {
                TaskRow(
                    fileName = item.task.fileName,
                    meta = meta,
                    tileIcon = item.category.tileIcon(item.task),
                    categoryColor = colors.category(item.category.fileCategory()),
                    ringKind = item.visual.ringKind(),
                    ringProgress = ringProgressOf(item),
                    ringContentDescription = ringLabelOf(text, item.visual, localHost),
                    onClick = {
                        if (nav.selecting) {
                            nav.toggleSelected(id)
                            haptics.tick()
                        } else {
                            nav.push(Route.TaskDetail(id))
                        }
                    },
                    onLongClick = {
                        val c = coords[0]
                        val pickingMenu = if (nav.selecting && id in nav.selection && nav.selection.size > 1) {
                            actions.btBatchMenuItems(nav.selection)
                        } else {
                            emptyList()
                        }
                        if (pickingMenu.isNotEmpty() && c != null && c.isAttached) {
                            overlays.showMenu(c.boundsInRoot(), pickingMenu)
                        } else if (nav.selecting) {
                            nav.toggleSelected(id)
                        } else if (c != null && c.isAttached) {
                            val cur = latest
                            overlays.showMenu(c.boundsInRoot(), actions.menuItems(cur.task, cur.boosted) { nav.enterSelection(id) })
                        }
                    },
                    onRingClick = { actions.primary(latest.task) },
                    onTileClick = {
                        if (nav.selecting) {
                            nav.toggleSelected(id)
                            haptics.tick()
                        } else {
                            haptics.longPress()
                            nav.enterSelection(id)
                        }
                    },
                    flowSegments = item.flow,
                    flowState = item.flowState,
                    density = if (compact) TaskRowDensity.Compact else TaskRowDensity.Comfortable,
                    selection = selection,
                    strikethrough = item.visual == TaskVisualState.Missing,
                    horizontalPadding = hPad,
                    onClickLabel = text.openDetails,
                    customActions = customActions,
                    badge = if (conflict) text.conflictPending else null,
                    onBadgeClick = if (conflict) openConflicts else null,
                )
            }
        }
    }
}

private fun ringProgressOf(item: TaskItem): Float? = when (item.visual) {
    TaskVisualState.Completed, TaskVisualState.Missing, TaskVisualState.Seeding,
    TaskVisualState.Preparing, TaskVisualState.Verifying,
    -> null
    else -> item.task.progress
}

/** 详情栏当前任务底色（draw 阶段读取，不触发重组）+ 行间发丝线（历史区首行另画顶线）。 */
@Composable
internal fun Modifier.rowDecor(
    zone: RowZone,
    first: Boolean,
    last: Boolean,
    inset: Dp,
    inPane: androidx.compose.runtime.State<Boolean>,
): Modifier {
    val c = FluxTheme.colors
    return drawBehind {
        if (inPane.value) drawRect(c.accentLo)
        val hw = 0.5.dp.toPx()
        val x = if (zone == RowZone.History && last) 0f else inset.toPx()
        if (zone == RowZone.History && first) {
            drawLine(c.hairline, Offset(0f, hw / 2f), Offset(size.width, hw / 2f), hw)
        }
        if (!last || zone == RowZone.History) {
            drawLine(c.hairline, Offset(x, size.height - hw / 2f), Offset(size.width, size.height - hw / 2f), hw)
        }
    }
}
