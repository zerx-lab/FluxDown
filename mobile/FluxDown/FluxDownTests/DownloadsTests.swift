import FluxDomain
import FluxUI
import Foundation
import Testing
@testable import FluxDown

// 下载列表派生逻辑（筛选计数 / 智能排序 / 分组 / 分段投影 / 重排节流 / 行文案）。

// MARK: - 夹具

@MainActor private func makeTask(
    _ id: String,
    _ status: TaskStatus,
    name: String? = nil,
    url: String = "https://example.com/f.bin",
    downloaded: Int64 = 0,
    total: Int64 = 0,
    created: Int64 = 100,
    completedAt: Int64 = 0,
    queue: String = "",
    group: String = "",
    error: String = "",
    missing: Bool = false,
    seeding: SeedingStatus = .none
) -> DownloadTask {
    DownloadTask(
        taskId: id,
        url: url,
        fileName: name ?? "\(id).bin",
        status: status,
        downloadedBytes: downloaded,
        totalBytes: total,
        errorMessage: error,
        createdAt: created,
        completedAt: completedAt,
        queueId: queue,
        groupId: group,
        fileMissing: missing,
        seedingStatus: seeding
    )
}

@MainActor private func makeState(
    _ tasks: [DownloadTask],
    speeds: [String: Int64] = [:],
    upSpeeds: [String: Int64] = [:],
    queues: [TaskQueue] = [],
    groups: [DownloadGroup] = [],
    positions: [String: Int32] = [:],
    boosted: String? = nil,
    runtime: [String: TaskRuntime] = [:]
) -> HostState {
    var state = HostState()
    state.connection = .live
    state.tasks = tasks
    for (id, bps) in speeds { state.speeds[id] = LiveSpeed(down: bps, up: upSpeeds[id] ?? 0) }
    for (id, bps) in upSpeeds where state.speeds[id] == nil { state.speeds[id] = LiveSpeed(down: 0, up: bps) }
    state.queues = queues
    state.groups = groups
    state.queueOrder = QueueOrder(sorted: positions.sorted { $0.value < $1.value }.map {
        QueueOrderEntry(taskId: $0.key, order: Int64($0.value))
    })
    state.priorityTaskId = boosted
    state.runtime = runtime
    return state
}

@MainActor private let smart = ViewOrder(groupBy: .none, sortKey: .smart, ascending: false)

@MainActor private func derive(
    _ state: HostState,
    order: ViewOrder? = nil,
    filter: DownloadsFilter = DownloadsFilter(),
    collapsed: Set<String> = [],
    nowMs: Int64 = 1_000_000,
    calendar: Calendar = .current
) -> DeriveResult {
    var deriver = DownloadsDeriver()
    return deriver.derive(
        DeriveInput(state: state, order: order ?? smart, filter: filter, collapsed: collapsed),
        nowMs: nowMs,
        interactionMs: 0,
        calendar: calendar
    )
}

@MainActor private func ids(_ result: DeriveResult) -> [String] {
    result.list.sections.flatMap { $0.rows.map(\.id) }
}

@MainActor private func firstItem(_ result: DeriveResult) throws -> TaskItem {
    try #require(result.list.sections.first?.rows.first?.item)
}

/// 恒等查表：文案 = 键 + 排序后的参数，与系统语言无关。
@MainActor private let echo = RowText { key, args in
    key + args.sorted { $0.key < $1.key }.map { "|\($0.key)=\($0.value)" }.joined()
}

@MainActor private var utc: Calendar {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = .gmt
    return calendar
}

// MARK: - 筛选与计数

@MainActor
struct DownloadsFilterTests {
    private let tasks = [
        makeTask("a", .completed, name: "a.mp4"),
        makeTask("b", .downloading, name: "b.mp4", downloaded: 10, total: 100),
        makeTask("c", .completed, name: "c.mp3"),
        makeTask("d", .failed, name: "d.xyz"),
        makeTask("e", .paused, name: "e.pdf"),
        makeTask("f", .pending, name: "f.zip"),
        makeTask("g", .preparing, name: "g.iso"),
    ]

    @Test func folderCountsIgnoreCategoryAndActiveIncludesPendingAndPreparing() {
        let state = makeState(tasks)
        var filter = DownloadsFilter()
        filter.categoryId = "builtin_video"
        let result = derive(state, filter: filter)
        let facets = result.facets
        #expect(facets.count(.all) == 7)
        #expect(facets.count(.active) == 3)
        #expect(facets.count(.completed) == 2)
        #expect(facets.count(.failed) == 1)
        #expect(facets.count(.paused) == 1)
    }

    #if FULL_FLAVOR
    @Test func seedingFolderHoldsSeedingAndQueuedAndStaysInsideCompleted() {
        let state = makeState([
            makeTask("s", .completed, seeding: .seeding),
            makeTask("q", .completed, seeding: .queuedForSlot),
            makeTask("u", .completed, seeding: .userStopped),
            makeTask("c", .completed),
            makeTask("d", .downloading, seeding: .seeding),
        ])
        let facets = derive(state).facets
        #expect(facets.count(.seeding) == 2)
        #expect(facets.count(.completed) == 4)
        var filter = DownloadsFilter()
        filter.folder = .seeding
        #expect(Set(ids(derive(state, filter: filter))) == ["s", "q"])
        #expect(StatusFolder.allCases.firstIndex(of: .seeding) == StatusFolder.allCases.firstIndex(of: .completed).map { $0 + 1 })
        #expect(!StatusFolder.seeding.accepts(.pending as RemoteTaskStatus))
    }
    #endif

    @Test func categoryPillsCountOnlyCurrentFolder() {
        let state = makeState(tasks)
        var filter = DownloadsFilter()
        filter.folder = .completed
        let pills = derive(state, filter: filter).facets.categories
        #expect(pills.map(\.category.id) == ["builtin_video", "builtin_audio"])
        #expect(pills.map(\.count) == [1, 1])
    }

    @Test func folderAndCategoryIntersect() {
        let state = makeState(tasks)
        var filter = DownloadsFilter()
        filter.folder = .completed
        filter.categoryId = "builtin_video"
        let result = derive(state, filter: filter)
        #expect(ids(result) == ["a"])
        #expect(result.facets.matching == 1)
        #expect(result.list.taskTotal == 7)
    }

    /// 任务表分页载入中：派生结果必须声明「未载入」并带上服务端总数，空列表不是完整列表。
    @Test func tasksLoadingStateIsNotACompleteList() {
        var state = makeState([])
        state.tasksLoading = true
        state.taskTotal = 230_000
        let loading = derive(state).list
        #expect(!loading.loaded)
        #expect(loading.loadingTotal == 230_000)
        #expect(loading.visibleIds.isEmpty)

        // 载入完成：同一状态不再是加载态，总数清零。
        let done = derive(makeState(tasks)).list
        #expect(done.loaded)
        #expect(done.loadingTotal == 0)
        #expect(done.taskTotal == 7)
    }

    @Test func unknownCategoryFallsBackToAll() {
        var filter = DownloadsFilter()
        filter.categoryId = "gone"
        #expect(derive(makeState(tasks), filter: filter).facets.matching == 7)
    }

    @Test func queueScopeNormalizesMainQueue() {
        let state = makeState(
            [makeTask("a", .paused, queue: ""), makeTask("b", .paused, queue: "main"), makeTask("c", .paused, queue: "later")],
            queues: [TaskQueue(queueId: "main", name: "Main", position: 0), TaskQueue(queueId: "later", name: "Later", position: 1)]
        )
        var filter = DownloadsFilter()
        filter.queueId = "main"
        let result = derive(state, filter: filter)
        #expect(Set(ids(result)) == ["a", "b"])
        #expect(result.facets.queues.map(\.count) == [2, 1])
    }

