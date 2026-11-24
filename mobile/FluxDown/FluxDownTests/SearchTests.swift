import FluxDomain
import Foundation
import Testing
@testable import FluxDown

@MainActor
struct SearchTests {
    private func entry(
        _ id: String,
        _ title: String,
        sub: String? = nil,
        keywords: [String] = [],
        action: SearchAction = .newDownload
    ) -> SearchEntry {
        SearchEntry(id: id, title: title, sub: sub, keywords: keywords, systemImage: "circle", action: action)
    }

    private func task(_ id: String, _ name: String, created: Int64 = 0) -> DownloadTask {
        DownloadTask(taskId: id, url: "https://example.com/\(name)", fileName: name, status: .completed, createdAt: created)
    }

    // MARK: tokenScore

    @Test func tokenScoreTiers() {
        #expect(SearchMatcher.tokenScore("Report.pdf", "report.PDF") == 100)
        #expect(SearchMatcher.tokenScore("Report.pdf", "rep") == 90)
        #expect(SearchMatcher.tokenScore("my report.pdf", "report") == 82)
        #expect(SearchMatcher.tokenScore("my-report.pdf", "report") == 82)
        #expect(SearchMatcher.tokenScore("myreport.pdf", "report") == 76)
        #expect(SearchMatcher.tokenScore("a1report.pdf", "report") == 76)
        #expect(SearchMatcher.tokenScore("movie.mkv", "zip") == 0)
    }

    @Test func tokenScoreIsCaseInsensitiveForNonASCII() {
        #expect(SearchMatcher.tokenScore("Ünïcode.txt", "ünï") == 90)
        #expect(SearchMatcher.tokenScore("全部暂停", "暂停") == 76)
    }

    @Test func looseScoreNeedsOrderedSubsequenceOfAtLeastTwo() {
        #expect(SearchMatcher.looseScore("Pause All", "pal") == 24)
        #expect(SearchMatcher.looseScore("Pause All", "lap") == 0)
        #expect(SearchMatcher.looseScore("Pause All", "p") == 0)
    }

    // MARK: 任务

    @Test func taskNeedsEveryToken() {
        let tasks = [task("1", "Ubuntu 24.04 desktop.iso"), task("2", "Ubuntu server.iso"), task("3", "fedora.iso")]
        let hits = SearchMatcher.rankTasks(tasks, tokens: SearchMatcher.tokens("ubuntu desktop"))
        #expect(hits.map(\.item.taskId) == ["1"])
        #expect(SearchMatcher.rankTasks(tasks, tokens: []).isEmpty)
    }

    @Test func taskScoreIsWorstTokenScore() {
        // "ubuntu" 前缀 90，"iso" 词边界 82 → 82
        #expect(SearchMatcher.taskScore("ubuntu.iso", tokens: ["ubuntu", "iso"]) == 82)
        #expect(SearchMatcher.taskScore("ubuntu.iso", tokens: ["ubuntu", "nope"]) == 0)
    }

    @Test func tasksRankByScoreThenNewestFirst() {
        let tasks = [
            task("old-prefix", "report-old.pdf", created: 10),
            task("new-prefix", "report-new.pdf", created: 20),
            task("substring", "myreport.pdf", created: 99),
            task("exact", "report", created: 1),
        ]
        let ids = SearchMatcher.rankTasks(tasks, tokens: ["report"]).map(\.item.taskId)
        #expect(ids == ["exact", "new-prefix", "old-prefix", "substring"])
    }

    @Test func equalScoreAndTimeKeepsOriginalOrder() {
        let tasks = [task("a", "x1.bin", created: 5), task("b", "x2.bin", created: 5), task("c", "x3.bin", created: 5)]
        #expect(SearchMatcher.rankTasks(tasks, tokens: ["x"]).map(\.item.taskId) == ["a", "b", "c"])
    }

    // MARK: 命令 / 设置

    @Test func entryKeywordAndSubAreDemoted() {
        let e = entry("pause", "Pause all", sub: "Stops every download", keywords: ["halt", "stop"])
        #expect(SearchMatcher.entryScore(e, tokens: ["pause"]) == 90)
        #expect(SearchMatcher.entryScore(e, tokens: ["alt"]) == 50)
        #expect(SearchMatcher.entryScore(e, tokens: ["every"]) == 40)
        #expect(SearchMatcher.entryScore(e, tokens: ["zzz"]) == 0)
    }

