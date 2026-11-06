import Foundation

// 任务相关 wire DTO（镜像 `native/protocol/src/{daemon,task_activity}.rs` 与 `web/src/lib/rpc/protocol/{task,params}.ts`）+
// 详情页逻辑：做种限制（D4）三态映射、活动日志游标（`ActivityFeed` 的 Swift 移植）、`.torrent` 提交、插件失败判定。

// MARK: - 通用参数

/// `daemon.plugin.ignoreRetry` / `daemon.task.get`（服务端 `id` 别名 `taskId`）。
public struct TaskIdParams: Codable, Sendable, Hashable {
    public var taskId: String

    public init(taskId: String) { self.taskId = taskId }
}

// MARK: - daemon.task.create（完整 wire 形态）

/// `CreateTaskRequest` 的完整 wire 形态（`HostSession.createTask` 用的 ``CreateTaskRequest`` 只是移动端子集，
/// 且不带 `torrentB64`）。可选字段为 nil 时省略键（serde 按缺省值处理）。
public struct TaskCreateWire: Codable, Sendable, Hashable {
    public var url: String
    public var fileName: String
    public var saveDir: String
    /// 0 = 由引擎按文件大小决定。
    public var segments: Int32
    public var cookies: String
    public var referrer: String
    public var proxyUrl: String
    public var userAgent: String
    public var queueId: String
    public var checksum: String
    public var ignoreTlsErrors: Bool
    public var headers: [String: String]?
    /// `.torrent` 字节（标准 base64）；非空时按种子任务创建，`url` 可为空。
    public var torrentB64: String?
    public var startPaused: Bool
    public var httpUser: String
    public var httpPassword: String
    public var saveSiteAuth: Bool

    public init(
        url: String,
        fileName: String = "",
        saveDir: String = "",
        segments: Int32 = 0,
        cookies: String = "",
        referrer: String = "",
        proxyUrl: String = "",
        userAgent: String = "",
        queueId: String = "",
        checksum: String = "",
        ignoreTlsErrors: Bool = false,
        headers: [String: String]? = nil,
        torrentB64: String? = nil,
        startPaused: Bool = false,
        httpUser: String = "",
        httpPassword: String = "",
        saveSiteAuth: Bool = false
    ) {
        self.url = url
        self.fileName = fileName
        self.saveDir = saveDir
        self.segments = segments
        self.cookies = cookies
        self.referrer = referrer
        self.proxyUrl = proxyUrl
        self.userAgent = userAgent
        self.queueId = queueId
        self.checksum = checksum
        self.ignoreTlsErrors = ignoreTlsErrors
        self.headers = headers
        self.torrentB64 = torrentB64
        self.startPaused = startPaused
        self.httpUser = httpUser
        self.httpPassword = httpPassword
        self.saveSiteAuth = saveSiteAuth
    }

    /// 从移动端子集转换（保持同一套字段语义）。
    public init(_ request: CreateTaskRequest) {
        self.init(
            url: request.url,
            fileName: request.fileName,
            saveDir: request.saveDir,
            segments: request.segments,
            cookies: request.cookies,
            referrer: request.referrer,
            proxyUrl: request.proxyUrl,
            userAgent: request.userAgent,
            queueId: request.queueId,
            checksum: request.checksum,
            ignoreTlsErrors: request.ignoreTlsErrors,
            headers: request.headers.isEmpty ? nil : request.headers,
            startPaused: request.startPaused,
            httpUser: request.httpUser,
            httpPassword: request.httpPassword,
            saveSiteAuth: request.saveSiteAuth
        )
    }
}

/// `daemon.task.create` 参数（`DaemonCreateTaskParams`）。
public struct DaemonCreateTaskParams: Codable, Sendable, Hashable {
    public var request: TaskCreateWire
    public var torrentBlobId: String?
    public var unattended: Bool
    public var hintFileSize: Int64?
    /// 参照文件名（浏览器捕获的原始名）：显式名缺扩展名时借它的扩展名补全。
    public var nameHint: String?