    @Test func searchMatchesNameUrlAndSiteCaseInsensitivelyAndTrims() {
        let state = makeState([
            makeTask("a", .paused, name: "Ubuntu.iso", url: "https://mirror.example.org/x"),
            makeTask("b", .paused, name: "b.bin", url: "https://www.Cdn.Test:8080/path?q=1"),
            makeTask("c", .paused, name: "c.bin", url: "magnet:?xt=urn:btih:abc"),
        ])
        func found(_ query: String) -> Set<String> {
            var filter = DownloadsFilter()
            filter.query = query
            return Set(ids(derive(state, filter: filter)))
        }
        #expect(found("  UBUNTU ") == ["a"])
        #expect(found("cdn.test") == ["b"])
        #expect(found("btih") == ["c"])
        #expect(found("mirror") == ["a"])
        #expect(found("nothing").isEmpty)
        #expect(found("   ") == ["a", "b", "c"])
    }

    @Test func siteOfStripsSchemePortUserinfoAndWww() {
        func site(_ url: String, origin: String = "") -> String {
            DownloadsDeriver.site(of: makeTask("x", .paused, url: url).with(originUrl: origin))
        }
        #expect(site("https://www.example.com:8443/a/b?x=1") == "example.com")
        #expect(site("ftp://user:pw@files.example.net/pub") == "files.example.net")
        #expect(site("magnet:?xt=urn:btih:1") == "")
        #expect(site("torrent-file://local") == "")
        #expect(site("ed2k://|file|x|1|h|/") == "")
        #expect(site("https://a.com/f", origin: "https://origin.org/p") == "origin.org")
        #expect(site("not a url") == "")
    }
}

// MARK: - 远程任务行（与本机任务合并进同一列表）

@MainActor
struct DownloadsRemoteRowsTests {
    /// ISO 时间 → Unix 秒（与远程行 `createdAt` 同一解析）。
    private func at(_ iso: String) -> Int64 { DownloadsDeriver.createdSeconds(iso) }

    private var local: [DownloadTask] {
        [
            makeTask("a", .downloading, name: "a.mp4", total: 500, created: at("2026-10-08T01:30:00Z")),
            makeTask("b", .completed, name: "b.zip", total: 100, created: at("2026-10-08T02:30:00Z")),
            makeTask("f", .failed, name: "f.bin", created: at("2026-10-08T03:30:00Z")),
        ]
    }

    private let remote = [
        RemoteTaskDto(id: "r1", toDevice: "pc", url: "https://e.com/movie.mp4", status: .accepted, createdAt: "2026-10-08T01:00:00Z"),
        RemoteTaskDto(
            id: "r2", toDevice: "pc", url: "https://www.e.com:8443/x", fileName: "song.mp3", status: .downloading,
            totalBytes: 300, speed: 5000, createdAt: "2026-10-08T02:00:00.500Z"
        ),
        RemoteTaskDto(id: "r3", toDevice: "pc", fileName: "doc.pdf", status: .canceled, speed: 9000, createdAt: "2026-10-08T03:00:00Z"),
        RemoteTaskDto(id: "r4", toDevice: "pc", fileName: "app.zip", status: .paused, createdAt: "2026-10-08T04:00:00Z"),
    ]

    private func run(
        _ filter: DownloadsFilter = DownloadsFilter(),
        order: ViewOrder? = nil,
        speeds: [String: Int64] = ["a": 1000],
        state: HostState? = nil,
        remote override: [RemoteTaskDto]? = nil
    ) -> DeriveResult {
        var deriver = DownloadsDeriver()
        return deriver.derive(
            DeriveInput(
                state: state ?? makeState(local, speeds: speeds),
                order: order ?? smart,
                filter: filter,
                collapsed: [],
                remote: override ?? remote
            ),
            nowMs: 1_000_000,
            interactionMs: 0,
            calendar: utc
        )
    }

    private func order(_ key: SortKey, ascending: Bool, group: GroupBy = .none) -> ViewOrder {
        ViewOrder(groupBy: group, sortKey: key, ascending: ascending)
    }

    @Test func createdAtParsesWithAndWithoutFractionAndFailsToZero() {
        #expect(at("2026-10-08T01:00:00Z") == at("2026-10-08T01:00:00.750Z"))
        #expect(at("2026-10-08T01:00:00+08:00") == at("2026-10-07T17:00:00Z"))
        #expect(at("2026-10-08T01:00:00Z") > 1_700_000_000)
        #expect(at("") == 0)
        #expect(at("yesterday") == 0)
    }

    @Test func remoteTasksCountInFoldersWithGpuiStatusMapping() {
        let result = run()
        let facets = result.facets
        #expect(facets.count(.all) == 7)
        // 已接单 / 下载中 → 下载中；已取消 → 失败。
        #expect(facets.count(.active) == 3)
        #expect(facets.count(.failed) == 2)
        #expect(facets.count(.paused) == 1)
        #expect(facets.count(.completed) == 1)
        #expect(facets.remoteCount == 4)
        #expect(facets.matching == 7)
        #expect(result.list.taskTotal == 7)
    }

    @Test func smartOrderInterleavesLocalAndRemoteWithOneComparator() {
        let result = run()
        // 档 1：下载中（添加正序）a 01:30 → r2 02:00；档 2：r1；档 3 / 5 历史新 → 旧：f 03:30 → r3 03:00；档 4：r4；档 5：b。
        #expect(ids(result) == ["a", "remote:r2", "remote:r1", "f", "remote:r3", "remote:r4", "b"])
        #expect(result.list.sections.map(\.id) == ["flow", "history"])
        // 分区头计数含远程行；汇总速度只算本机（r2 的 5000 不计入）。
        #expect(result.list.sections.map(\.count) == [3, 4])
        #expect(result.list.sections[0].downSpeed == 1000)
        // 远程行不参与多选。
        #expect(result.list.visibleIds == ["a", "f", "b"])
    }

    @Test func remoteUnknownSortsWithPendingAndBothTiersSplitInflightFromHistory() {
        let remote = [
            RemoteTaskDto(id: "u", status: .unknown("z"), createdAt: "2026-10-08T00:30:00Z"),
            RemoteTaskDto(id: "d", status: .completed, createdAt: "2026-10-08T05:00:00Z"),
        ]
        let result = run(remote: remote)
        // 未知与等待同档（2）且属于「传输中」；已完成进历史（档 3 的 f 在档 5 的 d / b 之前）。
        #expect(ids(result) == ["a", "remote:u", "f", "remote:d", "b"])
        #expect(result.list.sections[0].rows.map(\.id) == ["a", "remote:u"])
        #expect(result.list.sections[1].rows.map(\.id) == ["f", "remote:d", "b"])
    }

    @Test func nameSortUsesRemoteDisplayName() {
        let result = run(order: order(.name, ascending: true))
        // r1 无文件名 → 由 URL 推断 movie.mp4；不分组时先排传输中（a · movie · song），再排历史。
        #expect(ids(result) == ["a", "remote:r1", "remote:r2", "remote:r4", "b", "remote:r3", "f"])
    }

    @Test func sizeSortUsesTotalBytesOrZeroWithNewestFirstTies() {
        let result = run(order: order(.size, ascending: false))
        // 不分组时仍按「传输中 / 历史」分区：传输中 a · r2 · r1，历史 b · r4 · f · r3（0 字节按创建时间新 → 旧）。
        #expect(ids(result) == ["a", "remote:r2", "remote:r1", "b", "remote:r4", "f", "remote:r3"])
    }

    @Test func speedSortZeroesTerminalRemoteSpeed() {
        let result = run(order: order(.speed, ascending: false))
        // r3 已取消：9000 不计，与其它 0 速度行按创建时间新 → 旧。
        #expect(ids(result) == ["remote:r2", "a", "remote:r1", "remote:r4", "f", "remote:r3", "b"])
    }

    @Test func progressSortTreatsUnknownRemoteAsNoProgress() {
        let remote = [
            RemoteTaskDto(id: "p", status: .downloading, progress: 0.4, createdAt: "2026-10-08T01:00:00Z"),
            RemoteTaskDto(id: "u", status: .unknown("z"), progress: 0.9, createdAt: "2026-10-08T02:00:00Z"),
        ]
        let state = makeState([makeTask("l", .downloading, downloaded: 50, total: 100, created: at("2026-10-08T03:00:00Z"))])
        let result = run(order: order(.progress, ascending: false), state: state, remote: remote)
        // l 0.5 > p 0.4 > u（无进度，-1）。
        #expect(ids(result) == ["l", "remote:p", "remote:u"])
    }

