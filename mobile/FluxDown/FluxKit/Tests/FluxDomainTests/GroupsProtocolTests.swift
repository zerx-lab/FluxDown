import FluxDomain
import Foundation
import Testing

/// 任务组 / 清单预解析 wire DTO 与 D6 聚合、N5 勾选投影。
struct GroupsProtocolTests {
    @Test func previewResponseDecodesPluginManifest_andFlagsFallbackCases() throws {
        let data = Data(#"""
        {"name":"Album","sourceUrl":"https://x/s/1","error":"",
         "items":[{"id":"a","name":"a.jpg","path":"2024/trip","size":1024,"variants":[]},
                  {"id":"v","name":"v.mp4","path":"","size":0,"variants":[{"id":"hd","label":"1080p","size":9000},{"id":"sd","label":"480p","size":0}]}]}
        """#.utf8)
        let preview = try ProtocolJSON.decode(ResolvePreviewResponse.self, from: data, what: "preview")
        #expect(preview.hasManifest)
        #expect(preview.items[0].path == "2024/trip" && preview.items[1].variants.map(\.id) == ["hd", "sd"])

        let empty = try ProtocolJSON.decode(ResolvePreviewResponse.self, from: Data(#"{"name":"","sourceUrl":"u"}"#.utf8), what: "preview")
        #expect(!empty.hasManifest && empty.items.isEmpty && empty.error.isEmpty)
        let failed = try ProtocolJSON.decode(ResolvePreviewResponse.self, from: Data(#"{"name":"","sourceUrl":"u","error":"login expired","items":[{"id":"1","name":"n","path":"","size":1,"variants":[]}]}"#.utf8), what: "preview")
        #expect(!failed.hasManifest)
        #expect(empty.pluginId.isEmpty)
        let withPlugin = try ProtocolJSON.decode(ResolvePreviewResponse.self, from: Data(#"{"name":"A","sourceUrl":"u","pluginId":"com.x.p","items":[]}"#.utf8), what: "preview")
        #expect(withPlugin.pluginId == "com.x.p")
    }

    @Test func createGroupRequestEncodesWireNames() throws {
        let request = CreateGroupRequest(
            sourceUrl: "https://x/s/1", groupName: "Album", saveDir: "/d", queueId: "q", segments: 2,
            extraHeaders: ["A": "B"], startPaused: true,
            items: [GroupItemRequest(resolverItem: "v@hd", fileName: "v.mp4", relPath: "p", size: 9)],
            resolverPluginId: "com.example.archive",
            archiveMetadata: ArchiveMetadataDto(platform: "example", seriesTitle: "Album"),
            categoryRoute: CategoryRouteDto(
                rules: [CategoryRuleDto(matchMode: "extension", extensions: ["mp4"], saveDir: "/video")]
            )
        )
        let json = try ProtocolJSON.decode(JSONValue.self, from: ProtocolJSON.encode(request, what: "g"), what: "g")
        #expect(json["groupName"]?.stringValue == "Album")
        #expect(json["startPaused"]?.boolValue == true)
        #expect(json["extraHeaders"]?["A"]?.stringValue == "B")
        #expect(json["resolverPluginId"]?.stringValue == "com.example.archive")
        #expect(json["items"]?.arrayValue?.first?["resolverItem"]?.stringValue == "v@hd")
        #expect(json["categoryRoute"]?["rules"]?.arrayValue?.first?["saveDir"]?.stringValue == "/video")
        #expect(json["archiveMetadata"]?["seriesTitle"]?.stringValue == "Album")

        let capture = try ProtocolJSON.decode(JSONValue.self, from: ProtocolJSON.encode(CaptureCreateGroupParams(transactionId: "tx", request: request), what: "c"), what: "c")
        #expect(capture["transactionId"]?.stringValue == "tx")
        #expect(capture["request"]?["sourceUrl"]?.stringValue == "https://x/s/1")

        let preview = try ProtocolJSON.decode(JSONValue.self, from: ProtocolJSON.encode(CapturePreviewParams(transactionId: "tx", request: TaskCreateWire(url: "u")), what: "c"), what: "c")
        #expect(preview["request"]?["url"]?.stringValue == "u")

        let response = try ProtocolJSON.decode(CreateGroupResponse.self, from: Data(#"{"groupId":"g1"}"#.utf8), what: "r")
        #expect(response.groupId == "g1")
    }

    @Test func deleteParamsDefaultKeepsFiles() throws {
        let data = try ProtocolJSON.encode(GroupDeleteParams(groupId: "g"), what: "d")
        #expect(String(decoding: data, as: UTF8.self) == #"{"deleteFiles":false,"groupId":"g"}"#)
    }

    // MARK: 组聚合

    private func member(_ id: String, _ status: TaskStatus, done: Int64 = 0, total: Int64 = 0, group: String = "g") -> DownloadTask {
        DownloadTask(taskId: id, url: "https://x/\(id)", fileName: id, status: status, downloadedBytes: done, totalBytes: total, groupId: group)
    }

    @Test func summaryCountsStatusesAndProgress() {
        let tasks = [
            member("a", .completed, done: 100, total: 100),
            member("b", .downloading, done: 50, total: 100),
            member("c", .failed, done: 0, total: 100),
            member("d", .paused, done: 0, total: 100),
            member("e", .pending, done: 0, total: 100),
            member("x", .completed, group: "other"),
        ]
        let summary = GroupSummary(members: GroupSummary.members(of: "g", in: tasks))
        #expect(summary.total == 5 && summary.completed == 1 && summary.failed == 1)
        #expect(summary.downloading == 1 && summary.paused == 1 && summary.pending == 1)
        #expect(summary.failedIds == ["c"] && summary.pausedIds == ["d"] && summary.runningIds == ["b", "e"])
        // 全部大小已知 → 按字节：150 / 500
        #expect(summary.progress == 0.3)
        #expect(summary.canPauseAll && summary.canResumeAll && summary.canRetryFailed && !summary.isFinished)
    }

    @Test func summaryProgressFallsBackToCountWhenSizesUnknown_andHandlesEmpty() {
        let unknown = GroupSummary(members: [member("a", .completed, done: 10, total: 10), member("b", .downloading, done: 5, total: 0)])
        #expect(unknown.progress == 0.5 && unknown.unknownSizeCount == 1)

        let none = GroupSummary(members: [])
        #expect(none.progress == nil && !none.canPauseAll && !none.canResumeAll && !none.isFinished)
        #expect(GroupSummary.members(of: "", in: [member("a", .paused, group: "")]).isEmpty)

        let done = GroupSummary(members: [member("a", .completed, done: 1, total: 1)])
        #expect(done.isFinished && done.progress == 1 && !done.canPauseAll && !done.canResumeAll)
    }

    // MARK: 清单选择

    private func items() -> [PreviewItemDto] {
        [
            PreviewItemDto(id: "a", name: "a.jpg", path: "2024", size: 1000),
            PreviewItemDto(id: "v", name: "v.mp4", path: "", size: 0, variants: [
                PreviewVariantDto(id: "hd", label: "1080p", size: 9000),
                PreviewVariantDto(id: "sd", label: "480p", size: 0),
            ]),
            PreviewItemDto(id: "z", name: "z.zip", size: 0),
        ]
    }

    @Test func selectionToggleAllInvertAndTotals() {
        var selection = ManifestSelection(items: items())
        #expect(selection.count == 0)
        selection.toggle("a")
        selection.toggle("v")
        #expect(selection.count == 2)
        // 默认规格：条目大小（0 = 未知）。
        #expect(selection.selectedSize == (1000, 1))
        selection.chooseVariant("hd", for: "v")
        #expect(selection.selectedSize == (10_000, 0))
        selection.chooseVariant("sd", for: "v") // 规格大小未知 → 回落条目大小（0）
        #expect(selection.selectedSize == (1000, 1))
        selection.chooseVariant(nil, for: "v")
        #expect(selection.variant(for: items()[1]) == nil)

        let all = items().map(\.id)
        selection.toggleAll(in: all)
        #expect(selection.count == 3)
        selection.toggleAll(in: all)
        #expect(selection.count == 0)
        selection.set("a", selected: true)
        selection.invert(in: all)
        #expect(selection.selected == ["v", "z"])
        selection.clear()
        #expect(selection.selected.isEmpty)
    }

    @Test func requestItemsComposeResolverItemWithVariant() {
        var selection = ManifestSelection(items: items())
        selection.toggle("a")
        selection.toggle("v")
        selection.chooseVariant("hd", for: "v")
        let requests = selection.requestItems()
        #expect(requests == [
            GroupItemRequest(resolverItem: "a", fileName: "a.jpg", relPath: "2024", size: 1000),
            GroupItemRequest(resolverItem: "v@hd", fileName: "v.mp4", relPath: "", size: 9000),
        ])
    }

    @Test func defaultGroupNameAndPreviewableUrl() {
        #expect(ManifestSelection.defaultGroupName(manifestName: " Album ", sourceUrl: "https://x/s/1") == "Album")
        #expect(ManifestSelection.defaultGroupName(manifestName: "", sourceUrl: "https://x/share/%E7%9B%B8%E5%86%8C") == "相册")
        #expect(ManifestSelection.defaultGroupName(manifestName: "", sourceUrl: "https://x/") == "")
        #expect(ManifestSelection.isPreviewable(" HTTPS://a/b"))
        #expect(!ManifestSelection.isPreviewable("magnet:?xt=urn:btih:x"))
        #expect(!ManifestSelection.isPreviewable("ed2k://|file|x|1|h|/"))
    }
}