    public init(request: TaskCreateWire, torrentBlobId: String? = nil, unattended: Bool = false, hintFileSize: Int64? = nil, nameHint: String? = nil) {
        self.request = request
        self.torrentBlobId = torrentBlobId
        self.unattended = unattended
        self.hintFileSize = hintFileSize
        self.nameHint = nameHint
    }
}

/// `daemon.task.create` 结果。
public struct CreatedTask: Codable, Sendable, Hashable {
    public var taskId: String

    public init(taskId: String) { self.taskId = taskId }
}

// MARK: - .torrent 文件提交（N3）

public enum TorrentFile {
    /// daemon blob / 种子上限（`REQUEST_BODY_LIMIT`、RSS `MAX_TORRENT_BYTES`）。
    public static let maxBytes = 4 * 1024 * 1024

    public enum Issue: Error, Sendable, Hashable {
        case empty
        case tooLarge
        /// 不是 bencode 字典 / 没有 `info` 键（登录页、误选文件）。
        case notTorrent
    }

    /// 粗校验：非空、≤ 4 MiB、bencode 字典且含 `4:info`（真实种子必有）。
    public static func validate(_ data: Data) -> Issue? {
        if data.isEmpty { return .empty }
        if data.count > maxBytes { return .tooLarge }
        guard data.first == UInt8(ascii: "d"), data.range(of: Data("4:info".utf8)) != nil else { return .notTorrent }
        return nil
    }

    /// `daemon.task.create`：本机与远端主机一律走 `request.torrentB64`（daemon 在 actor 内解码；WS 单帧上限 16 MiB
    /// 远大于 4 MiB 种子的 base64）。
    public static func createParams(
        data: Data,
        saveDir: String,
        queueId: String,
        startPaused: Bool
    ) -> DaemonCreateTaskParams {
        DaemonCreateTaskParams(request: TaskCreateWire(
            url: "",
            saveDir: saveDir,
            queueId: queueId,
            torrentB64: data.base64EncodedString(),
            startPaused: startPaused
        ))
    }

    public static func isTorrentFileName(_ name: String) -> Bool {
        name.lowercased().hasSuffix(".torrent")
    }
}

// MARK: - 插件失败

public enum PluginFailure {
    /// 可「忽略插件重试」的插件失败错误消息前缀（`download_manager.rs`、GPUI `PLUGIN_ERROR_PREFIX`；
    /// 二段解析任务失败用 `[解析插件]`，不匹配本前缀）。
    public static let prefix = "[插件]"

    /// 仅“失败 + 错误消息以插件前缀开头 + 非任务组成员”的任务提供「忽略插件重试」
    /// （组成员的原始链接是分享页，daemon 拒绝忽略重试）。
    public static func isIgnorable(status: TaskStatus, errorMessage: String, groupId: String) -> Bool {
        status == .failed && groupId.isEmpty && errorMessage.hasPrefix(prefix)
    }
}

/// 引擎产出的稳定任务错误消息；客户端按整串（trim 后）匹配并本地化展示，其余消息原样显示。
public enum TaskErrorMessage {
    /// 镜像 `native/engine/src/downloader.rs` 的 `POST_ONLY_ENDPOINT_MESSAGE`。
    public static let postOnlyEndpoint = "This link only accepts POST requests (HTTP 405); start the download from the browser"
    /// 镜像 `native/engine/src/downloader.rs` 的 `POST_ONLY_RESUME_MESSAGE`。
    public static let postOnlyResume = "This task was started by a browser POST request and cannot be resumed; start it again from the browser"

    /// 命中稳定消息时返回共享 i18n 键，否则 nil。
    public static func localizationKey(for message: String) -> String? {
        switch message.trimmingCharacters(in: .whitespacesAndNewlines) {
        case postOnlyEndpoint: "taskErrorPostOnlyEndpoint"
        case postOnlyResume: "taskErrorPostOnlyResume"
        default: nil
        }
    }

