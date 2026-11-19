import Foundation
import Synchronization

/// 数值与单位分离（数字与单位分别排版，避免换行把单位孤立到下一行）。
public struct Measure: Sendable, Hashable, CustomStringConvertible {
    public var value: String
    public var unit: String

    public init(_ value: String, _ unit: String) {
        self.value = value
        self.unit = unit
    }

    public var description: String { "\(value) \(unit)" }
}

/// Display labels for binary byte values.
public enum FileSizeUnits: String, Sendable, CaseIterable, Identifiable {
    case legacy, iec

    public var id: String { rawValue }
}

/// 与 Android `Format.kt` 同规则的格式化。
public enum Format {
    private static let units = Mutex(FileSizeUnits.legacy)
    private static let legacyUnits = ["B", "KB", "MB", "GB", "TB"]
    private static let iecUnits = ["B", "KiB", "MiB", "GiB", "TiB"]

    public static func setFileSizeUnits(_ value: FileSizeUnits) {
        units.withLock { $0 = value }
    }

    public static func bytes(_ n: Int64) -> Measure { bytes(n, style: units.withLock { $0 }) }

    public static func bytes(_ n: Int64, style: FileSizeUnits) -> Measure {
        if n <= 0 { return Measure("0", "B") }
        var value = Double(n)
        let labels = style == .iec ? iecUnits : legacyUnits
        var index = 0
        while value >= 1024 && index < labels.count - 1 {
            value /= 1024
            index += 1
        }
        guard index > 0 else { return Measure(String(n), "B") }
        let digits = value >= 100 ? 0 : value >= 10 ? 1 : 2
        return Measure(String(format: "%.\(digits)f", locale: Locale(identifier: "en_US_POSIX"), value), labels[index])
    }

    public static func bytes(unsigned n: UInt64) -> Measure {
        bytes(Int64(clamping: n))
    }

    public static func bytes(unsigned n: UInt64, style: FileSizeUnits) -> Measure {
        bytes(Int64(clamping: n), style: style)
    }

    public static func speed(_ bps: Int64) -> Measure? { speed(bps, style: units.withLock { $0 }) }

    public static func speed(_ bps: Int64, style: FileSizeUnits) -> Measure? {
        guard bps > 0 else { return nil }
        let measure = bytes(bps, style: style)
        return Measure(measure.value, measure.unit + "/s")
    }

    public static func speedOrZero(_ bps: Int64) -> Measure {
        speed(bps) ?? Measure("0", "B/s")
    }

    public static func percent(_ fraction: Double) -> String {
        let floored = (fraction * 1000).rounded(.down) / 10
        return String(format: "%.1f%%", locale: Locale(identifier: "en_US_POSIX"), floored)
    }

    public static func etaSeconds(downloaded: Int64, total: Int64, speed: Int64) -> Int64? {
        guard speed > 0, total > 0, downloaded < total else { return nil }
        let seconds = Int64(ceil(Double(total - downloaded) / Double(speed)))
        return (1...86_400).contains(seconds) ? seconds : nil
    }

    public static let dash = "—"
}