    @Test func entryLooseMatchOnlyForTitle() {
        let e = entry("pause", "Pause all")
        #expect(SearchMatcher.entryScore(e, tokens: ["pal"]) == 24)
    }

    @Test func everyEntryTokenMustMatchAndWorstWins() {
        let e = entry("pause", "Pause all", keywords: ["stop"])
        #expect(SearchMatcher.entryScore(e, tokens: ["pause", "stop"]) == 50)
        #expect(SearchMatcher.entryScore(e, tokens: ["pause", "zzz"]) == 0)
    }

    @Test func rankEntriesIsStableAndEmptyQueryKeepsAll() {
        let entries = [entry("a", "Alpha one"), entry("b", "Beta one"), entry("c", "one")]
        #expect(SearchMatcher.rankEntries(entries, tokens: []).map(\.item.id) == ["a", "b", "c"])
        // "one": c 相等 100；a、b 词边界 82（保持原序）
        #expect(SearchMatcher.rankEntries(entries, tokens: ["one"]).map(\.item.id) == ["c", "a", "b"])
    }

    // MARK: 词表 / 高亮

    @Test func tokensSplitOnAnyWhitespace() {
        #expect(SearchMatcher.tokens("  a  b\tc\n") == ["a", "b", "c"])
        #expect(SearchMatcher.tokens("   ").isEmpty)
    }

    @Test func wordsSplitOnCommaPipeAndFullwidthComma() {
        #expect(SearchMatcher.words("全部开始|全部启动，全部继续, x ,") == ["全部开始", "全部启动", "全部继续", "x"])
        #expect(SearchMatcher.words("").isEmpty)
    }

    @Test func highlightTargetPrefersWholeQuery() {
        #expect(SearchMatcher.highlightTarget(text: "ubuntu desktop.iso", query: "ubuntu desktop", tokens: ["ubuntu", "desktop"]) == "ubuntu desktop")
        #expect(SearchMatcher.highlightTarget(text: "desktop ubuntu.iso", query: "ubuntu desktop", tokens: ["ubuntu", "desktop"]) == "ubuntu")
        #expect(SearchMatcher.highlightTarget(text: "x", query: "  ", tokens: []) == "")
        #expect(SearchMatcher.highlightTarget(text: "x", query: "zzz", tokens: ["zzz"]) == "")
    }

    @Test func rangesFindEveryCaseInsensitiveOccurrence() {
        let text = "Abab aBAb"
        let found = SearchMatcher.ranges(of: "ab", in: text).map { String(text[$0]) }
        #expect(found == ["Ab", "ab", "aB", "Ab"])
        #expect(SearchMatcher.ranges(of: "", in: text).isEmpty)
    }

    // MARK: 分区截断 / 范围

    @Test func allScopeCapsEachSection() {
        let tasks = (0..<12).map { task("t\($0)", "file\($0).bin", created: Int64($0)) }
        let commands = (0..<10).map { entry("c\($0)", "file command \($0)") }
        let settings = (0..<6).map { entry("s\($0)", "file setting \($0)") }
        let r = SearchResults(query: "file", scope: .all, tasks: tasks, commands: commands, settings: settings)
        #expect(r.shownTasks.count == 5 && r.taskHits.count == 12)
        #expect(r.shownCommands.count == 6 && r.commandHits.count == 10)
        #expect(r.shownSettings.count == 3 && r.settingHits.count == 6)
        #expect(r.count(for: .all) == 28)
    }

    @Test func idleAllScopeShowsEightCommandsAndNoTasks() {
        let commands = (0..<10).map { entry("c\($0)", "cmd \($0)") }
        let r = SearchResults(query: "", scope: .all, tasks: [task("1", "a.bin")], commands: commands, settings: [entry("s", "set")])
        #expect(r.shownTasks.isEmpty)
        #expect(r.shownCommands.count == 8)
        #expect(r.shownSettings.count == 1)
        #expect(r.count(for: .all) == nil)
    }

