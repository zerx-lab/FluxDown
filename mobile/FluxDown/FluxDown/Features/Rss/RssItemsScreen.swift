import FluxDomain
import FluxUI
import SwiftUI
import UIKit

/// R2 条目流：某订阅的条目列表；搜索、按发布时间排序、选择模式（批量下载 / 忽略 / 已读）、
/// 条目状态与关联任务状态联动（`HostState.tasks` 按 `taskId`，不另行轮询）。
/// 过滤原因文案来自引擎的稳定原因码，求值由主机完成。
struct RssItemsScreen: View {
    @Environment(AppContainer.self) private var container
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var model: RssItemsModel
    private let rss: RssModel

    init(sourceId: String, container: AppContainer, actions: TaskActions, rss: RssModel) {
        _model = State(initialValue: RssItemsModel(sourceId: sourceId, container: container, actions: actions))
        self.rss = rss
    }

    /// 重拉触发键：条目流修订号 / 手动重试 / 重连恢复。
    private nonisolated struct LoadKey: Hashable {
        var revision: UInt64
        var tick: Int
        var live: Bool
    }

    var body: some View {
        @Bindable var model = model
        let state = container.store.state
        let source = state.rssSources.first { $0.sourceId == model.sourceId }
        let visible = model.visible()
        let linked = linkedTasks(state)
        let readOnly = state.isReadOnly
        let key = LoadKey(revision: revision(state), tick: model.reloadTick, live: state.connection == .live)
        ScrollViewReader { proxy in
            List(selection: $model.selection) {
                ForEach(visible, id: \.guid) { item in
                    itemRow(item, linked: linked[item.taskId], readOnly: readOnly, tasksLoading: state.tasksLoading)
                }
            }
            .listStyle(.plain)
            .environment(\.editMode, Binding(
                get: { model.isSelecting ? .active : .inactive },
                set: { model.isSelecting = $0.isEditing }
            ))
            .onChange(of: model.scrollTopTick) {
                guard let first = visible.first?.guid else { return }
                withAnimation(reduceMotion ? nil : .smooth) { proxy.scrollTo(first, anchor: .top) }
            }
        }
        .overlay { emptyState(source: source, visible: visible, readOnly: readOnly) }
        .safeAreaBar(edge: .top, spacing: 0) { header(source: source, visible: visible, readOnly: readOnly) }
        .searchable(
            text: $model.query, placement: .navigationBarDrawer(displayMode: .always), prompt: L("rssSearchHint")
        )
        .refreshable { await refresh(source) }
        .navigationTitle(source.map(RssFormat.title(of:)) ?? "")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbarContent(source: source, visible: visible, readOnly: readOnly) }
        .toolbarVisibility(model.isSelecting ? .hidden : .automatic, for: .tabBar)
        .suppressesBottomAccessory("rss.items.selecting", when: model.isSelecting)
        .task(id: key) { if key.live { await model.load() } }
        .onChange(of: container.store.notices.last?.id) {
            model.consumeNotices(container.store.notices)
        }
        .fluxAnimation(.smooth, value: visible.map(\.guid))
        .sensoryFeedback(trigger: model.feedback) { @Sendable _, new in
            switch new?.tone {
            case .success: .success
            case .error: .error
            default: nil
            }
        }
    }

    // MARK: 派生

    private func revision(_ state: HostState) -> UInt64 {
        state.section(HostSection.daemonRssItemRevisions, as: [String: UInt64].self)?[model.sourceId] ?? 0
    }

    /// 已建任务条目引用的任务的当前状态（只取被引用的任务）。
    private func linkedTasks(_ state: HostState) -> [String: RssLinkedTask] {
        let ids = model.linkedTaskIds
        guard !ids.isEmpty else { return [:] }
        return state.tasks.reduce(into: [:]) { result, task in
            if ids.contains(task.taskId) { result[task.taskId] = RssLinkedTask(task) }
        }
    }

    private func refresh(_ source: RssSource?) async {
        guard let source else { return }
        rss.refresh(source)
        // 下拉刷新持续到抓取结束，避免转圈提前消失。
        while rss.busy.contains(source.sourceId) {
            try? await Task.sleep(for: .milliseconds(200))
        }
    }

    // MARK: 行

    @ViewBuilder
    private func itemRow(_ item: RssItemDto, linked: RssLinkedTask?, readOnly: Bool, tasksLoading: Bool) -> some View {
        let busy = model.busy.contains(item.guid)
        let hasTask = item.state == .downloaded && linked != nil
        RssItemRow(
            item: item,
            chip: RssFormat.chip(for: item, task: linked, tasksLoading: tasksLoading),
            busy: busy,
            expanded: model.expanded.contains(item.guid),
            selecting: model.isSelecting,
            canAct: !readOnly,
            hasTask: hasTask,
            onTap: { tap(item, hasTask: hasTask) },
            onDownload: { model.act([item.guid], action: .download) },
            onIgnore: { model.act([item.guid], action: .ignore) },
            onCopy: { model.copyLink(item) },
            onOpenTask: { container.router.showTask(item.taskId) }
        )
        .swipeActions(edge: .leading, allowsFullSwipe: true) {
            if !readOnly {
                Button(downloadTitle(item, busy: busy), systemImage: "arrow.down.circle") {
                    model.act([item.guid], action: .download)
                }
                .tint(.accentColor)
                .disabled(busy)
            }
        }
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            if !readOnly, item.state == .new {
                Button(L("rssActionIgnore"), systemImage: "eye.slash") {
                    model.act([item.guid], action: .ignore)
                }
                .tint(Color.fdStatusPaused)
                .disabled(busy)
            }
        }
        .contextMenu {
            if hasTask {
                Button(L("mobileRssOpenTask"), systemImage: FluxSymbol.openFile) {
                    container.router.showTask(item.taskId)
                }
                if item.imageCount > 0, !readOnly, !model.isSelecting, !model.expanded.contains(item.guid) {
                    Button(L("mobileExpand"), systemImage: "chevron.down") {
                        model.expanded.insert(item.guid)
                    }
                }
            }
            Button(downloadTitle(item, busy: busy), systemImage: "arrow.down.circle") {
                model.act([item.guid], action: .download)
            }
            .disabled(busy || readOnly)
            if item.state == .new {
                Button(L("rssActionIgnore"), systemImage: "eye.slash") {
                    model.act([item.guid], action: .ignore)
                }
                .disabled(busy || readOnly)
            }
            if !item.effectiveLink.isEmpty {
                Button(L("copyUrl"), systemImage: FluxSymbol.copy) { model.copyLink(item) }
            }
            Button(L("mobileRssSelect"), systemImage: "checklist") {
                model.isSelecting = true
                model.selection = [item.guid]
            }
        }
    }

    private func downloadTitle(_ item: RssItemDto, busy: Bool) -> String {
        if busy { return L("rssActionPreparing") }
        return L(item.state == .downloaded ? "rssActionRedownload" : "rssActionDownload")
    }

    /// 点按：已建任务且任务仍在 → 打开任务详情；否则展开行内详情。
    private func tap(_ item: RssItemDto, hasTask: Bool) {
        if hasTask {
            container.router.showTask(item.taskId)
        } else if model.expanded.contains(item.guid) {
            model.expanded.remove(item.guid)
        } else {
            model.expanded.insert(item.guid)
        }
    }

    // MARK: 顶部（横幅 / 订阅摘要 / 选择条）

    @ViewBuilder
    private func header(source: RssSource?, visible: [RssItemDto], readOnly: Bool) -> some View {
        let showsBanner = container.store.state.connection.isStaleOrFailed
        let hasContent = showsBanner || model.feedback != nil || source != nil || model.isSelecting
        if hasContent {
            VStack(spacing: 8) {
                if showsBanner {
                    Banner(text: L("localServiceDisconnected"), tone: .warning, systemImage: FluxSymbol.offline, slim: true)
                }
                if let feedback = model.feedback {
                    Banner(
                        text: feedback.text,
                        tone: feedback.tone,
                        systemImage: feedback.systemImage,
                        slim: true,
                        action: BannerAction(title: L("close")) { model.dismissFeedback() }
                    )
                    .onTapGesture { if feedback.jumpsToTop { model.jumpToTop() } }
                    .accessibilityAddTraits(feedback.jumpsToTop ? .isButton : [])
                }
                if let source {
                    summary(source)
                }
                if model.isSelecting {
                    selectionBar(visible: visible)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
    }

    private func summary(_ source: RssSource) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                RssStatusLine(source: source, refreshing: rss.busy.contains(source.sourceId))
                Spacer(minLength: 8)
                if source.unreadCount > 0 {
                    Text(L("rssUnreadCount", ["n": source.unreadCount]))
                        .font(.footnote.weight(.medium).monospacedDigit())
                        .foregroundStyle(.tint)
                        .contentTransition(.numericText())
                }
            }
            if source.failCount > 0, !source.lastError.isEmpty {
                Text(source.lastError)
                    .font(.caption.monospaced())
                    .foregroundStyle(Color.fdStatusFailedText)
                    .lineLimit(2)
                    .textSelection(.enabled)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }

    /// 选择条：三态复选（只作用于当前可见条目）+ 取消选择。
    private func selectionBar(visible: [RssItemDto]) -> some View {
        let selectedVisible = visible.filter { model.selection.contains($0.guid) }.count
        let all = !visible.isEmpty && selectedVisible == visible.count
        let symbol = all ? "checkmark.circle.fill" : (selectedVisible > 0 ? "minus.circle.fill" : "circle")
        return HStack {
            Button {
                model.setVisibleSelected(visible, selected: !all)
            } label: {
                Label(L("rssSelectVisible"), systemImage: symbol)
                    .font(.subheadline)
            }
            .disabled(visible.isEmpty)
            .sensoryFeedback(.selection, trigger: selectedVisible)
            Spacer(minLength: 8)
            if !model.selection.isEmpty {
                Button(L("rssClearSelection")) { model.clearSelection() }
                    .font(.subheadline)
            }
        }
    }

    // MARK: 空 / 加载 / 失败

    @ViewBuilder
    private func emptyState(source: RssSource?, visible: [RssItemDto], readOnly: Bool) -> some View {
        if visible.isEmpty {
            let trimmed = model.query.trimmingCharacters(in: .whitespacesAndNewlines)
            if model.phase == .loading {
                if container.store.state.connection.isStaleOrFailed {
                    ContentUnavailableView(L("localServiceDisconnected"), systemImage: FluxSymbol.offline)
                } else {
                    fetching
                }
            } else if !trimmed.isEmpty, !model.items.isEmpty {
                ContentUnavailableView(
                    L("rssNoMatch", ["query": trimmed.lowercased()]),
                    systemImage: FluxSymbol.search
                )
            } else if model.phase == .failed || (source?.lastError.isEmpty == false) {
                ContentUnavailableView {
                    Label(L("rssEmptyError"), systemImage: "exclamationmark.triangle")
                } description: {
                    Text(L("rssEmptyErrorHint"))
                } actions: {
                    Button(L("rssEmptyRetry")) {
                        if model.phase == .failed { model.reload() } else if let source { rss.refresh(source) }
                    }
                    .disabled(readOnly)
                    Button(L("rssCheckConfig")) {
                        rss.openEditor(.edit(sourceId: model.sourceId))
                    }
                    .disabled(readOnly)
                }
            } else if let source, source.enabled, source.lastSuccessAt == 0 {
                fetching
            } else {
                ContentUnavailableView {
                    Label(L("rssEmptyTitle"), systemImage: "dot.radiowaves.up.forward")
                } description: {
                    Text(L("rssEmptyDesc"))
                }
            }
        }
    }

    private var fetching: some View {
        ContentUnavailableView {
            ProgressView().controlSize(.large)
            Text(L("rssEmptyFetching")).font(.headline)
        } description: {
            Text(L("rssEmptyFetchingHint"))
        }
    }

    // MARK: 工具栏

    @ToolbarContentBuilder
    private func toolbarContent(source: RssSource?, visible: [RssItemDto], readOnly: Bool) -> some ToolbarContent {
        if model.isSelecting {
            ToolbarItem(placement: .primaryAction) {
                Button(L("mobileRssSelectDone")) { model.toggleSelecting() }
                    .fontWeight(.semibold)
            }
            selectionBottomBar(readOnly: readOnly)
        } else {
            ToolbarItem(placement: .primaryAction) {
                Button(L("mobileRssSelect"), systemImage: "checklist") { model.toggleSelecting() }
                    .disabled(model.items.isEmpty)
            }
            if let source {
                ToolbarItem(placement: .primaryAction) {
                    if rss.busy.contains(source.sourceId) {
                        ProgressView()
                            .accessibilityLabel(L("rssRefreshing"))
                    } else {
                        Button(L("rssRefreshNow"), systemImage: FluxSymbol.retry) { rss.refresh(source) }
                            .disabled(readOnly)
                    }
                }
                ToolbarItem(placement: .primaryAction) {
                    moreMenu(source: source, readOnly: readOnly)
                }
            }
        }
    }

    private func moreMenu(source: RssSource, readOnly: Bool) -> some View {
        @Bindable var model = model
        return Menu {
            Picker(L("rssPublishedAt"), selection: $model.oldestFirst) {
                Text(L("rssSortNewest")).tag(false)
                Text(L("rssSortOldest")).tag(true)
            }
            .pickerStyle(.inline)
            Section {
                Button(L("rssManageTitle"), systemImage: "slider.horizontal.3") {
                    rss.openEditor(.edit(sourceId: source.sourceId))
                }
                .disabled(readOnly)
                Button(L("rssMarkAllRead"), systemImage: "checkmark.circle") { model.markAllRead() }
                    .disabled(readOnly || model.readBusy || source.unreadCount == 0)
                Button(L("copyUrl"), systemImage: FluxSymbol.copy) { rss.copyLink(source) }
            }
            Section {
                Button(L("rssDeleteSource"), systemImage: FluxSymbol.delete, role: .destructive) { rss.requestDelete(source, from: .itemsMenu) }
                    .disabled(readOnly)
            }
        } label: {
            Label(L("moreActions"), systemImage: FluxSymbol.more)
        }
        .rssDeleteConfirmation(rss, source: source, origin: .itemsMenu)
    }

    /// 选择模式的底部工具栏（系统 `.bottomBar`：只渲染一层 Liquid Glass）。
    @ToolbarContentBuilder
    private func selectionBottomBar(readOnly: Bool) -> some ToolbarContent {
        let selected = model.selection
        let ignorable = model.items.filter { $0.state == .new && selected.contains($0.guid) }.map(\.guid)
        let downloadable = selected.filter { !model.busy.contains($0) }
        ToolbarItem(placement: .bottomBar) {
            Text(L("rssSelectedCount", ["n": selected.count]))
                .font(.subheadline.monospacedDigit())
                .contentTransition(.numericText())
        }
        ToolbarSpacer(.flexible, placement: .bottomBar)
        ToolbarItem(placement: .bottomBar) {
            Button(L("rssIgnoreSelected"), systemImage: "eye.slash") {
                model.act(ignorable, action: .ignore)
            }
            .disabled(readOnly || ignorable.allSatisfy { model.busy.contains($0) })
        }
        ToolbarItem(placement: .bottomBar) {
            Button(L("rssDownloadSelected"), systemImage: "arrow.down.circle") {
                model.act(Array(selected), action: .download)
            }
            .disabled(readOnly || downloadable.isEmpty)
        }
    }
}

private extension Connection {
    var isStaleOrFailed: Bool {
        switch self {
        case .stale, .failed: true
        case .live, .connecting: false
        }
    }
}

// MARK: - 条目行

/// 条目行：标题（新条目加粗）/ 元信息 / 状态 chip + 过滤原因；点按展开完整标题与链接。
/// chip 形状 + 颜色双通道；下载中带 14pt 进度环。
private struct RssItemRow: View {
    let item: RssItemDto
    let chip: RssItemChip
    let busy: Bool
    let expanded: Bool
    let selecting: Bool
    let canAct: Bool
    let hasTask: Bool
    let onTap: () -> Void
    let onDownload: () -> Void
    let onIgnore: () -> Void
    let onCopy: () -> Void
    let onOpenTask: () -> Void

    var body: some View {
        let meta = RssFormat.metaText(of: item)
        let reasonKey = item.reasonCode.i18nKey
        VStack(alignment: .leading, spacing: 6) {
            Text(item.title)
                .font(.body)
                .fontWeight(item.state == .new ? .semibold : .regular)
                .lineLimit(expanded ? nil : 2)
                .frame(maxWidth: .infinity, alignment: .leading)
            if !meta.isEmpty {
                Text(meta)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    chipView
                    reasonView(reasonKey)
                }
                VStack(alignment: .leading, spacing: 4) {
                    chipView
                    reasonView(reasonKey)
                }
            }
            if expanded {
                details
            }
        }
        .padding(.vertical, 4)
        .frame(minHeight: 60, alignment: .topLeading)
        .contentShape(Rectangle())
        .modifier(TapWhen(enabled: !selecting, action: onTap))
        .accessibilityElement(children: expanded ? .contain : .combine)
        .accessibilityLabel(accessibilityDescription(meta: meta, reasonKey: reasonKey))
        .accessibilityAddTraits(selecting ? [] : .isButton)
    }

    private var chipView: some View {
        HStack(spacing: 6) {
            if let progress = chip.progress {
                ProgressRing(fraction: progress)
                    .frame(width: 14, height: 14)
                    .accessibilityHidden(true)
            }
            StatusBadge(
                text: busy ? L("rssActionPreparing") : L(chip.titleKey),
                tone: chip.tone,
                systemImage: chip.progress == nil ? chip.systemImage : nil
            )
            .contentTransition(.interpolate)
            .fluxAnimation(.smooth, value: chip)
        }
    }

    @ViewBuilder
    private func reasonView(_ key: String?) -> some View {
        if let key {
            Text(L(key))
                .font(.caption)
                .foregroundStyle(Color.fdStatusWarningText)
                .lineLimit(2)
        }
    }

    private var details: some View {
        VStack(alignment: .leading, spacing: 8) {
            if !item.effectiveLink.isEmpty {
                Text(item.effectiveLink)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(5)
                    .textSelection(.enabled)
            }
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) { detailActions }
                VStack(alignment: .leading, spacing: 4) { detailActions }
            }
            .font(.footnote)
            .buttonStyle(.borderless)
            if item.imageCount > 0, !selecting, canAct {
                RssItemImages(item: item)
            }
        }
        .padding(.top, 2)
    }

    @ViewBuilder
    private var detailActions: some View {
        if !item.effectiveLink.isEmpty {
            Button(L("copyUrl"), systemImage: FluxSymbol.copy, action: onCopy)
        }
        if hasTask {
            Button(L("mobileRssOpenTask"), systemImage: FluxSymbol.openFile, action: onOpenTask)
        }
        Button(downloadTitle, systemImage: "arrow.down.circle", action: onDownload)
            .disabled(busy || !canAct)
        if item.state == .new {
            Button(L("rssActionIgnore"), systemImage: "eye.slash", action: onIgnore)
                .disabled(busy || !canAct)
        }
    }

    private var downloadTitle: String {
        if busy { return L("rssActionPreparing") }
        return L(item.state == .downloaded ? "rssActionRedownload" : "rssActionDownload")
    }

    private func accessibilityDescription(meta: String, reasonKey: String?) -> String {
        var parts = [item.title, busy ? L("rssActionPreparing") : L(chip.titleKey)]
        if !meta.isEmpty { parts.append(meta) }
        if let reasonKey { parts.append(L(reasonKey)) }
        return parts.joined(separator: ", ")
    }
}

