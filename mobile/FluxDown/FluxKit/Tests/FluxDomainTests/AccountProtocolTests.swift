import FluxDomain
import Foundation
import Testing

/// 账户 DTO 解码（真实 serde 形状）+ 昵称 / Origin ID / 密码 / 错误映射等纯规则。
struct AccountProtocolTests {
    private static func decode<T: Decodable>(_ json: String, as type: T.Type = T.self) throws -> T {
        try ProtocolJSON.decode(T.self, from: Data(json.utf8), what: "test")
    }

    // MARK: 会话

    private static let sessionJSON = """
    {"user":{"id":"u1","email":"zero@example.com","nickname":"Zero","plan":"pro","status":"active",
      "createdAt":"2025-01-02T03:04:05Z","lastLoginAt":null,"originId":12345,"originIdChanged":false,
      "membershipOrdinal":7,"hasPassword":true,"futureField":1},
     "entitlements":{"originIdEdit":true,"maxDevices":5,"tags":["a"]},
     "currentPlan":{"code":"pro","name":"Pro","description":"x","badge":"PRO","icon":"crown","color":"#fff",
       "badgeStyle":"medal","badgeColor":"FFAA00","badgeNumbered":true,"badgeNumberDigits":4,"priceMinor":100,
       "currency":"CNY","highlights":[],"entitlements":{},"sort":1,"campaign":null,"purchasable":true},
     "device":{"id":"row1","deviceId":"dev-1","name":"iPhone","platform":"ios","createdAt":"2025-01-02T03:04:05Z",
       "lastSeenAt":"2025-02-02T03:04:05Z","lastIp":null,"appVersion":"1.0.0","isOnline":true,"isCurrent":true,
       "defaultSaveDir":"/var/mobile/Documents","pathStyle":"posix"}}
    """

    @Test func sessionDecodesWithPlanEntitlementsAndDeviceAndIgnoresUnknownKeys() throws {
        let session = try Self.decode(Self.sessionJSON, as: AgentSessionDto.self)
        #expect(session.user.id == "u1")
        #expect(session.user.originId == 12345)
        #expect(session.user.hasPassword == true)
        #expect(session.user.status == .active)
        #expect(session.entitlements["originIdEdit"] == .bool(true))
        #expect(session.entitlements["maxDevices"]?.intValue == 5)
        #expect(session.currentPlan?.badgeStyle == "medal")
        #expect(session.device.id == "row1")
        #expect(session.device.deviceId == "dev-1")
        #expect(session.device.pathStyle == .posix)
        #expect(AccountRules.canEditOriginId(session))
        #expect(AccountRules.planBadgeText(session.currentPlan!, ordinal: session.user.membershipOrdinal) == "PRO No.0007")
    }

