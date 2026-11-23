package com.fluxdown.fluxui.overlay

import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.AnimationSpec
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Stable
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.BlurEffect
import androidx.compose.ui.graphics.TileMode
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.layer.drawLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.material.FluxBlur
import com.fluxdown.fluxui.material.FluxGlass
import com.fluxdown.fluxui.material.FluxGlassKind
import com.fluxdown.fluxui.material.LocalFluxBackdrop
import com.fluxdown.fluxui.material.fluxGlass
import com.fluxdown.fluxui.theme.FluxGlassMode
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.LocalFluxGlassMode
import com.fluxdown.fluxui.theme.fluxPressable

/**
 * 覆盖层状态机：Toast（同一时刻一条，新替旧）、Menu（一个）、Dialog（一个）。
 * Sheet 与命令搜索是声明式组件（[FluxSheet] / [CommandSearchScaffold]），不经过本状态。
 * 全部在主窗口内渲染（不使用 Popup / Dialog 窗口），因此玻璃可取样 [LocalFluxBackdrop]。
 */
@Stable
class FluxOverlayState {
    private var seq = 0L

    internal var toastEntry by mutableStateOf<ToastEntry?>(null)
    internal var menuEntry by mutableStateOf<MenuEntry?>(null)
    internal var dialogEntry by mutableStateOf<DialogEntry?>(null)

    /** 是否有菜单 / 对话框在显示（用于让出返回键优先级等）。 */
    val hasMenu: Boolean get() = menuEntry != null
    val hasDialog: Boolean get() = dialogEntry != null

    /**
     * 顶部浮动烟晶 Toast。新的立即替换旧的；[durationMs] 会经
     * `AccessibilityManager.getRecommendedTimeoutMillis` 放大。
     * 触感：Success → confirm；Warn / Error → reject。
     */
    fun toast(
        text: String,
        kind: FluxToastKind = FluxToastKind.Info,
        icon: ImageVector? = null,
        action: FluxToastAction? = null,
        durationMs: Long = 3200L,
    ) {
        toastEntry = ToastEntry(++seq, text, kind, icon, action, durationMs)
    }

    fun dismissToast() {
        toastEntry = null
    }

    internal fun dismissToastIf(id: Long) {
        if (toastEntry?.id == id) toastEntry = null
    }

    /**
     * 在锚点矩形（根坐标，如 `onGloballyPositioned { it.boundsInRoot() }`）旁弹出菜单。
     * 同时只有一个菜单，新的替换旧的。
     */
    fun showMenu(
        anchor: Rect,
        items: List<FluxMenuItem>,
        header: String? = null,
        align: FluxMenuAlign? = null,
    ) {
        menuEntry = MenuEntry(++seq, anchor, items, header, align)
    }

    /** 已打开的菜单随条目状态更新，保留锚点与进场状态。 */
    fun showMenu(anchor: Rect, items: State<List<FluxMenuItem>>) {
        menuEntry = MenuEntry(++seq, anchor, emptyList(), header = null, align = null, liveItems = items)
    }

    fun dismissMenu() {
        menuEntry = null
    }

    /** 居中对话框；同时只有一个，新的替换旧的。 */
    fun showDialog(spec: FluxDialogSpec) {
        dialogEntry = DialogEntry(++seq, spec)
    }

    fun dismissDialog() {
        dialogEntry = null
    }
}

@Composable
fun rememberFluxOverlayState(): FluxOverlayState = remember { FluxOverlayState() }

/** 屏幕层经此取得覆盖层状态：`LocalFluxOverlays.current.toast("已复制", FluxToastKind.Success)`。 */
val LocalFluxOverlays = staticCompositionLocalOf<FluxOverlayState> {
    error("LocalFluxOverlays not provided: wrap the app in CompositionLocalProvider(LocalFluxOverlays provides state)")
}

/**
 * 最顶层全屏宿主（z：菜单 80 < 对话框 85 < Toast 88）。放在应用根 Box 的最后一个子项，
 * 且必须是 `fluxBackdropSource` 内容的兄弟（之后绘制）。空闲时不拦截任何触摸。
 * 返回键优先级：菜单 → 对话框。
 */
@Composable
fun FluxOverlayHost(state: FluxOverlayState, modifier: Modifier = Modifier) {
    CompositionLocalProvider(LocalFluxOverlays provides state) {
        Box(modifier.fillMaxSize()) {
            MenuLayer(state)
            DialogLayer(state)
            ToastLayer(state)
        }
    }
}

