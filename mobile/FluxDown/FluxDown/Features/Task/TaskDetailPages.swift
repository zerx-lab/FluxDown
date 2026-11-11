import FluxDomain
import FluxUI
import SwiftUI

// D3 分页内容（02-downloads §5.3–5.7）。各页以若干 `Section` 直接嵌入详情 `List`。
// 「做种」页的「做种限制」(D4) 在 `SeedLimitsScreen.swift`（`daemon.task.get` 读取 / `daemon.task.setSeedLimits` 保存），
// 「日志」页在 `TaskActivityLogPage.swift`（`daemon.task.activity` + 实时 `taskActivityAdded`）；
// 哈希校验 / 任务代理 / TLS 等字段：`DownloadTask` 不携带，不凭空添加。

// MARK: - 常规

struct TaskDetailGeneralPage: View {
    let model: TaskDetailModel

    @Environment(AppContainer.self) private var container
    @Environment(TaskActions.self) private var actions

    private var task: DownloadTask { model.task }

    var body: some View {
        if task.downloadedBytes > 0 {
            TaskDetailSourcesSection(task: task)
        }
        Section {
            facts
        }
        #if FULL_FLAVOR
        if task.protocol == .bt {
            Section {
                NavigationLink {
                    BtFilesScreen(taskId: task.taskId)
                } label: {
                    Label(L("btTabFiles"), systemImage: "doc.on.doc")
                }
            }
        }
        #endif
        Section {
            Button {
                actions.copyLink(task)
            } label: {
                Label(L("copyUrl"), systemImage: FluxSymbol.copy)
                    .frame(maxWidth: .infinity)
            }
        }
    }

    @ViewBuilder private var facts: some View {
        let visual = model.visual
        let unfinished = !visual.isFinished
        let path = Self.joinPath(task.saveDir, task.fileName)

        KeyValueRow(
            key: L("infoStatus"),
            value: TaskDetailText.statusWord(visual, queuePosition: model.queuePosition),
            tone: visual.badgeTone == .neutral ? nil : visual.badgeTone
        )
        if task.totalBytes > 0 {
            KeyValueRow(key: L("infoSize"), value: Format.bytes(task.totalBytes).description)
        }
        if unfinished {
            let percent = task.progress.map { " · " + Format.percent($0) } ?? ""
            KeyValueRow(key: L("infoDownloaded"), value: Format.bytes(task.downloadedBytes).description + percent)
        }
        if model.isTransferring {
            KeyValueRow(key: L("infoSpeed"), value: Format.speedOrZero(model.speedDown).description, tone: .accent)
            KeyValueRow(key: L("infoRemaining"), value: TaskDetailText.eta(model.etaSeconds))
        }
        if task.createdAt > 0 {
            KeyValueRow(key: L("infoStartedAt"), value: TaskDetailFormat.dateTime(task.createdAt), monospaced: true)
        }
        if task.completedAt > 0 {
            KeyValueRow(key: L("infoCompletedAt"), value: TaskDetailFormat.dateTime(task.completedAt), monospaced: true)
        }
        KeyValueRow(
            key: L("infoPath"),
            value: path,
            monospaced: true,
            copyable: true,
            copyLabel: L("webCopy"),
            onCopy: { container.toasts.show(text: L("mobilePathCopied"), tone: .success, systemImage: FluxSymbol.copy) }
        )
        queueRow
        if !task.autoRoute.isEmpty {
            KeyValueRow(key: L("taskRoute"), value: TaskDetailText.routeLabel(task.autoRoute))
        }
        if task.status == .failed, !task.errorMessage.isEmpty {
            KeyValueRow(
                key: L("infoError"),
                value: TaskErrorMessage.displayText(task.errorMessage),
                monospaced: true,
                copyable: true,
                tone: .failure,
                copyLabel: L("webCopy"),
                onCopy: { container.toasts.show(text: L("detailErrorCopied"), tone: .success, systemImage: FluxSymbol.copy) }
            )
        }
        if let group = model.group {
            NavigationLink(value: DownloadsRoute.group(group.groupId)) {
                TaskDetailLinkLabel(key: L("mobileGroupLabel"), value: group.name, showsChevron: false)
            }
        }
    }

    /// 下载队列：未完成时点按 → 移动到队列（D5）。
    @ViewBuilder private var queueRow: some View {
        let name = TaskDetailText.queueName(model.queue)
        if task.status == .completed {
            KeyValueRow(key: L("taskQueueLabel"), value: name)
        } else {
            TaskDetailLinkRow(key: L("taskQueueLabel"), value: name) { actions.moveToQueue([task.taskId]) }
        }
    }

    static func joinPath(_ dir: String, _ name: String) -> String {
        var trimmed = Substring(dir)
        while trimmed.hasSuffix("/") { trimmed = trimmed.dropLast() }
        return "\(trimmed)/\(name)"
    }
}

