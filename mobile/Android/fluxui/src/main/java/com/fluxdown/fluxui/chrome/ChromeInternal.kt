package com.fluxdown.fluxui.chrome

import androidx.compose.animation.core.Animatable
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.TransformOrigin
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.fluxdown.fluxui.theme.FluxSpring
import com.fluxdown.fluxui.theme.FluxTheme
import androidx.compose.ui.graphics.Shape
import com.fluxdown.fluxui.material.FluxBlur
import com.fluxdown.fluxui.material.FluxGlass
import com.fluxdown.fluxui.material.FluxGlassKind
import com.fluxdown.fluxui.material.fluxGlass
import com.fluxdown.fluxui.material.fluxGlow
import com.fluxdown.fluxui.material.softShadow

/**
 * 出入场进度：`progress` 0..1（弹簧驱动），`composed` 表示内容是否仍需留在组合中
 * （退场动画结束后移除，也不再进入无障碍树）。
 */
@Stable
internal class ChromePresence(visible: Boolean) {
    val progress = Animatable(if (visible) 1f else 0f)
    var composed by mutableStateOf(visible)
}

@Composable
internal fun rememberChromePresence(visible: Boolean, spring: FluxSpring): ChromePresence {
    val motion = FluxTheme.motion
    val p = remember { ChromePresence(visible) }
    LaunchedEffect(visible, motion) {
        if (visible) p.composed = true
        p.progress.animateTo(if (visible) 1f else 0f, motion.of(spring))
        if (!visible) p.composed = false
    }
    return p
}

/**
 * 出入场图层：`alpha / translateY / scale` 随 [progress]（1 = 完全显示）变化。
 * [progress] 在 graphicsLayer 内读取，动画期间不触发重组。不做模糊：文字全程清晰，不会“糊 → 突然清晰”。
 */
internal fun Modifier.chromeFade(
    translateY: Dp = 0.dp,
    scaleFrom: Float = 1f,
    pivotY: Float = 0.5f,
    progress: () -> Float,
): Modifier = graphicsLayer {
    val raw = progress()
    alpha = raw.coerceIn(0f, 1f)
    translationY = translateY.toPx() * (1f - raw)
    val s = scaleFrom + (1f - scaleFrom) * raw
    scaleX = s
    scaleY = s
    transformOrigin = TransformOrigin(0.5f, pivotY)
}

/**
 * 浮动 chrome（导航坞 / 选择坞 / 顶部读数条 / 退出选择球）的共同表面，对齐 iOS Liquid Glass `regular`：
 * 真模糊玻璃（[FluxBlur.Regular] σ28 + 饱和度 1.4，底色 `glass2`）+ 0.5dp 顶沿高光与发丝线 + 向下软投影。
 * 下方内容带色穿过、文字被糊掉不可辨；省电 / 过热 / Android 12 以下或没有背景源时退化为 `glassSolid2` 实色，信息结构不变。
 * 必须位于 `fluxBackdropSource` 之后的兄弟层（用户 2026-11-06 决定改回毛玻璃，取代 2026-10-22 的实色底）。
 */
@Composable
internal fun Modifier.floatingChromeSurface(shape: Shape): Modifier =
    this
        .floatingChromeShadow(shape)
        .fluxGlass(FluxGlass.G2, shape, FluxBlur.Regular, FluxGlassKind.Real, strongLine = FluxTheme.colors.dark)

/** 浮动 chrome 与新建球共用的向下软投影（token 派生色，与 Toast 同一做法；Android 12 以下不画）。 */
@Composable
internal fun Modifier.floatingChromeShadow(shape: Shape): Modifier =
    fluxGlow(FluxTheme.colors.softShadow(0.42f), 14.dp, shape, spread = (-6).dp, dy = 8.dp)
