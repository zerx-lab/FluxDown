import FluxDomain
import Foundation
import Testing

/// 设置写路径（路由 / 同步键映射 / 规范化）、偏好 DTO 与分类规则。
struct PreferencesProtocolTests {
    // MARK: - 同步目录

    @Test func syncCatalogMirrorsSeventySevenSpecs() {
        // iOS 镜像 77 项：49 个 daemon 键（含 19 个 `bt.guard.*`）+ 27 个偏好 / agent 键 + 集合范围键 `appearance.custom_themes`
        // （Rust 目录 79 项，多出的 `ui.show_activity_account` 与 `appearance.file_icon_pack` 是 GPUI / Web 专属，
        // iOS 尚未镜像）。
        #expect(SettingsCatalog.daemonSyncNames.count == 49)
        #expect(SettingsCatalog.syncedPreferenceKeys.count == 27)
        #expect(SettingsCatalog.daemonSyncNames.count + SettingsCatalog.syncedPreferenceKeys.count + 1 == 77)
        #expect(SettingsCatalog.syncedPreferenceKeys.contains("custom_categories"))
        #expect(!SettingsCatalog.syncedPreferenceKeys.contains(SettingsCatalog.customThemesScopeKey))
        #expect(SettingsCatalog.syncNameToDaemonKey["download.max_concurrent_tasks"] == "max_concurrent_tasks")
        #expect(SettingsCatalog.syncNameToDaemonKey.count == 49)
    }

    @Test func everySyncedDaemonKeyIsAWritableDaemonField() throws {
        for key in SettingsCatalog.daemonSyncNames.keys {
            let field = try #require(SettingsCatalog.field(key), "\(key)")
            #expect(field.store == .daemon)
            #expect(field.kind != .readOnly, "\(key)")
        }
    }

    @Test func syncedMarkerFollowsTheCatalog() {
        #expect(SettingsCatalog.isSynced("max_concurrent_tasks"))
        #expect(SettingsCatalog.isSynced("bt_seed_time_limit_minutes"))
        #expect(!SettingsCatalog.isSynced("bt_seed_time_limit_unit")) // 单位键仅本机
        #expect(!SettingsCatalog.isSynced("upload_limit_bytes"))
        #expect(!SettingsCatalog.isSynced("bt_port_start"))
        #expect(SettingsCatalog.isSynced("download.keep_awake"))
        #expect(SettingsCatalog.isSynced("ui.show_activity_rss"))
        #expect(!SettingsCatalog.isSynced("ui.show_sidebar_devices")) // 设备本地
        #expect(!SettingsCatalog.isSynced("download.silent_skip_selection"))
        #expect(!SettingsCatalog.isSynced("analytics_enabled"))
        #expect(SettingsCatalog.isSynced("custom_categories"))
        #expect(SettingsCatalog.isSynced("appearance.custom_themes.nord"))
    }

    // MARK: - 写入路由

