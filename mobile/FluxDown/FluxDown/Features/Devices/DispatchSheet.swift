import FluxDomain
import FluxUI
import Foundation
import Observation
import SwiftUI

/// 一次下发（可能多条链接）的结果汇总（对齐 GPUI `DispatchSummary`）：成功数、失败数、第一条错误。
struct DispatchSummary {
    var successes = 0
    var failures = 0
    var firstError: HostError?
    var failedEntries: [UrlEntry] = []
}

/// 下发的一条链接；`fileName` 为 nil 时由目标设备按 URL 推断。
struct DispatchItem {
    var entry: UrlEntry
    var fileName: String?
}

/// 逐条下发并汇总（设备页下发表单与新建下载「下载到」共用）：顺序执行，避免一次性压垮对端。
/// 目标只接收链接 / 文件名 / 保存目录；`saveDir` 为 nil = 目标设备默认目录。
@MainActor
enum Dispatcher {
    static func run(
        _ items: [DispatchItem],
        to target: DispatchTarget,
        saveDir: String?,
        agent: AgentAPI,
        progress: (Int) -> Void = { _ in }
    ) async -> DispatchSummary {
        var summary = DispatchSummary()
        for (index, item) in items.enumerated() {
            do throws(HostError) {
                try await dispatch(item, to: target, saveDir: saveDir, agent: agent)
                summary.successes += 1
            } catch {
                summary.failures += 1
                summary.failedEntries.append(item.entry)
                if summary.firstError == nil { summary.firstError = error }
            }
            progress(index + 1)
        }
        return summary
    }

    private static func dispatch(
        _ item: DispatchItem,
        to target: DispatchTarget,
        saveDir: String?,
        agent: AgentAPI
    ) async throws(HostError) {
        let url = item.entry.url
        // 独立直连条目 / 挂接直连在线的云条目走局域网直连（一次性投递），否则走云。
        if let fingerprint = target.dispatchFingerprint {
            _ = try await agent.linkDispatch(
                LinkDispatchParams(fingerprint: fingerprint, url: url, fileName: item.fileName, saveDir: saveDir)
            )
        } else {
            _ = try await agent.remoteDispatch(
                RemoteDispatchParams(toDevice: target.deviceId, url: url, fileName: item.fileName, saveDir: saveDir)
            )
        }
    }

    /// 结果提示：全部成功 → 成功 Toast 并返回 nil；否则返回内联失败说明（部分失败另出警告 Toast）。
    static func announce(_ summary: DispatchSummary, target: DispatchTarget, toasts: ToastCenter) -> String? {
        let device = target.name
        if summary.failures == 0 {
            let text: String
            if target.routesViaLink {
                text = L("linkDispatchedToDevice", ["device": device])
            } else if target.isCloud, target.isOffline {
                text = L("downloadToDispatchedOffline", ["count": summary.successes, "device": device])
            } else if summary.successes == 1 {
                text = L("dispatchedToDevice", ["device": device])
            } else {
                text = L("downloadToDispatched", ["count": summary.successes, "device": device])
            }
            toasts.show(text: text, tone: .success)
            FluxHaptic.success.play()
            return nil
        }
        let reason = summary.firstError.map { AccountText.error($0, context: target.routesViaLink ? .pairing : .general) }
            ?? L("dispatchFailed")
        if summary.successes > 0 {
            let partial = L(
                "downloadToPartial",
                ["ok": summary.successes, "failed": summary.failures, "device": device]
            )
            toasts.show(text: partial, tone: .warning)
            FluxHaptic.warning.play()
            return partial + "\n" + reason
        }
        FluxHaptic.error.play()
        return reason
    }
}

/// V3a 下发表单的状态与提交：每条链接一次 dispatch，全部结束后汇总。
@MainActor
@Observable
final class DispatchModel {
    var urlText = ""
    var fileName = ""
    var saveDir = ""
    private(set) var sending = false
    private(set) var done = 0
    private(set) var total = 0
    /// 内联错误（全部失败 / 部分失败时保留失败的链接供重试）。
    private(set) var failure: String?

