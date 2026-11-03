import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

// N5 · 插件清单预解析 → 任务组（`02-downloads.md` §15）。
// N1 单条 http(s) 链接经 `daemon.group.resolvePreview` 命中清单，数据已就绪后推入本页。

/// 清单来源。
nonisolated enum ManifestSource: Hashable {
    case daemon(preview: ResolvePreviewResponse, sourceUrl: String)
}

nonisolated struct ManifestBaseOptions: Hashable {
    var saveDir = ""
    var offeredSaveDir: String?
    var queueId = ""
    var segments: Int32 = 0
    var cookies = ""
    var referrer = ""
    var userAgent = ""
    var proxyUrl = ""
    var headers: [String: String] = [:]
    var ignoreTls = false

    func previewRequest(url: String) -> ResolvePreviewRequest {
        ResolvePreviewRequest(url: url, cookies: cookies, referrer: referrer, userAgent: userAgent, extraHeaders: headers)
    }

    func groupRequest(
        sourceUrl: String,
        groupName: String,
        queueId: String,
        startPaused: Bool,
        items: [GroupItemRequest],
        resolverPluginId: String,
        archiveMetadata: ArchiveMetadataDto?
    ) -> CreateGroupRequest {
        CreateGroupRequest(
            sourceUrl: sourceUrl,
            groupName: groupName,
            saveDir: saveDir.trimmingCharacters(in: .whitespacesAndNewlines),
            queueId: queueId,
            segments: segments,
            cookies: cookies,
            referrer: referrer,
            userAgent: userAgent,
            proxyUrl: proxyUrl,
            extraHeaders: headers,
            ignoreTlsErrors: ignoreTls,
            startPaused: startPaused,
            items: items,
            resolverPluginId: resolverPluginId,
            archiveMetadata: archiveMetadata
        )
    }
}

// MARK: - 模型

@MainActor
@Observable
final class ManifestSelectModel {
    var base: ManifestBaseOptions
    let manifestName: String
    let sourceUrl: String
    /// 预览产出清单的插件 id，建组时回传以绑定同一插件。
    let resolverPluginId: String
    let archiveMetadata: ArchiveMetadataDto?
    var selection: ManifestSelection
    var groupName: String
    var search = ""
    private(set) var submitting = false

    init(source: ManifestSource, base: ManifestBaseOptions) {
        self.base = base
        switch source {
        case let .daemon(preview, sourceUrl):
            manifestName = preview.name
            self.sourceUrl = sourceUrl
            resolverPluginId = preview.pluginId
            archiveMetadata = preview.archiveMetadata
            selection = ManifestSelection(items: preview.items)
            groupName = ManifestSelection.defaultGroupName(manifestName: preview.name, sourceUrl: sourceUrl)
        }
    }

    var host: String { URL(string: sourceUrl)?.host ?? "" }

    // MARK: 可见条目（搜索）

    var visibleItems: [PreviewItemDto] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !query.isEmpty else { return selection.items }
        return selection.items.filter {
            $0.name.lowercased().contains(query) || $0.path.lowercased().contains(query)
        }
    }

    func selectAll(in ids: [String]) {
        for id in ids { selection.set(id, selected: true) }
    }

    func clear(in ids: [String]) {
        for id in ids { selection.set(id, selected: false) }
    }

    // MARK: 摘要

    var totalSizeText: String {
        let total = selection.items.reduce(Int64(0)) { $0 + max($1.size, 0) }
        return total > 0 ? Format.bytes(total).description : L("manifestFileSizeUnknown")
    }

    var summaryText: String {
        guard selection.count > 0 else { return L("manifestNoSelection") }
        let size = selection.selectedSize
        let head = L("manifestSelectedSummary", ["count": selection.count, "size": Format.bytes(size.bytes).description])
        guard size.unknown > 0 else { return head }
        return head + " " + L("manifestUnknownSizeNote", ["count": size.unknown])
    }

    // MARK: 建组

    var canSubmit: Bool { !submitting && selection.count > 0 }

    /// 成功返回 `true`（调用方关闭整个 Sheet）；失败已 Toast 并保留页面。
    func submit(startPaused: Bool, queueId: String, container: AppContainer) async -> Bool {
        guard canSubmit else { return false }
        let toasts = container.toasts
        if container.store.state.isReadOnly {
            FluxHaptic.error.play()
            toasts.show(text: L("localServiceDisconnected"), tone: .error, systemImage: FluxSymbol.offline)
            return false
        }
        if !container.isLocalHost, !isValidSaveDir(base.saveDir) {
            FluxHaptic.error.play()
            return false
        }
        submitting = true
        defer { submitting = false }
        let trimmed = groupName.trimmingCharacters(in: .whitespacesAndNewlines)
        let request = base.groupRequest(
            sourceUrl: sourceUrl,
            groupName: trimmed.isEmpty ? manifestName : trimmed,
            queueId: queueId,
            startPaused: startPaused,
            items: selection.requestItems(),
            resolverPluginId: resolverPluginId,
            archiveMetadata: archiveMetadata
        )
        let session = container.session
        let count = request.items.count
        do throws(HostError) {
            let _: CreateGroupResponse = try await session.createGroup(request, offeredSaveDir: base.offeredSaveDir)
        } catch {
            FluxHaptic.error.play()
            toasts.show(text: ErrorText.describe(error), tone: .error, systemImage: "exclamationmark.circle")
            return false
        }
        FluxHaptic.success.play()
        toasts.show(text: L("manifestGroupCreatedToast", ["count": count]), tone: .success, systemImage: "square.stack.3d.down.right")
        return true
    }
}

