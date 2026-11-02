package com.fluxdown.core.protocol

import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** 账户 DTO 解码（真实 serde 形状）+ 昵称 / Origin ID / 密码 / 错误映射等纯规则（同 iOS `AccountProtocolTests`）。 */
class AccountTest {
    private val sessionJson = """
        {"user":{"id":"u1","email":"zero@example.com","nickname":"Zero","plan":"pro","status":"active",
          "createdAt":"2025-01-02T03:04:05Z","lastLoginAt":null,"originId":12345,"originIdChanged":false,
          "membershipOrdinal":7,"hasPassword":true,"futureField":1},
         "entitlements":{"originIdEdit":true,"maxDevices":5,"tags":["a"]},
         "currentPlan":{"code":"pro","name":"Pro","description":"x","badge":"PRO","icon":"crown","color":"#fff",
           "badgeStyle":"medal","badgeColor":"FFAA00","badgeNumbered":true,"badgeNumberDigits":4,"priceMinor":100,
           "currency":"CNY","highlights":[],"entitlements":{},"sort":1,"campaign":null,"purchasable":true},
         "device":{"id":"row1","deviceId":"dev-1","name":"Pixel","platform":"android","createdAt":"2025-01-02T03:04:05Z",
           "lastSeenAt":"2025-02-02T03:04:05Z","lastIp":null,"appVersion":"1.0.0","isOnline":true,"isCurrent":true,
           "defaultSaveDir":"/sdcard/Download","pathStyle":"posix"}}
    """.trimIndent()

    private fun session(): AgentSessionDto = requireNotNull(AgentSessionDto.fromJson(Json.parse(sessionJson)))

    private fun err(code: HostErrorCode, reason: String? = null, retryable: Boolean = false) =
        HostException(code, reason = reason, retryable = retryable)

    // ── 会话 ──

    @Test
    fun sessionDecodesWithPlanEntitlementsAndDeviceAndIgnoresUnknownKeys() {
        val s = session()
        assertEquals("u1", s.user.id)
        assertEquals(12345L, s.user.originId)
        assertEquals(true, s.user.hasPassword)
        assertEquals(CloudUserStatus.Active, s.user.status)
        assertEquals(JsonValue.True, s.entitlements["originIdEdit"])
        assertEquals(5, s.entitlements["maxDevices"].intOrNull)
        assertEquals("medal", s.currentPlan?.badgeStyle)
        assertEquals("row1", s.device.id)
        assertEquals("dev-1", s.device.deviceId)
        assertEquals("posix", s.device.pathStyle)
        assertTrue(AccountRules.canEditOriginId(s))
        assertEquals("PRO No.0007", AccountRules.planBadgeText(s.currentPlan!!, s.user.membershipOrdinal))
    }

    @Test
    fun loggedOutSectionIsNullAndMissingOptionalsDefault() {
        assertNull(AgentSessionDto.fromJson(Json.parse("null")))
        assertNull(AgentSessionDto.fromJson(null))
        // 缺 `user.id` / `email` 的形状视为未登录，而不是半成品会话。
        assertNull(AgentSessionDto.fromJson(Json.parse("""{"user":{"email":"a@b.co"}}""")))

        val user = requireNotNull(CloudUser.fromJson(Json.parse("""{"id":"u","email":"a@b.co"}""")))
        assertEquals("", user.nickname)
        assertEquals(CloudUserStatus.Active, user.status)
        assertFalse(user.originIdChanged)
        assertNull(user.hasPassword)
        val unknown = requireNotNull(CloudUser.fromJson(Json.parse("""{"id":"u","email":"a@b.co","status":"frozen"}""")))
        assertEquals(CloudUserStatus.Unknown("frozen"), unknown.status)
    }

