package com.fluxdown.core.protocol

import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import java.text.BreakIterator
import java.util.Locale
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min

/*
 * FluxCloud 账户（`agent.auth.*` / `agent.profile.*` / `agent.session.get`）的协议 DTO 与纯规则。
 * 镜像 `native/protocol/src/agent.rs`（同 iOS `Account.swift`、Web `pages/settings/sections/account` 目录下的 ts、
 * GPUI `crates/account/src/{errors,verification}.rs`）。
 * 计费 / 订单 / 推荐（`agent.plan|order|referral.*`）在移动端不出现（与 iOS 一致），因此不建模。
 */

// ───────────────────────────── 连接 / 用户 / 套餐 ─────────────────────────────

/** `CloudConnectionState`：任务连接（SSE + 心跳 + 设备名册）的生命周期；未知值原样保留。 */
sealed interface CloudConnectionState {
    val wire: String

    data object Disconnected : CloudConnectionState {
        override val wire = "disconnected"
    }

    data object Connecting : CloudConnectionState {
        override val wire = "connecting"
    }

    data object Connected : CloudConnectionState {
        override val wire = "connected"
    }

    data object Reconnecting : CloudConnectionState {
        override val wire = "reconnecting"
    }

    data class Unknown(override val wire: String) : CloudConnectionState

    companion object {
        fun fromWire(wire: String): CloudConnectionState = when (wire) {
            "disconnected" -> Disconnected
            "connecting" -> Connecting
            "connected" -> Connected
            "reconnecting" -> Reconnecting
            else -> Unknown(wire)
        }
    }
}

/** `agent.cloudConnection` 分区。[lastErrorReason] 是 `ErrorReason` 的 wire 名（原样保存，见 [AccountRules.errorKeyForReason]）。 */
data class CloudConnectionDto(
    val state: CloudConnectionState = CloudConnectionState.Disconnected,
    val lastError: String? = null,
    val lastErrorReason: String? = null,
) {
    companion object {
        /** 分区缺失 / JSON `null` / 非对象 → null。 */
        fun fromJson(v: JsonValue?): CloudConnectionDto? {
            if (v !is JsonValue.Obj) return null
            return CloudConnectionDto(
                state = v.strOrNull("state")?.let { CloudConnectionState.fromWire(it) } ?: CloudConnectionState.Disconnected,
                lastError = v.strOrNull("lastError"),
                lastErrorReason = v.strOrNull("lastErrorReason"),
            )
        }
    }
}

/** 云端在线状态是否可信：本地服务就绪且任务连接已建立（Web `cloud-presence.ts`）。 */
object CloudPresence {
    fun isKnown(connection: CloudConnectionDto?, localReady: Boolean): Boolean =
        localReady && connection?.state == CloudConnectionState.Connected

    /** 连接状态文案键。 */
    fun labelKey(connection: CloudConnectionDto?, localReady: Boolean): String {
        if (!localReady) return "localServiceDisconnected"
        return when (connection?.state) {
            CloudConnectionState.Connected -> "cloudConnectionConnected"
            CloudConnectionState.Connecting -> "cloudConnectionConnecting"
            CloudConnectionState.Reconnecting -> "cloudConnectionReconnecting"
            else -> "cloudConnectionDisconnected"
        }
    }

    /** 设备在线状态文案键（presence 未知时不显示在线 / 离线）。 */
    fun deviceKey(isOnline: Boolean, presenceKnown: Boolean): String =
        if (presenceKnown) (if (isOnline) "deviceOnline" else "deviceOffline") else "devicePresenceUnknown"
}

/** `CloudUserStatus`；未知值原样保留。 */
sealed interface CloudUserStatus {
    val wire: String

    data object Active : CloudUserStatus {
        override val wire = "active"
    }

    data object Disabled : CloudUserStatus {
        override val wire = "disabled"
    }

    data object Pending : CloudUserStatus {
        override val wire = "pending"
    }