    @Test func scopedSectionOnlyAndWiderCap() {
        let tasks = (0..<40).map { task("t\($0)", "file\($0).bin") }
        let r = SearchResults(query: "file", scope: .tasks, tasks: tasks, commands: [entry("c", "file cmd")], settings: [])
        #expect(r.shownTasks.count == 30)
        #expect(r.shownCommands.isEmpty && r.shownSettings.isEmpty)
        #expect(r.count(for: .commands) == 1)
    }

    @Test func firstResultPrefersTaskThenCommandThenSetting() {
        let cmd = entry("c", "file cmd", action: .pauseAll)
        let set = entry("s", "file page", action: .openSettings(.about))
        let withTask = SearchResults(query: "file", scope: .all, tasks: [task("t", "file.bin")], commands: [cmd], settings: [set])
        #expect(withTask.first == .task("t"))
        let noTask = SearchResults(query: "file", scope: .all, tasks: [], commands: [cmd], settings: [set])
        #expect(noTask.first == .entry(.pauseAll))
        let onlySetting = SearchResults(query: "file", scope: .all, tasks: [], commands: [], settings: [set])
        #expect(onlySetting.first == .entry(.openSettings(.about)))
        let none = SearchResults(query: "zzz", scope: .all, tasks: [], commands: [cmd], settings: [set])
        #expect(none.first == nil && none.isEmpty)
    }
}

// MARK: - 设置行 / 新命令

@MainActor
struct SearchSettingsTests {
    private func settingEntry(
        _ id: String, route: SettingsRoute = .download, title: String = "Speed limit", detail: String = "Cap the total speed",
        keywords: [String] = []
    ) -> SettingsEntry {
        SettingsEntry(
            id: id, route: route, title: title, detail: detail, breadcrumb: "Download › Connection", symbol: "gauge",
            keywords: keywords
        )
    }

    @Test func rowEntriesJumpToTheRowAndShowTheBreadcrumb() {
        let mapped = SearchEntry(setting: settingEntry("download.speedLimit", keywords: ["bandwidth"]))
        #expect(mapped.action == .openSettingsRow(.download, "download.speedLimit"))
        #expect(mapped.sub == "Download › Connection")
        #expect(mapped.keywords == ["bandwidth", "Cap the total speed"])
        #expect(mapped.id == "setting.download.download.speedLimit")
    }

    @Test func categoryEntriesOnlyOpenThePage() {
        let mapped = SearchEntry(setting: settingEntry("category.download", title: "Download", detail: "Download engine settings"))
        #expect(mapped.action == .openSettings(.download))
        #expect(mapped.sub == "Download engine settings")
        #expect(mapped.keywords.isEmpty)
        let bare = SearchEntry(setting: settingEntry("category.webhook", route: .webhook, title: "Webhook", detail: ""))
        #expect(bare.sub == nil)
    }

    @Test func descriptionsAndAliasesAreDemotedMatches() {
        let row = SearchEntry(setting: settingEntry("download.speedLimit", keywords: ["bandwidth"]))
        let byTitle = SearchMatcher.entryScore(row, tokens: ["speed"])
        let byAlias = SearchMatcher.entryScore(row, tokens: ["bandwidth"])
        let byDescription = SearchMatcher.entryScore(row, tokens: ["cap"])
        #expect(byTitle > byAlias)
        #expect(byAlias == 50 && byDescription == 50)
        // 面包屑（副行）也能命中，但分数最低。
        #expect(SearchMatcher.entryScore(row, tokens: ["connection"]) == 40)
    }

    @Test func sameRowIdOnTwoPagesKeepsEntriesUnique() {
        let about = SearchEntry(setting: settingEntry("logs.maxSize", route: .about))
        let diagnostics = SearchEntry(setting: settingEntry("logs.maxSize", route: .diagnostics))
        #expect(about.id != diagnostics.id)
    }

    @Test func settingsHomeSearchFallsBackToAliases() {
        let withAlias = settingEntry("download.speedLimit", title: "Speed limit", detail: "Cap", keywords: ["Bandwidth"])
        let other = settingEntry("download.userAgent", title: "User agent", detail: "Browser identity")
        #expect(SettingsSearch.filter([other, withAlias], query: "bandwidth").map(\.id) == ["download.speedLimit"])
        // 标题 > 说明 > 别名
        let aliasOnly = settingEntry("a", title: "Alpha", detail: "", keywords: ["limit"])
        let detailHit = settingEntry("b", title: "Beta", detail: "the limit")
        let titleHit = settingEntry("c", title: "Limit", detail: "")
        #expect(SettingsSearch.filter([aliasOnly, detailHit, titleHit], query: "limit").map(\.id) == ["c", "b", "a"])
    }

