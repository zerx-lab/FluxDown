import FluxDomain
import Foundation

// 纯逻辑：aria2 风格多行链接解析、预览推断、N2 预设与 wire 拼装（同 Android `LinkParser.kt`）。
// App target 默认 MainActor 隔离；这里的值类型与函数都是纯计算，显式 `nonisolated` 以便后台 / 测试直接调用。

/// 一条解析出的下载条目。
nonisolated struct UrlEntry: Sendable, Hashable {
    var url: String
    var fileName: String = ""
    var checksum: String = ""
}

private nonisolated let schemes = ["https://", "http://", "ftp://"]
private nonisolated let trailingPunctuation: Set<Character> = [".", ",", ";", ":", "!", "?", "(", ")", "[", "]", "{", "}"]

/// 解析多行文本为下载条目。
/// - 以空格 / Tab 开头的行是选项行，附着到上一条（`out=` / `checksum=`）；
/// - `#` 开头为注释，空行跳过；
/// - 含 `magnet:?` / `ed2k://` 时从该位置截取到行尾；
/// - 其余行：`loose`（TXT 导入）取行内首个 `http(s)/ftp` 链接并去尾部标点，否则要求链接位于行首。
nonisolated func parseEntries(_ text: String, loose: Bool = false) -> [UrlEntry] {
    var entries: [UrlEntry] = []
    var current: UrlEntry?
    // Swift 把 "\r\n" 当作单个字符，所以用 components 而非 split(separator:)。
    for line in text.components(separatedBy: "\n") {
        if line.hasPrefix(" ") || line.hasPrefix("\t") {
            guard var entry = current else { continue }
            let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.hasPrefix("out=") {
                entry.fileName = String(trimmed.dropFirst("out=".count))
            } else if trimmed.hasPrefix("checksum=") {
                entry.checksum = String(trimmed.dropFirst("checksum=".count))
            }
            current = entry
            continue
        }
        let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed.hasPrefix("#") { continue }
        if let done = current { entries.append(done) }
        if let magnet = trimmed.range(of: "magnet:?", options: .caseInsensitive) {
            current = UrlEntry(url: String(trimmed[magnet.lowerBound...]))
        } else if let ed2k = trimmed.range(of: "ed2k://", options: .caseInsensitive) {
            current = UrlEntry(url: String(trimmed[ed2k.lowerBound...]))
        } else if loose {
            if var found = findHttpUrl(trimmed) {
                while let last = found.last, trailingPunctuation.contains(last) { found.removeLast() }
                current = found.isEmpty ? nil : UrlEntry(url: found)
            } else {
                current = nil
            }
        } else {
            current = leadingHttpUrl(trimmed).map { UrlEntry(url: $0) }
        }
    }
    if let done = current { entries.append(done) }
    return entries
}

private nonisolated func schemeLength(at index: String.Index, in text: String) -> Int? {
    let tail = text[index...]
    for scheme in schemes where tail.range(of: scheme, options: [.anchored, .caseInsensitive]) != nil {
        return scheme.count
    }
    return nil
}

/// `^(https?|ftp)://\S+`（忽略大小写）。
private nonisolated func leadingHttpUrl(_ text: String) -> String? {
    guard let schemeLen = schemeLength(at: text.startIndex, in: text) else { return nil }
    let bodyStart = text.index(text.startIndex, offsetBy: schemeLen)
    let body = text[bodyStart...]
    let end = body.firstIndex(where: \.isWhitespace) ?? text.endIndex
    return end > bodyStart ? String(text[..<end]) : nil
}

private nonisolated func findHttpUrl(_ line: String) -> String? {
    var i = line.startIndex
    while i < line.endIndex {
        if let schemeLen = schemeLength(at: i, in: line) {
            let start = line.index(i, offsetBy: schemeLen)
            let end = line[start...].firstIndex(where: \.isWhitespace) ?? line.endIndex
            if end > start { return String(line[i..<end]) }
        }
        i = line.index(after: i)
    }
    return nil
}

extension Array where Element == UrlEntry {
    /// 按 URL 去重（保留首条）。
    nonisolated func dedupe() -> [UrlEntry] {
        var seen = Set<String>()
        return filter { seen.insert($0.url).inserted }
    }
}

extension UrlEntry {
    /// 条目 → aria2 风格文本（含选项行）。
    nonisolated func toText() -> String {
        var text = url
        if !fileName.isEmpty { text += "\n  out=" + fileName }
        if !checksum.isEmpty { text += "\n  checksum=" + checksum }
        return text
    }
}

/// 把条目追加到现有文本末尾（按 URL 去重，已有文本逐字保留）。返回新文本与实际追加数。
nonisolated func appendEntries(_ existing: String, _ entries: [UrlEntry]) -> (text: String, added: Int) {
    var seen = Set(parseEntries(existing).map(\.url))
    var text = existing
    while let last = text.last, last.isWhitespace { text.removeLast() }
    var added = 0
    for entry in entries where seen.insert(entry.url).inserted {
        if !text.isEmpty { text += "\n" }
        text += entry.toText()
        added += 1
    }
    return (text, added)
}