    data class Unknown(override val wire: String) : CloudUserStatus

    companion object {
        fun fromWire(wire: String): CloudUserStatus = when (wire) {
            "active" -> Active
            "disabled" -> Disabled
            "pending" -> Pending
            else -> Unknown(wire)
        }
    }
}

/** FluxCloud 用户公开资料（`CloudUser`）。 */
data class CloudUser(
    val id: String,
    val email: String,
    val nickname: String = "",
    val plan: String = "",
    val status: CloudUserStatus = CloudUserStatus.Active,
    val createdAt: String = "",
    val lastLoginAt: String? = null,
    val originId: Long? = null,
    val originIdChanged: Boolean = false,
    val membershipOrdinal: Long? = null,
    /** 旧版云端不下发 → null（视为未知，按已设置处理）。 */
    val hasPassword: Boolean? = null,
) {
    companion object {
        /** `id` / `email` 缺失视为非法（null）；其余字段缺省宽松。 */
        fun fromJson(v: JsonValue?): CloudUser? {
            val id = v.strOrNull("id") ?: return null
            val email = v.strOrNull("email") ?: return null
            return CloudUser(
                id = id,
                email = email,
                nickname = v.str("nickname"),
                plan = v.str("plan"),
                status = v.strOrNull("status")?.let { CloudUserStatus.fromWire(it) } ?: CloudUserStatus.Active,
                createdAt = v.str("createdAt"),
                lastLoginAt = v.strOrNull("lastLoginAt"),
                originId = v.longOrNull("originId"),
                originIdChanged = v.bool("originIdChanged"),
                membershipOrdinal = v.longOrNull("membershipOrdinal"),
                hasPassword = v["hasPassword"].boolOrNull,
            )
        }
    }
}

/** 套餐徽标所需的 `CloudPlan` 子集（价格 / 活动 / 购买相关字段刻意不建模：移动端不出现购买流程）。 */
data class CloudPlan(
    val code: String,
    val name: String = "",
    val badge: String? = null,
    /** `outline | solid | medal | ribbon`，其它 = 纯文字。 */
    val badgeStyle: String = "outline",
    /** `RRGGBB` / `AARRGGBB`（可带 `#`）。 */
    val badgeColor: String = "",
    val badgeNumbered: Boolean = false,
    val badgeNumberDigits: Int = 4,
) {
    companion object {
        fun fromJson(v: JsonValue?): CloudPlan? {
            val code = v.strOrNull("code") ?: return null
            return CloudPlan(
                code = code,
                name = v.str("name"),
                badge = v.strOrNull("badge"),
                badgeStyle = v.str("badgeStyle", "outline"),
                badgeColor = v.str("badgeColor"),
                badgeNumbered = v.bool("badgeNumbered"),
                badgeNumberDigits = v.int("badgeNumberDigits", 4),
            )
        }
    }
}

/** `agent.session.device` / `agent.device.list` 元素：FluxCloud 受信任设备记录（`CloudDevice` 公开投影）。 */
data class SessionDeviceDto(
    /** 云端行 id（`agent.device.rename|delete` 用它；不是 [deviceId]）。 */
    val id: String = "",
    val deviceId: String = "",
    val name: String = "",
    val platform: String? = null,
    val createdAt: String = "",
    val lastSeenAt: String = "",
    val lastIp: String? = null,
    val appVersion: String? = null,
    val isOnline: Boolean = false,
    val isCurrent: Boolean = false,
    val defaultSaveDir: String? = null,
    val pathStyle: String? = null,
) {
    companion object {
        fun fromJson(v: JsonValue?): SessionDeviceDto = SessionDeviceDto(
            id = v.str("id"),
            deviceId = v.str("deviceId"),
            name = v.str("name"),
            platform = v.strOrNull("platform"),
            createdAt = v.str("createdAt"),
            lastSeenAt = v.str("lastSeenAt"),
            lastIp = v.strOrNull("lastIp"),
            appVersion = v.strOrNull("appVersion"),
            isOnline = v.bool("isOnline"),
            isCurrent = v.bool("isCurrent"),
            defaultSaveDir = v.strOrNull("defaultSaveDir"),
            pathStyle = v.strOrNull("pathStyle"),
        )

        /**
         * `agent.device.list` 结果（云端 `{ "devices": [...] }`；裸数组同样接受，同 agent `cloud_devices_from_value`）。
         * 缺云端行 id 或 deviceId 的元素无法改名 / 删除，跳过。
         */
        fun listFromJson(v: JsonValue?): List<SessionDeviceDto> =
            (v["devices"].arrayOrNull ?: v.arrayOrNull).orEmpty()
                .map(::fromJson)
                .filter { it.id.isNotEmpty() && it.deviceId.isNotEmpty() }
    }
}

