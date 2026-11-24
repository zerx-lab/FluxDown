package com.fluxdown.app.feature.settings.extensions

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import androidx.annotation.StringRes
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.PluginDiagnostic
import com.fluxdown.core.protocol.ComponentKind
import com.fluxdown.core.protocol.ComponentStatusDto
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.PluginAuth
import com.fluxdown.core.protocol.PluginDto
import com.fluxdown.core.protocol.PluginMarket
import com.fluxdown.core.protocol.QrEncoder
import com.fluxdown.core.protocol.QrMatrix
import com.fluxdown.fluxui.controls.FluxTag
import com.fluxdown.fluxui.controls.Tone
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import java.util.Locale

// 扩展 / 插件详情 / 市场 / 登录 / 组件共用的小构件：分区解码缓存、错误文案、徽标流、剪贴板、安全链接、二维码。

// ───────────────────────────── 分区解码缓存 ─────────────────────────────

/** `daemon.plugins` 分区（`Vec<PluginDto>`）：只在分区 JSON 文本变化时重新解码。 */
@Composable
internal fun rememberPlugins(): State<List<PluginDto>> {
    val host = hostState()
    val json by remember { derivedStateOf { host.value.sections[HostSection.daemonPlugins] } }
    return remember { derivedStateOf { PluginDto.listFromJson(Json.parseOrNull(json)) } }
}

/** `daemon.components` 分区（`Vec<ComponentStatusDto>`）：同 [rememberPlugins] 的缓存策略。 */
@Composable
internal fun rememberComponentStatuses(): State<List<ComponentStatusDto>> {
    val host = hostState()
    val json by remember { derivedStateOf { host.value.sections[HostSection.daemonComponents] } }
    return remember { derivedStateOf { ComponentStatusDto.listFromJson(Json.parseOrNull(json)) } }
}

// ───────────────────────────── 环境与错误文案 ─────────────────────────────

/** 模型操作需要的 UI 环境（文案 + toast）；与组合无关，可在后台协程里使用。 */
internal class ExtensionsEnv(val context: Context, private val overlays: FluxOverlayState) {
    fun text(@StringRes id: Int, vararg args: Pair<String, Any?>): String = context.str(id, *args)

    fun toast(text: String, kind: FluxToastKind) = overlays.toast(text, kind)

    fun error(e: HostException): String = extensionErrorText(context, e)

    fun showPluginFailure(@StringRes operation: Int, error: HostException) {
        val message = text(operation, "message" to pluginFailureText(context, error))
        overlays.showDialog(
            FluxDialogSpec(
                title = text(R.string.statusError),
                message = message,
                buttons = listOf(
                    FluxDialogButton(text(R.string.close)),
                    FluxDialogButton(text(R.string.pluginLoadErrorCopy)) {
                        context.copyPlainText(message)
                        toast(text(R.string.pluginLoadErrorCopied), FluxToastKind.Success)
                    },
                ),
            ),
        )
    }
}

@Composable
internal fun rememberExtensionsEnv(): ExtensionsEnv {
    val context = LocalContext.current.applicationContext
    val overlays = com.fluxdown.fluxui.overlay.LocalFluxOverlays.current
    return remember(context, overlays) { ExtensionsEnv(context, overlays) }
}

/**
 * 插件 / 组件 / 市场错误：先按 `reason`（`ErrorReason` wire 名）映射可操作文案，再按码回退
 * （`web/.../extensions/errors.ts` ↔ GPUI `error_text` ↔ iOS `ExtensionErrorText`）。
 */
internal fun extensionErrorText(context: Context, e: HostException): String {
    val byReason = when (e.reason) {
        "marketUnreachable" -> R.string.pluginErrorMarketUnreachable
        "marketIndexInvalid" -> R.string.pluginErrorMarketIndexInvalid
        "marketIndexRollback" -> R.string.pluginErrorMarketIndexRollback
        "pluginNotInMarket" -> R.string.pluginErrorNotInMarket
        "pluginYanked" -> R.string.pluginErrorYanked
        "pluginDownloadFailed" -> R.string.pluginErrorDownloadFailed
        "pluginPackageTooLarge" -> R.string.pluginErrorPackageTooLarge
        "pluginPackageInvalid" -> R.string.pluginErrorPackageInvalid
        "marketVersionChanged" -> R.string.pluginErrorMarketVersionChanged
        else -> null
    }
    if (byReason != null) return context.str(byReason)
    return context.str(
        when (e.code) {
            HostErrorCode.InvalidArgument, HostErrorCode.NotFound -> R.string.localServiceInvalidArgument
            HostErrorCode.Conflict -> R.string.localServiceConflict
            HostErrorCode.Unsupported -> R.string.settingsUnsupportedOnPlatform
            else -> R.string.localServiceActionFailed
        },
    )
}

