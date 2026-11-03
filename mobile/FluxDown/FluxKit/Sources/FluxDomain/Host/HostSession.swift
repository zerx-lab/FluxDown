import Foundation

/// 一台主机的会话端口：方法与 `fluxdown_protocol` 的 `daemon.*` 一一对应（同 Android `HostSession.kt`）。
///
/// 唯一生产实现是 FluxBridge 的 `RustHostSession` → UniFFI `native/mobile`：本机进程内 daemon + agent /
/// 远端 `--server` 的 `/rpc`。握手、epoch/sequence 游标、缓冲、断档重同步、重连退避与 800ms 离线宽限
/// 都在 Rust，Swift 只消费“已被接受”的信号，不重复实现游标。
///
/// 所有命令失败抛 `HostError`；慢方法由调用方显示加载态。
public protocol HostSession: AnyObject, Sendable {
    /// 信号流（单消费者）：首个信号必为 `.snapshot`；之后 event / stale / snapshot（重同步）交替；
    /// 会话永久关闭（fatal 之后、`close()` 之后）时流结束。
    var signals: AsyncStream<HostSignal> { get }

    /// 结束会话：停止驱动与重连。本机主机保持运行。
    func close()

    /// 新建下载表单的分类目录预测（同步、不访问网络）：按当前主机分类路由，返回 `fileName`
    /// （不含 `.` 时回退 `url` 路径末段）命中的分类保存目录；无路由 / 无命中为 nil。
    /// 与建任务时的路由同一套匹配语义（Rust `route_save_dir`）。
    func categorySaveDir(fileName: String, url: String) -> String?

    // daemon.task.*
    func createTask(_ request: CreateTaskRequest) async throws(HostError) -> String
    func pause(_ taskId: String) async throws(HostError)
    func resume(_ taskId: String) async throws(HostError)
    func delete(_ taskId: String, deleteFiles: Bool) async throws(HostError)
    func pauseMany(_ taskIds: [String]) async throws(HostError)
    func resumeMany(_ taskIds: [String]) async throws(HostError)
    func deleteMany(_ taskIds: [String], deleteFiles: Bool) async throws(HostError)
    func pauseAll() async throws(HostError)
    func resumeAll() async throws(HostError)
    func rename(_ taskId: String, fileName: String) async throws(HostError)
    func changeUrl(_ taskId: String, url: String) async throws(HostError)
    func rescan() async throws(HostError)

    // daemon.queue.*
    /// `queueId` 为空串 = 主队列。
    func moveToQueue(_ taskId: String, queueId: String) async throws(HostError)
    func boost(_ taskId: String) async throws(HostError)

    // daemon.selection.*
    func resolveSelection(_ requestId: String, outcome: SelectionOutcome) async throws(HostError)

    // daemon.config.*：乐观并发，revision 不符抛 `.conflict`。
    func patchConfig(expectedRevision: UInt64, values: [String: String]) async throws(HostError)

    // daemon.rss.*
    func refreshRssSource(_ sourceId: String) async throws(HostError)
    func setRssSourceEnabled(_ sourceId: String, enabled: Bool) async throws(HostError)

    // 通用通道：任意 `daemon.*` / `agent.*` 方法（其它前缀抛 `.invalidArgument`）。
    /// 原始 JSON 调用：`params` / 返回值都是协议 serde wire 的 JSON（camelCase）；无结果时返回 `null`。
    /// 业务代码应使用下方 `call(_:params:)` / `callVoid` 的类型化扩展。
    func call(_ method: String, params: Data?) async throws(HostError) -> Data
}

public extension HostSession {
    /// 类型化调用：编码参数、调用、解码结果；编解码失败为 `HostError(.internal)`。
    func call<R: Decodable>(_ method: String, params: (some Encodable)?) async throws(HostError) -> R {
        let data = try await call(method, params: encodeParams(params, method: method))
        return try ProtocolJSON.decode(R.self, from: data, what: "\(method) result")
    }

    /// 无参数的类型化调用。
    func call<R: Decodable>(_ method: String) async throws(HostError) -> R {
        let data = try await call(method, params: nil)
        return try ProtocolJSON.decode(R.self, from: data, what: "\(method) result")
    }

    /// 只关心成败的写操作（忽略结果）。
    func callVoid(_ method: String, params: (some Encodable)?) async throws(HostError) {
        _ = try await call(method, params: encodeParams(params, method: method))
    }

    /// 无参数的写操作。
    func callVoid(_ method: String) async throws(HostError) {
        _ = try await call(method, params: nil)
    }
    /// Creates a manifest group. `offeredSaveDir` is non-wire authorization context: Rust
    /// applies the host's current category routing only when the submitted directory follows it.
    func createGroup(_ request: CreateGroupRequest, offeredSaveDir: String?) async throws(HostError) -> CreateGroupResponse {
        let encoded = try ProtocolJSON.encode(request, what: "daemon.group.create params")
        var object = try ProtocolJSON.decode(JSONValue.self, from: encoded, what: "daemon.group.create params")
        guard case var .object(fields) = object else {
            throw HostError(.internal, message: "group request must encode as an object")
        }
        if let offeredSaveDir {
            fields["_offeredSaveDir"] = .string(offeredSaveDir)
        }
        object = .object(fields)
        let data = try ProtocolJSON.encode(object, what: "daemon.group.create params")
        let responseData = try await call(HostMethod.daemonGroupCreate, params: data)
        return try ProtocolJSON.decode(CreateGroupResponse.self, from: responseData, what: "daemon.group.create result")
    }

}