/** `agent.session` 分区 / 登录结果里的无令牌会话视图。 */
data class AgentSessionDto(
    val user: CloudUser,
    /** 前向兼容的套餐权益集合（未知字段原样保留）。 */
    val entitlements: Map<String, JsonValue> = emptyMap(),
    val currentPlan: CloudPlan? = null,
    val device: SessionDeviceDto = SessionDeviceDto(),
) {
    companion object {
        /** 分区缺失 / JSON `null`（未登录）/ 形状非法 → null。 */
        fun fromJson(v: JsonValue?): AgentSessionDto? {
            if (v !is JsonValue.Obj) return null
            val user = CloudUser.fromJson(v["user"]) ?: return null
            return AgentSessionDto(
                user = user,
                entitlements = v["entitlements"].objOrNull.orEmpty(),
                currentPlan = CloudPlan.fromJson(v["currentPlan"]),
                device = SessionDeviceDto.fromJson(v["device"]),
            )
        }
    }
}

/** `agent.auth.login|loginVerify|register|registerVerify|verifyCode` 的结果（`status` 判别；未知判别不抛错）。 */
sealed interface AgentLoginResult {
    data class Ok(val session: AgentSessionDto) : AgentLoginResult

    data class DeviceVerificationRequired(val ttlSeconds: Long, val willReplaceDevices: Boolean) : AgentLoginResult

    data class Unknown(val status: String) : AgentLoginResult

    companion object {
        fun fromJson(v: JsonValue?): AgentLoginResult {
            val status = v.str("status")
            return when (status) {
                "ok" -> AgentSessionDto.fromJson(v["session"])?.let { Ok(it) } ?: Unknown(status)
                "deviceVerificationRequired" -> DeviceVerificationRequired(
                    ttlSeconds = v.long("ttlSeconds"),
                    willReplaceDevices = v.bool("willReplaceDevices"),
                )
                else -> Unknown(status)
            }
        }
    }
}

// ───────────────────────────── 参数 / 结果 ─────────────────────────────

data class RegisterParams(val email: String, val password: String, val nickname: String? = null) {
    fun toJson(): JsonValue = jsonObjectOmitNulls("email" to email, "password" to password, "nickname" to nickname)

    override fun toString(): String = "RegisterParams(email=$email, nickname=$nickname)"
}

data class RegisterVerifyParams(val email: String, val code: String) {
    fun toJson(): JsonValue = jsonObject("email" to email, "code" to code)
}

/** [account] 接受邮箱或纯数字 Origin ID。 */
data class LoginParams(val account: String, val password: String) {
    fun toJson(): JsonValue = jsonObject("account" to account, "password" to password)

    override fun toString(): String = "LoginParams(account=$account)"
}

data class LoginVerifyParams(val account: String, val password: String, val code: String) {
    fun toJson(): JsonValue = jsonObject("account" to account, "password" to password, "code" to code)

    override fun toString(): String = "LoginVerifyParams(account=$account)"
}

