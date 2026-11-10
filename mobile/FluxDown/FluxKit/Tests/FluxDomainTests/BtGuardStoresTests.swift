import FluxDomain
import Foundation
import Testing

/// 防吸血仓库：分页追加 / 重取首页 / 批量解除分块 / 加白 / 迟到响应丢弃（脚本化假会话，不依赖主机）。
@MainActor
struct BtGuardStoresTests {
    private final class Scripted: HostSession, @unchecked Sendable {
        let signals = AsyncStream<HostSignal> { $0.finish() }
        private let lock = NSLock()
        private var recorded: [(method: String, params: String?)] = []
        var handler: @Sendable (String, [String: Any]) throws -> String = { _, _ in "{}" }

        var calls: [(method: String, params: String?)] { lock.withLock { recorded } }

        func call(_ method: String, params: Data?) async throws(HostError) -> Data {
            let text = params.map { String(decoding: $0, as: UTF8.self) }
            lock.withLock { recorded.append((method, text)) }
            let object = (params.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any]) ?? [:]
            do {
                return try Data(handler(method, object).utf8)
            } catch let error as HostError {
                throw error
            } catch {
                throw HostError(.internal, message: "\(error)")
            }
        }

        func close() {}
        func categorySaveDir(fileName _: String, url _: String) -> String? { nil }
        func createTask(_: CreateTaskRequest) async throws(HostError) -> String { "" }
        func pause(_: String) async throws(HostError) {}
        func resume(_: String) async throws(HostError) {}
        func delete(_: String, deleteFiles _: Bool) async throws(HostError) {}
        func pauseMany(_: [String]) async throws(HostError) {}
        func resumeMany(_: [String]) async throws(HostError) {}
        func deleteMany(_: [String], deleteFiles _: Bool) async throws(HostError) {}
        func pauseAll() async throws(HostError) {}
        func resumeAll() async throws(HostError) {}
        func rename(_: String, fileName _: String) async throws(HostError) {}
        func changeUrl(_: String, url _: String) async throws(HostError) {}
        func rescan() async throws(HostError) {}
        func moveToQueue(_: String, queueId _: String) async throws(HostError) {}
        func boost(_: String) async throws(HostError) {}
        func resolveSelection(_: String, outcome _: SelectionOutcome) async throws(HostError) {}
        func patchConfig(expectedRevision _: UInt64, values _: [String: String]) async throws(HostError) {}
        func refreshRssSource(_: String) async throws(HostError) {}
        func setRssSourceEnabled(_: String, enabled _: Bool) async throws(HostError) {}
    }

    /// 服务端模拟：`total` 条 manual 条目（id 1...total），按 offset / limit 分页。
    private static func pagedHandler(total: Int) -> @Sendable (String, [String: Any]) throws -> String {
        { method, params in
            guard method == HostMethod.daemonBtGuardEntriesList else { return "{}" }
            let offset = params["offset"] as? Int ?? 0
            let limit = params["limit"] as? Int ?? 100
            let upper = min(total, offset + limit)
            let rows = offset < upper
                ? (offset + 1 ... upper).map { #"{"id":\#($0),"layer":"manual","cidr":"10.0.0.\#($0 % 250)","action":"ban","createdAt":1}"# }
                : []
            return #"{"total":\#(total),"items":[\#(rows.joined(separator: ","))]}"#
        }
    }

    private static func json(_ text: String?) -> [String: Any] {
        guard let text, let object = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any] else { return [:] }
        return object
    }

    // MARK: 状态

    @Test func stateStoreLoadsAndKeepsOldStateOnFailure() async {
        let session = Scripted()
        session.handler = { _, _ in #"{"enabled":true,"sessionActive":true,"stats":{"todayObserved":4}}"# }
        let store = BtGuardStateStore()
        await store.load(session)
        #expect(store.state?.sessionActive == true && store.state?.stats.todayObserved == 4)
        #expect(store.error == nil)

        session.handler = { _, _ in throw HostError(.unavailable, message: "down") }
        await store.load(session)
        #expect(store.state?.stats.todayObserved == 4) // 保留旧数据
        #expect(store.error?.code == .unavailable)
        #expect(session.calls.allSatisfy { $0.method == "daemon.bt.guard.state" })

        session.handler = { _, _ in #"{"enabled":false}"# }
        await store.load(session)
        #expect(store.error == nil && store.state?.enabled == false)
    }

    @Test func stateStoreAppliesFilterSources() async {
        let session = Scripted()
        session.handler = { _, _ in #"{"enabled":true,"sessionActive":false}"# }
        let store = BtGuardStateStore()
        store.apply(filterSources: [BtGuardFilterSource(url: "u")]) // 尚无状态：忽略
        #expect(store.state == nil)
        await store.load(session)
        store.apply(filterSources: [BtGuardFilterSource(url: "https://e.x/l.txt", ok: true, entries: 2)])
        #expect(store.state?.filterSources.first?.entries == 2)
    }

    // MARK: 封禁名单分页

    @Test func resetLoadsFirstPageAndLoadMoreAppendsWithoutDuplicates() async {
        let session = Scripted()
        session.handler = Self.pagedHandler(total: 250)
        let store = BtGuardBansStore()
        await store.reset(session, debounce: false)
        #expect(store.isLoaded && store.items.count == 100 && store.total == 250 && store.hasMore)
        await store.loadMore(session)
        #expect(store.items.count == 200 && store.hasMore && !store.isLoadingMore)
        await store.loadMore(session)
        #expect(store.items.count == 250 && !store.hasMore)
        #expect(Set(store.items.map(\.id)).count == 250)
        await store.loadMore(session) // 已到末页：不再请求
        let listCalls = session.calls.filter { $0.method == HostMethod.daemonBtGuardEntriesList }
        #expect(listCalls.count == 3)
    }

    @Test func filterIsSentAsListParams() async {
        let session = Scripted()
        session.handler = Self.pagedHandler(total: 1)
        let store = BtGuardBansStore(layer: "auto")
        store.query = "  1.2.3  "
        await store.reset(session, debounce: false)
        let json = Self.json(session.calls.first?.params)
        #expect(json["layer"] as? String == "auto" && json["query"] as? String == "1.2.3")
        #expect(json["rule"] == nil && json["offset"] as? Int == 0 && json["limit"] as? Int == 100)
        #expect(store.filterKey == "auto|1.2.3")
        #expect(store.isFiltered)
        #expect(!BtGuardBansStore().isFiltered)
    }

    @Test func reloadKeepsTheShownCountUpToTheCap() async throws {
        let session = Scripted()
        session.handler = Self.pagedHandler(total: 700)
        let store = BtGuardBansStore()
        await store.reset(session, debounce: false)
        for _ in 0 ..< 3 { await store.loadMore(session) }
        #expect(store.items.count == 400)
        await store.reload(session)
        let json = Self.json(session.calls.last?.params)
        #expect(json["limit"] as? Int == 400 && json["offset"] as? Int == 0)
        #expect(store.items.count == 400)
        for _ in 0 ..< 3 { await store.loadMore(session) }
        await store.reload(session)
        let cappedJSON = Self.json(session.calls.last?.params)
        #expect(cappedJSON["limit"] as? Int == 500) // 封顶一页上限
    }

    @Test func failedFirstPageSurfacesErrorAndRecovers() async {
        let session = Scripted()
        session.handler = { _, _ in throw HostError(.timeout) }
        let store = BtGuardBansStore()
        await store.reset(session, debounce: false)
        #expect(!store.isLoaded && store.error?.code == .timeout && store.items.isEmpty)
        session.handler = Self.pagedHandler(total: 2)
        await store.reload(session)
        #expect(store.isLoaded && store.error == nil && store.items.count == 2)
    }

    @Test func resetClearsStaleRowsBeforeTheNewFilterLoads() async {
        let session = Scripted()
        session.handler = Self.pagedHandler(total: 3)
        let store = BtGuardBansStore()
        await store.reset(session, debounce: false)
        #expect(store.items.count == 3)
        session.handler = { _, _ in throw HostError(.unavailable) }
        store.layer = "cloud"
        await store.reset(session, debounce: false)
        #expect(store.items.isEmpty && store.total == 0 && !store.isLoaded)
    }

    // MARK: 解除 / 加白

    @Test func releaseChunksIdsAndReloads() async {
        let session = Scripted()
        let paged = Self.pagedHandler(total: 5)
        session.handler = { method, params in
            if method == HostMethod.daemonBtGuardEntriesRemove {
                let ids = params["ids"] as? [Int] ?? []
                return #"{"removed":\#(ids.count)}"#
            }
            return try paged(method, params)
        }
        let store = BtGuardBansStore()
        await store.reset(session, debounce: false)
        let ids = (1 ... 2300).map(Int64.init)
        let outcome = await store.release(ids, session: session)
        #expect(outcome == .init(removed: 2300, error: nil))
        let removeCalls = session.calls.filter { $0.method == HostMethod.daemonBtGuardEntriesRemove }
        #expect(removeCalls.count == 3)
        #expect(session.calls.last?.method == HostMethod.daemonBtGuardEntriesList) // 成功后重取
        #expect(!store.isBusy)
    }

    @Test func releaseStopsAtFirstFailureButStillReloads() async {
        let session = Scripted()
        let paged = Self.pagedHandler(total: 1)
        let counter = Counter()
        session.handler = { method, params in
            if method == HostMethod.daemonBtGuardEntriesRemove {
                if counter.next() == 2 { throw HostError(.invalidArgument, message: "cloud") }
                return #"{"removed":1000}"#
            }
            return try paged(method, params)
        }
        let store = BtGuardBansStore()
        let outcome = await store.release((1 ... 2500).map(Int64.init), session: session)
        #expect(outcome.removed == 1000 && outcome.error?.code == .invalidArgument)
        #expect(session.calls.filter { $0.method == HostMethod.daemonBtGuardEntriesRemove }.count == 2)
        #expect(session.calls.last?.method == HostMethod.daemonBtGuardEntriesList)
        #expect(!store.isBusy)
    }

    @Test func emptyReleaseSendsNothing() async {
        let session = Scripted()
        let store = BtGuardBansStore()
        let outcome = await store.release([], session: session)
        #expect(outcome == .init(removed: 0, error: nil) && session.calls.isEmpty)
    }

    @Test func allowAddsPermanentAllowEntryThenReloads() async throws {
        let session = Scripted()
        let paged = Self.pagedHandler(total: 1)
        session.handler = { method, params in
            method == HostMethod.daemonBtGuardEntriesAdd
                ? #"{"id":7,"layer":"allow","cidr":"1.2.3.4","action":"allow","createdAt":1}"#
                : try paged(method, params)
        }
        let store = BtGuardBansStore()
        let outcome = await store.allow(BtGuardEntry(id: 1, layer: "cloud", cidr: "1.2.3.4/32", action: "ban"), session: session)
        #expect(outcome == .added)
        let json = Self.json(session.calls.first { $0.method == HostMethod.daemonBtGuardEntriesAdd }?.params)
        #expect(json["cidr"] as? String == "1.2.3.4/32" && json["action"] as? String == "allow")
        #expect(json["durationSecs"] == nil)
        #expect(session.calls.last?.method == HostMethod.daemonBtGuardEntriesList)
    }

    @Test func allowReportsRpcFailureWithoutReloading() async {
        let session = Scripted()
        session.handler = { _, _ in throw HostError(.invalidArgument, message: "bad cidr") }
        let store = BtGuardBansStore()
        let outcome = await store.allow(BtGuardEntry(id: 1, layer: "auto", cidr: "x", action: "ban"), session: session)
        guard case let .failed(failure) = outcome else {
            Issue.record("expected failure, got \(outcome)")
            return
        }
        #expect(failure.code == .invalidArgument)
        #expect(session.calls.count == 1)
        #expect(!store.isBusy)
    }

    @Test func allowWhileAnotherOperationRunsReturnsBusyAndSendsNothing() async {
        let session = Scripted()
        let gate = DispatchSemaphore(value: 0)
        session.handler = { method, _ in
            if method == HostMethod.daemonBtGuardEntriesAdd { gate.wait() }
            return #"{"id":7,"layer":"allow","cidr":"1.2.3.4","action":"allow","createdAt":1}"#
        }
        let store = BtGuardBansStore()
        let entry = BtGuardEntry(id: 1, layer: "auto", cidr: "1.2.3.4/32", action: "ban")
        let first = Task { await store.allow(entry, session: session) }
        while !store.isBusy || session.calls.isEmpty { await Task.yield() }
        // 修复前忙碌返回 nil，被调用方当成功提示「已加白」；现在明确 .busy 且不发请求。
        let second = await store.allow(entry, session: session)
        #expect(second == .busy)
        #expect(session.calls.filter { $0.method == HostMethod.daemonBtGuardEntriesAdd }.count == 1)
        gate.signal()
        #expect(await first.value == .added)
        #expect(!store.isBusy)
    }

    @Test func pruneSelectionAndNoteHelpers() {
        let items = [BtGuardEntry(id: 1, layer: "manual", cidr: "a", action: "ban", reason: "  r  ", source: " user "),
                     BtGuardEntry(id: 2, layer: "auto", cidr: "b", action: "ban")]
        #expect(BtGuardRules.pruneSelection([1, 2, 3], items: items) == [1, 2])
        #expect(BtGuardRules.entryNote(items[0]) == "r · user")
        #expect(BtGuardRules.entryNote(items[1]) == nil)
    }

    private final class Counter: @unchecked Sendable {
        private let lock = NSLock()
        private var value = 0
        func next() -> Int { lock.withLock { value += 1; return value } }
    }
}