    @Test func syncedDaemonKeysRouteThroughPreferencesUnderTheirSyncName() throws {
        let plan = try SettingsWritePlan.make([
            "max_concurrent_tasks": "8", "cdn_multi_enabled": "1", "speed_limit_bytes": "1048576",
            "bt_seed_ratio_limit": "1.50", "global_user_agent": "  UA/1 ",
        ]).get()
        #expect(plan.daemon.isEmpty)
        #expect(plan.localPreferences.isEmpty)
        #expect(plan.syncedPreferences == [
            "download.max_concurrent_tasks": .int(8),
            "download.cdn_multi_enabled": .bool(true),
            "download.speed_limit_bytes": .int(1_048_576),
            "bt.seed_ratio_limit": .double(1.5),
            "download.global_user_agent": .string("UA/1"),
        ])
        #expect(plan.syncedDaemonKeys["download.max_concurrent_tasks"] == "max_concurrent_tasks")
    }

    @Test func nonSyncedDaemonKeysUseDaemonConfigPatch() throws {
        let plan = try SettingsWritePlan.make([
            "upload_limit_bytes": "0", "file_exists_behavior": " skip ", "bt_port_start": "6900",
            "proxy_mode": "manual", "bt_seed_time_limit_unit": "hours", "default_save_dir": " /a/b ",
        ]).get()
        #expect(plan.syncedPreferences.isEmpty && plan.localPreferences.isEmpty)
        #expect(plan.daemon == [
            "upload_limit_bytes": "0", "file_exists_behavior": "skip", "bt_port_start": "6900",
            "proxy_mode": "manual", "bt_seed_time_limit_unit": "hours", "default_save_dir": "/a/b",
        ])
    }

    @Test func preferenceKeysSplitBetweenSyncedAndLocal() throws {
        let plan = try SettingsWritePlan.make([
            "download.keep_awake": "true", "ui.show_activity_rss": "false",
            "download.silent_skip_selection": "true", "analytics_enabled": "false",
            "appearance.theme_mode": "dark", "appearance.custom_color": "4283782485",
        ]).get()
        #expect(plan.daemon.isEmpty)
        #expect(plan.syncedPreferences == [
            "download.keep_awake": .bool(true), "ui.show_activity_rss": .bool(false),
            "appearance.theme_mode": .string("dark"), "appearance.custom_color": .int(4_283_782_485),
        ])
        #expect(plan.localPreferences == [
            "download.silent_skip_selection": .bool(true), "analytics_enabled": .bool(false),
        ])
    }

    @Test func mixedEditsSplitAcrossChannels() throws {
        // 「开启多 CDN 并关闭代理」：cdn_multi_enabled 同步，proxy_mode 仅 daemon。
        let plan = try SettingsWritePlan.make(["cdn_multi_enabled": "true", "proxy_mode": "none"]).get()
        #expect(plan.syncedPreferences == ["download.cdn_multi_enabled": .bool(true)])
        #expect(plan.daemon == ["proxy_mode": "none"])
        #expect(!plan.isEmpty)
    }

    @Test func customCategoriesAndRawPreferencesPickTheirChannelByCatalog() {
        var plan = SettingsWritePlan()
        plan.addPreference("custom_categories", .array([]))
        plan.addPreference("mobile.whatever", .bool(true))
        #expect(plan.syncedPreferences.keys.contains("custom_categories"))
        #expect(plan.localPreferences.keys.contains("mobile.whatever"))
        #expect(SettingsWritePlan.validatePreferenceKey("appearance.custom_themes") != nil)
        #expect(SettingsWritePlan.validatePreferenceKey("appearance.custom_themes.nord") == nil)
        #expect(SettingsWritePlan.validatePreferenceKey("  ") != nil)
    }

    // MARK: - 校验

    private func rejected(_ key: String, _ value: String) -> Bool {
        if case .failure = SettingsCatalog.normalize(key, value) { true } else { false }
    }

    private func accepted(_ key: String, _ value: String) -> String? {
        if case let .success(wire) = SettingsCatalog.normalize(key, value) { wire } else { nil }
    }

    @Test func boolNormalization() {
        #expect(accepted("bt_enable_dht", " 1 ") == "true")
        #expect(accepted("bt_enable_dht", "0") == "false")
        #expect(accepted("bt_enable_dht", "TRUE") == nil)
        #expect(rejected("bt_enable_dht", "yes"))
    }

    @Test func integerRangesAreEnforcedLikeTheDaemon() {
        #expect(accepted("max_concurrent_tasks", "1024") == "1024")
        #expect(rejected("max_concurrent_tasks", "0"))
        #expect(rejected("max_concurrent_tasks", "1025"))
        #expect(accepted("max_auto_retries", "-1") == "-1")
        #expect(rejected("max_auto_retries", "-2"))
        #expect(rejected("max_auto_retries", "21"))
        #expect(accepted("cdn_max_nodes", "8") == "8")
        #expect(rejected("cdn_max_nodes", "9"))
        #expect(accepted("bt_port_start", "65535") == "65535")
        #expect(rejected("bt_port_start", "0"))
        #expect(accepted("ed2k_listen_port", "0") == "0")
        #expect(rejected("ed2k_listen_port", "65536"))
        #expect(accepted("log_max_size_mb", " 64 ") == "64")
        #expect(rejected("log_max_size_mb", "0"))
        #expect(rejected("log_max_size_mb", "1025"))
        #expect(accepted("auto_retry_delay_secs", "+5") == "5")
        #expect(rejected("auto_retry_delay_secs", "5.5"))
        #expect(rejected("auto_retry_delay_secs", "5s"))
        #expect(rejected("auto_retry_delay_secs", ""))
        #expect(accepted("speed_limit_bytes", "9223372036854775807") == "9223372036854775807")
        #expect(rejected("speed_limit_bytes", "9223372036854775808"))
        #expect(rejected("speed_limit_bytes", "-1"))
    }

    @Test func floatsRequireFiniteNonNegative() {
        #expect(accepted("bt_seed_ratio_limit", "2") == "2")
        #expect(accepted("bt_seed_ratio_limit", "1.50") == "1.5")
        #expect(accepted("bt_seed_ratio_limit", "0") == "0")
        #expect(rejected("bt_seed_ratio_limit", "-0.1"))
        #expect(rejected("bt_seed_ratio_limit", "nan"))
        #expect(rejected("bt_seed_ratio_limit", "inf"))
        #expect(rejected("bt_seed_ratio_limit", "abc"))
    }

    @Test func enumsMustBeMembers() {
        #expect(accepted("proxy_mode", " auto ") == "auto")
        #expect(rejected("proxy_mode", "pac"))
        #expect(accepted("bt_mse_mode", "forced") == "forced")
        #expect(rejected("bt_mse_mode", "required"))
        #expect(accepted("bt_seed_then_action", "delete_files") == "delete_files")
        #expect(rejected("bt_seed_limit_operator", "xor"))
        #expect(accepted("appearance.theme_mode", "dark") == "dark")
        #expect(rejected("appearance.theme_mode", "auto"))
        #expect(accepted("file_exists_behavior", " ask ") == "ask")
        #expect(rejected("file_exists_behavior", "prompt"))
    }

    @Test func textRulesAndReadOnlyKeys() {
        #expect(accepted("proxy_host", "  127.0.0.1  ") == "127.0.0.1")
        #expect(accepted("global_user_agent", "  Mozilla/5.0\t(X) 中文 ") == "Mozilla/5.0\t(X) 中文")
        for bad in ["a\nb", "a\rb", "a\u{1}b", "a\u{7f}b"] { #expect(rejected("global_user_agent", bad), "\(bad)") }
        #expect(rejected("bt_tracker_sub_cache", "x"))
        #expect(rejected("domain_conn_caps", "x"))
        #expect(rejected("nope", "x"))
        #expect(accepted("component_mirror_base", "") == "")
        #expect(accepted("component_mirror_base", " HTTPS://gh.example.com/proxy/ ") == "https://gh.example.com/proxy")
        for bad in ["http://x.test", "gh.example.com", "ftp://x", "https://", "https:///path", "https://h/a b", "https://h/?x=1", "https://h/#f"] {
            #expect(rejected("component_mirror_base", bad), "\(bad)")
        }
    }

    @Test func oneInvalidKeyRejectsTheWholePlan() {
        let result = SettingsWritePlan.make(["max_concurrent_tasks": "8", "cdn_max_nodes": "99"])
        guard case let .failure(error) = result else {
            Issue.record("plan must be rejected")
            return
        }
        #expect(error.key == "cdn_max_nodes")
        #expect(error.message.contains("between 0 and 8"))
    }

    @Test func defaultsAreValidForEveryWritableField() {
        for field in SettingsCatalog.daemonFields + SettingsCatalog.preferenceFields where field.kind != .readOnly {
            #expect(field.key.isEmpty == false)
            if case .failure = SettingsCatalog.normalize(field.key, field.defaultWire) {
                Issue.record("default of \(field.key) is invalid")
            }
        }
    }

    @Test func jsonWireConversionRoundTrips() {
        #expect(SettingsCatalog.wire(fromJSON: .bool(true)) == "true")
        #expect(SettingsCatalog.wire(fromJSON: .int(-1)) == "-1")
        #expect(SettingsCatalog.wire(fromJSON: .uint(7)) == "7")
        #expect(SettingsCatalog.wire(fromJSON: .double(2)) == "2")
        #expect(SettingsCatalog.wire(fromJSON: .double(0.5)) == "0.5")
        #expect(SettingsCatalog.wire(fromJSON: .string("dark")) == "dark")
        #expect(SettingsCatalog.wire(fromJSON: .null) == nil)
        #expect(SettingsCatalog.wire(fromJSON: .array([])) == nil)
    }

    // MARK: - 偏好 DTO

    @Test func preferencesSectionDecodesLeniently() throws {
        let text = #"{"revision":7,"values":{"download.keep_awake":true,"appearance.custom_color":4284704497,"general.locale":"zh"}}"#
        let dto = try ProtocolJSON.decode(AgentPreferencesDto.self, from: Data(text.utf8), what: "prefs")
        #expect(dto.revision == 7)
        #expect(dto.bool("download.keep_awake", default: false))
        #expect(dto["appearance.custom_color"]?.intValue == 4_284_704_497)
        #expect(dto.string("general.locale", default: "system") == "zh")
        #expect(dto.string("missing", default: "x") == "x")
        let empty = try ProtocolJSON.decode(AgentPreferencesDto.self, from: Data("{}".utf8), what: "prefs")
        #expect(empty.revision == 0 && empty.values.isEmpty)
    }

    @Test func patchParamsUseWireShape() throws {
        let synced = PreferencesPatchParams(values: ["download.keep_awake": .bool(true)])
        #expect(String(decoding: try ProtocolJSON.encode(synced, what: "x"), as: UTF8.self)
            == #"{"values":{"download.keep_awake":true}}"#)
        let local = PreferencesPatchParams(values: ["download.silent_skip_selection": .bool(false)], sync: false)
        #expect(String(decoding: try ProtocolJSON.encode(local, what: "x"), as: UTF8.self)
            == #"{"sync":false,"values":{"download.silent_skip_selection":false}}"#)
        let tombstone = PreferencesPatchParams(values: ["general.locale": .null])
        #expect(String(decoding: try ProtocolJSON.encode(tombstone, what: "x"), as: UTF8.self)
            == #"{"values":{"general.locale":null}}"#)
        let result = try ProtocolJSON.decode(PreferencesPatchResult.self, from: Data(#"{"ok":true,"revision":12}"#.utf8), what: "r")
        #expect(result.ok && result.revision == 12)
    }

    @Test @MainActor func hostStateExposesDecodedPreferences() {
        var state = HostState()
        #expect(state.preferences.values.isEmpty)
        state.sections[HostSection.agentPreferences] = Data(#"{"revision":3,"values":{"ui.show_activity_rss":false}}"#.utf8)
        #expect(state.preferences.revision == 3)
        #expect(!state.preferences.bool("ui.show_activity_rss", default: true))
        state.sections[HostSection.agentPreferences] = Data("not json".utf8)
        #expect(state.preferences.values.isEmpty)
    }

    // MARK: - 分类

    @Test func categoryJSONShapeMatchesDartAndRust() throws {
        let json = #"[{"id":"x","name":"eBooks","icon":"library","matchMode":"extension","extensions":["epub"],"regexPattern":"","position":3,"visible":true,"isBuiltin":false,"builtinType":null,"saveDir":""}]"#
        let list = try ProtocolJSON.decode([CustomCategoryDto].self, from: Data(json.utf8), what: "categories")
        #expect(list[0].extensions == ["epub"] && list[0].position == 3 && list[0].builtinType == nil)
        let back = String(decoding: try ProtocolJSON.encode(list, what: "categories"), as: UTF8.self)
        #expect(back.contains(#""builtinType":null"#) && back.contains(#""matchMode":"extension""#))
        // 缺失字段取 serde 默认值。
        let sparse = try ProtocolJSON.decode([CustomCategoryDto].self, from: Data(#"[{"id":"a","name":"A"}]"#.utf8), what: "c")
        #expect(sparse[0].icon == "file" && sparse[0].matchMode == "extension" && sparse[0].visible)
    }

    @Test func categoriesFromPreferenceFallBackAndSortByPosition() {
        #expect(CustomCategoryDto.fromPreference(nil) == CustomCategoryDto.builtinDefaults)
        #expect(CustomCategoryDto.fromPreference(.string("not json")) == CustomCategoryDto.builtinDefaults)
        #expect(CustomCategoryDto.fromPreference(.array([])) == CustomCategoryDto.builtinDefaults)
        let array: JSONValue = .array([
            .object(["id": .string("b"), "name": .string("B"), "position": .int(2)]),
            .object(["id": .string("a"), "name": .string("A"), "position": .int(1)]),
        ])
        #expect(CustomCategoryDto.fromPreference(array).map(\.id) == ["a", "b"])
        // Flutter 偏好同形：JSON 数组字符串。
        let text = #"[{"id":"z","name":"Z","position":0}]"#
        #expect(CustomCategoryDto.fromPreference(.string(text)).map(\.id) == ["z"])
    }

    @Test func preferenceValueReindexesPositionsAndRoundTrips() throws {
        var list = CustomCategoryDto.builtinDefaults
        list.reverse()
        let value = CustomCategoryDto.preferenceValue(list)
        let back = CustomCategoryDto.fromPreference(value)
        #expect(back.map(\.id) == list.map(\.id))
        #expect(back.map(\.position) == Array(0 ..< Int64(list.count)))
    }

    @Test func builtinDefaultsMatchTheProtocolBaseline() {
        let defaults = CustomCategoryDto.builtinDefaults
        #expect(defaults.map(\.builtinType) == ["all", "video", "audio", "document", "image", "program", "archive", "other"])
        #expect(defaults.allSatisfy { $0.isBuiltin })
        #expect(defaults[1].extensions.first == "mp4" && defaults[1].icon == "film")
        #expect(defaults[0].isAll && !defaults[0].hasMatchRules && defaults[7].isOther && !defaults[7].hasMatchRules)
    }

    // MARK: 分类名 / 目录规则

    @Test func dirNamesAreSanitizedLikeDesktop() {
        #expect(CategoryRules.sanitizeDirName("a/b:c ") == "a b c")
        #expect(CategoryRules.sanitizeDirName("name...") == "name")
        #expect(CategoryRules.sanitizeDirName("  ") == "")
        #expect(CategoryRules.sanitizeDirName("a\\b*c?d\"e<f>g|h") == "a b c d e f g h")
        #expect(CategoryRules.sanitizeDirName("tab\there\u{1}x") == "tab here x")
        #expect(CategoryRules.sanitizeDirName("  电子书   合集 ") == "电子书 合集")
        #expect(CategoryRules.sanitizeDirName("movies. . ") == "movies")
    }

    @Test func dirUnderJoinsWithTheHostSeparator() {
        #expect(CategoryRules.dirUnder(base: "", label: "Video") == "")
        #expect(CategoryRules.dirUnder(base: "/tmp/dl", label: "///") == "")
        #expect(CategoryRules.dirUnder(base: "/", label: "Video") == "/Video")
        #expect(CategoryRules.dirUnder(base: "/tmp/dl/", label: "Video") == "/tmp/dl/Video")
        #expect(CategoryRules.dirUnder(base: "/tmp/dl//", label: "Video") == "/tmp/dl/Video")
        #expect(CategoryRules.dirUnder(base: " /srv/files ", label: "a/b") == "/srv/files/a b")
        // Windows 主机：盘符或纯反斜杠路径用 `\`。
        #expect(CategoryRules.dirUnder(base: "D:\\Downloads", label: "Video") == "D:\\Downloads\\Video")
        #expect(CategoryRules.dirUnder(base: "D:/Downloads", label: "Video") == "D:/Downloads\\Video")
        #expect(CategoryRules.dirUnder(base: "\\\\nas\\share\\", label: "Audio") == "\\\\nas\\share\\Audio")
        #expect(CategoryRules.dirUnder(base: "C:\\", label: "Audio") == "C:\\Audio")
    }

    @Test func autoDirsUseEnglishBuiltinLabelsAndSkipAll() throws {
        var list = CustomCategoryDto.builtinDefaults
        list.append(CustomCategoryDto(id: "custom_1", name: "eBooks: new", position: 8))
        let updated = try #require(CategoryRules.autoDirs(list, baseDir: "/dl/"))
        let dirs = Dictionary(uniqueKeysWithValues: updated.map { ($0.builtinType ?? $0.id, $0.saveDir) })
        #expect(dirs["all"] == "")
        #expect(dirs["video"] == "/dl/Video" && dirs["audio"] == "/dl/Audio" && dirs["document"] == "/dl/Document")
        #expect(dirs["image"] == "/dl/Image" && dirs["program"] == "/dl/Programs" && dirs["archive"] == "/dl/Archive")
        #expect(dirs["other"] == "/dl/Other")
        #expect(dirs["custom_1"] == "/dl/eBooks new")
        #expect(CategoryRules.autoDirs(list, baseDir: "  ") == nil)
        #expect(CategoryRules.autoDirsChangeCount(list, baseDir: "/dl/") == 8)
        #expect(CategoryRules.autoDirsChangeCount(updated, baseDir: "/dl/") == 0)
    }

    @Test func extensionTextParsing() {
        #expect(CategoryRules.parseExtensions(" .EPUB, mobi\u{FF0C}azw3  txt ") == ["epub", "mobi", "azw3", "txt"])
        #expect(CategoryRules.parseExtensions(" , ").isEmpty)
        #expect(CategoryRules.parseExtensions("tar.gz") == ["targz"])
    }

    @Test func regexSanityCheck() {
        #expect(CategoryRules.regexLooksValid(#".*\.(epub|mobi)$"#))
        #expect(CategoryRules.regexLooksValid("[(]x"))
        #expect(!CategoryRules.regexLooksValid("(abc"))
        #expect(!CategoryRules.regexLooksValid("abc)"))
        #expect(!CategoryRules.regexLooksValid("[abc"))
        #expect(!CategoryRules.regexLooksValid("abc\\"))
        #expect(CategoryRules.regexLooksValid(""))
    }

    @Test func categoryReorderMovesToTheTargetSlot() throws {
        let list = CustomCategoryDto.builtinDefaults
        let ids = list.map(\.id)
        // 向下拖：a 落到 c 之后。
        let down = try #require(CategoryRules.reorder(list, from: ids[0], to: ids[2]))
        #expect(down.map(\.id).prefix(3) == [ids[1], ids[2], ids[0]])
        // 向上拖回原位。
        let up = try #require(CategoryRules.reorder(down, from: ids[0], to: ids[1]))
        #expect(up.map(\.id) == ids)
        #expect(CategoryRules.reorder(list, from: ids[0], to: ids[0]) == nil)
        #expect(CategoryRules.reorder(list, from: "missing", to: ids[0]) == nil)
    }

    @Test func offsetMoveFollowsOnMoveSemantics() throws {
        let list = ["a", "b", "c", "d"].map { CustomCategoryDto(id: $0, name: $0) }
        func ids(_ moved: [CustomCategoryDto]?) -> [String]? { moved?.map(\.id) }
        #expect(ids(CategoryRules.move(list, from: [0], to: 3)) == ["b", "c", "a", "d"]) // 向下
        #expect(ids(CategoryRules.move(list, from: [3], to: 1)) == ["a", "d", "b", "c"]) // 向上
        #expect(ids(CategoryRules.move(list, from: [0], to: 4)) == ["b", "c", "d", "a"]) // 到末尾
        #expect(CategoryRules.move(list, from: [1], to: 1) == nil)
        #expect(CategoryRules.move(list, from: [1], to: 2) == nil)
        #expect(CategoryRules.move(list, from: [9], to: 0) == nil)
    }

    // MARK: 分类表单校验

    @Test func newCategoryRequiresNameAndExtensions() throws {
        var draft = CategoryDraft(existing: nil)
        #expect(CategoryRules.build(draft, nowMs: 1) == .failure(.nameRequired))
        draft.name = "  "
        #expect(CategoryRules.build(draft, nowMs: 1) == .failure(.nameRequired))
        draft.name = "eBooks"
        #expect(CategoryRules.build(draft, nowMs: 1) == .failure(.extensionsRequired))
        draft.extensionsText = ".EPUB, mobi"
        draft.saveDir = "  /books  "
        let entry = try CategoryRules.build(draft, nowMs: 1_700_000_000_123).get()
        #expect(entry.id == "custom_1700000000123")
        #expect(entry.name == "eBooks" && entry.extensions == ["epub", "mobi"] && entry.regexPattern == "")
        #expect(entry.position == CategoryRules.newPosition && entry.visible && !entry.isBuiltin && entry.builtinType == nil)
        #expect(entry.saveDir == "/books" && entry.icon == "file" && entry.matchMode == "extension")
    }

    @Test func regexModeValidatesPatternAndClearsExtensions() throws {
        var draft = CategoryDraft(existing: nil)
        draft.name = "Rx"
        draft.matchMode = "regex"
        draft.extensionsText = "zip"
        draft.regexText = "(oops"
        #expect(CategoryRules.build(draft, nowMs: 1) == .failure(.regexInvalid))
        draft.regexText = #"  .*\.(epub|mobi)$ "#
        let entry = try CategoryRules.build(draft, nowMs: 1).get()
        #expect(entry.matchMode == "regex" && entry.regexPattern == #".*\.(epub|mobi)$"# && entry.extensions.isEmpty)
    }

    @Test func urlModeCarriesPatternRequiresItAndClearsOtherFields() throws {
        var draft = CategoryDraft(existing: nil)
        draft.name = "Site"
        draft.matchMode = "url"
        draft.extensionsText = "zip"
        draft.regexText = "x"
        draft.urlText = "   "
        #expect(CategoryRules.build(draft, nowMs: 1) == .failure(.urlPatternRequired))
        #expect(CategoryValidationError.urlPatternRequired.i18nKey == "urlPatternRequired")
        draft.urlText = "  https://example.com/*  "
        let entry = try CategoryRules.build(draft, nowMs: 1).get()
        #expect(entry.matchMode == "url" && entry.urlPattern == "https://example.com/*")
        #expect(entry.extensions.isEmpty && entry.regexPattern.isEmpty)

        // 编辑既有网址分类：草稿带出模式与模式文本；未改动保存不丢规则，改动写入新值，切走模式清空。
        var editing = CategoryDraft(existing: entry)
        #expect(editing.matchMode == "url" && editing.urlText == "https://example.com/*")
        editing.saveDir = "/mod"
        #expect(try CategoryRules.build(editing, nowMs: 2).get().urlPattern == "https://example.com/*")
        editing.urlText = "https://mirror.test/*"
        #expect(try CategoryRules.build(editing, nowMs: 2).get().urlPattern == "https://mirror.test/*")
        editing.matchMode = "extension"
        editing.extensionsText = "zip"
        let switched = try CategoryRules.build(editing, nowMs: 2).get()
        #expect(switched.matchMode == "extension" && switched.urlPattern.isEmpty && switched.extensions == ["zip"])
    }

    @Test func builtinCategoriesKeepIdentityAndSkipRequiredChecks() throws {
        let video = try #require(CustomCategoryDto.builtinDefaults.first { $0.builtinType == "video" })
        var draft = CategoryDraft(existing: video)
        draft.name = ""
        draft.extensionsText = ""
        draft.saveDir = "/v"
        let edited = try CategoryRules.build(draft, nowMs: 1).get()
        #expect(edited.id == video.id && edited.isBuiltin && edited.builtinType == "video" && edited.position == video.position)
        #expect(edited.extensions.isEmpty && edited.saveDir == "/v")

        // all / other：不展示匹配规则，规则字段保持原样。
        let other = try #require(CustomCategoryDto.builtinDefaults.first { $0.isOther })
        var otherDraft = CategoryDraft(existing: other)
        otherDraft.extensionsText = "zip"
        otherDraft.matchMode = "regex"
        otherDraft.regexText = "(("
        let kept = try CategoryRules.build(otherDraft, nowMs: 1).get()
        #expect(kept.extensions == other.extensions && kept.regexPattern == other.regexPattern)
    }

    @Test func upsertReplacesOrAppends() {
        let list = [CustomCategoryDto(id: "a", name: "A"), CustomCategoryDto(id: "b", name: "B")]
        let replaced = CategoryRules.upserting(CustomCategoryDto(id: "a", name: "A2"), in: list)
        #expect(replaced.map(\.name) == ["A2", "B"])
        let appended = CategoryRules.upserting(CustomCategoryDto(id: "c", name: "C"), in: list)
        #expect(appended.map(\.id) == ["a", "b", "c"])
    }

    @Test func iconCatalogHasTwentyFiveUniqueNames() {
        #expect(CategoryRules.iconNames.count == 25)
        #expect(Set(CategoryRules.iconNames).count == 25)
        #expect(CategoryRules.iconNames.contains("package2") && CategoryRules.iconNames.contains("hardDrive"))
    }
}
