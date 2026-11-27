import FluxDomain
import Foundation

/// 列表型配置键写入（`WebhookEndpointWriter` / `ProxyRuleWriter`）的生产端口：读 `HostStore` 快照，写 `daemon.config.*`。
@MainActor
final class ContainerDaemonConfigPort: DaemonConfigPort {
    private let container: AppContainer?
    private let session: () -> any HostSession

    init(_ container: AppContainer) {
        self.container = container
        session = { container.session }
    }

    init(session: any HostSession) {
        container = nil
        self.session = { session }
    }

    var cachedConfig: DaemonConfigSnapshotDto? {
        guard let container else { return nil }
        let state = container.store.state
        guard !state.isReadOnly else { return nil }
        return DaemonConfigSnapshotDto(revision: state.configRevision, values: state.config)
    }

    func fetchConfig() async throws(HostError) -> DaemonConfigSnapshotDto {
        try await session().call(HostMethod.daemonConfigGet)
    }

    func patchConfig(expectedRevision: UInt64, values: [String: String]) async throws(HostError) {
        try await session().patchConfig(expectedRevision: expectedRevision, values: values)
    }
}
