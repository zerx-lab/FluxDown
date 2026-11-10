package com.fluxdown.fluxui.material

import android.graphics.ColorMatrix as AndroidColorMatrix
import android.graphics.ColorMatrixColorFilter
import android.graphics.RenderEffect as AndroidRenderEffect
import android.graphics.Shader
import android.os.Build
import androidx.annotation.RequiresApi
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.BlurEffect
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.RenderEffect
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.TileMode
import androidx.compose.ui.graphics.asComposeRenderEffect
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.graphics.drawOutline
import androidx.compose.ui.graphics.drawscope.ContentDrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.layer.GraphicsLayer
import androidx.compose.ui.graphics.layer.drawLayer
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.addOutline
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.node.DrawModifierNode
import androidx.compose.ui.node.GlobalPositionAwareModifierNode
import androidx.compose.ui.node.ModifierNodeElement
import androidx.compose.ui.node.invalidateDraw
import androidx.compose.ui.node.requireGraphicsContext
import androidx.compose.ui.platform.InspectorInfo
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import com.fluxdown.fluxui.theme.FluxColors
import com.fluxdown.fluxui.theme.FluxGlassMode
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.LocalFluxGlassMode
import java.util.EnumMap

/** 三个共享模糊变体（σ 单位 dp）。模糊成本只与变体数（≤3）有关，与玻璃面数量无关。 */
enum class FluxBlur(val sigma: Dp, val saturation: Float) {
    Thin(20.dp, 1.00f), Regular(28.dp, 1.40f), Thick(38.dp, 1.55f);

    @RequiresApi(Build.VERSION_CODES.S)
    internal fun renderEffect(density: Density): RenderEffect {
        val px = with(density) { sigma.toPx() }
        val blur = AndroidRenderEffect.createBlurEffect(px, px, Shader.TileMode.CLAMP)
        val out = if (saturation == 1f) {
            blur
        } else {
            AndroidRenderEffect.createColorFilterEffect(
                ColorMatrixColorFilter(AndroidColorMatrix().apply { setSaturation(saturation) }),
                blur,
            )
        }
        return out.asComposeRenderEffect()
    }
}

/** 底色层：glass1..4 或浮层专用底。 */
enum class FluxGlass { G1, G2, G3, G4, Sheet, Menu }

/**
 * Real = 下方有内容穿过（共享模糊副本）；Flat = 下方只有画布（不模糊，直接合成色）。
 * 同屏 Real 面 ≤ 4（§5.3）；可回收列表项内禁止 Real。
 */
enum class FluxGlassKind { Real, Flat }

/** 页面级背景源：录制一次整屏内容，按需生成每个模糊变体的副本。每个窗口一个。 */
@Stable
class FluxBackdrop {
    internal var source: GraphicsLayer? = null
    internal val blurred = EnumMap<FluxBlur, GraphicsLayer>(FluxBlur::class.java)
    internal val requested = java.util.EnumSet.noneOf(FluxBlur::class.java)
    internal var originInRoot by mutableStateOf(Offset.Zero)

    /** 变体集合 / 副本就绪的版本号：源节点与玻璃面在 draw 中读取以互相失效。 */
    internal var version by mutableIntStateOf(0)

    /** 玻璃面声明使用某变体；首次声明时让源节点下一帧录制该副本。 */
    internal fun request(b: FluxBlur): GraphicsLayer? {
        if (requested.add(b)) version++
        return blurred[b]
    }
}

val LocalFluxBackdrop = staticCompositionLocalOf<FluxBackdrop?> { null }

/** 放在“整屏内容”的根上（画布 + 颗粒 + 页面内容）。玻璃面必须是其后绘制的兄弟。 */
fun Modifier.fluxBackdropSource(backdrop: FluxBackdrop): Modifier = this then BackdropSourceElement(backdrop)

private data class BackdropSourceElement(val b: FluxBackdrop) : ModifierNodeElement<BackdropSourceNode>() {
    override fun create() = BackdropSourceNode(b)
    override fun update(node: BackdropSourceNode) {
        node.b = b
    }

    override fun InspectorInfo.inspectableProperties() {
        name = "fluxBackdropSource"
    }
}

