import FluxDomain
import Foundation

/// 详情页的本地化文案映射（键与 Android `TaskDetailFormat.kt` / `TaskDetailTabs.kt` 一致；`L("字面量")` 便于 check-i18n 扫描）。
enum TaskDetailText {
    /// 状态词（英雄头与字段表共用）。
    static func statusWord(_ visual: TaskDetailVisual, queuePosition: Int) -> String {
        switch visual {
        case .downloading: L("statusDownloading")
        case .queued: L("subtitleQueued", ["pos": queuePosition])
        case .pending: L("statusPending")
        case .preparing: L("statusPreparing")
        case .verifying: L("statusVerifying")
        case .paused: L("statusPaused")
        case .failed: L("statusError")
        case .completed: L("statusCompleted")
        case .missing: L("statusFileMissing")
        case .seeding: L("statusSeeding")
        }
    }

    /// 队列名：主队列 / 稍后下载走 i18n，其余用队列自身名称。
    static func queueName(_ queue: TaskQueue?) -> String {
        guard let queue else { return L("mainQueue") }
        switch queue.queueId {
        case "", TaskQueue.main: return L("mainQueue")
        case TaskQueue.later: return L("downloadLater")
        default: return queue.name
        }
    }

    /// ETA / 时长文本：`<60s` 秒、`<1h` 分钟（向上取整）、其余 `h m`；nil = —。
    static func eta(_ seconds: Int64?) -> String {
        guard let seconds else { return Format.dash }
        if seconds < 60 { return L("etaSeconds", ["n": seconds]) }
        if seconds < 3600 { return L("etaMinutes", ["n": (seconds + 59) / 60]) }
        return L("etaHours", ["n": seconds / 3600]) + " " + L("etaMinutes", ["n": seconds % 3600 / 60])
    }

    /// ETA 的「数值 + 单位」形态（英雄大数字：数值大、单位小）。nil → 「—」无单位。
    static func etaMeasure(_ seconds: Int64?) -> Measure {
        guard let seconds else { return Measure(Format.dash, "") }
        if seconds < 60 { return Measure(String(seconds), unit(L("etaSeconds", ["n": ""]))) }
        if seconds < 3600 { return Measure(String((seconds + 59) / 60), unit(L("etaMinutes", ["n": ""]))) }
        let hours = unit(L("etaHours", ["n": ""]))
        let minutes = unit(L("etaMinutes", ["n": ""]))
        return Measure(String(seconds / 3600), "\(hours) \(seconds % 3600 / 60)\(minutes)")
    }

    /// 做种 / 耗时时长：`d h m` 三档，高位为 0 时省略。
    static func duration(_ totalSeconds: Int64) -> String {
        let parts = TaskDetailFormat.durationParts(totalSeconds)
        var out: [String] = []
        if parts.days > 0 { out.append("\(parts.days) \(L("timeUnitDays"))") }
        if parts.hours > 0 { out.append("\(parts.hours) \(L("timeUnitHours"))") }
        if parts.minutes > 0 || out.isEmpty { out.append("\(parts.minutes) \(L("timeUnitMinutes"))") }
        return out.joined(separator: " ")
    }

    /// 时长的「数值 + 单位」形态（英雄「耗时」）：只取最高两档以免撑破三栏。
    static func durationMeasure(_ totalSeconds: Int64?) -> Measure {
        guard let totalSeconds else { return Measure(Format.dash, "") }
        let parts = TaskDetailFormat.durationParts(totalSeconds)
        if parts.days > 0 {
            return Measure(String(parts.days), "\(L("timeUnitDays")) \(parts.hours) \(L("timeUnitHours"))")
        }
        if parts.hours > 0 {
            return Measure(String(parts.hours), "\(L("timeUnitHours")) \(parts.minutes) \(L("timeUnitMinutes"))")
        }
        if parts.minutes > 0 { return Measure(String(parts.minutes), L("timeUnitMinutes")) }
        return Measure(String(max(totalSeconds, 0)), unit(L("etaSeconds", ["n": ""])))
    }

    /// 做种状态词。
    static func seedingWord(_ status: SeedingStatus) -> String {
        switch status {
        case .seeding: L("seedingStatusSeeding")
        case .ratioReached: L("seedingStatusRatioReached")
        case .timeReached: L("seedingStatusTimeReached")
        case .userStopped: L("seedingStatusUserStopped")
        case .taskDeleted: L("seedingStatusDeleted")
        case .sessionReleased: L("seedingStatusSessionReleased")
        case .inactiveStop: L("seedingStatusInactiveReached")
        case .queuedForSlot: L("seedingStatusQueued")
        case .none, .unknown: L("seedingStatusNone")
        }
    }

    /// 任务链路 wire 标签（Auto 路由、站点规则、全局 / 任务级代理与 BT 会话出口）→ 文案；未知值原样返回。
    /// 整串精确匹配必须先于后缀拆分，否则 `rule:system`、`bt:all:manual` 会被误拆。
    static func routeLabel(_ route: String) -> String {
        switch route {
        case "rule:direct": return L("taskRouteRuleDirect")
        case "rule:system": return L("taskRouteRuleSystem")
        case "rule:manual": return L("taskRouteRuleManual")
        case "rule:custom": return L("taskRouteRuleCustom")
        case "global:direct": return L("taskRouteGlobalDirect")
        case "global:bypass": return L("taskRouteGlobalBypass")
        case "global:manual": return L("taskRouteGlobalManual")
        case "global:system": return L("taskRouteGlobalSystem")
        case "global:pac": return L("taskRouteGlobalPac")
        case "task:proxy": return L("taskRouteTaskProxy")
        case "task:direct": return L("taskRouteTaskDirect")
        case "bt:direct": return L("taskRouteBtDirect")
        case "bt:trackers:manual": return L("taskRouteBtTrackersManual")
        case "bt:trackers:system": return L("taskRouteBtTrackersSystem")
        case "bt:all:manual": return L("taskRouteBtAllManual")
        case "bt:all:system": return L("taskRouteBtAllSystem")
        default: break
        }
        var base = route
        var via: String?
        if route.hasSuffix(":system") {
            base = String(route.dropLast(":system".count))
            via = L("taskRouteViaSystem")
        } else if route.hasSuffix(":manual") {
            base = String(route.dropLast(":manual".count))
            via = L("taskRouteViaManual")
        }
        let label = switch base {
        case "direct": L("taskRouteDirect")
        case "direct:sampled": L("taskRouteDirectSampled")
        case "direct:pinned": L("taskRouteDirectPinned")
        case "direct:failover": L("taskRouteDirectFailover")
        case "proxy:cached": L("taskRouteProxyCached")
        case "proxy:sampled": L("taskRouteProxySampled")
        case "proxy:failover": L("taskRouteProxyFailover")
        default: route
        }
        return via.map { "\(label) · \($0)" } ?? label
    }

    /// 来源行名称。
    static func sourceName(_ kind: TaskSourceComposition.Kind) -> String {
        switch kind {
        case .origin: L("sourceOrigin")
        case .cdn: L("sourceCdn")
        case .proxy: L("sourceProxy")
        case .nic: L("sourceNic")
        case .p2p: L("sourceP2p")
        }
    }

    private static func unit(_ template: String) -> String {
        template.trimmingCharacters(in: .whitespaces)
    }
}
