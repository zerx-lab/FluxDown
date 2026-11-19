import Foundation

/// 自定义分类（`CustomCategoryDto`，偏好键 `custom_categories`）。
/// 列表由主机快照下发（Rust `CustomCategoryDto::from_preference`：空 / 损坏回退内置基线），
/// Swift 只做匹配，不另维护分类规则来源。
public struct TaskCategory: Sendable, Hashable, Identifiable {
    public var id: String
    /// 内置分类为空串：显示名走 i18n（`categoryVideo` …）。
    public var name: String
    public var icon: String
    public var extensions: [String]
    /// 非空 = 正则模式（`matchMode = regex`）。
    public var regexPattern: String
    /// 非空 = 网址模式（`matchMode = url`）的通配符模式（桥接层只在 url 模式下下发）。
    public var urlPattern: String
    public var position: Int32
    public var visible: Bool
    /// all / video / audio / document / image / program / archive / other；自定义为 nil。
    public var builtinType: String?

    public init(
        id: String,
        name: String,
        icon: String,
        extensions: [String],
        regexPattern: String = "",
        urlPattern: String = "",
        position: Int32,
        visible: Bool = true,
        builtinType: String? = nil
    ) {
        self.id = id
        self.name = name
        self.icon = icon
        self.extensions = extensions
        self.regexPattern = regexPattern
        self.urlPattern = urlPattern
        self.position = position
        self.visible = visible
        self.builtinType = builtinType
    }

    public var isAll: Bool { builtinType == "all" }
    public var isOther: Bool { builtinType == "other" }

