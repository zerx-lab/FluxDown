import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

// D4 做种限制（02-downloads §5.5 / §6）：做种页的 5 行摘要 + 推入的编辑页。
// 当前值不在 `DownloadTask` 里：进入做种页时 `daemon.task.get` 读取，保存（`daemon.task.setSeedLimits`）后更新并重读。

// MARK: - 文案

enum SeedLimitText {
    /// 摘要值：跟随全局 / 不限 / 数值（比例 `x.xx`、分钟走时长文本）。
    static func summary(wire: Int64, unit: SeedLimitField.Unit) -> String {
        let field = SeedLimitField(wire: wire, unit: unit)
        switch field.mode {
        case .inherit: return L("detailFollowGlobal")
        case .unlimited: return L("btSeedLimitsModeUnlimited")
        case .custom:
            switch unit {
            case .ratio: return field.text
            case .minutes: return TaskDetailText.duration(wire * 60)
            }
        }
    }

    /// 上传限速摘要：> 0 为速度，否则跟随全局。
    static func uploadSummary(bps: Int64) -> String {
        Format.speed(bps)?.description ?? L("detailFollowGlobal")
    }

    /// 页脚里的全局值：nil = 不限制。
    static func globalRatio(_ value: Double?) -> String {
        guard let value else { return L("btSeedLimitsModeUnlimited") }
        return String(format: "%.2f", locale: Locale(identifier: "en_US_POSIX"), value)
    }

    static func globalMinutes(_ value: Int64?) -> String {
        guard let value else { return L("btSeedLimitsModeUnlimited") }
        return TaskDetailText.duration(value * 60)
    }
}

// MARK: - 读取

/// 做种限制现值的读取器：`daemon.task.get` → `TaskSeedLimits`。已有现值时重读失败静默保留旧值。
@MainActor
@Observable
final class SeedLimitsLoader {
    nonisolated enum Phase: Equatable {
        case idle
        case loading
        case loaded(TaskSeedLimits)
        case failed(String)
    }

    private(set) var phase: Phase = .idle
    @ObservationIgnored private var inFlight = false

    var limits: TaskSeedLimits? {
        guard case let .loaded(value) = phase else { return nil }
        return value
    }

    /// 首次出现时读取（同一读取器只自动读一次；失败后由用户点「重试」）。
    func loadIfNeeded(session: any HostSession, taskId: String) async {
        guard phase == .idle else { return }
        await load(session: session, taskId: taskId)
    }

    func load(session: any HostSession, taskId: String) async {
        guard !inFlight else { return }
        inFlight = true
        defer { inFlight = false }
        let previous = limits
        if previous == nil { phase = .loading }
        do throws(HostError) {
            let result: TaskSeedLimits = try await session.call(HostMethod.daemonTaskGet, params: TaskIdParams(taskId: taskId))
            phase = .loaded(result)
        } catch {
            if previous == nil { phase = .failed(ErrorText.describe(error)) }
        }
    }

    /// 保存成功后立即以所发值更新摘要（随后会重读以对齐主机归一化后的值）。
    func apply(_ params: SetSeedLimitsParams) {
        phase = .loaded(
            TaskSeedLimits(
                seedRatioLimitMilli: params.ratioLimitMilli,
                seedPostRatioLimitMilli: params.postRatioLimitMilli,
                seedTimeLimitMinutes: params.seedTimeLimitMinutes,
                seedInactiveTimeLimitMinutes: params.inactiveTimeLimitMinutes,
                seedUploadLimitBps: params.uploadLimitBps
            )
        )
    }
}

// MARK: - 做种页「做种限制」分区

struct TaskSeedLimitsSection: View {
    let taskId: String

    @Environment(AppContainer.self) private var container
    @State private var loader = SeedLimitsLoader()

