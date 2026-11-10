import FluxDomain
import FluxUI
import SwiftUI
import UIKit

/// S8 · 网络与代理（03-settings §9）：代理模式（none / system / manual / auto）、系统代理检测、手动配置、
/// 连通性测试、已保存的网站凭据。`proxy_*` 键是主机配置、不参与云同步（不显示 ☁︎）。
/// 写入全部经 `ConfigEditor`；断线（只读）时主机侧控件置灰。
struct NetworkPage: View {
    @Environment(AppContainer.self) private var container
    @Environment(HostStore.self) private var store
    @Environment(ConfigEditor.self) private var editor
    @Environment(\.dynamicTypeSize) private var typeSize

    @State private var proxy = NetworkProxyModel()
    @State private var siteAuth = SiteAuthModel()
    @State private var rules: ProxyRulesModel?
    /// 站点规则的排序态（Section header 的「排序 / 完成」切换；`Form` 的 `editMode` 驱动拖动手柄）。
    @State private var rulesEditMode: EditMode = .inactive

    /// 系统代理检测的触发条件：切到系统模式 / 切换主机 / 连接恢复时重新检测。
    private nonisolated struct DetectKey: Hashable {
        let hostID: String
        let isLive: Bool
        let isSystemMode: Bool
    }

    private nonisolated struct SiteAuthKey: Hashable {
        let hostID: String
        let isLive: Bool
    }

    var body: some View {
        let form = editor.form
        let mode = NetworkProxyMode(wire: form.value("proxy_mode"))
        let readOnly = store.state.isReadOnly
        let isLive = store.state.connection == .live
        let manualRequest = NetworkProxyTest.manual(form: form)
        let systemRequest = NetworkProxyTest.system(dto: proxy.detection.system)
        let ruleCount = ProxyRule.enabledCount(form.value("proxy_rules"))
        SettingsPage(title: L("settingsCatProxy"), showsReadOnlyBanner: true) {
            if form.isLoaded {
                Section {
                    modeRow(mode)
                    if ruleCount > 0 { rulesActiveRow(ruleCount) }
                } footer: {
                    #if FULL_FLAVOR
                    Text(L("proxyBtNote"))
                    #endif
                }
                .disabled(readOnly)

                switch mode {
                case .auto:
                    autoSection
                case .system:
                    systemSection(request: systemRequest, readOnly: readOnly)
                case .none, .manual:
                    EmptyView()
                }
                manualSection(mode: mode, readOnly: readOnly)
                if let rules {
                    ProxyRulesSection(model: rules, editMode: $rulesEditMode)
                }
            } else {
                Section {
                    HStack(spacing: 10) {
                        ProgressView()
                        Text(L("mobileLoading")).foregroundStyle(.secondary)
                    }
                }
            }
            SiteAuthSection(model: siteAuth)
        }
        .environment(\.editMode, $rulesEditMode)
        .fluxAnimation(.smooth, value: mode)
        .fluxAnimation(.smooth, value: proxy.detection)
        .task(id: DetectKey(hostID: container.host.id, isLive: isLive, isSystemMode: mode == .system)) {
            guard isLive, mode == .system else { return }
            await proxy.detect(using: container.session)
        }
        .task(id: SiteAuthKey(hostID: container.host.id, isLive: isLive)) {
            await siteAuth.load(using: container.session, hostID: container.host.id)
        }
        // 两路测试参数各自变化才清各自的结果；切主机两路都清。
        .onChange(of: manualRequest) { _, _ in proxy.resetTest(.manual) }
        .onChange(of: systemRequest) { _, _ in proxy.resetTest(.system) }
        .onChange(of: container.host.id) { _, _ in
            proxy.resetTests()
            rules?.resetExplain()
        }
        .onAppear { if rules == nil { rules = ProxyRulesModel(container: container) } }
    }