    /// 与 `native/protocol/src/agent.rs::CustomCategoryDto::builtin_defaults` 同序同扩展名
    /// （主机未下发时的展示基线）。
    public static let builtin: [TaskCategory] = [
        make("all", "folders", [], 0),
        make("video", "film", ["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "m3u8"], 1),
        make("audio", "music", ["mp3", "flac", "wav", "aac", "ogg", "m4a", "wma", "opus"], 2),
        make("document", "fileText", ["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "epub", "md"], 3),
        make("image", "image", ["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "heic", "avif"], 4),
        make("program", "cpu", ["exe", "msi", "dmg", "pkg", "deb", "rpm", "apk", "appimage"], 5),
        make("archive", "archive", ["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso"], 6),
        make("other", "file", [], 7),
    ]

    private static func make(_ type: String, _ icon: String, _ exts: [String], _ position: Int32) -> TaskCategory {
        TaskCategory(id: "builtin_\(type)", name: "", icon: icon, extensions: exts, position: position, builtinType: type)
    }
}

/// 网址通配符（`*` 任意串、`?` 单字符，其余字面；不分大小写；须完整匹配整个网址）：
/// 与 agent `category_dir.rs` / 引擎 `wildcard_matches` 同语义。空白 / 非法模式编译为 nil（不命中）。
public enum UrlWildcard {
    public static func compile(_ pattern: String) -> NSRegularExpression? {
        let trimmed = pattern.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return nil }
        var source = "^"
        for ch in trimmed {
            switch ch {
            case "*": source += ".*"
            case "?": source += "."
            default: source += NSRegularExpression.escapedPattern(for: String(ch))
            }
        }
        source += "\\z"
        return try? NSRegularExpression(pattern: source, options: [.caseInsensitive])
    }

    public static func matches(_ regex: NSRegularExpression, _ url: String) -> Bool {
        regex.firstMatch(in: url, range: NSRange(url.startIndex..., in: url)) != nil
    }
}

/// 按 position 排序的匹配索引（语义同 GPUI `CategoryIndex` / Android `CategoryIndex`）：
/// 网址规则固定优先（规则间按列表顺序），其后是扩展名 / 正则规则（列表顺序）；都不命中 → other。
/// 网址规则匹配「来源地址」（调用方传 `DownloadTask.shareUrl`：`originUrl` 非空取它，否则取 `url`）；
/// 文件名未知（空串）时只有网址规则参与。规则（含正则 / 通配符编译）在构造时一次编译。
public struct CategoryIndex: Sendable {
    public let ordered: [TaskCategory]
    private let other: TaskCategory?
    private let urlRules: [Rule]
    private let nameRules: [Rule]

    private struct Rule: Sendable {
        enum Kind: Sendable {
            case url(NSRegularExpression?)
            case regex(NSRegularExpression?)
            case extensions(Set<String>)
        }

        let category: TaskCategory
        let firstWithID: Bool
        let kind: Kind

        var isUrl: Bool {
            if case .url = kind { return true }
            return false
        }

        func matches(_ fileName: String, url: String) -> Bool {
            switch kind {
            case let .url(regex):
                guard let regex else { return false }
                return UrlWildcard.matches(regex, url)
            case let .regex(regex):
                guard !fileName.isEmpty, let regex else { return false }
                let range = NSRange(fileName.startIndex..., in: fileName)
                return regex.firstMatch(in: fileName, range: range) != nil
            case let .extensions(extensions):
                return extensions.contains(Self.extensionOf(fileName))
            }
        }

        static func extensionOf(_ name: String) -> String {
            guard let dot = name.lastIndex(of: ".") else { return "" }
            return name[name.index(after: dot)...].lowercased()
        }
    }

    public init(_ categories: [TaskCategory]) {
        ordered = categories.sorted { $0.position < $1.position }
        other = ordered.first { $0.isOther }
        var seen = Set<String>()
        let rules: [Rule] = ordered.compactMap { category in
            let first = seen.insert(category.id).inserted
            guard !category.isAll, !category.isOther else { return nil }
            if !category.urlPattern.isEmpty {
                return Rule(category: category, firstWithID: first, kind: .url(UrlWildcard.compile(category.urlPattern)))
            }
            if !category.regexPattern.isEmpty {
                return Rule(
                    category: category,
                    firstWithID: first,
                    kind: .regex(try? NSRegularExpression(pattern: category.regexPattern, options: [.caseInsensitive]))
                )
            }
            let exts = Set(category.extensions.map { ext in
                let lower = ext.lowercased()
                return lower.hasPrefix(".") ? String(lower.dropFirst()) : lower
            })
            return Rule(category: category, firstWithID: first, kind: .extensions(exts))
        }
        urlRules = rules.filter(\.isUrl)
        nameRules = rules.filter { !$0.isUrl }
    }

    /// 任务命中的首个具体分类：网址规则优先，其后扩展名 / 正则；都不命中 → other（无 other 时 nil）。
    public func categoryOf(_ fileName: String, url: String = "") -> TaskCategory? {
        urlRules.first { $0.matches(fileName, url: url) }?.category
            ?? nameRules.first { $0.matches(fileName, url: url) }?.category
            ?? other
    }

    /// 种子成员没有独立来源地址，只按文件名匹配。
    public func categoryOfFileName(_ fileName: String) -> TaskCategory? {
        let hasExtension = fileName.lastIndex(of: ".").map { dot in
            dot != fileName.startIndex && fileName.index(after: dot) != fileName.endIndex
        } ?? false
        return nameRules.first { rule in
            guard rule.firstWithID else { return false }
            if case .extensions = rule.kind, !hasExtension { return false }
            return rule.matches(fileName, url: "")
        }?.category ?? other
    }

    /// 任务是否属于分类（all 恒真；other = 未命中任何具体分类）。
    public func matches(_ category: TaskCategory, _ fileName: String, url: String = "") -> Bool {
        if category.isAll { return true }
        if category.isOther {
            return !urlRules.contains { $0.matches(fileName, url: url) }
                && !nameRules.contains { $0.matches(fileName, url: url) }
        }
        let rule = urlRules.first { $0.category.id == category.id } ?? nameRules.first { $0.category.id == category.id }
        return rule?.matches(fileName, url: url) ?? false
    }
}
