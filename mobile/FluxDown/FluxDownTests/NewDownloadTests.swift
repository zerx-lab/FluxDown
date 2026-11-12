import FluxDomain
import Testing

@testable import FluxDown

@Suite("LinkParser")
struct LinkParserTests {
    @Test func plainLinksOnePerLine() {
        let e = parseEntries("https://a.com/x.zip\n\nhttp://b.com/y.iso\nftp://c.org/z")
        #expect(e.map(\.url) == ["https://a.com/x.zip", "http://b.com/y.iso", "ftp://c.org/z"])
    }

    @Test func commentsAndBlankLinesSkipped() {
        let e = parseEntries("# note\n\nhttps://a.com/1\n  \n#https://skip.me")
        #expect(e.map(\.url) == ["https://a.com/1"])
    }

    @Test func optionLinesAttachToPreviousEntry() {
        let e = parseEntries("https://a.com/1\n  out=name.bin\n\tchecksum=sha-256=abc\nhttps://a.com/2")
        #expect(e.count == 2)
        #expect(e[0].fileName == "name.bin")
        #expect(e[0].checksum == "sha-256=abc")
        #expect(e[1].fileName.isEmpty)
    }

    @Test func orphanOptionLineIgnored() {
        #expect(parseEntries("  out=x.bin").isEmpty)
    }

    @Test func httpLinkMustLeadTheLineWhenStrict() {
        #expect(parseEntries("see https://a.com/x").isEmpty)
        #expect(parseEntries("HTTPS://A.com/x").map(\.url) == ["HTTPS://A.com/x"])
    }

    @Test func magnetAndEd2kAreCutFromTheirPosition() {
        let e = parseEntries("download MAGNET:?xt=urn:btih:abc&dn=n\nprefix ed2k://|file|a.iso|1|h|/")
        #expect(e.map(\.url) == ["MAGNET:?xt=urn:btih:abc&dn=n", "ed2k://|file|a.iso|1|h|/"])
    }

    @Test func looseModeFindsLinkInsideLineAndTrimsPunctuation() {
        let e = parseEntries("get it: https://a.com/x.zip), thanks\nnothing here", loose: true)
        #expect(e.map(\.url) == ["https://a.com/x.zip"])
    }

    @Test func crlfLineEndings() {
        let e = parseEntries("https://a.com/1\r\n  out=a.bin\r\nhttps://a.com/2\r\n")
        #expect(e.map(\.url) == ["https://a.com/1", "https://a.com/2"])
        #expect(e[0].fileName == "a.bin")
    }

    @Test func dedupeKeepsFirst() {
        let list = [UrlEntry(url: "u1", fileName: "a"), UrlEntry(url: "u1", fileName: "b"), UrlEntry(url: "u2")]
        #expect(list.dedupe().map(\.fileName) == ["a", ""])
    }

    @Test func toTextRoundTrips() {
        let e = UrlEntry(url: "https://a.com/1", fileName: "n.bin", checksum: "md5=ff")
        #expect(parseEntries(e.toText()) == [e])
    }

    @Test func appendSkipsExistingUrlsAndKeepsText() {
        let existing = "https://a.com/1\n  out=keep.bin\n\n"
        let result = appendEntries(existing, [UrlEntry(url: "https://a.com/1"), UrlEntry(url: "https://a.com/2")])
        #expect(result.added == 1)
        #expect(result.text == "https://a.com/1\n  out=keep.bin\nhttps://a.com/2")
        #expect(appendEntries("", [UrlEntry(url: "u")]).text == "u")
    }

    @Test func protocolDetection() {
        #expect(protocolOf("magnet:?xt=1") == .bt)
        #expect(protocolOf("torrent-file://abc") == .bt)
        #expect(protocolOf("ED2K://|file|") == .ed2k)
        #expect(protocolOf("ftp://h/f") == .ftp)
        #expect(protocolOf("https://h/v.m3u8?token=1") == .hls)
        #expect(protocolOf("https://h/v.mp4") == .http)
    }