    var body: some View {
        Section {
            switch loader.phase {
            case .idle, .loading:
                HStack(spacing: 12) {
                    ProgressView()
                    Text(L("pluginCommonLoading")).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                .accessibilityElement(children: .combine)
            case let .failed(message):
                VStack(alignment: .leading, spacing: 8) {
                    Text(message)
                        .font(.subheadline)
                        .foregroundStyle(Color.fdStatusFailedText)
                    Button(L("detailActivityRetry")) { reload() }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            case let .loaded(limits):
                summaryRow(L("btSeedRatioLimit"), SeedLimitText.summary(wire: limits.seedRatioLimitMilli, unit: .ratio), limits)
                summaryRow(L("btSeedPostRatioLimit"), SeedLimitText.summary(wire: limits.seedPostRatioLimitMilli, unit: .ratio), limits)
                summaryRow(L("btSeedTimeLimit"), SeedLimitText.summary(wire: limits.seedTimeLimitMinutes, unit: .minutes), limits)
                summaryRow(
                    L("btSeedInactiveTimeLimit"),
                    SeedLimitText.summary(wire: limits.seedInactiveTimeLimitMinutes, unit: .minutes),
                    limits
                )
                summaryRow(L("btSeedUploadLimit"), SeedLimitText.uploadSummary(bps: limits.seedUploadLimitBps), limits)
            }
        } header: {
            Text(L("btSeedLimitsTitle"))
        } footer: {
            Text(L("btSeedUploadLimitHint"))
        }
        .task { await loader.loadIfNeeded(session: container.session, taskId: taskId) }
    }

    private func summaryRow(_ key: String, _ value: String, _ limits: TaskSeedLimits) -> some View {
        NavigationLink {
            SeedLimitsScreen(taskId: taskId, limits: limits) { params in
                loader.apply(params)
                reload()
            }
        } label: {
            TaskDetailLinkLabel(key: key, value: value, showsChevron: false)
        }
    }

    private func reload() {
        let session = container.session
        let id = taskId
        Task { await loader.load(session: session, taskId: id) }
    }
}

// MARK: - D4 编辑页

struct SeedLimitsScreen: View {
    let taskId: String
    let onSaved: (SetSeedLimitsParams) -> Void

    @Environment(AppContainer.self) private var container
    @Environment(TaskActions.self) private var actions
    @Environment(\.dismiss) private var dismiss
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.horizontalSizeClass) private var sizeClass
    /// 页内草稿：返回即丢弃。
    @State private var draft: SeedLimitsDraft
    @State private var saving = false

    init(taskId: String, limits: TaskSeedLimits, onSaved: @escaping (SetSeedLimitsParams) -> Void) {
        self.taskId = taskId
        self.onSaved = onSaved
        _draft = State(initialValue: SeedLimitsDraft(limits: limits))
    }

    private var isReadOnly: Bool { container.store.state.isReadOnly }
    private var canSave: Bool { draft.isValid && draft.isModified && !isReadOnly && !saving }
    private var modes: [SeedLimitMode] { [draft.ratio.mode, draft.postRatio.mode, draft.time.mode, draft.inactive.mode] }

    var body: some View {
        let globals = SeedLimitGlobals(config: container.store.state.config)
        Form {
            SeedLimitFieldSection(
                title: L("btSeedRatioLimit"),
                field: $draft.ratio,
                globalText: SeedLimitText.globalRatio(globals.ratio)
            )
            SeedLimitFieldSection(
                title: L("btSeedPostRatioLimit"),
                field: $draft.postRatio,
                globalText: SeedLimitText.globalRatio(globals.postRatio)
            )
            SeedLimitFieldSection(
                title: L("btSeedTimeLimit"),
                field: $draft.time,
                globalText: SeedLimitText.globalMinutes(globals.timeMinutes)
            )
            SeedLimitFieldSection(
                title: L("btSeedInactiveTimeLimit"),
                field: $draft.inactive,
                globalText: SeedLimitText.globalMinutes(globals.inactiveMinutes)
            )
            uploadSection
        }
        .disabled(isReadOnly)
        .fluxAnimation(.snappy, value: modes)
        .fluxAnimation(.snappy, value: draft.upload.custom)
        .navigationTitle(L("btSeedLimitsTitle"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbarVisibility(sizeClass == .compact ? .hidden : .automatic, for: .tabBar)
        .suppressesBottomAccessory("seedLimits", when: sizeClass == .compact)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if saving {
                    ProgressView().accessibilityLabel(L("confirm"))
                } else {
                    Button(L("confirm")) { save() }
                        .disabled(!canSave)
                }
            }
        }
    }

    // MARK: 上传限速（两态：跟随全局 / 自定义 KB/s）

    private var uploadPicker: some View {
        Picker(L("btSeedUploadLimit"), selection: $draft.upload.custom) {
            Text(L("detailFollowGlobal")).tag(false)
            Text(L("btSeedLimitsModeCustom")).tag(true)
        }
        .labelsHidden()
    }

    private var uploadSection: some View {
        Section {
            if typeSize.isAccessibilitySize {
                uploadPicker.pickerStyle(.inline)
            } else {
                uploadPicker.pickerStyle(.segmented)
            }
            if draft.upload.custom {
                SeedLimitInputRow(
                    title: L("btSeedUploadLimit"),
                    text: $draft.upload.text,
                    isValid: draft.upload.isValid,
                    keyboard: .numberPad,
                    prompt: "0",
                    unit: L("statusSpeedLimitKbs")
                )
            }
        } header: {
            Text(L("btSeedUploadLimit"))
        } footer: {
            Text(L("btSeedUploadLimitHint"))
        }
    }

    // MARK: 保存

    private func save() {
        guard canSave, let params = draft.params(taskId: taskId), actions.guardWritable() else { return }
        saving = true
        let session = container.session
        Task {
            do throws(HostError) {
                try await session.callVoid(HostMethod.daemonTaskSetSeedLimits, params: params)
                FluxHaptic.success.play()
                container.toasts.show(text: L("btSeedLimitsSaved"), tone: .success, systemImage: FluxSymbol.done)
                onSaved(params)
                dismiss()
            } catch {
                container.toasts.show(text: ErrorText.describe(error), tone: .error, systemImage: "exclamationmark.circle")
            }
            saving = false
        }
    }
}

/// 一个三态字段的分区：页眉 = 字段名；分段（跟随全局 / 不限 / 自定义）+ 自定义输入行；页脚 = 全局值。
private struct SeedLimitFieldSection: View {
    let title: String
    @Binding var field: SeedLimitField
    let globalText: String

    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        Section {
            picker
            if field.mode == .custom {
                SeedLimitInputRow(
                    title: title,
                    text: $field.text,
                    isValid: field.isValid,
                    keyboard: field.unit == .ratio ? .decimalPad : .numberPad,
                    prompt: field.unit == .ratio ? "1.00" : "60",
                    unit: field.unit == .minutes ? L("timeUnitMinutes") : nil
                )
            }
        } header: {
            Text(title)
        } footer: {
            Text(L("taskSeedLimitGlobalFooter", ["value": globalText]))
        }
    }

