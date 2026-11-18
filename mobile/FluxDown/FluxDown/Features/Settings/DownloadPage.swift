import FluxDomain
import FluxUI
import SwiftUI

/// 全局 User-Agent 预设（与 PC 端同一份取值）。名称是品牌专有名词，本地化只用于「默认 / 自定义」。
private nonisolated struct UAPreset: Identifiable {
    let id: String
    let labelKey: String
    let ua: String
}

private let uaPresets: [UAPreset] = [
    UAPreset(
        id: "chrome", labelKey: "userAgentPresetChrome",
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
    ),
    UAPreset(
        id: "firefox", labelKey: "userAgentPresetFirefox",
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:147.0) Gecko/20100101 Firefox/147.0"
    ),
    UAPreset(
        id: "edge", labelKey: "userAgentPresetEdge",
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.3800.70"
    ),
    UAPreset(
        id: "safari", labelKey: "userAgentPresetSafari",
        ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15"
    ),
]

private let uaDefault = "default"
private let uaCustom = "custom"

/// S5 · 下载设置。字段绑定主机 `config`（仅展示 config 中存在的键）；
/// 写入：本地乐观 → 250ms 防抖 → `patchConfig`；Conflict 重放一次；失败回滚 + 行内说明 + toast。
/// 断线（只读）时各分组整体置灰，顶部出现只读横幅。
struct DownloadPage: View {
    @Environment(AppContainer.self) private var container
    @Environment(HostStore.self) private var store
    @Environment(ConfigEditor.self) private var editor

    @State private var showCdnConfirm = false
    @State private var showNicHelp = false
    /// 用户选了「自定义」UA 但文本尚未写入：保持自定义文本框可见。
    @State private var uaForceCustom = false
    /// 用户选了保留期「自定义」但值尚未写入（或恰等于预设）：保持天数输入行可见。
    @State private var retentionForceCustom = false
    @State private var categoryTarget: GeneralCategoryTarget?

    private var context: SettingsDownloadContext {
        SettingsDownloadContext(form: editor.form, isLocalHost: container.isLocalHost)
    }

