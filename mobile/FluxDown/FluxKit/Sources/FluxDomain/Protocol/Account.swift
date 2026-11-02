import Foundation

// FluxCloud 账户（`agent.auth.*` / `agent.profile.*` / `agent.session.get`）的协议 DTO 与纯规则。
// 镜像 `native/protocol/src/agent.rs`（交叉核对 `web/src/lib/rpc/protocol/{agent,params}.ts`）；
// 规则镜像 Web `pages/settings/sections/account/*.ts` 与 GPUI `crates/account/src/{errors,verification}.rs`。
// 计费 / 订单 / 推荐（`agent.plan|order|referral.*`）在 iOS 不出现（App Store 规则），因此不建模。

// MARK: - 连接 / 用户 / 套餐

/// `CloudConnectionState`：任务连接（SSE + 心跳 + 设备名册）的生命周期。
public enum CloudConnectionState: WireStringEnum {
    case disconnected, connecting, connected, reconnecting
    case unknown(String)

    public init(wire: String) {
        switch wire {
        case "disconnected": self = .disconnected
        case "connecting": self = .connecting
        case "connected": self = .connected
        case "reconnecting": self = .reconnecting
        default: self = .unknown(wire)
        }
    }

    public var wire: String {
        switch self {
        case .disconnected: "disconnected"
        case .connecting: "connecting"
        case .connected: "connected"
        case .reconnecting: "reconnecting"
        case let .unknown(raw): raw
        }
    }
}

/// `agent.cloudConnection` 分区。`lastErrorReason` 是 `ErrorReason` 的 wire 名（原样保存，见 ``AccountRules/errorKey(forReason:context:)``）。
public struct CloudConnectionDto: Sendable, Hashable, Codable {
    public var state: CloudConnectionState
    public var lastError: String?
    public var lastErrorReason: String?

    public init(state: CloudConnectionState = .disconnected, lastError: String? = nil, lastErrorReason: String? = nil) {
        self.state = state
        self.lastError = lastError
        self.lastErrorReason = lastErrorReason
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        state = try c.decodeIfPresent(CloudConnectionState.self, forKey: .state) ?? .disconnected
        lastError = try c.decodeIfPresent(String.self, forKey: .lastError)
        lastErrorReason = try c.decodeIfPresent(String.self, forKey: .lastErrorReason)
    }
}

/// 云端在线状态是否可信：本地服务就绪且任务连接已建立（`web/src/lib/cloud-presence.ts`）。
public enum CloudPresence {
    public static func isKnown(_ connection: CloudConnectionDto?, localReady: Bool) -> Bool {
        guard localReady, let connection else { return false }
        if case .connected = connection.state { return true }
        return false
    }

    /// 连接状态文案键。
    public static func labelKey(_ connection: CloudConnectionDto?, localReady: Bool) -> String {
        guard localReady else { return "localServiceDisconnected" }
        switch connection?.state {
        case .connected?: return "cloudConnectionConnected"
        case .connecting?: return "cloudConnectionConnecting"
        case .reconnecting?: return "cloudConnectionReconnecting"
        default: return "cloudConnectionDisconnected"
        }
    }

    /// 设备在线状态文案键（presence 未知时不显示在线 / 离线）。
    public static func deviceKey(isOnline: Bool, presenceKnown: Bool) -> String {
        presenceKnown ? (isOnline ? "deviceOnline" : "deviceOffline") : "devicePresenceUnknown"
    }
}

/// `CloudUserStatus`。
public enum CloudUserStatus: WireStringEnum {
    case active, disabled, pending
    case unknown(String)

    public init(wire: String) {
        switch wire {
        case "active": self = .active
        case "disabled": self = .disabled
        case "pending": self = .pending
        default: self = .unknown(wire)
        }
    }

    public var wire: String {
        switch self {
        case .active: "active"
        case .disabled: "disabled"
        case .pending: "pending"
        case let .unknown(raw): raw
        }
    }
}

