import Foundation

// 主机投影的 Swift 侧模型：字段与 `native/protocol` 的 wire DTO 一一对应（camelCase 同名），
// 由 FluxBridge 在边界一次性转换（u64→Int64、i32 状态码→枚举、Unix 秒字符串→Int64）。
// 与 Android `:core` 的 `Models.kt` 逐字段对应；UI 只读这些不可变值类型。
// 命名避开 Swift 并发库的 `Task` / `TaskGroup`：任务 = `DownloadTask`，任务组 = `DownloadGroup`。

/// `TaskDto.status`（0–5）。未知值映射到 `.unknown`，一律不可控制。
public enum TaskStatus: Int32, Sendable, Hashable, CaseIterable {
    case pending = 0
    case downloading = 1
    case paused = 2
    case completed = 3
    case failed = 4
    case preparing = 5
    case unknown = -1

    public init(wire: Int32) { self = TaskStatus(rawValue: wire) ?? .unknown }

    /// 活跃 = 下载中或准备中（与 `event.rs` 的 `matches!(status, 1 | 5)` 一致）。
    public var isActive: Bool { self == .downloading || self == .preparing }
    /// 运行时（分段图 / 传输读数）仍有可见用途：完成任务不保留（与 `event.rs` 的 `runtime_retained` 一致）。
    public var retainsRuntime: Bool { self != .completed }
}

/// `TaskDto.seedingStatus`（0–8）。
public enum SeedingStatus: Int32, Sendable, Hashable {
    case none = 0
    case seeding = 1
    case ratioReached = 2
    case timeReached = 3
    case userStopped = 4
    case taskDeleted = 5
    case sessionReleased = 6
    case inactiveStop = 7
    case queuedForSlot = 8
    case unknown = -1

    public init(wire: Int32) { self = SeedingStatus(rawValue: wire) ?? .unknown }
}

/// 协议种类：由 URL / 哨兵推导，仅用于图标与徽标。
public enum TaskProtocol: String, Sendable, Hashable, CaseIterable {
    case http, ftp, bt, ed2k, hls, plugin
}

/// `TaskSourceBytesDto`：加速路径累计字节；源站 = 已下载 − 三者之和。
public struct SourceBytes: Sendable, Hashable {
    public var cdn: Int64
    public var proxy: Int64
    public var nic: Int64

    public init(cdn: Int64 = 0, proxy: Int64 = 0, nic: Int64 = 0) {
        self.cdn = cdn
        self.proxy = proxy
        self.nic = nic
    }

    public var total: Int64 { cdn + proxy + nic }
}

/// `TaskDto`（不含速度 / 分段：速度只在事件里，分段在 `TaskRuntime`）。
public struct DownloadTask: Sendable, Hashable, Identifiable {
    public var taskId: String
    public var url: String
    public var originUrl: String
    public var fileName: String
    public var saveDir: String
    public var status: TaskStatus
    public var downloadedBytes: Int64
    /// 0 = 未知大小。
    public var totalBytes: Int64
    public var errorMessage: String
    /// Unix 秒；0 = 无。
    public var createdAt: Int64
    public var completedAt: Int64
    /// "" = 主队列。
    public var queueId: String
    public var groupId: String
    public var rssSourceId: String
    public var fileMissing: Bool
    public var autoRoute: String
    public var sourceBytes: SourceBytes
    public var uploadedBytes: Int64
    public var seedingStatus: SeedingStatus
    public var seedingMessage: String
    public var seedingTimeSecs: Int64
    /// BT 选片：按顺序下载。缺省 / 非 BT = false。
    public var btSequential: Bool
    /// BT 选片：先下载首尾文件块。缺省 / 非 BT = false。
    public var btFirstLastPiecesFirst: Bool