    /// 展示用文本：命中则本地化，否则原样返回。
    public static func displayText(_ message: String, localize: (String) -> String = { L($0) }) -> String {
        localizationKey(for: message).map(localize) ?? message
    }
}

// MARK: - 做种限制（D4）

public enum SeedLimit {
    /// 跟随全局。
    public static let inherit: Int64 = -2
    /// 不限制（0 引擎也视同不限制）。
    public static let unlimited: Int64 = -1
}

/// `daemon.task.setSeedLimits`：六个字段全部必填；比例为千分比。
public struct SetSeedLimitsParams: Codable, Sendable, Hashable {
    public var taskId: String
    public var ratioLimitMilli: Int64
    public var postRatioLimitMilli: Int64
    public var seedTimeLimitMinutes: Int64
    public var inactiveTimeLimitMinutes: Int64
    /// 字节 / 秒，0 = 跟随全局。
    public var uploadLimitBps: Int64

    public init(
        taskId: String,
        ratioLimitMilli: Int64,
        postRatioLimitMilli: Int64,
        seedTimeLimitMinutes: Int64,
        inactiveTimeLimitMinutes: Int64,
        uploadLimitBps: Int64
    ) {
        self.taskId = taskId
        self.ratioLimitMilli = ratioLimitMilli
        self.postRatioLimitMilli = postRatioLimitMilli
        self.seedTimeLimitMinutes = seedTimeLimitMinutes
        self.inactiveTimeLimitMinutes = inactiveTimeLimitMinutes
        self.uploadLimitBps = uploadLimitBps
    }
}

/// `daemon.task.setPiecePicking`：省略的字段保持不变。`nil` 不写键（serde `Option` = None）。
public struct SetTaskPiecePickingParams: Codable, Sendable, Hashable {
    public var taskId: String
    public var sequential: Bool?
    public var firstLastPiecesFirst: Bool?

    public init(taskId: String, sequential: Bool? = nil, firstLastPiecesFirst: Bool? = nil) {
        self.taskId = taskId
        self.sequential = sequential
        self.firstLastPiecesFirst = firstLastPiecesFirst
    }
}

/// 任务菜单「选片方式」：下载顺序单选 + 「先下载首尾文件块」独立勾选。
///
/// 仅当所选任务全部是未完成的 BT（`magnet:` / `torrent-file://`，大小写不敏感）时显示。
/// 不看文件是否在这台手机上：正在控制的远端主机同样走 `daemon.task.setPiecePicking`。
/// 顺序：全体 `btSequential == false` 为最稀有优先（缺省），全体为 true 为按顺序下载；
/// 混选或菜单不显示时两项都不点亮。点选只对状态不同的任务发 `sequential`。
/// 「先下载首尾文件块」勾选 = 全部已开；点击把该项设为 `!allOn`，且只发送该字段。
public struct PiecePickingMenu: Sendable, Hashable {
    /// 下载顺序。全体一致才点亮对应单选；混合或不可见为 ``mixed``。
    public enum Order: Sendable, Hashable {
        /// 全体 `btSequential == false`（缺省：最稀有优先）。
        case rarest
        /// 全体 `btSequential == true`。
        case sequential
        /// 有的开、有的关，或菜单不显示（两个单选都不选）。
        case mixed
    }

    public var shown: Bool
    /// 全部所选任务都开了「先下载首尾文件块」。
    public var firstLastOn: Bool
    public var taskIds: [String]
    /// 当前 `btSequential == true` 的任务。与 `taskIds` 一起决定顺序，并跳过已是目标值的任务。
    private var sequentialTaskIds: Set<String>

    /// 全体一致才点亮；空选区与不可见选区为 ``Order/mixed``。
    public var order: Order {
        guard shown, !taskIds.isEmpty else { return .mixed }
        if taskIds.allSatisfy({ sequentialTaskIds.contains($0) }) { return .sequential }
        if taskIds.allSatisfy({ !sequentialTaskIds.contains($0) }) { return .rarest }
        return .mixed
    }