/** `agent.auth.sendCode` / `sendPasswordResetCode`。 */
data class SendCodeParams(val email: String) {
    fun toJson(): JsonValue = jsonObject("email" to email)
}

/** 验证码登录；邮箱不存在则自动注册（此时 [nickname] 生效）。 */
data class VerifyCodeParams(val email: String, val code: String, val nickname: String? = null) {
    fun toJson(): JsonValue = jsonObjectOmitNulls("email" to email, "code" to code, "nickname" to nickname)
}

/** 发送验证码类方法的结果。 */
data class TtlResult(val ttlSeconds: Long) {
    companion object {
        fun fromJson(v: JsonValue?): TtlResult = TtlResult(v.long("ttlSeconds"))
    }
}

data class SendNewEmailCodeParams(
    /** 新邮箱。 */
    val email: String,
    /** 原邮箱收到的验证码。 */
    val code: String,
) {
    fun toJson(): JsonValue = jsonObject("email" to email, "code" to code)
}

data class ChangeEmailParams(val email: String, val oldCode: String, val newCode: String) {
    fun toJson(): JsonValue = jsonObject("email" to email, "oldCode" to oldCode, "newCode" to newCode)
}

data class RandomOriginIdResult(val originId: Long) {
    companion object {
        fun fromJson(v: JsonValue?): RandomOriginIdResult = RandomOriginIdResult(v.long("originId"))
    }
}

data class CheckOriginIdParams(val value: Long) {
    fun toJson(): JsonValue = jsonObject("value" to value)
}

data class OriginIdCheckResult(
    val available: Boolean,
    /** `invalid` = 格式不合法；其它 = 已被占用等。 */
    val reason: String? = null,
) {
    companion object {
        fun fromJson(v: JsonValue?): OriginIdCheckResult =
            OriginIdCheckResult(available = v.bool("available"), reason = v.strOrNull("reason"))
    }
}

data class ChangeOriginIdParams(
    /** ≥ 10000，全局仅可成功一次。 */
    val originId: Long,
) {
    fun toJson(): JsonValue = jsonObject("originId" to originId)
}

data class ChangeNicknameParams(
    /** 1–32 字符（服务端 trim 后校验）。 */
    val nickname: String,
) {
    fun toJson(): JsonValue = jsonObject("nickname" to nickname)
}

/** 已登录修改 / 设置密码：[currentPassword] 与 [code]（`sendPasswordCode` 发到绑定邮箱）二选一。 */
data class ChangePasswordParams(
    val newPassword: String,
    val currentPassword: String? = null,
    val code: String? = null,
) {
    fun toJson(): JsonValue = jsonObjectOmitNulls(
        "newPassword" to newPassword,
        "currentPassword" to currentPassword,
        "code" to code,
    )

    override fun toString(): String = "ChangePasswordParams(byCode=${code != null})"
}

/** 未登录重置密码：`sendPasswordResetCode` 发到该邮箱的验证码 + 新密码。 */
data class ResetPasswordParams(val email: String, val code: String, val newPassword: String) {
    fun toJson(): JsonValue = jsonObject("email" to email, "code" to code, "newPassword" to newPassword)

    override fun toString(): String = "ResetPasswordParams(email=$email)"
}

/** `agent.cloud.endpointGet` 结果：FluxCloud 服务地址；正式构建 `editable=false`（地址恒为构建期固定值）。 */
data class CloudEndpointDto(val baseUrl: String, val defaultBaseUrl: String, val editable: Boolean) {
    companion object {
        fun fromJson(v: JsonValue?): CloudEndpointDto = CloudEndpointDto(
            baseUrl = v.str("baseUrl"),
            defaultBaseUrl = v.str("defaultBaseUrl"),
            editable = v.bool("editable"),
        )
    }
}

/** `agent.cloud.endpointSet` 参数；[baseUrl] 为空即恢复默认地址。 */
data class CloudEndpointSetParams(val baseUrl: String) {
    fun toJson(): JsonValue = jsonObject("baseUrl" to baseUrl)
}

