import FluxBridge
import FluxDomain
import FluxUI
import Foundation
import UIKit
import os

/// 进程级依赖（同 Android `AppContainer`）：主机会话、状态仓库、设备本地偏好、主机列表、反馈。
///
/// 主机 = 本机（FluxBridge 经 UniFFI 在进程内跑 daemon + agent，冷启动即打开）或已保存的远端
/// `fluxdown-agent --server`（`/rpc` + 访问密钥，密钥存钥匙串）。UI 只依赖 `HostSession` 端口与
/// `HostStore`；切换主机时 store 先 detach，再关旧会话，再 attach 新会话。
@MainActor
@Observable
final class AppContainer {
    let store = HostStore()
    let appearance = AppearanceStore()
    let viewPrefs = ViewPrefsStore()
    let toasts = ToastCenter()
    let router = AppRouter()
    /// 设备 / 远程任务的数据与动作（设备页与下载列表的远程行共用：命令在途状态只有一份）。
    @ObservationIgnored private(set) lazy var devices = DevicesModel(container: self)

    /// 本机 + 已保存的远端主机（本机恒为首项）。
    private(set) var hosts: [HostRef]
    /// 当前主机（打开期间 / 打开失败时仍是目标主机；连接状态看 `store.state.connection`）。
    private(set) var host: HostRef
    /// 正在切换 / 打开主机。
    private(set) var isSwitching = false

    /// 当前会话；本机尚未打开或打开失败时是 `UnavailableSession`（命令抛 `HostError`）。
    @ObservationIgnored private(set) var session: any HostSession

    @ObservationIgnored private let hostRepo = HostRepo()
    @ObservationIgnored private let localRef: HostRef
    @ObservationIgnored private var switchQueue: Task<Void, Never>?
    @ObservationIgnored private var localSaveDirRepair: Task<Void, Never>?
    @ObservationIgnored private var restoreStarted = false
    @ObservationIgnored private var restoreSuperseded = false
    @ObservationIgnored private var lastRescan: ContinuousClock.Instant?
    @ObservationIgnored private let log = Logger(subsystem: "com.fluxdown.app", category: "host")

    private static let connectTimeout: Duration = .seconds(15)
    private static let rescanCooldown: Duration = .seconds(10)

    init() {
        localRef = .local(displayName: L("mobileHostLocalName"))
        host = localRef
        session = UnavailableSession(HostError(.unavailable, message: "host starting"))
        hosts = [localRef]
        hosts = [localRef] + hostRepo.remotes
        // 不先绑定占位会话：store 保持 `.connecting`，避免冷启动闪一下“连接失败”。
        serialized { container in
            let opened = await container.open(container.localRef)
            switch opened {
            case let .success(session): container.adopt(container.localRef, session)
            case let .failure(error): container.adopt(container.localRef, UnavailableSession(error))
            }
        }
    }

    /// Publish the formatting preference before invalidating views that read it.
    func setFileSizeUnits(_ units: FileSizeUnits) {
        guard units != appearance.fileSizeUnits else { return }
        Format.setFileSizeUnits(units)
        appearance.fileSizeUnits = units
    }

    /// 是否本机主机（打开文件 / 在「文件」中显示 只对本机任务有意义）。
    var isLocalHost: Bool { host.isLocal }

    // MARK: 主机切换

    func supersedeHostRestore() {
        restoreSuperseded = true
    }

    /// 首次显示时启动容器级恢复；视图消失不取消远端连接。
    func startHostRestore() {
        guard !restoreStarted, !restoreSuperseded else { return }
        restoreStarted = true
        guard let ref = hostRepo.lastRemote else { return }
        Task { await self.restoreHost(ref) }
    }

    private func restoreHost(_ ref: HostRef) async {
        guard !restoreSuperseded else { return }
        let opened = await open(ref)
        await switchQueue?.value
        // Let the startup local session finish migrating its own configuration before closing it.
        if !restoreSuperseded, case .success = opened { await localSaveDirRepair?.value }
        guard !restoreSuperseded else {
            if case let .success(session) = opened { session.close() }
            return
        }
        switch opened {
        case let .success(session):
            adopt(ref, session)
        case .failure:
            let name = ref.localizedName
            let prefix = name.prefix(20)
            let label = prefix.endIndex == name.endIndex ? name : String(prefix) + "…"
            toasts.show(text: L("hostRestoreFailed", ["name": label]), tone: .warning)
        }
    }

