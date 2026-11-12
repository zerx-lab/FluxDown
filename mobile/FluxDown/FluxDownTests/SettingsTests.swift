import FluxDomain
import Foundation
import Testing
@testable import FluxDown

// MARK: - 限速

@MainActor
struct SettingsRateLimitTests {
    @Test func unitFactorsAreBase1024() {
        #expect(SettingsRateUnit.kb.factor == 1024)
        #expect(SettingsRateUnit.mb.factor == 1_048_576)
        #expect(SettingsRateUnit.gb.factor == 1_073_741_824)
    }

    @Test func textTrimsToTwoDecimals() {
        #expect(SettingsRateLimit.text(bytes: 0, unit: .mb) == "")
        #expect(SettingsRateLimit.text(bytes: -5, unit: .mb) == "")
        #expect(SettingsRateLimit.text(bytes: 10 * 1_048_576, unit: .mb) == "10")
        #expect(SettingsRateLimit.text(bytes: 1_572_864, unit: .mb) == "1.5")
        #expect(SettingsRateLimit.text(bytes: 512 * 1024, unit: .kb) == "512")
        #expect(SettingsRateLimit.text(bytes: 100 * 1_048_576, unit: .mb) == "100")
        #expect(SettingsRateLimit.text(bytes: 1, unit: .gb) == "0")
    }

    @Test func parseRoundsAndValidates() {
        let mb: Int64 = 1_048_576
        func parsed(_ text: String, _ unit: SettingsRateUnit) -> Int64? { SettingsRateLimit.parse(text, unit: unit) }
        let tenMB: Int64? = 10 * mb
        let onePointFiveMB: Int64? = 1_572_864
        let halfGB: Int64? = 536_870_912
        let zero: Int64? = 0
        let maxed: Int64? = Int64.max
        #expect(parsed("10", .mb) == tenMB)
        #expect(parsed("1,5", .mb) == onePointFiveMB)
        #expect(parsed(" 0.5 ", .gb) == halfGB)
        #expect(parsed("", .mb) == zero)
        #expect(parsed("-3", .mb) == nil)
        #expect(parsed("abc", .kb) == nil)
        #expect(parsed("1e400", .kb) == nil)
        #expect(parsed("99999999999999999999", .gb) == maxed)
    }

    @Test func switchingUnitKeepsTheNumber() {
        // 「10」+ MB/s → 10 MB/s；切到 KB/s 后同一串数字即 10 KB/s。
        let tenKB: Int64? = 10 * 1024
        #expect(SettingsRateLimit.parse("10", unit: .kb) == tenKB)
    }

    @Test func effectiveUnitFallsBackToExactUnit() {
        let oneMB: Int64 = 1_048_576
        #expect(SettingsRateLimit.effectiveUnit(bytes: 0, preferred: .gb) == .gb)
        #expect(SettingsRateLimit.effectiveUnit(bytes: 5 * oneMB, preferred: .mb) == .mb)
        // 512 KB 在 MB 下是 0.5，仍可精确表示。
        #expect(SettingsRateLimit.effectiveUnit(bytes: 512 * 1024, preferred: .mb) == .mb)
        // 1 KB 在 MB 下 = 0.000976… 不可精确表示 → 回落到能精确表示的 KB。
        #expect(SettingsRateLimit.effectiveUnit(bytes: 1024, preferred: .mb) == .kb)
        // 1 GB 在 GB 之外的单位也能表示，首选 MB 时保持 MB（1024 MB）。
        #expect(SettingsRateLimit.effectiveUnit(bytes: 1_073_741_824, preferred: .mb) == .mb)
        // 1000 字节没有任何单位能两位小数精确表示 → KB/s 兜底。
        #expect(SettingsRateLimit.effectiveUnit(bytes: 1000, preferred: .mb) == .kb)
    }

    @Test func sanitizeKeepsDigitsAndOneDot() {
        #expect(SettingsRateLimit.sanitize("1a2.3.4") == "12.34")
        #expect(SettingsRateLimit.sanitize("-5") == "5")
        #expect(SettingsRateLimit.sanitize("1,5") == "1.5")
        #expect(SettingsRateLimit.sanitize("1234567890123456").count == 12)
    }

