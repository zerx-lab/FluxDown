package com.fluxdown.core.capture

import com.fluxdown.core.model.Queue
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.JsonValue
import com.fluxdown.core.protocol.bool
import com.fluxdown.core.protocol.get
import com.fluxdown.core.protocol.int
import com.fluxdown.core.protocol.jsonObject
import com.fluxdown.core.protocol.str
import com.fluxdown.core.store.HostState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** 外部唤起载荷解析与免打扰建任务计划。 */
class ExternalDownloadTest {
    // ── 浏览器「外部下载器」VIEW ──

    @Test fun viewCarriesBrowserExtrasAsRequestContext() {
        val d = ExternalIntake.fromView(
            "  https://cdn.test/f.zip?t=1  ",
            userAgent = " Mozilla/5.0 X ",
            cookie = "sid=1; a=2",
            referer = " https://site.test/page ",
        )!!
        assertEquals("https://cdn.test/f.zip?t=1", d.url)
        assertEquals(mapOf("User-Agent" to "Mozilla/5.0 X"), d.headers)
        assertEquals("sid=1; a=2", d.cookies)
        assertEquals("https://site.test/page", d.referrer)
        assertEquals("", d.fileName)
    }

    @Test fun viewWithoutExtrasAndNonDownloadableDataAreHandled() {
        assertEquals(ExternalDownload("magnet:?xt=urn:btih:abc"), ExternalIntake.fromView("magnet:?xt=urn:btih:abc"))
        assertEquals("ed2k://|file|a.iso|1|ABC|/", ExternalIntake.fromView("ed2k://|file|a.iso|1|ABC|/")?.url)
        assertNull(ExternalIntake.fromView("content://media/1"))
        assertNull(ExternalIntake.fromView("   "))
        assertNull(ExternalIntake.fromView(null))
    }

    @Test fun extensionIntentExtrasCarryFileNameAndHeaders() {
        val d = ExternalIntake.fromView(
            "https://cdn.test/a.zip",
            userAgent = "UA/extra",
            fileName = "  报告 v2;final.zip ",
            headersJson = """{"user-agent":"UA/json","Authorization":"Bearer x"}""",
        )!!
        assertEquals("报告 v2;final.zip", d.fileName)
        // 请求头里已有 User-Agent（不区分大小写）时不再用 extra 覆盖或重复。
        assertEquals(mapOf("user-agent" to "UA/json", "Authorization" to "Bearer x"), d.headers)

        val noUaInJson = ExternalIntake.fromView("magnet:?xt=urn:btih:abc", userAgent = "UA/1", headersJson = """{"X-T":"1"}""")!!
        assertEquals(mapOf("X-T" to "1", "User-Agent" to "UA/1"), noUaInJson.headers)
        // 损坏的请求头 JSON 整体丢弃，不影响其余字段。
        assertEquals(mapOf("User-Agent" to "UA/1"), ExternalIntake.fromView("https://h.test/x", userAgent = "UA/1", headersJson = "{oops")!!.headers)
    }

    // ── fluxdown://download 协议 ──

    @Test fun protocolDecodesQueryParameters() {
        val headers = """{"User-Agent":"UA/1","X-Token":"t"}"""
        val data = "FluxDown://Download?url=" + enc("https://x.test/a b.zip") +
            "&filename=" + enc(" 名字.zip ") + "&cookies=" + enc("k=v; q=+") +
            "&referrer=" + enc(" https://x.test/ ") + "&headers=" + enc(headers) + "&url=https://ignored.test/"
        val d = ExternalIntake.fromView(data)!!
        // 链接从参数里提取首个 URL，遇空白截断。
        assertEquals("https://x.test/a", d.url)
        assertEquals("名字.zip", d.fileName)
        assertEquals("k=v; q=+", d.cookies)
        assertEquals("https://x.test/", d.referrer)
        assertEquals(mapOf("User-Agent" to "UA/1", "X-Token" to "t"), d.headers)
        // 查询串语义：`+` = 空格；非法百分号序列原样保留。
        assertEquals("a b%zz", ExternalIntake.fromView("fluxdown://download?url=https://x.test/&filename=a+b%zz")?.fileName)
    }

