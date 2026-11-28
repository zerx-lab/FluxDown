import FluxDomain
import FluxUI
import SwiftUI

/// 行样式（视图偏好）。
nonisolated struct RowStyle: Equatable {
    var density: Density
    var fields: Set<CardField>
}

/// 任务行 `TaskRow`（01-foundations §9.1 / 02-downloads §3.5）：
/// `HStack { KindIcon · VStack(任务名, 状态行, 分段图谱, 元信息) · ProgressRingButton }`。
///
/// 内容层，不上玻璃。点按文字区 → 详情（`onOpen`，多选模式下为 nil 让 `List` 接管勾选）；点按圆环 → 主动作且不改变选中。
/// 无障碍：文字区合并为一个元素（读法「文件名，状态，进度，元信息」），圆环独立；菜单动作映射为自定义动作。
struct DownloadRowView: View, Equatable {
    let item: TaskItem
    let style: RowStyle
    let isLocalHost: Bool
    /// 排队名次（1 起；非排队行 0）。由父视图按可见行从 `QueueOrder` 取：队首出队只让名次变化的可见行重绘，
    /// 任务行模型（`item`）不因名次变化而重建。
    var queuePosition = 0
    /// 多选模式下圆环让位给系统勾选。
    var showsRing = true
    var zoom: Namespace.ID?
    var onOpen: (() -> Void)?

    @Environment(TaskActions.self) private var actions
    @Environment(\.confirmTaskDelete) private var confirmDelete
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.fluxAccent) private var accent
    @Environment(AppearanceStore.self) private var appearance

    static func == (lhs: DownloadRowView, rhs: DownloadRowView) -> Bool {
        lhs.item == rhs.item && lhs.style == rhs.style && lhs.isLocalHost == rhs.isLocalHost
            && lhs.queuePosition == rhs.queuePosition
            && lhs.showsRing == rhs.showsRing && lhs.zoom == rhs.zoom && (lhs.onOpen == nil) == (rhs.onOpen == nil)
    }

    private var compact: Bool { style.density == .compact }

    var body: some View {
        let text = RowText.live(units: appearance.fileSizeUnits)
        let nowSec = Int64(Date().timeIntervalSince1970)
        let status = text.status(item, fields: style.fields, queuePosition: queuePosition)
        let percent = text.percent(item)
        let meta = compact ? nil : text.meta(item, fields: style.fields, nowSec: nowSec)
        let summary = text.summary(item, fields: style.fields, nowSec: nowSec, queuePosition: queuePosition)

        HStack(alignment: typeSize >= .accessibility1 ? .top : .center, spacing: compact ? 10 : 12) {
            tappable(
                HStack(alignment: typeSize >= .accessibility1 ? .top : .center, spacing: compact ? 10 : 12) {
                    KindIcon(
                        kind: item.kind,
                        size: compact ? 32 : 44,
                        dimmed: item.visual == .missing,
                        badge: compact ? .none : item.visual.badge
                    )
                    .zoomSource(item.id, in: zoom)
                    details(status: status, percent: percent, meta: meta)
                }
                .contentShape(.rect)
                .accessibilityElement(children: .combine)
                .accessibilityLabel(summary)
                .accessibilityAddTraits(onOpen != nil ? .isButton : [])
                .accessibilityActions { accessibilityMenu(text: text) }
            )
            if showsRing, let glyph = item.ring {
                ProgressRingButton(
                    progress: item.ringProgress,
                    glyph: glyph,
                    tint: item.visual.ringTint,
                    accessibilityLabel: text.ringAction(glyph, isLocalHost: isLocalHost) + ", " + item.task.fileName,
                    action: { actions.primary(item.task) }
                )
            }
        }
        .padding(.vertical, compact ? 2 : 4)
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private func tappable<Content: View>(_ content: Content) -> some View {
        if let onOpen {
            content.onTapGesture(perform: onOpen)
        } else {
            content
        }
    }

    private func details(status: StatusLine, percent: String?, meta: String?) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(item.task.fileName)
                .font(.fluxTaskName)
                .foregroundStyle(item.visual == .missing ? Color.secondary : Color.primary)
                .lineLimit(1)
                .truncationMode(.middle)
            HStack(spacing: 6) {
                if item.awaitingDecision {
                    // 点角标重新打开「文件已存在」对话框（点行其余部分仍进详情）。
                    Button { actions.openFileConflicts() } label: {
                        StatusBadge(text: status.text, tone: .warning, systemImage: FluxSymbol.warning)
                    }
                    .buttonStyle(.plain)
                } else {
                    Text(status.text)
                        .foregroundStyle(status.tone.color(accent: accent))
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
                if let percent {
                    Text(percent).foregroundStyle(.secondary)
                }
            }
            .font(.footnote)
            .monospacedDigit()
            if let bar = item.progressBar {
                SegmentMapView(
                    spans: bar.spans,
                    totalBytes: bar.totalBytes,
                    progress: bar.progress,
                    tone: bar.tone,
                    height: compact ? SegmentMapView.compactHeight : SegmentMapView.rowHeight
                )
                .padding(.vertical, 1)
            }
            if let meta {
                Text(meta)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                    .lineLimit(typeSize.isAccessibilitySize ? 2 : 1)
            }
        }
    }

    /// 滑动 / 长按菜单的 VoiceOver 等价动作（不需要滑动或长按）。
    @ViewBuilder
    private func accessibilityMenu(text: RowText) -> some View {
        let task = item.task
        if let glyph = item.ring, glyph != .preparing {
            Button(text.ringAction(glyph, isLocalHost: isLocalHost)) { actions.primary(task) }
        }
        if task.status == .downloading || task.status == .pending {
            Button(L(item.boosted ? "cancelBoost" : "boostDownload")) { actions.boost(task, boosted: item.boosted) }
        }
        Button(L("copyUrl")) { actions.copyLink(task) }
        Button(L("delete")) { confirmDelete?.confirm([task]) }
    }
}

extension View {
    /// 列表 → 详情的 zoom 共享元素源（仅 compact 栈使用）。
    @ViewBuilder
    func zoomSource(_ id: String, in namespace: Namespace.ID?) -> some View {
        if let namespace {
            matchedTransitionSource(id: id, in: namespace)
        } else {
            self
        }
    }
}

/// 首次快照到达前的占位行：文字走 `redacted`，图形用 `.quaternary` 填充（不填会按前景色画成纯黑块），
/// 整行 1.6s 一个周期呼吸（同 Android `FluxSkeletonRows`）；减弱动态效果下静止。
struct DownloadRowPlaceholder: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var dimmed = false

    var body: some View {
        HStack(spacing: 12) {
            RoundedRectangle(cornerRadius: 12, style: .continuous).fill(.quaternary).frame(width: 44, height: 44)
            VStack(alignment: .leading, spacing: 6) {
                Text(verbatim: "placeholder-file-name.zip").font(.fluxTaskName)
                Text(verbatim: "12.3 MB/s").font(.footnote)
                Capsule().fill(.quaternary).frame(height: 5)
            }
            Circle().fill(.quaternary).frame(width: 36, height: 36)
        }
        .padding(.vertical, 4)
        .redacted(reason: .placeholder)
        .opacity(dimmed ? 0.5 : 1)
        .onAppear {
            guard !reduceMotion else { return }
            withAnimation(.easeInOut(duration: 0.8).repeatForever(autoreverses: true)) { dimmed = true }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(L("mobileLoading"))
    }
}