    public init(shown: Bool, firstLastOn: Bool, taskIds: [String], sequentialTaskIds: Set<String> = []) {
        self.shown = shown
        self.firstLastOn = firstLastOn
        self.taskIds = taskIds
        self.sequentialTaskIds = sequentialTaskIds
    }

    public static func facts(tasks: [DownloadTask]) -> PiecePickingMenu {
        let eligible = !tasks.isEmpty && tasks.allSatisfy(Self.isEligible)
        guard eligible else {
            return PiecePickingMenu(shown: false, firstLastOn: false, taskIds: [])
        }
        return PiecePickingMenu(
            shown: true,
            firstLastOn: tasks.allSatisfy(\.btFirstLastPiecesFirst),
            taskIds: tasks.map(\.taskId),
            sequentialTaskIds: Set(tasks.filter(\.btSequential).map(\.taskId))
        )
    }

    /// 把下载顺序设为 `sequential`（`false` = 最稀有优先）。只带 `sequential`；
    /// 已是该状态的任务跳过。菜单不显示或全体已是目标值时为空（不下发）。
    public func settingSequential(_ sequential: Bool) -> [SetTaskPiecePickingParams] {
        guard shown else { return [] }
        return taskIds.compactMap { id in
            guard sequentialTaskIds.contains(id) != sequential else { return nil }
            return SetTaskPiecePickingParams(taskId: id, sequential: sequential)
        }
    }

    /// 把「先下载首尾文件块」设为当前全开态的反面；只带 `firstLastPiecesFirst`。
    public func togglingFirstLast() -> [SetTaskPiecePickingParams] {
        guard shown else { return [] }
        let next = !firstLastOn
        return taskIds.map { SetTaskPiecePickingParams(taskId: $0, firstLastPiecesFirst: next) }
    }

    private static func isEligible(_ task: DownloadTask) -> Bool {
        guard task.status != .completed else { return false }
        let url = task.url.lowercased()
        return url.hasPrefix("magnet:") || url.hasPrefix("torrent-file://")
    }
}

/// `daemon.task.get` 结果（`TaskDto`）里与做种限制相关的字段；缺字段时按旧主机默认（跟随全局）。
public struct TaskSeedLimits: Decodable, Sendable, Hashable {
    public var seedRatioLimitMilli: Int64
    public var seedPostRatioLimitMilli: Int64
    public var seedTimeLimitMinutes: Int64
    public var seedInactiveTimeLimitMinutes: Int64
    public var seedUploadLimitBps: Int64

    public init(
        seedRatioLimitMilli: Int64 = SeedLimit.inherit,
        seedPostRatioLimitMilli: Int64 = SeedLimit.inherit,
        seedTimeLimitMinutes: Int64 = SeedLimit.inherit,
        seedInactiveTimeLimitMinutes: Int64 = SeedLimit.inherit,
        seedUploadLimitBps: Int64 = 0
    ) {
        self.seedRatioLimitMilli = seedRatioLimitMilli
        self.seedPostRatioLimitMilli = seedPostRatioLimitMilli
        self.seedTimeLimitMinutes = seedTimeLimitMinutes
        self.seedInactiveTimeLimitMinutes = seedInactiveTimeLimitMinutes
        self.seedUploadLimitBps = seedUploadLimitBps
    }

    private enum CodingKeys: String, CodingKey {
        case seedRatioLimitMilli, seedPostRatioLimitMilli, seedTimeLimitMinutes
        case seedInactiveTimeLimitMinutes, seedUploadLimitBps
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        seedRatioLimitMilli = try c.decodeIfPresent(Int64.self, forKey: .seedRatioLimitMilli) ?? SeedLimit.inherit
        seedPostRatioLimitMilli = try c.decodeIfPresent(Int64.self, forKey: .seedPostRatioLimitMilli) ?? SeedLimit.inherit
        seedTimeLimitMinutes = try c.decodeIfPresent(Int64.self, forKey: .seedTimeLimitMinutes) ?? SeedLimit.inherit
        seedInactiveTimeLimitMinutes = try c.decodeIfPresent(Int64.self, forKey: .seedInactiveTimeLimitMinutes) ?? SeedLimit.inherit
        seedUploadLimitBps = try c.decodeIfPresent(Int64.self, forKey: .seedUploadLimitBps) ?? 0
    }
}

