import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

/// S11.3 · 插件详情（已安装）：manifest 信息 + 操作区（启用 / 设置 / 登录 / 更新 / 重载 / 卸载）。
struct PluginDetailPage: View {
    let identity: String

    @Environment(AppContainer.self) private var container
    @Environment(HostStore.self) private var store
    @Environment(ExtensionsModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var plugins = SectionMemo<[PluginDto]>(empty: [])
    @State private var confirmUninstall = false
    @State private var showFeedback = false

    var body: some View {
        let state = store.state
        let plugin = plugins.value(state.sections[HostSection.daemonPlugins]).first { $0.identity == identity }
        let readOnly = state.isReadOnly
        Group {
            if let plugin {
                content(plugin, readOnly: readOnly)
            } else {
                Color.clear
            }
        }
        .navigationTitle(plugin?.name ?? "")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(isPresented: $showFeedback) {
            if let plugin {
                PluginFeedbackSheet(
                    identity: plugin.identity, version: plugin.version,
                    loadStatus: plugin.loadStatus,
                    allowsPluginReport: !plugin.devMode && plugin.source != "private"
                )
                .id([plugin.identity, plugin.version, String(plugin.devMode), plugin.source])
            }
        }
        .onChange(of: plugin == nil) { _, gone in if gone { dismiss() } }
        .onChange(of: model.popRequest) { _, _ in dismiss() }
    }

    private func content(_ plugin: PluginDto, readOnly: Bool) -> some View {
        let busy = readOnly || model.busy.contains(plugin.identity)
        let yanked = PluginMarket.installedVersionYanked(model.marketEntries, plugin: plugin)
        return Form {
            Section {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(verbatim: "v\(plugin.version)").font(.subheadline.monospacedDigit())
                        if plugin.devMode { StatusBadge(text: L("pluginDevModeBadge"), tone: .accent, systemImage: "hammer") }
                        if plugin.loadFailed {
                            StatusBadge(text: L("pluginLoadStatusFailed"), tone: .failure, systemImage: FluxSymbol.warning)
                        } else {
                            StatusBadge(text: L("pluginLoadStatusLoaded"), tone: .success, systemImage: FluxSymbol.success)
                        }
                        if let yanked, let key = PluginMarket.yankedLabelKey(yanked) {
                            StatusBadge(text: L("pluginInstalledVersionYanked", ["label": L(key)]), tone: .failure, systemImage: "exclamationmark.octagon.fill")
                        }
                    }
                }
            }

            if plugin.disabledReason == "CircuitBreaker" {
                Section {
                    Banner(text: L("pluginDisabledCircuitBreaker"), tone: .error, systemImage: FluxSymbol.cancelBoost, slim: true)
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                    Button(L("pluginAutoDisabledReenable")) { Task { await model.setEnabled(plugin, true) } }
                        .disabled(busy || plugin.loadFailed)
                }
            }

            if plugin.loadFailed {
                Section {
                    Text(plugin.loadError.isEmpty ? L("pluginLoadErrorTitle") : plugin.loadError)
                        .font(.footnote)
                        .foregroundStyle(Color.fdStatusFailedText)
                        .textSelection(.enabled)
                    Button {
                        model.sheet = .loadError(plugin.identity)
                    } label: {
                        Label(L("pluginLoadErrorTitle"), systemImage: "exclamationmark.bubble")
                    }
                    Button {
                        copyError(plugin)
                    } label: {
                        Label(L("pluginLoadErrorCopy"), systemImage: FluxSymbol.copy)
                    }
                    .disabled(plugin.loadError.isEmpty)
                }
            }

            PluginInfoSections(detail: PluginDetail(plugin: plugin))

            Section {
                Button { showFeedback = true } label: {
                    Label(L("plugFeedbackTitle"), systemImage: "exclamationmark.bubble")
                }
            }

            Section {
                Toggle(isOn: Binding(
                    get: { plugin.enabled && !plugin.loadFailed },
                    set: { enabled in Task { await model.setEnabled(plugin, enabled) } }
                )) {
                    Text(L("rssEnabledLabel"))
                }
                .tint(Color.fdToggleOn)
                .disabled(busy || plugin.loadFailed)

                if !plugin.loadFailed, !plugin.settings.isEmpty {
                    Button { model.sheet = .settings(plugin.identity) } label: {
                        Label(L("pluginSettingsTooltip"), systemImage: "gearshape")
                    }
                    .disabled(busy)
                }
                if !plugin.loadFailed, plugin.authSupported {
                    Button { model.sheet = .auth(plugin.identity) } label: {
                        Label(L("pluginAuthButton"), systemImage: "key")
                    }
                    .disabled(busy)
                }
                if let update = model.update(for: plugin) {
                    Button { model.requestInstall(update, installed: plugin) } label: {
                        Label(L("pluginUpdateAvailable", ["version": update.version]), systemImage: "arrow.down.circle")
                    }
                    .disabled(busy || model.marketPending.contains(plugin.identity))
                }
                if plugin.devMode {
                    Button { Task { await model.reload(plugin) } } label: {
                        Label(L("pluginReloadTooltip"), systemImage: FluxSymbol.retry)
                    }
                    .disabled(busy)
                }
                Button(role: .destructive) { confirmUninstall = true } label: {
                    Label(L("pluginUninstallTooltip"), systemImage: FluxSymbol.delete)
                }
                .disabled(busy)
                .alert(L("pluginUninstallTitle"), isPresented: $confirmUninstall) {
                    Button(L("pluginUninstallTooltip"), role: .destructive) { Task { await model.uninstall(plugin) } }
                    Button(L("cancel"), role: .cancel) {}
                } message: {
                    Text(L("pluginUninstallMsg", ["name": plugin.name]))
                }
            }
        }
    }

    private func copyError(_ plugin: PluginDto) {
        ExtensionsClipboard.copy(plugin.loadError)
        container.toasts.show(text: L("pluginLoadErrorCopied"), tone: .success)
    }
}