    /// 切换主机。打开失败时保持原主机不变并返回失败。
    func switchHost(_ ref: HostRef, remember: Bool = true, forceReconnect: Bool = false) async -> Result<Void, HostError> {
        supersedeHostRestore()
        return await withCheckedContinuation { continuation in
            serialized { container in
                let changedHost = ref.id != container.host.id
                if !forceReconnect, !changedHost, !(container.session is UnavailableSession) {
                    if remember, !ref.isLocal { container.hostRepo.rememberRemote(ref.id) }
                    continuation.resume(returning: .success(()))
                    return
                }
                switch await container.open(ref) {
                case let .success(session):
                    container.adopt(ref, session)
                    if remember, changedHost || !ref.isLocal { container.hostRepo.rememberRemote(ref.isLocal ? nil : ref.id) }
                    continuation.resume(returning: .success(()))
                case let .failure(error):
                    continuation.resume(returning: .failure(error))
                }
            }
        }
    }

    /// 保存远端主机：先真实连接并拿到首个快照（鉴权 / 协议 / 可达性），通过后把凭据存入钥匙串。
    ///
    /// `linkFingerprint` 非空 = 局域网配对得到的设备控制凭据（`accessKey` 是该凭据）：同一指纹已保存过的主机
    /// 原地更新凭据与地址（重新配对不会新增重复主机），且它正是当前主机时立即用新凭据重开会话。
    func addRemoteHost(
        name: String,
        endpoint: String,
        accessKey: String,
        linkFingerprint: String? = nil
    ) async -> Result<HostRef, HostError> {
        let url = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        let key = accessKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty, !key.isEmpty else {
            return .failure(HostError(.invalidArgument, message: "endpoint and access key are required"))
        }
        do throws(HostError) {
            let probe = try await FluxBridge.openRemote(endpoint: url, accessKey: key)
            defer { probe.close() }
            _ = try await Self.awaitFirstSnapshot(probe)
            let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
            let display = trimmedName.isEmpty ? Self.displayHost(url) : trimmedName
            let ref: HostRef
            if let linkFingerprint, let existing = hostRepo.remote(linkFingerprint: linkFingerprint) {
                ref = try hostRepo.updateCredential(id: existing.id, endpoint: url, accessKey: key)
            } else {
                ref = try hostRepo.add(name: display, endpoint: url, accessKey: key, linkFingerprint: linkFingerprint)
            }
            hosts = [localRef] + hostRepo.remotes
            if ref.id == host.id, linkFingerprint != nil {
                if case let .failure(error) = await reopenCurrent(ref) { throw error }
            }
            return .success(ref)
        } catch {
            log.warning("add remote host failed: \(String(describing: error), privacy: .public)")
            return .failure(error)
        }
    }

    /// 当前远端主机的访问密钥已在主机侧更换：写入钥匙串并用新密钥重开同一主机
    /// （旧会话仍持旧密钥，重连会鉴权失败）。本机主机无访问密钥，返回 `.invalidArgument`。
    func updateRemoteAccessKey(_ key: String) async -> Result<Void, HostError> {
        guard case let .remote(id, _, _) = host else {
            return .failure(HostError(.invalidArgument, message: "current host is local"))
        }
        // 经局域网配对的主机用的是设备专属控制凭据：访问密钥怎么改都不影响它，不能用访问密钥覆盖它。
        if hostRepo.linkFingerprint(id: id) != nil { return .success(()) }
        do throws(HostError) {
            try hostRepo.updateAccessKey(id: id, key: key)
        } catch {
            return .failure(error)
        }
        return await reopenCurrent(host)
    }

    /// 用 `ref` 重开会话并接管为当前主机（凭据 / 地址已写入仓库之后）。
    private func reopenCurrent(_ ref: HostRef) async -> Result<Void, HostError> {
        await withCheckedContinuation { continuation in
            serialized { container in
                switch await container.open(ref) {
                case let .success(session):
                    container.adopt(ref, session)
                    continuation.resume(returning: .success(()))
                case let .failure(error):
                    continuation.resume(returning: .failure(error))
                }
            }
        }
    }