    @Test func loggedOutSectionIsNullAndMissingOptionalsDefault() throws {
        let none = try Self.decode("null", as: AgentSessionDto?.self)
        #expect(none == nil)
        let user = try Self.decode(#"{"id":"u","email":"a@b.co"}"#, as: CloudUser.self)
        #expect(user.nickname == "")
        #expect(user.status == .active)
        #expect(user.originIdChanged == false)
        #expect(user.hasPassword == nil)
        let unknown = try Self.decode(#"{"id":"u","email":"a@b.co","status":"frozen"}"#, as: CloudUser.self)
        #expect(unknown.status == .unknown("frozen"))
    }

    @Test func loginResultDecodesEveryStatusLeniently() throws {
        let ok = try Self.decode(#"{"status":"ok","session":\#(Self.sessionJSON)}"#, as: AgentLoginResult.self)
        guard case let .ok(session) = ok else {
            Issue.record("expected ok")
            return
        }
        #expect(session.user.email == "zero@example.com")

        let verify = try Self.decode(#"{"status":"deviceVerificationRequired","ttlSeconds":300,"willReplaceDevices":true}"#, as: AgentLoginResult.self)
        #expect(verify == .deviceVerificationRequired(ttlSeconds: 300, willReplaceDevices: true))
        let bare = try Self.decode(#"{"status":"deviceVerificationRequired","ttlSeconds":60}"#, as: AgentLoginResult.self)
        #expect(bare == .deviceVerificationRequired(ttlSeconds: 60, willReplaceDevices: false))
        let other = try Self.decode(#"{"status":"mfaRequired"}"#, as: AgentLoginResult.self)
        #expect(other == .unknown(status: "mfaRequired"))
    }

    @Test func cloudConnectionAndParamsMatchWireShape() throws {
        let connection = try Self.decode(#"{"state":"reconnecting","lastError":"boom","lastErrorReason":"cloudUnreachable"}"#, as: CloudConnectionDto.self)
        #expect(connection.state == .reconnecting)
        #expect(connection.lastErrorReason == "cloudUnreachable")
        #expect(try Self.decode(#"{"state":"warping"}"#, as: CloudConnectionDto.self).state == .unknown("warping"))

        // 可选字段省略；键名 camelCase。
        let register = try ProtocolJSON.encode(RegisterParams(email: "a@b.co", password: "pw"), what: "t")
        #expect(String(decoding: register, as: UTF8.self) == #"{"email":"a@b.co","password":"pw"}"#)
        let change = try ProtocolJSON.encode(ChangeEmailParams(email: "n@b.co", oldCode: "1", newCode: "2"), what: "t")
        #expect(String(decoding: change, as: UTF8.self) == #"{"email":"n@b.co","newCode":"2","oldCode":"1"}"#)
        let password = try ProtocolJSON.encode(ChangePasswordParams(newPassword: "n", code: "9"), what: "t")
        #expect(String(decoding: password, as: UTF8.self) == #"{"code":"9","newPassword":"n"}"#)
        #expect(try Self.decode(#"{"ttlSeconds":300}"#, as: TtlResult.self).ttlSeconds == 300)
        #expect(try Self.decode(#"{"available":false,"reason":"invalid"}"#, as: OriginIdCheckResult.self).reason == "invalid")
    }

    // MARK: 昵称 / Origin ID / 邮箱 / 密码

    @Test func nicknameIsTrimmedAndCountedInUnicodeScalars() {
        #expect(AccountRules.isValidNickname("Zero"))
        #expect(AccountRules.isValidNickname("  Zero \n"))
        #expect(!AccountRules.isValidNickname(""))
        #expect(!AccountRules.isValidNickname("   \t\u{3000}"))
        #expect(AccountRules.isValidNickname(String(repeating: "a", count: 32)))
        #expect(!AccountRules.isValidNickname(String(repeating: "a", count: 33)))
        // 32 个码点（含 emoji）仍合法；33 个不合法；空白只在两端裁剪。
        #expect(AccountRules.isValidNickname(String(repeating: "😀", count: 32)))
        #expect(!AccountRules.isValidNickname(String(repeating: "😀", count: 33)))
        #expect(AccountRules.isValidNickname("a b"))
        #expect(AccountRules.trimmed(" \u{00A0}x\u{2003}") == "x")
    }

    @Test func deviceNameRuleIs1To64() {
        #expect(AccountRules.isValidDeviceName("iPhone"))
        #expect(!AccountRules.isValidDeviceName("  "))
        #expect(AccountRules.isValidDeviceName(String(repeating: "x", count: 64)))
        #expect(!AccountRules.isValidDeviceName(String(repeating: "x", count: 65)))
    }

    @Test func displayNameFallsBackToEmailLocalPart() {
        #expect(AccountRules.displayName(CloudUser(id: "1", email: "zero@example.com", nickname: " Zero ")) == "Zero")
        #expect(AccountRules.displayName(CloudUser(id: "1", email: "zero@example.com", nickname: "  ")) == "zero")
        #expect(AccountRules.avatarInitial(CloudUser(id: "1", email: "zero@example.com")) == "Z")
    }

    @Test func originIdParsingAndEditGate() throws {
        #expect(AccountRules.parseOriginId("10000") == 10000)
        #expect(AccountRules.parseOriginId(" 123456 ") == 123_456)
        #expect(AccountRules.parseOriginId("9999") == nil)
        #expect(AccountRules.parseOriginId("12a4") == nil)
        #expect(AccountRules.parseOriginId("-10001") == nil)
        #expect(AccountRules.parseOriginId("") == nil)
        #expect(AccountRules.parseOriginId("9007199254740992") == nil)

        var session = try Self.decode(Self.sessionJSON, as: AgentSessionDto.self)
        #expect(AccountRules.canEditOriginId(session))
        session.user.originIdChanged = true
        #expect(!AccountRules.canEditOriginId(session))
        session.user.originIdChanged = false
        session.entitlements["originIdEdit"] = .bool(false)
        #expect(!AccountRules.canEditOriginId(session))
        session.entitlements.removeValue(forKey: "originIdEdit")
        #expect(!AccountRules.canEditOriginId(session))
    }

    @Test func emailPatternMatchesWebRegex() {
        for good in ["a@b.co", "zero+x@example.com", "a@b.c.d", "a@.b.c", "a@b.."] { #expect(AccountRules.isValidEmail(good), "\(good)") }
        for bad in ["", "a", "a@b", "@b.co", "a@", "a@b.", "a b@c.de", "a@@b.co", "a@b .co", "a@bc", "a@.b"] { #expect(!AccountRules.isValidEmail(bad), "\(bad)") }
    }

    @Test func passwordRulesCheckLengthMismatchAndSameAsCurrentInOrder() {
        #expect(AccountRules.newPasswordErrorKey(new: "short", confirm: "short", current: nil) == "accountErrorPasswordTooShort")
        // 8 个码点（emoji 各算 1）满足长度。
        #expect(AccountRules.newPasswordErrorKey(new: String(repeating: "😀", count: 8), confirm: String(repeating: "😀", count: 8), current: nil) == nil)
        #expect(AccountRules.newPasswordErrorKey(new: "longenough", confirm: "different1", current: nil) == "accountPasswordMismatch")
        #expect(AccountRules.newPasswordErrorKey(new: "longenough", confirm: "longenough", current: "longenough") == "accountPasswordSameAsCurrent")
        #expect(AccountRules.newPasswordErrorKey(new: "longenough", confirm: "longenough", current: "other-pass") == nil)
    }

    // MARK: 验证码倒计时

    @Test func sentCodeRestoresRemainingAndCooldown() {
        let record = AccountRules.SentCode(sentAtMs: 1_000_000, ttlSeconds: 300)
        let early = AccountRules.restore(record, nowMs: 1_000_000 + 10_000)
        #expect(early == AccountRules.RestoredCode(ttlSeconds: 300, remaining: 290, cooldown: 50))
        let mid = AccountRules.restore(record, nowMs: 1_000_000 + 100_500)
        #expect(mid == AccountRules.RestoredCode(ttlSeconds: 300, remaining: 200, cooldown: 0))
        #expect(AccountRules.restore(record, nowMs: 1_000_000 + 400_000) == nil)
        // 时钟回拨按未过去处理。
        #expect(AccountRules.restore(record, nowMs: 900_000)?.remaining == 300)
        // 有效期短于冷却：冷却不超过有效期。
        let short = AccountRules.SentCode(sentAtMs: 0, ttlSeconds: 30)
        #expect(AccountRules.restore(short, nowMs: 5_000) == AccountRules.RestoredCode(ttlSeconds: 30, remaining: 25, cooldown: 25))
        #expect(AccountRules.resendCooldown(remaining: 290, ttlSeconds: 300) == 50)
        #expect(AccountRules.resendCooldown(remaining: 200, ttlSeconds: 300) == 0)
    }

    // MARK: 错误映射

    @Test func reasonWinsOverCodeAndContextRefinesTheFallback() {
        let rate = HostError(.unavailable, reason: "rateLimited", retryable: true)
        #expect(AccountRules.errorKey(rate, context: .login) == "accountErrorRateLimited")
        #expect(AccountRules.errorKey(HostError(.unauthorized), context: .login) == "accountErrorInvalidCredentials")
        #expect(AccountRules.errorKey(HostError(.unauthorized), context: .general) == "localServiceActionFailed")
        #expect(AccountRules.errorKey(HostError(.invalidArgument), context: .code) == "accountErrorInvalidCode")
        #expect(AccountRules.errorKey(HostError(.invalidArgument), context: .register) == "accountErrorValidation")
        #expect(AccountRules.errorKey(HostError(.conflict), context: .register) == "accountErrorEmailTaken")
        #expect(AccountRules.errorKey(HostError(.unsupported), context: .register) == "accountErrorRegistrationClosed")
        #expect(AccountRules.errorKey(HostError(.unavailable, retryable: true), context: .login) == "accountErrorNetwork")
        #expect(AccountRules.errorKey(HostError(.unavailable, retryable: true), context: .sync) == "cloudSyncErrorNetwork")
        // 不带 reason 的 unavailable：本机服务问题，不显示「无法连接对端地址」。
        #expect(AccountRules.errorKey(HostError(.unavailable, retryable: true), context: .pairing) == "localServiceDisconnected")
        #expect(AccountRules.errorKey(HostError(.unavailable, reason: "pairingPeerUnreachable"), context: .pairing) == "errReasonPairingPeerUnreachable")
        #expect(AccountRules.errorKey(HostError(.timeout, reason: "pairingTimeout"), context: .pairing) == "errReasonPairingTimeout")
        #expect(AccountRules.errorKey(HostError(.unauthorized, reason: "peerClockSkew"), context: .pairing) == "errReasonPeerClockSkew")
        #expect(AccountRules.errorKey(HostError(.invalidArgument), context: .pairing) == "localPairingAddressInvalid")
        #expect(AccountRules.errorKey(HostError(.timeout)) == "localServiceDisconnected")
        #expect(AccountRules.errorKey(HostError(.conflict)) == "localServiceConflict")
        #expect(AccountRules.errorKey(HostError(.notFound)) == "localServiceInvalidArgument")
        // 同一 reason 在同步场景换文案；未映射的 reason 回退按码。
        let untrusted = HostError(.unauthorized, reason: "deviceUntrusted")
        #expect(AccountRules.errorKey(untrusted, context: .login) == "accountErrorDeviceUntrusted")
        #expect(AccountRules.errorKey(untrusted, context: .sync) == "cloudSyncErrorDeviceUntrusted")
        #expect(AccountRules.errorKey(HostError(.internal, reason: "marketUnreachable")) == "localServiceActionFailed")
    }

    @Test func registrationIncompleteIsDetectedFromReasonOrLegacyConflict() {
        #expect(AccountRules.isRegistrationIncomplete(HostError(.conflict, reason: "registrationIncomplete")))
        #expect(AccountRules.isRegistrationIncomplete(HostError(.conflict)))
        #expect(!AccountRules.isRegistrationIncomplete(HostError(.conflict, reason: "emailTaken")))
        #expect(!AccountRules.isRegistrationIncomplete(HostError(.unauthorized)))
    }

    @Test func sessionRevokedReasonsMapToOneOffMessages() {
        #expect(AccountRules.sessionRevokedKey(reason: "deviceUntrusted") == "accountSessionRevokedUntrusted")
        #expect(AccountRules.sessionRevokedKey(reason: "passwordChanged") == "accountSessionRevokedPasswordChanged")
        #expect(AccountRules.sessionRevokedKey(reason: "accountDisabled") == "accountErrorAccountDisabled")
        #expect(AccountRules.sessionRevokedKey(reason: "sessionExpired") == "accountSessionRevokedExpired")
        #expect(AccountRules.sessionRevokedKey(reason: "whatever") == "accountSessionRevokedExpired")
    }

    // MARK: 套餐徽标

    @Test func badgeTextAndColor() throws {
        var plan = CloudPlan(code: "p", badge: "  VIP ")
        #expect(AccountRules.planBadgeText(plan, ordinal: 12) == "VIP")
        plan.badgeNumbered = true
        plan.badgeNumberDigits = 3
        #expect(AccountRules.planBadgeText(plan, ordinal: 12) == "VIP No.012")
        #expect(AccountRules.planBadgeText(plan, ordinal: 123_456) == "VIP No.123456")
        #expect(AccountRules.planBadgeText(plan, ordinal: nil) == "VIP")
        plan.badgeNumberDigits = 99
        #expect(AccountRules.planBadgeText(plan, ordinal: 5) == "VIP No.000005")
        plan.badge = "  "
        #expect(AccountRules.planBadgeText(plan, ordinal: 5) == nil)
        plan.badge = nil
        #expect(AccountRules.planBadgeText(plan, ordinal: 5) == nil)

        let rgb = try #require(AccountRules.parseBadgeColor("#FF8000"))
        #expect(rgb.alpha == 1 && rgb.red == 1 && abs(rgb.green - 128.0 / 255) < 1e-9 && rgb.blue == 0)
        let argb = try #require(AccountRules.parseBadgeColor("80FF0000"))
        #expect(abs(argb.alpha - 128.0 / 255) < 1e-9 && argb.red == 1)
        #expect(AccountRules.parseBadgeColor("") == nil)
        #expect(AccountRules.parseBadgeColor("12345") == nil)
        #expect(AccountRules.parseBadgeColor("GGGGGG") == nil)
    }

    @Test func connectionPresence() {
        let connected = CloudConnectionDto(state: .connected)
        #expect(CloudPresence.isKnown(connected, localReady: true))
        #expect(!CloudPresence.isKnown(connected, localReady: false))
        #expect(!CloudPresence.isKnown(CloudConnectionDto(state: .reconnecting), localReady: true))
        #expect(!CloudPresence.isKnown(nil, localReady: true))
        #expect(CloudPresence.labelKey(nil, localReady: true) == "cloudConnectionDisconnected")
        #expect(CloudPresence.labelKey(connected, localReady: false) == "localServiceDisconnected")
        #expect(CloudPresence.deviceKey(isOnline: true, presenceKnown: false) == "devicePresenceUnknown")
        #expect(CloudPresence.deviceKey(isOnline: false, presenceKnown: true) == "deviceOffline")
    }
}
