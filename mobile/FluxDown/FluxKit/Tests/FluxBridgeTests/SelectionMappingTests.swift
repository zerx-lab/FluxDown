@testable import FluxBridge
import FluxDomain
import FluxRustBindings
import Testing

/// 「文件已存在」询问经 UniFFI DTO ↔ 领域模型的映射：字段、可选值与动作双向保真。
struct SelectionMappingTests {
    @Test func fileExistsRequestKeepsEveryFieldAndAllowedActions() {
        let dto = SelectionRequestDto(
            requestId: "r1",
            taskId: "t1",
            kind: .fileExists(
                fileName: "a.zip",
                saveDir: "/dl",
                existingSize: 1024,
                existingModifiedUnixMs: 1_760_000_000_000,
                incomingSize: nil,
                renamePreview: "a (1).zip",
                actions: [.rename, .overwrite]
            ),
            defaultChoice: .fileExists(action: .rename),
            deadlineUnixMs: 42
        )
        let request = dto.domain
        let conflict = request.fileConflict
        #expect(conflict?.fileName == "a.zip")
        #expect(conflict?.saveDir == "/dl")
        #expect(conflict?.existingSize == 1024)
        #expect(conflict?.existingModifiedUnixMs == 1_760_000_000_000)
        #expect(conflict?.incomingSize == nil)
        #expect(conflict?.renamePreview == "a (1).zip")
        #expect(conflict?.actions == [.rename, .overwrite])
        #expect(conflict?.allows(.skip) == false)
        #expect(request.defaultChoice == .fileExists(action: .rename))
    }

    @Test func fileExistsOutcomesRoundTripEveryAction() {
        for action in [FileExistsAction.rename, .overwrite, .skip] {
            let dto = SelectionOutcome.fileExists(action: action).dto
            #expect(dto.domain == .fileExists(action: action))
        }
        #expect(SelectionOutcome.cancelled.dto.domain == .cancelled)
    }

    @Test func pluginChoiceRequestAndOutcomeMapBothWays() {
        let dto = SelectionRequestDto(
            requestId: "r2",
            taskId: "t2",
            kind: .plugin(
                site: "bilibili.com",
                url: "https://www.bilibili.com/video/BV1",
                candidates: [
                    PluginCandidateDto(identity: "a.one", name: "One", version: "1.0.0", description: "d1"),
                    PluginCandidateDto(identity: "b.two", name: "Two", version: "2.0.0", description: "d2"),
                ]
            ),
            defaultChoice: .plugin(identity: "a.one", remember: false),
            deadlineUnixMs: 7
        )
        let request = dto.domain
        #expect(request.kind == .plugin(
            site: "bilibili.com",
            url: "https://www.bilibili.com/video/BV1",
            candidates: [
                PluginCandidate(identity: "a.one", name: "One", version: "1.0.0", description: "d1"),
                PluginCandidate(identity: "b.two", name: "Two", version: "2.0.0", description: "d2"),
            ]
        ))
        #expect(request.defaultChoice == .plugin(identity: "a.one", remember: false))
        let outcome = SelectionOutcome.plugin(identity: "b.two", remember: true)
        #expect(outcome.dto.domain == outcome)
    }

    @Test func btSaveDirMapsBothWays() {
        let dto = SelectionRequestDto(
            requestId: "r3",
            taskId: "t3",
            kind: .bt(files: [BtFileDto(index: 0, path: "a.bin", size: 12)], saveDir: "/data"),
            defaultChoice: .bt(indices: [0], saveDir: nil),
            deadlineUnixMs: 9
        )
        let request = dto.domain
        #expect(request.kind == .bt([BtFile(index: 0, path: "a.bin", size: 12)], saveDir: "/data"))
        #expect(request.defaultChoice == .bt(indices: [0], saveDir: nil))
        let relocate = SelectionOutcome.bt(indices: [0, 2], saveDir: "/new")
        #expect(relocate.dto.domain == relocate)
        let keep = SelectionOutcome.bt(indices: [1], saveDir: nil)
        #expect(keep.dto.domain == keep)
    }
}
