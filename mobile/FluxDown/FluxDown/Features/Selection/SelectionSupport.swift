import FluxDomain
import FluxUI
import SwiftUI

/// 答复成功后关闭 Sheet 的回调（在主 actor 上执行，可跨 Task 传递）。
typealias SelectionClose = @MainActor @Sendable () -> Void

// MARK: - 答复

/// 经 `HostSession.resolveSelection` 答复；去重、只读拦截与错误 toast 集中在这里。
@MainActor
@Observable
final class SelectionResolver {
    @ObservationIgnored private unowned let container: AppContainer
    @ObservationIgnored private var inFlight = Set<String>()
    @ObservationIgnored private var ours = Set<String>()

    init(container: AppContainer) {
        self.container = container
    }

    func wasOurs(_ requestId: String) -> Bool { ours.contains(requestId) }

    /// 只读（断连宽限后）时拒绝用户操作：触感 + 错误 toast，返回 true 表示已拦截。
    private func blockedByReadOnly() -> Bool {
        guard container.store.state.isReadOnly else { return false }
        FluxHaptic.error.play()
        container.toasts.show(text: L("localServiceDisconnected"), tone: .error, systemImage: FluxSymbol.offline)
        return true
    }

    /// - Parameters:
    ///   - successToast: 答复成功后的提示；nil 不提示。
    ///   - userInitiated: 用户操作才受只读拦截；倒计时自动答复照常尝试。
    func resolve(
        _ request: SelectionRequest,
        _ outcome: SelectionOutcome,
        successToast: String?,
        userInitiated: Bool,
        onSuccess: @escaping SelectionClose
    ) {
        let toasts = container.toasts
        if userInitiated, blockedByReadOnly() { return }
        let id = request.requestId
        guard inFlight.insert(id).inserted else { return }
        ours.insert(id) // 先登记：请求消失的事件可能先于本任务恢复到达
        let session = container.session
        Task {
            defer { inFlight.remove(id) }
            do throws(HostError) {
                try await session.resolveSelection(id, outcome: outcome)
                FluxHaptic.success.play()
                if let successToast { toasts.show(text: successToast, tone: .success, systemImage: "checkmark.circle") }
                onSuccess()
            } catch {
                // 其它设备已答复 / 引擎超时已处理（conflict / notFound）：请求即将消失，静默放弃；
                // 网络 / RPC 失败：请求仍在，提示错误让用户重试。
                ours.remove(id)
                if !Self.isStale(error), container.store.state.selections.contains(where: { $0.requestId == id }) {
                    FluxHaptic.error.play()
                    toasts.show(text: ErrorText.describe(error), tone: .error)
                }
            }
        }
    }

    /// 请求已被解决或已过期（而不是传输失败）。
    private static func isStale(_ error: HostError) -> Bool {
        error.code == .conflict || error.code == .notFound
    }

    /// 对一批「文件已存在」请求答复同一个结果（动作类结果只含该动作被允许的请求）：顺序答复，失败合并为一条 toast。
    func resolveAll(_ requests: [SelectionRequest], outcome: SelectionOutcome) {
        let targets: [String]
        if case let .fileExists(action) = outcome {
            targets = FileConflictBulk.targets(requests, action: action).map(\.requestId)
        } else {
            targets = requests.map(\.requestId)
        }
        guard !targets.isEmpty, !blockedByReadOnly() else { return }
        let toasts = container.toasts
        let session = container.session
        Task {
            var failure: HostError?
            for id in targets where inFlight.insert(id).inserted {
                ours.insert(id)
                do throws(HostError) {
                    try await session.resolveSelection(id, outcome: outcome)
                } catch {
                    ours.remove(id)
                    if !Self.isStale(error), container.store.state.selections.contains(where: { $0.requestId == id }) {
                        failure = failure ?? error
                    }
                }
                inFlight.remove(id)
            }
            if let failure {
                FluxHaptic.error.play()
                toasts.show(text: ErrorText.describe(failure), tone: .error)
            } else {
                FluxHaptic.success.play()
            }
        }
    }

    /// 取消：任务保持暂停。
    func cancel(_ request: SelectionRequest, onSuccess: @escaping SelectionClose) {
        resolve(request, .cancelled, successToast: L("mobileSelectionCancelled"), userInitiated: true, onSuccess: onSuccess)
    }

    /// 倒计时到期：按默认项答复。
    func applyDefault(_ request: SelectionRequest, onSuccess: @escaping SelectionClose) {
        resolve(request, request.defaultChoice, successToast: L("mobileSelectionAutoApplied"), userInitiated: false, onSuccess: onSuccess)
    }
}

// MARK: - 共用 UI

/// 倒计时基准：首次出现时固定 `total`，环按 deadline 重算（重建视图不会重置）。
/// `enabled == false`：请求无截止（用户关闭了自动选择默认项），不显示倒计时、不自动答复。
struct SelectionTiming {
    let deadline: Date
    let total: TimeInterval
    let enabled: Bool

    init(_ request: SelectionRequest) {
        enabled = request.hasDeadline
        deadline = Date(timeIntervalSince1970: TimeInterval(request.deadlineUnixMs) / 1000)
        total = max(deadline.timeIntervalSinceNow, 1)
    }
}

/// 任务行：类别图标 + 等宽文件名 + 协议徽标。
struct SelectionTaskRow: View {
    let task: DownloadTask?

    var body: some View {
        if let task {
            HStack(spacing: 12) {
                KindIcon(kind: task.protocol == .bt ? .torrent : FileKind.from(fileName: task.fileName), size: 36)
                Text(task.fileName)
                    .font(.subheadline.monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 8)
                ProtocolBadge(text: tag(task.protocol))
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(tag(task.protocol)), \(task.fileName)")
        }
    }

    private func tag(_ p: TaskProtocol) -> String { TaskDetailFormat.protocolTag(p) }
}

/// 底部栏：左侧倒计时环 + 文案，右侧主操作。使用系统 `.bottomBar` 工具栏——系统只渲染一层 Liquid Glass，
/// 在 medium（玻璃）/ large（不透明）Sheet 里都不会出现玻璃叠玻璃。
struct SelectionBar<Primary: View>: ToolbarContent {
    let timing: SelectionTiming
    let onExpire: () -> Void
    @ViewBuilder let primary: Primary

    var body: some ToolbarContent {
        ToolbarItem(placement: .bottomBar) {
            if timing.enabled {
                HStack(spacing: 8) {
                    CountdownRing(
                        deadline: timing.deadline,
                        total: timing.total,
                        label: { L("selectionAutoDefaultIn", ["seconds": $0]) },
                        onExpire: onExpire
                    )
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        let seconds = max(0, Int(timing.deadline.timeIntervalSince(context.date).rounded(.up)))
                        Text(L("selectionAutoDefaultIn", ["seconds": seconds]))
                            .font(.footnote)
                            .monospacedDigit()
                            .lineLimit(2)
                            .foregroundStyle(seconds <= 5 ? Color.fdStatusWarningText : Color.secondary)
                    }
                    .accessibilityHidden(true)
                }
            }
        }
        ToolbarSpacer(.flexible, placement: .bottomBar)
        ToolbarItem(placement: .bottomBar) { primary }
    }
}
