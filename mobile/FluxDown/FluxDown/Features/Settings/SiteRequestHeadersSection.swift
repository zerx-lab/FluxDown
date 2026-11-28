import FluxDomain
import FluxUI
import Foundation
import SwiftUI

nonisolated struct SiteHeaderRule: Codable, Equatable {
    static let refererName = "Referer"

    let origin: String
    let headers: [String: String]

    var referer: String? {
        headers.first { $0.key.caseInsensitiveCompare(Self.refererName) == .orderedSame }?.value
    }

    var extra: String {
        headers.sorted { $0.key < $1.key }
            .filter { $0.key.caseInsensitiveCompare(Self.refererName) != .orderedSame }
            .map { "\($0.key): \($0.value)" }
            .joined(separator: "\n")
    }

    static func parse(_ raw: String?) -> [Self]? {
        guard let raw else { return nil }
        if raw.isEmpty { return [] }
        return try? JSONDecoder().decode([Self].self, from: Data(raw.utf8))
    }

    init?(origin: String, referer: String, extra: String) {
        var headers: [String: String] = referer.isEmpty ? [:] : [Self.refererName: referer]
        for line in extra.components(separatedBy: "\n") {
            if line.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { continue }
            guard let colon = line.firstIndex(of: ":"), colon != line.startIndex else { return nil }
            let name = line[..<colon].trimmingCharacters(in: .whitespaces)
            guard !name.isEmpty,
                  name.caseInsensitiveCompare(Self.refererName) != .orderedSame,
                  !headers.keys.contains(where: { $0.caseInsensitiveCompare(name) == .orderedSame }) else { return nil }
            let value = line[line.index(after: colon)...]
            headers[name] = String(value.first == " " ? value.dropFirst() : value)
        }
        self.origin = origin
        self.headers = headers
    }
}

private struct SiteHeaderTarget: Identifiable {
    let id = UUID()
    let existing: SiteHeaderRule?
    let session: any HostSession
}

struct SiteRequestHeadersSection: View {
    let raw: String?

    @Environment(AppContainer.self) private var container
    @State private var target: SiteHeaderTarget?

    var body: some View {
        Section {
            if let rules = SiteHeaderRule.parse(raw) {
                ForEach(rules, id: \.origin) { rule in
                    Button {
                        target = SiteHeaderTarget(existing: rule, session: container.session)
                    } label: {
                        Text(rule.origin)
                            .foregroundStyle(.primary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                            .contentShape(.rect)
                    }
                }
                SettingsActionRow(title: L("siteRequestHeadersAdd"), systemImage: FluxSymbol.add) {
                    target = SiteHeaderTarget(existing: nil, session: container.session)
                }
                .settingsRow(SettingsDownloadRow.siteRequestHeaders.id)
            } else {
                Text(L("siteRequestHeadersInvalid"))
                    .foregroundStyle(.red)
                    .settingsRow(SettingsDownloadRow.siteRequestHeaders.id)
            }
        } header: {
            Text(L("siteRequestHeadersTitle"))
        } footer: {
            Text(L("siteRequestHeadersDesc"))
        }
        .onChange(of: container.host.id) { _, _ in target = nil }
        .sheet(item: $target) { target in
            SiteHeaderEditor(target: target)
        }
    }
}

private struct SiteHeaderEditor: View {
    @Environment(HostStore.self) private var store
    @Environment(\.dismiss) private var dismiss

    private let existing: SiteHeaderRule?
    private let writer: DaemonConfigListWriter<SiteHeaderRule?>
    @State private var origin: String
    @State private var referer: String
    @State private var extra: String
    @State private var error: String?
    @State private var busy = false
    @State private var confirmDelete = false

    init(target: SiteHeaderTarget) {
        let existing = target.existing
        self.existing = existing
        _origin = State(initialValue: existing?.origin ?? "")
        _referer = State(initialValue: existing?.referer ?? "")
        _extra = State(initialValue: existing?.extra ?? "")
        writer = DaemonConfigListWriter(
            port: ContainerDaemonConfigPort(session: target.session),
            key: SettingsDownloadRow.siteRequestHeaders.configKey,
            parse: { SiteHeaderRule.parse($0)?.map(Optional.some) ?? [nil] },
            encode: { rules throws(HostError) in
                let valid = rules.compactMap { $0 }
                guard valid.count == rules.count else {
                    throw HostError(.invalidArgument, message: L("siteRequestHeadersInvalid"))
                }
                if valid.isEmpty { return "" }
                do {
                    return String(decoding: try JSONEncoder().encode(valid), as: UTF8.self)
                } catch {
                    throw HostError(.invalidArgument, message: L("siteRequestHeadersInvalid"))
                }
            }
        )
    }

    private var disabled: Bool { busy || store.state.isReadOnly }

    var body: some View {
        NavigationStack {
            Form {
                Section(L("siteRequestHeadersOrigin")) {
                    TextField(L("siteRequestHeadersOrigin"), text: $origin)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }
                .disabled(disabled)
                Section(L("siteRequestHeadersReferer")) {
                    TextField(L("siteRequestHeadersReferer"), text: $referer)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }
                .disabled(disabled)
                Section {
                    TextEditor(text: $extra)
                        .font(.body.monospaced())
                        .frame(minHeight: 96)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityLabel(L("siteRequestHeadersExtra"))
                } header: {
                    Text(L("siteRequestHeadersExtra"))
                } footer: {
                    Text(L("siteRequestHeadersExtraHint"))
                }
                .disabled(disabled)
                if let error {
                    Section { Text(error).foregroundStyle(.red) }
                }
                if existing != nil {
                    Section {
                        Button(L("delete"), role: .destructive) { confirmDelete = true }
                    }
                    .disabled(disabled)
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle(L(existing == nil ? "siteRequestHeadersAdd" : "siteRequestHeadersTitle"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L("cancel")) { dismiss() }.disabled(busy)
                }
                ToolbarItem(placement: .confirmationAction) {
                    if busy {
                        ProgressView()
                    } else {
                        Button(L("proxyRuleSave"), action: save).disabled(disabled)
                    }
                }
            }
            .alert(L("delete"), isPresented: $confirmDelete) {
                Button(L("delete"), role: .destructive) { write(nil) }
                Button(L("cancel"), role: .cancel) {}
            } message: {
                Text(existing?.origin ?? "")
            }
        }
        .presentationDetents([.large])
        .interactiveDismissDisabled(busy)
    }

    private func save() {
        guard let draft = SiteHeaderRule(origin: origin, referer: referer, extra: extra) else {
            error = L("siteRequestHeadersInvalid")
            return
        }
        write(draft)
    }

    private func write(_ replacement: SiteHeaderRule?) {
        guard !disabled else { return }
        busy = true
        error = nil
        Task {
            defer { busy = false }
            do throws(HostError) {
                var changedElsewhere = false
                try await writer.mutate { rules in
                    // Preserve malformed data rather than writing a partial list over it.
                    if rules.contains(nil) { return rules }
                    guard let existing else { return rules + [replacement] }
                    guard let index = rules.firstIndex(of: existing) else {
                        changedElsewhere = true
                        return nil
                    }
                    var next = rules
                    if let replacement { next[index] = replacement } else { next.remove(at: index) }
                    return next
                }
                if changedElsewhere { throw HostError(.conflict) }
                dismiss()
            } catch {
                self.error = WebhookErrorText.write(error)
            }
        }
    }
}
