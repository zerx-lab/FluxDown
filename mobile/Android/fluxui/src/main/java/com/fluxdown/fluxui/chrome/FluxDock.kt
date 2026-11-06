package com.fluxdown.fluxui.chrome

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.AnimationVector1D
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.sizeIn
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.Placeable
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.isTraversalGroup
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.theme.FluxScaleGroup
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.launch

/** 导航坞徽标：计数或红点。 */
@Immutable
sealed interface FluxDockBadge {
    /** 计数徽标（> 99 显示 `99+`；≤ 0 不显示）。 */
    @Immutable
    data class Count(val n: Int) : FluxDockBadge

    /** 8dp 珊瑚红点。 */
    @Immutable
    data object Dot : FluxDockBadge
}

/**
 * 导航坞目的地。[badgeDescription] 会并入无障碍描述（如“3 条未读”），徽标本身不单独朗读。
 */
@Immutable
data class FluxDockItem(
    val icon: ImageVector,
    val label: String,
    val badge: FluxDockBadge? = null,
    val badgeDescription: String? = null,
)

private val DockPad = 4.dp
private val DockIcon = 24.dp
private val DockLabelGap = 2.dp

/**
 * 浮动导航坞：常驻的毛玻璃胶囊（[floatingChromeSurface]：真模糊 + 顶沿高光 + 向下软投影），不随滚动收起。
 * 尺寸对齐 iOS 26 标签栏：高 62、内边距 4、图标 24、标签 10sp；各项等宽、图标在上标签在下；
 * 选中项由 accentLo 胶囊托底，图标与标签转为 accentHi，胶囊随选中弹簧滑动。
 *
 * 本组件在父容器内**占满宽度、固定为主题 dockHeight 高**；坞本体宽 = 容器宽 − [reservedEnd]（球位 + dockOrbGap）。
 * 由应用负责摆放，系统导航栏上方留 8dp。
 *
 * - [hidden]：推入页 / Expanded 隐藏（`alpha 0、translateY 30dp`，退场结束后移出组合）。
 * - [selectionMode]：多选模式，坞淡出（`translateY 18、scale .94`），由 [SelectionDock] 接替。
 * - 大字体下任一标签放不下时整坞只显示图标；标签过宽按项宽省略。
 * - 点击不同项触发 tick 触感；点击当前项同样回调 [onSelect]（应用可据此回顶）。
 */
@Composable
fun FluxDock(
    items: List<FluxDockItem>,
    selected: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
    hidden: Boolean = false,
    selectionMode: Boolean = false,
    reservedEnd: Dp = FluxTheme.space.dockHeight + FluxTheme.space.dockOrbGap,
    contentDescription: String = "主导航",
) {
    if (items.isEmpty()) return
    val motion = FluxTheme.motion
    val hideP = rememberChromePresence(!hidden, motion.fluid)
    val selP = rememberChromePresence(!selectionMode, motion.liquid)
    BoxWithConstraints(
        modifier
            .fillMaxWidth()
            .height(FluxTheme.space.dockHeight),
        contentAlignment = Alignment.CenterStart,
    ) {
        if (hideP.composed && selP.composed) {
            DockBody(
                items = items,
                selected = selected.coerceIn(0, items.lastIndex),
                onSelect = onSelect,
                interactive = !hidden && !selectionMode,
                hideP = hideP,
                selP = selP,
                reservedEnd = reservedEnd,
                groupDescription = contentDescription,
            )
        }
    }
}