    @Test func reorderHoldIncludesRemoteIds() {
        var deriver = DownloadsDeriver()
        let key = order(.speed, ascending: false)
        func derive(_ remote: [RemoteTaskDto], nowMs: Int64) -> [String] {
            let result = deriver.derive(
                DeriveInput(state: makeState([]), order: key, filter: DownloadsFilter(), collapsed: [], remote: remote),
                nowMs: nowMs, interactionMs: 0
            )
            return result.list.sections.flatMap { $0.rows.map(\.id) }
        }
        let slow = RemoteTaskDto(id: "x", status: .downloading, speed: 10, createdAt: "2026-10-08T01:00:00Z")
        let fast = RemoteTaskDto(id: "y", status: .downloading, speed: 20, createdAt: "2026-10-08T02:00:00Z")
        #expect(derive([slow, fast], nowMs: 10_000) == ["remote:y", "remote:x"])
        // 2 秒内速度反超也不重排（远程行同样节流）。
        let overtaking = RemoteTaskDto(id: "x", status: .downloading, speed: 99, createdAt: "2026-10-08T01:00:00Z")
        #expect(derive([overtaking, fast], nowMs: 10_500) == ["remote:y", "remote:x"])
        #expect(derive([overtaking, fast], nowMs: 13_000) == ["remote:x", "remote:y"])
    }

    @Test func remoteOnlyKeepsOnlyRemoteRowsAndRemoteCountIgnoresCategory() {
        var filter = DownloadsFilter()
        filter.remoteOnly = true
        let result = run(filter)
        #expect(ids(result) == ["remote:r2", "remote:r1", "remote:r3", "remote:r4"])
        #expect(result.list.visibleIds.isEmpty)
        #expect(result.facets.matching == 4)
        #expect(result.facets.remoteCount == 4)
        #expect(!result.list.isEmpty)

        // 文件夹内的远程数；文件夹计数不受 remoteOnly 影响。
        filter.folder = .active
        let active = run(filter)
        #expect(ids(active) == ["remote:r2", "remote:r1"])
        #expect(active.facets.remoteCount == 2)
        #expect(active.facets.count(.all) == 7)
    }

    @Test func folderSearchAndCategoryFilterRemoteRowsLikeLocalRows() {
        var filter = DownloadsFilter()
        filter.folder = .active
        #expect(ids(run(filter)) == ["a", "remote:r2", "remote:r1"])
        // 文件名为空时按 URL 推断名称参与分类与搜索；选分类 = 本机 + 远程中属于该分类的，remoteCount 不受分类影响。
        filter.categoryId = "builtin_video"
        let video = run(filter)
        #expect(ids(video) == ["a", "remote:r1"])
        #expect(video.facets.categories.first { $0.id == "builtin_video" }?.count == 2)
        #expect(video.facets.remoteCount == 2)
        #expect(video.facets.matching == 2)
        filter = DownloadsFilter()
        filter.query = "SONG"
        let search = run(filter)
        #expect(ids(search) == ["remote:r2"])
        #expect(search.facets.remoteCount == 1)
    }

    @Test func queueScopeHidesRemoteRowsAndRemoteCount() {
        var filter = DownloadsFilter()
        filter.queueId = TaskQueue.main
        let scoped = run(filter)
        #expect(ids(scoped) == ["a", "f", "b"])
        #expect(scoped.facets.remoteCount == 0)
        #expect(scoped.facets.count(.all) == 3)

        filter.remoteOnly = true
        #expect(run(filter).list.isEmpty)
    }

    @Test func onlyRemoteTasksStillProduceSections() {
        let result = run(state: makeState([]))
        #expect(!result.list.isEmpty)
        #expect(result.list.sections.map(\.id) == ["flow", "history"])
        #expect(result.list.visibleIds.isEmpty)
    }

    @Test func groupByStatusPlacesRemoteByMapping() {
        let result = run(order: order(.smart, ascending: false, group: .status))
        let sections = Dictionary(uniqueKeysWithValues: result.list.sections.map { ($0.id, $0.rows.map(\.id)) })
        #expect(sections["status:1"] == ["a", "remote:r2"])
        #expect(sections["status:0"] == ["remote:r1"])
        #expect(sections["status:4"] == ["f", "remote:r3"])
        #expect(sections["status:2"] == ["remote:r4"])
        #expect(sections["status:3"] == ["b"])
        // 分组视图的 visibleIds 同样不含远程。
        #expect(result.list.visibleIds == ["a", "f", "b"])
    }

    @Test func groupByDateUsesRemoteCreatedAt() throws {
        let now = try #require(utc.date(from: DateComponents(year: 2026, month: 10, day: 8, hour: 12)))
        let nowMs = Int64(now.timeIntervalSince1970) * 1000
        var deriver = DownloadsDeriver()
        let remote = [
            RemoteTaskDto(id: "today", status: .paused, createdAt: "2026-10-08T08:00:00.250Z"),
            RemoteTaskDto(id: "old", status: .paused, createdAt: "2025-01-01T00:00:00Z"),
            RemoteTaskDto(id: "bad", status: .paused, createdAt: "garbage"),
        ]
        let result = deriver.derive(
            DeriveInput(
                state: makeState([]), order: order(.smart, ascending: false, group: .date),
                filter: DownloadsFilter(), collapsed: [], remote: remote
            ),
            nowMs: nowMs, interactionMs: 0, calendar: utc
        )
        #expect(result.list.sections.map(\.id) == ["date:0", "date:4"])
        #expect(result.list.sections[0].rows.map(\.id) == ["remote:today"])
        // 解析失败 = 0 → 最旧；历史档新 → 旧。
        #expect(result.list.sections[1].rows.map(\.id) == ["remote:old", "remote:bad"])
    }

    @Test func groupByQueueGivesRemoteItsOwnLastGroup() {
        let state = makeState(
            local.map { var task = $0; task.queueId = "main"; return task },
            queues: [TaskQueue(queueId: "main", name: "Main", position: 0)]
        )
        let result = run(order: order(.smart, ascending: false, group: .queue), state: state)
        #expect(result.list.sections.map(\.id) == ["queue:main", "queue:remote"])
        #expect(result.list.sections[1].title == .key("remoteTasksGroup"))
        #expect(result.list.sections[1].count == 4)
        #expect(result.list.sections[0].rows.map(\.id) == ["a", "f", "b"])
    }

    @Test func groupBySiteUsesRemoteUrlHost() {
        let remote = [
            RemoteTaskDto(id: "w", url: "https://www.e.com:8443/x", status: .paused, createdAt: "2026-10-08T01:00:00Z"),
            RemoteTaskDto(id: "m", url: "magnet:?xt=urn:btih:1", status: .paused, createdAt: "2026-10-08T02:00:00Z"),
        ]
        let state = makeState([makeTask("l", .paused, url: "https://e.com/f", created: at("2026-10-08T03:00:00Z"))])
        let result = run(order: order(.smart, ascending: false, group: .site), state: state, remote: remote)
        #if FULL_FLAVOR
        #expect(result.list.sections.map(\.id) == ["site:e.com", "site:"])
        #else
        #expect(result.list.sections.map(\.id) == ["site:e.com", "site:-"])
        #endif
        #expect(result.list.sections[0].rows.map(\.id) == ["l", "remote:w"])
        #expect(result.list.sections[1].rows.map(\.id) == ["remote:m"])
    }

    @Test func groupByTaskGroupFilesRemoteUnderUngrouped() {
        let state = makeState(
            [makeTask("g", .paused, created: at("2026-10-08T01:00:00Z"), group: "g1")],
            groups: [DownloadGroup(groupId: "g1", name: "G", sourceUrl: "", saveDir: "", createdAt: 0)]
        )
        let result = run(order: order(.smart, ascending: false, group: .group), state: state, remote: [remote[3]])
        #expect(result.list.sections.map(\.id) == ["group:g1", "group:none"])
        #expect(result.list.sections[0].doneOfTotal == DoneOfTotal(done: 0, total: 1))
        #expect(result.list.sections[1].rows.map(\.id) == ["remote:r4"])
    }

