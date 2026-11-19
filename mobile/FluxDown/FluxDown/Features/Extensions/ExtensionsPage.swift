import FluxDomain
import FluxUI
import SwiftUI
import UniformTypeIdentifiers
#if FULL_FLAVOR

/// S11 · 扩展：插件（已安装 / 市场 / 文件安装）与组件，**仅远端 `--server` 主机**。
///
/// iOS 本机引擎不带插件运行时（`native/mobile` 不启用 `plugins` feature）也没有可执行文件，下载并运行
/// 插件代码还有 App Store 2.5.2 风险；因此本机主机只显示一行说明，插件 / 市场 / 文件安装 / 组件整体隐藏。
/// 插件列表来自 `daemon.plugins` 分区；操作走通用通道 `daemon.plugin.*`。
struct ExtensionsPage: View {
    @Environment(AppContainer.self) private var container

    var body: some View {
        if container.isLocalHost {
            LocalHostExtensionsNote()
        } else {
            ExtensionsContent(container: container)
        }
    }
}

/// 本机主机：说明插件在已连接的 FluxDown 主机上运行。
private struct LocalHostExtensionsNote: View {
    var body: some View {
        SettingsPage(title: L("settingsCatExtensions")) {
            Section {
                Label(L("mobilePluginsRemoteOnlyNote"), systemImage: "externaldrive.badge.xmark")
                    .foregroundStyle(.secondary)
            }
        }
    }
}

private struct ExtensionsContent: View {
    @Environment(HostStore.self) private var store
    @Environment(AppContainer.self) private var container
    @State private var model: ExtensionsModel
    @State private var plugins = SectionMemo<[PluginDto]>(empty: [])
    @State private var importing = false
    @State private var uninstallTarget: PluginDto?

    init(container: AppContainer) {
        _model = State(initialValue: ExtensionsModel(container: container))
    }

    var body: some View {
        @Bindable var model = model
        let state = store.state
        let list = plugins.value(state.sections[HostSection.daemonPlugins])
        let readOnly = state.isReadOnly
        let tab = model.tab
        SettingsPage(title: L("settingsCatExtensions"), showsReadOnlyBanner: true) {
            Section {
                Picker(L("settingsCatExtensions"), selection: $model.tab) {
                    Text(L("settingsCatPlugins")).tag(ExtensionsModel.Tab.plugins)
                    Text(L("settingsCatComponents")).tag(ExtensionsModel.Tab.components)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets())
            }
            switch tab {
            case .plugins:
                pluginSections(list, readOnly: readOnly)
            case .components:
                ComponentsSections()
            }
        }
        .environment(model)
        .fluxAnimation(.smooth, value: tab)
        .pluginAutoDisabledToasts()
        .task(id: readOnly) { await model.ensureMarketLoaded() }
        .task(id: readOnly) { await model.loadSiteDefaults() }
        .fileImporter(isPresented: $importing, allowedContentTypes: Self.packageTypes, allowsMultipleSelection: false) { result in
            switch result {
            case let .success(urls):
                guard let url = urls.first else { return }
                Task { await model.installFile(url) }
            case let .failure(error):
                container.toasts.show(text: L("pluginOpInstallFailed", ["message": error.localizedDescription]), tone: .error)
            }
        }
        .sheet(item: $model.sheet) { sheet in
            PluginSheetHost(sheet: sheet)
                .environment(model)
        }
        .sheet(item: $model.permissionRequest) { request in
            PermissionConfirmSheet(request: request)
                .environment(model)
        }
        .alert(L("pluginDepsMissingTitle"), isPresented: Binding(
            get: { model.missingComponents != nil },
            set: { if !$0 { model.missingComponents = nil } }
        )) {
            Button(L("pluginDepsLater"), role: .cancel) {}
            Button(L("pluginDepsGoToComponents")) { model.goToComponents() }
        } message: {
            Text(missingMessage)
        }
    }

    private static let packageTypes: [UTType] = {
        var types: [UTType] = [.zip]
        if let fxplug = UTType("dev.fluxdown.fxplug") { types.append(fxplug) }
        if let byExtension = UTType(filenameExtension: "fxplug"), !types.contains(byExtension) { types.append(byExtension) }
        return types
    }()

    private var missingMessage: String {
        let names = (model.missingComponents ?? []).map(ComponentTitles.title(wire:)).joined(separator: ", ")
        return L("pluginDepsMissingBody", ["components": names])
    }

    // MARK: 插件

