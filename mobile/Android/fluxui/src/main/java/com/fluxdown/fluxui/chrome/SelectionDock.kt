package com.fluxdown.fluxui.chrome

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.isTraversalGroup
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
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

/**
 * 选择坞（01 §12.4）：多选模式下接替 [FluxDock] 的批量操作条，与坞同一浮动表面（[floatingChromeSurface]）。
 *
 * 布局同 [FluxDock]：占满父容器宽度、高为主题 dockHeight，本体宽 = 容器宽 − [reservedEnd]（右侧留给 [FluxOrb] 的“退出选择”球）；
 * Expanded 档浮在列表区底部居中时传 `reservedEnd = 0.dp` 并给 `modifier` 加 `widthIn(max = 560.dp)`。
 *
 * - 计数块：首行 [FluxIcons.CheckCheck]（18，全选后 accentHi、否则 inkMuted）+ `mono 17 / 600` 数字（accentHi），
 *   次行 micro 标签（[selectAllLabel] / 全选后 [deselectAllLabel]），点按 → [onToggleAll]。
 * - 动作：48×48（图标 20 + micro 文字），`fontScale ≥ 1.3` 只显示图标（描述保留）；放不下时横向滚动。
 * - 出入场：`translateY 18→0、scale .94→1`（`liquid`）。
 * - a11y：计数块 `liveRegion = Polite` 播报“已选 N 项”。
 */
@Composable
fun SelectionDock(
    count: Int,
    total: Int,
    onToggleAll: () -> Unit,
    actions: List<SelectionDockAction>,
    visible: Boolean,
    modifier: Modifier = Modifier,
    reservedEnd: Dp = FluxTheme.space.dockHeight + 12.dp,
    selectAllLabel: String = "已选 · 全选",
    deselectAllLabel: String = "取消全选",
    countDescription: (Int) -> String = { "已选 $it 项" },
    allSelectedDescription: String = "已全选",
    notAllSelectedDescription: String = "未全选",
    contentDescription: String = "批量操作",
) {
    val motion = FluxTheme.motion
    val presence = rememberChromePresence(visible, motion.liquid)
    Box(modifier.fillMaxWidth().height(FluxTheme.space.dockHeight)) {
        if (!presence.composed) return@Box
        val c = FluxTheme.colors
        val type = FluxTheme.type
        val shape = FluxTheme.shapes.sheet
        val iconOnly = type.fontScale >= 1.3f
        val allSelected = total > 0 && count >= total
        Row(
            Modifier
                .padding(end = reservedEnd)
                .fillMaxWidth()
                .height(FluxTheme.space.dockHeight)
                .chromeFade(translateY = 18.dp, scaleFrom = 0.94f) { presence.progress.value }
                .floatingChromeSurface(shape)
                .clip(shape)
                .semantics {
                    isTraversalGroup = true
                    this.contentDescription = contentDescription
                }
                .padding(start = 10.dp, end = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            val numStyle = remember(type) {
                type.sized(type.weight(type.mono, 600, mono = true), 17f, FluxScaleGroup.Km)
                    .copy(letterSpacing = (-0.02).em, lineHeight = 1.em)
            }
            Column(
                Modifier
                    .widthIn(min = FluxTheme.space.touch)
                    .height(FluxTheme.space.touch)
                    .clip(RoundedCornerShape(22.dp))
                    .fluxPressable(onClick = onToggleAll, scale = 0.95f, enabled = visible)
                    .padding(start = 6.dp, end = 12.dp)
                    .semantics {
                        liveRegion = LiveRegionMode.Polite
                        this.contentDescription = "${countDescription(count)}, ${if (allSelected) deselectAllLabel else selectAllLabel}"
                        stateDescription = if (allSelected) allSelectedDescription else notAllSelectedDescription
                    },
                verticalArrangement = Arrangement.Center,
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    FluxIcon(FluxIcons.CheckCheck, null, size = 18.dp, tint = if (allSelected) c.accentHi else c.inkMuted)
                    FluxText(
                        count.toString(),
                        modifier = Modifier.padding(start = 6.dp).clearAndSetSemantics { },
                        style = numStyle,
                        color = c.accentHi,
                        maxLines = 1,
                        softWrap = false,
                    )
                }
                if (!iconOnly) {
                    FluxText(
                        if (allSelected) deselectAllLabel else selectAllLabel,
                        modifier = Modifier.padding(top = 3.dp).clearAndSetSemantics { },
                        style = type.micro,
                        color = c.inkMuted,
                        maxLines = 1,
                        softWrap = false,
                        overflow = TextOverflow.Clip,
                    )
                }
            }
            Row(
                Modifier
                    .weight(1f, fill = false)
                    .horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                actions.forEach { a ->
                    SelectionActionButton(a, iconOnly, visible)
                }
            }
        }
    }
}

@Composable
private fun SelectionActionButton(action: SelectionDockAction, iconOnly: Boolean, interactive: Boolean) {
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val tint = if (action.danger) c.coralText else c.ink
    val style = remember(type) {
        type.sized(type.weight(type.micro, 500), 10f, FluxScaleGroup.Ks).copy(letterSpacing = 0.02.em)
    }
    Column(
        Modifier
            .widthIn(min = FluxTheme.space.touch)
            .height(FluxTheme.space.touch)
            .graphicsLayer { alpha = if (action.enabled) 1f else 0.32f }
            .clip(RoundedCornerShape(22.dp))
            .fluxPressable(
                onClick = action.onClick,
                scale = 0.9f,
                enabled = action.enabled && interactive,
                role = Role.Button,
            )
            .padding(horizontal = 4.dp)
            .semantics { contentDescription = action.label },
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(2.dp, Alignment.CenterVertically),
    ) {
        FluxIcon(action.icon, null, size = 20.dp, tint = tint)
        if (!iconOnly) {
            FluxText(
                action.label,
                modifier = Modifier.clearAndSetSemantics { },
                style = style,
                color = tint,
                maxLines = 1,
                softWrap = false,
                overflow = TextOverflow.Clip,
            )
        }
    }
}
