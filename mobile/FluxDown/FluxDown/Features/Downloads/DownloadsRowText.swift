import FluxDomain
import FluxUI
import Foundation

/// 状态行语气（颜色映射在视图层：accent → `accent.text`，其余用加深文字色，状态不只靠颜色 —— 文案本身带状态词）。
nonisolated enum StatusTone: Equatable {
    case accent, secondary, failure, success, warning
}

nonisolated struct StatusLine: Equatable {
    let text: String
    let tone: StatusTone
}

/// 行内文案（同 Android `DownloadsRowText.kt` + 02-downloads §1.3 状态矩阵）。
/// `lookup` 可注入，单测用恒等查表避免依赖系统语言。
struct RowText {
    typealias Lookup = (String, [String: CustomStringConvertible]) -> String

    let units: FileSizeUnits
    let lookup: Lookup

    init(units: FileSizeUnits = .legacy, lookup: @escaping Lookup) {
        self.units = units
        self.lookup = lookup
    }

    static func live(units: FileSizeUnits) -> RowText {
        RowText(units: units) { key, args in args.isEmpty ? L(key) : L(key, args) }
    }

    private func t(_ key: String, _ args: [String: CustomStringConvertible] = [:]) -> String {
        lookup(key, args)
    }

    // MARK: 数值

    /// ETA 文案（§1.2）：<60s 秒 · <1h 分钟（向上取整）· 其余「h 小时 m 分钟」。
    func eta(_ seconds: Int64) -> String {
        if seconds < 60 { return t("etaSeconds", ["n": seconds]) }
        if seconds < 3600 { return t("etaMinutes", ["n": (seconds + 59) / 60]) }
        var hours = seconds / 3600
        var minutes = (seconds % 3600 + 59) / 60
        if minutes == 60 {
            hours += 1
            minutes = 0
        }
        let h = t("etaHours", ["n": hours])
        return minutes == 0 ? h : h + " " + t("etaMinutes", ["n": minutes])
    }

    func relative(_ unixSec: Int64, now: Int64) -> String {
        let d = max(now - unixSec, 0)
        if d < 60 { return t("mobileTimeJustNow") }
        if d < 3600 { return t("mobileTimeMinutesAgo", ["n": d / 60]) }
        if d < 86_400 { return t("mobileTimeHoursAgo", ["n": d / 3600]) }
        return t("mobileTimeDaysAgo", ["n": d / 86_400])
    }

    func queueLabel(_ queue: TaskQueue) -> String {
        switch queue.queueId {
        case "", TaskQueue.main: t("mainQueue")
        case TaskQueue.later: t("downloadLater")
        default: queue.name
        }
    }

    /// 协议标识（技术名词随 PC 同写法；插件走本地化）。
    static func protocolLabel(_ proto: TaskProtocol) -> String { TaskDetailFormat.protocolTag(proto) }

    /// 进度百分比（§1.2）：向下取整；0 < p < 1% → `<1%`；总大小未知不显示。
    /// 仅在「有进度意义」的状态显示（完成 / 做种 / 文件已删除 / 准备中不显示）。
    func percent(_ item: TaskItem) -> String? {
        guard let p = item.task.progress else { return nil }
        switch item.visual {
        case .downloading, .paused, .failed, .verifying:
            break
        case .queued, .pending:
            guard item.task.downloadedBytes > 0 else { return nil }
        case .preparing, .seeding, .missing, .completed:
            return nil
        }
        let whole = Int((p * 100).rounded(.down))
        return p > 0 && whole < 1 ? "<1%" : "\(whole)%"
    }

    // MARK: 状态行（§1.3，13pt monospacedDigit）

