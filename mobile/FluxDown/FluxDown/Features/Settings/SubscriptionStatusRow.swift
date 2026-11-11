import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

/// 订阅状态行（BT Tracker / eD2K 服务器 / Kad 节点共用）：已订阅条数 + 更新时间（或失败原因）+「立即更新」。
///
/// 数据来自只读的 `*_sub_cache` / `*_sub_updated_at` / `ed2k_nodes_dat_{count,updated_at}`（`daemon.config` 快照）；
/// 「立即更新」调用慢 RPC（`daemon.bt.trackerSubscription.refresh` / `daemon.ed2k.serverSubscription.refresh` /
/// `daemon.ed2k.nodesSubscription.refresh`，无参数），行内显示进度，页面其余部分保持可用。
/// 刷新结果先于配置快照到达时按更新时间合并显示。
struct SubscriptionStatusRow: View {
    let kind: SubscriptionKind

    @Environment(AppContainer.self) private var container
    @Environment(ConfigEditor.self) private var editor

    private nonisolated enum Phase: Equatable {
        case idle
        case refreshing
        /// 失败：`detail` 为 daemon 错误摘要 / 传输错误文案（可为空）。
        case failed(detail: String)
    }

    @State private var phase: Phase = .idle
    @State private var fresh: SubscriptionRefreshOutcome?

    var body: some View {
        let model = SubscriptionStatusModel.make(kind: kind, form: editor.form, fresh: fresh)
        VStack(alignment: .leading, spacing: 4) {
            VStack(alignment: .leading, spacing: 4) {
                Text(L(kind.statusKey, ["n": model.count]))
                statusLine(model)
            }
            .accessibilityElement(children: .combine)
            SettingsActionRow(
                title: L(kind.updateNowKey), runningTitle: L(kind.updatingKey),
                systemImage: FluxSymbol.syncing, isRunning: phase == .refreshing, action: refresh
            )
        }
        .settingsRow(kind.rowID)
        .fluxAnimation(.smooth, value: phase)
    }

    @ViewBuilder
    private func statusLine(_ model: SubscriptionStatusModel) -> some View {
        if case let .failed(detail) = phase {
            SettingsStatusLine(
                text: detail.isEmpty ? L(kind.failedKey) : "\(L(kind.failedKey)): \(detail)", tone: .failure
            )
        } else {
            Text(
                model.updatedAt > 0
                    ? L(kind.updatedAtTextKey, ["time": SubscriptionTime.text(unix: model.updatedAt)])
                    : L(kind.neverKey)
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
        }
    }

    private func refresh() {
        guard phase != .refreshing else { return }
        phase = .refreshing
        Task {
            do throws(HostError) {
                let outcome = try await Self.run(kind, container.session)
                fresh = outcome
                phase = outcome.success ? .idle : .failed(detail: outcome.error)
                (outcome.success ? FluxHaptic.success : FluxHaptic.error).play()
            } catch {
                phase = .failed(detail: ErrorText.describe(error))
                FluxHaptic.error.play()
            }
        }
    }

    private static func run(_ kind: SubscriptionKind, _ session: any HostSession) async throws(HostError) -> SubscriptionRefreshOutcome {
        switch kind {
        case .btTrackers:
            let response: TrackerSubRefreshResponse = try await session.call(HostMethod.daemonBtTrackerSubscriptionRefresh)
            return response.outcome
        case .ed2kServers:
            let response: Ed2kServerSubRefreshResponse = try await session.call(HostMethod.daemonEd2kServerSubscriptionRefresh)
            return response.outcome
        case .ed2kNodes:
            let response: Ed2kNodesSubRefreshResponse = try await session.call(HostMethod.daemonEd2kNodesSubscriptionRefresh)
            return response.outcome
        }
    }
}
#endif