private func encodeParams(_ params: (some Encodable)?, method: String) throws(HostError) -> Data? {
    guard let params else { return nil }
    return try ProtocolJSON.encode(params, what: "\(method) params")
}

/// `CreateTaskRequest` 的移动端子集（N1 / N2）；空串 = 由引擎推断 / 跟随全局。
public struct CreateTaskRequest: Sendable, Hashable {
    public var url: String
    public var fileName: String
    public var saveDir: String
    /// 0 = 自动。
    public var segments: Int32
    public var queueId: String
    public var startPaused: Bool
    public var cookies: String
    public var referrer: String
    public var userAgent: String
    public var proxyUrl: String
    public var checksum: String
    public var ignoreTlsErrors: Bool
    public var headers: [String: String]
    public var httpUser: String
    public var httpPassword: String
    public var saveSiteAuth: Bool
    /// 新建下载表单 / 外部捕获给出的「分类前默认目录」；nil = 不做分类路由，
    /// 非 nil 时由 Rust 按当前主机的分类规则决定最终目录。
    public var offeredSaveDir: String?

    public init(
        url: String,
        fileName: String = "",
        saveDir: String = "",
        segments: Int32 = 0,
        queueId: String = "",
        startPaused: Bool = false,
        cookies: String = "",
        referrer: String = "",
        userAgent: String = "",
        proxyUrl: String = "",
        checksum: String = "",
        ignoreTlsErrors: Bool = false,
        headers: [String: String] = [:],
        httpUser: String = "",
        httpPassword: String = "",
        saveSiteAuth: Bool = false,
        offeredSaveDir: String? = nil
    ) {
        self.url = url
        self.fileName = fileName
        self.saveDir = saveDir
        self.segments = segments
        self.queueId = queueId
        self.startPaused = startPaused
        self.cookies = cookies
        self.referrer = referrer
        self.userAgent = userAgent
        self.proxyUrl = proxyUrl
        self.checksum = checksum
        self.ignoreTlsErrors = ignoreTlsErrors
        self.headers = headers
        self.httpUser = httpUser
        self.httpPassword = httpPassword
        self.saveSiteAuth = saveSiteAuth
        self.offeredSaveDir = offeredSaveDir
    }
}

/// 主机推送给 UI 仓库的信号（对应 UniFFI `HostSignalDto`）。
public enum HostSignal: Sendable {
    /// 全量快照（连接 / 重同步）。
    case snapshot(HostSnapshot)
    /// 已按游标规则接受的增量事件。
    case event(HostEvent)
    /// 离线宽限（800ms）已过：数据只读、清空运行时。
    case stale
    /// 不可恢复错误（协议不兼容、鉴权失败等）。
    case fatal(HostError)
}

/// `AgentSnapshot` 中移动端渲染的子集。
public struct HostSnapshot: Sendable {
    public var info: HostInfo
    public var daemonConnected: Bool
    public var tasks: [DownloadTask]
    /// 任务表尚未载入完成：此时 `tasks` 只是空占位（Rust 驱动在分页拉取期间先发这一帧，载入完成后再发完整快照），
    /// 绝不能当作完整列表（计数 / 空态 / 批量范围 / 删除推断）。
    public var tasksLoading: Bool
    /// `tasksLoading` 期间服务端的任务总数（供进度文案）；载入完成后为 0。
    public var taskTotal: UInt64
    public var runtime: [String: TaskRuntime]
    public var queues: [TaskQueue]
    /// 排队任务的排序键，按键升序（名次 = 下标 + 1 即「排队 #n」）。
    public var queueOrder: [QueueOrderEntry]
    public var groups: [DownloadGroup]
    public var stats: RuntimeStats
    /// 0 或 1 个优先任务。
    public var priorityTaskId: String?
    public var pendingSelections: [SelectionRequest]
    /// `DaemonConfigSnapshot`：全部主机配置键（字符串值）。
    public var config: [String: String]
    public var configRevision: UInt64
    public var rssSources: [RssSource]
    public var cloudDevices: [CloudDevice]
    public var linkDevices: [LinkDevice]
    /// 偏好 `custom_categories` 解析结果（空 / 损坏已由 Rust 回退内置基线）。
    public var categories: [TaskCategory]
    /// 其余 `AgentSnapshot` / `DaemonSnapshot` 分区：键见 ``HostSection``，值为协议 serde wire 的 JSON。
    public var sections: [String: Data]

