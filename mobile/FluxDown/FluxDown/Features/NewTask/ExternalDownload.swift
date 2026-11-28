import FluxDomain
import Foundation

nonisolated struct CaptureDeviceRule: Hashable {
    static let preferenceKey = "download.capture_device_rules"

    let suffix: String
    let targetId: String

    static func normalizeSuffix(_ raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let suffix = trimmed.hasPrefix(".") ? trimmed.dropFirst() : trimmed[...]
        guard !suffix.isEmpty, suffix.utf8.allSatisfy({
            (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0)
        }) else { return nil }
        return suffix.lowercased()
    }

    static func parse(_ text: String) -> [Self] {
        text.split(whereSeparator: { $0 == "\n" || $0 == "\r\n" }).compactMap { line in
            guard let delimiter = line.firstIndex(of: "="),
                  let suffix = normalizeSuffix(String(line[..<delimiter])) else { return nil }
            let target = line[line.index(after: delimiter)...].trimmingCharacters(in: .whitespacesAndNewlines)
            guard let colon = target.firstIndex(of: ":"),
                  target[..<colon] == "cloud" || target[..<colon] == "link",
                  !target[target.index(after: colon)...].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            else { return nil }
            return Self(suffix: suffix, targetId: target)
        }
    }

    static func serialize(_ rules: [Self]) -> String {
        rules.map { "\($0.suffix)=\($0.targetId)" }.joined(separator: "\n")
    }

    static func match(url: String, fileName: String, rules: [Self]) -> String? {
        guard !rules.isEmpty, ExternalDownload.httpComponents(url) != nil,
              let schemeEnd = url.range(of: "://") else { return nil }
        var name = fileName.trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty {
            // URLComponents can escape existing percent escapes again when the path also
            // contains literal Unicode. Select the raw path segment before decoding once.
            let tail = url[schemeEnd.upperBound...]
            let end = tail.firstIndex(where: { $0 == "?" || $0 == "#" }) ?? tail.endIndex
            guard let slash = tail[..<end].lastIndex(of: "/") else { return nil }
            let segment = tail[tail.index(after: slash)..<end]
            name = segment.removingPercentEncoding ?? String(segment)
        }
        guard !name.contains("/"), !name.contains("\\"),
              let dot = name.lastIndex(of: "."), dot != name.startIndex else { return nil }
        let suffix = name[name.index(after: dot)...]
        return rules.first { rule in
            suffix.utf8.elementsEqual(rule.suffix.utf8, by: { byte, lower in
                (65...90).contains(byte) ? byte + 32 == lower : byte == lower
            })
        }?.targetId
    }
}

/// One external request. Credentials belong only to this URL and are never attached to later pasted links.
nonisolated struct ExternalDownload: Hashable {
    let url: String
    var fileName = ""
    var cookies = ""
    var referrer = ""
    var headers: [String: String] = [:]
    var hasUnreadableContext = false

    var blocksRemoteDispatch: Bool {
        let parts = URLComponents(string: url)
        return parts?.user != nil || parts?.password != nil || !cookies.isEmpty || hasUnreadableContext
            || headers.keys.contains { $0.caseInsensitiveCompare("User-Agent") != .orderedSame }
    }
    var hasRequestContext: Bool {
        blocksRemoteDispatch || !referrer.isEmpty || !headers.isEmpty
    }

    var canDispatch: Bool { Self.httpComponents(url) != nil && !blocksRemoteDispatch }

    var entry: UrlEntry? {
        if Self.httpComponents(url) != nil { return UrlEntry(url: url) }
        return parseEntries(url).first { $0.url == url }
    }

    static func httpComponents(_ text: String) -> URLComponents? {
        guard !text.contains(where: \.isWhitespace),
              let parts = URLComponents(string: text),
              let scheme = parts.scheme?.lowercased(), scheme == "https" || scheme == "http",
              parts.host?.isEmpty == false else { return nil }
        return parts
    }

    static func capture(from url: URL) -> [Self] {
        guard url.scheme?.lowercased() == "fluxdown" else { return [Self(url: url.absoluteString)] }
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        var metadata: [String: String] = [:]
        var unsupported = false
        for item in query where item.name != "url" {
            switch item.name {
            case "filename", "cookies", "referrer", "headers":
                let value = item.value ?? ""
                if metadata[item.name] != nil { unsupported = true }
                if metadata[item.name]?.isEmpty != false { metadata[item.name] = value }
            default:
                unsupported = true
            }
        }
        let fileName = (metadata["filename"] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let headersText = metadata["headers"] ?? ""
        let headers: [String: String]? = if headersText.isEmpty { [:] } else {
            try? JSONDecoder().decode([String: String].self, from: Data(headersText.utf8))
        }
        let cookies = metadata["cookies"] ?? ""
        let referrer = metadata["referrer"] ?? ""
        let links = query.filter { $0.name == "url" }.flatMap { item in
            (item.value ?? "").split(whereSeparator: \.isNewline)
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { !$0.isEmpty }
        }
        guard let first = links.first else { return [Self(url: url.absoluteString)] }
        if fileName.isEmpty, cookies.isEmpty, referrer.isEmpty, headers?.isEmpty == true, !unsupported {
            return links.map { Self(url: $0) }
        }
        // The envelope's metadata describes only its first URL, never the entire batch.
        return [Self(url: first, fileName: fileName, cookies: cookies, referrer: referrer,
                     headers: headers ?? [:], hasUnreadableContext: headers == nil || unsupported)]
            + links.dropFirst().map { Self(url: $0, hasUnreadableContext: true) }
    }
}

extension HostState {
    /// The current host's remote choices, identical in settings, confirmation and silent routing.
    @MainActor
    func downloadDispatchTargets(sections: AgentSections) -> [DispatchTarget] {
        let live = connection == .live
        return DeviceRules.dispatchTargets(
            cloud: cloudDevices, link: linkDevices,
            cloudPresenceKnown: CloudPresence.isKnown(sections.connection(self), localReady: live),
            localReady: live
        )
    }
}

/// A queued confirmation is distinct even when the same URL was opened twice.
nonisolated struct ExternalDownloadConfirmation: Hashable {
    let id = UUID()
    let request: ExternalDownload
    let targetId: String?
    var failure: String?
}