private class BackdropSourceNode(var b: FluxBackdrop) : Modifier.Node(), DrawModifierNode, GlobalPositionAwareModifierNode {
    override fun ContentDrawScope.draw() {
        val gc = requireGraphicsContext()
        val src = b.source ?: gc.createGraphicsLayer().also { b.source = it }
        val sz = IntSize(size.width.toInt(), size.height.toInt())
        src.record(this, layoutDirection, sz) { this@draw.drawContent() }
        drawLayer(src)
        // 读取 version 以便新变体被请求时重绘；副本只引用源 RenderNode，内容变化自动传播。
        // Android 12 以下 FluxPerf 恒为 Solid，不会有变体被请求。
        if (b.version >= 0 && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            for (blur in b.requested) {
                val existing = b.blurred[blur]
                val layer = existing ?: gc.createGraphicsLayer().also {
                    it.renderEffect = blur.renderEffect(this)
                    b.blurred[blur] = it
                }
                if (existing == null || existing.size != sz) {
                    layer.record(this, layoutDirection, sz) { drawLayer(src) }
                }
                if (existing == null) b.version++
            }
        }
    }

    override fun onGloballyPositioned(coordinates: LayoutCoordinates) {
        b.originInRoot = coordinates.positionInRoot()
    }

    override fun onDetach() {
        val gc = requireGraphicsContext()
        b.source?.let(gc::releaseGraphicsLayer)
        b.source = null
        b.blurred.values.forEach(gc::releaseGraphicsLayer)
        b.blurred.clear()
        b.requested.clear()
    }
}

/**
 * 烟晶面：底色 + 0.5dp 发丝线 + 顶沿高光；Real 时先画自己下方那一窗口的模糊副本。
 * [canvasMix] 为“canvas X% + glassN”的复合比例（如坞 0.38、读数条 0.55）。
 */
@Composable
fun Modifier.fluxGlass(
    level: FluxGlass = FluxGlass.G2,
    shape: Shape = FluxTheme.shapes.card,
    blur: FluxBlur = FluxBlur.Regular,
    kind: FluxGlassKind = FluxGlassKind.Real,
    canvasMix: Float = 0f,
    strongLine: Boolean = level == FluxGlass.Sheet || level == FluxGlass.Menu,
): Modifier {
    val colors = FluxTheme.colors
    val mode = LocalFluxGlassMode.current
    val backdrop = LocalFluxBackdrop.current
    return this then GlassElement(level, shape, blur, kind, canvasMix, strongLine, colors, mode, backdrop)
}

private data class GlassElement(
    val level: FluxGlass,
    val shape: Shape,
    val blur: FluxBlur,
    val kind: FluxGlassKind,
    val canvasMix: Float,
    val strongLine: Boolean,
    val colors: FluxColors,
    val mode: FluxGlassMode,
    val backdrop: FluxBackdrop?,
) : ModifierNodeElement<GlassNode>() {
    override fun create() = GlassNode(this)
    override fun update(node: GlassNode) {
        node.spec = this
        node.invalidateDraw()
    }

    override fun InspectorInfo.inspectableProperties() {
        name = "fluxGlass"
    }
}

private class GlassNode(var spec: GlassElement) : Modifier.Node(), DrawModifierNode, GlobalPositionAwareModifierNode {
    private var origin = Offset.Zero
    private val path = Path()

    override fun onGloballyPositioned(coordinates: LayoutCoordinates) {
        val p = coordinates.positionInRoot()
        if (p != origin) {
            origin = p
            invalidateDraw()
        }
    }