private struct RssItemImages: View {
    @Environment(AppContainer.self) private var container
    let item: RssItemDto

    var body: some View {
        RssImageDisclosure(item: item)
            .id(Identity(
                hostId: container.host.id,
                itemId: item.id,
                count: item.imageCount
            ))
    }

    private nonisolated struct Identity: Hashable {
        let hostId: String
        let itemId: String
        let count: Int
    }
}

private struct RssImageDisclosure: View {
    let item: RssItemDto
    @State private var requested = false

    var body: some View {
        Button(L("rssViewImages"), systemImage: "photo") { requested.toggle() }
            .font(.footnote)
            .buttonStyle(.borderless)
        if requested {
            ForEach(0..<item.imageCount, id: \.self) { index in
                RssPreviewImage(sourceId: item.sourceId, guid: item.guid, index: index)
            }
        }
    }
}

private struct RssPreviewImage: View {
    @Environment(AppContainer.self) private var container
    let sourceId: String
    let guid: String
    let index: Int
    @State private var image: UIImage?
    @State private var failureKey: String?

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFit()
                    .frame(maxHeight: 240)
                    .accessibilityLabel(L("rssViewImages"))
            } else if let failureKey {
                Text(L(failureKey))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            } else {
                ProgressView(L("rssImageLoading"))
                    .font(.footnote)
            }
        }
        .task { await load() }
    }

    private func load() async {
        do throws(HostError) {
            let response: Reply = try await container.session.call(
                HostMethod.daemonRssGetImage,
                params: Params(sourceId: sourceId, guid: guid, index: index)
            )
            guard !Task.isCancelled else { return }
            if let data = Data(base64Encoded: response.pngBase64), let decoded = UIImage(data: data) {
                image = decoded
            } else {
                failureKey = "rssImageLoadFailed"
            }
        } catch {
            if !Task.isCancelled {
                failureKey = error.reason == "rssImageProxyUnverifiable"
                    ? "rssImageProxyUnverifiable" : "rssImageLoadFailed"
            }
        }
    }

    private nonisolated struct Params: Encodable, Sendable {
        let sourceId: String
        let guid: String
        let index: Int
    }

    private nonisolated struct Reply: Decodable, Sendable {
        let pngBase64: String
    }
}

/// 选择模式下把点按交还给系统（行选择），否则响应点按。
private struct TapWhen: ViewModifier {
    let enabled: Bool
    let action: () -> Void

    @ViewBuilder
    func body(content: Content) -> some View {
        if enabled {
            content.onTapGesture(perform: action)
        } else {
            content
        }
    }
}

/// 14pt 进度环（下载中的关联任务）；轨道用系统填充色，进度用强调色。
private struct ProgressRing: View {
    let fraction: Double

    var body: some View {
        ZStack {
            Circle().stroke(Color.fdProgressTrack, lineWidth: 2)
            Circle()
                .trim(from: 0, to: max(fraction, 0.04))
                .stroke(Color.accentColor, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
    }
}