    @Test
    fun deviceListDecodesCloudShapeAndSkipsUndeletableRows() {
        val wrapped = SessionDeviceDto.listFromJson(
            Json.parse(
                """{"devices":[
                  {"id":"row1","deviceId":"dev-1","name":"PC","isOnline":false,"isCurrent":false},
                  {"deviceId":"dev-2","name":"no row id"},
                  {"id":"row3","name":"no device id"},
                  {"id":"row4","deviceId":"dev-4","name":"Phone","isCurrent":true}]}""",
            ),
        )
        // 删除用云端行 id 而不是 deviceId；缺任一个的记录无法定位，不能出现在结果里。
        assertEquals(listOf("row1" to "dev-1", "row4" to "dev-4"), wrapped.map { it.id to it.deviceId })
        assertTrue(wrapped[1].isCurrent)
        // agent 也接受裸数组；非对象 / 缺失 = 空。
        assertEquals(listOf("row1"), SessionDeviceDto.listFromJson(Json.parse("""[{"id":"row1","deviceId":"dev-1"}]""")).map { it.id })
        assertTrue(SessionDeviceDto.listFromJson(Json.parse("""{"ok":true}""")).isEmpty())
        assertTrue(SessionDeviceDto.listFromJson(null).isEmpty())
    }

    @Test
    fun loginResultDecodesEveryStatusLeniently() {
        val ok = AgentLoginResult.fromJson(Json.parse("""{"status":"ok","session":$sessionJson}"""))
        assertTrue(ok is AgentLoginResult.Ok)
        assertEquals("zero@example.com", (ok as AgentLoginResult.Ok).session.user.email)

        assertEquals(
            AgentLoginResult.DeviceVerificationRequired(300, true),
            AgentLoginResult.fromJson(Json.parse("""{"status":"deviceVerificationRequired","ttlSeconds":300,"willReplaceDevices":true}""")),
        )
        assertEquals(
            AgentLoginResult.DeviceVerificationRequired(60, false),
            AgentLoginResult.fromJson(Json.parse("""{"status":"deviceVerificationRequired","ttlSeconds":60}""")),
        )
        assertEquals(AgentLoginResult.Unknown("mfaRequired"), AgentLoginResult.fromJson(Json.parse("""{"status":"mfaRequired"}""")))
        // `ok` 缺会话不能当作登录成功。
        assertEquals(AgentLoginResult.Unknown("ok"), AgentLoginResult.fromJson(Json.parse("""{"status":"ok"}""")))
    }

    @Test
    fun cloudConnectionAndParamsMatchWireShape() {
        val connection = requireNotNull(
            CloudConnectionDto.fromJson(Json.parse("""{"state":"reconnecting","lastError":"boom","lastErrorReason":"cloudUnreachable"}""")),
        )
        assertEquals(CloudConnectionState.Reconnecting, connection.state)
        assertEquals("cloudUnreachable", connection.lastErrorReason)
        assertEquals(
            CloudConnectionState.Unknown("warping"),
            CloudConnectionDto.fromJson(Json.parse("""{"state":"warping"}"""))?.state,
        )
        assertNull(CloudConnectionDto.fromJson(Json.parse("null")))

        // 可选字段省略；键名 camelCase。
        assertEquals(Json.parse("""{"email":"a@b.co","password":"pw"}"""), RegisterParams("a@b.co", "pw").toJson())
        assertEquals(
            Json.parse("""{"email":"a@b.co","password":"pw","nickname":"Zero"}"""),
            RegisterParams("a@b.co", "pw", "Zero").toJson(),
        )
        assertEquals(
            Json.parse("""{"email":"n@b.co","oldCode":"1","newCode":"2"}"""),
            ChangeEmailParams("n@b.co", "1", "2").toJson(),
        )
        assertEquals(Json.parse("""{"newPassword":"n","code":"9"}"""), ChangePasswordParams("n", code = "9").toJson())
        assertEquals(
            Json.parse("""{"newPassword":"n","currentPassword":"c"}"""),
            ChangePasswordParams("n", currentPassword = "c").toJson(),
        )
        assertEquals(Json.parse("""{"originId":10001}"""), ChangeOriginIdParams(10001).toJson())
        assertEquals(Json.parse("""{"value":10001}"""), CheckOriginIdParams(10001).toJson())
        assertEquals(Json.parse("""{"account":"12345","password":"p"}"""), LoginParams("12345", "p").toJson())
        assertEquals(300L, TtlResult.fromJson(Json.parse("""{"ttlSeconds":300}""")).ttlSeconds)
        assertEquals("invalid", OriginIdCheckResult.fromJson(Json.parse("""{"available":false,"reason":"invalid"}""")).reason)
        assertTrue(CloudEndpointDto.fromJson(Json.parse("""{"baseUrl":"a","defaultBaseUrl":"b","editable":true}""")).editable)
    }