@Composable
private fun DockBody(
    items: List<FluxDockItem>,
    selected: Int,
    onSelect: (Int) -> Unit,
    interactive: Boolean,
    hideP: ChromePresence,
    selP: ChromePresence,
    reservedEnd: Dp,
    groupDescription: String,
) {
    val type = FluxTheme.type
    val motion = FluxTheme.motion
    val haptics = FluxTheme.haptics
    val density = LocalDensity.current
    val dockHeight = FluxTheme.space.dockHeight
    val itemHeight = dockHeight - DockPad * 2
    val n = items.size

    val labelStyle = remember(type) { dockLabelStyle(type.sized(type.weight(type.sm, 500), 10f, FluxScaleGroup.Ks)) }
    val selectedLabelStyle = remember(type) { dockLabelStyle(type.sized(type.weight(type.sm, 600), 10f, FluxScaleGroup.Ks)) }
    val measurer = rememberTextMeasurer()
    val showLabels = remember(items, selectedLabelStyle, density, measurer, itemHeight) {
        val heights = IntArray(items.size) {
            measurer.measure(items[it].label, selectedLabelStyle, softWrap = false, maxLines = 1).size.height
        }
        with(density) { dockLabelsFit(itemHeight.roundToPx(), DockIcon.roundToPx(), DockLabelGap.roundToPx(), heights) }
    }
    val selA = remember(n) { List(n) { Animatable(if (it == selected) 1f else 0f) } }
    val geometry = remember(n) { DockGeometry(n) }
    val selectionWeights = remember(n) { FloatArray(n) }
    val placeables = remember(n) { arrayOfNulls<Placeable>(n) }
    LaunchedEffect(selected, n) {
        selA.forEachIndexed { i, a -> launch { a.animateTo(if (i == selected) 1f else 0f, motion.of(motion.liquid)) } }
    }

    val shape = CircleShape
    Box(
        Modifier
            .layout { measurable, c ->
                val h = dockHeight.roundToPx()
                val p = measurable.measure(Constraints.fixed(dockWidth(c.maxWidth, reservedEnd.roundToPx(), h), h))
                layout(p.width, p.height) { p.place(0, 0) }
            }
            .chromeFade(translateY = 30.dp) { hideP.progress.value }
            .chromeFade(translateY = 18.dp, scaleFrom = 0.94f) { selP.progress.value }
            .floatingChromeSurface(shape)
            .semantics {
                isTraversalGroup = true
                contentDescription = groupDescription
            },
    ) {
        Layout(
            content = {
                DockIndicator()
                items.forEachIndexed { i, item ->
                    DockItemView(
                        item = item,
                        isSelected = i == selected,
                        sel = selA[i],
                        labelStyle = labelStyle,
                        selectedLabelStyle = selectedLabelStyle,
                        showLabel = showLabels,
                        enabled = interactive,
                        onClick = {
                            if (i != selected) haptics.tick()
                            onSelect(i)
                        },
                    )
                }
            },
            modifier = Modifier.fillMaxSize(),
        ) { measurables, constraints ->
            val pad = DockPad.roundToPx()
            val itemH = (constraints.maxHeight - pad * 2).coerceAtLeast(0)
            for (i in items.indices) selectionWeights[i] = selA[i].value
            geometry.update((constraints.maxWidth - pad * 2).coerceAtLeast(0), selectionWeights, selected)
            val indPlaceable = measurables[0].measure(Constraints.fixed(geometry.indicatorWidth, itemH))
            for (i in items.indices) {
                placeables[i] = measurables[i + 1].measure(Constraints.fixed(geometry.widths[i], itemH))
            }
            val rtl = layoutDirection == LayoutDirection.Rtl
            layout(constraints.maxWidth, constraints.maxHeight) {
                indPlaceable.place(pad + geometry.indicatorLeft(rtl), pad)
                for (i in items.indices) placeables[i]?.place(pad + geometry.left(i, rtl), pad)
            }
        }
    }
}

private fun dockLabelStyle(base: TextStyle): TextStyle =
    base.copy(lineHeight = 1.2.em, letterSpacing = 0.01.em, textAlign = TextAlign.Center)

/** 选中胶囊：`accentLo` 纯底，与坞同心（无描边、无渐变、无辉光）。 */
@Composable
private fun DockIndicator() {
    Box(Modifier.background(FluxTheme.colors.accentLo, CircleShape))
}