/// FluxCloud 用户公开资料（`CloudUser`）。
public struct CloudUser: Sendable, Hashable, Codable {
    public var id: String
    public var email: String
    public var nickname: String
    public var plan: String
    public var status: CloudUserStatus
    public var createdAt: String
    public var lastLoginAt: String?
    public var originId: Int64?
    public var originIdChanged: Bool
    public var membershipOrdinal: Int64?
    /// 旧版云端不下发 → nil（视为未知，按已设置处理）。
    public var hasPassword: Bool?

    public init(
        id: String, email: String, nickname: String = "", plan: String = "", status: CloudUserStatus = .active,
        createdAt: String = "", lastLoginAt: String? = nil, originId: Int64? = nil, originIdChanged: Bool = false,
        membershipOrdinal: Int64? = nil, hasPassword: Bool? = nil
    ) {
        self.id = id
        self.email = email
        self.nickname = nickname
        self.plan = plan
        self.status = status
        self.createdAt = createdAt
        self.lastLoginAt = lastLoginAt
        self.originId = originId
        self.originIdChanged = originIdChanged
        self.membershipOrdinal = membershipOrdinal
        self.hasPassword = hasPassword
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        email = try c.decode(String.self, forKey: .email)
        nickname = try c.decodeIfPresent(String.self, forKey: .nickname) ?? ""
        plan = try c.decodeIfPresent(String.self, forKey: .plan) ?? ""
        status = try c.decodeIfPresent(CloudUserStatus.self, forKey: .status) ?? .active
        createdAt = try c.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        lastLoginAt = try c.decodeIfPresent(String.self, forKey: .lastLoginAt)
        originId = try c.decodeIfPresent(Int64.self, forKey: .originId)
        originIdChanged = try c.decodeIfPresent(Bool.self, forKey: .originIdChanged) ?? false
        membershipOrdinal = try c.decodeIfPresent(Int64.self, forKey: .membershipOrdinal)
        hasPassword = try c.decodeIfPresent(Bool.self, forKey: .hasPassword)
    }
}

/// 前向兼容的套餐权益集合（未知字段原样保留）。
public typealias Entitlements = [String: JSONValue]

/// 套餐徽标所需的 `CloudPlan` 子集（价格 / 活动 / 购买相关字段刻意不建模：iOS 不出现购买流程）。
public struct CloudPlan: Sendable, Hashable, Codable {
    public var code: String
    public var name: String
    public var badge: String?
    /// `outline | solid | medal | ribbon`，其它 = 纯文字。
    public var badgeStyle: String
    /// `RRGGBB` / `AARRGGBB`（可带 `#`）。
    public var badgeColor: String
    public var badgeNumbered: Bool
    public var badgeNumberDigits: Int

    public init(
        code: String, name: String = "", badge: String? = nil, badgeStyle: String = "outline", badgeColor: String = "",
        badgeNumbered: Bool = false, badgeNumberDigits: Int = 4
    ) {
        self.code = code
        self.name = name
        self.badge = badge
        self.badgeStyle = badgeStyle
        self.badgeColor = badgeColor
        self.badgeNumbered = badgeNumbered
        self.badgeNumberDigits = badgeNumberDigits
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        code = try c.decode(String.self, forKey: .code)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        badge = try c.decodeIfPresent(String.self, forKey: .badge)
        badgeStyle = try c.decodeIfPresent(String.self, forKey: .badgeStyle) ?? "outline"
        badgeColor = try c.decodeIfPresent(String.self, forKey: .badgeColor) ?? ""
        badgeNumbered = try c.decodeIfPresent(Bool.self, forKey: .badgeNumbered) ?? false
        badgeNumberDigits = try c.decodeIfPresent(Int.self, forKey: .badgeNumberDigits) ?? 4
    }
}

/// `agent.session` 分区 / 登录结果里的无令牌会话视图。
public struct AgentSessionDto: Sendable, Hashable, Codable {
    public var user: CloudUser
    public var entitlements: Entitlements
    public var currentPlan: CloudPlan?
    public var device: CloudDeviceRecord

