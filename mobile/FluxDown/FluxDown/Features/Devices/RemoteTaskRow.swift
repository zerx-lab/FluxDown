import FluxDomain
import FluxUI
import SwiftUI
import UIKit

/// 远程任务行（V1 §E 与 V3 共用）：文件名、`→ 目标设备` + 状态徽标、细进度条、百分比 / 字节 / 速度，
/// 行尾一个主按钮（暂停 / 继续），滑动取消 / 删除，长按含全部可用动作。
///
/// 控制矩阵只走 `RemoteTaskRules.canIssue`（状态矩阵 + 暂停 / 继续需目标在线）；未知状态没有任何控件，只显示中性「未知」徽标。
/// 命令在途时行尾显示 `ProgressView`；失败 Toast 由 `DevicesModel.issue` 负责。
struct RemoteTaskRow: View {
    @Environment(AppContainer.self) private var container
    @Environment(\.dynamicTypeSize) private var typeSize

    let task: RemoteTaskDto
    /// 目标设备名；`showsTarget == false`（设备详情里的列表）时不显示。
    let targetName: String
    var showsTarget = true
    /// `presenceKnown ? device.isOnline : nil`（未知不据此禁用）。
    let targetOnline: Bool?
    let busy: Bool
    let readOnly: Bool
    /// 下载页多选模式：行只负责被选中，不显示行内按钮 / 滑动 / 长按菜单。
    var selecting = false
    /// 下发命令：动作 + 是否同时删除文件。
    let issue: (RemoteCommandAction, Bool) -> Void

