import FluxDomain
import FluxUI
import SwiftUI
import UniformTypeIdentifiers

/// N1 · 新建下载（`02-downloads.md` §11）+ N2 高级选项（同一 `NavigationStack` 内推入）。
///
/// - 系统 `Form` 承载内容（不上玻璃）；玻璃只在底部浮层操作栏（§2.1 / §2.4）。
/// - 粘贴用系统 `PasteButton`（不主动读剪贴板，避免「允许粘贴」弹窗）。
/// - 表单每次打开新建一份，默认值来自主机配置。
struct NewDownloadSheet: View {
    let prefill: String

    @Environment(AppContainer.self) private var container
    @Environment(\.dismiss) private var dismiss
    @State private var form: NewDownloadForm?

    var body: some View {
        Group {
            if let form {
                NewDownloadContent(form: form, close: { dismiss() })
            } else {
                Color.clear
            }
        }
        .task {
            if form == nil { form = NewDownloadForm(prefill: prefill, state: container.store.state) }
        }
        // 关闭 Sheet 即取消进行中的清单预解析（在途 RPC 的晚到结果被忽略）。
        .onDisappear { form?.cancelProbe() }
    }
}

private struct NewDownloadContent: View {
    @Bindable var form: NewDownloadForm
    let close: () -> Void

    @Environment(AppContainer.self) private var container
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var showDiscard = false
    @State private var showImporter = false
    @State private var importerKind: ImporterKind = .text
    #if FULL_FLAVOR
    @State private var importingTorrent = false
    #endif
    @State private var dropTargeted = false
    @State private var path: [NewDownloadRoute] = []
    /// 「下载到」候选的分区缓存（云端 presence）。
    @State private var sections = AgentSections()
    /// 下发进行中的进度（已完成 / 总数）；nil = 未在下发。
    @State private var dispatchProgress: (done: Int, total: Int)?
    /// 下发失败的内联说明（失败的链接留在文本框里供重试）。
    @State private var dispatchFailure: String?
    @FocusState private var linksFocused: Bool

    private static let probeId = "manifestProbe"
    /// 「下载到」里当前主机的选择键（远端目标为 `cloud:` / `link:` 前缀，不会冲突）。
    private static let hostTargetTag = "host"

    private var state: HostState { container.store.state }

    /// 「下载到」候选：云账号其他设备 + 局域网已配对设备（快照实时投影）。
    private var dispatchTargets: [DispatchTarget] {
        let state = state
        let live = state.connection == .live
        return DeviceRules.dispatchTargets(
            cloud: state.cloudDevices,
            link: state.linkDevices,
            cloudPresenceKnown: CloudPresence.isKnown(sections.connection(state), localReady: live),
            localReady: live
        )
    }

