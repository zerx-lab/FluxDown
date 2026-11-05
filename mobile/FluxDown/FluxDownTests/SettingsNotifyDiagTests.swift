import FluxDomain
import Foundation
import Testing
import UIKit
import UserNotifications
@testable import FluxDown

// MARK: - 完成 / 失败检测

@MainActor
struct NotificationTrackerTests {
    private static func task(
        _ id: String, _ status: TaskStatus, error: String = "", createdAt: Int64 = 0, completedAt: Int64 = 0
    ) -> DownloadTask {
        DownloadTask(
            taskId: id, url: "https://example.com/\(id)", fileName: "\(id).bin", status: status,
            errorMessage: error, createdAt: createdAt, completedAt: completedAt
        )
    }

    private static func selection(_ id: String) -> SelectionRequest {
        SelectionRequest(requestId: id, taskId: "t-\(id)", kind: .bt([], saveDir: ""), defaultChoice: .cancelled, deadlineUnixMs: 0)
    }

    private static let now: Int64 = 1_760_000_000

    private func ingest(
        _ tracker: inout NotificationTracker, host: String = "h", live: Bool = true,
        _ tasks: [DownloadTask], selections: [SelectionRequest] = []
    ) -> NotificationDelta {
        tracker.ingest(hostID: host, isLive: live, tasks: tasks, selections: selections, now: Self.now)
    }

    @Test func firstLiveSnapshotSeedsSilently() {
        var tracker = NotificationTracker()
        let delta = ingest(&tracker, [Self.task("a", .completed), Self.task("b", .failed, error: "boom"), Self.task("c", .downloading)])
        #expect(delta.isEmpty)
        // 播种后同一状态不会再触发。
        #expect(ingest(&tracker, [Self.task("a", .completed), Self.task("b", .failed, error: "boom"), Self.task("c", .downloading)]).isEmpty)
    }

    @Test func notConnectedSnapshotDoesNotSeed() {
        var tracker = NotificationTracker()
        // 连接中的空表不能当作基线，否则首个真实快照里的已完成任务会被当成新完成。
        #expect(ingest(&tracker, live: false, []).isEmpty)
        #expect(ingest(&tracker, [Self.task("a", .completed)]).isEmpty)
    }

    @Test func completionAndFailureTransitionsNotifyOnce() {
        var tracker = NotificationTracker()
        _ = ingest(&tracker, [Self.task("a", .downloading), Self.task("b", .downloading)])
        let delta = ingest(&tracker, [Self.task("a", .completed), Self.task("b", .failed, error: "timeout")])
        #expect(delta.events == [
            NotificationEvent(kind: .completed, taskId: "a", fileName: "a.bin", errorMessage: ""),
            NotificationEvent(kind: .failed, taskId: "b", fileName: "b.bin", errorMessage: "timeout"),
        ])
        #expect(ingest(&tracker, [Self.task("a", .completed), Self.task("b", .failed, error: "timeout")]).isEmpty)
    }

    @Test func redownloadNotifiesAgain() {
        var tracker = NotificationTracker()
        _ = ingest(&tracker, [Self.task("a", .completed)])
        #expect(ingest(&tracker, [Self.task("a", .pending)]).isEmpty)
        #expect(ingest(&tracker, [Self.task("a", .completed)]).events.map(\.taskId) == ["a"])
    }

    @Test func deletedMarkerIsIgnored() {
        var tracker = NotificationTracker()
        _ = ingest(&tracker, [Self.task("a", .downloading)])
        #expect(ingest(&tracker, [Self.task("a", .failed, error: "deleted")]).isEmpty)
    }

    @Test func hostChangeAndReconnectReseedSilently() {
        var tracker = NotificationTracker()
        _ = ingest(&tracker, host: "one", [Self.task("a", .downloading)])
        // 换主机：新主机里已完成的任务不通知。
        #expect(ingest(&tracker, host: "two", [Self.task("a", .completed)]).isEmpty)
        #expect(ingest(&tracker, host: "two", [Self.task("a", .completed)]).isEmpty)
        // 断线重连后整表重新同步：断线期间完成的任务不通知。
        _ = ingest(&tracker, host: "two", [Self.task("b", .downloading)])
        #expect(ingest(&tracker, host: "two", live: false, []).isEmpty)
        #expect(ingest(&tracker, host: "two", [Self.task("b", .completed)]).isEmpty)
    }

    @Test func unseenTerminalTasksNotifyOnlyWhenFresh() {
        var tracker = NotificationTracker()
        _ = ingest(&tracker, [Self.task("seed", .downloading)])
        let fresh = Self.task("quick", .completed, completedAt: Self.now - 5)
        let old = Self.task("old", .completed, completedAt: Self.now - 3600)
        let unknownTime = Self.task("none", .completed)
        let failedFresh = Self.task("bad", .failed, error: "x", createdAt: Self.now - 2)
        let failedOld = Self.task("badOld", .failed, error: "x", createdAt: Self.now - 9999)
        let delta = ingest(&tracker, [Self.task("seed", .downloading), fresh, old, unknownTime, failedFresh, failedOld])
        #expect(delta.events.map(\.taskId) == ["quick", "bad"])
    }

    @Test func removedTasksAreForgotten() {
        var tracker = NotificationTracker()
        _ = ingest(&tracker, [Self.task("a", .completed), Self.task("b", .downloading)])
        _ = ingest(&tracker, [Self.task("b", .downloading)])
        // `a` 被删除后同 id 回来（status 已是 completed）按新任务处理：不新鲜 → 不通知。
        #expect(ingest(&tracker, [Self.task("a", .completed), Self.task("b", .downloading)]).isEmpty)
    }

    @Test func selectionRequestsAreTrackedAndResolved() {
        var tracker = NotificationTracker()
        // 播种时已挂起的请求不通知。
        #expect(ingest(&tracker, [], selections: [Self.selection("old")]).isEmpty)
        let gained = ingest(&tracker, [], selections: [Self.selection("old"), Self.selection("new")])
        #expect(gained.newSelections == ["new"])
        #expect(gained.resolvedSelections.isEmpty)
        let resolved = ingest(&tracker, [], selections: [Self.selection("new")])
        #expect(resolved.newSelections.isEmpty)
        #expect(resolved.resolvedSelections == ["old"])
    }
}

