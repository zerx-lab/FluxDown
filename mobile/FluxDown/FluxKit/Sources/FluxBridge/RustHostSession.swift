import FluxDomain
import Foundation
internal import FluxRustBindings

/// 生成的 UniFFI 类型（模块内部使用，避免与 FluxDomain 的 `HostSession` 同名）。
typealias RustCore = FluxRustBindings.FluxCore
typealias RustSession = FluxRustBindings.HostSession

/// 进程级入口（同 Android `FluxBridge.kt`）：持有唯一的 UniFFI `FluxCore`（多线程 tokio runtime +
/// 至多一个本机主机），对外只暴露 FluxDomain 的 `HostSession` 端口；生成的 `*Dto` 与 `FluxError`
/// 在这里归一为 FluxDomain 模型与 `HostError`。
public enum FluxBridge {
    /// 设备平台名（Rust `LocalHostConfig.platform`）。
    public static let platform = "ios"

    /// 首次访问才建 tokio runtime；创建失败（极少见）作为每次打开的错误返回。
    private static let core: Result<RustCore, HostError> = {
        do {
            return .success(try RustCore())
        } catch {
            return .failure(hostError(error))
        }
    }()

    private static func instance() throws(HostError) -> RustCore {
        try core.get()
    }

    /// 启动（仅一次）进程内 daemon + 嵌入式 agent，并返回连到它的新会话（已完成首个快照）。
    /// - Parameters:
    ///   - dataDir: 应用私有数据目录（引擎 DB、agent 状态）。
    ///   - saveDir: 默认保存目录（仅在库里尚无 `default_save_dir` 时播种）。
    ///   - deviceName: 系统设备名（`UIDevice.name`），本机设备名缺失或仍是占位名时用作云端默认名；
    ///     `nil` 回落主机名探测。
    public static func openLocal(
        dataDir: String,
        saveDir: String,
        deviceName: String?
    ) async throws(HostError) -> any FluxDomain.HostSession {
        let core = try instance()
        do {
            let config = LocalHostConfig(dataDir: dataDir, externalLogDir: nil, saveDir: saveDir, platform: platform, deviceName: deviceName)
            return RustHostSession(try await core.openLocal(config: config))
        } catch {
            throw hostError(error)
        }
    }

    /// 连接远端 `fluxdown-agent --server`：`endpoint` 为 `http(s)://host:port` 或 `ws(s)://host:port`。
    /// 完成连接、鉴权、握手与首个快照后才返回；密钥错误 / 主机不可达在此抛出。
    public static func openRemote(endpoint: String, accessKey: String) async throws(HostError) -> any FluxDomain.HostSession {
        let core = try instance()
        do {
            return RustHostSession(try await core.openRemote(endpoint: endpoint, accessKey: accessKey))
        } catch {
            throw hostError(error)
        }
    }

    /// 停止本机主机（若在运行）：先放开所有本机会话，再停 agent 与 daemon。
    public static func shutdownLocal() async {
        guard let core = try? core.get() else { return }
        await core.shutdownLocal()
    }
}

/// UniFFI `HostSession` → FluxDomain `HostSession` 端口。
/// 信号流由一个泵任务循环 `nextSignal()` 驱动；Rust 侧返回 nil（会话永久关闭）时流结束。
final class RustHostSession: FluxDomain.HostSession, @unchecked Sendable {
    private let inner: RustSession
    let signals: AsyncStream<HostSignal>

    init(_ inner: RustSession) {
        self.inner = inner
        signals = AsyncStream { continuation in
            let pump = Task.detached {
                while let signal = await inner.nextSignal() {
                    continuation.yield(signal.domain)
                }
                continuation.finish()
            }
            // 单消费者：消费方停止（切换主机 / 视图销毁）即会话无用，一并断开，避免 Rust 侧驱动空转。
            continuation.onTermination = { _ in
                pump.cancel()
                inner.disconnect()
            }
        }
    }

    deinit { inner.disconnect() }

    func close() { inner.disconnect() }

    func categorySaveDir(fileName: String, url: String) -> String? {
        inner.categorySaveDir(fileName: fileName, url: url)
    }

    private func call<T>(_ body: () async throws -> T) async throws(HostError) -> T {
        do {
            return try await body()
        } catch {
            throw hostError(error)
        }
    }

    func createTask(_ request: CreateTaskRequest) async throws(HostError) -> String {
        try await call { try await inner.createTask(request: request.dto) }
    }

    func pause(_ taskId: String) async throws(HostError) {
        try await call { try await inner.pause(taskId: taskId) }
    }

    func resume(_ taskId: String) async throws(HostError) {
        try await call { try await inner.resume(taskId: taskId) }
    }

    func delete(_ taskId: String, deleteFiles: Bool) async throws(HostError) {
        try await call { try await inner.delete(taskId: taskId, deleteFiles: deleteFiles) }
    }

    func pauseMany(_ taskIds: [String]) async throws(HostError) {
        try await call { try await inner.pauseMany(taskIds: taskIds) }
    }

    func resumeMany(_ taskIds: [String]) async throws(HostError) {
        try await call { try await inner.resumeMany(taskIds: taskIds) }
    }

    func deleteMany(_ taskIds: [String], deleteFiles: Bool) async throws(HostError) {
        try await call { try await inner.deleteMany(taskIds: taskIds, deleteFiles: deleteFiles) }
    }

    func pauseAll() async throws(HostError) {
        try await call { try await inner.pauseAll() }
    }

    func resumeAll() async throws(HostError) {
        try await call { try await inner.resumeAll() }
    }

    func rename(_ taskId: String, fileName: String) async throws(HostError) {
        try await call { try await inner.rename(taskId: taskId, fileName: fileName) }
    }

    func changeUrl(_ taskId: String, url: String) async throws(HostError) {
        try await call { try await inner.changeUrl(taskId: taskId, url: url) }
    }

    func rescan() async throws(HostError) {
        try await call { try await inner.rescan() }
    }

    func moveToQueue(_ taskId: String, queueId: String) async throws(HostError) {
        try await call { try await inner.moveToQueue(taskId: taskId, queueId: queueId) }
    }

    func boost(_ taskId: String) async throws(HostError) {
        try await call { try await inner.boost(taskId: taskId) }
    }

    func resolveSelection(_ requestId: String, outcome: SelectionOutcome) async throws(HostError) {
        try await call { try await inner.resolveSelection(requestId: requestId, outcome: outcome.dto) }
    }

    func patchConfig(expectedRevision: UInt64, values: [String: String]) async throws(HostError) {
        try await call { try await inner.patchConfig(expectedRevision: expectedRevision, values: values) }
    }

    func refreshRssSource(_ sourceId: String) async throws(HostError) {
        try await call { try await inner.refreshRssSource(sourceId: sourceId) }
    }

    func setRssSourceEnabled(_ sourceId: String, enabled: Bool) async throws(HostError) {
        try await call { try await inner.setRssSourceEnabled(sourceId: sourceId, enabled: enabled) }
    }

    func call(_ method: String, params: Data?) async throws(HostError) -> Data {
        let paramsJson: String?
        if let params {
            guard let text = String(data: params, encoding: .utf8) else {
                throw HostError(.invalidArgument, message: "params of \(method) are not UTF-8")
            }
            paramsJson = text
        } else {
            paramsJson = nil
        }
        let result = try await call { try await inner.call(method: method, paramsJson: paramsJson) }
        return Data(result.utf8)
    }
}
