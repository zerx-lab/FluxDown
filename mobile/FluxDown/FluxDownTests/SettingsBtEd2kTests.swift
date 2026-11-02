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
        #expect(BtSettingsRow.visible(in: .general, loaded).count == 7)
        #expect(Ed2kSettingsRow.visible(in: .servers, loaded).count == 4)
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
        #expect(BtSettingsRow.tab(forRowID: "bt.seedThenAction") == .seeding)
        #expect(BtSettingsRow.tab(forRowID: "download.maxConcurrent") == nil)
        #expect(Ed2kSettingsRow.tab(forRowID: "ed2k.serverSubStatus") == .servers)
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
