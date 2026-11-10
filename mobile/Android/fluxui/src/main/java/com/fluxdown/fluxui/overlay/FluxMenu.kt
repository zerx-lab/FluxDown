package com.fluxdown.fluxui.overlay

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.TransformOrigin
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.toggleableState
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.dp
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.FluxBlur
import com.fluxdown.fluxui.material.FluxGlass
import com.fluxdown.fluxui.material.fluxGlass
import com.fluxdown.fluxui.material.fluxGlow
import com.fluxdown.fluxui.material.softShadow
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.fluxPressable
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/** 菜单条目：动作 / 分组头 / 分隔线（沿用 PC `MenuEntry` 的顺序语义）。 */
@Immutable
sealed interface FluxMenuItem {
    /**
     * @param hint 标签下方的次要说明（sm inkMuted）
     * @param hintIsPath [hint] 是路径：单行中间省略，保留开头与末段目录名
     * @param trailing 行尾等宽小字（如快捷键）
     * @param destructive 危险动作（coral 文字 / 图标）
     * @param keepOpen 点击后不关闭菜单
     * @param indent 缩进 38dp（子项）
     * @param showTrailingCheck 为 false 时不画行尾勾。勾选态仍由 [checked] 暴露：默认 `Role.Checkbox` + `toggleableState`（行首图标已是方框）；[radio] 为 true 时改为 `Role.RadioButton` + `selected`（行首图标已是圆，两态都画）。
     * @param radio 单选行。不画行尾勾；选中态走 `selected`，未选中也显式为 false。
     */
    @Immutable
    data class Action(
        val label: String,
        val onClick: () -> Unit,
        val icon: ImageVector? = null,
        val hint: String? = null,
        val trailing: String? = null,
        val checked: Boolean = false,
        val destructive: Boolean = false,
        val enabled: Boolean = true,
        val keepOpen: Boolean = false,
        val indent: Boolean = false,
        val hintIsPath: Boolean = false,
        val showTrailingCheck: Boolean = true,
        val radio: Boolean = false,
    ) : FluxMenuItem

    @Immutable
    data class Header(val label: String) : FluxMenuItem

    @Immutable
    data object Divider : FluxMenuItem
}

/** 强制对齐锚点的哪一侧；null = 锚点中心在屏幕右半则 End。 */
enum class FluxMenuAlign { Start, End }

internal class MenuEntry(
    val id: Long,
    val anchor: Rect,
    val items: List<FluxMenuItem>,
    val header: String?,
    val align: FluxMenuAlign?,
)

/**
 * z80。烟晶菜单（Real / Thick / menuBg）：自锚点最近角缩放 .82→1 + 模糊 6→0（fluid）；
 * 定位：`y = anchor.bottom + 8`，放不下翻到上方；水平夹取 [10, W−w−10]；max-height 70% 可滚。
 */
