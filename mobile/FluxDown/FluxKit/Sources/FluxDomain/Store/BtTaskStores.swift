import Foundation
import Observation

// BT 任务「文件」页与设置「连接」页端口映射状态的视图仓库（主 actor、不持有会话：每次调用传入当前 `HostSession`）。
// 与 ``BtGuardStateStore`` 同一模式：可见时按固定间隔轮询，迟到的响应按代数丢弃，失败保留旧数据并暴露错误。

// MARK: - 文件表

@MainActor
@Observable
public final class BtFilesStore {
    /// 最近一次成功的文件表；首轮在途时为 nil。
    public private(set) var files: BtFiles?
    /// 最近一次加载失败；成功后清空。已有 `files` 时界面保留旧数据。
    public private(set) var error: HostError?
    /// 改优先级请求在途（期间不再接受新的改动，避免乱序覆盖）。
    public private(set) var isSaving = false

    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var taskId: String?

    /// 文件进度没有变化事件，可见时按此间隔刷新。
    public static let pollInterval: Duration = .seconds(2)

    public init() {}

    public func load(taskId: String, _ session: any HostSession) async {
        if self.taskId != taskId {
            // 换任务：丢弃上一个任务的数据，避免短暂显示别的任务的文件。
            self.taskId = taskId
            files = nil
            error = nil
        }
        generation += 1
        let mine = generation
        do throws(HostError) {
            let next: BtFiles = try await session.call(HostMethod.daemonTaskBtFiles, params: TaskIdParams(taskId: taskId))
            guard mine == generation else { return }
            files = next
            error = nil
        } catch {
            guard mine == generation else { return }
            self.error = error
        }
    }

    /// 立即加载一次，然后每 ``pollInterval`` 刷新，直到任务被取消（页面离开 / 换任务 / 退到后台）。
    public func poll(taskId: String, _ session: any HostSession) async {
        while !Task.isCancelled {
            await load(taskId: taskId, session)
            do {
                try await Task.sleep(for: Self.pollInterval)
            } catch {
                return
            }
        }
    }

    /// 下发优先级改动并乐观更新本地表；失败抛出（调用方走通用错误 Toast），本地表回到主机下一次轮询的值。
    public func setPriorities(taskId: String, _ changes: [BtFilePriorityChange], _ session: any HostSession) async throws(HostError) {
        guard !changes.isEmpty, !isSaving else { return }
        isSaving = true
        defer { isSaving = false }
        try await session.callVoid(
            HostMethod.daemonTaskSetFilePriorities,
            params: SetBtFilePrioritiesParams(taskId: taskId, priorities: changes)
        )
        guard self.taskId == taskId, var current = files else { return }
        // 让在途的旧轮询结果作废，免得把刚改好的级别闪回旧值。
        generation += 1
        current.files = BtFilesRules.applying(changes, to: current.files)
        files = current
    }
}

// MARK: - 端口映射状态

@MainActor
@Observable
public final class BtPortStatusStore {
    /// 最近一次成功的快照；首轮在途 / 旧 daemon 无此方法时为 nil（行内留空）。
    public private(set) var status: BtPortStatus?

    @ObservationIgnored private var generation = 0

    /// 映射结果没有变化事件，可见时按此间隔刷新。
    public static let pollInterval: Duration = .seconds(5)

    public init() {}

    /// 失败（含旧 daemon 的 method-not-found）静默保留旧值：状态行是只读读数，不弹 Toast（同 GPUI）。
    public func load(_ session: any HostSession) async {
        generation += 1
        let mine = generation
        do throws(HostError) {
            let next: BtPortStatus = try await session.call(HostMethod.daemonBtPortStatus)
            guard mine == generation else { return }
            status = next
        } catch {
            return
        }
    }

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

    /// 切主机时清空，避免显示上一台主机的映射。
    public func reset() {
        generation += 1
        status = nil
    }
}