/// 来源构成（环形图 + 图例 + 摘要）。
private struct TaskDetailSourcesSection: View {
    let task: DownloadTask

    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        let composition = TaskSourceComposition(task: task)
        Section {
            VStack(spacing: 16) {
                SourceDonut(
                    slices: composition.rows.map { DonutSlice(label: TaskDetailText.sourceName($0.kind), value: Double($0.bytes), color: Self.color($0.kind)) },
                    size: 150,
                    accessibilityLabel: L("detailSourcesTitle")
                ) {
                    VStack(spacing: 2) {
                        Text(L("infoDownloaded")).font(.caption).foregroundStyle(.secondary)
                        TaskStatValue(item: TaskStatItem(label: L("infoDownloaded"), measure: Format.bytes(composition.downloaded)), size: .tile)
                    }
                }
                VStack(spacing: 10) {
                    ForEach(composition.rows, id: \.kind) { row in
                        legend(row, composition: composition)
                    }
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 6)
        } header: {
            Text(L("detailSourcesTitle"))
        } footer: {
            Text(summary(composition))
        }
    }

    private func summary(_ composition: TaskSourceComposition) -> String {
        if composition.isP2P { return L("sourcesP2pHint") }
        if composition.accelShare > 0 { return L("sourcesAccelShare", ["percent": composition.accelPercentText]) }
        return L("sourcesNoAccel")
    }

    /// 图例行：色块 + 名称 + 百分比 + 字节数（AX 下名称独占一行）。
    private func legend(_ row: TaskSourceComposition.Row, composition: TaskSourceComposition) -> some View {
        let name = TaskDetailText.sourceName(row.kind)
        let percent = TaskSourceComposition.percentText(composition.share(of: row))
        let bytes = Format.bytes(row.bytes).description
        let swatch = RoundedRectangle(cornerRadius: 3, style: .continuous)
            .fill(Self.color(row.kind))
            .frame(width: 10, height: 10)
            .accessibilityHidden(true)
        return Group {
            if typeSize.isAccessibilitySize {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 8) {
                        swatch
                        Text(name).font(.subheadline)
                    }
                    Text("\(percent) · \(bytes)")
                        .font(.subheadline)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                HStack(spacing: 10) {
                    swatch
                    Text(name).font(.subheadline).lineLimit(1)
                    Spacer(minLength: 8)
                    Text(percent)
                        .font(.subheadline.weight(.semibold))
                        .monospacedDigit()
                        .frame(minWidth: 52, alignment: .trailing)
                    Text(bytes)
                        .font(.subheadline)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                        .frame(minWidth: 64, alignment: .trailing)
                }
            }
        }
        .accessibilityElement(children: .combine)
    }

    /// 来源色（02-downloads §5.3）：源站 accent · CDN 紫 · 代理 橙 · 多网卡 青 · P2P 绿；沿用调色板令牌。
    private static func color(_ kind: TaskSourceComposition.Kind) -> Color {
        switch kind {
        case .origin: Color.accentColor
        case .cdn: .fdKindVideo
        case .proxy: .fdKindImage
        case .nic: .fdKindEbook
        case .p2p: .fdStatusSeeding
        }
    }
}

// MARK: - 速度

struct TaskDetailSpeedPage: View {
    let model: TaskDetailModel

    @Environment(AppContainer.self) private var container
    @State private var showsSegments = false
    @State private var speed = TaskSpeedHistoryModel()

    private var task: DownloadTask { model.task }

    private struct RunKey: Equatable {
        let taskId: String
        let downloading: Bool
    }

    var body: some View {
        let history = speed.history?.taskId == task.taskId ? speed.history : nil
        let current = Format.speedOrZero(model.speedDown)
        let average = Format.speedOrZero(history?.averageBps ?? 0)
        let peak = Format.speedOrZero(history?.peakBps ?? 0)
        let activeTime = TaskDetailText.durationMeasure(history?.activeSecs ?? 0)

        Section {
            VStack(spacing: 10) {
                TaskStatTiles(items: [
                    TaskStatItem(label: L("infoSpeed"), measure: current, tint: model.isTransferring ? Color.accentColor : nil),
                    TaskStatItem(label: L("speedAverage"), measure: average),
                ])
                TaskStatTiles(items: [
                    TaskStatItem(label: L("speedPeak"), measure: peak),
                    TaskStatItem(label: L("speedActiveTime"), measure: activeTime),
                ])
            }
            .taskDetailBareRow()
        }

        Section {
            chart(history: history)
                .task(id: RunKey(taskId: task.taskId, downloading: model.isTransferring)) {
                    await speed.run(taskId: task.taskId, downloading: model.isTransferring, session: container.session)
                }
        }

        if model.isTransferring, let runtime = model.runtime {
            Section {
                KeyValueRow(key: L("infoRemaining"), value: TaskDetailText.eta(model.etaSeconds))
                if let transfers = runtime.activeTransfers {
                    KeyValueRow(key: L("detailActiveTransfers"), value: String(transfers), monospaced: true)
                }
                #if FULL_FLAVOR
                if task.protocol == .bt, let peers = runtime.connectedPeers {
                    KeyValueRow(key: L("detailConnectedPeers"), value: String(peers), monospaced: true)
                }
                #endif
            }
        }

        if task.protocol != .bt, let segments = model.runtime?.segments, segments.count >= 2 {
            TaskDetailSegmentsSection(model: model, segments: segments, expanded: $showsSegments)
        }
    }