    @Test func steppingUsesUnitStepAndFloorsAtZero() {
        #expect(SettingsRateLimit.stepped(bytes: 0, unit: .kb, up: true) == 64 * 1024)
        #expect(SettingsRateLimit.stepped(bytes: 64 * 1024, unit: .kb, up: false) == 0)
        #expect(SettingsRateLimit.stepped(bytes: 0, unit: .mb, up: false) == 0)
        #expect(SettingsRateLimit.stepped(bytes: 2 * 1_048_576, unit: .mb, up: true) == 3 * 1_048_576)
    }

    @Test func presetsMatchSpec() {
        let values = SettingsRateLimit.presets.map { SettingsRateLimit.bytes(amount: $0.amount, unit: $0.unit) }
        let mb: Int64 = 1_048_576
        let expected: [Int64] = [0, 512 * 1024, mb, 2 * mb, 5 * mb, 10 * mb, 20 * mb]
        #expect(values == expected)
    }
}

// MARK: - 数值 / 配置视图 / 目录

@MainActor
struct SettingsValueTests {
    @Test func clampReportsAdjustment() {
        #expect(SettingsNumber.clamp(5, to: 1 ... 10) == (5, false))
        #expect(SettingsNumber.clamp(0, to: 1 ... 1024) == (1, true))
        #expect(SettingsNumber.clamp(5000, to: 1 ... 1024) == (1024, true))
        #expect(SettingsNumber.clamp(-9, to: -1 ... 20) == (-1, true))
    }

    @Test func numberParsing() {
        #expect(SettingsNumber.parse(" 42 ") == 42)
        #expect(SettingsNumber.parse("-1") == -1)
        #expect(SettingsNumber.parse("\u{2212}1") == -1)
        #expect(SettingsNumber.parse("") == nil)
        #expect(SettingsNumber.parse("4x") == nil)
        #expect(SettingsNumber.parse("-") == nil)
        #expect(SettingsNumber.parse("99999999999999999999999") == Int.max)
        #expect(SettingsNumber.parse("-99999999999999999999999") == Int.min)
    }

    @Test func formPrefersOptimisticValues() {
        let form = SettingsConfigForm(host: ["a": "1", "b": "true", "c": "x"], optimistic: ["a": "7", "d": "1"])
        #expect(form.int("a", default: 0) == 7)
        #expect(form.has("a") && !form.has("d"))
        #expect(form.bool("b") && form.bool("d") && !form.bool("c") && !form.bool("missing"))
        #expect(form.int("c", default: 3) == 3)
        #expect(form.long("missing", default: 9) == 9)
        #expect(SettingsConfigForm.wire(true) == "true" && SettingsConfigForm.wire(false) == "false")
    }

    @Test func saveDirectoryValidation() {
        #expect(SettingsSaveDirectory.isValid(""))
        #expect(SettingsSaveDirectory.isValid("/srv/downloads"))
        #expect(SettingsSaveDirectory.isValid("C:\\Users\\me"))
        #expect(SettingsSaveDirectory.isValid("D:/dl"))
        #expect(SettingsSaveDirectory.isValid("\\\\nas\\share"))
        #expect(!SettingsSaveDirectory.isValid("downloads"))
        #expect(!SettingsSaveDirectory.isValid("~/dl"))
        #expect(SettingsSaveDirectory.lastComponent("/a/b/") == "b")
        #expect(SettingsSaveDirectory.lastComponent("C:\\x\\y") == "y")
        #expect(SettingsSaveDirectory.lastComponent("") == "")
    }

    @Test func versionDisplay() {
        #expect(SettingsAppVersion.display(short: "1.2.0", build: "42") == "1.2.0 (42)")
        #expect(SettingsAppVersion.display(short: "1.2.0", build: "1.2.0") == "1.2.0")
        #expect(SettingsAppVersion.display(short: "1.0", build: nil) == "1.0")
        #expect(SettingsAppVersion.display(short: nil, build: "3") == "")
    }
}

// MARK: - 行可见性与搜索