    public init(
        taskId: String,
        url: String,
        originUrl: String = "",
        fileName: String,
        saveDir: String = "",
        status: TaskStatus,
        downloadedBytes: Int64 = 0,
        totalBytes: Int64 = 0,
        errorMessage: String = "",
        createdAt: Int64 = 0,
        completedAt: Int64 = 0,
        queueId: String = "",
        groupId: String = "",
        rssSourceId: String = "",
        fileMissing: Bool = false,
        autoRoute: String = "",
        sourceBytes: SourceBytes = SourceBytes(),
        uploadedBytes: Int64 = 0,
        seedingStatus: SeedingStatus = .none,
        seedingMessage: String = "",
        seedingTimeSecs: Int64 = 0,
        btSequential: Bool = false,
        btFirstLastPiecesFirst: Bool = false
    ) {
        self.taskId = taskId
        self.url = url
        self.originUrl = originUrl
        self.fileName = fileName
        self.saveDir = saveDir
        self.status = status
        self.downloadedBytes = downloadedBytes
        self.totalBytes = totalBytes
        self.errorMessage = errorMessage
        self.createdAt = createdAt
        self.completedAt = completedAt
        self.queueId = queueId
        self.groupId = groupId
        self.rssSourceId = rssSourceId
        self.fileMissing = fileMissing
        self.autoRoute = autoRoute
        self.sourceBytes = sourceBytes
        self.uploadedBytes = uploadedBytes
        self.seedingStatus = seedingStatus
        self.seedingMessage = seedingMessage
        self.seedingTimeSecs = seedingTimeSecs
        self.btSequential = btSequential
        self.btFirstLastPiecesFirst = btFirstLastPiecesFirst
    }

    public var id: String { taskId }

    /// 复制链接：读 originUrl，空则回退 url（torrent 任务的 url 是哨兵）。
    public var shareUrl: String { originUrl.isEmpty ? url : originUrl }

    public var `protocol`: TaskProtocol {
        if url.hasPrefix("magnet:") || url.hasPrefix("torrent-file://") { return .bt }
        if url.hasPrefix("ed2k://") { return .ed2k }
        if url.hasPrefix("ftp://") || url.hasPrefix("ftps://") || url.hasPrefix("sftp://") { return .ftp }
        let path = url.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first ?? ""
        if path.hasSuffix(".m3u8") { return .hls }
        return .http
    }

    /// 0...1；总大小未知时为 nil（不确定进度）。
    public var progress: Double? {
        guard totalBytes > 0 else { return nil }
        return min(max(Double(downloadedBytes) / Double(totalBytes), 0), 1)
    }

    /// 本机 BT 任务正在做种（含排队等做种槽位）：已完成且 seedingStatus ∈ {1, 8}。可暂停（停止做种）。
    public var isSeeding: Bool {
        status == .completed && (seedingStatus == .seeding || seedingStatus == .queuedForSlot)
    }

    /// 已完成且做种已停止（2...7）、文件仍在 → 可恢复做种。其他已完成任务不可恢复（#935）。
    public var canResumeSeeding: Bool {
        guard status == .completed, !fileMissing else { return false }
        switch seedingStatus {
        case .ratioReached, .timeReached, .userStopped, .taskDeleted, .sessionReleased, .inactiveStop: return true
        case .none, .seeding, .queuedForSlot, .unknown: return false
        }
    }

    /// 「全部继续」可恢复的做种任务：用户手动停止（4）且文件仍在。
    public var isUserStoppedSeed: Bool {
        status == .completed && seedingStatus == .userStopped && !fileMissing
    }
}

extension Sequence<DownloadTask> {
    /// 存在可暂停的做种任务（「全部暂停」可用；纯做种主机不算「全部已暂停」）。
    public var hasSeeding: Bool { contains { $0.isSeeding } }

    /// 活动条「全部暂停 ↔ 全部恢复」切换：没有运行 / 排队 / 做种，且存在可恢复任务（暂停 / 失败 / 用户停止的做种）。
    public func isAllPaused(active: Int, pending: Int) -> Bool {
        guard active == 0, pending == 0, !hasSeeding else { return false }
        return contains { $0.status == .paused || $0.status == .failed || $0.isUserStoppedSeed }
    }
}

/// `TaskSegmentDto`：真实字节区间；`active == nil` 表示未知，不得推断为空闲。
public struct Segment: Sendable, Hashable {
    public var index: Int32
    public var startByte: Int64
    public var endByte: Int64
    public var downloadedBytes: Int64
    public var active: Bool?

    public init(index: Int32, startByte: Int64, endByte: Int64, downloadedBytes: Int64, active: Bool?) {
        self.index = index
        self.startByte = startByte
        self.endByte = endByte
        self.downloadedBytes = downloadedBytes
        self.active = active
    }
}