    @Test
    fun secretBearingParamsNeverPrintPasswordsOrCodes() {
        val text = listOf(
            RegisterParams("a@b.co", "s3cret").toString(),
            LoginParams("a@b.co", "s3cret").toString(),
            LoginVerifyParams("a@b.co", "s3cret", "123456").toString(),
            ChangePasswordParams("s3cret", currentPassword = "old-s3cret").toString(),
            ResetPasswordParams("a@b.co", "123456", "s3cret").toString(),
        )
        text.forEach { assertFalse(it, it.contains("s3cret")) }
    }

    // ── 昵称 / Origin ID / 邮箱 / 密码 ──

    @Test
    fun nicknameIsTrimmedAndCountedInUnicodeScalars() {
        assertTrue(AccountRules.isValidNickname("Zero"))
        assertTrue(AccountRules.isValidNickname("  Zero \n"))
        assertFalse(AccountRules.isValidNickname(""))
        assertFalse(AccountRules.isValidNickname("   \t\u3000"))
        assertTrue(AccountRules.isValidNickname("a".repeat(32)))
        assertFalse(AccountRules.isValidNickname("a".repeat(33)))
        // 32 个码点（含 emoji，每个 2 个 UTF-16 单元）仍合法；33 个不合法；空白只在两端裁剪。
        assertTrue(AccountRules.isValidNickname("😀".repeat(32)))
        assertFalse(AccountRules.isValidNickname("😀".repeat(33)))
        assertTrue(AccountRules.isValidNickname("a b"))
        assertEquals("x", AccountRules.trimmed(" \u00A0x\u2003"))
        // 服务端 `str::trim` 也会裁掉 NEL（U+0085）。
        assertEquals("x", AccountRules.trimmed("\u0085x\u0085"))
        // 零宽空格不是 White_Space，不裁。
        assertEquals("\u200Bx", AccountRules.trimmed("\u200Bx"))
    }

    @Test
    fun deviceNameRuleIs1To64() {
        assertTrue(AccountRules.isValidDeviceName("Pixel"))
        assertFalse(AccountRules.isValidDeviceName("  "))
        assertTrue(AccountRules.isValidDeviceName("x".repeat(64)))
        assertFalse(AccountRules.isValidDeviceName("x".repeat(65)))
    }

    @Test
    fun displayNameFallsBackToEmailLocalPart() {
        assertEquals("Zero", AccountRules.displayName(CloudUser("1", "zero@example.com", nickname = " Zero ")))
        assertEquals("zero", AccountRules.displayName(CloudUser("1", "zero@example.com", nickname = "  ")))
        assertEquals("Z", AccountRules.avatarInitial(CloudUser("1", "zero@example.com")))
        assertEquals("😀", AccountRules.avatarInitial(CloudUser("1", "x@y.co", nickname = "😀 smile")))
        assertNull(AccountRules.avatarInitial(CloudUser("1", "@example.com")))
    }