    @Test fun protocolRequiresDownloadHostAndUrl() {
        assertNull(ExternalIntake.fromView("fluxdown://open?url=https://x.test/a.zip"))
        assertNull(ExternalIntake.fromView("fluxdown://download?filename=a.zip"))
        assertNull(ExternalIntake.fromView("fluxdown://download?url=%20%20"))
        assertNull(ExternalIntake.fromView("fluxdown://download?url=not-a-link"))
    }

    // ── 分享文本 ──

    @Test fun sharedTextExtractsFirstLink() {
        assertEquals("https://x.test/f.zip", ExternalIntake.fromSharedText("看看这个\u3000https://x.test/f.zip 很好用")?.url)
        assertEquals("magnet:?xt=urn:btih:abc", ExternalIntake.fromSharedText("magnet:?xt=urn:btih:abc https://x.test")?.url)
        assertEquals("ftp://x.test/a", ExternalIntake.fromSharedText("FTP://x.test/a")?.url?.lowercase())
        assertNull(ExternalIntake.fromSharedText("no links here"))
        assertNull(ExternalIntake.fromSharedText(null))
    }

    // ── 截断上限 ──

    @Test fun headersAreBoundedAndTyped() {
        val fields = LinkedHashMap<String, Any?>()
        fields["n"] = 1
        fields["k".repeat(ExternalIntake.MAX_HEADER_KEY_LEN + 1)] = "skip"
        fields["Long"] = "v".repeat(ExternalIntake.MAX_HEADER_VALUE_LEN + 5)
        for (i in 0 until 100) fields["H$i"] = "$i"
        val decoded = ExternalIntake.decodeHeaders(JsonValue.from(fields).toJson())
        assertEquals(ExternalIntake.MAX_HEADERS, decoded.size)
        assertEquals(ExternalIntake.MAX_HEADER_VALUE_LEN, decoded.getValue("Long").length)
        assertFalse("n" in decoded)
        assertEquals("Long", decoded.keys.first())
        assertEquals(emptyMap<String, String>(), ExternalIntake.decodeHeaders("[1]"))
        assertEquals(emptyMap<String, String>(), ExternalIntake.decodeHeaders("{broken"))
        val huge = """{"a":"${"x".repeat(ExternalIntake.MAX_HEADERS_JSON_LEN)}"}"""
        assertEquals(emptyMap<String, String>(), ExternalIntake.decodeHeaders(huge))
    }

    @Test fun oversizedFieldsAreTruncated() {
        val url = "https://x.test/" + "a".repeat(ExternalIntake.MAX_URL_LEN)
        val d = ExternalIntake.fromView(url, cookie = "c".repeat(ExternalIntake.MAX_COOKIES_LEN + 1))!!
        assertEquals(ExternalIntake.MAX_URL_LEN, d.url.length)
        assertEquals(ExternalIntake.MAX_COOKIES_LEN, d.cookies.length)
    }

    // ── 免打扰 ──

    private fun state(prefs: Map<String, Any?>, config: Map<String, String> = mapOf("default_save_dir" to "/dl")): HostState =
        HostState(
            queues = listOf(Queue("later", "Later"), Queue(Queue.MAIN, "Main")),
            config = config,
            sections = mapOf(HostSection.agentPreferences to jsonObject("revision" to 1, "values" to prefs).toJson()),
        )

    private val video = ExternalDownload(url = "https://x.test/v/movie.mp4", cookies = "c=1", referrer = "https://x.test/")

    @Test fun silentPlanRequiresPreference() {
        assertNull(SilentCapture.plan(video, state(emptyMap())))
        assertNull(SilentCapture.plan(video, state(mapOf(SilentCapture.SILENT_PREF to false))))
    }

