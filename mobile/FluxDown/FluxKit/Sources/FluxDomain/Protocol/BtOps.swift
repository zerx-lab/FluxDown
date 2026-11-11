import Foundation

// BT 任务操作的 wire DTO 与纯逻辑（镜像 `native/protocol/src/daemon.rs` 的 `BtFilesDto` / `BtTaskFileDto` /
// `SetTaskFilePrioritiesParams` / `SetTaskBtLimitsParams` / `SetTaskSuperSeedingParams` / `BtPortStatusDto` /
// `BtPortMappingDto`；用例照抄 `native/protocol/tests/protocol/bt_ops_wire.rs`）。
// 新字段一律可缺省：旧 daemon 不下发时落到「未设置」，不当成解码失败。
//
// 移动端子集：文件列表（优先级）、强制重新校验、任务限速、超级做种、端口映射状态。
// 文件重命名、移动存储、tracker 层级 / 统计、HTTP 源、种子生成选项只在桌面与 Web 提供。

// MARK: - 文件优先级

/// BT 文件优先级（与 qBittorrent 同值）：0 不下载 / 1 普通 / 6 高 / 7 最高。
public enum BtFilePriority: Int, Sendable, Hashable, CaseIterable, Identifiable {
    case skip = 0
    case normal = 1
    case high = 6
    case maximum = 7

    public var id: Int { rawValue }

    /// wire 值 → 显示级别。daemon 只下发 0 / 1 / 6 / 7；其余值就近归档（2…5 = 普通，> 7 = 最高，< 0 = 不下载）。
    public init(wire: Int) {
        switch wire {
        case ...0: self = .skip
        case 1 ... 5: self = .normal
        case 6: self = .high
        default: self = .maximum
        }
    }

    public var titleKey: String {
        switch self {
        case .skip: "btFilePrioritySkip"
        case .normal: "btFilePriorityNormal"
        case .high: "btFilePriorityHigh"
        case .maximum: "btFilePriorityMaximum"
        }
    }
}

// MARK: - 文件表（`daemon.task.btFiles`）

/// 种子内的一个文件（不含 padding 文件）。`index` 与 `daemon.task.setFilePriorities` 的 `index` 同一编号。
public struct BtTaskFile: Decodable, Sendable, Hashable, Identifiable {
    public var index: Int
    /// 种子内相对路径（`/` 分隔，已是重命名后的当前路径）。
    public var path: String
    public var size: Int64
    /// 已下载并校验通过的字节。
    public var downloaded: Int64
    /// wire 优先级（见 ``BtFilePriority``）。
    public var priority: Int

    public var id: Int { index }

    public init(index: Int, path: String, size: Int64, downloaded: Int64 = 0, priority: Int = BtFilePriority.normal.rawValue) {
        self.index = index
        self.path = path
        self.size = size
        self.downloaded = downloaded
        self.priority = priority
    }

    private enum CodingKeys: String, CodingKey {
        case index, path, size, downloaded, priority
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        index = try c.decode(Int.self, forKey: .index)
        path = try c.decodeIfPresent(String.self, forKey: .path) ?? ""
        size = try c.decodeIfPresent(Int64.self, forKey: .size) ?? 0
        downloaded = try c.decodeIfPresent(Int64.self, forKey: .downloaded) ?? 0
        priority = try c.decodeIfPresent(Int.self, forKey: .priority) ?? BtFilePriority.normal.rawValue
    }

    public var level: BtFilePriority { BtFilePriority(wire: priority) }

    /// 0...1。大小为 0 的文件没有可下载的内容，视为完成；已下载超过大小（旧数据）按 1 封顶。
    public var progress: Double {
        guard size > 0 else { return 1 }
        return min(max(Double(downloaded) / Double(size), 0), 1)
    }
}

/// `daemon.task.btFiles` 结果。`editable == false`（已完成 / 尚无元数据）时优先级只读。
public struct BtFiles: Decodable, Sendable, Hashable {
    public var editable: Bool
    public var files: [BtTaskFile]

    public init(editable: Bool = false, files: [BtTaskFile] = []) {
        self.editable = editable
        self.files = files
    }

    private enum CodingKeys: String, CodingKey {
        case editable, files
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        editable = try c.decodeIfPresent(Bool.self, forKey: .editable) ?? false
        files = try c.decodeIfPresent([BtTaskFile].self, forKey: .files) ?? []
    }
}

/// `daemon.task.setFilePriorities` 的一项。
public struct BtFilePriorityChange: Codable, Sendable, Hashable {
    public var index: Int
    public var priority: Int

    public init(index: Int, priority: BtFilePriority) {
        self.index = index
        self.priority = priority.rawValue
    }
}

