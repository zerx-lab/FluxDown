package com.fluxdown.app.feature.settings.extensions

import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.PluginDto
import com.fluxdown.core.protocol.jsonObject
import com.fluxdown.core.protocol.objOrNull
import com.fluxdown.core.protocol.strOrNull
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PluginFeedbackApiTest {
    private val plugin = PluginDto("publisher@plugin", "扩展 🧩", "2.1.0")

    @Test
    fun onlyEligibleInstalledPluginsAreLookedUp() {
        assertEquals(PluginFeedbackTarget("publisher@plugin", "2.1.0"), PluginFeedbackTarget.forPlugin(plugin))
        assertNull(PluginFeedbackTarget.forPlugin(plugin.copy(devMode = true)))
        assertNull(PluginFeedbackTarget.forPlugin(plugin.copy(source = "private")))
        assertNull(PluginFeedbackTarget.forPlugin(plugin.copy(version = " \n")))
        assertNull(PluginFeedbackTarget.forPlugin(plugin.copy(identity = "publisher@plugin&next=private")))
        assertNull(PluginFeedbackTarget.forPlugin(plugin.copy(identity = "publisher@plugin\n")))
        assertEquals(
            "https://fluxdown.com/api/feedback?pluginIdentity=publisher%40plugin",
            PluginFeedbackApi.destinationUrl(plugin.identity).toExternalForm(),
        )
    }

    @Test
    fun reportHasOnlyReviewedFieldsAndOmitsDiagnosticsByDefault() {
        val report = PluginFeedbackReport(
            PluginFeedbackTarget(plugin.identity, plugin.version), "publisher/repo", "标题 🧩",
            "第一行\n第二行  with  spacing", "1.2.3", null,
        )
        val body = Json.parse(report.toJson())
        assertEquals(
            setOf("type", "source", "title", "description", "appVersion", "pluginIdentity", "pluginVersion", "repository"),
            body.objOrNull?.keys,
        )
        assertEquals("bug", body.strOrNull("type"))
        assertEquals("app", body.strOrNull("source"))
        assertEquals(report.title, body.strOrNull("title"))
        assertEquals(report.description, body.strOrNull("description"))
        assertEquals(report.repository, body.strOrNull("repository"))
        val optedIn = report.copy(environment = "Android API 36; plugin: loaded")
        assertEquals(optedIn.environment, Json.parse(optedIn.toJson()).strOrNull("environment"))
    }

    @Test
    fun onlyValidRepositoriesAndIssueNumbersAreAccepted() {
        val route = PluginFeedbackApi.decodeDestination(200, """{"repository":"Owner-name/r.e_p-o"}""")
        assertEquals("Owner-name/r.e_p-o", (route as? PluginFeedbackResult.Ok)?.value)
        for (body in listOf("{}", """{"repository":"https://evil.example/repo"}""", """{"repository":"owner/.."}""")) {
            assertEquals(PluginFeedbackResult.Failure.Failed, PluginFeedbackApi.decodeDestination(200, body))
        }
        val submitted = PluginFeedbackApi.decodeSubmission(201, """{"issueNumber":42}""")
        assertEquals(42L, (submitted as? PluginFeedbackResult.Ok)?.value)
        assertEquals(PluginFeedbackResult.Failure.Unconfirmed, PluginFeedbackApi.decodeSubmission(201, """{"issueNumber":0}"""))
        assertEquals(PluginFeedbackResult.Failure.Unconfirmed, PluginFeedbackApi.decodeSubmission(502, null))
    }

    @Test
    fun unavailableChangedRateLimitedAndRedirectResponsesAreDistinct() {
        assertEquals(PluginFeedbackResult.Failure.Unavailable, PluginFeedbackApi.decodeDestination(404, null))
        assertEquals(PluginFeedbackResult.Failure.Unavailable, PluginFeedbackApi.decodeSubmission(404, null))
        assertEquals(PluginFeedbackResult.Failure.RouteChanged, PluginFeedbackApi.decodeSubmission(409, null))
        assertEquals(PluginFeedbackResult.Failure.RateLimited, PluginFeedbackApi.decodeDestination(429, null))
        assertEquals(PluginFeedbackResult.Failure.RateLimited, PluginFeedbackApi.decodeSubmission(429, null))
        assertEquals(PluginFeedbackResult.Failure.Failed, PluginFeedbackApi.decodeSubmission(307, null))
    }

    @Test
    fun auditProjectsOnlyTheFirstTwentyEntriesAndOnlyTheSelectedIdentity() {
        fun entry(outcome: String, identity: String = plugin.identity) = jsonObject(
            "identity" to identity, "outcome" to outcome, "detail" to "/private/secret?token=secret",
        )
        val entries = List(17) { entry("ok") } + listOf(
            entry("error"), entry("denied"), entry("error", "other@plugin"), entry("error"),
        )
        assertEquals(PluginFeedbackAudit(1, 1), PluginFeedbackAudit.fromJson(jsonObject("entries" to entries), plugin.identity))
        assertNull(PluginFeedbackAudit.fromJson(jsonObject("entries" to "bad response"), plugin.identity))
    }
}
