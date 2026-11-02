import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

/// BT 设置 › 防吸血页签（移动端子集）：总开关与运行状态 · 今日统计 · 各规则模式 · 云名单（开关 / 模式 / 状态卡）·
/// 证据上报（显式同意 + 需登录提示）· 封禁名单入口。阈值 / 处置阶梯 / 豁免 / 过滤文件订阅 / 事件日志仅桌面与 Web 提供。
/// 状态来自 `daemon.bt.guard.state`：页签可见时每 5 秒刷新，收到 `btGuardChanged` 通知立即刷新。
/// 轮询与 `btGuardChanged` 订阅由 `BtPage` 持有（页签与 scenePhase 门控，不随行滚动取消），本视图只渲染。
struct BtGuardTab: View {
    @Environment(HostStore.self) private var store
    @Environment(ConfigEditor.self) private var editor

    let model: BtGuardStateStore
    /// 立即重新加载（重启轮询任务）。
    let reload: () -> Void

    @State private var sections = AgentSections()
    @State private var consentShown = false

    var body: some View {
        let form = editor.form
        let readOnly = store.state.isReadOnly
        let guardOn = BtGuardRow.isGuardEnabled(in: form)
        let loggedIn = sections.session(store.state) != nil

        Section {
            if BtGuardRow.enabled.isVisible(in: form) { ConfigToggleRow(item: BtGuardRow.enabled.item) }
            statusRow(guardOn: guardOn)
        } footer: {
            Text(L("btGuardSectionRulesDesc"))
        }
        .disabled(readOnly)

        if guardOn, let state = model.state {
            statsSection(state)
        }

        if BtGuardRow.ruleRows.contains(where: { $0.isVisible(in: form) }) {
            Section {
                ForEach(BtGuardRow.ruleRows.filter { $0.isVisible(in: form) }) { row in
                    ConfigPickerRow(item: row.item, options: Self.modeOptions, fallback: "observe")
                }
            } header: {
                Text(L("btGuardSectionRules"))
            }
            .disabled(readOnly || !guardOn)
        }

        cloudSection(form, guardOn: guardOn, readOnly: readOnly)
        reportSection(form, guardOn: guardOn, loggedIn: loggedIn, readOnly: readOnly)
        bansSection(guardOn: guardOn)
    }

    static var modeOptions: [SettingsOption] {
        BtGuardRow.modeOptions.map { SettingsOption(id: $0.id, label: L($0.labelKey)) }
    }

    // MARK: 状态

    @ViewBuilder
    private func statusRow(guardOn: Bool) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if model.state == nil, let error = model.error {
                SettingsStatusLine(text: "\(L("btGuardLoadFailed")): \(ErrorText.describe(error))", tone: .failure)
                SettingsActionRow(title: L("btGuardReload"), systemImage: FluxSymbol.syncing, action: reload)
            } else {
                SettingsStatusLine(
                    text: L(BtGuardRules.statusKey(enabled: guardOn, state: model.state)),
                    tone: guardOn && model.state?.sessionActive == true ? .success : .neutral
                )
            }
        }
        .settingsRow(BtGuardRow.enabled.id + ".status")
    }

    private func statsSection(_ state: BtGuardState) -> some View {
        Section {
            SettingsInfoRow(title: L("btGuardStatObserved"), value: String(state.stats.todayObserved))
            SettingsInfoRow(title: L("btGuardStatEnforced"), value: String(state.stats.todayEnforced))
            SettingsInfoRow(title: L("btGuardStatBlocked"), value: String(state.stats.todayBlockedConnections))
            SettingsInfoRow(
                title: L("btGuardStatRefusedBytes"), value: Format.bytes(unsigned: state.stats.todayRefusedBytes).description
            )
        } header: {
            Text(L("btGuardSectionStats"))
        } footer: {
            Text(Self.countsSummary(state.counts))
        }
    }

    static func countsSummary(_ counts: BtGuardCounts) -> String {
        L("btGuardCountsSummary", [
            "allow": counts.allow, "manual": counts.manual, "auto": counts.auto, "cloud": counts.cloud, "file": counts.file,
        ])
    }

    // MARK: 云名单

    @ViewBuilder
    private func cloudSection(_ form: SettingsConfigForm, guardOn: Bool, readOnly: Bool) -> some View {
        if BtGuardRow.cloudEnabled.isVisible(in: form) || BtGuardRow.cloudMode.isVisible(in: form) {
            let cloudOn = form.bool(BtGuardRow.cloudEnabled.configKey)
            Section {
                if BtGuardRow.cloudEnabled.isVisible(in: form) { ConfigToggleRow(item: BtGuardRow.cloudEnabled.item) }
                if BtGuardRow.cloudMode.isVisible(in: form) {
                    ConfigPickerRow(item: BtGuardRow.cloudMode.item, options: Self.modeOptions, fallback: "observe")
                        .disabled(!BtGuardRow.cloudModeEditable(in: form))
                }
                if let state = model.state {
                    BtGuardCloudCard(cloud: state.cloud, enabled: cloudOn)
                }
            } header: {
                Text(L("btGuardSectionCloud"))
            }
            .disabled(readOnly || !guardOn)
        }
    }

    // MARK: 证据上报

    @ViewBuilder
    private func reportSection(_ form: SettingsConfigForm, guardOn: Bool, loggedIn: Bool, readOnly: Bool) -> some View {
        if BtGuardRow.reportEnabled.isVisible(in: form) {
            let row = BtGuardRow.reportEnabled
            let reportOn = form.bool(row.configKey)
            // 已开启时始终可关；未开启时要求防吸血已启用且已登录。
            let canToggle = reportOn || (guardOn && !BtGuardRules.reportNeedsLogin(loggedIn: loggedIn))
            Section {
                Toggle(isOn: Binding(
                    get: { reportOn },
                    set: { next in
                        if next { consentShown = true } else { editor.set(row.configKey, SettingsConfigForm.wire(false)) }
                    }
                )) {
                    SettingsText(title: row.item.title, detail: row.item.detail)
                }
                .tint(Color.fdToggleOn)
                .disabled(readOnly || !canToggle)
                .settingsRow(row.id, failureKey: row.configKey)
                .sheet(isPresented: $consentShown) {
                    BtGuardReportConsentSheet {
                        editor.set(row.configKey, SettingsConfigForm.wire(true))
                    }
                }
                if BtGuardRules.reportNeedsLogin(loggedIn: loggedIn) {
                    SettingsStatusLine(text: L("btGuardReportLoginRequired"), tone: .warning, systemImage: FluxSymbol.info)
                }
                VStack(alignment: .leading, spacing: 4) {
                    Text(L("btGuardReportContentsTitle")).font(.footnote.weight(.semibold))
                    Text(L("btGuardReportContents")).font(.footnote).foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            } header: {
                Text(L("btGuardSectionReport"))
            }
        }
    }

    // MARK: 封禁名单入口

    @ViewBuilder
    private func bansSection(guardOn: Bool) -> some View {
        if BtGuardRow.bans.isVisible(in: editor.form) {
            Section {
                NavigationLink {
                    BtGuardBansPage()
                } label: {
                    LabeledContent {
                        if let counts = model.state?.counts {
                            Text(String(Self.totalEntries(counts))).monospacedDigit()
                        }
                    } label: {
                        SettingsText(title: L("btGuardTabBans"))
                    }
                }
                .settingsRow(BtGuardRow.bans.id)
            }
        }
    }

    /// 各层未过期条目总数（入口行读数）。
    static func totalEntries(_ counts: BtGuardCounts) -> Int {
        Int(counts.allow) + Int(counts.manual) + Int(counts.auto) + Int(counts.cloud) + Int(counts.file)
    }
}

