package com.fluxdown.core.model

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class TaskErrorMessageTest {
    @Test
    fun mapsEngineMessagesToKeys() {
        assertEquals(
            "taskErrorPostOnlyEndpoint",
            TaskErrorMessage.match("This link only accepts POST requests (HTTP 405); start the download from the browser")?.i18nKey,
        )
        assertEquals(
            "taskErrorPostOnlyResume",
            TaskErrorMessage.match("  This task was started by a browser POST request and cannot be resumed; start it again from the browser\n")?.i18nKey,
        )
        assertEquals(
            "taskErrorUnexpectedHtml",
            TaskErrorMessage.match(" server returned HTML page (Content-Type: text/html), redirected to /login\n")?.i18nKey,
        )
        assertEquals(
            "taskErrorHtmlChanged",
            TaskErrorMessage.match("the server did not return a complete HTML page; nothing was saved")?.i18nKey,
        )
        assertEquals(
            "taskErrorMagnetMetadataTimeout",
            TaskErrorMessage.match("magnet metadata resolution took too long (300s) — no peers/DHT response; check trackers or network")?.i18nKey,
        )
    }

    @Test
    fun unrelatedMessagePassesThrough() {
        assertNull(TaskErrorMessage.match("connection reset"))
        assertNull(TaskErrorMessage.match("This link only accepts POST requests (HTTP 405); start the download from the browser."))
        assertNull(TaskErrorMessage.match("the server did not return a complete HTML page; nothing was saved."))
    }
}