    let target: DispatchTarget
    private let sourceHostID: String
    @ObservationIgnored private unowned let container: AppContainer

    init(target: DispatchTarget, sourceHostID: String, container: AppContainer) {
        self.target = target
        self.sourceHostID = sourceHostID
        self.container = container
    }

    var entries: [UrlEntry] { parseEntries(urlText).dedupe() }

    var saveDirCheck: DeviceRules.SaveDirCheck {
        DeviceRules.checkSaveDir(saveDir, style: target.pathStyle)
    }

    /// 提交；返回是否全部成功（成功时调用方关闭表单）。
    func send() async -> Bool {
        guard !sending else { return false }
        guard container.host.id == sourceHostID else {
            failure = L("mobileDownloadDraftHostChanged")
            return false
        }
        let entries = entries
        guard !entries.isEmpty else { return false }
        let dir: String?
        switch saveDirCheck {
        case .invalid: return false
        case .useDefault: dir = nil
        case let .explicit(path): dir = path
        }
        container.supersedeHostRestore()
        sending = true
        failure = nil
        done = 0
        total = entries.count
        defer { sending = false }

        let typedName = fileName.trimmingCharacters(in: .whitespacesAndNewlines)
        let single = entries.count == 1
        let items = entries.map { entry in
            let name = single && !typedName.isEmpty ? typedName : entry.fileName
            return DispatchItem(entry: entry, fileName: name.isEmpty ? nil : name)
        }
        let summary = await Dispatcher.run(items, to: target, saveDir: dir, agent: container.agent) { done = $0 }
        guard let failure = Dispatcher.announce(summary, target: target, toasts: container.toasts) else { return true }
        // 只保留失败的链接，成功的不会被重复下发。
        urlText = summary.failedEntries.map { $0.toText() }.joined(separator: "\n")
        self.failure = failure
        return false
    }
}

/// V3a 下发下载 sheet：多行链接（aria2 风格，与新建下载同一解析）、可选文件名（单条链接时）、可选保存目录
/// （按目标设备路径风格校验，留空 = 目标设备默认目录）。
struct DispatchSheet: View {
    @Environment(AppContainer.self) private var container
    let target: DispatchTarget
    let sourceHostID: String

    var body: some View {
        DispatchForm(target: target, sourceHostID: sourceHostID, container: container)
    }
}

private struct DispatchForm: View {
    @Environment(AppContainer.self) private var container
    @Environment(\.dismiss) private var dismiss
    @State private var model: DispatchModel
    @FocusState private var linksFocused: Bool

    init(target: DispatchTarget, sourceHostID: String, container: AppContainer) {
        _model = State(initialValue: DispatchModel(target: target, sourceHostID: sourceHostID, container: container))
    }