nonisolated func protocolOf(_ url: String) -> TaskProtocol {
    let lower = url.lowercased()
    if lower.hasPrefix("magnet:") || url.hasPrefix("torrent-file://") { return .bt }
    if lower.hasPrefix("ed2k://") { return .ed2k }
    if lower.hasPrefix("ftp://") || lower.hasPrefix("ftps://") { return .ftp }
    let beforeQuery = lower.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first ?? ""
    if beforeQuery.hasSuffix(".m3u8") { return .hls }
    return .http
}

private nonisolated func decode(_ s: String) -> String {
    s.replacingOccurrences(of: "+", with: "%2B").removingPercentEncoding ?? s
}

nonisolated func batchExportSize(_ url: String) -> Int64 {
    guard url.range(of: "ed2k://|file|", options: [.anchored, .caseInsensitive]) != nil else { return 0 }
    let fields = url.split(separator: "|", maxSplits: 5, omittingEmptySubsequences: false)
    guard fields.count >= 5, !fields[2].isEmpty else { return 0 }
    let hash = fields[4].utf8
    let size = fields[3]
    let digits = size.first == "+" ? size.dropFirst() : size
    guard hash.count == 32,
          hash.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }),
          digits.utf8.allSatisfy({ (48...57).contains($0) }),
          let bytes = Int64(digits), bytes <= 0x40_0000_0000 else { return 0 }
    return bytes
}

nonisolated func batchExportName(_ entry: UrlEntry) -> String {
    if !entry.fileName.isEmpty { return entry.fileName }
    if entry.url.range(of: "magnet:?", options: [.anchored, .caseInsensitive]) != nil {
        return magnetName(entry.url) ?? ""
    }
    if entry.url.range(of: "ed2k://", options: [.anchored, .caseInsensitive]) != nil {
        return ed2kName(entry.url) ?? ""
    }
    guard let scheme = entry.url.range(of: "://"),
          ["http", "https", "ftp"].contains(entry.url[..<scheme.lowerBound].lowercased()) else { return "" }
    let path = splitUrl(entry.url).path
    guard !path.hasSuffix("/"), let segment = path.split(separator: "/").last else { return "" }
    return decode(String(segment))
}

/// 预览用的推断文件名：`out=` 优先；磁力取 `dn=`，eD2K 取 `|file|名称|`，其余取路径末段（取不到用主机名）。
nonisolated func inferName(_ entry: UrlEntry) -> String {
    if !entry.fileName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return entry.fileName }
    let url = entry.url
    switch protocolOf(url) {
    case .bt:
        return magnetName(url) ?? btihOf(url).map { "magnet:" + $0 } ?? String(url.prefix(40))
    case .ed2k:
        return ed2kName(url) ?? String(url.prefix(40))
    default:
        let parts = splitUrl(url)
        let segment = parts.path.split(separator: "/", omittingEmptySubsequences: true).last.map(String.init)
        if let segment, !segment.isEmpty { return decode(segment) }
        return parts.host ?? String(url.prefix(40))
    }
}

/// 轻量拆分 `scheme://[user@]host[:port]/path[?query][#frag]`（不要求 URL 严格合法）。
private nonisolated func splitUrl(_ url: String) -> (host: String?, path: String) {
    var rest = Substring(url)
    if let hash = rest.firstIndex(of: "#") { rest = rest[..<hash] }
    if let q = rest.firstIndex(of: "?") { rest = rest[..<q] }
    guard let sep = rest.range(of: "://") else { return (nil, String(rest)) }
    rest = rest[sep.upperBound...]
    let authorityEnd = rest.firstIndex(of: "/") ?? rest.endIndex
    var authority = rest[..<authorityEnd]
    let path = String(rest[authorityEnd...])
    if let at = authority.lastIndex(of: "@") { authority = authority[authority.index(after: at)...] }
    if authority.hasPrefix("[") {
        if let close = authority.firstIndex(of: "]") { authority = authority[...close] }
    } else if let colon = authority.lastIndex(of: ":") {
        authority = authority[..<colon]
    }
    let host = String(authority)
    return (host.isEmpty ? nil : host, path)
}

private nonisolated func magnetName(_ url: String) -> String? {
    guard let q = url.firstIndex(of: "?") else { return nil }
    let query = url[url.index(after: q)...].prefix { $0 != "#" }
    for part in query.split(separator: "&") where part.hasPrefix("dn=") {
        let value = decode(String(part.dropFirst(3)))
        if !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return value }
        return nil
    }
    return nil
}

private nonisolated func ed2kName(_ url: String) -> String? {
    guard let prefix = url.range(of: "ed2k://|file|", options: [.anchored, .caseInsensitive]) else { return nil }
    let name = decode(String(url[prefix.upperBound...].prefix { $0 != "|" }))
    return name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : name
}

