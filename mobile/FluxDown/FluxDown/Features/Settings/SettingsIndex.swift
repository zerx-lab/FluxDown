import FluxDomain
import Foundation

/// 设置搜索索引：设置首页搜索（S1）与全局搜索（G1）共用同一份。
///
/// 每个分类页自己提供行条目（`XxxPage.searchEntries`，与页面渲染共用可见性判定），这里负责汇总、补上每个分类页本身的入口，
/// 并给条目挂上 PC 命令面板的别名词表（`searchKeywords…`）。
enum SettingsIndex {
    static func context(state: HostState, isLocalHost: Bool) -> SettingsSearchContext {
        SettingsSearchContext(
            form: SettingsConfigForm(host: state.config, prefs: state.preferences.values),
            isLocalHost: isLocalHost,
            capabilities: state.info?.capabilities ?? []
        )
    }

    /// 全部条目，顺序同设置首页分类顺序（分类入口在前，其下是该页的行）。
    static func entries(state: HostState, isLocalHost: Bool) -> [SettingsEntry] {
        let ctx = context(state: state, isLocalHost: isLocalHost)
        var list: [SettingsEntry] = []
        if state.has(HostCapability.agentAuth) {
            list.append(category(.account, "settingsCatAccount", "settingsCatAccountDesc", "person.crop.circle.fill"))
        }
        list.append(category(.general, "settingsCatGeneral", "settingsCatGeneralDesc", "gearshape.fill", ["searchKeywordsClipboardWatch"]))
        list += GeneralPage.searchEntries(ctx)
        list.append(category(
            .appearance, "settingsCatAppearance", "settingsCatAppearanceDesc", "paintpalette.fill",
            ["searchKeywordsThemeMode", "searchKeywordsThemeColor"]
        ))
        list += AppearancePage.searchEntries(ctx)
        list.append(category(.notify, "settingsCatNotify", "settingsCatNotifyDesc", "bell.badge.fill", ["searchKeywordsNotifyOnComplete"]))
        list += NotifyPage.searchEntries(ctx)
        list.append(category(
            .download, "settingsCatDownload", "settingsCatDownloadDesc", "arrow.down.circle.fill",
            ["searchKeywordsSaveDir", "searchKeywordsThreads", "searchKeywordsConcurrent", "searchKeywordsSpeedLimit"]
        ))
        list += DownloadPage.searchEntries(ctx)
        #if FULL_FLAVOR
        list.append(category(.bt, "settingsCatBt", "settingsCatBtDesc", "point.3.connected.trianglepath.dotted", ["searchKeywordsBtSettings"]))
        list += BtPage.searchEntries(ctx)
        list.append(category(.ed2k, "ed2kSettings", "ed2kSettingsDesc", "server.rack", ["searchKeywordsEd2kSettings"]))
        list += Ed2kPage.searchEntries(ctx)
        #endif
        list.append(category(.network, "settingsCatProxy", "settingsCatProxyDesc", "globe", ["searchKeywordsProxy"]))
        list += NetworkPage.searchEntries(ctx)
        #if FULL_FLAVOR
        list.append(category(.extensions, "settingsCatExtensions", "settingsCatExtensionsDesc", "puzzlepiece.extension.fill"))
        #endif
        if FeatureFlags.webhooks {
            list.append(category(.webhook, "webhookNavTitle", nil, "bolt.horizontal.fill", ["searchKeywordsWebhook"]))
        }
        if !isLocalHost, state.has(HostCapability.agentGateway) {
            list.append(category(.api, "settingsCatApiService", "settingsCatApiServiceDesc", "curlybraces", ["searchKeywordsApiService"]))
        }
        list.append(category(.diagnostics, "settingsCatDoctor", "settingsCatDoctorDesc", "stethoscope", ["searchKeywordsDoctor"]))
        list += DiagnosticsPage.searchEntries(ctx)
        list.append(category(.about, "settingsCatAbout", "settingsCatAboutDesc", "info.circle.fill", ["searchKeywordsUpdate"]))
        list += AboutPage.searchEntries(ctx)
        return list.map(withKeywords)
    }

    /// 分类页入口条目（点击进入该页，不定位具体行）。
    static func category(
        _ route: SettingsRoute, _ titleKey: String, _ detailKey: String?, _ symbol: String, _ keywordKeys: [String] = []
    ) -> SettingsEntry {
        SettingsEntry(
            id: "category.\(route)", route: route, title: L(titleKey), detail: detailKey.map { L($0) } ?? "",
            breadcrumb: L("settings"), symbol: symbol, keywords: keywordKeys.flatMap(words)
        )
    }

    /// 给行条目补上别名词表（已带别名的分类入口保持原样）。
    private static func withKeywords(_ entry: SettingsEntry) -> SettingsEntry {
        guard entry.keywords.isEmpty, let keys = rowKeywordKeys[entry.id] else { return entry }
        var next = entry
        next.keywords = keys.flatMap(words)
        return next
    }

    /// 行 id → PC 命令面板别名词表键（只列 PC 有别名的行）。
    static let rowKeywordKeys: [String: [String]] = [
        "general.keepAwake": ["searchKeywordsKeepAwake"],
        "general.sidebarStatus": ["searchKeywordsSidebarVisibility"],
        "general.sidebarQueues": ["searchKeywordsSidebarVisibility"],
        "general.sidebarCategory": ["searchKeywordsSidebarVisibility"],
        "download.categories": ["searchKeywordsCustomCategories"],
        "appearance.language": ["searchKeywordsLanguage"],
        "appearance.mode": ["searchKeywordsThemeMode"],
        "appearance.color": ["searchKeywordsThemeColor"],
        "download.saveDir": ["searchKeywordsSaveDir"],
        "download.silentDownload": ["searchKeywordsSilentDownload"],
        "download.silentSkipSelection": ["searchKeywordsSilentSkipSelection"],
        "download.useServerTime": ["searchKeywordsUseServerTime"],
        "download.fileExistsBehavior": ["searchKeywordsFileExists"],
        "download.fileMissingAction": ["searchKeywordsFileMissing"],
        "download.defaultSegments": ["searchKeywordsThreads"],
        "download.cdnMulti": ["searchKeywordsCdnMulti"],
        "download.multiNic": ["searchKeywordsMultiNic"],
        "download.maxConcurrent": ["searchKeywordsConcurrent"],
        "download.speedLimit": ["searchKeywordsSpeedLimit"],
        "download.userAgent": ["searchKeywordsUserAgent"],
        "logs.maxSize": ["searchKeywordsLogExport"],
        "logs.export": ["searchKeywordsLogExport"],
        "btGuard.enabled": ["btGuardKeywords"],
        "btGuard.bans": ["btGuardKeywords"],
    ]

    /// `, | ，` 分隔的别名词表 → 词。
    static func words(_ key: String) -> [String] {
        L(key).split(whereSeparator: { $0 == "," || $0 == "|" || $0 == "\u{FF0C}" })
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }
}
