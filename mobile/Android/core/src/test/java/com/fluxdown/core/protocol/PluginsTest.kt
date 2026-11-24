package com.fluxdown.core.protocol

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.Assert.assertTrue
import java.util.Base64

/** 插件 DTO 解码（真实 wire 形状）与插件设置校验、市场、登录挑战规则（对应 iOS `PluginsProtocolTests`）。 */
class PluginsTest {
    // ───────────── DTO ─────────────

    @Test
    fun siteDefaultsDecodeAndParams() {
        val json = Json.parseOrNull(
            """[{"site":"bilibili.com","identity":"a","candidates":["a","b"],"automatic":true},{"site":"x.com","identity":"b"},{"identity":"z"}]""",
        )
        val list = PluginSiteDefault.listFromJson(json)
        assertEquals(
            listOf(
                PluginSiteDefault("bilibili.com", "a", listOf("a", "b"), true),
                PluginSiteDefault("x.com", "b", emptyList(), false),
            ),
            list,
        )
        assertEquals("""{"site":"x.com","identity":null}""", pluginSetSiteDefaultParams("x.com", null).toJson())
    }

    @Test
    fun pluginDtoDecodesWireShapeAndToleratesMissingDefaults() {
        val json = """
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
        val plugins = PluginDto.listFromJson(Json.parse(json))
        assertEquals(2, plugins.size)
        assertTrue(plugins[0].loadFailed)
        assertEquals("private", plugins[0].source)
        assertEquals("hd", plugins[0].settings[0].defaultValue)
        assertEquals("string", plugins[0].settings[0].settingType)
        assertEquals(1.0, plugins[0].settings[1].min)
        assertEquals(10.0, plugins[0].settings[1].max)
        assertTrue(plugins[0].settings[1].options.isEmpty())
        assertEquals("CircuitBreaker", plugins[0].disabledReason)
        assertEquals(mapOf("quality" to "hd"), plugins[0].settingsValues)
        // 缺省字段：loadStatus 默认 Loaded，其余集合为空。
        assertEquals("Loaded", plugins[1].loadStatus)
        assertFalse(plugins[1].loadFailed)
        assertEquals("", plugins[1].source)
        assertTrue(plugins[1].permissions.isEmpty())
        assertFalse(plugins[1].authSupported)
        assertTrue(plugins[1].description.isEmpty())
    }

    @Test
    fun authMarketAndInstalledDecode() {
        val auth = PluginAuthResponse.fromJson(
            Json.parse("""{"status":"pending","sessionId":"s1","challenge":"abc","challengeType":"qrcode","message":""}"""),
        )
        assertNotNull(auth)
        assertEquals("pending", auth!!.status)
        assertEquals("abc", auth.challenge)
        assertNull(auth.authRef)
        val installed = InstalledPlugin.fromJson(Json.parse("""{"identity":"x"}"""))
        assertTrue(installed!!.missingComponents.isEmpty())
        val market = MarketEntry.listFromJson(
            Json.parse(
                """[{"pluginId":"a","version":"1.0.0","sequence":7,"contentHash":"h","name":"","yanked":"none","tags":["t"],"permissions":["auth"]}]""",
            ),
        )
        assertEquals("a", market[0].displayName)
        assertTrue(market[0].installable)
        assertEquals(7L, market[0].sequence)
    }

    @Test
    fun requestParamsMatchWireShape() {
        val request = PluginAuthRequest(identity = "i", action = "poll", authRef = "r", sessionId = "s").toJson().toJson()
        assertEquals(
            """{"identity":"i","action":"poll","site":"","authRef":"r","sessionId":"s","input":""}""",
            request,
        )
        assertEquals("""{"pluginId":"p"}""", pluginMarketInstallParams("p", null).toJson())
        assertEquals("""{"pluginId":"p","version":"1.0.0"}""", pluginMarketInstallParams("p", "1.0.0").toJson())
        assertEquals("""{"identity":"i","entries":{"k":"v"}}""", pluginUpdateSettingsParams("i", mapOf("k" to "v")).toJson())
    }

    @Test
    fun uploadUrlDropsPathAndMapsWebSocketSchemes() {
        assertEquals("http://h:17800/api/web/blobs/plugins", PluginPackage.uploadUrl("ws://h:17800/rpc"))
        assertEquals("https://h/api/web/blobs/plugins", PluginPackage.uploadUrl(" wss://h/x?y=1 "))
        assertEquals("http://10.0.0.2:80/api/web/blobs/plugins", PluginPackage.uploadUrl("http://10.0.0.2:80/"))
        assertNull(PluginPackage.uploadUrl("ftp://h"))
        assertNull(PluginPackage.uploadUrl("h:17800"))
        assertNull(PluginPackage.uploadUrl("not a url"))
    }

    // ───────────── 设置校验 ─────────────

    private fun field(
        key: String = "k",
        type: String = "string",
        widget: String = "text",
        options: List<String> = emptyList(),
        required: Boolean = false,
        min: Double? = null,
        max: Double? = null,
        pattern: String? = null,
    ) = PluginSettingField(
        key = key, settingType = type, widget = widget,
        options = options.map { PluginSettingOption(it, it) },
        required = required, min = min, max = max, pattern = pattern,
    )

    @Test
    fun validationFollowsRequiredNumberMinMaxSelectOrder() {
        assertEquals(PluginFieldError.Required, PluginSettings.validate(field(required = true), "  "))
        assertNull(PluginSettings.validate(field(), "  "))
        val number = field(type = "number", widget = "number", min = 1.0, max = 10.0)
        assertEquals(PluginFieldError.Number, PluginSettings.validate(number, "abc"))
        assertEquals(PluginFieldError.Number, PluginSettings.validate(number, "0x10"))
        assertEquals(PluginFieldError.Number, PluginSettings.validate(number, "inf"))
        assertNull(PluginSettings.validate(number, ""))
        assertEquals(PluginFieldError.Min("1"), PluginSettings.validate(number, "0"))
        assertEquals(PluginFieldError.Max("10"), PluginSettings.validate(number, "10.5"))
        assertNull(PluginSettings.validate(number, " 5 "))
        assertNull(PluginSettings.validate(number, "1e1"))
        assertEquals(PluginFieldError.Min("1"), PluginSettings.validate(number, ".5"))
        assertNull(PluginSettings.validate(number, "5."))
        assertEquals(PluginFieldError.Min("0.5"), PluginSettings.validate(field(type = "number", min = 0.5), "0.25"))
        val select = field(widget = "select", options = listOf("a", "b"))
        assertEquals(PluginFieldError.Select, PluginSettings.validate(select, "c"))
        assertNull(PluginSettings.validate(select, "a"))
        // 没有选项的 select 不校验成员。
        assertNull(PluginSettings.validate(field(widget = "select"), "zzz"))
    }

    @Test
    fun validateAllCollectsErrorsByKey() {
        val fields = listOf(field("a", required = true), field("b", type = "number", min = 1.0), field("c"))
        val errors = PluginSettings.validateAll(fields, mapOf("a" to "", "b" to "0", "c" to "x"))
        assertEquals(mapOf("a" to PluginFieldError.Required, "b" to PluginFieldError.Min("1")), errors)
    }

    @Test
    fun initialValuesAndRangeHint() {
        val toggle = PluginSettingField(key = "t", widget = "toggle")
        val withDefault = PluginSettingField(key = "d", defaultValue = "x")
        val plain = PluginSettingField(key = "p")
        val values = PluginSettings.initialValues(listOf(toggle, withDefault, plain), mapOf("p" to "saved"))
        assertEquals(mapOf("t" to "false", "d" to "x", "p" to "saved"), values)
        assertEquals("", PluginSettings.initialValue(withDefault, mapOf("d" to "")))
        assertEquals("1 – 10", PluginSettings.rangeHint(field(min = 1.0, max = 10.0)))
        assertEquals("≥ 1", PluginSettings.rangeHint(field(min = 1.0)))
        assertEquals("≤ 2.5", PluginSettings.rangeHint(field(max = 2.5)))
        assertNull(PluginSettings.rangeHint(field()))
    }

    @Test
    fun submitEntriesSkipOptionalEmptyNumberSelectPatternFields() {
        val fields = listOf(
            field("num", type = "number"), field("sel", widget = "select", options = listOf("a")),
            field("pat", pattern = "^x"), field("req", widget = "select", options = listOf("a"), required = true),
            field("free"), field("selEmptyOk", widget = "select", options = listOf("", "a")),
        )
        val entries = PluginSettings.submitEntries(
            fields,
            mapOf("num" to " ", "sel" to "", "pat" to "", "req" to "", "free" to "", "selEmptyOk" to ""),
        )
        assertEquals(mapOf("req" to "", "free" to "", "selEmptyOk" to ""), entries)
        val trimmed = PluginSettings.submitEntries(listOf(field("num", type = "number")), mapOf("num" to " 3 "))
        assertEquals(mapOf("num" to "3"), trimmed)
    }

    // ───────────── 市场 ─────────────

    private fun entry(id: String, version: String, seq: Long, yanked: String = "none", perms: List<String> = emptyList()) =
        MarketEntry(pluginId = id, version = version, sequence = seq, yanked = yanked, permissions = perms)

    @Test
    fun latestPerPluginPrefersInstallableAndHidesFullyYankedKeepingFirstSeenOrder() {
        val list = listOf(
            entry("a", "1.0.0", 1), entry("b", "1.0.0", 5, yanked = "malicious"),
            entry("a", "1.1.0", 2), entry("b", "1.1.0", 6, yanked = "deprecated"),
            entry("a", "1.2.0", 3, yanked = "vulnerable"), entry("c", "1.0.0", 7),
        )
        val latest = PluginMarket.latestPerPlugin(list)
        assertEquals(listOf("a", "c"), latest.map { it.pluginId })
        assertEquals("1.1.0", latest[0].version)
    }

    @Test
    fun versionComparisonAndActions() {
        assertTrue(PluginMarket.versionNewer("1.10.0", "1.9.9"))
        assertTrue(PluginMarket.versionNewer("v2.0.0-rc1", "1.9.9"))
        assertFalse(PluginMarket.versionNewer("1.0.0", "1.0.0"))
        assertFalse(PluginMarket.versionNewer("1.0", "0.9.0"))
        val market = entry("a", "2.0.0", 2, perms = listOf("ffmpeg", "auth"))
        assertEquals(MarketAction.Install, PluginMarket.action(market, null))
        assertEquals(MarketAction.Unavailable, PluginMarket.action(entry("a", "2.0.0", 2, yanked = "malicious"), null))
        val old = PluginDto(identity = "a", name = "A", version = "1.0.0", permissions = listOf("ffmpeg"))
        assertEquals(MarketAction.Update, PluginMarket.action(market, old))
        assertEquals(listOf("auth"), PluginMarket.permissionsToConfirm(market, old))
        assertEquals(listOf("ffmpeg", "auth"), PluginMarket.permissionsToConfirm(market, null))
        val dev = PluginDto(identity = "a", name = "A", version = "1.0.0", devMode = true)
        assertEquals(MarketAction.Installed, PluginMarket.action(market, dev))
        assertEquals("marketYankedMalicious", PluginMarket.yankedLabelKey("malicious"))
        assertNull(PluginMarket.yankedLabelKey("none"))
        val yankedEntries = listOf(entry("a", "1.0.0", 1, yanked = "vulnerable"))
        assertEquals("vulnerable", PluginMarket.installedVersionYanked(yankedEntries, old))
        assertNull(PluginMarket.installedVersionYanked(yankedEntries, dev))
    }

    @Test
    fun marketFilterMatchesNameIdDescriptionAuthorAndTags() {
        val list = listOf(
            MarketEntry(pluginId = "dev.x.alpha", version = "1.0.0", name = "Alpha", description = "Video grabber", author = "Zed", tags = listOf("media")),
            MarketEntry(pluginId = "dev.x.beta", version = "1.0.0", name = "Beta"),
        )
        assertEquals(2, PluginMarket.filter(list, "").size)
        assertEquals(listOf("Alpha"), PluginMarket.filter(list, "GRABBER").map { it.name })
        assertEquals(1, PluginMarket.filter(list, "zed").size)
        assertEquals(1, PluginMarket.filter(list, "MEDIA").size)
        assertEquals(listOf("Beta"), PluginMarket.filter(list, "dev.x.beta").map { it.name })
    }

    // ───────────── 登录挑战 ─────────────

    @Test
    fun pendingPollKeepsPreviousChallengeButTerminalStatesDoNot() {
        val begin = PluginAuth.apply(
            PluginAuthState(),
            PluginAuthResponse(status = "pending", sessionId = "s", challenge = "QR", challengeType = "qrcode"),
            wasLogout = false,
        )
        assertTrue(begin.sessionPending)
        assertTrue(begin.isQrChallenge)
        assertEquals("QR", begin.challenge)
        val poll = PluginAuth.apply(begin, PluginAuthResponse(status = "pending", sessionId = "s"), wasLogout = false)
        assertEquals("QR", poll.challenge)
        assertEquals("qrcode", poll.challengeType)
        val done = PluginAuth.apply(poll, PluginAuthResponse(status = "success", sessionId = "s", authRef = "ref"), wasLogout = false)
        assertNull(done.challenge)
        assertTrue(done.loggedIn)
        val out = PluginAuth.apply(done, PluginAuthResponse(status = "success", sessionId = "s2", authRef = "ref"), wasLogout = true)
        assertFalse(out.loggedIn)
        assertTrue(out.authRef.isEmpty())
        assertTrue(out.sessionId.isEmpty())
    }

    @Test
    fun challengeImagesOnlyAcceptSafeBase64DataUrls() {
        val raw = byteArrayOf(0x89.toByte(), 0x50, 0x4E, 0x47)
        val png = Base64.getEncoder().encodeToString(raw)
        assertTrue(raw.contentEquals(PluginAuth.dataImageBytes("data:image/png;base64,$png")))
        assertNotNull(PluginAuth.dataImageBytes("DATA:IMAGE/PNG;charset=x;BASE64,$png"))
        assertNull(PluginAuth.dataImageBytes("data:text/html;base64,$png"))
        assertNull(PluginAuth.dataImageBytes("data:image/png,$png"))
        assertNull(PluginAuth.dataImageBytes("data:image/png;base64,!!!"))
        assertNull(PluginAuth.dataImageBytes("data:image/png;base64,"))
        assertNull(PluginAuth.dataImageBytes("https://x/y.png"))
        val huge = "data:image/png;base64," + "A".repeat(PluginAuth.MAX_CHALLENGE_DATA_URL_LENGTH)
        assertNull(PluginAuth.dataImageBytes(huge))
    }

    @Test
    fun challengeTextTruncatesAndOnlyHttpLinksAreSafe() {
        val long = "x".repeat(600)
        assertEquals(PluginAuth.CHALLENGE_TEXT_LIMIT + 1, PluginAuth.truncate(long).length)
        assertEquals("short", PluginAuth.truncate("short"))
        assertNotNull(PluginAuth.safeHttpUrl("https://example.com/a"))
        assertNull(PluginAuth.safeHttpUrl("javascript:alert(1)"))
        assertNull(PluginAuth.safeHttpUrl("weixin://dl/login"))
        assertNull(PluginAuth.safeHttpUrl("not a url"))
    }
}
