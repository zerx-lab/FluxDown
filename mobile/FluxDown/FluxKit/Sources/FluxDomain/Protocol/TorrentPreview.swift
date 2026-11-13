import Foundation

// 镜像 `native/protocol/src/torrent_preview.rs`：只有 wire 类型，不新增任何 P2P 界面入口。
public struct MagnetPreviewParams: Codable, Sendable, Hashable {
    public var url: String
    public init(url: String) { self.url = url }
}

public struct MagnetPreviewTicket: Codable, Sendable, Hashable {
    public var previewId: String
    public init(previewId: String) { self.previewId = previewId }
}

public struct TorrentFilePreviewParams: Codable, Sendable, Hashable {
    public var path: String
    public init(path: String) { self.path = path }
}

public struct TorrentPreviewParams: Codable, Sendable, Hashable {
    public var torrentBlobId: String
    public init(torrentBlobId: String) { self.torrentBlobId = torrentBlobId }
}

public struct TorrentPreviewFile: Codable, Sendable, Hashable {
    public var index: Int32
    public var path: String
    public var size: Int64
}

public struct TorrentPreviewDto: Codable, Sendable, Hashable {
    public var torrentBlobId: String
    public var name: String
    public var totalBytes: Int64
    public var files: [TorrentPreviewFile]
}

public struct TorrentConfirmParams: Codable, Sendable, Hashable {
    public var torrentBlobId: String
    public var saveDir: String
    public var selectedFileIndices: [Int32]
    public var queueId: String?
    public var startPaused: Bool?
    public var customName: String?
    public var sourceUrl: String?
    /// 完整的 CreateTaskRequest 草稿；与上面的显式确认字段冲突时以显式字段为准。
    public var options: [String: JSONValue]?

    public init(torrentBlobId: String, saveDir: String, selectedFileIndices: [Int32],
                queueId: String? = nil, startPaused: Bool? = nil, customName: String? = nil,
                sourceUrl: String? = nil, options: [String: JSONValue]? = nil) {
        self.torrentBlobId = torrentBlobId
        self.saveDir = saveDir
        self.selectedFileIndices = selectedFileIndices
        self.queueId = queueId
        self.startPaused = startPaused
        self.customName = customName
        self.sourceUrl = sourceUrl
        self.options = options
    }
}