    var body: some View {
        let ctx = context
        let readOnly = store.state.isReadOnly
        SettingsPage(title: L("settingsCatDownload"), showsReadOnlyBanner: true, showsSyncLegend: true) {
            if !SettingsDownloadRow.visible(in: .saveLocation, ctx).isEmpty {
                Section(L(SettingsDownloadSection.saveLocation.titleKey)) { saveLocationRows(ctx) }
                    .disabled(readOnly)
            }
            if !SettingsDownloadRow.visible(in: .behavior, ctx).isEmpty {
                Section(L(SettingsDownloadSection.behavior.titleKey)) { behaviorRows(ctx) }
                    .disabled(readOnly)
            }
            if !SettingsDownloadRow.visible(in: .connection, ctx).isEmpty {
                Section(L(SettingsDownloadSection.connection.titleKey)) { connectionRows(ctx) }
                    .disabled(readOnly)
            }
            if !SettingsDownloadRow.visible(in: .retry, ctx).isEmpty {
                Section(L(SettingsDownloadSection.retry.titleKey)) { retryRows(ctx) }
                    .disabled(readOnly)
            }
            if !SettingsDownloadRow.visible(in: .advanced, ctx).isEmpty {
                Section(L(SettingsDownloadSection.advanced.titleKey)) { advancedRows(ctx) }
                    .disabled(readOnly)
            }
            CategoriesSection(editing: $categoryTarget, readOnly: readOnly)
        }
        .fluxAnimation(.smooth, value: ctx.form)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                EditButton().disabled(readOnly)
            }
        }
        .sheet(item: $categoryTarget) { target in
            let categories = GeneralCategoryStore(editor: editor, toasts: container.toasts)
            CategoryEditorSheet(
                existing: target.existing,
                onSave: { categories.save($0) },
                onDelete: target.existing.map { entry in { categories.delete(entry) } }
            )
        }
        .alert(L("cdnMultiProxyConfirmTitle"), isPresented: $showCdnConfirm) {
            Button(L("cancel"), role: .cancel) {}
            Button(L("cdnMultiProxyConfirmDisable")) {
                editor.setNow(["cdn_multi_enabled": "true", "proxy_mode": "none"])
            }
        } message: {
            Text(editor.form.value("proxy_mode") == "system"
                ? L("cdnMultiProxyConfirmDescSystem")
                : L("cdnMultiProxyConfirmDescManual"))
        }
    }

    // MARK: 保存位置

    @ViewBuilder
    private func saveLocationRows(_ ctx: SettingsDownloadContext) -> some View {
        if SettingsDownloadRow.saveDir.isVisible(in: ctx) {
            if container.isLocalHost {
                LocalSaveDirRow()
            } else {
                // 远端主机：路径在服务器上——可手填，也可用 `daemon.fs.list` 浏览服务器目录。
                RemoteSaveDirRow()
            }
        }
        if SettingsDownloadRow.rememberLastSaveDir.isVisible(in: ctx) {
            ConfigToggleRow(row: .rememberLastSaveDir)
        }
    }

    // MARK: 行为

    @ViewBuilder
    private func behaviorRows(_ ctx: SettingsDownloadContext) -> some View {
        if SettingsDownloadRow.silentDownload.isVisible(in: ctx) { ConfigToggleRow(row: .silentDownload) }
        if SettingsDownloadRow.silentAutoCategory.isVisible(in: ctx) {
            ConfigToggleRow(row: .silentAutoCategory)
                .transition(.opacity.combined(with: .move(edge: .top)))
        }
        if SettingsDownloadRow.silentSkipSelection.isVisible(in: ctx) {
            ConfigToggleRow(row: .silentSkipSelection)
                .transition(.opacity.combined(with: .move(edge: .top)))
        }
        if SettingsDownloadRow.useServerTime.isVisible(in: ctx) { ConfigToggleRow(row: .useServerTime) }
        if SettingsDownloadRow.fileExistsBehavior.isVisible(in: ctx) {
            ConfigPickerRow(
                row: .fileExistsBehavior,
                options: [
                    .init(id: "rename", label: L("fileExistsRename")),
                    .init(id: "overwrite", label: L("fileExistsOverwrite")),
                    .init(id: "skip", label: L("fileExistsSkip")),
                    .init(id: "ask", label: L("fileExistsAsk")),
                ],
                fallback: "rename"
            )
        }
        if SettingsDownloadRow.fileMissingAction.isVisible(in: ctx) {
            ConfigPickerRow(
                row: .fileMissingAction,
                options: [
                    .init(id: "keep", label: L("fileMissingKeep")),
                    .init(id: "delete", label: L("fileMissingDelete")),
                ],
                fallback: "keep"
            )
        }
        if SettingsDownloadRow.completedRecordRetention.isVisible(in: ctx) { retentionRows }
        if SettingsDownloadRow.idleFileScan.isVisible(in: ctx) { ConfigToggleRow(row: .idleFileScan) }
        if SettingsDownloadRow.defaultQueue.isVisible(in: ctx) {
            ConfigPickerRow(
                row: .defaultQueue,
                options: [.init(id: "", label: L("defaultQueue"))]
                    + store.state.queues.map { .init(id: $0.queueId, label: Self.queueLabel($0)) },
                fallback: ""
            )
        }
        if SettingsDownloadRow.dedupSameUrl.isVisible(in: ctx) { ConfigToggleRow(row: .dedupSameUrl) }
    }

    /// 队列显示名：内置队列本地化，自建队列取其名称。
    private static func queueLabel(_ queue: TaskQueue) -> String {
        switch queue.queueId {
        case "", TaskQueue.main: L("mainQueue")
        case TaskQueue.later: L("downloadLater")
        default: queue.name
        }
    }

    /// 已完成记录保留天数：预设选择 + 「自定义」时的天数输入行（镜像 GPUI `sections/retention.rs`）。
    @ViewBuilder
    private var retentionRows: some View {
        let row = SettingsDownloadRow.completedRecordRetention
        let days = editor.form.int(row.configKey, default: 0)
        let effective = SettingsRetention.modeID(days: days, customFlag: retentionForceCustom)
        let options: [ConfigPickerRow.Option] = SettingsRetention.presets.map { preset in
            let key = switch preset {
            case 0: "retentionNever"
            case 1: "retentionDay1"
            case 7: "retentionDay7"
            default: "retentionDay30"
            }
            return .init(id: String(preset), label: L(key))
        } + [.init(id: SettingsRetention.customID, label: L("retentionCustom"))]
        SettingsTrailingLayout(title: L(row.titleKey), detail: L(row.detailKey), synced: row.item.isSynced) {
            Picker(L(row.titleKey), selection: Binding(
                get: { effective },
                set: { pickRetention($0, key: row.configKey, current: days) }
            )) {
                ForEach(options) { Text($0.label).tag($0.id) }
            }
            .pickerStyle(.menu)
            .labelsHidden()
        }
        .settingsRow(row.id, failureKey: row.configKey)
        if effective == SettingsRetention.customID {
            ConfigNumberRow(
                item: SettingsItem(
                    id: "download.completedRecordRetention.custom", key: row.configKey,
                    titleKey: "retentionCustom", detailKey: nil
                ),
                range: 1 ... 3650, fallback: SettingsRetention.customStart, unitHint: L("retentionDaysUnit")
            )
            .transition(.opacity.combined(with: .move(edge: .top)))
        }
    }

    private func pickRetention(_ id: String, key: String, current: Int) {
        if id == SettingsRetention.customID {
            retentionForceCustom = true
            let target = SettingsRetention.customTarget(current: current, max: 3650)
            if target != current { editor.set(key, String(target)) }
        } else {
            retentionForceCustom = false
            editor.set(key, id)
        }
    }

    // MARK: 连接与性能

    @ViewBuilder
    private func connectionRows(_ ctx: SettingsDownloadContext) -> some View {
        if SettingsDownloadRow.defaultSegments.isVisible(in: ctx) {
            ConfigNumberRow(row: .defaultSegments, range: 0 ... 64, fallback: 0, specials: [0: L("auto")])
        }
        if SettingsDownloadRow.autoMaxConnections.isVisible(in: ctx) {
            ConfigNumberRow(row: .autoMaxConnections, range: 0 ... 128, fallback: 16)
                .transition(.opacity.combined(with: .move(edge: .top)))
        }
        if SettingsDownloadRow.cdnMulti.isVisible(in: ctx) { cdnMultiRow }
        if SettingsDownloadRow.cdnMaxNodes.isVisible(in: ctx) {
            ConfigNumberRow(row: .cdnMaxNodes, range: 0 ... 8, fallback: 0, specials: [0: L("auto")])
                .transition(.opacity.combined(with: .move(edge: .top)))
        }
        if SettingsDownloadRow.multiNic.isVisible(in: ctx) {
            ConfigToggleRow(row: .multiNic)
            Button {
                showNicHelp = true
            } label: {
                Label(L("multiNicHelpHint"), systemImage: "questionmark.circle")
            }
            .popover(isPresented: $showNicHelp) { NicHelpPopover().presentationCompactAdaptation(.popover) }
        }
        if ctx.form.isLoaded { ConnPolicyRow() }
        if SettingsDownloadRow.maxConcurrent.isVisible(in: ctx) {
            ConfigNumberRow(row: .maxConcurrent, range: 1 ... 1024, fallback: 5)
        }
        if SettingsDownloadRow.speedLimit.isVisible(in: ctx) { SettingsRateLimitRow(row: .speedLimit) }
        if SettingsDownloadRow.uploadLimit.isVisible(in: ctx) { SettingsRateLimitRow(row: .uploadLimit) }
    }

    /// 多 CDN：开启时若代理启用，先确认「关闭代理并开启」（代理下该功能不生效）。
    private var cdnMultiRow: some View {
        let row = SettingsDownloadRow.cdnMulti
        return Toggle(isOn: Binding(
            get: { editor.form.bool(row.configKey) },
            set: { on in
                let proxy = editor.form.value("proxy_mode")
                if on, proxy == "system" || proxy == "manual" {
                    showCdnConfirm = true
                } else {
                    editor.set(row.configKey, SettingsConfigForm.wire(on))
                }
            }
        )) {
            SettingsText(title: L(row.titleKey), detail: L(row.detailKey), synced: row.item.isSynced)
        }
        .tint(Color.fdToggleOn)
        .settingsRow(row.id, failureKey: row.configKey)
    }

    // MARK: 失败自动重试

    @ViewBuilder
    private func retryRows(_ ctx: SettingsDownloadContext) -> some View {
        if SettingsDownloadRow.autoRetryCount.isVisible(in: ctx) {
            ConfigNumberRow(
                row: .autoRetryCount, range: -1 ... 20, fallback: 3,
                specials: [0: L("autoRetryOff"), -1: L("autoRetryUnlimited")]
            )
        }
        if SettingsDownloadRow.autoRetryDelay.isVisible(in: ctx) {
            ConfigNumberRow(row: .autoRetryDelay, range: 0 ... 86_400, fallback: 5, unitHint: L("autoRetryDelayUnit"))
        }
        if SettingsDownloadRow.autoResumeOnStart.isVisible(in: ctx) { ConfigToggleRow(row: .autoResumeOnStart) }
    }

    // MARK: 高级

    @ViewBuilder
    private func advancedRows(_ ctx: SettingsDownloadContext) -> some View {
        if SettingsDownloadRow.userAgent.isVisible(in: ctx) {
            let key = SettingsDownloadRow.userAgent.configKey
            let current = editor.form.value(key) ?? ""
            let matched = uaPresets.first { $0.ua == current }?.id ?? (current.isEmpty ? uaDefault : uaCustom)
            let effective = uaForceCustom ? uaCustom : matched
            let options: [ConfigPickerRow.Option] = [.init(id: uaDefault, label: L("userAgentPresetDefault"))]
                + uaPresets.map { .init(id: $0.id, label: L($0.labelKey)) }
                + [.init(id: uaCustom, label: L("userAgentPresetCustom"))]
            SettingsTrailingLayout(title: L("userAgent"), detail: L("userAgentDesc"), synced: true) {
                Picker(L("userAgent"), selection: Binding(get: { effective }, set: { pickUA($0, key: key) })) {
                    ForEach(options) { Text($0.label).tag($0.id) }
                }
                .pickerStyle(.menu)
                .labelsHidden()
            }
            .settingsRow(SettingsDownloadRow.userAgent.id, failureKey: key)
            if effective == uaCustom {
                ConfigTextRow(
                    id: "download.userAgent.custom",
                    key: key,
                    title: L("userAgentPresetCustom"),
                    prompt: L("userAgentPlaceholder")
                )
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
    }

    private func pickUA(_ id: String, key: String) {
        switch id {
        case uaDefault:
            uaForceCustom = false
            editor.set(key, "")
        case uaCustom:
            uaForceCustom = true
        default:
            uaForceCustom = false
            if let preset = uaPresets.first(where: { $0.id == id }) { editor.set(key, preset.ua) }
        }
    }
}

/// 本机保存目录：iOS 沙盒里固定为 `Documents/`（「文件」App › 我的 iPhone › FluxDown），不可更改；
/// 提供「在『文件』中显示」。
private struct LocalSaveDirRow: View {
    @Environment(AppContainer.self) private var container
    @Environment(\.openURL) private var openURL

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SettingsText(title: L("defaultSaveDir"), detail: L("defaultSaveDirDesc"))
            Text(L("mobileSaveDirIosFixed"))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button {
                showInFiles()
            } label: {
                Label(L("mobileShowInFiles"), systemImage: FluxSymbol.folder)
            }
            .buttonStyle(.borderless)
        }
        .settingsRow(SettingsDownloadRow.saveDir.id)
    }

    private func showInFiles() {
        guard let url = LocalPaths.filesAppURL(forDirectory: LocalPaths.documents.path) else {
            container.toasts.show(text: L("mobileOpenFileFailed"), tone: .error)
            return
        }
        openURL(url) { accepted in
            if !accepted { container.toasts.show(text: L("mobileOpenFileFailed"), tone: .error) }
        }
    }
}