    public init(user: CloudUser, entitlements: Entitlements = [:], currentPlan: CloudPlan? = nil, device: CloudDeviceRecord) {
        self.user = user
        self.entitlements = entitlements
        self.currentPlan = currentPlan
        self.device = device
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        user = try c.decode(CloudUser.self, forKey: .user)
        entitlements = try c.decodeIfPresent(Entitlements.self, forKey: .entitlements) ?? [:]
        currentPlan = try c.decodeIfPresent(CloudPlan.self, forKey: .currentPlan)
        device = try c.decode(CloudDeviceRecord.self, forKey: .device)
    }
}

/// `agent.auth.login|loginVerify|register|registerVerify|verifyCode` 的结果（`status` 判别；未知判别不抛错）。
public enum AgentLoginResult: Sendable, Hashable, Decodable {
    case ok(AgentSessionDto)
    case deviceVerificationRequired(ttlSeconds: UInt64, willReplaceDevices: Bool)
    case unknown(status: String)

    private enum CodingKeys: String, CodingKey { case status, session, ttlSeconds, willReplaceDevices }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let status = try c.decode(String.self, forKey: .status)
        switch status {
        case "ok":
            self = try .ok(c.decode(AgentSessionDto.self, forKey: .session))
        case "deviceVerificationRequired":
            self = try .deviceVerificationRequired(
                ttlSeconds: c.decode(UInt64.self, forKey: .ttlSeconds),
                willReplaceDevices: c.decodeIfPresent(Bool.self, forKey: .willReplaceDevices) ?? false
            )
        default:
            self = .unknown(status: status)
        }
    }
}

// MARK: - 参数 / 结果

public struct RegisterParams: Sendable, Hashable, Codable {
    public var email: String
    public var password: String
    public var nickname: String?

    public init(email: String, password: String, nickname: String? = nil) {
        self.email = email
        self.password = password
        self.nickname = nickname
    }
}

public struct RegisterVerifyParams: Sendable, Hashable, Codable {
    public var email: String
    public var code: String

    public init(email: String, code: String) {
        self.email = email
        self.code = code
    }
}

/// `account` 接受邮箱或纯数字 Origin ID。
public struct LoginParams: Sendable, Hashable, Codable {
    public var account: String
    public var password: String

    public init(account: String, password: String) {
        self.account = account
        self.password = password
    }
}

public struct LoginVerifyParams: Sendable, Hashable, Codable {
    public var account: String
    public var password: String
    public var code: String

    public init(account: String, password: String, code: String) {
        self.account = account
        self.password = password
        self.code = code
    }
}

/// `agent.auth.sendCode` / `sendPasswordResetCode`。
public struct SendCodeParams: Sendable, Hashable, Codable {
    public var email: String

    public init(email: String) { self.email = email }
}

/// 验证码登录；邮箱不存在则自动注册（此时 `nickname` 生效）。
public struct VerifyCodeParams: Sendable, Hashable, Codable {
    public var email: String
    public var code: String
    public var nickname: String?

    public init(email: String, code: String, nickname: String? = nil) {
        self.email = email
        self.code = code
        self.nickname = nickname
    }
}

/// 发送验证码类方法的结果。
public struct TtlResult: Sendable, Hashable, Codable {
    public var ttlSeconds: UInt64

    public init(ttlSeconds: UInt64) { self.ttlSeconds = ttlSeconds }
}

public struct SendNewEmailCodeParams: Sendable, Hashable, Codable {
    /// 新邮箱。
    public var email: String
    /// 原邮箱收到的验证码。
    public var code: String

    public init(email: String, code: String) {
        self.email = email
        self.code = code
    }
}

public struct ChangeEmailParams: Sendable, Hashable, Codable {
    public var email: String
    public var oldCode: String
    public var newCode: String

    public init(email: String, oldCode: String, newCode: String) {
        self.email = email
        self.oldCode = oldCode
        self.newCode = newCode
    }
}

public struct RandomOriginIdResult: Sendable, Hashable, Codable {
    public var originId: Int64

    public init(originId: Int64) { self.originId = originId }
}

public struct CheckOriginIdParams: Sendable, Hashable, Codable {
    public var value: Int64

    public init(value: Int64) { self.value = value }
}

public struct OriginIdCheckResult: Sendable, Hashable, Codable {
    public var available: Bool
    /// `invalid` = 格式不合法；其它 = 已被占用等。
    public var reason: String?

