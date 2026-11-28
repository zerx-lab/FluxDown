import FluxDomain
import FluxUI
import SwiftUI

struct SiteUserAgentRulesRows: View {
    let presets: [ConfigPickerRow.Option]

    @Environment(AppContainer.self) private var container
    @Environment(ConfigEditor.self) private var editor
    @State private var target: EditTarget?
    @State private var writeError: String?
    @State private var isWriting = false

    private let row = SettingsDownloadRow.siteUserAgentRules

    private nonisolated struct EditTarget: Identifiable {
        let id = UUID()
        let index: Int?
        let rules: [SiteUserAgentRule]
        let raw: String
    }

    var body: some View {
        let raw = editor.form.string(row.configKey)
        let rules = try? SiteUserAgentRule.decodeList(raw)
        Section {
            SettingsText(title: L(row.titleKey), detail: L(row.detailKey))
                .settingsRow(row.id, failureKey: row.configKey)
            if let rules {
                ForEach(rules.indices, id: \.self) { index in
                    ruleRow(rules, at: index, raw: raw)
                }
                SettingsActionRow(title: L("siteUserAgentAdd"), systemImage: FluxSymbol.add) {
                    target = EditTarget(index: nil, rules: rules, raw: raw)
                }
                .disabled(rules.count >= SiteUserAgentRule.maxCount)
            }
            if let message = rules == nil ? L("siteUserAgentInvalid") : writeError {
                SettingsStatusLine(text: message, tone: .failure)
            }
        }
        .disabled(editor.isReadOnly || isWriting)
        .sheet(item: $target) { target in
            SiteUserAgentRuleSheet(
                rule: target.index.map { target.rules[$0] } ?? SiteUserAgentRule(),
                isNew: target.index == nil,
                presets: presets
            ) { updated in
                var next = target.rules
                if let index = target.index { next[index] = updated } else { next.append(updated) }
                return await save(next, replacing: target.raw)
            }
        }
    }

    private func ruleRow(_ rules: [SiteUserAgentRule], at index: Int, raw: String) -> some View {
        let rule = rules[index]
        return HStack(spacing: 12) {
            Button {
                target = EditTarget(index: index, rules: rules, raw: raw)
            } label: {
                VStack(alignment: .leading, spacing: 4) {
                    Text(rule.domain)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if rule.includeSubdomains {
                        Text(L("siteUserAgentSubdomains"))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Text(rule.userAgent)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
                .frame(minHeight: 44)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)

            Menu {
                Button(L("siteUserAgentMoveUp"), systemImage: "arrow.up") {
                    move(rules, from: index, to: index - 1, raw: raw)
                }
                .disabled(index == rules.startIndex)
                Button(L("siteUserAgentMoveDown"), systemImage: "arrow.down") {
                    move(rules, from: index, to: index + 1, raw: raw)
                }
                .disabled(index + 1 == rules.endIndex)
                Button(L("delete"), systemImage: FluxSymbol.delete, role: .destructive) {
                    var next = rules
                    next.remove(at: index)
                    write(next, replacing: raw)
                }
            } label: {
                Image(systemName: "ellipsis.circle")
                    .frame(minWidth: 44, minHeight: 44)
            }
            .buttonStyle(.borderless)
            .accessibilityLabel(L("siteUserAgentRules"))
        }
    }

    private func move(_ rules: [SiteUserAgentRule], from: Int, to: Int, raw: String) {
        guard rules.indices.contains(to) else { return }
        var next = rules
        next.swapAt(from, to)
        write(next, replacing: raw)
    }

    private func write(_ rules: [SiteUserAgentRule], replacing raw: String) {
        guard !isWriting else { return }
        isWriting = true
        writeError = nil
        Task {
            defer { isWriting = false }
            writeError = await save(rules, replacing: raw)
        }
    }

    private func save(_ rules: [SiteUserAgentRule], replacing raw: String) async -> String? {
        do throws(HostError) {
            try await SiteUserAgentRule.replaceList(rules, replacing: raw, on: ContainerDaemonConfigPort(container))
            return nil
        } catch {
            return error.code == .invalidArgument ? L("siteUserAgentInvalid") : ErrorText.describe(error)
        }
    }
}

private struct SiteUserAgentRuleSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(ConfigEditor.self) private var editor
    @State private var rule: SiteUserAgentRule
    @State private var writeError: String?
    @State private var isSaving = false
    @State private var forceCustom = false

    let isNew: Bool
    let presets: [ConfigPickerRow.Option]
    let onSave: (SiteUserAgentRule) async -> String?

    init(rule: SiteUserAgentRule, isNew: Bool, presets: [ConfigPickerRow.Option], onSave: @escaping (SiteUserAgentRule) async -> String?) {
        _rule = State(initialValue: rule)
        self.isNew = isNew
        self.presets = presets
        self.onSave = onSave
    }

    private var canSave: Bool {
        !isSaving && !editor.isReadOnly && !rule.domain.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !rule.userAgent.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(L("siteUserAgentDomain"), text: $rule.domain)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                    Toggle(L("siteUserAgentSubdomains"), isOn: $rule.includeSubdomains)
                        .tint(Color.fdToggleOn)
                }
                Section(L("userAgent")) {
                    Picker(L("userAgent"), selection: Binding(
                        get: { !forceCustom && presets.contains { $0.id == rule.userAgent } ? rule.userAgent : "" },
                        set: { value in
                            forceCustom = value.isEmpty
                            if !forceCustom { rule.userAgent = value }
                        }
                    )) {
                        Text(L("userAgentPresetCustom")).tag("")
                        ForEach(presets) { Text($0.label).tag($0.id) }
                    }
                    TextField(L("userAgent"), text: $rule.userAgent, axis: .vertical)
                        .lineLimit(2 ... 4)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }
                if let writeError {
                    SettingsStatusLine(text: writeError, tone: .failure)
                }
            }
            .disabled(isSaving)
            .readableContentWidth()
            .navigationTitle(L(isNew ? "siteUserAgentAdd" : "siteUserAgentEdit"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L("cancel")) { dismiss() }
                    .disabled(isSaving)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(L("proxyRuleSave")) {
                        guard canSave else { return }
                        isSaving = true
                        writeError = nil
                        Task {
                            defer { isSaving = false }
                            if let message = await onSave(rule) {
                                writeError = message
                            } else {
                                dismiss()
                            }
                        }
                    }
                    .disabled(!canSave)
                }
            }
        }
        .interactiveDismissDisabled(isSaving)
    }
}
