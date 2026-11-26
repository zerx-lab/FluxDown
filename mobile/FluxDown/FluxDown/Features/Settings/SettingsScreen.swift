import FluxDomain
import FluxUI
import SwiftUI

/// S1 · 设置首页（Tab 根页）。分类顺序见 03-settings §1.1；各分类行的读数 / 搜索条目由页面自己提供
/// （`XxxPage.readout` / `XxxPage.searchEntries`，与页面渲染共用同一份可见性判定）。
///
/// `NavigationStack(path: router.settingsPath)`；主机范围标签 + 切换菜单。
/// 断线时「引擎」分组的页眉追加「离线」标记，分类行仍可点进（页内只读）。
struct SettingsScreen: View {
    @Environment(AppContainer.self) private var container

    var body: some View {
        SettingsStack(container: container)
    }
}

private struct SettingsStack: View {
    @Environment(AppRouter.self) private var router

    @State private var editor: ConfigEditor
    private let focus = SettingsFocus.shared

    init(container: AppContainer) {
        _editor = State(initialValue: ConfigEditor(transport: ContainerConfigTransport(container)))
    }

    var body: some View {
        @Bindable var router = router
        NavigationStack(path: $router.settingsPath) {
            SettingsHome()
                .navigationDestination(for: SettingsRoute.self) { route in
                    switch route {
                    case .account: AccountPage()
                    case .general: GeneralPage()
                    case .appearance: AppearancePage()
                    case .notify: NotifyPage()
                    case .download: DownloadPage()
                    #if FULL_FLAVOR
                    case .bt: BtPage()
                    case .ed2k: Ed2kPage()
                    #endif
                    case .network: NetworkPage()
                    #if FULL_FLAVOR
                    case .extensions: ExtensionsPage()
                    #endif
                    case .webhook: WebhookPage()
                    case .api: ApiServicePage()
                    case .diagnostics: DiagnosticsPage()
                    case .about: AboutPage()
                    }
                }
        }
        .environment(editor)
        .environment(focus)
    }
}

// MARK: - 首页

private struct SettingsHome: View {
    @State private var query = ""

    var body: some View {
        SettingsHomeList(query: query)
            .rootNavigationTitle(L("settings"))
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .automatic), prompt: L("settingsSearchHint"))
            .toolbar {
                ToolbarItem(placement: .primaryAction) { GlobalSearchButton() }
            }
    }
}

private struct SettingsHomeList: View {
    let query: String

    @Environment(AppContainer.self) private var container
    @Environment(AppRouter.self) private var router
    @Environment(HostStore.self) private var store
    @Environment(AppearanceStore.self) private var appearance
    @Environment(SettingsFocus.self) private var focus
    @Environment(\.dismissSearch) private var dismissSearch

    var body: some View {
        let state = store.state
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        List {
            if !trimmed.isEmpty {
                results(for: trimmed, state: state)
            } else {
                categories(state: state)
            }
        }
        .readableContentWidth()
        .overlay {
            if !trimmed.isEmpty, SettingsSearch.filter(entries(state: state), query: trimmed).isEmpty {
                ContentUnavailableView.search(text: trimmed)
            }
        }
        .fluxAnimation(.smooth, value: trimmed.isEmpty)
    }

    // MARK: 分类