    @Test
    fun originIdParsingAndEditGate() {
        assertEquals(10000L, AccountRules.parseOriginId("10000"))
        assertEquals(123_456L, AccountRules.parseOriginId(" 123456 "))
        assertNull(AccountRules.parseOriginId("9999"))
        assertNull(AccountRules.parseOriginId("12a4"))
        assertNull(AccountRules.parseOriginId("-10001"))
        assertNull(AccountRules.parseOriginId(""))
        assertNull(AccountRules.parseOriginId("９９９９９"))
        assertEquals(9_007_199_254_740_991L, AccountRules.parseOriginId("9007199254740991"))
        assertNull(AccountRules.parseOriginId("9007199254740992"))
        assertNull(AccountRules.parseOriginId("99999999999999999999"))

        val base = session()
        assertTrue(AccountRules.canEditOriginId(base))
        assertFalse(AccountRules.canEditOriginId(base.copy(user = base.user.copy(originIdChanged = true))))
        assertFalse(AccountRules.canEditOriginId(base.copy(entitlements = base.entitlements + ("originIdEdit" to JsonValue.False))))
        assertFalse(AccountRules.canEditOriginId(base.copy(entitlements = base.entitlements - "originIdEdit")))
        // 权益必须是布尔 true，字符串 / 数字不算。
        assertFalse(AccountRules.canEditOriginId(base.copy(entitlements = base.entitlements + ("originIdEdit" to JsonValue.Str("true")))))
    }

    @Test
    fun emailPatternMatchesWebRegex() {
        for (good in listOf("a@b.co", "zero+x@example.com", "a@b.c.d", "a@.b.c", "a@b..")) {
            assertTrue(good, AccountRules.isValidEmail(good))
        }
        for (bad in listOf("", "a", "a@b", "@b.co", "a@", "a@b.", "a b@c.de", "a@@b.co", "a@b .co", "a@bc", "a@.b")) {
            assertFalse(bad, AccountRules.isValidEmail(bad))
        }
        assertTrue(AccountRules.isValidEmail("  a@b.co \n"))
    }

    @Test
    fun passwordRulesCheckLengthMismatchAndSameAsCurrentInOrder() {
        assertEquals("accountErrorPasswordTooShort", AccountRules.newPasswordErrorKey("short", "short", null))
        // 8 个码点（emoji 各算 1）满足长度。
        val emoji = "😀".repeat(8)
        assertNull(AccountRules.newPasswordErrorKey(emoji, emoji, null))
        assertEquals("accountPasswordMismatch", AccountRules.newPasswordErrorKey("longenough", "different1", null))
        assertEquals("accountPasswordSameAsCurrent", AccountRules.newPasswordErrorKey("longenough", "longenough", "longenough"))
        assertNull(AccountRules.newPasswordErrorKey("longenough", "longenough", "other-pass"))
    }

    // ── 验证码倒计时 ──

    @Test
    fun sentCodeRestoresRemainingAndCooldown() {
        val record = AccountRules.SentCode(sentAtMs = 1_000_000, ttlSeconds = 300)
        assertEquals(AccountRules.RestoredCode(300, remaining = 290, cooldown = 50), AccountRules.restore(record, 1_010_000L))
        assertEquals(AccountRules.RestoredCode(300, remaining = 200, cooldown = 0), AccountRules.restore(record, 1_100_500L))
        assertNull(AccountRules.restore(record, 1_400_000L))
        // 时钟回拨按未过去处理。
        assertEquals(300L, AccountRules.restore(record, 900_000)?.remaining)
        // 有效期短于冷却：冷却不超过有效期。
        val short = AccountRules.SentCode(sentAtMs = 0, ttlSeconds = 30)
        assertEquals(AccountRules.RestoredCode(30, remaining = 25, cooldown = 25), AccountRules.restore(short, 5_000))
        assertEquals(50L, AccountRules.resendCooldown(290, 300))
        assertEquals(0L, AccountRules.resendCooldown(200, 300))
    }

    @Test
    fun codeClockCountsDownAndNeverGoesNegative() {
        val clock = CodeClock(ttl = 300, sentAtMs = 10_000)
        assertEquals(300L, clock.remaining(10_000))
        assertEquals(290L, clock.remaining(20_000))
        assertEquals(50L, clock.cooldown(20_000))
        assertEquals(0L, clock.cooldown(80_000))
        assertEquals(0L, clock.remaining(311_000L))
        // 回拨的时钟不会让剩余时间超过有效期。
        assertEquals(300L, clock.remaining(0))
    }

