import FluxDomain
import Foundation
import Testing

/// 插件 / 组件 / 网关 DTO 解码（真实 wire 形状）与插件设置校验、市场、登录挑战规则。
struct PluginsProtocolTests {
    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try ProtocolJSON.decode(type, from: Data(json.utf8), what: "test")
    }

    // MARK: DTO

    @Test func pluginDtoDecodesWireShapeAndToleratesMissingDefaults() throws {
        let json = """
        [{
          "identity": "dev.example.bili", "name": "Bili", "version": "1.2.3",
          "description": "d", "homepage": "https://example.dev", "enabled": true, "devMode": false,
          "disabledReason": "CircuitBreaker",
          "settings": [{
            "key": "quality", "title": "Quality", "description": "", "type": "string", "widget": "select",
            "options": [{"value": "hd", "label": "HD"}], "default": "hd", "required": false,
            "min": null, "max": null, "pattern": null, "helperScript": null, "helperLabel": null
          }, {
            "key": "retries", "title": "Retries", "type": "number", "widget": "number", "min": 1, "max": 10
          }],
          "settingsValues": {"quality": "hd"},
          "permissions": ["ffmpeg"], "authSupported": true, "subscriptionProviderIds": ["p"],
          "loadStatus": "Failed", "loadError": "boom", "source": "private"
        }, {
          "identity": "min", "name": "Min", "version": "0.0.1", "enabled": false, "devMode": true,
          "disabledReason": "None", "settings": [], "settingsValues": {}
        }]
        """
        let plugins = try decode([PluginDto].self, json)
        #expect(plugins.count == 2)
        #expect(plugins[0].loadFailed)
        #expect(plugins[0].settings[0].defaultValue == "hd")
        #expect(plugins[0].settings[0].settingType == "string")
        #expect(plugins[0].settings[1].min == 1 && plugins[0].settings[1].max == 10)
        #expect(plugins[0].settings[1].options.isEmpty)
        #expect(plugins[0].disabledReason == "CircuitBreaker")
        #expect(plugins[0].source == "private")
        // 缺省字段：loadStatus 默认 Loaded，其余集合为空。
        #expect(plugins[1].loadStatus == "Loaded" && !plugins[1].loadFailed)
        #expect(plugins[1].permissions.isEmpty && !plugins[1].authSupported && plugins[1].description.isEmpty)
        #expect(plugins[1].source.isEmpty)
    }

    @Test func authAndMarketAndInstalledDecode() throws {
        let auth = try decode(PluginAuthResponse.self, #"{"status":"pending","sessionId":"s1","challenge":"abc","challengeType":"qrcode","message":""}"#)
        #expect(auth.status == "pending" && auth.challenge == "abc" && auth.authRef == nil)
        let installed = try decode(InstalledPlugin.self, #"{"identity":"x"}"#)
        #expect(installed.missingComponents.isEmpty)
        let market = try decode([MarketEntry].self, """
        [{"pluginId":"a","version":"1.0.0","sequence":7,"contentHash":"h","name":"","yanked":"none","tags":["t"],"permissions":["auth"]}]
        """)
        #expect(market[0].displayName == "a" && market[0].installable && market[0].sequence == 7)
    }

    @Test func authRequestOmitsNothingAndEncodesCamelCase() throws {
        let data = try ProtocolJSON.encode(PluginAuthRequest(identity: "i", action: "poll", authRef: "r", sessionId: "s"), what: "t")
        let text = String(decoding: data, as: UTF8.self)
        #expect(text == #"{"action":"poll","authRef":"r","identity":"i","input":"","sessionId":"s","site":""}"#)
        let params = try ProtocolJSON.encode(PluginMarketInstallParams(pluginId: "p", version: nil), what: "t")
        #expect(String(decoding: params, as: UTF8.self) == #"{"pluginId":"p"}"#)
    }

    @Test func siteDefaultParamsEncodeNullWhenRemoving() throws {
        let remove = try ProtocolJSON.encode(PluginSetSiteDefaultParams(site: "bilibili.com", identity: nil), what: "t")
        #expect(String(decoding: remove, as: UTF8.self) == #"{"identity":null,"site":"bilibili.com"}"#)
        let set = try ProtocolJSON.encode(PluginSetSiteDefaultParams(site: "bilibili.com", identity: "a.b"), what: "t")
        #expect(String(decoding: set, as: UTF8.self) == #"{"identity":"a.b","site":"bilibili.com"}"#)
        let rows = try ProtocolJSON.decode(
            [PluginSiteDefault].self,
            from: Data(#"[{"site":"bilibili.com","identity":"a","candidates":["a","b"],"automatic":true}]"#.utf8),
            what: "t"
        )
        #expect(rows == [PluginSiteDefault(site: "bilibili.com", identity: "a", candidates: ["a", "b"], automatic: true)])
    }

    @Test func componentStatusUsesAdjacentTagAndToleratesUnknownComponent() throws {
        let json = """
        [{"component":"ffmpeg","status":{"source":"managed","path":"/x/ffmpeg","version":"7.1","managedVersion":"7.1","systemPath":"","managedSupported":true}},
         {"component":"ytdlp","status":{"source":"none","path":"","version":"","managedVersion":"","systemPath":"/usr/bin/yt-dlp","managedSupported":false}},
         {"component":"future","status":{"source":"system"}}]
        """
        let list = try decode([ComponentStatusDto].self, json)
        #expect(list[0].component == .ffmpeg && list[0].status.hasManagedInstall)
        #expect(list[1].component == .ytdlp && !list[1].status.managedSupported)
        #expect(list[2].component == .unknown("future") && list[2].status.source == "system")
        let versions = try decode(ComponentVersions.self, #"{"versions":["7.1","7.0"],"latestStable":"7.1"}"#)
        #expect(versions.defaultSelection(current: "7.0") == "7.0")
        #expect(versions.defaultSelection(current: "6") == "7.1")
        let params = try ProtocolJSON.encode(ComponentInstallParams(component: .ytdlp, version: nil), what: "t")
        #expect(String(decoding: params, as: UTF8.self) == #"{"component":"ytdlp"}"#)
    }

    @Test func gatewayStatusDefaultsAndPatchOmitsNil() throws {
        let status = try decode(GatewayStatusDto.self, """
        {"takeoverEnabled":true,"jsonrpcEnabled":false,"apiEnabled":true,"mcpEnabled":false,"corsEnabled":false,"userTokenConfigured":true}
        """)
        #expect(status.port == 17800 && status.portEditable && !status.lanEnabled)
        let patch = try ProtocolJSON.encode(GatewayFeature.mcp.patch(true), what: "t")
        #expect(String(decoding: patch, as: UTF8.self) == #"{"mcpEnabled":true}"#)
        let clear = try ProtocolJSON.encode(GatewayPatchParams(userToken: ""), what: "t")
        #expect(String(decoding: clear, as: UTF8.self) == #"{"userToken":""}"#)
    }

    // MARK: 设置校验

    private func field(
        _ key: String = "k",
        type: String = "string",
        widget: String = "text",
        options: [String] = [],
        required: Bool = false,
        min: Double? = nil,
        max: Double? = nil,
        pattern: String? = nil
    ) -> PluginSettingField {
        PluginSettingField(
            key: key, settingType: type, widget: widget,
            options: options.map { PluginSettingOption(value: $0, label: $0) },
            required: required, min: min, max: max, pattern: pattern
        )
    }

    @Test func validationFollowsRequiredNumberMinMaxSelectOrder() {
        #expect(PluginSettings.validate(field(required: true), raw: "  ") == .required)
        #expect(PluginSettings.validate(field(), raw: "  ") == nil)
        let number = field(type: "number", widget: "number", min: 1, max: 10)
        #expect(PluginSettings.validate(number, raw: "abc") == .number)
        #expect(PluginSettings.validate(number, raw: "0x10") == .number)
        #expect(PluginSettings.validate(number, raw: "inf") == .number)
        #expect(PluginSettings.validate(number, raw: "") == nil)
        #expect(PluginSettings.validate(number, raw: "0") == .min("1"))
        #expect(PluginSettings.validate(number, raw: "10.5") == .max("10"))
        #expect(PluginSettings.validate(number, raw: " 5 ") == nil)
        #expect(PluginSettings.validate(number, raw: "1e1") == nil)
        #expect(PluginSettings.validate(number, raw: ".5") == .min("1"))
        #expect(PluginSettings.validate(number, raw: "5.") == nil)
        #expect(PluginSettings.validate(field(type: "number", min: 0.5), raw: "0.25") == .min("0.5"))
        let select = field(widget: "select", options: ["a", "b"])
        #expect(PluginSettings.validate(select, raw: "c") == .select)
        #expect(PluginSettings.validate(select, raw: "a") == nil)
        // 没有选项的 select 不校验成员。
        #expect(PluginSettings.validate(field(widget: "select"), raw: "zzz") == nil)
    }

    @Test func validateAllCollectsErrorsByKey() {
        let fields = [field("a", required: true), field("b", type: "number", min: 1), field("c")]
        let errors = PluginSettings.validateAll(fields, values: ["a": "", "b": "0", "c": "x"])
        #expect(errors == ["a": .required, "b": .min("1")])
    }

    @Test func initialValuesAndRangeHint() {
        let toggle = PluginSettingField(key: "t", widget: "toggle")
        let withDefault = PluginSettingField(key: "d", defaultValue: "x")
        let plain = PluginSettingField(key: "p")
        let values = PluginSettings.initialValues([toggle, withDefault, plain], saved: ["p": "saved"])
        #expect(values == ["t": "false", "d": "x", "p": "saved"])
        #expect(PluginSettings.initialValue(withDefault, saved: ["d": ""]) == "")
        #expect(PluginSettings.rangeHint(field(min: 1, max: 10)) == "1 – 10")
        #expect(PluginSettings.rangeHint(field(min: 1)) == "≥ 1")
        #expect(PluginSettings.rangeHint(field(max: 2.5)) == "≤ 2.5")
        #expect(PluginSettings.rangeHint(field()) == nil)
    }

    @Test func submitEntriesSkipOptionalEmptyNumberSelectPatternFields() {
        let fields = [
            field("num", type: "number"), field("sel", widget: "select", options: ["a"]),
            field("pat", pattern: "^x"), field("req", widget: "select", options: ["a"], required: true),
            field("free"), field("selEmptyOk", widget: "select", options: ["", "a"]),
        ]
        let entries = PluginSettings.submitEntries(
            fields,
            values: ["num": " ", "sel": "", "pat": "", "req": "", "free": "", "selEmptyOk": ""]
        )
        #expect(entries == ["req": "", "free": "", "selEmptyOk": ""])
        let trimmed = PluginSettings.submitEntries([field("num", type: "number")], values: ["num": " 3 "])
        #expect(trimmed == ["num": "3"])
    }

    // MARK: 市场

    private func entry(_ id: String, _ version: String, seq: UInt64, yanked: String = "none", perms: [String] = []) -> MarketEntry {
        MarketEntry(pluginId: id, version: version, sequence: seq, yanked: yanked, permissions: perms)
    }

    @Test func latestPerPluginPrefersInstallableAndHidesFullyYankedKeepingFirstSeenOrder() {
        let list = [
            entry("a", "1.0.0", seq: 1), entry("b", "1.0.0", seq: 5, yanked: "malicious"),
            entry("a", "1.1.0", seq: 2), entry("b", "1.1.0", seq: 6, yanked: "deprecated"),
            entry("a", "1.2.0", seq: 3, yanked: "vulnerable"), entry("c", "1.0.0", seq: 7),
        ]
        let latest = PluginMarket.latestPerPlugin(list)
        #expect(latest.map(\.pluginId) == ["a", "c"])
        #expect(latest[0].version == "1.1.0")
    }

    @Test func versionComparisonAndActions() {
        #expect(PluginMarket.versionNewer("1.10.0", than: "1.9.9"))
        #expect(PluginMarket.versionNewer("v2.0.0-rc1", than: "1.9.9"))
        #expect(!PluginMarket.versionNewer("1.0.0", than: "1.0.0"))
        #expect(!PluginMarket.versionNewer("1.0", than: "0.9.0"))
        let market = entry("a", "2.0.0", seq: 2, perms: ["ffmpeg", "auth"])
        #expect(PluginMarket.action(for: market, installed: nil) == .install)
        #expect(PluginMarket.action(for: entry("a", "2.0.0", seq: 2, yanked: "malicious"), installed: nil) == .unavailable)
        let old = PluginDto(identity: "a", name: "A", version: "1.0.0", permissions: ["ffmpeg"])
        #expect(PluginMarket.action(for: market, installed: old) == .update)
        #expect(PluginMarket.permissionsToConfirm(market, installed: old) == ["auth"])
        #expect(PluginMarket.permissionsToConfirm(market, installed: nil) == ["ffmpeg", "auth"])
        let dev = PluginDto(identity: "a", name: "A", version: "1.0.0", devMode: true)
        #expect(PluginMarket.action(for: market, installed: dev) == .installed)
        #expect(PluginMarket.yankedLabelKey("malicious") == "marketYankedMalicious")
        #expect(PluginMarket.yankedLabelKey("none") == nil)
        let yankedEntries = [entry("a", "1.0.0", seq: 1, yanked: "vulnerable")]
        #expect(PluginMarket.installedVersionYanked(yankedEntries, plugin: old) == "vulnerable")
        #expect(PluginMarket.installedVersionYanked(yankedEntries, plugin: dev) == nil)
    }

    @Test func marketFilterMatchesNameIdDescriptionAuthorAndTags() {
        let list = [
            MarketEntry(pluginId: "dev.x.alpha", version: "1.0.0", name: "Alpha", description: "Video grabber", author: "Zed", tags: ["media"]),
            MarketEntry(pluginId: "dev.x.beta", version: "1.0.0", name: "Beta"),
        ]
        #expect(PluginMarket.filter(list, query: "").count == 2)
        #expect(PluginMarket.filter(list, query: "GRABBER").map(\.name) == ["Alpha"])
        #expect(PluginMarket.filter(list, query: "zed").count == 1)
        #expect(PluginMarket.filter(list, query: "MEDIA").count == 1)
        #expect(PluginMarket.filter(list, query: "dev.x.beta").map(\.name) == ["Beta"])
    }

    // MARK: 登录挑战

    @Test func pendingPollKeepsPreviousChallengeButTerminalStatesDoNot() {
        let begin = PluginAuth.apply(
            PluginAuthState(),
            response: PluginAuthResponse(status: "pending", sessionId: "s", challenge: "QR", challengeType: "qrcode", authRef: nil),
            wasLogout: false
        )
        #expect(begin.sessionPending && begin.isQrChallenge && begin.challenge == "QR")
        let poll = PluginAuth.apply(begin, response: PluginAuthResponse(status: "pending", sessionId: "s"), wasLogout: false)
        #expect(poll.challenge == "QR" && poll.challengeType == "qrcode")
        let done = PluginAuth.apply(poll, response: PluginAuthResponse(status: "success", sessionId: "s", authRef: "ref"), wasLogout: false)
        #expect(done.challenge == nil && done.loggedIn)
        let out = PluginAuth.apply(done, response: PluginAuthResponse(status: "success", sessionId: "s2", authRef: "ref"), wasLogout: true)
        #expect(!out.loggedIn && out.authRef.isEmpty && out.sessionId.isEmpty)
    }

    @Test func challengeImagesOnlyAcceptSafeBase64DataURLs() {
        let png = Data([0x89, 0x50, 0x4E, 0x47]).base64EncodedString()
        #expect(PluginAuth.dataImageBytes("data:image/png;base64,\(png)") == Data([0x89, 0x50, 0x4E, 0x47]))
        #expect(PluginAuth.dataImageBytes("DATA:IMAGE/PNG;charset=x;BASE64,\(png)") != nil)
        #expect(PluginAuth.dataImageBytes("data:text/html;base64,\(png)") == nil)
        #expect(PluginAuth.dataImageBytes("data:image/png,\(png)") == nil)
        #expect(PluginAuth.dataImageBytes("data:image/png;base64,!!!") == nil)
        #expect(PluginAuth.dataImageBytes("data:image/png;base64,") == nil)
        #expect(PluginAuth.dataImageBytes("https://x/y.png") == nil)
        let huge = "data:image/png;base64," + String(repeating: "A", count: PluginAuth.maxChallengeDataURLLength)
        #expect(PluginAuth.dataImageBytes(huge) == nil)
    }

    @Test func challengeTextTruncatesAndOnlyHttpLinksAreSafe() {
        let long = String(repeating: "x", count: 600)
        #expect(PluginAuth.truncate(long).count == PluginAuth.challengeTextLimit + 1)
        #expect(PluginAuth.truncate("short") == "short")
        #expect(PluginAuth.safeHTTPURL("https://example.com/a") != nil)
        #expect(PluginAuth.safeHTTPURL("javascript:alert(1)") == nil)
        #expect(PluginAuth.safeHTTPURL("weixin://dl/login") == nil)
        #expect(PluginAuth.safeHTTPURL("not a url") == nil)
    }
}
