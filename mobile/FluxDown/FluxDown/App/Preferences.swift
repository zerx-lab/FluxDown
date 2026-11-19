import FluxDomain
import FluxUI
import Foundation
import SwiftUI

/// 明暗模式：`appearance.theme_mode`。
nonisolated enum ThemeMode: String, CaseIterable, Identifiable {
    case system, light, dark

    var id: String { rawValue }

    /// 根视图 `preferredColorScheme`：跟随系统 = nil。
    var colorScheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }
}

/// 外观偏好的设备本地副本（键名与 Android `AppearanceRepo` 一致）。`appearance.theme_mode / color_scheme /
/// custom_color` 是云同步键：`Features/Settings/AppearanceSync` 把它们与主机 `agent.preferences` 双向同步，
/// 本地 `UserDefaults` 只作冷启动与离线时的回退。
@MainActor
@Observable
final class AppearanceStore {
    private enum Key {
        static let mode = "appearance.theme_mode"
        static let scheme = "appearance.color_scheme"
        static let custom = "appearance.custom_color"
        static let fileSizeUnits = "appearance.file_size_units"
    }

    @ObservationIgnored private let defaults: UserDefaults

    var mode: ThemeMode { didSet { defaults.set(mode.rawValue, forKey: Key.mode) } }
    /// blue / green / violet / rose / orange / indigo / custom。
    var scheme: String { didSet { defaults.set(scheme, forKey: Key.scheme) } }
    /// 自定义强调色（0xRRGGBB）。
    var customColor: UInt32 { didSet { defaults.set(Int(customColor), forKey: Key.custom) } }
    /// Size labels are device-local and are not part of appearance sync.
    var fileSizeUnits: FileSizeUnits {
        didSet { defaults.set(fileSizeUnits.rawValue, forKey: Key.fileSizeUnits) }
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        mode = ThemeMode(rawValue: defaults.string(forKey: Key.mode) ?? "") ?? .system
        scheme = defaults.string(forKey: Key.scheme) ?? FluxAccent.blue.id
        customColor = UInt32(truncatingIfNeeded: defaults.object(forKey: Key.custom) as? Int ?? Int(FluxAccent.blue.rgb))
        fileSizeUnits = FileSizeUnits(rawValue: defaults.string(forKey: Key.fileSizeUnits) ?? "") ?? .legacy
    }

    /// 当前强调色：自定义 > 预设。
    var accent: FluxAccent {
        scheme == "custom" ? FluxAccent(id: "custom", rgb: customColor) : FluxAccent.preset(scheme)
    }

    func setCustom(_ rgb: UInt32) {
        customColor = rgb
        scheme = "custom"
    }
}

/// 列表分组（PC 视图弹层同序）。
nonisolated enum GroupBy: String, CaseIterable, Identifiable {
    case none, status, date, type, queue, site, group
    var id: String { rawValue }
}

/// 排序键；smart = 智能排序（活跃优先 → 最近）。
nonisolated enum SortKey: String, CaseIterable, Identifiable {
    case smart, created, name, size, progress, speed, status
    var id: String { rawValue }
}

nonisolated enum Density: String, CaseIterable, Identifiable {
    case comfortable, compact
    var id: String { rawValue }
}

/// 卡片显示字段（PC「列」换算）。
nonisolated enum CardField: String, CaseIterable, Identifiable {
    case size, speed, eta, `protocol`, site, queue, created
    var id: String { rawValue }
}

/// 下载列表视图偏好（设备本地，不随主机切换、不上云；键名同 Android `ViewPrefsRepo`）。
@MainActor
@Observable
final class ViewPrefsStore {
    private enum Key {
        static let group = "ui.downloads.group_by"
        static let sort = "ui.downloads.sort_key"
        static let dir = "ui.downloads.sort_dir"
        static let density = "ui.downloads.density"
        static let fields = "ui.downloads.card_fields"
    }

    static let defaultFields: [CardField] = [.size, .speed, .eta, .protocol]

    @ObservationIgnored private let defaults: UserDefaults

    var groupBy: GroupBy { didSet { defaults.set(groupBy.rawValue, forKey: Key.group) } }
    var sortKey: SortKey { didSet { defaults.set(sortKey.rawValue, forKey: Key.sort) } }
    var ascending: Bool { didSet { defaults.set(ascending ? "asc" : "desc", forKey: Key.dir) } }
    var density: Density { didSet { defaults.set(density.rawValue, forKey: Key.density) } }
    /// 有序、去重。
    var fields: [CardField] { didSet { defaults.set(fields.map(\.rawValue).joined(separator: ","), forKey: Key.fields) } }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        groupBy = GroupBy(rawValue: defaults.string(forKey: Key.group) ?? "") ?? .none
        sortKey = SortKey(rawValue: defaults.string(forKey: Key.sort) ?? "") ?? .smart
        ascending = defaults.string(forKey: Key.dir) == "asc"
        density = Density(rawValue: defaults.string(forKey: Key.density) ?? "") ?? .comfortable
        if let raw = defaults.string(forKey: Key.fields) {
            var seen = Set<CardField>()
            fields = raw.split(separator: ",").compactMap { CardField(rawValue: String($0)) }.filter { seen.insert($0).inserted }
        } else {
            fields = Self.defaultFields
        }
    }

    /// 与默认不同 → 视图按钮显示 on 态。
    var isCustomized: Bool { groupBy != .none || sortKey != .smart || density != .comfortable }

    func toggle(_ field: CardField) {
        if let index = fields.firstIndex(of: field) {
            fields.remove(at: index)
        } else {
            fields = CardField.allCases.filter { $0 == field || fields.contains($0) }
        }
    }
}