// MARK: - 合批 / 内容 / 点击意图

@MainActor
struct NotificationPlanTests {
    private func event(_ id: String, _ kind: NotificationEvent.Kind = .completed) -> NotificationEvent {
        NotificationEvent(kind: kind, taskId: id, fileName: "\(id).bin", errorMessage: kind == .failed ? "err" : "")
    }

    @Test func fewEventsStayIndividual() {
        let items = NotificationBatch.plan([event("a"), event("b")])
        #expect(items == [.single(event("a")), .single(event("b"))])
    }

    @Test func threeCompletionsBecomeOneSummary() {
        let items = NotificationBatch.plan([event("a"), event("b"), event("c"), event("d")])
        #expect(items == [.summary(kind: .completed, count: 4, names: ["a.bin", "b.bin", "c.bin"])])
    }

    @Test func completionsAndFailuresSummarizeIndependently() {
        let events = [event("a"), event("b"), event("c"), event("x", .failed), event("y", .failed)]
        let items = NotificationBatch.plan(events)
        #expect(items == [
            .summary(kind: .completed, count: 3, names: ["a.bin", "b.bin", "c.bin"]),
            .single(event("x", .failed)),
            .single(event("y", .failed)),
        ])
    }

    @Test func completedSpecCategoryFollowsActionsAndShareability() {
        let single = NotificationItem.single(event("a"))
        let none = NotificationSpec.make(single, hostID: "local", actions: false) { _ in true }
        #expect(none.category == NotificationIDs.completed)
        let open = NotificationSpec.make(single, hostID: "local", actions: true) { _ in false }
        #expect(open.category == NotificationIDs.completedOpen)
        let share = NotificationSpec.make(single, hostID: "local", actions: true) { _ in true }
        #expect(share.category == NotificationIDs.completedOpenShare)
        #expect(share.threadID == "fluxdown.local.a")
        #expect(share.userInfo == [NotificationIDs.keyTask: "a", NotificationIDs.keyHost: "local"])
        #expect(share.body == "a.bin")
    }

    @Test func failedSpecCarriesReasonAndNoActions() {
        let spec = NotificationSpec.make(.single(event("x", .failed)), hostID: "h1", actions: true) { _ in true }
        #expect(spec.category == NotificationIDs.failed)
        #expect(spec.body == "x.bin\nerr")
        #expect(spec.identifier == "fail.h1.x")
    }

    @Test func summarySpecHasNoTaskTarget() {
        let spec = NotificationSpec.make(
            .summary(kind: .completed, count: 5, names: ["a", "b", "c"]), hostID: "h", actions: true
        ) { _ in true }
        #expect(spec.userInfo == [NotificationIDs.keyHost: "h"])
        #expect(spec.category == NotificationIDs.completed)
        #expect(spec.body == "a, b, c…")
    }

    @Test func selectionSpecCarriesRequest() {
        let spec = NotificationSpec.selection(requestId: "r1", taskId: "t1", fileName: "", hostID: "h")
        #expect(spec.category == NotificationIDs.selection)
        #expect(spec.userInfo[NotificationIDs.keyRequest] == "r1")
        #expect(spec.identifier == "sel.h.r1")
    }

    @Test func fileConflictSpecNamesTheFileAndOffersRenameAsDefault() {
        let spec = NotificationSpec.fileConflict(requestId: "r1", taskId: "t1", fileName: "a.zip", hostID: "h")
        #expect(spec.category == NotificationIDs.fileConflict)
        #expect(spec.title == L("fileConflictTitle"))
        #expect(spec.body == L("mobileNotifFileConflictBody", ["name": "a.zip"]))
        #expect(spec.identifier == "sel.h.r1")
        #expect(spec.userInfo[NotificationIDs.keyRequest] == "r1")
        #expect(NotificationIntent.foregroundPresentation(category: NotificationIDs.fileConflict) == [.banner, .list, .sound])
    }

    @Test func intentParsing() {
        let task = [NotificationIDs.keyTask: "t", NotificationIDs.keyHost: "h"]
        let request = [NotificationIDs.keyRequest: "r", NotificationIDs.keyTask: "t", NotificationIDs.keyHost: "h"]
        let tap = UNNotificationDefaultActionIdentifier
        #expect(NotificationIntent.parse(actionIdentifier: tap, userInfo: task) == .openTask(taskId: "t", hostID: "h"))
        #expect(NotificationIntent.parse(actionIdentifier: NotificationIDs.actionOpen, userInfo: task) == .openTask(taskId: "t", hostID: "h"))
        #expect(NotificationIntent.parse(actionIdentifier: NotificationIDs.actionShare, userInfo: task) == .shareTask(taskId: "t", hostID: "h"))
        #expect(NotificationIntent.parse(actionIdentifier: tap, userInfo: request) == .openSelection(requestId: "r", hostID: "h"))
        #expect(NotificationIntent.parse(actionIdentifier: NotificationIDs.actionUseDefault, userInfo: request) == .resolveDefault(requestId: "r", hostID: "h"))
        #expect(NotificationIntent.parse(actionIdentifier: tap, userInfo: [NotificationIDs.keyHost: "h"]) == .openDownloads)
        #expect(NotificationIntent.parse(actionIdentifier: NotificationIDs.actionShare, userInfo: [:]) == nil)
        #expect(NotificationIntent.parse(actionIdentifier: UNNotificationDismissActionIdentifier, userInfo: task) == nil)
    }

    @Test func foregroundOnlyBannersFailuresAndSelections() {
        #expect(NotificationIntent.foregroundPresentation(category: NotificationIDs.completedOpen).isEmpty)
        #expect(NotificationIntent.foregroundPresentation(category: NotificationIDs.completed).isEmpty)
        #expect(NotificationIntent.foregroundPresentation(category: NotificationIDs.failed).contains(.banner))
        #expect(NotificationIntent.foregroundPresentation(category: NotificationIDs.selection).contains(.banner))
    }

    @Test func authorizationDelivery() {
        #expect(UNAuthorizationStatus.authorized.deliversNotifications)
        #expect(UNAuthorizationStatus.provisional.deliversNotifications)
        #expect(!UNAuthorizationStatus.denied.deliversNotifications)
        #expect(!UNAuthorizationStatus.notDetermined.deliversNotifications)
    }

    @Test func readoutReflectsPermissionAndSwitches() {
        func readout(_ status: UNAuthorizationStatus, _ complete: Bool, _ failure: Bool) -> String {
            NotifyPage.readout(authorization: status, notifyOnComplete: complete, notifyOnFailure: failure)
        }
        #expect(readout(.denied, true, true) == L("mobileNotifReadoutBlocked"))
        #expect(readout(.authorized, false, false) == L("mobileNotifReadoutOff"))
        #expect(readout(.authorized, true, false) == L("mobileNotifReadoutOn"))
        #expect(readout(.authorized, false, true) == L("mobileNotifReadoutFailuresOnly"))
        #expect(readout(.notDetermined, true, true) == L("mobileNotifReadoutNeedsPermission"))
        #expect(readout(.notDetermined, false, false) == L("mobileNotifReadoutOff"))
    }
}