    public init(available: Bool, reason: String? = nil) {
        self.available = available
        self.reason = reason
    }
}

public struct ChangeOriginIdParams: Sendable, Hashable, Codable {
    /// ≥ 10000，全局仅可成功一次。
    public var originId: Int64

    public init(originId: Int64) { self.originId = originId }
}

public struct ChangeNicknameParams: Sendable, Hashable, Codable {
    /// 1–32 字符（服务端 trim 后校验）。
    public var nickname: String

    public init(nickname: String) { self.nickname = nickname }
}

/// 删除账号：`currentPassword` 与 `code`（`sendDeleteCode` 发到绑定邮箱）二选一，nil 字段不编码。
public struct DeleteAccountParams: Sendable, Hashable, Codable {
    public var currentPassword: String?
    public var code: String?

    public init(currentPassword: String? = nil, code: String? = nil) {
        self.currentPassword = currentPassword
        self.code = code
    }
}

/// 已登录修改 / 设置密码：`currentPassword` 与 `code`（`sendPasswordCode` 发到绑定邮箱）二选一。
public struct ChangePasswordParams: Sendable, Hashable, Codable {
    public var newPassword: String
    public var currentPassword: String?
    public var code: String?

    public init(newPassword: String, currentPassword: String? = nil, code: String? = nil) {
        self.newPassword = newPassword
        self.currentPassword = currentPassword
        self.code = code
    }
}

/// 未登录重置密码：`sendPasswordResetCode` 发到该邮箱的验证码 + 新密码。
public struct ResetPasswordParams: Sendable, Hashable, Codable {
    public var email: String
    public var code: String
    public var newPassword: String

    public init(email: String, code: String, newPassword: String) {
        self.email = email
        self.code = code
        self.newPassword = newPassword
    }
}

/// `agent.cloud.endpointGet` 结果：FluxCloud 服务地址；正式构建 `editable=false`（地址恒为构建期固定值）。
public struct CloudEndpointDto: Sendable, Hashable, Codable {
    public var baseUrl: String
    public var defaultBaseUrl: String
    public var editable: Bool

    public init(baseUrl: String, defaultBaseUrl: String, editable: Bool) {
        self.baseUrl = baseUrl
        self.defaultBaseUrl = defaultBaseUrl
        self.editable = editable
    }
}

/// `agent.cloud.endpointSet` 参数；`baseUrl` 为空即恢复默认地址。
public struct CloudEndpointSetParams: Sendable, Hashable, Codable {
    public var baseUrl: String

    public init(baseUrl: String) { self.baseUrl = baseUrl }
}

// MARK: - 纯规则

/// 账户错误出现的位置：只在没有 `reason`（旧 agent / 非云端错误）时决定按 `code` 回退的措辞
/// （GPUI `errors.rs::ErrorContext`，Web `errorText.ts::AccountErrorContext`）。
public enum AccountErrorContext: Sendable, Hashable {
    case general, login, register
    /// 设备验证码 / 邮箱验证码校验。
    case code
    /// 配置同步。
    case sync
    /// 局域网直连配对与已配对设备操作。
    case pairing
}

/// 账户 / 设备页的表单与文案规则（无 UI 依赖，可单测）。
public enum AccountRules {
    public static let nicknameMaxLength = 32
    public static let deviceNameMaxLength = 64
    public static let minPasswordLength = 8
    public static let minOriginId: Int64 = 10000
    /// 验证码重发冷却（云端对同一用途重发的最短间隔，秒）。
    public static let resendCooldownSeconds = 60

    // MARK: 昵称 / 设备名