@MainActor
struct SettingsRowVisibilityTests {
    private func context(_ host: [String: String], prefs: [String: JSONValue] = [:], local: Bool = false) -> SettingsDownloadContext {
        SettingsDownloadContext(form: SettingsConfigForm(host: host, prefs: prefs), isLocalHost: local)
    }

    @Test func daemonRowsNeedLoadedConfigAndFallBackToCatalogDefaults() {
        let ctx = context(["max_concurrent_tasks": "5"])
        #expect(SettingsDownloadRow.maxConcurrent.isVisible(in: ctx))
        // 配置已加载：目录内的键缺省取默认值，行照常渲染（同 Web `useDaemonValue`）。
        #expect(SettingsDownloadRow.speedLimit.isVisible(in: ctx))
        #expect(ctx.form.long("speed_limit_bytes", default: -1) == 0)
        #expect(ctx.form.int("max_auto_retries", default: 0) == 3)
        // 配置未加载（连接前）：daemon 行一律不渲染。
        let loading = context([:])
        #expect(!SettingsDownloadRow.maxConcurrent.isVisible(in: loading))
        #expect(SettingsDownloadRow.visible(in: .retry, loading).isEmpty)
    }

    @Test func preferenceRowsAreAlwaysAvailableWithCatalogDefaults() {
        let ctx = context([:])
        #expect(SettingsDownloadRow.rememberLastSaveDir.isVisible(in: ctx))
        #expect(SettingsDownloadRow.silentDownload.isVisible(in: ctx))
        #expect(!ctx.form.bool("download.silent_download"))
        #expect(ctx.form.bool("download.silent_auto_category")) // 默认开
        #expect(ctx.form.bool("download.notify_on_complete")) // 默认开
    }

    @Test func conditionalRows() {
        let auto = context(["default_segments": "0", "auto_max_connections": "16"])
        #expect(SettingsDownloadRow.autoMaxConnections.isVisible(in: auto))
        let manual = context(["default_segments": "8", "auto_max_connections": "16"])
        #expect(!SettingsDownloadRow.autoMaxConnections.isVisible(in: manual))

        let cdnOn = context(["cdn_multi_enabled": "true", "cdn_max_nodes": "0"])
        #expect(SettingsDownloadRow.cdnMaxNodes.isVisible(in: cdnOn))
        let cdnOff = context(["cdn_multi_enabled": "false", "cdn_max_nodes": "0"])
        #expect(!SettingsDownloadRow.cdnMaxNodes.isVisible(in: cdnOff))

        // 免打扰子开关读偏好 `download.silent_download`（不是 daemon 配置键）。
        let silent = context(["k": "1"], prefs: ["download.silent_download": .bool(true)])
        #expect(SettingsDownloadRow.silentAutoCategory.isVisible(in: silent))
        #expect(SettingsDownloadRow.silentSkipSelection.isVisible(in: silent))
        let loud = context(["k": "1"], prefs: ["download.silent_download": .bool(false)])
        #expect(!SettingsDownloadRow.silentAutoCategory.isVisible(in: loud))
        #expect(!SettingsDownloadRow.silentSkipSelection.isVisible(in: loud))
    }

    @Test func idleScanHiddenOnLocalHost() {
        let host = ["idle_file_scan": "false"]
        #expect(!SettingsDownloadRow.idleFileScan.isVisible(in: context(host, local: true)))
        #expect(SettingsDownloadRow.idleFileScan.isVisible(in: context(host, local: false)))
    }

    /// 与 `crates/settings/tests/fixtures/retention_cases.json` 同一组用例（GPUI / Web / Android 共用语义）。
    @Test func retentionModeFollowsSharedCases() {
        #expect(SettingsRetention.presets == [0, 1, 7, 30])
        #expect(SettingsRetention.customStart == 14)
        let cases: [(days: Int, flag: Bool, expect: String)] = [
            (0, false, "0"), (1, false, "1"), (7, false, "7"), (30, false, "30"),
            (14, false, "custom"), (365, false, "custom"), (7, true, "custom"), (0, true, "0"),
        ]
        for c in cases {
            #expect(SettingsRetention.modeID(days: c.days, customFlag: c.flag) == c.expect, "days=\(c.days)")
        }
        let targets: [(current: Int, expect: Int)] = [(0, 14), (7, 14), (30, 14), (90, 90), (99_999, 3650)]
        for t in targets {
            #expect(SettingsRetention.customTarget(current: t.current, max: 3650) == t.expect, "current=\(t.current)")
        }
    }