    // ── 错误映射 ──

    @Test
    fun reasonWinsOverCodeAndContextRefinesTheFallback() {
        val key = { e: HostException, c: AccountErrorContext -> AccountRules.errorKey(e, c) }
        val general = AccountErrorContext.General
        assertEquals(
            "accountErrorRateLimited",
            key(err(HostErrorCode.Unavailable, "rateLimited", retryable = true), AccountErrorContext.Login),
        )
        assertEquals("accountErrorInvalidCredentials", key(err(HostErrorCode.Unauthorized), AccountErrorContext.Login))
        assertEquals("localServiceActionFailed", key(err(HostErrorCode.Unauthorized), general))
        assertEquals("accountErrorInvalidCode", key(err(HostErrorCode.InvalidArgument), AccountErrorContext.Code))
        assertEquals("accountErrorValidation", key(err(HostErrorCode.InvalidArgument), AccountErrorContext.Register))
        assertEquals("accountErrorEmailTaken", key(err(HostErrorCode.Conflict), AccountErrorContext.Register))
        assertEquals("accountErrorRegistrationClosed", key(err(HostErrorCode.Unsupported), AccountErrorContext.Register))
        assertEquals("accountErrorNetwork", key(err(HostErrorCode.Unavailable, retryable = true), AccountErrorContext.Login))
        assertEquals("cloudSyncErrorNetwork", key(err(HostErrorCode.Unavailable, retryable = true), AccountErrorContext.Sync))
        assertEquals(
            "localServiceDisconnected",
            key(err(HostErrorCode.Unavailable, retryable = true), AccountErrorContext.Pairing),
        )
        assertEquals("errReasonPairingTimeout", AccountRules.errorKeyForReason("pairingTimeout", AccountErrorContext.Pairing))
        assertEquals("errReasonPeerClockSkew", AccountRules.errorKeyForReason("peerClockSkew", AccountErrorContext.Pairing))
        assertEquals("localPairingAddressInvalid", key(err(HostErrorCode.InvalidArgument), AccountErrorContext.Pairing))
        assertEquals("localServiceDisconnected", key(err(HostErrorCode.Timeout), general))
        // 不可重试的 unavailable 不是「云端网络不通」。
        assertEquals("localServiceDisconnected", key(err(HostErrorCode.Unavailable), AccountErrorContext.Login))
        assertEquals("localServiceConflict", key(err(HostErrorCode.Conflict), general))
        assertEquals("localServiceInvalidArgument", key(err(HostErrorCode.NotFound), general))
        assertEquals("settingsUnsupportedOnPlatform", key(err(HostErrorCode.Unsupported), general))
        // 同一 reason 在同步场景换文案；未映射的 reason 回退按码。
        val untrusted = err(HostErrorCode.Unauthorized, "deviceUntrusted")
        assertEquals("accountErrorDeviceUntrusted", key(untrusted, AccountErrorContext.Login))
        assertEquals("cloudSyncErrorDeviceUntrusted", key(untrusted, AccountErrorContext.Sync))
        assertEquals("localServiceActionFailed", key(err(HostErrorCode.Internal, "marketUnreachable"), general))
        assertEquals("accountOriginIdErrorTaken", key(err(HostErrorCode.Conflict, "originIdTaken"), general))
        assertEquals("accountOriginIdErrorAlreadyChanged", key(err(HostErrorCode.Conflict, "originIdAlreadyChanged"), general))
        assertEquals("accountOriginIdErrorNotAllowed", key(err(HostErrorCode.Conflict, "originIdChangeNotAllowed"), general))
        assertEquals("errReasonMailNotConfigured", key(err(HostErrorCode.Unavailable, "mailNotConfigured"), general))
    }