    @Test fun silentPlanSaveDirPrecedence() {
        val categories = listOf(
            mapOf("id" to "builtin_video", "name" to "", "extensions" to listOf("mp4"), "position" to 1,
                "isBuiltin" to true, "builtinType" to "video", "saveDir" to "/videos"),
        )
        val remembered = mapOf(
            SilentCapture.SILENT_PREF to true,
            "download.remember_last_save_dir" to true,
            "download.last_save_dir" to " /last ",
        )
        // 目录仍按跟随上次 > 默认优先；分类开关开启（缺省）才交由 Rust 路由。
        val withCategories = SilentCapture.plan(video, state(remembered + ("custom_categories" to categories)))!!.request
        assertEquals("/last", withCategories.saveDir)
        assertEquals("/last", withCategories.offeredSaveDir)
        assertEquals("/last", SilentCapture.plan(video, state(remembered))?.request?.saveDir)
        val fallback = SilentCapture.plan(video, state(remembered - "download.remember_last_save_dir"))!!.request
        assertEquals("/dl", fallback.saveDir)
        assertEquals("/dl", fallback.offeredSaveDir)
        val disabled = SilentCapture.plan(video, state(remembered + (SilentCapture.AUTO_CATEGORY_PREF to false)))!!.request
        assertEquals("/last", disabled.saveDir)
        assertNull(disabled.offeredSaveDir)
        val defaultOnly = SilentCapture.plan(video, state(remembered + (SilentCapture.AUTO_CATEGORY_PREF to false) - "download.remember_last_save_dir"))!!.request
        assertEquals("/dl", defaultOnly.saveDir)
        assertNull(defaultOnly.offeredSaveDir)
        // 一个目录都算不出：退回确认弹层
        assertNull(SilentCapture.plan(video, state(mapOf(SilentCapture.SILENT_PREF to true), config = emptyMap())))
    }

    @Test fun categorySaveDirFollowsRulesAndIgnoresUnconfiguredDirs() {
        val categories = listOf(
            mapOf("id" to "builtin_video", "name" to "", "extensions" to listOf("mp4"), "position" to 1,
                "isBuiltin" to true, "builtinType" to "video", "saveDir" to " /videos "),
            mapOf("id" to "builtin_archive", "name" to "", "extensions" to listOf("zip"), "position" to 2,
                "isBuiltin" to true, "builtinType" to "archive", "saveDir" to ""),
        )
        val configured = state(mapOf("custom_categories" to categories))
        // 命中且配置了目录 → 该目录（修剪空白）；确认弹层与免打扰共用
        assertEquals("/videos", video.categorySaveDir(configured))
        // 命中但未配置目录 / 未命中任何分类 → null，由调用方回落到默认目录
        assertNull(ExternalDownload(url = "https://x.test/a.zip").categorySaveDir(configured))
        assertNull(video.categorySaveDir(state(emptyMap())))
    }

    @Test fun silentPlanUsesHostDefaultsAndBuildsCreateRequest() {
        val prefs = mapOf(SilentCapture.SILENT_PREF to true, SilentCapture.SKIP_SELECTION_PREF to true)
        val plan = SilentCapture.plan(video, state(prefs, mapOf("default_save_dir" to "/dl", "default_segments" to "16")))!!
        assertTrue(plan.unattended)
        val request = plan.request
        assertEquals(Queue.MAIN, request.queueId)
        assertEquals(16, request.segments)
        assertEquals("https://x.test/v/movie.mp4", request.url)
        assertEquals("/dl", request.saveDir)
        assertEquals("c=1", request.cookies)
        assertEquals("https://x.test/", request.referrer)
        assertTrue(request.headers.isEmpty())
        val withHeaders = SilentCapture.plan(video.copy(headers = mapOf("User-Agent" to "UA")), state(prefs))!!
        assertEquals("UA", withHeaders.request.headers["User-Agent"])
        assertFalse(SilentCapture.plan(video, state(mapOf(SilentCapture.SILENT_PREF to true)))!!.unattended)
    }

    private fun enc(s: String): String = java.net.URLEncoder.encode(s, Charsets.UTF_8)
}
