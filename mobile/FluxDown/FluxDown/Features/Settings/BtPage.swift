import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

/// S6 · BitTorrent 设置（常规 · Tracker · 做种）。
/// 行与 GPUI `sections/bt.rs` 同序同键同范围；全部写入走 `ConfigEditor`（云同步键 ☁︎ 由目录决定）。
/// 做种关闭时其余做种行整体隐藏；订阅关闭时订阅地址置灰。断线（只读）时各分组整体置灰。
struct BtPage: View {
    @Environment(AppContainer.self) private var container
    @Environment(HostStore.self) private var store
    @Environment(ConfigEditor.self) private var editor
    @Environment(SettingsFocus.self) private var focus

    @SceneStorage("settings.bt.tab") private var tab: BtSettingsTab = .general
    @Environment(\.scenePhase) private var scenePhase
    @State private var guardModel = BtGuardStateStore()
    @State private var guardReload = 0

    private struct GuardPollKey: Hashable {
        let active: Bool
        let visible: Bool
        let token: Int
    }

    var body: some View {
        let form = editor.form
        let btOn = !form.isLoaded || BtSettingsRow.isEnabled(in: form)
        let shownTab = btOn ? tab : .general
        let readOnly = store.state.isReadOnly
        SettingsPage(title: L("settingsCatBt"), showsReadOnlyBanner: true, showsSyncLegend: true) {
            if btOn { BtSettingsTabPicker(title: L("settingsCatBt"), selection: $tab) { L($0.titleKey) } }
            if !form.isLoaded {
                Section {
                    ProgressView().frame(maxWidth: .infinity, minHeight: 44)
                }
            } else {
                switch shownTab {
                case .general: generalSections(form, readOnly: readOnly)
                case .tracker: trackerSections(form, readOnly: readOnly)
                case .seeding: seedingSections(form, readOnly: readOnly)
                case .antiLeech: BtGuardTab(model: guardModel, reload: { guardReload += 1 })
                }
            }
        }
        .fluxAnimation(.smooth, value: form)
        // 防吸血状态轮询挂在页面层（不随 Form 行滚动取消）：仅在前台且防吸血页签可见时运行；
        // 回到前台 / 切回页签 / 收到 `btGuardChanged` 都会重启并立即加载一次。
        .task(id: GuardPollKey(active: scenePhase == .active, visible: btOn && shownTab == .antiLeech, token: guardReload)) {
            guard scenePhase == .active, btOn, shownTab == .antiLeech else { return }
            await guardModel.poll(container.session)
        }
        .modifier(BtGuardChangeObserver { guardReload += 1 })
        // 搜索命中：先切到目标行所在页签（reveal 在 350ms 后才滚动，页签内容此时已渲染）。
        .onChange(of: focus.target, initial: true) { _, target in
            if let target, let owner = BtSettingsRow.tab(forRowID: target) { tab = owner }
        }
        .onChange(of: form.int(BtSettingsRow.portStart.configKey, default: 6881)) { _, start in
            raiseEndPort(toAtLeast: start, readOnly: readOnly)
        }
    }

    /// 起始端口被本机改到结束端口之上时，把结束端口一并抬到起始端口（只响应本机的待提交修改，不改写主机侧变化）。
    private func raiseEndPort(toAtLeast start: Int, readOnly: Bool) {
        let endKey = BtSettingsRow.portEnd.configKey
        guard !readOnly, editor.optimistic[BtSettingsRow.portStart.configKey] != nil,
              editor.form.int(endKey, default: 6891) < start else { return }
        editor.set(endKey, String(start))
    }

    // MARK: 常规

