package com.fluxdown.core.protocol

import org.junit.Assert.assertEquals
import org.junit.Test

/** eD2K ID 检测结果解析与格式化（契约 §4）。 */
class Ed2kNetworkTest {
    private val texts = Ed2kIdCheckTexts(
        idLabel = { it.wire.uppercase() },
        upnpLabel = { "upnp-${it.wire}" },
        result = { id, server, port, upnp -> "$id|$server|$port|$upnp" },
        lowHint = "LOW-HINT",
        failed = "FAILED",
    )

    private fun format(json: String) = Ed2kIdCheckFormat.text(Ed2kNetworkDto.fromJson(Json.parse(json)), texts)

    @Test fun highIdShowsResultLineOnly() {
        assertEquals(
            "HIGH|45.82.80.155:5687|4662|upnp-mapped",
            format("""{"id":"high","clientId":123,"server":"45.82.80.155:5687","listenPort":4662,"upnp":"mapped","upnpEnabled":true,"kadEnabled":true,"kadNodes":0,"serverCount":3}"""),
        )
    }

    @Test fun lowIdAppendsTheHint() {
        assertEquals(
            "LOW|srv.example:4242|4662|upnp-failed\nLOW-HINT",
            format("""{"id":"low","clientId":5,"server":"srv.example:4242","listenPort":4662,"upnp":"failed"}"""),
        )
    }

    @Test fun noneShowsFailureWithOptionalError() {
        assertEquals("FAILED: connection refused", format("""{"id":"none","upnp":"disabled","error":"connection refused"}"""))
        assertEquals("FAILED", format("""{"id":"none","upnp":"disabled"}"""))
        assertEquals("FAILED: timeout", Ed2kIdCheckFormat.failed("timeout", texts))
        assertEquals("FAILED", Ed2kIdCheckFormat.failed("  ", texts))
    }

    @Test fun omittedFieldsFallBackToDashesAndDefaults() {
        assertEquals("HIGH|—|—|upnp-disabled", format("""{"id":"high"}"""))
        assertEquals("FAILED", format("{}"))
        assertEquals("HIGH|—|—|upnp-disabled", format("""{"id":"high","upnp":"mysteryState","listenPort":0,"server":""}"""))
    }
}