// MARK: - 视图

/// 清单选择页：在 N1 的 `NavigationStack` 内推入。
/// `close` 关闭承载它的整个 Sheet（建组成功后调用）。
struct ManifestSelectView: View {
    let close: () -> Void

    @Environment(AppContainer.self) private var container
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @State private var model: ManifestSelectModel

    init(source: ManifestSource, base: ManifestBaseOptions, close: @escaping () -> Void) {
        _model = State(initialValue: ManifestSelectModel(source: source, base: base))
        self.close = close
    }

    private var state: HostState { container.store.state }

    var body: some View {
        readyList
            .navigationTitle(L("manifestDialogTitle"))
            .navigationBarTitleDisplayMode(.inline)
            .navigationBarBackButtonHidden(model.submitting)
            .interactiveDismissDisabled(model.submitting)
    }

    // MARK: 就绪

    private var readyList: some View {
        let visible = model.visibleItems
        let ids = visible.map(\.id)
        return List {
            bannerSection
            groupSection
            summarySection(ids)
            itemsSection(visible)
        }
        .listStyle(.insetGrouped)
        .scrollDismissesKeyboard(.interactively)
        .searchable(
            text: $model.search,
            placement: .navigationBarDrawer(displayMode: .automatic),
            prompt: L("manifestSearchPlaceholder")
        )
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .fluxAnimation(.snappy, value: ids)
        .toolbar { bottomBar }
    }

    private var bannerSection: some View {
        Section {
            let host = model.host
            Banner(
                text: host.isEmpty ? L("manifestPluginBadge") : L("manifestPluginBadge") + " · " + host,
                tone: .info,
                systemImage: "puzzlepiece.extension",
                slim: true
            )
            .listRowInsets(EdgeInsets())
            .listRowBackground(Color.clear)
        }
    }

    // MARK: 组卡