    /// 删除已保存的远端主机；正在使用时先回到本机。本机不可删除。
    /// 经局域网配对添加的主机（带 `linkFingerprint`）同时对本机主机尽力调用 `agent.link.remove`，
    /// 让主机侧撤销这台手机的控制授权；失败只记日志（主机离线时手机侧仍然删除，主机侧可手动解除）。
    func removeHost(id: String) async -> Result<Void, HostError> {
        guard id != HostRef.localId else { return .success(()) }
        supersedeHostRestore()
        return await withCheckedContinuation { continuation in
            serialized { container in
                if container.host.id == id {
                    switch await container.open(container.localRef) {
                    case let .success(session): container.adopt(container.localRef, session)
                    case let .failure(error): container.adopt(container.localRef, UnavailableSession(error))
                    }
                }
                let fingerprint = container.hostRepo.linkFingerprint(id: id)
                do throws(HostError) {
                    try container.hostRepo.remove(id: id)
                    container.hosts = [container.localRef] + container.hostRepo.remotes
                    continuation.resume(returning: .success(()))
                } catch {
                    continuation.resume(returning: .failure(error))
                    return
                }
                if let fingerprint {
                    Task { [weak container] in await container?.revokeLinkPairing(fingerprint: fingerprint) }
                }
            }
        }
    }

    /// 主机侧的配对记录指纹（经局域网配对添加的主机）；访问密钥主机 = nil。
    func linkFingerprint(ofHost id: String) -> String? { hostRepo.linkFingerprint(id: id) }

    /// 本机主机的独立会话（配对用，与当前连着哪台主机无关；本机主机进程内唯一，多个会话共享它）。
    /// 调用方用完必须 `close()`。
    func openLocalSession() async -> Result<any HostSession, HostError> {
        await open(localRef)
    }

    /// 后台通知使用独立会话，不切换前台主机或改写启动记录。
    func resolveDefaultSelection(_ requestID: String, hostID: String) async throws(HostError) {
        guard let ref = hosts.first(where: { $0.id == hostID }) else { return }
        let connection = await open(ref)
        let session = try connection.get()
        defer { session.close() }
        let snapshot = try await Self.awaitFirstSnapshot(session)
        guard let request = snapshot.pendingSelections.first(where: { $0.requestId == requestID }) else { return }
        try await session.resolveSelection(requestID, outcome: request.defaultChoice)
    }

    private func revokeLinkPairing(fingerprint: String) async {
        switch await open(localRef) {
        case let .success(session):
            defer { session.close() }
            do throws(HostError) {
                try await AgentAPI(session: session).linkRemove(fingerprint: fingerprint)
            } catch {
                log.warning("revoke link pairing failed: \(error.description, privacy: .public)")
            }
        case let .failure(error):
            log.warning("revoke link pairing: local host unavailable: \(error.description, privacy: .public)")
        }
    }

    /// 回到前台时请求主机重扫文件跟踪（`daemon.task.rescan`），10s 冷却；前后台均不周期轮询（空闲静默）。
    func rescanOnForeground() {
        let now = ContinuousClock.now
        if let last = lastRescan, now - last < Self.rescanCooldown { return }
        lastRescan = now
        let session = session
        Task {
            do throws(HostError) {
                try await session.rescan()
            } catch {
                log.info("rescan failed: \(error.description, privacy: .public)")
            }
        }
    }

    // MARK: 内部

    /// 主机打开 / 切换串行执行（同 Android `switchLock`）。
    private func serialized(_ body: @escaping @MainActor (AppContainer) async -> Void) {
        let previous = switchQueue
        switchQueue = Task { [weak self] in
            await previous?.value
            guard let self else { return }
            self.isSwitching = true
            await body(self)
            self.isSwitching = false
        }
    }

    private func open(_ ref: HostRef) async -> Result<any HostSession, HostError> {
        do {
            switch ref {
            case .local:
                try prepareLocalDirectories()
                let session = try await FluxBridge.openLocal(
                    dataDir: LocalPaths.dataDirectory.path,
                    saveDir: LocalPaths.documents.path,
                    deviceName: LocalDevice.name
                )
                return .success(session)
            case let .remote(id, _, endpoint):
                guard let key = try hostRepo.accessKey(id: id) else {
                    return .failure(HostError(.unauthorized, message: "access key unavailable"))
                }
                return .success(try await FluxBridge.openRemote(endpoint: endpoint, accessKey: key))
            }
        } catch {
            log.warning("open host \(ref.id, privacy: .public) failed: \(error.description, privacy: .public)")
            return .failure(error)
        }
    }

    private func prepareLocalDirectories() throws(HostError) {
        do {
            try LocalPaths.prepare()
        } catch {
            throw HostError(.internal, message: "prepare directories: \(error.localizedDescription)")
        }
    }