/// `TaskRuntimeDto`。
public struct TaskRuntime: Sendable, Hashable {
    public var taskId: String
    public var sampleSequence: UInt64
    public var activeTransfers: UInt32?
    public var connectedPeers: UInt32?
    public var totalBytes: Int64
    public var segments: [Segment]

    public init(
        taskId: String,
        sampleSequence: UInt64,
        activeTransfers: UInt32?,
        connectedPeers: UInt32?,
        totalBytes: Int64,
        segments: [Segment]
    ) {
        self.taskId = taskId
        self.sampleSequence = sampleSequence
        self.activeTransfers = activeTransfers
        self.connectedPeers = connectedPeers
        self.totalBytes = totalBytes
        self.segments = segments
    }
}

/// `QueueDto`。
public struct TaskQueue: Sendable, Hashable, Identifiable {
    public static let main = "main"
    public static let later = "later"

    public var queueId: String
    public var name: String
    public var speedLimitKbps: Int64
    public var uploadLimitKbps: Int64
    public var maxConcurrent: Int32
    public var defaultSaveDir: String
    public var position: Int32
    public var isRunning: Bool
    public var scheduleEnabled: Bool
    public var scheduleStart: String
    public var scheduleStop: String
    /// bit0 = 周一 … bit6 = 周日；127 = 每天。
    public var scheduleDays: Int32

    /// 主队列始终运行：运行中不能停止（停止态异常时仍可启动）。
    public var canStop: Bool { QueueIds.canStop(queueId) }
    /// 主队列不可定时。
    public var canSchedule: Bool { QueueIds.canSchedule(queueId) }

    public init(
        queueId: String,
        name: String,
        speedLimitKbps: Int64 = 0,
        uploadLimitKbps: Int64 = 0,
        maxConcurrent: Int32 = 0,
        defaultSaveDir: String = "",
        position: Int32 = 0,
        isRunning: Bool = true,
        scheduleEnabled: Bool = false,
        scheduleStart: String = "",
        scheduleStop: String = "",
        scheduleDays: Int32 = 127
    ) {
        self.queueId = queueId
        self.name = name
        self.speedLimitKbps = speedLimitKbps
        self.uploadLimitKbps = uploadLimitKbps
        self.maxConcurrent = maxConcurrent
        self.defaultSaveDir = defaultSaveDir
        self.position = position
        self.isRunning = isRunning
        self.scheduleEnabled = scheduleEnabled
        self.scheduleStart = scheduleStart
        self.scheduleStop = scheduleStop
        self.scheduleDays = scheduleDays
    }

    public var id: String { queueId }
}

/// `GroupDto`；组进度由客户端汇总成员任务。
public struct DownloadGroup: Sendable, Hashable, Identifiable {
    public var groupId: String
    public var name: String
    public var sourceUrl: String
    public var saveDir: String
    public var createdAt: Int64

    public init(groupId: String, name: String, sourceUrl: String, saveDir: String, createdAt: Int64) {
        self.groupId = groupId
        self.name = name
        self.sourceUrl = sourceUrl
        self.saveDir = saveDir
        self.createdAt = createdAt
    }

    public var id: String { groupId }
}

/// `DaemonRuntimeStatsDto`。
public struct RuntimeStats: Sendable, Hashable {
    public var activeTasks: UInt32
    public var pendingTasks: UInt32
    public var totalDownloadBps: Int64
    public var totalUploadBps: Int64
    /// nil = 探测失败。
    public var diskFreeBytes: UInt64?
    public var saveDir: String
    public var retryPendingTasks: UInt32

    public init(
        activeTasks: UInt32 = 0,
        pendingTasks: UInt32 = 0,
        totalDownloadBps: Int64 = 0,
        totalUploadBps: Int64 = 0,
        diskFreeBytes: UInt64? = nil,
        saveDir: String = "",
        retryPendingTasks: UInt32 = 0
    ) {
        self.activeTasks = activeTasks
        self.pendingTasks = pendingTasks
        self.totalDownloadBps = totalDownloadBps
        self.totalUploadBps = totalUploadBps
        self.diskFreeBytes = diskFreeBytes
        self.saveDir = saveDir
        self.retryPendingTasks = retryPendingTasks
    }
}