    /// Rust `str::trim`（Unicode White_Space）。
    public static func trimmed(_ value: String) -> String {
        value.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// 昵称按 Unicode 标量计数（与服务端 `chars().count()` 一致）。
    public static func isValidNickname(_ value: String) -> Bool {
        let count = trimmed(value).unicodeScalars.count
        return count >= 1 && count <= nicknameMaxLength
    }

    public static func isValidDeviceName(_ value: String) -> Bool {
        let count = trimmed(value).unicodeScalars.count
        return count >= 1 && count <= deviceNameMaxLength
    }

    /// 显示名：昵称，为空取邮箱 @ 前部分。
    public static func displayName(_ user: CloudUser) -> String {
        let nickname = trimmed(user.nickname)
        if !nickname.isEmpty { return nickname }
        return user.email.split(separator: "@", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? user.email
    }

    /// 头像字符：显示名首个字符大写；空串 = nil。
    public static func avatarInitial(_ user: CloudUser) -> String? {
        trimmed(displayName(user)).first.map { String($0).uppercased() }
    }

    // MARK: Origin ID

    /// Origin ID 一次性修改：权益允许且尚未改过。
    public static func canEditOriginId(_ session: AgentSessionDto) -> Bool {
        session.entitlements["originIdEdit"]?.boolValue == true && !session.user.originIdChanged
    }

    /// 仅数字、≥ 10000 的 Origin ID；非法为 nil。
    public static func parseOriginId(_ value: String) -> Int64? {
        let text = trimmed(value)
        guard !text.isEmpty, text.utf8.allSatisfy({ (48 ... 57).contains($0) }), let parsed = Int64(text) else { return nil }
        // 与 Web `Number.isSafeInteger` 同界：超出 2^53-1 的值服务端也无法精确表示。
        return parsed >= minOriginId && parsed <= 9_007_199_254_740_991 ? parsed : nil
    }

    // MARK: 邮箱 / 密码

    /// `^[^\s@]+@[^\s@]+\.[^\s@]+$`（Web `EMAIL_PATTERN`）。
    public static func isValidEmail(_ value: String) -> Bool {
        let text = trimmed(value)
        let parts = text.split(separator: "@", omittingEmptySubsequences: false)
        guard parts.count == 2, !parts[0].isEmpty, !parts[1].isEmpty else { return false }
        if text.contains(where: { $0.isWhitespace }) { return false }
        // `[^\s@]+\.[^\s@]+`：域名里至少有一个不在首尾的点。
        let domain = Array(parts[1])
        guard domain.count >= 3, domain[1 ..< domain.count - 1].contains(".") else { return false }
        return true
    }

    /// 密码长度按 Unicode 码点计。
    public static func passwordLength(_ value: String) -> Int { value.unicodeScalars.count }

    /// 校验新密码，返回第一个违反规则的文案键；通过为 nil。`current` 仅「当前密码模式」传入，用于拒绝新旧相同。
    public static func newPasswordErrorKey(new: String, confirm: String, current: String?) -> String? {
        if passwordLength(new) < minPasswordLength { return "accountErrorPasswordTooShort" }
        if new != confirm { return "accountPasswordMismatch" }
        if let current, new == current { return "accountPasswordSameAsCurrent" }
        return nil
    }

    // MARK: 验证码倒计时

    /// 已发出验证码的记录（跨对话框实例恢复倒计时，云端 60s 限频、码仍有效）。
    public struct SentCode: Sendable, Hashable {
        /// 发码时刻（Unix 毫秒）。
        public var sentAtMs: Int64
        public var ttlSeconds: Int64

        public init(sentAtMs: Int64, ttlSeconds: Int64) {
            self.sentAtMs = sentAtMs
            self.ttlSeconds = ttlSeconds
        }
    }

    public struct RestoredCode: Sendable, Hashable {
        public var ttlSeconds: Int64
        /// 验证码剩余有效秒数（≥ 0）。
        public var remaining: Int64
        /// 重发冷却剩余秒数（0 = 可重发）。
        public var cooldown: Int64

        public init(ttlSeconds: Int64, remaining: Int64, cooldown: Int64) {
            self.ttlSeconds = ttlSeconds
            self.remaining = remaining
            self.cooldown = cooldown
        }
    }

    /// 按记录恢复倒计时；有效期与冷却都已归零返回 nil。
    public static func restore(_ record: SentCode, nowMs: Int64) -> RestoredCode? {
        let elapsed = Double(max(0, nowMs - record.sentAtMs)) / 1000
        let remaining = Int64((Double(record.ttlSeconds) - elapsed).rounded(.up))
        let cooldown = Int64((Double(min(Int64(resendCooldownSeconds), record.ttlSeconds)) - elapsed).rounded(.up))
        if remaining <= 0, cooldown <= 0 { return nil }
        return RestoredCode(ttlSeconds: record.ttlSeconds, remaining: max(0, remaining), cooldown: max(0, cooldown))
    }

    /// 重发冷却剩余秒数：验证码剩余有效期里，发码后前 60 秒内不可重发。
    public static func resendCooldown(remaining: Int64, ttlSeconds: Int64) -> Int64 {
        max(0, remaining - max(0, ttlSeconds - Int64(resendCooldownSeconds)))
    }

    // MARK: 错误 → 文案键

    /// 先按 `reason`（agent 已把云端错误码归一化），缺失 / 未映射再按 `code` 与上下文回退。
    public static func errorKey(_ error: HostError, context: AccountErrorContext = .general) -> String {
        if let reason = error.reason, let key = errorKey(forReason: reason, context: context) { return key }
        return codeKey(error, context: context)
    }

    /// `reason` wire 名 → 文案键；未映射（含 `unknown` 与插件市场 / Doctor / 网关类）为 nil。
    public static func errorKey(forReason reason: String, context: AccountErrorContext = .general) -> String? {
        switch reason {
        case "invalidCredentials": "accountErrorInvalidCredentials"
        case "wrongPassword": "accountErrorWrongPassword"
        case "passwordChanged": "accountSessionRevokedPasswordChanged"
        case "invalidVerificationCode": "accountErrorInvalidCode"
        case "rateLimited": "accountErrorRateLimited"
        case "emailTaken": "accountErrorEmailTaken"
        case "originIdTaken": "accountOriginIdErrorTaken"
        case "originIdChangeNotAllowed": "accountOriginIdErrorNotAllowed"
        case "originIdAlreadyChanged": "accountOriginIdErrorAlreadyChanged"
        case "accountDisabled": "accountErrorAccountDisabled"
        case "registrationClosed": "accountErrorRegistrationClosed"
        case "registrationIncomplete": "accountErrorRegistrationIncomplete"
        case "mailNotConfigured": "errReasonMailNotConfigured"
        case "deviceLimit": "accountErrorDeviceLimit"
        case "onlineDeviceLimit": "cloudSyncErrorOnlineLimit"
        case "deviceUntrusted": context == .sync ? "cloudSyncErrorDeviceUntrusted" : "accountErrorDeviceUntrusted"
        case "sessionExpired": "errReasonSessionExpired"
        case "cloudUnreachable": context == .sync ? "cloudSyncErrorNetwork" : "accountErrorNetwork"
        case "pairingCodeInvalid": "errReasonPairingCodeInvalid"
        case "pairingSessionExpired": "errReasonPairingSessionExpired"
        case "pairingPeerUnreachable": "errReasonPairingPeerUnreachable"
        case "pairingNotFluxDown": "errReasonPairingNotFluxDown"
        case "pairingThrottled": "errReasonPairingThrottled"
        case "pairingRejected": "errReasonPairingRejected"
        case "pairingSignatureInvalid": "errReasonPairingSignatureInvalid"
        case "pairingSelf": "errReasonPairingSelf"
        case "pairingVersionMismatch": "errReasonPairingVersionMismatch"
        case "pairingTimeout": "errReasonPairingTimeout"
        case "peerClockSkew": "errReasonPeerClockSkew"
        case "peerNotPaired": "errReasonPeerNotPaired"
        case "peerOffline": "errReasonPeerOffline"
        case "peerOutdated": "errReasonPeerOutdated"
        case "linkUnavailable": "errReasonLinkUnavailable"
        case "targetDeviceOffline": "errReasonTargetDeviceOffline"
        case "taskStateConflict": "errReasonTaskStateConflict"
        case "taskDeviceMismatch": "errReasonTaskDeviceMismatch"
        case "saveDirUnavailable": "errReasonSaveDirUnavailable"
        default: nil
        }
    }

    private static func codeKey(_ error: HostError, context: AccountErrorContext) -> String {
        let network = error.code == .unavailable && error.retryable
        switch context {
        case .login:
            switch error.code {
            case .unauthorized: return "accountErrorInvalidCredentials"
            case .invalidArgument: return "accountErrorValidation"
            case .conflict: return "accountErrorRegistrationIncomplete"
            default: if network { return "accountErrorNetwork" }
            }
        case .register:
            switch error.code {
            case .conflict: return "accountErrorEmailTaken"
            case .invalidArgument: return "accountErrorValidation"
            case .unsupported: return "accountErrorRegistrationClosed"
            default: if network { return "accountErrorNetwork" }
            }
        case .code:
            switch error.code {
            case .unauthorized, .invalidArgument, .notFound: return "accountErrorInvalidCode"
            default: if network { return "accountErrorNetwork" }
            }
        case .sync:
            if network { return "cloudSyncErrorNetwork" }
        case .pairing:
            switch error.code {
            case .invalidArgument: return "localPairingAddressInvalid"
            // 不带 reason 的 `unavailable` 是本机服务 / 会话问题，不是「对端地址不可达」（那种情形 agent 会带 `pairingPeerUnreachable`）。
            default: break
            }
        case .general:
            break
        }
        return genericKey(error.code)
    }

    private static func genericKey(_ code: HostErrorCode) -> String {
        switch code {
        case .unavailable, .timeout: "localServiceDisconnected"
        case .invalidArgument, .notFound: "localServiceInvalidArgument"
        case .conflict: "localServiceConflict"
        case .unsupported: "settingsUnsupportedOnPlatform"
        case .protocolIncompatible, .unauthorized, .cancelled, .internal: "localServiceActionFailed"
        }
    }

    /// 服务端要求先完成注册验证：登录流程据此引导到注册验证步骤。
    public static func isRegistrationIncomplete(_ error: HostError) -> Bool {
        switch error.reason {
        case "registrationIncomplete"?: true
        // 旧 agent 不带 reason，云端 `registration_incomplete` 折成 `conflict`。
        case nil: error.code == .conflict
        default: false
        }
    }

    /// `sessionRevoked` 通知（载荷 = `ErrorReason` wire 名）→ 一次性提示文案键。
    public static func sessionRevokedKey(reason: String) -> String {
        switch reason {
        case "deviceUntrusted": "accountSessionRevokedUntrusted"
        case "passwordChanged": "accountSessionRevokedPasswordChanged"
        case "accountDisabled": "accountErrorAccountDisabled"
        default: "accountSessionRevokedExpired"
        }
    }

    // MARK: 套餐徽标

    /// `{badge}` 或 `{badge} No.{序号补零到 digits(1…6)}`；badge 为空 = nil（不显示）。
    public static func planBadgeText(_ plan: CloudPlan, ordinal: Int64?) -> String? {
        guard let base = plan.badge.map(trimmed), !base.isEmpty else { return nil }
        guard plan.badgeNumbered, let ordinal else { return base }
        let width = min(6, max(1, plan.badgeNumberDigits))
        let digits = String(ordinal)
        let padded = String(repeating: "0", count: max(0, width - digits.count)) + digits
        return "\(base) No.\(padded)"
    }

    /// 徽标颜色：接受可选 `#` 前缀的 6 位 `RRGGBB` 或 8 位 `AARRGGBB`；非法 nil。分量 0…1。
    public struct BadgeColor: Sendable, Hashable {
        public var red: Double
        public var green: Double
        public var blue: Double
        public var alpha: Double
    }

    public static func parseBadgeColor(_ value: String) -> BadgeColor? {
        var hex = trimmed(value)
        if hex.hasPrefix("#") { hex.removeFirst() }
        guard hex.utf8.allSatisfy({ ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 70) || ($0 >= 97 && $0 <= 102) }) else { return nil }
        guard hex.count == 6 || hex.count == 8, let number = UInt32(hex, radix: 16) else { return nil }
        let hasAlpha = hex.count == 8
        let alpha = hasAlpha ? Double((number >> 24) & 0xFF) / 255 : 1
        return BadgeColor(
            red: Double((number >> 16) & 0xFF) / 255,
            green: Double((number >> 8) & 0xFF) / 255,
            blue: Double(number & 0xFF) / 255,
            alpha: alpha
        )
    }
}

