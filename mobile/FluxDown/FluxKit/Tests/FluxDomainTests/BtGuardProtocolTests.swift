import FluxDomain
import Foundation
import Testing

/// BT 防吸血：设置目录镜像、方法名 / 通知名、DTO 往返与纯逻辑（CIDR 校验、剩余时长、批量解除、云名单 / 上报状态）。
struct BtGuardProtocolTests {
    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try ProtocolJSON.makeDecoder().decode(type, from: Data(json.utf8))
    }

    private func encoded(_ value: some Encodable) throws -> [String: Any] {
        let data = try ProtocolJSON.makeEncoder().encode(value)
        let object = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        return try #require(object)
    }

    private func entry(_ id: Int64, layer: String = "manual", cidr: String = "1.2.3.4") -> BtGuardEntry {
        BtGuardEntry(id: id, layer: layer, cidr: cidr, action: "ban")
    }

    // MARK: - 设置目录

    private static let syncedGuardKeys = [
        "enabled", "rule_excessive", "rule_rerequest", "rule_rewind", "rule_lag", "rule_fingerprint", "cloud_mode",
        "excess_ratio_pct", "min_torrent_mib", "rewind_tolerance_pct", "throttle_kib", "ban_base_hours",
        "ban_max_days", "downloading_max_action", "skip_private", "skip_lan", "ipv6_prefix", "filter_urls",
        "cloud_enabled",
    ]

    @Test func catalogMirrorsTwentyGuardDaemonKeys() {
        let guardKeys = SettingsCatalog.daemonFields.filter { $0.key.hasPrefix("bt_guard_") }
        // 20 个 daemon 键：19 个同步 + 设备本地上报开关
        #expect(guardKeys.count == 20)
        #expect(Set(guardKeys.map(\.key)).count == 20)
    }

    @Test func nineteenGuardKeysSyncUnderTheirDottedNames() {
        let synced = SettingsCatalog.daemonSyncNames.filter { $0.key.hasPrefix("bt_guard_") }
        #expect(synced.count == 19)
        for suffix in Self.syncedGuardKeys {
            #expect(synced["bt_guard_" + suffix] == "bt.guard." + suffix, "\(suffix)")
            #expect(SettingsCatalog.syncNameToDaemonKey["bt.guard." + suffix] == "bt_guard_" + suffix)
        }
        #expect(!SettingsCatalog.isSynced("bt_guard_report_enabled")) // 设备本地：不进云同步
        #expect(SettingsCatalog.isSynced("bt_guard_rule_lag"))
    }

    @Test func guardSyncKeysAreInTheBtScopeGroup() throws {
        let found = SyncRules.groups.first { $0.id == .bt }
        let group = try #require(found)
        let expected = Self.syncedGuardKeys.map { "bt.guard." + $0 }
        #expect(expected.allSatisfy { group.keys.contains($0) })
        #expect(!group.keys.contains("bt.guard.report_enabled"))
    }

    @Test func defaultsMatchTheContract() {
        #expect(SettingsCatalog.defaultWire("bt_guard_enabled") == "true")
        for rule in ["excessive", "rerequest", "rewind", "lag", "fingerprint"] {
            #expect(SettingsCatalog.defaultWire("bt_guard_rule_" + rule) == "observe", "\(rule)")
        }
        #expect(SettingsCatalog.defaultWire("bt_guard_cloud_mode") == "observe")
        #expect(SettingsCatalog.defaultWire("bt_guard_excess_ratio_pct") == "200")
        #expect(SettingsCatalog.defaultWire("bt_guard_min_torrent_mib") == "64")
        #expect(SettingsCatalog.defaultWire("bt_guard_rewind_tolerance_pct") == "7")
        #expect(SettingsCatalog.defaultWire("bt_guard_throttle_kib") == "32")
        #expect(SettingsCatalog.defaultWire("bt_guard_ban_base_hours") == "24")
        #expect(SettingsCatalog.defaultWire("bt_guard_ban_max_days") == "30")
        #expect(SettingsCatalog.defaultWire("bt_guard_downloading_max_action") == "throttle")
        #expect(SettingsCatalog.defaultWire("bt_guard_skip_private") == "true")
        #expect(SettingsCatalog.defaultWire("bt_guard_skip_lan") == "true")
        #expect(SettingsCatalog.defaultWire("bt_guard_ipv6_prefix") == "64")
        #expect(SettingsCatalog.defaultWire("bt_guard_filter_urls").isEmpty)
        #expect(SettingsCatalog.defaultWire("bt_guard_cloud_enabled") == "true")
        #expect(SettingsCatalog.defaultWire("bt_guard_report_enabled") == "false")
    }

    private func accepted(_ key: String, _ value: String) -> String? {
        if case let .success(wire) = SettingsCatalog.normalize(key, value) { wire } else { nil }
    }

    @Test func enumAndRangeValidationMatchesTheDaemon() {
        #expect(accepted("bt_guard_rule_rewind", " enforce ") == "enforce")
        #expect(accepted("bt_guard_rule_rewind", "ban") == nil)
        #expect(accepted("bt_guard_cloud_mode", "") == nil)
        #expect(accepted("bt_guard_downloading_max_action", "ban") == "ban")
        #expect(accepted("bt_guard_downloading_max_action", "off") == nil)
        #expect(accepted("bt_guard_excess_ratio_pct", "110") == "110")
        #expect(accepted("bt_guard_excess_ratio_pct", "109") == nil)
        #expect(accepted("bt_guard_excess_ratio_pct", "1001") == nil)
        #expect(accepted("bt_guard_min_torrent_mib", "0") == "0")
        #expect(accepted("bt_guard_min_torrent_mib", "1048577") == nil)
        #expect(accepted("bt_guard_rewind_tolerance_pct", "101") == nil)
        #expect(accepted("bt_guard_throttle_kib", "0") == nil)
        #expect(accepted("bt_guard_ban_base_hours", "721") == nil)
        #expect(accepted("bt_guard_ban_max_days", "366") == nil)
        #expect(accepted("bt_guard_ipv6_prefix", "48") == "48")
        #expect(accepted("bt_guard_ipv6_prefix", "47") == nil)
        #expect(accepted("bt_guard_ipv6_prefix", "129") == nil)
        #expect(SettingsCatalog.field("bt_guard_cloud_cache") == nil) // 云缓存键已从协议移除
    }

    @Test func syncedGuardKeysRouteThroughPreferencesButReportFlagStaysOnDaemon() throws {
        let plan = try SettingsWritePlan.make([
            "bt_guard_rule_lag": "enforce", "bt_guard_report_enabled": "true", "bt_guard_ipv6_prefix": "56",
        ]).get()
        #expect(plan.daemon == ["bt_guard_report_enabled": "true"])
        #expect(plan.syncedPreferences == ["bt.guard.rule_lag": .string("enforce"), "bt.guard.ipv6_prefix": .int(56)])
    }

    // MARK: - 方法 / 通知名

    @Test func methodAndNoticeNamesMirrorRust() {
        #expect(HostMethod.daemonBtGuardState == "daemon.bt.guard.state")
        #expect(HostMethod.daemonBtGuardEntriesList == "daemon.bt.guard.entries.list")
        #expect(HostMethod.daemonBtGuardEntriesAdd == "daemon.bt.guard.entries.add")
        #expect(HostMethod.daemonBtGuardEntriesRemove == "daemon.bt.guard.entries.remove")
        #expect(HostMethod.daemonBtGuardEventsList == "daemon.bt.guard.events.list")
        #expect(HostMethod.daemonBtGuardExport == "daemon.bt.guard.export")
        #expect(HostMethod.daemonBtGuardFilterSourcesRefresh == "daemon.bt.guard.filterSources.refresh")
        #expect(HostMethod.daemonBtGuardCloudApply == "daemon.bt.guard.cloud.apply")
        #expect(HostMethod.daemonBtGuardEvidenceDrain == "daemon.bt.guard.evidence.drain")
        #expect(HostNoticeName.btGuardChanged == "btGuardChanged")
    }

    // MARK: - DTO

    @Test func stateDecodesFullPayload() throws {
        let state = try decode(BtGuardState.self, """
        {"enabled":true,"sessionActive":true,
         "stats":{"todayObserved":3,"todayEnforced":2,"todayBlockedConnections":5,"todayRefusedBytes":9007199254740993},
         "cloud":{"sequence":42,"updatedAt":1761900000,"expiresAt":1763100000,"entries":1200,"clientRules":7,
                  "stale":false,"rejectedReason":"rollback","attribution":["BTN-Collected-Rules by PBH-BTN, CC BY 4.0"]},
         "filterSources":[{"url":"https://e.x/a.txt","ok":false,"entries":0,"updatedAt":0,"error":"timeout"}],
         "counts":{"allow":1,"manual":2,"auto":3,"cloud":4,"file":5}}
        """)
        #expect(state.enabled && state.sessionActive)
        #expect(state.stats.todayRefusedBytes == 9_007_199_254_740_993) // 超过 2^53：u64 不经 Double 失真
        #expect(state.stats.todayBlockedConnections == 5)
        #expect(state.cloud.sequence == 42 && state.cloud.entries == 1200 && state.cloud.clientRules == 7)
        #expect(state.cloud.rejectedReason == "rollback" && state.cloud.lastError == nil)
        #expect(state.cloud.attribution.count == 1)
        #expect(state.filterSources.first?.error == "timeout")
        #expect(state.counts == BtGuardCounts(allow: 1, manual: 2, auto: 3, cloud: 4, file: 5))
    }

    @Test func stateToleratesSkippedSerdeFields() throws {
        // serde 对 None / 空 Vec 不下发键：缺省字段一律按默认值解码。
        let state = try decode(BtGuardState.self, """
        {"enabled":false,"sessionActive":false,"stats":{},"cloud":{"stale":true},"filterSources":[],"counts":{}}
        """)
        #expect(!state.enabled)
        #expect(state.cloud.stale && state.cloud.attribution.isEmpty && state.cloud.lastError == nil)
        #expect(state.stats == BtGuardStats())
        #expect(state.counts == BtGuardCounts())
    }

    @Test func entryDecodesOptionalAndSkippedFields() throws {
        let full = try decode(BtGuardEntry.self, """
        {"id":9,"layer":"auto","cidr":"1.2.3.4","action":"ban","rule":"excessive","reason":"x","source":"auto",
         "createdAt":100,"expiresAt":200,"remainingSecs":3700,"evidence":{"deliveredBytes":1048576,"ratioMilli":3700}}
        """)
        #expect(full.id == 9 && full.rule == "excessive" && full.remainingSecs == 3700 && full.expiresAt == 200)
        #expect(full.evidence?.objectValue?["ratioMilli"]?.intValue == 3700)

        let permanent = try decode(BtGuardEntry.self, """
        {"id":1,"layer":"manual","cidr":"10.0.0.0/24","action":"allow","createdAt":5}
        """)
        #expect(permanent.rule.isEmpty && permanent.reason.isEmpty && permanent.source.isEmpty)
        #expect(permanent.expiresAt == nil && permanent.remainingSecs == nil && permanent.evidence == nil)
    }

    @Test func pageDecodes() throws {
        let page = try decode(BtGuardEntriesPage.self, """
        {"total":250,"items":[{"id":1,"layer":"cloud","cidr":"1.1.1.0/24","action":"throttle","createdAt":1}]}
        """)
        #expect(page.total == 250 && page.items.count == 1 && page.items[0].id == 1)
    }

    @Test func listParamsOmitNilFields() throws {
        let json = try encoded(BtGuardEntriesListParams(layer: "cloud", offset: 100, limit: 100))
        #expect(json["layer"] as? String == "cloud")
        #expect(json["rule"] == nil && json["query"] == nil)
        #expect(json["offset"] as? Int == 100 && json["limit"] as? Int == 100)
    }

    @Test func addAndRemoveParamsEncodeCamelCase() throws {
        let add = try encoded(BtGuardEntriesAddParams(cidr: "1.2.3.4", action: "ban", durationSecs: 3600, reason: "r"))
        #expect(add["durationSecs"] as? Int == 3600 && add["cidr"] as? String == "1.2.3.4")
        let forever = try encoded(BtGuardEntriesAddParams(cidr: "1.2.3.4", action: "allow"))
        #expect(forever["durationSecs"] == nil) // 缺省 = 永久
        let remove = try encoded(BtGuardEntriesRemoveParams(ids: [3, 4]))
        #expect(remove["ids"] as? [Int] == [3, 4])
        let removed = try decode(BtGuardEntriesRemoveResult.self, #"{"removed":2}"#)
        #expect(removed.removed == 2)
    }

    @Test func exportAndRefreshResultsDecode() throws {
        let export = try decode(BtGuardExportResult.self, #"{"text":"1.2.3.4 # manual","lines":1}"#)
        #expect(export.lines == 1 && export.text.hasPrefix("1.2.3.4"))
        let refresh = try decode(BtGuardFilterSourcesRefresh.self, #"{"sources":[{"url":"u","ok":true,"entries":3,"updatedAt":9}]}"#)
        #expect(refresh.sources.first?.entries == 3 && refresh.sources.first?.ok == true)
        let none = try encoded(BtGuardExportParams())
        #expect(none["layer"] == nil)
    }

    // MARK: - CIDR

    @Test func cidrAcceptsAddressesAndPrefixes() {
        func ok(_ text: String) -> BtGuardRules.CidrCheck? {
            if case let .valid(cidr, family, prefix) = BtGuardRules.parseCidr(text) {
                return .valid(cidr: cidr, family: family, prefix: prefix)
            }
            return nil
        }
        #expect(ok("1.2.3.4") == .valid(cidr: "1.2.3.4", family: 4, prefix: nil))
        #expect(ok("  1.2.3.0/24 ") == .valid(cidr: "1.2.3.0/24", family: 4, prefix: 24))
        #expect(ok("0.0.0.0/0") == .valid(cidr: "0.0.0.0/0", family: 4, prefix: 0))
        #expect(ok("2001:db8::/64") == .valid(cidr: "2001:db8::/64", family: 6, prefix: 64))
        #expect(ok("::1") == .valid(cidr: "::1", family: 6, prefix: nil))
        #expect(ok("::") != nil)
        #expect(ok("::ffff:1.2.3.4") != nil)
        #expect(ok("2001:db8:0:0:0:0:0:1/128") == .valid(cidr: "2001:db8:0:0:0:0:0:1/128", family: 6, prefix: 128))
    }

    @Test func cidrRejectsMalformedInput() {
        #expect(BtGuardRules.parseCidr("") == .invalid(.empty))
        #expect(BtGuardRules.parseCidr("   ") == .invalid(.empty))
        for bad in ["1.2.3", "1.2.3.4.5", "256.1.1.1", "01.2.3.4", "a.b.c.d", "host", "fe80::1%en0",
                    ":::", "1::2::3", "12345::1", "1:2:3:4:5:6:7:8:9", "1:2:3:4:5:6:7"] {
            #expect(BtGuardRules.parseCidr(bad) == .invalid(.address), "\(bad)")
        }
        for bad in ["1.2.3.4/", "1.2.3.4/33", "1.2.3.4/-1", "1.2.3.4/x", "1.2.3.4/1000", "2001:db8::/129"] {
            #expect(BtGuardRules.parseCidr(bad) == .invalid(.prefix), "\(bad)")
        }
    }

    @Test func sameGroupTreatsHostPrefixAsOmitted() {
        #expect(BtGuardRules.sameGroup("1.2.3.4/32", "1.2.3.4"))
        #expect(BtGuardRules.sameGroup("1.2.3.4", "1.2.3.4/32"))
        #expect(BtGuardRules.sameGroup("2001:DB8::1/128", "2001:db8::1"))
        #expect(BtGuardRules.sameGroup("2001:db8::/64", " 2001:db8::/64 "))
        #expect(!BtGuardRules.sameGroup("1.2.3.0/24", "1.2.3.4"))
        #expect(!BtGuardRules.sameGroup("", ""))
    }

    // MARK: - 时长

    @Test func remainingPartsPicksTheLargestWholeUnit() {
        #expect(BtGuardRules.remainingParts(nil) == nil)
        #expect(BtGuardRules.remainingParts(0) == .init(unit: .seconds, n: 0))
        #expect(BtGuardRules.remainingParts(-5) == .init(unit: .seconds, n: 0))
        #expect(BtGuardRules.remainingParts(59) == .init(unit: .seconds, n: 59))
        #expect(BtGuardRules.remainingParts(60) == .init(unit: .minutes, n: 1))
        #expect(BtGuardRules.remainingParts(3599) == .init(unit: .minutes, n: 59))
        #expect(BtGuardRules.remainingParts(3600) == .init(unit: .hours, n: 1))
        #expect(BtGuardRules.remainingParts(86_399) == .init(unit: .hours, n: 23))
        #expect(BtGuardRules.remainingParts(86_400 * 3 + 5) == .init(unit: .days, n: 3))
        #expect(BtGuardRules.RemainingUnit.days.textKey == "btGuardDurationD")
        #expect(BtGuardRules.RemainingUnit.hours.textKey == "btGuardDurationH")
        #expect(BtGuardRules.RemainingUnit.minutes.textKey == "btGuardDurationM")
        #expect(BtGuardRules.RemainingUnit.seconds.textKey == "btGuardDurationS")
    }

    @Test func durationPresetsMapToSeconds() {
        #expect(BtGuardRules.DurationPreset.allCases.map(\.seconds) == [3600, 86_400, 604_800, nil])
    }

    // MARK: - 条目操作

    @Test func cloudEntriesCannotBeRemovedButCanBeWhitelisted() {
        #expect(!BtGuardRules.canRemove(entry(1, layer: "cloud")))
        #expect(BtGuardRules.canRemove(entry(1, layer: "file")))
        #expect(BtGuardRules.canRemove(entry(1, layer: "allow")))
        #expect(!BtGuardRules.canWhitelist(entry(1, layer: "allow")))
        #expect(BtGuardRules.canWhitelist(entry(1, layer: "cloud")))
        #expect(BtGuardRules.allowParams(cidr: "1.2.3.4") == BtGuardEntriesAddParams(cidr: "1.2.3.4", action: "allow"))
    }

    @Test func removableIDsSkipCloudAndKeepListOrder() {
        let list = [entry(1), entry(2, layer: "cloud"), entry(3, layer: "auto"), entry(4)]
        #expect(BtGuardRules.removableIDs(list, selected: [4, 2, 3]) == [3, 4])
        #expect(BtGuardRules.removableIDs(list, selected: []).isEmpty)
        #expect(BtGuardRules.removableIDs(list, selected: [99]).isEmpty)
    }

    @Test func chunkIDsRespectTheProtocolLimit() {
        #expect(BtGuardRules.chunkIDs([]).isEmpty)
        let ids = (1 ... 2500).map(Int64.init)
        let chunks = BtGuardRules.chunkIDs(ids)
        #expect(chunks.map(\.count) == [1000, 1000, 500])
        #expect(chunks.flatMap { $0 } == ids)
        #expect(BtGuardRules.chunkIDs([1, 2, 3], size: 2) == [[1, 2], [3]])
        #expect(BtGuardRules.chunkIDs([1], size: 0).isEmpty)
    }

    @Test func entriesQueryOmitsEmptyFiltersAndClampsPaging() {
        let all = BtGuardRules.entriesQuery(layer: nil, query: "  ", offset: -3)
        #expect(all == BtGuardEntriesListParams(layer: nil, rule: nil, query: nil, offset: 0, limit: 100))
        let filtered = BtGuardRules.entriesQuery(layer: "auto", rule: "lag", query: " 1.2 ", offset: 200, limit: 9999)
        #expect(filtered == BtGuardEntriesListParams(layer: "auto", rule: "lag", query: "1.2", offset: 200, limit: 500))
        #expect(BtGuardRules.entriesQuery(layer: nil, offset: 0, limit: 0).limit == 1)
    }

    @Test func mergePageDeduplicatesById() {
        let merged = BtGuardRules.mergePage([entry(1), entry(2)], [entry(2, cidr: "9.9.9.9"), entry(3)])
        #expect(merged.map(\.id) == [1, 2, 3])
        #expect(merged[1].cidr == "1.2.3.4") // 先到先得
    }

    // MARK: - 云名单 / 上报

    @Test func cloudStatusPriority() {
        func kind(_ cloud: BtGuardCloudStatus, enabled: Bool = true) -> BtGuardRules.CloudStatusKind {
            BtGuardRules.cloudStatusKind(cloud, enabled: enabled)
        }
        #expect(kind(BtGuardCloudStatus(sequence: 5, rejectedReason: "x"), enabled: false) == .disabled)
        #expect(kind(BtGuardCloudStatus()) == .none)
        #expect(kind(BtGuardCloudStatus(rejectedReason: "bad sig")) == .rejected)
        #expect(kind(BtGuardCloudStatus(lastError: "net")) == .error)
        #expect(kind(BtGuardCloudStatus(lastError: "e", rejectedReason: "r")) == .rejected)
        #expect(kind(BtGuardCloudStatus(sequence: 5, stale: true, lastError: "e", rejectedReason: "r")) == .stale)
        #expect(kind(BtGuardCloudStatus(sequence: 5, lastError: "e", rejectedReason: "r")) == .rejected)
        #expect(kind(BtGuardCloudStatus(sequence: 5, lastError: "e")) == .error)
        #expect(kind(BtGuardCloudStatus(sequence: 5)) == .ok)
        // 空串视为无
        #expect(kind(BtGuardCloudStatus(sequence: 5, lastError: "", rejectedReason: "")) == .ok)
    }

    @Test func reportGateRequiresLoginOnlyToTurnOn() {
        #expect(BtGuardRules.reportNeedsLogin(loggedIn: false))
        #expect(!BtGuardRules.reportNeedsLogin(loggedIn: true))
        #expect(!BtGuardRules.reportToggleEnabled(enabled: false, loggedIn: false))
        #expect(BtGuardRules.reportToggleEnabled(enabled: true, loggedIn: false)) // 已开启时始终可关
        #expect(BtGuardRules.reportToggleEnabled(enabled: false, loggedIn: true))
        #expect(BtGuardRules.reportActive(enabled: true, loggedIn: true))
        #expect(!BtGuardRules.reportActive(enabled: true, loggedIn: false))
        #expect(!BtGuardRules.reportActive(enabled: false, loggedIn: true))
    }

    @Test func statusKeyFollowsEnabledThenState() {
        #expect(BtGuardRules.statusKey(enabled: false, state: nil) == "btGuardStatusDisabled")
        #expect(BtGuardRules.statusKey(enabled: true, state: nil) == "btGuardLoading")
        #expect(BtGuardRules.statusKey(enabled: true, state: BtGuardState(sessionActive: true)) == "btGuardSessionOn")
        #expect(BtGuardRules.statusKey(enabled: true, state: BtGuardState(sessionActive: false)) == "btGuardSessionOff")
    }

    @Test func labelKeysCapitalizeTheWireValue() {
        #expect(BtGuardRules.labelKey(prefix: "btGuardLayer", value: "allow") == "btGuardLayerAllow")
        #expect(BtGuardRules.labelKey(prefix: "btGuardAction", value: "banned") == "btGuardActionBanned")
        #expect(BtGuardRules.labelKey(prefix: "btGuardRule", value: "fingerprint") == "btGuardRuleFingerprint")
        #expect(BtGuardRules.labelKey(prefix: "btGuardRule", value: "") == nil)
        // 每个已知取值都能得到文案键（键存在性由 scripts/check-i18n.py 对 L(...) 字面量校验，动态键在此枚举）。
        let layers = BtGuardLayer.all.compactMap { BtGuardRules.labelKey(prefix: "btGuardLayer", value: $0) }
        #expect(layers == ["btGuardLayerAllow", "btGuardLayerManual", "btGuardLayerAuto", "btGuardLayerCloud", "btGuardLayerFile"])
        let actions = BtGuardEntryAction.all.compactMap { BtGuardRules.labelKey(prefix: "btGuardAction", value: $0) }
        #expect(actions == ["btGuardActionAllow", "btGuardActionBan", "btGuardActionChoke", "btGuardActionThrottle"])
        let rules = BtGuardRule.all.compactMap { BtGuardRules.labelKey(prefix: "btGuardRule", value: $0) }
        #expect(rules.count == 6 && rules.last == "btGuardRuleCloud")
    }

    @Test func entryRulesIncludeManualAndFileLabels() {
        // 条目的 rule 除六条检测规则外还有 manual / file（手动封禁 / 过滤文件条目）；未知取值不得得到空键。
        #expect(BtGuardRules.labelKey(prefix: "btGuardRule", value: "manual") == "btGuardRuleManual")
        #expect(BtGuardRules.labelKey(prefix: "btGuardRule", value: "file") == "btGuardRuleFile")
        #expect(BtGuardRule.entryValues == BtGuardRule.all + ["manual", "file"])
    }

    @Test func cloudProblemKeysMatchTheStatusKinds() {
        #expect(BtGuardRules.cloudProblemKey(.ok) == nil)
        #expect(BtGuardRules.cloudProblemKey(.disabled) == "btGuardCloudDisabled")
        #expect(BtGuardRules.cloudProblemKey(.none) == "btGuardCloudNone")
        #expect(BtGuardRules.cloudProblemKey(.stale) == "btGuardCloudStale")
        #expect(BtGuardRules.cloudProblemKey(.rejected) == "btGuardCloudRejected")
        #expect(BtGuardRules.cloudProblemKey(.error) == "btGuardCloudLastError")
        #expect(BtGuardRules.cloudProblemIsAlert(.rejected) && BtGuardRules.cloudProblemIsAlert(.error))
        #expect(BtGuardRules.cloudProblemIsAlert(.stale))
        #expect(!BtGuardRules.cloudProblemIsAlert(.disabled) && !BtGuardRules.cloudProblemIsAlert(.none))
    }
}