/// 引擎发起的选择请求（X1–X3），到期按 `defaultChoice` 处理；`noDeadline` = 不自动处理。
public struct SelectionRequest: Sendable, Hashable, Identifiable {
    public var requestId: String
    public var taskId: String
    public var kind: SelectionKind
    public var defaultChoice: SelectionOutcome
    public var deadlineUnixMs: Int64

    public init(
        requestId: String,
        taskId: String,
        kind: SelectionKind,
        defaultChoice: SelectionOutcome,
        deadlineUnixMs: Int64
    ) {
        self.requestId = requestId
        self.taskId = taskId
        self.kind = kind
        self.defaultChoice = defaultChoice
        self.deadlineUnixMs = deadlineUnixMs
    }

    public var id: String { requestId }

    /// 「无截止」（protocol `SELECTION_NO_DEADLINE` = i64::MAX）：用户关闭了 BT 选择的自动选择默认项。
    public static let noDeadline: Int64 = .max

    /// 有自动按默认项处理的截止时刻（否则不显示倒计时、不自动答复）。
    public var hasDeadline: Bool { deadlineUnixMs != Self.noDeadline }
}

public enum SelectionKind: Sendable, Hashable {
    case hls([HlsOption])
    case bt([BtFile], saveDir: String)
    case variant([VariantOption])
    case fileExists(FileConflict)
    case plugin(site: String, url: String, candidates: [PluginCandidate])
}

/// 同站点多个插件命中时的候选插件（按 identity 升序）。
public struct PluginCandidate: Sendable, Hashable {
    public var identity: String
    public var name: String
    public var version: String
    public var description: String

    public init(identity: String, name: String, version: String, description: String) {
        self.identity = identity
        self.name = name
        self.version = version
        self.description = description
    }
}

public struct HlsOption: Sendable, Hashable {
    public var index: Int32
    public var bandwidth: Int64
    public var width: Int64
    public var height: Int64

    public init(index: Int32, bandwidth: Int64, width: Int64, height: Int64) {
        self.index = index
        self.bandwidth = bandwidth
        self.width = width
        self.height = height
    }
}

public struct BtFile: Sendable, Hashable {
    public var index: Int32
    public var path: String
    public var size: Int64

    public init(index: Int32, path: String, size: Int64) {
        self.index = index
        self.path = path
        self.size = size
    }
}

public struct VariantOption: Sendable, Hashable {
    public var index: Int32
    public var label: String
    public var container: String
    public var bandwidth: Int64
    public var width: Int64
    public var height: Int64
    public var totalBytes: Int64

    public init(
        index: Int32,
        label: String,
        container: String,
        bandwidth: Int64,
        width: Int64,
        height: Int64,
        totalBytes: Int64
    ) {
        self.index = index
        self.label = label
        self.container = container
        self.bandwidth = bandwidth
        self.width = width
        self.height = height
        self.totalBytes = totalBytes
    }
}

public enum SelectionOutcome: Sendable, Hashable {
    case hls(index: Int32)
    case bt(indices: [Int32], saveDir: String?)
    case variant(index: Int32)
    case fileExists(action: FileExistsAction)
    case plugin(identity: String, remember: Bool)
    case cancelled
}

/// `ServiceHello` 的 UI 相关子集。能力名见 README §2.1（`daemon.rss`、`agent.sync` …）。
public struct HostInfo: Sendable, Hashable {
    public var serviceName: String
    public var serviceVersion: String
    public var protocolVersion: UInt32
    public var capabilities: Set<String>

    public init(serviceName: String, serviceVersion: String, protocolVersion: UInt32, capabilities: Set<String>) {
        self.serviceName = serviceName
        self.serviceVersion = serviceVersion
        self.protocolVersion = protocolVersion
        self.capabilities = capabilities
    }

    public func has(_ capability: String) -> Bool { capabilities.contains(capability) }
}

/// 主机身份：本机（进程内 daemon + agent）或已保存的 `--server` 主机。
public enum HostRef: Sendable, Hashable, Identifiable {
    public static let localId = "local"

    case local(displayName: String)
    case remote(id: String, displayName: String, endpoint: String)

    public var id: String {
        switch self {
        case .local: Self.localId
        case let .remote(id, _, _): id
        }
    }

    public var displayName: String {
        switch self {
        case let .local(name): name
        case let .remote(_, name, _): name
        }
    }

