import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

/// BT 任务级限速（任务菜单「任务限速…」）：下载 / 上传两个 KiB/s 数值，0 = 不限。
/// 只下发改动的字段（`BtTaskLimitsDraft.params`）；保存成功后关闭。草稿以打开时的任务值为基线，
/// 主机侧的后续变化不覆盖正在编辑的文本。任务消失（被删 / 换主机）时自动关闭。
struct BtTaskLimitsSheet: View {
    let taskId: String

    @Environment(AppContainer.self) private var container
    @Environment(TaskActions.self) private var actions
    @Environment(\.dismiss) private var dismiss
    @State private var draft: BtTaskLimitsDraft?
    @State private var saving = false

    var body: some View {
        let task = container.store.state.task(taskId)
        NavigationStack {
            Form {
                if let task, draft != nil {
                    Section {
                        Text(task.fileName)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .foregroundStyle(.secondary)
                            .contextMenu { Text(task.fileName) }
                            .accessibilityLabel(task.fileName)
                    }
                    Section {
                        SeedLimitInputRow(
                            title: L("btTaskLimitDownload"),
                            text: binding(\.download),
                            isValid: draft?.isDownloadValid ?? true,
                            keyboard: .numberPad,
                            prompt: "0",
                            unit: L("statusSpeedLimitKbs")
                        )
                    } header: {
                        Text(L("btTaskLimitDownload"))
                    }
                    Section {
                        SeedLimitInputRow(
                            title: L("btTaskLimitUpload"),
                            text: binding(\.upload),
                            isValid: draft?.isUploadValid ?? true,
                            keyboard: .numberPad,
                            prompt: "0",
                            unit: L("statusSpeedLimitKbs")
                        )
                    } header: {
                        Text(L("btTaskLimitUpload"))
                    } footer: {
                        Text(L("btTaskLimitHint"))
                    }
                }
            }
            .disabled(container.store.state.isReadOnly || saving)
            .navigationTitle(L("btTaskLimitsTitle"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L("close"), systemImage: FluxSymbol.close) { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if saving {
                        ProgressView().accessibilityLabel(L("confirm"))
                    } else {
                        Button(L("confirm")) { save() }
                            .disabled(params == nil || container.store.state.isReadOnly)
                    }
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .onChange(of: task == nil, initial: true) { _, gone in
            if let task, draft == nil { draft = BtTaskLimitsDraft(task: task) }
            if gone && !container.store.state.tasksLoading { dismiss() }
        }
    }

    private var params: SetBtLimitsParams? { draft?.params(taskId: taskId) }

    private func binding(_ field: WritableKeyPath<BtTaskLimitsDraft, String>) -> Binding<String> {
        Binding(
            get: { draft?[keyPath: field] ?? "" },
            set: { draft?[keyPath: field] = $0 }
        )
    }

    private func save() {
        guard let params, !saving else { return }
        saving = true
        actions.submitBtLimits(params) { saved in
            saving = false
            if saved { dismiss() }
        }
    }
}
#endif