/// `daemon.task.setFilePriorities`：部分更新，只带改动的文件。
public struct SetBtFilePrioritiesParams: Codable, Sendable, Hashable {
    public var taskId: String
    public var priorities: [BtFilePriorityChange]

    public init(taskId: String, priorities: [BtFilePriorityChange]) {
        self.taskId = taskId
        self.priorities = priorities
    }
}

/// 改优先级前的本地判定。
public enum BtFilePriorityPlan: Sendable, Hashable {
    /// 目标文件都已是该级别（或不存在），不下发。
    case unchanged
    /// 会让任务没有任何要下载的文件（daemon 也会以 `invalidArgument` 拒绝）：提示 `btFilesKeepOne`。
    case keepOne
    /// 只含级别真正变化的文件。
    case send([BtFilePriorityChange])
}

public enum BtFilesRules {
    /// 把 `indices` 这些文件设为 `priority`：跳过已是该级别的文件；设为「不下载」后若一个要下载的文件都不剩，拒绝。
    public static func plan(_ files: [BtTaskFile], indices: Set<Int>, to priority: BtFilePriority) -> BtFilePriorityPlan {
        let changes = files
            .filter { indices.contains($0.index) && $0.level != priority }
            .map { BtFilePriorityChange(index: $0.index, priority: priority) }
        guard !changes.isEmpty else { return .unchanged }
        if priority == .skip {
            let remaining = files.contains { !indices.contains($0.index) && $0.level != .skip }
            if !remaining { return .keepOne }
        }
        return .send(changes)
    }

    /// 乐观更新：把已下发的改动并入本地文件表（下一次轮询以主机为准）。
    public static func applying(_ changes: [BtFilePriorityChange], to files: [BtTaskFile]) -> [BtTaskFile] {
        guard !changes.isEmpty else { return files }
        var byIndex: [Int: Int] = [:]
        for change in changes { byIndex[change.index] = change.priority }
        return files.map { file in
            guard let priority = byIndex[file.index] else { return file }
            var next = file
            next.priority = priority
            return next
        }
    }

    /// 行内读数「已下载 / 大小」，如 `1.20 GB / 4.66 GB`。
    public static func sizeText(_ file: BtTaskFile) -> String {
        "\(Format.bytes(min(max(file.downloaded, 0), max(file.size, 0))).description) / \(Format.bytes(file.size).description)"
    }

    /// 种子内相对路径 → (文件名, 所在子目录)：`a/b/c.mkv` → (`c.mkv`, `a/b`)；顶层文件目录为空串。
    public static func split(_ path: String) -> (name: String, dir: String) {
        guard let slash = path.lastIndex(of: "/") else { return (path, "") }
        return (String(path[path.index(after: slash)...]), String(path[..<slash]))
    }
}

// MARK: - 任务限速 / 超级做种 / 强制重新校验

/// `daemon.task.setBtLimits`：字节/秒，0 = 不限；`nil` 不写键（daemon 侧保持原值）。
public struct SetBtLimitsParams: Codable, Sendable, Hashable {
    public var taskId: String
    public var downloadLimitBps: Int64?
    public var uploadLimitBps: Int64?

    public init(taskId: String, downloadLimitBps: Int64? = nil, uploadLimitBps: Int64? = nil) {
        self.taskId = taskId
        self.downloadLimitBps = downloadLimitBps
        self.uploadLimitBps = uploadLimitBps
    }
}

/// `daemon.task.setSuperSeeding`。
public struct SetSuperSeedingParams: Codable, Sendable, Hashable {
    public var taskId: String
    public var enabled: Bool

    public init(taskId: String, enabled: Bool) {
        self.taskId = taskId
        self.enabled = enabled
    }
}

/// 「任务限速…」对话框草稿：两个 KiB/s 输入框（0 / 空 = 不限），初值取任务的 `btDownloadLimitBps` / `seedUploadLimitBps`。
/// 只下发改动的字段；字节数不是 1024 整数倍的旧值按向下取整显示，未改动时不会被改写。
public struct BtTaskLimitsDraft: Sendable, Hashable {
    /// 输入上限（KiB/s）：1 TiB/s，远超任何链路，同时保证换算成字节不溢出。
    public static let maxKiB: Int64 = 1 << 30

    public var download: String
    public var upload: String
    private let initialDownloadKiB: Int64
    private let initialUploadKiB: Int64

    public init(task: DownloadTask) {
        self.init(downloadLimitBps: task.btDownloadLimitBps, uploadLimitBps: task.seedUploadLimitBps)
    }

    public init(downloadLimitBps: Int64, uploadLimitBps: Int64) {
        initialDownloadKiB = max(downloadLimitBps, 0) / 1024
        initialUploadKiB = max(uploadLimitBps, 0) / 1024
        download = String(initialDownloadKiB)
        upload = String(initialUploadKiB)
    }