    @ViewBuilder
    private func generalSections(_ form: SettingsConfigForm, readOnly: Bool) -> some View {
        let start = form.int(BtSettingsRow.portStart.configKey, default: 6881)
        let end = form.int(BtSettingsRow.portEnd.configKey, default: 6891)
        Section {
            if BtSettingsRow.enabled.isVisible(in: form) { ConfigToggleRow(item: BtSettingsRow.enabled.item) }
            if BtSettingsRow.dht.isVisible(in: form) { ConfigToggleRow(item: BtSettingsRow.dht.item) }
            if BtSettingsRow.upnp.isVisible(in: form) { ConfigToggleRow(item: BtSettingsRow.upnp.item) }
            if BtSettingsRow.utp.isVisible(in: form) { ConfigToggleRow(item: BtSettingsRow.utp.item) }
            if BtSettingsRow.portStart.isVisible(in: form) {
                ConfigNumberRow(item: BtSettingsRow.portStart.item, range: BtPortRange.bounds, fallback: 6881)
            }
            if BtSettingsRow.portEnd.isVisible(in: form) {
                ConfigNumberRow(
                    item: BtSettingsRow.portEnd.item,
                    range: min(max(start, BtPortRange.bounds.lowerBound), BtPortRange.bounds.upperBound) ... BtPortRange.bounds.upperBound,
                    fallback: 6891
                )
                if !BtPortRange.isValid(start: start, end: end) {
                    SettingsStatusLine(text: L("btPortRangeInvalid"), tone: .warning)
                        .transition(.opacity)
                }
            }
            if BtSettingsRow.mseMode.isVisible(in: form) { BtMseModeRow() }
        } header: {
            Text(L("btSettingsRestartHint")).textCase(nil)
        } footer: {
            if container.isLocalHost { Text(L("btMobileFootnote")) }
        }
        .disabled(readOnly)
    }

    // MARK: Tracker

    @ViewBuilder
    private func trackerSections(_ form: SettingsConfigForm, readOnly: Bool) -> some View {
        if BtSettingsRow.customTrackers.isVisible(in: form) {
            Section {
                ConfigLinesRow(item: BtSettingsRow.customTrackers.item, placeholder: L("btTrackerPlaceholder"))
            }
            .disabled(readOnly)
        }
        if !BtSettingsRow.visible(in: .tracker, form).filter({ $0 != .customTrackers }).isEmpty {
            Section {
                if BtSettingsRow.trackerSub.isVisible(in: form) { ConfigToggleRow(item: BtSettingsRow.trackerSub.item) }
                if BtSettingsRow.trackerSubUrls.isVisible(in: form) {
                    ConfigLinesRow(
                        item: BtSettingsRow.trackerSubUrls.item, placeholder: L("btTrackerSubPlaceholder"),
                        isEnabled: form.bool(BtSettingsRow.trackerSub.configKey)
                    )
                }
                if BtSettingsRow.trackerSubStatus.isVisible(in: form) { SubscriptionStatusRow(kind: .btTrackers) }
            }
            .disabled(readOnly)
        }
    }

    // MARK: 做种

    @ViewBuilder
    private func seedingSections(_ form: SettingsConfigForm, readOnly: Bool) -> some View {
        let seeding = BtSettingsRow.visible(in: .seeding, form)
        if !seeding.isEmpty {
            Section {
                if BtSettingsRow.seedEnabled.isVisible(in: form) { ConfigToggleRow(item: BtSettingsRow.seedEnabled.item) }
                if BtSettingsRow.seedMaxActive.isVisible(in: form) {
                    ConfigNumberRow(
                        item: BtSettingsRow.seedMaxActive.item, range: 0 ... 1_000_000, fallback: 0,
                        specials: [0: L("btSeedMaxActiveUnlimited")]
                    )
                    .transition(.opacity.combined(with: .move(edge: .top)))
                }
                if BtSettingsRow.autoReseed.isVisible(in: form) {
                    ConfigToggleRow(item: BtSettingsRow.autoReseed.item)
                        .transition(.opacity.combined(with: .move(edge: .top)))
                }
            }
            .disabled(readOnly)
        }
        if seeding.contains(where: { $0 != .seedEnabled && $0 != .seedMaxActive && $0 != .autoReseed }) {
            Section(L("btSeedLimitsTitle")) {
                if BtSettingsRow.seedRatio.isVisible(in: form) {
                    ConfigDecimalRow(item: BtSettingsRow.seedRatio.item, zeroText: L("autoRetryOff"))
                }
                if BtSettingsRow.seedPostRatio.isVisible(in: form) {
                    ConfigDecimalRow(item: BtSettingsRow.seedPostRatio.item, zeroText: L("autoRetryOff"))
                }
                durationRow(.seedTimeLimit, form)
                durationRow(.seedInactiveTimeLimit, form)
                if BtSettingsRow.seedOperator.isVisible(in: form) {
                    ConfigSegmentedRow(
                        item: BtSettingsRow.seedOperator.item,
                        options: [
                            .init(id: "or", label: L("btSeedOperatorOr")),
                            .init(id: "and", label: L("btSeedOperatorAnd")),
                        ],
                        fallback: "or"
                    )
                }
                if BtSettingsRow.seedThenAction.isVisible(in: form) {
                    ConfigPickerRow(
                        item: BtSettingsRow.seedThenAction.item,
                        options: [
                            .init(id: "stop", label: L("btSeedStopSeeding")),
                            .init(id: "delete", label: L("btSeedDeleteTask")),
                            .init(id: "delete_files", label: L("btSeedDeleteTaskAndFiles")),
                        ],
                        fallback: "stop"
                    )
                }
            }
            .disabled(readOnly)
        }
    }