    @Test func retentionRowIsBehaviorRowBoundToCatalogKey() {
        let row = SettingsDownloadRow.completedRecordRetention
        #expect(row.group == .behavior)
        #expect(row.configKey == "completed_record_retention_days")
        #expect(!row.item.isSynced)
        #expect(row.isVisible(in: context(["completed_record_retention_days": "7"])))
        #expect(!row.isVisible(in: context([:])))
    }

    @Test func rowIdsAreUniqueAndKeysAreInTheCatalog() {
        let ids = SettingsDownloadRow.allCases.map(\.id)
        #expect(Set(ids).count == ids.count)
        for row in SettingsDownloadRow.allCases {
            #expect(SettingsCatalog.field(row.configKey) != nil, "\(row.configKey)")
        }
    }

    @Test func syncMarkerFollowsTheCatalog() {
        #expect(SettingsDownloadRow.maxConcurrent.item.isSynced)
        #expect(SettingsDownloadRow.rememberLastSaveDir.item.isSynced)
        #expect(!SettingsDownloadRow.uploadLimit.item.isSynced)
        #expect(!SettingsDownloadRow.silentAutoCategory.item.isSynced)
        #expect(!SettingsDownloadRow.silentSkipSelection.item.isSynced)
        #expect(!SettingsDownloadRow.saveDir.item.isSynced)
    }

    @Test func searchRanksTitleBeforeDetailAndIgnoresCase() {
        func entry(_ id: String, _ title: String, _ detail: String) -> SettingsEntry {
            SettingsEntry(id: id, route: .download, title: title, detail: detail, breadcrumb: "", symbol: "gear")
        }
        let entries = [
            entry("a", "Retry Interval", "Seconds to wait before each speed check"),
            entry("b", "Download Limit", "Limit download speed"),
            entry("c", "Résumé", ""),
        ]
        #expect(SettingsSearch.filter(entries, query: "SPEED").map(\.id) == ["a", "b"])
        #expect(SettingsSearch.filter(entries, query: "limit").map(\.id) == ["b"])
        #expect(SettingsSearch.filter(entries, query: "resume").map(\.id) == ["c"])
        #expect(SettingsSearch.filter(entries, query: "   ").isEmpty)
        #expect(SettingsSearch.filter(entries, query: "zzz").isEmpty)
    }

    @Test func downloadSearchEntriesFollowVisibility() {
        let ctx = SettingsSearchContext(
            form: SettingsConfigForm(host: ["default_segments": "8"], prefs: [:]), isLocalHost: true, capabilities: []
        )
        let ids = DownloadPage.searchEntries(ctx).map(\.id)
        #expect(ids.contains("download.maxConcurrent"))
        #expect(!ids.contains("download.autoMaxConnections")) // 线程数 ≠ 0
        #expect(!ids.contains("download.idleFileScan")) // 本机隐藏
        #expect(!ids.contains("download.silentAutoCategory")) // 免打扰关
        #expect(!ids.contains("download.silentSkipSelection")) // 免打扰关
    }
}

// MARK: - ConfigEditor

@MainActor
final class FakeConfigTransport: ConfigTransport {
    var hostID = "local"
    var config: [String: String]
    var configRevision: UInt64 = 1
    var preferences = AgentPreferencesDto(revision: 1)
    var isReadOnly = false
    private(set) var patches: [(revision: UInt64, values: [String: String])] = []
    private(set) var prefPatches: [(values: [String: JSONValue], sync: Bool)] = []
    private(set) var reports: [String] = []
    /// 依次抛出的错误；空 = 成功。`.conflict` 模拟「主机已推进 revision」。
    var pendingErrors: [HostError] = []
    var pendingPrefErrors: [HostError] = []

    init(config: [String: String], prefs: [String: JSONValue] = [:]) {
        self.config = config
        preferences.values = prefs
    }