    private func adopt(_ ref: HostRef, _ next: any HostSession) {
        store.detach()
        let old = session
        session = next
        host = ref
        old.close()
        store.attach(next)
        localSaveDirRepair?.cancel()
        localSaveDirRepair = nil
        if ref.isLocal, !(next is UnavailableSession) {
            localSaveDirRepair = Task { [weak self] in await self?.repairLocalSaveDir(session: next) }
        }
    }

    /// App 更新后沙盒容器路径可能变化：库里的 `default_save_dir` 若指向旧容器的 Documents，改写到当前容器。
    private func repairLocalSaveDir(session: any HostSession) async {
        // 新会话的首个快照通常在 attach 后立即到达；最多等 5s。
        var attempts = 0
        while store.state.connection != .live, attempts < 50 {
            attempts += 1
            do {
                try await Task.sleep(for: .milliseconds(100))
            } catch {
                return
            }
        }
        let state = store.state
        guard !Task.isCancelled, self.session === session, state.connection == .live,
              let saved = state.config["default_save_dir"], !saved.isEmpty else { return }
        let current = LocalPaths.rebased(saved)
        guard current != saved else { return }
        do throws(HostError) {
            try await session.patchConfig(expectedRevision: state.configRevision, values: ["default_save_dir": current])
        } catch {
            log.warning("repair default_save_dir failed: \(error.description, privacy: .public)")
        }
    }

    private static func awaitFirstSnapshot(_ probe: any HostSession) async throws(HostError) -> HostSnapshot {
        let signals = probe.signals
        let outcome: Result<HostSnapshot, HostError> = await withTaskGroup(of: Result<HostSnapshot, HostError>?.self) { group in
            group.addTask {
                for await signal in signals {
                    switch signal {
                    case let .snapshot(snapshot): return .success(snapshot)
                    case let .fatal(error): return .failure(error)
                    case .event, .stale: continue
                    }
                }
                return .failure(HostError(.unavailable, retryable: true, message: "connection closed"))
            }
            group.addTask {
                try? await Task.sleep(for: connectTimeout)
                return Task.isCancelled ? nil : .failure(HostError(.timeout, retryable: true, message: "no response from host"))
            }
            let first = await group.next() ?? nil
            group.cancelAll()
            return first ?? .failure(HostError(.cancelled))
        }
        return try outcome.get()
    }

    private static func displayHost(_ endpoint: String) -> String {
        guard let range = endpoint.range(of: "://") else { return endpoint }
        return String(endpoint[range.upperBound...])
    }
}

/// 没有可用连接时的占位会话：信号流只发 `.fatal`（store 显示失败态），命令一律抛同一个错误。
/// 用于本机引擎启动前 / 启动失败（如数据目录不可写）。
nonisolated final class UnavailableSession: HostSession {
    let error: HostError
    let signals: AsyncStream<HostSignal>

    init(_ error: HostError) {
        self.error = error
        signals = AsyncStream { continuation in
            continuation.yield(.fatal(error))
            continuation.finish()
        }
    }

    func close() {}

    func categorySaveDir(fileName: String, url: String) -> String? { nil }
    func createTask(_ request: CreateTaskRequest) async throws(HostError) -> String { throw error }
    func pause(_ taskId: String) async throws(HostError) { throw error }
    func resume(_ taskId: String) async throws(HostError) { throw error }
    func delete(_ taskId: String, deleteFiles: Bool) async throws(HostError) { throw error }
    func pauseMany(_ taskIds: [String]) async throws(HostError) { throw error }
    func resumeMany(_ taskIds: [String]) async throws(HostError) { throw error }
    func deleteMany(_ taskIds: [String], deleteFiles: Bool) async throws(HostError) { throw error }
    func pauseAll() async throws(HostError) { throw error }
    func resumeAll() async throws(HostError) { throw error }
    func rename(_ taskId: String, fileName: String) async throws(HostError) { throw error }
    func changeUrl(_ taskId: String, url: String) async throws(HostError) { throw error }
    func rescan() async throws(HostError) { throw error }
    func moveToQueue(_ taskId: String, queueId: String) async throws(HostError) { throw error }
    func boost(_ taskId: String) async throws(HostError) { throw error }
    func resolveSelection(_ requestId: String, outcome: SelectionOutcome) async throws(HostError) { throw error }
    func patchConfig(expectedRevision: UInt64, values: [String: String]) async throws(HostError) { throw error }
    func refreshRssSource(_ sourceId: String) async throws(HostError) { throw error }
    func setRssSourceEnabled(_ sourceId: String, enabled: Bool) async throws(HostError) { throw error }
    func call(_ method: String, params: Data?) async throws(HostError) -> Data { throw error }
}
