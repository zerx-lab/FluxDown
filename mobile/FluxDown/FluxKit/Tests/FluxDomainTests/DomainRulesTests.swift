@testable import FluxDomain
import Testing

struct DomainRulesTests {
    @Test func categoryIndexPicksFirstSpecificMatchAndFallsBackToOther() {
        let index = CategoryIndex(TaskCategory.builtin)
        #expect(index.categoryOf("Movie.MKV")?.builtinType == "video")
        #expect(index.categoryOf("no-extension")?.builtinType == "other")
        let other = TaskCategory.builtin.first { $0.isOther }!
        let all = TaskCategory.builtin.first { $0.isAll }!
        #expect(index.matches(other, "a.unknownext"))
        #expect(!index.matches(other, "a.zip"))
        #expect(index.matches(all, "anything"))
    }

    @Test func regexCategoryWinsByPositionAndInvalidRegexMatchesNothing() {
        let docs = TaskCategory(id: "c1", name: "Specs", icon: "file", extensions: [], regexPattern: "spec.*\\.pdf$", position: 0)
        let broken = TaskCategory(id: "c2", name: "Broken", icon: "file", extensions: [], regexPattern: "([", position: 1)
        let index = CategoryIndex([docs, broken] + TaskCategory.builtin.map { var c = $0; c.position += 10; return c })
        #expect(index.categoryOf("SPEC-v2.PDF")?.id == "c1")
        #expect(index.categoryOf("manual.pdf")?.builtinType == "document")
        #expect(!index.matches(broken, "(["))
    }

    @Test func urlRulesWinOverExtensionAndRegexRegardlessOfListPosition() {
        let zip = TaskCategory(id: "zip", name: "zip", icon: "file", extensions: ["zip"], position: 1)
        let regex = TaskCategory(id: "rx", name: "rx", icon: "file", extensions: [], regexPattern: "\\.iso$", position: 2)
        let first = TaskCategory(id: "first", name: "first", icon: "file", extensions: [], urlPattern: "https://mirror.test/*", position: 3)
        let second = TaskCategory(id: "second", name: "second", icon: "file", extensions: [], urlPattern: "https://mirror.test/iso/*", position: 4)
        let other = TaskCategory.builtin.first { $0.isOther }!
        let index = CategoryIndex([zip, regex, first, second, other])
        #expect(index.categoryOf("a.zip", url: "https://mirror.test/a.zip")?.id == "first")
        #expect(index.categoryOf("a.zip", url: "https://x.test/a.zip")?.id == "zip")
        #expect(index.categoryOf("ubuntu.iso", url: "https://mirror.test/iso/ubuntu.iso")?.id == "first")
        #expect(index.categoryOf("ubuntu.iso", url: "https://x.test/ubuntu.iso")?.id == "rx")
        #expect(index.categoryOf("a.exe", url: "https://x.test/a.exe")?.isOther == true)
        // 不带网址的老调用方式只看文件名规则。
        #expect(index.categoryOf("a.zip")?.id == "zip")
    }

    @Test func wildcardIsAnchoredCaseInsensitiveAndLiteralOutsideStarAndQuestion() {
        let site = TaskCategory(id: "site", name: "site", icon: "file", extensions: [], urlPattern: "HTTPS://Example.com/file?.zip", position: 1)
        let dotted = TaskCategory(id: "dot", name: "dot", icon: "file", extensions: [], urlPattern: "https://a.test/v1.0/*", position: 2)
        let other = TaskCategory.builtin.first { $0.isOther }!
        let index = CategoryIndex([site, dotted, other])
        #expect(index.categoryOf("x", url: "https://example.com/file1.zip")?.id == "site")
        #expect(index.categoryOf("x", url: "https://example.com/file12.zip")?.isOther == true)
        #expect(index.categoryOf("x", url: "https://example.com/file1.zip?x=1")?.isOther == true)
        #expect(index.categoryOf("x", url: "see https://example.com/file1.zip")?.isOther == true)
        #expect(index.categoryOf("x", url: "https://a.test/v1x0/f")?.isOther == true)
        #expect(index.categoryOf("x", url: "https://a.test/v1.0/f")?.id == "dot")
    }