// ───────────────────────────── 纯规则 ─────────────────────────────

/**
 * 账户错误出现的位置：只在没有 `reason`（旧 agent / 非云端错误）时决定按 `code` 回退的措辞
 * （GPUI `errors.rs::ErrorContext`，Web `errorText.ts::AccountErrorContext`）。
 */
enum class AccountErrorContext {
    General, Login, Register,

    /** 设备验证码 / 邮箱验证码校验。 */
    Code,

    /** 配置同步。 */
    Sync,

    /** 局域网直连配对与已配对设备操作。 */
    Pairing,
}

/** 验证码倒计时：有效期（[ttl] 秒）与重发冷却（发码后前 60 秒，见 [AccountRules.resendCooldown]）。 */
data class CodeClock(val ttl: Long, val sentAtMs: Long) {
    private fun elapsedSeconds(nowMs: Long): Double = max(0L, nowMs - sentAtMs) / 1000.0

    /** 验证码剩余有效秒数（≥ 0）。 */
    fun remaining(nowMs: Long): Long = max(0L, ceil(ttl - elapsedSeconds(nowMs)).toLong())

    /** 重发冷却剩余秒数（0 = 可重发）。 */
    fun cooldown(nowMs: Long): Long = AccountRules.resendCooldown(remaining(nowMs), ttl)
}

/** 账户 / 设备页的表单与文案规则（无 UI 依赖，可单测）。 */
object AccountRules {
    const val NICKNAME_MAX_LENGTH = 32
    const val DEVICE_NAME_MAX_LENGTH = 64
    const val MIN_PASSWORD_LENGTH = 8
    const val MIN_ORIGIN_ID = 10000L

    /** 验证码重发冷却（云端对同一用途重发的最短间隔，秒）。 */
    const val RESEND_COOLDOWN_SECONDS = 60

    /** 与 Web `Number.isSafeInteger` 同界：超出 2^53-1 的值服务端也无法精确表示。 */
    private const val MAX_SAFE_ORIGIN_ID = 9_007_199_254_740_991L

    // ── 昵称 / 设备名 ──

    /** Unicode `White_Space`（同 Rust `char::is_whitespace`，服务端 `str::trim` 所用）。 */
    private fun isWhiteSpace(ch: Char): Boolean = when (ch.code) {
        in 0x09..0x0D, 0x20, 0x85, 0xA0, 0x1680, in 0x2000..0x200A, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000 -> true
        else -> false
    }

    /** Rust `str::trim`：只裁两端的 Unicode 空白。 */
    fun trimmed(value: String): String {
        var start = 0
        var end = value.length
        while (start < end && isWhiteSpace(value[start])) start++
        while (end > start && isWhiteSpace(value[end - 1])) end--
        return value.substring(start, end)
    }

    private fun scalarCount(value: String): Int = value.codePointCount(0, value.length)

    /** 昵称按 Unicode 标量计数（与服务端 `chars().count()` 一致），trim 后 1–32。 */
    fun isValidNickname(value: String): Boolean = scalarCount(trimmed(value)) in 1..NICKNAME_MAX_LENGTH

    fun isValidDeviceName(value: String): Boolean = scalarCount(trimmed(value)) in 1..DEVICE_NAME_MAX_LENGTH

    /** 显示名：昵称，为空取邮箱 @ 前部分。 */
    fun displayName(user: CloudUser): String {
        val nickname = trimmed(user.nickname)
        if (nickname.isNotEmpty()) return nickname
        return user.email.substringBefore('@')
    }

    /** 头像字符：显示名首个字符（字形簇）大写；空串 = null。 */
    fun avatarInitial(user: CloudUser): String? {
        val name = trimmed(displayName(user))
        if (name.isEmpty()) return null
        val iterator = BreakIterator.getCharacterInstance()
        iterator.setText(name)
        val end = iterator.next()
        return name.substring(0, if (end == BreakIterator.DONE) name.length else end).uppercase(Locale.ROOT)
    }

