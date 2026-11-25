package com.fluxdown.app.feature.devices

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import com.fluxdown.app.AppContainer
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.model.HostRef
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogButtonStyle
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxHaptics
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * 局域网配对的主机被主机拒绝（设备控制凭据失效：对端已解除配对 / 主机重装 / 访问被撤销）：需要重新配对，
 * 不是「访问密钥错误」——这类主机没有可重新输入的访问密钥。
 */
internal fun HostRef.needsRepair(error: Throwable?): Boolean =
    this is HostRef.Remote && linkFingerprint != null && (error as? HostException)?.code == HostErrorCode.Unauthorized

/**
 * 把主机连接失败映射为用户可读文案：先按 code（Unauthorized / Timeout / Unavailable / ProtocolIncompatible），其余回退通用失败。
 * 传入失败的主机 [ref] 时，局域网配对主机的 Unauthorized 给「需要重新配对」。
 */
internal fun Context.hostErrorText(error: Throwable, ref: HostRef? = null): String = when {
    ref?.needsRepair(error) == true -> str(R.string.mobileHostRepairNeeded)
    else -> when ((error as? HostException)?.code) {
        HostErrorCode.Unauthorized -> str(R.string.webLoginInvalidKey)
        HostErrorCode.Timeout -> str(R.string.mobileHostErrTimeout)
        HostErrorCode.Unavailable -> str(R.string.webLoginUnreachable)
        HostErrorCode.ProtocolIncompatible -> str(R.string.mobileHostErrIncompatible)
        else -> str(R.string.localServiceActionFailed)
    }
}

/**
 * 主机切换 / 移除的唯一分发点（主机切换器与添加主机共用）：
 * 串行化切换、给出 toast 与触感；确认框只用于移除。
 */
@Stable
internal class HostActions(
    private val container: AppContainer,
    private val context: Context,
    private val scope: CoroutineScope,
    private val overlays: FluxOverlayState,
    private val haptics: FluxHaptics,
) {
    /** 正在切换到的主机 id；非空时其余切换请求被忽略。 */
    var switchingId: String? by mutableStateOf(null)
        private set

    /** 切换到 [ref]；已是当前主机则直接成功。[onDone] 带结果在主线程回调。 */
    fun switchTo(ref: HostRef, onDone: (Boolean) -> Unit = {}) {
        if (switchingId != null) {
            onDone(false)
            return
        }
        switchingId = ref.id
        val name = ref.displayNameFor(context)
        overlays.toast(context.str(R.string.mobileHostSwitching, "name" to name), FluxToastKind.Info, FluxIcons.Server)
        scope.launch {
            val result = try {
                container.switchHost(ref)
            } finally {
                switchingId = null
            }
            result.fold(
                onSuccess = {
                    haptics.confirm()
                    overlays.toast(context.str(R.string.mobileHostSwitched, "name" to name), FluxToastKind.Success, FluxIcons.Check)
                },
                onFailure = { e ->
                    if (e is CancellationException) throw e
                    haptics.reject()
                    overlays.toast(
                        context.str(R.string.mobileHostSwitchFailed, "name" to name, "reason" to context.hostErrorText(e, ref)),
                        FluxToastKind.Error,
                        FluxIcons.WifiOff,
                    )
                },
            )
            onDone(result.isSuccess)
        }
    }

    /** 移除已保存的远程主机（确认框；本机不可移除）。移除当前主机时 [AppContainer.removeHost] 会先回到本机。 */
    fun confirmRemove(ref: HostRef.Remote) {
        haptics.longPress()
        overlays.showDialog(
            FluxDialogSpec(
                title = context.str(R.string.mobileHostRemoveTitle, "name" to ref.displayName),
                message = context.str(R.string.mobileHostRemoveMessage),
                icon = FluxIcons.Trash2,
                buttons = listOf(
                    FluxDialogButton(context.str(R.string.cancel)),
                    FluxDialogButton(context.str(R.string.mobileHostRemoveConfirm), FluxDialogButtonStyle.Destructive) { remove(ref) },
                ),
            ),
        )
    }

    private fun remove(ref: HostRef.Remote) {
        scope.launch {
            container.removeHost(ref.id)
            haptics.confirm()
            overlays.toast(context.str(R.string.mobileHostRemoved, "name" to ref.displayName), FluxToastKind.Info, FluxIcons.Trash2)
        }
    }
}

/** 本机显示名随语言变化；远端沿用保存的名称。 */
private fun HostRef.displayNameFor(context: Context): String = when (this) {
    is HostRef.Local -> context.str(R.string.mobileHostLocalName)
    is HostRef.Remote -> displayName
}

@Composable
internal fun rememberHostActions(): HostActions {
    val container = LocalAppContainer.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val overlays = LocalFluxOverlays.current
    val haptics = FluxTheme.haptics
    return remember(container, context, scope, overlays, haptics) { HostActions(container, context, scope, overlays, haptics) }
}