    // MARK: 索引覆盖

    private func loadedState(capabilities: Set<String> = []) -> HostState {
        var state = HostState()
        state.config = ["max_concurrent_tasks": "5", "bt_enable_dht": "true", "proxy_mode": "none", "log_max_size_mb": "10"]
        state.info = HostInfo(serviceName: "t", serviceVersion: "1", protocolVersion: 1, capabilities: capabilities)
        return state
    }

    @Test func indexCoversEveryCategoryAndItsRows() {
        let entries = SettingsIndex.entries(state: loadedState(capabilities: [HostCapability.agentAuth]), isLocalHost: true)
        let categories = Set(entries.filter { $0.id.hasPrefix("category.") }.map(\.route))
        var expected: Set<SettingsRoute> = [
            .account, .general, .appearance, .notify, .download, .network, .diagnostics, .about,
        ]
        #if FULL_FLAVOR
        expected.formUnion([.extensions, .bt, .ed2k])
        #endif
        #expect(categories == expected) // 本机：没有 API 服务页；Webhook 受 `FeatureFlags.webhooks` 隐藏
        #expect(!entries.contains { $0.route == .webhook || $0.id.contains("webhook") })
        let withRows = Set(entries.filter { !$0.id.hasPrefix("category.") }.map(\.route))
        var routesWithRows: [SettingsRoute] = [.general, .appearance, .notify, .download, .network, .diagnostics, .about]
        #if FULL_FLAVOR
        routesWithRows += [.bt, .ed2k]
        #endif
        for route in routesWithRows {
            #expect(withRows.contains(route), "\(route) contributes no rows to the index")
        }
        #expect(entries.allSatisfy { !$0.title.isEmpty })
        let keys = entries.map { "\($0.route).\($0.id)" }
        #expect(Set(keys).count == keys.count, "route+id must be unique")
    }

    @Test func indexHidesHostBoundPagesWithoutCapabilities() {
        let local = SettingsIndex.entries(state: loadedState(), isLocalHost: true)
        #expect(!local.contains { $0.route == .account || $0.route == .api })
        let remote = SettingsIndex.entries(state: loadedState(capabilities: [HostCapability.agentGateway]), isLocalHost: false)
        #expect(remote.contains { $0.route == .api })
        // 本机主机不显示 API 服务页，即使能力在。
        let localWithGateway = SettingsIndex.entries(state: loadedState(capabilities: [HostCapability.agentGateway]), isLocalHost: true)
        #expect(!localWithGateway.contains { $0.route == .api })
    }

    @Test func indexShowsLogLevelOnlyWhenHostPublishesIt() throws {
        let key = "diagnostics.log_level"
        var state = loadedState()
        func publish(_ value: JSONValue) throws {
            state.sections[HostSection.agentPreferences] = try ProtocolJSON.encode(
                AgentPreferencesDto(values: [key: value]), what: "agent preferences"
            )
        }
        func logLevelRoutes() -> Set<SettingsRoute> {
            Set(SettingsIndex.entries(state: state, isLocalHost: false)
                .filter { $0.id == "logs.level" }.map(\.route))
        }
        #expect(logLevelRoutes().isEmpty)
        let invalidValues: [JSONValue] = [.null, .bool(true), .string("trace")]
        for invalid in invalidValues {
            try publish(invalid)
            #expect(logLevelRoutes().isEmpty)
        }
        try publish(.string("auto"))
        #expect(logLevelRoutes() == [.about, .diagnostics])
    }

    @Test func indexFollowsRowVisibility() {
        // 配置未加载（连接前）：daemon 行不出现，偏好行仍在。
        let loading = SettingsIndex.entries(state: HostState(), isLocalHost: true)
        #expect(!loading.contains { $0.id == "download.maxConcurrent" })
        #expect(loading.contains { $0.id == "general.keepAwake" })
        let loaded = SettingsIndex.entries(state: loadedState(), isLocalHost: true)
        #expect(loaded.contains { $0.id == "download.maxConcurrent" })
        // 本机不显示的行（空闲文件扫描）同样不进索引。
        #expect(!loaded.contains { $0.id == "download.idleFileScan" })
    }