    /// 站点规则优先于代理模式的提示。
    private func rulesActiveRow(_ count: Int) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "info.circle")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text(L("proxyRulesActiveHint", ["count": count]))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }

    // MARK: 代理模式

    private func modeRow(_ mode: NetworkProxyMode) -> some View {
        let item = NetworkRow.mode
        return Picker(selection: Binding(
            get: { mode },
            set: { editor.set("proxy_mode", $0.rawValue) }
        )) {
            ForEach(NetworkProxyMode.allCases) { Text(L($0.titleKey)).tag($0) }
        } label: {
            SettingsText(title: L(item.titleKey), detail: item.detailKey.map { L($0) })
        }
        .pickerStyle(.navigationLink)
        .settingsRow(item.id, failureKey: "proxy_mode")
    }

    // MARK: 自动

    private var autoSection: some View {
        Section {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "info.circle")
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(L("proxyModeAutoDesc"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .accessibilityElement(children: .combine)
        }
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    // MARK: 系统代理

    private func systemSection(request: ProxyTestRequest?, readOnly: Bool) -> some View {
        Section {
            systemStatusRow
            if let dto = proxy.detection.system {
                SettingsInfoRow(title: L("proxyType"), value: dto.proxyType.uppercased())
                    .settingsRow("network.system.type")
                SettingsInfoRow(title: L("proxyHost"), value: dto.host)
                    .settingsRow("network.system.host")
                SettingsInfoRow(title: L("proxyPort"), value: String(dto.port))
                    .settingsRow("network.system.port")
                if !dto.noList.isEmpty {
                    SettingsInfoRow(title: L("proxyNoList"), value: dto.noList)
                        .settingsRow("network.system.noList")
                }
                if let request { testRow(request, test: proxy.systemTest, rowID: "network.system.test", source: .system) }
            }
        } header: {
            Text(L("proxyModeSystem"))
        } footer: {
            Text(L("proxyModeSystemDesc"))
        }
        .disabled(readOnly)
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    @ViewBuilder
    private var systemStatusRow: some View {
        switch proxy.detection {
        case .idle, .detecting:
            HStack(spacing: 10) {
                ProgressView()
                Text(L("proxySystemDetecting"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .frame(minHeight: 44, alignment: .leading)
            .accessibilityElement(children: .combine)
        case let .detected(dto):
            if dto.detected {
                SettingsStatusLine(text: L("proxySystemDetected"), tone: .success)
            } else {
                SettingsStatusLine(text: L("proxySystemNotConfigured"), tone: .neutral)
            }
        case let .failed(message):
            VStack(alignment: .leading, spacing: 4) {
                SettingsStatusLine(text: message, tone: .failure)
                SettingsActionRow(title: L("mobileRetry"), systemImage: FluxSymbol.retry) {
                    let session = container.session
                    Task { await proxy.detect(using: session) }
                }
            }
        }
    }

    // MARK: 手动

    private func manualSection(mode: NetworkProxyMode, readOnly: Bool) -> some View {
        Section {
            typeRow
            ConfigTextRow(
                id: NetworkRow.host.id, key: "proxy_host", title: L("proxyHost"),
                prompt: L("proxyHostPlaceholder"), keyboard: .URL
            )
            NetworkPortRow()
            ConfigTextRow(
                id: NetworkRow.username.id, key: "proxy_username", title: L("proxyUsername"),
                prompt: L("proxyUsernamePlaceholder"), monospaced: false
            )
            ConfigTextRow(
                id: NetworkRow.password.id, key: "proxy_password", title: L("proxyPassword"),
                prompt: L("proxyPasswordPlaceholder"), isSecure: true
            )
            ConfigTextRow(
                id: NetworkRow.noList.id, key: "proxy_no_list", title: L("proxyNoList"),
                detail: L("proxyNoListDesc"), prompt: L("proxyNoListPlaceholder")
            )
            testRow(NetworkProxyTest.manual(form: editor.form), test: proxy.manualTest, rowID: NetworkRow.test.id, source: .manual)
        } header: {
            Text(L("proxyModeManual"))
        } footer: {
            Text(mode == .manual ? L("proxyModeManualDesc") : L("proxyManualKeptDesc"))
        }
        .disabled(readOnly)
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    /// HTTP / HTTPS / SOCKS4 / SOCKS5：分段控件；辅助功能字号下四段放不下，改用菜单。
    @ViewBuilder
    private var typeRow: some View {
        if let item = NetworkRow.type.item {
            let options = ["http", "https", "socks4", "socks5"].map { SettingsOption(id: $0, label: $0.uppercased()) }
            if typeSize.isAccessibilitySize {
                ConfigPickerRow(item: item, options: options, fallback: "http")
            } else {
                ConfigSegmentedRow(item: item, options: options, fallback: "http")
            }
        }
    }

    private func testRow(
        _ request: ProxyTestRequest, test: NetworkProxyModel.Test, rowID: String, source: NetworkProxyModel.TestSource
    ) -> some View {
        NetworkTestRow(test: test, rowID: rowID) {
            let session = container.session
            Task { await proxy.runTest(request, source: source, using: session) }
        }
    }
}

// MARK: - 测试连接

/// `proxyTestConnection`：慢 RPC——行内 `ProgressView` + 「测试中…」，页面其余部分保持可交互；
/// 结果行成功 / 失败带图标与触感，延迟数字用 `numericText` 过渡，失败原因可长按选择复制。
private struct NetworkTestRow: View {
    let test: NetworkProxyModel.Test
    let rowID: String
    let run: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            SettingsActionRow(
                title: L("proxyTestConnection"), runningTitle: L("proxyTesting"), systemImage: "network",
                isRunning: test == .running, action: run
            )
            result
        }
        .settingsRow(rowID)
        .fluxAnimation(.smooth, value: test)
        .sensoryFeedback(trigger: test) { @Sendable _, new in
            switch new {
            case .success: FluxHaptic.success.sensoryFeedback
            case .failure: FluxHaptic.error.sensoryFeedback
            case .idle, .running: nil
            }
        }
        .onChange(of: test) { _, new in
            // VoiceOver：结果出现在按钮下方，朗读一次让用户知道结果。
            switch new {
            case .success, .failure: UIAccessibility.post(notification: .announcement, argument: resultText(new))
            case .idle, .running: break
            }
        }
    }

    @ViewBuilder
    private var result: some View {
        switch test {
        case .idle, .running:
            EmptyView()
        case .success:
            SettingsStatusLine(text: resultText(test) ?? "", tone: .success)
                .contentTransition(.numericText())
                .transition(.opacity)
        case .failure:
            SettingsStatusLine(text: resultText(test) ?? "", tone: .failure)
                .transition(.opacity)
        }
    }

    private func resultText(_ state: NetworkProxyModel.Test) -> String? {
        switch state {
        case let .success(latencyMs): L("proxyTestSuccess", ["ms": latencyMs])
        case let .failure(detail): L("proxyTestFailed", ["error": detail])
        case .idle, .running: nil
        }
    }
}

// MARK: - 端口

/// `proxy_port`：文本；只接受 ASCII 数字，提交（失焦 / 完成 / 离开页面）时钳位到 1–65535，空 = 清空。
private struct NetworkPortRow: View {
    @Environment(ConfigEditor.self) private var editor
    @State private var draft = ""
    @State private var adjustedTo: Int?
    @State private var warnTick = 0
    @FocusState private var focused: Bool

    private var current: String { editor.form.string("proxy_port") }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            SettingsText(title: L("proxyPort"))
            TextField(L("proxyPort"), text: $draft, prompt: Text(L("proxyPortPlaceholder")))
                .keyboardType(.numberPad)
                .font(.fluxMono)
                .monospacedDigit()
                .focused($focused)
                .onSubmit(commit)
                .textFieldStyle(.roundedBorder)
                .frame(minHeight: 44)
            if let adjustedTo {
                Text(L("mobileAdjustedTo", ["n": adjustedTo]))
                    .font(.caption)
                    .foregroundStyle(Color.fdStatusWarningText)
                    .transition(.opacity)
            }
        }
        .settingsRow(NetworkRow.port.id, failureKey: "proxy_port")
        .fluxAnimation(.smooth, value: adjustedTo)
        .sensoryFeedback(FluxHaptic.warning.sensoryFeedback, trigger: warnTick)
        .onAppear { draft = current }
        .onChange(of: current) { _, new in if !focused { draft = new } }
        .onChange(of: draft) { _, new in
            let digits = NetworkPortInput.digits(new)
            if digits != new { draft = digits }
        }
        .onChange(of: focused) { _, isFocused in if !isFocused { commit() } }
        .onDisappear { commit() }
    }

    private func commit() {
        let result = NetworkPortInput.commit(draft)
        draft = result.wire
        if let adjusted = result.adjusted {
            warnTick += 1
            adjustedTo = adjusted
            Task {
                do {
                    try await Task.sleep(for: .seconds(3))
                } catch {
                    return
                }
                adjustedTo = nil
            }
        } else {
            adjustedTo = nil
        }
        if result.wire != current { editor.set("proxy_port", result.wire) }
    }
}

// MARK: - 搜索索引

extension NetworkPage {
    /// 本页所有可见行：代理模式、手动字段与测试按钮、站点规则、站点凭据（标题 / 添加 / 全部清除）。
    /// 手动字段在配置已加载时始终进索引，不随模式隐藏。系统模式的测试按钮取决于异步检测结果，不进索引。
    static func searchEntries(_ ctx: SettingsSearchContext) -> [SettingsEntry] {
        let name = L("settingsCatProxy")
        return NetworkRow.visible(isLoaded: ctx.form.isLoaded).map { row in
            SettingsEntry(
                id: row.id, route: .network, title: L(row.titleKey), detail: row.detailKey.map { L($0) } ?? "",
                breadcrumb: row.titleKey == row.group.titleKey ? name : "\(name) › \(L(row.group.titleKey))",
                symbol: row.symbol
            )
        }
    }

    /// 设置首页读数：当前代理模式，启用站点规则时追加规则数（配置未加载时 nil）。
    static func readout(_ ctx: SettingsSearchContext) -> String? {
        guard ctx.form.isLoaded else { return nil }
        let mode = L(NetworkProxyMode(wire: ctx.form.value("proxy_mode")).titleKey)
        let count = ProxyRule.enabledCount(ctx.form.value("proxy_rules"))
        return count > 0 ? "\(mode) · \(L("proxyRulesCount", ["count": count]))" : mode
    }
}
