import FluxDomain
import FluxUI
import SwiftUI
import UIKit
#if FULL_FLAVOR

/// BT 任务「文件」页（详情「常规」页的入口推入）：种子内每个文件的名称、大小、进度与优先级。
/// 页面可见且 App 在前台时每 2 秒轮询 `daemon.task.btFiles`，离开 / 退到后台 / 换主机即停（同防吸血页轮询）。
/// 优先级改动经 `daemon.task.setFilePriorities` 只发变化的文件；会让任务没有任何要下载的文件时本地拦下并提示。
/// `editable == false`（已完成 / 尚无元数据）时只读。`List` 按需构建行，数千个文件也只渲染可见部分。
struct BtFilesScreen: View {
    let taskId: String

    @Environment(AppContainer.self) private var container
    @Environment(TaskActions.self) private var actions
    @Environment(\.scenePhase) private var scenePhase
    @State private var store = BtFilesStore()

    private struct PollKey: Hashable {
        let taskId: String
        let hostId: String
        let active: Bool
    }

    var body: some View {
        content
            .navigationTitle(L("btTabFiles"))
            .navigationBarTitleDisplayMode(.inline)
            .task(id: PollKey(taskId: taskId, hostId: container.host.id, active: scenePhase == .active)) {
                guard scenePhase == .active else { return }
                await store.poll(taskId: taskId, container.session)
            }
    }

    @ViewBuilder private var content: some View {
        if let files = store.files {
            if files.files.isEmpty {
                ContentUnavailableView {
                    Label(L("btTabFiles"), systemImage: "doc.on.doc")
                } description: {
                    Text(L("btFilesEmpty"))
                }
            } else {
                List {
                    Section {
                        ForEach(files.files) { file in
                            BtFileRow(
                                file: file,
                                editable: files.editable && !container.store.state.isReadOnly && !store.isSaving,
                                onChange: { change(file, to: $0, in: files.files) }
                            )
                        }
                    }
                }
                .listStyle(.insetGrouped)
            }
        } else if let error = store.error {
            ContentUnavailableView {
                Label(L("btTabFiles"), systemImage: FluxSymbol.failure)
            } description: {
                Text(ErrorText.describe(error))
            } actions: {
                Button(L("detailActivityRetry")) { reload() }
            }
        } else {
            ProgressView()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityLabel(L("pluginCommonLoading"))
        }
    }

    private func change(_ file: BtTaskFile, to level: BtFilePriority, in files: [BtTaskFile]) {
        switch BtFilesRules.plan(files, indices: [file.index], to: level) {
        case .unchanged:
            return
        case .keepOne:
            container.toasts.show(text: L("btFilesKeepOne"), tone: .warning, systemImage: "exclamationmark.triangle")
        case let .send(changes):
            guard actions.guardWritable() else { return }
            let session = container.session
            let toasts = container.toasts
            let id = taskId
            Task {
                do throws(HostError) {
                    try await store.setPriorities(taskId: id, changes, session)
                } catch {
                    toasts.show(text: ErrorText.describe(error), tone: .error, systemImage: "exclamationmark.circle")
                }
            }
        }
    }

    private func reload() {
        let session = container.session
        let id = taskId
        Task { await store.load(taskId: id, session) }
    }
}

/// 一行文件：文件名单行中间省略（保留扩展名），所在子目录在下方同样中间省略；长按看全路径并可复制。
/// 读数：已下载 / 大小 · 百分比 + 细进度条；优先级在行尾（只读时为文字）。
private struct BtFileRow: View {
    let file: BtTaskFile
    let editable: Bool
    let onChange: (BtFilePriority) -> Void

    @Environment(AppContainer.self) private var container

    var body: some View {
        let parts = BtFilesRules.split(file.path)
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(verbatim: parts.name)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .foregroundStyle(file.level == .skip ? .secondary : .primary)
                if !parts.dir.isEmpty {
                    Text(verbatim: parts.dir)
                        .font(.caption)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .foregroundStyle(.secondary)
                }
                Text(verbatim: "\(BtFilesRules.sizeText(file)) · \(Format.percent(file.progress))")
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                ProgressView(value: file.progress)
                    .tint(file.progress >= 1 ? Color.fdStatusSeedingText : Color.accentColor)
                    .accessibilityHidden(true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            priority
                .fixedSize()
        }
        .padding(.vertical, 2)
        .contextMenu {
            Text(verbatim: file.path)
            Button(L("webCopy"), systemImage: FluxSymbol.copy) {
                UIPasteboard.general.string = file.path
                container.toasts.show(text: L("mobilePathCopied"), tone: .success, systemImage: FluxSymbol.copy)
            }
        }
    }

    @ViewBuilder private var priority: some View {
        if editable {
            Menu {
                Picker(L("btFileColumnPriority"), selection: Binding(get: { file.level }, set: onChange)) {
                    ForEach(BtFilePriority.allCases) { level in
                        Text(L(level.titleKey)).tag(level)
                    }
                }
            } label: {
                Text(L(file.level.titleKey))
                    .font(.subheadline)
                    .lineLimit(1)
            }
            .accessibilityLabel(L("btFileColumnPriority"))
            .accessibilityValue(L(file.level.titleKey))
        } else {
            Text(L(file.level.titleKey))
                .font(.subheadline)
                .lineLimit(1)
                .foregroundStyle(.secondary)
        }
    }
}
#endif