public enum SeedLimitMode: Sendable, Hashable, CaseIterable {
    case inherit, unlimited, custom
}

/// 一个三态做种限制字段（总分享率 / 做种后分享率 / 总做种分钟 / 不活跃分钟）。
///
/// 未改动 = 保持原线上值（不因取整 / 0↔-1 归一而改写，对齐 Web `seedUpload.ts` 的“未改动保持原值”）。
public struct SeedLimitField: Sendable, Hashable {
    public enum Unit: Sendable, Hashable {
        /// 输入为十进制比例（如 `1.50`），线上千分比。
        case ratio
        /// 输入为整数分钟。
        case minutes
    }

    public let unit: Unit
    public var mode: SeedLimitMode
    /// 自定义值的输入文本（`mode == .custom` 时有效）。
    public var text: String

    private let originalWire: Int64
    private let originalMode: SeedLimitMode
    private let originalText: String

    public init(wire: Int64, unit: Unit) {
        self.unit = unit
        originalWire = wire
        let mode: SeedLimitMode
        let text: String
        if wire == SeedLimit.inherit {
            (mode, text) = (.inherit, "")
        } else if wire <= 0 {
            // 负数（-1）与 0 都是“不限制”。
            (mode, text) = (.unlimited, "")
        } else {
            (mode, text) = (.custom, Self.display(wire: wire, unit: unit))
        }
        self.mode = mode
        self.text = text
        originalMode = mode
        originalText = text
    }

    static func display(wire: Int64, unit: Unit) -> String {
        switch unit {
        case .ratio: String(format: "%.2f", locale: Locale(identifier: "en_US_POSIX"), Double(wire) / 1000)
        case .minutes: String(wire)
        }
    }

    /// 自定义文本解析：比例接受 `.` / `,` 小数点；分钟只接受非负整数。
    static func parseCustom(_ text: String, unit: Unit) -> Int64? {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return nil }
        switch unit {
        case .minutes:
            guard trimmed.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
            return Int64(trimmed)
        case .ratio:
            let normalized = trimmed.replacingOccurrences(of: ",", with: ".")
            guard normalized.allSatisfy({ $0.isASCII && ($0.isNumber || $0 == ".") }),
                  normalized.filter({ $0 == "." }).count <= 1,
                  let value = Double(normalized), value.isFinite, value >= 0,
                  value * 1000 < Double(Int64.max / 2) else { return nil }
            return Int64((value * 1000).rounded())
        }
    }

    /// 自定义模式下输入无法解析 → 无效（不可保存）。
    public var isValid: Bool {
        mode != .custom || Self.parseCustom(text, unit: unit) != nil
    }

    /// 要发送的线上值；无效为 nil。
    public var wireValue: Int64? {
        if mode == originalMode, mode != .custom || text == originalText { return originalWire }
        switch mode {
        case .inherit: return SeedLimit.inherit
        case .unlimited: return SeedLimit.unlimited
        case .custom: return Self.parseCustom(text, unit: unit)
        }
    }

    public var isModified: Bool { mode != originalMode || (mode == .custom && text != originalText) }
}

/// 上传限速：KB/s 输入，线上字节 / 秒；留空 / 0 = 跟随全局。未改动保持原 bps（不因取整丢精度）。
public struct SeedUploadLimitField: Sendable, Hashable {
    /// 自定义（`true`）= 按 `text` KB/s；`false` = 跟随全局。
    public var custom: Bool
    public var text: String

    private let originalBps: Int64
    private let originalCustom: Bool
    private let originalText: String

    public init(bps: Int64) {
        originalBps = max(bps, 0)
        let custom = bps > 0
        self.custom = custom
        text = custom ? String((bps + 512) / 1024) : ""
        originalCustom = custom
        originalText = text
    }

