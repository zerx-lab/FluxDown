import FluxDomain
import Foundation
import Testing
@testable import FluxDown

@MainActor
struct ExternalDownloadTests {
    @Test func firstValidRuleWinsAndSerializesStableIdentifiers() {
        let rules = CaptureDeviceRule.parse("""
            apk=unknown:abc
            ..apk=cloud:wrong
            APK=cloud:
             .ApK = cloud:primary
            apk=cloud:next
            exe=link:01234
            扩展=cloud:not-ascii
            """)
        #expect(CaptureDeviceRule.serialize(rules) == "apk=cloud:primary\napk=cloud:next\nexe=link:01234")
        #expect(CaptureDeviceRule.match(url: "https://host.test/安装%2EAPK?next=app.exe#x", fileName: "", rules: rules)
            == "cloud:primary")
        #expect(CaptureDeviceRule.match(url: "https://host.test/package.apk", fileName: "  build.tést.ExE  ", rules: rules)
            == "link:01234")
        #expect(CaptureDeviceRule.match(url: "https://host.test/package.apk", fileName: "no-extension", rules: rules) == nil)
    }

    @Test(arguments: [
        "https://host.test/.apk", "https://host.test/app.apk.", "https://host.test/app.apk/",
        "https://host.test/app.apk%20", "https://host.test/x%2Fy.apk", "https://host.test/path?q=app.apk",
        "ftp://host.test/app.apk", "magnet:?dn=app.apk",
    ])
    func doesNotInferFromNonFilenames(_ url: String) {
        let rules = CaptureDeviceRule.parse("apk=cloud:primary")
        #expect(CaptureDeviceRule.match(url: url, fileName: "", rules: rules) == nil)
    }

    @Test func mixedCaptureLinksHaveIndependentTargets() throws {
        let payload = try envelope([
            .init(name: "url", value: "https://host.test/a.apk\nhttps://host.test/b.exe"),
            .init(name: "url", value: "https://host.test/c.zip"),
        ])
        let captures = ExternalDownload.capture(from: payload)
        let rules = CaptureDeviceRule.parse("apk=cloud:first\nexe=link:second")
        let ids = captures.map { CaptureDeviceRule.match(url: $0.url, fileName: $0.fileName, rules: rules) }
        #expect(ids == ["cloud:first", "link:second", nil])
        let allDispatchable = captures.allSatisfy { $0.canDispatch }
        #expect(allDispatchable)
    }

    @Test func capturedCredentialsAndHeadersStayOnTheirOwnURLInConfirmation() throws {
        let url = "https://host.test/download?next=magnet:?dn=other"
        let payload = try envelope([
            .init(name: "url", value: url),
            .init(name: "url", value: "https://other.test/other.apk"),
            .init(name: "filename", value: "app.apk"),
            .init(name: "cookies", value: "session=private"),
            .init(name: "referrer", value: "https://host.test/page"),
            .init(name: "headers", value: #"{"Authorization":"Bearer secret","Referer":"https://host.test/page"}"#),
        ])
        let captures = ExternalDownload.capture(from: payload)
        let capture = try #require(captures.first)
        let other = try #require(captures.last)
        #expect(!capture.canDispatch)
        #expect(other.cookies.isEmpty && other.headers.isEmpty && other.fileName.isEmpty)
        #expect(other.hasUnreadableContext && !other.canDispatch)
        let confirmation = ExternalDownloadConfirmation(request: capture, targetId: "link:gone", failure: "offline")
        let form = NewDownloadForm(prefill: capture.url, state: HostState(), capture: confirmation)
        #expect(form.target(in: []) == nil)
        #expect(form.targetId == "link:gone")
        #expect(form.locksExternalLinks)
        #expect(form.entries.map(\.url) == [url])
        form.targetId = nil // Explicit user override is still allowed.
        let request = try #require(form.buildRequests(startPaused: false, queue: "", manualProxy: "").first)
        #expect(request.fileName == "app.apk")
        #expect(request.cookies == "session=private")
        #expect(request.referrer == "https://host.test/page")
        #expect(request.headers["Authorization"] == "Bearer secret")
        #expect(request.headers["Referer"] == "https://host.test/page")
    }

    @Test func nonCredentialSourceMetadataCanBeRouted() throws {
        let payload = try envelope([
            .init(name: "url", value: "https://host.test/app.apk"),
            .init(name: "referrer", value: "https://host.test/page"),
            .init(name: "headers", value: #"{"user-agent":"browser"}"#),
        ])
        let capture = try #require(ExternalDownload.capture(from: payload).first)
        #expect(capture.canDispatch)
        #expect(capture.hasRequestContext)
        #expect(!ExternalDownload(url: "https://user:password@host.test/app.apk").canDispatch)
        #expect(!ExternalDownload(url: "ftp://host.test/app.apk").canDispatch)
        #expect(!ExternalDownload(url: "https://host.test/app.apk", headers: ["Origin": "https://example.test"]).canDispatch)
    }

    @Test func malformedHeaderContextDoesNotSilentlyDispatch() throws {
        let payload = try envelope([
            .init(name: "url", value: "https://host.test/app.apk"),
            .init(name: "headers", value: #"{"Cookie":true}"#),
        ])
        let capture = try #require(ExternalDownload.capture(from: payload).first)
        #expect(capture.hasUnreadableContext)
        #expect(!capture.canDispatch)
    }

    private func envelope(_ query: [URLQueryItem]) throws -> URL {
        var components = URLComponents()
        components.scheme = "fluxdown"
        components.host = "download"
        components.queryItems = query
        return try #require(components.url)
    }
}
