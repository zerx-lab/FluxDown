import Foundation

// 跨设备任务（`agent.remote.*`，经 FluxCloud）。镜像 `native/protocol/src/agent.rs::RemoteTaskDto` 等；
// 控制矩阵镜像 Web `batchPlan.ts::remoteCan` ↔ GPUI `dispatch.rs::remote_action_applies`。

/// `RemoteTaskStatus`：云端新增、本端不认识的状态落入 `unknown`（不参与控制）。
public enum RemoteTaskStatus: WireStringEnum {
    case accepted, downloading, paused, completed, failed, canceled, pending
    case unknown(String)

    public init(wire: String) {
        switch wire {
        case "accepted": self = .accepted
        case "downloading": self = .downloading
        case "paused": self = .paused
        case "completed": self = .completed
        case "failed": self = .failed
        case "canceled": self = .canceled
        case "pending": self = .pending
        default: self = .unknown(wire)
        }
    }

    public var wire: String {
        switch self {
        case .accepted: "accepted"
        case .downloading: "downloading"
        case .paused: "paused"
        case .completed: "completed"
        case .failed: "failed"
        case .canceled: "canceled"
        case .pending: "pending"
        case let .unknown(raw): raw
        }
    }

    public var isUnknown: Bool {
        if case .unknown = self { true } else { false }
    }

    /// 任务已结束（不再有速度 / 进度变化）。
    public var isTerminal: Bool {
        switch self {
        case .completed, .failed, .canceled: true
        default: false
        }
    }

    /// 暂停 / 继续这类需要目标设备在线才能送达的命令（取消 / 删除在云端直接生效）。
    public func needsTargetOnline(for action: RemoteCommandAction) -> Bool {
        action == .pause || action == .resume
    }

    /// 云端命令是否适用于该状态：未知一律不可控制（含删除）；暂停 / 取消只对进行中，继续只对已暂停（取消也适用），
    /// 删除对任何已知状态成立。
    public func allows(_ action: RemoteCommandAction) -> Bool {
        switch self {
        case .unknown:
            false
        case .pending, .accepted, .downloading:
            action == .pause || action == .cancel || action == .delete
        case .paused:
            action == .resume || action == .cancel || action == .delete
        case .completed, .failed, .canceled:
            action == .delete
        }
    }
}

/// `agent.remote.command` 的动作。
public enum RemoteCommandAction: String, Sendable, Hashable, Codable, CaseIterable {
    case pause, resume
    /// 云端直接置 `canceled`（不依赖目标在线），目标设备删除其本地任务、保留已下载文件。
    case cancel
    /// 云端直接删除记录；目标在线时按 `deleteFiles` 决定是否同时删文件。
    case delete
}

/// 跨设备任务的 UI 投影（`RemoteTaskDto`）。
public struct RemoteTaskDto: Sendable, Hashable, Identifiable, Codable {
    public var id: String
    public var fromDevice: String
    /// 目标设备的 `CloudDeviceRecord.deviceId`。
    public var toDevice: String
    public var url: String
    public var saveDir: String?
    /// 云端对未指定文件名的任务回 `null`，按空串处理。
    public var fileName: String
    public var status: RemoteTaskStatus
    public var totalBytes: Int64?
    public var downloadedBytes: Int64
    public var speed: Int64
    /// 0…1。
    public var progress: Double
    public var error: String?
    public var createdAt: String
    public var updatedAt: String