    /// 文本 → KiB；空 = 0（不限）；非数字、负数、超上限 → nil。
    public static func kib(_ text: String) -> Int64? {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { return 0 }
        guard trimmed.allSatisfy({ $0.isASCII && $0.isNumber }), let value = Int64(trimmed), value <= maxKiB else { return nil }
        return value
    }

    public var isDownloadValid: Bool { Self.kib(download) != nil }
    public var isUploadValid: Bool { Self.kib(upload) != nil }
    public var isValid: Bool { isDownloadValid && isUploadValid }

    /// 有效且至少改了一个字段时的参数；否则 nil（确认按钮置灰）。
    public func params(taskId: String) -> SetBtLimitsParams? {
        guard let down = Self.kib(download), let up = Self.kib(upload) else { return nil }
        let params = SetBtLimitsParams(
            taskId: taskId,
            downloadLimitBps: down == initialDownloadKiB ? nil : down * 1024,
            uploadLimitBps: up == initialUploadKiB ? nil : up * 1024
        )
        return params.downloadLimitBps == nil && params.uploadLimitBps == nil ? nil : params
    }
}

/// 任务菜单的 BT 动作（多选取交集，沿用 ``PiecePickingMenu`` 的判定方式）：
/// - 强制重新校验：全部为未完成的 BT 任务；
/// - 超级做种（勾选项）：全部为 BT 任务；勾选 = 全部已开，点击把全部设为 `!allOn`（只发状态不同的任务）；
/// - 任务限速…：恰好一个 BT 任务。
/// 不看文件是否在这台手机上：正在控制的远端主机同样适用。
public struct BtTaskMenu: Sendable, Hashable {
    public var showsRecheck: Bool
    public var showsSuperSeeding: Bool
    /// 全部所选任务都开了超级做种。
    public var superSeedingOn: Bool
    /// 单选 BT 任务时为该任务（「任务限速…」）。
    public var limitsTask: DownloadTask?
    public var taskIds: [String]
    private var superSeedingTaskIds: Set<String>

    public var isEmpty: Bool { !showsRecheck && !showsSuperSeeding && limitsTask == nil }

    public static func facts(tasks: [DownloadTask]) -> BtTaskMenu {
        let allBt = !tasks.isEmpty && tasks.allSatisfy { isBt($0.url) }
        return BtTaskMenu(
            showsRecheck: allBt && tasks.allSatisfy { $0.status != .completed },
            showsSuperSeeding: allBt,
            superSeedingOn: allBt && tasks.allSatisfy(\.btSuperSeeding),
            limitsTask: allBt && tasks.count == 1 ? tasks[0] : nil,
            taskIds: allBt ? tasks.map(\.taskId) : [],
            superSeedingTaskIds: Set(tasks.filter(\.btSuperSeeding).map(\.taskId))
        )
    }

    /// 强制重新校验的目标（菜单不显示时为空）。
    public var recheckTaskIds: [String] { showsRecheck ? taskIds : [] }

    /// 把超级做种设为当前全开态的反面；已是目标值的任务跳过。
    public func togglingSuperSeeding() -> [SetSuperSeedingParams] {
        guard showsSuperSeeding else { return [] }
        let next = !superSeedingOn
        return taskIds.compactMap { id in
            superSeedingTaskIds.contains(id) == next ? nil : SetSuperSeedingParams(taskId: id, enabled: next)
        }
    }

    /// BT 任务：`magnet:` / `torrent-file://`（大小写不敏感，同 ``PiecePickingMenu``）。
    static func isBt(_ url: String) -> Bool {
        let lower = url.lowercased()
        return lower.hasPrefix("magnet:") || lower.hasPrefix("torrent-file://")
    }
}

// MARK: - 端口映射状态（`daemon.bt.portStatus`）

/// 一条端口映射（UPnP / NAT-PMP / PCP）。为 0 的端口与空串字段不下发。
public struct BtPortMapping: Decodable, Sendable, Hashable {
    /// `tcp` / `udp`。
    public var `protocol`: String
    public var internalPort: Int
    /// 路由器上的外部端口；未知为 0。
    public var externalPort: Int
    /// `upnp` / `natPmp` / `pcp`；尚未成功为空串。
    public var method: String
    /// 外网地址；未知为空串。
    public var externalIp: String
    /// `pending` / `mapped` / `failed`。
    public var state: String
    /// 失败原因；非失败为空串。
    public var message: String

    public init(
        protocol: String, internalPort: Int, externalPort: Int = 0, method: String = "", externalIp: String = "",
        state: String, message: String = ""
    ) {
        self.protocol = `protocol`
        self.internalPort = internalPort
        self.externalPort = externalPort
        self.method = method
        self.externalIp = externalIp
        self.state = state
        self.message = message
    }