    // ── Origin ID ──

    /** Origin ID 一次性修改：权益 `originIdEdit == true` 且尚未改过。 */
    fun canEditOriginId(session: AgentSessionDto): Boolean =
        session.entitlements["originIdEdit"].boolOrNull == true && !session.user.originIdChanged

    /** 仅数字、≥ 10000 的 Origin ID；非法为 null。 */
    fun parseOriginId(value: String): Long? {
        val text = trimmed(value)
        if (text.isEmpty() || !text.all { it in '0'..'9' }) return null
        val parsed = text.toLongOrNull() ?: return null
        return parsed.takeIf { it in MIN_ORIGIN_ID..MAX_SAFE_ORIGIN_ID }
    }

    // ── 邮箱 / 密码 ──

    /** `^[^\s@]+@[^\s@]+\.[^\s@]+$`（Web `EMAIL_PATTERN`）。 */
    fun isValidEmail(value: String): Boolean {
        val text = trimmed(value)
        val parts = text.split('@')
        if (parts.size != 2 || parts[0].isEmpty() || parts[1].isEmpty()) return false
        if (text.any { isWhiteSpace(it) }) return false
        // `[^\s@]+\.[^\s@]+`：域名里至少有一个不在首尾的点。
        val domain = parts[1]
        return domain.length >= 3 && domain.substring(1, domain.length - 1).contains('.')
    }

    /** 密码长度按 Unicode 码点计。 */
    fun passwordLength(value: String): Int = scalarCount(value)

    /** 校验新密码，返回第一个违反规则的文案键；通过为 null。[current] 仅「当前密码模式」传入，用于拒绝新旧相同。 */
    fun newPasswordErrorKey(new: String, confirm: String, current: String?): String? = when {
        passwordLength(new) < MIN_PASSWORD_LENGTH -> "accountErrorPasswordTooShort"
        new != confirm -> "accountPasswordMismatch"
        current != null && new == current -> "accountPasswordSameAsCurrent"
        else -> null
    }

    // ── 验证码倒计时 ──

    /** 已发出验证码的记录（跨对话框实例恢复倒计时，云端 60s 限频、码仍有效）。 */
    data class SentCode(
        /** 发码时刻（Unix 毫秒）。 */
        val sentAtMs: Long,
        val ttlSeconds: Long,
    )

    data class RestoredCode(
        val ttlSeconds: Long,
        /** 验证码剩余有效秒数（≥ 0）。 */
        val remaining: Long,
        /** 重发冷却剩余秒数（0 = 可重发）。 */
        val cooldown: Long,
    )

    /** 按记录恢复倒计时；有效期与冷却都已归零返回 null。时钟回拨按未过去处理。 */
    fun restore(record: SentCode, nowMs: Long): RestoredCode? {
        val elapsed = max(0L, nowMs - record.sentAtMs) / 1000.0
        val remaining = ceil(record.ttlSeconds - elapsed).toLong()
        val cooldown = ceil(min(RESEND_COOLDOWN_SECONDS.toLong(), record.ttlSeconds) - elapsed).toLong()
        if (remaining <= 0 && cooldown <= 0) return null
        return RestoredCode(record.ttlSeconds, max(0L, remaining), max(0L, cooldown))
    }

    /** 重发冷却剩余秒数：验证码剩余有效期里，发码后前 60 秒内不可重发。 */
    fun resendCooldown(remaining: Long, ttlSeconds: Long): Long =
        max(0L, remaining - max(0L, ttlSeconds - RESEND_COOLDOWN_SECONDS))

    // ── 错误 → 文案键 ──

    /** 先按 `reason`（agent 已把云端错误码归一化），缺失 / 未映射再按 `code` 与上下文回退。 */
    fun errorKey(error: HostException, context: AccountErrorContext = AccountErrorContext.General): String {
        error.reason?.let { reason -> errorKeyForReason(reason, context)?.let { return it } }
        return codeKey(error, context)
    }