    var body: some View {
        let entries = form.entries
        let targets = dispatchTargets
        let target = form.target(in: targets)
        NavigationStack(path: $path) {
            ScrollViewReader { proxy in
                Form {
                    #if FULL_FLAVOR
                    if let probe = form.probe { probeSection(probe) }
                    #endif
                    linksSection(entries, remote: target != nil)
                    if !entries.isEmpty { previewSection(entries) }
                    if !targets.isEmpty { targetSection(targets, selected: target) }
                    if let target {
                        remoteDestinationSection(entries, target: target)
                        dispatchStatusSection
                    } else {
                        destinationSection(entries)
                        optionsSection(entries)
                        advancedSection(entries)
                    }
                }
                .fluxAnimation(.smooth, value: form.probe != nil)
                .scrollDismissesKeyboard(.interactively)
                .onChange(of: form.probe?.id) { _, id in
                    guard id != nil else { return }
                    withFluxAnimation(.smooth, reduceMotion: reduceMotion) {
                        proxy.scrollTo(Self.probeId, anchor: .top)
                    }
                }
            }
            .navigationTitle(L("newDownload"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L("cancel"), systemImage: FluxSymbol.close, action: requestClose)
                        .disabled(form.submitting)
                        .alert(L("mobileDiscardTitle"), isPresented: $showDiscard) {
                            Button(L("mobileDiscard"), role: .destructive, action: close)
                            Button(L("mobileKeepEditing"), role: .cancel) {}
                        } message: {
                            Text(L("mobileDiscardMessage"))
                        }
                }
                ToolbarItemGroup(placement: .keyboard) {
                    Spacer()
                    Button(L("confirm")) { linksFocused = false }
                }
            }
            .toolbar { bottomBar(entries, target: target) }
            .navigationDestination(for: NewDownloadRoute.self) { route in
                switch route {
                case .advanced:
                    AdvancedOptionsView(form: form)
                #if FULL_FLAVOR
                case let .manifest(source, base):
                    ManifestSelectView(source: source, base: base, close: close)
                #endif
                }
            }
        }
        .onDrop(of: [.fileURL], isTargeted: $dropTargeted) { providers in
            handleDrop(providers)
        }
        .overlay {
            if dropTargeted {
                RoundedRectangle(cornerRadius: 28, style: .continuous)
                    .strokeBorder(.tint, style: StrokeStyle(lineWidth: 3, dash: [10, 6]))
                    .padding(8)
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
            }
        }
        .fluxAnimation(.snappy, value: dropTargeted)
        // 预测输入（第一条链接的文件名 / 链接 / 重命名）变化，及打开表单时：重算分类目录并按「自动」规则跟随。
        .onChange(of: form.categoryPredictionKey, initial: true) {
            form.followCategory(session: container.session)
        }
        .interactiveDismissDisabled(form.isDirty || form.submitting)
        .fileImporter(
            isPresented: $showImporter,
            allowedContentTypes: importerContentTypes,
            allowsMultipleSelection: true
        ) { result in
            switch importerKind {
            case .text: importFiles(result)
            #if FULL_FLAVOR
            case .torrent: importTorrentFiles(result)
            #endif
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }

    #if FULL_FLAVOR
    // MARK: 清单探测（N5 · 解析中）

    private func probeSection(_ probe: ManifestProbe) -> some View {
        let layout = dynamicTypeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 12))
            : AnyLayout(HStackLayout(spacing: 12))
        return Section {
            layout {
                HStack(spacing: 12) {
                    ProgressView()
                    VStack(alignment: .leading, spacing: 2) {
                        Text(L("manifestResolvingLabel"))
                        if !probe.host.isEmpty {
                            Text(probe.host)
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Button(L("manifestResolvingCancel")) { form.cancelProbe() }
                    .buttonStyle(.borderless)
            }
            .id(Self.probeId)
        }
    }
    #endif

    // MARK: 链接

    private func linksSection(_ entries: [UrlEntry], remote: Bool) -> some View {
        let hasText = !form.urlText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let error: String? =
            if hasText, entries.isEmpty { L(BuildFlavor.key(full: "newDownloadNoValidUrl", store: "newDownloadNoValidUrlGeneric")) }
            else if !hasText, form.showEmptyError { L("mobileEnterUrl") }
            else { nil }
        return Section {
            TextEditor(text: $form.urlText)
                .font(.callout.monospaced())
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
                .focused($linksFocused)
                .disabled(form.submitting || form.probe != nil)
                .frame(minHeight: 110)
                .overlay(alignment: .topLeading) {
                    if form.urlText.isEmpty {
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
                .onChange(of: form.urlText) { form.showEmptyError = false }
            if let error {
                Label(error, systemImage: FluxSymbol.failure)
                    .font(.footnote)
                    .foregroundStyle(Color.fdStatusFailedText)
            }
            pasteButton
            #if FULL_FLAVOR
            torrentButton(remote: remote)
            #endif
            importButton
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
            Text(L(BuildFlavor.key(full: "batchDownloadDesc", store: "batchDownloadDescGeneric")))
        }
    }

    private var pasteButton: some View {
        PasteButton(payloadType: String.self) { strings in
            let text = strings.joined(separator: "\n")
            Task { @MainActor in
                if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    FluxHaptic.error.play()
                    container.toasts.show(text: L("mobileClipboardEmpty"), tone: .warning)
                } else {
                    form.appendText(text)
                    form.showEmptyError = false
                    FluxHaptic.success.play()
                    container.toasts.show(text: L("mobilePasted"), tone: .info, systemImage: "list.clipboard")
                }
            }
        }
        .labelStyle(.titleAndIcon)
        .buttonBorderShape(.capsule)
        .disabled(form.submitting)
    }

    #if FULL_FLAVOR
    /// 种子文件只能在当前主机建任务，「下载到」选了其他设备时禁用（同 GPUI）。
    private func torrentButton(remote: Bool) -> some View {
        Button {
            importerKind = .torrent
            showImporter = true
        } label: {
            if importingTorrent {
                Label { Text(L("openTorrentFile")) } icon: { ProgressView() }
            } else {
                Label(L("openTorrentFile"), systemImage: "document.badge.plus")
            }
        }
        .disabled(form.submitting || importingTorrent || form.probe != nil || remote)
        .accessibilityHint(importingTorrent ? L("btProbing") : "")
    }
    #endif

    private var importButton: some View {
        Button(L("importTxtFile"), systemImage: "text.document") {
            importerKind = .text
            showImporter = true
        }
        .disabled(form.submitting)
    }

    private var importerContentTypes: [UTType] {
        #if FULL_FLAVOR
        if importerKind == .torrent { return [TorrentImport.contentType] }
        #endif
        return [.plainText, .text, .data]
    }

    // MARK: 预览

    private func previewSection(_ entries: [UrlEntry]) -> some View {
        let duplicate = duplicateName(entries)
        return Section {
            ForEach(Array(entries.prefix(3)), id: \.url) { entry in
                PreviewRow(entry: entry)
            }
            if entries.count > 3 {
                Text(L("mobileNewDownloadPreviewMore", ["n": entries.count - 3]))
                    .font(.footnote.monospaced())
                    .foregroundStyle(.secondary)
            }
            if let duplicate {
                Banner(text: L("mobileNewDownloadDuplicateUrl", ["name": duplicate]), tone: .warning, slim: true)
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }
        }
    }

    private func duplicateName(_ entries: [UrlEntry]) -> String? {
        guard let first = entries.first?.url else { return nil }
        return state.tasks.first { $0.url == first || $0.originUrl == first }?.fileName
    }

    // MARK: 下载到

    private func targetSection(_ targets: [DispatchTarget], selected: DispatchTarget?) -> some View {
        Section {
            Picker(L("downloadTo"), selection: targetChoice(selected)) {
                Text(hostTargetLabel).tag(Self.hostTargetTag)
                ForEach(targets) { target in
                    Text(targetLabel(target)).tag(target.id)
                }
            }
            .pickerStyle(.menu)
            .disabled(form.submitting || form.probe != nil)
        } footer: {
            if let selected {
                VStack(alignment: .leading, spacing: 4) {
                    if let presence = presenceHint(selected) { Text(presence) }
                    Text(L("downloadToRemoteOptionsIgnored"))
                }
            } else {
                Text(L("downloadToHint"))
            }
        }
    }

    /// 当前主机：本机显示「本机」，远端主机显示主机名。
    private var hostTargetLabel: String {
        if container.isLocalHost { return L("thisDevice") }
        let name = container.host.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        return name.isEmpty ? L("webDownloadToServer") : name
    }

    /// 同 GPUI `target_label`：云设备 `名称 · 状态`，已配对设备 `名称 · 局域网 · 状态`。
    private func targetLabel(_ target: DispatchTarget) -> String {
        let status = switch target.online {
        case true?: L("deviceOnline")
        case false?: L("deviceOffline")
        case nil: L("devicePresenceUnknown")
        }
        return target.isCloud
            ? "\(target.name) · \(status)"
            : "\(target.name) · \(L("deviceLocalTag")) · \(status)"
    }

    /// 目标非在线时的提示：云设备离线由云端排队，局域网设备离线送不到，状态未知如实说明。
    private func presenceHint(_ target: DispatchTarget) -> String? {
        switch target.online {
        case true?: nil
        case false?: L(target.isCloud ? "downloadToOfflineHint" : "errReasonPeerOffline")
        case nil: L("devicePresenceUnknown")
        }
    }

    /// 所选目标消失（登出 / 解除配对）时读作当前主机。
    private func targetChoice(_ selected: DispatchTarget?) -> Binding<String> {
        Binding(
            get: { selected?.id ?? Self.hostTargetTag },
            set: { value in
                form.targetId = value == Self.hostTargetTag ? nil : value
                dispatchFailure = nil
            }
        )
    }

    // MARK: 目录 / 文件名

    private func destinationSection(_ entries: [UrlEntry]) -> some View {
        let options = categoryOptions
        return Section {
            categoryPicker(options)
            if container.isLocalHost {
                LabeledContent(L("saveDir")) {
                    Text(form.saveDir.isEmpty ? LocalPaths.documents.path : form.saveDir)
                        .font(.footnote.monospaced())
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .multilineTextAlignment(.trailing)
                        .textSelection(.enabled)
                }
            } else {
                TextField(L("saveDir"), text: saveDirBinding)
                    .font(.callout.monospaced())
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .disabled(form.submitting)
                if !form.saveDirValid {
                    Label(L("mobileSaveDirInvalid"), systemImage: FluxSymbol.failure)
                        .font(.footnote)
                        .foregroundStyle(Color.fdStatusFailedText)
                }
            }
            renameField(entries)
        } footer: {
            if let hint = categoryHint { Text(hint) }
        }
    }

    // MARK: 分类

    /// 能产生分类路由的分类（可见、非「全部」、目录非空）；没有具体分类时仍显示「自动 / 不分类」。
    private var categoryOptions: [CustomCategoryDto] {
        SaveDirCategoryState.options(CustomCategoryDto.fromPreference(state.preferences[CustomCategoryDto.preferenceKey]))
    }

    /// 「自动」「不分类」，分隔后是各分类（分类图标同设置页）。「自动」命中时标题带上命中分类名。
    private func categoryPicker(_ options: [CustomCategoryDto]) -> some View {
        let matched = SaveDirCategoryState.matched(options, predicted: form.predictedCategoryDir)
        let autoTitle = matched.map { L("saveDirCategoryAutoMatched", ["name": GeneralCategoryText.displayName($0)]) }
            ?? L("saveDirCategoryAuto")
        return Picker(L("saveDirCategory"), selection: categoryBinding(options)) {
            Label(autoTitle, systemImage: "sparkles").tag(SaveDirCategoryMode.auto)
            Label(L("saveDirCategoryNone"), systemImage: FluxSymbol.folder).tag(SaveDirCategoryMode.noCategory)
            Section {
                ForEach(options) { entry in
                    Label(GeneralCategoryText.displayName(entry), systemImage: GeneralCategoryIcon.symbol(entry.icon))
                        .tag(SaveDirCategoryMode.category(entry.id))
                }
            }
        }
        .pickerStyle(.menu)
        .disabled(form.submitting)
    }

    /// 所选分类已从列表消失（被隐藏 / 删除 / 清空目录）时读作「自动」，避免选择器空白。
    private func categoryBinding(_ options: [CustomCategoryDto]) -> Binding<SaveDirCategoryMode> {
        Binding(
            get: {
                if case let .category(id) = form.categoryState.mode, !options.contains(where: { $0.id == id }) {
                    return .auto
                }
                return form.categoryState.mode
            },
            set: { form.selectCategory($0, options: options, session: container.session) }
        )
    }
    private var saveDirBinding: Binding<String> {
        Binding(
            get: { form.saveDir },
            set: {
                form.saveDir = $0
                form.markSaveDirManual()
            }
        )
    }

    /// 当前模式的一句话说明（固定到某分类时目录已在下方「保存目录」行显示，无需再说明）。
    private var categoryHint: String? {
        switch form.categoryState.mode {
        case .auto: L("saveDirCategoryAutoDesc")
        case .noCategory: L("saveDirCategoryNoneDesc")
        case .category: nil
        }
    }

    /// 远端目标的保存目录：目标设备上的路径（按其路径风格校验），留空 = 目标设备默认目录。
    private func remoteDestinationSection(_ entries: [UrlEntry], target: DispatchTarget) -> some View {
        let invalid = DeviceRules.checkSaveDir(form.remoteSaveDir, style: target.pathStyle) == .invalid
        let placeholder = target.defaultSaveDir.map { L("downloadToRemoteDirDefault", ["dir": $0]) }
            ?? L("downloadToRemoteDirUseDefault")
        return Section {
            TextField(L("saveDir"), text: $form.remoteSaveDir, prompt: Text(placeholder))
                .font(.callout.monospaced())
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
                .disabled(form.submitting)
            if invalid {
                Label(
                    L("downloadToPathInvalid", ["example": PathStyle.example(target.pathStyle)]),
                    systemImage: FluxSymbol.failure
                )
                .font(.footnote)
                .foregroundStyle(Color.fdStatusFailedText)
            }
            renameField(entries)
        } header: {
            Text(L("saveDir"))
        } footer: {
            Text(L("downloadToRemoteDirHint"))
        }
    }

    @ViewBuilder
    private func renameField(_ entries: [UrlEntry]) -> some View {
        if entries.count == 1 {
            TextField(L("renameTask"), text: $form.rename, prompt: Text(L("autoDetectFilename")))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .disabled(form.submitting)
        }
    }

    /// 下发进度 / 失败说明（同设备页下发表单）。
    @ViewBuilder
    private var dispatchStatusSection: some View {
        if let progress = dispatchProgress {
            Section {
                HStack(spacing: 12) {
                    ProgressView()
                    Text(L("mobileDispatchSending", ["done": progress.done, "total": progress.total]))
                        .monospacedDigit()
                }
                .accessibilityElement(children: .combine)
            }
        } else if let dispatchFailure {
            Section {
                Banner(text: dispatchFailure, tone: .error, systemImage: FluxSymbol.failure, slim: true)
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets())
            }
        }
    }

    // MARK: 线程 / 队列

    @ViewBuilder
    private func optionsSection(_ entries: [UrlEntry]) -> some View {
        let showThreads = NewDownloadForm.threadsApplicable(entries)
        let queues = state.queues
        if showThreads || !queues.isEmpty {
            Section {
                if showThreads {
                    Picker(L("threads"), selection: threadChoice) {
                        Text(L("auto")).tag(0)
                        ForEach(threadPresets, id: \.self) { n in
                            Text(L("nThreads", ["n": n])).tag(n)
                        }
                        Text(L("customThreads")).tag(-1)
                    }
                    .pickerStyle(.menu)
                    .disabled(form.submitting)
                    if form.threadMode == .custom {
                        Stepper(value: $form.customThreads, in: 1...maxThreads) {
                            Text(L("nThreads", ["n": form.customThreads])).monospacedDigit()
                        }
                        .accessibilityLabel(L("customThreads"))
                    }
                }
                if !queues.isEmpty {
                    Picker(L("taskQueueLabel"), selection: queueChoice(queues)) {
                        ForEach(queues) { q in
                            Text("\(newTaskQueueLabel(q)) · \(q.isRunning ? L("queueRunningBadge") : L("queueStoppedBadge"))")
                                .tag(q.queueId)
                        }
                    }
                    .pickerStyle(.menu)
                    .disabled(form.submitting)
                }
            } footer: {
                if showThreads, form.threadMode == .custom { Text(L("customThreadsHint")) }
            }
        }
    }

    private var threadChoice: Binding<Int> {
        Binding(
            get: {
                switch form.threadMode {
                case .auto: 0
                case .preset: form.presetThreads
                case .custom: -1
                }
            },
            set: { value in
                switch value {
                case 0: form.threadMode = .auto
                case -1: form.threadMode = .custom
                default:
                    form.threadMode = .preset
                    form.presetThreads = value
                }
            }
        )
    }

    /// 表单里的队列不在列表中（列表晚于表单到达 / 队列被删）→ 显示并提交第一个队列。
    private func queueChoice(_ queues: [TaskQueue]) -> Binding<String> {
        Binding(
            get: { currentQueueId(queues) },
            set: { form.queueId = $0 }
        )
    }

    private func currentQueueId(_ queues: [TaskQueue]) -> String {
        queues.contains { $0.queueId == form.queueId } ? form.queueId : (queues.first?.queueId ?? form.queueId)
    }

    // MARK: 高级入口

    private func advancedSection(_ entries: [UrlEntry]) -> some View {
        let single = entries.count == 1
        let singleHttp = single && NewDownloadForm.isHttpLike(entries[0].url)
        let modified = form.advanced.modified(single: single, singleHttp: singleHttp)
        let headerCount = form.advanced.headers.filter { !$0.key.trimmingCharacters(in: .whitespaces).isEmpty }.count
        let summary = modified.isEmpty
            ? L("mobileAdvancedNone")
            : modified.map { advancedLabel($0, headerCount: headerCount) }.joined(separator: " · ")
        return Section {
            NavigationLink(value: NewDownloadRoute.advanced) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(L("taskProxyAdvanced"))
                    Text(summary)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            .accessibilityElement(children: .combine)
        }
    }

    // MARK: 底部操作栏（系统 `.bottomBar`：只渲染一层 Liquid Glass）

    @ToolbarContentBuilder
    private func bottomBar(_ entries: [UrlEntry], target: DispatchTarget?) -> some ToolbarContent {
        let queues = state.queues
        let queueId = currentQueueId(queues)
        let startLabel = entries.count > 1
            ? L("startBatchDownload", ["count": entries.count])
            : L("startDownload")
        let canSubmit = !form.submitting && form.probe == nil && !entries.isEmpty
        if let target {
            // 下发只有「立即开始」：没有队列，也没有稍后下载（同 GPUI）。
            ToolbarSpacer(.flexible, placement: .bottomBar)
            ToolbarItem(placement: .bottomBar) {
                startButton(startLabel) { submitRemote(target) }
                    .disabled(!canSubmit)
            }
        } else {
            localBottomBar(queues: queues, queueId: queueId, startLabel: startLabel, canSubmit: canSubmit)
        }
    }

    @ToolbarContentBuilder
    private func localBottomBar(
        queues: [TaskQueue],
        queueId: String,
        startLabel: String,
        canSubmit: Bool
    ) -> some ToolbarContent {
        ToolbarItem(placement: .bottomBar) {
            Button { submit(startPaused: true, queueId: queueId) } label: {
                if dynamicTypeSize.isAccessibilitySize {
                    Label(L("downloadLater"), systemImage: "clock").labelStyle(.iconOnly)
                } else {
                    Label(L("downloadLater"), systemImage: "clock").labelStyle(.titleAndIcon)
                }
            }
            .disabled(!canSubmit)
        }
        ToolbarSpacer(.flexible, placement: .bottomBar)
        if queues.count > 1 {
            ToolbarItem(placement: .bottomBar) {
                Menu {
                    Section {
                        ForEach(queues) { q in
                            Button(L("mobileStartIntoQueue", ["name": newTaskQueueLabel(q)]), systemImage: "arrow.down") {
                                submit(startPaused: false, queueId: q.queueId)
                            }
                        }
                    }
                    Section {
                        ForEach(queues) { q in
                            Button(L("mobileLaterIntoQueue", ["name": newTaskQueueLabel(q)]), systemImage: "clock") {
                                submit(startPaused: true, queueId: q.queueId)
                            }
                        }
                    }
                } label: {
                    Image(systemName: FluxSymbol.more)
                }
                .disabled(!canSubmit)
                .accessibilityLabel(L("moreActions"))
            }
            ToolbarSpacer(.fixed, placement: .bottomBar)
        }
        ToolbarItem(placement: .bottomBar) {
            startButton(startLabel) { submit(startPaused: false, queueId: queueId) }
                .disabled(!canSubmit)
        }
    }

    private func startButton(_ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            if dynamicTypeSize.isAccessibilitySize {
                Label(label, systemImage: "arrow.down").labelStyle(.iconOnly)
            } else {
                Label(label, systemImage: "arrow.down").lineLimit(1)
            }
        }
        .buttonStyle(.borderedProminent)
        .accessibilityLabel(label)
    }

    // MARK: 动作

    private func requestClose() {
        guard !form.submitting else { return }
        if form.isDirty { showDiscard = true } else { close() }
    }

    private func reject() { FluxHaptic.error.play() }

    private func submit(startPaused: Bool, queueId: String) {
        guard !form.submitting, form.probe == nil else { return }
        let state = container.store.state
        if state.isReadOnly {
            reject()
            container.toasts.show(text: L("localServiceDisconnected"), tone: .error, systemImage: FluxSymbol.offline)
            return
        }
        if form.entries.isEmpty {
            reject()
            form.showEmptyError = true
            return
        }
        #if !FULL_FLAVOR
        // 商店构建不含 P2P：任何主机（本机 / 远端 / 下发到其它设备）都不提交 magnet / ed2k。
        if form.entries.contains(where: { P2PLink.isP2P($0.url) }) {
            reject()
            container.toasts.show(text: L("webChangeUrlErrProtocolUnsupported"), tone: .warning)
            return
        }
        #endif
        if !container.isLocalHost, !form.saveDirValid {
            reject()
            return
        }
        if form.entries.count == 1, !checksumHexValid(form.advanced.checksumHex) {
            reject()
            return
        }
        let manualProxy = manualProxyUrl(state.config)
        let requests = form.buildRequests(startPaused: startPaused, queue: queueId, manualProxy: manualProxy)
        #if FULL_FLAVOR
        // N5：单条 http(s) 链接先探测插件多文件清单；未命中 / 失败 / 超时都静默回退为普通建任务。
        // store 构建不含插件，不探测。
        if requests.count == 1, let request = requests.first, shouldProbeManifest(request.url) {
            probeManifest(request, requests: requests, startPaused: startPaused, queueId: queueId, manualProxy: manualProxy)
            return
        }
        #endif
        createTasks(requests, startPaused: startPaused)
    }

    #if FULL_FLAVOR
    private func shouldProbeManifest(_ url: String) -> Bool {
        ManifestSelection.isPreviewable(url) && !TorrentFile.isTorrentFileName(URL(string: url)?.path ?? "")
    }

    /// 行内进度（非阻塞）：探测期间表单保持可见，提交键禁用；关闭 Sheet 取消。
    private func probeManifest(
        _ request: CreateTaskRequest,
        requests: [CreateTaskRequest],
        startPaused: Bool,
        queueId: String,
        manualProxy: String
    ) {
        let url = request.url
        let base = form.manifestBase(queue: queueId, manualProxy: manualProxy)
        let probe = ManifestProbe(
            session: container.session,
            method: HostMethod.daemonGroupResolvePreview,
            params: base.previewRequest(url: url),
            host: URL(string: url)?.host ?? ""
        )
        form.probe = probe
        linksFocused = false
        AccessibilityNotification.Announcement(L("manifestResolvingLabel")).post()
        Task {
            let outcome = await probe.outcome()
            guard form.probe?.id == probe.id else { return }
            form.probe = nil
            if case let .manifest(preview) = outcome {
                FluxHaptic.soft.play()
                path.append(.manifest(.daemon(preview: preview, sourceUrl: url), base))
            } else {
                createTasks(requests, startPaused: startPaused)
            }
        }
    }
    #endif

    private func createTasks(_ requests: [CreateTaskRequest], startPaused: Bool) {
        form.submitting = true
        let session = container.session
        let toasts = container.toasts
        Task {
            var done = Set<String>()
            var failure: HostError?
            for request in requests {
                do throws(HostError) {
                    _ = try await session.createTask(request)
                    done.insert(request.url)
                } catch {
                    failure = error
                }
            }
            form.submitting = false
            if let failure {
                reject()
                form.removeUrls(done)
                toasts.show(text: ErrorText.describe(failure), tone: .error)
            } else {
                FluxHaptic.success.play()
                let text =
                    if done.count > 1 { L("mobileDownloadStartedN", ["n": done.count]) }
                    else if startPaused { L("taskCreatedToast") }
                    else { L("mobileDownloadStarted") }
                toasts.show(text: text, tone: .success, systemImage: startPaused ? "clock" : "arrow.down")
                close()
            }
        }
    }

    /// 「下载到」其他设备：逐条下发链接 / 文件名 / 保存目录；不探测插件清单（由目标设备解析）。
    /// 全部成功关闭表单；有失败时只把失败的链接留在文本框里，成功的不会被重复下发。
    private func submitRemote(_ target: DispatchTarget) {
        guard !form.submitting, form.probe == nil else { return }
        if state.isReadOnly {
            reject()
            container.toasts.show(text: L("localServiceDisconnected"), tone: .error, systemImage: FluxSymbol.offline)
            return
        }
        if form.entries.isEmpty {
            reject()
            form.showEmptyError = true
            return
        }
        let saveDir: String?
        switch DeviceRules.checkSaveDir(form.remoteSaveDir, style: target.pathStyle) {
        case .invalid:
            reject()
            return
        case .useDefault: saveDir = nil
        case let .explicit(dir): saveDir = dir
        }
        let items = form.dispatchItems()
        form.submitting = true
        linksFocused = false
        dispatchFailure = nil
        dispatchProgress = (0, items.count)
        let agent = container.agent
        let toasts = container.toasts
        Task {
            let summary = await Dispatcher.run(items, to: target, saveDir: saveDir, agent: agent) { done in
                dispatchProgress = (done, items.count)
            }
            dispatchProgress = nil
            form.submitting = false
            guard let failure = Dispatcher.announce(summary, target: target, toasts: toasts) else {
                close()
                return
            }
            form.retain(summary.failedEntries)
            dispatchFailure = failure
        }
    }

    private func importFiles(_ result: Result<[URL], Error>) {
        switch result {
        case let .failure(error):
            container.toasts.show(text: error.localizedDescription, tone: .error)
        case let .success(urls):
            Task { await mergeTextFiles(urls) }
        }
    }

    /// TXT 导入与拖放共用：宽松解析链接，按 URL 去重合并进文本框。
    private func mergeTextFiles(_ urls: [URL]) async {
        var found: [UrlEntry] = []
        var unreadable: [String] = []
        for url in urls {
            let result = await Task.detached { Result { try readTextFile(url) } }.value
            switch result {
            case let .success(text): found += parseEntries(text, loose: true)
            case .failure: unreadable.append(url.lastPathComponent)
            }
        }
        if !unreadable.isEmpty {
            FluxHaptic.error.play()
            container.toasts.show(text: L("importTxtReadFailed", ["name": unreadable.joined(separator: ", ")]), tone: .error)
        }
        found = found.dedupe()
        if found.isEmpty {
            if unreadable.isEmpty {
                FluxHaptic.error.play()
                container.toasts.show(text: L("importTxtNoUrls"), tone: .warning)
            }
        } else {
            let merged = appendEntries(form.urlText, found)
            form.urlText = merged.text
            container.toasts.show(text: L("importTxtFound", ["count": merged.added]), tone: .success, systemImage: "text.document")
        }
    }

    #if FULL_FLAVOR
    private func importTorrentFiles(_ result: Result<[URL], Error>) {
        switch result {
        case let .failure(error):
            if (error as? CocoaError)?.code == .userCancelled { return }
            container.toasts.show(text: error.localizedDescription, tone: .error)
        case let .success(urls):
            Task { await importTorrents(urls) }
        }
    }

    /// N3：以表单当前保存目录与所选队列立即提交；表单里仍有链接时 N1 保留，否则关闭。
    private func importTorrents(_ urls: [URL]) async {
        guard !urls.isEmpty, !importingTorrent, !form.submitting else { return }
        if !container.isLocalHost, !form.saveDirValid {
            reject()
            return
        }
        importingTorrent = true
        defer { importingTorrent = false }
        let outcome = await TorrentImport.submit(
            urls,
            container: container,
            saveDir: form.saveDir,
            queueId: currentQueueId(state.queues),
            startPaused: false
        )
        if outcome.created > 0, form.urlText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            close()
        }
    }
    #endif