/// 多网卡聚合说明（沿用 PC 文案），popover（compact 下也保持 popover）。
private struct NicHelpPopover: View {
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                Text(L("multiNicHelpTitle")).font(.headline)
                Text(L("multiNicHelp"))
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding()
        }
        .frame(idealWidth: 340, idealHeight: 420)
    }
}

// MARK: - 远端保存目录

/// 远端主机默认保存目录：文本框（手填，校验为绝对路径）+「浏览…」打开服务器目录选择器（`daemon.fs.list`）。
private struct RemoteSaveDirRow: View {
    @Environment(ConfigEditor.self) private var editor
    @State private var showPicker = false

    var body: some View {
        let row = SettingsDownloadRow.saveDir
        VStack(alignment: .leading, spacing: 4) {
            ConfigTextRow(
                id: row.id,
                key: row.configKey,
                title: L("defaultSaveDir"),
                detail: L("defaultSaveDirDesc"),
                prompt: L("mobileSaveDirUnset"),
                validate: { SettingsSaveDirectory.isValid($0) ? nil : L("mobileSaveDirInvalid") }
            )
            Button {
                showPicker = true
            } label: {
                Label(L("mobileBrowseServerFolders"), systemImage: "folder.badge.gearshape")
            }
            .buttonStyle(.borderless)
            .frame(minHeight: 44, alignment: .leading)
        }
        .sheet(isPresented: $showPicker) {
            RemoteDirectoryPicker(startPath: editor.form.value(row.configKey) ?? "") { picked in
                editor.set(row.configKey, picked)
            }
        }
    }
}