    /** `reason` wire 名 → 文案键；未映射（含 `unknown` 与插件市场 / Doctor / 网关类）为 null。 */
    fun errorKeyForReason(reason: String, context: AccountErrorContext = AccountErrorContext.General): String? = when (reason) {
        "invalidCredentials" -> "accountErrorInvalidCredentials"
        "wrongPassword" -> "accountErrorWrongPassword"
        "passwordChanged" -> "accountSessionRevokedPasswordChanged"
        "invalidVerificationCode" -> "accountErrorInvalidCode"
        "rateLimited" -> "accountErrorRateLimited"
        "emailTaken" -> "accountErrorEmailTaken"
        "originIdTaken" -> "accountOriginIdErrorTaken"
        "originIdChangeNotAllowed" -> "accountOriginIdErrorNotAllowed"
        "originIdAlreadyChanged" -> "accountOriginIdErrorAlreadyChanged"
        "accountDisabled" -> "accountErrorAccountDisabled"
        "registrationClosed" -> "accountErrorRegistrationClosed"
        "registrationIncomplete" -> "accountErrorRegistrationIncomplete"
        "mailNotConfigured" -> "errReasonMailNotConfigured"
        "deviceLimit" -> "accountErrorDeviceLimit"
        "onlineDeviceLimit" -> "cloudSyncErrorOnlineLimit"
        "deviceUntrusted" -> if (context == AccountErrorContext.Sync) "cloudSyncErrorDeviceUntrusted" else "accountErrorDeviceUntrusted"
        "sessionExpired" -> "errReasonSessionExpired"
        "cloudUnreachable" -> if (context == AccountErrorContext.Sync) "cloudSyncErrorNetwork" else "accountErrorNetwork"
        "pairingCodeInvalid" -> "errReasonPairingCodeInvalid"
        "pairingSessionExpired" -> "errReasonPairingSessionExpired"
        "pairingTimeout" -> "errReasonPairingTimeout"
        "peerClockSkew" -> "errReasonPeerClockSkew"
        "pairingPeerUnreachable" -> "errReasonPairingPeerUnreachable"
        "pairingNotFluxDown" -> "errReasonPairingNotFluxDown"
        "pairingThrottled" -> "errReasonPairingThrottled"
        "pairingRejected" -> "errReasonPairingRejected"
        "pairingSignatureInvalid" -> "errReasonPairingSignatureInvalid"
        "pairingSelf" -> "errReasonPairingSelf"
        "pairingVersionMismatch" -> "errReasonPairingVersionMismatch"
        "peerNotPaired" -> "errReasonPeerNotPaired"
        "peerOffline" -> "errReasonPeerOffline"
        "peerOutdated" -> "errReasonPeerOutdated"
        "linkUnavailable" -> "errReasonLinkUnavailable"
        "targetDeviceOffline" -> "errReasonTargetDeviceOffline"
        "taskStateConflict" -> "errReasonTaskStateConflict"
        "taskDeviceMismatch" -> "errReasonTaskDeviceMismatch"
        "saveDirUnavailable" -> "errReasonSaveDirUnavailable"
        else -> null
    }

