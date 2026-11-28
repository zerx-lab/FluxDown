import Foundation
import Testing
@testable import FluxDown

struct SiteRequestHeadersTests {
    @Test func siteAndSourceAreSeparateAndOtherHeaderValuesRoundTrip() throws {
        let origin = "https://例え.example:8443"
        let referer = "https://source.example/日本語?q=" + String(repeating: "x", count: 1_024)
        let extra = "X-Example:  a:  b\nX-Locale: עברית"
        let draft = try #require(SiteHeaderRule(origin: origin, referer: referer, extra: extra))

        #expect(draft.origin == origin)
        #expect(draft.referer == referer)
        #expect(draft.headers["X-Example"] == " a:  b")
        #expect(draft.extra == extra)
        let encoded = String(decoding: try JSONEncoder().encode([draft]), as: UTF8.self)
        #expect(SiteHeaderRule.parse(encoded) == [draft])
    }

    @Test func rejectsMalformedExtrasAndDoesNotTreatCorruptConfigAsEmpty() {
        let origin = "https://files.example"
        #expect(SiteHeaderRule(origin: origin, referer: "", extra: "X-Example") == nil)
        #expect(SiteHeaderRule(origin: origin, referer: "", extra: "X-Example: a\nx-example: b") == nil)
        #expect(SiteHeaderRule(origin: origin, referer: "", extra: "referer: https://source.example") == nil)
        #expect(SiteHeaderRule.parse(#"[{"origin":"https://files.example"}]"#) == nil)
        #expect(SiteHeaderRule.parse("") == [])
    }
}
