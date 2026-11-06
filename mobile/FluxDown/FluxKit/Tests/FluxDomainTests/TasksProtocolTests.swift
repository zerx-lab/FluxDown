import FluxDomain
import Foundation
import Testing

/// 任务 wire DTO 与详情页纯逻辑：做种限制三态映射（D4）、活动日志游标、`.torrent` 提交、插件失败判定。
struct TasksProtocolTests {
    // MARK: 做种限制

    @Test func seedLimitsDecodeFromTaskDto_missingFieldsMeanFollowGlobal() throws {
        let full = Data(#"""
        {"taskId":"t","url":"magnet:?xt=urn:btih:x","fileName":"f","status":3,
         "seedRatioLimitMilli":1500,"seedPostRatioLimitMilli":-1,"seedTimeLimitMinutes":90,
         "seedInactiveTimeLimitMinutes":-2,"seedUploadLimitBps":524288}
        """#.utf8)
        let limits = try ProtocolJSON.decode(TaskSeedLimits.self, from: full, what: "task")
        #expect(limits == TaskSeedLimits(seedRatioLimitMilli: 1500, seedPostRatioLimitMilli: -1, seedTimeLimitMinutes: 90, seedInactiveTimeLimitMinutes: -2, seedUploadLimitBps: 524_288))

        let old = try ProtocolJSON.decode(TaskSeedLimits.self, from: Data(#"{"taskId":"t"}"#.utf8), what: "task")
        #expect(old == TaskSeedLimits())
        #expect(old.seedRatioLimitMilli == SeedLimit.inherit && old.seedUploadLimitBps == 0)
    }

    /// 协议 17 起 `TaskDto` 默认值字段不再上线：只剩恒在字段的任务对象必须解码成「全部跟随全局」，
    /// 且单个字段缺省不影响其余字段（四个 `seed*Limit*` 缺省 = -2；上传限速缺省 = 0）。
    @Test func seedLimitsDecodeFromPagedTaskDtoWithDefaultFieldsOmitted() throws {
        let bare = Data(#"""
        {"taskId":"t","url":"https://e.com/f","fileName":"f","saveDir":"/d","status":1,
         "downloadedBytes":10,"totalBytes":100,"createdAt":1760000000}
        """#.utf8)
        let limits = try ProtocolJSON.decode(TaskSeedLimits.self, from: bare, what: "task")
        #expect(limits == TaskSeedLimits())
        #expect(limits.seedRatioLimitMilli == -2)
        #expect(limits.seedPostRatioLimitMilli == -2)
        #expect(limits.seedTimeLimitMinutes == -2)
        #expect(limits.seedInactiveTimeLimitMinutes == -2)
        #expect(limits.seedUploadLimitBps == 0)

        // 只有非默认值的字段会上线：其余仍按缺省回落。
        let partial = Data(#"""
        {"taskId":"t","url":"magnet:?xt=urn:btih:x","fileName":"f","saveDir":"/d","status":3,
         "downloadedBytes":0,"totalBytes":0,"createdAt":1,"seedTimeLimitMinutes":-1,"seedUploadLimitBps":2048}
        """#.utf8)
        let some = try ProtocolJSON.decode(TaskSeedLimits.self, from: partial, what: "task")
        #expect(some == TaskSeedLimits(seedTimeLimitMinutes: SeedLimit.unlimited, seedUploadLimitBps: 2048))
    }

    @Test func seedLimitField_modeMappingFromWire() {
        #expect(SeedLimitField(wire: -2, unit: .ratio).mode == .inherit)
        #expect(SeedLimitField(wire: -1, unit: .ratio).mode == .unlimited)
        // 0 引擎视同不限制。
        #expect(SeedLimitField(wire: 0, unit: .minutes).mode == .unlimited)
        let custom = SeedLimitField(wire: 1500, unit: .ratio)
        #expect(custom.mode == .custom && custom.text == "1.50")
        let minutes = SeedLimitField(wire: 90, unit: .minutes)
        #expect(minutes.mode == .custom && minutes.text == "90")
    }

    @Test func seedLimitField_unchangedKeepsOriginalWire_changesMapToSentinelsAndMilli() {
        // 未改动：保持原线上值（0 不被改写成 -1）。
        #expect(SeedLimitField(wire: 0, unit: .minutes).wireValue == 0)
        #expect(SeedLimitField(wire: -1, unit: .ratio).wireValue == -1)
        #expect(SeedLimitField(wire: 1234, unit: .ratio).wireValue == 1234)

        var field = SeedLimitField(wire: -2, unit: .ratio)
        field.mode = .unlimited
        #expect(field.wireValue == SeedLimit.unlimited)
        field.mode = .custom
        field.text = "2"
        #expect(field.wireValue == 2000)
        field.text = "1,25"
        #expect(field.wireValue == 1250)
        field.text = "1.2346"
        #expect(field.wireValue == 1235) // 千分比四舍五入
        field.text = "x"
        #expect(!field.isValid && field.wireValue == nil)
        field.text = ""
        #expect(!field.isValid)
        field.text = "-1"
        #expect(!field.isValid)
        field.mode = .inherit
        #expect(field.isValid && field.wireValue == SeedLimit.inherit)

        var minutes = SeedLimitField(wire: 60, unit: .minutes)
        minutes.text = "120"
        #expect(minutes.wireValue == 120)
        minutes.text = "1.5"
        #expect(minutes.wireValue == nil)
        minutes.text = "120"
        minutes.mode = .unlimited
        #expect(minutes.wireValue == -1)
        minutes.mode = .custom
        minutes.text = "60"
        // 改回原值 = 未改动。
        #expect(minutes.wireValue == 60 && !minutes.isModified)
    }

    @Test func seedUploadLimit_followGlobalCustomAndUnchanged() {
        #expect(SeedUploadLimitField(bps: 0).wireBps == 0)
        var field = SeedUploadLimitField(bps: 0)
        field.custom = true
        field.text = "512"
        #expect(field.wireBps == 524_288)
        field.text = ""
        #expect(!field.isValid && field.wireBps == nil)
        field.custom = false
        #expect(field.wireBps == 0)

        // 未改动：原 bps 不因 KB 取整而丢精度。
        let odd = SeedUploadLimitField(bps: 1_500_000)
        #expect(odd.text == "1465" && odd.wireBps == 1_500_000 && !odd.isModified)
        var edited = odd
        edited.text = "1466"
        #expect(edited.wireBps == 1466 * 1024 && edited.isModified)
        var cleared = odd
        cleared.custom = false
        #expect(cleared.wireBps == 0)
    }

    @Test func seedLimitsDraftBuildsSetSeedLimitsParams() throws {
        var draft = SeedLimitsDraft(limits: TaskSeedLimits(seedRatioLimitMilli: -2, seedPostRatioLimitMilli: -2, seedTimeLimitMinutes: 30, seedInactiveTimeLimitMinutes: -1, seedUploadLimitBps: 0))
        #expect(!draft.isModified && draft.isValid)
        draft.ratio.mode = .custom
        draft.ratio.text = "2.5"
        draft.upload.custom = true
        draft.upload.text = "100"
        #expect(draft.isModified)
        let params = try #require(draft.params(taskId: "t1"))
        #expect(params == SetSeedLimitsParams(taskId: "t1", ratioLimitMilli: 2500, postRatioLimitMilli: -2, seedTimeLimitMinutes: 30, inactiveTimeLimitMinutes: -1, uploadLimitBps: 102_400))
        let json = try ProtocolJSON.decode(JSONValue.self, from: ProtocolJSON.encode(params, what: "p"), what: "p")
        #expect(Set(json.objectValue?.keys.map { $0 } ?? []) == ["taskId", "ratioLimitMilli", "postRatioLimitMilli", "seedTimeLimitMinutes", "inactiveTimeLimitMinutes", "uploadLimitBps"])

        draft.time.mode = .custom
        draft.time.text = "abc"
        #expect(!draft.isValid && draft.params(taskId: "t1") == nil)
    }

    @Test func seedLimitGlobalsReadConfig_zeroMeansUnlimited() {
        let globals = SeedLimitGlobals(config: [
            "bt_seed_ratio_limit": "2.0", "bt_seed_post_ratio_limit": "0",
            "bt_seed_time_limit_minutes": "1440", "bt_seed_inactive_time_limit_minutes": "0",
        ])
        #expect(globals.ratio == 2.0 && globals.postRatio == nil)
        #expect(globals.timeMinutes == 1440 && globals.inactiveMinutes == nil)
        #expect(SeedLimitGlobals(config: [:]).ratio == nil)
    }

    // MARK: 活动日志

    private func entry(_ id: Int64, task: String = "t", kind: String = "status") -> TaskActivityDto {
        TaskActivityDto(id: id, taskId: task, timestampMs: id * 1000, kind: kind, message: "m\(id)", status: nil)
    }

    @Test func activityDtoAndPageDecodeServerShape() throws {
        let data = Data(#"""
        {"entries":[{"id":7,"taskId":"t","timestampMs":1700000000123,"kind":"status","message":"","status":1},
                    {"id":8,"taskId":"t","timestampMs":1700000001123,"kind":"error","message":"boom","status":null}],
         "hasMore":true,"oldestId":7,"newestId":8,"truncated":false}
        """#.utf8)
        let page = try ProtocolJSON.decode(TaskActivityPage.self, from: data, what: "page")
        #expect(page.entries.map(\.id) == [7, 8])
        #expect(page.entries[0].status == 1 && page.entries[1].status == nil)
        #expect(page.hasMore && page.oldestId == 7 && page.newestId == 8 && !page.truncated)
        let notice = try ProtocolJSON.decode(TaskActivityDto.self, from: Data(#"{"id":9,"taskId":"t","timestampMs":1,"kind":"retry","message":"x"}"#.utf8), what: "dto")
        #expect(notice.status == nil && notice.kind == "retry")
    }

    @Test func activityQueryOmitsNilCursors() throws {
        let data = try ProtocolJSON.encode(TaskActivityQuery(taskId: "t", beforeId: 5), what: "q")
        #expect(String(decoding: data, as: UTF8.self) == #"{"beforeId":5,"limit":0,"taskId":"t"}"#)
    }

    @Test func speedHistoryDecodesWireShape_nullMeansNoRecord_andDerivesAverage() throws {
        let json = #"{"taskId":"t","bucketSecs":2,"samples":[0,100,300],"activeSecs":6,"transferredBytes":1200,"peakBps":400}"#
        let history = try ProtocolJSON.decode(TaskSpeedHistory.self, from: Data(json.utf8), what: "h")
        #expect(history.bucketSecs == 2 && history.samples == [0, 100, 300])
        #expect(history.averageBps == 200 && history.hasCurve)
        let none = try ProtocolJSON.decode(TaskSpeedHistory?.self, from: Data("null".utf8), what: "h")
        #expect(none == nil)
        #expect(TaskSpeedHistory(taskId: "t").averageBps == 0)
        #expect(!TaskSpeedHistory(taskId: "t", samples: [0, 0], peakBps: 0).hasCurve)
        #expect(!TaskSpeedHistory(taskId: "t", samples: [5], peakBps: 5).hasCurve)
    }

    @Test func speedHistoryElapsedSeconds_bucketedClampedAndStrictlyIncreasing() {
        let full = TaskSpeedHistory(taskId: "t", bucketSecs: 2, samples: [1, 2, 3], activeSecs: 6)
        #expect(full.elapsedSeconds == [2, 4, 6])
        // 末尾不完整桶被 activeSecs 截断。
        let partial = TaskSpeedHistory(taskId: "t", bucketSecs: 5, samples: [1, 2, 3], activeSecs: 11)
        #expect(partial.elapsedSeconds == [5, 10, 11])
        // 截断后重复的点强制严格递增。
        let tiny = TaskSpeedHistory(taskId: "t", bucketSecs: 5, samples: [1, 2, 3], activeSecs: 3)
        #expect(tiny.elapsedSeconds == [3, 4, 5])
        #expect(TaskSpeedHistory(taskId: "t", bucketSecs: 0, samples: [1, 2], activeSecs: 9).elapsedSeconds == [1, 2])
    }

    @Test func clockDurationText_minutesAndHours() {
        #expect(clockDurationText(0) == "0:00" && clockDurationText(65) == "1:05")
        #expect(clockDurationText(3599) == "59:59" && clockDurationText(3661) == "1:01:01")
        #expect(clockDurationText(-4) == "0:00")
    }

    @Test func feed_latestThenLoadOlderThenLiveEvents() throws {
        var feed = TaskActivityFeed(taskId: "t")
        let firstTicket = feed.begin()
        let first = try #require(firstTicket)
        #expect(first.fetch == .latest && first.query.beforeId == nil && first.query.afterId == nil)
        let second = feed.begin() // 单飞
        #expect(second == nil)
        #expect(feed.isLoading)
        feed.finish(first, page: TaskActivityPage(entries: [entry(10), entry(11)], hasMore: true, oldestId: 10, newestId: 11))
        #expect(feed.loaded && feed.hasOlder && !feed.isLoading)
        #expect(feed.entries.map(\.id) == [10, 11])

        feed.loadOlder()
        let olderTicket = feed.begin()
        let older = try #require(olderTicket)
        #expect(older.fetch == .before && older.query.beforeId == 10)
        feed.finish(older, page: TaskActivityPage(entries: [entry(8), entry(9)], hasMore: false, oldestId: 8, newestId: 11))
        #expect(feed.entries.map(\.id) == [8, 9, 10, 11] && !feed.hasOlder)

        // 实时事件：新增返回 true；重复 / 别的任务 / 非法 id 忽略。
        let added = feed.add(entry(12))
        let duplicate = feed.add(entry(12))
        let foreign = feed.add(entry(13, task: "other"))
        let invalid = feed.add(entry(0))
        #expect(added && !duplicate && !foreign && !invalid)
        #expect(feed.entries.last?.id == 12)
    }

    @Test func feed_failureCanRetry_andReconnectCatchesUpFromCursor() throws {
        var feed = TaskActivityFeed(taskId: "t")
        let firstTicket = feed.begin()
        let first = try #require(firstTicket)
        feed.finish(first, page: nil)
        let blocked = feed.begin()
        #expect(feed.failed && blocked == nil)
        feed.retry()
        let againTicket = feed.begin()
        let again = try #require(againTicket)
        #expect(again.fetch == .latest)
        feed.finish(again, page: TaskActivityPage(entries: [entry(1), entry(2)], hasMore: false, oldestId: 1, newestId: 2))

        feed.suspend()
        feed.reconnect()
        let afterTicket = feed.begin()
        let after = try #require(afterTicket)
        #expect(after.fetch == .after && after.query.afterId == 2)
        feed.finish(after, page: TaskActivityPage(entries: [entry(3)], hasMore: false, oldestId: 1, newestId: 3))
        #expect(feed.entries.map(\.id) == [1, 2, 3])
        let idle = feed.begin()
        #expect(idle == nil)
    }

    @Test func feed_staleTicketsAreIgnoredAfterSuspend() throws {
        var feed = TaskActivityFeed(taskId: "t")
        let ticketValue = feed.begin()
        let ticket = try #require(ticketValue)
        feed.suspend()
        let accepted = feed.finish(ticket, page: TaskActivityPage(entries: [entry(1)], hasMore: false, oldestId: 1, newestId: 1))
        #expect(!accepted)
        #expect(feed.entries.isEmpty)
    }

    @Test func feed_liveEventDuringLatestFetchTriggersCatchUp_andJournalGapFlagged() throws {
        var feed = TaskActivityFeed(taskId: "t")
        let ticketValue = feed.begin()
        let ticket = try #require(ticketValue)
        feed.add(entry(6, kind: "journal_overflow"))
        feed.finish(ticket, page: TaskActivityPage(entries: [entry(4), entry(5)], hasMore: false, oldestId: 4, newestId: 5))
        #expect(feed.hasJournalGap)
        let catchUpTicket = feed.begin()
        let catchUp = try #require(catchUpTicket)
        #expect(catchUp.fetch == .after && catchUp.query.afterId == 5)
    }

    @Test func feed_rebuiltDatabaseResetsTheHistory() throws {
        var feed = TaskActivityFeed(taskId: "t")
        let latestTicket = feed.begin()
        let latest = try #require(latestTicket)
        feed.finish(latest, page: TaskActivityPage(entries: [entry(50)], hasMore: false, oldestId: 50, newestId: 50))
        feed.reconnect()
        let afterTicket = feed.begin()
        let after = try #require(afterTicket)
        feed.finish(after, page: TaskActivityPage(entries: [], hasMore: false, oldestId: nil, newestId: nil))
        #expect(feed.entries.isEmpty && !feed.loaded)
        let restart = feed.begin()
        #expect(restart?.fetch == .latest)
    }

    @Test func feed_truncationFlagFromRetentionRange() throws {
        var feed = TaskActivityFeed(taskId: "t")
        let ticketValue = feed.begin()
        let ticket = try #require(ticketValue)
        feed.finish(ticket, page: TaskActivityPage(entries: [entry(20)], hasMore: false, oldestId: 20, newestId: 20, truncated: true))
        let range = feed.retainedRange
        #expect(range.truncated && range.oldest == 20 && range.newest == 20)
    }

    // MARK: 创建任务 / .torrent

    @Test func torrentValidationAndCreateParams() throws {
        #expect(TorrentFile.validate(Data()) == .empty)
        #expect(TorrentFile.validate(Data("<html>login</html>".utf8)) == .notTorrent)
        #expect(TorrentFile.validate(Data("d8:announce3:foo".utf8)) == .notTorrent)
        let torrent = Data("d8:announce3:url4:infod6:lengthi1e4:name1:aee".utf8)
        #expect(TorrentFile.validate(torrent) == nil)
        #expect(TorrentFile.validate(Data(count: TorrentFile.maxBytes + 1)) == .tooLarge)
        #expect(TorrentFile.isTorrentFileName("Movie.TORRENT") && !TorrentFile.isTorrentFileName("a.txt"))

        let params = TorrentFile.createParams(data: torrent, saveDir: "/d", queueId: "later", startPaused: true)
        let json = try ProtocolJSON.decode(JSONValue.self, from: ProtocolJSON.encode(params, what: "p"), what: "p")
        let request = try #require(json["request"])
        #expect(request["url"]?.stringValue == "")
        #expect(request["torrentB64"]?.stringValue == torrent.base64EncodedString())
        #expect(request["startPaused"]?.boolValue == true)
        #expect(request["queueId"]?.stringValue == "later")
        #expect(request["saveDir"]?.stringValue == "/d")
        #expect(request["headers"] == nil) // nil 省略键
        #expect(json["torrentBlobId"] == nil)
        #expect(json["unattended"]?.boolValue == false)
    }

    @Test func createWireFromMobileSubset() {
        let wire = TaskCreateWire(CreateTaskRequest(url: "https://a/b", fileName: "b", saveDir: "/x", segments: 4, queueId: "q", headers: ["K": "V"]))
        #expect(wire.url == "https://a/b" && wire.segments == 4 && wire.headers == ["K": "V"])
        #expect(TaskCreateWire(CreateTaskRequest(url: "u")).headers == nil)
    }

    // MARK: 插件失败

    @Test func pluginFailureDetection() {
        #expect(PluginFailure.isIgnorable(status: .failed, errorMessage: "[插件] yt: boom", groupId: ""))
        #expect(!PluginFailure.isIgnorable(status: .failed, errorMessage: "connection reset", groupId: ""))
        #expect(!PluginFailure.isIgnorable(status: .paused, errorMessage: "[插件] yt: boom", groupId: ""))
        #expect(!PluginFailure.isIgnorable(status: .failed, errorMessage: "[插件] yt: boom", groupId: "g1"))
    }

    @Test func taskErrorMessageLocalization() {
        #expect(TaskErrorMessage.localizationKey(for: TaskErrorMessage.postOnlyEndpoint) == "taskErrorPostOnlyEndpoint")
        #expect(TaskErrorMessage.localizationKey(for: " " + TaskErrorMessage.postOnlyResume + "\n") == "taskErrorPostOnlyResume")
        #expect(TaskErrorMessage.localizationKey(for: "connection reset") == nil)
        #expect(TaskErrorMessage.displayText("connection reset", localize: { _ in "x" }) == "connection reset")
        #expect(TaskErrorMessage.displayText(TaskErrorMessage.postOnlyResume, localize: { "L:" + $0 }) == "L:taskErrorPostOnlyResume")
    }

    // MARK: BT 选片

    @Test func piecePickingParamsOmitNilFields() throws {
        let sequential = SetTaskPiecePickingParams(taskId: "t1", sequential: false)
        let sequentialJson = try ProtocolJSON.decode(JSONValue.self, from: ProtocolJSON.encode(sequential, what: "p"), what: "p")
        #expect(Set(sequentialJson.objectValue?.keys.map { $0 } ?? []) == ["sequential", "taskId"])
        #expect(sequentialJson["sequential"]?.boolValue == false)
        #expect(sequentialJson["firstLastPiecesFirst"] == nil)

        let firstLast = SetTaskPiecePickingParams(taskId: "t1", firstLastPiecesFirst: true)
        let firstLastJson = try ProtocolJSON.decode(JSONValue.self, from: ProtocolJSON.encode(firstLast, what: "p"), what: "p")
        #expect(Set(firstLastJson.objectValue?.keys.map { $0 } ?? []) == ["firstLastPiecesFirst", "taskId"])
        #expect(firstLastJson["sequential"] == nil)

        let roundTrip = try ProtocolJSON.decode(SetTaskPiecePickingParams.self, from: ProtocolJSON.encode(sequential, what: "p"), what: "p")
        #expect(roundTrip == sequential)
        #expect(roundTrip.firstLastPiecesFirst == nil)
    }

    @Test func piecePickingMenuShownOnlyForIncompleteBt() {
        let magnet = bt("m", .downloading, url: "magnet:?xt=urn:btih:abc")
        let uppercase = bt("u", .downloading, url: "MAGNET:?xt=urn:btih:ABC")
        let torrent = bt("f", .paused, url: "torrent-file://abc", sequential: true, firstLast: true)
        let http = bt("h", .downloading, url: "https://x/a.bin")
        let done = bt("d", .completed, url: "magnet:?xt=urn:btih:abc", sequential: true)
        let ed2k = bt("e", .downloading, url: "ed2k://|file|a|1|/")

        #expect(PiecePickingMenu.facts(tasks: [magnet]).shown)
        #expect(PiecePickingMenu.facts(tasks: [uppercase]).shown)
        #expect(PiecePickingMenu.facts(tasks: [torrent]).shown)
        #expect(!PiecePickingMenu.facts(tasks: [http]).shown)
        #expect(!PiecePickingMenu.facts(tasks: [done]).shown)
        #expect(!PiecePickingMenu.facts(tasks: [ed2k]).shown)
        #expect(!PiecePickingMenu.facts(tasks: [magnet, http]).shown)
        #expect(!PiecePickingMenu.facts(tasks: []).shown)
        #expect(PiecePickingMenu.facts(tasks: [http]).taskIds.isEmpty)
    }

    @Test func piecePickingOrderSelectsAllOrNeitherAndSetSkipsUnchanged() {
        let off = bt("a", .downloading)
        let alsoOff = bt("b", .pending)
        let sequential = bt("c", .paused, sequential: true)
        let sequentialEnds = bt("d", .failed, sequential: true, firstLast: true)

        let defaults = PiecePickingMenu.facts(tasks: [off, alsoOff])
        #expect(defaults.shown && defaults.order == .rarest && !defaults.firstLastOn)
        #expect(defaults.settingSequential(false).isEmpty)
        #expect(defaults.settingSequential(true) == [
            SetTaskPiecePickingParams(taskId: "a", sequential: true),
            SetTaskPiecePickingParams(taskId: "b", sequential: true),
        ])
        #expect(defaults.settingSequential(true).allSatisfy { $0.sequential == true && $0.firstLastPiecesFirst == nil })

        let allSequential = PiecePickingMenu.facts(tasks: [sequential, sequentialEnds])
        #expect(allSequential.order == .sequential && !allSequential.firstLastOn)
        #expect(allSequential.settingSequential(true).isEmpty)
        #expect(allSequential.settingSequential(false) == [
            SetTaskPiecePickingParams(taskId: "c", sequential: false),
            SetTaskPiecePickingParams(taskId: "d", sequential: false),
        ])
        #expect(allSequential.settingSequential(false).allSatisfy { $0.firstLastPiecesFirst == nil })

        let mixed = PiecePickingMenu.facts(tasks: [off, sequential, sequentialEnds])
        #expect(mixed.order == .mixed)
        #expect(mixed.settingSequential(true) == [
            SetTaskPiecePickingParams(taskId: "a", sequential: true),
        ])
        #expect(mixed.settingSequential(false) == [
            SetTaskPiecePickingParams(taskId: "c", sequential: false),
            SetTaskPiecePickingParams(taskId: "d", sequential: false),
        ])
        #expect(mixed.settingSequential(false).allSatisfy { $0.sequential == false && $0.firstLastPiecesFirst == nil })

        // 首尾勾选与顺序无关：混合未勾，点击把全部设为开，且只带该字段；全开则翻成关。
        let endsOn = bt("e", .downloading, firstLast: true)
        let endsMixed = PiecePickingMenu.facts(tasks: [off, endsOn])
        #expect(endsMixed.order == .rarest && !endsMixed.firstLastOn)
        #expect(endsMixed.togglingFirstLast() == [
            SetTaskPiecePickingParams(taskId: "a", firstLastPiecesFirst: true),
            SetTaskPiecePickingParams(taskId: "e", firstLastPiecesFirst: true),
        ])
        #expect(endsMixed.togglingFirstLast().allSatisfy { $0.sequential == nil })
        let endsAllOn = PiecePickingMenu.facts(tasks: [endsOn, bt("f", .paused, sequential: true, firstLast: true)])
        #expect(endsAllOn.order == .mixed && endsAllOn.firstLastOn)
        #expect(endsAllOn.togglingFirstLast().map(\.firstLastPiecesFirst) == [false, false])
        #expect(endsAllOn.togglingFirstLast().allSatisfy { $0.sequential == nil })

        let hidden = PiecePickingMenu.facts(tasks: [bt("h", .downloading, url: "https://x/a.bin")])
        #expect(hidden.order == .mixed)
        #expect(hidden.settingSequential(true).isEmpty && hidden.settingSequential(false).isEmpty)
        #expect(hidden.togglingFirstLast().isEmpty)
    }

    private func bt(
        _ id: String,
        _ status: TaskStatus,
        url: String = "magnet:?xt=urn:btih:1",
        sequential: Bool = false,
        firstLast: Bool = false
    ) -> DownloadTask {
        DownloadTask(
            taskId: id, url: url, fileName: id, status: status,
            btSequential: sequential, btFirstLastPiecesFirst: firstLast
        )
    }
}