    @Test func groupByTypeUsesRemoteDisplayNameCategory() {
        let result = run(order: order(.smart, ascending: false, group: .type))
        let video = result.list.sections.first { $0.id == "type:builtin_video" }
        #expect(video?.rows.map(\.id) == ["a", "remote:r1"])
    }
}

/// 「远程任务」芯片与分类 / 文件夹的互斥规则（`DownloadsModel` 的筛选入口）。
@MainActor
struct DownloadsRemoteFilterStateTests {
    private func makeModel() -> DownloadsModel {
        DownloadsModel(defaults: UserDefaults(suiteName: "DownloadsRemoteFilterStateTests") ?? .standard)
    }

    @Test func remoteOnlyAndCategoryAreMutuallyExclusive() {
        let model = makeModel()
        model.setCategory("builtin_video")
        model.setRemoteOnly(true)
        #expect(model.filter.remoteOnly)
        #expect(model.filter.categoryId == nil)

        model.setCategory("builtin_audio")
        #expect(!model.filter.remoteOnly)
        #expect(model.filter.categoryId == "builtin_audio")

        model.setRemoteOnly(false)
        #expect(model.filter.categoryId == "builtin_audio")
    }

    @Test func switchingFolderClearsRemoteOnlyEvenForTheSameFolder() {
        let model = makeModel()
        model.setRemoteOnly(true)
        model.setFolder(.all)
        #expect(!model.filter.remoteOnly)

        model.setRemoteOnly(true)
        model.setFolder(.failed)
        #expect(!model.filter.remoteOnly)
        #expect(model.filter.folder == .failed)
    }

    @Test func clearFiltersResetsRemoteOnly() {
        let model = makeModel()
        model.setRemoteOnly(true)
        model.clearFilters()
        #expect(model.filter == DownloadsFilter())
    }
}

private extension DownloadTask {
    func with(originUrl: String) -> DownloadTask {
        var copy = self
        copy.originUrl = originUrl
        return copy
    }
}

// MARK: - 智能排序与排序键

@MainActor
struct DownloadsSortTests {
    @Test func smartTiersBoostActiveQueuedFailedPausedCompleted() {
        let state = makeState(
            [
                makeTask("done-old", .completed, created: 10),
                makeTask("done-new", .completed, created: 60),
                makeTask("paused", .paused, created: 40),
                makeTask("failed", .failed, created: 30),
                makeTask("queued", .pending, created: 20),
                makeTask("dl2", .downloading, created: 12),
                makeTask("dl1", .downloading, created: 5),
                makeTask("prep", .preparing, created: 8),
                makeTask("boost", .pending, created: 99),
            ],
            boosted: "boost"
        )
        // 优先下载 → 活跃（下载中 / 准备中，按添加顺序正序）→ 排队 → 失败 → 暂停 → 完成（新 → 旧）
        #expect(ids(derive(state)) == ["boost", "dl1", "prep", "dl2", "queued", "failed", "paused", "done-new", "done-old"])
    }

    @Test func boostOnlyCountsWhileDownloadingOrPending() {
        let state = makeState(
            [makeTask("paused", .paused, created: 1), makeTask("dl", .downloading, created: 2)],
            boosted: "paused"
        )
        #expect(ids(derive(state)) == ["dl", "paused"])
    }

    @Test func smartIgnoresDirection() {
        let tasks = [makeTask("a", .paused, created: 1), makeTask("b", .completed, created: 2)]
        let asc = derive(makeState(tasks), order: ViewOrder(groupBy: .none, sortKey: .smart, ascending: true))
        #expect(ids(asc) == ["a", "b"])
        #expect(ids(asc) == ids(derive(makeState(tasks))))
    }

    @Test func nameIsCaseInsensitiveAndRespectsDirection() {
        let state = makeState([
            makeTask("1", .paused, name: "banana"),
            makeTask("2", .paused, name: "Apple"),
            makeTask("3", .paused, name: "cherry"),
        ])
        #expect(ids(derive(state, order: ViewOrder(groupBy: .none, sortKey: .name, ascending: true))) == ["2", "1", "3"])
        #expect(ids(derive(state, order: ViewOrder(groupBy: .none, sortKey: .name, ascending: false))) == ["3", "1", "2"])
    }

    @Test func sizeProgressSpeedAndTieBreakNewestFirst() {
        let state = makeState(
            [
                makeTask("small", .paused, downloaded: 50, total: 100, created: 1),
                makeTask("big", .paused, downloaded: 10, total: 1000, created: 2),
                makeTask("unknown", .paused, downloaded: 0, total: 0, created: 3),
                makeTask("tie", .paused, downloaded: 50, total: 100, created: 9),
            ],
            speeds: ["small": 5, "big": 50]
        )
        #expect(ids(derive(state, order: ViewOrder(groupBy: .none, sortKey: .size, ascending: false))) == ["big", "tie", "small", "unknown"])
        // 进度：未知大小（nil → -1）排最后；并列按创建时间新 → 旧
        #expect(ids(derive(state, order: ViewOrder(groupBy: .none, sortKey: .progress, ascending: false))) == ["tie", "small", "big", "unknown"])
        #expect(ids(derive(state, order: ViewOrder(groupBy: .none, sortKey: .speed, ascending: false))).prefix(2) == ["big", "small"])
        #expect(ids(derive(state, order: ViewOrder(groupBy: .none, sortKey: .created, ascending: true))) == ["small", "big", "unknown", "tie"])
    }

    @Test func statusKeyUsesSmartTier() {
        let state = makeState([makeTask("p", .paused), makeTask("d", .downloading), makeTask("f", .failed)])
        let asc = derive(state, order: ViewOrder(groupBy: .none, sortKey: .status, ascending: true))
        #expect(ids(asc) == ["d", "f", "p"])
    }
}

// MARK: - 重排节流

@MainActor
struct DownloadsReorderHoldTests {
    private let order = ViewOrder(groupBy: .none, sortKey: .speed, ascending: false)

    private func state(a: Int64, b: Int64) -> HostState {
        makeState(
            [makeTask("a", .downloading, created: 1), makeTask("b", .downloading, created: 2)],
            speeds: ["a": a, "b": b]
        )
    }

    private func run(_ deriver: inout DownloadsDeriver, _ state: HostState, now: Int64, touch: Int64 = 0) -> DeriveResult {
        deriver.derive(DeriveInput(state: state, order: order, filter: DownloadsFilter(), collapsed: []), nowMs: now, interactionMs: touch)
    }

    @Test func dynamicKeyHoldsOrderForTwoSecondsThenReorders() {
        var deriver = DownloadsDeriver()
        let first = run(&deriver, state(a: 10, b: 20), now: 1_000_000)
        #expect(ids(first) == ["b", "a"])
        #expect(first.retryAtMs == 0)

        let held = run(&deriver, state(a: 30, b: 20), now: 1_000_500)
        #expect(ids(held) == ["b", "a"])
        #expect(held.retryAtMs == 1_002_000)

        let released = run(&deriver, state(a: 30, b: 20), now: 1_002_500)
        #expect(ids(released) == ["a", "b"])
        #expect(released.retryAtMs == 0)
    }

    @Test func recentInteractionDelaysReorder() {
        var deriver = DownloadsDeriver()
        _ = run(&deriver, state(a: 10, b: 20), now: 1_000_000)
        let held = run(&deriver, state(a: 30, b: 20), now: 1_002_500, touch: 1_002_400)
        #expect(ids(held) == ["b", "a"])
        #expect(held.retryAtMs == 1_003_900)
        let released = run(&deriver, state(a: 30, b: 20), now: 1_004_000, touch: 1_002_400)
        #expect(ids(released) == ["a", "b"])
    }

