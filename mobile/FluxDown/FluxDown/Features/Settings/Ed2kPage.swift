import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

/// S7 · eD2K 设置（常规 · 服务器 · Kad 节点）。行与 GPUI `sections/ed2k.rs` 同序同键同范围。
/// `ed2k_server_list` 存逗号分隔的 `host:port`，编辑区每行一条，写回时按条目去空白并忽略大小写去重。
/// `ed2k_nodes_dat_url` 与订阅地址一样按行存储；它是设备本地 daemon 键（不进云同步目录）。
struct Ed2kPage: View {
    @Environment(HostStore.self) private var store
    @Environment(ConfigEditor.self) private var editor
    @Environment(SettingsFocus.self) private var focus

    @SceneStorage("settings.ed2k.tab") private var tab: Ed2kSettingsTab = .general

    var body: some View {
        let form = editor.form
        let readOnly = store.state.isReadOnly
        SettingsPage(title: L("settingsCatEd2k"), showsReadOnlyBanner: true, showsSyncLegend: true) {
            BtSettingsTabPicker(title: L("settingsCatEd2k"), selection: $tab) { L($0.titleKey) }
            if !form.isLoaded {
                Section {
                    ProgressView().frame(maxWidth: .infinity, minHeight: 44)
                }
            } else {
                switch tab {
                case .general: generalSection(form, readOnly: readOnly)
                case .servers: serversSections(form, readOnly: readOnly)
                case .kad: kadSection(form, readOnly: readOnly)
                }
            }
        }
        .fluxAnimation(.smooth, value: form)
        // 搜索命中：先切到目标行所在页签。
        .onChange(of: focus.target, initial: true) { _, target in
            if let target, let owner = Ed2kSettingsRow.tab(forRowID: target) { tab = owner }
        }
    }

    // MARK: 常规

    @ViewBuilder
    private func generalSection(_ form: SettingsConfigForm, readOnly: Bool) -> some View {
        Section {
            if Ed2kSettingsRow.kad.isVisible(in: form) { ConfigToggleRow(item: Ed2kSettingsRow.kad.item) }
            if Ed2kSettingsRow.upnp.isVisible(in: form) { ConfigToggleRow(item: Ed2kSettingsRow.upnp.item) }
            if Ed2kSettingsRow.listenPort.isVisible(in: form) {
                ConfigNumberRow(
                    item: Ed2kSettingsRow.listenPort.item, range: 0 ... 65_535, fallback: 0, specials: [0: L("auto")]
                )
            }
            if Ed2kSettingsRow.idCheck.isVisible(in: form) { Ed2kIdCheckRow() }
        }
        .disabled(readOnly)
    }

    // MARK: 服务器

    @ViewBuilder
    private func serversSections(_ form: SettingsConfigForm, readOnly: Bool) -> some View {
        if Ed2kSettingsRow.serverList.isVisible(in: form) {
            Section {
                ConfigLinesRow(
                    item: Ed2kSettingsRow.serverList.item, placeholder: L("ed2kServerPlaceholder"),
                    toEditor: { SubscriptionListFormat.comma.toEditor($0) },
                    fromEditor: { SubscriptionListFormat.comma.toStored($0) }
                )
            }
            .disabled(readOnly)
        }
        if !Ed2kSettingsRow.visible(in: .servers, form).filter({ $0 != .serverList }).isEmpty {
            Section {
                if Ed2kSettingsRow.serverSub.isVisible(in: form) { ConfigToggleRow(item: Ed2kSettingsRow.serverSub.item) }
                if Ed2kSettingsRow.serverSubUrls.isVisible(in: form) {
                    ConfigLinesRow(
                        item: Ed2kSettingsRow.serverSubUrls.item, placeholder: L("ed2kServerSubPlaceholder"),
                        isEnabled: form.bool(Ed2kSettingsRow.serverSub.configKey)
                    )
                }
                if Ed2kSettingsRow.serverSubStatus.isVisible(in: form) { SubscriptionStatusRow(kind: .ed2kServers) }
            }
            .disabled(readOnly)
        }
    }

    // MARK: Kad 节点

    @ViewBuilder
    private func kadSection(_ form: SettingsConfigForm, readOnly: Bool) -> some View {
        if !Ed2kSettingsRow.visible(in: .kad, form).isEmpty {
            Section {
                if Ed2kSettingsRow.nodesSubUrls.isVisible(in: form) {
                    ConfigLinesRow(
                        item: Ed2kSettingsRow.nodesSubUrls.item, placeholder: L("ed2kNodesSubPlaceholder"),
                        isEnabled: form.bool(Ed2kSettingsRow.kad.configKey)
                    )
                }
                if Ed2kSettingsRow.nodesSubStatus.isVisible(in: form) { SubscriptionStatusRow(kind: .ed2kNodes) }
            }
            .disabled(readOnly)
        }
    }
}

/// HighID / LowID 检测行：调 `daemon.ed2k.idCheck`（慢方法），结果只存本行状态（离开页面即丢弃）。
private struct Ed2kIdCheckRow: View {
    @Environment(AppContainer.self) private var container

    @State private var running = false
    @State private var result: Ed2kIdCheckDisplay?

    var body: some View {
        let item = Ed2kSettingsRow.idCheck.item
        VStack(alignment: .leading, spacing: 4) {
            SettingsText(title: item.title, detail: item.detail)
            switch result {
            case let .ok(summary, lowHint):
                SettingsStatusLine(text: summary, tone: lowHint == nil ? .success : .warning)
                if let lowHint { Text(lowHint).font(.footnote).foregroundStyle(.secondary) }
            case let .failed(text):
                SettingsStatusLine(text: text, tone: .failure)
            case nil:
                EmptyView()
            }
            SettingsActionRow(
                title: L("ed2kIdCheckRun"), runningTitle: L("ed2kIdCheckRunning"),
                systemImage: "network", isRunning: running, action: run
            )
        }
        .settingsRow(item.id)
        .fluxAnimation(.smooth, value: result)
    }

    private func run() {
        guard !running else { return }
        running = true
        Task {
            do throws(HostError) {
                let network: Ed2kNetworkDto = try await container.session.call(HostMethod.daemonEd2kIdCheck)
                let display = Ed2kIdCheckDisplay.make(network)
                result = display
                if case .ok = display { FluxHaptic.success.play() } else { FluxHaptic.error.play() }
            } catch {
                result = .failure(ErrorText.describe(error))
                FluxHaptic.error.play()
            }
            running = false
        }
    }
}

// MARK: - 搜索索引 / 读数

extension Ed2kPage {
    /// 设置搜索索引：每个页签的所有可见行（与页面渲染共用 `Ed2kSettingsRow.isVisible`）。
    static func searchEntries(_ ctx: SettingsSearchContext) -> [SettingsEntry] {
        let name = L("settingsCatEd2k")
        return Ed2kSettingsRow.allCases.filter { $0.isVisible(in: ctx.form) }.map { row in
            SettingsEntry(
                item: row.item, route: .ed2k, breadcrumb: "\(name) › \(L(row.tab.titleKey))", symbol: "server.rack"
            )
        }
    }

    /// 设置首页该分类行的读数（nil = 使用分类说明文案）。
    static func readout(_ ctx: SettingsSearchContext) -> String? { Ed2kSettingsRow.readout(ctx.form) }
}
#endif
