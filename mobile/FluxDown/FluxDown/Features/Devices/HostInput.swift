import Foundation

/// 地址解析结果。成功时 `endpoint` 恒为 `http(s)://host:port`（桥接层再规范到 `ws(s)://…/rpc`）。
nonisolated enum EndpointParse: Equatable {
    /// `host` 为不含端口的主机（IPv6 带方括号）；`cleartext` = 明文 HTTP 且非回环。
    case ok(endpoint: String, host: String, cleartext: Bool)
    case empty
    case badAddress
    case badPort
}

/// 访问密钥违规项；顺序与 `validate_access_key` 的检查顺序一致。
nonisolated enum KeyIssue: Equatable {
    case badChars, tooShort, tooLong, needsMix
}

/// 主机地址与访问密钥的输入规则（逐条移植 Android `HostInput.kt`）。
enum HostInput {
    /// 默认监听端口：`fluxdown-agent --server` 缺省 `[::]:17800`（`native/agent/src/server_mode.rs::DEFAULT_BIND`）。
    static let defaultHttpPort = 17800
    static let defaultHttpsPort = 443
    static let accessKeyMinLength = 8
    static let accessKeyMaxLength = 128

    private static let allowedSchemes: Set<String> = ["http", "https", "ws", "wss"]
    private static let secureSchemes: Set<String> = ["https", "wss"]

    /// 解析用户输入的主机地址：`host`、`host:port`、`http(s)://host[:port][/…]`（`ws(s)://` 视同）。
    /// 未写协议按 http；未写端口 http 用 17800、https 用 443（与 `agent.link.probe` 的地址规则一致）。
    /// 路径 / 查询 / 片段被丢弃：`/rpc` 由桥接层补齐，不支持带前缀路径的反向代理。
    static func parseEndpoint(_ input: String) -> EndpointParse {
        let raw = input.trimmingCharacters(in: .whitespacesAndNewlines)
        if raw.isEmpty { return .empty }

        let schemeRange = raw.range(of: "://")
        let scheme = schemeRange.map { String(raw[..<$0.lowerBound]).lowercased() } ?? "http"
        guard allowedSchemes.contains(scheme) else { return .badAddress }
        let secure = secureSchemes.contains(scheme)
        let rest = schemeRange.map { String(raw[$0.upperBound...]) } ?? raw

        let authority = rest.prefix { $0 != "/" && $0 != "?" && $0 != "#" }
        if authority.isEmpty || authority.contains("@") || authority.contains(where: \.isWhitespace) { return .badAddress }

        let host: String
        let portText: String?
        if authority.hasPrefix("[") {
            guard let close = authority.firstIndex(of: "]"), authority.distance(from: authority.startIndex, to: close) > 1 else {
                return .badAddress
            }
            host = String(authority[...close])
            let tail = authority[authority.index(after: close)...]
            if tail.isEmpty {
                portText = nil
            } else if tail.hasPrefix(":") {
                portText = String(tail.dropFirst())
            } else {
                return .badAddress
            }
            let inner = host.dropFirst().dropLast()
            guard inner.allSatisfy({ $0.isHexDigit || $0 == ":" || $0 == "." }) else { return .badAddress }
        } else {
            let colons = authority.filter { $0 == ":" }.count
            if colons > 1 { return .badAddress }
            if colons == 1, let colon = authority.firstIndex(of: ":") {
                host = String(authority[..<colon])
                portText = String(authority[authority.index(after: colon)...])
            } else {
                host = String(authority)
                portText = nil
            }
            guard isValidHostName(host) else { return .badAddress }
        }

        let port: Int
        if let portText {
            guard !portText.isEmpty, portText.allSatisfy({ $0.isASCII && $0.isNumber }) else { return .badAddress }
            guard let value = Int(portText), (1...65535).contains(value) else { return .badPort }
            port = value
        } else {
            port = secure ? defaultHttpsPort : defaultHttpPort
        }

        let endpoint = "\(secure ? "https" : "http")://\(host):\(port)"
        return .ok(endpoint: endpoint, host: host, cleartext: !secure && !isLoopback(host))
    }

    /// 访问密钥策略，逐条对齐 `native/agent/src/server_mode.rs::validate_access_key` 与
    /// `web/src/lib/token-policy.ts`：可见 ASCII、8–128 位、同时含字母与数字。
    static func validateAccessKey(_ key: String) -> KeyIssue? {
        let scalars = key.unicodeScalars
        if !scalars.allSatisfy({ (0x21...0x7E).contains($0.value) }) { return .badChars }
        if scalars.count < accessKeyMinLength { return .tooShort }
        if scalars.count > accessKeyMaxLength { return .tooLong }
        let hasLetter = scalars.contains { ($0.value >= 0x41 && $0.value <= 0x5A) || ($0.value >= 0x61 && $0.value <= 0x7A) }
        let hasDigit = scalars.contains { $0.value >= 0x30 && $0.value <= 0x39 }
        return hasLetter && hasDigit ? nil : .needsMix
    }

    /// RFC 952/1123 主机名或 IPv4 字面量：ASCII 字母数字、`-`、`_`、`.`；不以点 / 连字符起止。
    private static func isValidHostName(_ host: String) -> Bool {
        !host.isEmpty
            && host.utf16.count <= 253
            && host.allSatisfy { ($0.isASCII && ($0.isLetter || $0.isNumber)) || $0 == "-" || $0 == "_" || $0 == "." }
            && !host.hasPrefix(".") && !host.hasSuffix(".") && !host.hasPrefix("-")
    }

    private static func isLoopback(_ host: String) -> Bool {
        let h = host.lowercased()
        return h == "localhost" || h == "[::1]" || h.hasPrefix("127.")
    }
}