    var body: some View {
        @Bindable var model = model
        let entries = model.entries
        let readOnly = container.store.state.isReadOnly
        let target = model.target
        let saveDirInvalid: Bool = if case .invalid = model.saveDirCheck { true } else { false }
        let canSend = !entries.isEmpty && !saveDirInvalid && !readOnly && !model.sending
        NavigationStack {
            Form {
                if readOnly {
                    Section {
                        Banner(text: L("localServiceDisconnected"), tone: .warning, systemImage: FluxSymbol.offline, slim: true)
                            .listRowBackground(Color.clear)
                            .listRowInsets(EdgeInsets())
                    }
                }
                if target.isOffline {
                    Section {
                        Banner(
                            text: L(target.isCloud ? "downloadToOfflineHint" : "errReasonPeerOffline"),
                            tone: .warning,
                            systemImage: "moon.zzz.fill",
                            slim: true
                        )
                        .listRowBackground(Color.clear)
                        .listRowInsets(EdgeInsets())
                    }
                }
                linksSection(model: $model, entries: entries)
                Section {
                    TextField(L("filenameOptional"), text: $model.fileName)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .disabled(entries.count > 1)
                } footer: {
                    if entries.count > 1 { Text(L("mobileDispatchFileNameSingleOnly")) }
                }
                saveDirSection(model: $model, invalid: saveDirInvalid)
                if model.sending {
                    Section {
                        HStack(spacing: 12) {
                            ProgressView()
                            Text(L("mobileDispatchSending", ["done": model.done, "total": model.total]))
                                .monospacedDigit()
                        }
                        .accessibilityElement(children: .combine)
                    }
                }
                if let failure = model.failure {
                    Section {
                        Banner(text: failure, tone: .error, systemImage: FluxSymbol.failure, slim: true)
                            .listRowBackground(Color.clear)
                            .listRowInsets(EdgeInsets())
                    }
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle(L("mobileDispatchTitle", ["device": target.name]))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L("cancel"), role: .cancel) { dismiss() }
                        .disabled(model.sending)
                }
                ToolbarItem(placement: .confirmationAction) {
                    if model.sending {
                        ProgressView().accessibilityLabel(L("mobileDispatchSend"))
                    } else {
                        Button(L("mobileDispatchSend")) {
                            linksFocused = false
                            Task {
                                if await model.send() { dismiss() }
                            }
                        }
                        .disabled(!canSend)
                    }
                }
            }
            .interactiveDismissDisabled(model.sending)
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .linkPairingPrompts()
    }

    // MARK: 链接

    private func linksSection(model: Bindable<DispatchModel>, entries: [UrlEntry]) -> some View {
        let text = model.wrappedValue.urlText
        let hasText = !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let noValid = hasText && entries.isEmpty
        return Section {
            TextEditor(text: model.urlText)
                .font(.callout.monospaced())
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
                .focused($linksFocused)
                .disabled(model.wrappedValue.sending)
                .frame(minHeight: 110)
                .overlay(alignment: .topLeading) {
                    if text.isEmpty {
                        Text(L(BuildFlavor.key(full: "batchUrlPlaceholder", store: "batchUrlPlaceholderGeneric")))
                            .font(.callout.monospaced())
                            .foregroundStyle(.tertiary)
                            .padding(.top, 8)
                            .padding(.leading, 5)
                            .allowsHitTesting(false)
                            .accessibilityHidden(true)
                    }
                }
                .accessibilityLabel(L("downloadUrl"))
            if noValid {
                Label(
                    L(BuildFlavor.key(full: "newDownloadNoValidUrl", store: "newDownloadNoValidUrlGeneric")),
                    systemImage: FluxSymbol.failure
                )
                .font(.footnote)
                .foregroundStyle(Color.fdStatusFailedText)
            }
        } header: {
            HStack {
                Text(L("downloadUrl"))
                Spacer()
                if !entries.isEmpty {
                    Text(L("urlCount", ["count": entries.count]))
                        .monospacedDigit()
                        .accessibilityAddTraits(.updatesFrequently)
                }
            }
        } footer: {
            Text(L("mobileDispatchUrlsHint"))
        }
    }

    // MARK: 保存目录

    private func saveDirSection(model: Bindable<DispatchModel>, invalid: Bool) -> some View {
        let target = model.wrappedValue.target
        let placeholder = target.defaultSaveDir.map { L("downloadToRemoteDirDefault", ["dir": $0]) }
            ?? L("downloadToRemoteDirUseDefault")
        return Section {
            TextField(L("saveDir"), text: model.saveDir, prompt: Text(placeholder))
                .font(.body.monospaced())
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
                .disabled(model.wrappedValue.sending)
        } header: {
            Text(L("saveDir"))
        } footer: {
            VStack(alignment: .leading, spacing: 4) {
                Text(L("downloadToRemoteDirHint"))
                if invalid {
                    Label(
                        L("downloadToPathInvalid", ["example": PathStyle.example(target.pathStyle)]),
                        systemImage: FluxSymbol.failure
                    )
                    .foregroundStyle(Color.fdStatusFailedText)
                }
            }
        }
    }
}
