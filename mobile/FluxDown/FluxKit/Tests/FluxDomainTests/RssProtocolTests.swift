import FluxDomain
import Foundation
import Testing

/// `daemon.rss.*` 的 wire 形状（真实 serde camelCase JSON）与体积字面量。
struct RssProtocolTests {
    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try ProtocolJSON.decode(type, from: Data(json.utf8), what: "test")
    }

    @Test func sourceDecodesEveryField() throws {
        let json = """
        {"sourceId":"s1","providerId":"rss","providerConfig":"{}","url":"https://example.com/rss.xml","name":"Mikan",
         "enabled":false,"autoDownload":false,"startPaused":true,"queueId":"later","saveDir":"/data/rss",
         "intervalMinutes":120,"includePattern":"1080p|720p","excludePattern":"CHS","useRegex":true,"smartEpisode":true,
         "sizeMinBytes":209715200,"sizeMaxBytes":2147483648,"sendReferer":false,"notifyOnDownload":false,"maxPerFetch":50,
         "cookies":"a=b","userAgent":"UA","proxyUrl":"http://p:1","lastFetchAt":1700000100,"lastSuccessAt":1700000000,
         "lastError":"boom","failCount":3,"seeded":true,"position":2,"unreadCount":7}
        """
        let source = try decode(RssSourceDetail.self, json)
        #expect(source.sourceId == "s1")
        #expect(source.providerConfig == "{}")
        #expect(source.enabled == false)
        #expect(source.autoDownload == false)
        #expect(source.startPaused)
        #expect(source.queueId == "later")
        #expect(source.intervalMinutes == 120)
        #expect(source.useRegex && source.smartEpisode)
        #expect(source.sizeMinBytes == 209_715_200)
        #expect(source.sizeMaxBytes == 2_147_483_648)
        #expect(source.sendReferer == false && source.notifyOnDownload == false)
        #expect(source.maxPerFetch == 50)
        #expect(source.lastFetchAt == 1_700_000_100)
        #expect(source.failCount == 3)
        #expect(source.unreadCount == 7)
        // 往返无损：updateSource 要把完整订阅写回。
        let again = try ProtocolJSON.decode(RssSourceDetail.self, from: ProtocolJSON.encode(source, what: "t"), what: "t")
        #expect(again == source)
    }

    @Test func sourceDefaultsMatchRustSerdeDefaultsWhenFieldsAreMissing() throws {
        let source = try decode(RssSourceDetail.self, #"{"sourceId":"s2","url":"https://feed/2","name":"Two","enabled":true}"#)
        #expect(source.providerId == "rss")
        #expect(source.autoDownload)
        #expect(source.sendReferer)
        #expect(source.notifyOnDownload)
        #expect(source.startPaused == false)
        #expect(source.intervalMinutes == 0)
        #expect(source.effectiveIntervalMinutes == 30)
        #expect(source.effectiveMaxPerFetch == 20)
        #expect(source.lastError.isEmpty)
    }

    @Test func sourceWithoutUrlIsRejected() {
        #expect(throws: (any Error).self) { try decode(RssSourceDetail.self, #"{"sourceId":"s"}"#) }
    }

    @Test func itemDecodesStatusReasonAndTaskLink() throws {
        let json = """
        [{"sourceId":"s1","guid":"g1","title":"Show - 02 [1080p]","link":"https://x/1","enclosureUrl":"https://x/1.torrent",
          "enclosureLength":734003200,"pubDate":1700000000,"fetchedAt":1700000050,"status":1,"taskId":"t9","episodeKey":"show:2","reason":"","imageCount":2},
         {"sourceId":"s1","guid":"g2","title":"Other","link":"","enclosureUrl":"","enclosureLength":0,"pubDate":0,"fetchedAt":1,
          "status":3,"taskId":"","episodeKey":"","reason":"too_large"},
         {"sourceId":"s1","guid":"g3","title":"Future","link":"https://x/3","enclosureUrl":"","enclosureLength":0,"pubDate":0,
          "fetchedAt":1,"status":42,"taskId":"","episodeKey":"","reason":"brand_new_reason"}]
        """
        let items = try decode([RssItemDto].self, json)
        #expect(items[0].state == .downloaded)
        #expect(items[0].taskId == "t9")
        #expect(items[0].enclosureLength == 734_003_200)
        #expect(items[0].effectiveLink == "https://x/1.torrent")
        #expect(items[0].imageCount == 2)
        #expect(items[1].state == .filtered)
        #expect(items[1].reasonCode == .tooLarge)
        #expect(items[1].imageCount == 0)
        #expect(items[1].reasonCode.i18nKey == "rssReasonTooLarge")
        #expect(items[2].state == .unknown(42))
        #expect(items[2].reasonCode == .unknown("brand_new_reason"))
        #expect(items[2].reasonCode.i18nKey == nil)
        #expect(items[2].effectiveLink == "https://x/3")
    }

    @Test func reasonCodesMapToTheDocumentedKeys() {
        #expect(RssReason(wire: "not_included").i18nKey == "rssReasonNotIncluded")
        #expect(RssReason(wire: "excluded").i18nKey == "rssReasonExcluded")
        #expect(RssReason(wire: "too_small").i18nKey == "rssReasonTooSmall")
        #expect(RssReason(wire: "dup_episode").i18nKey == "rssReasonDupEpisode")
        #expect(RssReason(wire: "torrent_fetch_failed").i18nKey == "rssReasonTorrentFetchFailed")
        #expect(RssReason(wire: "seed_skipped").i18nKey == nil)
        #expect(RssReason(wire: "").i18nKey == nil)
    }

    @Test func itemActionParamsOmitGuidForReadAll() throws {
        let readAll = try ProtocolJSON.encode(RssItemActionParams(sourceId: "s1", action: .readAll), what: "t")
        #expect(String(decoding: readAll, as: UTF8.self) == #"{"action":"readAll","sourceId":"s1"}"#)
        let download = try ProtocolJSON.encode(RssItemActionParams(sourceId: "s1", guid: "g/1", action: .download), what: "t")
        #expect(String(decoding: download, as: UTF8.self) == #"{"action":"download","guid":"g\/1","sourceId":"s1"}"# ||
            String(decoding: download, as: UTF8.self) == #"{"action":"download","guid":"g/1","sourceId":"s1"}"#)
    }

    @Test func validateResponseCarriesDiagnosticsWithoutThrowing() throws {
        let failed = try decode(RssValidateResponse.self, #"{"url":"https://bad","feedTitle":"","items":[],"error":"HTTP 403"}"#)
        #expect(failed.error == "HTTP 403")
        let ok = try decode(RssValidateResponse.self, """
        {"url":"https://ok","feedTitle":"Feed","error":"","items":[{"sourceId":"","guid":"a","title":"A","link":"","enclosureUrl":"",
         "enclosureLength":0,"pubDate":0,"fetchedAt":0,"status":0,"taskId":"","episodeKey":"","reason":""}]}
        """)
        #expect(ok.feedTitle == "Feed")
        #expect(ok.items.count == 1)
        let request = try ProtocolJSON.encode(RssValidateRequest(url: "https://ok", cookies: "a=b"), what: "t")
        #expect(String(decoding: request, as: UTF8.self) == #"{"cookies":"a=b","proxyUrl":"","url":"https:\/\/ok","userAgent":""}"# ||
            String(decoding: request, as: UTF8.self) == #"{"cookies":"a=b","proxyUrl":"","url":"https://ok","userAgent":""}"#)
    }

    @Test func itemsChangedNoticeDecodesTheEngineMessage() throws {
        let json = """
        {"type":"rssItemsChanged","sourceId":"s1","notifyTitles":["A","B"],"items":[{"sourceId":"s1","guid":"g","title":"T",
         "link":"","enclosureUrl":"","enclosureLength":0,"pubDate":0,"fetchedAt":0,"status":0,"taskId":"","episodeKey":"","reason":""}]}
        """
        let notice = try decode(RssItemsChangedNotice.self, json)
        #expect(notice.sourceId == "s1")
        #expect(notice.notifyTitles == ["A", "B"])
        #expect(notice.items.map(\.guid) == ["g"])
    }

    @Test func revisionsSectionIsAStringToIntegerMap() throws {
        let revisions = try decode([String: UInt64].self, #"{"s1":4,"s2":0}"#)
        #expect(revisions["s1"] == 4)
        #expect(revisions["missing"] == nil)
    }

    @Test func directoryListingTreatsDeniedAsDefaultFalse() throws {
        let listing = try decode(RssDirListing.self, #"{"path":"/data","parent":"/","dirs":[{"name":"rss","path":"/data/rss"}]}"#)
        #expect(listing.denied == false)
        #expect(listing.parent == "/")
        #expect(listing.dirs.first?.path == "/data/rss")
        let root = try decode(RssDirListing.self, #"{"path":"/","parent":null,"dirs":[],"denied":true}"#)
        #expect(root.parent == nil)
        #expect(root.denied)
    }

    // MARK: 体积字面量

    @Test func sizeLiteralParsesUnitsFractionsAndSuffix() {
        #expect(RssSizeLiteral.parse("200M") == 209_715_200)
        #expect(RssSizeLiteral.parse("2g") == 2_147_483_648)
        #expect(RssSizeLiteral.parse("1.5 GB") == 1_610_612_736)
        #expect(RssSizeLiteral.parse(" 1024 ") == 1024)
        #expect(RssSizeLiteral.parse("10KB") == 10_240)
        #expect(RssSizeLiteral.parse("5 b") == 5)
        #expect(RssSizeLiteral.parse("1T") == 1_099_511_627_776)
    }

    @Test func sizeLiteralRejectsMalformedInput() {
        for bad in ["", "   ", "abc", "-1", ".5M", "1.M", "1 k b", "gb", "1..5", "1e3", "12 X"] {
            #expect(RssSizeLiteral.parse(bad) == nil, "\(bad)")
        }
        #expect(RssSizeLiteral.parse("99999999999T") == nil)
    }

    @Test func sizeLiteralFormatRoundTrips() {
        #expect(RssSizeLiteral.format(0) == "")
        #expect(RssSizeLiteral.format(-5) == "")
        #expect(RssSizeLiteral.format(209_715_200) == "200M")
        #expect(RssSizeLiteral.format(2_147_483_648) == "2G")
        #expect(RssSizeLiteral.format(1_099_511_627_776) == "1T")
        #expect(RssSizeLiteral.format(1500) == "1500")
        for bytes: Int64 in [1, 1023, 1024, 1536, 209_715_200, 3_221_225_472] {
            #expect(RssSizeLiteral.parse(RssSizeLiteral.format(bytes)) == bytes)
        }
    }
}