    @ViewBuilder
    private func durationRow(_ row: BtSettingsRow, _ form: SettingsConfigForm) -> some View {
        if row.isVisible(in: form), let unitKey = row.unitKey, let unitTitleKey = row.unitTitleKey {
            BtDurationRow(item: row.renderedItem, unitKey: unitKey, unitTitleKey: unitTitleKey)
        }
    }
}

// MARK: - MSE（协议加密）

/// `bt_mse_mode`：推入单选页（三项带说明）。行内显示当前选项。
private struct BtMseModeRow: View {
    @Environment(ConfigEditor.self) private var editor

    var body: some View {
        let item = BtSettingsRow.mseMode.item
        let current = BtMseMode(rawValue: editor.form.string(item.key, default: BtMseMode.enabled.rawValue)) ?? .enabled
        NavigationLink {
            BtMseModePage()
        } label: {
            LabeledContent {
                Text(L(current.titleKey))
            } label: {
                SettingsText(title: item.title, synced: item.isSynced)
            }
        }
        .settingsRow(item.id, failureKey: item.key)
    }
}

private struct BtMseModePage: View {
    @Environment(ConfigEditor.self) private var editor
    @Environment(HostStore.self) private var store

    var body: some View {
        let key = BtSettingsRow.mseMode.configKey
        let current = BtMseMode(rawValue: editor.form.string(key, default: BtMseMode.enabled.rawValue)) ?? .enabled
        SettingsPage(title: L("btMseMode"), showsReadOnlyBanner: true) {
            Section {
                ForEach(BtMseMode.allCases) { mode in
                    Button {
                        editor.set(key, mode.rawValue)
                    } label: {
                        HStack(spacing: 12) {
                            SettingsText(title: L(mode.titleKey), detail: L(mode.detailKey))
                            Spacer(minLength: 8)
                            if mode == current {
                                Image(systemName: FluxSymbol.done).fontWeight(.semibold).foregroundStyle(.tint)
                            }
                        }
                        .frame(minHeight: 44)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(mode == current ? .isSelected : [])
                }
            }
            .disabled(store.state.isReadOnly)
        }
        .fluxAnimation(.smooth, value: current)
    }
}

// MARK: - 搜索索引 / 读数

extension BtPage {
    /// 设置搜索索引：每个页签的所有可见行（与页面渲染共用 `BtSettingsRow.isVisible`）。
    static func searchEntries(_ ctx: SettingsSearchContext) -> [SettingsEntry] {
        let name = L("settingsCatBt")
        let basic = BtSettingsRow.allCases.filter { $0.isVisible(in: ctx.form) }.map { row in
            SettingsEntry(
                item: row.item, route: .bt, breadcrumb: "\(name) › \(L(row.tab.titleKey))",
                symbol: "point.3.connected.trianglepath.dotted"
            )
        }
        let leech = BtGuardRow.visible(ctx.form).map { row in
            SettingsEntry(
                item: row.item, route: .bt, breadcrumb: "\(name) › \(L(BtSettingsTab.antiLeech.titleKey))",
                symbol: "shield.lefthalf.filled"
            )
        }
        return basic + leech
    }

    /// 设置首页该分类行的读数（nil = 使用分类说明文案）。
    static func readout(_ ctx: SettingsSearchContext) -> String? { BtSettingsRow.readout(ctx.form) }
}
#endif
