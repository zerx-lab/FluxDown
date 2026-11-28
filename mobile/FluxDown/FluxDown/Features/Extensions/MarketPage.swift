import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

/// S11.2 · 插件市场：`daemon.plugin.marketList` 拉取索引，本地关键字过滤 + 分页展开，`marketInstall` 安装
/// （安装前确认权限，撤回版本不可安装）。
struct MarketPage: View {
    @Environment(HostStore.self) private var store
    @Environment(ExtensionsModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var plugins = SectionMemo<[PluginDto]>(empty: [])
    @State private var query = ""
    @State private var limit = PluginMarket.pageSize

    var body: some View {
        let state = store.state
        let list = plugins.value(state.sections[HostSection.daemonPlugins])
        let installed = Dictionary(
            list.map { ($0.identity, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        let latest = model.marketLatest
        let filtered = PluginMarket.filter(latest, query: query)
        let readOnly = state.isReadOnly
        List {
            ResolverUrlConflictSection(plugins: list, identities: Set(latest.map(\.pluginId)))
            ForEach(filtered.prefix(limit)) { entry in
                let current = installed[entry.pluginId]
                let action = PluginMarket.action(for: entry, installed: current)
                let pending = model.marketPending.contains(entry.pluginId)
                let canInstall = (action == .install || action == .update) && !pending && !readOnly
                NavigationLink {
                    MarketDetailPage(entry: entry)
                } label: {
                    MarketRow(
                        entry: entry,
                        installed: current,
                        installedYanked: current.flatMap { PluginMarket.installedVersionYanked(model.marketEntries, plugin: $0) },
                        pending: pending
                    )
                }
                .swipeActions(edge: .trailing) {
                    if canInstall {
                        Button(MarketRow.title(action, pending: false)) { model.requestInstall(entry, installed: current) }
                            .tint(.accentColor)
                    }
                }
                .contextMenu {
                    if canInstall {
                        Button(MarketRow.title(action, pending: false), systemImage: "square.and.arrow.down") {
                            model.requestInstall(entry, installed: current)
                        }
                    }
                }
            }
            if filtered.count > limit {
                Button(L("marketShowMore", ["count": filtered.count - limit])) { limit += PluginMarket.pageSize }
                    .frame(maxWidth: .infinity)
            }
        }
        .overlay { overlay(latest: latest, filtered: filtered) }
        .navigationTitle(L("marketSectionTitle"))
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: L("marketSearchPlaceholder"))
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button(L("marketRefreshTooltip"), systemImage: FluxSymbol.retry) { Task { await model.loadMarket() } }
                    .disabled(readOnly || model.marketPhase == .loading)
            }
        }
        .onChange(of: query) { _, _ in limit = PluginMarket.pageSize }
        .onChange(of: model.popRequest) { _, _ in dismiss() }
        .task { await model.ensureMarketLoaded() }
        .fluxAnimation(.smooth, value: model.marketPhase)
    }

    @ViewBuilder
    private func overlay(latest: [MarketEntry], filtered: [MarketEntry]) -> some View {
        switch model.marketPhase {
        case .loading where latest.isEmpty:
            VStack(spacing: 12) {
                ProgressView()
                Text(L("pluginCommonLoading")).foregroundStyle(.secondary)
            }
        case let .failed(message) where latest.isEmpty:
            ContentUnavailableView {
                Label(L("marketLoadFailed", ["message": message]), systemImage: "exclamationmark.triangle")
            } actions: {
                Button(L("mobileRetry")) { Task { await model.loadMarket() } }
                    .buttonStyle(.borderedProminent)
            }
        case .loaded where latest.isEmpty:
            ContentUnavailableView(L("marketEmpty"), systemImage: "shippingbox")
        default:
            if !latest.isEmpty, filtered.isEmpty {
                ContentUnavailableView(L("marketSearchNoResult"), systemImage: FluxSymbol.search)
            }
        }
    }
}

private struct MarketRow: View {
    let entry: MarketEntry
    let installed: PluginDto?
    let installedYanked: String?
    let pending: Bool

    var body: some View {
        let action = PluginMarket.action(for: entry, installed: installed)
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(entry.displayName).font(.headline)
                Text(verbatim: "v\(entry.version)")
                    .font(.footnote.monospacedDigit())
                    .foregroundStyle(.secondary)
                if !entry.author.isEmpty {
                    Text(entry.author).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer(minLength: 0)
                if pending {
                    ProgressView().controlSize(.small)
                } else if let tone = Self.badgeTone(action) {
                    StatusBadge(text: Self.title(action, pending: false), tone: tone)
                }
            }
            if !entry.homepage.isEmpty {
                Text(entry.homepage).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
            }
            if !entry.description.isEmpty {
                Text(entry.description).font(.footnote).foregroundStyle(.secondary).lineLimit(2)
            }
            FlowLayout(spacing: 6) {
                if let key = PluginMarket.yankedLabelKey(entry.yanked) {
                    StatusBadge(text: L(key), tone: .failure, systemImage: "exclamationmark.octagon.fill")
                }
                if let installedYanked, let key = PluginMarket.yankedLabelKey(installedYanked) {
                    StatusBadge(text: L("pluginInstalledVersionYanked", ["label": L(key)]), tone: .failure, systemImage: "exclamationmark.octagon.fill")
                }
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    private static func badgeTone(_ action: MarketAction) -> BadgeTone? {
        switch action {
        case .install: nil
        case .update: .accent
        case .installed, .unavailable: .neutral
        }
    }

    static func title(_ action: MarketAction, pending: Bool) -> String {
        if pending { return L(action == .update ? "marketUpdatingButton" : "marketInstallingButton") }
        switch action {
        case .install: return L("marketInstallButton")
        case .update: return L("marketUpdateButton")
        case .installed: return L("marketInstalledButton")
        case .unavailable: return L("marketUnavailableButton")
        }
    }
}

/// 市场条目详情（撤回条目顶部红色横幅）；安装 / 更新在右上角工具栏。
struct MarketDetailPage: View {
    @Environment(HostStore.self) private var store
    @Environment(ExtensionsModel.self) private var model
    @State private var plugins = SectionMemo<[PluginDto]>(empty: [])
    let entry: MarketEntry

    var body: some View {
        let state = store.state
        let list = plugins.value(state.sections[HostSection.daemonPlugins])
        let installed = list.first { $0.identity == entry.pluginId }
        let action = PluginMarket.action(for: entry, installed: installed)
        let pending = model.marketPending.contains(entry.pluginId)
        Form {
            ResolverUrlConflictSection(plugins: list, identities: [entry.pluginId])
            PluginInfoSections(detail: PluginDetail(market: entry))
        }
        .navigationTitle(entry.displayName)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if action == .install || action == .update || pending {
                ToolbarItem(placement: .primaryAction) {
                    if pending {
                        ProgressView()
                    } else {
                        Button(MarketRow.title(action, pending: false)) {
                            model.requestInstall(entry, installed: installed)
                        }
                        .disabled(state.isReadOnly)
                    }
                }
            }
        }
    }
}
#endif
