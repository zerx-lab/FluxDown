@testable import FluxBridge
import FluxDomain
import FluxRustBindings
import Testing

/// `HostSnapshotDto` 的任务表分页载入标志经 UniFFI DTO → 领域模型 → 状态仓库保真：
/// 加载态快照（空占位 + 总数）不得被当成完整列表。
struct SnapshotMappingTests {
    private func dto(tasksLoading: Bool, taskTotal: UInt64) -> HostSnapshotDto {
        HostSnapshotDto(
            info: HostInfoDto(serviceName: "t", serviceVersion: "1", protocolVersion: 17, capabilities: []),
            daemonConnected: true,
            tasksLoading: tasksLoading,
            taskTotal: taskTotal,
            tasks: [],
            runtime: [],
            queues: [],
            queueOrder: [],
            groups: [],
            stats: RuntimeStatsDto(
                activeTasks: 0, pendingTasks: 0, totalDownloadBps: 0, totalUploadBps: 0,
                diskFreeBytes: nil, saveDir: "", retryPendingTasks: 0
            ),
            priorityTaskId: nil,
            pendingSelections: [],
            config: [:],
            configRevision: 1,
            rssSources: [],
            cloudDevices: [],
            linkDevices: [],
            categories: [],
            sections: [:]
        )
    }

    @Test func loadingSnapshotKeepsTheFlagAndTotal() {
        let snapshot = dto(tasksLoading: true, taskTotal: 230_000).domain
        #expect(snapshot.tasksLoading)
        #expect(snapshot.taskTotal == 230_000)
        #expect(snapshot.tasks.isEmpty)
    }

    @Test func completeSnapshotIsNotLoading() {
        let snapshot = dto(tasksLoading: false, taskTotal: 0).domain
        #expect(!snapshot.tasksLoading)
        #expect(snapshot.taskTotal == 0)
    }

    @Test func categoryMappingCarriesUrlPattern() {
        let dto = CategoryDto(
            id: "site", name: "Site", icon: "globe", extensions: [], regexPattern: "",
            urlPattern: "https://*.example.com/*", position: 2, visible: true, builtinType: nil
        )
        let category = dto.domain
        #expect(category.urlPattern == "https://*.example.com/*")
        #expect(category.regexPattern.isEmpty)
        #expect(category.id == "site")
    }

    /// 任务级 BT 限速与超级做种逐字段落到领域模型（`native/mobile/src/dto.rs::TaskDto`）。
    @Test func taskMappingCarriesBtLimitsAndSuperSeeding() {
        let dto = TaskDto(
            taskId: "t", url: "magnet:?xt=urn:btih:x", originUrl: "", fileName: "f", saveDir: "/d",
            status: 3, downloadedBytes: 0, totalBytes: 0, errorMessage: "", createdAt: 0, completedAt: 0,
            queueId: "", groupId: "", rssSourceId: "", fileMissing: false, autoRoute: "",
            sourceCdn: 0, sourceProxy: 0, sourceNic: 0, uploadedBytes: 0, seedingStatus: 1,
            seedingMessage: "", seedingTimeSecs: 0, btSequential: true, btFirstLastPiecesFirst: false,
            btDownloadLimitBps: 3_000_000_000, seedUploadLimitBps: 51_200, btSuperSeeding: true
        )
        let task = dto.domain
        #expect(task.btSequential && !task.btFirstLastPiecesFirst)
        #expect(task.btDownloadLimitBps == 3_000_000_000)
        #expect(task.seedUploadLimitBps == 51_200)
        #expect(task.btSuperSeeding)
        var unlimited = dto
        unlimited.btDownloadLimitBps = 0
        unlimited.seedUploadLimitBps = 0
        unlimited.btSuperSeeding = false
        let plain = unlimited.domain
        #expect(plain.btDownloadLimitBps == 0 && plain.seedUploadLimitBps == 0 && !plain.btSuperSeeding)
    }

    @MainActor
    @Test func loadingFlagReachesTheStoreAndClearsOnTheCompleteSnapshot() {
        let store = HostStore()
        store.apply(.snapshot(dto(tasksLoading: true, taskTotal: 5).domain))
        #expect(store.state.tasksLoading && store.state.taskTotal == 5)
        store.apply(.snapshot(dto(tasksLoading: false, taskTotal: 5).domain))
        #expect(!store.state.tasksLoading && store.state.taskTotal == 0)
    }
}