    private enum CodingKeys: String, CodingKey {
        case `protocol`, internalPort, externalPort, method, externalIp, state, message
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        `protocol` = try c.decodeIfPresent(String.self, forKey: .protocol) ?? ""
        internalPort = try c.decodeIfPresent(Int.self, forKey: .internalPort) ?? 0
        externalPort = try c.decodeIfPresent(Int.self, forKey: .externalPort) ?? 0
        method = try c.decodeIfPresent(String.self, forKey: .method) ?? ""
        externalIp = try c.decodeIfPresent(String.self, forKey: .externalIp) ?? ""
        state = try c.decodeIfPresent(String.self, forKey: .state) ?? ""
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
    }
}

/// BT 监听端口与端口映射实况。`running == false`：没有 BT 任务，会话未创建，尚未请求映射。
public struct BtPortStatus: Decodable, Sendable, Hashable {
    public var running: Bool
    public var listenPort: Int
    public var mappings: [BtPortMapping]

    public init(running: Bool = false, listenPort: Int = 0, mappings: [BtPortMapping] = []) {
        self.running = running
        self.listenPort = listenPort
        self.mappings = mappings
    }

    private enum CodingKeys: String, CodingKey {
        case running, listenPort, mappings
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        running = try c.decodeIfPresent(Bool.self, forKey: .running) ?? false
        listenPort = try c.decodeIfPresent(Int.self, forKey: .listenPort) ?? 0
        mappings = try c.decodeIfPresent([BtPortMapping].self, forKey: .mappings) ?? []
    }
}

/// 端口映射状态行的一段（同 GPUI `sections/bt.rs::PortSegment`）。
public enum BtPortSegment: Sendable, Hashable {
    /// BT 会话未运行：尚未请求映射。
    case idle
    /// `port` = `TCP 6881` / `UDP 6881 → 50123`。
    case mapping(port: String, state: State)
    case externalIp(String)

    public enum State: Sendable, Hashable {
        /// 附显示用方法名（`UPnP` / `NAT-PMP` / `PCP`，未知取原值）。
        case mapped(method: String)
        case pending
        /// 失败原因（已去首尾空白；可能为空）。
        case failed(String)
        /// 未识别的 wire 状态，原样显示。
        case other(String)
    }

    /// 未运行一段「空闲」；否则每个映射一段，任一映射带外网地址时末尾追加一段（取第一个非空地址）。
    public static func segments(_ status: BtPortStatus) -> [BtPortSegment] {
        guard status.running else { return [.idle] }
        var segments: [BtPortSegment] = status.mappings.map { mapping in
            var port = "\(mapping.protocol.uppercased()) \(mapping.internalPort)"
            if mapping.state == "mapped", mapping.externalPort != 0, mapping.externalPort != mapping.internalPort {
                port += " → \(mapping.externalPort)"
            }
            let state: State = switch mapping.state {
            case "mapped": .mapped(method: methodLabel(mapping.method))
            case "pending": .pending
            case "failed": .failed(mapping.message.trimmingCharacters(in: .whitespacesAndNewlines))
            default: .other(mapping.state)
            }
            return .mapping(port: port, state: state)
        }
        let ip = status.mappings.lazy
            .map { $0.externalIp.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty }
        if let ip { segments.append(.externalIp(ip)) }
        return segments
    }

    /// wire 方法名 → 显示名；未知值原样返回。
    public static func methodLabel(_ method: String) -> String {
        switch method {
        case "upnp": "UPnP"
        case "natPmp": "NAT-PMP"
        case "pcp": "PCP"
        default: method
        }
    }

    /// 单行显示文字（失败原因压成一行）。
    public var text: String {
        switch self {
        case .idle:
            return L("btPortMappingIdle")
        case let .externalIp(ip):
            return L("btPortMappingExternalIp", ["ip": ip])
        case let .mapping(port, state):
            let status: String = switch state {
            case let .mapped(method): L("btPortMappingMapped", ["method": method])
            case .pending: L("btPortMappingPending")
            case let .failed(message) where message.isEmpty: L("btPortMappingFailed")
            case let .failed(message): "\(L("btPortMappingFailed")): \(Self.singleLine(message))"
            case let .other(raw): raw
            }
            return "\(port) \(status)"
        }
    }

    /// 长按可见的全文（只有带原因的失败段可能被截断）。
    public var fullText: String? {
        guard case let .mapping(port, .failed(message)) = self, !message.isEmpty else { return nil }
        return "\(port) \(L("btPortMappingFailed")): \(message)"
    }

    static func singleLine(_ text: String) -> String {
        text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }
}