    @Test func nonDynamicKeysAlwaysReorderImmediately() {
        var deriver = DownloadsDeriver()
        let created = ViewOrder(groupBy: .none, sortKey: .created, ascending: true)
        func go(_ created1: Int64, _ created2: Int64, now: Int64) -> [String] {
            let state = makeState([makeTask("a", .paused, created: created1), makeTask("b", .paused, created: created2)])
            return ids(deriver.derive(DeriveInput(state: state, order: created, filter: DownloadsFilter(), collapsed: []), nowMs: now, interactionMs: 0))
        }
        #expect(go(1, 2, now: 1_000_000) == ["a", "b"])
        #expect(go(3, 2, now: 1_000_010) == ["b", "a"])
    }

    @Test func changingFilterOrOrderResetsTheHold() {
        var deriver = DownloadsDeriver()
        _ = run(&deriver, state(a: 10, b: 20), now: 1_000_000)
        var filter = DownloadsFilter()
        filter.query = "a.bin"
        let result = deriver.derive(
            DeriveInput(state: state(a: 30, b: 20), order: order, filter: filter, collapsed: []),
            nowMs: 1_000_100, interactionMs: 0
        )
        #expect(ids(result) == ["a"])
        #expect(result.retryAtMs == 0)
    }
}

// MARK: - 分区与分组

@MainActor
struct DownloadsGroupingTests {
    @Test func ungroupedSplitsInFlightAndHistory() throws {
        let state = makeState(
            [makeTask("dl", .downloading, created: 1), makeTask("done", .completed, created: 2), makeTask("q", .pending, created: 3)],
            speeds: ["dl": 1000]
        )
        let sections = derive(state).list.sections
        #expect(sections.map(\.id) == ["flow", "history"])
        #expect(sections[0].kind == .inFlight)
        #expect(sections[0].downSpeed == 1000)
        #expect(sections[0].rows.map(\.id) == ["dl", "q"])
        #expect(sections[1].title == .history(nil))
    }

    @Test func historyTitleNamesTheSelectedFolder() throws {
        let state = makeState([makeTask("f", .failed), makeTask("d", .completed)])
        var filter = DownloadsFilter()
        filter.folder = .failed
        let section = try #require(derive(state, filter: filter).list.sections.first)
        #expect(section.title == .history(.failed))
    }

    @Test func groupByStatusOrderAndMembership() {
        let state = makeState([
            makeTask("done", .completed), makeTask("pause", .paused), makeTask("fail", .failed),
            makeTask("prep", .preparing), makeTask("pend", .pending), makeTask("dl", .downloading),
        ])
        let sections = derive(state, order: ViewOrder(groupBy: .status, sortKey: .smart, ascending: false)).list.sections
        #expect(sections.map(\.id) == ["status:1", "status:0", "status:4", "status:2", "status:3"])
        #expect(Set(sections[1].rows.map(\.id)) == ["prep", "pend"])
        #expect(sections.allSatisfy { $0.kind == .group })
    }

    @Test func groupByDateBucketsRelativeToToday() throws {
        let calendar = utc
        let now = try #require(calendar.date(from: DateComponents(year: 2026, month: 10, day: 5, hour: 12)))
        let nowSec = Int64(now.timeIntervalSince1970)
        let day: Int64 = 86_400
        let state = makeState([
            makeTask("today", .paused, created: nowSec - 3600),
            makeTask("yesterday", .paused, created: nowSec - day),
            makeTask("week", .paused, created: nowSec - 4 * day),
            makeTask("month", .paused, created: nowSec - 20 * day),
            makeTask("old", .paused, created: nowSec - 90 * day),
        ])
        let sections = derive(state, order: ViewOrder(groupBy: .date, sortKey: .smart, ascending: false), nowMs: nowSec * 1000, calendar: calendar).list.sections
        #expect(sections.map(\.id) == ["date:0", "date:1", "date:2", "date:3", "date:4"])
        #expect(sections.map(\.title) == [.key("today"), .key("yesterday"), .key("thisWeek"), .key("thisMonth"), .key("older")])
        #expect(sections.flatMap { $0.rows.map(\.id) } == ["today", "yesterday", "week", "month", "old"])
    }

    @Test func groupByTypeFollowsCategoryOrderAndOtherLast() {
        let state = makeState([
            makeTask("z", .paused, name: "a.zip"), makeTask("v", .paused, name: "a.mp4"),
            makeTask("o", .paused, name: "a.unknownext"), makeTask("m", .paused, name: "a.mp3"),
        ])
        let sections = derive(state, order: ViewOrder(groupBy: .type, sortKey: .smart, ascending: false)).list.sections
        #expect(sections.map(\.id) == ["type:builtin_video", "type:builtin_audio", "type:builtin_archive", "type:builtin_other"])
    }

    @Test func groupByQueueOrdersByPositionAndMergesImplicitMain() {
        let state = makeState(
            [makeTask("a", .paused, queue: "later"), makeTask("b", .paused, queue: ""), makeTask("c", .paused, queue: "ghost")],
            queues: [TaskQueue(queueId: "later", name: "Later", position: 1), TaskQueue(queueId: "main", name: "Main", position: 0)]
        )
        let sections = derive(state, order: ViewOrder(groupBy: .queue, sortKey: .smart, ascending: false)).list.sections
        #expect(sections.map(\.id) == ["queue:main", "queue:later", "queue:ghost"])
        #expect(sections[2].title == .text("ghost"))
        #expect(sections[0].rows.map(\.id) == ["b"])
    }

    @Test func groupBySiteSortsHostsAndSplitsHostless() {
        let state = makeState([
            makeTask("b", .paused, url: "https://b.example.com/x"),
            makeTask("a", .paused, url: "https://A.example.com/x"),
            makeTask("bt", .paused, url: "magnet:?xt=urn:btih:1"),
            makeTask("ed", .paused, url: "ed2k://|file|x|1|h|/"),
        ])
        let sections = derive(state, order: ViewOrder(groupBy: .site, sortKey: .smart, ascending: false)).list.sections
        #if FULL_FLAVOR
        #expect(sections.map(\.id) == ["site:A.example.com", "site:b.example.com", "site:", "site:-"])
        #expect(sections[2].title == .key("viewSiteBt"))
        #expect(sections[3].title == .text(Format.dash))
        #else
        // store 不含 P2P：无 host 的磁力 / eD2K 行一律归「—」。
        #expect(sections.map(\.id) == ["site:A.example.com", "site:b.example.com", "site:-"])
        #expect(sections[2].title == .text(Format.dash))
        #expect(sections[2].rows.map(\.id).sorted() == ["bt", "ed"])
        #endif
    }

    @Test func groupByTaskGroupShowsDoneOfTotalAndUngroupedLast() {
        let state = makeState(
            [
                makeTask("a", .completed, group: "g2"), makeTask("b", .downloading, group: "g2"),
                makeTask("c", .paused, group: "g1"), makeTask("d", .paused, group: ""), makeTask("e", .paused, group: "deleted"),
            ],
            groups: [
                DownloadGroup(groupId: "g2", name: "Zeta", sourceUrl: "", saveDir: "", createdAt: 0),
                DownloadGroup(groupId: "g1", name: "alpha", sourceUrl: "", saveDir: "", createdAt: 0),
            ]
        )
        let sections = derive(state, order: ViewOrder(groupBy: .group, sortKey: .smart, ascending: false)).list.sections
        #expect(sections.map(\.id) == ["group:g1", "group:g2", "group:none"])
        #expect(sections[1].doneOfTotal == DoneOfTotal(done: 1, total: 2))
        #expect(sections[2].title == .key("ungroupedTasks"))
        #expect(Set(sections[2].rows.map(\.id)) == ["d", "e"])
    }

    @Test func collapsedGroupKeepsCountButDropsRowsAndVisibleIds() {
        let state = makeState([makeTask("a", .paused), makeTask("b", .failed)])
        let order = ViewOrder(groupBy: .status, sortKey: .smart, ascending: false)
        let result = derive(state, order: order, collapsed: ["status:2"])
        let paused = result.list.sections.first { $0.id == "status:2" }
        #expect(paused?.collapsed == true)
        #expect(paused?.count == 1)
        #expect(paused?.rows.isEmpty == true)
        #expect(result.list.visibleIds == ["b"])
    }

