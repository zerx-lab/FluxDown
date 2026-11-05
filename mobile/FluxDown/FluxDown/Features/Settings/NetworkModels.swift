import FluxDomain
import Foundation
import Observation

// 网络与代理页的异步状态：系统代理检测 + 连通性测试（慢 RPC），站点凭据列表。
// 模型不持有会话：每次调用由视图传入当前 `HostSession`（切换主机后会话会变），并在 await 之后检查取消 / 主机，
// 避免旧主机的迟到结果覆盖新主机。

// MARK: - 代理检测与测试

@MainActor
@Observable
final class NetworkProxyModel {
    nonisolated enum Detection: Equatable {
        case idle
        case detecting
        case detected(SystemProxyDto)
        case failed(String)

        /// 已检测到的系统代理（仅 `detected == true`）。
        var system: SystemProxyDto? {
            if case let .detected(dto) = self, dto.detected { return dto }
            return nil
        }
    }

    nonisolated enum Test: Equatable {
        case idle
        case running
        case success(latencyMs: Int64)
        /// `detail` 已是可直接展示的原因（不含 `proxyTestFailed` 外壳）。
        case failure(detail: String)
    }

    private(set) var detection: Detection = .idle
    /// 手动分组与系统分组的测试结果互相独立：改一边的参数不会清掉另一边。
    private(set) var manualTest: Test = .idle
    private(set) var systemTest: Test = .idle
    /// 每次重置 / 新测试 +1：丢弃过期（请求参数已变或已切主机）的测试结果。两路各计各的。
    private var manualGeneration = 0
    private var systemGeneration = 0

    enum TestSource {
        case manual
        case system
    }

    /// `daemon.config.systemProxy`：进入系统代理模式 / 页面出现 / 重试时重新检测。
    func detect(using session: any HostSession) async {
        detection = .detecting
        do throws(HostError) {
            let dto: SystemProxyDto = try await session.call(HostMethod.daemonConfigSystemProxy)
            guard !Task.isCancelled else { return }
            detection = .detected(dto)
        } catch {
            guard !Task.isCancelled else { return }
            detection = .failed(ErrorText.describe(error))
        }
    }

    /// 该来源的参数或主机变化后，旧结果不再代表当前配置。另一来源不受影响。
    func resetTest(_ source: TestSource) {
        switch source {
        case .manual:
            guard manualTest != .idle else { return }
            manualGeneration += 1
            manualTest = .idle
        case .system:
            guard systemTest != .idle else { return }
            systemGeneration += 1
            systemTest = .idle
        }
    }

    func resetTests() {
        resetTest(.manual)
        resetTest(.system)
    }

    /// `daemon.config.proxyTest`（慢方法）：测试期间页面其余部分保持可交互。
    func runTest(_ request: ProxyTestRequest, source: TestSource, using session: any HostSession) async {
        let generation = beginTest(source)
        guard let generation else { return }
        let next = await perform(request, using: session)
        guard isCurrent(generation, source: source) else { return }
        finish(next, source: source)
    }

    /// 已在跑则返回 nil，调用方不再发第二次。
    private func beginTest(_ source: TestSource) -> Int? {
        switch source {
        case .manual:
            guard manualTest != .running else { return nil }
            manualGeneration += 1
            manualTest = .running
            return manualGeneration
        case .system:
            guard systemTest != .running else { return nil }
            systemGeneration += 1
            systemTest = .running
            return systemGeneration
        }
    }

    private func isCurrent(_ generation: Int, source: TestSource) -> Bool {
        switch source {
        case .manual: generation == manualGeneration
        case .system: generation == systemGeneration
        }
    }

    private func finish(_ next: Test, source: TestSource) {
        switch source {
        case .manual: manualTest = next
        case .system: systemTest = next
        }
    }

    private func perform(_ request: ProxyTestRequest, using session: any HostSession) async -> Test {
        do throws(HostError) {
            let response: ProxyTestResponse = try await session.call(HostMethod.daemonConfigProxyTest, params: request)
            return .success(latencyMs: response.latencyMs)
        } catch {
            return .failure(detail: NetworkProxyTest.failureDetail(
                message: error.message,
                fallback: ErrorText.describe(error),
                tlsHint: L("proxyTestTlsEndpointHint")
            ))
        }
    }
}

// MARK: - 站点凭据

@MainActor
@Observable
final class SiteAuthModel {
    nonisolated enum Phase: Equatable {
        case loading
        case loaded
        /// 首次加载失败（已有数据时的刷新失败保持 `.loaded`，不打断列表）。
        case failed(String)
    }

    private(set) var entries: [SiteAuthEntryDto] = []
    private(set) var phase: Phase = .loading
    private(set) var isClearing = false
    /// 正在删除的站点（行内禁用，避免重复请求）。
    private(set) var deleting: Set<String> = []
    private var loadedHost: String?

    /// `daemon.siteAuth.list`。切换主机先清空旧列表；已有数据时刷新失败静默保留旧数据。
    func load(using session: any HostSession, hostID: String) async {
        if loadedHost != hostID {
            loadedHost = hostID
            entries = []
            deleting = []
            phase = .loading
        } else if entries.isEmpty {
            phase = .loading
        }
        do throws(HostError) {
            let list: [SiteAuthEntryDto] = try await session.call(HostMethod.daemonSiteAuthList)
            guard !Task.isCancelled, loadedHost == hostID else { return }
            entries = list
            phase = .loaded
        } catch {
            guard !Task.isCancelled, loadedHost == hostID else { return }
            if phase != .loaded { phase = .failed(ErrorText.describe(error)) }
        }
    }

    /// `daemon.siteAuth.delete`：先乐观移除，成功后以服务端返回的新列表为准；失败则恢复并抛出。
    func delete(_ site: String, using session: any HostSession) async throws(HostError) {
        guard !deleting.contains(site) else { return }
        let before = entries
        deleting.insert(site)
        entries.removeAll { $0.site == site }
        defer { deleting.remove(site) }
        do throws(HostError) {
            let list: [SiteAuthEntryDto] = try await session.call(
                HostMethod.daemonSiteAuthDelete, params: SiteAuthDeleteParams(site: site)
            )
            entries = list
        } catch {
            entries = before
            throw error
        }
    }

    /// `daemon.siteAuth.clear`：返回清空后的列表。
    func clearAll(using session: any HostSession) async throws(HostError) {
        guard !isClearing else { return }
        isClearing = true
        defer { isClearing = false }
        entries = try await session.call(HostMethod.daemonSiteAuthClear)
    }

    /// 编辑表单回填：`daemon.siteAuth.get`（明文密码仅供表单，不进入列表状态）；站点已不存在 → nil。
    func credential(for site: String, using session: any HostSession) async throws(HostError) -> SiteAuthCredentialDto? {
        try await session.call(HostMethod.daemonSiteAuthGet, params: SiteAuthGetParams(site: site))
    }

    /// `daemon.siteAuth.save` 后重新拉取列表（保存结果只带脱敏条目，且站点可能被服务端归一化）。
    func save(_ request: SiteAuthSaveRequest, using session: any HostSession) async throws(HostError) {
        let _: SiteAuthEntryDto = try await session.call(HostMethod.daemonSiteAuthSave, params: request)
        do throws(HostError) {
            entries = try await session.call(HostMethod.daemonSiteAuthList)
            phase = .loaded
        } catch {
            // 保存已成功；列表刷新失败不应让表单报错——下次进入页面会重新加载。
            phase = entries.isEmpty ? .failed(ErrorText.describe(error)) : .loaded
        }
    }
}
