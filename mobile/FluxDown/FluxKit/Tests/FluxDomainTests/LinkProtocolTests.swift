import FluxDomain
import Foundation
import Testing

/// 局域网直连 + 云同步：解码 + 纯规则。
struct LinkProtocolTests {
    private static func decode<T: Decodable>(_ json: String, as type: T.Type = T.self) throws -> T {
        try ProtocolJSON.decode(T.self, from: Data(json.utf8), what: "test")
    }

    // MARK: 解码

    @Test func linkDtosDecodeRealShapes() throws {
        let request = try Self.decode(
            #"{"sessionId":"s1","peerName":"Pixel","peerFingerprint":"ab:cd","peerPlatform":"android","sas":"482917","expiresAtUnixMs":1760000000000}"#,
            as: LinkPairingRequestDto.self
        )
        #expect(request.id == "s1")
        #expect(request.sas == "482917")
        #expect(request.peerPlatform == "android")
        let noPlatform = try Self.decode(
            #"{"sessionId":"s2","peerName":"x","peerFingerprint":"f","sas":"111222","expiresAtUnixMs":1}"#,
            as: LinkPairingRequestDto.self
        )
        #expect(noPlatform.peerPlatform == nil)

        let code = try Self.decode(
            #"{"code":"123456","expiresAtUnixMs":1760000120000,"addresses":[],"fingerprint":"fp","deviceName":"iPhone"}"#,
            as: LinkPairingCodeDto.self
        )
        #expect(code.addresses.isEmpty)
        #expect(code.deviceName == "iPhone")

        let peers = try Self.decode(
            #"[{"name":"NAS","host":"192.168.1.20","port":17800,"source":"mdns"},{"fingerprint":"fp","name":"Mac","platform":"macos","host":"fe80::1","port":17800,"appVersion":"0.18.0","source":"manual"}]"#,
            as: [LinkDiscoveredPeer].self
        )
        #expect(peers[0].fingerprint == nil)
        #expect(peers[1].appVersion == "0.18.0")
        #expect(peers[1].source == "manual")

        let begin = try Self.decode(#"{"token":"t","sas":"123456","peerName":"Mac","peerFingerprint":"fp"}"#, as: LinkPairBeginResponse.self)
        #expect(begin.token == "t")
        let finished = try Self.decode(
            #"{"paired":true,"device":{"fingerprint":"fp","name":"Mac","platform":"macos","online":true,"pairedAt":1760000000,"lastSeenAt":1760000001,"defaultSaveDir":"/Users/z/Downloads","pathStyle":"posix"}}"#,
            as: LinkPairFinishResponse.self
        )
        #expect(finished.paired)
        #expect(finished.device?.effectivePathStyle == .posix)
        #expect(try Self.decode(#"{"paired":false}"#, as: LinkPairFinishResponse.self).device == nil)
        #expect(try Self.decode(#"{"taskId":"abc"}"#, as: LinkDispatchResult.self).taskId == "abc")
    }

    @Test func linkParamsUseWireKeys() throws {
        func json(_ value: some Encodable) throws -> String { String(decoding: try ProtocolJSON.encode(value, what: "t"), as: UTF8.self) }
        #expect(try json(LinkPairBeginParams(address: "10.0.0.2:17800", code: "123456")) == #"{"address":"10.0.0.2:17800","code":"123456"}"#)
        #expect(try json(LinkPairFinishParams(token: "t", accept: false)) == #"{"accept":false,"token":"t"}"#)
        #expect(try json(LinkApproveParams(sessionId: "s", accept: true)) == #"{"accept":true,"sessionId":"s"}"#)
        #expect(try json(LinkDiscoveryParams(enabled: true)) == #"{"enabled":true}"#)
        #expect(try json(LinkDispatchParams(fingerprint: "fp", url: "u")) == #"{"fingerprint":"fp","url":"u"}"#)
        #expect(try json(LinkDeviceParams(fingerprint: "fp")) == #"{"fingerprint":"fp"}"#)
    }

    // MARK: 配对规则

    @Test func peerAddressBracketsIPv6() {
        #expect(LinkRules.address(of: LinkDiscoveredPeer(name: "a", host: "192.168.1.20", port: 17800)) == "192.168.1.20:17800")
        #expect(LinkRules.address(of: LinkDiscoveredPeer(name: "a", host: "fe80::1", port: 17800)) == "[fe80::1]:17800")
        #expect(LinkRules.address(of: LinkDiscoveredPeer(name: "a", host: "[fe80::1]", port: 1)) == "[fe80::1]:1")
    }

    @Test func pairingCodeNormalizationAndValidation() {
        #expect(LinkRules.normalizePairingCode("48a2-917 99") == "482917")
        #expect(LinkRules.normalizePairingCode("12") == "12")
        #expect(LinkRules.normalizePairingCode("１２３４５６") == "")
        #expect(LinkRules.isPairingCodeComplete("123456"))
        #expect(!LinkRules.isPairingCodeComplete("12345"))
        #expect(!LinkRules.isPairingCodeComplete("12345a"))

        guard case let .success(params) = LinkRules.validate(address: "  host:17800 ", code: "123 456") else {
            Issue.record("valid input must pass")
            return
        }
        #expect(params == LinkPairBeginParams(address: "host:17800", code: "123456"))
        #expect(LinkRules.validate(address: "  ", code: "123456") == .failure(.addressMissing))
        #expect(LinkRules.validate(address: "a b", code: "123456") == .failure(.addressMissing))
        #expect(LinkRules.validate(address: "h", code: "123") == .failure(.codeIncomplete))
        #expect(LinkRules.InputError.addressMissing.key == "localPairingHostRequired")
        #expect(LinkRules.InputError.codeIncomplete.key == "localPairingCodeIncomplete")
        #expect(LinkRules.isManualAddressValid("https://nas.local/base"))
        #expect(!LinkRules.isManualAddressValid(""))
    }

    @Test func discoveredEntriesDedupeMarkPairedAndSort() {
        let peers = [
            LinkDiscoveredPeer(fingerprint: "p1", name: "zeta", host: "10.0.0.3", port: 17800),
            LinkDiscoveredPeer(fingerprint: nil, name: "Alpha", host: "10.0.0.1", port: 17800),
            LinkDiscoveredPeer(fingerprint: nil, name: "Alpha dup", host: "10.0.0.1", port: 17800),
            LinkDiscoveredPeer(fingerprint: "unknown", name: " ", host: "10.0.0.2", port: 17800),
        ]
        let entries = LinkRules.discoveredEntries(peers, paired: [LinkDeviceInfo(fingerprint: "p1", name: "zeta")])
        #expect(entries.map(\.name) == ["10.0.0.2", "Alpha", "zeta"])
        #expect(entries.map(\.paired) == [false, false, true])
        #expect(entries.map(\.address) == ["10.0.0.2:17800", "10.0.0.1:17800", "10.0.0.3:17800"])
    }

    @Test func countdownRoundsUpAndSasIsGrouped() {
        #expect(LinkRules.secondsUntil(expiresAtUnixMs: 10_400, nowMs: 10_000) == 1)
        #expect(LinkRules.secondsUntil(expiresAtUnixMs: 12_000, nowMs: 10_000) == 2)
        #expect(LinkRules.secondsUntil(expiresAtUnixMs: 10_000, nowMs: 10_000) == 0)
        #expect(LinkRules.secondsUntil(expiresAtUnixMs: 5_000, nowMs: 10_000) == 0)
        #expect(LinkRules.secondsUntil(expiresAtUnixMs: Int64.max, nowMs: Int64.min) > 0)
        #expect(LinkRules.groupSAS("482917") == "482 917")
        #expect(LinkRules.groupSAS("48 29 17") == "482 917")
        #expect(LinkRules.groupSAS("1234") == "123 4")
        #expect(LinkRules.groupSAS("") == "")
    }

    @Test func unixTimestampsAcceptSecondsOrMilliseconds() {
        #expect(LinkRules.date(fromUnix: 0) == nil)
        #expect(LinkRules.date(fromUnix: -5) == nil)
        #expect(LinkRules.date(fromUnix: 1_760_000_000) == Date(timeIntervalSince1970: 1_760_000_000))
        #expect(LinkRules.date(fromUnix: 1_760_000_000_000) == Date(timeIntervalSince1970: 1_760_000_000))
    }

    @Test func inboundQueueHandlesMostUrgentFirst() {
        func request(_ id: String, _ expires: Int64) -> LinkPairingRequestDto {
            LinkPairingRequestDto(sessionId: id, peerName: id, peerFingerprint: id, sas: "000000", expiresAtUnixMs: expires)
        }
        #expect(LinkRules.queue([request("b", 200), request("a", 100), request("c", 200)]).map(\.sessionId) == ["a", "b", "c"])
    }

    // MARK: 同步

    @Test func syncStatusDecodesWithDefaults() throws {
        let full = try Self.decode(
            #"{"enabled":true,"revision":42,"dirtyKeys":["ui.show_sidebar_rss"],"lastError":null,"connected":true,"halted":false,"lastSyncedAtUnixMs":1760000000000,"localOnlyKeys":["appearance.theme_mode"]}"#,
            as: SyncStatusDto.self
        )
        #expect(full.revision == 42)
        #expect(full.dirtyKeys == ["ui.show_sidebar_rss"])
        #expect(full.lastSyncedAtUnixMs == 1_760_000_000_000)
        #expect(full.localOnlyKeys == ["appearance.theme_mode"])
        // 旧 agent 不下发的字段取默认。
        let legacy = try Self.decode(#"{"enabled":false,"revision":0,"dirtyKeys":[]}"#, as: SyncStatusDto.self)
        #expect(legacy == SyncStatusDto())
    }

    @Test func syncPhasePriority() {
        func phase(_ status: SyncStatusDto) -> SyncPhase { SyncRules.phase(status) }
        #expect(phase(SyncStatusDto(enabled: false, lastError: "x", halted: true)) == .off)
        #expect(phase(SyncStatusDto(enabled: true, lastError: "x", halted: true)) == .halted)
        #expect(phase(SyncStatusDto(enabled: true, lastError: "x", connected: true)) == .error)
        #expect(phase(SyncStatusDto(enabled: true, lastErrorReason: "cloudUnreachable", connected: true)) == .error)
        #expect(phase(SyncStatusDto(enabled: true, connected: false)) == .connecting)
        #expect(phase(SyncStatusDto(enabled: true, dirtyKeys: ["a"], connected: true)) == .syncing)
        #expect(phase(SyncStatusDto(enabled: true, connected: true)) == .synced)
    }

    @Test func syncReasonKeyLocalizesByReasonWithGenericFallback() {
        #expect(SyncRules.reasonKey(SyncStatusDto(lastErrorReason: "onlineDeviceLimit")) == "cloudSyncErrorOnlineLimit")
        #expect(SyncRules.reasonKey(SyncStatusDto(lastErrorReason: "deviceUntrusted")) == "cloudSyncErrorDeviceUntrusted")
        #expect(SyncRules.reasonKey(SyncStatusDto(lastErrorReason: "cloudUnreachable")) == "cloudSyncErrorNetwork")
        #expect(SyncRules.reasonKey(SyncStatusDto(lastErrorReason: "mystery")) == "cloudSyncErrorGeneric")
        #expect(SyncRules.reasonKey(SyncStatusDto(lastError: "raw diagnostic text")) == "cloudSyncErrorGeneric")
    }

    @Test func scopeGroupStateAndToggleRule() throws {
        let appearance = try #require(SyncRules.groups.first { $0.id == .appearance })
        #expect(appearance.state(localOnlyKeys: []) == .sync)
        #expect(appearance.state(localOnlyKeys: ["download.keep_awake"]) == .sync)
        #expect(appearance.state(localOnlyKeys: ["appearance.theme_mode"]) == .mixed)
        #expect(appearance.state(localOnlyKeys: appearance.keys) == .local)

        // 只有全部参与同步的分组切为本设备专属；本设备专属与混合都恢复同步。
        #expect(appearance.toggleParams(from: .sync) == SyncLocalOnlyParams(keys: appearance.keys, localOnly: true))
        #expect(appearance.toggleParams(from: .local).localOnly == false)
        #expect(appearance.toggleParams(from: .mixed).localOnly == false)
    }

    @Test func syncGroupsPartitionTheFullCatalogWithoutOverlap() {
        let all = SyncRules.groups.flatMap(\.keys)
        #expect(all.count == 77)
        #expect(all.contains("bt.enabled"))
        #expect(Set(all).count == all.count)
        #expect(SyncRules.groups.map(\.id) == SyncGroup.ID.allCases)
        #expect(all.contains("custom_categories"))
        #expect(all.contains("bt.seed_max_active"))
        #expect(all.contains("bt.guard.cloud_enabled"))
    }

    @Test func relativeSyncTime() {
        let now: Int64 = 1_760_000_000_000
        #expect(SyncRules.ago(syncedAtMs: now - 30_000, nowMs: now) == .justNow)
        #expect(SyncRules.ago(syncedAtMs: now - 5 * 60_000, nowMs: now) == .minutes(5))
        #expect(SyncRules.ago(syncedAtMs: now - 3 * 3_600_000, nowMs: now) == .hours(3))
        #expect(SyncRules.ago(syncedAtMs: now - 2 * 86_400_000, nowMs: now) == .days(2))
        // 时钟回拨（未来时间）按刚刚处理。
        #expect(SyncRules.ago(syncedAtMs: now + 5_000, nowMs: now) == .justNow)
    }
}