    /// iPad 拖放：`.txt/.url/.list` → TXT 导入路径；full 构建另支持 `.torrent` → 导入；其余 → 不支持提示。
    private func handleDrop(_ providers: [NSItemProvider]) -> Bool {
        guard !providers.isEmpty, !form.submitting else { return false }
        Task {
            let loaded = await DroppedFiles.load(providers)
            defer { DroppedFiles.discard(loaded.files) }
            if loaded.rejected > 0 {
                FluxHaptic.warning.play()
                container.toasts.show(text: L(BuildFlavor.key(full: "unsupportedDropHint", store: "unsupportedDropHintGeneric")), tone: .warning)
            }
            if loaded.failed > 0 {
                FluxHaptic.error.play()
                container.toasts.show(text: L("dropReadFailed", ["count": loaded.failed]), tone: .error)
            }
            let texts = loaded.files.filter { $0.kind == .text }.map(\.url)
            #if FULL_FLAVOR
            let torrents = loaded.files.filter { $0.kind == .torrent }.map(\.url)
            if !torrents.isEmpty {
                if form.target(in: dispatchTargets) != nil {
                    FluxHaptic.warning.play()
                    container.toasts.show(text: L("downloadToTorrentLocalOnly"), tone: .warning)
                } else {
                    await importTorrents(torrents)
                }
            }
            #endif
            if !texts.isEmpty { await mergeTextFiles(texts) }
        }
        return true
    }