// MARK: - 诊断

@MainActor
struct DiagnosticsLogicTests {
    @Test func camelMatchesWeb() {
        #expect(DiagnosticsLogic.camel("check_disk") == "CheckDisk")
        #expect(DiagnosticsLogic.camel("save_dir") == "SaveDir")
        #expect(DiagnosticsLogic.camel("refreshTrackers") == "RefreshTrackers")
        #expect(DiagnosticsLogic.checkKey("log_dir") == "doctorCheckLogDir")
        #expect(DiagnosticsLogic.hintKey("low_disk_space") == "doctorHintLowDiskSpace")
        #expect(DiagnosticsLogic.actionKey("fix_component") == "doctorActionFixComponent")
    }

    @Test func desktopOnlyChecksAreHidden() {
        let checks = ["nmh_binary", "url_protocol", "autostart", "notifications", "elevated_run", "save_dir", "daemon", "log_dir"]
            .map { DiagnosticCheckDto(id: $0) }
        #expect(DiagnosticsLogic.visibleChecks(checks).map(\.id) == ["save_dir", "daemon", "log_dir"])
    }

    @Test func repairHidesDesktopActionsAndUnlabelledOnes() {
        func check(_ action: String?) -> DiagnosticCheckDto {
            DiagnosticCheckDto(id: "x", repair: action.map { DiagnosticRepairParams(action: $0, target: "t") })
        }
        let labelled: (String) -> Bool = { _ in true }
        #expect(DiagnosticsLogic.repair(for: check("fix_component"), hasLabel: labelled)?.target == "t")
        #expect(DiagnosticsLogic.repair(for: check("enable_service"), hasLabel: labelled) != nil)
        for hidden in ["open_log_dir", "openLogDir", "fix_dir_access", "open_settings", "reregister", "register", "test_notification"] {
            #expect(DiagnosticsLogic.repair(for: check(hidden), hasLabel: labelled) == nil)
        }
        #expect(DiagnosticsLogic.repair(for: check(nil), hasLabel: labelled) == nil)
        #expect(DiagnosticsLogic.repair(for: check("fix_component"), hasLabel: { _ in false }) == nil)
    }