    @Test
    fun registrationIncompleteIsDetectedFromReasonOrLegacyConflict() {
        assertTrue(AccountRules.isRegistrationIncomplete(err(HostErrorCode.Conflict, "registrationIncomplete")))
        assertTrue(AccountRules.isRegistrationIncomplete(err(HostErrorCode.Conflict)))
        assertFalse(AccountRules.isRegistrationIncomplete(err(HostErrorCode.Conflict, "emailTaken")))
        assertFalse(AccountRules.isRegistrationIncomplete(err(HostErrorCode.Unauthorized)))
    }

    @Test
    fun sessionRevokedReasonsMapToOneOffMessages() {
        assertEquals("accountSessionRevokedUntrusted", AccountRules.sessionRevokedKey("deviceUntrusted"))
        assertEquals("accountSessionRevokedPasswordChanged", AccountRules.sessionRevokedKey("passwordChanged"))
        assertEquals("accountErrorAccountDisabled", AccountRules.sessionRevokedKey("accountDisabled"))
        assertEquals("accountSessionRevokedExpired", AccountRules.sessionRevokedKey("sessionExpired"))
        assertEquals("accountSessionRevokedExpired", AccountRules.sessionRevokedKey("whatever"))
        assertEquals("accountSessionRevokedExpired", AccountRules.sessionRevokedKey(""))
    }

    // ── 套餐徽标 ──

    @Test
    fun badgeTextAndColor() {
        var plan = CloudPlan("p", badge = "  VIP ")
        assertEquals("VIP", AccountRules.planBadgeText(plan, 12))
        plan = plan.copy(badgeNumbered = true, badgeNumberDigits = 3)
        assertEquals("VIP No.012", AccountRules.planBadgeText(plan, 12))
        assertEquals("VIP No.123456", AccountRules.planBadgeText(plan, 123_456))
        assertEquals("VIP", AccountRules.planBadgeText(plan, null))
        plan = plan.copy(badgeNumberDigits = 99)
        assertEquals("VIP No.000005", AccountRules.planBadgeText(plan, 5))
        assertNull(AccountRules.planBadgeText(plan.copy(badge = "  "), 5))
        assertNull(AccountRules.planBadgeText(plan.copy(badge = null), 5))

        val rgb = requireNotNull(AccountRules.parseBadgeColor("#FF8000"))
        assertTrue(rgb.alpha == 1.0 && rgb.red == 1.0 && Math.abs(rgb.green - 128.0 / 255) < 1e-9 && rgb.blue == 0.0)
        val argb = requireNotNull(AccountRules.parseBadgeColor("80FF0000"))
        assertTrue(Math.abs(argb.alpha - 128.0 / 255) < 1e-9 && argb.red == 1.0)
        assertNull(AccountRules.parseBadgeColor(""))
        assertNull(AccountRules.parseBadgeColor("12345"))
        assertNull(AccountRules.parseBadgeColor("GGGGGG"))
        assertNotNull(AccountRules.parseBadgeColor("  #abcdef  "))
    }

    @Test
    fun connectionPresence() {
        val connected = CloudConnectionDto(CloudConnectionState.Connected)
        assertTrue(CloudPresence.isKnown(connected, localReady = true))
        assertFalse(CloudPresence.isKnown(connected, localReady = false))
        assertFalse(CloudPresence.isKnown(CloudConnectionDto(CloudConnectionState.Reconnecting), localReady = true))
        assertFalse(CloudPresence.isKnown(null, localReady = true))
        assertEquals("cloudConnectionDisconnected", CloudPresence.labelKey(null, localReady = true))
        assertEquals("localServiceDisconnected", CloudPresence.labelKey(connected, localReady = false))
        assertEquals("cloudConnectionReconnecting", CloudPresence.labelKey(CloudConnectionDto(CloudConnectionState.Reconnecting), localReady = true))
        assertEquals("devicePresenceUnknown", CloudPresence.deviceKey(isOnline = true, presenceKnown = false))
        assertEquals("deviceOffline", CloudPresence.deviceKey(isOnline = false, presenceKnown = true))
    }
}