    @ViewBuilder
    private func categories(state: HostState) -> some View {
        let ctx = searchContext(state)
        if state.isReadOnly {
            Section {
                Banner(text: L("localServiceDisconnected"), tone: .warning, systemImage: FluxSymbol.offline, slim: true)
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }
        }

        if state.has("agent.auth") {
            Section {
                tile(.account, L("settingsCatAccount"), L("settingsCatAccountDesc"), "person.crop.circle.fill", .blue)
            }
        }

        Section {
            SettingsHostMenu { hostRow(state: state) }
        }

        Section(L("settingsGroupPersonal")) {
            tile(.general, L("settingsCatGeneral"), L("settingsCatGeneralDesc"), "gearshape.fill", .gray)
            tile(.appearance, L("settingsCatAppearance"), appearanceReadout(), "paintpalette.fill", .pink)
            tile(.notify, L("settingsCatNotify"), NotifyPage.readout(ctx) ?? L("settingsCatNotifyDesc"), "bell.badge.fill", .red)
        }

        Section {
            tile(.download, L("settingsCatDownload"), downloadReadout(state.config), "arrow.down.circle.fill", .accentColor)
            #if FULL_FLAVOR
            tile(
                .bt, L("settingsCatBt"), BtPage.readout(ctx) ?? L("settingsCatBtDesc"),
                "point.3.connected.trianglepath.dotted", .green
            )
            tile(.ed2k, L("ed2kSettings"), Ed2kPage.readout(ctx) ?? L("ed2kSettingsDesc"), "server.rack", .orange)
            #endif
            tile(.network, L("settingsCatProxy"), NetworkPage.readout(ctx) ?? L("settingsCatProxyDesc"), "globe", .blue)
            #if FULL_FLAVOR
            tile(.extensions, L("settingsCatExtensions"), L("settingsCatExtensionsDesc"), "puzzlepiece.extension.fill", .purple)
            #endif
            // 功能开关关闭时无入口；开启后 `ui.show_activity_webhooks` = false 仍可隐藏首页行（搜索仍可进入）。
            if FeatureFlags.webhooks, state.preferences.bool("ui.show_activity_webhooks", default: true) {
                tile(.webhook, L("webhookNavTitle"), nil, "bolt.horizontal.fill", .indigo)
            }
            if !container.isLocalHost, state.has("agent.gateway") {
                tile(.api, L("settingsCatApiService"), L("settingsCatApiServiceDesc"), "curlybraces", .cyan)
            }
        } header: {
            engineHeader(state: state)
        }

        Section {
            tile(.diagnostics, L("settingsCatDoctor"), DiagnosticsPage.readout(ctx) ?? L("settingsCatDoctorDesc"), "stethoscope", .mint)
            tile(.about, L("settingsCatAbout"), "v\(SettingsAppVersion.current)", "info.circle.fill", .gray)
        } header: {
            Text(L("settingsGroupMaintenance"))
        } footer: {
            Text(L("mobileFooter"))
                .frame(maxWidth: .infinity)
                .multilineTextAlignment(.center)
                .padding(.top, 12)
        }
    }

    private func searchContext(_ state: HostState) -> SettingsSearchContext {
        SettingsIndex.context(state: state, isLocalHost: container.isLocalHost)
    }

    private func tile(_ route: SettingsRoute, _ title: String, _ subtitle: String?, _ symbol: String, _ color: Color) -> some View {
        NavigationLink(value: route) {
            SettingsTileLabel(title: title, subtitle: subtitle, symbol: symbol, color: color)
        }
    }

    /// 当前主机卡：名称 + 副标题 + 连接状态；点按弹出主机切换菜单。
    private func hostRow(state: HostState) -> some View {
        let host = container.host
        return HStack(spacing: 12) {
            SettingsTileLabel(
                title: host.displayName,
                subtitle: Self.subtitle(host),
                symbol: host.symbolName,
                color: host.isLocal ? .gray : .indigo
            )
            Spacer(minLength: 8)
            ConnectionBadge(connection: state.connection)
            Image(systemName: "chevron.up.chevron.down")
                .font(.footnote.weight(.semibold))
                .foregroundStyle(.tertiary)
                .accessibilityHidden(true)
        }
        .contentShape(.rect)
        .accessibilityElement(children: .combine)
        .accessibilityHint(L("mobileSettingsSwitchHost", ["name": host.displayName]))
    }