    @Test func dynamicSectionTitlesResolveToExistingKeys() {
        let english = L10n(locale: "en")
        let keys = [
            "today", "yesterday", "thisWeek", "thisMonth", "older", "categoryOther", "viewSiteBt", "ungroupedTasks",
            "statusDownloading", "statusPending", "statusError", "statusPaused", "statusCompleted",
            "btFileSelectTitle", "hlsQualityTitle", "resolveVariantTitle", "mobileExpand", "mobileCollapse",
            "mainQueue", "downloadLater", "queueRunningBadge", "queueStoppedBadge", "remoteTasksGroup",
        ]
        for key in keys { #expect(english.has(key), "missing i18n key \(key)") }
    }
}

// MARK: - 行模型：分段投影 / 圆环 / 状态

@MainActor
struct DownloadsRowModelTests {
    private func runtime(_ id: String, _ segments: [Segment]) -> TaskRuntime {
        TaskRuntime(taskId: id, sampleSequence: 1, activeTransfers: 2, connectedPeers: 5, totalBytes: 1000, segments: segments)
    }

    @Test func segmentsProjectToRealByteRangesWithExclusiveEnd() throws {
        let task = makeTask("a", .downloading, downloaded: 400, total: 1000)
        let segments = [
            Segment(index: 0, startByte: 0, endByte: 499, downloadedBytes: 400, active: true),
            Segment(index: 1, startByte: 500, endByte: 999, downloadedBytes: 0, active: nil),
        ]
        let item = try firstItem(derive(makeState([task], speeds: ["a": 100], runtime: ["a": runtime("a", segments)])))
        let bar = try #require(item.progressBar)
        #expect(bar.tone == .downloading)
        #expect(bar.totalBytes == 1000)
        #expect(bar.spans == [
            SegmentSpan(startByte: 0, endByte: 500, downloadedBytes: 400, active: true),
            SegmentSpan(startByte: 500, endByte: 1000, downloadedBytes: 0, active: nil),
        ])
        #expect(item.activeTransfers == 2)
        #expect(item.connectedPeers == 5)
    }

    @Test func pausedSegmentsAreNeverActive() throws {
        let task = makeTask("a", .paused, downloaded: 400, total: 1000)
        let segments = [Segment(index: 0, startByte: 0, endByte: 999, downloadedBytes: 400, active: true)]
        let item = try firstItem(derive(makeState([task], runtime: ["a": runtime("a", segments)])))
        let bar = try #require(item.progressBar)
        #expect(bar.tone == .paused)
        #expect(bar.spans.map(\.active) == [false])
    }

    @Test func withoutSegmentsFallsBackToTotalProgress() throws {
        let item = try firstItem(derive(makeState([makeTask("a", .failed, downloaded: 250, total: 1000)])))
        let bar = try #require(item.progressBar)
        #expect(bar.spans.isEmpty)
        #expect(bar.totalBytes == 0)
        #expect(bar.progress == 0.25)
        #expect(bar.tone == .failed)
    }

    @Test func malformedSegmentsAreDropped() throws {
        let segments = [
            Segment(index: 0, startByte: 600, endByte: 100, downloadedBytes: 0, active: false),
            Segment(index: 1, startByte: 0, endByte: 99, downloadedBytes: 10, active: false),
        ]
        let task = makeTask("a", .paused, downloaded: 10, total: 1000)
        let item = try firstItem(derive(makeState([task], runtime: ["a": runtime("a", segments)])))
        #expect(try #require(item.progressBar).spans.count == 1)
    }

    @Test func barVisibilityPerState() throws {
        func bar(_ task: DownloadTask, queued: Bool = false) throws -> ProgressBarModel? {
            try firstItem(derive(makeState([task], positions: queued ? [task.taskId: 2] : [:]))).progressBar
        }
        #expect(try bar(makeTask("c", .completed, downloaded: 10, total: 10)) == nil)
        #expect(try bar(makeTask("m", .completed, total: 10, missing: true)) == nil)
        #expect(try bar(makeTask("s", .completed, total: 10, seeding: .seeding)) == nil)
        #expect(try bar(makeTask("q", .pending)) == nil)
        #expect(try bar(makeTask("q2", .pending, downloaded: 5, total: 10), queued: true)?.tone == .queued)
        // 准备中（总大小未知）= 不定进度；校验中（总大小已知）= 细进度条
        let preparing = try #require(try bar(makeTask("p", .preparing)))
        #expect(preparing.progress == nil)
        #expect(preparing.tone == .downloading)
        #expect(try bar(makeTask("v", .preparing, downloaded: 5, total: 10))?.progress == 0.5)
        // 下载中且总大小未知 → 不定进度
        #expect(try bar(makeTask("d", .downloading))?.progress == nil)
    }

    /// 队首出队只删一个键：其余排队行的稳定键不变 → 行模型逐个相等（不重建）；名次在渲染时按键二分得到。
    @Test func dequeuingTheHeadKeepsOtherQueuedRowsEqualAndRanksShift() throws {
        let tasks = ["a", "b", "c"].map { makeTask($0, .pending) }
        var deriver = DownloadsDeriver()
        func items(_ state: HostState) -> [String: TaskItem] {
            let result = deriver.derive(
                DeriveInput(state: state, order: smart, filter: DownloadsFilter(), collapsed: []),
                nowMs: 1_000_000,
                interactionMs: 0
            )
            return Dictionary(uniqueKeysWithValues: result.list.sections.flatMap { $0.rows.compactMap(\.item) }.map { ($0.id, $0) })
        }
        var state = makeState(tasks)
        state.queueOrder = QueueOrder(sorted: [("a", 10), ("b", 20), ("c", 30)].map { QueueOrderEntry(taskId: $0.0, order: $0.1) })
        let before = items(state)
        #expect(state.queueOrder.rank(of: "b") == 2)

        state.queueOrder = QueueOrder(sorted: state.queueOrder.entries.filter { $0.taskId != "a" })
        let after = items(state)
        #expect(after["b"] == before["b"] && after["c"] == before["c"])
        #expect(after["b"]?.visual == .queued)
        #expect(state.queueOrder.rank(of: "b") == 1 && state.queueOrder.rank(of: "c") == 2)
        #expect(state.queueOrder.rank(of: "a") == 0)
    }

    @Test func visualStateAndRingMapping() throws {
        func item(_ task: DownloadTask, queued: Bool = false) throws -> TaskItem {
            try firstItem(derive(makeState([task], positions: queued ? [task.taskId: 3] : [:])))
        }
        #expect(try item(makeTask("a", .downloading, downloaded: 1, total: 10)).visual == .downloading)
        #expect(try item(makeTask("a", .pending), queued: true).visual == .queued)
        #expect(try item(makeTask("a", .pending)).visual == .pending)
        #expect(try item(makeTask("a", .preparing)).visual == .preparing)
        #expect(try item(makeTask("a", .preparing, total: 10)).visual == .verifying)
        #expect(try item(makeTask("a", .completed, missing: true)).visual == .missing)
        #if FULL_FLAVOR
        #expect(try item(makeTask("a", .completed, seeding: .seeding)).visual == .seeding)
        #else
        // store 不含做种：做种任务按已完成呈现。
        #expect(try item(makeTask("a", .completed, seeding: .seeding)).visual == .completed)
        #endif

        #expect(try item(makeTask("a", .downloading)).ring == .pause)
        #expect(try item(makeTask("a", .preparing)).ring == .preparing)
        #expect(try item(makeTask("a", .paused)).ring == .resume)
        #expect(try item(makeTask("a", .failed)).ring == .retry)
        #expect(try item(makeTask("a", .completed)).ring == .open)
        #expect(try item(makeTask("a", .completed, missing: true)).ring == .redownload)
        #expect(try item(makeTask("a", .unknown)).ring == nil)
        #expect(try item(makeTask("a", .completed)).ringProgress == nil)
        #expect(try item(makeTask("a", .paused, downloaded: 5, total: 10)).ringProgress == 0.5)
    }

    @Test func etaOnlyWhileDownloadingWithKnownSpeedAndSize() throws {
        let task = makeTask("a", .downloading, downloaded: 100, total: 1100)
        #expect(try firstItem(derive(makeState([task], speeds: ["a": 100]))).eta == 10)
        #expect(try firstItem(derive(makeState([task]))).eta == nil)
        #expect(try firstItem(derive(makeState([makeTask("b", .paused, downloaded: 1, total: 100)], speeds: ["b": 10]))).eta == nil)
    }

    @Test func unchangedInputsReuseTheSameItemValue() throws {
        var deriver = DownloadsDeriver()
        let state = makeState([makeTask("a", .paused, downloaded: 1, total: 2)])
        let input = DeriveInput(state: state, order: smart, filter: DownloadsFilter(), collapsed: [])
        let first = deriver.derive(input, nowMs: 1, interactionMs: 0)
        let second = deriver.derive(input, nowMs: 2, interactionMs: 0)
        #expect(first.list == second.list)
    }

    @Test func btMagnetWithUnknownExtensionUsesTorrentIcon() throws {
        let item = try firstItem(derive(makeState([makeTask("a", .paused, name: "linux", url: "magnet:?xt=urn:btih:1")])))
        #expect(item.kind == .torrent)
    }
}