    override fun ContentDrawScope.draw() {
        val s = spec
        val c = s.colors
        val outline = s.shape.createOutline(size, layoutDirection, this)
        val bd = s.backdrop
        // 弹出层（Sheet / 对话框 / 菜单等）一律不透明：不采样下层模糊副本，直接画合成到画布的实色
        val popup = s.level == FluxGlass.Sheet || s.level == FluxGlass.Menu
        val real = !popup && s.kind == FluxGlassKind.Real && s.mode == FluxGlassMode.Blur && bd != null && bd.version >= 0
        val layer = if (real) bd?.request(s.blur) else null
        if (layer != null && bd != null) {
            val d = origin - bd.originInRoot
            path.reset()
            path.addOutline(outline)
            // 先垫画布：页面级背景源（只录列表）是透明的，不垫底时下方清晰的文字会从模糊副本的透明处透出来。
            clipPath(path) {
                drawRect(c.canvas)
                translate(-d.x, -d.y) { drawLayer(layer) }
            }
            drawOutline(outline, tint(s, c))
        } else {
            drawOutline(outline, solid(s, c))
        }
        val hw = 0.5.dp.toPx()
        drawOutline(outline, Brush.verticalGradient(0f to c.highlight, .18f to Color.Transparent), style = Stroke(hw))
        drawOutline(outline, if (s.strongLine) c.hairlineStrong else c.hairline, style = Stroke(hw))
        drawContent()
    }

    private fun base(level: FluxGlass, c: FluxColors): Color = when (level) {
        FluxGlass.G1 -> c.glass1
        FluxGlass.G2 -> c.glass2
        FluxGlass.G3 -> c.glass3
        FluxGlass.G4 -> c.glass4
        FluxGlass.Sheet -> c.sheetBg
        FluxGlass.Menu -> c.menuBg
    }

    /** 叠在模糊副本上的半透明底。 */
    private fun tint(s: GlassElement, c: FluxColors): Color {
        val b = base(s.level, c)
        return if (s.canvasMix > 0f) c.mixCanvas(b, s.canvasMix) else b
    }

    /** 无模糊（Flat / 降级）时的实色：合成到 canvas 上。 */
    private fun solid(s: GlassElement, c: FluxColors): Color = when (s.level) {
        FluxGlass.G1 -> if (s.kind == FluxGlassKind.Flat) tint(s, c) else c.glassSolid1
        FluxGlass.G2 -> if (s.kind == FluxGlassKind.Flat) tint(s, c) else c.glassSolid2
        FluxGlass.G3 -> if (s.kind == FluxGlassKind.Flat) tint(s, c) else c.glassSolid3
        FluxGlass.G4 -> if (s.kind == FluxGlassKind.Flat) tint(s, c) else c.glassSolid4
        FluxGlass.Sheet -> c.sheetBg.compositeOver(c.canvas)
        FluxGlass.Menu -> c.menuBg.compositeOver(c.canvas)
    }
}

/**
 * 辉光 / 软阴影（§5.5 的有限例外）：在内容之后画一个被高斯模糊的形状。
 * [radius] = 模糊 σ；[spread] 向外扩（负值内缩）；[dy] 为 Y 偏移。
 * Android 12 以下图层不支持 `RenderEffect`（会画出未模糊的实心形状），整体不画。
 */
fun Modifier.fluxGlow(
    color: Color,
    radius: Dp,
    shape: Shape,
    spread: Dp = 0.dp,
    dy: Dp = 0.dp,
): Modifier = if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) this else drawWithCache {
    val pad = radius.toPx() * 3 + kotlin.math.abs(spread.toPx())
    val layer = obtainGraphicsLayer()
    val sp = spread.toPx()
    val outline = shape.createOutline(
        androidx.compose.ui.geometry.Size(size.width + 2 * sp, size.height + 2 * sp),
        layoutDirection,
        this,
    )
    layer.renderEffect = BlurEffect(radius.toPx(), radius.toPx(), TileMode.Decal)
    layer.record(this, layoutDirection, IntSize((size.width + 2 * pad).toInt(), (size.height + 2 * pad).toInt())) {
        translate(pad - sp, pad - sp) { drawOutline(outline, color) }
    }
    onDrawBehind {
        translate(-pad, dy.toPx() - pad) { drawLayer(layer) }
    }
}

/**
 * 弹出浮层（菜单、Toast、对话框）与浮动 chrome（坞、新建球、读数条）的环境阴影色（§5.5 例外）：
 * 深色近黑，浅色降到 ink 的低透明度。仅 token 派生，配合 [fluxGlow] 的向下偏移 + 内缩使用（投影，不是光晕）。
 */
internal fun FluxColors.softShadow(alphaDark: Float): Color =
    if (dark) ramp[0].copy(alpha = alphaDark) else ramp[11].copy(alpha = alphaDark * 0.45f)
