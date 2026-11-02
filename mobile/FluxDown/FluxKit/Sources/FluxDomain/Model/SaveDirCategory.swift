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
    /// 保存目录由用户手动编辑/选择后，自动预测不再覆盖，提交也不再请求 Rust 自动路由。
    public private(set) var hasManualSaveDir = false
    /// 上一次由「自动」写入目录框的值。
    public private(set) var lastAutoDir: String?

    /// - Parameter initialDir: 打开表单时目录框的值（外部预填的分类目录也视为自动写入值）。
    public init(initialDir: String = "") {
        let dir = Self.trimmed(initialDir)
        lastAutoDir = dir.isEmpty ? nil : dir
    }

    // MARK: 选项

    /// 分类选项的非空性独立于默认目录授权路由；授权路由即使无分类规则也存在。
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

    /// 预测输入变化时，「自动」模式且目录未被用户手动指定时跟随预测。
    public mutating func follow(dir: String, offered: String, predicted: String?) -> String? {
        guard mode == .auto, !hasManualSaveDir else { return nil }
        let current = Self.trimmed(dir)
        let base = Self.trimmed(offered)
        guard current.isEmpty || current == base || current == lastAutoDir else { return nil }
        let guess = Self.trimmed(predicted ?? "")
        let next = guess.isEmpty ? base : guess
        lastAutoDir = next
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
            lastAutoDir = next
            return next
        case .noCategory:
            mode = .noCategory
            return base
        case let .category(id):
            guard let entry = options.first(where: { $0.id == id }) else { return nil }
            mode = newMode
            return Self.trimmed(entry.saveDir)
        }
    }

    /// 提交用的 `offeredSaveDir`：只有未手动指定的「自动」模式交给 Rust 路由。
    public func offeredSaveDir(initial: String) -> String? {
        mode == .auto && !hasManualSaveDir ? initial : nil
    }

    private static func trimmed(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