// MARK: - 已学习的服务器策略

/// `connPolicyCache`：引擎学到的按域连接上限条数（`daemon.config.connPolicy`）+ 清除（`clearConnPolicy`）。
private struct ConnPolicyRow: View {
    @Environment(AppContainer.self) private var container
    @Environment(HostStore.self) private var store

    @State private var count: UInt64?
    @State private var isClearing = false
    @State private var failed = false

    var body: some View {
        Group {
            LabeledContent {
                if let count {
                    Text(count == 0 ? L("connPolicyCacheEmpty") : String(count))
                        .monospacedDigit()
                } else if failed {
                    Text(verbatim: "—").foregroundStyle(.secondary)
                } else {
                    ProgressView()
                }
            } label: {
                SettingsText(title: L("connPolicyCache"))
            }
            .settingsRow("download.connPolicy")
            if let count, count > 0 {
                SettingsActionRow(
                    title: L("connPolicyCacheClear"), systemImage: FluxSymbol.delete, role: .destructive, isRunning: isClearing,
                    action: clear
                )
            }
        }
        .task(id: container.host.id) { await load() }
        .onChange(of: store.state.connection == .live) { _, live in
            if live { Task { await load() } }
        }
    }

    private func load() async {
        do throws(HostError) {
            let summary: ConnPolicySummaryDto = try await container.session.call(HostMethod.daemonConfigConnPolicy)
            count = summary.domainCount
            failed = false
        } catch {
            failed = true
        }
    }

