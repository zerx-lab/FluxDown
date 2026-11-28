package com.fluxdown.app.feature.settings

import android.content.Context
import com.fluxdown.app.AppContainer
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.protocol.DaemonConfigPort
import com.fluxdown.core.protocol.DaemonConfigSnapshot
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.callJson

/**
 * 整键 JSON 数组配置（`webhook.endpoints`、`proxy_rules`）写入器的生产端口：每次写入前都 `daemon.config.get`
 * 取最新值与 revision（[cachedConfig] 恒为 null，不信任 store 快照——发布有合帧延迟），
 * 再 `daemon.config.patch {expectedRevision}`。
 */
internal class ContainerDaemonConfigPort(private val session: () -> HostSession) : DaemonConfigPort {
    constructor(container: AppContainer) : this({ container.session })

    override val cachedConfig: DaemonConfigSnapshot? = null

    override suspend fun fetchConfig(): DaemonConfigSnapshot =
        DaemonConfigSnapshot.fromJson(session().callJson(HostMethod.daemonConfigGet))

    override suspend fun patchConfig(expectedRevision: Long, values: Map<String, String>) {
        session().patchConfig(expectedRevision, values)
    }
}

/** 主机给出的细节（没有则空串）。`HostException.message` 缺省为 code 名，视为没有细节。 */
internal fun daemonWriteDetail(e: HostException): String = e.message.orEmpty().takeIf { it != e.code.name }.orEmpty()

/** 写回失败 → 反馈文案（Web `writeErrorText` ↔ GPUI `SettingsErrorKind`；校验失败附带具体原因）。 */
internal fun daemonWriteErrorText(context: Context, e: HostException): String = when (e.code) {
    HostErrorCode.Unavailable, HostErrorCode.Timeout -> context.str(R.string.localServiceDisconnected)
    HostErrorCode.Conflict -> context.str(R.string.localServiceConflict)
    HostErrorCode.InvalidArgument -> {
        val detail = daemonWriteDetail(e)
        val base = context.str(R.string.localServiceInvalidArgument)
        if (detail.isEmpty()) base else "$base: $detail"
    }
    else -> context.str(R.string.localServiceActionFailed)
}
