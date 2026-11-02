import Foundation

/// 新建下载「分类」选择器的模式：自动随文件名预测 / 不分类 / 固定到某个分类目录。
public enum SaveDirCategoryMode: Sendable, Hashable {
    case auto
    /// 不分类（契约里的 None；避开与 `Optional.none` 重名）。
    case noCategory
    case category(String)
}

/// 新建下载表单「分类 → 保存目录」的纯规则（四端同一契约；匹配语义唯一来源是 Rust `route_save_dir`，
/// 经 `HostSession.categorySaveDir` 取得预测目录，这里只负责选项 / 自动跟随 / 模式切换的判定）。
public struct SaveDirCategoryState: Sendable, Equatable {
    public private(set) var mode: SaveDirCategoryMode = .auto
    /// 表单最近一次由程序写入目录框的值；目录框等于它视为用户没有手动改过。
    public private(set) var lastWritten: String?
    /// 保存目录由用户手动编辑/选择后，自动预测不再覆盖，提交也不再请求自动路由。
    public private(set) var hasManualSaveDir = false

    /// - Parameter initialDir: 打开表单时目录框的值（外部预填的分类目录也视为自动写入值）。
    public init(initialDir: String = "") {
        let dir = Self.trimmed(initialDir)
        lastWritten = dir.isEmpty ? nil : dir
    }

    // MARK: 选项

    /// 下拉里的具体分类：可见、非「全部」、目录非空，按 `position` 排序（含配了目录的「其他」）。
    /// 非空 ⇔ 分类路由存在（Rust `CategoryRouteDto::from_categories` 为 Some），选择器据此显隐。
    public static func options(_ categories: [CustomCategoryDto]) -> [CustomCategoryDto] {
        categories
            .filter { $0.visible && !$0.isAll && !trimmed($0.saveDir).isEmpty }
            .enumerated()
            .sorted { ($0.element.position, $0.offset) < ($1.element.position, $1.offset) }
            .map(\.element)
    }

    /// 命中分类 = 选项里第一个目录等于预测目录的项。
    public static func matched(_ options: [CustomCategoryDto], predicted: String?) -> CustomCategoryDto? {
        guard let predicted = predicted.map(trimmed), !predicted.isEmpty else { return nil }
        return options.first { trimmed($0.saveDir) == predicted }
    }

    /// 预测输入的文件名：重命名框非空优先，否则第一条链接已识别的文件名（可为空串，交给 URL 末段回退）。
    public static func predictionName(rename: String, entryFileName: String) -> String {
        let renamed = trimmed(rename)
        return renamed.isEmpty ? trimmed(entryFileName) : renamed
    }

    // MARK: 状态转移

    /// 目录框是否仍是系统给的值；空或等于 `lastWritten`，尚未写入则比较初始目录。
    public func untouched(dir: String, offered: String) -> Bool {
        let current = Self.trimmed(dir)
        if current.isEmpty { return true }
        if let last = lastWritten, !last.isEmpty { return current == last }
        return current == Self.trimmed(offered)
    }

    /// 预测输入变化（含打开表单）：「自动」模式下目录框未被手改（见 `untouched`）时跟随预测；
    /// 返回应写入目录框的新值，nil = 不改（非「自动」或用户手动改过目录，含显式改回默认目录）。
    public mutating func follow(dir: String, offered: String, predicted: String?) -> String? {
        guard mode == .auto, !hasManualSaveDir, untouched(dir: dir, offered: offered) else { return nil }
        let guess = Self.trimmed(predicted ?? "")
        let next = guess.isEmpty ? Self.trimmed(offered) : guess
        lastWritten = next
        return next
    }

    /// 标记目录框的显式编辑或目录选择；即使选中的值等于初始目录也必须阻止路由。
    public mutating func markSaveDirManual() {
        hasManualSaveDir = true
    }

    /// 用户选了新模式：返回应写入目录框的新值，nil = 选择无效（所选分类已不在选项里），模式保持不变。
    /// - 自动：立即按当前输入重算（无条件写入）；
    /// - 不分类：回到初始目录；
    /// - 分类：该分类目录（用户显式选择优先，不论文件类型是否相符）。
    public mutating func select(
        _ newMode: SaveDirCategoryMode,
        offered: String,
        options: [CustomCategoryDto],
        predicted: String?
    ) -> String? {
        let base = Self.trimmed(offered)
        switch newMode {
        case .auto:
            mode = .auto
            hasManualSaveDir = false
            let guess = Self.trimmed(predicted ?? "")
            let next = guess.isEmpty ? base : guess
            lastWritten = next
            return next
        case .noCategory:
            mode = .noCategory
            lastWritten = base
            return base
        case let .category(id):
            guard let entry = options.first(where: { $0.id == id }) else { return nil }
            mode = newMode
            let next = Self.trimmed(entry.saveDir)
            lastWritten = next
            return next
        }
    }

    /// 提交用 offeredSaveDir：仅自动模式且未手动选择目录时请求自动路由。
    public func offeredSaveDir(dir: String, initial: String) -> String? {
        mode == .auto && !hasManualSaveDir && untouched(dir: dir, offered: initial) ? initial : nil
    }

    private static func trimmed(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