    private func clear() {
        isClearing = true
        Task {
            defer { isClearing = false }
            do throws(HostError) {
                let summary: ConnPolicySummaryDto = try await container.session.call(HostMethod.daemonConfigClearConnPolicy)
                count = summary.domainCount
                FluxHaptic.success.play()
            } catch {
                container.toasts.show(text: ErrorText.describe(error), tone: .error)
            }
        }
    }
}

// MARK: - 搜索索引

extension DownloadPage {
    static func searchEntries(_ ctx: SettingsSearchContext) -> [SettingsEntry] {
        let name = L("settingsCatDownload")
        let rows = SettingsDownloadContext(form: ctx.form, isLocalHost: ctx.isLocalHost)
        let visible = SettingsDownloadRow.allCases.filter { $0.isVisible(in: rows) }
        func entry(_ row: SettingsDownloadRow) -> SettingsEntry {
            SettingsEntry(
                item: row.item, route: .download, breadcrumb: "\(name) › \(L(row.group.titleKey))",
                symbol: "arrow.down.circle.fill"
            )
        }
        // 「自定义分类」位于下载页最底部。
        let categories = SettingsEntry(
            id: CategoriesRow.id, route: .download, title: L(CategoriesRow.titleKey), detail: L(CategoriesRow.detailKey),
            breadcrumb: "\(name) › \(L(CategoriesRow.titleKey))", symbol: "folder.fill"
        )
        return visible.map(entry) + [categories]
    }
}
