import FluxDomain
import Testing

struct FileConflictTests {
    private func conflictRequest(_ id: String, actions: [FileExistsAction]) -> SelectionRequest {
        SelectionRequest(
            requestId: id,
            taskId: "t-\(id)",
            kind: .fileExists(FileConflict(fileName: "\(id).bin", saveDir: "/dl", renamePreview: "\(id) (1).bin", actions: actions)),
            defaultChoice: .fileExists(action: .rename),
            deadlineUnixMs: 0
        )
    }

    private let btRequest = SelectionRequest(
        requestId: "bt", taskId: "t-bt", kind: .bt([], saveDir: ""), defaultChoice: .cancelled, deadlineUnixMs: 0
    )

    @Test func fileConflictsAreSplitFromOtherSelectionKindsInHostOrder() {
        let all = [conflictRequest("a", actions: [.rename, .overwrite]), btRequest, conflictRequest("b", actions: [.rename, .overwrite, .skip])]
        #expect(all.fileConflicts.map(\.requestId) == ["a", "b"])
        #expect(btRequest.fileConflict == nil)
    }

    @Test func bulkActionTargetsOnlyRequestsThatAllowIt() {
        let httpLike = conflictRequest("http", actions: [.rename, .overwrite, .skip])
        let ed2kLike = conflictRequest("ed2k", actions: [.rename, .overwrite])
        let list = [httpLike, ed2kLike]
        #expect(FileConflictBulk.targets(list, action: .skip).map(\.requestId) == ["http"])
        #expect(FileConflictBulk.targets(list, action: .overwrite).map(\.requestId) == ["http", "ed2k"])
        #expect(FileConflictBulk.targets(list, action: .rename).count == 2)
    }
}