    @State private var confirmingDelete = false

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 6) {
                Text(displayName)
                    .font(.body)
                    .lineLimit(typeSize.isAccessibilitySize ? 3 : 1)
                    .truncationMode(.middle)
                metaLine
                if !task.status.isUnknown {
                    progress
                    detailLine
                }
                if let error = errorText {
                    Label(error, systemImage: FluxSymbol.failure)
                        .font(.caption)
                        .foregroundStyle(Color.fdStatusFailedText)
                        .lineLimit(3)
                }
                if offlineBlocksPrimary {
                    Label(L("errReasonTargetDeviceOffline"), systemImage: FluxSymbol.offline)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
            trailing
        }
        .padding(.vertical, 2)
        // 滑动删除只弹确认框：不能用 `role: .destructive`，否则 List 会立即动画移除该行，
        // 数据源仍有这一行 → UICollectionView 行数不一致崩溃（同本地任务行，用红色 tint 代替）。
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            if !selecting, allows(.delete) {
                Button(L("delete"), systemImage: FluxSymbol.delete) { confirmingDelete = true }
                    .tint(Color.fdStatusFailed)
            }
            if !selecting, allows(.cancel) {
                Button(L("cancel"), systemImage: "xmark.circle") { issue(.cancel, false) }
                    .tint(Color.fdStatusWarning)
            }
        }
        .contextMenu {
            if !selecting { menuItems }
        }
        .alert(L("deleteTask"), isPresented: $confirmingDelete) {
            Button(L("deleteTask"), role: .destructive) { issue(.delete, false) }
            // 目标离线时无法让它删除文件，只能删记录：不提供「同时删除文件」。
            if targetOnline != false {
                Button(L("deleteTaskAndFile"), role: .destructive) { issue(.delete, true) }
            }
            Button(L("cancel"), role: .cancel) {}
        } message: {
            Text(displayName)
        }
    }

    @ViewBuilder
    private var menuItems: some View {
        if let action = RemoteTaskRules.primaryAction(for: task), allows(action) {
            Button(L(action == .pause ? "pause" : "resume"), systemImage: action == .pause ? FluxSymbol.pause : FluxSymbol.resume) {
                issue(action, false)
            }
        }
        if allows(.cancel) {
            Button(L("cancel"), systemImage: "xmark.circle") { issue(.cancel, false) }
        }
        if !task.url.isEmpty {
            Button(L("mobileSwipeCopyLink"), systemImage: FluxSymbol.link) {
                UIPasteboard.general.string = task.url
                container.toasts.show(text: L("urlCopied"), tone: .success)
            }
        }
        if allows(.delete) {
            Button(L("delete"), systemImage: FluxSymbol.delete, role: .destructive) { confirmingDelete = true }
        }
    }

    // MARK: 规则

    private func allows(_ action: RemoteCommandAction) -> Bool {
        !readOnly && !busy && RemoteTaskRules.canIssue(action, to: task, targetOnline: targetOnline)
    }

    /// 进行中 / 已暂停的任务因目标离线而不能暂停 / 继续。
    private var offlineBlocksPrimary: Bool {
        guard let action = RemoteTaskRules.primaryAction(for: task) else { return false }
        return !RemoteTaskRules.canIssue(action, to: task, targetOnline: targetOnline)
    }

    private var displayName: String { task.displayName }

    private var errorText: String? {
        guard let error = task.error?.trimmingCharacters(in: .whitespacesAndNewlines), !error.isEmpty else { return nil }
        return error
    }

    // MARK: 内容

    private var metaLine: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) {
                if showsTarget { targetLabel }
                RemoteStatusChip(status: task.status)
            }
            VStack(alignment: .leading, spacing: 4) {
                if showsTarget { targetLabel }
                RemoteStatusChip(status: task.status)
            }
        }
    }

    private var targetLabel: some View {
        Text(verbatim: "→ \(targetName)")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .lineLimit(1)
            .truncationMode(.middle)
    }

    private var progress: some View {
        ProgressView(value: min(max(task.progress, 0), 1))
            .progressViewStyle(.linear)
            .tint(progressTint)
            .fluxAnimation(.smooth, value: task.progress)
            .accessibilityHidden(true)
    }

    private var progressTint: Color {
        switch task.status {
        case .failed: .fdStatusFailed
        case .paused, .canceled: .fdStatusPaused
        case .completed: .fdStatusSeeding
        default: .accentColor
        }
    }

    /// 「百分比 · 已下载 / 总量 · 速度」；终态不显示速度。
    private var detailLine: some View {
        var parts = [Format.percent(min(max(task.progress, 0), 1))]
        let downloaded = Format.bytes(task.downloadedBytes)
        if let total = task.totalBytes, total > 0 {
            parts.append("\(downloaded) / \(Format.bytes(total))")
        } else {
            parts.append(downloaded.description)
        }
        if !task.status.isTerminal, let speed = Format.speed(task.speed) {
            parts.append(speed.description)
        }
        return Text(parts.joined(separator: " · "))
            .font(.footnote)
            .monospacedDigit()
            .foregroundStyle(.secondary)
            .lineLimit(2)
            .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder
    private var trailing: some View {
        if busy {
            ProgressView()
                .frame(width: 44, height: 44)
                .accessibilityLabel(L("mobileLoading"))
        } else if !selecting, let action = RemoteTaskRules.primaryAction(for: task) {
            Button {
                issue(action, false)
            } label: {
                Image(systemName: action == .pause ? FluxSymbol.pause : FluxSymbol.resume)
                    .font(.body)
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .disabled(readOnly || !RemoteTaskRules.canIssue(action, to: task, targetOnline: targetOnline))
            .accessibilityLabel(L(action == .pause ? "pause" : "resume"))
        }
    }
}

/// 远程任务状态徽标：文字 + 图标（不只靠颜色）；未知状态显示中性「未知」。
struct RemoteStatusChip: View {
    let status: RemoteTaskStatus

    var body: some View {
        switch status {
        case .pending, .accepted:
            StatusBadge(text: L("statusPending"), tone: .neutral, systemImage: "clock")
        case .downloading:
            StatusBadge(text: L("statusDownloading"), tone: .accent, systemImage: "arrow.down.circle")
        case .paused:
            StatusBadge(text: L("statusPaused"), tone: .neutral, systemImage: "pause.circle")
        case .completed:
            StatusBadge(text: L("statusCompleted"), tone: .success, systemImage: "checkmark.circle")
        case .failed:
            StatusBadge(text: L("statusError"), tone: .failure, systemImage: "exclamationmark.circle")
        case .canceled:
            StatusBadge(text: L("statusCanceled"), tone: .neutral, systemImage: "xmark.circle")
        case .unknown:
            StatusBadge(text: L("mobileRemoteStatusUnknown"), tone: .neutral, systemImage: "questionmark.circle")
        }
    }
}

extension RemoteTaskDto {
    /// 显示名：云端未带文件名时按 URL 推断（与新建下载同一规则）。
    var displayName: String {
        fileName.isEmpty ? inferName(UrlEntry(url: url)) : fileName
    }
}
