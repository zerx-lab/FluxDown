import FluxDomain
import FluxUI
import SwiftUI
#if FULL_FLAVOR

/// BT 设置 › 防吸血 › 封禁名单（移动端子集）：分层筛选 · 搜索 · 剩余时长 · 解除（单条 / 批量）· 加白。
/// 云层条目不可解除（daemon 拒绝），只能加白覆盖；批量解除自动跳过云层条目。
/// 数据来自 `daemon.bt.guard.entries.list`，分页追加；`btGuardChanged` 通知时按已载入条数重取。
struct BtGuardBansPage: View {
    @Environment(AppContainer.self) private var container
    @Environment(HostStore.self) private var store

    @State private var model = BtGuardBansStore()
    @State private var selecting = false
    @State private var selection = Set<Int64>()

    var body: some View {
        let readOnly = store.state.isReadOnly
        let removable = BtGuardRules.removableIDs(model.items, selected: selection)
        SettingsPage(title: L("btGuardTabBans"), showsReadOnlyBanner: true) {
            filterSection
            if selecting, !removable.isEmpty {
                Section {
                    Button(role: .destructive) {
                        release(removable)
                    } label: {
                        Label(
                            "\(L("btGuardReleaseSelected")) · \(L("btGuardSelectedCount", ["n": removable.count]))",
                            systemImage: FluxSymbol.delete
                        )
                    }
                    .disabled(readOnly || model.isBusy)
                }
            }
            entriesSection(readOnly: readOnly)
        }
        .searchable(text: $model.query, prompt: L("btGuardSearchPlaceholder"))
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(selecting ? L("mobileExitSelection") : L("mobileMenuSelect")) {
                    selecting.toggle()
                    selection = []
                }
                .disabled(model.items.isEmpty)
            }
        }
        // 筛选变化 → 清空并重取首页；搜索输入防抖。
        .task(id: model.filterKey) {
            selection = []
            await model.reset(container.session, debounce: !model.query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
        .modifier(BtGuardChangeObserver { Task { await model.reload(container.session) } })
        .onChange(of: model.items) { _, items in
            selection = BtGuardRules.pruneSelection(selection, items: items)
            if items.isEmpty { selecting = false }
        }
    }

    // MARK: 筛选

    private var filterSection: some View {
        Section {
            Picker(L("btGuardColLayer"), selection: $model.layer) {
                Text(L("btGuardLayerAll")).tag(String?.none)
                ForEach(BtGuardLayer.all, id: \.self) { layer in
                    Text(Self.label("btGuardLayer", layer)).tag(String?.some(layer))
                }
            }
            .pickerStyle(.menu)
        } footer: {
            if model.isLoaded {
                Text(L("btGuardEntriesTotal", ["total": model.total]))
            }
        }
    }

    // MARK: 列表

    @ViewBuilder
    private func entriesSection(readOnly: Bool) -> some View {
        if let error = model.error, model.items.isEmpty {
            Section {
                SettingsStatusLine(text: "\(L("btGuardLoadFailed")): \(ErrorText.describe(error))", tone: .failure)
                SettingsActionRow(title: L("btGuardReload"), systemImage: FluxSymbol.syncing) {
                    Task { await model.reset(container.session, debounce: false) }
                }
            }
        } else if !model.isLoaded {
            Section {
                HStack(spacing: 8) {
                    ProgressView()
                    Text(L("btGuardLoading")).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 44)
                .accessibilityElement(children: .combine)
            }
        } else if model.items.isEmpty {
            Section {
                Text(model.isFiltered ? L("btGuardEntriesFilterEmpty") : L("btGuardEntriesEmpty"))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 44)
            }
        } else {
            Section {
                ForEach(model.items) { entry in
                    BtGuardEntryRow(
                        entry: entry, selecting: selecting, selected: selection.contains(entry.id), readOnly: readOnly || model.isBusy,
                        toggle: { toggle(entry) }, release: { release([entry.id]) }, allow: { allow(entry) }
                    )
                }
                if model.hasMore {
                    Button {
                        Task { await model.loadMore(container.session) }
                    } label: {
                        HStack(spacing: 8) {
                            if model.isLoadingMore { ProgressView() }
                            Text(L("btGuardLoadMore", ["shown": model.items.count, "total": model.total]))
                        }
                        .frame(maxWidth: .infinity, minHeight: 44)
                    }
                    .disabled(model.isLoadingMore)
                }
            }
        }
    }

    private func toggle(_ entry: BtGuardEntry) {
        guard BtGuardRules.canRemove(entry) else { return }
        if !selection.insert(entry.id).inserted { selection.remove(entry.id) }
    }

    // MARK: 操作

    private func release(_ ids: [Int64]) {
        guard !ids.isEmpty else { return }
        Task {
            let outcome = await model.release(ids, session: container.session)
            selection = []
            if outcome.removed > 0 {
                container.toasts.show(text: L("btGuardReleased", ["n": outcome.removed]), tone: .success)
                FluxHaptic.success.play()
            }
            if let error = outcome.error {
                container.toasts.show(text: "\(L("btGuardReleaseFailed")): \(ErrorText.describe(error))", tone: .error)
                FluxHaptic.error.play()
            }
        }
    }

    private func allow(_ entry: BtGuardEntry) {
        Task {
            switch await model.allow(entry, session: container.session) {
            case .added:
                container.toasts.show(text: L("btGuardAllowed", ["cidr": entry.cidr]), tone: .success)
                FluxHaptic.success.play()
            case .busy:
                break
            case let .failed(error):
                container.toasts.show(text: "\(L("btGuardAddFailed")): \(ErrorText.describe(error))", tone: .error)
                FluxHaptic.error.play()
            }
        }
    }

    /// 取值 → 本地化文案；主机新增、本端还没有文案的取值显示原文。
    static func label(_ prefix: String, _ value: String) -> String {
        if let key = BtGuardRules.labelKey(prefix: prefix, value: value), L10n.shared.has(key) { return L(key) }
        return value
    }
}