    @Test func rowAliasesComeFromTheCommandPaletteWordLists() {
        let entries = SettingsIndex.entries(state: loadedState(), isLocalHost: true)
        // 词表随界面语言而变：只断言挂的是同一份 i18n 词表。
        let speedWords = SettingsIndex.words("searchKeywordsSpeedLimit")
        #expect(!speedWords.isEmpty)
        #expect(entries.first { $0.id == "download.speedLimit" }?.keywords == speedWords)
        #if FULL_FLAVOR
        let btWords = SettingsIndex.words("searchKeywordsBtSettings")
        #expect(!btWords.isEmpty)
        #expect(entries.first { $0.id == "category.bt" }?.keywords == btWords)
        #endif
    }

    // MARK: 命令

    @Test func commandsIncludeTheNewDestinations() {
        let actions = SearchCommands.entries(isDark: false, canAddDevice: true, showsThemeToggle: true).map(\.action)
        #expect(actions.contains(.openQueues) && actions.contains(.newRssSource) && actions.contains(.addDevice) && actions.contains(.addHost))
        #expect(actions.contains(.goTab(.settings)) && actions.contains(.toggleTheme))
    }

    @Test func addDeviceNeedsAccountOrLinkCapability() {
        let none = SearchCommands.entries(isDark: false, canAddDevice: false, showsThemeToggle: true).map(\.action)
        #expect(!none.contains(.addDevice))
    }

    @Test func themeToggleFollowsTheSyncedPreference() {
        let off = SearchCommands.entries(isDark: true, canAddDevice: true, showsThemeToggle: false)
        #expect(!off.contains { $0.action == .toggleTheme })
        let dark = SearchCommands.entries(isDark: true, canAddDevice: true, showsThemeToggle: true).first { $0.action == .toggleTheme }
        let light = SearchCommands.entries(isDark: false, canAddDevice: true, showsThemeToggle: true).first { $0.action == .toggleTheme }
        #expect(dark?.title == L("toggleToLight") && light?.title == L("toggleToDark"))
    }

    @Test func commandIdsAreUnique() {
        let ids = SearchCommands.entries(isDark: false, canAddDevice: true, showsThemeToggle: true).map(\.id)
        #expect(Set(ids).count == ids.count)
    }

    @Test func idleCommandListLeadsWithTheActionsNotNavigation() {
        let r = SearchResults(
            query: "", scope: .all, tasks: [],
            commands: SearchCommands.entries(isDark: false, canAddDevice: true, showsThemeToggle: true), settings: []
        )
        let shown = r.shownCommands.map(\.item.action)
        #expect(shown.count == 8)
        #expect(shown.contains(.openQueues) && shown.contains(.newRssSource) && shown.contains(.addDevice))
    }
}

@MainActor
struct FilterBarVisibilityTests {
    @Test func everythingShowsByDefault() {
        #expect(FilterBarVisibility(AgentPreferencesDto()) == FilterBarVisibility())
    }

    @Test func readsTheThreeSidebarPreferences() {
        let prefs = AgentPreferencesDto(values: [
            "ui.show_sidebar_status": .bool(false), "ui.show_sidebar_queues": .bool(true), "ui.show_sidebar_category": .bool(false),
        ])
        let visibility = FilterBarVisibility(prefs)
        #expect(!visibility.status && visibility.queues && !visibility.categories)
    }

    @Test func emptyOnlyWhenNothingIsVisible() {
        let hidden = FilterBarVisibility(status: false, queues: false, categories: false)
        #expect(hidden.isEmpty(hasCategories: true, hasScopeChip: false))
        // 状态条显示 → 非空
        #expect(!FilterBarVisibility(status: true, queues: false, categories: false).isEmpty(hasCategories: true, hasScopeChip: false))
        // 分类开着且有分类 → 非空；分类开着但没有分类 → 空
        let categoriesOnly = FilterBarVisibility(status: false, queues: false, categories: true)
        #expect(!categoriesOnly.isEmpty(hasCategories: true, hasScopeChip: false))
        #expect(categoriesOnly.isEmpty(hasCategories: false, hasScopeChip: false))
        // 队列芯片显示 → 非空
        #expect(!hidden.isEmpty(hasCategories: false, hasScopeChip: true))
    }
}
