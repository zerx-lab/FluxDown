import FluxDomain
import Foundation
import Testing

/// BT 任务操作的 wire 镜像与纯逻辑（夹具照抄 `native/protocol/tests/protocol/bt_ops_wire.rs`）。
@MainActor
struct BtOpsTests {
    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try JSONDecoder().decode(type, from: Data(json.utf8))
    }

    private func object(_ value: some Encodable) throws -> NSDictionary {
        let data = try JSONEncoder().encode(value)
        return try #require(JSONSerialization.jsonObject(with: data) as? NSDictionary)
    }

    private func file(_ index: Int, _ priority: BtFilePriority, size: Int64 = 100, downloaded: Int64 = 0) -> BtTaskFile {
        BtTaskFile(index: index, path: "f\(index)", size: size, downloaded: downloaded, priority: priority.rawValue)
    }

    private func task(_ id: String, url: String = "magnet:?xt=urn:btih:aa", status: TaskStatus = .downloading, superSeeding: Bool = false) -> DownloadTask {
        DownloadTask(taskId: id, url: url, fileName: id, status: status, btSuperSeeding: superSeeding)
    }

    // MARK: wire

    @Test func filesDecodeTheRustFixtureIncludingSizesOver4GiB() throws {
        let files = try decode(BtFiles.self, #"""
        {"editable": true, "files": [{"index": 3, "path": "Season 1/E01.mkv", "size": 5000000000,
          "downloaded": 4294967296, "priority": 7}]}
        """#)
        #expect(files.editable)
        #expect(files.files == [BtTaskFile(index: 3, path: "Season 1/E01.mkv", size: 5_000_000_000, downloaded: 4_294_967_296, priority: 7)])
        #expect(files.files[0].level == .maximum)
        #expect(abs(files.files[0].progress - 0.8589934592) < 1e-9)
    }

    @Test func olderOrSparseRepliesFallBackToDefaults() throws {
        let empty = try decode(BtFiles.self, "{}")
        #expect(!empty.editable && empty.files.isEmpty)
        let bare = try decode(BtFiles.self, #"{"files":[{"index":0}]}"#)
        #expect(bare.files[0] == BtTaskFile(index: 0, path: "", size: 0, downloaded: 0, priority: 1))
        let idle = try decode(BtPortStatus.self, #"{"running":false}"#)
        #expect(idle == BtPortStatus())
    }

    @Test func paramsUseCamelCaseAndOmitUnsetFields() throws {
        let priorities = SetBtFilePrioritiesParams(taskId: "t1", priorities: [BtFilePriorityChange(index: 2, priority: .high)])
        #expect(try object(priorities) == ["taskId": "t1", "priorities": [["index": 2, "priority": 6]]] as NSDictionary)
        let limits = SetBtLimitsParams(taskId: "t1", downloadLimitBps: 0)
        #expect(try object(limits) == ["taskId": "t1", "downloadLimitBps": 0] as NSDictionary)
        #expect(try object(SetSuperSeedingParams(taskId: "t1", enabled: true)) == ["taskId": "t1", "enabled": true] as NSDictionary)
    }

    @Test func methodNamesMirrorRust() {
        #expect(HostMethod.daemonTaskBtFiles == "daemon.task.btFiles")
        #expect(HostMethod.daemonTaskSetFilePriorities == "daemon.task.setFilePriorities")
        #expect(HostMethod.daemonTaskRenameBtFile == "daemon.task.renameBtFile")
        #expect(HostMethod.daemonTaskRecheck == "daemon.task.recheck")
        #expect(HostMethod.daemonTaskMoveStorage == "daemon.task.moveStorage")
        #expect(HostMethod.daemonTaskSetBtLimits == "daemon.task.setBtLimits")
        #expect(HostMethod.daemonTaskSetSuperSeeding == "daemon.task.setSuperSeeding")
        #expect(HostMethod.daemonTaskReannounce == "daemon.task.reannounce")
        #expect(HostMethod.daemonBtPortStatus == "daemon.bt.portStatus")
    }

    // MARK: 优先级

    @Test func priorityLevelsMatchQBittorrentAndOddValuesAreBucketed() {
        #expect(BtFilePriority.allCases.map(\.rawValue) == [0, 1, 6, 7])
        #expect(BtFilePriority.allCases.map(\.titleKey) == [
            "btFilePrioritySkip", "btFilePriorityNormal", "btFilePriorityHigh", "btFilePriorityMaximum",
        ])
        #expect(BtFilePriority(wire: -1) == .skip)
        #expect(BtFilePriority(wire: 4) == .normal)
        #expect(BtFilePriority(wire: 255) == .maximum)
    }

    @Test func progressHandlesZeroSizeAndOverrun() {
        #expect(file(0, .normal, size: 0).progress == 1)
        #expect(file(0, .normal, size: 10, downloaded: 50).progress == 1)
        #expect(file(0, .normal, size: 10, downloaded: -5).progress == 0)
        #expect(file(0, .normal, size: 4, downloaded: 1).progress == 0.25)
    }

    @Test func planSendsOnlyRealChangesAndKeepsOneFile() {
        let files = [file(0, .normal), file(1, .skip), file(2, .high)]
        #expect(BtFilesRules.plan(files, indices: [1], to: .skip) == .unchanged)
        #expect(BtFilesRules.plan(files, indices: [0, 1], to: .high) == .send([
            BtFilePriorityChange(index: 0, priority: .high), BtFilePriorityChange(index: 1, priority: .high),
        ]))
        #expect(BtFilesRules.plan(files, indices: [0], to: .skip) == .send([BtFilePriorityChange(index: 0, priority: .skip)]))
        // 跳过最后一个要下载的文件会让任务无事可做。
        #expect(BtFilesRules.plan(files, indices: [0, 2], to: .skip) == .keepOne)
        #expect(BtFilesRules.plan([file(0, .normal)], indices: [0], to: .skip) == .keepOne)
        #expect(BtFilesRules.plan(files, indices: [9], to: .high) == .unchanged)
    }

    @Test func applyingChangesUpdatesOnlyTheNamedFiles() {
        let files = [file(0, .normal), file(1, .normal)]
        let next = BtFilesRules.applying([BtFilePriorityChange(index: 1, priority: .maximum)], to: files)
        #expect(next.map(\.level) == [.normal, .maximum])
    }

    @Test func sizeTextClampsDownloadedToSize() {
        let big = file(0, .normal, size: 5_000_000_000, downloaded: 6_000_000_000)
        let five = Format.bytes(5_000_000_000).description
        #expect(BtFilesRules.sizeText(big) == "\(five) / \(five)")
        #expect(BtFilesRules.sizeText(file(0, .normal, size: 0)) == "0 B / 0 B")
    }

    @Test func splitKeepsNameAndExtensionApartFromDirectory() {
        #expect(BtFilesRules.split("Season 1/E01.mkv") == ("E01.mkv", "Season 1"))
        #expect(BtFilesRules.split("a/b/c.tar.gz") == ("c.tar.gz", "a/b"))
        #expect(BtFilesRules.split("README") == ("README", ""))
        #expect(BtFilesRules.split("字幕/第 1 集  🎬.srt") == ("第 1 集  🎬.srt", "字幕"))
        #expect(BtFilesRules.split("dir/") == ("", "dir"))
    }

    // MARK: 任务限速

    @Test func limitsDraftSendsOnlyChangedFieldsInBytes() {
        var draft = BtTaskLimitsDraft(downloadLimitBps: 512 * 1024, uploadLimitBps: 0)
        #expect(draft.download == "512" && draft.upload == "0")
        #expect(draft.params(taskId: "t") == nil)
        draft.upload = "100"
        #expect(draft.params(taskId: "t") == SetBtLimitsParams(taskId: "t", uploadLimitBps: 102_400))
        draft.download = ""
        #expect(draft.params(taskId: "t") == SetBtLimitsParams(taskId: "t", downloadLimitBps: 0, uploadLimitBps: 102_400))
    }

    @Test func limitsDraftRejectsGarbageAndHuge() {
        var draft = BtTaskLimitsDraft(downloadLimitBps: 0, uploadLimitBps: 0)
        for bad in ["-1", "1.5", "abc", "１２", "\(BtTaskLimitsDraft.maxKiB + 1)"] {
            draft.download = bad
            #expect(!draft.isDownloadValid, "\(bad)")
            #expect(draft.params(taskId: "t") == nil)
        }
        draft.download = " 7 "
        #expect(draft.params(taskId: "t") == SetBtLimitsParams(taskId: "t", downloadLimitBps: 7 * 1024))
    }

    @Test func limitsDraftFloorsOddByteValuesWithoutRewritingThem() {
        let draft = BtTaskLimitsDraft(downloadLimitBps: 1500, uploadLimitBps: -3)
        #expect(draft.download == "1" && draft.upload == "0")
        #expect(draft.params(taskId: "t") == nil)
    }

    // MARK: 菜单

    @Test func menuFactsNeedAllBtTasks() {
        let mixed = BtTaskMenu.facts(tasks: [task("a"), task("b", url: "https://e.com/f")])
        #expect(mixed.isEmpty && mixed.recheckTaskIds.isEmpty && mixed.togglingSuperSeeding().isEmpty)
        #expect(BtTaskMenu.facts(tasks: []).isEmpty)

        let one = BtTaskMenu.facts(tasks: [task("a", url: "TORRENT-FILE:///x.torrent")])
        #expect(one.showsRecheck && one.showsSuperSeeding && one.limitsTask?.taskId == "a")
        #expect(one.recheckTaskIds == ["a"])
    }

    @Test func recheckIsHiddenForCompletedTasksButSuperSeedingStays() {
        let done = BtTaskMenu.facts(tasks: [task("a", status: .completed)])
        #expect(!done.showsRecheck && done.showsSuperSeeding && done.limitsTask != nil)
        let two = BtTaskMenu.facts(tasks: [task("a"), task("b", status: .completed)])
        #expect(!two.showsRecheck && two.limitsTask == nil)
    }

    @Test func superSeedingTogglesTowardsAllOnAndSkipsTasksAlreadyThere() {
        let mixed = BtTaskMenu.facts(tasks: [task("a", superSeeding: true), task("b")])
        #expect(!mixed.superSeedingOn)
        #expect(mixed.togglingSuperSeeding() == [SetSuperSeedingParams(taskId: "b", enabled: true)])
        let allOn = BtTaskMenu.facts(tasks: [task("a", superSeeding: true), task("b", superSeeding: true)])
        #expect(allOn.superSeedingOn)
        #expect(allOn.togglingSuperSeeding() == [
            SetSuperSeedingParams(taskId: "a", enabled: false), SetSuperSeedingParams(taskId: "b", enabled: false),
        ])
    }

    // MARK: 端口映射

    @Test func portSegmentsFollowGpui() throws {
        let status = try decode(BtPortStatus.self, #"""
        {"running": true, "listenPort": 6881, "mappings": [
          {"protocol": "tcp", "internalPort": 6881, "externalPort": 50123, "method": "natPmp",
           "externalIp": "203.0.113.7", "state": "mapped"},
          {"protocol": "udp", "internalPort": 6881, "state": "failed", "message": "  no   gateway\nanswered "},
          {"protocol": "udp", "internalPort": 6882, "state": "pending"},
          {"protocol": "tcp", "internalPort": 6883, "externalPort": 6883, "method": "upnp", "state": "mapped"},
          {"protocol": "tcp", "internalPort": 1, "state": "weird"}
        ]}
        """#)
        let segments = BtPortSegment.segments(status)
        #expect(segments == [
            .mapping(port: "TCP 6881 → 50123", state: .mapped(method: "NAT-PMP")),
            .mapping(port: "UDP 6881", state: .failed("no   gateway\nanswered")),
            .mapping(port: "UDP 6882", state: .pending),
            .mapping(port: "TCP 6883", state: .mapped(method: "UPnP")),
            .mapping(port: "TCP 1", state: .other("weird")),
            .externalIp("203.0.113.7"),
        ])
        #expect(segments[1].text == "UDP 6881 \(L("btPortMappingFailed")): no gateway answered")
        #expect(segments[1].fullText == "UDP 6881 \(L("btPortMappingFailed")): no   gateway\nanswered")
        #expect(segments[0].fullText == nil)
        #expect(segments[4].text == "TCP 1 weird")
        #expect(BtPortSegment.segments(BtPortStatus()) == [.idle])
        #expect(BtPortSegment.segments(BtPortStatus(running: true)).isEmpty)
        #expect(BtPortSegment.methodLabel("pcp") == "PCP" && BtPortSegment.methodLabel("x") == "x")
    }
}