    @Test func inferredNames() {
        #expect(inferName(UrlEntry(url: "https://h/a%20b.zip?x=1#f")) == "a b.zip")
        #expect(inferName(UrlEntry(url: "https://example.com/")) == "example.com")
        #expect(inferName(UrlEntry(url: "https://h/x", fileName: "out.bin")) == "out.bin")
        #expect(inferName(UrlEntry(url: "magnet:?xt=urn:btih:ABCDEF1234567890&dn=My+Show%21")) == "My+Show!")
        #expect(inferName(UrlEntry(url: "magnet:?xt=urn:btih:ABCDEF1234567890")) == "magnet:ABCDEF123456")
        #expect(inferName(UrlEntry(url: "ed2k://|file|f%20x.iso|100|h|/")) == "f x.iso")
    }

    @Test func hostExtraction() {
        #expect(hostOrNull("https://www.example.com:8080/a") == "example.com")
        #expect(hostOrNull("ftp://user:pw@files.org/a") == "files.org")
        #expect(hostOrNull("magnet:?xt=1") == nil)
    }

    @Test func uaPresetDetection() {
        #expect(detectUaPreset("") == taskUaDefault)
        #expect(detectUaPreset(uaPresetValue("firefox")) == "firefox")
        #expect(detectUaPreset("curl/8") == taskUaCustom)
    }

    @Test func checksumHelpers() {
        #expect(checksumSpec(algorithm: "md5", hash: "  ") == "")
        #expect(checksumSpec(algorithm: "sha-1", hash: " ab ") == "sha-1=ab")
        #expect(checksumHexValid(""))
        #expect(checksumHexValid(String(repeating: "a", count: 64)))
        #expect(!checksumHexValid(String(repeating: "g", count: 64)))
        #expect(!checksumHexValid("abc"))
    }

    @Test func proxyWire() {
        #expect(ProxyChoice.follow.wire(manualUrl: "m", customUrl: "c") == "")
        #expect(ProxyChoice.direct.wire(manualUrl: "m", customUrl: "c") == "direct://")
        #expect(ProxyChoice.system.wire(manualUrl: "m", customUrl: "c") == "system://")
        #expect(ProxyChoice.globalManual.wire(manualUrl: "m", customUrl: "c") == "m")
        #expect(ProxyChoice.custom.wire(manualUrl: "m", customUrl: " c ") == "c")
    }

    @Test func manualProxyUrlBuilding() {
        #expect(manualProxyUrl([:]) == "")
        #expect(manualProxyUrl(["proxy_host": "h", "proxy_port": "0"]) == "")
        #expect(manualProxyUrl(["proxy_host": "h", "proxy_port": "8080"]) == "http://h:8080")
        let full = manualProxyUrl([
            "proxy_type": "socks5", "proxy_host": "h", "proxy_port": "1080",
            "proxy_username": "u@x", "proxy_password": "p w",
        ])
        #expect(full == "socks5://u%40x:p%20w@h:1080")
    }

    @Test func saveDirValidation() {
        #expect(isValidSaveDir(""))
        #expect(isValidSaveDir("/var/mobile"))
        #expect(isValidSaveDir("C:\\Downloads"))
        #expect(isValidSaveDir("D:/x"))
        #expect(isValidSaveDir("\\\\nas\\share"))
        #expect(!isValidSaveDir("relative/dir"))
        #expect(!isValidSaveDir("C:"))
    }
}

#if FULL_FLAVOR
@Suite("BtFileTree")
struct BtFileTreeTests {
    private let files = [
        BtFile(index: 0, path: "root.txt", size: 1),
        BtFile(index: 1, path: "B/two.bin", size: 20),
        BtFile(index: 2, path: "a/x/deep.bin", size: 300),
        BtFile(index: 3, path: "a/one.bin", size: 4000),
    ]

    @Test func foldersFirstThenFilesAndSizesAccumulate() {
        let root = buildBtTree(files)
        let rows = flattenBtTree(root, collapsed: [])
        #expect(rows.map(\.id) == ["d:a", "d:a/x", "f:2", "f:3", "d:B", "f:1", "f:0"])
        #expect(root.size == 4321)
        guard case let .folder(a) = root.children[0] else {
            Issue.record("expected folder")
            return
        }
        #expect(a.indices.sorted() == [2, 3])
        #expect(a.size == 4300)
    }