// MARK: - 云名单状态卡

/// 序号 / 条目数 / 更新与过期时间 + 问题行（过期 · 被拒 · 拉取失败 · 未启用 · 暂无）+ 署名（CC BY 4.0 要求展示）。
private struct BtGuardCloudCard: View {
    let cloud: BtGuardCloudStatus
    let enabled: Bool

    var body: some View {
        let kind = BtGuardRules.cloudStatusKind(cloud, enabled: enabled)
        let hasDocument = kind != .disabled && cloud.sequence > 0
        VStack(alignment: .leading, spacing: 6) {
            if hasDocument {
                VStack(alignment: .leading, spacing: 2) {
                    Text(L("btGuardCloudSequence", ["n": cloud.sequence]))
                    Group {
                        Text(L("btGuardCloudEntries", ["entries": cloud.entries, "rules": cloud.clientRules]))
                        if cloud.updatedAt > 0 {
                            Text(L("btGuardCloudUpdatedAt", ["time": SubscriptionTime.text(unix: cloud.updatedAt)]))
                        }
                        if cloud.expiresAt > 0 {
                            Text(L("btGuardCloudExpiresAt", ["time": SubscriptionTime.expiryText(unix: cloud.expiresAt)]))
                        }
                    }
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            }
            if let key = BtGuardRules.cloudProblemKey(kind) {
                SettingsStatusLine(
                    text: L(key, ["reason": cloud.rejectedReason ?? "", "error": cloud.lastError ?? ""]),
                    tone: BtGuardRules.cloudProblemIsAlert(kind) ? (kind == .stale ? .warning : .failure) : .neutral
                )
                .lineLimit(4)
                .truncationMode(.tail)
            }
            if !cloud.attribution.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    Text(L("btGuardCloudAttribution")).font(.caption.weight(.semibold))
                    ForEach(Array(cloud.attribution.enumerated()), id: \.offset) { _, line in
                        Text(line).font(.caption).lineLimit(3).truncationMode(.middle).textSelection(.enabled)
                    }
                }
                .foregroundStyle(.secondary)
                .padding(.top, 4)
            }
        }
        .settingsRow("btGuard.cloudStatus")
    }
}

// MARK: - 上报同意

/// 开启证据上报前的显式同意：说明上报什么、绝不上报什么，确认后才写入 `bt_guard_report_enabled`。
private struct BtGuardReportConsentSheet: View {
    let onConfirm: () -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(L("btGuardReportConsentBody"))
                }
            }
            .navigationTitle(L("btGuardReportConsentTitle"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L("cancel")) { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(L("btGuardReportConsentConfirm")) {
                        onConfirm()
                        dismiss()
                    }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }
}

// MARK: - 变化通知

/// `btGuardChanged` 一次性通知 → 回调。只处理挂载之后到达的通知（不重放历史）。
struct BtGuardChangeObserver: ViewModifier {
    let onChange: () -> Void

    @Environment(HostStore.self) private var store
    @State private var handled: Int?

    func body(content: Content) -> some View {
        content
            .onAppear { if handled == nil { handled = store.notices.last?.id ?? 0 } }
            .onChange(of: store.notices.last?.id) { _, _ in consume() }
    }

    private func consume() {
        let last = handled ?? 0
        let pending = store.notices.filter { $0.id > last }
        guard let newest = pending.last else { return }
        handled = newest.id
        if pending.contains(where: { $0.name == HostNoticeName.btGuardChanged }) { onChange() }
    }
}
#endif