    private var groupSection: some View {
        let queues = state.queues
        return Section {
            TextField(L("manifestGroupNameTooltip"), text: $model.groupName, prompt: Text(L("manifestGroupNamePlaceholder")))
                .disabled(model.submitting)
            if container.isLocalHost {
                LabeledContent(L("saveDir")) {
                    Text(model.base.saveDir.isEmpty ? LocalPaths.documents.path : model.base.saveDir)
                        .font(.footnote.monospaced())
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .multilineTextAlignment(.trailing)
                        .textSelection(.enabled)
                }
            } else {
                TextField(L("saveDir"), text: $model.base.saveDir)
                    .font(.callout.monospaced())
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .disabled(model.submitting)
                if !isValidSaveDir(model.base.saveDir) {
                    Label(L("mobileSaveDirInvalid"), systemImage: FluxSymbol.failure)
                        .font(.footnote)
                        .foregroundStyle(Color.fdStatusFailedText)
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
                .disabled(model.submitting)
            }
        }
    }

    /// 基础队列不在列表中（列表晚到 / 队列被删）→ 显示并提交第一个队列。
    private func queueChoice(_ queues: [TaskQueue]) -> Binding<String> {
        Binding(
            get: { effectiveQueueId(queues) },
            set: { model.base.queueId = $0 }
        )
    }

    private func effectiveQueueId(_ queues: [TaskQueue]) -> String {
        queues.contains { $0.queueId == model.base.queueId } ? model.base.queueId : (queues.first?.queueId ?? model.base.queueId)
    }

    // MARK: 摘要与批量操作（作用于当前可见 = 搜索结果）

    private func summarySection(_ ids: [String]) -> some View {
        let allSelected = !ids.isEmpty && ids.allSatisfy { model.selection.selected.contains($0) }
        let anySelected = ids.contains { model.selection.selected.contains($0) }
        return Section {
            HStack(spacing: 12) {
                Text(model.summaryText)
                    .font(.subheadline)
                    .monospacedDigit()
                    .accessibilityAddTraits(.updatesFrequently)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Menu {
                    selectAllButton(ids, disabled: allSelected)
                    clearButton(ids, disabled: !anySelected)
                    invertButton(ids)
                } label: {
                    Image(systemName: "checklist")
                }
                .accessibilityLabel(L("manifestSelectAll"))
            }
        }
    }

    private func selectAllButton(_ ids: [String], disabled: Bool) -> some View {
        Button(L("manifestSelectAll"), systemImage: "checkmark.circle") {
            FluxHaptic.selection.play()
            model.selectAll(in: ids)
        }
        .disabled(disabled || ids.isEmpty || model.submitting)
    }

    private func clearButton(_ ids: [String], disabled: Bool) -> some View {
        Button(L("manifestClearSelection"), systemImage: "circle") {
            FluxHaptic.selection.play()
            model.clear(in: ids)
        }
        .disabled(disabled || model.submitting)
    }

    private func invertButton(_ ids: [String]) -> some View {
        Button(L("manifestInvertSelection"), systemImage: "circle.lefthalf.filled") {
            FluxHaptic.selection.play()
            model.selection.invert(in: ids)
        }
        .disabled(ids.isEmpty || model.submitting)
    }

    // MARK: 条目

    private func itemsSection(_ items: [PreviewItemDto]) -> some View {
        Section {
            if items.isEmpty {
                Text(L("manifestTreeEmpty"))
                    .foregroundStyle(.secondary)
            }
            ForEach(items) { item in
                itemRow(item)
            }
        } header: {
            Text(L("manifestSummary", ["count": model.selection.items.count, "size": model.totalSizeText]))
        }
    }

    private func itemRow(_ item: PreviewItemDto) -> some View {
        let on = model.selection.selected.contains(item.id)
        let size = model.selection.effectiveSize(of: item)
        let sizeText = size > 0 ? Format.bytes(size).description : L("manifestFileSizeUnknown")
        let layout = dynamicTypeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 8))
            : AnyLayout(HStackLayout(spacing: 12))
        return layout {
            Button {
                FluxHaptic.selection.play()
                model.selection.toggle(item.id)
            } label: {
                HStack(spacing: 12) {
                    Image(systemName: on ? FluxSymbol.success : "circle")
                        .font(.title3)
                        .foregroundStyle(on ? AnyShapeStyle(TintShapeStyle()) : AnyShapeStyle(HierarchicalShapeStyle.secondary))
                        .accessibilityHidden(true)
                    KindIcon(kind: FileKind.from(fileName: item.name), size: 32)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(item.name)
                            .font(.body)
                            .lineLimit(2)
                            .multilineTextAlignment(.leading)
                        if !item.path.isEmpty {
                            Text(item.path)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Text(sizeText)
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 0)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .disabled(model.submitting)
            .accessibilityLabel("\(item.name), \(sizeText)")
            .accessibilityAddTraits(on ? .isSelected : [])
            if !item.variants.isEmpty {
                variantMenu(item)
            }
        }
    }

    private func variantMenu(_ item: PreviewItemDto) -> some View {
        let title = model.selection.variant(for: item)?.label ?? L("manifestVariantDefault")
        let binding = Binding<String?>(
            get: { model.selection.variants[item.id] },
            set: { model.selection.chooseVariant($0, for: item.id) }
        )
        return Menu {
            Picker(L("manifestVariantLabel"), selection: binding) {
                Text(L("manifestVariantDefault")).tag(String?.none)
                ForEach(item.variants) { variant in
                    Text(variant.size > 0 ? "\(variant.label) · \(Format.bytes(variant.size).description)" : variant.label)
                        .tag(String?.some(variant.id))
                }
            }
            .pickerStyle(.inline)
        } label: {
            HStack(spacing: 4) {
                Text(title).lineLimit(1)
                Image(systemName: "chevron.up.chevron.down").font(.caption2)
            }
            .frame(maxWidth: 160)
        }
        .buttonStyle(.borderless)
        .disabled(model.submitting)
        .accessibilityLabel(L("manifestVariantLabel"))
        .accessibilityValue(title)
    }

    // MARK: 底部操作栏（系统 `.bottomBar`，与 N1 同构）

    @ToolbarContentBuilder
    private var bottomBar: some ToolbarContent {
        let queues = state.queues
        let queueId = effectiveQueueId(queues)
        let count = model.selection.count
        let canSubmit = model.canSubmit
        ToolbarItem(placement: .bottomBar) {
            Button { run(startPaused: true, queueId: queueId) } label: {
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
                            Button(L("manifestStartToQueue", ["name": newTaskQueueLabel(q)]), systemImage: "arrow.down") {
                                run(startPaused: false, queueId: q.queueId)
                            }
                        }
                    }
                    Section {
                        ForEach(queues) { q in
                            Button(L("manifestLaterToQueue", ["name": newTaskQueueLabel(q)]), systemImage: "clock") {
                                run(startPaused: true, queueId: q.queueId)
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
            Button { run(startPaused: false, queueId: queueId) } label: {
                if dynamicTypeSize.isAccessibilitySize {
                    Label(L("manifestStartDownloadWithCount", ["count": count]), systemImage: "arrow.down").labelStyle(.iconOnly)
                } else {
                    Label(L("manifestStartDownloadWithCount", ["count": count]), systemImage: "arrow.down").lineLimit(1)
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(!canSubmit)
            .accessibilityLabel(L("manifestStartDownloadWithCount", ["count": count]))
        }
    }

    private func run(startPaused: Bool, queueId: String) {
        guard model.canSubmit else { return }
        Task {
            if await model.submit(startPaused: startPaused, queueId: queueId, container: container) { close() }
        }
    }
}
#endif