/// 详情信息分区（已安装与市场条目共用）：标识 / 作者 / 主页 / 发布时间 / 最低版本 / 设置项 / 描述 / 权限 / 须知。
struct PluginInfoSections: View {
    let detail: PluginDetail

    var body: some View {
        if let key = PluginMarket.yankedLabelKey(detail.yanked) {
            Section {
                Banner(text: L(key), tone: .error, systemImage: "exclamationmark.octagon.fill", slim: true)
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }
        }
        if !detail.tags.isEmpty {
            Section {
                FlowLayout(spacing: 6) {
                    ForEach(detail.tags, id: \.self) { StatusBadge(text: $0, tone: .neutral) }
                }
            }
        }
        Section {
            LabeledContent(L("pluginDetailIdentity")) { Text(detail.identity).textSelection(.enabled) }
            if !detail.author.isEmpty {
                LabeledContent(L("pluginDetailAuthor")) { Text(detail.author) }
            }
            if !detail.homepage.isEmpty {
                LabeledContent(L("pluginDetailHomepage")) { SafeLinkText(text: detail.homepage) }
            }
            if !detail.publishTime.isEmpty {
                LabeledContent(L("pluginDetailPublishTime")) { Text(detail.publishTime) }
            }
            if !detail.minAppVersion.isEmpty {
                LabeledContent(L("pluginDetailMinAppVersion")) { Text(detail.minAppVersion) }
            }
            if detail.settingsCount > 0 {
                LabeledContent(L("pluginDetailSettings")) {
                    Text(L("pluginDetailSettingsCount", ["count": detail.settingsCount]))
                        .multilineTextAlignment(.trailing)
                }
            }
        }
        if !detail.description.isEmpty {
            Section(L("pluginDetailDescription")) {
                Text(detail.description).textSelection(.enabled)
            }
        }
        if !detail.permissions.isEmpty {
            Section(L("pluginDetailPermissions")) {
                ForEach(detail.permissions, id: \.self) { PermissionRow(permission: $0) }
            }
        }
        Section(L("pluginDetailUsage")) {
            Text(L("pluginDetailUsageBody"))
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }
}

/// 加载失败详情（`pluginLoadErrorTitle` / `pluginLoadErrorBody` + 复制）。
struct PluginLoadErrorSheet: View {
    let plugin: PluginDto

    @Environment(AppContainer.self) private var container
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Text(plugin.loadError.isEmpty ? "—" : plugin.loadError)
                        .font(.fluxMono)
                        .textSelection(.enabled)
                } header: {
                    Text(L("pluginLoadErrorBody")).textCase(nil)
                }
            }
            .navigationTitle(L("pluginLoadErrorTitle"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(L("close")) { dismiss() } }
                ToolbarItem(placement: .primaryAction) {
                    Button(L("pluginLoadErrorCopy"), systemImage: FluxSymbol.copy) {
                        ExtensionsClipboard.copy(plugin.loadError)
                        container.toasts.show(text: L("pluginLoadErrorCopied"), tone: .success)
                    }
                    .disabled(plugin.loadError.isEmpty)
                }
            }
        }
        .presentationDetents([.medium, .large])
    }
}
#endif