    @Test func unknownFileNameOnlyLetsUrlRulesParticipate() {
        let wild = TaskCategory(id: "rx", name: "rx", icon: "file", extensions: [], regexPattern: ".*", position: 1)
        let site = TaskCategory(id: "site", name: "site", icon: "file", extensions: [], urlPattern: "https://mirror.test/*", position: 2)
        let blank = TaskCategory(id: "blank", name: "blank", icon: "file", extensions: [], urlPattern: "   ", position: 3)
        let other = TaskCategory.builtin.first { $0.isOther }!
        let index = CategoryIndex([wild, site, blank, other])
        #expect(index.categoryOf("", url: "https://mirror.test/download")?.id == "site")
        #expect(index.categoryOf("", url: "https://x.test/download")?.isOther == true)
        #expect(!index.matches(blank, "a.zip", url: "   "))
        #expect(UrlWildcard.compile("   ") == nil)
    }

    @Test func matchesHonorsUrlRulesForCategoryFilters() {
        let zip = TaskCategory(id: "zip", name: "zip", icon: "file", extensions: ["zip"], position: 1)
        let site = TaskCategory(id: "site", name: "site", icon: "file", extensions: [], urlPattern: "https://mirror.test/*", position: 2)
        let other = TaskCategory.builtin.first { $0.isOther }!
        let all = TaskCategory.builtin.first { $0.isAll }!
        let index = CategoryIndex([all, zip, site, other])
        let url = "https://mirror.test/a.zip"
        #expect(index.matches(all, "a.zip", url: url))
        #expect(index.matches(site, "a.zip", url: url))
        #expect(index.matches(zip, "a.zip", url: url))
        #expect(!index.matches(other, "a.zip", url: url))
        #expect(index.matches(site, "blob", url: url))
        #expect(!index.matches(zip, "blob", url: url))
        #expect(!index.matches(other, "blob", url: url))
        #expect(index.matches(other, "blob", url: "https://x.test/blob"))
    }

    @Test func shareUrlPrefersOriginUrlOverTaskUrl() {
        let site = TaskCategory(id: "site", name: "site", icon: "file", extensions: [], urlPattern: "https://origin.test/*", position: 1)
        let other = TaskCategory.builtin.first { $0.isOther }!
        let index = CategoryIndex([site, other])
        func task(origin: String, url: String) -> DownloadTask {
            DownloadTask(taskId: "t", url: url, originUrl: origin, fileName: "a.bin", status: .completed)
        }
        let withOrigin = task(origin: "https://origin.test/page", url: "https://cdn.test/a.bin")
        let withoutOrigin = task(origin: "", url: "https://cdn.test/a.bin")
        #expect(index.categoryOf(withOrigin.fileName, url: withOrigin.shareUrl)?.id == "site")
        #expect(index.categoryOf(withoutOrigin.fileName, url: withoutOrigin.shareUrl)?.isOther == true)
    }

    @Test func bytesUseBinaryUnitsWithMagnitudeDependentDigits() {
        #expect(Format.bytes(0).description == "0 B")
        #expect(Format.bytes(1023).description == "1023 B")
        #expect(Format.bytes(1536).description == "1.50 KB")
        #expect(Format.bytes(15 * 1024 * 1024).description == "15.0 MB")
        #expect(Format.bytes(150 * 1024 * 1024 * 1024).description == "150 GB")
        #expect(Format.speed(0) == nil)
    }

    @Test func etaIsUnknownWhenStalledUnsizedDoneOrBeyondADay() {
        #expect(Format.etaSeconds(downloaded: 0, total: 100, speed: 0) == nil)
        #expect(Format.etaSeconds(downloaded: 0, total: 0, speed: 10) == nil)
        #expect(Format.etaSeconds(downloaded: 100, total: 100, speed: 10) == nil)
        #expect(Format.etaSeconds(downloaded: 0, total: 86_401, speed: 1) == nil)
        #expect(Format.etaSeconds(downloaded: 0, total: 101, speed: 10) == 11)
        #expect(Format.percent(0.12345) == "12.3%")
    }