// ───────────────────────────── 内部共享件 ─────────────────────────────

/** 保存“最后一个非空值”，供退场动画期间继续渲染。 */
internal class Last<T : Any> {
    var value: T? = null
}

/** 进出场进度（0..1，弹簧允许过冲）与“是否仍在组合树中”。 */
@Stable
internal class OverlayProgress {
    val anim = Animatable(0f)
    var present by mutableStateOf(false)
        private set

    val value: Float get() = anim.value

    suspend fun enter(spec: AnimationSpec<Float>, restartAt: Float? = null) {
        present = true
        if (restartAt != null && anim.value > restartAt) anim.snapTo(restartAt)
        anim.animateTo(1f, spec)
    }

    suspend fun exit(spec: AnimationSpec<Float>) {
        anim.animateTo(0f, spec)
        present = false
    }
}

@Composable
internal fun rememberOverlayProgress(): OverlayProgress = remember { OverlayProgress() }

/**
 * 遮罩：`dim` 渐入 + 背景 [blurSigma] 模糊（§5.2：Sheet 8 / Dialog 10，直接 RenderEffect，
 * 非共享变体）。模糊副本引用背景源 RenderNode，所以内容变化自动传播。Solid 模式只画 dim。
 */
@Composable
internal fun Modifier.fluxScrim(blurSigma: Dp, progress: () -> Float): Modifier {
    val dim = FluxTheme.colors.dim
    val backdrop = LocalFluxBackdrop.current
    val blurOn = LocalFluxGlassMode.current == FluxGlassMode.Blur && backdrop != null
    val origin = remember { FloatArray(2) }
    return this
        .onGloballyPositioned {
            val p = it.positionInRoot()
            origin[0] = p.x
            origin[1] = p.y
        }
        .drawWithCache {
            val src = backdrop?.source
            val layer = if (blurOn && src != null && src.size.width > 0) {
                obtainGraphicsLayer().also { l ->
                    val px = blurSigma.toPx()
                    l.renderEffect = BlurEffect(px, px, TileMode.Clamp)
                    l.record(this, layoutDirection, IntSize(src.size.width, src.size.height)) { drawLayer(src) }
                }
            } else {
                null
            }
            onDrawBehind {
                val p = progress().coerceIn(0f, 1f)
                if (layer != null && backdrop != null) {
                    layer.alpha = p
                    translate(backdrop.originInRoot.x - origin[0], backdrop.originInRoot.y - origin[1]) {
                        drawLayer(layer)
                    }
                }
                drawRect(dim, alpha = p)
            }
        }
}

/** 吞掉点击（浮层表面空白处不穿透到遮罩）。 */
internal fun Modifier.swallowTaps(): Modifier = pointerInput(Unit) { detectTapGestures { } }

/** 命中 48dp、视觉 [visualSize] 的圆形图标钮（关闭钮 / 清除钮）。 */
@Composable
internal fun OverlayIconButton(
    icon: ImageVector,
    contentDescription: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    visualSize: Dp = 36.dp,
    iconSize: Dp = 18.dp,
    glass: Boolean = true,
) {
    val c = FluxTheme.colors
    Box(
        modifier
            .size(48.dp)
            .fluxPressable(onClick, scale = 0.92f, role = Role.Button)
            .semantics { this.contentDescription = contentDescription },
        contentAlignment = Alignment.Center,
    ) {
        val inner = if (glass) {
            Modifier.size(visualSize).fluxGlass(FluxGlass.G2, FluxTheme.shapes.full, kind = FluxGlassKind.Flat)
        } else {
            Modifier.size(visualSize)
        }
        Box(inner, contentAlignment = Alignment.Center) {
            FluxIcon(icon, null, size = iconSize, tint = c.ink)
        }
    }
}

/** 幽灵文字钮（取消）：视觉 36dp、命中 48dp。 */
@Composable
internal fun OverlayTextButton(label: String, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val type = FluxTheme.type
    Box(
        modifier
            .sizeInMin48()
            .fluxPressable(onClick, role = Role.Button),
        contentAlignment = Alignment.Center,
    ) {
        FluxText(label, style = type.weight(type.sm, 600), color = FluxTheme.colors.ink, maxLines = 1)
    }
}

private fun Modifier.sizeInMin48(): Modifier =
    this.widthIn(min = 56.dp).heightIn(min = 48.dp).padding(horizontal = 8.dp)