    func patchConfig(expectedRevision: UInt64, values: [String: String]) async throws(HostError) {
        patches.append((expectedRevision, values))
        if !pendingErrors.isEmpty {
            let error = pendingErrors.removeFirst()
            if error.code == .conflict { configRevision += 1 }
            throw error
        }
        config.merge(values) { _, new in new }
        configRevision += 1
    }

    /// 镜像 agent：同步的 daemon 键由 agent 代写 daemon（`mark_local` → `patch_daemon`）。
    func patchPreferences(_ values: [String: JSONValue], sync: Bool) async throws(HostError) -> UInt64 {
        prefPatches.append((values, sync))
        if !pendingPrefErrors.isEmpty { throw pendingPrefErrors.removeFirst() }
        for (name, json) in values {
            if sync, let daemonKey = SettingsCatalog.syncNameToDaemonKey[name], let wire = SettingsCatalog.wire(fromJSON: json) {
                config[daemonKey] = wire
                configRevision += 1
            } else if json == .null {
                preferences.values[name] = nil
            } else {
                preferences.values[name] = json
            }
        }
        preferences.revision += 1
        return preferences.revision
    }

    func fetchConfigRevision() async -> UInt64? { configRevision }

    func report(_ message: String) { reports.append(message) }
}

@MainActor
struct ConfigEditorTests {
    private func makeEditor(_ transport: FakeConfigTransport) -> ConfigEditor {
        ConfigEditor(transport: transport, disconnectedText: "offline") { "failed:\($0.code.rawValue)" }
    }

    // MARK: daemon.config.patch（非同步键）

    @Test func debouncesRapidEditsIntoOnePatch() async {
        let transport = FakeConfigTransport(config: ["upload_limit_bytes": "0"])
        let editor = makeEditor(transport)
        editor.set("upload_limit_bytes", "1")
        editor.set("upload_limit_bytes", "2")
        editor.set("upload_limit_bytes", "3")
        #expect(editor.form.value("upload_limit_bytes") == "3") // 乐观值立即可见
        #expect(transport.patches.isEmpty)
        await editor.settle()
        #expect(transport.patches.count == 1)
        #expect(transport.patches.first?.values == ["upload_limit_bytes": "3"])
        #expect(transport.patches.first?.revision == 1)
        #expect(transport.prefPatches.isEmpty)
        #expect(editor.optimistic.isEmpty)
        #expect(transport.config["upload_limit_bytes"] == "3")
    }

    @Test func ignoresNoOpEdits() async {
        let transport = FakeConfigTransport(config: ["bt_port_start": "6881"])
        let editor = makeEditor(transport)
        editor.set("bt_port_start", "6881")
        editor.set("bt_enable_dht", "1") // 缺省 true 的默认值（配置已加载）
        await editor.settle()
        #expect(transport.patches.isEmpty && transport.prefPatches.isEmpty)
    }

    @Test func conflictReplaysWithFreshRevision() async {
        let transport = FakeConfigTransport(config: ["bt_port_start": "6881"])
        transport.pendingErrors = [HostError(.conflict)]
        let editor = makeEditor(transport)
        editor.set("bt_port_start", "7000")
        await editor.settle()
        #expect(transport.patches.count == 2)
        #expect(transport.patches[0].revision == 1)
        #expect(transport.patches[1].revision == 2)
        #expect(transport.config["bt_port_start"] == "7000")
        #expect(transport.reports.isEmpty)
        #expect(editor.failures.isEmpty)
    }

    @Test func conflictsRetryUpToThreeTimesThenRollBack() async {
        let transport = FakeConfigTransport(config: ["bt_port_start": "6881"])
        transport.pendingErrors = Array(repeating: HostError(.conflict), count: 4)
        let editor = makeEditor(transport)
        editor.set("bt_port_start", "7000")
        await editor.settle()
        #expect(transport.patches.count == 4) // 首发 + 3 次重放
        #expect(editor.optimistic.isEmpty)
        #expect(editor.form.value("bt_port_start") == "6881")
        #expect(editor.failures["bt_port_start"] == "failed:conflict")
        #expect(transport.reports == ["failed:conflict"])
    }

