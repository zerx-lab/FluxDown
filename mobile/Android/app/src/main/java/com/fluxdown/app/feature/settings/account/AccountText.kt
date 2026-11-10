package com.fluxdown.app.feature.settings.account

import android.content.Context
import androidx.annotation.StringRes
import androidx.compose.runtime.Composable
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.core.host.HostException
import com.fluxdown.core.protocol.SyncStatusDto
import com.fluxdown.core.protocol.AccountErrorContext
import com.fluxdown.core.protocol.AccountRules
import com.fluxdown.core.protocol.SyncRules
import androidx.compose.ui.res.stringResource as composeString

/**
 * 账户 / 云同步文案：规则在 `:core` 的 [AccountRules] / [SyncRules]（只产出 i18n 键名），
 * 这里把键名映射到编译期的 `R.string`（不做运行期反射，缺键在编译期暴露）。
 */
internal object AccountText {
    /** 规则层可能产出的全部键（reason / code / 密码规则 / 会话撤销 / 同步 / 云连接）。 */
    private val keys: Map<String, Int> = mapOf(
        "accountErrorInvalidCredentials" to R.string.accountErrorInvalidCredentials,
        "accountErrorWrongPassword" to R.string.accountErrorWrongPassword,
        "accountSessionRevokedPasswordChanged" to R.string.accountSessionRevokedPasswordChanged,
        "accountErrorInvalidCode" to R.string.accountErrorInvalidCode,
        "accountErrorRateLimited" to R.string.accountErrorRateLimited,
        "accountErrorEmailTaken" to R.string.accountErrorEmailTaken,
        "accountOriginIdErrorTaken" to R.string.accountOriginIdErrorTaken,
        "accountOriginIdErrorNotAllowed" to R.string.accountOriginIdErrorNotAllowed,
        "accountOriginIdErrorAlreadyChanged" to R.string.accountOriginIdErrorAlreadyChanged,
        "accountErrorAccountDisabled" to R.string.accountErrorAccountDisabled,
        "accountErrorRegistrationClosed" to R.string.accountErrorRegistrationClosed,
        "accountErrorRegistrationIncomplete" to R.string.accountErrorRegistrationIncomplete,
        "errReasonMailNotConfigured" to R.string.errReasonMailNotConfigured,
        "accountErrorDeviceLimit" to R.string.accountErrorDeviceLimit,
        "cloudSyncErrorOnlineLimit" to R.string.cloudSyncErrorOnlineLimit,
        "cloudSyncErrorDeviceUntrusted" to R.string.cloudSyncErrorDeviceUntrusted,
        "accountErrorDeviceUntrusted" to R.string.accountErrorDeviceUntrusted,
        "errReasonSessionExpired" to R.string.errReasonSessionExpired,
        "cloudSyncErrorNetwork" to R.string.cloudSyncErrorNetwork,
        "accountErrorNetwork" to R.string.accountErrorNetwork,
        "errReasonPairingCodeInvalid" to R.string.errReasonPairingCodeInvalid,
        "errReasonPairingSessionExpired" to R.string.errReasonPairingSessionExpired,
        "errReasonPairingTimeout" to R.string.errReasonPairingTimeout,
        "errReasonPeerClockSkew" to R.string.errReasonPeerClockSkew,
        "errReasonPairingPeerUnreachable" to R.string.errReasonPairingPeerUnreachable,
        "errReasonPairingNotFluxDown" to R.string.errReasonPairingNotFluxDown,
        "errReasonPairingThrottled" to R.string.errReasonPairingThrottled,
        "errReasonPairingRejected" to R.string.errReasonPairingRejected,
        "errReasonPairingSignatureInvalid" to R.string.errReasonPairingSignatureInvalid,
        "errReasonPairingSelf" to R.string.errReasonPairingSelf,
        "errReasonPairingVersionMismatch" to R.string.errReasonPairingVersionMismatch,
        "errReasonPeerNotPaired" to R.string.errReasonPeerNotPaired,
        "errReasonPeerOffline" to R.string.errReasonPeerOffline,
        "errReasonPeerOutdated" to R.string.errReasonPeerOutdated,
        "errReasonLinkUnavailable" to R.string.errReasonLinkUnavailable,
        "errReasonTargetDeviceOffline" to R.string.errReasonTargetDeviceOffline,
        "errReasonTaskStateConflict" to R.string.errReasonTaskStateConflict,
        "errReasonTaskDeviceMismatch" to R.string.errReasonTaskDeviceMismatch,
        "errReasonSaveDirUnavailable" to R.string.errReasonSaveDirUnavailable,
        "accountErrorValidation" to R.string.accountErrorValidation,
        "localPairingAddressInvalid" to R.string.localPairingAddressInvalid,
        "localServiceDisconnected" to R.string.localServiceDisconnected,
        "localServiceInvalidArgument" to R.string.localServiceInvalidArgument,
        "localServiceConflict" to R.string.localServiceConflict,
        "settingsUnsupportedOnPlatform" to R.string.settingsUnsupportedOnPlatform,
        "localServiceActionFailed" to R.string.localServiceActionFailed,
        "accountErrorPasswordTooShort" to R.string.accountErrorPasswordTooShort,
        "accountPasswordMismatch" to R.string.accountPasswordMismatch,
        "accountPasswordSameAsCurrent" to R.string.accountPasswordSameAsCurrent,
        "accountSessionRevokedUntrusted" to R.string.accountSessionRevokedUntrusted,
        "accountSessionRevokedExpired" to R.string.accountSessionRevokedExpired,
        "cloudSyncErrorGeneric" to R.string.cloudSyncErrorGeneric,
        "cloudConnectionConnected" to R.string.cloudConnectionConnected,
        "cloudConnectionConnecting" to R.string.cloudConnectionConnecting,
        "cloudConnectionReconnecting" to R.string.cloudConnectionReconnecting,
        "cloudConnectionDisconnected" to R.string.cloudConnectionDisconnected,
    )

    /** 键 → 资源；未登记的键回退通用「操作失败」，不显示键名。 */
    @StringRes
    fun res(key: String): Int = keys[key] ?: R.string.localServiceActionFailed

    /** 账户场景错误文案：先按 `reason`（`ErrorReason` wire 名）、再按 `code` + [scene] 回退。 */
    fun error(context: Context, e: HostException, scene: AccountErrorContext = AccountErrorContext.General): String =
        context.getString(res(AccountRules.errorKey(e, scene)))

    /** 云同步失败 / 暂停原因（按 reason 本地化，未映射回退通用文案，不显示服务端诊断原文）。 */
    fun syncReason(context: Context, sync: SyncStatusDto): String = context.getString(res(SyncRules.reasonKey(sync)))

    /** 「已同步 · {time}」的相对时间。 */
    fun syncAgo(context: Context, ago: SyncRules.Ago): String = when (ago) {
        SyncRules.Ago.JustNow -> context.str(R.string.cloudSyncTimeJustNow)
        is SyncRules.Ago.Minutes -> context.str(R.string.cloudSyncTimeMinutesAgo, "n" to ago.n)
        is SyncRules.Ago.Hours -> context.str(R.string.cloudSyncTimeHoursAgo, "n" to ago.n)
        is SyncRules.Ago.Days -> context.str(R.string.cloudSyncTimeDaysAgo, "n" to ago.n)
    }
}

/** 组合内按键名取文案（规则层产出的键）。 */
@Composable
internal fun accountKeyText(key: String): String = composeString(AccountText.res(key))
