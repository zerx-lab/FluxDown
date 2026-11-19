import FluxDomain
import FluxUI
import SwiftUI

/// 表单内可聚焦的输入（数字键盘没有「完成」键，用键盘工具栏收起）。
nonisolated enum QueueEditorField: Hashable {
    case name, speed, upload, concurrent, segments, dir, userAgent
}

/// D8 队列表单（02-downloads §9.2）：新建（`queueId == nil`）与编辑共用，推入 D7 的 `NavigationStack`。
/// 保存 = 导航栏右上；保存期间整表单置灰并显示进度。断连只读：横幅 + 全部写入控件置灰。
struct QueueEditorView: View {
    let queueId: String?

    @Environment(AppContainer.self) private var container
    @Environment(\.dismiss) private var dismiss
    @State private var model: QueueEditorModel
    @State private var editMode: EditMode = .inactive
    @State private var confirmingDelete = false
    @State private var pickingDirectory = false
    @FocusState private var focus: QueueEditorField?

    init(queueId: String?) {
        self.queueId = queueId
        _model = State(initialValue: QueueEditorModel(queueId: queueId))
    }

    var body: some View {
        @Bindable var form = model
        let state = container.store.state
        let readOnly = state.isReadOnly
        let snapshot = queueId.flatMap { id in state.queues.first { $0.queueId == id } }
        let running = snapshot?.isRunning ?? model.detail?.isRunning ?? true
        let name = snapshot?.displayName ?? model.detail?.name ?? ""
        let pending = queueId.map {
            QueueOrdering.pending(in: $0, tasks: state.tasks, order: state.queueOrder)
        } ?? []
        Form {
            if readOnly || model.errorText != nil {
                Section {
                    VStack(spacing: 8) {
                        if readOnly {
                            Banner(text: L("localServiceDisconnected"), tone: .warning, systemImage: FluxSymbol.offline, slim: true)
                        }
                        if let text = model.errorText {
                            Banner(text: text, tone: .error, slim: true)
                        }
                    }
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
                }
            }
            switch model.loadState {
            case .loaded:
                Group {
                    nameSection(draft: $form.draft, running: running, displayName: name, issue: model.issue)
                    limitsSection(draft: $form.draft, issue: model.issue)
                    defaultsSection(draft: $form.draft)
                    if QueueIds.canSchedule(queueId ?? "") || model.isCreating {
                        QueueScheduleSection(draft: $form.draft, issue: model.issue)
                    } else {
                        Section { Text(L("mainQueueAlwaysRunningHint")).font(.footnote).foregroundStyle(.secondary) }
                    }
                    if !model.isCreating {
                        QueuePendingOrderSection(
                            serverTasks: pending,
                            override: model.orderOverride,
                            readOnly: readOnly,
                            loading: state.tasksLoading,
                            editMode: $editMode,
                            onReorder: { ids in Task { await model.reorder(ids, session: container.session) } }
                        )
                        actionsSection(running: running, displayName: name)
                    }
                }
                .disabled(readOnly || model.isSaving)
            case .loading, .missing:
                Section {
                    HStack(spacing: 12) {
                        ProgressView()
                        Text(L("mobileLoading")).foregroundStyle(.secondary)
                    }
                }
            case let .failed(text):
                Section {
                    QueueIssueText(text: text)
                    Button(L("mobileRetry"), systemImage: FluxSymbol.retry) {
                        Task { await model.reload(session: container.session) }
                    }
                }
            }
        }
        .environment(\.editMode, $editMode)
        .scrollDismissesKeyboard(.interactively)
        .navigationTitle(model.isCreating ? L("createQueueAction") : (name.isEmpty ? L("editQueue") : name))
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(model.isSaving)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if model.isSaving {
                    ProgressView()
                } else {
                    Button(L("queueSaveAction")) { Task { await save() } }
                        .disabled(readOnly || model.loadState != .loaded)
                }
            }
            ToolbarItemGroup(placement: .keyboard) {
                Spacer()
                Button(L("mobileViewDone")) { focus = nil }
            }
        }
        .alert(L("deleteQueueAction"), isPresented: $confirmingDelete) {
            Button(L("cancel"), role: .cancel) {}
            Button(L("delete"), role: .destructive) { Task { await delete(name: name) } }
        } message: {
            Text(L("queueDeleteConfirmDesc", ["name": name]))
        }
        .navigationDestination(isPresented: $pickingDirectory) {
            // 复用订阅的目录浏览（`daemon.fs.list`）：本机主机限定在 App 的 Documents 内，远端主机不限。
            RssDirectoryPicker(
                initialPath: model.draft.saveDir.trimmingCharacters(in: .whitespacesAndNewlines),
                rootPath: container.isLocalHost ? LocalPaths.documents.path : nil
            ) { path in
                model.draft.saveDir = path
            }
        }
        .task(id: state.queues) { await model.reload(session: container.session) }
        .onChange(of: model.loadState) { _, new in
            if new == .missing { dismiss() }
        }
        .onChange(of: pending.map(\.taskId)) { _, _ in model.orderOverride = nil }
        .onChange(of: model.draft) { _, _ in model.issue = nil }
        .sensoryFeedback(.selection, trigger: editMode.isEditing)
    }

    // MARK: 分区

    private func nameSection(draft: Binding<QueueDraft>, running: Bool, displayName: String, issue: QueueDraft.Issue?) -> some View {
        Section {
            if !model.isCreating {
                QueueStateBadge(running: running)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if model.isBuiltin {
                LabeledContent(L("queueNameLabel")) {
                    Text(displayName)
                }
            } else {
                QueueFormRow(
                    title: L("queueNameLabel"),
                    error: issue == .nameRequired ? L("queueNameRequired") : nil
                ) {
                    TextField(L("queueNameLabel"), text: draft.name, prompt: Text(verbatim: ""))
                        .focused($focus, equals: .name)
                        .submitLabel(.done)
                        .autocorrectionDisabled()
                }
            }
        } footer: {
            if model.isBuiltin { Text(L("builtinQueueRenameHint")) }
        }
    }

    private func limitsSection(draft: Binding<QueueDraft>, issue: QueueDraft.Issue?) -> some View {
        let speedUnit = SettingsRateUnit.kb.unitLabel(iec: container.appearance.fileSizeUnits == .iec)
        return Section {
            numberRow(L("queueDownloadLimitWithUnit", ["unit": speedUnit]), hint: L("queueSpeedLimitHint"), text: draft.speedLimit, field: .speed)
            numberRow(L("queueUploadLimitWithUnit", ["unit": speedUnit]), hint: L("queueUploadLimitDesc"), text: draft.uploadLimit, field: .upload)
            numberRow(L("queueMaxConcurrent"), hint: L("queueMaxConcurrentHint"), text: draft.maxConcurrent, field: .concurrent)
            numberRow(L("queueDefaultSegments"), hint: L("queueDefaultSegmentsHint"), text: draft.segments, field: .segments)
        } footer: {
            if issue == .invalidNumber { QueueIssueText(text: L("queueInvalidNumber")) }
        }
    }

    private func defaultsSection(draft: Binding<QueueDraft>) -> some View {
        Section {
            QueueFormRow(title: L("queueSaveDir"), hint: L("queueDirInheritHint"), stacked: true) {
                HStack(spacing: 8) {
                    TextField(L("queueSaveDir"), text: draft.saveDir, prompt: Text(verbatim: ""))
                        .font(.system(.body, design: .monospaced))
                        .focused($focus, equals: .dir)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button {
                        focus = nil
                        pickingDirectory = true
                    } label: {
                        Label(L("browse"), systemImage: FluxSymbol.folder)
                            .labelStyle(.iconOnly)
                            .frame(minWidth: 44, minHeight: 44)
                            .contentShape(.rect)
                    }
                    .buttonStyle(.borderless)
                }
            }
            QueueFormRow(title: L("queueDefaultUserAgent"), hint: L("queueUaHint"), stacked: true) {
                TextField(L("queueDefaultUserAgent"), text: draft.userAgent, prompt: Text(verbatim: ""))
                    .font(.system(.body, design: .monospaced))
                    .focused($focus, equals: .userAgent)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            }
        }
    }

    private func numberRow(_ title: String, hint: String, text: Binding<String>, field: QueueEditorField) -> some View {
        QueueFormRow(title: title, hint: hint) {
            TextField(title, text: text, prompt: Text(verbatim: "0"))
                .keyboardType(.numberPad)
                .monospacedDigit()
                .focused($focus, equals: field)
        }
    }

    @ViewBuilder
    private func actionsSection(running: Bool, displayName: String) -> some View {
        let canStop = QueueIds.canStop(queueId ?? "")
        Section {
            if !running || canStop {
                Button {
                    Task { await setRunning(!running, name: displayName) }
                } label: {
                    Label(
                        L(running ? "stopQueueAction" : "startQueueAction"),
                        systemImage: running ? FluxSymbol.pause : FluxSymbol.resume
                    )
                }
                .disabled(model.isWorking)
            }
            if !model.isBuiltin {
                Button(role: .destructive) {
                    confirmingDelete = true
                } label: {
                    Label(L("deleteQueueAction"), systemImage: FluxSymbol.delete)
                }
                .disabled(model.isWorking)
            }
        }
    }

    // MARK: 动作

    private func save() async {
        focus = nil
        guard !container.store.state.isReadOnly else {
            model.errorText = L("localServiceDisconnected")
            FluxHaptic.error.play()
            return
        }
        if await model.save(session: container.session) {
            container.toasts.show(text: L("queueSavedToast"), tone: .success, systemImage: FluxSymbol.done)
            dismiss()
        }
    }

    private func setRunning(_ running: Bool, name: String) async {
        if await model.setRunning(running, session: container.session) {
            container.toasts.show(
                text: QueueText.runningToast(running, name: name),
                tone: .info,
                systemImage: running ? FluxSymbol.resume : FluxSymbol.pause
            )
        }
    }

    private func delete(name: String) async {
        if await model.delete(session: container.session) {
            container.toasts.show(text: L("queueDeletedToast", ["name": name]), tone: .success, systemImage: FluxSymbol.delete)
            dismiss()
        }
    }
}

/// 表单行：标题 + 控件（常规字号并排，控件右对齐；AX 字号或 `stacked` 纵排）+ 可选提示 / 错误。
struct QueueFormRow<Control: View>: View {
    let title: String
    var hint: String?
    var error: String?
    var stacked = false
    @ViewBuilder var control: () -> Control

    @Environment(\.dynamicTypeSize) private var typeSize

    init(
        title: String,
        hint: String? = nil,
        error: String? = nil,
        stacked: Bool = false,
        @ViewBuilder control: @escaping () -> Control
    ) {
        self.title = title
        self.hint = hint
        self.error = error
        self.stacked = stacked
        self.control = control
    }

    var body: some View {
        let vertical = stacked || typeSize.isAccessibilitySize
        let layout = vertical
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 4))
            : AnyLayout(HStackLayout(alignment: .firstTextBaseline, spacing: 12))
        VStack(alignment: .leading, spacing: 4) {
            layout {
                Text(title)
                    .fixedSize(horizontal: false, vertical: true)
                    .layoutPriority(1)
                    .accessibilityHidden(true)
                control()
                    .multilineTextAlignment(vertical ? .leading : .trailing)
            }
            if let hint {
                Text(hint)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let error { QueueIssueText(text: error) }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
    }
}