// MARK: - 行文案

@MainActor
struct DownloadsRowTextTests {
    private let allFields = Set(CardField.allCases)

    private func item(_ task: DownloadTask, state: (inout HostState) -> Void = { _ in }) throws -> TaskItem {
        var s = makeState([task])
        state(&s)
        return try firstItem(derive(s))
    }

    @Test func etaFormatting() {
        #expect(echo.eta(45) == "etaSeconds|n=45")
        #expect(echo.eta(60) == "etaMinutes|n=1")
        #expect(echo.eta(61) == "etaMinutes|n=2")
        #expect(echo.eta(3599) == "etaMinutes|n=60")
        #expect(echo.eta(3600) == "etaHours|n=1")
        #expect(echo.eta(3 * 3600 + 20 * 60) == "etaHours|n=3 etaMinutes|n=20")
        // 分钟向上取整到 60 → 进位到小时
        #expect(echo.eta(3600 + 3599) == "etaHours|n=2")
    }

    @Test func relativeTimeBuckets() {
        let now: Int64 = 1_000_000
        #expect(echo.relative(now - 5, now: now) == "mobileTimeJustNow")
        #expect(echo.relative(now - 300, now: now) == "mobileTimeMinutesAgo|n=5")
        #expect(echo.relative(now - 7200, now: now) == "mobileTimeHoursAgo|n=2")
        #expect(echo.relative(now - 3 * 86_400, now: now) == "mobileTimeDaysAgo|n=3")
        #expect(echo.relative(now + 99, now: now) == "mobileTimeJustNow")
    }

    @Test func percentFloorsAndShowsLessThanOne() throws {
        func percent(_ downloaded: Int64, total: Int64 = 1000, status: TaskStatus = .downloading) throws -> String? {
            echo.percent(try item(makeTask("a", status, downloaded: downloaded, total: total)))
        }
        #expect(try percent(425) == "42%")
        #expect(try percent(999) == "99%")
        #expect(try percent(1000) == "100%")
        #expect(try percent(4) == "<1%")
        #expect(try percent(0) == "0%")
        #expect(try percent(10, total: 0) == nil)
        #expect(try percent(500, status: .completed) == nil)
        #expect(try percent(0, status: .pending) == nil)
    }

    @Test func downloadingStatusLineHonoursSpeedAndEtaFields() throws {
        let task = makeTask("a", .downloading, downloaded: 100, total: 1100)
        let downloading = try item(task) { $0.speeds["a"] = LiveSpeed(down: 100, up: 0) }
        let both = echo.status(downloading, fields: [.speed, .eta])
        #expect(both.tone == .accent)
        #expect(both.text == "\(try #require(Format.speed(100)).description) · groupEtaRemaining|eta=etaSeconds|n=10")
        #expect(echo.status(downloading, fields: [.eta]).text == "groupEtaRemaining|eta=etaSeconds|n=10")
        // 速度 / 剩余时间都不可用 → 「下载中」
        let stalled = try item(task)
        #expect(echo.status(stalled, fields: [.speed, .eta]).text == "statusDownloading")
        #expect(echo.status(downloading, fields: [.size]).text == "statusDownloading")
    }

    @Test func stateStatusLines() throws {
        func line(_ task: DownloadTask, queued: Int = 0) throws -> StatusLine {
            let built = try item(task) {
                guard queued > 0 else { return }
                let fillers = (1..<queued).map { QueueOrderEntry(taskId: "f\($0)", order: Int64($0)) }
                $0.queueOrder = QueueOrder(sorted: fillers + [QueueOrderEntry(taskId: task.taskId, order: Int64(queued))])
            }
            return echo.status(built, fields: allFields, queuePosition: queued)
        }
        #expect(try line(makeTask("a", .pending), queued: 3) == StatusLine(text: "subtitleQueued|pos=3", tone: .secondary))
        #expect(try line(makeTask("a", .pending)) == StatusLine(text: "statusPending", tone: .secondary))
        #expect(try line(makeTask("a", .preparing)) == StatusLine(text: "subtitlePreparing", tone: .secondary))
        #expect(try line(makeTask("a", .preparing, total: 10)) == StatusLine(text: "statusVerifying", tone: .secondary))
        #expect(try line(makeTask("a", .paused)) == StatusLine(text: "statusPaused", tone: .secondary))
        #expect(try line(makeTask("a", .completed)) == StatusLine(text: "statusCompleted", tone: .secondary))
        #expect(try line(makeTask("a", .completed, missing: true)) == StatusLine(text: "statusFileMissing", tone: .warning))
        #expect(try line(makeTask("a", .failed, error: "\n  connection reset \nsecond line")) == StatusLine(text: "statusError · connection reset", tone: .failure))
        #expect(try line(makeTask("a", .failed)) == StatusLine(text: "statusError", tone: .failure))
    }

    #if FULL_FLAVOR
    @Test func seedingShowsUploadSpeed() throws {
        let seeding = try item(makeTask("a", .completed, seeding: .seeding)) { $0.speeds["a"] = LiveSpeed(down: 0, up: 640 * 1024) }
        let line = echo.status(seeding, fields: allFields)
        #expect(line.tone == .success)
        #expect(line.text == "statusSeeding · ↑ \(try #require(Format.speed(640 * 1024)).description)")
    }

    @Test func queuedSeedingShowsQueuedLabelAndStoppedSeedKeepsCompleted() throws {
        let queued = try item(makeTask("q", .completed, seeding: .queuedForSlot))
        #expect(queued.visual == .seeding)
        #expect(echo.status(queued, fields: allFields) == StatusLine(text: "seedingStatusQueued", tone: .success))
        let stopped = try item(makeTask("s", .completed, seeding: .userStopped))
        #expect(stopped.visual == .completed)
        #expect(echo.status(stopped, fields: allFields) == StatusLine(text: "statusCompleted", tone: .secondary))
    }
    #else
    @Test func seedingTasksReadAsCompletedInStoreBuilds() throws {
        let seeding = try item(makeTask("a", .completed, seeding: .seeding)) { $0.speeds["a"] = LiveSpeed(down: 0, up: 640 * 1024) }
        #expect(seeding.visual == .completed)
        #expect(echo.status(seeding, fields: allFields) == StatusLine(text: "statusCompleted", tone: .secondary))
    }

    @Test func storeBuildsSkipBtAndPluginSelections() {
        func request(_ id: String, _ kind: SelectionKind) -> SelectionRequest {
            SelectionRequest(requestId: id, taskId: "t", kind: kind, defaultChoice: .cancelled, deadlineUnixMs: 0)
        }
        let hidden = [request("b", .bt([], saveDir: "")), request("p", .plugin(site: "s", url: "u", candidates: [])), request("v", .variant([]))]
        #expect(hidden.presentable.isEmpty)
        #expect(PendingSelection.make(hidden) { _ in nil } == nil)
        // 队首被剔除时横幅落到下一条可处理的请求（HLS 照常）。
        #expect(PendingSelection.make(hidden + [request("h", .hls([]))]) { _ in nil }?.route == .selection(requestId: "h"))
    }
    #endif

