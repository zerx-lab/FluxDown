package com.fluxdown.fluxui.chrome

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.BasicText
import androidx.compose.runtime.Composable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.theme.FluxScaleGroup
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.fluxPressable

/**
 * 顶部读数条（01 §12.6）：下载页滚动后，速度仪表收为顶部 Real 玻璃细条上的实时读数。
 *
 * - [progress]：折叠进度 0..1（`collapse`，§8.7）；`> [threshold]`（默认 0.55）时以 `fluid` 入场
 *   （`translateY −14→0、scale .96→1、alpha`），回落则离场（退场结束后移出组合，不再占用背景副本）。
 *   `progress` 在派生状态内读取，滚动时只在越过阈值时重组。
 * - 槽位（左→右）：速度读数（`mono 15·km / 600` accentHi + 0.72em 单位）、[waveform] 迷你波形槽（占满剩余宽度，高 22dp）、
 *   [meta]（`monoS inkMuted`）、36dp 暂停 / 恢复钮（[allPaused] 时显示播放）。
 * - 点按整条 → [onClick]。无障碍：读数 / 波形 / 元信息合并为一句“实时下载速度 …”且**不设 liveRegion**；暂停钮独立。
 * - [label] 非空（多选模式）：整条只显示这行文字（如「已选 3 项」，`h2` ink），不显示读数 / 波形 / 暂停钮，也不可点。
 * - 尺寸：满宽、高 46、圆角全圆；位置（left/right 12、top = 状态栏 − 4）由调用方摆放。
 */
@Composable
fun FluxTopStrip(
    progress: () -> Float,
    speedValue: String,
    speedUnit: String,
    meta: String,
    allPaused: Boolean,
    onTogglePause: () -> Unit,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    threshold: Float = 0.55f,
    speedDescription: String = "实时下载速度 $speedValue $speedUnit",
    pauseDescription: String = "全部暂停",
    resumeDescription: String = "全部恢复",
    waveform: @Composable BoxScope.() -> Unit = {},
    label: String? = null,
) {
    val motion = FluxTheme.motion
    val progressState = rememberUpdatedState(progress)
    val visible by remember(threshold) { derivedStateOf { progressState.value() > threshold } }
    val presence = rememberChromePresence(visible, motion.fluid)
    if (!presence.composed) return

    val c = FluxTheme.colors
    val type = FluxTheme.type
    val numStyle = remember(type) {
        type.sized(type.weight(type.mono, 600, mono = true), 15f, FluxScaleGroup.Km).copy(letterSpacing = (-0.02).em)
    }
    val metaStyle = remember(type) { type.weight(type.monoS, 500, mono = true) }
    val readout = remember(speedValue, speedUnit, c) {
        buildAnnotatedString {
            append(speedValue)
            withStyle(SpanStyle(fontSize = 0.72.em, color = c.inkMuted)) {
                append(' ')
                append(speedUnit)
            }
        }
    }
    val shape = FluxTheme.shapes.full
    Row(
        modifier
            .fillMaxWidth()
            .height(48.dp)
            .chromeFade(translateY = (-14).dp, scaleFrom = 0.96f, pivotY = 0f) { presence.progress.value }
            .then(if (label == null) Modifier.fluxPressable(onClick = onClick, scale = 0.98f, enabled = visible, role = Role.Button) else Modifier)
            .floatingChromeSurface(shape)
            .padding(start = 16.dp, end = 5.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (label != null) {
            FluxText(
                label,
                modifier = Modifier.weight(1f),
                style = type.h2,
                color = c.ink,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            return@Row
        }
        Row(
            Modifier
                .weight(1f)
                .clearAndSetSemantics { contentDescription = speedDescription },
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            BasicText(
                readout,
                style = numStyle.copy(color = c.accentHi),
                maxLines = 1,
                softWrap = false,
                overflow = TextOverflow.Clip,
            )
            Box(Modifier.weight(1f).height(22.dp), content = waveform)
            FluxText(meta, style = metaStyle, color = c.inkMuted, maxLines = 1, softWrap = false, overflow = TextOverflow.Clip)
        }
        FluxGlassIconButton(
            icon = if (allPaused) FluxIcons.Play else FluxIcons.Pause,
            contentDescription = if (allPaused) resumeDescription else pauseDescription,
            onClick = onTogglePause,
            size = 36.dp,
            iconSize = 18.dp,
            enabled = visible,
        )
    }
}
