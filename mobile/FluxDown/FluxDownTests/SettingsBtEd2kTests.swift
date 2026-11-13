import FluxDomain
import Foundation
import Testing
@testable import FluxDown

#if FULL_FLAVOR
@MainActor
struct SettingsBtEd2kTests {
    private func form(_ values: [String: String]) -> SettingsConfigForm {
        SettingsConfigForm(host: values.isEmpty ? [:] : values)
    }

    // MARK: 时长换算

    @Test func durationTextShowsValueInTheChosenUnit() {
        #expect(BtDuration.text(minutes: 0, unit: .hours).isEmpty)
        #expect(BtDuration.text(minutes: 90, unit: .minutes) == "90")
        #expect(BtDuration.text(minutes: 90, unit: .hours) == "1.5")
        #expect(BtDuration.text(minutes: 120, unit: .hours) == "2")
        #expect(BtDuration.text(minutes: 2_880, unit: .days) == "2")
    }

    @Test func durationParsesBackToMinutes() {
        #expect(BtDuration.minutes(from: "", unit: .days) == 0)
        #expect(BtDuration.minutes(from: "1.5", unit: .hours) == 90)
        #expect(BtDuration.minutes(from: "1,5", unit: .hours) == 90)
        #expect(BtDuration.minutes(from: "2", unit: .days) == 2_880)
        #expect(BtDuration.minutes(from: "abc", unit: .minutes) == nil)
        #expect(BtDuration.minutes(from: "-1", unit: .minutes) == nil)
    }

    @Test func switchingUnitKeepsTheTypedNumber() {
        // 输入框里的 “3” 从分钟切到小时：数字不变，分钟数按新单位重算。
        #expect(BtDuration.minutes(from: "3", unit: .minutes) == 3)
        #expect(BtDuration.minutes(from: "3", unit: .hours) == 180)
    }

    @Test func durationStepMovesByOneUnitAndStopsAtZero() {
        #expect(BtDuration.stepped(minutes: 60, unit: .hours, up: true) == 120)
        #expect(BtDuration.stepped(minutes: 30, unit: .hours, up: false) == 0)
        #expect(BtDuration.stepped(minutes: 0, unit: .days, up: false) == 0)
    }

    @Test func unknownUnitFallsBackToMinutes() {
        #expect(BtDurationUnit(wire: nil) == .minutes)
        #expect(BtDurationUnit(wire: "weeks") == .minutes)
        #expect(BtDurationUnit(wire: "days") == .days)
    }

    // MARK: 可见性

    @Test func seedingRowsHideWhenSeedingIsOff() {
        let off = form(["bt_seed_enabled": "false"])
        #expect(BtSettingsRow.visible(in: .seeding, off) == [.seedEnabled])
        let on = form(["bt_seed_enabled": "true"])
        #expect(BtSettingsRow.visible(in: .seeding, on).count == 9)
    }

