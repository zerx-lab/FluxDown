import FluxDomain
import Foundation
import Testing

struct TorrentPreviewTests {
    @Test func previewRetainsOriginalIndicesAndLargeSizes() throws {
        let data = Data(#"{"torrentBlobId":"torrent:test","name":"资料","totalBytes":5000000000,"files":[{"index":2,"path":"目录/a.mkv","size":5000000000}]}"#.utf8)
        let preview = try JSONDecoder().decode(TorrentPreviewDto.self, from: data)
        #expect(preview.files.first?.index == 2)
        #expect(preview.files.first?.path == "目录/a.mkv")
        #expect(preview.totalBytes == 5_000_000_000)
    }

    @Test func confirmationRoundTripsSourceAndDraft() throws {
        let data = Data(#"{"torrentBlobId":"torrent:test","saveDir":"/downloads","selectedFileIndices":[2],"queueId":"queue","startPaused":true,"customName":"资料","sourceUrl":"magnet:?xt=test","options":{"url":"magnet:?xt=test","userAgent":"draft"}}"#.utf8)
        let params = try JSONDecoder().decode(TorrentConfirmParams.self, from: data)
        #expect(params.sourceUrl == "magnet:?xt=test")
        #expect(params.startPaused == true)
        let encoded = try JSONEncoder().encode(params)
        #expect(try JSONDecoder().decode(TorrentConfirmParams.self, from: encoded) == params)
        let legacy = try JSONDecoder().decode(TorrentConfirmParams.self, from: Data(#"{"torrentBlobId":"torrent:test","saveDir":"/downloads","selectedFileIndices":[2]}"#.utf8))
        #expect(legacy.options == nil)
        #expect(legacy.sourceUrl == nil)
    }
}