    @Test func threeConflictsStillSucceedOnTheFourthAttempt() async {
        let transport = FakeConfigTransport(config: ["bt_port_start": "6881"])
        transport.pendingErrors = Array(repeating: HostError(.conflict), count: 3)
        let editor = makeEditor(transport)
        editor.set("bt_port_start", "7000")
        await editor.settle()
        #expect(transport.patches.count == 4)
        #expect(transport.config["bt_port_start"] == "7000")
        #expect(transport.reports.isEmpty)
    }

    @Test func failureRollsBackWithInlineMessage() async {
        let transport = FakeConfigTransport(config: ["bt_port_start": "6881"])
        transport.pendingErrors = [HostError(.invalidArgument)]
        let editor = makeEditor(transport)
        editor.set("bt_port_start", "7000")
        await editor.settle()
        #expect(transport.patches.count == 1)
        #expect(editor.form.value("bt_port_start") == "6881")
        #expect(editor.failures["bt_port_start"] == "failed:invalidArgument")
        #expect(transport.reports == ["failed:invalidArgument"])
    }

    // MARK: agent.preferences.patch（同步 daemon 键 / 偏好键）

    @Test func syncedDaemonKeysAreWrittenThroughPreferencesUnderTheirSyncName() async {
        let transport = FakeConfigTransport(config: ["max_concurrent_tasks": "5", "bt_seed_ratio_limit": "0"])
        let editor = makeEditor(transport)
        editor.set("max_concurrent_tasks", "8")
        editor.set("bt_seed_ratio_limit", "1.5")
        await editor.settle()
        #expect(transport.patches.isEmpty) // 绝不走 daemon.config.patch
        #expect(transport.prefPatches.count == 2)
        let merged = transport.prefPatches.reduce(into: [String: JSONValue]()) { $0.merge($1.values) { _, new in new } }
        #expect(merged == ["download.max_concurrent_tasks": .int(8), "bt.seed_ratio_limit": .double(1.5)])
        #expect(transport.prefPatches.allSatisfy { $0.sync }) // 目录内：同步
        #expect(transport.config["max_concurrent_tasks"] == "8") // agent 代写 daemon
        #expect(editor.optimistic.isEmpty && transport.reports.isEmpty)
    }

    @Test func localPreferencesCarrySyncFalse() async {
        let transport = FakeConfigTransport(config: [:])
        let editor = makeEditor(transport)
        editor.set("download.silent_skip_selection", "true")
        editor.set("analytics_enabled", "false")
        await editor.settle()
        #expect(transport.patches.isEmpty)
        #expect(transport.prefPatches.count == 2)
        #expect(transport.prefPatches.allSatisfy { !$0.sync })
        #expect(transport.preferences.values["download.silent_skip_selection"] == .bool(true))
        #expect(transport.preferences.values["analytics_enabled"] == .bool(false))
    }

    @Test func syncedPreferencesCarryDefaultSync() async {
        let transport = FakeConfigTransport(config: [:])
        let editor = makeEditor(transport)
        editor.set("download.keep_awake", "true")
        editor.set("appearance.theme_mode", "dark", immediate: true)
        await editor.settle()
        #expect(transport.prefPatches.allSatisfy { $0.sync })
        #expect(transport.preferences.values["download.keep_awake"] == .bool(true))
        #expect(transport.preferences.values["appearance.theme_mode"] == .string("dark"))
        #expect(editor.form.bool("download.keep_awake"))
    }

    @Test func immediateWritesSkipTheDebounce() async {
        let transport = FakeConfigTransport(config: [:])
        let editor = makeEditor(transport)
        editor.set("appearance.theme_mode", "light", immediate: true)
        #expect(editor.form.value("appearance.theme_mode") == "light")
        await editor.settle()
        #expect(transport.prefPatches.count == 1)
    }

    @Test func rawJSONPreferencesRouteByCatalog() async {
        let transport = FakeConfigTransport(config: [:])
        let editor = makeEditor(transport)
        let list = CustomCategoryDto.preferenceValue(CustomCategoryDto.builtinDefaults)
        editor.setPreference("custom_categories", list)
        #expect(editor.form.pref("custom_categories") == list)
        await editor.settle()
        #expect(transport.prefPatches.count == 1 && transport.prefPatches[0].sync)
        #expect(transport.preferences.values["custom_categories"] == list)
        #expect(editor.optimisticPrefs.isEmpty)
    }