    @Test func daemonRowsNeedALoadedConfig() {
        let unloaded = form([:])
        #expect(BtSettingsRow.allCases.allSatisfy { !$0.isVisible(in: unloaded) })
        #expect(Ed2kSettingsRow.allCases.allSatisfy { !$0.isVisible(in: unloaded) })
        let loaded = form(["bt_enable_dht": "true"])
        // 常规 = 总开关 + 新任务默认三行；监听 / 传输 / 来源 / 上限 / I2P 都在「连接」页签。
        #expect(BtSettingsRow.visible(in: .general, loaded) == [.enabled, .pieceOrder, .firstLastPiecesFirst, .selectionAutoDefault])
        // 默认 uTP 开（拥塞控制可见）、I2P 关（SAM 三行隐藏）。
        #expect(BtSettingsRow.visible(in: .connection, loaded) == [
            .portStart, .portEnd, .upnp,
            .utp, .utpCongestion, .mseMode, .uploadSlots,
            .dht, .lsd, .webseed,
            .maxConnections, .maxConnectionsPerTorrent,
            .i2pEnabled,
        ])
        #expect(BtSettingsRow.visible(in: .tracker, loaded).suffix(2) == [.announceAllTiers, .announceAllTrackers])
        #expect(Ed2kSettingsRow.visible(in: .servers, loaded).count == 4)
    }

    @Test func connectionRowsFollowTheirSwitches() {
        let utpOff = form(["bt_enable_utp": "false"])
        #expect(!BtSettingsRow.visible(in: .connection, utpOff).contains(.utpCongestion))
        let i2pOn = form(["bt_i2p_enabled": "true"])
        #expect(BtSettingsRow.visible(inSection: .i2p, i2pOn) == [.i2pEnabled, .i2pSamHost, .i2pSamPort, .i2pMixed])
        #expect(BtSettingsRow.showsPortMappingStatus(in: form(["bt_enable_upnp": "true"])))
        #expect(!BtSettingsRow.showsPortMappingStatus(in: form(["bt_enable_upnp": "false"])))
        #expect(!BtSettingsRow.showsPortMappingStatus(in: form(["bt_enabled": "false", "bt_enable_upnp": "true"])))
        #expect(!BtSettingsRow.showsPortMappingStatus(in: form([:])))
    }

    @Test func sectionsFollowGpuiOrder() {
        #expect(BtSettingsSection.all(in: .connection) == [.listen, .transport, .sources, .limits, .i2p])
        #expect(BtSettingsSection.all(in: .tracker) == [.trackers, .trackerSub, .announce])
        #expect(BtSettingsRow.allCases.allSatisfy { $0.section.tab == $0.tab })
    }

    @Test func newConnectionKeysAreDeviceLocalWithContractDefaults() {
        let defaults: [String: String] = [
            "bt_enable_lsd": "true", "bt_utp_congestion": "ledbat", "bt_max_connections": "0",
            "bt_max_connections_per_torrent": "512", "bt_enable_webseed": "true",
            "bt_announce_all_tiers": "true", "bt_announce_all_trackers": "true",
            "bt_proxy_enabled": "false", "bt_proxy_source": "manual", "bt_proxy_peers": "true", "bt_i2p_enabled": "false",
            "bt_i2p_sam_host": "127.0.0.1", "bt_i2p_sam_port": "7656", "bt_i2p_mixed": "false",
        ]
        for (key, value) in defaults {
            #expect(SettingsCatalog.defaultWire(key) == value, "\(key)")
            #expect(!SettingsCatalog.isSynced(key), "\(key)")
        }
        #expect(BtSettingsRow.maxConnections.range == 0 ... 65_535)
        #expect(BtSettingsRow.maxConnectionsPerTorrent.range == 1 ... 4_096)
        #expect(BtSettingsRow.i2pSamPort.range == 1 ... 65_535)
        #expect(BtUtpCongestion.allCases.map(\.rawValue) == ["ledbat", "cubic"])
    }

    @Test func btDisabledHidesEverythingButTheToggle() {
        let off = form(["bt_enabled": "false", "bt_seed_enabled": "true"])
        #expect(BtSettingsRow.visible(in: .general, off) == [.enabled])
        #expect(BtSettingsRow.visible(in: .tracker, off).isEmpty)
        #expect(BtSettingsRow.visible(in: .seeding, off).isEmpty)
        #expect(BtSettingsRow.availableTabs(in: off) == [.general])
        #expect(BtSettingsRow.readout(off) == nil)
        let on = form(["bt_enabled": "true"])
        #expect(BtSettingsRow.availableTabs(in: on) == BtSettingsTab.allCases)
        #expect(BtSettingsRow.visible(in: .general, on).first == .enabled)
        #expect(form(["bt_enable_dht": "true"]).bool("bt_enabled"))
    }

    @Test func rowKeysResolveInTheCatalogAndIdsAreUnique() {
        for row in BtSettingsRow.allCases { #expect(SettingsCatalog.field(row.configKey) != nil, "\(row)") }
        for row in Ed2kSettingsRow.allCases { #expect(SettingsCatalog.field(row.configKey) != nil, "\(row)") }
        let ids = BtSettingsRow.allCases.map(\.id) + Ed2kSettingsRow.allCases.map(\.id)
        #expect(Set(ids).count == ids.count)
        #expect(BtSettingsRow.seedTimeLimit.item.isSynced)
        #expect(SettingsCatalog.field("bt_seed_time_limit_unit") != nil)
        #expect(!SettingsCatalog.isSynced("bt_seed_time_limit_unit"))
    }

    @Test func searchTargetsMapToTheirTab() {
        #expect(BtSettingsRow.tab(forRowID: "bt.trackerSubUrls") == .tracker)
        #expect(BtSettingsRow.tab(forRowID: "bt.announceAllTiers") == .tracker)
        #expect(BtSettingsRow.tab(forRowID: "bt.portStart") == .connection)
        #expect(BtSettingsRow.tab(forRowID: BtSettingsRow.portMappingStatusID) == .connection)
        #expect(BtSettingsRow.tab(forRowID: "bt.seedThenAction") == .seeding)
        #expect(BtSettingsRow.tab(forRowID: "download.maxConcurrent") == nil)
        #expect(Ed2kSettingsRow.tab(forRowID: "ed2k.serverSubStatus") == .servers)
        #expect(Ed2kSettingsRow.tab(forRowID: "ed2k.nodesSubUrls") == .kad)
        #expect(Ed2kSettingsRow.tab(forRowID: "ed2k.idCheck") == .general)
    }

    @Test func portRangeRequiresEndAtLeastStart() {
        #expect(BtPortRange.isValid(start: 6881, end: 6891))
        #expect(BtPortRange.isValid(start: 7000, end: 7000))
        #expect(!BtPortRange.isValid(start: 7000, end: 6999))
    }

    // MARK: 订阅状态

    @Test func statusCountsCacheEntriesAndStoredTime() {
        let bt = form(["bt_tracker_sub_cache": "udp://a:1\n\nudp://b:2\n", "bt_tracker_sub_updated_at": "1759622400"])
        #expect(SubscriptionStatusModel.make(kind: .btTrackers, form: bt, fresh: nil) == .init(count: 2, updatedAt: 1_759_622_400))
        let ed2k = form(["ed2k_server_sub_cache": "1.1.1.1:1,2.2.2.2:2,3.3.3.3:3"])
        #expect(SubscriptionStatusModel.make(kind: .ed2kServers, form: ed2k, fresh: nil) == .init(count: 3, updatedAt: 0))
        let nodes = form(["ed2k_nodes_dat_count": "321", "ed2k_nodes_dat_updated_at": "1759622400"])
        #expect(SubscriptionStatusModel.make(kind: .ed2kNodes, form: nodes, fresh: nil) == .init(count: 321, updatedAt: 1_759_622_400))
        #expect(SubscriptionStatusModel.make(kind: .ed2kNodes, form: form(["ed2k_nodes_dat_count": "x"]), fresh: nil) == .init(count: 0, updatedAt: 0))
    }

    @Test func kadTabRowsFollowTheCatalog() {
        let f = form(["ed2k_nodes_dat_url": "https://a/nodes.dat\nhttps://b/nodes.dat", "ed2k_nodes_dat_count": "0"])
        #expect(Ed2kSettingsRow.visible(in: .kad, f) == [.nodesSubUrls, .nodesSubStatus])
        #expect(!Ed2kSettingsRow.nodesSubUrls.item.isSynced)
        #expect(Ed2kSettingsRow.kad.item.isSynced)
    }

    // MARK: ID 检测

    @Test func idCheckHighShowsSummaryWithoutHint() {
        let dto = Ed2kNetworkDto(id: .high, server: "1.2.3.4:4661", listenPort: 4662, upnp: .mapped)
        let expected = L("ed2kIdCheckResult", [
            "id": L("ed2kIdHigh"), "server": "1.2.3.4:4661", "port": "4662", "upnp": L("ed2kUpnpMapped"),
        ])
        #expect(Ed2kIdCheckDisplay.make(dto) == .ok(summary: expected, lowHint: nil))
    }

    @Test func idCheckLowAddsTheLowIdHint() {
        let dto = Ed2kNetworkDto(id: .low, server: "s:1", listenPort: 1, upnp: .failed)
        guard case let .ok(summary, hint) = Ed2kIdCheckDisplay.make(dto) else {
            Issue.record("expected ok")
            return
        }
        #expect(summary.contains(L("ed2kIdLow")) && summary.contains(L("ed2kUpnpFailed")))
        #expect(hint == L("ed2kIdLowHint"))
    }

    @Test func idCheckFailedUpnpShowsTheReason() {
        let dto = Ed2kNetworkDto(id: .high, server: "s:1", listenPort: 1, upnp: .failed, upnpError: " UPnP: no gateway ")
        let expected = L("ed2kIdCheckResult", [
            "id": L("ed2kIdHigh"), "server": "s:1", "port": "1", "upnp": "\(L("ed2kUpnpFailed")): UPnP: no gateway",
        ])
        #expect(Ed2kIdCheckDisplay.make(dto) == .ok(summary: expected, lowHint: nil))
    }

    @Test func idCheckNoneOrFailureShowsFailedWithDetail() {
        #expect(Ed2kIdCheckDisplay.make(Ed2kNetworkDto(id: .none, error: " all servers timed out ")) ==
            .failed("\(L("ed2kIdCheckFailed")): all servers timed out"))
        #expect(Ed2kIdCheckDisplay.make(Ed2kNetworkDto()) == .failed(L("ed2kIdCheckFailed")))
        #expect(Ed2kIdCheckDisplay.failure("") == .failed(L("ed2kIdCheckFailed")))
    }

    @Test func idCheckDecodesWithOmittedFields() throws {
        let dto = try JSONDecoder().decode(Ed2kNetworkDto.self, from: Data(#"{"id":"high","upnp":"weird","upnpEnabled":true}"#.utf8))
        #expect(dto.id == .high && dto.upnp == .disabled && dto.listenPort == 0 && dto.server.isEmpty && dto.upnpEnabled)
        #expect(dto.upnpError.isEmpty)
        let expected = L("ed2kIdCheckResult", [
            "id": L("ed2kIdHigh"), "server": "—", "port": "—", "upnp": L("ed2kUpnpDisabled"),
        ])
        #expect(Ed2kIdCheckDisplay.make(dto) == .ok(summary: expected, lowHint: nil))
        let empty = try JSONDecoder().decode(Ed2kNodesSubRefreshResponse.self, from: Data("{}".utf8))
        #expect(empty.outcome == SubscriptionRefreshOutcome(success: false, count: 0, okSources: 0, totalSources: 0, updatedAt: 0, error: ""))
    }

    @Test func freshSuccessWinsOnlyWhenNewerThanTheSnapshot() {
        let stored = form(["bt_tracker_sub_cache": "udp://a:1", "bt_tracker_sub_updated_at": "100"])
        let newer = SubscriptionRefreshOutcome(success: true, count: 50, okSources: 2, totalSources: 2, updatedAt: 200, error: "")
        #expect(SubscriptionStatusModel.make(kind: .btTrackers, form: stored, fresh: newer) == .init(count: 50, updatedAt: 200))
        let caughtUp = form(["bt_tracker_sub_cache": "a\nb", "bt_tracker_sub_updated_at": "200"])
        #expect(SubscriptionStatusModel.make(kind: .btTrackers, form: caughtUp, fresh: newer) == .init(count: 2, updatedAt: 200))
        let failed = SubscriptionRefreshOutcome(success: false, count: 0, okSources: 0, totalSources: 2, updatedAt: 300, error: "x")
        #expect(SubscriptionStatusModel.make(kind: .btTrackers, form: stored, fresh: failed) == .init(count: 1, updatedAt: 100))
    }

    @Test func subscriptionTimeIsRelativeWithinAWeekAndAbsoluteBeyond() {
        let now = Date(timeIntervalSince1970: 1_759_700_000)
        let recent = SubscriptionTime.text(unix: 1_759_700_000 - 3 * 3600, now: now, languageCode: "en")
        #expect(recent.contains("3 hours ago"))
        let old = SubscriptionTime.text(unix: 1_759_700_000 - 30 * 86_400, now: now, languageCode: "en")
        #expect(!old.contains("ago"))
        #expect(!old.isEmpty)
    }

    @Test func expiryTimeShowsFutureAsAbsoluteDateNotNow() {
        let now = Date(timeIntervalSince1970: 1_759_700_000)
        // 修复前走 text()：未来时间被夹成 now →「now / 0 秒前」，无法看出何时过期。
        let future = SubscriptionTime.expiryText(unix: 1_759_700_000 + 3 * 86_400, now: now, languageCode: "en")
        #expect(!future.contains("now") && !future.contains("ago") && !future.contains("in "))
        #expect(future.contains("2025"))
        #expect(future != SubscriptionTime.text(unix: 1_759_700_000 + 3 * 86_400, now: now, languageCode: "en"))
        let past = SubscriptionTime.expiryText(unix: 1_759_700_000 - 3 * 3600, now: now, languageCode: "en")
        #expect(past.contains("3 hours ago"))
    }

    // MARK: 读数

    @Test func btReadoutListsActiveFeatures() {
        #expect(BtSettingsRow.readout(form([:])) == nil)
        let f = form(["bt_enable_dht": "true", "bt_seed_enabled": "false", "bt_port_start": "7000", "bt_port_end": "7000"])
        #expect(BtSettingsRow.readout(f) == "DHT · 7000")
    }

    @Test func ed2kReadoutCountsDedupedServers() {
        let f = form([
            "ed2k_enable_kad": "true", "ed2k_server_list": "A:1,b:2", "ed2k_server_sub_cache": "a:1,c:3",
        ])
        #expect(Ed2kSettingsRow.readout(f)?.hasPrefix("Kad · ") == true)
        let none = form(["ed2k_enable_kad": "false", "ed2k_server_list": ""])
        #expect(Ed2kSettingsRow.readout(none) == nil)
    }
}
#endif
