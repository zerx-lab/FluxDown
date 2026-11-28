import FluxDomain
import FluxUI
import SwiftUI

struct CaptureDeviceRulesSection: View {
    @Environment(AppContainer.self) private var container
    @Environment(ConfigEditor.self) private var editor
    @State private var draft: Draft?
    @State private var sections = AgentSections()

    private struct Draft: Identifiable {
        let id = UUID()
        var index: Int?
        var suffix = ""
        var targetId = ""
    }

    private var rules: [CaptureDeviceRule] {
        CaptureDeviceRule.parse(editor.form.string(CaptureDeviceRule.preferenceKey))
    }

    var body: some View {
        let targets = container.store.state.downloadDispatchTargets(sections: sections)
        let current = rules
        Section {
            SettingsText(title: L("captureDeviceRules"), detail: L("captureDeviceRulesDesc"), synced: false)
                .settingsRow(CaptureDeviceRule.preferenceKey, failureKey: CaptureDeviceRule.preferenceKey)
            ForEach(current.indices, id: \.self) { index in
                let rule = current[index]
                Button {
                    draft = Draft(index: index, suffix: rule.suffix, targetId: rule.targetId)
                } label: {
                    LabeledContent("." + rule.suffix) {
                        Text(deviceName(rule.targetId, in: targets))
                            .truncationMode(.middle)
                    }
                    .font(.callout)
                    .lineLimit(1)
                }
                .swipeActions {
                    Button(L("delete"), systemImage: FluxSymbol.delete, role: .destructive) { delete(at: index) }
                }
            }
            .onMove { source, destination in
                var next = rules
                next.move(fromOffsets: source, toOffset: destination)
                write(next)
            }
            Button(L("captureDeviceRuleAdd"), systemImage: "plus") {
                draft = Draft()
            }
            .disabled(targets.isEmpty)
        } footer: {
            if targets.isEmpty { Text(L("captureDeviceRuleNoDevices")) }
        }
        .disabled(editor.isReadOnly)
        .sheet(item: $draft) { editing in
            ruleEditor(editing, targets: targets)
        }
    }

    private func ruleEditor(_ editing: Draft, targets: [DispatchTarget]) -> some View {
        CaptureDeviceRuleEditor(
            editing: editing.index != nil, initialSuffix: editing.suffix, initialTarget: editing.targetId,
            targets: targets, onCancel: { draft = nil }, onSave: { suffix, target in
                var next = rules
                let rule = CaptureDeviceRule(suffix: suffix, targetId: target)
                if let index = editing.index {
                    guard next.indices.contains(index) else { return }
                    next[index] = rule
                } else {
                    next.append(rule)
                }
                write(next)
                draft = nil
            }, onDelete: editing.index.map { index in { delete(at: index); draft = nil } }
        )
    }

    private func deviceName(_ id: String, in targets: [DispatchTarget]) -> String {
        DeviceRules.resolveTarget(id: id, in: targets)?.name ?? L("marketUnavailableButton")
    }

    private func delete(at index: Int) {
        var next = rules
        guard next.indices.contains(index) else { return }
        next.remove(at: index)
        write(next)
    }

    private func write(_ rules: [CaptureDeviceRule]) {
        editor.set(CaptureDeviceRule.preferenceKey, CaptureDeviceRule.serialize(rules), immediate: true)
    }
}

private struct CaptureDeviceRuleEditor: View {
    @State private var suffix: String
    @State private var targetId: String
    let editing: Bool
    let targets: [DispatchTarget]
    let onCancel: () -> Void
    let onSave: (String, String) -> Void
    let onDelete: (() -> Void)?

    init(
        editing: Bool, initialSuffix: String, initialTarget: String, targets: [DispatchTarget],
        onCancel: @escaping () -> Void, onSave: @escaping (String, String) -> Void, onDelete: (() -> Void)?
    ) {
        _suffix = State(initialValue: initialSuffix)
        _targetId = State(initialValue: initialTarget.isEmpty ? targets.first?.id ?? "" : initialTarget)
        self.editing = editing
        self.targets = targets
        self.onCancel = onCancel
        self.onSave = onSave
        self.onDelete = onDelete
    }

    var body: some View {
        let normalized = CaptureDeviceRule.normalizeSuffix(suffix)
        NavigationStack {
            Form {
                Section {
                    TextField(L("captureDeviceRuleExtension"), text: $suffix, prompt: Text(verbatim: "apk"))
                        .font(.callout.monospaced())
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } footer: {
                    if !suffix.isEmpty, normalized == nil { Text(L("captureDeviceRuleExtensionInvalid")) }
                }
                Picker(L("captureDeviceRuleTarget"), selection: $targetId) {
                    if !targets.contains(where: { $0.id == targetId }) {
                        let name = DeviceRules.resolveTarget(id: targetId, in: targets)?.name ?? L("marketUnavailableButton")
                        Text(name).tag(targetId)
                    }
                    ForEach(targets) { target in
                        Text(target.name).lineLimit(1).truncationMode(.middle).tag(target.id)
                    }
                }
                if let onDelete {
                    Button(L("delete"), role: .destructive, action: onDelete)
                }
            }
            .navigationTitle(L(editing ? "captureDeviceRuleEdit" : "captureDeviceRuleAdd"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L("cancel"), action: onCancel)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(L("proxyRuleSave")) {
                        guard let normalized, !targetId.isEmpty else { return }
                        onSave(normalized, targetId)
                    }
                    .disabled(normalized == nil || targetId.isEmpty)
                }
            }
        }
    }
}