    public init(
        id: String, fromDevice: String = "", toDevice: String = "", url: String = "", saveDir: String? = nil,
        fileName: String = "", status: RemoteTaskStatus = .pending, totalBytes: Int64? = nil, downloadedBytes: Int64 = 0,
        speed: Int64 = 0, progress: Double = 0, error: String? = nil, createdAt: String = "", updatedAt: String = ""
    ) {
        self.id = id
        self.fromDevice = fromDevice
        self.toDevice = toDevice
        self.url = url
        self.saveDir = saveDir
        self.fileName = fileName
        self.status = status
        self.totalBytes = totalBytes
        self.downloadedBytes = downloadedBytes
        self.speed = speed
        self.progress = progress
        self.error = error
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        fromDevice = try c.decodeIfPresent(String.self, forKey: .fromDevice) ?? ""
        toDevice = try c.decodeIfPresent(String.self, forKey: .toDevice) ?? ""
        url = try c.decodeIfPresent(String.self, forKey: .url) ?? ""
        saveDir = try c.decodeIfPresent(String.self, forKey: .saveDir)
        fileName = try c.decodeIfPresent(String.self, forKey: .fileName) ?? ""
        status = try c.decodeIfPresent(RemoteTaskStatus.self, forKey: .status) ?? .pending
        totalBytes = try c.decodeIfPresent(Int64.self, forKey: .totalBytes)
        downloadedBytes = try c.decodeIfPresent(Int64.self, forKey: .downloadedBytes) ?? 0
        speed = try c.decodeIfPresent(Int64.self, forKey: .speed) ?? 0
        progress = try c.decodeIfPresent(Double.self, forKey: .progress) ?? 0
        error = try c.decodeIfPresent(String.self, forKey: .error)
        createdAt = try c.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        updatedAt = try c.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
    }
}

/// `agent.remote.dispatch` 参数：把下载经 FluxCloud 下发到本账号另一台受信任设备。
public struct RemoteDispatchParams: Sendable, Hashable, Codable {
    /// 目标设备的 `CloudDeviceRecord.deviceId`。
    public var toDevice: String
    public var url: String
    /// 省略 = 由目标设备按 URL 推断。
    public var fileName: String?
    /// 目标设备上的保存目录；省略 = 目标设备默认下载目录。必须符合目标设备路径风格。
    public var saveDir: String?

    public init(toDevice: String, url: String, fileName: String? = nil, saveDir: String? = nil) {
        self.toDevice = toDevice
        self.url = url
        self.fileName = fileName
        self.saveDir = saveDir
    }
}

public struct RemoteDispatchResult: Sendable, Hashable, Codable {
    public var task: RemoteTaskDto

    public init(task: RemoteTaskDto) { self.task = task }
}

/// `agent.remote.command` 参数。
public struct RemoteCommandParams: Sendable, Hashable, Codable {
    public var taskId: String
    public var action: RemoteCommandAction
    /// 幂等键；省略时 agent 生成唯一值（同一动作可重复下发）。
    public var commandId: String?
    /// 仅 `delete`：目标设备同时删除已下载文件。
    public var deleteFiles: Bool

    public init(taskId: String, action: RemoteCommandAction, commandId: String? = nil, deleteFiles: Bool = false) {
        self.taskId = taskId
        self.action = action
        self.commandId = commandId
        self.deleteFiles = deleteFiles
    }
}

/// `agent.remote.commandMany` 参数：同一动作批量作用于多条远程任务（语义同逐条 `agent.remote.command`）。
public struct RemoteCommandManyParams: Sendable, Hashable, Encodable {
    /// 单次调用的任务数上限（`REMOTE_COMMAND_MANY_MAX`）；更大的选择由客户端分块。
    public static let maxIds = 1000

    public var taskIds: [String]
    public var action: RemoteCommandAction
    /// 仅 `delete`：目标设备同时删除已下载文件。
    public var deleteFiles: Bool

    public init(taskIds: [String], action: RemoteCommandAction, deleteFiles: Bool = false) {
        self.taskIds = taskIds
        self.action = action
        self.deleteFiles = deleteFiles
    }
}

/// 批量命令中失败的一条：`error` 是标准 `RpcErrorData`，映射为 `HostError`（未知 `code` 落入 `.internal`）。
public struct RemoteCommandFailure: Sendable, Hashable, Decodable {
    public var taskId: String
    public var error: HostError