    public var isLocal: Bool {
        if case .local = self { return true }
        return false
    }
}

/// RSS 订阅（`RssSourceDto` 的 UI 子集）。
public struct RssSource: Sendable, Hashable, Identifiable {
    public var sourceId: String
    public var name: String
    public var url: String
    public var enabled: Bool
    public var autoDownload: Bool
    public var intervalMinutes: Int32
    /// Unix 秒；0 = 从未成功。
    public var lastSuccessAt: Int64
    public var lastError: String
    public var failCount: Int32
    public var unreadCount: Int32

    public init(
        sourceId: String,
        name: String,
        url: String,
        enabled: Bool,
        autoDownload: Bool,
        intervalMinutes: Int32,
        lastSuccessAt: Int64,
        lastError: String,
        failCount: Int32,
        unreadCount: Int32
    ) {
        self.sourceId = sourceId
        self.name = name
        self.url = url
        self.enabled = enabled
        self.autoDownload = autoDownload
        self.intervalMinutes = intervalMinutes
        self.lastSuccessAt = lastSuccessAt
        self.lastError = lastError
        self.failCount = failCount
        self.unreadCount = unreadCount
    }

    public var id: String { sourceId }
}

/// 云端已信任设备（`CloudDevice` 子集）。
public struct CloudDevice: Sendable, Hashable, Identifiable {
    public var deviceId: String
    public var name: String
    public var platform: String?
    public var isOnline: Bool
    public var isCurrent: Bool
    public var appVersion: String?
    /// 设备自报的默认下载目录（远程下发不填保存目录时目标使用它）。
    public var defaultSaveDir: String?
    /// 设备自报的路径风格；nil = 未上报（按 `platform` 推断，见 ``effectivePathStyle``）。
    public var pathStyle: PathStyle?

    public init(
        deviceId: String, name: String, platform: String?, isOnline: Bool, isCurrent: Bool, appVersion: String?,
        defaultSaveDir: String? = nil, pathStyle: PathStyle? = nil
    ) {
        self.deviceId = deviceId
        self.name = name
        self.platform = platform
        self.isOnline = isOnline
        self.isCurrent = isCurrent
        self.appVersion = appVersion
        self.defaultSaveDir = defaultSaveDir
        self.pathStyle = pathStyle
    }

    public var id: String { deviceId }

    public var effectivePathStyle: PathStyle? { PathStyle.effective(reported: pathStyle, platform: platform) }
}

/// 局域网已配对设备（`LinkDeviceInfo` 子集）。
public struct LinkDevice: Sendable, Hashable, Identifiable {
    public var fingerprint: String
    public var name: String
    public var platform: String?
    public var online: Bool
    /// 设备自报的默认下载目录（远程下发不填保存目录时目标使用它）。
    public var defaultSaveDir: String?
    /// 设备自报的路径风格；nil = 未上报（按 `platform` 推断，见 ``effectivePathStyle``）。
    public var pathStyle: PathStyle?
    /// 本机备注名（`name` 已是生效名）。
    public var customName: String?
    public var appVersion: String?
    /// 对端已登录云账号时的云设备 id；与云名册同 id 时合并为一条。
    public var cloudDeviceId: String?
    /// 对端已解除配对：不可再下发，只能删除。
    public var revoked: Bool
    public var pairedAt: Int64
    public var lastSeenAt: Int64

    public init(
        fingerprint: String, name: String, platform: String?, online: Bool,
        defaultSaveDir: String? = nil, pathStyle: PathStyle? = nil,
        customName: String? = nil, appVersion: String? = nil, cloudDeviceId: String? = nil, revoked: Bool = false,
        pairedAt: Int64 = 0, lastSeenAt: Int64 = 0
    ) {
        self.fingerprint = fingerprint
        self.name = name
        self.platform = platform
        self.online = online
        self.defaultSaveDir = defaultSaveDir
        self.pathStyle = pathStyle
        self.customName = customName
        self.appVersion = appVersion
        self.cloudDeviceId = cloudDeviceId
        self.revoked = revoked
        self.pairedAt = pairedAt
        self.lastSeenAt = lastSeenAt
    }

    public var id: String { fingerprint }

    public var effectivePathStyle: PathStyle? { PathStyle.effective(reported: pathStyle, platform: platform) }
}
