import FluxUI
import SwiftUI

#if FULL_FLAVOR
struct PluginFeedbackSheet: View {
    let identity: String
    let version: String
    let loadStatus: String
    let allowsPluginReport: Bool

    @Environment(ExtensionsModel.self) private var model
    @Environment(AppContainer.self) private var container
    @Environment(\.dismiss) private var dismiss
    @State private var route: Route = .loading
    @State private var title = ""
    @State private var description = ""
    @State private var includeDiagnostics = false
    @State private var auditReviewed = false
    @State private var auditCounts: (error: Int, denied: Int)?
    @State private var submitting = false
    @State private var failure: PluginFeedbackHTTP.Failure?
    @State private var issueNumber: Int?

    private enum Route {
        case loading, ready(String), failed(PluginFeedbackHTTP.Failure)
    }

    private var canLookUp: Bool {
        allowsPluginReport && !identity.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !version.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var diagnosticEnvironment: String {
        var fields: [String] = []
        if container.isLocalHost {
            let os = ProcessInfo.processInfo.operatingSystemVersion
            fields.append("platform=iOS; osVersion=\(os.majorVersion).\(os.minorVersion).\(os.patchVersion)")
        }
        let status = switch loadStatus {
        case "Loaded": "loaded"
        case "Failed": "failed"
        default: "unknown"
        }
        fields.append("Plugin load: \(status)")
        if let auditCounts {
            fields.append("Recent audit: \(auditCounts.error) errors, \(auditCounts.denied) denied")
        }
        return fields.joined(separator: "; ")
    }

    private func loadDiagnostics() async {
        guard includeDiagnostics, !auditReviewed else { return }
        let counts = await model.feedbackAuditCounts(identity: identity)
        guard !Task.isCancelled else { return }
        auditCounts = counts
        auditReviewed = true
    }

    var body: some View {
        NavigationStack {
            Form {
                if let issueNumber {
                    Section {
                        Text(L("plugFeedbackSent", ["number": String(issueNumber)]))
                    }
                } else {
                    if canLookUp {
                        Section { Text(L("plugFeedbackLookupNotice")) }
                    }
                    routeSection
                    if let failure {
                        Section { Text(L(failure.messageKey)) }
                    }
                    if case let .ready(repository) = route {
                        reportForm(repository: repository)
                    }
                    Section {
                        Link(L("plugFeedbackFallback"), destination: PluginFeedbackHTTP.fallback)
                    }
                    .disabled(submitting)
                }
            }
            .navigationTitle(L("plugFeedbackTitle"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L("close")) { dismiss() }.disabled(submitting)
                }
            }
        }
        .interactiveDismissDisabled(submitting)
        .task { await loadRoute() }
    }

    private var routeSection: some View {
        Section {
            switch route {
            case .loading:
                ProgressView()
            case let .ready(repository):
                LabeledContent {
                    Text(verbatim: repository).textSelection(.enabled)
                } label: {
                    Text(L("plugFeedbackDestination"))
                }
            case let .failed(error):
                Text(L(error.messageKey))
                if canLookUp, error != .unavailable {
                    Button(L("mobileRetry")) { Task { await loadRoute() } }
                }
            }
        } footer: {
            if case .ready = route {
                Text(L("plugFeedbackPublic"))
            }
        }
    }

    @ViewBuilder
    private func reportForm(repository: String) -> some View {
        let appVersion = SettingsAppVersion.current
        let titleValue = title.trimmingCharacters(in: .whitespacesAndNewlines)
        let descriptionValue = description.trimmingCharacters(in: .whitespacesAndNewlines)
        let reviewedEnvironment = includeDiagnostics ? diagnosticEnvironment : nil
        Section {
            LabeledContent(L("feedbackTypeLabel"), value: L("feedbackTypeBug"))
            LabeledContent(L("pluginDetailIdentity")) {
                Text(verbatim: identity).textSelection(.enabled)
            }
            LabeledContent(L("plugFeedbackPluginVersion")) {
                Text(verbatim: version).textSelection(.enabled)
            }
            LabeledContent(L("feedbackVersionLabel"), value: appVersion)
            if container.isLocalHost { LabeledContent(L("feedbackSysInfoSystem"), value: "iOS") }
            VStack(alignment: .leading) {
                Text(L("feedbackTitleLabel"))
                TextField(L("feedbackTitlePlaceholder"), text: $title)
                Text(L("feedbackTitleCount", ["n": String(titleValue.utf16.count)]))
                    .font(.caption).foregroundStyle(.secondary)
            }
            VStack(alignment: .leading) {
                Text(L("feedbackDescLabel"))
                TextField(L("feedbackDescPlaceholder"), text: $description, axis: .vertical)
                    .lineLimit(3...8)
                Text(L("feedbackDescCount", ["n": String(descriptionValue.utf16.count)]))
                    .font(.caption).foregroundStyle(.secondary)
            }
            Toggle(L("plugFeedbackDiagnostics"), isOn: Binding(
                get: { includeDiagnostics },
                set: { enabled in
                    auditReviewed = false
                    auditCounts = nil
                    includeDiagnostics = enabled
                }
            ))
            .task(id: includeDiagnostics) { await loadDiagnostics() }
            if includeDiagnostics {
                if auditReviewed {
                    if let reviewedEnvironment {
                        Text(verbatim: reviewedEnvironment).font(.footnote).textSelection(.enabled)
                    }
                    if auditCounts == nil {
                        Text(L("plugFeedbackAuditUnavailable")).font(.footnote)
                    }
                } else {
                    ProgressView()
                }
            }
        }
        .disabled(submitting)
        Section {
            Button(submitting ? L("feedbackSubmitting") : L("plugFeedbackSend")) {
                guard !submitting else { return }
                let report = PluginFeedbackHTTP.Report(
                    title: titleValue, description: descriptionValue, appVersion: appVersion,
                    environment: reviewedEnvironment ?? (container.isLocalHost ? "iOS" : nil),
                    pluginIdentity: identity, pluginVersion: version, repository: repository
                )
                submitting = true
                Task { await submit(report) }
            }
            .disabled(submitting || (includeDiagnostics && !auditReviewed)
                || titleValue.isEmpty || titleValue.utf16.count > 200
                || descriptionValue.isEmpty || descriptionValue.utf16.count > 5000)
        }
    }

    private func loadRoute() async {
        guard canLookUp else {
            route = .failed(.unavailable)
            return
        }
        route = .loading
        do {
            route = .ready(try await PluginFeedbackHTTP.repository(for: identity))
        } catch let error as PluginFeedbackHTTP.Failure {
            route = .failed(error)
        } catch {
            route = .failed(.failed)
        }
    }

    private func submit(_ report: PluginFeedbackHTTP.Report) async {
        defer { submitting = false }
        failure = nil
        do {
            issueNumber = try await PluginFeedbackHTTP.submit(report)
        } catch PluginFeedbackHTTP.Failure.routeChanged {
            failure = .routeChanged
            await loadRoute()
        } catch PluginFeedbackHTTP.Failure.unavailable {
            route = .failed(.unavailable)
        } catch let error as PluginFeedbackHTTP.Failure {
            failure = error
        } catch {
            failure = .unconfirmed
        }
    }
}
#endif