@Composable
private fun DockItemView(
    item: FluxDockItem,
    isSelected: Boolean,
    sel: Animatable<Float, AnimationVector1D>,
    labelStyle: TextStyle,
    selectedLabelStyle: TextStyle,
    showLabel: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    val c = FluxTheme.colors
    val motion = FluxTheme.motion
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val pressScale by animateFloatAsState(if (pressed) 0.92f else 1f, motion.of(motion.press), label = "dockPress")
    val description = item.badgeDescription?.let { "${item.label}，$it" } ?: item.label
    Box(
        Modifier
            .selectable(
                selected = isSelected,
                interactionSource = interaction,
                indication = null,
                enabled = enabled,
                role = Role.Tab,
                onClick = onClick,
            )
            .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .padding(horizontal = 4.dp)
                .graphicsLayer {
                    scaleX = pressScale
                    scaleY = pressScale
                }
                .clearAndSetSemantics { },
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(DockLabelGap),
        ) {
            Box(Modifier.size(DockIcon)) {
                FluxIcon(
                    item.icon, null,
                    Modifier.graphicsLayer { alpha = 1f - sel.value.coerceIn(0f, 1f) },
                    size = DockIcon,
                    tint = c.ink,
                )
                FluxIcon(
                    item.icon, null,
                    Modifier.graphicsLayer { alpha = sel.value.coerceIn(0f, 1f) },
                    size = DockIcon,
                    tint = c.accentHi,
                )
                val badge = item.badge
                if (badge != null && !(badge is FluxDockBadge.Count && badge.n <= 0)) {
                    DockBadgeView(badge)
                }
            }
            if (showLabel) {
                Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.TopCenter) {
                    DockLabel(item.label, labelStyle, c.ink) { 1f - sel.value.coerceIn(0f, 1f) }
                    DockLabel(item.label, selectedLabelStyle, c.accentHi) { sel.value.coerceIn(0f, 1f) }
                }
            }
        }
    }
}

@Composable
private fun DockLabel(text: String, style: TextStyle, color: Color, alpha: () -> Float) {
    FluxText(
        text,
        modifier = Modifier.fillMaxWidth().graphicsLayer { this.alpha = alpha() },
        style = style,
        color = color,
        maxLines = 1,
        softWrap = false,
        overflow = TextOverflow.Ellipsis,
    )
}

/** 徽标压在图标右上角，带一圈 `glassSolid4` 描边（与玻璃坞面同色系）与图标分开。 */
@Composable
private fun BoxScope.DockBadgeView(badge: FluxDockBadge) {
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val ring = 1.5.dp
    when (badge) {
        is FluxDockBadge.Count -> {
            val style = remember(type) {
                type.sized(type.weight(type.monoS, 600, mono = true), 10f, FluxScaleGroup.Ks)
            }
            Box(
                Modifier
                    .align(Alignment.TopEnd)
                    .offset(x = 12.dp, y = (-6).dp)
                    .sizeIn(minWidth = 16.dp + ring * 2, minHeight = 16.dp + ring * 2)
                    .clip(RoundedCornerShape(9.5.dp))
                    .background(c.accent)
                    .border(ring, c.glassSolid4, RoundedCornerShape(9.5.dp))
                    .padding(horizontal = 4.dp + ring)
                    .clearAndSetSemantics { },
                contentAlignment = Alignment.Center,
            ) {
                FluxText(
                    if (badge.n > 99) "99+" else badge.n.toString(),
                    style = style,
                    color = c.onAccent,
                    maxLines = 1,
                    softWrap = false,
                )
            }
        }
        FluxDockBadge.Dot -> Box(
            Modifier
                .align(Alignment.TopEnd)
                .offset(x = 4.dp, y = (-2).dp)
                .size(8.dp + ring * 2)
                .clip(CircleShape)
                .background(c.coral)
                .border(ring, c.glassSolid4, CircleShape)
                .clearAndSetSemantics { },
        )
    }
}