// MARK: - 条目行

/// 一条过滤条目：地址（等宽、过长中间省略）· 层 / 动作 / 规则 · 剩余时长 · 原因与来源 · 云层不可解除提示。
/// 左滑解除、右滑加白，长按菜单同样提供；选择模式下整行切换选中（云层条目不可选）。
private struct BtGuardEntryRow: View {
    let entry: BtGuardEntry
    let selecting: Bool
    let selected: Bool
    let readOnly: Bool
    let toggle: () -> Void
    let release: () -> Void
    let allow: () -> Void

    var body: some View {
        let removable = BtGuardRules.canRemove(entry)
        let content = HStack(alignment: .top, spacing: 12) {
            if selecting {
                Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                    .font(.title3)
                    .foregroundStyle(selected ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                    .opacity(removable ? 1 : 0.3)
                    .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 4) {
                Text(entry.cidr)
                    .font(.body.monospaced())
                    .lineLimit(2)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Text(summary)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                if let note = BtGuardRules.entryNote(entry) {
                    Text(note)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                }
                if !removable {
                    Text(L("btGuardCloudEntryHint")).font(.caption).foregroundStyle(.tertiary)
                }
            }
            Spacer(minLength: 0)
        }
        .frame(minHeight: 44)
        .contentShape(.rect)

        Group {
            if selecting {
                Button(action: toggle) { content }
                    .buttonStyle(.plain)
                    .disabled(!removable)
                    .accessibilityAddTraits(selected ? .isSelected : [])
            } else {
                content.accessibilityElement(children: .combine)
            }
        }
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            if removable, !readOnly {
                Button(role: .destructive, action: release) { Label(L("btGuardRelease"), systemImage: FluxSymbol.delete) }
            }
        }
        .swipeActions(edge: .leading, allowsFullSwipe: false) {
            if BtGuardRules.canWhitelist(entry), !readOnly {
                Button(action: allow) { Label(L("btGuardAllowEntry"), systemImage: "checkmark.shield") }
                    .tint(.green)
            }
        }
        .contextMenu {
            if BtGuardRules.canWhitelist(entry) {
                Button(action: allow) { Label(L("btGuardAllowEntry"), systemImage: "checkmark.shield") }
                    .disabled(readOnly)
            }
            if removable {
                Button(role: .destructive, action: release) { Label(L("btGuardRelease"), systemImage: FluxSymbol.delete) }
                    .disabled(readOnly)
            }
        }
    }

    /// `手动 · 封禁 · 过量上传 · 剩余 3 天`。
    private var summary: String {
        var parts = [BtGuardBansPage.label("btGuardLayer", entry.layer), BtGuardBansPage.label("btGuardAction", entry.action)]
        if !entry.rule.isEmpty { parts.append(BtGuardBansPage.label("btGuardRule", entry.rule)) }
        parts.append(Self.remainingText(entry.remainingSecs))
        return parts.joined(separator: " · ")
    }

    /// 永久 / 剩余时长文案。
    static func remainingText(_ secs: Int64?) -> String {
        guard let parts = BtGuardRules.remainingParts(secs) else { return L("btGuardPermanent") }
        return L("btGuardRemaining", ["time": L(parts.unit.textKey, ["n": parts.n])])
    }
}
#endif