    /// 「引擎 · 作用于 [主机]」页眉：说明这一组设置写到哪台主机；胶囊可切换主机。
    private func engineHeader(state: HostState) -> some View {
        HStack(spacing: 8) {
            Text(L("settingsGroupEngine"))
            SettingsHostMenu {
                Label(container.host.displayName, systemImage: "externaldrive.connected.to.line.below")
                    .font(.footnote.weight(.semibold))
                    .lineLimit(1)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(.fill.tertiary, in: .capsule)
                    .frame(minHeight: 44)
                    .contentShape(.rect)
            }
            .accessibilityLabel(L("mobileSettingsSwitchHost", ["name": container.host.displayName]))
            if state.isReadOnly {
                Label(L("mobileHostOffline"), systemImage: FluxSymbol.offline)
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.fdStatusWarningText)
            }
            Spacer(minLength: 0)
        }
        .textCase(nil)
    }

    private static func subtitle(_ host: HostRef) -> String {
        switch host {
        case .local: L("mobileHostLocalEngine")
        case let .remote(_, _, endpoint): endpoint
        }
    }

    // MARK: 读数

    private func appearanceReadout() -> String {
        let mode: String = switch appearance.mode {
        case .system: L("themeModeSystem")
        case .light: L("themeModeLight")
        case .dark: L("themeModeDark")
        }
        let accent: String = appearance.scheme == "custom"
            ? L("colorCustom") + " " + AppearancePage.hex(appearance.customColor)
            : AccentName.of(appearance.scheme)
        return [mode, accent].joined(separator: " · ")
    }

    /// 下载读数：默认目录末段 · 并发数 [· 限速]；全部来自 `config`。
    private func downloadReadout(_ config: [String: String]) -> String {
        let dir = container.isLocalHost ? "" : SettingsSaveDirectory.lastComponent(config["default_save_dir"] ?? "")
        let concurrent = config["max_concurrent_tasks"]
        let limit = config["speed_limit_bytes"].flatMap { Int64($0) } ?? 0
        var parts: [String] = []
        switch (dir.isEmpty, concurrent) {
        case let (false, n?): parts.append(L("mobileSettingsReadoutDownload", ["dir": dir, "n": n]))
        case let (true, n?): parts.append(L("mobileSettingsReadoutConcurrent", ["n": n]))
        case (false, nil): parts.append(dir)
        case (true, nil): break
        }
        if let speed = Format.speed(limit) {
            parts.append(L("mobileSettingsReadoutLimit", ["speed": speed.description]))
        }
        return parts.joined(separator: " · ")
    }

    // MARK: 搜索

    @ViewBuilder
    private func results(for query: String, state: HostState) -> some View {
        let hits = SettingsSearch.filter(entries(state: state), query: query)
        if !hits.isEmpty {
            Section {
                ForEach(hits) { entry in
                    Button {
                        open(entry)
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: entry.symbol)
                                .foregroundStyle(.secondary)
                                .frame(width: 24)
                                .accessibilityHidden(true)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(highlight(entry.title, query: query))
                                Text(entry.breadcrumb)
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer(minLength: 8)
                            Image(systemName: "chevron.right")
                                .font(.footnote.weight(.semibold))
                                .foregroundStyle(.tertiary)
                                .accessibilityHidden(true)
                        }
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .accessibilityElement(children: .combine)
                }
            }
        }
    }

    private func open(_ entry: SettingsEntry) {
        focus.request(entry.id)
        dismissSearch()
        router.settingsPath = [entry.route]
    }

    /// 匹配字符加粗 + 强调色。
    private func highlight(_ title: String, query: String) -> AttributedString {
        var text = AttributedString(title)
        let options: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive, .widthInsensitive]
        if let range = title.range(of: query.trimmingCharacters(in: .whitespacesAndNewlines), options: options),
           let target = Range(range, in: text)
        {
            text[target].font = .body.bold()
            text[target].foregroundColor = .accentColor
        }
        return text
    }

    /// 索引 = `SettingsIndex`（各页自己贡献的行 + 每个分类页入口），与全局搜索共用。
    private func entries(state: HostState) -> [SettingsEntry] {
        SettingsIndex.entries(state: state, isLocalHost: container.isLocalHost)
    }
}

// MARK: - 连接状态

/// 连接状态读数：徽标（文字 + 图标 + 语气，不只靠颜色）。
private struct ConnectionBadge: View {
    let connection: Connection

    var body: some View {
        switch connection {
        case .live:
            StatusBadge(text: L("mobileSettingsConnLive"), tone: .success, systemImage: FluxSymbol.success)
        case .connecting:
            StatusBadge(text: L("mobileSettingsConnConnecting"), tone: .warning, systemImage: FluxSymbol.syncing)
        case .stale:
            StatusBadge(text: L("mobileSettingsConnStale"), tone: .warning, systemImage: FluxSymbol.syncing)
        case .failed:
            StatusBadge(text: L("mobileSettingsConnFailed"), tone: .failure, systemImage: FluxSymbol.warning)
        }
    }
}

/// 强调色名称（预设 id → 文案）。
private enum AccentName {
    static func of(_ scheme: String) -> String {
        switch scheme {
        case "blue": L("colorBlue")
        case "green": L("colorGreen")
        case "violet": L("colorViolet")
        case "rose": L("colorRose")
        case "orange": L("colorOrange")
        case "indigo": L("colorIndigo")
        default: L("colorBlue")
        }
    }
}

// MARK: - 主机切换菜单

/// 主机切换菜单（与 Devices 页 / 下载页标题菜单同一份主机数据）：单选已保存主机 + 「添加主机」。
private struct SettingsHostMenu<Label: View>: View {
    @Environment(AppContainer.self) private var container
    @Environment(AppRouter.self) private var router
    @ViewBuilder let label: Label

    var body: some View {
        Menu {
            Picker(L("mobileHostSwitchTitle"), selection: Binding(get: { container.host.id }, set: switchTo)) {
                ForEach(container.hosts) { host in
                    Text(host.displayName).tag(host.id)
                }
            }
            .pickerStyle(.inline)
            Divider()
            Button(L("mobileHostAdd"), systemImage: FluxSymbol.add) { router.sheet = .addHost }
        } label: {
            label
        }
        .disabled(container.isSwitching)
    }

    private func switchTo(_ id: String) {
        guard let target = container.hosts.first(where: { $0.id == id }) else { return }
        Task { await HostFlow.switchTo(target, container: container) }
    }
}
