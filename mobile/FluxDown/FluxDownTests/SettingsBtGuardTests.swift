import FluxDomain
import Foundation
import Testing
@testable import FluxDown

#if FULL_FLAVOR
/// BT 防吸血页签的行目录、可见性、页签映射与搜索索引（纯逻辑；界面不做 GUI 测试）。
@MainActor
struct SettingsBtGuardTests {
    private func form(_ values: [String: String]) -> SettingsConfigForm {
        SettingsConfigForm(host: values)
    }

    private let btOn = ["bt_enabled": "true", "bt_guard_enabled": "true"]

    @Test func everyRowReadsAKeyThatExistsInTheCatalog() {
        for row in BtGuardRow.allCases {
            #expect(SettingsCatalog.field(row.configKey) != nil, "\(row)")
        }
        #expect(BtGuardRow.ruleRows.map(\.configKey) == [
            "bt_guard_rule_excessive", "bt_guard_rule_rerequest", "bt_guard_rule_rewind",
            "bt_guard_rule_lag", "bt_guard_rule_fingerprint",
        ])
        #expect(BtGuardRow.cloudMode.configKey == "bt_guard_cloud_mode")
    }

    @Test func rowIDsAreUniqueAndDoNotCollideWithTheBasicRows() {
        let ids = BtGuardRow.allCases.map(\.id) + BtSettingsRow.allCases.map(\.id)
        #expect(Set(ids).count == ids.count)
        #expect(BtGuardRow.allCases.allSatisfy { $0.id.hasPrefix("btGuard.") })
    }

    @Test func modeOptionsCoverTheCatalogEnum() {
        #expect(BtGuardRow.modeOptions.map(\.id) == SettingsCatalog.btGuardModes)
        for option in BtGuardRow.modeOptions { #expect(!option.labelKey.isEmpty) }
    }

    @Test func searchRowsLandOnTheAntiLeechTab() {
        for row in BtGuardRow.allCases {
            #expect(BtSettingsRow.tab(forRowID: row.id) == .antiLeech, "\(row)")
        }
        #expect(BtSettingsRow.tab(forRowID: BtSettingsRow.dht.id) == .general)
        #expect(BtSettingsRow.tab(forRowID: BtSettingsRow.seedRatio.id) == .seeding)
        #expect(BtSettingsRow.tab(forRowID: "btGuard.unknown") == nil)
    }

    @Test func rowsNeedALoadedConfigAndBtEnabled() {
        let unloaded = form([:])
        #expect(BtGuardRow.allCases.allSatisfy { !$0.isVisible(in: unloaded) })
        let on = form(btOn)
        #expect(BtGuardRow.visible(on) == BtGuardRow.allCases)
        // BT 总开关关闭：防吸血页签随 BT 页其余部分一起隐藏。
        let off = form(["bt_enabled": "false", "bt_guard_enabled": "true"])
        #expect(BtGuardRow.visible(off).isEmpty)
    }

    @Test func antiLeechTabIsOfferedOnlyWhileBtIsEnabled() {
        #expect(BtSettingsRow.availableTabs(in: form(btOn)).contains(.antiLeech))
        #expect(BtSettingsRow.availableTabs(in: form(["bt_enabled": "false"])) == [.general])
        #expect(BtSettingsTab.allCases.map(\.rawValue) == ["general", "tracker", "seeding", "antiLeech"])
        #expect(BtSettingsTab.antiLeech.titleKey == "btGuardTab")
        // SceneStorage 以 rawValue 持久化页签：旧值仍可解码。
        #expect(BtSettingsTab(rawValue: "seeding") == .seeding)
    }

    @Test func guardSwitchDefaultsOnAndGatesCloudMode() {
        // 目录默认：防吸血开启、云名单开启。
        #expect(BtGuardRow.isGuardEnabled(in: form(["bt_enabled": "true"])))
        #expect(BtGuardRow.cloudModeEditable(in: form(["bt_enabled": "true"])))
        #expect(!BtGuardRow.isGuardEnabled(in: form(["bt_enabled": "true", "bt_guard_enabled": "false"])))
        #expect(!BtGuardRow.cloudModeEditable(in: form(["bt_enabled": "true", "bt_guard_enabled": "false"])))
        #expect(!BtGuardRow.cloudModeEditable(in: form(["bt_enabled": "true", "bt_guard_cloud_enabled": "false"])))
    }

    @Test func searchEntriesIncludeGuardRowsWithTheAntiLeechBreadcrumb() {
        let ctx = SettingsSearchContext(form: form(btOn), isLocalHost: true, capabilities: [])
        let entries = BtPage.searchEntries(ctx)
        let ids = Set(entries.map(\.id))
        #expect(BtGuardRow.allCases.allSatisfy { ids.contains($0.id) })
        #expect(BtSettingsRow.allCases.filter { $0.isVisible(in: ctx.form) }.allSatisfy { ids.contains($0.id) })
        #expect(entries.filter { $0.id.hasPrefix("btGuard.") }.allSatisfy { $0.route == .bt })
        let unloaded = SettingsSearchContext(form: form([:]), isLocalHost: true, capabilities: [])
        #expect(BtPage.searchEntries(unloaded).isEmpty)
    }

    @Test func entryCountsSumEveryLayer() {
        #expect(BtGuardTab.totalEntries(BtGuardCounts()) == 0)
        #expect(BtGuardTab.totalEntries(BtGuardCounts(allow: 1, manual: 2, auto: 3, cloud: 40_000, file: 5)) == 40_011)
    }

    @Test func searchKeywordsAreAttachedToTheGuardEntries() {
        #expect(SettingsIndex.rowKeywordKeys["btGuard.enabled"] == ["btGuardKeywords"])
        #expect(SettingsIndex.rowKeywordKeys["btGuard.bans"] == ["btGuardKeywords"])
    }
}
#endif