    @Test func translationsComeFromSharedAssetsWithEnglishKeyFallback() {
        let zh = L10n(locale: "zh")
        let en = L10n(locale: "en")
        #expect(zh("cancel") == "取消")
        #expect(en("cancel") == "Cancel")
        #expect(zh("__missing_key__") == "__missing_key__")
        #expect(L10n.fill("{n} items", ["n": 3]) == "3 items")
    }

    @Test(arguments: [
        "zh-TW", "zh-tw", "ZH_TW", " ZH_hant_hk ", "zh-Hant", "zh-Hant-TW", "zh-Hant-CN",
        "zh-HK", "zh-MO", "zh-HK-u-nu-hanidec", "zh-Hant-HK-u-nu-hanidec",
    ])
    func traditionalChineseLocalesLoadSharedTaiwaneseResources(language: String) {
        let translation = L10n(locale: language)
        #expect(translation.locale == "zh-TW")
        #expect(translation("settings") == "設定")
        #expect(translation("__missing_key__") == "__missing_key__")
    }

    @Test(arguments: [
        "zh", "zh-CN", "zh-SG", "zh-Hans", "zh-Hans-TW", "zh-Hans-HK", "zh-Hans-MO",
        "zh-Hans-HK-u-nu-hanidec", "zh-Hantfoo", "zh-TWfoo", "zh-Latn-TW",
    ])
    func simplifiedChineseAndExplicitScriptsRemainSimplified(language: String) {
        let translation = L10n(locale: language)
        #expect(translation.locale == "zh")
        #expect(translation("settings") == "设置")
    }

    @Test func languageResolutionPreservesPreferenceOrderCanonicalNamesAndFallbacks() {
        let available: Set<String> = ["en", "zh", "zh-TW"]
        #expect(L10n.resolve(preferred: ["xx-YY", "zh-HK", "en"], available: available) == "zh-TW")
        #expect(L10n.resolve(preferred: ["en-US", "zh-TW"], available: available) == "en")
        #expect(L10n.resolve(preferred: ["zh-HK"], available: ["en", "zh", "zh-HK", "zh-TW"]) == "zh-HK")
        #expect(L10n.resolve(preferred: ["zh-Hant"], available: ["en", "zh"]) == "zh")
        #expect(L10n.resolve(preferred: ["zh-Hant", "en-GB"], available: ["en"]) == "en")
        #expect(L10n.resolve(preferred: [], available: available) == "en")
        #expect(L10n.resolve(preferred: ["xx-YY"], available: available) == "en")
        #expect(L10n(locale: "xx-YY")("cancel") == "Cancel")
    }

    @Test func taskProtocolAndShareUrlFollowSentinels() {
        let torrent = DownloadTask(taskId: "t", url: "torrent-file://abc", originUrl: "https://x/a.torrent", fileName: "a", status: .downloading)
        #expect(torrent.protocol == .bt)
        #expect(torrent.shareUrl == "https://x/a.torrent")
        let hls = DownloadTask(taskId: "h", url: "https://x/live.m3u8?token=1", fileName: "v", status: .pending)
        #expect(hls.protocol == .hls)
        #expect(hls.shareUrl == hls.url)
        #expect(hls.progress == nil)
    }

    @Test func btNameOutcomeOnlyReportsTrimmedChanges() {
        #expect(btNameOutcome(initial: "Show", input: "Show") == nil)
        #expect(btNameOutcome(initial: "Show", input: "  Show ") == nil)
        #expect(btNameOutcome(initial: " Show ", input: "Show") == nil)
        #expect(btNameOutcome(initial: "Show", input: "   ") == nil)
        #expect(btNameOutcome(initial: "Show", input: "") == nil)
        #expect(btNameOutcome(initial: "Show", input: "  New Name ") == "New Name")
        #expect(btNameOutcome(initial: "", input: "x.mkv") == "x.mkv")
    }

    @Test func onlyBtCancelDeletesTask() {
        #expect(SelectionKind.bt([], saveDir: "/d").cancelDeletesTask)
        #expect(!SelectionKind.hls([]).cancelDeletesTask)
        #expect(!SelectionKind.variant([]).cancelDeletesTask)
        #expect(!SelectionKind.plugin(site: "s", url: "u", candidates: []).cancelDeletesTask)
    }
}