    @Test func repairErrorReasons() {
        #expect(DiagnosticsLogic.repairErrorKey(reason: "elevationCancelled") == "doctorRepairCancelled")
        #expect(DiagnosticsLogic.repairErrorKey(reason: "elevationUnavailable") == "doctorRepairUnavailable")
        #expect(DiagnosticsLogic.repairErrorKey(reason: "runningElevated") == "doctorRepairRunningElevated")
        #expect(DiagnosticsLogic.repairErrorKey(reason: "repairIncomplete") == "doctorRepairIncomplete")
        #expect(DiagnosticsLogic.repairErrorKey(reason: "repairNotApplicable") == "doctorRepairNotApplicable")
        #expect(DiagnosticsLogic.repairErrorKey(reason: "other") == nil)
        #expect(DiagnosticsLogic.repairErrorKey(reason: nil) == nil)
    }

    @Test func issueCountIsWarnPlusError() {
        let checks = [
            DiagnosticCheckDto(id: "a", level: .ok), DiagnosticCheckDto(id: "b", level: .warn),
            DiagnosticCheckDto(id: "c", level: .error), DiagnosticCheckDto(id: "d", level: .info),
        ]
        #expect(DiagnosticsLogic.issueCount(checks) == 2)
    }

    @Test func reportListsEveryCheckWithHint() {
        let input = DiagnosticsLogic.ReportInput(
            appVersion: "1.2 (3)", deviceDescription: "iOS 26.1 · iPhone", generatedAt: Date(timeIntervalSince1970: 0),
            device: [DiagnosticsLogic.ReportLine(level: .ok, label: "Notifications", target: "", detail: "Allowed", hint: "")],
            host: DiagnosticsLogic.HostPart(
                name: "NAS", appVersion: "1.4.0", platform: "linux-x86_64", dataDir: "/data", daemonConnected: true,
                daemonSummary: "1.4.0 · 3 tasks",
                lines: [
                    DiagnosticsLogic.ReportLine(level: .warn, label: "Default download folder", target: "/dl", detail: "low", hint: "free space"),
                ]
            )
        )
        let text = DiagnosticsLogic.renderReport(input)
        #expect(text.hasPrefix("FluxDown 1.2 (3) · iOS 26.1 · iPhone · 1970-01-01T00:00:00Z\n"))
        #expect(text.contains("[OK] Notifications: Allowed\n"))
        #expect(text.contains("[Download host: NAS]\n"))
        #expect(text.contains("agent data dir: /data\n"))
        #expect(text.contains("daemon connected: true\n"))
        #expect(text.contains("daemon: 1.4.0 · 3 tasks\n"))
        #expect(text.contains("[WARN] Default download folder (/dl): low\n  hint: free space\n"))
    }