    static func parseKbps(_ text: String) -> Int64? {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, trimmed.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        return Int64(trimmed)
    }

    public var isValid: Bool {
        !custom || Self.parseKbps(text) != nil
    }

    /// 字节 / 秒；无效为 nil。自定义且为 0 = 跟随全局（0）。
    public var wireBps: Int64? {
        if custom == originalCustom, !custom || text == originalText { return originalBps }
        guard custom else { return 0 }
        guard let kbps = Self.parseKbps(text) else { return nil }
        return kbps > Int64.max / 1024 ? nil : kbps * 1024
    }

    public var isModified: Bool { custom != originalCustom || (custom && text != originalText) }
}

/// D4 草稿：五个字段 + 保存参数。
public struct SeedLimitsDraft: Sendable, Hashable {
    public var ratio: SeedLimitField
    public var postRatio: SeedLimitField
    public var time: SeedLimitField
    public var inactive: SeedLimitField
    public var upload: SeedUploadLimitField

    public init(limits: TaskSeedLimits) {
        ratio = SeedLimitField(wire: limits.seedRatioLimitMilli, unit: .ratio)
        postRatio = SeedLimitField(wire: limits.seedPostRatioLimitMilli, unit: .ratio)
        time = SeedLimitField(wire: limits.seedTimeLimitMinutes, unit: .minutes)
        inactive = SeedLimitField(wire: limits.seedInactiveTimeLimitMinutes, unit: .minutes)
        upload = SeedUploadLimitField(bps: limits.seedUploadLimitBps)
    }

    public var isValid: Bool {
        ratio.isValid && postRatio.isValid && time.isValid && inactive.isValid && upload.isValid
    }

    public var isModified: Bool {
        ratio.isModified || postRatio.isModified || time.isModified || inactive.isModified || upload.isModified
    }

    public func params(taskId: String) -> SetSeedLimitsParams? {
        guard let ratio = ratio.wireValue, let postRatio = postRatio.wireValue,
              let time = time.wireValue, let inactive = inactive.wireValue,
              let upload = upload.wireBps else { return nil }
        return SetSeedLimitsParams(
            taskId: taskId,
            ratioLimitMilli: ratio,
            postRatioLimitMilli: postRatio,
            seedTimeLimitMinutes: time,
            inactiveTimeLimitMinutes: inactive,
            uploadLimitBps: upload
        )
    }
}

/// 全局做种限制（`daemon.config` 键）：D4 各字段页脚“全局：…”。0 = 不限制。
public struct SeedLimitGlobals: Sendable, Hashable {
    /// 总分享率；nil = 不限制。
    public var ratio: Double?
    public var postRatio: Double?
    /// 分钟；nil = 不限制。
    public var timeMinutes: Int64?
    public var inactiveMinutes: Int64?

    public init(config: [String: String]) {
        func ratio(_ key: String) -> Double? {
            guard let text = config[key], let value = Double(text), value > 0 else { return nil }
            return value
        }
        func minutes(_ key: String) -> Int64? {
            guard let text = config[key], let value = Int64(text), value > 0 else { return nil }
            return value
        }
        self.ratio = ratio("bt_seed_ratio_limit")
        postRatio = ratio("bt_seed_post_ratio_limit")
        timeMinutes = minutes("bt_seed_time_limit_minutes")
        inactiveMinutes = minutes("bt_seed_inactive_time_limit_minutes")
    }
}

// MARK: - 活动日志

/// 持久任务事件（`TaskActivityDto`）；ID 在同一个下载数据库内单调递增。
public struct TaskActivityDto: Codable, Sendable, Hashable, Identifiable {
    public var id: Int64
    public var taskId: String
    public var timestampMs: Int64
    public var kind: String
    public var message: String
    public var status: Int32?

    public init(id: Int64, taskId: String, timestampMs: Int64, kind: String, message: String = "", status: Int32? = nil) {
        self.id = id
        self.taskId = taskId
        self.timestampMs = timestampMs
        self.kind = kind
        self.message = message
        self.status = status
    }

