import Foundation

/// 界面文案：直接读取仓库根 `assets/i18n/{en,zh,zh-TW}.json`（GPUI / Web / Android 共用的同一份基线，
/// 包内经 `Resources/i18n` 符号链接打包，Swift 侧不手写同名文案）。
///
/// 语义对齐 `crates/i18n`：按 locale 查表，键缺失或值为空回退英文；`{name}` 占位按参数替换。
/// 语言 = 系统首选语言（iOS「设置 › FluxDown › 语言」的按 App 语言同样生效）。
public final class L10n: Sendable {
    public static let shared = L10n()

    /// 当前语言代码，与 JSON 文件名一致（如 `en` / `zh` / `zh-TW`）。
    public let locale: String
    private let table: [String: String]
    private let fallback: [String: String]

    private init() {
        let available = Self.availableLocales()
        locale = Self.resolve(preferred: Locale.preferredLanguages, available: available)
        fallback = Self.load("en")
        table = locale == "en" ? fallback : Self.load(locale)
    }

    /// 测试用：指定语言。
    public init(locale: String) {
        self.locale = Self.resolve(preferred: [locale], available: Self.availableLocales())
        fallback = Self.load("en")
        table = self.locale == "en" ? fallback : Self.load(self.locale)
    }

    /// 查表；缺键回退英文，再缺返回键名本身（便于发现漏键）。
    public func callAsFunction(_ key: String) -> String {
        if let value = table[key], !value.isEmpty { return value }
        if let value = fallback[key], !value.isEmpty { return value }
        return key
    }

    /// 查表并替换 `{name}` 占位。
    public func callAsFunction(_ key: String, _ args: [String: CustomStringConvertible]) -> String {
        Self.fill(self(key), args)
    }

    public func has(_ key: String) -> Bool { table[key] != nil || fallback[key] != nil }

    public static func fill(_ template: String, _ args: [String: CustomStringConvertible]) -> String {
        var out = template
        for (name, value) in args {
            out = out.replacingOccurrences(of: "{\(name)}", with: value.description)
        }
        return out
    }

    static func resolve(preferred: [String], available: Set<String>) -> String {
        for language in preferred {
            let code = language.trimmingCharacters(in: .whitespacesAndNewlines)
                .replacingOccurrences(of: "_", with: "-").lowercased()
            // 保留实际文件名大小写，避免大小写敏感文件系统上的 zh-TW.json 加载失败。
            if let exact = available.first(where: { $0.caseInsensitiveCompare(code) == .orderedSame }) { return exact }
            let parts = code.split(separator: "-")
            let base = String(parts.first ?? "")
            if base == "zh" {
                let chinese: String
                switch parts.dropFirst().first {
                case "hant", "tw", "hk", "mo": chinese = "zh-TW"
                default: chinese = "zh"
                }
                if let match = available.first(where: { $0.caseInsensitiveCompare(chinese) == .orderedSame }) {
                    return match
                }
                // 繁中资源缺失时仍遵循原有中文、英文回退顺序。
            }
            if let match = available.first(where: { $0.caseInsensitiveCompare(base) == .orderedSame }) { return match }
            // 同主语言的其他地区（pt-PT → pt-BR）：与 crates/i18n 的主语言匹配一致，按文件名排序取第一个。
            if let match = available.sorted().first(where: {
                $0.split(separator: "-").first.map { String($0).lowercased() } == base
            }) { return match }
        }
        return "en"
    }

    private static func directory() -> URL? {
        Bundle.module.url(forResource: "i18n", withExtension: nil)
    }

    private static func availableLocales() -> Set<String> {
        guard let dir = directory(),
              let files = try? FileManager.default.contentsOfDirectory(atPath: dir.path) else { return ["en"] }
        return Set(files.filter { $0.hasSuffix(".json") }.map { String($0.dropLast(5)) })
    }

    private static func load(_ locale: String) -> [String: String] {
        guard let url = directory()?.appendingPathComponent("\(locale).json"),
              let data = try? Data(contentsOf: url),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return [:] }
        return object.compactMapValues { $0 as? String }
    }
}

/// 全局简写：`L(键)`、`L(键, ["n": 3])`。
public func L(_ key: String) -> String { L10n.shared(key) }

public func L(_ key: String, _ args: [String: CustomStringConvertible]) -> String { L10n.shared(key, args) }