    @Test func reportWithoutHostOmitsHostSection() {
        let input = DiagnosticsLogic.ReportInput(
            appVersion: "1", deviceDescription: "iOS", generatedAt: Date(timeIntervalSince1970: 0), device: [], host: nil
        )
        #expect(!DiagnosticsLogic.renderReport(input).contains("Download host"))
    }

    @Test func summaryResetsOnHostChange() {
        let summary = DiagnosticsSummary.shared
        summary.record(hostID: "a", issues: 2)
        summary.reset(ifHostIsNot: "a")
        #expect(summary.issues == 2)
        summary.reset(ifHostIsNot: "b")
        #expect(summary.issues == nil)
        #expect(summary.readout == nil)
        summary.record(hostID: "b", issues: 0)
        #expect(summary.readout == L("doctorAllHealthy"))
        summary.reset(ifHostIsNot: "other")
    }
}

// MARK: - 设备检查级别

@MainActor
struct MobileChecksTests {
    @Test func notificationLevels() {
        #expect(MobileChecks.notifications(.authorized).level == .ok)
        #expect(MobileChecks.notifications(.provisional).level == .info)
        #expect(MobileChecks.notifications(.notDetermined).level == .info)
        #expect(!MobileChecks.notifications(.notDetermined).canOpenSettings)
        let denied = MobileChecks.notifications(.denied)
        #expect(denied.level == .warn)
        #expect(denied.canOpenSettings)
        #expect(!denied.hint.isEmpty)
    }

    @Test func backgroundLevels() {
        #expect(MobileChecks.background(refresh: .available, lowPower: false, continueInBackground: true).level == .ok)
        #expect(MobileChecks.background(refresh: .available, lowPower: true, continueInBackground: true).level == .warn)
        #expect(MobileChecks.background(refresh: .denied, lowPower: false, continueInBackground: true).level == .warn)
        // 关闭「离开 App 时继续下载」时，后台刷新状态无关紧要，只提示 info。
        #expect(MobileChecks.background(refresh: .denied, lowPower: false, continueInBackground: false).level == .info)
        #expect(MobileChecks.background(refresh: .restricted, lowPower: false, continueInBackground: true).level == .warn)
    }

    @Test func storageLevels() {
        #expect(MobileChecks.storage(writable: false, freeBytes: 10 << 30).level == .error)
        #expect(MobileChecks.storage(writable: true, freeBytes: nil).level == .info)
        #expect(MobileChecks.storage(writable: true, freeBytes: 50 << 20).level == .error)
        #expect(MobileChecks.storage(writable: true, freeBytes: 500 << 20).level == .warn)
        #expect(MobileChecks.storage(writable: true, freeBytes: 5 << 30).level == .ok)
    }

    #if FULL_FLAVOR
    @Test func linkLevelsFollowInfoPlistDeclarations() {
        let all: Set<String> = ["magnet", "ed2k", "fluxdown"]
        #expect(MobileChecks.links(declaredSchemes: all, torrentDeclared: true, handlers: [:]).level == .ok)
        #expect(MobileChecks.links(declaredSchemes: all, torrentDeclared: false, handlers: [:]).level == .info)
        #expect(MobileChecks.links(declaredSchemes: ["magnet"], torrentDeclared: true, handlers: [:]).level == .warn)
        // 可查询的 scheme：声明了但没有任何 App 能打开 → 警告。
        #expect(MobileChecks.links(declaredSchemes: all, torrentDeclared: true, handlers: ["magnet": false]).level == .warn)
        #expect(MobileChecks.links(declaredSchemes: all, torrentDeclared: true, handlers: ["magnet": true, "ed2k": true]).level == .ok)
    }