    /// `queuePosition`：排队名次（1 起），只对排队行有意义，由渲染方按可见行取，不随行模型缓存。
    func status(_ item: TaskItem, fields: Set<CardField>, queuePosition: Int = 0) -> StatusLine {
        let task = item.task
        if item.awaitingDecision { return StatusLine(text: t("fileConflictPending"), tone: .warning) }
        switch item.visual {
        case .downloading:
            var parts: [String] = []
            if fields.contains(.speed), item.speedDown > 0, let speed = Format.speed(item.speedDown, style: units) {
                parts.append(speed.description)
            }
            if fields.contains(.eta), let seconds = item.eta {
                parts.append(t("groupEtaRemaining", ["eta": eta(seconds)]))
            }
            return StatusLine(text: parts.isEmpty ? t("statusDownloading") : parts.joined(separator: " · "), tone: .accent)
        case .preparing:
            return StatusLine(text: t("subtitlePreparing"), tone: .secondary)
        case .verifying:
            return StatusLine(text: t("statusVerifying"), tone: .secondary)
        case .queued:
            return StatusLine(text: t("subtitleQueued", ["pos": queuePosition]), tone: .secondary)
        case .pending:
            return StatusLine(text: t("statusPending"), tone: .secondary)
        case .paused:
            return StatusLine(text: t("statusPaused"), tone: .secondary)
        case .failed:
            let base = t("statusError")
            if let line = Self.firstLine(TaskErrorMessage.displayText(task.errorMessage, localize: { t($0) })) {
                return StatusLine(text: base + " · " + line, tone: .failure)
            }
            return StatusLine(text: base, tone: .failure)
        case .completed:
            return StatusLine(text: t("statusCompleted"), tone: .secondary)
        case .seeding:
            var text = t(item.task.seedingStatus == .queuedForSlot ? "seedingStatusQueued" : "statusSeeding")
            if item.speedUp > 0, let up = Format.speed(item.speedUp, style: units) { text += " · ↑ " + up.description }
            return StatusLine(text: text, tone: .success)
        case .missing:
            return StatusLine(text: t("statusFileMissing"), tone: .warning)
        }
    }

    static func firstLine(_ text: String) -> String? {
        text.split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty }
    }

    // MARK: 元信息行（§1.3 末注；舒适密度，12pt）

    /// 已下 / 总量 · 活跃连接 · BT 节点 + 用户勾选的 协议 / 来源 / 队列 / 创建时间；空 → nil。
    func meta(_ item: TaskItem, fields: Set<CardField>, nowSec: Int64) -> String? {
        let task = item.task
        let total = task.totalBytes
        let showSize = fields.contains(.size)
        let totalSize = total > 0 ? Format.bytes(total, style: units).description : nil
        var parts: [String] = []

        switch item.visual {
        case .downloading:
            if showSize { parts.append(doneOfTotal(task)) }
            if let n = item.activeTransfers, n > 0 { parts.append(t("mobileActiveTransfersCount", ["n": n])) }
            if task.protocol == .bt, let n = item.connectedPeers, n > 0 { parts.append(t("mobilePeersCount", ["n": n])) }
            if fields.contains(.speed), item.speedUp > 0, let up = Format.speed(item.speedUp, style: units) { parts.append("↑ " + up.description) }
        case .paused, .failed, .verifying:
            if showSize, total > 0 || task.downloadedBytes > 0 { parts.append(doneOfTotal(task)) }
        case .queued, .pending, .missing, .seeding:
            if showSize, let totalSize { parts.append(totalSize) }
        case .preparing:
            break
        case .completed:
            if showSize, let totalSize { parts.append(totalSize) }
            if !fields.contains(.created), task.completedAt > 0 { parts.append(relative(task.completedAt, now: nowSec)) }
        }

        if fields.contains(.protocol) { parts.append(Self.protocolLabel(task.protocol)) }
        if fields.contains(.site), !item.site.isEmpty { parts.append(item.site) }
        if fields.contains(.queue), let queue = item.queue { parts.append(queueLabel(queue)) }
        if fields.contains(.created), task.createdAt > 0 { parts.append(relative(task.createdAt, now: nowSec)) }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private func doneOfTotal(_ task: DownloadTask) -> String {
        task.totalBytes > 0
            ? Format.bytes(task.downloadedBytes, style: units).description + " / " + Format.bytes(task.totalBytes, style: units).description
            : Format.bytes(task.downloadedBytes, style: units).description
    }

    // MARK: 圆环 / 无障碍

    /// 圆环按钮的动作词（`accessibilityLabel` 前半，后接文件名）。
    func ringAction(_ glyph: RingGlyph, isLocalHost: Bool) -> String {
        switch glyph {
        case .pause, .preparing: t("pause")
        case .resume: t("resume")
        case .retry: t("mobileRetry")
        case .redownload: t("redownloadTask")
        case .open: isLocalHost ? t("mobileOpenFile") : t("mobileTaskDetail")
        }
    }

    /// 整行朗读：「{文件名}，{状态}，{进度}，{元信息}」。
    func summary(_ item: TaskItem, fields: Set<CardField>, nowSec: Int64, queuePosition: Int = 0) -> String {
        [
            item.task.fileName,
            status(item, fields: fields, queuePosition: queuePosition).text,
            percent(item),
            meta(item, fields: fields, nowSec: nowSec),
        ]
        .compactMap { $0 }
        .joined(separator: ", ")
    }
}