    @ViewBuilder
    private func pluginSections(_ list: [PluginDto], readOnly: Bool) -> some View {
        ResolverUrlConflictSection(plugins: list)
        Section {
            if list.isEmpty {
                Label(L("pluginsEmpty"), systemImage: "puzzlepiece.extension")
                    .foregroundStyle(.secondary)
            }
            ForEach(list) { plugin in
                NavigationLink {
                    PluginDetailPage(identity: plugin.identity)
                } label: {
                    PluginRow(
                        plugin: plugin,
                        update: model.update(for: plugin),
                        yanked: PluginMarket.installedVersionYanked(model.marketEntries, plugin: plugin)
                    )
                }
                // 卸载只弹确认框：不用 `role: .destructive`（会让 List 先行移除该行而数据未变，导致行数不一致崩溃）。
                .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                    Button(L("pluginUninstallTooltip"), systemImage: FluxSymbol.delete) { uninstallTarget = plugin }
                        .tint(Color.fdStatusFailed)
                        .disabled(readOnly)
                    if !plugin.loadFailed, !plugin.settings.isEmpty {
                        Button(L("pluginSettingsTooltip"), systemImage: "gearshape") { model.sheet = .settings(plugin.identity) }
                            .tint(Color.fdStatusPaused)
                    }
                }
                .swipeActions(edge: .leading, allowsFullSwipe: false) {
                    if let update = model.update(for: plugin) {
                        Button(L("marketUpdateButton"), systemImage: "arrow.down.circle") {
                            model.requestInstall(update, installed: plugin)
                        }
                        .tint(.accentColor)
                        .disabled(readOnly)
                    }
                    if !plugin.loadFailed, plugin.authSupported {
                        Button(L("pluginAuthButton"), systemImage: "key") { model.sheet = .auth(plugin.identity) }
                            .tint(Color.fdBoost)
                            .disabled(readOnly)
                    }
                }
                .contextMenu {
                    pluginMenu(plugin, readOnly: readOnly)
                }
                .alert(
                    L("pluginUninstallTitle"),
                    isPresented: Binding(
                        get: { uninstallTarget?.identity == plugin.identity },
                        set: { if !$0 { uninstallTarget = nil } }
                    )
                ) {
                    Button(L("pluginUninstallTooltip"), role: .destructive) { Task { await model.uninstall(plugin) } }
                    Button(L("cancel"), role: .cancel) {}
                } message: {
                    Text(L("pluginUninstallMsg", ["name": plugin.name]))
                }
            }
        } header: {
            Text(L("pluginsSectionTitle"))
        }

        if !model.siteDefaults.isEmpty {
            Section {
                ForEach(model.siteDefaults) { entry in
                    SiteDefaultRow(entry: entry, plugins: list, readOnly: readOnly)
                }
            } header: {
                Text(L("pluginSiteDefaultsTitle"))
            } footer: {
                Text(L("pluginSiteDefaultsDesc"))
            }
        }

        Section {
            Button {
                importing = true
            } label: {
                HStack {
                    Label(installLabel, systemImage: "square.and.arrow.down")
                    if model.installPhase != nil {
                        Spacer()
                        ProgressView()
                    }
                }
            }
            .disabled(readOnly || model.installPhase != nil)
            if case let .uploading(fraction) = model.installPhase {
                ProgressView(value: fraction)
                    .accessibilityLabel(installLabel)
            }
        }

        Section {
            NavigationLink {
                MarketPage()
            } label: {
                SettingsTileLabel(
                    title: L("marketSectionTitle"),
                    subtitle: L("marketSectionDesc"),
                    symbol: "bag.fill",
                    color: .purple
                )
            }
        }
    }

    private var installLabel: String {
        switch model.installPhase {
        case let .uploading(fraction): L("webPluginUploading", ["percent": Int((fraction * 100).rounded())])
        case .installing: L("marketInstallingButton")
        case nil: L("pluginInstallZipButton")
        }
    }

    @ViewBuilder
    private func pluginMenu(_ plugin: PluginDto, readOnly: Bool) -> some View {
        if let update = model.update(for: plugin) {
            Button(L("marketUpdateButton"), systemImage: "arrow.down.circle") { model.requestInstall(update, installed: plugin) }
                .disabled(readOnly)
        }
        if !plugin.loadFailed, !plugin.settings.isEmpty {
            Button(L("pluginSettingsTooltip"), systemImage: "gearshape") { model.sheet = .settings(plugin.identity) }
        }
        if !plugin.loadFailed, plugin.authSupported {
            Button(L("pluginAuthButton"), systemImage: "key") { model.sheet = .auth(plugin.identity) }
                .disabled(readOnly)
        }
        if plugin.devMode {
            Button(L("pluginReloadTooltip"), systemImage: FluxSymbol.retry) { Task { await model.reload(plugin) } }
                .disabled(readOnly)
        }
        if plugin.loadFailed, !plugin.loadError.isEmpty {
            Button(L("pluginLoadErrorCopy"), systemImage: FluxSymbol.copy) {
                ExtensionsClipboard.copy(plugin.loadError)
                container.toasts.show(text: L("pluginLoadErrorCopied"), tone: .success)
            }
        }
        Divider()
        Button(L("pluginUninstallTooltip"), systemImage: FluxSymbol.delete, role: .destructive) { uninstallTarget = plugin }
            .disabled(readOnly)
    }
}