    @Test func preferenceFailureRollsBack() async {
        let transport = FakeConfigTransport(config: [:])
        transport.pendingPrefErrors = [HostError(.invalidArgument)]
        let editor = makeEditor(transport)
        editor.set("download.keep_awake", "true")
        await editor.settle()
        #expect(!editor.form.bool("download.keep_awake"))
        #expect(editor.failures["download.keep_awake"] == "failed:invalidArgument")
        #expect(transport.reports == ["failed:invalidArgument"])
    }

    @Test func setNowSplitsAcrossChannelsInOneAction() async {
        let transport = FakeConfigTransport(config: ["cdn_multi_enabled": "false", "proxy_mode": "system"])
        let editor = makeEditor(transport)
        editor.setNow(["cdn_multi_enabled": "true", "proxy_mode": "none"])
        await editor.settle()
        #expect(transport.patches.count == 1 && transport.patches[0].values == ["proxy_mode": "none"])
        #expect(transport.prefPatches.count == 1 && transport.prefPatches[0].values == ["download.cdn_multi_enabled": .bool(true)])
        #expect(transport.config["proxy_mode"] == "none")
        #expect(transport.config["cdn_multi_enabled"] == "true")
    }

    // MARK: 校验与只读

    @Test func invalidValuesAreRejectedBeforeAnyRequest() async {
        let transport = FakeConfigTransport(config: ["max_concurrent_tasks": "5", "cdn_max_nodes": "0"])
        let editor = makeEditor(transport)
        editor.set("max_concurrent_tasks", "0") // 范围 1...1024
        editor.set("cdn_max_nodes", "9") // 范围 0...8
        editor.set("bt_tracker_sub_cache", "x") // 只读键
        editor.set("proxy_mode", "pac") // 非成员
        editor.set("nope", "1") // 未知键
        await editor.settle()
        #expect(transport.patches.isEmpty && transport.prefPatches.isEmpty)
        #expect(editor.optimistic.isEmpty)
        #expect(editor.failures.keys.contains("max_concurrent_tasks") && editor.failures.keys.contains("cdn_max_nodes"))
        #expect(transport.reports.count == 5)
    }

    @Test func setNowRejectsTheWholeBatchWhenOneKeyIsInvalid() async {
        let transport = FakeConfigTransport(config: ["cdn_multi_enabled": "false"])
        let editor = makeEditor(transport)
        editor.setNow(["cdn_multi_enabled": "true", "proxy_mode": "bogus"])
        await editor.settle()
        #expect(transport.patches.isEmpty && transport.prefPatches.isEmpty)
        #expect(editor.optimistic.isEmpty)
    }

    @Test func readOnlyRejectsWithoutWriting() async {
        let transport = FakeConfigTransport(config: ["bt_port_start": "6881"])
        transport.isReadOnly = true
        let editor = makeEditor(transport)
        editor.set("bt_port_start", "7000")
        editor.set("download.keep_awake", "true")
        editor.setNow(["bt_port_start": "7001"])
        editor.setPreference("custom_categories", .array([]))
        await editor.settle()
        #expect(transport.patches.isEmpty && transport.prefPatches.isEmpty)
        #expect(editor.optimistic.isEmpty && editor.optimisticPrefs.isEmpty)
        #expect(transport.reports == ["offline", "offline", "offline", "offline"])
    }

    @Test func hostSwitchDropsPendingEdits() async {
        let transport = FakeConfigTransport(config: ["bt_port_start": "6881"])
        let editor = makeEditor(transport)
        editor.set("bt_port_start", "7000")
        editor.set("download.keep_awake", "true")
        transport.hostID = "remote-1"
        await editor.settle()
        #expect(transport.patches.isEmpty && transport.prefPatches.isEmpty)
        #expect(editor.optimistic.isEmpty)
    }

    @Test func configMatchingToleratesFloatFormatting() {
        #expect(ConfigEditor.configMatches("1", "1.0"))
        #expect(ConfigEditor.configMatches("1.5", "1.5"))
        #expect(!ConfigEditor.configMatches("2", "1"))
        #expect(!ConfigEditor.configMatches(nil, "1"))
    }
}