    @ViewBuilder private var picker: some View {
        if typeSize.isAccessibilitySize {
            // ≥ AX1 放不下三段：改为内联单选列表，用完整的「跟随全局设置」。
            Picker(title, selection: $field.mode) {
                Text(L("btSeedLimitsModeGlobal")).tag(SeedLimitMode.inherit)
                Text(L("btSeedLimitsModeUnlimited")).tag(SeedLimitMode.unlimited)
                Text(L("btSeedLimitsModeCustom")).tag(SeedLimitMode.custom)
            }
            .labelsHidden()
            .pickerStyle(.inline)
        } else {
            Picker(title, selection: $field.mode) {
                Text(L("detailFollowGlobal")).tag(SeedLimitMode.inherit)
                Text(L("btSeedLimitsModeUnlimited")).tag(SeedLimitMode.unlimited)
                Text(L("btSeedLimitsModeCustom")).tag(SeedLimitMode.custom)
            }
            .labelsHidden()
            .pickerStyle(.segmented)
        }
    }
}

/// 自定义值输入行：数值右对齐 + 单位；无法解析时红字 + 图标（不只靠颜色）。也用于任务限速表单。
struct SeedLimitInputRow: View {
    let title: String
    @Binding var text: String
    let isValid: Bool
    let keyboard: UIKeyboardType
    let prompt: String
    let unit: String?

    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        let layout = typeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 4))
            : AnyLayout(HStackLayout(alignment: .firstTextBaseline, spacing: 8))
        layout {
            TextField(title, text: $text, prompt: Text(verbatim: prompt))
                .keyboardType(keyboard)
                .multilineTextAlignment(typeSize.isAccessibilitySize ? .leading : .trailing)
                .monospacedDigit()
                .foregroundStyle(isValid ? Color.primary : Color.fdStatusFailedText)
            if let unit {
                Text(unit).foregroundStyle(.secondary)
            }
            if !isValid {
                Label(L("pluginErrPattern"), systemImage: FluxSymbol.failure)
                    .labelStyle(.iconOnly)
                    .foregroundStyle(Color.fdStatusFailedText)
            }
        }
        .frame(minHeight: 44)
    }
}
#endif