    public init(
        info: HostInfo,
        daemonConnected: Bool = true,
        tasks: [DownloadTask] = [],
        tasksLoading: Bool = false,
        taskTotal: UInt64 = 0,
        runtime: [String: TaskRuntime] = [:],
        queues: [TaskQueue] = [],
        queueOrder: [QueueOrderEntry] = [],
        groups: [DownloadGroup] = [],
        stats: RuntimeStats = RuntimeStats(),
        priorityTaskId: String? = nil,
        pendingSelections: [SelectionRequest] = [],
        config: [String: String] = [:],
        configRevision: UInt64 = 0,
        rssSources: [RssSource] = [],
        cloudDevices: [CloudDevice] = [],
        linkDevices: [LinkDevice] = [],
        categories: [TaskCategory] = TaskCategory.builtin,
        sections: [String: Data] = [:]
    ) {
        self.info = info
        self.daemonConnected = daemonConnected
        self.tasks = tasks
        self.tasksLoading = tasksLoading
        self.taskTotal = taskTotal
        self.runtime = runtime
        self.queues = queues
        self.queueOrder = queueOrder
        self.groups = groups
        self.stats = stats
        self.priorityTaskId = priorityTaskId
        self.pendingSelections = pendingSelections
        self.config = config
        self.configRevision = configRevision
        self.rssSources = rssSources
        self.cloudDevices = cloudDevices
        self.linkDevices = linkDevices
        self.categories = categories
        self.sections = sections
    }
}

/// 移动端订阅的事件子集（`DaemonEvent` / `WsServerMsg` 经 Rust 侧归一）。
public enum HostEvent: Sendable {
    case taskChanged(DownloadTask)
    case taskDeleted(taskId: String)
    /// `WsServerMsg::TaskProgress`：速度只在这里出现。
    case taskProgress(TaskProgress)
    case taskRuntimeChanged(TaskRuntime)
    case queuesChanged([TaskQueue])
    /// 队列顺序整表替换（首次 / 用户重排）：按键升序。
    case queuePositionsChanged([QueueOrderEntry])
    /// 队列顺序增量：先移除 `removed`，再按键插入 / 更新 `added`。
    case queueOrderChanged(removed: [String], added: [QueueOrderEntry])
    case groupsChanged([DownloadGroup])
    case fileMissingChanged([String: Bool])
    case priorityTaskChanged(taskId: String?)
    case runtimeStatsChanged(RuntimeStats)
    case daemonConnectionChanged(connected: Bool)
    case selectionPending(SelectionRequest)
    case selectionResolved(requestId: String)
    case configChanged(values: [String: String], revision: UInt64)
    case rssSourcesChanged([RssSource])
    case cloudDevicesChanged([CloudDevice])
    case linkedDevicesChanged([LinkDevice])
    case categoriesChanged([TaskCategory])
    /// 通用分区变化（键见 ``HostSection``）：`json` 是该分区的完整新值（协议 serde wire）。
    case sectionChanged(name: String, json: Data)
    /// 一次性通知（不进快照；名称见 ``HostNoticeName``），`json` 为载荷。
    case notice(name: String, json: Data)
}

/// `HostEvent.taskProgress` 载荷。
public struct TaskProgress: Sendable, Hashable {
    public var taskId: String
    public var status: Int32
    public var downloadedBytes: Int64
    public var totalBytes: Int64
    public var speed: Int64
    public var uploadSpeed: Int64
    public var fileName: String
    public var errorMessage: String
    public var uploadedBytes: Int64
    public var seedingStatus: Int32

    public init(
        taskId: String,
        status: Int32,
        downloadedBytes: Int64,
        totalBytes: Int64,
        speed: Int64,
        uploadSpeed: Int64 = 0,
        fileName: String = "",
        errorMessage: String = "",
        uploadedBytes: Int64 = 0,
        seedingStatus: Int32 = 0
    ) {
        self.taskId = taskId
        self.status = status
        self.downloadedBytes = downloadedBytes
        self.totalBytes = totalBytes
        self.speed = speed
        self.uploadSpeed = uploadSpeed
        self.fileName = fileName
        self.errorMessage = errorMessage
        self.uploadedBytes = uploadedBytes
        self.seedingStatus = seedingStatus
    }
}

/// `ApplicationErrorCode`。
public enum HostErrorCode: String, Sendable, Hashable, CaseIterable {
    case protocolIncompatible, unauthorized, invalidArgument, notFound, conflict
    case unavailable, timeout, cancelled, unsupported, `internal`
}

/// `RpcErrorData`：UI 先按 `reason`（`ErrorReason` 的 wire 名）本地化，再按 `code` 回退。
public struct HostError: Error, Sendable, Hashable, CustomStringConvertible {
    public var code: HostErrorCode
    public var reason: String?
    public var retryable: Bool
    public var message: String

    public init(_ code: HostErrorCode, reason: String? = nil, retryable: Bool = false, message: String = "") {
        self.code = code
        self.reason = reason
        self.retryable = retryable
        self.message = message
    }

    public var description: String {
        let detail = message.isEmpty ? (reason ?? code.rawValue) : message
        return "\(code.rawValue): \(detail)"
    }
}