    @ViewBuilder
    private func chart(history: TaskSpeedHistory?) -> some View {
        if let history, history.hasCurve {
            let end = TaskDetailText.duration(history.activeSecs)
            let bucket = Double(history.bucketSecs)
            let elapsed = history.elapsedSeconds
            VStack(spacing: 4) {
                SpeedAreaChart(
                    down: history.samples.map(Double.init),
                    valueFormat: { Format.speedOrZero(Int64($0)).description },
                    title: L("infoSpeed"),
                    downLabel: L("mobileActivityDownload"),
                    timeLabel: "\(L("speedAxisStart")) – \(end)",
                    xValueLabel: { TaskDetailText.duration(Int64($0 * bucket)) },
                    readoutTime: { clockDurationText(elapsed[$0]) }
                )
                .frame(height: 162)
                HStack {
                    Text(L("speedAxisStart"))
                    Spacer()
                    Text(end)
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            }
            .padding(.vertical, 6)
        } else {
            Text(L("speedChartEmpty"))
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, minHeight: 96, alignment: .center)
                .multilineTextAlignment(.center)
        }
    }
}

/// 分段并发：标题 + 方格热图 + 可展开的分段明细（Android `SegmentsCard`）。
private struct TaskDetailSegmentsSection: View {
    let model: TaskDetailModel
    let segments: [Segment]
    @Binding var expanded: Bool

    var body: some View {
        let live = model.isTransferring
        let activeCount = live ? segments.filter { $0.active == true }.count : 0
        Section {
            VStack(alignment: .leading, spacing: 12) {
                Text(L("segmentsDownloading", ["active": activeCount, "total": segments.count]))
                    .font(.subheadline)
                    .monospacedDigit()
                TaskSegmentGrid(segments: segments, live: live)
                    .accessibilityHidden(true)
            }
            .padding(.vertical, 4)
            Button {
                withAnimation(.fluxSmooth) { expanded.toggle() }
            } label: {
                Label(
                    L(expanded ? "mobileSegHideDetails" : "mobileSegShowDetails"),
                    systemImage: expanded ? "chevron.up" : "chevron.down"
                )
            }
            if expanded {
                ForEach(segments, id: \.index) { segment in
                    TaskSegmentRow(segment: segment, paused: model.task.status == .paused, live: live)
                }
            }
        }
    }
}

private func segmentLength(_ segment: Segment) -> Int64 {
    max(segment.endByte - segment.startByte + 1, 1)
}

private func segmentFraction(_ segment: Segment) -> Double {
    min(max(Double(segment.downloadedBytes) / Double(segmentLength(segment)), 0), 1)
}

/// 分段方格热图：每段一个 12pt 方格，已下载比例自左向右填充；活跃段用强调色。装饰性，对 VoiceOver 隐藏（明细行承载信息）。
private struct TaskSegmentGrid: View {
    let segments: [Segment]
    let live: Bool

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 12, maximum: 12), spacing: 4)], alignment: .leading, spacing: 4) {
            ForEach(segments, id: \.index) { segment in
                let fraction = segmentFraction(segment)
                let active = live && segment.active == true
                RoundedRectangle(cornerRadius: 3, style: .continuous)
                    .fill(Color.fdProgressTrack)
                    .overlay(alignment: .leading) {
                        if fraction > 0 {
                            Rectangle()
                                .fill(active ? Color.accentColor : Color.fdStatusCompleted)
                                .frame(width: max(12 * fraction, 3))
                        }
                    }
                    .clipShape(.rect(cornerRadius: 3, style: .continuous))
                    .frame(width: 12, height: 12)
            }
        }
    }
}