    @Test func collapsedFolderHidesDescendants() {
        let rows = flattenBtTree(buildBtTree(files), collapsed: ["a"])
        #expect(rows.map(\.id) == ["d:a", "d:B", "f:1", "f:0"])
    }

    @Test func triStateFolder() {
        let root = buildBtTree(files)
        guard case let .folder(a) = root.children[0] else {
            Issue.record("expected folder")
            return
        }
        #expect(btFolderState(a, selected: []) == .off)
        #expect(btFolderState(a, selected: [2]) == .mixed)
        #expect(btFolderState(a, selected: [2, 3]) == .on)
    }

    @Test func defaultCollapseOnlyForLargeLists() {
        let root = buildBtTree(files)
        #expect(defaultCollapsedFolders(root, fileCount: 4).isEmpty)
        #expect(defaultCollapsedFolders(root, fileCount: 61) == ["a/x"])
    }

    @Test func initialSelectionFallsBackToAll() {
        #expect(initialBtSelection(files: files, defaultIndices: [1, 99]) == [1])
        #expect(initialBtSelection(files: files, defaultIndices: []) == [0, 1, 2, 3])
        #expect(initialBtSelection(files: files, defaultIndices: [99]) == [0, 1, 2, 3])
    }
}
#endif

@Suite("Remembered download category")
@MainActor
struct NewDownloadCategoryMemoryTests {
    private let category = CustomCategoryDto(
        id: "custom:完整-id", name: "分类", position: 0, saveDir: #"D:\下载  2024\Programs"#
    )

    private func state(mode: JSONValue?, categories: [CustomCategoryDto]? = nil) throws -> HostState {
        var state = HostState()
        state.config["default_save_dir"] = "/default"
        var values = [CustomCategoryDto.preferenceKey: CustomCategoryDto.preferenceValue(categories ?? [category])]
        if let mode { values[SaveDirCategoryMode.preferenceKey] = mode }
        state.sections[HostSection.agentPreferences] = try ProtocolJSON.encode(
            AgentPreferencesDto(values: values), what: "category memory test"
        )
        return state
    }

    @Test func formsRestoreExplicitModeAndNeverAuthorizeAutomaticRouting() throws {
        for mode: SaveDirCategoryMode in [.noCategory, .category(category.id)] {
            let form = NewDownloadForm(prefill: "https://example.com/movie.mp4", state: try state(mode: mode.preferenceValue))
            #expect(form.categoryState.mode == mode)
            #expect(form.saveDir == (mode == .noCategory ? "/default" : category.saveDir))
            let request = try #require(form.buildRequests(startPaused: false, queue: "", manualProxy: "").first)
            #expect(request.offeredSaveDir == nil)
            #expect(request.saveDir == form.saveDir)
            #if FULL_FLAVOR
            let base = form.manifestBase(queue: "", manualProxy: "")
            #expect(base.categoryMode == mode)
            #expect(base.offeredSaveDir == nil)
            #expect(base.saveDir == form.saveDir)
            #endif
        }
    }

    @Test func missingInvalidAndUnavailableModesFallBackToAutomaticRouting() throws {
        for value: JSONValue? in [nil, .bool(true), .string("invalid"), .string("category:deleted")] {
            let form = NewDownloadForm(prefill: "https://example.com/file.zip", state: try state(mode: value))
            #expect(form.categoryState.mode == .auto)
            let request = try #require(form.buildRequests(startPaused: true, queue: "", manualProxy: "").first)
            #expect(request.offeredSaveDir == "/default")
        }
        let none = NewDownloadForm(prefill: "", state: try state(mode: .string("none"), categories: []))
        #expect(none.categoryState.mode == .noCategory)
        #expect(none.saveDir == "/default")
    }

    @Test func separateHostSnapshotsRestoreTheirOwnModes() throws {
        let first = NewDownloadForm(prefill: "", state: try state(mode: .string("none")))
        let second = NewDownloadForm(prefill: "", state: try state(mode: .string("category:" + category.id)))
        let third = NewDownloadForm(prefill: "", state: try state(mode: nil))
        #expect(first.categoryState.mode == .noCategory)
        #expect(second.categoryState.mode == .category(category.id))
        #expect(third.categoryState.mode == .auto)
    }
}