@Composable
internal fun MenuLayer(state: FluxOverlayState) {
    val entry = state.menuEntry
    val motion = FluxTheme.motion
    val prog = rememberOverlayProgress()
    val last = remember { Last<MenuEntry>() }
    if (entry != null) last.value = entry

    LaunchedEffect(entry) {
        if (entry != null) prog.enter(motion.of(motion.fluid), restartAt = 0f) else prog.exit(motion.of(motion.snap))
    }
    BackHandler(enabled = entry != null) { state.dismissMenu() }

    val shown = last.value
    if (shown == null || !prog.present) return

    val colors = FluxTheme.colors
    val status = WindowInsets.statusBars
    val nav = WindowInsets.navigationBars
    val hostOrigin = remember { FloatArray(2) }
    val pivot = remember { floatArrayOf(0f, 0f) }
    val shape = FluxTheme.shapes.menu

    Box(
        Modifier
            .fillMaxSize()
            .onGloballyPositioned {
                val p = it.positionInRoot()
                hostOrigin[0] = p.x
                hostOrigin[1] = p.y
            }
            .then(
                if (entry != null) {
                    Modifier.pointerInput(entry.id) { detectTapGestures { state.dismissMenu() } }
                } else {
                    Modifier
                },
            ),
    ) {
        Column(
            Modifier
                .layout { measurable, constraints ->
                    val w = constraints.maxWidth
                    val h = constraints.maxHeight
                    val mg = 10.dp.roundToPx()
                    val gap = 8.dp.roundToPx()
                    val maxW = max(0, min(300.dp.roundToPx(), w - 2 * mg))
                    val minW = min(216.dp.roundToPx(), maxW)
                    val topLimit = status.getTop(this) + mg
                    val bottomLimit = h - nav.getBottom(this) - mg
                    val p = measurable.measure(Constraints(minW, maxW, 0, (h * 0.7f).toInt()))

                    val a = shown.anchor
                    val left = a.left - hostOrigin[0]
                    val right = a.right - hostOrigin[0]
                    val top = a.top - hostOrigin[1]
                    val bottom = a.bottom - hostOrigin[1]
                    val alignEnd = shown.align?.let { it == FluxMenuAlign.End } ?: ((left + right) / 2f > w / 2f)
                    val x = (if (alignEnd) right - p.width else left)
                        .coerceIn(mg.toFloat(), max(mg, w - p.width - mg).toFloat())
                    var y = bottom + gap
                    if (y + p.height > bottomLimit) {
                        y = top - p.height - gap
                        if (y < topLimit) y = max(topLimit, bottomLimit - p.height).toFloat()
                    }
                    val anchorX = if (alignEnd) right else left
                    pivot[0] = ((anchorX - x) / p.width.coerceAtLeast(1)).coerceIn(0f, 1f)
                    pivot[1] = if (y < top) 1f else 0f
                    layout(w, h) { p.place(x.roundToInt(), y.roundToInt()) }
                }
                .graphicsLayer {
                    val v = prog.value
                    alpha = v.coerceIn(0f, 1f)
                    val s = 0.82f + 0.18f * v
                    scaleX = s
                    scaleY = s
                    transformOrigin = TransformOrigin(pivot[0], pivot[1])
                }
                .fluxGlow(colors.softShadow(0.5f), 25.dp, shape, spread = (-10).dp, dy = 20.dp)
                .fluxGlass(FluxGlass.Menu, shape, FluxBlur.Thick)
                .swallowTaps()
                .semantics { paneTitle = shown.header ?: "菜单" }
                .verticalScroll(rememberScrollState())
                .padding(6.dp),
        ) {
            shown.header?.let { MenuHeader(it) }
            shown.items.forEach { item ->
                when (item) {
                    is FluxMenuItem.Header -> MenuHeader(item.label)
                    FluxMenuItem.Divider -> Box(
                        Modifier
                            .fillMaxWidth()
                            .padding(horizontal = 8.dp, vertical = 6.dp)
                            .height(0.5.dp)
                            .background(colors.hairline),
                    )
                    is FluxMenuItem.Action -> MenuRow(item, state)
                }
            }

        }
    }
}

@Composable
private fun MenuHeader(label: String) {
    FluxText(
        label,
        modifier = Modifier.padding(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 6.dp),
        style = FluxTheme.type.micro,
        color = FluxTheme.colors.inkMuted,
        maxLines = 1,
    )
}

@Composable
private fun MenuRow(item: FluxMenuItem.Action, state: FluxOverlayState) {
    val colors = FluxTheme.colors
    val type = FluxTheme.type
    val haptics = FluxTheme.haptics
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val fg = if (item.destructive) colors.coralText else colors.ink
    val iconColor: Color = if (item.destructive) colors.coralText else colors.inkMuted

    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .alpha(if (item.enabled) 1f else 0.38f)
            .fluxPressable(
                onClick = {
                    haptics.tick()
                    if (!item.keepOpen) state.dismissMenu()
                    item.onClick()
                },
                enabled = item.enabled,
                role = when {
                    item.radio -> Role.RadioButton
                    item.showTrailingCheck -> Role.Button
                    else -> Role.Checkbox
                },
                interactionSource = source,
            )
            .background(if (pressed) colors.glass3 else Color.Transparent, FluxTheme.shapes.control)
            .semantics {
                if (item.radio || item.showTrailingCheck) {
                    selected = item.checked
                } else {
                    toggleableState = ToggleableState(item.checked)
                }
            }
            .padding(start = if (item.indent) 38.dp else 12.dp, end = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item.icon?.let { FluxIcon(it, null, size = 18.dp, tint = iconColor) }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.Center) {
            FluxText(item.label, style = type.body, color = fg, maxLines = 2)
            item.hint?.let {
                if (item.hintIsPath) {
                    FluxText(it, style = type.sm, color = colors.inkMuted, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                } else {
                    FluxText(it, style = type.sm, color = colors.inkMuted, maxLines = 2)
                }
            }
        }
        item.trailing?.let { FluxText(it, style = type.weight(type.monoS, 500, mono = true), color = colors.inkFaint, maxLines = 1) }
        if (item.checked && item.showTrailingCheck && !item.radio) FluxIcon(FluxIcons.Check, null, size = 18.dp, tint = colors.accentHi)
    }
}
