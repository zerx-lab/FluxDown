import Foundation
import Observation

// BT 防吸血的两个视图仓库（主 actor、不持有会话：每次调用传入当前 `HostSession`）。
// 镜像 Web `useGuard.ts`：状态按固定间隔 + `btGuardChanged` 通知刷新；条目分页追加，变化时按已载入条数重取首页，
// 迟到的响应按代数丢弃。

// MARK: - 状态总览

@MainActor
@Observable
public final class BtGuardStateStore {
    public private(set) var state: BtGuardState?
    /// 最近一次加载失败；成功后清空。已有 `state` 时界面保留旧数据并仍可显示该错误。
    public private(set) var error: HostError?

    @ObservationIgnored private var generation = 0

    /// 统计没有变化事件，可见时按此间隔刷新。
    public static let pollInterval: Duration = .seconds(5)

    public init() {}

    public func load(_ session: any HostSession) async {
        generation += 1
        let mine = generation
        do throws(HostError) {
            let next: BtGuardState = try await session.call(HostMethod.daemonBtGuardState)
            guard mine == generation else { return }
            state = next
            error = nil
        } catch {
            guard mine == generation else { return }
            self.error = error
        }
    }

    /// 立即加载一次，然后每 ``pollInterval`` 刷新，直到任务被取消（页面离开 / 通知触发重启）。
    public func poll(_ session: any HostSession) async {
        while !Task.isCancelled {
            await load(session)
            do {
                try await Task.sleep(for: Self.pollInterval)
            } catch {
                return
            }
        }
    }

    /// 过滤订阅刷新结果先于状态轮询到达时直接并入。
    public func apply(filterSources: [BtGuardFilterSource]) {
        state?.filterSources = filterSources
    }
}

// MARK: - 封禁名单

@MainActor
@Observable
public final class BtGuardBansStore {
    /// 层筛选；nil = 全部。
    public var layer: String?
    public var query = ""

    public private(set) var items: [BtGuardEntry] = []
    public private(set) var total = 0
    /// 当前筛选的首页已载入（失败不算）。
    public private(set) var isLoaded = false
    public private(set) var isLoadingMore = false
    /// 解除 / 加白进行中（期间拒绝重复提交）。
    public private(set) var isBusy = false
    public private(set) var error: HostError?

    @ObservationIgnored private var generation = 0

    public struct ReleaseOutcome: Sendable, Hashable {
        public var removed: Int
        public var error: HostError?

        public init(removed: Int, error: HostError? = nil) {
            self.removed = removed
            self.error = error
        }
    }

    public static let searchDebounce: Duration = .milliseconds(300)

    public init(layer: String? = nil) { self.layer = layer }

    /// 筛选条件键：变化时视图重启 ``reset(_:debounce:)``。
    public var filterKey: String {
        "\(layer ?? "all")|\(query.trimmingCharacters(in: .whitespacesAndNewlines))"
    }

    public var isFiltered: Bool {
        layer != nil || !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    public var hasMore: Bool { items.count < total }

    /// 筛选变化：清空并重取首页（`debounce` 用于搜索输入）。
    public func reset(_ session: any HostSession, debounce: Bool) async {
        generation += 1
        let mine = generation
        items = []
        total = 0
        isLoaded = false
        isLoadingMore = false
        error = nil
        if debounce {
            do {
                try await Task.sleep(for: Self.searchDebounce)
            } catch {
                return
            }
            guard mine == generation else { return }
        }
        await fetch(session, offset: 0, limit: BtGuardLimits.defaultPage, replace: true, generation: mine)
    }

    /// 变化通知 / 解除后：按已载入条数（封顶一页上限）重取首页，不丢已展开的内容。
    public func reload(_ session: any HostSession) async {
        generation += 1
        let mine = generation
        isLoadingMore = false
        let limit = min(max(items.count, BtGuardLimits.defaultPage), BtGuardLimits.maxPage)
        await fetch(session, offset: 0, limit: limit, replace: true, generation: mine)
    }

    public func loadMore(_ session: any HostSession) async {
        guard isLoaded, hasMore, !isLoadingMore else { return }
        generation += 1
        let mine = generation
        isLoadingMore = true
        await fetch(session, offset: items.count, limit: BtGuardLimits.defaultPage, replace: false, generation: mine)
        if mine == generation { isLoadingMore = false }
    }

    private func fetch(_ session: any HostSession, offset: Int, limit: Int, replace: Bool, generation mine: Int) async {
        let params = BtGuardRules.entriesQuery(layer: layer, query: query, offset: offset, limit: limit)
        do throws(HostError) {
            let page: BtGuardEntriesPage = try await session.call(HostMethod.daemonBtGuardEntriesList, params: params)
            guard mine == generation else { return }
            items = replace ? page.items : BtGuardRules.mergePage(items, page.items)
            total = Int(clamping: page.total)
            isLoaded = true
            error = nil
        } catch {
            guard mine == generation else { return }
            self.error = error
        }
    }

    /// 批量解除：按协议单次上限分块；云层条目由调用方预先过滤（`BtGuardRules.removableIDs`）。
    /// 中途失败时返回已解除条数与错误；无论成败都重取列表。
    public func release(_ ids: [Int64], session: any HostSession) async -> ReleaseOutcome {
        guard !ids.isEmpty, !isBusy else { return ReleaseOutcome(removed: 0) }
        isBusy = true
        defer { isBusy = false }
        var removed = 0
        var failure: HostError?
        for chunk in BtGuardRules.chunkIDs(ids) {
            do throws(HostError) {
                let result: BtGuardEntriesRemoveResult = try await session.call(
                    HostMethod.daemonBtGuardEntriesRemove, params: BtGuardEntriesRemoveParams(ids: chunk)
                )
                removed += Int(result.removed)
            } catch {
                failure = error
                break
            }
        }
        await reload(session)
        return ReleaseOutcome(removed: removed, error: failure)
    }

    public enum AllowOutcome: Sendable, Hashable {
        case added
        /// 另一项解除 / 加白进行中，本次未执行。
        case busy
        case failed(HostError)
    }

    /// 把条目的地址加入白名单（永久）。忙碌时不执行并返回 `.busy`（调用方不得提示成功）。
    public func allow(_ entry: BtGuardEntry, session: any HostSession) async -> AllowOutcome {
        guard !isBusy else { return .busy }
        isBusy = true
        defer { isBusy = false }
        do throws(HostError) {
            try await session.callVoid(HostMethod.daemonBtGuardEntriesAdd, params: BtGuardRules.allowParams(cidr: entry.cidr))
        } catch {
            return .failed(error)
        }
        await reload(session)
        return .added
    }
}