    private fun codeKey(error: HostException, context: AccountErrorContext): String {
        val network = error.code == HostErrorCode.Unavailable && error.retryable
        when (context) {
            AccountErrorContext.Login -> when (error.code) {
                HostErrorCode.Unauthorized -> return "accountErrorInvalidCredentials"
                HostErrorCode.InvalidArgument -> return "accountErrorValidation"
                HostErrorCode.Conflict -> return "accountErrorRegistrationIncomplete"
                else -> if (network) return "accountErrorNetwork"
            }
            AccountErrorContext.Register -> when (error.code) {
                HostErrorCode.Conflict -> return "accountErrorEmailTaken"
                HostErrorCode.InvalidArgument -> return "accountErrorValidation"
                HostErrorCode.Unsupported -> return "accountErrorRegistrationClosed"
                else -> if (network) return "accountErrorNetwork"
            }
            AccountErrorContext.Code -> when (error.code) {
                HostErrorCode.Unauthorized, HostErrorCode.InvalidArgument, HostErrorCode.NotFound -> return "accountErrorInvalidCode"
                else -> if (network) return "accountErrorNetwork"
            }
            AccountErrorContext.Sync -> if (network) return "cloudSyncErrorNetwork"
            // 不带 reason 的 Unavailable 是本机服务 / 连接断开，不是「无法连接对端地址」（对端不可达由 reason pairingPeerUnreachable 表达）。
            AccountErrorContext.Pairing -> if (error.code == HostErrorCode.InvalidArgument) return "localPairingAddressInvalid"
            AccountErrorContext.General -> Unit
        }
        return genericKey(error.code)
    }

    private fun genericKey(code: HostErrorCode): String = when (code) {
        HostErrorCode.Unavailable, HostErrorCode.Timeout -> "localServiceDisconnected"
        HostErrorCode.InvalidArgument, HostErrorCode.NotFound -> "localServiceInvalidArgument"
        HostErrorCode.Conflict -> "localServiceConflict"
        HostErrorCode.Unsupported -> "settingsUnsupportedOnPlatform"
        HostErrorCode.ProtocolIncompatible, HostErrorCode.Unauthorized, HostErrorCode.Cancelled, HostErrorCode.Internal ->
            "localServiceActionFailed"
    }

    /** 服务端要求先完成注册验证：登录流程据此引导到注册验证步骤。 */
    fun isRegistrationIncomplete(error: HostException): Boolean = when (error.reason) {
        "registrationIncomplete" -> true
        // 旧 agent 不带 reason，云端 `registration_incomplete` 折成 `conflict`。
        null -> error.code == HostErrorCode.Conflict
        else -> false
    }

    /** `sessionRevoked` 通知（载荷 = `ErrorReason` wire 名）→ 一次性提示文案键。 */
    fun sessionRevokedKey(reason: String): String = when (reason) {
        "deviceUntrusted" -> "accountSessionRevokedUntrusted"
        "passwordChanged" -> "accountSessionRevokedPasswordChanged"
        "accountDisabled" -> "accountErrorAccountDisabled"
        else -> "accountSessionRevokedExpired"
    }

    // ── 套餐徽标 ──

    /** `{badge}` 或 `{badge} No.{序号补零到 digits(1…6)}`；badge 为空 = null（不显示）。 */
    fun planBadgeText(plan: CloudPlan, ordinal: Long?): String? {
        val base = plan.badge?.let { trimmed(it) }?.takeIf { it.isNotEmpty() } ?: return null
        if (!plan.badgeNumbered || ordinal == null) return base
        val width = plan.badgeNumberDigits.coerceIn(1, 6)
        return "$base No.${ordinal.toString().padStart(width, '0')}"
    }

    /** 徽标颜色：接受可选 `#` 前缀的 6 位 `RRGGBB` 或 8 位 `AARRGGBB`；非法 null。分量 0…1。 */
    data class BadgeColor(val red: Double, val green: Double, val blue: Double, val alpha: Double)

    fun parseBadgeColor(value: String): BadgeColor? {
        val hex = trimmed(value).removePrefix("#")
        if (hex.length != 6 && hex.length != 8) return null
        if (!hex.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }) return null
        val number = hex.toLong(16)
        val alpha = if (hex.length == 8) ((number shr 24) and 0xFF) / 255.0 else 1.0
        return BadgeColor(
            red = ((number shr 16) and 0xFF) / 255.0,
            green = ((number shr 8) and 0xFF) / 255.0,
            blue = (number and 0xFF) / 255.0,
            alpha = alpha,
        )
    }
}