/// 站点默认插件一行：站点名 + 候选插件选择器；未确认（automatic）的条目带提示与「确认」按钮。
private struct SiteDefaultRow: View {
    @Environment(ExtensionsModel.self) private var model
    let entry: PluginSiteDefault
    let plugins: [PluginDto]
    let readOnly: Bool

    private func name(_ identity: String) -> String {
        plugins.first { $0.identity == identity }?.name ?? identity
    }

    var body: some View {
        let busy = model.siteDefaultBusy.contains(entry.site)
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(entry.site).fontWeight(.medium).lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 8)
                Picker(entry.site, selection: Binding(
                    get: { entry.identity },
                    set: { new in
                        guard new != entry.identity else { return }
                        Task { await model.setSiteDefault(site: entry.site, identity: new) }
                    }
                )) {
                    ForEach(entry.candidates, id: \.self) { identity in
                        Text(name(identity)).tag(identity)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
            }
            if entry.automatic {
                Label(L("pluginSiteDefaultAutomatic"), systemImage: "exclamationmark.triangle.fill")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Color.fdStatusPaused)
                Text(L("pluginSiteDefaultAutomaticHint"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            HStack(spacing: 12) {
                if entry.automatic {
                    Button(L("pluginSiteDefaultConfirm")) {
                        Task { await model.setSiteDefault(site: entry.site, identity: entry.identity) }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
                Button(L("pluginSiteDefaultRemove"), role: .destructive) {
                    Task { await model.setSiteDefault(site: entry.site, identity: nil) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        }
        .padding(.vertical, 4)
        .disabled(readOnly || busy)
    }
}

// MARK: - 行

/// 已安装插件行（纯信息）：图标 · 名称 / 版本 / 主页 / 描述 / 徽标流 / 加载错误；启用开关在详情页。
struct PluginRow: View {
    let plugin: PluginDto
    let update: MarketEntry?
    let yanked: String?

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            SettingsTile(
                symbol: "puzzlepiece.extension.fill",
                color: plugin.loadFailed || plugin.disabledReason == "CircuitBreaker" ? Color.fdStatusFailed : .purple
            )
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(plugin.name).font(.headline)
                    Text(verbatim: "v\(plugin.version)")
                        .font(.footnote.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                if !plugin.homepage.isEmpty {
                    Text(plugin.homepage).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                }
                if !plugin.description.isEmpty {
                    Text(plugin.description)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
                badges
                if plugin.loadFailed, !plugin.loadError.isEmpty {
                    Text(plugin.loadError)
                        .font(.footnote)
                        .foregroundStyle(Color.fdStatusFailedText)
                        .lineLimit(2)
                }
            }
        }
        .accessibilityElement(children: .contain)
    }

    private var badges: some View {
        FlowLayout(spacing: 6) {
            if plugin.devMode {
                StatusBadge(text: L("pluginDevModeBadge"), tone: .accent, systemImage: "hammer")
            }
            if plugin.loadFailed {
                StatusBadge(text: L("pluginLoadStatusFailed"), tone: .failure, systemImage: FluxSymbol.warning)
            } else {
                StatusBadge(text: L("pluginLoadStatusLoaded"), tone: .success, systemImage: FluxSymbol.success)
            }
            if plugin.disabledReason == "Manual" {
                StatusBadge(text: L("pluginDisabledManual"), tone: .neutral, systemImage: "pause.circle")
            }
            if plugin.disabledReason == "CircuitBreaker" {
                StatusBadge(text: L("pluginDisabledCircuitBreaker"), tone: .failure, systemImage: FluxSymbol.cancelBoost)
            }
            if plugin.disabledReason == "Revoked" {
                StatusBadge(text: L("pluginDisabledRevoked"), tone: .failure, systemImage: "exclamationmark.octagon.fill")
            }
            if let yanked, let key = PluginMarket.yankedLabelKey(yanked) {
                StatusBadge(text: L("pluginInstalledVersionYanked", ["label": L(key)]), tone: .failure, systemImage: "exclamationmark.octagon.fill")
            }
            if let update {
                StatusBadge(text: L("pluginUpdateAvailable", ["version": update.version]), tone: .accent, systemImage: "arrow.down.circle")
            }
        }
    }
}

// MARK: - Sheet 路由

/// 设置表单 / 登录 / 加载错误详情：由根页统一呈现，按 identity 读取最新插件。
private struct PluginSheetHost: View {
    let sheet: ExtensionsModel.PluginSheet

    @Environment(HostStore.self) private var store
    @State private var plugins = SectionMemo<[PluginDto]>(empty: [])

    var body: some View {
        let list = plugins.value(store.state.sections[HostSection.daemonPlugins])
        switch sheet {
        case let .settings(identity):
            if let plugin = list.first(where: { $0.identity == identity }) {
                PluginSettingsSheet(plugin: plugin)
            }
        case let .auth(identity):
            if let plugin = list.first(where: { $0.identity == identity }) {
                PluginAuthSheet(plugin: plugin)
            }
        case let .loadError(identity):
            if let plugin = list.first(where: { $0.identity == identity }) {
                PluginLoadErrorSheet(plugin: plugin)
            }
        }
    }
}

// MARK: - 权限确认

private struct PermissionConfirmSheet: View {
    let request: ExtensionsModel.PermissionRequest

    @Environment(ExtensionsModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let entry = request.entry
        NavigationStack {
            List {
                if request.versionChanged {
                    Section {
                        Banner(text: L("pluginErrorMarketVersionChanged"), tone: .warning, systemImage: FluxSymbol.warning, slim: true)
                            .listRowInsets(EdgeInsets())
                            .listRowBackground(Color.clear)
                    }
                }
                Section {
                    ForEach(request.permissions, id: \.self) { permission in
                        PermissionRow(permission: permission)
                    }
                } header: {
                    Text(L(request.isUpdate ? "pluginPermConfirmUpdateBody" : "pluginPermConfirmInstallBody", ["version": entry.version]))
                        .textCase(nil)
                        .font(.subheadline)
                        .foregroundStyle(.primary)
                }
            }
            .navigationTitle(L(request.isUpdate ? "pluginPermConfirmUpdateTitle" : "pluginPermConfirmInstallTitle", ["name": entry.displayName]))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L("cancel")) {
                        model.permissionRequest = nil
                        dismiss()
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(L(request.isUpdate ? "pluginPermConfirmUpdateOk" : "pluginPermConfirmInstallOk")) {
                        model.confirmInstall(request)
                    }
                    .fontWeight(.semibold)
                }
            }
        }
        .presentationDetents([.medium, .large])
        .interactiveDismissDisabled()
    }
}

/// 权限行：名称 + 说明；未知权限原样显示名称并以橙色标注「未知权限」。
struct PermissionRow: View {
    let permission: String

    var body: some View {
        let keys = PluginMarket.permissionKeys(permission)
        VStack(alignment: .leading, spacing: 2) {
            Text(keys.map { L($0.name) } ?? permission)
                .font(.body.weight(.medium))
            Text(L(keys?.desc ?? "pluginPermUnknownDesc"))
                .font(.footnote)
                .foregroundStyle(keys == nil ? Color.fdStatusWarningText : .secondary)
        }
        .accessibilityElement(children: .combine)
    }
}

struct ResolverUrlConflictSection: View {
    let plugins: [PluginDto]
    var identities: Set<String>? = nil

    private func matches(_ identity: String, _ otherIdentity: String) -> Bool {
        guard let identities else { return true }
        return identities.contains(identity) || identities.contains(otherIdentity)
    }

    var body: some View {
        let owners = plugins.filter { plugin in
            plugin.resolverUrlConflicts.contains { matches(plugin.identity, $0.otherIdentity) }
        }
        if !owners.isEmpty {
            Section {
                if owners.contains(where: { plugin in
                    plugin.resolverUrlConflicts.contains {
                        plugin.identity != $0.otherIdentity && matches(plugin.identity, $0.otherIdentity)
                    }
                }) {
                    Text(L("pluginResolverUrlConflictsHint"))
                        .foregroundStyle(.secondary)
                }
                ForEach(owners) { plugin in
                    ForEach(plugin.resolverUrlConflicts.filter {
                        matches(plugin.identity, $0.otherIdentity)
                    }, id: \.self) { conflict in
                        VStack(alignment: .leading) {
                            if plugin.identity == conflict.otherIdentity {
                                Text(L("pluginResolverUrlDuplicateHint"))
                                    .foregroundStyle(.secondary)
                            }
                            Text(verbatim: "\(plugin.identity): \(conflict.pattern)")
                            Text(verbatim: "\(conflict.otherIdentity): \(conflict.otherPattern)")
                        }
                    }
                }
            } header: {
                Label(L("pluginResolverUrlConflictsTitle"), systemImage: "exclamationmark.triangle")
            }
        }
    }
}
#endif