private struct TaskSegmentRow: View {
    let segment: Segment
    let paused: Bool
    let live: Bool

    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        let length = segmentLength(segment)
        let fraction = segmentFraction(segment)
        let done = segment.downloadedBytes >= length
        let active = live && segment.active == true
        let word: String
        let symbol: String
        if done {
            (word, symbol) = (L("mobileSegDone"), "checkmark.circle.fill")
        } else if paused {
            (word, symbol) = (L("statusPaused"), "pause.circle")
        } else if active {
            (word, symbol) = (L("mobileSegActive"), "arrow.down.circle.fill")
        } else {
            (word, symbol) = (L("mobileSegPending"), "circle")
        }
        let tint: Color = active ? .accentColor : .secondary
        let layout = typeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 4))
            : AnyLayout(HStackLayout(spacing: 10))
        return layout {
            Text("#\(segment.index + 1)")
                .font(.footnote)
                .monospacedDigit()
                .foregroundStyle(.secondary)
                .frame(minWidth: 30, alignment: .leading)
            Label(word, systemImage: symbol)
                .font(.footnote)
                .foregroundStyle(tint)
                .labelStyle(.titleAndIcon)
                .frame(minWidth: 84, alignment: .leading)
            ProgressView(value: fraction)
                .tint(tint)
            Text(Format.bytes(length).description)
                .font(.footnote)
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
        .frame(minHeight: 32)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("#\(segment.index + 1), \(word)")
        .accessibilityValue(Text(verbatim: "\(Format.percent(fraction)), \(Format.bytes(length).description)"))
    }
}

#if FULL_FLAVOR
// MARK: - 做种（仅 BT）

struct TaskDetailSeedingPage: View {
    let model: TaskDetailModel

    @Environment(AppContainer.self) private var container

    private var task: DownloadTask { model.task }

    var body: some View {
        let history = container.store.state.taskSpeedHistory[task.taskId]
        Section {
            KeyValueRow(
                key: L("seedingStatus"),
                value: TaskDetailText.seedingWord(task.seedingStatus),
                tone: task.seedingStatus == .seeding ? .success : nil
            )
            if !task.seedingMessage.isEmpty {
                KeyValueRow(key: L("infoError"), value: task.seedingMessage)
            }
            KeyValueRow(key: L("uploadedTotal"), value: Format.bytes(task.uploadedBytes).description, monospaced: true)
            KeyValueRow(
                key: L("seedRatio"),
                value: TaskDetailFormat.seedRatioText(uploaded: task.uploadedBytes, downloaded: task.downloadedBytes, total: task.totalBytes),
                monospaced: true
            )
            KeyValueRow(key: L("seedTime"), value: TaskDetailText.duration(task.seedingTimeSecs))
            KeyValueRow(key: L("mobileUploadChip"), value: Format.speedOrZero(model.speedUp).description, monospaced: true)
        }
        TaskSeedLimitsSection(taskId: task.taskId)
        if let history, history.count >= 2 {
            Section {
                SpeedAreaChart(
                    down: history.upSamples.map(Double.init),
                    valueFormat: { Format.speedOrZero(Int64($0)).description },
                    title: L("mobileUploadChip"),
                    downLabel: L("mobileActivityUpload"),
                    upLabel: L("mobileActivityUpload"),
                    timeLabel: "\(L("mobileSpeedAxisStart")) – \(L("mobileSpeedAxisNow"))"
                )
                .frame(height: 64)
                .padding(.vertical, 6)
            } header: {
                Text(L("mobileUploadChip"))
            }
        }
    }
}
#endif

// MARK: - 高级

struct TaskDetailAdvancedPage: View {
    let model: TaskDetailModel

    @Environment(AppContainer.self) private var container

    private var task: DownloadTask { model.task }

    var body: some View {
        Section {
            copyableRow(L("mobileTaskId"), task.taskId)
            KeyValueRow(key: L("mobileProtocol"), value: "\(TaskDetailFormat.protocolTag(task.protocol)) · \(TaskDetailFormat.siteLabel(task))")
            if !task.url.hasPrefix("torrent-file://") {
                copyableRow(L("infoUrl"), task.url)
            }
            if !task.originUrl.isEmpty, task.originUrl != task.url {
                copyableRow(L("mobileOriginUrl"), task.originUrl)
            }
            copyableRow(L("saveDir"), task.saveDir)
            if task.totalBytes > 0 {
                KeyValueRow(key: L("infoSize"), value: TaskDetailFormat.exactBytes(task.totalBytes), monospaced: true)
            }
            if task.fileMissing {
                KeyValueRow(key: L("statusFileMissing"), value: "✓", tone: .warning)
            }
            if let group = model.group {
                KeyValueRow(key: L("mobileGroupLabel"), value: group.name)
            }
            if !task.groupId.isEmpty {
                copyableRow(L("mobileGroupId"), task.groupId)
            }
        }
    }

    private func copyableRow(_ key: String, _ value: String) -> some View {
        KeyValueRow(
            key: key,
            value: value,
            monospaced: true,
            copyable: true,
            copyLabel: L("webCopy"),
            onCopy: { container.toasts.show(text: L("webCopied"), tone: .success, systemImage: FluxSymbol.copy) }
        )
    }
}