    public init(taskId: String, error: HostError) {
        self.taskId = taskId
        self.error = error
    }

    private enum CodingKeys: String, CodingKey { case taskId, error }

    private struct Wire: Decodable {
        var code: String?
        var retryable: Bool?
        var reason: String?
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        taskId = try c.decode(String.self, forKey: .taskId)
        let wire = try c.decodeIfPresent(Wire.self, forKey: .error)
        error = HostError(
            wire?.code.flatMap(HostErrorCode.init(rawValue:)) ?? .internal,
            reason: wire?.reason,
            retryable: wire?.retryable ?? false
        )
    }
}

/// `agent.remote.commandMany` 结果：`succeeded` 与 `failed` 合起来覆盖去重后的输入 id。缺失的数组按空处理。
public struct RemoteCommandManyResult: Sendable, Hashable, Decodable {
    public var succeeded: [String]
    public var failed: [RemoteCommandFailure]

    public init(succeeded: [String] = [], failed: [RemoteCommandFailure] = []) {
        self.succeeded = succeeded
        self.failed = failed
    }

    private enum CodingKeys: String, CodingKey { case succeeded, failed }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        succeeded = try c.decodeIfPresent([String].self, forKey: .succeeded) ?? []
        failed = try c.decodeIfPresent([RemoteCommandFailure].self, forKey: .failed) ?? []
    }
}

/// 多选里的远程任务批量命令：筛选可下发项、分块、执行与汇总（纯逻辑；调用方注入 RPC 闭包）。
public enum RemoteBatch {
    /// 旧主机（无 `agent.remoteBatch`）逐条下发的并发上限。
    public static let fallbackConcurrency = 4

    /// 汇总：成功 id 与失败项均按输入顺序。
    public struct Outcome: Sendable, Hashable {
        public var succeeded: [String]
        public var failed: [RemoteCommandFailure]

        public init(succeeded: [String] = [], failed: [RemoteCommandFailure] = []) {
            self.succeeded = succeeded
            self.failed = failed
        }

        /// 第一个失败的错误（Toast 用）。
        public var firstError: HostError? { failed.first?.error }
        public var isEmpty: Bool { succeeded.isEmpty && failed.isEmpty }
    }

    /// 选中的远程任务里对 `action` 适用的 id（去重、保序）；适用性只走 `RemoteTaskRules.canIssue`。
    public static func eligibleIds(
        _ action: RemoteCommandAction,
        in tasks: [RemoteTaskDto],
        targetOnline: (RemoteTaskDto) -> Bool?
    ) -> [String] {
        var seen = Set<String>()
        var out: [String] = []
        for task in tasks where RemoteTaskRules.canIssue(action, to: task, targetOnline: targetOnline(task)) {
            if seen.insert(task.id).inserted { out.append(task.id) }
        }
        return out
    }

    /// 按 `size`（默认 `RemoteCommandManyParams.maxIds`）分块。
    public static func chunks(_ ids: [String], size: Int = RemoteCommandManyParams.maxIds) -> [[String]] {
        let size = max(1, size)
        return stride(from: 0, to: ids.count, by: size).map { Array(ids[$0 ..< min($0 + size, ids.count)]) }
    }