    @Test func pendingFileConflictMarksOnlyItsTaskAndOverridesTheStatusLine() throws {
        let conflict = SelectionRequest(
            requestId: "r", taskId: "a",
            kind: .fileExists(FileConflict(fileName: "a.bin", saveDir: "/dl", renamePreview: "a (1).bin", actions: [.rename, .overwrite])),
            defaultChoice: .fileExists(action: .rename), deadlineUnixMs: 0
        )
        var state = makeState([makeTask("a", .pending), makeTask("b", .pending)])
        state.selections = [conflict]
        let rows = try #require(derive(state).list.sections.first?.rows)
        let items = rows.compactMap(\.item)
        let flagged = Dictionary(uniqueKeysWithValues: items.map { ($0.id, $0.awaitingDecision) })
        #expect(flagged == ["a": true, "b": false])
        let a = try #require(items.first { $0.id == "a" })
        #expect(echo.status(a, fields: allFields) == StatusLine(text: "fileConflictPending", tone: .warning))
    }

    @Test func conflictBannerAggregatesAllConflictsWithEarliestDeadline() {
        func conflict(_ id: String, deadline: Int64) -> SelectionRequest {
            SelectionRequest(
                requestId: id, taskId: "t-\(id)",
                kind: .fileExists(FileConflict(fileName: "\(id).bin", saveDir: "/dl", renamePreview: "\(id) (1).bin", actions: [.rename, .overwrite])),
                defaultChoice: .fileExists(action: .rename), deadlineUnixMs: deadline
            )
        }
        let one = PendingSelection.make([conflict("a", deadline: 9)]) { _ in nil }
        #expect(one?.route == .fileConflicts)
        #expect(one?.titleKey == "fileConflictTitle" && one?.taskName == "a.bin" && one?.titleCount == nil)

        let many = PendingSelection.make([conflict("a", deadline: 9), conflict("b", deadline: 5)]) { _ in nil }
        #expect(many?.titleKey == "fileConflictTitleMany" && many?.titleCount == 2)
        #expect(many?.deadlineUnixMs == 5 && many?.countdownKey == "fileConflictAutoRenameIn")

        let bt = SelectionRequest(requestId: "bt", taskId: "t", kind: .bt([], saveDir: ""), defaultChoice: .cancelled, deadlineUnixMs: 1)
        #if FULL_FLAVOR
        #expect(PendingSelection.make([bt]) { _ in "x" }?.route == .selection(requestId: "bt"))
        #else
        #expect(PendingSelection.make([bt]) { _ in "x" } == nil)
        #endif
    }

    @Test func metaLineByStateAndFields() throws {
        let nowSec: Int64 = 2_000_000
        let downloading = try item(
            makeTask("a", .downloading, url: "https://www.example.com/f", downloaded: 512 * 1024, total: 1024 * 1024, created: nowSec - 120)
        ) {
            $0.speeds["a"] = LiveSpeed(down: 10, up: 2048)
            $0.runtime["a"] = TaskRuntime(taskId: "a", sampleSequence: 1, activeTransfers: 4, connectedPeers: 9, totalBytes: 1024 * 1024, segments: [])
            $0.queues = [TaskQueue(queueId: "main", name: "Main", position: 0)]
        }
        let full = try #require(echo.meta(downloading, fields: allFields, nowSec: nowSec))
        // 已下 / 总量 · 活跃连接 · ↑ 上行 · 协议 · 站点 · 队列 · 创建时间（BT 节点只对 BT 任务显示）
        let up = try #require(Format.speed(2048)).description
        #expect(full == "512 KB / 1.00 MB · mobileActiveTransfersCount|n=4 · ↑ \(up) · HTTP · example.com · mainQueue · mobileTimeMinutesAgo|n=2")
        #expect(echo.meta(downloading, fields: [.protocol], nowSec: nowSec) == "mobileActiveTransfersCount|n=4 · HTTP")
        #expect(echo.meta(downloading, fields: [], nowSec: nowSec) == "mobileActiveTransfersCount|n=4")
    }

    @Test func btMetaShowsPeersAndCompletedShowsSizeAndFinishedTime() throws {
        let nowSec: Int64 = 2_000_000
        let bt = try item(makeTask("a", .downloading, url: "magnet:?xt=urn:btih:1", downloaded: 1, total: 2)) {
            $0.runtime["a"] = TaskRuntime(taskId: "a", sampleSequence: 1, activeTransfers: nil, connectedPeers: 7, totalBytes: 2, segments: [])
        }
        #expect(echo.meta(bt, fields: [], nowSec: nowSec) == "mobilePeersCount|n=7")

        let done = try item(makeTask("b", .completed, total: 2048, created: nowSec - 100, completedAt: nowSec - 4000))
        #expect(echo.meta(done, fields: [.size], nowSec: nowSec) == "2.00 KB · mobileTimeHoursAgo|n=1")
        // 勾选「创建时间」后完成行不再追加完成时间（避免两个时间并列）
        #expect(echo.meta(done, fields: [.size, .created], nowSec: nowSec) == "2.00 KB · mobileTimeMinutesAgo|n=1")
        #expect(echo.meta(try item(makeTask("c", .preparing)), fields: [.size], nowSec: nowSec) == nil)
    }

    @Test func pausedMetaShowsDoneOfTotalOrOnlyDoneWhenSizeUnknown() throws {
        let known = try item(makeTask("a", .paused, downloaded: 1024, total: 4096))
        #expect(echo.meta(known, fields: [.size], nowSec: 0) == "1.00 KB / 4.00 KB")
        let unknown = try item(makeTask("b", .paused, downloaded: 1024, total: 0))
        #expect(echo.meta(unknown, fields: [.size], nowSec: 0) == "1.00 KB")
    }

    @Test func ringActionAndSummary() throws {
        #expect(echo.ringAction(.pause, isLocalHost: true) == "pause")
        #expect(echo.ringAction(.resume, isLocalHost: true) == "resume")
        #expect(echo.ringAction(.retry, isLocalHost: true) == "mobileRetry")
        #expect(echo.ringAction(.redownload, isLocalHost: true) == "redownloadTask")
        #expect(echo.ringAction(.open, isLocalHost: true) == "mobileOpenFile")
        #expect(echo.ringAction(.open, isLocalHost: false) == "mobileTaskDetail")

        let paused = try item(makeTask("a", .paused, name: "movie.mkv", downloaded: 500, total: 1000))
        #expect(echo.summary(paused, fields: [.size], nowSec: 0) == "movie.mkv, statusPaused, 50%, 500 B / 1000 B")
    }

    @Test func queueLabelResolvesBuiltInQueues() {
        #expect(echo.queueLabel(TaskQueue(queueId: "", name: "x")) == "mainQueue")
        #expect(echo.queueLabel(TaskQueue(queueId: "main", name: "x")) == "mainQueue")
        #expect(echo.queueLabel(TaskQueue(queueId: "later", name: "x")) == "downloadLater")
        #expect(echo.queueLabel(TaskQueue(queueId: "q1", name: "Night")) == "Night")
    }
}

// MARK: - 移动到队列

@MainActor
struct MoveToQueueTests {
    @Test func currentQueueOnlyWhenAllSelectedShareOne() {
        let tasks = [makeTask("a", .paused, queue: ""), makeTask("b", .paused, queue: "main"), makeTask("c", .paused, queue: "later")]
        #expect(MoveToQueueSheet.currentQueue(of: ["a", "b"], in: tasks) == "main")
        #expect(MoveToQueueSheet.currentQueue(of: ["a", "c"], in: tasks) == nil)
        #expect(MoveToQueueSheet.currentQueue(of: ["c"], in: tasks) == "later")
        #expect(MoveToQueueSheet.currentQueue(of: [], in: tasks) == nil)
    }

    @Test func countsMergeImplicitMainQueue() {
        let tasks = [makeTask("a", .paused, queue: ""), makeTask("b", .paused, queue: "main"), makeTask("c", .paused, queue: "later")]
        #expect(MoveToQueueSheet.counts(of: tasks) == ["main": 2, "later": 1])
    }
}