    private enum CodingKeys: String, CodingKey { case id, taskId, timestampMs, kind, message, status }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int64.self, forKey: .id)
        taskId = try c.decode(String.self, forKey: .taskId)
        timestampMs = try c.decodeIfPresent(Int64.self, forKey: .timestampMs) ?? 0
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? ""
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
        status = try c.decodeIfPresent(Int32.self, forKey: .status)
    }
}

/// `daemon.task.activity` 参数：`beforeId` 向前翻页、`afterId` 断线补齐（互斥）；`limit` 0 = 服务端默认。
public struct TaskActivityQuery: Codable, Sendable, Hashable {
    public var taskId: String
    public var beforeId: Int64?
    public var afterId: Int64?
    public var limit: UInt32

    public init(taskId: String, beforeId: Int64? = nil, afterId: Int64? = nil, limit: UInt32 = 0) {
        self.taskId = taskId
        self.beforeId = beforeId
        self.afterId = afterId
        self.limit = limit
    }
}

/// 页内按 ID 升序；`truncated` 标明历史被保留策略清理。
public struct TaskActivityPage: Codable, Sendable, Hashable {
    public var entries: [TaskActivityDto]
    public var hasMore: Bool
    public var oldestId: Int64?
    public var newestId: Int64?
    public var truncated: Bool

    public init(entries: [TaskActivityDto], hasMore: Bool, oldestId: Int64? = nil, newestId: Int64? = nil, truncated: Bool = false) {
        self.entries = entries
        self.hasMore = hasMore
        self.oldestId = oldestId
        self.newestId = newestId
        self.truncated = truncated
    }

    private enum CodingKeys: String, CodingKey { case entries, hasMore, oldestId, newestId, truncated }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        entries = try c.decodeIfPresent([TaskActivityDto].self, forKey: .entries) ?? []
        hasMore = try c.decodeIfPresent(Bool.self, forKey: .hasMore) ?? false
        oldestId = try c.decodeIfPresent(Int64.self, forKey: .oldestId)
        newestId = try c.decodeIfPresent(Int64.self, forKey: .newestId)
        truncated = try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }
}

/// 活动日志的持久游标（`web/src/pages/downloads/detail/activityFeed.ts` 的移植，原件源自
/// `crates/downloads/src/pages/task_detail_activity.rs`）：历史页与实时 `taskActivityAdded` 按源端 ID 合并。
///
/// 使用方式：`begin()` 取票据 → 发 RPC → `finish(ticket, page:)`（失败传 nil）→ 视 `begin()` 是否还有后续；
/// 离线 `suspend()`、重连 `reconnect()`、翻页 `loadOlder()`、失败 `retry()`。
public struct TaskActivityFeed: Sendable {
    public enum Fetch: Sendable, Hashable { case latest, after, before }

    public struct Ticket: Sendable, Hashable {
        public let generation: Int
        public let serial: Int
        public let fetch: Fetch
        public let query: TaskActivityQuery
    }

    public let taskId: String

    private var generation = 0
    private var serial = 0
    private var map: [Int64: TaskActivityDto] = [:]
    private var cursor: Int64 = 0
    private var isLoaded = false
    private var older = false
    private var oldest: Int64?
    private var newest: Int64?
    private var isTruncated = false
    private var gap = false
    private var pending: Fetch? = .latest
    private var inFlight: Ticket?
    private var failedFetch: Fetch?
    /// 事件可能先于页 RPC 的结果到达；页的游标只能由查询结果推进。
    private var liveDuringFetch: Int64 = 0

    public init(taskId: String) { self.taskId = taskId }

    private mutating func reset() {
        serial = 0
        map = [:]
        cursor = 0
        isLoaded = false
        older = false
        oldest = nil
        newest = nil
        isTruncated = false
        gap = false
        pending = .latest
        inFlight = nil
        failedFetch = nil
        liveDuringFetch = 0
    }