    @Test func infoPlistParsing() {
        let info: [String: Any] = [
            "CFBundleURLTypes": [["CFBundleURLSchemes": ["Magnet", "ed2k"]], ["CFBundleURLSchemes": ["fluxdown"]]],
            "CFBundleDocumentTypes": [["CFBundleTypeExtensions": ["TORRENT"]]],
            "LSApplicationQueriesSchemes": ["magnet"],
        ]
        #expect(MobileChecks.declaredURLSchemes(in: info) == ["magnet", "ed2k", "fluxdown"])
        #expect(MobileChecks.declaresTorrentDocument(in: info))
        #expect(MobileChecks.queryableSchemes(in: info) == ["magnet"])
        let byType: [String: Any] = ["CFBundleDocumentTypes": [["LSItemContentTypes": ["org.bittorrent.torrent"]]]]
        #expect(MobileChecks.declaresTorrentDocument(in: byType))
        #expect(!MobileChecks.declaresTorrentDocument(in: [:]))
        #expect(MobileChecks.declaredURLSchemes(in: [:]).isEmpty)
    }
    #endif

    @Test func networkLevels() {
        func snapshot(
            _ reachability: MobileNetworkSnapshot.Reachability = .satisfied, _ interface: MobileNetworkSnapshot.Interface = .wifi,
            expensive: Bool = false, constrained: Bool = false
        ) -> MobileNetworkSnapshot {
            MobileNetworkSnapshot(
                reachability: reachability, interface: interface, isExpensive: expensive, isConstrained: constrained,
                supportsIPv4: true, supportsIPv6: true
            )
        }
        #expect(MobileChecks.network(nil).level == .info)
        #expect(MobileChecks.network(snapshot(.unsatisfied, .none)).level == .error)
        #expect(MobileChecks.network(snapshot()).level == .ok)
        #expect(MobileChecks.network(snapshot(.satisfied, .cellular, expensive: true)).level == .info)
        let lowData = MobileChecks.network(snapshot(.satisfied, .wifi, expensive: true, constrained: true))
        #expect(lowData.level == .warn)
        #expect(lowData.canOpenSettings)
        #expect(MobileChecks.network(snapshot()).detail.contains("IPv4 + IPv6"))
    }

    @Test func engineLevels() {
        let info = HostInfo(serviceName: "n", serviceVersion: "1.4.0", protocolVersion: 7, capabilities: [])
        #expect(MobileChecks.engine(connection: .live, info: info, failureText: nil).level == .ok)
        #expect(MobileChecks.engine(connection: .connecting, info: nil, failureText: nil).level == .info)
        #expect(MobileChecks.engine(connection: .stale, info: info, failureText: nil).level == .warn)
        let failed = MobileChecks.engine(connection: .failed(HostError(.unavailable)), info: nil, failureText: "no route")
        #expect(failed.level == .error)
        #expect(failed.detail == "no route")
    }

    @Test func worstLevelOrdering() {
        #expect(MobileChecks.worst([.ok, .info, .warn]) == .warn)
        #expect(MobileChecks.worst([.ok, .error, .warn]) == .error)
        #expect(MobileChecks.worst([.ok, .info]) == .info)
        #expect(MobileChecks.worst([]) == .ok)
        let checks = [
            MobileCheck(id: .storage, level: .warn, detail: ""), MobileCheck(id: .network, level: .error, detail: ""),
            MobileCheck(id: .engine, level: .ok, detail: ""),
        ]
        #expect(MobileChecks.issueCount(checks) == 2)
    }
}

// MARK: - 日志导出地址

@MainActor
struct LogExportURLTests {
    @Test func exportURLNormalizesEndpoint() {
        func url(_ endpoint: String) -> String? { LogExportHTTP.exportURL(endpoint: endpoint)?.absoluteString }
        #expect(url("http://nas.local:17800") == "http://nas.local:17800/api/web/logs/export")
        #expect(url("ws://nas.local:17800/rpc") == "http://nas.local:17800/api/web/logs/export")
        #expect(url("wss://dl.example.com/") == "https://dl.example.com/api/web/logs/export")
        #expect(url("https://example.com/fluxdown/rpc/") == "https://example.com/fluxdown/api/web/logs/export")
        #expect(url("192.168.1.5:17800") == "http://192.168.1.5:17800/api/web/logs/export")
        #expect(url("  https://h.example?x=1#frag ") == "https://h.example/api/web/logs/export")
        #expect(url("ftp://x") == nil)
        #expect(url("") == nil)
    }
}