    /// 执行：`batched` 时逐块调用 `many`（整块调用失败 = 该块全部记失败），否则用 `single` 以有界并发逐条下发。
    /// 不抛出：所有失败都进入结果。
    public static func run(
        ids: [String],
        batched: Bool,
        chunkSize: Int = RemoteCommandManyParams.maxIds,
        concurrency: Int = fallbackConcurrency,
        many: @escaping @Sendable ([String]) async throws(HostError) -> RemoteCommandManyResult,
        single: @escaping @Sendable (String) async throws(HostError) -> Void
    ) async -> Outcome {
        var outcome = Outcome()
        if batched {
            for chunk in chunks(ids, size: chunkSize) {
                do throws(HostError) {
                    let result = try await many(chunk)
                    outcome.succeeded.append(contentsOf: result.succeeded)
                    outcome.failed.append(contentsOf: result.failed)
                } catch {
                    outcome.failed.append(contentsOf: chunk.map { RemoteCommandFailure(taskId: $0, error: error) })
                }
            }
            return outcome
        }
        let limit = max(1, concurrency)
        let results = await withTaskGroup(of: (Int, HostError?).self, returning: [(Int, HostError?)].self) { group in
            var collected: [(Int, HostError?)] = []
            var next = 0
            func start(_ index: Int, in group: inout TaskGroup<(Int, HostError?)>) {
                let id = ids[index]
                group.addTask {
                    do throws(HostError) {
                        try await single(id)
                        return (index, nil)
                    } catch {
                        return (index, error)
                    }
                }
            }
            while next < min(limit, ids.count) {
                start(next, in: &group)
                next += 1
            }
            while let done = await group.next() {
                collected.append(done)
                if next < ids.count {
                    start(next, in: &group)
                    next += 1
                }
            }
            return collected
        }
        for (index, error) in results.sorted(by: { $0.0 < $1.0 }) {
            if let error {
                outcome.failed.append(RemoteCommandFailure(taskId: ids[index], error: error))
            } else {
                outcome.succeeded.append(ids[index])
            }
        }
        return outcome
    }
}

// MARK: - 纯规则

/// 远程任务列表规则。
public enum RemoteTaskRules {
    /// 目标为本机的远程任务在本地已有真实任务，不再作为镜像行显示。
    public static func visible(_ tasks: [RemoteTaskDto], currentDeviceId: String?) -> [RemoteTaskDto] {
        guard let currentDeviceId else { return tasks }
        return tasks.filter { $0.toDevice != currentDeviceId }
    }

    /// 按目标设备计数。
    public static func countByTarget(_ tasks: [RemoteTaskDto]) -> [String: Int] {
        tasks.reduce(into: [:]) { counts, task in counts[task.toDevice, default: 0] += 1 }
    }

    /// 命令是否应当显示为可点：状态矩阵 + （暂停 / 继续）目标在线。`targetOnline == nil` = 在线状态未知，不据此禁用。
    public static func canIssue(_ action: RemoteCommandAction, to task: RemoteTaskDto, targetOnline: Bool?) -> Bool {
        guard task.status.allows(action) else { return false }
        if task.status.needsTargetOnline(for: action), targetOnline == false { return false }
        return true
    }

    /// 行尾单一主按钮：进行中 → 暂停，已暂停 → 继续，其余无。
    public static func primaryAction(for task: RemoteTaskDto) -> RemoteCommandAction? {
        if task.status.allows(.pause) { return .pause }
        if task.status.allows(.resume) { return .resume }
        return nil
    }

    /// 状态筛选（V1 §E 顶部分段）。
    public enum Filter: Sendable, Hashable, CaseIterable {
        case all, downloading, paused, completed, failedOrCanceled

        public func matches(_ status: RemoteTaskStatus) -> Bool {
            switch self {
            case .all: true
            case .downloading:
                switch status {
                case .pending, .accepted, .downloading: true
                default: false
                }
            case .paused: status == .paused
            case .completed: status == .completed
            case .failedOrCanceled: status == .failed || status == .canceled
            }
        }
    }

    /// 排序：未结束优先，同组按 `updatedAt`（ISO 字符串）降序。
    public static func sorted(_ tasks: [RemoteTaskDto]) -> [RemoteTaskDto] {
        tasks.enumerated().sorted { lhs, rhs in
            let a = lhs.element, b = rhs.element
            if a.status.isTerminal != b.status.isTerminal { return !a.status.isTerminal }
            if a.updatedAt != b.updatedAt { return a.updatedAt > b.updatedAt }
            return lhs.offset < rhs.offset
        }.map(\.element)
    }
}