/** Only the structured diagnostic and localized reason/code fallback are safe to show or copy. */
private fun pluginFailureText(context: Context, error: HostException): String =
    pluginDiagnosticText(context, error.pluginDiagnostic) ?: extensionErrorText(context, error)

internal fun pluginLoadFailureText(context: Context, plugin: PluginDto): String =
    pluginDiagnosticText(context, plugin.loadDetail) ?: context.str(R.string.pluginDiagnosticLoadFailed)

private fun pluginDiagnosticText(context: Context, detail: PluginDiagnostic?): String? {
    if (detail == null) return null
    return when (detail.kind) {
        "manifestUnreadable" -> context.str(R.string.pluginDiagnosticManifestUnreadable)
        "manifestUnknownField" -> context.str(R.string.pluginDiagnosticManifestUnknownField)
        "manifestJsonInvalid" -> context.str(R.string.pluginDiagnosticManifestJsonInvalid)
        "manifestIdentityInvalid" -> context.str(R.string.pluginDiagnosticManifestIdentityInvalid)
        "manifestNameMissing" -> context.str(R.string.pluginDiagnosticManifestNameMissing)
        "manifestVersionInvalid" -> context.str(R.string.pluginDiagnosticManifestVersionInvalid)
        "manifestMinAppVersionInvalid" -> context.str(R.string.pluginDiagnosticManifestMinAppVersionInvalid)
        "manifestUnknownPermission" -> context.str(R.string.pluginDiagnosticManifestUnknownPermission)
        "manifestAuthPermissionMissing" -> context.str(R.string.pluginDiagnosticManifestAuthPermissionMissing)
        "scriptCompileFailed" -> context.str(R.string.pluginDiagnosticScriptCompileFailed)
        "scriptCompileFailedAt" -> {
            val line = detail.line
            val column = detail.column
            if (line == null || column == null) context.str(R.string.pluginDiagnosticScriptCompileFailed)
            else context.str(R.string.pluginDiagnosticScriptCompileFailedAt, "line" to line, "column" to column)
        }
        "missingFile" -> detail.path?.takeIf { it.isNotBlank() }?.let {
            context.str(R.string.pluginDiagnosticMissingFile, "path" to it)
        }
        "loadFailed" -> context.str(R.string.pluginDiagnosticLoadFailed)
        "notDevPlugin" -> context.str(R.string.pluginDiagnosticNotDevPlugin)
        else -> null
    }
}

// ───────────────────────────── 文案键映射 ─────────────────────────────

/** 撤回标记 → 文案（空 / 未知值为 null，不展示）。 */
@StringRes
internal fun yankedLabelRes(yanked: String): Int? = when (PluginMarket.yankedLabelKey(yanked)) {
    "marketYankedDeprecated" -> R.string.marketYankedDeprecated
    "marketYankedVulnerable" -> R.string.marketYankedVulnerable
    "marketYankedMalicious" -> R.string.marketYankedMalicious
    else -> null
}

/** 权限名称 / 说明文案；未知权限为 null（调用侧回退原名 + `pluginPermUnknownDesc`）。 */
internal class PermissionText(@StringRes val name: Int, @StringRes val desc: Int)

internal fun permissionText(permission: String): PermissionText? = when (PluginMarket.permissionKeys(permission)?.first) {
    "pluginPermFfmpegName" -> PermissionText(R.string.pluginPermFfmpegName, R.string.pluginPermFfmpegDesc)
    "pluginPermYtdlpName" -> PermissionText(R.string.pluginPermYtdlpName, R.string.pluginPermYtdlpDesc)
    "pluginPermAuthName" -> PermissionText(R.string.pluginPermAuthName, R.string.pluginPermAuthDesc)
    else -> null
}

@StringRes
internal fun componentTitleRes(kind: ComponentKind): Int? = when (kind) {
    ComponentKind.Ffmpeg -> R.string.componentsFfmpegTitle
    ComponentKind.Ytdlp -> R.string.componentsYtdlpTitle
    is ComponentKind.Unknown -> null
}

/** 组件标题；未知组件回退 wire 名（依赖提醒与组件页共用）。 */
internal fun Context.componentTitle(wire: String): String {
    val kind = ComponentKind.fromWire(wire)
    return componentTitleRes(kind)?.let { str(it) } ?: wire
}

// ───────────────────────────── 剪贴板 / 分享 / 链接 ─────────────────────────────

