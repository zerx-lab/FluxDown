package com.fluxdown.app.feature.newtask

import android.content.Context
import com.fluxdown.app.R
import com.fluxdown.app.feature.settings.account.AccountText
import com.fluxdown.app.i18n.str
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.protocol.AccountErrorContext
import com.fluxdown.core.protocol.DispatchTarget
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.LinkDispatchParams
import com.fluxdown.core.protocol.RemoteDispatchParams
import com.fluxdown.core.protocol.callUnit

/** 下发的一条链接；[fileName] 为 null 时由目标设备按 URL 推断。 */
internal data class DispatchItem(val entry: UrlEntry, val fileName: String?)

/** 一次下发（可能多条链接）的结果汇总（对齐 GPUI `DispatchSummary` / iOS `DispatchSummary`）。 */
internal data class DispatchSummary(
    val successes: Int,
    val failures: Int,
    val firstError: HostException?,
    val failedEntries: List<UrlEntry>,
)

/** 下发结果的提示：[toast] 一次性提示；[failure] 非 null = 表单内联失败说明（失败的链接留在文本框里供重试）。 */
internal data class DispatchOutcome(val toast: String?, val partial: Boolean, val failure: String?)

/**
 * 逐条下发并汇总（同 iOS `Dispatcher`）：顺序执行，避免一次性压垮对端。
 * 目标只接收链接 / 文件名 / 保存目录；`saveDir` 为 null = 目标设备默认目录。
 */
internal object Dispatcher {
    suspend fun run(
        items: List<DispatchItem>,
        target: DispatchTarget,
        saveDir: String?,
        session: HostSession,
        isSessionCurrent: () -> Boolean,
        progress: (Int) -> Unit,
    ): DispatchSummary {
        var successes = 0
        var firstError: HostException? = null
        val failed = ArrayList<UrlEntry>()
        for ((index, item) in items.withIndex()) {
            // The main-thread session can change while the previous RPC is suspended.
            if (!isSessionCurrent()) {
                if (firstError == null) {
                    firstError = HostException(HostErrorCode.Unavailable, message = "dispatch session changed")
                }
                for (remaining in index until items.size) failed += items[remaining].entry
                break
            }
            try {
                dispatch(item, target, saveDir, session)
                successes++
            } catch (e: HostException) {
                failed += item.entry
                if (firstError == null) firstError = e
            }
            progress(index + 1)
        }
        return DispatchSummary(successes, failed.size, firstError, failed)
    }

    private suspend fun dispatch(item: DispatchItem, target: DispatchTarget, saveDir: String?, session: HostSession) {
        val url = item.entry.url
        // 独立直连条目 / 挂接且直连在线的云条目走局域网（一次性投递），其余走云
        val fingerprint = target.linkRouteFingerprint
        if (fingerprint != null) {
            session.callUnit(HostMethod.agentLinkDispatch, LinkDispatchParams(fingerprint, url, item.fileName, saveDir).toJson())
        } else {
            session.callUnit(HostMethod.agentRemoteDispatch, RemoteDispatchParams(target.deviceId, url, item.fileName, saveDir).toJson())
        }
    }

    /** 全部成功 → 成功提示；部分失败 → 警告提示 + 内联说明；全部失败 → 只给内联说明。 */
    fun outcome(context: Context, summary: DispatchSummary, target: DispatchTarget): DispatchOutcome {
        val device = target.name
        if (summary.failures == 0) {
            val text = when {
                // 直连是一次性投递：进度要到对端设备上看
                target.routesViaLink -> context.str(R.string.linkDispatchedToDevice, "device" to device)
                target.isCloud && target.isOffline ->
                    context.str(R.string.downloadToDispatchedOffline, "count" to summary.successes, "device" to device)
                summary.successes == 1 -> context.str(R.string.dispatchedToDevice, "device" to device)
                else -> context.str(R.string.downloadToDispatched, "count" to summary.successes, "device" to device)
            }
            return DispatchOutcome(text, partial = false, failure = null)
        }
        val scene = if (target.routesViaLink) AccountErrorContext.Pairing else AccountErrorContext.General
        val reason = summary.firstError?.let { AccountText.error(context, it, scene) } ?: context.str(R.string.dispatchFailed)
        if (summary.successes > 0) {
            val partial = context.str(
                R.string.downloadToPartial,
                "ok" to summary.successes,
                "failed" to summary.failures,
                "device" to device,
            )
            return DispatchOutcome(partial, partial = true, failure = "$partial\n$reason")
        }
        return DispatchOutcome(null, partial = false, failure = reason)
    }
}