    private func advancedLabel(_ item: AdvancedItem, headerCount: Int) -> String {
        switch item {
        case .auth: L("taskHttpAuth")
        case .proxy: L("taskProxy")
        case .userAgent: L("userAgent")
        case .cookie: L("taskCookie")
        case .referrer: L("mobileReferrer")
        case .checksum: L("taskChecksum")
        case .headers: L("taskHeaders") + " ×\(headerCount)"
        case .tls: L("taskIgnoreTlsErrors")
        }
    }
}

/// 队列显示名：主队列 / 稍后下载用本地化名，其余取队列自带名称（N1 与清单选择共用）。
func newTaskQueueLabel(_ q: TaskQueue) -> String {
    switch q.queueId {
    case "", TaskQueue.main: L("mainQueue")
    case TaskQueue.later: L("downloadLater")
    default: q.name
    }
}

nonisolated enum NewDownloadRoute: Hashable {
    case advanced
    #if FULL_FLAVOR
    /// 插件清单选择（N5）：来源 + 组级基础选项。
    case manifest(ManifestSource, ManifestBaseOptions)
    #endif
}

/// 文件选择器的用途：决定可选类型与结果去向。
private nonisolated enum ImporterKind {
    case text
    #if FULL_FLAVOR
    case torrent
    #endif
}

/// 读取文本文件（上限 2 MiB，防止误选大文件）；需要 security-scoped 访问。读取失败向上抛出，由调用方提示。
private nonisolated func readTextFile(_ url: URL) throws -> String {
    let scoped = url.startAccessingSecurityScopedResource()
    defer { if scoped { url.stopAccessingSecurityScopedResource() } }
    let handle = try FileHandle(forReadingFrom: url)
    defer { try? handle.close() }
    let data = try handle.read(upToCount: 2 * 1024 * 1024) ?? Data()
    return String(decoding: data, as: UTF8.self)
}

// MARK: 预览行

private struct PreviewRow: View {
    let entry: UrlEntry
    var body: some View {
        let proto = protocolOf(entry.url)
        let name = inferName(entry)
        let tag = Self.tag(proto)
        let subtitle = hostOrNull(entry.url) ?? tag
        HStack(spacing: 12) {
            KindIcon(kind: proto == .bt ? .torrent : FileKind.from(fileName: name), size: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                    .font(.subheadline.monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            ProtocolBadge(text: tag)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(tag), \(name), \(subtitle)")
    }

    static func tag(_ p: TaskProtocol) -> String { TaskDetailFormat.protocolTag(p) }
}