private nonisolated func btihOf(_ url: String) -> String? {
    guard let range = url.range(of: "btih:", options: .caseInsensitive) else { return nil }
    let hash = url[range.upperBound...].prefix { $0.isASCII && ($0.isLetter || $0.isNumber) }
    return hash.isEmpty ? nil : String(hash.prefix(12))
}

/// 站点 / 来源描述：http(s) / ftp 取 host；磁力 / eD2K 为 nil。
nonisolated func hostOrNull(_ url: String) -> String? {
    let p = protocolOf(url)
    if p == .bt || p == .ed2k { return nil }
    guard var host = splitUrl(url).host else { return nil }
    if host.lowercased().hasPrefix("www.") { host = String(host.dropFirst(4)) }
    return host
}

// MARK: 预设

nonisolated let threadPresets = [4, 8, 16, 32, 64]
nonisolated let maxThreads = 256

/// 预设 UA（key → UA）。版本基准与 GPUI 桌面端 / Web 一致。
nonisolated let taskUaPresets: [(key: String, value: String)] = [
    ("chrome", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"),
    ("firefox", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:147.0) Gecko/20100101 Firefox/147.0"),
    ("edge", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.3800.70"),
    ("safari", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15"),
]
nonisolated let taskUaDefault = "default"
nonisolated let taskUaCustom = "custom"

nonisolated func detectUaPreset(_ ua: String) -> String {
    if ua.isEmpty { return taskUaDefault }
    return taskUaPresets.first { $0.value == ua }?.key ?? taskUaCustom
}

nonisolated func uaPresetValue(_ key: String) -> String {
    taskUaPresets.first { $0.key == key }?.value ?? ""
}

/// 哈希校验算法（wire 字面量，无本地化），默认 `sha-256`。
nonisolated let hashAlgorithms = ["md5", "sha-1", "sha-256", "sha-512"]
nonisolated let defaultHashAlgorithm = "sha-256"

nonisolated func checksumSpec(algorithm: String, hash: String) -> String {
    let h = hash.trimmingCharacters(in: .whitespacesAndNewlines)
    return h.isEmpty ? "" : "\(algorithm)=\(h)"
}

/// 32 / 40 / 64 / 128 位十六进制，或留空。
nonisolated func checksumHexValid(_ hex: String) -> Bool {
    let h = hex.trimmingCharacters(in: .whitespacesAndNewlines)
    return h.isEmpty || ([32, 40, 64, 128].contains(h.count) && h.allSatisfy(\.isHexDigit))
}

/// 任务代理选择；wire 语义同桌面端 `ProxyChoice`。
nonisolated enum ProxyChoice: Sendable, CaseIterable {
    case follow, direct, system, globalManual, custom

    func wire(manualUrl: String, customUrl: String) -> String {
        switch self {
        case .follow: ""
        case .direct: "direct://"
        case .system: "system://"
        case .globalManual: manualUrl
        case .custom: customUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }
}

/// 由主机配置拼全局手动代理 URL：`type://[user[:pass]@]host:port`；host 空或端口非法 = 未配置（空串）。
nonisolated func manualProxyUrl(_ config: [String: String]) -> String {
    func cfg(_ key: String) -> String { (config[key] ?? "").trimmingCharacters(in: .whitespacesAndNewlines) }
    let host = cfg("proxy_host")
    let port = Int(cfg("proxy_port")) ?? 0
    if host.isEmpty || port <= 0 { return "" }
    let type = cfg("proxy_type").isEmpty ? "http" : cfg("proxy_type")
    let user = config["proxy_username"] ?? ""
    let password = config["proxy_password"] ?? ""
    var out = type + "://"
    if !user.isEmpty {
        out += encodeComponent(user)
        if !password.isEmpty { out += ":" + encodeComponent(password) }
        out += "@"
    }
    return out + host + ":" + String(port)
}

/// URI 组件编码：除 `A-Za-z0-9-_.!~*'()` 外全部按 UTF-8 百分号编码。
nonisolated func encodeComponent(_ value: String) -> String {
    let hex = Array("0123456789ABCDEF")
    let safe = Set("-_.!~*'()")
    var out = ""
    for byte in value.utf8 {
        let c = Character(UnicodeScalar(byte))
        if byte < 128, c.isASCII, c.isLetter || c.isNumber || safe.contains(c) {
            out.append(c)
        } else {
            out.append("%")
            out.append(hex[Int(byte >> 4)])
            out.append(hex[Int(byte & 15)])
        }
    }
    return out
}

/// 保存目录合法：留空（用主机默认）或绝对路径（posix `/…`、Windows `X:\…` / `\\…`）。
nonisolated func isValidSaveDir(_ dir: String) -> Bool {
    let d = dir.trimmingCharacters(in: .whitespacesAndNewlines)
    if d.isEmpty || d.hasPrefix("/") || d.hasPrefix("\\\\") { return true }
    let chars = Array(d)
    return chars.count >= 3 && chars[0].isASCII && chars[0].isLetter && chars[1] == ":" && (chars[2] == "\\" || chars[2] == "/")
}