    /// 升序（id 小 → 大）。
    public var entries: [TaskActivityDto] { map.values.sorted { $0.id < $1.id } }
    public var isLoading: Bool { inFlight != nil }
    public var loaded: Bool { isLoaded }
    public var failed: Bool { failedFetch != nil }
    public var hasOlder: Bool { isLoaded && older }
    public var hasJournalGap: Bool { gap }
    public var retainedRange: (oldest: Int64?, newest: Int64?, truncated: Bool) { (oldest, newest, isTruncated) }

    private mutating func switchTask() {
        generation += 1
        reset()
    }

    public mutating func suspend() {
        generation += 1
        inFlight = nil
        liveDuringFetch = 0
    }

    public mutating func reconnect() {
        suspend()
        pending = isLoaded ? .after : .latest
        failedFetch = nil
    }

    public mutating func loadOlder() {
        if older, inFlight == nil, pending == nil {
            pending = .before
            failedFetch = nil
        }
    }

    public mutating func retry() {
        if let failedFetch {
            pending = failedFetch
            self.failedFetch = nil
        }
    }

    /// 实时事件；返回是否有新增条目。
    @discardableResult
    public mutating func add(_ entry: TaskActivityDto) -> Bool {
        guard entry.taskId == taskId, entry.id > 0 else { return false }
        if entry.kind == "journal_overflow" { gap = true }
        let changed = map[entry.id] == nil
        map[entry.id] = entry
        if let inFlight, inFlight.fetch != .before {
            liveDuringFetch = max(liveDuringFetch, entry.id)
        }
        return changed
    }

    public mutating func begin() -> Ticket? {
        if inFlight != nil || failedFetch != nil { return nil }
        guard let fetch = pending else { return nil }
        pending = nil
        var beforeId: Int64?
        if fetch == .before {
            guard let first = entries.first else {
                older = false
                return nil
            }
            beforeId = first.id
        }
        serial += 1
        let ticket = Ticket(
            generation: generation,
            serial: serial,
            fetch: fetch,
            query: TaskActivityQuery(taskId: taskId, beforeId: beforeId, afterId: fetch == .after ? cursor : nil, limit: 0)
        )
        liveDuringFetch = 0
        inFlight = ticket
        return ticket
    }

    /// 只接受本任务本世代的结果；查询期间到达的实时事件永不被页覆盖。
    @discardableResult
    public mutating func finish(_ ticket: Ticket, page: TaskActivityPage?) -> Bool {
        guard let current = inFlight, current.generation == ticket.generation, current.serial == ticket.serial,
              current.query.taskId == ticket.query.taskId else { return false }
        inFlight = nil
        guard let page else {
            failedFetch = ticket.fetch
            return true
        }
        failedFetch = nil
        if ticket.fetch == .after, cursor > 0, page.newestId == nil || (page.newestId ?? 0) < cursor {
            // 数据库被重建或该任务的保留记录全部过期；旧游标不属于新历史。
            switchTask()
            return true
        }
        oldest = page.oldestId
        newest = page.newestId
        isTruncated = isTruncated || page.truncated
        if let oldest {
            if let first = entries.first, first.id < oldest { isTruncated = true }
            for id in map.keys where id < oldest { map[id] = nil }
        }
        let lastPageId = page.entries.last?.id ?? 0
        for entry in page.entries where entry.taskId == taskId && entry.id > 0 {
            if entry.kind == "journal_overflow" { gap = true }
            map[entry.id] = entry
        }
        switch ticket.fetch {
        case .latest:
            isLoaded = true
            older = page.hasMore
            cursor = max(cursor, lastPageId)
            if liveDuringFetch > cursor { pending = .after }
        case .after:
            // 不能以实时事件的最大 ID 推进游标：中间尚未取回的记录会被跳过。
            cursor = max(cursor, lastPageId)
            if page.hasMore, lastPageId <= (ticket.query.afterId ?? 0) {
                failedFetch = .after
            } else if page.hasMore || liveDuringFetch > cursor {
                pending = .after
            }
        case .before:
            older = page.hasMore
        }
        liveDuringFetch = 0
        return true
    }
}