internal fun Context.copyPlainText(text: String) {
    getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText("", text))
}

/** 以系统分享面板分享纯文本；没有可处理的应用时静默忽略（面板本身不存在）。 */
internal fun Context.shareText(text: String, title: String) {
    val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
    try {
        startActivity(Intent.createChooser(send, title).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    } catch (_: ActivityNotFoundException) {
        // 没有任何分享目标：无需打扰用户
    }
}


// ───────────────────────────── 徽标流 ─────────────────────────────

/** 徽标文本 + 语气（图标可选）。 */
internal class Badge(val text: String, val tone: Tone, val icon: androidx.compose.ui.graphics.vector.ImageVector? = null)

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun BadgeFlow(badges: List<Badge>, modifier: Modifier = Modifier) {
    if (badges.isEmpty()) return
    FlowRow(
        modifier,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        for (b in badges) FluxTag(b.text, tone = b.tone, icon = b.icon)
    }
}

/** 权限行内容：名称 + 说明；未知权限原样显示名称并以琥珀色标注「未知权限」。 */
@Composable
internal fun PermissionBlock(permission: String, modifier: Modifier = Modifier) {
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val text = remember(permission) { permissionText(permission) }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        FluxText(if (text != null) str(text.name) else permission, style = t.body, color = c.ink)
        FluxText(
            str(text?.desc ?: R.string.pluginPermUnknownDesc),
            style = t.sm,
            color = if (text == null) c.amberText else c.inkMuted,
        )
    }
}

// ───────────────────────────── 挑战渲染（二维码 / data URL 图片） ─────────────────────────────

/** 可渲染的挑战：已解码位图，或本地编码的二维码矩阵。 */
internal sealed interface ChallengeVisual {
    class Bitmap(val image: ImageBitmap) : ChallengeVisual
    class Qr(val matrix: QrMatrix) : ChallengeVisual
}

private const val MAX_CHALLENGE_PIXELS = 16_000_000L

/**
 * 挑战不可信：只使用安全的 `data:image` 或本地编码的二维码，绝不把挑战 URL 当图片请求；
 * 其余返回 null（调用侧退化为截断文本 + 复制）。CPU 密集，请在后台调度。
 */
internal fun renderChallenge(value: String, type: String): ChallengeVisual? {
    val bytes = PluginAuth.dataImageBytes(value)
    if (bytes != null) return decodeBitmap(bytes)?.let { ChallengeVisual.Bitmap(it) }
    // data: 载荷无法解码为图片时不当二维码文本编码（避免把整段 base64 画成二维码）。
    if (value.lowercase(Locale.ROOT).startsWith("data:")) return null
    if (!PluginAuth.isQrcode(type)) return null
    if (value.isEmpty() || value.length > PluginAuth.MAX_QR_TEXT_LENGTH) return null
    return QrEncoder.encode(value)?.let { ChallengeVisual.Qr(it) }
}

private fun decodeBitmap(bytes: ByteArray): ImageBitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
    if (bounds.outWidth.toLong() * bounds.outHeight > MAX_CHALLENGE_PIXELS) return null
    return BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
}

/** 240dp 白底圆角框（二维码需要浅色底与静区才能被扫描，不随主题变化）。 */
@Composable
internal fun ChallengeImage(visual: ChallengeVisual, description: String, modifier: Modifier = Modifier) {
    Box(
        modifier
            .background(Color.White, RoundedCornerShape(20.dp))
            .padding(16.dp)
            .semantics { contentDescription = description },
    ) {
        when (visual) {
            is ChallengeVisual.Bitmap -> Image(
                bitmap = visual.image,
                contentDescription = null,
                modifier = Modifier.size(240.dp),
                filterQuality = FilterQuality.None,
            )
            is ChallengeVisual.Qr -> QrCanvas(visual.matrix, Modifier.size(240.dp))
        }
    }
}

/** 按整数像素格绘制二维码（四周各 4 个模块的静区）。 */
@Composable
private fun QrCanvas(matrix: QrMatrix, modifier: Modifier) {
    Canvas(modifier) {
        val total = matrix.size + 2 * QUIET_ZONE
        val cell = kotlin.math.floor(size.minDimension / total)
        val origin = (size.minDimension - cell * total) / 2f + cell * QUIET_ZONE
        for (y in 0 until matrix.size) {
            for (x in 0 until matrix.size) {
                if (matrix[x, y]) {
                    drawRect(Color.Black, Offset(origin + x * cell, origin + y * cell), Size(cell, cell))
                }
            }
        }
    }
}

private const val QUIET_ZONE = 4
