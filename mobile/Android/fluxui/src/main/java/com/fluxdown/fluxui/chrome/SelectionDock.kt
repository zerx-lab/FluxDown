package com.fluxdown.fluxui.chrome

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.isTraversalGroup
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.theme.FluxScaleGroup
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.fluxPressable

/** 选择坞的一个批量动作。[danger] 用 `coralText` 着色；[enabled] = false 时 α .32。 */
@Immutable
data class SelectionDockAction(
    val id: String,
    val label: String,
    val icon: ImageVector,
    val onClick: () -> Unit,
    val danger: Boolean = false,
    val enabled: Boolean = true,
)

/** 坞面到动作按钮的内边距（iOS 底部工具栏 5）：62 高的坞里按钮高 52。 */
private val SelectionInset = 5.dp
private val ActionHeight = 52.dp
private val ActionMinWidth = 48.dp

/**
 * 选择坞（01 §12.4）：多选模式下接替 [FluxDock] 的批量操作条，与坞同一毛玻璃表面（[floatingChromeSurface]）。
 *
 * 布局同 [FluxDock]：占满父容器宽度、高为主题 dockHeight，本体宽 = 容器宽 − [reservedEnd]（右侧留给 [FluxOrb] 的“退出选择”球）；
 * Expanded 档浮在列表区底部居中时传 `reservedEnd = 0.dp` 并给 `modifier` 加 `widthIn(max = 560.dp)`。
 *
 * 坞里只有动作，与导航坞同一节奏（各项图标在上、标签在下，放得下时等分，见 [selectionSlotWidths]，放不下时横向滚动）；
 * 「已选 N 项」由页头显示（同 iOS 选择模式的导航栏标题），坞不再放计数。
 * - 第一项是全选：复选框图标表达状态（未全选 [FluxIcons.Square] → 已全选 [FluxIcons.SquareCheck]，accentHi），
 *   标签 [selectAllLabel] / [deselectAllLabel]，点按 → [onToggleAll]；`stateDescription` = [allSelectedDescription] / [notAllSelectedDescription]。
 * - 动作按钮高 52、最小宽 48（图标 22 + 10sp 标签），`fontScale ≥ 1.3` 只显示图标（描述保留）。
 * - a11y：坞 `stateDescription` = [countDescription]（`liveRegion = Polite`，增减时播报）。
 * - 出入场：`translateY 18→0、scale .94→1`（`liquid`）。
 */
@Composable
fun SelectionDock(
    count: Int,
    total: Int,
    onToggleAll: () -> Unit,
    actions: List<SelectionDockAction>,
    visible: Boolean,
    modifier: Modifier = Modifier,
    reservedEnd: Dp = FluxTheme.space.dockHeight + FluxTheme.space.dockOrbGap,
    selectAllLabel: String = "全选",
    deselectAllLabel: String = "取消全选",
    countDescription: (Int) -> String = { "已选 $it 项" },
    allSelectedDescription: String = "已全选",
    notAllSelectedDescription: String = "未全选",
    contentDescription: String = "批量操作",
) {
    val motion = FluxTheme.motion
    val presence = rememberChromePresence(visible, motion.liquid)
    val dockHeight = FluxTheme.space.dockHeight
    Box(modifier.fillMaxWidth().height(dockHeight)) {
        if (!presence.composed) return@Box
        val c = FluxTheme.colors
        val type = FluxTheme.type
        val iconOnly = type.fontScale >= 1.3f
        val allSelected = total > 0 && count >= total
        Row(
            Modifier
                .padding(end = reservedEnd)
                .fillMaxWidth()
                .height(dockHeight)
                .chromeFade(translateY = 18.dp, scaleFrom = 0.94f) { presence.progress.value }
                .floatingChromeSurface(CircleShape)
                .clip(CircleShape)
                .semantics {
                    isTraversalGroup = true
                    liveRegion = LiveRegionMode.Polite
                    this.contentDescription = contentDescription
                    stateDescription = countDescription(count)
                }
                .padding(horizontal = SelectionInset),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            BoxWithConstraints(Modifier.weight(1f).fillMaxHeight()) {
                val available = constraints.maxWidth
                val slots = actions.size + 1
                val natural = remember(slots) { IntArray(slots) }
                val widths = remember(slots) { IntArray(slots) }
                Layout(
                    content = {
                        SelectionActionButton(
                            label = if (allSelected) deselectAllLabel else selectAllLabel,
                            icon = if (allSelected) FluxIcons.SquareCheck else FluxIcons.Square,
                            onClick = onToggleAll,
                            tint = if (allSelected) c.accentHi else c.ink,
                            iconOnly = iconOnly,
                            enabled = visible,
                            stateDescription = if (allSelected) allSelectedDescription else notAllSelectedDescription,
                        )
                        actions.forEach { a ->
                            SelectionActionButton(
                                label = a.label,
                                icon = a.icon,
                                onClick = a.onClick,
                                tint = if (a.danger) c.coralText else c.ink,
                                iconOnly = iconOnly,
                                enabled = a.enabled && visible,
                                dimmed = !a.enabled,
                            )
                        }
                    },
                    modifier = Modifier.fillMaxHeight().horizontalScroll(rememberScrollState()),
                ) { measurables, constraints ->
                    val h = constraints.maxHeight
                    measurables.forEachIndexed { i, m -> natural[i] = m.maxIntrinsicWidth(h) }
                    val laidWidth = selectionSlotWidths(available, natural, widths)
                    val placeables = measurables.mapIndexed { i, m ->
                        m.measure(Constraints(minWidth = widths[i], maxWidth = widths[i], maxHeight = h))
                    }
                    layout(laidWidth, h) {
                        var x = 0
                        placeables.forEach { p ->
                            p.placeRelative(x, (h - p.height) / 2)
                            x += p.width
                        }
                    }
                }
            }
        }
    }
}

/**
 * 动作按钮：命中区为整个槽位，内容居中；无底色，按压缩放。
 * [dimmed] 只管视觉（α .32），可点与否由 [enabled] 决定。
 */
@Composable
private fun SelectionActionButton(
    label: String,
    icon: ImageVector,
    onClick: () -> Unit,
    tint: Color,
    iconOnly: Boolean,
    enabled: Boolean,
    dimmed: Boolean = false,
    stateDescription: String? = null,
) {
    val type = FluxTheme.type
    val style = remember(type) {
        type.sized(type.weight(type.micro, 500), 10f, FluxScaleGroup.Ks).copy(letterSpacing = 0.02.em)
    }
    Box(
        Modifier
            .height(ActionHeight)
            .graphicsLayer { alpha = if (dimmed) 0.32f else 1f }
            .clip(CircleShape)
            .fluxPressable(onClick = onClick, scale = 0.9f, enabled = enabled, role = Role.Button)
            .semantics {
                contentDescription = label
                if (stateDescription != null) this.stateDescription = stateDescription
            },
        contentAlignment = Alignment.Center,
    ) {
        Column(
            Modifier
                .widthIn(min = ActionMinWidth)
                .padding(horizontal = 6.dp)
                .clearAndSetSemantics { },
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(3.dp, Alignment.CenterVertically),
        ) {
            FluxIcon(icon, null, size = 22.dp, tint = tint)
            if (!iconOnly) {
                FluxText(
                    label,
                    style = style,
                    color = tint,
                    maxLines = 1,
                    softWrap = false,
                    overflow = TextOverflow.Clip,
                )
            }
        }
    }
}
